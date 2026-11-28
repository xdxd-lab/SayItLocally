//! macOS 隱私許可權：輔助功能、麥克風、安全輸入狀態，以及開啟對應的系統設定頁。
//!
//! SayIt 在 mac 上需要兩項授權：
//! - **輔助功能**：能吞鍵的全域性熱鍵（CGEventTap）、模擬 ⌘V、讀取游標附近文字都靠它。
//!   系統不會彈窗直接授予，只能引導使用者去「系統設定 → 隱私與安全性 → 輔助功能」勾選。
//! - **麥克風**：第一次 getUserMedia 時系統自動彈窗（Info.plist 裡有用途說明）。

use objc2::rc::autoreleasepool;
use objc2::{class, msg_send};
use objc2_foundation::NSString;
use serde::Serialize;

#[link(name = "AVFoundation", kind = "framework")]
extern "C" {}

/// AVAuthorizationStatus 的取值。
fn describe_av_status(status: isize) -> &'static str {
    match status {
        0 => "not_determined",
        1 => "restricted",
        2 => "denied",
        3 => "authorized",
        _ => "unknown",
    }
}

/// 麥克風授權狀態：not_determined / restricted / denied / authorized。
pub fn microphone_status() -> &'static str {
    autoreleasepool(|_| unsafe {
        // AVMediaTypeAudio 的值就是 "soun"
        let media_type = NSString::from_str("soun");
        let status: isize = msg_send![class!(AVCaptureDevice), authorizationStatusForMediaType: &*media_type];
        describe_av_status(status)
    })
}

/// 是否有程式開啟了「安全輸入」（密碼框、終端的「安全鍵盤輸入」等）。
/// 開啟期間 event tap 收不到鍵盤事件，鍵盤熱鍵會失靈（滑鼠鍵不受影響）。
pub fn secure_input_enabled() -> bool {
    unsafe { super::ffi::IsSecureEventInputEnabled() != 0 }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionStatus {
    pub platform: &'static str,
    /// 輔助功能是否已授權
    pub accessibility: bool,
    /// 全域性熱鍵的 event tap 是否已經裝上並處於啟用狀態
    pub hotkey_active: bool,
    pub microphone: &'static str,
    pub secure_input: bool,
    /// App 是否在被「App Translocation」隨機路徑裡執行（直接在 DMG / 下載目錄裡開啟時會這樣），
    /// 這種情況下自動更新會失敗，應提示使用者把 SayIt 拖進「應用程式」。
    pub translocated: bool,
    pub bundle_path: String,
}

pub fn status(hotkey_active: bool) -> PermissionStatus {
    let bundle_path = current_bundle_path().unwrap_or_default();
    PermissionStatus {
        platform: "macos",
        accessibility: super::ax::is_trusted(),
        hotkey_active,
        microphone: microphone_status(),
        secure_input: secure_input_enabled(),
        translocated: bundle_path.contains("/AppTranslocation/"),
        bundle_path,
    }
}

/// 當前執行的 `.app` 包路徑（開發模式下是執行檔所在目錄）。
pub fn current_bundle_path() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let mut path = exe.as_path();
    while let Some(parent) = path.parent() {
        if path.extension().map(|e| e == "app").unwrap_or(false) {
            return Some(path.to_string_lossy().into_owned());
        }
        path = parent;
    }
    exe.parent().map(|p| p.to_string_lossy().into_owned())
}

/// 系統設定裡的隱私面板。
pub fn open_privacy_pane(pane: &str) -> Result<(), String> {
    let anchor = match pane {
        "accessibility" => "Privacy_Accessibility",
        "microphone" => "Privacy_Microphone",
        "input_monitoring" => "Privacy_ListenEvent",
        _ => return Err(format!("unknown privacy pane: {pane}")),
    };
    let url = format!("x-apple.systempreferences:com.apple.preference.security?{anchor}");
    std::process::Command::new("open")
        .arg(&url)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("failed to open System Settings: {e}"))
}

/// 請求輔助功能授權：沒授權時系統會彈出「SayIt 想控制這台電腦」的對話方塊，
/// 帶「開啟系統設定」按鈕。返回當前是否已授權。
pub fn request_accessibility() -> bool {
    super::ax::is_trusted_with_prompt(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_is_reported_without_prompting() {
        let s = status(false);
        assert_eq!(s.platform, "macos");
        assert!(["not_determined", "restricted", "denied", "authorized", "unknown"].contains(&s.microphone));
    }
}
