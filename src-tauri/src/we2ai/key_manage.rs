//! Key 管理：列表、创建、编辑、删除、启停、复制（功能 21，设计：
//! `docs/we2ai/充值与Key管理设计方案.md` 第 2 节）。
//!
//! - 全部请求经 `call_protected_api`（续期、401 终止会话与 `keys.rs` 一致）。
//! - **明文只在 Rust**：列表命令返回的 [`KeyManageView`] 只有掩码；列表里的明文
//!   存进 [`We2aiKeyState`] 的管理页缓存（按会话身份隔离，登出/换身份清空，与模型
//!   广场缓存分开，不改变后者只列 `active`/`quota_exhausted` 的语义）。前端只有一处
//!   能拿到明文：创建成功那一次（[`CreatedKeyView::plaintext`]，只用于「Key 已创建」
//!   卡片展示）。「复制」由 Rust 侧完成：`we2ai_copy_key` 从缓存取明文直接写系统
//!   剪贴板，明文不经 IPC。
//! - 任一写操作成功后：失效模型广场 Key 缓存（[`We2aiKeyState::invalidate`]）并发
//!   `we2ai-keys-changed` 事件，让 Key 管理页与模型广场重新拉取。
//! - 字段名对照 SubPanel `handler/api_key_handler.go`（`CreateAPIKeyRequest`、
//!   `UpdateAPIKeyRequest`）与 `handler/dto/types.go`（`APIKey`、`Group`）。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tauri::State;

use super::api::{RemoteGroupOption, RemoteManagedKey};
use super::commands_auth::We2aiApiError;
use super::keys::{
    clear_selection_if, fetch_all_pages, mask_key, unknown_key_error, We2aiKeyState,
    KEYS_MAX_PAGES, KEYS_PAGE_SIZE, KEY_LIST_SUPERSEDED,
};
use super::session::{SessionError, SessionIdentity, SessionManager, We2aiSessionState};

/// Key 写操作成功后发给前端的刷新事件。Rust 与 `src/we2ai/api.ts` 各写一份字面量，
/// 由 `scripts/we2ai/check-guards.sh` 4.15 校验两侧一致。
pub const EVENT_KEYS_CHANGED: &str = "we2ai-keys-changed";

/// 写操作成功后的通知回调。要求 `Sync`：命令的 future 必须是 `Send`，持有
/// `&dyn Fn()` 跨 `await` 时 trait object 本身得是 `Sync`。
type Notify<'a> = &'a (dyn Fn() + Sync);

/// 名称长度上限：SubPanel `ent/schema/api_key.go` 的 `name` 为 `MaxLen(100)`，按
/// UTF-8 **字节**计，且校验发生在 `html.EscapeString` 之后（创建/更新都会转义）。
/// 所以这里按「转义后的字节数 ≤ 100」放行（约 33 个汉字；`&` 占 5 字节）。
const NAME_MAX_ESCAPED_BYTES: usize = 100;
/// 创建时有效期天数上限（SubPanel 只要求 > 0，这里防止前端误传天文数字）。
const EXPIRES_DAYS_MAX: i64 = 36_500;
/// 服务端返回「Key 不存在」的业务码（`service.ErrAPIKeyNotFound`）。
const API_KEY_NOT_FOUND: &str = "API_KEY_NOT_FOUND";
/// 写系统剪贴板失败的错误码。
const CLIPBOARD_FAILED: &str = "CLIPBOARD_FAILED";

// ---------------------------------------------------------------------------
// 视图
// ---------------------------------------------------------------------------

/// Key 所属分组与实际倍率（用户专属倍率优先，其次分组默认倍率，都无效时 1）。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KeyGroupView {
    pub id: i64,
    pub name: String,
    pub rate: f64,
}

/// Key 管理页的行视图。**不含明文**：字段清单由 `check-guards.sh` 4.15 锁定，
/// 禁止出现 `key`/`secret`/`plaintext` 字段。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KeyManageView {
    pub id: i64,
    pub name: String,
    /// `active` | `inactive` | `quota_exhausted` | `expired`。
    pub status: String,
    pub masked_key: String,
    pub group: Option<KeyGroupView>,
    /// 额度上限（美元，0 = 不限）。
    pub quota: f64,
    pub quota_used: f64,
    /// RFC3339；`None` = 永不过期。
    pub expires_at: Option<String>,
    pub last_used_at: Option<String>,
}

/// 创建结果：行视图 + 仅此一次交给前端的明文（「Key 已创建」卡片）。
/// 手写 `Debug` 隐去明文。
#[derive(Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CreatedKeyView {
    pub key: KeyManageView,
    pub plaintext: String,
}

impl std::fmt::Debug for CreatedKeyView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreatedKeyView")
            .field("key", &self.key)
            .field("plaintext", &"<redacted>")
            .finish()
    }
}

/// 新建/编辑弹窗里分组下拉的一项。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KeyGroupOptionView {
    pub id: i64,
    pub name: String,
    pub platform: String,
    pub rate: f64,
}

// ---------------------------------------------------------------------------
// 输入
// ---------------------------------------------------------------------------

/// `we2ai_create_key` 的输入。`idempotency_key` 由前端每次打开弹窗生成一个 UUID，
/// 同一次弹窗内的重试复用它，服务端据此去重。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateKeyInput {
    pub idempotency_key: String,
    pub name: String,
    #[serde(default)]
    pub group_id: Option<i64>,
    /// 美元，0 或缺省 = 不限。
    #[serde(default)]
    pub quota: Option<f64>,
    /// 缺省 = 永久。
    #[serde(default)]
    pub expires_in_days: Option<i64>,
}

/// `we2ai_update_key` 的输入：缺省的字段不修改。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateKeyInput {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub group_id: Option<i64>,
    #[serde(default)]
    pub quota: Option<f64>,
    /// RFC3339 设置过期时间；空字符串清除（永不过期）；缺省不修改。
    #[serde(default)]
    pub expires_at: Option<String>,
    /// `active` | `inactive`。
    #[serde(default)]
    pub status: Option<String>,
    /// `true` 时把已用额度清零。
    #[serde(default)]
    pub reset_quota: Option<bool>,
}

fn invalid(code: &str, message: &str) -> We2aiApiError {
    We2aiApiError {
        code: code.to_string(),
        message: message.to_string(),
    }
}

/// Go `html.EscapeString` 之后的 UTF-8 字节数：`&`→`&amp;`(5)、`<`→`&lt;`(4)、
/// `>`→`&gt;`(4)、`"`→`&#34;`(5)、`'`→`&#39;`(5)，其余字符按自身 UTF-8 长度。
fn escaped_name_bytes(name: &str) -> usize {
    name.chars()
        .map(|c| match c {
            '&' | '"' | '\'' => 5,
            '<' | '>' => 4,
            other => other.len_utf8(),
        })
        .sum()
}

fn validate_name(name: &str) -> Result<String, We2aiApiError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || escaped_name_bytes(trimmed) > NAME_MAX_ESCAPED_BYTES {
        return Err(invalid(
            "KEY_NAME_INVALID",
            "Key 名称不能为空，且不超过 100 字节（约 33 个汉字）",
        ));
    }
    Ok(trimmed.to_string())
}

fn validate_quota(quota: f64) -> Result<f64, We2aiApiError> {
    if !quota.is_finite() || quota < 0.0 {
        return Err(invalid(
            "KEY_QUOTA_INVALID",
            "额度上限必须是不小于 0 的数字",
        ));
    }
    Ok(quota)
}

fn validate_group_id(id: i64) -> Result<i64, We2aiApiError> {
    if id <= 0 {
        return Err(invalid("KEY_GROUP_INVALID", "分组无效"));
    }
    Ok(id)
}

/// 幂等键必须是带连字符的标准 UUID（36 字符），满足服务端「≤128 字符的可见
/// ASCII」要求且不会被前端随手传入奇怪的值。
fn validate_idempotency_key(key: &str) -> Result<String, We2aiApiError> {
    if key.len() == 36 && uuid::Uuid::parse_str(key).is_ok() {
        Ok(key.to_string())
    } else {
        Err(invalid("IDEMPOTENCY_KEY_INVALID", "幂等键格式无效"))
    }
}

/// 校验并构造 `POST /keys` 请求体（`CreateAPIKeyRequest`：`name` 必填，
/// `group_id`、`quota`、`expires_in_days` 可选）。
fn build_create_body(input: &CreateKeyInput) -> Result<(String, Value), We2aiApiError> {
    let idempotency_key = validate_idempotency_key(&input.idempotency_key)?;
    let mut body = Map::new();
    body.insert("name".into(), Value::String(validate_name(&input.name)?));
    if let Some(group_id) = input.group_id {
        body.insert("group_id".into(), json!(validate_group_id(group_id)?));
    }
    if let Some(quota) = input.quota {
        body.insert("quota".into(), json!(validate_quota(quota)?));
    }
    if let Some(days) = input.expires_in_days {
        if !(1..=EXPIRES_DAYS_MAX).contains(&days) {
            return Err(invalid("KEY_EXPIRY_INVALID", "有效期天数无效"));
        }
        body.insert("expires_in_days".into(), json!(days));
    }
    Ok((idempotency_key, Value::Object(body)))
}

