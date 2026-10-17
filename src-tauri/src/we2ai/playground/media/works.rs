//! 本机作品索引：生成状态与文件元数据原子落盘，旧媒体文件自动补入。
use super::*;
use std::collections::HashSet;

static WORKS_LOCK: Mutex<()> = Mutex::new(());
static RUN_ID: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| uuid::Uuid::new_v4().to_string());

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StudioWork {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub status: String,
    pub saved_at_ms: u64,
    pub favorite: bool,
    pub model: String,
    pub progress: Option<u32>,
    pub message: Option<String>,
    pub files: Vec<PlaygroundMediaFile>,
    pub content: Option<String>,
    #[serde(default)]
    run_id: String,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn record_path(root: &Path, id: &str) -> std::io::Result<PathBuf> {
    if id.is_empty()
        || id.len() > 160
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        || id.contains("..")
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "无效作品编号",
        ));
    }
    Ok(studio_dir(root).join("works").join(format!("{id}.json")))
}

fn write_record(root: &Path, work: &StudioWork) -> std::io::Result<()> {
    let path = record_path(root, &work.id)?;
    std::fs::create_dir_all(path.parent().unwrap())?;
    let partial = path.with_extension("json.part");
    let bytes = serde_json::to_vec(work)?;
    let result = std::fs::write(&partial, bytes).and_then(|_| std::fs::rename(&partial, path));
    if result.is_err() {
        let _ = std::fs::remove_file(partial);
    }
    result
}

fn load_record(root: &Path, id: &str) -> std::io::Result<StudioWork> {
    let path = record_path(root, id)?;
    if std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "无效作品记录",
        ));
    }
    let work: StudioWork = serde_json::from_slice(&std::fs::read(path)?)?;
    if work.id != id {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "作品编号不匹配",
        ));
    }
    Ok(work)
}

pub(crate) struct WorkGuard {
    root: PathBuf,
    id: String,
    finished: bool,
}

impl WorkGuard {
    pub(crate) fn start(
        root: &Path,
        _request_id: &str,
        kind: &str,
        model: &str,
        prompt: &str,
    ) -> Result<Self, We2aiApiError> {
        let _lock = WORKS_LOCK.lock().unwrap();
        let work = StudioWork {
            id: uuid::Uuid::new_v4().to_string(),
            name: prompt.trim().chars().take(80).collect(),
            kind: kind.into(),
            status: "processing".into(),
            saved_at_ms: now_ms(),
            favorite: false,
            model: model.into(),
            progress: None,
            message: None,
            files: vec![],
            content: None,
            run_id: RUN_ID.clone(),
        };
        write_record(root, &work)
            .map_err(|_| playground_error(CODE_SAVE_FAILED, "创建作品记录失败，未提交生成"))?;
        Ok(Self {
            root: root.into(),
            id: work.id,
            finished: false,
        })
    }

    fn update(&self, change: impl FnOnce(&mut StudioWork)) -> std::io::Result<()> {
        let _lock = WORKS_LOCK.lock().unwrap();
        let mut work = load_record(&self.root, &self.id)?;
        change(&mut work);
        write_record(&self.root, &work)
    }

    pub(crate) fn progress(&self, value: Option<u32>) {
        if let Err(e) = self.update(|w| w.progress = value) {
            log::warn!("保存作品进度失败: {e}");
        }
    }

    pub(crate) fn finish(
        &mut self,
        result: Result<Vec<PlaygroundMediaFile>, &We2aiApiError>,
        content: Option<String>,
    ) -> Result<(), We2aiApiError> {
        self.update(|w| match result {
            Ok(files) => {
                w.status = "success".into();
                w.progress = Some(100);
                w.content = content;
                w.files = files
                    .into_iter()
                    .map(|mut f| {
                        f.data_url = None;
                        f
                    })
                    .collect();
            }
            Err(e) => {
                w.status = if e.code == CODE_CANCELLED || e.code == CODE_IDENTITY_CHANGED {
                    "cancelled"
                } else {
                    "failed"
                }
                .into();
                // 只保存稳定错误码，不保存上游错误正文或凭据。
                w.message = Some(e.code.clone());
            }
        })
        .map_err(|_| playground_error(CODE_SAVE_FAILED, "保存作品记录失败，请到作品列表刷新"))?;
        self.finished = true;
        Ok(())
    }

    pub(crate) fn save_text(&self, text: &str) -> Result<PlaygroundMediaFile, We2aiApiError> {
        let path = studio_dir(&self.root).join(format!("{}.txt", self.id));
        let partial = path.with_extension("txt.part");
        std::fs::write(&partial, text)
            .and_then(|_| std::fs::rename(&partial, &path))
            .map_err(|_| playground_error(CODE_SAVE_FAILED, "保存文案失败"))?;
        Ok(PlaygroundMediaFile {
            path: path.to_string_lossy().into_owned(),
            mime: "text/plain".into(),
            bytes: text.len() as u64,
            data_url: None,
        })
    }
}

