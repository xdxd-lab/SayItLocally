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

fn resolve_models_path(path: &Path, allow_unresolved: bool) -> std::io::Result<(PathBuf, bool)> {
    use std::io::{Error, ErrorKind};
    use std::path::Component;

    if !path.is_absolute() {
        return Err(Error::new(ErrorKind::InvalidInput, "Choose an absolute directory path"));
    }

    let mut existing = PathBuf::new();
    let mut missing = PathBuf::new();
    let mut unresolved = false;
    for part in path.components() {
        match part {
            Component::ParentDir => {
                if !missing.pop() {
                    existing.pop();
                }
            }
            Component::Normal(name) if missing.as_os_str().is_empty() => {
                let next = existing.join(name);
                let resolved = match next.symlink_metadata() {
                    Err(error) if error.kind() == ErrorKind::NotFound => {
                        missing.push(name);
                        continue;
                    }
                    metadata => metadata.and_then(|_| {
                        let resolved = next.canonicalize()?;
                        if !resolved.metadata()?.is_dir() {
                            return Err(Error::new(ErrorKind::NotADirectory, "An existing path is not a directory"));
                        }
                        Ok(resolved)
                    }),
                };
                match resolved {
                    Ok(path) => existing = path,
                    Err(_) if allow_unresolved => {
                        unresolved = true;
                        missing.push(name);
                    }
                    Err(error) => return Err(error),
                }
            }
            Component::Normal(name) => missing.push(name),
            Component::RootDir => {
                existing.push(part);
                match existing.canonicalize() {
                    Ok(root) => existing = root,
                    Err(_) if allow_unresolved => unresolved = true,
                    Err(error) => return Err(error),
                }
            }
            _ => existing.push(part),
        }
    }

    if !missing.as_os_str().is_empty() {
        existing.push(missing);
    }
    Ok((existing, unresolved))
}

fn model_dir_path_error(error: std::io::Error) -> String {
    use std::io::ErrorKind;
    let code = match error.kind() {
        ErrorKind::InvalidInput => "model_dir_invalid",
        ErrorKind::PermissionDenied => "model_dir_permission",
        _ => "model_dir_not_directory",
    };
    error_protocol::encode(code, error.to_string())
}

fn lexical_models_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut normalized = PathBuf::new();
    for part in path.components() {
        if part == Component::ParentDir {
            normalized.pop();
        } else {
            normalized.push(part);
        }
    }
    normalized
}

#[cfg(target_os = "macos")]
fn case_insensitive_path(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let existing = path.ancestors().find(|part| part.is_dir()).unwrap_or(Path::new("/"));
    let Ok(name) = std::ffi::CString::new(existing.as_os_str().as_bytes()) else {
        return true;
    };
    // SAFETY: name points to a NUL-terminated absolute path for the duration of the call.
    unsafe { libc::pathconf(name.as_ptr(), libc::_PC_CASE_SENSITIVE) != 1 }
}

#[cfg(not(target_os = "macos"))]
fn case_insensitive_path(_: &Path) -> bool {
    cfg!(windows)
}

fn lexical_path_starts_with(path: &Path, root: &Path, insensitive: bool) -> bool {
    path.starts_with(root) || (insensitive && path.components().count() >= root.components().count()
        && path.components().zip(root.components()).all(|(actual, expected)| {
            actual == expected || actual.as_os_str().to_str().zip(expected.as_os_str().to_str())
                .is_some_and(|(a, b)| a.to_lowercase() == b.to_lowercase())
        }))
}

fn models_path_starts_with(path: &Path, root: &Path, insensitive: bool) -> bool {
    if lexical_path_starts_with(path, root, insensitive) {
        return true;
    }

    // A macOS firmlink can put equivalent directory entries at different
    // depths. Compare the suffixes after matching actual directory identities.
    #[cfg(unix)]
    for root_ancestor in root.ancestors() {
        if !root_ancestor.is_dir() {
            continue;
        }
        let root_suffix = root.strip_prefix(root_ancestor).expect("root ancestor");
        for ancestor in path.ancestors() {
            if same_directory(ancestor, root_ancestor)
                && lexical_path_starts_with(
                    path.strip_prefix(ancestor).expect("path ancestor"), root_suffix, insensitive,
                )
            {
                return true;
            }
        }
    }
    false
}

