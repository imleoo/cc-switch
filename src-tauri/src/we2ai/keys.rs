//! Key 列表与模型广场（方案第 1 节用户旅程、第 3.3 节、第 8 节 P3）。
//!
//! - `we2ai_list_keys`：分页拉取 `/api/v1/keys` 直到最后一页，只保留
//!   `active` 与 `quota_exhausted` 两种状态；返回给前端的是**脱敏视图**，
//!   明文 Key 只留在 Rust 进程内存（[`We2aiKeyState`]），供 P4 写工具配置时
//!   使用，从不经过 IPC 返回。
//! - Key 选择规则（减少选择）：记住的上次选择仍存在则沿用；否则取列表第一个
//!   （只有一个 Key 时即自动选中）。记忆按"区域 + 用户"分开存放在
//!   `~/.we2ai/key_selection.json`，不含任何秘密。
//! - `we2ai_key_models`：调 SubPanel B1，返回模型、每个模型可用的工具、
//!   Key 级准入结果。工具名只保留客户端认识的三个。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::State;

use super::api::{RemoteApiKey, RemoteKeyModels};
use super::commands_auth::We2aiApiError;
use super::session::{SessionError, SessionIdentity, SessionManager, We2aiSessionState};

/// `/api/v1/keys` 每页条数（SubPanel `ParsePagination` 上限 1000）。
const KEYS_PAGE_SIZE: u32 = 100;
/// 分页安全上限：100 × 50 = 5000 个 Key，远超正常账号规模；服务端若返回
/// 异常的 `pages` 也不会无限循环。
const KEYS_MAX_PAGES: u32 = 50;

/// 客户端能写入的三个工具（方案第 0 节决策 4）。B1 返回其他值时忽略。
pub const KNOWN_TOOLS: [&str; 3] = ["claude_code", "codex", "workbuddy"];

/// 前端展示的 Key 状态（方案 3.3 节：只展示这两种）。
const VISIBLE_STATUSES: [&str; 2] = ["active", "quota_exhausted"];

/// 进程内缓存的 Key 列表（含明文），与拉取时的会话身份绑定。
struct KeyCache {
    identity: SessionIdentity,
    keys: Vec<RemoteApiKey>,
}

#[derive(Default)]
pub struct We2aiKeyState {
    cache: Mutex<Option<KeyCache>>,
    /// 列表拉取序号：每次拉取开始时领取，只有比已写入缓存的序号更新的结果
    /// 才能写缓存，避免较早发起、较晚完成的拉取覆盖较新的列表（Codex P3
    /// 验收第 2 轮中危项）。
    fetch_seq: std::sync::atomic::AtomicU64,
    stored_seq: Mutex<u64>,
}

impl We2aiKeyState {
    fn begin_fetch(&self) -> u64 {
        self.fetch_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1
    }

    /// 写入缓存；`seq` 不比上次写入的新时丢弃，返回是否写入。
    fn store(&self, seq: u64, identity: SessionIdentity, keys: Vec<RemoteApiKey>) -> bool {
        let mut cache = self.cache.lock().unwrap();
        let mut stored = self.stored_seq.lock().unwrap();
        if seq <= *stored {
            return false;
        }
        *stored = seq;
        *cache = Some(KeyCache { identity, keys });
        true
    }

    /// 清空缓存（登出后调用，避免明文 Key 在内存里多留）。
    pub fn clear(&self) {
        *self.cache.lock().unwrap() = None;
    }

    /// 取某个 Key 的明文。只有缓存身份与当前会话身份一致才返回；身份不一致
    /// （已登出、换账号、换区域、重新登录）时顺带清空缓存。
    pub fn secret_for(&self, current: Option<SessionIdentity>, key_id: i64) -> Option<String> {
        let mut guard = self.cache.lock().unwrap();
        match (&*guard, current) {
            (Some(cache), Some(current)) if cache.identity == current => cache
                .keys
                .iter()
                .find(|k| k.id == key_id)
                .map(|k| k.key.clone()),
            (Some(_), _) => {
                *guard = None;
                None
            }
            (None, _) => None,
        }
    }

