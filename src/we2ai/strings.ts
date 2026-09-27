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
  marketplaceDescription: string;
  // 模型广场（方案第 1 节、第 8 节 P3）
  keyLabel: string;
  keyPlaceholder: string;
  loadingKeys: string;
  noKeys: string;
  refresh: string;
  loadingModels: string;
  noModels: string;
  modelNoTools: string;
  // 分组折扣价（B1 定价扩展，docs/we2ai/B1定价契约.md）
  priceInput: string;
  priceOutput: string;
  priceCacheRead: string;
  /** 按次计费行的标签（Opus 复核 P8），与 `priceUnitPerRequest`（单位文案）分开。 */
  pricePerRequest: string;
  /** 按秒计费行的标签（Opus 复核 R1，取代"标签仍写按次、单位写每秒"这种
   * 自相矛盾的组合），与 `priceUnitPerSecond`（单位文案）分开。 */
  pricePerSecond: string;
  priceUnitPerMillionTokens: string;
  priceUnitPerRequest: string;
  /** 视频等按秒计费的单位文案（v3 契约 `per_request_unit === "second"`）。 */
  priceUnitPerSecond: string;
  pricePeakActive: string;
  /** 加价场景（`effective_multiplier > 1`）的倍率标注，`{multiplier}` 占位符
   * 传入去尾零后的倍率数字（Opus 复核 P5）。 */
  priceMultiplierBadge: string;
  priceFootnote: string;
  priceUnavailable: string;
  /** 屏幕阅读器专用前缀，不影响可见文案（Opus 复核 P7）。 */
  priceOriginalSrLabel: string;
  priceDiscountedSrLabel: string;
  /** 价格获取时间页脚，`{time}` 占位符传入 `HH:mm`（Opus 复核 P3）。 */
  priceFetchedAt: string;
  // 工具写入（方案第 4 节、第 8 节 P4）
  toolNotInstalled: string;
  toolBroken: string;
  toolDownload: string;
  toolCurrentModel: string;
  toolNotConfigured: string;
  ccSwitchRunningBanner: string;
  /** apply 前的快速检测超时未完成时的提示（Codex 验收 Y1）。 */
  applyDetectionIncomplete: string;
  // 恢复官方配置（方案 P6，顶栏单工具入口）
  restoreOfficialAction: string;
  restoreOfficialConfirmTitle: string;
  restoreOfficialConfirmDescription: string;
  restoreOfficialConfirmConfirm: string;
  restoring: string;
  applyConfirmTitle: string;
  applyConfirmDescription: string;
  applyFilesLabel: string;
  applyFieldsLabel: string;
  applyExtraChangesLabel: string;
  applyAdvanced: string;
  slotSonnet: string;
  slotOpus: string;
  slotHaiku: string;
  applyConfirm: string;
  applyCancel: string;
  applying: string;
  applySuccess: string;
  applyToolNotInstalled: string;
  applyCurrent: string;
  applyPlanFailed: string;
  workbuddyOverwriteTitle: string;
  workbuddyOverwriteDescription: string;
  workbuddyOverwriteConfirm: string;
  errorApplyTakeover: string;
  errorApplyTakeoverDetected: string;
  errorApplyPrecondition: string;
  errorApplyGeneric: string;
  errorApplyConcurrent: string;
  errorApplyExtraChangesStale: string;
  keyBlockedQuotaExhausted: string;
  keyBlockedExpired: string;
  keyBlockedDisabled: string;
  keyBlockedInsufficientBalance: string;
  keyBlockedSubscription: string;
  keyBlockedUsageLimit: string;
  keyBlockedGroup: string;
  keyBlockedIp: string;
  keyBlockedGeneric: string;
  keyBlockedWithCode: string;
  errorKeyNotFound: string;
  errorKeyListTooLarge: string;
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
  /**
   * 检查更新失败时 toast 的描述文案：不直接展示英文原始错误（如
   * updater 插件抛出的 "Could not fetch a valid release JSON from the
   * remote"），原始错误只写 console；这里是面向用户的友好提示。
   */
  checkFailedHint: string;
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
  logoutRestoreOfficial: string;
  toolsRestored: string;
  toolsRestoreSkipped: string;
  toolsRestoreFailed: string;
  /** 没有任何工具需要恢复（本来就没指向 WE2AI），不是警告也不是失败。 */
  toolsRestoreNoop: string;
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
  marketplaceDescription: "选择 Key 后查看它可用的模型，以及每个模型支持的工具",
  keyLabel: "Key",
  keyPlaceholder: "选择一个 Key",
  loadingKeys: "正在加载 Key 列表",
  noKeys: "当前账号没有可用的 Key，请先在 WE2AI 网站创建并分组",
  refresh: "刷新",
  loadingModels: "正在加载可用模型",
  noModels: "这个 Key 当前没有可用的模型",
  modelNoTools: "暂无支持的工具",
  priceInput: "输入",
  priceOutput: "输出",
  priceCacheRead: "缓存读",
  pricePerRequest: "按次",
  pricePerSecond: "按秒",
  priceUnitPerMillionTokens: "每百万 tokens",
  priceUnitPerRequest: "每次",
  priceUnitPerSecond: "每秒",
  pricePeakActive: "高峰价",
  priceMultiplierBadge: "×{multiplier} 倍率",
  priceFootnote:
    "折后价已含该模型适用的分组倍率与高峰规则；长上下文分档、推理等级等可能使实际扣费不同",
  priceUnavailable: "暂无定价",
  priceOriginalSrLabel: "原价",
  priceDiscountedSrLabel: "折后价",
  priceFetchedAt: "价格获取于 {time}",
  toolNotInstalled: "未安装",
  toolBroken: "已安装但无法运行",
  toolDownload: "下载",
  toolCurrentModel: "当前：{model}",
  toolNotConfigured: "未指定 WE2AI 模型",
  ccSwitchRunningBanner:
    "检测到另一个配置管理工具正在运行，可能与 WE2AI 互相覆盖这些工具的配置",
  applyDetectionIncomplete: "未能完成检测（可能是网络较慢），仍可继续确认",
  restoreOfficialAction: "恢复官方",
  restoreOfficialConfirmTitle: "恢复 {tool} 的官方配置？",
  restoreOfficialConfirmDescription:
    "将从下列文件中移除 WE2AI 写入的这些字段，其余内容保持不变",
  restoreOfficialConfirmConfirm: "恢复",
  restoring: "正在恢复",
  applyConfirmTitle: "将 {model} 指定给 {tool}",
  applyConfirmDescription: "只改写以下文件中的这些字段，其余内容保持不变",
  applyFilesLabel: "文件",
  applyFieldsLabel: "字段",
  applyExtraChangesLabel: "以下内容也会被一并改动（非 WE2AI 托管，由工具自身的写入逻辑触发）",
  applyAdvanced: "高级：分别指定各槽位模型",
  slotSonnet: "Sonnet 槽位",
  slotOpus: "Opus 槽位",
  slotHaiku: "Haiku 槽位",
  applyConfirm: "写入并激活",
  applyCancel: "取消",
  applying: "正在写入",
  applySuccess: "已将 {model} 指定给 {tool}",
  applyToolNotInstalled: "{tool} 尚未安装，配置已写好，安装后即可使用",
  applyCurrent: "当前使用中",
  applyPlanFailed: "无法读取将写入的文件清单，请关闭后重试",
  workbuddyOverwriteTitle: "覆盖 WorkBuddy 中的同名条目？",
  workbuddyOverwriteDescription:
    "WorkBuddy 中已有 {model} 条目，且内容与 WE2AI 上次写入的不同。",
  workbuddyOverwriteConfirm: "覆盖为 WE2AI 配置",
  errorApplyTakeover:
    "该工具正被其他程序代理接管，请先在该程序中关闭接管",
  errorApplyTakeoverDetected: "检测到代理接管，未生效",
  errorApplyPrecondition: "WE2AI 记录的供应商状态异常，已停止写入",
  errorApplyGeneric: "写入失败",
  errorApplyConcurrent: "WorkBuddy 配置正被其他程序频繁修改，请稍后重试",
  errorApplyExtraChangesStale:
    "确认弹窗展示的内容与当前配置不一致（配置在确认期间被修改），未写入，请重新打开确认弹窗核对后再试",
  keyBlockedQuotaExhausted: "这个 Key 的额度已用完，暂时无法调用",
  keyBlockedExpired: "这个 Key 已过期",
  keyBlockedDisabled: "这个 Key 已被停用",
  keyBlockedInsufficientBalance: "账户余额不足",
  keyBlockedSubscription: "没有有效的订阅",
  keyBlockedUsageLimit: "订阅用量已达上限",
  keyBlockedGroup: "这个 Key 所在的分组不可用",
  keyBlockedIp: "当前网络的 IP 不在这个 Key 的访问名单内",
  keyBlockedGeneric: "这个 Key 当前无法调用",
  keyBlockedWithCode: "这个 Key 当前无法调用（{code}）",
  errorKeyNotFound: "这个 Key 已不在列表中，请刷新",
  errorKeyListTooLarge:
    "Key 数量过多，无法完整加载，请在 WE2AI 网站清理不用的 Key",
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
  checkFailedHint: "暂时无法获取更新信息，请稍后重试",
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
  logoutRestoreOfficial: "同时恢复工具的官方配置（移除 WE2AI 写入的内容）",
  toolsRestored: "已恢复 {count} 个工具的官方配置",
  toolsRestoreSkipped: "部分工具的官方配置未恢复",
  toolsRestoreFailed: "未能恢复工具的官方配置",
  toolsRestoreNoop: "工具未指向 WE2AI，无需恢复",
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
  marketplaceDescription:
    "Pick a key to see the models it can use and the tools each model supports",
  keyLabel: "Key",
  keyPlaceholder: "Select a key",
  loadingKeys: "Loading keys",
  noKeys:
    "This account has no usable keys. Create one and assign it to a group on the WE2AI website first.",
  refresh: "Refresh",
  loadingModels: "Loading available models",
  noModels: "This key has no available models right now",
  modelNoTools: "No supported tools yet",
  priceInput: "Input",
  priceOutput: "Output",
  priceCacheRead: "Cache read",
  pricePerRequest: "Per request",
  pricePerSecond: "Per second",
  priceUnitPerMillionTokens: "per 1M tokens",
  priceUnitPerRequest: "per request",
  priceUnitPerSecond: "per second",
  pricePeakActive: "Peak pricing",
  priceMultiplierBadge: "×{multiplier} multiplier",
  priceFootnote:
    "The discounted price already reflects this model's applicable group multiplier and peak pricing. Long-context tiers and reasoning-effort levels may still change the actual charge.",
  priceUnavailable: "No pricing available",
  priceOriginalSrLabel: "Original price",
  priceDiscountedSrLabel: "Discounted price",
  priceFetchedAt: "Prices fetched at {time}",
  toolNotInstalled: "Not installed",
  toolBroken: "Installed but not working",
  toolDownload: "Download",
  toolCurrentModel: "Current: {model}",
  toolNotConfigured: "No WE2AI model set",
  ccSwitchRunningBanner:
    "Another configuration manager is also running. It and WE2AI may overwrite each other's tool settings.",
  applyDetectionIncomplete:
    "Couldn't finish the check (network may be slow); you can still confirm",
  restoreOfficialAction: "Restore official",
  restoreOfficialConfirmTitle: "Restore the official config for {tool}?",
  restoreOfficialConfirmDescription:
    "These fields written by WE2AI will be removed from the files below. Everything else stays as is.",
  restoreOfficialConfirmConfirm: "Restore",
  restoring: "Restoring",
  applyConfirmTitle: "Use {model} in {tool}",
  applyConfirmDescription:
    "Only these fields in these files are changed. Everything else stays as is.",
  applyFilesLabel: "Files",
  applyFieldsLabel: "Fields",
  applyExtraChangesLabel:
    "These will also be changed (not managed by WE2AI; triggered by the tool's own write logic)",
  applyAdvanced: "Advanced: choose a model per slot",
  slotSonnet: "Sonnet slot",
  slotOpus: "Opus slot",
  slotHaiku: "Haiku slot",
  applyConfirm: "Write and activate",
  applyCancel: "Cancel",
  applying: "Writing",
  applySuccess: "{tool} now uses {model}",
  applyToolNotInstalled:
    "{tool} isn't installed yet. The settings are ready once you install it.",
  applyCurrent: "In use",
  applyPlanFailed:
    "Couldn't load the list of files to be written. Close and try again.",
  workbuddyOverwriteTitle: "Overwrite the WorkBuddy entry?",
  workbuddyOverwriteDescription:
    "WorkBuddy already has a {model} entry that differs from what WE2AI last wrote.",
  workbuddyOverwriteConfirm: "Overwrite with WE2AI settings",
  errorApplyTakeover:
    "This tool is being proxy-managed by another program. Turn off takeover there first.",
  errorApplyTakeoverDetected:
    "Proxy takeover detected. The change did not take effect.",
  errorApplyPrecondition:
    "WE2AI's provider records are in an unexpected state. Nothing was written.",
  errorApplyGeneric: "Write failed",
  errorApplyConcurrent:
    "Another program keeps changing the WorkBuddy settings. Please try again later.",
  errorApplyExtraChangesStale:
    "The confirmation dialog no longer matches the current config (it was changed while you were confirming). Nothing was written — please reopen the dialog and try again.",
  keyBlockedQuotaExhausted: "This key has used up its quota",
  keyBlockedExpired: "This key has expired",
  keyBlockedDisabled: "This key is disabled",
  keyBlockedInsufficientBalance: "Your account balance is too low",
  keyBlockedSubscription: "No active subscription",
  keyBlockedUsageLimit: "Your subscription usage limit has been reached",
  keyBlockedGroup: "This key's group is unavailable",
  keyBlockedIp: "Your current IP is not allowed for this key",
  keyBlockedGeneric: "This key can't be used right now",
  keyBlockedWithCode: "This key can't be used right now ({code})",
  errorKeyNotFound: "This key is no longer in the list. Please refresh.",
  errorKeyListTooLarge:
    "Too many keys to load completely. Remove unused keys on the WE2AI website.",
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
  checkFailedHint: "Couldn't fetch update information right now. Please try again later.",
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
  logoutRestoreOfficial:
    "Also restore the official config for tools (remove what WE2AI wrote)",
  toolsRestored: "Restored the official config for {count} tool(s)",
  toolsRestoreSkipped: "The official config for some tools wasn't restored",
  toolsRestoreFailed: "Couldn't restore the official config for tools",
  toolsRestoreNoop: "The tool wasn't pointed at WE2AI; nothing to restore",
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
    case "KEY_NOT_FOUND":
      return t.errorKeyNotFound;
    case "KEY_LIST_TOO_LARGE":
      return t.errorKeyListTooLarge;
    default:
      return t.errorGeneric;
  }
}
