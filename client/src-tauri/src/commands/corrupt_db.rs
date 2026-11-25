// [自訂] L-72：只管理 SQLite 復原留下的備份及其側車檔。
use std::{collections::BTreeMap, fs, io, path::{Path, PathBuf}};
use serde::Serialize;
use crate::storage::recovery;

const PREFIX: &str = "sayit.db.corrupt-";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CorruptDbCopy {
    name: String,
    created_at_local: String,
    size_bytes: u64,
}

struct CopyGroup {
    info: CorruptDbCopy,
    files: Vec<PathBuf>,
}

fn backup_timestamp(base: &str) -> Option<chrono::NaiveDateTime> {
    let name = base.strip_prefix(PREFIX)?;
    let stamp = name.get(..15)?;
    let suffix = name.get(15..)?;
    if !suffix.is_empty() {
        let id = suffix.strip_prefix('-')?;
        if !uuid::Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id) {
            return None;
        }
    }
    let (date, time) = stamp.split_once('-')?;
    if date.len() != 8 || time.len() != 6 || !date.bytes().chain(time.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%d-%H%M%S").ok()
}

fn copy_groups(dir: &Path) -> io::Result<BTreeMap<String, CopyGroup>> {
    let mut groups = BTreeMap::new();
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err(io::Error::other("App data directory is not a regular directory")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(groups),
        Err(error) => return Err(error),
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let base = recovery::COMPANION_SUFFIXES.iter()
            .find_map(|suffix| name.strip_suffix(suffix)).unwrap_or(name);
        let Some(timestamp) = backup_timestamp(base) else { continue };
        let path = entry.path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => meta,
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let group = groups.entry(base.to_owned()).or_insert_with(|| CopyGroup {
            info: CorruptDbCopy {
                name: base.to_owned(),
                created_at_local: timestamp.format("%Y-%m-%dT%H:%M:%S").to_string(),
                size_bytes: 0,
            },
            files: Vec::new(),
        });
        group.info.size_bytes = group.info.size_bytes.saturating_add(metadata.len());
        group.files.push(path);
    }
    Ok(groups)
}

fn find_copy(dir: &Path, name: &str) -> Result<CopyGroup, String> {
    if backup_timestamp(name).is_none() {
        return Err("Invalid database copy name".into());
    }
    copy_groups(dir).map_err(|error| error.to_string())?
        .remove(name).ok_or_else(|| "Database copy no longer exists".into())
}

fn delete_copy(dir: &Path, name: &str) -> Result<(), String> {
    let _startup = recovery::startup_lock(dir).map_err(|error| error.to_string())?;
    if recovery::recovery_move_pending(dir) {
        return Err("Database recovery is still pending".into());
    }
    let mut group = find_copy(dir, name)?;
    // 先處理側車檔，若中途出錯，剩下的檔案在重新列舉後仍可操作。
    group.files.sort_unstable_by_key(|path| path.file_name().is_some_and(|file| file == name));
    for path in group.files {
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err("Database copy member is no longer a regular file".into());
        }
        fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn list_corrupt_db_copies() -> Result<Vec<CorruptDbCopy>, String> {
    tokio::task::spawn_blocking(|| {
        copy_groups(&super::app_data_dir())
            .map(|groups| groups.into_values().rev().map(|group| group.info).collect())
            .map_err(|error| error.to_string())
    }).await.map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn reveal_corrupt_db_copy(name: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let dir = super::app_data_dir().canonicalize().map_err(|error| error.to_string())?;
        let group = find_copy(&super::app_data_dir(), &name)?;
        let file = group.files.iter()
            .find(|path| path.file_name().is_some_and(|file| file == name.as_str()))
            .or_else(|| group.files.first())
            .and_then(|path| path.file_name())
            .ok_or("Database copy no longer exists")?;
        let path = dir.join(file);
        if !fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file()) {
            return Err("Database copy no longer exists".into());
        }
        super::system::reveal_file_in_folder(
            path.to_str().ok_or("Cannot display this path")?.to_owned(),
        )
    }).await.map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn delete_corrupt_db_copy(name: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || delete_copy(&super::app_data_dir(), &name))
        .await.map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn recording_cleanup_protected() -> bool {
    tauri::async_runtime::spawn_blocking(|| recovery::recording_cleanup_protected(&super::app_data_dir()))
        .await.unwrap_or(true)
}

