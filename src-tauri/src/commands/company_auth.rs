use std::sync::Arc;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::State;
use tokio::sync::RwLock;

use crate::app_config::AppType;
use crate::auth_client::{
    feishu_login_start, AuthState, CatalogEntry, FeishuLoginStart, LoginResponse,
    ProductionAuthService,
};
use crate::provider::{Provider, ProviderMeta};
use crate::services::ProviderService;
use crate::store::AppState;
use toml_edit::DocumentMut;

pub struct CompanyAuthState(pub Arc<RwLock<ProductionAuthService>>);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanyProviderSyncResult {
    pub synced: usize,
}

#[tauri::command]
pub async fn company_auth_get_state(
    state: State<'_, CompanyAuthState>,
) -> Result<AuthState, String> {
    state
        .0
        .read()
        .await
        .get_state()
        .await
        .map_err(|err| err.to_command_error())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn company_auth_login_with_ticket(
    ticket: String,
    state: State<'_, CompanyAuthState>,
) -> Result<AuthState, String> {
    state
        .0
        .write()
        .await
        .login_with_ticket(&ticket)
        .await
        .map_err(|err| err.to_command_error())
}

#[tauri::command]
pub async fn company_auth_refresh_catalog(
    state: State<'_, CompanyAuthState>,
) -> Result<AuthState, String> {
    state
        .0
        .write()
        .await
        .refresh_catalog()
        .await
        .map_err(|err| err.to_command_error())
}

#[tauri::command]
pub async fn company_auth_catalog_version(
    state: State<'_, CompanyAuthState>,
) -> Result<String, String> {
    state
        .0
        .read()
        .await
        .catalog_version()
        .await
        .map_err(|err| err.to_command_error())
}

#[tauri::command]
pub async fn company_auth_sync_providers(
    auth_state: State<'_, CompanyAuthState>,
    app_state: State<'_, AppState>,
) -> Result<CompanyProviderSyncResult, String> {
    // Hold the guard through projection: logout/login must not overtake this snapshot.
    let auth_guard = auth_state.0.read().await;
    let login = auth_guard
        .current_login()
        .await
        .map_err(|err| err.to_command_error())?;

    sync_company_providers(app_state.inner(), &login).map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn company_auth_logout(state: State<'_, CompanyAuthState>) -> Result<AuthState, String> {
    state
        .0
        .write()
        .await
        .logout()
        .await
        .map_err(|err| err.to_command_error())
}

#[tauri::command]
pub async fn company_auth_start_feishu_login() -> Result<FeishuLoginStart, String> {
    Ok(feishu_login_start())
}

fn sync_company_providers(
    state: &AppState,
    login: &LoginResponse,
) -> Result<CompanyProviderSyncResult, crate::error::AppError> {
    let mut synced = 0;
    let created_at = current_timestamp_millis();
    let mut active_ids = std::collections::HashSet::new();

    for entry in &login.catalog {
        let Some((app_type, provider)) = company_catalog_provider(entry, login, created_at) else {
            continue;
        };

        active_ids.insert((app_type.as_str().to_string(), provider.id.clone()));
        upsert_company_provider(state, app_type, provider)?;
        synced += 1;
    }

    // Keep stale rows (including current/live and Stack members) and their CLI
    // credentials intact. Annotate only company-owned cards; never delete rows.
    for app_type in [AppType::Claude, AppType::Codex, AppType::Gemini] {
        let _switch_guard = crate::mode::controller::lock_settled_blocking(state, &app_type)?;
        for (_, mut provider) in state.db.get_all_providers(app_type.as_str())? {
            if !is_company_provider(&provider)
                || active_ids.contains(&(app_type.as_str().to_string(), provider.id.clone()))
            {
                continue;
            }
            let notes = provider.notes.get_or_insert_with(String::new);
            if !notes.contains(STALE_COMPANY_NOTE) {
                if !notes.is_empty() {
                    notes.push('\n');
                }
                notes.push_str(STALE_COMPANY_NOTE);
                state.db.save_provider(app_type.as_str(), &provider)?;
            }
        }
    }

    Ok(CompanyProviderSyncResult { synced })
}

const STALE_COMPANY_NOTE: &str = "已从公司目录移除（保留本地配置）";

fn is_company_provider(provider: &Provider) -> bool {
    provider.meta.as_ref().is_some_and(|meta| {
        meta.provider_type.as_deref() == Some("company_auth")
            || (meta.provider_type.is_none()
                && meta.partner_promotion_key.as_deref() == Some("company_pool"))
    })
}

fn upsert_company_provider(
    state: &AppState,
    app_type: AppType,
    provider: Provider,
) -> Result<(), crate::error::AppError> {
    let existing = state
        .db
        .get_provider_by_id(&provider.id, app_type.as_str())?;

    if let Some(mut existing) = existing {
        if !is_company_provider(&existing) {
            return Err(crate::error::AppError::InvalidInput(
                "公司目录卡片 ID 与个人配置冲突".to_string(),
            ));
        }
        // Keep card ordering, creation time, user notes and unrelated metadata.
        existing.name = provider.name;
        existing.settings_config = provider.settings_config;
        existing.website_url = provider.website_url;
        if let Some(notes) = existing.notes.as_mut() {
            *notes = notes
                .lines()
                .filter(|line| *line != STALE_COMPANY_NOTE)
                .collect::<Vec<_>>()
                .join("\n");
        }
        let generated_meta = provider.meta.expect("company provider metadata");
        let meta = existing.meta.get_or_insert_with(Default::default);
        meta.provider_type = generated_meta.provider_type;
        meta.partner_promotion_key = generated_meta.partner_promotion_key;
        meta.is_partner = generated_meta.is_partner;
        meta.api_format = generated_meta.api_format;
        ProviderService::update(state, app_type, None, existing)?;
    } else {
        // Catalog discovery must not replace an external CLI login/config.
        ProviderService::add_without_switching(state, app_type, provider)?;
    }

    Ok(())
}

fn company_catalog_provider(
    entry: &CatalogEntry,
    login: &LoginResponse,
    created_at: i64,
) -> Option<(AppType, Provider)> {
    let tool = entry.tool.trim().to_ascii_lowercase();
    let app_type = match tool.as_str() {
        "claude" | "claude-code" | "claude_code" => AppType::Claude,
        "codex" | "openai-codex" | "openai_codex" => AppType::Codex,
        "gemini" | "gemini-cli" | "gemini_cli" => AppType::Gemini,
        _ => return None,
    };

    let id = format!(
        "company-{}-{}-{}",
        app_type.as_str(),
        slugify_provider_id(&entry.label),
        stable_label_hash(&entry.label)
    );
    let settings_config = match app_type {
        AppType::Claude => company_claude_settings(entry, login),
        AppType::Codex => company_codex_settings(entry, login),
        AppType::Gemini => company_gemini_settings(entry, login),
        _ => return None,
    };

    let mut provider = Provider::with_id(
        id,
        entry.label.clone(),
        settings_config,
        Some(login.base_url.clone()),
    );
    provider.category = Some("custom".to_string());
    provider.created_at = Some(created_at);
    provider.notes = Some("由公司账号池登录自动同步".to_string());
    provider.icon = Some(
        match app_type {
            AppType::Claude => "anthropic",
            AppType::Codex => "openai",
            AppType::Gemini => "gemini",
            _ => "generic",
        }
        .to_string(),
    );
    provider.icon_color = Some(
        match app_type {
            AppType::Claude => "#D4915D",
            AppType::Codex => "#00A67E",
            AppType::Gemini => "#4285F4",
            _ => "#6B7280",
        }
        .to_string(),
    );
    provider.meta = Some(company_provider_meta(entry, &app_type));

    Some((app_type, provider))
}

fn company_claude_settings(entry: &CatalogEntry, login: &LoginResponse) -> Value {
    json!({
        "env": {
            "ANTHROPIC_BASE_URL": login.base_url,
            "ANTHROPIC_AUTH_TOKEN": login.api_key,
            "ANTHROPIC_MODEL": entry.default_model,
            "ANTHROPIC_DEFAULT_HAIKU_MODEL": entry.default_model,
            "ANTHROPIC_DEFAULT_SONNET_MODEL": entry.default_model,
            "ANTHROPIC_DEFAULT_OPUS_MODEL": entry.default_model
        }
    })
}

fn company_codex_settings(entry: &CatalogEntry, login: &LoginResponse) -> Value {
    let base_url = codex_responses_base_url(&login.base_url);
    let model_catalog: Vec<Value> = entry
        .models
        .iter()
        .map(|model| {
            let mut row = json!({
                "model": model,
                "displayName": model
            });
            // 服务端声明了该模型的能力就原样带上，供生成 Codex catalog 时使用。
            // 号池支持什么由服务端说了算，不再依赖使用者个人账号的本机缓存。
            if let Some(caps) = entry
                .model_capabilities
                .as_ref()
                .and_then(|caps| caps.get(model))
            {
                if let Some(obj) = row.as_object_mut() {
                    obj.insert("capabilities".to_string(), caps.clone());
                }
            }
            row
        })
        .collect();
    let config = format!(
        "model_provider = \"custom\"\n\
         model = {}\n\
         model_reasoning_effort = \"high\"\n\
         disable_response_storage = false\n\
         experimental_realtime_webrtc_call_base_url = {}\n\
         experimental_realtime_ws_base_url = {}\n\
         \n\
         [model_providers.custom]\n\
         name = {}\n\
         base_url = {}\n\
         wire_api = \"responses\"\n\
         requires_openai_auth = true\n",
        toml_string(&entry.default_model),
        toml_string(&base_url),
        toml_string(&base_url),
        toml_string(&entry.label),
        toml_string(&base_url)
    );

    json!({
        "auth": {
            "OPENAI_API_KEY": login.api_key
        },
        "config": config,
        "modelCatalog": {
            "models": model_catalog
        }
    })
}

fn company_gemini_settings(entry: &CatalogEntry, login: &LoginResponse) -> Value {
    json!({
        "env": {
            "GOOGLE_GEMINI_BASE_URL": login.base_url,
            "GEMINI_API_KEY": login.api_key,
            "GEMINI_MODEL": entry.default_model
        }
    })
}

fn company_provider_meta(entry: &CatalogEntry, app_type: &AppType) -> ProviderMeta {
    let mut meta = ProviderMeta {
        is_partner: Some(true),
        partner_promotion_key: Some("company_pool".to_string()),
        provider_type: Some("company_auth".to_string()),
        ..Default::default()
    };

    if entry.protocol.eq_ignore_ascii_case("openai-responses") {
        meta.api_format = Some("openai_responses".to_string());
    } else if entry.protocol.eq_ignore_ascii_case("openai-chat") {
        meta.api_format = Some("openai_chat".to_string());
    } else if matches!(app_type, AppType::Codex) {
        meta.api_format = Some("openai_responses".to_string());
    }

    meta
}

pub fn normalize_company_codex_providers(
    state: &AppState,
) -> Result<usize, crate::error::AppError> {
    let mut updated = 0;
    let providers = state.db.get_all_providers(AppType::Codex.as_str())?;

    for (_, mut provider) in providers {
        if !is_company_provider(&provider) {
            continue;
        }

        let mut changed = false;

        if let Some(config_text) = provider
            .settings_config
            .get("config")
            .and_then(|value| value.as_str())
        {
            let normalized = normalize_company_codex_config_text(config_text);
            if normalized != config_text {
                if let Some(obj) = provider.settings_config.as_object_mut() {
                    obj.insert("config".to_string(), Value::String(normalized));
                    changed = true;
                }
            }
        }

        if changed {
            ProviderService::update(state, AppType::Codex, None, provider)?;
            updated += 1;
        }
    }

    Ok(updated)
}

fn normalize_company_codex_config_text(config_text: &str) -> String {
    let Ok(mut doc) = config_text.parse::<DocumentMut>() else {
        let mut fallback = config_text
            .replace(r#"wire_api = "chat""#, r#"wire_api = "responses""#)
            .replace(
                r#"requires_openai_auth = false"#,
                r#"requires_openai_auth = true"#,
            );
        fallback = remove_config_key_lines(&fallback, &["disable_response_storage"]);
        fallback = remove_true_config_key_lines(&fallback, "supports_websockets");
        let mut additions = String::from("disable_response_storage = false\n");
        if let Some(base_url) = extract_codex_base_url_from_text(&fallback) {
            if !fallback.contains("experimental_realtime_webrtc_call_base_url") {
                additions.push_str(&format!(
                    "experimental_realtime_webrtc_call_base_url = {}\n",
                    toml_string(&base_url)
                ));
            }
            if !fallback.contains("experimental_realtime_ws_base_url") {
                additions.push_str(&format!(
                    "experimental_realtime_ws_base_url = {}\n",
                    toml_string(&base_url)
                ));
            }
        }
        fallback = insert_top_level_config_lines(&fallback, &additions);
        return fallback;
    };

    let provider_name = doc
        .get("model_provider")
        .and_then(|value| value.as_str())
        .unwrap_or("custom")
        .to_string();

    let realtime_base_url = doc
        .get("model_providers")
        .and_then(|item| item.as_table())
        .and_then(|table| table.get(&provider_name))
        .and_then(|item| item.as_table())
        .and_then(|table| table.get("base_url"))
        .and_then(|item| item.as_str())
        .or_else(|| doc.get("base_url").and_then(|item| item.as_str()))
        .map(codex_responses_base_url);

    doc["disable_response_storage"] = toml_edit::value(false);
    if let Some(base_url) = realtime_base_url {
        doc["experimental_realtime_webrtc_call_base_url"] = toml_edit::value(base_url.clone());
        doc["experimental_realtime_ws_base_url"] = toml_edit::value(base_url);
    }

    if let Some(table) = doc
        .get_mut("model_providers")
        .and_then(|item| item.as_table_mut())
        .and_then(|table| table.get_mut(&provider_name))
        .and_then(|item| item.as_table_mut())
    {
        table["wire_api"] = toml_edit::value("responses");
        table["requires_openai_auth"] = toml_edit::value(true);
        if table
            .get("supports_websockets")
            .and_then(|item| item.as_bool())
            == Some(true)
        {
            table.remove("supports_websockets");
        }
        if let Some(base_url) = table.get("base_url").and_then(|item| item.as_str()) {
            table["base_url"] = toml_edit::value(codex_responses_base_url(base_url));
        }
    } else {
        doc["wire_api"] = toml_edit::value("responses");
        doc["requires_openai_auth"] = toml_edit::value(true);
        if let Some(base_url) = doc.get("base_url").and_then(|item| item.as_str()) {
            doc["base_url"] = toml_edit::value(codex_responses_base_url(base_url));
        }
    }

    doc.to_string()
}

fn remove_config_key_lines(config_text: &str, keys: &[&str]) -> String {
    config_text
        .split_inclusive('\n')
        .filter(|line| {
            let trimmed = line.trim();
            let Some((key, _)) = trimmed.split_once('=') else {
                return true;
            };
            !keys.iter().any(|candidate| key.trim() == *candidate)
        })
        .collect()
}

fn remove_true_config_key_lines(config_text: &str, key_to_remove: &str) -> String {
    config_text
        .split_inclusive('\n')
        .filter(|line| {
            let trimmed = line.trim();
            let Some((key, value)) = trimmed.split_once('=') else {
                return true;
            };
            !(key.trim() == key_to_remove && value.trim() == "true")
        })
        .collect()
}

fn codex_responses_base_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1")
    }
}

fn extract_codex_base_url_from_text(config_text: &str) -> Option<String> {
    let mut provider_name = "custom".to_string();
    let mut section = None;

    for line in config_text.lines() {
        let trimmed = line.trim();
        if let Some(header) = trimmed
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            section = Some(header.trim().to_string());
            continue;
        }
        if section.is_some() {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        if key.trim() != "model_provider" {
            continue;
        }
        if let Some(value) = parse_toml_string_value(value.trim()) {
            provider_name = value;
            break;
        }
    }

    let active_provider_section = format!("model_providers.{provider_name}");
    let mut top_level_base_url = None;
    section = None;

    for line in config_text.lines() {
        let trimmed = line.trim();
        if let Some(header) = trimmed
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            section = Some(header.trim().to_string());
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        if key.trim() != "base_url" {
            continue;
        }
        let Some(base_url) = parse_toml_string_value(value.trim()) else {
            continue;
        };
        if section.as_deref() == Some(active_provider_section.as_str()) {
            return Some(codex_responses_base_url(&base_url));
        }
        if section.is_none() {
            top_level_base_url = Some(base_url);
        }
    }

    top_level_base_url.map(|base_url| codex_responses_base_url(&base_url))
}

fn parse_toml_string_value(value: &str) -> Option<String> {
    let document = format!("value = {value}\n").parse::<DocumentMut>().ok()?;
    document
        .get("value")
        .and_then(|item| item.as_str())
        .map(str::to_string)
}

fn insert_top_level_config_lines(config_text: &str, additions: &str) -> String {
    if additions.is_empty() {
        return config_text.to_string();
    }

    let insertion = if config_text.starts_with('[') {
        0
    } else {
        config_text
            .find("\n[")
            .map(|index| index + 1)
            .unwrap_or(config_text.len())
    };
    let mut normalized = String::with_capacity(config_text.len() + additions.len() + 1);
    normalized.push_str(&config_text[..insertion]);
    if insertion > 0 && !normalized.ends_with('\n') {
        normalized.push('\n');
    }
    normalized.push_str(additions);
    normalized.push_str(&config_text[insertion..]);
    normalized
}

fn slugify_provider_id(label: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;

    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }

    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "pool".to_string()
    } else {
        slug.to_string()
    }
}

