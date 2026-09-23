/**
 * We2aiShell 的极简本地化字符串表。
 *
 * We2aiShell 是 WE2AI 模式下唯一渲染的界面，其余上游视图（含完整 i18n 资源）
 * 都不会挂载。P0 阶段只覆盖 zh / en 两种语言的完整文案，zh-TW 回退到 zh、
 * ja 回退到 en——这是本阶段的已知范围收窄，后续阶段可以按需扩充。
 */

export type We2aiLanguage = "zh" | "en";

export interface We2aiStrings {
  brand: string;
  navMarketplace: string;
  navSettings: string;
  marketplaceTitle: string;
  marketplaceComingSoon: string;
  marketplaceDescription: string;
  settingsTitle: string;
  themeLabel: string;
  themeLight: string;
  themeDark: string;
  themeSystem: string;
  languageLabel: string;
  launchOnStartupLabel: string;
  launchOnStartupDescription: string;
  silentStartupLabel: string;
  silentStartupDescription: string;
  aboutTitle: string;
  versionLabel: string;
  checkForUpdates: string;
  checking: string;
  upToDate: string;
  updateAvailable: string;
  installAndRestart: string;
  checkFailed: string;
  officialWebsite: string;
  saveFailed: string;
}

const zh: We2aiStrings = {
  brand: "WE2AI",
  navMarketplace: "模型广场",
  navSettings: "设置",
  marketplaceTitle: "模型广场",
  marketplaceComingSoon: "即将上线",
  marketplaceDescription:
    "登录与模型广场正在开发中。上线后，你可以在这里选择账号下可用的模型，一键指定给 Claude Code、Codex、WorkBuddy 使用。",
  settingsTitle: "设置",
  themeLabel: "外观",
  themeLight: "浅色",
  themeDark: "深色",
  themeSystem: "跟随系统",
  languageLabel: "语言",
  launchOnStartupLabel: "开机自启",
  launchOnStartupDescription: "随系统启动自动运行 WE2AI",
  silentStartupLabel: "静默启动",
  silentStartupDescription: "启动时不显示主窗口，仅在托盘运行",
  aboutTitle: "关于",
  versionLabel: "版本",
  checkForUpdates: "检查更新",
  checking: "检查中…",
  upToDate: "已是最新版本",
  updateAvailable: "发现新版本",
  installAndRestart: "安装并重启",
  checkFailed: "检查更新失败",
  officialWebsite: "官方网站",
  saveFailed: "保存设置失败",
};

const en: We2aiStrings = {
  brand: "WE2AI",
  navMarketplace: "Model Marketplace",
  navSettings: "Settings",
  marketplaceTitle: "Model Marketplace",
  marketplaceComingSoon: "Coming soon",
  marketplaceDescription:
    "Sign-in and the model marketplace are under development. Once available, you'll pick a model from your account here and apply it to Claude Code, Codex, or WorkBuddy in one click.",
  settingsTitle: "Settings",
  themeLabel: "Appearance",
  themeLight: "Light",
  themeDark: "Dark",
  themeSystem: "System",
  languageLabel: "Language",
  launchOnStartupLabel: "Launch on startup",
  launchOnStartupDescription: "Automatically run WE2AI when you log in",
  silentStartupLabel: "Silent startup",
  silentStartupDescription:
    "Start minimized to the tray without showing the main window",
  aboutTitle: "About",
  versionLabel: "Version",
  checkForUpdates: "Check for updates",
  checking: "Checking…",
  upToDate: "You're up to date",
  updateAvailable: "Update available",
  installAndRestart: "Install and restart",
  checkFailed: "Failed to check for updates",
  officialWebsite: "Official website",
  saveFailed: "Failed to save settings",
};

const TABLE: Record<We2aiLanguage, We2aiStrings> = { zh, en };

/** 把设置里的语言码（含 zh-TW / ja 等）折叠为本壳支持的 zh / en 之一。 */
export function resolveWe2aiLanguage(
  raw: string | null | undefined,
): We2aiLanguage {
  if (raw && raw.toLowerCase().startsWith("zh")) {
    return "zh";
  }
  if (raw && raw.toLowerCase().startsWith("en")) {
    return "en";
  }
  if (raw && raw.toLowerCase().startsWith("ja")) {
    // P0 未提供日文文案，回退到英文而不是中文，更贴近日文用户的预期。
    return "en";
  }
  return "zh";
}

export function getWe2aiStrings(language: We2aiLanguage): We2aiStrings {
  return TABLE[language];
}
