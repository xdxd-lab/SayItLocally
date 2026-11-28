//! macOS 前台應用與焦點元素採集（對應 Windows 的 GetGUIThreadInfo + UI Automation）。
//!
//! 字段沿用 Windows 的 AppContext，含义按 mac 填：
//! - `hwnd`：前台應用的 pid（不透明 token，前端只會原樣傳回 paste_text）
//! - `processName`：執行檔名；`exePath`：執行檔路徑；`bundleId`：Bundle ID
//! - `windowClass`：Bundle ID（mac 沒有視窗類，借這個欄位讓日誌/診斷能看到是哪個應用）
//! - `controlType` / `focusClass` / `automationId`：AXRole / AXSubrole / AXIdentifier
//! - `hasCaret`：焦點元素有沒有 AXSelectedTextRange

use super::{sanitize_uia_text, tail_chars, text_diag, AppContext, MonitorBounds, TextContext};
use crate::platform::macos::{appkit, focus};
use crate::platform::macos::ax::{self, AxElement};
use crate::platform::macos::ffi::CFRange;

const TEXT_BEFORE_LIMIT: usize = 500;
const TEXT_AFTER_LIMIT: usize = 300;
const SELECTED_TEXT_LIMIT: usize = 6000;
/// 退回讀整個 AXValue 時的上限（UTF-16 單元）。超大的文件不讀，免得把編輯器卡住。
const VALUE_FALLBACK_LIMIT: usize = 200_000;

pub fn capture_context_with_text(reason: &str, include_text_context: bool) -> AppContext {
    capture_context(reason, include_text_context, None)
}

pub(super) fn capture_recording_with_focus(include_text_context: bool) -> (AppContext, focus::Snapshot<AxElement>) {
    let mut snapshot = focus::Snapshot::default();
    let context = capture_context("recording_start", include_text_context, Some(&mut snapshot));
    (context, snapshot)
}

fn capture_context(
    reason: &str,
    include_text_context: bool,
    mut snapshot: Option<&mut focus::Snapshot<AxElement>>,
) -> AppContext {
    let mut ctx = AppContext {
        reason: reason.to_string(),
        timestamp: chrono::Utc::now().timestamp_millis(),
        is_enabled: true,
        ..Default::default()
    };
    let started = std::time::Instant::now();
    // 請求了正文卻在中途返回時，記下停在哪一步（原因碼見 TextContextDiag）
    let give_up = |ctx: &mut AppContext, result: &str| {
        if include_text_context {
            ctx.text_context_diag = Some(text_diag(result, None, started));
        }
    };

    let Some(app) = appkit::frontmost_app() else {
        give_up(&mut ctx, "no_foreground");
        return ctx;
    };
    if let Some(target) = &mut snapshot {
        target.pid = (app.pid > 0).then_some(app.pid);
    }
    ctx.hwnd = app.pid.to_string();
    ctx.pid = app.pid.max(0) as u32;
    ctx.process_name = app.executable_name();
    ctx.exe_path = app.executable_path.clone();
    ctx.window_class = app.bundle_id.clone();
    ctx.bundle_id = app.bundle_id.clone();
    if ctx.process_name.is_empty() {
        ctx.process_name = app.name.clone();
    }

    // [自訂] L-80：自身輸入框走 PID 判斷，避免跨執行緒向自己的 AX 伺服器送查詢。
    if app.pid == std::process::id() as i32 {
        give_up(&mut ctx, "own_process");
        return ctx;
    }

    // 沒有輔助功能授權時只能拿到應用級資訊；可編輯判據會退回 bundle 白名單。
    if !ax::is_trusted() {
        give_up(&mut ctx, "no_permission");
        return ctx;
    }
    let Some(app_element) = AxElement::application(app.pid) else {
        give_up(&mut ctx, "uia_unavailable");
        return ctx;
    };
    app_element.set_timeout(ax::DEFAULT_TIMEOUT_SECS);

    // Electron 應用預設不建無障礙樹，只有真要讀正文時才開啟（會讓對方多耗一點 CPU/記憶體）。
    if include_text_context {
        let _ = app_element.set_bool("AXManualAccessibility", true);
    }

    // [自訂] L-80：錄音開始沿用原有的查詢順序與 DEFAULT_TIMEOUT_SECS。
    let window = app_element.attr_element("AXFocusedWindow");
    if let Some(window) = &window {
        window.set_timeout(ax::DEFAULT_TIMEOUT_SECS);
        ctx.window_title = window.attr_string("AXTitle").unwrap_or_default();
    }
    if let Some(target) = &mut snapshot {
        target.window = window;
    }

    let Some(focused) = app_element.attr_element("AXFocusedUIElement") else {
        give_up(&mut ctx, "no_focused_element");
        return ctx;
    };
    focused.set_timeout(ax::DEFAULT_TIMEOUT_SECS);
    ctx.focus_hwnd = ctx.hwnd.clone();
    ctx.control_type = focused.attr_string("AXRole").unwrap_or_default();
    ctx.focus_class = focused.attr_string("AXSubrole").unwrap_or_default();
    ctx.automation_id = focused.attr_string("AXIdentifier").unwrap_or_default();
    ctx.is_enabled = focused.attr_bool("AXEnabled").unwrap_or(true);
    ctx.is_keyboard_focusable = focused.attr_bool("AXFocused").unwrap_or(false);
    ctx.is_password = ctx.focus_class == "AXSecureTextField";

    let selection = focused.attr_range("AXSelectedTextRange");
    ctx.has_caret = selection.is_some();
    ctx.is_text_pattern_available = selection.is_some();
    let value_settable = focused.is_settable("AXValue");
    ctx.is_value_pattern_available = value_settable.is_some();
    ctx.is_read_only = value_settable.map(|settable| !settable);

    // 密碼框是硬邊界：即使開了上下文感知也絕不讀取。
    if include_text_context {
        if ctx.is_password {
            give_up(&mut ctx, "password");
        } else if !ctx.is_enabled {
            give_up(&mut ctx, "element_disabled");
        } else {
            let text_started = std::time::Instant::now();
            let outcome = match selection {
                Some(range) => capture_text_context(&focused, range),
                None => Err("no_caret"),
            };
            let (text, result, depth) = match outcome {
                Ok(context) => (Some(context), "captured", Some(0)),
                Err(reason) => (None, reason, None),
            };
            ctx.text_context = text;
            ctx.text_context_diag = Some(text_diag(result, depth, text_started));
        }
    }
    // Chrome／Electron 的首個焦點回應可能只是容器。只有初始文字元件可用於比較。
    set_snapshot_element(snapshot, focused, &ctx.control_type, ctx.is_password, selection.is_some());
    ctx
}