fn stable_label_hash(label: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in label.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:08x}", hash as u32)
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn current_timestamp_millis() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_millis().min(i64::MAX as u128) as i64,
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry() -> CatalogEntry {
        CatalogEntry {
            tool: "codex".to_string(),
            label: "Company Codex".to_string(),
            protocol: "openai-responses".to_string(),
            default_model: "gpt-5.5".to_string(),
            models: vec!["gpt-5.5".to_string()],
            group: "default".to_string(),
            model_capabilities: None,
        }
    }

    fn sample_login() -> LoginResponse {
        LoginResponse {
            session_token: "session".to_string(),
            api_key: "api-key".to_string(),
            base_url: "https://leharrt.com".to_string(),
            user: crate::auth_client::UserIdentity {
                display_name: "Test User".to_string(),
                email: "test@example.com".to_string(),
            },
            catalog: vec![sample_entry()],
        }
    }

    struct IsolatedHome {
        _dir: tempfile::TempDir,
        previous: Option<std::ffi::OsString>,
    }

    impl IsolatedHome {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let previous = std::env::var_os("CC_SWITCH_TEST_HOME");
            std::env::set_var("CC_SWITCH_TEST_HOME", dir.path());
            crate::settings::reload_settings().unwrap();
            Self {
                _dir: dir,
                previous,
            }
        }
    }

    impl Drop for IsolatedHome {
        fn drop(&mut self) {
            match &self.previous {
                Some(previous) => std::env::set_var("CC_SWITCH_TEST_HOME", previous),
                None => std::env::remove_var("CC_SWITCH_TEST_HOME"),
            }
            crate::settings::reload_settings().unwrap();
        }
    }

    fn test_state() -> AppState {
        AppState::new(Arc::new(crate::database::Database::memory().unwrap()))
    }

    fn company_caps(window: u64) -> Value {
        json!({
            "supported_reasoning_levels": [{"effort": "high", "description": "High"}, {"effort": "ultra", "description": "Ultra"}],
            "default_reasoning_level": "high",
            "additional_speed_tiers": ["fast"],
            "service_tiers": [{"id": "priority", "name": "Priority", "description": "Priority tier"}],
            "context_window": window
        })
    }

    #[test]
    fn company_protocol_is_retained_for_codex_proxy_translation() {
        let mut entry = sample_entry();
        entry.protocol = "openai-chat".into();
        let (_, provider) = company_catalog_provider(&entry, &sample_login(), 1).unwrap();
        assert_eq!(
            provider.meta.unwrap().api_format.as_deref(),
            Some("openai_chat")
        );
    }

    #[test]
    #[serial_test::serial]
    fn normalization_preserves_declared_company_chat_protocol() {
        let _home = IsolatedHome::new();
        let state = test_state();
        let mut entry = sample_entry();
        entry.protocol = "openai-chat".into();
        let (_, provider) = company_catalog_provider(&entry, &sample_login(), 1).unwrap();
        state.db.save_provider("codex", &provider).unwrap();
        normalize_company_codex_providers(&state).unwrap();
        let normalized = state
            .db
            .get_provider_by_id(&provider.id, "codex")
            .unwrap()
            .unwrap();
        assert_eq!(
            normalized.meta.unwrap().api_format.as_deref(),
            Some("openai_chat")
        );
    }

    #[test]
    #[serial_test::serial]
    fn first_catalog_sync_preserves_external_cli_and_personal_oauth() {
        let _home = IsolatedHome::new();
        let state = test_state();
        let auth_path = crate::codex_config::get_codex_auth_path();
        let config_path = crate::codex_config::get_codex_config_path();
        std::fs::create_dir_all(auth_path.parent().unwrap()).unwrap();
        let oauth =
            br#"{"tokens":{"access_token":"fixture-personal-oauth"},"OPENAI_API_KEY":null}"#;
        let config = "model = \"personal-model\"\n";
        std::fs::write(&auth_path, oauth).unwrap();
        std::fs::write(&config_path, config).unwrap();
        sync_company_providers(&state, &sample_login()).unwrap();
        assert_eq!(std::fs::read(&auth_path).unwrap(), oauth);
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), config);
        assert!(ProviderService::current(&state, AppType::Codex)
            .unwrap()
            .is_empty());
    }

    #[test]
    #[serial_test::serial]
    fn company_sync_rejects_personal_card_id_collision() {
        let _home = IsolatedHome::new();
        let state = test_state();
        let (_, mut personal) =
            company_catalog_provider(&sample_entry(), &sample_login(), 1).unwrap();
        personal.meta = None;
        personal.name = "Personal".into();
        personal.settings_config["auth"]["OPENAI_API_KEY"] = json!("personal-key");
        state.db.save_provider("codex", &personal).unwrap();
        assert!(sync_company_providers(&state, &sample_login()).is_err());
        assert_eq!(
            state
                .db
                .get_provider_by_id(&personal.id, "codex")
                .unwrap()
                .unwrap()
                .settings_config,
            personal.settings_config
        );
        assert_eq!(normalize_company_codex_providers(&state).unwrap(), 0);
    }

    #[test]
    #[serial_test::serial]
    fn stale_catalog_keeps_company_current_and_personal_cards() {
        let _home = IsolatedHome::new();
        let state = test_state();
        let (_, company) = company_catalog_provider(&sample_entry(), &sample_login(), 1).unwrap();
        state.db.save_provider("codex", &company).unwrap();
        state.db.set_current_provider("codex", &company.id).unwrap();
        let personal = Provider::with_id(
            "personal".into(),
            "Personal".into(),
            json!({"auth": {"OPENAI_API_KEY": "personal-key"}}),
            None,
        );
        state.db.save_provider("codex", &personal).unwrap();
        let login = LoginResponse {
            catalog: vec![],
            ..sample_login()
        };
        sync_company_providers(&state, &login).unwrap();
        let stale = state
            .db
            .get_provider_by_id(&company.id, "codex")
            .unwrap()
            .unwrap();
        assert_eq!(stale.settings_config, company.settings_config);
        assert!(stale.notes.unwrap().contains("已从公司目录移除"));
        assert_eq!(
            state
                .db
                .get_provider_by_id("personal", "codex")
                .unwrap()
                .unwrap()
                .settings_config,
            personal.settings_config
        );
    }

    #[test]
    #[serial_test::serial]
    fn capability_only_sync_regenerates_live_catalog_without_touching_personal_card() {
        let _home = IsolatedHome::new();
        let state = test_state();
        let mut login = sample_login();
        login.catalog[0].model_capabilities = Some(std::collections::BTreeMap::from([(
            "gpt-5.5".into(),
            company_caps(272000),
        )]));
        let (_, company) = company_catalog_provider(&login.catalog[0], &login, 1).unwrap();
        let mut personal = Provider::with_id(
            "personal".into(),
            "Personal".into(),
            json!({"auth": {"tokens": {"access_token": "fixture-personal-oauth"}}, "config": "model = \"personal-model\"\n"}),
            None,
        );
        personal.category = Some("official".into());
        state.db.save_provider("codex", &personal).unwrap();
        sync_company_providers(&state, &login).unwrap();
        ProviderService::switch(&state, AppType::Codex, &company.id).unwrap();
        let catalog_path = crate::codex_config::get_codex_model_catalog_path();
        let before: Value = serde_json::from_slice(&std::fs::read(&catalog_path).unwrap()).unwrap();
        assert_eq!(before["models"][0]["context_window"], 272000);
        login.catalog[0].model_capabilities = Some(std::collections::BTreeMap::from([(
            "gpt-5.5".into(),
            company_caps(400000),
        )]));
        login.api_key = "rotated-company-key".into();
        sync_company_providers(&state, &login).unwrap();
        let after: Value = serde_json::from_slice(&std::fs::read(&catalog_path).unwrap()).unwrap();
        assert_eq!(after["models"][0]["context_window"], 400000);
        assert_eq!(after["models"][0]["default_reasoning_level"], "high");
        assert_eq!(
            after["models"][0]["additional_speed_tiers"],
            json!(["fast"])
        );
        assert_eq!(
            after["models"][0]["service_tiers"],
            company_caps(400000)["service_tiers"]
        );
        assert_eq!(
            after["models"][0]["supported_reasoning_levels"],
            company_caps(400000)["supported_reasoning_levels"]
        );
        assert_eq!(
            state
                .db
                .get_provider_by_id(&company.id, "codex")
                .unwrap()
                .unwrap()
                .settings_config["auth"]["OPENAI_API_KEY"],
            "rotated-company-key"
        );
        assert_eq!(
            state
                .db
                .get_provider_by_id("personal", "codex")
                .unwrap()
                .unwrap()
                .settings_config,
            personal.settings_config
        );
        ProviderService::switch(&state, AppType::Codex, "personal").unwrap();
        let auth: Value = serde_json::from_slice(
            &std::fs::read(crate::codex_config::get_codex_auth_path()).unwrap(),
        )
        .unwrap();
        assert_eq!(auth["tokens"]["access_token"], "fixture-personal-oauth");
    }

    #[test]
    fn generated_company_codex_config_disables_response_storage_without_forcing_websocket() {
        let value = company_codex_settings(&sample_entry(), &sample_login());
        let config = value
            .get("config")
            .and_then(Value::as_str)
            .expect("company Codex config");
        let document = config
            .parse::<DocumentMut>()
            .expect("generated config must be valid TOML");

        assert_eq!(
            document
                .get("disable_response_storage")
                .and_then(|item| item.as_bool()),
            Some(false)
        );
        assert_eq!(
            document
                .get("experimental_realtime_webrtc_call_base_url")
                .and_then(|item| item.as_str()),
            Some("https://leharrt.com/v1")
        );
        assert_eq!(
            document
                .get("experimental_realtime_ws_base_url")
                .and_then(|item| item.as_str()),
            Some("https://leharrt.com/v1")
        );
        assert_eq!(
            document["model_providers"]["custom"]["name"].as_str(),
            Some("Company Codex")
        );
        assert!(document["model_providers"]["custom"]
            .as_table()
            .expect("custom provider table")
            .get("supports_websockets")
            .is_none());
    }

    #[test]
    fn normalizing_existing_company_codex_config_removes_enabled_websocket_setting() {
        let input = r#"model_provider = "custom"
disable_response_storage = true

[model_providers.custom]
wire_api = "chat"
requires_openai_auth = false
supports_websockets = true
base_url = "https://leharrt.com"
"#;

        let normalized = normalize_company_codex_config_text(input);
        let document = normalized
            .parse::<DocumentMut>()
            .expect("normalized config must be valid TOML");

        assert_eq!(
            document
                .get("disable_response_storage")
                .and_then(|item| item.as_bool()),
            Some(false)
        );
        assert_eq!(
            document
                .get("experimental_realtime_webrtc_call_base_url")
                .and_then(|item| item.as_str()),
            Some("https://leharrt.com/v1")
        );
        assert_eq!(
            document
                .get("experimental_realtime_ws_base_url")
                .and_then(|item| item.as_str()),
            Some("https://leharrt.com/v1")
        );
        assert_eq!(
            document["model_providers"]["custom"]["wire_api"].as_str(),
            Some("responses")
        );
        assert_eq!(
            document["model_providers"]["custom"]["requires_openai_auth"].as_bool(),
            Some(true)
        );
        assert!(document["model_providers"]["custom"]
            .as_table()
            .expect("custom provider table")
            .get("supports_websockets")
            .is_none());
    }

    #[test]
    fn malformed_config_uses_active_provider_base_url_for_realtime_settings() {
        let input = r#"model_provider = "custom"

[mcp_servers.example]
base_url = "https://mcp.example"

[model_providers.custom]
base_url = "https://leharrt.com"

this is not valid toml
"#;

        let normalized = normalize_company_codex_config_text(input);
        let first_table = normalized.find('[').expect("table header");
        let top_level = &normalized[..first_table];

        assert!(top_level
            .contains("experimental_realtime_webrtc_call_base_url = \"https://leharrt.com/v1\""));
        assert!(
            top_level.contains("experimental_realtime_ws_base_url = \"https://leharrt.com/v1\"")
        );
        assert!(!top_level.contains("https://mcp.example"));
    }
}
