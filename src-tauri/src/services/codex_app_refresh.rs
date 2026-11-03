//! Reload the desktop's cached login mode after a verified quota transition.
use super::codex_app_process::{self, AppServer};
use crate::{codex_config, store::AppState};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tauri::Emitter;
use tokio::sync::{OwnedRwLockReadGuard, RwLock};

static REQUEST_GATE: Lazy<Arc<RwLock<()>>> = Lazy::new(|| Arc::new(RwLock::new(())));
static LAST_ACTIVITY: Lazy<Mutex<Instant>> = Lazy::new(|| Mutex::new(Instant::now()));
static INTENT_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct RefreshRequest {
    home: PathBuf,
    route: String,
    account: String,
}

fn intent_path() -> PathBuf {
    crate::config::get_app_config_dir().join("codex-login-mode-refresh.json")
}

fn read_intent(path: &Path) -> Option<RefreshRequest> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn save_intent(path: &Path, request: &RefreshRequest) -> Result<(), ()> {
    crate::config::atomic_write_private(path, &serde_json::to_vec(request).map_err(|_| ())?)
        .map_err(|_| ())
}

#[cfg(test)]
fn resumable_intent(path: &Path, current: Option<&RefreshRequest>) -> Option<RefreshRequest> {
    read_intent(path).filter(|saved| Some(saved) == current)
}

// Persist before the config write, including restoration. A crash before the
// write leaves a mismatched intent, which startup discards without signalling.
pub(crate) fn persist_refresh_intent(
    intent_path: &Path,
    config_path: &Path,
    text: &str,
    account: &str,
) -> Result<(), ()> {
    let request = RefreshRequest {
        home: config_path.parent().ok_or(())?.to_path_buf(),
        route: route_fingerprint(text).ok_or(())?,
        account: account.to_string(),
    };
    let _queue = INTENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    save_intent(intent_path, &request)
}

// Held for the whole streaming response, not just until response headers arrive.
pub(crate) async fn enter_request() -> OwnedRwLockReadGuard<()> {
    let guard = REQUEST_GATE.clone().read_owned().await;
    note_activity();
    guard
}

pub(crate) fn note_activity() {
    *LAST_ACTIVITY.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
}

fn current_request() -> Option<RefreshRequest> {
    let text = codex_config::read_codex_config_text().ok()?;
    Some(RefreshRequest {
        home: codex_config::get_codex_config_dir(),
        route: route_fingerprint(&text)?,
        account: super::subscription::codex_login_quota_identity()?,
    })
}

fn route_fingerprint(text: &str) -> Option<String> {
    let doc = text.parse::<toml::Value>().ok()?;
    if doc.get("profile").is_some() || doc.get("profiles").is_some() {
        return None;
    }
    let provider = doc.get("model_provider")?.as_str()?;
    let route = serde_json::json!({
        "provider": provider,
        "model": doc.get("model"),
        "table": doc.get("model_providers")?.get(provider)?,
    });
    Some(format!(
        "{:x}",
        Sha256::digest(route.to_string().as_bytes())
    ))
}

fn queued_request() -> Option<RefreshRequest> {
    let _guard = INTENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    read_intent(&intent_path())
}

fn clear_request(expected: &RefreshRequest) {
    let _guard = INTENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_intent(&intent_path(), expected);
}

fn clear_intent(path: &Path, expected: &RefreshRequest) {
    // The file is the queue: an older check cannot erase or replace a newer transition.
    if read_intent(path).as_ref() == Some(expected) {
        if let Err(error) = std::fs::remove_file(path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!("Codex login refresh intent could not be removed");
            }
        }
    }
}

struct Pending {
    request: RefreshRequest,
    servers: Vec<(AppServer, u8)>,
}

fn may_reload(connections: usize, idle: Duration, turn_open: bool) -> bool {
    connections == 0 && idle >= Duration::from_secs(10) && !turn_open
}

fn notify(app: &tauri::AppHandle, status: &str) {
    let _ = app.emit(
        "codex-login-mode-refresh",
        serde_json::json!({"status": status}),
    );
}

