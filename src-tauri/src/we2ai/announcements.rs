//! 公告同步与系统通知（功能 19，见自定义开发功能列表.md）。
//!
//! - `we2ai_list_announcements` / `we2ai_mark_announcement_read`：代理 SubPanel
//!   用户公告接口，会话续期与 401 终止处理与 `keys.rs` 完全一致（走
//!   `call_protected_api`）。服务端 `ListForUser` 已按生效时间窗与定向条件过滤，
//!   已读状态以服务端 `read_at` 为准，客户端不做时间窗兜底。
//! - 后台轮询放在 Rust 侧：窗口隐藏到托盘 / 最小化后 WebView 的定时器与 JS 会被
//!   系统节流甚至挂起（macOS 隐藏窗口的 WKWebView、Windows 后台 WebView2 均如此），
//!   前端定时器无法保证 5 分钟一次。轮询任务只在会话处于 `Active` 时才发请求，
//!   会话结束（登出/终止）或换账号/区域后状态立即重置，不沿用上一会话的任何数据。
//! - 系统通知由 Rust 侧直接调用 `tauri-plugin-notification` 发出：只针对
//!   「未读 + popup + 此前没通知过」的公告，且仅在主窗口不在前台时发送；已通知的
//!   id 按「区域 + 用户」持久化到 `~/.we2ai/announcement_notified.json`（最多保留
//!   最近 200 条，只有公告 id，不含任何秘密）。注意：桌面端插件的权限查询恒为已
//!   授权、`show()` 是异步派发后恒返回 `Ok`，因此无法感知「用户关闭了本应用通知」
//!   或发送失败，调用一次即记为已通知；权限检查在桌面端是空操作，仅为移动端预留。
//! - 每次轮询发现未读集合变化时向前端发 `we2ai-announcements-changed` 事件，前端
//!   收到后重新拉取列表。

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::State;

use super::api::RemoteAnnouncement;
use super::commands_auth::We2aiApiError;
use super::session::{SessionError, SessionIdentity, SessionManager, We2aiSessionState};

/// 前端刷新事件名（Tauri 事件名只允许字母数字与 `-/:_`）。
pub const EVENT_CHANGED: &str = "we2ai-announcements-changed";

/// 后台轮询间隔。
pub const POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// 监督循环检查会话身份的间隔（只读内存状态，代价极低）。
const SUPERVISOR_TICK: Duration = Duration::from_secs(5);

/// 每个「区域 + 用户」最多保留的已通知公告 id 数。
pub const NOTIFIED_MAX: usize = 200;
const NOTIFIED_FILE_NAME: &str = "announcement_notified.json";

/// 系统通知正文的最大字符数。
const NOTIFICATION_BODY_MAX_CHARS: usize = 80;

// ---------------------------------------------------------------------------
// 前端视图
// ---------------------------------------------------------------------------

/// 返回给前端的公告视图（camelCase）。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AnnouncementView {
    pub id: i64,
    pub title: String,
    /// Markdown 正文，由前端净化后渲染。
    pub content: String,
    /// `"popup"` | `"silent"`（未识别的值归一化为 `silent`）。
    pub notify_mode: String,
    pub starts_at: Option<String>,
    pub ends_at: Option<String>,
    pub read_at: Option<String>,
    pub created_at: String,
}

fn is_popup(a: &RemoteAnnouncement) -> bool {
    a.notify_mode.eq_ignore_ascii_case("popup")
}

fn is_unread(a: &RemoteAnnouncement) -> bool {
    a.read_at.as_deref().map(str::trim).unwrap_or("").is_empty()
}

fn to_view(a: &RemoteAnnouncement) -> AnnouncementView {
    AnnouncementView {
        id: a.id,
        title: a.title.clone(),
        content: a.content.clone(),
        notify_mode: if is_popup(a) { "popup" } else { "silent" }.to_string(),
        starts_at: a.starts_at.clone().filter(|s| !s.is_empty()),
        ends_at: a.ends_at.clone().filter(|s| !s.is_empty()),
        read_at: a.read_at.clone().filter(|s| !s.is_empty()),
        created_at: a.created_at.clone(),
    }
}

/// 创建时间从旧到新，同一时刻按 id。时间解析失败按 0 处理（排最前）。
fn sort_key(a: &RemoteAnnouncement) -> (i64, i64) {
    let ts = chrono::DateTime::parse_from_rfc3339(&a.created_at)
        .map(|d| d.timestamp_millis())
        .unwrap_or(0);
    (ts, a.id)
}

