//! macOS 文字插入：写 NSPasteboard → 模拟 ⌘V → 延时还原剪贴板。
//!
//! 与 Windows 的对应关系：
//! - Windows 先试 `WM_PASTE` 直发控件、再退 `SendInput` Ctrl+V；mac 没有跨进程直发粘贴消息的
//!   通道，统一用 `CGEventPost` 发 ⌘V（终端里的 TUI 也能用：⌘V 由终端程序自己处理，
//!   不需要 conhost 那条特殊路径）。
//! - 剪贴板保护：插入前给剪贴板拍全类型快照，粘贴后 500 ms 若剪贴板仍是我们写的那份就还原。
//!   「是否被改过」看 changeCount（对应 GetClipboardSequenceNumber）。
//! - 剪贴板历史排除：写入时附带 `org.nspasteboard.TransientType`。
//! - 没有「抢前台」：悬浮窗不激活，焦点一直在目标程序。插入前若前台程序已经不是录音开始时
//!   那个（用户切走了），不硬往新窗口里插，交给兜底卡片。

use super::InjectResult;
use crate::commands::system::write_log_line;
use crate::platform::macos::{appkit, ax, cf::CfOwned, ffi::*, focus, keycodes};
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 每次粘贴递增。旧的还原线程看到新一代粘贴就放弃，避免覆盖新内容。
static PASTE_GENERATION: AtomicU64 = AtomicU64::new(0);
// [自訂] L-80：焦點不符時立即還原，避免與前次貼上的延後還原互相覆蓋。
static PASTE_TRANSACTION: Mutex<()> = Mutex::new(());

const CLIPBOARD_SNAPSHOT_BUDGET: Duration = Duration::from_millis(150);
const CLIPBOARD_SNAPSHOT_MAX_BYTES: usize = 64 * 1024 * 1024;
/// 还原前等多久。比 Windows 的 400 ms 稍长：mac 上目标程序是在处理 ⌘V 事件时才去读剪贴板，
/// Electron / 浏览器还是异步读，太早还原会让它读到旧内容。
const RESTORE_DELAY: Duration = Duration::from_millis(500);

fn result(ok: bool, strategy: &str, reason: Option<&str>, detail: String) -> InjectResult {
    InjectResult {
        ok,
        strategy: Some(strategy.to_string()),
        reason: reason.map(str::to_string),
        detail: Some(detail),
        uncertain: false,
    }
}

// [自訂] L-80：主執行緒忙時沿用最近一次成功查得的鍵碼。
static PASTE_KEYCODE: AtomicU16 = AtomicU16::new(keycodes::KVK_ANSI_V);

pub fn cached_keycode_for_v() -> CGKeyCode {
    PASTE_KEYCODE.load(Ordering::Relaxed)
}

/// 当前键盘布局下打出「v」的那个键码。Dvorak 等布局里 ⌘V 不在 QWERTY 的 V 位置上。
/// 中文输入法激活时取它底下的 ASCII 布局。找不到就用 ANSI V。
pub fn keycode_for_v() -> CGKeyCode {
    unsafe {
        let source = TISCopyCurrentASCIICapableKeyboardLayoutInputSource();
        if source.is_null() {
            return cached_keycode_for_v();
        }
        let layout_data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData) as CFDataRef;
        let mut found = None;
        if !layout_data.is_null() {
            let layout = CFDataGetBytePtr(layout_data) as *const std::ffi::c_void;
            let kbd_type = LMGetKbdType() as u32;
            for keycode in 0u16..128 {
                let mut dead_key_state: u32 = 0;
                let mut chars = [0u16; 4];
                let mut len: usize = 0;
                let status = UCKeyTranslate(
                    layout,
                    keycode,
                    kUCKeyActionDisplay,
                    0,
                    kbd_type,
                    kUCKeyTranslateNoDeadKeysMask,
                    &mut dead_key_state,
                    chars.len(),
                    &mut len,
                    chars.as_mut_ptr(),
                );
                if status == 0 && len == 1 && (chars[0] == 'v' as u16 || chars[0] == 'V' as u16) {
                    found = Some(keycode);
                    break;
                }
            }
        }
        CFRelease(source as CFTypeRef);
        if let Some(keycode) = found {
            PASTE_KEYCODE.store(keycode, Ordering::Relaxed);
        }
        cached_keycode_for_v()
    }
}

