use serde_json::Value;
use tauri::{AppHandle, Manager, State};
use crate::storage::Storage;
use std::sync::{Mutex, MutexGuard};

// [自訂] S61：同一臨界區內讀取設定並完成落盤，關閉後才統計待保留資料。
static HISTORY_RETENTION: Mutex<()> = Mutex::new(());

pub(super) fn lock_history_retention() -> MutexGuard<'static, ()> {
    HISTORY_RETENTION.lock().unwrap_or_else(|e| e.into_inner())
}

#[tauri::command]
pub fn store_get(key: String, storage: State<Storage>) -> Result<Value, String> {
    Ok(storage.get(&key, None))
}

#[tauri::command]
pub async fn store_set(key: String, value: Value, app: AppHandle) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let _retention = matches!(key.as_str(), "historyEnabled" | "audioRetentionEnabled")
            .then(lock_history_retention);
        app.state::<Storage>().set(&key, &value).map_err(|e| e.to_string())
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn store_delete(key: String, app: AppHandle) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let _retention = matches!(key.as_str(), "historyEnabled" | "audioRetentionEnabled")
            .then(lock_history_retention);
        app.state::<Storage>().delete(&key).map_err(|e| e.to_string())
    }).await.map_err(|e| e.to_string())?
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
