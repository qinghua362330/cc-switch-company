//! Quota fallback for direct and verified local proxy routes. Never writes auth.json.
use super::subscription::{self, CodexQuotaAvailability};
use crate::{app_config::AppType, codex_config, config, settings, store::AppState};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tauri::Emitter;
use toml_edit::DocumentMut;

#[derive(Clone, Serialize, Deserialize)]
struct Ownership {
    account: String,
    provider: String,
    path: String,
    original: String,
    applied: String,
    #[serde(default)]
    proxy_url: Option<String>,
    #[serde(default)]
    undo: Undo,
}

#[derive(Clone, Default, Serialize, Deserialize)]
enum Undo {
    #[default]
    Flag,
    InjectedBearer,
}

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn routing_digest(text: &str, proxy_url: Option<&str>) -> Option<String> {
    let doc = text.parse::<DocumentMut>().ok()?;
    if doc.get("profile").is_some() || doc.get("profiles").is_some() {
        return None;
    }
    let id = doc
        .get("model_provider")
        .and_then(|v| v.as_str())
        .unwrap_or("openai");
    if proxy_url.is_none() {
        active_table(&doc)?;
    } else if let Some(url) = proxy_url {
        if !is_loopback_url(url)
            || doc
                .get("model_providers")
                .and_then(|tables| tables.get(id))
                .and_then(|table| table.get("base_url"))
                .and_then(|value| value.as_str())
                != Some(url)
        {
            return None;
        }
    }
    let parsed = text.parse::<toml::Value>().ok()?;
    // MCP/settings sync is independent of login mode. Track only routing and
    // credentials; recovery patches one flag in the current file, never replays it.
    Some(digest(
        &serde_json::json!({
            "provider": id,
            // Selecting another published Stack model does not change route ownership.
            "model": if proxy_url.is_none() { parsed.get("model") } else { None },
            "route": parsed.get("model_providers").and_then(|tables| tables.get(id)),
            "openai_base_url": parsed.get("openai_base_url"),
        })
        .to_string(),
    ))
}

fn apply_override(text: &str, proxy_url: Option<&str>) -> Option<(String, Undo)> {
    let Some(url) = proxy_url else {
        return change_auth_flag(text, true).map(|s| (s, Undo::Flag));
    };
    if !is_loopback_url(url) {
        return None;
    }
    let mut doc = text.parse::<DocumentMut>().ok()?;
    if doc.get("profile").is_some() || doc.get("profiles").is_some() {
        return None;
    }
    let selector = doc
        .get("model_provider")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if selector.as_deref().is_none_or(|id| id == "openai") {
        // Built-in OpenAI cannot disable OAuth without changing session buckets.
        return None;
    }
    let id = selector?;
    let table = doc.get("model_providers")?.get(&id)?.as_table_like()?;
    if table.get("base_url").and_then(|v| v.as_str()) != Some(url)
        || table.get("requires_openai_auth").and_then(|v| v.as_bool()) != Some(true)
    {
        return None;
    }
    let undo = if table.get("experimental_bearer_token").is_none() && table.get("env_key").is_none()
    {
        doc["model_providers"][&id]["experimental_bearer_token"] =
            toml_edit::value("PROXY_MANAGED");
        Undo::InjectedBearer
    } else {
        Undo::Flag
    };
    doc["model_providers"][&id]["requires_openai_auth"] = toml_edit::value(false);
    Some((doc.to_string(), undo))
}

fn restore_override(text: &str, marker: &Ownership) -> Option<String> {
    if routing_digest(text, marker.proxy_url.as_deref()).as_deref() != Some(marker.applied.as_str())
    {
        return None;
    }
    let mut doc = text.parse::<DocumentMut>().ok()?;
    match &marker.undo {
        Undo::Flag => return change_auth_flag_for_route(text, false, marker.proxy_url.as_deref()),
        Undo::InjectedBearer => {
            let id = doc.get("model_provider")?.as_str()?.to_string();
            let table = doc
                .get_mut("model_providers")?
                .get_mut(&id)?
                .as_table_like_mut()?;
            table.remove("experimental_bearer_token");
            table.insert("requires_openai_auth", toml_edit::value(true));
        }
    }
    Some(doc.to_string())
}

fn marker_path() -> PathBuf {
    config::get_app_config_dir().join("codex-quota-fallback.json")
}

