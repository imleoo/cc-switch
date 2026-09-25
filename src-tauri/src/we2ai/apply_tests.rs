//! P4 工具写入测试（方案第 4.1–4.3 节、第 8 节 P4 行）。
//!
//! 每个用例在独立临时 home 下运行（`CC_SWITCH_TEST_HOME` + `HOME`），数据库用
//! 内存库；这些环境变量是进程级的，全部用 `#[serial]` 串行，与上游同类测试
//! 共用 serial_test 的全局串行组。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use serial_test::serial;

use super::apply::{self, ApplyParams, ClaudeSlots, ProviderTool, RestoreTool, Stage};
use super::secret_store::test_support::InMemorySecretStore;
use super::secret_store::{self, SecretStore, SecretStoreError};
use super::workbuddy;
use crate::app_config::AppType;
use crate::database::Database;
use crate::store::AppState;

const KEY_A: &str = "sk-we2ai-test-key-aaaa";
const KEY_B: &str = "sk-we2ai-test-key-bbbb";
const GATEWAY: &str = "https://api.we2ai.com";

struct TestHome {
    dir: tempfile::TempDir,
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl TestHome {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let vars = [
            "CC_SWITCH_TEST_HOME",
            "HOME",
            "WORKBUDDY_CONFIG_DIR",
            "CODEBUDDY_CONFIG_DIR",
        ];
        let saved = vars.iter().map(|v| (*v, std::env::var_os(v))).collect();
        std::env::set_var("CC_SWITCH_TEST_HOME", dir.path());
        std::env::set_var("HOME", dir.path());
        std::env::remove_var("WORKBUDDY_CONFIG_DIR");
        std::env::remove_var("CODEBUDDY_CONFIG_DIR");
        crate::settings::reload_settings().expect("reload settings");
        Self { dir, saved }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        apply::clear_test_hook();
        workbuddy::clear_test_hooks();
        for (k, v) in &self.saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        let _ = crate::settings::reload_settings();
    }
}

fn state() -> AppState {
    AppState::new(Arc::new(Database::memory().expect("memory db")))
}

fn ok() -> bool {
    true
}

/// 大多数用例不关心系统钥匙串内容，每次给一个全新的、进程内泄漏的假钥匙串
/// （测试专用，`InMemorySecretStore` 不接系统钥匙串）。需要观察/预置钥匙串
/// 内容的用例（ANTHROPIC_API_KEY 往返）自己构造 `Arc<InMemorySecretStore>`
/// 并复用同一个实例，不调用这个函数。
fn ss() -> &'static dyn SecretStore {
    Box::leak(Box::new(InMemorySecretStore::new()))
}

fn params(model: &str, key: &str) -> ApplyParams {
    ApplyParams {
        model: model.to_string(),
        claude_slots: ClaudeSlots::default(),
        api_key: key.to_string(),
        gateway_root: GATEWAY.to_string(),
        capabilities: None,
    }
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn read_json_str(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn read_toml(path: &Path) -> toml::Value {
    toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn claude_settings(home: &Path) -> PathBuf {
    home.join(".claude/settings.json")
}

fn codex_config(home: &Path) -> PathBuf {
    home.join(".codex/config.toml")
}

fn codex_auth(home: &Path) -> PathBuf {
    home.join(".codex/auth.json")
}

/// 快照清单里数据库与本地设置部分的可比较视图。
fn db_view(
    state: &AppState,
    app: AppType,
    id: &str,
) -> (Option<Value>, Option<String>, bool, Option<String>) {
    let row = state
        .db
        .get_provider_by_id(id, app.as_str())
        .unwrap()
        .map(|p| p.settings_config);
    let current = state.db.get_current_provider(app.as_str()).unwrap();
    let backup = futures::executor::block_on(state.db.get_live_backup(app.as_str()))
        .unwrap()
        .is_some();
    (
        row,
        current,
        backup,
        crate::settings::get_current_provider(&app),
    )
}

// ---------------------------------------------------------------------------
// Claude Code
// ---------------------------------------------------------------------------

const CLAUDE_BASE: &str = r#"{
  "env": { "CUSTOM_FLAG": "1", "ANTHROPIC_API_KEY": "sk-user-own" },
  "hooks": { "PreToolUse": [ { "matcher": "Bash", "hooks": [ { "type": "command", "command": "echo hi" } ] } ] },
  "permissions": { "allow": ["Bash(ls:*)"] },
  "model": "opus"
}"#;

#[test]
#[serial]
fn claude_apply_twice_keeps_unmanaged_fields_and_converges_db_row() {
    let home = TestHome::new();
    let state = state();
    let path = claude_settings(home.path());
    write(&path, CLAUDE_BASE);
    let before = read_json(&path);

    let out = apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("claude-sonnet-4-5", KEY_A),
        &ok,
        ss(),
    )
    .unwrap();
    assert_eq!(out.model, "claude-sonnet-4-5");

    let mut second = params("claude-opus-4-1", KEY_B);
    second.claude_slots.haiku = Some("claude-haiku-4-5".into());
    apply::apply_provider_tool(&state, ProviderTool::ClaudeCode, &second, &ok, ss()).unwrap();

    let after = read_json(&path);
    let env = &after["env"];
    assert_eq!(env["ANTHROPIC_BASE_URL"], GATEWAY);
    assert_eq!(env["ANTHROPIC_AUTH_TOKEN"], KEY_B);
    assert_eq!(env["ANTHROPIC_MODEL"], "claude-opus-4-1");
    assert_eq!(env["ANTHROPIC_DEFAULT_SONNET_MODEL"], "claude-opus-4-1");
    assert_eq!(env["ANTHROPIC_DEFAULT_OPUS_MODEL"], "claude-opus-4-1");
    assert_eq!(env["ANTHROPIC_DEFAULT_HAIKU_MODEL"], "claude-haiku-4-5");
    assert!(
        env.get("ANTHROPIC_API_KEY").is_none(),
        "conflicting key must be removed"
    );
    assert_eq!(env["CUSTOM_FLAG"], "1");
    for k in ["hooks", "permissions", "model"] {
        assert_eq!(after[k], before[k], "unmanaged field {k} changed");
    }

    let (row, current, backup, local) = db_view(&state, AppType::Claude, apply::CLAUDE_PROVIDER_ID);
    let row = row.unwrap();
    assert_eq!(
        row["env"].as_object().unwrap().len(),
        6,
        "db row keeps managed env only: {row}"
    );
    assert!(row.get("hooks").is_none());
    assert!(!row.to_string().contains("sk-user-own"));
    assert_eq!(current.as_deref(), Some(apply::CLAUDE_PROVIDER_ID));
    assert_eq!(local.as_deref(), Some(apply::CLAUDE_PROVIDER_ID));
    assert!(!backup);
}

#[cfg(unix)]
#[test]
#[serial]
fn first_apply_creates_all_three_config_dirs_private_and_files_0600() {
    let home = TestHome::new();
    let state = state();
    apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        ss(),
    )
    .unwrap();
    for dir in [".claude", ".codex", ".workbuddy"] {
        assert_eq!(mode(&home.path().join(dir)), 0o700, "{dir}");
    }
    assert_eq!(mode(&claude_settings(home.path())), 0o600);
}

#[cfg(unix)]
#[test]
#[serial]
fn existing_open_dirs_and_files_are_tightened() {
    use std::os::unix::fs::PermissionsExt;
    let home = TestHome::new();
    let state = state();
    let path = claude_settings(home.path());
    write(&path, "{}");
    std::fs::set_permissions(
        home.path().join(".claude"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        ss(),
    )
    .unwrap();
    assert_eq!(mode(&home.path().join(".claude")), 0o700);
    assert_eq!(mode(&path), 0o600);
}

fn assert_claude_restored(state: &AppState, path: &Path, original: &str) {
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    assert_eq!(
        db_view(state, AppType::Claude, apply::CLAUDE_PROVIDER_ID),
        (None, None, false, None)
    );
}

#[test]
#[serial]
fn claude_failures_at_each_stage_restore_the_whole_snapshot() {
    for stage in [Stage::AfterUpsert, Stage::Switch, Stage::AfterLiveWrite] {
        let home = TestHome::new();
        let state = state();
        let path = claude_settings(home.path());
        write(&path, CLAUDE_BASE);
        let partial_path = path.clone();
        apply::set_test_hook(move |s| {
            if s != stage {
                return Ok(());
            }
            if s == Stage::Switch {
                // 模拟上游管道改了一半就报错。
                std::fs::write(&partial_path, "{\"env\":{}}").unwrap();
            }
            Err(format!("injected at {s:?}"))
        });
        let err = apply::apply_provider_tool(
            &state,
            ProviderTool::ClaudeCode,
            &params("m", KEY_A),
            &ok,
            ss(),
        )
        .unwrap_err();
        assert_eq!(err.code, apply::ERR_FAILED, "{stage:?}: {err}");
        assert_claude_restored(&state, &path, CLAUDE_BASE);
    }
}

#[test]
#[serial]
fn failure_after_a_previous_success_restores_the_previous_state() {
    let home = TestHome::new();
    let state = state();
    let path = claude_settings(home.path());
    write(&path, CLAUDE_BASE);
    apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m1", KEY_A),
        &ok,
        ss(),
    )
    .unwrap();
    let live_before = std::fs::read_to_string(&path).unwrap();
    let db_before = db_view(&state, AppType::Claude, apply::CLAUDE_PROVIDER_ID);

    apply::set_test_hook(|s| {
        if s == Stage::AfterLiveWrite {
            Err("boom".into())
        } else {
            Ok(())
        }
    });
    apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m2", KEY_B),
        &ok,
        ss(),
    )
    .unwrap_err();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), live_before);
    assert_eq!(
        db_view(&state, AppType::Claude, apply::CLAUDE_PROVIDER_ID),
        db_before
    );
}

#[test]
#[serial]
fn external_write_before_rollback_is_not_overwritten() {
    let home = TestHome::new();
    let state = state();
    let path = claude_settings(home.path());
    write(&path, CLAUDE_BASE);
    let ext = path.clone();
    apply::set_test_hook(move |s| {
        if s == Stage::AfterLiveWrite {
            std::fs::write(&ext, "{\"written_by\":\"other\"}").unwrap();
            return Err("boom".into());
        }
        Ok(())
    });
    let err = apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        ss(),
    )
    .unwrap_err();
    assert_eq!(err.code, apply::ERR_EXTERNAL, "{err}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{\"written_by\":\"other\"}"
    );
    let snapshots = home.path().join(".we2ai/apply-snapshots");
    let copies: Vec<_> = std::fs::read_dir(&snapshots).unwrap().collect();
    assert_eq!(copies.len(), 1);
    let copy = copies[0].as_ref().unwrap().path();
    assert_eq!(std::fs::read_to_string(copy).unwrap(), CLAUDE_BASE);
    // 数据库仍然回滚。
    assert_eq!(
        db_view(&state, AppType::Claude, apply::CLAUDE_PROVIDER_ID),
        (None, None, false, None)
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn legacy_claude_json_is_the_fixed_target_and_is_restored_private() {
    let home = TestHome::new();
    let state = state();
    let legacy = home.path().join(".claude/claude.json");
    write(&legacy, "{\"env\":{\"X\":\"1\"}}");
    apply::set_test_hook(|s| {
        if s == Stage::AfterLiveWrite {
            Err("boom".into())
        } else {
            Ok(())
        }
    });
    apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        ss(),
    )
    .unwrap_err();
    assert_eq!(
        std::fs::read_to_string(&legacy).unwrap(),
        "{\"env\":{\"X\":\"1\"}}"
    );
    assert_eq!(mode(&legacy), 0o600);
    assert!(!claude_settings(home.path()).exists());
}

