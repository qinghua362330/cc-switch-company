use std::sync::{Arc, Mutex};

use cc_switch_lib::auth_client::{
    redact_authorization_header, redact_secret, AuthError, AuthMetadata, AuthMetadataStore,
    AuthPersistence, AuthSecrets, AuthService, CatalogEntry, FileAuthMetadataStore,
    InMemorySecretStore, LoginResponse, ProvisionClient, ProvisionConfig, SecretStore,
    UserIdentity, DEFAULT_PROVISION_BASE_URL,
};
use futures::future::BoxFuture;

#[derive(Clone, Default)]
struct MockProvisionClient {
    login_result: Arc<Mutex<Option<Result<LoginResponse, AuthError>>>>,
    catalog_result: Arc<Mutex<Option<Result<(String, Vec<CatalogEntry>), AuthError>>>>,
    seen_authorization: Arc<Mutex<Option<String>>>,
}

#[derive(Clone, Default)]
struct FailingMetadataStore;

impl AuthMetadataStore for FailingMetadataStore {
    fn load_metadata(&self) -> Result<Option<AuthMetadata>, AuthError> {
        Ok(None)
    }

    fn save_metadata(&self, _metadata: &AuthMetadata) -> Result<(), AuthError> {
        Err(AuthError::new("storage", "metadata save failed"))
    }

    fn clear_metadata(&self) -> Result<(), AuthError> {
        Ok(())
    }
}

#[derive(Clone, Default)]
struct FailingClearSecretStore;

impl SecretStore for FailingClearSecretStore {
    fn load_secrets(&self) -> Result<Option<AuthSecrets>, AuthError> {
        Ok(Some(AuthSecrets {
            session_token: "test_session_token_1234567890".to_string(),
            api_key: ["fixture", "api", "key"].join("-"),
        }))
    }

    fn save_secrets(&self, _secrets: &AuthSecrets) -> Result<(), AuthError> {
        Ok(())
    }

    fn clear_secrets(&self) -> Result<(), AuthError> {
        Err(AuthError::new("storage", "secret clear failed"))
    }
}

impl ProvisionClient for MockProvisionClient {
    fn login<'a>(&'a self, _ticket: &'a str) -> BoxFuture<'a, Result<LoginResponse, AuthError>> {
        Box::pin(async move {
            self.login_result
                .lock()
                .unwrap()
                .take()
                .expect("login result")
        })
    }

    fn catalog<'a>(
        &'a self,
        session_token: &'a str,
    ) -> BoxFuture<'a, Result<(String, Vec<CatalogEntry>), AuthError>> {
        Box::pin(async move {
            *self.seen_authorization.lock().unwrap() = Some(format!("Bearer {session_token}"));
            self.catalog_result
                .lock()
                .unwrap()
                .take()
                .expect("catalog result")
        })
    }
}

fn sample_catalog() -> Vec<CatalogEntry> {
    vec![CatalogEntry {
        tool: "codex".to_string(),
        label: "Company Codex".to_string(),
        protocol: "openai-responses".to_string(),
        default_model: "gpt-5.5".to_string(),
        models: vec!["gpt-5.5".to_string(), "gpt-5.4".to_string()],
        group: "default".to_string(),
    }]
}

fn sample_login() -> LoginResponse {
    LoginResponse {
        session_token: "test_session_token_1234567890".to_string(),
        api_key: ["fixture", "api", "key"].join("-"),
        base_url: "https://leharrt.com".to_string(),
        user: UserIdentity {
            display_name: "Alice".to_string(),
            email: "alice@example.com".to_string(),
        },
        catalog: sample_catalog(),
    }
}

fn sample_login_with_newapi_gemini_models() -> LoginResponse {
    LoginResponse {
        catalog: vec![CatalogEntry {
            tool: "codex".to_string(),
            label: "Company NewAPI".to_string(),
            protocol: "openai-responses".to_string(),
            default_model: "gpt-5.5".to_string(),
            models: vec![
                "gpt-5.5".to_string(),
                "google/gemini-3.5-flash".to_string(),
                "gemini-2.5-pro".to_string(),
            ],
            group: "default".to_string(),
        }],
        ..sample_login()
    }
}

#[tokio::test]
async fn login_persists_redacted_state_without_exposing_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let client = MockProvisionClient::default();
    *client.login_result.lock().unwrap() = Some(Ok(sample_login()));
    let persistence = AuthPersistence::new(
        FileAuthMetadataStore::new(dir.path().to_path_buf()),
        InMemorySecretStore::default(),
    );
    let service = AuthService::new(client, persistence);

    let state = service.login_with_ticket("fs_test_ok").await.unwrap();

    assert!(state.authenticated);
    assert_eq!(state.user.as_ref().unwrap().email, "alice@example.com");
    assert_eq!(state.catalog.len(), 1);
    assert!(state.api_key_preview.as_deref().unwrap().contains("..."));
    assert!(state
        .session_token_preview
        .as_deref()
        .unwrap()
        .contains("..."));
    let json = serde_json::to_string(&state).unwrap();
    assert!(!json.contains("sk-test-secret-1234567890"));
    assert!(!json.contains("test_session_token_1234567890"));
}

