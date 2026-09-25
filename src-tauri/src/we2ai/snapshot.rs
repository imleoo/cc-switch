//! 工具 live 文件的快照与"写后记代次，恢复前比对"（方案第 4.1 节"跨进程"表）。
//!
//! - 快照：记录原字节（或"不存在"）与哈希 H0。
//! - 标记：真正可能开始写入前调用 `mark_write_attempted()`，同时记录这一刻
//!   的哈希 H_pre（P6 四轮新增）。
//! - 写入后：`switch` 返回后回读计算 H1（已计入上游规范化与 MCP 重投影）。
//! - 恢复：当前 = H0 → 未动过，`Unchanged`；从未标记过 → 现在的任何差异都
//!   只可能来自外部程序，`ExternalModified`，不覆盖；已标记但 H_pre 未知
//!   （标记那一刻读取失败，例如权限问题）→ 无法判断是否真的写过，一律不
//!   写入，`Failed`；已标记且当前 = H_pre → 从标记那一刻到现在这个文件其实
//!   从未被写过（例如一次 `switch` 统一给多个文件打标记，其中某个文件因
//!   管道内部逻辑或更早的失败而没被真正碰到）——按 H_pre 是否等于 H0 分别
//!   报告 `Unchanged`/`ExternalModified`，不写入；已标记且当前 ≠ H_pre 时
//!   才看 H1：有 H1 时当前 = H1 才恢复，其余视为被外部改写、不恢复并报告；
//!   没有 H1（`switch` 中途失败）时按快照写回。
//!
//! 比对与替换之间的毫秒级窗口无法消除，是方案登记的已知限制。

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub type Hash = Option<[u8; 32]>;

