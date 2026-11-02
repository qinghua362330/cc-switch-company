//! 代理服务业务逻辑层
//!
//! 提供代理服务器的启动、停止和配置管理

use crate::app_config::AppType;
use crate::config::{get_claude_settings_path, read_json_file};
use crate::database::Database;
use crate::provider::Provider;
use crate::proxy::server::ProxyServer;
use crate::proxy::switch_lock::SwitchLockManager;
use crate::proxy::types::*;
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::RwLock;

use crate::live::project::claude::PROXY_TOKEN_PLACEHOLDER;

#[derive(Clone)]
pub struct ProxyService {
    db: Arc<Database>,
    server: Arc<RwLock<Option<ProxyServer>>>,
    /// AppHandle，用于传递给 ProxyServer 以支持故障转移时的 UI 更新
    app_handle: Arc<RwLock<Option<tauri::AppHandle>>>,
    switch_locks: SwitchLockManager,
}

/// 重启失败时写回库的那份配置：设置页只动四个全局字段，旧接口是整份七个字段。
enum SavedProxyConfig {
    Global(GlobalProxyConfig),
    Legacy(ProxyConfig),
}

impl SavedProxyConfig {
    async fn write(&self, db: &Database) -> Result<(), String> {
        match self {
            Self::Global(config) => db.update_global_proxy_config(config.clone()).await,
            Self::Legacy(config) => db.update_proxy_config(config.clone()).await,
        }
        .map_err(|e| format!("恢复原代理配置失败: {e}"))
    }
}

/// 客户端连接代理用的地址。`host` 是设置里的客户端地址，没设时是监听地址，后者可能是
/// `0.0.0.0` / `::`（监听所有网卡），客户端连不上这个地址，改用本机回环；IPv6 加方括号。
pub(crate) fn proxy_origin(host: &str, listen_port: u16) -> String {
    let host = match host {
        "0.0.0.0" => "127.0.0.1",
        "::" => "::1",
        other => other,
    };
    if host.contains(':') && !host.starts_with('[') {
        format!("http://[{host}]:{listen_port}")
    } else {
        format!("http://{host}:{listen_port}")
    }
}

