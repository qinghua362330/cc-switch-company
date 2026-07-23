use std::time::Duration;

use futures::future::BoxFuture;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::models::{AuthError, CatalogEntry, CatalogResponse, LoginResponse, UserIdentity};

pub const DEFAULT_PROVISION_BASE_URL: &str = "https://leharrt.com";
const PROVISION_HTTP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct ProvisionConfig {
    pub base_url: String,
}

impl Default for ProvisionConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_PROVISION_BASE_URL.to_string(),
        }
    }
}

impl ProvisionConfig {
    pub fn from_env() -> Self {
        let base_url = std::env::var("CC_SWITCH_PROVISION_BASE_URL")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_PROVISION_BASE_URL.to_string());
        Self { base_url }
    }

    pub fn endpoint(&self, path: &str) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), path)
    }

    pub fn feishu_start_url(&self) -> String {
        self.endpoint("/api/provision/feishu/start")
    }
}

pub trait ProvisionClient: Clone + Send + Sync + 'static {
    fn login<'a>(&'a self, ticket: &'a str) -> BoxFuture<'a, Result<LoginResponse, AuthError>>;

    fn catalog<'a>(
        &'a self,
        session_token: &'a str,
    ) -> BoxFuture<'a, Result<(String, Vec<CatalogEntry>), AuthError>>;
}

#[derive(Clone)]
pub struct ReqwestProvisionClient {
    client: reqwest::Client,
    config: ProvisionConfig,
}

impl ReqwestProvisionClient {
    pub fn try_new(config: ProvisionConfig) -> Result<Self, AuthError> {
        let client = reqwest::Client::builder()
            .timeout(PROVISION_HTTP_TIMEOUT)
            .build()
            .map_err(|_| AuthError::new("network", "初始化认证 HTTP 客户端失败"))?;
        Ok(Self { client, config })
    }
}

impl ProvisionClient for ReqwestProvisionClient {
    fn login<'a>(&'a self, ticket: &'a str) -> BoxFuture<'a, Result<LoginResponse, AuthError>> {
        Box::pin(async move {
            let response = self
                .client
                .post(self.config.endpoint("/api/client/login"))
                .json(&serde_json::json!({ "ticket": ticket }))
                .send()
                .await
                .map_err(|_| AuthError::new("network", "无法连接认证服务"))?;

            let status = response.status();
            if !status.is_success() {
                if status.as_u16() == 404 {
                    return self.login_with_legacy_codex_key(ticket).await;
                }
                return Err(AuthError::from_status(status.as_u16()));
            }

            let body = response
                .json::<LoginResponse>()
                .await
                .map_err(|_| AuthError::malformed_response())?;
            body.validate()?;
            Ok(body)
        })
    }

    fn catalog<'a>(
        &'a self,
        session_token: &'a str,
    ) -> BoxFuture<'a, Result<(String, Vec<CatalogEntry>), AuthError>> {
        Box::pin(async move {
            let response = self
                .client
                .get(self.config.endpoint("/api/client/catalog"))
                .bearer_auth(session_token)
                .send()
                .await
                .map_err(|_| AuthError::new("network", "无法连接认证服务"))?;

            let status = response.status();
            if !status.is_success() {
                if status.as_u16() == 404 && session_token.starts_with("legacy-") {
                    return Ok((self.config.base_url.clone(), legacy_default_catalog()));
                }
                return Err(AuthError::from_status(status.as_u16()));
            }

            let body = response
                .json::<CatalogResponse>()
                .await
                .map_err(|_| AuthError::malformed_response())?;
            body.validate()?;
            Ok((body.base_url, body.catalog))
        })
    }
}

impl ReqwestProvisionClient {
    async fn login_with_legacy_codex_key(&self, ticket: &str) -> Result<LoginResponse, AuthError> {
        let response = self
            .client
            .post(self.config.endpoint("/api/provision/codex-key"))
            .json(&serde_json::json!({ "ticket": ticket }))
            .send()
            .await
            .map_err(|_| AuthError::new("network", "无法连接认证服务"))?;

        let status = response.status();
        if !status.is_success() {
            return Err(AuthError::from_status(status.as_u16()));
        }

        let body = response
            .json::<Value>()
            .await
            .map_err(|_| AuthError::malformed_response())?;
        legacy_login_from_value(ticket, &self.config.base_url, &body)
    }
}

fn legacy_login_from_value(
    ticket: &str,
    default_base_url: &str,
    value: &Value,
) -> Result<LoginResponse, AuthError> {
    let api_key = find_string(value, &["api_key", "apiKey", "key", "raw_key", "rawKey"])
        .ok_or_else(AuthError::malformed_response)?;
    let base_url = find_string(value, &["base_url", "baseUrl"])
        .unwrap_or_else(|| default_base_url.to_string());
    let display_name = find_string(
        value,
        &[
            "display_name",
            "displayName",
            "name",
            "username",
            "newapi_username",
            "email",
        ],
    )
    .unwrap_or_else(|| "公司账号".to_string());
    let email = find_string(value, &["email", "username", "newapi_username"])
        .unwrap_or_else(|| "company-user@local".to_string());
    let session_token = format!("legacy-{}", stable_ticket_hash(ticket, &api_key));

    let response = LoginResponse {
        session_token,
        api_key,
        base_url,
        user: UserIdentity {
            display_name,
            email,
        },
        catalog: legacy_default_catalog(),
    };
    response.validate()?;
    Ok(response)
}

