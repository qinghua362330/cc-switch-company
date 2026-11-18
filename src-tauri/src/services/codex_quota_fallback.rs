//! One-time migration of old quota fallback ownership; never writes auth.json.
use super::subscription;
use crate::{codex_config, config};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
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
        active_table_for_route(&doc, None)?;
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

/// One-time migration of an old, verified quota fallback. Never applies a new
/// login-mode override; changes to route, account or credentials are left alone.
pub(crate) fn migrate_owned_override() -> Result<bool, ()> {
    let marker = match load_marker()? {
        Some(marker) => marker,
        None => return Ok(false),
    };
    let path = codex_config::get_codex_config_path();
    if marker.path != digest(&path.to_string_lossy())
        || subscription::codex_login_quota_identity().as_deref() != Some(marker.account.as_str())
    {
        return Ok(false);
    }
    let _write = codex_config::codex_live_write_lock();
    let live = codex_config::read_codex_config_text().map_err(|_| ())?;
    let Some(restored) = restore_override(&live, &marker) else {
        // The user changed the route or credentials; retire our stale marker
        // without modifying any user-owned configuration.
        std::fs::remove_file(marker_path()).map_err(|_| ())?;
        return Ok(false);
    };
    super::codex_app_refresh::persist_refresh_intent(
        &marker_path().with_file_name("codex-login-mode-refresh.json"),
        &path,
        &restored,
        &marker.account,
    )?;
    config::atomic_write_private(&path, restored.as_bytes()).map_err(|_| ())?;
    std::fs::remove_file(marker_path()).map_err(|_| ())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "model_provider = \"company\"\nmodel = \"company-model\"\n[model_providers.company]\nname = \"Company\"\nbase_url = \"https://relay.example/v1\"\nexperimental_bearer_token = \"test-key\"\nrequires_openai_auth = true\n";

    fn marker(text: &str, proxy_url: Option<&str>) -> Ownership {
        Ownership {
            account: "account-hash".into(),
            provider: "company".into(),
            path: "path-hash".into(),
            original: "unused".into(),
            applied: routing_digest(text, proxy_url).unwrap(),
            proxy_url: proxy_url.map(str::to_string),
            undo: Undo::Flag,
        }
    }

    #[test]
    fn restores_only_owned_auth_flag_and_keeps_unrelated_edits() {
        let applied = change_auth_flag_for_route(CONFIG, true, None).unwrap();
        let owner = marker(&applied, None);
        let edited = format!("{applied}\n[mcp_servers.test]\ncommand = \"test-server\"\n");
        let restored = restore_override(&edited, &owner).unwrap();
        assert!(restored.contains("requires_openai_auth = true"));
        assert!(restored.contains("command = \"test-server\""));
        assert!(restore_override(&restored, &owner).is_none());
    }

    #[test]
    fn refuses_changed_route_or_credential_and_unverified_proxy() {
        let applied = change_auth_flag_for_route(CONFIG, true, None).unwrap();
        let owner = marker(&applied, None);
        assert!(restore_override(&applied.replace("test-key", "user-key"), &owner).is_none());
        assert!(
            restore_override(&applied.replace("relay.example", "other.example"), &owner).is_none()
        );
        let proxy = "http://127.0.0.1:15721/v1";
        let applied_proxy = applied.replace("https://relay.example/v1", proxy);
        let proxy_owner = marker(&applied_proxy, Some(proxy));
        assert!(restore_override(&applied_proxy, &proxy_owner).is_some());
        assert!(restore_override(
            &applied_proxy.replace(proxy, "http://127.0.0.2:15721/v1"),
            &proxy_owner
        )
        .is_none());
    }

    #[test]
    fn removes_only_injected_proxy_bearer() {
        let proxy = "http://127.0.0.1:15721/v1";
        let text = CONFIG
            .replace("https://relay.example/v1", proxy)
            .replace(
                "experimental_bearer_token = \"test-key\"\n",
                "experimental_bearer_token = \"PROXY_MANAGED\"\n",
            )
            .replace(
                "requires_openai_auth = true",
                "requires_openai_auth = false",
            );
        let mut owner = marker(&text, Some(proxy));
        owner.undo = Undo::InjectedBearer;
        let restored = restore_override(&text, &owner).unwrap();
        assert!(!restored.contains("PROXY_MANAGED"));
        assert!(restored.contains("requires_openai_auth = true"));
    }
}
