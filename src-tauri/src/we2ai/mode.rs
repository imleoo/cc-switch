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

/// Unix 下把数据根收紧为仅本人可访问（方案第 8 节 P5：`~/.we2ai` 为 0700、数据库
/// 与备份为 0600）。目录 0700、文件 0600，递归处理子目录（最多到根下第 5 层；`backups/`、
/// `apply-snapshots/`、`logs/` 等），只收紧不放宽。启动时在数据库初始化之后调用；
/// 数据库 WAL 等运行期新建的文件落在 0700 目录内，其他本机用户无法进入。
pub fn harden_data_root() -> Vec<String> {
    // Windows 不做 Unix 权限处理，无需遍历目录树。
    if cfg!(not(unix)) {
        return Vec::new();
    }
    harden_data_root_at(&data_root())
}

/// 进程级记录：本次启动 `harden_data_root()` 的执行结果。`None` 表示尚未
/// 记录（理论上只会发生在 apply 相关命令不可达的极早期，或未走真实启动流程
/// 的测试环境）。用 `Mutex` 而非 `OnceLock`，是为了让单测能显式设置/重置这
/// 个全局状态覆盖失败与成功两种场景（`OnceLock` 一旦写入无法在同一进程内
/// 改写，测试无法覆盖"先失败后成功"）。
static HARDEN_RESULT: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);

/// 记录一次 [`harden_data_root`] 的执行结果，供 [`data_root_hardened`] 判断。
/// 正常运行时在启动阶段调用一次；重复调用直接覆盖为最新结果。
///
/// 锁中毒（曾有线程持锁期间 panic）时不静默跳过写入——用
/// `PoisonError::into_inner()` 拿回锁继续写，保证这次调用的结果一定被记录，
/// 不会因为某次无关的 panic 就让后续 apply 永远拿不到最新状态（Opus 复核
/// 中危项 M1）。
pub fn record_harden_result(failures: Vec<String>) {
    let mut guard = HARDEN_RESULT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = Some(failures);
}

/// 数据根权限是否已确认收紧成功：写入任何含 Key 的凭据（数据库行或工具 live
/// 文件）之前必须先检查这个函数。**fail-closed**：尚未记录过结果（`None`）
/// 或锁中毒都视为"未通过"，只有明确记录到"空失败列表"才视为通过——反过来
/// "未记录时默认放行"的写法（旧实现用 `Option::unwrap_or` 搭配 `true` 兜底）
/// 曾是 Opus 复核中危项 M1 指出的问题：一旦记录环节因为某种原因没跑到（如
/// 启动逻辑被上游同步改动打乱顺序），旧实现会默默放行写入，而不是拒绝。
/// 记录了非空失败项时同样视为"未通过"。
/// 非 Unix 平台 [`harden_data_root`] 恒返回空 `Vec`，正常启动流程下这里恒为
/// `true`。真实启动流程保证在任何写入入口可达之前就完成一次记录（`lib.rs`，
/// 早于 `app.manage(We2aiSessionState(...))`），测试环境需要显式调用
/// [`record_harden_result`] 模拟这一步（见 `apply_tests::TestHome::new`）。
pub fn data_root_hardened() -> bool {
    match HARDEN_RESULT.lock() {
        Ok(guard) => matches!(&*guard, Some(failures) if failures.is_empty()),
        Err(poisoned) => {
            // 锁中毒：状态不可信，一律视为未通过；立即释放拿回的守卫，不
            // 读取、不信任其中的内容。
            drop(poisoned.into_inner());
            false
        }
    }
}

/// 供错误提示展示具体的收紧失败原因；未记录、已成功或锁中毒时返回空。
pub fn data_root_harden_failures() -> Vec<String> {
    match HARDEN_RESULT.lock() {
        Ok(guard) => guard.clone().unwrap_or_default(),
        Err(poisoned) => poisoned.into_inner().clone().unwrap_or_default(),
    }
}

/// 仅供测试设置/重置 [`HARDEN_RESULT`]，覆盖失败/成功/未记录三种场景。
#[cfg(test)]
pub(crate) fn set_harden_result_for_test(result: Option<Vec<String>>) {
    let mut guard = HARDEN_RESULT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = result;
}

