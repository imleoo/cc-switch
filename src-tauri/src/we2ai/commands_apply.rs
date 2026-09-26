//! 工具写入与状态相关的 WE2AI 命令（方案第 4 节）。
//!
//! 前端只传工具、Key id 与模型；Key 明文由 `We2aiKeyState` 按当前会话身份
//! 取出，不经过 IPC。写入在 `spawn_blocking` 中执行：上游 `switch` 内部用
//! `futures::executor::block_on` 获取锁，不能直接在 async 上下文调用。

use serde::Deserialize;
use tauri::{Manager, State};

use super::apply::{
    self, ApplyError, ApplyOutcome, ApplyParams, ApplyPlan, ClaudeSlots, PlanFile, ProviderTool,
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
            // 统一脱敏（Opus 复核低危项 S3）：回滚/快照/外部改写等错误消息
            // 可能带着 Codex 模型目录文件的真实路径/文件名，跨 IPC 边界前
            // 在这一处统一替换成中性显示名，覆盖 apply.rs 内所有消息构造点。
            message: apply::sanitize_message_for_display(&err.message),
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

/// 只做"CC Switch 是否在运行"这一项快速检测（Codex 验收 Y1）：完整的
/// [`we2ai_tool_status`] 会连带调用 `get_tool_versions`——每次都无缓存联网
/// 查询 npm 最新版本，单次超时 `LATEST_PROBE_TIMEOUT`=15s
/// （`commands/misc.rs:804-809,996`），国内网络下可能让打开确认弹窗前的
/// 检测耗时接近 30 秒，进而让确认按钮被禁用同样长的时间。这个命令只做
/// `detect::cc_switch_running_status()`（本身已有子进程级超时），apply 前
/// 的检测只等这一个命令；顶栏完整的工具状态刷新仍然异步、独立进行，不受
/// 影响。
///
/// 返回三态而不是布尔（Codex 验收 Z2）：子进程启动失败/等待超时/非零
/// 退出（macOS `lsappinfo` 此前用 `osascript` 时完全没检查退出状态、
/// Linux `pgrep` 此前把"没有匹配"和"真正的执行错误"都当成同一种
/// "未运行"）都不是"确认过、真的没在运行"，前端需要能区分出"这次没能
/// 确认"从而展示"未能完成检测"，而不是既不警告也不提示。
#[tauri::command]
pub async fn we2ai_cc_switch_running_quick() -> detect::CcSwitchRunningStatus {
    detect::cc_switch_running_status().await
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
            let mut outcome = apply::restore_official(
                state.inner(),
                &data_root,
                secret_store.as_ref(),
                &restore_tools,
                &still_allowed,
            );
            // Codex 验收 Y3：`skipped` 里的失败原因同样可能带着路径/表名等
            // 动态内容，跨 IPC 边界前统一走同一处兜底扫描（与
            // `From<ApplyError>` 共用，不另写一套规则）。
            outcome.skipped = outcome
                .skipped
                .into_iter()
                .map(|s| apply::sanitize_message_for_display(&s))
                .collect();
            Ok(outcome)
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
            files: vec![PlanFile::identity(&workbuddy::models_path())],
            fields: workbuddy::plan_fields(),
            // WorkBuddy 由 WE2AI 自己的写入器直接维护 `models.json`（见
            // `workbuddy.rs`），不经过上游 provider 管道，没有这类上游副作用。
            extra_changes: Vec::new(),
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
    // 前端确认弹窗展示、用户已经点击确认的 `extra_changes`（`we2ai_apply_plan`
    // 返回的那份）；Claude/Codex 写入前会重新计算一次并与这份对比，不一致则
    // 拒绝（偏差修复项 B 的 L6 追加：配置在确认期间被改动时不能悄悄按旧计划
    // 写入）。前端未传（如旧版本前端或非 Claude/Codex 工具）时按空处理。
    expected_extra_changes: Option<Vec<String>>,
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
        expected_extra_changes: expected_extra_changes.unwrap_or_default(),
    };
    let data_root = manager.data_root().to_path_buf();
    let secret_store = manager.secret_store();
    let overwrite = overwrite.unwrap_or(false);
    let checker = manager.clone();

    let mut outcome =
        tauri::async_runtime::spawn_blocking(move || -> Result<ApplyOutcome, We2aiApiError> {
            let still_current = move || checker.current_identity() == Some(identity);
            match tool {
                We2aiToolArg::Workbuddy => {
                    Ok(workbuddy::apply_workbuddy(&data_root, &params, overwrite, &still_current)?)
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
                    Ok(apply::apply_provider_tool(
                        state.inner(),
                        provider_tool,
                        &params,
                        &still_current,
                        secret_store.as_ref(),
                    )?)
                }
            }
        })
        .await
        .map_err(|e| error("APPLY_FAILED", format!("写入任务执行失败: {e}")))??;
    // Codex 验收 Z5：不在 spawn_blocking 的同步闭包内用
    // `futures::executor::block_on` 桥接这个异步检测——`spawn_blocking` 的
    // `.await` 已经把执行权交回 async 上下文，这里直接 `.await` 更直接，
    // 也避免在阻塞线程池的线程上嵌套跑一个 async executor（`block_on`）。
    if detect::cc_switch_running().await {
        outcome
            .warnings
            .push("检测到另一个配置管理工具也在运行，可能与 WE2AI 互相覆盖这些工具的配置".to_string());
    }
    Ok(outcome)
}
