use super::*;
use rusqlite::{ffi, Connection};
use serde_json::json;
use std::{cell::Cell, time::Instant};

struct Fixture {
    dir: PathBuf,
    db: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("sayit-db-recovery-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let db = dir.join("sayit.db");
        Self { dir, db }
    }

    fn open(&self) -> (Storage, Option<StorageRecovery>) {
        open_or_recover(&self.db, Duration::from_millis(12))
    }

    fn journal_db(&self) -> Connection {
        let mut db = Connection::open(&self.db).unwrap();
        Storage::initialize_schema(&mut db).unwrap();
        db
    }

    fn write_broken_group(&self) {
        for suffix in MEMBERS {
            fs::write(append_suffix(&self.db, suffix), format!("old data {suffix}")).unwrap();
        }
    }

    fn assert_volatile_unchanged(&self, before: [Option<Vec<u8>>; 4]) {
        let (storage, mode) = self.open();
        assert!(matches!(mode, Some(StorageRecovery::InMemory)), "{mode:?}");
        assert_eq!(snapshot(&self.db), before);
        assert!(!recovery_move_pending(&self.dir));
        assert!(!recording_cleanup_protected(&self.dir));
        assert_eq!(storage.conn().path(), Some(""));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn snapshot(base: &Path) -> [Option<Vec<u8>>; 4] {
    MEMBERS.map(|suffix| match fs::read(append_suffix(base, suffix)) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => panic!("cannot snapshot test group: {error}"),
    })
}

fn sqlite_error(code: i32) -> OpenError {
    OpenError::Sql(rusqlite::Error::SqliteFailure(ffi::Error::new(code), None))
}

#[test]
fn only_two_consecutive_sqlite_corruption_results_authorize_a_move() {
    for code in [
        ffi::SQLITE_BUSY, ffi::SQLITE_LOCKED, ffi::SQLITE_CANTOPEN, ffi::SQLITE_PERM,
        ffi::SQLITE_READONLY, ffi::SQLITE_IOERR, ffi::SQLITE_IOERR_CORRUPTFS,
        ffi::SQLITE_FULL, ffi::SQLITE_NOMEM, ffi::SQLITE_SCHEMA, ffi::SQLITE_ERROR,
        ffi::SQLITE_MISMATCH,
    ] {
        assert!(!sqlite_error(code).is_corruption(), "{code}");
    }
    for code in [ffi::SQLITE_CORRUPT, ffi::SQLITE_CORRUPT_INDEX, ffi::SQLITE_NOTADB] {
        assert!(sqlite_error(code).is_corruption(), "{code}");
        let recoverable = retry_open(Duration::ZERO, |_| Err(sqlite_error(code)));
        assert!(matches!(recoverable, Err((_, true))));
        let calls = Cell::new(0);
        let result = retry_open(Duration::ZERO, |_| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                return Err(sqlite_error(code));
            }
            Ok(Storage::new(PathBuf::from(":memory:")).unwrap())
        });
        assert!(result.is_ok());
        assert_eq!(calls.get(), 2);
        for sequence in [[code, ffi::SQLITE_BUSY], [ffi::SQLITE_BUSY, code]] {
            let mut sequence = sequence.into_iter();
            assert!(matches!(retry_open(Duration::ZERO, |_| Err(sqlite_error(sequence.next().unwrap()))), Err((_, false))));
        }
    }
}

#[test]
fn exclusive_sqlite_lock_returns_memory_without_moving_the_group() {
    let fixture = Fixture::new();
    let blocker = fixture.journal_db();
    blocker.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let before = snapshot(&fixture.db);
    let started = Instant::now();
    fixture.assert_volatile_unchanged(before);
    assert!(started.elapsed() < Duration::from_secs(2));
    blocker.execute_batch("ROLLBACK").unwrap();
    let (_storage, mode) = fixture.open();
    assert!(mode.is_none());
}

