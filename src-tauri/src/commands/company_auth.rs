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
        if let Some(compaction_entry) = company_compaction_entry(entry, &login.base_url) {
            let (app_type, provider) =
                company_catalog_provider(&compaction_entry, login, created_at)
                    .expect("derived Codex catalog entry");
            if entry
                .models
                .iter()
                .any(|m| m != &compaction_entry.default_model)
                || state
                    .db
                    .get_provider_by_id(&provider.id, app_type.as_str())?
                    .is_some()
            {
                active_ids.insert((app_type.as_str().to_string(), provider.id.clone()));
                upsert_company_provider(state, app_type, provider)?;
                synced += 1;
            }
        }
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
// Keep the old default and label-derived card identity when expanding its catalog.
const VERIFIED_COMPACTION_MODEL: &str = "gpt-5.6-sol";
// Exact production compaction + continuation verification, 2026-11-03.
// A model prefix or server catalog membership alone does not establish support.
const VERIFIED_COMPACTION_MODELS: &[&str] = &[
    "gpt-6.1-terra",
    "gpt-5.6-luna",
    "codex-auto-review",
    "gpt-5.6-sol",
    "gpt-6-sol",
    "gpt-6.1-sol",
    "gpt-6-astra",
    "gpt-6-luna",
    "gpt-5.6-terra",
];

fn verified_compaction_model(model: &str) -> bool {
    VERIFIED_COMPACTION_MODELS.contains(&model)
}

fn verified_compaction_catalog<'a>(
    selected: Option<&str>,
    models: impl IntoIterator<Item = Option<&'a str>>,
) -> bool {
    let Some(selected) = selected.filter(|model| verified_compaction_model(model)) else {
        return false;
    };
    let mut contains_selected = false;
    for model in models {
        let Some(model) = model.filter(|model| verified_compaction_model(model)) else {
            return false;
        };
        contains_selected |= model == selected;
    }
    // Empty, malformed, unverified and externally selected models fail closed.
    contains_selected
}

fn verified_compaction_base(base: &str) -> bool {
    url::Url::parse(base).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some("leharrt.com")
            && url.port_or_known_default() == Some(443)
            && matches!(url.path().trim_end_matches('/'), "" | "/v1")
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    })
}

fn company_compaction_entry(entry: &CatalogEntry, base: &str) -> Option<CatalogEntry> {
    if !matches!(
        entry.tool.trim().to_ascii_lowercase().as_str(),
        "codex" | "openai-codex" | "openai_codex"
    ) || !entry.protocol.eq_ignore_ascii_case("openai-responses")
        || !verified_compaction_base(base)
    {
        return None;
    }
    let mut derived = entry.clone();
    derived.label = format!("{} · 远程压缩 V2", entry.label);
    derived
        .models
        .retain(|model| verified_compaction_model(model));
    if derived.models.is_empty() {
        return None;
    }
    derived.default_model = if derived
        .models
        .iter()
        .any(|m| m == VERIFIED_COMPACTION_MODEL)
    {
        VERIFIED_COMPACTION_MODEL.to_string()
    } else if derived.models.contains(&entry.default_model) {
        entry.default_model.clone()
    } else {
        derived.models[0].clone()
    };
    if let Some(caps) = derived.model_capabilities.as_mut() {
        caps.retain(|model, _| derived.models.contains(model));
    }
    Some(derived)
}

fn is_company_provider(provider: &Provider) -> bool {
    provider.meta.as_ref().is_some_and(|meta| {
        meta.provider_type.as_deref() == Some("company_auth")
            || (meta.provider_type.is_none()
                && meta.partner_promotion_key.as_deref() == Some("company_pool"))
    })
}