#[test]
#[serial]
fn a_second_provider_row_blocks_apply_before_any_write() {
    let home = TestHome::new();
    let state = state();
    let path = claude_settings(home.path());
    write(&path, CLAUDE_BASE);
    let other = crate::provider::Provider::with_id(
        "other".into(),
        "Other".into(),
        json!({"env": {}}),
        None,
    );
    state.db.save_provider("claude", &other).unwrap();
    let err = apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        ss(),
    )
    .unwrap_err();
    assert_eq!(err.code, apply::ERR_PRECONDITION);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), CLAUDE_BASE);
}

const TAKEN_OVER: &str = r#"{"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:15721","ANTHROPIC_AUTH_TOKEN":"PROXY_MANAGED"}}"#;

#[test]
#[serial]
fn proxy_takeover_in_live_or_backup_blocks_apply() {
    let home = TestHome::new();
    let state = state();
    let path = claude_settings(home.path());
    write(&path, TAKEN_OVER);
    let err = apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        ss(),
    )
    .unwrap_err();
    assert_eq!(err.code, apply::ERR_TAKEOVER);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), TAKEN_OVER);

    write(&path, CLAUDE_BASE);
    futures::executor::block_on(state.db.save_live_backup("claude", "{}")).unwrap();
    let err = apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        ss(),
    )
    .unwrap_err();
    assert_eq!(err.code, apply::ERR_TAKEOVER);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), CLAUDE_BASE);
}

/// 前置检查之后、switch 之前 CC Switch 开启接管：上游 switch 的热切换拒绝
/// 生效，apply 返回冲突，CC Switch 写入的代理地址与占位认证原样保留，WE2AI
/// 数据库没有 proxy_live_backup。
#[test]
#[serial]
fn takeover_starting_right_before_switch_is_refused_and_left_intact() {
    let home = TestHome::new();
    let state = state();
    let path = claude_settings(home.path());
    write(&path, CLAUDE_BASE);
    let p = path.clone();
    apply::set_test_hook(move |s| {
        if s == Stage::BeforeSwitch {
            std::fs::write(&p, TAKEN_OVER).unwrap();
        }
        Ok(())
    });
    let err = apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        ss(),
    )
    .unwrap_err();
    assert_eq!(err.code, apply::ERR_TAKEOVER, "{err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), TAKEN_OVER);
    assert_eq!(
        db_view(&state, AppType::Claude, apply::CLAUDE_PROVIDER_ID),
        (None, None, false, None)
    );
}

#[test]
#[serial]
fn concurrent_applies_to_the_same_tool_queue_and_both_finish() {
    let home = TestHome::new();
    let state = state();
    write(&claude_settings(home.path()), "{}");
    std::thread::scope(|s| {
        let a = s.spawn(|| {
            apply::apply_provider_tool(
                &state,
                ProviderTool::ClaudeCode,
                &params("m1", KEY_A),
                &ok,
                ss(),
            )
        });
        let b = s.spawn(|| {
            apply::apply_provider_tool(
                &state,
                ProviderTool::ClaudeCode,
                &params("m2", KEY_B),
                &ok,
                ss(),
            )
        });
        a.join().unwrap().unwrap();
        b.join().unwrap().unwrap();
    });
    let model = read_json(&claude_settings(home.path()))["env"]["ANTHROPIC_MODEL"].clone();
    assert!(model == "m1" || model == "m2");
}

#[test]
#[serial]
fn logout_scrub_removes_keys_from_managed_rows_but_keeps_live_files() {
    let home = TestHome::new();
    let state = state();
    write(&codex_config(home.path()), "");
    apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        ss(),
    )
    .unwrap();
    apply::apply_provider_tool(
        &state,
        ProviderTool::Codex,
        &params("gpt-5", KEY_B),
        &ok,
        ss(),
    )
    .unwrap();
    let claude_live = std::fs::read_to_string(claude_settings(home.path())).unwrap();

    apply::scrub_managed_provider_keys(&state).unwrap();

    for (app, id) in [
        ("claude", apply::CLAUDE_PROVIDER_ID),
        ("codex", apply::CODEX_PROVIDER_ID),
    ] {
        let row = state.db.get_provider_by_id(id, app).unwrap().unwrap();
        let text = row.settings_config.to_string();
        assert!(
            !text.contains(KEY_A) && !text.contains(KEY_B),
            "{app}: {text}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(claude_settings(home.path())).unwrap(),
        claude_live
    );
    // 清除后仍可再次指定。
    apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m2", KEY_A),
        &ok,
        ss(),
    )
    .unwrap();
}

#[test]
#[serial]
fn logout_material_cleanup_empties_backups_and_drops_proxy_backups() {
    let home = TestHome::new();
    let state = state();
    apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        ss(),
    )
    .unwrap();
    let backups = home.path().join(".we2ai/backups");
    std::fs::create_dir_all(backups.join("nested")).unwrap();
    std::fs::write(backups.join("db_backup_1.db"), KEY_A).unwrap();
    std::fs::write(backups.join("nested/x.db"), KEY_A).unwrap();
    futures::executor::block_on(state.db.save_live_backup("codex", "{}")).unwrap();

    apply::clear_local_key_material(&state, &ok).unwrap();

    assert_eq!(std::fs::read_dir(&backups).unwrap().count(), 0);
    assert!(
        futures::executor::block_on(state.db.get_live_backup("codex"))
            .unwrap()
            .is_none()
    );
    let row = state
        .db
        .get_provider_by_id(apply::CLAUDE_PROVIDER_ID, "claude")
        .unwrap()
        .unwrap();
    assert!(!row.settings_config.to_string().contains(KEY_A));
}

/// 取 Key 后已登出：apply 在锁内复查会话失败，不写任何东西。
#[test]
#[serial]
fn apply_after_the_session_changed_writes_nothing() {
    let home = TestHome::new();
    let state = state();
    let path = claude_settings(home.path());
    write(&path, CLAUDE_BASE);
    let gone = || false;
    let err = apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &gone,
        ss(),
    )
    .unwrap_err();
    assert_eq!(err.code, apply::ERR_SESSION_CHANGED);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), CLAUDE_BASE);
    assert_eq!(
        db_view(&state, AppType::Claude, apply::CLAUDE_PROVIDER_ID),
        (None, None, false, None)
    );
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    let err = workbuddy::apply_workbuddy(&root, &params("m", KEY_A), false, &gone).unwrap_err();
    assert_eq!(err.code, apply::ERR_SESSION_CHANGED);
    assert!(!wb_models(home.path()).exists());
}

/// 登出清理与进行中的 apply 交错：清理等 apply 结束后才执行，最终数据库行
/// 不含 Key（不会出现"清理完又写回"）。
#[test]
#[serial]
fn logout_cleanup_waits_for_an_in_flight_apply_and_then_removes_its_key() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    let _home = TestHome::new();
    let state = state();
    let (reached_tx, reached_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let cleanup_done = AtomicBool::new(false);
    std::thread::scope(|s| {
        let applier = s.spawn(|| {
            apply::set_test_hook(move |stage| {
                if stage == Stage::AfterUpsert {
                    reached_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                }
                Ok(())
            });
            let r = apply::apply_provider_tool(
                &state,
                ProviderTool::ClaudeCode,
                &params("m", KEY_A),
                &ok,
                ss(),
            );
            apply::clear_test_hook();
            r
        });
        reached_rx.recv().unwrap();
        let cleaner = s.spawn(|| {
            apply::clear_local_key_material(&state, &ok).unwrap();
            cleanup_done.store(true, Ordering::SeqCst);
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        // 先放行再断言：断言失败时不能让 apply 线程永远卡在钩子里。
        let finished_early = cleanup_done.load(Ordering::SeqCst);
        release_tx.send(()).unwrap();
        applier.join().unwrap().unwrap();
        cleaner.join().unwrap();
        assert!(!finished_early, "cleanup must wait for the apply lock");
    });
    let row = state
        .db
        .get_provider_by_id(apply::CLAUDE_PROVIDER_ID, "claude")
        .unwrap()
        .unwrap();
    assert!(!row.settings_config.to_string().contains(KEY_A));
}

/// 备份目录里有删不掉的条目：清理必须报失败，不能误报成功清除待清理标记。
#[cfg(unix)]
#[test]
#[serial]
fn backup_entries_that_cannot_be_removed_make_cleanup_fail() {
    use std::os::unix::fs::PermissionsExt;
    let home = TestHome::new();
    let state = state();
    let locked = home.path().join(".we2ai/backups/locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("db_backup.db"), "x").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = apply::clear_local_key_material(&state, &ok);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        result.is_err(),
        "cleanup must not report success: {result:?}"
    );
    assert!(apply::key_material_residue(&state));
}

/// 登出后、清理拿到锁之前又登录并指定了模型：旧登出的清理不动新会话的 Key。
#[test]
#[serial]
fn stale_logout_cleanup_skips_material_owned_by_a_new_session() {
    let home = TestHome::new();
    let state = state();
    apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_B),
        &ok,
        ss(),
    )
    .unwrap();
    let backups = home.path().join(".we2ai/backups");
    std::fs::create_dir_all(&backups).unwrap();
    std::fs::write(backups.join("db_backup.db"), "x").unwrap();
    let logged_in_again = || false;
    apply::clear_local_key_material(&state, &logged_in_again).unwrap();
    let row = state
        .db
        .get_provider_by_id(apply::CLAUDE_PROVIDER_ID, "claude")
        .unwrap()
        .unwrap();
    assert!(row.settings_config.to_string().contains(KEY_B));
    assert!(backups.join("db_backup.db").exists());
}

/// 不依赖持久标记：直接检查供应商行、代理备份与备份目录里的残留。
#[test]
#[serial]
fn key_material_residue_is_detected_without_the_marker() {
    let home = TestHome::new();
    let state = state();
    assert!(!apply::key_material_residue(&state));
    apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        ss(),
    )
    .unwrap();
    assert!(apply::key_material_residue(&state));
    apply::clear_local_key_material(&state, &ok).unwrap();
    assert!(!apply::key_material_residue(&state));

    futures::executor::block_on(state.db.save_live_backup("codex", "{}")).unwrap();
    assert!(apply::key_material_residue(&state));
    futures::executor::block_on(state.db.delete_live_backup("codex")).unwrap();

    let backups = home.path().join(".we2ai/backups");
    std::fs::create_dir_all(&backups).unwrap();
    std::fs::write(backups.join("db_backup.db"), "x").unwrap();
    assert!(apply::key_material_residue(&state));
}

#[test]
#[serial]
fn material_cleanup_marker_survives_restart_until_cleared() {
    let home = TestHome::new();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    assert!(!apply::material_cleanup_pending(&root));
    apply::set_material_cleanup_pending(&root, true).unwrap();
    // 模拟重启：只靠磁盘标记。
    assert!(apply::material_cleanup_pending(&root));
    let marker = std::fs::read_dir(&root)
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy() == "key_material_cleanup_pending");
    assert!(marker.is_some());
    apply::set_material_cleanup_pending(&root, false).unwrap();
    assert!(!apply::material_cleanup_pending(&root));
    apply::set_material_cleanup_pending(&root, false).unwrap();
}

