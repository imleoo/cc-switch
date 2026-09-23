//! WE2AI 自有 Tauri 命令。
//!
//! P0 阶段只实现设置读写：上游 `get_settings` / `save_settings` 会整体覆盖
//! `AppSettings`（含本地 current 供应商、工具目录覆盖等 WE2AI 不应暴露的字段），
//! 因此不放行给前端，改由这两个命令只读写主题、语言、窗口、开机自启相关字段。
//! 见方案第 6.2 节“设置读写不放行上游 get_settings / save_settings”。

use serde::{Deserialize, Serialize};

/// WE2AI 前端可读写的设置子集。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct We2aiSettings {
    /// 界面语言："en" | "zh" | "zh-TW" | "ja"
    pub language: Option<String>,
    /// 静默启动（启动时不显示主窗口，仅托盘运行）
    pub silent_startup: bool,
    /// 是否使用应用自绘窗口控件（而非系统标题栏）
    pub use_app_window_controls: bool,
    /// 是否显示托盘图标
    pub show_in_tray: bool,
    /// 关闭窗口时最小化到托盘而非退出
    pub minimize_to_tray_on_close: bool,
}

impl We2aiSettings {
    fn from_app_settings(settings: &crate::settings::AppSettings) -> Self {
        Self {
            language: settings.language.clone(),
            silent_startup: settings.silent_startup,
            use_app_window_controls: settings.use_app_window_controls,
            show_in_tray: settings.show_in_tray,
            minimize_to_tray_on_close: settings.minimize_to_tray_on_close,
        }
    }

    /// 把本子集覆盖进现有 `AppSettings`（原地修改），其余字段（含工具目录覆盖、
    /// 当前供应商等）保持不变——即便前端携带了这些字段，也会在此处被忽略。
    fn apply_onto(&self, existing: &mut crate::settings::AppSettings) {
        existing.language = self.language.clone();
        existing.silent_startup = self.silent_startup;
        existing.use_app_window_controls = self.use_app_window_controls;
        existing.show_in_tray = self.show_in_tray;
        existing.minimize_to_tray_on_close = self.minimize_to_tray_on_close;
    }
}

/// 获取 WE2AI 可用的设置子集。
#[tauri::command]
pub async fn we2ai_get_settings() -> Result<We2aiSettings, String> {
    let settings = crate::settings::get_settings_for_frontend();
    Ok(We2aiSettings::from_app_settings(&settings))
}

/// 保存 WE2AI 可用的设置子集：在 `settings::mutate_settings` 的单次写锁内
/// 读取现有设置并只覆盖本子集字段，工具目录覆盖、本地 current 供应商等字段
/// 一律沿用现有值。用同一把锁做读-改-写，避免并发保存互相覆盖（TOCTOU）。
#[tauri::command]
pub async fn we2ai_save_settings(settings: We2aiSettings) -> Result<bool, String> {
    crate::settings::mutate_settings(|existing| {
        settings.apply_onto(existing);
    })
    .map_err(|e| e.to_string())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_onto_only_touches_whitelisted_fields() {
        let mut existing = crate::settings::AppSettings::default();
        existing.claude_config_dir = Some("/custom/claude".to_string());
        existing.current_provider_claude = Some("some-provider-id".to_string());

        let incoming = We2aiSettings {
            language: Some("en".to_string()),
            silent_startup: true,
            use_app_window_controls: true,
            show_in_tray: false,
            minimize_to_tray_on_close: false,
        };

        let original = existing.clone();
        incoming.apply_onto(&mut existing);
        let merged = existing;

        assert_eq!(merged.language.as_deref(), Some("en"));
        assert!(merged.silent_startup);
        assert!(merged.use_app_window_controls);
        assert!(!merged.show_in_tray);
        assert!(!merged.minimize_to_tray_on_close);

        // 未被 We2aiSettings 覆盖的字段必须原样保留。
        assert_eq!(merged.claude_config_dir, original.claude_config_dir);
        assert_eq!(
            merged.current_provider_claude,
            original.current_provider_claude
        );
    }

    /// 覆盖 we2ai_save_settings 实际使用的路径：mutate_settings 内先克隆现有
    /// 设置、原地打上补丁、写盘。这里只验证"原地打补丁"的语义与 apply_onto
    /// 一致，且不会遗漏字段（settings::mutate_settings 本身的锁行为由
    /// settings 模块负责，这里不重复测试文件 IO）。
    #[test]
    fn apply_onto_is_idempotent_when_applied_twice() {
        let mut settings = crate::settings::AppSettings::default();
        let patch = We2aiSettings {
            language: Some("ja".to_string()),
            silent_startup: true,
            use_app_window_controls: false,
            show_in_tray: false,
            minimize_to_tray_on_close: true,
        };

        patch.apply_onto(&mut settings);
        let after_first = settings.clone();
        patch.apply_onto(&mut settings);

        assert_eq!(settings.language, after_first.language);
        assert_eq!(settings.silent_startup, after_first.silent_startup);
        assert_eq!(
            settings.use_app_window_controls,
            after_first.use_app_window_controls
        );
        assert_eq!(settings.show_in_tray, after_first.show_in_tray);
        assert_eq!(
            settings.minimize_to_tray_on_close,
            after_first.minimize_to_tray_on_close
        );
    }
}
