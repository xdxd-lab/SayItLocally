use super::Storage;
use rusqlite::ErrorCode;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

mod lease;
use lease::{lock, unlock};

pub(crate) const COMPANION_SUFFIXES: &[&str] = &["-wal", "-shm", "-journal"];
// Move companion files before the main file. A pending plan blocks *all* disk opens.
const MEMBERS: [&str; 4] = ["-wal", "-shm", "-journal", ""];
const INTENT: &str = "sayit.db.recovery-pending";
const RECORDING_HOLD: &str = "sayit.db.preserve-recordings";
const STARTUP_LOCK: &str = "sayit.db.startup-lock";
const LIVE_LOCK: &str = "sayit.db.live-lock";
pub(crate) const MANUAL_RECOVERY_FILES: &[&str] =
    &["sayit.db", "sayit.db-wal", "sayit.db-shm", "sayit.db-journal", INTENT];
pub(crate) const RECORDING_HOLD_FILE: &str = RECORDING_HOLD;

#[derive(Debug)]
pub(crate) enum StorageRecovery {
    MovedAside { backup: PathBuf, orphaned: bool },
    MovedAsideInMemory { backup: PathBuf },
    InMemory,
    IncompleteMove,
}

#[derive(Debug)]
enum OpenError {
    Sql(rusqlite::Error),
    Io(io::Error),
}

impl From<rusqlite::Error> for OpenError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}

impl From<io::Error> for OpenError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl OpenError {
    fn is_corruption(&self) -> bool {
        matches!(&self, Self::Sql(rusqlite::Error::SqliteFailure(error, _))
            if matches!(error.code, ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase))
    }

    fn log(&self, stage: &str) {
        let reason = match self {
            Self::Sql(rusqlite::Error::SqliteFailure(error, _)) => {
                format!("SQLite {:?} ({})", error.code, error.extended_code)
            }
            Self::Sql(_) => "SQLite query/type error".into(),
            Self::Io(error) => format!("I/O {:?} ({:?})", error.kind(), error.raw_os_error()),
        };
        let line = format!("[storage] {stage}: {reason}");
        log::warn!("{line}");
        crate::commands::system::write_log_line(&line);
    }
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}

// Recovery only moves regular files. Ordinary SQLite opens may follow the main link.
fn regular_file(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(io::Error::new(io::ErrorKind::InvalidData, "Not a regular file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn file_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options
}

fn open_lock(dir: &Path, name: &str) -> io::Result<File> {
    let path = dir.join(name);
    let exists = regular_file(&path)?;
    let file = match file_options().write(true).create(true).open(&path) {
        Err(error) if exists && is_read_only_error(&error) => file_options().open(&path)?,
        result => result?,
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid database lock file"));
    }
    Ok(file)
}

pub(crate) fn startup_lock(dir: &Path) -> io::Result<File> {
    let file = open_lock(dir, STARTUP_LOCK)?;
    for _ in 0..4 {
        match lock(&file, false) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(25));
            }
            result => {
                result?;
                return Ok(file);
            }
        }
    }
    Err(io::ErrorKind::WouldBlock.into())
}

