//! Text injection — writes text into the target application.
//!
//! Strategy priority:
//! 1. clipboard + WM_PASTE message (sent directly to target hwnd — works cross-process)
//! 2. clipboard + SendInput Ctrl+V (fallback for apps that don't handle WM_PASTE)
//!
//! Two entry points:
//! - `inject_text_to_hwnd(text, hwnd, focus_hwnd)` — uses pre-probed hwnd (preferred)
//! - `inject_text(text)` — re-captures context (legacy fallback)

use serde::Serialize;

#[cfg(windows)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(windows)]
use std::time::Instant;

/// 每次剪貼簿貼上遞增。舊恢復執行緒看到新一代貼上後會放棄，避免覆蓋新內容。
#[cfg(windows)]
static PASTE_GENERATION: AtomicU64 = AtomicU64::new(0);

#[cfg(windows)]
struct ClipboardRestoreGuard {
    enabled: bool,
    previous_text: Option<String>,
    /// 插入前剪貼簿的全部格式。None = 沒拍成（超預算 / 打不開），退回只還原文字。
    previous_snapshot: Option<ClipboardSnapshot>,
    injected_text: String,
    paste_id: u64,
}

#[cfg(windows)]
impl ClipboardRestoreGuard {
    fn new(
        enabled: bool,
        previous_text: Option<String>,
        previous_snapshot: Option<ClipboardSnapshot>,
        injected_text: &str,
        paste_id: u64,
    ) -> Self {
        Self {
            enabled,
            previous_text,
            previous_snapshot,
            injected_text: injected_text.to_owned(),
            paste_id,
        }
    }
}

/// 插入前剪貼簿裡的全部內容：每種能按位元組複製的格式各一份。
///
/// 原來只存文字（CF_UNICODETEXT），使用者剛複製的截圖、檔案、帶格式的文本在口述一次之後
/// 就沒了，或者被降級成純文本。
#[cfg(windows)]
#[derive(Default)]
struct ClipboardSnapshot {
    formats: Vec<(u32, Vec<u8>)>,
}

/// 拍快照的預算：超了就放棄，退回只還原文字。
///
/// 有些程式是延遲渲染（Excel 複製一大片區域時，讀取那一刻才現場生成幾十種格式），
/// 讀起來可能很慢，而這一步在插字的關鍵路徑上，不能為了還原剪貼簿把插字拖慢。
#[cfg(windows)]
const CLIPBOARD_SNAPSHOT_BUDGET_MS: u128 = 150;
#[cfg(windows)]
const CLIPBOARD_SNAPSHOT_MAX_BYTES: usize = 64 * 1024 * 1024;

/// 這些格式的控制代碼不是 HGLOBAL（點陣圖、調色盤、元檔案、自繪、私有/GDI 物件區間），不能按位元組複製。
/// 圖片靠 CF_DIB / CF_DIBV5 儲存，系統會從它們合成 CF_BITMAP。
#[cfg(windows)]
fn clipboard_format_is_copyable(format: u32) -> bool {
    const CF_BITMAP: u32 = 2;
    const CF_METAFILEPICT: u32 = 3;
    const CF_PALETTE: u32 = 9;
    const CF_ENHMETAFILE: u32 = 14;
    !matches!(format, CF_BITMAP | CF_METAFILEPICT | CF_PALETTE | CF_ENHMETAFILE)
        && !(0x0080..=0x008F).contains(&format) // CF_OWNERDISPLAY / CF_DSP*
        && !(0x0200..=0x03FF).contains(&format) // CF_PRIVATEFIRST..CF_GDIOBJLAST
}

#[cfg(windows)]
impl Drop for ClipboardRestoreGuard {
    fn drop(&mut self) {
        if !self.enabled {
            return;
        }

        let previous_text = self.previous_text.take();
        let previous_snapshot = self.previous_snapshot.take();
        let injected_text = std::mem::take(&mut self.injected_text);
        let paste_id = self.paste_id;
        let scheduled_at = Instant::now();

        // Drop 發生在 WM_PASTE/SendInput 已執行之後，因此 400ms 從實際貼上觸發時開始計算。
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(400));
            let elapsed_ms = scheduled_at.elapsed().as_millis();
            let current_generation = PASTE_GENERATION.load(Ordering::Acquire);
            if current_generation != paste_id {
                crate::commands::system::write_log_line(&format!(
                    "[RUST] [inject] clipboard restore skipped pasteId={} reason=superseded currentGeneration={} elapsedMs={}",
                    paste_id, current_generation, elapsed_ms
                ));
                return;
            }

            unsafe {
                // 使用者或目標程式若已改寫剪貼簿，不再用舊內容覆蓋它。
                let current_text = native_get_clipboard_text();
                if current_text.as_deref() != Some(injected_text.as_str()) {
                    crate::commands::system::write_log_line(&format!(
                        "[RUST] [inject] clipboard restore skipped pasteId={} reason=clipboard_changed currentUtf16Len={} elapsedMs={}",
                        paste_id,
                        current_text.as_deref().map(|value| value.encode_utf16().count()).unwrap_or(0),
                        elapsed_ms
                    ));
                    return;
                }

                let (restored, mode, format_count) = match &previous_snapshot {
                    Some(snapshot) if !snapshot.formats.is_empty() => {
                        (restore_clipboard_snapshot(snapshot, 3, 20), "all_formats", snapshot.formats.len())
                    }
                    Some(_) => (native_clear_clipboard(), "was_empty", 0),
                    None => match &previous_text {
                        Some(text) => (set_clipboard_with_retry(text, 3, 20), "text_only", 1),
                        None => (native_clear_clipboard(), "text_only_empty", 0),
                    },
                };
                crate::commands::system::write_log_line(&format!(
                    "[RUST] [inject] clipboard restore finished pasteId={} restored={} mode={} formats={} previousUtf16Len={} elapsedMs={}",
                    paste_id,
                    restored,
                    mode,
                    format_count,
                    previous_text.as_deref().map(|value| value.encode_utf16().count()).unwrap_or(0),
                    elapsed_ms
                ));
            }
        });
    }
}

#[cfg(windows)]
use windows::Win32::Foundation::HWND;
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
    SendMessageTimeoutW, SMTO_ABORTIFHUNG,
};
#[cfg(windows)]
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, KEYBD_EVENT_FLAGS,
};
#[cfg(windows)]
use windows::Win32::System::Threading::GetCurrentThreadId;
#[cfg(windows)]
use windows::Win32::System::DataExchange::{
    OpenClipboard, CloseClipboard, EmptyClipboard, SetClipboardData, GetClipboardData,
    GetOpenClipboardWindow, GetClipboardSequenceNumber,
};
#[cfg(windows)]
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
#[cfg(windows)]
use windows::Win32::Foundation::HANDLE;

use crate::context;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{cached_keycode_for_v, copy_to_clipboard, inject_recording_text, keycode_for_v};

#[derive(Debug, Clone, Serialize, Default)]
pub struct InjectResult {
    pub ok: bool,
    pub strategy: Option<String>,
    pub reason: Option<String>,
    pub detail: Option<String>,
    /// True when SendInput was used on a Chromium-class window without caret —
    /// we can't verify if paste actually landed in an input field.
    #[serde(default)]
    pub uncertain: bool,
}

#[cfg(windows)]
pub fn inject_text_to_hwnd(text: &str, target_hwnd_val: isize, focus_hwnd_val: isize, restore_clipboard: bool) -> InjectResult {
    let target = HWND(target_hwnd_val as *mut _);
    let focus = if focus_hwnd_val != 0 {
        HWND(focus_hwnd_val as *mut _)
    } else {
        target
    };

    unsafe { do_inject(target, focus, text, restore_clipboard) }
}

