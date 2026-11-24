// 模型管理 Tauri commands

use super::catalog::{self, LocalModelInfo, ModelInfo};
use super::downloader;
use crate::storage::Storage;
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::AppHandle;

use crate::error_protocol;

// [自訂] S88 / SC-35：每個來源必須提供同一份且唯一的權重。
fn catalog_file(model: &ModelInfo) -> Result<&catalog::ModelFile, String> {
    let invalid = || error_protocol::encode(
        "local_model_invalid",
        "The catalog must specify the same single weight file for every source.",
    );
    let Some([expected]) = model.sources.first().map(|source| source.files.as_slice()) else {
        return Err(invalid());
    };
    for source in &model.sources {
        match source.files.as_slice() {
            [file] if file.name == expected.name
                && file.size_bytes == expected.size_bytes
                && file.sha256 == expected.sha256 => {}
            _ => return Err(invalid()),
        }
    }
    Ok(expected)
}

fn model_file_path(dir: &Path, file: &catalog::ModelFile) -> Result<PathBuf, String> {
    let missing = || error_protocol::encode(
        "local_model_missing",
        format!("Missing model file: {}.", file.name),
    );
    let read_error = |e| error_protocol::encode(
        "local_model_read",
        format!("Cannot read model folder for {}: {e}", file.name),
    );
    let entries = std::fs::read_dir(dir).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound { missing() } else { read_error(e) }
    })?;
    let mut other_gguf = false;
    for entry in entries {
        let entry = entry.map_err(read_error)?;
        let name = entry.file_name();
        if name == std::ffi::OsStr::new(&file.name) {
            return Ok(entry.path());
        }
        other_gguf |= Path::new(&name).extension().and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("gguf"));
    }
    if other_gguf {
        return Err(error_protocol::encode(
            "local_model_invalid",
            format!("Use the exact catalog filename: {}.", file.name),
        ));
    }
    Err(missing())
}

// [自訂] S88：只快取最近一次成功驗證的檔案。Unix 的 ctime 也會反映同大小覆寫。
#[cfg(unix)]
#[derive(PartialEq, Eq)]
struct FileIdentity {
    path: PathBuf,
    device: u64,
    inode: u64,
    size: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
    expected_sha256: String,
}

#[cfg(unix)]
impl FileIdentity {
    fn new(path: &Path, metadata: &std::fs::Metadata, sha256: &str) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            path: path.into(),
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            mtime: (metadata.mtime(), metadata.mtime_nsec()),
            ctime: (metadata.ctime(), metadata.ctime_nsec()),
            expected_sha256: sha256.into(),
        }
    }
}

#[cfg(unix)]
static VERIFIED_FILE: std::sync::Mutex<Option<FileIdentity>> = std::sync::Mutex::new(None);

fn verify_local_file(
    dir: &Path,
    spec: &catalog::ModelFile,
    #[cfg(unix)] cache: &std::sync::Mutex<Option<FileIdentity>>,
    verify_hash: impl FnOnce(&std::fs::File, &str) -> Result<(), String>,
) -> Result<PathBuf, String> {
    let path = model_file_path(dir, spec)?;
    let read_error = |e| error_protocol::encode(
        "local_model_read",
        format!("Cannot read model file {}: {e}", spec.name),
    );
    if !std::fs::metadata(&path).map_err(read_error)?.is_file() {
        return Err(error_protocol::encode(
            "local_model_invalid",
            format!("Model file {} must be a regular file.", spec.name),
        ));
    }
    let file = std::fs::File::open(&path).map_err(read_error)?;
    let metadata = file.metadata().map_err(read_error)?;
    if !metadata.is_file() || metadata.len() != spec.size_bytes {
        return Err(error_protocol::encode(
            "local_model_invalid",
            format!(
                "Model file {} has the wrong size: expected {} bytes, found {}.",
                spec.name, spec.size_bytes, metadata.len()
            ),
        ));
    }
    let sha256 = spec
        .sha256
        .as_deref()
        .ok_or_else(|| error_protocol::encode(
            "local_model_invalid",
            format!("Missing catalog SHA-256 for {}.", spec.name),
        ))?;

    #[cfg(unix)]
    let identity = FileIdentity::new(&path, &metadata, sha256);
    #[cfg(unix)]
    if cache
        .lock()
        .ok()
        .is_some_and(|cached| cached.as_ref() == Some(&identity))
    {
        return Ok(path);
    }

    verify_hash(&file, sha256).map_err(|error| {
        if downloader::is_checksum_error(&error) {
            return error_protocol::encode(
                "local_model_invalid",
                format!("SHA-256 verification failed for {}.", spec.name),
            );
        }
        // [自訂] SC-35：共用驗證器的 I/O 細節來自已開啟的檔案，沒有路徑。
        let reason = error.splitn(3, ':').nth(2).unwrap_or(&error);
        error_protocol::encode("local_model_read", format!("{}: {reason}", spec.name))
    })?;

    #[cfg(unix)]
    {
        if identity != FileIdentity::new(&path, &file.metadata().map_err(read_error)?, sha256) {
            return Err(error_protocol::encode(
                "local_model_invalid",
                format!("Model file {} changed during verification. Try again.", spec.name),
            ));
        }
        if let Ok(mut cached) = cache.lock() {
            *cached = Some(identity);
        }
    }
    Ok(path)
}

