use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Result as SqlResult, TransactionBehavior};
use serde_json::Value;
use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

pub(crate) mod recovery;
pub(crate) use recovery::StorageRecovery;

/// 新庫的種子值。JSON 字面量用不了常量，快捷鍵幾項與 crate::defaults 的對帳在 defaults.rs 的測試裡。
pub(crate) const DEFAULT_SETTINGS: &[(&str, &str)] = &[
    // 兩個快捷鍵預設值按平台不同（mac 沒有右 Ctrl），字面量來自 defaults.rs 的宏。
    ("shortcutPTT", concat!("\"", crate::default_shortcut_ptt!(), "\"")),
    ("shortcutPTTCombo", r#""Alt+Q""#),
    ("shortcutHandsFree", concat!("\"", crate::default_shortcut_hands_free!(), "\"")),
    ("shortcutToggleAi", r#""""#),
    ("autoLaunch", "false"), // [自訂] S30：預設不開機自啟（原為 "true"）
    ("selectedMic", r#""""#),
    ("hotwords", "[]"),
    ("builtinHotwordSets", r#"{"ai":false}"#),
    ("stats", r#"{"totalDurationSec":0,"totalChars":0}"#),
    ("activePresetId", r#""intent""#),
];

/// Collection keys that map to dedicated tables instead of app_settings
const COLLECTION_KEYS: &[&str] = &[
    "history",
    "manualCorrections",
    "feedbackQueue",
    "promptPresets",
    "appPromptRules",
];

fn is_collection_key(key: &str) -> bool {
    COLLECTION_KEYS.contains(&key)
}

fn collection_table(key: &str) -> Option<&'static str> {
    match key {
        "history" => Some("history_records"),
        "manualCorrections" => Some("manual_corrections"),
        "feedbackQueue" => Some("feedback_queue"),
        "promptPresets" => Some("prompt_presets"),
        "appPromptRules" => Some("app_prompt_rules"),
        _ => None,
    }
}

pub struct Storage {
    pub db: Mutex<Connection>,
    // Drop SQLite before releasing the process lease.
    recovery_lease: Option<fs::File>,
    // [自訂] SC-55：由開啟目標決定；記憶體庫的內容在退出後消失。
    volatile: bool,
    // [自訂] SC-56：本次啟動經過任何復原模式（含換新磁碟庫）。
    recovered: bool,
}

/// [自訂] SC-55：記憶體庫不接受還原與匯入，前端依此碼顯示說明。
pub(crate) const VOLATILE_STORAGE_ERROR: &str = "BACKUP_VOLATILE_STORAGE";
/// [自訂] SC-56：記憶體庫不寫出匯出檔或備份。
pub(crate) const VOLATILE_EXPORT_ERROR: &str = "BACKUP_VOLATILE_EXPORT";

impl Storage {
    pub fn open_or_recover(db_path: PathBuf) -> (Self, Option<StorageRecovery>) {
        let (mut storage, mode) = recovery::open_or_recover(&db_path, Duration::from_secs(5));
        storage.recovered = mode.is_some();
        (storage, mode)
    }

    pub(crate) fn recovered_at_startup(&self) -> bool {
        self.recovered
    }

    // [自訂] SC-56：匯出與備份在建立任何檔案之前呼叫。
    pub(crate) fn ensure_exportable(&self) -> Result<(), String> {
        if self.volatile {
            return Err(VOLATILE_EXPORT_ERROR.into());
        }
        Ok(())
    }

    pub(crate) fn is_volatile(&self) -> bool {
        self.volatile
    }

    // [自訂] SC-55：還原與匯入在讀寫任何東西之前呼叫。
    pub(crate) fn ensure_persistent(&self) -> Result<(), String> {
        if self.volatile {
            return Err(VOLATILE_STORAGE_ERROR.into());
        }
        Ok(())
    }

    /// Use the connection even if an earlier holder panicked.
    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[cfg(test)]
    pub fn new(db_path: PathBuf) -> SqlResult<Self> {
        if db_path.as_os_str() != ":memory:" {
            if let Some(parent) = db_path.parent() {
                fs::create_dir_all(parent).map_err(|_| rusqlite::Error::InvalidPath(db_path.clone()))?;
            }
        }
        Self::open_sqlite(&db_path, true, Duration::from_secs(5))
    }

    fn sqlite_path(db_path: &Path) -> SqlResult<Cow<'_, Path>> {
        if db_path.as_os_str() == ":memory:" || cfg!(not(unix)) {
            return Ok(Cow::Borrowed(db_path));
        }
        // Resolve the directory spelling without resolving the database filename.
        // The recovery lease uses NOFOLLOW, including for ancestors on Unix.
        let parent = db_path.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let name = db_path.file_name().ok_or_else(|| rusqlite::Error::InvalidPath(db_path.into()))?;
        let path = parent.canonicalize()
            .map_err(|_| rusqlite::Error::InvalidPath(db_path.into()))?.join(name);
        Ok(Cow::Owned(path))
    }

    fn open_sqlite(db_path: &Path, create: bool, timeout: Duration) -> SqlResult<Self> {
        let disk = db_path.as_os_str() != ":memory:";
        let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_EXRESCODE;
        if create {
            flags |= OpenFlags::SQLITE_OPEN_CREATE;
        }
        let mut db = Connection::open_with_flags(Self::sqlite_path(db_path)?, flags)?;
        db.busy_timeout(timeout)?;
        let no_checkpoint = rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE;
        if disk {
            // An unsuccessful startup probe must not checkpoint/remove WAL
            // on close. A successfully opened database keeps normal behavior.
            db.set_db_config(no_checkpoint, true)?;
        }
        db.pragma_update(None, "foreign_keys", true)?;
        Self::initialize_schema(&mut db)?;
        if disk {
            // A database with valid committed schema is usable in its current
            // journal mode. Request WAL only after initialization succeeds.
            if let Err(error) = db.pragma_update(None, "journal_mode", "WAL") {
                log_backup_sqlite_error("set journal mode", &error);
            }
            if let Err(error) = db.set_db_config(no_checkpoint, false) {
                log_backup_sqlite_error("restore checkpoint setting", &error);
            }
        }
        db.busy_timeout(Duration::from_secs(5))?;
        Ok(Self { db: Mutex::new(db), recovery_lease: None, volatile: !disk, recovered: false })
    }

    fn initialize_schema(db: &mut Connection) -> SqlResult<()> {
        // Both migrations and defaults are atomic, including marker creation.
        // Check markers only after acquiring SQLite's write lock.
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                applied_at INTEGER NOT NULL
            )"
        )?;

        if Self::migration_exists(&tx, 1)? {
            let table_count: i64 = tx.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name IN
                 ('db_meta', 'app_settings', 'history_records', 'manual_corrections',
                  'feedback_queue', 'prompt_presets', 'app_prompt_rules')",
                [],
                |row| row.get(0),
            )?;
            if table_count != 7 {
                return Err(Self::inconsistent_migration());
            }
        } else {
            // Older versions may have applied all or part of this idempotent DDL.
            tx.execute_batch(include_str!("migration_001.sql"))?;
            Self::mark_migration(&tx, 1, "init-sqlite-storage")?;
        }

        let has_audio_path = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('history_records') WHERE name = 'audio_file_path')",
            [],
            |row| row.get::<_, bool>(0),
        )?;
        if Self::migration_exists(&tx, 2)? {
            if !has_audio_path {
                return Err(Self::inconsistent_migration());
            }
        } else {
            if !has_audio_path {
                tx.execute_batch("ALTER TABLE history_records ADD COLUMN audio_file_path TEXT;")?;
            }
            Self::mark_migration(&tx, 2, "add-audio-file-path")?;
        }

        Self::seed_defaults(&tx)?;
        tx.commit()
    }

    fn inconsistent_migration() -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_SCHEMA), None)
    }

    fn migration_exists(db: &Connection, version: i64) -> SqlResult<bool> {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
            [version],
            |row| row.get(0),
        )
    }

    fn mark_migration(db: &Connection, version: i64, name: &str) -> SqlResult<()> {
        db.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
            params![version, name, chrono::Utc::now().timestamp_millis()],
        )?;
        Ok(())
    }

    fn seed_defaults(db: &Connection) -> SqlResult<()> {
        let now = chrono::Utc::now().timestamp_millis();
        for &(key, value_json) in DEFAULT_SETTINGS {
            db.execute(
                "INSERT INTO app_settings (key, value_json, updated_at)
                 SELECT ?1, ?2, ?3 WHERE NOT EXISTS (SELECT 1 FROM app_settings WHERE key = ?1)",
                params![key, value_json, now],
            )?;
        }
        Ok(())
    }

    // ─── Get / Set / Delete ───

    pub fn get(&self, key: &str, fallback: Option<&Value>) -> Value {
        if is_collection_key(key) {
            return self.read_collection(key).unwrap_or(Value::Array(vec![]));
        }

        let db = self.conn();
        let result: Option<String> = db
            .query_row(
                "SELECT value_json FROM app_settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .ok();

        match result {
            Some(json_str) => serde_json::from_str(&json_str).unwrap_or_else(|_| {
                fallback.cloned().unwrap_or(Value::Null)
            }),
            None => {
                // Check defaults
                for &(k, v) in DEFAULT_SETTINGS {
                    if k == key {
                        return serde_json::from_str(v).unwrap_or(Value::Null);
                    }
                }
                fallback.cloned().unwrap_or(Value::Null)
            }
        }
    }

    // [自訂] REV9：只有不存在的設定才能套用預設，讀取錯誤或型別損壞一律跳過清理。
    pub(crate) fn retention_days(&self, key: &str, default: i64) -> Result<i64, String> {
        let db = self.conn();
        let raw: Option<String> = db.query_row(
            "SELECT value_json FROM app_settings WHERE key = ?1",
            params![key],
            |row| row.get(0),
        ).optional().map_err(|error| error.to_string())?;
        match raw {
            None => Ok(default),
            Some(raw) => serde_json::from_str::<i64>(&raw).map_err(|error| error.to_string()),
        }
    }

    // [自訂] SC-55：只有不存在或空字串才算「未自選」；讀取錯誤或型別不符回 Err，不當成預設。
    pub(crate) fn custom_models_dir(&self) -> Result<Option<PathBuf>, String> {
        let db = self.conn();
        let raw: Option<String> = db.query_row(
            "SELECT value_json FROM app_settings WHERE key = 'localAsr.modelsDir'",
            [],
            |row| row.get(0),
        ).optional().map_err(|error| error.to_string())?;
        let Some(raw) = raw else { return Ok(None) };
        let dir = serde_json::from_str::<String>(&raw).map_err(|error| error.to_string())?;
        let dir = dir.trim();
        Ok((!dir.is_empty()).then(|| PathBuf::from(dir)))
    }

    pub fn set(&self, key: &str, value: &Value) -> SqlResult<()> {
        if is_collection_key(key) {
            let items = match value {
                Value::Array(arr) => arr.clone(),
                _ => vec![],
            };
            return self.replace_collection(key, &items);
        }

        let db = self.conn();
        let value = &sanitize_setting_value(key, value); // [自訂] #20：不保存金鑰／密碼
        let json_str = serde_json::to_string(value).unwrap_or_else(|_| "null".to_string());
        let now = chrono::Utc::now().timestamp_millis();

        db.execute(
            "INSERT INTO app_settings (key, value_json, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at",
            params![key, json_str, now],
        )?;
        Ok(())
    }

    pub fn delete(&self, key: &str) -> SqlResult<()> {
        let db = self.conn();
        if let Some(table) = collection_table(key) {
            db.execute(&format!("DELETE FROM {}", table), [])?;
        } else {
            db.execute("DELETE FROM app_settings WHERE key = ?1", params![key])?;
        }
        Ok(())
    }

    // ─── History ───

    pub fn history_list(&self, keyword: Option<&str>, favorite_only: bool, limit: Option<i64>, offset: Option<i64>) -> Vec<Value> {
        let db = self.conn();
        let mut where_clauses = Vec::new();
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if favorite_only {
            where_clauses.push("favorite = 1".to_string());
        }
        if let Some(kw) = keyword {
            let trimmed = kw.trim().to_lowercase();
            if !trimmed.is_empty() {
                where_clauses.push(format!("LOWER(raw_json) LIKE ?{}", param_values.len() + 1));
                param_values.push(Box::new(format!("%{}%", trimmed)));
            }
        }

        let where_sql = if where_clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", where_clauses.join(" AND "))
        };

        let limit_sql = match limit {
            Some(l) if l > 0 => {
                let off = offset.unwrap_or(0).max(0);
                format!(" LIMIT {} OFFSET {}", l, off)
            }
            _ => String::new(),
        };

        let sql = format!(
            "SELECT raw_json FROM history_records {} ORDER BY list_order ASC{}",
            where_sql, limit_sql
        );

        let params_ref: Vec<&dyn rusqlite::types::ToSql> = param_values.iter().map(|p| p.as_ref()).collect();
        let mut stmt = db.prepare(&sql).unwrap();
        let rows = stmt
            .query_map(params_ref.as_slice(), |row| {
                let json_str: String = row.get(0)?;
                Ok(json_str)
            })
            .unwrap();

        rows.filter_map(|r| r.ok())
            .filter_map(|s| serde_json::from_str(&s).ok())
            .collect()
    }

    pub fn history_count(&self, keyword: Option<&str>, favorite_only: bool) -> i64 {
        let db = self.conn();
        let mut where_clauses = Vec::new();
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if favorite_only {
            where_clauses.push("favorite = 1".to_string());
        }
        if let Some(kw) = keyword {
            let trimmed = kw.trim().to_lowercase();
            if !trimmed.is_empty() {
                where_clauses.push(format!("LOWER(raw_json) LIKE ?{}", param_values.len() + 1));
                param_values.push(Box::new(format!("%{}%", trimmed)));
            }
        }

        let where_sql = if where_clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", where_clauses.join(" AND "))
        };

        let sql = format!("SELECT COUNT(*) FROM history_records {}", where_sql);
        let params_ref: Vec<&dyn rusqlite::types::ToSql> = param_values.iter().map(|p| p.as_ref()).collect();

        db.query_row(&sql, params_ref.as_slice(), |row| row.get(0)).unwrap_or(0)
    }

    /// 新記錄排在最前面。
    ///
    /// list_order 只用來排序（ORDER BY list_order ASC），不要求從 0 開始連續，所以新記錄
    /// 取「當前最小值 - 1」即可，走索引。原來是先把全表每行 +1 再插 0，記錄越多每次口述
    /// 儲存越慢（幾千條時每次都是幾千行的寫入）。插入和統計放進同一個事務，不再三次各自提交。
    pub fn history_add(&self, record: &Value) -> SqlResult<()> {
        let mut db = self.conn();
        let tx = db.transaction()?;

        let list_order: i64 = tx.query_row(
            "SELECT COALESCE(MIN(list_order), 0) - 1 FROM history_records",
            [],
            |row| row.get(0),
        )?;

        let obj = record.as_object().cloned().unwrap_or_default();
        let id = obj.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");
        let timestamp = obj.get("timestamp").and_then(|v| v.as_i64()).unwrap_or(0);
        let favorite = obj.get("favorite").and_then(|v| v.as_bool()).unwrap_or(false);
        let char_count = obj.get("charCount").and_then(|v| v.as_i64()).unwrap_or(0);
        let duration_sec = obj.get("durationSec").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let is_empty = obj.get("isEmpty").and_then(|v| v.as_bool()).unwrap_or(false);
        let app_id = obj.get("appId").and_then(|v| v.as_str());
        let app_name = obj.get("appName").and_then(|v| v.as_str());
        let audio_file_path = obj.get("audioFilePath").and_then(|v| v.as_str());
        let raw_json = serde_json::to_string(record).unwrap_or_else(|_| "{}".to_string());

        tx.execute(
            "INSERT INTO history_records (id, list_order, timestamp, favorite, char_count, duration_sec, is_empty, app_id, app_name, audio_file_path, raw_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![id, list_order, timestamp, favorite as i32, char_count, duration_sec, is_empty as i32, app_id, app_name, audio_file_path, raw_json],
        )?;

        // Update stats
        self.update_stats_delta(&tx, char_count, duration_sec, 1);
        tx.commit()
    }

    pub fn history_update(&self, id: &str, patch: &Value) -> SqlResult<()> {
        let db = self.conn();

        let row: Option<(i64, String)> = db.query_row(
            "SELECT list_order, raw_json FROM history_records WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).ok();

        let (_list_order, raw_json) = match row {
            Some(r) => r,
            None => return Ok(()),
        };

        let mut prev: serde_json::Map<String, Value> = serde_json::from_str(&raw_json).unwrap_or_default();
        let prev_chars = prev.get("charCount").and_then(|v| v.as_i64()).unwrap_or(0);
        let prev_dur = prev.get("durationSec").and_then(|v| v.as_f64()).unwrap_or(0.0);

        if let Some(patch_obj) = patch.as_object() {
            for (k, v) in patch_obj {
                prev.insert(k.clone(), v.clone());
            }
        }

        let next = Value::Object(prev.clone());
        let timestamp = prev.get("timestamp").and_then(|v| v.as_i64()).unwrap_or(0);
        let favorite = prev.get("favorite").and_then(|v| v.as_bool()).unwrap_or(false);
        let char_count = prev.get("charCount").and_then(|v| v.as_i64()).unwrap_or(0);
        let duration_sec = prev.get("durationSec").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let is_empty = prev.get("isEmpty").and_then(|v| v.as_bool()).unwrap_or(false);
        let app_id = prev.get("appId").and_then(|v| v.as_str()).map(|s| s.to_string());
        let app_name = prev.get("appName").and_then(|v| v.as_str()).map(|s| s.to_string());
        let audio_file_path = prev.get("audioFilePath").and_then(|v| v.as_str()).map(|s| s.to_string());
        let new_json = serde_json::to_string(&next).unwrap_or_else(|_| "{}".to_string());

        db.execute(
            "UPDATE history_records SET timestamp=?1, favorite=?2, char_count=?3, duration_sec=?4, is_empty=?5, app_id=?6, app_name=?7, audio_file_path=?8, raw_json=?9 WHERE id=?10",
            params![timestamp, favorite as i32, char_count, duration_sec, is_empty as i32, app_id, app_name, audio_file_path, new_json, id],
        )?;

        // Update stats: subtract old, add new
        self.update_stats_replacement(&db, prev_chars, prev_dur, char_count, duration_sec);
        Ok(())
    }

    pub fn history_delete(&self, id: &str) -> SqlResult<()> {
        let mut db = self.conn();
        let db = db.transaction()?;

        let row: Option<(i64, String)> = db.query_row(
            "SELECT list_order, raw_json FROM history_records WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?; // [自訂] S61：清除失敗必須傳回，僅不存在可以忽略。

        let (list_order, raw_json) = match row {
            Some(r) => r,
            None => return Ok(()),
        };

        let obj: serde_json::Map<String, Value> = serde_json::from_str(&raw_json).unwrap_or_default();
        let char_count = obj.get("charCount").and_then(|v| v.as_i64()).unwrap_or(0);
        let duration_sec = obj.get("durationSec").and_then(|v| v.as_f64()).unwrap_or(0.0);

        // 後面的記錄整體前移一位不是必須的（排序允許有空位），保留是為了和舊資料的形態一致。
        db.execute("DELETE FROM history_records WHERE id = ?1", params![id])?;
        db.execute("UPDATE history_records SET list_order = list_order - 1 WHERE list_order > ?1", params![list_order])?;

        self.update_stats_delta(&db, char_count, duration_sec, -1);
        db.commit()
    }

    // [自訂] REV9：統計及清除直接使用資料表，SQL 錯誤不折疊為空資料。
    pub(crate) fn retained_history_count(&self) -> SqlResult<usize> {
        Self::count_retained_history_on(&self.conn())
    }

    fn count_retained_history_on(db: &Connection) -> SqlResult<usize> {
        db.query_row("SELECT COUNT(*) FROM history_records", [], |row| row.get(0))
    }

    pub(crate) fn clear_retained_history(&self, confirmed_count: usize) -> Result<(), String> {
        let mut db = self.conn();
        let tx = db.transaction().map_err(|error| error.to_string())?;
        if Self::count_retained_history_on(&tx).map_err(|error| error.to_string())? > confirmed_count {
            return Err("Retained history changed after confirmation".into());
        }
        tx.execute("DELETE FROM history_records", []).map_err(|error| error.to_string())?;
        let stats = r#"{"totalDurationSec":0,"totalChars":0}"#;
        tx.execute(
            "INSERT INTO app_settings (key, value_json, updated_at) VALUES ('stats', ?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at",
            params![stats, chrono::Utc::now().timestamp_millis()],
        ).map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())
    }

    pub fn history_set_favorite(&self, id: &str, favorite: bool) -> SqlResult<()> {
        self.history_update(id, &serde_json::json!({ "favorite": favorite }))
    }

    // ─── Collections ───

    fn read_collection(&self, key: &str) -> SqlResult<Value> {
        let table = collection_table(key).ok_or(rusqlite::Error::InvalidParameterName("unknown collection".into()))?;
        let db = self.conn();
        let mut stmt = db.prepare(&format!("SELECT raw_json FROM {} ORDER BY list_order ASC", table))?;
        let rows = stmt.query_map([], |row| {
            let s: String = row.get(0)?;
            Ok(s)
        })?;

        let items: Vec<Value> = rows
            .filter_map(|r| r.ok())
            .filter_map(|s| serde_json::from_str(&s).ok())
            .collect();

        Ok(Value::Array(items))
    }

    fn replace_collection_on(db: &Connection, key: &str, items: &[Value]) -> SqlResult<()> {
        let table = collection_table(key).ok_or(rusqlite::Error::InvalidParameterName(
            "unknown collection".into(),
        ))?;

        db.execute(&format!("DELETE FROM {}", table), [])?;

        for (index, item) in items.iter().enumerate() {
            let obj = item.as_object();
            let id = obj
                .and_then(|o| o.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let raw_json = serde_json::to_string(item).unwrap_or_else(|_| "{}".to_string());

            match key {
                "history" => {
                    let timestamp = obj
                        .and_then(|o| o.get("timestamp"))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let favorite = obj
                        .and_then(|o| o.get("favorite"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let char_count = obj
                        .and_then(|o| o.get("charCount"))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let duration_sec = obj
                        .and_then(|o| o.get("durationSec"))
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let is_empty = obj
                        .and_then(|o| o.get("isEmpty"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let app_id = obj
                        .and_then(|o| o.get("appId"))
                        .and_then(|v| v.as_str());
                    let app_name = obj
                        .and_then(|o| o.get("appName"))
                        .and_then(|v| v.as_str());
                    db.execute(
                        "INSERT INTO history_records (id, list_order, timestamp, favorite, char_count, duration_sec, is_empty, app_id, app_name, raw_json) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                        params![id, index as i64, timestamp, favorite as i32, char_count, duration_sec, is_empty as i32, app_id, app_name, raw_json],
                    )?;
                }
                _ => {
                    // Generic collection insert (manual_corrections, feedback_queue, prompt_presets, app_prompt_rules)
                    let name = obj
                        .and_then(|o| o.get("name"))
                        .and_then(|v| v.as_str());
                    match table {
                        "manual_corrections" => {
                            let created_at = obj
                                .and_then(|o| o.get("createdAt"))
                                .and_then(|v| v.as_i64())
                                .unwrap_or(0);
                            let history_id = obj
                                .and_then(|o| o.get("historyId"))
                                .and_then(|v| v.as_str());
                            db.execute(
                                "INSERT INTO manual_corrections (id, list_order, created_at, history_id, raw_json) VALUES (?1,?2,?3,?4,?5)",
                                params![id, index as i64, created_at, history_id, raw_json],
                            )?;
                        }
                        "feedback_queue" => {
                            let created_at = obj
                                .and_then(|o| o.get("createdAt"))
                                .and_then(|v| v.as_i64())
                                .unwrap_or(0);
                            let history_id = obj
                                .and_then(|o| o.get("historyId"))
                                .and_then(|v| v.as_str());
                            let status = obj
                                .and_then(|o| o.get("status"))
                                .and_then(|v| v.as_str());
                            db.execute(
                                "INSERT INTO feedback_queue (id, list_order, created_at, history_id, status, raw_json) VALUES (?1,?2,?3,?4,?5,?6)",
                                params![id, index as i64, created_at, history_id, status, raw_json],
                            )?;
                        }
                        "prompt_presets" => {
                            db.execute(
                                "INSERT INTO prompt_presets (id, list_order, name, raw_json) VALUES (?1,?2,?3,?4)",
                                params![id, index as i64, name, raw_json],
                            )?;
                        }
                        "app_prompt_rules" => {
                            let app_id = obj
                                .and_then(|o| o.get("appId"))
                                .and_then(|v| v.as_str());
                            let enabled = obj
                                .and_then(|o| o.get("enabled"))
                                .and_then(|v| v.as_bool())
                                .unwrap_or(true);
                            let priority = obj
                                .and_then(|o| o.get("priority"))
                                .and_then(|v| v.as_i64())
                                .unwrap_or(0);
                            db.execute(
                                "INSERT INTO app_prompt_rules (id, list_order, app_id, name, enabled, priority, raw_json) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                                params![id, index as i64, app_id, name, enabled as i32, priority, raw_json],
                            )?;
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }

    fn replace_collection(&self, key: &str, items: &[Value]) -> SqlResult<()> {
        let mut db = self.conn();
        let tx = db.transaction()?;
        Self::replace_collection_on(&tx, key, items)?;
        tx.commit()
    }

    // ─── Stats helpers ───

    fn update_stats_delta(&self, db: &Connection, char_count: i64, duration_sec: f64, direction: i64) {
        let stats_json: Option<String> = db.query_row(
            "SELECT value_json FROM app_settings WHERE key = 'stats'",
            [], |row| row.get(0),
        ).ok();

        let (mut total_dur, mut total_chars) = parse_stats(&stats_json);
        total_dur = (total_dur + duration_sec * direction as f64).max(0.0);
        total_chars = (total_chars + char_count * direction).max(0);

        let new_stats = serde_json::json!({"totalDurationSec": total_dur, "totalChars": total_chars});
        let now = chrono::Utc::now().timestamp_millis();
        let _ = db.execute(
            "INSERT INTO app_settings (key, value_json, updated_at) VALUES ('stats', ?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at",
            params![new_stats.to_string(), now],
        );
    }

    fn update_stats_replacement(&self, db: &Connection, prev_chars: i64, prev_dur: f64, next_chars: i64, next_dur: f64) {
        let stats_json: Option<String> = db.query_row(
            "SELECT value_json FROM app_settings WHERE key = 'stats'",
            [], |row| row.get(0),
        ).ok();

        let (mut total_dur, mut total_chars) = parse_stats(&stats_json);
        total_dur = (total_dur - prev_dur + next_dur).max(0.0);
        total_chars = (total_chars - prev_chars + next_chars).max(0);

        let new_stats = serde_json::json!({"totalDurationSec": total_dur, "totalChars": total_chars});
        let now = chrono::Utc::now().timestamp_millis();
        let _ = db.execute(
            "INSERT INTO app_settings (key, value_json, updated_at) VALUES ('stats', ?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at",
            params![new_stats.to_string(), now],
        );
    }
}

impl Storage {
    /// Migrate data from the Electron app's SQLite database (one-time).
    /// Looks for sayit.db in common Electron userData paths.
    ///
    /// [自訂] S37：一律不做任何事（不檢查、不匯入舊資料夾）。呼叫處在 main.rs 也已註解停用。
    #[allow(dead_code, unreachable_code)]
    pub fn migrate_from_electron(&self) -> Result<(), String> {
        return Ok(()); // [自訂] S37：短路，下方原碼保留供日後移除
        let db = self.conn();

        // Check if already migrated
        let already: bool = db.query_row(
            "SELECT COUNT(*) > 0 FROM db_meta WHERE key = 'electron_data_migrated_v2'",
            [],
            |row| row.get(0),
        ).unwrap_or(false);

        if already {
            log::info!("Electron data migration already done, skipping");
            return Ok(());
        }

        // Find the Electron DB
        let electron_db_path = Self::find_electron_db();
        let electron_path = match electron_db_path {
            Some(p) => p,
            None => {
                log::info!("No Electron sayit.db found, skipping migration");
                return Ok(());
            }
        };

        log::info!("Found Electron DB at: {:?}", electron_path);

        // Open the Electron DB read-only
        let src = Connection::open_with_flags(
            &electron_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ).map_err(|e| format!("Failed to open Electron DB: {}", e))?;

        // Migrate each table
        let mut migrated_counts: Vec<(String, usize)> = Vec::new();

        // 1. app_settings
        let count = Self::migrate_table_settings(&src, &db);
        migrated_counts.push(("app_settings".into(), count));

        // 2. history_records
        let count = Self::migrate_table_generic(&src, &db, "history_records",
            "id, list_order, timestamp, favorite, char_count, duration_sec, is_empty, app_id, app_name, audio_file_path, raw_json");
        migrated_counts.push(("history_records".into(), count));

        // 3. manual_corrections
        let count = Self::migrate_table_generic(&src, &db, "manual_corrections",
            "id, list_order, created_at, history_id, raw_json");
        migrated_counts.push(("manual_corrections".into(), count));

        // 4. feedback_queue
        let count = Self::migrate_table_generic(&src, &db, "feedback_queue",
            "id, list_order, created_at, history_id, status, raw_json");
        migrated_counts.push(("feedback_queue".into(), count));

        // 5. prompt_presets
        let count = Self::migrate_table_generic(&src, &db, "prompt_presets",
            "id, list_order, name, raw_json");
        migrated_counts.push(("prompt_presets".into(), count));

        // 6. app_prompt_rules
        let count = Self::migrate_table_generic(&src, &db, "app_prompt_rules",
            "id, list_order, app_id, name, enabled, priority, raw_json");
        migrated_counts.push(("app_prompt_rules".into(), count));

        // Mark migration as done
        let now = chrono::Utc::now().timestamp_millis();
        let _ = db.execute(
            "INSERT INTO db_meta (key, value_json, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at",
            params![
                "electron_data_migrated_v2",
                now.to_string(),
                now
            ],
        );
        let _ = db.execute(
            "INSERT INTO db_meta (key, value_json, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at",
            params![
                "electron_data_migrated_at",
                now.to_string(),
                now
            ],
        );
        let _ = db.execute(
            "INSERT INTO db_meta (key, value_json, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at",
            params![
                "electron_db_source_path",
                serde_json::json!(electron_path.to_string_lossy()).to_string(),
                now
            ],
        );

        log::info!("Electron data migration complete: {:?}", migrated_counts);
        Ok(())
    }

    #[allow(unreachable_code)]
    fn find_electron_db() -> Option<PathBuf> {
        return None; // [自訂] S37：不探測舊 SayIt／sayit／SayIt-dev／sayit-dev 資料夾
        // Electron's app.getPath('userData') on Windows is typically:
        //   C:\Users\<user>\AppData\Roaming\<appName>
        // The app name could be "SayIt", "sayit", or "SayIt-dev"
        let roaming = dirs::data_dir()?; // AppData\Roaming
        let candidates = [
            roaming.join("SayIt").join("sayit.db"),
            roaming.join("sayit").join("sayit.db"),
            roaming.join("SayIt-dev").join("sayit.db"),
            roaming.join("sayit-dev").join("sayit.db"),
        ];

        for path in &candidates {
            if path.exists() {
                return Some(path.clone());
            }
        }
        None
    }

    fn migrate_table_settings(src: &Connection, dst: &Connection) -> usize {
        let mut count = 0usize;
        let result = src.prepare("SELECT key, value_json, updated_at FROM app_settings");
        let mut stmt = match result {
            Ok(s) => s,
            Err(_) => return 0,
        };

        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        });

        if let Ok(rows) = rows {
            for row in rows.flatten() {
                let (key, value_json, updated_at) = row;
                // Always overwrite: Electron data takes priority over seed defaults
                let res = dst.execute(
                    "INSERT INTO app_settings (key, value_json, updated_at) VALUES (?1, ?2, ?3)
                     ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at",
                    params![key, value_json, updated_at],
                );
                if res.is_ok() {
                    count += 1;
                }
            }
        }
        count
    }

    fn migrate_table_generic(src: &Connection, dst: &Connection, table: &str, columns: &str) -> usize {
        // Check if destination table already has data — skip if so
        let dst_count: i64 = dst.query_row(
            &format!("SELECT COUNT(*) FROM {}", table),
            [],
            |row| row.get(0),
        ).unwrap_or(0);

        if dst_count > 0 {
            log::info!("Table {} already has {} rows, skipping", table, dst_count);
            return 0;
        }

        let select_sql = format!("SELECT {} FROM {} ORDER BY list_order ASC", columns, table);
        let mut stmt = match src.prepare(&select_sql) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("Cannot read source table {}: {}", table, e);
                return 0;
            }
        };

        let col_count = columns.split(',').count();
        let placeholders: Vec<String> = (1..=col_count).map(|i| format!("?{}", i)).collect();
        let insert_sql = format!(
            "INSERT OR IGNORE INTO {} ({}) VALUES ({})",
            table, columns, placeholders.join(", ")
        );

        let mut count = 0usize;
        let rows = stmt.query_map([], |row| {
            let mut values: Vec<rusqlite::types::Value> = Vec::with_capacity(col_count);
            for i in 0..col_count {
                values.push(row.get(i)?);
            }
            Ok(values)
        });

        if let Ok(rows) = rows {
            for row in rows.flatten() {
                let params: Vec<&dyn rusqlite::types::ToSql> = row.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();
                if dst.execute(&insert_sql, params.as_slice()).is_ok() {
                    count += 1;
                }
            }
        }
        count
    }
}

fn parse_stats(json: &Option<String>) -> (f64, i64) {
    match json {
        Some(s) => {
            let v: Value = serde_json::from_str(s).unwrap_or(Value::Null);
            let dur = v.get("totalDurationSec").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let chars = v.get("totalChars").and_then(|v| v.as_i64()).unwrap_or(0);
            (dur, chars)
        }
        None => (0.0, 0),
    }
}

// ─── 備份 / 恢復輔助（匯入匯出用）───

/// [自訂] S35／S36：排除清單比對。一般項目整個 key 相同才算；結尾為 `*` 的項目做前綴比對
/// （例如 `cloudAi.*` 會排除 `cloudAi.apiKey`、`cloudAi.profiles`、`cloudAi.deepseek.apiKey`）。
/// 匯出與匯入共用這一個判斷，兩個方向不會不一致。
pub fn is_settings_key_excluded(key: &str, exclude: &[&str]) -> bool {
    exclude.iter().any(|pattern| match pattern.strip_suffix('*') {
        Some(prefix) => key.starts_with(prefix),
        None => key == *pattern,
    })
}

pub(crate) struct BackupSnapshot<'a>(&'a Connection);

fn log_backup_sqlite_error(operation: &str, error: &rusqlite::Error) {
    if let rusqlite::Error::SqliteFailure(code, _) = error {
        log::warn!("Backup database {operation}: {:?} ({})", code.code, code.extended_code);
    } else {
        let category = match error {
            rusqlite::Error::InvalidColumnType(..) => "column type",
            rusqlite::Error::FromSqlConversionFailure(..) => "value conversion",
            rusqlite::Error::QueryReturnedNoRows => "missing row",
            _ => "query error",
        };
        log::warn!("Backup database {operation}: {category}");
    }
}

impl BackupSnapshot<'_> {
    pub(crate) fn settings(&self, exclude: &[&str]) -> Result<Value, String> {
        let read_error = |error| {
            log_backup_sqlite_error("read settings", &error);
            "BACKUP_READ_SETTINGS".to_owned()
        };
        let mut stmt = self.0.prepare("SELECT key, value_json FROM app_settings").map_err(read_error)?;
        let mut rows = stmt.query([]).map_err(read_error)?;
        let mut settings = serde_json::Map::new();
        while let Some(row) = rows.next().map_err(read_error)? {
            let key: String = row.get(0).map_err(read_error)?;
            if !is_settings_key_excluded(&key, exclude) {
                if let Some(value) = Self::parse_setting(&key, row.get_ref(1).map_err(read_error)?) {
                    settings.insert(key, value);
                }
            }
        }
        Ok(Value::Object(settings))
    }

    // A missing setting has its normal null default. Malformed JSON is omitted.
    pub(crate) fn setting(&self, key: &str) -> Result<Option<Value>, String> {
        let value = self.0.query_row(
            "SELECT value_json FROM app_settings WHERE key = ?1",
            [key],
            |row| Ok(Self::parse_setting(key, row.get_ref(0)?)),
        ).optional().map_err(|error| {
            log_backup_sqlite_error("read setting", &error);
            format!("BACKUP_READ_SETTINGS|{key}")
        })?;
        Ok(value.unwrap_or(Some(Value::Null)))
    }

    fn parse_setting(key: &str, raw: rusqlite::types::ValueRef<'_>) -> Option<Value> {
        if let rusqlite::types::ValueRef::Text(bytes) = raw {
            if let Ok(value) = serde_json::from_slice(bytes) {
                return Some(value);
            }
        }
        log::warn!("Skipping malformed backup setting {key:?}");
        None
    }

    pub(crate) fn collection(&self, key: &str) -> Result<Value, String> {
        let read_error = |error| {
            log_backup_sqlite_error("read collection", &error);
            format!("BACKUP_READ_COLLECTION|{key}")
        };
        let table = collection_table(key).ok_or_else(|| format!("BACKUP_READ_COLLECTION|{key}"))?;
        let mut stmt = self.0.prepare(&format!("SELECT raw_json FROM {table} ORDER BY list_order ASC"))
            .map_err(read_error)?;
        let mut rows = stmt.query([]).map_err(read_error)?;
        let mut items = Vec::new();
        loop {
            let row_error = |error| {
                log_backup_sqlite_error("read collection row", &error);
                format!("BACKUP_READ_COLLECTION|{key}|{}", items.len() + 1)
            };
            let Some(row) = rows.next().map_err(row_error)? else { break };
            let raw: String = row.get(0).map_err(row_error)?;
            let item = serde_json::from_str(&raw).map_err(|_| {
                format!("BACKUP_READ_COLLECTION|{key}|{}", items.len() + 1)
            })?;
            items.push(item);
        }
        Ok(Value::Array(items))
    }
}

impl Storage {
    pub(crate) fn with_backup_snapshot<T>(
        &self,
        read: impl FnOnce(&BackupSnapshot<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut db = self.db.lock().map_err(|_| "BACKUP_READ_DATABASE")?;
        let read_error = |error| {
            log_backup_sqlite_error("snapshot transaction", &error);
            "BACKUP_READ_DATABASE"
        };
        let tx = db.transaction().map_err(read_error)?;
        let result = read(&BackupSnapshot(&tx))?;
        tx.commit().map_err(read_error)?;
        Ok(result)
    }

    /// 原子應用一組配置：設定 key 與可選集合要麼全部成功，要麼全部回滾。
    pub fn apply_config_transaction(
        &self,
        app_settings: &serde_json::Map<String, Value>,
        exclude: &[&str],
        prompt_presets: Option<&[Value]>,
        app_prompt_rules: Option<&[Value]>,
    ) -> Result<(), String> {
        // [自訂] SC-56：寫入層的縱深檢查，入口漏掉時也不寫進記憶體庫。
        self.ensure_persistent()?;
        self.apply_config_sql(app_settings, exclude, prompt_presets, app_prompt_rules)
            .map_err(|error| format!("Failed to write configuration: {}", error))
    }

    fn apply_config_sql(
        &self,
        app_settings: &serde_json::Map<String, Value>,
        exclude: &[&str],
        prompt_presets: Option<&[Value]>,
        app_prompt_rules: Option<&[Value]>,
    ) -> SqlResult<()> {
        let mut db = self.conn();
        let log_error = |error: &rusqlite::Error| log_backup_sqlite_error("import configuration", error);
        let tx = db.transaction().inspect_err(log_error)?;
        Self::apply_backup_data_on(&tx, app_settings, exclude, &[
            ("promptPresets", prompt_presets),
            ("appPromptRules", app_prompt_rules),
        ]).inspect_err(log_error)?;
        tx.commit().inspect_err(log_error)
    }

    pub(crate) fn apply_restore_transaction(
        &self,
        app_settings: &serde_json::Map<String, Value>,
        exclude: &[&str],
        collections: &[(&str, Option<&[Value]>)],
        dry_run: bool,
    ) -> Result<(), String> {
        // [自訂] SC-56：寫入層的縱深檢查。
        self.ensure_persistent()?;
        let mut db = self.db.lock().map_err(|_| "BACKUP_RESTORE_DB|lock")?;
        let restore_error = |stage| move |error| {
            log_backup_sqlite_error(stage, &error);
            format!("BACKUP_RESTORE_DB|{stage}")
        };
        let tx = db.transaction().map_err(restore_error("begin"))?;
        Self::apply_backup_data_on(&tx, app_settings, exclude, collections)
            .map_err(restore_error("write"))?;
        if dry_run {
            // Deferred foreign keys are otherwise checked only at commit.
            let violation = tx.query_row("PRAGMA foreign_key_check", [], |_| Ok(()))
                .optional().map_err(restore_error("check"))?;
            if violation.is_some() {
                return Err("BACKUP_RESTORE_DB|constraint".into());
            }
            tx.rollback().map_err(restore_error("rollback"))?;
        } else {
            tx.commit().map_err(restore_error("commit"))?;
        }
        Ok(())
    }

    fn apply_backup_data_on(
        db: &Connection,
        app_settings: &serde_json::Map<String, Value>,
        exclude: &[&str],
        collections: &[(&str, Option<&[Value]>)],
    ) -> SqlResult<()> {
        let now = chrono::Utc::now().timestamp_millis();

        for (key, value) in app_settings {
            if is_settings_key_excluded(key, exclude) { // [自訂] S36：支援前綴比對
                continue;
            }
            let value = &sanitize_setting_value(key, value); // [自訂] #20：不保存金鑰／密碼
            let json_str = serde_json::to_string(value).unwrap_or_else(|_| "null".to_string());
            db.execute(
                "INSERT INTO app_settings (key, value_json, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at",
                params![key, json_str, now],
            )?;
        }

        for (key, items) in collections {
            if let Some(items) = items {
                Self::replace_collection_on(db, key, items)?;
            }
        }
        Ok(())
    }
}


// ─── [自訂] #20／L-70：不在 sayit.db 保存金鑰與密碼 ───
// 本 fork 只用本機 Ollama（不需金鑰），雲端 ASR／AI 與 WebDAV 都已停用，
// 因此金鑰、密碼一律不寫入資料庫（存成空字串），啟動時也清除既有的值。
// 範圍：`cloudAi.*`、`cloudAsr.*`、`webdav.*` 中欄位名為 apiKey／consoleKey／otherKey／accessToken／password 者，
// 以及 `cloudAi.profiles`、`cloudAsr.profiles` 等設定值裡同名的巢狀欄位。

// [自訂] SCAN1-1（L-71）：補上 `otherKey`（豆包另一代的金鑰，存在 cloudAsr.profiles[] 裡）與 `accessToken`。
const SECRET_FIELD_NAMES: &[&str] = &["apiKey", "consoleKey", "otherKey", "accessToken", "password"];

fn is_secret_scope(key: &str) -> bool {
    key.starts_with("cloudAi.") || key.starts_with("cloudAsr.") || key.starts_with("webdav.")
}

fn is_secret_setting_key(key: &str) -> bool {
    is_secret_scope(key)
        && key.rsplit('.').next().map_or(false, |last| SECRET_FIELD_NAMES.contains(&last))
}

fn strip_secret_fields(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if SECRET_FIELD_NAMES.contains(&k.as_str()) && v.is_string() {
                    *v = Value::String(String::new());
                } else {
                    strip_secret_fields(v);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(strip_secret_fields),
        _ => {}
    }
}

/// 寫入前清掉金鑰／密碼；其他設定原樣返回。
pub(crate) fn sanitize_setting_value(key: &str, value: &Value) -> Value {
    if is_secret_setting_key(key) {
        return Value::String(String::new());
    }
    if is_secret_scope(key) {
        let mut cleaned = value.clone();
        strip_secret_fields(&mut cleaned);
        return cleaned;
    }
    value.clone()
}

impl Storage {
    /// 啟動時呼叫：把資料庫裡既有的金鑰、密碼改成空字串。回傳改動的筆數。
    pub fn purge_stored_secrets(&self) -> SqlResult<usize> {
        let db = self.conn();
        let rows: Vec<(String, String)> = {
            let mut stmt = db.prepare(
                "SELECT key, value_json FROM app_settings
                 WHERE key LIKE 'cloudAi.%' OR key LIKE 'cloudAsr.%' OR key LIKE 'webdav.%'",
            )?;
            let mapped = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
            mapped.flatten().collect()
        };
        let mut changed = 0;
        for (key, value_json) in rows {
            // REV1：值不是合法 JSON 時，金鑰欄位直接改成空字串；其他無法解析的值只計數、記錄 key 名，不記內容。
            let Ok(value) = serde_json::from_str::<Value>(&value_json) else {
                if is_secret_setting_key(&key) {
                    db.execute(
                        "UPDATE app_settings SET value_json = '\"\"' WHERE key = ?1",
                        params![key],
                    )?;
                    changed += 1;
                } else {
                    log::warn!("[storage] could not parse setting {} while clearing secrets; left unchanged", key);
                }
                continue;
            };
            let cleaned = sanitize_setting_value(&key, &value);
            if cleaned != value {
                let json_str = serde_json::to_string(&cleaned).unwrap_or_else(|_| "\"\"".to_string());
                db.execute(
                    "UPDATE app_settings SET value_json = ?1 WHERE key = ?2",
                    params![json_str, key],
                )?;
                changed += 1;
            }
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod history_order_tests {
    use super::*;
    use serde_json::json;

    fn ids(storage: &Storage) -> Vec<String> {
        storage
            .history_list(None, false, None, None)
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect()
    }

    /// 新記錄排最前，刪掉中間一條後順序不亂，再加一條仍排最前；統計跟著增減。
    /// 也覆蓋「舊庫裡 list_order 從 0 開始連續」和「新寫法取最小值 - 1」混用的情況。
    #[test]
    fn newest_first_survives_add_delete_and_old_rows() {
        let dir = std::env::temp_dir().join(format!("sayit-history-order-{}", uuid::Uuid::new_v4()));
        let storage = Storage::new(dir.join("t.db")).unwrap();
        storage
            .set("history", &json!([{ "id": "old1", "charCount": 1 }, { "id": "old2", "charCount": 1 }]))
            .unwrap();
        storage.history_add(&json!({ "id": "a", "charCount": 3, "durationSec": 1.5 })).unwrap();
        storage.history_add(&json!({ "id": "b", "charCount": 4, "durationSec": 2.0 })).unwrap();
        assert_eq!(ids(&storage), ["b", "a", "old1", "old2"]);

        storage.history_delete("a").unwrap();
        assert_eq!(ids(&storage), ["b", "old1", "old2"]);
        storage.history_add(&json!({ "id": "c", "charCount": 5 })).unwrap();
        assert_eq!(ids(&storage), ["c", "b", "old1", "old2"]);

        let stats = storage.get("stats", None);
        assert_eq!(stats["totalChars"], json!(4 + 5));

        let page = storage.history_list(None, false, Some(2), Some(1));
        assert_eq!(page.iter().map(|r| r["id"].as_str().unwrap()).collect::<Vec<_>>(), ["b", "old1"]);

        drop(storage);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[test]
    fn corrupt_database_is_moved_aside_and_a_fresh_one_opens() {
        let dir = std::env::temp_dir().join(format!("sayit-recovery-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let db = dir.join("sayit.db");
        fs::write(&db, b"this is not a sqlite database at all, just garbage bytes").unwrap();

        let (storage, recovery) = Storage::open_or_recover(db.clone());
        match recovery {
            Some(StorageRecovery::MovedAside { backup, .. }) => {
                assert!(backup.exists());
                assert_eq!(fs::read(&backup).unwrap(), b"this is not a sqlite database at all, just garbage bytes");
            }
            other => panic!("expected MovedAside, got {other:?}"),
        }
        storage.set("overlayWidth", &serde_json::json!("long")).unwrap();
        assert_eq!(storage.get("overlayWidth", None), serde_json::json!("long"));

        drop(storage);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn healthy_database_needs_no_recovery() {
        let dir = std::env::temp_dir().join(format!("sayit-recovery-{}", uuid::Uuid::new_v4()));
        let (storage, recovery) = Storage::open_or_recover(dir.join("sayit.db"));
        assert!(recovery.is_none());
        drop(storage);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn poisoned_lock_does_not_break_later_reads() {
        let dir = std::env::temp_dir().join(format!("sayit-recovery-{}", uuid::Uuid::new_v4()));
        let storage = std::sync::Arc::new(Storage::new(dir.join("sayit.db")).unwrap());
        let s2 = storage.clone();
        let _ = std::thread::spawn(move || {
            let _guard = s2.db.lock().unwrap();
            panic!("simulated panic while holding the database lock");
        })
        .join();
        assert!(storage.db.is_poisoned());
        storage.set("k", &serde_json::json!(1)).unwrap();
        assert_eq!(storage.get("k", None), serde_json::json!(1));
        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod secret_tests {
    // [自訂] #20／L-70
    use super::*;
    use serde_json::json;

    #[test]
    fn secrets_are_never_stored_and_existing_ones_are_purged() {
        let dir = std::env::temp_dir().join(format!("sayit-secrets-{}", uuid::Uuid::new_v4()));
        let storage = Storage::new(dir.join("t.db")).unwrap();

        storage.set("cloudAi.apiKey", &json!("sk-secret")).unwrap();
        storage.set("cloudAi.qwen.apiKey", &json!("sk-secret")).unwrap();
        storage.set("cloudAsr.doubao.consoleKey", &json!("secret")).unwrap();
        storage.set("webdav.password", &json!("secret")).unwrap();
        storage
            .set("cloudAi.profiles", &json!([{ "id": "p1", "provider": "ollama", "apiKey": "sk-secret", "model": "m" }]))
            .unwrap();
        storage.set("cloudAi.apiUrl", &json!("http://127.0.0.1:11434")).unwrap();
        storage.set("webdav.username", &json!("me")).unwrap();

        assert_eq!(storage.get("cloudAi.apiKey", None), json!(""));
        assert_eq!(storage.get("cloudAi.qwen.apiKey", None), json!(""));
        assert_eq!(storage.get("cloudAsr.doubao.consoleKey", None), json!(""));
        assert_eq!(storage.get("webdav.password", None), json!(""));
        let profiles = storage.get("cloudAi.profiles", None);
        assert_eq!(profiles[0]["apiKey"], json!(""));
        assert_eq!(profiles[0]["model"], json!("m"));
        // 非機密設定照常保存
        assert_eq!(storage.get("cloudAi.apiUrl", None), json!("http://127.0.0.1:11434"));
        assert_eq!(storage.get("webdav.username", None), json!("me"));

        // 模擬舊資料庫裡已有明文金鑰：直接寫入後再清除
        storage
            .conn()
            .execute(
                "INSERT INTO app_settings (key, value_json, updated_at) VALUES ('cloudAsr.apiKey', '\"old\"', 0)
                 ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
                [],
            )
            .unwrap();
        assert_eq!(storage.purge_stored_secrets().unwrap(), 1);
        assert_eq!(storage.get("cloudAsr.apiKey", None), json!(""));
        assert_eq!(storage.purge_stored_secrets().unwrap(), 0);

        // SCAN1-1（L-71）：cloudAsr.profiles[] 裡的 otherKey 也要清掉（寫入與舊資料兩種情況）
        storage
            .set(
                "cloudAsr.profiles",
                &json!([{ "id": "a1", "provider": "doubao_v2", "apiKey": "k1", "otherKey": "k2", "model": "m" }]),
            )
            .unwrap();
        let asr = storage.get("cloudAsr.profiles", None);
        assert_eq!(asr[0]["apiKey"], json!(""));
        assert_eq!(asr[0]["otherKey"], json!(""));
        assert_eq!(asr[0]["model"], json!("m"));
        storage
            .conn()
            .execute(
                "UPDATE app_settings SET value_json = '[{\"id\":\"a1\",\"otherKey\":\"old\",\"accessToken\":\"old\"}]' WHERE key = 'cloudAsr.profiles'",
                [],
            )
            .unwrap();
        assert_eq!(storage.purge_stored_secrets().unwrap(), 1);
        let asr = storage.get("cloudAsr.profiles", None);
        assert_eq!(asr[0]["otherKey"], json!(""));
        assert_eq!(asr[0]["accessToken"], json!(""));
        assert_eq!(asr[0]["id"], json!("a1"));

        // REV1：金鑰欄位的值不是合法 JSON 時，也要清掉
        storage
            .conn()
            .execute("UPDATE app_settings SET value_json = 'not-json' WHERE key = 'cloudAi.apiKey'", [])
            .unwrap();
        assert_eq!(storage.purge_stored_secrets().unwrap(), 1);
        assert_eq!(storage.get("cloudAi.apiKey", None), json!(""));

        drop(storage);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
