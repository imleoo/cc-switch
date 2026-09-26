//! 登录会话管理：单飞 refresh、失败分类、登出四步、会话索引（方案第 5.2 节）。
//!
//! 简化说明：本实现只维护"当前活跃的一个会话"（同一时刻只登录一个
//! `{region, user_id}`），这与桌面客户端 UI 的实际形态一致——顶栏只显示一个
//! 已登录账号，切区域即先登出/切换。方案第 5.2 节"每个 `{region}:{user_id}`
//! 一把会话锁"由此简化为"当前活跃会话共用 `SessionManager` 内部的单把状态
//! 锁"，不引入按 key 的锁表——没有多会话并存的需求，按 key 加锁会是不必要的
//! 复杂度（YAGNI）。

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::future::{BoxFuture, Shared};
use futures::FutureExt;
use serde::{Deserialize, Serialize};

use super::api::{ApiCallError, ApiClient, ErrorCode, LoginResult, TokenPair, UserProfile};
use super::region::Region;
use super::secret_store::{self, SecretStore};

/// 会话索引：`~/.we2ai/session_index.json`，非秘密（`SecretStoreError` 不涉及
/// 此文件）。登录成功时写入，重启时按当前区域查索引再读钥匙串。
///
/// "记住上次选择的区域"**不**存在这个文件里——见 [`LastRegionFile`]（Codex
/// 代码评审第 5 轮高危项 2）：这两者原来共享同一个文件，`set_last_region`
/// 对整份文件做"读—改—存"，如果与登录/登出对同一份索引文件的"读—改—存"
/// 交错执行，后写入的一方会把先写入的一方刚加/删的条目覆盖掉（登录成功
/// 后紧跟着的一次 `set_last_region` 有可能读到登录写盘之前的旧文件内容，
/// 存回去时把刚写的账号条目冲掉）。拆成两个独立文件后，两者不再共享任何
/// 可变状态，这类"读—改—存"竞态从根源上消失，不需要额外加锁或排队。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionIndex {
    #[serde(default)]
    entries: std::collections::HashMap<String, SessionIndexEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionIndexEntry {
    pub user_id: i64,
    pub email_masked: String,
}

impl SessionIndex {
    fn path(data_root: &std::path::Path) -> PathBuf {
        data_root.join("session_index.json")
    }

    pub fn load(data_root: &std::path::Path) -> Self {
        let path = Self::path(data_root);
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// 区分"确认没有索引文件"（`Ok(空索引)`）与"读取或解析失败"（`Err`）。
    /// 用于登录提交的读回校验：[`Self::load`] 把两者都折叠成空索引，读取
    /// 暂时失败会被误判为"没有可被重启恢复的条目"（Codex 验收第 6 轮高危项 1）。
    fn try_load(data_root: &std::path::Path) -> Result<Self, ()> {
        let path = Self::path(data_root);
        match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).map_err(|_| ()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(_) => Err(()),
        }
    }

    /// 原子写入（临时文件 + rename），避免进程崩溃/断电导致文件半写；Unix
    /// 上固定 0600（本文件虽然"非秘密"，但含 `email_masked` 等账号相关信息，
    /// 复用上游已有的 `atomic_write_private` 而不是自己再写一遍原子写逻辑，
    /// 顺带把权限收紧，Codex 代码评审低危项 2）。
    fn save(&self, data_root: &std::path::Path) -> Result<(), crate::error::AppError> {
        let path = Self::path(data_root);
        let json = serde_json::to_string_pretty(self).unwrap_or_default();
        crate::config::atomic_write_private(&path, json.as_bytes())
    }

    pub fn get(&self, region: Region) -> Option<&SessionIndexEntry> {
        self.entries.get(region.storage_key())
    }

    fn set(&mut self, region: Region, entry: SessionIndexEntry) {
        self.entries.insert(region.storage_key().to_string(), entry);
    }

    fn remove(&mut self, region: Region) {
        self.entries.remove(region.storage_key());
    }
}

/// 登录页"记住上次选择的区域"，`~/.we2ai/last_region.json`，与
/// [`SessionIndex`] 完全独立的文件——独立于是否有活跃会话（用户可能选了
/// 区域但还没登录成功就退出应用），也不参与 `commit_lock` 保护的会话身份/
/// 持久化提交流程：它只是一个 UI 便利性记忆，不影响任何安全性判断。
/// `we2ai_get_last_region`/`we2ai_set_last_region` 命令仍是同步命令、直接
/// 同步文件 I/O——单个几十字节的原子写入不构成有意义的阻塞，拆分文件已经
/// 消除了原来真正的问题（读—改—存竞态），额外为这两个命令引入
/// `async`+`spawn_blocking` 不会带来实质收益（Codex 代码评审第 5 轮高危项
/// 2，二选一：选择"拆独立文件"）。
struct LastRegionFile;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LastRegionData {
    region: String,
}

impl LastRegionFile {
    fn path(data_root: &std::path::Path) -> PathBuf {
        data_root.join("last_region.json")
    }

    fn load(data_root: &std::path::Path) -> Option<Region> {
        let path = Self::path(data_root);
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<LastRegionData>(&s).ok())
            .and_then(|d| Region::from_storage_key(&d.region))
    }

    fn save(data_root: &std::path::Path, region: Region) -> Result<(), crate::error::AppError> {
        let path = Self::path(data_root);
        let data = LastRegionData {
            region: region.storage_key().to_string(),
        };
        let json = serde_json::to_string_pretty(&data).unwrap_or_default();
        crate::config::atomic_write_private(&path, json.as_bytes())
    }
}

/// 邮箱脱敏：`ab****@domain.com`，保留前两个字符（不足两个字符时保留一个）。
pub fn mask_email(email: &str) -> String {
    let Some((local, domain)) = email.split_once('@') else {
        return "***".to_string();
    };
    let keep = local.chars().count().clamp(1, 2);
    let visible: String = local.chars().take(keep).collect();
    format!("{visible}****@{domain}")
}

#[derive(Debug, Clone)]
struct ActiveSession {
    region: Region,
    user_id: i64,
    email_masked: String,
    /// 内存缓存的 refresh token（源头是钥匙串；每次轮转都会同步写钥匙串，见
    /// [`SessionManager::persist_rotated_token`]）。
    refresh_token: String,
    access_token: String,
    /// 钥匙串在登录 / 上一次轮转时是否写入成功；为 `false` 表示已退化为
    /// 本次会话内存保存，前端应提示"重启后需要重新登录"。
    keyring_degraded: bool,
    /// 登录成功时会话索引（`~/.we2ai/session_index.json`）是否写入成功；为
    /// `false` 表示这次登录本身在钥匙串里是好的，但没有任何索引条目指向
    /// 它——下次启动 `resume_session` 找不到索引条目，只能回登录页，跟
    /// `keyring_degraded` 一样都是"仅本次运行内有效、不可持久恢复"（Codex
    /// 代码评审第 3 轮高危项 2）。
    index_degraded: bool,
    generation: u64,
}

#[derive(Debug, Clone)]
enum SessionState {
    LoggedOut,
    Active(ActiveSession),
    /// 登出流程进行中：拒绝新的 refresh，仍持有登出请求需要用到的 refresh_token。
    LoggingOut(ActiveSession),
}

/// 一次操作发起时捕获的"身份快照"：generation + region。所有会在后续
/// 某个时间点"重新读取当前会话状态、决定是否继续（重放/提交/清理）"的
/// 操作，都应该在发起时捕获它，并且此后统一用它做前置校验——不能拿
/// "当前状态"直接替代"发起时打算操作的那个会话"，哪怕两者恰好都是
/// `Active`（Codex 代码评审第 6 轮高危项 1/2：统一用发起时快照的
/// generation + 区域，一次性覆盖"跨账号/跨区域重放"“登出中被恢复覆盖”
/// 这类同源问题，而不是逐个症状打补丁）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OperationContext {
    generation: u64,
    region: Region,
}

/// 对外暴露的会话身份：区域 + 用户 + 代次。代次在每次登录、恢复、登出时
/// 变化，两个身份相等即表示同一次会话。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionIdentity {
    pub region: Region,
    pub user_id: i64,
    pub generation: u64,
}

/// 会话失败分类（方案第 5.2 节"失败分类"表）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// 会话被终止（`TOKEN_REVOKED` / `INVALID_TOKEN` / `USER_NOT_ACTIVE` /
    /// `BACKEND_MODE_ACTIVE` / 未知 401 / 各类 `REFRESH_TOKEN_*` 等），
    /// 携带触发终止的错误码，供 UI 展示。
    Terminated(String),
    /// 网络错误 / 429 / 5xx：保留凭证，调用方可退避重试。
    Transient(String),
    /// 钥匙串写入失败：进入"需要重新登录"（不同于 Terminated——会话在内存中
    /// 已失去有效凭证，但没有明确的服务端撤销事件）。
    NeedsRelogin,
    /// 当前没有活跃会话。
    NoActiveSession,
    /// 这次调用发起时绑定的会话身份（generation + region）已经不再是
    /// "当前"的了——账号或区域在调用进行期间被切换/登出（Codex 代码评审
    /// 第 6 轮高危项 1）。调用方必须放弃这次调用，绝不能拿"当前"会话的
    /// token 去重放一个发往*旧*身份/区域 API 的请求；应当视为调用失败，
    /// 让上层（前端）按需重新发起。
    SessionChanged,
    /// 登录本身成功（服务端已经签发新的 token 对），但无法安全地把它保存
    /// 下来——具体是"新账号钥匙串写入之后，会话索引持续写入失败，且旧账号
    /// 的钥匙串条目也无法作废"（Codex 代码评审第 4 轮结构性重构：索引持续
    /// 写失败的处理）。这种情况下不能报告登录成功，因为重启后无法保证不会
    /// 误恢复旧账号；调用方应当把这次新签发的 refresh_token 视为已撤销。
    PersistFailed(String),
    /// 其他业务失败（非 401、非已知终止码），不影响会话状态。
    Other(String),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Terminated(code) => write!(f, "会话已失效: {code}"),
            SessionError::Transient(msg) => write!(f, "请求失败，可重试: {msg}"),
            SessionError::NeedsRelogin => write!(f, "需要重新登录"),
            SessionError::NoActiveSession => write!(f, "尚未登录"),
            SessionError::SessionChanged => write!(f, "会话已切换，操作已放弃"),
            SessionError::PersistFailed(msg) => write!(f, "无法保存登录状态: {msg}"),
            SessionError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for SessionError {}

/// 受保护接口失败分类结果。
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProtectedOutcome {
    RetryAfterRefresh,
    Terminate(String),
    RetryWithBackoff,
    Other(String),
}

/// `/auth/refresh` 失败分类结果。
#[derive(Debug, Clone, PartialEq, Eq)]
enum RefreshOutcome {
    Terminate(String),
    RetryWithBackoff,
    /// 未知的非 401 业务失败（如无 reason 的 400）：按方案第 3.1 节"其余按
    /// 普通请求失败"，保留会话与凭据，只把错误上抛（Codex 验收第 5 轮中危项）。
    Failed(String),
}

const PROTECTED_TERMINATE_CODES: &[&str] = &[
    "TOKEN_REVOKED",
    "INVALID_TOKEN",
    "USER_NOT_ACTIVE",
    "BACKEND_MODE_ACTIVE",
];

/// `EMPTY_TOKEN`（SubPanel `jwt_auth.go`：`Authorization: Bearer` 后面是空
/// 字符串时的错误码）按"需刷新"类处理，而不是当成未知 401 终止会话——
/// `ResumeOutcome::OfflineRetained` 恢复出的会话在成功刷新前 access_token
/// 本就是空字符串（Codex 代码评审中危项 2），`call_protected` 已经在发请求
/// 前主动检测空 token 并提前刷新，这里额外把该错误码归类是防御性兜底（例如
/// 未来某个受保护调用点绕过了 `call_protected` 的空 token 前置检查）。
const PROTECTED_RETRY_AFTER_REFRESH_CODES: &[&str] =
    &["TOKEN_EXPIRED", "ACCESS_TOKEN_EXPIRED", "EMPTY_TOKEN"];

const REFRESH_TERMINATE_CODES: &[&str] = &[
    "REFRESH_TOKEN_INVALID",
    "REFRESH_TOKEN_EXPIRED",
    "REFRESH_TOKEN_REUSED",
    "TOKEN_REVOKED",
    "SESSION_BINDING_MISMATCH",
    "USER_NOT_ACTIVE",
    "BACKEND_MODE_ACTIVE",
];

fn classify_protected_error(err: &ApiCallError) -> ProtectedOutcome {
    match err {
        ApiCallError::Network(_) => ProtectedOutcome::RetryWithBackoff,
        ApiCallError::Api(api_err) => {
            if api_err.status == 429 || api_err.status >= 500 {
                return ProtectedOutcome::RetryWithBackoff;
            }
            match &api_err.code {
                ErrorCode::Named(code) => {
                    if PROTECTED_RETRY_AFTER_REFRESH_CODES.contains(&code.as_str()) {
                        ProtectedOutcome::RetryAfterRefresh
                    } else if PROTECTED_TERMINATE_CODES.contains(&code.as_str()) {
                        ProtectedOutcome::Terminate(code.clone())
                    } else if api_err.status == 401 {
                        // 其他未知 401 按终止会话处理。
                        ProtectedOutcome::Terminate(code.clone())
                    } else {
                        ProtectedOutcome::Other(code.clone())
                    }
                }
                ErrorCode::Status(401) => ProtectedOutcome::Terminate("UNKNOWN_401".to_string()),
                ErrorCode::Status(status) => ProtectedOutcome::Other(status.to_string()),
            }
        }
    }
}

fn classify_refresh_error(err: &ApiCallError) -> RefreshOutcome {
    match err {
        ApiCallError::Network(_) => RefreshOutcome::RetryWithBackoff,
        ApiCallError::Api(api_err) => {
            if api_err.status == 429 || api_err.status >= 500 {
                return RefreshOutcome::RetryWithBackoff;
            }
            match &api_err.code {
                // 方案第 5.2 节列出的 `/auth/refresh` 终止码；未在清单内的具名
                // 错误码同样终止会话（与"其他未知 401 按终止会话处理"的通用
                // 规则一致——refresh 接口没有"重试后继续用旧 token"这条路）。
                ErrorCode::Named(code) if REFRESH_TERMINATE_CODES.contains(&code.as_str()) => {
                    RefreshOutcome::Terminate(code.clone())
                }
                // 清单外的具名码与无业务码的响应：只有 401 终止会话（方案
                // 第 3.1 节"401 按未知 401 终止会话，其余按普通请求失败"），
                // 其他 4xx 保留会话，避免一次参数类错误把用户踢出登录。
                ErrorCode::Named(code) if api_err.status == 401 => {
                    RefreshOutcome::Terminate(code.clone())
                }
                ErrorCode::Status(401) => {
                    RefreshOutcome::Terminate("UNKNOWN_REFRESH_FAILURE".to_string())
                }
                ErrorCode::Named(code) => RefreshOutcome::Failed(code.clone()),
                ErrorCode::Status(status) => RefreshOutcome::Failed(status.to_string()),
            }
        }
    }
}

/// 登出结果（方案第 5.2 节：仅 `revoked:true` 才显示"已退出"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogoutOutcome {
    Revoked,
    /// 网络失败 / `revoked:false` / B4 未部署，但本地清理（索引/钥匙串）
    /// 至少有一项成功——重启不会恢复这个会话，只是远端撤销未确认。
    LocalOnly,
    /// 远端未确认撤销，且本地清理（索引 + 钥匙串）两项全部失败——不能
    /// 保证重启不会恢复这个会话。前端不能提示"已退出"，必须提示清理失败
    /// 并提供重试（Codex 代码评审第 5 轮高危项 3）。
    LocalCleanupFailed,
    NotLoggedIn,
}

/// 应用启动时尝试恢复会话的结果（Codex 代码评审高危项 5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeOutcome {
    /// 成功刷新出新的 access token，会话可用。
    Restored,
    /// 没有可恢复的会话（索引没记录 / 钥匙串没有对应条目 / refresh 本身被
    /// 服务端拒绝），应回登录页。
    NeedLogin,
    /// 恢复出的会话在刷新阶段遇到网络错误：内存状态原样保留（不清钥匙串、
    /// 不清索引），前端应保持"已登录"界面并提示离线，而不是回登录页。
    OfflineRetained,
}

/// [`SessionManager::persist_rotated_token`] 的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PersistOutcome {
    /// 写入钥匙串成功，或该会话本就已退化为内存模式（视为预期内的"写入"）。
    Written,
    /// generation 不匹配：会话已经被别的操作推进，这次写入是迟到的，原样
    /// 丢弃，不改动任何状态。
    Stale,
    /// 会话原本是钥匙串正常模式，这次写入真的失败了。
    WriteFailed,
}

/// [`SessionManager::wipe_persisted_credentials`] 的清理结果（Codex 代码
/// 评审第 5 轮高危项 3）。
#[derive(Debug, Clone, Copy)]
struct PersistedCredentialsCleanup {
    /// 确认该区域没有索引项（读取成功且无条目）。读取失败不算。
    index_absent: bool,
    /// 该区域索引项指向这个 user_id，且已成功删除并落盘。
    index_entry_removed: bool,
    keyring_removed: bool,
}

impl PersistedCredentialsCleanup {
    /// 登出/终止当前会话时的判定：确认无索引、删掉了指向它的索引项、或删掉
    /// 了钥匙串条目，三者任一即可保证重启 `resume_session` 走 `NeedLogin`。
    /// 索引**读取失败**不属于任何一项（Codex 验收第 8 轮高危项 1）。
    fn reliably_invalidated(&self) -> bool {
        self.index_absent || self.index_entry_removed || self.keyring_removed
    }

    /// 待清理旧账号的移出条件：只有"删掉了指向它的索引项"或"删掉了它的
    /// 钥匙串条目"才算。索引不指向它（包括无条目、已被新账号占用）时，
    /// 旧 refresh 仍在钥匙串里，必须靠删除钥匙串判定完成（Codex 验收第 8 轮
    /// 高危项 2）。
    fn clears_pending(&self) -> bool {
        self.index_entry_removed || self.keyring_removed
    }
}

/// 登出或终止后是否登记待清理目标。
///
/// - 远端确认撤销：残留凭据已失效，不登记。
/// - 重启可能恢复（三项清理全失败）：必须登记。
/// - 索引已作废但钥匙串删除失败：服务端仍有效的 refresh 残留在钥匙串里，
///   也登记，给出提示与重试入口（Fable 终验中危项 2）。例外是会话本身处于
///   钥匙串退化状态（登录或轮转时就写不进钥匙串，典型是平台没有可用钥匙串）：
///   此时删除同样会失败，登记只会留下一个永远重试不掉的提示；重启安全已由
///   索引作废保证。
fn needs_pending_cleanup(
    cleanup: &PersistedCredentialsCleanup,
    remote_revoked: bool,
    keyring_degraded: bool,
) -> bool {
    if remote_revoked {
        return false;
    }
    if !cleanup.reliably_invalidated() {
        return true;
    }
    !cleanup.keyring_removed && !keyring_degraded
}

/// 当前会话状态摘要，供前端展示（不含任何令牌）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub logged_in: bool,
    pub region: Option<String>,
    pub email_masked: Option<String>,
    pub keyring_degraded: bool,
    /// 见 [`ActiveSession::index_degraded`]：会话索引写入失败，这次登录只在
    /// 当前进程内存里有效，前端应提示"登录状态无法保存，下次启动需要重新
    /// 登录"（Codex 代码评审第 3 轮高危项 2）。
    pub index_degraded: bool,
    /// 断网自动重试的下次尝试倒计时（秒），不在重试状态时为 `None`。前端用
    /// 它展示离线横幅的倒计时（方案 5.2 节"断网不回登录页"，Codex 代码
    /// 评审中危项 3：自动退避重试）。
    pub offline_retry_in_seconds: Option<u64>,
    /// 上一次登出或会话终止后，本机保存的索引与钥匙串都未能清除
    /// （记录在 `Inner::pending_cleanups`）：重启可能恢复该会话。前端据此展示提示并
    /// 提供 `we2ai_retry_local_cleanup` 重试入口，即使当前处于未登录界面
    /// （Codex 验收第 5 轮高危项 2）。
    pub local_cleanup_pending: bool,
}

/// 指数退避重试的等待时长：1s、2s、4s、……，第 6 次（`attempt=6`）起封顶
/// 60s（`2^6=64` 被截到 60）。`attempt` 从 0 开始。
fn backoff_delay(attempt: u32) -> Duration {
    let secs = 1u64.checked_shl(attempt.min(6)).unwrap_or(60);
    Duration::from_secs(secs.min(60))
}

/// 受保护接口自身的快速重试次数上限：只做 2 次快速重试（`backoff_delay(0)`
/// =1s、`backoff_delay(1)`=2s），之后把最后一次错误当作 `Transient` 直接
/// 上抛。
///
/// Codex 代码评审第 3 轮中危项 1：原先这里是 7 次（对应 1+2+4+8+16+32+60=123
/// 秒的退避总等待时间），叠加每次请求本身 20 秒的超时（见 `api.rs` 的
/// `reqwest::Client` 构造），断网时最坏要 8×20s + 123s ≈ 4.7 分钟才会让一次
/// `invoke` 返回——前端等于被挂起近 5 分钟。真正的长时间断网恢复交给
/// `SessionManager` 的后台自动退避循环（`start_offline_retry`/
/// `run_offline_retry_loop`，仍然用完整的 `backoff_delay` 序列且没有总次数
/// 上限），一次 `call_protected` 调用只负责应对"抖动式"的短暂失败，快速
/// 放弃并把状态交回调用方（离线横幅 + 后台循环）。
const PROTECTED_FAST_RETRY_ATTEMPTS: u32 = 2;

type RefreshFuture = Shared<BoxFuture<'static, Result<TokenPair, SessionError>>>;

/// 断网自动重试的后台循环状态。`loop_id` 用来在"停止后立即重新开始"的
/// 时序下区分新旧循环（Codex 代码评审第 4 轮中危项 2）：`run_offline_retry_loop`
/// 每次醒来、每次准备推进/清空状态之前，都要确认自己的 `loop_id` 仍然是
/// `offline_retry` 里记录的那个，不是就直接退出，绝不代替一个更新的循环
/// 继续工作或清空它的状态。
struct OfflineRetryState {
    loop_id: u64,
    attempt: u32,
    next_attempt_at: Instant,
}

struct Inner {
    secret_store: Arc<dyn SecretStore>,
    data_root: PathBuf,
    app_version: String,
    /// 只读快照：`summary()`、`call_protected()` 取 access token 等所有读
    /// 路径只 `.lock().unwrap().clone()` 这个 `Arc` 指针，从不在持有这把锁
    /// 时做任何 I/O 或 `.await`——因此永远不会被钥匙串/索引 I/O 卡住（Codex
    /// 代码评审第 4 轮结构性重构：读写分离）。写路径（提交）只在真正要发布
    /// 新状态的那一刻整体替换这个 `Arc`。
    snapshot: Mutex<Arc<SessionState>>,
    /// 提交锁：所有改变会话持久化或身份的操作——登录/2FA/手机登录提交、
    /// 区域恢复/切换、refresh 结果落盘、登出清理、generation 匹配的终止
    /// 清理、索引降级处理——都必须持有它才能真正"发布新快照"。用
    /// `tokio::sync::Mutex` 而不是 `std::sync::Mutex`，因为需要跨
    /// `spawn_blocking().await` 持有；持有它不会阻塞任何读路径（读路径
    /// 根本不碰这把锁），只会串行化"会改变身份/持久化"的操作彼此之间的
    /// 提交顺序。
    commit_lock: tokio::sync::Mutex<()>,
    /// `(attempt_id, future)`。`attempt_id` 用来在清空槽位时确认"这仍是我
    /// 发起/加入的那次 refresh"，而不是盲目置 `None`——Codex 代码评审高危项
    /// 4：若每个等待者都在自己的 `await` 之后无条件清空，一个刚完成的旧
    /// future 的清空动作可能会把另一个并发调用者刚刚放进去的新 future 顶掉，
    /// 导致同一批并发请求触发两次实际的 `/auth/refresh`（B5 下会被判定为
    /// token 复用，撤销整个家族）。
    refresh_inflight: Mutex<Option<(u64, RefreshFuture)>>,
    refresh_attempt_counter: AtomicU64,
    generation_counter: AtomicU64,
    /// 登录（含 2FA、手机登录）与区域恢复/切换共享的操作代次（operation
    /// epoch）。严格递增；领取代次号本身也必须在 `commit_lock` 内进行
    /// （见 [`SessionManager::begin_login_epoch`]），只有在真正提交那一刻
    /// 代次号仍然等于"当前已发出的最新代次"，这次操作才被允许提交，否则
    /// 视为过期丢弃（Codex 代码评审第 3/4 轮高危项 1：区域 A 登录在途时
    /// 切到区域 B——即便 B 没有可恢复的会话、直接 `NeedLogin`——也必须让 A
    /// 的登录结果作废，而不是覆盖已经生效的 B 状态）。
    login_epoch: AtomicU64,
    /// 本地凭据清理代次：每次 [`SessionManager::wipe_persisted_credentials`]
    /// （登出④、会话终止、重试清理，均在 `commit_lock` 内）递增。
    /// `resume_session` 在锁外读索引与钥匙串之前记下它，提交时不相等说明
    /// 读到的凭据可能已被清掉，放弃提交，避免已登出的会话被旧读取复活
    /// （Fable 终验中危项 1）。不复用 `login_epoch`：那会让与登出并发的
    /// 正常登录也被作废。
    wipe_epoch: AtomicU64,
    /// 断网自动重试状态（Codex 代码评审中危项 3）。`None` 表示当前没有在
    /// 后台重试；`Some` 时后台有一个 `run_offline_retry_loop` 任务在跑。
    offline_retry: Mutex<Option<OfflineRetryState>>,
    offline_retry_loop_counter: AtomicU64,
    /// 测试专用：覆盖 `api_for()` 使用的基址，指向 `wiremock::MockServer`，
    /// 从而能对 refresh/logout 等流程做真实 HTTP 往返的端到端断言，而不是只
    /// 测分类纯函数。生产路径下恒为 `None`，且没有公开方法能在生产构建下设置
    /// 它（setter 本身 `#[cfg(test)]`）。
    #[cfg(test)]
    base_url_override: Mutex<Option<String>>,
    /// 测试专用：强制接下来的 N 次 [`SessionIndex::save`] 调用失败，用于
    /// 确定性地复现"会话索引写入失败"这类真实环境里偶发的磁盘错误（Codex
    /// 代码评审第 3 轮高危项 2 的回归测试）。
    #[cfg(test)]
    force_index_save_failures: AtomicU64,
    /// 测试专用：登录提交的可控放行点。设置后，下一次 `commit_new_session`
    /// 在网络阶段结束、获取 `commit_lock` 之前通知"已到达"并等待测试放行，
    /// 使交错测试能在这段窗口里执行**真实的**另一次登录/切换，而不依赖
    /// 响应延迟（wiremock 单线程运行时无法在 `respond()` 内阻塞放行；
    /// Codex 验收第 5 轮中危项）。
    #[cfg(test)]
    commit_gate: Mutex<Option<CommitGate>>,
    /// 本地清理（索引与钥匙串）两项都失败、尚待重试的会话，按区域记录
    /// `region → user_id`。独立于当前会话状态保存：此前放在
    /// 已移除的 `SessionState::LogoutCleanupPending` 状态里，登录或恢复其他区域一发布
    /// `Active` 就把它覆盖，待清理目标随之丢失（Codex 验收第 6 轮高危项 2）。
    /// 只在持有 `commit_lock` 时修改。
    pending_cleanups: Mutex<HashMap<Region, i64>>,
    /// 测试专用：强制接下来的 N 次索引读回（`try_load`）失败。
    #[cfg(test)]
    force_index_read_failures: AtomicU64,
    /// 测试专用：覆盖 [`SessionManager::ensure_refreshed_with_backoff`] 的
    /// 总时限（生产恒为 30 秒）。真实的 30 秒太长，测试没法在合理时间内
    /// 断言"超过截止时间后不再发起新的重试"；设置为更短的值（如 1 秒），
    /// 配合足够长的 mock 响应延迟（如 3 秒），既能验证截止时间检查确实
    /// 生效，又能验证它只在"发起下一次重试之前"检查、绝不取消正在进行中
    /// 的请求（Opus 复核低危项 S5）。生产路径下恒为 `None`，且没有公开
    /// 方法能在生产构建下设置它（setter 本身 `#[cfg(test)]`）。
    #[cfg(test)]
    refresh_backoff_deadline_override: Mutex<Option<Duration>>,
}

