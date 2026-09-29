import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/**
 * WE2AI 前端可读写的设置子集，字段对应 `src-tauri/src/we2ai/commands.rs`
 * 的 `We2aiSettings`（camelCase，由 serde `rename_all = "camelCase"` 保证）。
 */
export interface We2aiSettings {
  language: string | null;
  silentStartup: boolean;
  useAppWindowControls: boolean;
  showInTray: boolean;
  minimizeToTrayOnClose: boolean;
}

/**
 * 登录会话相关类型，字段名与 `src-tauri/src/we2ai/commands_auth.rs` /
 * `session.rs` 的 serde 输出一一对应（已用 Rust 单测
 * `commands_auth::serde_shape_tests*` 锁定实际 wire 形状，不是按猜测写的）。
 */
export type We2aiRegion = "international" | "domestic_prod" | "domestic_dev";

export interface We2aiApiError {
  code: string;
  message: string;
}

/** 判断一个 `invoke()` 的 rejection 是否是 [`We2aiApiError`] 形状。 */
export function isWe2aiApiError(error: unknown): error is We2aiApiError {
  return (
    typeof error === "object" &&
    error !== null &&
    typeof (error as { code?: unknown }).code === "string" &&
    typeof (error as { message?: unknown }).message === "string"
  );
}

export interface We2aiPublicSettings {
  captchaRequired: boolean;
  backendModeEnabled: boolean;
}

export type We2aiLoginOutcome =
  | { kind: "loggedIn" }
  | { kind: "requires2fa"; tempToken: string; emailMasked: string };

export type We2aiLogoutOutcome =
  | "revoked"
  | "localOnly"
  /**
   * 远端未确认撤销，且本地清理（索引 + 钥匙串）两项全部失败——不能保证
   * 重启不会恢复这个会话。前端不能提示"已退出"，必须提示清理失败并提供
   * 重试（Codex 代码评审第 5 轮高危项 3）。
   */
  | "localCleanupFailed"
  | "notLoggedIn";

/**
 * 启动恢复会话的结果（对应 `src-tauri/src/we2ai/commands_auth.rs` 的
 * `We2aiResumeOutcome`）：
 * - `restored`：刷新成功，会话可用；
 * - `needLogin`：没有可恢复的会话，应展示登录页；
 * - `offlineRetained`：恢复出的会话在刷新阶段遇到网络错误，Rust 侧内存状态
 *   原样保留（钥匙串、会话索引都没有被清空），前端必须继续展示"已登录"
 *   界面并提示离线，不能回登录页（方案第 5.2 节"断网不回登录页"）。
 */
export type We2aiResumeOutcome = "restored" | "needLogin" | "offlineRetained";

export interface We2aiSessionSummary {
  loggedIn: boolean;
  region: We2aiRegion | null;
  emailMasked: string | null;
  keyringDegraded: boolean;
  /**
   * 登录成功时会话索引写入失败：这次会话仅在当前进程内有效，不可持久
   * 恢复，下次启动需要重新登录（Codex 代码评审第 3 轮高危项 2）。语义
   * 与 `keyringDegraded` 一致但原因不同，前端分开展示提示文案，避免把
   * "索引写失败"错误归因成"系统钥匙串不可用"。
   */
  indexDegraded: boolean;
  /** 断网自动重试的下次尝试倒计时（秒），不在重试状态时为 `null`。 */
  offlineRetryInSeconds: number | null;
  /**
   * 上次登出或会话终止后本机凭据（索引与钥匙串）都未能清除：重启可能恢复
   * 该会话。未登录界面也要展示提示与重试入口（Codex 验收第 5 轮高危项 2）。
   */
  localCleanupPending: boolean;
}

/** 客户端能写入的三个工具（B1 `tools` 字段取值）。 */
export type We2aiTool = "claude_code" | "codex" | "workbuddy";

/** Key 的脱敏视图（`src-tauri/src/we2ai/keys.rs` 的 `KeyView`），不含明文。 */
export interface We2aiKeyView {
  id: number;
  name: string;
  groupName: string | null;
  status: "active" | "quota_exhausted" | string;
  maskedKey: string;
}