fn inspection_failed(failures: &mut u8) -> bool {
    *failures = failures.saturating_add(1);
    *failures >= 3
}

pub(crate) fn start(app: tauri::AppHandle, state: AppState) {
    tauri::async_runtime::spawn(async move {
        let mut pending: Option<Pending> = None;
        let mut observed_request = None;
        let mut failures = 0;
        let mut interval = tokio::time::interval(Duration::from_secs(3));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            // Serialize this snapshot with quota transitions. Persisting an intent
            // precedes the config write, but both occur under the same switch lock.
            let (request, current) = {
                let _switch = state.proxy_service.lock_switch_for_app("codex").await;
                (queued_request(), current_request())
            };
            let Some(request) = request else {
                pending = None;
                observed_request = None;
                failures = 0;
                continue;
            };
            if observed_request.as_ref() != Some(&request) {
                observed_request = Some(request.clone());
                failures = 0;
            }
            if current.is_none() {
                if inspection_failed(&mut failures) {
                    notify(&app, "manual");
                    clear_request(&request);
                    pending = None;
                }
                continue;
            }
            if current.as_ref() != Some(&request) {
                clear_request(&request);
                pending = None;
                continue;
            }
            let home = request.home.clone();
            let discovery =
                tokio::task::spawn_blocking(move || codex_app_process::discover(&home)).await;
            let servers = match discovery {
                Ok(Ok(servers)) => servers,
                _ => {
                    if inspection_failed(&mut failures) {
                        notify(&app, "manual");
                        clear_request(&request);
                        pending = None;
                    }
                    continue;
                }
            };
            if pending.as_ref().map(|p| &p.request) != Some(&request) {
                pending = Some(Pending {
                    request: request.clone(),
                    servers: servers.iter().cloned().map(|s| (s, 0)).collect(),
                });
                if !servers.is_empty() {
                    notify(&app, "waiting");
                }
            }
            let work = pending.as_mut().expect("pending refresh initialized");
            // New app-servers already read the updated file. Only stop the captured ones.
            work.servers.retain(|(server, _)| servers.contains(server));
            if work.servers.is_empty() {
                notify(&app, "reloaded");
                clear_request(&request);
                pending = None;
                continue;
            }
            if work.servers.iter().all(|(_, tries)| *tries >= 3) {
                notify(&app, "manual");
                clear_request(&request);
                pending = None;
                continue;
            }
            let Ok(status) = state.proxy_service.get_status().await else {
                continue;
            };
            let idle = LAST_ACTIVITY
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .elapsed();
            if !may_reload(status.active_connections, idle, false) {
                continue;
            }
            let home = request.home.clone();
            let inspection =
                tokio::task::spawn_blocking(move || codex_app_process::session_state(&home)).await;
            let busy = match inspection {
                Ok(Ok(busy)) => {
                    failures = 0;
                    busy
                }
                _ => {
                    if inspection_failed(&mut failures) {
                        notify(&app, "manual");
                        clear_request(&request);
                        pending = None;
                    }
                    continue;
                }
            };
            if !may_reload(status.active_connections, idle, busy) {
                continue;
            }

            // Never race a newly started proxy request or a provider switch.
            let _switch = state.proxy_service.lock_switch_for_app("codex").await;
            let Ok(_request_guard) = REQUEST_GATE.clone().try_write_owned() else {
                continue;
            };
            if LAST_ACTIVITY
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .elapsed()
                < Duration::from_secs(10)
                || current_request().as_ref() != Some(&request)
                || queued_request().as_ref() != Some(&request)
            {
                continue;
            }
            for (server, tries) in &mut work.servers {
                if *tries >= 3 {
                    continue;
                }
                *tries += 1;
                let server = server.clone();
                let home = request.home.clone();
                let _outcome = tokio::task::spawn_blocking(move || {
                    codex_app_process::end_if_same(&server, &home)
                })
                .await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_login_refresh_old_completion_cannot_erase_new_transition() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("intent.json");
        let old = RefreshRequest {
            home: dir.path().to_path_buf(),
            route: "old".into(),
            account: "account".into(),
        };
        let new = RefreshRequest {
            route: "new".into(),
            ..old.clone()
        };
        save_intent(&path, &old).unwrap();
        let captured = read_intent(&path).unwrap();
        save_intent(&path, &new).unwrap();
        clear_intent(&path, &captured);
        assert_eq!(read_intent(&path), Some(new.clone()));
        clear_intent(&path, &new);
        assert!(!path.exists());
    }

    #[test]
    fn codex_login_refresh_unknown_checks_share_a_bounded_retry_budget() {
        let mut failures = 0;
        // A session timeout followed by discovery still running is not immediately fatal.
        assert!(!inspection_failed(&mut failures));
        assert!(!inspection_failed(&mut failures));
        assert!(inspection_failed(&mut failures));
        failures = 0; // A successful session inspection resets the consecutive error budget.
        assert!(!inspection_failed(&mut failures));
    }

    #[test]
    fn codex_login_refresh_restoration_intent_survives_restart_without_fallback_marker() {
        let dir = tempfile::tempdir().unwrap();
        let intent = dir.path().join("refresh.json");
        let config_path = dir.path().join("config.toml");
        let config = "model_provider = \"company\"\nmodel = \"test\"\n[model_providers.company]\nrequires_openai_auth = false\nexperimental_bearer_token = \"secret-test-key\"\n";
        let fallback = RefreshRequest {
            home: dir.path().to_path_buf(),
            route: route_fingerprint(config).unwrap(),
            account: "account-hash".into(),
        };
        persist_refresh_intent(&intent, &config_path, config, &fallback.account).unwrap();
        assert_eq!(
            resumable_intent(&intent, Some(&fallback)),
            Some(fallback.clone())
        );
        let restored = config.replace("false", "true");
        persist_refresh_intent(&intent, &config_path, &restored, &fallback.account).unwrap();
        // If the process died before the config write, the old file cannot trigger a reload.
        assert_eq!(resumable_intent(&intent, Some(&fallback)), None);
        let current = RefreshRequest {
            route: route_fingerprint(&restored).unwrap(),
            ..fallback
        };
        assert_eq!(
            resumable_intent(&intent, Some(&current)),
            Some(current.clone())
        );
        let changed_account = RefreshRequest {
            account: "another-account".into(),
            ..current
        };
        assert_eq!(resumable_intent(&intent, Some(&changed_account)), None);
        assert!(!std::fs::read_to_string(intent)
            .unwrap()
            .contains("secret-test-key"));
    }

    #[test]
    fn desktop_reload_route_tracks_auth_not_mcp_changes() {
        let config = "model_provider = \"company\"\nmodel = \"test-model\"\n[model_providers.company]\nbase_url = \"http://127.0.0.1:8317/v1\"\nrequires_openai_auth = true\n";
        let original = route_fingerprint(config).unwrap();
        assert_ne!(
            route_fingerprint(&config.replace("true", "false")).unwrap(),
            original
        );
        assert_ne!(
            route_fingerprint(&config.replace("test-model", "other-model")).unwrap(),
            original
        );
        assert_eq!(
            route_fingerprint(&format!(
                "{config}\n[mcp_servers.test]\ncommand = \"test\"\n"
            ))
            .unwrap(),
            original
        );
        assert!(route_fingerprint(&format!("profile = \"work\"\n{config}")).is_none());
        assert!(!original.contains("8317"));
    }

    #[test]
    fn desktop_reload_requires_idle_proxy_and_no_open_turn() {
        assert!(may_reload(0, Duration::from_secs(10), false));
        assert!(!may_reload(1, Duration::from_secs(60), false));
        assert!(!may_reload(0, Duration::from_secs(9), false));
        assert!(!may_reload(0, Duration::from_secs(60), true));
    }

    #[tokio::test]
    async fn desktop_reload_gate_cannot_interrupt_streaming_requests() {
        let gate = Arc::new(RwLock::new(()));
        let request = gate.clone().read_owned().await;
        assert!(gate.clone().try_write_owned().is_err());
        drop(request);
        let reload = gate.clone().try_write_owned().unwrap();
        assert!(gate.clone().try_read_owned().is_err());
        drop(reload);
        assert!(gate.try_read_owned().is_ok());
    }
}
