//! AppKit / Foundation（objc2）：前台應用、剪貼簿、視窗層級。
//!
//! 執行緒：NSWindow 的方法必須在主執行緒呼叫（呼叫方負責 `run_on_main_thread`）；
//! NSWorkspace / NSPasteboard 在後台執行緒也能用，但都包在 autoreleasepool 裡，
//! 避免後台執行緒上自動釋放的物件沒人回收。

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send, sel}; // [自訂] S33：sel 用於 respondsToSelector
use objc2_foundation::{NSArray, NSData, NSRect, NSString};
use std::ffi::{c_char, c_void, CStr};
use std::time::{Duration, Instant};

/// NSString → Rust String。
///
/// # Safety
/// `obj` 必須是 NSString 或空指標。
unsafe fn nsstring_to_string(obj: *mut AnyObject) -> Option<String> {
    if obj.is_null() {
        return None;
    }
    let utf8: *const c_char = msg_send![obj, UTF8String];
    if utf8.is_null() {
        return None;
    }
    Some(CStr::from_ptr(utf8).to_string_lossy().into_owned())
}

/// NSURL → 檔案路徑。
unsafe fn nsurl_path(url: *mut AnyObject) -> Option<String> {
    if url.is_null() {
        return None;
    }
    nsstring_to_string(msg_send![url, path])
}

// ─── 前台應用 ───

#[derive(Debug, Clone, Default)]
pub struct RunningApp {
    pub pid: i32,
    pub bundle_id: String,
    pub name: String,
    /// 執行檔路徑（`…/Contents/MacOS/xxx`）
    pub executable_path: String,
    /// `.app` 包路徑
    pub bundle_path: String,
}