fn sort_oldest_first(items: &mut [RemoteAnnouncement]) {
    items.sort_by_key(sort_key);
}

// ---------------------------------------------------------------------------
// 命令
// ---------------------------------------------------------------------------

async fn fetch(
    manager: &SessionManager,
    unread_only: bool,
) -> Result<(SessionIdentity, Vec<RemoteAnnouncement>), SessionError> {
    let (mut items, identity) = manager
        .call_protected_api(true, move |api, token| async move {
            api.list_announcements(&token, unread_only).await
        })
        .await?;
    sort_oldest_first(&mut items);
    Ok((identity, items))
}

/// 当前用户可见的全部公告（含已读），创建时间从旧到新。
pub async fn list_announcements(
    manager: &SessionManager,
    unread_only: bool,
) -> Result<Vec<AnnouncementView>, SessionError> {
    let (_, items) = fetch(manager, unread_only).await?;
    Ok(items.iter().map(to_view).collect())
}

/// 标记已读。服务端为 upsert，因此按幂等请求处理（瞬时错误可自动重放）。
pub async fn mark_announcement_read(manager: &SessionManager, id: i64) -> Result<(), SessionError> {
    manager
        .call_protected_api(true, move |api, token| async move {
            api.mark_announcement_read(&token, id).await
        })
        .await
        .map(|_| ())
}

#[tauri::command]
pub async fn we2ai_list_announcements(
    session: State<'_, We2aiSessionState>,
    unread_only: Option<bool>,
) -> Result<Vec<AnnouncementView>, We2aiApiError> {
    let manager = session.0.clone();
    Ok(list_announcements(&manager, unread_only.unwrap_or(false)).await?)
}

#[tauri::command]
pub async fn we2ai_mark_announcement_read(
    session: State<'_, We2aiSessionState>,
    id: i64,
) -> Result<(), We2aiApiError> {
    let manager = session.0.clone();
    Ok(mark_announcement_read(&manager, id).await?)
}

// ---------------------------------------------------------------------------
// 已通知 id 的持久化
// ---------------------------------------------------------------------------

/// `~/.we2ai/announcement_notified.json`：`"<区域>:<用户 id>" → [公告 id…]`，
/// 顺序即通知先后。只是去重记忆，读失败按「没有记忆」处理。
struct NotifiedFile;

#[derive(Debug, Default, Serialize, Deserialize)]
struct NotifiedData {
    #[serde(default)]
    notified: HashMap<String, Vec<i64>>,
}

impl NotifiedFile {
    fn path(data_root: &Path) -> PathBuf {
        data_root.join(NOTIFIED_FILE_NAME)
    }

    fn slot(identity: &SessionIdentity) -> String {
        format!("{}:{}", identity.region.storage_key(), identity.user_id)
    }

    fn read(data_root: &Path) -> NotifiedData {
        std::fs::read_to_string(Self::path(data_root))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn load(data_root: &Path, identity: &SessionIdentity) -> HashSet<i64> {
        Self::read(data_root)
            .notified
            .get(&Self::slot(identity))
            .map(|ids| ids.iter().copied().collect())
            .unwrap_or_default()
    }

    fn record(
        data_root: &Path,
        identity: &SessionIdentity,
        ids: &[i64],
    ) -> Result<(), crate::error::AppError> {
        let mut data = Self::read(data_root);
        let list = data.notified.entry(Self::slot(identity)).or_default();
        for id in ids {
            if !list.contains(id) {
                list.push(*id);
            }
        }
        if list.len() > NOTIFIED_MAX {
            let excess = list.len() - NOTIFIED_MAX;
            list.drain(..excess);
        }
        let json = serde_json::to_string_pretty(&data).unwrap_or_default();
        crate::config::atomic_write_private(&Self::path(data_root), json.as_bytes())
    }
}

// ---------------------------------------------------------------------------
// 系统通知文案
// ---------------------------------------------------------------------------

/// 把 Markdown 粗略压成一行纯文本（只用于系统通知摘要，不做渲染）。
fn plain_snippet(markdown: &str, max_chars: usize) -> String {
    use once_cell::sync::Lazy;
    use regex::Regex;
    static IMAGE: Lazy<Regex> = Lazy::new(|| Regex::new(r"!\[[^\]]*\]\([^)]*\)").unwrap());
    static LINK: Lazy<Regex> = Lazy::new(|| Regex::new(r"\[([^\]]*)\]\([^)]*\)").unwrap());
    static TAG: Lazy<Regex> = Lazy::new(|| Regex::new(r"<[^>]*>").unwrap());
    static MARKS: Lazy<Regex> = Lazy::new(|| Regex::new(r"[`*_#>~]").unwrap());
    let s = IMAGE.replace_all(markdown, "");
    let s = LINK.replace_all(&s, "$1");
    let s = TAG.replace_all(&s, "");
    let s = MARKS.replace_all(&s, "");
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&collapsed, max_chars)
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('\u{2026}');
    out
}