fn catalog_model(model_id: &str) -> Result<ModelInfo, String> {
    catalog::get_available_models()
        .into_iter()
        .find(|model| model.id == model_id)
        .ok_or_else(|| error_protocol::encode(
            "local_model_missing",
            "Unknown local model ID. Select a model from the catalog.",
        ))
}

pub(super) fn verified_model_path(model_id: &str) -> Result<PathBuf, String> {
    let model = catalog_model(model_id)?;
    let file = catalog_file(&model)?;
    verify_local_file(
        &downloader::model_dir(&model.id),
        file,
        #[cfg(unix)]
        &VERIFIED_FILE,
        downloader::verify_open_file_sha256,
    )
}

// [自訂] SC-35：列表只讀檔名與大小，載入時才核對 SHA-256。
fn listed_weight(dir: &Path, file: &catalog::ModelFile) -> Option<(u64, bool)> {
    let path = model_file_path(dir, file).ok()?;
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() == 0 {
        return None;
    }
    let size = metadata.len();
    Some((size, size == file.size_bytes))
}

/// 列出所有可用模型
#[tauri::command]
pub fn list_available_models() -> Vec<ModelInfo> {
    catalog::get_available_models()
}

/// 列出已下载的本地模型
#[tauri::command]
pub fn list_downloaded_models() -> Vec<LocalModelInfo> {
    let models_root = downloader::models_dir();
    if !models_root.exists() {
        return vec![];
    }

    let catalog = catalog::get_available_models();
    let mut result = vec![];

    for model in &catalog {
        let model_path = downloader::model_dir(&model.id);
        if !model_path.exists() {
            continue;
        }

        let Some((actual_size, complete)) = catalog_file(model)
            .ok()
            .and_then(|file| listed_weight(&model_path, file))
        else {
            continue;
        };

        result.push(LocalModelInfo {
            id: model.id.clone(),
            name: model.name.clone(),
            model_type: model.model_type.clone(),
            total_size_bytes: actual_size,
            path: model_path.to_string_lossy().to_string(),
            complete,
        });
    }

    result
}

/// 下载模型
#[tauri::command]
pub async fn download_model(
    app: AppHandle,
    model_id: String,
    source: String,
) -> Result<(), String> {
    let catalog = catalog::get_available_models();
    let model = catalog
        .iter()
        .find(|m| m.id == model_id)
        .ok_or_else(|| error_protocol::encode("download_failed", format!("Unknown model: {}", model_id)))?;

    let dest_dir = downloader::model_dir(&model_id);
    let _download_lease = downloader::acquire_model_path(&dest_dir)?;

    let download_source = model
        .sources
        .iter()
        .find(|s| s.source == source)
        // [自訂] S25：來源名稱對不上時，先回落到官方 huggingface.co，而不是清單第一個（hf-mirror.com）
        .or_else(|| model.sources.iter().find(|s| s.source == "HuggingFace"))
        .or_else(|| model.sources.first())
        .ok_or_else(|| error_protocol::encode("download_network", format!("Model {} has no available download source", model_id)))?;

    let file_count = download_source.files.len() as u32;
    for (i, file) in download_source.files.iter().enumerate() {
        downloader::download_file(
            app.clone(),
            &model_id,
            &file.name,
            &file.url,
            file.size_bytes,
            file.sha256.as_deref(),
            &dest_dir,
            (i + 1) as u32,
            file_count,
        )
        .await?;
    }

    // 写入 meta.json
    let meta = serde_json::json!({
        "id": model.id,
        "name": model.name,
        "model_type": model.model_type,
        "source": &source,
        "downloaded_at": chrono::Utc::now().to_rfc3339(),
    });
    let meta_path = dest_dir.join("meta.json");
    std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap_or_default())
        .map_err(|e| error_protocol::encode("download_failed", format!("Failed to write meta.json: {}", e)))?;

    Ok(())
}