#[cfg(windows)]
pub fn inject_text(text: &str, restore_clipboard: bool) -> InjectResult {
    let ctx = context::capture_context("inject");

    if ctx.hwnd.is_empty() || ctx.hwnd == "0" {
        // 這個分支此前一行日誌都不寫，於是「插不進去」的排查裡它是個盲區：使用者看到
        // 兜底卡片，日誌裡卻只有前端那條 reason=no_foreground_window，說不清是真的
        // 沒有前台視窗，還是捕獲本身失敗了。
        crate::commands::system::write_log_line(&format!(
            "[RUST] [inject] no foreground window hwnd={:?} focusHwnd={:?} process={:?}",
            ctx.hwnd, ctx.focus_hwnd, ctx.process_name
        ));
        return InjectResult {
            ok: false, strategy: None,
            reason: Some("no_foreground_window".to_string()), detail: None,
            uncertain: false,
        };
    }

    let gate = editability_gate(&ctx);
    if !gate.is_editable() {
        return InjectResult {
            ok: false,
            strategy: Some("overlay_fallback".to_string()),
            reason: Some("not_editable".to_string()),
            // gate 說明結論是哪一層判據給出的，後面四項是那四層各自的輸入。缺了它們，
            // 日誌只能說明"被攔了"，說不出是哪一層落空，排查得另跑一遍探測指令碼
            // （2026-09 查微信 4.1 改名時吃過這個虧）。
            detail: Some(describe_editability(&ctx, gate)),
            uncertain: false,
        };
    }

    let target_hwnd = ctx.hwnd.parse::<isize>().unwrap_or(0);
    let focus_hwnd = ctx.focus_hwnd.parse::<isize>().unwrap_or(0);
    inject_text_to_hwnd(text, target_hwnd, focus_hwnd, restore_clipboard)
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn inject_text_to_hwnd(_text: &str, _target: isize, _focus: isize, _restore_clipboard: bool) -> InjectResult {
    InjectResult { ok: false, strategy: None, reason: Some("not_windows".to_string()), detail: None, uncertain: false }
}
#[cfg(not(any(windows, target_os = "macos")))]
pub fn inject_text(_text: &str, _restore_clipboard: bool) -> InjectResult {
    InjectResult { ok: false, strategy: None, reason: Some("not_windows".to_string()), detail: None, uncertain: false }
}

/// 「這裡能不能打字」的結論，外加**是哪一層判據給出的**。
///
/// 為什麼要帶上「哪一層」而不只是 bool：判據一共四層（caret / 原生控制元件類名 / UIA /
/// 程序名白名單），任何一層都會因為第三方應用換 UI 框架、改程序名而失效，而失效的
/// 表現完全一樣 —— 插字退化成兜底卡片，不報錯。只有 bool 的話日誌只能說明「被攔
/// 了」，說不出是哪一層落空，排查得另跑一遍 `dev-scripts/probe-foreground-target.ps1`
/// 才能定性（2026-09 微信 4.1 把 WeChat.exe 改名成 Weixin.exe 那次就是這麼查的）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditableGate {
    /// explorer 的桌面圖示列表：明確拒絕，優先順序高於 caret
    ExplorerDesktopReject,
    /// 系統報告了文本游標，最可靠的訊號
    Caret,
    /// 焦點或視窗類名命中原生可編輯控制元件（Edit / RichEdit / Scintilla / Word）
    NativeClass,
    /// UIA 報了 Edit / Document / ComboBox
    UiaEditableControl,
    /// UIA 報了 Custom/Group/Pane 且有 ValuePattern，即富文本編輯器
    UiaRichEditor,
    /// UIA 說這個可編輯控制元件是隻讀的
    UiaReadOnly,
    /// Chromium 視窗且能獲得鍵盤焦點，樂觀放行
    ChromiumOptimistic,
    /// 前面幾層都沒訊號，靠程序名白名單放行
    ProcessAllowlist,
    /// 四層判據全部落空
    NoSignal,
    /// macOS：輔助功能沒給出否定訊號（拿不到焦點元素，或焦點不是按鈕/選單這類控制元件），樂觀放行
    #[cfg(target_os = "macos")]
    AxOptimistic,
}

impl EditableGate {
    pub fn is_editable(self) -> bool {
        !matches!(
            self,
            Self::ExplorerDesktopReject | Self::UiaReadOnly | Self::NoSignal
        )
    }

    /// 寫進日誌和探測結果的穩定標識。改名等於改日誌契約，排查指令碼會對不上。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExplorerDesktopReject => "explorer_desktop_reject",
            Self::Caret => "caret",
            Self::NativeClass => "native_class",
            Self::UiaEditableControl => "uia_editable_control",
            Self::UiaRichEditor => "uia_rich_editor",
            Self::UiaReadOnly => "uia_read_only",
            Self::ChromiumOptimistic => "chromium_optimistic",
            Self::ProcessAllowlist => "process_allowlist",
            Self::NoSignal => "no_signal",
            #[cfg(target_os = "macos")]
            Self::AxOptimistic => "ax_optimistic",
        }
    }
}

/// `editability_gate` 的 bool 檢視，只給測試用。
///
/// 加 `#[cfg(test)]` 是因為生產程式碼一處都不調它（真正的呼叫方要的是 gate 本身，
/// 好把"是哪一層放行的"寫進日誌），於是 release 構建每次都報一條 dead_code 警告。
/// 不刪掉它是因為測試確實要斷言"最終結論是可編輯"這一面 —— 見下面註釋說的漂移風險。
#[cfg(test)]
pub fn is_likely_editable_pub(ctx: &context::AppContext) -> bool {
    windows_editability_gate(ctx).is_editable()
}

/// 判據只有這一份實現。別再複製一份出來只為了拿一個 bool 結論 —— 兩份判據遲早漂移，
/// 而漂移的症狀是「改了一處，另一條路徑照舊」。需要 bool 就 `.is_editable()`。
pub fn editability_gate(ctx: &context::AppContext) -> EditableGate {
    #[cfg(target_os = "macos")]
    {
        macos::editability_gate(ctx)
    }
    #[cfg(not(target_os = "macos"))]
    {
        windows_editability_gate(ctx)
    }
}

/// Windows 的四層判據（caret / 原生控制元件類名 / UIA / 程序名白名單）。
/// 純邏輯、不調系統 API，所以在 mac 上也編譯，單測照樣跑。
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn windows_editability_gate(ctx: &context::AppContext) -> EditableGate {
    let fc = ctx.focus_class.to_lowercase();
    let wc = ctx.window_class.to_lowercase();
    let proc = ctx.process_name.to_lowercase();

    // Windows 桌面也屬於 explorer.exe，但 Progman / WorkerW 下的圖示列表不是文本
    // 輸入目標。必須在 caret/程序兜底之前明確拒絕，否則 SendInput 只會報告“按鍵已
    // 入隊”，前端會誤以為貼上成功而不展示兜底卡片。
    let explorer_desktop_classes = ["progman", "workerw", "shelldll_defview", "syslistview32"];
    if proc.contains("explorer")
        && explorer_desktop_classes
            .iter()
            .any(|class_name| fc == *class_name || wc == *class_name)
    {
        return EditableGate::ExplorerDesktopReject;
    }

    if ctx.has_caret { return EditableGate::Caret; }

    // Native Win32 editable controls — always considered editable
    let native_editable_classes = [
        "edit", "richedit", "richedit20w", "richedit50w",
        "scintilla", "texteditorsid",
        // Office Word editor control (used by Outlook, Word, etc.)
        "_wwg",
    ];
    for cls in &native_editable_classes {
        if fc.contains(cls) || wc.contains(cls) { return EditableGate::NativeClass; }
    }

    // UIA-based detection: if control_type is populated, use it as primary signal.
    // This works for both Chromium and native windows.
    if !ctx.control_type.is_empty() {
        let ct = ctx.control_type.as_str();

        // Definitely editable control types
        let is_editable_control = ct == "Edit" || ct == "Document" || ct == "ComboBox";

        // Custom/Group/Pane with ValuePattern (rich text editors like CodeMirror,
        // Notion, Feishu docs, etc.)
        let has_value = ctx.is_value_pattern_available;
        let is_rich_editor = (ct == "Custom" || ct == "Group" || ct == "Pane") && has_value;

        if is_editable_control || is_rich_editor {
            if ctx.is_enabled {
                if ctx.is_read_only == Some(true) {
                    return EditableGate::UiaReadOnly;
                }
                return if is_editable_control {
                    EditableGate::UiaEditableControl
                } else {
                    EditableGate::UiaRichEditor
                };
            }
        }

        // For Chromium windows: if UIA says it's a keyboard-focusable Group/Pane
        // (even without ValuePattern), be optimistic — many web editors
        // (Feishu, Notion, Slack) use contenteditable divs that expose as
        // Group without ValuePattern. SendInput Ctrl+V is harmless if wrong.
        let is_chromium_class = fc.contains("chrome_widgetwin_1")
            || fc.contains("chrome_renderwidgethostview")
            || wc.contains("chrome_widgetwin_1")
            || fc.contains("intermediate d3d window");

        if is_chromium_class && ctx.is_keyboard_focusable && ctx.is_enabled {
            // Optimistic: keyboard-focusable element in Chromium is likely an
            // input area. Only reject known non-editable types.
            let definitely_not_editable = ct == "Button"
                || ct == "MenuItem"
                || ct == "MenuBar"
                || ct == "Menu"
                || ct == "Tab"
                || ct == "TabItem"
                || ct == "ToolBar"
                || ct == "TitleBar"
                || ct == "ScrollBar"
                || ct == "Image"
                || ct == "Hyperlink"
                || ct == "StatusBar"
                || ct == "Header"
                || ct == "HeaderItem"
                || ct == "Separator"
                || ct == "ProgressBar";
            if !definitely_not_editable {
                return EditableGate::ChromiumOptimistic;
            }
        }

        // For non-Chromium windows with UIA data: if it's not an editable type,
        // fall through to process-based heuristic (don't hard-reject).
    }

    let editable_procs = [
        "notepad", "winword", "excel", "powerpnt", "outlook",
        "code", "devenv", "idea64",
        // VS Code 派生的 AI IDE：焦點常報為 Pane 且無 ValuePattern，
        // 與 code/devenv 同類，文本區可編輯，按程序名兜底放行
        "trae", "cursor", "windsurf", "kiro",
        "chrome", "msedge", "firefox", "opera", "brave",
        // 微信自 4.x 起整個介面用 Qt 重寫（視窗類 Qt51514QWindowIcon，輸入框自繪）：
        // 沒有 Win32 caret、沒有子控制元件 HWND（focusClass 就等於視窗類）、UIA 只能看到
        // 頂層 Window 且無 ValuePattern —— 上面三層判據對它全部為空，程序名是唯一
        // 的放行依据。而 4.1 又把 exe 从 WeChat.exe 改名成 Weixin.exe（装到
        // Tencent\Weixin\），只寫 "wechat" 的話使用者升級後就靜默退化成兜底卡片
        // （2026-09-20 實測 4.1.13.65）。兩個名字都要留：3.9 老版本和微信開發者
        // 工具（wechatdevtools.exe）仍然叫 wechat。
        "teams", "wechat", "weixin", "dingtalk", "slack",
        "windowsterminal", "cmd", "powershell",
        "mobaxterm", "putty", "securecrt", "xshell",
    ];
    for p in &editable_procs {
        if proc.contains(p) { return EditableGate::ProcessAllowlist; }
    }
    EditableGate::NoSignal
}

