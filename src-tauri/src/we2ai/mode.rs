//! WE2AI 模式开关：数据根目录、启动/退出白名单、IPC 默认拒绝白名单。
//!
//! `enabled()` 当前恒为 `true`——本 fork 编译产物即为 WE2AI 客户端，不提供运行时
//! 切换回 CC Switch 的开关。之所以仍以“模式判断”的形式实现（而不是直接删除
//! 上游分支），是为了让每一处差异都收敛到可单测的纯函数上，方便上游同步时
//! 用 `cargo test we2ai` 快速核对行为未被静默还原（见方案第 7 节）。

use std::path::PathBuf;

/// WE2AI 模式是否启用。恒为 `true`：本 fork 的产物只作为 WE2AI 客户端分发。
pub const fn enabled() -> bool {
    true
}

/// WE2AI 数据根目录：`~/.we2ai`。
///
/// 对应方案第 6.1 节的 4 个写死 `~/.cc-switch` 的位置，全部改为调用本函数。
/// 使用 `crate::config::get_home_dir()` 而非 `dirs::home_dir()`，与上游对
/// Windows `HOME` 环境变量的既有规避保持一致。
pub fn data_root() -> PathBuf {
    crate::config::get_home_dir().join(".we2ai")
}

/// 启动/退出阶段可能被 WE2AI 模式禁用的任务。
///
/// 每个变体对应自定义开发功能列表.md 登记的一个调用点，新增调用点时在此追加
/// 变体并在 `startup_allowed` 中登记为 `false`，同时更新功能列表。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StartupTask {
    /// 首次运行导入现有工具（Claude/Codex/Gemini/...）live 配置为默认供应商。
    FirstRunImportToolProviders,
    /// 官方供应商种子 `init_default_official_providers()`。
    SeedOfficialProviders,
    /// 累加模式应用（OpenCode/OpenClaw/Hermes/Pi）每次启动从 live 同步供应商，
    /// 以及表空时的 OMO / OMO Slim 本地导入（同属“自动从本地文件建供应商”）。
    AdditiveProviderImport,
    /// 默认 Skills 仓库初始化。
    DefaultSkillsInit,
    /// 表为空时从各工具 live 配置导入 MCP 服务器。
    ImportMcpOnEmptyTable,
    /// 表为空时从各工具 live 配置导入提示词。
    ImportPromptsOnEmptyTable,
    /// 启动时按 settings 表恢复本地代理接管状态。
    ProxyStateRestoreOnStartup,
    /// 启动时检测到上次异常退出的接管残留并恢复。
    CrashRecovery,
    /// 退出时保留代理状态并恢复 live 配置。
    ExitLiveRestore,
    /// WebDAV / S3 自动同步 worker。
    WebdavS3Sync,
    /// 会话用量日志同步与轮询（含启动首轮费用回填）。
    SessionUsageSync,
    /// 从用户 live 配置抽取公共配置片段，以及清理泄漏进 Gemini 公共片段的凭据。
    CommonConfigSnippets,
    /// Skills 统一管理迁移（`skills_ssot_migration_pending` 触发）：扫描各工具
    /// 的 Skills 目录（`~/.claude/skills`、`~/.codex/skills` 等）并复制进 SSOT。
    SkillsSsotMigration,
    /// Codex 历史会话迁移三件套：第三方 provider 历史桶迁移、provider 模板桶
    /// 迁移、统一会话开关的官方历史迁移，均会改写共享的 Codex 会话 jsonl 与
    /// state DB 文件。
    CodexHistoryMigration,
}

/// 给定任务在 WE2AI 模式下是否允许执行。
///
/// WE2AI 模式下上表全部任务均被禁用（返回 `false`），避免污染 `~/.we2ai`
/// 数据库、意外接管或恢复用户工具的 live 配置。
pub fn startup_allowed(task: StartupTask) -> bool {
    if !enabled() {
        return true;
    }
    match task {
        StartupTask::FirstRunImportToolProviders
        | StartupTask::SeedOfficialProviders
        | StartupTask::AdditiveProviderImport
        | StartupTask::DefaultSkillsInit
        | StartupTask::ImportMcpOnEmptyTable
        | StartupTask::ImportPromptsOnEmptyTable
        | StartupTask::ProxyStateRestoreOnStartup
        | StartupTask::CrashRecovery
        | StartupTask::ExitLiveRestore
        | StartupTask::WebdavS3Sync
        | StartupTask::SessionUsageSync
        | StartupTask::CommonConfigSnippets
        | StartupTask::SkillsSsotMigration
        | StartupTask::CodexHistoryMigration => false,
    }
}

/// IPC 默认拒绝白名单：WE2AI 模式下上游命令里唯一允许直接调用的集合。
///
/// 与自定义开发功能列表.md 及方案第 6.2 节的“IPC 白名单”表一一对应。
/// `we2ai_*` 命令不经过此表，由 [`gate`] 单独识别后交给 WE2AI 自己的
/// handler。
const UPSTREAM_COMMAND_WHITELIST: &[&str] = &[
    // 上游启动必需
    "get_init_error",
    "get_migration_result",
    "get_skills_migration_result",
    "set_window_theme",
    // 上游功能
    "get_tool_versions",
    "check_for_updates",
    "install_update_and_restart",
    "restart_app",
    "open_external",
    "get_auto_launch_status",
    "set_auto_launch",
];