fn load_marker() -> Result<Option<Ownership>, ()> {
    match std::fs::read(marker_path()) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|_| ()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    }
}

fn active_table(doc: &DocumentMut) -> Option<(&str, &dyn toml_edit::TableLike)> {
    active_table_for_route(doc, None)
}

fn is_loopback_url(value: &str) -> bool {
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    url.scheme() == "http"
        && url.username().is_empty()
        && url.password().is_none()
        && match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        }
}

fn active_table_for_route<'a>(
    doc: &'a DocumentMut,
    proxy_url: Option<&str>,
) -> Option<(&'a str, &'a dyn toml_edit::TableLike)> {
    // Profile overrides can select a different model/provider than the root.
    if doc.get("profile").is_some() || doc.get("profiles").is_some() {
        return None;
    }
    let id = doc.get("model_provider")?.as_str()?;
    if !codex_config::is_custom_codex_model_provider_id(id)
        || id == codex_config::CC_SWITCH_CODEX_OFFICIAL_PROXY_PROVIDER_ID
    {
        return None;
    }
    let table = doc
        .get("model_providers")?
        .as_table_like()?
        .get(id)?
        .as_table_like()?;
    // A bearer or environment key must short-circuit official OAuth auth.
    let bearer = table
        .get("experimental_bearer_token")
        .and_then(|v| v.as_str());
    let env = table.get("env_key").and_then(|v| v.as_str());
    if let Some(expected) = proxy_url {
        if !is_loopback_url(expected)
            || table.get("base_url").and_then(|v| v.as_str()) != Some(expected)
        {
            return None;
        }
    }
    if bearer == Some("PROXY_MANAGED") {
        return proxy_url.map(|_| (id, table));
    }
    if !bearer.is_some_and(|v| !v.trim().is_empty() && v != "PROXY_MANAGED")
        && env.is_none_or(|v| v.trim().is_empty())
    {
        return None;
    }
    Some((id, table))
}

fn change_auth_flag(text: &str, expected: bool) -> Option<String> {
    change_auth_flag_for_route(text, expected, None)
}

fn change_auth_flag_for_route(
    text: &str,
    expected: bool,
    proxy_url: Option<&str>,
) -> Option<String> {
    let mut doc = text.parse::<DocumentMut>().ok()?;
    let (id, table) = active_table_for_route(&doc, proxy_url)?;
    if table.get("requires_openai_auth")?.as_bool()? != expected {
        return None;
    }
    let id = id.to_string();
    doc["model_providers"][&id]["requires_openai_auth"] = toml_edit::value(!expected);
    Some(doc.to_string())
}

enum Decision {
    Keep,
    Forget,
    Apply(String, Ownership),
    Restore(String, String),
}

fn decide(
    text: &str,
    owner: &Ownership,
    marker: Option<&Ownership>,
    enabled: bool,
    quota: CodexQuotaAvailability,
) -> Decision {
    let proxy_url = owner.proxy_url.as_deref();
    if let Some(marker) = marker {
        if marker.account != owner.account
            || marker.provider != owner.provider
            || marker.path != owner.path
            || marker.proxy_url != owner.proxy_url
            || routing_digest(text, proxy_url).as_deref() != Some(marker.applied.as_str())
        {
            if marker.account == owner.account && marker.path == owner.path {
                if let Some(restored) = restore_override(text, marker) {
                    return Decision::Restore(restored, marker.account.clone());
                }
            }
            // User/account/provider/config changes cancel our ownership.
            return Decision::Forget;
        }
        if !enabled || quota == CodexQuotaAvailability::Available {
            return restore_override(text, marker)
                .map(|text| Decision::Restore(text, marker.account.clone()))
                .unwrap_or(Decision::Forget);
        }
        return Decision::Keep;
    }
    if enabled && quota == CodexQuotaAvailability::Exhausted {
        if let Some((output, undo)) = apply_override(text, proxy_url) {
            let mut marker = owner.clone();
            marker.undo = undo;
            marker.original = routing_digest(text, proxy_url).expect("validated route");
            marker.applied = routing_digest(&output, proxy_url).expect("validated route");
            return Decision::Apply(output, marker);
        }
    }
    Decision::Keep
}

