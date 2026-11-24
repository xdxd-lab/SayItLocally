pub mod audio;
pub mod audio_mute;
pub mod backup;
pub mod corrupt_db;
pub mod diagnostics;
pub mod export;
pub mod paste;
pub mod permissions;
pub mod shortcuts;
pub mod storage;
pub mod system;
pub mod tray;
pub mod webdav;
pub mod window;
pub mod update_notification;

// [自訂] REV9：測試程序一律將 App 資料導向獨立暫存位置。
pub(crate) fn app_data_dir() -> std::path::PathBuf {
    #[cfg(test)]
    {
        static ROOT: std::sync::LazyLock<std::path::PathBuf> = std::sync::LazyLock::new(|| {
            std::env::temp_dir().join(format!("sayit-test-{}", uuid::Uuid::new_v4()))
        });
        ROOT.clone()
    }
    #[cfg(not(test))]
    {
        dirs::data_local_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("com.sayitlocally.desktop")
    }
}