/// 把結論和四層判據各自的輸入拼成一行，日誌與探測結果共用同一份格式。
///
/// 刻意不含視窗標題和 exe 全路徑：這行會進 sayit.log，而日誌整份跟著診斷包發給
/// 開發者。類名、控制元件型別、程序名足夠定位「為什麼判成不可輸入」，標題裡的聊天
/// 物件名和路徑裡的 Windows 使用者名稱對排查沒有任何作用。
pub fn describe_editability(ctx: &context::AppContext, gate: EditableGate) -> String {
    format!(
        "gate={} editable={} class={} focusClass={} hasCaret={} controlType={} valuePattern={} kbFocusable={} enabled={} readOnly={} process={}",
        gate.as_str(),
        gate.is_editable(),
        ctx.window_class,
        ctx.focus_class,
        ctx.has_caret,
        if ctx.control_type.is_empty() { "-" } else { ctx.control_type.as_str() },
        ctx.is_value_pattern_available,
        ctx.is_keyboard_focusable,
        ctx.is_enabled,
        ctx.is_read_only.map_or("-", |v| if v { "true" } else { "false" }),
        ctx.process_name
    )
}

// ─── Core injection logic ───

/// WM_PASTE = 0x0302
#[cfg(windows)]
const WM_PASTE: u32 = 0x0302;

/// WM_COMMAND = 0x0111
#[cfg(windows)]
const WM_COMMAND: u32 = 0x0111;

/// conhost 系統選單「貼上」命令 ID（內建於 conhost）。
/// 參考：0xFFF0=複製 0xFFF1=貼上 0xFFF2=滾動 0xFFF3=標記 0xFFF5=全選
#[cfg(windows)]
const ID_CONSOLE_PASTE: usize = 0xFFF1;

/// Main injection: try WM_PASTE first, then SendInput Ctrl+V as fallback.
///
/// `restore_clipboard`: 若為 true，在寫入文本前先儲存剪貼簿原有文本內容，
/// 待貼上指令發出、目標程式有機會讀取剪貼簿之後（延遲一小段時間，在獨立執行緒裡），
/// 再把剪貼簿還原為原內容，避免使用者之前複製的東西被覆蓋丟失。
#[cfg(windows)]
unsafe fn do_inject(target: HWND, focus: HWND, text: &str, restore_clipboard: bool) -> InjectResult {
    let paste_id = PASTE_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;

    // 儲存原剪貼簿內容（在寫入新文本之前）。剪貼簿本來為空/非文本時為 None，
    // 還原時會清空剪貼簿而不是留著我們剛插入的文本。
    let previous_clipboard_text = if restore_clipboard {
        native_get_clipboard_text()
    } else {
        None
    };
    let previous_clipboard_snapshot = if restore_clipboard {
        snapshot_clipboard(paste_id)
    } else {
        None
    };

    // Step 1: Write text to clipboard
    let clipboard_ok = set_clipboard_with_retry(text, 5, 30);
    if !clipboard_ok {
        // 連 5 次都寫不進剪貼簿，基本就是被安全軟體的剪貼簿保護擋著。
        // 把命中的軟體名寫進日誌和 detail，使用者才知道該去哪兒加信任。
        let guards = detect_input_guard_software();
        crate::commands::system::write_log_line(&format!(
            "[RUST] [inject] clipboard write failed pasteId={} utf8Len={} utf16Len={} inputGuards={} {}",
            paste_id,
            text.len(),
            text.encode_utf16().count(),
            guards,
            describe_clipboard_holder()
        ));
        return InjectResult {
            ok: false,
            strategy: Some("clipboard".to_string()),
            reason: Some("clipboard_blocked".to_string()),
            detail: Some(format!(
                "pasteId={} failed to write to the clipboard after 5 attempts inputGuards={}",
                paste_id, guards
            )),
            uncertain: false,
        };
    }

    let clipboard_matches = native_get_clipboard_text().as_deref() == Some(text);
    crate::commands::system::write_log_line(&format!(
        "[RUST] [inject] clipboard write ok pasteId={} utf8Len={} utf16Len={} clipboardMatches={} restoreRequested={}",
        paste_id,
        text.len(),
        text.encode_utf16().count(),
        clipboard_matches,
        restore_clipboard
    ));

    // 守衛在函式返回時才啟動恢復計時，確保恢復延遲從實際貼上觸發之後開始。
    // generation 與剪貼簿內容複核可阻止舊執行緒覆蓋後續貼上或使用者新複製的內容。
    let _clipboard_restore_guard = ClipboardRestoreGuard::new(
        restore_clipboard,
        previous_clipboard_text,
        previous_clipboard_snapshot,
        text,
        paste_id,
    );

    // Step 1.5: 经典控制台窗口（conhost，类名 ConsoleWindowClass）——用控制台
    // 宿主自帶的「貼上」命令，而不是模擬 Ctrl+V 按鍵。
    //
    // 原因：當控制台裡執行 TUI 程式（如 Claude Code）時，控制台被切到 raw 模式，
    // 合成的 Ctrl+V 按鍵會被該程式當作普通按鍵吃掉，不會觸發貼上，導致剪貼簿內容
    // 根本沒插入（但 SendInput 仍返回“成功”，形成假成功）。
    // WM_COMMAND + ID_CONSOLE_PASTE 由 conhost 自身處理，不經過子程式的按鍵流，
    // 且會遵循控制台當前輸入模式（含 bracketed paste），因此 raw 模式下依然有效。
    let target_class = crate::context::read_class_name(target).to_lowercase();
    if target_class.contains("consolewindowclass") {
        crate::commands::system::write_log_line(
            &format!("[RUST] [inject] console paste attempt hwnd={} class={} textLen={}",
                target.0 as isize, target_class, text.len())
        );

        // 先把控制台置前台，確保貼上落在正確的視窗上
        let fg_ok = force_foreground(target);

        let mut result_val: usize = 0;
        let send_ok = SendMessageTimeoutW(
            target,
            WM_COMMAND,
            windows::Win32::Foundation::WPARAM(ID_CONSOLE_PASTE),
            windows::Win32::Foundation::LPARAM(0),
            SMTO_ABORTIFHUNG,
            2000, // 2 second timeout
            Some(&mut result_val),
        );

        if send_ok.0 != 0 {
            crate::commands::system::write_log_line(
                &format!("[RUST] [inject] console paste ok hwnd={} fgOk={}", target.0 as isize, fg_ok)
            );
            return InjectResult {
                ok: true,
                strategy: Some("console_paste".to_string()),
                reason: None,
                detail: Some(format!(
                    "hwnd={} class={} textLen={} fgOk={}",
                    target.0 as isize, target_class, text.len(), fg_ok
                )),
                uncertain: false,
            };
        }
        crate::commands::system::write_log_line(
            "[RUST] [inject] console paste failed, fallback to SendInput"
        );
    }

    // Step 2: Try WM_PASTE — this is a message sent directly to the target
    // window handle, so it works even if the target is not the foreground
    // window. Most native Win32 controls (Edit, RichEdit) handle it.
    let probed_paste_target = if focus.0 != std::ptr::null_mut() { focus } else { target };
    // 焦點是錄音開始時抓的。若那個控制元件此刻已經不可見（比如 Everything 原地重新命名用的
    // 臨時 Edit），WM_PASTE 照樣返回成功、文本卻進了看不見的地方，長度核實也會誤判為
    // 成功 —— 所以先換到目標執行緒此刻真正的焦點上，前提是它仍在同一個頂層視窗裡。
    let paste_target = rebind_invisible_focus(target, probed_paste_target);
    let focus_class = crate::context::read_class_name(paste_target).to_lowercase();

    // WM_PASTE works reliably for native Win32 edit controls
    let try_wm_paste = focus_class.contains("edit")
        || focus_class.contains("richedit")
        || focus_class.contains("scintilla");

    if try_wm_paste {
        if let Some(result) = wm_paste_verified(target, paste_target, &focus_class, text, paste_id) {
            return result;
        }
        crate::commands::system::write_log_line("[RUST] [inject] WM_PASTE failed, fallback to SendInput");
    }

    // Step 3: Fallback — force foreground + SendInput Ctrl+V
    let fg_ok = force_foreground(target);
    crate::commands::system::write_log_line(
        &format!("[RUST] [inject] SendInput fallback target={} fg_ok={} class={} textLen={} {}",
            target.0 as isize, fg_ok, focus_class, text.len(), describe_elevation(target))
    );

    // Release stuck modifiers
    release_modifiers();
    std::thread::sleep(std::time::Duration::from_millis(15));

    // Set focus to child control if needed
    if focus != target && focus.0 != std::ptr::null_mut() {
        attach_and_set_focus(target, focus);
    }

    // SendInput Ctrl+V
    let vk_ctrl = VIRTUAL_KEY(0x11);
    let vk_v = VIRTUAL_KEY(0x56);
    let inputs = [
        make_key_input(vk_ctrl, KEYBD_EVENT_FLAGS(0)),
        make_key_input(vk_v, KEYBD_EVENT_FLAGS(0)),
        make_key_input(vk_v, KEYEVENTF_KEYUP),
        make_key_input(vk_ctrl, KEYEVENTF_KEYUP),
    ];
    let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    // 立刻抓 GetLastError（后面的 sleep/release_modifiers 会把它冲掉）。
    // sent=0 且 err=5(ERROR_ACCESS_DENIED) => UIPI 攔截：目標視窗許可權比 SayIt 高
    // （目標以管理員執行而 SayIt 沒有），解決辦法是讓 SayIt 以管理員身份執行。
    let last_err = windows::Win32::Foundation::GetLastError().0;

    std::thread::sleep(std::time::Duration::from_millis(10));
    release_modifiers();

    let detail = format!(
        "sent={} err={} class={} target={} focus={} textLen={} fgOk={}",
        sent, last_err, focus_class, target.0 as isize, focus.0 as isize, text.len(), fg_ok
    );

    // SendInput Ctrl+V is fire-and-forget. For Chromium-class windows we
    // used to mark ALL results as uncertain, but that was too aggressive —
    // it blocked every browser input field. Now we only mark as uncertain
    // when UIA says the focused element is NOT an editable control.
    let uncertain = false; // UIA-based editability is checked upstream in probe

    if sent >= 4 {
        return InjectResult {
            ok: true,
            strategy: Some("send_input".to_string()),
            reason: None,
            detail: Some(detail),
            uncertain,
        };
    }

    // ── SendInput 沒被完整接受：模擬按鍵被擋在了 OS 層 ──
    //
    // err=5(ERROR_ACCESS_DENIED) 有兩個常見來源，對使用者來說解法不同：
    //   ① UIPI：目標程式以管理員執行、SayIt 沒有 → SayIt 也要以管理員執行
    //   ② 安全軟體的「防鍵盤模擬」攔截 → 要在那邊給 SayIt 加信任
    // 兩者都只能靠日誌區分，所以把程序掃描結果一起記下來。
    const ERROR_ACCESS_DENIED: u32 = 5;
    let guards = detect_input_guard_software();
    crate::commands::system::write_log_line(&format!(
        "[RUST] [inject] SendInput blocked {} inputGuards={}",
        detail, guards
    ));

    // 最後一搏：改用 WM_PASTE。它是發給視窗的訊息，不經過模擬輸入那條通道，
    // 因此「按鍵被攔但訊息能過」的情況下還能救回來。
    // 只在 SendInput 已經失敗時做 —— 那時什麼都沒粘上，誤傷的風險遠小於徹底失敗。
    if !try_wm_paste {
        let mut result_val: usize = 0;
        let send_ok = SendMessageTimeoutW(
            paste_target,
            WM_PASTE,
            windows::Win32::Foundation::WPARAM(0),
            windows::Win32::Foundation::LPARAM(0),
            SMTO_ABORTIFHUNG,
            2000,
            Some(&mut result_val),
        );
        if send_ok.0 != 0 {
            crate::commands::system::write_log_line(&format!(
                "[RUST] [inject] WM_PASTE last-resort ok hwnd={} class={}",
                paste_target.0 as isize, focus_class
            ));
            return InjectResult {
                ok: true,
                strategy: Some("wm_paste_last_resort".to_string()),
                reason: None,
                detail: Some(format!("{} inputGuards={}", detail, guards)),
                uncertain: false,
            };
        }
        crate::commands::system::write_log_line(
            "[RUST] [inject] WM_PASTE last-resort also failed",
        );
    }

    let blocked_by_privilege = sent == 0 && last_err == ERROR_ACCESS_DENIED;
    InjectResult {
        ok: false,
        strategy: Some("send_input".to_string()),
        reason: Some(
            if blocked_by_privilege {
                "input_blocked_access_denied"
            } else {
                "send_input_short_write"
            }
            .to_string(),
        ),
        detail: Some(format!("{} inputGuards={}", detail, guards)),
        uncertain: false,
    }
}

