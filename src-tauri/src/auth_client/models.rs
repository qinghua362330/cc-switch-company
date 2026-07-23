use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserIdentity {
    pub display_name: String,
    pub email: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub tool: String,
    pub label: String,
    pub protocol: String,
    pub default_model: String,
    pub models: Vec<String>,
    pub group: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginResponse {
    pub session_token: String,
    pub api_key: String,
    pub base_url: String,
    pub user: UserIdentity,
    pub catalog: Vec<CatalogEntry>,
}

impl LoginResponse {
    pub fn validate(&self) -> Result<(), AuthError> {
        require_non_empty("session_token", &self.session_token)?;
        require_non_empty("api_key", &self.api_key)?;
        require_non_empty("base_url", &self.base_url)?;
        require_non_empty("user.display_name", &self.user.display_name)?;
        require_non_empty("user.email", &self.user.email)?;
        validate_catalog(&self.catalog)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CatalogResponse {
    pub(crate) base_url: String,
    pub(crate) catalog: Vec<CatalogEntry>,
}

impl CatalogResponse {
    pub(crate) fn validate(&self) -> Result<(), AuthError> {
        require_non_empty("base_url", &self.base_url)?;
        validate_catalog(&self.catalog)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthSecrets {
    pub session_token: String,
    pub api_key: String,
}

impl AuthSecrets {
    pub(crate) fn from_login(response: &LoginResponse) -> Self {
        Self {
            session_token: response.session_token.clone(),
            api_key: response.api_key.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthMetadata {
    pub base_url: String,
    pub user: UserIdentity,
    pub catalog: Vec<CatalogEntry>,
    pub api_key_preview: String,
    pub session_token_preview: String,
}

impl AuthMetadata {
    pub(crate) fn from_login(response: &LoginResponse) -> Self {
        Self {
            base_url: response.base_url.clone(),
            user: response.user.clone(),
            catalog: response.catalog.clone(),
            api_key_preview: redact_secret(&response.api_key),
            session_token_preview: redact_secret(&response.session_token),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthState {
    pub authenticated: bool,
    pub base_url: Option<String>,
    pub user: Option<UserIdentity>,
    pub catalog: Vec<CatalogEntry>,
    pub api_key_preview: Option<String>,
    pub session_token_preview: Option<String>,
}

impl AuthState {
    pub(crate) fn unauthenticated() -> Self {
        Self {
            authenticated: false,
            base_url: None,
            user: None,
            catalog: Vec::new(),
            api_key_preview: None,
            session_token_preview: None,
        }
    }

    pub(crate) fn from_metadata(metadata: AuthMetadata) -> Self {
        Self {
            authenticated: true,
            base_url: Some(metadata.base_url),
            user: Some(metadata.user),
            catalog: metadata.catalog,
            api_key_preview: Some(metadata.api_key_preview),
            session_token_preview: Some(metadata.session_token_preview),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeishuLoginStart {
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuthError {
    kind: &'static str,
    message: String,
}

impl AuthError {
    pub fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> &'static str {
        self.kind
    }

    pub fn from_status(status: u16) -> Self {
        match status {
            401 => Self::new("unauthorized", "登录已失效，请重新登录"),
            403 => Self::new("forbidden", "当前账号不在允许的租户范围内"),
            500..=599 => Self::new("server", "认证服务暂时不可用"),
            _ => Self::new("http", format!("认证服务返回 HTTP {status}")),
        }
    }

    pub(crate) fn invalid_ticket() -> Self {
        Self::new("invalid_ticket", "请输入有效的飞书 ticket")
    }

    pub(crate) fn not_authenticated() -> Self {
        Self::new("not_authenticated", "尚未登录")
    }

    pub(crate) fn malformed_response() -> Self {
        Self::new("malformed_response", "认证服务响应格式不正确")
    }

    pub fn to_command_error(&self) -> String {
        format!("{}: {}", self.kind, self.message)
    }
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for AuthError {}

pub fn redact_secret(secret: &str) -> String {
    let trimmed = secret.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= 8 {
        return "***".to_string();
    }
    let prefix: String = chars.iter().take(4).collect();
    let suffix: String = chars
        .iter()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{prefix}...{suffix}")
}

pub fn redact_authorization_header(value: &str) -> String {
    match value.trim().split_once(' ') {
        Some((scheme, token)) if scheme.eq_ignore_ascii_case("bearer") => {
            format!("Bearer {}", redact_secret(token))
        }
        _ => "[redacted]".to_string(),
    }
}

fn validate_catalog(catalog: &[CatalogEntry]) -> Result<(), AuthError> {
    for (index, entry) in catalog.iter().enumerate() {
        require_non_empty(&format!("catalog[{index}].tool"), &entry.tool)?;
        require_non_empty(&format!("catalog[{index}].label"), &entry.label)?;
        require_non_empty(&format!("catalog[{index}].protocol"), &entry.protocol)?;
        require_non_empty(
            &format!("catalog[{index}].default_model"),
            &entry.default_model,
        )?;
        require_non_empty(&format!("catalog[{index}].group"), &entry.group)?;
        if entry.models.is_empty() {
            return Err(AuthError::new(
                "malformed_response",
                format!("catalog[{index}].models 不能为空"),
            ));
        }
    }
    Ok(())
}

fn require_non_empty(field: &str, value: &str) -> Result<(), AuthError> {
    if value.trim().is_empty() {
        return Err(AuthError::new(
            "malformed_response",
            format!("{field} 不能为空"),
        ));
    }
    Ok(())
}