#[test]
fn wal_writer_lock_does_not_trigger_recovery() {
    let fixture = Fixture::new();
    let blocker = fixture.journal_db();
    blocker.execute_batch("PRAGMA journal_mode = WAL;
        UPDATE app_settings SET value_json = 'true' WHERE key = 'autoLaunch';
        BEGIN IMMEDIATE;").unwrap();
    fixture.assert_volatile_unchanged(snapshot(&fixture.db));
    blocker.execute_batch("ROLLBACK").unwrap();
}

#[cfg(unix)]
#[test]
fn unreadable_database_and_unwritable_directory_preserve_originals() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    drop(fixture.journal_db());
    let before = snapshot(&fixture.db);
    fs::set_permissions(&fixture.db, fs::Permissions::from_mode(0)).unwrap();
    // A privileged runner can legitimately bypass permission bits.
    if File::open(&fixture.db).is_err() {
        let (_storage, mode) = fixture.open();
        assert!(matches!(mode, Some(StorageRecovery::InMemory)));
    }
    fs::set_permissions(&fixture.db, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(snapshot(&fixture.db), before);
    for lock in [STARTUP_LOCK, LIVE_LOCK] {
        let _ = fs::remove_file(fixture.dir.join(lock));
    }
    fs::set_permissions(&fixture.dir, fs::Permissions::from_mode(0o500)).unwrap();
    if fs::File::create_new(fixture.dir.join("permission-probe")).is_err() {
        let (storage, mode) = fixture.open();
        if mode.is_none() {
            assert!(storage.conn().path().is_some_and(|path| !path.is_empty()));
        } else {
            assert!(matches!(mode, Some(StorageRecovery::InMemory)), "{mode:?}");
            assert_eq!(snapshot(&fixture.db), before);
        }
        assert!(!recovery_move_pending(&fixture.dir));
        assert!(!recording_cleanup_protected(&fixture.dir));
    }
    fs::set_permissions(&fixture.dir, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(fixture.db.is_file());
}

#[test]
fn directory_paths_and_orphaned_sidecars_are_never_given_to_sqlite() {
    let fixture = Fixture::new();
    fs::create_dir(&fixture.db).unwrap();
    let (_storage, mode) = fixture.open();
    assert!(matches!(mode, Some(StorageRecovery::InMemory)));
    assert!(fixture.db.is_dir());
    let path_without_directory = fixture.db.join("not-a-dir");
    fs::write(&path_without_directory, b"ordinary file").unwrap();
    let (_storage, mode) = open_or_recover(&path_without_directory.join("sayit.db"), Duration::ZERO);
    assert!(matches!(mode, Some(StorageRecovery::InMemory)));
    assert_eq!(fs::read(&path_without_directory).unwrap(), b"ordinary file");
    fs::remove_dir_all(&fixture.db).unwrap();
    fs::write(append_suffix(&fixture.db, "-wal"), b"orphan WAL").unwrap();
    fs::write(append_suffix(&fixture.db, "-journal"), b"orphan journal").unwrap();
    fixture.assert_volatile_unchanged(snapshot(&fixture.db));
    fs::write(&fixture.db, b"").unwrap();
    fixture.assert_volatile_unchanged(snapshot(&fixture.db));
}

#[cfg(unix)]
#[test]
fn database_and_sidecar_symlinks_stay_in_place() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let target = fixture.dir.join("target");
    let target_storage = Storage::new(target.clone()).unwrap();
    target_storage.set("overlayWidth", &json!("long")).unwrap();
    drop(target_storage);
    symlink(&target, &fixture.db).unwrap();
    let (storage, mode) = fixture.open();
    assert!(mode.is_none(), "{mode:?}");
    assert_eq!(storage.get("overlayWidth", None), json!("long"));
    drop(storage);
    fs::write(&target, b"not a database").unwrap();
    let (_storage, mode) = fixture.open();
    assert!(matches!(mode, Some(StorageRecovery::InMemory)));
    assert_eq!(fs::read_link(&fixture.db).unwrap(), target);
    fs::remove_file(&fixture.db).unwrap();
    fs::write(&fixture.db, b"bad database").unwrap();
    let wal = append_suffix(&fixture.db, "-wal");
    symlink(fixture.dir.join("missing"), &wal).unwrap();
    let (_storage, mode) = fixture.open();
    assert!(matches!(mode, Some(StorageRecovery::InMemory)));
    assert!(fs::symlink_metadata(wal).unwrap().is_symlink());
    assert_eq!(fs::read(&fixture.db).unwrap(), b"bad database");
    assert_eq!(fs::read(&target).unwrap(), b"not a database");
}

#[test]
fn failed_initialization_does_not_checkpoint_or_unlink_a_preexisting_wal() {
    let fixture = Fixture::new();
    let db = fixture.journal_db();
    db.pragma_update(None, "journal_mode", "WAL").unwrap();
    db.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true).unwrap();
    db.execute_batch("ALTER TABLE history_records DROP COLUMN audio_file_path").unwrap();
    drop(db);
    let wal = append_suffix(&fixture.db, "-wal");
    let before = fs::read(&wal).unwrap();
    assert!(!before.is_empty());
    let (_storage, mode) = fixture.open();
    assert!(matches!(mode, Some(StorageRecovery::InMemory)), "{mode:?}");
    assert_eq!(fs::read(&wal).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn existing_wal_database_opens_without_written_startup_state_in_read_only_directory() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let db = fixture.journal_db();
    db.pragma_update(None, "journal_mode", "WAL").unwrap();
    db.execute_batch("UPDATE app_settings SET value_json = 'true' WHERE key = 'autoLaunch'").unwrap();
    fs::set_permissions(&fixture.dir, fs::Permissions::from_mode(0o500)).unwrap();
    let result = fixture.open();
    fs::set_permissions(&fixture.dir, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.1.is_none(), "{:?}", result.1);
    assert_eq!(result.0.get("autoLaunch", None), json!(true));
}

#[test]
fn inconsistent_schema_is_non_corrupt_and_original_bytes_are_unchanged() {
    let fixture = Fixture::new();
    let db = fixture.journal_db();
    db.execute_batch("ALTER TABLE history_records DROP COLUMN audio_file_path").unwrap();
    drop(db);
    fixture.assert_volatile_unchanged(snapshot(&fixture.db));
}

#[test]
fn sqlite_reports_a_damaged_btree_as_corrupt_and_preserves_it() {
    let fixture = Fixture::new();
    drop(fixture.journal_db());
    let mut damaged = fs::read(&fixture.db).unwrap();
    // The first b-tree page starts after the 100-byte SQLite header.
    damaged[100] = 0;
    fs::write(&fixture.db, &damaged).unwrap();
    let error = open_existing_or_empty(&fixture.db, Duration::ZERO).err().unwrap();
    assert!(matches!(error, OpenError::Sql(rusqlite::Error::SqliteFailure(code, _))
        if code.code == ErrorCode::DatabaseCorrupt));
    let (_storage, mode) = fixture.open();
    let Some(StorageRecovery::MovedAside { backup }) = mode else { panic!("{mode:?}") };
    assert_eq!(fs::read(backup).unwrap(), damaged);
}

#[test]
fn not_a_database_is_confirmed_and_existing_wal_bytes_are_preserved() {
    let fixture = Fixture::new();
    fixture.write_broken_group();
    let before = snapshot(&fixture.db);
    let (_storage, mode) = fixture.open();
    let Some(StorageRecovery::MovedAside { backup }) = mode else { panic!("{mode:?}") };
    // The pager can rebuild SHM or consume a rollback journal on its ordinary open.
    let saved = snapshot(&backup);
    assert_eq!(saved[0], before[0]);
    assert_eq!(saved[3], before[3]);
    assert!(recording_cleanup_protected(&fixture.dir));
    assert!(!recovery_move_pending(&fixture.dir));
}

#[test]
fn every_injected_move_failure_rolls_back_original_members() {
    for fail_at in 0..MEMBERS.len() {
        let fixture = Fixture::new();
        fixture.write_broken_group();
        let before = snapshot(&fixture.db);
        let plan = MovePlan::start(&fixture.dir, &fixture.db).unwrap();
        let step = Cell::new(0);
        assert!(plan.finish(&fixture.dir, &fixture.db, |from, to| {
            assert!(recording_cleanup_protected(&fixture.dir));
            assert!(fixture.dir.join(RECORDING_HOLD).is_file());
            assert!(recovery_move_pending(&fixture.dir));
            if step.get() == fail_at {
                return Err(io::Error::other("injected move failure"));
            }
            step.set(step.get() + 1);
            move_exclusive(from, to)
        }).is_err());
        assert_eq!(step.get(), fail_at);
        assert_eq!(snapshot(&fixture.db), before);
        assert_eq!(snapshot(&plan.backup(&fixture.db)), [None, None, None, None]);
        assert!(!recovery_move_pending(&fixture.dir));
    }
}

#[test]
fn restarting_after_each_move_finishes_before_opening_a_new_database() {
    for completed in 0..=MEMBERS.len() {
        let fixture = Fixture::new();
        fixture.write_broken_group();
        let before = snapshot(&fixture.db);
        let plan = MovePlan::start(&fixture.dir, &fixture.db).unwrap();
        let backup = plan.backup(&fixture.db);
        for suffix in &MEMBERS[..completed] {
            move_exclusive(&append_suffix(&fixture.db, suffix), &append_suffix(&backup, suffix)).unwrap();
        }
        let (storage, mode) = fixture.open();
        assert!(matches!(mode, Some(StorageRecovery::MovedAside { backup: moved }) if moved == backup));
        assert_eq!(snapshot(&backup), before);
        assert!(!recovery_move_pending(&fixture.dir));
        assert!(recording_cleanup_protected(&fixture.dir));
        drop(storage);
    }
}

#[test]
fn invalid_plan_and_unexpected_members_never_reach_sqlite() {
    let fixture = Fixture::new();
    fs::write(append_suffix(&fixture.db, "-wal"), b"orphan").unwrap();
    fs::write(fixture.dir.join(INTENT), b"partial intent").unwrap();
    let (_storage, mode) = fixture.open();
    assert!(matches!(mode, Some(StorageRecovery::IncompleteMove)));
    assert!(recording_cleanup_protected(&fixture.dir));
    assert!(resume_recording_cleanup(&fixture.dir).is_err());
    assert_eq!(fs::read(append_suffix(&fixture.db, "-wal")).unwrap(), b"orphan");
    assert!(!fixture.db.exists());

    fs::remove_file(fixture.dir.join(INTENT)).unwrap();
    fs::remove_file(append_suffix(&fixture.db, "-wal")).unwrap();
    fs::write(&fixture.db, b"old data").unwrap();
    let plan = MovePlan::start(&fixture.dir, &fixture.db).unwrap();
    let backup = plan.backup(&fixture.db);
    move_exclusive(&fixture.db, &backup).unwrap();
    fs::write(append_suffix(&fixture.db, "-journal"), b"unexpected companion").unwrap();
    let (_storage, mode) = fixture.open();
    assert!(matches!(mode, Some(StorageRecovery::IncompleteMove)));
    assert!(!fixture.db.exists());
    assert_eq!(fs::read(&backup).unwrap(), b"old data");
    assert_eq!(fs::read(append_suffix(&fixture.db, "-journal")).unwrap(), b"unexpected companion");
    assert!(recovery_move_pending(&fixture.dir));
}

#[test]
fn a_pending_plan_with_both_main_files_never_reinstalls_old_companions() {
    let fixture = Fixture::new();
    fixture.write_broken_group();
    let plan = MovePlan::start(&fixture.dir, &fixture.db).unwrap();
    let backup = plan.backup(&fixture.db);
    for suffix in MEMBERS {
        move_exclusive(&append_suffix(&fixture.db, suffix), &append_suffix(&backup, suffix)).unwrap();
    }
    let saved = snapshot(&backup);
    drop(fixture.journal_db());
    let replacement = snapshot(&fixture.db);
    let (_storage, mode) = fixture.open();
    assert!(matches!(mode, Some(StorageRecovery::IncompleteMove)));
    assert_eq!(snapshot(&fixture.db), replacement);
    assert_eq!(snapshot(&backup), saved);
    assert!(recovery_move_pending(&fixture.dir));
}

#[test]
fn same_second_backup_names_are_distinct_and_moves_never_overwrite() {
    let fixture = Fixture::new();
    fixture.write_broken_group();
    let plan = MovePlan::start(&fixture.dir, &fixture.db).unwrap();
    let other = MovePlan { stamp: plan.stamp.clone(), id: uuid::Uuid::new_v4().to_string(), members: plan.members };
    assert_ne!(plan.backup(&fixture.db), other.backup(&fixture.db));
    let backup = plan.backup(&fixture.db);
    fs::write(&backup, b"older backup").unwrap();
    let original = snapshot(&fixture.db);
    assert!(plan.finish(&fixture.dir, &fixture.db, move_exclusive).is_err());
    assert_eq!(snapshot(&fixture.db), original);
    assert_eq!(fs::read(&backup).unwrap(), b"older backup");
    assert!(recovery_move_pending(&fixture.dir));
}

#[test]
fn failure_to_persist_protection_does_not_create_a_plan_or_move_files() {
    let fixture = Fixture::new();
    fs::write(&fixture.db, b"damaged data").unwrap();
    fs::create_dir(fixture.dir.join(RECORDING_HOLD)).unwrap();
    let (_storage, mode) = fixture.open();
    assert!(matches!(mode, Some(StorageRecovery::IncompleteMove)));
    assert!(recording_cleanup_protected(&fixture.dir));
    assert!(resume_recording_cleanup(&fixture.dir).is_err());
    assert!(!recovery_move_pending(&fixture.dir));
    assert_eq!(fs::read(&fixture.db).unwrap(), b"damaged data");
}

#[test]
fn protection_survives_reopens_and_database_import_transactions_until_confirmed() {
    let fixture = Fixture::new();
    fs::write(&fixture.db, b"damaged data").unwrap();
    let (storage, mode) = fixture.open();
    assert!(matches!(mode, Some(StorageRecovery::MovedAside { .. })));
    drop(storage);
    for _ in 0..2 {
        let (storage, mode) = fixture.open();
        assert!(mode.is_none());
        assert_eq!(crate::recording_retention_days(&storage, false, &fixture.dir), None);
        let settings = json!({"audioRetentionDays": 30});
        storage.apply_config_transaction(settings.as_object().unwrap(), &[], None, None).unwrap();
        storage.apply_restore_transaction(settings.as_object().unwrap(), &[], &[], false).unwrap();
        assert_eq!(crate::recording_retention_days(&storage, false, &fixture.dir), None);
    }
    resume_recording_cleanup(&fixture.dir).unwrap();
    let (storage, mode) = fixture.open();
    assert!(mode.is_none());
    assert_eq!(crate::recording_retention_days(&storage, false, &fixture.dir), Some(30));
}

#[test]
fn live_database_lease_refuses_exclusive_recovery_until_storage_drops() {
    let fixture = Fixture::new();
    let (storage, mode) = fixture.open();
    assert!(mode.is_none());
    let other = open_lock(&fixture.dir, LIVE_LOCK).unwrap();
    assert_eq!(lock(&other, false).unwrap_err().kind(), io::ErrorKind::WouldBlock);
    drop(storage);
    lock(&other, false).unwrap();
}

#[test]
fn exclusive_sqlite_file_lease_blocks_other_sqlite_connections_without_touching_the_group() {
    let fixture = Fixture::new();
    let db = fixture.journal_db();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let before = snapshot(&fixture.db);
    assert!(lease::sqlite_exclusive(&fixture.db).is_err());
    assert_eq!(snapshot(&fixture.db), before);
    db.execute_batch("ROLLBACK").unwrap();

    let lease = lease::sqlite_exclusive(&fixture.db).unwrap();
    db.busy_timeout(Duration::ZERO).unwrap();
    assert!(db.execute_batch("BEGIN IMMEDIATE").is_err());
    assert_eq!(snapshot(&fixture.db), before);
    drop(lease);
    db.execute_batch("BEGIN IMMEDIATE; ROLLBACK").unwrap();
}

const MIGRATION_TABLE: &str = "CREATE TABLE schema_migrations (
    version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at INTEGER NOT NULL
);";