// Called under the live-write mutex. Only a refresh of the same original route
// is overlaid; model, credential, provider and account changes take precedence.
pub(crate) fn preserve_owned_fallback(provider: &str, proposed: &str) -> String {
    if !settings::get_settings().codex_quota_auto_fallback {
        return proposed.to_string();
    }
    let Some(marker) = load_marker().ok().flatten() else {
        return proposed.to_string();
    };
    if marker.proxy_url.is_none() && !settings::preserve_codex_official_auth_on_switch() {
        return proposed.to_string();
    }
    if marker.path != digest(&codex_config::get_codex_config_path().to_string_lossy())
        || subscription::codex_login_quota_identity().as_deref() != Some(marker.account.as_str())
    {
        return proposed.to_string();
    }
    let live = codex_config::read_codex_config_text().unwrap_or_default();
    refresh_overlay(provider, proposed, &live, &marker)
}

/// Verify persisted ownership without probing quota or mutating live files.
pub(crate) fn has_owned_fallback() -> bool {
    let Some(marker) = load_marker().ok().flatten() else {
        return false;
    };
    marker.path == digest(&codex_config::get_codex_config_path().to_string_lossy())
        && subscription::codex_login_quota_identity().as_deref() == Some(marker.account.as_str())
        && codex_config::read_codex_config_text()
            .ok()
            .and_then(|text| routing_digest(&text, marker.proxy_url.as_deref()))
            .as_deref()
            == Some(marker.applied.as_str())
}

fn refresh_overlay(provider: &str, proposed: &str, live: &str, marker: &Ownership) -> String {
    let proxy_url = marker.proxy_url.as_deref();
    if marker.provider == provider
        && routing_digest(proposed, proxy_url).as_deref() == Some(marker.original.as_str())
        && routing_digest(live, proxy_url).as_deref() == Some(marker.applied.as_str())
    {
        apply_override(proposed, proxy_url)
            .map(|(text, _)| text)
            .unwrap_or_else(|| proposed.to_string())
    } else {
        proposed.to_string()
    }
}

pub(crate) fn needs_projection() -> bool {
    marker_path().exists()
}

pub(crate) struct QuotaProjection {
    identity: Option<String>,
    cleanup: bool,
}

pub(crate) fn projection(identity: Option<String>, cleanup: bool) -> QuotaProjection {
    QuotaProjection { identity, cleanup }
}

impl crate::live::patch::toml::TomlDocPatch for QuotaProjection {
    fn apply_to(
        &self,
        _: &Path,
        doc: &mut DocumentMut,
    ) -> Result<(), crate::live::patch::LiveWriteError> {
        let text = doc.to_string();
        let replacement = if self.cleanup {
            load_marker()
                .ok()
                .flatten()
                .filter(|marker| {
                    marker.path == digest(&codex_config::get_codex_config_path().to_string_lossy())
                })
                .and_then(|marker| restore_override(&text, &marker))
                .unwrap_or(text)
        } else {
            self.identity
                .as_deref()
                .map(|id| preserve_owned_fallback(id, &text))
                .unwrap_or(text)
        };
        *doc = replacement.parse().expect("validated quota projection");
        Ok(())
    }
}

pub(crate) fn projection_committed() {
    let Some(marker) = load_marker().ok().flatten() else {
        return;
    };
    let Ok(live) = codex_config::read_codex_config_text() else {
        return;
    };
    if routing_digest(&live, marker.proxy_url.as_deref()).as_deref()
        == Some(marker.applied.as_str())
    {
        return;
    }
    if let Some(account) = subscription::codex_login_quota_identity() {
        let _ = super::codex_app_refresh::persist_refresh_intent(
            &marker_path().with_file_name("codex-login-mode-refresh.json"),
            &codex_config::get_codex_config_path(),
            &live,
            &account,
        );
    }
    let _ = std::fs::remove_file(marker_path());
}

fn independent(provider: &crate::provider::Provider) -> Option<String> {
    if super::provider::codex_direct::is_official(provider) || provider.is_codex_oauth() {
        return None;
    }
    let text = codex_config::prepare_codex_provider_live_config(
        provider
            .settings_config
            .get("auth")
            .unwrap_or(&serde_json::Value::Null),
        provider.settings_config.get("config")?.as_str()?,
    )
    .ok()?;
    let doc = text.parse::<DocumentMut>().ok()?;
    let (_, table) = active_table(&doc)?;
    let env_credential = if let Some(key) = table.get("env_key").and_then(|value| value.as_str()) {
        let value = std::env::var(key).ok()?;
        if value.trim().is_empty() || value == "PROXY_MANAGED" {
            return None;
        }
        Some(value)
    } else {
        None
    };
    let endpoint = table.get("base_url")?.as_str()?;
    let url = url::Url::parse(endpoint).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let parsed = text.parse::<toml::Value>().ok()?;
    Some(digest(
        &serde_json::json!({
            "tables": parsed.get("model_providers")?, "env": env_credential,
        })
        .to_string(),
    ))
}