/// 校验并构造 `PUT /keys/:id` 请求体（`UpdateAPIKeyRequest`）。
fn build_update_body(input: &UpdateKeyInput) -> Result<Value, We2aiApiError> {
    let mut body = Map::new();
    if let Some(name) = &input.name {
        body.insert("name".into(), Value::String(validate_name(name)?));
    }
    if let Some(group_id) = input.group_id {
        body.insert("group_id".into(), json!(validate_group_id(group_id)?));
    }
    if let Some(quota) = input.quota {
        body.insert("quota".into(), json!(validate_quota(quota)?));
    }
    if let Some(expires_at) = &input.expires_at {
        if !expires_at.is_empty() && chrono::DateTime::parse_from_rfc3339(expires_at).is_err() {
            return Err(invalid("KEY_EXPIRY_INVALID", "过期时间格式无效"));
        }
        // 空字符串原样发送：服务端据此清除过期时间。
        body.insert("expires_at".into(), Value::String(expires_at.clone()));
    }
    if let Some(status) = &input.status {
        if status != "active" && status != "inactive" {
            return Err(invalid("KEY_STATUS_INVALID", "状态只能是启用或禁用"));
        }
        body.insert("status".into(), Value::String(status.clone()));
    }
    if let Some(reset) = input.reset_quota {
        body.insert("reset_quota".into(), Value::Bool(reset));
    }
    Ok(Value::Object(body))
}

// ---------------------------------------------------------------------------
// 远端数据 → 视图
// ---------------------------------------------------------------------------

/// 服务端状态归一化：`active`/`quota_exhausted`/`expired` 原样，其余（`inactive`、
/// `disabled` 以及未知值）一律视为已禁用——未知状态不能当作可用。
fn normalize_status(status: &str) -> String {
    match status {
        "active" | "quota_exhausted" | "expired" => status.to_string(),
        _ => "inactive".to_string(),
    }
}

/// SubPanel 创建/更新时对名称做了 `html.EscapeString`（`&`→`&amp;`、`<`→`&lt;`、
/// `>`→`&gt;`、`"`→`&#34;`、`'`→`&#39;`）。客户端展示与「输入名称确认删除」
/// 都要用原文，所以还原这五种实体；`&amp;` 必须最后还原，避免二次还原。
fn unescape_name(name: &str) -> String {
    name.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&#34;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

fn finite_positive(v: Option<f64>) -> Option<f64> {
    v.filter(|x| x.is_finite() && *x > 0.0)
}

fn finite_nonnegative(v: Option<f64>) -> f64 {
    v.filter(|x| x.is_finite() && *x >= 0.0).unwrap_or(0.0)
}

fn non_empty(v: &Option<String>) -> Option<String> {
    v.as_ref().filter(|s| !s.is_empty()).cloned()
}

/// 分组实际倍率：用户专属倍率 > 分组默认倍率 > 1。
fn group_rate(user_rates: &HashMap<String, f64>, id: i64, default: Option<f64>) -> f64 {
    finite_positive(user_rates.get(&id.to_string()).copied())
        .or_else(|| finite_positive(default))
        .unwrap_or(1.0)
}

fn to_manage_view(key: &RemoteManagedKey, user_rates: &HashMap<String, f64>) -> KeyManageView {
    KeyManageView {
        id: key.id,
        name: unescape_name(&key.name),
        status: normalize_status(&key.status),
        masked_key: mask_key(&key.key),
        group: key.group.as_ref().map(|g| KeyGroupView {
            id: g.id,
            name: g.name.clone(),
            rate: group_rate(user_rates, g.id, g.rate_multiplier),
        }),
        quota: finite_nonnegative(key.quota),
        quota_used: finite_nonnegative(key.quota_used),
        expires_at: non_empty(&key.expires_at),
        last_used_at: non_empty(&key.last_used_at),
    }
}

fn to_group_option(
    group: &RemoteGroupOption,
    user_rates: &HashMap<String, f64>,
) -> KeyGroupOptionView {
    KeyGroupOptionView {
        id: group.id,
        name: group.name.clone(),
        platform: group.platform.clone(),
        rate: group_rate(user_rates, group.id, group.rate_multiplier),
    }
}

/// 用户专属分组倍率：尽力而为。拿不到（网络抖动、接口异常）时退回分组默认倍率，
/// 不让一个辅助接口拖垮整个列表；但会话已失效/已切换必须照常上抛。
async fn fetch_user_rates(
    manager: &SessionManager,
    identity: SessionIdentity,
) -> Result<HashMap<String, f64>, SessionError> {
    // 辅助接口，不自动重试：瞬时失败直接退回默认倍率，不拖慢列表。
    let result = manager
        .call_protected_api(false, move |api, token| async move {
            api.list_group_rates(&token).await
        })
        .await;
    match result {
        Ok((rates, call_identity)) => {
            if call_identity != identity {
                return Err(SessionError::SessionChanged);
            }
            Ok(rates.unwrap_or_default())
        }
        Err(
            e @ (SessionError::Terminated(_)
            | SessionError::SessionChanged
            | SessionError::NoActiveSession
            | SessionError::NeedsRelogin),
        ) => Err(e),
        Err(e) => {
            log::debug!("读取专属分组倍率失败，改用分组默认倍率: {e}");
            Ok(HashMap::new())
        }
    }
}

// ---------------------------------------------------------------------------
// 业务函数
// ---------------------------------------------------------------------------

pub async fn manage_list_keys(
    manager: &SessionManager,
    state: &We2aiKeyState,
) -> Result<Vec<KeyManageView>, SessionError> {
    let seq = state.begin_manage_fetch();
    let (identity, all) = fetch_all_pages(manager, KEYS_MAX_PAGES, |api, token, page| async move {
        api.list_managed_keys(&token, page, KEYS_PAGE_SIZE).await
    })
    .await?;
    let rates = fetch_user_rates(manager, identity).await?;
    let views: Vec<KeyManageView> = all.iter().map(|k| to_manage_view(k, &rates)).collect();
    let secrets: HashMap<i64, String> = all.into_iter().map(|k| (k.id, k.key)).collect();
    if !state.store_manage(seq, identity, secrets) {
        // 更新的一次拉取（或写操作后的失效）已经取代了这次结果：不能交给前端，
        // 否则界面显示的列表与缓存里的明文对不上。
        return Err(SessionError::Other(KEY_LIST_SUPERSEDED.to_string()));
    }
    Ok(views)
}

pub async fn list_key_groups(
    manager: &SessionManager,
) -> Result<Vec<KeyGroupOptionView>, SessionError> {
    let (groups, identity) = manager
        .call_protected_api(true, move |api, token| async move {
            api.list_available_groups(&token).await
        })
        .await?;
    let rates = fetch_user_rates(manager, identity).await?;
    Ok(groups.iter().map(|g| to_group_option(g, &rates)).collect())
}

/// 写操作成功后的统一收尾：失效模型广场 Key 缓存并通知前端刷新。
///
/// 只对**发起请求时的会话身份**生效：响应返回时会话（账号/区域/代次）已变，则
/// 不失效、不发事件——否则会把新身份的缓存清掉，或让新身份的界面为旧身份的写
/// 操作白白重拉。返回是否执行了收尾。（`call_protected_api` 在成功响应返回前已经
/// 复查过身份，这里再校验一次覆盖「复查之后、收尾之前」的极小窗口。）
fn after_write(
    manager: &SessionManager,
    identity: SessionIdentity,
    state: &We2aiKeyState,
    notify: Notify<'_>,
) -> bool {
    if manager.current_identity() != Some(identity) {
        return false;
    }
    state.invalidate();
    notify();
    true
}

pub async fn create_key(
    manager: &SessionManager,
    state: &We2aiKeyState,
    input: CreateKeyInput,
    notify: Notify<'_>,
) -> Result<CreatedKeyView, We2aiApiError> {
    let (idempotency_key, body) = build_create_body(&input)?;
    // 带幂等键，网络抖动时自动重放同一个请求是安全的（服务端返回首次结果）。
    let (remote, identity) = manager
        .call_protected_api(true, move |api, token| {
            let idempotency_key = idempotency_key.clone();
            let body = body.clone();
            async move { api.create_key(&token, &idempotency_key, &body).await }
        })
        .await?;
    // 失效之后再并入新 Key 的明文：「Key 已创建」卡片的复制走 `copy_key`，不必等
    // 列表重拉（重拉会整体覆盖这份缓存，新 Key 已在其中）。会话已变则不碰缓存：
    // `insert_manage_secret` 对不同身份会重建缓存，会冲掉新身份的内容。
    if after_write(manager, identity, state, notify) && !remote.key.is_empty() {
        state.insert_manage_secret(identity, remote.id, remote.key.clone());
    }
    Ok(CreatedKeyView {
        key: to_manage_view(&remote, &HashMap::new()),
        plaintext: remote.key,
    })
}

pub async fn update_key(
    manager: &SessionManager,
    state: &We2aiKeyState,
    key_id: i64,
    input: UpdateKeyInput,
    notify: Notify<'_>,
) -> Result<KeyManageView, We2aiApiError> {
    let body = build_update_body(&input)?;
    // 「重置已用额度」不是幂等操作：SubPanel 每次执行都把 `quota_used` 清零
    // （`service/api_key_service.go:869`），`PUT` 也没有幂等去重
    // （`handler/api_key_handler.go:288`）。首次已生效但响应丢失时自动重放会二次清
    // 零中间产生的用量，所以含 `reset_quota=true` 的更新不自动重放瞬时错误
    // （`idempotent=false`；401 续期后的重放不受影响，那种情况请求并未被处理）。
    // 其余字段（名称、分组、额度上限、过期、状态）都是绝对值赋值，可重放。
    let replayable = input.reset_quota != Some(true);
    let (remote, identity) = manager
        .call_protected_api(replayable, move |api, token| {
            let body = body.clone();
            async move { api.update_key(&token, key_id, &body).await }
        })
        .await?;
    after_write(manager, identity, state, notify);
    Ok(to_manage_view(&remote, &HashMap::new()))
}

/// 删除 Key；删除的是 `key_selection.json` 里的当前选择时一并清掉。服务端回
/// 「不存在」按成功处理（常见于上一次请求其实已生效、响应丢失后的重试）。
///
/// 结果绑定**发起请求时的会话身份**：请求在途期间账号/区域/代次变了，响应（无论
/// 成功还是「不存在」）都不能作用于新身份——不清任何选择记忆、不失效缓存、不发事件，
/// 返回 `SESSION_CHANGED`。选择记忆只清发起身份自己的槽位。
pub async fn delete_key(
    manager: &SessionManager,
    state: &We2aiKeyState,
    key_id: i64,
    notify: Notify<'_>,
) -> Result<(), We2aiApiError> {
    let initiating = manager.current_identity();
    let result = manager
        .call_protected_api(true, move |api, token| async move {
            api.delete_key(&token, key_id).await
        })
        .await;
    let identity = match result {
        Ok(((), identity)) => identity,
        // 「不存在」是普通业务错误，`call_protected_api` 不会为它复查身份：这里必须
        // 自己确认会话仍是发起时的那一个，否则旧身份的 404 会被当成对新身份的成功。
        Err(SessionError::Other(code)) if code == API_KEY_NOT_FOUND => match initiating {
            Some(identity) if manager.current_identity() == Some(identity) => identity,
            _ => return Err(SessionError::SessionChanged.into()),
        },
        Err(e) => return Err(e.into()),
    };
    clear_selection_if(manager.data_root(), &identity, key_id);
    after_write(manager, identity, state, notify);
    Ok(())
}

/// 复制：从管理页缓存取明文并交给 `write` 写系统剪贴板，明文不离开 Rust。
/// 不在缓存里（未拉过列表、列表里没有、身份已变、写操作后缓存已失效）一律
/// `KEY_NOT_FOUND`，此时不会调用 `write`；写剪贴板失败映射为 `CLIPBOARD_FAILED`
/// （错误文案不含明文）。`write` 可注入，测试里不真写剪贴板。
pub async fn copy_key<W, Fut>(
    manager: &SessionManager,
    state: &We2aiKeyState,
    key_id: i64,
    write: W,
) -> Result<(), We2aiApiError>
where
    W: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let plaintext = state
        .manage_secret_for(manager.current_identity(), key_id)
        .ok_or_else(unknown_key_error)?;
    write(plaintext).await.map_err(|message| We2aiApiError {
        code: CLIPBOARD_FAILED.to_string(),
        message,
    })
}