// [自訂] L-80：初始容器不提供可信的編輯目標身分。
fn set_snapshot_element<T>(
    snapshot: Option<&mut focus::Snapshot<T>>,
    element: T,
    role: &str,
    is_password: bool,
    has_range: bool,
) {
    if let Some(target) = snapshot {
        target.element = (ax::is_text_role(role) || is_password || has_range).then_some(element);
    }
}

fn utf16_slice(units: &[u16], start: usize, end: usize) -> String {
    let start = start.min(units.len());
    let end = end.clamp(start, units.len());
    String::from_utf16_lossy(&units[start..end])
}

/// 游標前 500 字、選區（超過 6000 字不發）、游標後 300 字。
/// 優先用 AXStringForRange 只取需要的那一小段；不支援的控制元件退回讀 AXValue 再切。
/// 讀不到時返回 TextContextDiag 的原因碼（no_caret / empty）。
fn capture_text_context(element: &AxElement, selection: CFRange) -> Result<TextContext, &'static str> {
    if selection.location < 0 || selection.length < 0 {
        return Err("no_caret");
    }
    let location = selection.location as usize;
    let length = selection.length as usize;
    let total = element.attr_i64("AXNumberOfCharacters").map(|n| n.max(0) as usize);

    // AX 的區間以 UTF-16 為單位；限額按字元數近似，前後各多取一點再按字元截。
    let before_start = location.saturating_sub(TEXT_BEFORE_LIMIT);
    let after_start = location + length;
    let after_len = match total {
        Some(total) => total.saturating_sub(after_start).min(TEXT_AFTER_LIMIT),
        None => TEXT_AFTER_LIMIT,
    };

    let mut text_before = element
        .string_for_range(CFRange { location: before_start as isize, length: (location - before_start) as isize })
        .map(|s| sanitize_uia_text(&s));
    let mut text_after = if after_len > 0 {
        element
            .string_for_range(CFRange { location: after_start as isize, length: after_len as isize })
            .map(|s| sanitize_uia_text(&s))
    } else {
        Some(String::new())
    };

    let mut selection_truncated = false;
    let mut selected_text = String::new();
    if length > SELECTED_TEXT_LIMIT {
        selection_truncated = true;
    } else if length > 0 {
        let raw = element
            .attr_string("AXSelectedText")
            .or_else(|| element.string_for_range(selection))
            .map(|s| sanitize_uia_text(&s))
            .unwrap_or_default();
        if raw.chars().count() > SELECTED_TEXT_LIMIT {
            selection_truncated = true;
        } else if !raw.trim().is_empty() {
            selected_text = raw;
        }
    }

    // 控制元件不支援按區間取字：退回讀整個值（太大的不讀）
    if text_before.is_none() || text_after.is_none() {
        if total.map(|t| t <= VALUE_FALLBACK_LIMIT).unwrap_or(true) {
            if let Some(value) = element.attr_string("AXValue") {
                let units: Vec<u16> = value.encode_utf16().collect();
                if units.len() <= VALUE_FALLBACK_LIMIT {
                    if text_before.is_none() {
                        text_before = Some(sanitize_uia_text(&utf16_slice(&units, before_start, location)));
                    }
                    if text_after.is_none() {
                        text_after = Some(sanitize_uia_text(&utf16_slice(&units, after_start, after_start + TEXT_AFTER_LIMIT)));
                    }
                }
            }
        }
    }

    let text_before = tail_chars(&text_before.unwrap_or_default(), TEXT_BEFORE_LIMIT);
    let text_after: String = text_after.unwrap_or_default().chars().take(TEXT_AFTER_LIMIT).collect();
    if text_before.is_empty() && selected_text.is_empty() && text_after.is_empty() && !selection_truncated {
        return Err("empty");
    }
    Ok(TextContext {
        source: "ax_selected_text_range".to_string(),
        text_before,
        selected_text,
        text_after,
        selection_truncated,
    })
}