/// 命令是否属于 WE2AI 自有命令（`we2ai_` 前缀）。
fn is_we2ai_command(command: &str) -> bool {
    command.starts_with("we2ai_")
}

/// 命令是否在上游白名单内。纯函数，便于单测覆盖“白名单外一律拒绝”。
fn is_upstream_whitelisted(command: &str) -> bool {
    UPSTREAM_COMMAND_WHITELIST.contains(&command)
}

/// 构造 WE2AI 模式下的 IPC 分发器：`we2ai_*` 交给 `we2ai_handler`，
/// 上游白名单命令交给 `upstream_handler`，其余一律 `reject` 并返回 `true`
/// （避免框架在 `false` 返回后再报一次 command not found）。
///
/// 用法：
/// ```ignore
/// .invoke_handler(we2ai::mode::gate(
///     tauri::generate_handler![we2ai::commands::we2ai_get_settings, ...],
///     tauri::generate_handler![ /* 上游命令列表，原样不动 */ ],
/// ))
/// ```
pub fn gate<R, WE, UP>(
    we2ai_handler: WE,
    upstream_handler: UP,
) -> impl Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static
where
    R: tauri::Runtime,
    WE: Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static,
    UP: Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static,
{
    move |invoke: tauri::ipc::Invoke<R>| {
        let command = invoke.message.command().to_string();
        if is_we2ai_command(&command) {
            we2ai_handler(invoke)
        } else if is_upstream_whitelisted(&command) {
            upstream_handler(invoke)
        } else {
            log::warn!("[WE2AI] IPC 命令 {command} 不在白名单内，已拒绝");
            invoke
                .resolver
                .reject(format!("命令 `{command}` 在 WE2AI 模式下不可用"));
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_root_ends_with_we2ai_dir() {
        let root = data_root();
        assert!(root.ends_with(".we2ai"), "got {}", root.display());
    }

    #[test]
    fn data_root_ignores_app_config_dir_override() {
        // 负例：`get_app_config_dir()` 是 WE2AI 模式下真正被全局调用的函数。
        // 预置一个指向 `~/.cc-switch` 的 Store override 后，它仍必须返回
        // `data_root()`（`<home>/.we2ai`），因为 WE2AI 分支排在 override 判断
        // 之前（`config.rs::get_app_config_dir()`）。
        let home = crate::config::get_home_dir();
        let fake_override = home.join(".cc-switch-override-for-test");
        crate::app_store::set_app_config_dir_override_for_test(Some(fake_override.clone()));

        let result = crate::config::get_app_config_dir();

        // 无论测试结果如何都要恢复全局缓存，避免影响其他测试（虽然 enabled()==true
        // 时这条 override 路径本就不可达，恢复仍是良好习惯，防止未来行为变化后遗留状态）。
        crate::app_store::set_app_config_dir_override_for_test(None);

        assert_eq!(result, home.join(".we2ai"));
        assert_ne!(result, fake_override);
        assert_eq!(data_root(), home.join(".we2ai"));
    }

    #[test]
    fn all_startup_tasks_are_disabled_in_we2ai_mode() {
        let all_tasks = [
            StartupTask::FirstRunImportToolProviders,
            StartupTask::SeedOfficialProviders,
            StartupTask::AdditiveProviderImport,
            StartupTask::DefaultSkillsInit,
            StartupTask::ImportMcpOnEmptyTable,
            StartupTask::ImportPromptsOnEmptyTable,
            StartupTask::ProxyStateRestoreOnStartup,
            StartupTask::CrashRecovery,
            StartupTask::ExitLiveRestore,
            StartupTask::WebdavS3Sync,
            StartupTask::SessionUsageSync,
            StartupTask::CommonConfigSnippets,
            StartupTask::SkillsSsotMigration,
            StartupTask::CodexHistoryMigration,
        ];
        for task in all_tasks {
            assert!(
                !startup_allowed(task),
                "expected {task:?} to be disabled in WE2AI mode"
            );
        }
    }

    #[test]
    fn we2ai_commands_are_recognized_by_prefix() {
        assert!(is_we2ai_command("we2ai_get_settings"));
        assert!(is_we2ai_command("we2ai_apply_model"));
        assert!(!is_we2ai_command("get_settings"));
    }

    #[test]
    fn upstream_whitelist_accepts_only_listed_commands() {
        for cmd in UPSTREAM_COMMAND_WHITELIST {
            assert!(is_upstream_whitelisted(cmd), "expected {cmd} to be allowed");
        }
    }

    #[test]
    fn upstream_whitelist_rejects_everything_else() {
        let rejected = [
            "get_settings",
            "save_settings",
            "add_provider",
            "update_provider",
            "delete_provider",
            "switch_provider",
            "import_config_from_file",
            "restore_db_backup",
            "get_providers",
            "get_mcp_servers",
            "get_prompts",
            "parse_deeplink",
            "import_from_deeplink_unified",
        ];
        for cmd in rejected {
            assert!(
                !is_upstream_whitelisted(cmd),
                "expected {cmd} to be rejected by the upstream whitelist"
            );
        }
    }

    /// `gate()` 本身的分发测试：不只测底层判定纯函数，而是用 `tauri::test`
    /// 的 `MockRuntime` 真正构造一个 `.invoke_handler(gate(...))` 的 App，
    /// 逐一发起三种命令，核对 IPC 分发结果——覆盖 we2ai_* 命令交给 we2ai
    /// handler、白名单内命令交给上游 handler、白名单外命令被 reject 三条路径。
    #[test]
    fn gate_dispatches_ipc_commands_to_the_correct_handler() {
        use tauri::ipc::{CallbackFn, InvokeBody};
        use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, INVOKE_KEY};
        use tauri::webview::InvokeRequest;
        use tauri::WebviewWindowBuilder;

        #[tauri::command]
        fn we2ai_probe() -> &'static str {
            "handled-by-we2ai"
        }

        // 命名为一个真实的上游白名单命令：gate() 只按命令名字符串路由，不关心
        // 函数体，用同名的测试替身验证"白名单内命令交给 upstream_handler"。
        #[tauri::command]
        fn get_init_error() -> &'static str {
            "handled-by-upstream"
        }

        let app = mock_builder()
            .invoke_handler(gate(
                tauri::generate_handler![we2ai_probe],
                tauri::generate_handler![get_init_error],
            ))
            .build(mock_context(noop_assets()))
            .expect("build mock app");
        let webview = WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("build mock webview window");

        let invoke = |cmd: &str| {
            get_ipc_response(
                &webview,
                InvokeRequest {
                    cmd: cmd.into(),
                    callback: CallbackFn(0),
                    error: CallbackFn(1),
                    url: "http://tauri.localhost".parse().unwrap(),
                    body: InvokeBody::default(),
                    headers: Default::default(),
                    invoke_key: INVOKE_KEY.to_string(),
                },
            )
        };

        let we2ai_result = invoke("we2ai_probe").expect("we2ai_probe should resolve");
        assert_eq!(
            we2ai_result.deserialize::<String>().unwrap(),
            "handled-by-we2ai"
        );

        let upstream_result = invoke("get_init_error").expect("get_init_error should resolve");
        assert_eq!(
            upstream_result.deserialize::<String>().unwrap(),
            "handled-by-upstream"
        );

        let rejected = invoke("switch_provider");
        assert!(
            rejected.is_err(),
            "expected a non-whitelisted command to be rejected, got {rejected:?}"
        );
    }

    /// 前后端各存一份上游命令白名单（Rust 的 [`UPSTREAM_COMMAND_WHITELIST`] 与
    /// `src/we2ai/ipcWhitelist.ts` 的 `WE2AI_UPSTREAM_IPC_WHITELIST`）。没有跨
    /// 语言共享常量的机制，这里在测试期直接读取并解析 TS 源文件，逐项 diff
    /// 两份列表，防止某一侧改动后另一侧忘记同步。
    #[test]
    fn upstream_whitelist_matches_frontend_ipc_whitelist() {
        let ts_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../src/we2ai/ipcWhitelist.ts");
        let source = std::fs::read_to_string(ts_path)
            .unwrap_or_else(|e| panic!("failed to read {ts_path}: {e}"));

        // 只截取 `export const WE2AI_UPSTREAM_IPC_WHITELIST = [ ... ]` 数组字面量
        // 的范围（用声明语句定位，而不是文件头部注释里同名的提及），避免把
        // isWe2aiIpcAllowed 里的 "we2ai_" 前缀字符串也当成白名单项解析进来。
        const DECL_MARKER: &str = "WE2AI_UPSTREAM_IPC_WHITELIST: readonly string[] = [";
        let decl_start = source
            .find(DECL_MARKER)
            .unwrap_or_else(|| panic!("`{DECL_MARKER}` declaration not found in ipcWhitelist.ts"));
        let array_start = decl_start + DECL_MARKER.len();
        let array_end = source[array_start..]
            .find(']')
            .map(|i| array_start + i)
            .expect("closing ] of WE2AI_UPSTREAM_IPC_WHITELIST not found");
        let array_source = &source[array_start..array_end];

        let frontend_whitelist: Vec<&str> = array_source.split('"').skip(1).step_by(2).collect();

        assert!(
            !frontend_whitelist.is_empty(),
            "parsed zero entries from ipcWhitelist.ts; parsing likely broke"
        );

        let mut frontend_sorted = frontend_whitelist.clone();
        frontend_sorted.sort_unstable();
        let mut backend_sorted: Vec<&str> = UPSTREAM_COMMAND_WHITELIST.to_vec();
        backend_sorted.sort_unstable();

        assert_eq!(
            frontend_sorted, backend_sorted,
            "src/we2ai/ipcWhitelist.ts (frontend) and \
             src-tauri/src/we2ai/mode.rs::UPSTREAM_COMMAND_WHITELIST (backend) \
             have drifted apart; update both together"
        );
    }
}