/// Only hashes are persisted. Bind the route and every published member's credentials.
pub(crate) fn proxy_identity(
    route: &crate::provider::Provider,
    url: &str,
    members: &[crate::mode::stack::Member],
    catalog: Option<&serde_json::Value>,
) -> Option<String> {
    if !is_loopback_url(url) {
        return None;
    }
    let slugs: Vec<&str> = catalog
        .and_then(|c| c.get("models"))
        .and_then(|m| m.as_array())
        .into_iter()
        .flatten()
        .filter_map(|m| m.get("slug")?.as_str())
        .collect();
    let official = super::provider::codex_direct::is_official(route);
    let route_key = independent(route);
    let available = members.iter().any(|member| {
        independent(&member.provider).is_some()
            && member
                .model_ids
                .iter()
                .any(|id| slugs.contains(&id.as_str()))
    });
    if official && (!settings::unify_codex_session_history() || !available) {
        return None;
    }
    if !official && route_key.is_none() {
        return None;
    }
    let member_keys: Vec<_> = members.iter().map(|m| serde_json::json!({
        "id": m.provider.id, "key": m.key, "models": m.model_ids,
        "credentials": independent(&m.provider),
        "published": m.model_ids.iter().filter(|id| slugs.contains(&id.as_str())).collect::<Vec<_>>(),
    })).collect();
    Some(format!(
        "proxy:{}:{}",
        route.id,
        digest(
            &serde_json::json!({
                "url": url, "route": route_key, "official": official,
                "route_meta": route.meta, "members": member_keys,
            })
            .to_string()
        )
    ))
}

fn read_owned_catalog(text: &str, catalog_path: &Path) -> Option<serde_json::Value> {
    let resolved = codex_config::resolve_cc_switch_catalog_path(text, catalog_path.parent()?)?;
    // The projection uses a relative filename. Resolve it with the shared path
    // guard, but do not accept another same-named catalog elsewhere in the home.
    if resolved != std::fs::canonicalize(catalog_path).ok()? {
        return None;
    }
    serde_json::from_slice(&std::fs::read(resolved).ok()?).ok()
}

async fn eligible_route(state: &AppState, text: &str) -> Option<(String, Option<String>)> {
    use crate::live::engine::DeviceStore;
    let mode = crate::mode::state::mode_state(&DeviceStore::for_device(), "codex").ok()?;
    let doc = text.parse::<DocumentMut>().ok()?;
    if mode.is_proxy() {
        if !mode.attached || !state.proxy_service.is_running().await {
            return None;
        }
        let id = mode.proxy_route.as_deref()?;
        let provider = state.db.get_provider_by_id(id, "codex").ok()??;
        let url = state.proxy_service.build_proxy_urls().await.ok()?.1;
        let selector = doc.get("model_provider")?.as_str()?;
        let table = doc.get("model_providers")?.get(selector)?.as_table_like()?;
        if table.get("base_url").and_then(|v| v.as_str()) != Some(url.as_str()) {
            return None;
        }
        let stack = crate::mode::state::stack(&DeviceStore::for_device(), "codex").ok()?;
        let members =
            crate::mode::stack::published_members(&state.db, &AppType::Codex, &stack, Some(id))
                .ok()?;
        let catalog_path = codex_config::get_codex_model_catalog_path();
        let catalog = read_owned_catalog(text, &catalog_path);
        let identity = proxy_identity(&provider, &url, &members, catalog.as_ref())?;
        Some((identity, Some(url)))
    } else {
        let id = crate::mode::current::provider_for(
            &state.db,
            &AppType::Codex,
            crate::mode::current::Purpose::Direct,
        )
        .ok()??;
        let provider = state.db.get_provider_by_id(&id, "codex").ok()??;
        independent(&provider)?;
        active_table(&doc)?;
        Some((id, None))
    }
}
async fn check(state: &AppState, due: bool) -> Result<Option<bool>, ()> {
    check_with_probe(state, due, subscription::probe_codex_login_quota).await
}