/// 删除已下载的模型
#[tauri::command]
pub fn delete_model(model_id: String) -> Result<(), String> {
    let model_path = downloader::checked_model_dir(&model_id)?;
    let _delete_lease = downloader::acquire_model_path(&model_path)?;
    if model_path.exists() {
        std::fs::remove_dir_all(&model_path)
            .map_err(|e| format!("Failed to delete model: {}", e))?;
        log::info!("Deleted model: {}", model_id);
    }
    Ok(())
}

/// 打开模型存储目录
#[tauri::command]
pub fn open_models_folder() -> Result<String, String> {
    let dir = downloader::models_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create directory: {}", e))?;
    let path = dir.to_string_lossy().to_string();
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg(&path).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&path).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(&path).spawn();
    }
    Ok(path)
}

/// 模型存储位置信息（供设置页展示）
#[derive(Debug, Clone, Serialize)]
pub struct ModelsDirInfo {
    /// 当前生效的模型根目录
    pub current: String,
    /// 默认模型根目录
    pub default_dir: String,
    /// 当前是否为用户自定义（非默认）
    pub is_custom: bool,
}

/// 查询当前模型存储位置
#[tauri::command]
pub fn get_models_dir() -> ModelsDirInfo {
    let current = downloader::models_dir();
    let default_dir = downloader::default_models_dir();
    ModelsDirInfo {
        is_custom: current != default_dir,
        current: current.to_string_lossy().to_string(),
        default_dir: default_dir.to_string_lossy().to_string(),
    }
}

/// 递归复制目录/文件（跨盘 rename 失败时的兜底）。
fn copy_path_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    if src.is_dir() {
        std::fs::create_dir_all(dst).map_err(|e| format!("Failed to create directory: {}", e))?;
        for entry in std::fs::read_dir(src).map_err(|e| format!("Failed to read directory: {}", e))? {
            let entry = entry.map_err(|e| format!("Failed to read directory entry: {}", e))?;
            copy_path_recursive(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(src, dst).map_err(|e| format!("Failed to copy file: {}", e))?;
    }
    Ok(())
}

/// 把旧模型根目录下的每个条目移动到新根目录。
/// 优先 rename（同盘瞬时）；跨盘失败则递归复制后删除源。
/// 目标已存在同名条目则跳过（不覆盖新目录已有内容）。
fn move_dir_contents(from: &Path, to: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(from).map_err(|e| format!("Failed to read old directory: {}", e))? {
        let entry = entry.map_err(|e| format!("Failed to read directory entry: {}", e))?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if dst.exists() {
            continue;
        }
        if std::fs::rename(&src, &dst).is_ok() {
            continue;
        }
        // 跨盘：递归复制后删除源
        copy_path_recursive(&src, &dst)?;
        if src.is_dir() {
            let _ = std::fs::remove_dir_all(&src);
        } else {
            let _ = std::fs::remove_file(&src);
        }
    }
    Ok(())
}

/// 设置模型存储位置。
/// - `dir`：新目录（绝对路径）；传 None 或空串 = 恢复默认路径。
/// - `move_existing`：为 true 时把旧目录下已下载的模型一并移动到新目录。
///
/// 会校验新目录可创建且可写；随后更新进程内生效路径并持久化到设置，
/// 使重启后仍生效（main.rs 启动时会读取 `localAsr.modelsDir`）。
#[tauri::command]
pub async fn set_models_dir(
    storage: tauri::State<'_, Storage>,
    dir: Option<String>,
    move_existing: bool,
) -> Result<String, String> {
    let trimmed = dir
        .as_deref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let old_dir = downloader::models_dir();
    let new_dir = match &trimmed {
        Some(d) => std::path::PathBuf::from(d),
        None => downloader::default_models_dir(),
    };

    // 迁移、切换生效路径和持久化必须作为一个整体与下载/删除互斥。
    let _storage_lease = if new_dir != old_dir {
        Some(downloader::acquire_model_storage()?)
    } else {
        None
    };

    // 目标与当前一致：仅确保持久化的设置与之同步，不做迁移。
    if new_dir != old_dir {
        let old_for_task = old_dir.clone();
        let new_for_task = new_dir.clone();
        // fs 操作（可能是跨盘大文件复制）放到阻塞线程池，避免占用异步运行时线程。
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            std::fs::create_dir_all(&new_for_task)
                .map_err(|e| format!("Failed to create directory: {}", e))?;
            // 可写性探测
            let probe = new_for_task.join(".sayit_write_test");
            std::fs::write(&probe, b"ok").map_err(|e| format!("Directory is not writable: {}", e))?;
            let _ = std::fs::remove_file(&probe);
            if move_existing && old_for_task.exists() {
                move_dir_contents(&old_for_task, &new_for_task)?;
            }
            Ok(())
        })
        .await
        .map_err(|e| format!("Model migration task failed: {}", e))??;
    }

    // 更新进程内生效路径：自定义 → Some，恢复默认 → None
    downloader::set_custom_models_dir(trimmed.as_ref().map(|_| new_dir.clone()));

    // 持久化（重启后由 main.rs 读回）。恢复默认时存空串。
    let value = match &trimmed {
        Some(d) => serde_json::json!(d),
        None => serde_json::json!(""),
    };
    storage
        .set("localAsr.modelsDir", &value)
        .map_err(|e| format!("Failed to save setting: {}", e))?;

    Ok(new_dir.to_string_lossy().to_string())
}