#[tauri::command]
pub async fn resume_recording_cleanup() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| recovery::resume_recording_cleanup(&super::app_data_dir())
        .map_err(|error| error.to_string()))
        .await.map_err(|error| error.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_sidecars_and_deletes_only_the_selected_backup() {
        let dir = std::env::temp_dir().join(format!("sayit-corrupt-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let base = "sayit.db.corrupt-20261019-153213";
        let wal = format!("{base}-wal");
        let other = "sayit.db.corrupt-20261019-160000-shm";
        let invalid = "sayit.db.corrupt-20260230-153213";
        for name in [base, wal.as_str(), other, invalid, "sayit.db", "sayit.db-wal"] {
            fs::write(dir.join(name), b"test").unwrap();
        }
        fs::create_dir(dir.join(format!("{base}-shm"))).unwrap();
        let groups = copy_groups(&dir).unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[base].info.size_bytes, 8);
        assert_eq!(groups[base].info.created_at_local, "2026-10-19T15:32:13");
        assert_eq!(groups["sayit.db.corrupt-20261019-160000"].info.size_bytes, 4);
        for name in ["sayit.db", "../sayit.db", wal.as_str(), invalid, "sayit.db.corrupt-20261019-153213/../sayit.db"] {
            assert!(delete_copy(&dir, name).is_err());
        }
        delete_copy(&dir, base).unwrap();
        assert!(!dir.join(base).exists());
        assert!(!dir.join(&wal).exists());
        for untouched in ["sayit.db", "sayit.db-wal", other, invalid] {
            assert_eq!(fs::read(dir.join(untouched)).unwrap(), b"test");
        }
        assert!(dir.join(format!("{base}-shm")).is_dir());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn uuid_group_journal_is_listed_and_deleting_the_copy_preserves_the_recording_hold() {
        use crate::storage::{Storage, StorageRecovery};

        let dir = std::env::temp_dir().join(format!("sayit-corrupt-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let db = dir.join("sayit.db");
        fs::write(&db, b"damaged test database").unwrap();
        let (storage, recovery) = Storage::open_or_recover(db);
        let Some(StorageRecovery::MovedAside { backup, .. }) = recovery else { panic!("{recovery:?}") };
        let name = backup.file_name().unwrap().to_str().unwrap();
        let journal = dir.join(format!("{name}-journal"));
        fs::write(&journal, b"journal").unwrap();
        let groups = copy_groups(&dir).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[name].info.size_bytes, 28);
        assert!(groups[name].files.contains(&journal));
        assert!(recovery::recording_cleanup_protected(&dir));
        delete_copy(&dir, name).unwrap();
        assert!(copy_groups(&dir).unwrap().is_empty());
        assert!(!journal.exists());
        assert!(recovery::recording_cleanup_protected(&dir));
        drop(storage);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn recovery_copy_with_only_a_wal_can_be_listed_and_deleted() {
        use crate::storage::{Storage, StorageRecovery};

        let dir = std::env::temp_dir().join(format!("sayit-corrupt-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("sayit.db-wal"), b"orphan WAL").unwrap();
        let (storage, mode) = Storage::open_or_recover(dir.join("sayit.db"));
        let Some(StorageRecovery::MovedAside { backup, orphaned: true }) = mode else { panic!("{mode:?}") };
        let name = backup.file_name().unwrap().to_str().unwrap();
        let copy = copy_groups(&dir).unwrap().remove(name).unwrap();
        assert_eq!(copy.info.size_bytes, 10);
        assert_eq!(copy.files, [dir.join(format!("{name}-wal"))]);
        delete_copy(&dir, name).unwrap();
        assert!(copy_groups(&dir).unwrap().is_empty());
        assert!(recovery::recording_cleanup_protected(&dir));
        drop(storage);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn deletion_requires_the_startup_lock_and_no_pending_move() {
        let dir = std::env::temp_dir().join(format!("sayit-corrupt-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let name = "sayit.db.corrupt-20261019-153213";
        let copy = dir.join(name);
        fs::write(&copy, b"original database copy").unwrap();
        let lock = recovery::startup_lock(&dir).unwrap();
        assert!(delete_copy(&dir, name).is_err());
        assert_eq!(fs::read(&copy).unwrap(), b"original database copy");
        drop(lock);
        let pending = dir.join("sayit.db.recovery-pending");
        fs::write(&pending, b"pending move").unwrap();
        assert!(delete_copy(&dir, name).is_err());
        assert_eq!(fs::read(&copy).unwrap(), b"original database copy");
        fs::remove_file(pending).unwrap();
        delete_copy(&dir, name).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn ignores_symlinks_for_main_file_and_sidecars() {
        let dir = std::env::temp_dir().join(format!("sayit-corrupt-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let target = dir.join("sayit.db");
        fs::write(&target, b"active").unwrap();
        let base = "sayit.db.corrupt-20261019-153213";
        let link = dir.join(base);
        let symlink = std::os::unix::fs::symlink;
        symlink(&target, &link).unwrap();
        let side = dir.join(format!("{base}-shm"));
        symlink(&target, &side).unwrap();
        assert!(copy_groups(&dir).unwrap().is_empty());
        assert!(delete_copy(&dir, base).is_err());
        fs::write(dir.join(format!("{base}-wal")), b"backup").unwrap();
        delete_copy(&dir, base).unwrap();
        assert!(fs::symlink_metadata(link).unwrap().is_symlink());
        assert!(fs::symlink_metadata(side).unwrap().is_symlink());
        assert_eq!(fs::read(target).unwrap(), b"active");
        fs::remove_dir_all(dir).unwrap();
    }
}