    fn contains(&self, current: Option<SessionIdentity>, key_id: i64) -> bool {
        self.secret_for(current, key_id).is_some()
    }
}

/// 前端可见的 Key 视图（不含明文）。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct KeyView {
    pub id: i64,
    pub name: String,
    pub group_name: Option<String>,
    pub status: String,
    pub masked_key: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct KeyListView {
    pub keys: Vec<KeyView>,
    pub selected_key_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelView {
    pub id: String,
    pub provider: Option<String>,
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct KeyModelsView {
    pub models: Vec<ModelView>,
    pub callable: bool,
    pub blocked_reason: Option<String>,
}

/// 脱敏：保留前 6 位与后 4 位，中间用 `…`；过短时只保留前 2 位。
pub fn mask_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 12 {
        let head: String = chars.iter().take(2).collect();
        return format!("{head}…");
    }
    let head: String = chars[..6].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

fn to_view(key: &RemoteApiKey) -> KeyView {
    KeyView {
        id: key.id,
        name: key.name.clone(),
        group_name: key.group.as_ref().map(|g| g.name.clone()),
        status: key.status.clone(),
        masked_key: mask_key(&key.key),
    }
}

fn to_models_view(remote: RemoteKeyModels) -> KeyModelsView {
    let models = remote
        .models
        .into_iter()
        .map(|m| ModelView {
            id: m.id,
            provider: m.provider.filter(|p| !p.is_empty()),
            tools: KNOWN_TOOLS
                .iter()
                .filter(|t| m.tools.iter().any(|x| x == *t))
                .map(|t| t.to_string())
                .collect(),
        })
        .collect();
    KeyModelsView {
        models,
        callable: remote.callable,
        blocked_reason: if remote.callable {
            None
        } else {
            remote.blocked_reason.filter(|r| !r.is_empty())
        },
    }
}

/// 选择规则：记住的 Key 仍在列表里则沿用；否则第一个；列表为空为 `None`。
fn choose_selected(keys: &[KeyView], remembered: Option<i64>) -> Option<i64> {
    remembered
        .filter(|id| keys.iter().any(|k| k.id == *id))
        .or_else(|| keys.first().map(|k| k.id))
}

/// `~/.we2ai/key_selection.json`：`"<区域>:<用户 id>" → key_id`。只是界面
/// 便利性记忆，读失败按"没有记忆"处理。
struct KeySelectionFile;

#[derive(Debug, Default, Serialize, Deserialize)]
struct KeySelectionData {
    #[serde(default)]
    selections: HashMap<String, i64>,
}

impl KeySelectionFile {
    fn path(data_root: &Path) -> PathBuf {
        data_root.join("key_selection.json")
    }

    fn slot(identity: &SessionIdentity) -> String {
        format!("{}:{}", identity.region.storage_key(), identity.user_id)
    }

    fn read(data_root: &Path) -> KeySelectionData {
        std::fs::read_to_string(Self::path(data_root))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn load(data_root: &Path, identity: &SessionIdentity) -> Option<i64> {
        Self::read(data_root)
            .selections
            .get(&Self::slot(identity))
            .copied()
    }

    fn save(
        data_root: &Path,
        identity: &SessionIdentity,
        key_id: i64,
    ) -> Result<(), crate::error::AppError> {
        let mut data = Self::read(data_root);
        data.selections.insert(Self::slot(identity), key_id);
        let json = serde_json::to_string_pretty(&data).unwrap_or_default();
        crate::config::atomic_write_private(&Self::path(data_root), json.as_bytes())
    }
}

/// 分页拉取全部 Key。每页都经过 `call_protected_api`（续期、身份复查）；
/// 各页身份必须一致，中途换了会话按 `SessionChanged` 失败。
async fn fetch_all_keys(
    manager: &SessionManager,
) -> Result<(SessionIdentity, Vec<RemoteApiKey>), SessionError> {
    fetch_all_keys_with_limit(manager, KEYS_MAX_PAGES).await
}

/// 超过页数上限仍未到最后一页时返回错误，而不是交出不完整的列表
/// （Codex P3 验收第 1 轮高危项）。
pub const KEY_LIST_TOO_LARGE: &str = "KEY_LIST_TOO_LARGE";

/// 本次列表拉取被更新的一次拉取取代（前端只采纳最后一次请求，忽略即可）。
pub const KEY_LIST_SUPERSEDED: &str = "KEY_LIST_SUPERSEDED";

async fn fetch_all_keys_with_limit(
    manager: &SessionManager,
    max_pages: u32,
) -> Result<(SessionIdentity, Vec<RemoteApiKey>), SessionError> {
    let mut identity: Option<SessionIdentity> = None;
    let mut all = Vec::new();
    let mut page = 1u32;
    loop {
        let (data, page_identity) = manager
            .call_protected_api(true, move |api, token| async move {
                api.list_keys(&token, page, KEYS_PAGE_SIZE).await
            })
            .await?;
        match identity {
            None => identity = Some(page_identity),
            Some(first) if first != page_identity => return Err(SessionError::SessionChanged),
            Some(_) => {}
        }
        let empty = data.items.is_empty();
        all.extend(data.items);
        if empty || i64::from(page) >= data.pages {
            break;
        }
        if page >= max_pages {
            return Err(SessionError::Other(KEY_LIST_TOO_LARGE.to_string()));
        }
        page += 1;
    }
    let identity = identity.expect("at least one page fetched");
    Ok((identity, all))
}

pub async fn list_keys(
    manager: &SessionManager,
    state: &We2aiKeyState,
) -> Result<KeyListView, SessionError> {
    let seq = state.begin_fetch();
    let (identity, all) = fetch_all_keys(manager).await?;
    let visible: Vec<RemoteApiKey> = all
        .into_iter()
        .filter(|k| VISIBLE_STATUSES.contains(&k.status.as_str()))
        .collect();
    let views: Vec<KeyView> = visible.iter().map(to_view).collect();
    let remembered = KeySelectionFile::load(manager.data_root(), &identity);
    let selected_key_id = choose_selected(&views, remembered);
    if !state.store(seq, identity, visible) {
        // 更新的一次拉取已经写入缓存：这次结果已过期，不能交给前端展示，
        // 否则界面显示的 Key 与缓存不一致。
        return Err(SessionError::Other(KEY_LIST_SUPERSEDED.to_string()));
    }
    Ok(KeyListView {
        keys: views,
        selected_key_id,
    })
}

fn unknown_key_error() -> We2aiApiError {
    We2aiApiError {
        code: "KEY_NOT_FOUND".to_string(),
        message: "该 Key 不在当前账号的 Key 列表中，请刷新列表".to_string(),
    }
}

#[tauri::command]
pub async fn we2ai_list_keys(
    session: State<'_, We2aiSessionState>,
    keys: State<'_, We2aiKeyState>,
) -> Result<KeyListView, We2aiApiError> {
    let manager = session.0.clone();
    Ok(list_keys(&manager, &keys).await?)
}

#[tauri::command]
pub async fn we2ai_select_key(
    session: State<'_, We2aiSessionState>,
    keys: State<'_, We2aiKeyState>,
    key_id: i64,
) -> Result<(), We2aiApiError> {
    let manager = session.0.clone();
    select_key(&manager, &keys, key_id)
}

pub fn select_key(
    manager: &SessionManager,
    state: &We2aiKeyState,
    key_id: i64,
) -> Result<(), We2aiApiError> {
    let identity = manager.current_identity();
    if !state.contains(identity, key_id) {
        return Err(unknown_key_error());
    }
    let identity = identity.expect("contains() implies an identity");
    KeySelectionFile::save(manager.data_root(), &identity, key_id).map_err(|e| We2aiApiError {
        code: "KEY_SELECTION_SAVE_FAILED".to_string(),
        message: e.to_string(),
    })
}

pub async fn key_models(
    manager: &SessionManager,
    state: &We2aiKeyState,
    key_id: i64,
) -> Result<KeyModelsView, We2aiApiError> {
    let identity = manager.current_identity();
    if !state.contains(identity, key_id) {
        return Err(unknown_key_error());
    }
    let (remote, call_identity) = manager
        .call_protected_api(true, move |api, token| async move {
            api.get_key_models(&token, key_id).await
        })
        .await?;
    // 校验 Key 归属时的会话与实际发请求的会话必须是同一个。
    if Some(call_identity) != identity {
        return Err(SessionError::SessionChanged.into());
    }
    Ok(to_models_view(remote))
}

#[tauri::command]
pub async fn we2ai_key_models(
    session: State<'_, We2aiSessionState>,
    keys: State<'_, We2aiKeyState>,
    key_id: i64,
) -> Result<KeyModelsView, We2aiApiError> {
    let manager = session.0.clone();
    key_models(&manager, &keys, key_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::we2ai::region::Region;
    use crate::we2ai::secret_store::test_support::InMemorySecretStore;
    use serde_json::{json, Value};
    use std::sync::Arc;
    use tempfile::TempDir;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SECRET_A: &str = "sk-we2ai-aaaaaaaaaaaaaaaaaaaa1111";
    const SECRET_B: &str = "sk-we2ai-bbbbbbbbbbbbbbbbbbbb2222";
    const SECRET_C: &str = "sk-we2ai-cccccccccccccccccccc3333";

    fn manager(dir: &TempDir) -> SessionManager {
        SessionManager::new(
            Arc::new(InMemorySecretStore::new()),
            dir.path().to_path_buf(),
            "test".to_string(),
        )
    }

    fn key_json(id: i64, key: &str, status: &str, group: Option<&str>) -> Value {
        json!({
            "id": id, "user_id": 42, "key": key, "name": format!("key-{id}"),
            "group_id": group.map(|_| id * 10), "status": status,
            "group": group.map(|g| json!({"id": id * 10, "name": g, "platform": "anthropic"})),
        })
    }

    async fn mount_page(server: &MockServer, page: u32, pages: i64, items: Vec<Value>) {
        Mock::given(method("GET"))
            .and(path("/api/v1/keys"))
            .and(query_param("page", page.to_string()))
            .and(query_param("page_size", KEYS_PAGE_SIZE.to_string()))
            .and(header("authorization", "Bearer access-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"items": items, "total": 3, "page": page, "page_size": KEYS_PAGE_SIZE, "pages": pages}
            })))
            .expect(1)
            .mount(server)
            .await;
    }

    /// 三页 Key：逐页拉到最后一页，只保留 active 与 quota_exhausted，视图
    /// 与序列化结果都不含明文，明文只留在缓存里。
    #[tokio::test]
    async fn list_keys_pages_to_the_end_filters_status_and_never_exposes_secrets() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        mount_page(
            &server,
            1,
            3,
            vec![key_json(1, SECRET_A, "active", Some("Claude 组"))],
        )
        .await;
        mount_page(&server, 2, 3, vec![key_json(2, SECRET_B, "disabled", None)]).await;
        mount_page(
            &server,
            3,
            3,
            vec![key_json(3, SECRET_C, "quota_exhausted", None)],
        )
        .await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        let state = We2aiKeyState::default();

        let view = list_keys(&manager, &state).await.unwrap();

        assert_eq!(
            view.keys.iter().map(|k| k.id).collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(view.keys[0].group_name.as_deref(), Some("Claude 组"));
        assert_eq!(view.keys[1].status, "quota_exhausted");
        assert_eq!(view.selected_key_id, Some(1));
        let serialized = serde_json::to_string(&view).unwrap();
        for secret in [SECRET_A, SECRET_B, SECRET_C] {
            assert!(
                !serialized.contains(secret),
                "plaintext key leaked: {serialized}"
            );
        }
        assert_eq!(view.keys[0].masked_key, "sk-we2…1111");

        let identity = manager.current_identity();
        assert_eq!(state.secret_for(identity, 1).as_deref(), Some(SECRET_A));
        assert_eq!(
            state.secret_for(identity, 2),
            None,
            "filtered key must not be cached"
        );
    }

    /// 记住的选择仍存在则沿用；该 Key 消失后回退到第一个。记忆按区域 + 用户
    /// 分开，另一个账号看不到这份记忆。
    #[tokio::test]
    async fn remembered_selection_is_used_per_account_and_falls_back_when_the_key_is_gone() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"items": [key_json(1, SECRET_A, "active", None), key_json(3, SECRET_C, "active", None)],
                         "total": 2, "page": 1, "page_size": 100, "pages": 1}
            })))
            .mount(&server)
            .await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        let state = We2aiKeyState::default();
        list_keys(&manager, &state).await.unwrap();

        select_key(&manager, &state, 3).unwrap();
        assert_eq!(
            list_keys(&manager, &state).await.unwrap().selected_key_id,
            Some(3)
        );

        // 不在列表里的 Key 不能被选中，也不发任何请求。
        let err = select_key(&manager, &state, 99).unwrap_err();
        assert_eq!(err.code, "KEY_NOT_FOUND");

        // 另一个账号（同区域）没有记忆，取第一个。
        manager.test_seed_active(Region::International, 7, "access-1", server.uri());
        assert_eq!(
            list_keys(&manager, &state).await.unwrap().selected_key_id,
            Some(1)
        );

        // 记住的 Key 被删除后回退到第一个。
        let mut data = KeySelectionFile::read(dir.path());
        data.selections.insert("international:7".to_string(), 12345);
        std::fs::write(
            KeySelectionFile::path(dir.path()),
            serde_json::to_string(&data).unwrap(),
        )
        .unwrap();
        assert_eq!(
            list_keys(&manager, &state).await.unwrap().selected_key_id,
            Some(1)
        );
    }

    /// 会话身份变化（换账号、重新登录、登出）后缓存里的明文不再可取，并被清空。
    #[tokio::test]
    async fn cached_secrets_are_dropped_once_the_session_identity_changes() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        mount_page(&server, 1, 1, vec![key_json(1, SECRET_A, "active", None)]).await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        let state = We2aiKeyState::default();
        list_keys(&manager, &state).await.unwrap();
        let old = manager.current_identity();
        assert!(state.secret_for(old, 1).is_some());

        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        assert_eq!(state.secret_for(manager.current_identity(), 1), None);
        // 已被清空：即使拿旧身份来取也没有了。
        assert_eq!(state.secret_for(old, 1), None);
    }

    /// B1：只保留客户端认识的工具，可调用时不带原因，不可调用时透传原因；
    /// 不在 Key 列表中的 id 直接拒绝、不发请求。
    #[tokio::test]
    async fn key_models_keeps_known_tools_and_reports_admission() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        mount_page(
            &server,
            1,
            1,
            vec![
                key_json(1, SECRET_A, "active", None),
                key_json(3, SECRET_C, "quota_exhausted", None),
            ],
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/desktop/keys/1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"models": [
                    {"id": "claude-sonnet-4-5", "provider": "anthropic", "tools": ["workbuddy", "claude_code", "gemini_cli"]},
                    {"id": "gpt-5", "tools": []}
                ], "callable": true, "blocked_reason": "IGNORED"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/desktop/keys/3/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"models": [{"id": "gpt-5", "tools": ["codex"]}],
                         "callable": false, "blocked_reason": "API_KEY_QUOTA_EXHAUSTED"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        let state = We2aiKeyState::default();
        list_keys(&manager, &state).await.unwrap();

        let ok = key_models(&manager, &state, 1).await.unwrap();
        assert!(ok.callable);
        assert_eq!(ok.blocked_reason, None);
        assert_eq!(ok.models[0].tools, vec!["claude_code", "workbuddy"]);
        assert_eq!(ok.models[0].provider.as_deref(), Some("anthropic"));
        assert!(ok.models[1].tools.is_empty());

        let blocked = key_models(&manager, &state, 3).await.unwrap();
        assert!(!blocked.callable);
        assert_eq!(
            blocked.blocked_reason.as_deref(),
            Some("API_KEY_QUOTA_EXHAUSTED")
        );
        assert_eq!(blocked.models[0].tools, vec!["codex"]);

        let err = key_models(&manager, &state, 2).await.unwrap_err();
        assert_eq!(err.code, "KEY_NOT_FOUND");
    }

    /// 同一会话两次列表拉取乱序完成：较早发起、较晚完成的结果不覆盖较新
    /// 缓存，随后选择较新列表里的 Key 成功。
    #[tokio::test]
    async fn an_older_list_fetch_finishing_last_does_not_overwrite_the_newer_cache() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        let page = |key: Value| {
            json!({"code": 0, "message": "success",
                   "data": {"items": [key], "total": 1, "page": 1, "page_size": 100, "pages": 1}})
        };
        Mock::given(method("GET"))
            .and(path("/api/v1/keys"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(page(key_json(1, SECRET_A, "active", None)))
                    .set_delay(std::time::Duration::from_millis(600)),
            )
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/keys"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(page(key_json(3, SECRET_C, "active", None))),
            )
            .with_priority(2)
            .mount(&server)
            .await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        let state = Arc::new(We2aiKeyState::default());

        let (m, s) = (manager.clone(), state.clone());
        let older = tokio::spawn(async move { list_keys(&m, &s).await });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let newer = list_keys(&manager, &state).await.unwrap();
        assert_eq!(newer.keys[0].id, 3);

        let older_result = older.await.unwrap();
        assert_eq!(
            older_result.unwrap_err(),
            SessionError::Other(KEY_LIST_SUPERSEDED.to_string())
        );
        select_key(&manager, &state, 3).unwrap();
        assert_eq!(
            select_key(&manager, &state, 1).unwrap_err().code,
            "KEY_NOT_FOUND"
        );
    }

    /// 已到页数上限但服务端还有下一页：报错，不缓存、不交出不完整列表。
    #[tokio::test]
    async fn paging_past_the_page_limit_fails_instead_of_truncating() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        mount_page(&server, 1, 3, vec![key_json(1, SECRET_A, "active", None)]).await;
        mount_page(&server, 2, 3, vec![key_json(2, SECRET_B, "active", None)]).await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        let state = We2aiKeyState::default();

        let err = fetch_all_keys_with_limit(&manager, 2).await.unwrap_err();
        assert_eq!(err, SessionError::Other(KEY_LIST_TOO_LARGE.to_string()));
        assert_eq!(state.secret_for(manager.current_identity(), 1), None);
    }

    /// 分页途中切换了会话（重新登录），整次拉取失败，不把两个会话的 Key
    /// 拼在一起，也不写缓存。
    #[tokio::test]
    async fn session_switch_during_paging_fails_without_mixing_accounts() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        mount_page(&server, 1, 2, vec![key_json(1, SECRET_A, "active", None)]).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/keys"))
            .and(query_param("page", "2"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({
                        "code": 0, "message": "success",
                        "data": {"items": [key_json(2, SECRET_B, "active", None)],
                                 "total": 2, "page": 2, "page_size": 100, "pages": 2}
                    }))
                    .set_delay(std::time::Duration::from_millis(800)),
            )
            .mount(&server)
            .await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        let state = Arc::new(We2aiKeyState::default());

        let task_manager = manager.clone();
        let task_state = state.clone();
        let task = tokio::spawn(async move { list_keys(&task_manager, &task_state).await });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        manager.test_seed_active(Region::International, 7, "access-1", server.uri());

        let err = task.await.unwrap().unwrap_err();
        assert_eq!(err, SessionError::SessionChanged);
        assert_eq!(state.secret_for(manager.current_identity(), 1), None);
    }

    #[tokio::test]
    async fn list_keys_requires_an_active_session() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let state = We2aiKeyState::default();
        let err = list_keys(&manager, &state).await.unwrap_err();
        assert_eq!(err, SessionError::NoActiveSession);
    }

    #[test]
    fn mask_key_keeps_head_and_tail_only() {
        assert_eq!(mask_key("sk-1234567890abcdef"), "sk-123…cdef");
        assert_eq!(mask_key("short"), "sh…");
        assert_eq!(mask_key(""), "…");
    }

    #[test]
    fn remote_api_key_debug_redacts_the_secret() {
        let key: RemoteApiKey =
            serde_json::from_value(key_json(1, SECRET_A, "active", None)).unwrap();
        assert!(!format!("{key:?}").contains(SECRET_A));
    }
}
