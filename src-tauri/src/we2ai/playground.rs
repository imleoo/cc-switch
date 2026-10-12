//! AI 营销工坊（`docs/we2ai/AI营销工坊设计方案.md`，功能 27，P1 文本对话）。
//!
//! - 明文 Key 复用模型广场缓存（`We2aiKeyState::active_secret_for`），
//!   不新增明文通道：请求由 Rust 侧直接发往网关 `POST {base_url}/v1/chat/completions`
//!   （SSE 流式），明文不出 IPC。
//! - 请求生命周期：`we2ai_playground_chat` 阻塞至终态（成功/失败/取消），增量
//!   delta 经 [`PLAYGROUND_CHUNK_EVENT`] 事件推送；每个请求恰有一个终态，终态后
//!   从任务表移除，迟到数据丢弃。
//! - 身份取消链路（方案第 5.3 节）：任务按完整 [`SessionIdentity`]（区域 + 用户 +
//!   代次）登记；登出路径调用 [`PlaygroundState::cancel_all`]，任务内每次事件
//!   投递前与周期 tick 都复查身份，不一致即中断。
//! - 错误契约：稳定 code + 本地化文案，未知上游响应不透传 body（原始错误只写
//!   log）。
//!
//! 分层：HTTP 流式/SSE 解析在 [`run_chat_stream`]（不依赖 Tauri 与 SessionManager，
//! wiremock 端到端测试）；命令层只做身份、Key 校验与事件转发。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::State;
use tokio::sync::Notify;

use super::api::USER_AGENT;
use super::commands_auth::We2aiApiError;
use super::keys::We2aiKeyState;
use super::session::{SessionIdentity, SessionManager, We2aiSessionState};

/// 前端增量事件名（Rust → 前端）。守卫 4.17 校验两侧一致。
pub const PLAYGROUND_CHUNK_EVENT: &str = "we2ai-playground-chunk";

/// messages 条数上限（方案第 5.1 节上下文预算）。
pub const PLAYGROUND_MAX_MESSAGES: usize = 50;
/// 序列化请求体上限（方案第 5.1 节上下文预算）。
pub const PLAYGROUND_MAX_BODY_BYTES: usize = 512 * 1024;
/// `max_tokens` 上限（下发前 clamp）。
pub const PLAYGROUND_MAX_TOKENS_CAP: u32 = 8192;
/// 单条消息/系统提示词长度上限（字符，防止把预算绕到单条超长消息上）。
const PLAYGROUND_MAX_MESSAGE_CHARS: usize = 32_000;
/// `request_id` 长度上限（前端生成 UUID，宽松校验）。
const PLAYGROUND_MAX_REQUEST_ID_LEN: usize = 64;
/// 连接超时：只约束建连阶段，无总超时（流式响应总时长不可预期）。
const PLAYGROUND_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 两次数据之间的空闲上限：超过按网络错误终态。
const PLAYGROUND_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// 身份复查周期：即使没有新数据也定期核对会话身份。
const PLAYGROUND_IDENTITY_TICK: Duration = Duration::from_secs(2);

/// 流式 HTTP 客户端：独立于 `api.rs` 的共享客户端（那个带 20s 总超时且整包读取，
/// 不适用于 SSE 长流）。代理策略与其保持一致（reqwest 默认：系统代理与环境
/// 变量）。测试构建下不保留空闲连接（理由同 `api.rs` 的 `SHARED_HTTP_CLIENT`）。
static PLAYGROUND_HTTP_CLIENT: once_cell::sync::Lazy<reqwest::Client> =
    once_cell::sync::Lazy::new(|| {
        let builder = reqwest::Client::builder()
            .connect_timeout(PLAYGROUND_CONNECT_TIMEOUT)
            .user_agent(USER_AGENT);
        #[cfg(test)]
        let builder = builder.pool_max_idle_per_host(0);
        builder
            .build()
            .expect("failed to build we2ai playground reqwest client")
    });

// ---------------------------------------------------------------------------
// 请求 / 视图结构
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaygroundMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaygroundChatRequest {
    pub request_id: String,
    pub key_id: i64,
    pub model: String,
    pub messages: Vec<PlaygroundMessage>,
    pub system: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlaygroundUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlaygroundChatOutcome {
    /// 网关给出的结束原因（如 `stop`）；流未显式给出时为 `None`（前端按正常结束
    /// 展示）。
    pub finish_reason: Option<String>,
    /// 网关流式 usage（可能缺失——契约见方案第 2 节【待联调】）；缺失时前端
    /// 不显示 token 数，不视为错误。
    pub usage: Option<PlaygroundUsage>,
}

/// `we2ai_playground_models` 的返回：只列明确 `kind == "text"` 的模型（工坊口径
/// 比模型广场更严：缺 kind 不放行，见方案第 5.1 节）。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlaygroundModelsView {
    pub models: Vec<PlaygroundModelView>,
    /// Key 级准入被拒的原因（B1 `blocked_reason`）；`None` 表示可调用。
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlaygroundModelView {
    pub id: String,
    pub provider: Option<String>,
}

/// `we2ai_playground_choose_key` 的返回。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlaygroundChooseKeyView {
    pub studio_selected_key_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PlaygroundChunkPayload {
    request_id: String,
    delta: String,
}

// ---------------------------------------------------------------------------
// 错误 code（稳定契约，前端按 code 映射本地化文案；未知上游 body 不透传）
// ---------------------------------------------------------------------------

/// Key 不在当前账号的可见列表里（缓存未热也走这里：前端应先 `we2ai_list_keys`）。
const CODE_KEY_NOT_FOUND: &str = "PLAYGROUND_KEY_NOT_FOUND";
/// Key 在列表里但不是 `active`（额度耗尽等）。
const CODE_KEY_INACTIVE: &str = "PLAYGROUND_KEY_INACTIVE";
/// 网关 401/403：Key 已失效或被删除。
const CODE_KEY_INVALID: &str = "PLAYGROUND_KEY_INVALID";
const CODE_QUOTA_EXHAUSTED: &str = "PLAYGROUND_QUOTA_EXHAUSTED";
const CODE_RATE_LIMITED: &str = "PLAYGROUND_RATE_LIMITED";
/// 登出/切换会话后任务身份失效，流被中断。
const CODE_IDENTITY_CHANGED: &str = "PLAYGROUND_IDENTITY_CHANGED";
const CODE_CANCELLED: &str = "PLAYGROUND_CANCELLED";
const CODE_NETWORK: &str = "PLAYGROUND_NETWORK";
const CODE_PAYLOAD_TOO_LARGE: &str = "PLAYGROUND_PAYLOAD_TOO_LARGE";
const CODE_REQUEST_ID_IN_USE: &str = "PLAYGROUND_REQUEST_ID_IN_USE";
/// 并发上限（同时只允许一个活跃流）。
const CODE_BUSY: &str = "PLAYGROUND_BUSY";
const CODE_UPSTREAM_ERROR: &str = "PLAYGROUND_UPSTREAM_ERROR";
const CODE_BAD_REQUEST: &str = "PLAYGROUND_BAD_REQUEST";

fn playground_error(code: &str, message: impl Into<String>) -> We2aiApiError {
    We2aiApiError {
        code: code.to_string(),
        message: message.into(),
    }
}

/// 上游 HTTP 状态 → 稳定错误 code（方案第 5.3 节错误契约）。
fn upstream_status_code(status: u16) -> &'static str {
    match status {
        401 | 403 => CODE_KEY_INVALID,
        402 => CODE_QUOTA_EXHAUSTED,
        429 => CODE_RATE_LIMITED,
        _ => CODE_UPSTREAM_ERROR,
    }
}

