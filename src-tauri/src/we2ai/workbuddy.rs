//! WorkBuddy 写入（方案第 4.3 节）。
//!
//! `models.json` 是顶层数组，一个条目 = 一个模型（以 `id` 作模型名）。WE2AI
//! 记录上次托管条目的 `id` 与内容指纹（规范化 JSON 的 SHA-256），切换时：
//!
//! | 情形 | 处理 |
//! |---|---|
//! | 新旧 id 不同，旧条目指纹一致 | 删除旧条目，写入新条目 |
//! | 新旧 id 不同，旧条目被改过 | 保留旧条目并提示，写入新条目 |
//! | 新旧 id 相同，指纹一致 | 原地替换 |
//! | 新旧 id 相同但被改过，或与非托管条目重名 | 需用户确认覆盖，否则不写 |
//!
//! 写入前后比对文件哈希，变化则重读重算（最多 3 次）；WorkBuddy 不使用文件
//! 锁，最终比对与替换之间的窗口无法消除，属尽力检测。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::apply::{ApplyError, ApplyOutcome, ApplyParams, ERR_FAILED};
use super::snapshot::{hash_file, FileSnapshot, RestoreResult};

pub const MODELS_FILE: &str = "models.json";
const RECORD_FILE: &str = "workbuddy_managed.json";
const MAX_ATTEMPTS: usize = 3;

pub const ERR_CONFIRM: &str = "WORKBUDDY_CONFIRM_OVERWRITE";
pub const ERR_CONCURRENT: &str = "WORKBUDDY_CONCURRENT_MODIFICATION";

/// `$WORKBUDDY_CONFIG_DIR` → `$CODEBUDDY_CONFIG_DIR` → `<home>/.workbuddy`。
/// home 用 `get_home_dir()`（`dirs::home_dir()`），不读 `HOME`：Windows 上
/// Git/MSYS 注入的 `HOME` 可能不是用户目录。
pub fn config_dir() -> PathBuf {
    for var in ["WORKBUDDY_CONFIG_DIR", "CODEBUDDY_CONFIG_DIR"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim();
            if !v.is_empty() {
                return PathBuf::from(v);
            }
        }
    }
    crate::config::get_home_dir().join(".workbuddy")
}

pub fn models_path() -> PathBuf {
    config_dir().join(MODELS_FILE)
}

/// WE2AI 自有的托管记录（`~/.we2ai/workbuddy_managed.json`，0600，不含 Key）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedRecord {
    pub id: String,
    pub fingerprint: String,
}

fn record_path(data_root: &Path) -> PathBuf {
    data_root.join(RECORD_FILE)
}