// ---------------------------------------------------------------------------
// 用户自己的 ANTHROPIC_API_KEY 保护（P6 方案决定 2）
// ---------------------------------------------------------------------------

/// apply 删除用户自己的 `ANTHROPIC_API_KEY` 前，先把它保存到系统钥匙串
/// （这里用注入的 `InMemorySecretStore` 断言，不依赖真实系统钥匙串）。
#[test]
#[serial]
fn claude_apply_saves_the_users_own_api_key_before_removing_it() {
    let home = TestHome::new();
    let state = state();
    write(&claude_settings(home.path()), CLAUDE_BASE);
    let store = InMemorySecretStore::new();
    apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        &store,
    )
    .unwrap();
    assert_eq!(
        store
            .get(secret_store::SERVICE_NAME, apply::CLAUDE_API_KEY_ACCOUNT)
            .unwrap(),
        Some("sk-user-own".to_string())
    );
    assert!(read_json(&claude_settings(home.path()))["env"]
        .get("ANTHROPIC_API_KEY")
        .is_none());
}

/// live 里没有用户自己的 Key：不调用钥匙串，也不会用"没有"覆盖掉已保存的值。
#[test]
#[serial]
fn claude_apply_does_not_touch_the_keychain_when_live_has_no_api_key() {
    let home = TestHome::new();
    let state = state();
    write(&claude_settings(home.path()), "{}");
    let store = InMemorySecretStore::new();
    store
        .set(
            secret_store::SERVICE_NAME,
            apply::CLAUDE_API_KEY_ACCOUNT,
            "sk-previously-saved",
        )
        .unwrap();
    apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        &store,
    )
    .unwrap();
    assert_eq!(
        store
            .get(secret_store::SERVICE_NAME, apply::CLAUDE_API_KEY_ACCOUNT)
            .unwrap(),
        Some("sk-previously-saved".to_string()),
        "an empty live key must not overwrite a previously saved one"
    );
}

/// 钥匙串保存失败：整次 apply 中止，live 文件与数据库都不写入——用户的 Key
/// 绝不能被静默销毁。
#[test]
#[serial]
fn claude_apply_aborts_and_writes_nothing_when_saving_the_users_api_key_fails() {
    let home = TestHome::new();
    let state = state();
    let path = claude_settings(home.path());
    write(&path, CLAUDE_BASE);
    let store = InMemorySecretStore::new();
    store.set_fail_set(true);
    let err = apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        &store,
    )
    .unwrap_err();
    assert_eq!(err.code, apply::ERR_FAILED, "{err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), CLAUDE_BASE);
    assert_eq!(
        db_view(&state, AppType::Claude, apply::CLAUDE_PROVIDER_ID),
        (None, None, false, None)
    );
}

// ---------------------------------------------------------------------------
// 恢复官方配置（P6：`restore_official`，取代功能 12 的 `remove_tool_keys`）
// ---------------------------------------------------------------------------

/// 恢复确认弹窗展示的计划：Claude 六个托管键全部标"移除"、`ANTHROPIC_API_KEY`
/// 标"写回"（不是 apply 计划里的"删除"——恢复的语义完全相反，Opus 复核中危
/// 项 2）。
#[test]
fn restore_plan_for_claude_lists_all_six_managed_keys_as_removed() {
    let plan = apply::restore_plan_for(RestoreTool::ClaudeCode);
    for key in [
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    ] {
        assert!(
            plan.fields
                .iter()
                .any(|f| f.contains(key) && f.contains("移除")),
            "missing {key} in {:?}",
            plan.fields
        );
    }
    assert!(
        plan.fields
            .iter()
            .any(|f| f.contains("ANTHROPIC_API_KEY") && f.contains("写回")),
        "{:?}",
        plan.fields
    );
    assert!(
        plan.fields.iter().all(|f| !f.contains("删除")),
        "{:?}",
        plan.fields
    );
}

/// Codex 恢复计划只列 `config.toml`（apply 计划额外列出模型目录文件，恢复
/// 从不触碰它）；字段标注移除，并说明 `auth.json` 不受影响。
#[test]
fn restore_plan_for_codex_lists_only_config_toml_and_notes_auth_json_untouched() {
    let plan = apply::restore_plan_for(RestoreTool::Codex);
    assert_eq!(plan.files.len(), 1, "{:?}", plan.files);
    assert!(plan.files[0].ends_with("config.toml"), "{:?}", plan.files);
    assert!(plan.fields.iter().any(|f| f.contains("auth.json")));
}

/// 文件已经写回用户的 Key，但钥匙串条目删不掉：不算恢复失败（`restored`
/// 仍记这个文件），额外报告一条钥匙串清理失败的提示（Codex 复核中危项 3）。
#[test]
#[serial]
fn restore_claude_reports_but_does_not_fail_when_keychain_delete_fails_after_write() {
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    let path = claude_settings(home.path());
    write(&path, CLAUDE_BASE);
    let store = InMemorySecretStore::new();
    apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        &store,
    )
    .unwrap();
    store.set_fail_delete(true);

    let out = apply::restore_official(&state, &root, &store, &[RestoreTool::ClaudeCode], &ok);
    assert_eq!(out.restored.len(), 1, "{out:?}");
    assert!(
        out.skipped
            .iter()
            .any(|s| s.contains("Claude Code") && s.contains("钥匙串")),
        "{out:?}"
    );
    let after = read_json(&path);
    assert_eq!(
        after["env"]["ANTHROPIC_API_KEY"], "sk-user-own",
        "the file must still be restored even though the keychain cleanup failed"
    );
    assert!(
        store.contains(secret_store::SERVICE_NAME, apply::CLAUDE_API_KEY_ACCOUNT),
        "entry must remain since delete failed"
    );
}

/// 上一次恢复删钥匙串失败留下的残留，在下一次恢复（此时 live 已经不再指向
/// WE2AI）时被自动清理：钥匙串里的值与 live 当前的 Key 一致才删，成功不
/// 产生任何消息（Codex 复核中危项 3）。
#[test]
#[serial]
fn a_later_restore_call_cleans_up_a_previously_undeletable_keychain_entry() {
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    let path = claude_settings(home.path());
    write(&path, CLAUDE_BASE);
    let store = InMemorySecretStore::new();
    apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        &store,
    )
    .unwrap();
    store.set_fail_delete(true);
    let first = apply::restore_official(&state, &root, &store, &[RestoreTool::ClaudeCode], &ok);
    assert_eq!(first.restored.len(), 1, "{first:?}");
    assert!(store.contains(secret_store::SERVICE_NAME, apply::CLAUDE_API_KEY_ACCOUNT));

    // 第二次恢复：钥匙串现在能正常删除了；live 已经不指向 WE2AI（上一次已经
    // 恢复过），走"没有可做的事"分支，顺手清理陈旧残留。
    store.set_fail_delete(false);
    let second = apply::restore_official(&state, &root, &store, &[RestoreTool::ClaudeCode], &ok);
    assert!(second.restored.is_empty(), "{second:?}");
    assert!(
        second.skipped.is_empty(),
        "cleanup success must be silent: {second:?}"
    );
    assert!(
        !store.contains(secret_store::SERVICE_NAME, apply::CLAUDE_API_KEY_ACCOUNT),
        "stale entry must be cleaned up now that it matches live"
    );
}

/// live 已经有自己的 `ANTHROPIC_API_KEY`（不是空的）：钥匙串里保存的旧 Key
/// 不会覆盖它，钥匙串条目也不会被动用（既没写回也没删除）。
#[test]
#[serial]
fn restore_claude_does_not_overwrite_an_api_key_already_present_in_live() {
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    let path = claude_settings(home.path());
    write(&path, CLAUDE_BASE);
    let store = InMemorySecretStore::new();
    apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        &store,
    )
    .unwrap();
    assert!(store.contains(secret_store::SERVICE_NAME, apply::CLAUDE_API_KEY_ACCOUNT));
    // 模拟用户在恢复前手动往 live 里加回了一个新 Key（与钥匙串保存的旧
    // Key 不同）。
    let mut value = read_json(&path);
    value["env"]["ANTHROPIC_API_KEY"] = json!("sk-manually-added");
    write(&path, &serde_json::to_string(&value).unwrap());

    let out = apply::restore_official(&state, &root, &store, &[RestoreTool::ClaudeCode], &ok);
    assert_eq!(out.restored.len(), 1, "{out:?}");
    let after = read_json(&path);
    assert_eq!(
        after["env"]["ANTHROPIC_API_KEY"], "sk-manually-added",
        "must not overwrite an API key already present in live"
    );
    assert!(
        store.contains(secret_store::SERVICE_NAME, apply::CLAUDE_API_KEY_ACCOUNT),
        "saved key must be left untouched since it was neither used nor stale"
    );
}

/// 恢复移除的是 WE2AI 写入的一切（不只是 Key）：Claude 的全部托管 env 键、
/// Codex 的顶层字段与整张 we2ai 表、WorkBuddy 的托管条目；用户自己的
/// `ANTHROPIC_API_KEY` 在 apply 时已被保存，恢复时写回；数据库两条固定 id
/// 行与 current 标记被清空，恢复后不再有 Key 残留。
#[test]
#[serial]
fn restore_official_strips_all_we2ai_content_and_restores_saved_api_key() {
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    write(&claude_settings(home.path()), CLAUDE_BASE);
    write(&codex_config(home.path()), CODEX_BASE);
    write(&wb_models(home.path()), &format!("[{USER_ENTRY}]"));
    let store: Arc<InMemorySecretStore> = Arc::new(InMemorySecretStore::new());
    let store_ref: &dyn SecretStore = store.as_ref();
    apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        store_ref,
    )
    .unwrap();
    apply::apply_provider_tool(
        &state,
        ProviderTool::Codex,
        &params("gpt-5", KEY_A),
        &ok,
        ss(),
    )
    .unwrap();
    workbuddy::apply_workbuddy(&root, &params("glm", KEY_A), false, &ok).unwrap();
    // 用户原本自己的 ANTHROPIC_API_KEY（CLAUDE_BASE 里的 "sk-user-own"）已被
    // 保存到钥匙串，不在 live 文件里了。
    assert!(store.contains(secret_store::SERVICE_NAME, apply::CLAUDE_API_KEY_ACCOUNT));
    assert!(read_json(&claude_settings(home.path()))["env"]
        .get("ANTHROPIC_API_KEY")
        .is_none());

    let out = apply::restore_official(
        &state,
        &root,
        store_ref,
        &[
            RestoreTool::ClaudeCode,
            RestoreTool::Codex,
            RestoreTool::Workbuddy,
        ],
        &ok,
    );
    assert_eq!(out.restored.len(), 3, "{out:?}");
    assert!(out.unchanged.is_empty(), "{out:?}");
    assert!(out.skipped.is_empty(), "{out:?}");

    let claude = read_json(&claude_settings(home.path()));
    for key in [
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    ] {
        assert!(claude["env"].get(key).is_none(), "{key} not removed");
    }
    assert_eq!(claude["env"]["CUSTOM_FLAG"], "1");
    assert_eq!(claude["hooks"], read_json_str(CLAUDE_BASE)["hooks"]);
    // 用户自己的 Key 写回，钥匙串条目随之删除。
    assert_eq!(claude["env"]["ANTHROPIC_API_KEY"], "sk-user-own");
    assert!(!store.contains(secret_store::SERVICE_NAME, apply::CLAUDE_API_KEY_ACCOUNT));

    let codex = read_toml(&codex_config(home.path()));
    assert!(codex.get("model_provider").is_none());
    assert!(codex.get("model").is_none());
    assert!(codex
        .get("model_providers")
        .and_then(|p| p.get("we2ai"))
        .is_none());
    assert_eq!(
        codex["model_providers"]["mine"]["experimental_bearer_token"].as_str(),
        Some("sk-mine-secret"),
        "user's own provider token untouched"
    );

    let items = wb_items(home.path());
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], "deepseek-v3");
    assert!(workbuddy::load_record(&root).is_none());

    assert!(!apply::key_material_residue(&state));
}

