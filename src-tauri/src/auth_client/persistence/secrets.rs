use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use super::{keychain, SecretStore};
use crate::auth_client::models::{AuthError, AuthSecrets};

const SECRETS_FILE_NAME: &str = "company_auth_secrets.json";

#[derive(Clone, Default)]
pub struct InMemorySecretStore {
    secrets: Arc<Mutex<Option<AuthSecrets>>>,
}

impl InMemorySecretStore {
    fn secrets_guard(&self) -> Result<MutexGuard<'_, Option<AuthSecrets>>, AuthError> {
        self.secrets
            .lock()
            .map_err(|_| AuthError::new("storage", "认证密钥存储锁已损坏"))
    }
}

impl SecretStore for InMemorySecretStore {
    fn load_secrets(&self) -> Result<Option<AuthSecrets>, AuthError> {
        Ok(self.secrets_guard()?.clone())
    }

    fn save_secrets(&self, secrets: &AuthSecrets) -> Result<(), AuthError> {
        *self.secrets_guard()? = Some(secrets.clone());
        Ok(())
    }

    fn clear_secrets(&self) -> Result<(), AuthError> {
        *self.secrets_guard()? = None;
        Ok(())
    }
}

#[derive(Clone)]
pub struct FileSecretStore {
    base_dir: PathBuf,
}

impl FileSecretStore {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    fn secrets_path(&self) -> PathBuf {
        self.base_dir.join(SECRETS_FILE_NAME)
    }
}

impl SecretStore for FileSecretStore {
    fn load_secrets(&self) -> Result<Option<AuthSecrets>, AuthError> {
        let path = self.secrets_path();
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|_| AuthError::new("storage", "读取公司认证密钥失败"))?;
        let secrets = serde_json::from_str::<AuthSecrets>(&raw)
            .map_err(|_| AuthError::new("storage", "公司认证密钥格式不正确"))?;
        Ok(Some(secrets))
    }

    fn save_secrets(&self, secrets: &AuthSecrets) -> Result<(), AuthError> {
        std::fs::create_dir_all(&self.base_dir)
            .map_err(|_| AuthError::new("storage", "创建公司认证密钥目录失败"))?;
        let path = self.secrets_path();
        let tmp_path = path.with_extension("json.tmp");
        let raw = serde_json::to_vec_pretty(secrets)
            .map_err(|_| AuthError::new("storage", "序列化公司认证密钥失败"))?;
        std::fs::write(&tmp_path, raw)
            .map_err(|_| AuthError::new("storage", "写入公司认证密钥失败"))?;
        set_owner_only_permissions(&tmp_path)?;
        std::fs::rename(&tmp_path, &path)
            .map_err(|_| AuthError::new("storage", "保存公司认证密钥失败"))?;
        set_owner_only_permissions(&path)?;
        Ok(())
    }

    fn clear_secrets(&self) -> Result<(), AuthError> {
        let path = self.secrets_path();
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(AuthError::new("storage", "清除公司认证密钥失败")),
        }
    }
}

#[cfg(unix)]
fn set_owner_only_permissions(path: &std::path::Path) -> Result<(), AuthError> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|_| AuthError::new("storage", "设置公司认证密钥权限失败"))
}

#[cfg(not(unix))]
fn set_owner_only_permissions(_path: &std::path::Path) -> Result<(), AuthError> {
    Ok(())
}

#[derive(Clone, Default)]
pub struct MacKeychainSecretStore;

impl SecretStore for MacKeychainSecretStore {
    fn load_secrets(&self) -> Result<Option<AuthSecrets>, AuthError> {
        keychain::load_keychain_secrets()
    }

    fn save_secrets(&self, secrets: &AuthSecrets) -> Result<(), AuthError> {
        keychain::save_keychain_secrets(secrets)
    }

    fn clear_secrets(&self) -> Result<(), AuthError> {
        keychain::clear_keychain_secrets()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_secrets() -> AuthSecrets {
        AuthSecrets {
            session_token: "test_session_token_1234567890".to_string(),
            api_key: ["fixture", "api", "key"].join("-"),
        }
    }

    #[test]
    fn file_secret_store_round_trips_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path().to_path_buf());

        assert_eq!(store.load_secrets().unwrap(), None);

        store.save_secrets(&sample_secrets()).unwrap();
        assert_eq!(store.load_secrets().unwrap(), Some(sample_secrets()));

        store.clear_secrets().unwrap();
        assert_eq!(store.load_secrets().unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn file_secret_store_uses_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path().to_path_buf());

        store.save_secrets(&sample_secrets()).unwrap();

        let mode = std::fs::metadata(store.secrets_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