// ─── Verified WM_PASTE ───

#[cfg(windows)]
const WM_GETTEXTLENGTH: u32 = 0x000E;
#[cfg(windows)]
const EM_GETSEL: u32 = 0x00B0;
#[cfg(windows)]
const EM_REPLACESEL: u32 = 0x00C2;
#[cfg(windows)]
const GWL_STYLE: i32 = -16;
#[cfg(windows)]
const ES_MULTILINE: i32 = 0x0004;
#[cfg(windows)]
const ES_READONLY: i32 = 0x0800;

/// 核實用查詢的超時。只取一個數字，正常目標 1ms 內就回；超時說明目標卡住，當作「核實不了」。
#[cfg(windows)]
const VERIFY_QUERY_TIMEOUT_MS: u32 = 150;
/// 每次 WM_PASTE 之後等長度變化的上限。WM_PASTE 是同步訊息，標準 Edit 返回時文本已經
/// 進去了；多等這一會兒是防「目標自己非同步處理貼上」—— 沒等夠就重試，文本會插兩遍。
#[cfg(windows)]
const VERIFY_WAIT_MS: u64 = 150;
/// 第 2、3 次 WM_PASTE 之前的等待，給正佔著剪貼簿的程式（剪貼簿管理器等）讓出時間。
#[cfg(windows)]
const WM_PASTE_RETRY_DELAYS_MS: [u64; 2] = [40, 120];

#[cfg(windows)]
mod ffi {
    use std::ffi::c_void;
    use windows::Win32::Foundation::HWND;

    #[link(name = "user32")]
    extern "system" {
        pub fn IsWindowVisible(hwnd: HWND) -> i32;
        pub fn IsWindowEnabled(hwnd: HWND) -> i32;
        pub fn GetWindowLongW(hwnd: HWND, index: i32) -> i32;
        pub fn GetDlgCtrlID(hwnd: HWND) -> i32;
        pub fn GetParent(hwnd: HWND) -> HWND;
        pub fn IsChild(parent: HWND, child: HWND) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        pub fn CloseHandle(handle: *mut c_void) -> i32;
        pub fn GetCurrentProcess() -> *mut c_void;
    }

    #[link(name = "advapi32")]
    extern "system" {
        pub fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
        pub fn GetTokenInformation(
            token: *mut c_void,
            class: u32,
            info: *mut c_void,
            len: u32,
            ret_len: *mut u32,
        ) -> i32;
    }
}