#[test]
#[serial]
fn restore_official_leaves_non_we2ai_claude_and_edited_workbuddy_entries() {
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    let other = r#"{"env":{"ANTHROPIC_BASE_URL":"https://other.example","ANTHROPIC_AUTH_TOKEN":"sk-other"}}"#;
    write(&claude_settings(home.path()), other);
    workbuddy::apply_workbuddy(&root, &params("glm", KEY_A), false, &ok).unwrap();
    let mut items = wb_items(home.path());
    items[0]["name"] = json!("改过");
    write(
        &wb_models(home.path()),
        &serde_json::to_string(&items).unwrap(),
    );

    let out = apply::restore_official(
        &state,
        &root,
        ss(),
        &[RestoreTool::ClaudeCode, RestoreTool::Workbuddy],
        &ok,
    );
    assert!(out.restored.is_empty(), "{out:?}");
    // Claude 不指向 WE2AI 是"本来就没有可做的事"，不是失败：进 unchanged，
    // 不能让"三个工具只指定了一个"这种正常情况在登出恢复时弹出警告 toast
    // （Opus 复核高危项 1）。WorkBuddy 条目被手工修改过是真失败，留在
    // skipped。
    assert_eq!(out.unchanged.len(), 1, "{out:?}");
    assert!(
        out.unchanged
            .iter()
            .any(|s| s.contains("Claude Code") && s.contains("未指向 WE2AI")),
        "{out:?}"
    );
    assert_eq!(out.skipped.len(), 1, "{out:?}");
    assert!(
        out.skipped.iter().any(|s| s.contains("手工修改")),
        "{out:?}"
    );
    assert_eq!(
        std::fs::read_to_string(claude_settings(home.path())).unwrap(),
        other
    );
    assert_eq!(wb_items(home.path()).len(), 1);
}

/// 配置损坏或不可读时报告为未恢复，不静默成功。
#[test]
#[serial]
fn restore_official_reports_unreadable_or_corrupt_configs() {
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    write(&claude_settings(home.path()), "{ not json");
    write(&codex_config(home.path()), "[[[ not toml");
    let out = apply::restore_official(
        &state,
        &root,
        ss(),
        &[RestoreTool::ClaudeCode, RestoreTool::Codex],
        &ok,
    );
    assert!(out.restored.is_empty(), "{out:?}");
    assert_eq!(out.skipped.len(), 2, "{out:?}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = claude_settings(home.path());
        std::fs::write(
            &path,
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://api.we2ai.com","ANTHROPIC_AUTH_TOKEN":"k"}}"#,
        )
        .unwrap();
        std::fs::write(codex_config(home.path()), "").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let out = apply::restore_official(&state, &root, ss(), &[RestoreTool::ClaudeCode], &ok);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            out.skipped.iter().any(|s| s.contains("读取失败")),
            "{out:?}"
        );
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("ANTHROPIC_AUTH_TOKEN"));
    }
}

/// 用户把 we2ai 表改成自有端点：不动它，并如实报告没有可恢复的内容。
#[test]
#[serial]
fn restore_official_skips_a_we2ai_codex_table_pointing_elsewhere() {
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    let cfg = "model_provider = \"we2ai\"\n[model_providers.we2ai]\nbase_url = \"https://mine.example/v1\"\nexperimental_bearer_token = \"sk-mine\"\n";
    write(&codex_config(home.path()), cfg);
    let out = apply::restore_official(&state, &root, ss(), &[RestoreTool::Codex], &ok);
    // 顶层 model_provider == "we2ai" 触发移除，但整张表 base_url 不再指向
    // WE2AI，所以只删了顶层两个字段、保留了表本身（与旧 remove_tool_keys 对
    // Codex 的判定不同：那时整张表都不会碰；本命令的顶层字段判定与表判定各
    // 自独立，见 restore_codex 的实现说明）。
    assert_eq!(out.restored.len(), 1, "{out:?}");
    let after = std::fs::read_to_string(codex_config(home.path())).unwrap();
    assert!(!after.contains("model_provider ="), "{after}");
    assert!(after.contains("experimental_bearer_token = \"sk-mine\""));
}

/// 顶层不指向 WE2AI（用户已经手动切换）：完全不动这个文件。
#[test]
#[serial]
fn restore_official_codex_untouched_when_model_provider_is_not_we2ai() {
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    write(&codex_config(home.path()), CODEX_BASE);
    let out = apply::restore_official(&state, &root, ss(), &[RestoreTool::Codex], &ok);
    assert!(out.restored.is_empty(), "{out:?}");
    assert_eq!(out.unchanged.len(), 1, "{out:?}");
    assert!(out.skipped.is_empty(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(codex_config(home.path())).unwrap(),
        CODEX_BASE
    );
}

/// CC Switch 正在代理接管（`ANTHROPIC_AUTH_TOKEN` 变成占位符）：即便
/// `ANTHROPIC_BASE_URL` 仍是 WE2AI 网关，也完全不动这个文件——CC Switch 热
/// 切换会保留 `model_provider`/字段名不变、只换地址与凭据，先判断"指向
/// WE2AI"再删字段会误删 CC Switch 正在依赖的内容。
#[test]
#[serial]
fn restore_official_does_not_touch_claude_live_under_cc_switch_takeover() {
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    let taken_over_but_our_gateway = format!(
        r#"{{"env":{{"ANTHROPIC_BASE_URL":"{GATEWAY}","ANTHROPIC_AUTH_TOKEN":"PROXY_MANAGED"}}}}"#
    );
    write(&claude_settings(home.path()), &taken_over_but_our_gateway);
    let out = apply::restore_official(&state, &root, ss(), &[RestoreTool::ClaudeCode], &ok);
    assert!(out.restored.is_empty(), "{out:?}");
    assert_eq!(out.unchanged.len(), 1, "{out:?}");
    assert!(out.skipped.is_empty(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(claude_settings(home.path())).unwrap(),
        taken_over_but_our_gateway
    );
}

/// 只有 `model` 前有注释（`model_provider` 没有）：注释接到恢复后新的第一个
/// 键上（Codex 复核中危项 2）。
#[test]
#[serial]
fn restore_codex_comment_moves_when_only_model_is_commented() {
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    let cfg = "model_provider = \"we2ai\"\n# only model comment\nmodel = \"gpt-5\"\napproval_policy = \"on-request\"\n\n[model_providers.we2ai]\nname = \"WE2AI\"\nbase_url = \"https://api.we2ai.com/v1\"\nwire_api = \"responses\"\nexperimental_bearer_token = \"sk-x\"\n";
    write(&codex_config(home.path()), cfg);
    let out = apply::restore_official(&state, &root, ss(), &[RestoreTool::Codex], &ok);
    assert_eq!(out.restored.len(), 1, "{out:?}");
    let text = std::fs::read_to_string(codex_config(home.path())).unwrap();
    assert_eq!(
        text.matches("only model comment").count(),
        1,
        "comment must appear exactly once:\n{text}"
    );
    assert!(
        text.contains("# only model comment\napproval_policy = \"on-request\""),
        "comment must move to the new first key:\n{text}"
    );
}

/// 同一行末尾的注释（`model = "gpt-5" # trailing note`）也要保留，不能只顾
/// 键前面的 leading 注释（P6 三轮 Codex 验收中危项 3）。
#[test]
#[serial]
fn restore_codex_preserves_a_trailing_comment_on_the_same_line_as_model() {
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    let cfg = "model_provider = \"we2ai\"\nmodel = \"gpt-5\" # trailing note\napproval_policy = \"on-request\"\n\n[model_providers.we2ai]\nname = \"WE2AI\"\nbase_url = \"https://api.we2ai.com/v1\"\nwire_api = \"responses\"\nexperimental_bearer_token = \"sk-x\"\n";
    write(&codex_config(home.path()), cfg);
    let out = apply::restore_official(&state, &root, ss(), &[RestoreTool::Codex], &ok);
    assert_eq!(out.restored.len(), 1, "{out:?}");
    let text = std::fs::read_to_string(codex_config(home.path())).unwrap();
    assert_eq!(
        text.matches("trailing note").count(),
        1,
        "comment must appear exactly once:\n{text}"
    );
    assert!(
        text.contains("# trailing note\napproval_policy = \"on-request\""),
        "trailing comment must be preserved as its own line before the new first key:\n{text}"
    );
}

/// 恢复两次不会重复注释：第二次因为 `model_provider` 已经不在了，天然是
/// 空转（`Ok(None)`），文件逐字节不变（P6 三轮 Codex 验收中危项 3）。
#[test]
#[serial]
fn restoring_codex_twice_does_not_duplicate_comments() {
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    let cfg = "# c1\nmodel_provider = \"we2ai\"\nmodel = \"gpt-5\" # trailing note\napproval_policy = \"on-request\"\n\n[model_providers.we2ai]\nname = \"WE2AI\"\nbase_url = \"https://api.we2ai.com/v1\"\nwire_api = \"responses\"\nexperimental_bearer_token = \"sk-x\"\n";
    write(&codex_config(home.path()), cfg);
    let first = apply::restore_official(&state, &root, ss(), &[RestoreTool::Codex], &ok);
    assert_eq!(first.restored.len(), 1, "{first:?}");
    let after_first = std::fs::read_to_string(codex_config(home.path())).unwrap();

    let second = apply::restore_official(&state, &root, ss(), &[RestoreTool::Codex], &ok);
    assert!(second.restored.is_empty(), "{second:?}");
    let after_second = std::fs::read_to_string(codex_config(home.path())).unwrap();
    assert_eq!(
        after_first, after_second,
        "a second restore must be a no-op and must not duplicate any comment"
    );
    for comment in ["c1", "trailing note"] {
        assert_eq!(
            after_second.matches(comment).count(),
            1,
            "{comment} must still appear exactly once after a second restore:\n{after_second}"
        );
    }
}

/// 两个键都有注释，且恢复后新的第一个键自己也有注释：三段注释按原始顺序
/// 拼接，都保留，谁的都不丢（Codex 复核中危项 2）。
#[test]
#[serial]
fn restore_codex_concatenates_both_removed_comments_and_keeps_the_next_keys_own_comment() {
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    let cfg = "# c1\nmodel_provider = \"we2ai\"\n# c2\nmodel = \"gpt-5\"\n# own\napproval_policy = \"on-request\"\n\n[model_providers.we2ai]\nname = \"WE2AI\"\nbase_url = \"https://api.we2ai.com/v1\"\nwire_api = \"responses\"\nexperimental_bearer_token = \"sk-x\"\n";
    write(&codex_config(home.path()), cfg);
    let out = apply::restore_official(&state, &root, ss(), &[RestoreTool::Codex], &ok);
    assert_eq!(out.restored.len(), 1, "{out:?}");
    let text = std::fs::read_to_string(codex_config(home.path())).unwrap();
    for comment in ["c1", "c2", "own"] {
        assert_eq!(
            text.matches(comment).count(),
            1,
            "{comment} must appear exactly once:\n{text}"
        );
    }
    assert!(
        text.contains("# c1\n# c2\n# own\napproval_policy = \"on-request\""),
        "comments must be concatenated in original order and prepended, not replacing the existing comment:\n{text}"
    );
}

/// 恢复后顶层不剩任何普通键（只剩表）：注释落到文档级 leading 文本上，出现
/// 在第一张表之前，不丢失（Codex 复核中危项 2）。
#[test]
#[serial]
fn restore_codex_keeps_comments_as_document_leading_text_when_only_tables_remain() {
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    let cfg = "# c1\nmodel_provider = \"we2ai\"\n# c2\nmodel = \"gpt-5\"\n\n[model_providers.we2ai]\nname = \"WE2AI\"\nbase_url = \"https://api.we2ai.com/v1\"\nwire_api = \"responses\"\nexperimental_bearer_token = \"sk-x\"\n\n[mcp_servers.docs]\ncommand = \"npx\"\n";
    write(&codex_config(home.path()), cfg);
    let out = apply::restore_official(&state, &root, ss(), &[RestoreTool::Codex], &ok);
    assert_eq!(out.restored.len(), 1, "{out:?}");
    let text = std::fs::read_to_string(codex_config(home.path())).unwrap();
    for comment in ["c1", "c2"] {
        assert_eq!(
            text.matches(comment).count(),
            1,
            "{comment} must appear exactly once:\n{text}"
        );
    }
    assert!(
        text.starts_with("# c1\n# c2\n"),
        "comments must lead the document before the first remaining table:\n{text}"
    );
    assert!(text.contains("[mcp_servers.docs]"));
    assert!(!text.contains("model_providers"));
}

/// 恢复保留注释、其他 provider 表、mcp_servers、profiles，`auth.json` 逐字节
/// 不变。
#[test]
#[serial]
fn restore_official_codex_preserves_comments_other_providers_and_auth_json() {
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    write(&codex_config(home.path()), CODEX_BASE);
    write(&codex_auth(home.path()), CHATGPT_AUTH);
    apply::apply_provider_tool(
        &state,
        ProviderTool::Codex,
        &params("gpt-5", KEY_A),
        &ok,
        ss(),
    )
    .unwrap();

    let out = apply::restore_official(&state, &root, ss(), &[RestoreTool::Codex], &ok);
    assert_eq!(out.restored.len(), 1, "{out:?}");

    let text = std::fs::read_to_string(codex_config(home.path())).unwrap();
    assert!(text.contains("# 用户自己的注释"), "comment lost:\n{text}");
    let cfg = read_toml(&codex_config(home.path()));
    assert!(cfg.get("model_provider").is_none());
    assert!(cfg.get("model").is_none());
    assert!(cfg
        .get("model_providers")
        .and_then(|p| p.get("we2ai"))
        .is_none());
    assert_eq!(
        cfg["model_providers"]["mine"]["experimental_bearer_token"].as_str(),
        Some("sk-mine-secret")
    );
    assert_eq!(cfg["mcp_servers"]["docs"]["command"].as_str(), Some("npx"));
    assert_eq!(
        cfg["profiles"]["fast"]["model"].as_str(),
        Some("gpt-5-mini")
    );
    assert_eq!(
        std::fs::read_to_string(codex_auth(home.path())).unwrap(),
        CHATGPT_AUTH,
        "auth.json must be untouched"
    );
}

/// 恢复过程中会话变化（如登出流程里重新登录）：此后不再处理剩余工具。
#[test]
#[serial]
fn restore_official_stops_remaining_tools_when_the_session_changes_midway() {
    use std::cell::Cell;
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        ss(),
    )
    .unwrap();
    apply::apply_provider_tool(
        &state,
        ProviderTool::Codex,
        &params("gpt-5", KEY_A),
        &ok,
        ss(),
    )
    .unwrap();
    // 前两次检查仍允许：一次是 restore_official 每个工具开始前的检查，一次
    // 是 restore_claude 写入前的 TOCTOU 复查（Codex 复核高危项 1 新增，
    // Claude Code 完整走完需要两次 still_allowed() 都返回 true）；第三次
    // 检查（Codex 工具开始前）起已变化。
    let calls = Cell::new(0);
    let still_allowed = || {
        calls.set(calls.get() + 1);
        calls.get() <= 2
    };
    let out = apply::restore_official(
        &state,
        &root,
        ss(),
        &[RestoreTool::ClaudeCode, RestoreTool::Codex],
        &still_allowed,
    );
    assert_eq!(
        out.restored,
        vec![claude_settings(home.path()).display().to_string()]
    );
    let codex_text = std::fs::read_to_string(codex_config(home.path())).unwrap();
    assert!(
        codex_text.contains("model_provider = \"we2ai\""),
        "codex must be untouched: {codex_text}"
    );
    assert!(
        out.skipped.iter().any(|s| s.contains("登录状态已变化")),
        "{out:?}"
    );
}

// ---------------------------------------------------------------------------
// 恢复的 TOCTOU 防护（Codex 复核高危项 1）
// ---------------------------------------------------------------------------

/// 包一层 `InMemorySecretStore`，在 `get()` 里执行一次副作用（模拟"系统
/// 钥匙串授权弹窗停留期间，其他程序改写了 live 文件"），其余方法原样转发。
struct SecretStoreWithGetSideEffect<'a, F: Fn() + Send + Sync> {
    inner: &'a InMemorySecretStore,
    on_get: F,
}

