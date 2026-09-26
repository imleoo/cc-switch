//! 工具安装检测与状态（方案第 4.4 节；第 4.1 节 CC Switch 并存提示）。
//!
//! | 工具 | 检测 | 下载 |
//! |---|---|---|
//! | Claude Code | 复用 `get_tool_versions` | Anthropic 官方文档页 |
//! | Codex | 复用 `get_tool_versions` | openai/codex 发布页 |
//! | WorkBuddy | macOS 读 `Info.plist` 版本；Windows 查安装路径；兜底查配置目录 | 国际 / 国内下载页 |

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::region::Region;

pub const CLAUDE_DOWNLOAD_URL: &str = "https://docs.anthropic.com/en/docs/claude-code/setup";
pub const CODEX_DOWNLOAD_URL: &str = "https://github.com/openai/codex/releases";
pub const WORKBUDDY_DOWNLOAD_URL_INTL: &str = "https://www.workbuddy.ai/downloads";
pub const WORKBUDDY_DOWNLOAD_URL_CN: &str = "https://www.workbuddy.cn/downloads/";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolStatus {
    /// `claude_code` / `codex` / `workbuddy`。
    pub tool: String,
    pub installed: bool,
    /// 已定位到可执行文件但 `--version` 失败（装了却跑不起来）。
    pub broken: bool,
    pub version: Option<String>,
    pub download_url: String,
    /// 当前生效的 WE2AI 模型（该工具配置指向 WE2AI 时）。
    pub managed_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolStatusReport {
    pub tools: Vec<ToolStatus>,
    /// CC Switch 也在运行：两者可能互相覆盖工具配置。
    pub cc_switch_running: bool,
}

pub fn workbuddy_download_url(region: Option<Region>) -> &'static str {
    match region {
        Some(Region::DomesticProd) | Some(Region::DomesticDev) => WORKBUDDY_DOWNLOAD_URL_CN,
        _ => WORKBUDDY_DOWNLOAD_URL_INTL,
    }
}

/// 从 `get_tool_versions` 的结果里取某个工具的 (版本, 已安装但无法运行)。
/// `ToolVersion` 字段私有，经 JSON 读取。版本为空且未标记 broken 视为未安装。
fn version_from_tool_versions(list: &[serde_json::Value], name: &str) -> (Option<String>, bool) {
    let Some(entry) = list
        .iter()
        .find(|v| v.get("name").and_then(|n| n.as_str()) == Some(name))
    else {
        return (None, false);
    };
    let version = entry
        .get("version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
    let broken = entry
        .get("installed_but_broken")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    (version, broken)
}

async fn cli_versions() -> Vec<serde_json::Value> {
    match crate::commands::get_tool_versions(
        Some(vec!["claude".to_string(), "codex".to_string()]),
        None,
    )
    .await
    {
        Ok(list) => list
            .into_iter()
            .filter_map(|v| serde_json::to_value(v).ok())
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// 从 XML 格式的 Info.plist 中读取 `CFBundleShortVersionString`。二进制 plist
/// 读不出版本，按"已安装、版本未知"处理。
fn plist_short_version(text: &str) -> Option<String> {
    let re = regex::Regex::new(r"<key>CFBundleShortVersionString</key>\s*<string>([^<]+)</string>")
        .ok()?;
    re.captures(text).map(|c| c[1].trim().to_string())
}

/// WorkBuddy 检测，路径可注入以便测试。
pub(crate) fn detect_workbuddy_at(
    mac_app: Option<&Path>,
    windows_exe: Option<&Path>,
    config_dir: &Path,
) -> (bool, Option<String>) {
    if let Some(app) = mac_app {
        let plist = app.join("Contents").join("Info.plist");
        if let Ok(bytes) = std::fs::read(&plist) {
            let version = std::str::from_utf8(&bytes)
                .ok()
                .and_then(plist_short_version);
            return (true, version);
        }
    }
    if let Some(exe) = windows_exe {
        if exe.is_file() {
            return (true, None);
        }
    }
    (workbuddy_config_dir_shows_install(config_dir), None)
}

/// 配置目录兜底：WE2AI 写任何工具前都会以 0700 创建 `~/.workbuddy`，写
/// WorkBuddy 时还会建 `models.json`，所以"目录存在"不能说明已安装（Fable P4
/// 终验中危项）。只有目录里有 WorkBuddy 自己的其他文件，或 `models.json` 里
/// 有不是 WE2AI 写入的条目时才视为已安装。
fn workbuddy_config_dir_shows_install(config_dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(config_dir) else {
        return false;
    };
    let mut has_models = false;
    for entry in entries.flatten() {
        if entry.file_name() == super::workbuddy::MODELS_FILE {
            has_models = true;
        } else {
            return true;
        }
    }
    if !has_models {
        return false;
    }
    std::fs::read_to_string(config_dir.join(super::workbuddy::MODELS_FILE))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.as_array().cloned())
        .is_some_and(|items| {
            items.iter().any(|item| {
                !item
                    .get("name")
                    .and_then(|n| n.as_str())
                    .is_some_and(|n| n.starts_with("WE2AI "))
            })
        })
}

fn workbuddy_mac_app() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        Some(PathBuf::from("/Applications/WorkBuddy.app"))
    } else {
        None
    }
}