/// 打开指定模型的存储目录（不存在则创建）
#[tauri::command]
pub fn open_model_folder(model_id: String) -> Result<String, String> {
    let dir = downloader::checked_model_dir(&model_id)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create directory: {}", e))?;
    let path = dir.to_string_lossy().to_string();
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg(&path).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&path).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(&path).spawn();
    }
    Ok(path)
}

// [自訂] S88：驗證流程以小型檔案測試，無需下載模型或初始化辨識引擎。
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const MODEL_FILE: &str = "registered.gguf";

    struct ModelFixture {
        dir: PathBuf,
        spec: catalog::ModelFile,
        #[cfg(unix)]
        cache: std::sync::Mutex<Option<FileIdentity>>,
    }

    impl ModelFixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("sayit-model-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&dir).unwrap();
            Self {
                dir,
                spec: catalog::ModelFile {
                    name: MODEL_FILE.into(),
                    url: String::new(),
                    size_bytes: 3,
                    sha256: Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into()),
                },
                #[cfg(unix)]
                cache: std::sync::Mutex::new(None),
            }
        }

        fn verify(&self) -> Result<PathBuf, String> {
            verify_local_file(
                &self.dir,
                &self.spec,
                #[cfg(unix)]
                &self.cache,
                downloader::verify_open_file_sha256,
            )
        }
    }

    impl Drop for ModelFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn accepts_catalog_filename_size_and_hash() {
        let fixture = ModelFixture::new();
        let path = fixture.dir.join(MODEL_FILE);
        fs::write(&path, b"abc").unwrap();
        assert_eq!(fixture.verify().unwrap(), path);
    }

    #[test]
    fn rejects_nonmatching_filename_even_on_case_insensitive_filesystems() {
        let fixture = ModelFixture::new();
        fs::write(fixture.dir.join("Registered.gguf"), b"abc").unwrap();
        let error = fixture.verify().unwrap_err();
        assert!(error.starts_with("sayit_error:local_model_invalid:"));
        assert!(error.contains("exact catalog filename"));
    }

    #[test]
    fn rejects_incorrect_size() {
        let fixture = ModelFixture::new();
        fs::write(fixture.dir.join(MODEL_FILE), b"abcd").unwrap();
        assert!(fixture.verify().unwrap_err().contains("wrong size"));
    }

    #[test]
    fn rejects_incorrect_hash_without_deleting_file() {
        let fixture = ModelFixture::new();
        let path = fixture.dir.join(MODEL_FILE);
        fs::write(&path, b"def").unwrap();
        assert!(fixture.verify().unwrap_err().contains("SHA-256 verification failed"));
        assert_eq!(fs::read(&path).unwrap(), b"def");
    }

    #[test]
    fn rejects_unknown_model_id() {
        let result = verified_model_path("unregistered-test-model");
        assert!(result.unwrap_err().starts_with("sayit_error:local_model_missing:"));
    }

    #[test]
    fn ignores_larger_unregistered_gguf() {
        let fixture = ModelFixture::new();
        let path = fixture.dir.join(MODEL_FILE);
        fs::write(&path, b"abc").unwrap();
        fs::write(fixture.dir.join("other.gguf"), b"bigger").unwrap();
        assert_eq!(fixture.verify().unwrap(), path);
    }

    #[test]
    fn rejects_replacement_after_successful_verification() {
        let fixture = ModelFixture::new();
        let path = fixture.dir.join(MODEL_FILE);
        fs::write(&path, b"abc").unwrap();
        assert_eq!(fixture.verify().unwrap(), path);
        fs::rename(&path, fixture.dir.join("original.gguf")).unwrap();
        fs::write(&path, b"def").unwrap();
        assert!(fixture.verify().unwrap_err().contains("SHA-256 verification failed"));
    }

    // [自訂] SC-35：區分缺檔、讀檔、catalog、快取與列表的判定。
    #[test]
    fn missing_weight_has_its_own_code() {
        let fixture = ModelFixture::new();
        let error = fixture.verify().unwrap_err();
        assert!(error.starts_with("sayit_error:local_model_missing:"));
        assert!(error.contains(MODEL_FILE));
    }

    #[test]
    fn read_error_keeps_reason_without_directory_path() {
        let fixture = ModelFixture::new();
        let not_a_directory = fixture.dir.join("file");
        fs::write(&not_a_directory, b"abc").unwrap();
        let error = model_file_path(&not_a_directory, &fixture.spec).unwrap_err();
        assert!(error.starts_with("sayit_error:local_model_read:"));
        assert!(error.contains(MODEL_FILE));
        let reason = fs::read_dir(&not_a_directory).unwrap_err().to_string();
        assert!(error.contains(&reason));
        assert!(!error.contains(fixture.dir.to_str().unwrap()));
    }

    #[test]
    fn hash_read_error_preserves_the_io_reason() {
        let fixture = ModelFixture::new();
        fs::write(fixture.dir.join(MODEL_FILE), b"abc").unwrap();
        let reason = "Failed to read file during SHA-256 verification: Permission denied";
        let error = verify_local_file(
            &fixture.dir,
            &fixture.spec,
            #[cfg(unix)]
            &fixture.cache,
            |_, _| Err(error_protocol::encode("download_permission", reason)),
        ).unwrap_err();
        assert_eq!(error, format!("sayit_error:local_model_read:{MODEL_FILE}: {reason}"));
    }

    #[test]
    fn rejects_any_catalog_source_without_exactly_one_weight() {
        let mut model = catalog::get_available_models().remove(0);
        let mut other_source = model.sources[0].clone();
        other_source.files.push(other_source.files[0].clone());
        model.sources.push(other_source);
        assert!(catalog_file(&model).unwrap_err().starts_with("sayit_error:local_model_invalid:"));
    }

    #[cfg(unix)]
    #[test]
    fn reuses_verified_file_identity_on_cache_hit() {
        let fixture = ModelFixture::new();
        let path = fixture.dir.join(MODEL_FILE);
        fs::write(&path, b"abc").unwrap();
        assert_eq!(fixture.verify().unwrap(), path);
        let cached = verify_local_file(
            &fixture.dir,
            &fixture.spec,
            &fixture.cache,
            |_, _| panic!("cache hit should not rehash the file"),
        );
        assert_eq!(cached.unwrap(), path);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_same_inode_and_size_overwrite_with_restored_mtime() {
        use std::os::unix::fs::MetadataExt;

        let fixture = ModelFixture::new();
        let path = fixture.dir.join(MODEL_FILE);
        fs::write(&path, b"abc").unwrap();
        fixture.verify().unwrap();
        let before = fs::metadata(&path).unwrap();
        let modified = before.modified().unwrap();
        // 等待可區辨的時戳，涵蓋 ctime 精度為一秒的檔案系統。
        std::thread::sleep(std::time::Duration::from_secs(1));
        fs::write(&path, b"xyz").unwrap();
        fs::OpenOptions::new().write(true).open(&path).unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified)).unwrap();
        let after = fs::metadata(&path).unwrap();
        assert_eq!(before.ino(), after.ino());
        assert_eq!(before.len(), after.len());
        assert_eq!((before.mtime(), before.mtime_nsec()), (after.mtime(), after.mtime_nsec()));
        assert!(fixture.verify().unwrap_err().contains("SHA-256 verification failed"));
    }

    #[test]
    fn listed_weight_marks_complete_by_size_without_reading_hash() {
        let fixture = ModelFixture::new();
        let path = fixture.dir.join(MODEL_FILE);
        fs::write(&path, b"ab").unwrap();
        assert_eq!(listed_weight(&fixture.dir, &fixture.spec), Some((2, false)));
        fs::write(&path, b"xyz").unwrap();
        assert_eq!(listed_weight(&fixture.dir, &fixture.spec), Some((3, true)));
        assert!(fixture.verify().is_err());
    }
}
