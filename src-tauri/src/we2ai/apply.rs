//! `we2ai_apply_model` 的 Claude Code 与 Codex 写入（方案第 4.1、4.2 节）。
//!
//! 流程（同步，调用方在 `spawn_blocking` 中执行）：
//!
//! ```text
//! 获取 we2ai_apply 锁（不是 switch lock）
//!   → 三个配置目录收紧 0700、已有凭据文件 0600
//!   → 前置条件：该应用只有固定 id 供应商、current 为空或指向它、未被代理接管
//!   → 快照：数据库行 / current / proxy_live_backup / 本地 current / live 文件
//!   → 以当前 live 为基底只覆盖托管字段 → upsert 固定 id 供应商
//!   → ProviderService::switch（内部自行加 switch lock）
//!   → Codex：把 [model_providers.we2ai].requires_openai_auth 改为 false
//!   → 记 H1 → 回读门 → 收敛数据库行为仅托管字段 → 文件再收紧 0600
//! 任一步失败：按快照清单恢复（live 文件按 H0/H1 规则），回读核对
//! ```

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use serde_json::{json, Value};

use super::fsguard;
use super::snapshot::{FileSnapshot, RestoreResult};
use crate::app_config::AppType;
use crate::provider::Provider;
use crate::services::ProviderService;
use crate::store::AppState;

pub const CLAUDE_PROVIDER_ID: &str = "we2ai-claude";
pub const CODEX_PROVIDER_ID: &str = "we2ai-codex";
/// Codex 配置里 WE2AI 自己的 provider 表名（不是上游保留名）。
pub const CODEX_MODEL_PROVIDER: &str = "we2ai";
const PROVIDER_NAME: &str = "WE2AI";
const WEBSITE_URL: &str = "https://we2ai.com";

/// WE2AI 自身的串行锁：串行化所有 apply，`switch` 内部再拿自己的 switch
/// lock（外层不能拿 switch lock，否则死锁）。
static APPLY_LOCK: Mutex<()> = Mutex::new(());

pub(crate) fn apply_lock() -> std::sync::MutexGuard<'static, ()> {
    APPLY_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 上游 `switch` 的热切换拒绝判定用：固定 id 供应商在接管状态下一律拒绝。
pub fn is_managed_provider_id(id: &str) -> bool {
    id == CLAUDE_PROVIDER_ID || id == CODEX_PROVIDER_ID
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderTool {
    ClaudeCode,
    Codex,
}

impl ProviderTool {
    fn app_type(self) -> AppType {
        match self {
            ProviderTool::ClaudeCode => AppType::Claude,
            ProviderTool::Codex => AppType::Codex,
        }
    }

    fn provider_id(self) -> &'static str {
        match self {
            ProviderTool::ClaudeCode => CLAUDE_PROVIDER_ID,
            ProviderTool::Codex => CODEX_PROVIDER_ID,
        }
    }
}

/// Claude Code 的三个槽位模型；未指定的槽位与主模型相同。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeSlots {
    pub sonnet: Option<String>,
    pub opus: Option<String>,
    pub haiku: Option<String>,
}

#[derive(Clone)]
pub struct ApplyParams {
    pub model: String,
    pub claude_slots: ClaudeSlots,
    /// 所选 Key 的明文，只在内存中传递。
    pub api_key: String,
    /// 区域根地址，如 `https://api.we2ai.com`。
    pub gateway_root: String,
    /// B1 返回的可选能力（仅 WorkBuddy 使用），无数据为 `None`。
    pub capabilities: Option<super::api::ModelCapabilities>,
}

impl std::fmt::Debug for ApplyParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApplyParams")
            .field("model", &self.model)
            .field("claude_slots", &self.claude_slots)
            .field("api_key", &"<redacted>")
            .field("gateway_root", &self.gateway_root)
            .finish()
    }
}

