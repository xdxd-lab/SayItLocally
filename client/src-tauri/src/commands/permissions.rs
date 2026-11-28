//! 系統許可權狀態與引導（主要給 macOS 用）。
//!
//! macOS 上熱鍵吞鍵、模擬 ⌘V、讀游標上下文都要「輔助功能」授權，錄音要麥克風授權；
//! 前端用這幾個命令顯示授權狀態、帶使用者去系統設定。Windows 上不需要額外授權，
//! 返回固定的「已授權」，前端據此不顯示引導。

use serde_json::{json, Value};

/// 當前平台的許可權狀態。
#[tauri::command]
pub fn get_platform_permissions() -> Value {
    #[cfg(target_os = "macos")]
    {
        let status = crate::platform::macos::permissions::status(crate::keyboard::tap_active());
        serde_json::to_value(status).unwrap_or_else(|_| json!({ "platform": "macos" }))
    }
    #[cfg(not(target_os = "macos"))]
    {
        json!({
            "platform": std::env::consts::OS,
            "accessibility": true,
            "hotkeyActive": true,
            "microphone": "authorized",
            "secureInput": false,
            "translocated": false,
            "bundlePath": "",
        })
    }
}

/// 請求「輔助功能」授權：還沒授權時系統會彈出引導對話方塊。返回當前是否已授權。
#[tauri::command]
pub fn request_accessibility_permission() -> bool {
    #[cfg(target_os = "macos")]
    {
        crate::platform::macos::permissions::request_accessibility()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// 開啟系統設定裡的隱私面板：accessibility / microphone / input_monitoring。
#[tauri::command]
pub fn open_privacy_settings(pane: String) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        crate::platform::macos::permissions::open_privacy_pane(&pane)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pane;
        Err("Privacy settings are only needed on macOS".to_string())
    }
}