impl RunningApp {
    /// 執行檔名（對應 Windows 的程序名，例如 `WeChat`、`Electron`）。
    pub fn executable_name(&self) -> String {
        std::path::Path::new(&self.executable_path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

unsafe fn running_app_info(app: *mut AnyObject) -> Option<RunningApp> {
    if app.is_null() {
        return None;
    }
    let pid: i32 = msg_send![app, processIdentifier];
    Some(RunningApp {
        pid,
        bundle_id: nsstring_to_string(msg_send![app, bundleIdentifier]).unwrap_or_default(),
        name: nsstring_to_string(msg_send![app, localizedName]).unwrap_or_default(),
        executable_path: nsurl_path(msg_send![app, executableURL]).unwrap_or_default(),
        bundle_path: nsurl_path(msg_send![app, bundleURL]).unwrap_or_default(),
    })
}

/// 當前前台應用（選單欄屬於它的那個）。
pub fn frontmost_app() -> Option<RunningApp> {
    autoreleasepool(|_| unsafe {
        let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        if ws.is_null() {
            return None;
        }
        let app: *mut AnyObject = msg_send![ws, frontmostApplication];
        running_app_info(app)
    })
}

// [自訂] L-80：貼上前只拿鍵盤前景 PID，不讀 App 名稱、路徑或視窗內容。
pub fn frontmost_pid() -> Option<i32> {
    autoreleasepool(|_| unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        if workspace.is_null() {
            return None;
        }
        let app: *mut AnyObject = msg_send![workspace, frontmostApplication];
        if app.is_null() {
            return None;
        }
        let pid: i32 = msg_send![app, processIdentifier];
        (pid > 0).then_some(pid)
    })
}

/// 按 pid 取應用資訊。
pub fn running_app_for_pid(pid: i32) -> Option<RunningApp> {
    if pid <= 0 {
        return None;
    }
    autoreleasepool(|_| unsafe {
        let app: *mut AnyObject =
            msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
        running_app_info(app)
    })
}

// ─── 剪貼簿（NSPasteboard）───

/// 純文本型別（`NSPasteboardTypeString` 的值）。
pub const PASTEBOARD_TYPE_STRING: &str = "public.utf8-plain-text";
/// nspasteboard.org 約定：這份內容很快會被還原或替換，剪貼簿管理器不要記進歷史。
/// Maccy、Paste、Raycast、Alfred 等都認它（對應 Windows 側的 ExcludeClipboardContentFromMonitorProcessing 等格式）。
pub const PASTEBOARD_TYPE_TRANSIENT: &str = "org.nspasteboard.TransientType";

unsafe fn general_pasteboard() -> *mut AnyObject {
    msg_send![class!(NSPasteboard), generalPasteboard]
}

/// [自訂] S33：`NSPasteboardContentsCurrentHostOnly`（NSPasteboardContentsOptions 的 1 << 0）。
/// 寫入的內容只留在這台 Mac，不經「通用剪貼板」（接力）同步到其他裝置。
const NS_PASTEBOARD_CONTENTS_CURRENT_HOST_ONLY: usize = 1 << 0;

/// [自訂] S33：清空剪貼簿並標記「只限本機」，取代 clearContents／declareTypes。
/// `prepareForNewContentsWithOptions:` 是 macOS 10.12+；更舊的系統退回 clearContents。
/// 注意：之後不能再呼叫 declareTypes:owner:（它會重新清空並丟掉這個選項）。
unsafe fn prepare_current_host_only(pb: *mut AnyObject) -> isize {
    let responds: bool = msg_send![pb, respondsToSelector: sel!(prepareForNewContentsWithOptions:)];
    if responds {
        msg_send![pb, prepareForNewContentsWithOptions: NS_PASTEBOARD_CONTENTS_CURRENT_HOST_ONLY]
    } else {
        msg_send![pb, clearContents]
    }
}

/// 剪貼簿的修改計數。每次有人寫剪貼簿它都會變（對應 GetClipboardSequenceNumber）。
pub fn pasteboard_change_count() -> isize {
    autoreleasepool(|_| unsafe {
        let pb = general_pasteboard();
        if pb.is_null() {
            return -1;
        }
        msg_send![pb, changeCount]
    })
}

pub fn pasteboard_get_text() -> Option<String> {
    autoreleasepool(|_| unsafe {
        let pb = general_pasteboard();
        if pb.is_null() {
            return None;
        }
        let ty = NSString::from_str(PASTEBOARD_TYPE_STRING);
        let value: *mut AnyObject = msg_send![pb, stringForType: &*ty];
        nsstring_to_string(value)
    })
}

// [自訂] L-80：寫入失敗時保留取得所有權時的計數，只還原本程式仍持有的剪貼簿。
pub struct PasteboardWriteError {
    pub detail: &'static str,
    pub written_count: Option<isize>,
}

pub fn pasteboard_set_text(text: &str, transient: bool) -> Result<isize, PasteboardWriteError> {
    write_pasteboard_text(text, transient, None)
}

pub fn pasteboard_restore_text(text: &str, expected_count: isize) -> bool {
    write_pasteboard_text(text, true, Some(expected_count)).is_ok()
}

fn write_pasteboard_text(
    text: &str,
    transient: bool,
    expected_count: Option<isize>,
) -> Result<isize, PasteboardWriteError> {
    autoreleasepool(|_| unsafe {
        let pb = general_pasteboard();
        if pb.is_null() {
            return Err(PasteboardWriteError { detail: "step=generalPasteboard err=null", written_count: None });
        }
        let string_type = NSString::from_str(PASTEBOARD_TYPE_STRING);
        let item: Option<Retained<AnyObject>> = msg_send![class!(NSPasteboardItem), new];
        let Some(item) = item else {
            return Err(PasteboardWriteError { detail: "step=NSPasteboardItem err=null", written_count: None });
        };
        let value = NSString::from_str(text);
        let ok: bool = msg_send![&*item, setString: &*value, forType: &*string_type];
        if !ok {
            return Err(PasteboardWriteError { detail: "step=setString err=rejected", written_count: None });
        }
        if transient {
            let transient_type = NSString::from_str(PASTEBOARD_TYPE_TRANSIENT);
            let empty = NSData::with_bytes(&[]);
            let _: bool = msg_send![&*item, setData: &*empty, forType: &*transient_type];
        }
        let array = NSArray::from_retained_slice(&[item]);
        if let Some(expected) = expected_count {
            let actual: isize = msg_send![pb, changeCount];
            if actual != expected {
                return Err(PasteboardWriteError { detail: "clipboard ownership changed", written_count: None });
            }
        }
        let written_count = prepare_current_host_only(pb);
        let written: bool = msg_send![pb, writeObjects: &*array];
        if !written {
            return Err(PasteboardWriteError { detail: "step=writeObjects err=rejected", written_count: Some(written_count) });
        }
        Ok(written_count)
    })
}

pub fn pasteboard_clear() -> bool {
    autoreleasepool(|_| unsafe {
        let pb = general_pasteboard();
        if pb.is_null() {
            return false;
        }
        let _: isize = msg_send![pb, clearContents];
        true
    })
}

// [自訂] L-80：拒絕貼上時不清除其他程式後來放入的內容。
pub fn pasteboard_clear_if_unchanged(expected_count: isize) -> bool {
    autoreleasepool(|_| unsafe {
        let pb = general_pasteboard();
        if pb.is_null() {
            return false;
        }
        let count: isize = msg_send![pb, changeCount];
        if count != expected_count {
            return false;
        }
        prepare_current_host_only(pb);
        true
    })
}

/// 插入前剪貼簿裡的全部內容：每個 item 的每種型別各一份位元組。
#[derive(Debug, Default, Clone)]
pub struct PasteboardSnapshot {
    pub items: Vec<Vec<(String, Vec<u8>)>>,
}

impl PasteboardSnapshot {
    pub fn type_count(&self) -> usize {
        self.items.iter().map(Vec::len).sum()
    }
}

unsafe fn nsarray_count(array: *mut AnyObject) -> usize {
    if array.is_null() {
        0
    } else {
        msg_send![array, count]
    }
}

unsafe fn nsarray_get(array: *mut AnyObject, index: usize) -> *mut AnyObject {
    msg_send![array, objectAtIndex: index]
}

/// 儘量儲存剪貼簿格式，跳過讀不到的型別。超時或超出空間上限返回 None，
/// 呼叫方仍可貼上，並在保護開啟時嘗試以之前的純文本還原。
///
/// 預算的理由與 Windows 側相同：有些程式是「延遲提供」資料，讀的那一刻才現場生成，
/// 可能很慢，而這一步在插字的關鍵路徑上。
pub fn pasteboard_snapshot(budget: Duration, max_bytes: usize) -> Option<PasteboardSnapshot> {
    let started = Instant::now();
    autoreleasepool(|_| unsafe {
        let pb = general_pasteboard();
        if pb.is_null() {
            return None;
        }
        let items: *mut AnyObject = msg_send![pb, pasteboardItems];
        if items.is_null() {
            return None;
        }
        let mut snapshot = PasteboardSnapshot::default();
        let mut total = 0usize;
        for i in 0..nsarray_count(items) {
            let item = nsarray_get(items, i);
            let types: *mut AnyObject = msg_send![item, types];
            let mut entry = Vec::new();
            for j in 0..nsarray_count(types) {
                if started.elapsed() > budget {
                    return None;
                }
                let ty = nsarray_get(types, j);
                let Some(type_name) = nsstring_to_string(ty) else { continue };
                // 我們自己的標記不必還原
                if type_name == PASTEBOARD_TYPE_TRANSIENT {
                    continue;
                }
                let data: *mut AnyObject = msg_send![item, dataForType: ty];
                if data.is_null() {
                    continue;
                }
                let len: usize = msg_send![data, length];
                total = total.saturating_add(len);
                if total > max_bytes {
                    return None;
                }
                let bytes: *const c_void = msg_send![data, bytes];
                if len > 0 && bytes.is_null() {
                    continue;
                }
                let copy = if len == 0 {
                    Vec::new()
                } else {
                    std::slice::from_raw_parts(bytes as *const u8, len).to_vec()
                };
                entry.push((type_name, copy));
            }
            if !entry.is_empty() {
                snapshot.items.push(entry);
            }
        }
        if started.elapsed() > budget {
            return None;
        }
        Some(snapshot)
    })
}

/// 把快照寫回剪貼簿。`transient` 為真時給第一個 item 加 TransientType 標記，
/// 免得剪貼簿管理器把「還原」再記成一條新歷史。
pub fn pasteboard_restore(snapshot: &PasteboardSnapshot, transient: bool, expected_count: isize) -> bool {
    autoreleasepool(|_| unsafe {
        let pb = general_pasteboard();
        if pb.is_null() {
            return false;
        }
        let transient_type = NSString::from_str(PASTEBOARD_TYPE_TRANSIENT);
        let mut new_items: Vec<Retained<AnyObject>> = Vec::with_capacity(snapshot.items.len());
        for (index, entry) in snapshot.items.iter().enumerate() {
            let item: Option<Retained<AnyObject>> = msg_send![class!(NSPasteboardItem), new];
            let Some(item) = item else { continue };
            for (type_name, bytes) in entry {
                let ty = NSString::from_str(type_name);
                let data = NSData::with_bytes(bytes);
                let _: bool = msg_send![&*item, setData: &*data, forType: &*ty];
            }
            if transient && index == 0 {
                let empty = NSData::with_bytes(&[]);
                let _: bool = msg_send![&*item, setData: &*empty, forType: &*transient_type];
            }
            new_items.push(item);
        }
        let array = NSArray::from_retained_slice(&new_items);
        let count: isize = msg_send![pb, changeCount];
        if count != expected_count {
            return false;
        }
        // [自訂] S33：還原也標記「只限本機」。先備妥所有格式，最後才清除與寫入。
        prepare_current_host_only(pb);
        new_items.is_empty() || msg_send![pb, writeObjects: &*array]
    })
}

// ─── 系統提示框 ───

/// 彈一個系統警告框（對應 Windows 的 MessageBoxW）。走 osascript 獨立程序：
/// 不佔主執行緒，Tauri 還沒起來（例如資料庫損壞、在 Builder 之前）也能彈。
/// 文字經 argv 傳給指令碼，不拼進 AppleScript 原始碼，避免引號注入。
pub fn show_alert_async(title: &str, message: &str) {
    let _ = std::process::Command::new("/usr/bin/osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "display alert (item 1 of argv) message (item 2 of argv) as warning",
            "-e",
            "end run",
            title,
            message,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

// ─── 視窗（必須在主執行緒呼叫）───

/// NSStatusWindowLevel（kCGStatusWindowLevel）。比普通「置頂」(NSFloatingWindowLevel = 3) 高，
/// 能壓過其它程式的置頂視窗，又低於選單和系統彈窗。
const OVERLAY_WINDOW_LEVEL: isize = 25;

const COLLECTION_CAN_JOIN_ALL_SPACES: usize = 1 << 0;
const COLLECTION_STATIONARY: usize = 1 << 4;
const COLLECTION_IGNORES_CYCLE: usize = 1 << 6;
const COLLECTION_FULL_SCREEN_AUXILIARY: usize = 1 << 8;

/// 把一個 Tauri 視窗調成「懸浮層」：高視窗層級、出現在所有桌面空間與全屏應用之上、
/// 不參與 ⌘` 視窗切換、App 失去啟用時也不隱藏。
///
/// # Safety
/// `ns_window` 必須是有效的 NSWindow 指標，且在主執行緒呼叫。
pub unsafe fn configure_overlay_window(ns_window: *mut c_void) {
    if ns_window.is_null() {
        return;
    }
    let window = ns_window as *mut AnyObject;
    let _: () = msg_send![window, setLevel: OVERLAY_WINDOW_LEVEL];
    let behavior: usize = COLLECTION_CAN_JOIN_ALL_SPACES
        | COLLECTION_STATIONARY
        | COLLECTION_IGNORES_CYCLE
        | COLLECTION_FULL_SCREEN_AUXILIARY;
    let _: () = msg_send![window, setCollectionBehavior: behavior];
    let _: () = msg_send![window, setHidesOnDeactivate: false];
    let _: () = msg_send![window, setCanHide: false];
}

/// 不啟用本程式、不搶鍵盤焦點地把視窗放到最前（對應 Windows 的 SWP_NOACTIVATE）。
///
/// # Safety
/// `ns_window` 必須是有效的 NSWindow 指標，且在主執行緒呼叫。
pub unsafe fn order_front_regardless(ns_window: *mut c_void) {
    if ns_window.is_null() {
        return;
    }
    let window = ns_window as *mut AnyObject;
    let _: () = msg_send![window, orderFrontRegardless];
}

/// 視窗當前是否在螢幕上。
///
/// # Safety
/// `ns_window` 必須是有效的 NSWindow 指標，且在主執行緒呼叫。
pub unsafe fn window_is_visible(ns_window: *mut c_void) -> bool {
    if ns_window.is_null() {
        return false;
    }
    msg_send![ns_window as *mut AnyObject, isVisible]
}

// ─── 顯示器 ───

/// 一塊顯示器的整屏矩形與可用區域（去掉選單欄和程式塢），都是全域性邏輯點 `(x, y, 寬, 高)`，
/// 左上角為原點、y 向下，與 CoreGraphics 和 Tauri 的顯示器座標一致。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenArea {
    pub frame: (f64, f64, f64, f64),
    pub visible: (f64, f64, f64, f64),
}

/// 列舉所有顯示器的整屏矩形與可用區域。
///
/// 執行緒：和 Tauri 自己算 `Monitor::work_area()` 一樣只讀 NSScreen 的幾何屬性，呼叫方在哪個執行緒都行。
pub fn screen_areas() -> Vec<ScreenArea> {
    autoreleasepool(|_| unsafe {
        let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
        let count = nsarray_count(screens);
        if count == 0 {
            return Vec::new();
        }
        // Cocoa 座標以主屏（第 0 個，帶選單欄的那塊）左下角為原點、y 向上，這裡翻成 y 向下
        let primary: NSRect = msg_send![nsarray_get(screens, 0), frame];
        let primary_height = primary.size.height;
        (0..count)
            .filter_map(|index| {
                let screen = nsarray_get(screens, index);
                if screen.is_null() {
                    return None;
                }
                let frame: NSRect = msg_send![screen, frame];
                let visible: NSRect = msg_send![screen, visibleFrame];
                Some(ScreenArea {
                    frame: flip_rect(frame, primary_height),
                    visible: flip_rect(visible, primary_height),
                })
            })
            .collect()
    })
}

fn flip_rect(rect: NSRect, primary_height: f64) -> (f64, f64, f64, f64) {
    (
        rect.origin.x,
        primary_height - (rect.origin.y + rect.size.height),
        rect.size.width,
        rect.size.height,
    )
}

// ─── 登入時重新開啟 ───

/// 不讓系統的「重新登入時重新開啟視窗」在登入後把 SayIt 拉起來。
///
/// 系統會記下注銷 / 關機時開著視窗的普通應用，登入後**不帶引數**地重新開啟它們。SayIt 主視窗
/// 開著時 Dock 裡有圖示，也會被記進去，於是登入後主視窗彈了出來；十幾秒後登入項
/// （LaunchAgent，帶 `--minimized`）又啟動一次。實測日誌：loginwindow 的
/// `PersistentAppsSupport persistentAppPreLaunch … bundleID:com.sayitlocally.desktop`。
///
/// 登入時要不要啟動，只由 SayIt 自己的「登入時自動啟動」決定。必須在主執行緒呼叫；
/// 每次啟動都要調（系統按程序記，不落盤）。
pub fn disable_relaunch_on_login() {
    autoreleasepool(|_| unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if !app.is_null() {
            let _: () = msg_send![app, disableRelaunchOnLogin];
        }
    });
}

// ─── WKWebView 文字輸入 ───

/// WebKit 的「智慧引號」「智慧破折號」開關在 NSUserDefaults 裡的鍵名（見 WebKit 的 TextCheckerMac.mm）。
const WEBKIT_SMART_SUBSTITUTION_KEYS: [&str; 2] = [
    "WebAutomaticQuoteSubstitutionEnabled",
    "WebAutomaticDashSubstitutionEnabled",
];

/// 關掉 WKWebView 輸入框裡的「智慧引號」和「智慧破折號」。
///
/// WebKit 在本程序裡讀 NSUserDefaults 決定這兩項；沒設定時跟隨系統「鍵盤 → 文本輸入」裡的
/// 「使用智慧引號和破折號」（預設開），於是在 Prompt、熱詞、文本替換規則裡打的 `"`、`--`
/// 會被悄悄換成 “ ” 和 —。這裡只在註冊域裡給預設值 NO：不寫偏好檔案，也不影響別的程式。
///
/// 拼寫檢查（紅色波浪線）WebKit 預設就關著，拼寫自動糾正又依賴它，所以不用管；
/// 系統的「文本替換」（使用者自己定義的縮寫）保持跟隨系統。
///
/// 必須在建立第一個 WebView 之前呼叫：WebKit 第一次用到時讀一次就快取下來。
pub fn disable_webkit_smart_substitutions() {
    autoreleasepool(|_| unsafe {
        let no: *mut AnyObject = msg_send![class!(NSNumber), numberWithBool: false];
        if no.is_null() {
            return;
        }
        let keys: Vec<Retained<NSString>> = WEBKIT_SMART_SUBSTITUTION_KEYS
            .iter()
            .map(|key| NSString::from_str(key))
            .collect();
        let key_ptrs: Vec<*mut AnyObject> =
            keys.iter().map(|key| Retained::as_ptr(key) as *mut AnyObject).collect();
        let values: Vec<*mut AnyObject> = vec![no; key_ptrs.len()];
        let dict: *mut AnyObject = msg_send![
            class!(NSDictionary),
            dictionaryWithObjects: values.as_ptr(),
            forKeys: key_ptrs.as_ptr(),
            count: key_ptrs.len()
        ];
        let defaults: *mut AnyObject = msg_send![class!(NSUserDefaults), standardUserDefaults];
        if dict.is_null() || defaults.is_null() {
            return;
        }
        let _: () = msg_send![defaults, registerDefaults: dict];
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webkit_smart_substitutions_default_to_off() {
        disable_webkit_smart_substitutions();
        autoreleasepool(|_| unsafe {
            let defaults: *mut AnyObject = msg_send![class!(NSUserDefaults), standardUserDefaults];
            for key in WEBKIT_SMART_SUBSTITUTION_KEYS {
                let name = NSString::from_str(key);
                // WebKit 先看有沒有值（沒有就跟隨系統），再按 bool 讀
                let value: *mut AnyObject = msg_send![defaults, objectForKey: &*name];
                assert!(!value.is_null(), "{key} should have a registered default");
                let enabled: bool = msg_send![defaults, boolForKey: &*name];
                assert!(!enabled, "{key} should default to NO");
            }
        });
    }

    #[test]
    fn frontmost_app_is_queryable() {
        // 在有圖形會話的機器上總能拿到一個前台應用；CI 無頭環境裡可能是 None，不算失敗。
        if let Some(app) = frontmost_app() {
            assert!(app.pid > 0);
        }
    }

    #[test]
    fn screen_areas_put_visible_frame_inside_screen() {
        // 無頭環境可能一塊屏都沒有，不算失敗
        for area in screen_areas() {
            let (fx, fy, fw, fh) = area.frame;
            let (vx, vy, vw, vh) = area.visible;
            assert!(vx >= fx - 0.5 && vy >= fy - 0.5, "{area:?}");
            assert!(vx + vw <= fx + fw + 0.5 && vy + vh <= fy + fh + 0.5, "{area:?}");
        }
    }
}
