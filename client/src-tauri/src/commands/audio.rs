use base64::{engine::general_purpose::STANDARD, Engine};
use std::fs;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard, TryLockError};

// [自訂] S61 / S71：播放、寫入、備份與清理使用相同的檔案鎖。
static RECORDING_FILES: RwLock<()> = RwLock::new(());
type PlaybackLeases = std::collections::HashMap<String, (PathBuf, std::time::Instant)>;
static PLAYING_RECORDINGS: LazyLock<Mutex<PlaybackLeases>> =
    LazyLock::new(|| Mutex::new(PlaybackLeases::new()));

pub(crate) fn lock_recordings() -> RwLockReadGuard<'static, ()> {
    RECORDING_FILES.read().unwrap_or_else(|e| e.into_inner())
}

pub(super) fn lock_recordings_for_deletion() -> Result<RwLockWriteGuard<'static, ()>, String> {
    RECORDING_FILES.try_write().map_err(|_| "Recordings are in use".into())
}

// [自訂] S71：播放器把檔案讀入記憶體後，仍在播放期間保護原錄音。
#[tauri::command]
pub async fn protect_playing_recording(file_path: String) -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(move || protect_recording(&file_path))
        .await.map_err(|e| e.to_string())?
}

fn protect_recording(file_path: &str) -> Result<Option<String>, String> {
    let _files = lock_recordings();
    let Some(path) = audio_file_in_dir(file_path) else { return Ok(None) };
    let token = uuid::Uuid::new_v4().to_string();
    let mut playing = PLAYING_RECORDINGS.lock().unwrap_or_else(|e| e.into_inner());
    playing.insert(token.clone(), (path, std::time::Instant::now()));
    Ok(Some(token))
}

#[tauri::command]
pub fn release_playing_recording(token: String) {
    PLAYING_RECORDINGS.lock().unwrap_or_else(|e| e.into_inner()).remove(&token);
}

fn recording_is_playing(path: &std::path::Path) -> bool {
    let Ok(path) = path.canonicalize() else { return false };
    let mut playing = PLAYING_RECORDINGS.lock().unwrap_or_else(|e| e.into_inner());
    // 前端崩潰可能收不到停止通知；播放中的視窗會定期延長保護時間。
    playing.retain(|_, (_, start)| start.elapsed() < std::time::Duration::from_secs(30 * 60));
    playing.values().any(|(active, _)| *active == path)
}

#[tauri::command]
pub fn renew_playing_recording(token: String) {
    if let Some((_, start)) = PLAYING_RECORDINGS.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&token) {
        *start = std::time::Instant::now();
    }
}

fn audio_dir() -> PathBuf {
    super::app_data_dir().join("audio")
}

/// id 会拼进文件名，带 `..\` 就能写到录音目录外面去。
fn check_record_id(id: &str) -> Result<(), String> {
    if crate::path_guard::is_record_id(id) {
        Ok(())
    } else {
        Err("Invalid recording id".to_string())
    }
}

/// 读、删、判存在只认录音目录里的 .wav。历史记录里的路径会被导入的备份改写，
/// 不能当成可信输入（见 backup.rs 的 rewrite_audio_path）。
fn audio_file_in_dir(file_path: &str) -> Option<PathBuf> {
    let path = PathBuf::from(file_path);
    if !crate::path_guard::has_extension(&path, &["wav"]) {
        return None;
    }
    crate::path_guard::existing_file_in_dir(&path, &audio_dir())
}

#[tauri::command]
pub async fn save_audio_file(id: String, wav_base64: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let _files = lock_recordings();
        check_record_id(&id)?;
        let dir = audio_dir();
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

        let bytes = STANDARD.decode(&wav_base64).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{}.wav", id));
        fs::write(&path, bytes).map_err(|e| e.to_string())?;

        Ok(path.to_string_lossy().to_string())
    }).await.map_err(|e| e.to_string())?
}