pub(super) async fn check_with_probe<F, Fut>(
    state: &AppState,
    due: bool,
    probe: F,
) -> Result<Option<bool>, ()>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Option<(String, CodexQuotaAvailability)>>,
{
    let path = codex_config::get_codex_config_path();
    let before = codex_config::read_codex_config_text().map_err(|_| ())?;
    let Some((provider, proxy_url)) = eligible_route(state, &before).await else {
        let Some(marker) = load_marker()? else {
            return Ok(None);
        };
        let _switch = crate::mode::controller::lock_settled(state, &AppType::Codex)
            .await
            .map_err(|_| ())?;
        if eligible_route(state, &before).await.is_some() {
            return Ok(None);
        }
        let _write = codex_config::codex_live_write_lock();
        if marker.path == digest(&path.to_string_lossy())
            && path == codex_config::get_codex_config_path()
            && subscription::codex_login_quota_identity().as_deref()
                == Some(marker.account.as_str())
            && codex_config::read_codex_config_text().map_err(|_| ())? == before
        {
            if let Some(restored) = restore_override(&before, &marker) {
                return commit_decision(
                    &path,
                    &marker_path(),
                    Decision::Restore(restored, marker.account),
                );
            }
        }
        return Ok(None);
    };
    let enabled = settings::get_settings().codex_quota_auto_fallback
        && (proxy_url.is_some() || settings::preserve_codex_official_auth_on_switch());
    if !enabled && load_marker()?.is_none() {
        return Ok(None);
    }
    if enabled && !due {
        return Ok(None);
    }

    let Some(account) = subscription::codex_login_quota_identity() else {
        return Ok(None);
    };
    let quota = if enabled {
        let Some((identity, quota)) = probe().await else {
            return Ok(None);
        };
        if identity != account {
            return Ok(None);
        }
        quota
    } else {
        CodexQuotaAvailability::Unknown
    };

    let _switch_guard = crate::mode::controller::lock_settled(state, &AppType::Codex)
        .await
        .map_err(|_| ())?;
    if eligible_route(state, &before).await != Some((provider.clone(), proxy_url.clone())) {
        return Ok(None);
    }
    let _write_guard = codex_config::codex_live_write_lock();
    // No network under either lock. Discard observations made against stale state.
    if enabled
        != (settings::get_settings().codex_quota_auto_fallback
            && (proxy_url.is_some() || settings::preserve_codex_official_auth_on_switch()))
        || path != codex_config::get_codex_config_path()
        || subscription::codex_login_quota_identity().as_deref() != Some(account.as_str())
        || codex_config::read_codex_config_text().map_err(|_| ())? != before
    {
        return Ok(None);
    }
    let owner = Ownership {
        account,
        provider,
        path: digest(&path.to_string_lossy()),
        original: String::new(),
        applied: String::new(),
        proxy_url,
        undo: Undo::default(),
    };
    commit_decision(
        &path,
        &marker_path(),
        decide(&before, &owner, load_marker()?.as_ref(), enabled, quota),
    )
}

fn commit_decision(
    path: &Path,
    marker_path: &Path,
    decision: Decision,
) -> Result<Option<bool>, ()> {
    match decision {
        Decision::Keep => Ok(None),
        Decision::Forget => {
            std::fs::remove_file(marker_path).map_err(|_| ())?;
            Ok(None)
        }
        Decision::Apply(text, marker) => {
            super::codex_app_refresh::persist_refresh_intent(
                &marker_path.with_file_name("codex-login-mode-refresh.json"),
                path,
                &text,
                &marker.account,
            )?;
            // Record intent before the atomic write so a crash is recoverable.
            config::atomic_write_private(
                marker_path,
                &serde_json::to_vec(&marker).map_err(|_| ())?,
            )
            .map_err(|_| ())?;
            config::atomic_write_private(path, text.as_bytes()).map_err(|_| ())?;
            Ok(Some(true))
        }
        Decision::Restore(text, account) => {
            super::codex_app_refresh::persist_refresh_intent(
                &marker_path.with_file_name("codex-login-mode-refresh.json"),
                path,
                &text,
                &account,
            )?;
            config::atomic_write_private(path, text.as_bytes()).map_err(|_| ())?;
            std::fs::remove_file(marker_path).map_err(|_| ())?;
            Ok(Some(false))
        }
    }
}

