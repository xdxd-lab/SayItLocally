use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, State};
use crate::storage::Storage;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use base64::Engine;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

/// Persistent log file writer — opened once, reused across calls.
static LOG_FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

const MAX_LOG_SIZE: u64 = 5 * 1024 * 1024;
// [自訂] REV9：總量上限包含目前及舊版日誌。一般用量仍依設定天數保留。
const MAX_TOTAL_LOG_SIZE: u64 = 512 * 1024 * 1024;
const CURRENT_LOG: &str = "sayit.log";

pub(super) fn log_dir() -> PathBuf {
    super::app_data_dir().join("logs")
}

fn log_file_path() -> PathBuf {
    log_dir().join(CURRENT_LOG)
}

// 接受舊版 1～3，或日期＋序號的輪替檔名。
fn rotation_id(name: &str) -> Option<(Option<chrono::NaiveDate>, u32)> {
    let suffix = name.strip_prefix("sayit.")?.strip_suffix(".log")?;
    if matches!(suffix, "1" | "2" | "3") {
        return Some((None, 4 - suffix.parse::<u32>().ok()?));
    }
    let (date, number) = suffix.split_once('.')?;
    let day = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let sequence = number.parse::<u32>().ok()?;
    (date.len() == 10 && number.len() == 10 && number.bytes().all(|b| b.is_ascii_digit()))
        .then_some((Some(day), sequence))
}

// [自訂] REV9：清理、預覽、匯出及檢視採同一個名單，依新到舊排序。
pub(super) fn rotated_logs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut logs: Vec<_> = entries.flatten().filter_map(|entry| {
        let name = entry.file_name();
        let (date, sequence) = rotation_id(name.to_str()?)?;
        let metadata = std::fs::symlink_metadata(entry.path()).ok()?;
        if !metadata.is_file() {
            return None;
        }
        let modified = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
        let modified = chrono::DateTime::<chrono::Local>::from(modified);
        Some((date.unwrap_or(modified.date_naive()), date.is_some(), sequence, modified, entry.path()))
    }).collect();
    logs.sort_unstable_by(|a, b| b.cmp(a));
    logs.into_iter().map(|(_, _, _, _, path)| path).collect()
}

pub(super) fn log_paths(dir: &Path) -> impl Iterator<Item = PathBuf> {
    let active = dir.join(CURRENT_LOG);
    let active = std::fs::symlink_metadata(&active)
        .is_ok_and(|metadata| metadata.is_file()).then_some(active);
    active.into_iter().chain(rotated_logs(dir))
}

fn open_log_file() -> Option<std::fs::File> {
    let dir = log_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("[log] failed to create log dir {:?}: {}", dir, e);
        return None;
    }
    let path = log_file_path();
    if std::fs::symlink_metadata(&path).is_ok_and(|metadata| !metadata.is_file()) {
        return None;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()
}

// [自訂] S71：日期改變或超過大小時輪替；不覆寫任何舊的日誌。
fn rotate_if_needed(
    dir: &Path,
    file: &mut Option<std::fs::File>,
    now: chrono::DateTime<chrono::Local>,
    incoming: u64,
) -> bool {
    let path = dir.join(CURRENT_LOG);
    let Ok(metadata) = std::fs::symlink_metadata(&path) else { return false };
    let Ok(modified) = metadata.modified() else { return false };
    if !metadata.is_file() || metadata.len() == 0 {
        return false;
    }
    let previous_date = chrono::DateTime::<chrono::Local>::from(modified).date_naive();
    if previous_date == now.date_naive()
        && metadata.len().saturating_add(incoming) <= MAX_LOG_SIZE
    {
        return false;
    }
    let prefix = format!("sayit.{previous_date}");
    let next_sequence = rotated_logs(dir).iter().find_map(|path| {
        let (date, sequence) = rotation_id(path.file_name()?.to_str()?)?;
        (date == Some(previous_date)).then_some(sequence)
    }).unwrap_or(0).saturating_add(1);
    for sequence in next_sequence..=u32::MAX {
        let target = dir.join(format!("{prefix}.{sequence:010}.log"));
        match std::fs::symlink_metadata(&target) {
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                eprintln!("[log] failed to inspect rotation destination: {error}");
                return false;
            }
        }
        *file = None; // Windows: close the active file before renaming.
        if let Err(error) = std::fs::rename(&path, target) {
            eprintln!("[log] failed to rotate log: {error}");
            return false;
        }
        return true;
    }
    false
}

pub(crate) fn log_retention_days(storage: &Storage) -> Result<i64, String> {
    let days = storage.retention_days("logRetentionDays", 30)?;
    if days <= 0 || matches!(days, 7 | 15 | 30 | 90) {
        Ok(days)
    } else {
        Err("Invalid log retention days".into())
    }
}

pub(crate) fn cleanup_retained_logs(days: i64) {
    let mut file = LOG_FILE.lock().unwrap_or_else(|e| e.into_inner());
    maintain_logs(&log_dir(), &mut file, chrono::Local::now(), days, MAX_TOTAL_LOG_SIZE);
}

fn cap_log_size(dir: &Path, limit: u64, pending_bytes: u64) {
    let logs: Vec<_> = log_paths(dir).filter_map(|path| {
        let size = std::fs::symlink_metadata(&path).ok()?.len();
        Some((path, size))
    }).collect();
    let mut total = logs.iter().fold(pending_bytes, |total, (_, size)| total.saturating_add(*size));
    for (path, size) in logs.into_iter().rev() {
        if total <= limit || path.file_name().is_some_and(|name| name == CURRENT_LOG) {
            break;
        }
        match std::fs::remove_file(path) {
            Ok(()) => total -= size,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => total -= size,
            Err(error) => {
                eprintln!("[log] failed to apply log size limit: {error}");
                break;
            }
        }
    }
}

// [自訂] S71：輪替和清理與寫入使用同一把鎖。舊版混合多日的日誌也逐行套用保留期。
fn maintain_logs(
    dir: &std::path::Path,
    file: &mut Option<std::fs::File>,
    now: chrono::DateTime<chrono::Local>,
    days: i64,
    total_limit: u64,
) {
    if !std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir()) {
        return;
    }
    // 前次程序中斷可能留下完成一半的保留期暫存檔。
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let staging = name.to_str()
                .and_then(|name| name.strip_prefix(".sayit-retention-"))
                .and_then(|name| name.strip_suffix(".tmp"))
                .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok());
            if staging && entry.file_type().is_ok_and(|kind| kind.is_file()) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    rotate_if_needed(dir, file, now, 0);
    let cutoff = (days > 0).then(|| chrono::Duration::try_days(days))
        .flatten().and_then(|age| now.checked_sub_signed(age));
    if let Some(cutoff) = cutoff {
        trim_logs(dir, file, cutoff);
    }
    cap_log_size(dir, total_limit, 0);
}

fn trim_logs(dir: &Path, file: &mut Option<std::fs::File>, cutoff: chrono::DateTime<chrono::Local>) {
    let cutoff_day = cutoff.date_naive();
    let cutoff = cutoff.format("%Y-%m-%d %H:%M:%S%.3f").to_string();
    for path in log_paths(dir) {
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
        if let Some((Some(day), _)) = rotation_id(name) {
            // [自訂] REV9：比截止日新的封存整檔保留，只需逐行檢查邊界日。
            if day > cutoff_day {
                continue;
            }
            // 日期是輪替前最後寫入時間，早於截止日的整個封存檔都已到期。
            if day < cutoff_day {
                if let Err(error) = std::fs::remove_file(&path) {
                    eprintln!("[log] failed to remove expired log: {error}");
                }
                continue;
            }
        }
        let modified = std::fs::symlink_metadata(&path).and_then(|metadata| metadata.modified());
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let content = String::from_utf8_lossy(&bytes);
        let kept = super::diagnostics::filter_raw_log_since(&content, Some(&cutoff));
        if kept.as_bytes() == bytes {
            continue;
        }
        if name == CURRENT_LOG {
            *file = None;
        }
        let result = if kept.is_empty() {
            std::fs::remove_file(&path)
        } else {
            let temp = dir.join(format!(".sayit-retention-{}.tmp", uuid::Uuid::new_v4()));
            let result = (|| {
                let mut staged = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
                staged.write_all(kept.as_bytes())?;
                if let Ok(modified) = modified {
                    staged.set_times(std::fs::FileTimes::new().set_modified(modified))?;
                }
                drop(staged);
                std::fs::rename(&temp, &path)
            })();
            if result.is_err() {
                let _ = std::fs::remove_file(&temp);
            }
            result
        };
        if let Err(e) = result {
            eprintln!("[log] failed to apply log retention: {e}");
        }
    }
}

pub fn write_log_line(line: &str) {
    write_log_line_at(line, chrono::Local::now());
}