#[cfg(unix)]
fn same_directory(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.metadata().ok().zip(b.metadata().ok()).is_some_and(|(a, b)| {
        a.is_dir() && b.is_dir() && (a.dev(), a.ino()) == (b.dev(), b.ino())
    })
}

fn same_models_path(a: &Path, b: &Path, insensitive: bool) -> bool {
    models_path_starts_with(a, b, insensitive) && models_path_starts_with(b, a, insensitive)
}

fn probe_models_dir(dir: &Path, name: &str) -> std::io::Result<()> {
    let path = dir.join(name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_DELETE_ON_CLOSE removes the file owned by this handle.
        options.share_mode(0).custom_flags(0x0400_0000);
    }
    use std::io::Write;
    let mut file = options.open(&path)?;
    let write = file.write_all(b"ok");
    #[cfg(windows)]
    drop(file);
    #[cfg(not(windows))]
    {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let created = file.metadata()?;
            let entry = path.symlink_metadata()?;
            if (created.dev(), created.ino()) != (entry.dev(), entry.ino()) {
                return Err(std::io::Error::other("The write probe was replaced"));
            }
        }
        std::fs::remove_file(path)?;
    }
    write
}

fn switch_models_dir(
    requested: Option<&Path>,
    current: &Path,
    default: &Path,
    persist: impl FnOnce(Option<&Path>) -> Result<(), String>,
    activate: impl FnOnce(Option<PathBuf>),
) -> Result<PathBuf, String> {
    let requested_path = requested.unwrap_or(default);
    let (target, _) = resolve_models_path(requested_path, false).map_err(model_dir_path_error)?;
    let mut custom = requested.map(|_| target.as_path());
    if requested.is_some() {
        let insensitive = case_insensitive_path(&target);
        for (root, is_default) in [(current, false), (default, true)] {
            // Older settings allowed relative paths. Use their original base
            // when available, but a broken old setting must not block a switch.
            let absolute = if root.is_absolute() {
                root.to_path_buf()
            } else if let Ok(cwd) = std::env::current_dir() {
                cwd.join(root)
            } else {
                continue;
            };
            let (reference, unresolved) =
                resolve_models_path(&absolute, true).map_err(model_dir_path_error)?;
            if !unresolved && same_models_path(&target, &reference, insensitive) {
                if !is_default {
                    return Ok(current.to_path_buf());
                }
                custom = None;
                continue;
            }
            let overlaps = |path: &Path, root: &Path| {
                models_path_starts_with(path, root, insensitive)
                    || models_path_starts_with(root, path, insensitive)
            };
            if overlaps(&target, &reference)
                || (unresolved && overlaps(&lexical_models_path(requested_path), &lexical_models_path(&absolute)))
            {
                return Err(error_protocol::encode("model_dir_overlap", "Choose a separate model directory"));
            }
        }
        if let Some(app_dir) = default.parent() {
            let (app_dir, _) = resolve_models_path(app_dir, true).map_err(model_dir_path_error)?;
            if same_models_path(&target, &app_dir, insensitive) {
                return Err(error_protocol::encode("model_dir_overlap", "The app data folder cannot be used as the model directory"));
            }
        }
    }

    let probe_name = format!(".sayit_write_test-{}", uuid::Uuid::new_v4());
    std::fs::create_dir_all(&target)
        .and_then(|()| probe_models_dir(&target, &probe_name))
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                model_dir_path_error(error)
            } else {
                error_protocol::encode("model_dir_unwritable", error.to_string())
            }
        })?;

    persist(custom)?;
    activate(custom.map(Path::to_path_buf));
    Ok(custom.unwrap_or(default).to_path_buf())
}

#[derive(serde::Deserialize)]
struct ModelsDirRequest {
    dir: Option<String>,
}