export interface We2aiKeyList {
  keys: We2aiKeyView[];
  /** 记住的上次选择；没有记忆时为第一个 Key；列表为空时为 `null`。 */
  selectedKeyId: number | null;
}

/**
 * B1 定价扩展顶层 `pricing`（`src-tauri/src/we2ai/keys.rs` 的 `PricingView`，
 * 见 `docs/we2ai/B1定价契约.md`）：Key 分组缺失或倍率无法解析时
 * 整个 `pricing` 为 `null`，前端不显示价格区。
 */
export interface We2aiPricing {
  cnyRate: number;
  rateMultiplier: number;
  peakMultiplier: number;
  peakActive: boolean;
  effectiveMultiplier: number;
  unit: string;
}

/**
 * B1 定价扩展每模型 `price`（`ModelPriceView`）：与实际扣费同源的标准
 * 首档价，折后价已乘该模型实际 `multiplier`（旧服务端缺省时回退
 * `pricing.effectiveMultiplier`），`base*` 为未乘倍率的原价，均为美元单价；
 * 人民币换算 = 美元 × `pricing.cnyRate`。无法解析价格的
 * 模型该字段为 `null`。
 */
export interface We2aiModelPrice {
  billingMode: string;
  input: number | null;
  output: number | null;
  cacheRead: number | null;
  cacheWrite: number | null;
  cacheWrite1h: number | null;
  perRequest: number | null;
  baseInput: number | null;
  baseOutput: number | null;
  baseCacheRead: number | null;
  baseCacheWrite: number | null;
  baseCacheWrite1h: number | null;
  basePerRequest: number | null;
  /**
   * v2 契约新增：该模型实际扣费倍率（token 类 = 分组倍率 × 分组高峰；
   * image/video 类是独立的图片/视频倍率，不叠加高峰）。折后字段 =
   * `base_* × multiplier`。缺失或服务端给出的值无效（非有限/≤0，已在
   * Rust 侧过滤）时为 `null`，客户端回退到顶层 `pricing.effectiveMultiplier`
   * （见 `pricing.ts` 的 `resolveWe2aiEffectiveMultiplier`）。
   */
  multiplier: number | null;
  /**
   * v3 契约新增：按次计费的单位，已在 Rust 侧归一化为 `"request"`（缺省
   * 即此）/`"second"`（视频按秒）之一；服务端给出未识别的值时为 `null`，
   * 客户端据此不展示按次这一行，避免展示错误单位。
   */
  perRequestUnit: "request" | "second" | null;
}

export interface We2aiModelView {
  id: string;
  provider: string | null;
  tools: We2aiTool[];
  price: We2aiModelPrice | null;
}

/** SubPanel B1 结果：模型、支持的工具与 Key 级准入。 */
export interface We2aiKeyModels {
  models: We2aiModelView[];
  callable: boolean;
  blockedReason: string | null;
  pricing: We2aiPricing | null;
}

/** 工具安装与当前生效模型（`src-tauri/src/we2ai/detect.rs`）。 */
export interface We2aiToolStatus {
  tool: We2aiTool;
  installed: boolean;
  /** 已安装但 `--version` 失败（装了却跑不起来）。 */
  broken: boolean;
  version: string | null;
  downloadUrl: string;
  /** 该工具配置当前指向 WE2AI 时的模型。 */
  managedModel: string | null;
}

export interface We2aiToolStatusReport {
  tools: We2aiToolStatus[];
  /** CC Switch 也在运行，两者可能互相覆盖工具配置。 */
  ccSwitchRunning: boolean;
}

/**
 * apply 前快速检测的三态结果（Codex 验收 Z2）：`"unknown"` 表示检测本身
 * 没能得出结论（子进程启动失败、非零退出等），不是"确认没有在运行"。
 */
export type We2aiCcSwitchRunningStatus = "running" | "not_running" | "unknown";