/// 發 WM_PASTE 並核實文本確實進了輸入框。
///
/// 為什麼要核實：WM_PASTE 返回成功只說明訊息被收下了。Edit 控制元件處理它時自己去
/// OpenClipboard，那一刻剪貼簿若被別的程式佔著就**靜默什麼都不做**。2026-09 有使用者在
/// Everything 裡連續幾次日誌都是 `WM_PASTE ok`，視窗標題（= 搜尋框內容）卻一個字沒變；
/// 不核實的話這類失敗日誌裡看不出來，使用者也看不到兜底卡片。
///
/// 核實只用 WM_GETTEXTLENGTH：它只回一個數字、不復制內容，Edit / Scintilla 內部存著這個數，
/// 10MB 的文件也是直接返回（WM_GETTEXT 才會複製全文，別換成它）。判據只能是「長度變沒變」，
/// 不能是「漲了插入的字數」—— 單行框會截斷換行、有選區時是替換、還有 EM_LIMITTEXT 上限。
///
/// 返回 None 表示 WM_PASTE 訊息本身沒送達，呼叫方照舊走 SendInput。
#[cfg(windows)]
unsafe fn wm_paste_verified(
    target: HWND,
    paste_target: HWND,
    focus_class: &str,
    text: &str,
    paste_id: u64,
) -> Option<InjectResult> {
    let utf16_len = text.encode_utf16().count();
    let len_before = query_text_len(paste_target);
    let sel_before = query_selection(paste_target);

    crate::commands::system::write_log_line(&format!(
        "[RUST] [inject] WM_PASTE attempt pasteId={} hwnd={} class={} textLen={} utf16Len={} lenBefore={} sel={} {} {} clipSeq={} {}",
        paste_id,
        paste_target.0 as isize,
        focus_class,
        text.len(),
        utf16_len,
        fmt_opt(len_before),
        fmt_sel(sel_before),
        describe_paste_target(target, paste_target),
        describe_elevation(target),
        GetClipboardSequenceNumber(),
        describe_clipboard_holder()
    ));

    let base_detail = format!(
        "hwnd={} class={} textLen={}",
        paste_target.0 as isize, focus_class, text.len()
    );
    let succeeded = |strategy: &str, verify: &str, attempts: usize| {
        crate::commands::system::write_log_line(&format!(
            "[RUST] [inject] WM_PASTE ok hwnd={} class={} pasteId={} strategy={} verify={} attempts={}",
            paste_target.0 as isize, focus_class, paste_id, strategy, verify, attempts
        ));
        InjectResult {
            ok: true,
            strategy: Some(strategy.to_string()),
            reason: None,
            detail: Some(format!("{} verify={} attempts={}", base_detail, verify, attempts)),
            uncertain: false,
        }
    };

    // 讀不到長度（目標不回這條查詢）或者沒有可插的字：沒法核實，退回老行為 ——
    // 發一次、信任返回值。這裡絕不重試，否則一旦其實粘上了就是兩遍。
    let before = match len_before {
        Some(n) if utf16_len > 0 => n,
        _ => {
            if !send_wm_paste(paste_target, paste_id, 1) {
                return None;
            }
            return Some(succeeded("wm_paste", "unavailable", 1));
        }
    };

    let mut attempts = 0usize;
    for attempt in 0..=WM_PASTE_RETRY_DELAYS_MS.len() {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(WM_PASTE_RETRY_DELAYS_MS[attempt - 1]));
            // 上一次沒粘上的這段時間裡，剪貼簿可能被別的程式改寫；重試前確認粘的還是我們的文本
            if native_get_clipboard_text().as_deref() != Some(text) {
                let rewritten = set_clipboard_with_retry(text, 3, 20);
                crate::commands::system::write_log_line(&format!(
                    "[RUST] [inject] clipboard changed before retry pasteId={} attempt={} rewritten={} clipSeq={} {}",
                    paste_id,
                    attempt + 1,
                    rewritten,
                    GetClipboardSequenceNumber(),
                    describe_clipboard_holder()
                ));
                if !rewritten {
                    break;
                }
            }
        }

        attempts += 1;
        let seq_before = GetClipboardSequenceNumber();
        if !send_wm_paste(paste_target, paste_id, attempts) {
            if attempt == 0 {
                return None;
            }
            break;
        }
        let (after, waited_ms) = wait_for_text_len_change(paste_target, before);
        crate::commands::system::write_log_line(&format!(
            "[RUST] [inject] WM_PASTE verify pasteId={} attempt={} lenBefore={} lenAfter={} waitedMs={} clipSeqBefore={} clipSeqAfter={}",
            paste_id,
            attempts,
            before,
            fmt_opt(after),
            waited_ms,
            seq_before,
            GetClipboardSequenceNumber()
        ));

        match after {
            // 發出去之後反而讀不到了：不知道進沒進，不能重試
            None => return Some(succeeded("wm_paste", "unreadable", attempts)),
            Some(n) if n != before => return Some(succeeded("wm_paste", "changed", attempts)),
            Some(_) => {
                // 選中的字數恰好等於插入字數時，替換成功長度也不變 —— 分不清就不重試
                if selection_len_matches(sel_before, before, utf16_len) {
                    return Some(succeeded("wm_paste", "ambiguous_same_length", attempts));
                }
            }
        }
    }

    // WM_PASTE 反覆無效，多半是剪貼簿這條路本身被卡住了。純 Edit 控制元件還能用 EM_REPLACESEL
    // 把文字直接寫進選區，完全不經過剪貼簿。只限類名正好是 "edit"：系統會替它跨程序
    // 轉送字串引數（與 WM_SETTEXT 同理）；RichEdit / Scintilla / 各種改名的子類不保證。
    if focus_class == "edit" {
        let multiline = ffi::GetWindowLongW(paste_target, GWL_STYLE) & ES_MULTILINE != 0;
        let normalized = normalize_for_edit(text, multiline);
        let wide: Vec<u16> = normalized.encode_utf16().chain(std::iter::once(0)).collect();
        let mut result_val: usize = 0;
        let send_ok = SendMessageTimeoutW(
            paste_target,
            EM_REPLACESEL,
            windows::Win32::Foundation::WPARAM(1), // 可撤銷
            windows::Win32::Foundation::LPARAM(wide.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            2000,
            Some(&mut result_val),
        );
        let after = if send_ok.0 != 0 { query_text_len(paste_target) } else { None };
        crate::commands::system::write_log_line(&format!(
            "[RUST] [inject] EM_REPLACESEL fallback pasteId={} sent={} multiline={} lenBefore={} lenAfter={}",
            paste_id,
            send_ok.0 != 0,
            multiline,
            before,
            fmt_opt(after)
        ));
        if matches!(after, Some(n) if n != before) {
            return Some(succeeded("em_replacesel", "changed", attempts));
        }
    }

    // 確認沒插進去：如實報失敗，讓前端彈兜底卡片。以前這裡會報成功，使用者什麼都看不到。
    let detail = format!(
        "{} verify=no_effect attempts={} lenBefore={} sel={} {}",
        base_detail,
        attempts,
        before,
        fmt_sel(sel_before),
        describe_paste_target(target, paste_target)
    );
    crate::commands::system::write_log_line(&format!(
        "[RUST] [inject] WM_PASTE no effect pasteId={} {} clipSeq={} {}",
        paste_id,
        detail,
        GetClipboardSequenceNumber(),
        describe_clipboard_holder()
    ));
    Some(InjectResult {
        ok: false,
        strategy: Some("wm_paste".to_string()),
        reason: Some("paste_no_effect".to_string()),
        detail: Some(detail),
        uncertain: false,
    })
}

#[cfg(windows)]
unsafe fn send_wm_paste(hwnd: HWND, paste_id: u64, attempt: usize) -> bool {
    let mut result_val: usize = 0;
    let send_ok = SendMessageTimeoutW(
        hwnd,
        WM_PASTE,
        windows::Win32::Foundation::WPARAM(0),
        windows::Win32::Foundation::LPARAM(0),
        SMTO_ABORTIFHUNG,
        2000,
        Some(&mut result_val),
    );
    if send_ok.0 == 0 {
        let err = windows::Win32::Foundation::GetLastError().0;
        crate::commands::system::write_log_line(&format!(
            "[RUST] [inject] WM_PASTE send failed pasteId={} attempt={} err={}",
            paste_id, attempt, err
        ));
        return false;
    }
    true
}

/// 文本長度（UTF-16 單位）。只取長度不取內容；拿不到返回 None，呼叫方當作「核實不了」。
#[cfg(windows)]
unsafe fn query_text_len(hwnd: HWND) -> Option<usize> {
    let mut out: usize = 0;
    let ok = SendMessageTimeoutW(
        hwnd,
        WM_GETTEXTLENGTH,
        windows::Win32::Foundation::WPARAM(0),
        windows::Win32::Foundation::LPARAM(0),
        SMTO_ABORTIFHUNG,
        VERIFY_QUERY_TIMEOUT_MS,
        Some(&mut out),
    );
    if ok.0 == 0 { None } else { Some(out) }
}

/// 選區 (start, end)。用的是返回值那種形式：兩端各只有 16 位，超過 65535 字會被截斷；
/// 指標引數那種形式跨程序不一定被轉送，不敢用。所以它只進日誌、只在短文本里參與判斷。
#[cfg(windows)]
unsafe fn query_selection(hwnd: HWND) -> Option<(usize, usize)> {
    let mut out: usize = 0;
    let ok = SendMessageTimeoutW(
        hwnd,
        EM_GETSEL,
        windows::Win32::Foundation::WPARAM(0),
        windows::Win32::Foundation::LPARAM(0),
        SMTO_ABORTIFHUNG,
        VERIFY_QUERY_TIMEOUT_MS,
        Some(&mut out),
    );
    if ok.0 == 0 { None } else { Some((out & 0xFFFF, (out >> 16) & 0xFFFF)) }
}

#[cfg(windows)]
unsafe fn wait_for_text_len_change(hwnd: HWND, before: usize) -> (Option<usize>, u64) {
    let started = Instant::now();
    loop {
        let now = query_text_len(hwnd);
        let waited_ms = started.elapsed().as_millis() as u64;
        match now {
            Some(n) if n == before && waited_ms < VERIFY_WAIT_MS => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            _ => return (now, waited_ms),
        }
    }
}

#[cfg(any(windows, test))]
/// 替換選區時長度不變的那種情況。選區位置只有 16 位，文本超過 0xFFFF 時位置不可信，一律不認。
fn selection_len_matches(sel: Option<(usize, usize)>, text_len: usize, inserted_len: usize) -> bool {
    match sel {
        Some((start, end)) if text_len < 0xFFFF && end > start => end - start == inserted_len,
        _ => false,
    }
}

#[cfg(any(windows, test))]
/// EM_REPLACESEL 不像貼上那樣替你處理換行：多行框要 CRLF，單行框遇到換行會顯示成亂碼，換成空格。
fn normalize_for_edit(text: &str, multiline: bool) -> String {
    let unified = text.replace("\r\n", "\n");
    if multiline {
        unified.replace('\n', "\r\n")
    } else {
        unified.replace('\n', " ")
    }
}

/// 錄音開始時抓到的焦點若已不可見，換成目標執行緒此刻的焦點（限同一個頂層視窗內）。
#[cfg(windows)]
unsafe fn rebind_invisible_focus(target: HWND, probed: HWND) -> HWND {
    if probed == target || ffi::IsWindowVisible(probed) != 0 {
        return probed;
    }
    let current = current_focus_of(target);
    let usable = current.filter(|cur| {
        *cur != probed
            && ffi::IsWindowVisible(*cur) != 0
            && (*cur == target || ffi::IsChild(target, *cur) != 0)
    });
    crate::commands::system::write_log_line(&format!(
        "[RUST] [inject] probed focus invisible hwnd={} class={} currentFocus={} rebound={}",
        probed.0 as isize,
        crate::context::read_class_name(probed),
        current.map_or(0, |h| h.0 as isize),
        usable.is_some()
    ));
    usable.unwrap_or(probed)
}

#[cfg(windows)]
unsafe fn current_focus_of(target: HWND) -> Option<HWND> {
    use windows::Win32::UI::WindowsAndMessaging::{GetGUIThreadInfo, GUITHREADINFO};
    let tid = GetWindowThreadProcessId(target, None);
    if tid == 0 {
        return None;
    }
    let mut info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    if GetGUIThreadInfo(tid, &mut info).is_err() || info.hwndFocus.0.is_null() {
        return None;
    }
    Some(info.hwndFocus)
}

