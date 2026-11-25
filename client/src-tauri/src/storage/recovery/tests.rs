use super::*;
use rusqlite::{ffi, Connection};
use serde_json::json;
use std::{
    cell::Cell,
    io::Read,
    process::{Child, Command, Stdio},
    time::Instant,
};

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

    fn start_move(&self, orphaned: bool) -> io::Result<MovePlan> {
        let members = group_members(&self.db)?;
        MovePlan::start(&self.dir, &self.db, members, orphaned)
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
fn directory_paths_are_never_given_to_sqlite() {
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
}

#[test]
fn interrupted_first_initialization_reopens_the_existing_empty_database() {
    for (journal, wal) in [(true, false), (false, true), (true, true)] {
        let fixture = Fixture::new();
        fs::write(&fixture.db, []).unwrap();
        if journal {
            fs::write(append_suffix(&fixture.db, "-journal"), [0; 512]).unwrap();
        }
        if wal {
            fs::write(append_suffix(&fixture.db, "-wal"), []).unwrap();
        }
        for _ in 0..2 {
            let (storage, mode) = fixture.open();
            assert!(mode.is_none(), "{mode:?}");
            let db_path = fixture.dir.canonicalize().unwrap().join("sayit.db");
            assert_eq!(storage.conn().path().map(Path::new), Some(db_path.as_path()));
            assert!(Storage::migration_exists(&storage.conn(), 2).unwrap());
            assert!(!recording_cleanup_protected(&fixture.dir));
        }
    }
}

#[test]
fn orphaned_companions_are_archived_intact_before_sqlite_can_open() {
    for (main, suffix, contents) in [
        (false, "-wal", &b"orphan WAL"[..]),
        (false, "-journal", &b""[..]),
        (false, "-wal", &b""[..]),
        (true, "-wal", &b"orphan WAL"[..]),
        (true, "-shm", &b"orphan SHM"[..]),
    ] {
        let fixture = Fixture::new();
        if main {
            fs::write(&fixture.db, []).unwrap();
        }
        fs::write(append_suffix(&fixture.db, suffix), contents).unwrap();
        let original = snapshot(&fixture.db);
        let (storage, mode) = fixture.open();
        let Some(StorageRecovery::MovedAside { backup, orphaned: true }) = mode else {
            panic!("{mode:?}");
        };
        assert_eq!(snapshot(&backup), original);
        assert!(Storage::migration_exists(&storage.conn(), 2).unwrap());
        assert!(recording_cleanup_protected(&fixture.dir));
        assert!(!recovery_move_pending(&fixture.dir));
        drop(storage);
        let (_storage, mode) = fixture.open();
        assert!(mode.is_none(), "{mode:?}");
        assert_eq!(snapshot(&backup), original);
    }
}

#[test]
fn orphaned_group_resumes_after_every_companion_move_without_a_main_file() {
    for completed in 0..=3 {
        let fixture = Fixture::new();
        for suffix in COMPANION_SUFFIXES {
            fs::write(append_suffix(&fixture.db, suffix), suffix).unwrap();
        }
        let original = snapshot(&fixture.db);
        let plan = fixture.start_move(true).unwrap();
        let backup = plan.backup(&fixture.db);
        for suffix in &COMPANION_SUFFIXES[..completed] {
            move_exclusive(&append_suffix(&fixture.db, suffix), &append_suffix(&backup, suffix)).unwrap();
        }
        let (storage, mode) = fixture.open();
        assert!(matches!(mode, Some(StorageRecovery::MovedAside { backup: moved, orphaned: true }) if moved == backup));
        assert_eq!(snapshot(&backup), original);
        assert!(Storage::migration_exists(&storage.conn(), 2).unwrap());
        assert!(recording_cleanup_protected(&fixture.dir));
        assert!(!recovery_move_pending(&fixture.dir));
    }
}

#[test]
fn a_main_that_appears_during_orphan_recovery_is_excluded_from_the_move() {
    let fixture = Fixture::new();
    let wal = append_suffix(&fixture.db, "-wal");
    fs::write(&wal, b"orphan WAL").unwrap();
    let observed = group_members(&fixture.db).unwrap();
    fs::write(&fixture.db, b"new main file").unwrap();

    assert!(MovePlan::start(&fixture.dir, &fixture.db, observed, true).is_err());
    assert!(!recording_cleanup_protected(&fixture.dir));
    assert_eq!(fs::read(&fixture.db).unwrap(), b"new main file");
    assert_eq!(fs::read(wal).unwrap(), b"orphan WAL");
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
    let Some(StorageRecovery::MovedAside { backup, .. }) = mode else { panic!("{mode:?}") };
    assert_eq!(fs::read(backup).unwrap(), damaged);
}

#[test]
fn not_a_database_is_confirmed_and_existing_wal_bytes_are_preserved() {
    let fixture = Fixture::new();
    fixture.write_broken_group();
    let before = snapshot(&fixture.db);
    let (_storage, mode) = fixture.open();
    let Some(StorageRecovery::MovedAside { backup, .. }) = mode else { panic!("{mode:?}") };
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
        let plan = fixture.start_move(false).unwrap();
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
        let plan = fixture.start_move(false).unwrap();
        let backup = plan.backup(&fixture.db);
        for suffix in &MEMBERS[..completed] {
            move_exclusive(&append_suffix(&fixture.db, suffix), &append_suffix(&backup, suffix)).unwrap();
        }
        let (storage, mode) = fixture.open();
        assert!(matches!(mode, Some(StorageRecovery::MovedAside { backup: moved, .. }) if moved == backup));
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
    let plan = fixture.start_move(false).unwrap();
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
fn plan_rejects_invalid_dates_and_noncanonical_ids_without_touching_the_database() {
    for (field, invalid) in [
        ("stamp", "20260230-153213"),
        ("stamp", "../2026-153213"),
        ("id", "../not-a-uuid"),
        ("id", "AC3E64B2-BC86-4A43-900A-A67D5A9EE4A3"),
    ] {
        let fixture = Fixture::new();
        fixture.write_broken_group();
        let original = snapshot(&fixture.db);
        let plan = fixture.start_move(false).unwrap();
        let mut malformed = serde_json::to_value(&plan).unwrap();
        malformed[field] = json!(invalid);
        fs::write(fixture.dir.join(INTENT), serde_json::to_vec(&malformed).unwrap()).unwrap();
        let (_storage, mode) = fixture.open();
        assert!(matches!(mode, Some(StorageRecovery::IncompleteMove)), "{field} {invalid}");
        assert_eq!(snapshot(&fixture.db), original);
        assert_eq!(snapshot(&plan.backup(&fixture.db)), [None, None, None, None]);
    }
}

#[test]
fn a_pending_plan_with_both_main_files_never_reinstalls_old_companions() {
    let fixture = Fixture::new();
    fixture.write_broken_group();
    let plan = fixture.start_move(false).unwrap();
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
fn isolating_the_files_named_in_the_dialog_allows_a_safe_restart() {
    for scenario in ["malformed plan", "missing member", "both mains", "blocked hold", "busy live lock"] {
        let fixture = Fixture::new();
        let backup_dir = Fixture::new();
        let isolation_dir = Fixture::new();
        let audio = fixture.dir.join("example.wav");
        fs::write(&audio, b"existing recording").unwrap();
        fixture.write_broken_group();
        let mut blocker = None;
        let mut recovery_backup = None;
        match scenario {
            "malformed plan" => fs::write(fixture.dir.join(INTENT), b"invalid plan").unwrap(),
            "missing member" | "both mains" | "busy live lock" => {
                let plan = fixture.start_move(false).unwrap();
                let backup = plan.backup(&fixture.db);
                move_exclusive(&fixture.db, &backup).unwrap();
                if scenario == "missing member" {
                    fs::remove_file(append_suffix(&fixture.db, "-shm")).unwrap();
                } else if scenario == "both mains" {
                    fs::write(&fixture.db, b"replacement from older app").unwrap();
                } else {
                    let lease = open_lock(&fixture.dir, LIVE_LOCK).unwrap();
                    lock(&lease, true).unwrap();
                    blocker = Some(lease);
                }
                recovery_backup = Some(backup);
            }
            "blocked hold" => fs::create_dir(fixture.dir.join(RECORDING_HOLD)).unwrap(),
            _ => unreachable!(),
        }
        let (storage, mode) = fixture.open();
        assert!(matches!(mode, Some(StorageRecovery::IncompleteMove)), "{scenario}: {mode:?}");
        drop(storage);
        drop(blocker);

        // Simulate copying the complete fixture, then moving the exact
        // active names printed by the dialog. Leave previous copies in place.
        for entry in fs::read_dir(&fixture.dir).unwrap() {
            let entry = entry.unwrap();
            let destination = backup_dir.dir.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                fs::create_dir(destination).unwrap();
            } else {
                fs::copy(entry.path(), destination).unwrap();
            }
        }
        for name in MANUAL_RECOVERY_FILES {
            let source = fixture.dir.join(name);
            if source.exists() {
                let isolated = isolation_dir.dir.join(name);
                fs::rename(&source, &isolated).unwrap();
                assert_eq!(fs::read(isolated).unwrap(), fs::read(backup_dir.dir.join(name)).unwrap());
            }
            assert!(!source.exists());
        }
        let hold = fixture.dir.join(RECORDING_HOLD_FILE);
        if !hold.exists() {
            fs::write(hold, []).unwrap();
        }
        let (storage, mode) = fixture.open();
        assert!(mode.is_none(), "{scenario}: {mode:?}");
        assert!(Storage::migration_exists(&storage.conn(), 2).unwrap());
        assert!(recording_cleanup_protected(&fixture.dir));
        assert_eq!(fs::read(audio).unwrap(), b"existing recording");
        if let Some(backup) = recovery_backup {
            assert_eq!(fs::read(&backup).unwrap(), fs::read(backup_dir.dir.join(backup.file_name().unwrap())).unwrap());
        }
    }
}

#[cfg(unix)]
#[test]
fn isolating_a_linked_plan_never_moves_its_target() {
    let fixture = Fixture::new();
    let outside = Fixture::new();
    let isolated = Fixture::new();
    fs::write(&outside.db, b"keep this file").unwrap();
    std::os::unix::fs::symlink(&outside.db, fixture.dir.join(INTENT)).unwrap();
    let (storage, mode) = fixture.open();
    assert!(matches!(mode, Some(StorageRecovery::IncompleteMove)));
    drop(storage);

    let saved = isolated.dir.join(INTENT);
    fs::rename(fixture.dir.join(INTENT), &saved).unwrap();
    fs::write(fixture.dir.join(RECORDING_HOLD_FILE), []).unwrap();
    let (_storage, mode) = fixture.open();
    assert!(mode.is_none(), "{mode:?}");
    assert_eq!(fs::read_link(saved).unwrap(), outside.db);
    assert_eq!(fs::read(&outside.db).unwrap(), b"keep this file");
    assert!(recording_cleanup_protected(&fixture.dir));
}

#[test]
fn same_second_backup_names_are_distinct_and_moves_never_overwrite() {
    let fixture = Fixture::new();
    fixture.write_broken_group();
    let plan = fixture.start_move(false).unwrap();
    let other = MovePlan { stamp: plan.stamp.clone(), id: uuid::Uuid::new_v4().to_string(), members: plan.members, orphaned: false };
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
fn startup_lock_prevents_the_recovery_flow_from_opening_sqlite() {
    let fixture = Fixture::new();
    fixture.write_broken_group();
    let original = snapshot(&fixture.db);
    let startup = startup_lock(&fixture.dir).unwrap();
    let (_storage, mode) = open_or_recover_with(
        &fixture.db, Duration::ZERO,
        |_| panic!("opened SQLite without the startup lock"),
        || panic!("attempted recovery without the startup lock"),
        move_exclusive,
    );
    assert!(matches!(mode, Some(StorageRecovery::InMemory)));
    assert_eq!(snapshot(&fixture.db), original);
    assert!(!recording_cleanup_protected(&fixture.dir));
    drop(startup);
}

#[test]
fn confirmed_corruption_cannot_move_files_while_another_app_holds_a_live_lease() {
    let fixture = Fixture::new();
    fixture.write_broken_group();
    let original = snapshot(&fixture.db);
    let live = open_lock(&fixture.dir, LIVE_LOCK).unwrap();
    lock(&live, true).unwrap();
    let confirmed = Cell::new(false);
    let (_storage, mode) = open_or_recover_with(
        &fixture.db, Duration::ZERO,
        |_| Err(sqlite_error(ffi::SQLITE_NOTADB)),
        || confirmed.set(true),
        move_exclusive,
    );
    assert!(confirmed.get());
    assert!(matches!(mode, Some(StorageRecovery::InMemory)));
    assert_eq!(snapshot(&fixture.db), original);
    assert!(!recording_cleanup_protected(&fixture.dir));
    drop(live);
}

#[test]
fn confirmed_corruption_cannot_move_a_group_changed_before_exclusive_locks() {
    let fixture = Fixture::new();
    fixture.write_broken_group();
    let companion = append_suffix(&fixture.db, "-wal");
    let changed = b"WAL grew since the confirmed corruption";
    let (_storage, mode) = open_or_recover_with(
        &fixture.db, Duration::ZERO,
        |_| Err(sqlite_error(ffi::SQLITE_CORRUPT)),
        || fs::write(&companion, changed).unwrap(),
        move_exclusive,
    );
    assert!(matches!(mode, Some(StorageRecovery::InMemory)));
    assert_eq!(fs::read(&companion).unwrap(), changed);
    assert_eq!(fs::read(&fixture.db).unwrap(), b"old data ");
    assert!(!recording_cleanup_protected(&fixture.dir));
}

#[test]
fn leftover_files_in_the_active_group_block_replacement_database_creation() {
    let fixture = Fixture::new();
    fixture.write_broken_group();
    let journal = append_suffix(&fixture.db, "-journal");
    let (_storage, mode) = open_or_recover_with(
        &fixture.db, Duration::ZERO,
        |_| Err(sqlite_error(ffi::SQLITE_CORRUPT)),
        || {},
        |source, target| {
            move_exclusive(source, target)?;
            if source == fixture.db {
                // These look like an interrupted first initialization and
                // would be accepted by the ordinary disk opener.
                fs::write(&fixture.db, [])?;
                fs::write(&journal, [0; 512])?;
            }
            Ok(())
        },
    );
    assert!(matches!(mode, Some(StorageRecovery::IncompleteMove)));
    assert!(recovery_move_pending(&fixture.dir));
    assert_eq!(fs::metadata(&fixture.db).unwrap().len(), 0);
    assert_eq!(fs::read(&journal).unwrap(), [0; 512]);
    let plan = MovePlan::load(&fixture.dir).unwrap().unwrap();
    assert_eq!(fs::read(plan.backup(&fixture.db)).unwrap(), b"old data ");
}

#[test]
fn files_reappearing_after_plan_completion_also_block_replacement_creation() {
    let fixture = Fixture::new();
    fixture.write_broken_group();
    let _startup = startup_lock(&fixture.dir).unwrap();
    let live = open_lock(&fixture.dir, LIVE_LOCK).unwrap();
    lock(&live, false).unwrap();
    let plan = fixture.start_move(false).unwrap();
    let backup = plan.finish(&fixture.dir, &fixture.db, move_exclusive).unwrap();
    assert!(!recovery_move_pending(&fixture.dir));
    let journal = append_suffix(&fixture.db, "-journal");
    fs::write(&fixture.db, []).unwrap();
    fs::write(&journal, [0; 512]).unwrap();
    assert!(open_replacement(&fixture.db, Duration::ZERO, &live).is_err());
    assert_eq!(fs::metadata(&fixture.db).unwrap().len(), 0);
    assert_eq!(fs::read(&journal).unwrap(), [0; 512]);
    assert_eq!(fs::read(backup).unwrap(), b"old data ");
}

struct SqliteChild(Child);

impl SqliteChild {
    fn start(fixture: &Fixture, test_name: &str) -> Self {
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture"])
            .env("SAYIT_RECOVERY_TEST_CHILD_DB", &fixture.db)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn().unwrap();
        let mut child = Self(child);
        let start = Instant::now();
        while !fixture.dir.join("child-ready").exists() {
            assert!(child.0.try_wait().unwrap().is_none(), "SQLite test child exited before opening");
            assert!(start.elapsed() < Duration::from_secs(5), "SQLite test child did not become ready");
            std::thread::sleep(Duration::from_millis(5));
        }
        child
    }
}

impl Drop for SqliteChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn park_with_open_connection(db: &Path) {
    fs::write(db.with_file_name("child-ready"), []).unwrap();
    let _ = std::io::stdin().read(&mut [0]);
}

#[test]
fn confirmed_corruption_cannot_move_a_database_locked_by_another_sqlite_process() {
    if let Some(path) = std::env::var_os("SAYIT_RECOVERY_TEST_CHILD_DB") {
        let path = PathBuf::from(path);
        let db = Connection::open(&path).unwrap();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        park_with_open_connection(&path);
        return;
    }
    let fixture = Fixture::new();
    drop(fixture.journal_db());
    let child = SqliteChild::start(&fixture, concat!(
        module_path!(), "::confirmed_corruption_cannot_move_a_database_locked_by_another_sqlite_process"
    ).strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::")).unwrap());
    let original = snapshot(&fixture.db);
    let confirmed = Cell::new(false);
    let (_storage, mode) = open_or_recover_with(
        &fixture.db, Duration::ZERO,
        |_| Err(sqlite_error(ffi::SQLITE_CORRUPT)),
        || confirmed.set(true),
        move_exclusive,
    );
    assert!(confirmed.get());
    assert!(matches!(mode, Some(StorageRecovery::InMemory)));
    assert_eq!(snapshot(&fixture.db), original);
    assert!(!recording_cleanup_protected(&fixture.dir));
    drop(child);
}

#[test]
fn committed_wal_data_survives_a_killed_writer() {
    if let Some(path) = std::env::var_os("SAYIT_RECOVERY_TEST_CHILD_DB") {
        let path = PathBuf::from(path);
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
            UPDATE app_settings SET value_json = 'true' WHERE key = 'autoLaunch';").unwrap();
        park_with_open_connection(&path);
        return;
    }
    let fixture = Fixture::new();
    drop(fixture.journal_db());
    let child = SqliteChild::start(&fixture, concat!(
        module_path!(), "::committed_wal_data_survives_a_killed_writer"
    ).strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::")).unwrap());
    let wal = append_suffix(&fixture.db, "-wal");
    let committed = fs::read(&wal).unwrap();
    assert!(!committed.is_empty());
    drop(child);
    assert_eq!(fs::read(wal).unwrap(), committed);
    for _ in 0..2 {
        let (storage, mode) = fixture.open();
        assert!(mode.is_none(), "{mode:?}");
        assert_eq!(storage.get("autoLaunch", None), json!(true));
    }
}

#[test]
fn killing_a_new_database_transaction_allows_normal_restart() {
    if let Some(path) = std::env::var_os("SAYIT_RECOVERY_TEST_CHILD_DB") {
        let path = PathBuf::from(path);
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
            BEGIN IMMEDIATE; CREATE TABLE interrupted_initialization (id INTEGER);").unwrap();
        park_with_open_connection(&path);
        return;
    }
    for replacement in [false, true] {
        let fixture = Fixture::new();
        let saved = if replacement {
            fixture.write_broken_group();
            let original = snapshot(&fixture.db);
            let plan = fixture.start_move(false).unwrap();
            let backup = plan.finish(&fixture.dir, &fixture.db, move_exclusive).unwrap();
            Some((backup, original))
        } else {
            None
        };
        let child = SqliteChild::start(&fixture, concat!(
            module_path!(), "::killing_a_new_database_transaction_allows_normal_restart"
        ).strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::")).unwrap());
        assert_eq!(fs::metadata(&fixture.db).unwrap().len(), 0);
        let journal = append_suffix(&fixture.db, "-journal");
        assert!(journal.is_file());
        drop(child);
        assert!(journal.is_file());
        let (storage, mode) = fixture.open();
        assert!(mode.is_none(), "{mode:?}");
        assert!(Storage::migration_exists(&storage.conn(), 2).unwrap());
        assert!(!table_exists(&storage.conn(), "interrupted_initialization"));
        if let Some((backup, original)) = saved {
            assert_eq!(snapshot(&backup), original);
        }
        assert_eq!(recording_cleanup_protected(&fixture.dir), replacement);
    }
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
    for _ in 0..40 {
        let fixture = Fixture::new();
        Connection::open(&fixture.db).unwrap().execute_batch(MIGRATION_TABLE).unwrap();
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
}

#[test]
fn normal_open_enforces_foreign_keys() {
    let fixture = Fixture::new();
    let (storage, mode) = fixture.open();
    assert!(mode.is_none(), "{mode:?}");
    let db = storage.conn();
    db.execute_batch("CREATE TEMP TABLE test_parent(id INTEGER PRIMARY KEY);
        CREATE TEMP TABLE test_child(parent INTEGER REFERENCES test_parent(id));").unwrap();
    let error = db.execute_batch("INSERT INTO test_child VALUES (1)").unwrap_err();
    assert!(matches!(error, rusqlite::Error::SqliteFailure(code, _)
        if code.extended_code == ffi::SQLITE_CONSTRAINT_FOREIGNKEY));
}
