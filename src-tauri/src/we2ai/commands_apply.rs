//! 工具写入与状态相关的 WE2AI 命令（方案第 4 节）。
//!
//! 前端只传工具、Key id 与模型；Key 明文由 `We2aiKeyState` 按当前会话身份
//! 取出，不经过 IPC。写入在 `spawn_blocking` 中执行：上游 `switch` 内部用
//! `futures::executor::block_on` 获取锁，不能直接在 async 上下文调用。

use serde::Deserialize;
use tauri::{Manager, State};

use super::apply::{
    self, ApplyError, ApplyOutcome, ApplyParams, ApplyPlan, ClaudeSlots, ProviderTool,
};
use super::commands_auth::We2aiApiError;
use super::detect::{self, ToolStatusReport};
use super::keys::We2aiKeyState;
use super::session::We2aiSessionState;
use super::workbuddy;
use crate::store::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum We2aiToolArg {
    ClaudeCode,
    Codex,
    Workbuddy,
}

impl From<ApplyError> for We2aiApiError {
    fn from(err: ApplyError) -> Self {
        We2aiApiError {
            code: err.code.to_string(),
            message: err.message,
        }
    }
}

fn error(code: &str, message: impl Into<String>) -> We2aiApiError {
    We2aiApiError {
        code: code.to_string(),
        message: message.into(),
    }
}

#[tauri::command]
pub async fn we2ai_tool_status(
    session: State<'_, We2aiSessionState>,
) -> Result<ToolStatusReport, We2aiApiError> {
    let manager = session.0.clone();
    let region = manager.current_identity().map(|i| i.region);
    Ok(detect::tool_status(region, manager.data_root().to_path_buf()).await)
}

/// 登出弹窗勾选"同时从工具配置中移除 Key"时，登出成功后调用（方案 5.2）。
#[tauri::command]
pub async fn we2ai_remove_tool_keys(
    session: State<'_, We2aiSessionState>,
) -> Result<apply::RemoveToolKeysOutcome, We2aiApiError> {
    let manager = session.0.clone();
    let data_root = manager.data_root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        let still_logged_out = move || manager.current_identity().is_none();
        apply::remove_tool_keys(&data_root, &still_logged_out).map_err(We2aiApiError::from)
    })
    .await
    .map_err(|e| error("APPLY_FAILED", format!("移除任务执行失败: {e}")))?
}

#[tauri::command]
pub fn we2ai_apply_plan(tool: We2aiToolArg) -> ApplyPlan {
    match tool {
        We2aiToolArg::ClaudeCode => apply::plan_for(ProviderTool::ClaudeCode),
        We2aiToolArg::Codex => apply::plan_for(ProviderTool::Codex),
        We2aiToolArg::Workbuddy => ApplyPlan {
            files: vec![workbuddy::models_path().display().to_string()],
            fields: workbuddy::plan_fields(),
        },
    }
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn we2ai_apply_model(
    app_handle: tauri::AppHandle,
    session: State<'_, We2aiSessionState>,
    keys: State<'_, We2aiKeyState>,
    tool: We2aiToolArg,
    key_id: i64,
    model: String,
    claude_slots: Option<ClaudeSlots>,
    overwrite: Option<bool>,
) -> Result<ApplyOutcome, We2aiApiError> {
    let model = model.trim().to_string();
    if model.is_empty() {
        return Err(error("INVALID_MODEL", "模型不能为空"));
    }
    let manager = session.0.clone();
    let identity = manager
        .current_identity()
        .ok_or_else(|| error("NO_ACTIVE_SESSION", "请先登录"))?;
    let api_key = keys.secret_for(Some(identity), key_id).ok_or_else(|| {
        error(
            "KEY_NOT_FOUND",
            "该 Key 不在当前账号的 Key 列表中，请刷新列表",
        )
    })?;
    let params = ApplyParams {
        model: model.clone(),
        claude_slots: claude_slots.unwrap_or_default(),
        api_key,
        gateway_root: identity.region.base_url().to_string(),
        capabilities: keys.capabilities_for(Some(identity), key_id, &model),
    };
    let data_root = manager.data_root().to_path_buf();
    let overwrite = overwrite.unwrap_or(false);
    let checker = manager.clone();

    let outcome =
        tauri::async_runtime::spawn_blocking(move || -> Result<ApplyOutcome, We2aiApiError> {
            let still_current = move || checker.current_identity() == Some(identity);
            let mut outcome = match tool {
                We2aiToolArg::Workbuddy => {
                    workbuddy::apply_workbuddy(&data_root, &params, overwrite, &still_current)?
                }
                We2aiToolArg::ClaudeCode | We2aiToolArg::Codex => {
                    let state = app_handle
                        .try_state::<AppState>()
                        .ok_or_else(|| error("APP_STATE_UNAVAILABLE", "应用状态不可用"))?;
                    let provider_tool = if tool == We2aiToolArg::ClaudeCode {
                        ProviderTool::ClaudeCode
                    } else {
                        ProviderTool::Codex
                    };
                    apply::apply_provider_tool(
                        state.inner(),
                        provider_tool,
                        &params,
                        &still_current,
                    )?
                }
            };
            if detect::cc_switch_running() {
                outcome
                    .warnings
                    .push("CC Switch 也在管理这些工具，可能互相覆盖".to_string());
            }
            Ok(outcome)
        })
        .await
        .map_err(|e| error("APPLY_FAILED", format!("写入任务执行失败: {e}")))??;
    Ok(outcome)
}
