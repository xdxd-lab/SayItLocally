//! macOS 全域性熱鍵：CGEventTap。
//!
//! 與 Windows 的 `low_level_keyboard_proc` / `low_level_mouse_proc` 一一對應：
//! 事件入口把 macOS 鍵碼翻譯成同一套內部鍵 ID（Windows VK），之後的 PTT 代次、
//! 組合鍵點陣圖、Esc / 卡片快捷鍵閘門、看門狗全部複用 mod.rs 裡的實現。
//!
//! 與 Windows 不同的幾處：
//! - **修飾鍵永遠放行**。macOS 的修飾狀態在 HID 層維護，吞掉 flagsChanged 並不能讓後續
//!   按鍵失去該修飾位，反而讓前台程式看到「沒按下就有修飾位」的錯亂狀態；macOS 也沒有
//!   「單擊 Alt 啟用選單欄」的問題，所以不需要 Windows 那套吞 Alt keyup 的處理。
//! - **不會被系統靜默摘除**：回撥超時或使用者輸入保護時系統會投遞
//!   `kCGEventTapDisabledBy*`，收到後立刻重新啟用（另有看門狗兜底）。
//! - **需要「輔助功能」授權**才能建出能吞鍵的 tap。沒授權時每 2 秒重試一次，
//!   使用者在系統設定裡勾選後不用重啟 SayIt。
//! - 自己注入的事件（插字用的 ⌘V）帶 `kCGEventSourceUserData` 標記，回撥裡直接放行。

#![allow(non_upper_case_globals)]

use super::*;
use crate::platform::macos::ffi::*;
use crate::platform::macos::keycodes;
use std::sync::atomic::AtomicUsize;

/// 我們自己 post 的事件在 `kCGEventSourceUserData` 裡寫這個值（"SAYI"）。
pub(crate) const SAYIT_EVENT_MARKER: i64 = 0x5341_5949;

/// 當前 tap 的 CFMachPortRef（0 = 沒裝上）。
static TAP_PORT: AtomicUsize = AtomicUsize::new(0);
/// tap 執行緒的 CFRunLoopRef（0 = 執行緒沒在跑）。
static TAP_RUN_LOOP: AtomicUsize = AtomicUsize::new(0);
/// 請求 tap 執行緒退出。
static TAP_STOP_REQUESTED: AtomicBool = AtomicBool::new(false);
/// tap 被系統停用後由回撥重新啟用的次數（寫進診斷日誌）。
static TAP_REENABLED_IN_CALLBACK: AtomicU64 = AtomicU64::new(0);

/// event tap 是否已裝上且處於啟用狀態（給許可權/診斷頁用）。
pub fn tap_active() -> bool {
    let port = TAP_PORT.load(Ordering::SeqCst) as CFMachPortRef;
    !port.is_null() && unsafe { CGEventTapIsEnabled(port) }
}

/// 看門狗兜底：tap 存在但被停用時重新啟用。返回是否做了重啟用。
pub fn reenable_tap_if_disabled() -> bool {
    let port = TAP_PORT.load(Ordering::SeqCst) as CFMachPortRef;
    if port.is_null() {
        return false;
    }
    unsafe {
        if CGEventTapIsEnabled(port) {
            return false;
        }
        CGEventTapEnable(port, true);
    }
    true
}

/// 讓 tap 執行緒退出（stop() 呼叫；執行緒會自己解除安裝 tap 並完成收尾）。
pub fn request_tap_thread_stop() {
    TAP_STOP_REQUESTED.store(true, Ordering::SeqCst);
    let run_loop = TAP_RUN_LOOP.load(Ordering::SeqCst) as CFRunLoopRef;
    if !run_loop.is_null() {
        unsafe {
            CFRunLoopStop(run_loop);
            CFRunLoopWakeUp(run_loop);
        }
    }
}

unsafe fn create_tap() -> CFMachPortRef {
    let mask: CGEventMask = (1 << kCGEventKeyDown)
        | (1 << kCGEventKeyUp)
        | (1 << kCGEventFlagsChanged)
        | (1 << kCGEventOtherMouseDown)
        | (1 << kCGEventOtherMouseUp);
    CGEventTapCreate(
        kCGSessionEventTap,
        kCGHeadInsertEventTap,
        kCGEventTapOptionDefault,
        mask,
        tap_callback,
        std::ptr::null_mut(),
    )
}