impl<F: Fn() + Send + Sync> SecretStore for SecretStoreWithGetSideEffect<'_, F> {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, SecretStoreError> {
        (self.on_get)();
        self.inner.get(service, account)
    }
    fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), SecretStoreError> {
        self.inner.set(service, account, secret)
    }
    fn delete(&self, service: &str, account: &str) -> Result<(), SecretStoreError> {
        self.inner.delete(service, account)
    }
}

/// 同上，但包在 `set()` 上（用于验证 Claude apply 侧"钥匙串写入挪到快照
/// 之前"的保护，P6 三轮 Opus 复核高危项 1b）。
struct SecretStoreWithSetSideEffect<'a, F: Fn() + Send + Sync> {
    inner: &'a InMemorySecretStore,
    on_set: F,
}

impl<F: Fn() + Send + Sync> SecretStore for SecretStoreWithSetSideEffect<'_, F> {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, SecretStoreError> {
        self.inner.get(service, account)
    }
    fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), SecretStoreError> {
        (self.on_set)();
        self.inner.set(service, account, secret)
    }
    fn delete(&self, service: &str, account: &str) -> Result<(), SecretStoreError> {
        self.inner.delete(service, account)
    }
}

/// P6 三轮 Opus 复核高危项 1b：Claude apply 在捕获任何快照之前先读一次
/// live 的 `ANTHROPIC_API_KEY` 并存入钥匙串（可能阻塞在系统授权弹窗上）；
/// 弹窗停留期间发生的编辑，会被随后才进行的快照捕获自然拿到，不会被
/// "写回弹窗之前的旧快照"覆盖丢失。
#[test]
#[serial]
fn claude_apply_reads_the_keychain_before_capturing_the_snapshot_so_concurrent_edits_survive() {
    let home = TestHome::new();
    let state = state();
    let path = claude_settings(home.path());
    write(&path, CLAUDE_BASE);
    let store = InMemorySecretStore::new();
    let path_for_hook = path.clone();
    let wrapper = SecretStoreWithSetSideEffect {
        inner: &store,
        on_set: move || {
            let mut value = read_json(&path_for_hook);
            value["env"]["EDITED_WHILE_WAITING"] = json!("yes");
            write(&path_for_hook, &serde_json::to_string(&value).unwrap());
        },
    };
    let out = apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        &wrapper,
    )
    .unwrap();
    assert_eq!(out.model, "m");
    let after = read_json(&path);
    assert_eq!(
        after["env"]["EDITED_WHILE_WAITING"], "yes",
        "edit made while the blocking keychain set() was pending must survive: {after}"
    );
    assert_eq!(after["env"]["ANTHROPIC_BASE_URL"], GATEWAY);
    assert!(after["env"].get("ANTHROPIC_API_KEY").is_none(), "{after}");
    assert!(store.contains(secret_store::SERVICE_NAME, apply::CLAUDE_API_KEY_ACCOUNT));
}

/// 钥匙串保存失败：`preserve_and_read_users_claude_api_key` 在任何快照被
/// 捕获之前就直接返回错误，`apply_provider_tool` 里没有任何回滚可做——文件
/// 必须保持外部编辑（`set()` 的副作用）留下的样子，不能被"回滚到一个从未
/// 存在过的快照"（P6 三轮 Opus 复核高危项 1b）。
#[test]
#[serial]
fn claude_apply_does_not_revert_an_edit_made_while_a_failing_keychain_save_was_blocking() {
    let home = TestHome::new();
    let state = state();
    let path = claude_settings(home.path());
    write(&path, CLAUDE_BASE);
    let store = InMemorySecretStore::new();
    let path_for_hook = path.clone();
    let wrapper = SecretStoreWithSetSideEffect {
        inner: &store,
        on_set: move || {
            let mut value = read_json(&path_for_hook);
            value["env"]["EDITED_WHILE_WAITING"] = json!("yes");
            write(&path_for_hook, &serde_json::to_string(&value).unwrap());
        },
    };
    store.set_fail_set(true);
    let err = apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        &wrapper,
    )
    .unwrap_err();
    assert_eq!(err.code, apply::ERR_FAILED, "{err}");
    let after = read_json(&path);
    assert_eq!(
        after["env"]["EDITED_WHILE_WAITING"], "yes",
        "must stay exactly as the concurrent edit left it: {after}"
    );
    assert_eq!(after["env"]["ANTHROPIC_API_KEY"], "sk-user-own");
    assert_eq!(
        db_view(&state, AppType::Claude, apply::CLAUDE_PROVIDER_ID),
        (None, None, false, None)
    );
}

/// 钥匙串保存成功，但同一次 `set()` 副作用又把 live 的 Key 改成了另一个值
/// （模拟：用户在授权弹窗其间又编辑了一次）：`merged_claude_settings` 随后
/// 重新读到的值与已保存的值不一致，直接中止、不写入任何内容（P6 三轮
/// Opus 复核高危项 1b）。
#[test]
#[serial]
fn claude_apply_aborts_without_writing_when_the_api_key_changes_again_while_keychain_set_was_blocking(
) {
    let home = TestHome::new();
    let state = state();
    let path = claude_settings(home.path());
    write(&path, CLAUDE_BASE);
    let store = InMemorySecretStore::new();
    let path_for_hook = path.clone();
    let wrapper = SecretStoreWithSetSideEffect {
        inner: &store,
        on_set: move || {
            let mut value = read_json(&path_for_hook);
            value["env"]["ANTHROPIC_API_KEY"] = json!("sk-changed-again");
            write(&path_for_hook, &serde_json::to_string(&value).unwrap());
        },
    };
    let err = apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        &wrapper,
    )
    .unwrap_err();
    assert!(err.message.contains("授权期间被修改"), "{err}");
    let after = read_json(&path);
    assert_eq!(
        after["env"]["ANTHROPIC_API_KEY"], "sk-changed-again",
        "must not write; file stays as the second edit left it: {after}"
    );
    assert_eq!(
        db_view(&state, AppType::Claude, apply::CLAUDE_PROVIDER_ID),
        (None, None, false, None)
    );
}