pub(crate) fn harden_data_root_at(root: &std::path::Path) -> Vec<String> {
    let mut failures = Vec::new();
    // 数据根本身是符号链接时不处理：收紧会改到链接目标（可能是用户其他目录）
    // （Codex P5 验收中危项）。
    if std::fs::symlink_metadata(root).is_ok_and(|m| m.file_type().is_symlink()) {
        failures.push(format!("{} 是符号链接，未收紧权限", root.display()));
        return failures;
    }
    if let Err(e) = super::fsguard::tighten_dir(root) {
        failures.push(e.to_string());
        return failures;
    }
    harden_children(root, 0, &mut failures);
    failures
}

fn harden_children(dir: &std::path::Path, depth: usize, failures: &mut Vec<String>) {
    const MAX_DEPTH: usize = 4;
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            failures.push(format!("{}: {e}", dir.display()));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                failures.push(format!("{}: {e}", dir.display()));
                continue;
            }
        };
        let path = entry.path();
        // 不跟随符号链接：数据根里不应有指向外部的链接，收紧它们的目标可能误伤。
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                failures.push(format!("{}: {e}", path.display()));
                continue;
            }
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            if let Err(e) = super::fsguard::tighten_dir(&path) {
                failures.push(e.to_string());
                continue;
            }
            if depth < MAX_DEPTH {
                harden_children(&path, depth + 1, failures);
            }
        } else if let Err(e) = super::fsguard::tighten_file(&path) {
            failures.push(format!("{}: {e}", path.display()));
        }
    }
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
    /// 定期数据库备份（启动时一次 + 每日定时）。备份是整库副本，含供应商行里
    /// 的 Key。
    PeriodicBackup,
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
        | StartupTask::CodexHistoryMigration
        | StartupTask::PeriodicBackup => false,
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

/// 唯一被信任发起 IPC 调用的窗口标签。
///
/// **安全边界（Codex 代码评审高危项）**：Tauri 2.10.3 会向*每一个*
/// webview 的主 frame 无条件注入 IPC 桥接脚本（`manager/webview.rs` 的
/// `IpcJavascript` 初始化脚本，不受 `capabilities/*.json` 的 `windows`
/// 字段影响），而 ACL 校验只在“命令属于插件”或“应用声明了自己的 ACL
/// manifest”时才生效（`webview/mod.rs` 的 `on_message`：
/// `if (plugin_command.is_some() || has_app_acl_manifest) ...`）。本应用
/// `build.rs` 只调用 `tauri_build::build()`，没有为自定义命令声明 ACL，
/// 因此 `capabilities/default.json` 的 `"windows": ["main"]` **只挡得住
/// `plugin:*` 命令，挡不住 `we2ai_*` 等自定义命令**——`gate()` 若只看命令名
/// 字符串，验证码窗口加载的远程页面（及其引入的第三方验证码 SDK 脚本）就能
/// 直接调用 `we2ai_logout`、`we2ai_login_*`、`we2ai_session_status`、
/// `set_auto_launch`、`open_external`、`install_update_and_restart` 等。
/// 因此 `gate()` 必须在分发前先校验调用来源：窗口标签必须是 `"main"`
/// （`captcha.rs` 的验证码窗口标签为 `"we2ai-captcha"`，天然被挡在外），且
/// `"main"` 窗口当前加载的 URL 必须是本应用资源（见 [`is_app_local_url`]），
/// 防止 `"main"` 未来被某处代码误导航到远程地址后仍被当作可信来源。
const TRUSTED_WEBVIEW_LABEL: &str = "main";

/// 开发构建下 `devUrl` 的端口（`tauri.conf.json` 的 `build.devUrl` 固定为
/// `http://localhost:3000`）。改这个端口时要同步改这里。
const DEV_SERVER_PORT: u16 = 3000;