/// 貼上目標控制元件此刻的狀態。用來區分「粘進了看不見的控制元件」「焦點在錄音期間換了」
/// 「控制元件只讀 / 被停用」這幾種同樣表現為"日誌成功、框裡沒字"的情況。
#[cfg(windows)]
unsafe fn describe_paste_target(target: HWND, paste_target: HWND) -> String {
    let style = ffi::GetWindowLongW(paste_target, GWL_STYLE);
    let parent = ffi::GetParent(paste_target);
    let fg_now = GetForegroundWindow();
    let focus_now = current_focus_of(target);
    format!(
        "visible={} enabled={} readOnly={} multiline={} ctrlId={} parent={}/{} inTarget={} fgIsTarget={} focusNow={} focusUnchanged={}",
        ffi::IsWindowVisible(paste_target) != 0,
        ffi::IsWindowEnabled(paste_target) != 0,
        style & ES_READONLY != 0,
        style & ES_MULTILINE != 0,
        ffi::GetDlgCtrlID(paste_target),
        parent.0 as isize,
        if parent.0.is_null() { String::new() } else { crate::context::read_class_name(parent) },
        paste_target == target || ffi::IsChild(target, paste_target) != 0,
        fg_now == target,
        focus_now.map_or(0, |h| h.0 as isize),
        focus_now == Some(paste_target)
    )
}

/// SayIt 與目標程序是否以管理員執行。目標許可權更高時往往連它的令牌都打不開，記成 "?"
/// —— 此時 "selfElevated=false targetElevated=?" 本身就強烈暗示目標是管理員程序。
#[cfg(windows)]
fn describe_elevation(target: HWND) -> String {
    use std::sync::OnceLock;
    static SELF_ELEVATED: OnceLock<Option<bool>> = OnceLock::new();
    let self_elevated = *SELF_ELEVATED.get_or_init(|| unsafe { token_elevated(ffi::GetCurrentProcess()) });

    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(target, Some(&mut pid)) };
    let target_elevated = if pid == 0 {
        None
    } else {
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        unsafe {
            let process = ffi::OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                None
            } else {
                let elevated = token_elevated(process);
                ffi::CloseHandle(process);
                elevated
            }
        }
    };
    format!(
        "selfElevated={} targetElevated={}",
        fmt_bool_opt(self_elevated),
        fmt_bool_opt(target_elevated)
    )
}

#[cfg(windows)]
unsafe fn token_elevated(process: *mut std::ffi::c_void) -> Option<bool> {
    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_ELEVATION_CLASS: u32 = 20;
    let mut token: *mut std::ffi::c_void = std::ptr::null_mut();
    if ffi::OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
        return None;
    }
    let mut elevation: u32 = 0;
    let mut ret_len: u32 = 0;
    let ok = ffi::GetTokenInformation(
        token,
        TOKEN_ELEVATION_CLASS,
        &mut elevation as *mut u32 as *mut std::ffi::c_void,
        std::mem::size_of::<u32>() as u32,
        &mut ret_len,
    );
    ffi::CloseHandle(token);
    if ok == 0 { None } else { Some(elevation != 0) }
}

#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
fn fmt_opt(value: Option<usize>) -> String {
    value.map_or_else(|| "?".to_string(), |v| v.to_string())
}

#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
fn fmt_sel(sel: Option<(usize, usize)>) -> String {
    sel.map_or_else(|| "?".to_string(), |(start, end)| format!("{}-{}", start, end))
}

#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
fn fmt_bool_opt(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "true",
        Some(false) => "false",
        None => "?",
    }
}

// ─── Window activation helpers ───

#[cfg(windows)]
unsafe fn force_foreground(target: HWND) -> bool {
    #[link(name = "user32")]
    extern "system" {
        fn AttachThreadInput(id_attach: u32, id_attach_to: u32, f_attach: i32) -> i32;
        fn BringWindowToTop(hwnd: HWND) -> i32;
        fn ShowWindow(hwnd: HWND, n_cmd_show: i32) -> i32;
    }
    const SW_SHOW: i32 = 5;

    let my_tid = GetCurrentThreadId();
    let mut pid: u32 = 0;
    let target_tid = GetWindowThreadProcessId(target, Some(&mut pid));

    let attached = if target_tid != 0 && target_tid != my_tid {
        AttachThreadInput(my_tid, target_tid, 1) != 0
    } else {
        false
    };

    // Use a harmless key (VK_F24 = 0x87) to satisfy SetForegroundWindow's
    // "caller must have received input" requirement. Alt is problematic
    // because its keyup activates menus in many apps.
    let f24_down = make_key_input(VIRTUAL_KEY(0x87), KEYBD_EVENT_FLAGS(0));
    let f24_up = make_key_input(VIRTUAL_KEY(0x87), KEYEVENTF_KEYUP);
    let _ = SendInput(&[f24_down, f24_up], std::mem::size_of::<INPUT>() as i32);

    let _ = ShowWindow(target, SW_SHOW);
    let _ = BringWindowToTop(target);
    let _ = SetForegroundWindow(target);
    std::thread::sleep(std::time::Duration::from_millis(50));

    let fg_ok = GetForegroundWindow() == target;
    if !fg_ok {
        let _ = SetForegroundWindow(target);
        std::thread::sleep(std::time::Duration::from_millis(30));
    }

    if attached {
        AttachThreadInput(my_tid, target_tid, 0);
    }

    GetForegroundWindow() == target
}

#[cfg(windows)]
unsafe fn attach_and_set_focus(target: HWND, focus: HWND) {
    #[link(name = "user32")]
    extern "system" {
        fn AttachThreadInput(id_attach: u32, id_attach_to: u32, f_attach: i32) -> i32;
    }

    let my_tid = GetCurrentThreadId();
    let mut pid: u32 = 0;
    let target_tid = GetWindowThreadProcessId(target, Some(&mut pid));
    let attached = if target_tid != 0 && target_tid != my_tid {
        AttachThreadInput(my_tid, target_tid, 1) != 0
    } else {
        false
    };

    use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    let _ = SetFocus(focus);
    std::thread::sleep(std::time::Duration::from_millis(10));

    if attached {
        AttachThreadInput(my_tid, target_tid, 0);
    }
}

// ─── Key input helpers ───

#[cfg(windows)]
fn make_key_input(vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0,
            },
        },
    }
}

#[cfg(windows)]
unsafe fn release_modifiers() {
    let modifiers: [u16; 6] = [0xA4, 0xA5, 0xA0, 0xA1, 0xA2, 0xA3];
    let inputs: Vec<INPUT> = modifiers.iter().map(|&vk| {
        make_key_input(VIRTUAL_KEY(vk), KEYEVENTF_KEYUP)
    }).collect();
    let _ = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
}

// ─── Clipboard helpers ───

/// 指認「現在是誰佔著剪貼簿」。
///
/// 這是排查安全軟體（360 安全衛士、火絨、QQ 電腦管家等）攔截剪貼簿最直接的手段：
/// 它們的剪貼簿保護會先把剪貼簿開啟，我們的 `OpenClipboard` 就拿不到、只能拿到一個
/// 含義模糊的 ERROR_ACCESS_DENIED。`GetOpenClipboardWindow` 能給出持有者視窗，
/// 順著它拿到程序名，日誌裡就直接寫著是哪個程式，不用再靠猜或者讓使用者逐個關軟體。
#[cfg(windows)]
unsafe fn describe_clipboard_holder() -> String {
    // 拿不到持有者（Err 或空 HWND）卻仍然打不開剪貼簿：通常是驅動層/鉤子攔截，
    // 而不是正常的「另一個程式剛好佔著」的競爭
    let holder = match GetOpenClipboardWindow() {
        Ok(h) if !h.0.is_null() => h,
        _ => return "holder=<none>".to_string(),
    };
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(holder, Some(&mut pid));
    let name = crate::context::get_process_name(pid);
    format!(
        "holder_hwnd={} holder_pid={} holder_process={} holder_class={}",
        holder.0 as isize,
        pid,
        if name.is_empty() { "<unknown>" } else { &name },
        crate::context::read_class_name(holder)
    )
}

/// 已知會攔截模擬按鍵 / 剪貼簿的安全軟體程序名（小寫）。
///
/// **只用於日誌**：命中了不改任何行為，僅在插入失敗時把它寫進日誌，讓「文本插不進去」
/// 一眼能看出環境裡有這類軟體。絕不能拿它做拉黑或者繞過 —— 那既不可靠，也會讓我們
/// 更像惡意程式（本來就已經有誤報問題了）。
#[cfg(windows)]
const INPUT_GUARD_PROCESSES: &[&str] = &[
    // 360
    "360tray.exe", "360safe.exe", "zhudongfangyu.exe", "360sd.exe", "360rp.exe",
    // 火絨
    "hipstray.exe", "usysdiag.exe", "wsctrlsvc.exe",
    // 騰訊電腦管家
    "qqpctray.exe", "qqpcmgr.exe", "qqpcrtp.exe",
    // 金山毒霸
    "kxetray.exe", "kislive.exe", "kwsprotect64.exe",
    // 聯想電腦管家 / 其他常見國產安全套件
    "lavasoft.exe", "baidusdtray.exe", "avp.exe",
];

/// 掃一遍程序，列出命中的安全軟體。結果快取 —— 這些軟體不會在一次會話中途裝上，
/// 而插入失敗可能連續發生，沒必要每次都列舉全部程序。
#[cfg(windows)]
fn detect_input_guard_software() -> &'static str {
    use std::sync::OnceLock;
    static DETECTED: OnceLock<String> = OnceLock::new();
    DETECTED.get_or_init(|| {
        use windows::Win32::System::ProcessStatus::K32EnumProcesses;
        let mut pids = vec![0u32; 1024];
        let mut needed: u32 = 0;
        let ok = unsafe {
            K32EnumProcesses(
                pids.as_mut_ptr(),
                (pids.len() * std::mem::size_of::<u32>()) as u32,
                &mut needed,
            )
        };
        if !ok.as_bool() {
            return String::from("<enum_failed>");
        }
        let count = needed as usize / std::mem::size_of::<u32>();
        let mut hits: Vec<String> = Vec::new();
        for &pid in pids.iter().take(count) {
            if pid == 0 {
                continue;
            }
            let name = crate::context::get_process_name(pid).to_lowercase();
            if name.is_empty() {
                continue;
            }
            if INPUT_GUARD_PROCESSES.contains(&name.as_str()) && !hits.contains(&name) {
                hits.push(name);
            }
        }
        if hits.is_empty() {
            String::from("<none>")
        } else {
            hits.join(",")
        }
    })
    .as_str()
}