/// `restore_claude` 与 apply 不同，是"先读 live 文件、只在确实需要时才碰
/// 钥匙串"（P6 三轮 Opus 复核中危项 2：不能让登出批量恢复对着从未指定过
/// 的工具也触发一次钥匙串访问）。因此钥匙串等待期间发生的外部编辑，由
/// 写入前的字节比对复查拦下——不写、不删钥匙串条目、报告为未恢复，而不
/// 是像 apply 那样自然拿到最新内容。
#[test]
#[serial]
fn restore_claude_aborts_without_writing_when_live_is_edited_during_its_own_keychain_read() {
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    let path = claude_settings(home.path());
    write(&path, CLAUDE_BASE);
    let store = InMemorySecretStore::new();
    apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        &store,
    )
    .unwrap();
    assert!(store.contains(secret_store::SERVICE_NAME, apply::CLAUDE_API_KEY_ACCOUNT));

    let path_for_hook = path.clone();
    let written: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    let written_ref = &written;
    let wrapper = SecretStoreWithGetSideEffect {
        inner: &store,
        on_get: move || {
            let mut value = read_json(&path_for_hook);
            value["env"]["EDITED_WHILE_WAITING"] = json!("yes");
            let text = serde_json::to_string(&value).unwrap();
            write(&path_for_hook, &text);
            *written_ref.lock().unwrap() = Some(text);
        },
    };
    let out = apply::restore_official(&state, &root, &wrapper, &[RestoreTool::ClaudeCode], &ok);
    let expected = written.lock().unwrap().clone().expect("hook must have run");
    assert!(out.restored.is_empty(), "{out:?}");
    assert!(
        out.skipped.iter().any(|s| s.contains("检测到其他程序修改")),
        "{out:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        expected,
        "must not overwrite the concurrent edit"
    );
    assert!(
        store.contains(secret_store::SERVICE_NAME, apply::CLAUDE_API_KEY_ACCOUNT),
        "keychain entry must remain since nothing was written"
    );
}

/// 写入前的复查发现"调用方已不再允许继续"（如登出恢复流程里用户又重新
/// 登录）：不写，钥匙串条目原样保留，报告为未恢复而不是静默丢弃差异。
#[test]
#[serial]
fn restore_claude_does_not_write_when_session_changes_during_keychain_read() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    let path = claude_settings(home.path());
    write(&path, CLAUDE_BASE);
    let store = InMemorySecretStore::new();
    apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        &store,
    )
    .unwrap();
    let before = std::fs::read_to_string(&path).unwrap();

    let allowed = AtomicBool::new(true);
    let wrapper = SecretStoreWithGetSideEffect {
        inner: &store,
        on_get: || allowed.store(false, Ordering::SeqCst),
    };
    let still_allowed = || allowed.load(Ordering::SeqCst);
    let out = apply::restore_official(
        &state,
        &root,
        &wrapper,
        &[RestoreTool::ClaudeCode],
        &still_allowed,
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        before,
        "must not write after the session changed mid-flight"
    );
    assert!(
        store.contains(secret_store::SERVICE_NAME, apply::CLAUDE_API_KEY_ACCOUNT),
        "keychain entry must remain since nothing was written"
    );
    assert!(
        out.skipped.iter().any(|s| s.contains("登录状态已变化")),
        "{out:?}"
    );
}

/// 写入前的复查发现 CC Switch 刚开始代理接管（钥匙串授权弹窗停留期间发生，
/// 表现为 live 文件字节变化）：不写，钥匙串条目原样保留。这里命中的是三项
/// 复查里最先做的字节比对（字节已经不同，不需要再看接管判定），报告的是
/// 通用的"检测到其他程序修改"，不是接管专用文案——`restore_codex` 侧的
/// 独立接管判定同样存在，只是在字节比对已经能覆盖的场景里不会被单独触发
/// 到，见 `restore_official_does_not_touch_claude_live_under_cc_switch_takeover`
/// 覆盖"live 从一开始就是接管状态"（不涉及字节变化）的情形。
#[test]
#[serial]
fn restore_claude_does_not_write_when_cc_switch_takeover_starts_during_keychain_read() {
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    let path = claude_settings(home.path());
    write(&path, CLAUDE_BASE);
    let store = InMemorySecretStore::new();
    apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        &store,
    )
    .unwrap();

    let path_for_hook = path.clone();
    let written: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    let written_ref = &written;
    let wrapper = SecretStoreWithGetSideEffect {
        inner: &store,
        on_get: move || {
            let mut value = read_json(&path_for_hook);
            value["env"]["ANTHROPIC_AUTH_TOKEN"] = json!("PROXY_MANAGED");
            let text = serde_json::to_string(&value).unwrap();
            write(&path_for_hook, &text);
            *written_ref.lock().unwrap() = Some(text);
        },
    };
    let out = apply::restore_official(&state, &root, &wrapper, &[RestoreTool::ClaudeCode], &ok);
    let expected = written.lock().unwrap().clone().expect("hook must have run");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        expected,
        "must not write after CC Switch takeover starts mid-flight"
    );
    assert!(
        store.contains(secret_store::SERVICE_NAME, apply::CLAUDE_API_KEY_ACCOUNT),
        "keychain entry must remain since nothing was written"
    );
    assert!(
        out.skipped.iter().any(|s| s.contains("检测到其他程序修改")),
        "{out:?}"
    );
}

/// Codex 侧同一套写入前复查：`still_allowed()` 在写入前再次变为 false 时
/// 不写。
#[test]
#[serial]
fn restore_codex_does_not_write_when_session_changes_before_write() {
    use std::cell::Cell;
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    apply::apply_provider_tool(
        &state,
        ProviderTool::Codex,
        &params("gpt-5", KEY_A),
        &ok,
        ss(),
    )
    .unwrap();
    let before = std::fs::read_to_string(codex_config(home.path())).unwrap();

    // 第一次检查（restore_official 每个工具开始前）仍允许，第二次
    // （restore_codex 写入前的复查）起已变化。
    let calls = Cell::new(0);
    let still_allowed = || {
        calls.set(calls.get() + 1);
        calls.get() == 1
    };
    let out = apply::restore_official(&state, &root, ss(), &[RestoreTool::Codex], &still_allowed);
    assert_eq!(
        std::fs::read_to_string(codex_config(home.path())).unwrap(),
        before,
        "must not write after the session changed mid-flight"
    );
    assert!(
        out.skipped.iter().any(|s| s.contains("登录状态已变化")),
        "{out:?}"
    );
}

/// 包一层 `InMemorySecretStore`，统计 `get()` 被调用的次数——用于验证
/// `restore_claude` 只在确实需要时才碰钥匙串（P6 三轮 Opus 复核中危项 2）。
struct CountingSecretStore<'a> {
    inner: &'a InMemorySecretStore,
    get_calls: std::sync::atomic::AtomicU32,
}

impl<'a> CountingSecretStore<'a> {
    fn new(inner: &'a InMemorySecretStore) -> Self {
        Self {
            inner,
            get_calls: std::sync::atomic::AtomicU32::new(0),
        }
    }

    fn get_call_count(&self) -> u32 {
        self.get_calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl SecretStore for CountingSecretStore<'_> {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, SecretStoreError> {
        self.get_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.get(service, account)
    }
    fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), SecretStoreError> {
        self.inner.set(service, account, secret)
    }
    fn delete(&self, service: &str, account: &str) -> Result<(), SecretStoreError> {
        self.inner.delete(service, account)
    }
}

/// `settings.json` 不存在：登出批量恢复对着从未指定过 Claude Code 的机器
/// 不该触发任何钥匙串访问（P6 三轮 Opus 复核中危项 2）。
#[test]
#[serial]
fn restore_claude_does_not_touch_the_keychain_when_settings_file_is_missing() {
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    let inner = InMemorySecretStore::new();
    let counting = CountingSecretStore::new(&inner);
    let out = apply::restore_official(&state, &root, &counting, &[RestoreTool::ClaudeCode], &ok);
    assert!(out.restored.is_empty() && out.skipped.is_empty(), "{out:?}");
    assert_eq!(counting.get_call_count(), 0);
}

/// `settings.json` 存在但不指向 WE2AI、且没有 `ANTHROPIC_API_KEY`：同样不
/// 需要碰钥匙串（没有陈旧残留可能需要清理）。
#[test]
#[serial]
fn restore_claude_does_not_touch_the_keychain_when_pointing_elsewhere_without_an_api_key() {
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    write(
        &claude_settings(home.path()),
        r#"{"env":{"ANTHROPIC_BASE_URL":"https://other.example"}}"#,
    );
    let inner = InMemorySecretStore::new();
    let counting = CountingSecretStore::new(&inner);
    let out = apply::restore_official(&state, &root, &counting, &[RestoreTool::ClaudeCode], &ok);
    assert!(out.restored.is_empty(), "{out:?}");
    assert_eq!(counting.get_call_count(), 0);
}

/// `settings.json` 不指向 WE2AI，但存在非空的 `ANTHROPIC_API_KEY`：需要查
/// 一次钥匙串，判断是不是能顺手清理的陈旧残留（P6 三轮 Opus 复核中危项 2）。
#[test]
#[serial]
fn restore_claude_touches_the_keychain_when_pointing_elsewhere_with_an_api_key() {
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    write(
        &claude_settings(home.path()),
        r#"{"env":{"ANTHROPIC_BASE_URL":"https://other.example","ANTHROPIC_API_KEY":"sk-other"}}"#,
    );
    let inner = InMemorySecretStore::new();
    let counting = CountingSecretStore::new(&inner);
    let out = apply::restore_official(&state, &root, &counting, &[RestoreTool::ClaudeCode], &ok);
    assert!(out.restored.is_empty(), "{out:?}");
    assert!(counting.get_call_count() >= 1, "{out:?}");
}

/// 指向 WE2AI、真正要恢复：一定会查一次钥匙串。
#[test]
#[serial]
fn restore_claude_touches_the_keychain_when_actually_restoring() {
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    let path = claude_settings(home.path());
    write(&path, CLAUDE_BASE);
    let inner = InMemorySecretStore::new();
    apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        &inner,
    )
    .unwrap();
    let counting = CountingSecretStore::new(&inner);
    let out = apply::restore_official(&state, &root, &counting, &[RestoreTool::ClaudeCode], &ok);
    assert_eq!(out.restored.len(), 1, "{out:?}");
    assert!(counting.get_call_count() >= 1, "{out:?}");
}

// ---------------------------------------------------------------------------
// Codex
// ---------------------------------------------------------------------------

const CODEX_BASE: &str = r#"# 用户自己的注释
model_provider = "mine"
model = "gpt-5"
approval_policy = "on-request"

[model_providers.mine]
name = "Mine"
base_url = "https://example.com/v1"
wire_api = "responses"
experimental_bearer_token = "sk-mine-secret"