#[cfg(test)]
struct CommitGate {
    reached: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

/// 会话管理器。廉价 `Clone`（内部 `Arc`），可安全地在多个 Tauri 命令 /
/// 异步任务之间共享。
#[derive(Clone)]
pub struct SessionManager(Arc<Inner>);

/// Tauri managed state 包装。
pub struct We2aiSessionState(pub SessionManager);

/// 在阻塞线程池上执行一次钥匙串读取，避免占用 tokio 工作线程（真实系统
/// 钥匙串可能弹出授权对话框、阻塞数十秒甚至更久——Codex 代码评审第 4 轮
/// 中危项 1）。返回 `None` 表示条目不存在或读取失败，调用方（`resume_session`）
/// 对这两种情况的处理完全一致。
async fn keyring_get(store: Arc<dyn SecretStore>, account: String) -> Option<String> {
    tokio::task::spawn_blocking(move || {
        store
            .get(secret_store::SERVICE_NAME, &account)
            .ok()
            .flatten()
    })
    .await
    .expect("blocking keyring get task panicked")
}

/// 三态读取：`Ok(Some)` 有值、`Ok(None)` 确认不存在、`Err` 读取失败（钥匙串
/// 锁定或不可用）。用于"重启会恢复成什么"的读回校验——那里必须区分"确认
/// 不存在"与"现在读不到但重启后可能读到"，[`keyring_get`] 把两者合并成
/// `None`，不能用于这个判断。
async fn keyring_read(store: Arc<dyn SecretStore>, account: String) -> Result<Option<String>, ()> {
    tokio::task::spawn_blocking(move || {
        store
            .get(secret_store::SERVICE_NAME, &account)
            .map_err(|_| ())
    })
    .await
    .expect("blocking keyring read task panicked")
}

/// 同上，写入。
async fn keyring_set(store: Arc<dyn SecretStore>, account: String, secret: String) -> bool {
    tokio::task::spawn_blocking(move || {
        store
            .set(secret_store::SERVICE_NAME, &account, &secret)
            .is_ok()
    })
    .await
    .expect("blocking keyring set task panicked")
}

/// 同上，删除（幂等：条目不存在也算成功，见 [`SecretStore::delete`]）。
async fn keyring_delete(store: Arc<dyn SecretStore>, account: String) -> bool {
    tokio::task::spawn_blocking(move || store.delete(secret_store::SERVICE_NAME, &account).is_ok())
        .await
        .expect("blocking keyring delete task panicked")
}

impl SessionManager {
    pub fn new(
        secret_store: Arc<dyn SecretStore>,
        data_root: PathBuf,
        app_version: String,
    ) -> Self {
        Self(Arc::new(Inner {
            secret_store,
            data_root,
            app_version,
            snapshot: Mutex::new(Arc::new(SessionState::LoggedOut)),
            commit_lock: tokio::sync::Mutex::new(()),
            refresh_inflight: Mutex::new(None),
            refresh_attempt_counter: AtomicU64::new(0),
            generation_counter: AtomicU64::new(0),
            login_epoch: AtomicU64::new(0),
            wipe_epoch: AtomicU64::new(0),
            offline_retry: Mutex::new(None),
            offline_retry_loop_counter: AtomicU64::new(0),
            #[cfg(test)]
            base_url_override: Mutex::new(None),
            #[cfg(test)]
            force_index_save_failures: AtomicU64::new(0),
            #[cfg(test)]
            commit_gate: Mutex::new(None),
            pending_cleanups: Mutex::new(HashMap::new()),
            #[cfg(test)]
            force_index_read_failures: AtomicU64::new(0),
            #[cfg(test)]
            refresh_backoff_deadline_override: Mutex::new(None),
        }))
    }

    fn api_for(&self, region: Region) -> ApiClient {
        #[cfg(test)]
        {
            if let Some(base) = self.0.base_url_override.lock().unwrap().clone() {
                return ApiClient::new_with_base_url(region, &self.0.app_version, base);
            }
        }
        ApiClient::new(region, &self.0.app_version)
    }

    /// 测试专用：见 [`Inner::refresh_backoff_deadline_override`] 的说明。
    #[cfg(test)]
    pub(crate) fn set_test_refresh_backoff_deadline(&self, deadline: Duration) {
        *self.0.refresh_backoff_deadline_override.lock().unwrap() = Some(deadline);
    }

    /// 测试专用：见 [`Inner::base_url_override`] 的说明。
    #[cfg(test)]
    pub(crate) fn set_test_base_url_override(&self, base_url: String) {
        *self.0.base_url_override.lock().unwrap() = Some(base_url);
    }

    /// 测试专用：见 [`Inner::commit_gate`]。返回 (已到达信号, 放行句柄)。
    #[cfg(test)]
    pub(crate) fn set_test_commit_gate(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *self.0.commit_gate.lock().unwrap() = Some(CommitGate {
            reached: reached_tx,
            release: release_rx,
        });
        (reached_rx, release_tx)
    }

    /// 测试专用：见 [`Inner::force_index_read_failures`]。
    #[cfg(test)]
    pub(crate) fn set_test_force_index_read_failures(&self, n: u64) {
        self.0.force_index_read_failures.store(n, Ordering::SeqCst);
    }

    /// 测试专用：见 [`Inner::force_index_save_failures`] 的说明。
    #[cfg(test)]
    pub(crate) fn set_test_force_index_save_failures(&self, n: u64) {
        self.0.force_index_save_failures.store(n, Ordering::SeqCst);
    }