fn write_log_line_at(line: &str, now: chrono::DateTime<chrono::Local>) {
    // 用 unwrap_or_else 而非 unwrap()：即使這個 Mutex 曾在某次 panic 中被"汙染"
    // （poisoned），日誌功能也不能跟著永久失效——否則會形成"一次意外 panic 導致
    // 之後所有診斷日誌都寫不出來"的雪崩，恰恰是排查間歇性問題最怕遇到的情況。
    let mut guard = LOG_FILE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let incoming = line.len() as u64 + 32;
    if incoming > MAX_LOG_SIZE {
        return;
    }
    let dir = log_dir();
    if rotate_if_needed(&dir, &mut guard, now, incoming) {
        cap_log_size(&dir, MAX_TOTAL_LOG_SIZE, MAX_LOG_SIZE);
    }
    if guard.is_none() {
        *guard = open_log_file();
    }
    if let Some(ref mut f) = *guard {
        let ts = now.format("%Y-%m-%d %H:%M:%S%.3f");
        let _ = writeln!(f, "[{}] {}", ts, line);
        let _ = f.flush();
    }
}

#[cfg(test)]
mod log_retention_tests {
    use super::*;
    use std::fs;

    #[test]
    fn frontend_log_lines_keep_their_order_and_arrival_time_off_the_caller_thread() {
        let (done, written) = std::sync::mpsc::channel();
        let queue = spawn_log_writer(DEBUG_LOG_QUEUE_CAP, move |at, line: String, dropped| {
            done.send((at, line, dropped, std::thread::current().id())).unwrap();
        })
        .unwrap();
        let base = chrono::Local::now() - chrono::Duration::seconds(30);
        for i in 0..50 {
            queue.push(base + chrono::Duration::milliseconds(i), format!("line {i}")).unwrap();
        }
        let rows: Vec<_> = written.iter().take(50).collect();
        for (i, (at, line, dropped, thread)) in rows.into_iter().enumerate() {
            assert_eq!(line, format!("line {i}"));
            assert_eq!(at, base + chrono::Duration::milliseconds(i as i64));
            assert_eq!(dropped, 0);
            assert_ne!(thread, std::thread::current().id());
        }
    }

