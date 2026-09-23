//! 负例：即便数据库里已经有会触发迁移的存量数据（迁移标记 + 会被识别的第三方
//! Codex 供应商 / 工具 Skills 目录），WE2AI 模式下 SkillsSsotMigration 与
//! CodexHistoryMigration 两个 StartupTask 都必须保持禁用，lib.rs 的门控模式
//! （`if we2ai::mode::startup_allowed(task) { <迁移调用> }`）才会让迁移真的
//! 跑不到。这里直接复现该门控模式而不是驱动整个 Tauri App 的 `.setup()`
//! 闭包——后者依赖真实 AppHandle/窗口/托盘等一整套状态，不适合做单元级回归。

use cc_switch_lib::Provider;
use std::fs;

mod support;
use support::{create_test_state, ensure_test_home, reset_test_fs, test_mutex};

fn third_party_codex_provider() -> Provider {
    // 与 codex_history_migration.rs 自身测试用的 fixture 一致：一个非官方分类、
    // config 里带 `[model_providers.aihubmix]` 的供应商，能让
    // collect_source_model_provider_ids 返回非空集合，从而让
    // maybe_migrate_codex_third_party_history_provider_bucket 越过"空 id 直接
    // no-op"的早退分支，真正进入文件扫描逻辑。
    Provider::with_id(
        "rightcode".to_string(),
        "RightCode".to_string(),
        serde_json::json!({
            "auth": {},
            "config": "model_provider = \"aihubmix\"\n\n[model_providers.aihubmix]\nname = \"AIHubMix\"\nbase_url = \"https://example.com/v1\""
        }),
        None,
    )
}

#[test]
fn skills_ssot_migration_does_not_run_in_we2ai_mode_even_with_pending_flag_and_real_skill() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let home = ensure_test_home();
    let state = create_test_state().expect("create test state");

    // 存量数据：迁移标记开启，且 Claude 目录下真的有一个会被扫到的 Skill。
    state
        .db
        .set_setting("skills_ssot_migration_pending", "true")
        .expect("set pending flag");
    let claude_skill_dir = home.join(".claude").join("skills").join("real-skill");
    fs::create_dir_all(&claude_skill_dir).expect("create claude skill dir");
    fs::write(
        claude_skill_dir.join("SKILL.md"),
        "---\nname: real-skill\n---\nhello",
    )
    .expect("write SKILL.md");

    // WE2AI 模式下门控必须是 false——这是 lib.rs 里 `if
    // we2ai::mode::startup_allowed(StartupTask::SkillsSsotMigration) { ... }`
    // 唯一的判断依据。
    assert!(!cc_switch_lib::we2ai::mode::startup_allowed(
        cc_switch_lib::we2ai::mode::StartupTask::SkillsSsotMigration
    ));

    // 复现 lib.rs 的门控模式：门控为 false 时完全不调用迁移函数。
    if cc_switch_lib::we2ai::mode::startup_allowed(
        cc_switch_lib::we2ai::mode::StartupTask::SkillsSsotMigration,
    ) {
        panic!("should never reach here in WE2AI mode");
    }

    // 验证：数据库 Skills 表仍为空（真的没有导入），迁移标记原样保留（未被清除，
    // 因为迁移代码根本没跑到 `set_setting(..., "false")` 那一步）。
    let installed = state
        .db
        .get_all_installed_skills()
        .expect("read installed skills");
    assert!(
        installed.is_empty(),
        "expected no skills to be imported in WE2AI mode, found {installed:?}"
    );
    let flag = state
        .db
        .get_setting("skills_ssot_migration_pending")
        .expect("read pending flag");
    assert_eq!(
        flag.as_deref(),
        Some("true"),
        "pending flag must remain untouched when migration is gated off"
    );
}

#[test]
fn codex_history_migration_does_not_run_in_we2ai_mode_even_with_existing_third_party_provider() {
    let _guard = test_mutex().lock().expect("acquire test mutex");
    reset_test_fs();
    let home = ensure_test_home();
    let state = create_test_state().expect("create test state");

    // 存量数据：数据库里已经有一个会让 collect_source_model_provider_ids 非空
    // 的第三方 Codex 供应商，且 ~/.codex/sessions 下有一条真实会被改写的
    // session_meta 记录（provider = "aihubmix"）。
    state
        .db
        .save_provider("codex", &third_party_codex_provider())
        .expect("save third-party codex provider");

    let sessions_dir = home.join(".codex").join("sessions");
    fs::create_dir_all(&sessions_dir).expect("create codex sessions dir");
    let session_file = sessions_dir.join("rollout-test.jsonl");
    let original_content =
        r#"{"type":"session_meta","payload":{"id":"s1","model_provider":"aihubmix"}}"#;
    fs::write(&session_file, format!("{original_content}\n")).expect("write session jsonl");

    // WE2AI 模式下门控必须是 false。
    assert!(!cc_switch_lib::we2ai::mode::startup_allowed(
        cc_switch_lib::we2ai::mode::StartupTask::CodexHistoryMigration
    ));

    if cc_switch_lib::we2ai::mode::startup_allowed(
        cc_switch_lib::we2ai::mode::StartupTask::CodexHistoryMigration,
    ) {
        panic!("should never reach here in WE2AI mode");
    }

    // 验证：session 文件内容原样未变（没有被改写成 "custom" 桶），迁移完成标记
    // 也没有被写入。
    let after = fs::read_to_string(&session_file).expect("read session jsonl back");
    assert_eq!(
        after.trim_end(),
        original_content,
        "session file must be untouched when Codex history migration is gated off"
    );
    assert!(
        !cc_switch_lib::is_codex_third_party_history_provider_bucket_migrated(),
        "migration-completed marker must not be set when migration never ran"
    );
}