    fn next_generation(&self) -> u64 {
        self.0.generation_counter.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// 读取当前快照（只读，永远不阻塞——见 [`Inner::snapshot`] 的文档注释）。
    fn current_state(&self) -> Arc<SessionState> {
        self.0.snapshot.lock().unwrap().clone()
    }

    /// 发布新状态：整体替换快照指针。调用方必须已经持有 `commit_lock`
    /// （这个方法本身不去拿 `commit_lock`，因为它总是作为某次提交的最后
    /// 一步，和同一次提交里的其他步骤共享同一次持有）。
    fn publish(&self, new_state: SessionState) {
        *self.0.snapshot.lock().unwrap() = Arc::new(new_state);
    }

    /// 领取一个新的登录/恢复操作代次号，在 `commit_lock` 内进行——这一步
    /// 本身就是"提交"的一部分（Codex 代码评审第 4 轮）：哪怕这次操作最终
    /// 无事可提交（比如目标区域没有索引，直接 `NeedLogin`），领号这个动作
    /// 也已经让更早发起、仍在网络在途的登录/恢复操作作废。
    async fn begin_login_epoch(&self) -> u64 {
        let _guard = self.0.commit_lock.lock().await;
        self.0.login_epoch.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// 加载会话索引，通过 `spawn_blocking` 执行（同步文件 I/O，避免占用
    /// tokio 工作线程）。
    /// 读回校验用的三态索引读取，见 [`SessionIndex::try_load`]。
    async fn load_index_checked(&self) -> Result<SessionIndex, ()> {
        #[cfg(test)]
        {
            let remaining = self.0.force_index_read_failures.load(Ordering::SeqCst);
            if remaining > 0 {
                self.0
                    .force_index_read_failures
                    .fetch_sub(1, Ordering::SeqCst);
                return Err(());
            }
        }
        let data_root = self.0.data_root.clone();
        tokio::task::spawn_blocking(move || SessionIndex::try_load(&data_root))
            .await
            .expect("blocking index load task panicked")
    }

    fn has_pending_cleanup(&self) -> bool {
        !self.0.pending_cleanups.lock().unwrap().is_empty()
    }

    async fn load_index(&self) -> SessionIndex {
        let data_root = self.0.data_root.clone();
        tokio::task::spawn_blocking(move || SessionIndex::load(&data_root))
            .await
            .expect("blocking index load task panicked")
    }

    /// 保存会话索引，测试构建下可以被 [`Self::set_test_force_index_save_failures`]
    /// 强制判定为失败，用来确定性地复现磁盘/权限错误这类真实环境里的偶发
    /// 故障（Codex 代码评审第 3 轮高危项 2）。真正的写入通过 `spawn_blocking`
    /// 执行。
    async fn index_save(&self, index: SessionIndex) -> bool {
        #[cfg(test)]
        {
            let remaining = self.0.force_index_save_failures.load(Ordering::SeqCst);
            if remaining > 0 {
                self.0
                    .force_index_save_failures
                    .fetch_sub(1, Ordering::SeqCst);
                return false;
            }
        }
        let data_root = self.0.data_root.clone();
        tokio::task::spawn_blocking(move || index.save(&data_root).is_ok())
            .await
            .expect("blocking index save task panicked")
    }

    /// 当前会话摘要，供 `we2ai_session_status` 命令使用。只读快照 +
    /// `offline_retry` 的瞬时计算，不涉及任何锁竞争或 I/O，保证不会被
    /// 钥匙串/索引操作卡住（Codex 代码评审第 4 轮中危项 1）。
    pub fn summary(&self) -> SessionSummary {
        let offline_retry_in_seconds = self.offline_retry_in_seconds();
        match &*self.current_state() {
            SessionState::Active(s) | SessionState::LoggingOut(s) => SessionSummary {
                logged_in: true,
                region: Some(s.region.storage_key().to_string()),
                email_masked: Some(s.email_masked.clone()),
                keyring_degraded: s.keyring_degraded,
                index_degraded: s.index_degraded,
                offline_retry_in_seconds,
                local_cleanup_pending: self.has_pending_cleanup(),
            },
            SessionState::LoggedOut => SessionSummary {
                logged_in: false,
                region: None,
                email_masked: None,
                keyring_degraded: false,
                index_degraded: false,
                offline_retry_in_seconds: None,
                local_cleanup_pending: self.has_pending_cleanup(),
            },
        }
    }

    /// 断网自动重试的下次尝试倒计时（秒），不在重试状态时为 `None`。
    fn offline_retry_in_seconds(&self) -> Option<u64> {
        let guard = self.0.offline_retry.lock().unwrap();
        guard.as_ref().map(|s| {
            let now = Instant::now();
            if s.next_attempt_at > now {
                // 向上取整：调度时设的是"整 1 秒之后"，但查询这一刻必然已经
                // 过去几微秒，`.as_secs()` 直接截断会把刚进入离线状态那一瞬间
                // 显示成 "0 秒后重试"，向用户传达了错误的即将信息。
                (s.next_attempt_at - now).as_secs_f64().ceil() as u64
            } else {
                0
            }
        })
    }

    /// 启动断网自动重试后台循环（如果还没在跑）。分配一个新的 `loop_id`。
    fn start_offline_retry(&self) {
        let mut guard = self.0.offline_retry.lock().unwrap();
        if guard.is_some() {
            // 已经有一个循环在跑，不重复启动。
            return;
        }
        let loop_id = self
            .0
            .offline_retry_loop_counter
            .fetch_add(1, Ordering::SeqCst)
            + 1;
        *guard = Some(OfflineRetryState {
            loop_id,
            attempt: 0,
            next_attempt_at: Instant::now() + backoff_delay(0),
        });
        drop(guard);
        let manager = self.clone();
        tokio::spawn(async move { manager.run_offline_retry_loop(loop_id).await });
    }

    /// 停止断网自动重试（成功恢复 / 判定为终止会话 / 用户登出时调用）。
    fn clear_offline_retry(&self) {
        *self.0.offline_retry.lock().unwrap() = None;
    }

    /// 同上，但只有当前记录的循环仍然是 `my_id` 才清空——`run_offline_retry_loop`
    /// 在 `ensure_refreshed().await` 完成之后（成功或遇到终止性错误）用这个
    /// 而不是无条件的 [`Self::clear_offline_retry`]，否则一次耗时较长的旧
    /// 循环调用完成时，可能会清掉在它等待期间已经启动的新循环的状态
    /// （Codex 代码评审第 5 轮中危项 4）。
    fn clear_offline_retry_if_current(&self, my_id: u64) {
        let mut guard = self.0.offline_retry.lock().unwrap();
        if matches!(guard.as_ref(), Some(s) if s.loop_id == my_id) {
            *guard = None;
        }
    }

    /// `my_id` 是启动时分配的 `loop_id`；每次醒来、每次准备推进/清空状态
    /// 之前都要确认自己仍然是"当前"这个循环，不是就直接退出——避免"停止后
    /// 立即重新开始"时，旧循环醒来后继续操作新循环的状态，出现两个循环
    /// 同时生效的情况（Codex 代码评审第 4 轮中危项 2）。
    async fn run_offline_retry_loop(self, my_id: u64) {
        loop {
            let delay = {
                let guard = self.0.offline_retry.lock().unwrap();
                match guard.as_ref() {
                    Some(s) if s.loop_id == my_id => {
                        s.next_attempt_at.saturating_duration_since(Instant::now())
                    }
                    _ => return, // 已被清空，或者已经被更新的循环取代。
                }
            };
            tokio::time::sleep(delay).await;
            {
                let guard = self.0.offline_retry.lock().unwrap();
                match guard.as_ref() {
                    Some(s) if s.loop_id == my_id => {}
                    _ => return,
                }
            }
            match self.ensure_refreshed().await {
                Ok(()) => {
                    self.clear_offline_retry_if_current(my_id);
                    return;
                }
                Err(SessionError::Transient(_)) => {
                    let mut guard = self.0.offline_retry.lock().unwrap();
                    match guard.as_mut() {
                        Some(s) if s.loop_id == my_id => {
                            s.attempt = s.attempt.saturating_add(1);
                            s.next_attempt_at = Instant::now() + backoff_delay(s.attempt);
                        }
                        _ => return,
                    }
                }
                Err(_) => {
                    // 终止性错误：ensure_refreshed 内部已经清理了会话状态，
                    // 这里只需要停掉重试循环本身（如果它还是当前这个循环）。
                    self.clear_offline_retry_if_current(my_id);
                    return;
                }
            }
        }
    }

    /// 立即重试一次（窗口获得焦点 / 网络恢复事件 / 用户点击"重试"按钮触发），
    /// 不等待后台循环的下一次定时唤醒。成功则清空重试状态；仍然失败（瞬时
    /// 错误）则保留后台循环按原计划继续；遇到终止性错误则清空并回登录页。
    /// 不在重试状态时视为"当前会话状态就是答案"，不发起任何请求。
    pub async fn retry_now(&self) -> ResumeOutcome {
        if self.0.offline_retry.lock().unwrap().is_none() {
            return if self.summary().logged_in {
                ResumeOutcome::Restored
            } else {
                ResumeOutcome::NeedLogin
            };
        }
        match self.ensure_refreshed().await {
            Ok(()) => {
                self.clear_offline_retry();
                ResumeOutcome::Restored
            }
            Err(SessionError::Transient(_)) => ResumeOutcome::OfflineRetained,
            Err(_) => {
                self.clear_offline_retry();
                ResumeOutcome::NeedLogin
            }
        }
    }

    /// 同时取出当前 access token 与一份"操作身份快照"（generation + 区域）。
    /// 只读快照，不会被钥匙串 I/O 卡住。
    ///
    /// 统一约定（Codex 代码评审第 6 轮高危项 1/2）：任何"发起一次操作、
    /// 之后某个时间点还要回来读状态决定是否继续（重放/提交/清理）"的
    /// 代码，都应该在发起时调用这个方法捕获 [`OperationContext`]，此后
    /// 统一用 [`Self::current_token_if_context_matches`] 做前置校验，而
    /// 不是各自发明"比较 token 字符串"或"只比较 generation 不比较区域"
    /// 这类局部方案——那些方案在"账号已切换但生成的新 token 字符串恰好
    /// 不同""区域已切换但 generation 恰好还没被别的操作推进"等边界下都
    /// 会出错。
    fn current_token_and_context(&self) -> Result<(String, OperationContext), SessionError> {
        match &*self.current_state() {
            SessionState::Active(s) => Ok((
                s.access_token.clone(),
                OperationContext {
                    generation: s.generation,
                    region: s.region,
                },
            )),
            SessionState::LoggingOut(_) => Err(SessionError::Terminated("LOGGING_OUT".to_string())),
            SessionState::LoggedOut => Err(SessionError::NoActiveSession),
        }
    }

    /// 校验当前状态是否仍然匹配某次操作发起时捕获的 [`OperationContext`]，
    /// 匹配则返回*当前*的 access token（可能已经被其他并发调用刷新轮转
    /// 过，但身份——generation + region——必须和发起时完全一致）；不匹配
    /// （账号变了、区域变了、登出了、正在登出）一律返回
    /// [`SessionError::SessionChanged`]，绝不把不属于这次操作的会话的
    /// token 交给调用方去重放一个发往*旧*身份/区域 API 的请求。
    fn current_token_if_context_matches(
        &self,
        ctx: OperationContext,
    ) -> Result<String, SessionError> {
        match &*self.current_state() {
            SessionState::Active(s) if s.generation == ctx.generation && s.region == ctx.region => {
                Ok(s.access_token.clone())
            }
            _ => Err(SessionError::SessionChanged),
        }
    }

    fn active_region(&self) -> Option<Region> {
        match &*self.current_state() {
            SessionState::Active(s) | SessionState::LoggingOut(s) => Some(s.region),
            SessionState::LoggedOut => None,
        }
    }

    /// 建立新会话（登录提交）。网络请求结束后，在**同一次** `commit_lock`
    /// 持有内完成"复查代次 → 写钥匙串 → 写索引（含失败时的补救）→ 发布
    /// 快照"整段序列，与 [`Self::commit_refreshed_token`] 一致——见文件顶部
    /// `结构性重构` 设计说明与 `自定义开发功能列表.md` 功能 9（Codex 代码
    /// 评审第 5 轮高危项 1：此前钥匙串/索引 I/O 在两次持锁之间的锁外执行，
    /// 一次过期登录可能在这段窗口里把已经提交的更新登录覆盖掉，且被作废
    /// 登录写下的钥匙串/索引残留也从未回滚）。
    ///
    /// `epoch` 由调用方（`login_email`/`login_2fa`/`login_phone`）在发起
    /// 网络请求之前用 [`Self::begin_login_epoch`] 领取。
    async fn commit_new_session(
        &self,
        region: Region,
        pair: TokenPair,
        epoch: u64,
    ) -> Result<UserProfile, SessionError> {
        let api = self.api_for(region);
        let profile = match api.get_profile(&pair.access_token).await {
            Ok(p) => p,
            Err(e) => {
                // 已经拿到 token 对但取不到 profile：这个 refresh_token 家族
                // 还没被记录到钥匙串或会话索引的任何一处，用户也没有别的入口
                // 能登出它，尽力撤销一次避免孤儿家族长期有效（不确定服务端是
                // 否部署 B4，忽略这次尽力撤销本身的结果——原始错误才是需要
                // 上抛给调用方的）。
                let _ = api.logout(Some(&pair.refresh_token)).await;
                return Err(session_error_from_api_call(&e));
            }
        };

        let account = secret_store::account_key(region.storage_key(), profile.id);
        let email_masked = mask_email(&profile.email);

        #[cfg(test)]
        {
            let gate = self.0.commit_gate.lock().unwrap().take();
            if let Some(gate) = gate {
                let _ = gate.reached.send(());
                let _ = gate.release.await;
            }
        }

        let guard = self.0.commit_lock.lock().await;

        // 代次复查：网络请求结束后、真正做任何持久化写入之前，在这同一次
        // `commit_lock` 持有内重新确认代次仍然最新——不通过就完全不碰
        // 钥匙串/索引，直接放弃（不会留下任何残留需要回滚）。
        if self.0.login_epoch.load(Ordering::SeqCst) != epoch {
            drop(guard);
            let _ = api.logout(Some(&pair.refresh_token)).await;
            return Err(SessionError::Transient(
                "登录已被更新的登录/切换操作取代，已放弃".to_string(),
            ));
        }

        // Codex 代码评审第 5 轮低危项 7：登出正在进行（`LoggingOut`）时不能
        // 被这次登录提交覆盖——这不是"代次过期"，而是"当前有另一个操作
        // 正在收尾"，返回可重试的错误，不碰当前状态，调用方（前端）之后
        // 可以重新发起登录。
        if matches!(&*self.current_state(), SessionState::LoggingOut(_)) {
            drop(guard);
            let _ = api.logout(Some(&pair.refresh_token)).await;
            return Err(SessionError::Transient(
                "有登出操作正在进行，请稍后重试登录".to_string(),
            ));
        }

        let keyring_ok = keyring_set(
            self.0.secret_store.clone(),
            account.clone(),
            pair.refresh_token.clone(),
        )
        .await;

        let mut index = self.load_index().await;
        let old_entry = index.get(region).cloned();
        index.set(
            region,
            SessionIndexEntry {
                user_id: profile.id,
                email_masked: email_masked.clone(),
            },
        );
        let index_persisted = self.index_save(index).await;

        // Codex 代码评审第 3/4/5 轮高危项：会话索引写入失败时，不能把该
        // 区域旧的（属于另一个账号的）索引项留在磁盘上——否则下次启动
        // `resume_session` 会用旧索引项的 user_id 拼出旧账号的钥匙串 key，
        // 把它当成"可以免登录恢复"的账号，而用户明明已经登录了一个新账号。
        let mut index_degraded = if index_persisted {
            // 索引写成功：如果该区域原来指向另一个账号，顺手删掉旧账号的
            // 钥匙串条目，不留孤儿凭据（第 5 轮高危项 1 扩展：以前只在索引
            // 写失败的分支里清理旧账号，成功分支完全没有清理）。
            if let Some(old) = &old_entry {
                if old.user_id != profile.id {
                    let old_account = secret_store::account_key(region.storage_key(), old.user_id);
                    let _ = keyring_delete(self.0.secret_store.clone(), old_account).await;
                }
            }
            false
        } else {
            let mut old_credential_invalidated = true;
            if let Some(old) = &old_entry {
                if old.user_id != profile.id {
                    let old_account = secret_store::account_key(region.storage_key(), old.user_id);
                    old_credential_invalidated =
                        keyring_delete(self.0.secret_store.clone(), old_account).await;
                }
            }
            // 尽力删除该区域残留的索引项（不论是旧账号还是这次半途而废的新
            // 账号写入），确保重启时这个区域没有任何索引条目可用。
            let mut cleanup = self.load_index().await;
            cleanup.remove(region);
            let _ = self.index_save(cleanup).await;

            if !old_credential_invalidated {
                // 连旧账号的钥匙串条目都无法作废：重启后 `resume_session`
                // 仍然可能用旧索引项（如果它后来又被写回）或残留的旧钥匙串
                // 条目恢复出旧账号。不能报告这次登录成功——撤销新签发的
                // refresh，返回明确错误，且不发布任何新状态。
                drop(guard);
                let _ = api.logout(Some(&pair.refresh_token)).await;
                return Err(SessionError::PersistFailed("无法保存登录状态".to_string()));
            }
            true
        };

        // 读回校验（Codex 验收第 5 轮高危项 1）：不再逐个枚举"钥匙串写失败 ×
        // 索引写失败 × 同账号/不同账号"的组合，而是在发布前直接从磁盘读回，
        // 判断**重启后 `resume_session` 会恢复成什么**：
        // - 该区域没有索引项，或索引项指向的钥匙串条目确认不存在 → 重启走
        //   NeedLogin，安全；
        // - 索引项指向本次新账号且钥匙串里正是本次新 refresh → 已完整持久化；
        // - 其余情况（指向旧账号、同账号但钥匙串里是旧 refresh、或钥匙串此刻
        //   读不出）→ 重启可能恢复一个用户已放弃的会话：先删该钥匙串条目，
        //   删不掉再删该区域索引项，都失败则不报告登录成功。
        let persisted_for_restart = match self.load_index_checked().await {
            // 索引读回失败（暂时的 I/O 错误或文件损坏）：无法判断重启会恢复
            // 成什么，不能按"无条目"放行。直接把索引覆盖写成已知内容——钥匙串
            // 写入成功时只含本区域的新条目，否则为空——其他区域的条目随之
            // 丢弃（下次需重新登录，是安全方向）。覆盖写也失败则不报告登录
            // 成功（Codex 验收第 6 轮高危项 1）。
            Err(()) => {
                let mut reset = SessionIndex::default();
                if keyring_ok {
                    reset.set(
                        region,
                        SessionIndexEntry {
                            user_id: profile.id,
                            email_masked: email_masked.clone(),
                        },
                    );
                }
                if !self.index_save(reset).await {
                    drop(guard);
                    let _ = api.logout(Some(&pair.refresh_token)).await;
                    return Err(SessionError::PersistFailed("无法保存登录状态".to_string()));
                }
                keyring_ok
            }
            Ok(on_disk) => match on_disk.get(region).cloned() {
                None => false,
                Some(entry) => {
                    let entry_account =
                        secret_store::account_key(region.storage_key(), entry.user_id);
                    let stored =
                        keyring_read(self.0.secret_store.clone(), entry_account.clone()).await;
                    let provably_new = entry.user_id == profile.id
                        && matches!(&stored, Ok(Some(tok)) if *tok == pair.refresh_token);
                    if provably_new {
                        true
                    } else if matches!(stored, Ok(None)) {
                        false
                    } else {
                        let deleted =
                            keyring_delete(self.0.secret_store.clone(), entry_account).await;
                        if !deleted {
                            let mut cleanup = self.load_index().await;
                            cleanup.remove(region);
                            if !self.index_save(cleanup).await {
                                drop(guard);
                                let _ = api.logout(Some(&pair.refresh_token)).await;
                                return Err(SessionError::PersistFailed(
                                    "无法保存登录状态".to_string(),
                                ));
                            }
                        }
                        false
                    }
                }
            },
        };
        // 未能完整持久化的会话只在本次运行内有效（前端提示下次启动需重新登录）。
        index_degraded = index_degraded || (keyring_ok && !persisted_for_restart);

        // 该区域若有待清理残留：同一账号时钥匙串条目已被本次写入或读回校验
        // 处理，可以移出；不同账号时必须确认旧账号的钥匙串条目已删除才移出，
        // 否则保留记录，继续提示并允许按旧 user_id 重试（Codex 验收第 7 轮
        // 高危项 1）。
        let pending_uid = self
            .0
            .pending_cleanups
            .lock()
            .unwrap()
            .get(&region)
            .copied();
        if let Some(old_uid) = pending_uid {
            let cleared = old_uid == profile.id
                || keyring_delete(
                    self.0.secret_store.clone(),
                    secret_store::account_key(region.storage_key(), old_uid),
                )
                .await;
            if cleared {
                self.0.pending_cleanups.lock().unwrap().remove(&region);
            }
        }
        let generation = self.next_generation();
        self.publish(SessionState::Active(ActiveSession {
            region,
            user_id: profile.id,
            email_masked,
            refresh_token: pair.refresh_token,
            access_token: pair.access_token,
            keyring_degraded: !keyring_ok,
            index_degraded,
            generation,
        }));
        drop(guard);
        Ok(profile)
    }

    pub async fn login_email(
        &self,
        region: Region,
        email: &str,
        password: &str,
        captcha: Option<&super::api::CaptchaTicket>,
    ) -> Result<LoginOutcome, SessionError> {
        // 在发起任何网络请求之前领取代次号，代表"用户在这一刻发起了这次
        // 登录"（Codex 代码评审第 3/4 轮高危项 1）。
        let epoch = self.begin_login_epoch().await;
        let api = self.api_for(region);
        match api
            .login_email(email, password, captcha)
            .await
            .map_err(|e| session_error_from_api_call(&e))?
        {
            LoginResult::Tokens(pair) => {
                self.commit_new_session(region, pair, epoch).await?;
                Ok(LoginOutcome::LoggedIn)
            }
            LoginResult::Requires2fa {
                temp_token,
                email_masked,
            } => Ok(LoginOutcome::Requires2fa {
                temp_token,
                email_masked,
            }),
        }
    }

    pub async fn login_2fa(
        &self,
        region: Region,
        temp_token: &str,
        totp_code: &str,
    ) -> Result<(), SessionError> {
        let epoch = self.begin_login_epoch().await;
        let api = self.api_for(region);
        let pair = api
            .login_2fa(temp_token, totp_code)
            .await
            .map_err(|e| session_error_from_api_call(&e))?;
        self.commit_new_session(region, pair, epoch).await?;
        Ok(())
    }

    pub async fn send_sms_code(
        &self,
        region: Region,
        phone: &str,
        captcha: Option<&super::api::CaptchaTicket>,
    ) -> Result<(), SessionError> {
        let api = self.api_for(region);
        api.send_sms_code(phone, captcha)
            .await
            .map_err(|e| session_error_from_api_call(&e))
    }

    pub async fn login_phone(
        &self,
        region: Region,
        phone: &str,
        code: &str,
        captcha: Option<&super::api::CaptchaTicket>,
    ) -> Result<(), SessionError> {
        let epoch = self.begin_login_epoch().await;
        let api = self.api_for(region);
        let pair = api
            .login_phone(phone, code, captcha)
            .await
            .map_err(|e| session_error_from_api_call(&e))?;
        self.commit_new_session(region, pair, epoch).await?;
        Ok(())
    }

    /// 应用启动时按区域读会话索引并尝试恢复会话。索引里没有记录，或钥匙串
    /// 读不到 / 不可用，均视为"没有可恢复的会话"（[`ResumeOutcome::NeedLogin`]），
    /// 回登录页。恢复后紧跟着的刷新若因为网络原因失败，则保留刚恢复的内存
    /// 状态并报告 [`ResumeOutcome::OfflineRetained`]——前端应继续展示"已登录"
    /// 界面并提示离线，而不是回登录页（方案第 5.2 节"断网不回登录页"同样
    /// 适用于启动恢复场景，Codex 代码评审高危项 5）。
    ///
    /// 与 [`Self::commit_new_session`] 共用同一个登录/恢复操作代次：先在
    /// `commit_lock` 内领取代次号（即便接下来发现没有可恢复的会话、直接
    /// `NeedLogin`，这一步也已经让更早发起的登录作废——Codex 代码评审第
    /// 3/4 轮高危项 1），索引/钥匙串读取在锁外进行，最终在一次新的
    /// `commit_lock` 持有内重新校验代次并发布新快照。
    pub async fn resume_session(&self, region: Region) -> ResumeOutcome {
        let epoch = self.begin_login_epoch().await;
        // 必须在读取索引/钥匙串之前记下，见 [`Inner::wipe_epoch`]。
        let wipe_epoch = self.0.wipe_epoch.load(Ordering::SeqCst);

        let index = self.load_index().await;
        let Some(entry) = index.get(region).cloned() else {
            return ResumeOutcome::NeedLogin;
        };
        let account = secret_store::account_key(region.storage_key(), entry.user_id);
        let refresh_token = match keyring_get(self.0.secret_store.clone(), account).await {
            Some(token) => token,
            None => return ResumeOutcome::NeedLogin,
        };

        let (committed, blocked_by_cleanup) = {
            let guard = self.0.commit_lock.lock().await;
            let epoch_ok = self.0.login_epoch.load(Ordering::SeqCst) == epoch
                && self.0.wipe_epoch.load(Ordering::SeqCst) == wipe_epoch;
            // Codex 代码评审第 6 轮高危项 2：正在登出（`LoggingOut`）时不能
            // 被这次恢复提交覆盖——登出的网络请求还在路上，如果这里把状态
            // 改回 `Active`，`logout()` 第③/④步再读到的就是"当前状态已经
            // 不是 LoggingOut"，会直接短路成 `NotLoggedIn`，跳过远端撤销
            // 与本地清理。全面检查过其余两条会发布 `Active` 的路径：
            // `commit_new_session`（登录提交）第 5 轮已经拒绝 `LoggingOut`；
            // `commit_refreshed_token`（refresh 落盘）走的是
            // `update_active_token_locked`，只在原地更新 `Active`/
            // `LoggingOut` 已有变体的字段、从不把 `LoggingOut` 换成
            // `Active`，本来就不受影响。这里补上恢复提交这一处，三条路径
            // 对 `LoggingOut` 的处理就此保持一致。
            let not_logging_out = !matches!(&*self.current_state(), SessionState::LoggingOut(_));
            // 上一次登出在该区域的本地清理尚未完成（`Inner::pending_cleanups`，
            // 索引与钥匙串都没删掉）时，不能从这些残留里把刚登出的会话
            // "恢复"回来——那等于静默撤销用户的登出，并丢失待重试的清理
            // 目标。只拦同一区域；切到其他区域照常恢复。
            // 只拦"索引仍指向待清理的那个用户"：同区域已被另一账号的新会话
            // 占用时，恢复的是新账号，不受旧账号残留影响（Codex 验收第 7 轮）。
            let cleanup_pending_here =
                self.0.pending_cleanups.lock().unwrap().get(&region) == Some(&entry.user_id);
            if epoch_ok && not_logging_out && !cleanup_pending_here {
                let generation = self.next_generation();
                self.publish(SessionState::Active(ActiveSession {
                    region,
                    user_id: entry.user_id,
                    email_masked: entry.email_masked,
                    refresh_token,
                    access_token: String::new(),
                    keyring_degraded: false,
                    index_degraded: false,
                    generation,
                }));
                drop(guard);
                (true, false)
            } else {
                drop(guard);
                (false, cleanup_pending_here)
            }
        };
        if blocked_by_cleanup {
            // 请求的区域有待清理残留：这次恢复被拒绝，必须明确回报 NeedLogin。
            // 不能落到下面"按当前状态汇报"的分支——若当前是另一区域的会话，
            // 那会返回 Restored，前端会把另一区域的会话当成用户所选区域的
            // 会话展示（Codex 验收第 6 轮高危项 2 回归测试发现）。
            return ResumeOutcome::NeedLogin;
        }
        if !committed {
            // 提交前已经有更新的登录/恢复操作抢先拿到了更大的代次号，或者
            // 登出正在进行——这次恢复是过期/不该继续的，不碰当前会话，
            // 直接按"当前实际状态"汇报结果，而不是继续对着一个可能已经
            // 被别的操作改写的状态做 `ensure_refreshed()`（那会误刷/误判
            // 一个跟这次调用毫无关系的会话）。
            return if self.summary().logged_in {
                ResumeOutcome::Restored
            } else {
                ResumeOutcome::NeedLogin
            };
        }

        match self.ensure_refreshed().await {
            Ok(()) => {
                self.clear_offline_retry();
                ResumeOutcome::Restored
            }
            Err(SessionError::Transient(_)) => {
                self.start_offline_retry();
                ResumeOutcome::OfflineRetained
            }
            Err(_) => ResumeOutcome::NeedLogin,
        }
    }

    /// 单飞续期：同一时刻只有一个 refresh 请求，其余调用者等待同一个结果
    /// （方案第 5.2 节）。清空 `refresh_inflight` 槽位前核对 `attempt_id`，
    /// 只清掉自己发起/加入的那次——见 [`Inner::refresh_inflight`] 的文档
    /// 注释（Codex 代码评审高危项 4）。
    pub async fn ensure_refreshed(&self) -> Result<(), SessionError> {
        let (attempt_id, fut) = {
            let mut guard = self.0.refresh_inflight.lock().unwrap();
            if let Some((id, existing)) = guard.as_ref() {
                // 加入一个已经在跑的 future：它创建时已经检查过会话状态，
                // 加入不会触发新的网络请求，不需要再检查一次。
                (*id, existing.clone())
            } else {
                // 只有真正要新建一次 refresh 时才检查会话状态，且检查必须在
                // 已经拿到 `refresh_inflight` 锁之后进行——如果先检查状态、
                // 再单独去拿 `refresh_inflight` 锁插入新槽位，中间这段空档
                // 可能被 `logout()` 插进来把状态改成 `LoggingOut`，导致这里
                // 仍然新建了一次 refresh，而 `logout()` 读取 inflight 槽位
                // 的时机如果恰好在这次插入之前，就会漏等这次刷新（Codex
                // 代码评审低危项 1）。把状态检查纳入同一次持锁范围，杜绝这段
                // 检查-插入之间的竞态窗口。
                match &*self.current_state() {
                    SessionState::LoggingOut(_) => {
                        return Err(SessionError::Terminated("LOGGING_OUT".to_string()))
                    }
                    SessionState::LoggedOut => return Err(SessionError::NoActiveSession),
                    SessionState::Active(_) => {}
                }

                let id = self
                    .0
                    .refresh_attempt_counter
                    .fetch_add(1, Ordering::SeqCst)
                    + 1;
                let manager = self.clone();
                let shared: RefreshFuture =
                    async move { manager.do_refresh().await }.boxed().shared();
                *guard = Some((id, shared.clone()));
                (id, shared)
            }
        };

        let result = fut.await;
        {
            let mut guard = self.0.refresh_inflight.lock().unwrap();
            if matches!(guard.as_ref(), Some((id, _)) if *id == attempt_id) {
                *guard = None;
            }
            // 槽位已经被更新的一次 attempt 取代：说明本次完成之后、清空之前，
            // 已经有新的 ensure_refreshed() 调用（多半是本次因某种原因结束
            // 后触发的下一轮），不能动它。
        }
        result.map(|_| ())
    }

    /// [`run_protected`] 里"access token 过期 → 单飞刷新"分支专用：刷新请求
    /// 本身遇到网络错误/429/5xx（[`ensure_refreshed`] 返回
    /// [`SessionError::Transient`]）时，做与请求重放同一套有限次数快速退避
    /// 重试（复用 [`PROTECTED_FAST_RETRY_ATTEMPTS`]/[`backoff_delay`]），而不
    /// 是把第一次 Transient 直接上抛终止整个受保护调用——刷新接口的瞬时失败
    /// 与请求接口的瞬时失败享受同一种"抖动式短暂失败"待遇（方案第 3.1/5.2
    /// 节"网络错误、429、5xx 保留凭证并退避重试"）。凭证在整个过程中都不
    /// 清除：`ensure_refreshed()`/`do_refresh()` 对 Transient 分类本就不触碰
    /// 钥匙串。
    ///
    /// 每次重试前都用 `ctx` 复查身份（generation + 区域）：会话在退避等待
    /// 期间登出或切换，立即以 `SessionChanged` 停止重试，不再对着一个已经
    /// 不属于这次调用的会话继续刷新（与 `run_protected` 里请求重放前的身份
    /// 复查同一套逻辑）。次数耗尽仍失败时，把最后一次 `Transient` 错误原样
    /// 上抛，交由调用方界面提示并重试，不在这里无限重试或悄悄延长等待。
    ///
    /// **总时限（Opus 复核低危项 L1，中危项 R4 修正实现方式）**：不整体包
    /// 一层 `tokio::time::timeout` 强行取消进行中的请求——那样会在 refresh
    /// 请求已经发到服务端、服务端已经轮转了 refresh token，但响应还没送达
    /// 客户端时把这次 `await` 提前掐断：本地既没拿到新 token，下次还会拿
    /// 这个已经失效的旧 token 去刷新，服务端会把这当成"已轮转 token 被
    /// 重放"，直接撤销整条会话（`REFRESH_TOKEN_REUSED`）。改为只在**发起
    /// 下一次重试之前**检查这个软时限，已经发出的请求永远等它自然返回，
    /// 不强行中断；`PROTECTED_FAST_RETRY_ATTEMPTS` 次数上限仍然是主要的
    /// 收敛手段，这个时限只是防止个别请求异常缓慢时无限期地叠加等待。
    const REFRESH_BACKOFF_OVERALL_TIMEOUT: Duration = Duration::from_secs(30);

    /// 本次调用要用的总时限：测试可通过
    /// [`Self::set_test_refresh_backoff_deadline`] 覆盖成更短的值（真实的
    /// 30 秒太长，测试没法在合理时间内断言"截止后不再重试"），生产路径下
    /// 恒为 [`Self::REFRESH_BACKOFF_OVERALL_TIMEOUT`]。
    fn refresh_backoff_overall_timeout(&self) -> Duration {
        #[cfg(test)]
        {
            if let Some(d) = *self.0.refresh_backoff_deadline_override.lock().unwrap() {
                return d;
            }
        }
        Self::REFRESH_BACKOFF_OVERALL_TIMEOUT
    }

    async fn ensure_refreshed_with_backoff(
        &self,
        ctx: OperationContext,
        stale_token: &str,
    ) -> Result<(), SessionError> {
        let deadline = Instant::now() + self.refresh_backoff_overall_timeout();
        let mut attempt = 0u32;
        let mut last_transient = String::new();
        loop {
            // 只在**真正要发起下一次请求之前**检查软时限（Codex 验收 X3：旧
            // 实现把这个检查放在上一次请求失败、决定要不要睡这一轮退避的
            // 那一刻——睡眠本身可能跨过截止时间，睡醒后却没有再检查一次，
            // 于是截止时间之后仍然发出了下一次 refresh 请求）。首次尝试
            // （`attempt == 0`）永远放行，不受这个检查约束——截止时间是从
            // 这个函数开始计时的，此刻必然还没过期，也不应该因为函数入口
            // 到这里之间的极短调度延迟而误判。
            if attempt > 0 && Instant::now() >= deadline {
                return Err(SessionError::Transient(last_transient));
            }
            match self.ensure_refreshed().await {
                Ok(()) => return Ok(()),
                Err(SessionError::Transient(msg)) => {
                    if attempt >= PROTECTED_FAST_RETRY_ATTEMPTS {
                        return Err(SessionError::Transient(msg));
                    }
                    last_transient = msg;
                    let delay = backoff_delay(attempt);
                    attempt += 1;
                    tokio::time::sleep(delay).await;
                    let current = self.current_token_if_context_matches(ctx)?;
                    // 醒来时 token 已经不是发起这次退避时的旧值：说明另一路
                    // 并发调用已经独立完成了一轮刷新（不一定是同一个
                    // single-flight future——两次重试各自新建的 future 在
                    // 时序上可能没有重叠），没必要再发一次多余的刷新请求
                    // （Opus 复核中危项 M5）。
                    if current != stale_token {
                        return Ok(());
                    }
                }
                Err(other) => return Err(other),
            }
        }
    }

    async fn do_refresh(&self) -> Result<TokenPair, SessionError> {
        let (region, refresh_token, generation) = match &*self.current_state() {
            SessionState::Active(s) => (s.region, s.refresh_token.clone(), s.generation),
            SessionState::LoggingOut(s) => (s.region, s.refresh_token.clone(), s.generation),
            SessionState::LoggedOut => return Err(SessionError::NoActiveSession),
        };

        let api = self.api_for(region);
        match api.refresh(&refresh_token).await {
            Ok(pair) => {
                match self
                    .commit_refreshed_token(region, pair.clone(), generation)
                    .await
                {
                    PersistOutcome::Written => Ok(pair),
                    PersistOutcome::Stale => {
                        // 会话在这次刷新进行期间已经被别的操作推进（重新
                        // 登录 / 登出），这次结果已经过时：不触碰当前状态
                        // （可能已经是全新的有效会话），只告诉这一批等待者
                        // 去重试（Codex 代码评审高危项 2）。
                        Err(SessionError::Transient("会话已更新，请重试".to_string()))
                    }
                    PersistOutcome::WriteFailed => {
                        // 真正的"钥匙串刚变得不可用"：只有仍是当前这个
                        // generation 时才终止；已经推进则视为陈旧，不误伤
                        // 新会话（Codex 代码评审高危项 2、3）。
                        self.terminate_if_generation_matches(generation).await;
                        Err(SessionError::NeedsRelogin)
                    }
                }
            }
            Err(api_err) => match classify_refresh_error(&api_err) {
                RefreshOutcome::Terminate(code) => {
                    self.terminate_if_generation_matches(generation).await;
                    Err(SessionError::Terminated(code))
                }
                RefreshOutcome::RetryWithBackoff => {
                    Err(SessionError::Transient(api_err.to_string()))
                }
                RefreshOutcome::Failed(code) => Err(SessionError::Other(code)),
            },
        }
    }

    /// refresh 结果落盘：generation 校验与钥匙串写入在**同一次** `commit_lock`
    /// 持有内、通过 `spawn_blocking` 连续完成，中途不释放锁（Codex 代码评审
    /// 第 4 轮高危项，修高-3 前半）——如果检查完就放锁、写钥匙串放在锁外，
    /// 同账号的一次新登录可能在这段窗口里抢先写入更新的 token，随后这次
    /// refresh 的写入会把它覆盖回旧值。改成同一次持有后，新登录的提交只能
    /// 排在这次 refresh 完全提交（或判定 Stale/WriteFailed）之后才能拿到
    /// `commit_lock`，保证"最终钥匙串是最后一次真正提交的值"。
    ///
    /// 已经退化为内存模式的会话（`keyring_degraded`/`index_degraded`）不再
    /// 重试写入——本来就写不进钥匙串或没有索引指着它，这次同样写不进去不
    /// 代表"钥匙串刚变得不可用"，直接按 [`PersistOutcome::Written`] 处理
    /// （内存里的 token 一并更新），避免被误判为需要重新登录（Codex 代码
    /// 评审中危项 1）。
    async fn commit_refreshed_token(
        &self,
        region: Region,
        pair: TokenPair,
        expected_generation: u64,
    ) -> PersistOutcome {
        let guard = self.0.commit_lock.lock().await;

        let (user_id, not_persistable) = match &*self.current_state() {
            SessionState::Active(s) | SessionState::LoggingOut(s)
                if s.generation == expected_generation =>
            {
                (s.user_id, s.keyring_degraded || s.index_degraded)
            }
            _ => {
                drop(guard);
                return PersistOutcome::Stale;
            }
        };

        if not_persistable {
            self.update_active_token_locked(&pair, expected_generation);
            drop(guard);
            return PersistOutcome::Written;
        }

        let account = secret_store::account_key(region.storage_key(), user_id);
        let ok = keyring_set(
            self.0.secret_store.clone(),
            account,
            pair.refresh_token.clone(),
        )
        .await;
        if !ok {
            drop(guard);
            return PersistOutcome::WriteFailed;
        }
        self.update_active_token_locked(&pair, expected_generation);
        drop(guard);
        PersistOutcome::Written
    }

    /// 更新内存中的令牌对：`Active` 与 `LoggingOut` 两种状态都要更新——后者
    /// 对应"刷新在登出等待在途请求期间完成"，新 token 必须记入 `LoggingOut`
    /// 状态，供 `logout()` 第③步读到最新值（Codex 代码评审高危项 3）。
    /// generation 不匹配（迟到）时丢弃，不触碰状态。调用方必须已经持有
    /// `commit_lock`。
    fn update_active_token_locked(&self, pair: &TokenPair, expected_generation: u64) {
        let current = self.current_state();
        let mut new_state = (*current).clone();
        let matched = match &mut new_state {
            SessionState::Active(s) | SessionState::LoggingOut(s)
                if s.generation == expected_generation =>
            {
                s.refresh_token = pair.refresh_token.clone();
                s.access_token = pair.access_token.clone();
                true
            }
            _ => false,
        };
        if matched {
            self.publish(new_state);
        }
    }

    /// 清空持久化凭据（钥匙串条目 + 会话索引记录），通过 `spawn_blocking`
    /// 执行，调用方必须已经持有 `commit_lock`。
    ///
    /// 索引先删、钥匙串后删（Codex 代码评审第 5 轮高危项 3："可靠作废"设计）：
    /// 只要索引项被成功删除，`resume_session` 从一开始就不会去拼这个账号
    /// 的钥匙串 key，重启必然 `NeedLogin`，钥匙串那一步删不删得掉都不影响
    /// 这个结论；反过来，索引删不掉但钥匙串删掉了，`resume_session` 找得到
    /// 索引项却读不到 refresh_token，同样必然 `NeedLogin`。也就是说
    /// **两项里只要有一项成功，就能保证重启不会恢复这个会话**——
    /// [`PersistedCredentialsCleanup::reliably_invalidated`] 正是这个判断；
    /// 两项都失败才是真正无法保证的情况。
    async fn wipe_persisted_credentials(
        &self,
        region: Region,
        user_id: i64,
    ) -> PersistedCredentialsCleanup {
        // 只删除**指向这个 user_id** 的索引项：同区域若已被另一账号的新会话
        // 占用，重试清理旧账号时不能把新账号的索引删掉（Codex 验收第 7 轮
        // 高危项 1）。索引不指向该用户时不算"已通过索引作废"，必须靠删除
        // 钥匙串条目才能判定清理完成。
        // 用区分"确认无条目"与"读取失败"的读取：读取失败时既不能当作已无
        // 索引，也不能改写索引（改写会丢掉读不到的内容），只能靠钥匙串删除。
        self.0.wipe_epoch.fetch_add(1, Ordering::SeqCst);
        let (index_absent, index_entry_removed) = match self.load_index_checked().await {
            Err(()) => (false, false),
            Ok(mut index) => match index.get(region) {
                None => (true, false),
                Some(entry) if entry.user_id == user_id => {
                    index.remove(region);
                    (false, self.index_save(index).await)
                }
                Some(_) => (false, false),
            },
        };

        let account = secret_store::account_key(region.storage_key(), user_id);
        let keyring_removed = keyring_delete(self.0.secret_store.clone(), account).await;

        PersistedCredentialsCleanup {
            index_absent,
            index_entry_removed,
            keyring_removed,
        }
    }

    /// 只有当前状态仍然是**这个 generation** 的 `Active` 时才终止：清空
    /// 持久化凭据、置 `LoggedOut`、generation 递增，整段在同一次
    /// `commit_lock` 持有内完成（Codex 代码评审第 4 轮，修高-3 后半的
    /// 姊妹逻辑——`LoggingOut` 状态下的失败交给 `logout()` 统一收尾，不能
    /// 抢先清空；generation 不匹配则说明这次失败针对的已经是一个不存在的
    /// 旧会话，不能误伤期间新建立的会话）。这条路径是服务端已经明确判定
    /// 会话失效（`TOKEN_REVOKED` 等）时的兜底本地清理，没有面向用户的登出
    /// 结果需要上报，因此只做"可靠作废"的清理，不像 [`Self::logout`] 那样
    /// 把清理结果转化成不同的 [`LogoutOutcome`]。
    async fn terminate_if_generation_matches(&self, expected_generation: u64) -> bool {
        let guard = self.0.commit_lock.lock().await;
        let target = match &*self.current_state() {
            SessionState::Active(s) if s.generation == expected_generation => {
                Some((s.region, s.user_id, s.keyring_degraded))
            }
            _ => None,
        };
        let Some((region, user_id, keyring_degraded)) = target else {
            drop(guard);
            return false;
        };
        let cleanup = self.wipe_persisted_credentials(region, user_id).await;
        // 本地两项清理都失败时，残留的索引与 refresh 仍可能让重启恢复会话
        // （例如 BACKEND_MODE_ACTIVE 在消费前拒绝，旧 refresh 服务端仍有效，
        // 后端模式关闭后即可复活）。与登出路径一致，保留待清理目标供
        // `retry_local_cleanup` 重试，而不是声称已退出（Codex 验收第 5 轮高危项 2）。
        // 钥匙串里的 refresh 没删掉也登记：即使索引已删、重启不会恢复，
        // 残留凭据仍应可重试清除（Fable 终验中危项 2）。
        if needs_pending_cleanup(&cleanup, false, keyring_degraded) {
            self.0
                .pending_cleanups
                .lock()
                .unwrap()
                .insert(region, user_id);
        }
        self.publish(SessionState::LoggedOut);
        self.next_generation();
        // 持锁内清：锁释放后另一次恢复可能已经发布新会话并启动了自己的
        // 离线重试，锁外清会误清它（Fable 终验低危项）。
        self.clear_offline_retry();
        drop(guard);
        true
    }

    /// 登出四步（方案第 5.2 节）：
    /// ① 置登出中，拒绝新 refresh；② 等待在途 refresh 完成；
    /// ③ 持最新 refresh_token 调 `/auth/logout`；④ 清钥匙串和索引。
    ///
    /// 第③步读取的 `latest_refresh_token` 必须是等待在途 refresh **完成
    /// 之后**再读（而不是进入等待前缓存的旧值）——若该 refresh 成功轮转了
    /// token，`update_active_token_locked` 会把新值写进 `LoggingOut` 状态，
    /// 这里才能读到它，否则会拿旧 token 登出，服务端家族已经轮转、旧 token
    /// 查不到家族，导致 `revoked` 恒为 `false`（Codex 代码评审高危项 3）。
    ///
    /// 第④步（置 `LoggedOut`、清理失败时登记 `pending_cleanups` + 删钥匙串/索引）整体在
    /// **同一次** `commit_lock` 持有内完成，且只有当前状态仍是"发起登出时
    /// 那个 generation 的 `LoggingOut`"才动手——账号的钥匙串 key 是
    /// `region:user_id`，如果登出等待网络那段时间里，同一个账号被重新
    /// 登录取代（key 完全一样），此时如果仍然删钥匙串会把新登录刚写入的
    /// 凭据删掉；不满足就整个跳过清理，把这次登出视为"目标会话已不存在"
    /// （Codex 代码评审第 4 轮，修高-3 后半）。
    pub async fn logout(&self) -> LogoutOutcome {
        let (region, generation) = {
            let guard = self.0.commit_lock.lock().await;
            let current = self.current_state();
            let result = match &*current {
                SessionState::Active(s) => {
                    let info = (s.region, s.generation);
                    self.publish(SessionState::LoggingOut(s.clone()));
                    Some(info)
                }
                SessionState::LoggingOut(s) => Some((s.region, s.generation)),
                SessionState::LoggedOut => None,
            };
            drop(guard);
            match result {
                Some(info) => info,
                // 未登录但仍有待清理的残留：再次点击"登出"等价于重试本地清理
                // （Codex 代码评审第 6 轮高危项 3）。
                None if self.has_pending_cleanup() => return self.retry_local_cleanup().await,
                None => return LogoutOutcome::NotLoggedIn,
            }
        };

        // ② 等待在途 refresh 完成（single-flight 的 Shared future 可安全重复
        // await）。刷新失败时 `do_refresh` 不会触碰 `LoggingOut` 状态（见
        // `terminate_if_generation_matches` 只处理 `Active`），所以这里等待
        // 完之后状态必然仍是 `LoggingOut`（除非用户在此期间又调用了一次
        // logout，那也仍是 `LoggingOut`）。
        let inflight = self.0.refresh_inflight.lock().unwrap().clone();
        if let Some((_, fut)) = inflight {
            let _ = fut.await;
        }

        let latest_refresh_token = match &*self.current_state() {
            SessionState::LoggingOut(s) if s.generation == generation => s.refresh_token.clone(),
            // generation 不匹配，或状态已经不是 LoggingOut：这次登出的目标
            // 会话已经不存在了（罕见：等待期间发生了针对同一 identity 的
            // 另一次操作），没有 refresh_token 可用来调用 /auth/logout。
            _ => return LogoutOutcome::NotLoggedIn,
        };

        // ③ 持最新 refresh_token 调 /auth/logout，网络请求在锁外执行。
        let api = self.api_for(region);
        let logout_result = api.logout(Some(&latest_refresh_token)).await;
        let remote_revoked = matches!(&logout_result, Ok(res) if res.revoked);

        // ④ 清理：只有当前仍是这个 generation 的 LoggingOut 才清。
        let cleanup = {
            let guard = self.0.commit_lock.lock().await;
            let target = match &*self.current_state() {
                SessionState::LoggingOut(s) if s.generation == generation => {
                    Some((s.user_id, s.keyring_degraded))
                }
                _ => None,
            };
            let Some((user_id, keyring_degraded)) = target else {
                // 状态已经不是这个 generation 的 LoggingOut（罕见：等待
                // 期间发生了针对同一 identity 的另一次操作）——这次登出的
                // 目标会话已经不存在，不碰当前状态，不解读网络调用结果
                // （那是对着一个已经不相关的旧 identity 发的，不代表"当前"
                // 会话的登出结果）。
                drop(guard);
                return LogoutOutcome::NotLoggedIn;
            };
            let cleanup = self.wipe_persisted_credentials(region, user_id).await;
            // Codex 代码评审第 6 轮高危项 3：远端确认撤销、或本地清理至少
            // 一项成功时，才能安全地置 `LoggedOut`；否则保留待清理目标
            // （登记到 `pending_cleanups`），支持之后用
            // `retry_local_cleanup()`/再次点击"登出"重试，而不是假装
            // 已经清空、让重启有机会误恢复这个仍然可能有效的会话。
            // Fable 终验中危项 2：远端未确认时，钥匙串删除失败即登记——
            // 索引已删时重启不会恢复，但服务端仍有效的 refresh 残留在钥匙串
            // 里，需要提示与重试入口。
            if needs_pending_cleanup(&cleanup, remote_revoked, keyring_degraded) {
                self.0
                    .pending_cleanups
                    .lock()
                    .unwrap()
                    .insert(region, user_id);
            }
            self.publish(SessionState::LoggedOut);
            self.next_generation();
            self.clear_offline_retry();
            drop(guard);
            cleanup
        };

        // Codex 代码评审第 5 轮高危项 3：远端确认撤销（`revoked: true`）时
        // 优先报告 `Revoked`，即使本地两项清理都失败——服务端家族已经真的
        // 失效，本地残留的 refresh_token 下次被用来 refresh 时会拿到
        // `REFRESH_TOKEN_REVOKED`/`INVALID` 之类的终止码，同样会被清理，
        // 不构成"重启后误恢复"的风险。远端未确认时，如果本地清理也两项
        // 全部失败，就不能报告"已退出"，必须提示清理失败并允许重试
        // （`LogoutOutcome::LocalCleanupFailed`，第 6 轮高危项 3）。
        match logout_result {
            Ok(res) if res.revoked => LogoutOutcome::Revoked,
            _ if !cleanup.reliably_invalidated() => LogoutOutcome::LocalCleanupFailed,
            _ => LogoutOutcome::LocalOnly,
        }
    }

    /// 重试所有待清理区域的本地清理（索引 + 钥匙串）。全部清理成功返回
    /// `LocalOnly`，仍有区域清不掉返回 `LocalCleanupFailed`，没有待清理项时
    /// 按当前状态汇报（Codex 代码评审第 6 轮高危项 3、验收第 6 轮高危项 2）。
    pub async fn retry_local_cleanup(&self) -> LogoutOutcome {
        let guard = self.0.commit_lock.lock().await;
        let targets: Vec<(Region, i64)> = self
            .0
            .pending_cleanups
            .lock()
            .unwrap()
            .iter()
            .map(|(r, u)| (*r, *u))
            .collect();
        if targets.is_empty() {
            drop(guard);
            return if self.summary().logged_in {
                LogoutOutcome::LocalOnly
            } else {
                LogoutOutcome::NotLoggedIn
            };
        }
        let mut all_cleared = true;
        for (region, user_id) in targets {
            let cleanup = self.wipe_persisted_credentials(region, user_id).await;
            if cleanup.clears_pending() {
                self.0.pending_cleanups.lock().unwrap().remove(&region);
            } else {
                all_cleared = false;
            }
        }
        drop(guard);
        if all_cleared {
            LogoutOutcome::LocalOnly
        } else {
            LogoutOutcome::LocalCleanupFailed
        }
    }

    /// 通用受保护接口调用包装：自动处理 `TOKEN_EXPIRED`/`ACCESS_TOKEN_EXPIRED`
    /// 触发的单飞 refresh 后重放（只重放一次，避免无限循环）。
    ///
    /// 发起时用 [`Self::current_token_and_context`] 捕获一份
    /// [`OperationContext`]（generation + region），此后每一次"要不要刷新/
    /// 要不要用当前 token 重放"的判断都统一通过
    /// [`Self::current_token_if_context_matches`] 校验这份快照，而不是
    /// 只比较 token 字符串（Codex 代码评审第 6 轮高危项 1：原实现收到
    /// `TOKEN_EXPIRED` 后只比较"当前 token 字符串"与"失败时用的 token
    /// 字符串"，如果会话已经切到新账号或新区域，字符串大概率不同，代码会
    /// 误判成"别人已经刷新过"，直接把新会话的 token 拿来重放这次发往*旧*
    /// 身份/区域 API 的请求——跨区域场景下这等于把新区域的合法 token 发给
    /// 了旧区域的服务器）。身份一旦不匹配（账号变了、区域变了、登出了、
    /// 正在登出），立即停止并返回 [`SessionError::SessionChanged`]，绝不
    /// 读取/使用新会话的 token；本次调用触发的所有重放请求，其目标 API
    /// （由调用方在构造 `call` 闭包时已经绑定发起时的 region/base_url）
    /// 保持不变。
    ///
    /// 重放结果同样经过失败分类（而不是原样转成 `Other`），确保重放拿到
    /// `TOKEN_REVOKED` 等终止码时仍会正确终止会话（Codex 代码评审中危项
    /// 2）。
    ///
    /// `idempotent` 由调用方按 HTTP 方法语义传入：GET/HEAD 等幂等请求可以在
    /// 网络错误/429/5xx 时安全地自动重放，`false`（POST 等非幂等请求）遇到
    /// 同类瞬时错误直接返回 [`SessionError::Transient`]，不做任何自动重放——
    /// 服务端可能已经处理了第一次请求，重放有产生重复副作用的风险（Codex
    /// 代码评审第 3 轮中危项 1）。
    ///
    /// 只读取快照拿 token，不涉及 `commit_lock`，因此不会被任何正在进行的
    /// 钥匙串/索引 I/O 卡住（Codex 代码评审第 4 轮中危项 1）。
    pub async fn call_protected<T, F, Fut>(
        &self,
        idempotent: bool,
        call: F,
    ) -> Result<T, SessionError>
    where
        F: Fn(String) -> Fut,
        Fut: Future<Output = Result<T, ApiCallError>>,
    {
        let (token, ctx) = self.current_token_and_context()?;
        self.run_protected(token, ctx, idempotent, call).await
    }

    /// 业务命令用的受保护调用：发起时捕获会话身份，并用**同一份**身份的
    /// 区域构造 [`ApiClient`] 交给闭包，保证请求目标与 token 属于同一会话
    /// （分两步取区域与 token 时，中间切换区域会把新区域 token 发往旧区域）。
    /// 返回值附带发起时的 [`SessionIdentity`]，供调用方把结果与会话绑定。
    pub async fn call_protected_api<T, F, Fut>(
        &self,
        idempotent: bool,
        call: F,
    ) -> Result<(T, SessionIdentity), SessionError>
    where
        F: Fn(ApiClient, String) -> Fut,
        Fut: Future<Output = Result<T, ApiCallError>>,
    {
        let (token, ctx) = self.current_token_and_context()?;
        let identity = self
            .identity_for_context(ctx)
            .ok_or(SessionError::SessionChanged)?;
        let api = self.api_for(ctx.region);
        let value = self
            .run_protected(token, ctx, idempotent, |t| call(api.clone(), t))
            .await?;
        Ok((value, identity))
    }

    /// 当前 `Active` 会话的身份；未登录或登出中为 `None`。
    pub fn current_identity(&self) -> Option<SessionIdentity> {
        match &*self.current_state() {
            SessionState::Active(s) => Some(SessionIdentity {
                region: s.region,
                user_id: s.user_id,
                generation: s.generation,
            }),
            _ => None,
        }
    }

    fn identity_for_context(&self, ctx: OperationContext) -> Option<SessionIdentity> {
        self.current_identity()
            .filter(|i| i.generation == ctx.generation && i.region == ctx.region)
    }

    /// WE2AI 数据根（`~/.we2ai`），供其他模块存放非秘密的界面记忆。
    pub fn data_root(&self) -> &std::path::Path {
        &self.0.data_root
    }

    /// 系统钥匙串抽象的克隆句柄（`Arc` 廉价克隆）。供 `apply.rs` 保存/恢复
    /// 用户自己的 Claude `ANTHROPIC_API_KEY`（P6：恢复官方配置），复用登录
    /// 会话已经注入的同一个 `SecretStore` 实现（生产为系统钥匙串，测试为
    /// `InMemorySecretStore`），不单独再造一套注入路径。
    pub fn secret_store(&self) -> Arc<dyn SecretStore> {
        self.0.secret_store.clone()
    }

    /// 测试专用：直接发布一个 `Active` 会话并把请求指向 mock server，供其他
    /// 模块（如 `keys.rs`）测试受保护调用。
    #[cfg(test)]
    pub(crate) fn test_seed_active(
        &self,
        region: Region,
        user_id: i64,
        access_token: &str,
        base_url: String,
    ) -> u64 {
        self.set_test_base_url_override(base_url);
        let generation = self.next_generation();
        self.publish(SessionState::Active(ActiveSession {
            region,
            user_id,
            email_masked: "u****@we2ai.com".to_string(),
            refresh_token: "test-refresh".to_string(),
            access_token: access_token.to_string(),
            keyring_degraded: false,
            index_degraded: false,
            generation,
        }));
        generation
    }

    async fn run_protected<T, F, Fut>(
        &self,
        mut token: String,
        ctx: OperationContext,
        idempotent: bool,
        call: F,
    ) -> Result<T, SessionError>
    where
        F: Fn(String) -> Fut,
        Fut: Future<Output = Result<T, ApiCallError>>,
    {
        let mut refreshed_once = false;
        let mut backoff_attempt = 0u32;
        if token.is_empty() {
            // `ResumeOutcome::OfflineRetained` 恢复出的会话在成功刷新前
            // access_token 是空字符串（断网启动时保留了内存状态，但从没有
            // 真正拿到过新 token）。直接先刷新，不必先发一次带空 Bearer 的
            // 请求去确认失败——SubPanel `jwt_auth.go` 对空 token 返回 401
            // `EMPTY_TOKEN`，按未知 401 处理的话会误终止这个本来能救回来的
            // 会话（Codex 代码评审中危项 2）。
            refreshed_once = true;
            self.ensure_refreshed().await?;
            token = self.current_token_if_context_matches(ctx)?;
        }
        loop {
            match call(token.clone()).await {
                Ok(value) => {
                    // 成功响应也要复查身份：请求在途期间若已切换账号或区域，
                    // 这份数据属于旧会话，不能交给当前界面使用（Codex 验收
                    // 第 5 轮中危项）。
                    self.current_token_if_context_matches(ctx)?;
                    return Ok(value);
                }
                Err(err) => match classify_protected_error(&err) {
                    ProtectedOutcome::RetryAfterRefresh if !refreshed_once => {
                        refreshed_once = true;
                        // 不再靠"字符串是否相等"判断要不要刷新——必须先确认
                        // 身份（generation + region）没变；变了立刻停止，
                        // 绝不把新会话的 token 拿去重放这次发往旧身份/区域
                        // API 的请求（Codex 代码评审第 6 轮高危项 1）。
                        let current = self.current_token_if_context_matches(ctx)?;
                        if current == token {
                            self.ensure_refreshed_with_backoff(ctx, &token).await?;
                        }
                        token = self.current_token_if_context_matches(ctx)?;
                        continue;
                    }
                    ProtectedOutcome::RetryAfterRefresh => {
                        // 已经重放过一次仍要求刷新：不再无限重试，按可重试的
                        // 瞬时失败上抛，避免死循环。
                        return Err(SessionError::Transient(format!(
                            "受保护接口重放后仍要求刷新: {err}"
                        )));
                    }
                    ProtectedOutcome::Terminate(code) => {
                        self.terminate_if_generation_matches(ctx.generation).await;
                        return Err(SessionError::Terminated(code));
                    }
                    ProtectedOutcome::RetryWithBackoff => {
                        // 网络错误/429/5xx：非幂等请求不自动重放（见上方
                        // `idempotent` 文档），幂等请求最多快速重试
                        // `PROTECTED_FAST_RETRY_ATTEMPTS` 次（1s、2s），超过
                        // 就把最后一次的错误当 Transient 上抛，由调用方界面
                        // 提示并提供重试，不在一次 invoke 里挂起到分钟级
                        // （Codex 代码评审第 3 轮中危项 1）。后台自动退避循环
                        // 只由启动恢复（`resume_session`）启动，这里不启动。
                        if !idempotent || backoff_attempt >= PROTECTED_FAST_RETRY_ATTEMPTS {
                            return Err(SessionError::Transient(err.to_string()));
                        }
                        let delay = backoff_delay(backoff_attempt);
                        backoff_attempt += 1;
                        tokio::time::sleep(delay).await;
                        // 重试前重新确认身份没有在等待期间变化（同上，
                        // 中危项 1 与第 3 轮中危项 1 的合并统一实现）。
                        self.current_token_if_context_matches(ctx)?;
                        continue;
                    }
                    ProtectedOutcome::Other(msg) => return Err(SessionError::Other(msg)),
                },
            }
        }
    }

    pub fn active_region_or(&self, fallback: Region) -> Region {
        self.active_region().unwrap_or(fallback)
    }

    /// 登录页"记住上次"的区域选择，与是否有活跃会话无关；存在独立文件里，
    /// 见 [`LastRegionFile`]。
    pub fn last_region(&self) -> Option<Region> {
        LastRegionFile::load(&self.0.data_root)
    }

    /// 写入失败时把错误返回给命令层（Codex 代码评审第 6 轮中危项 3：此前
    /// 错误被丢弃、IPC 仍返回成功，前端的失败处理永远不会触发，下次启动
    /// 可能选回旧区域而用户毫不知情）。
    pub fn set_last_region(&self, region: Region) -> Result<(), String> {
        LastRegionFile::save(&self.0.data_root, region).map_err(|e| e.to_string())
    }
}

fn session_error_from_api_call(err: &ApiCallError) -> SessionError {
    match err {
        ApiCallError::Network(msg) => SessionError::Transient(msg.to_string()),
        ApiCallError::Api(api_err) => match &api_err.code {
            ErrorCode::Named(code) => SessionError::Other(code.clone()),
            ErrorCode::Status(status) => SessionError::Other(status.to_string()),
        },
    }
}

/// 登录结果（供命令层判断是否需要展示 2FA 输入）。
#[derive(Debug, Clone)]
pub enum LoginOutcome {
    LoggedIn,
    Requires2fa {
        temp_token: String,
        email_masked: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::we2ai::secret_store::test_support::InMemorySecretStore;
    use serde_json::json;
    use std::sync::mpsc;
    use std::time::Duration;
    use tempfile::TempDir;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    /// 网络请求到达信号：请求真正到达 mock server 时通过 `reached` 发一次
    /// **非阻塞**通知，立即返回预先配置好延迟的响应模板。
    ///
    /// 最初尝试的设计是让 `respond()` 同步阻塞在一个"放行"信号上（到达时
    /// 通知、等测试主动释放才应答），但实测会把交错测试跑挂/跑出错误
    /// 结果——`wiremock::MockServer` 内部用 `std::thread::spawn` +
    /// `tokio::runtime::Builder::new_current_thread()` 起了一个**单线程**
    /// 运行时来处理所有连接（`wiremock-0.6.5` 的 `bare_server.rs`）；在
    /// `respond()` 里同步阻塞会让这个单线程完全停摆，导致同一个
    /// `MockServer` 上*其他*并发请求（比如交错测试里 B 的登录请求）根本
    /// 得不到处理，不是"只卡住这一个请求"。
    ///
    /// 因此改为：`respond()` 只做非阻塞的"到达"通知，真正的响应延迟通过
    /// `ResponseTemplate::set_delay()` 交给 wiremock 自己的异步调度（用的是
    /// 真正的 `tokio::time::sleep`，不会阻塞线程）。这样"请求已经到达"是
    /// 一个真实、确定性的信号，不再需要靠 sleep/忙等猜测另一侧代码是否已
    /// 经跑到某一步；延迟本身只需要"明显长于交错测试里另一侧要做的那点
    /// 本地内存操作"，不需要复杂的按需释放机制（Codex 代码评审第 6 轮
    /// 中危项 2）。
    struct RequestGate {
        reached: mpsc::Sender<()>,
        response: ResponseTemplate,
    }

    impl Respond for RequestGate {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            let _ = self.reached.send(());
            self.response.clone()
        }
    }

    /// 异步等待请求到达信号（把 std 阻塞 `recv()` 丢给 `spawn_blocking`，
    /// 不占用/阻塞 tokio 工作线程或当前测试任务）。
    async fn wait_for_request(reached_rx: mpsc::Receiver<()>) {
        tokio::task::spawn_blocking(move || {
            let _ = reached_rx.recv();
        })
        .await
        .expect("blocking wait-for-request task panicked");
    }

    /// `response` 会先经过 `delay` 才真正返回（延迟本身由 wiremock 异步
    /// 调度，不阻塞任何线程）；调用方在 `wait_for_request` 返回之后，有
    /// 这整段 `delay` 的窗口可以确定性地做其他交错操作。
    fn request_gate(
        response: ResponseTemplate,
        delay: Duration,
    ) -> (RequestGate, mpsc::Receiver<()>) {
        let (reached_tx, reached_rx) = mpsc::channel();
        (
            RequestGate {
                reached: reached_tx,
                response: response.set_delay(delay),
            },
            reached_rx,
        )
    }

    fn manager_with_store(
        data_root: &std::path::Path,
        store: Arc<InMemorySecretStore>,
    ) -> SessionManager {
        SessionManager::new(store, data_root.to_path_buf(), "test".to_string())
    }

    /// 测试期把 `SessionManager` 指向一个 `wiremock` server：直接操作内部状态
    /// 而不是走真实登录接口，因为登录接口地址由 `Region::base_url()` 编译期
    /// 常量决定，测试改用"预置 Active 状态 + 用 `ApiClient::new_with_base_url`
    /// 替换请求目标"的方式覆盖 refresh/logout 场景。
    fn seed_active(
        manager: &SessionManager,
        region: Region,
        user_id: i64,
        refresh_token: &str,
    ) -> u64 {
        let generation = manager.next_generation();
        manager.publish(SessionState::Active(ActiveSession {
            region,
            user_id,
            email_masked: "u****@we2ai.com".to_string(),
            refresh_token: refresh_token.to_string(),
            access_token: "old-access".to_string(),
            keyring_degraded: false,
            index_degraded: false,
            generation,
        }));
        generation
    }

    // 由于 do_refresh/logout 内部用 `self.api_for(region)` 基于 Region 编译期
    // 常量地址构造 ApiClient，测试无法直接把它指向 wiremock。为了仍然对真实
    // HTTP 往返做端到端验证（而不是只测分类纯函数），下面的测试改为直接构造
    // `ApiClient::new_with_base_url` 并单独调用 `SessionManager` 暴露的分类
    // 逻辑与状态机方法所依赖的最小单元；纯状态机行为（单飞、generation、
    // 登出四步）用一个内嵌的假 API 完成端到端断言。

    #[test]
    fn mask_email_keeps_first_chars_and_domain() {
        assert_eq!(mask_email("ab@example.com"), "ab****@example.com");
        assert_eq!(mask_email("a@example.com"), "a****@example.com");
        assert_eq!(mask_email("abcdef@example.com"), "ab****@example.com");
        assert_eq!(mask_email("no-at-sign"), "***");
    }

    #[test]
    fn last_region_persists_independently_of_login_state() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store);
        assert_eq!(manager.last_region(), None);
        manager.set_last_region(Region::DomesticProd).unwrap();
        assert_eq!(manager.last_region(), Some(Region::DomesticProd));
        assert!(!manager.summary().logged_in);
    }

