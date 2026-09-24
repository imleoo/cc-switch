//! P4 工具写入测试（方案第 4.1–4.3 节、第 8 节 P4 行）。
//!
//! 每个用例在独立临时 home 下运行（`CC_SWITCH_TEST_HOME` + `HOME`），数据库用
//! 内存库；这些环境变量是进程级的，全部用 `#[serial]` 串行，与上游同类测试
//! 共用 serial_test 的全局串行组。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use serial_test::serial;

use super::apply::{self, ApplyParams, ClaudeSlots, ProviderTool, Stage};
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
    )
    .unwrap();
    assert_eq!(out.model, "claude-sonnet-4-5");

    let mut second = params("claude-opus-4-1", KEY_B);
    second.claude_slots.haiku = Some("claude-haiku-4-5".into());
    apply::apply_provider_tool(&state, ProviderTool::ClaudeCode, &second, &ok).unwrap();

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
    apply::apply_provider_tool(&state, ProviderTool::ClaudeCode, &params("m", KEY_A), &ok).unwrap();
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
    apply::apply_provider_tool(&state, ProviderTool::ClaudeCode, &params("m", KEY_A), &ok).unwrap();
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
        let err =
            apply::apply_provider_tool(&state, ProviderTool::ClaudeCode, &params("m", KEY_A), &ok)
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
    apply::apply_provider_tool(&state, ProviderTool::ClaudeCode, &params("m1", KEY_A), &ok)
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
    apply::apply_provider_tool(&state, ProviderTool::ClaudeCode, &params("m2", KEY_B), &ok)
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
    let err =
        apply::apply_provider_tool(&state, ProviderTool::ClaudeCode, &params("m", KEY_A), &ok)
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
    apply::apply_provider_tool(&state, ProviderTool::ClaudeCode, &params("m", KEY_A), &ok)
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
    let err =
        apply::apply_provider_tool(&state, ProviderTool::ClaudeCode, &params("m", KEY_A), &ok)
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
    let err =
        apply::apply_provider_tool(&state, ProviderTool::ClaudeCode, &params("m", KEY_A), &ok)
            .unwrap_err();
    assert_eq!(err.code, apply::ERR_TAKEOVER);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), TAKEN_OVER);

    write(&path, CLAUDE_BASE);
    futures::executor::block_on(state.db.save_live_backup("claude", "{}")).unwrap();
    let err =
        apply::apply_provider_tool(&state, ProviderTool::ClaudeCode, &params("m", KEY_A), &ok)
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
    let err =
        apply::apply_provider_tool(&state, ProviderTool::ClaudeCode, &params("m", KEY_A), &ok)
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
            apply::apply_provider_tool(&state, ProviderTool::ClaudeCode, &params("m1", KEY_A), &ok)
        });
        let b = s.spawn(|| {
            apply::apply_provider_tool(&state, ProviderTool::ClaudeCode, &params("m2", KEY_B), &ok)
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
    apply::apply_provider_tool(&state, ProviderTool::ClaudeCode, &params("m", KEY_A), &ok).unwrap();
    apply::apply_provider_tool(&state, ProviderTool::Codex, &params("gpt-5", KEY_B), &ok).unwrap();
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
    apply::apply_provider_tool(&state, ProviderTool::ClaudeCode, &params("m2", KEY_A), &ok)
        .unwrap();
}

#[test]
#[serial]
fn logout_material_cleanup_empties_backups_and_drops_proxy_backups() {
    let home = TestHome::new();
    let state = state();
    apply::apply_provider_tool(&state, ProviderTool::ClaudeCode, &params("m", KEY_A), &ok).unwrap();
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
    let err =
        apply::apply_provider_tool(&state, ProviderTool::ClaudeCode, &params("m", KEY_A), &gone)
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
    apply::apply_provider_tool(&state, ProviderTool::ClaudeCode, &params("m", KEY_B), &ok).unwrap();
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
    apply::apply_provider_tool(&state, ProviderTool::ClaudeCode, &params("m", KEY_A), &ok).unwrap();
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

/// 登出弹窗勾选"同时从工具配置中移除 Key"：只移除 WE2AI 写入的部分。
#[test]
#[serial]
fn remove_tool_keys_strips_only_we2ai_credentials() {
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    write(&claude_settings(home.path()), CLAUDE_BASE);
    write(&codex_config(home.path()), CODEX_BASE);
    write(&wb_models(home.path()), &format!("[{USER_ENTRY}]"));
    apply::apply_provider_tool(&state, ProviderTool::ClaudeCode, &params("m", KEY_A), &ok).unwrap();
    apply::apply_provider_tool(&state, ProviderTool::Codex, &params("gpt-5", KEY_A), &ok).unwrap();
    workbuddy::apply_workbuddy(&root, &params("glm", KEY_A), false, &ok).unwrap();

    // 仍登录时拒绝。
    let logged_in = || false;
    assert_eq!(
        apply::remove_tool_keys(&root, &logged_in).unwrap_err().code,
        apply::ERR_SESSION_CHANGED
    );

    let out = apply::remove_tool_keys(&root, &ok).unwrap();
    assert_eq!(out.removed.len(), 3, "{out:?}");
    assert!(out.skipped.is_empty(), "{out:?}");

    let claude = read_json(&claude_settings(home.path()));
    assert!(claude["env"].get("ANTHROPIC_AUTH_TOKEN").is_none());
    assert_eq!(claude["env"]["CUSTOM_FLAG"], "1");
    assert_eq!(claude["hooks"], read_json_str(CLAUDE_BASE)["hooks"]);
    let codex = read_toml(&codex_config(home.path()));
    assert!(codex["model_providers"]["we2ai"]
        .get("experimental_bearer_token")
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
}

#[test]
#[serial]
fn remove_tool_keys_leaves_non_we2ai_claude_and_edited_workbuddy_entries() {
    let home = TestHome::new();
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

    let out = apply::remove_tool_keys(&root, &ok).unwrap();
    assert!(out.removed.is_empty(), "{out:?}");
    assert_eq!(out.skipped.len(), 1, "{out:?}");
    assert_eq!(
        std::fs::read_to_string(claude_settings(home.path())).unwrap(),
        other
    );
    assert_eq!(wb_items(home.path()).len(), 1);
}

/// 配置损坏或不可读时报告为未移除，不静默成功。
#[test]
#[serial]
fn remove_tool_keys_reports_unreadable_or_corrupt_configs() {
    let home = TestHome::new();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    write(&claude_settings(home.path()), "{ not json");
    write(&codex_config(home.path()), "[[[ not toml");
    let out = apply::remove_tool_keys(&root, &ok).unwrap();
    assert!(out.removed.is_empty(), "{out:?}");
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
        let out = apply::remove_tool_keys(&root, &ok).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(out.skipped.len(), 1, "{out:?}");
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("ANTHROPIC_AUTH_TOKEN"));
    }
}

/// 用户把 we2ai 表改成自有端点：token 不动，并如实报告没有可移除的内容。
#[test]
#[serial]
fn remove_tool_keys_skips_a_we2ai_codex_table_pointing_elsewhere() {
    let home = TestHome::new();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    let cfg = "model_provider = \"we2ai\"\n[model_providers.we2ai]\nbase_url = \"https://mine.example/v1\"\nexperimental_bearer_token = \"sk-mine\"\n";
    write(&codex_config(home.path()), cfg);
    let out = apply::remove_tool_keys(&root, &ok).unwrap();
    assert!(out.removed.is_empty(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(codex_config(home.path())).unwrap(),
        cfg
    );
}

/// 移除过程中重新登录：此后不再改文件，剩余项报告为未移除。
#[test]
#[serial]
fn remove_tool_keys_stops_when_the_user_logs_in_midway() {
    use std::cell::Cell;
    let home = TestHome::new();
    let state = state();
    let root = data_root(home.path());
    std::fs::create_dir_all(&root).unwrap();
    write(&codex_config(home.path()), "");
    apply::apply_provider_tool(&state, ProviderTool::ClaudeCode, &params("m", KEY_A), &ok).unwrap();
    apply::apply_provider_tool(&state, ProviderTool::Codex, &params("gpt-5", KEY_A), &ok).unwrap();
    // 第一次检查（取锁后）未登录，之后变为已登录。
    let calls = Cell::new(0);
    let logged_out = || {
        calls.set(calls.get() + 1);
        calls.get() == 1
    };
    let out = apply::remove_tool_keys(&root, &logged_out).unwrap();
    assert!(out.removed.is_empty(), "{out:?}");
    assert!(read_json(&claude_settings(home.path()))["env"]["ANTHROPIC_AUTH_TOKEN"] == KEY_A);
    assert!(
        out.skipped.iter().any(|s| s.contains("已重新登录")),
        "{out:?}"
    );
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
    )
    .unwrap();
    let out = apply::apply_provider_tool(&state, ProviderTool::Codex, &params("gpt-5", KEY_B), &ok)
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
        apply::apply_provider_tool(&state, ProviderTool::Codex, &params("gpt-5", KEY_A), &ok)
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
        let err =
            apply::apply_provider_tool(&state, ProviderTool::Codex, &params("gpt-5", KEY_A), &ok)
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
