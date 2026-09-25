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

impl From<We2aiToolArg> for apply::RestoreTool {
    fn from(tool: We2aiToolArg) -> Self {
        match tool {
            We2aiToolArg::ClaudeCode => apply::RestoreTool::ClaudeCode,
            We2aiToolArg::Codex => apply::RestoreTool::Codex,
            We2aiToolArg::Workbuddy => apply::RestoreTool::Workbuddy,
        }
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

/// 恢复某个/某些工具的官方配置：移除 WE2AI 为其写入的一切（P6，取代功能 12
/// 的旧移除 Key 命令）。登出弹窗勾选"同时恢复工具的官方配置"时
/// 在登出成功后调用；顶栏"恢复官方"按钮直接对单个工具调用，登录态下也可用。
///
/// `still_allowed` 捕获调用发起时的会话身份（可能是 `None`，即未登录），
/// 只要执行期间身份没变就继续处理剩余工具——登出后立即调用时身份恒为
/// `None`，直到用户重新登录才会变化；登录态下调用时則在用户中途登出/切换
/// 账号时停止，语义上延续旧 `remove_tool_keys` 的"登出流程重新登录即停止"，
/// 同时覆盖登录态下调用这个更宽的场景。
#[tauri::command]
pub async fn we2ai_restore_official(
    app_handle: tauri::AppHandle,
    session: State<'_, We2aiSessionState>,
    tools: Vec<We2aiToolArg>,
) -> Result<apply::RestoreOfficialOutcome, We2aiApiError> {
    let manager = session.0.clone();
    let data_root = manager.data_root().to_path_buf();
    let secret_store = manager.secret_store();
    let identity = manager.current_identity();
    let checker = manager.clone();
    let restore_tools: Vec<apply::RestoreTool> = tools.into_iter().map(Into::into).collect();
    tauri::async_runtime::spawn_blocking(
        move || -> Result<apply::RestoreOfficialOutcome, We2aiApiError> {
            let state = app_handle
                .try_state::<AppState>()
                .ok_or_else(|| error("APP_STATE_UNAVAILABLE", "应用状态不可用"))?;
            let still_allowed = move || checker.current_identity() == identity;
            Ok(apply::restore_official(
                state.inner(),
                &data_root,
                secret_store.as_ref(),
                &restore_tools,
                &still_allowed,
            ))
        },
    )
    .await
    .map_err(|e| error("APPLY_FAILED", format!("恢复任务执行失败: {e}")))?
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

/// 恢复官方确认弹窗展示的"将移除的文件与字段"（P6，Opus 复核中危项 2）。
/// 与 [`we2ai_apply_plan`] 是两份独立的计划——那份是"将写入什么"，
/// `env.ANTHROPIC_API_KEY（删除）` 与 Codex 模型目录文件对恢复场景是反的。
#[tauri::command]
pub fn we2ai_restore_plan(tool: We2aiToolArg) -> ApplyPlan {
    apply::restore_plan_for(tool.into())
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
    let secret_store = manager.secret_store();
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
                        secret_store.as_ref(),
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
