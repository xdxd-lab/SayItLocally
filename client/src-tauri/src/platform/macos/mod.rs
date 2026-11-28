//! macOS 系統整合。
//!
//! - `ffi`：CoreFoundation / CoreGraphics / Accessibility / Carbon / CoreAudio 的 C 宣告
//! - `cf`：CF 物件所有權與字串互轉
//! - `ax`：Accessibility（對應 Windows 的 UI Automation）
//! - `keycodes`：macOS 鍵碼 ↔ 內部鍵 ID（沿用 Windows VK）
//! - `appkit`：NSWorkspace / NSPasteboard / NSWindow（objc2）
//! - `audio`：CoreAudio 靜音
//! - `sysinfo`：系統版本、記憶體、語言
//! - `permissions`：輔助功能 / 麥克風授權狀態與引導

// 平台封裝層：部分介面給診斷與後續功能預留，未全部被呼叫。
#![allow(dead_code)]

pub mod appkit;
pub mod audio;
pub mod ax;
pub mod cf;
pub mod ffi;
pub mod focus;
pub mod keycodes;
pub mod permissions;
pub mod sysinfo;