fn table_exists(db: &Connection, name: &str) -> bool {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [name],
        |row| row.get(0),
    ).unwrap()
}

fn has_audio_path(db: &Connection) -> bool {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('history_records') WHERE name = 'audio_file_path')",
        [],
        |row| row.get(0),
    ).unwrap()
}

#[test]
fn either_migration_marker_failure_rolls_back_ddl_and_versions() {
    for version in [1, 2] {
        let fixture = Fixture::new();
        let mut db = Connection::open(&fixture.db).unwrap();
        db.execute_batch(MIGRATION_TABLE).unwrap();
        db.execute_batch(&format!("CREATE TRIGGER fail_marker BEFORE INSERT ON schema_migrations
            WHEN NEW.version = {version} BEGIN SELECT RAISE(ABORT, 'test marker failure'); END;")).unwrap();
        assert!(Storage::initialize_schema(&mut db).is_err());
        assert!(!table_exists(&db, "history_records"));
        assert!(!table_exists(&db, "app_settings"));
        assert!(!Storage::migration_exists(&db, 1).unwrap());
        assert!(!Storage::migration_exists(&db, 2).unwrap());
        db.execute_batch("DROP TRIGGER fail_marker").unwrap();
        Storage::initialize_schema(&mut db).unwrap();
        assert!(has_audio_path(&db));
    }
}

#[test]
fn default_insert_failure_rolls_back_prior_defaults_and_migrations() {
    let fixture = Fixture::new();
    let mut db = Connection::open(&fixture.db).unwrap();
    db.execute_batch(include_str!("../migration_001.sql")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_default BEFORE INSERT ON app_settings
        WHEN NEW.key = 'selectedMic' BEGIN SELECT RAISE(ABORT, 'test default failure'); END;").unwrap();
    assert!(Storage::initialize_schema(&mut db).is_err());
    assert!(!table_exists(&db, "schema_migrations"));
    assert!(!has_audio_path(&db));
    let count: i64 = db.query_row("SELECT count(*) FROM app_settings", [], |row| row.get(0)).unwrap();
    assert_eq!(count, 0);
}

#[test]
fn existing_version_one_tables_and_audio_column_without_markers_are_adopted() {
    let fixture = Fixture::new();
    let db = Connection::open(&fixture.db).unwrap();
    db.execute_batch(include_str!("../migration_001.sql")).unwrap();
    db.execute_batch("ALTER TABLE history_records ADD COLUMN audio_file_path TEXT;
        INSERT INTO history_records (id, list_order, raw_json, audio_file_path)
        VALUES ('example', 0, '{}', 'recording.wav');").unwrap();
    drop(db);
    let (storage, mode) = fixture.open();
    assert!(mode.is_none());
    let db = storage.conn();
    assert!(Storage::migration_exists(&db, 1).unwrap());
    assert!(Storage::migration_exists(&db, 2).unwrap());
    let audio: String = db.query_row("SELECT audio_file_path FROM history_records", [], |row| row.get(0)).unwrap();
    assert_eq!(audio, "recording.wav");
}

#[test]
fn version_one_marker_with_missing_table_is_not_silently_accepted() {
    let fixture = Fixture::new();
    let db = fixture.journal_db();
    db.execute_batch("DROP TABLE manual_corrections").unwrap();
    drop(db);
    fixture.assert_volatile_unchanged(snapshot(&fixture.db));
}

#[test]
fn concurrent_connections_check_versions_after_getting_the_write_lock() {
    let fixture = Fixture::new();
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let run = || {
            let mut db = Connection::open(&fixture.db).unwrap();
            db.busy_timeout(Duration::from_secs(2)).unwrap();
            barrier.wait();
            Storage::initialize_schema(&mut db).unwrap();
        };
        let first = scope.spawn(run);
        let second = scope.spawn(run);
        first.join().unwrap();
        second.join().unwrap();
    });
    let db = Connection::open(&fixture.db).unwrap();
    let markers: i64 = db.query_row("SELECT count(*) FROM schema_migrations", [], |row| row.get(0)).unwrap();
    assert_eq!(markers, 2);
    assert!(has_audio_path(&db));
}
