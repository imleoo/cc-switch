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

use super::api::{
    ApiCallError, ApiClient, ModelCapabilities, Paginated, RemoteApiKey, RemoteKeyModels,
    RemoteModelPrice, RemotePricing,
};
use super::commands_auth::We2aiApiError;
use super::session::{SessionError, SessionIdentity, SessionManager, We2aiSessionState};

/// `/api/v1/keys` 每页条数（SubPanel `ParsePagination` 上限 1000）。
pub(super) const KEYS_PAGE_SIZE: u32 = 100;
/// 分页安全上限：100 × 50 = 5000 个 Key，远超正常账号规模；服务端若返回
/// 异常的 `pages` 也不会无限循环。
pub(super) const KEYS_MAX_PAGES: u32 = 50;

/// 客户端能写入的三个工具（方案第 0 节决策 4）。B1 返回其他值时忽略。
pub const KNOWN_TOOLS: [&str; 3] = ["claude_code", "codex", "workbuddy"];

/// 前端展示的 Key 状态（方案 3.3 节：只展示这两种）。
const VISIBLE_STATUSES: [&str; 2] = ["active", "quota_exhausted"];

/// 进程内缓存的 Key 列表（含明文），与拉取时的会话身份绑定。
struct KeyCache {
    identity: SessionIdentity,
    keys: Vec<RemoteApiKey>,
}

/// Key 管理页（`key_manage.rs`）的明文缓存：含全部状态的 Key（含已禁用、
/// 已过期），只用于「复制」，不参与模型广场选择与写工具，因此与 [`KeyCache`]
/// 分开存放，不改变模型广场只列 `active`/`quota_exhausted` 的缓存语义。
/// 不派生 `Debug`，避免明文经日志泄露。
struct ManageCache {
    identity: SessionIdentity,
    secrets: HashMap<i64, String>,
}

/// 最近一次 B1 结果里各模型的可选能力，与会话身份和 Key 绑定。
type CapabilityCache = (SessionIdentity, i64, HashMap<String, ModelCapabilities>);

