/**
 * WE2AI 模式下允许直接 `invoke()` 的上游命令白名单。
 *
 * 必须与 Rust 侧 `src-tauri/src/we2ai/mode.rs` 的 `UPSTREAM_COMMAND_WHITELIST`
 * 逐字保持一致——这是唯一的真实防线（IPC gate），本文件只是前端测试用来验证
 * "We2aiShell 树里发出的每个 invoke() 都落在白名单内"的参照表，不参与运行时
 * 拦截。同步方式：`src-tauri/src/we2ai/mode.rs` 里的
 * `upstream_whitelist_matches_frontend_ipc_whitelist` 单测会在测试期读取并
 * 解析本文件，逐项 diff 两份列表，任何一侧改动而另一侧未同步都会测试失败。
 *
 * 除本列表外，所有 `we2ai_*` 前缀命令也被 gate() 放行（见 mode.rs），前端
 * 判定逻辑见 `isWe2aiIpcAllowed`。
 */
export const WE2AI_UPSTREAM_IPC_WHITELIST: readonly string[] = [
  "get_init_error",
  "get_migration_result",
  "get_skills_migration_result",
  "set_window_theme",
  "get_tool_versions",
  "check_for_updates",
  "install_update_and_restart",
  "restart_app",
  "open_external",
  "get_auto_launch_status",
  "set_auto_launch",
];

export function isWe2aiIpcAllowed(command: string): boolean {
  return (
    command.startsWith("we2ai_") ||
    WE2AI_UPSTREAM_IPC_WHITELIST.includes(command)
  );
}