impl ApplyParams {
    fn slot(&self, pick: impl Fn(&ClaudeSlots) -> &Option<String>) -> String {
        pick(&self.claude_slots)
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&self.model)
            .to_string()
    }

    fn codex_base_url(&self) -> String {
        format!("{}/v1", self.gateway_root.trim_end_matches('/'))
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ApplyOutcome {
    /// 回读到的最终模型。
    pub model: String,
    /// 本次写入的文件。
    pub files: Vec<String>,
    /// 不阻止成功的提示（如 WorkBuddy 保留了手改条目）。
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyError {
    pub code: &'static str,
    pub message: String,
}

impl ApplyError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

pub const ERR_TAKEOVER: &str = "TAKEOVER_CONFLICT";
pub const ERR_TAKEOVER_DETECTED: &str = "TAKEOVER_DETECTED";
pub const ERR_PRECONDITION: &str = "PROVIDER_PRECONDITION";
pub const ERR_DIR: &str = "CONFIG_DIR_NOT_PRIVATE";
pub const ERR_FAILED: &str = "APPLY_FAILED";
pub const ERR_READBACK: &str = "APPLY_READBACK_MISMATCH";
pub const ERR_EXTERNAL: &str = "APPLY_EXTERNAL_MODIFICATION";
pub const ERR_ROLLBACK: &str = "APPLY_ROLLBACK_INCOMPLETE";
pub const ERR_SESSION_CHANGED: &str = "SESSION_CHANGED";

/// 在 apply 锁内复查会话：取 Key 之后若已登出（或换了会话），不再写入。登出
/// 清理 Key 材料也在同一把锁内进行，因此二者不会交错成"清理完又写回 Key"
/// （Codex P4 验收第 2 轮高危项）。
pub(crate) fn ensure_session_current(still_current: &dyn Fn() -> bool) -> Result<(), ApplyError> {
    if still_current() {
        Ok(())
    } else {
        Err(ApplyError::new(
            ERR_SESSION_CHANGED,
            "登录状态已变化，未写入，请重新操作",
        ))
    }
}

const MATERIAL_MARKER: &str = "key_material_cleanup_pending";

/// 登出时 Key 材料未清除的持久标记（不含秘密），重启后仍能提示并重试。
pub fn material_cleanup_pending(data_root: &Path) -> bool {
    data_root.join(MATERIAL_MARKER).exists()
}

pub fn set_material_cleanup_pending(data_root: &Path, pending: bool) -> std::io::Result<()> {
    let path = data_root.join(MATERIAL_MARKER);
    if pending {
        crate::config::atomic_write_private(&path, b"1")
            .map_err(|e| std::io::Error::other(e.to_string()))
    } else {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// 上游 `switch` 在接管状态下拒绝固定 id 供应商时返回的本地化键。
pub const TAKEOVER_LOCALIZED_KEY: &str = "we2ai.takeover_conflict";

// ---------------------------------------------------------------------------
// 路径
// ---------------------------------------------------------------------------

fn workbuddy_dir() -> PathBuf {
    super::workbuddy::config_dir()
}

/// 三个实际解析的配置目录（方案 4.2 节：apply 开始、快照之前全部收紧）。
fn config_dirs() -> Vec<PathBuf> {
    vec![
        crate::config::get_claude_config_dir(),
        crate::codex_config::get_codex_config_dir(),
        workbuddy_dir(),
    ]
}

/// 需要收紧为 0600 的凭据文件（存在才处理）。
fn credential_files(claude_settings: &Path) -> Vec<PathBuf> {
    vec![
        claude_settings.to_path_buf(),
        crate::codex_config::get_codex_config_path(),
        crate::codex_config::get_codex_auth_path(),
        workbuddy_dir().join(super::workbuddy::MODELS_FILE),
    ]
}

fn codex_managed_marker_path() -> PathBuf {
    crate::config::get_app_config_dir().join("codex_managed_oauth_live_auth.json")
}

/// 该工具的 live 文件快照清单。
fn live_files(tool: ProviderTool, claude_settings: &Path) -> Vec<PathBuf> {
    match tool {
        ProviderTool::ClaudeCode => vec![claude_settings.to_path_buf()],
        ProviderTool::Codex => vec![
            crate::codex_config::get_codex_auth_path(),
            crate::codex_config::get_codex_config_path(),
            crate::codex_config::get_codex_model_catalog_path(),
            codex_managed_marker_path(),
        ],
    }
}

/// 确认弹窗里展示的"将写入的文件与字段"。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ApplyPlan {
    pub files: Vec<String>,
    pub fields: Vec<String>,
}

pub fn plan_for(tool: ProviderTool) -> ApplyPlan {
    match tool {
        ProviderTool::ClaudeCode => ApplyPlan {
            files: vec![crate::config::get_claude_settings_path()
                .display()
                .to_string()],
            fields: vec![
                "env.ANTHROPIC_BASE_URL".into(),
                "env.ANTHROPIC_AUTH_TOKEN".into(),
                "env.ANTHROPIC_MODEL".into(),
                "env.ANTHROPIC_DEFAULT_SONNET_MODEL".into(),
                "env.ANTHROPIC_DEFAULT_OPUS_MODEL".into(),
                "env.ANTHROPIC_DEFAULT_HAIKU_MODEL".into(),
                "env.ANTHROPIC_API_KEY（删除）".into(),
            ],
        },
        ProviderTool::Codex => ApplyPlan {
            // 上游管道同时维护模型目录文件（快照清单已包含），一并展示。
            files: vec![
                crate::codex_config::get_codex_config_path()
                    .display()
                    .to_string(),
                crate::codex_config::get_codex_model_catalog_path()
                    .display()
                    .to_string(),
            ],
            fields: vec![
                "model_provider".into(),
                "model".into(),
                format!("[model_providers.{CODEX_MODEL_PROVIDER}]"),
            ],
        },
    }
}

// ---------------------------------------------------------------------------
// 测试钩子
// ---------------------------------------------------------------------------

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    AfterUpsert,
    BeforeSwitch,
    /// 钩子返回 `Err` 时代替 `switch` 调用（钩子自己可先做部分写入）。
    Switch,
    AfterLiveWrite,
}

#[cfg(test)]
type Hook = Box<dyn FnMut(Stage) -> Result<(), String>>;

#[cfg(test)]
thread_local! {
    static HOOK: std::cell::RefCell<Option<Hook>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_test_hook(hook: impl FnMut(Stage) -> Result<(), String> + 'static) {
    HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
pub(crate) fn clear_test_hook() {
    HOOK.with(|h| *h.borrow_mut() = None);
}

#[cfg(test)]
fn run_hook(stage: Stage) -> Result<(), String> {
    HOOK.with(|h| match h.borrow_mut().as_mut() {
        Some(f) => f(stage),
        None => Ok(()),
    })
}

macro_rules! hook {
    ($stage:ident) => {{
        #[cfg(test)]
        {
            run_hook(Stage::$stage)
        }
        #[cfg(not(test))]
        {
            Ok::<(), String>(())
        }
    }};
}

// ---------------------------------------------------------------------------
// 数据库快照
// ---------------------------------------------------------------------------

struct DbSnapshot {
    app: AppType,
    provider_id: &'static str,
    row: Option<Provider>,
    db_current: Option<String>,
    live_backup: Option<String>,
    local_current: Option<String>,
}

impl DbSnapshot {
    fn capture(state: &AppState, tool: ProviderTool) -> Result<Self, ApplyError> {
        let app = tool.app_type();
        let db_err = |e: crate::error::AppError| ApplyError::new(ERR_FAILED, e.to_string());
        Ok(Self {
            provider_id: tool.provider_id(),
            row: state
                .db
                .get_provider_by_id(tool.provider_id(), app.as_str())
                .map_err(db_err)?,
            db_current: state
                .db
                .get_current_provider(app.as_str())
                .map_err(db_err)?,
            live_backup: futures::executor::block_on(state.db.get_live_backup(app.as_str()))
                .map_err(db_err)?
                .map(|b| b.original_config),
            local_current: crate::settings::get_current_provider(&app),
            app,
        })
    }

    /// 恢复数据库行、current 标记、代理备份与本地 current；返回失败项描述。
    fn restore(&self, state: &AppState) -> Vec<String> {
        let mut failures = Vec::new();
        let app = self.app.as_str();
        let r = match &self.row {
            Some(row) => state.db.save_provider(app, row),
            None => state.db.delete_provider(app, self.provider_id),
        };
        if let Err(e) = r {
            failures.push(format!("数据库供应商行: {e}"));
        }
        let r = match &self.db_current {
            Some(id) => state.db.set_current_provider(app, id),
            None => clear_db_current(state, app),
        };
        if let Err(e) = r {
            failures.push(format!("数据库 current 标记: {e}"));
        }
        let r = match &self.live_backup {
            Some(config) => futures::executor::block_on(state.db.save_live_backup(app, config)),
            None => futures::executor::block_on(state.db.delete_live_backup(app)),
        };
        if let Err(e) = r {
            failures.push(format!("代理备份: {e}"));
        }
        if let Err(e) =
            crate::settings::set_current_provider(&self.app, self.local_current.as_deref())
        {
            failures.push(format!("本地 current: {e}"));
        }
        failures
    }
}

/// 上游 DAO 只有 `set_current_provider`，没有清空函数；回滚"行存在、current
/// 为空"的状态时由 WE2AI 在自有模块执行这条 SQL（方案 4.1 节快照清单）。
fn clear_db_current(state: &AppState, app: &str) -> Result<(), crate::error::AppError> {
    let conn = state
        .db
        .conn
        .lock()
        .map_err(|e| crate::error::AppError::Database(e.to_string()))?;
    conn.execute(
        "UPDATE providers SET is_current = 0 WHERE app_type = ?1",
        rusqlite::params![app],
    )
    .map_err(|e| crate::error::AppError::Database(e.to_string()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 合并基底
// ---------------------------------------------------------------------------

fn claude_managed_env(params: &ApplyParams) -> Vec<(&'static str, String)> {
    vec![
        (
            "ANTHROPIC_BASE_URL",
            params.gateway_root.trim_end_matches('/').to_string(),
        ),
        ("ANTHROPIC_AUTH_TOKEN", params.api_key.clone()),
        ("ANTHROPIC_MODEL", params.model.clone()),
        ("ANTHROPIC_DEFAULT_SONNET_MODEL", params.slot(|s| &s.sonnet)),
        ("ANTHROPIC_DEFAULT_OPUS_MODEL", params.slot(|s| &s.opus)),
        ("ANTHROPIC_DEFAULT_HAIKU_MODEL", params.slot(|s| &s.haiku)),
    ]
}

/// 读当前 Claude live 作为基底，只覆盖托管 env，删除冲突的 `ANTHROPIC_API_KEY`。
fn merged_claude_settings(path: &Path, params: &ApplyParams) -> Result<Value, ApplyError> {
    let mut base = match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => json!({}),
        Ok(text) => serde_json::from_str::<Value>(&text).map_err(|e| {
            ApplyError::new(
                ERR_FAILED,
                format!("{} 不是有效的 JSON，未写入：{e}", path.display()),
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => {
            return Err(ApplyError::new(
                ERR_FAILED,
                format!("读取 {} 失败：{e}", path.display()),
            ))
        }
    };
    let obj = base
        .as_object_mut()
        .ok_or_else(|| ApplyError::new(ERR_FAILED, format!("{} 顶层不是对象", path.display())))?;
    let env = obj.entry("env").or_insert_with(|| json!({}));
    if !env.is_object() {
        *env = json!({});
    }
    let env = env.as_object_mut().expect("env is object");
    env.remove("ANTHROPIC_API_KEY");
    for (k, v) in claude_managed_env(params) {
        env.insert(k.to_string(), Value::String(v));
    }
    Ok(base)
}

fn converged_claude_settings(params: &ApplyParams) -> Value {
    let env: serde_json::Map<String, Value> = claude_managed_env(params)
        .into_iter()
        .map(|(k, v)| (k.to_string(), Value::String(v)))
        .collect();
    json!({ "env": env })
}

fn we2ai_codex_table(params: &ApplyParams) -> toml_edit::Table {
    let mut t = toml_edit::Table::new();
    t["name"] = toml_edit::value(PROVIDER_NAME);
    t["base_url"] = toml_edit::value(params.codex_base_url());
    t["wire_api"] = toml_edit::value("responses");
    t
}

/// 读当前 config.toml 作为基底（保留格式与注释），覆盖顶层 `model_provider`、
/// `model` 与整张 `[model_providers.we2ai]`。
fn merged_codex_config(path: &Path, params: &ApplyParams) -> Result<String, ApplyError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(ApplyError::new(
                ERR_FAILED,
                format!("读取 {} 失败：{e}", path.display()),
            ))
        }
    };
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| {
        ApplyError::new(
            ERR_FAILED,
            format!("{} 不是有效的 TOML，未写入：{e}", path.display()),
        )
    })?;
    doc["model_provider"] = toml_edit::value(CODEX_MODEL_PROVIDER);
    doc["model"] = toml_edit::value(params.model.as_str());
    let providers = doc
        .entry("model_providers")
        .or_insert_with(|| {
            let mut t = toml_edit::Table::new();
            t.set_implicit(true);
            toml_edit::Item::Table(t)
        })
        .as_table_like_mut()
        .ok_or_else(|| ApplyError::new(ERR_FAILED, "config.toml 的 model_providers 不是表"))?;
    providers.insert(
        CODEX_MODEL_PROVIDER,
        toml_edit::Item::Table(we2ai_codex_table(params)),
    );
    Ok(doc.to_string())
}

fn converged_codex_settings(params: &ApplyParams) -> Value {
    let mut doc = toml_edit::DocumentMut::new();
    doc["model_provider"] = toml_edit::value(CODEX_MODEL_PROVIDER);
    doc["model"] = toml_edit::value(params.model.as_str());
    let mut providers = toml_edit::Table::new();
    providers.set_implicit(true);
    providers.insert(
        CODEX_MODEL_PROVIDER,
        toml_edit::Item::Table(we2ai_codex_table(params)),
    );
    doc["model_providers"] = toml_edit::Item::Table(providers);
    json!({
        "auth": { "OPENAI_API_KEY": params.api_key },
        "config": doc.to_string(),
    })
}

/// `switch` 之后只改 WE2AI 自己那张表：`requires_openai_auth = false`（方案
/// 4.2 节"保留 ChatGPT 登录"表）。
fn set_codex_requires_openai_auth_false(path: &Path) -> Result<(), ApplyError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| ApplyError::new(ERR_FAILED, format!("读取 {} 失败：{e}", path.display())))?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| ApplyError::new(ERR_FAILED, format!("解析 {} 失败：{e}", path.display())))?;
    let table = doc
        .get_mut("model_providers")
        .and_then(|p| p.as_table_like_mut())
        .and_then(|p| p.get_mut(CODEX_MODEL_PROVIDER))
        .and_then(|t| t.as_table_like_mut())
        .ok_or_else(|| {
            ApplyError::new(ERR_READBACK, "写入后 config.toml 缺少 WE2AI provider 表")
        })?;
    table.insert("requires_openai_auth", toml_edit::value(false));
    crate::config::atomic_write_private(path, doc.to_string().as_bytes())
        .map_err(|e| ApplyError::new(ERR_FAILED, e.to_string()))
}

// ---------------------------------------------------------------------------
// 回读门
// ---------------------------------------------------------------------------

enum Readback {
    Ok { model: String },
    TakenOver,
    Mismatch(String),
}

fn readback(
    state: &AppState,
    tool: ProviderTool,
    claude_settings: &Path,
    params: &ApplyParams,
) -> Readback {
    if state
        .proxy_service
        .detect_takeover_in_live_config_for_app(&tool.app_type())
    {
        return Readback::TakenOver;
    }
    match tool {
        ProviderTool::ClaudeCode => {
            let value: Value = match std::fs::read_to_string(claude_settings)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
            {
                Some(v) => v,
                None => return Readback::Mismatch("无法读取写入后的 Claude 配置".into()),
            };
            let env = &value["env"];
            for (k, expected) in claude_managed_env(params) {
                if env[k].as_str() != Some(expected.as_str()) {
                    return Readback::Mismatch(format!("env.{k} 与期望不一致"));
                }
            }
            if env.get("ANTHROPIC_API_KEY").is_some() {
                return Readback::Mismatch("env.ANTHROPIC_API_KEY 仍存在".into());
            }
            Readback::Ok {
                model: params.model.clone(),
            }
        }
        ProviderTool::Codex => {
            let path = crate::codex_config::get_codex_config_path();
            let value: toml::Value = match std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| toml::from_str(&t).ok())
            {
                Some(v) => v,
                None => return Readback::Mismatch("无法读取写入后的 config.toml".into()),
            };
            if value.get("model_provider").and_then(|v| v.as_str()) != Some(CODEX_MODEL_PROVIDER) {
                return Readback::Mismatch("model_provider 不是 WE2AI".into());
            }
            let model = value
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if model != params.model {
                return Readback::Mismatch("model 与期望不一致".into());
            }
            let table = value
                .get("model_providers")
                .and_then(|p| p.get(CODEX_MODEL_PROVIDER));
            let field = |k: &str| table.and_then(|t| t.get(k));
            if field("base_url").and_then(|v| v.as_str()) != Some(params.codex_base_url().as_str())
            {
                return Readback::Mismatch("base_url 与期望不一致".into());
            }
            if field("experimental_bearer_token").and_then(|v| v.as_str())
                != Some(params.api_key.as_str())
            {
                return Readback::Mismatch("experimental_bearer_token 与所选 Key 不一致".into());
            }
            if field("requires_openai_auth").and_then(|v| v.as_bool()) != Some(false) {
                return Readback::Mismatch("requires_openai_auth 不是 false".into());
            }
            Readback::Ok {
                model: model.to_string(),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 主流程
// ---------------------------------------------------------------------------

/// 目录与文件收紧；任一目录失败即停止（快照与任何写入之前）。
pub(crate) fn tighten_all(claude_settings: &Path) -> Result<(), ApplyError> {
    for dir in config_dirs() {
        fsguard::tighten_dir(&dir).map_err(|e| ApplyError::new(ERR_DIR, e.to_string()))?;
    }
    for file in credential_files(claude_settings) {
        fsguard::tighten_file(&file)
            .map_err(|e| ApplyError::new(ERR_DIR, format!("{}: {e}", file.display())))?;
    }
    Ok(())
}

fn check_preconditions(state: &AppState, tool: ProviderTool) -> Result<(), ApplyError> {
    let app = tool.app_type();
    let id = tool.provider_id();
    let providers = state
        .db
        .get_all_providers(app.as_str())
        .map_err(|e| ApplyError::new(ERR_FAILED, e.to_string()))?;
    if providers.keys().any(|k| k != id) {
        return Err(ApplyError::new(
            ERR_PRECONDITION,
            "WE2AI 数据库中该工具存在其他供应商记录，已停止写入",
        ));
    }
    let db_current = state
        .db
        .get_current_provider(app.as_str())
        .map_err(|e| ApplyError::new(ERR_FAILED, e.to_string()))?;
    let local_current = crate::settings::get_current_provider(&app);
    for current in [db_current, local_current].into_iter().flatten() {
        if current != id {
            return Err(ApplyError::new(
                ERR_PRECONDITION,
                "WE2AI 记录的当前供应商不是 WE2AI，已停止写入",
            ));
        }
    }
    // 读取失败不能当作"没有备份"：接管状态下继续写会与 CC Switch 冲突。
    let has_backup = futures::executor::block_on(state.db.get_live_backup(app.as_str()))
        .map_err(|e| ApplyError::new(ERR_FAILED, format!("读取代理备份失败：{e}")))?
        .is_some();
    if has_backup
        || state
            .proxy_service
            .detect_takeover_in_live_config_for_app(&app)
    {
        return Err(ApplyError::new(
            ERR_TAKEOVER,
            "CC Switch 正在代理接管此工具，请先在 CC Switch 中关闭接管",
        ));
    }
    Ok(())
}

fn ensure_codex_login_preservation() -> Result<(), ApplyError> {
    if crate::settings::preserve_codex_official_auth_on_switch() {
        return Ok(());
    }
    crate::settings::mutate_settings(|s| s.preserve_codex_official_auth_on_switch = true)
        .map_err(|e| ApplyError::new(ERR_FAILED, e.to_string()))
}

/// 把被外部改写、未回滚的文件的快照另存一份，供用户手动恢复。
fn save_snapshot_copy(snapshot: &FileSnapshot) -> Option<PathBuf> {
    let bytes = snapshot.original_bytes()?;
    let dir = crate::config::get_app_config_dir().join("apply-snapshots");
    fsguard::tighten_dir(&dir).ok()?;
    let stamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
    let name = snapshot
        .path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "snapshot".into());
    let path = dir.join(format!("{stamp}-{name}"));
    crate::config::atomic_write_private(&path, bytes).ok()?;
    Some(path)
}

/// 失败恢复：数据库与本地设置、live 文件（`restore_files=false` 时跳过，用于
/// 检测到代理接管的兜底路径）。返回用户可读的错误。
fn rollback(
    state: &AppState,
    db: &DbSnapshot,
    files: &[FileSnapshot],
    restore_files: bool,
    cause: ApplyError,
) -> ApplyError {
    let mut failures = db.restore(state);
    let mut external = Vec::new();
    if restore_files {
        for snap in files {
            match snap.restore() {
                RestoreResult::Restored | RestoreResult::Unchanged => {}
                RestoreResult::ExternalModified => {
                    let copy = save_snapshot_copy(snap);
                    external.push(match copy {
                        Some(c) => {
                            format!("{}（原内容已另存为 {}）", snap.path.display(), c.display())
                        }
                        None => snap.path.display().to_string(),
                    });
                }
                RestoreResult::Failed(e) => failures.push(format!("{}: {e}", snap.path.display())),
            }
        }
    }
    // 只回滚数据库的路径同样收紧：上游可能已用普通权限写过 live（Fable P4 终验低危项）。
    for snap in files {
        let _ = fsguard::tighten_file(&snap.path);
    }
    if !external.is_empty() {
        return ApplyError::new(
            ERR_EXTERNAL,
            format!(
                "{}；检测到其他程序修改，未回滚：{}",
                cause.message,
                external.join("、")
            ),
        );
    }
    if !failures.is_empty() {
        return ApplyError::new(
            ERR_ROLLBACK,
            format!("{}；以下项未能恢复：{}", cause.message, failures.join("、")),
        );
    }
    cause
}

/// 登出时从 WE2AI 固定 id 供应商行里去掉 Key（方案第 8 节 P5：登出后数据库
/// 供应商行无 Key）。工具 live 文件属于用户，保留不动；下次 apply 从 live 重新
/// 取基底，不依赖数据库行。
pub fn scrub_managed_provider_keys(state: &AppState) -> Result<(), crate::error::AppError> {
    let _lock = apply_lock();
    scrub_managed_provider_keys_locked(state)
}

fn scrub_managed_provider_keys_locked(state: &AppState) -> Result<(), crate::error::AppError> {
    for (app, id) in [
        (AppType::Claude, CLAUDE_PROVIDER_ID),
        (AppType::Codex, CODEX_PROVIDER_ID),
    ] {
        let Some(row) = state.db.get_provider_by_id(id, app.as_str())? else {
            continue;
        };
        let mut config = row.settings_config.clone();
        if let Some(env) = config.get_mut("env").and_then(Value::as_object_mut) {
            env.remove("ANTHROPIC_AUTH_TOKEN");
            env.remove("ANTHROPIC_API_KEY");
        }
        if let Some(auth) = config.get_mut("auth").and_then(Value::as_object_mut) {
            auth.remove("OPENAI_API_KEY");
        }
        if let Some(text) = config.get("config").and_then(Value::as_str) {
            if let Ok(mut doc) = text.parse::<toml_edit::DocumentMut>() {
                if let Some(table) = doc
                    .get_mut("model_providers")
                    .and_then(|p| p.as_table_like_mut())
                    .and_then(|p| p.get_mut(CODEX_MODEL_PROVIDER))
                    .and_then(|t| t.as_table_like_mut())
                {
                    table.remove("experimental_bearer_token");
                }
                config["config"] = Value::String(doc.to_string());
            }
        }
        if config != row.settings_config {
            state
                .db
                .update_provider_settings_config(app.as_str(), id, &config)?;
        }
    }
    Ok(())
}

/// 登出时清除 WE2AI 数据目录里的 Key 材料：固定 id 供应商行里的 Key，以及
/// 数据库备份目录（备份是整库副本，含供应商行）。方案第 8 节 P5："登出后数据库
/// 供应商行与 proxy_live_backup 表均无 Key、备份目录为空"。WE2AI 从不创建
/// proxy_live_backup（接管状态下拒绝写入），这里一并删除以防万一。
pub fn clear_local_key_material(
    state: &AppState,
    still_logged_out: &dyn Fn() -> bool,
) -> Result<(), String> {
    // 整个清理持 apply 锁：进行中的 apply 先完成（它在锁内复查会话），之后
    // 清理再去掉它写进数据库行的 Key。
    let _lock = apply_lock();
    // 登出之后、清理拿到锁之前又登录了新会话：此刻的 Key 材料属于新会话，
    // 旧登出的清理不再执行（Codex P4 验收第 3 轮高危项 1）。
    if !still_logged_out() {
        return Ok(());
    }
    let mut failures = Vec::new();
    if let Err(e) = scrub_managed_provider_keys_locked(state) {
        failures.push(format!("供应商行: {e}"));
    }
    for app in [AppType::Claude, AppType::Codex] {
        if let Err(e) = futures::executor::block_on(state.db.delete_live_backup(app.as_str())) {
            failures.push(format!("代理备份: {e}"));
        }
    }
    let backups = crate::config::get_app_config_dir().join("backups");
    match std::fs::read_dir(&backups) {
        Ok(entries) => {
            // 逐项处理遍历错误：跳过读不到的条目会让清理误报成功（Codex P4
            // 验收第 4 轮中危项）。
            for entry in entries {
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        failures.push(format!("{}: {e}", backups.display()));
                        continue;
                    }
                };
                let path = entry.path();
                let r = if path.is_dir() {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
                if let Err(e) = r {
                    failures.push(format!("{}: {e}", path.display()));
                }
            }
            // 成功前复查目录确实为空。
            match std::fs::read_dir(&backups) {
                Ok(mut rest) => {
                    if rest.next().is_some() {
                        failures.push(format!("{} 仍有残留", backups.display()));
                    }
                }
                Err(e) => failures.push(format!("{}: {e}", backups.display())),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => failures.push(format!("{}: {e}", backups.display())),
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("、"))
    }
}

/// 本机是否仍残留 Key 材料（供应商行里的 Key、代理备份、数据库备份）。未登录
/// 时用于恢复"待清理"提示与重试入口，不依赖持久标记能否写入（Codex P4 验收
/// 第 3 轮高危项 2）。
pub fn key_material_residue(state: &AppState) -> bool {
    let non_empty = |v: Option<&Value>| v.and_then(Value::as_str).is_some_and(|s| !s.is_empty());
    for (app, id) in [
        (AppType::Claude, CLAUDE_PROVIDER_ID),
        (AppType::Codex, CODEX_PROVIDER_ID),
    ] {
        match state.db.get_provider_by_id(id, app.as_str()) {
            Ok(Some(row)) => {
                let c = &row.settings_config;
                if non_empty(c.pointer("/env/ANTHROPIC_AUTH_TOKEN"))
                    || non_empty(c.pointer("/env/ANTHROPIC_API_KEY"))
                    || non_empty(c.pointer("/auth/OPENAI_API_KEY"))
                    || c.get("config")
                        .and_then(Value::as_str)
                        .is_some_and(|t| t.contains("experimental_bearer_token"))
                {
                    return true;
                }
            }
            Ok(None) => {}
            // 读不到就按有残留处理：宁可多一次重试提示。
            Err(_) => return true,
        }
        match futures::executor::block_on(state.db.get_live_backup(app.as_str())) {
            Ok(Some(_)) | Err(_) => return true,
            Ok(None) => {}
        }
    }
    let backups = crate::config::get_app_config_dir().join("backups");
    match std::fs::read_dir(&backups) {
        Ok(mut entries) => entries.next().is_some(),
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

pub fn apply_provider_tool(
    state: &AppState,
    tool: ProviderTool,
    params: &ApplyParams,
    still_current: &dyn Fn() -> bool,
) -> Result<ApplyOutcome, ApplyError> {
    let _lock = apply_lock();
    ensure_session_current(still_current)?;

    // 固定 Claude 实际路径：settings.json 不存在而旧版 claude.json 存在时上游写后者。
    let claude_settings = crate::config::get_claude_settings_path();
    tighten_all(&claude_settings)?;
    check_preconditions(state, tool)?;
    if tool == ProviderTool::Codex {
        ensure_codex_login_preservation()?;
    }

    let db = DbSnapshot::capture(state, tool)?;
    let paths = live_files(tool, &claude_settings);
    let mut files = Vec::with_capacity(paths.len());
    for path in &paths {
        let snap = FileSnapshot::capture(path).map_err(|e| {
            ApplyError::new(ERR_FAILED, format!("快照 {} 失败：{e}", path.display()))
        })?;
        files.push(snap);
    }

    let fail = |files: &[FileSnapshot], err: ApplyError| rollback(state, &db, files, true, err);

    let merged = match tool {
        ProviderTool::ClaudeCode => merged_claude_settings(&claude_settings, params),
        ProviderTool::Codex => {
            merged_codex_config(&crate::codex_config::get_codex_config_path(), params).map(
                |config| json!({ "auth": { "OPENAI_API_KEY": params.api_key }, "config": config }),
            )
        }
    };
    let merged = match merged {
        Ok(v) => v,
        Err(e) => return Err(fail(&files, e)),
    };

    let mut provider = db.row.clone().unwrap_or_else(|| {
        Provider::with_id(
            tool.provider_id().into(),
            PROVIDER_NAME.into(),
            json!({}),
            Some(WEBSITE_URL.into()),
        )
    });
    provider.name = PROVIDER_NAME.into();
    provider.settings_config = merged;
    provider.category = None;
    let mut meta = provider.meta.clone().unwrap_or_default();
    // 必须显式为 Some(false)：None 时上游会回退到"超集即合并公共片段"。
    meta.common_config_enabled = Some(false);
    provider.meta = Some(meta);

    if let Err(e) = state.db.save_provider(tool.app_type().as_str(), &provider) {
        return Err(fail(&files, ApplyError::new(ERR_FAILED, e.to_string())));
    }
    if let Err(e) = hook!(AfterUpsert) {
        return Err(fail(&files, ApplyError::new(ERR_FAILED, e)));
    }
    if let Err(e) = hook!(BeforeSwitch) {
        return Err(fail(&files, ApplyError::new(ERR_FAILED, e)));
    }

    let switch_result = match hook!(Switch) {
        Err(e) => Err(ApplyError::new(ERR_FAILED, e)),
        Ok(()) => ProviderService::switch(state, tool.app_type(), tool.provider_id()).map_err(|e| {
            if matches!(&e, crate::error::AppError::Localized { key, .. } if *key == TAKEOVER_LOCALIZED_KEY) {
                ApplyError::new(ERR_TAKEOVER, "CC Switch 正在代理接管此工具，请先在 CC Switch 中关闭接管")
            } else {
                ApplyError::new(ERR_FAILED, e.to_string())
            }
        }),
    };
    if let Err(e) = switch_result {
        if e.code == ERR_TAKEOVER {
            // switch 在任何 live 写入之前就因接管拒绝：live 文件此刻属于 CC
            // Switch 的接管，只恢复 WE2AI 自有的数据库与本地设置，不动 live。
            return Err(rollback(state, &db, &files, false, e));
        }
        // 没有 H1：≠ H0 的文件视为本次 switch 的部分写入，按快照写回。
        return Err(fail(&files, e));
    }

    if tool == ProviderTool::Codex {
        if let Err(e) =
            set_codex_requires_openai_auth_false(&crate::codex_config::get_codex_config_path())
        {
            for f in files.iter_mut() {
                f.record_h1();
            }
            return Err(fail(&files, e));
        }
    }
    for f in files.iter_mut() {
        f.record_h1();
    }
    if let Err(e) = hook!(AfterLiveWrite) {
        return Err(fail(&files, ApplyError::new(ERR_FAILED, e)));
    }

    let model = match readback(state, tool, &claude_settings, params) {
        Readback::Ok { model } => model,
        Readback::TakenOver => {
            // live 已被 CC Switch 的代理路由接管：只恢复 WE2AI 自有的数据库与
            // 本地设置，不动工具 live 文件，否则会拔掉正在运行的代理。
            return Err(rollback(
                state,
                &db,
                &files,
                false,
                ApplyError::new(ERR_TAKEOVER_DETECTED, "检测到代理接管，未生效"),
            ));
        }
        Readback::Mismatch(reason) => {
            return Err(fail(
                &files,
                ApplyError::new(ERR_READBACK, format!("写入后核对失败：{reason}")),
            ));
        }
    };

    // 收敛数据库行：只保留托管字段，不把 live 里用户自己的其他密钥留在库里。
    provider.settings_config = match tool {
        ProviderTool::ClaudeCode => converged_claude_settings(params),
        ProviderTool::Codex => converged_codex_settings(params),
    };
    if let Err(e) = state.db.save_provider(tool.app_type().as_str(), &provider) {
        return Err(fail(&files, ApplyError::new(ERR_FAILED, e.to_string())));
    }
    for file in credential_files(&claude_settings) {
        let _ = fsguard::tighten_file(&file);
    }
    for f in &files {
        let _ = fsguard::tighten_file(&f.path);
    }

    Ok(ApplyOutcome {
        model,
        files: files
            .iter()
            .filter(|f| f.changed())
            .map(|f| f.path.display().to_string())
            .collect(),
        warnings: Vec::new(),
    })
}