/// tap 執行緒主體。結構與 Windows 的 hook_thread 相同：action 佇列 → dispatcher 執行緒，
/// 成員看門狗，退出時按 FIFO 排入配對的 up 和 Shutdown。
pub(super) fn hook_thread(state: Arc<HookSharedState>, tx: std::sync::mpsc::Sender<u32>) {
    log::info!(
        "macOS event tap thread starting, ptt_setting={} vk_codes={:?}",
        state.ptt_setting, state.ptt_vk_codes
    );

    let (action_tx, action_rx) = std::sync::mpsc::channel::<HookAction>();
    let watchdog_action_tx = action_tx.clone();
    let member_watchdog = spawn_ptt_member_watchdog(state.clone(), action_tx.clone());
    let dispatcher_thread = spawn_dispatcher(state.clone(), action_rx, watchdog_action_tx);

    HOOK_STATE.with(|s| *s.borrow_mut() = Some(state.clone()));
    HOOK_ACTION_TX.with(|s| *s.borrow_mut() = Some(action_tx));

    TAP_STOP_REQUESTED.store(false, Ordering::SeqCst);
    let run_loop = unsafe { CFRunLoopGetCurrent() };
    TAP_RUN_LOOP.store(run_loop as usize, Ordering::SeqCst);
    // mac 上沒有 Windows 那樣的執行緒訊息 id，用 1 佔位，告訴 start() 執行緒已經起來了。
    let _ = tx.send(1);

    // 等授權：沒有「輔助功能」授權時 CGEventTapCreate 返回 NULL。
    let mut waiting_logged = false;
    let port = loop {
        if TAP_STOP_REQUESTED.load(Ordering::SeqCst) {
            break std::ptr::null_mut();
        }
        let port = unsafe { create_tap() };
        if !port.is_null() {
            break port;
        }
        if !waiting_logged {
            waiting_logged = true;
            crate::commands::system::write_log_line(&format!(
                "[ptt-lifecycle] event tap unavailable (accessibility_trusted={}); retrying every 2s until permission is granted",
                crate::platform::macos::ax::is_trusted(),
            ));
        }
        for _ in 0..20 {
            if TAP_STOP_REQUESTED.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(100));
        }
    };

    if !port.is_null() {
        unsafe {
            let source = CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0);
            CFRunLoopAddSource(run_loop, source, kCFRunLoopCommonModes);
            CGEventTapEnable(port, true);
            TAP_PORT.store(port as usize, Ordering::SeqCst);
            crate::commands::system::write_log_line(&format!(
                "[ptt-lifecycle] event tap installed (waited_for_permission={})",
                waiting_logged
            ));

            while !TAP_STOP_REQUESTED.load(Ordering::SeqCst) {
                CFRunLoopRunInMode(kCFRunLoopDefaultMode, 1.0, 0);
            }

            TAP_PORT.store(0, Ordering::SeqCst);
            CGEventTapEnable(port, false);
            CFRunLoopRemoveSource(run_loop, source, kCFRunLoopCommonModes);
            CFMachPortInvalidate(port);
            CFRelease(source as CFTypeRef);
            CFRelease(port as CFTypeRef);
        }
        log::info!("macOS event tap removed");
    }
    TAP_RUN_LOOP.store(0, Ordering::SeqCst);

    // 收尾與 Windows 相同：先等成員看門狗退出，再用回撥同一個 sender 排入配對 up 和 Shutdown。
    state.ptt_hook_alive.store(false, Ordering::SeqCst);
    if let Some(handle) = member_watchdog {
        if handle.join().is_err() {
            crate::commands::system::write_log_line("[ptt-lifecycle] member watchdog panicked while stopping");
        }
    }
    HOOK_ACTION_TX.with(|s| {
        if let Some(sender) = s.borrow_mut().take() {
            clear_ptt_members(&state);
            let vk = state.ptt_vk_codes.first().copied().unwrap_or(0);
            let _ = queue_ptt_release(&sender, &state.ptt_active_generation, None, vk, "manager_stop");
            let _ = sender.send(HookAction::Shutdown);
        }
    });
    HOOK_STATE.with(|s| *s.borrow_mut() = None);
    if dispatcher_thread.join().is_err() {
        crate::commands::system::write_log_line("[ptt-lifecycle] dispatcher thread panicked while stopping");
    }
}

// ─── 物理按鍵狀態（給看門狗、WebView 回退用）───

/// 這個內部鍵此刻是否按著。修飾鍵看修飾標誌（區分左右），普通鍵看鍵狀態，
/// 滑鼠鍵看按鈕狀態（mac 上能查到，Windows 上查不到）。
///
/// HID 狀態表只記真實硬體；遠端控制（向日葵等）和自動化工具用 CGEventPost 注入的按鍵
/// 只記在會話狀態表裡。只看 HID 的話，遠端按住右 ⌥ 說話時看門狗約 2 秒後就會判成
/// 「已鬆開」把錄音掐斷。兩張表都看：本機鍵盤兩者一致；Windows 的 GetAsyncKeyState
/// 本來也包含注入的按鍵。
pub(super) unsafe fn is_physically_down(vk: u32) -> bool {
    is_down_in(kCGEventSourceStateHIDSystemState, vk)
        || is_down_in(kCGEventSourceStateCombinedSessionState, vk)
}