[mcp_servers.docs]
command = "npx"
args = ["-y", "docs-mcp"]

[profiles.fast]
model = "gpt-5-mini"
"#;

const CHATGPT_AUTH: &str = r#"{"OPENAI_API_KEY":null,"tokens":{"id_token":"x.y.z","access_token":"at","refresh_token":"rt","account_id":"acc"},"last_refresh":"2026-09-01T00:00:00Z"}"#;

#[test]
#[serial]
fn codex_apply_twice_writes_managed_table_and_keeps_everything_else() {
    let home = TestHome::new();
    let state = state();
    write(&codex_config(home.path()), CODEX_BASE);
    write(&codex_auth(home.path()), CHATGPT_AUTH);

    apply::apply_provider_tool(
        &state,
        ProviderTool::Codex,
        &params("gpt-5-codex", KEY_A),
        &ok,
        ss(),
    )
    .unwrap();
    let out = apply::apply_provider_tool(
        &state,
        ProviderTool::Codex,
        &params("gpt-5", KEY_B),
        &ok,
        ss(),
    )
    .unwrap();
    assert_eq!(out.model, "gpt-5");

    let text = std::fs::read_to_string(codex_config(home.path())).unwrap();
    assert!(text.contains("# 用户自己的注释"), "comment lost:\n{text}");
    let cfg = read_toml(&codex_config(home.path()));
    assert_eq!(cfg["model_provider"].as_str(), Some("we2ai"));
    assert_eq!(cfg["model"].as_str(), Some("gpt-5"));
    assert_eq!(cfg["approval_policy"].as_str(), Some("on-request"));
    let we2ai = &cfg["model_providers"]["we2ai"];
    assert_eq!(we2ai["base_url"].as_str(), Some("https://api.we2ai.com/v1"));
    assert_eq!(we2ai["wire_api"].as_str(), Some("responses"));
    assert_eq!(we2ai["experimental_bearer_token"].as_str(), Some(KEY_B));
    assert_eq!(we2ai["requires_openai_auth"].as_bool(), Some(false));
    let mine = &cfg["model_providers"]["mine"];
    assert_eq!(mine["base_url"].as_str(), Some("https://example.com/v1"));
    assert_eq!(
        mine["experimental_bearer_token"].as_str(),
        Some("sk-mine-secret")
    );
    assert_eq!(cfg["mcp_servers"]["docs"]["command"].as_str(), Some("npx"));
    assert_eq!(
        cfg["profiles"]["fast"]["model"].as_str(),
        Some("gpt-5-mini")
    );

    #[cfg(unix)]
    {
        assert_eq!(mode(&codex_config(home.path())), 0o600);
        assert_eq!(mode(&codex_auth(home.path())), 0o600);
        assert_eq!(mode(&home.path().join(".codex")), 0o700);
    }

    // ChatGPT 登录保留，Key 不写进 auth.json。
    assert_eq!(
        std::fs::read_to_string(codex_auth(home.path())).unwrap(),
        CHATGPT_AUTH
    );

    let (row, current, _, local) = db_view(&state, AppType::Codex, apply::CODEX_PROVIDER_ID);
    let row = row.unwrap().to_string();
    assert!(
        !row.contains("sk-mine-secret"),
        "foreign token must not stay in db row"
    );
    assert!(!row.contains("mcp_servers"));
    assert_eq!(current.as_deref(), Some(apply::CODEX_PROVIDER_ID));
    assert_eq!(local.as_deref(), Some(apply::CODEX_PROVIDER_ID));
    assert!(crate::settings::preserve_codex_official_auth_on_switch());
}

