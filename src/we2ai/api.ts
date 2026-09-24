import { invoke } from "@tauri-apps/api/core";

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

export interface We2aiModelView {
  id: string;
  provider: string | null;
  tools: We2aiTool[];
}

/** SubPanel B1 结果：模型、支持的工具与 Key 级准入。 */
export interface We2aiKeyModels {
  models: We2aiModelView[];
  callable: boolean;
  blockedReason: string | null;
}

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
};
