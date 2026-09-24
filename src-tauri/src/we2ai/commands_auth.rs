//! 登录会话相关的 WE2AI 自有 Tauri 命令（方案第 5.1、5.2 节）。
//!
//! 前端只传 email/password/phone/code/totp_code 等业务字段，从不接触验证码
//! 票据——需要验证码时由这里驱动 `captcha.rs` 打开窗口、拿到票据后直接拼进
//! 登录 / 发短信请求，票据本身不经过 IPC 返回给前端。

use serde::Serialize;
use tauri::State;

use super::api::ApiClient;
use super::captcha::{self, We2aiCaptchaState};
use super::region::Region;
use super::session::{
    LoginOutcome, LogoutOutcome, SessionError, SessionManager, SessionSummary, We2aiSessionState,
};

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 统一的错误负载：`code` 供前端做错误码中文化与分支判断（如
/// `BACKEND_MODE_ACTIVE` 在手机登录页提示"请使用邮箱登录"），`message` 是
/// 兜底展示文案。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct We2aiApiError {
    pub code: String,
    pub message: String,
}

impl From<SessionError> for We2aiApiError {
    fn from(err: SessionError) -> Self {
        let code = match &err {
            SessionError::Terminated(code) => code.clone(),
            SessionError::Transient(_) => "TRANSIENT".to_string(),
            SessionError::NeedsRelogin => "NEEDS_RELOGIN".to_string(),
            SessionError::NoActiveSession => "NO_ACTIVE_SESSION".to_string(),
            SessionError::SessionChanged => "SESSION_CHANGED".to_string(),
            SessionError::PersistFailed(_) => "SESSION_PERSIST_FAILED".to_string(),
            SessionError::Other(code) => code.clone(),
        };
        We2aiApiError {
            code,
            message: err.to_string(),
        }
    }
}

impl From<captcha::CaptchaFlowError> for We2aiApiError {
    fn from(err: captcha::CaptchaFlowError) -> Self {
        let code = match &err {
            captcha::CaptchaFlowError::WindowClosed => "CAPTCHA_WINDOW_CLOSED",
            captcha::CaptchaFlowError::Timeout => "CAPTCHA_TIMEOUT",
            captcha::CaptchaFlowError::WindowCreateFailed(_) => "CAPTCHA_WINDOW_FAILED",
        };
        We2aiApiError {
            code: code.to_string(),
            message: err.to_string(),
        }
    }
}

fn parse_region(region: &str) -> Result<Region, We2aiApiError> {
    Region::from_storage_key(region)
        .filter(|r| r.is_available())
        .ok_or_else(|| We2aiApiError {
            code: "INVALID_REGION".to_string(),
            message: format!("未知或当前构建不可用的区域: {region}"),
        })
}

/// 登录 / 手机登录 / 发短信前先查公开设置，决定是否需要弹验证码窗口。
async fn maybe_get_captcha_ticket(
    app: &tauri::AppHandle,
    captcha_state: &State<'_, We2aiCaptchaState>,
    region: Region,
) -> Result<Option<super::api::CaptchaTicket>, We2aiApiError> {
    let api = ApiClient::new(region, APP_VERSION);
    let settings = api.get_public_settings().await.map_err(|e| We2aiApiError {
        code: "NETWORK_ERROR".to_string(),
        message: e.to_string(),
    })?;
    let Some(provider) = settings.active_provider() else {
        return Ok(None);
    };
    let registry = captcha_state.0.clone();
    let ticket = captcha::open_captcha_window(app.clone(), registry, region, provider).await?;
    Ok(Some(ticket))
}

/// 公开设置视图（方案 3.3 节"公开设置"，取验证码开关）。字段名与 SubPanel
/// `dto.PublicSettings` 对齐但只暴露前端需要的子集。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct We2aiPublicSettings {
    pub captcha_required: bool,
    pub backend_mode_enabled: bool,
}

