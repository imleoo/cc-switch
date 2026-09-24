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

  // 登录（方案第 5.1、5.2 节）
  loginTitle: string;
  loginSubtitle: string;
  regionLabel: string;
  regionInternational: string;
  regionDomesticProd: string;
  regionDomesticDev: string;
  tabEmailLogin: string;
  tabPhoneLogin: string;
  emailLabel: string;
  passwordLabel: string;
  loginButton: string;
  loginButtonBusy: string;
  twoFaTitle: string;
  twoFaDescription: string;
  twoFaCodeLabel: string;
  twoFaSubmit: string;
  twoFaSubmitBusy: string;
  twoFaBack: string;
  phoneLabel: string;
  phoneCodeLabel: string;
  sendCodeButton: string;
  sendCodeButtonBusy: string;
  resendCodeIn: string;
  phoneLoginButton: string;
  phoneLoginButtonBusy: string;
  freeLoginNote: string;
  errorNetwork: string;
  errorGeneric: string;
  errorTokenRevoked: string;
  errorUserNotActive: string;
  errorBackendModeActive: string;
  errorBackendModeActivePhoneHint: string;
  errorInvalidCredentials: string;
  errorSessionPersistFailed: string;

  // 已登录状态 / 登出
  loggedInAs: string;
  keyringDegradedWarning: string;
  sessionNotPersistableWarning: string;
  offlineBanner: string;
  offlineRetryCountdown: string;
  offlineRetry: string;
  offlineRetrying: string;
  logoutButton: string;
  logoutConfirmTitle: string;
  logoutConfirmDescription: string;
  logoutConfirmConfirm: string;
  logoutConfirmCancel: string;
  logoutSuccessRevoked: string;
  logoutSuccessLocalOnly: string;
  logoutFailed: string;
  /** Codex 代码评审第 5 轮高危项 3：本地清理两项全部失败时的提示。 */
  logoutCleanupFailed: string;
  lastRegionSaveFailed: string;
  localCleanupPendingBanner: string;
  /** Codex 代码评审第 5 轮中危项 5：等待系统钥匙串授权超过 10 秒的提示。 */
  waitingForKeyringAuthorization: string;
}

const zh: We2aiStrings = {
  brand: "WE2AI",
  navMarketplace: "模型广场",
  navSettings: "设置",
  marketplaceTitle: "模型广场",
  marketplaceComingSoon: "即将上线",
  marketplaceDescription:
    "模型广场正在开发中。上线后，你可以在这里选择账号下可用的模型，一键指定给 Claude Code、Codex、WorkBuddy 使用。",
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

  loginTitle: "登录 WE2AI",
  loginSubtitle:
    "登录后即可在这里管理 Claude Code、Codex、WorkBuddy 使用的模型",
  regionLabel: "区域",
  regionInternational: "国际版",
  regionDomesticProd: "国内版",
  regionDomesticDev: "国内版（测试环境）",
  tabEmailLogin: "邮箱登录",
  tabPhoneLogin: "手机登录",
  emailLabel: "邮箱",
  passwordLabel: "密码",
  loginButton: "登录",
  loginButtonBusy: "登录中…",
  twoFaTitle: "两步验证",
  twoFaDescription: "已向 {email} 发送验证，请输入验证器 App 中的 6 位动态码",
  twoFaCodeLabel: "6 位动态码",
  twoFaSubmit: "验证并登录",
  twoFaSubmitBusy: "验证中…",
  twoFaBack: "返回",
  phoneLabel: "手机号",
  phoneCodeLabel: "短信验证码",
  sendCodeButton: "发送验证码",
  sendCodeButtonBusy: "发送中…",
  resendCodeIn: "{seconds} 秒后可重新发送",
  phoneLoginButton: "登录",
  phoneLoginButtonBusy: "登录中…",
  freeLoginNote:
    "默认最长 30 天免登录，服务端策略、凭证撤销或网络指纹变化可能提前失效",
  errorNetwork: "网络连接失败，请检查网络后重试",
  errorGeneric: "操作失败，请稍后重试",
  errorTokenRevoked: "登录状态已被撤销，请重新登录",
  errorUserNotActive: "账号不可用，请联系管理员",
  errorBackendModeActive: "平台当前仅允许管理员登录",
  errorBackendModeActivePhoneHint: "手机登录暂不可用，请使用邮箱登录",
  errorInvalidCredentials: "邮箱或密码不正确",
  errorSessionPersistFailed: "无法保存登录状态，请重试",

  loggedInAs: "已登录：{email}",
  keyringDegradedWarning:
    "系统钥匙串不可用，登录状态仅保留在本次会话中，重启后需要重新登录",
  sessionNotPersistableWarning: "登录状态无法保存，下次启动需要重新登录",
  offlineBanner: "网络连接不可用，正在使用离线缓存的登录状态",
  offlineRetryCountdown: "{seconds} 秒后自动重试",
  offlineRetry: "重试",
  offlineRetrying: "重试中…",
  logoutButton: "登出",
  logoutConfirmTitle: "确认登出？",
  logoutConfirmDescription:
    "登出后需要重新登录才能继续使用。工具（Claude Code / Codex / WorkBuddy）配置中已写入的 Key 会保留，不会被删除。",
  logoutConfirmConfirm: "确认登出",
  logoutConfirmCancel: "取消",
  logoutSuccessRevoked: "已退出",
  logoutSuccessLocalOnly: "本地已退出，远端撤销未确认",
  logoutFailed: "登出失败，请稍后重试",
  logoutCleanupFailed: "退出未完成：无法清除本机保存的登录凭据",
  lastRegionSaveFailed: "无法保存区域选择，下次启动可能恢复为之前的区域",
  localCleanupPendingBanner: "本机保存的登录凭据未能完全清除，请重试",
  waitingForKeyringAuthorization: "正在等待系统钥匙串授权",
};

