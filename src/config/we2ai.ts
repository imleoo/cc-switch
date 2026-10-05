/**
 * WE2AI 模式开关与品牌常量。
 *
 * 恒为 `true`：本 fork 的前端产物只作为 WE2AI 客户端分发，不提供运行时切换
 * 回 CC Switch 的开关。以常量形式存在（而不是直接删除上游代码路径）是为了
 * 让 `App.tsx` 里“渲染 We2aiShell 还是渲染上游视图”只有一处判断，方便上游
 * 同步时核对该分支未被静默还原（见 claudedocs/we2ai-客户端定制方案.md 第 7 节）。
 */
export const WE2AI_MODE = true;

export const WE2AI_BRAND_NAME = "WE2AI";
export const WE2AI_WEBSITE_URL = "https://we2ai.com";
/** 官网注册页：登录页「去官网注册」按钮跳转目标（客户端不提供注册）。 */
export const WE2AI_REGISTER_URL = `${WE2AI_WEBSITE_URL}/register`;