// ---------------------------------------------------------------------------
// 流任务注册表（身份取消链路，方案第 5.3 节）
// ---------------------------------------------------------------------------

struct ActiveTask {
    identity: SessionIdentity,
    cancel: Arc<Notify>,
    token: u64,
}

/// 工坊在途流任务表：`request_id → 任务`。并发上限 1。
/// 不派生 Debug（虽不含明文，但保持与 keys 缓存一致的谨慎习惯）。
#[derive(Default)]
pub struct PlaygroundState {
    tasks: Mutex<HashMap<String, ActiveTask>>,
    /// 任务代次发号器：单调递增，永不复用。
    next_token: std::sync::atomic::AtomicU64,
}

/// 注册结果：成功时返回该任务的取消信号与代次（任务内 `select!` 监听前者，
/// guard 用后者做条件删除）。
enum RegisterOutcome {
    Registered { cancel: Arc<Notify>, token: u64 },
    RequestIdInUse,
    Busy,
}

impl PlaygroundState {
    fn register(&self, request_id: &str, identity: SessionIdentity) -> RegisterOutcome {
        let mut tasks = self.tasks.lock().unwrap();
        if tasks.contains_key(request_id) {
            return RegisterOutcome::RequestIdInUse;
        }
        if !tasks.is_empty() {
            return RegisterOutcome::Busy;
        }
        let cancel = Arc::new(Notify::new());
        let token = self
            .next_token
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        tasks.insert(
            request_id.to_string(),
            ActiveTask {
                identity,
                cancel: cancel.clone(),
                token,
            },
        );
        RegisterOutcome::Registered { cancel, token }
    }

    /// 终态移除：只删代次匹配的那一条（P2：防止旧任务误删新任务）。
    fn remove(&self, request_id: &str, token: u64) {
        let mut tasks = self.tasks.lock().unwrap();
        if tasks.get(request_id).is_some_and(|task| task.token == token) {
            tasks.remove(request_id);
        }
    }

    /// 取消指定请求：存在且属当前会话才触发（方案第 5.1 节「校验任务归属」）。
    /// `notify_one` 会保留许可，任务即使尚未走到 `select!` 分支也能立即感知。
    /// **不**移除任务项——留给任务自身的 guard 在真正终态时移除，避免取消
    /// 与终止之间的窗口里同 ID 被重新注册（P2 ABA 竞态）。
    fn cancel(&self, request_id: &str, identity: Option<SessionIdentity>) -> bool {
        let tasks = self.tasks.lock().unwrap();
        match tasks.get(request_id) {
            Some(task) if Some(task.identity) == identity => {
                task.cancel.notify_one();
                true
            }
            _ => false,
        }
    }

    /// 登出/会话终止路径调用：取消全部在途流（`we2ai_logout` 挂接）。同样
    /// **不** drain 任务项，交给各任务自己的 guard 按代次移除——drain 后任务
    /// 尚未退出时同 ID 可被重新注册，旧 guard 会误删新任务（P2 ABA 竞态）。
    pub fn cancel_all(&self) {
        let tasks = self.tasks.lock().unwrap();
        for task in tasks.values() {
            task.cancel.notify_one();
        }
    }

    #[cfg(test)]
    fn is_idle(&self) -> bool {
        self.tasks.lock().unwrap().is_empty()
    }
}

/// 终态后从任务表移除句柄的 RAII 守卫：命令的所有返回路径都会经过它，
/// 保证「终态 → 移除」不会因为提前 `return` 而遗漏（方案第 5.1 节生命周期契约）。
struct TaskGuard<'a> {
    state: &'a PlaygroundState,
    request_id: String,
    token: u64,
}

impl Drop for TaskGuard<'_> {
    fn drop(&mut self) {
        self.state.remove(&self.request_id, self.token);
    }
}

// ---------------------------------------------------------------------------
// SSE 解析（纯函数，测试覆盖：正常流 / [DONE] / 半包 / 错误 JSON）
// ---------------------------------------------------------------------------