/// Only explicit form fields survive an automatic company catalog refresh.
fn preserve_company_catalog_overrides(old: Option<&Value>, settings: &mut Value) {
    let Some(old_rows) = old
        .and_then(|catalog| catalog.get("models"))
        .and_then(Value::as_array)
    else {
        return;
    };
    let Some(new_rows) = settings
        .get_mut("modelCatalog")
        .and_then(|catalog| catalog.get_mut("models"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for row in new_rows {
        let Some(model) = row.get("model").and_then(Value::as_str) else {
            continue;
        };
        let Some(previous) = old_rows
            .iter()
            .find(|old| old.get("model").and_then(Value::as_str) == Some(model))
        else {
            continue;
        };
        let Some(object) = row.as_object_mut() else {
            continue;
        };
        // Match the parser's camelCase-first alias precedence, and normalize
        // saved overrides without retaining stale automatic capabilities.
        for (key, alias) in [
            ("displayName", "display_name"),
            ("contextWindow", "context_window"),
            ("supportsParallelToolCalls", "supports_parallel_tool_calls"),
            ("inputModalities", "input_modalities"),
            ("baseInstructions", "base_instructions"),
            ("reasoningLevels", "reasoning_levels"),
            ("defaultReasoningLevel", "default_reasoning_level"),
        ] {
            if let Some(value) = previous.get(key).or_else(|| previous.get(alias)) {
                object.insert(key.to_string(), value.clone());
            }
        }
    }
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
        let old_config = existing
            .settings_config
            .get("config")
            .and_then(Value::as_str)
            .and_then(|text| text.parse::<DocumentMut>().ok());
        let preserve_disabled_websocket = matches!(app_type, AppType::Codex)
            && existing
                .settings_config
                .get("config")
                .and_then(Value::as_str)
                .and_then(|text| text.parse::<DocumentMut>().ok())
                .is_some_and(|doc| {
                    let name = doc
                        .get("model_provider")
                        .and_then(|v| v.as_str())
                        .unwrap_or("custom");
                    doc.get("model_providers")
                        .and_then(|v| v.as_table_like())
                        .and_then(|v| v.get(name))
                        .and_then(|v| v.as_table_like())
                        .and_then(|v| v.get("supports_websockets"))
                        .or_else(|| doc.get("supports_websockets"))
                        .and_then(|v| v.as_bool())
                        == Some(false)
                });
        let old_catalog = existing.settings_config.get("modelCatalog").cloned();
        existing.name = provider.name;
        existing.settings_config = provider.settings_config;
        // Refresh automatic metadata, but keep explicit edits for models that
        // still exist. Never copy old capabilities: they may be stale.
        if matches!(app_type, AppType::Codex) {
            preserve_company_catalog_overrides(old_catalog.as_ref(), &mut existing.settings_config);
        }
        if preserve_disabled_websocket {
            if let Some(config) = existing
                .settings_config
                .get("config")
                .and_then(Value::as_str)
            {
                if let Ok(mut doc) = config.parse::<DocumentMut>() {
                    doc["model_providers"]["custom"]["supports_websockets"] =
                        toml_edit::value(false);
                    existing.settings_config["config"] = Value::String(doc.to_string());
                }
            }
        }
        if matches!(app_type, AppType::Codex) {
            if let (Some(old), Some(new_text)) = (
                old_config,
                existing
                    .settings_config
                    .get("config")
                    .and_then(Value::as_str),
            ) {
                if let Ok(mut new) = new_text.parse::<DocumentMut>() {
                    for key in ["model_context_window", "model_auto_compact_token_limit"] {
                        if let Some(value) = old
                            .get(key)
                            .filter(|value| value.as_integer().is_some_and(|n| n > 0))
                        {
                            new[key] = value.clone();
                        }
                    }
                    if let Some(features) = old.get("features").and_then(|v| v.as_table_like()) {
                        let mut retained = toml_edit::Table::new();
                        for (key, value) in features
                            .iter()
                            .filter(|(key, _)| *key != "remote_compaction_v2")
                        {
                            retained.insert(key, value.clone());
                        }
                        if !retained.is_empty() {
                            new["features"] = toml_edit::Item::Table(retained);
                        }
                    }
                    let old_name = old
                        .get("model_provider")
                        .and_then(|v| v.as_str())
                        .unwrap_or("custom");
                    if let Some(caps) = old
                        .get("model_providers")
                        .and_then(|v| v.get(old_name))
                        .and_then(|v| v.get("capabilities"))
                        .and_then(|v| v.as_table_like())
                    {
                        for (key, value) in caps.iter() {
                            if new
                                .get("model_providers")
                                .and_then(|v| v.get("custom"))
                                .and_then(|v| v.get("capabilities"))
                                .and_then(|v| v.get(key))
                                .is_none()
                            {
                                new["model_providers"]["custom"]["capabilities"][key] =
                                    if key == "remote_compaction" {
                                        toml_edit::value("unsupported")
                                    } else {
                                        value.clone()
                                    };
                            }
                        }
                    }
                    existing.settings_config["config"] = Value::String(new.to_string());
                }
            }
        }
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
            // 自动快照仅作为本机 bundled 目录未命中时的兜底，不提升为用户显式覆盖。
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
    let mut config = format!(
        "model_provider = \"custom\"\n\
         model = {}\n\
         model_reasoning_effort = \"high\"\n\
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
    if entry.protocol.eq_ignore_ascii_case("openai-responses")
        && verified_compaction_base(&login.base_url)
        && verified_compaction_catalog(
            Some(&entry.default_model),
            entry.models.iter().map(|model| Some(model.as_str())),
        )
    {
        config.push_str("\n[model_providers.custom.capabilities]\nremote_compaction = \"v2\"\n");
    }

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
            let mut normalized = normalize_company_codex_config_text(config_text);
            if let Ok(mut doc) = normalized.parse::<DocumentMut>() {
                let name = doc
                    .get("model_provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("custom")
                    .to_string();
                let base = doc
                    .get("model_providers")
                    .and_then(|v| v.get(&name))
                    .and_then(|v| v.get("base_url"))
                    .and_then(|v| v.as_str());
                let models = provider
                    .settings_config
                    .get("modelCatalog")
                    .and_then(|v| v.get("models"))
                    .and_then(Value::as_array);
                let supported = provider.meta.as_ref().and_then(|m| m.api_format.as_deref())
                    == Some("openai_responses")
                    && crate::live::project::codex::foreign_catalog(&doc).is_none()
                    && base.is_some_and(verified_compaction_base)
                    && models.is_some_and(|rows| {
                        verified_compaction_catalog(
                            doc.get("model").and_then(|v| v.as_str()),
                            rows.iter()
                                .map(|row| row.get("model").and_then(Value::as_str)),
                        )
                    });
                if supported {
                    doc["model_providers"][&name]["capabilities"]["remote_compaction"] =
                        toml_edit::value("v2");
                    normalized = doc.to_string();
                } else if doc
                    .get("model_providers")
                    .and_then(|v| v.get(&name))
                    .and_then(|v| v.get("capabilities"))
                    .and_then(|v| v.get("remote_compaction"))
                    .is_some()
                {
                    doc["model_providers"][&name]["capabilities"]["remote_compaction"] =
                        toml_edit::value("unsupported");
                    normalized = doc.to_string();
                }
            }
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
        let mut additions = String::new();
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
        .and_then(|item| item.as_table_like())
        .and_then(|table| table.get(&provider_name))
        .and_then(|item| item.as_table_like())
        .and_then(|table| table.get("base_url"))
        .and_then(|item| item.as_str())
        .or_else(|| doc.get("base_url").and_then(|item| item.as_str()))
        .map(codex_responses_base_url);

    doc.as_table_mut().remove("disable_response_storage");
    if let Some(features) = doc.get_mut("features").and_then(|v| v.as_table_like_mut()) {
        features.remove("remote_compaction_v2");
    }
    if let Some(base_url) = realtime_base_url {
        doc["experimental_realtime_webrtc_call_base_url"] = toml_edit::value(base_url.clone());
        doc["experimental_realtime_ws_base_url"] = toml_edit::value(base_url);
    }

    if let Some(table) = doc
        .get_mut("model_providers")
        .and_then(|item| item.as_table_like_mut())
        .and_then(|table| table.get_mut(&provider_name))
        .and_then(|item| item.as_table_like_mut())
    {
        table.insert("wire_api", toml_edit::value("responses"));
        table.insert("requires_openai_auth", toml_edit::value(true));
        if table
            .get("supports_websockets")
            .and_then(|item| item.as_bool())
            == Some(true)
        {
            table.remove("supports_websockets");
        }
        if let Some(base_url) = table.get("base_url").and_then(|item| item.as_str()) {
            let base_url = codex_responses_base_url(base_url);
            table.insert("base_url", toml_edit::value(base_url));
        }
    } else {
        if doc
            .get("supports_websockets")
            .and_then(|item| item.as_bool())
            == Some(true)
        {
            doc.as_table_mut().remove("supports_websockets");
        }
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
            let value = value.split('#').next().unwrap_or(value).trim();
            !(key.trim() == key_to_remove && value == "true")
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
        login.catalog[0].default_model = "company-unlisted-model".into();
        login.catalog[0].models = vec!["company-unlisted-model".into()];
        login.catalog[0].model_capabilities = Some(std::collections::BTreeMap::from([(
            "company-unlisted-model".into(),
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
            "company-unlisted-model".into(),
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
    fn company_refresh_keeps_explicit_fields_but_replaces_snapshot() {
        let old = json!({"models":[{"model":"m","contextWindow":300000,
            "inputModalities":["text"],"capabilities":{"context_window":128000}},
            {"model":"removed","contextWindow":100000}]});
        let mut new = json!({"modelCatalog":{"models":[{"model":"m",
            "capabilities":{"context_window":500000}}, {"model":"added"}]}});
        preserve_company_catalog_overrides(Some(&old), &mut new);
        let rows = new["modelCatalog"]["models"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["contextWindow"], 300000);
        assert_eq!(rows[0]["inputModalities"], json!(["text"]));
        assert_eq!(rows[0]["capabilities"]["context_window"], 500000);
        assert!(rows[1].get("contextWindow").is_none());
    }

    #[test]
    fn company_refresh_preserves_snake_case_overrides_with_camel_case_precedence() {
        let old = json!({"models":[{
            "model":"m", "context_window":300000, "display_name":"Custom",
            "supports_parallel_tool_calls":false, "input_modalities":["text"],
            "base_instructions":"Explicit prompt", "reasoning_levels":["high"],
            "default_reasoning_level":"high"
        }, {"model":"both", "contextWindow":400000, "context_window":300000}]});
        let mut new = json!({"modelCatalog":{"models":[
            {"model":"m", "capabilities":{"context_window":500000}}, {"model":"both"}
        ]}});
        preserve_company_catalog_overrides(Some(&old), &mut new);
        let rows = new["modelCatalog"]["models"].as_array().unwrap();
        assert_eq!(rows[0]["contextWindow"], 300000);
        assert_eq!(rows[0]["displayName"], "Custom");
        assert_eq!(rows[0]["supportsParallelToolCalls"], false);
        assert_eq!(rows[0]["inputModalities"], json!(["text"]));
        assert_eq!(rows[0]["baseInstructions"], "Explicit prompt");
        assert_eq!(rows[0]["reasoningLevels"], json!(["high"]));
        assert_eq!(rows[0]["defaultReasoningLevel"], "high");
        assert_eq!(rows[0]["capabilities"]["context_window"], 500000);
        assert!(rows[0].get("context_window").is_none());
        assert_eq!(rows[1]["contextWindow"], 400000);
    }

    #[test]
    fn generated_company_codex_config_uses_default_storage_without_forcing_websocket() {
        let value = company_codex_settings(&sample_entry(), &sample_login());
        let config = value
            .get("config")
            .and_then(Value::as_str)
            .expect("company Codex config");
        let document = config
            .parse::<DocumentMut>()
            .expect("generated config must be valid TOML");

        assert!(document.get("disable_response_storage").is_none());
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

        assert!(document.get("disable_response_storage").is_none());
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
    #[serial_test::serial]
    fn company_catalog_refresh_cleans_legacy_transport_in_database_and_live_config() {
        for websocket in [true, false] {
            let _home = IsolatedHome::new();
            let state = test_state();
            let login = sample_login();
            let (_, mut provider) = company_catalog_provider(&sample_entry(), &login, 1).unwrap();
            let mut doc = provider.settings_config["config"]
                .as_str()
                .unwrap()
                .parse::<DocumentMut>()
                .unwrap();
            doc["disable_response_storage"] = toml_edit::value(false);
            doc["model_providers"]["custom"]["supports_websockets"] = toml_edit::value(websocket);
            provider.settings_config["config"] = Value::String(doc.to_string());
            state.db.save_provider("codex", &provider).unwrap();
            ProviderService::switch(&state, AppType::Codex, &provider.id).unwrap();
            sync_company_providers(&state, &login).unwrap();
            let refreshed = state
                .db
                .get_provider_by_id(&provider.id, "codex")
                .unwrap()
                .unwrap();
            let live =
                std::fs::read_to_string(crate::codex_config::get_codex_config_path()).unwrap();
            for config in [
                refreshed.settings_config["config"].as_str().unwrap(),
                live.as_str(),
            ] {
                let doc = config.parse::<DocumentMut>().unwrap();
                assert!(doc.get("disable_response_storage").is_none());
                assert_eq!(
                    doc["model_providers"]["custom"]
                        .as_table_like()
                        .unwrap()
                        .get("supports_websockets")
                        .and_then(|v| v.as_bool()),
                    if websocket { None } else { Some(false) }
                );
            }
        }
    }

    #[test]
    fn company_v2_verified_catalog_filters_unknown_models_and_keeps_identity() {
        let login = sample_login();
        let mut entry = sample_entry();
        entry.models = VERIFIED_COMPACTION_MODELS
            .iter()
            .map(|m| (*m).into())
            .collect();
        entry
            .models
            .extend(["gpt-future".into(), "deepseek-v4-pro".into()]);
        let derived = company_compaction_entry(&entry, &login.base_url).unwrap();
        assert_eq!(derived.models.len(), 9);
        assert_eq!(derived.default_model, VERIFIED_COMPACTION_MODEL);
        assert_eq!(derived.label, format!("{} · 远程压缩 V2", entry.label));
        let (_, expanded) = company_catalog_provider(&derived, &login, 1).unwrap();
        entry.models = vec![VERIFIED_COMPACTION_MODEL.into()];
        let old = company_compaction_entry(&entry, &login.base_url).unwrap();
        let (_, old) = company_catalog_provider(&old, &login, 1).unwrap();
        assert_eq!(expanded.id, old.id);
        assert_eq!(expanded.name, old.name);
        for model in VERIFIED_COMPACTION_MODELS {
            entry.models = vec![(*model).into()];
            entry.default_model = (*model).into();
            let derived = company_compaction_entry(&entry, &login.base_url).unwrap();
            assert_eq!(derived.default_model, *model);
            assert!(company_codex_settings(&derived, &login)["config"]
                .as_str()
                .unwrap()
                .contains("remote_compaction = \"v2\""));
        }
        entry.models = vec!["gpt-6-luna".into(), "gpt-6-sol".into()];
        entry.default_model = "gpt-6-sol".into();
        entry.model_capabilities = Some(std::collections::BTreeMap::from([
            ("gpt-6-luna".into(), json!({"context_window": 1000})),
            ("gpt-future".into(), json!({"context_window": 2000})),
        ]));
        let derived = company_compaction_entry(&entry, &login.base_url).unwrap();
        assert_eq!(derived.default_model, "gpt-6-sol");
        assert_eq!(derived.model_capabilities.as_ref().unwrap().len(), 1);
        entry.default_model = "gpt-future".into();
        assert_eq!(
            company_compaction_entry(&entry, &login.base_url)
                .unwrap()
                .default_model,
            "gpt-6-luna"
        );
        entry.models = vec!["gpt-future".into()];
        assert!(company_compaction_entry(&entry, &login.base_url).is_none());
        entry.models = vec![VERIFIED_COMPACTION_MODEL.into()];
        assert!(company_compaction_entry(&entry, "https://other.example/v1").is_none());
        entry.protocol = "openai-chat".into();
        assert!(company_compaction_entry(&entry, &login.base_url).is_none());
    }

    #[test]
    #[serial_test::serial]
    fn company_v2_all_verified_models_normalize_and_unknown_models_revoke() {
        let _home = IsolatedHome::new();
        let state = test_state();
        let login = sample_login();
        let mut entry = sample_entry();
        entry.models = VERIFIED_COMPACTION_MODELS
            .iter()
            .map(|m| (*m).into())
            .collect();
        for model in VERIFIED_COMPACTION_MODELS {
            entry.default_model = (*model).into();
            let (_, provider) = company_catalog_provider(&entry, &login, 1).unwrap();
            assert!(provider.settings_config["config"]
                .as_str()
                .unwrap()
                .contains("remote_compaction = \"v2\""));
            state.db.save_provider("codex", &provider).unwrap();
            normalize_company_codex_providers(&state).unwrap();
            let refreshed = state
                .db
                .get_provider_by_id(&provider.id, "codex")
                .unwrap()
                .unwrap();
            assert!(refreshed.settings_config["config"]
                .as_str()
                .unwrap()
                .contains("remote_compaction = \"v2\""));
            assert_eq!(normalize_company_codex_providers(&state).unwrap(), 0);
        }
        for invalid in ["unknown_row", "unknown_selected", "empty", "malformed_row"] {
            entry.default_model = VERIFIED_COMPACTION_MODEL.into();
            let (_, mut provider) = company_catalog_provider(&entry, &login, 1).unwrap();
            match invalid {
                "unknown_row" => provider.settings_config["modelCatalog"]["models"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"model": "gpt-future"})),
                "unknown_selected" => {
                    let text = provider.settings_config["config"]
                        .as_str()
                        .unwrap()
                        .replace("model = \"gpt-5.6-sol\"", "model = \"gpt-future\"");
                    provider.settings_config["config"] = json!(text);
                }
                "empty" => provider.settings_config["modelCatalog"]["models"] = json!([]),
                _ => provider.settings_config["modelCatalog"]["models"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"displayName": "unknown"})),
            }
            state.db.save_provider("codex", &provider).unwrap();
            normalize_company_codex_providers(&state).unwrap();
            let refreshed = state
                .db
                .get_provider_by_id(&provider.id, "codex")
                .unwrap()
                .unwrap();
            assert!(
                refreshed.settings_config["config"]
                    .as_str()
                    .unwrap()
                    .contains("remote_compaction = \"unsupported\""),
                "{invalid}"
            );
        }
        entry.models.push("gpt-future".into());
        assert!(!company_codex_settings(&entry, &login)["config"]
            .as_str()
            .unwrap()
            .contains("remote_compaction"));
    }

    #[test]
    fn company_v2_generation_is_limited_to_verified_route_and_model() {
        let mut entry = sample_entry();
        entry.default_model = VERIFIED_COMPACTION_MODEL.into();
        entry.models = vec![VERIFIED_COMPACTION_MODEL.into()];
        let login = sample_login();
        let settings = company_codex_settings(&entry, &login);
        let text = settings["config"].as_str().unwrap();
        let doc = text.parse::<DocumentMut>().unwrap();
        assert_eq!(
            doc["model_providers"]["custom"]["name"].as_str(),
            Some(entry.label.as_str())
        );
        assert_eq!(
            doc["model_providers"]["custom"]["capabilities"]["remote_compaction"].as_str(),
            Some("v2")
        );
        assert!(!text.contains("disable_response_storage"));
        assert!(!text.contains("supports_websockets"));
        if let Some(path) = std::env::var_os("CC_SWITCH_COMPACTION_FIXTURE_OUTPUT") {
            // Synthetic config only, never export real provider credentials.
            std::fs::write(path, text).unwrap();
        }
        for base in [
            "https://other.example/v1",
            "http://leharrt.com",
            "https://leharrt.com/other",
            "https://leharrt.com:444/v1",
        ] {
            let other = LoginResponse {
                base_url: base.into(),
                ..login.clone()
            };
            assert!(!company_codex_settings(&entry, &other)["config"]
                .as_str()
                .unwrap()
                .contains("remote_compaction"));
        }
        entry.models.push("deepseek-v4-pro".into());
        assert!(!company_codex_settings(&entry, &login)["config"]
            .as_str()
            .unwrap()
            .contains("remote_compaction"));
        entry.models = vec![VERIFIED_COMPACTION_MODEL.into()];
        entry.protocol = "openai-chat".into();
        assert!(!company_codex_settings(&entry, &login)["config"]
            .as_str()
            .unwrap()
            .contains("remote_compaction"));
    }

    #[test]
    #[serial_test::serial]
    fn company_v2_refresh_preserves_mixed_card_selection_and_projects_dedicated_card() {
        let _home = IsolatedHome::new();
        let state = test_state();
        let mut login = sample_login();
        login.catalog[0]
            .models
            .extend(VERIFIED_COMPACTION_MODELS.iter().map(|m| (*m).to_string()));
        login.catalog[0].models.push("deepseek-v4-pro".into());
        let (_, mixed) = company_catalog_provider(&login.catalog[0], &login, 1).unwrap();
        state.db.save_provider("codex", &mixed).unwrap();
        ProviderService::switch(&state, AppType::Codex, &mixed.id).unwrap();
        let result = sync_company_providers(&state, &login).unwrap();
        assert_eq!(result.synced, 2);
        assert_eq!(
            ProviderService::current(&state, AppType::Codex).unwrap(),
            mixed.id
        );
        let derived_entry = company_compaction_entry(&login.catalog[0], &login.base_url).unwrap();
        let (_, derived) = company_catalog_provider(&derived_entry, &login, 1).unwrap();
        let original = state
            .db
            .get_provider_by_id(&mixed.id, "codex")
            .unwrap()
            .unwrap();
        assert!(!original.settings_config["config"]
            .as_str()
            .unwrap()
            .contains("remote_compaction"));
        ProviderService::switch(&state, AppType::Codex, &derived.id).unwrap();
        login.api_key = format!("fixture-token-{number}", number = 9);
        sync_company_providers(&state, &login).unwrap();
        assert_eq!(
            ProviderService::current(&state, AppType::Codex).unwrap(),
            derived.id
        );
        let refreshed = state
            .db
            .get_provider_by_id(&derived.id, "codex")
            .unwrap()
            .unwrap();
        assert_eq!(refreshed.name, derived.name);
        assert_eq!(
            refreshed.settings_config["auth"]["OPENAI_API_KEY"],
            format!("fixture-token-{number}", number = 9)
        );
        assert_eq!(
            refreshed.settings_config["modelCatalog"]["models"]
                .as_array()
                .unwrap()
                .len(),
            9
        );
        assert_eq!(normalize_company_codex_providers(&state).unwrap(), 0);
        let live = std::fs::read_to_string(crate::codex_config::get_codex_config_path()).unwrap();
        let doc = live.parse::<DocumentMut>().unwrap();
        let id = doc["model_provider"].as_str().unwrap();
        assert_eq!(
            doc["model_providers"][id]["capabilities"]["remote_compaction"].as_str(),
            Some("v2")
        );
        assert!(!live.contains("disable_response_storage"));
        assert!(!live.contains("supports_websockets = true"));
        assert_eq!(state.db.get_all_providers("codex").unwrap().len(), 2);
    }

    #[test]
    #[serial_test::serial]
    fn company_v2_normalization_handles_actual_selector_and_inline_capabilities() {
        let _home = IsolatedHome::new();
        let state = test_state();
        let mut entry = sample_entry();
        entry.models = vec![VERIFIED_COMPACTION_MODEL.into()];
        entry.default_model = VERIFIED_COMPACTION_MODEL.into();
        let (_, mut provider) = company_catalog_provider(&entry, &sample_login(), 1).unwrap();
        provider.settings_config["config"] = json!("model_provider = \"company_alias\"\nmodel = \"gpt-5.6-sol\"\ndisable_response_storage = false\nfeatures = { remote_compaction_v2 = false, shell_snapshot = false }\nmodel_providers = { company_alias = { name = \"Company\", base_url = \"https://leharrt.com/v1\", supports_websockets = true, capabilities = { image_generation = false } } }\n");
        state.db.save_provider("codex", &provider).unwrap();
        assert_eq!(normalize_company_codex_providers(&state).unwrap(), 1);
        assert_eq!(normalize_company_codex_providers(&state).unwrap(), 0);
        let refreshed = state
            .db
            .get_provider_by_id(&provider.id, "codex")
            .unwrap()
            .unwrap();
        let doc = refreshed.settings_config["config"]
            .as_str()
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(
            doc["model_providers"]["company_alias"]["capabilities"]["remote_compaction"].as_str(),
            Some("v2")
        );
        assert_eq!(
            doc["model_providers"]["company_alias"]["capabilities"]["image_generation"].as_bool(),
            Some(false)
        );
        assert_eq!(doc["features"]["shell_snapshot"].as_bool(), Some(false));
        assert!(doc["features"].get("remote_compaction_v2").is_none());
        assert!(doc.get("disable_response_storage").is_none());
    }

    #[test]
    #[serial_test::serial]
    fn company_catalog_refresh_preserves_unrelated_capabilities_and_features() {
        let _home = IsolatedHome::new();
        let state = test_state();
        let login = sample_login();
        let (_, mut provider) = company_catalog_provider(&sample_entry(), &login, 1).unwrap();
        let mut doc = provider.settings_config["config"]
            .as_str()
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        doc["model_context_window"] = toml_edit::value(300000);
        doc["model_auto_compact_token_limit"] = toml_edit::value(270000);
        doc["features"]["remote_compaction_v2"] = toml_edit::value(false);
        doc["features"]["shell_snapshot"] = toml_edit::value(false);
        doc["model_providers"]["custom"]["capabilities"]["image_generation"] =
            toml_edit::value(false);
        provider.settings_config["config"] = Value::String(doc.to_string());
        state.db.save_provider("codex", &provider).unwrap();
        sync_company_providers(&state, &login).unwrap();
        let refreshed = state
            .db
            .get_provider_by_id(&provider.id, "codex")
            .unwrap()
            .unwrap();
        let doc = refreshed.settings_config["config"]
            .as_str()
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(doc["model_context_window"].as_integer(), Some(300000));
        assert_eq!(
            doc["model_auto_compact_token_limit"].as_integer(),
            Some(270000)
        );
        assert!(doc["features"].get("remote_compaction_v2").is_none());
        assert_eq!(doc["features"]["shell_snapshot"].as_bool(), Some(false));
        assert_eq!(
            doc["model_providers"]["custom"]["capabilities"]["image_generation"].as_bool(),
            Some(false)
        );
        assert!(doc["model_providers"]["custom"]["capabilities"]
            .get("remote_compaction")
            .is_none());
    }

    #[test]
    #[serial_test::serial]
    fn company_v2_refresh_revokes_capability_after_route_changes() {
        let _home = IsolatedHome::new();
        let state = test_state();
        let mut login = sample_login();
        login.catalog[0].models = vec![VERIFIED_COMPACTION_MODEL.into()];
        login.catalog[0].default_model = VERIFIED_COMPACTION_MODEL.into();
        let (_, provider) = company_catalog_provider(&login.catalog[0], &login, 1).unwrap();
        for changed_base in [false, true] {
            state.db.save_provider("codex", &provider).unwrap();
            let mut changed = login.clone();
            if changed_base {
                changed.base_url = "https://other.example/v1".into();
            } else {
                changed.catalog[0].models.push("deepseek-v4-pro".into());
            }
            sync_company_providers(&state, &changed).unwrap();
            let refreshed = state
                .db
                .get_provider_by_id(&provider.id, "codex")
                .unwrap()
                .unwrap();
            let doc = refreshed.settings_config["config"]
                .as_str()
                .unwrap()
                .parse::<DocumentMut>()
                .unwrap();
            assert_eq!(
                doc["model_providers"]["custom"]["capabilities"]["remote_compaction"].as_str(),
                Some("unsupported")
            );
        }
    }

    #[test]
    #[serial_test::serial]
    fn company_v2_existing_derived_card_stays_fresh_when_catalog_shrinks() {
        let _home = IsolatedHome::new();
        let state = test_state();
        let mut login = sample_login();
        login.catalog[0]
            .models
            .push(VERIFIED_COMPACTION_MODEL.into());
        sync_company_providers(&state, &login).unwrap();
        let entry = company_compaction_entry(&login.catalog[0], &login.base_url).unwrap();
        let (_, derived) = company_catalog_provider(&entry, &login, 1).unwrap();
        ProviderService::switch(&state, AppType::Codex, &derived.id).unwrap();
        login.catalog[0].models = vec![VERIFIED_COMPACTION_MODEL.into()];
        login.catalog[0].default_model = VERIFIED_COMPACTION_MODEL.into();
        login.api_key = "rotated".into();
        sync_company_providers(&state, &login).unwrap();
        let refreshed = state
            .db
            .get_provider_by_id(&derived.id, "codex")
            .unwrap()
            .unwrap();
        assert_eq!(
            refreshed.settings_config["auth"]["OPENAI_API_KEY"],
            "rotated"
        );
        assert!(!refreshed
            .notes
            .unwrap_or_default()
            .contains(STALE_COMPANY_NOTE));
        assert_eq!(
            ProviderService::current(&state, AppType::Codex).unwrap(),
            derived.id
        );
    }

    #[test]
    #[serial_test::serial]
    fn company_v2_does_not_trust_an_external_model_catalog() {
        let _home = IsolatedHome::new();
        let state = test_state();
        let mut entry = sample_entry();
        entry.models = vec![VERIFIED_COMPACTION_MODEL.into()];
        entry.default_model = VERIFIED_COMPACTION_MODEL.into();
        let (_, mut provider) = company_catalog_provider(&entry, &sample_login(), 1).unwrap();
        let mut doc = provider.settings_config["config"]
            .as_str()
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        doc["model_catalog_json"] = toml_edit::value("/tmp/unverified-model-catalog.json");
        provider.settings_config["config"] = Value::String(doc.to_string());
        state.db.save_provider("codex", &provider).unwrap();
        normalize_company_codex_providers(&state).unwrap();
        let refreshed = state
            .db
            .get_provider_by_id(&provider.id, "codex")
            .unwrap()
            .unwrap();
        let doc = refreshed.settings_config["config"]
            .as_str()
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(
            doc["model_providers"]["custom"]["capabilities"]["remote_compaction"].as_str(),
            Some("unsupported")
        );
        assert_eq!(
            doc["model_catalog_json"].as_str(),
            Some("/tmp/unverified-model-catalog.json")
        );
    }

    #[test]
    fn company_refresh_cleans_inline_provider_transport_settings() {
        for websocket in ["true", "false"] {
            let input = format!("model_provider = \"company\"\ndisable_response_storage = false\nmodel_providers = {{ company = {{ base_url = \"https://leharrt.com\", supports_websockets = {websocket} }} }}\n");
            let normalized = normalize_company_codex_config_text(&input);
            let doc = normalized.parse::<DocumentMut>().unwrap();
            let table = doc["model_providers"]["company"].as_table_like().unwrap();
            assert!(doc.get("disable_response_storage").is_none());
            assert_eq!(
                table.get("supports_websockets").and_then(|v| v.as_bool()),
                if websocket == "true" {
                    None
                } else {
                    Some(false)
                }
            );
            assert_eq!(
                table.get("base_url").and_then(|v| v.as_str()),
                Some("https://leharrt.com/v1")
            );
        }
    }

    #[test]
    fn company_refresh_removes_both_storage_values_and_preserves_disabled_websocket() {
        for storage in ["true", "false"] {
            let input = format!(
                "model_provider = \"company\"\ndisable_response_storage = {storage}\n\
                 [model_providers.company]\n\
                 base_url = \"https://leharrt.com/v1\"\n\
                 supports_websockets = false\n\
                 [mcp_servers.example]\ncommand = \"example\"\n"
            );
            let normalized = normalize_company_codex_config_text(&input);
            let doc = normalized.parse::<DocumentMut>().unwrap();
            assert!(doc.get("disable_response_storage").is_none());
            assert_eq!(
                doc["model_providers"]["company"]["supports_websockets"].as_bool(),
                Some(false)
            );
            assert_eq!(
                doc["mcp_servers"]["example"]["command"].as_str(),
                Some("example")
            );
            assert_eq!(normalize_company_codex_config_text(&normalized), normalized);
        }
    }

    #[test]
    fn company_refresh_cleans_legacy_flat_transport_settings() {
        let normalized = normalize_company_codex_config_text(
            "disable_response_storage = false\nsupports_websockets = true\nbase_url = \"https://leharrt.com\"\n",
        );
        let doc = normalized.parse::<DocumentMut>().unwrap();
        assert!(doc.get("disable_response_storage").is_none());
        assert!(doc.get("supports_websockets").is_none());
        assert_eq!(doc["wire_api"].as_str(), Some("responses"));
    }

    #[test]
    fn company_refresh_cleans_malformed_config_without_forcing_storage() {
        for storage in ["true", "false"] {
            for websocket in ["true", "false"] {
                let input = format!(
                    "disable_response_storage = {storage} # legacy\n\
                     [model_providers.custom]\n\
                     base_url = \"https://leharrt.com\"\n\
                     supports_websockets = {websocket} # legacy\n\
                     this is not valid toml\n"
                );
                let normalized = normalize_company_codex_config_text(&input);
                assert!(!normalized.contains("disable_response_storage"));
                assert_eq!(
                    normalized.contains("supports_websockets"),
                    websocket == "false"
                );
                assert!(normalized.contains("this is not valid toml"));
            }
        }
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