#[test]
#[serial]
fn codex_requires_openai_auth_is_false_for_every_login_state() {
    let cases: [(&str, Option<&str>, &str); 5] = [
        ("no auth.json", None, ""),
        (
            "api key only",
            Some(r#"{"OPENAI_API_KEY":"sk-openai-own"}"#),
            "",
        ),
        (
            "metadata only",
            Some(r#"{"last_refresh":"2026-09-01T00:00:00Z"}"#),
            "",
        ),
        (
            "keyring store",
            None,
            "cli_auth_credentials_store = \"keyring\"\n",
        ),
        (
            "ephemeral store",
            None,
            "cli_auth_credentials_store = \"ephemeral\"\n",
        ),
    ];
    for (name, auth, prefix) in cases {
        let home = TestHome::new();
        let state = state();
        write(&codex_config(home.path()), prefix);
        if let Some(auth) = auth {
            write(&codex_auth(home.path()), auth);
        }
        apply::apply_provider_tool(
            &state,
            ProviderTool::Codex,
            &params("gpt-5", KEY_A),
            &ok,
            ss(),
        )
        .unwrap_or_else(|e| panic!("{name}: {e}"));
        let cfg = read_toml(&codex_config(home.path()));
        assert_eq!(
            cfg["model_providers"]["we2ai"]["requires_openai_auth"].as_bool(),
            Some(false),
            "{name}"
        );
        if let Some(auth) = auth {
            assert_eq!(
                std::fs::read_to_string(codex_auth(home.path())).unwrap(),
                auth,
                "{name}"
            );
        }
    }
}

#[test]
#[serial]
fn codex_failures_at_each_stage_restore_the_whole_snapshot() {
    for stage in [Stage::AfterUpsert, Stage::Switch, Stage::AfterLiveWrite] {
        let home = TestHome::new();
        let state = state();
        write(&codex_config(home.path()), CODEX_BASE);
        write(&codex_auth(home.path()), CHATGPT_AUTH);
        let cfg_path = codex_config(home.path());
        apply::set_test_hook(move |s| {
            if s != stage {
                return Ok(());
            }
            if s == Stage::Switch {
                std::fs::write(&cfg_path, "model = \"half-written\"\n").unwrap();
            }
            Err(format!("injected at {s:?}"))
        });
        let err = apply::apply_provider_tool(
            &state,
            ProviderTool::Codex,
            &params("gpt-5", KEY_A),
            &ok,
            ss(),
        )
        .unwrap_err();
        assert_eq!(err.code, apply::ERR_FAILED, "{stage:?}: {err}");
        assert_eq!(
            std::fs::read_to_string(codex_config(home.path())).unwrap(),
            CODEX_BASE,
            "{stage:?}"
        );
        assert_eq!(
            std::fs::read_to_string(codex_auth(home.path())).unwrap(),
            CHATGPT_AUTH,
            "{stage:?}"
        );
        assert!(
            !home
                .path()
                .join(".codex/cc-switch-model-catalog.json")
                .exists(),
            "{stage:?}"
        );
        assert_eq!(
            db_view(&state, AppType::Codex, apply::CODEX_PROVIDER_ID),
            (None, None, false, None),
            "{stage:?}"
        );
    }
}

/// P6 四轮 Opus 复核高危项 1：`mark_write_attempted()` 会给这次 apply 涉及
/// 的全部文件统一打标记，但"已标记"不代表这个文件真的被写过——`switch`
/// 可能在触碰任何 live 文件之前就失败（如保存本地 current 失败）。标记
/// 之前（`Stage::BeforeSwitch` 钩子里）发生的外部编辑必须原样保留，不能
/// 被"没有 H1 就当部分写入回滚"的旧逻辑覆盖掉。
#[test]
#[serial]
fn claude_apply_keeps_an_external_edit_made_before_the_write_attempt_mark_when_switch_fails_immediately(
) {
    let home = TestHome::new();
    let state = state();
    let path = claude_settings(home.path());
    write(&path, CLAUDE_BASE);
    let path_for_hook = path.clone();
    apply::set_test_hook(move |stage| {
        if stage == Stage::BeforeSwitch {
            // 这次编辑发生在 mark_write_attempted() 之前（BeforeSwitch 钩子
            // 成功返回后才会标记）。
            let mut value = read_json(&path_for_hook);
            value["env"]["EDITED_BEFORE_MARK"] = json!("yes");
            write(&path_for_hook, &serde_json::to_string(&value).unwrap());
        }
        if stage == Stage::Switch {
            return Err("boom".into());
        }
        Ok(())
    });
    let err = apply::apply_provider_tool(
        &state,
        ProviderTool::ClaudeCode,
        &params("m", KEY_A),
        &ok,
        ss(),
    )
    .unwrap_err();
    // rollback() 把 `RestoreResult::ExternalModified` 当成"外部修改，未回滚"
    // 上报，错误码升级为 ERR_EXTERNAL（与 `external_write_before_rollback_is_not_overwritten`
    // 是同一条既有路径）——这正是本用例要验证的：文件没有被覆盖回旧快照。
    assert_eq!(err.code, apply::ERR_EXTERNAL, "{err}");
    let after = read_json(&path);
    assert_eq!(
        after["env"]["EDITED_BEFORE_MARK"], "yes",
        "an edit made before mark_write_attempted() must survive a failure that never actually wrote this file: {after}"
    );
}

const CHATGPT_AUTH_REFRESHED: &str = r#"{"OPENAI_API_KEY":null,"tokens":{"id_token":"x.y.z","access_token":"at2","refresh_token":"rt2","account_id":"acc"},"last_refresh":"2026-09-25T12:00:00Z"}"#;

/// P6 四轮 Opus 复核高危项 1：Codex 保留 ChatGPT 登录时 `auth.json` 从不被
/// `switch` 写入，但仍然和其他文件一起被 `mark_write_attempted()` 标记。
/// 标记前 `auth.json` 因为 Codex 自己刷新令牌而改变内容，随后 `switch`
/// 正常执行（真的写了 `config.toml`），但更后面的阶段失败：`auth.json`
/// 必须保留刷新后的内容，`config.toml` 正常回滚到 apply 之前的样子。
#[test]
#[serial]
fn codex_apply_keeps_a_refreshed_auth_json_while_rolling_back_config_toml_on_a_later_stage_failure()
{
    let home = TestHome::new();
    let state = state();
    write(&codex_config(home.path()), CODEX_BASE);
    write(&codex_auth(home.path()), CHATGPT_AUTH);
    let auth_path_for_hook = codex_auth(home.path());
    apply::set_test_hook(move |stage| {
        if stage == Stage::BeforeSwitch {
            // 模拟 Codex 自己在这期间刷新了 ChatGPT 令牌；这次改动发生在
            // mark_write_attempted() 之前。
            write(&auth_path_for_hook, CHATGPT_AUTH_REFRESHED);
        }
        if stage == Stage::AfterLiveWrite {
            // 这一步在 switch 真正写完 config.toml、且已经对全部文件调用过
            // record_h1() 之后才失败。
            return Err("boom".into());
        }
        Ok(())
    });
    let err = apply::apply_provider_tool(
        &state,
        ProviderTool::Codex,
        &params("gpt-5", KEY_A),
        &ok,
        ss(),
    )
    .unwrap_err();
    // auth.json 报 `ExternalModified`（rollback() 升级为 ERR_EXTERNAL），
    // config.toml 仍然按正常快照恢复——两者是同一次 rollback() 里各自独立
    // 判定的结果，参见 `rollback()` 对 `files` 逐项 `.restore()` 的实现。
    assert_eq!(err.code, apply::ERR_EXTERNAL, "{err}");
    assert_eq!(
        std::fs::read_to_string(codex_auth(home.path())).unwrap(),
        CHATGPT_AUTH_REFRESHED,
        "auth.json was never written by switch; the refresh made before the write-attempt mark must survive"
    );
    assert_eq!(
        std::fs::read_to_string(codex_config(home.path())).unwrap(),
        CODEX_BASE,
        "config.toml was actually written by switch and must be rolled back"
    );
}

// ---------------------------------------------------------------------------
// WorkBuddy
// ---------------------------------------------------------------------------

fn wb_models(home: &Path) -> PathBuf {
    home.join(".workbuddy/models.json")
}

fn wb_items(home: &Path) -> Vec<Value> {
    read_json(&wb_models(home)).as_array().unwrap().clone()
}

fn data_root(home: &Path) -> PathBuf {
    home.join(".we2ai")
}

const USER_ENTRY: &str = r#"{"id":"deepseek-v3","name":"My DeepSeek","vendor":"Custom","url":"https://ds.example/v1","apiKey":"sk-user","useCustomProtocol":false}"#;

#[test]
#[serial]
fn workbuddy_writes_entry_and_replaces_in_place_on_key_change() {
    let home = TestHome::new();
    std::fs::create_dir_all(data_root(home.path())).unwrap();
    write(&wb_models(home.path()), &format!("[{USER_ENTRY}]"));

    workbuddy::apply_workbuddy(
        &data_root(home.path()),
        &params("glm-4.6", KEY_A),
        false,
        &ok,
    )
    .unwrap();
    workbuddy::apply_workbuddy(
        &data_root(home.path()),
        &params("glm-4.6", KEY_B),
        false,
        &ok,
    )
    .unwrap();

    let items = wb_items(home.path());
    assert_eq!(items.len(), 2);
    assert_eq!(items[0], serde_json::from_str::<Value>(USER_ENTRY).unwrap());
    let ours = &items[1];
    assert_eq!(ours["id"], "glm-4.6");
    assert_eq!(ours["name"], "WE2AI glm-4.6");
    assert_eq!(ours["vendor"], "Custom");
    assert_eq!(ours["url"], "https://api.we2ai.com/v1");
    assert_eq!(ours["apiKey"], KEY_B);
    assert_eq!(ours["useCustomProtocol"], false);
    assert_eq!(
        workbuddy::managed_model(&data_root(home.path())).as_deref(),
        Some("glm-4.6")
    );
    let record =
        std::fs::read_to_string(data_root(home.path()).join("workbuddy_managed.json")).unwrap();
    assert!(!record.contains(KEY_B), "record must not contain the key");
}

#[test]
#[serial]
fn workbuddy_switching_model_removes_unchanged_old_entry_and_keeps_edited_one() {
    let home = TestHome::new();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    workbuddy::apply_workbuddy(&root, &params("m1", KEY_A), false, &ok).unwrap();
    workbuddy::apply_workbuddy(&root, &params("m2", KEY_A), false, &ok).unwrap();
    let ids: Vec<_> = wb_items(home.path())
        .iter()
        .map(|v| v["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, vec!["m2"]);

    // 用户手改 m2 后再切到 m3：m2 保留并提示。
    let mut items = wb_items(home.path());
    items[0]["name"] = json!("我改过的名字");
    write(
        &wb_models(home.path()),
        &serde_json::to_string(&items).unwrap(),
    );
    let out = workbuddy::apply_workbuddy(&root, &params("m3", KEY_A), false, &ok).unwrap();
    assert_eq!(out.warnings.len(), 1, "{:?}", out.warnings);
    let ids: Vec<_> = wb_items(home.path())
        .iter()
        .map(|v| v["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, vec!["m2", "m3"]);
}

#[test]
#[serial]
fn workbuddy_same_id_edited_or_unmanaged_requires_confirmation() {
    let home = TestHome::new();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();

    // 与非托管条目重名。
    write(&wb_models(home.path()), &format!("[{USER_ENTRY}]"));
    let before = std::fs::read_to_string(wb_models(home.path())).unwrap();
    let err =
        workbuddy::apply_workbuddy(&root, &params("deepseek-v3", KEY_A), false, &ok).unwrap_err();
    assert_eq!(err.code, workbuddy::ERR_CONFIRM);
    assert_eq!(
        std::fs::read_to_string(wb_models(home.path())).unwrap(),
        before,
        "cancel keeps file"
    );
    workbuddy::apply_workbuddy(&root, &params("deepseek-v3", KEY_A), true, &ok).unwrap();
    let items = wb_items(home.path());
    assert_eq!(items.len(), 1, "no duplicate id");
    assert_eq!(items[0]["apiKey"], KEY_A);

    // 托管条目被手改后再次指定同一模型。
    let mut items = wb_items(home.path());
    items[0]["url"] = json!("https://edited.example/v1");
    write(
        &wb_models(home.path()),
        &serde_json::to_string(&items).unwrap(),
    );
    let err =
        workbuddy::apply_workbuddy(&root, &params("deepseek-v3", KEY_B), false, &ok).unwrap_err();
    assert_eq!(err.code, workbuddy::ERR_CONFIRM);
}

#[cfg(unix)]
#[test]
#[serial]
fn workbuddy_file_becomes_private_whether_new_or_previously_0644() {
    use std::os::unix::fs::PermissionsExt;
    let home = TestHome::new();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    workbuddy::apply_workbuddy(&root, &params("m1", KEY_A), false, &ok).unwrap();
    assert_eq!(mode(&wb_models(home.path())), 0o600);
    assert_eq!(mode(&home.path().join(".workbuddy")), 0o700);

    std::fs::set_permissions(
        wb_models(home.path()),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    workbuddy::apply_workbuddy(&root, &params("m2", KEY_A), false, &ok).unwrap();
    assert_eq!(mode(&wb_models(home.path())), 0o600);
}

#[test]
#[serial]
fn workbuddy_concurrent_edit_between_read_and_write_is_not_lost() {
    let home = TestHome::new();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    write(&wb_models(home.path()), &format!("[{USER_ENTRY}]"));
    let path = wb_models(home.path());
    let mut fired = false;
    workbuddy::set_before_final_compare(move || {
        if !fired {
            fired = true;
            let mut items: Vec<Value> =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            items[0]["name"] = json!("changed by WorkBuddy");
            std::fs::write(&path, serde_json::to_string(&items).unwrap()).unwrap();
        }
    });
    workbuddy::apply_workbuddy(&root, &params("m1", KEY_A), false, &ok).unwrap();
    let items = wb_items(home.path());
    assert_eq!(items[0]["name"], "changed by WorkBuddy");
    assert_eq!(items[1]["id"], "m1");
}

#[test]
#[serial]
fn workbuddy_record_write_failure_restores_models_json() {
    let home = TestHome::new();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    write(&wb_models(home.path()), &format!("[{USER_ENTRY}]"));
    let before = std::fs::read_to_string(wb_models(home.path())).unwrap();
    workbuddy::set_fail_record_write(true);
    workbuddy::apply_workbuddy(&root, &params("m1", KEY_A), false, &ok).unwrap_err();
    assert_eq!(
        std::fs::read_to_string(wb_models(home.path())).unwrap(),
        before
    );
    assert!(workbuddy::load_record(&root).is_none());
}

#[test]
#[serial]
fn workbuddy_writes_capability_fields_only_when_b1_returns_them() {
    let home = TestHome::new();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    let mut p = params("m1", KEY_A);
    p.capabilities = Some(super::api::ModelCapabilities {
        supports_tool_call: Some(true),
        supports_images: Some(false),
        reasoning_efforts: Some(vec!["low".into(), "high".into()]),
    });
    workbuddy::apply_workbuddy(&root, &p, false, &ok).unwrap();
    let entry = &wb_items(home.path())[0];
    assert_eq!(entry["supportsToolCall"], true);
    assert_eq!(entry["supportsImages"], false);
    assert_eq!(entry["supportsReasoning"], true);
    assert_eq!(
        entry["reasoning"]["supportedEfforts"],
        json!(["low", "high"])
    );

    workbuddy::apply_workbuddy(&root, &params("m2", KEY_A), false, &ok).unwrap();
    let entry = &wb_items(home.path())[0];
    for k in [
        "supportsToolCall",
        "supportsImages",
        "supportsReasoning",
        "reasoning",
    ] {
        assert!(
            entry.get(k).is_none(),
            "{k} must be omitted without B1 data"
        );
    }
}

/// 组合故障：重算期间被其他程序修改、随后托管记录写入失败。回滚只能恢复到
/// 本次写入前的内容，保留其他程序的修改。
#[test]
#[serial]
fn workbuddy_record_failure_after_a_concurrent_edit_keeps_the_other_edit() {
    let home = TestHome::new();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    write(&wb_models(home.path()), &format!("[{USER_ENTRY}]"));
    let path = wb_models(home.path());
    let mut fired = false;
    workbuddy::set_before_final_compare(move || {
        if !fired {
            fired = true;
            let mut items: Vec<Value> =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            items[0]["name"] = json!("changed by WorkBuddy");
            std::fs::write(&path, serde_json::to_string(&items).unwrap()).unwrap();
        }
    });
    workbuddy::set_fail_record_write(true);
    workbuddy::apply_workbuddy(&root, &params("m1", KEY_A), false, &ok).unwrap_err();
    let items = wb_items(home.path());
    assert_eq!(items.len(), 1, "our entry rolled back");
    assert_eq!(items[0]["name"], "changed by WorkBuddy");
}

/// P6 四轮 Opus 复核高危项 2：models.json 已经写盘之后（H1 已经从内存字节
/// 记下，不是重新读盘得到的）、写托管记录之前，被外部程序改写；随后托管
/// 记录写入失败触发回滚。旧实现在这个失败分支里才重新读盘记 H1，会把外部
/// 编辑误认成"就是我们写的内容"并覆盖掉；修复后 H1 从一开始就固定成我们
/// 真正写下的字节，回滚必须识别出这是外部改动、不覆盖。
#[test]
#[serial]
fn workbuddy_record_failure_after_an_edit_made_right_after_our_write_keeps_the_external_content() {
    let home = TestHome::new();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    write(&wb_models(home.path()), &format!("[{USER_ENTRY}]"));
    let path = wb_models(home.path());
    workbuddy::set_after_write_before_record(move || {
        // 这一刻 models.json 已经是我们写入的内容（含新增的 WE2AI 条目），
        // 外部程序在这里抢先改写，发生在 H1 被记录**之后**。
        std::fs::write(&path, "[{\"written_by\":\"someone_else\"}]").unwrap();
    });
    workbuddy::set_fail_record_write(true);
    let err = workbuddy::apply_workbuddy(&root, &params("m1", KEY_A), false, &ok).unwrap_err();
    assert!(err.message.contains("已被其他程序修改，未回滚"), "{err}");
    let text = std::fs::read_to_string(wb_models(home.path())).unwrap();
    assert_eq!(
        text, "[{\"written_by\":\"someone_else\"}]",
        "external edit made right after our write must survive, not be treated as ours: {text}"
    );
}

#[test]
#[serial]
fn workbuddy_honors_config_dir_env_override() {
    let home = TestHome::new();
    let custom = home.path().join("custom-wb");
    std::env::set_var("WORKBUDDY_CONFIG_DIR", &custom);
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    workbuddy::apply_workbuddy(&root, &params("m1", KEY_A), false, &ok).unwrap();
    assert!(custom.join("models.json").is_file());
    assert!(!wb_models(home.path()).exists());
}
