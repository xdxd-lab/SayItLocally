//! 平台相關的系統整合。
//!
//! Windows 的實現目前仍分散在 keyboard / inject / context / window 等模組裡的
//! `#[cfg(windows)]` 分支中（歷史原因，暫不搬動，避免在無法編譯 Windows 的環境裡改動它）。
//! macOS 的實現集中在 `macos` 子模組，各業務模組在自己的 `#[cfg(target_os = "macos")]`
//! 分支裡呼叫它。

#[cfg(target_os = "macos")]
pub mod macos;