/**
 * 合并"apply 前快速检测"的三态结果与完整报告 `ccSwitchRunning` 布尔值，
 * 得出最终展示用的"CC Switch 是否在运行"（Codex 验收 W2/V2：确认弹窗与
 * 顶栏必须复用同一份判定，不要各写一套可能不一致的逻辑）。快速检测取得
 * 确定结果（`"running"`/`"not_running"`）时优先采用——它比完整报告更新、
 * 更准确；只有快速结果是 `"unknown"`（检测本身没能得出结论）、或还没有
 * 过一次快速结果（`null`/`undefined`）时，才回退到完整报告的值。
 */
export function resolveCcSwitchRunning(
  quickStatus: We2aiCcSwitchRunningStatus | null | undefined,
  fullReportRunning: boolean | null | undefined,
): boolean {
  if (quickStatus === "running") return true;
  if (quickStatus === "not_running") return false;
  return fullReportRunning ?? false;
}

/**
 * 计划里的单个文件项：`display` 是展示给用户的名称，`path` 是真实路径
 * （Opus 复核中危项 S1）——绝大多数文件两者相同；Codex 模型目录文件的真实
 * 文件名含上游历史遗留字样，`display` 会是中性标签。**只渲染 `display`**，
 * `path` 不出现在任何界面文本（含 tooltip/title），只用作 React key 之类
 * 的内部用途。
 */
export interface We2aiPlanFile {
  display: string;
  path: string;
}

/**
 * 确认弹窗里展示的单条"额外变更"：`id` 是稳定内部标识，只用于随
 * `We2aiApplyRequest.expectedExtraChanges` 原样带回给 Rust 侧做写入前的
 * STALE 比对，**不渲染到界面**；`display` 才是真正展示给用户看的文案
 * （Codex 验收 X2②：某些额外变更的文本片段来自用户自己起的 provider 表
 * 名，`display` 已经在 Rust 侧对品牌残留模式做过中性化，前端只需原样
 * 渲染，不需要也不应该再对它做二次处理）。
 */
export interface We2aiExtraChange {
  id: string;
  display: string;
}

/** 确认弹窗展示的"将写入的文件与字段"。 */
export interface We2aiApplyPlan {
  files: We2aiPlanFile[];
  fields: string[];
  /**
   * 上游写入管道会无条件一并改动、但不属于 WE2AI 托管字段的内容（如 Claude
   * 内部专用字段被移除、Codex 保留名 provider 表被迁移改名）。写入前如实
   * 展示，不悄悄发生（Codex 验收偏差修复项 B）。恢复计划不经过上游 switch
   * 管道，恒为空数组。
   */
  extraChanges: We2aiExtraChange[];
}

/** Claude Code 三个槽位；未指定的与主模型相同。 */
export interface We2aiClaudeSlots {
  sonnet?: string | null;
  opus?: string | null;
  haiku?: string | null;
}

export interface We2aiApplyRequest {
  tool: We2aiTool;
  keyId: number;
  model: string;
  claudeSlots?: We2aiClaudeSlots;
  /** WorkBuddy 同名条目需要确认覆盖时传 true。 */
  overwrite?: boolean;
  /**
   * 确认弹窗展示、用户已经看到并点击确认的那份 `We2aiApplyPlan.extraChanges`
   * 里每一条的 `id`（不是 `display`：`id` 才是稳定内部标识，参见
   * `We2aiExtraChange` 的文档）。Rust 侧写入前会重新计算一次并与这份 id
   * 列表比对，不一致（配置在"计划展示→点击确认"期间被外部改动）即拒绝
   * 写入（Opus 复核低危项 L6）。
   */
  expectedExtraChanges?: string[];
}

export interface We2aiRestoreOfficialOutcome {
  restored: string[];
  /** 本来就没有可做的事，不是失败（未指向 WE2AI、或正被 CC Switch 代理接管）。 */
  unchanged: string[];
  /** 真失败：读取/解析失败、WorkBuddy 条目被手工修改、会话已变化等。 */
  skipped: string[];
}