/// 单条 `data:` 行解析结果。
enum SseItem {
    Delta(String),
    Finish(String),
    Usage(PlaygroundUsage),
    Done,
    /// 上游在流里回报的错误对象（P2）：`{"error": {...}}`。识别它，别当成
    /// 「无关行」忽略——否则出错后紧跟连接关闭会被误判为成功轮次。
    UpstreamError,
    /// 合法 JSON 但不含关注字段（如角色行、空 delta）——静默忽略。
    Ignored,
}

/// SSE 解析失败（超限）——作为稳定错误上抛，不走「忽略」路径。
const CODE_STREAM_OVERFLOW: &str = "PLAYGROUND_STREAM_OVERFLOW";
/// 单行最大字节数（含 `data:` 前缀）；超限视为异常流。
const PLAYGROUND_MAX_SSE_LINE_BYTES: usize = 256 * 1024;
/// 未出现换行时的缓冲上限；超限视为异常流（防止网关持续发无换行字节撑爆内存）。
const PLAYGROUND_MAX_SSE_BUFFER_BYTES: usize = 512 * 1024;

/// 增量 SSE 解析器：字节缓冲 + 按 `\n` 切行。多字节 UTF-8 字符被拆在两个
/// chunk 时，只有凑齐完整行（出现 `\n`）才会解码，天然规避截断问题。
#[derive(Default)]
struct SseParser {
    buffer: Vec<u8>,
}

impl SseParser {
    /// 喂入一段原始字节，返回已凑齐的完整行里解析出的条目。缓冲/单行超限返回
    /// 稳定错误（P2：防止网关持续发不含换行的字节撑爆内存）。
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<SseItem>, We2aiApiError> {
        self.buffer.extend_from_slice(bytes);
        let mut items = Vec::new();
        loop {
            let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') else {
                if self.buffer.len() > PLAYGROUND_MAX_SSE_BUFFER_BYTES {
                    return Err(playground_error(
                        CODE_STREAM_OVERFLOW,
                        "响应异常，已停止生成",
                    ));
                }
                break;
            };
            if pos > PLAYGROUND_MAX_SSE_LINE_BYTES {
                return Err(playground_error(
                    CODE_STREAM_OVERFLOW,
                    "响应异常，已停止生成",
                ));
            }
            let line: Vec<u8> = self.buffer.drain(..=pos).collect();
            // 去掉行尾 `\n` 与可选的 `\r`（CRLF）。
            let mut line = &line[..line.len() - 1];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            items.extend(Self::parse_line(line));
        }
        Ok(items)
    }

    /// 解析一行：只认 `data:` 前缀；`event:`/`id:`/注释（`:` 开头）/空行忽略。
    /// 返回 0..=3 条：delta、finish_reason、usage 可以在同一行并存（OpenAI
    /// 兼容流式惯例是最后一个 chunk 同时带 finish_reason 与 usage）。
    fn parse_line(line: &[u8]) -> Vec<SseItem> {
        if !line.starts_with(b"data:") {
            return Vec::new();
        }
        let payload = &line[b"data:".len()..];
        // 冒号后至多一个空格（SSE 规范）。
        let payload = if payload.first() == Some(&b' ') {
            &payload[1..]
        } else {
            payload
        };
        if payload == b"[DONE]" {
            return vec![SseItem::Done];
        }
        let text = match std::str::from_utf8(payload) {
            Ok(text) => text,
            Err(_) => {
                log::warn!("playground: SSE 行不是有效 UTF-8，忽略");
                return Vec::new();
            }
        };
        let value: serde_json::Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(_) => {
                // 兼容 keep-alive 注释性 data 行（某些网关发空 data）。
                if text.trim().is_empty() {
                    return Vec::new();
                }
                log::warn!("playground: SSE data 不是合法 JSON，忽略");
                return Vec::new();
            }
        };
        let mut items = Vec::new();
        // 上游错误对象优先：出现即整条判为错误（P2）。
        if value.get("error").is_some() {
            return vec![SseItem::UpstreamError];
        }
        if let Some(delta) = value
            .pointer("/choices/0/delta/content")
            .and_then(|v| v.as_str())
        {
            if !delta.is_empty() {
                items.push(SseItem::Delta(delta.to_string()));
            }
        }
        if let Some(finish) = value
            .pointer("/choices/0/finish_reason")
            .and_then(|v| v.as_str())
        {
            if !finish.is_empty() {
                items.push(SseItem::Finish(finish.to_string()));
            }
        }
        if let Some(usage) = value.get("usage").filter(|u| u.is_object()) {
            if let Some(parsed) = parse_usage(usage) {
                items.push(SseItem::Usage(parsed));
            }
        }
        if items.is_empty() {
            items.push(SseItem::Ignored);
        }
        items
    }
}

fn parse_usage(value: &serde_json::Value) -> Option<PlaygroundUsage> {
    let prompt = value.get("prompt_tokens")?.as_u64()?;
    let completion = value.get("completion_tokens")?.as_u64()?;
    let total = value.get("total_tokens")?.as_u64()?;
    Some(PlaygroundUsage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: total,
    })
}

// ---------------------------------------------------------------------------
// 请求体构造与校验（纯函数）
// ---------------------------------------------------------------------------