pub fn load_record(data_root: &Path) -> Option<ManagedRecord> {
    std::fs::read_to_string(record_path(data_root))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

fn save_record(data_root: &Path, record: &ManagedRecord) -> Result<(), crate::error::AppError> {
    let json = serde_json::to_string_pretty(record).unwrap_or_default();
    crate::config::atomic_write_private(&record_path(data_root), json.as_bytes())
}

/// 规范化：对象键递归排序后序列化（本 crate 开启了 serde_json 的
/// `preserve_order`，不能依赖默认顺序）。
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = Map::new();
            for k in keys {
                out.insert(k.clone(), canonical(&map[k]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

pub fn fingerprint(entry: &Value) -> String {
    let text = serde_json::to_string(&canonical(entry)).unwrap_or_default();
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn build_entry(params: &ApplyParams) -> Value {
    let mut entry = json!({
        "id": params.model,
        "name": format!("WE2AI {}", params.model),
        "vendor": "Custom",
        "url": format!("{}/v1", params.gateway_root.trim_end_matches('/')),
        "apiKey": params.api_key,
        "useCustomProtocol": false,
    });
    // 能力字段仅在 B1 返回时写入，未返回则省略（方案 4.3 节键名映射）。
    if let Some(caps) = &params.capabilities {
        let obj = entry.as_object_mut().expect("entry is object");
        if let Some(v) = caps.supports_tool_call {
            obj.insert("supportsToolCall".into(), json!(v));
        }
        if let Some(v) = caps.supports_images {
            obj.insert("supportsImages".into(), json!(v));
        }
        if let Some(efforts) = caps.reasoning_efforts.as_ref().filter(|e| !e.is_empty()) {
            obj.insert("supportsReasoning".into(), json!(true));
            obj.insert("reasoning".into(), json!({ "supportedEfforts": efforts }));
        }
    }
    entry
}

fn entry_id(v: &Value) -> Option<&str> {
    v.get("id").and_then(Value::as_str)
}

enum Plan {
    Write {
        items: Vec<Value>,
        warnings: Vec<String>,
    },
    NeedsConfirm,
}

fn plan_update(
    mut items: Vec<Value>,
    record: Option<&ManagedRecord>,
    entry: &Value,
    overwrite: bool,
) -> Plan {
    let new_id = entry_id(entry).unwrap_or_default().to_string();
    let mut warnings = Vec::new();

    let target = items
        .iter()
        .position(|v| entry_id(v) == Some(new_id.as_str()));
    if let Some(i) = target {
        let managed_unchanged = record
            .map(|r| r.id == new_id && fingerprint(&items[i]) == r.fingerprint)
            .unwrap_or(false);
        if !managed_unchanged && !overwrite {
            return Plan::NeedsConfirm;
        }
    }

    if let Some(rec) = record.filter(|r| r.id != new_id) {
        if let Some(i) = items
            .iter()
            .position(|v| entry_id(v) == Some(rec.id.as_str()))
        {
            if fingerprint(&items[i]) == rec.fingerprint {
                items.remove(i);
            } else {
                warnings.push(format!(
                    "检测到 WorkBuddy 中 {} 条目被手工修改，未删除",
                    rec.id
                ));
            }
        }
    }

    match items
        .iter()
        .position(|v| entry_id(v) == Some(new_id.as_str()))
    {
        Some(i) => items[i] = entry.clone(),
        None => items.push(entry.clone()),
    }
    Plan::Write { items, warnings }
}

fn read_items(path: &Path) -> Result<Vec<Value>, ApplyError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(ApplyError::new(
                ERR_FAILED,
                format!("读取 {} 失败：{e}", path.display()),
            ))
        }
    };
    parse_items(path, bytes.as_deref())
}

fn parse_items(path: &Path, bytes: Option<&[u8]>) -> Result<Vec<Value>, ApplyError> {
    let Some(bytes) = bytes else {
        return Ok(Vec::new());
    };
    let text = String::from_utf8_lossy(bytes);
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Array(items)) => Ok(items),
        Ok(_) => Err(ApplyError::new(
            ERR_FAILED,
            format!("{} 顶层不是数组，未写入", path.display()),
        )),
        Err(e) => Err(ApplyError::new(
            ERR_FAILED,
            format!("{} 不是有效的 JSON，未写入：{e}", path.display()),
        )),
    }
}

