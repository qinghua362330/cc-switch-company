use crate::auth_client::models::{AuthError, AuthSecrets};

const KEYCHAIN_SERVICE: &str = "cc-switch-company-auth";
const KEYCHAIN_ACCOUNT: &str = "default";

#[cfg(target_os = "macos")]
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct MacKeychainError {
    code: i32,
    message: Option<String>,
}

#[cfg(target_os = "macos")]
impl MacKeychainError {
    const fn is_not_found(&self) -> bool {
        self.code == ERR_SEC_ITEM_NOT_FOUND
    }
}

#[cfg(target_os = "macos")]
impl std::fmt::Display for MacKeychainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.message {
            Some(message) if !message.is_empty() => write!(f, "{message} ({})", self.code),
            _ => write!(f, "Keychain OSStatus {}", self.code),
        }
    }
}

#[cfg(target_os = "macos")]
impl From<security_framework::base::Error> for MacKeychainError {
    fn from(error: security_framework::base::Error) -> Self {
        Self {
            code: error.code(),
            message: Some(error.to_string()),
        }
    }
}

#[cfg(target_os = "macos")]
trait MacKeychainBackend {
    fn get_generic_password(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Vec<u8>, MacKeychainError>;

    fn set_generic_password(
        &self,
        service: &str,
        account: &str,
        password: &[u8],
    ) -> Result<(), MacKeychainError>;

    fn delete_generic_password(&self, service: &str, account: &str)
        -> Result<(), MacKeychainError>;
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
struct SecurityFrameworkKeychainBackend;

#[cfg(target_os = "macos")]
impl MacKeychainBackend for SecurityFrameworkKeychainBackend {
    fn get_generic_password(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Vec<u8>, MacKeychainError> {
        security_framework::passwords::get_generic_password(service, account)
            .map_err(MacKeychainError::from)
    }

    fn set_generic_password(
        &self,
        service: &str,
        account: &str,
        password: &[u8],
    ) -> Result<(), MacKeychainError> {
        security_framework::passwords::set_generic_password(service, account, password)
            .map_err(MacKeychainError::from)
    }

    fn delete_generic_password(
        &self,
        service: &str,
        account: &str,
    ) -> Result<(), MacKeychainError> {
        security_framework::passwords::delete_generic_password(service, account)
            .map_err(MacKeychainError::from)
    }
}

#[cfg(target_os = "macos")]
pub(super) fn load_keychain_secrets() -> Result<Option<AuthSecrets>, AuthError> {
    load_keychain_secrets_with_backend(&SecurityFrameworkKeychainBackend)
}

#[cfg(target_os = "macos")]
fn load_keychain_secrets_with_backend(
    backend: &impl MacKeychainBackend,
) -> Result<Option<AuthSecrets>, AuthError> {
    let password = match backend.get_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT) {
        Ok(password) => password,
        Err(err) if err.is_not_found() => return Ok(None),
        Err(_) => return Err(AuthError::new("storage", "无法访问 macOS Keychain")),
    };
    let raw = String::from_utf8(password)
        .map_err(|_| AuthError::new("storage", "Keychain 数据格式不正确"))?;
    let secrets = serde_json::from_str::<AuthSecrets>(raw.trim())
        .map_err(|_| AuthError::new("storage", "Keychain 数据格式不正确"))?;
    Ok(Some(secrets))
}

#[cfg(not(target_os = "macos"))]
pub(super) fn load_keychain_secrets() -> Result<Option<AuthSecrets>, AuthError> {
    Ok(None)
}

#[cfg(target_os = "macos")]
pub(super) fn save_keychain_secrets(secrets: &AuthSecrets) -> Result<(), AuthError> {
    save_keychain_secrets_with_backend(&SecurityFrameworkKeychainBackend, secrets)
}

#[cfg(target_os = "macos")]
fn save_keychain_secrets_with_backend(
    backend: &impl MacKeychainBackend,
    secrets: &AuthSecrets,
) -> Result<(), AuthError> {
    let raw = serde_json::to_vec(secrets)
        .map_err(|_| AuthError::new("storage", "序列化 Keychain 数据失败"))?;
    backend
        .set_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT, &raw)
        .map_err(|_| AuthError::new("storage", "保存 Keychain 数据失败"))
}

#[cfg(not(target_os = "macos"))]
pub(super) fn save_keychain_secrets(_secrets: &AuthSecrets) -> Result<(), AuthError> {
    Err(AuthError::new("storage", "当前平台未启用公司认证安全存储"))
}

#[cfg(target_os = "macos")]
pub(super) fn clear_keychain_secrets() -> Result<(), AuthError> {
    clear_keychain_secrets_with_backend(&SecurityFrameworkKeychainBackend)
}

#[cfg(target_os = "macos")]
fn clear_keychain_secrets_with_backend(backend: &impl MacKeychainBackend) -> Result<(), AuthError> {
    match backend.delete_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT) {
        Ok(()) => Ok(()),
        Err(err) if err.is_not_found() => Ok(()),
        Err(err) => Err(AuthError::new(
            "storage",
            format!("清除 Keychain 数据失败: {err}"),
        )),
    }
}