fn workbuddy_windows_exe() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        std::env::var_os("LOCALAPPDATA").map(|base| {
            PathBuf::from(base)
                .join("Programs")
                .join("WorkBuddy")
                .join("WorkBuddy.exe")
        })
    } else {
        None
    }
}

/// Claude Code 当前指向 WE2AI 时的模型。
fn claude_managed_model(gateway_root: Option<&str>) -> Option<String> {
    let root = gateway_root?.trim_end_matches('/');
    let text = std::fs::read_to_string(crate::config::get_claude_settings_path()).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let env = value.get("env")?;
    if env
        .get("ANTHROPIC_BASE_URL")?
        .as_str()?
        .trim_end_matches('/')
        != root
    {
        return None;
    }
    // Key 已被移除（登出时勾选）则不算生效。
    env.get("ANTHROPIC_AUTH_TOKEN")?
        .as_str()
        .filter(|t| !t.is_empty())?;
    env.get("ANTHROPIC_MODEL")?.as_str().map(|s| s.to_string())
}

/// Codex 当前使用 WE2AI provider 时的模型。
fn codex_managed_model() -> Option<String> {
    let text = std::fs::read_to_string(crate::codex_config::get_codex_config_path()).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    if value.get("model_provider")?.as_str()? != super::apply::CODEX_MODEL_PROVIDER {
        return None;
    }
    value
        .get("model_providers")?
        .get(super::apply::CODEX_MODEL_PROVIDER)?
        .get("experimental_bearer_token")?
        .as_str()
        .filter(|t| !t.is_empty())?;
    value.get("model")?.as_str().map(|s| s.to_string())
}

/// 单个检测子进程的超时（Codex 验收 Y1）：`lsappinfo`/`tasklist`/`pgrep`
/// 正常几十毫秒内就会返回；一旦系统异常导致某次调用挂起，不能让这个检测
/// 本身无限期阻塞下去——超时按 `Unknown`（未能完成检测）处理（保守，不因为
/// 检测本身卡住而报告一个无法验证的"正在运行"或"没有在运行"）。
/// `kill_on_drop` 保证超时发生时子进程被真正杀掉，不留孤儿进程。
const CC_SWITCH_DETECT_SUBPROCESS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

async fn run_detect_subprocess(mut cmd: tokio::process::Command) -> Option<std::process::Output> {
    cmd.kill_on_drop(true);
    tokio::time::timeout(CC_SWITCH_DETECT_SUBPROCESS_TIMEOUT, cmd.output())
        .await
        .ok()
        .and_then(Result::ok)
}

/// CC Switch 是否在运行的检测结果（Codex 验收 Z2）：此前一律折叠成
/// `bool`，子进程启动失败/等待超时/等待中被杀、以及（此前完全没检查的）
/// 非零退出码，全部被 `unwrap_or(false)` 悄悄当成"确认没有在运行"——
/// 而这几种情况其实都是"这次检测没能得出结论"，不该等同于"确认过、真的
/// 没在运行"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CcSwitchRunningStatus {
    Running,
    NotRunning,
    /// 检测本身没能得出结论：子进程启动失败、等待超时被杀、或者进程正常
    /// 退出但结果没法按预期解读（macOS `lsappinfo` 非零退出、或者退出码
    /// 是 0 但 stdout 既不是空也不含 `ASN:` 这种没见过的输出形态；Linux
    /// `pgrep` 大于 1 的退出码——`pgrep` 的 1 是"语法正确但没有匹配到
    /// 进程"，是正常结果，只有更大的退出码才代表参数错误/执行故障）。
    Unknown,
}

