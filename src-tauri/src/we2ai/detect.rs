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

/// CC Switch 是否在运行（方案 4.1 节检测键）。用户自行改名的便携版检测不到。
pub fn cc_switch_running() -> bool {
    use std::process::{Command, Stdio};
    #[cfg(target_os = "macos")]
    {
        Command::new("osascript")
            .args(["-e", "application id \"com.ccswitch.desktop\" is running"])
            .stderr(Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
            .unwrap_or(false)
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq cc-switch.exe", "/NH"])
            .creation_flags(CREATE_NO_WINDOW)
            .stderr(Stdio::null())
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .to_ascii_lowercase()
                    .contains("cc-switch.exe")
            })
            .unwrap_or(false)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("pgrep")
            .args(["-x", "cc-switch"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
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
            cc_switch_running(),
        )
    })
    .await;
    let (wb_installed, wb_version, claude_model, codex_model, wb_model, cc_running) =
        blocking.unwrap_or((false, None, None, None, None, false));

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
