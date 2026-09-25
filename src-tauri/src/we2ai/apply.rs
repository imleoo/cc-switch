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
use super::secret_store::{self, SecretStore};
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
///
/// **不包含 Codex 的 `auth.json`**（P6 五轮 Codex 验收高危项 1）：WE2AI 模式
/// 下 `preserve_codex_official_auth_on_switch` 恒为 `true`
/// （见 [`ensure_codex_login_preservation`]），上游 `switch` 管道在这个设置
/// 下从不写入 `auth.json`（`codex_config.rs` 的 ChatGPT 登录保留分支）。把一
/// 个"这次调用保证不会写"的文件也纳入快照/标记/回滚清单，只会徒增
/// `mark_write_attempted()` 之后、`switch` 真正返回之前这段窗口里的误判
/// 面——哪怕有 `h_pre` 兜底（P6 四轮）能避免误覆盖，也不该让一个从不在
/// 本次调用职责范围内的文件出现在"我们负责回滚"的清单里。
fn live_files(tool: ProviderTool, claude_settings: &Path) -> Vec<PathBuf> {
    match tool {
        ProviderTool::ClaudeCode => vec![claude_settings.to_path_buf()],
        ProviderTool::Codex => vec![
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

/// Claude 托管 env 键的唯一列表：`claude_managed_env` 取值、
/// [`restore_claude`] 移除字段都从这里派生，不重复维护第二份清单（P6 方案
/// 决定 1）。
const CLAUDE_MANAGED_ENV_KEYS: [&str; 6] = [
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
];

fn claude_managed_env(params: &ApplyParams) -> Vec<(&'static str, String)> {
    let values = [
        params.gateway_root.trim_end_matches('/').to_string(),
        params.api_key.clone(),
        params.model.clone(),
        params.slot(|s| &s.sonnet),
        params.slot(|s| &s.opus),
        params.slot(|s| &s.haiku),
    ];
    CLAUDE_MANAGED_ENV_KEYS.into_iter().zip(values).collect()
}

/// 系统钥匙串里保存"用户自己的 Claude `ANTHROPIC_API_KEY`"的固定 account。
/// 与登录会话的 account（`{region}:{user_id}`，第二段恒为数字）格式不同，
/// 不会冲突；同一 `service`（[`secret_store::SERVICE_NAME`]）下按 account
/// 区分条目（P6 方案决定 2）。
pub const CLAUDE_API_KEY_ACCOUNT: &str = "tool:claude:ANTHROPIC_API_KEY";

/// apply 前保存用户自己的 `env.ANTHROPIC_API_KEY`（若存在且非空）到系统钥匙
/// 串，供将来"恢复官方配置"时写回。保存失败则整次 apply 中止、不写入任何
/// 内容——用户的 Key 绝不能被静默销毁（P6 方案决定 2）。已保存的值不会被
/// "本次 live 没有该字段"覆盖为空；live 有值时无论是否与已保存的值相同都
/// 直接覆盖为最新值（幂等，不需要先读旧值比较）。
/// Claude：在任何快照捕获之前，读一次 live 文件当前的 `env.ANTHROPIC_API_KEY`
/// （若非空）保存到系统钥匙串，返回保存的值供 [`merged_claude_settings`]
/// 核对漂移。**必须在 `FileSnapshot::capture` 之前调用**：这里的钥匙串写入
/// 可能阻塞在系统授权弹窗上，放在快照之前能让快照的 H0 天然反映弹窗结束
/// 后的最新内容，不会把弹窗期间的外部编辑误判成需要保护的旧状态（P6 二轮
/// Opus 复核高危项 1b）。读取或解析失败在这里不报告——统一交给随后
/// `merged_claude_settings` 的同一份读取路径报告，不重复处理坏文件。
fn preserve_and_read_users_claude_api_key(
    path: &Path,
    secret_store: &dyn SecretStore,
) -> Result<Option<String>, ApplyError> {
    let key = std::fs::read_to_string(path).ok().and_then(|text| {
        serde_json::from_str::<Value>(&text).ok().and_then(|v| {
            v.pointer("/env/ANTHROPIC_API_KEY")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
    });
    if let Some(k) = &key {
        secret_store
            .set(secret_store::SERVICE_NAME, CLAUDE_API_KEY_ACCOUNT, k)
            .map_err(|e| {
                ApplyError::new(
                    ERR_FAILED,
                    format!("保存用户自己的 ANTHROPIC_API_KEY 到系统钥匙串失败，未写入：{e}"),
                )
            })?;
    }
    Ok(key)
}

/// 读当前 Claude live 作为基底，只覆盖托管 env，删除 `ANTHROPIC_API_KEY`。
/// `saved_key` 是 [`preserve_and_read_users_claude_api_key`] 在快照之前读到
/// 并已存入钥匙串的值：这里重新读到的 `ANTHROPIC_API_KEY` 若非空且与
/// `saved_key` 不同，说明文件在"保存到钥匙串"与"这次真正读取"之间的极短
/// 窗口里又被改过（例如钥匙串授权弹窗其间用户又编辑了一次），此时那份新
/// 值从未被保存，直接中止、不写入任何内容，比静默丢弃更安全（P6 二轮
/// Opus 复核高危项 1b）。
fn merged_claude_settings(
    path: &Path,
    params: &ApplyParams,
    saved_key: Option<&str>,
) -> Result<Value, ApplyError> {
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
    let current_key = env
        .get("ANTHROPIC_API_KEY")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    if let Some(current) = current_key {
        if saved_key != Some(current) {
            return Err(ApplyError::new(
                ERR_FAILED,
                "检测到 Claude 配置在授权期间被修改，请重试".to_string(),
            ));
        }
    }
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
    // 两种情况可能同时发生：一部分文件被外部程序改写、不能覆盖，另一部分
    // 文件确实需要恢复但恢复本身失败了。旧逻辑只要 `external` 非空就直接
    // 返回，会把 `failures` 列表整个丢掉——用户看不到"另外还有文件恢复
    // 失败"这件事。现在两者都收集进消息；只要有恢复失败就升级成
    // `ERR_ROLLBACK`（这是更严重的情况，外部修改至少内容还在原地，恢复
    // 失败则状态不确定），否则外部修改单独存在时仍用 `ERR_EXTERNAL`
    // （P6 五轮 Codex 验收高危项 3）。
    if !external.is_empty() || !failures.is_empty() {
        let mut parts = Vec::new();
        if !external.is_empty() {
            parts.push(format!(
                "检测到其他程序修改，未回滚：{}",
                external.join("、")
            ));
        }
        if !failures.is_empty() {
            parts.push(format!("以下项未能恢复：{}", failures.join("、")));
        }
        let code = if !failures.is_empty() {
            ERR_ROLLBACK
        } else {
            ERR_EXTERNAL
        };
        return ApplyError::new(code, format!("{}；{}", cause.message, parts.join("；")));
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
    // 方案 5.2 "登出处理"：清空两条固定供应商行的整个 settings_config。下次
    // apply 从 live 重新取基底，不依赖数据库行。
    for (app, id) in [
        (AppType::Claude, CLAUDE_PROVIDER_ID),
        (AppType::Codex, CODEX_PROVIDER_ID),
    ] {
        let Some(row) = state.db.get_provider_by_id(id, app.as_str())? else {
            continue;
        };
        if row.settings_config != json!({}) {
            state
                .db
                .update_provider_settings_config(app.as_str(), id, &json!({}))?;
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

fn is_we2ai_gateway_root(url: &str) -> bool {
    let url = url.trim_end_matches('/');
    super::region::Region::all()
        .iter()
        .any(|r| r.base_url().trim_end_matches('/') == url)
}

// ---------------------------------------------------------------------------
// 恢复官方配置（P6，取代功能 12 的 remove_tool_keys：不再只删 Key，而是移除
// WE2AI 为该工具写入的一切，让工具回到"WE2AI 从未碰过"的状态）。
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreTool {
    ClaudeCode,
    Codex,
    Workbuddy,
}

impl RestoreTool {
    fn label(self) -> &'static str {
        match self {
            RestoreTool::ClaudeCode => "Claude Code",
            RestoreTool::Codex => "Codex",
            RestoreTool::Workbuddy => "WorkBuddy",
        }
    }
}

/// 确认弹窗里展示的"恢复将移除的文件与字段"——独立于 [`plan_for`]（那份是
/// apply 会*写入*什么，`env.ANTHROPIC_API_KEY（删除）` 与 Codex 模型目录文件
/// 对恢复场景完全说反了：恢复不删 API Key，是把它写回；也从不碰模型目录
/// 文件）。Claude 的字段列表直接从 [`CLAUDE_MANAGED_ENV_KEYS`] 派生，不重复
/// 维护第二份键清单。
pub fn restore_plan_for(tool: RestoreTool) -> ApplyPlan {
    match tool {
        RestoreTool::ClaudeCode => ApplyPlan {
            files: vec![crate::config::get_claude_settings_path()
                .display()
                .to_string()],
            fields: CLAUDE_MANAGED_ENV_KEYS
                .iter()
                .map(|k| format!("env.{k}（移除）"))
                .chain(std::iter::once(
                    "env.ANTHROPIC_API_KEY（如之前保存过用户自己的 Key，则写回）".to_string(),
                ))
                .collect(),
        },
        RestoreTool::Codex => ApplyPlan {
            files: vec![crate::codex_config::get_codex_config_path()
                .display()
                .to_string()],
            fields: vec![
                "model_provider（移除）".into(),
                "model（移除）".into(),
                format!("[model_providers.{CODEX_MODEL_PROVIDER}]（移除）"),
                "auth.json（ChatGPT 登录）不受影响".into(),
            ],
        },
        RestoreTool::Workbuddy => ApplyPlan {
            files: vec![super::workbuddy::models_path().display().to_string()],
            fields: vec!["WE2AI 条目（移除）".into()],
        },
    }
}

/// "恢复官方"的结果：`we2ai_restore_official`（取代功能 12 的旧移除 Key 命令）。
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RestoreOfficialOutcome {
    /// 已恢复（移除了 WE2AI 写入内容）的文件。
    pub restored: Vec<String>,
    /// 本来就没有可做的事，不是失败：未指向 WE2AI、或正被 CC Switch 代理
    /// 接管（Opus 复核高危项 1：与 `skipped` 混在一起会让"三个工具只指定了
    /// 一个"这种正常情况在登出恢复时始终弹出警告 toast，是假警报）。
    pub unchanged: Vec<String>,
    /// 未恢复的项及失败原因（读取/解析失败、WorkBuddy 条目被手工修改、
    /// 会话已变化、数据库清理失败、钥匙串读取失败等）。
    pub skipped: Vec<String>,
}

/// 恢复某个工具的官方配置：移除 WE2AI 写入的一切，用户其余配置原样保留。
/// 与 [`apply_provider_tool`] 共用 [`apply_lock`]（进行中的 apply 先完成，
/// 恢复再动手，不会撞见半写状态）。`still_allowed` 由调用方决定"是否继续
/// 处理剩余工具"：登出流程传入"是否仍是当时那个（已登出）身份"，一旦
/// 用户中途重新登录 / 换号就停止（沿用功能 12 `remove_tool_keys` 的
/// "登出流程重新登录即停止"语义）；顶栏"恢复官方"按钮等登录态下的直接
/// 调用传入"当前身份是否仍与发起时相同"，本身不要求未登录，但同样在会话
/// 变化时停止，不假设"一直允许"。不整体要求登出，因为方案已明确本命令
/// "无论是否登录都可用"。
pub fn restore_official(
    state: &AppState,
    data_root: &Path,
    secret_store: &dyn SecretStore,
    tools: &[RestoreTool],
    still_allowed: &dyn Fn() -> bool,
) -> RestoreOfficialOutcome {
    let _lock = apply_lock();
    let mut outcome = RestoreOfficialOutcome::default();
    for &tool in tools {
        if !still_allowed() {
            outcome
                .skipped
                .push(format!("{}：登录状态已变化，未继续恢复", tool.label()));
            continue;
        }
        match tool {
            RestoreTool::ClaudeCode => {
                match restore_claude(state, secret_store, still_allowed) {
                    Ok(ClaudeRestoreResult::Restored {
                        path,
                        keychain_delete_failed,
                    }) => {
                        outcome.restored.push(path);
                        if let Some(e) = keychain_delete_failed {
                            outcome.skipped.push(format!("Claude Code：{e}"));
                        }
                    }
                    Ok(ClaudeRestoreResult::Unchanged {
                        keychain_cleanup_failed,
                    }) => {
                        outcome.unchanged.push(
                            "Claude Code：未指向 WE2AI 或正被 CC Switch 代理接管，无需恢复".into(),
                        );
                        if let Some(e) = keychain_cleanup_failed {
                            outcome.skipped.push(format!("Claude Code：{e}"));
                        }
                    }
                    Err(reason) => outcome.skipped.push(reason),
                }
                // DB 清理与 live 文件恢复是否成功无关：即使 live 没动（未
                // 指向 WE2AI）或本次恢复失败，也照常清理——这个 DB 完全是
                // WE2AI 私有数据（`~/.we2ai`），顶栏工具状态读的是 live 文件
                // 不是 DB，清理与否不影响"该工具是否还能重试恢复"。
                if let Err(e) = cleanup_db_for_app(state, AppType::Claude, CLAUDE_PROVIDER_ID) {
                    outcome
                        .skipped
                        .push(format!("Claude Code：WE2AI 数据库清理失败：{e}"));
                }
            }
            RestoreTool::Codex => {
                match restore_codex(state, still_allowed) {
                    Ok(Some(path)) => outcome.restored.push(path),
                    Ok(None) => outcome
                        .unchanged
                        .push("Codex：未指向 WE2AI 或正被 CC Switch 代理接管，无需恢复".into()),
                    Err(reason) => outcome.skipped.push(reason),
                }
                if let Err(e) = cleanup_db_for_app(state, AppType::Codex, CODEX_PROVIDER_ID) {
                    outcome
                        .skipped
                        .push(format!("Codex：WE2AI 数据库清理失败：{e}"));
                }
            }
            RestoreTool::Workbuddy => match super::workbuddy::remove_managed_entry(data_root) {
                Ok(Some(path)) => outcome.restored.push(path),
                Ok(None) => outcome
                    .unchanged
                    .push("WorkBuddy：未指向 WE2AI，无需恢复".into()),
                Err(reason) => outcome.skipped.push(reason),
            },
        }
    }
    outcome
}

/// [`restore_claude`] 的结果：成功恢复（可能带一条"钥匙串条目删不掉"的
/// 附加提示）或本来就没有可做的事（可能带一条"陈旧钥匙串残留清不掉"的
/// 附加提示）。两条附加提示都是真失败，调用方塞进 `skipped`；主结果各自
/// 塞进 `restored`/`unchanged`（Codex 复核中危项 3）。
enum ClaudeRestoreResult {
    Restored {
        path: String,
        keychain_delete_failed: Option<String>,
    },
    Unchanged {
        keychain_cleanup_failed: Option<String>,
    },
}

/// 未指向 WE2AI 的"什么都不用做"路径里，顺手清理陈旧的钥匙串残留：只有当
/// 钥匙串里存的值与 live 当前的 `ANTHROPIC_API_KEY` 完全一致时才删除（说明
/// 用户的 Key 已经安全地留在 live 里，钥匙串那份纯属多余的历史残留，例如
/// 上次恢复写回成功但删钥匙串失败）；删除失败报告为失败，成功不产生任何
/// 消息（Codex 复核中危项 3）。
fn cleanup_stale_claude_key(
    live: &Value,
    saved_key: Option<&str>,
    secret_store: &dyn SecretStore,
) -> Option<String> {
    let saved_key = saved_key?;
    let live_key = live
        .pointer("/env/ANTHROPIC_API_KEY")
        .and_then(Value::as_str);
    if live_key != Some(saved_key) {
        return None;
    }
    secret_store
        .delete(secret_store::SERVICE_NAME, CLAUDE_API_KEY_ACCOUNT)
        .err()
        .map(|e| {
            format!(
                "陈旧的 ANTHROPIC_API_KEY 钥匙串残留未能清理：{e}\
                 ；下次执行“恢复官方”或登出恢复时会自动重试清理"
            )
        })
}

/// 只有 `env.ANTHROPIC_BASE_URL` 仍指向 WE2AI 网关、且未检测到 CC Switch 代理
/// 接管才动手：移除全部托管 env 键（[`CLAUDE_MANAGED_ENV_KEYS`]），其余字段
/// 原样保留；若系统钥匙串里存有之前 apply 时保存的用户自己的 Key 且当前
/// `env` 没有 `ANTHROPIC_API_KEY`，写回该 Key，文件写入成功后才删除钥匙串
/// 条目（方案决定：先写文件、成功后再删钥匙串，避免文件写失败导致 Key
/// material 两头都没有；删除失败不算恢复失败，文件已经写好，只是额外报告
/// 一条提示）。恢复后即便 `env` 变成空对象也不删除这个对象本身，保持结构
/// 最小改动（方案决定 1）。
///
/// **TOCTOU 防护**（Codex 复核高危项 1）：钥匙串读取可能阻塞在系统授权
/// 弹窗上，等待期间 live 文件可能被外部程序（用户手改、CC Switch 开始
/// 接管）改写。因此：① 钥匙串读取放在读 live 文件**之前**，缩短"读到的
/// live 内容"到"真正决定要不要写"之间的窗口；② 写入前重新读一次文件字节
/// 与最初解析时逐字节比对、重新判一次接管、重新调一次 `still_allowed()`，
/// 命中任一项都不写、钥匙串条目原样保留、报告未恢复。这个复查缩小但不能
/// 消除"比对"与"写入"之间的窗口——方案第 4.1 节"只做尽力检测"同样适用
/// 于恢复路径，是已知限制，见 `自定义开发功能列表.md` 功能 17。
fn restore_claude(
    state: &AppState,
    secret_store: &dyn SecretStore,
    still_allowed: &dyn Fn() -> bool,
) -> Result<ClaudeRestoreResult, String> {
    if state
        .proxy_service
        .detect_takeover_in_live_config_for_app(&AppType::Claude)
    {
        return Ok(ClaudeRestoreResult::Unchanged {
            keychain_cleanup_failed: None,
        });
    }

    // 先读 live 文件，钥匙串留到真正需要时才碰（Codex P6 二轮验收中危项
    // 2）：文件不存在、或存在但未指向 WE2AI 且本来就没有 Key，都不需要弹
    // 系统钥匙串授权窗，登出批量恢复时尤其明显——三个工具里没指定过的那
    // 些不该白白触发一次钥匙串访问。
    let path = crate::config::get_claude_settings_path();
    let original_bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ClaudeRestoreResult::Unchanged {
                keychain_cleanup_failed: None,
            })
        }
        Err(e) => return Err(format!("{}：读取失败（{e}），未恢复", path.display())),
    };
    let mut value: Value = match serde_json::from_slice(&original_bytes) {
        Ok(v) => v,
        Err(e) => {
            return Err(format!(
                "{}：不是有效的 JSON（{e}），未恢复",
                path.display()
            ))
        }
    };
    let points_to_we2ai = value
        .pointer("/env/ANTHROPIC_BASE_URL")
        .and_then(Value::as_str)
        .is_some_and(is_we2ai_gateway_root);
    if !points_to_we2ai {
        // 只有 live 里确实有非空的 ANTHROPIC_API_KEY 才有必要查一次钥匙串
        // （判断是不是能顺手清理的陈旧残留）；否则完全不碰钥匙串。
        let live_has_key = value
            .pointer("/env/ANTHROPIC_API_KEY")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());
        let keychain_cleanup_failed = if live_has_key {
            let saved_key = match secret_store
                .get(secret_store::SERVICE_NAME, CLAUDE_API_KEY_ACCOUNT)
            {
                Ok(v) => v.filter(|k| !k.is_empty()),
                Err(e) => return Err(format!("读取保存的 ANTHROPIC_API_KEY 失败（{e}），未恢复")),
            };
            cleanup_stale_claude_key(&value, saved_key.as_deref(), secret_store)
        } else {
            None
        };
        return Ok(ClaudeRestoreResult::Unchanged {
            keychain_cleanup_failed,
        });
    }

    // 指向 WE2AI，真正要恢复：现在才读钥匙串（可能长时间阻塞在系统授权
    // 弹窗上）。等待期间发生的外部编辑/CC Switch 接管/会话变化，都由写入
    // 前的复查（与最初读到的 `original_bytes` 逐字节比对）兜底。
    let saved_key = match secret_store.get(secret_store::SERVICE_NAME, CLAUDE_API_KEY_ACCOUNT) {
        Ok(v) => v.filter(|k| !k.is_empty()),
        Err(e) => return Err(format!("读取保存的 ANTHROPIC_API_KEY 失败（{e}），未恢复")),
    };
    let env = match value.get_mut("env").and_then(Value::as_object_mut) {
        Some(env) => env,
        None => {
            return Ok(ClaudeRestoreResult::Unchanged {
                keychain_cleanup_failed: None,
            })
        }
    };
    for key in CLAUDE_MANAGED_ENV_KEYS {
        env.remove(key);
    }
    let mut restored_saved_key = false;
    if !env.contains_key("ANTHROPIC_API_KEY") {
        if let Some(key) = &saved_key {
            env.insert("ANTHROPIC_API_KEY".into(), Value::String(key.clone()));
            restored_saved_key = true;
        }
    }
    let text =
        serde_json::to_string_pretty(&value).map_err(|e| format!("{}: {e}", path.display()))?;

    // 写入前复查：文件是否被外部改写、CC Switch 是否刚开始接管、调用方是否
    // 仍允许继续——命中任一项都不写，钥匙串条目保留。
    match std::fs::read(&path) {
        Ok(current) if current == original_bytes => {}
        _ => {
            return Err(format!(
                "{}：检测到其他程序修改，未恢复，请重试",
                path.display()
            ))
        }
    }
    if state
        .proxy_service
        .detect_takeover_in_live_config_for_app(&AppType::Claude)
    {
        return Err(format!(
            "{}：检测到 CC Switch 代理接管，未恢复",
            path.display()
        ));
    }
    if !still_allowed() {
        return Err(format!("{}：登录状态已变化，未恢复", path.display()));
    }

    crate::config::atomic_write_private(&path, text.as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let _ = fsguard::tighten_file(&path);
    let keychain_delete_failed = if restored_saved_key {
        // 只有文件写入成功后才删钥匙串条目：写文件失败时保留，避免用户的
        // Key 两头都没有（既不在钥匙串也没写回文件）。删除本身失败不算
        // 恢复失败（文件已经写回 Key），但要报告，否则钥匙串会一直留着
        // 一份陈旧副本——留给下次恢复调用时的 `cleanup_stale_claude_key`
        // 重试清理。
        secret_store
            .delete(secret_store::SERVICE_NAME, CLAUDE_API_KEY_ACCOUNT)
            .err()
            .map(|e| {
                format!(
                    "已写回用户的 ANTHROPIC_API_KEY，但未能从系统钥匙串删除保存的副本：{e}\
                     ；下次执行“恢复官方”或登出恢复时会自动重试清理"
                )
            })
    } else {
        None
    };
    Ok(ClaudeRestoreResult::Restored {
        path: path.display().to_string(),
        keychain_delete_failed,
    })
}

/// 收集将被删除的顶层键（`model_provider`、`model`）各自的 leading 注释
/// （按它们在文件中的原始出现顺序拼接，不假设固定先后），删除后接到表里
/// 新的第一个位置上：若那是一个普通键值对，就前置到它自己的 leading 注释
/// 前面（不覆盖、不丢弃它原有的注释）；若那是一张表（含隐式表——`toml_edit`
/// 对隐式表自身的 leading 注释不会渲染，实测确认过，见开发笔记）或已经没有
/// 任何顶层条目，就前置到文档级的 leading 文本上（同样不覆盖已有内容）。
/// 三种情形（只注释一个键、两个键都有注释且新首键自己也有注释、恢复后只剩
/// 表）都有对应单测锁定行为（Codex 复核中危项 2）。
fn attach_removed_codex_comment(doc: &mut toml_edit::DocumentMut, combined: &str) {
    if combined.is_empty() {
        return;
    }
    let mut attached_to_key = false;
    if let Some((mut first_key, item)) = doc.iter_mut().next() {
        if !item.is_table() {
            let existing = first_key
                .leaf_decor()
                .prefix()
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            first_key
                .leaf_decor_mut()
                .set_prefix(format!("{combined}{existing}"));
            attached_to_key = true;
        }
    }
    if !attached_to_key {
        let existing = doc
            .as_table()
            .decor()
            .prefix()
            .and_then(|p| p.as_str())
            .unwrap_or("")
            .to_string();
        doc.as_table_mut()
            .decor_mut()
            .set_prefix(format!("{combined}{existing}"));
    }
}

/// 只有顶层 `model_provider == "we2ai"`、且未检测到 CC Switch 代理接管才
/// 动手（理由同 [`restore_claude`]）：移除顶层 `model_provider`、`model`；
/// `[model_providers.we2ai]` 表只在其 `base_url` 仍是 WE2AI 网关时才整张
/// 删除（用户若把这张表改成自有端点，token 与整张表都不动，与旧
/// `remove_tool_keys` 对 Codex 的判定一致）；`model_providers` 变空则一并
/// 删除该表。`auth.json`（ChatGPT 官方登录）从不触碰。写入前的 TOCTOU 复查
/// 与 [`restore_claude`] 相同（Codex 复核高危项 1）。
fn restore_codex(
    state: &AppState,
    still_allowed: &dyn Fn() -> bool,
) -> Result<Option<String>, String> {
    if state
        .proxy_service
        .detect_takeover_in_live_config_for_app(&AppType::Codex)
    {
        return Ok(None);
    }
    let path = crate::codex_config::get_codex_config_path();
    let original_bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}：读取失败（{e}），未恢复", path.display())),
    };
    let text = match std::str::from_utf8(&original_bytes) {
        Ok(s) => s,
        Err(e) => {
            return Err(format!(
                "{}：不是有效的 UTF-8（{e}），未恢复",
                path.display()
            ))
        }
    };
    let mut doc: toml_edit::DocumentMut = match text.parse() {
        Ok(d) => d,
        Err(e) => {
            return Err(format!(
                "{}：不是有效的 TOML（{e}），未恢复",
                path.display()
            ))
        }
    };
    let points_to_we2ai =
        doc.get("model_provider").and_then(|v| v.as_str()) == Some(CODEX_MODEL_PROVIDER);
    if !points_to_we2ai {
        return Ok(None);
    }
    // toml_edit 把"某个 key 前面的注释"存成该 key 自己的 leading decor、
    // "同一行值后面的注释"（`model = "x" # note`）存成那个值的 decor
    // suffix；直接 `remove()` 两者都会跟着丢。删除前按文件原始顺序、且
    // "该键的 leading 注释在前、trailing 注释在后"摘出两个待删键各自的
    // 注释（P6 二轮 Codex 验收中危项 3：早期版本只收集了 leading，漏了
    // `model = "x" # note` 这种写在同一行的注释）。
    let mut removed_comments = Vec::new();
    for (key, item) in doc.iter_mut() {
        if key.get() != "model_provider" && key.get() != "model" {
            continue;
        }
        if let Some(p) = key.leaf_decor().prefix().and_then(|p| p.as_str()) {
            if !p.trim().is_empty() {
                removed_comments.push(p.to_string());
            }
        }
        if let Some(suffix) = item
            .as_value()
            .and_then(|v| v.decor().suffix())
            .and_then(|p| p.as_str())
        {
            let trimmed = suffix.trim();
            if !trimmed.is_empty() {
                removed_comments.push(format!("{trimmed}\n"));
            }
        }
    }
    doc.remove("model_provider");
    doc.remove("model");
    attach_removed_codex_comment(&mut doc, &removed_comments.concat());
    if let Some(providers) = doc
        .get_mut("model_providers")
        .and_then(|p| p.as_table_like_mut())
    {
        let table_points_to_we2ai = providers
            .get(CODEX_MODEL_PROVIDER)
            .and_then(|t| t.as_table_like())
            .and_then(|t| t.get("base_url"))
            .and_then(|v| v.as_str())
            .and_then(|u| u.trim_end_matches('/').strip_suffix("/v1"))
            .is_some_and(is_we2ai_gateway_root);
        if table_points_to_we2ai {
            providers.remove(CODEX_MODEL_PROVIDER);
        }
        if providers.is_empty() {
            doc.remove("model_providers");
        }
    }
    let new_text = doc.to_string();

    match std::fs::read(&path) {
        Ok(current) if current == original_bytes => {}
        _ => {
            return Err(format!(
                "{}：检测到其他程序修改，未恢复，请重试",
                path.display()
            ))
        }
    }
    if state
        .proxy_service
        .detect_takeover_in_live_config_for_app(&AppType::Codex)
    {
        return Err(format!(
            "{}：检测到 CC Switch 代理接管，未恢复",
            path.display()
        ));
    }
    if !still_allowed() {
        return Err(format!("{}：登录状态已变化，未恢复", path.display()));
    }

    crate::config::atomic_write_private(&path, new_text.as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let _ = fsguard::tighten_file(&path);
    Ok(Some(path.display().to_string()))
}

/// WE2AI 自己的数据库对某个 app 的收尾：清空固定 id 供应商行的
/// `settings_config`、清空该 app 的 DB current 标记；`proxy_live_backup`
/// 只在其内容仍能匹配到 WE2AI 网关（即备份的是 WE2AI 自己那份 live，通常
/// 出现在"CC Switch 接管前 live 恰好是 WE2AI"的场景）时才删除——其余情况
/// 保留，那是 CC Switch 对其他供应商的合法备份，恢复官方配置不应该动它
/// （方案决定，字面依据见 P6 任务说明）。DB 完全是 WE2AI 私有数据根
/// （`~/.we2ai`）内的数据，与真实 CC Switch 的 `~/.cc-switch` 数据库无关，
/// 因此这里的清理与"live 是否仍指向 WE2AI"无关，总是执行。
fn cleanup_db_for_app(state: &AppState, app: AppType, id: &str) -> Result<(), String> {
    let mut errors = Vec::new();
    if let Err(e) = clear_db_current(state, app.as_str()) {
        errors.push(format!("current 标记: {e}"));
    }
    match state.db.get_provider_by_id(id, app.as_str()) {
        Ok(Some(row)) if row.settings_config != json!({}) => {
            if let Err(e) = state
                .db
                .update_provider_settings_config(app.as_str(), id, &json!({}))
            {
                errors.push(format!("供应商行: {e}"));
            }
        }
        Ok(_) => {}
        Err(e) => errors.push(format!("读取供应商行: {e}")),
    }
    match futures::executor::block_on(state.db.get_live_backup(app.as_str())) {
        Ok(Some(backup)) if backup_config_is_we2ai(&backup.original_config) => {
            if let Err(e) = futures::executor::block_on(state.db.delete_live_backup(app.as_str())) {
                errors.push(format!("代理备份: {e}"));
            }
        }
        Ok(_) => {}
        Err(e) => errors.push(format!("读取代理备份: {e}")),
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("、"))
    }
}

/// `proxy_live_backup.original_config` 是否是 WE2AI 自己那份配置：粗略按
/// "文本中含有任一区域的 WE2AI 网关地址"判断（Claude/Codex 的备份内容结构
/// 不同，不值得为此各写一套精确解析）。
fn backup_config_is_we2ai(config: &str) -> bool {
    super::region::Region::all()
        .iter()
        .any(|r| config.contains(r.base_url().trim_end_matches('/')))
}

pub fn apply_provider_tool(
    state: &AppState,
    tool: ProviderTool,
    params: &ApplyParams,
    still_current: &dyn Fn() -> bool,
    secret_store: &dyn SecretStore,
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

    // Claude：钥匙串写入可能阻塞在系统授权弹窗上，必须在捕获任何文件快照
    // 之前完成——这样快照的 H0 天然反映弹窗结束后的最新内容，不会把弹窗
    // 期间发生的外部编辑误判成需要保护的旧状态（P6 二轮 Opus 复核高危项
    // 1b）。
    let claude_saved_key = if tool == ProviderTool::ClaudeCode {
        preserve_and_read_users_claude_api_key(&claude_settings, secret_store)?
    } else {
        None
    };

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
        ProviderTool::ClaudeCode => {
            merged_claude_settings(&claude_settings, params, claude_saved_key.as_deref())
        }
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

    // 从这里开始才真正可能触碰 live 文件（上游 switch 管道与紧接着 WE2AI
    // 自己对 Codex 的追加写入）。标记之前的任何失败都不该把这段准备期间
    // 发生的外部编辑当成"我们的半写状态"去回滚覆盖（P6 二轮 Opus 复核
    // 高危项 1a，见 `snapshot.rs::FileSnapshot::mark_write_attempted`）。
    //
    // 标记本身也可能失败（权限问题等真正的 I/O 错误，`NotFound` 已经在
    // `mark_write_attempted()` 内部归一化成"文件不存在"）：一旦发生，必须
    // 在真正调用 `switch` 之前就中止整个 apply，不写任何 live 文件——此时
    // 没有任何文件被写过，直接按失败路径回滚（对已经标记成功的文件是
    // 安全的 no-op，对标记失败的文件本身，`restore()` 会因为"已标记但
    // H_pre 未知"拒绝写入并单独报告，见 P6 五轮 Codex 验收高危项 2）。
    let mut mark_err = None;
    for f in files.iter_mut() {
        if let Err(e) = f.mark_write_attempted() {
            mark_err = Some(ApplyError::new(
                ERR_FAILED,
                format!("标记 {} 为即将写入失败：{e}", f.path.display()),
            ));
            break;
        }
    }
    if let Some(e) = mark_err {
        return Err(fail(&files, e));
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