// ---------------------------------------------------------------------------
// Tauri 命令
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn we2ai_manage_list_keys(
    session: State<'_, We2aiSessionState>,
    keys: State<'_, We2aiKeyState>,
) -> Result<Vec<KeyManageView>, We2aiApiError> {
    let manager = session.0.clone();
    Ok(manage_list_keys(&manager, &keys).await?)
}

#[tauri::command]
pub async fn we2ai_list_key_groups(
    session: State<'_, We2aiSessionState>,
) -> Result<Vec<KeyGroupOptionView>, We2aiApiError> {
    let manager = session.0.clone();
    Ok(list_key_groups(&manager).await?)
}

#[tauri::command]
pub async fn we2ai_create_key(
    app: tauri::AppHandle,
    session: State<'_, We2aiSessionState>,
    keys: State<'_, We2aiKeyState>,
    input: CreateKeyInput,
) -> Result<CreatedKeyView, We2aiApiError> {
    use tauri::Emitter;
    let manager = session.0.clone();
    let notify = || {
        let _ = app.emit(EVENT_KEYS_CHANGED, ());
    };
    create_key(&manager, &keys, input, &notify).await
}

#[tauri::command]
pub async fn we2ai_update_key(
    app: tauri::AppHandle,
    session: State<'_, We2aiSessionState>,
    keys: State<'_, We2aiKeyState>,
    id: i64,
    input: UpdateKeyInput,
) -> Result<KeyManageView, We2aiApiError> {
    use tauri::Emitter;
    let manager = session.0.clone();
    let notify = || {
        let _ = app.emit(EVENT_KEYS_CHANGED, ());
    };
    update_key(&manager, &keys, id, input, &notify).await
}

#[tauri::command]
pub async fn we2ai_delete_key(
    app: tauri::AppHandle,
    session: State<'_, We2aiSessionState>,
    keys: State<'_, We2aiKeyState>,
    id: i64,
) -> Result<(), We2aiApiError> {
    use tauri::Emitter;
    let manager = session.0.clone();
    let notify = || {
        let _ = app.emit(EVENT_KEYS_CHANGED, ());
    };
    delete_key(&manager, &keys, id, &notify).await
}