enum PostFailure {
    Focus(focus::FocusChange),
    EventCreation,
}

// [自訂] L-80：兩道核對共用同一原生貼上路徑；測試可替換所有系統動作。
enum PasteAttempt<S, E> {
    FocusRejected(focus::FocusChange),
    StageFailed(E),
    Staged(S, Result<(), PostFailure>),
}

fn guarded_paste<S, E, K>(
    mut verify: impl FnMut() -> Result<(), focus::FocusChange>,
    stage: impl FnOnce() -> Result<S, E>,
    create_events: impl FnOnce() -> Result<K, PostFailure>,
    post: impl FnOnce(K),
) -> PasteAttempt<S, E> {
    if let Err(change) = verify() {
        return PasteAttempt::FocusRejected(change);
    }
    let staged = match stage() {
        Ok(staged) => staged,
        Err(error) => return PasteAttempt::StageFailed(error),
    };
    let posted = create_events().and_then(|events| {
        verify().map_err(PostFailure::Focus)?;
        post(events);
        Ok(())
    });
    PasteAttempt::Staged(staged, posted)
}

/// ⌘V 事件带 SayIt 标记，自己的 event tap 会放行、不当成用户按键。
struct PasteEvents {
    down: CfOwned,
    up: CfOwned,
    _source: Option<CfOwned>,
}

impl PasteEvents {
    fn create(keycode: CGKeyCode) -> Result<Self, PostFailure> {
        unsafe {
            let source = CfOwned::from_create(CGEventSourceCreate(kCGEventSourceStateHIDSystemState));
            let source_ptr = source.as_ref().map_or(std::ptr::null_mut(), |source| source.as_ptr().cast_mut());
            let down = CfOwned::from_create(CGEventCreateKeyboardEvent(source_ptr, keycode, true))
                .ok_or(PostFailure::EventCreation)?;
            let up = CfOwned::from_create(CGEventCreateKeyboardEvent(source_ptr, keycode, false))
                .ok_or(PostFailure::EventCreation)?;
            for event in [&down, &up] {
                let ptr = event.as_ptr().cast_mut();
                CGEventSetFlags(ptr, kCGEventFlagMaskCommand);
                CGEventSetIntegerValueField(ptr, kCGEventSourceUserData, crate::keyboard::SAYIT_EVENT_MARKER);
            }
            Ok(Self { down, up, _source: source })
        }
    }

    fn post(self) {
        unsafe {
            CGEventPost(kCGHIDEventTap, self.down.as_ptr().cast_mut());
            std::thread::sleep(Duration::from_millis(8));
            CGEventPost(kCGHIDEventTap, self.up.as_ptr().cast_mut());
        }
    }
}

#[derive(Default)]
struct ClipboardBackup {
    snapshot: Option<appkit::PasteboardSnapshot>,
    previous_text: Option<String>,
}

impl ClipboardBackup {
    // [自訂] L-80：保護關閉時略過所有剪貼簿讀取。
    fn capture(
        protect: bool,
        read_text: impl FnOnce() -> Option<String>,
        read_snapshot: impl FnOnce() -> Option<appkit::PasteboardSnapshot>,
    ) -> Self {
        if !protect {
            return Self::default();
        }
        let previous_text = read_text();
        let snapshot = read_snapshot();
        Self { snapshot, previous_text }
    }