impl ModelsDirRequest {
    fn path(&self) -> Option<&Path> {
        self.dir.as_deref().map(str::trim).filter(|value| !value.is_empty()).map(Path::new)
    }
}

fn apply_models_dir_request(
    request: ModelsDirRequest,
    storage: &Storage,
    current: impl FnOnce() -> PathBuf,
    default: &Path,
    activate: impl FnOnce(Option<PathBuf>),
) -> Result<PathBuf, String> {
    let _storage_lease = downloader::acquire_model_storage()?;
    switch_models_dir(request.path(), &current(), default, |custom| {
        storage.set("localAsr.modelsDir", &serde_json::json!(custom.unwrap_or(Path::new(""))))
            .map_err(|error| error_protocol::encode("model_dir_save", error.to_string()))
    }, activate)
}

/// Change the effective directory only after the new setting has been saved.
/// Older callers can still send moveExisting. It is ignored.
#[tauri::command]
pub async fn set_models_dir(
    app: AppHandle,
    request: tauri::ipc::Request<'_>,
) -> Result<String, String> {
    use tauri::Manager;
    let tauri::ipc::InvokeBody::Json(payload) = request.body() else {
        return Err(error_protocol::encode("model_dir_invalid", "Expected a JSON directory request"));
    };
    let request = <ModelsDirRequest as serde::Deserialize>::deserialize(payload)
        .map_err(|error| error_protocol::encode("model_dir_invalid", error.to_string()))?;
    tokio::task::spawn_blocking(move || {
        let storage = app.state::<Storage>();
        let result = apply_models_dir_request(
            request, &storage, downloader::models_dir, &downloader::default_models_dir(),
            downloader::set_custom_models_dir,
        )?;
        Ok(result.to_string_lossy().into_owned())
    })
    .await
    .map_err(|error| error_protocol::encode("model_dir_save", error.to_string()))?
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
    fn model_directory_switch_ignores_old_move_flag_and_preserves_all_files() {
        let fixture = ModelFixture::new();
        let current = fixture.dir.join("old");
        let default = fixture.dir.join("app/models");
        let target = fixture.dir.join("new");
        fs::create_dir(&current).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(current.join(MODEL_FILE), b"weights").unwrap();
        fs::write(current.join("notes.txt"), b"keep").unwrap();
        fs::write(target.join("already-here"), b"untouched").unwrap();
        fs::write(target.join(".sayit_write_test"), b"existing probe").unwrap();
        let request: ModelsDirRequest = serde_json::from_value(serde_json::json!({
            "dir": target, "moveExisting": true
        })).unwrap();

        let chosen = switch_models_dir(request.path(), &current, &default, |value| {
            assert_eq!(value, Some(target.canonicalize().unwrap().as_path()));
            Ok(())
        }, |value| assert_eq!(value, Some(target.canonicalize().unwrap()))).unwrap();

        let mut entries: Vec<_> = fs::read_dir(&current).unwrap()
            .map(|entry| entry.unwrap().file_name()).collect();
        entries.sort();
        assert_eq!(entries, ["notes.txt", MODEL_FILE]);
        assert_eq!(fs::read(current.join(MODEL_FILE)).unwrap(), b"weights");
        assert_eq!(fs::read(current.join("notes.txt")).unwrap(), b"keep");
        assert_eq!(fs::read_dir(&target).unwrap().count(), 2);
        assert_eq!(fs::read(target.join(".sayit_write_test")).unwrap(), b"existing probe");
        assert_eq!(fs::read(target.join("already-here")).unwrap(), b"untouched");
        assert!(!chosen.as_os_str().to_string_lossy().ends_with(std::path::MAIN_SEPARATOR));
        assert_eq!(chosen, target.canonicalize().unwrap());
    }

    #[test]
    fn model_directory_rejects_overlaps_and_invalid_paths_before_writing() {
        let fixture = ModelFixture::new();
        let current = fixture.dir.join("Current");
        let app = fixture.dir.join("app");
        let default = app.join("Models");
        let file = fixture.dir.join("file");
        fs::create_dir(&current).unwrap();
        fs::create_dir_all(&default).unwrap();
        fs::write(&file, b"keep").unwrap();

        let select = |path: &Path| switch_models_dir(
            Some(path), &current, &default,
            |_| panic!("invalid choice changed the setting"),
            |_| panic!("invalid choice changed the effective path"),
        );
        for target in [
            fixture.dir.clone(), app, current.join("not-created/deeper"),
            default.join("not-created/deeper"), current.join("../app/Models/not-created"),
        ] {
            assert!(select(&target).unwrap_err().starts_with("sayit_error:model_dir_overlap:"), "{target:?}");
        }
        assert!(select(Path::new("relative")).unwrap_err().starts_with("sayit_error:model_dir_invalid:"));
        for target in [file.clone(), file.join("nested")] {
            assert!(select(&target).unwrap_err().starts_with("sayit_error:model_dir_not_directory:"), "{target:?}");
        }
        if current.with_file_name("current").is_dir() {
            let alias = current.with_file_name("current");
            assert_eq!(select(&alias).unwrap(), current);
            assert!(select(&alias.join("not-created")).unwrap_err().starts_with("sayit_error:model_dir_overlap:"));
        }
        assert!(models_path_starts_with(Path::new("/App/Models/New"), Path::new("/app/models"), true));
        assert!(!models_path_starts_with(Path::new("/App/Models2/New"), Path::new("/app/models"), true));
        assert_eq!(fs::read_dir(&current).unwrap().count(), 0);
        assert_eq!(fs::read_dir(&default).unwrap().count(), 0);
        assert_eq!(fs::read(file).unwrap(), b"keep");

        let separate = fixture.dir.join("new/nested");
        assert!(switch_models_dir(Some(&separate), &current, &default, |_| Ok(()), |_| {}).is_ok());
        assert!(separate.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn model_directory_follows_aliases_and_parent_components_in_filesystem_order() {
        let fixture = ModelFixture::new();
        let current = fixture.dir.join("original/models");
        let default = fixture.dir.join("default");
        let alias = fixture.dir.join("alias");
        fs::create_dir_all(&current).unwrap();
        std::os::unix::fs::symlink(&current, &alias).unwrap();
        let choose = |path: &Path| switch_models_dir(
            Some(path), &current, &default,
            |_| panic!("alias must not change the setting"),
            |_| panic!("alias must not change the effective path"),
        );
        assert_eq!(choose(&alias).unwrap(), current);
        for path in [alias.join("child"), alias.join("..")] {
            assert!(choose(&path).unwrap_err().starts_with("sayit_error:model_dir_overlap:"));
        }
        assert_eq!(choose(&current.join("missing/..")).unwrap(), current);
        assert!(!current.join("missing").exists());
    }

    #[cfg(windows)]
    #[test]
    fn drive_root_uses_the_same_prefix_as_its_resolved_children() {
        let fixture = ModelFixture::new();
        let root: PathBuf = fixture.dir.components().take(2).collect();
        let resolved = resolve_models_path(&fixture.dir, false).unwrap().0;
        let resolved_root = resolve_models_path(&root, false).unwrap().0;
        assert!(models_path_starts_with(&resolved, &resolved_root, true));
    }

    #[test]
    fn model_directory_restores_default_despite_legacy_overlap() {
        let fixture = ModelFixture::new();
        let default = fixture.dir.join("app/models");
        let current = default.join("nested");
        fs::create_dir_all(&current).unwrap();
        fs::write(current.join(MODEL_FILE), b"weights").unwrap();
        fs::write(current.join("notes.txt"), b"keep").unwrap();
        let result = switch_models_dir(None, &current, &default, |value| {
            assert_eq!(value, None);
            Ok(())
        }, |value| assert_eq!(value, None)).unwrap();
        assert_eq!(result, default);
        assert_eq!(fs::read_dir(&default).unwrap().count(), 1);
        assert_eq!(fs::read(current.join(MODEL_FILE)).unwrap(), b"weights");
        assert_eq!(fs::read(current.join("notes.txt")).unwrap(), b"keep");
    }

    #[test]
    fn model_directory_probe_leaves_existing_files_and_excludes_collisions() {
        let fixture = ModelFixture::new();
        let dir = &fixture.dir;
        fs::write(dir.join(".sayit_write_test"), b"legacy").unwrap();
        let expected_name = ".sayit_write_test-collision";
        fs::write(dir.join(expected_name), b"keep").unwrap();
        assert_eq!(probe_models_dir(dir, expected_name).unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
        probe_models_dir(dir, ".sayit_write_test-current").unwrap();
        assert_eq!(fs::read(dir.join(".sayit_write_test")).unwrap(), b"legacy");
        assert_eq!(fs::read(dir.join(expected_name)).unwrap(), b"keep");
        assert_eq!(fs::read_dir(dir).unwrap().count(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn model_directory_probe_never_follows_existing_links() {
        let fixture = ModelFixture::new();
        let dir = &fixture.dir;
        let referent = dir.join("referent");
        fs::write(&referent, b"keep").unwrap();
        for (name, target) in [
            (".sayit_write_test", referent.clone()),
            (".sayit_write_test-linked", referent.clone()),
            (".sayit_write_test-dangling", dir.join("missing-target")),
        ] {
            let link = dir.join(name);
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(probe_models_dir(dir, name).is_err());
            assert_eq!(fs::read_link(link).unwrap(), target);
        }
        assert_eq!(fs::read(referent).unwrap(), b"keep");
        assert!(!dir.join("missing-target").exists());
    }

    #[cfg(unix)]
    #[test]
    fn model_directory_readonly_destination_leaves_setting_untouched() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = ModelFixture::new();
        let target = fixture.dir.join("readonly");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o500)).unwrap();
        let result = switch_models_dir(
            Some(&target), &fixture.dir.join("old"), &fixture.dir.join("default"),
            |_| panic!("read-only directory changed the setting"),
            |_| panic!("read-only directory changed the effective path"),
        );
        fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.unwrap_err().starts_with("sayit_error:model_dir_permission:"));
        assert_eq!(fs::read_dir(target).unwrap().count(), 0);
    }

    #[test]
    fn model_directory_checks_case_before_default_root_exists() {
        let fixture = ModelFixture::new();
        let current = fixture.dir.join("existing");
        let app = fixture.dir.join("CaseProbe");
        fs::create_dir(&app).unwrap();
        let default = app.join("models");
        let target = app.join("Models/child");
        // Detect the test volume independently of the implementation under test.
        let insensitive = fixture.dir.join("caseprobe").is_dir();
        let result = switch_models_dir(Some(&target), &current, &default, |_| Ok(()), |_| {});
        if insensitive {
            assert!(result.unwrap_err().starts_with("sayit_error:model_dir_overlap:"));
            assert!(!default.exists());
        } else {
            assert!(result.is_ok());
            assert!(target.is_dir());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn model_directory_checks_data_volume_aliases_at_different_depths() {
        let fixture = ModelFixture::new();
        let ordinary = fixture.dir.canonicalize().unwrap();
        let data = Path::new("/System/Volumes/Data").join(ordinary.strip_prefix("/").unwrap());
        if !same_directory(&ordinary, &data) {
            return; // The test volume does not have the macOS Data alias.
        }
        let alias = resolve_models_path(&data, false).unwrap().0;
        let current = ordinary.join("models");
        let default = ordinary.join("default");
        assert!(same_models_path(&ordinary, &alias, false));
        for target in [alias, data.join("models/not-created")] {
            let error = switch_models_dir(Some(&target), &current, &default,
                |_| panic!("alias overlaps the old directory"),
                |_| panic!("alias overlaps the old directory"),
            ).unwrap_err();
            assert!(error.starts_with("sayit_error:model_dir_overlap:"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn model_directory_recovers_from_broken_references_and_classifies_errors() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let fixture = ModelFixture::new();
        let base = &fixture.dir;
        let parent = base.join("parent");
        let alias = base.join("alias");
        fs::create_dir(&parent).unwrap();
        symlink(&parent, &alias).unwrap();
        let dangling = parent.join("dangling");
        let file = parent.join("file");
        let denied = parent.join("denied");
        let missing = parent.join("missing/models");
        symlink(parent.join("unmounted"), &dangling).unwrap();
        fs::write(&file, b"keep").unwrap();
        fs::create_dir(&denied).unwrap();
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o000)).unwrap();

        for (i, broken) in [&dangling, &file, &denied.join("models"), &missing].into_iter().enumerate() {
            let target = base.join(format!("new-{i}"));
            let original_path = alias.join(broken.strip_prefix(&parent).unwrap());
            // Missing, inaccessible, and invalid old paths all allow unrelated choices.
            assert!(switch_models_dir(Some(&target), &original_path, &base.join("default"), |_| Ok(()), |_| {}).is_ok());
            assert!(switch_models_dir(Some(&target), &base.join("old"), &original_path, |_| Ok(()), |_| {}).is_ok());
            let overlap = switch_models_dir(Some(&parent), &original_path, &base.join("default"),
                |_| panic!("must check the resolved ancestor"), |_| panic!("must check the resolved ancestor"));
            assert!(overlap.unwrap_err().starts_with("sayit_error:model_dir_overlap:"));
            assert!(switch_models_dir(None, &original_path, &base.join("default"), |_| Ok(()), |_| {}).is_ok());
        }
        for (path, code) in [
            (&dangling, "model_dir_not_directory"),
            (&file, "model_dir_not_directory"),
            (&denied.join("models"), "model_dir_permission"),
        ] {
            for requested in [None, Some(path.as_path())] {
                let result = switch_models_dir(requested, &base.join("old"), path,
                    |_| panic!("broken target must not be persisted"),
                    |_| panic!("broken target must not be activated"));
                assert!(result.unwrap_err().starts_with(&format!("sayit_error:{code}:")));
            }
        }
        fs::set_permissions(denied, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn model_directory_request_respects_download_lease_before_reading_current_path() {
        let _guard = downloader::TEST_MODEL_LEASES.lock().unwrap();
        let fixture = ModelFixture::new();
        let storage = Storage::new(fixture.dir.join("test.db")).unwrap();
        let target = fixture.dir.join("new");
        let _download = downloader::acquire_model_path(&fixture.dir.join("download")).unwrap();
        let result = apply_models_dir_request(
            ModelsDirRequest { dir: Some(target.to_string_lossy().into_owned()) },
            &storage, || panic!("blocked requests do not read the current path"),
            &fixture.dir.join("default"), |_| panic!("blocked requests do not activate"),
        );
        assert!(result.unwrap_err().starts_with("sayit_error:download_busy:"));
        assert!(!target.exists());
    }

    #[test]
    fn model_directory_sqlite_failure_leaves_saved_and_effective_values_untouched() {
        let _guard = downloader::TEST_MODEL_LEASES.lock().unwrap();
        let fixture = ModelFixture::new();
        let db_path = fixture.dir.join("test.db");
        let storage = Storage::new(db_path.clone()).unwrap();
        let current = fixture.dir.join("old");
        let stored = serde_json::json!(current);
        storage.set("localAsr.modelsDir", &stored).unwrap();
        let db = rusqlite::Connection::open(db_path).unwrap();
        db.execute_batch("CREATE TRIGGER prevent_model_change BEFORE UPDATE ON app_settings
            WHEN NEW.key = 'localAsr.modelsDir' BEGIN SELECT RAISE(ABORT, 'test'); END;").unwrap();

        let target = fixture.dir.join("new");
        let effective = std::cell::RefCell::new(current.clone());
        let result = apply_models_dir_request(
            ModelsDirRequest { dir: Some(target.to_string_lossy().into_owned()) },
            &storage, || current.clone(), &fixture.dir.join("default"),
            |value| { *effective.borrow_mut() = value.unwrap(); },
        );
        let error = result.unwrap_err();
        assert!(error.starts_with("sayit_error:model_dir_save:"));
        assert!(error.contains("test"));
        assert_eq!(storage.get("localAsr.modelsDir", None), stored);
        assert_eq!(*effective.borrow(), current);
        assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
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