pub(crate) fn start(app: tauri::AppHandle, state: AppState) {
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_probe = None;
        let mut was_enabled = false;
        loop {
            interval.tick().await;
            let enabled = settings::get_settings().codex_quota_auto_fallback;
            let due = !was_enabled
                || last_probe
                    .is_none_or(|time: Instant| time.elapsed() >= Duration::from_secs(300));
            if due && enabled {
                last_probe = Some(Instant::now());
            }
            was_enabled = enabled;
            match check(&state, due).await {
                Ok(Some(active)) => {
                    log::info!("Codex quota API fallback active={active}; restart Codex if needed");
                    let _ = app.emit(
                        "codex-quota-fallback-changed",
                        serde_json::json!({"active": active}),
                    );
                }
                Ok(None) => {}
                Err(()) => log::warn!(
                    "Codex quota fallback check skipped; state could not be safely verified"
                ),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quota_catalog_accepts_projected_relative_path_but_not_foreign_catalogs() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join("codex");
        std::fs::create_dir_all(base.join("other")).unwrap();
        let filename = codex_config::CC_SWITCH_CODEX_MODEL_CATALOG_FILENAME;
        let catalog = base.join(filename);
        let foreign = base.join("other").join(filename);
        let outside = home.path().join(filename);
        let value = serde_json::json!({"models": [{"slug": "ccs-company/company-model"}]});
        for path in [&catalog, &foreign, &outside] {
            std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
        }
        let config = |path: &Path| {
            let mut doc = DocumentMut::new();
            doc["model_catalog_json"] = toml_edit::value(path.to_str().unwrap());
            doc.to_string()
        };
        for path in [Path::new(filename), catalog.as_path()] {
            assert_eq!(
                read_owned_catalog(&config(path), &catalog),
                Some(value.clone())
            );
        }
        for path in [
            foreign.as_path(),
            outside.as_path(),
            Path::new("other.json"),
        ] {
            assert!(read_owned_catalog(&config(path), &catalog).is_none());
        }
        assert!(read_owned_catalog("", &catalog).is_none());
        std::fs::write(&catalog, "invalid-json").unwrap();
        assert!(read_owned_catalog(&config(Path::new(filename)), &catalog).is_none());
    }

    const CONFIG: &str = "model_provider = \"company\"\nmodel = \"company-model\"\n[model_providers.company]\nname = \"Company\"\nbase_url = \"https://relay.example/v1\"\nexperimental_bearer_token = \"test-key\"\nrequires_openai_auth = true\n";
    fn owner() -> Ownership {
        Ownership {
            account: "account-hash".into(),
            provider: "company".into(),
            path: "path-hash".into(),
            original: String::new(),
            applied: String::new(),
            proxy_url: None,
            undo: Undo::default(),
        }
    }
    fn fallback() -> (String, Ownership) {
        match decide(
            CONFIG,
            &owner(),
            None,
            true,
            CodexQuotaAvailability::Exhausted,
        ) {
            Decision::Apply(text, marker) => (text, marker),
            _ => panic!("expected fallback"),
        }
    }
    #[test]
    fn quota_fallback_survives_refresh_without_leaking_to_another_card() {
        let (live, marker) = fallback();
        assert_eq!(refresh_overlay("company", CONFIG, &live, &marker), live);
        assert_eq!(
            refresh_overlay("other-card", CONFIG, &live, &marker),
            CONFIG
        );
        let edited = CONFIG.replace("company-model", "other-model");
        assert_eq!(refresh_overlay("company", &edited, &live, &marker), edited);
        // Transactional projection cleans only the override it still owns.
        let stored_a = restore_override(&live, &marker).unwrap();
        assert_eq!(stored_a, CONFIG);
        assert!(restore_override(&edited, &marker).is_none());
        assert!(matches!(
            decide(
                &stored_a,
                &owner(),
                None,
                true,
                CodexQuotaAvailability::Available
            ),
            Decision::Keep
        ));
        let refreshed = format!("{CONFIG}\n[mcp_servers.test]\ncommand = \"test-server\"\n");
        let refreshed = refresh_overlay("company", &refreshed, &live, &marker);
        assert!(refreshed.contains("requires_openai_auth = false"));
        assert!(refreshed.contains("test-server"));
        match decide(
            &refreshed,
            &owner(),
            Some(&marker),
            true,
            CodexQuotaAvailability::Available,
        ) {
            Decision::Restore(restored, _) => {
                assert!(restored.contains("requires_openai_auth = true"));
                assert!(restored.contains("test-server"));
            }
            _ => panic!("MCP sync must not strand automatic API mode"),
        }
    }

    #[test]
    fn quota_fallback_disk_cycle_survives_restart_and_never_writes_auth() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        let marker_path = temp.path().join("marker.json");
        let auth_path = temp.path().join("auth.json");
        let auth = br#"{"auth_mode":"chatgpt","tokens":{"access_token":"unchanged"}}"#;
        std::fs::write(&path, CONFIG).unwrap();
        std::fs::write(&auth_path, auth).unwrap();
        let action = decide(
            CONFIG,
            &owner(),
            None,
            true,
            CodexQuotaAvailability::Exhausted,
        );
        assert_eq!(commit_decision(&path, &marker_path, action), Ok(Some(true)));
        let live = std::fs::read_to_string(&path).unwrap();
        let marker: Ownership =
            serde_json::from_slice(&std::fs::read(&marker_path).unwrap()).unwrap();
        let action = decide(
            &live,
            &owner(),
            Some(&marker),
            true,
            CodexQuotaAvailability::Available,
        );
        assert_eq!(
            commit_decision(&path, &marker_path, action),
            Ok(Some(false))
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), CONFIG);
        assert_eq!(std::fs::read(auth_path).unwrap(), auth);
        assert!(!marker_path.exists());
        assert!(temp.path().join("codex-login-mode-refresh.json").exists());
    }

    #[test]
    fn quota_fallback_is_idempotent_and_restores_only_owned_change() {
        let (text, marker) = fallback();
        assert!(text.contains("requires_openai_auth = false"));
        assert!(matches!(
            decide(
                &text,
                &owner(),
                Some(&marker),
                true,
                CodexQuotaAvailability::Exhausted
            ),
            Decision::Keep
        ));
        assert!(matches!(
            decide(
                &text,
                &owner(),
                Some(&marker),
                true,
                CodexQuotaAvailability::Unknown
            ),
            Decision::Keep
        ));
        for (enabled, quota) in [
            (true, CodexQuotaAvailability::Available),
            (false, CodexQuotaAvailability::Unknown),
        ] {
            match decide(&text, &owner(), Some(&marker), enabled, quota) {
                Decision::Restore(restored, _) => assert_eq!(restored, CONFIG),
                _ => panic!("expected restore"),
            }
        }
        let json = serde_json::to_string(&marker).unwrap();
        assert!(!json.contains("test-key"));
        assert!(!json.contains("https://"));
    }
    #[test]
    fn quota_fallback_never_changes_manual_api_or_unknown_quota() {
        let (text, _) = fallback();
        for quota in [
            CodexQuotaAvailability::Available,
            CodexQuotaAvailability::Exhausted,
            CodexQuotaAvailability::Unknown,
        ] {
            assert!(matches!(
                decide(&text, &owner(), None, true, quota),
                Decision::Keep
            ));
        }
        assert!(matches!(
            decide(
                CONFIG,
                &owner(),
                None,
                true,
                CodexQuotaAvailability::Unknown
            ),
            Decision::Keep
        ));
        assert!(matches!(
            decide(
                CONFIG,
                &owner(),
                None,
                false,
                CodexQuotaAvailability::Exhausted
            ),
            Decision::Keep
        ));
    }
    #[test]
    fn quota_fallback_cancels_ownership_after_user_or_account_changes() {
        let (text, marker) = fallback();
        for changed in [
            text.replace("company-model", "other"),
            text.replace("test-key", "new-key"),
            text.replace("false", "true"),
        ] {
            assert!(matches!(
                decide(
                    &changed,
                    &owner(),
                    Some(&marker),
                    true,
                    CodexQuotaAvailability::Available
                ),
                Decision::Forget
            ));
        }
        let mut account = owner();
        account.account = "other-account".into();
        assert!(matches!(
            decide(
                &text,
                &account,
                Some(&marker),
                true,
                CodexQuotaAvailability::Available
            ),
            Decision::Forget
        ));
    }
    #[test]
    fn quota_fallback_excludes_official_profiles_and_missing_keys() {
        for text in [
            CONFIG.replace(
                "model_provider = \"company\"",
                "model_provider = \"openai\"",
            ),
            CONFIG.replace("test-key", ""),
            format!("profile = \"work\"\n{CONFIG}"),
            CONFIG.replace("test-key", "PROXY_MANAGED"),
        ] {
            assert!(change_auth_flag(&text, true).is_none());
        }
    }

    #[test]
    fn official_mirror_override_restores_only_its_flag_and_injected_bearer() {
        let url = "http://127.0.0.1:15721/v1";
        let original = format!("model_provider = \"custom\"\nmodel = \"official-model\"\n[model_providers.custom]\nname = \"OpenAI\"\nbase_url = \"{url}\"\nrequires_openai_auth = true\nwire_api = \"responses\"\n[model_providers.user]\nname = \"User\"\nbase_url = \"https://user.example/v1\"\nexperimental_bearer_token = \"user-key\"\n");
        let mut owner = owner();
        owner.provider = "proxy:official:members-hash".into();
        owner.proxy_url = Some(url.into());
        let Decision::Apply(applied, marker) = decide(
            &original,
            &owner,
            None,
            true,
            CodexQuotaAvailability::Exhausted,
        ) else {
            panic!("expected override");
        };
        assert!(matches!(marker.undo, Undo::InjectedBearer));
        let applied = format!("{applied}\n[mcp_servers.user]\ncommand = \"keep-me\"\n");
        let restored = restore_override(&applied, &marker).unwrap();
        let restored: toml::Value = restored.parse().unwrap();
        let original: toml::Value = original.parse().unwrap();
        assert_eq!(restored["model_provider"], original["model_provider"]);
        assert_eq!(restored["model_providers"], original["model_providers"]);
        assert_eq!(
            restored["mcp_servers"]["user"]["command"].as_str(),
            Some("keep-me")
        );
        let edited = applied.replace("name = \"OpenAI\"", "name = \"User edited\"");
        assert!(restore_override(&edited, &marker).is_none());
        for selector in ["", "model_provider = \"openai\"\n"] {
            let builtin = format!("{selector}openai_base_url = \"{url}\"\n[model_providers.custom]\nname = \"User-owned\"\n");
            assert!(apply_override(&builtin, Some(url)).is_none());
        }
    }

    #[test]
    fn proxy_placeholder_requires_exact_verified_loopback_route() {
        let url = "http://127.0.0.1:15721/v1";
        let local = CONFIG
            .replace("https://relay.example/v1", url)
            .replace("test-key", "PROXY_MANAGED");
        assert!(change_auth_flag(&local, true).is_none());
        assert!(change_auth_flag_for_route(&local, true, Some(url)).is_some());
        for expected in [
            "http://127.0.0.1:15722/v1",
            "http://192.0.2.1:15721/v1",
            "https://relay.example/v1",
        ] {
            assert!(change_auth_flag_for_route(&local, true, Some(expected)).is_none());
        }
        let remote = local.replace(url, "https://relay.example/v1");
        assert!(
            change_auth_flag_for_route(&remote, true, Some("https://relay.example/v1")).is_none()
        );
        let remote_with_env = format!("{remote}env_key = \"API_KEY\"\n");
        assert!(change_auth_flag(&remote_with_env, true).is_none());
        let ipv6 = "http://[::1]:15721/v1";
        assert!(change_auth_flag_for_route(&local.replace(url, ipv6), true, Some(ipv6)).is_some());

        let mut proxy_owner = owner();
        proxy_owner.provider = "proxy:company".into();
        proxy_owner.proxy_url = Some(url.into());
        let Decision::Apply(live, marker) = decide(
            &local,
            &proxy_owner,
            None,
            true,
            CodexQuotaAvailability::Exhausted,
        ) else {
            panic!("proxy fallback expected");
        };
        assert_eq!(
            refresh_overlay("proxy:company", &local, &live, &marker),
            live
        );
        assert_eq!(refresh_overlay("company", &local, &live, &marker), local);
        assert_eq!(
            refresh_overlay("proxy:other", &local, &live, &marker),
            local
        );
        assert!(matches!(
            decide(
                CONFIG,
                &owner(),
                Some(&marker),
                true,
                CodexQuotaAvailability::Available
            ),
            Decision::Forget
        ));
    }
}