#[derive(Default)]
pub struct We2aiKeyState {
    cache: Mutex<Option<KeyCache>>,
    capabilities: Mutex<Option<CapabilityCache>>,
    /// 登出时数据库供应商行或备份里的 Key 未能清除，等待重试。
    material_cleanup_pending: std::sync::atomic::AtomicBool,
    /// 列表拉取序号：每次拉取开始时领取，只有比已写入缓存的序号更新的结果
    /// 才能写缓存，避免较早发起、较晚完成的拉取覆盖较新的列表（Codex P3
    /// 验收第 2 轮中危项）。
    fetch_seq: std::sync::atomic::AtomicU64,
    stored_seq: Mutex<u64>,
    /// Key 管理页明文缓存及其拉取序号（语义同上面两个字段，互不影响）。
    manage: Mutex<Option<ManageCache>>,
    manage_fetch_seq: std::sync::atomic::AtomicU64,
    manage_stored_seq: Mutex<u64>,
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
        *self.capabilities.lock().unwrap() = None;
        *self.manage.lock().unwrap() = None;
    }

    /// Key 写操作（创建/编辑/删除）成功后调用：清空模型广场 Key 缓存、B1 能力
    /// 缓存与管理页明文缓存，并让**此刻已在途**的列表拉取失效（它们读到的是
    /// 写操作之前的数据，写回缓存会让界面与服务端不一致）。之后前端重新拉取
    /// 才会重建缓存；缓存为空期间取明文一律按「不在列表中」处理。
    pub fn invalidate(&self) {
        {
            let mut cache = self.cache.lock().unwrap();
            let mut stored = self.stored_seq.lock().unwrap();
            *cache = None;
            *stored = self.fetch_seq.load(std::sync::atomic::Ordering::SeqCst);
        }
        *self.capabilities.lock().unwrap() = None;
        let mut manage = self.manage.lock().unwrap();
        let mut stored = self.manage_stored_seq.lock().unwrap();
        *manage = None;
        *stored = self
            .manage_fetch_seq
            .load(std::sync::atomic::Ordering::SeqCst);
    }

    pub(super) fn begin_manage_fetch(&self) -> u64 {
        self.manage_fetch_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1
    }

    /// 写入管理页明文缓存；`seq` 不比上次写入的新时丢弃，返回是否写入。
    pub(super) fn store_manage(
        &self,
        seq: u64,
        identity: SessionIdentity,
        secrets: HashMap<i64, String>,
    ) -> bool {
        let mut cache = self.manage.lock().unwrap();
        let mut stored = self.manage_stored_seq.lock().unwrap();
        if seq <= *stored {
            return false;
        }
        *stored = seq;
        *cache = Some(ManageCache { identity, secrets });
        true
    }

    /// 创建成功后把新 Key 的明文并入管理页缓存（按创建时的会话身份）：缓存身份一致
    /// 就追加，缓存为空或属于别的身份就以这一个 Key 重建。随后的列表拉取会整体
    /// 覆盖它，新 Key 已在其中。
    pub(super) fn insert_manage_secret(
        &self,
        identity: SessionIdentity,
        key_id: i64,
        secret: String,
    ) {
        let mut guard = self.manage.lock().unwrap();
        match &mut *guard {
            Some(cache) if cache.identity == identity => {
                cache.secrets.insert(key_id, secret);
            }
            _ => {
                *guard = Some(ManageCache {
                    identity,
                    secrets: HashMap::from([(key_id, secret)]),
                });
            }
        }
    }

    /// 管理页「复制」取明文：只有缓存身份与当前会话身份一致才返回，不一致时
    /// 顺带清空缓存（同 [`Self::secret_for`]）。
    pub(super) fn manage_secret_for(
        &self,
        current: Option<SessionIdentity>,
        key_id: i64,
    ) -> Option<String> {
        let mut guard = self.manage.lock().unwrap();
        match (&*guard, current) {
            (Some(cache), Some(current)) if cache.identity == current => {
                cache.secrets.get(&key_id).cloned()
            }
            (Some(_), _) => {
                *guard = None;
                None
            }
            (None, _) => None,
        }
    }

    pub fn material_cleanup_pending(&self) -> bool {
        self.material_cleanup_pending
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn set_material_cleanup_pending(&self, pending: bool) {
        self.material_cleanup_pending
            .store(pending, std::sync::atomic::Ordering::SeqCst);
    }

    fn store_capabilities(
        &self,
        identity: SessionIdentity,
        key_id: i64,
        caps: HashMap<String, ModelCapabilities>,
    ) {
        *self.capabilities.lock().unwrap() = Some((identity, key_id, caps));
    }

    /// 某个 Key 下某个模型的可选能力；没有对应的 B1 结果时为 `None`（写入时
    /// 省略能力字段）。
    pub fn capabilities_for(
        &self,
        current: Option<SessionIdentity>,
        key_id: i64,
        model: &str,
    ) -> Option<ModelCapabilities> {
        let guard = self.capabilities.lock().unwrap();
        match (&*guard, current) {
            (Some((identity, k, caps)), Some(current)) if *identity == current && *k == key_id => {
                caps.get(model).cloned()
            }
            _ => None,
        }
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
    /// 仅来自 `key_selection.json` 的记忆（用户显式选择过的 Key），没有记忆为
    /// `None`；与 `selected_key_id` 不同，它不会在无记忆时回退到第一个 Key，也不
    /// 校验该 Key 是否还在列表里。Key 管理页据此判断「当前工具在用」，避免无记忆
    /// 时把第一个 Key 误报成在用。`KeyListView` 不在守卫 4.5 的字段锁定范围内
    /// （只锁 `KeyView`/`We2aiKeyView`）。
    pub remembered_key_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelView {
    pub id: String,
    pub provider: Option<String>,
    pub tools: Vec<String>,
    /// B1 模型类型（`text`/`image`/`video`/`audio`），旧服务端为 `None`。
    pub kind: Option<String>,
    /// B1 定价扩展（`docs/we2ai/B1定价契约.md`）：无法解析价格的
    /// 模型为 `None`，前端显示"暂无定价"。
    pub price: Option<ModelPriceView>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KeyModelsView {
    pub models: Vec<ModelView>,
    pub callable: bool,
    pub blocked_reason: Option<String>,
    /// Key 分组缺失或倍率无法解析时为 `None`，前端整体不显示价格区。
    pub pricing: Option<PricingView>,
}

/// 顶层 `pricing.unit` 唯一接受的值（token 类）；未知单位不可信，见
/// [`to_pricing_view`]（Opus 复核 P1）。
const PRICING_UNIT_USD_PER_1M_TOKENS: &str = "usd_per_1m_tokens";

/// 前端展示用的定价倍率信息，对应 [`super::api::RemotePricing`]。核心字段
/// 只有 `cny_rate` 与 `effective_multiplier`（必须有限且 > 0，任一缺失/无效
/// 都视为整个定价不可用）；`rate_multiplier`/`peak_multiplier` 缺失或无效时
/// 各自回退到 `1.0`（"无倍率影响"），不影响整体可用性（Opus 复核 P6）。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PricingView {
    pub cny_rate: f64,
    pub rate_multiplier: f64,
    pub peak_multiplier: f64,
    pub peak_active: bool,
    pub effective_multiplier: f64,
    pub unit: String,
}

/// 前端展示用的模型定价，对应 [`super::api::RemoteModelPrice`]。已乘倍率的
/// 折后价与 `base_*` 原价都是美元，人民币换算由前端用 `PricingView.cnyRate`
/// 完成（B1 契约"客户端展示规则"）。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelPriceView {
    pub billing_mode: String,
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    pub cache_write_1h: Option<f64>,
    pub per_request: Option<f64>,
    pub base_input: Option<f64>,
    pub base_output: Option<f64>,
    pub base_cache_read: Option<f64>,
    pub base_cache_write: Option<f64>,
    pub base_cache_write_1h: Option<f64>,
    pub base_per_request: Option<f64>,
    /// v2 契约：该模型实际扣费倍率，缺失或无效（非有限/≤0）时为 `None`，
    /// 前端回退到顶层 `pricing.effectiveMultiplier`（`docs/we2ai/B1定价契约.md`）。
    pub multiplier: Option<f64>,
    /// v3 契约：按次计费单位，归一化为 `"request"`/`"second"` 之一；缺省
    /// 按 `"request"` 处理，未识别的值为 `None`（前端据此不展示按次行）。
    pub per_request_unit: Option<String>,
}

/// 有限且严格为正；核心字段（`cny_rate`/`effective_multiplier`）用这个
/// 校验，0、负数、`NaN`、`Infinity` 都视为不可信（Opus 复核 P6）。
fn finite_positive(v: Option<f64>) -> Option<f64> {
    v.filter(|x| x.is_finite() && *x > 0.0)
}

/// 只要求有限，不要求为正；用于非核心的 `rate_multiplier`/`peak_multiplier`
/// ——缺失或非有限时由调用方决定回退值，这里只负责过滤掉不可信的数值。
fn finite_only(v: Option<f64>) -> Option<f64> {
    v.filter(|x| x.is_finite())
}

/// 有限且非负；每个模型价格字段（`input`/`output`/`base_*` 等）用这个校验，
/// 负数或非有限视为该字段无法解析，单独置 `None`，不影响其余字段
/// （Opus 复核 P6）。允许为 0（免费档位）。
fn finite_nonnegative(v: Option<f64>) -> Option<f64> {
    v.filter(|x| x.is_finite() && *x >= 0.0)
}

/// 顶层 `pricing`：
/// - `unit` 校验（Opus 复核 P1）：契约里 token 类顶层单位固定为
///   `"usd_per_1m_tokens"`；显式给出但不是这个值 → 不可信，整个 `pricing`
///   视为不可用；缺省视为默认兼容，仍按 `"usd_per_1m_tokens"` 处理。
/// - 核心字段（Opus 复核 P6）：只有 `cny_rate` 与 `effective_multiplier` 是
///   必需的，且必须有限且 > 0；任一缺失/无效则整个 `pricing` 视为不可用。
/// - `rate_multiplier`/`peak_multiplier` 不再是必需字段，缺失或无效时各自
///   回退到 `1.0`；`peak_active` 显式给出就用显式值，缺省按未经默认值填充
///   的 `peak_multiplier` 是否 `> 1.0` 推导，`peak_multiplier` 也缺省（或
///   无效）则默认 `false`。
fn to_pricing_view(pricing: Option<RemotePricing>) -> Option<PricingView> {
    let p = pricing?;

    if let Some(u) = p.unit.as_deref() {
        if u != PRICING_UNIT_USD_PER_1M_TOKENS {
            return None;
        }
    }

    let cny_rate = finite_positive(p.cny_rate)?;
    let effective_multiplier = finite_positive(p.effective_multiplier)?;

    let peak_multiplier_raw = finite_only(p.peak_multiplier);
    let rate_multiplier = finite_only(p.rate_multiplier).unwrap_or(1.0);
    let peak_multiplier = peak_multiplier_raw.unwrap_or(1.0);
    let peak_active = p
        .peak_active
        .unwrap_or_else(|| peak_multiplier_raw.is_some_and(|m| m > 1.0));

    Some(PricingView {
        cny_rate,
        rate_multiplier,
        peak_multiplier,
        peak_active,
        effective_multiplier,
        unit: PRICING_UNIT_USD_PER_1M_TOKENS.to_string(),
    })
}

/// v3 契约：按次计费单位缺省按 `"request"` 处理；服务端给出未识别的值
/// （既不是 `"request"` 也不是 `"second"`）视为不可信，返回 `None`——
/// 前端据此不展示按次这一行，避免展示错误单位（追加需求，随 Q1–Q6 一并
/// 完成）。
/// `unit` 的类型是 `Option<Option<String>>`（Codex 复验 C4），区分三种
/// 情形：
/// - 外层 `None`：字段在 JSON 里完全不存在（服务端 `omitempty` 语义下
///   没有这个 key）→ 缺省按 `"request"` 处理。
/// - 外层 `Some`、内层 `None`：字段存在但显式给了 `null` → 和"未识别的
///   字符串"一样不可信（服务端不应该发这个，出现即属异常数据），不展示
///   按次这一行，不能默认成 `"request"`。
/// - 外层 `Some`、内层 `Some("")`：显式空字符串（Codex 验收 C2）——同样
///   不可信，处理方式与显式 `null` 一致。
/// - 外层 `Some`、内层 `Some("request"/"second")`：原样保留；其余字符串
///   视为未识别。
fn normalize_per_request_unit(unit: Option<Option<String>>) -> Option<String> {
    match unit {
        None => Some("request".to_string()),
        Some(None) => None,
        Some(Some(ref s)) if s.is_empty() => None,
        Some(Some(ref s)) if s == "request" => Some("request".to_string()),
        Some(Some(ref s)) if s == "second" => Some("second".to_string()),
        Some(Some(_)) => None,
    }
}

fn to_price_view(price: Option<RemoteModelPrice>) -> Option<ModelPriceView> {
    let p = price?;
    Some(ModelPriceView {
        // 缺省或空字符串一律按 "token" 处理（Opus 复核 Q4）：这是唯一的
        // 归一化点，前端不需要再对空字符串做特殊判断，直接按
        // `billingMode !== "token"` 分支即可。
        billing_mode: p
            .billing_mode
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "token".to_string()),
        input: finite_nonnegative(p.input),
        output: finite_nonnegative(p.output),
        cache_read: finite_nonnegative(p.cache_read),
        cache_write: finite_nonnegative(p.cache_write),
        cache_write_1h: finite_nonnegative(p.cache_write_1h),
        per_request: finite_nonnegative(p.per_request),
        base_input: finite_nonnegative(p.base_input),
        base_output: finite_nonnegative(p.base_output),
        base_cache_read: finite_nonnegative(p.base_cache_read),
        base_cache_write: finite_nonnegative(p.base_cache_write),
        base_cache_write_1h: finite_nonnegative(p.base_cache_write_1h),
        base_per_request: finite_nonnegative(p.base_per_request),
        multiplier: finite_positive(p.multiplier),
        per_request_unit: normalize_per_request_unit(p.per_request_unit),
    })
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
    let pricing = to_pricing_view(remote.pricing);
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
            kind: m.kind.filter(|k| !k.is_empty()),
            price: to_price_view(m.price),
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
        pricing,
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

/// 删除 Key 后清掉指向它的选择记忆（只清命中的那一项）。记忆只是界面便利，
/// 写失败不影响删除结果：`choose_selected` 对已不在列表里的记忆本来就会回退。
pub(super) fn clear_selection_if(data_root: &Path, identity: &SessionIdentity, key_id: i64) {
    let mut data = KeySelectionFile::read(data_root);
    let slot = KeySelectionFile::slot(identity);
    if data.selections.get(&slot) != Some(&key_id) {
        return;
    }
    data.selections.remove(&slot);
    let json = serde_json::to_string_pretty(&data).unwrap_or_default();
    if let Err(e) =
        crate::config::atomic_write_private(&KeySelectionFile::path(data_root), json.as_bytes())
    {
        log::warn!("清除已删除 Key 的选择记忆失败: {e}");
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
    fetch_all_pages(manager, max_pages, |api, token, page| async move {
        api.list_keys(&token, page, KEYS_PAGE_SIZE).await
    })
    .await
}

/// 分页拉取的公共实现，模型广场（`list_keys`）与 Key 管理页
/// （`key_manage.rs`）共用：每页走 `call_protected_api`，各页身份一致，到页数
/// 上限仍未到最后一页返回 [`KEY_LIST_TOO_LARGE`]。
pub(super) async fn fetch_all_pages<T, F, Fut>(
    manager: &SessionManager,
    max_pages: u32,
    fetch: F,
) -> Result<(SessionIdentity, Vec<T>), SessionError>
where
    F: Fn(ApiClient, String, u32) -> Fut,
    Fut: std::future::Future<Output = Result<Paginated<T>, ApiCallError>>,
{
    let mut identity: Option<SessionIdentity> = None;
    let mut all = Vec::new();
    let mut page = 1u32;
    loop {
        let fetch_ref = &fetch;
        let (data, page_identity) = manager
            .call_protected_api(true, move |api, token| fetch_ref(api, token, page))
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
        remembered_key_id: remembered,
    })
}

pub(super) fn unknown_key_error() -> We2aiApiError {
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
    let caps = remote
        .models
        .iter()
        .map(|m| (m.id.clone(), m.capabilities()))
        .collect();
    state.store_capabilities(call_identity, key_id, caps);
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
        // 没有任何记忆：回退到第一个，但 remembered_key_id 为空（不能把默认项当「在用」）。
        let first = list_keys(&manager, &state).await.unwrap();
        assert_eq!(first.selected_key_id, Some(1));
        assert_eq!(first.remembered_key_id, None);

        select_key(&manager, &state, 3).unwrap();
        let after = list_keys(&manager, &state).await.unwrap();
        assert_eq!(after.selected_key_id, Some(3));
        assert_eq!(after.remembered_key_id, Some(3));

        // 不在列表里的 Key 不能被选中，也不发任何请求。
        let err = select_key(&manager, &state, 99).unwrap_err();
        assert_eq!(err.code, "KEY_NOT_FOUND");

        // 另一个账号（同区域）没有记忆，取第一个，且看不到上一个账号的记忆。
        manager.test_seed_active(Region::International, 7, "access-1", server.uri());
        let other = list_keys(&manager, &state).await.unwrap();
        assert_eq!(other.selected_key_id, Some(1));
        assert_eq!(other.remembered_key_id, None);

        // 记住的 Key 被删除后回退到第一个。
        let mut data = KeySelectionFile::read(dir.path());
        data.selections.insert("international:7".to_string(), 12345);
        std::fs::write(
            KeySelectionFile::path(dir.path()),
            serde_json::to_string(&data).unwrap(),
        )
        .unwrap();
        let stale = list_keys(&manager, &state).await.unwrap();
        assert_eq!(stale.selected_key_id, Some(1));
        assert_eq!(
            stale.remembered_key_id,
            Some(12345),
            "记忆原样返回，不校验是否还在列表里"
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
                    {"id": "claude-sonnet-4-5", "provider": "anthropic", "tools": ["workbuddy", "claude_code", "gemini_cli"], "supports_tool_call": true, "reasoning_efforts": ["low", "high"]},
                    {"id": "gpt-5", "tools": []}
                ], "callable": true, "blocked_reason": "IGNORED"}
            })))
            .expect(2)
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

        // 可选能力按 Key 缓存：最近一次查询的是 Key 3，Key 1 的能力不再可取。
        let identity = manager.current_identity();
        assert_eq!(
            state.capabilities_for(identity, 1, "claude-sonnet-4-5"),
            None
        );
        key_models(&manager, &state, 1).await.ok();
        let caps = state
            .capabilities_for(identity, 1, "claude-sonnet-4-5")
            .expect("capabilities cached for key 1");
        assert_eq!(caps.supports_tool_call, Some(true));
        assert_eq!(caps.supports_images, None);
        assert_eq!(
            caps.reasoning_efforts,
            Some(vec!["low".to_string(), "high".to_string()])
        );
        assert_eq!(
            state.capabilities_for(identity, 3, "claude-sonnet-4-5"),
            None
        );
    }

    /// B1 定价扩展：`pricing` 与每个模型的 `price` 原样透传给前端视图，
    /// `base_*` 与折后价分开保留，供前端在倍率 ≠ 1 时显示划线原价。
    #[tokio::test]
    async fn key_models_passes_through_pricing_and_model_price() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        mount_page(&server, 1, 1, vec![key_json(1, SECRET_A, "active", None)]).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/desktop/keys/1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {
                    "models": [{
                        "id": "claude-sonnet-4-5",
                        "provider": "anthropic",
                        "tools": ["claude_code"],
                        "price": {
                            "billing_mode": "token",
                            "input": 3.0,
                            "output": 15.0,
                            "cache_read": 0.3,
                            "base_input": 6.0,
                            "base_output": 30.0,
                            "base_cache_read": 0.6
                        }
                    }],
                    "callable": true,
                    "pricing": {
                        "cny_rate": 7.2,
                        "rate_multiplier": 0.5,
                        "peak_multiplier": 1.0,
                        "peak_active": false,
                        "effective_multiplier": 0.5,
                        "unit": "usd_per_1m_tokens"
                    }
                }
            })))
            .mount(&server)
            .await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        let state = We2aiKeyState::default();
        list_keys(&manager, &state).await.unwrap();

        let result = key_models(&manager, &state, 1).await.unwrap();
        let pricing = result.pricing.expect("pricing present");
        assert_eq!(pricing.cny_rate, 7.2);
        assert_eq!(pricing.effective_multiplier, 0.5);
        assert!(!pricing.peak_active);
        let price = result.models[0].price.as_ref().expect("price present");
        assert_eq!(price.billing_mode, "token");
        assert_eq!(price.input, Some(3.0));
        assert_eq!(price.base_input, Some(6.0));
        assert_eq!(price.cache_write, None);
        assert_eq!(price.multiplier, None, "fixture omits model-level multiplier");
    }

    /// B1 `kind` 透传给前端视图：有值原样保留，缺失（旧服务端）或空串为 `None`。
    #[tokio::test]
    async fn key_models_passes_through_kind_and_tolerates_missing() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        mount_page(&server, 1, 1, vec![key_json(1, SECRET_A, "active", None)]).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/desktop/keys/1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {
                    "models": [
                        {"id": "claude-sonnet-4-5", "tools": ["claude_code"], "mode": "chat", "kind": "text"},
                        {"id": "jimeng_t2v_v30", "tools": ["codex"], "kind": "video"},
                        {"id": "legacy-model", "tools": ["codex"]},
                        {"id": "empty-kind", "tools": ["codex"], "kind": ""}
                    ],
                    "callable": true
                }
            })))
            .mount(&server)
            .await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        let state = We2aiKeyState::default();
        list_keys(&manager, &state).await.unwrap();

        let result = key_models(&manager, &state, 1).await.unwrap();
        let kinds: Vec<Option<&str>> = result.models.iter().map(|m| m.kind.as_deref()).collect();
        assert_eq!(kinds, vec![Some("text"), Some("video"), None, None]);
        let json = serde_json::to_value(&result.models[1]).unwrap();
        assert_eq!(
            json["kind"], "video",
            "serialized to the frontend as `kind`"
        );
    }

    /// v2 契约端到端：image 类模型自带独立倍率（0.5，不叠加高峰），与顶层
    /// `pricing.effective_multiplier`（2.0，来自分组高峰）不同，两者都要
    /// 原样透传给前端，不能互相覆盖——前端据此决定用哪一个判定划线/加价。
    #[tokio::test]
    async fn key_models_keeps_model_level_multiplier_distinct_from_top_level() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        mount_page(&server, 1, 1, vec![key_json(1, SECRET_A, "active", None)]).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/desktop/keys/1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {
                    "models": [{
                        "id": "image-gen-1",
                        "tools": ["codex"],
                        "price": {
                            "billing_mode": "image",
                            "per_request": 0.02,
                            "base_per_request": 0.04,
                            "multiplier": 0.5
                        }
                    }],
                    "callable": true,
                    "pricing": {
                        "cny_rate": 7.2,
                        "effective_multiplier": 2.0
                    }
                }
            })))
            .mount(&server)
            .await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        let state = We2aiKeyState::default();
        list_keys(&manager, &state).await.unwrap();

        let result = key_models(&manager, &state, 1).await.unwrap();
        let pricing = result.pricing.expect("pricing present");
        assert_eq!(pricing.effective_multiplier, 2.0);
        let price = result.models[0].price.as_ref().expect("price present");
        assert_eq!(price.billing_mode, "image");
        assert_eq!(
            price.multiplier,
            Some(0.5),
            "model's own multiplier must not be replaced by the top-level one"
        );
    }

    /// v3 契约端到端：视频类模型按秒计费，`per_request_unit` 原样透传给
    /// 前端视图（"second"）。
    #[tokio::test]
    async fn key_models_passes_through_per_request_unit_for_a_video_model() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        mount_page(&server, 1, 1, vec![key_json(1, SECRET_A, "active", None)]).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/desktop/keys/1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {
                    "models": [{
                        "id": "video-gen-1",
                        "tools": ["codex"],
                        "price": {
                            "billing_mode": "video",
                            "per_request": 0.5,
                            "per_request_unit": "second"
                        }
                    }],
                    "callable": true,
                    "pricing": {"cny_rate": 7.2, "effective_multiplier": 1.0}
                }
            })))
            .mount(&server)
            .await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        let state = We2aiKeyState::default();
        list_keys(&manager, &state).await.unwrap();

        let result = key_models(&manager, &state, 1).await.unwrap();
        let price = result.models[0].price.as_ref().expect("price present");
        assert_eq!(price.per_request_unit, Some("second".to_string()));
    }

    /// 旧服务端（未实现 B1 定价扩展）响应里没有 `pricing`/`price`：解析
    /// 不报错，视图里两者都为 `None`，前端据此不显示价格区（B1 契约"客户端
    /// 展示规则"）。
    #[tokio::test]
    async fn key_models_omits_pricing_when_old_server_does_not_provide_it() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        mount_page(&server, 1, 1, vec![key_json(1, SECRET_A, "active", None)]).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/desktop/keys/1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {"models": [{"id": "gpt-5", "tools": ["codex"]}], "callable": true}
            })))
            .mount(&server)
            .await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        let state = We2aiKeyState::default();
        list_keys(&manager, &state).await.unwrap();

        let result = key_models(&manager, &state, 1).await.unwrap();
        assert!(result.pricing.is_none());
        assert!(result.models[0].price.is_none());
    }

    /// 顶层 `pricing` 缺 `effective_multiplier`（唯二必需字段之一）时，整个
    /// `pricing` 视为不可用，而不是把半份数据交给前端（Opus 复核 P6：只有
    /// `cny_rate`/`effective_multiplier` 是必需字段，`rate_multiplier` 单独
    /// 缺失不会触发这个整体降级，见下面 `to_pricing_view_*` 系列纯函数测试）。
    #[tokio::test]
    async fn key_models_drops_pricing_when_effective_multiplier_is_missing() {
        let dir = TempDir::new().unwrap();
        let manager = manager(&dir);
        let server = MockServer::start().await;
        mount_page(&server, 1, 1, vec![key_json(1, SECRET_A, "active", None)]).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/desktop/keys/1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0, "message": "success",
                "data": {
                    "models": [{"id": "gpt-5", "tools": ["codex"]}],
                    "callable": true,
                    "pricing": {"cny_rate": 7.2}
                }
            })))
            .mount(&server)
            .await;
        manager.test_seed_active(Region::International, 42, "access-1", server.uri());
        let state = We2aiKeyState::default();
        list_keys(&manager, &state).await.unwrap();

        let result = key_models(&manager, &state, 1).await.unwrap();
        assert!(result.pricing.is_none());
    }

    // ── to_pricing_view / to_price_view 纯函数测试（Opus 复核 P1/P6） ──────

    fn base_pricing() -> RemotePricing {
        RemotePricing {
            cny_rate: Some(7.2),
            rate_multiplier: Some(0.5),
            peak_multiplier: Some(1.0),
            peak_active: Some(false),
            effective_multiplier: Some(0.5),
            unit: Some(PRICING_UNIT_USD_PER_1M_TOKENS.to_string()),
        }
    }

    /// P1：unit 显式给出且不是契约里的 token 类单位 → 整个 pricing 不可信。
    #[test]
    fn to_pricing_view_rejects_unknown_unit() {
        let pricing = RemotePricing {
            unit: Some("usd_per_request".to_string()),
            ..base_pricing()
        };
        assert!(to_pricing_view(Some(pricing)).is_none());
    }

    /// P1：unit 缺省视为默认兼容（旧服务端未实现该字段的场景）。
    #[test]
    fn to_pricing_view_defaults_missing_unit_to_token_unit() {
        let pricing = RemotePricing {
            unit: None,
            ..base_pricing()
        };
        let view = to_pricing_view(Some(pricing)).expect("unit missing is compatible");
        assert_eq!(view.unit, PRICING_UNIT_USD_PER_1M_TOKENS);
    }

    /// P1：unit 与契约值完全一致时正常通过。
    #[test]
    fn to_pricing_view_accepts_known_unit() {
        assert!(to_pricing_view(Some(base_pricing())).is_some());
    }

    /// P6：`rate_multiplier`/`peak_multiplier` 均缺失时不再让整个 pricing
    /// 不可用，各自回退到 1.0，`peak_active` 缺省按 `peak_multiplier` 推导
    /// （缺省时视为 false）。
    #[test]
    fn to_pricing_view_defaults_non_core_multipliers_when_missing() {
        let pricing = RemotePricing {
            cny_rate: Some(7.2),
            rate_multiplier: None,
            peak_multiplier: None,
            peak_active: None,
            effective_multiplier: Some(1.0),
            unit: None,
        };
        let view = to_pricing_view(Some(pricing)).expect("only cny_rate/effective_multiplier required");
        assert_eq!(view.rate_multiplier, 1.0);
        assert_eq!(view.peak_multiplier, 1.0);
        assert!(!view.peak_active);
    }

    /// P6：`peak_active` 未显式给出时按 `peak_multiplier > 1.0` 推导为真。
    #[test]
    fn to_pricing_view_derives_peak_active_from_peak_multiplier() {
        let pricing = RemotePricing {
            peak_active: None,
            peak_multiplier: Some(1.5),
            ..base_pricing()
        };
        let view = to_pricing_view(Some(pricing)).unwrap();
        assert!(view.peak_active);
    }

    /// P6：显式给出的 `peak_active` 优先于按 `peak_multiplier` 的推导。
    #[test]
    fn to_pricing_view_prefers_explicit_peak_active_over_derivation() {
        let pricing = RemotePricing {
            peak_active: Some(false),
            peak_multiplier: Some(2.0),
            ..base_pricing()
        };
        let view = to_pricing_view(Some(pricing)).unwrap();
        assert!(!view.peak_active, "explicit false must not be overridden");
    }

    /// P6：`cny_rate`/`effective_multiplier` 必须有限且 > 0；0、负数、非有限
    /// 都视为不可信，整个 pricing 不可用。
    #[test]
    fn to_pricing_view_requires_core_fields_finite_and_positive() {
        for cny_rate in [Some(0.0), Some(-1.0), Some(f64::NAN), Some(f64::INFINITY), None] {
            let pricing = RemotePricing {
                cny_rate,
                ..base_pricing()
            };
            assert!(
                to_pricing_view(Some(pricing)).is_none(),
                "cny_rate={cny_rate:?} should be rejected"
            );
        }
        for effective_multiplier in [Some(0.0), Some(-0.5), Some(f64::NAN), None] {
            let pricing = RemotePricing {
                effective_multiplier,
                ..base_pricing()
            };
            assert!(
                to_pricing_view(Some(pricing)).is_none(),
                "effective_multiplier={effective_multiplier:?} should be rejected"
            );
        }
    }

    /// P6：单个价格字段为负数或非有限时该字段单独置 None，不影响其余字段，
    /// 也不影响整个 price 对象的可用性（billing_mode 等仍然正常返回）。
    #[test]
    fn to_price_view_drops_individual_invalid_numeric_fields() {
        let price = RemoteModelPrice {
            billing_mode: Some("token".to_string()),
            input: Some(-1.0),
            output: Some(f64::NAN),
            cache_read: Some(f64::INFINITY),
            base_input: Some(6.0),
            ..Default::default()
        };
        let view = to_price_view(Some(price)).expect("price object itself still available");
        assert_eq!(view.billing_mode, "token");
        assert_eq!(view.input, None, "negative price must be dropped");
        assert_eq!(view.output, None, "NaN price must be dropped");
        assert_eq!(view.cache_read, None, "infinite price must be dropped");
        assert_eq!(view.base_input, Some(6.0), "valid fields stay untouched");
    }

    /// P6：0 是合法价格（免费档位），不应被当成无效值丢弃。
    #[test]
    fn to_price_view_keeps_zero_as_a_valid_price() {
        let price = RemoteModelPrice {
            input: Some(0.0),
            ..Default::default()
        };
        let view = to_price_view(Some(price)).unwrap();
        assert_eq!(view.input, Some(0.0));
    }

    /// Opus 复核 Q4：`billing_mode` 缺省（`None`）或服务端给了空字符串都
    /// 归一化为 `"token"`，前端不需要再对空字符串做特殊判断。
    #[test]
    fn to_price_view_normalizes_missing_or_empty_billing_mode_to_token() {
        let missing = RemoteModelPrice {
            billing_mode: None,
            ..Default::default()
        };
        assert_eq!(to_price_view(Some(missing)).unwrap().billing_mode, "token");

        let empty = RemoteModelPrice {
            billing_mode: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(to_price_view(Some(empty)).unwrap().billing_mode, "token");
    }

    /// v2 契约：模型级 `multiplier` 有效（有限且 > 0）时原样保留，供前端
    /// 优先于顶层 `pricing.effectiveMultiplier` 使用。
    #[test]
    fn to_price_view_keeps_a_valid_model_level_multiplier() {
        let price = RemoteModelPrice {
            multiplier: Some(0.5),
            ..Default::default()
        };
        let view = to_price_view(Some(price)).unwrap();
        assert_eq!(view.multiplier, Some(0.5));
    }

    /// v2 契约：模型级 `multiplier` 无效（0、负数、非有限）时置 `None`，前端
    /// 据此回退到顶层倍率，而不是把不可信的值透传出去。
    #[test]
    fn to_price_view_drops_invalid_model_level_multiplier() {
        for multiplier in [Some(0.0), Some(-1.0), Some(f64::NAN), Some(f64::INFINITY)] {
            let price = RemoteModelPrice {
                multiplier,
                ..Default::default()
            };
            let view = to_price_view(Some(price)).unwrap();
            assert_eq!(view.multiplier, None, "multiplier={multiplier:?} must be dropped");
        }
    }

    /// v3 契约：`per_request_unit` 缺省按 `"request"` 处理；显式给出
    /// `"request"`/`"second"` 原样保留；未识别的值归一化为 `None`（前端
    /// 据此不展示按次行，避免展示错误单位）。
    #[test]
    fn to_price_view_normalizes_per_request_unit() {
        // 字段完全不存在（外层 None）：缺省按 "request" 处理。
        let missing = RemoteModelPrice {
            per_request_unit: None,
            ..Default::default()
        };
        assert_eq!(
            to_price_view(Some(missing)).unwrap().per_request_unit,
            Some("request".to_string())
        );

        let request = RemoteModelPrice {
            per_request_unit: Some(Some("request".to_string())),
            ..Default::default()
        };
        assert_eq!(
            to_price_view(Some(request)).unwrap().per_request_unit,
            Some("request".to_string())
        );

        let second = RemoteModelPrice {
            per_request_unit: Some(Some("second".to_string())),
            ..Default::default()
        };
        assert_eq!(
            to_price_view(Some(second)).unwrap().per_request_unit,
            Some("second".to_string())
        );

        let unknown = RemoteModelPrice {
            per_request_unit: Some(Some("hour".to_string())),
            ..Default::default()
        };
        assert_eq!(to_price_view(Some(unknown)).unwrap().per_request_unit, None);

        // Codex 验收 C2（撤销上一轮 R4）：显式给出的空字符串不等同于
        // 缺省，服务端 `omitempty` 不会主动发出空串，出现即属异常数据，
        // 按"未识别的值"处理（宁可不显示按次行，也不能把秒价当次价）。
        let empty = RemoteModelPrice {
            per_request_unit: Some(Some(String::new())),
            ..Default::default()
        };
        assert_eq!(to_price_view(Some(empty)).unwrap().per_request_unit, None);

        // Codex 复验 C4：字段存在但显式给了 null（外层 Some、内层
        // None）——和未识别的字符串一样不可信，不能默认成 "request"。
        let explicit_null = RemoteModelPrice {
            per_request_unit: Some(None),
            ..Default::default()
        };
        assert_eq!(
            to_price_view(Some(explicit_null)).unwrap().per_request_unit,
            None
        );
    }

    // Codex 复验 C4：直接从原始 JSON 反序列化，验证"字段缺失"/"显式
    // null"/"显式空字符串"三种情形在 wire 层面就已经被正确区分——不是只
    // 在手工构造的 `RemoteModelPrice` 上验证 `to_price_view` 的行为，
    // 而是覆盖 serde 反序列化这一步本身（这正是上一轮 bug 的根源：
    // `Option<String>` 字段类型下，serde 会把"缺失"和"显式 null"都解析
    // 成同一个 `None`，反序列化这一步就已经丢失了区分）。
    #[test]
    fn remote_model_price_distinguishes_missing_null_and_empty_per_request_unit() {
        let missing: RemoteModelPrice =
            serde_json::from_value(json!({"per_request": 0.5})).unwrap();
        assert_eq!(
            missing.per_request_unit, None,
            "field entirely absent must deserialize to the outer None"
        );

        let explicit_null: RemoteModelPrice =
            serde_json::from_value(json!({"per_request": 0.5, "per_request_unit": null}))
                .unwrap();
        assert_eq!(
            explicit_null.per_request_unit,
            Some(None),
            "an explicit JSON null must deserialize to Some(None), distinct from the field being absent"
        );

        let empty: RemoteModelPrice =
            serde_json::from_value(json!({"per_request": 0.5, "per_request_unit": ""})).unwrap();
        assert_eq!(empty.per_request_unit, Some(Some(String::new())));

        // 三种 wire 形态送进 to_price_view 之后必须都不可信（None），
        // 只有"字段缺失"才归一化成 "request"。
        assert_eq!(
            to_price_view(Some(missing)).unwrap().per_request_unit,
            Some("request".to_string())
        );
        assert_eq!(
            to_price_view(Some(explicit_null)).unwrap().per_request_unit,
            None
        );
        assert_eq!(to_price_view(Some(empty)).unwrap().per_request_unit, None);
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