/// 系统通知的标题与正文：单条用公告标题与正文摘要，多条合并为一条。
fn build_notification(zh: bool, items: &[&RemoteAnnouncement]) -> (String, String) {
    match items {
        [] => (String::new(), String::new()),
        [one] => {
            let title = if one.title.trim().is_empty() {
                if zh {
                    "WE2AI 公告"
                } else {
                    "WE2AI announcement"
                }
                .to_string()
            } else {
                truncate_chars(one.title.trim(), 60)
            };
            (
                title,
                plain_snippet(&one.content, NOTIFICATION_BODY_MAX_CHARS),
            )
        }
        many => {
            let title = if zh {
                format!("WE2AI：{} 条新公告", many.len())
            } else {
                format!("WE2AI: {} new announcements", many.len())
            };
            let sep = if zh { "、" } else { ", " };
            let titles = many
                .iter()
                .map(|a| a.title.trim())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(sep);
            (title, truncate_chars(&titles, NOTIFICATION_BODY_MAX_CHARS))
        }
    }
}

// ---------------------------------------------------------------------------
// 轮询
// ---------------------------------------------------------------------------

/// 轮询对外部世界的依赖，测试期替换为假实现。
pub trait Host: Send + Sync {
    /// 主窗口当前不在前台（隐藏、最小化或未聚焦）。
    fn window_in_background(&self) -> bool;
    /// 界面语言是否为中文（决定多条合并通知的标题语言）。
    fn prefers_chinese(&self) -> bool;
    /// 发送系统通知；返回 `false` 表示没有发出（静默降级为仅应用内提示）。桌面端
    /// 插件不会返回 `false`（见模块文档），该分支主要是宿主契约。
    fn show_notification(&self, title: &str, body: &str) -> bool;
    /// 通知前端未读集合变化，让它重新拉取。
    fn announcements_changed(&self);
}

