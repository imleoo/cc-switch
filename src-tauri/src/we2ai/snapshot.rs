//! 工具 live 文件的快照与"写后记代次，恢复前比对"（方案第 4.1 节"跨进程"表）。
//!
//! - 快照：记录原字节（或"不存在"）与哈希 H0。
//! - 写入后：`switch` 返回后回读计算 H1（已计入上游规范化与 MCP 重投影）。
//! - 恢复：有 H1 时，当前 = H1 才恢复，= H0 视为未写入，其余视为被外部改写，
//!   不恢复并报告；没有 H1（`switch` 中途失败）时，当前 = H0 跳过，否则视为
//!   本次部分写入，按快照写回。
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
        })
    }

    /// 标记"从这一刻起会真正尝试写入这个文件"（例如即将调用上游 `switch`
    /// 管道，或 WE2AI 自己紧接着要写入）。在标记之前，`restore()` 遇到与
    /// H0 不同的当前内容一律视为外部程序的改动、不覆盖——快照捕获之后、
    /// 真正动笔写入之前可能有一段耗时的准备工作（如阻塞在系统钥匙串授权
    /// 弹窗上），这段时间里发生的外部编辑不是我们造成的半写状态，不该被
    /// "没有 H1 就按部分写入回滚"的规则误伤（P6 二轮 Opus 复核高危项 1a）。
    pub fn mark_write_attempted(&mut self) {
        self.write_attempted = true;
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
        a.mark_write_attempted();
        b.mark_write_attempted();
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
        snap.mark_write_attempted();
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
        snap.mark_write_attempted();
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

    #[cfg(unix)]
    #[test]
    fn restored_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("claude.json");
        std::fs::write(&f, "old").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut snap = FileSnapshot::capture(&f).unwrap();
        snap.mark_write_attempted();
        std::fs::write(&f, "ours").unwrap();
        snap.record_h1();
        assert_eq!(snap.restore(), RestoreResult::Restored);
        let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