#[cfg(test)]
thread_local! {
    /// 测试钩子：读取之后、最终比对之前执行（模拟其他写入者）。
    static BEFORE_FINAL_COMPARE: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
    /// 测试钩子：写托管记录时注入失败。
    static FAIL_RECORD_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// 测试钩子：models.json 已经写盘、H1 已经从内存字节记下之后，写托管
    /// 记录之前执行（模拟这个窗口里发生的外部改写，P6 四轮 Opus 复核高危
    /// 项 2）。
    static AFTER_WRITE_BEFORE_RECORD: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_before_final_compare(f: impl FnMut() + 'static) {
    BEFORE_FINAL_COMPARE.with(|h| *h.borrow_mut() = Some(Box::new(f)));
}

#[cfg(test)]
pub(crate) fn set_fail_record_write(fail: bool) {
    FAIL_RECORD_WRITE.with(|c| c.set(fail));
}

#[cfg(test)]
pub(crate) fn set_after_write_before_record(f: impl FnMut() + 'static) {
    AFTER_WRITE_BEFORE_RECORD.with(|h| *h.borrow_mut() = Some(Box::new(f)));
}

#[cfg(test)]
pub(crate) fn clear_test_hooks() {
    BEFORE_FINAL_COMPARE.with(|h| *h.borrow_mut() = None);
    FAIL_RECORD_WRITE.with(|c| c.set(false));
    AFTER_WRITE_BEFORE_RECORD.with(|h| *h.borrow_mut() = None);
}

pub fn apply_workbuddy(
    data_root: &Path,
    params: &ApplyParams,
    overwrite: bool,
    still_current: &dyn Fn() -> bool,
) -> Result<ApplyOutcome, ApplyError> {
    let _lock = super::apply::apply_lock();
    super::apply::ensure_session_current(still_current)?;
    super::apply::tighten_all(&crate::config::get_claude_settings_path())?;

    let path = models_path();
    let record = load_record(data_root);
    let entry = build_entry(params);
    for _ in 0..MAX_ATTEMPTS {
        // 每次重算都重新取快照：回滚只能恢复到"本次写入前"的内容，不能用第一次
        // 读取的旧内容覆盖期间其他程序的修改（Codex P4 验收第 1 轮高危项）。
        let mut models_snapshot = FileSnapshot::capture(&path).map_err(|e| {
            ApplyError::new(ERR_FAILED, format!("读取 {} 失败：{e}", path.display()))
        })?;
        let read_hash = models_snapshot.h0();
        let items = parse_items(&path, models_snapshot.original_bytes())?;
        let (items, warnings) = match plan_update(items, record.as_ref(), &entry, overwrite) {
            Plan::NeedsConfirm => {
                return Err(ApplyError::new(
                    ERR_CONFIRM,
                    format!(
                        "WorkBuddy 中已有 {} 条目且内容与 WE2AI 上次写入的不同",
                        params.model
                    ),
                ))
            }
            Plan::Write { items, warnings } => (items, warnings),
        };
        let mut bytes = serde_json::to_vec_pretty(&Value::Array(items))
            .map_err(|e| ApplyError::new(ERR_FAILED, e.to_string()))?;
        bytes.push(b'\n');

        #[cfg(test)]
        BEFORE_FINAL_COMPARE.with(|h| {
            if let Some(f) = h.borrow_mut().as_mut() {
                f();
            }
        });

        let current = hash_file(&path).map_err(|e| {
            ApplyError::new(ERR_FAILED, format!("读取 {} 失败：{e}", path.display()))
        })?;
        if current != read_hash {
            continue;
        }
        // 即将真正尝试写入：此刻 current == read_hash（H0），标记写入尝试
        // 记下的 H_pre 因此等于 H0，不会干扰后续基于 H1 的正常判定（见
        // `FileSnapshot::mark_write_attempted` 文档）。标记本身的读取失败
        // （极小概率的竞态：文件在两次读取之间被删除又权限异常等）不再被
        // 吞掉，直接中止、不写入（P6 五轮 Codex 验收高危项 2）。
        models_snapshot.mark_write_attempted().map_err(|e| {
            ApplyError::new(ERR_FAILED, format!("标记写入 {} 失败：{e}", path.display()))
        })?;
        crate::config::atomic_write_private(&path, &bytes)
            .map_err(|e| ApplyError::new(ERR_FAILED, e.to_string()))?;
        // 直接用刚刚写盘的字节记录 H1，不重新读盘：写入与记录之间如果重新
        // 读盘，一旦外部程序在这个窗口抢先改写了文件，读到的会是外部内容
        // 而不是我们真正写下的内容，H1 就会被外部编辑"认领"，导致后续恢复
        // 把这次外部编辑误判成"我们写的、可以安全回滚"而覆盖掉
        // （P6 四轮 Opus 复核高危项 2）。
        models_snapshot.record_h1_from_bytes(&bytes);

        #[cfg(test)]
        AFTER_WRITE_BEFORE_RECORD.with(|h| {
            if let Some(f) = h.borrow_mut().as_mut() {
                f();
            }
        });

        let new_record = ManagedRecord {
            id: params.model.clone(),
            fingerprint: fingerprint(&entry),
        };
        #[cfg(test)]
        let record_result = if FAIL_RECORD_WRITE.with(|c| c.get()) {
            Err(crate::error::AppError::Message(
                "注入的托管记录写入失败".into(),
            ))
        } else {
            save_record(data_root, &new_record)
        };
        #[cfg(not(test))]
        let record_result = save_record(data_root, &new_record);
        if let Err(e) = record_result {
            // 托管记录没写上：把 models.json 恢复到写入前，避免出现"文件里有
            // WE2AI 条目、记录却指向旧条目"的不一致。
            let restore = models_snapshot.restore();
            let _ = super::fsguard::tighten_file(&path);
            let suffix = match restore {
                RestoreResult::Restored | RestoreResult::Unchanged => String::new(),
                RestoreResult::ExternalModified => "；models.json 已被其他程序修改，未回滚".into(),
                RestoreResult::Failed(r) => format!("；models.json 未能恢复：{r}"),
            };
            return Err(ApplyError::new(
                ERR_FAILED,
                format!("写入托管记录失败：{e}{suffix}"),
            ));
        }
        let _ = super::fsguard::tighten_file(&path);
        return Ok(ApplyOutcome {
            model: params.model.clone(),
            files: vec![path.display().to_string()],
            warnings,
        });
    }
    Err(ApplyError::new(
        ERR_CONCURRENT,
        "WorkBuddy 的 models.json 正被其他程序频繁修改，请稍后重试",
    ))
}

/// 删除托管记录指向且未被手改的条目，并清除托管记录。返回写过的文件；条目
/// 被手改过时保留并返回原因。调用方持 apply 锁。
pub fn remove_managed_entry(data_root: &Path) -> Result<Option<String>, String> {
    let Some(record) = load_record(data_root) else {
        return Ok(None);
    };
    let path = models_path();
    let read_hash = hash_file(&path).map_err(|e| e.to_string())?;
    let items = read_items(&path).map_err(|e| e.message)?;
    let Some(index) = items
        .iter()
        .position(|v| entry_id(v) == Some(record.id.as_str()))
    else {
        let _ = std::fs::remove_file(record_path(data_root));
        return Ok(None);
    };
    if fingerprint(&items[index]) != record.fingerprint {
        return Err(format!(
            "WorkBuddy 中 {} 条目被手工修改过，未删除",
            record.id
        ));
    }
    let mut items = items;
    items.remove(index);
    let mut bytes = serde_json::to_vec_pretty(&Value::Array(items)).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    // 读后被其他程序改过则不覆盖（与 apply_workbuddy 同样的尽力检测）。
    if hash_file(&path).map_err(|e| e.to_string())? != read_hash {
        return Err("WorkBuddy 的 models.json 刚被其他程序修改，未删除 WE2AI 条目，请重试".into());
    }
    crate::config::atomic_write_private(&path, &bytes).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(record_path(data_root));
    Ok(Some(path.display().to_string()))
}

/// 当前生效的 WE2AI 模型：托管记录指向的条目仍在且未被改动。
pub fn managed_model(data_root: &Path) -> Option<String> {
    let record = load_record(data_root)?;
    let items = read_items(&models_path()).ok()?;
    items
        .iter()
        .find(|v| entry_id(v) == Some(record.id.as_str()))
        .filter(|v| fingerprint(v) == record.fingerprint)
        .map(|_| record.id.clone())
}

/// 登出时清除托管记录不涉及 Key；WorkBuddy 条目属于用户工具配置，保留。
pub fn plan_fields() -> Vec<String> {
    vec![
        "id".into(),
        "name".into(),
        "vendor".into(),
        "url".into(),
        "apiKey".into(),
        "useCustomProtocol".into(),
    ]
}