/// 单个会话内的轮询状态。会话身份变化时整体丢弃重建。
#[derive(Default)]
pub struct PollState {
    /// 本进程内已处理过、不应再发系统通知的公告：窗口在前台时已走弹窗的，
    /// 以及已调用过通知（或宿主报告未发出）的。不持久化。
    handled: HashSet<i64>,
    /// 上一次轮询的未读集合（id + `updated_at`），用于判断是否要通知前端刷新；
    /// 带上 `updated_at` 是为了让同一条未读公告被修改时前台也能更新。
    last_unread: Option<BTreeSet<(i64, String)>>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PollOutcome {
    /// 本次发出系统通知的公告 id。
    pub notified: Vec<i64>,
    /// 是否向前端发了刷新事件。
    pub changed: bool,
}

/// 拉取一次未读公告并按规则决定是否发系统通知：
/// - 只考虑 `popup` 且未读、且从未通知过（持久化记录 + 本进程记录）的公告；
/// - 窗口在前台：不通知（前端走弹窗），记为已处理，避免之后切到后台再补发；
/// - 窗口在后台：合并为一条系统通知，成功后持久化 id。
pub async fn poll_once(
    manager: &SessionManager,
    host: &dyn Host,
    state: &mut PollState,
) -> Result<PollOutcome, SessionError> {
    let (identity, items) = fetch(manager, true).await?;
    // 拉取期间会话已切换：这份数据属于旧会话，丢弃。
    if manager.current_identity() != Some(identity) {
        return Err(SessionError::SessionChanged);
    }
    let unread: Vec<&RemoteAnnouncement> = items.iter().filter(|a| is_unread(a)).collect();
    let unread_versions: BTreeSet<(i64, String)> = unread
        .iter()
        .map(|a| (a.id, a.updated_at.clone()))
        .collect();

    let mut outcome = PollOutcome::default();

    let notified_before = NotifiedFile::load(manager.data_root(), &identity);
    let candidates: Vec<&RemoteAnnouncement> = unread
        .iter()
        .copied()
        .filter(|a| {
            is_popup(a) && !notified_before.contains(&a.id) && !state.handled.contains(&a.id)
        })
        .collect();

    if !candidates.is_empty() {
        if host.window_in_background() {
            let (title, body) = build_notification(host.prefers_chinese(), &candidates);
            let ids: Vec<i64> = candidates.iter().map(|a| a.id).collect();
            state.handled.extend(ids.iter().copied());
            if host.show_notification(&title, &body) {
                if let Err(e) = NotifiedFile::record(manager.data_root(), &identity, &ids) {
                    log::warn!("记录已通知公告失败（本进程内仍不会重复通知）: {e}");
                }
                outcome.notified = ids;
            }
        } else {
            state.handled.extend(candidates.iter().map(|a| a.id));
        }
    }

    if state.last_unread.as_ref() != Some(&unread_versions) {
        state.last_unread = Some(unread_versions);
        host.announcements_changed();
        outcome.changed = true;
    }
    Ok(outcome)
}

/// 监督循环的内部状态：当前会话身份、下次轮询时间与该会话的轮询状态。
pub struct SupervisorState {
    identity: Option<SessionIdentity>,
    next_due: Instant,
    poll: PollState,
}

impl SupervisorState {
    pub fn new(now: Instant) -> Self {
        Self {
            identity: None,
            next_due: now,
            poll: PollState::default(),
        }
    }
}

/// 监督循环的一步：会话身份变化（登录、登出、换账号/区域）时重置轮询状态并
/// 立即轮询一次；无 `Active` 会话时什么都不做；否则到点才轮询。返回是否发了请求。
pub async fn supervisor_step(
    manager: &SessionManager,
    host: &dyn Host,
    state: &mut SupervisorState,
    now: Instant,
    interval: Duration,
) -> bool {
    let current = manager.current_identity();
    if current != state.identity {
        state.identity = current;
        state.poll = PollState::default();
        state.next_due = now;
    }
    if current.is_none() || now < state.next_due {
        return false;
    }
    state.next_due = now + interval;
    if let Err(e) = poll_once(manager, host, &mut state.poll).await {
        // 静默：不打扰用户，下一个间隔自然重试。
        log::debug!("公告后台轮询失败: {e}");
        // 会话已被终止（401）：让前端重拉，拿到非网络类错误码后复查会话状态，
        // 否则窗口一直在前台时界面会继续显示已登录。
        if matches!(e, SessionError::Terminated(_)) {
            host.announcements_changed();
        }
    }
    true
}

async fn run_supervisor<H: Host>(
    manager: SessionManager,
    host: H,
    tick: Duration,
    interval: Duration,
) {
    let mut state = SupervisorState::new(Instant::now());
    loop {
        supervisor_step(&manager, &host, &mut state, Instant::now(), interval).await;
        tokio::time::sleep(tick).await;
    }
}

// ---------------------------------------------------------------------------
// Tauri 宿主
// ---------------------------------------------------------------------------

struct TauriHost {
    app: tauri::AppHandle,
}

impl Host for TauriHost {
    fn window_in_background(&self) -> bool {
        use tauri::Manager;
        let Some(window) = self.app.get_webview_window("main") else {
            return true;
        };
        let visible = window.is_visible().unwrap_or(false);
        let minimized = window.is_minimized().unwrap_or(false);
        let focused = window.is_focused().unwrap_or(false);
        !(visible && !minimized && focused)
    }

    fn prefers_chinese(&self) -> bool {
        match crate::settings::get_settings().language {
            Some(lang) => {
                !lang.to_lowercase().starts_with("en") && !lang.to_lowercase().starts_with("ja")
            }
            None => true,
        }
    }

    fn show_notification(&self, title: &str, body: &str) -> bool {
        use tauri_plugin_notification::{NotificationExt, PermissionState};
        let notification = self.app.notification();
        let granted = match notification.permission_state() {
            Ok(PermissionState::Granted) => true,
            _ => matches!(
                notification.request_permission(),
                Ok(PermissionState::Granted)
            ),
        };
        if !granted {
            log::info!("系统通知权限未授予，公告仅在应用内提示");
            return false;
        }
        match notification.builder().title(title).body(body).show() {
            Ok(()) => true,
            Err(e) => {
                log::warn!("发送公告系统通知失败: {e}");
                false
            }
        }
    }

