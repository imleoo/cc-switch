//! WE2AI 定制模块
//!
//! 集中存放 WE2AI 客户端相对上游 cc-switch 的行为差异：模式开关、数据隔离、
//! 启动/退出白名单、IPC 默认拒绝白名单，以及 WE2AI 自有的 Tauri 命令。
//! 详见 `claudedocs/we2ai-客户端定制方案.md` 第 6 节与自定义开发功能列表.md。

pub mod api;
pub mod captcha;
pub mod commands;
pub mod commands_auth;
pub mod keys;
pub mod mode;
pub mod region;
pub mod secret_store;
pub mod session;