/// 校验请求并把可选参数归一（`max_tokens` clamp 到上限；非有限浮点拒绝）。
/// `None` = 不下发该字段（与 Web Playground 参数语义对齐）。
fn build_chat_body(request: &PlaygroundChatRequest) -> Result<serde_json::Value, We2aiApiError> {
    if request.request_id.trim().is_empty()
        || request.request_id.len() > PLAYGROUND_MAX_REQUEST_ID_LEN
    {
        return Err(playground_error(CODE_BAD_REQUEST, "请求标识无效"));
    }
    if request.model.trim().is_empty() {
        return Err(playground_error(CODE_BAD_REQUEST, "未选择模型"));
    }
    // 消息条数上限按「实际下发给网关的条数」计（P2）：system 会被追加成一条
    // 独立消息，所以这里留出它的位置，避免校验 50 条却实际发 51 条。
    let system_count = request
        .system
        .as_deref()
        .map(|s| usize::from(!s.is_empty()))
        .unwrap_or(0);
    if request.messages.is_empty()
        || request.messages.len() + system_count > PLAYGROUND_MAX_MESSAGES
    {
        return Err(playground_error(
            CODE_PAYLOAD_TOO_LARGE,
            format!("消息条数超出上限（{PLAYGROUND_MAX_MESSAGES} 条），请开启新会话"),
        ));
    }
    for message in &request.messages {
        if message.role != "user" && message.role != "assistant" {
            return Err(playground_error(
                CODE_BAD_REQUEST,
                format!("不支持的消息角色: {}", message.role),
            ));
        }
        if message.content.chars().count() > PLAYGROUND_MAX_MESSAGE_CHARS {
            return Err(playground_error(
                CODE_PAYLOAD_TOO_LARGE,
                "单条消息过长，请缩短后重试",
            ));
        }
    }
    if let Some(system) = request.system.as_deref() {
        if system.chars().count() > PLAYGROUND_MAX_MESSAGE_CHARS {
            return Err(playground_error(
                CODE_PAYLOAD_TOO_LARGE,
                "系统提示词过长，请缩短后重试",
            ));
        }
    }
    // temperature 与 top_p 的取值范围不同（P2）：temperature 是 0..=2，
    // top_p 是 0..=1。共用一套范围会让 `top_p: 1.5` 通过校验后下发。
    if let Some(value) = request.temperature {
        if !value.is_finite() || !(0.0..=2.0).contains(&value) {
            return Err(playground_error(
                CODE_BAD_REQUEST,
                "temperature 取值无效（应为 0 到 2 之间的数值）",
            ));
        }
    }
    if let Some(value) = request.top_p {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(playground_error(
                CODE_BAD_REQUEST,
                "top_p 取值无效（应为 0 到 1 之间的数值）",
            ));
        }
    }
    if let Some(max_tokens) = request.max_tokens {
        if max_tokens == 0 {
            return Err(playground_error(
                CODE_BAD_REQUEST,
                "max_tokens 必须大于 0",
            ));
        }
    }

    let mut messages: Vec<serde_json::Value> = Vec::with_capacity(request.messages.len() + 1);
    if let Some(system) = request.system.as_deref() {
        if !system.is_empty() {
            messages.push(serde_json::json!({
                "role": "system",
                "content": system,
            }));
        }
    }
    for message in &request.messages {
        messages.push(serde_json::json!({
            "role": message.role,
            "content": message.content,
        }));
    }
    let mut body = serde_json::json!({
        "model": request.model,
        "messages": messages,
        "stream": true,
    });
    if let Some(temperature) = request.temperature {
        body["temperature"] = serde_json::json!(temperature);
    }
    if let Some(top_p) = request.top_p {
        body["top_p"] = serde_json::json!(top_p);
    }
    // max_tokens：未指定时也下发硬上限（P2）。此前 `None` 就完全不下发，输出
    // 上限交给上游默认值，可能超过我们承诺的预算；这里始终下发，`None` 用上限值。
    body["max_tokens"] =
        serde_json::json!(request.max_tokens.unwrap_or(PLAYGROUND_MAX_TOKENS_CAP).min(PLAYGROUND_MAX_TOKENS_CAP));
    let serialized = serde_json::to_string(&body)
        .map_err(|e| playground_error(CODE_BAD_REQUEST, format!("请求体序列化失败: {e}")))?;
    if serialized.len() > PLAYGROUND_MAX_BODY_BYTES {
        return Err(playground_error(
            CODE_PAYLOAD_TOO_LARGE,
            format!(
                "请求体超出上限（约 {} KB），请开启新会话",
                PLAYGROUND_MAX_BODY_BYTES / 1024
            ),
        ));
    }
    Ok(body)
}

// ---------------------------------------------------------------------------
// HTTP 流式执行（不依赖 Tauri / SessionManager，wiremock 端到端测试）
// ---------------------------------------------------------------------------

/// 把一个 future 置于取消/身份监督之下执行：取消信号触发即返回 `CANCELLED`，
/// 周期 tick 复查身份（返回 [`CODE_IDENTITY_CHANGED`]）。连接阶段（`send()`，
/// 可能因网络慢而长时间无响应）与流式读取阶段共用这一套监督。
async fn supervised<F>(
    cancel: &Notify,
    identity_ok: &(dyn Fn() -> bool + Send + Sync),
    fut: F,
) -> Result<F::Output, We2aiApiError>
where
    F: std::future::Future,
{
    tokio::pin!(fut);
    let timer = tokio::time::sleep(PLAYGROUND_IDENTITY_TICK);
    tokio::pin!(timer);
    loop {
        tokio::select! {
            biased;
            _ = cancel.notified() => {
                return Err(playground_error(CODE_CANCELLED, "已停止生成"));
            }
            _ = &mut timer => {
                if !identity_ok() {
                    return Err(playground_error(
                        CODE_IDENTITY_CHANGED,
                        "会话已切换，本次生成已停止",
                    ));
                }
                timer
                    .as_mut()
                    .reset(tokio::time::Instant::now() + PLAYGROUND_IDENTITY_TICK);
            }
            out = &mut fut => return Ok(out),
        }
    }
}