#[tokio::test]
async fn login_derives_gemini_catalog_entry_from_newapi_models() {
    let dir = tempfile::tempdir().unwrap();
    let client = MockProvisionClient::default();
    *client.login_result.lock().unwrap() = Some(Ok(sample_login_with_newapi_gemini_models()));
    let persistence = AuthPersistence::new(
        FileAuthMetadataStore::new(dir.path().to_path_buf()),
        InMemorySecretStore::default(),
    );
    let service = AuthService::new(client, persistence);

    let state = service.login_with_ticket("fs_test_ok").await.unwrap();

    let gemini = state
        .catalog
        .iter()
        .find(|entry| entry.tool == "gemini")
        .expect("gemini catalog entry");
    assert_eq!(gemini.label, "公司号池 Gemini");
    assert_eq!(gemini.default_model, "google/gemini-3.5-flash");
    assert_eq!(
        gemini.models,
        vec![
            "google/gemini-3.5-flash".to_string(),
            "gemini-2.5-pro".to_string()
        ]
    );
}

#[tokio::test]
async fn login_rolls_back_secret_when_metadata_save_fails() {
    let client = MockProvisionClient::default();
    *client.login_result.lock().unwrap() = Some(Ok(sample_login()));
    let secret_store = InMemorySecretStore::default();
    let persistence = AuthPersistence::new(FailingMetadataStore, secret_store.clone());
    let service = AuthService::new(client, persistence);

    let err = service.login_with_ticket("fs_test_ok").await.unwrap_err();

    assert_eq!(err.kind(), "storage");
    assert!(secret_store.load_secrets().unwrap().is_none());
    assert!(!service.get_state().await.unwrap().authenticated);
}

#[test]
fn parses_valid_login_and_accepts_empty_catalog() {
    let raw = r#"{
        "session_token":"test_session_token_1234567890",
        "api_key":"sk-test-secret-1234567890",
        "base_url":"https://leharrt.com",
        "user":{"display_name":"Alice","email":"alice@example.com"},
        "catalog":[]
    }"#;

    let parsed: LoginResponse = serde_json::from_str(raw).unwrap();
    parsed.validate().unwrap();
    assert!(parsed.catalog.is_empty());
}

#[test]
fn rejects_missing_required_fields_and_malformed_json() {
    let missing_api_key = r#"{
        "session_token":"test_session_token_1234567890",
        "base_url":"https://leharrt.com",
        "user":{"display_name":"Alice","email":"alice@example.com"},
        "catalog":[]
    }"#;
    assert!(serde_json::from_str::<LoginResponse>(missing_api_key).is_err());
    assert!(serde_json::from_str::<LoginResponse>("{not json").is_err());
}

#[test]
fn maps_auth_errors_and_redacts_secrets() {
    assert_eq!(AuthError::from_status(401).kind(), "unauthorized");
    assert_eq!(AuthError::from_status(403).kind(), "forbidden");
    assert_eq!(redact_secret("sk-test-secret-1234567890"), "sk-t...7890");
    assert_eq!(
        redact_authorization_header("Bearer test_session_token_1234567890"),
        "Bearer test...7890"
    );
}

#[tokio::test]
async fn logout_clears_state_and_catalog_401_clears_auth() {
    let dir = tempfile::tempdir().unwrap();
    let client = MockProvisionClient::default();
    *client.login_result.lock().unwrap() = Some(Ok(sample_login()));
    let persistence = AuthPersistence::new(
        FileAuthMetadataStore::new(dir.path().to_path_buf()),
        InMemorySecretStore::default(),
    );
    let service = AuthService::new(client.clone(), persistence);

    service.login_with_ticket("fs_test_ok").await.unwrap();
    service.logout().await.unwrap();
    assert!(!service.get_state().await.unwrap().authenticated);

    *client.login_result.lock().unwrap() = Some(Ok(sample_login()));
    service.login_with_ticket("fs_test_ok").await.unwrap();
    *client.catalog_result.lock().unwrap() = Some(Err(AuthError::from_status(401)));
    assert!(service.refresh_catalog().await.is_err());
    assert!(!service.get_state().await.unwrap().authenticated);
}

#[tokio::test]
async fn logout_returns_error_when_secret_clear_fails() {
    let dir = tempfile::tempdir().unwrap();
    let service = AuthService::new(
        MockProvisionClient::default(),
        AuthPersistence::new(
            FileAuthMetadataStore::new(dir.path().to_path_buf()),
            FailingClearSecretStore,
        ),
    );

    let err = service.logout().await.unwrap_err();

    assert_eq!(err.kind(), "storage");
    assert!(err.to_string().contains("secret clear failed"));
}

#[tokio::test]
async fn refresh_catalog_sends_bearer_and_never_returns_it() {
    let dir = tempfile::tempdir().unwrap();
    let client = MockProvisionClient::default();
    *client.login_result.lock().unwrap() = Some(Ok(sample_login()));
    *client.catalog_result.lock().unwrap() = Some(Ok((
        "https://leharrt.com".to_string(),
        vec![CatalogEntry {
            label: "Changed".to_string(),
            ..sample_catalog().remove(0)
        }],
    )));
    let persistence = AuthPersistence::new(
        FileAuthMetadataStore::new(dir.path().to_path_buf()),
        InMemorySecretStore::default(),
    );
    let service = AuthService::new(client.clone(), persistence);

    service.login_with_ticket("fs_test_ok").await.unwrap();
    let state = service.refresh_catalog().await.unwrap();

    assert_eq!(
        client.seen_authorization.lock().unwrap().as_deref(),
        Some("Bearer test_session_token_1234567890")
    );
    assert_eq!(state.catalog[0].label, "Changed");
    assert!(!serde_json::to_string(&state)
        .unwrap()
        .contains("test_session_token_1234567890"));
}

#[test]
fn provision_base_url_defaults_to_leharrt() {
    assert_eq!(DEFAULT_PROVISION_BASE_URL, "https://leharrt.com");
    assert_eq!(
        ProvisionConfig::default().base_url.as_str(),
        "https://leharrt.com"
    );
}