impl ProxyService {
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            db,
            server: Arc::new(RwLock::new(None)),
            app_handle: Arc::new(RwLock::new(None)),
            switch_locks: SwitchLockManager::new(),
        }
    }

    /// 设置 AppHandle（在应用初始化时调用）
    pub fn set_app_handle(&self, handle: tauri::AppHandle) {
        futures::executor::block_on(async {
            *self.app_handle.write().await = Some(handle);
        });
    }

    pub(crate) async fn lock_switch_for_app(
        &self,
        app_type: &str,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        self.switch_locks.lock_for_app(app_type).await
    }

    /// 启动代理服务器。成功、失败都让托盘比对一次（图标圆点、问题区、「退出」的后果跟着变）。
    pub async fn start(&self) -> Result<ProxyServerInfo, String> {
        let result = self.start_server().await;
        self.notify_tray().await;
        result
    }

    /// 停止代理服务器。同 [`Self::start`]，结束后让托盘比对一次。
    pub async fn stop(&self) -> Result<(), String> {
        let result = self.stop_server().await;
        self.notify_tray().await;
        result
    }

    /// 托盘在后台线程比对、变了才重建，这里不等它。
    async fn notify_tray(&self) {
        if let Some(handle) = self.app_handle.read().await.as_ref() {
            crate::tray::schedule_tray_status_check(handle);
        }
    }

    async fn start_server(&self) -> Result<ProxyServerInfo, String> {
        // 1. 启动时自动设置 proxy_enabled = true
        let mut global_config = self
            .db
            .get_global_proxy_config()
            .await
            .map_err(|e| format!("获取全局代理配置失败: {e}"))?;

        if !global_config.proxy_enabled {
            global_config.proxy_enabled = true;
            self.db
                .update_global_proxy_config(global_config.clone())
                .await
                .map_err(|e| format!("更新代理总开关失败: {e}"))?;
        }

        // 2. 获取配置
        let config = self
            .db
            .get_proxy_config()
            .await
            .map_err(|e| format!("获取代理配置失败: {e}"))?;

        // 3. 若已在运行：确保持久化状态（如需要）并返回当前信息
        if let Some(server) = self.server.read().await.as_ref() {
            let status = server.get_status().await;
            return Ok(ProxyServerInfo {
                address: status.address,
                port: status.port,
                // 无法精确取回首次启动时间，返回当前时间用于 UI 展示即可
                started_at: chrono::Utc::now().to_rfc3339(),
            });
        }

        // 4. 创建并启动服务器
        let app_handle = self.app_handle.read().await.clone();
        let server = ProxyServer::new(config.clone(), self.db.clone(), app_handle);
        let info = server
            .start()
            .await
            .map_err(|e| format!("启动代理服务器失败: {e}"))?;
        if let Err(e) = self
            .persist_ephemeral_listen_port_if_needed(&config, info.port)
            .await
        {
            let _ = server.stop().await;
            return Err(e);
        }

        // 5. 保存服务器实例
        *self.server.write().await = Some(server);

        log::info!("代理服务器已启动: {}:{}", info.address, info.port);
        Ok(info)
    }

    async fn persist_ephemeral_listen_port_if_needed(
        &self,
        config: &ProxyConfig,
        actual_port: u16,
    ) -> Result<(), String> {
        if config.listen_port != 0 {
            return Ok(());
        }

        // 端口是全局字段，不能通过旧接口回写各应用独立的重试和超时配置。
        let mut resolved_config = self
            .db
            .get_global_proxy_config()
            .await
            .map_err(|e| format!("获取全局代理配置失败: {e}"))?;
        resolved_config.listen_port = actual_port;
        self.db
            .update_global_proxy_config(resolved_config)
            .await
            .map_err(|e| format!("保存动态代理端口失败: {e}"))
    }

    /// 各应用是否处于代理模式（读设备本地的模式状态，不读会随云同步的
    /// `proxy_config.enabled`）。
    pub async fn get_takeover_status(&self) -> Result<ProxyTakeoverStatus, String> {
        let [claude, codex, gemini, grokbuild, claude_desktop] =
            crate::mode::current::proxy_flags([
                AppType::Claude,
                AppType::Codex,
                AppType::Gemini,
                AppType::GrokBuild,
                AppType::ClaudeDesktop,
            ]);
        Ok(ProxyTakeoverStatus {
            claude,
            codex,
            gemini,
            grokbuild,
            claude_desktop,
            // OpenCode and OpenClaw don't support proxy features
            opencode: false,
            openclaw: false,
        })
    }

    /// 把代理的上游指向 `provider`（状态面板里的「使用中」）。
    pub(crate) async fn set_active_target(&self, app_type: &AppType, provider: &Provider) {
        if let Some(server) = self.server.read().await.as_ref() {
            server
                .set_active_target(app_type.as_str(), &provider.id, &provider.name)
                .await;
        }
    }

    pub(crate) async fn emit(&self, event: &str, payload: Value) {
        if let Some(handle) = self.app_handle.read().await.as_ref() {
            if let Err(error) = handle.emit(event, payload) {
                log::warn!("发送事件 {event} 失败: {error}");
            }
        }
    }

    async fn stop_server(&self) -> Result<(), String> {
        if let Some(server) = self.server.write().await.take() {
            server
                .stop()
                .await
                .map_err(|e| format!("停止代理服务器失败: {e}"))?;

            // 停止时设置 proxy_enabled = false
            let mut global_config = self
                .db
                .get_global_proxy_config()
                .await
                .map_err(|e| format!("获取全局代理配置失败: {e}"))?;

            if global_config.proxy_enabled {
                global_config.proxy_enabled = false;
                if let Err(e) = self.db.update_global_proxy_config(global_config).await {
                    log::warn!("更新代理总开关失败: {e}");
                }
            }

            log::info!("代理服务器已停止");
            Ok(())
        } else {
            Err("代理服务器未运行".to_string())
        }
    }

    /// 构造写入 Live 的代理地址（处理 0.0.0.0 / IPv6 等特殊情况）
    pub(crate) async fn build_proxy_urls(&self) -> Result<(String, String), String> {
        let config = self
            .db
            .get_proxy_config()
            .await
            .map_err(|e| format!("获取代理配置失败: {e}"))?;

        let mut listen_port = config.listen_port;
        if let Some(server) = self.server.read().await.as_ref() {
            let status = server.get_status().await;
            if status.running {
                listen_port = status.port;
            }
        }
        if listen_port == 0 {
            return Err("代理监听端口为 0，但代理服务器尚未运行，无法生成接管地址".to_string());
        }

        let host = crate::settings::get_proxy_client_host().unwrap_or(config.listen_address);
        let proxy_url = proxy_origin(&host, listen_port);
        let proxy_codex_base_url = format!("{proxy_url}/v1");
        Ok((proxy_url, proxy_codex_base_url))
    }

    /// 客户端文件里有没有接管占位符 `PROXY_MANAGED`（旧版接管的遗留物，或新版接上代理
    /// 时写的契约）。Codex 的官方路由不带占位符，按指向本地代理的地址认。
    pub(crate) fn live_has_proxy_placeholder(&self, app_type: &AppType) -> bool {
        match app_type {
            AppType::Claude => match self.read_claude_live() {
                Ok(config) => Self::is_claude_live_taken_over(&config),
                Err(_) => false,
            },
            AppType::Codex => {
                match self.read_codex_live() {
                    Ok(config) => Self::is_codex_live_taken_over(&config)
                        || config
                            .get("config")
                            .and_then(|v| v.as_str())
                            .is_some_and(|text| {
                                crate::services::provider::codex_direct::routes_official_to_proxy(
                                    &self.db, text,
                                )
                            }),
                    Err(_) => false,
                }
            }
            AppType::Gemini => match self.read_gemini_live() {
                Ok(config) => Self::is_gemini_live_taken_over(&config),
                Err(_) => false,
            },
            AppType::GrokBuild => match self.read_grok_live() {
                Ok(config) => Self::is_grok_live_taken_over(&config),
                Err(_) => false,
            },
            _ => false,
        }
    }

    /// 一份配置（客户端文件或供应商行）里有没有接管占位符：行里带着它的是旧版接管期间
    /// 被导入的残留，不能照写回 live，否则客户端会一直指着已经不在的本地代理。
    pub(crate) fn config_has_proxy_placeholder(app_type: &AppType, config: &Value) -> bool {
        match app_type {
            AppType::Claude => Self::is_claude_live_taken_over(config),
            AppType::Codex => Self::is_codex_live_taken_over(config),
            AppType::Gemini => Self::is_gemini_live_taken_over(config),
            AppType::GrokBuild => Self::is_grok_live_taken_over(config),
            _ => false,
        }
    }

    /// 是否有应用处于代理模式
    pub async fn is_takeover_active(&self) -> Result<bool, String> {
        let status = self.get_takeover_status().await?;
        Ok(status.claude
            || status.codex
            || status.gemini
            || status.grokbuild
            || status.claude_desktop)
    }

    fn is_claude_live_taken_over(config: &Value) -> bool {
        let env = match config.get("env").and_then(|v| v.as_object()) {
            Some(env) => env,
            None => return false,
        };

        for key in [
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "OPENROUTER_API_KEY",
            "OPENAI_API_KEY",
        ] {
            if env.get(key).and_then(|v| v.as_str()) == Some(PROXY_TOKEN_PLACEHOLDER) {
                return true;
            }
        }

        false
    }

    fn codex_live_has_proxy_placeholder(config: &Value) -> bool {
        if config
            .get("auth")
            .and_then(|v| v.as_object())
            .and_then(|auth| auth.get("OPENAI_API_KEY"))
            .and_then(|v| v.as_str())
            == Some(PROXY_TOKEN_PLACEHOLDER)
        {
            return true;
        }

        config
            .get("config")
            .and_then(|v| v.as_str())
            .and_then(crate::codex_config::extract_codex_experimental_bearer_token)
            .as_deref()
            == Some(PROXY_TOKEN_PLACEHOLDER)
    }

    fn is_codex_live_taken_over(config: &Value) -> bool {
        Self::codex_live_has_proxy_placeholder(config)
            || config
                .get("config")
                .and_then(|v| v.as_str())
                .is_some_and(crate::codex_config::codex_config_has_official_proxy_route)
    }

    fn is_gemini_live_taken_over(config: &Value) -> bool {
        let env = match config.get("env").and_then(|v| v.as_object()) {
            Some(env) => env,
            None => return false,
        };
        env.get("GEMINI_API_KEY").and_then(|v| v.as_str()) == Some(PROXY_TOKEN_PLACEHOLDER)
    }

    fn is_grok_live_taken_over(config: &Value) -> bool {
        config
            .get("config")
            .and_then(Value::as_str)
            .is_some_and(|config_toml| {
                crate::grok_config::has_proxy_placeholder(config_toml, PROXY_TOKEN_PLACEHOLDER)
            })
    }

    // ==================== Live 配置读写辅助方法 ====================

    fn read_claude_live(&self) -> Result<Value, String> {
        let path = get_claude_settings_path();
        if !path.exists() {
            return Err("Claude 配置文件不存在".to_string());
        }

        let mut value: Value =
            read_json_file(&path).map_err(|e| format!("读取 Claude 配置失败: {e}"))?;

        if value.is_null() {
            value = json!({});
        }

        if !value.is_object() {
            let kind = match &value {
                Value::Null => "null",
                Value::Bool(_) => "boolean",
                Value::Number(_) => "number",
                Value::String(_) => "string",
                Value::Array(_) => "array",
                Value::Object(_) => "object",
            };
            return Err(format!(
                "Claude 配置文件格式错误：根节点必须是 JSON 对象（当前为 {kind}），路径: {}",
                path.display()
            ));
        }

        Ok(value)
    }

    fn read_codex_live(&self) -> Result<Value, String> {
        crate::codex_config::read_codex_live_settings()
            .map_err(|e| format!("读取 Codex Live 配置失败: {e}"))
    }

    fn read_gemini_live(&self) -> Result<Value, String> {
        use crate::gemini_config::{env_to_json, get_gemini_env_path, read_gemini_env};

        let env_path = get_gemini_env_path();
        if !env_path.exists() {
            return Err("Gemini .env 文件不存在".to_string());
        }

        let env_map = read_gemini_env().map_err(|e| format!("读取 Gemini env 失败: {e}"))?;
        Ok(env_to_json(&env_map))
    }

    fn read_grok_live(&self) -> Result<Value, String> {
        crate::grok_config::read_grok_live_settings()
            .map_err(|e| format!("读取 Grok Build 配置失败: {e}"))
    }

    // ==================== 原有方法 ====================

    /// 获取服务器状态
    pub async fn get_status(&self) -> Result<ProxyStatus, String> {
        if let Some(server) = self.server.read().await.as_ref() {
            Ok(server.get_status().await)
        } else {
            // 服务器未运行时返回默认状态
            Ok(ProxyStatus {
                running: false,
                ..Default::default()
            })
        }
    }

    /// 获取代理配置
    pub async fn get_config(&self) -> Result<ProxyConfig, String> {
        self.db
            .get_proxy_config()
            .await
            .map_err(|e| format!("获取代理配置失败: {e}"))
    }

    /// 更新代理配置（旧接口：七个字段整份写进四行）。返回代理是否因地址或端口变了而重启：
    /// 重启后调用方要按新地址重写接上代理的客户端（`mode::controller::resync_route`）。
    pub async fn update_config(&self, config: &ProxyConfig) -> Result<bool, String> {
        // 记录旧配置用于判定是否需要重启
        let previous = self
            .db
            .get_proxy_config()
            .await
            .map_err(|e| format!("获取代理配置失败: {e}"))?;

        // 保存到数据库（保持 live_takeover_active 状态不变）
        let mut new_config = config.clone();
        new_config.live_takeover_active = previous.live_takeover_active;

        self.db
            .update_proxy_config(new_config.clone())
            .await
            .map_err(|e| format!("保存代理配置失败: {e}"))?;

        // 判断是否需要重启（地址或端口变更）
        let require_restart = new_config.listen_address != previous.listen_address
            || new_config.listen_port != previous.listen_port;
        self.apply_saved_config(require_restart, SavedProxyConfig::Legacy(previous))
            .await
    }

    /// 设置页「保存并重启服务」：只写四个全局字段。各应用自己的重试次数和超时不碰——
    /// [`Self::update_config`] 走的旧 DAO 读的是 claude 行、写的是四行，会把 claude 的
    /// 应用级字段广播给其他三家。返回值同 [`Self::update_config`]。
    pub async fn update_global_config(&self, config: &GlobalProxyConfig) -> Result<bool, String> {
        let previous = self
            .db
            .get_global_proxy_config()
            .await
            .map_err(|e| format!("获取全局代理配置失败: {e}"))?;

        self.db
            .update_global_proxy_config(config.clone())
            .await
            .map_err(|e| format!("保存全局代理配置失败: {e}"))?;

        let require_restart = config.listen_address != previous.listen_address
            || config.listen_port != previous.listen_port;
        self.apply_saved_config(require_restart, SavedProxyConfig::Global(previous))
            .await
    }

    /// 让运行中的服务用上刚写进库的配置：地址或端口变了就重启（返回 true），否则实时应用。
    /// 服务没在跑时什么都不做。新地址绑不上就把 `previous` 写回库、按旧地址重新拉起，让
    /// 「保存失败」就是什么都没变：服务停着不管的话，下一次保存会因为没在跑而假成功，
    /// 客户端也还指着旧地址。
    async fn apply_saved_config(
        &self,
        require_restart: bool,
        previous: SavedProxyConfig,
    ) -> Result<bool, String> {
        let mut server_guard = self.server.write().await;
        if server_guard.is_none() {
            return Ok(false);
        }
        let new_config = self
            .db
            .get_proxy_config()
            .await
            .map_err(|e| format!("获取代理配置失败: {e}"))?;

        if !require_restart {
            if let Some(server) = server_guard.as_ref() {
                server.apply_runtime_config(&new_config).await;
                log::info!("代理配置已实时应用，无需重启代理服务器");
            }
            return Ok(false);
        }

        if let Some(server) = server_guard.take() {
            server
                .stop()
                .await
                .map_err(|e| format!("重启前停止代理服务器失败: {e}"))?;
        }

        let error = match self.start_with(&new_config).await {
            Ok(server) => {
                *server_guard = Some(server);
                log::info!("代理配置已更新，服务器已自动重启应用最新配置");
                return Ok(true);
            }
            Err(error) => format!("重启代理服务器失败: {error}"),
        };

        log::warn!("{error}，恢复原配置并重新启动");
        match self.restore_and_start(previous).await {
            Ok(server) => {
                *server_guard = Some(server);
                Err(format!("{error}；已按原配置重新启动"))
            }
            Err(restore_error) => Err(format!("{error}；恢复原配置也失败: {restore_error}")),
        }
    }

    /// 按 `config` 起一个新服务，动态端口回写进库。
    async fn start_with(&self, config: &ProxyConfig) -> Result<ProxyServer, String> {
        let app_handle = self.app_handle.read().await.clone();
        let server = ProxyServer::new(config.clone(), self.db.clone(), app_handle);
        let info = server.start().await.map_err(|e| e.to_string())?;
        if let Err(e) = self
            .persist_ephemeral_listen_port_if_needed(config, info.port)
            .await
        {
            let _ = server.stop().await;
            return Err(e);
        }
        Ok(server)
    }

    async fn restore_and_start(&self, previous: SavedProxyConfig) -> Result<ProxyServer, String> {
        previous.write(&self.db).await?;
        let config = self
            .db
            .get_proxy_config()
            .await
            .map_err(|e| format!("获取代理配置失败: {e}"))?;
        self.start_with(&config).await
    }

    /// 检查服务器是否正在运行
    pub async fn is_running(&self) -> bool {
        self.server.read().await.is_some()
    }

    /// 同 [`Self::is_running`]，但不等锁：托盘菜单这种同步路径用。正在启动 / 停止、拿不到锁时
    /// 为 `None`（按「不知道」处理，不报问题）。
    pub fn running_now(&self) -> Option<bool> {
        self.server.try_read().ok().map(|server| server.is_some())
    }

    /// 热更新熔断器配置
    ///
    /// 如果代理服务器正在运行，将新配置应用到所有已创建的熔断器实例
    pub async fn update_circuit_breaker_configs(
        &self,
        config: crate::proxy::CircuitBreakerConfig,
    ) -> Result<(), String> {
        if let Some(server) = self.server.read().await.as_ref() {
            server.update_circuit_breaker_configs(config).await;
            log::info!("已热更新运行中的熔断器配置");
        } else {
            log::debug!("代理服务器未运行，熔断器配置将在下次启动时生效");
        }
        Ok(())
    }

    /// 热更新指定应用的熔断器配置
    pub async fn update_circuit_breaker_config_for_app(
        &self,
        app_type: &str,
        config: crate::proxy::CircuitBreakerConfig,
    ) -> Result<(), String> {
        if let Some(server) = self.server.read().await.as_ref() {
            server
                .update_circuit_breaker_config_for_app(app_type, config)
                .await;
            log::info!("已热更新 {app_type} 运行中的熔断器配置");
        } else {
            log::debug!("{app_type} 熔断器配置将在下次代理启动时生效");
        }
        Ok(())
    }

    /// 重置指定 Provider 的熔断器
    ///
    /// 如果代理服务器正在运行，立即重置内存中的熔断器状态
    pub async fn reset_provider_circuit_breaker(
        &self,
        provider_id: &str,
        app_type: &str,
    ) -> Result<(), String> {
        if let Some(server) = self.server.read().await.as_ref() {
            server
                .reset_provider_circuit_breaker(provider_id, app_type)
                .await;
            log::info!("已重置 Provider {provider_id} (app: {app_type}) 的熔断器");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;
    use tempfile::TempDir;

    struct TempHome {
        #[allow(dead_code)]
        dir: TempDir,
        original_home: Option<String>,
        original_userprofile: Option<String>,
        original_test_home: Option<String>,
    }

    impl TempHome {
        fn new() -> Self {
            let dir = TempDir::new().expect("failed to create temp home");
            let original_home = env::var("HOME").ok();
            let original_userprofile = env::var("USERPROFILE").ok();
            let original_test_home = env::var("CC_SWITCH_TEST_HOME").ok();

            env::set_var("HOME", dir.path());
            env::set_var("USERPROFILE", dir.path());
            env::set_var("CC_SWITCH_TEST_HOME", dir.path());

            Self {
                dir,
                original_home,
                original_userprofile,
                original_test_home,
            }
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            match &self.original_home {
                Some(value) => env::set_var("HOME", value),
                None => env::remove_var("HOME"),
            }

            match &self.original_userprofile {
                Some(value) => env::set_var("USERPROFILE", value),
                None => env::remove_var("USERPROFILE"),
            }

            match &self.original_test_home {
                Some(value) => env::set_var("CC_SWITCH_TEST_HOME", value),
                None => env::remove_var("CC_SWITCH_TEST_HOME"),
            }
        }
    }

    #[tokio::test]
    #[serial]
    async fn company_compaction_controller_pipeline_preserves_native_auth_and_selector() {
        use crate::mode::controller;
        use crate::services::provider::codex_official_models::{self as official_models, testing};
        use crate::services::provider::ProviderService;

        struct ResetModels;
        impl Drop for ResetModels {
            fn drop(&mut self) {
                official_models::reset_test_env();
            }
        }
        fn live() -> toml_edit::DocumentMut {
            crate::codex_config::read_codex_config_text()
                .unwrap()
                .parse()
                .unwrap()
        }
        fn assert_clean_features() {
            let doc = live();
            assert!(doc["features"].get("remote_compaction_v2").is_none());
            assert_eq!(doc["features"]["keep_feature"].as_bool(), Some(true));
        }
        async fn refresh(state: &crate::store::AppState, provider: &Provider) -> bool {
            let _guard = controller::lock_settled(state, &AppType::Codex)
                .await
                .unwrap();
            controller::switch_route_locked(state, &AppType::Codex, provider)
                .await
                .unwrap()
                .client_config_changed
        }
        let _home = TempHome::new();
        crate::settings::reload_settings().unwrap();
        crate::settings::update_settings(crate::settings::AppSettings {
            unify_codex_session_history: true,
            preserve_codex_official_auth_on_switch: true,
            codex_quota_auto_fallback: false,
            ..Default::default()
        })
        .unwrap();
        let _models = ResetModels;
        testing::Fake {
            version: None,
            responses: Arc::new(std::sync::Mutex::new(vec![])),
            bundled: Some(testing::native_models(&[("official-model", 0)])),
            keychain: Arc::new(std::sync::Mutex::new(
                crate::services::subscription::CodexKeychainLogin::Unknown,
            )),
            keychain_reads: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            now: Arc::new(std::sync::Mutex::new(1)),
            calls: Arc::new(std::sync::Mutex::new(vec![])),
            on_fetch: None,
        }
        .install();
        let db = Arc::new(Database::memory().unwrap());
        let mut proxy_config = db.get_proxy_config().await.unwrap();
        proxy_config.listen_port = 0;
        db.update_proxy_config(proxy_config).await.unwrap();
        let state = crate::store::AppState::new(db.clone());
        let config = "model_provider = \"vendor-id\"\nmodel = \"gpt-5.6-sol\"\n[model_providers.vendor-id]\nname = \"Company\"\nbase_url = \"https://fixture.invalid/v1\"\nwire_api = \"responses\"\n[model_providers.vendor-id.capabilities]\nremote_compaction = \"v2\"\nunrelated = true\n";
        let mut company = Provider::with_id(
            "company-v2".into(),
            "Company V2".into(),
            json!({
                "auth": {"OPENAI_API_KEY": "company-fixture-key"}, "config": config,
            }),
            None,
        );
        company.meta = Some(crate::provider::ProviderMeta {
            provider_type: Some("company_auth".into()),
            ..Default::default()
        });
        let mut legacy = company.clone();
        legacy.id = "company-legacy".into();
        legacy.settings_config["config"] =
            json!(config.replace("remote_compaction = \"v2\"\n", ""));
        let mut official = Provider::with_id(
            "official".into(),
            "Official".into(),
            json!({
                "auth": {}, "config": "model = \"official-model\"\n",
            }),
            None,
        );
        official.category = Some("official".into());
        for provider in [&company, &legacy, &official] {
            db.save_provider("codex", provider).unwrap();
        }
        let auth = testing::login("workspace", "compaction-test");
        crate::codex_config::write_codex_live_atomic(&auth, Some("model = \"official-model\"\n"))
            .unwrap();
        let config_path = crate::codex_config::get_codex_config_path();
        let auth_path = crate::codex_config::get_codex_auth_path();
        let auth_bytes = std::fs::read(&auth_path).unwrap();
        // Real direct writes run the auth planner, stash and transactional config writer.
        for features in [
            "[features]\nremote_compaction_v2 = false\nkeep_feature = true\n",
            "features = { remote_compaction_v2 = false, keep_feature = true }\n",
        ] {
            std::fs::write(&config_path, features).unwrap();
            ProviderService::switch(&state, AppType::Codex, &company.id).unwrap();
            assert_clean_features();
            assert_eq!(std::fs::read(&auth_path).unwrap(), auth_bytes);
        }

        // An existing noncompany proxy contract must change when company cleanup is adopted.
        let mut personal = company.clone();
        personal.meta = None;
        db.save_provider("codex", &personal).unwrap();
        std::fs::write(
            &config_path,
            "features = { remote_compaction_v2 = false, keep_feature = true }\n",
        )
        .unwrap();
        controller::enter_with_route(&state, &AppType::Codex, false, Some(&personal.id))
            .await
            .unwrap();
        assert_eq!(
            live()["features"]["remote_compaction_v2"].as_bool(),
            Some(false)
        );
        db.save_provider("codex", &company).unwrap();
        assert!(
            refresh(&state, &company).await,
            "first company cleanup must publish"
        );
        assert_clean_features();
        let cleaned = std::fs::read(&config_path).unwrap();
        let stash_path =
            crate::live::engine::DeviceStore::for_device().file("codex-login-stash.json");
        let stash = std::fs::read(&stash_path).ok();
        assert!(
            !refresh(&state, &company).await,
            "same contract must skip the writer"
        );
        assert_eq!(std::fs::read(&config_path).unwrap(), cleaned);
        assert_eq!(std::fs::read(&auth_path).unwrap(), auth_bytes);
        assert_eq!(std::fs::read(&stash_path).ok(), stash);
        controller::exit(&state, &AppType::Codex).await.unwrap();

        ProviderService::switch(&state, AppType::Codex, &official.id).unwrap();
        controller::enter_with_route(&state, &AppType::Codex, true, Some(&official.id))
            .await
            .unwrap();
        let selector = live()["model_provider"].as_str().unwrap().to_string();
        let official_auth = std::fs::read(&auth_path).unwrap();
        assert_eq!(official_auth, auth_bytes);
        let official_stash = std::fs::read(&stash_path).ok();
        controller::set_stack_member(&state, &AppType::Codex, &company.id, true)
            .await
            .unwrap();
        let mixed = live();
        assert_eq!(mixed["model_provider"].as_str(), Some(selector.as_str()));
        assert_eq!(
            mixed["model_providers"][selector.as_str()]["capabilities"]["remote_compaction"]
                .as_str(),
            Some("unsupported")
        );
        assert_eq!(
            mixed["model_providers"][selector.as_str()]["requires_openai_auth"].as_bool(),
            Some(true)
        );
        assert!(mixed["model_providers"][selector.as_str()]
            .get("experimental_bearer_token")
            .is_none());
        assert_eq!(std::fs::read(&auth_path).unwrap(), official_auth);
        assert_eq!(std::fs::read(&stash_path).ok(), official_stash);
        let stack =
            crate::mode::state::stack(&crate::live::engine::DeviceStore::for_device(), "codex")
                .unwrap();
        let model = crate::mode::stack::encode(
            &AppType::Codex,
            stack.key_of(&company.id).unwrap(),
            "gpt-5.6-sol",
            false,
        );
        let catalog: Value =
            crate::config::read_json_file(&crate::codex_config::get_codex_model_catalog_path())
                .unwrap();
        assert!(catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["slug"] == model));
        controller::exit(&state, &AppType::Codex).await.unwrap();

        let mut settings = crate::settings::get_settings();
        settings.unify_codex_session_history = false;
        crate::settings::update_settings(settings).unwrap();
        ProviderService::switch(&state, AppType::Codex, &official.id).unwrap();
        assert!(live().get("model_provider").is_none());
        let before = std::fs::read(&config_path).unwrap();
        let before_auth = std::fs::read(&auth_path).unwrap();
        let before_stash = std::fs::read(&stash_path).ok();
        let error = controller::enter_with_route(&state, &AppType::Codex, true, Some(&official.id))
            .await
            .unwrap_err();
        assert!(error.contains("统一会话镜像"));
        assert_eq!(std::fs::read(&config_path).unwrap(), before);
        assert_eq!(std::fs::read(&auth_path).unwrap(), before_auth);
        assert_eq!(std::fs::read(&stash_path).ok(), before_stash);
        // The existing company Stack path without an explicit V2 opt-in stays allowed.
        controller::set_stack_member(&state, &AppType::Codex, &company.id, false)
            .await
            .unwrap();
        controller::set_stack_member(&state, &AppType::Codex, &legacy.id, true)
            .await
            .unwrap();
        controller::enter_with_route(&state, &AppType::Codex, true, Some(&official.id))
            .await
            .unwrap();
        assert!(live().get("model_provider").is_none());
        assert_eq!(std::fs::read(&auth_path).unwrap(), auth_bytes);
        controller::exit(&state, &AppType::Codex).await.unwrap();
        if state.proxy_service.is_running().await {
            state.proxy_service.stop().await.unwrap();
        }
        for provider in [&company, &legacy, &official] {
            assert_eq!(
                db.get_provider_by_id(&provider.id, "codex")
                    .unwrap()
                    .unwrap()
                    .settings_config,
                provider.settings_config
            );
        }
    }

    #[tokio::test]
    #[serial]
    async fn codex_quota_fallback_proxy_pipeline_preserves_auth_and_backup() {
        use crate::services::codex_quota_fallback::{check_with_probe, has_owned_fallback};
        use crate::services::subscription::{codex_login_quota_identity, CodexQuotaAvailability};
        use axum::{http::HeaderMap, routing::post, Json, Router};

        let requests = Arc::new(tokio::sync::Mutex::new(Vec::<(String, Value)>::new()));
        let upstream = Router::new().route(
            "/v1/responses",
            post({
                let requests = requests.clone();
                move |headers: HeaderMap, Json(body): Json<Value>| {
                    let requests = requests.clone();
                    async move {
                        let authorization = headers.get("authorization")
                            .and_then(|value| value.to_str().ok()).unwrap_or_default().to_string();
                        requests.lock().await.push((authorization, body));
                        Json(json!({
                            "id": "resp_company_test", "object": "response",
                            "created_at": 1, "status": "completed", "model": "company-model",
                            "output": [{"id": "msg_company_test", "type": "message",
                                "role": "assistant", "status": "completed",
                                "content": [{"type": "output_text", "text": "company response", "annotations": []}]}],
                            "usage": {"input_tokens": 1, "output_tokens": 2, "total_tokens": 3}
                        }))
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let upstream_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let upstream_handle = tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap();

        async fn assert_company_response(
            service: &ProxyService,
            client: &reqwest::Client,
            requests: &tokio::sync::Mutex<Vec<(String, Value)>>,
            phase: &str,
            expected_count: usize,
            model: &str,
        ) {
            let response = client
                .post(format!(
                    "{}/responses",
                    service.build_proxy_urls().await.unwrap().1
                ))
                .bearer_auth(PROXY_TOKEN_PLACEHOLDER)
                .json(&json!({"model": model, "input": phase, "stream": false}))
                .send()
                .await
                .unwrap();
            let status = response.status();
            let body = response.text().await.unwrap();
            assert_eq!(status, reqwest::StatusCode::OK, "proxy response: {body}");
            let body: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(body["output"][0]["content"][0]["text"], "company response");
            let requests = requests.lock().await;
            assert_eq!(requests.len(), expected_count);
            for (authorization, _) in requests.iter() {
                assert_eq!(
                    authorization, "Bearer company-key",
                    "never forward native OAuth or the proxy placeholder"
                );
            }
            let (_, forwarded) = requests.last().unwrap();
            assert_eq!(forwarded["model"], "company-model");
            assert_eq!(forwarded["input"], phase);
            assert_eq!(forwarded["stream"], false);
        }

        async fn refresh_route(state: &crate::store::AppState, provider: &Provider, effort: &str) {
            let mut updated = provider.clone();
            let config = provider.settings_config["config"].as_str().unwrap();
            updated.settings_config["config"] =
                json!(format!("model_reasoning_effort = \"{effort}\"\n{config}"));
            state.db.save_provider("codex", &updated).unwrap();
            crate::mode::controller::resync_route(state, &AppType::Codex)
                .await
                .unwrap();
            let live = crate::codex_config::read_codex_config_text().unwrap();
            assert!(live.contains(&format!("model_reasoning_effort = \"{effort}\"")));
        }

        fn backup_snapshot() -> Vec<(std::path::PathBuf, Vec<u8>)> {
            let dir = crate::live::engine::DeviceStore::for_device().first_write_backup_dir();
            let mut files: Vec<_> = std::fs::read_dir(dir)
                .unwrap()
                .map(|entry| {
                    let path = entry.unwrap().path();
                    let bytes = std::fs::read(&path).unwrap();
                    (path, bytes)
                })
                .collect();
            files.sort_by(|a, b| a.0.cmp(&b.0));
            files
        }
        let _home = TempHome::new();
        crate::settings::reload_settings().unwrap();
        crate::settings::update_settings(crate::settings::AppSettings {
            codex_quota_auto_fallback: true,
            preserve_codex_official_auth_on_switch: true,
            ..Default::default()
        })
        .unwrap();
        let db = Arc::new(Database::memory().unwrap());
        let mut proxy_config = db.get_proxy_config().await.unwrap();
        proxy_config.listen_port = 0;
        db.update_proxy_config(proxy_config).await.unwrap();
        let state = crate::store::AppState::new(db.clone());
        let service = &state.proxy_service;
        let config = format!("model_provider = \"company\"\nmodel = \"company-model\"\n[model_providers.company]\nname = \"Company\"\nbase_url = \"{upstream_url}\"\nwire_api = \"responses\"\nrequires_openai_auth = true\n");
        let provider = Provider::with_id(
            "company".into(),
            "Company".into(),
            json!({"auth": {"OPENAI_API_KEY": "company-key"}, "config": config}),
            None,
        );
        db.save_provider("codex", &provider).unwrap();
        db.set_current_provider("codex", &provider.id).unwrap();
        crate::settings::set_current_provider(&AppType::Codex, Some(&provider.id)).unwrap();
        let auth = json!({"auth_mode": "chatgpt", "tokens": {
            "access_token": "test-access", "refresh_token": "test-refresh", "account_id": "test-account"
        }});
        crate::codex_config::write_codex_live_atomic(&auth, Some(&config)).unwrap();
        let auth_path = crate::codex_config::get_codex_auth_path();
        let auth_bytes = std::fs::read(&auth_path).unwrap();
        crate::mode::controller::enter_with_route(
            &state,
            &AppType::Codex,
            true,
            Some(&provider.id),
        )
        .await
        .unwrap();
        assert!(crate::mode::current::mode_state(&AppType::Codex).attached);
        // The controller mirrors mode into this legacy flag; deliberately make
        // it stale to prove quota eligibility uses DeviceStore instead.
        db.set_proxy_flags_sync("codex", false, false).unwrap();
        assert!(
            !db.get_proxy_config_for_app("codex").await.unwrap().enabled,
            "legacy enabled is not route-mode truth"
        );
        let backup = backup_snapshot();
        let original = crate::codex_config::read_codex_config_text().unwrap();
        assert!(original.contains(&service.build_proxy_urls().await.unwrap().1));
        assert!(original.contains("PROXY_MANAGED"));
        assert!(original.contains("requires_openai_auth = true"));
        let identity = codex_login_quota_identity().expect("test native identity");
        let probe = |quota| {
            let identity = identity.clone();
            move || async move { Some((identity, quota)) }
        };
        // A placeholder at any other endpoint is not evidence of this listener.
        for wrong_url in ["http://127.0.0.1:1/v1", "https://relay.example/v1"] {
            let wrong = original.replace(&service.build_proxy_urls().await.unwrap().1, wrong_url);
            crate::codex_config::write_codex_live_config_atomic(Some(&wrong)).unwrap();
            assert_eq!(
                check_with_probe(&state, true, || async {
                    panic!("unverified route must not probe quota")
                })
                .await,
                Ok(None)
            );
            assert_eq!(
                crate::codex_config::read_codex_config_text().unwrap(),
                wrong
            );
        }
        crate::codex_config::write_codex_live_config_atomic(Some(&original)).unwrap();
        assert_eq!(
            check_with_probe(&state, true, probe(CodexQuotaAvailability::Exhausted)).await,
            Ok(Some(true))
        );
        assert!(has_owned_fallback());
        let changed_auth = json!({"auth_mode": "chatgpt", "tokens": {
            "access_token": "other-access", "account_id": "other-account"
        }});
        std::fs::write(&auth_path, serde_json::to_vec(&changed_auth).unwrap()).unwrap();
        assert!(!has_owned_fallback());
        std::fs::write(&auth_path, &auth_bytes).unwrap();
        assert!(has_owned_fallback());
        let depleted = crate::codex_config::read_codex_config_text().unwrap();
        assert!(depleted.contains("requires_openai_auth = false"));
        assert_company_response(service, &client, &requests, "exhausted", 1, "company-model").await;
        assert_eq!(
            check_with_probe(&state, true, probe(CodexQuotaAvailability::Unknown)).await,
            Ok(None)
        );
        assert_eq!(
            check_with_probe(&state, true, || async { None }).await,
            Ok(None)
        );
        assert_eq!(
            crate::codex_config::read_codex_config_text().unwrap(),
            depleted
        );
        refresh_route(&state, &provider, "high").await;
        assert!(crate::codex_config::read_codex_config_text()
            .unwrap()
            .contains("requires_openai_auth = false"));
        assert_eq!(backup_snapshot(), backup);
        assert_eq!(
            check_with_probe(&state, true, probe(CodexQuotaAvailability::Available)).await,
            Ok(Some(false))
        );
        assert!(!has_owned_fallback());
        assert!(crate::codex_config::read_codex_config_text()
            .unwrap()
            .contains("requires_openai_auth = true"));
        assert_eq!(std::fs::read(&auth_path).unwrap(), auth_bytes);

        assert_company_response(service, &client, &requests, "recovered", 2, "company-model").await;

        // The direct-switch login preference does not apply to takeover.
        let mut takeover_settings = crate::settings::get_settings();
        takeover_settings.preserve_codex_official_auth_on_switch = false;
        crate::settings::update_settings(takeover_settings.clone()).unwrap();
        assert_eq!(
            check_with_probe(&state, true, probe(CodexQuotaAvailability::Exhausted)).await,
            Ok(Some(true))
        );
        assert!(has_owned_fallback());
        refresh_route(&state, &provider, "low").await;
        assert!(has_owned_fallback());
        assert!(crate::codex_config::read_codex_config_text()
            .unwrap()
            .contains("requires_openai_auth = false"));
        assert_company_response(
            service,
            &client,
            &requests,
            "preserve-off-exhausted",
            3,
            "company-model",
        )
        .await;
        assert_eq!(
            check_with_probe(&state, true, probe(CodexQuotaAvailability::Available)).await,
            Ok(Some(false))
        );
        assert!(!has_owned_fallback());
        assert!(crate::codex_config::read_codex_config_text()
            .unwrap()
            .contains("requires_openai_auth = true"));
        refresh_route(&state, &provider, "high").await;
        assert!(crate::codex_config::read_codex_config_text()
            .unwrap()
            .contains("requires_openai_auth = true"));
        assert_company_response(
            service,
            &client,
            &requests,
            "preserve-off-recovered",
            4,
            "company-model",
        )
        .await;
        assert_eq!(std::fs::read(&auth_path).unwrap(), auth_bytes);
        assert_eq!(backup_snapshot(), backup);
        takeover_settings.preserve_codex_official_auth_on_switch = true;
        crate::settings::update_settings(takeover_settings).unwrap();

        assert_eq!(
            check_with_probe(&state, true, probe(CodexQuotaAvailability::Exhausted)).await,
            Ok(Some(true))
        );
        let mut settings = crate::settings::get_settings();
        settings.codex_quota_auto_fallback = false;
        crate::settings::update_settings(settings.clone()).unwrap();
        assert_eq!(
            check_with_probe(&state, false, || async {
                panic!("disabling fallback must restore without probing")
            })
            .await,
            Ok(Some(false))
        );
        assert!(!has_owned_fallback());
        settings.codex_quota_auto_fallback = true;
        crate::settings::update_settings(settings).unwrap();

        // An observation made before a live edit must not change the new snapshot.
        let stale_identity = identity.clone();
        assert_eq!(
            check_with_probe(&state, true, || async {
                let live = crate::codex_config::read_codex_config_text().unwrap();
                crate::codex_config::write_codex_live_config_atomic(Some(&format!(
                    "{live}\n# concurrent edit\n"
                )))
                .unwrap();
                Some((stale_identity, CodexQuotaAvailability::Exhausted))
            })
            .await,
            Ok(None)
        );
        assert!(!has_owned_fallback());
        assert_eq!(
            check_with_probe(&state, true, probe(CodexQuotaAvailability::Exhausted)).await,
            Ok(Some(true))
        );

        // Identical upstream TOML on another card still has independent ownership.
        let mut other = provider.clone();
        other.id = "other-company".into();
        db.save_provider("codex", &other).unwrap();
        crate::mode::controller::switch_route(&state, &AppType::Codex, &other.id)
            .await
            .unwrap();
        assert!(crate::codex_config::read_codex_config_text()
            .unwrap()
            .contains("requires_openai_auth = true"));
        assert_eq!(
            check_with_probe(&state, true, probe(CodexQuotaAvailability::Available)).await,
            Ok(None)
        );
        assert!(!has_owned_fallback());
        assert_eq!(
            check_with_probe(&state, true, probe(CodexQuotaAvailability::Exhausted)).await,
            Ok(Some(true))
        );
        let backup = backup_snapshot();
        assert!(backup
            .iter()
            .all(|(_, bytes)| !String::from_utf8_lossy(bytes).contains("PROXY_MANAGED")));
        assert!(backup
            .iter()
            .all(|(_, bytes)| !String::from_utf8_lossy(bytes)
                .contains("requires_openai_auth = false")));
        crate::mode::controller::exit(&state, &AppType::Codex)
            .await
            .unwrap();
        let restored = crate::codex_config::read_codex_config_text().unwrap();
        assert!(!restored.contains("PROXY_MANAGED"));
        assert!(restored.contains("requires_openai_auth = true"));
        assert_eq!(
            check_with_probe(&state, true, probe(CodexQuotaAvailability::Available)).await,
            Ok(None)
        );
        assert_eq!(
            crate::codex_config::read_codex_config_text().unwrap(),
            restored
        );
        assert_eq!(std::fs::read(&auth_path).unwrap(), auth_bytes);
        // Official default route with a genuinely published third-party Stack member.
        use crate::services::provider::codex_official_models::{self as official_models, testing};
        struct ResetOfficialModels;
        impl Drop for ResetOfficialModels {
            fn drop(&mut self) {
                official_models::reset_test_env();
            }
        }
        let _models = ResetOfficialModels;
        testing::Fake {
            version: None,
            responses: Arc::new(std::sync::Mutex::new(vec![])),
            bundled: Some(testing::native_models(&[("official-model", 0)])),
            keychain: Arc::new(std::sync::Mutex::new(
                crate::services::subscription::CodexKeychainLogin::Unknown,
            )),
            keychain_reads: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            now: Arc::new(std::sync::Mutex::new(1)),
            calls: Arc::new(std::sync::Mutex::new(vec![])),
            on_fetch: None,
        }
        .install();
        let mut official = Provider::with_id(
            "native-official".into(),
            "Official".into(),
            json!({"auth": {}, "config": "model = \"official-model\"\n"}),
            None,
        );
        official.category = Some("official".into());
        db.save_provider("codex", &official).unwrap();
        // Use a proper native identity without any external authentication operation.
        let native = testing::login("workspace", "quota-test");
        std::fs::write(&auth_path, serde_json::to_vec(&native).unwrap()).unwrap();
        let native_bytes = std::fs::read(&auth_path).unwrap();
        crate::mode::controller::enter_with_route(
            &state,
            &AppType::Codex,
            true,
            Some(&official.id),
        )
        .await
        .unwrap();
        crate::mode::controller::set_stack_member(&state, &AppType::Codex, &provider.id, true)
            .await
            .unwrap();
        let stack =
            crate::mode::state::stack(&crate::live::engine::DeviceStore::for_device(), "codex")
                .unwrap();
        let stack_model = crate::mode::stack::encode(
            &AppType::Codex,
            stack.key_of(&provider.id).unwrap(),
            "company-model",
            false,
        );
        let catalog: Value =
            crate::config::read_json_file(&crate::codex_config::get_codex_model_catalog_path())
                .unwrap();
        assert!(catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["slug"] == stack_model));
        let baseline = crate::codex_config::read_codex_config_text().unwrap();
        let baseline_doc: toml::Value = baseline.parse().unwrap();
        let native_identity = codex_login_quota_identity().unwrap();
        let native_probe = |quota| {
            let identity = native_identity.clone();
            move || async move { Some((identity, quota)) }
        };
        let stack_backups = backup_snapshot();
        let stash_path =
            crate::live::engine::DeviceStore::for_device().file("codex-login-stash.json");
        let stash = std::fs::read(&stash_path).ok();
        assert_eq!(
            check_with_probe(
                &state,
                true,
                native_probe(CodexQuotaAvailability::Exhausted)
            )
            .await,
            Ok(Some(true))
        );
        let exhausted: toml::Value = crate::codex_config::read_codex_config_text()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(exhausted["model_provider"], baseline_doc["model_provider"]);
        let id = exhausted["model_provider"].as_str().unwrap();
        assert_eq!(
            exhausted["model_providers"][id]["requires_openai_auth"].as_bool(),
            Some(false)
        );
        assert_eq!(
            exhausted["model_providers"][id]["experimental_bearer_token"].as_str(),
            Some("PROXY_MANAGED")
        );
        assert_company_response(
            service,
            &client,
            &requests,
            "official-stack-exhausted",
            5,
            &stack_model,
        )
        .await;
        // Placeholder auth must never upgrade to native OAuth for the official route.
        let response = client.post(format!("{}/responses", service.build_proxy_urls().await.unwrap().1))
            .bearer_auth(PROXY_TOKEN_PLACEHOLDER)
            .json(&json!({"model": "official-model", "input": "official must remain limited", "stream": false}))
            .send().await.unwrap();
        assert!(response.status().is_client_error());
        assert_eq!(requests.lock().await.len(), 5);
        crate::mode::controller::resync_route(&state, &AppType::Codex)
            .await
            .unwrap();
        assert!(
            has_owned_fallback(),
            "same-contract refresh must retain quota ownership"
        );
        assert_eq!(
            check_with_probe(
                &state,
                true,
                native_probe(CodexQuotaAvailability::Available)
            )
            .await,
            Ok(Some(false))
        );
        let recovered: toml::Value = crate::codex_config::read_codex_config_text()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(recovered, baseline_doc);
        assert_company_response(
            service,
            &client,
            &requests,
            "official-stack-recovered",
            6,
            &stack_model,
        )
        .await;
        assert_eq!(std::fs::read(&auth_path).unwrap(), native_bytes);
        assert_eq!(std::fs::read(&stash_path).ok(), stash);
        assert_eq!(backup_snapshot(), stack_backups);

        assert_eq!(
            check_with_probe(
                &state,
                true,
                native_probe(CodexQuotaAvailability::Exhausted)
            )
            .await,
            Ok(Some(true))
        );
        crate::mode::controller::set_stack_member(&state, &AppType::Codex, &provider.id, false)
            .await
            .unwrap();
        // Remove the other independently authenticated member left by the earlier phase.
        crate::mode::controller::set_stack_member(&state, &AppType::Codex, &other.id, false)
            .await
            .unwrap();
        assert!(!has_owned_fallback());
        assert!(!crate::codex_config::read_codex_config_text()
            .unwrap()
            .contains("experimental_bearer_token = \"PROXY_MANAGED\""));
        assert_eq!(
            check_with_probe(&state, true, || async {
                panic!("no usable published members")
            })
            .await,
            Ok(None)
        );
        crate::mode::controller::set_stack_member(&state, &AppType::Codex, &provider.id, true)
            .await
            .unwrap();
        assert_eq!(
            check_with_probe(
                &state,
                true,
                native_probe(CodexQuotaAvailability::Exhausted)
            )
            .await,
            Ok(Some(true))
        );
        crate::mode::controller::detach_all(&state).await;
        assert!(!crate::mode::current::mode_state(&AppType::Codex).attached);
        assert!(!has_owned_fallback());
        assert_eq!(std::fs::read(&auth_path).unwrap(), native_bytes);
        assert_eq!(
            check_with_probe(&state, true, || async { panic!("detached must not probe") }).await,
            Ok(None)
        );

        let mut settings = crate::settings::get_settings();
        settings.unify_codex_session_history = false;
        crate::settings::update_settings(settings).unwrap();
        crate::mode::controller::enter_with_route(
            &state,
            &AppType::Codex,
            true,
            Some(&official.id),
        )
        .await
        .unwrap();
        let builtin = crate::codex_config::read_codex_config_text().unwrap();
        assert_eq!(
            check_with_probe(&state, true, || async {
                panic!("builtin OpenAI needs manual mode")
            })
            .await,
            Ok(None)
        );
        assert_eq!(
            crate::codex_config::read_codex_config_text().unwrap(),
            builtin
        );
        crate::mode::controller::exit(&state, &AppType::Codex)
            .await
            .unwrap();
        upstream_handle.abort();
        assert!(upstream_handle.await.unwrap_err().is_cancelled());
        // Disabling the last takeover already stops and awaits the proxy server.
        assert!(!service.is_running().await);
        assert_eq!(requests.lock().await.len(), 6);
        crate::settings::update_settings(crate::settings::AppSettings::default()).unwrap();
    }
    async fn seed_distinct_app_proxy_configs(db: &Database) -> Vec<Value> {
        let mut configs = Vec::new();
        for (app, retries) in [("claude", 6), ("codex", 0), ("gemini", 2), ("grokbuild", 3)] {
            let mut config = db.get_proxy_config_for_app(app).await.unwrap();
            config.enabled = retries % 2 == 0;
            config.auto_failover_enabled = retries % 2 != 0;
            config.max_retries = retries;
            config.streaming_first_byte_timeout = 30 + retries;
            config.streaming_idle_timeout = 90 + retries;
            config.non_streaming_timeout = 300 + retries;
            config.circuit_failure_threshold = 5 + retries;
            configs.push(serde_json::to_value(&config).unwrap());
            db.update_proxy_config_for_app(config).await.unwrap();
        }
        configs
    }

    async fn assert_app_proxy_configs_unchanged(db: &Database, configs: &[Value]) {
        for expected in configs {
            let app = expected["appType"].as_str().unwrap();
            let actual = db.get_proxy_config_for_app(app).await.unwrap();
            assert_eq!(serde_json::to_value(actual).unwrap(), *expected, "{app}");
        }
    }

    #[tokio::test]
    async fn ephemeral_port_preserves_app_proxy_configs() {
        let db = Arc::new(Database::memory().unwrap());
        let configs = seed_distinct_app_proxy_configs(&db).await;
        let service = ProxyService::new(db.clone());
        let mut config = db.get_proxy_config().await.unwrap();
        config.listen_port = 0;
        let mut expected_global = db.get_global_proxy_config().await.unwrap();
        expected_global.listen_port = 23456;

        service
            .persist_ephemeral_listen_port_if_needed(&config, 23456)
            .await
            .unwrap();

        assert_app_proxy_configs_unchanged(&db, &configs).await;
        assert_eq!(
            serde_json::to_value(db.get_global_proxy_config().await.unwrap()).unwrap(),
            serde_json::to_value(expected_global).unwrap()
        );

        config.listen_port = 23456;
        service
            .persist_ephemeral_listen_port_if_needed(&config, 34567)
            .await
            .unwrap();
        assert_eq!(
            db.get_global_proxy_config().await.unwrap().listen_port,
            23456
        );
        assert_app_proxy_configs_unchanged(&db, &configs).await;
    }

    #[tokio::test]
    async fn update_global_config_preserves_app_proxy_configs() {
        let db = Arc::new(Database::memory().unwrap());
        let configs = seed_distinct_app_proxy_configs(&db).await;
        let service = ProxyService::new(db.clone());
        let mut global = db.get_global_proxy_config().await.unwrap();
        global.enable_logging = !global.enable_logging;
        global.listen_port = 23456;

        let restarted = service.update_global_config(&global).await.unwrap();

        assert!(!restarted, "服务没在跑，不该报重启");
        assert_app_proxy_configs_unchanged(&db, &configs).await;
        assert_eq!(
            serde_json::to_value(db.get_global_proxy_config().await.unwrap()).unwrap(),
            serde_json::to_value(global).unwrap()
        );
    }

    /// 新端口绑不上时「保存失败」要等于什么都没变：库里还是旧端口、服务还在旧端口上跑。
    /// 服务停着不管的话，下一次保存会因为「没在跑」直接假成功。
    #[tokio::test]
    async fn failed_restart_restores_previous_config_and_server() {
        let db = Arc::new(Database::memory().unwrap());
        let service = ProxyService::new(db.clone());
        let mut global = db.get_global_proxy_config().await.unwrap();
        global.listen_address = "127.0.0.1".to_string();
        global.listen_port = 0;
        db.update_global_proxy_config(global).await.unwrap();
        let running_port = service.start().await.unwrap().port;
        assert_ne!(running_port, 0);

        // 占住另一个端口，让重启时绑定失败
        let blocker = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let taken_port = blocker.local_addr().unwrap().port();
        let mut attempt = db.get_global_proxy_config().await.unwrap();
        attempt.listen_port = taken_port;

        let error = service
            .update_global_config(&attempt)
            .await
            .expect_err("binding a taken port must fail");
        assert!(error.contains("重启代理服务器失败"), "{error}");

        assert_eq!(
            db.get_global_proxy_config().await.unwrap().listen_port,
            running_port,
            "库里应回到原端口"
        );
        let status = service.get_status().await.unwrap();
        assert!(status.running, "服务应按原配置重新拉起");
        assert_eq!(status.port, running_port);

        // 再保存一次不再假成功：服务在跑，配置没变就实时应用
        let again = db.get_global_proxy_config().await.unwrap();
        assert!(!service.update_global_config(&again).await.unwrap());
        assert!(service.is_running().await);
        service.stop().await.unwrap();
        drop(blocker);
    }
}