/// 执行一次流式 chat：增量经 `on_delta` 回调，终态返回 [`PlaygroundChatOutcome`]。
///
/// `identity_ok` 在每次 delta 回调前与周期 tick 调用；返回 `false` 即以
/// `PLAYGROUND_IDENTITY_CHANGED` 终止（登出/切会话防线，方案第 5.3 节）。
/// `cancel` 是任务取消信号（`we2ai_playground_cancel` / 登出 `cancel_all`）。
async fn run_chat_stream(
    http: reqwest::Client,
    url: &str,
    api_key: &str,
    body: serde_json::Value,
    cancel: Arc<Notify>,
    identity_ok: &(dyn Fn() -> bool + Send + Sync),
    on_delta: &mut (dyn FnMut(&str) + Send),
) -> Result<PlaygroundChatOutcome, We2aiApiError> {
    let response = match supervised(
        &cancel,
        identity_ok,
        http.post(url).bearer_auth(api_key).json(&body).send(),
    )
    .await
    {
        // 监督错误（取消/身份失效）直接作为终态返回，不吞成网络错误。
        Err(err) => return Err(err),
        Ok(Ok(response)) => response,
        Ok(Err(e)) => {
            log::warn!("playground: 请求发送失败: {e}");
            return Err(playground_error(CODE_NETWORK, "网络连接失败，请稍后重试"));
        }
    };
    let status = response.status();
    if !status.is_success() {
        // 不读错误 body（P1）：body 可能回显 Key 明文，读它会把它写进日志；且
        // `text().await` 不受取消/身份/读取超时监督，网关挂住响应体时任务无法
        // 终止（旧任务占着注册表，新请求一直 BUSY）。只记录状态码——状态码本身
        // 不含秘密，足以定位问题。
        log::warn!("playground: 上游返回 {}", status.as_u16());
        let code = upstream_status_code(status.as_u16());
        return Err(playground_error(code, upstream_message(code)));
    }

    let mut stream = response.bytes_stream();
    let mut parser = SseParser::default();
    let mut finish_reason: Option<String> = None;
    let mut usage: Option<PlaygroundUsage> = None;
    let mut last_data = tokio::time::Instant::now();
    // 跨循环复用的 interval（P1）：此前每轮 select 新建一次 sleep，网关只要在
    // 2 秒内持续发心跳/chunk，身份 tick 就永远不到期——身份失效后旧任务仍会
    // 继续请求计费。用带 deadline 的 interval 保证到期必然触发一次复查。
    let mut identity_tick = tokio::time::interval(PLAYGROUND_IDENTITY_TICK);
    // 第一次 tick 会立即返回，跳过（否则刚进循环就白查一次）。
    identity_tick.tick().await;

    loop {
        let waited = tokio::select! {
            biased;
            _ = cancel.notified() => {
                return Err(playground_error(CODE_CANCELLED, "已停止生成"));
            }
            _ = identity_tick.tick() => {
                if !identity_ok() {
                    return Err(playground_error(
                        CODE_IDENTITY_CHANGED,
                        "会话已切换，本次生成已停止",
                    ));
                }
                if last_data.elapsed() > PLAYGROUND_IDLE_TIMEOUT {
                    return Err(playground_error(
                        CODE_NETWORK,
                        "响应超时，请稍后重试",
                    ));
                }
                continue;
            }
            chunk = stream.next() => chunk,
        };
        match waited {
            Some(Ok(bytes)) => {
                last_data = tokio::time::Instant::now();
                // 处理任何数据前先复查身份（P1）：不只看 delta——心跳、finish、
                // usage 行同样可能来自已失效的会话。
                if !identity_ok() {
                    return Err(playground_error(
                        CODE_IDENTITY_CHANGED,
                        "会话已切换，本次生成已停止",
                    ));
                }
                for item in parser.feed(&bytes)? {
                    match item {
                        SseItem::Delta(delta) => on_delta(&delta),
                        SseItem::Finish(reason) => finish_reason = Some(reason),
                        SseItem::Usage(parsed) => usage = Some(parsed),
                        SseItem::Done => {
                            // 立即返回终态（P2）：`[DONE]` 之后同一 chunk 里若还有
                            // 数据（delta/error），那是终止后的噪声，不能继续投递，
                            // 也不能把已完成的请求翻成失败。是否同一 chunk 不应改变
                            // 结果——所以这里直接返回，而不是置旗标等到批次末尾。
                            return Ok(PlaygroundChatOutcome {
                                finish_reason,
                                usage,
                            });
                        }
                        SseItem::UpstreamError => {
                            // 流内错误对象：映射为稳定错误，不透传 body。
                            log::warn!("playground: 上游在流内返回错误对象");
                            return Err(playground_error(
                                CODE_UPSTREAM_ERROR,
                                upstream_message(CODE_UPSTREAM_ERROR),
                            ));
                        }
                        SseItem::Ignored => {}
                    }
                }
            }
            Some(Err(e)) => {
                log::warn!("playground: 流读取失败: {e}");
                return Err(playground_error(CODE_NETWORK, "网络连接中断，请稍后重试"));
            }
            // 流关闭（P2）：`[DONE]` 已在上面直接返回，走到这里说明没收到它；
            // 只有拿到非空 finish_reason 才算完整结束。连接被截断时不算成功——
            // 否则残缺文本会被当成完整轮次进入下一轮历史。
            None => {
                if finish_reason.is_some() {
                    return Ok(PlaygroundChatOutcome {
                        finish_reason,
                        usage,
                    });
                }
                return Err(playground_error(
                    CODE_UPSTREAM_ERROR,
                    "生成被中断，内容不完整，请重试",
                ));
            }
        }
    }
}

fn upstream_message(code: &str) -> &'static str {
    match code {
        CODE_KEY_INVALID => "所选 Key 已失效或已被删除，请重新选择",
        CODE_QUOTA_EXHAUSTED => "Key 额度已用完，请充值或更换 Key",
        CODE_RATE_LIMITED => "请求过于频繁，请稍后重试",
        _ => "网关服务异常，请稍后重试",
    }
}

// ---------------------------------------------------------------------------
// Tauri 命令
// ---------------------------------------------------------------------------

