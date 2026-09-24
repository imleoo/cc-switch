//! 工具配置目录与凭据文件的权限收紧（方案第 4.2 节"凭据文件保护"）。
//!
//! 目录收紧是主防线：写入前把三个实际解析的配置目录设为 0700，目录内任何
//! 临时文件、任何写入路径的中间状态都不会被其他本机用户读到。文件 0600 是
//! 纵深防御。只收紧，不放宽。Windows 不做 Unix 权限处理。

use std::path::{Path, PathBuf};

/// 目录收紧失败：属主不是当前用户、无法 chmod、chmod 后模式不符等。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirGuardError {
    pub path: PathBuf,
    pub reason: String,
}

impl std::fmt::Display for DirGuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "配置目录 {} 不属于当前用户或无法设为仅本人可访问，已停止写入（{}）",
            self.path.display(),
            self.reason
        )
    }
}

/// 依次执行：⓪ 不存在则以 0700 创建（含缺失的父级）；① 解析符号链接；
/// ② 核对属主为当前用户；③ chmod 0700；④ 重新 stat 确认。
#[cfg(unix)]
pub fn tighten_dir(path: &Path) -> Result<(), DirGuardError> {
    tighten_dir_for_uid(path, unsafe { libc::geteuid() })
}

#[cfg(unix)]
pub(crate) fn tighten_dir_for_uid(path: &Path, expected_uid: u32) -> Result<(), DirGuardError> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let fail = |reason: String| DirGuardError {
        path: path.to_path_buf(),
        reason,
    };
    if !path.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(|e| fail(format!("创建失败: {e}")))?;
    }
    let real = std::fs::canonicalize(path).map_err(|e| fail(format!("解析路径失败: {e}")))?;
    let meta = std::fs::metadata(&real).map_err(|e| fail(format!("读取属性失败: {e}")))?;
    if !meta.is_dir() {
        return Err(fail("不是目录".to_string()));
    }
    if meta.uid() != expected_uid {
        return Err(fail(format!("属主 uid {} 不是当前用户", meta.uid())));
    }
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| fail(format!("chmod 失败: {e}")))?;
    let mode = std::fs::metadata(&real)
        .map_err(|e| fail(format!("复查失败: {e}")))?
        .mode()
        & 0o777;
    if mode != 0o700 {
        return Err(fail(format!("chmod 后模式为 {mode:o}")));
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn tighten_dir(path: &Path) -> Result<(), DirGuardError> {
    if !path.exists() {
        std::fs::create_dir_all(path).map_err(|e| DirGuardError {
            path: path.to_path_buf(),
            reason: format!("创建失败: {e}"),
        })?;
    }
    Ok(())
}

/// 已存在的文件收紧为 0600；不存在则跳过。只收紧，不放宽（本来就更严格
/// 的模式如 0400 保持不变）。
pub fn tighten_file(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(path) {
            Ok(meta) => {
                let mode = meta.permissions().mode() & 0o777;
                let tightened = mode & 0o600;
                if tightened != mode {
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(tightened))?;
                }
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn creates_missing_dirs_as_0700_and_tightens_existing() {
        let tmp = TempDir::new().unwrap();
        let fresh = tmp.path().join("a/b/.claude");
        tighten_dir(&fresh).unwrap();
        assert_eq!(mode(&fresh), 0o700);

        let open = tmp.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
        tighten_dir(&open).unwrap();
        assert_eq!(mode(&open), 0o700);
    }

    #[test]
    fn follows_symlinks_to_the_real_dir() {
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        tighten_dir(&link).unwrap();
        assert_eq!(mode(&real), 0o700);
    }

    #[test]
    fn refuses_dirs_owned_by_another_user() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("shared");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let other_uid = unsafe { libc::geteuid() } + 1;
        let err = tighten_dir_for_uid(&dir, other_uid).unwrap_err();
        assert!(err.reason.contains("属主"));
        assert_eq!(mode(&dir), 0o755, "must not chmod a dir it refused");
    }

    #[test]
    fn tighten_file_only_removes_bits() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("models.json");
        std::fs::write(&f, "[]").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        tighten_file(&f).unwrap();
        assert_eq!(mode(&f), 0o600);
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o400)).unwrap();
        tighten_file(&f).unwrap();
        assert_eq!(mode(&f), 0o400);
        tighten_file(&tmp.path().join("missing")).unwrap();
    }
}