/// 判定一个 URL 是否是本应用自己的资源（而非任意远程页面）。
///
/// **修复记录（Opus 5 代码评审阻断项，v1 实现有严重 bug）**：v1 只在
/// `cfg!(debug_assertions)` 为真时放行 `http://` scheme，且只认
/// `localhost`/`127.0.0.1`——这忽略了 Tauri 2 在 Windows/Android 上的
/// **生产构建**默认就是用 `http://tauri.localhost`（不是 `https`）：
/// `WindowManager::tauri_protocol_url()`（`manager/mod.rs:331-337`）在
/// `cfg!(windows) || cfg!(target_os = "android")` 时返回
/// `{http,https}://tauri.localhost`，由 `useHttpsScheme` 配置项决定
/// `http`/`https`（本项目 `tauri.conf.json` 未设置该项，默认 `false` →
/// `http`）；`tauri://localhost` 只用于 macOS/Linux。v1 上线会导致 Windows
/// 正式版主窗口（真实地址 `http://tauri.localhost/...`）的**全部** IPC 调用
/// 被 `gate()` 拒绝，应用在 Windows 上不可用。
///
/// 修复后按 scheme+host 精确匹配（**不区分当前编译目标平台**，接受下列
/// 全部形态的并集）：
/// - `tauri://localhost`：macOS/Linux 生产构建的地址。
/// - `http://tauri.localhost` 或 `https://tauri.localhost`：Windows/Android
///   生产构建的地址（分别对应 `useHttpsScheme=false`/`true`；本项目当前是
///   `false`，两个都放行是为了不因为将来打开这个开关而需要再改这里）。
///   在其他平台上收到这个 host 同样放行，不构成安全放宽——`.localhost` 是
///   IANA 保留的特殊用途域名（RFC 6761），真实互联网上的第三方不可能注册到
///   这个精确 host，接受"平台并集"只是省掉按 `cfg!(windows)` 分支的复杂度。
/// - `http://localhost:3000` 或 `http://127.0.0.1:3000`（端口精确匹配
///   [`DEV_SERVER_PORT`]）：仅 `debug_build` 为真时放行，对应 `tauri dev`
///   加载的 `devUrl`；release 构建不放行任何 `http://localhost` 形态的地址，
///   避免这条开发期例外被打包进正式产物。
/// - 其余（含 host 只是"看起来像"、加了后缀/前缀的仿冒域名）一律拒绝。
///
/// 拆成 `is_app_local_url`（真实调用，`debug_build` 取编译期
/// `cfg!(debug_assertions)`）与内部的构建类型显式传参版本，是为了让单测能
/// 同时覆盖"debug 构建"与"release 构建"两种判定结果——`cfg!(debug_assertions)`
/// 本身是编译期常量，没法在同一个测试二进制里让它先真后假。
fn is_app_local_url(url: &tauri::Url) -> bool {
    is_app_local_url_for_build(url, cfg!(debug_assertions))
}

fn is_app_local_url_for_build(url: &tauri::Url, debug_build: bool) -> bool {
    match (url.scheme(), url.host_str()) {
        ("tauri", Some("localhost")) => true,
        // 要求端口为 None（即没有显式端口，或显式端口恰好是该 scheme 的默认
        // 端口——`url` crate 会把后者规范化掉，`port()` 同样返回 `None`）。
        // `http://tauri.localhost:8080` 这种带非默认端口的地址不是 Tauri 生产
        // 构建会产生的真实地址，只可能是仿冒/中间人（Codex 代码评审第 3 轮
        // 低危项 1）。
        ("http" | "https", Some("tauri.localhost")) => url.port().is_none(),
        ("http", Some("localhost") | Some("127.0.0.1")) => {
            debug_build && url.port() == Some(DEV_SERVER_PORT)
        }
        _ => false,
    }
}

/// 调用来源是否可信：窗口标签为 `"main"` 且当前 URL 是本应用资源。
fn is_trusted_invoke_source<R: tauri::Runtime>(webview: &tauri::Webview<R>) -> bool {
    if webview.label() != TRUSTED_WEBVIEW_LABEL {
        return false;
    }
    match webview.url() {
        Ok(url) => is_app_local_url(&url),
        Err(_) => false,
    }
}