    fn announcements_changed(&self) {
        use tauri::Emitter;
        let _ = self.app.emit(EVENT_CHANGED, ());
    }
}

/// 在应用启动时调用一次：启动后台公告轮询。任务只在会话 `Active` 时发请求。
pub fn start_background_poller(app: &tauri::AppHandle) {
    use tauri::Manager;
    let manager = app.state::<We2aiSessionState>().0.clone();
    let host = TauriHost { app: app.clone() };
    tauri::async_runtime::spawn(run_supervisor(
        manager,
        host,
        SUPERVISOR_TICK,
        POLL_INTERVAL,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::we2ai::region::Region;
    use crate::we2ai::secret_store::test_support::InMemorySecretStore;
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn manager(dir: &TempDir) -> SessionManager {
        SessionManager::new(
            Arc::new(InMemorySecretStore::new()),
            dir.path().to_path_buf(),
            "test".to_string(),
        )
    }

    fn ann(id: i64, mode: &str, created: &str, read: Option<&str>) -> Value {
        json!({
            "id": id, "title": format!("标题{id}"), "content": format!("**正文** {id} [链接](https://we2ai.com)"),
            "notify_mode": mode, "created_at": created, "updated_at": created,
            "read_at": read,
        })
    }

    async fn mount_list(server: &MockServer, unread_only: bool, items: Vec<Value>) {
        Mock::given(method("GET"))
            .and(path("/api/v1/announcements"))
            .and(query_param("unread_only", unread_only.to_string()))
            .and(header("authorization", "Bearer access-1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"code": 0, "message": "success", "data": items})),
            )
            .mount(server)
            .await;
    }

    #[derive(Default)]
    struct FakeHost {
        background: AtomicBool,
        notify_ok: AtomicBool,
        shown: Mutex<Vec<(String, String)>>,
        changed: AtomicUsize,
    }

    impl FakeHost {
        fn new(background: bool) -> Self {
            Self {
                background: AtomicBool::new(background),
                notify_ok: AtomicBool::new(true),
                ..Default::default()
            }
        }
        fn shown(&self) -> Vec<(String, String)> {
            self.shown.lock().unwrap().clone()
        }
    }

    impl Host for FakeHost {
        fn window_in_background(&self) -> bool {
            self.background.load(Ordering::SeqCst)
        }
        fn prefers_chinese(&self) -> bool {
            true
        }
        fn show_notification(&self, title: &str, body: &str) -> bool {
            if !self.notify_ok.load(Ordering::SeqCst) {
                return false;
            }
            self.shown
                .lock()
                .unwrap()
                .push((title.to_string(), body.to_string()));
            true
        }
        fn announcements_changed(&self) {
            self.changed.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn list_returns_oldest_first_and_normalizes_fields() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        mount_list(
            &server,
            false,
            vec![
                ann(3, "popup", "2026-03-01T00:00:00Z", None),
                ann(
                    1,
                    "silent",
                    "2026-01-01T00:00:00Z",
                    Some("2026-01-02T00:00:00Z"),
                ),
                ann(2, "weird", "2026-02-01T00:00:00Z", None),
            ],
        )
        .await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());

        let views = list_announcements(&manager, false).await.unwrap();

        assert_eq!(
            views.iter().map(|v| v.id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(views[0].read_at.as_deref(), Some("2026-01-02T00:00:00Z"));
        assert_eq!(
            views[1].notify_mode, "silent",
            "未识别的 notify_mode 归一化为 silent"
        );
        assert_eq!(views[2].notify_mode, "popup");
        assert_eq!(views[2].read_at, None);
        let json = serde_json::to_value(&views[2]).unwrap();
        assert_eq!(json["notifyMode"], "popup");
        assert_eq!(json["createdAt"], "2026-03-01T00:00:00Z");
        assert!(json.get("notify_mode").is_none());
    }

    #[tokio::test]
    async fn mark_read_posts_to_the_announcement_with_bearer_token() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/announcements/7/read"))
            .and(header("authorization", "Bearer access-1"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"code": 0, "message": "success", "data": {"message": "ok"}}),
                ),
            )
            .expect(1)
            .mount(&server)
            .await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());

        mark_announcement_read(&manager, 7).await.unwrap();
    }

    #[tokio::test]
    async fn revoked_token_terminates_the_session_and_surfaces_the_code() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/announcements"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_json(json!({"code": "TOKEN_REVOKED", "message": "revoked"})),
            )
            .mount(&server)
            .await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());

