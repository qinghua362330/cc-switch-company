mod keychain;
mod metadata;
mod secrets;

use super::models::{AuthError, AuthMetadata, AuthSecrets, AuthState, CatalogEntry, LoginResponse};

pub use metadata::FileAuthMetadataStore;
pub use secrets::{FileSecretStore, InMemorySecretStore, MacKeychainSecretStore};

pub trait AuthMetadataStore: Clone + Send + Sync + 'static {
    fn load_metadata(&self) -> Result<Option<AuthMetadata>, AuthError>;
    fn save_metadata(&self, metadata: &AuthMetadata) -> Result<(), AuthError>;
    fn clear_metadata(&self) -> Result<(), AuthError>;
}

pub trait SecretStore: Clone + Send + Sync + 'static {
    fn load_secrets(&self) -> Result<Option<AuthSecrets>, AuthError>;
    fn save_secrets(&self, secrets: &AuthSecrets) -> Result<(), AuthError>;
    fn clear_secrets(&self) -> Result<(), AuthError>;
}

#[derive(Clone)]
pub struct AuthPersistence<M, S>
where
    M: AuthMetadataStore,
    S: SecretStore,
{
    metadata_store: M,
    secret_store: S,
}

impl<M, S> AuthPersistence<M, S>
where
    M: AuthMetadataStore,
    S: SecretStore,
{
    pub fn new(metadata_store: M, secret_store: S) -> Self {
        Self {
            metadata_store,
            secret_store,
        }
    }

    pub(crate) fn load_state(&self) -> Result<AuthState, AuthError> {
        let metadata = self.metadata_store.load_metadata()?;
        let secrets = self.secret_store.load_secrets()?;
        match (metadata, secrets) {
            (Some(metadata), Some(_)) => Ok(AuthState::from_metadata(metadata)),
            _ => Ok(AuthState::unauthenticated()),
        }
    }

    pub(crate) fn load_metadata(&self) -> Result<Option<AuthMetadata>, AuthError> {
        self.metadata_store.load_metadata()
    }

    pub(crate) fn load_secrets(&self) -> Result<AuthSecrets, AuthError> {
        self.secret_store
            .load_secrets()?
            .ok_or_else(AuthError::not_authenticated)
    }

    pub(crate) fn save_login(&self, response: &LoginResponse) -> Result<(), AuthError> {
        self.secret_store
            .save_secrets(&AuthSecrets::from_login(response))?;
        if let Err(save_error) = self
            .metadata_store
            .save_metadata(&AuthMetadata::from_login(response))
        {
            if let Err(clear_error) = self.secret_store.clear_secrets() {
                return Err(AuthError::new(
                    "storage",
                    format!("{save_error}; 密钥已写入但回滚清理失败: {clear_error}"),
                ));
            }
            return Err(save_error);
        }
        Ok(())
    }

    pub(crate) fn update_catalog(
        &self,
        base_url: String,
        catalog: Vec<CatalogEntry>,
    ) -> Result<AuthState, AuthError> {
        let mut metadata = self
            .metadata_store
            .load_metadata()?
            .ok_or_else(AuthError::not_authenticated)?;
        metadata.base_url = base_url;
        metadata.catalog = catalog;
        self.metadata_store.save_metadata(&metadata)?;
        Ok(AuthState::from_metadata(metadata))
    }

    pub(crate) fn clear(&self) -> Result<(), AuthError> {
        self.secret_store.clear_secrets()?;
        self.metadata_store.clear_metadata()?;
        Ok(())
    }
}
