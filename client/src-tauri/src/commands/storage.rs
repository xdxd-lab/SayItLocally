use serde_json::Value;
use tauri::{AppHandle, Manager, State};
use crate::storage::Storage;
use std::sync::{Mutex, MutexGuard};

// [自訂] S61：同一臨界區內讀取設定並完成落盤，關閉後才統計待保留資料。
static HISTORY_RETENTION: Mutex<()> = Mutex::new(());

pub(super) fn lock_history_retention() -> MutexGuard<'static, ()> {
    HISTORY_RETENTION.lock().unwrap_or_else(|e| e.into_inner())
}

// [自訂] REV9：在 IPC 接收時排入 FIFO，讀寫均在同一背景工作執行緒。
type SettingJob = Box<dyn FnOnce() + Send>;

fn settings_queue() -> std::sync::mpsc::Sender<SettingJob> {
    let (sender, receiver) = std::sync::mpsc::channel::<SettingJob>();
    std::thread::spawn(move || {
        for job in receiver {
            // An unexpected panic in the response callback must not stop the queue.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
        }
    });
    sender
}

// [自訂] REV10：工作出錯時也要送回拒絕，讓同一 key 的下一個請求繼續。
fn setting_job(
    run: impl FnOnce() -> Result<Value, String> + Send + 'static,
    respond: impl FnOnce(Result<Value, String>) + Send + 'static,
) -> SettingJob {
    Box::new(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
            .unwrap_or_else(|_| Err("Settings operation failed unexpectedly".into()));
        respond(result);
    })
}

pub fn with_ordered_settings(
    next: impl Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool + Send + Sync + 'static,
) -> impl Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool + Send + Sync + 'static {
    let settings = settings_queue();

    move |invoke| {
        let command = invoke.message.command();
        if !matches!(command, "store_get" | "store_set" | "store_delete") {
            return next(invoke);
        }
        let tauri::ipc::InvokeBody::Json(args) = invoke.message.payload() else {
            invoke.resolver.reject("Expected a JSON settings request");
            return true;
        };
        let Some(key) = args.get("key").and_then(Value::as_str).map(str::to_owned) else {
            invoke.resolver.reject("Expected a settings key");
            return true;
        };
        let operation = match command {
            "store_get" => SettingOperation::Get,
            "store_set" => match args.get("value") {
                Some(value) => SettingOperation::Set(value.clone()),
                None => {
                    invoke.resolver.reject("Expected a settings value");
                    return true;
                }
            },
            _ => SettingOperation::Delete,
        };
        let app = invoke.message.webview_ref().app_handle().clone();
        let resolver = invoke.resolver.clone();
        let submitted = settings.send(setting_job(
            move || operation.run(app.state::<Storage>().inner(), &key),
            move |result| resolver.respond(result.map_err(tauri::ipc::InvokeError::from)),
        ));
        if submitted.is_err() {
            invoke.resolver.reject("Settings worker is unavailable");
        }
        true
    }
}

enum SettingOperation {
    Get,
    Set(Value),
    Delete,
}

impl SettingOperation {
    fn run(self, storage: &Storage, key: &str) -> Result<Value, String> {
        if matches!(self, Self::Get) {
            return Ok(storage.get(key, None));
        }
        let _retention = matches!(key, "historyEnabled" | "audioRetentionEnabled")
            .then(lock_history_retention);
        match self {
            Self::Get => unreachable!(),
            Self::Set(value) => storage.set(key, &value),
            Self::Delete => storage.delete(key),
        }.map(|()| Value::Null).map_err(|error| error.to_string())
    }
}

#[tauri::command]
pub fn history_list(query: Option<Value>, storage: State<Storage>) -> Result<Vec<Value>, String> {
    let (keyword, favorite_only, limit, offset) = parse_history_query(&query);
    Ok(storage.history_list(keyword.as_deref(), favorite_only, limit, offset))
}

#[tauri::command]
pub fn history_count(query: Option<Value>, storage: State<Storage>) -> Result<i64, String> {
    let (keyword, favorite_only, _, _) = parse_history_query(&query);
    Ok(storage.history_count(keyword.as_deref(), favorite_only))
}