/// 校验 Key：在当前身份的可见缓存里且 `active`，返回 (状态, 明文)。
fn resolve_active_key(
    manager: &SessionManager,
    keys: &We2aiKeyState,
    key_id: i64,
) -> Result<(SessionIdentity, String), We2aiApiError> {
    let identity = manager
        .current_identity()
        .ok_or_else(|| playground_error("NO_ACTIVE_SESSION", "尚未登录"))?;
    let status = keys
        .visible_key_status(Some(identity), key_id)
        .ok_or_else(|| {
            playground_error(
                CODE_KEY_NOT_FOUND,
                "该 Key 不在当前账号的 Key 列表中，请刷新列表",
            )
        })?;
    if status != "active" {
        return Err(playground_error(
            CODE_KEY_INACTIVE,
            "该 Key 当前不可用（额度耗尽或已停用），请选择其他 Key",
        ));
    }
    let secret = keys
        .active_secret_for(Some(identity), key_id)
        .ok_or_else(|| {
            playground_error(
                CODE_KEY_NOT_FOUND,
                "该 Key 不在当前账号的 Key 列表中，请刷新列表",
            )
        })?;
    Ok((identity, secret))
}

#[tauri::command]
pub async fn we2ai_playground_chat(
    app: tauri::AppHandle,
    session: State<'_, We2aiSessionState>,
    keys: State<'_, We2aiKeyState>,
    playground: State<'_, PlaygroundState>,
    request: PlaygroundChatRequest,
) -> Result<PlaygroundChatOutcome, We2aiApiError> {
    use tauri::Emitter;

    let manager = session.0.clone();
    // 校验与构建请求体失败时直接返回：不登记任务、不触碰 Key 缓存。
    let body = build_chat_body(&request)?;
    let (identity, secret) = resolve_active_key(&manager, keys.inner(), request.key_id)?;
    let (cancel, task_token) = match playground.register(&request.request_id, identity) {
        RegisterOutcome::Registered { cancel, token } => (cancel, token),
        RegisterOutcome::RequestIdInUse => {
            return Err(playground_error(
                CODE_REQUEST_ID_IN_USE,
                "请求标识重复，请重新发送",
            ))
        }
        RegisterOutcome::Busy => {
            return Err(playground_error(
                CODE_BUSY,
                "已有生成任务进行中，请等待完成或先停止",
            ))
        }
    };
    let _guard = TaskGuard {
        state: playground.inner(),
        request_id: request.request_id.clone(),
        token: task_token,
    };

    let url = format!("{}/v1/chat/completions", identity.region.base_url());
    let identity_manager = manager.clone();
    let registered_identity = identity;
    let identity_ok = move || identity_manager.current_identity() == Some(registered_identity);
    let emit_app = app.clone();
    let emit_request_id = request.request_id.clone();
    let mut on_delta = move |delta: &str| {
        let _ = emit_app.emit(
            PLAYGROUND_CHUNK_EVENT,
            PlaygroundChunkPayload {
                request_id: emit_request_id.clone(),
                delta: delta.to_string(),
            },
        );
    };

    run_chat_stream(
        PLAYGROUND_HTTP_CLIENT.clone(),
        &url,
        &secret,
        body,
        cancel,
        &identity_ok,
        &mut on_delta,
    )
    .await
}

#[tauri::command]
pub async fn we2ai_playground_cancel(
    session: State<'_, We2aiSessionState>,
    playground: State<'_, PlaygroundState>,
    request_id: String,
) -> Result<(), We2aiApiError> {
    let manager = session.0.clone();
    playground.cancel(&request_id, manager.current_identity());
    Ok(())
}

#[tauri::command]
pub async fn we2ai_playground_models(
    session: State<'_, We2aiSessionState>,
    keys: State<'_, We2aiKeyState>,
    key_id: i64,
) -> Result<PlaygroundModelsView, We2aiApiError> {
    let manager = session.0.clone();
    let remote = super::keys::key_models(&manager, keys.inner(), key_id).await?;
    // 工坊口径：只列明确 `kind == "text"` 的模型；缺 kind 不放行（比模型广场
    // 的 `modelKind.ts` 缺省放行更严，方案第 5.1 节）。
    let models = remote
        .models
        .into_iter()
        .filter(|m| m.kind.as_deref() == Some("text"))
        .map(|m| PlaygroundModelView {
            id: m.id,
            provider: m.provider,
        })
        .collect();
    Ok(PlaygroundModelsView {
        models,
        // `key_models` 已保证 callable 时为 None。
        blocked_reason: remote.blocked_reason,
    })
}

