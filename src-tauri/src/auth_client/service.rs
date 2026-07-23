use std::path::Path;

use super::client::{
    legacy_default_catalog, ProvisionClient, ProvisionConfig, ReqwestProvisionClient,
};
use super::models::{AuthError, AuthState, CatalogEntry, FeishuLoginStart, LoginResponse};
use super::persistence::{
    AuthMetadataStore, AuthPersistence, FileAuthMetadataStore, FileSecretStore,
    MacKeychainSecretStore, SecretStore,
};

#[derive(Clone)]
pub struct AuthService<C, M, S>
where
    C: ProvisionClient,
    M: AuthMetadataStore,
    S: SecretStore,
{
    client: C,
    persistence: AuthPersistence<M, S>,
}

impl<C, M, S> AuthService<C, M, S>
where
    C: ProvisionClient,
    M: AuthMetadataStore,
    S: SecretStore,
{
    pub fn new(client: C, persistence: AuthPersistence<M, S>) -> Self {
        Self {
            client,
            persistence,
        }
    }

    pub async fn get_state(&self) -> Result<AuthState, AuthError> {
        let mut state = self.persistence.load_state()?;
        if state.authenticated {
            if let Ok(secrets) = self.persistence.load_secrets() {
                state.catalog = complete_company_catalog(
                    state.catalog,
                    secrets.session_token.starts_with("legacy-"),
                );
            }
        }
        Ok(state)
    }

    pub async fn current_login(&self) -> Result<LoginResponse, AuthError> {
        let metadata = self
            .persistence
            .load_metadata()?
            .ok_or_else(AuthError::not_authenticated)?;
        let secrets = self.persistence.load_secrets()?;

        let catalog = complete_company_catalog(
            metadata.catalog,
            secrets.session_token.starts_with("legacy-"),
        );

        Ok(LoginResponse {
            session_token: secrets.session_token,
            api_key: secrets.api_key,
            base_url: metadata.base_url,
            user: metadata.user,
            catalog,
        })
    }

    pub async fn login_with_ticket(&self, ticket: &str) -> Result<AuthState, AuthError> {
        let ticket = ticket.trim();
        if !ticket.starts_with("fs_") || ticket.len() <= 3 {
            return Err(AuthError::invalid_ticket());
        }

        let mut response = self.client.login(ticket).await?;
        response.validate()?;
        response.catalog = complete_company_catalog(
            response.catalog,
            response.session_token.starts_with("legacy-"),
        );
        response.validate()?;
        self.persistence.save_login(&response)?;
        self.persistence.load_state()
    }

    pub async fn refresh_catalog(&self) -> Result<AuthState, AuthError> {
        let secrets = self.persistence.load_secrets()?;
        match self.client.catalog(&secrets.session_token).await {
            Ok((base_url, catalog)) => {
                let catalog =
                    complete_company_catalog(catalog, secrets.session_token.starts_with("legacy-"));
                self.persistence.update_catalog(base_url, catalog)
            }
            Err(err) if err.kind() == "unauthorized" => {
                self.persistence.clear()?;
                Err(err)
            }
            Err(err) => Err(err),
        }
    }

    pub async fn logout(&self) -> Result<AuthState, AuthError> {
        self.persistence.clear()?;
        Ok(AuthState::unauthenticated())
    }
}

fn expand_legacy_catalog(catalog: Vec<CatalogEntry>) -> Vec<CatalogEntry> {
    let mut expanded = catalog;
    for default_entry in legacy_default_catalog() {
        let already_present = expanded.iter().any(|entry| {
            entry.tool.eq_ignore_ascii_case(&default_entry.tool)
                && entry.label.eq_ignore_ascii_case(&default_entry.label)
        });
        if !already_present {
            expanded.push(default_entry);
        }
    }
    expanded
}

fn complete_company_catalog(
    catalog: Vec<CatalogEntry>,
    include_legacy_defaults: bool,
) -> Vec<CatalogEntry> {
    let mut expanded = if include_legacy_defaults {
        expand_legacy_catalog(catalog)
    } else {
        catalog
    };
    add_gemini_entry_from_model_catalog(&mut expanded);
    expanded
}

fn add_gemini_entry_from_model_catalog(catalog: &mut Vec<CatalogEntry>) {
    if catalog.iter().any(|entry| matches_gemini_tool(&entry.tool)) {
        return;
    }

    let mut models = Vec::new();
    let mut group = "default".to_string();
    for entry in catalog.iter() {
        for model in &entry.models {
            if is_gemini_model(model) && !models.iter().any(|seen| seen == model) {
                if models.is_empty() {
                    group = entry.group.clone();
                }
                models.push(model.clone());
            }
        }
    }

    let Some(default_model) = models.first().cloned() else {
        return;
    };

    catalog.push(CatalogEntry {
        tool: "gemini".to_string(),
        label: "公司号池 Gemini".to_string(),
        protocol: "gemini".to_string(),
        default_model,
        models,
        group,
        // 这条是客户端本地补出来的，服务端没有对应的能力声明。
        model_capabilities: None,
    });
}

fn matches_gemini_tool(tool: &str) -> bool {
    matches!(
        tool.trim().to_ascii_lowercase().as_str(),
        "gemini" | "gemini-cli" | "gemini_cli"
    )
}

fn is_gemini_model(model: &str) -> bool {
    model.to_ascii_lowercase().contains("gemini")
}

#[derive(Clone)]
pub enum ProductionSecretStore {
    File(FileSecretStore),
    MacKeychain(MacKeychainSecretStore),
}

impl SecretStore for ProductionSecretStore {
    fn load_secrets(&self) -> Result<Option<super::models::AuthSecrets>, AuthError> {
        match self {
            Self::File(store) => store.load_secrets(),
            Self::MacKeychain(store) => store.load_secrets(),
        }
    }

    fn save_secrets(&self, secrets: &super::models::AuthSecrets) -> Result<(), AuthError> {
        match self {
            Self::File(store) => store.save_secrets(secrets),
            Self::MacKeychain(store) => store.save_secrets(secrets),
        }
    }

    fn clear_secrets(&self) -> Result<(), AuthError> {
        match self {
            Self::File(store) => store.clear_secrets(),
            Self::MacKeychain(store) => store.clear_secrets(),
        }
    }
}

pub type ProductionAuthService =
    AuthService<ReqwestProvisionClient, FileAuthMetadataStore, ProductionSecretStore>;

fn production_secret_store(app_config_dir: &Path) -> ProductionSecretStore {
    if std::env::var("CC_SWITCH_COMPANY_AUTH_SECRET_STORE")
        .is_ok_and(|value| value.eq_ignore_ascii_case("keychain"))
    {
        return ProductionSecretStore::MacKeychain(MacKeychainSecretStore);
    }

    ProductionSecretStore::File(FileSecretStore::new(app_config_dir.to_path_buf()))
}

pub fn production_auth_service(app_config_dir: &Path) -> Result<ProductionAuthService, AuthError> {
    let config = ProvisionConfig::from_env();
    Ok(AuthService::new(
        ReqwestProvisionClient::try_new(config)?,
        AuthPersistence::new(
            FileAuthMetadataStore::new(app_config_dir.to_path_buf()),
            production_secret_store(app_config_dir),
        ),
    ))
}

pub fn feishu_login_start() -> FeishuLoginStart {
    FeishuLoginStart {
        url: ProvisionConfig::from_env().feishu_start_url(),
    }
}