    fn restore(&self, written_count: isize) -> bool {
        if let Some(snapshot) = &self.snapshot {
            appkit::pasteboard_restore(snapshot, true, written_count)
        } else if let Some(text) = &self.previous_text {
            appkit::pasteboard_restore_text(text, written_count)
        } else {
            // 保護關閉時不讀取舊內容，只在剪貼簿仍是本次寫入時清掉暫存文字。
            appkit::pasteboard_clear_if_unchanged(written_count)
        }
    }
}

/// 粘贴后延时还原剪贴板（在独立线程里，不拖慢插字）。
fn schedule_restore(paste_id: u64, written_change_count: isize, backup: ClipboardBackup) {
    let scheduled_at = Instant::now();
    std::thread::spawn(move || {
        std::thread::sleep(RESTORE_DELAY);
        let _transaction = PASTE_TRANSACTION.lock().unwrap_or_else(|error| error.into_inner());
        let elapsed_ms = scheduled_at.elapsed().as_millis();
        let current_generation = PASTE_GENERATION.load(Ordering::Acquire);
        if current_generation != paste_id {
            write_log_line(&format!(
                "[RUST] [inject] clipboard restore skipped pasteId={paste_id} reason=superseded currentGeneration={current_generation} elapsedMs={elapsed_ms}"
            ));
            return;
        }
        // 用户或目标程序已经改写过剪贴板：不拿旧内容覆盖它
        let change_count = appkit::pasteboard_change_count();
        if change_count != written_change_count {
            write_log_line(&format!(
                "[RUST] [inject] clipboard restore skipped pasteId={paste_id} reason=clipboard_changed changeCount={change_count} written={written_change_count} elapsedMs={elapsed_ms}"
            ));
            return;
        }
        let restored = backup.restore(written_change_count);
        let formats = backup.snapshot.as_ref().map_or(0, |snapshot| snapshot.type_count());
        write_log_line(&format!(
            "[RUST] [inject] clipboard restore finished pasteId={paste_id} restored={restored} formats={formats} elapsedMs={elapsed_ms}"
        ));
    });
}

fn focus_failure(change: focus::FocusChange) -> InjectResult {
    let reason = change.code();
    write_log_line(&format!("[RUST] [inject] {reason}"));
    InjectResult {
        ok: false,
        strategy: Some("cmd_v".into()),
        reason: Some(reason.into()),
        detail: None,
        uncertain: false,
    }
}

// [自訂] L-80：實際貼上接入原生焦點核對與剪貼簿服務。
fn inject_with_native_services(
    text: &str, target_pid: i32, restore_clipboard: bool, focus_token: Option<&str>, keycode: CGKeyCode,
) -> InjectResult {
    let _transaction = PASTE_TRANSACTION.lock().unwrap_or_else(|error| error.into_inner());

    // 模拟按键需要「辅助功能」授权；没有授权时 CGEventPost 会被系统静默丢弃。
    if !ax::is_trusted() {
        write_log_line("[RUST] [inject] accessibility not granted");
        return result(false, "cmd_v", Some("accessibility_not_granted"), String::new());
    }

    do_inject(
        restore_clipboard,
        || focus::verify(focus_token, target_pid),
        || {
            let backup = ClipboardBackup::capture(
                restore_clipboard,
                appkit::pasteboard_get_text,
                || appkit::pasteboard_snapshot(CLIPBOARD_SNAPSHOT_BUDGET, CLIPBOARD_SNAPSHOT_MAX_BYTES),
            );
            match appkit::pasteboard_set_text(text, true) {
                Ok(count) => Ok((count, backup)),
                Err(error) => Err((error, backup)),
            }
        },
        || PasteEvents::create(keycode),
        PasteEvents::post,
        ClipboardBackup::restore,
    )
}