    /// 寫入端卡在第一行時，再收的行都排在佇列裡；`release` 放行。
    fn gated_writer(
        cap: usize,
    ) -> (
        std::sync::Arc<LogQueue<u32>>,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Sender<()>,
        std::sync::Arc<std::sync::Mutex<Vec<(u32, u64)>>>,
    ) {
        let (started_tx, started) = std::sync::mpsc::channel();
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink_written = written.clone();
        let mut first = true;
        let queue = spawn_log_writer(cap, move |_, item: u32, dropped| {
            if first {
                first = false;
                started_tx.send(()).unwrap();
                gate.recv().unwrap();
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
            sink_written.lock().unwrap().push((item, dropped));
        })
        .unwrap();
        (queue, started, release, written)
    }

    #[test]
    fn flush_waits_for_queued_lines_and_gives_up_at_its_deadline() {
        let (queue, started, release, written) = gated_writer(DEBUG_LOG_QUEUE_CAP);
        let now = chrono::Local::now();
        queue.push(now, 0).unwrap();
        started.recv().unwrap();
        for i in 1..=20 {
            queue.push(now, i).unwrap();
        }
        // 寫入端卡住：flush 在時限到時放棄，不會無限等。
        let begun = std::time::Instant::now();
        assert!(!queue.flush(std::time::Duration::from_millis(100)));
        let waited = begun.elapsed();
        assert!(waited >= std::time::Duration::from_millis(100), "{waited:?}");
        assert!(waited < std::time::Duration::from_millis(1000), "{waited:?}");
        // 放行後 flush 返回時，之前收的每一行都已寫出。
        release.send(()).unwrap();
        assert!(queue.flush(std::time::Duration::from_secs(5)));
        let rows: Vec<u32> = written.lock().unwrap().iter().map(|(item, _)| *item).collect();
        assert_eq!(rows, (0..=20).collect::<Vec<_>>());
        // 佇列沒有積壓時立即返回。
        assert!(queue.flush(std::time::Duration::ZERO));
    }

    #[test]
    fn a_full_queue_drops_the_oldest_lines_and_reports_how_many() {
        let (queue, started, release, written) = gated_writer(3);
        let now = chrono::Local::now();
        queue.push(now, 0).unwrap();
        started.recv().unwrap();
        for i in 1..=5 {
            queue.push(now, i).unwrap();
        }
        release.send(()).unwrap();
        // 丟掉的行也算處理完，flush 不會因此等到逾時。
        assert!(queue.flush(std::time::Duration::from_secs(5)));
        assert_eq!(*written.lock().unwrap(), vec![(0, 0), (3, 2), (4, 0), (5, 0)]);
    }

    #[test]
    fn flushing_before_any_frontend_log_returns_at_once() {
        if DEBUG_LOG_QUEUE.get().is_none() {
            assert!(flush_debug_log(std::time::Duration::from_secs(5)));
        }
        append_debug_log(serde_json::json!({"kind": "runtime", "level": "info", "source": "t", "message": "flush"})).unwrap();
        assert!(flush_debug_log(std::time::Duration::from_secs(5)));
    }

    #[test]
    fn a_queued_frontend_line_is_stamped_with_its_arrival_time() {
        let marker = format!("queued-{}", uuid::Uuid::new_v4());
        let held = LOG_FILE.lock().unwrap_or_else(|e| e.into_inner());
        // 第一行讓寫入執行緒卡在日誌鎖上，第二行就在佇列裡排隊。
        append_debug_log(serde_json::json!({"kind": "runtime", "level": "info", "source": "t", "message": "ahead"})).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let called = chrono::Local::now();
        append_debug_log(serde_json::json!({"kind": "runtime", "level": "info", "source": "t", "message": marker})).unwrap();
        // 寫入執行緒被擋住時命令照樣立即返回；放開後才寫入。
        std::thread::sleep(std::time::Duration::from_millis(300));
        let released = chrono::Local::now();
        drop(held);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let line = loop {
            let text = fs::read_to_string(log_file_path()).unwrap_or_default();
            if let Some(line) = text.lines().find(|line| line.contains(&marker)) {
                break line.to_string();
            }
            assert!(std::time::Instant::now() < deadline, "queued line was not written");
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let stamp = chrono::NaiveDateTime::parse_from_str(&line[1..24], "%Y-%m-%d %H:%M:%S%.3f").unwrap();
        assert!(stamp >= called.naive_local() - chrono::Duration::milliseconds(1));
        assert!(stamp < released.naive_local() - chrono::Duration::milliseconds(200), "{line}");
    }

    #[test]
    fn frontend_payloads_format_as_before() {
        let runtime = serde_json::json!({"kind": "runtime", "level": "warn", "source": "audio", "message": "silent"});
        assert_eq!(format_debug_payload(runtime), "[WARN] [audio] silent");
        let session = serde_json::json!({"kind": "session_end", "sessionId": "s1", "durationMs": 12, "messageCount": 3});
        assert_eq!(format_debug_payload(session), "[SESSION] end id=s1 duration=12ms messages=3");
    }

    #[test]
    fn validates_log_retention_setting_and_routes_test_logger_to_temp() {
        let storage = Storage::new(PathBuf::from(":memory:")).unwrap();
        storage.set("logRetentionDays", &serde_json::json!(1)).unwrap();
        assert!(log_retention_days(&storage).is_err());
        for days in [0, -1, 7, 15, 30, 90] {
            storage.set("logRetentionDays", &serde_json::json!(days)).unwrap();
            assert_eq!(log_retention_days(&storage).unwrap(), days);
        }
        assert!(log_dir().starts_with(std::env::temp_dir()));
        write_log_line("[INFO] test log is isolated");
        assert!(fs::read_to_string(log_file_path()).unwrap().contains("test log is isolated"));
    }

    // [自訂] S71：日期與大小各自觸發輪替。
    #[test]
    fn rotates_logs_on_date_change_and_size_limit() {
        let dir = std::env::temp_dir().join(format!("sayit-log-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let active = dir.join("sayit.log");
        fs::write(&active, b"previous day").unwrap();
        let modified = chrono::DateTime::<chrono::Local>::from(fs::metadata(&active).unwrap().modified().unwrap());
        let mut file = Some(fs::File::open(&active).unwrap());
        rotate_if_needed(&dir, &mut file, modified + chrono::Duration::days(2), 0);
        assert!(file.is_none());
        assert_eq!(fs::read(&rotated_logs(&dir)[0]).unwrap(), b"previous day");

        let large = fs::File::create(&active).unwrap();
        large.set_len(MAX_LOG_SIZE).unwrap();
        let modified = chrono::DateTime::<chrono::Local>::from(large.metadata().unwrap().modified().unwrap());
        let mut file = Some(large);
        rotate_if_needed(&dir, &mut file, modified, 1);
        assert!(file.is_none());
        let archived = rotated_logs(&dir);
        assert_eq!(archived.len(), 2);
        assert_eq!(fs::metadata(&archived[0]).unwrap().len(), MAX_LOG_SIZE);
        assert_eq!(fs::read(&archived[1]).unwrap(), b"previous day");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn retention_trims_current_log_and_removes_expired_rotated_logs() {
        let dir = std::env::temp_dir().join(format!("sayit-log-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let active = dir.join("sayit.log");
        let now = chrono::Local::now();
        let line = |date: chrono::DateTime<chrono::Local>, marker| {
            format!("[{}] {marker}\n", date.format("%Y-%m-%d %H:%M:%S%.3f"))
        };
        let expired = line(now - chrono::Duration::days(10), "expired");
        let recent = line(now, "recent");
        fs::write(&active, format!("{expired}old stack trace\n{recent}")).unwrap();
        fs::write(dir.join("sayit.1.log"), expired).unwrap();
        let staging = dir.join(format!(".sayit-retention-{}.tmp", uuid::Uuid::new_v4()));
        fs::write(&staging, b"incomplete log rewrite").unwrap();
        let mut file = Some(fs::File::open(&active).unwrap());
        maintain_logs(&dir, &mut file, now, 7, MAX_TOTAL_LOG_SIZE);
        assert_eq!(fs::read_to_string(&active).unwrap(), recent);
        assert!(file.is_none());
        assert!(!dir.join("sayit.1.log").exists());
        assert!(!staging.exists());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn retention_trims_cutoff_date_and_keeps_entries_at_exact_cutoff() {
        use chrono::TimeZone;
        let dir = std::env::temp_dir().join(format!("sayit-log-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let cutoff = chrono::Local.with_ymd_and_hms(2026, 6, 15, 12, 0, 0).single().unwrap();
        let boundary = dir.join("sayit.2026-06-15.0000000001.log");
        let old = dir.join("sayit.2026-06-14.0000000001.log");
        let recent = dir.join("sayit.2026-06-16.0000000001.log");
        let at_cutoff = "[2026-06-15 12:00:00.000] [INFO] exact cutoff\n";
        fs::write(&boundary, format!("[2026-06-15 11:59:59.999] [INFO] expired\n{at_cutoff}")).unwrap();
        fs::write(&old, b"expired file").unwrap();
        fs::write(&recent, b"recent archive stays byte-for-byte").unwrap();
        trim_logs(&dir, &mut None, cutoff);
        assert_eq!(fs::read_to_string(boundary).unwrap(), at_cutoff);
        assert!(!old.exists());
        assert_eq!(fs::read(recent).unwrap(), b"recent archive stays byte-for-byte");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn keeps_more_than_four_dated_logs_and_caps_oldest_first_without_day_limit() {
        let dir = std::env::temp_dir().join(format!("sayit-log-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let now = chrono::Local::now();
        let active = dir.join(CURRENT_LOG);
        for age in (2..=7).rev() {
            let date = now - chrono::Duration::days(age);
            let mut file = fs::File::create(&active).unwrap();
            writeln!(file, "[{}] example", date.format("%Y-%m-%d %H:%M:%S%.3f")).unwrap();
            file.set_times(fs::FileTimes::new().set_modified(date.into())).unwrap();
            let mut file = Some(file);
            assert!(rotate_if_needed(&dir, &mut file, date + chrono::Duration::days(1), 0));
        }
        maintain_logs(&dir, &mut None, now, 15, MAX_TOTAL_LOG_SIZE);
        let full = rotated_logs(&dir);
        assert_eq!(full.len(), 6);
        maintain_logs(&dir, &mut None, now + chrono::Duration::days(100), 0, MAX_TOTAL_LOG_SIZE);
        assert_eq!(rotated_logs(&dir), full);
        let bytes_in_newest_two: u64 = full.iter().take(2)
            .map(|path| fs::metadata(path).unwrap().len()).sum();
        maintain_logs(&dir, &mut None, now, 0, bytes_in_newest_two);
        assert_eq!(rotated_logs(&dir), &full[..2]);
        fs::remove_dir_all(dir).unwrap();
    }

}

fn get_os_version() -> String {
    #[cfg(target_os = "windows")]
    {
        // 從登錄檔讀取，不彈視窗
        use std::process::Command;
        Command::new("cmd")
            .args(["/C", "ver"])
            .creation_flags(0x08000000)
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s: String| s.trim().to_string())
            .unwrap_or_else(|| "Windows".to_string())
    }
    #[cfg(target_os = "macos")]
    {
        crate::platform::macos::sysinfo::os_version()
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        whoami::distro()
    }
}

#[allow(dead_code)] // [自訂] S63：已不呼叫
fn get_local_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|addr| addr.ip().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

fn get_system_locale() -> String {
    // 用原生 Win32 API 取區域設定，避免拉起 PowerShell（冷啟動 1-2 秒）。
    // GetUserDefaultLocaleName 返回形如 "zh-CN" 的 BCP-47 名稱，與原 (Get-Culture).Name 一致。
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Globalization::GetUserDefaultLocaleName;
        // LOCALE_NAME_MAX_LENGTH = 85
        let mut buf = [0u16; 85];
        let len = unsafe { GetUserDefaultLocaleName(&mut buf) };
        if len > 1 {
            // 返回值含結尾的 NUL，切掉它。
            String::from_utf16_lossy(&buf[..(len as usize - 1)])
        } else {
            "unknown".to_string()
        }
    }
    // macOS：區域格式（CFLocale），不能讀 LANG（Finder 啟動的 App 沒有這個變數）。
    #[cfg(target_os = "macos")]
    {
        crate::platform::macos::sysinfo::locale_identifier().unwrap_or_else(|| "unknown".to_string())
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::env::var("LANG").unwrap_or_else(|_| "unknown".to_string())
    }
}

fn get_total_memory_mb() -> u64 {
    // 用原生 GlobalMemoryStatusEx 取實體記憶體，避免拉起 PowerShell。
    // ullTotalPhys 為位元組，/ 1MB 與原 TotalPhysicalMemory / 1MB 語義一致。
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut status = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        if unsafe { GlobalMemoryStatusEx(&mut status) }.is_ok() {
            status.ullTotalPhys / (1024 * 1024)
        } else {
            0
        }
    }
    #[cfg(target_os = "macos")]
    {
        crate::platform::macos::sysinfo::total_memory_mb()
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        0
    }
}

#[derive(Serialize)]
pub struct ClientRuntimeInfo {
    #[serde(rename = "userId")]
    pub user_id: String,
    #[serde(rename = "userName")]
    pub user_name: String,
    #[serde(rename = "deviceId")]
    pub device_id: String,
    pub hostname: String,
    #[serde(rename = "clientVersion")]
    pub client_version: String,
    pub platform: String,
    /// CPU 架構（std::env::consts::ARCH：x86_64 / aarch64），更新通道按它選安裝包。
    pub arch: String,
    #[serde(rename = "osVersion")]
    pub os_version: String,
    #[serde(rename = "localIp")]
    pub local_ip: String,
    #[serde(rename = "systemLocale")]
    pub system_locale: String,
    #[serde(rename = "cpuCores")]
    pub cpu_cores: usize,
    #[serde(rename = "memoryMb")]
    pub memory_mb: u64,
}

// [自訂] S63：本 fork 只用本機模式，這些欄位只會送往已停用的雲端服務。
// 不再讀取系統帳號、主機名、區網 IP（原本會對 8.8.8.8 做 UDP connect 選路由），
// 也不再產生並保存固定裝置 ID；一律回傳固定值。原實作保留在 get_local_ip 等函式與 git 歷史。
const REDACTED_CLIENT_FIELD: &str = "local";

#[tauri::command]
pub fn get_client_runtime_info(_storage: State<Storage>) -> Result<ClientRuntimeInfo, String> {
    let hostname = REDACTED_CLIENT_FIELD.to_string();
    let user_name = REDACTED_CLIENT_FIELD.to_string();
    let device_id = REDACTED_CLIENT_FIELD.to_string();

    Ok(ClientRuntimeInfo {
        user_id: user_name.clone(),
        user_name,
        device_id,
        hostname,
        client_version: env!("CARGO_PKG_VERSION").to_string(),
        platform: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        os_version: get_os_version(),
        local_ip: REDACTED_CLIENT_FIELD.to_string(), // [自訂] S63：原 get_local_ip()
        system_locale: get_system_locale(),
        cpu_cores: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        memory_mb: get_total_memory_mb(),
    })
}

/// 系統**顯示語言**，返回前端 `Locale` 用的標籤（`zh-CN` / `en`）。
///
/// 前端 `ui.language = 'auto'` 時靠這個定語言。刻意不讓前端用
/// `navigator.language` 自己判：那個跟的是 WebView 的語言，與托盤用的
/// `GetUserDefaultUILanguage` 可能給出不同結論，會出現「托盤中文、介面英文」。
/// 判定只保留一處（`locale::system_ui_lang`），兩邊都問它。
#[tauri::command]
pub fn get_system_ui_language() -> String {
    crate::locale::system_ui_lang().tag().to_string()
}

#[tauri::command]
pub fn get_auto_launch(app: AppHandle) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_auto_launch(app: AppHandle, _enable: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let autostart = app.autolaunch();
    if _enable {
        autostart.enable().map_err(|e| e.to_string())
    } else {
        autostart.disable().map_err(|e| e.to_string())
    }
}

// ─── 自動更新 ───
//
// 分工：版本檢查在前端（updateChecker.ts 拉 manifest 比版本號），Rust 只負責
// 下載、校驗完整性、以及把安裝程式拉起來。四個入口：
//   · download_update            下載並校驗 SHA-512
//   · verify_update_package      啟動時確認上次下載的包還在、還完整（省掉重複下載整包）
//   · install_downloaded_update  使用者主動點「立即更新」時裝，裝完重新拉起
//   · install_pending_update_on_exit  使用者沒點就直接退出時，在退出路徑上靜默裝掉
//
// 最後那個是"強制更新"真正落地的地方：只靠使用者點圖示，不點的人永遠留在舊版。

/// 安裝程式是否已經被拉起過。
///
/// 使用者主動安裝與退出兜底安裝共用同一個 spawn，必須互斥：install_downloaded_update
/// 自己就會 app.exit(0)，那次退出同樣會走 RunEvent::Exit，不攔住就會起兩個安裝程式
/// 互相搶檔案鎖。suppress_exit_install() 也靠它遮蔽掉"重啟回同一版本"那種退出。
static INSTALLER_SPAWNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 讓本次退出不要觸發兜底安裝。用於匯入配置後的重啟：使用者要的是重啟回同一個版本，
/// 不是更新；若在這裡把新版裝下去，重啟拉起來的會是正在被覆蓋的 exe。
pub fn suppress_exit_install() {
    INSTALLER_SPAWNED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// 計算檔案 SHA-512 並輸出 **Base64** —— 與 manifest 的 sha512 欄位同一種編碼。
/// （gen-latest-yml.ps1 把 Get-FileHash 的 hex 轉成了 Base64，別按 hex 去比。）
fn file_sha512_base64(path: &std::path::Path) -> Result<String, String> {
    use sha2::{Digest, Sha512};
    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("Failed to open the installer file: {}", e))?;
    let mut hasher = Sha512::new();
    // 安裝包有幾十 MB，分塊讀，別整個塞進記憶體
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = std::io::Read::read(&mut file, &mut buffer)
            .map_err(|e| format!("Failed to read the installer file: {}", e))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(hasher.finalize()))
}

/// 確認磁碟上那個安裝包仍然可用（存在 + 雜湊對得上）。
///
/// 啟動時用它決定「上次下載的包還能不能直接裝」。沒有這一步，"下載完等使用者點"
/// 會讓每次開機都把整包重下一遍 —— 舊流程下載完立刻安裝，所以從來沒暴露過。
#[tauri::command]
pub fn verify_update_package(file_path: String, sha512: Option<String>) -> Result<bool, String> {
    // 沒記雜湊的包（0.1.x 早期下載的）不再複用，寧可重下一遍：
    // 「只確認檔案在」正是惡意備份能把任意 exe 塞進待安裝的那個口子。
    let (Ok(path), Some(expected)) = (resolve_update_package(&file_path), non_empty(sha512.as_deref())) else {
        return Ok(false);
    };
    Ok(file_sha512_base64(&path)? == expected)
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// 更新包只放在這一個目錄裡。安裝前一律核對路徑確實在這裡面。
fn update_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("sayit-update")
}

/// 更新包的副檔名：Windows 是 NSIS 安裝程式，macOS 是裝著 SayIt.app 的 zip（見 spawn_installer）。
#[cfg(not(target_os = "macos"))]
const UPDATE_PACKAGE_EXTENSION: &str = "exe";
#[cfg(target_os = "macos")]
const UPDATE_PACKAGE_EXTENSION: &str = "zip";

/// 安裝包落盤用的檔名：只取 URL 路徑的最後一段，而且必須是普通的 `xxx.exe`（mac 上是 `xxx.zip`）。
/// 原來直接 `url.split('/').last()`，一個帶查詢串或奇怪字元的地址就能讓檔名跑偏。
fn installer_file_name(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let last = parsed.path_segments()?.last()?.to_string();
    let valid = last.len() <= 128
        && last.to_ascii_lowercase().ends_with(&format!(".{UPDATE_PACKAGE_EXTENSION}"))
        && !last.starts_with('.')
        && last.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    valid.then_some(last)
}

/// 把前端或設定裡給的安裝包路徑解析成真實路徑，並確認它在更新目錄裡、是 .exe。
///
/// 安裝程式是以使用者身份靜默執行的，所以這裡是"SayIt 會執行哪個檔案"的最後一道關。
/// 惡意備份曾經能把 pendingUpdate 指到 audio 目錄裡偽裝成錄音的 exe（稽核 S3）。
fn resolve_update_package(file_path: &str) -> Result<std::path::PathBuf, String> {
    let path = std::fs::canonicalize(file_path)
        .map_err(|_| "The installer file does not exist".to_string())?;
    let dir = std::fs::canonicalize(update_dir())
        .map_err(|_| "The update directory does not exist".to_string())?;
    let is_exe = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(UPDATE_PACKAGE_EXTENSION));
    if !path.is_file() || path.parent() != Some(dir.as_path()) || !is_exe {
        return Err("The installer is not in the update directory".to_string());
    }
    Ok(path)
}

/// 安裝前的完整核對：路徑在更新目錄裡，且雜湊等於設定裡 pendingUpdate 記的那個值。
/// 使用者主動安裝和退出兜底安裝共用，兩條路都不能跳過。
fn checked_pending_installer(storage: &Storage, file_path: &str) -> Result<std::path::PathBuf, String> {
    let path = resolve_update_package(file_path)?;
    let pending = storage.get("pendingUpdate", None);
    let pending_path = pending
        .get("filePath")
        .and_then(Value::as_str)
        .and_then(|p| std::fs::canonicalize(p).ok());
    if pending_path.as_deref() != Some(path.as_path()) {
        return Err("The installer does not match the downloaded update".to_string());
    }
    let expected = non_empty(pending.get("sha512").and_then(Value::as_str))
        .ok_or_else(|| "The downloaded update has no checksum".to_string())?;
    if file_sha512_base64(&path)? != expected {
        return Err("Update package integrity check failed (SHA-512 mismatch)".to_string());
    }
    Ok(path)
}

#[derive(Serialize, Clone)]
struct UpdateDownloadProgress {
    #[serde(rename = "downloadedBytes")]
    downloaded_bytes: u64,
    #[serde(rename = "totalBytes")]
    total_bytes: u64,
    percent: f64,
    status: String,
    error: Option<String>,
}

fn emit_update_progress(app: &AppHandle, downloaded: u64, total: u64, status: &str, error: Option<&str>) {
    let percent = if total > 0 {
        (downloaded as f64 / total as f64 * 100.0).min(100.0)
    } else {
        0.0
    };
    let _ = app.emit(
        "update-download-progress",
        UpdateDownloadProgress {
            downloaded_bytes: downloaded,
            total_bytes: total,
            percent,
            status: status.to_string(),
            error: error.map(String::from),
        },
    );
}

/// 下載更新安裝包到臨時目錄，下載過程中透過 update-download-progress 事件上報真實位元組進度。
/// sha512（manifest 裡的 Base64）必須有，落盤後校驗，不透過直接刪掉並報錯。
/// 下載地址必須與伺服器設定或官方地址同源（net_policy::update_url_allowed）。
#[tauri::command]
pub async fn download_update(
    app: AppHandle,
    storage: State<'_, Storage>,
    url: String,
    sha512: Option<String>,
) -> Result<String, String> {
    // [自訂] 下載更新已停用：不從網路下載任何安裝檔。
    let _ = (&app, &storage, &url, &sha512);
    if true {
        return Err("Update download is disabled in this build".to_string());
    }

    let fail = |msg: String| {
        write_log_line(&format!("[update] download refused: {}", msg));
        emit_update_progress(&app, 0, 0, "failed", Some(&msg));
        msg
    };
    let configured = storage
        .get(crate::net_policy::BACKEND_BASE_URL_KEY, None)
        .as_str()
        .unwrap_or("")
        .to_string();
    if !crate::net_policy::update_url_allowed(&url, &configured) {
        return Err(fail("Update download address is not on the configured or official server".to_string()));
    }
    // 兩個更新通道的 manifest 都帶 sha512（2026-10 核對過）。沒有雜湊就無從確認下到的是什麼，不下。
    let Some(expected_sha512) = non_empty(sha512.as_deref()).map(str::to_string) else {
        return Err(fail("Update manifest has no SHA-512 checksum".to_string()));
    };
    let Some(filename) = installer_file_name(&url) else {
        return Err(fail("Update download address does not point to an installer".to_string()));
    };

    // 必須帶 User-Agent：生產環境 AWS WAF 的 NoUserAgent_HEADER 規則會攔截無 UA 的請求（403）
    let client = reqwest::Client::builder()
        .user_agent(concat!("SayIt/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("Failed to initialize download client: {}", e))?;

    let temp_dir = update_dir();
    std::fs::create_dir_all(&temp_dir).map_err(|e| format!("Failed to create temporary directory: {}", e))?;
    let file_path = temp_dir.join(&filename);
    // 先寫到 .part，校驗通過才改名：半截檔案永遠不會被當成可安裝的包。
    let part_path = temp_dir.join(format!("{}.part", filename));
    let (downloaded, total) = fetch_installer_part(&client, &url, &part_path, &mut |done, total, status, error| {
        emit_update_progress(&app, done, total, status, error)
    })
    .await?;

    // 校驗放在"completed"之前：前端收到 completed 就會把這個包記成待安裝，
    // 先報完成再發現雜湊不對，等於讓一個壞包進入待安裝狀態。
    {
        let actual = file_sha512_base64(&part_path).map_err(|e| {
            emit_update_progress(&app, downloaded, total, "failed", Some(&e));
            e
        })?;
        if actual != expected_sha512 {
            // 壞包必須刪掉（續傳接錯了版本也會走到這裡）：下次從頭下。
            let _ = std::fs::remove_file(&part_path);
            let msg = "Update package integrity check failed (SHA-512 mismatch)".to_string();
            write_log_line("[update] downloaded package failed SHA-512 verification, discarded");
            emit_update_progress(&app, downloaded, total, "failed", Some(&msg));
            return Err(msg);
        }
    }

    // 新包已經校驗通過，把同目錄裡其它版本的安裝包刪掉。
    // 舊流程下載完立刻安裝，包只存在幾秒；現在它按設計等到使用者點更新或退出時才用，
    // 於是每升一次版就在 %TEMP% 裡多躺一個十幾 MB 的安裝包，沒人清。
    std::fs::rename(&part_path, &file_path)
        .map_err(|e| format!("Failed to finalize the downloaded installer: {}", e))?;
    prune_stale_packages(&temp_dir, &filename);

    emit_update_progress(&app, downloaded, total.max(downloaded), "completed", None);

    Ok(file_path.to_string_lossy().to_string())
}

/// 把安裝包下載（或接著下載）到 `part_path`。返回 (已下載位元組, 總位元組)。
///
/// 已有 .part 就用 Range 續傳；伺服器回 200 就整包重下。沒有總超時，只在 60 秒收不到
/// 任何資料時判定中斷，.part 留著下次接著下。拆成獨立函式是為了能用本地 HTTP 伺服器測。
async fn fetch_installer_part(
    client: &reqwest::Client,
    url: &str,
    part_path: &std::path::Path,
    progress: &mut (dyn FnMut(u64, u64, &str, Option<&str>) + Send),
) -> Result<(u64, u64), String> {
    use futures_util::StreamExt;
    use std::io::Write;

    let resume_from = std::fs::metadata(part_path).map(|m| m.len()).unwrap_or(0);

    progress(resume_from, 0, "downloading", None);

    // 不設整次請求的總超時：原來是 300 秒，網慢的使用者（14.8 MB 在 50 KB/s 下要 5 分鐘）
    // 永遠下不完、每次都從頭來。現在只限制連線和「多久一個位元組都沒收到」（見下面的 STALL）。
    let mut request = client.get(url);
    if resume_from > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={}-", resume_from));
    }
    let resp = request
        .send()
        .await
        .map_err(|e| {
            let msg = format!("Download failed: {}", e);
            progress(resume_from, 0, "failed", Some(&msg));
            msg
        })?;

    if !resp.status().is_success() {
        let msg = format!("Download failed: HTTP {}", resp.status());
        progress(resume_from, 0, "failed", Some(&msg));
        // 416：.part 已經比伺服器上的檔案還長（多半是換了版本、同名），扔掉重來。
        if resp.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            let _ = std::fs::remove_file(part_path);
        }
        return Err(msg);
    }

    // 206 = 伺服器接著給；200 = 不支援續傳，整包重下。
    let resumed = resume_from > 0 && resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    let mut downloaded: u64 = if resumed { resume_from } else { 0 };
    let total = resp.content_length().map(|len| len + downloaded).unwrap_or(0);
    if resume_from > 0 {
        write_log_line(&format!(
            "[update] resuming download from {} bytes: server {}",
            resume_from,
            if resumed { "accepted the range" } else { "ignored the range, starting over" }
        ));
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resumed)
        .truncate(!resumed)
        .open(part_path)
        .map_err(|e| format!("Failed to create file: {}", e))?;

    let mut stream = resp.bytes_stream();
    let mut last_emit = std::time::Instant::now();
    // 60 秒一個位元組都沒收到才算斷：慢網照樣能下完，真斷線也不會一直掛著。
    const STALL: std::time::Duration = std::time::Duration::from_secs(60);

    loop {
        let next = match tokio::time::timeout(STALL, stream.next()).await {
            Ok(next) => next,
            Err(_) => {
                let msg = "Download stalled: no data for 60 seconds".to_string();
                progress(downloaded, total, "failed", Some(&msg));
                // .part 留著，下次接著下
                return Err(msg);
            }
        };
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(|e| {
            let msg = format!("Download interrupted: {}", e);
            progress(downloaded, total, "failed", Some(&msg));
            msg
        })?;
        file.write_all(&chunk).map_err(|e| format!("Failed to write file: {}", e))?;
        downloaded += chunk.len() as u64;

        // 每 200ms 傳送一次進度，避免刷屏
        if last_emit.elapsed().as_millis() >= 200 {
            progress(downloaded, total, "downloading", None);
            last_emit = std::time::Instant::now();
        }
    }

    file.flush().map_err(|e| format!("Failed to flush file: {}", e))?;
    drop(file);

    Ok((downloaded, total))
}

/// 刪掉更新目錄裡除 keep 之外的所有檔案。
///
/// 只在新包**校驗通過之後**呼叫：下載失敗時磁碟上那個舊包可能還是待安裝的有效包，
/// 提前清理等於把使用者已經下好的更新弄丟，還得重下一遍。
/// 刪不掉（被佔用等）只記日誌不報錯 —— 清理失敗不該讓一次成功的下載變成失敗。
fn prune_stale_packages(dir: &std::path::Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let matches_keep = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n == keep);
        if matches_keep {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(e) => write_log_line(&format!(
                "[update] could not remove the stale package {}: {}",
                path.display(),
                e
            )),
        }
    }
    if removed > 0 {
        write_log_line(&format!("[update] removed {} stale update package(s)", removed));
    }
}

/// 拉起 NSIS 安裝程式（/S 靜默）。
///
/// 靜默模式下 NSIS 不提供"安裝完成後執行"，所以自己接管：起一個不依賴當前程序存活的
/// "看門人" cmd 程序，等安裝程序真正退出（檔案覆蓋完成）後再決定要不要拉起新 exe。
/// 必須等舊程序完全退出才能覆蓋 exe（Windows 檔案鎖），所以看門人要活在本程序之外。
///
/// 關鍵：不要把帶引號、含 && 的複合命令直接塞給 `cmd /C` —— Rust 會把這個含空格的
/// 引數整體再套引號並把內部 " 轉義成 \"，而 cmd.exe 不認識 \" 轉義，路徑會被啃壞
/// （歷史 bug：裝完自動重啟報「找不到 \ 檔案」）。指令碼檔案裡的引號是檔案字面量，
/// 不經過引數轉義層；給 cmd /C 傳單個指令碼路徑是 cmd 唯一能幹淨處理的情形。
///
/// relaunch=false 會裝完就結束、不碰應用。目前兩個呼叫方都傳 true（使用者點「立即安裝」
/// 和退出兜底安裝都要讓使用者看到新版本的關於頁），引數保留是因為"裝完別拉起來"這個選項
/// 一旦需要就必須立刻可用 —— 關機前退出那種場景下拉起應用是明確的錯誤行為。
#[cfg(target_os = "windows")]
fn spawn_installer(installer_path: &str, relaunch: bool) -> Result<(), String> {
    use std::process::Command;
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let relaunch_line = if relaunch {
        let current_exe = std::env::current_exe()
            .map_err(|e| format!("Failed to get the current executable path: {}", e))?;
        // 同一路徑：currentUser 安裝模式下更新會裝回原目錄。--open-about 跳關於頁，
        // 讓使用者能確認更新真的生效了。
        format!("start \"\" \"{}\" --open-about\r\n", current_exe.to_string_lossy())
    } else {
        String::new()
    };

    //   - ping 兜 ~1s，等舊程序完全退出，避免安裝程式覆蓋 exe 時撞檔案鎖
    //   - start /wait 等靜默安裝結束
    //   - del "%~f0" 讓指令碼執行完自刪
    let script = format!(
        "@echo off\r\n\
         ping -n 2 127.0.0.1 >nul\r\n\
         start /wait \"\" \"{installer}\" /S\r\n\
         {relaunch}del \"%~f0\"\r\n",
        installer = installer_path,
        relaunch = relaunch_line,
    );
    let script_path = std::env::temp_dir().join("sayit-update-relaunch.bat");
    std::fs::write(&script_path, script)
        .map_err(|e| format!("Failed to write the update restart script: {}", e))?;

    Command::new("cmd")
        .args(["/C", &script_path.to_string_lossy()])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("Failed to start the installer watchdog: {}", e))?;
    Ok(())
}

/// 把字串包成 sh 的單引號字面量。
#[cfg(target_os = "macos")]
fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// macOS：更新包是裝著 `SayIt.app` 的 zip。和 Windows 一樣起一個不依賴本程序存活的
/// 「看門人」指令碼：等本程序退出 → 解壓 → 校驗簽名完整性 → 原地替換 `.app` → 拉起新版。
///
/// 替換走「先拷到旁邊、再兩次 rename」，不是先刪再拷：關機或斷電時指令碼被殺在半路，
/// 最壞也只是留下一個 `.sayit-new`/`.sayit-old` 目錄，而不是一個被刪掉一半的 SayIt。
/// `/Applications` 沒有寫許可權（非管理員帳戶）時替換會失敗，此時在 Finder 裡選中更新包，
/// 讓使用者手動拖進去。
#[cfg(target_os = "macos")]
fn spawn_installer(installer_path: &str, relaunch: bool) -> Result<(), String> {
    let bundle = crate::platform::macos::permissions::current_bundle_path()
        .filter(|p| p.ends_with(".app"))
        .ok_or_else(|| "SayIt is not running from an .app bundle".to_string())?;
    if bundle.contains("/AppTranslocation/") {
        return Err("SayIt is running from a temporary location; move it to the Applications folder first".to_string());
    }
    let script = macos_installer_script(installer_path, &bundle, std::process::id(), relaunch);
    let script_path = std::env::temp_dir().join("sayit-update-relaunch.sh");
    std::fs::write(&script_path, script).map_err(|e| format!("Failed to write the update script: {}", e))?;
    std::process::Command::new("/bin/sh")
        .arg(&script_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("Failed to start the update script: {}", e))?;
    Ok(())
}

/// 生成 macOS 更新看門人指令碼：等 `pid` 退出 → 解壓 `installer_path` → 校驗簽名 → 替換 `bundle` →
/// （可選）重新開啟。單獨拆出來是為了能在單測裡真跑一遍。
#[cfg(target_os = "macos")]
fn macos_installer_script(installer_path: &str, bundle: &str, pid: u32, relaunch: bool) -> String {
    let relaunch_line = if relaunch {
        "/usr/bin/open \"$APP\" --args --open-about\n"
    } else {
        ""
    };
    format!(
        r#"#!/bin/sh
PKG={pkg}
APP={app}
PID={pid}
i=0
while kill -0 "$PID" 2>/dev/null && [ $i -lt 300 ]; do sleep 0.1; i=$((i+1)); done
WORK=$(/usr/bin/mktemp -d "${{TMPDIR:-/tmp}}/sayit-update.XXXXXX") || exit 1
fail() {{ rm -rf "$WORK" "$APP.sayit-new"; /usr/bin/open -R "$PKG"; rm -f "$0"; exit 1; }}
/usr/bin/ditto -x -k "$PKG" "$WORK" || fail
NEW=$(/usr/bin/find "$WORK" -maxdepth 2 -name '*.app' -type d | head -n 1)
[ -n "$NEW" ] || fail
/usr/bin/codesign --verify --deep --strict "$NEW" >/dev/null 2>&1 || fail
/usr/bin/xattr -dr com.apple.quarantine "$NEW" 2>/dev/null
rm -rf "$APP.sayit-new" "$APP.sayit-old"
/usr/bin/ditto "$NEW" "$APP.sayit-new" || fail
mv "$APP" "$APP.sayit-old" || fail
if ! mv "$APP.sayit-new" "$APP"; then mv "$APP.sayit-old" "$APP"; fail; fi
rm -rf "$APP.sayit-old" "$WORK"
{relaunch}rm -f "$0"
"#,
        pkg = sh_quote(installer_path),
        app = sh_quote(bundle),
        pid = pid,
        relaunch = relaunch_line,
    )
}

/// 系統是否正在關機或登出（GetSystemMetrics(SM_SHUTTINGDOWN)）。
#[cfg(target_os = "windows")]
fn system_is_shutting_down() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SHUTTINGDOWN};
    unsafe { GetSystemMetrics(SM_SHUTTINGDOWN) != 0 }
}

/// 使用者主動點「立即更新」：裝完重新拉起應用。
#[tauri::command]
pub fn install_downloaded_update(
    file_path: String,
    relaunch: bool,
    app: AppHandle,
    storage: State<'_, Storage>,
) -> Result<(), String> {
    let checked = checked_pending_installer(storage.inner(), &file_path).map_err(|e| {
        write_log_line(&format!("[update] install refused: {}", e));
        e
    })?;
    let file_path = checked.to_string_lossy().trim_start_matches(r"\\?\").to_string();

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        use std::sync::atomic::Ordering;
        // 搶在 app.exit(0) 之前置位：那次退出同樣會走 RunEvent::Exit，
        // 退出兜底安裝必須讓開，否則兩個安裝程式搶同一個 exe。
        INSTALLER_SPAWNED.store(true, Ordering::SeqCst);
        if let Err(e) = spawn_installer(&file_path, relaunch) {
            INSTALLER_SPAWNED.store(false, Ordering::SeqCst);
            return Err(e);
        }
        write_log_line(&format!("[update] installer launched by user (relaunch={})", relaunch));
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = (&file_path, relaunch);
        return Err("Automatic installation is not supported on this platform".to_string());
    }

    // 退出當前應用，讓安裝程式能夠覆蓋 exe 檔案
    app.exit(0);
    Ok(())
}

/// 退出時的兜底安裝，由 main.rs 的 RunEvent::Exit 呼叫。
///
/// 使用者沒點「立即更新」就直接關掉了 SayIt —— 那就在退出路徑上裝掉。
/// 少了這一步，"發現新版就更新"完全依賴使用者去點那個圖示。
///
/// 裝完會把應用重新拉起來並跳到關於頁（`relaunch=true`）。這一條 2026-08 改過方向：
/// 原本傳 false，理由是"使用者是要關掉它，裝完又拉起來像關不掉"。改回 true 的理由是
/// 升級完全無聲時使用者根本不知道版本變了，而這個"又自己開了"每個版本最多發生一次
/// （pendingUpdate 裝完即清），代價比"永遠不知道更新了什麼"小。取捨記在
/// .kiro/decisions.md。
///
/// 裝之前與使用者主動安裝同一套核對（路徑在更新目錄裡 + 雜湊對得上）。原來這裡只看檔案在不在，
/// 理由是退出路徑不能卡幾百毫秒；實際 15 MB 的包算一次 SHA-512 是幾十毫秒，
/// 而"只看檔案在不在"正是惡意備份能讓退出時執行任意 exe 的那個口子（稽核 S3）。
pub fn install_pending_update_on_exit(app: &AppHandle) {
    // [自訂] 結束時自動安裝已停用：絕不以官方安裝檔覆蓋自建版本。
    let _ = app;
    if true {
        return;
    }
    use std::sync::atomic::Ordering;
    use tauri::Manager;

    // swap 而不是 load + store：退出事件理論上只來一次，但這裡是唯一沒有其它
    // 互斥保護的路徑，用原子交換把"檢查過了"和"佔位"合成一步。
    if INSTALLER_SPAWNED.swap(true, Ordering::SeqCst) {
        return;
    }

    // 關機 / 登出時不裝：系統幾秒內就會結束所有程序，安裝程式可能被殺在一半，
    // 下次開機就是一個裝了一半的 SayIt。待安裝包留著，下次正常退出時再裝。
    #[cfg(target_os = "windows")]
    if system_is_shutting_down() {
        write_log_line("[update] system is shutting down; leaving the pending update for the next exit");
        return;
    }

    let storage: State<Storage> = app.state();
    let pending = storage.get("pendingUpdate", None);
    let file_path = match pending.get("filePath").and_then(|v| v.as_str()) {
        Some(value) if !value.is_empty() => value.to_string(),
        _ => return,
    };
    let file_path = match checked_pending_installer(storage.inner(), &file_path) {
        Ok(path) => path.to_string_lossy().trim_start_matches(r"\\?\").to_string(),
        Err(e) => {
            write_log_line(&format!("[update] exit-path install skipped: {}", e));
            return;
        }
    };

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        // relaunch=true：裝完把應用拉回來並跳到關於頁。見本函式上方註釋裡的取捨說明。
        match spawn_installer(&file_path, true) {
            Ok(()) => write_log_line("[update] pending update is being installed on exit (will relaunch)"),
            Err(e) => write_log_line(&format!("[update] exit-path install failed: {}", e)),
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let _ = file_path;
}

/// [自訂] SC-62：前端日誌原本在主執行緒同步格式化、stat、寫檔並 flush。
/// 命令維持同步（主執行緒依序收件，行序與呼叫順序一致），只記下時間後交給專屬寫入執行緒；
/// 時間戳記用收件時間，不因排隊而延後。
#[tauri::command]
pub fn append_debug_log(payload: Value) -> Result<(), String> {
    let now = chrono::Local::now();
    match debug_log_queue() {
        Some(queue) => {
            if let Err(payload) = queue.push(now, payload) {
                write_log_line_at(&format_debug_payload(payload), now);
            }
        }
        // 開不了寫入執行緒時照原本方式直接寫。
        None => write_log_line_at(&format_debug_payload(payload), now),
    }
    Ok(())
}

/// [自訂] SC-62 補充（F10b）：佇列最多積壓的前端日誌行數，超過就丟最舊的並記下丟了幾行。
const DEBUG_LOG_QUEUE_CAP: usize = 10_000;
/// 程序結束前等佇列寫完的上限。
pub const DEBUG_LOG_FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);

static DEBUG_LOG_QUEUE: std::sync::OnceLock<Option<std::sync::Arc<LogQueue<Value>>>> =
    std::sync::OnceLock::new();

fn debug_log_queue() -> Option<&'static std::sync::Arc<LogQueue<Value>>> {
    DEBUG_LOG_QUEUE
        .get_or_init(|| {
            spawn_log_writer(DEBUG_LOG_QUEUE_CAP, |at, payload, dropped| {
                if dropped > 0 {
                    write_log_line_at(
                        &format!("[WARN] [log] dropped {dropped} queued frontend log lines (queue full)"),
                        at,
                    );
                }
                write_log_line_at(&format_debug_payload(payload), at)
            })
        })
        .as_ref()
}

/// 程序結束（App 結束、restart_app）前呼叫：等佇列裡已收的前端日誌寫完，最多等 `timeout`。
/// 佇列從沒用過就立即返回。回傳是否在時限內寫完。
pub fn flush_debug_log(timeout: std::time::Duration) -> bool {
    match DEBUG_LOG_QUEUE.get() {
        Some(Some(queue)) => queue.flush(timeout),
        _ => true,
    }
}

struct LogQueueState<T> {
    items: std::collections::VecDeque<(chrono::DateTime<chrono::Local>, T)>,
    /// 收進佇列的總行數。
    pushed: u64,
    /// 已寫出或已丟棄的總行數；flush 等它追上 `pushed`。
    settled: u64,
    /// 上一次交給寫入端之後丟掉的行數。
    dropped: u64,
    writer_alive: bool,
}

struct LogQueue<T> {
    state: std::sync::Mutex<LogQueueState<T>>,
    wake_writer: std::sync::Condvar,
    progress: std::sync::Condvar,
    cap: usize,
}

impl<T> LogQueue<T> {
    fn lock(&self) -> std::sync::MutexGuard<'_, LogQueueState<T>> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 寫入執行緒已不在時交還該行，由呼叫端直接寫。
    fn push(&self, at: chrono::DateTime<chrono::Local>, item: T) -> Result<(), T> {
        let mut state = self.lock();
        if !state.writer_alive {
            return Err(item);
        }
        if state.items.len() >= self.cap {
            state.items.pop_front();
            state.dropped += 1;
            state.settled += 1;
        }
        state.items.push_back((at, item));
        state.pushed += 1;
        drop(state);
        self.wake_writer.notify_one();
        self.progress.notify_all();
        Ok(())
    }

    fn flush(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let mut state = self.lock();
        let target = state.pushed;
        while state.settled < target && state.writer_alive {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            state = self
                .progress
                .wait_timeout(state, left)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
        state.settled >= target
    }
}

fn spawn_log_writer<T: Send + 'static>(
    cap: usize,
    mut sink: impl FnMut(chrono::DateTime<chrono::Local>, T, u64) + Send + 'static,
) -> Option<std::sync::Arc<LogQueue<T>>> {
    let queue = std::sync::Arc::new(LogQueue {
        state: std::sync::Mutex::new(LogQueueState {
            items: std::collections::VecDeque::new(),
            pushed: 0,
            settled: 0,
            dropped: 0,
            writer_alive: true,
        }),
        wake_writer: std::sync::Condvar::new(),
        progress: std::sync::Condvar::new(),
        cap: cap.max(1),
    });
    let worker = queue.clone();
    std::thread::Builder::new()
        .name("debug-log-writer".into())
        .spawn(move || {
            // 寫入端 panic 時標記失效：之後的行由呼叫端直接寫，flush 也不再等。
            struct Alive<T>(std::sync::Arc<LogQueue<T>>);
            impl<T> Drop for Alive<T> {
                fn drop(&mut self) {
                    self.0.lock().writer_alive = false;
                    self.0.progress.notify_all();
                }
            }
            let alive = Alive(worker);
            loop {
                let (at, item, dropped) = {
                    let mut state = alive.0.lock();
                    loop {
                        if let Some((at, item)) = state.items.pop_front() {
                            break (at, item, std::mem::take(&mut state.dropped));
                        }
                        state = alive
                            .0
                            .wake_writer
                            .wait(state)
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                    }
                };
                sink(at, item, dropped);
                alive.0.lock().settled += 1;
                alive.0.progress.notify_all();
            }
        })
        .ok()?;
    Some(queue)
}

fn format_debug_payload(payload: Value) -> String {
    // Format a compact single-line representation for the log file
    match payload {
        Value::Object(ref map) => {
            let kind = map.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
            match kind {
                "runtime" => {
                    let level = map.get("level").and_then(|v| v.as_str()).unwrap_or("info");
                    let source = map.get("source").and_then(|v| v.as_str()).unwrap_or("");
                    let message = map.get("message").and_then(|v| v.as_str()).unwrap_or("");
                    let detail = map.get("detail");
                    if let Some(d) = detail {
                        format!("[{}] [{}] {} {}", level.to_uppercase(), source, message, d)
                    } else {
                        format!("[{}] [{}] {}", level.to_uppercase(), source, message)
                    }
                }
                "session_start" => {
                    let sid = map.get("sessionId").and_then(|v| v.as_str()).unwrap_or("?");
                    format!("[SESSION] start id={}", sid)
                }
                "session_end" => {
                    let sid = map.get("sessionId").and_then(|v| v.as_str()).unwrap_or("?");
                    let dur = map.get("durationMs").and_then(|v| v.as_i64()).unwrap_or(0);
                    let msgs = map.get("messageCount").and_then(|v| v.as_i64()).unwrap_or(0);
                    format!("[SESSION] end id={} duration={}ms messages={}", sid, dur, msgs)
                }
                "ws_message" => {
                    let dir = map.get("direction").and_then(|v| v.as_str()).unwrap_or("?");
                    let typ = map.get("type").and_then(|v| v.as_str()).unwrap_or("?");
                    let data = map.get("data");
                    if let Some(d) = data {
                        format!("[WS] {} {} {}", dir, typ, d)
                    } else {
                        format!("[WS] {} {}", dir, typ)
                    }
                }
                _ => {
                    serde_json::to_string(&payload).unwrap_or_else(|_| format!("{:?}", payload))
                }
            }
        }
        _ => {
            serde_json::to_string(&payload).unwrap_or_else(|_| format!("{:?}", payload))
        }
    }
}

#[tauri::command]
pub fn save_audio_to_downloads(base64_data: String, filename: String) -> Result<String, String> {
    let downloads = dirs::download_dir()
        .ok_or_else(|| "Could not locate the Downloads directory".to_string())?;

    // filename 會拼進下載目錄，帶 `..\` 就能寫到別處（比如開機啟動目錄）。只收單段的 .wav 名字。
    if !crate::path_guard::is_plain_file_name(&filename, "wav") {
        return Err("Invalid audio file name".to_string());
    }

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&base64_data)
        .map_err(|e| format!("Failed to decode base64 audio: {}", e))?;

    let dest = downloads.join(&filename);
    std::fs::write(&dest, &bytes).map_err(|e| format!("Failed to write file: {}", e))?;

    let path_str = dest.to_string_lossy().to_string();
    write_log_line(&format!("[INFO] [audio] Audio saved to {}", path_str));
    Ok(path_str)
}

/// 開啟檔案所在的資料夾（並選中檔案）
#[tauri::command]
pub fn reveal_file_in_folder(file_path: String) -> Result<(), String> {
    // 只對真實存在的本地檔案/目錄調 explorer。explorer 的引數裡塞 URL 或 shell: 路徑
    // 會被它當成要開啟的東西，不再是"在資料夾裡選中"。
    let path = std::path::Path::new(&file_path);
    if !path.is_absolute() || !path.exists() {
        return Err(format!("File does not exist: {}", file_path));
    }
    #[cfg(target_os = "windows")]
    {
        // explorer /select, 會開啟資料夾並高亮選中檔案
        std::process::Command::new("explorer")
            .arg("/select,")
            .arg(&file_path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(&file_path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }
    #[cfg(target_os = "linux")]
    {
        let path = std::path::PathBuf::from(&file_path);
        let dir = path.parent().unwrap_or(&path);
        std::process::Command::new("xdg-open")
            .arg(dir.to_string_lossy().to_string())
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }
    Ok(())
}

/// 直接開啟指定資料夾。
#[tauri::command]
pub fn open_folder(folder_path: String) -> Result<(), String> {
    let path = std::path::PathBuf::from(&folder_path);
    if !path.is_dir() {
        return Err(format!("Folder does not exist: {}", folder_path));
    }

    #[cfg(target_os = "windows")]
    std::process::Command::new("explorer")
        .arg(&path)
        .spawn()
        .map_err(|e| format!("Failed to open folder: {}", e))?;

    #[cfg(target_os = "macos")]
    std::process::Command::new("open")
        .arg(&path)
        .spawn()
        .map_err(|e| format!("Failed to open folder: {}", e))?;

    #[cfg(target_os = "linux")]
    std::process::Command::new("xdg-open")
        .arg(&path)
        .spawn()
        .map_err(|e| format!("Failed to open folder: {}", e))?;

    Ok(())
}

#[cfg(test)]
mod update_path_tests {
    use super::*;

    #[test]
    fn installer_name_comes_from_the_last_path_segment_only() {
        assert_eq!(
            installer_file_name(&format!("https://sayitapp.site/api/desktop-updates/win32/x64/SayIt_0.2.2_x64-setup.{ext}", ext = UPDATE_PACKAGE_EXTENSION)).as_deref(),
            Some(format!("SayIt_0.2.2_x64-setup.{UPDATE_PACKAGE_EXTENSION}").as_str())
        );
        assert_eq!(
            installer_file_name(&format!("https://sayitapp.site/a/SayIt.{UPDATE_PACKAGE_EXTENSION}?x=../../evil")).as_deref(),
            Some(format!("SayIt.{UPDATE_PACKAGE_EXTENSION}").as_str())
        );
        assert!(installer_file_name("https://sayitapp.site/a/run.bat").is_none());
        assert!(installer_file_name(&format!("https://sayitapp.site/a/..%5Cevil.{UPDATE_PACKAGE_EXTENSION}")).is_none());
        assert!(installer_file_name("https://sayitapp.site/").is_none());
    }

    #[test]
    fn packages_outside_the_update_directory_are_rejected() {
        let outside = std::env::temp_dir().join(format!("sayit-not-update-{}.{UPDATE_PACKAGE_EXTENSION}", uuid::Uuid::new_v4()));
        std::fs::write(&outside, b"MZ").unwrap();
        assert!(resolve_update_package(&outside.to_string_lossy()).is_err());
        let _ = std::fs::remove_file(&outside);

        std::fs::create_dir_all(update_dir()).unwrap();
        let inside_txt = update_dir().join(format!("sayit-test-{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&inside_txt, b"x").unwrap();
        assert!(resolve_update_package(&inside_txt.to_string_lossy()).is_err());
        let _ = std::fs::remove_file(&inside_txt);
    }

    #[test]
    fn package_without_a_recorded_hash_is_not_reused() {
        std::fs::create_dir_all(update_dir()).unwrap();
        let pkg = update_dir().join(format!("sayit-test-{}.{UPDATE_PACKAGE_EXTENSION}", uuid::Uuid::new_v4()));
        std::fs::write(&pkg, b"MZ").unwrap();
        let path = pkg.to_string_lossy().to_string();
        assert_eq!(verify_update_package(path.clone(), None).unwrap(), false);
        let good = file_sha512_base64(&pkg).unwrap();
        assert_eq!(verify_update_package(path.clone(), Some(good)).unwrap(), true);
        assert_eq!(verify_update_package(path, Some("AAAA".into())).unwrap(), false);
        let _ = std::fs::remove_file(&pkg);
    }
}

#[cfg(test)]
mod resume_download_tests {
    use super::fetch_installer_part;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// 每個連線回一個預先寫好的響應，並把收到的請求頭記下來。
    fn serve(responses: Vec<Vec<u8>>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut req = Vec::new();
                let mut buf = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = stream.read(&mut buf).unwrap();
                    if n == 0 { break; }
                    req.extend_from_slice(&buf[..n]);
                }
                requests.push(String::from_utf8_lossy(&req).to_lowercase());
                stream.write_all(&response).unwrap();
            }
            requests
        });
        (format!("http://{}/SayIt.exe", addr), handle)
    }

    fn part_file(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("sayit-resume-{}-{}", tag, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("SayIt.exe.part")
    }

    #[tokio::test]
    async fn resumes_from_an_existing_part_file() {
        let part = part_file("resume");
        std::fs::write(&part, b"HELLO ").unwrap();
        let (url, server) = serve(vec![
            b"HTTP/1.1 206 Partial Content\r\nContent-Length: 5\r\nContent-Range: bytes 6-10/11\r\nConnection: close\r\n\r\nWORLD".to_vec(),
        ]);
        let client = reqwest::Client::new();
        let (done, total) = fetch_installer_part(&client, &url, &part, &mut |_, _, _, _| {}).await.unwrap();
        assert_eq!(std::fs::read(&part).unwrap(), b"HELLO WORLD");
        assert_eq!((done, total), (11, 11));
        let requests = server.join().unwrap();
        assert!(requests[0].contains("range: bytes=6-"), "{}", requests[0]);
    }

    #[tokio::test]
    async fn starts_over_when_the_server_ignores_the_range() {
        let part = part_file("restart");
        std::fs::write(&part, b"STALE BYTES").unwrap();
        let (url, server) = serve(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nNEW!".to_vec(),
        ]);
        let client = reqwest::Client::new();
        fetch_installer_part(&client, &url, &part, &mut |_, _, _, _| {}).await.unwrap();
        assert_eq!(std::fs::read(&part).unwrap(), b"NEW!");
        server.join().unwrap();
    }

    #[tokio::test]
    async fn fresh_download_sends_no_range() {
        let part = part_file("fresh");
        let (url, server) = serve(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc".to_vec(),
        ]);
        let client = reqwest::Client::new();
        fetch_installer_part(&client, &url, &part, &mut |_, _, _, _| {}).await.unwrap();
        assert_eq!(std::fs::read(&part).unwrap(), b"abc");
        let requests = server.join().unwrap();
        assert!(!requests[0].contains("range:"));
    }

    #[tokio::test]
    async fn range_not_satisfiable_discards_the_part_file() {
        let part = part_file("416");
        std::fs::write(&part, b"TOO LONG ALREADY").unwrap();
        let (url, server) = serve(vec![
            b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
        ]);
        let client = reqwest::Client::new();
        assert!(fetch_installer_part(&client, &url, &part, &mut |_, _, _, _| {}).await.is_err());
        assert!(!part.exists());
        server.join().unwrap();
    }
}

/// macOS 更新指令碼端到端：用兩個 ad-hoc 簽名的假 .app 真跑一遍「解壓 → 驗籤 → 替換」。
/// 路徑裡故意帶空格和單引號，釘住 sh_quote。
#[cfg(all(test, target_os = "macos"))]
mod macos_installer_tests {
    use super::macos_installer_script;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// 最小的 .app：執行檔拷自 /usr/bin/true，版本號寫進 Info.plist 用來區分新舊。
    fn make_app(dir: &Path, version: &str) -> PathBuf {
        let app = dir.join("SayIt.app");
        let macos = app.join("Contents").join("MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::copy("/usr/bin/true", macos.join("sayit")).unwrap();
        std::fs::write(
            app.join("Contents").join("Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>sayit</string>
<key>CFBundleIdentifier</key><string>com.sayit.installer-test</string>
<key>CFBundleShortVersionString</key><string>{version}</string>
</dict></plist>
"#
            ),
        )
        .unwrap();
        let signed = Command::new("/usr/bin/codesign").args(["--force", "-s", "-"]).arg(&app).output().unwrap();
        assert!(signed.status.success(), "codesign: {}", String::from_utf8_lossy(&signed.stderr));
        app
    }

    #[test]
    fn update_script_replaces_the_app_bundle_in_place() {
        let root = std::env::temp_dir().join(format!("sayit installer 'test' {}", uuid::Uuid::new_v4()));
        let installed_dir = root.join("Applications");
        let new_dir = root.join("new");
        std::fs::create_dir_all(&installed_dir).unwrap();
        std::fs::create_dir_all(&new_dir).unwrap();
        let installed = make_app(&installed_dir, "1.0.0");
        let new_app = make_app(&new_dir, "2.0.0");
        let pkg = root.join("SayIt_2.0.0_arm64.zip");
        let zipped = Command::new("/usr/bin/ditto")
            .args(["-c", "-k", "--norsrc", "--noextattr", "--keepParent"])
            .arg(&new_app)
            .arg(&pkg)
            .status()
            .unwrap();
        assert!(zipped.success());

        // 一個已經退出的程序號：指令碼不用等
        let mut child = Command::new("/usr/bin/true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();

        let script_path = root.join("update.sh");
        let script = macos_installer_script(&pkg.to_string_lossy(), &installed.to_string_lossy(), pid, false);
        std::fs::write(&script_path, script).unwrap();
        let status = Command::new("/bin/sh").arg(&script_path).status().unwrap();
        assert!(status.success(), "更新指令碼失敗");

        let plist = std::fs::read_to_string(installed.join("Contents").join("Info.plist")).unwrap();
        assert!(plist.contains("2.0.0"), "舊版沒被換掉");
        assert!(!installed_dir.join("SayIt.app.sayit-new").exists());
        assert!(!installed_dir.join("SayIt.app.sayit-old").exists());
        assert!(!script_path.exists(), "指令碼結束時應刪掉自己");
        let verified = Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(&installed)
            .status()
            .unwrap();
        assert!(verified.success(), "換上去的 .app 簽名應完好");

        let _ = std::fs::remove_dir_all(&root);
    }
}