const en: We2aiStrings = {
  brand: "WE2AI",
  navMarketplace: "Model Marketplace",
  navSettings: "Settings",
  marketplaceTitle: "Model Marketplace",
  marketplaceComingSoon: "Coming soon",
  marketplaceDescription:
    "The model marketplace is under development. Once available, you'll pick a model from your account here and apply it to Claude Code, Codex, or WorkBuddy in one click.",
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

  loginTitle: "Sign in to WE2AI",
  loginSubtitle:
    "Sign in to manage the models used by Claude Code, Codex, and WorkBuddy",
  regionLabel: "Region",
  regionInternational: "International",
  regionDomesticProd: "Mainland China",
  regionDomesticDev: "Mainland China (test)",
  tabEmailLogin: "Email",
  tabPhoneLogin: "Phone",
  emailLabel: "Email",
  passwordLabel: "Password",
  loginButton: "Sign in",
  loginButtonBusy: "Signing in…",
  twoFaTitle: "Two-factor authentication",
  twoFaDescription:
    "Verifying {email}. Enter the 6-digit code from your authenticator app.",
  twoFaCodeLabel: "6-digit code",
  twoFaSubmit: "Verify and sign in",
  twoFaSubmitBusy: "Verifying…",
  twoFaBack: "Back",
  phoneLabel: "Phone number",
  phoneCodeLabel: "SMS code",
  sendCodeButton: "Send code",
  sendCodeButtonBusy: "Sending…",
  resendCodeIn: "Resend in {seconds}s",
  phoneLoginButton: "Sign in",
  phoneLoginButtonBusy: "Signing in…",
  freeLoginNote:
    "You'll stay signed in for up to 30 days by default; server policy, credential revocation, or network fingerprint changes may end it sooner.",
  errorNetwork:
    "Network request failed, please check your connection and try again",
  errorGeneric: "Something went wrong, please try again later",
  errorTokenRevoked: "Your session has been revoked, please sign in again",
  errorUserNotActive:
    "This account is not active, please contact an administrator",
  errorBackendModeActive: "Only administrators can sign in right now",
  errorBackendModeActivePhoneHint:
    "Phone sign-in is unavailable, please use email instead",
  errorInvalidCredentials: "Incorrect email or password",
  errorSessionPersistFailed:
    "Couldn't save your sign-in state, please try again",

  loggedInAs: "Signed in as {email}",
  keyringDegradedWarning:
    "The system keychain is unavailable; you'll need to sign in again after restarting the app",
  sessionNotPersistableWarning:
    "Your sign-in state couldn't be saved; you'll need to sign in again next time you start the app",
  offlineBanner: "No network connection; showing your cached sign-in state",
  offlineRetryCountdown: "retrying automatically in {seconds}s",
  offlineRetry: "Retry",
  offlineRetrying: "Retrying…",
  logoutButton: "Sign out",
  logoutConfirmTitle: "Sign out?",
  logoutConfirmDescription:
    "You'll need to sign in again to continue. API keys already written into Claude Code, Codex, or WorkBuddy configs will be kept, not removed.",
  logoutConfirmConfirm: "Sign out",
  logoutConfirmCancel: "Cancel",
  logoutSuccessRevoked: "Signed out",
  logoutSuccessLocalOnly:
    "Signed out locally; the server could not confirm revocation",
  logoutFailed: "Failed to sign out, please try again later",
  logoutCleanupFailed:
    "Sign-out incomplete: couldn't clear the credentials saved on this machine",
  lastRegionSaveFailed:
    "Couldn't save your region choice; the previous region may be restored next launch",
  localCleanupPendingBanner:
    "Couldn't fully clear the sign-in credentials saved on this machine. Please retry.",
  waitingForKeyringAuthorization: "Waiting for system keychain authorization",
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

/** 简单的 `{key}` 占位符替换，避免为几处插值引入完整的 i18n 插值库。 */
export function formatWe2aiString(
  template: string,
  vars: Record<string, string | number>,
): string {
  return template.replace(/\{(\w+)\}/g, (match, key: string) =>
    key in vars ? String(vars[key]) : match,
  );
}

/**
 * 把后端错误码（`We2aiApiError.code`，见 `commands_auth.rs`）中文化 /
 * 英文化。未识别的错误码回退到 `errorGeneric`，网络错误单独识别。
 */
export function getWe2aiErrorMessage(t: We2aiStrings, code: string): string {
  switch (code) {
    case "NETWORK_ERROR":
    case "TRANSIENT":
      return t.errorNetwork;
    case "TOKEN_REVOKED":
    case "REFRESH_TOKEN_REUSED":
    case "REFRESH_TOKEN_INVALID":
    case "REFRESH_TOKEN_EXPIRED":
    case "SESSION_BINDING_MISMATCH":
    case "NEEDS_RELOGIN":
    case "NO_ACTIVE_SESSION":
      return t.errorTokenRevoked;
    case "USER_NOT_ACTIVE":
      return t.errorUserNotActive;
    case "BACKEND_MODE_ACTIVE":
      return t.errorBackendModeActive;
    case "INVALID_TOKEN":
    case "INVALID_CREDENTIALS":
    case "INVALID_USER":
      return t.errorInvalidCredentials;
    case "SESSION_PERSIST_FAILED":
      return t.errorSessionPersistFailed;
    default:
      return t.errorGeneric;
  }
}