// [自訂] L-80：測試和實際貼上共用完整核對、暫存及拒絕還原流程。
fn do_inject<K>(
    restore_clipboard: bool,
    verify: impl FnMut() -> Result<(), focus::FocusChange>,
    stage: impl FnOnce() -> Result<(isize, ClipboardBackup), (appkit::PasteboardWriteError, ClipboardBackup)>,
    create_events: impl FnOnce() -> Result<K, PostFailure>,
    post: impl FnOnce(K),
    rollback: impl FnOnce(&ClipboardBackup, isize) -> bool,
) -> InjectResult {
    let attempt = guarded_paste(verify, stage, create_events, post);
    let (written_count, backup, posted) = match attempt {
        PasteAttempt::FocusRejected(change) => return focus_failure(change),
        PasteAttempt::StageFailed((error, backup)) => {
            if let Some(count) = error.written_count {
                rollback(&backup, count);
            }
            write_log_line("[RUST] [inject] clipboard write failed");
            return result(false, "clipboard", Some("clipboard_blocked"), error.detail.to_owned());
        }
        PasteAttempt::Staged((count, backup), posted) => (count, backup, posted),
    };
    let paste_id = PASTE_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    if let Err(PostFailure::Focus(change)) = posted {
        rollback(&backup, written_count);
        return focus_failure(change);
    }

    if restore_clipboard {
        schedule_restore(paste_id, written_count, backup);
    }
    if posted.is_ok() {
        result(true, "cmd_v", None, format!("pasteId={paste_id}"))
    } else {
        write_log_line("[RUST] [inject] failed to create paste event");
        result(false, "cmd_v", Some("post_event_failed"), format!("pasteId={paste_id}"))
    }
}

// [自訂] L-80：錄音貼上與診斷貼上共用最後一道原生核對。
pub fn inject_recording_text(text: &str, target_pid: i32, restore_clipboard: bool, focus_token: Option<&str>, keycode: CGKeyCode) -> InjectResult {
    if target_pid > 0 {
        return inject_with_native_services(text, target_pid, restore_clipboard, focus_token, keycode);
    }
    // [自訂] L-80：無 PID 時仍走基準版可編輯探測；失效權杖先拒絕。
    if let Some(token) = focus_token {
        if let Err(change) = focus::TARGET.ensure_active(token) {
            return focus_failure(change);
        }
    }
    let ctx = crate::context::capture_context("inject");
    if ctx.pid == 0 {
        write_log_line("[RUST] [inject] no frontmost application");
        return InjectResult {
            ok: false,
            strategy: None,
            reason: Some("no_foreground_window".to_string()),
            detail: None,
            uncertain: false,
        };
    }
    let gate = super::editability_gate(&ctx);
    if !gate.is_editable() {
        return InjectResult {
            ok: false,
            strategy: Some("overlay_fallback".to_string()),
            reason: Some("not_editable".to_string()),
            detail: Some(super::describe_editability(&ctx, gate)),
            uncertain: false,
        };
    }
    inject_with_native_services(text, ctx.pid as i32, restore_clipboard, focus_token, keycode)
}

/// 只写剪贴板（结果卡的「复制」、不可输入时的兜底）。复制是用户要的，正常进剪贴板历史。
/// [自訂] S33／S11：改為 transient（剪貼簿歷史 App 不記錄）＋只限本機（pasteboard_set_text 內處理）。
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    appkit::pasteboard_set_text(text, true).map(|_| ()).map_err(|error| error.detail.into())
}

// ─── 「这里能不能打字」的 mac 判据 ───

/// 明确不是输入区域的 AX 角色（按钮、菜单、列表……）。
fn is_definitely_not_editable(role: &str) -> bool {
    matches!(
        role,
        "AXButton"
            | "AXMenuItem"
            | "AXMenuBar"
            | "AXMenuBarItem"
            | "AXMenu"
            | "AXMenuButton"
            | "AXPopUpButton"
            | "AXTabGroup"
            | "AXToolbar"
            | "AXScrollBar"
            | "AXImage"
            | "AXLink"
            | "AXCheckBox"
            | "AXRadioButton"
            | "AXSlider"
            | "AXDisclosureTriangle"
            | "AXList"
            | "AXTable"
            | "AXOutline"
            | "AXRow"
            | "AXCell"
            | "AXBrowser"
            | "AXDockItem"
    )
}

