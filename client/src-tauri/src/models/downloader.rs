// 模型下载器 — 支持断点续传和进度事件

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use tauri::{AppHandle, Emitter};

use crate::error_protocol;

fn download_error(code: &str, detail: impl AsRef<str>) -> String {
    error_protocol::encode(code, detail)
}

fn download_io_error(context: &str, error: std::io::Error) -> String {
    let code = if error.kind() == std::io::ErrorKind::PermissionDenied {
        "download_permission"
    } else if matches!(error.raw_os_error(), Some(28 | 112)) {
        // ENOSPC on Unix and ERROR_DISK_FULL on Windows.
        "download_no_space"
    } else {
        "download_failed"
    };
    download_error(code, format!("{}: {}", context, error))
}

const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const READ_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const PARALLEL_STATE_VERSION: u32 = 1;

#[derive(Default)]
struct DownloadActivity {
    model_paths: HashSet<PathBuf>,
    storage_mutating: bool,
}

static DOWNLOAD_ACTIVITY: Lazy<Mutex<DownloadActivity>> =
    Lazy::new(|| Mutex::new(DownloadActivity::default()));

/// 同一路径只能有一个下载或删除任务；存储目录迁移期间也不接受新下载。
pub struct ModelPathLease {
    path: PathBuf,
}

impl Drop for ModelPathLease {
    fn drop(&mut self) {
        let mut activity = DOWNLOAD_ACTIVITY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        activity.model_paths.remove(&self.path);
    }
}

pub fn acquire_model_path(path: &Path) -> Result<ModelPathLease, String> {
    let key = path.to_path_buf();
    let mut activity = DOWNLOAD_ACTIVITY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if activity.storage_mutating || !activity.model_paths.insert(key.clone()) {
        return Err(download_error(
            "download_busy",
            "The model is already being downloaded or the model directory is being changed",
        ));
    }
    Ok(ModelPathLease { path: key })
}

/// 目录迁移必须与所有模型下载、删除互斥，避免跨盘复制正在写入的临时文件。
pub struct ModelStorageLease;

impl Drop for ModelStorageLease {
    fn drop(&mut self) {
        let mut activity = DOWNLOAD_ACTIVITY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        activity.storage_mutating = false;
    }
}

pub fn acquire_model_storage() -> Result<ModelStorageLease, String> {
    let mut activity = DOWNLOAD_ACTIVITY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if activity.storage_mutating || !activity.model_paths.is_empty() {
        return Err(download_error(
            "download_busy",
            "Wait for active model downloads to finish before changing the model directory",
        ));
    }
    activity.storage_mutating = true;
    Ok(ModelStorageLease)
}

/// 精确判断错误是否为 checksum 不匹配（Fail-Closed 设计）
pub(super) fn is_checksum_error(err_msg: &str) -> bool {
    err_msg.starts_with("sayit_error:download_checksum:")
}

/// 统一安全删除文件（文件不存在视为成功，其他 I/O 或权限错误显式返回）
fn remove_file_if_exists(path: &Path) -> Result<(), String> {
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(download_io_error("Failed to remove temporary/corrupted file", e));
        }
    }
    Ok(())
}

/// 校验落地文件的 SHA-256 哈希值
fn verify_file_sha256(file_path: &Path, expected_sha256: &str) -> Result<(), String> {
    let file = std::fs::File::open(file_path)
        .map_err(|e| download_io_error("Failed to open file for SHA-256 verification", e))?;
    verify_open_file_sha256(&file, expected_sha256)
}

// [自訂] S88：下載與載入共用雜湊檢查；載入端可核對同一個已開啟檔案的身分。
pub(super) fn verify_open_file_sha256(
    mut file: &std::fs::File,
    expected_sha256: &str,
) -> Result<(), String> {
    use std::io::Read;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];

    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|e| download_io_error("Failed to read file during SHA-256 verification", e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    let actual_hash = format!("{:x}", hasher.finalize());
    let expected_lower = expected_sha256.to_ascii_lowercase();

    if actual_hash != expected_lower {
        return Err(download_error(
            "download_checksum",
            format!(
                "SHA-256 verification failed: expected {}, got {}",
                expected_lower, actual_hash
            ),
        ));
    }

    Ok(())
}

/// 校验临时文件；仅明确哈希不匹配时删除，I/O 错误保留现场。
fn verify_temp_file_sha256(file_path: &Path, expected_sha256: &str) -> Result<(), String> {
    match verify_file_sha256(file_path, expected_sha256) {
        Ok(()) => Ok(()),
        Err(e) if is_checksum_error(&e) => {
            remove_file_if_exists(file_path)?;
            Err(e)
        }
        Err(e) => Err(e),
    }
}

// [自訂] SC-36 / SC-38：優先取精確拼法，其次只接受 ASCII 大小寫差異。
fn catalog_directory_entry(dest_path: &Path) -> Result<Option<PathBuf>, String> {
    let invalid_path = || download_error("download_failed", "Missing catalog filename");
    let expected = dest_path.file_name().ok_or_else(invalid_path)?;
    let parent = dest_path.parent().ok_or_else(invalid_path)?;
    let entries = std::fs::read_dir(parent)
        .map_err(|e| download_io_error("Failed to read model directory", e))?;
    let mut existing = None;
    for entry in entries {
        let entry = entry.map_err(|e| download_io_error("Failed to read model directory entry", e))?;
        let name = entry.file_name();
        if name == expected {
            return Ok(Some(entry.path()));
        }
        if name.eq_ignore_ascii_case(expected) {
            existing = Some(entry.path());
        }
    }
    Ok(existing)
}

// [自訂] SC-38：檔案系統可能將 Unicode 特殊字元視作同名，但我們只校正 ASCII。
// 在覆寫前就拒絕這類目錄項，避免動到原檔。
fn validate_download_destination(dest_path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(dest_path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(download_io_error("Failed to inspect model destination", e)),
        Ok(_) => {}
    }
    if std::fs::metadata(dest_path).is_ok_and(|info| info.is_dir()) {
        return Err(download_error("download_failed", "Model destination is a directory"));
    }
    if catalog_directory_entry(dest_path)?.is_none() {
        return Err(download_error("download_failed", "Unsupported model filename spelling"));
    }
    Ok(())
}

// [自訂] SC-36：不分大小寫的 APFS 替換檔案時可能保留舊的目錄項名稱。
fn restore_catalog_filename(dest_path: &Path) -> Result<(), String> {
    let existing = catalog_directory_entry(dest_path)?.ok_or_else(|| {
        download_error("download_failed", "Could not find the verified model in its directory")
    })?;
    if existing.file_name() == dest_path.file_name() {
        return Ok(());
    }
    let parent = existing.parent().unwrap_or_else(|| std::path::Path::new("."));

    // 以獨立的同目錄暫存名稱中轉，避免覆寫續傳檔，也保留相對符號連結。
    let scratch_name = format!(".sayit-model-{}.tmp", uuid::Uuid::new_v4());
    let scratch = parent.join(&scratch_name);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&scratch)
        .map_err(|e| download_io_error("Failed to reserve a model filename for renaming", e))?;
    if let Err(e) = std::fs::rename(&existing, &scratch) {
        // [自訂] SC-38：只移除我們預留的空白檔；保留可能已有資料的暫存檔。
        remove_empty_rename_placeholder(&scratch);
        return Err(download_io_error("Failed to stage verified model for renaming", e));
    }
    if let Err(e) = std::fs::rename(&scratch, dest_path) {
        let error = download_io_error("Failed to apply the catalog filename", e);
        if std::fs::rename(&scratch, &existing).is_err() {
            return Err(format!("{error}. The verified file is still saved as {scratch_name}."));
        }
        return Err(error);
    }
    Ok(())
}

fn remove_empty_rename_placeholder(path: &Path) {
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.len() == 0) {
        let _ = std::fs::remove_file(path);
    }
}

// [自訂] SC-35：所有完成路徑都核對暫存檔本身，通過後才以同目錄 rename 替換。
fn finalize_download(
    temp_path: &Path,
    dest_path: &Path,
    expected_size: u64,
    expected_sha256: &str,
    size_error_code: &str,
) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(temp_path)
        .map_err(|e| download_io_error("Failed to inspect temporary download", e))?;
    if !metadata.is_file() || metadata.len() != expected_size {
        return Err(download_error(
            size_error_code,
            format!(
                "Expected a temporary file of {} bytes, found {} bytes",
                expected_size, metadata.len()
            ),
        ));
    }
    verify_temp_file_sha256(temp_path, expected_sha256)?;
    validate_download_destination(dest_path)?;
    std::fs::rename(temp_path, dest_path)
        .map_err(|e| download_io_error("Failed to finalize downloaded file", e))?;
    restore_catalog_filename(dest_path)
}

// [自訂] SC-38：中斷的檔名校正只救回大小及雜湊都符合的暫存權重。
fn recover_staged_weight(
    dest_path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<bool, String> {
    let dir = dest_path.parent().unwrap_or_else(|| Path::new("."));
    let entries = std::fs::read_dir(dir)
        .map_err(|e| download_io_error("Failed to check for staged model files", e))?;
    for entry in entries {
        let entry = entry.map_err(|e| download_io_error("Failed to inspect staged model files", e))?;
        let name = entry.file_name();
        let staged = name.to_str()
            .and_then(|name| name.strip_prefix(".sayit-model-"))
            .and_then(|name| name.strip_suffix(".tmp"))
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok());
        if !staged {
            continue;
        }
        let path = entry.path();
        if !std::fs::symlink_metadata(&path).is_ok_and(|metadata| {
            metadata.is_file() && metadata.len() == expected_size && !artifact_has_multiple_links(&metadata)
        })
            || verify_file_sha256(&path, expected_sha256).is_err()
        {
            continue;
        }
        validate_download_destination(dest_path)?;
        std::fs::rename(&path, dest_path)
            .map_err(|e| download_io_error("Failed to recover verified model file", e))?;
        restore_catalog_filename(dest_path)?;
        return Ok(true);
    }
    Ok(false)
}