export interface We2aiApplyOutcome {
  model: string;
  files: string[];
  warnings: string[];
}

/**
 * 用户公告（`src-tauri/src/we2ai/announcements.rs` 的 `AnnouncementView`）。
 * `content` 是 Markdown，必须经 `announcementMarkdown.ts` 净化后再渲染；
 * 已读以服务端 `readAt` 为准。
 */
export interface We2aiAnnouncement {
  id: number;
  title: string;
  content: string;
  /** `"popup"` 登录后弹窗；`"silent"` 只在铃铛里显示。 */
  notifyMode: "popup" | "silent";
  startsAt: string | null;
  endsAt: string | null;
  readAt: string | null;
  createdAt: string;
}

/** Rust 后台轮询发现未读公告集合变化时发出的事件名。 */
export const WE2AI_ANNOUNCEMENTS_CHANGED_EVENT = "we2ai-announcements-changed";

/**
 * WE2AI 自有命令的前端封装。P0 阶段只有设置读写——上游 `get_settings` /
 * `save_settings` 在 WE2AI 模式下被 IPC 白名单拒绝，不能复用 `settingsApi`。
 * P2 新增登录会话命令：前端只传业务字段，验证码票据完全由 Rust 侧
 * （`captcha.rs`）在内部处理，不经过这里的任何返回值。
 */
