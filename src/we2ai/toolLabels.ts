import type { We2aiTool } from "./api";

/** 三个目标工具的展示名（产品名，不做本地化）。 */
export const WE2AI_TOOL_LABELS: Record<We2aiTool, string> = {
  claude_code: "Claude Code",
  codex: "Codex",
  workbuddy: "WorkBuddy",
};