#[tauri::command]
pub async fn we2ai_get_public_settings(
    region: String,
) -> Result<We2aiPublicSettings, We2aiApiError> {
    let region = parse_region(&region)?;
    let api = ApiClient::new(region, APP_VERSION);
    let settings = api.get_public_settings().await.map_err(|e| We2aiApiError {
        code: "NETWORK_ERROR".to_string(),
        message: e.to_string(),
    })?;
    Ok(We2aiPublicSettings {
        captcha_required: settings.active_provider().is_some(),
        backend_mode_enabled: settings.backend_mode_enabled,
    })
}

#[tauri::command]
pub fn we2ai_available_regions() -> Vec<String> {
    Region::available_regions()
        .into_iter()
        .map(|r| r.storage_key().to_string())
        .collect()
}

#[tauri::command]
pub fn we2ai_get_last_region(session: State<'_, We2aiSessionState>) -> Option<String> {
    // 调试构建保存的 `domestic_dev` 在正式构建中不可用，按没有记忆处理，
    // 避免启动恢复直接报 INVALID_REGION（Fable 终验低危项）。
    session
        .0
        .last_region()
        .filter(|r| r.is_available())
        .map(|r| r.storage_key().to_string())
}

#[tauri::command]
pub fn we2ai_set_last_region(
    region: String,
    session: State<'_, We2aiSessionState>,
) -> Result<(), We2aiApiError> {
    let region = parse_region(&region)?;
    session
        .0
        .set_last_region(region)
        .map_err(|message| We2aiApiError {
            code: "LAST_REGION_SAVE_FAILED".to_string(),
            message,
        })
}

/// 应用启动时尝试静默恢复会话。返回 `true` 表示已登录（前端随后应调用
/// `we2ai_session_status` 获取展示信息。三种结果都不是错误（`Result` 的
/// `Err` 只用于区域参数不合法这类调用层面的问题）：
/// - `restored`：刷新成功，会话可用；
/// - `needLogin`：没有可恢复的会话，应回登录页；
/// - `offlineRetained`：恢复出的会话在刷新阶段遇到网络错误，内存状态原样
///   保留，前端应保持"已登录"界面并提示离线（方案第 5.2 节"断网不回登录页"
///   同样适用于启动恢复，Codex 代码评审高危项 5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum We2aiResumeOutcome {
    Restored,
    NeedLogin,
    OfflineRetained,
}

impl From<super::session::ResumeOutcome> for We2aiResumeOutcome {
    fn from(outcome: super::session::ResumeOutcome) -> Self {
        match outcome {
            super::session::ResumeOutcome::Restored => We2aiResumeOutcome::Restored,
            super::session::ResumeOutcome::NeedLogin => We2aiResumeOutcome::NeedLogin,
            super::session::ResumeOutcome::OfflineRetained => We2aiResumeOutcome::OfflineRetained,
        }
    }
}

#[tauri::command]
pub async fn we2ai_resume_session(
    region: String,
    session: State<'_, We2aiSessionState>,
) -> Result<We2aiResumeOutcome, We2aiApiError> {
    let region = parse_region(&region)?;
    let manager = session.0.clone();
    Ok(manager.resume_session(region).await.into())
}