export const we2aiApi = {
  async getSettings(): Promise<We2aiSettings> {
    return await invoke("we2ai_get_settings");
  },

  async saveSettings(settings: We2aiSettings): Promise<boolean> {
    return await invoke("we2ai_save_settings", { settings });
  },

  async getAvailableRegions(): Promise<We2aiRegion[]> {
    return (
      (await invoke<We2aiRegion[] | null>("we2ai_available_regions")) ?? []
    );
  },

  async getLastRegion(): Promise<We2aiRegion | null> {
    return (await invoke<We2aiRegion | null>("we2ai_get_last_region")) ?? null;
  },

  async setLastRegion(region: We2aiRegion): Promise<void> {
    await invoke("we2ai_set_last_region", { region });
  },

  async getPublicSettings(region: We2aiRegion): Promise<We2aiPublicSettings> {
    return await invoke("we2ai_get_public_settings", { region });
  },

  async resumeSession(region: We2aiRegion): Promise<We2aiResumeOutcome> {
    return await invoke("we2ai_resume_session", { region });
  },

  /** 立即重试一次断网自动退避，不等后台定时器（窗口聚焦/`online`/手动按钮）。 */
  async retryNow(): Promise<We2aiResumeOutcome> {
    return await invoke("we2ai_retry_now");
  },

  async loginEmail(
    region: We2aiRegion,
    email: string,
    password: string,
  ): Promise<We2aiLoginOutcome> {
    return await invoke("we2ai_login_email", { region, email, password });
  },

  async login2fa(
    region: We2aiRegion,
    tempToken: string,
    totpCode: string,
  ): Promise<void> {
    await invoke("we2ai_login_2fa", { region, tempToken, totpCode });
  },

  async sendSmsCode(region: We2aiRegion, phone: string): Promise<void> {
    await invoke("we2ai_send_sms_code", { region, phone });
  },

  async loginPhone(
    region: We2aiRegion,
    phone: string,
    code: string,
  ): Promise<void> {
    await invoke("we2ai_login_phone", { region, phone, code });
  },

  async sessionStatus(): Promise<We2aiSessionSummary> {
    return await invoke("we2ai_session_status");
  },

  async logout(): Promise<We2aiLogoutOutcome> {
    return await invoke("we2ai_logout");
  },

  /**
   * 登出远端未确认、且本地清理（索引 + 钥匙串）两项都失败时会话停在
   * "待清理"状态。重试必须调用这个专门的命令，不能再调 `logout()`——
   * 那样会尝试重新发起一次远端登出请求，但此时已经没有可用的 refresh
   * token 了（Codex 代码评审第 6 轮高危项 3）。
   */
  async retryLocalCleanup(): Promise<We2aiLogoutOutcome> {
    return await invoke("we2ai_retry_local_cleanup");
  },

  /** 拉取当前账号全部可用 Key（Rust 侧分页到最后一页）。 */
  async listKeys(): Promise<We2aiKeyList> {
    return await invoke("we2ai_list_keys");
  },

  /** 记住选中的 Key（按区域 + 账号分开存放）。 */
  async selectKey(keyId: number): Promise<void> {
    await invoke("we2ai_select_key", { keyId });
  },

  async keyModels(keyId: number): Promise<We2aiKeyModels> {
    return await invoke("we2ai_key_models", { keyId });
  },

  async toolStatus(): Promise<We2aiToolStatusReport> {
    return await invoke("we2ai_tool_status");
  },

  /**
   * 只做"CC Switch 是否在运行"这一项快速检测（Codex 验收 Y1）：完整的
   * `toolStatus()` 会连带查询工具版本（每次无缓存联网查 npm 最新版本，
   * 单次超时 15 秒），国内网络下可能让 apply 前的检测耗时接近 30 秒。
   * apply 前只等这一个命令，顶栏完整刷新仍然异步、独立进行。
   *
   * 返回三态而不是布尔（Codex 验收 Z2）：`"unknown"` 表示这次检测没能
   * 得出结论（子进程启动失败/非零退出等），调用方要把它和"确认没有在
   * 运行"区分开，不能直接当成 `false`。
   */
  async ccSwitchRunningQuick(): Promise<We2aiCcSwitchRunningStatus> {
    return await invoke("we2ai_cc_switch_running_quick");
  },

  /**
   * 恢复某个/某些工具的官方配置：移除 WE2AI 为其写入的一切（P6，取代
   * `removeToolKeys`）。登出弹窗勾选"同时恢复工具的官方配置"时对全部三个
   * 工具调用；顶栏"恢复官方"按钮对单个工具调用，无论是否登录都可用。
   */
  async restoreOfficial(
    tools: We2aiTool[],
  ): Promise<We2aiRestoreOfficialOutcome> {
    return await invoke("we2ai_restore_official", { tools });
  },

  async applyPlan(tool: We2aiTool): Promise<We2aiApplyPlan> {
    return await invoke("we2ai_apply_plan", { tool });
  },

  /**
   * 恢复官方确认弹窗展示的"将移除的文件与字段"（P6）。与 `applyPlan` 是两份
   * 独立的计划——那份是"将写入什么"，恢复场景下 `env.ANTHROPIC_API_KEY`
   * 是写回而不是删除，Codex 也不会触碰模型目录文件。
   */
  async restorePlan(tool: We2aiTool): Promise<We2aiApplyPlan> {
    return await invoke("we2ai_restore_plan", { tool });
  },

  /** 把模型写入工具配置并激活；Key 明文由 Rust 侧按 keyId 取出。 */
  async applyModel(request: We2aiApplyRequest): Promise<We2aiApplyOutcome> {
    return await invoke("we2ai_apply_model", {
      tool: request.tool,
      keyId: request.keyId,
      model: request.model,
      claudeSlots: request.claudeSlots ?? null,
      overwrite: request.overwrite ?? false,
      expectedExtraChanges: request.expectedExtraChanges ?? [],
    });
  },

  /** 当前账号可见的全部公告（含已读），创建时间从旧到新。 */
  async listAnnouncements(): Promise<We2aiAnnouncement[]> {
    return (
      (await invoke<We2aiAnnouncement[] | null>("we2ai_list_announcements", {
        unreadOnly: false,
      })) ?? []
    );
  },

  async markAnnouncementRead(id: number): Promise<void> {
    await invoke("we2ai_mark_announcement_read", { id });
  },

  /** 订阅 Rust 后台轮询的"公告有变化"事件，返回取消订阅函数。 */
  async onAnnouncementsChanged(handler: () => void): Promise<() => void> {
    return await listen(WE2AI_ANNOUNCEMENTS_CHANGED_EVENT, () => handler());
  },
};