impl Drop for WorkGuard {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.update(|w| {
                w.status = "interrupted".into();
                w.message = Some("PLAYGROUND_INTERRUPTED".into());
            });
        }
    }
}

fn list_works(root: &Path) -> std::io::Result<Vec<StudioWork>> {
    let _lock = WORKS_LOCK.lock().unwrap();
    let mut works = vec![];
    let dir = studio_dir(root).join("works");
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if !entry.file_type()?.is_file()
                    || entry.path().extension().and_then(|s| s.to_str()) != Some("json")
                {
                    continue;
                }
                let id = entry
                    .path()
                    .file_stem()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                let mut work = match load_record(root, &id) {
                    Ok(w) => w,
                    Err(e) => {
                        log::warn!("忽略损坏的作品记录: {e}");
                        continue;
                    }
                };
                if work.status == "processing" && work.run_id != *RUN_ID {
                    work.status = "interrupted".into();
                    work.message = Some("PLAYGROUND_INTERRUPTED".into());
                    write_record(root, &work)?;
                }
                works.push(work);
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let indexed: HashSet<_> = works
        .iter()
        .flat_map(|w| {
            w.files.iter().map(|f| {
                PathBuf::from(&f.path)
                    .canonicalize()
                    .unwrap_or_else(|_| PathBuf::from(&f.path))
            })
        })
        .collect();
    for old in list_studio_works(root)? {
        if indexed.contains(Path::new(&old.file.path)) {
            continue;
        }
        works.push(StudioWork {
            id: format!("legacy-{}", old.name),
            name: old.name,
            kind: if old.file.mime.starts_with("image/") {
                "image"
            } else {
                "video"
            }
            .into(),
            status: "success".into(),
            saved_at_ms: old.saved_at_ms,
            favorite: false,
            model: String::new(),
            progress: Some(100),
            message: None,
            files: vec![old.file],
            content: None,
            run_id: String::new(),
        });
    }
    works.sort_by(|a, b| {
        b.saved_at_ms
            .cmp(&a.saved_at_ms)
            .then_with(|| b.id.cmp(&a.id))
    });
    Ok(works)
}

#[tauri::command]
pub async fn we2ai_playground_media_list(
    session: State<'_, We2aiSessionState>,
) -> Result<Vec<StudioWork>, We2aiApiError> {
    let root = session.0.data_root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || list_works(&root))
        .await
        .map_err(|_| playground_error(CODE_SAVE_FAILED, "读取本机作品失败"))?
        .map_err(|_| playground_error(CODE_SAVE_FAILED, "读取本机作品失败"))
}