// [自訂] SC-36：大小與雜湊都相符時，直接校正檔名並沿用現有權重。
fn reuse_verified_download(
    dest_path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<bool, String> {
    let metadata = match std::fs::metadata(dest_path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(download_io_error("Failed to read existing model metadata", e)),
    };
    if metadata.is_file() && metadata.len() == expected_size {
        match verify_file_sha256(dest_path, expected_sha256) {
            Ok(()) => {
                restore_catalog_filename(dest_path)?;
                return Ok(true);
            }
            Err(e) if is_checksum_error(&e) => {}
            Err(e) => return Err(e),
        }
    }
    log::warn!(
        "Existing model file {:?} failed size or SHA-256 validation; redownloading",
        dest_path.file_name().unwrap_or_default()
    );
    Ok(false)
}

// [自訂] REV9：下載暫存與中斷後的權重都不得指向其它檔案。
fn artifact_has_multiple_links(_metadata: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        _metadata.nlink() > 1
    }
    #[cfg(not(unix))]
    {
        false
    }
}

// [自訂] SC-35：避免舊暫存檔透過連結指向既有權重，導致寫入暫存時改到原檔。
fn check_download_artifact(path: &Path, existing_target: Option<&Path>) -> Result<(), String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(download_io_error("Failed to inspect download artifact", e)),
    };
    if artifact_has_multiple_links(&metadata) {
        return Err(download_error(
            "download_failed",
            "Download artifact has multiple links",
        ));
    }
    if !metadata.is_file() || existing_target.is_some_and(|target| target == path) {
        return Err(download_error(
            "download_failed",
            "Download artifact is not a separate regular file",
        ));
    }
    Ok(())
}


#[derive(Debug, Clone, Serialize)]
pub struct DownloadProgress {
    pub model_id: String,
    pub file_name: String,
    /// 当前文件已下载字节
    pub downloaded_bytes: u64,
    /// 当前文件总字节（从 Content-Length 获取，0 表示未知）
    pub total_bytes: u64,
    /// 整体进度百分比（跨所有文件）
    pub percent: f64,
    /// 当前文件索引（从 1 开始）
    pub file_index: u32,
    /// 总文件数
    pub file_count: u32,
    pub status: String,
    pub error: Option<String>,
}

fn emit_progress(
    report: &(dyn Fn(DownloadProgress) + Sync),
    model_id: &str,
    file_name: &str,
    downloaded: u64,
    total: u64,
    status: &str,
    error: Option<&str>,
    file_index: u32,
    file_count: u32,
) {
    let percent = if total > 0 {
        (downloaded as f64 / total as f64 * 100.0).min(100.0)
    } else {
        0.0
    };
    report(DownloadProgress {
            model_id: model_id.into(),
            file_name: file_name.into(),
            downloaded_bytes: downloaded,
            total_bytes: total,
            percent,
            file_index,
            file_count,
            status: status.into(),
            error: error.map(Into::into),
    });
}

/// 用户自定义的模型存储根目录（进程级）。None = 用默认路径。
/// 启动时由 main.rs 从设置 `localAsr.modelsDir` 灌入；用户在设置里更改时同步更新。
/// 所有取模型路径的地方都走 `models_dir()`，改这一处即全链路生效。
static CUSTOM_MODELS_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);

/// 默认模型存储根目录（未自定义时使用）。
pub fn default_models_dir() -> PathBuf {
    crate::commands::app_data_dir()
        .join("models")
}

/// 设置/清除自定义模型根目录。传 None 恢复默认。
pub fn set_custom_models_dir(dir: Option<PathBuf>) {
    if let Ok(mut guard) = CUSTOM_MODELS_DIR.write() {
        *guard = dir;
    }
}

/// 获取模型存储根目录：优先自定义路径，否则默认路径。
pub fn models_dir() -> PathBuf {
    if let Ok(guard) = CUSTOM_MODELS_DIR.read() {
        if let Some(ref dir) = *guard {
            return dir.clone();
        }
    }
    default_models_dir()
}

/// 获取指定模型的目录
pub fn model_dir(model_id: &str) -> PathBuf {
    models_dir().join(model_id)
}

/// 模型 id 会直接拼进路径。来自前端的 id 必须先过这一关：
/// `..\..` 能跳出模型目录，绝对路径会让 join 把前缀整个换掉 —— 而 delete_model 拿它去
/// `remove_dir_all`，传一个 `C:\Users\<name>` 就是删掉整个用户目录。
/// 目录里的 id 都是字母、数字、`-`、`.`、`_` 组成的短串。
pub fn is_valid_model_id(model_id: &str) -> bool {
    !model_id.is_empty()
        && model_id.len() <= 96
        && !model_id.starts_with('.')
        && !model_id.contains("..")
        && model_id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// 校验过的 model_dir：id 不合规就报错，不碰磁盘。
pub fn checked_model_dir(model_id: &str) -> Result<PathBuf, String> {
    if is_valid_model_id(model_id) {
        Ok(model_dir(model_id))
    } else {
        Err(format!("Invalid model id: {}", crate::providers::diag::truncate(model_id, 60)))
    }
}

use futures_util::stream::{FuturesUnordered, StreamExt};
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug, Default, Clone)]
struct RemoteProbe {
    size: u64,
    supports_range: bool,
    strong_etag: Option<String>,
}

fn parse_content_range_header(value: &str) -> Option<(u64, u64, u64)> {
    let (bounds, total) = value.trim().strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = bounds.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}

fn strong_etag(resp: &reqwest::Response) -> Option<String> {
    let value = resp
        .headers()
        .get(reqwest::header::ETAG)?
        .to_str()
        .ok()?
        .trim();
    if value.is_empty() || value.starts_with("W/") {
        None
    } else {
        Some(value.to_string())
    }
}

/// 构建带 User-Agent、连接超时和读取空闲超时的 HTTP 客户端。
/// 不设置整个请求的总时限：慢速下载可以持续很久，但连接或单次读取不能永久挂住。
fn build_http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("SayIt/1.0")
        .tcp_nodelay(true)
        .pool_max_idle_per_host(16)
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_IDLE_TIMEOUT)
        .build()
        .map_err(|e| download_error("download_network", format!("Failed to create HTTP client: {}", e)))
}

