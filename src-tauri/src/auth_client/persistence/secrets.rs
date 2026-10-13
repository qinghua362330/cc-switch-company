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
        let tmp_path = path.with_extension(format!("json.tmp.{}", uuid::Uuid::new_v4()));
        let result = (|| {
            let raw = serde_json::to_vec_pretty(secrets)
                .map_err(|_| AuthError::new("storage", "序列化公司认证密钥失败"))?;
            let mut file = create_secret_temp_file(&tmp_path)?;
            use std::io::Write;
            file.write_all(&raw)
                .and_then(|_| file.sync_all())
                .map_err(|_| AuthError::new("storage", "写入公司认证密钥失败"))?;
            drop(file);
            std::fs::rename(&tmp_path, &path)
                .map_err(|_| AuthError::new("storage", "保存公司认证密钥失败"))
        })();
        if result.is_err() {
            remove_secret_file(&tmp_path)?;
        }
        result
    }

    fn clear_secrets(&self) -> Result<(), AuthError> {
        // Attempt temporary cleanup even if the primary file cannot be removed.
        let mut result = remove_secret_file(&self.secrets_path());
        match std::fs::read_dir(&self.base_dir) {
            Ok(entries) => {
                for entry in entries {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(_) => {
                            if result.is_ok() {
                                result = Err(AuthError::new("storage", "读取公司认证密钥目录失败"));
                            }
                            continue;
                        }
                    };
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if name == format!("{SECRETS_FILE_NAME}.tmp")
                        || name
                            .strip_prefix(&format!("{SECRETS_FILE_NAME}.tmp."))
                            .is_some_and(|suffix| uuid::Uuid::parse_str(suffix).is_ok())
                    {
                        let cleanup = remove_secret_file(&entry.path());
                        if result.is_ok() {
                            result = cleanup;
                        }
                    }
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                if result.is_ok() {
                    result = Err(AuthError::new("storage", "读取公司认证密钥目录失败"));
                }
            }
        }
        result
    }
}

fn create_secret_temp_file(path: &std::path::Path) -> Result<std::fs::File, AuthError> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|_| AuthError::new("storage", "创建公司认证密钥临时文件失败"))
}

fn remove_secret_file(path: &std::path::Path) -> Result<(), AuthError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(AuthError::new("storage", "清除公司认证密钥失败")),
    }
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

    #[test]
    fn file_secret_store_clear_removes_abandoned_temporary_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path().to_path_buf());
        let tmp = store.secrets_path().with_extension("json.tmp");
        std::fs::write(&tmp, b"abandoned secret").unwrap();
        store.clear_secrets().unwrap();
        assert!(!tmp.exists());
    }

    #[test]
    fn file_secret_store_failed_replace_cleans_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path().to_path_buf());
        std::fs::create_dir(store.secrets_path()).unwrap();
        assert!(store.save_secrets(&sample_secrets()).is_err());
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| {
                name.to_string_lossy()
                    .starts_with("company_auth_secrets.json.tmp")
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "temporary secrets remain: {leftovers:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_secret_store_does_not_follow_temporary_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path().to_path_buf());
        let unrelated = dir.path().join("unrelated");
        std::fs::write(&unrelated, b"keep me").unwrap();
        std::os::unix::fs::symlink(&unrelated, store.secrets_path().with_extension("json.tmp"))
            .unwrap();
        let _ = store.save_secrets(&sample_secrets());
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"keep me");
    }

    #[test]
    fn file_secret_store_clear_cleans_all_temps_even_if_primary_removal_fails() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path().to_path_buf());
        std::fs::create_dir(store.secrets_path()).unwrap();
        let legacy = store.secrets_path().with_extension("json.tmp");
        let unique = store
            .secrets_path()
            .with_extension(format!("json.tmp.{}", uuid::Uuid::new_v4()));
        std::fs::write(&legacy, b"legacy secret").unwrap();
        std::fs::write(&unique, b"unique secret").unwrap();
        assert!(store.clear_secrets().is_err());
        assert!(!legacy.exists());
        assert!(!unique.exists());
    }

    #[cfg(unix)]
    #[test]
    fn secret_temp_is_owner_only_before_any_secret_is_written() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new-secret.tmp");
        let file = create_secret_temp_file(&path).unwrap();
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(file.metadata().unwrap().len(), 0);
        assert!(create_secret_temp_file(&path).is_err());
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