fn change_work(root: &Path, id: &str, favorite: Option<bool>) -> std::io::Result<()> {
    let mut work = list_works(root)?
        .into_iter()
        .find(|w| w.id == id)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "作品不存在"))?;
    let _lock = WORKS_LOCK.lock().unwrap();
    if record_path(root, id)?.exists() {
        work = load_record(root, id)?;
    }
    if let Some(favorite) = favorite {
        work.favorite = favorite;
        return write_record(root, &work);
    }
    if work.status == "processing" {
        return Err(std::io::Error::other("生成中的作品不能删除，请先停止生成"));
    }
    // 先校验所有路径，再修改，拒绝索引中注入的目录外文件。
    let paths = work
        .files
        .iter()
        .map(|f| {
            let path = PathBuf::from(&f.path);
            if path.parent().and_then(|p| p.canonicalize().ok())
                != studio_dir(root).canonicalize().ok()
            {
                return Err(std::io::Error::other("无效作品文件"));
            }
            if path.exists() {
                resolve_studio_path(root, &f.path)
                    .map_err(|_| std::io::Error::other("无效作品文件"))
            } else {
                Ok(path)
            }
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    for path in paths {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    let record = record_path(root, id)?;
    if record.exists() {
        std::fs::remove_file(record)?;
    }
    Ok(())
}

#[tauri::command]
pub async fn we2ai_playground_work_update(
    session: State<'_, We2aiSessionState>,
    id: String,
    favorite: Option<bool>,
) -> Result<(), We2aiApiError> {
    let root = session.0.data_root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || change_work(&root, &id, favorite))
        .await
        .map_err(|_| playground_error(CODE_SAVE_FAILED, "作品操作失败"))?
        .map_err(|_| playground_error(CODE_SAVE_FAILED, "作品操作失败，请刷新后重试"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn image(root: &Path, id: &str) -> PlaygroundMediaFile {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend([0u8; 32]);
        let media = DecodedMedia {
            bytes,
            mime: "image/png",
        };
        let path = save_media(&studio_dir(root), id, 0, &media).unwrap();
        media_file_view(&path, &media, true)
    }
    #[test]
    fn text_image_and_legacy_video_persist_without_duplicates() {
        let root = tempfile::tempdir().unwrap();
        let mut text =
            WorkGuard::start(root.path(), "r1", "text", "text-model", "营销文案").unwrap();
        assert_eq!(list_works(root.path()).unwrap()[0].status, "processing");
        let file = text.save_text("完整的生成文案").unwrap();
        text.finish(Ok(vec![file.clone()]), Some("完整的生成文案".into()))
            .unwrap();
        let mut images =
            WorkGuard::start(root.path(), "r2", "image", "image-model", "产品图片").unwrap();
        images
            .finish(
                Ok(vec![image(root.path(), "one"), image(root.path(), "two")]),
                None,
            )
            .unwrap();
        let video = DecodedMedia {
            bytes: b"\0\0\0\x18ftypisom0000".to_vec(),
            mime: "video/mp4",
        };
        save_media(&studio_dir(root.path()), "old-video", 0, &video).unwrap();
        drop(images);
        drop(text);
        let works = list_works(root.path()).unwrap();
        assert_eq!(works.len(), 3);
        assert_eq!(
            works
                .iter()
                .find(|w| w.kind == "image")
                .unwrap()
                .files
                .len(),
            2
        );
        let text = works.iter().find(|w| w.kind == "text").unwrap();
        assert_eq!(text.content.as_deref(), Some("完整的生成文案"));
        assert_eq!(
            std::fs::read_to_string(file.path).unwrap(),
            "完整的生成文案"
        );
        assert!(works
            .iter()
            .flat_map(|w| &w.files)
            .all(|f| f.data_url.is_none()));
    }
    #[test]
    fn favorites_survive_progress_and_reload_and_delete_requires_terminal() {
        let root = tempfile::tempdir().unwrap();
        let mut work = WorkGuard::start(root.path(), "r", "video", "m", "title").unwrap();
        assert!(change_work(root.path(), &work.id, None).is_err());
        work.progress(Some(52));
        change_work(root.path(), &work.id, Some(true)).unwrap();
        work.progress(Some(70));
        work.finish(Err(&playground_error(CODE_CANCELLED, "cancelled")), None)
            .unwrap();
        let listed = list_works(root.path()).unwrap();
        assert!(listed[0].favorite);
        assert_eq!(listed[0].status, "cancelled");
        assert_eq!(listed[0].progress, Some(70));
        change_work(root.path(), &work.id, None).unwrap();
        assert!(list_works(root.path()).unwrap().is_empty());
    }
    #[test]
    fn old_process_records_and_failed_tasks_are_not_left_generating() {
        let root = tempfile::tempdir().unwrap();
        let work = WorkGuard::start(root.path(), "r", "video", "m", "title").unwrap();
        work.update(|w| w.run_id = "previous-process".into())
            .unwrap();
        assert_eq!(list_works(root.path()).unwrap()[0].status, "interrupted");
        drop(work);
        let mut failed = WorkGuard::start(root.path(), "r2", "video", "m", "title").unwrap();
        failed
            .finish(
                Err(&playground_error(
                    CODE_MEDIA_FAILED,
                    "private upstream message",
                )),
                None,
            )
            .unwrap();
        let w = load_record(root.path(), &failed.id).unwrap();
        assert_eq!(w.status, "failed");
        assert_eq!(w.message.as_deref(), Some(CODE_MEDIA_FAILED));
    }
    #[test]
    fn legacy_image_favorite_and_confirmed_delete_remove_record_and_file() {
        let root = tempfile::tempdir().unwrap();
        let file = image(root.path(), "legacy");
        let id = list_works(root.path()).unwrap()[0].id.clone();
        change_work(root.path(), &id, Some(true)).unwrap();
        assert_eq!(list_works(root.path()).unwrap().len(), 1);
        assert!(list_works(root.path()).unwrap()[0].favorite);
        change_work(root.path(), &id, None).unwrap();
        assert!(!Path::new(&file.path).exists());
        assert!(list_works(root.path()).unwrap().is_empty());
    }
    #[test]
    fn deleting_an_index_cannot_delete_outside_files() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let mut work = WorkGuard::start(root.path(), "r", "image", "m", "title").unwrap();
        work.finish(
            Ok(vec![PlaygroundMediaFile {
                path: outside.path().to_string_lossy().into_owned(),
                mime: "image/png".into(),
                bytes: 0,
                data_url: None,
            }]),
            None,
        )
        .unwrap();
        assert!(change_work(root.path(), &work.id, None).is_err());
        assert!(outside.path().exists());
        assert!(record_path(root.path(), "../../outside").is_err());
    }
    #[test]
    fn missing_file_can_be_removed_and_corrupt_record_does_not_hide_other_works() {
        let root = tempfile::tempdir().unwrap();
        let mut work = WorkGuard::start(root.path(), "r", "text", "m", "title").unwrap();
        let file = work.save_text("saved").unwrap();
        work.finish(Ok(vec![file.clone()]), Some("saved".into()))
            .unwrap();
        std::fs::write(studio_dir(root.path()).join("works/broken.json"), b"broken").unwrap();
        assert_eq!(list_works(root.path()).unwrap().len(), 1);
        std::fs::remove_file(file.path).unwrap();
        change_work(root.path(), &work.id, None).unwrap();
        assert!(list_works(root.path()).unwrap().is_empty());
    }
}
