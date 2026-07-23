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
        .read()
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
        .read()
        .await
        .refresh_catalog()
        .await
        .map_err(|err| err.to_command_error())
}

#[tauri::command]
pub async fn company_auth_sync_providers(
    auth_state: State<'_, CompanyAuthState>,
    app_state: State<'_, AppState>,
) -> Result<CompanyProviderSyncResult, String> {
    let login = auth_state
        .0
        .read()
        .await
        .current_login()
        .await
        .map_err(|err| err.to_command_error())?;

    sync_company_providers(app_state.inner(), &login).map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn company_auth_logout(state: State<'_, CompanyAuthState>) -> Result<AuthState, String> {
    state
        .0
        .read()
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

    for entry in &login.catalog {
        let Some((app_type, provider)) = company_catalog_provider(entry, login, created_at) else {
            continue;
        };

        upsert_company_provider(state, app_type, provider)?;
        synced += 1;
    }

    Ok(CompanyProviderSyncResult { synced })
}

fn upsert_company_provider(
    state: &AppState,
    app_type: AppType,
    provider: Provider,
) -> Result<(), crate::error::AppError> {
    let exists = state
        .db
        .get_provider_by_id(&provider.id, app_type.as_str())?
        .is_some();

    if exists {
        ProviderService::update(state, app_type, None, provider)?;
    } else {
        ProviderService::add(state, app_type, provider, false)?;
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
         disable_response_storage = true\n\
         \n\
         [model_providers.custom]\n\
         name = {}\n\
         base_url = {}\n\
         wire_api = \"responses\"\n\
         requires_openai_auth = true\n",
        toml_string(&entry.default_model),
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

    if matches!(app_type, AppType::Codex) {
        meta.api_format = Some("openai_responses".to_string());
    } else if entry.protocol.eq_ignore_ascii_case("openai-responses") {
        meta.api_format = Some("openai_responses".to_string());
    } else if entry.protocol.eq_ignore_ascii_case("openai-chat") {
        meta.api_format = Some("openai_chat".to_string());
    }

    meta
}

pub fn normalize_company_codex_providers(
    state: &AppState,
) -> Result<usize, crate::error::AppError> {
    let mut updated = 0;
    let providers = state.db.get_all_providers(AppType::Codex.as_str())?;

    for (_, mut provider) in providers {
        let is_company_provider = provider.id.starts_with("company-codex-")
            || provider
                .meta
                .as_ref()
                .and_then(|meta| meta.provider_type.as_deref())
                == Some("company_auth");

        if !is_company_provider {
            continue;
        }

        let mut changed = false;

        if let Some(meta) = provider.meta.as_mut() {
            if meta.api_format.as_deref() != Some("openai_responses") {
                meta.api_format = Some("openai_responses".to_string());
                changed = true;
            }
        }

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
        return config_text
            .replace(r#"wire_api = "chat""#, r#"wire_api = "responses""#)
            .replace(
                r#"requires_openai_auth = false"#,
                r#"requires_openai_auth = true"#,
            );
    };

    let provider_name = doc
        .get("model_provider")
        .and_then(|value| value.as_str())
        .unwrap_or("custom")
        .to_string();

    if let Some(table) = doc
        .get_mut("model_providers")
        .and_then(|item| item.as_table_mut())
        .and_then(|table| table.get_mut(&provider_name))
        .and_then(|item| item.as_table_mut())
    {
        table["wire_api"] = toml_edit::value("responses");
        table["requires_openai_auth"] = toml_edit::value(true);
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

fn codex_responses_base_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1")
    }
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