/// 接收 PCM Int16 LE 原始数据（base64），在 Rust 侧编码 WAV header 并写入文件。
/// 避免前端拼 WAV + base64 编码的开销。
#[tauri::command]
pub async fn save_pcm_as_wav(id: String, pcm_base64: String, sample_rate: Option<u32>, app: tauri::AppHandle) -> Result<String, String> {
    use tauri::Manager;
    tokio::task::spawn_blocking(move || {
        let pcm = STANDARD.decode(&pcm_base64).map_err(|e| e.to_string())?;
        write_pcm_wav(&id, &pcm, sample_rate, &app.state())
    }).await.map_err(|e| e.to_string())?
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavePcmMeta {
    id: String,
    sample_rate: Option<u32>,
}

/// 同 save_pcm_as_wav，但 PCM 以原始字节发过来（见 ipc_body）。录完每次都走这里。
#[tauri::command]
pub async fn save_pcm_as_wav_raw(request: tauri::ipc::Request<'_>, app: tauri::AppHandle) -> Result<String, String> {
    use tauri::Manager;
    let (meta, pcm): (SavePcmMeta, Vec<u8>) = crate::ipc_body::split_framed(&request)?;
    tokio::task::spawn_blocking(move || {
        write_pcm_wav(&meta.id, &pcm, meta.sample_rate, &app.state())
    }).await.map_err(|e| e.to_string())?
}

fn write_pcm_wav(id: &str, pcm: &[u8], sample_rate: Option<u32>, storage: &crate::storage::Storage) -> Result<String, String> {
    let _files = lock_recordings();
    // [自訂] S61：停用後不接受先前已排隊、尚未開始落盤的錄音。
    let _retention = super::storage::lock_history_retention();
    if storage.get("historyEnabled", None).as_bool() != Some(true)
        || storage.get("audioRetentionEnabled", None).as_bool() != Some(true)
    {
        return Err("Recording retention is disabled".into());
    }
    check_record_id(id)?;
    let dir = audio_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let sr = sample_rate.unwrap_or(16000);
    let data_len = pcm.len() as u32;

    // Build 44-byte WAV header + PCM data
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());   // chunk size
    wav.extend_from_slice(&1u16.to_le_bytes());    // PCM format
    wav.extend_from_slice(&1u16.to_le_bytes());    // mono
    wav.extend_from_slice(&sr.to_le_bytes());      // sample rate
    wav.extend_from_slice(&(sr * 2).to_le_bytes()); // byte rate
    wav.extend_from_slice(&2u16.to_le_bytes());    // block align
    wav.extend_from_slice(&16u16.to_le_bytes());   // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);

    let path = dir.join(format!("{}.wav", id));
    fs::write(&path, &wav).map_err(|e| e.to_string())?;

    Ok(path.to_string_lossy().to_string())
}

/// 录音文件还在不在。
///
/// 单独一个命令而不是复用 `read_audio_file`：后者会把整份 WAV 读出来再 base64
/// （五分钟录音约 13MB 的字符串），只为判断"文件存不存在"的话代价太大 ——
/// 历史列表里每条记录都要判一次。
#[tauri::command]
pub fn audio_file_exists(file_path: String) -> bool {
    audio_file_in_dir(&file_path).is_some()
}

/// 目录外的路径按"文件不在"处理，与原来文件缺失时的表现一致，界面显示"录音已不存在"。
#[tauri::command]
pub async fn read_audio_file(file_path: String) -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(move || {
        let _files = lock_recordings();
        let Some(path) = audio_file_in_dir(&file_path) else {
            return Ok(None);
        };
        let bytes = fs::read(&path).map_err(|e| e.to_string())?;
        Ok(Some(STANDARD.encode(&bytes)))
    }).await.map_err(|e| e.to_string())?
}

/// 同 read_audio_file，但直接返回字节（前端拿到 ArrayBuffer），不转 base64。
/// 五分钟录音转 base64 是 13 MB 的字符串，前端还要再解一遍；点播放、重新识别、提交纠错都走这里。
/// 文件不在 / 不在录音目录里时返回空字节。
#[tauri::command]
pub async fn read_audio_file_bytes(file_path: String) -> Result<tauri::ipc::Response, String> {
    tokio::task::spawn_blocking(move || {
        let _files = lock_recordings();
        let bytes = match audio_file_in_dir(&file_path) {
            Some(path) => fs::read(&path).map_err(|e| e.to_string())?,
            None => Vec::new(),
        };
        Ok(tauri::ipc::Response::new(bytes))
    }).await.map_err(|e| e.to_string())?
}

/// 把一条录音复制到「下载」文件夹。原来是前端读成 base64、再整份发回 Rust 写文件。
#[tauri::command]
pub async fn save_recording_to_downloads(file_path: String, filename: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let _files = lock_recordings();
        if !crate::path_guard::is_plain_file_name(&filename, "wav") {
            return Err("Invalid audio file name".to_string());
        }
        let source = audio_file_in_dir(&file_path).ok_or_else(|| "The recording no longer exists".to_string())?;
        let downloads = dirs::download_dir().ok_or_else(|| "Could not locate the Downloads directory".to_string())?;
        let dest = downloads.join(&filename);
        fs::copy(&source, &dest).map_err(|e| format!("Failed to write file: {}", e))?;
        let path_str = dest.to_string_lossy().to_string();
        crate::commands::system::write_log_line(&format!("[INFO] [audio] Audio saved to {}", path_str));
        Ok(path_str)
    }).await.map_err(|e| e.to_string())?
}

/// 目录外的路径静默忽略：删除是清理动作，删不到别人的文件就是正确结果。
#[tauri::command]
pub async fn delete_audio_file(file_path: String) -> Result<(), String> {
    // [自訂] S71：個別刪除也等備份或讀取完成，避免打包中途少了錄音。
    tokio::task::spawn_blocking(move || {
        let _files = wait_for_recording_deletion(&RECORDING_FILES);
        audio_file_in_dir(&file_path).map_or(Ok(()), |path| remove_recording(&path))
    }).await.map_err(|e| e.to_string())?
}