/// 前台應用焦點視窗的位置（**邏輯點**，左上角為原點的全域性座標）。
///
/// 與 Windows 不同，這裡返回的不是顯示器工作區，而是視窗矩形本身：mac 上各顯示器縮放
/// 可以不同，物理畫素座標不能跨屏比較，懸浮窗定位改在 window 模組裡按邏輯座標找顯示器、
/// 取它的工作區（見 window::macos_overlay_bounds）。
pub fn capture_foreground_monitor() -> Option<MonitorBounds> {
    if !ax::is_trusted() {
        return None;
    }
    let app = appkit::frontmost_app()?;
    let app_element = AxElement::application(app.pid)?;
    app_element.set_timeout(ax::DEFAULT_TIMEOUT_SECS);
    let window = app_element.attr_element("AXFocusedWindow")?;
    window.set_timeout(ax::DEFAULT_TIMEOUT_SECS);
    let position = window.attr_point("AXPosition")?;
    let size = window.attr_size("AXSize")?;
    if size.width <= 0.0 || size.height <= 0.0 {
        return None;
    }
    Some(MonitorBounds {
        left: position.x.round() as i32,
        top: position.y.round() as i32,
        right: (position.x + size.width).round() as i32,
        bottom: (position.y + size.height).round() as i32,
        source: "ax_focused_window".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_slice_is_bounds_safe() {
        let units: Vec<u16> = "你好世界".encode_utf16().collect();
        assert_eq!(utf16_slice(&units, 1, 3), "好世");
        assert_eq!(utf16_slice(&units, 3, 99), "界");
        assert_eq!(utf16_slice(&units, 9, 2), "");
    }

    #[test]
    fn capture_never_panics_without_permission() {
        let ctx = capture_context_with_text("test", false);
        assert_eq!(ctx.reason, "test");
    }
}


#[cfg(test)]
mod focus_target_tests {
    use super::{focus::Snapshot, set_snapshot_element};

    #[test]
    fn only_initial_editable_elements_are_retained_for_focus_comparison() {
        let mut snapshot = Snapshot { pid: Some(42), window: Some(7), element: None };
        set_snapshot_element(Some(&mut snapshot), 1, "AXTextField", false, false);
        assert_eq!(snapshot.element, Some(1));
        set_snapshot_element(Some(&mut snapshot), 2, "AXGroup", false, true);
        assert_eq!(snapshot.element, Some(2));
        set_snapshot_element(Some(&mut snapshot), 3, "AXUnknown", true, false);
        assert_eq!(snapshot.element, Some(3));
        set_snapshot_element(Some(&mut snapshot), 4, "AXGroup", false, false);
        assert_eq!(snapshot.element, None);
        set_snapshot_element(Some(&mut snapshot), 5, "", false, false);
        assert_eq!(snapshot.element, None);
        set_snapshot_element(None, 6, "AXTextField", false, false);
        assert_eq!(snapshot.pid, Some(42));
        assert_eq!(snapshot.window, Some(7));
    }
}