fn full_request(client: &reqwest::Client, url: &str) -> reqwest::RequestBuilder {
    client
        .get(url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
}

fn resume_request(
    client: &reqwest::Client,
    url: &str,
    offset: u64,
    strong_etag: Option<&str>,
) -> reqwest::RequestBuilder {
    let request = client
        .get(url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .header(reqwest::header::RANGE, format!("bytes={}-", offset));
    if let Some(etag) = strong_etag {
        request.header(reqwest::header::IF_RANGE, etag)
    } else {
        request
    }
}

fn range_request(
    client: &reqwest::Client,
    url: &str,
    range: String,
    strong_etag: Option<&str>,
) -> reqwest::RequestBuilder {
    let request = client
        .get(url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .header(reqwest::header::RANGE, range);
    if let Some(etag) = strong_etag {
        request.header(reqwest::header::IF_MATCH, etag)
    } else {
        request
    }
}

/// 用真实的 0-0 Range 请求探测能力、总大小和强 ETag。
async fn probe_url(client: &reqwest::Client, url: &str) -> RemoteProbe {
    let resp = match range_request(client, url, "bytes=0-0".to_string(), None)
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(error) => {
            log::warn!("Range probe failed for {}: {}", url, error);
            return RemoteProbe::default();
        }
    };

    let etag = strong_etag(&resp);
    if resp.status().as_u16() == 206 {
        if let Some((0, 0, total)) = resp
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_content_range_header)
        {
            if total > 0 {
                return RemoteProbe {
                    size: total,
                    supports_range: true,
                    strong_etag: etag,
                };
            }
        }
    }

    RemoteProbe {
        size: resp.content_length().unwrap_or(0),
        supports_range: false,
        strong_etag: etag,
    }
}

fn resolve_download_size(expected_size: u64, remote_size: u64) -> Result<u64, String> {
    if expected_size > 0 && remote_size > 0 && expected_size != remote_size {
        return Err(download_error(
            "download_source_mismatch",
            format!(
                "Catalog size is {}, but the server reports {} bytes",
                expected_size, remote_size
            ),
        ));
    }
    Ok(if expected_size > 0 {
        expected_size
    } else {
        remote_size
    })
}

/// 纯函数校验 Range 响应头（支持零依赖单元测试与生产复用）
fn validate_range_response_parts(
    status: u16,
    content_range_str: Option<&str>,
    content_length: Option<u64>,
    expected_start: u64,
    expected_end: u64,
    expected_total: u64,
) -> Result<(), String> {
    if status != 206 {
        let code = if status == 412 {
            "download_source_changed"
        } else if status == 429 || status >= 500 {
            "download_network"
        } else {
            "download_range_invalid"
        };
        return Err(download_error(
            code,
            format!("Expected HTTP 206 for Range request, got HTTP {}", status),
        ));
    }

    let header = content_range_str.ok_or_else(|| {
        download_error(
            "download_range_invalid",
            "Missing Content-Range header in 206 response",
        )
    })?;
    let (start, end, total) = parse_content_range_header(header).ok_or_else(|| {
        download_error(
            "download_range_invalid",
            format!("Invalid Content-Range: {}", header),
        )
    })?;

    if start != expected_start || end != expected_end {
        return Err(download_error(
            "download_range_invalid",
            format!(
                "Content-Range mismatch: expected bytes {}-{}, got bytes {}-{}",
                expected_start, expected_end, start, end
            ),
        ));
    }
    if total != expected_total {
        return Err(download_error(
            "download_source_changed",
            format!(
                "Remote size changed during download: expected {}, got {}",
                expected_total, total
            ),
        ));
    }

    let expected_len = expected_end - expected_start + 1;
    if let Some(content_len) = content_length {
        if content_len != expected_len {
            return Err(download_error(
                "download_range_invalid",
                format!(
                    "Content-Length mismatch: expected {}, got {}",
                    expected_len, content_len
                ),
            ));
        }
    }

    Ok(())
}

fn validate_chunk_response(
    resp: &reqwest::Response,
    expected_start: u64,
    expected_end: u64,
    expected_total: u64,
    expected_etag: Option<&str>,
) -> Result<(), String> {
    let content_range = resp
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok());
    validate_range_response_parts(
        resp.status().as_u16(),
        content_range,
        resp.content_length(),
        expected_start,
        expected_end,
        expected_total,
    )?;

    if let (Some(expected), Some(actual)) = (
        expected_etag,
        resp.headers()
            .get(reqwest::header::ETAG)
            .and_then(|value| value.to_str().ok()),
    ) {
        if actual.trim() != expected {
            return Err(download_error(
                "download_source_changed",
                format!("Remote ETag changed from {} to {}", expected, actual.trim()),
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ChunkSpec {
    index: usize,
    start: u64,
    end: u64,
}

impl ChunkSpec {
    fn len(&self) -> u64 {
        self.end - self.start + 1
    }
}

/// 单个分片写入独立文件。文件长度就是已连续完成的字节数，因此应用重启后可直接续传。
async fn download_chunk(
    client: reqwest::Client,
    url: String,
    chunk_path: PathBuf,
    chunk: ChunkSpec,
    total_file_size: u64,
    expected_etag: Option<String>,
    downloaded_total: Arc<AtomicU64>,
) -> Result<usize, String> {
    let chunk_total = chunk.len();
    let mut downloaded_in_chunk = match std::fs::metadata(&chunk_path) {
        Ok(metadata) if metadata.len() <= chunk_total => metadata.len(),
        Ok(_) => {
            remove_file_if_exists(&chunk_path)?;
            0
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(download_io_error("Failed to inspect chunk file", error)),
    };
    if downloaded_in_chunk == chunk_total {
        return Ok(chunk.index);
    }

    let max_attempts = 3;
    for attempt in 1..=max_attempts {
        let current_start = chunk.start + downloaded_in_chunk;
        let response = match range_request(
            &client,
            &url,
            format!("bytes={}-{}", current_start, chunk.end),
            expected_etag.as_deref(),
        )
        .send()
        .await
        {
            Ok(response) => response,
            Err(error) => {
                if attempt == max_attempts {
                    return Err(download_error(
                        "download_network",
                        format!(
                            "Chunk {} failed after {} attempts: {}",
                            chunk.index, max_attempts, error
                        ),
                    ));
                }
                tokio::time::sleep(std::time::Duration::from_millis(500 * attempt as u64)).await;
                continue;
            }
        };

        if let Err(error) = validate_chunk_response(
            &response,
            current_start,
            chunk.end,
            total_file_size,
            expected_etag.as_deref(),
        ) {
            if !error.starts_with("sayit_error:download_network:") || attempt == max_attempts {
                return Err(error);
            }
            tokio::time::sleep(std::time::Duration::from_millis(500 * attempt as u64)).await;
            continue;
        }

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&chunk_path)
            .map_err(|error| download_io_error("Failed to open resumable chunk file", error))?;
        let mut stream = response.bytes_stream();
        let mut interrupted = false;

        while let Some(item) = stream.next().await {
            match item {
                Ok(bytes) => {
                    let len = bytes.len() as u64;
                    if downloaded_in_chunk + len > chunk_total {
                        return Err(download_error(
                            "download_range_invalid",
                            format!("Chunk {} exceeded its requested range", chunk.index),
                        ));
                    }
                    file.write_all(&bytes)
                        .map_err(|error| download_io_error("Failed to write chunk data", error))?;
                    downloaded_in_chunk += len;
                    downloaded_total.fetch_add(len, Ordering::Relaxed);
                }
                Err(error) => {
                    log::warn!(
                        "Chunk {} stream interrupted on attempt {}: {}",
                        chunk.index,
                        attempt,
                        error
                    );
                    interrupted = true;
                    break;
                }
            }
        }
        file.flush()
            .map_err(|error| download_io_error("Failed to flush chunk data", error))?;
        drop(file);

        if !interrupted && downloaded_in_chunk == chunk_total {
            return Ok(chunk.index);
        }
        if attempt < max_attempts {
            tokio::time::sleep(std::time::Duration::from_millis(500 * attempt as u64)).await;
        }
    }

    Err(download_error(
        "download_network",
        format!(
            "Chunk {} incomplete: received {}/{} bytes",
            chunk.index, downloaded_in_chunk, chunk_total
        ),
    ))
}

/// 根据总大小计算合理的分片规划
fn calculate_chunks(total_size: u64) -> Vec<ChunkSpec> {
    if total_size == 0 {
        return vec![];
    }
    let num_chunks = (total_size / (32 * 1024 * 1024)).clamp(4, 16) as usize;
    let chunk_size = (total_size + num_chunks as u64 - 1) / (num_chunks as u64);

    let mut chunks = Vec::with_capacity(num_chunks);
    for i in 0..num_chunks {
        let start = i as u64 * chunk_size;
        if start >= total_size {
            break;
        }
        let end = ((i as u64 + 1) * chunk_size - 1).min(total_size - 1);
        chunks.push(ChunkSpec { index: i, start, end });
    }
    chunks
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ParallelPhase {
    Downloading,
    Assembling,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ParallelDownloadState {
    version: u32,
    total_size: u64,
    expected_sha256: Option<String>,
    source_url: String,
    strong_etag: Option<String>,
    chunks: Vec<ChunkSpec>,
    phase: ParallelPhase,
    assembled_chunks: usize,
}

fn sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let name = path
        .file_name()
        .map(|value| value.to_string_lossy())
        .unwrap_or_default();
    path.with_file_name(format!("{}{}", name, suffix))
}

fn parallel_state_path(temp_path: &Path) -> PathBuf {
    sibling_path(temp_path, ".json")
}

fn parallel_chunk_path(temp_path: &Path, index: usize) -> PathBuf {
    sibling_path(temp_path, &format!(".chunk-{:02}", index))
}

fn persist_parallel_state(path: &Path, state: &ParallelDownloadState) -> Result<(), String> {
    let bytes = serde_json::to_vec(state)
        .map_err(|error| download_error("download_failed", format!("Failed to serialize download state: {}", error)))?;
    std::fs::write(path, bytes)
        .map_err(|error| download_io_error("Failed to persist parallel download state", error))
}

fn cleanup_parallel_artifacts(
    temp_path: &Path,
    state_path: &Path,
    chunks: &[ChunkSpec],
) -> Result<(), String> {
    remove_file_if_exists(temp_path)?;
    remove_file_if_exists(state_path)?;
    for chunk in chunks {
        remove_file_if_exists(&parallel_chunk_path(temp_path, chunk.index))?;
    }
    Ok(())
}

fn cleanup_parallel_artifacts_best_effort(
    temp_path: &Path,
    state_path: &Path,
    chunks: &[ChunkSpec],
) {
    let chunk_paths = chunks.iter().map(|chunk| parallel_chunk_path(temp_path, chunk.index));
    let paths = [state_path.to_path_buf(), temp_path.to_path_buf()].into_iter().chain(chunk_paths);
    for path in paths {
        if let Err(error) = remove_file_if_exists(&path) {
            log::warn!("Failed to clean parallel download artifact: {}", error);
        }
    }
}

fn parallel_state_matches(
    state: &ParallelDownloadState,
    total_size: u64,
    expected_sha256: Option<&str>,
    source_url: &str,
    strong_etag: Option<&str>,
    chunks: &[ChunkSpec],
) -> bool {
    state.version == PARALLEL_STATE_VERSION
        && state.total_size == total_size
        && state.expected_sha256.as_deref() == expected_sha256
        && state.source_url == source_url
        && state.strong_etag.as_deref() == strong_etag
        && state.chunks == chunks
        && state.assembled_chunks <= chunks.len()
}

fn load_or_create_parallel_state(
    temp_path: &Path,
    total_size: u64,
    expected_sha256: Option<&str>,
    source_url: &str,
    strong_etag: Option<&str>,
    chunks: &[ChunkSpec],
) -> Result<ParallelDownloadState, String> {
    let state_path = parallel_state_path(temp_path);
    match std::fs::read(&state_path) {
        Ok(bytes) => match serde_json::from_slice::<ParallelDownloadState>(&bytes) {
            Ok(state)
                if parallel_state_matches(
                    &state,
                    total_size,
                    expected_sha256,
                    source_url,
                    strong_etag,
                    chunks,
                ) =>
            {
                return Ok(state);
            }
            Ok(old_state) => {
                cleanup_parallel_artifacts(temp_path, &state_path, &old_state.chunks)?;
                // 新旧规划数量不同时，再清理一遍当前规划可能对应的孤儿分片。
                for chunk in chunks {
                    remove_file_if_exists(&parallel_chunk_path(temp_path, chunk.index))?;
                }
            }
            Err(_) => {
                cleanup_parallel_artifacts(temp_path, &state_path, chunks)?;
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // 清理由旧版下载器或崩溃在状态文件落盘前留下的同名文件。
            cleanup_parallel_artifacts(temp_path, &state_path, chunks)?;
        }
        Err(error) => return Err(download_io_error("Failed to read parallel download state", error)),
    }

    let state = ParallelDownloadState {
        version: PARALLEL_STATE_VERSION,
        total_size,
        expected_sha256: expected_sha256.map(str::to_string),
        source_url: source_url.to_string(),
        strong_etag: strong_etag.map(str::to_string),
        chunks: chunks.to_vec(),
        phase: ParallelPhase::Downloading,
        assembled_chunks: 0,
    };
    persist_parallel_state(&state_path, &state)?;
    Ok(state)
}

fn chunk_file_len(path: &Path, maximum: u64) -> Result<u64, String> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.len() <= maximum => Ok(metadata.len()),
        Ok(_) => {
            remove_file_if_exists(path)?;
            Ok(0)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(download_io_error("Failed to inspect chunk file", error)),
    }
}

fn prepare_assembly_file(
    temp_path: &Path,
    chunks: &[ChunkSpec],
    assembled_chunks: usize,
) -> Result<(), String> {
    let expected_len: u64 = chunks
        .iter()
        .take(assembled_chunks)
        .map(ChunkSpec::len)
        .sum();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(temp_path)
        .map_err(|error| download_io_error("Failed to open assembly file", error))?;
    let actual_len = file
        .metadata()
        .map_err(|error| download_io_error("Failed to inspect assembly file", error))?
        .len();
    if actual_len < expected_len {
        return Err(download_error(
            "download_parallel_verify_failed",
            format!(
                "Assembly file is shorter than its persisted state: {}/{} bytes",
                actual_len, expected_len
            ),
        ));
    }
    if actual_len != expected_len {
        file.set_len(expected_len)
            .map_err(|error| download_io_error("Failed to repair assembly file length", error))?;
    }
    Ok(())
}

fn assemble_or_reset_parallel_download(
    temp_path: &Path,
    state_path: &Path,
    chunks: &[ChunkSpec],
    state: &mut ParallelDownloadState,
) -> Result<(), String> {
    let result = assemble_parallel_chunks(temp_path, state_path, chunks, state);
    if result.is_err() {
        cleanup_parallel_artifacts_best_effort(temp_path, state_path, chunks);
    }
    result
}

fn assemble_parallel_chunks(
    temp_path: &Path,
    state_path: &Path,
    chunks: &[ChunkSpec],
    state: &mut ParallelDownloadState,
) -> Result<(), String> {
    prepare_assembly_file(temp_path, chunks, state.assembled_chunks)?;

    for index in 0..state.assembled_chunks {
        // 崩溃可能发生在状态落盘后、旧分片删除前；这些残留现在可以安全清理。
        remove_file_if_exists(&parallel_chunk_path(temp_path, index))?;
    }

    let mut assembled = std::fs::OpenOptions::new()
        .append(true)
        .open(temp_path)
        .map_err(|error| download_io_error("Failed to open assembly file for append", error))?;
    for chunk in chunks.iter().skip(state.assembled_chunks) {
        let chunk_path = parallel_chunk_path(temp_path, chunk.index);
        let actual = chunk_file_len(&chunk_path, chunk.len())?;
        if actual != chunk.len() {
            return Err(download_error(
                "download_parallel_verify_failed",
                format!("Chunk {} disappeared before assembly", chunk.index),
            ));
        }
        let mut source = std::fs::File::open(&chunk_path)
            .map_err(|error| download_io_error("Failed to open completed chunk", error))?;
        let copied = std::io::copy(&mut source, &mut assembled)
            .map_err(|error| download_io_error("Failed to assemble chunk", error))?;
        if copied != chunk.len() {
            return Err(download_error(
                "download_parallel_verify_failed",
                format!(
                    "Chunk {} changed during assembly: copied {}/{} bytes",
                    chunk.index,
                    copied,
                    chunk.len()
                ),
            ));
        }
        assembled
            .flush()
            .map_err(|error| download_io_error("Failed to flush assembly file", error))?;
        assembled
            .sync_data()
            .map_err(|error| download_io_error("Failed to persist assembled chunk", error))?;
        state.assembled_chunks = chunk.index + 1;
        persist_parallel_state(state_path, state)?;
        remove_file_if_exists(&chunk_path)?;
    }
    Ok(())
}

/// 并发下载到独立分片文件；每个分片可跨调用续传，完成后按顺序组装并逐段释放空间。
async fn download_file_parallel(
    report: &(dyn Fn(DownloadProgress) + Sync),
    model_id: &str,
    file_name: &str,
    url: &str,
    total_size: u64,
    expected_sha256: &str,
    strong_etag: Option<&str>,
    temp_path: &Path,
    dest_path: &Path,
    file_index: u32,
    file_count: u32,
) -> Result<(), String> {
    let chunks = calculate_chunks(total_size);
    let state_path = parallel_state_path(temp_path);
    let mut state = load_or_create_parallel_state(
        temp_path,
        total_size,
        Some(expected_sha256),
        url,
        strong_etag,
        &chunks,
    )?;

    if state.phase == ParallelPhase::Downloading {
        let mut initial_downloaded = 0u64;
        for chunk in &chunks {
            initial_downloaded += chunk_file_len(
                &parallel_chunk_path(temp_path, chunk.index),
                chunk.len(),
            )?;
        }
        let downloaded_total = Arc::new(AtomicU64::new(initial_downloaded));
        let client = build_http_client()?;
        let mut futures = FuturesUnordered::new();

        for chunk in chunks.iter().cloned() {
            let path = parallel_chunk_path(temp_path, chunk.index);
            if chunk_file_len(&path, chunk.len())? == chunk.len() {
                continue;
            }
            futures.push(download_chunk(
                client.clone(),
                url.to_string(),
                path,
                chunk,
                total_size,
                strong_etag.map(str::to_string),
                Arc::clone(&downloaded_total),
            ));
        }

        emit_progress(
            report,
            model_id,
            file_name,
            initial_downloaded,
            total_size,
            "downloading",
            None,
            file_index,
            file_count,
        );
        let mut progress_interval = tokio::time::interval(std::time::Duration::from_millis(150));
        let mut worker_error = None;
        while !futures.is_empty() {
            tokio::select! {
                _ = progress_interval.tick() => {
                    emit_progress(
                        report,
                        model_id,
                        file_name,
                        downloaded_total.load(Ordering::Relaxed),
                        total_size,
                        "downloading",
                        None,
                        file_index,
                        file_count,
                    );
                }
                result = futures.next() => {
                    if let Some(Err(error)) = result {
                        worker_error = Some(error);
                        break;
                    }
                }
            }
        }
        // 必须先释放其他 worker 持有的文件句柄，再决定是否清理临时文件。
        drop(futures);
        if let Some(error) = worker_error {
            if error.starts_with("sayit_error:download_range_invalid:")
                || error.starts_with("sayit_error:download_source_changed:")
            {
                cleanup_parallel_artifacts(temp_path, &state_path, &chunks)?;
            }
            return Err(error);
        }

        for chunk in &chunks {
            let path = parallel_chunk_path(temp_path, chunk.index);
            let actual = chunk_file_len(&path, chunk.len())?;
            if actual != chunk.len() {
                return Err(download_error(
                    "download_parallel_verify_failed",
                    format!(
                        "Chunk {} is incomplete after workers finished: {}/{} bytes",
                        chunk.index,
                        actual,
                        chunk.len()
                    ),
                ));
            }
        }

        state.phase = ParallelPhase::Assembling;
        state.assembled_chunks = 0;
        persist_parallel_state(&state_path, &state)?;
        remove_file_if_exists(temp_path)?;
    }

    // [自訂] REV9：完成途中讀不到分片也重置狀態，避免重試困在同一個分片。
    assemble_or_reset_parallel_download(temp_path, &state_path, &chunks, &mut state)?;

    if let Err(error) = finalize_download(
        temp_path,
        dest_path,
        total_size,
        expected_sha256,
        "download_parallel_verify_failed",
    ) {
        // [自訂] SC-38：完成階段錯誤後，下次從乾淨狀態重新下載或組裝。
        cleanup_parallel_artifacts_best_effort(temp_path, &state_path, &chunks);
        return Err(error);
    }
    cleanup_parallel_artifacts_best_effort(temp_path, &state_path, &chunks);
    emit_progress(
        report,
        model_id,
        file_name,
        total_size,
        total_size,
        "completed",
        None,
        file_index,
        file_count,
    );
    log::info!(
        "Parallel download resumed, verified and completed: {} ({} bytes)",
        file_name,
        total_size
    );
    Ok(())
}

/// 并发失败后只有协议不兼容或最终对账失败适合改走单流；
/// 普通网络中断保留分片，让用户重试时从已有进度继续。
fn should_fallback_to_single_stream(err_msg: &str) -> bool {
    err_msg.starts_with("sayit_error:download_range_invalid:")
        || err_msg.starts_with("sayit_error:download_source_changed:")
        || err_msg.starts_with("sayit_error:download_parallel_verify_failed:")
        || is_checksum_error(err_msg)
}

/// 纯函数校验断点续传响应（严格遵循 RFC 9110 §14.1.2）
/// 规则：
/// 1. 必须提供明确的 Content-Range 响应头（格式: bytes <start>-<end>/<total>）。
/// 2. start 必须严格等于 expected_downloaded。
/// 3. total 必须是明确合法的十进制数字（严禁 *，未知 total 必须拒绝 resume 并从头全量下载）。
/// 4. 如果 expected_total > 0，total 必须等于 expected_total。
/// 5. 对于 Range: bytes=N- 的请求，响应必须覆盖到文件最后一个字节，即 end == total - 1。
/// 6. 如果提供了 content_length，必须满足 content_length == end - start + 1（即 total - start）。
fn is_valid_resume_content_range(
    content_range_opt: Option<&str>,
    content_length_opt: Option<u64>,
    expected_downloaded: u64,
    expected_total: u64,
) -> bool {
    let Some((start, end, total)) = content_range_opt.and_then(parse_content_range_header) else {
        return false;
    };
    if total == 0 || start != expected_downloaded || end < start || end != total - 1 {
        return false;
    }
    if expected_total > 0 && total != expected_total {
        return false;
    }
    content_length_opt.map_or(true, |length| length == end - start + 1)
}

fn parse_unsatisfied_range_total(content_range: Option<&str>) -> Option<u64> {
    content_range?
        .trim()
        .strip_prefix("bytes */")?
        .parse()
        .ok()
}

async fn send_full_response(
    client: &reqwest::Client,
    url: &str,
) -> Result<reqwest::Response, String> {
    let response = full_request(client, url)
        .send()
        .await
        .map_err(|error| download_error("download_network", format!("Full download request failed: {}", error)))?;
    if response.status().as_u16() != 200 {
        return Err(download_error(
            "download_network",
            format!("Full download failed with HTTP {}", response.status()),
        ));
    }
    Ok(response)
}

// [自訂] SC-35：完整斷點經驗證後直接替換；只有不完整的檔案需要續傳。
fn prepare_stream_partial(
    temp_path: &Path,
    dest_path: &Path,
    total_size: u64,
    expected_sha256: &str,
) -> Result<Option<u64>, String> {
    let raw_initial = match std::fs::metadata(temp_path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => {
            return Err(download_io_error(
                "Failed to read partial download metadata",
                error,
            ));
        }
    };

    match raw_initial.cmp(&total_size) {
        std::cmp::Ordering::Less => return Ok(Some(raw_initial)),
        std::cmp::Ordering::Greater => remove_file_if_exists(temp_path)?,
        std::cmp::Ordering::Equal => {
            match finalize_download(
                temp_path,
                dest_path,
                total_size,
                expected_sha256,
                "download_source_mismatch",
            ) {
                Ok(()) => return Ok(None),
                Err(error) if is_checksum_error(&error) => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(Some(0))
}

/// 单流下载（用于小文件、不支持 Range 的服务端或并发协议失败时的 Fallback）
async fn download_file_single_stream(
    report: &(dyn Fn(DownloadProgress) + Sync),
    model_id: &str,
    file_name: &str,
    url: &str,
    total_size: u64,
    expected_sha256: &str,
    strong_etag: Option<&str>,
    temp_path: &Path,
    dest_path: &Path,
    file_index: u32,
    file_count: u32,
) -> Result<(), String> {
    let Some(downloaded_initial) =
        prepare_stream_partial(temp_path, dest_path, total_size, expected_sha256)?
    else {
        emit_progress(
            report,
            model_id,
            file_name,
            total_size,
            total_size,
            "completed",
            None,
            file_index,
            file_count,
        );
        return Ok(());
    };

    let client = build_http_client()?;
    let (response, mut downloaded, is_resume) = if downloaded_initial > 0 {
        emit_progress(
            report,
            model_id,
            file_name,
            downloaded_initial,
            total_size,
            "downloading",
            None,
            file_index,
            file_count,
        );
        let response = resume_request(&client, url, downloaded_initial, strong_etag)
            .send()
            .await
            .map_err(|error| download_error("download_network", format!("Resume request failed: {}", error)))?;
        match response.status().as_u16() {
            206 => {
                let content_range = response
                    .headers()
                    .get(reqwest::header::CONTENT_RANGE)
                    .and_then(|value| value.to_str().ok());
                if is_valid_resume_content_range(
                    content_range,
                    response.content_length(),
                    downloaded_initial,
                    total_size,
                ) {
                    (response, downloaded_initial, true)
                } else {
                    log::warn!("Invalid resume Content-Range; restarting {} from zero", file_name);
                    drop(response);
                    remove_file_if_exists(temp_path)?;
                    (send_full_response(&client, url).await?, 0, false)
                }
            }
            200 => {
                // If-Range 不匹配或服务端忽略 Range：当前响应是完整新表示，覆盖旧 partial。
                (response, 0, false)
            }
            416 => {
                let remote_total = parse_unsatisfied_range_total(
                    response
                        .headers()
                        .get(reqwest::header::CONTENT_RANGE)
                        .and_then(|value| value.to_str().ok()),
                );
                drop(response);
                if let Some(remote_total) = remote_total.filter(|&size| size != total_size) {
                    return Err(download_error(
                        "download_source_mismatch",
                        format!(
                            "Catalog size is {}, but the server reports {} bytes",
                            total_size, remote_total
                        ),
                    ));
                }
                // [自訂] SC-35：發出 Range 時 offset 必定小於 catalog 大小。
                // 416 無法證明暫存檔已完整，改以全量請求重試。
                remove_file_if_exists(temp_path)?;
                (send_full_response(&client, url).await?, 0, false)
            }
            status => {
                return Err(download_error(
                    "download_network",
                    format!("Resume request failed with HTTP {}", status),
                ));
            }
        }
    } else {
        emit_progress(
            report,
            model_id,
            file_name,
            0,
            total_size,
            "downloading",
            None,
            file_index,
            file_count,
        );
        (send_full_response(&client, url).await?, 0, false)
    };

    let content_len = response.content_length().unwrap_or(0);
    if !is_resume && content_len > 0 && content_len != total_size {
        return Err(download_error(
            "download_source_mismatch",
            format!(
                "Catalog size is {}, but the full response contains {} bytes",
                total_size, content_len
            ),
        ));
    }
    let mut file = if is_resume {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(temp_path)
            .map_err(|error| download_io_error("Failed to open partial download for append", error))?
    } else {
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(temp_path)
            .map_err(|error| download_io_error("Failed to restart partial download", error))?
    };

    let mut stream = response.bytes_stream();
    let mut last_emit = std::time::Instant::now();
    while let Some(item) = stream.next().await {
        let bytes = item.map_err(|error| {
            download_error("download_network", format!("Download interrupted: {}", error))
        })?;
        if downloaded + bytes.len() as u64 > total_size {
            return Err(download_error(
                "download_source_mismatch",
                format!("Server sent more than the expected {} bytes", total_size),
            ));
        }
        file.write_all(&bytes)
            .map_err(|error| download_io_error("Failed to write partial download", error))?;
        downloaded += bytes.len() as u64;
        if last_emit.elapsed().as_millis() >= 200 {
            emit_progress(
                report,
                model_id,
                file_name,
                downloaded,
                total_size,
                "downloading",
                None,
                file_index,
                file_count,
            );
            last_emit = std::time::Instant::now();
        }
    }
    file.flush()
        .map_err(|error| download_io_error("Failed to flush partial download", error))?;
    drop(file);

    if downloaded != total_size {
        let error = download_error(
            "download_failed",
            format!(
                "Single-stream download incomplete for {}: received {}/{} bytes",
                file_name, downloaded, total_size
            ),
        );
        return Err(error);
    }
    finalize_download(
        temp_path,
        dest_path,
        total_size,
        expected_sha256,
        "download_source_mismatch",
    )?;
    emit_progress(
        report,
        model_id,
        file_name,
        downloaded,
        total_size,
        "completed",
        None,
        file_index,
        file_count,
    );
    Ok(())
}

/// 下载单个文件，支持智能并发分片、临时文件隔离、SHA-256 完整性验证与安全 Fallback 降级
pub async fn download_file(
    app: AppHandle,
    model_id: &str,
    file_name: &str,
    url: &str,
    expected_size: u64,
    expected_sha256: Option<&str>,
    dest_dir: &Path,
    file_index: u32,
    file_count: u32,
) -> Result<(), String> {
    download_file_with_progress(
        &|progress| { let _ = app.emit("model-download-progress", progress); },
        model_id, file_name, url, expected_size, expected_sha256, dest_dir, file_index, file_count,
    ).await
}

// [自訂] SC-38：測試可走完整下載流程，終止失敗只通知一次。
async fn download_file_with_progress(
    report: &(dyn Fn(DownloadProgress) + Sync),
    model_id: &str,
    file_name: &str,
    url: &str,
    expected_size: u64,
    expected_sha256: Option<&str>,
    dest_dir: &Path,
    file_index: u32,
    file_count: u32,
) -> Result<(), String> {
    let result = download_file_inner(
        report, model_id, file_name, url, expected_size, expected_sha256, dest_dir, file_index, file_count,
    ).await;
    if let Err(error) = &result {
        emit_progress(
            report, model_id, file_name, 0, expected_size, "failed", Some(error), file_index, file_count,
        );
    }
    result
}

async fn download_file_inner(
    report: &(dyn Fn(DownloadProgress) + Sync),
    model_id: &str,
    file_name: &str,
    url: &str,
    expected_size: u64,
    expected_sha256: Option<&str>,
    dest_dir: &Path,
    file_index: u32,
    file_count: u32,
) -> Result<(), String> {
    // [自訂] SC-35：下載與續傳只接受 catalog 提供的大小與 SHA-256。
    let expected_sha256 = expected_sha256
        .filter(|_| expected_size > 0)
        .ok_or_else(|| download_error("download_failed", "Missing catalog size or SHA-256"))?;
    let dest_path = dest_dir.join(file_name);
    let stream_temp = dest_dir.join(format!("{}.part", file_name));
    let parallel_temp = dest_dir.join(format!("{}.par.part", file_name));

    std::fs::create_dir_all(dest_dir)
        .map_err(|error| download_io_error("Failed to create model directory", error))?;
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| download_io_error("Failed to create model subdirectory", error))?;
    }

    validate_download_destination(&dest_path)?;

    // [自訂] SC-35：手動放入的模型可能是符號連結，也要保留它指向的檔案。
    let existing_target = std::fs::canonicalize(&dest_path).ok();
    let artifact_dir = std::fs::canonicalize(dest_path.parent().unwrap_or(dest_dir))
        .map_err(|e| download_io_error("Failed to resolve model directory", e))?;
    let check_artifact = |path: &Path| {
        let canonical_path = artifact_dir.join(path.file_name().unwrap_or_default());
        check_download_artifact(&canonical_path, existing_target.as_deref())
    };
    check_artifact(&stream_temp)?;
    check_artifact(&parallel_temp)?;
    check_artifact(&parallel_state_path(&parallel_temp))?;
    for chunk in calculate_chunks(expected_size) {
        check_artifact(&parallel_chunk_path(&parallel_temp, chunk.index))?;
    }

    if reuse_verified_download(&dest_path, expected_size, expected_sha256)?
        || recover_staged_weight(&dest_path, expected_size, expected_sha256)?
    {
        if let Err(error) = remove_file_if_exists(&stream_temp) {
            log::warn!("Failed to remove stale single-stream partial: {}", error);
        }
        cleanup_parallel_artifacts_best_effort(
            &parallel_temp,
            &parallel_state_path(&parallel_temp),
            &calculate_chunks(expected_size),
        );
        emit_progress(
            report,
            model_id,
            file_name,
            expected_size,
            expected_size,
            "completed",
            None,
            file_index,
            file_count,
        );
        return Ok(());
    }

    // [自訂] SC-35：舊檔保留至新檔通過驗證。
    let client = build_http_client()?;
    let probe = probe_url(&client, url).await;
    let total_size = resolve_download_size(expected_size, probe.size)?;

    // 旧版留下的连续 .part 优先续传，避免为了切换并发实现而浪费已有进度。
    let stream_partial_exists = std::fs::metadata(&stream_temp)
        .map(|metadata| metadata.len() > 0)
        .unwrap_or(false);
    let parallel_state_exists = std::fs::metadata(parallel_state_path(&parallel_temp)).is_ok();
    let can_parallel = probe.supports_range && total_size >= 16 * 1024 * 1024;

    if can_parallel && (!stream_partial_exists || parallel_state_exists) {
        let result = download_file_parallel(
            report,
            model_id,
            file_name,
            url,
            total_size,
            expected_sha256,
            probe.strong_etag.as_deref(),
            &parallel_temp,
            &dest_path,
            file_index,
            file_count,
        )
        .await;
        match result {
            Ok(()) => {
                if let Err(error) = remove_file_if_exists(&stream_temp) {
                    log::warn!("Failed to remove stale single-stream partial: {}", error);
                }
                Ok(())
            }
            Err(error) if should_fallback_to_single_stream(&error) => {
                log::warn!(
                    "Parallel protocol failed for {}; falling back to a full single stream: {}",
                    file_name,
                    error
                );
                let chunks = calculate_chunks(total_size);
                cleanup_parallel_artifacts_best_effort(
                    &parallel_temp,
                    &parallel_state_path(&parallel_temp),
                    &chunks,
                );
                download_file_single_stream(
                    report,
                    model_id,
                    file_name,
                    url,
                    total_size,
                    expected_sha256,
                    probe.strong_etag.as_deref(),
                    &stream_temp,
                    &dest_path,
                    file_index,
                    file_count,
                )
                .await
            }
            // 網路中斷保留分片，重試時可續傳。
            Err(error) => Err(error),
        }
    } else {
        let result = download_file_single_stream(
            report,
            model_id,
            file_name,
            url,
            total_size,
            expected_sha256,
            probe.strong_etag.as_deref(),
            &stream_temp,
            &dest_path,
            file_index,
            file_count,
        )
        .await;
        if result.is_ok() && total_size > 0 {
            let chunks = calculate_chunks(total_size);
            cleanup_parallel_artifacts_best_effort(
                &parallel_temp,
                &parallel_state_path(&parallel_temp),
                &chunks,
            );
        }
        result
    }
}

#[cfg(test)]
mod model_id_tests {
    use super::{checked_model_dir, is_valid_model_id};

    #[test]
    fn catalog_style_ids_pass_and_paths_are_rejected() {
        for ok in ["sensevoice-small-gguf", "qwen3-asr-0.6b", "fun_asr.nano"] {
            assert!(is_valid_model_id(ok), "{ok}");
        }
        for bad in ["", "..", "../x", r"..\..", r"C:\Users\someone", "/etc", "a/b", r"a\b", ".hidden", "x..y"] {
            assert!(!is_valid_model_id(bad), "{bad}");
            assert!(checked_model_dir(bad).is_err(), "{bad}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn unique_test_path(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "sayit_downloader_{}_{}_{}",
            label,
            std::process::id(),
            nonce
        ))
    }

    fn spawn_http_server(
        responses: Vec<Vec<u8>>,
    ) -> (String, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 1024];
                loop {
                    let read = stream.read(&mut buffer).unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                requests.push(String::from_utf8_lossy(&request).to_string());
                stream.write_all(&response).unwrap();
                stream.flush().unwrap();
            }
            requests
        });
        (format!("http://{}/model", address), handle)
    }

    fn partial_response(
        start: u64,
        end: u64,
        total: u64,
        etag: &str,
        body: &[u8],
    ) -> Vec<u8> {
        let headers = format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nETag: {}\r\nConnection: close\r\n\r\n",
            start,
            end,
            total,
            body.len(),
            etag
        );
        [headers.as_bytes(), body].concat()
    }

    // [自訂] SC-38：涵蓋入口到網路失敗，先刪 dest 的回歸會讓此測試失敗。
    #[tokio::test(flavor = "current_thread")]
    async fn full_download_keeps_old_weight_and_reports_one_failure() {
        let dir = unique_test_path("full_download");
        std::fs::create_dir(&dir).unwrap();
        let dest = dir.join("weight.gguf");
        std::fs::write(&dest, b"old").unwrap();
        let (url, server) = spawn_http_server(vec![
            partial_response(0, 0, 3, "\"model-v1\"", b"x"),
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nxyz".to_vec(),
        ]);
        let statuses = Mutex::new(Vec::new());
        let error = download_file_with_progress(
            &|progress| statuses.lock().unwrap().push(progress.status),
            "test", "weight.gguf", &url, 3,
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
            &dir, 1, 1,
        ).await.unwrap_err();
        assert!(is_checksum_error(&error));
        assert_eq!(std::fs::read(&dest).unwrap(), b"old");
        let statuses = statuses.into_inner().unwrap();
        assert_eq!(statuses.iter().filter(|status| *status == "failed").count(), 1);
        assert_eq!(statuses.last().map(String::as_str), Some("failed"));
        assert_eq!(server.join().unwrap().len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn download_recovers_verified_weight_without_network_and_keeps_invalid_staging() {
        let dir = unique_test_path("recover_weight");
        std::fs::create_dir(&dir).unwrap();
        let invalid = dir.join(format!(".sayit-model-{}.tmp", uuid::Uuid::new_v4()));
        let valid = dir.join(format!(".sayit-model-{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&invalid, b"xyz").unwrap();
        std::fs::write(&valid, b"abc").unwrap();
        let dest = dir.join("weight.gguf");
        let statuses = Mutex::new(Vec::new());
        download_file_with_progress(
            &|progress| statuses.lock().unwrap().push(progress.status),
            "test", "weight.gguf", "http://127.0.0.1:0/model", 3,
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
            &dir, 1, 1,
        ).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"abc");
        assert_eq!(std::fs::read(&invalid).unwrap(), b"xyz");
        assert!(!valid.exists());
        assert_eq!(statuses.into_inner().unwrap(), ["completed"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn download_rejects_destination_directory_before_connecting() {
        let dir = unique_test_path("directory_destination");
        std::fs::create_dir_all(dir.join("weight.gguf")).unwrap();
        let error = download_file_with_progress(
            &|_| {},
            "test", "weight.gguf", "http://127.0.0.1:0/model", 3,
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
            &dir, 1, 1,
        ).await.unwrap_err();
        assert!(error.contains("Model destination is a directory"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn probe_requires_exact_range_and_captures_strong_etag() {
        let (url, server) = spawn_http_server(vec![partial_response(
            0,
            0,
            6,
            "\"model-v1\"",
            b"a",
        )]);
        let probe = probe_url(&build_http_client().unwrap(), &url).await;
        assert!(probe.supports_range);
        assert_eq!(probe.size, 6);
        assert_eq!(probe.strong_etag.as_deref(), Some("\"model-v1\""));

        let requests = server.join().unwrap();
        let request = requests[0].to_ascii_lowercase();
        assert!(request.contains("range: bytes=0-0"));
        assert!(request.contains("accept-encoding: identity"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn chunk_resumes_existing_file_and_uses_if_match() {
        let chunk_path = unique_test_path("chunk_resume");
        std::fs::write(&chunk_path, b"ab").unwrap();
        let (url, server) = spawn_http_server(vec![partial_response(
            2,
            5,
            6,
            "\"model-v1\"",
            b"cdef",
        )]);
        let downloaded = Arc::new(AtomicU64::new(2));
        let result = download_chunk(
            build_http_client().unwrap(),
            url,
            chunk_path.clone(),
            ChunkSpec {
                index: 0,
                start: 0,
                end: 5,
            },
            6,
            Some("\"model-v1\"".to_string()),
            Arc::clone(&downloaded),
        )
        .await;
        assert_eq!(result.unwrap(), 0);
        assert_eq!(std::fs::read(&chunk_path).unwrap(), b"abcdef");
        assert_eq!(downloaded.load(Ordering::Relaxed), 6);

        let request = server.join().unwrap()[0].to_ascii_lowercase();
        assert!(request.contains("range: bytes=2-5"));
        assert!(request.contains("if-match: \"model-v1\""));
        std::fs::remove_file(chunk_path).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn chunk_rejects_a_changed_response_etag_before_writing() {
        let chunk_path = unique_test_path("chunk_etag_change");
        let (url, server) = spawn_http_server(vec![partial_response(
            0,
            3,
            4,
            "\"model-v2\"",
            b"data",
        )]);
        let error = download_chunk(
            build_http_client().unwrap(),
            url,
            chunk_path.clone(),
            ChunkSpec {
                index: 0,
                start: 0,
                end: 3,
            },
            4,
            Some("\"model-v1\"".to_string()),
            Arc::new(AtomicU64::new(0)),
        )
        .await
        .unwrap_err();
        assert!(error.starts_with("sayit_error:download_source_changed:"));
        assert!(!chunk_path.exists());
        let request = server.join().unwrap()[0].to_ascii_lowercase();
        assert!(request.contains("if-match: \"model-v1\""));
    }

    #[cfg(unix)]
    #[test]
    fn model_recovery_leaves_symbolic_and_hard_links_untouched() {
        use std::fs;
        let dir = std::env::temp_dir().join(format!("sayit-download-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let backing = dir.join("backing");
        let symbolic = dir.join(format!(".sayit-model-{}.tmp", uuid::Uuid::new_v4()));
        let hard = dir.join(format!(".sayit-model-{}.tmp", uuid::Uuid::new_v4()));
        let dest = dir.join("expected.gguf");
        fs::write(&backing, b"abc").unwrap();
        std::os::unix::fs::symlink(&backing, &symbolic).unwrap();
        fs::hard_link(&backing, &hard).unwrap();
        let sha = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(!recover_staged_weight(&dest, 3, sha).unwrap());
        assert!(!dest.exists());
        assert!(fs::symlink_metadata(&symbolic).unwrap().is_symlink());
        assert_eq!(fs::read(&hard).unwrap(), b"abc");
        assert_eq!(fs::read(&backing).unwrap(), b"abc");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_catalog_rename_only_cleans_empty_placeholders() {
        use std::fs;
        let dir = std::env::temp_dir().join(format!("sayit-download-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let empty = dir.join("empty.tmp");
        let weight = dir.join("weight.tmp");
        fs::write(&empty, b"").unwrap();
        fs::write(&weight, b"keep").unwrap();
        remove_empty_rename_placeholder(&empty);
        remove_empty_rename_placeholder(&weight);
        assert!(!empty.exists());
        assert_eq!(fs::read(&weight).unwrap(), b"keep");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unicode_filename_alias_leaves_original_weight_untouched() {
        use std::fs;
        let dir = std::env::temp_dir().join(format!("sayit-download-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let alias = dir.join("Kit.gguf");
        let expected = dir.join("Kit.gguf");
        let partial = dir.join("weight.part");
        fs::write(&alias, b"original").unwrap();
        assert!(catalog_directory_entry(&expected).unwrap().is_none());
        if expected.exists() {
            fs::write(&partial, b"abc").unwrap();
            let sha = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
            let error = finalize_download(&partial, &expected, 3, sha, "download_source_mismatch").unwrap_err();
            assert!(error.contains("Unsupported model filename spelling"));
            assert_eq!(fs::read(&partial).unwrap(), b"abc");
        }
        assert_eq!(fs::read(&alias).unwrap(), b"original");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_completed_chunk_resets_assembly_and_leaves_destination_intact() {
        use std::fs;
        let dir = std::env::temp_dir().join(format!("sayit-download-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let dest = dir.join("weight.gguf");
        let partial = dir.join("weight.part");
        let state_path = dir.join("assembly-state.json");
        let chunks = vec![
            ChunkSpec { index: 0, start: 0, end: 1 },
            ChunkSpec { index: 1, start: 2, end: 3 },
        ];
        let mut state = ParallelDownloadState {
            version: PARALLEL_STATE_VERSION,
            total_size: 4,
            expected_sha256: None,
            source_url: String::new(),
            strong_etag: None,
            chunks: chunks.clone(),
            phase: ParallelPhase::Assembling,
            assembled_chunks: 0,
        };
        fs::write(&dest, b"original").unwrap();
        fs::write(parallel_chunk_path(&partial, 0), b"ab").unwrap();
        persist_parallel_state(&state_path, &state).unwrap();
        let error = assemble_or_reset_parallel_download(&partial, &state_path, &chunks, &mut state).unwrap_err();
        assert!(error.contains("Chunk 1 disappeared before assembly"));
        assert!(!state_path.exists());
        assert!(!partial.exists());
        assert!(!parallel_chunk_path(&partial, 0).exists());
        assert_eq!(fs::read(&dest).unwrap(), b"original");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            state.assembled_chunks = 0;
            fs::write(parallel_chunk_path(&partial, 0), b"ab").unwrap();
            let blocked = parallel_chunk_path(&partial, 1);
            fs::write(&blocked, b"cd").unwrap();
            fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
            if fs::File::open(&blocked).is_err() {
                persist_parallel_state(&state_path, &state).unwrap();
                let error = assemble_or_reset_parallel_download(&partial, &state_path, &chunks, &mut state)
                    .unwrap_err();
                assert!(error.contains("Failed to open completed chunk"));
                assert!(!state_path.exists());
                assert!(!blocked.exists());
                assert_eq!(fs::read(&dest).unwrap(), b"original");
            }
        }

        state.assembled_chunks = 0;
        fs::write(parallel_chunk_path(&partial, 0), b"ab").unwrap();
        remove_file_if_exists(&parallel_chunk_path(&partial, 1)).unwrap();
        fs::write(parallel_chunk_path(&partial, 1), b"cd").unwrap();
        assemble_or_reset_parallel_download(&partial, &state_path, &chunks, &mut state).unwrap();
        assert_eq!(fs::read(&partial).unwrap(), b"abcd");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn assembly_rewinds_bytes_written_after_the_last_persisted_boundary() {
        let temp_path = unique_test_path("assembly_rewind");
        std::fs::write(&temp_path, b"abcdefEXTRA").unwrap();
        let chunks = vec![
            ChunkSpec {
                index: 0,
                start: 0,
                end: 1,
            },
            ChunkSpec {
                index: 1,
                start: 2,
                end: 3,
            },
            ChunkSpec {
                index: 2,
                start: 4,
                end: 5,
            },
            ChunkSpec {
                index: 3,
                start: 6,
                end: 7,
            },
        ];
        prepare_assembly_file(&temp_path, &chunks, 3).unwrap();
        assert_eq!(std::fs::read(&temp_path).unwrap(), b"abcdef");
        std::fs::remove_file(temp_path).unwrap();
    }

    #[test]
    fn model_path_and_storage_leases_are_mutually_exclusive() {
        let path = unique_test_path("lease");
        let lease = acquire_model_path(&path).unwrap();
        assert!(acquire_model_path(&path).is_err());
        assert!(acquire_model_storage().is_err());
        drop(lease);

        let storage = acquire_model_storage().unwrap();
        assert!(acquire_model_path(&path).is_err());
        drop(storage);
        assert!(acquire_model_path(&path).is_ok());
    }

    #[test]
    fn parallel_state_is_reused_only_for_the_same_representation() {
        let temp_path = unique_test_path("parallel_state");
        let chunks = calculate_chunks(100);
        let state = load_or_create_parallel_state(
            &temp_path,
            100,
            Some("abc"),
            "https://example.test/model",
            Some("\"v1\""),
            &chunks,
        )
        .unwrap();
        assert_eq!(state.phase, ParallelPhase::Downloading);
        let first_chunk = parallel_chunk_path(&temp_path, 0);
        std::fs::write(&first_chunk, b"partial").unwrap();

        let reused = load_or_create_parallel_state(
            &temp_path,
            100,
            Some("abc"),
            "https://example.test/model",
            Some("\"v1\""),
            &chunks,
        )
        .unwrap();
        assert_eq!(reused.strong_etag.as_deref(), Some("\"v1\""));
        assert!(first_chunk.exists());

        let reset = load_or_create_parallel_state(
            &temp_path,
            100,
            Some("abc"),
            "https://example.test/model",
            Some("\"v2\""),
            &chunks,
        )
        .unwrap();
        assert_eq!(reset.strong_etag.as_deref(), Some("\"v2\""));
        assert!(!first_chunk.exists());
        cleanup_parallel_artifacts(
            &temp_path,
            &parallel_state_path(&temp_path),
            &chunks,
        )
        .unwrap();
    }

    #[test]
    fn rejects_catalog_and_remote_size_mismatch_before_downloading() {
        assert_eq!(resolve_download_size(100, 100).unwrap(), 100);
        assert_eq!(resolve_download_size(0, 100).unwrap(), 100);
        assert_eq!(resolve_download_size(100, 0).unwrap(), 100);
        let error = resolve_download_size(100, 99).unwrap_err();
        assert!(error.starts_with("sayit_error:download_source_mismatch:"));
    }

    #[test]
    fn parses_only_valid_unsatisfied_range_totals() {
        assert_eq!(parse_unsatisfied_range_total(Some("bytes */123")), Some(123));
        assert_eq!(parse_unsatisfied_range_total(Some("bytes 1-2/123")), None);
        assert_eq!(parse_unsatisfied_range_total(Some("bytes */*")), None);
        assert_eq!(parse_unsatisfied_range_total(None), None);
    }

    #[test]
    fn test_calculate_chunks_edge_cases() {
        assert!(calculate_chunks(0).is_empty());

        let chunks = calculate_chunks(100);
        assert!(!chunks.is_empty());
        assert_eq!(chunks.first().unwrap().start, 0);
        assert_eq!(chunks.last().unwrap().end, 99);

        // 验证各分片连续且无遗漏无重叠
        for i in 1..chunks.len() {
            assert_eq!(chunks[i].start, chunks[i - 1].end + 1);
        }
    }

    #[test]
    fn test_calculate_chunks_large_model() {
        // 500MB 模型
        let total_size = 500 * 1024 * 1024;
        let chunks = calculate_chunks(total_size);
        assert!(chunks.len() >= 4 && chunks.len() <= 16);
        assert_eq!(chunks.first().unwrap().start, 0);
        assert_eq!(chunks.last().unwrap().end, total_size - 1);

        let mut sum_bytes = 0u64;
        for i in 0..chunks.len() {
            if i > 0 {
                assert_eq!(chunks[i].start, chunks[i - 1].end + 1);
            }
            sum_bytes += chunks[i].end - chunks[i].start + 1;
        }
        assert_eq!(sum_bytes, total_size);
    }

    #[test]
    fn test_validate_range_response_parts_cases() {
        // 1. 标准有效 206 匹配通过
        assert!(validate_range_response_parts(
            206,
            Some("bytes 0-99/1000"),
            Some(100),
            0,
            99,
            1000
        ).is_ok());

        // 2. HTTP 200 OK 必须被严格拒绝
        assert!(validate_range_response_parts(
            200,
            Some("bytes 0-99/1000"),
            Some(100),
            0,
            99,
            1000
        ).is_err());

        // 7. 已知文件大小时，未知、畸形或不匹配的 total 必须拒绝
        for header in ["bytes 0-99/*", "bytes 0-99/garbage", "bytes 0-99/999"] {
            assert!(validate_range_response_parts(206, Some(header), Some(100), 0, 99, 1000).is_err());
        }

        // 3. Content-Range 缺失必须拒绝
        assert!(validate_range_response_parts(
            206,
            None,
            Some(100),
            0,
            99,
            1000
        ).is_err());

        // 4. 起始 offset 错位必须拒绝
        assert!(validate_range_response_parts(
            206,
            Some("bytes 10-99/1000"),
            Some(90),
            0,
            99,
            1000
        ).is_err());

        // 5. 结束 offset 错位必须拒绝
        assert!(validate_range_response_parts(
            206,
            Some("bytes 0-199/1000"),
            Some(200),
            0,
            99,
            1000
        ).is_err());

        // 6. Content-Length 与区间不符必须拒绝
        assert!(validate_range_response_parts(
            206,
            Some("bytes 0-99/1000"),
            Some(99),
            0,
            99,
            1000
        ).is_err());
    }

    #[test]
    fn test_is_valid_resume_content_range_rfc9110() {
        // 1. 标准有效匹配（bytes 100-999/1000，请求 100-，覆盖到末尾 999，长度 900）
        assert!(is_valid_resume_content_range(
            Some("bytes 100-999/1000"),
            Some(900),
            100,
            1000
        ));

        // 2. 拒绝未覆盖到文件末尾的响应（例如 bytes 100-199/1000，end != total - 1）
        assert!(!is_valid_resume_content_range(
            Some("bytes 100-199/1000"),
            Some(100),
            100,
            1000
        ));

        // 3. 拒绝缺少 Content-Range 或畸形
        assert!(!is_valid_resume_content_range(None, Some(900), 100, 1000));
        assert!(!is_valid_resume_content_range(Some("invalid-header"), Some(900), 100, 1000));
        assert!(!is_valid_resume_content_range(Some("bytes 100-garbage/1000"), Some(900), 100, 1000));
        assert!(!is_valid_resume_content_range(Some("bytes 100-999/garbage"), Some(900), 100, 1000));

        // 4. 拒绝 total 为 *（断点续传未知 total 时强制全量重传）
        assert!(!is_valid_resume_content_range(Some("bytes 100-999/*"), Some(900), 100, 0));

        // 5. 拒绝 start 错位
        assert!(!is_valid_resume_content_range(Some("bytes 0-999/1000"), Some(1000), 100, 1000));
        assert!(!is_valid_resume_content_range(Some("bytes 50-999/1000"), Some(950), 100, 1000));

        // 6. 拒绝 Content-Length 与区间不符
        assert!(!is_valid_resume_content_range(
            Some("bytes 100-999/1000"),
            Some(899), // 应该是 900
            100,
            1000
        ));
    }

    #[test]
    fn test_single_stream_fallback_classification() {
        assert!(should_fallback_to_single_stream(
            "sayit_error:download_range_invalid: bad Content-Range"
        ));
        assert!(should_fallback_to_single_stream(
            "sayit_error:download_source_changed: ETag changed"
        ));
        assert!(should_fallback_to_single_stream(
            "sayit_error:download_parallel_verify_failed: bytes mismatch"
        ));
        assert!(should_fallback_to_single_stream(
            "sayit_error:download_checksum: SHA-256 mismatch"
        ));

        // 普通网络中断必须保留 chunk，等待下次从现有长度续传，不能清空后全量重下。
        assert!(!should_fallback_to_single_stream(
            "sayit_error:download_network: connection reset"
        ));
        assert!(!should_fallback_to_single_stream(
            "sayit_error:download_no_space: No space on disk"
        ));
        assert!(!should_fallback_to_single_stream(
            "sayit_error:download_permission: Access denied"
        ));
    }

    #[test]
    fn test_verify_file_sha256() {
        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("sayit_test_sha256.tmp");

        // 写入测试数据 "hello world\n" (SHA-256: a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447)
        std::fs::write(&test_file, b"hello world\n").unwrap();

        // 匹配成功
        assert!(verify_file_sha256(&test_file, "a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447").is_ok());
        // 大小写不敏感匹配
        assert!(verify_file_sha256(&test_file, "A948904F2F0F479B8F8197694B30184B0D2ED1C1CD2A1EC0FB85D299A192A447").is_ok());

        // hash 不匹配生成前端可识别的 sayit_error:download_checksum: 错误代码
        let mismatch_err = verify_file_sha256(&test_file, "0000000000000000000000000000000000000000000000000000000000000000").unwrap_err();
        assert!(is_checksum_error(&mismatch_err));

        // 验证非前缀的文本（即便是包含关键字）不会被生产分类器误判
        let wrapped_err = format!("other_wrapper: {}", mismatch_err);
        assert!(!is_checksum_error(&wrapped_err));

        let mismatch_err = verify_temp_file_sha256(&test_file, "0").unwrap_err();
        assert!(is_checksum_error(&mismatch_err));
        assert!(!test_file.exists());
    }

    #[test]
    fn test_verify_temp_file_sha256_preserves_io_error() {
        let test_dir = std::env::temp_dir().join(format!("sayit_sha256_dir_{}", std::process::id()));
        std::fs::create_dir(&test_dir).unwrap();

        let err = verify_temp_file_sha256(&test_dir, "0").unwrap_err();
        assert!(!is_checksum_error(&err));
        assert!(test_dir.is_dir());

        std::fs::remove_dir(test_dir).unwrap();
    }

    // [自訂] SC-35：用與下載、續傳相同的落盤路徑驗證替換與失敗保留。
    #[test]
    fn prepares_stream_partial_without_touching_existing_weight_until_verified() {
        use std::fs;
        let dir = std::env::temp_dir().join(format!("sayit-download-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let dest = dir.join("weight.gguf");
        let partial = dir.join("weight.gguf.part");
        let sha = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let prepare = || prepare_stream_partial(&partial, &dest, 3, sha);
        fs::write(&dest, b"original").unwrap();

        assert_eq!(prepare().unwrap(), Some(0));
        fs::write(&partial, b"ab").unwrap();
        assert_eq!(prepare().unwrap(), Some(2));
        fs::write(&partial, b"abcd").unwrap();
        assert_eq!(prepare().unwrap(), Some(0));
        assert!(!partial.exists());
        fs::write(&partial, b"xyz").unwrap();
        assert_eq!(prepare().unwrap(), Some(0));
        assert!(!partial.exists());
        assert_eq!(fs::read(&dest).unwrap(), b"original");

        fs::write(&partial, b"abc").unwrap();
        assert_eq!(prepare().unwrap(), None);
        assert_eq!(fs::read(&dest).unwrap(), b"abc");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn verified_download_replaces_existing_file_only_after_validation() {
        use std::fs;
        let dir = std::env::temp_dir().join(format!("sayit-download-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let dest = dir.join("weight.gguf");
        let partial = dir.join("weight.gguf.part");
        let sha = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let finalize = || finalize_download(&partial, &dest, 3, sha, "download_source_mismatch");
        fs::write(&dest, b"original").unwrap();

        assert!(finalize().is_err()); // 沒有檔案可完成。
        assert_eq!(fs::read(&dest).unwrap(), b"original");

        fs::write(&partial, b"ab").unwrap();
        assert!(finalize().unwrap_err().starts_with("sayit_error:download_source_mismatch:"));
        assert_eq!(fs::read(&dest).unwrap(), b"original");

        fs::write(&partial, b"xyz").unwrap();
        assert!(is_checksum_error(&finalize().unwrap_err()));
        assert!(!partial.exists());
        assert_eq!(fs::read(&dest).unwrap(), b"original");

        fs::write(&partial, b"abc").unwrap();
        finalize().unwrap();
        assert!(!partial.exists());
        assert_eq!(fs::read(&dest).unwrap(), b"abc");
        fs::remove_dir_all(dir).unwrap();
    }

    // [自訂] SC-36：目錄項必須逐字符合 catalog；原路徑能開啟檔案仍不足。
    #[test]
    fn finalized_download_repairs_existing_filename_case() {
        use std::fs;
        let dir = std::env::temp_dir().join(format!("sayit-download-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let dest = dir.join("weight.gguf");
        fs::write(dir.join("weight.GGUF"), b"old").unwrap();
        if !dest.exists() {
            fs::remove_dir_all(dir).unwrap();
            return;
        }
        let partial = dir.join("weight.gguf.part");
        fs::write(&partial, b"abc").unwrap();
        finalize_download(
            &partial,
            &dest,
            3,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "download_source_mismatch",
        ).unwrap();

        let names: Vec<_> = fs::read_dir(&dir).unwrap()
            .map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(names, [std::ffi::OsString::from("weight.gguf")]);
        assert_eq!(fs::read(&dest).unwrap(), b"abc");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reusing_verified_weight_repairs_filename_case_without_downloading() {
        use std::fs;
        let dir = std::env::temp_dir().join(format!("sayit-download-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let dest = dir.join("weight.gguf");
        fs::write(dir.join("weight.GGUF"), b"abc").unwrap();
        if !dest.exists() {
            fs::remove_dir_all(dir).unwrap();
            return;
        }
        assert!(reuse_verified_download(
            &dest,
            3,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ).unwrap());

        let names: Vec<_> = fs::read_dir(&dir).unwrap()
            .map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(names, [std::ffi::OsString::from("weight.gguf")]);
        assert_eq!(fs::read(&dest).unwrap(), b"abc");
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_download_artifacts_linked_to_existing_weight() {
        use std::fs;
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!("sayit-download-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let dest = dir.join("weight.gguf");
        let partial = dir.join("weight.gguf.part");
        fs::write(&dest, b"original").unwrap();

        symlink(&dest, &partial).unwrap();
        assert!(check_download_artifact(&partial, Some(&dest)).is_err());
        fs::remove_file(&partial).unwrap();

        fs::hard_link(&dest, &partial).unwrap();
        assert!(check_download_artifact(&partial, Some(&dest)).is_err());
        fs::remove_file(&partial).unwrap();

        fs::rename(&dest, &partial).unwrap();
        symlink(&partial, &dest).unwrap();
        assert!(check_download_artifact(&partial, Some(&partial)).is_err());
        assert_eq!(fs::read(&dest).unwrap(), b"original");
        fs::remove_dir_all(dir).unwrap();
    }
}