fn sync_directory(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    return File::open(dir)?.sync_all();
    // Process interruption leaves completed namespace operations intact on
    // Windows. Group moves use MoveFileExW with WRITE_THROUGH.
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

fn write_state(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut file = file_options().write(true).create_new(true).open(path)?;
    file.write_all(data)?;
    file.sync_all()?;
    sync_directory(path.parent().unwrap())
}

fn persist_recording_hold(dir: &Path) -> io::Result<()> {
    let path = dir.join(RECORDING_HOLD);
    if regular_file(&path)? {
        file_options().write(true).open(path)?.sync_all()?;
        sync_directory(dir)
    } else {
        write_state(&path, b"1\n")
    }
}

pub(crate) fn recording_cleanup_protected(dir: &Path) -> bool {
    // An unrecognized file type or filesystem error is also a hold.
    !matches!(regular_file(&dir.join(RECORDING_HOLD)), Ok(false))
        || recovery_move_pending(dir)
}

pub(crate) fn recovery_move_pending(dir: &Path) -> bool {
    !matches!(regular_file(&dir.join(INTENT)), Ok(false))
}

pub(crate) fn resume_recording_cleanup(dir: &Path) -> io::Result<()> {
    let _startup = startup_lock(dir)?;
    if recovery_move_pending(dir) {
        return Err(io::Error::other("Database recovery is still pending"));
    }
    let hold = dir.join(RECORDING_HOLD);
    if regular_file(&hold)? {
        fs::remove_file(hold)?;
        if let Err(error) = sync_directory(dir) {
            let _ = persist_recording_hold(dir);
            return Err(error);
        }
    }
    Ok(())
}

fn group_members(db_path: &Path) -> io::Result<[bool; 4]> {
    let mut members = [false; 4];
    for (present, suffix) in members.iter_mut().zip(MEMBERS) {
        *present = regular_file(&append_suffix(db_path, suffix))?;
    }
    Ok(members)
}

#[derive(Debug, Serialize, Deserialize)]
struct MovePlan {
    stamp: String,
    id: String,
    members: [bool; 4],
    #[serde(default)]
    orphaned: bool,
}

impl MovePlan {
    fn backup(&self, db: &Path) -> PathBuf {
        append_suffix(db, &format!(".corrupt-{}-{}", self.stamp, self.id))
    }

    fn start(dir: &Path, db: &Path, members: [bool; 4], orphaned: bool) -> io::Result<Self> {
        if !members[3] && (!orphaned || !members.iter().any(|&present| present)) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Database main file missing"));
        }
        if group_members(db)? != members {
            return Err(io::Error::new(io::ErrorKind::WouldBlock, "Database group changed before planning"));
        }
        let plan = Self {
            stamp: chrono::Local::now().format("%Y%m%d-%H%M%S").to_string(),
            id: uuid::Uuid::new_v4().to_string(),
            members,
            orphaned,
        };
        if group_members(&plan.backup(db))? != [false; 4] {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        persist_recording_hold(dir)?;
        let temporary = dir.join(format!("{INTENT}-{}.tmp", plan.id));
        write_state(&temporary, &serde_json::to_vec(&plan)?)?;
        let saved = move_exclusive(&temporary, &dir.join(INTENT))
            .and_then(|()| sync_directory(dir));
        let _ = fs::remove_file(temporary);
        saved?;
        Ok(plan)
    }

    fn load(dir: &Path) -> io::Result<Option<Self>> {
        let path = dir.join(INTENT);
        if !regular_file(&path)? {
            return Ok(None);
        }
        let file = file_options().open(path)?;
        // The plan contains only a date, UUID, and four booleans.
        if !file.metadata()?.is_file() || file.metadata()?.len() > 1024 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid database recovery plan"));
        }
        let plan: Self = serde_json::from_reader(file)?;
        if (!plan.members[3] && (!plan.orphaned || !plan.members.iter().any(|&present| present)))
            || plan.stamp.len() != 15
            || !plan.stamp.as_bytes().iter().enumerate().all(|(i, byte)| {
                if i == 8 { *byte == b'-' } else { byte.is_ascii_digit() }
            })
            || chrono::NaiveDateTime::parse_from_str(&plan.stamp, "%Y%m%d-%H%M%S").is_err()
            || !uuid::Uuid::parse_str(&plan.id).is_ok_and(|id| id.to_string() == plan.id)
        {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid database recovery plan"));
        }
        Ok(Some(plan))
    }

    fn finish(
        &self,
        dir: &Path,
        db: &Path,
        move_file: impl Fn(&Path, &Path) -> io::Result<()>,
    ) -> io::Result<PathBuf> {
        let backup = self.backup(db);
        let originals = group_members(db)?;
        let copies = group_members(&backup)?;
        // Verify the whole plan before changing either group, especially when
        // a replacement main file and an archived main file are both present.
        for ((expected, original), saved) in self.members.into_iter().zip(originals).zip(copies) {
            if (expected && original == saved) || (!expected && (original || saved)) {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "Ambiguous database group"));
            }
        }
        let result = (|| {
            // The prior process may have stopped after any individual move.
            // Never accept two files, a missing expected file, or new sidecars.
            for (suffix, expected) in MEMBERS.into_iter().zip(self.members) {
                let original = append_suffix(db, suffix);
                let saved = append_suffix(&backup, suffix);
                match (expected, regular_file(&original)?, regular_file(&saved)?) {
                    (true, true, false) => {
                        move_file(&original, &saved)?;
                        sync_directory(dir)?;
                    }
                    (true, false, true) | (false, false, false) => {}
                    _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "Ambiguous database group")),
                }
            }
            if group_members(db)? != [false; 4] || group_members(&backup)? != self.members {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "Database group changed during recovery"));
            }
            sync_directory(dir)
        })();
        if let Err(error) = result {
            self.rollback(dir, db, &backup);
            return Err(error);
        }
        // A new main file must not become durable before intent removal is durable.
        // Once unlinked, the saved group is complete and must never be rolled back.
        fs::remove_file(dir.join(INTENT))?;
        sync_directory(dir)?;
        Ok(backup)
    }

    fn rollback(&self, dir: &Path, db: &Path, backup: &Path) {
        let main = (regular_file(db), regular_file(backup));
        let unambiguous = match main {
            (Ok(true), Ok(false)) | (Ok(false), Ok(true)) => self.members[3],
            (Ok(false), Ok(false)) => !self.members[3],
            _ => false,
        };
        if !unambiguous {
            return;
        }
        for (suffix, expected) in MEMBERS.into_iter().zip(self.members).rev() {
            let original = append_suffix(db, suffix);
            let saved = append_suffix(backup, suffix);
            if expected && matches!((regular_file(&original), regular_file(&saved)), (Ok(false), Ok(true))) {
                let _ = move_exclusive(&saved, &original);
            }
        }
        // A failed/ambiguous rollback leaves the plan in place: SQLite must not
        // see a partial group. Successful rollback can be tried again next time.
        if sync_directory(dir).is_ok()
            && group_members(db).is_ok_and(|members| members == self.members)
            && group_members(backup).is_ok_and(|members| members == [false; 4])
        {
            if fs::remove_file(dir.join(INTENT)).is_ok() {
                let _ = sync_directory(dir);
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn move_exclusive(source: &Path, target: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let source = CString::new(source.as_os_str().as_bytes())?;
    let target = CString::new(target.as_os_str().as_bytes())?;
    if unsafe { libc::renamex_np(source.as_ptr(), target.as_ptr(), libc::RENAME_EXCL) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn move_exclusive(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(source: *const u16, target: *const u16, flags: u32) -> i32;
    }
    let parent = source.parent().unwrap().canonicalize()?;
    let encode = |path: &Path| -> Vec<u16> {
        parent.join(path.file_name().unwrap()).as_os_str().encode_wide().chain([0]).collect()
    };
    // WRITE_THROUGH, without REPLACE_EXISTING.
    if unsafe { MoveFileExW(encode(source).as_ptr(), encode(target).as_ptr(), 0x8) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
fn move_exclusive(source: &Path, target: &Path) -> io::Result<()> {
    fs::hard_link(source, target)?;
    fs::remove_file(source)
}

#[derive(Debug, Eq, PartialEq)]
struct MemberStamp {
    size: u64,
    modified: SystemTime,
    #[cfg(unix)]
    identity: (u64, u64),
}

type GroupStamp = [Option<MemberStamp>; 4];

fn group_stamp(db: &Path) -> io::Result<GroupStamp> {
    let mut stamp = [None, None, None, None];
    for (suffix, member) in MEMBERS.into_iter().zip(&mut stamp) {
        match fs::symlink_metadata(append_suffix(db, suffix)) {
            Ok(metadata) if metadata.is_file() => {
                #[cfg(unix)]
                use std::os::unix::fs::MetadataExt;
                *member = Some(MemberStamp {
                    size: metadata.len(),
                    modified: metadata.modified()?,
                    #[cfg(unix)]
                    identity: (metadata.dev(), metadata.ino()),
                });
            }
            Ok(_) => return Err(io::ErrorKind::InvalidData.into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(stamp)
}

fn main_file_size(db: &Path) -> io::Result<Option<u64>> {
    match fs::metadata(db) {
        Ok(metadata) if metadata.is_file() => Ok(Some(metadata.len())),
        Ok(_) => Err(io::ErrorKind::InvalidData.into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match fs::symlink_metadata(db) {
                Ok(_) => Err(error),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

fn has_orphaned_companions([wal, shm, journal, main]: [Option<u64>; 4]) -> bool {
    match main {
        None => wal.is_some() || shm.is_some() || journal.is_some(),
        // Interrupted initial creation may leave a rollback journal or an
        // empty WAL. SQLite can retry that transaction safely.
        Some(0) => shm.is_some() || wal.is_some_and(|size| size != 0),
        Some(_) => false,
    }
}

fn inspect_database(db: &Path) -> io::Result<(Option<u64>, bool)> {
    let main = main_file_size(db)?;
    let mut sizes = [None, None, None, main];
    for (slot, suffix) in sizes.iter_mut().zip(COMPANION_SUFFIXES) {
        match fs::symlink_metadata(append_suffix(db, suffix)) {
            Ok(metadata) if metadata.is_file() => *slot = Some(metadata.len()),
            Ok(_) => return Err(io::ErrorKind::InvalidData.into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok((main, has_orphaned_companions(sizes)))
}

fn open_existing_or_empty(db: &Path, timeout: Duration) -> Result<Storage, OpenError> {
    let (main, orphaned) = inspect_database(db)?;
    if orphaned {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Database has orphaned companion files").into());
    }
    Ok(Storage::open_sqlite(db, main.is_none(), timeout)?)
}

fn is_read_only_error(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem)
}

fn open_without_recovery_on_read_only_dir(
    dir: &Path,
    db: &Path,
    timeout: Duration,
    error: io::Error,
) -> (Storage, Option<StorageRecovery>) {
    if is_read_only_error(&error) && !recovery_move_pending(dir)
        && main_file_size(db).is_ok_and(|size| size.is_some_and(|size| size != 0))
    {
        return match open_existing_or_empty(db, timeout) {
            Ok(storage) => (storage, None),
            Err(error) => failed("open existing database without recovery lease", error, StorageRecovery::InMemory),
        };
    }
    failed("acquire database startup or live lock", error, StorageRecovery::InMemory)
}

fn retry_open(
    timeout: Duration,
    mut open: impl FnMut(Duration) -> Result<Storage, OpenError>,
) -> Result<Storage, (OpenError, bool)> {
    let started = Instant::now();
    let first = match open(timeout) {
        Ok(storage) => return Ok(storage),
        Err(error) => error,
    };
    if !first.is_corruption() {
        return Err((first, false));
    }
    first.log("initial database open");
    std::thread::sleep(Duration::from_millis(75));
    match open(timeout.saturating_sub(started.elapsed())) {
        Ok(storage) => Ok(storage),
        Err(second) => {
            let confirmed = second.is_corruption();
            Err((second, confirmed))
        }
    }
}

fn memory(reason: StorageRecovery) -> (Storage, Option<StorageRecovery>) {
    let storage = Storage::open_sqlite(Path::new(":memory:"), true, Duration::ZERO)
        .expect("an in-memory SQLite database must always open");
    (storage, Some(reason))
}

fn failed(stage: &str, error: impl Into<OpenError>, reason: StorageRecovery) -> (Storage, Option<StorageRecovery>) {
    error.into().log(stage);
    memory(reason)
}

pub(super) fn open_or_recover(db: &Path, timeout: Duration) -> (Storage, Option<StorageRecovery>) {
    open_or_recover_with(
        db,
        timeout,
        |remaining| open_existing_or_empty(db, remaining),
        || {},
        move_exclusive,
    )
}

fn open_or_recover_with(
    db: &Path,
    timeout: Duration,
    open_disk: impl FnMut(Duration) -> Result<Storage, OpenError>,
    before_move_locks: impl FnOnce(),
    move_file: impl Fn(&Path, &Path) -> io::Result<()>,
) -> (Storage, Option<StorageRecovery>) {
    let dir = db.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or(Path::new("."));
    if let Err(error) = fs::create_dir_all(dir) {
        return failed("create database directory", error, StorageRecovery::InMemory);
    }
    let _startup = match startup_lock(dir) {
        Ok(file) => file,
        Err(error) => return open_without_recovery_on_read_only_dir(dir, db, timeout, error),
    };
    let plan = match MovePlan::load(dir) {
        Ok(plan) => plan,
        Err(error) => return failed("read database recovery plan", error, StorageRecovery::IncompleteMove),
    };
    let live = match open_lock(dir, LIVE_LOCK) {
        Ok(file) => file,
        Err(error) => return open_without_recovery_on_read_only_dir(dir, db, timeout, error),
    };

    let (plan, sqlite_guard) = if let Some(plan) = plan {
        if let Err(error) = lock(&live, false).and_then(|()| persist_recording_hold(dir)) {
            return failed("resume database recovery", error, StorageRecovery::IncompleteMove);
        }
        let backup = plan.backup(db);
        let guard = match (regular_file(db), regular_file(&backup), plan.members[3]) {
            (Ok(true), Ok(false), true) => lease::sqlite_exclusive(db).map(Some),
            (Ok(false), Ok(true), true) => lease::sqlite_exclusive(&backup).map(Some),
            (Ok(false), Ok(false), false) => Ok(None),
            _ => Err(io::Error::new(io::ErrorKind::InvalidData, "Ambiguous database main file").into()),
        };
        match guard {
            Ok(guard) => (plan, guard),
            Err(error) => return failed("lock database for resumed recovery", error, StorageRecovery::IncompleteMove),
        }
    } else {
        if let Err(error) = lock(&live, true) {
            return failed("acquire database lease", error, StorageRecovery::InMemory);
        }
        let orphaned = match inspect_database(db) {
            Ok((_, orphaned)) => orphaned,
            Err(error) => return failed("inspect database", error, StorageRecovery::InMemory),
        };
        if !orphaned {
            match retry_open(timeout, open_disk) {
                Ok(mut storage) => {
                    storage.recovery_lease = Some(live);
                    return (storage, None);
                }
                Err((error, false)) => return failed("database unavailable", error, StorageRecovery::InMemory),
                Err((error, true)) => error.log("confirmed database corruption"),
            }
        }
        // SQLite may have done standard journal recovery. Compare the
        // resulting group before and after acquiring the move locks.
        let stamp = match group_stamp(db) {
            Ok(stamp) if if orphaned {
                has_orphaned_companions(stamp.each_ref().map(|member| member.as_ref().map(|m| m.size)))
            } else {
                stamp[3].is_some()
            } => stamp,
            Ok(_) => return failed("database changed before recovery", io::Error::from(io::ErrorKind::WouldBlock), StorageRecovery::InMemory),
            Err(error) => return failed("inspect database for recovery", error, StorageRecovery::InMemory),
        };
        before_move_locks();
        // Other live App connections retain a shared lease. SQLite's BUSY /
        // LOCKED results above also prohibit moving a database used externally.
        if let Err(error) = unlock(&live).and_then(|()| lock(&live, false)) {
            return failed("acquire exclusive recovery lease", error, StorageRecovery::InMemory);
        }
        let guard = match stamp[3].as_ref().map(|_| lease::sqlite_exclusive(db)).transpose() {
            Ok(guard) => guard,
            Err(error) => return failed("lock original database for recovery", error, StorageRecovery::InMemory),
        };
        match group_stamp(db) {
            Ok(current) if current == stamp => {}
            Ok(_) => return failed("database changed during confirmation", io::Error::from(io::ErrorKind::WouldBlock), StorageRecovery::InMemory),
            Err(error) => return failed("validate database for recovery", error, StorageRecovery::InMemory),
        }
        let members = stamp.each_ref().map(Option::is_some);
        match MovePlan::start(dir, db, members, orphaned) {
            Ok(plan) => (plan, guard),
            Err(error) => return failed("prepare database recovery", error, StorageRecovery::IncompleteMove),
        }
    };

    let backup = match plan.finish(dir, db, move_file) {
        Ok(backup) => backup,
        Err(error) => return failed("move database recovery group", error, StorageRecovery::IncompleteMove),
    };
    drop(sqlite_guard);
    match open_replacement(db, timeout, &live) {
        Ok(mut storage) => {
            storage.recovery_lease = Some(live);
            (storage, Some(StorageRecovery::MovedAside { backup, orphaned: plan.orphaned }))
        }
        Err(error) => failed("open replacement database", error, StorageRecovery::MovedAsideInMemory { backup }),
    }
}

fn open_replacement(db: &Path, timeout: Duration, live: &File) -> Result<Storage, OpenError> {
    unlock(live)?;
    lock(live, true)?;
    // Revalidate after the move is complete, immediately before SQLite opens.
    if group_members(db)? != [false; 4] {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Database files reappeared").into());
    }
    open_existing_or_empty(db, timeout)
}

#[cfg(test)]
mod tests;