    // Codex 代码评审第 5 轮高危项 2："记住上次区域"拆到独立文件
    // （`last_region.json`）之后，与会话索引（`session_index.json`）不再
    // 共享任何可变状态——不会再出现"set_last_region 对整份索引文件做
    // 读—改—存，冲掉登录/登出刚写入的条目"这类竞态。这里让 `set_last_region`
    // 在登录、登出进行期间反复并发调用，断言两份文件各自的最终内容都符合
    // 预期，互不干扰。
    #[tokio::test]
    async fn last_region_writes_do_not_interfere_with_concurrent_login_or_logout_index_writes() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(50))
                    .set_body_json(json!({
                        "code": 0, "message": "success",
                        "data": {"access_token": "a", "refresh_token": "r", "expires_in": 3600}
                    })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/profile"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"id": 1, "email": "u@we2ai.com"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"revoked": true}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let login_task = {
            let m = manager.clone();
            tokio::spawn(async move {
                m.login_email(Region::International, "u@we2ai.com", "pw", None)
                    .await
            })
        };
        for _ in 0..20 {
            manager.set_last_region(Region::DomesticProd).unwrap();
            tokio::task::yield_now().await;
        }
        let login_result = login_task.await.unwrap();
        assert!(matches!(login_result, Ok(LoginOutcome::LoggedIn)));

        assert!(
            SessionIndex::load(dir.path())
                .get(Region::International)
                .is_some(),
            "the login's session index entry must survive concurrent last_region writes"
        );
        assert_eq!(manager.last_region(), Some(Region::DomesticProd));

        let logout_task = {
            let m = manager.clone();
            tokio::spawn(async move { m.logout().await })
        };
        for _ in 0..20 {
            manager.set_last_region(Region::International).unwrap();
            tokio::task::yield_now().await;
        }
        let logout_outcome = logout_task.await.unwrap();
        assert_eq!(logout_outcome, LogoutOutcome::Revoked);