/// 立即重试一次断网自动退避（窗口获得焦点 / 浏览器 `online` 事件 / 用户点击
/// "重试"按钮均调用这个命令），不等后台指数退避循环的下一次定时唤醒
/// （Codex 代码评审中危项 3）。不在离线重试状态时是无操作，直接回报当前
/// 会话状态。
#[tauri::command]
pub async fn we2ai_retry_now(
    session: State<'_, We2aiSessionState>,
) -> Result<We2aiResumeOutcome, We2aiApiError> {
    let manager = session.0.clone();
    Ok(manager.retry_now().await.into())
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum We2aiLoginOutcome {
    LoggedIn,
    // `rename_all` 在容器（枚举）级别只改写 variant 名，不会连带改写结构体
    // variant 内部字段名（已用 `login_outcome_serde_shape` 单测验证过这个
    // 容易踩坑的细节），因此这里显式给每个字段加 `rename`。
    Requires2fa {
        #[serde(rename = "tempToken")]
        temp_token: String,
        #[serde(rename = "emailMasked")]
        email_masked: String,
    },
}

impl From<LoginOutcome> for We2aiLoginOutcome {
    fn from(outcome: LoginOutcome) -> Self {
        match outcome {
            LoginOutcome::LoggedIn => We2aiLoginOutcome::LoggedIn,
            LoginOutcome::Requires2fa {
                temp_token,
                email_masked,
            } => We2aiLoginOutcome::Requires2fa {
                temp_token,
                email_masked,
            },
        }
    }
}

#[tauri::command]
pub async fn we2ai_login_email(
    app: tauri::AppHandle,
    region: String,
    email: String,
    password: String,
    session: State<'_, We2aiSessionState>,
    captcha_state: State<'_, We2aiCaptchaState>,
) -> Result<We2aiLoginOutcome, We2aiApiError> {
    let region = parse_region(&region)?;
    let ticket = maybe_get_captcha_ticket(&app, &captcha_state, region).await?;
    let manager = session.0.clone();
    let outcome = manager
        .login_email(region, &email, &password, ticket.as_ref())
        .await?;
    Ok(outcome.into())
}

#[tauri::command]
pub async fn we2ai_login_2fa(
    region: String,
    temp_token: String,
    totp_code: String,
    session: State<'_, We2aiSessionState>,
) -> Result<(), We2aiApiError> {
    let region = parse_region(&region)?;
    let manager = session.0.clone();
    manager.login_2fa(region, &temp_token, &totp_code).await?;
    Ok(())
}

#[tauri::command]
pub async fn we2ai_send_sms_code(
    app: tauri::AppHandle,
    region: String,
    phone: String,
    session: State<'_, We2aiSessionState>,
    captcha_state: State<'_, We2aiCaptchaState>,
) -> Result<(), We2aiApiError> {
    let region = parse_region(&region)?;
    let ticket = maybe_get_captcha_ticket(&app, &captcha_state, region).await?;
    let manager = session.0.clone();
    manager
        .send_sms_code(region, &phone, ticket.as_ref())
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn we2ai_login_phone(
    app: tauri::AppHandle,
    region: String,
    phone: String,
    code: String,
    session: State<'_, We2aiSessionState>,
    captcha_state: State<'_, We2aiCaptchaState>,
) -> Result<(), We2aiApiError> {
    let region = parse_region(&region)?;
    let ticket = maybe_get_captcha_ticket(&app, &captcha_state, region).await?;
    let manager = session.0.clone();
    manager
        .login_phone(region, &phone, &code, ticket.as_ref())
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn we2ai_session_status(
    app_handle: tauri::AppHandle,
    session: State<'_, We2aiSessionState>,
    keys: State<'_, super::keys::We2aiKeyState>,
) -> Result<SessionSummary, We2aiApiError> {
    let mut summary = session.0.summary();
    // 数据库里的 Key 未清除同样属于"本机凭据未清除"，复用同一提示与重试入口；
    // 持久标记保证重启后仍能提示（Codex P4 验收第 2 轮高危项）。
    summary.local_cleanup_pending |= keys.material_cleanup_pending()
        || super::apply::material_cleanup_pending(session.0.data_root());
    // 标记写不进磁盘时，未登录状态下直接检查残留（第 3 轮高危项 2）。
    if !summary.logged_in && !summary.local_cleanup_pending {
        summary.local_cleanup_pending = key_material_residue(app_handle).await;
    }
    Ok(summary)
}

async fn key_material_residue(app_handle: tauri::AppHandle) -> bool {
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        app_handle
            .try_state::<crate::store::AppState>()
            .map(|state| super::apply::key_material_residue(state.inner()))
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum We2aiLogoutOutcome {
    Revoked,
    LocalOnly,
    /// 远端未确认撤销，且本地清理（索引 + 钥匙串）两项全部失败——前端不能
    /// 提示"已退出"，必须提示清理失败并提供重试（Codex 代码评审第 5 轮
    /// 高危项 3）。
    LocalCleanupFailed,
    NotLoggedIn,
}

impl From<LogoutOutcome> for We2aiLogoutOutcome {
    fn from(outcome: LogoutOutcome) -> Self {
        match outcome {
            LogoutOutcome::Revoked => We2aiLogoutOutcome::Revoked,
            LogoutOutcome::LocalOnly => We2aiLogoutOutcome::LocalOnly,
            LogoutOutcome::LocalCleanupFailed => We2aiLogoutOutcome::LocalCleanupFailed,
            LogoutOutcome::NotLoggedIn => We2aiLogoutOutcome::NotLoggedIn,
        }
    }
}

#[tauri::command]
pub async fn we2ai_logout(
    app_handle: tauri::AppHandle,
    session: State<'_, We2aiSessionState>,
    keys: State<'_, super::keys::We2aiKeyState>,
) -> Result<We2aiLogoutOutcome, We2aiApiError> {
    let manager = session.0.clone();
    let outcome: We2aiLogoutOutcome = manager.logout().await.into();
    // 登出后不在内存里继续保留明文 Key，并清除数据库行与备份里的 Key。
    keys.clear();
    let material_ok = clear_key_material(app_handle, manager.clone()).await;
    record_material_pending(&keys, manager.data_root(), !material_ok);
    Ok(logout_outcome_with_material(outcome, material_ok))
}

/// 内存标记与持久标记一起更新；持久标记写失败时内存标记仍保证本次运行提示。
fn record_material_pending(
    keys: &super::keys::We2aiKeyState,
    data_root: &std::path::Path,
    pending: bool,
) {
    keys.set_material_cleanup_pending(pending);
    if let Err(e) = super::apply::set_material_cleanup_pending(data_root, pending) {
        log::warn!("更新 Key 材料待清理标记失败: {e}");
    }
}

async fn clear_key_material(app_handle: tauri::AppHandle, manager: SessionManager) -> bool {
    let result = tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        let still_logged_out = move || manager.current_identity().is_none();
        match app_handle.try_state::<crate::store::AppState>() {
            Some(state) => super::apply::clear_local_key_material(state.inner(), &still_logged_out),
            None => Err("应用状态不可用".to_string()),
        }
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r);
    if let Err(e) = &result {
        log::warn!("清除数据库中的 WE2AI Key 失败: {e}");
    }
    result.is_ok()
}

/// 数据库行或备份里的 Key 没清掉时不能报"已退出"：前端提示清理失败并给出重试
/// 入口（Codex P4 验收第 1 轮高危项）。
fn logout_outcome_with_material(
    outcome: We2aiLogoutOutcome,
    material_ok: bool,
) -> We2aiLogoutOutcome {
    match outcome {
        We2aiLogoutOutcome::Revoked | We2aiLogoutOutcome::LocalOnly if !material_ok => {
            We2aiLogoutOutcome::LocalCleanupFailed
        }
        other => other,
    }
}

/// 重试结果：会话凭据与 Key 材料两部分都清干净才算完成。`material` 为 `None`
/// 表示没有待清理的 Key 材料、未尝试。
fn retry_outcome_with_material(
    session_outcome: We2aiLogoutOutcome,
    material: Option<bool>,
) -> We2aiLogoutOutcome {
    match (session_outcome, material) {
        (_, Some(false)) => We2aiLogoutOutcome::LocalCleanupFailed,
        (We2aiLogoutOutcome::NotLoggedIn, Some(true)) => We2aiLogoutOutcome::LocalOnly,
        (other, _) => other,
    }
}

/// 登出或会话终止时远端未确认、且本地清理（索引 + 钥匙串）两项都失败，
/// 会话置为 `LoggedOut` 并把 `region → user_id` 登记到待清理集合
/// （`SessionManager` 的 `pending_cleanups`）。前端"重试"按钮必须调用这个专门的命令重试本地
/// 清理，而不是再次调用 `we2ai_logout`（那样会尝试重新发起一次远端登出请
/// 求，但此时已经没有可用的 refresh token 了）（Codex 代码评审第 6 轮高危
/// 项 3）。
#[tauri::command]
pub async fn we2ai_retry_local_cleanup(
    app_handle: tauri::AppHandle,
    session: State<'_, We2aiSessionState>,
    keys: State<'_, super::keys::We2aiKeyState>,
) -> Result<We2aiLogoutOutcome, We2aiApiError> {
    let manager = session.0.clone();
    let session_outcome: We2aiLogoutOutcome = manager.retry_local_cleanup().await.into();
    let pending = keys.material_cleanup_pending()
        || super::apply::material_cleanup_pending(manager.data_root())
        || (manager.current_identity().is_none() && key_material_residue(app_handle.clone()).await);
    let material = if pending {
        let ok = clear_key_material(app_handle, manager.clone()).await;
        record_material_pending(&keys, manager.data_root(), !ok);
        Some(ok)
    } else {
        None
    };
    Ok(retry_outcome_with_material(session_outcome, material))
}

#[cfg(test)]
mod serde_shape_tests {
    use super::super::session::SessionSummary;
    use super::*;

    #[test]
    fn login_outcome_serde_shape() {
        let logged_in = serde_json::to_value(We2aiLoginOutcome::LoggedIn).unwrap();
        assert_eq!(logged_in, serde_json::json!({"kind": "loggedIn"}));
        let requires = serde_json::to_value(We2aiLoginOutcome::Requires2fa {
            temp_token: "t".into(),
            email_masked: "e".into(),
        })
        .unwrap();
        assert_eq!(
            requires,
            serde_json::json!({"kind": "requires2fa", "tempToken": "t", "emailMasked": "e"})
        );
    }

    #[test]
    fn key_material_failure_turns_a_successful_logout_into_cleanup_failed() {
        use We2aiLogoutOutcome::*;
        assert_eq!(
            logout_outcome_with_material(Revoked, false),
            LocalCleanupFailed
        );
        assert_eq!(
            logout_outcome_with_material(LocalOnly, false),
            LocalCleanupFailed
        );
        assert_eq!(logout_outcome_with_material(Revoked, true), Revoked);
        assert_eq!(
            logout_outcome_with_material(NotLoggedIn, false),
            NotLoggedIn
        );
        assert_eq!(
            logout_outcome_with_material(LocalCleanupFailed, true),
            LocalCleanupFailed
        );
    }

    #[test]
    fn retry_succeeds_only_when_both_parts_are_clean() {
        use We2aiLogoutOutcome::*;
        assert_eq!(
            retry_outcome_with_material(NotLoggedIn, Some(true)),
            LocalOnly
        );
        assert_eq!(
            retry_outcome_with_material(LocalOnly, Some(false)),
            LocalCleanupFailed
        );
        assert_eq!(
            retry_outcome_with_material(LocalCleanupFailed, Some(true)),
            LocalCleanupFailed
        );
        assert_eq!(retry_outcome_with_material(NotLoggedIn, None), NotLoggedIn);
        assert_eq!(retry_outcome_with_material(LocalOnly, None), LocalOnly);
    }

    #[test]
    fn logout_outcome_serde_shape() {
        assert_eq!(
            serde_json::to_value(We2aiLogoutOutcome::Revoked).unwrap(),
            serde_json::json!("revoked")
        );
        assert_eq!(
            serde_json::to_value(We2aiLogoutOutcome::LocalOnly).unwrap(),
            serde_json::json!("localOnly")
        );
    }

    #[test]
    fn session_summary_serde_shape() {
        let summary = SessionSummary {
            logged_in: true,
            region: Some("international".to_string()),
            email_masked: Some("a****@b.com".to_string()),
            keyring_degraded: false,
            index_degraded: false,
            offline_retry_in_seconds: Some(4),
            local_cleanup_pending: false,
        };
        let value = serde_json::to_value(summary).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "loggedIn": true,
                "region": "international",
                "emailMasked": "a****@b.com",
                "keyringDegraded": false,
                "indexDegraded": false,
                "offlineRetryInSeconds": 4,
                "localCleanupPending": false
            })
        );
    }
}
