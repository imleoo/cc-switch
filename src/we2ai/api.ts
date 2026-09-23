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
 * WE2AI 自有命令的前端封装。P0 阶段只有设置读写——上游 `get_settings` /
 * `save_settings` 在 WE2AI 模式下被 IPC 白名单拒绝，不能复用 `settingsApi`。
 */
export const we2aiApi = {
  async getSettings(): Promise<We2aiSettings> {
    return await invoke("we2ai_get_settings");
  },

  async saveSettings(settings: We2aiSettings): Promise<boolean> {
    return await invoke("we2ai_save_settings", { settings });
  },
};
