//! [自訂] SC-60（L-72）：在開啟資料庫、初始化日誌之前，先決定本程序是否為唯一實例。
//!
//! 單例外掛要到 Builder 建立時才生效，而且第一個程序在外掛就緒前（例如資料庫復原中），
//! 後啟動的程序會以為沒有實例而自己接手。所以在資料目錄用兩個鎖檔（作業系統的檔案鎖，
//! macOS 為 flock、Windows 為 LockFileEx，程序結束即釋放，異常結束不會留下死鎖）：
//!
//! - `startup.lock`：主程序從啟動持有到單例外掛就緒（使用者 setup 第一行）才放開。
//! - `instance.lock`：主程序持有到結束。
//!
//! 順序固定先 startup 再 instance。拿到 startup 但 instance 被占用，代表主程序已就緒，
//! 本程序只交由單例外掛轉交參數並結束；等不到 startup 就直接結束。兩種情況都不開資料庫、
//! 不做復原或清理，也不寫日誌。

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

pub(crate) const STARTUP_LOCK: &str = "startup.lock";
pub(crate) const INSTANCE_LOCK: &str = "instance.lock";
const POLL: Duration = Duration::from_millis(50);

/// 主程序持有的鎖。`instance` 要活到程序結束；`starting` 在單例外掛就緒後放開。
pub(crate) struct Primary {
    pub(crate) instance: File,
    pub(crate) starting: File,
}

pub(crate) enum Claim {
    Primary(Primary),
    /// 主程序已就緒：交給單例外掛轉交參數。
    Secondary,
    /// 主程序仍在啟動（例如資料庫復原）且超過等待時間。
    StartupTimeout,
    /// 檔案系統不支援鎖等錯誤：維持原本行為（直接啟動），由呼叫端記錄。
    Unavailable(io::Error),
}

/// 啟動時要做的事。只有 `Run` 會開資料庫。
pub(crate) enum Role {
    Run(Option<Primary>),
    Forward,
    Exit,
}

/// 主程序的實例鎖，持有到程序結束。
static INSTANCE: std::sync::OnceLock<File> = std::sync::OnceLock::new();

/// 主程序保存實例鎖到程序結束。
pub(crate) fn hold(instance: File) {
    let _ = INSTANCE.set(instance);
}

/// 程序內重新啟動（`AppHandle::restart` 會先產生新程序再結束自己）之前放開實例鎖，
/// 新程序才會成為主程序，而不是把參數轉交給正在結束的舊程序。
pub(crate) fn release_for_restart() {
    if let Some(instance) = INSTANCE.get() {
        release(instance);
    }
}

fn release(instance: &File) {
    if let Err(error) = instance.unlock() {
        log::warn!("[instance] failed to release the instance lock before restart: {error}");
    }
}

pub(crate) fn role(claim: Claim) -> Role {
    match claim {
        Claim::Primary(primary) => Role::Run(Some(primary)),
        Claim::Unavailable(_) => Role::Run(None),
        Claim::Secondary => Role::Forward,
        Claim::StartupTimeout => Role::Exit,
    }
}

fn open_lock(dir: &Path, name: &str) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).create(true).truncate(false).open(dir.join(name))
}

pub(crate) fn claim(dir: &Path, wait: Duration) -> Claim {
    match try_claim(dir, wait) {
        Ok(claim) => claim,
        Err(error) => Claim::Unavailable(error),
    }
}

fn try_claim(dir: &Path, wait: Duration) -> io::Result<Claim> {
    // 已有主程序時資料夾一定存在，這一步不會建立任何東西。
    std::fs::create_dir_all(dir)?;
    let starting = open_lock(dir, STARTUP_LOCK)?;
    let deadline = Instant::now() + wait;
    loop {
        match starting.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => std::thread::sleep(POLL),
            Err(TryLockError::WouldBlock) => return Ok(Claim::StartupTimeout),
            Err(TryLockError::Error(error)) => return Err(error),
        }
    }
    let instance = open_lock(dir, INSTANCE_LOCK)?;
    match instance.try_lock() {
        Ok(()) => Ok(Claim::Primary(Primary { instance, starting })),
        Err(TryLockError::WouldBlock) => Ok(Claim::Secondary),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("sayit-instance-{name}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir).unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    const SHORT: Duration = Duration::from_millis(200);

    #[test]
    fn a_second_launch_waits_for_startup_then_only_forwards() {
        let dir = temp_dir("forward");
        let Claim::Primary(mut primary) = claim(&dir, SHORT) else { panic!("first launch must be primary") };

        // 主程序還在啟動（例如資料庫復原中）：後來的程序等不到，直接結束，不跑單例外掛。
        let started = Instant::now();
        assert!(matches!(role(claim(&dir, SHORT)), Role::Exit));
        assert!(started.elapsed() >= SHORT);

        // 單例外掛就緒後放開 startup：後來的程序只轉交。
        primary.starting = open_lock(&dir, "released.lock").unwrap();
        assert!(matches!(role(claim(&dir, SHORT)), Role::Forward));
        // 後來的程序沒有開資料庫或寫入其他檔案。
        assert_eq!(entries(&dir), ["instance.lock", "released.lock", "startup.lock"]);

        drop(primary);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn locks_are_released_when_the_primary_ends() {
        let dir = temp_dir("release");
        let Claim::Primary(primary) = claim(&dir, SHORT) else { panic!() };
        // 異常結束等同關閉檔案：鎖隨之釋放，鎖檔留著也不妨礙之後啟動。
        drop(primary);
        assert!(matches!(role(claim(&dir, SHORT)), Role::Run(Some(_))));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn releasing_before_restart_lets_the_new_process_become_primary() {
        let dir = temp_dir("restart");
        let Claim::Primary(primary) = claim(&dir, SHORT) else { panic!() };
        drop(primary.starting);
        // 舊程序還活著、檔案仍開著，但已放開實例鎖。
        release(&primary.instance);
        assert!(matches!(role(claim(&dir, SHORT)), Role::Run(Some(_))));
        drop(primary.instance);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_slow_startup_is_waited_for() {
        let dir = temp_dir("wait");
        let Claim::Primary(primary) = claim(&dir, SHORT) else { panic!() };
        let Primary { instance, starting } = primary;
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            drop(starting);
        });
        assert!(matches!(role(claim(&dir, Duration::from_secs(5))), Role::Forward));
        release.join().unwrap();
        drop(instance);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn roles_open_storage_only_for_a_primary_or_when_locks_are_unavailable() {
        assert!(matches!(role(Claim::Secondary), Role::Forward));
        assert!(matches!(role(Claim::StartupTimeout), Role::Exit));
        assert!(matches!(role(Claim::Unavailable(io::ErrorKind::Unsupported.into())), Role::Run(None)));
        let dir = temp_dir("unavailable");
        let file = dir.join("not-a-dir");
        fs::write(&file, b"").unwrap();
        assert!(matches!(claim(&file, SHORT), Claim::Unavailable(_)));
        fs::remove_dir_all(dir).unwrap();
    }
}