// [自訂] REV9：不排入寫者佇列，備份讀鎖持有期間仍允許新的讀取。
fn wait_for_recording_deletion(lock: &RwLock<()>) -> RwLockWriteGuard<'_, ()> {
    loop {
        match lock.try_write() {
            Ok(guard) => return guard,
            Err(TryLockError::Poisoned(error)) => return error.into_inner(),
            Err(TryLockError::WouldBlock) => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
}

// [自訂] S61 / S71：只列舉錄音根目錄的普通 WAV，不跟隨符號連結。
fn recording_paths(dir: &std::path::Path) -> std::io::Result<Vec<PathBuf>> {
    match fs::symlink_metadata(dir) {
        Ok(info) if info.is_dir() => {}
        Ok(_) => return Err(std::io::Error::other("Recording directory is not a regular directory")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    }
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() && crate::path_guard::has_extension(&entry.path(), &["wav"]) {
            paths.push(entry.path());
        }
    }
    Ok(paths)
}

pub(super) fn retained_recordings() -> Result<Vec<PathBuf>, String> {
    recording_paths(&audio_dir()).map_err(|e| e.to_string())
}

pub(super) fn remove_recording(path: &std::path::Path) -> Result<(), String> {
    if recording_is_playing(path) {
        return Err("Recording is playing".into());
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

pub(crate) fn cleanup_expired_recordings(days: i64) {
    let Some(age) = u64::try_from(days).ok().and_then(|days| days.checked_mul(86_400)) else {
        return;
    };
    let Some(cutoff) = std::time::SystemTime::now().checked_sub(std::time::Duration::from_secs(age)) else {
        return;
    };
    // 清理不等正在讀寫、複製或打包的錄音，下次排程再試。
    cleanup_recordings_in(&audio_dir(), cutoff, &RECORDING_FILES);
}

fn cleanup_recordings_in(dir: &std::path::Path, cutoff: std::time::SystemTime, guard: &RwLock<()>) {
    let Ok(_files) = guard.try_write() else { return };
    if let Ok(paths) = recording_paths(dir) {
        for path in paths {
            if fs::symlink_metadata(&path).and_then(|m| m.modified()).is_ok_and(|mtime| mtime < cutoff) {
                let _ = remove_recording(&path);
            }
        }
    }
}

#[cfg(test)]
mod retention_tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    #[test]
    fn waiting_delete_does_not_queue_a_writer_ahead_of_new_readers() {
        let dir = std::env::temp_dir().join(format!("sayit-delete-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let file = dir.join("pending.wav");
        fs::write(&file, b"recording").unwrap();
        let lock = RwLock::new(());
        let original = lock.read().unwrap();
        let (started, waiting) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let (lock_ref, file_ref) = (&lock, &file);
            let worker = scope.spawn(move || {
                started.send(()).unwrap();
                let _guard = wait_for_recording_deletion(lock_ref);
                remove_recording(file_ref).unwrap();
            });
            waiting.recv().unwrap();
            std::thread::sleep(Duration::from_millis(60));
            let new_reader = lock.try_read();
            assert!(file.exists());
            drop(original);
            assert!(new_reader.is_ok());
            drop(new_reader);
            worker.join().unwrap();
        });
        assert!(!file.exists());
        fs::remove_dir(dir).unwrap();
    }

    // [自訂] S71：使用中的錄音延至下次清理，近期錄音與模型目錄保留。
    #[test]
    fn retention_skips_busy_audio_and_only_removes_expired_recordings() {
        let dir = std::env::temp_dir().join(format!("sayit-audio-test-{}", uuid::Uuid::new_v4()));
        let models = dir.join("models");
        fs::create_dir_all(&models).unwrap();
        let expired = dir.join("expired.wav");
        let recent = dir.join("recent.wav");
        let model = models.join("model.gguf");
        fs::write(&expired, b"audio").unwrap();
        fs::write(&recent, b"audio").unwrap();
        fs::write(&model, b"model").unwrap();
        let now = SystemTime::now();
        fs::OpenOptions::new().write(true).open(&expired).unwrap()
            .set_times(fs::FileTimes::new().set_modified(now - Duration::from_secs(3600))).unwrap();
        let cutoff = now - Duration::from_secs(1800);
        let guard = RwLock::new(());
        let playback = guard.read().unwrap();
        cleanup_recordings_in(&dir, cutoff, &guard);
        assert!(expired.exists());
        drop(playback);
        let token = uuid::Uuid::new_v4().to_string();
        PLAYING_RECORDINGS.lock().unwrap().insert(token.clone(), (
            expired.canonicalize().unwrap(),
            std::time::Instant::now(),
        ));
        cleanup_recordings_in(&dir, cutoff, &guard);
        assert!(expired.exists());
        release_playing_recording(token);
        cleanup_recordings_in(&dir, cutoff, &guard);
        assert!(!expired.exists());
        assert!(recent.exists());
        assert!(model.exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