#[tauri::command]
pub async fn history_add(record: Value, app: AppHandle) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        // [自訂] S61：關閉歷史後擋下之前已開始辨識、尚未寫入的結果。
        let _recordings = super::audio::lock_recordings();
        let _retention = lock_history_retention();
        let storage = app.state::<Storage>();
        if storage.get("historyEnabled", None).as_bool() != Some(true) {
            return Err("History is disabled".into());
        }
        storage.history_add(&record).map_err(|e| e.to_string())
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn history_update(id: String, patch: Value, storage: State<Storage>) -> Result<(), String> {
    storage.history_update(&id, &patch).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn history_delete(id: String, storage: State<Storage>) -> Result<(), String> {
    storage.history_delete(&id).map_err(|e| e.to_string())
}

// [自訂] S61：也計入未連結到歷史列的本機錄音。
#[tauri::command]
pub async fn history_retained_counts(app: AppHandle) -> Result<(usize, usize), String> {
    tokio::task::spawn_blocking(move || {
        let _recordings = super::audio::lock_recordings();
        let _retention = lock_history_retention();
        let count = app.state::<Storage>().retained_history_count().map_err(|e| e.to_string())?;
        let recordings = super::audio::retained_recordings()?;
        Ok((count, recordings.len()))
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn history_clear_retained(
    history_count: usize,
    audio_count: usize,
    app: AppHandle,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let _recordings = super::audio::lock_recordings_for_deletion()?;
        let _retention = lock_history_retention();
        let storage = app.state::<Storage>();
        if storage.get("historyEnabled", None).as_bool() != Some(false) {
            return Err("Turn off history before clearing retained data".into());
        }
        clear_confirmed_history(
            &storage, history_count, audio_count, super::audio::retained_recordings()?,
            super::audio::remove_recording,
        )
    }).await.map_err(|e| e.to_string())?
}

// [自訂] REV9：錄音刪除失敗時保留歷史列，所有 SQL 變更在同一交易中提交。
fn clear_confirmed_history(
    storage: &Storage,
    history_count: usize,
    audio_count: usize,
    recordings: Vec<std::path::PathBuf>,
    remove: impl Fn(&std::path::Path) -> Result<(), String>,
) -> Result<(), String> {
    let count = storage.retained_history_count().map_err(|e| e.to_string())?;
    if count > history_count || recordings.len() > audio_count {
        return Err("Retained history or recordings changed after confirmation".into());
    }
    for path in recordings {
        remove(&path)?;
    }
    storage.clear_retained_history(history_count)
}

#[tauri::command]
pub fn history_set_favorite(id: String, favorite: bool, storage: State<Storage>) -> Result<(), String> {
    storage.history_set_favorite(&id, favorite).map_err(|e| e.to_string())
}

fn parse_history_query(query: &Option<Value>) -> (Option<String>, bool, Option<i64>, Option<i64>) {
    match query {
        Some(v) => {
            let keyword = v.get("keyword").and_then(|k| k.as_str()).map(|s| s.to_string());
            let favorite_only = v.get("favoriteOnly").and_then(|f| f.as_bool()).unwrap_or(false);
            let limit = v.get("limit").and_then(|l| l.as_i64());
            let offset = v.get("offset").and_then(|o| o.as_i64());
            (keyword, favorite_only, limit, offset)
        }
        None => (None, false, None, None),
    }
}


#[cfg(test)]
mod retention_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn setting_operations_keep_ipc_order_and_reads_follow_writes() {
        let storage = std::sync::Arc::new(Storage::new(PathBuf::from(":memory:")).unwrap());
        let queue = settings_queue();
        let (done, results) = std::sync::mpsc::channel();
        let (resume, paused) = std::sync::mpsc::channel();
        queue.send(Box::new(move || paused.recv().unwrap())).unwrap();
        for operation in [
            SettingOperation::Set(serde_json::json!(1)),
            SettingOperation::Set(serde_json::json!(2)),
            SettingOperation::Get,
            SettingOperation::Delete,
            SettingOperation::Get,
        ] {
            let db = storage.clone();
            let done = done.clone();
            queue.send(Box::new(move || done.send(operation.run(&db, "orderTest")).unwrap())).unwrap();
        }
        assert!(results.try_recv().is_err());
        resume.send(()).unwrap();
        let outputs: Vec<Value> = (0..5).map(|_| results.recv_timeout(std::time::Duration::from_secs(2))
            .unwrap().unwrap()).collect();
        assert_eq!(outputs, serde_json::json!([null, null, 2, null, null]).as_array().unwrap().clone());
    }

    #[test]
    fn panicking_setting_job_replies_with_error_and_worker_processes_next_request() {
        let queue = settings_queue();
        let (done, results) = std::sync::mpsc::channel();
        let failed = done.clone();
        queue.send(setting_job(
            || panic!("simulated settings panic"),
            move |result| failed.send(result).unwrap(),
        )).unwrap();
        queue.send(setting_job(
            || Ok(serde_json::json!(42)),
            move |result| done.send(result).unwrap(),
        )).unwrap();
        let timeout = std::time::Duration::from_secs(2);
        assert_eq!(results.recv_timeout(timeout).unwrap().unwrap_err(), "Settings operation failed unexpectedly");
        assert_eq!(results.recv_timeout(timeout).unwrap().unwrap(), 42);
    }

    #[test]
    fn history_clear_reports_partial_audio_failure_and_rolls_back_sql_failure() {
        let storage = Storage::new(PathBuf::from(":memory:")).unwrap();
        for id in ["a", "b"] {
            storage.history_add(&serde_json::json!({
                "id": id, "charCount": 5, "durationSec": 2
            })).unwrap();
        }
        let recordings = vec![PathBuf::from("first.wav"), PathBuf::from("second.wav")];
        let calls = std::cell::Cell::new(0);
        let result = clear_confirmed_history(&storage, 2, 2, recordings, |_| {
            calls.set(calls.get() + 1);
            if calls.get() == 2 { Err("simulated audio error".into()) } else { Ok(()) }
        });
        assert_eq!(result.unwrap_err(), "simulated audio error");
        assert_eq!(storage.retained_history_count().unwrap(), 2);

        storage.db.lock().unwrap().execute_batch(
            "CREATE TRIGGER fail_stats BEFORE UPDATE ON app_settings
             WHEN NEW.key = 'stats' BEGIN SELECT RAISE(ABORT, 'simulated SQL error'); END;",
        ).unwrap();
        assert!(clear_confirmed_history(&storage, 2, 0, vec![], |_| unreachable!()).is_err());
        assert_eq!(storage.retained_history_count().unwrap(), 2);

        storage.db.lock().unwrap().execute_batch("DROP TRIGGER fail_stats").unwrap();
        clear_confirmed_history(&storage, 2, 0, vec![], |_| unreachable!()).unwrap();
        assert_eq!(storage.retained_history_count().unwrap(), 0);
        assert_eq!(storage.get("stats", None)["totalChars"], 0);
    }

    #[test]
    fn retention_settings_distinguish_missing_corrupt_and_unreadable_values() {
        let storage = Storage::new(PathBuf::from(":memory:")).unwrap();
        for (key, default) in [("audioRetentionDays", 7), ("logRetentionDays", 30)] {
            assert_eq!(storage.retention_days(key, default).unwrap(), default);
            storage.set(key, &serde_json::json!(90)).unwrap();
            assert_eq!(storage.retention_days(key, default).unwrap(), 90);
            storage.set(key, &serde_json::json!("90")).unwrap();
            assert!(storage.retention_days(key, default).is_err());
        }
        storage.db.lock().unwrap().execute(
            "UPDATE app_settings SET value_json = 'bad json' WHERE key = 'audioRetentionDays'", [],
        ).unwrap();
        assert!(storage.retention_days("audioRetentionDays", 7).is_err());
        storage.db.lock().unwrap().execute("DROP TABLE app_settings", []).unwrap();
        assert!(storage.retention_days("audioRetentionDays", 7).is_err());
        assert!(storage.retention_days("logRetentionDays", 30).is_err());
    }
}