#[cfg(windows)]
unsafe fn set_clipboard_with_retry(text: &str, max_retries: u32, retry_delay_ms: u64) -> bool {
    for attempt in 0..max_retries {
        match native_set_clipboard_text(text) {
            Ok(()) => return true,
            Err(why) => {
                // 每次都記：能區分「被佔用一下、重試就成了」和「一直被攔」，
                // 這兩種給使用者的建議完全不同
                crate::commands::system::write_log_line(&format!(
                    "[RUST] [inject] clipboard set attempt {}/{} failed {}",
                    attempt + 1,
                    max_retries,
                    why
                ));
            }
        }
        if attempt < max_retries - 1 {
            std::thread::sleep(std::time::Duration::from_millis(retry_delay_ms));
        }
    }
    false
}

/// 寫剪貼簿。失敗時返回**具體卡在哪一步**，而不是一個籠統的 false ——
/// OpenClipboard 被攔、記憶體分配失敗、SetClipboardData 被拒，三者成因完全不同。
#[cfg(windows)]
unsafe fn native_set_clipboard_text(text: &str) -> Result<(), String> {
    if OpenClipboard(HWND(std::ptr::null_mut())).is_err() {
        let err = windows::Win32::Foundation::GetLastError().0;
        return Err(format!(
            "step=OpenClipboard err={} {}",
            err,
            describe_clipboard_holder()
        ));
    }

    let result = (|| -> Result<(), String> {
        let _ = EmptyClipboard();
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let byte_len = wide.len() * 2;

        let hmem = match GlobalAlloc(GMEM_MOVEABLE, byte_len) {
            Ok(h) => h,
            Err(e) => return Err(format!("step=GlobalAlloc bytes={} err={}", byte_len, e)),
        };

        let locked = GlobalLock(hmem);
        if locked.is_null() {
            return Err("step=GlobalLock err=null".to_string());
        }

        std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, locked as *mut u8, byte_len);
        let _ = GlobalUnlock(hmem);

        // CF_UNICODETEXT = 13
        if let Err(e) = SetClipboardData(13, HANDLE(hmem.0 as *mut _)) {
            let err = windows::Win32::Foundation::GetLastError().0;
            return Err(format!("step=SetClipboardData err={} detail={}", err, e));
        }

        // 標記本次剪貼簿內容「不進入 Win+V 歷史 / 不上傳雲剪貼簿」。
        // 否則每次插入文本都會往剪貼簿歷史裡塞一條（配合「保護剪貼簿」還原時甚至塞兩條）。
        // 與瀏覽器無痕模式、密碼管理器用的是同一套機制。失敗不影響貼上，忽略即可。
        mark_clipboard_history_excluded();

        Ok(())
    })();

    let _ = CloseClipboard();
    result
}

/// 往當前剪貼簿會話寫入排除標記，阻止內容進入剪貼簿歷史(Win+V)與雲剪貼簿。
/// 必須在 OpenClipboard/EmptyClipboard 之後、CloseClipboard 之前呼叫。
#[cfg(windows)]
unsafe fn mark_clipboard_history_excluded() {
    use windows::core::PCWSTR;
    use windows::Win32::System::DataExchange::RegisterClipboardFormatW;

    // 寫一個 4 位元組值為 0 的 DWORD 到指定格式。
    let write_dword_format = |format_name: &[u16], value: u32| {
        let fmt = RegisterClipboardFormatW(PCWSTR(format_name.as_ptr()));
        if fmt == 0 { return; }
        let hmem = match GlobalAlloc(GMEM_MOVEABLE, 4) {
            Ok(h) => h,
            Err(_) => return,
        };
        let locked = GlobalLock(hmem);
        if locked.is_null() { return; }
        std::ptr::copy_nonoverlapping(&value as *const u32 as *const u8, locked as *mut u8, 4);
        let _ = GlobalUnlock(hmem);
        // 標記寫失敗幾乎不會發生；與上面 CF_UNICODETEXT 的處理一致，失敗時不額外釋放。
        let _ = SetClipboardData(fmt, HANDLE(hmem.0 as *mut _));
    };

    // "CanIncludeInClipboardHistory" = 0 → 不進 Win+V 歷史
    let hist: Vec<u16> = "CanIncludeInClipboardHistory\0".encode_utf16().collect();
    write_dword_format(&hist, 0);

    // "CanUploadToCloudClipboard" = 0 → 不同步到雲剪貼簿
    let cloud: Vec<u16> = "CanUploadToCloudClipboard\0".encode_utf16().collect();
    write_dword_format(&cloud, 0);

    // "ExcludeClipboardContentFromMonitorProcessing" → 兜底：整體排除監控處理
    // 該格式只要存在即生效，值內容不限，這裡同樣寫 0。
    let exclude: Vec<u16> = "ExcludeClipboardContentFromMonitorProcessing\0".encode_utf16().collect();
    write_dword_format(&exclude, 0);

    // "Clipboard Viewer Ignore" → 給第三方剪貼簿管理器看的，KeePass 2.x 用的也是它。
    // 上面三個是微軟的約定，Ditto 的正式版（截至 3.25.113，2025-09）一個都不認，只認這個。
    // 它的原始碼 2025-12 才補上微軟那兩個，2026-01 又掛到一個可關的選項後面；而這個格式
    // 一直是無條件跳過（ClipboardViewer.cpp 的 GetIgnoreClipboardChange）。所以別以為
    // 有了上面三個就可以刪掉它。只要存在就生效，值不限。KeePass 早年預設不開它，是因為
    // 配 .NET/OLE 剪貼簿介面時會讓 Office 剪貼簿擴充套件崩潰；我們走原生 SetClipboardData，
    // 不是那条路。（issue #84）
    let viewer_ignore: Vec<u16> = "Clipboard Viewer Ignore\0".encode_utf16().collect();
    write_dword_format(&viewer_ignore, 0);
}

#[cfg(windows)]
pub unsafe fn set_clipboard_with_retry_pub(text: &str, max_retries: u32, retry_delay_ms: u64) -> bool {
    set_clipboard_with_retry(text, max_retries, retry_delay_ms)
}

/// 給剪貼簿拍快照。超預算、打不開時返回 None（呼叫方退回只還原文字）；剪貼簿為空時返回空快照。
#[cfg(windows)]
unsafe fn snapshot_clipboard(paste_id: u64) -> Option<ClipboardSnapshot> {
    use windows::Win32::System::DataExchange::EnumClipboardFormats;
    use windows::Win32::System::Memory::GlobalSize;

    let started = Instant::now();
    if OpenClipboard(HWND(std::ptr::null_mut())).is_err() {
        return None;
    }
    let result = (|| -> Result<ClipboardSnapshot, &'static str> {
        let mut snapshot = ClipboardSnapshot::default();
        let mut total = 0usize;
        let mut format = 0u32;
        loop {
            format = EnumClipboardFormats(format);
            if format == 0 {
                break;
            }
            if started.elapsed().as_millis() > CLIPBOARD_SNAPSHOT_BUDGET_MS {
                return Err("over_time_budget");
            }
            if !clipboard_format_is_copyable(format) {
                continue;
            }
            let Ok(handle) = GetClipboardData(format) else { continue };
            let hmem = windows::Win32::Foundation::HGLOBAL(handle.0);
            let size = GlobalSize(hmem);
            if size == 0 {
                continue;
            }
            total += size;
            if total > CLIPBOARD_SNAPSHOT_MAX_BYTES {
                return Err("over_size_budget");
            }
            let locked = GlobalLock(hmem);
            if locked.is_null() {
                continue;
            }
            let bytes = std::slice::from_raw_parts(locked as *const u8, size).to_vec();
            let _ = GlobalUnlock(hmem);
            snapshot.formats.push((format, bytes));
        }
        Ok(snapshot)
    })();
    let _ = CloseClipboard();
    match result {
        Ok(snapshot) => Some(snapshot),
        Err(reason) => {
            crate::commands::system::write_log_line(&format!(
                "[RUST] [inject] clipboard snapshot skipped pasteId={} reason={} elapsedMs={}; restoring text only",
                paste_id,
                reason,
                started.elapsed().as_millis()
            ));
            None
        }
    }
}

/// 把快照整份寫回剪貼簿。剪貼簿被別的程式佔著時按 retry 重試。
#[cfg(windows)]
unsafe fn restore_clipboard_snapshot(snapshot: &ClipboardSnapshot, max_retries: u32, retry_delay_ms: u64) -> bool {
    use windows::Win32::Foundation::GlobalFree;
    for attempt in 0..=max_retries {
        if OpenClipboard(HWND(std::ptr::null_mut())).is_ok() {
            let _ = EmptyClipboard();
            let mut written = 0usize;
            for (format, bytes) in &snapshot.formats {
                let Ok(hmem) = GlobalAlloc(GMEM_MOVEABLE, bytes.len()) else { continue };
                let locked = GlobalLock(hmem);
                if locked.is_null() {
                    let _ = GlobalFree(hmem);
                    continue;
                }
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), locked as *mut u8, bytes.len());
                let _ = GlobalUnlock(hmem);
                if SetClipboardData(*format, HANDLE(hmem.0 as *mut _)).is_ok() {
                    written += 1;
                } else {
                    let _ = GlobalFree(hmem);
                }
            }
            // 還原的是使用者自己複製的內容，它進剪貼簿歷史那一次已經記過了，別再記一條。
            mark_clipboard_history_excluded();
            let _ = CloseClipboard();
            return written > 0;
        }
        if attempt < max_retries {
            std::thread::sleep(std::time::Duration::from_millis(retry_delay_ms));
        }
    }
    false
}