/// 构造 WE2AI 模式下的 IPC 分发器：`we2ai_*` 交给 `we2ai_handler`，
/// 上游白名单命令交给 `upstream_handler`，其余一律 `reject` 并返回 `true`
/// （避免框架在 `false` 返回后再报一次 command not found）。
///
/// 分发前先校验 [`is_trusted_invoke_source`]——不可信来源（如验证码窗口）
/// 一律拒绝，即便命令名本身在白名单内。
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
        if !is_trusted_invoke_source(invoke.message.webview_ref()) {
            let webview = invoke.message.webview();
            log::warn!(
                "[WE2AI] IPC 命令 {command} 来自不受信的窗口（label={:?}），已拒绝",
                webview.label()
            );
            invoke
                .resolver
                .reject(format!("命令 `{command}` 不允许从当前窗口调用"));
            return true;
        }
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
    #[serial_test::serial]
    fn data_root_ends_with_we2ai_dir() {
        let root = data_root();
        assert!(root.ends_with(".we2ai"), "got {}", root.display());
    }

    #[test]
    #[serial_test::serial]
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
            StartupTask::PeriodicBackup,
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

    /// 表驱动：平台 × 构建类型 → 期望的"本地地址"判定（Opus 5 代码评审阻断
    /// 项修复的回归测试）。`debug_build` 显式传参而不是依赖
    /// `cfg!(debug_assertions)`，这样才能在同一个测试二进制里同时断言
    /// "debug 下真"和"release 下假"两种结果。
    #[test]
    fn is_app_local_url_table_driven_platform_and_build_matrix() {
        struct Case {
            desc: &'static str,
            url: &'static str,
            debug_build: bool,
            expected: bool,
        }
        let cases = [
            // macOS / Linux 生产构建。
            Case {
                desc: "macOS/Linux prod, release",
                url: "tauri://localhost/",
                debug_build: false,
                expected: true,
            },
            Case {
                desc: "macOS/Linux prod, debug",
                url: "tauri://localhost/index.html",
                debug_build: true,
                expected: true,
            },
            // Windows/Android 生产构建：默认 useHttpsScheme=false → http。
            Case {
                desc: "Windows prod (http, useHttpsScheme=false), release",
                url: "http://tauri.localhost/index.html",
                debug_build: false,
                expected: true,
            },
            Case {
                desc: "Windows prod (http), debug",
                url: "http://tauri.localhost/",
                debug_build: true,
                expected: true,
            },
            // Windows/Android 若打开 useHttpsScheme=true。
            Case {
                desc: "Windows prod (https, useHttpsScheme=true), release",
                url: "https://tauri.localhost/",
                debug_build: false,
                expected: true,
            },
            // 开发服务器：仅 debug 放行，且端口必须精确匹配 devUrl。
            Case {
                desc: "dev server, debug build",
                url: "http://localhost:3000/",
                debug_build: true,
                expected: true,
            },
            Case {
                desc: "dev server host as 127.0.0.1, debug build",
                url: "http://127.0.0.1:3000/",
                debug_build: true,
                expected: true,
            },
            Case {
                desc: "dev server address compiled into a release build must not be trusted",
                url: "http://localhost:3000",
                debug_build: false,
                expected: false,
            },
            Case {
                desc: "debug build but wrong port must not match devUrl",
                url: "http://localhost:8080/",
                debug_build: true,
                expected: false,
            },
            // 仿冒/远程地址。
            Case {
                desc: "host with .evil.com suffix must not match tauri.localhost",
                url: "http://tauri.localhost.evil.com/",
                debug_build: false,
                expected: false,
            },
            Case {
                desc: "arbitrary https remote host",
                url: "https://evil.com/",
                debug_build: false,
                expected: false,
            },
            Case {
                desc: "arbitrary https remote host, debug build",
                url: "https://evil.com/",
                debug_build: true,
                expected: false,
            },
            Case {
                desc: "tauri scheme with wrong host",
                url: "tauri://evil.com/",
                debug_build: false,
                expected: false,
            },
            // Codex 代码评审第 3 轮低危项 3：补充更刁钻的仿冒形态，逐个用
            // `url` crate（2.5.8）的真实解析结果核实，而不是假设。
            Case {
                // userinfo 部分是 "tauri.localhost"，真正的 host 是
                // "evil.com"（`url::Url::host_str()` 已验证）——`@` 之前的
                // 部分只是用户名，浏览器/`url` crate 都不会把它当 host。
                desc: "userinfo trick: tauri.localhost@evil.com must resolve to host evil.com and be rejected",
                url: "http://tauri.localhost@evil.com/",
                debug_build: false,
                expected: false,
            },
            Case {
                // http/https 是 WHATWG "special scheme"，host 会被规范化成
                // 小写（`url::Url::host_str()` 验证返回 "tauri.localhost"），
                // 应当放行。
                desc: "uppercase host on a special scheme is lowercased by the URL parser and must be allowed",
                url: "http://TAURI.LOCALHOST/",
                debug_build: false,
                expected: true,
            },
            Case {
                // 结尾多一个点（FQDN 记法）：`url` crate 保留这个点
                // （`host_str()` 验证返回 "tauri.localhost."，与
                // "tauri.localhost" 不相等），必须拒绝。
                desc: "trailing dot (FQDN notation) must not match tauri.localhost",
                url: "http://tauri.localhost./",
                debug_build: false,
                expected: false,
            },
            Case {
                // host 里的 "%2e" 会被 URL 解析器当作百分号编码的 "."
                // 解码（`host_str()` 验证解码后返回 "tauri.localhost"），最终
                // 和字面量 "http://tauri.localhost/" 是同一个 host，因此按
                // 实际解析结果放行——这不是可被利用的旁路：真正发请求时用的
                // 也是这个已解码后的 host，不存在"匹配用一个值、连接用另一个
                // 值"的不一致。
                desc: "percent-encoded dot in host decodes to the literal host before matching (asserted against the actual url crate behavior)",
                url: "http://tauri%2elocalhost/",
                debug_build: false,
                expected: true,
            },
            Case {
                // "tauri" 是自定义（非 special）scheme，WHATWG URL 标准不会
                // 对非 special scheme 的 host 做大小写规范化，`host_str()`
                // 验证返回原样大写的 "LOCALHOST"，与 "localhost" 不相等，
                // 必须拒绝。
                desc: "custom scheme host casing is preserved (not lowercased) and must not match",
                url: "tauri://LOCALHOST",
                debug_build: false,
                expected: false,
            },
            Case {
                // 带显式非默认端口：修复后的实现要求 `url.port()` 为
                // `None`，带端口一律拒绝（Codex 代码评审第 3 轮低危项 1）。
                desc: "tauri.localhost with an explicit non-default port must be rejected",
                url: "http://tauri.localhost:8080/",
                debug_build: false,
                expected: false,
            },
        ];

        for case in cases {
            let url: tauri::Url = case.url.parse().expect("valid test URL");
            assert_eq!(
                is_app_local_url_for_build(&url, case.debug_build),
                case.expected,
                "case failed: {} ({})",
                case.desc,
                case.url
            );
        }
    }

    /// `is_app_local_url()`（真实调用路径）必须把 `cfg!(debug_assertions)`
    /// 转发给内部函数，而不是写死某个值——否则上面的表驱动测试验证的是另一
    /// 个函数，跟真正在 `gate()` 里跑的逻辑脱节。
    #[test]
    fn is_app_local_url_forwards_current_build_type() {
        let url: tauri::Url = "http://localhost:3000/".parse().unwrap();
        assert_eq!(
            is_app_local_url(&url),
            is_app_local_url_for_build(&url, cfg!(debug_assertions))
        );
    }

    /// 安全边界回归测试（Codex 代码评审高危项 1）：`gate()` 必须先校验调用
    /// 来源窗口，即便命令名本身在白名单/`we2ai_*` 前缀内，非 `"main"` 窗口
    /// （如验证码窗口 `we2ai-captcha`）发起的调用也必须被拒绝——否则远程验证
    /// 码页面及其第三方 SDK 脚本能直接调用 `we2ai_session_status` 之类的
    /// 命令（Tauri 2.10.3 向所有 webview 无条件注入 IPC 桥接脚本，见
    /// [`is_trusted_invoke_source`] 文档注释的证据链）。
    #[test]
    fn gate_rejects_invokes_from_non_main_windows_even_for_whitelisted_commands() {
        use tauri::ipc::{CallbackFn, InvokeBody};
        use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, INVOKE_KEY};
        use tauri::webview::InvokeRequest;
        use tauri::WebviewWindowBuilder;

        #[tauri::command]
        fn we2ai_probe() -> &'static str {
            "handled-by-we2ai"
        }
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

        // 模拟验证码窗口：非 "main" 标签（真实实现见 captcha.rs，标签固定为
        // "we2ai-captcha"），加载的 URL 与 label 无关——即便凑巧是本地协议，
        // 标签校验本身就必须先拒绝。
        let captcha_webview = WebviewWindowBuilder::new(&app, "we2ai-captcha", Default::default())
            .build()
            .expect("build mock captcha webview window");

        let invoke = |cmd: &str| {
            get_ipc_response(
                &captcha_webview,
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

        let we2ai_result = invoke("we2ai_probe");
        assert!(
            we2ai_result.is_err(),
            "we2ai_* command from a non-main window must be rejected, got {we2ai_result:?}"
        );

        let upstream_result = invoke("get_init_error");
        assert!(
            upstream_result.is_err(),
            "whitelisted upstream command from a non-main window must be rejected, got {upstream_result:?}"
        );
    }

    /// 同一个安全边界的第二层：即便窗口标签是 `"main"`，若其当前 URL 不是
    /// 本应用资源（例如被误导航到远程地址），也必须被拒绝。
    #[test]
    fn gate_rejects_invokes_from_main_window_navigated_to_a_remote_url() {
        use tauri::ipc::{CallbackFn, InvokeBody};
        use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, INVOKE_KEY};
        use tauri::webview::InvokeRequest;
        use tauri::{WebviewUrl, WebviewWindowBuilder};

        #[tauri::command]
        fn we2ai_probe() -> &'static str {
            "handled-by-we2ai"
        }

        let app = mock_builder()
            .invoke_handler(gate(
                tauri::generate_handler![we2ai_probe],
                tauri::generate_handler![],
            ))
            .build(mock_context(noop_assets()))
            .expect("build mock app");

        let remote_main = WebviewWindowBuilder::new(
            &app,
            "main",
            WebviewUrl::External("https://evil.example.com".parse().unwrap()),
        )
        .build()
        .expect("build mock main webview window pointed at a remote URL");

        let result = get_ipc_response(
            &remote_main,
            InvokeRequest {
                cmd: "we2ai_probe".into(),
                callback: CallbackFn(0),
                error: CallbackFn(1),
                url: "https://evil.example.com".parse().unwrap(),
                body: InvokeBody::default(),
                headers: Default::default(),
                invoke_key: INVOKE_KEY.to_string(),
            },
        );
        assert!(
            result.is_err(),
            "a \"main\"-labelled window navigated to a remote URL must not be trusted, got {result:?}"
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

    #[cfg(unix)]
    #[test]
    fn harden_data_root_makes_dirs_0700_and_files_0600_without_following_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join(".we2ai");
        let backups = root.join("backups");
        std::fs::create_dir_all(&backups).unwrap();
        for dir in [&root, &backups] {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let db = root.join("cc-switch.db");
        let backup = backups.join("db_backup_1.db");
        for f in [&db, &backup] {
            std::fs::write(f, "x").unwrap();
            std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let outside = tmp.path().join("outside.txt");
        std::fs::write(&outside, "x").unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();

        assert!(harden_data_root_at(&root).is_empty());

        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(&backups), 0o700);
        assert_eq!(mode(&db), 0o600);
        assert_eq!(mode(&backup), 0o600);
        assert_eq!(
            mode(&outside),
            0o644,
            "symlink targets outside the data root are untouched"
        );

        // 数据根本身是链接：报失败，不改目标目录。
        let target = tmp.path().join("real-root");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        let linked_root = tmp.path().join("linked-root");
        std::os::unix::fs::symlink(&target, &linked_root).unwrap();
        assert_eq!(harden_data_root_at(&linked_root).len(), 1);
        assert_eq!(mode(&target), 0o755);
    }

    /// [`data_root_hardened`] 三态：未记录（**fail-closed，视为未通过**——
    /// Opus 复核中危项 M1 之后的语义，真实启动流程保证在任何写入入口可达
    /// 之前就完成一次记录）、记录为空失败列表（通过）、记录非空失败列表
    /// （不通过）。测试结束前把全局状态复原为 `None`，避免影响同进程内其他
    /// 测试。与 `apply_tests.rs` 中同样操作这个全局状态的用例共用
    /// `#[serial]` 默认组，避免并行测试线程互相踩踏。
    #[test]
    #[serial_test::serial]
    fn data_root_hardened_reflects_recorded_result() {
        set_harden_result_for_test(None);
        assert!(
            !data_root_hardened(),
            "fail-closed: an unrecorded result must NOT default to hardened (Opus 复核中危项 M1)"
        );
        assert!(data_root_harden_failures().is_empty());

        record_harden_result(Vec::new());
        assert!(data_root_hardened());
        assert!(data_root_harden_failures().is_empty());

        record_harden_result(vec!["/tmp/x 属主不是当前用户".to_string()]);
        assert!(!data_root_hardened());
        assert_eq!(
            data_root_harden_failures(),
            vec!["/tmp/x 属主不是当前用户".to_string()]
        );

        // 复原，避免污染其他测试。
        set_harden_result_for_test(None);
    }

    /// 锁中毒：`record_harden_result` 不能静默跳过写入，`data_root_hardened`
    /// 必须 fail-closed（Opus 复核中危项 M1）。用一个真的会 panic 的临界区
    /// 制造真实的 `PoisonError`，而不是只测试 API 契约。`HARDEN_RESULT` 是
    /// 整个测试二进制共享的 `static`，`std::sync::Mutex` 中毒是永久的
    /// （直到显式 `clear_poison()`）——用 Drop 守卫保证无论断言是否 panic，
    /// 测试结束前都会清除中毒状态并把内容复原为安全默认值，不会污染同进程
    /// 内其他依赖这个全局状态的测试（`apply_tests::TestHome` 等）。
    #[test]
    #[serial_test::serial]
    fn poisoned_lock_is_fail_closed_and_record_still_writes() {
        struct ClearPoisonOnDrop;
        impl Drop for ClearPoisonOnDrop {
            fn drop(&mut self) {
                HARDEN_RESULT.clear_poison();
                if let Ok(mut guard) = HARDEN_RESULT.lock() {
                    *guard = None;
                }
            }
        }
        let _restore_on_drop = ClearPoisonOnDrop;

        set_harden_result_for_test(None);
        record_harden_result(Vec::new());
        assert!(data_root_hardened(), "sanity: recorded success is hardened");

        // 真的把锁弄中毒：临界区内 panic 后释放锁即中毒。
        let poison_result = std::panic::catch_unwind(|| {
            let _guard = HARDEN_RESULT.lock().unwrap();
            panic!("intentionally poisoning HARDEN_RESULT for the test");
        });
        assert!(poison_result.is_err());

        // 锁确实中毒了：直接 .lock() 会返回 Err。
        assert!(HARDEN_RESULT.lock().is_err());
        // fail-closed：即便锁中毒前的内容是"已收紧"，中毒之后也必须视为未通过。
        assert!(!data_root_hardened());

        // 中毒后仍能继续记录新结果（不能静默跳过写入）。
        record_harden_result(Vec::new());
        // 锁本身仍处于"中毒"状态（未调用 `clear_poison` 之前），但内容已经
        // 被更新——`record_harden_result` 用 `into_inner()` 恢复锁继续写入。
        // `data_root_hardened()` 对"中毒"统一判 false、不读取内容，这是有意
        // 的保守选择（见其文档注释），因此这里断言的是"写入没有被跳过"这件
        // 事本身，而不是 `data_root_hardened()` 的返回值。
        match HARDEN_RESULT.lock() {
            Ok(_) => panic!("lock should still be reported as poisoned"),
            Err(poisoned) => {
                assert_eq!(poisoned.into_inner().as_deref(), Some(&[][..]));
            }
        }
    }
}