/// 按 Bundle ID / 可执行文件名放行的常用程序（对应 Windows 的进程名白名单）。
/// Electron / Qt / 自绘界面的程序常常拿不到可用的 AX 信息，只能靠这一层。
const EDITABLE_APP_TOKENS: &[&str] = &[
    // 浏览器
    "chrome", "safari", "firefox", "edgemac", "brave", "thebrowser", "opera", "vivaldi",
    // 编辑器 / IDE
    "vscode", "todesktop", "cursor", "windsurf", "kiro", "jetbrains", "xcode", "sublime", "zed",
    "notion", "obsidian",
    // 聊天 / 办公
    "xinwechat", "wechat", "tencent.qq", "wework", "lark", "feishu", "dingtalk", "slack",
    "teams", "telegram", "discord", "outlook", "microsoft.word", "microsoft.excel",
    "microsoft.powerpoint", "iwork", "apple.notes", "textedit",
    // 终端
    "apple.terminal", "iterm", "warp", "wezterm", "kitty", "ghostty", "alacritty",
];

fn is_allowlisted(bundle_id: &str, process_name: &str) -> bool {
    EDITABLE_APP_TOKENS
        .iter()
        .any(|token| bundle_id.contains(token) || process_name.contains(token))
}

/// macOS 的可编辑判据。比 Windows 乐观：⌘V 打到不能输入的地方只会「嘟」一声，
/// 而误判成不可编辑会让每一次口述都要从兜底卡片手动粘贴；只有拿到明确的否定信号才拒绝。
pub fn editability_gate(ctx: &crate::context::AppContext) -> super::EditableGate {
    use super::EditableGate;
    let role = ctx.control_type.as_str();
    let bundle = ctx.bundle_id.to_lowercase();
    let process = ctx.process_name.to_lowercase();
    let text_role = ax::is_text_role(role) || ctx.focus_class == "AXSecureTextField";

    // Finder（含桌面）：只有重命名框、搜索框这类文本框才是输入目标（对应 explorer 桌面）
    if bundle == "com.apple.finder" && !text_role {
        return EditableGate::ExplorerDesktopReject;
    }
    if text_role && ctx.is_enabled {
        return EditableGate::UiaEditableControl;
    }
    if ctx.has_caret && ctx.is_enabled {
        return EditableGate::Caret;
    }
    if is_allowlisted(&bundle, &process) {
        return EditableGate::ProcessAllowlist;
    }
    // 辅助功能没给出任何信息（未授权、程序不支持无障碍）：不当成否定
    if role.is_empty() {
        return EditableGate::AxOptimistic;
    }
    if is_definitely_not_editable(role) || !ctx.is_enabled {
        return EditableGate::NoSignal;
    }
    EditableGate::AxOptimistic
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::AppContext;
    use crate::inject::EditableGate;

    fn ctx(bundle: &str, role: &str) -> AppContext {
        AppContext {
            bundle_id: bundle.to_string(),
            process_name: bundle.rsplit('.').next().unwrap_or("").to_string(),
            control_type: role.to_string(),
            is_enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn text_fields_are_editable_everywhere() {
        assert_eq!(editability_gate(&ctx("com.example.app", "AXTextArea")), EditableGate::UiaEditableControl);
        assert_eq!(editability_gate(&ctx("com.apple.finder", "AXTextField")), EditableGate::UiaEditableControl);
    }

    #[test]
    fn finder_desktop_and_lists_are_rejected() {
        assert_eq!(editability_gate(&ctx("com.apple.finder", "AXList")), EditableGate::ExplorerDesktopReject);
        assert_eq!(editability_gate(&ctx("com.apple.finder", "")), EditableGate::ExplorerDesktopReject);
    }

    #[test]
    fn known_apps_pass_even_without_ax_details() {
        assert_eq!(editability_gate(&ctx("com.tencent.xinWeChat", "AXGroup")), EditableGate::ProcessAllowlist);
        assert_eq!(editability_gate(&ctx("com.microsoft.VSCode", "AXWindow")), EditableGate::ProcessAllowlist);
        assert_eq!(editability_gate(&ctx("com.googlecode.iterm2", "")), EditableGate::ProcessAllowlist);
    }

    #[test]
    fn buttons_in_unknown_apps_are_rejected_but_missing_ax_is_optimistic() {
        assert_eq!(editability_gate(&ctx("com.example.player", "AXButton")), EditableGate::NoSignal);
        assert_eq!(editability_gate(&ctx("com.example.player", "")), EditableGate::AxOptimistic);
        assert_eq!(editability_gate(&ctx("com.example.player", "AXGroup")), EditableGate::AxOptimistic);
    }

    #[test]
    fn initial_focus_mismatch_neither_stages_text_nor_posts_events() {
        let attempt: PasteAttempt<(), ()> = guarded_paste(
            || Err(focus::FocusChange::Window),
            || panic!("clipboard must not be staged for a changed target"),
            || Ok(()),
            |_| panic!("must not post to a changed target"),
        );
        assert!(matches!(attempt, PasteAttempt::FocusRejected(focus::FocusChange::Window)));
    }

    #[test]
    fn second_check_blocks_post_when_focus_changes_while_events_are_prepared() {
        use std::cell::Cell;
        let focus_changed = Cell::new(false);
        let attempt = guarded_paste(
            || if focus_changed.get() { Err(focus::FocusChange::Element) } else { Ok(()) },
            || Ok::<_, ()>("rollback state"),
            || {
                focus_changed.set(true);
                Ok(())
            },
            |_| panic!("must recheck immediately before posting"),
        );
        assert!(matches!(attempt, PasteAttempt::Staged(
            "rollback state", Err(PostFailure::Focus(focus::FocusChange::Element)),
        )));
    }

    #[test]
    fn missing_clipboard_backup_still_posts_for_an_unchanged_target() {
        use std::cell::Cell;
        let posted = Cell::new(false);
        let attempt = guarded_paste(
            || Ok(()),
            || Ok::<_, ()>(None::<appkit::PasteboardSnapshot>),
            || Ok(()),
            |_| posted.set(true),
        );
        assert!(matches!(attempt, PasteAttempt::Staged(None, Ok(()))));
        assert!(posted.get());
    }

    #[test]
    fn do_inject_runs_both_focus_checks_and_rolls_back_a_late_mismatch() {
        use std::cell::RefCell;
        let calls = RefCell::new(Vec::new());
        let result = do_inject(false,
            || {
                let mut calls = calls.borrow_mut();
                let at_start = calls.is_empty();
                calls.push("verify");
                if at_start { Ok(()) } else { Err(focus::FocusChange::Window) }
            },
            || {
                calls.borrow_mut().push("stage");
                Ok((41, ClipboardBackup::default()))
            },
            || {
                calls.borrow_mut().push("prepare");
                Ok(())
            },
            |_| panic!("no keys should reach the changed window"),
            |_, count| {
                assert_eq!(count, 41);
                calls.borrow_mut().push("rollback");
                true
            },
        );
        assert!(!result.ok);
        assert_eq!(result.reason.as_deref(), Some("window_changed"));
        assert_eq!(*calls.borrow(), ["verify", "stage", "prepare", "verify", "rollback"]);
    }

    #[test]
    fn clipboard_protection_off_never_reads_old_text_or_formats() {
        let backup = ClipboardBackup::capture(false,
            || panic!("old clipboard text must not be read"),
            || panic!("clipboard formats must not be read"),
        );
        assert!(backup.previous_text.is_none());
        assert!(backup.snapshot.is_none());
    }

    #[test]
    fn keycode_for_v_is_a_valid_keycode() {
        assert!(keycode_for_v() < 128);
    }
}