impl CcSwitchRunningStatus {
    /// 完整 `tool_status()` 报表的 `cc_switch_running` 字段目前仍是布尔值
    /// （按最小改动选择：这个字段只喂给顶栏横幅，语义本就是"确认在运行才
    /// 提示，其余情况都不提示"的降级展示，不是这次要收紧精度的边界；真正
    /// 需要区分"没有在运行"与"没能确认"的是 apply 前的快速检测，那里直接
    /// 使用这个三态类型本身，不经过这次布尔收窄）。`Unknown` 归到 `false`
    /// 与收紧之前的行为一致，不是这次改动新引入的精度损失。
    fn as_bool_for_full_report(self) -> bool {
        matches!(self, Self::Running)
    }
}

/// macOS `lsappinfo find bundleid=<id>` 检测结果 → 三态：退出码是否成功、
/// 以及 stdout 内容（Codex 验收 W1）。此前用 `osascript -e 'application id
/// "..." is running'`：目标 bundle id 从未被 Launch Services 登记过时
/// （用户机器上没装过 CC Switch——这是绝大多数用户的真实状态），
/// `osascript` 本身会以非零退出码结束并报 AppleScript 层的 `-1728` 错误
/// （"can't get application ..."），错误文案还随系统语言变化、不宜按文本
/// 匹配；这会被当前的"非零退出 = Unknown"规则命中，导致几乎所有用户每次
/// 打开确认弹窗都看到"未能完成检测"，而不是"确认没有在运行"。实测对照：
/// `lsappinfo find bundleid=<从未安装的 id>` 退出码恒为 0、stdout 为空；
/// `lsappinfo find bundleid=<正在运行的 id>` 退出码 0、stdout 含
/// `ASN:<...>`；只有真正的执行故障（进程启动失败、等待超时等）才会走到
/// 非零退出/无法执行这条路径。
///
/// 三条判定规则（Codex 验收 V1，在 W1 的基础上再收紧一档）：① 退出码非 0
/// → `Unknown`（执行本身失败，见上）；② 退出码 0 且 stdout 去除首尾空白后
/// 为空 → `NotRunning`（未安装/未运行的正常情况，实测形态）；③ 退出码 0
/// 但 stdout 非空、又不含 `ASN:` → `Unknown`，不是 `NotRunning`——这是一种
/// 没见过的输出形态（`lsappinfo` 未来版本改了输出格式、或者传入了非预期
/// 参数），不能悄悄当成"确认没有在运行"，宁可提示"未能完成检测"。
///
/// 抽成不做任何 I/O 的纯函数（Codex 验收 Z2：补各平台分支的单测），不需要
/// 真的在 macOS 上跑一次 `lsappinfo` 才能验证这段映射逻辑本身对不对，其余
/// 两个平台的单测同理。`cfg` 加 `test`：这样在非 macOS 主机上跑
/// `cargo test` 也能编译并测到这段逻辑，而普通（非测试）编译在非 macOS
/// 目标上不会把它当成从未用到的死代码。
#[cfg(any(target_os = "macos", test))]
fn map_lsappinfo_result(success: bool, stdout: &str) -> CcSwitchRunningStatus {
    if !success {
        return CcSwitchRunningStatus::Unknown;
    }
    if stdout.trim().is_empty() {
        return CcSwitchRunningStatus::NotRunning;
    }
    if stdout.contains("ASN:") {
        CcSwitchRunningStatus::Running
    } else {
        // 退出码 0、stdout 非空，却不含 `ASN:`：没见过的输出形态，不能
        // 当成"确认没有在运行"。
        CcSwitchRunningStatus::Unknown
    }
}

/// Windows `tasklist` 检测结果 → 三态：退出码是否成功 + stdout 内容。
#[cfg(any(target_os = "windows", test))]
fn map_tasklist_result(success: bool, stdout: &str) -> CcSwitchRunningStatus {
    if !success {
        return CcSwitchRunningStatus::Unknown;
    }
    if stdout.to_ascii_lowercase().contains("cc-switch.exe") {
        CcSwitchRunningStatus::Running
    } else {
        CcSwitchRunningStatus::NotRunning
    }
}