unsafe fn is_down_in(state: CGEventSourceStateID, vk: u32) -> bool {
    if let Some(button) = keycodes::mouse_button_for_vk(vk) {
        return CGEventSourceButtonState(state, button);
    }
    if keycodes::is_modifier_vk(vk) {
        let flags = CGEventSourceFlagsState(state);
        return keycodes::modifier_vk_is_down(vk, flags).unwrap_or(false);
    }
    match keycodes::keycode_for_vk(vk) {
        Some(keycode) => CGEventSourceKeyState(state, keycode),
        None => false,
    }
}

/// 單鍵熱鍵按下時是否帶著額外修飾鍵（帶了就整條放行，對應 Windows 的 single_key_blocked_by_modifiers）。
fn single_key_blocked_by_modifiers(vk: u32, flags: CGEventFlags) -> bool {
    single_key_requires_bare_press(vk) && flags & keycodes::BLOCKING_MODIFIER_MASK != 0
}

/// 這一刻是不是**只有** ⌘ 的 ⌘C（卡片「複製」快捷鍵）。⌘⇧C、⌘⌥C 等都不算。
fn only_command_is_down(flags: CGEventFlags) -> bool {
    flags & kCGEventFlagMaskCommand != 0
        && flags & (kCGEventFlagMaskShift | kCGEventFlagMaskControl | kCGEventFlagMaskAlternate) == 0
}

fn send_action(action: HookAction) {
    HOOK_ACTION_TX.with(|tx| {
        if let Some(sender) = tx.borrow().as_ref() {
            if sender.send(action).is_err() {
                TRY_SEND_FAIL_COUNT.fetch_add(1, Ordering::Relaxed);
            }
        }
    });
}

fn queue_release(state: &HookSharedState, vk: u32, reason: &'static str) {
    HOOK_ACTION_TX.with(|tx| {
        if let Some(sender) = tx.borrow().as_ref() {
            let _ = queue_ptt_release(sender, &state.ptt_active_generation, None, vk, reason);
        }
    });
}