        let err = list_announcements(&manager, false).await.unwrap_err();

        assert_eq!(err, SessionError::Terminated("TOKEN_REVOKED".to_string()));
        assert!(manager.current_identity().is_none(), "会话应已终止");
        let api_err: We2aiApiError = err.into();
        assert_eq!(api_err.code, "TOKEN_REVOKED");
    }

    #[tokio::test]
    async fn network_failure_is_a_transient_error_and_keeps_the_session() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        // 端口 1 无人监听：连接被拒绝。
        manager.test_seed_active(
            Region::International,
            42,
            "access-1",
            "http://127.0.0.1:1".to_string(),
        );

        let err = list_announcements(&manager, true).await.unwrap_err();

        assert!(matches!(err, SessionError::Transient(_)), "got {err:?}");
        assert!(manager.current_identity().is_some(), "网络失败不应终止会话");
    }

    #[tokio::test]
    async fn requires_an_active_session() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let err = list_announcements(&manager, false).await.unwrap_err();
        assert_eq!(err, SessionError::NoActiveSession);
    }

    /// 后台：只通知未读 popup（silent、已读不通知），多条合并为一条；已通知
    /// 记录写盘、只含 id；同一进程与重启后都不会重复通知。
    #[tokio::test]
    async fn background_poll_notifies_new_unread_popups_once_and_persists_ids() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        mount_list(
            &server,
            true,
            vec![
                ann(1, "popup", "2026-01-01T00:00:00Z", None),
                ann(2, "silent", "2026-01-02T00:00:00Z", None),
                ann(
                    3,
                    "popup",
                    "2026-01-03T00:00:00Z",
                    Some("2026-01-04T00:00:00Z"),
                ),
                ann(4, "popup", "2026-01-05T00:00:00Z", None),
            ],
        )
        .await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        let host = FakeHost::new(true);
        let mut state = PollState::default();

        let first = poll_once(&manager, &host, &mut state).await.unwrap();
        assert_eq!(first.notified, vec![1, 4]);
        assert!(first.changed);
        let shown = host.shown();
        assert_eq!(shown.len(), 1, "多条合并为一条系统通知");
        assert_eq!(shown[0].0, "WE2AI：2 条新公告");

        let second = poll_once(&manager, &host, &mut state).await.unwrap();
        assert!(second.notified.is_empty());
        assert!(!second.changed, "未读集合没变不重复通知前端");
        assert_eq!(host.shown().len(), 1);

        // 模拟应用重启：内存状态丢失，靠持久化记录去重。
        let mut fresh = PollState::default();
        let third = poll_once(&manager, &host, &mut fresh).await.unwrap();
        assert!(third.notified.is_empty());
        assert_eq!(host.shown().len(), 1);

        let raw = std::fs::read_to_string(dir.path().join("announcement_notified.json")).unwrap();
        let parsed: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["notified"]["international:42"], json!([1, 4]));
        assert!(
            !raw.contains("access-1") && !raw.contains("标题"),
            "只能有 id，不含正文或秘密"
        );
    }

    #[tokio::test]
    async fn single_notification_uses_title_and_plain_text_snippet() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        mount_list(
            &server,
            true,
            vec![ann(9, "popup", "2026-01-01T00:00:00Z", None)],
        )
        .await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        let host = FakeHost::new(true);

        poll_once(&manager, &host, &mut PollState::default())
            .await
            .unwrap();

        let shown = host.shown();
        assert_eq!(
            shown,
            vec![("标题9".to_string(), "正文 9 链接".to_string())]
        );
    }

    /// 启动后首次拉取里已有的未读：前台走弹窗不发系统通知，之后切到后台也不
    /// 补发；全新进程若窗口在后台则会发。
    #[tokio::test]
    async fn foreground_poll_never_notifies_but_a_backgrounded_restart_does() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        mount_list(
            &server,
            true,
            vec![ann(1, "popup", "2026-01-01T00:00:00Z", None)],
        )
        .await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        let host = FakeHost::new(false);
        let mut state = PollState::default();

        let first = poll_once(&manager, &host, &mut state).await.unwrap();
        assert!(first.notified.is_empty());
        assert!(first.changed, "前端仍要被告知刷新以弹窗");

        host.background.store(true, Ordering::SeqCst);
        let second = poll_once(&manager, &host, &mut state).await.unwrap();
        assert!(second.notified.is_empty(), "前台已弹窗处理过，切后台不补发");
        assert!(host.shown().is_empty());
        assert!(
            !dir.path().join("announcement_notified.json").exists(),
            "没发过系统通知就不写记录"
        );

        let third = poll_once(&manager, &host, &mut PollState::default())
            .await
            .unwrap();
        assert_eq!(third.notified, vec![1]);
        assert_eq!(host.shown().len(), 1);
    }

    #[tokio::test]
    /// 测的是 `Host` 契约：宿主报告「没发出」时不写去重记录、不弹错。tauri 桌面端
    /// 插件的 `show()` 恒返回 `Ok`、权限恒为已授权，生产环境不会走到这条路径。
    async fn refused_permission_degrades_silently_without_recording() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        mount_list(
            &server,
            true,
            vec![ann(1, "popup", "2026-01-01T00:00:00Z", None)],
        )
        .await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        let host = FakeHost::new(true);
        host.notify_ok.store(false, Ordering::SeqCst);
        let mut state = PollState::default();

        let outcome = poll_once(&manager, &host, &mut state).await.unwrap();
        assert!(outcome.notified.is_empty());
        assert!(host.shown().is_empty());
        assert!(!dir.path().join("announcement_notified.json").exists());

        // 同一进程不反复尝试；权限之后放开、且是新进程时才会补发。
        host.notify_ok.store(true, Ordering::SeqCst);
        assert!(poll_once(&manager, &host, &mut state)
            .await
            .unwrap()
            .notified
            .is_empty());
        let restarted = poll_once(&manager, &host, &mut PollState::default())
            .await
            .unwrap();
        assert_eq!(restarted.notified, vec![1]);
    }

    #[tokio::test]
    async fn notified_records_are_kept_per_region_and_user() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        mount_list(
            &server,
            true,
            vec![ann(1, "popup", "2026-01-01T00:00:00Z", None)],
        )
        .await;
        let host = FakeHost::new(true);

        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        poll_once(&manager, &host, &mut PollState::default())
            .await
            .unwrap();
        manager.test_seed_active(Region::International, 43, "access-1", server.uri());
        let other_user = poll_once(&manager, &host, &mut PollState::default())
            .await
            .unwrap();
        assert_eq!(other_user.notified, vec![1], "另一个用户有自己的去重记录");
        manager.test_seed_active(Region::DomesticProd, 42, "access-1", server.uri());
        let other_region = poll_once(&manager, &host, &mut PollState::default())
            .await
            .unwrap();
        assert_eq!(other_region.notified, vec![1], "另一个区域有自己的去重记录");
        assert_eq!(host.shown().len(), 3);
    }

    #[test]
    fn notified_file_keeps_only_the_latest_200_ids() {
        let dir = TempDir::new().unwrap();
        let identity = SessionIdentity {
            region: Region::International,
            user_id: 42,
            generation: 1,
        };
        let first: Vec<i64> = (1..=150).collect();
        let second: Vec<i64> = (151..=260).collect();
        NotifiedFile::record(dir.path(), &identity, &first).unwrap();
        NotifiedFile::record(dir.path(), &identity, &second).unwrap();
        // 已在列表里的 id 不重复追加。
        NotifiedFile::record(dir.path(), &identity, &[260]).unwrap();

        let loaded = NotifiedFile::load(dir.path(), &identity);
        assert_eq!(loaded.len(), NOTIFIED_MAX);
        assert!(!loaded.contains(&60), "最旧的应被淘汰");
        assert!(loaded.contains(&61) && loaded.contains(&260));

        // 再追加一条新 id：挤掉当前最旧的 61。
        NotifiedFile::record(dir.path(), &identity, &[999]).unwrap();
        let loaded = NotifiedFile::load(dir.path(), &identity);
        assert_eq!(loaded.len(), NOTIFIED_MAX);
        assert!(!loaded.contains(&61) && loaded.contains(&62) && loaded.contains(&999));
    }

    /// 监督循环：无会话不发请求；登录后立即轮询；间隔内不重复；换账号立即
    /// 重置状态并重新轮询；登出后停止。
    #[tokio::test]
    async fn supervisor_polls_only_while_a_session_is_active_and_resets_on_switch() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        mount_list(&server, true, vec![]).await;
        let host = FakeHost::new(true);
        let interval = Duration::from_secs(300);
        let t0 = Instant::now();
        let mut state = SupervisorState::new(t0);

        assert!(!supervisor_step(&manager, &host, &mut state, t0, interval).await);
        assert_eq!(server.received_requests().await.unwrap().len(), 0);

        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        assert!(supervisor_step(&manager, &host, &mut state, t0, interval).await);
        assert!(
            !supervisor_step(
                &manager,
                &host,
                &mut state,
                t0 + Duration::from_secs(60),
                interval
            )
            .await,
            "间隔内不重复轮询"
        );
        assert!(
            supervisor_step(&manager, &host, &mut state, t0 + interval, interval).await,
            "到点再次轮询"
        );

        // 换账号：不等间隔，立即重新轮询，且 PollState 已重置（changed 再次触发）。
        let changed_before = host.changed.load(Ordering::SeqCst);
        manager.test_seed_active(Region::International, 43, "access-1", server.uri());
        assert!(
            supervisor_step(
                &manager,
                &host,
                &mut state,
                t0 + interval + Duration::from_secs(1),
                interval
            )
            .await
        );
        assert_eq!(host.changed.load(Ordering::SeqCst), changed_before + 1);
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }

    /// 同一条未读公告被修改（id 与已读状态不变、`updated_at` 变化）也要通知前端刷新。
    #[tokio::test]
    async fn editing_an_unread_announcement_notifies_the_frontend() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        mount_list(
            &server,
            true,
            vec![ann(1, "silent", "2026-01-01T00:00:00Z", None)],
        )
        .await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        let host = FakeHost::new(false);
        let mut state = PollState::default();

        assert!(poll_once(&manager, &host, &mut state).await.unwrap().changed);
        assert!(!poll_once(&manager, &host, &mut state).await.unwrap().changed);

        let mut edited = ann(1, "silent", "2026-01-01T00:00:00Z", None);
        edited["updated_at"] = json!("2026-01-05T00:00:00Z");
        server.reset().await;
        mount_list(&server, true, vec![edited]).await;

        assert!(poll_once(&manager, &host, &mut state).await.unwrap().changed);
        assert_eq!(host.changed.load(Ordering::SeqCst), 2);
    }

    /// 后台轮询遇到会话被终止（401）：通知前端重拉，让它复查会话状态；网络失败不通知。
    #[tokio::test]
    async fn supervisor_tells_the_frontend_when_the_session_is_terminated() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/announcements"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_json(json!({"code": "TOKEN_REVOKED", "message": "revoked"})),
            )
            .mount(&server)
            .await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        let host = FakeHost::new(true);
        let t0 = Instant::now();
        let mut state = SupervisorState::new(t0);

        assert!(supervisor_step(&manager, &host, &mut state, t0, Duration::from_secs(300)).await);

        assert!(manager.current_identity().is_none(), "会话应已终止");
        assert_eq!(host.changed.load(Ordering::SeqCst), 1);

        // 网络失败（会话仍在）不触发刷新事件。
        let offline_host = FakeHost::new(true);
        let offline_manager = self::manager(&dir);
        offline_manager.test_seed_active(
            Region::International,
            42,
            "access-1",
            "http://127.0.0.1:1".to_string(),
        );
        let mut offline_state = SupervisorState::new(t0);
        assert!(
            supervisor_step(
                &offline_manager,
                &offline_host,
                &mut offline_state,
                t0,
                Duration::from_secs(300)
            )
            .await
        );
        assert_eq!(offline_host.changed.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn snippet_strips_markdown_and_truncates() {
        assert_eq!(
            plain_snippet(
                "# 标题\n**加粗** 与 [链接](https://a.b) ![图](https://x/y.png)\n> 引用",
                80
            ),
            "标题 加粗 与 链接 引用"
        );
        let long = "字".repeat(100);
        let out = plain_snippet(&long, 10);
        assert_eq!(out.chars().count(), 11);
        assert!(out.ends_with('\u{2026}'));
    }

    #[test]
    fn notification_text_is_localized_for_multiple_items() {
        let a = RemoteAnnouncement {
            id: 1,
            title: "A".into(),
            content: String::new(),
            notify_mode: "popup".into(),
            starts_at: None,
            ends_at: None,
            read_at: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let b = RemoteAnnouncement {
            id: 2,
            title: "B".into(),
            ..a.clone()
        };
        assert_eq!(
            build_notification(false, &[&a, &b]),
            ("WE2AI: 2 new announcements".to_string(), "A, B".to_string())
        );
        assert_eq!(build_notification(true, &[&a, &b]).0, "WE2AI：2 条新公告");
    }
}