/// Linux/其他 Unix `pgrep -x` 的退出码 → 三态：0 = 找到匹配进程，1 = 语法
/// 正确但没有匹配（正常的"没有在运行"，此前与真正的执行错误混在一起都
/// 算"未运行"），其余（2 语法错误、3 致命错误等，包括进程被信号杀死、
/// 没有退出码的情形）才是真正的执行故障。
#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn map_pgrep_exit_code(code: Option<i32>) -> CcSwitchRunningStatus {
    match code {
        Some(0) => CcSwitchRunningStatus::Running,
        Some(1) => CcSwitchRunningStatus::NotRunning,
        _ => CcSwitchRunningStatus::Unknown,
    }
}

pub async fn cc_switch_running_status() -> CcSwitchRunningStatus {
    use tokio::process::Command;
    #[cfg(target_os = "macos")]
    {
        let mut cmd = Command::new("/usr/bin/lsappinfo");
        cmd.args(["find", "bundleid=com.ccswitch.desktop"])
            .stderr(std::process::Stdio::null());
        match run_detect_subprocess(cmd).await {
            Some(o) => map_lsappinfo_result(o.status.success(), &String::from_utf8_lossy(&o.stdout)),
            None => CcSwitchRunningStatus::Unknown,
        }
    }
    #[cfg(target_os = "windows")]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut cmd = Command::new("tasklist");
        cmd.args(["/FI", "IMAGENAME eq cc-switch.exe", "/NH"])
            .creation_flags(CREATE_NO_WINDOW)
            .stderr(std::process::Stdio::null());
        match run_detect_subprocess(cmd).await {
            Some(o) => map_tasklist_result(o.status.success(), &String::from_utf8_lossy(&o.stdout)),
            None => CcSwitchRunningStatus::Unknown,
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut cmd = Command::new("pgrep");
        cmd.args(["-x", "cc-switch"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        match run_detect_subprocess(cmd).await {
            Some(o) => map_pgrep_exit_code(o.status.code()),
            None => CcSwitchRunningStatus::Unknown,
        }
    }
}

/// CC Switch 是否在运行（方案 4.1 节检测键）。用户自行改名的便携版检测不到。
///
/// 异步 + 子进程级超时（Codex 验收 Y1）：此前是同步阻塞调用、且没有任何
/// 超时，只能靠调用方把它塞进 `spawn_blocking`；改成基于 `tokio::process`
/// 的异步实现后，既能被"只检测这一项"的快速命令直接 `.await`，也能与
/// `tool_status()` 里其余阻塞检测项通过 `tokio::join!` 并发执行。
pub async fn cc_switch_running() -> bool {
    cc_switch_running_status().await.as_bool_for_full_report()
}

pub async fn tool_status(region: Option<Region>, data_root: PathBuf) -> ToolStatusReport {
    let versions = cli_versions().await;
    let gateway_root = region.map(|r| r.base_url());
    let blocking = tokio::task::spawn_blocking(move || {
        let (wb_installed, wb_version) = detect_workbuddy_at(
            workbuddy_mac_app().as_deref(),
            workbuddy_windows_exe().as_deref(),
            &super::workbuddy::config_dir(),
        );
        (
            wb_installed,
            wb_version,
            claude_managed_model(gateway_root),
            codex_managed_model(),
            super::workbuddy::managed_model(&data_root),
        )
    });
    // `cc_switch_running()` 现在是基于 `tokio::process` 的异步实现，不再需要
    // 塞进 `spawn_blocking`；与其余阻塞检测项并发执行（Codex 验收 Y1）。
    let (blocking, cc_running) = tokio::join!(blocking, cc_switch_running());
    let (wb_installed, wb_version, claude_model, codex_model, wb_model) =
        blocking.unwrap_or((false, None, None, None, None));

    let (claude_version, claude_broken) = version_from_tool_versions(&versions, "claude");
    let (codex_version, codex_broken) = version_from_tool_versions(&versions, "codex");
    ToolStatusReport {
        tools: vec![
            ToolStatus {
                tool: "claude_code".into(),
                installed: claude_version.is_some() || claude_broken,
                broken: claude_broken,
                version: claude_version,
                download_url: CLAUDE_DOWNLOAD_URL.into(),
                managed_model: claude_model,
            },
            ToolStatus {
                tool: "codex".into(),
                installed: codex_version.is_some() || codex_broken,
                broken: codex_broken,
                version: codex_version,
                download_url: CODEX_DOWNLOAD_URL.into(),
                managed_model: codex_model,
            },
            ToolStatus {
                tool: "workbuddy".into(),
                installed: wb_installed,
                broken: false,
                version: wb_version,
                download_url: workbuddy_download_url(region).into(),
                managed_model: wb_model,
            },
        ],
        cc_switch_running: cc_running,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // Codex 验收 Z2：三个平台的"检测结果 → 三态"映射逻辑，跑在哪个宿主
    // 平台上都能测——不需要真的在 macOS/Windows/Linux 上各跑一次对应的
    // 子进程才能验证这段映射本身对不对。

    #[test]
    fn lsappinfo_result_mapping_covers_running_not_installed_and_unknown() {
        // Codex 验收 W1：从未安装过目标 bundle id 时，`lsappinfo find` 退出码
        // 是 0、stdout 为空——不是 osascript 那样的非零退出+文案报错，必须
        // 映射成 NotRunning，不能是 Unknown（这是绝大多数用户的真实状态，
        // 旧的 `osascript` 实现会在这里误报"未能完成检测"）。
        assert_eq!(
            map_lsappinfo_result(true, ""),
            CcSwitchRunningStatus::NotRunning
        );
        // 正在运行：实测 `lsappinfo find bundleid=com.apple.finder`（运行中）
        // 输出形如 `ASN:0x0-0x11011-"访达":`。
        assert_eq!(
            map_lsappinfo_result(true, "ASN:0x0-0x11011-\"访达\":\n"),
            CcSwitchRunningStatus::Running
        );
        // 非零退出：真正的执行故障，不能当成"未运行"，也不能当成"在运行"。
        assert_eq!(
            map_lsappinfo_result(false, "ASN:0x0-0x11011-\"foo\":\n"),
            CcSwitchRunningStatus::Unknown
        );
        // 只有空白字符（无实际内容）同样按"未安装/未运行"处理，不要求
        // stdout 必须是字面意义上的空字符串。
        assert_eq!(
            map_lsappinfo_result(true, "\n"),
            CcSwitchRunningStatus::NotRunning
        );
        // Codex 验收 V1：退出码 0、stdout 非空，却不含 `ASN:`——没见过的
        // 输出形态（比如 `lsappinfo` 未来版本改了格式），不能悄悄当成
        // "确认没有在运行"，必须是 Unknown。
        assert_eq!(
            map_lsappinfo_result(true, "some unexpected output\n"),
            CcSwitchRunningStatus::Unknown
        );
    }

    #[test]
    fn tasklist_result_mapping_covers_running_not_running_and_unknown() {
        assert_eq!(
            map_tasklist_result(true, "cc-switch.exe  1234"),
            CcSwitchRunningStatus::Running
        );
        assert_eq!(
            map_tasklist_result(true, "INFO: No tasks..."),
            CcSwitchRunningStatus::NotRunning
        );
        assert_eq!(
            map_tasklist_result(false, "cc-switch.exe  1234"),
            CcSwitchRunningStatus::Unknown
        );
    }

    #[test]
    fn pgrep_exit_code_mapping_distinguishes_no_match_from_real_errors() {
        assert_eq!(map_pgrep_exit_code(Some(0)), CcSwitchRunningStatus::Running);
        // 1 = 语法正确但没有匹配到进程：正常的"没有在运行"，不是错误。
        assert_eq!(
            map_pgrep_exit_code(Some(1)),
            CcSwitchRunningStatus::NotRunning
        );
        // 2/3 等更大的退出码才是真正的执行故障（参数错误、致命错误）。
        assert_eq!(map_pgrep_exit_code(Some(2)), CcSwitchRunningStatus::Unknown);
        assert_eq!(map_pgrep_exit_code(Some(3)), CcSwitchRunningStatus::Unknown);
        // 没有退出码（如被信号杀死）同样算未能确认。
        assert_eq!(map_pgrep_exit_code(None), CcSwitchRunningStatus::Unknown);
    }

    #[test]
    fn unknown_status_is_reported_as_not_running_in_the_full_report_bool() {
        assert!(CcSwitchRunningStatus::Running.as_bool_for_full_report());
        assert!(!CcSwitchRunningStatus::NotRunning.as_bool_for_full_report());
        assert!(!CcSwitchRunningStatus::Unknown.as_bool_for_full_report());
    }

    #[test]
    fn reads_workbuddy_version_from_xml_plist() {
        let tmp = TempDir::new().unwrap();
        let app = tmp.path().join("WorkBuddy.app");
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>com.tencent.workbuddy.mac</string>\
             <key>CFBundleShortVersionString</key>\n  <string>5.5.3</string></dict></plist>",
        )
        .unwrap();
        let (installed, version) =
            detect_workbuddy_at(Some(&app), None, &tmp.path().join("missing"));
        assert!(installed);
        assert_eq!(version.as_deref(), Some("5.5.3"));
    }

    #[test]
    fn binary_plist_counts_as_installed_with_unknown_version() {
        let tmp = TempDir::new().unwrap();
        let app = tmp.path().join("WorkBuddy.app");
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        std::fs::write(app.join("Contents/Info.plist"), b"bplist00\xd1\x01\x02").unwrap();
        assert_eq!(
            detect_workbuddy_at(Some(&app), None, &tmp.path().join("missing")),
            (true, None)
        );
    }

    #[test]
    fn falls_back_to_windows_exe_then_config_dir() {
        let tmp = TempDir::new().unwrap();
        let exe = tmp.path().join("WorkBuddy.exe");
        std::fs::write(&exe, "").unwrap();
        let missing_app = tmp.path().join("none.app");
        assert_eq!(
            detect_workbuddy_at(Some(&missing_app), Some(&exe), &tmp.path().join("missing")),
            (true, None)
        );
        let config = tmp.path().join(".workbuddy");
        assert_eq!(detect_workbuddy_at(None, None, &config), (false, None));
        // WE2AI 自己建的空目录与只含 WE2AI 条目的 models.json 都不算安装。
        std::fs::create_dir(&config).unwrap();
        assert_eq!(detect_workbuddy_at(None, None, &config), (false, None));
        std::fs::write(
            config.join("models.json"),
            r#"[{"id":"m1","name":"WE2AI m1"}]"#,
        )
        .unwrap();
        assert_eq!(detect_workbuddy_at(None, None, &config), (false, None));
        std::fs::write(
            config.join("models.json"),
            r#"[{"id":"m1","name":"WE2AI m1"},{"id":"ds","name":"My DeepSeek"}]"#,
        )
        .unwrap();
        assert_eq!(detect_workbuddy_at(None, None, &config), (true, None));
        std::fs::write(config.join("models.json"), "[]").unwrap();
        std::fs::write(config.join("settings.json"), "{}").unwrap();
        assert_eq!(detect_workbuddy_at(None, None, &config), (true, None));
    }

    #[test]
    fn download_url_follows_region() {
        assert_eq!(
            workbuddy_download_url(Some(Region::International)),
            WORKBUDDY_DOWNLOAD_URL_INTL
        );
        assert_eq!(
            workbuddy_download_url(Some(Region::DomesticProd)),
            WORKBUDDY_DOWNLOAD_URL_CN
        );
        assert_eq!(workbuddy_download_url(None), WORKBUDDY_DOWNLOAD_URL_INTL);
    }

    #[test]
    fn version_lookup_treats_empty_as_not_installed() {
        let list = vec![
            serde_json::json!({"name": "claude", "version": "2.1.0", "installed_but_broken": false}),
            serde_json::json!({"name": "codex", "version": null, "installed_but_broken": false}),
        ];
        assert_eq!(
            version_from_tool_versions(&list, "claude"),
            (Some("2.1.0".to_string()), false)
        );
        assert_eq!(version_from_tool_versions(&list, "codex"), (None, false));
        assert_eq!(version_from_tool_versions(&list, "missing"), (None, false));
    }

    #[test]
    fn installed_but_broken_counts_as_installed() {
        let list = vec![serde_json::json!({
            "name": "codex", "version": null, "installed_but_broken": true
        })];
        assert_eq!(version_from_tool_versions(&list, "codex"), (None, true));
    }

    /// 读取的字段名与上游 `ToolVersion` 一致，防止上游改名后静默失效。
    #[test]
    fn upstream_tool_version_has_the_fields_we_read() {
        let source = include_str!("../commands/misc.rs");
        assert!(source.contains("installed_but_broken: bool"));
        assert!(source.contains("    version: Option<String>,"));
    }
}