        assert!(
            SessionIndex::load(dir.path())
                .get(Region::International)
                .is_none(),
            "logout's index cleanup must survive concurrent last_region writes"
        );
        assert_eq!(manager.last_region(), Some(Region::International));
    }

    #[tokio::test]
    async fn resume_session_restores_session_and_fetches_new_access_token() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());

        // 存量数据：会话索引 + 钥匙串里都有上次登录留下的 refresh_token
        // （模拟应用重启后的状态，而不是刚登录完的内存状态）。
        let mut index = SessionIndex::default();
        index.set(
            Region::International,
            SessionIndexEntry {
                user_id: 42,
                email_masked: "u****@we2ai.com".to_string(),
            },
        );
        index.save(dir.path()).unwrap();
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 42),
                "stored-refresh-token",
            )
            .unwrap();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "resumed-access", "refresh_token": "resumed-refresh", "expires_in": 3600, "token_type": "Bearer"}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let restored = manager.resume_session(Region::International).await;
        assert_eq!(
            restored,
            ResumeOutcome::Restored,
            "resume_session should report success (免登录)"
        );

        let summary = manager.summary();
        assert!(summary.logged_in);
        assert_eq!(summary.region.as_deref(), Some("international"));
        assert_eq!(summary.email_masked.as_deref(), Some("u****@we2ai.com"));
        // 轮转后的新 refresh_token 已落盘钥匙串（旧值不再是唯一凭证）。
        assert!(store.contains(
            secret_store::SERVICE_NAME,
            &secret_store::account_key("international", 42)
        ));
    }

    #[tokio::test]
    async fn resume_session_returns_false_when_no_session_index_entry() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store);
        let restored = manager.resume_session(Region::International).await;
        assert_eq!(restored, ResumeOutcome::NeedLogin);
        assert!(!manager.summary().logged_in);
    }

    #[tokio::test]
    async fn resume_session_returns_false_when_keyring_has_no_entry() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store);
        // 索引里有记录，但钥匙串里没有对应的 refresh_token（如用户手动清空了
        // 系统钥匙串），必须视为"没有可恢复的会话"，而不是 panic 或误报成功。
        let mut index = SessionIndex::default();
        index.set(
            Region::International,
            SessionIndexEntry {
                user_id: 42,
                email_masked: "u****@we2ai.com".to_string(),
            },
        );
        index.save(dir.path()).unwrap();

        let restored = manager.resume_session(Region::International).await;
        assert_eq!(restored, ResumeOutcome::NeedLogin);
        assert!(!manager.summary().logged_in);
    }

    #[tokio::test]
    async fn resume_session_returns_need_login_when_refresh_is_rejected_after_restore() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        let mut index = SessionIndex::default();
        index.set(
            Region::International,
            SessionIndexEntry {
                user_id: 42,
                email_masked: "u****@we2ai.com".to_string(),
            },
        );
        index.save(dir.path()).unwrap();
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 42),
                "expired-refresh-token",
            )
            .unwrap();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "code": "REFRESH_TOKEN_EXPIRED", "message": "expired"
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let restored = manager.resume_session(Region::International).await;
        assert_eq!(
            restored,
            ResumeOutcome::NeedLogin,
            "an outright-rejected refresh token must not report a restored session"
        );
        assert!(!manager.summary().logged_in);
        assert!(!store.contains(
            secret_store::SERVICE_NAME,
            &secret_store::account_key("international", 42)
        ));
    }

    #[tokio::test]
    async fn resume_session_retains_session_offline_and_does_not_touch_keyring_or_index() {
        // Codex 代码评审高危项 5：断网启动时必须保持"已登录"界面（前端据此
        // 展示离线提示），而不是把用户踢回登录页；本地钥匙串与会话索引也
        // 不应该被清空——用户很可能只是暂时没有网络，恢复网络后应该能直接
        // 用回这份会话，不需要重新登录。
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        let mut index = SessionIndex::default();
        index.set(
            Region::International,
            SessionIndexEntry {
                user_id: 42,
                email_masked: "u****@we2ai.com".to_string(),
            },
        );
        index.save(dir.path()).unwrap();
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 42),
                "refresh-1",
            )
            .unwrap();

        // 指向一个不存在的端口模拟断网。
        manager.set_test_base_url_override("http://127.0.0.1:1".to_string());

        let restored = manager.resume_session(Region::International).await;
        assert_eq!(restored, ResumeOutcome::OfflineRetained);
        assert!(
            manager.summary().logged_in,
            "front-end must keep showing the logged-in UI while offline"
        );
        assert!(store.contains(
            secret_store::SERVICE_NAME,
            &secret_store::account_key("international", 42)
        ));
        assert!(SessionIndex::load(dir.path())
            .get(Region::International)
            .is_some());
        assert!(
            manager.summary().offline_retry_in_seconds.is_some(),
            "offline retry countdown must be exposed for the front-end banner"
        );
    }

    // Codex 代码评审中危项 3：自动退避重试，不推迟到 P3。

    #[test]
    fn backoff_delay_sequence_is_1_2_4_capped_at_60() {
        assert_eq!(backoff_delay(0), Duration::from_secs(1));
        assert_eq!(backoff_delay(1), Duration::from_secs(2));
        assert_eq!(backoff_delay(2), Duration::from_secs(4));
        assert_eq!(backoff_delay(3), Duration::from_secs(8));
        assert_eq!(backoff_delay(4), Duration::from_secs(16));
        assert_eq!(backoff_delay(5), Duration::from_secs(32));
        assert_eq!(backoff_delay(6), Duration::from_secs(60), "capped at 60s");
        assert_eq!(backoff_delay(7), Duration::from_secs(60), "stays capped");
        assert_eq!(backoff_delay(1000), Duration::from_secs(60), "stays capped");
    }

    #[tokio::test]
    async fn offline_retry_loop_eventually_recovers_after_transient_failures() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        let mut index = SessionIndex::default();
        index.set(
            Region::International,
            SessionIndexEntry {
                user_id: 42,
                email_masked: "u****@we2ai.com".to_string(),
            },
        );
        index.save(dir.path()).unwrap();
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 42),
                "refresh-1",
            )
            .unwrap();

        let server = MockServer::start().await;
        // 第一次调用（resume_session 自己发起的那次）失败；exhausted 之后
        // 落到第二个 mock，之后的调用（后台重试循环发起的）都成功。这样只
        // 需要真实等过第一个退避档位（1 秒）即可观察到自动恢复，不需要
        // 引入 `tokio::time::pause`（wiremock 的 `set_delay` 内部也用真实
        // 定时器，与暂停的虚拟时钟混用容易互相卡住，不值得为这一个测试
        // 引入这层复杂度）。
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "recovered-access", "refresh_token": "refresh-2", "expires_in": 3600, "token_type": "Bearer"}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let outcome = manager.resume_session(Region::International).await;
        assert_eq!(outcome, ResumeOutcome::OfflineRetained);
        assert_eq!(
            manager.summary().offline_retry_in_seconds,
            Some(1),
            "first backoff attempt should be scheduled ~1s out"
        );

        // 第一档退避是 1 秒；等足够久，让后台循环的第一次自动重试跑完。
        tokio::time::sleep(Duration::from_millis(1300)).await;

        assert!(manager.summary().logged_in, "session must still be active");
        assert_eq!(
            manager.summary().offline_retry_in_seconds,
            None,
            "offline retry must clear itself once recovered"
        );
    }

    #[tokio::test]
    async fn retry_now_recovers_immediately_without_waiting_for_the_backoff_timer() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        let mut index = SessionIndex::default();
        index.set(
            Region::International,
            SessionIndexEntry {
                user_id: 42,
                email_masked: "u****@we2ai.com".to_string(),
            },
        );
        index.save(dir.path()).unwrap();
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 42),
                "refresh-1",
            )
            .unwrap();

        // 断网：resume_session 自己那次必然失败，进入 OfflineRetained 并
        // 启动后台退避循环（下一次定时重试排在 1 秒之后）。
        manager.set_test_base_url_override("http://127.0.0.1:1".to_string());
        let outcome = manager.resume_session(Region::International).await;
        assert_eq!(outcome, ResumeOutcome::OfflineRetained);

        // 网络"恢复"：切到一个真实可用的 mock server，不等后台循环的 1 秒
        // 定时器，直接调用 retry_now()（对应窗口获得焦点/`online` 事件/
        // 用户点击"重试"按钮）。
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "recovered-access", "refresh_token": "refresh-2", "expires_in": 3600, "token_type": "Bearer"}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let retry_outcome = manager.retry_now().await;
        assert_eq!(retry_outcome, ResumeOutcome::Restored);
        assert_eq!(manager.summary().offline_retry_in_seconds, None);
        assert!(manager.summary().logged_in);
    }

    #[tokio::test]
    async fn retry_now_is_a_no_op_when_not_in_offline_retry_state() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store);
        // 从未登录、也没有在离线重试：retry_now 不应该发起任何请求（没有
        // mock server 可用，如果真的发了请求这里会直接连接失败并 panic）。
        let outcome = manager.retry_now().await;
        assert_eq!(outcome, ResumeOutcome::NeedLogin);
    }

    #[tokio::test]
    async fn call_protected_retries_with_backoff_on_repeated_server_errors_then_succeeds() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_active(&manager, Region::International, 42, "refresh-1");

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/probe"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/probe"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"ok": true}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let api = manager.api_for(Region::International);
        let result = manager
            .call_protected(true, |token| {
                let api = &api;
                async move { api.get_authed("/api/v1/user/probe", &token).await }
            })
            .await
            .expect("should retry with backoff and eventually succeed");
        assert_eq!(
            result.get("ok").and_then(serde_json::Value::as_bool),
            Some(true)
        );
    }

    // Codex 代码评审第 3 轮中危项 1：断网时 call_protected 必须在秒级（而不是
    // 旧实现 7 次退避 × 20 秒请求超时叠加出来的 ~4.7 分钟）内返回 Transient，
    // 把重试交给调用方界面。这里用一个立即拒绝连接的
    // 端口模拟"断网"（网络错误是连接失败而不是真的等满 20 秒请求超时），
    // 断言总耗时落在 `PROTECTED_FAST_RETRY_ATTEMPTS=2` 次快速重试的退避
    // 窗口（1s+2s=3s）量级，而不是分钟级。
    #[tokio::test]
    async fn call_protected_returns_transient_within_a_few_seconds_when_offline() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store);
        seed_active(&manager, Region::International, 42, "refresh-1");
        manager.set_test_base_url_override("http://127.0.0.1:1".to_string());

        let api = manager.api_for(Region::International);
        let started = std::time::Instant::now();
        let result = manager
            .call_protected(true, |token| {
                let api = &api;
                async move { api.get_authed("/api/v1/user/probe", &token).await }
            })
            .await;
        let elapsed = started.elapsed();

        assert!(
            matches!(result, Err(SessionError::Transient(_))),
            "offline calls must fail as Transient, got {result:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "call_protected must give up within a few seconds while offline, took {elapsed:?}"
        );
        assert!(
            manager.summary().logged_in,
            "a transient network failure must not terminate the session"
        );
    }

    // Codex 代码评审第 3 轮中危项 1：重试等待期间会话已经登出，唤醒后必须
    // 停止重试（不再发出第二次请求），而不是傻等退避窗口结束后再重放一个
    // 已经不存在的会话。
    #[tokio::test]
    async fn call_protected_stops_retrying_once_the_session_logs_out_mid_backoff() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_active(&manager, Region::International, 42, "refresh-1");
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 42),
                "refresh-1",
            )
            .unwrap();

        let server = MockServer::start().await;
        // 第一次请求失败（触发一次快速重试的退避等待）；如果实现在等待后
        // 仍然发起第二次请求，这个 `.expect(1)` 会让测试失败。
        Mock::given(method("GET"))
            .and(path("/api/v1/user/probe"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"revoked": true}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let manager_for_call = manager.clone();
        let api = manager.api_for(Region::International);
        let call_task = tokio::spawn(async move {
            manager_for_call
                .call_protected(true, |token| {
                    let api = &api;
                    async move { api.get_authed("/api/v1/user/probe", &token).await }
                })
                .await
        });

        // 第一次快速重试的退避是 1 秒；在它睡着的这段时间内把会话登出掉。
        tokio::time::sleep(Duration::from_millis(300)).await;
        let logout_outcome = manager.logout().await;
        assert_eq!(logout_outcome, LogoutOutcome::Revoked);

        let result = call_task.await.unwrap();
        // Codex 代码评审第 6 轮高危项 1：重试前的守卫统一改成
        // `current_token_if_context_matches`，身份（generation + region）
        // 一旦不匹配一律返回 `SessionChanged`，不再区分
        // `Terminated`/`NoActiveSession` 这些子情形——登出之后会话已经
        // 不是发起这次调用时的那个身份了。
        assert!(
            matches!(result, Err(SessionError::SessionChanged)),
            "retrying against a session that logged out mid-backoff must stop instead of replaying, got {result:?}"
        );
    }

    // Opus 复核低危项 L2：与上面的用例同源，但拦截的是*刷新本身*的退避
    // （偏差修复项 C / 中危项 M5 新增的 `ensure_refreshed_with_backoff`），
    // 不是请求重放的退避。第一次 refresh 遇到 503 后进入 1 秒退避，退避期间
    // 登出；醒来时身份已变化，必须停止，不发起第二次 refresh 请求。
    #[tokio::test]
    async fn refresh_backoff_stops_retrying_and_issues_no_second_refresh_once_the_session_logs_out_mid_backoff(
    ) {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_active(&manager, Region::International, 42, "refresh-1");
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 42),
                "refresh-1",
            )
            .unwrap();

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/probe"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "code": "TOKEN_EXPIRED", "message": "expired"
            })))
            .expect(1)
            .mount(&server)
            .await;
        // 只应该被调用一次：退避期间登出后，醒来必须停止，不发起第二次
        // refresh 请求。如果实现在登出后仍然重试，这个 `.expect(1)` 会让
        // 测试失败。
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"revoked": true}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let manager_for_call = manager.clone();
        let api = manager.api_for(Region::International);
        let call_task = tokio::spawn(async move {
            manager_for_call
                .call_protected(true, |token| {
                    let api = &api;
                    async move { api.get_authed("/api/v1/user/probe", &token).await }
                })
                .await
        });

        // 第一次 refresh 失败后的退避是 1 秒；在它睡着的这段时间内登出。
        tokio::time::sleep(Duration::from_millis(300)).await;
        let logout_outcome = manager.logout().await;
        assert_eq!(logout_outcome, LogoutOutcome::Revoked);

        let result = call_task.await.unwrap();
        assert!(
            matches!(result, Err(SessionError::SessionChanged)),
            "retrying the refresh itself against a session that logged out mid-backoff must stop instead of issuing a second refresh, got {result:?}"
        );
        // 两个 `.expect(1)`（受保护请求 1 次、refresh 1 次）在 `server` 析构
        // 时校验：确认没有发生第二次 refresh 请求。
    }

    // Codex 代码评审第 3 轮中危项 1：POST 等非幂等请求遇到网络错误/429/5xx
    // 不能自动重放（服务端可能已经处理了第一次请求），必须立即返回
    // Transient，一次都不重试。
    #[tokio::test]
    async fn call_protected_does_not_retry_non_idempotent_requests() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store);
        seed_active(&manager, Region::International, 42, "refresh-1");

        let server = MockServer::start().await;
        // `.expect(1)`：如果实现对非幂等请求也重试，这里会因为多余的请求
        // 而失败。
        Mock::given(method("POST"))
            .and(path("/api/v1/user/do-something"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let api = manager.api_for(Region::International);
        let started = std::time::Instant::now();
        let result = manager
            .call_protected(false, |token| {
                let api = &api;
                async move { api.post_authed("/api/v1/user/do-something", &token).await }
            })
            .await;
        let elapsed = started.elapsed();

        assert!(
            matches!(result, Err(SessionError::Transient(_))),
            "non-idempotent requests must surface a Transient error without replay, got {result:?}"
        );
        assert!(
            elapsed < Duration::from_millis(500),
            "non-idempotent requests must not wait for any backoff delay, took {elapsed:?}"
        );
    }

    // Codex 代码评审中危项 2：断网启动保留的会话（access_token 为空字符串）
    // 网络恢复后调用受保护接口，必须能自动刷新出真正的 access token 并成功
    // 重放，而不是先拿空 Bearer 发一次请求、被 SubPanel 判成 401
    // `EMPTY_TOKEN` 之后按未知 401 误终止会话。
    #[tokio::test]
    async fn call_protected_recovers_from_offline_retained_empty_access_token() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        let mut index = SessionIndex::default();
        index.set(
            Region::International,
            SessionIndexEntry {
                user_id: 42,
                email_masked: "u****@we2ai.com".to_string(),
            },
        );
        index.save(dir.path()).unwrap();
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 42),
                "refresh-1",
            )
            .unwrap();

        // 先离线恢复：resume_session 会把 access_token 设成空字符串。
        manager.set_test_base_url_override("http://127.0.0.1:1".to_string());
        let restored = manager.resume_session(Region::International).await;
        assert_eq!(restored, ResumeOutcome::OfflineRetained);

        // 网络恢复：refresh 与受保护接口都能正常应答。这里不给
        // "带空 Bearer 的 /api/v1/user/probe" 设置 mock——如果实现退化成先
        // 发一次空 token 的请求，wiremock 找不到匹配的 mock 会直接报错，
        // 测试会因为这个"意外请求"失败，从而验证空 token 从未被真的发出去。
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .and(wiremock::matchers::body_json(
                json!({ "refresh_token": "refresh-1" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "recovered-access", "refresh_token": "refresh-2", "expires_in": 3600, "token_type": "Bearer"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/probe"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer recovered-access",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"ok": true}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let api = manager.api_for(Region::International);
        let result = manager
            .call_protected(true, |token| {
                let api = &api;
                async move { api.get_authed("/api/v1/user/probe", &token).await }
            })
            .await
            .expect("should recover from an empty access token by refreshing first");
        assert_eq!(
            result.get("ok").and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert!(
            manager.summary().logged_in,
            "the session must not be terminated by the recovery flow"
        );
    }

    #[test]
    fn classify_protected_token_expired_retries_after_refresh() {
        let err = ApiCallError::Api(crate::we2ai::api::ApiError {
            code: ErrorCode::Named("TOKEN_EXPIRED".to_string()),
            status: 401,
            message: "expired".to_string(),
        });
        assert_eq!(
            classify_protected_error(&err),
            ProtectedOutcome::RetryAfterRefresh
        );
    }

    #[test]
    fn classify_protected_terminate_codes() {
        for code in PROTECTED_TERMINATE_CODES {
            let err = ApiCallError::Api(crate::we2ai::api::ApiError {
                code: ErrorCode::Named(code.to_string()),
                status: 401,
                message: "x".to_string(),
            });
            assert_eq!(
                classify_protected_error(&err),
                ProtectedOutcome::Terminate(code.to_string()),
                "code {code} should terminate the session"
            );
        }
    }

    #[test]
    fn classify_protected_unknown_401_terminates() {
        let err = ApiCallError::Api(crate::we2ai::api::ApiError {
            code: ErrorCode::Status(401),
            status: 401,
            message: "x".to_string(),
        });
        assert_eq!(
            classify_protected_error(&err),
            ProtectedOutcome::Terminate("UNKNOWN_401".to_string())
        );
    }

    #[test]
    fn classify_protected_network_and_429_and_5xx_retry_with_backoff() {
        let network = ApiCallError::Network(crate::we2ai::api::NetworkError("boom".to_string()));
        assert_eq!(
            classify_protected_error(&network),
            ProtectedOutcome::RetryWithBackoff
        );

        let rate_limited = ApiCallError::Api(crate::we2ai::api::ApiError {
            code: ErrorCode::Named("RATE_LIMITED".to_string()),
            status: 429,
            message: "x".to_string(),
        });
        assert_eq!(
            classify_protected_error(&rate_limited),
            ProtectedOutcome::RetryWithBackoff
        );

        let server_error = ApiCallError::Api(crate::we2ai::api::ApiError {
            code: ErrorCode::Status(503),
            status: 503,
            message: "x".to_string(),
        });
        assert_eq!(
            classify_protected_error(&server_error),
            ProtectedOutcome::RetryWithBackoff
        );
    }

    #[test]
    fn classify_refresh_terminate_codes() {
        for code in REFRESH_TERMINATE_CODES {
            let err = ApiCallError::Api(crate::we2ai::api::ApiError {
                code: ErrorCode::Named(code.to_string()),
                status: 401,
                message: "x".to_string(),
            });
            assert_eq!(
                classify_refresh_error(&err),
                RefreshOutcome::Terminate(code.to_string())
            );
        }
    }

    #[test]
    fn classify_refresh_network_and_5xx_retry_with_backoff() {
        let network = ApiCallError::Network(crate::we2ai::api::NetworkError("boom".to_string()));
        assert_eq!(
            classify_refresh_error(&network),
            RefreshOutcome::RetryWithBackoff
        );
        let server_error = ApiCallError::Api(crate::we2ai::api::ApiError {
            code: ErrorCode::Status(500),
            status: 500,
            message: "x".to_string(),
        });
        assert_eq!(
            classify_refresh_error(&server_error),
            RefreshOutcome::RetryWithBackoff
        );
    }

    #[tokio::test]
    async fn concurrent_protected_calls_trigger_exactly_one_refresh_and_keep_session() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store);
        seed_active(&manager, Region::International, 42, "refresh-1");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "new-access", "refresh_token": "refresh-2", "expires_in": 3600, "token_type": "Bearer"}
            })))
            .expect(1) // 关键断言：10 个并发调用只应触发 1 次 /auth/refresh。
            .mount(&server)
            .await;

        // 把内部 api_for() 指向 mock server：通过反射不可行，改为直接调用
        // ensure_refreshed 内部依赖的 do_refresh 逻辑——为此在 manager 上
        // 挂一个测试专用的 base_url 覆盖点。
        manager.set_test_base_url_override(server.uri());

        let mut handles = Vec::new();
        for _ in 0..10 {
            let m = manager.clone();
            handles.push(tokio::spawn(async move { m.ensure_refreshed().await }));
        }
        for h in handles {
            h.await
                .unwrap()
                .expect("refresh should succeed for all callers");
        }

        let summary = manager.summary();
        assert!(
            summary.logged_in,
            "session must be kept alive after refresh"
        );
    }

    // Codex 代码评审低危项 2：与验收原话逐字对应的用例——10 个 call_protected
    // 并发、受保护接口先返回带延迟的 TOKEN_EXPIRED，断言 refresh 恰好 1 次、
    // 会话保留、10 个请求均成功重放（不是只测 ensure_refreshed 本身，而是走
    // 完整的 call_protected 单飞 + 重放路径）。
    #[tokio::test]
    async fn ten_concurrent_call_protected_share_a_single_refresh_and_all_replay_successfully() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store);
        seed_active(&manager, Region::International, 42, "refresh-1");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "new-access", "refresh_token": "refresh-2", "expires_in": 3600, "token_type": "Bearer"}
            })))
            .expect(1) // 关键断言：10 个并发调用只应触发 1 次 /auth/refresh。
            .mount(&server)
            .await;
        // 带延迟的 TOKEN_EXPIRED：延迟拉宽 10 个并发调用互相重叠的时间窗口，
        // 让"多个调用同时判定需要刷新"这条竞态路径真的被覆盖到，而不是
        // 侥幸串行执行。
        Mock::given(method("GET"))
            .and(path("/api/v1/user/probe"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer old-access",
            ))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_delay(Duration::from_millis(30))
                    .set_body_json(json!({"code": "TOKEN_EXPIRED", "message": "expired"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/probe"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer new-access",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"ok": true}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let mut handles = Vec::new();
        for _ in 0..10 {
            let manager = manager.clone();
            handles.push(tokio::spawn(async move {
                let api = manager.api_for(Region::International);
                manager
                    .call_protected(true, |token| {
                        let api = &api;
                        async move { api.get_authed("/api/v1/user/probe", &token).await }
                    })
                    .await
            }));
        }
        for h in handles {
            let result = h
                .await
                .unwrap()
                .expect("every call must replay successfully");
            assert_eq!(
                result.get("ok").and_then(serde_json::Value::as_bool),
                Some(true)
            );
        }

        assert!(
            manager.summary().logged_in,
            "session must be kept alive after the shared refresh"
        );
    }

    // Opus 复核中危项 M5：两路并发 call_protected 同时撞上 access token 过期，
    // 第一次 refresh 遇到瞬时失败（503）、退避一次后第二次成功——断言 refresh
    // 总共只被调用 2 次（每次尝试各 1 次，不是 2 个调用者各自触发 2 次、共
    // 4 次）。两个 mock 都带延迟拉宽窗口，让两路调用真的有机会互相重叠、
    // 加入同一次尝试，而不是侥幸串行执行侥幸通过。
    #[tokio::test]
    async fn two_concurrent_call_protected_share_the_retry_after_a_transient_refresh_failure() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store);
        seed_active(&manager, Region::International, 42, "refresh-1");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(503).set_delay(Duration::from_millis(30)))
            .up_to_n_times(1)
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(30))
                    .set_body_json(json!({
                        "code": 0, "message": "success",
                        "data": {"access_token": "new-access", "refresh_token": "refresh-2", "expires_in": 3600, "token_type": "Bearer"}
                    })),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/probe"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer old-access",
            ))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_delay(Duration::from_millis(30))
                    .set_body_json(json!({"code": "TOKEN_EXPIRED", "message": "expired"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/probe"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer new-access",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"ok": true}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let mut handles = Vec::new();
        for _ in 0..2 {
            let manager = manager.clone();
            handles.push(tokio::spawn(async move {
                let api = manager.api_for(Region::International);
                manager
                    .call_protected(true, |token| {
                        let api = &api;
                        async move { api.get_authed("/api/v1/user/probe", &token).await }
                    })
                    .await
            }));
        }
        for h in handles {
            let result = h
                .await
                .unwrap()
                .expect("both concurrent calls must eventually succeed");
            assert_eq!(
                result.get("ok").and_then(serde_json::Value::as_bool),
                Some(true)
            );
        }

        assert!(manager.summary().logged_in);
        // 两个 refresh mock 各自的 `.expect(1)` 在 `server` 析构时校验：总共
        // 恰好 2 次 refresh 请求（不是两个调用者各自独立重试导致的 4 次）。
    }

    #[tokio::test]
    async fn network_error_during_refresh_keeps_credentials() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_active(&manager, Region::International, 42, "refresh-1");
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 42),
                "refresh-1",
            )
            .unwrap();

        // 指向一个不存在的端口模拟断网。
        manager.set_test_base_url_override("http://127.0.0.1:1".to_string());

        let err = manager.ensure_refreshed().await.unwrap_err();
        assert!(matches!(err, SessionError::Transient(_)));
        assert!(
            manager.summary().logged_in,
            "network failure must not clear session"
        );
        assert!(store.contains(
            secret_store::SERVICE_NAME,
            &secret_store::account_key("international", 42)
        ));
    }

    #[tokio::test]
    async fn refresh_token_reused_clears_keyring_and_terminates() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_active(&manager, Region::International, 42, "refresh-1");
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 42),
                "refresh-1",
            )
            .unwrap();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "code": "REFRESH_TOKEN_REUSED", "message": "reused"
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let err = manager.ensure_refreshed().await.unwrap_err();
        assert_eq!(
            err,
            SessionError::Terminated("REFRESH_TOKEN_REUSED".to_string())
        );
        assert!(!manager.summary().logged_in);
        assert!(!store.contains(
            secret_store::SERVICE_NAME,
            &secret_store::account_key("international", 42)
        ));
    }

    #[tokio::test]
    async fn keyring_write_failure_on_refresh_enters_needs_relogin() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_active(&manager, Region::International, 42, "refresh-1");
        store.set_fail_set(true);

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "new-access", "refresh_token": "refresh-2", "expires_in": 3600, "token_type": "Bearer"}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let err = manager.ensure_refreshed().await.unwrap_err();
        assert_eq!(err, SessionError::NeedsRelogin);
        assert!(!manager.summary().logged_in);
    }

    #[tokio::test]
    async fn stale_generation_write_is_discarded() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        let account = secret_store::account_key("international", 42);
        let old_generation = seed_active(&manager, Region::International, 42, "refresh-1");
        // 模拟"更新的操作"已经推进了会话 generation（如登出后又重新登录）：
        // 重新 seed 一次，state 里的 generation 随之前进。
        let new_generation = seed_active(&manager, Region::International, 42, "refresh-2");
        assert_ne!(old_generation, new_generation);

        let stale_pair = TokenPair {
            access_token: "stale-access".to_string(),
            refresh_token: "stale-refresh".to_string(),
            expires_in: 3600,
        };
        let stale_outcome = manager
            .commit_refreshed_token(Region::International, stale_pair, old_generation)
            .await;
        assert_eq!(
            stale_outcome,
            PersistOutcome::Stale,
            "stale generation write must be discarded"
        );
        assert!(
            !store.contains(secret_store::SERVICE_NAME, &account),
            "stale write must not reach the keyring"
        );

        // 用当前 generation 写入必须成功——证明上面的失败确实是 generation
        // 判定在起作用，而不是这条路径本来就写不进去。
        let fresh_pair = TokenPair {
            access_token: "fresh-access".to_string(),
            refresh_token: "fresh-refresh".to_string(),
            expires_in: 3600,
        };
        let fresh_outcome = manager
            .commit_refreshed_token(Region::International, fresh_pair, new_generation)
            .await;
        assert_eq!(
            fresh_outcome,
            PersistOutcome::Written,
            "matching generation write must succeed"
        );
        assert!(store.contains(secret_store::SERVICE_NAME, &account));
    }

    #[tokio::test]
    async fn logout_four_steps_and_revoked_true() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_active(&manager, Region::International, 42, "refresh-1");
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 42),
                "refresh-1",
            )
            .unwrap();
        let mut index = SessionIndex::default();
        index.set(
            Region::International,
            SessionIndexEntry {
                user_id: 42,
                email_masked: "u****@we2ai.com".to_string(),
            },
        );
        index.save(dir.path()).unwrap();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"message": "ok", "revoked": true}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let outcome = manager.logout().await;
        assert_eq!(outcome, LogoutOutcome::Revoked);
        assert!(!manager.summary().logged_in);
        assert!(!store.contains(
            secret_store::SERVICE_NAME,
            &secret_store::account_key("international", 42)
        ));
        let index_after = SessionIndex::load(dir.path());
        assert!(index_after.get(Region::International).is_none());
    }

    #[tokio::test]
    async fn logout_local_only_when_revoked_false() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store);
        seed_active(&manager, Region::International, 42, "refresh-1");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"message": "ok", "revoked": false}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let outcome = manager.logout().await;
        assert_eq!(outcome, LogoutOutcome::LocalOnly);
        assert!(
            !manager.summary().logged_in,
            "local state must still be cleared"
        );
    }

    // Codex 代码评审第 5 轮高危项 3："可靠作废"设计：索引、钥匙串两项清理
    // 里只要有一项成功，重启就必然走 NeedLogin，不会恢复该会话——即使
    // 远端也没有确认撤销。这里让索引清理失败、只有钥匙串删除成功。
    #[tokio::test]
    async fn logout_with_only_keyring_cleanup_succeeding_still_forces_need_login_on_restart() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        let mut index = SessionIndex::default();
        index.set(
            Region::International,
            SessionIndexEntry {
                user_id: 42,
                email_masked: "u****@we2ai.com".to_string(),
            },
        );
        index.save(dir.path()).unwrap();
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 42),
                "refresh-1",
            )
            .unwrap();
        seed_active(&manager, Region::International, 42, "refresh-1");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"message": "ok", "revoked": false}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        // 索引清理这一步强制失败，钥匙串删除保持正常。
        manager.set_test_force_index_save_failures(1);

        let outcome = manager.logout().await;
        assert_eq!(
            outcome,
            LogoutOutcome::LocalOnly,
            "one successful local cleanup step is enough, even though remote wasn't confirmed"
        );
        assert!(!manager.summary().logged_in);

        // 磁盘索引仍然指向这个账号（清理失败），但钥匙串条目已经没了。
        assert!(SessionIndex::load(dir.path())
            .get(Region::International)
            .is_some());
        assert!(!store.contains(
            secret_store::SERVICE_NAME,
            &secret_store::account_key("international", 42)
        ));

        // 模拟重启：索引指向这个账号，但钥匙串读不到 refresh_token，
        // resume_session 必须走 NeedLogin。
        let fresh_manager = manager_with_store(dir.path(), store.clone());
        fresh_manager.set_test_base_url_override(server.uri());
        let restart_outcome = fresh_manager.resume_session(Region::International).await;
        assert_eq!(restart_outcome, ResumeOutcome::NeedLogin);
    }

    // 同上的镜像：索引清理成功、钥匙串删除失败，重启同样必须走 NeedLogin。
    #[tokio::test]
    async fn logout_with_only_index_cleanup_succeeding_still_forces_need_login_on_restart() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        let mut index = SessionIndex::default();
        index.set(
            Region::International,
            SessionIndexEntry {
                user_id: 42,
                email_masked: "u****@we2ai.com".to_string(),
            },
        );
        index.save(dir.path()).unwrap();
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 42),
                "refresh-1",
            )
            .unwrap();
        seed_active(&manager, Region::International, 42, "refresh-1");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"message": "ok", "revoked": false}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        store.set_fail_delete(true);

        let outcome = manager.logout().await;
        assert_eq!(outcome, LogoutOutcome::LocalOnly);
        assert!(!manager.summary().logged_in);

        assert!(SessionIndex::load(dir.path())
            .get(Region::International)
            .is_none());
        assert!(store.contains(
            secret_store::SERVICE_NAME,
            &secret_store::account_key("international", 42)
        ));

        // Fable 终验中危项 2：钥匙串里仍留有服务端未撤销的 refresh，登记
        // 待清理并给出提示；钥匙串恢复后重试即可清掉。
        assert!(manager.summary().local_cleanup_pending);
        store.set_fail_delete(false);
        assert_eq!(
            manager.retry_local_cleanup().await,
            LogoutOutcome::LocalOnly
        );
        assert!(!manager.summary().local_cleanup_pending);
        assert!(!store.contains(
            secret_store::SERVICE_NAME,
            &secret_store::account_key("international", 42)
        ));
        store.set_fail_delete(true);

        let fresh_manager = manager_with_store(dir.path(), store.clone());
        fresh_manager.set_test_base_url_override(server.uri());
        let restart_outcome = fresh_manager.resume_session(Region::International).await;
        assert_eq!(restart_outcome, ResumeOutcome::NeedLogin);
    }

    fn seed_active_keyring_degraded(manager: &SessionManager, region: Region, user_id: i64) {
        let generation = manager.next_generation();
        manager.publish(SessionState::Active(ActiveSession {
            region,
            user_id,
            email_masked: "u****@we2ai.com".to_string(),
            refresh_token: "refresh-1".to_string(),
            access_token: "old-access".to_string(),
            keyring_degraded: true,
            index_degraded: false,
            generation,
        }));
    }

    fn seed_index_and_keyring(dir: &std::path::Path, store: &InMemorySecretStore, user_id: i64) {
        let mut index = SessionIndex::default();
        index.set(
            Region::International,
            SessionIndexEntry {
                user_id,
                email_masked: "u****@we2ai.com".to_string(),
            },
        );
        index.save(dir).unwrap();
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", user_id),
                "refresh-1",
            )
            .unwrap();
    }

    async fn mount_logout_not_revoked(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"message": "ok", "revoked": false}
            })))
            .mount(server)
            .await;
    }

    /// 钥匙串退化的会话（写不进钥匙串）登出时删除钥匙串同样会失败：索引已
    /// 作废、重启安全，不登记一个永远重试不掉的待清理提示。
    #[tokio::test]
    async fn keyring_degraded_session_logout_does_not_leave_an_unclearable_pending_cleanup() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_index_and_keyring(dir.path(), &store, 42);
        seed_active_keyring_degraded(&manager, Region::International, 42);
        let server = MockServer::start().await;
        mount_logout_not_revoked(&server).await;
        manager.set_test_base_url_override(server.uri());
        store.set_fail_delete(true);

        assert_eq!(manager.logout().await, LogoutOutcome::LocalOnly);
        assert!(!manager.summary().local_cleanup_pending);
    }

    /// Fable 终验中危项 1：恢复在锁外读到凭据之后、提交之前，登出第④步清掉
    /// 了本地凭据并置为 `LoggedOut`。恢复不得用旧读取把已登出的会话复活。
    /// 用钥匙串退化会话让残留 refresh 留在钥匙串且不登记待清理，排除
    /// `pending_cleanups` 拦截的干扰，只验证清理代次这道检查。
    #[tokio::test]
    async fn resume_read_before_logout_wipe_does_not_revive_the_logged_out_session() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_index_and_keyring(dir.path(), &store, 42);
        seed_active_keyring_degraded(&manager, Region::International, 42);
        let server = MockServer::start().await;
        mount_logout_not_revoked(&server).await;
        manager.set_test_base_url_override(server.uri());

        // 恢复读完索引后卡在钥匙串读取；此时尚未发生清理，放行后读到的
        // 仍是登出前的 refresh（钥匙串删除被注入失败，残留在原处）。
        let get_gate = store.block_next_get();
        let resumer = manager.clone();
        let resume_task =
            tokio::spawn(async move { resumer.resume_session(Region::International).await });
        let release_get = get_gate.wait_reached().await;

        store.set_fail_delete(true);
        assert_eq!(manager.logout().await, LogoutOutcome::LocalOnly);
        assert!(!manager.summary().logged_in);

        release_get.release();
        let outcome = resume_task.await.unwrap();
        assert_eq!(outcome, ResumeOutcome::NeedLogin);
        assert!(!manager.summary().logged_in);
    }

    // Codex 代码评审第 5 轮高危项 3：远端未确认撤销，且本地两项清理全部
    // 失败——不能报告"已退出"，必须是 `LocalCleanupFailed`；用一次"模拟
    // 重启"证明这不是过度谨慎：这种情况下重启真的会恢复出这个会话。
    #[tokio::test]
    async fn logout_reports_local_cleanup_failed_when_remote_unconfirmed_and_both_local_cleanups_fail(
    ) {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        let mut index = SessionIndex::default();
        index.set(
            Region::International,
            SessionIndexEntry {
                user_id: 42,
                email_masked: "u****@we2ai.com".to_string(),
            },
        );
        index.save(dir.path()).unwrap();
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 42),
                "refresh-1",
            )
            .unwrap();
        seed_active(&manager, Region::International, 42, "refresh-1");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"message": "ok", "revoked": false}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        manager.set_test_force_index_save_failures(1);
        store.set_fail_delete(true);

        let outcome = manager.logout().await;
        assert_eq!(
            outcome,
            LogoutOutcome::LocalCleanupFailed,
            "must not claim success when neither local cleanup step succeeded and remote didn't confirm either"
        );

        // 模拟重启：索引和钥匙串都还在，resume_session 必须能成功恢复——
        // 这正是 `LocalCleanupFailed` 存在的意义：如果这里悄悄报告
        // LocalOnly/已退出，用户会误以为账号安全，实际上重启会把它救活。
        let refresh_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "a", "refresh_token": "b", "expires_in": 3600, "token_type": "Bearer"}
            })))
            .mount(&refresh_server)
            .await;
        let fresh_manager = manager_with_store(dir.path(), store.clone());
        fresh_manager.set_test_base_url_override(refresh_server.uri());
        let restart_outcome = fresh_manager.resume_session(Region::International).await;
        assert_eq!(
            restart_outcome,
            ResumeOutcome::Restored,
            "this demonstrates why LocalCleanupFailed must not be reported as a success"
        );
    }

    // Codex 代码评审第 5 轮高危项 3：远端确认撤销（revoked: true）时，即使
    // 本地两项清理都失败，也应该报告 Revoked——服务端家族已经真的失效，
    // 残留的本地凭据不构成"重启误恢复一个仍然有效的会话"的风险。
    #[tokio::test]
    async fn logout_reports_revoked_even_when_both_local_cleanups_fail_if_remote_confirms() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_active(&manager, Region::International, 42, "refresh-1");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"message": "ok", "revoked": true}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        manager.set_test_force_index_save_failures(1);
        store.set_fail_delete(true);

        let outcome = manager.logout().await;
        assert_eq!(outcome, LogoutOutcome::Revoked);
        assert!(!manager.summary().logged_in);
    }

    #[test]
    fn logout_when_not_logged_in_reports_not_logged_in() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store);
        let outcome = futures::executor::block_on(manager.logout());
        assert_eq!(outcome, LogoutOutcome::NotLoggedIn);
    }

    #[tokio::test]
    async fn call_protected_retries_once_after_token_expired_via_single_refresh() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_active(&manager, Region::International, 42, "refresh-1");
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 42),
                "refresh-1",
            )
            .unwrap();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "new-access", "refresh_token": "refresh-2", "expires_in": 3600, "token_type": "Bearer"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        // 第一次带旧 access token 调用受保护接口 → TOKEN_EXPIRED；刷新后带新
        // access token 重放 → 成功。
        Mock::given(method("GET"))
            .and(path("/api/v1/user/probe"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer old-access",
            ))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "code": "TOKEN_EXPIRED", "message": "expired"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/probe"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer new-access",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"ok": true}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let api = manager.api_for(Region::International);
        let result = manager
            .call_protected(true, |token| {
                let api = &api;
                async move { api.get_authed("/api/v1/user/probe", &token).await }
            })
            .await
            .expect("should succeed after single-flight refresh + retry");
        assert_eq!(
            result.get("ok").and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert!(manager.summary().logged_in, "session must remain active");
    }

    // 偏差修复项 C（方案第 3.1/5.2 节"网络错误、429、5xx 保留凭证并退避
    // 重试"）：受保护接口过期触发的刷新本身遇到 429/5xx 时，`run_protected`
    // 必须像对待请求重放一样做有限次数快速退避重试，而不是把第一次
    // Transient 直接上抛终止整个调用。这里第一次 refresh 返回 503，退避一次
    // 后第二次 refresh 成功，最终请求应当成功、凭证（钥匙串里的 refresh
    // token）随之更新为轮转后的新值。
    #[tokio::test]
    async fn call_protected_retries_refresh_after_transient_failure_then_succeeds() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_active(&manager, Region::International, 42, "refresh-1");
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 42),
                "refresh-1",
            )
            .unwrap();

        let server = MockServer::start().await;
        // 第一次 refresh 遇到 503（瞬时失败）；退避一次后第二次 refresh 成功。
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "new-access", "refresh_token": "refresh-2", "expires_in": 3600, "token_type": "Bearer"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/probe"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer old-access",
            ))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "code": "TOKEN_EXPIRED", "message": "expired"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/probe"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer new-access",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"ok": true}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let api = manager.api_for(Region::International);
        let started = std::time::Instant::now();
        let result = manager
            .call_protected(true, |token| {
                let api = &api;
                async move { api.get_authed("/api/v1/user/probe", &token).await }
            })
            .await
            .expect("should retry the refresh itself with backoff and then succeed");
        let elapsed = started.elapsed();

        assert_eq!(
            result.get("ok").and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert!(
            manager.summary().logged_in,
            "credentials must not be cleared by a transient refresh failure"
        );
        assert!(
            elapsed >= Duration::from_millis(900),
            "the retry must wait out a backoff delay instead of hammering refresh immediately, took {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "must still resolve quickly (bounded fast-retry), took {elapsed:?}"
        );
        assert_eq!(
            store
                .get(
                    secret_store::SERVICE_NAME,
                    &secret_store::account_key("international", 42)
                )
                .unwrap(),
            Some("refresh-2".to_string()),
            "the rotated refresh token from the eventually-successful refresh must be persisted"
        );
    }

    // Opus 复核中危项 R4：总时限不能靠 `tokio::time::timeout` 包裹整个重试
    // 循环去强行取消一个已经发出的 refresh 请求——那样即便服务端已经处理
    // 完并轮转了 refresh token，客户端也会因为本地提前掐断而拿不到新
    // token，下次还会用这个已经失效的旧 token 去刷新，被服务端当成"已轮转
    // token 被重放"而撤销整条会话。这里让第二次（最终成功的）refresh 请求
    // 故意"很慢"（2 秒延迟，明显长于两次快速重试的退避窗口 1s+2s=3s 里
    // 单次退避的量级），断言这次慢请求依然被完整等待、其成功结果被正常
    // 采纳，而不是被提前判定失败或返回 Transient。
    #[tokio::test]
    async fn refresh_backoff_waits_out_a_slow_in_flight_refresh_request_instead_of_cancelling_it()
    {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_active(&manager, Region::International, 42, "refresh-1");
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 42),
                "refresh-1",
            )
            .unwrap();

        let server = MockServer::start().await;
        // 第一次 refresh 快速返回 503（进入退避分支）。
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        // 第二次 refresh 明显"慢"（2 秒），验证它不会被任何隐藏的超时机制
        // 提前取消——响应必须被完整等待并采纳。
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(2))
                    .set_body_json(json!({
                        "code": 0, "message": "success",
                        "data": {"access_token": "new-access", "refresh_token": "refresh-2", "expires_in": 3600, "token_type": "Bearer"}
                    })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/probe"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer old-access",
            ))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "code": "TOKEN_EXPIRED", "message": "expired"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/probe"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer new-access",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"ok": true}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let api = manager.api_for(Region::International);
        let started = std::time::Instant::now();
        let result = manager
            .call_protected(true, |token| {
                let api = &api;
                async move { api.get_authed("/api/v1/user/probe", &token).await }
            })
            .await
            .expect("the slow-but-eventually-successful refresh must not be cancelled early");
        let elapsed = started.elapsed();

        assert_eq!(
            result.get("ok").and_then(serde_json::Value::as_bool),
            Some(true)
        );
        // 至少要等满 2 秒的慢请求延迟（加上第一次退避的 1 秒），证明请求被
        // 完整等待，而不是被某个更短的隐藏超时提前打断。
        assert!(
            elapsed >= Duration::from_secs(2),
            "the in-flight refresh request must be awaited to completion, not cancelled, took {elapsed:?}"
        );
        assert_eq!(
            store
                .get(
                    secret_store::SERVICE_NAME,
                    &secret_store::account_key("international", 42)
                )
                .unwrap(),
            Some("refresh-2".to_string()),
            "the rotated refresh token from the slow-but-successful refresh must be persisted, \
             not lost to a cancelled request"
        );
    }

    // Codex 验收 X3：拆成两个独立用例，分别验证"截止前已发出的请求继续
    // 等待到完成"与"截止后不再发出新请求"——此前这两条挤在同一个用例里，
    // 而软时限检查的位置本身有错（放在决定要不要睡这一轮退避之前，而不是
    // 真正发起下一次请求之前），导致这个旧用例断言的其实是"睡眠跨过截止后
    // 仍然发出下一次请求"这个 bug 本身，误把它当成"不取消在途请求"的证据。
    //
    // 本例：deadline 设为 2 秒。第一次 refresh 快速 503，退避 1 秒（此时
    // 已耗时约 1 秒，仍小于 2 秒的截止，发起下一次请求前的检查放行）；第
    // 二次 refresh 故意延迟 3 秒才成功——发起时截止还没到，发起之后这次
    // 请求必须被完整等到，即便响应到达时（约 4 秒）已经明显晚于截止。
    #[tokio::test]
    async fn refresh_backoff_waits_out_a_request_dispatched_before_the_deadline_even_if_its_response_arrives_after_it(
    ) {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        manager.set_test_refresh_backoff_deadline(Duration::from_secs(2));
        seed_active(&manager, Region::International, 42, "refresh-1");
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 42),
                "refresh-1",
            )
            .unwrap();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        // 3 秒延迟：响应到达时（约 1s 退避 + 3s 延迟 = 4s）已经晚于 2 秒的
        // 截止，但这次请求是在截止之前（约 1 秒时）发出的，必须被完整等待。
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(3))
                    .set_body_json(json!({
                        "code": 0, "message": "success",
                        "data": {"access_token": "new-access", "refresh_token": "refresh-2", "expires_in": 3600, "token_type": "Bearer"}
                    })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/probe"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer old-access",
            ))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "code": "TOKEN_EXPIRED", "message": "expired"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/probe"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer new-access",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"ok": true}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let api = manager.api_for(Region::International);
        let started = std::time::Instant::now();
        let result = manager
            .call_protected(true, |token| {
                let api = &api;
                async move { api.get_authed("/api/v1/user/probe", &token).await }
            })
            .await
            .expect("a request dispatched before the deadline must still be awaited to completion even if its response arrives after the deadline");
        let elapsed = started.elapsed();

        assert_eq!(
            result.get("ok").and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert!(
            elapsed >= Duration::from_secs(4),
            "must wait out the full 1s backoff + 3s in-flight request despite the 2-second deadline having elapsed by the time the response arrives, took {elapsed:?}"
        );
        assert_eq!(
            store
                .get(
                    secret_store::SERVICE_NAME,
                    &secret_store::account_key("international", 42)
                )
                .unwrap(),
            Some("refresh-2".to_string()),
            "the rotated token must be persisted even though it arrived after the deadline"
        );
    }

    // Codex 验收 X3（第二例）：把总时限调到 1 秒，让 refresh 一直快速失败
    // （不延迟）——第一次退避睡满 1 秒后，截止已经用尽，发起第二次请求
    // 之前的检查必须拦下，全程只应该有 1 次 refresh 调用，即使按
    // `PROTECTED_FAST_RETRY_ATTEMPTS` 次数上限本来还有余量（会允许 3 次，
    // 见 `call_protected_refresh_backoff_exhausts_and_returns_transient`）。
    #[tokio::test]
    async fn refresh_backoff_stops_issuing_new_retries_once_the_test_deadline_has_elapsed() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store);
        manager.set_test_refresh_backoff_deadline(Duration::from_secs(1));
        seed_active(&manager, Region::International, 42, "refresh-1");

        let server = MockServer::start().await;
        // 只应该被调用一次：第一次退避睡满 1 秒后，截止已经用尽，发起
        // 第二次请求之前的检查必须提前拦下。
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/probe"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "code": "TOKEN_EXPIRED", "message": "expired"
            })))
            .expect(1)
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let api = manager.api_for(Region::International);
        let started = std::time::Instant::now();
        let result = manager
            .call_protected(true, |token| {
                let api = &api;
                async move { api.get_authed("/api/v1/user/probe", &token).await }
            })
            .await;
        let elapsed = started.elapsed();

        assert!(
            matches!(result, Err(SessionError::Transient(_))),
            "must give up once the (shortened) deadline has elapsed, got {result:?}"
        );
        assert!(
            elapsed >= Duration::from_secs(1) && elapsed < Duration::from_secs(3),
            "must stop after the single 1s backoff sleep once the deadline elapses, not fall back \
             to the attempt-count budget's second 2s sleep, took {elapsed:?}"
        );
        // `.expect(1)` 在 `server` 析构时校验：确认没有发生第二次 refresh 请求。
    }

    // 偏差修复项 C 的反面：refresh 持续失败（一直 429/5xx）时，重试必须在
    // `PROTECTED_FAST_RETRY_ATTEMPTS` 次内放弃并把最后一次错误当 Transient
    // 上抛，而不是无限重试或退化成分钟级的后台离线循环；凭证全程不清除。
    #[tokio::test]
    async fn call_protected_refresh_backoff_exhausts_and_returns_transient() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_active(&manager, Region::International, 42, "refresh-1");
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 42),
                "refresh-1",
            )
            .unwrap();

        let server = MockServer::start().await;
        // refresh 一直返回 503：受保护调用的刷新重试必须在有限次数内放弃。
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/probe"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "code": "TOKEN_EXPIRED", "message": "expired"
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let api = manager.api_for(Region::International);
        let started = std::time::Instant::now();
        let result = manager
            .call_protected(true, |token| {
                let api = &api;
                async move { api.get_authed("/api/v1/user/probe", &token).await }
            })
            .await;
        let elapsed = started.elapsed();

        assert!(
            matches!(result, Err(SessionError::Transient(_))),
            "exhausted refresh retries must surface as Transient, got {result:?}"
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "refresh retries must be bounded (not the unbounded background offline loop), took {elapsed:?}"
        );
        assert!(
            manager.summary().logged_in,
            "a transient refresh failure must not clear credentials or terminate the session"
        );
        assert_eq!(
            store
                .get(
                    secret_store::SERVICE_NAME,
                    &secret_store::account_key("international", 42)
                )
                .unwrap(),
            Some("refresh-1".to_string()),
            "refresh token in the keyring must be untouched"
        );
    }

    // Codex 代码评审高危项 2：refresh 进行中重新登录，旧 refresh 完成后不能
    // 清掉/覆盖新会话。
    #[tokio::test]
    async fn stale_refresh_completing_after_relogin_does_not_clobber_new_session() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store);
        seed_active(&manager, Region::International, 1, "old-session-refresh");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(80))
                    .set_body_json(json!({
                        "code": 0, "message": "success",
                        "data": {"access_token": "old-session-new-access", "refresh_token": "old-session-rotated-refresh", "expires_in": 3600, "token_type": "Bearer"}
                    })),
            )
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let refresh_task = {
            let m = manager.clone();
            tokio::spawn(async move { m.ensure_refreshed().await })
        };
        // 等待旧 refresh 的请求已经真正发出（进入 80ms 延迟）但还没完成，
        // 期间模拟"用户已经登出并重新登录了一个新账号"。
        tokio::time::sleep(Duration::from_millis(20)).await;
        seed_active(&manager, Region::International, 2, "new-session-refresh");

        let _ = refresh_task.await.unwrap();

        assert!(
            manager.summary().logged_in,
            "the new session must survive the stale refresh completing"
        );

        // 新会话的 refresh_token 必须还是它自己的（没有被旧 refresh 的轮转
        // 结果覆盖）：用一个只匹配 "new-session-refresh" 请求体的 mock 去验证
        // 下一次真正的 refresh 请求携带的是这个值。
        server.reset().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .and(wiremock::matchers::body_json(
                json!({ "refresh_token": "new-session-refresh" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "final-access", "refresh_token": "final-refresh", "expires_in": 3600, "token_type": "Bearer"}
            })))
            .mount(&server)
            .await;
        manager
            .ensure_refreshed()
            .await
            .expect("refresh using the new session's untouched refresh_token must succeed");
    }

    // Codex 代码评审高危项 3：refresh 在途时点登出，refresh 成功轮转后，
    // logout() 必须带着轮转后的最新 refresh_token 调用一次 /auth/logout，
    // 而不是提前把状态清成 LoggedOut 导致 logout() 直接短路成 NotLoggedIn。
    #[tokio::test]
    async fn logout_during_inflight_refresh_uses_latest_rotated_refresh_token() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_active(&manager, Region::International, 42, "refresh-1");
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 42),
                "refresh-1",
            )
            .unwrap();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(60))
                    .set_body_json(json!({
                        "code": 0, "message": "success",
                        "data": {"access_token": "new-access", "refresh_token": "rotated-refresh", "expires_in": 3600, "token_type": "Bearer"}
                    })),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .and(wiremock::matchers::body_json(
                json!({ "refresh_token": "rotated-refresh" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"message": "ok", "revoked": true}
            })))
            .expect(1)
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let refresh_task = {
            let m = manager.clone();
            tokio::spawn(async move { m.ensure_refreshed().await })
        };
        // 等 refresh 请求真正发出（进入 60ms 延迟）但还没完成，此时点登出。
        tokio::time::sleep(Duration::from_millis(15)).await;
        let outcome = manager.logout().await;

        let _ = refresh_task.await.unwrap();
        assert_eq!(outcome, LogoutOutcome::Revoked);
        assert!(!manager.summary().logged_in);
        assert!(!store.contains(
            secret_store::SERVICE_NAME,
            &secret_store::account_key("international", 42)
        ));
    }

    // Codex 代码评审高危项 4：单飞 refresh 的槽位清空必须只清自己发起/加入的
    // 那次，不能顶掉后来者的新 future。用真实并发 + 延迟制造竞态窗口，而不是
    // 只测"10 个调用者共享同一次 refresh"这个已经覆盖过的正常路径。
    #[tokio::test]
    async fn refresh_slot_is_not_cleared_by_a_stale_completion_racing_a_new_attempt() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store);
        seed_active(&manager, Region::International, 42, "refresh-1");

        let server = MockServer::start().await;
        // 只允许恰好一次 /auth/refresh：如果清槽位的竞态真的导致重复触发，
        // wiremock 在验证阶段会因为命中次数不为 1 而失败。
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(30)).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "new-access", "refresh_token": "refresh-2", "expires_in": 3600, "token_type": "Bearer"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        // 10 个协程近乎同时调用 ensure_refreshed，且故意让它们的启动时间错开
        // 几毫秒，覆盖"有的还没来得及加入、有的已经在清槽位"的时间窗口。
        let mut handles = Vec::new();
        for i in 0..10 {
            let m = manager.clone();
            handles.push(tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(i)).await;
                m.ensure_refreshed().await
            }));
        }
        for h in handles {
            let _ = h.await.unwrap();
        }

        assert!(manager.summary().logged_in);
    }

    // Codex 代码评审中危项 1：已经退化为内存模式的会话，轮转后只更新内存，
    // 不应该被判定为"钥匙串刚变得不可用"而强制登出。
    #[tokio::test]
    async fn refresh_on_already_degraded_session_stays_in_memory_without_forcing_relogin() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        let generation = {
            let g = manager.next_generation();
            manager.publish(SessionState::Active(ActiveSession {
                region: Region::International,
                user_id: 42,
                email_masked: "u****@we2ai.com".to_string(),
                refresh_token: "refresh-1".to_string(),
                access_token: "old-access".to_string(),
                keyring_degraded: true,
                index_degraded: false,
                generation: g,
            }));
            g
        };
        // 钥匙串本身此时可以正常写（不设 fail_set），验证"已退化"完全是靠
        // `keyring_degraded` 标记决定跳过写入，而不是因为钥匙串真的写不进去。

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "new-access", "refresh_token": "refresh-2", "expires_in": 3600, "token_type": "Bearer"}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        manager
            .ensure_refreshed()
            .await
            .expect("refreshing an already-degraded session must succeed in-memory");
        assert!(manager.summary().logged_in);
        assert!(
            manager.summary().keyring_degraded,
            "session must remain marked as degraded"
        );
        assert!(
            !store.contains(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 42)
            ),
            "an already-degraded session must not attempt to write the keyring"
        );
        let _ = generation;
    }

    // Codex 代码评审中危项 2：受保护接口重放拿到的结果要再走一次失败分类，
    // 而不是原样上抛——否则重放遇到 TOKEN_REVOKED 也不会终止会话。
    #[tokio::test]
    async fn call_protected_terminates_session_when_replay_gets_token_revoked() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_active(&manager, Region::International, 42, "refresh-1");
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 42),
                "refresh-1",
            )
            .unwrap();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "new-access", "refresh_token": "refresh-2", "expires_in": 3600, "token_type": "Bearer"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/probe"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer old-access",
            ))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "code": "TOKEN_EXPIRED", "message": "expired"
            })))
            .mount(&server)
            .await;
        // 重放（用刷新后的新 token）依然被拒——服务端判定该用户已被封禁/
        // token 已撤销，这在现实里会发生（比如两次请求之间账号被管理员冻结）。
        Mock::given(method("GET"))
            .and(path("/api/v1/user/probe"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer new-access",
            ))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "code": "TOKEN_REVOKED", "message": "revoked"
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let api = manager.api_for(Region::International);
        let result = manager
            .call_protected(true, |token| {
                let api = &api;
                async move { api.get_authed("/api/v1/user/probe", &token).await }
            })
            .await;

        assert_eq!(
            result.unwrap_err(),
            SessionError::Terminated("TOKEN_REVOKED".to_string())
        );
        assert!(
            !manager.summary().logged_in,
            "the session must be terminated when the replay itself is revoked"
        );
    }

    // Codex 代码评审第 3 轮高危项 1：区域 A 登录在途 → 切到区域 B 恢复
    // 成功 → A 的登录才完成。当前会话必须仍是 B，A 拿到的 refresh_token
    // 必须被尽力登出，不留孤儿 token 家族。
    #[tokio::test]
    async fn stale_login_completing_after_a_newer_region_switch_wins_is_discarded_and_logged_out() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());

        // 预置区域 B（DomesticProd）已经有可恢复的会话：索引 + 钥匙串条目。
        let mut index = SessionIndex::default();
        index.set(
            Region::DomesticProd,
            SessionIndexEntry {
                user_id: 99,
                email_masked: "b****@we2ai.com".to_string(),
            },
        );
        index.save(dir.path()).unwrap();
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("domestic_prod", 99),
                "b-refresh-1",
            )
            .unwrap();

        let server = MockServer::start().await;
        // 区域 A 的登录：带延迟，模拟"登录请求在途"。
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(150))
                    .set_body_json(json!({
                        "code": 0, "message": "success",
                        "data": {
                            "access_token": "a-access",
                            "refresh_token": "a-refresh",
                            "expires_in": 3600
                        }
                    })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/profile"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer a-access",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"id": 1, "email": "a@we2ai.com"}
            })))
            .mount(&server)
            .await;
        // 区域 B 的恢复：快速返回，先于 A 的登录完成。
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .and(wiremock::matchers::body_json(
                json!({ "refresh_token": "b-refresh-1" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "b-access", "refresh_token": "b-refresh-2", "expires_in": 3600, "token_type": "Bearer"}
            })))
            .mount(&server)
            .await;
        // A 的登录一旦被判定过期，必须尽力登出它的 refresh_token。
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .and(wiremock::matchers::body_json(
                json!({ "refresh_token": "a-refresh" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"revoked": true}
            })))
            .expect(1)
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let login_task = {
            let m = manager.clone();
            tokio::spawn(async move {
                m.login_email(Region::International, "a@we2ai.com", "pw", None)
                    .await
            })
        };
        // 确保区域 A 的登录请求已经真正发出、进入延迟期，再发起区域切换。
        tokio::time::sleep(Duration::from_millis(30)).await;
        let resume_outcome = manager.resume_session(Region::DomesticProd).await;
        assert_eq!(resume_outcome, ResumeOutcome::Restored);

        let login_result = login_task.await.unwrap();
        assert!(
            matches!(login_result, Err(SessionError::Transient(_))),
            "the stale login must be discarded once a newer region switch has already committed, got {login_result:?}"
        );

        let summary = manager.summary();
        assert!(summary.logged_in, "session B must still be active");
        assert_eq!(summary.region.as_deref(), Some("domestic_prod"));
        assert_eq!(summary.email_masked.as_deref(), Some("b****@we2ai.com"));
    }

    // 上面场景的镜像：区域 A 的恢复在途（比如刷新走得慢），期间用户切到
    // 区域 B 并直接登录成功。A 的恢复稍后完成，必须同样被丢弃，不能把
    // 已经生效的 B 会话覆盖回 A。
    #[tokio::test]
    async fn stale_resume_completing_after_a_newer_login_wins_does_not_clobber_the_new_session() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());

        let mut index = SessionIndex::default();
        index.set(
            Region::International,
            SessionIndexEntry {
                user_id: 1,
                email_masked: "a****@we2ai.com".to_string(),
            },
        );
        index.save(dir.path()).unwrap();
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 1),
                "a-refresh-1",
            )
            .unwrap();

        let server = MockServer::start().await;
        // 区域 A 的恢复刷新：带延迟。
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .and(wiremock::matchers::body_json(
                json!({ "refresh_token": "a-refresh-1" }),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(150))
                    .set_body_json(json!({
                        "code": 0, "message": "success",
                        "data": {"access_token": "a-access", "refresh_token": "a-refresh-2", "expires_in": 3600, "token_type": "Bearer"}
                    })),
            )
            .mount(&server)
            .await;
        // 区域 B 的登录：快速返回。
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "b-access", "refresh_token": "b-refresh", "expires_in": 3600}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/profile"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer b-access",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"id": 2, "email": "b@we2ai.com"}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let resume_task = {
            let m = manager.clone();
            tokio::spawn(async move { m.resume_session(Region::International).await })
        };
        tokio::time::sleep(Duration::from_millis(30)).await;
        manager
            .login_email(Region::DomesticProd, "b@we2ai.com", "pw", None)
            .await
            .expect("the newer login must succeed");

        // 过期的恢复不应该继续对着一个已经不属于它的会话做 ensure_refreshed；
        // 它的返回值只是"当前实际状态"的镜子（这里已登录），不代表它自己
        // 的刷新真的生效了。
        let _ = resume_task.await.unwrap();

        let summary = manager.summary();
        assert!(summary.logged_in, "session must still be logged in");
        assert_eq!(
            summary.region.as_deref(),
            Some("domestic_prod"),
            "the newer login (region B) must win, not the stale resume (region A)"
        );
        assert_eq!(summary.email_masked.as_deref(), Some("b****@we2ai.com"));
    }

    // Codex 代码评审第 3 轮高危项 2：新账号登录成功、钥匙串写入之后，会话
    // 索引写入失败——旧索引项必须被清掉（否则重启会用旧索引项的 user_id
    // 拼出旧账号的钥匙串 key，把它当成"可以免登录恢复"的账号），这次会话
    // 标记为 `index_degraded`（仅本次运行内有效），当前内存里的会话仍然
    // 可用。
    #[tokio::test]
    async fn login_with_failed_index_write_degrades_session_and_does_not_resurrect_the_old_account()
    {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());

        // 旧账号（user_id=1）已经登录过，索引和钥匙串里都有它的记录。
        let mut index = SessionIndex::default();
        index.set(
            Region::International,
            SessionIndexEntry {
                user_id: 1,
                email_masked: "old****@we2ai.com".to_string(),
            },
        );
        index.save(dir.path()).unwrap();
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 1),
                "old-refresh",
            )
            .unwrap();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "new-access", "refresh_token": "new-refresh", "expires_in": 3600}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/profile"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer new-access",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"id": 2, "email": "new@we2ai.com"}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        // 只强制第一次索引写入（写入新账号那次）失败，紧接着的"删除旧索引
        // 项"的补救写入正常成功——这是一次典型的瞬时磁盘故障。
        manager.set_test_force_index_save_failures(1);

        manager
            .login_email(Region::International, "new@we2ai.com", "pw", None)
            .await
            .expect("login itself must still succeed even though the index write failed");

        // 当前进程内的会话必须是新账号，且标记为 index_degraded。
        let summary = manager.summary();
        assert!(summary.logged_in);
        assert_eq!(summary.region.as_deref(), Some("international"));
        assert_eq!(summary.email_masked.as_deref(), Some("ne****@we2ai.com"));
        assert!(
            summary.index_degraded,
            "a failed index write must be surfaced so the frontend can warn about it"
        );

        // 磁盘上的索引项必须已经被删除——不是旧账号，也不是新账号。
        let on_disk = SessionIndex::load(dir.path());
        assert!(
            on_disk.get(Region::International).is_none(),
            "the stale index entry must be removed, not left pointing at the old account"
        );

        // 模拟重启：用同一个钥匙串 store、同一个 data_root 构造一个全新的
        // `SessionManager`（内存状态清零），resume 必须回登录页，绝不能
        // 悄悄恢复旧账号（它的 refresh_token 在钥匙串里原封不动，如果索引
        // 没被清理，旧的 bug 会让这里恢复出 user_id=1 的旧账号）。
        let fresh_manager = manager_with_store(dir.path(), store.clone());
        fresh_manager.set_test_base_url_override(server.uri());
        let restart_outcome = fresh_manager.resume_session(Region::International).await;
        assert_eq!(
            restart_outcome,
            ResumeOutcome::NeedLogin,
            "restart must require a fresh login instead of resurrecting the old account"
        );
        assert!(!fresh_manager.summary().logged_in);
    }

    // Codex 代码评审第 4 轮高危项 2（索引持续写失败的处理，扩展场景）：
    // 新写入 + 尽力清理旧索引项两次都失败，但旧账号的钥匙串条目本身删除
    // 成功——磁盘上的索引仍然指向旧账号，重启必须仍然是 NeedLogin，因为
    // 旧账号的钥匙串条目已经不在了（不是靠索引干净，而是靠钥匙串这条腿
    // 本身失效）。
    #[tokio::test]
    async fn login_survives_two_consecutive_index_write_failures_without_resurrecting_the_old_account(
    ) {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());

        let mut index = SessionIndex::default();
        index.set(
            Region::International,
            SessionIndexEntry {
                user_id: 1,
                email_masked: "old****@we2ai.com".to_string(),
            },
        );
        index.save(dir.path()).unwrap();
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 1),
                "old-refresh",
            )
            .unwrap();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "new-access", "refresh_token": "new-refresh", "expires_in": 3600}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/profile"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer new-access",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"id": 2, "email": "new@we2ai.com"}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        // 两次都失败：写新账号那次、以及紧接着"尽力删除该区域索引项"的那次
        // 补救写入，都会强制失败——磁盘上的索引最终仍然是旧账号那条。
        manager.set_test_force_index_save_failures(2);

        let outcome = manager
            .login_email(Region::International, "new@we2ai.com", "pw", None)
            .await
            .expect("login must still succeed: the old keyring entry was successfully invalidated");
        assert!(matches!(outcome, LoginOutcome::LoggedIn));

        let summary = manager.summary();
        assert!(summary.logged_in);
        assert_eq!(summary.email_masked.as_deref(), Some("ne****@we2ai.com"));
        assert!(summary.index_degraded);

        // 磁盘索引仍然指向旧账号（两次补救写入都失败了）。
        let on_disk = SessionIndex::load(dir.path());
        assert_eq!(
            on_disk.get(Region::International).map(|e| e.user_id),
            Some(1),
            "both index writes failed, so the stale entry is still on disk"
        );
        // 但旧账号的钥匙串条目已经被删掉——重启即使读到这条指向旧账号的
        // 索引，也拿不到可用的 refresh_token。
        assert!(!store.contains(
            secret_store::SERVICE_NAME,
            &secret_store::account_key("international", 1)
        ));

        let fresh_manager = manager_with_store(dir.path(), store.clone());
        fresh_manager.set_test_base_url_override(server.uri());
        let restart_outcome = fresh_manager.resume_session(Region::International).await;
        assert_eq!(
            restart_outcome,
            ResumeOutcome::NeedLogin,
            "the old account's keyring entry is gone, so restart must not resurrect it"
        );
    }

    // Codex 代码评审第 4 轮高危项 2：索引写失败、且旧账号的钥匙串条目也
    // 删不掉——重启无法保证不会恢复旧账号，这次登录必须整体报错（不能
    // 报告成功），并且尽力撤销这次新签发的 refresh_token。
    #[tokio::test]
    async fn login_reports_persist_failed_and_revokes_new_refresh_when_old_credential_cannot_be_invalidated(
    ) {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());

        let mut index = SessionIndex::default();
        index.set(
            Region::International,
            SessionIndexEntry {
                user_id: 1,
                email_masked: "old****@we2ai.com".to_string(),
            },
        );
        index.save(dir.path()).unwrap();
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 1),
                "old-refresh",
            )
            .unwrap();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "new-access", "refresh_token": "new-refresh", "expires_in": 3600}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/profile"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer new-access",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"id": 2, "email": "new@we2ai.com"}
            })))
            .mount(&server)
            .await;
        // 新签发的 refresh 必须被尽力撤销一次。
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .and(wiremock::matchers::body_json(
                json!({ "refresh_token": "new-refresh" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"revoked": true}
            })))
            .expect(1)
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        manager.set_test_force_index_save_failures(2);
        store.set_fail_delete(true); // 旧账号的钥匙串条目怎么删都删不掉。

        let result = manager
            .login_email(Region::International, "new@we2ai.com", "pw", None)
            .await;
        assert!(
            matches!(result, Err(SessionError::PersistFailed(_))),
            "got {result:?}"
        );
        assert!(
            !manager.summary().logged_in,
            "must not report a logged-in session when the old credential can't be invalidated"
        );
    }

    // Codex 代码评审第 4 轮中危项 2：离线重试循环停止后立即重新开始，旧
    // 循环必须在唤醒后发现自己已经被取代而退出，不会和新循环同时生效
    // （不会触发两次真正的 refresh）。
    #[tokio::test]
    async fn stopping_and_immediately_restarting_the_offline_retry_loop_never_runs_two_loops_concurrently(
    ) {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_active(&manager, Region::International, 42, "refresh-1");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "new-access", "refresh_token": "refresh-2", "expires_in": 3600, "token_type": "Bearer"}
            })))
            // 关键断言：即使新旧两个循环在极短时间内先后启动，最终也只应该
            // 真正触发一次成功的 /auth/refresh——旧循环唤醒后发现自己的
            // loop_id 已经不是当前记录的那个，必须直接退出，不能继续操作
            // 新循环的状态或者自己再发一次请求。
            .expect(1)
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        manager.start_offline_retry();
        let first_id = manager
            .0
            .offline_retry
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.loop_id);
        assert!(first_id.is_some());

        // 立即停止、立即重新启动，模拟"停止后立即重新开始"的时序——旧循环
        // 这时大概率还在它的第一次 backoff sleep（1 秒）里，尚未醒来做第一
        // 次检查。
        manager.clear_offline_retry();
        manager.start_offline_retry();
        let second_id = manager
            .0
            .offline_retry
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.loop_id);
        assert!(second_id.is_some());
        assert_ne!(first_id, second_id);

        // 等过第一次 backoff（1 秒）之后的一小段缓冲时间，让两个循环都有
        // 机会醒来并各自决定是否行动。
        tokio::time::sleep(Duration::from_millis(1400)).await;

        assert_eq!(
            manager.summary().offline_retry_in_seconds,
            None,
            "the surviving loop must have successfully refreshed and cleared the retry state"
        );
        assert!(manager.summary().logged_in);
    }

    // Codex 代码评审第 5 轮中危项 4：旧循环已经醒来、进入 `ensure_refreshed()`
    // 之后（卡在钥匙串写入这一步），一个新循环启动并取代了 `offline_retry`
    // 里记录的状态；旧循环的这次刷新完成时，必须发现自己已经不是"当前"
    // 循环，不能无条件清空——那会把新循环的状态清没。
    #[tokio::test]
    async fn old_loop_in_flight_refresh_completing_after_a_new_loop_started_does_not_clear_it() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_active(&manager, Region::International, 42, "refresh-1");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "new-access", "refresh_token": "refresh-2", "expires_in": 3600, "token_type": "Bearer"}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        // 手动布置"旧循环"状态：loop_id=1，下一次尝试时间已经过去，模拟它
        // 醒来后即将进入 `ensure_refreshed()`。
        *manager.0.offline_retry.lock().unwrap() = Some(OfflineRetryState {
            loop_id: 1,
            attempt: 0,
            next_attempt_at: Instant::now(),
        });

        // 让旧循环这次刷新卡在钥匙串写入这一步。
        let block = store.block_next_set();
        let old_loop_task = {
            let m = manager.clone();
            tokio::spawn(async move { m.run_offline_retry_loop(1).await })
        };
        let release = block.wait_reached().await;

        // "新循环"：在旧循环仍卡在它自己的 `ensure_refreshed()` 里没有返回
        // 时，取代 `offline_retry` 记录的状态（对应另一次触发全新离线重试
        // 的场景，比如另一个区域独立进入了离线状态）。
        *manager.0.offline_retry.lock().unwrap() = Some(OfflineRetryState {
            loop_id: 2,
            attempt: 0,
            next_attempt_at: Instant::now() + Duration::from_secs(3600),
        });

        // 放开旧循环的钥匙串写入，让它的 `ensure_refreshed()` 成功走完。
        release.release();
        let _ = old_loop_task.await;

        let current_id = manager
            .0
            .offline_retry
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.loop_id);
        assert_eq!(
            current_id,
            Some(2),
            "the old loop's completion must not clear the newer loop's state"
        );
    }

    // Codex 代码评审第 4/5 轮交错测试：A 登录发起网络请求之后（代次号已经
    // 在发起请求前领取），B 发起一次切到完全没有索引的区域——B 的
    // `NeedLogin` 决定本身就在 commit_lock 内领号，必须让 A 的登录作废，
    // 最终会话保持未登录。commit_new_session 现在是"网络请求结束后单次
    // 持锁完成校验+写入+发布"（第 5 轮高危项 1 的结构性修复），锁内不再有
    // 可以被打断的窗口，因此这里改用 [`RequestGate`] 卡住 A 的登录请求——
    // 确认它已经真正到达 mock server（而不是靠固定延迟猜时机——Codex 代码
    // 评审第 6 轮中危项 2）之后再发起 B，全程不依赖任何 sleep/忙等。
    #[tokio::test]
    async fn interleave_login_write_window_lets_a_concurrent_no_index_region_switch_invalidate_it()
    {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());

        let server = MockServer::start().await;
        let (gate, reached_rx) = request_gate(
            ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "a-access", "refresh_token": "a-refresh", "expires_in": 3600}
            })),
            Duration::from_millis(300),
        );
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .respond_with(gate)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/profile"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"id": 1, "email": "a@we2ai.com"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .and(wiremock::matchers::body_json(
                json!({ "refresh_token": "a-refresh" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"revoked": true}
            })))
            .expect(1)
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let login_task = {
            let m = manager.clone();
            tokio::spawn(async move {
                m.login_email(Region::International, "a@we2ai.com", "pw", None)
                    .await
            })
        };
        // 确认 A 的登录请求已经真正到达 mock server（`begin_login_epoch`
        // 在发起登录请求之前调用，此时代次号已经领到），确保下面 B 的领号
        // 严格发生在 A 之后。A 的响应还要再等 300ms 才会真正送达。
        wait_for_request(reached_rx).await;

        // B：切到一个完全没有索引的区域。领号本身（代次推进到 2）已经让
        // A 的登录作废，不需要等 A 的网络请求或钥匙串写入。
        let resume_outcome = manager.resume_session(Region::DomesticProd).await;
        assert_eq!(resume_outcome, ResumeOutcome::NeedLogin);

        // A 的登录响应到达后会继续走到 commit_lock 内的代次复查，此时
        // 发现代次已经被 B 推进，判定过期——不会做任何钥匙串/索引写入
        // （新设计：复查在任何持久化 I/O 之前）。
        let login_result = login_task.await.unwrap();
        assert!(
            matches!(login_result, Err(SessionError::Transient(_))),
            "got {login_result:?}"
        );

        assert!(
            !manager.summary().logged_in,
            "no session should end up active: A was invalidated, B never had anything to restore"
        );
        assert!(
            !store.contains(
                secret_store::SERVICE_NAME,
                &secret_store::account_key("international", 1)
            ),
            "the invalidated login must never have written its refresh_token to the keyring"
        );
        assert!(
            SessionIndex::load(dir.path())
                .get(Region::International)
                .is_none(),
            "the invalidated login must never have written a session index entry"
        );
    }

    // Codex 代码评审第 5 轮高危项 1：同一个账号被两个并发登录同时提交，
    // 较早发起的那个（A）在网络在途期间被较晚发起的那个（B）的代次号
    // 抢先推进；A 的响应回来后在 commit_lock 内复查代次发现已经过期，
    // 完全不做任何钥匙串/索引写入就放弃——最终钥匙串与索引必须是 B（唯一
    // 真正提交的一方）的值，A 不留下任何持久化残留。附加"模拟重启"断言。
    #[tokio::test]
    async fn same_account_double_login_race_leaves_only_the_final_committers_credentials() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());

        let server = MockServer::start().await;
        // A 的网络阶段正常完成，随后停在提交放行点（见 `Inner::commit_gate`），
        // 不依赖任何响应延迟。
        let a_login_response = ResponseTemplate::new(200).set_body_json(json!({
            "code": 0, "message": "success",
            "data": {"access_token": "a-access", "refresh_token": "a-refresh", "expires_in": 3600}
        }));
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .and(wiremock::matchers::body_json(json!({
                "email": "same@we2ai.com", "password": "a-pw"
            })))
            .respond_with(a_login_response)
            .mount(&server)
            .await;
        // B：快速响应，先于 A 完成整个提交。
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .and(wiremock::matchers::body_json(json!({
                "email": "same@we2ai.com", "password": "b-pw"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "b-access", "refresh_token": "b-refresh", "expires_in": 3600}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/profile"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"id": 1, "email": "same@we2ai.com"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .and(wiremock::matchers::body_json(
                json!({ "refresh_token": "a-refresh" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"revoked": true}
            })))
            .expect(1)
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let account = secret_store::account_key("international", 1);

        let (a_reached, a_release) = manager.set_test_commit_gate();
        let a_task = {
            let m = manager.clone();
            tokio::spawn(async move {
                m.login_email(Region::International, "same@we2ai.com", "a-pw", None)
                    .await
            })
        };
        // A 已完成网络阶段并停在提交放行点（代次号早已领到）。此时执行一次
        // **真实的** B 登录：B 领取更新的代次、完整提交；随后才放行 A。
        a_reached.await.expect("A never reached the commit gate");
        let b_result = manager
            .login_email(Region::International, "same@we2ai.com", "b-pw", None)
            .await;
        assert!(matches!(b_result, Ok(LoginOutcome::LoggedIn)));
        a_release.send(()).expect("A dropped its commit gate");

        let a_result = a_task.await.unwrap();
        assert!(
            matches!(a_result, Err(SessionError::Transient(_))),
            "got {a_result:?}"
        );

        // 最终钥匙串与索引都必须是 B 的值。
        assert_eq!(
            store.get(secret_store::SERVICE_NAME, &account).unwrap(),
            Some("b-refresh".to_string()),
            "the keyring must hold the final committer's (B's) refresh token"
        );
        let on_disk = SessionIndex::load(dir.path());
        assert_eq!(
            on_disk
                .get(Region::International)
                .map(|e| e.email_masked.as_str()),
            Some("sa****@we2ai.com")
        );
        let summary = manager.summary();
        assert!(summary.logged_in);
        assert_eq!(summary.email_masked.as_deref(), Some("sa****@we2ai.com"));

        // 模拟重启：新的 SessionManager 用同一份钥匙串 + 索引恢复，必须
        // 恢复出 B 的会话（能用 "b-refresh" 成功刷新），而不是曾经短暂
        // 存在过、但从未真正写入任何东西的 A。
        let server2 = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .and(wiremock::matchers::body_json(
                json!({ "refresh_token": "b-refresh" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "restarted-access", "refresh_token": "restarted-refresh", "expires_in": 3600, "token_type": "Bearer"}
            })))
            .mount(&server2)
            .await;
        let fresh_manager = manager_with_store(dir.path(), store.clone());
        fresh_manager.set_test_base_url_override(server2.uri());
        let restart_outcome = fresh_manager.resume_session(Region::International).await;
        assert_eq!(restart_outcome, ResumeOutcome::Restored);
    }

    // Codex 代码评审第 5 轮低危项 7：登出正在进行（`LoggingOut`）时收到一次
    // 登录提交——不能覆盖 `LoggingOut`，必须直接拒绝并返回可重试的错误，
    // 让调用方（前端）之后重新发起登录；登出本身应该不受影响地正常完成。
    #[tokio::test]
    async fn login_commit_is_rejected_while_a_logout_is_in_progress() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_active(&manager, Region::International, 1, "old-refresh");

        let server = MockServer::start().await;
        // 登出的远端请求被信号屏障卡住，确定性地制造"登出在途"窗口（Codex
        // 代码评审第 6 轮中危项 2：不用固定延迟猜时机）。
        let (gate, reached_rx) = request_gate(
            ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"revoked": true}
            })),
            Duration::from_millis(2000),
        );
        // 只有原登出（old-refresh）走延迟屏障；被拒登录撤销孤儿 token
        // （new-refresh）的登出立即返回，避免它也被卡住、拖到原登出完成。
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .and(wiremock::matchers::body_json(
                json!({ "refresh_token": "old-refresh" }),
            ))
            .respond_with(gate)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .and(wiremock::matchers::body_json(
                json!({ "refresh_token": "new-refresh" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"revoked": true}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "new-access", "refresh_token": "new-refresh", "expires_in": 3600}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/profile"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"id": 2, "email": "new@we2ai.com"}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        // 登录先完成网络阶段并停在提交放行点。
        let (login_reached, login_release) = manager.set_test_commit_gate();
        let login_task = {
            let m = manager.clone();
            tokio::spawn(async move {
                m.login_email(Region::International, "new@we2ai.com", "pw", None)
                    .await
            })
        };
        login_reached
            .await
            .expect("login never reached the commit gate");

        // 再发起登出，等到其远端请求真正到达（此时状态已是 LoggingOut）。
        let logout_task = {
            let m = manager.clone();
            tokio::spawn(async move { m.logout().await })
        };
        wait_for_request(reached_rx).await;
        assert!(matches!(
            &*manager.current_state(),
            SessionState::LoggingOut(_)
        ));

        // 放行登录提交：它必须看到 LoggingOut 并被拒绝。
        login_release.send(()).expect("login dropped its gate");
        let login_result = login_task.await.unwrap();
        assert!(
            matches!(login_result, Err(SessionError::Transient(_))),
            "a login commit must be rejected while a logout is in progress, got {login_result:?}"
        );
        assert!(
            matches!(&*manager.current_state(), SessionState::LoggingOut(_)),
            "the LoggingOut state must not be overwritten by the rejected login"
        );

        let logout_outcome = logout_task.await.unwrap();
        assert_eq!(
            logout_outcome,
            LogoutOutcome::Revoked,
            "the in-progress logout must complete normally, unaffected by the rejected login"
        );
        assert!(!manager.summary().logged_in);
    }

    // Codex 代码评审第 4 轮交错测试：旧 refresh 检查 generation 之后、写
    // 钥匙串期间被假钥匙串卡住（持续持有 commit_lock，符合"同一次持有内
    // 完成"的设计），同账号的新登录只能排队等待；旧 refresh 写完释放锁后
    // 新登录才真正执行，最终钥匙串必须是新登录的 token。
    #[tokio::test]
    async fn refresh_write_blocked_by_commit_lock_lets_a_queued_same_account_relogin_win_the_final_keyring_value(
    ) {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        let account = secret_store::account_key("international", 1);
        store
            .set(secret_store::SERVICE_NAME, &account, "old-refresh")
            .unwrap();
        seed_active(&manager, Region::International, 1, "old-refresh");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "rotated-access", "refresh_token": "rotated-refresh", "expires_in": 3600, "token_type": "Bearer"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "new-login-access", "refresh_token": "new-login-refresh", "expires_in": 3600}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/profile"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"id": 1, "email": "same@we2ai.com"}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let block = store.block_next_set();

        let refresh_task = {
            let m = manager.clone();
            tokio::spawn(async move { m.ensure_refreshed().await })
        };
        // 确认旧 refresh 真的已经卡在写钥匙串这一步（此时它持续持有
        // commit_lock），不用 sleep 猜时机（Codex 代码评审第 5 轮低危项 6）。
        let release = block.wait_reached().await;

        // 新登录此刻即使已经开始排队等待 commit_lock 也没关系——旧 refresh
        // 正持有这把锁（卡在写钥匙串），新登录的 `begin_login_epoch()` 必须
        // 等旧 refresh 完全释放锁之后才能拿到，先 spawn 还是先 release 不
        // 影响最终顺序，commit_lock 的互斥性保证了这一点。
        let login_task = {
            let m = manager.clone();
            tokio::spawn(async move {
                m.login_email(Region::International, "same@we2ai.com", "pw", None)
                    .await
            })
        };

        release.release();

        let _refresh_result = refresh_task.await.unwrap();
        let login_result = login_task.await.unwrap();
        assert!(login_result.is_ok(), "got {login_result:?}");

        assert_eq!(
            store.get(secret_store::SERVICE_NAME, &account).unwrap().as_deref(),
            Some("new-login-refresh"),
            "the final keyring value must be the newer login's token, not the stale refresh's rotated one"
        );
        assert_eq!(
            manager.summary().email_masked.as_deref(),
            Some("sa****@we2ai.com")
        );
    }

    // Codex 代码评审第 4 轮交错测试：终止清理（置 LoggedOut + 删钥匙串）
    // 被假钥匙串卡在删除那一步，期间持续持有 commit_lock；同账号的新登录
    // 只能排队，直到清理完全结束才真正执行，新登录的持久凭据不会被清理
    // 动作误删。
    #[tokio::test]
    async fn cleanup_blocked_on_keyring_delete_lets_a_queued_relogin_survive_with_fresh_credentials(
    ) {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        let account = secret_store::account_key("international", 1);
        store
            .set(secret_store::SERVICE_NAME, &account, "old-refresh")
            .unwrap();
        let mut index = SessionIndex::default();
        index.set(
            Region::International,
            SessionIndexEntry {
                user_id: 1,
                email_masked: "o****@we2ai.com".to_string(),
            },
        );
        index.save(dir.path()).unwrap();
        seed_active(&manager, Region::International, 1, "old-refresh");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "code": "TOKEN_REVOKED", "message": "revoked"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "new-access", "refresh_token": "new-refresh", "expires_in": 3600}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/profile"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"id": 1, "email": "same@we2ai.com"}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let block = store.block_next_delete();

        let terminate_task = {
            let m = manager.clone();
            tokio::spawn(async move { m.ensure_refreshed().await })
        };
        // 确认终止清理真的已经卡在删钥匙串这一步（此时它持续持有
        // commit_lock），不用 sleep 猜时机（Codex 代码评审第 5 轮低危项 6）。
        let release = block.wait_reached().await;

        // 新登录此刻即使已经开始排队等待 commit_lock 也没关系——终止清理
        // 正持有这把锁，新登录的 `begin_login_epoch()` 必须等清理完全释放
        // 锁之后才能拿到。
        let login_task = {
            let m = manager.clone();
            tokio::spawn(async move {
                m.login_email(Region::International, "same@we2ai.com", "pw", None)
                    .await
            })
        };

        release.release();

        let terminate_result = terminate_task.await.unwrap();
        assert!(matches!(terminate_result, Err(SessionError::Terminated(_))));
        let login_result = login_task.await.unwrap();
        assert!(login_result.is_ok(), "got {login_result:?}");

        assert!(manager.summary().logged_in);
        assert_eq!(
            store
                .get(secret_store::SERVICE_NAME, &account)
                .unwrap()
                .as_deref(),
            Some("new-refresh"),
            "the relogin's credential must survive the earlier termination cleanup"
        );
    }

    // Codex 代码评审第 4 轮中危项 1：钥匙串写入被真实系统卡住（如 macOS
    // Keychain 弹出授权对话框）期间，`summary()`/`call_protected()` 必须
    // 不受影响、立即返回——它们只读快照，不涉及 commit_lock 或钥匙串 I/O。
    #[tokio::test]
    async fn summary_and_call_protected_are_not_blocked_while_a_keyring_write_is_pending() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_active(&manager, Region::International, 42, "refresh-1");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "new-access", "refresh_token": "refresh-2", "expires_in": 3600, "token_type": "Bearer"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/probe"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"ok": true}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        // 让下一次钥匙串写入阻塞，直到测试主动释放。
        let block = store.block_next_set();

        let refresh_task = {
            let m = manager.clone();
            tokio::spawn(async move { m.ensure_refreshed().await })
        };
        // 确认后台 refresh 真的已经卡在钥匙串写入那一步（不用 sleep 猜
        // 时机——Codex 代码评审第 5 轮低危项 6）。
        let release = block.wait_reached().await;

        let summary_result =
            tokio::time::timeout(Duration::from_millis(300), async { manager.summary() }).await;
        assert!(
            summary_result.is_ok(),
            "summary() must not be blocked by a pending keyring write"
        );

        let api = manager.api_for(Region::International);
        let call_result = tokio::time::timeout(
            Duration::from_millis(300),
            manager.call_protected(true, |token| {
                let api = &api;
                async move { api.get_authed("/api/v1/user/probe", &token).await }
            }),
        )
        .await;
        assert!(
            call_result.is_ok(),
            "call_protected must not be blocked by a pending keyring write"
        );
        assert!(call_result.unwrap().is_ok());

        release.release();
        let _ = refresh_task.await;
    }

    // ---- Codex 代码评审第 6 轮（第 4 轮验收）补测 ----

    /// 预置某区域的会话索引项与钥匙串条目（模拟"此前登录过、已持久化"）。
    fn seed_persisted(
        dir: &std::path::Path,
        store: &InMemorySecretStore,
        region: Region,
        user_id: i64,
        refresh: &str,
    ) {
        let mut index = SessionIndex::load(dir);
        index.set(
            region,
            SessionIndexEntry {
                user_id,
                email_masked: "u****@we2ai.com".to_string(),
            },
        );
        index.save(dir).unwrap();
        store
            .set(
                secret_store::SERVICE_NAME,
                &secret_store::account_key(region.storage_key(), user_id),
                refresh,
            )
            .unwrap();
    }

    // 高危项 1：call_protected 收到旧会话的 TOKEN_EXPIRED 时，若会话已切到
    // 新账号或新区域，必须返回 SessionChanged，既不刷新也不把新会话的
    // token 拿去重放旧请求。新会话故意使用与旧会话相同的 access token
    // 字符串——这正是"只比较 token 字符串"的旧实现会误判为"无需刷新、直接
    // 重放"的形态。
    #[tokio::test]
    async fn call_protected_stops_with_session_changed_when_account_or_region_switches_mid_call() {
        for (new_region, new_user) in [
            (Region::International, 99_i64),
            (Region::DomesticProd, 42_i64),
        ] {
            let dir = TempDir::new().unwrap();
            let store = Arc::new(InMemorySecretStore::new());
            let manager = manager_with_store(dir.path(), store.clone());
            seed_active(&manager, Region::International, 42, "refresh-1");

            let server = MockServer::start().await;
            let (gate, reached_rx) = request_gate(
                ResponseTemplate::new(401).set_body_json(json!({
                    "code": "TOKEN_EXPIRED", "message": "Token has expired"
                })),
                Duration::from_millis(300),
            );
            Mock::given(method("GET"))
                .and(path("/api/v1/user/probe"))
                .respond_with(gate)
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/api/v1/auth/refresh"))
                .respond_with(ResponseTemplate::new(500))
                .expect(0)
                .mount(&server)
                .await;
            manager.set_test_base_url_override(server.uri());

            let api = manager.api_for(Region::International);
            let call_task = {
                let m = manager.clone();
                tokio::spawn(async move {
                    m.call_protected(true, |token| {
                        let api = &api;
                        async move { api.get_authed("/api/v1/user/probe", &token).await }
                    })
                    .await
                })
            };
            // 旧请求已经真正到达服务端（响应还要再等 300ms），此时切换会话。
            wait_for_request(reached_rx).await;
            seed_active(&manager, new_region, new_user, "refresh-new");

            let result: Result<serde_json::Value, SessionError> = call_task.await.unwrap();
            assert!(
                matches!(result, Err(SessionError::SessionChanged)),
                "switch to {new_region:?}/{new_user} must stop the old call, got {result:?}"
            );
            // 新会话未被触碰。
            assert!(
                matches!(&*manager.current_state(), SessionState::Active(s) if s.user_id == new_user && s.region == new_region)
            );
            server.verify().await;
        }
    }

    // 高危项 2：登出的网络请求在途（LoggingOut）时，同区域的会话恢复不能把
    // 状态改回 Active；登出随后必须照常完成远端撤销与本地清理。
    #[tokio::test]
    async fn resume_during_logout_does_not_revive_the_session_or_skip_cleanup() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_persisted(dir.path(), &store, Region::International, 42, "refresh-1");
        seed_active(&manager, Region::International, 42, "refresh-1");

        let server = MockServer::start().await;
        let (gate, reached_rx) = request_gate(
            ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"revoked": true}
            })),
            Duration::from_millis(300),
        );
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .respond_with(gate)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let logout_task = {
            let m = manager.clone();
            tokio::spawn(async move { m.logout().await })
        };
        wait_for_request(reached_rx).await;
        assert!(matches!(
            &*manager.current_state(),
            SessionState::LoggingOut(_)
        ));

        let _ = manager.resume_session(Region::International).await;
        assert!(
            matches!(&*manager.current_state(), SessionState::LoggingOut(_)),
            "resume must not overwrite LoggingOut"
        );

        assert_eq!(logout_task.await.unwrap(), LogoutOutcome::Revoked);
        assert!(matches!(&*manager.current_state(), SessionState::LoggedOut));
        assert!(!store.contains(
            secret_store::SERVICE_NAME,
            &secret_store::account_key("international", 42)
        ));
        assert!(SessionIndex::load(dir.path())
            .get(Region::International)
            .is_none());
        server.verify().await;
    }

    // 高危项 3：远端未确认 + 本地双失败后，专门的本地清理重试在故障解除后
    // 能完成清理，模拟重启走 NeedLogin；同时待清理期间同区域
    // 的恢复不会把刚登出的会话救活。
    #[tokio::test]
    async fn retry_local_cleanup_completes_after_failures_clear_and_restart_needs_login() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_persisted(dir.path(), &store, Region::International, 42, "refresh-1");
        seed_active(&manager, Region::International, 42, "refresh-1");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"revoked": false}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "a", "refresh_token": "b", "expires_in": 3600}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        manager.set_test_force_index_save_failures(1);
        store.set_fail_delete(true);
        assert_eq!(manager.logout().await, LogoutOutcome::LocalCleanupFailed);
        assert!(matches!(&*manager.current_state(), SessionState::LoggedOut));
        assert_eq!(
            manager
                .0
                .pending_cleanups
                .lock()
                .unwrap()
                .get(&Region::International),
            Some(&42)
        );

        // 清理待重试期间，同区域恢复不得复活会话，待清理目标保持不变。
        assert_eq!(
            manager.resume_session(Region::International).await,
            ResumeOutcome::NeedLogin
        );
        assert!(matches!(&*manager.current_state(), SessionState::LoggedOut));
        assert_eq!(
            manager
                .0
                .pending_cleanups
                .lock()
                .unwrap()
                .get(&Region::International),
            Some(&42)
        );

        // 故障解除后重试成功。
        store.set_fail_delete(false);
        assert_eq!(
            manager.retry_local_cleanup().await,
            LogoutOutcome::LocalOnly
        );
        assert!(matches!(&*manager.current_state(), SessionState::LoggedOut));

        // 模拟重启：不得恢复出已登出的会话。
        let fresh = manager_with_store(dir.path(), store.clone());
        fresh.set_test_base_url_override(server.uri());
        assert_eq!(
            fresh.resume_session(Region::International).await,
            ResumeOutcome::NeedLogin
        );
    }

    // 中危项 1：同区域同账号再次登录时新 refresh 写钥匙串失败——该账号
    // 自己的旧 refresh 不能留在钥匙串被重启拿去恢复会话。
    #[tokio::test]
    async fn same_account_relogin_with_keyring_write_failure_does_not_restore_old_refresh_on_restart(
    ) {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_persisted(dir.path(), &store, Region::International, 1, "old-refresh");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "new-access", "refresh_token": "new-refresh", "expires_in": 3600}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/profile"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"id": 1, "email": "same@we2ai.com"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "x", "refresh_token": "y", "expires_in": 3600}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        store.set_fail_set(true);
        let result = manager
            .login_email(Region::International, "same@we2ai.com", "pw", None)
            .await;
        assert!(
            matches!(result, Ok(LoginOutcome::LoggedIn)),
            "got {result:?}"
        );
        assert!(manager.summary().keyring_degraded);
        store.set_fail_set(false);

        // 旧 refresh 已被清除，重启不会恢复会话。
        assert!(!store.contains(
            secret_store::SERVICE_NAME,
            &secret_store::account_key("international", 1)
        ));
        let fresh = manager_with_store(dir.path(), store.clone());
        fresh.set_test_base_url_override(server.uri());
        assert_eq!(
            fresh.resume_session(Region::International).await,
            ResumeOutcome::NeedLogin
        );
    }

    // 中危项 3：last_region 写入失败必须上报，而不是被吞掉。
    #[test]
    fn set_last_region_reports_write_failure() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store);
        // 让目标路径是一个目录，原子写必然失败。
        std::fs::create_dir(dir.path().join("last_region.json")).unwrap();
        assert!(manager.set_last_region(Region::DomesticProd).is_err());
    }

    // ---- Codex 验收第 5 轮补测 ----

    fn login_mocks_json() -> (serde_json::Value, serde_json::Value) {
        (
            json!({"code": 0, "message": "success",
                   "data": {"access_token": "new-access", "refresh_token": "new-refresh", "expires_in": 3600}}),
            json!({"code": 0, "message": "success", "data": {"id": 1, "email": "same@we2ai.com"}}),
        )
    }

    async fn mount_login(server: &MockServer) {
        let (login, profile) = login_mocks_json();
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(login))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/profile"))
            .respond_with(ResponseTemplate::new(200).set_body_json(profile))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "x", "refresh_token": "y", "expires_in": 3600}
            })))
            .mount(server)
            .await;
    }

    // 高危项 1：同账号再次登录时钥匙串写失败，且索引首次写入与补救删除都
    // 失败——读回校验发现索引仍指向带旧 refresh 的钥匙串条目，删除该条目后
    // 才发布会话；重启不会恢复旧会话。
    #[tokio::test]
    async fn keyring_and_double_index_failure_on_same_account_relogin_is_not_restorable_on_restart()
    {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_persisted(dir.path(), &store, Region::International, 1, "old-refresh");
        let server = MockServer::start().await;
        mount_login(&server).await;
        manager.set_test_base_url_override(server.uri());

        store.set_fail_set(true);
        manager.set_test_force_index_save_failures(2);
        let result = manager
            .login_email(Region::International, "same@we2ai.com", "pw", None)
            .await;
        assert!(
            matches!(result, Ok(LoginOutcome::LoggedIn)),
            "got {result:?}"
        );
        store.set_fail_set(false);

        assert!(!store.contains(
            secret_store::SERVICE_NAME,
            &secret_store::account_key("international", 1)
        ));
        let fresh = manager_with_store(dir.path(), store.clone());
        fresh.set_test_base_url_override(server.uri());
        assert_eq!(
            fresh.resume_session(Region::International).await,
            ResumeOutcome::NeedLogin
        );
    }

    // 高危项 1（无法作废分支）：同上组合故障，且旧钥匙串条目删不掉、读回
    // 后的索引清理也失败——不得报告登录成功，新 refresh 被撤销。
    #[tokio::test]
    async fn login_fails_with_persist_failed_when_stale_credential_cannot_be_invalidated_by_any_means(
    ) {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_persisted(dir.path(), &store, Region::International, 1, "old-refresh");
        let server = MockServer::start().await;
        mount_login(&server).await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .and(wiremock::matchers::body_json(
                json!({ "refresh_token": "new-refresh" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"revoked": true}
            })))
            .expect(1)
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        store.set_fail_set(true);
        store.set_fail_delete(true);
        manager.set_test_force_index_save_failures(10);
        let result = manager
            .login_email(Region::International, "same@we2ai.com", "pw", None)
            .await;
        assert!(
            matches!(result, Err(SessionError::PersistFailed(_))),
            "got {result:?}"
        );
        assert!(!manager.summary().logged_in);
        server.verify().await;
    }

    // 高危项 2：会话终止（refresh 返回 BACKEND_MODE_ACTIVE）时本地两项清理
    // 都失败——登记到 pending_cleanups 并在摘要中暴露，重试成功后重启走
    // NeedLogin。
    #[tokio::test]
    async fn terminate_with_double_cleanup_failure_keeps_pending_cleanup_and_retry_clears_it() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_persisted(dir.path(), &store, Region::International, 42, "refresh-1");
        seed_active(&manager, Region::International, 42, "refresh-1");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "code": 403, "message": "backend mode", "reason": "BACKEND_MODE_ACTIVE"
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        manager.set_test_force_index_save_failures(1);
        store.set_fail_delete(true);
        let result = manager.ensure_refreshed().await;
        assert!(
            matches!(result, Err(SessionError::Terminated(ref c)) if c == "BACKEND_MODE_ACTIVE"),
            "got {result:?}"
        );
        assert!(matches!(&*manager.current_state(), SessionState::LoggedOut));
        assert_eq!(
            manager
                .0
                .pending_cleanups
                .lock()
                .unwrap()
                .get(&Region::International),
            Some(&42)
        );
        let summary = manager.summary();
        assert!(!summary.logged_in);
        assert!(summary.local_cleanup_pending);

        store.set_fail_delete(false);
        assert_eq!(
            manager.retry_local_cleanup().await,
            LogoutOutcome::LocalOnly
        );
        assert!(!manager.summary().local_cleanup_pending);
        let fresh = manager_with_store(dir.path(), store.clone());
        fresh.set_test_base_url_override(server.uri());
        assert_eq!(
            fresh.resume_session(Region::International).await,
            ResumeOutcome::NeedLogin
        );
    }

    // 中危项：refresh 收到无 reason 的 400（未知非 401 失败）——保留会话与
    // 凭据，只作为普通失败上抛。
    #[tokio::test]
    async fn refresh_unknown_400_keeps_the_session_and_credentials() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_persisted(dir.path(), &store, Region::International, 42, "refresh-1");
        seed_active(&manager, Region::International, 42, "refresh-1");
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "code": 400, "message": "bad request"
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let result = manager.ensure_refreshed().await;
        assert!(
            matches!(result, Err(SessionError::Other(_))),
            "got {result:?}"
        );
        assert!(matches!(&*manager.current_state(), SessionState::Active(s) if s.user_id == 42));
        assert!(store.contains(
            secret_store::SERVICE_NAME,
            &secret_store::account_key("international", 42)
        ));
    }

    #[test]
    fn classify_refresh_unknown_non_401_is_a_non_terminal_failure() {
        let named_400 = ApiCallError::Api(crate::we2ai::api::ApiError {
            code: ErrorCode::Named("SOME_VALIDATION_ERROR".to_string()),
            status: 400,
            message: "x".to_string(),
        });
        assert_eq!(
            classify_refresh_error(&named_400),
            RefreshOutcome::Failed("SOME_VALIDATION_ERROR".to_string())
        );
        let bare_401 = ApiCallError::Api(crate::we2ai::api::ApiError {
            code: ErrorCode::Status(401),
            status: 401,
            message: "x".to_string(),
        });
        assert!(matches!(
            classify_refresh_error(&bare_401),
            RefreshOutcome::Terminate(_)
        ));
    }

    // 中危项：旧请求的成功响应晚于会话切换到达——不能把旧会话的数据交给
    // 调用方。
    #[tokio::test]
    async fn call_protected_success_arriving_after_session_switch_returns_session_changed() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_active(&manager, Region::International, 42, "refresh-1");
        let server = MockServer::start().await;
        let (gate, reached_rx) = request_gate(
            ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"owner": 42}
            })),
            Duration::from_millis(300),
        );
        Mock::given(method("GET"))
            .and(path("/api/v1/user/probe"))
            .respond_with(gate)
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        let api = manager.api_for(Region::International);
        let call_task = {
            let m = manager.clone();
            tokio::spawn(async move {
                m.call_protected(true, |token| {
                    let api = &api;
                    async move { api.get_authed("/api/v1/user/probe", &token).await }
                })
                .await
            })
        };
        wait_for_request(reached_rx).await;
        seed_active(&manager, Region::International, 99, "refresh-new");
        let result: Result<serde_json::Value, SessionError> = call_task.await.unwrap();
        assert!(
            matches!(result, Err(SessionError::SessionChanged)),
            "got {result:?}"
        );
    }

    // ---- Codex 验收第 6 轮补测 ----

    // 高危项 1：同账号再次登录，钥匙串写失败、索引写成功，但读回索引时读取
    // 失败——不能按"无条目"放行；索引被覆盖写成已知内容（钥匙串失败时为空），
    // 重启不会恢复带旧 refresh 的会话。
    #[tokio::test]
    async fn index_read_failure_during_read_back_resets_index_so_restart_cannot_restore_stale_session(
    ) {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_persisted(dir.path(), &store, Region::International, 1, "old-refresh");
        let server = MockServer::start().await;
        mount_login(&server).await;
        manager.set_test_base_url_override(server.uri());

        store.set_fail_set(true);
        manager.set_test_force_index_read_failures(1);
        let result = manager
            .login_email(Region::International, "same@we2ai.com", "pw", None)
            .await;
        assert!(
            matches!(result, Ok(LoginOutcome::LoggedIn)),
            "got {result:?}"
        );
        store.set_fail_set(false);

        // 旧 refresh 仍在钥匙串里，但索引已被重置，重启无从定位它。
        assert!(SessionIndex::load(dir.path())
            .get(Region::International)
            .is_none());
        let fresh = manager_with_store(dir.path(), store.clone());
        fresh.set_test_base_url_override(server.uri());
        assert_eq!(
            fresh.resume_session(Region::International).await,
            ResumeOutcome::NeedLogin
        );
    }

    // 高危项 2：区域 A 终止时本地双失败 → 切到区域 B 并恢复成功 → A 的待清理
    // 目标仍在（不被 B 的 Active 覆盖），提示仍在；切回 A 不会复活；重试后
    // 清掉，重启走 NeedLogin。
    #[tokio::test]
    async fn pending_cleanup_survives_switching_to_another_region_and_blocks_reviving_it() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_persisted(dir.path(), &store, Region::International, 42, "refresh-a");
        seed_persisted(dir.path(), &store, Region::DomesticProd, 7, "refresh-b");
        seed_active(&manager, Region::International, 42, "refresh-a");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .and(wiremock::matchers::body_json(
                json!({ "refresh_token": "refresh-a" }),
            ))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "code": 403, "message": "backend mode", "reason": "BACKEND_MODE_ACTIVE"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/refresh"))
            .and(wiremock::matchers::body_json(json!({ "refresh_token": "refresh-b" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"access_token": "b-access", "refresh_token": "refresh-b2", "expires_in": 3600}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        manager.set_test_force_index_save_failures(1);
        store.set_fail_delete(true);
        assert!(matches!(
            manager.ensure_refreshed().await,
            Err(SessionError::Terminated(_))
        ));
        store.set_fail_delete(false);
        assert_eq!(
            manager
                .0
                .pending_cleanups
                .lock()
                .unwrap()
                .get(&Region::International),
            Some(&42)
        );

        // 切到 B：恢复成功，A 的待清理目标保留，提示仍在。
        assert_eq!(
            manager.resume_session(Region::DomesticProd).await,
            ResumeOutcome::Restored
        );
        let summary = manager.summary();
        assert!(summary.logged_in);
        assert!(summary.local_cleanup_pending);
        assert_eq!(
            manager
                .0
                .pending_cleanups
                .lock()
                .unwrap()
                .get(&Region::International),
            Some(&42)
        );

        // 切回 A：不得从残留凭据复活；当前 B 会话不受影响。
        assert_eq!(
            manager.resume_session(Region::International).await,
            ResumeOutcome::NeedLogin
        );
        assert!(
            matches!(&*manager.current_state(), SessionState::Active(s) if s.region == Region::DomesticProd)
        );

        // 重试清理成功，A 的残留清除；重启恢复 A 走 NeedLogin。
        assert_eq!(
            manager.retry_local_cleanup().await,
            LogoutOutcome::LocalOnly
        );
        assert!(!manager.summary().local_cleanup_pending);
        let fresh = manager_with_store(dir.path(), store.clone());
        fresh.set_test_base_url_override(server.uri());
        assert_eq!(
            fresh.resume_session(Region::International).await,
            ResumeOutcome::NeedLogin
        );
    }

    // ---- Codex 验收第 7 轮补测 ----

    // 高危项 1：区域 A 的账号 X 本地清理双失败待清理 → 在 A 以另一账号 Y 登录，
    // 旧账号钥匙串仍删不掉 → X 的待清理记录保留；恢复 A 恢复的是 Y、不被拦；
    // 重试按 X 定位，不会删掉 Y 的索引；钥匙串可删后清理完成。
    #[tokio::test]
    async fn relogin_as_another_account_keeps_pending_cleanup_until_old_keyring_entry_is_deleted() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_persisted(dir.path(), &store, Region::International, 42, "refresh-x");
        seed_active(&manager, Region::International, 42, "refresh-x");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"revoked": false}
            })))
            .mount(&server)
            .await;
        mount_login(&server).await; // 登录返回 user id 1、refresh "new-refresh"
        manager.set_test_base_url_override(server.uri());

        manager.set_test_force_index_save_failures(1);
        store.set_fail_delete(true);
        assert_eq!(manager.logout().await, LogoutOutcome::LocalCleanupFailed);
        assert_eq!(
            manager
                .0
                .pending_cleanups
                .lock()
                .unwrap()
                .get(&Region::International),
            Some(&42)
        );

        // 以账号 Y（id 1）登录同一区域；旧账号 X 的钥匙串仍删不掉。
        let result = manager
            .login_email(Region::International, "same@we2ai.com", "pw", None)
            .await;
        assert!(
            matches!(result, Ok(LoginOutcome::LoggedIn)),
            "got {result:?}"
        );
        assert_eq!(
            manager
                .0
                .pending_cleanups
                .lock()
                .unwrap()
                .get(&Region::International),
            Some(&42),
            "X's pending cleanup must survive while its keyring entry still exists"
        );
        assert!(manager.summary().local_cleanup_pending);
        assert!(store.contains(
            secret_store::SERVICE_NAME,
            &secret_store::account_key("international", 42)
        ));

        // 恢复 A：索引指向 Y，不受 X 的残留拦截。
        assert_eq!(
            manager.resume_session(Region::International).await,
            ResumeOutcome::Restored
        );
        assert!(matches!(&*manager.current_state(), SessionState::Active(s) if s.user_id == 1));

        // 重试仍失败：不得删掉 Y 的索引。
        assert_eq!(
            manager.retry_local_cleanup().await,
            LogoutOutcome::LocalCleanupFailed
        );
        assert_eq!(
            SessionIndex::load(dir.path())
                .get(Region::International)
                .map(|e| e.user_id),
            Some(1)
        );

        // 钥匙串恢复可用后重试成功：X 的条目删除，Y 的索引保留。
        store.set_fail_delete(false);
        assert_eq!(
            manager.retry_local_cleanup().await,
            LogoutOutcome::LocalOnly
        );
        assert!(!manager.summary().local_cleanup_pending);
        assert!(!store.contains(
            secret_store::SERVICE_NAME,
            &secret_store::account_key("international", 42)
        ));
        assert_eq!(
            SessionIndex::load(dir.path())
                .get(Region::International)
                .map(|e| e.user_id),
            Some(1)
        );
    }

    // ---- Codex 验收第 8 轮补测 ----

    // 高危项 1：登出时索引**读取**失败、钥匙串删除也失败、远端未确认撤销——
    // 不得判为清理成功；登记待清理并报告 LocalCleanupFailed，索引文件本身
    // 不被改写。
    #[tokio::test]
    async fn logout_with_index_read_failure_and_keyring_delete_failure_is_not_reported_as_cleaned()
    {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_persisted(dir.path(), &store, Region::International, 42, "refresh-1");
        seed_active(&manager, Region::International, 42, "refresh-1");
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"revoked": false}
            })))
            .mount(&server)
            .await;
        manager.set_test_base_url_override(server.uri());

        manager.set_test_force_index_read_failures(1);
        store.set_fail_delete(true);
        assert_eq!(manager.logout().await, LogoutOutcome::LocalCleanupFailed);
        assert_eq!(
            manager
                .0
                .pending_cleanups
                .lock()
                .unwrap()
                .get(&Region::International),
            Some(&42)
        );
        // 读取失败时没有改写索引：条目仍在，需要后续重试来清理。
        assert_eq!(
            SessionIndex::load(dir.path())
                .get(Region::International)
                .map(|e| e.user_id),
            Some(42)
        );
        store.set_fail_delete(false);
        assert_eq!(
            manager.retry_local_cleanup().await,
            LogoutOutcome::LocalOnly
        );
        assert!(SessionIndex::load(dir.path())
            .get(Region::International)
            .is_none());
    }

    // 高危项 2：X 待清理 → 同区域登录 Y → 登出 Y 使索引无条目 → X 的钥匙串
    // 仍删不掉时重试不得移出 X；可删后才移出。
    #[tokio::test]
    async fn pending_old_account_is_not_dropped_when_index_is_empty_but_its_keyring_entry_remains()
    {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let manager = manager_with_store(dir.path(), store.clone());
        seed_persisted(dir.path(), &store, Region::International, 42, "refresh-x");
        seed_active(&manager, Region::International, 42, "refresh-x");
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .and(wiremock::matchers::body_json(
                json!({ "refresh_token": "refresh-x" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"revoked": false}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/logout"))
            .and(wiremock::matchers::body_json(
                json!({ "refresh_token": "new-refresh" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success", "data": {"revoked": true}
            })))
            .mount(&server)
            .await;
        mount_login(&server).await; // Y：id 1、refresh "new-refresh"
        manager.set_test_base_url_override(server.uri());

        manager.set_test_force_index_save_failures(1);
        store.set_fail_delete(true);
        assert_eq!(manager.logout().await, LogoutOutcome::LocalCleanupFailed);

        assert!(matches!(
            manager
                .login_email(Region::International, "same@we2ai.com", "pw", None)
                .await,
            Ok(LoginOutcome::LoggedIn)
        ));
        assert_eq!(manager.logout().await, LogoutOutcome::Revoked);
        assert!(SessionIndex::load(dir.path())
            .get(Region::International)
            .is_none());

        // 索引已无条目，但 X 的钥匙串仍删不掉：不得移出。
        assert_eq!(
            manager.retry_local_cleanup().await,
            LogoutOutcome::LocalCleanupFailed
        );
        assert_eq!(
            manager
                .0
                .pending_cleanups
                .lock()
                .unwrap()
                .get(&Region::International),
            Some(&42)
        );
        assert!(manager.summary().local_cleanup_pending);

        store.set_fail_delete(false);
        assert_eq!(
            manager.retry_local_cleanup().await,
            LogoutOutcome::LocalOnly
        );
        assert!(!store.contains(
            secret_store::SERVICE_NAME,
            &secret_store::account_key("international", 42)
        ));
        assert!(!manager.summary().local_cleanup_pending);
    }
}