/// PTT 成員按下：記帳，湊齊整個組合時開始一次新的 PTT 代次。
fn press_member(state: &HookSharedState, member_index: usize, vk: u32) {
    let member_bit = 1_u64 << member_index;
    state.ptt_last_down_ms[member_index].store(now_ms(), Ordering::SeqCst);
    let became_complete = press_ptt_member(&state.ptt_pressed_mask, member_index, state.ptt_full_mask);
    if !became_complete || state.hands_free_active.load(Ordering::SeqCst) {
        return;
    }
    if let Some(gen) = begin_ptt_press(&state.ptt_active_generation, &state.ptt_generation) {
        HOOK_ACTION_TX.with(|tx| {
            if let Some(sender) = tx.borrow().as_ref() {
                if sender.send(HookAction::PttDown { vk, gen }).is_err() {
                    let _ = release_ptt_member(&state.ptt_pressed_mask, member_index);
                    state.ptt_consumed_down_mask.fetch_and(!member_bit, Ordering::SeqCst);
                    cancel_ptt_press_start(&state.ptt_active_generation, gen);
                    TRY_SEND_FAIL_COUNT.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
    }
}

// ─── 系統快捷鍵接不住的組合鍵 ───
//
// 免持、AI 整理開關、潤色模式切換的組合鍵平時走系統的 RegisterEventHotKey
// （tauri-plugin-global-shortcut，見 commands/shortcuts.rs）：「安全輸入」期間（密碼框等）
// 它照樣有效，event tap 卻收不到按鍵，所以能交給系統的都交給系統。
//
// 但從 macOS 15 起，修飾鍵只有 ⌥、只有 ⇧ 或只有 ⌥⇧ 的組合註冊「成功」卻永遠不觸發。
// Windows 上 Alt 組合很常見，從 Windows 匯入的配置也會帶過來。這一類改由 event tap 匹配：
// 命中就吞掉主鍵的按下、自動重複和抬起（修飾鍵照常放行），再發出和系統快捷鍵相同的事件。

/// 組合鍵觸發的動作，與 commands/shortcuts.rs 裡系統快捷鍵發出的事件一一對應。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TapComboAction {
    HandsFree,
    AiToggle,
    Preset(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TapCombo {
    /// 必須按著、而且只能按著的修飾鍵（⌘ ⌃ ⌥ ⇧ 的 kCGEventFlagMask* 組合）
    modifiers: CGEventFlags,
    /// 主鍵（內部鍵 ID，沿用 Windows VK）
    vk: u32,
    action: TapComboAction,
}

static TAP_COMBOS: std::sync::Mutex<Vec<TapCombo>> = std::sync::Mutex::new(Vec::new());
/// 正被吞掉的組合鍵主鍵（0 = 沒有）。它的自動重複和抬起也要吞掉，不能漏給前台程式。
static TAP_COMBO_HELD_VK: AtomicU32 = AtomicU32::new(0);

/// 組合鍵比對的修飾鍵：只看這四個，Caps Lock、Fn、小鍵盤這些標誌不看。
const COMBO_MODIFIER_MASK: CGEventFlags =
    kCGEventFlagMaskCommand | kCGEventFlagMaskControl | kCGEventFlagMaskAlternate | kCGEventFlagMaskShift;

/// 把 accelerator（如 `Alt+Shift+D`）換成交給 event tap 的組合鍵。
///
/// 只接系統快捷鍵在 macOS 15 上接不住的那一類：修飾鍵只有 ⌥ / ⇧。帶 ⌘ 或 ⌃ 的、解析不了的、
/// 主鍵在 Mac 鍵盤上沒有的，返回 None，照舊交給系統快捷鍵。
pub(crate) fn tap_combo_for_accelerator(accelerator: &str, action: TapComboAction) -> Option<TapCombo> {
    use tauri_plugin_global_shortcut::{Modifiers, Shortcut};
    let shortcut: Shortcut = accelerator.parse().ok()?;
    if shortcut.mods.intersects(Modifiers::SUPER | Modifiers::META | Modifiers::CONTROL) {
        return None;
    }
    let mut modifiers: CGEventFlags = 0;
    if shortcut.mods.contains(Modifiers::ALT) {
        modifiers |= kCGEventFlagMaskAlternate;
    }
    if shortcut.mods.contains(Modifiers::SHIFT) {
        modifiers |= kCGEventFlagMaskShift;
    }
    if modifiers == 0 {
        return None;
    }
    let vk = vk_for_combo_key(&shortcut.key.to_string())?;
    keycodes::keycode_for_vk(vk)?;
    Some(TapCombo { modifiers, vk, action })
}

/// 組合鍵主鍵（DOM code 寫法，如 `KeyD`、`F5`、`Comma`）→ 內部 VK。
/// 字母、數字、F 鍵、方向鍵等沿用按住說話的對映，再補上標點和小鍵盤。
fn vk_for_combo_key(code: &str) -> Option<u32> {
    if let Some(vk) = vk_for_ptt_code(code) {
        return Some(vk);
    }
    let vk = match code {
        "Semicolon" => 0xBA,
        "Equal" => 0xBB,
        "Comma" => 0xBC,
        "Minus" => 0xBD,
        "Period" => 0xBE,
        "Slash" => 0xBF,
        "Backquote" => 0xC0,
        "BracketLeft" => 0xDB,
        "Backslash" => 0xDC,
        "BracketRight" => 0xDD,
        "Quote" => 0xDE,
        "NumpadMultiply" => 0x6A,
        "NumpadAdd" => 0x6B,
        "NumpadSubtract" => 0x6D,
        "NumpadDecimal" => 0x6E,
        "NumpadDivide" => 0x6F,
        _ => {
            let digit = code.strip_prefix("Numpad")?;
            if digit.len() != 1 {
                return None;
            }
            0x60 + digit.parse::<u32>().ok()?
        }
    };
    Some(vk)
}

/// 換一套交給 event tap 的組合鍵（每次重新註冊全域性快捷鍵時呼叫，空表 = 不接任何組合）。
pub(crate) fn set_tap_combos(combos: Vec<TapCombo>) {
    if let Ok(mut current) = TAP_COMBOS.lock() {
        *current = combos;
    }
    TAP_COMBO_HELD_VK.store(0, Ordering::SeqCst);
}

/// 主鍵按下時，修飾鍵必須和登記的完全一致：多按了 ⌘ 或少按了 ⇧ 都不算。
fn match_tap_combo(combos: &[TapCombo], vk: u32, flags: CGEventFlags) -> Option<TapComboAction> {
    let pressed = flags & COMBO_MODIFIER_MASK;
    combos
        .iter()
        .find(|combo| combo.vk == vk && combo.modifiers == pressed)
        .map(|combo| combo.action.clone())
}

/// 一次非修飾鍵事件是不是登記的組合鍵。返回 true = 吞掉。
fn handle_tap_combo(vk: u32, is_kdown: bool, is_kup: bool, flags: CGEventFlags) -> bool {
    if is_kup {
        return TAP_COMBO_HELD_VK
            .compare_exchange(vk, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
    }
    if !is_kdown {
        return false;
    }
    let action = match TAP_COMBOS.lock() {
        Ok(combos) if !combos.is_empty() => match_tap_combo(&combos, vk, flags),
        _ => None,
    };
    let Some(action) = action else {
        // 同一個鍵這次不帶組合的修飾鍵：說明錯過了上次的抬起（比如 tap 剛好被系統停用），
        // 把「按住中」清掉，免得這個鍵接下來的正常按鍵被當成自動重複吞掉
        let _ = TAP_COMBO_HELD_VK.compare_exchange(vk, 0, Ordering::SeqCst, Ordering::SeqCst);
        return false;
    };
    // 自動重複：同一次按住只觸發一次
    if TAP_COMBO_HELD_VK.swap(vk, Ordering::SeqCst) == vk {
        return true;
    }
    send_action(match action {
        TapComboAction::HandsFree => HookAction::HfToggle { vk },
        TapComboAction::AiToggle => HookAction::AiToggle { vk },
        TapComboAction::Preset(preset_id) => HookAction::PresetSwitch { preset_id },
    });
    true
}

// ─── 回撥 ───

unsafe extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    _user_info: *mut std::ffi::c_void,
) -> CGEventRef {
    let _timer = CallbackTimer(std::time::Instant::now());
    LAST_CALLBACK_MS.store(now_ms(), Ordering::Relaxed);

    if event_type == kCGEventTapDisabledByTimeout || event_type == kCGEventTapDisabledByUserInput {
        let port = TAP_PORT.load(Ordering::SeqCst) as CFMachPortRef;
        if !port.is_null() {
            CGEventTapEnable(port, true);
        }
        let count = TAP_REENABLED_IN_CALLBACK.fetch_add(1, Ordering::SeqCst) + 1;
        send_action(HookAction::Diag {
            vk: 0,
            msg_name: if event_type == kCGEventTapDisabledByTimeout {
                "tap-disabled-by-timeout-reenabled"
            } else {
                "tap-disabled-by-user-input-reenabled"
            },
            flags: count as u32,
            scan_code: 0,
        });
        return event;
    }
    if event.is_null() {
        return event;
    }
    // 自己 post 的事件（插字的 ⌘V）不是使用者按鍵
    if CGEventGetIntegerValueField(event, kCGEventSourceUserData) == SAYIT_EVENT_MARKER {
        return event;
    }

    let consumed = match event_type {
        kCGEventKeyDown | kCGEventKeyUp | kCGEventFlagsChanged => handle_key_event(event_type, event),
        kCGEventOtherMouseDown | kCGEventOtherMouseUp => handle_mouse_event(event_type, event),
        _ => false,
    };
    if consumed {
        std::ptr::null_mut()
    } else {
        event
    }
}

unsafe fn handle_key_event(event_type: CGEventType, event: CGEventRef) -> bool {
    let keycode = CGEventGetIntegerValueField(event, kCGKeyboardEventKeycode) as CGKeyCode;
    let flags = CGEventGetFlags(event);
    if event_type == kCGEventFlagsChanged {
        // Caps Lock 和未知修飾鍵不在表裡：直接放行
        let Some(vk) = keycodes::modifier_vk_for_keycode(keycode) else { return false };
        let down = keycodes::modifier_is_down(keycode, flags);
        process_key(vk, down, !down, true, flags);
        // 修飾鍵永遠放行（見檔案頭）
        false
    } else {
        let Some(vk) = keycodes::vk_for_keycode(keycode) else { return false };
        process_key(vk, event_type == kCGEventKeyDown, event_type == kCGEventKeyUp, false, flags)
    }
}

/// 一個已歸一化的按鍵事件。返回 true = 吞掉。邏輯與 Windows 的 `low_level_keyboard_proc`
/// 逐段對應（順序也一樣：錄製捕獲 → Esc → 卡片 ⌘C → PTT / 免持 / AI 開關）。
fn process_key(vk: u32, is_kdown: bool, is_kup: bool, is_modifier: bool, flags: CGEventFlags) -> bool {
    // ── 設定頁錄製快捷鍵：先結束仍活動的 PTT，然後一律放行給 webview 去繫結 ──
    if SHORTCUT_CAPTURE.load(Ordering::SeqCst) {
        HOOK_STATE.with(|s| {
            if let Some(state) = s.borrow().as_ref() {
                clear_ptt_members(state);
                if let Some(&ptt_vk) = state.ptt_vk_codes.first() {
                    queue_release(state, ptt_vk, "shortcut_capture");
                }
                state.hf_key_down.store(false, Ordering::SeqCst);
            }
        });
        // WebKit 不給 Fn 發 DOM 事件，由這裡回報（複用 mouse-shortcut-captured 通道）
        if vk == keycodes::VK_FN && is_kdown {
            SHORTCUT_CAPTURE.store(false, Ordering::SeqCst);
            send_action(HookAction::MouseCaptured { vk });
        }
        return false;
    }

    // ── 懸浮窗不聚焦：錄音中/處理中/結果卡的 Esc 在這裡全域性捕獲 ──
    const VK_ESCAPE: u32 = 0x1B;
    if vk == VK_ESCAPE {
        if is_kdown {
            if ESCAPE_KEY_DOWN.load(Ordering::SeqCst) {
                return true;
            }
            let (mode, token) = active_escape_action();
            if mode != ESCAPE_MODE_OFF {
                if !ESCAPE_KEY_DOWN.swap(true, Ordering::SeqCst) {
                    send_action(HookAction::Escape { mode, token });
                }
                return true;
            }
        }
        if is_kup && ESCAPE_KEY_DOWN.swap(false, Ordering::SeqCst) {
            return true;
        }
    }

    // ── 結果卡的 ⌘C（Windows 是 Ctrl+C）：只在卡片可見、且前台還是彈卡時那個程式時接管 ──
    const VK_C: u32 = 0x43;
    if vk == VK_C {
        if is_kdown {
            if CARD_HOTKEY_C_DOWN.load(Ordering::SeqCst) {
                return true;
            }
            let (mask, token) = active_card_hotkeys();
            if mask & CARD_HOTKEY_COPY != 0 && only_command_is_down(flags) && card_hotkey_foreground_matches() {
                CARD_HOTKEY_C_DOWN.store(true, Ordering::SeqCst);
                send_action(HookAction::CardHotkey { action: CARD_HOTKEY_COPY, token });
                return true;
            }
        }
        if is_kup && CARD_HOTKEY_C_DOWN.swap(false, Ordering::SeqCst) {
            return true;
        }
    }

    // ── 系統快捷鍵接不住、交給 event tap 的組合鍵（只帶 ⌥ / ⇧，見 TAP_COMBOS）──
    if !is_modifier && handle_tap_combo(vk, is_kdown, is_kup, flags) {
        return true;
    }

    let mut consumed = false;
    HOOK_STATE.with(|s| {
        let borrowed = s.borrow();
        let Some(state) = borrowed.as_ref() else { return };

        let ptt_member = ptt_member_index(&state.ptt_vk_codes, vk);
        let is_ptt_key = ptt_member.is_some();
        let is_hf_key = !state.hf_vk_codes.is_empty() && state.hf_vk_codes.contains(&vk) && !is_ptt_key;
        let is_ai_toggle_key = !state.ai_toggle_vk_codes.is_empty()
            && state.ai_toggle_vk_codes.contains(&vk)
            && !is_ptt_key
            && !is_hf_key;

        // 單鍵熱鍵必須裸按才算命中（只看 keydown；keyup 一律照常處理，否則會停不下來）
        let modifiers_block_down = single_key_blocked_by_modifiers(vk, flags);

        let should_log_hotkey_down = is_kdown
            && ((ptt_member.is_some_and(|index| state.ptt_pressed_mask.load(Ordering::SeqCst) & (1_u64 << index) == 0)
                && !state.hands_free_active.load(Ordering::SeqCst))
                || (is_hf_key && !state.hf_key_down.load(Ordering::SeqCst)));
        if should_log_hotkey_down {
            send_action(HookAction::Diag {
                vk,
                msg_name: if modifiers_block_down { "hotkey-down-with-modifier" } else { "hotkey-down" },
                flags: flags as u32,
                scan_code: 0,
            });
        }

        if let Some(member_index) = ptt_member {
            if is_kdown {
                unsafe { reconcile_stale_ptt_members_before_down(state, member_index) };
            }
            let member_bit = 1_u64 << member_index;
            let is_combo = state.ptt_vk_codes.len() > 1;
            let is_pure_modifier_combo = is_combo && state.ptt_modifier_mask == state.ptt_full_mask;
            let is_main_key = state.ptt_modifier_mask & member_bit == 0;
            let bare_press_violated = !is_combo && modifiers_block_down;

            if !is_combo {
                if is_kdown && !bare_press_violated {
                    consumed = true;
                }
            } else if !is_pure_modifier_combo && is_main_key {
                if is_kdown {
                    let consume_this_press = should_consume_combo_main_down(
                        state.ptt_pressed_mask.load(Ordering::SeqCst),
                        state.ptt_consumed_down_mask.load(Ordering::SeqCst),
                        member_bit,
                        state.ptt_modifier_mask,
                    );
                    if consume_this_press {
                        state.ptt_consumed_down_mask.fetch_or(member_bit, Ordering::SeqCst);
                        consumed = true;
                    }
                }
                if is_kup && state.ptt_consumed_down_mask.fetch_and(!member_bit, Ordering::SeqCst) & member_bit != 0 {
                    consumed = true;
                }
            }

            if is_kdown && !bare_press_violated {
                press_member(state, member_index, vk);
            }
            if is_kup && release_ptt_member(&state.ptt_pressed_mask, member_index) {
                queue_release(state, vk, "member_keyup");
            }
        }

        // 免持：首個按下立即觸發，repeat 忽略，抬起重新佈防
        if is_hf_key {
            if is_kdown && !modifiers_block_down {
                consumed = true;
                if begin_hf_press(&state.hf_key_down) {
                    send_action(HookAction::HfToggle { vk });
                }
            }
            if is_kup && end_hf_press(&state.hf_key_down) {
                consumed = true;
            }
        }

        if is_ai_toggle_key {
            if is_kdown && !modifiers_block_down {
                consumed = true;
                if begin_hf_press(&state.ai_toggle_key_down) {
                    send_action(HookAction::AiToggle { vk });
                }
            }
            if is_kup && end_hf_press(&state.ai_toggle_key_down) {
                consumed = true;
            }
        }
    });

    consumed && !is_modifier
}

/// 滑鼠中鍵 / 側鍵。邏輯與 Windows 的 `low_level_mouse_proc` 對應。
unsafe fn handle_mouse_event(event_type: CGEventType, event: CGEventRef) -> bool {
    let button = CGEventGetIntegerValueField(event, kCGMouseEventButtonNumber);
    let Some(vk) = keycodes::vk_for_mouse_button(button) else { return false };
    let is_down = event_type == kCGEventOtherMouseDown;
    let is_up = event_type == kCGEventOtherMouseUp;

    // 錄製捕獲：吞掉並回報，避免 webview 把側鍵當「後退」導航
    if SHORTCUT_CAPTURE.load(Ordering::SeqCst) {
        if is_down {
            SHORTCUT_CAPTURE.store(false, Ordering::SeqCst);
            CONSUME_XUP_VK.store(vk, Ordering::SeqCst);
            send_action(HookAction::MouseCaptured { vk });
        }
        return true;
    }
    if is_up && CONSUME_XUP_VK.load(Ordering::SeqCst) == vk {
        CONSUME_XUP_VK.store(0, Ordering::SeqCst);
        return true;
    }

    let mut consumed = false;
    HOOK_STATE.with(|s| {
        let borrowed = s.borrow();
        let Some(state) = borrowed.as_ref() else { return };
        let ptt_member = ptt_member_index(&state.ptt_vk_codes, vk);
        let is_ptt_key = ptt_member.is_some();
        let is_hf_key = !state.hf_vk_codes.is_empty() && state.hf_vk_codes.contains(&vk) && !is_ptt_key;
        let is_ai_toggle_key = !state.ai_toggle_vk_codes.is_empty()
            && state.ai_toggle_vk_codes.contains(&vk)
            && !is_ptt_key
            && !is_hf_key;

        if let Some(member_index) = ptt_member {
            consumed = true;
            if is_down {
                press_member(state, member_index, vk);
            }
            if is_up && release_ptt_member(&state.ptt_pressed_mask, member_index) {
                queue_release(state, vk, "keyup");
            }
        }
        if is_hf_key {
            if is_down {
                consumed = true;
                if begin_hf_press(&state.hf_key_down) {
                    send_action(HookAction::HfToggle { vk });
                }
            }
            if is_up && end_hf_press(&state.hf_key_down) {
                consumed = true;
            }
        }
        if is_ai_toggle_key {
            if is_down {
                consumed = true;
                if begin_hf_press(&state.ai_toggle_key_down) {
                    send_action(HookAction::AiToggle { vk });
                }
            }
            if is_up && end_hf_press(&state.ai_toggle_key_down) {
                consumed = true;
            }
        }
    });
    consumed
}

/// 當前 macOS 的「最後一次輸入距今多少毫秒」（對應 GetLastInputInfo）。
#[allow(dead_code)]
pub(super) fn last_input_age_ms() -> Option<i64> {
    let secs = unsafe { CGEventSourceSecondsSinceLastEventType(kCGEventSourceStateHIDSystemState, kCGAnyInputEventType) };
    (secs.is_finite() && secs >= 0.0).then(|| (secs * 1000.0) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_press_blocks_main_keys_with_modifiers_but_not_modifier_hotkeys() {
        // F5 帶 ⌥：放行
        assert!(single_key_blocked_by_modifiers(0x74, kCGEventFlagMaskAlternate));
        // F5 只帶 Fn 位（功能鍵自帶）：不算修飾
        assert!(!single_key_blocked_by_modifiers(0x74, kCGEventFlagMaskSecondaryFn));
        // 右 ⌥ 本身是熱鍵：不套裸按規則
        assert!(!single_key_blocked_by_modifiers(0xA5, kCGEventFlagMaskAlternate));
    }

    #[test]
    fn card_copy_requires_command_only() {
        assert!(only_command_is_down(kCGEventFlagMaskCommand));
        assert!(!only_command_is_down(kCGEventFlagMaskCommand | kCGEventFlagMaskShift));
        assert!(!only_command_is_down(kCGEventFlagMaskControl));
    }

    #[test]
    fn last_input_age_is_reported() {
        assert!(last_input_age_ms().is_some());
    }

    #[test]
    fn option_and_shift_only_combos_go_to_the_event_tap() {
        let combo = tap_combo_for_accelerator("Alt+Shift+D", TapComboAction::HandsFree).unwrap();
        assert_eq!(combo.modifiers, kCGEventFlagMaskAlternate | kCGEventFlagMaskShift);
        assert_eq!(combo.vk, 0x44);
        assert_eq!(combo.action, TapComboAction::HandsFree);
        let f5 = tap_combo_for_accelerator("Shift+F5", TapComboAction::AiToggle).unwrap();
        assert_eq!((f5.modifiers, f5.vk), (kCGEventFlagMaskShift, 0x74));
        let comma = tap_combo_for_accelerator("Alt+Shift+Comma", TapComboAction::Preset("p".into())).unwrap();
        assert_eq!(comma.vk, 0xBC);
        assert_eq!(tap_combo_for_accelerator("Alt+Shift+Numpad3", TapComboAction::HandsFree).map(|c| c.vk), Some(0x63));
        assert_eq!(tap_combo_for_accelerator("Option+Shift+1", TapComboAction::HandsFree).map(|c| c.vk), Some(0x31));
    }

    #[test]
    fn combos_with_command_or_control_stay_with_the_system_hotkey() {
        for accel in ["Control+Alt+D", "CommandOrControl+Shift+D", "Command+Alt+K", "Control+F5"] {
            assert!(tap_combo_for_accelerator(accel, TapComboAction::HandsFree).is_none(), "{accel}");
        }
        // 解析不了的、Mac 鍵盤上沒有的、根本不是組合鍵的：也交回系統快捷鍵那邊處理
        for accel in ["Alt+Shift+Return", "Alt+F24", "Alt+", "D", "Alt+Shift+Î"] {
            assert!(tap_combo_for_accelerator(accel, TapComboAction::HandsFree).is_none(), "{accel}");
        }
    }

    #[test]
    fn tap_combo_needs_exactly_the_registered_modifiers() {
        let combos = vec![tap_combo_for_accelerator("Alt+Shift+D", TapComboAction::HandsFree).unwrap()];
        let opt_shift = kCGEventFlagMaskAlternate | kCGEventFlagMaskShift;
        assert_eq!(match_tap_combo(&combos, 0x44, opt_shift), Some(TapComboAction::HandsFree));
        // Caps Lock、Fn 這些標誌不影響
        assert_eq!(
            match_tap_combo(&combos, 0x44, opt_shift | kCGEventFlagMaskAlphaShift | kCGEventFlagMaskSecondaryFn),
            Some(TapComboAction::HandsFree)
        );
        // 多按了 ⌘、少按了 ⇧、換了主鍵，都不算
        assert_eq!(match_tap_combo(&combos, 0x44, opt_shift | kCGEventFlagMaskCommand), None);
        assert_eq!(match_tap_combo(&combos, 0x44, kCGEventFlagMaskAlternate), None);
        assert_eq!(match_tap_combo(&combos, 0x45, opt_shift), None);
    }

    #[test]
    fn tap_combo_fires_once_and_swallows_its_repeats_and_key_up_only() {
        let (tx, rx) = std::sync::mpsc::channel();
        HOOK_ACTION_TX.with(|cell| *cell.borrow_mut() = Some(tx));
        set_tap_combos(vec![
            tap_combo_for_accelerator("Alt+Shift+D", TapComboAction::HandsFree).unwrap(),
            tap_combo_for_accelerator("Alt+Shift+2", TapComboAction::Preset("email".into())).unwrap(),
        ]);
        let opt_shift = kCGEventFlagMaskAlternate | kCGEventFlagMaskShift;

        assert!(handle_tap_combo(0x44, true, false, opt_shift)); // 按下：觸發併吞掉
        assert!(handle_tap_combo(0x44, true, false, opt_shift)); // 自動重複：吞掉，不再觸發
        assert!(handle_tap_combo(0x44, false, true, opt_shift)); // 抬起：吞掉
        assert!(!handle_tap_combo(0x44, true, false, 0)); // 單按 D：照常打字
        assert!(!handle_tap_combo(0x44, false, true, 0));
        assert!(!handle_tap_combo(0x45, true, false, opt_shift)); // ⌥⇧E 沒登記
        assert!(handle_tap_combo(0x32, true, false, opt_shift)); // ⌥⇧2 → 切換潤色模式
        assert!(handle_tap_combo(0x32, false, true, 0)); // 先松修飾鍵再松 2：抬起照樣吞掉

        // 錯過了抬起：同一個鍵不帶修飾鍵再按，就不算「按住中」了
        assert!(handle_tap_combo(0x44, true, false, opt_shift));
        assert!(!handle_tap_combo(0x44, true, false, 0));
        assert!(!handle_tap_combo(0x44, false, true, 0));

        let actions: Vec<HookAction> = rx.try_iter().collect();
        assert_eq!(actions.len(), 3, "按下三次組合鍵，觸發三次");
        assert!(matches!(actions[0], HookAction::HfToggle { vk: 0x44 }));
        assert!(matches!(&actions[1], HookAction::PresetSwitch { preset_id } if preset_id.as_str() == "email"));
        assert!(matches!(actions[2], HookAction::HfToggle { vk: 0x44 }));

        set_tap_combos(Vec::new());
        HOOK_ACTION_TX.with(|cell| *cell.borrow_mut() = None);
    }
}