#[tauri::command]
pub async fn we2ai_playground_choose_key(
    session: State<'_, We2aiSessionState>,
    keys: State<'_, We2aiKeyState>,
    key_id: i64,
) -> Result<PlaygroundChooseKeyView, We2aiApiError> {
    let manager = session.0.clone();
    let (identity, _) = resolve_active_key(&manager, keys.inner(), key_id)?;
    super::keys::save_studio_selection(manager.data_root(), &identity, key_id)
        .map_err(|e| playground_error("KEY_SELECTION_SAVE_FAILED", format!("保存选择失败: {e}")))?;
    Ok(PlaygroundChooseKeyView {
        studio_selected_key_id: Some(key_id),
    })
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: &str, content: &str) -> PlaygroundMessage {
        PlaygroundMessage {
            role: role.to_string(),
            content: content.to_string(),
        }
    }

    fn request() -> PlaygroundChatRequest {
        PlaygroundChatRequest {
            request_id: "req-1".to_string(),
            key_id: 7,
            model: "gpt-test".to_string(),
            messages: vec![message("user", "写一句文案")],
            system: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
        }
    }

    fn identity() -> SessionIdentity {
        SessionIdentity {
            region: super::super::region::Region::International,
            user_id: 42,
            generation: 1,
        }
    }

    // ---- SSE 解析 ----

    fn feed_all(parser: &mut SseParser, chunks: &[&[u8]]) -> Vec<SseItem> {
        let mut items = Vec::new();
        for chunk in chunks {
            items.extend(parser.feed(chunk).expect("测试数据不应触发流超限"));
        }
        items
    }

    #[test]
    fn sse_parses_deltas_finish_usage_and_done() {
        let mut parser = SseParser::default();
        let items = feed_all(
            &mut parser,
            &[
                "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n".as_bytes(),
                "data: {\"choices\":[{\"delta\":{\"content\":\"你好\"}}]}\n".as_bytes(),
                "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}\n".as_bytes(),
                "data: [DONE]\n".as_bytes(),
            ],
        );
        let deltas: Vec<&str> = items
            .iter()
            .filter_map(|i| match i {
                SseItem::Delta(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, vec!["你好"]);
        assert!(items
            .iter()
            .any(|i| matches!(i, SseItem::Finish(r) if r == "stop")));
        assert!(items.iter().any(|i| matches!(
            i,
            SseItem::Usage(u) if u.total_tokens == 15 && u.prompt_tokens == 10
        )));
        assert!(matches!(items.last(), Some(SseItem::Done)));
    }

    #[test]
    fn sse_handles_partial_lines_across_chunks_and_crlf() {
        let mut parser = SseParser::default();
        // 「文案」的 UTF-8 字节被拆到两个 chunk，且行尾是 CRLF。
        let line = "data: {\"choices\":[{\"delta\":{\"content\":\"文案\"}}]}\r\n".as_bytes();
        let split = line.len() / 2;
        let items = feed_all(&mut parser, &[&line[..split], &line[split..]]);
        let deltas: Vec<&str> = items
            .iter()
            .filter_map(|i| match i {
                SseItem::Delta(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, vec!["文案"]);
        // 缓冲里不应残留未凑齐的行。
        assert!(parser.buffer.is_empty());
    }

    #[test]
    fn sse_ignores_event_fields_comments_and_invalid_json() {
        let mut parser = SseParser::default();
        let items = feed_all(
            &mut parser,
            &[
                ": keep-alive\n".as_bytes(),
                "event: message\n".as_bytes(),
                "id: 42\n".as_bytes(),
                "\n".as_bytes(),
                "data: not-json\n".as_bytes(),
                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n".as_bytes(),
            ],
        );
        let deltas: Vec<&str> = items
            .iter()
            .filter_map(|i| match i {
                SseItem::Delta(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, vec!["ok"]);
    }

    // ---- 请求体构造 ----

    #[test]
    fn build_body_includes_system_params_and_clamps_max_tokens() {
        let mut req = request();
        req.system = Some("你是营销专家".to_string());
        req.temperature = Some(0.7);
        req.top_p = Some(0.9);
        req.max_tokens = Some(99_999);
        let body = build_chat_body(&req).unwrap();
        assert_eq!(body["stream"], serde_json::json!(true));
        assert_eq!(body["model"], serde_json::json!("gpt-test"));
        assert_eq!(
            body["max_tokens"],
            serde_json::json!(PLAYGROUND_MAX_TOKENS_CAP)
        );
        assert_eq!(body["temperature"], serde_json::json!(0.7));
        assert_eq!(
            body["messages"][0],
            serde_json::json!({"role": "system", "content": "你是营销专家"})
        );
        assert_eq!(
            body["messages"][1],
            serde_json::json!({"role": "user", "content": "写一句文案"})
        );
    }

    #[test]
    fn build_body_omits_unset_optional_params_but_always_caps_max_tokens() {
        let body = build_chat_body(&request()).unwrap();
        assert!(body.get("temperature").is_none());
        assert!(body.get("top_p").is_none());
        assert!(body.get("system").is_none());
        // P2：max_tokens 未指定时也下发硬上限，不把输出预算交给上游默认值。
        assert_eq!(
            body["max_tokens"],
            serde_json::json!(PLAYGROUND_MAX_TOKENS_CAP)
        );
    }

    #[test]
    fn build_body_rejects_zero_and_out_of_range_top_p() {
        let mut req = request();
        req.max_tokens = Some(0);
        assert_eq!(build_chat_body(&req).unwrap_err().code, CODE_BAD_REQUEST);

        let mut req = request();
        req.top_p = Some(1.5);
        assert_eq!(build_chat_body(&req).unwrap_err().code, CODE_BAD_REQUEST);

        let mut req = request();
        req.top_p = Some(1.0);
        assert!(build_chat_body(&req).is_ok());
    }

    #[test]
    fn build_body_rejects_bad_requests() {
        let mut req = request();
        req.messages.clear();
        assert_eq!(
            build_chat_body(&req).unwrap_err().code,
            CODE_PAYLOAD_TOO_LARGE
        );

        let mut req = request();
        req.messages = vec![message("system", "越权角色")];
        assert_eq!(build_chat_body(&req).unwrap_err().code, CODE_BAD_REQUEST);

        let mut req = request();
        req.temperature = Some(f64::NAN);
        assert_eq!(build_chat_body(&req).unwrap_err().code, CODE_BAD_REQUEST);

        let mut req = request();
        req.request_id = " ".to_string();
        assert_eq!(build_chat_body(&req).unwrap_err().code, CODE_BAD_REQUEST);

        let mut req = request();
        req.messages = vec![message(
            "user",
            &"长".repeat(PLAYGROUND_MAX_MESSAGE_CHARS + 1),
        )];
        assert_eq!(
            build_chat_body(&req).unwrap_err().code,
            CODE_PAYLOAD_TOO_LARGE
        );
    }

    // ---- 任务注册表 ----

    /// `notify_one` 的许可语义：取消后 `notified()` 立即完成（不需要等到
    /// select! 恰好正在等待）。
    async fn assert_notified(cancel: &Notify) {
        tokio::time::timeout(Duration::from_secs(1), cancel.notified())
            .await
            .expect("取消信号应立即可感知");
    }

    #[tokio::test]
    async fn registry_register_busy_duplicate_cancel_and_remove() {
        let state = PlaygroundState::default();
        let ident = identity();
        let cancel = match state.register("req-1", ident) {
            RegisterOutcome::Registered { cancel, .. } => cancel,
            _ => panic!("应注册成功"),
        };
        assert!(matches!(
            state.register("req-1", ident),
            RegisterOutcome::RequestIdInUse
        ));
        assert!(matches!(
            state.register("req-2", ident),
            RegisterOutcome::Busy
        ));
        // 非归属身份不能取消。
        let other = SessionIdentity {
            generation: 99,
            ..ident
        };
        assert!(!state.cancel("req-1", Some(other)));
        // 归属身份取消成功，任务侧能立即感知（notify_one 保留许可）。
        assert!(state.cancel("req-1", Some(ident)));
        assert_notified(&cancel).await;
        // 取消不删任务项（P2 ABA 防护）：仍占着，同 ID 不能重复注册。
        assert!(matches!(
            state.register("req-1", ident),
            RegisterOutcome::RequestIdInUse
        ));
        // 终态移除（代次匹配）后可再次注册同一 id。
        state.remove("req-1", 1);
        assert!(state.is_idle());
        assert!(matches!(
            state.register("req-1", ident),
            RegisterOutcome::Registered { .. }
        ));
    }

    #[tokio::test]
    async fn registry_remove_is_scoped_to_its_own_token() {
        // P2 ABA：旧任务的 token 不能删掉复用同 ID 的新任务。
        let state = PlaygroundState::default();
        let ident = identity();
        let (_, old_token) = match state.register("req-1", ident) {
            RegisterOutcome::Registered { cancel, token } => (cancel, token),
            _ => panic!("应注册成功"),
        };
        state.remove("req-1", old_token);
        let (_, new_token) = match state.register("req-1", ident) {
            RegisterOutcome::Registered { cancel, token } => (cancel, token),
            _ => panic!("应注册成功"),
        };
        assert_ne!(old_token, new_token);
        // 旧 token 的延迟清理不得删除新任务。
        state.remove("req-1", old_token);
        assert!(!state.is_idle(), "旧代次的清理不应删掉新任务");
        state.remove("req-1", new_token);
        assert!(state.is_idle());
    }

    #[tokio::test]
    async fn registry_cancel_all_terminates_every_task() {
        let state = PlaygroundState::default();
        let ident = identity();
        let cancel = match state.register("req-1", ident) {
            RegisterOutcome::Registered { cancel, .. } => cancel,
            _ => panic!("应注册成功"),
        };
        state.cancel_all();
        assert_notified(&cancel).await;
        // cancel_all 同样不 drain：任务项由各自 guard 按代次移除。
        assert!(!state.is_idle());
    }

    // ---- 上游状态映射 ----

    #[test]
    fn upstream_status_maps_to_stable_codes() {
        assert_eq!(upstream_status_code(401), CODE_KEY_INVALID);
        assert_eq!(upstream_status_code(403), CODE_KEY_INVALID);
        assert_eq!(upstream_status_code(402), CODE_QUOTA_EXHAUSTED);
        assert_eq!(upstream_status_code(429), CODE_RATE_LIMITED);
        assert_eq!(upstream_status_code(500), CODE_UPSTREAM_ERROR);
        assert_eq!(upstream_status_code(400), CODE_UPSTREAM_ERROR);
    }

    // ---- run_chat_stream（wiremock 端到端） ----

    fn stream_http_client() -> reqwest::Client {
        reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn chat_stream_emits_deltas_and_returns_usage() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"第一\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"第二\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],",
            "\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}\n\n",
            "data: [DONE]\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse),
            )
            .mount(&server)
            .await;

        let mut deltas: Vec<String> = Vec::new();
        let outcome = run_chat_stream(
            stream_http_client(),
            &format!("{}/v1/chat/completions", server.uri()),
            "sk-test",
            serde_json::json!({"model": "m", "messages": [], "stream": true}),
            Arc::new(Notify::new()),
            &|| true,
            &mut |delta| deltas.push(delta.to_string()),
        )
        .await
        .unwrap();
        assert_eq!(deltas, vec!["第一", "第二"]);
        assert_eq!(outcome.finish_reason.as_deref(), Some("stop"));
        assert_eq!(outcome.usage.unwrap().total_tokens, 5);
    }

    #[tokio::test]
    async fn chat_stream_maps_upstream_401_to_key_invalid_without_body() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_string(
                serde_json::json!({"error": {"message": "bad key sk-leak"}}).to_string(),
            ))
            .mount(&server)
            .await;

        let err = run_chat_stream(
            stream_http_client(),
            &format!("{}/v1/chat/completions", server.uri()),
            "sk-test",
            serde_json::json!({}),
            Arc::new(Notify::new()),
            &|| true,
            &mut |_| {},
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, CODE_KEY_INVALID);
        // 错误 body（可能含敏感回显）不得透传到 message。
        assert!(!err.message.contains("sk-leak"));
    }

    #[tokio::test]
    async fn chat_stream_cancel_signal_yields_cancelled_terminal() {
        let cancel = Arc::new(Notify::new());
        // 先触发取消，再发起请求（验证 notify_one 的许可语义）。
        cancel.notify_one();
        let err = run_chat_stream(
            stream_http_client(),
            "http://127.0.0.1:9/v1/chat/completions",
            "sk-test",
            serde_json::json!({}),
            cancel,
            &|| true,
            &mut |_| {},
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, CODE_CANCELLED);
    }

    #[tokio::test]
    async fn chat_stream_identity_change_yields_identity_terminal() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // 响应延迟 3 秒：连接阶段的身份 tick（2s）先到，验证 send() 阶段同样
        // 被取消/身份监督覆盖（不只是流式读取阶段）。
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_delay(Duration::from_secs(3)),
            )
            .mount(&server)
            .await;

        let err = run_chat_stream(
            stream_http_client(),
            &format!("{}/v1/chat/completions", server.uri()),
            "sk-test",
            serde_json::json!({}),
            Arc::new(Notify::new()),
            &|| false,
            &mut |_| {},
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, CODE_IDENTITY_CHANGED);
    }
}