pub(crate) fn legacy_default_catalog() -> Vec<CatalogEntry> {
    vec![
        legacy_catalog_entry("codex", "公司号池 Codex", "openai-responses", "gpt-5.5"),
        legacy_catalog_entry("claude", "公司号池 Claude", "anthropic", "claude-opus-4-8"),
        legacy_catalog_entry("claude", "GLM", "anthropic", "glm-4.6"),
        legacy_catalog_entry("claude", "Grok", "anthropic", "grok-4-1-fast"),
        legacy_catalog_entry("gemini", "公司号池 Gemini", "gemini", "gemini-3.5-flash"),
    ]
}

fn legacy_catalog_entry(
    tool: &str,
    label: &str,
    protocol: &str,
    default_model: &str,
) -> CatalogEntry {
    CatalogEntry {
        tool: tool.to_string(),
        label: label.to_string(),
        protocol: protocol.to_string(),
        default_model: default_model.to_string(),
        models: vec![default_model.to_string()],
        group: "default".to_string(),
    }
}

fn find_string(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(text) = map
                    .get(*key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                {
                    return Some(text.to_string());
                }
            }
            for child in map.values() {
                if let Some(text) = find_string(child, keys) {
                    return Some(text);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(|item| find_string(item, keys)),
        _ => None,
    }
}

fn stable_ticket_hash(ticket: &str, api_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(ticket.as_bytes());
    hasher.update(api_key.as_bytes());
    let digest = hasher.finalize();
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn legacy_login_accepts_nested_codex_key_response() {
        let value = serde_json::json!({
            "codex": {
                "api_key": "sk-legacy-secret-1234567890",
                "base_url": "https://leharrt.com/v1"
            },
            "user": {
                "display_name": "Legacy User",
                "email": "legacy@example.com"
            }
        });

        let login = legacy_login_from_value("fs_legacy_ok", DEFAULT_PROVISION_BASE_URL, &value)
            .expect("legacy login");

        assert_eq!(login.api_key, "sk-legacy-secret-1234567890");
        assert_eq!(login.base_url, "https://leharrt.com/v1");
        assert_eq!(login.user.display_name, "Legacy User");
        assert_eq!(login.user.email, "legacy@example.com");
        assert_eq!(login.catalog[0].tool, "codex");
        assert!(login.session_token.starts_with("legacy-"));
    }

    #[test]
    fn legacy_login_requires_api_key() {
        let err = legacy_login_from_value(
            "fs_legacy_missing",
            DEFAULT_PROVISION_BASE_URL,
            &serde_json::json!({"user":{"email":"legacy@example.com"}}),
        )
        .unwrap_err();

        assert_eq!(err.kind(), "malformed_response");
    }

    #[test]
    fn legacy_default_catalog_matches_temporary_backend_fallback() {
        let catalog = legacy_default_catalog();

        assert_eq!(catalog.len(), 5);
        assert_eq!(catalog[0].label, "公司号池 Codex");
        assert_eq!(catalog[0].default_model, "gpt-5.5");
        assert_eq!(catalog[1].label, "公司号池 Claude");
        assert_eq!(catalog[1].default_model, "claude-opus-4-8");
        assert_eq!(catalog[2].label, "GLM");
        assert_eq!(catalog[2].default_model, "glm-4.6");
        assert_eq!(catalog[3].label, "Grok");
        assert_eq!(catalog[3].default_model, "grok-4-1-fast");
        assert_eq!(catalog[4].label, "公司号池 Gemini");
        assert_eq!(catalog[4].default_model, "gemini-3.5-flash");
    }

    #[tokio::test]
    async fn legacy_catalog_refresh_404_keeps_temporary_catalog() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0_u8; 2048];
            let _ = stream.read(&mut buffer).await.unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        });
        let client = ReqwestProvisionClient::try_new(ProvisionConfig {
            base_url: format!("http://{addr}"),
        })
        .unwrap();

        let (base_url, catalog) = client.catalog("legacy-session").await.unwrap();
        server.await.unwrap();

        assert_eq!(base_url, format!("http://{addr}"));
        assert_eq!(catalog.len(), 5);
        assert!(catalog.iter().any(|entry| entry.label == "公司号池 Codex"));
        assert!(catalog.iter().any(|entry| entry.label == "公司号池 Claude"));
        assert!(catalog.iter().any(|entry| entry.label == "公司号池 Gemini"));
    }
}