/// 清空剪貼簿（還原場景：插入前剪貼簿本來就沒有文本內容）。
#[cfg(windows)]
unsafe fn native_clear_clipboard() -> bool {
    if OpenClipboard(HWND(std::ptr::null_mut())).is_err() { return false; }
    let ok = EmptyClipboard().is_ok();
    let _ = CloseClipboard();
    ok
}

/// 讀取剪貼簿當前的 Unicode 文本內容（CF_UNICODETEXT=13）。
/// 用於「插入前儲存、插入後還原」——避免使用者之前複製的內容被覆蓋丟失。
/// 剪貼簿為空、或內容不是文本（圖片/檔案等）時返回 None，不做處理即等價於「插入前剪貼簿本來就沒有可還原的文本」。
#[cfg(windows)]
unsafe fn native_get_clipboard_text() -> Option<String> {
    if OpenClipboard(HWND(std::ptr::null_mut())).is_err() { return None; }

    let result = (|| -> Option<String> {
        let handle = GetClipboardData(13).ok()?; // CF_UNICODETEXT
        let hmem = windows::Win32::Foundation::HGLOBAL(handle.0);
        let locked = GlobalLock(hmem);
        if locked.is_null() { return None; }

        // Unicode 文本以 NUL 結尾，逐 u16 掃描找長度（記憶體塊可能比字串長）
        let mut len = 0usize;
        let ptr = locked as *const u16;
        loop {
            if *ptr.add(len) == 0 { break; }
            len += 1;
            if len > 10_000_000 { break; } // 防禦性上限，避免異常記憶體導致死迴圈
        }
        let slice = std::slice::from_raw_parts(ptr, len);
        let text = String::from_utf16_lossy(slice);
        let _ = GlobalUnlock(hmem);
        Some(text)
    })();

    let _ = CloseClipboard();
    result
}

#[cfg(all(test, windows))]
mod clipboard_format_tests {
    use super::clipboard_format_is_copyable;

    #[test]
    fn byte_copyable_formats_are_kept_and_gdi_handles_skipped() {
        // 文字、DIB 圖片、檔案列表、註冊格式（HTML / PNG 之類）都保留
        for keep in [1u32, 7, 8, 13, 15, 16, 17, 0xC000, 0xC1A3] {
            assert!(clipboard_format_is_copyable(keep), "{keep:#x}");
        }
        for skip in [2u32, 3, 9, 14, 0x80, 0x82, 0x8E, 0x200, 0x2FF, 0x300, 0x3FF] {
            assert!(!clipboard_format_is_copyable(skip), "{skip:#x}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        describe_editability, is_likely_editable_pub, normalize_for_edit, selection_len_matches,
        windows_editability_gate as editability_gate, EditableGate,
    };
    use crate::context::AppContext;

    /// 這條判據決定「長度沒變」時要不要重試。認錯成「替換成功」只是少救一次；
    /// 認錯成「沒粘上」會重試，文本就插兩遍 —— 所以邊界都往「分不清、不重試」那邊收。
    #[test]
    fn same_length_replacement_is_treated_as_ambiguous() {
        // 選中 3 個字、插入 3 個字：替換成功長度也不變
        assert!(selection_len_matches(Some((2, 5)), 100, 3));
        // 沒有選區時長度不變就是真沒粘上
        assert!(!selection_len_matches(Some((5, 5)), 100, 3));
        assert!(!selection_len_matches(Some((2, 4)), 100, 3));
        assert!(!selection_len_matches(None, 100, 3));
    }

    #[test]
    fn selection_positions_are_distrusted_beyond_16_bits() {
        // EM_GETSEL 返回值兩端各只有 16 位，長文本里位置被截斷，不能據此下結論
        assert!(!selection_len_matches(Some((2, 5)), 0x1_0000, 3));
    }

    #[test]
    fn edit_newlines_are_normalized_per_style() {
        assert_eq!(normalize_for_edit("a\nb\r\nc", true), "a\r\nb\r\nc");
        assert_eq!(normalize_for_edit("a\nb\r\nc", false), "a b c");
    }

    /// 微信 4.1.13.65 的真實取值，來自 dev-scripts/probe-foreground-target.ps1：
    /// Qt 自繪視窗，caret / 原生類名 / UIA 三層判據全部為空，只有程序名能放行它。
    fn weixin_4x_ctx(process_name: &str) -> AppContext {
        AppContext {
            process_name: process_name.to_string(),
            window_class: "Qt51514QWindowIcon".to_string(),
            // Qt 整個視窗只有一個 HWND，GetFocus 返回的就是頂層視窗本身
            focus_class: "Qt51514QWindowIcon".to_string(),
            has_caret: false,
            // UIA 只能看到頂層視窗，看不到自繪的輸入框
            control_type: "Window".to_string(),
            is_value_pattern_available: false,
            is_keyboard_focusable: false,
            is_enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn weixin_4x_is_editable_via_process_allowlist() {
        // 4.1 起 exe 叫 Weixin.exe。這條 2026-09-20 之前是 false，表現為使用者升級微信後
        // 插不進文本、只能從兜底卡片手動複製。
        //
        // 斷言的是 gate 而不只是"能插"：前三層判據對 Qt 自繪視窗全都是空的，唯一放行
        // 它的是程序名白名單。只斷言 bool 的話，將來若有人給 Qt 類名開了個後門，結論
        // 照樣是 true，測試照樣全綠，而真正在起作用的機制已經換了。
        let ctx = weixin_4x_ctx("Weixin.exe");
        assert_eq!(editability_gate(&ctx), EditableGate::ProcessAllowlist);
        assert!(is_likely_editable_pub(&ctx));
    }

    #[test]
    fn wechat_3x_name_still_editable() {
        // 老版本與微信開發者工具仍然叫 WeChat.exe，改名後不能把它們丟掉
        assert_eq!(
            editability_gate(&weixin_4x_ctx("WeChat.exe")),
            EditableGate::ProcessAllowlist
        );
        assert_eq!(
            editability_gate(&weixin_4x_ctx("wechatdevtools.exe")),
            EditableGate::ProcessAllowlist
        );
    }

    /// 明確否掉的方案：不能因為"Qt 視窗拿不到 UIA 資訊"就把所有 Qt 應用當成可編輯。
    /// 判錯的代價不對稱 —— 漏判只是讓使用者從兜底卡片手動粘一次，誤判會讓前端報
    /// 插入成功而目標一個字都沒收到（SendInput 成功不等於插進去了，見 pitfalls #28）。
    #[test]
    fn unknown_qt_app_falls_through_all_gates() {
        let ctx = weixin_4x_ctx("qbittorrent.exe");
        // 與上面那條微信用例唯一的差別就是程序名，所以這一對測試合起來證明：
        // 放行微信的確實是程序名那一層，不是別的判據順手放過去的。
        assert_eq!(editability_gate(&ctx), EditableGate::NoSignal);
        assert!(!is_likely_editable_pub(&ctx));
    }

    /// 桌面圖示重新命名時 explorer 確實有 caret，但那裡不是文本注入目標；
    /// 這條釘住 caret 之前的那道明確拒絕沒有被後來的放寬繞過。
    #[test]
    fn explorer_desktop_is_rejected_before_caret() {
        let ctx = AppContext {
            process_name: "explorer.exe".to_string(),
            window_class: "Progman".to_string(),
            focus_class: "SysListView32".to_string(),
            has_caret: true,
            is_enabled: true,
            ..Default::default()
        };
        // has_caret=true 卻仍然拒絕，說明那道明確拒絕排在 caret 之前 —— 這正是它存在
        // 的理由，改動判據順序會讓它失效。
        assert_eq!(editability_gate(&ctx), EditableGate::ExplorerDesktopReject);
        assert!(!is_likely_editable_pub(&ctx));
    }

    /// 這行診斷串會進 sayit.log，而日誌整份跟著診斷包發給開發者。視窗標題裡可能是
    /// 聊天物件名或文件名，exe 路徑裡帶 Windows 使用者名稱，兩者對排查毫無作用。
    #[test]
    fn describe_editability_never_leaks_title_or_path() {
        let mut ctx = weixin_4x_ctx("Weixin.exe");
        ctx.window_title = "文件传输助手 - 季度奖金方案".to_string();
        ctx.exe_path = r"C:\Users\zhangsan\AppData\Local\Programs\Weixin.exe".to_string();

        let line = describe_editability(&ctx, editability_gate(&ctx));

        assert!(!line.contains("季度奖金"), "window title leaked: {line}");
        assert!(!line.contains("zhangsan"), "exe path leaked: {line}");
        // 同時確認該有的還在，別把這條測試變成"什麼都不記也能過"
        assert!(line.contains("gate=process_allowlist"), "{line}");
        assert!(line.contains("process=Weixin.exe"), "{line}");
        assert!(line.contains("focusClass=Qt51514QWindowIcon"), "{line}");
    }
}
