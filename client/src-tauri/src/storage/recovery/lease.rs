use super::OpenError;
use std::{fs::File, io, path::Path};

#[cfg(unix)]
fn flock(file: &File, operation: i32) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    extern "C" {
        #[link_name = "flock"]
        fn native_flock(fd: i32, operation: i32) -> i32;
    }
    // The call is synchronous, and the File keeps the descriptor alive.
    if unsafe { native_flock(file.as_raw_fd(), operation) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(unix)]
pub(super) fn lock(file: &File, shared: bool) -> io::Result<()> {
    const SHARED: i32 = 1;
    const EXCLUSIVE: i32 = 2;
    const NONBLOCKING: i32 = 4;
    flock(file, (if shared { SHARED } else { EXCLUSIVE }) | NONBLOCKING)
}

#[cfg(unix)]
pub(super) fn unlock(file: &File) -> io::Result<()> {
    const UNLOCK: i32 = 8;
    flock(file, UNLOCK)
}

// Lock SQLite's actual database file through its VFS, without running SQL or
// letting the pager touch WAL/journal. The connection owns the handle and does
// not checkpoint at close. Do not open and close another FD to the same inode
// while this guard lives: POSIX would release the process's database locks.
#[cfg(unix)]
pub(super) fn sqlite_exclusive(path: &Path) -> Result<rusqlite::Connection, OpenError> {
    use super::Storage;
    use rusqlite::{ffi, Connection, OpenFlags};
    use std::{ffi::c_void, ptr};

    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW
        | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_EXRESCODE;
    let db = Connection::open_with_flags(Storage::sqlite_path(path)?, flags)?;
    db.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
    let mut file: *mut ffi::sqlite3_file = ptr::null_mut();
    // SQLite returns its own VFS file pointer, valid until this connection closes.
    let result = unsafe {
        ffi::sqlite3_file_control(db.handle(), c"main".as_ptr(), ffi::SQLITE_FCNTL_FILE_POINTER,
            (&mut file as *mut *mut ffi::sqlite3_file).cast::<c_void>())
    };
    sqlite_result(result)?;
    let methods = unsafe { file.as_ref().and_then(|file| file.pMethods.as_ref()) };
    let lock = methods.and_then(|methods| methods.xLock)
        .ok_or_else(|| io::Error::from(io::ErrorKind::Unsupported))?;
    for level in [ffi::SQLITE_LOCK_SHARED, ffi::SQLITE_LOCK_RESERVED, ffi::SQLITE_LOCK_EXCLUSIVE] {
        sqlite_result(unsafe { lock(file, level) })?;
    }
    Ok(db)
}

#[cfg(unix)]
fn sqlite_result(result: i32) -> Result<(), OpenError> {
    if result == rusqlite::ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(result), None).into())
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{ffi::c_void, os::windows::io::AsRawHandle, ptr};

    #[repr(C)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u32,
        offset_high: u32,
        event: *mut c_void,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn LockFileEx(handle: *mut c_void, flags: u32, reserved: u32, low: u32, high: u32, data: *mut Overlapped) -> i32;
        fn UnlockFileEx(handle: *mut c_void, reserved: u32, low: u32, high: u32, data: *mut Overlapped) -> i32;
    }

    pub(super) fn lock_range(file: &File, offset: u32, size: u32, shared: bool) -> io::Result<()> {
        let mut data = Overlapped { internal: 0, internal_high: 0, offset, offset_high: 0, event: ptr::null_mut() };
        const FAIL_IMMEDIATELY: u32 = 1;
        const EXCLUSIVE: u32 = 2;
        let flags = FAIL_IMMEDIATELY | if shared { 0 } else { EXCLUSIVE };
        // The file is opened for synchronous I/O; the offset lives until this call returns.
        if unsafe { LockFileEx(file.as_raw_handle(), flags, 0, size, 0, &mut data) } != 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(33) { // ERROR_LOCK_VIOLATION
            return Err(io::ErrorKind::WouldBlock.into());
        }
        Err(error)
    }

    pub(super) fn unlock(file: &File) -> io::Result<()> {
        let mut data = Overlapped { internal: 0, internal_high: 0, offset: 0, offset_high: 0, event: ptr::null_mut() };
        if unsafe { UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut data) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(windows)]
pub(super) fn lock(file: &File, shared: bool) -> io::Result<()> {
    windows::lock_range(file, 0, 1, shared)
}

#[cfg(windows)]
pub(super) fn unlock(file: &File) -> io::Result<()> {
    windows::unlock(file)
}

#[cfg(windows)]
pub(super) fn sqlite_exclusive(path: &Path) -> Result<File, OpenError> {
    // SQLite's Windows VFS doesn't allow renaming its open handle. Open with
    // Windows' default Rust share-delete mode and reserve SQLite's 512-byte
    // lock range (pending, reserved and all 510 shared lock bytes) ourselves.
    let file = super::file_options().write(true).open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::from(io::ErrorKind::InvalidData).into());
    }
    windows::lock_range(&file, 0x4000_0000, 512, false)?;
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn lock(_file: &File, _shared: bool) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(not(any(unix, windows)))]
pub(super) fn unlock(_file: &File) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(not(any(unix, windows)))]
pub(super) fn sqlite_exclusive(_path: &Path) -> Result<File, OpenError> {
    Err(io::Error::from(io::ErrorKind::Unsupported).into())
}