/// 把 Key 明文写进系统剪贴板，明文不经 IPC。复用上游的
/// `copy_text_to_clipboard`（`arboard` + `spawn_blocking`），不改它的签名与行为。
#[tauri::command]
pub async fn we2ai_copy_key(
    session: State<'_, We2aiSessionState>,
    keys: State<'_, We2aiKeyState>,
    id: i64,
) -> Result<(), We2aiApiError> {
    copy_key(&session.0, &keys, id, |text| async move {
        crate::commands::copy_text_to_clipboard(text)
            .await
            .map(|_| ())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::we2ai::region::Region;
    use crate::we2ai::secret_store::test_support::InMemorySecretStore;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tempfile::TempDir;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SECRET_A: &str = "sk-we2ai-aaaaaaaaaaaaaaaaaaaa1111";
    const SECRET_B: &str = "sk-we2ai-bbbbbbbbbbbbbbbbbbbb2222";
    const SECRET_C: &str = "sk-we2ai-cccccccccccccccccccc3333";
    const IDEM: &str = "7f9c24e8-3b1a-4f0e-9a52-6d1c2b3a4e5f";

    fn manager(dir: &TempDir) -> SessionManager {
        SessionManager::new(
            Arc::new(InMemorySecretStore::new()),
            dir.path().to_path_buf(),
            "test".to_string(),
        )
    }

    fn mkey(id: i64, key: &str, status: &str) -> Value {
        json!({
            "id": id, "user_id": 42, "key": key, "name": format!("key-{id}"),
            "group_id": 7, "status": status,
            "quota": 10.0, "quota_used": 3.2,
            "expires_at": null, "last_used_at": "2026-09-30T08:00:00Z",
            "group": {"id": 7, "name": "默认分组", "platform": "anthropic", "rate_multiplier": 0.8},
        })
    }

    fn envelope(data: Value) -> Value {
        json!({"code": 0, "message": "success", "data": data})
    }

    async fn mount_keys_page(server: &MockServer, page: u32, pages: i64, items: Vec<Value>) {
        Mock::given(method("GET"))
            .and(path("/api/v1/keys"))
            .and(query_param("page", page.to_string()))
            .and(query_param("page_size", KEYS_PAGE_SIZE.to_string()))
            .and(header("authorization", "Bearer access-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!({
                "items": items, "total": 3, "page": page,
                "page_size": KEYS_PAGE_SIZE, "pages": pages
            }))))
            .expect(1)
            .mount(server)
            .await;
    }

    async fn mount_rates(server: &MockServer, data: Value) {
        Mock::given(method("GET"))
            .and(path("/api/v1/groups/rates"))
            .respond_with(ResponseTemplate::new(200).set_body_json(envelope(data)))
            .mount(server)
            .await;
    }

    fn seeded(dir: &TempDir, server: &MockServer) -> SessionManager {
        let manager = manager(dir);
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        manager
    }

    fn counter() -> (Arc<AtomicUsize>, impl Fn()) {
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        (count, move || {
            c.fetch_add(1, Ordering::SeqCst);
        })
    }

    // ----- 纯函数 -----

    #[test]
    fn status_normalization_treats_unknown_values_as_disabled() {
        assert_eq!(normalize_status("active"), "active");
        assert_eq!(normalize_status("quota_exhausted"), "quota_exhausted");
        assert_eq!(normalize_status("expired"), "expired");
        assert_eq!(normalize_status("inactive"), "inactive");
        assert_eq!(normalize_status("disabled"), "inactive");
        assert_eq!(normalize_status(""), "inactive");
        assert_eq!(normalize_status("something_new"), "inactive");
    }

    #[test]
    fn unescape_restores_the_five_go_html_entities_without_double_decoding() {
        assert_eq!(unescape_name("a &amp; b"), "a & b");
        assert_eq!(
            unescape_name("&lt;x&gt; &#34;q&#34; &#39;s&#39;"),
            "<x> \"q\" 's'"
        );
        // 原文就含 `&lt;` 字面量：服务端存的是 `&amp;lt;`，只能还原一层。
        assert_eq!(unescape_name("&amp;lt;"), "&lt;");
        assert_eq!(unescape_name("plain"), "plain");
    }

    #[test]
    fn view_uses_user_rate_then_group_rate_then_one_and_sanitizes_numbers() {
        let mut remote: RemoteManagedKey =
            serde_json::from_value(mkey(1, SECRET_A, "active")).unwrap();
        let mut rates = HashMap::new();
        assert_eq!(to_manage_view(&remote, &rates).group.unwrap().rate, 0.8);
        rates.insert("7".to_string(), 0.5);
        assert_eq!(to_manage_view(&remote, &rates).group.unwrap().rate, 0.5);

        remote.group.as_mut().unwrap().rate_multiplier = None;
        assert_eq!(
            to_manage_view(&remote, &HashMap::new()).group.unwrap().rate,
            1.0
        );

        remote.quota = Some(f64::NAN);
        remote.quota_used = Some(-5.0);
        remote.expires_at = Some(String::new());
        let view = to_manage_view(&remote, &HashMap::new());
        assert_eq!(view.quota, 0.0);
        assert_eq!(view.quota_used, 0.0);
        assert_eq!(view.expires_at, None);
    }

    #[test]
    fn manage_view_serializes_camel_case_without_any_secret_field() {
        let view = KeyManageView {
            id: 1,
            name: "n".into(),
            status: "active".into(),
            masked_key: "sk-we2…1111".into(),
            group: Some(KeyGroupView {
                id: 7,
                name: "g".into(),
                rate: 0.8,
            }),
            quota: 10.0,
            quota_used: 3.2,
            expires_at: None,
            last_used_at: None,
        };
        let value = serde_json::to_value(&view).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "expiresAt",
                "group",
                "id",
                "lastUsedAt",
                "maskedKey",
                "name",
                "quota",
                "quotaUsed",
                "status"
            ]
        );
        assert!(value["group"].get("rate").is_some());
    }

    #[test]
    fn created_view_debug_redacts_plaintext() {
        let created = CreatedKeyView {
            key: KeyManageView {
                id: 1,
                name: "n".into(),
                status: "active".into(),
                masked_key: "m".into(),
                group: None,
                quota: 0.0,
                quota_used: 0.0,
                expires_at: None,
                last_used_at: None,
            },
            plaintext: SECRET_A.to_string(),
        };
        assert!(!format!("{created:?}").contains(SECRET_A));
        let remote: RemoteManagedKey = serde_json::from_value(mkey(1, SECRET_A, "active")).unwrap();
        assert!(!format!("{remote:?}").contains(SECRET_A));
    }

    // ----- 输入校验 -----

    fn create_input() -> CreateKeyInput {
        CreateKeyInput {
            idempotency_key: IDEM.to_string(),
            name: "我的 Key".to_string(),
            group_id: None,
            quota: None,
            expires_in_days: None,
        }
    }

    #[test]
    fn create_body_maps_fields_and_trims_name() {
        let mut input = create_input();
        input.name = "  工作  ".into();
        input.group_id = Some(7);
        input.quota = Some(10.5);
        input.expires_in_days = Some(30);
        let (idem, body) = build_create_body(&input).unwrap();
        assert_eq!(idem, IDEM);
        assert_eq!(
            body,
            json!({"name": "工作", "group_id": 7, "quota": 10.5, "expires_in_days": 30})
        );

        let (_, minimal) = build_create_body(&create_input()).unwrap();
        assert_eq!(minimal, json!({"name": "我的 Key"}));

        let mut zero = create_input();
        zero.quota = Some(0.0);
        assert_eq!(build_create_body(&zero).unwrap().1["quota"], json!(0.0));
    }

    #[test]
    fn create_validation_rejects_bad_input() {
        let code = |input: CreateKeyInput| build_create_body(&input).unwrap_err().code;

        let mut i = create_input();
        i.name = "   ".into();
        assert_eq!(code(i), "KEY_NAME_INVALID");

        let mut i = create_input();
        i.quota = Some(-1.0);
        assert_eq!(code(i), "KEY_QUOTA_INVALID");

        let mut i = create_input();
        i.quota = Some(f64::INFINITY);
        assert_eq!(code(i), "KEY_QUOTA_INVALID");

        for days in [0, -3, EXPIRES_DAYS_MAX + 1] {
            let mut i = create_input();
            i.expires_in_days = Some(days);
            assert_eq!(code(i), "KEY_EXPIRY_INVALID");
        }

        let mut i = create_input();
        i.group_id = Some(0);
        assert_eq!(code(i), "KEY_GROUP_INVALID");

        for bad in [
            "",
            "abc",
            "not-a-uuid-not-a-uuid-not-a-uuid-xxxx",
            "7f9c24e83b1a4f0e9a526d1c2b3a4e5f",
        ] {
            let mut i = create_input();
            i.idempotency_key = bad.to_string();
            assert_eq!(code(i), "IDEMPOTENCY_KEY_INVALID", "{bad}");
        }
    }

    /// SubPanel `name` 是 `MaxLen(100)`，按 UTF-8 字节、在 `html.EscapeString` 之后校验。
    #[test]
    fn name_limit_counts_html_escaped_utf8_bytes() {
        let ok = |name: &str| validate_name(name).is_ok();
        // 汉字 3 字节：33 个 = 99 通过，34 个 = 102 拒绝。
        assert!(ok(&"中".repeat(33)));
        assert!(!ok(&"中".repeat(34)));
        // ASCII 1 字节：100 通过，101 拒绝。
        assert!(ok(&"x".repeat(100)));
        assert!(!ok(&"x".repeat(101)));
        // `&` 转义为 5 字节：95 + 1 个 `&` = 100 通过；96 + `&` = 101 拒绝。
        assert!(ok(&format!("{}&", "x".repeat(95))));
        assert!(!ok(&format!("{}&", "x".repeat(96))));
        // 另外四种：`<` `>` 4 字节，`"` `'` 5 字节。
        assert!(ok(&format!("{}<", "x".repeat(96))));
        assert!(!ok(&format!("{}<", "x".repeat(97))));
        assert!(ok(&format!("{}>", "x".repeat(96))));
        assert!(!ok(&format!("{}>", "x".repeat(97))));
        assert!(ok(&format!("{}\"", "x".repeat(95))));
        assert!(!ok(&format!("{}\"", "x".repeat(96))));
        assert!(ok(&format!("{}'", "x".repeat(95))));
        assert!(!ok(&format!("{}'", "x".repeat(96))));
        // 混合：2 个汉字(6) + 2 个 `&`(10) + 84 个 ASCII = 100。
        assert!(ok(&format!("中中&&{}", "x".repeat(84))));
        assert!(!ok(&format!("中中&&{}", "x".repeat(85))));
        // 4 字节字符（emoji）：25 个 = 100 通过，26 个拒绝。
        assert!(ok(&"😀".repeat(25)));
        assert!(!ok(&"😀".repeat(26)));
        // 非空（trim 之后）。
        assert!(!ok(""));
        assert!(!ok("   "));
        // 前后空白不计入长度。
        assert!(ok(&format!("  {}  ", "x".repeat(100))));
    }

    #[test]
    fn escaped_length_matches_go_html_escape_string() {
        assert_eq!(escaped_name_bytes("a&b"), "a&amp;b".len());
        assert_eq!(escaped_name_bytes("<>\"'"), "&lt;&gt;&#34;&#39;".len());
        assert_eq!(escaped_name_bytes("中"), 3);
    }

    #[test]
    fn update_body_only_contains_provided_fields() {
        assert_eq!(
            build_update_body(&UpdateKeyInput::default()).unwrap(),
            json!({})
        );

        let body = build_update_body(&UpdateKeyInput {
            name: Some(" 新名字 ".into()),
            group_id: Some(9),
            quota: Some(0.0),
            expires_at: Some("2026-12-31T15:59:59Z".into()),
            status: Some("inactive".into()),
            reset_quota: Some(true),
        })
        .unwrap();
        assert_eq!(
            body,
            json!({
                "name": "新名字", "group_id": 9, "quota": 0.0,
                "expires_at": "2026-12-31T15:59:59Z", "status": "inactive",
                "reset_quota": true
            })
        );

        // 空字符串 = 清除过期时间，原样发给服务端。
        let cleared = build_update_body(&UpdateKeyInput {
            expires_at: Some(String::new()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(cleared, json!({"expires_at": ""}));
    }

    #[test]
    fn update_validation_rejects_bad_input() {
        let code = |input: UpdateKeyInput| build_update_body(&input).unwrap_err().code;
        assert_eq!(
            code(UpdateKeyInput {
                name: Some(String::new()),
                ..Default::default()
            }),
            "KEY_NAME_INVALID"
        );
        assert_eq!(
            code(UpdateKeyInput {
                name: Some("中".repeat(34)),
                ..Default::default()
            }),
            "KEY_NAME_INVALID"
        );
        assert_eq!(
            code(UpdateKeyInput {
                quota: Some(-0.01),
                ..Default::default()
            }),
            "KEY_QUOTA_INVALID"
        );
        assert_eq!(
            code(UpdateKeyInput {
                expires_at: Some("2026-12-31".into()),
                ..Default::default()
            }),
            "KEY_EXPIRY_INVALID"
        );
        assert_eq!(
            code(UpdateKeyInput {
                status: Some("disabled".into()),
                ..Default::default()
            }),
            "KEY_STATUS_INVALID"
        );
        assert_eq!(
            code(UpdateKeyInput {
                group_id: Some(-1),
                ..Default::default()
            }),
            "KEY_GROUP_INVALID"
        );
    }

    #[test]
    fn input_deserializes_from_camel_case() {
        let create: CreateKeyInput = serde_json::from_value(json!({
            "idempotencyKey": IDEM, "name": "n", "groupId": 3, "quota": 5, "expiresInDays": 7
        }))
        .unwrap();
        assert_eq!(create.group_id, Some(3));
        assert_eq!(create.expires_in_days, Some(7));
        let update: UpdateKeyInput = serde_json::from_value(json!({
            "expiresAt": "", "resetQuota": true, "groupId": 2
        }))
        .unwrap();
        assert_eq!(update.expires_at.as_deref(), Some(""));
        assert_eq!(update.reset_quota, Some(true));
        assert_eq!(update.group_id, Some(2));
        assert!(update.name.is_none() && update.status.is_none());
    }

    // ----- 列表与明文缓存 -----

    /// 全部状态都列出（含已禁用、已过期）；视图与序列化结果不含明文；明文进管理页缓存，
    /// 模型广场缓存不受影响。
    #[tokio::test]
    async fn list_returns_all_statuses_without_plaintext_and_caches_secrets_separately() {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        let mut escaped = mkey(2, SECRET_B, "inactive");
        escaped["name"] = json!("a &amp; &lt;b&gt;");
        escaped["group"] = Value::Null;
        escaped["quota"] = json!(0);
        mount_keys_page(
            &server,
            1,
            1,
            vec![
                mkey(1, SECRET_A, "active"),
                escaped,
                mkey(3, SECRET_C, "expired"),
            ],
        )
        .await;
        mount_rates(&server, json!({"7": 0.5})).await;
        let manager = seeded(&dir, &server);
        let state = We2aiKeyState::default();

        let views = manage_list_keys(&manager, &state).await.unwrap();

        assert_eq!(
            views.iter().map(|v| v.id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(views[0].group.as_ref().unwrap().rate, 0.5, "专属倍率优先");
        assert_eq!(views[0].quota, 10.0);
        assert_eq!(views[0].quota_used, 3.2);
        assert_eq!(
            views[0].last_used_at.as_deref(),
            Some("2026-09-30T08:00:00Z")
        );
        assert_eq!(views[0].masked_key, "sk-we2…1111");
        assert_eq!(views[1].name, "a & <b>");
        assert_eq!(views[1].status, "inactive");
        assert_eq!(views[1].group, None);
        assert_eq!(views[1].quota, 0.0);
        assert_eq!(views[2].status, "expired");

        let serialized = serde_json::to_string(&views).unwrap();
        for secret in [SECRET_A, SECRET_B, SECRET_C] {
            assert!(
                !serialized.contains(secret),
                "plaintext leaked: {serialized}"
            );
        }

        let identity = manager.current_identity();
        for (id, secret) in [(1, SECRET_A), (2, SECRET_B), (3, SECRET_C)] {
            assert_eq!(
                state.manage_secret_for(identity, id).as_deref(),
                Some(secret)
            );
        }
        assert_eq!(
            state.secret_for(identity, 2),
            None,
            "模型广场缓存不应被管理页列表写入"
        );
    }

    #[tokio::test]
    async fn list_pages_to_the_end_like_the_marketplace_list() {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        mount_keys_page(&server, 1, 2, vec![mkey(1, SECRET_A, "active")]).await;
        mount_keys_page(&server, 2, 2, vec![mkey(2, SECRET_B, "inactive")]).await;
        mount_rates(&server, Value::Null).await;
        let manager = seeded(&dir, &server);
        let state = We2aiKeyState::default();

        let views = manage_list_keys(&manager, &state).await.unwrap();

        assert_eq!(views.iter().map(|v| v.id).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(
            views[0].group.as_ref().unwrap().rate,
            0.8,
            "rates 为 null 时回退分组默认倍率"
        );
    }

    #[tokio::test]
    async fn rates_failure_falls_back_to_group_rate_but_session_loss_propagates() {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        mount_keys_page(&server, 1, 1, vec![mkey(1, SECRET_A, "active")]).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/groups/rates"))
            .respond_with(ResponseTemplate::new(400))
            .mount(&server)
            .await;
        let manager = seeded(&dir, &server);
        let state = We2aiKeyState::default();
        let views = manage_list_keys(&manager, &state).await.unwrap();
        assert_eq!(views[0].group.as_ref().unwrap().rate, 0.8);

        let dir2 = TempDir::new().unwrap();
        let server2 = MockServer::start().await;
        mount_keys_page(&server2, 1, 1, vec![mkey(1, SECRET_A, "active")]).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/groups/rates"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_json(json!({"code": "TOKEN_REVOKED", "message": "revoked"})),
            )
            .mount(&server2)
            .await;
        let manager2 = seeded(&dir2, &server2);
        let err = manage_list_keys(&manager2, &We2aiKeyState::default())
            .await
            .unwrap_err();
        assert_eq!(err, SessionError::Terminated("TOKEN_REVOKED".to_string()));
    }

    #[tokio::test]
    async fn list_requires_an_active_session() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let err = manage_list_keys(&manager, &We2aiKeyState::default())
            .await
            .unwrap_err();
        assert_eq!(err, SessionError::NoActiveSession);
    }

    #[tokio::test]
    async fn older_list_is_superseded_by_a_newer_one() {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        // 第一次响应慢，第二次响应快：较早发起的拉取较晚完成，不能覆盖较新的缓存。
        Mock::given(method("GET"))
            .and(path("/api/v1/keys"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(envelope(json!({
                        "items": [mkey(1, SECRET_A, "active")],
                        "total": 1, "page": 1, "page_size": 100, "pages": 1
                    })))
                    .set_delay(std::time::Duration::from_millis(500)),
            )
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!({
                "items": [mkey(3, SECRET_C, "active")],
                "total": 1, "page": 1, "page_size": 100, "pages": 1
            }))))
            .with_priority(2)
            .mount(&server)
            .await;
        mount_rates(&server, Value::Null).await;
        let manager = seeded(&dir, &server);
        let state = Arc::new(We2aiKeyState::default());

        let (m, s) = (manager.clone(), state.clone());
        let older = tokio::spawn(async move { manage_list_keys(&m, &s).await });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let newer = manage_list_keys(&manager, &state).await.unwrap();
        assert_eq!(newer[0].id, 3);

        assert_eq!(
            older.await.unwrap().unwrap_err(),
            SessionError::Other(KEY_LIST_SUPERSEDED.to_string())
        );
        let identity = manager.current_identity();
        assert!(state.manage_secret_for(identity, 3).is_some());
        assert!(state.manage_secret_for(identity, 1).is_none());
    }

    #[test]
    fn invalidate_drops_in_flight_fetches_and_clears_both_caches() {
        let state = We2aiKeyState::default();
        let identity = SessionIdentity {
            region: Region::International,
            user_id: 42,
            generation: 1,
        };
        let seq = state.begin_manage_fetch();
        assert!(state.store_manage(seq, identity, HashMap::from([(1, SECRET_A.to_string())])));
        let in_flight = state.begin_manage_fetch();

        state.invalidate();

        assert_eq!(state.manage_secret_for(Some(identity), 1), None);
        assert!(
            !state.store_manage(in_flight, identity, HashMap::new()),
            "失效前已发起的拉取不能再写回缓存"
        );
        let fresh = state.begin_manage_fetch();
        assert!(state.store_manage(fresh, identity, HashMap::new()));
    }

    // ----- copy 与缓存隔离 -----

    /// 用记录型 writer 代替真实剪贴板：返回「被写入剪贴板的文本」。
    async fn copied(
        manager: &SessionManager,
        state: &We2aiKeyState,
        id: i64,
    ) -> Result<String, We2aiApiError> {
        let slot = Arc::new(std::sync::Mutex::new(None::<String>));
        let writer_slot = slot.clone();
        copy_key(manager, state, id, |text| async move {
            *writer_slot.lock().unwrap() = Some(text);
            Ok(())
        })
        .await?;
        let text = slot.lock().unwrap().take().expect("writer was called");
        Ok(text)
    }

    #[tokio::test]
    async fn copy_reads_only_from_the_cache_and_never_calls_the_writer_on_a_miss() {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        mount_keys_page(
            &server,
            1,
            1,
            vec![mkey(1, SECRET_A, "active"), mkey(2, SECRET_B, "inactive")],
        )
        .await;
        mount_rates(&server, Value::Null).await;
        let manager = seeded(&dir, &server);
        let state = We2aiKeyState::default();

        // 没拉过列表：不在缓存。
        let calls = Arc::new(AtomicUsize::new(0));
        let miss = |id: i64| {
            let calls = calls.clone();
            let (manager, state) = (&manager, &state);
            async move {
                copy_key(manager, state, id, |_| async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
                .await
                .unwrap_err()
                .code
            }
        };
        assert_eq!(miss(1).await, "KEY_NOT_FOUND");

        manage_list_keys(&manager, &state).await.unwrap();
        assert_eq!(copied(&manager, &state, 1).await.unwrap(), SECRET_A);
        assert_eq!(
            copied(&manager, &state, 2).await.unwrap(),
            SECRET_B,
            "已禁用的 Key 也能复制"
        );
        assert_eq!(miss(99).await, "KEY_NOT_FOUND");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "缓存未命中不得写剪贴板");
    }

    #[tokio::test]
    async fn cache_is_isolated_per_session_identity_and_cleared_on_logout() {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        mount_keys_page(&server, 1, 1, vec![mkey(1, SECRET_A, "active")]).await;
        mount_rates(&server, Value::Null).await;
        let manager = seeded(&dir, &server);
        let state = We2aiKeyState::default();
        manage_list_keys(&manager, &state).await.unwrap();
        assert!(copied(&manager, &state, 1).await.is_ok());

        // 换账号：旧缓存不可见，并被顺带清空。
        manager.test_seed_active(Region::International, 7, "access-1", server.uri());
        assert_eq!(
            copied(&manager, &state, 1).await.unwrap_err().code,
            "KEY_NOT_FOUND"
        );
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        assert_eq!(
            copied(&manager, &state, 1).await.unwrap_err().code,
            "KEY_NOT_FOUND",
            "回到原账号也不能复活已被清空的缓存（generation 已变）"
        );

        // 登出清空。
        let dir2 = TempDir::new().unwrap();
        let server2 = MockServer::start().await;
        mount_keys_page(&server2, 1, 1, vec![mkey(1, SECRET_A, "active")]).await;
        mount_rates(&server2, Value::Null).await;
        let manager2 = seeded(&dir2, &server2);
        let state2 = We2aiKeyState::default();
        manage_list_keys(&manager2, &state2).await.unwrap();
        state2.clear();
        assert_eq!(
            copied(&manager2, &state2, 1).await.unwrap_err().code,
            "KEY_NOT_FOUND"
        );
    }

    // ----- 创建 -----

    #[tokio::test]
    async fn create_sends_idempotency_header_and_returns_plaintext_once() {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/keys"))
            .and(header("idempotency-key", IDEM))
            .and(header("authorization", "Bearer access-1"))
            .and(body_json(json!({
                "name": "新建", "group_id": 7, "quota": 10.0, "expires_in_days": 30
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!({
                "id": 11, "key": SECRET_A, "name": "新建", "status": "active",
                "quota": 10.0, "quota_used": 0.0, "expires_at": "2026-10-31T00:00:00Z",
                "last_used_at": null
            }))))
            .expect(1)
            .mount(&server)
            .await;
        let manager = seeded(&dir, &server);
        let state = We2aiKeyState::default();
        let (count, notify) = counter();

        let created = create_key(
            &manager,
            &state,
            CreateKeyInput {
                idempotency_key: IDEM.into(),
                name: "新建".into(),
                group_id: Some(7),
                quota: Some(10.0),
                expires_in_days: Some(30),
            },
            &notify,
        )
        .await
        .unwrap();

        assert_eq!(created.plaintext, SECRET_A);
        assert_eq!(created.key.id, 11);
        assert_eq!(created.key.masked_key, "sk-we2…1111");
        assert_eq!(
            created.key.expires_at.as_deref(),
            Some("2026-10-31T00:00:00Z")
        );
        assert!(!serde_json::to_string(&created.key)
            .unwrap()
            .contains(SECRET_A));
        assert_eq!(count.load(Ordering::SeqCst), 1, "应发出一次 keys-changed");
        // 创建后新 Key 的明文已并入管理页缓存：卡片里的复制（copy_key）立刻命中，
        // 不必等列表重拉。
        assert_eq!(copied(&manager, &state, 11).await.unwrap(), SECRET_A);
    }

    /// 创建会先失效整个缓存再并入新 Key：旧 Key 在下次列表前不可复制，新 Key 可以。
    #[tokio::test]
    async fn create_invalidates_old_entries_but_keeps_the_new_key_copyable() {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        mount_keys_page(&server, 1, 1, vec![mkey(1, SECRET_A, "active")]).await;
        mount_rates(&server, Value::Null).await;
        Mock::given(method("POST"))
            .and(path("/api/v1/keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!({
                "id": 11, "key": SECRET_C, "name": "新建", "status": "active"
            }))))
            .mount(&server)
            .await;
        let manager = seeded(&dir, &server);
        let state = We2aiKeyState::default();
        manage_list_keys(&manager, &state).await.unwrap();
        assert!(copied(&manager, &state, 1).await.is_ok());

        create_key(&manager, &state, create_input(), &|| {})
            .await
            .unwrap();

        assert_eq!(copied(&manager, &state, 11).await.unwrap(), SECRET_C);
        assert_eq!(
            copied(&manager, &state, 1).await.unwrap_err().code,
            "KEY_NOT_FOUND"
        );
    }

    #[test]
    fn insert_manage_secret_appends_for_the_same_identity_and_rebuilds_for_another() {
        let state = We2aiKeyState::default();
        let a = SessionIdentity {
            region: Region::International,
            user_id: 42,
            generation: 1,
        };
        let b = SessionIdentity {
            region: Region::International,
            user_id: 7,
            generation: 2,
        };
        let seq = state.begin_manage_fetch();
        state.store_manage(seq, a, HashMap::from([(1, SECRET_A.to_string())]));

        state.insert_manage_secret(a, 2, SECRET_B.to_string());
        assert_eq!(
            state.manage_secret_for(Some(a), 1).as_deref(),
            Some(SECRET_A)
        );
        assert_eq!(
            state.manage_secret_for(Some(a), 2).as_deref(),
            Some(SECRET_B)
        );

        // 别的身份的缓存不能混入：以新身份重建，旧身份的 Key 取不到。
        state.insert_manage_secret(b, 3, SECRET_C.to_string());
        assert_eq!(
            state.manage_secret_for(Some(b), 3).as_deref(),
            Some(SECRET_C)
        );
        assert_eq!(state.manage_secret_for(Some(b), 1), None);
        assert_eq!(state.manage_secret_for(Some(a), 3), None);
    }

    #[tokio::test]
    async fn clipboard_failure_maps_to_clipboard_failed_without_leaking_the_secret() {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        mount_keys_page(&server, 1, 1, vec![mkey(1, SECRET_A, "active")]).await;
        mount_rates(&server, Value::Null).await;
        let manager = seeded(&dir, &server);
        let state = We2aiKeyState::default();
        manage_list_keys(&manager, &state).await.unwrap();

        let err = copy_key(&manager, &state, 1, |_| async {
            Err("写入系统剪贴板失败: denied".to_string())
        })
        .await
        .unwrap_err();

        assert_eq!(err.code, "CLIPBOARD_FAILED");
        assert!(!err.message.contains(SECRET_A));
        // 失败不清缓存，可重试。
        assert!(copied(&manager, &state, 1).await.is_ok());
    }

    #[tokio::test]
    async fn copy_without_a_session_reports_key_not_found() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let err = copy_key(&manager, &We2aiKeyState::default(), 1, |_| async { Ok(()) })
            .await
            .unwrap_err();
        assert_eq!(err.code, "KEY_NOT_FOUND");
    }

    #[tokio::test]
    async fn invalid_create_input_makes_no_request_and_no_event() {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let manager = seeded(&dir, &server);
        let state = We2aiKeyState::default();
        let (count, notify) = counter();

        let mut input = create_input();
        input.name = String::new();
        let err = create_key(&manager, &state, input, &notify)
            .await
            .unwrap_err();

        assert_eq!(err.code, "KEY_NAME_INVALID");
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn create_failure_keeps_caches_and_emits_nothing() {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        mount_keys_page(&server, 1, 1, vec![mkey(1, SECRET_A, "active")]).await;
        mount_rates(&server, Value::Null).await;
        Mock::given(method("POST"))
            .and(path("/api/v1/keys"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "code": 403, "message": "limit", "reason": "API_KEY_COUNT_EXCEEDED"
            })))
            .mount(&server)
            .await;
        let manager = seeded(&dir, &server);
        let state = We2aiKeyState::default();
        manage_list_keys(&manager, &state).await.unwrap();
        let (count, notify) = counter();

        let err = create_key(&manager, &state, create_input(), &notify)
            .await
            .unwrap_err();

        assert_eq!(err.code, "API_KEY_COUNT_EXCEEDED");
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert!(copied(&manager, &state, 1).await.is_ok(), "失败不应清缓存");
    }

    #[tokio::test]
    async fn create_with_revoked_token_terminates_the_session() {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/keys"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_json(json!({"code": "TOKEN_REVOKED", "message": "revoked"})),
            )
            .mount(&server)
            .await;
        let manager = seeded(&dir, &server);
        let (count, notify) = counter();

        let err = create_key(&manager, &We2aiKeyState::default(), create_input(), &notify)
            .await
            .unwrap_err();

        assert_eq!(err.code, "TOKEN_REVOKED");
        assert!(manager.current_identity().is_none(), "会话应已终止");
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    /// 5xx 触发自动重放时必须带同一个幂等键（服务端据此返回首次结果，不会重复创建）。
    #[tokio::test]
    async fn transient_5xx_replays_create_with_the_same_idempotency_key() {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/keys"))
            .and(header("idempotency-key", IDEM))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/keys"))
            .and(header("idempotency-key", IDEM))
            .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!({
                "id": 11, "key": SECRET_A, "name": "我的 Key", "status": "active"
            }))))
            .with_priority(2)
            .mount(&server)
            .await;
        let manager = seeded(&dir, &server);

        let created = create_key(&manager, &We2aiKeyState::default(), create_input(), &|| {})
            .await
            .unwrap();

        assert_eq!(created.key.id, 11);
        let requests = server.received_requests().await.unwrap();
        let posts: Vec<_> = requests
            .iter()
            .filter(|r| r.method.as_str() == "POST")
            .collect();
        assert_eq!(posts.len(), 2, "第一次 503，第二次重放");
        for request in posts {
            assert_eq!(
                request
                    .headers
                    .get("idempotency-key")
                    .map(|v| v.to_str().unwrap()),
                Some(IDEM)
            );
            assert_eq!(
                serde_json::from_slice::<Value>(&request.body).unwrap(),
                json!({"name": "我的 Key"}),
                "重放的请求体不变"
            );
        }
    }

    /// 写操作的 `invalidate()` 同样作废模型广场（`keys::list_keys`）此刻在途的拉取：
    /// 它读到的是写操作之前的数据，不能写回缓存，也不能交给前端。
    #[tokio::test]
    async fn invalidate_voids_an_in_flight_marketplace_list() {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/keys"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(envelope(json!({
                        "items": [mkey(1, SECRET_A, "active")],
                        "total": 1, "page": 1, "page_size": 100, "pages": 1
                    })))
                    .set_delay(std::time::Duration::from_millis(400)),
            )
            .mount(&server)
            .await;
        let manager = seeded(&dir, &server);
        let state = Arc::new(We2aiKeyState::default());

        let (m, s) = (manager.clone(), state.clone());
        let in_flight = tokio::spawn(async move { crate::we2ai::keys::list_keys(&m, &s).await });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        state.invalidate();

        assert_eq!(
            in_flight.await.unwrap().unwrap_err(),
            SessionError::Other(KEY_LIST_SUPERSEDED.to_string())
        );
        assert_eq!(
            state.secret_for(manager.current_identity(), 1),
            None,
            "旧数据不得写回模型广场缓存"
        );
        // 失效之后新发起的拉取正常写入。
        let fresh = crate::we2ai::keys::list_keys(&manager, &state)
            .await
            .unwrap();
        assert_eq!(fresh.keys.len(), 1);
        assert!(state.secret_for(manager.current_identity(), 1).is_some());
    }

    // ----- 更新 -----

    #[tokio::test]
    async fn update_puts_only_changed_fields_and_invalidates() {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/v1/keys/5"))
            .and(body_json(json!({
                "name": "改名", "status": "inactive", "expires_at": "", "reset_quota": true
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(envelope({
                let mut k = mkey(5, SECRET_B, "inactive");
                k["name"] = json!("改名");
                k
            })))
            .expect(1)
            .mount(&server)
            .await;
        mount_keys_page(&server, 1, 1, vec![mkey(5, SECRET_B, "active")]).await;
        mount_rates(&server, Value::Null).await;
        let manager = seeded(&dir, &server);
        let state = We2aiKeyState::default();
        manage_list_keys(&manager, &state).await.unwrap();
        let (count, notify) = counter();

        let view = update_key(
            &manager,
            &state,
            5,
            UpdateKeyInput {
                name: Some("改名".into()),
                status: Some("inactive".into()),
                expires_at: Some(String::new()),
                reset_quota: Some(true),
                ..Default::default()
            },
            &notify,
        )
        .await
        .unwrap();

        assert_eq!(view.name, "改名");
        assert_eq!(view.status, "inactive");
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(
            copied(&manager, &state, 5).await.unwrap_err().code,
            "KEY_NOT_FOUND",
            "写操作后缓存失效"
        );
    }

    #[tokio::test]
    async fn invalid_update_makes_no_request() {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let manager = seeded(&dir, &server);
        let (count, notify) = counter();

        let err = update_key(
            &manager,
            &We2aiKeyState::default(),
            5,
            UpdateKeyInput {
                expires_at: Some("tomorrow".into()),
                ..Default::default()
            },
            &notify,
        )
        .await
        .unwrap_err();

        assert_eq!(err.code, "KEY_EXPIRY_INVALID");
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    // ----- 删除 -----

    fn write_selection(manager: &SessionManager, slot: &str, key_id: i64) {
        let json = json!({"selections": {slot: key_id}}).to_string();
        std::fs::write(manager.data_root().join("key_selection.json"), json).unwrap();
    }

    fn read_selection(manager: &SessionManager) -> Value {
        let text = std::fs::read_to_string(manager.data_root().join("key_selection.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    async fn mount_delete(server: &MockServer, id: i64, response: ResponseTemplate) {
        Mock::given(method("DELETE"))
            .and(path(format!("/api/v1/keys/{id}")))
            .and(header("authorization", "Bearer access-1"))
            .respond_with(response)
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn deleting_the_selected_key_clears_the_selection_and_notifies() {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        mount_delete(
            &server,
            5,
            ResponseTemplate::new(200)
                .set_body_json(envelope(json!({"message": "API key deleted successfully"}))),
        )
        .await;
        let manager = seeded(&dir, &server);
        write_selection(&manager, "international:42", 5);
        let state = We2aiKeyState::default();
        let (count, notify) = counter();

        delete_key(&manager, &state, 5, &notify).await.unwrap();

        assert_eq!(
            read_selection(&manager)["selections"]
                .as_object()
                .unwrap()
                .len(),
            0,
            "选中项应被清除"
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn deleting_another_key_keeps_the_selection() {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        mount_delete(
            &server,
            6,
            ResponseTemplate::new(200).set_body_json(envelope(json!({"message": "ok"}))),
        )
        .await;
        let manager = seeded(&dir, &server);
        write_selection(&manager, "international:42", 5);

        delete_key(&manager, &We2aiKeyState::default(), 6, &|| {})
            .await
            .unwrap();

        assert_eq!(
            read_selection(&manager)["selections"]["international:42"],
            5
        );
    }

    #[tokio::test]
    async fn deleting_a_missing_key_counts_as_success_and_still_refreshes() {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        mount_delete(
            &server,
            5,
            ResponseTemplate::new(404).set_body_json(json!({
                "code": 404, "message": "api key not found", "reason": "API_KEY_NOT_FOUND"
            })),
        )
        .await;
        let manager = seeded(&dir, &server);
        write_selection(&manager, "international:42", 5);
        let (count, notify) = counter();

        delete_key(&manager, &We2aiKeyState::default(), 5, &notify)
            .await
            .unwrap();

        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(
            read_selection(&manager)["selections"]
                .as_object()
                .unwrap()
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn delete_failure_surfaces_the_error_and_changes_nothing() {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        mount_delete(
            &server,
            5,
            ResponseTemplate::new(403).set_body_json(json!({
                "code": 403, "message": "no", "reason": "FORBIDDEN"
            })),
        )
        .await;
        let manager = seeded(&dir, &server);
        write_selection(&manager, "international:42", 5);
        let (count, notify) = counter();

        let err = delete_key(&manager, &We2aiKeyState::default(), 5, &notify)
            .await
            .unwrap_err();

        assert_eq!(err.code, "FORBIDDEN");
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert_eq!(
            read_selection(&manager)["selections"]["international:42"],
            5
        );
    }

    // ----- 重置额度不可自动重放 -----

    async fn put_count(server: &MockServer) -> usize {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.method.as_str() == "PUT")
            .count()
    }

    /// 含 `reset_quota=true` 的更新遇到瞬时错误（5xx/429）只发一次请求：首次已生效
    /// 但响应丢失时再放一次会把中间产生的用量二次清零。
    #[tokio::test]
    async fn reset_quota_update_is_sent_once_on_transient_errors() {
        for status in [503u16, 429, 500] {
            let dir = TempDir::new().unwrap();
            let server = MockServer::start().await;
            Mock::given(method("PUT"))
                .and(path("/api/v1/keys/5"))
                .respond_with(ResponseTemplate::new(status))
                .mount(&server)
                .await;
            let manager = seeded(&dir, &server);
            let (count, notify) = counter();

            let started = std::time::Instant::now();
            let err = update_key(
                &manager,
                &We2aiKeyState::default(),
                5,
                UpdateKeyInput {
                    reset_quota: Some(true),
                    ..Default::default()
                },
                &notify,
            )
            .await
            .unwrap_err();

            assert_eq!(err.code, "TRANSIENT", "status {status}");
            assert_eq!(put_count(&server).await, 1, "status {status}: 不得自动重放");
            assert!(
                started.elapsed() < std::time::Duration::from_millis(900),
                "没有退避等待"
            );
            assert_eq!(count.load(Ordering::SeqCst), 0, "失败不发事件");
        }
    }

    /// 重置额度混在其他字段里同样不重放（整个请求按非幂等处理）。
    #[tokio::test]
    async fn reset_quota_mixed_with_other_fields_is_not_replayed() {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/v1/keys/5"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let manager = seeded(&dir, &server);

        let err = update_key(
            &manager,
            &We2aiKeyState::default(),
            5,
            UpdateKeyInput {
                name: Some("改名".into()),
                quota: Some(20.0),
                reset_quota: Some(true),
                ..Default::default()
            },
            &|| {},
        )
        .await
        .unwrap_err();

        assert_eq!(err.code, "TRANSIENT");
        assert_eq!(put_count(&server).await, 1);
    }

    /// `reset_quota=false` 或缺省的更新仍然可重放（都是绝对值赋值）。
    #[tokio::test]
    async fn regular_updates_are_still_replayed_on_a_transient_error() {
        for reset in [None, Some(false)] {
            let dir = TempDir::new().unwrap();
            let server = MockServer::start().await;
            Mock::given(method("PUT"))
                .and(path("/api/v1/keys/5"))
                .respond_with(ResponseTemplate::new(503))
                .up_to_n_times(1)
                .with_priority(1)
                .mount(&server)
                .await;
            Mock::given(method("PUT"))
                .and(path("/api/v1/keys/5"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(envelope(mkey(5, SECRET_A, "active"))),
                )
                .with_priority(2)
                .mount(&server)
                .await;
            let manager = seeded(&dir, &server);

            update_key(
                &manager,
                &We2aiKeyState::default(),
                5,
                UpdateKeyInput {
                    name: Some("改名".into()),
                    reset_quota: reset,
                    ..Default::default()
                },
                &|| {},
            )
            .await
            .unwrap();

            assert_eq!(
                put_count(&server).await,
                2,
                "reset={reset:?}: 第一次 503 后重放"
            );
        }
    }

    // ----- 响应晚到：写结果绑定发起身份 -----

    const OLD_SLOT: &str = "international:42";
    /// 同一个用户 id 切到另一个区域（最容易误清的场景）。
    const NEW_SLOT: &str = "domestic_prod:42";

    fn write_selections(manager: &SessionManager, slots: &[(&str, i64)]) {
        let selections: serde_json::Map<String, Value> = slots
            .iter()
            .map(|(slot, id)| (slot.to_string(), json!(id)))
            .collect();
        std::fs::write(
            manager.data_root().join("key_selection.json"),
            json!({ "selections": selections }).to_string(),
        )
        .unwrap();
    }

    /// 在途写请求期间切换到另一个区域的同一用户：返回新身份，并给新身份的管理页缓存
    /// 放一个 Key，之后断言它没被旧响应碰过。
    async fn switch_identity_mid_flight(
        manager: &SessionManager,
        state: &We2aiKeyState,
        server: &MockServer,
    ) -> SessionIdentity {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        manager.test_seed_active(Region::DomesticProd, 42, "access-1", server.uri());
        let new_identity = manager.current_identity().unwrap();
        let seq = state.begin_manage_fetch();
        assert!(state.store_manage(
            seq,
            new_identity,
            HashMap::from([(5, SECRET_B.to_string())])
        ));
        new_identity
    }

    fn delayed(template: ResponseTemplate) -> ResponseTemplate {
        template.set_delay(std::time::Duration::from_millis(350))
    }

    async fn assert_delete_response_ignored_after_identity_switch(response: ResponseTemplate) {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v1/keys/5"))
            .respond_with(delayed(response))
            .expect(1)
            .mount(&server)
            .await;
        let manager = seeded(&dir, &server);
        write_selections(&manager, &[(OLD_SLOT, 5), (NEW_SLOT, 5)]);
        let state = Arc::new(We2aiKeyState::default());
        let (count, notify) = counter();

        let (m, s) = (manager.clone(), state.clone());
        let task = tokio::spawn(async move { delete_key(&m, &s, 5, &notify).await });
        let new_identity = switch_identity_mid_flight(&manager, &state, &server).await;

        let err = task.await.unwrap().unwrap_err();

        assert_eq!(err.code, "SESSION_CHANGED");
        let selections = read_selection(&manager)["selections"].clone();
        assert_eq!(selections[NEW_SLOT], 5, "新身份的选择记忆不能被旧响应清掉");
        assert_eq!(selections[OLD_SLOT], 5, "结果未知，旧身份的记忆也不动");
        assert_eq!(count.load(Ordering::SeqCst), 0, "不发事件");
        assert_eq!(
            state.manage_secret_for(Some(new_identity), 5).as_deref(),
            Some(SECRET_B),
            "新身份的缓存不被失效"
        );
    }

    #[tokio::test]
    async fn a_late_delete_success_after_an_identity_switch_touches_nothing() {
        assert_delete_response_ignored_after_identity_switch(
            ResponseTemplate::new(200).set_body_json(envelope(json!({"message": "ok"}))),
        )
        .await;
    }

    #[tokio::test]
    async fn a_late_delete_not_found_after_an_identity_switch_touches_nothing() {
        assert_delete_response_ignored_after_identity_switch(
            ResponseTemplate::new(404).set_body_json(json!({
                "code": 404, "message": "api key not found", "reason": "API_KEY_NOT_FOUND"
            })),
        )
        .await;
    }

    /// 没有切换身份时，「不存在」与成功都只清发起身份自己的槽位。
    #[tokio::test]
    async fn delete_only_clears_the_initiating_identitys_slot() {
        for response in [
            ResponseTemplate::new(200).set_body_json(envelope(json!({"message": "ok"}))),
            ResponseTemplate::new(404).set_body_json(json!({
                "code": 404, "message": "nf", "reason": "API_KEY_NOT_FOUND"
            })),
        ] {
            let dir = TempDir::new().unwrap();
            let server = MockServer::start().await;
            Mock::given(method("DELETE"))
                .and(path("/api/v1/keys/5"))
                .respond_with(response)
                .mount(&server)
                .await;
            let manager = seeded(&dir, &server);
            write_selections(
                &manager,
                &[(OLD_SLOT, 5), (NEW_SLOT, 5), ("international:7", 5)],
            );

            delete_key(&manager, &We2aiKeyState::default(), 5, &|| {})
                .await
                .unwrap();

            let selections = read_selection(&manager)["selections"].clone();
            assert!(selections.get(OLD_SLOT).is_none());
            assert_eq!(selections[NEW_SLOT], 5);
            assert_eq!(selections["international:7"], 5);
        }
    }

    /// 创建/更新的响应晚到：会话已变就不失效新身份缓存、不并入明文、不发事件。
    #[tokio::test]
    async fn late_create_and_update_responses_do_not_touch_the_new_identitys_state() {
        for is_create in [true, false] {
            let dir = TempDir::new().unwrap();
            let server = MockServer::start().await;
            Mock::given(method(if is_create { "POST" } else { "PUT" }))
                .respond_with(delayed(ResponseTemplate::new(200).set_body_json(envelope(
                    json!({"id": 11, "key": SECRET_C, "name": "n", "status": "active"}),
                ))))
                .mount(&server)
                .await;
            let manager = seeded(&dir, &server);
            let state = Arc::new(We2aiKeyState::default());
            let (count, notify) = counter();

            let (m, s) = (manager.clone(), state.clone());
            let task = tokio::spawn(async move {
                if is_create {
                    create_key(&m, &s, create_input(), &notify)
                        .await
                        .map(|_| ())
                } else {
                    update_key(
                        &m,
                        &s,
                        5,
                        UpdateKeyInput {
                            name: Some("n".into()),
                            ..Default::default()
                        },
                        &notify,
                    )
                    .await
                    .map(|_| ())
                }
            });
            let new_identity = switch_identity_mid_flight(&manager, &state, &server).await;

            let err = task.await.unwrap().unwrap_err();

            assert_eq!(err.code, "SESSION_CHANGED", "create={is_create}");
            assert_eq!(count.load(Ordering::SeqCst), 0, "create={is_create}");
            assert_eq!(
                state.manage_secret_for(Some(new_identity), 5).as_deref(),
                Some(SECRET_B),
                "create={is_create}: 新身份缓存不被失效"
            );
            assert_eq!(
                state.manage_secret_for(Some(new_identity), 11),
                None,
                "create={is_create}: 旧身份的新 Key 明文不得并入新身份缓存"
            );
        }
    }

    // ----- 分组 -----

    #[tokio::test]
    async fn key_groups_merge_available_groups_with_user_rates() {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/groups/available"))
            .and(header("authorization", "Bearer access-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!([
                {"id": 7, "name": "默认分组", "platform": "anthropic", "rate_multiplier": 1.0},
                {"id": 8, "name": "OpenAI 组", "platform": "openai", "rate_multiplier": 0.8},
                {"id": 9, "name": "坏倍率", "platform": "openai", "rate_multiplier": 0}
            ]))))
            .expect(1)
            .mount(&server)
            .await;
        mount_rates(&server, json!({"7": 0.6})).await;
        let manager = seeded(&dir, &server);

        let groups = list_key_groups(&manager).await.unwrap();

        assert_eq!(
            groups,
            vec![
                KeyGroupOptionView {
                    id: 7,
                    name: "默认分组".into(),
                    platform: "anthropic".into(),
                    rate: 0.6
                },
                KeyGroupOptionView {
                    id: 8,
                    name: "OpenAI 组".into(),
                    platform: "openai".into(),
                    rate: 0.8
                },
                KeyGroupOptionView {
                    id: 9,
                    name: "坏倍率".into(),
                    platform: "openai".into(),
                    rate: 1.0
                },
            ]
        );
        let value = serde_json::to_value(&groups[0]).unwrap();
        assert_eq!(
            value,
            json!({"id": 7, "name": "默认分组", "platform": "anthropic", "rate": 0.6})
        );
    }
}
