use std::path::PathBuf;

use super::AuthMetadataStore;
use crate::auth_client::models::{AuthError, AuthMetadata};

const METADATA_FILE_NAME: &str = "company_auth.json";

#[derive(Clone)]
pub struct FileAuthMetadataStore {
    base_dir: PathBuf,
}

impl FileAuthMetadataStore {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    fn metadata_path(&self) -> PathBuf {
        self.base_dir.join(METADATA_FILE_NAME)
    }
}

impl AuthMetadataStore for FileAuthMetadataStore {
    fn load_metadata(&self) -> Result<Option<AuthMetadata>, AuthError> {
        let path = self.metadata_path();
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|_| AuthError::new("storage", "读取登录元数据失败"))?;
        let metadata = serde_json::from_str::<AuthMetadata>(&raw)
            .map_err(|_| AuthError::new("storage", "登录元数据格式不正确"))?;
        Ok(Some(metadata))
    }

    fn save_metadata(&self, metadata: &AuthMetadata) -> Result<(), AuthError> {
        std::fs::create_dir_all(&self.base_dir)
            .map_err(|_| AuthError::new("storage", "创建登录元数据目录失败"))?;
        let path = self.metadata_path();
        let tmp_path = path.with_extension("json.tmp");
        let raw = serde_json::to_vec_pretty(metadata)
            .map_err(|_| AuthError::new("storage", "序列化登录元数据失败"))?;
        std::fs::write(&tmp_path, raw)
            .map_err(|_| AuthError::new("storage", "写入登录元数据失败"))?;
        std::fs::rename(&tmp_path, &path)
            .map_err(|_| AuthError::new("storage", "保存登录元数据失败"))?;
        Ok(())
    }

    fn clear_metadata(&self) -> Result<(), AuthError> {
        let path = self.metadata_path();
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(AuthError::new("storage", "清除登录元数据失败")),
        }
    }
}