#[cfg(not(target_os = "macos"))]
pub(super) fn clear_keychain_secrets() -> Result<(), AuthError> {
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod mac_keychain_tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct RecordingKeychainBackend {
        saved_passwords: RefCell<Vec<Vec<u8>>>,
        delete_result: RefCell<Option<Result<(), MacKeychainError>>>,
    }

    impl MacKeychainBackend for RecordingKeychainBackend {
        fn get_generic_password(
            &self,
            _service: &str,
            _account: &str,
        ) -> Result<Vec<u8>, MacKeychainError> {
            Ok(Vec::new())
        }

        fn set_generic_password(
            &self,
            _service: &str,
            _account: &str,
            password: &[u8],
        ) -> Result<(), MacKeychainError> {
            self.saved_passwords.borrow_mut().push(password.to_vec());
            Ok(())
        }

        fn delete_generic_password(
            &self,
            _service: &str,
            _account: &str,
        ) -> Result<(), MacKeychainError> {
            self.delete_result.borrow_mut().take().unwrap_or(Ok(()))
        }
    }

    fn sample_secrets() -> AuthSecrets {
        AuthSecrets {
            session_token: "test_session_token_1234567890".to_string(),
            api_key: ["fixture", "api", "key"].join("-"),
        }
    }

    #[test]
    fn save_keychain_secrets_passes_serialized_secret_bytes_to_backend() {
        let backend = RecordingKeychainBackend::default();

        save_keychain_secrets_with_backend(&backend, &sample_secrets()).unwrap();

        let saved_passwords = backend.saved_passwords.borrow();
        assert_eq!(saved_passwords.len(), 1);
        let saved = std::str::from_utf8(&saved_passwords[0]).unwrap();
        assert!(saved.contains("test_session_token_1234567890"));
        assert!(saved.contains("sk-test-secret-1234567890"));
    }

    #[test]
    fn clear_keychain_secrets_treats_missing_item_as_success() {
        let backend = RecordingKeychainBackend::default();
        *backend.delete_result.borrow_mut() = Some(Err(MacKeychainError {
            code: ERR_SEC_ITEM_NOT_FOUND,
            message: None,
        }));

        clear_keychain_secrets_with_backend(&backend).unwrap();
    }

    #[test]
    fn clear_keychain_secrets_surfaces_real_delete_failures() {
        let backend = RecordingKeychainBackend::default();
        *backend.delete_result.borrow_mut() = Some(Err(MacKeychainError {
            code: -25293,
            message: None,
        }));

        let err = clear_keychain_secrets_with_backend(&backend).unwrap_err();

        assert_eq!(err.kind(), "storage");
        assert!(err.to_string().contains("清除 Keychain 数据失败"));
    }
}