/// 读取文件并计算哈希；文件不存在为 `None`。
pub fn hash_file(path: &Path) -> std::io::Result<Hash> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(Sha256::digest(&bytes).into())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreResult {
    /// 已按快照写回（或删除本次新建的文件）。
    Restored,
    /// 文件与快照一致，无需恢复。
    Unchanged,
    /// 写入后又被其他程序改写，未回滚。
    ExternalModified,
    /// 恢复失败或恢复后核对不一致。
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct FileSnapshot {
    pub path: PathBuf,
    original: Option<Vec<u8>>,
    h0: Hash,
    h1: Option<Hash>,
    /// 是否已经真正开始尝试写入这个文件（见 [`Self::mark_write_attempted`]）。
    write_attempted: bool,
    /// 标记那一刻的哈希（见 [`Self::mark_write_attempted`]）。
    h_pre: Option<Hash>,
}

impl FileSnapshot {
    pub fn capture(path: &Path) -> std::io::Result<Self> {
        let original = match std::fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        let h0 = original.as_ref().map(|b| Sha256::digest(b).into());
        Ok(Self {
            path: path.to_path_buf(),
            original,
            h0,
            h1: None,
            write_attempted: false,
            h_pre: None,
        })
    }

    /// 标记"从这一刻起会真正尝试写入这个文件"（例如即将调用上游 `switch`
    /// 管道，或 WE2AI 自己紧接着要写入），同时记录这一刻的哈希 `h_pre`。
    ///
    /// 在标记之前，`restore()` 遇到与 H0 不同的当前内容一律视为外部程序的
    /// 改动、不覆盖——快照捕获之后、真正动笔写入之前可能有一段耗时的准备
    /// 工作（如阻塞在系统钥匙串授权弹窗上），这段时间里发生的外部编辑不是
    /// 我们造成的半写状态，不该被"没有 H1 就按部分写入回滚"的规则误伤
    /// （P6 二轮 Opus 复核高危项 1a）。
    ///
    /// 但"已标记"不等于"这个文件真的被写过"：一次 `switch` 调用会给多个
    /// 文件统一打标记，其中某些文件可能因为管道内部逻辑（如 Codex 保留
    /// ChatGPT 登录时 `auth.json` 从不写入）或更早的失败（写本地 current
    /// 失败在触碰任何 live 文件之前）而从未被真正写入。`h_pre` 就是用来
    /// 区分这种情况：恢复时如果当前哈希仍然等于标记那一刻的 `h_pre`，说明
    /// 从标记到现在这个文件压根没有变化——不管 `write_attempted` 是不是
    /// `true`，都不该走"没有 H1 就当部分写入回滚"的逻辑，而要看 `h_pre`
    /// 是否等于 H0 来判定是"确实没动过"还是"标记前已经被外部改过"
    /// （P6 四轮 Opus 复核高危项 1）。
    ///
    /// 读取失败时不再静默吞掉（`.ok()`）：调用方必须把 `Err` 当作"标记这一
    /// 步本身失败了"处理——`NotFound` 已经在 `hash_file` 内部归一化成
    /// "文件不存在"这个正常状态（`Ok(None)`），这里只会因为权限问题等真正
    /// 的 I/O 错误才返回 `Err`。调用方应在真正调用 `switch` 之前就中止整个
    /// apply，不写任何 live 文件（P6 五轮 Codex 验收高危项 2）。即便调用方
    /// 没有正确中止，`write_attempted` 仍会被设为 `true` 而 `h_pre` 保持
    /// 未知，`restore()` 对这种"标记了但不知道标记那一刻内容"的状态有独立
    /// 的兜底：一律不写入。
    pub fn mark_write_attempted(&mut self) -> std::io::Result<()> {
        self.write_attempted = true;
        self.h_pre = Some(hash_file(&self.path)?);
        Ok(())
    }

    pub fn existed(&self) -> bool {
        self.original.is_some()
    }

    pub fn h0(&self) -> Hash {
        self.h0
    }

    pub fn original_bytes(&self) -> Option<&[u8]> {
        self.original.as_deref()
    }

    /// 写入后（已记录 H1）内容与快照不同。
    pub fn changed(&self) -> bool {
        matches!(self.h1, Some(h1) if h1 != self.h0)
    }

    /// 记录写入后的哈希 H1。读取失败时不记录，恢复时按"无 H1"处理。
    pub fn record_h1(&mut self) {
        if let Ok(h) = hash_file(&self.path) {
            self.h1 = Some(h);
        }
    }

    /// 直接用调用方已经成功写入磁盘的字节计算并记录 H1，不重新读盘。
    ///
    /// 与 [`Self::record_h1`] 的区别：那个方法在"写入"与"记录"之间隔着一次
    /// 磁盘读取，如果这两步之间有外部程序抢先改写了文件，读到的会是外部
    /// 内容而不是我们真正写下的内容，导致 H1 被错误地"认领"成外部编辑
    /// ——后续恢复时只要外部程序没有再次改动，`current == h1` 就会成立，
    /// 从而把这次外部编辑当成"我们写的、可以安全回滚"的内容覆盖掉
    /// （P6 四轮 Opus 复核高危项 2）。调用方必须保证传入的 `bytes` 与刚刚
    /// 成功写入磁盘的字节完全一致，这样 H1 从一开始就不依赖任何后续读盘。
    pub fn record_h1_from_bytes(&mut self, bytes: &[u8]) {
        self.h1 = Some(Some(Sha256::digest(bytes).into()));
    }

    pub fn restore(&self) -> RestoreResult {
        let current = match hash_file(&self.path) {
            Ok(h) => h,
            Err(e) => return RestoreResult::Failed(format!("读取失败: {e}")),
        };
        if current == self.h0 {
            return RestoreResult::Unchanged;
        }
        if !self.write_attempted {
            // 从未真正尝试写入：现在的差异只可能来自外部程序，不是我们的
            // 半写状态，不能覆盖（P6 二轮 Opus 复核高危项 1a）。
            return RestoreResult::ExternalModified;
        }
        if self.h_pre.is_none() {
            // 已标记，但标记那一刻读取失败（权限问题等真正的 I/O 错误，
            // `mark_write_attempted()` 不再吞掉这类错误）：无法判断从标记
            // 到现在这段时间文件是否真的被我们写过，为安全起见一律不写
            // 入，按"恢复失败"上报，交给调用方决定如何提示用户
            // （P6 五轮 Codex 验收高危项 2）。
            return RestoreResult::Failed("标记写入前读取文件失败，无法判断是否需要恢复".into());
        }
        if let Some(h_pre) = self.h_pre {
            if current == h_pre {
                // 从"标记即将写入"那一刻到现在，这个文件的内容压根没变过
                // ——不管 `write_attempted` 是不是 true，这次调用实际上从未
                // 写过这个文件（例如 Codex 保留 ChatGPT 登录时 `auth.json`
                // 从不写入，或更早的失败发生在触碰任何 live 文件之前）。
                // 不能套用"没有 H1 就当部分写入回滚"的逻辑，否则会把标记
                // 之前就已经存在、且从未被我们改动过的外部差异强行覆盖掉
                // （P6 四轮 Opus 复核高危项 1）。按差异是"标记前已经存在"
                // 还是"确实没有任何变化"分别报告。
                return if h_pre == self.h0 {
                    RestoreResult::Unchanged
                } else {
                    RestoreResult::ExternalModified
                };
            }
        }
        if let Some(h1) = self.h1 {
            if current != h1 {
                return RestoreResult::ExternalModified;
            }
        }
        let write_result = match &self.original {
            Some(bytes) => {
                crate::config::atomic_write_private(&self.path, bytes).map_err(|e| e.to_string())
            }
            None => match std::fs::remove_file(&self.path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.to_string()),
            },
        };
        if let Err(e) = write_result {
            return RestoreResult::Failed(e);
        }
        match hash_file(&self.path) {
            Ok(h) if h == self.h0 => RestoreResult::Restored,
            Ok(_) => RestoreResult::Failed("恢复后内容与快照不一致".to_string()),
            Err(e) => RestoreResult::Failed(format!("恢复后读取失败: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn restores_our_write_and_deletes_files_we_created() {
        let tmp = TempDir::new().unwrap();
        let existing = tmp.path().join("settings.json");
        std::fs::write(&existing, "old").unwrap();
        let created = tmp.path().join("new.json");

        let mut a = FileSnapshot::capture(&existing).unwrap();
        let mut b = FileSnapshot::capture(&created).unwrap();
        a.mark_write_attempted().unwrap();
        b.mark_write_attempted().unwrap();
        std::fs::write(&existing, "ours").unwrap();
        std::fs::write(&created, "ours").unwrap();
        a.record_h1();
        b.record_h1();

        assert_eq!(a.restore(), RestoreResult::Restored);
        assert_eq!(std::fs::read_to_string(&existing).unwrap(), "old");
        assert_eq!(b.restore(), RestoreResult::Restored);
        assert!(!created.exists());
    }

    #[test]
    fn does_not_overwrite_an_external_change_after_our_write() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("config.toml");
        std::fs::write(&f, "old").unwrap();
        let mut snap = FileSnapshot::capture(&f).unwrap();
        snap.mark_write_attempted().unwrap();
        std::fs::write(&f, "ours").unwrap();
        snap.record_h1();
        std::fs::write(&f, "someone else").unwrap();
        assert_eq!(snap.restore(), RestoreResult::ExternalModified);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "someone else");
    }

    #[test]
    fn without_h1_a_change_after_a_write_was_attempted_is_treated_as_a_partial_write() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("config.toml");
        std::fs::write(&f, "old").unwrap();
        let mut snap = FileSnapshot::capture(&f).unwrap();
        assert_eq!(snap.restore(), RestoreResult::Unchanged);
        snap.mark_write_attempted().unwrap();
        std::fs::write(&f, "partial").unwrap();
        assert_eq!(snap.restore(), RestoreResult::Restored);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "old");
    }

    /// P6 二轮 Opus 复核高危项 1a：从未标记"尝试写入"（例如失败发生在快照
    /// 之后、真正开始写之前的准备阶段）时，即便没有 H1，外部改动也绝不能
    /// 被当成"我们的半写状态"覆盖掉。
    #[test]
    fn without_a_write_attempt_an_external_change_is_never_overwritten() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("config.toml");
        std::fs::write(&f, "old").unwrap();
        let snap = FileSnapshot::capture(&f).unwrap();
        std::fs::write(&f, "edited by someone else while we were still preparing").unwrap();
        assert_eq!(snap.restore(), RestoreResult::ExternalModified);
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            "edited by someone else while we were still preparing"
        );
    }

    /// P6 四轮 Opus 复核高危项 1：标记"即将写入"之前，文件就已经被外部改过
    /// （`h_pre` 因此已经与 H0 不同）；标记之后这个文件从未被真正写过（没有
    /// 记录 H1）。即便 `write_attempted` 是 `true`，也不能套用"没有 H1 就当
    /// 部分写入回滚"的旧逻辑——那会把标记前就存在的外部差异错误地覆盖掉。
    #[test]
    fn a_pre_existing_external_edit_survives_when_the_file_was_marked_but_never_actually_written() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("auth.json");
        std::fs::write(&f, "old").unwrap();
        let mut snap = FileSnapshot::capture(&f).unwrap();
        // 标记之前，外部程序已经把内容改了（例如 Codex 自己刷新了令牌）。
        std::fs::write(&f, "refreshed by codex itself").unwrap();
        snap.mark_write_attempted().unwrap();
        // 这个文件从标记到现在从未被我们写过，也就没有 record_h1()。
        assert_eq!(snap.restore(), RestoreResult::ExternalModified);
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            "refreshed by codex itself"
        );
    }

    /// P6 五轮 Codex 验收高危项 2：标记那一刻读取失败（权限问题等真正的
    /// I/O 错误，不是"文件不存在"）不能被 `.ok()` 静默吞掉——`mark_write_attempted()`
    /// 必须把错误报给调用方；即便调用方之后仍然调用了 `restore()`
    /// （防御性兜底），也绝不能因为 H_pre 未知就冒险按旧逻辑写入，只能报告
    /// `Failed` 且不改动文件。
    #[cfg(unix)]
    #[test]
    fn a_read_error_at_mark_time_is_not_swallowed_and_restore_never_writes_when_h_pre_is_unknown() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("config.toml");
        std::fs::write(&f, "old").unwrap();
        let mut snap = FileSnapshot::capture(&f).unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o000)).unwrap();
        let mark_result = snap.mark_write_attempted();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            mark_result.is_err(),
            "a permission-denied read at mark time must not be swallowed as success"
        );
        // 标记失败之后内容又变了（current != H0），才会真正走到"已标记但
        // H_pre 未知"这条新分支——如果内容没变，`restore()` 会在更早的
        // "current == H0" 检查里直接返回 `Unchanged`，测不到这条分支。
        std::fs::write(&f, "changed after the failed mark").unwrap();
        match snap.restore() {
            RestoreResult::Failed(_) => {}
            other => panic!("expected Failed when h_pre is unknown, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            "changed after the failed mark",
            "restore() must never write when h_pre is unknown"
        );
    }

    #[cfg(unix)]
    #[test]
    fn restored_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("claude.json");
        std::fs::write(&f, "old").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut snap = FileSnapshot::capture(&f).unwrap();
        snap.mark_write_attempted().unwrap();
        std::fs::write(&f, "ours").unwrap();
        snap.record_h1();
        assert_eq!(snap.restore(), RestoreResult::Restored);
        let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
