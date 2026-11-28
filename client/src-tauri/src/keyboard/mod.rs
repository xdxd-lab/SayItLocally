//! Global keyboard hook for PTT (Push-to-Talk) functionality.
//!
//! Uses Win32 SetWindowsHookExW(WH_KEYBOARD_LL) to capture key events globally.
//! Runs the message loop on a dedicated thread.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::{tap_active, SAYIT_EVENT_MARKER};
#[cfg(target_os = "macos")]
pub(crate) use macos::{set_tap_combos, tap_combo_for_accelerator, TapCombo, TapComboAction};

use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicIsize, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use tauri::{AppHandle, Emitter};

// ── 健康監測埋點（懸浮窗/熱鍵間歇性失效排查）──
// 這些是程序級靜態量，與具體某次 hook 例項無關，用於在鉤子"悄悄失效"時留下痕跡：
// - LAST_CALLBACK_MS：鉤子回撥最近一次被 Windows 呼叫的時間（任意按鍵都會更新，
//   不限於 PTT 鍵）。如果這個值長時間不再更新但使用者在正常打字，說明鉤子已被
//   系統摘除（WH_KEYBOARD_LL 回撥超時/異常會被靜默移除，是本次排查的頭號嫌疑）。
// - DISPATCHER_ALIVE：dispatcher 執行緒是否仍在執行；執行緒 panic 或 recv() 返回
//   Err（傳送端全部丟棄）都會導致這個迴圈退出。
// - TRY_SEND_FAIL_COUNT：鉤子回撥向 dispatcher 傳送訊息失敗的累計次數——一旦
//   >0 就說明 dispatcher 側出了問題（佇列滿或已退出），即使 hook 本身還活著，
//   PTT 事件也發不出去，前端永遠等不到 ptt-down。
static LAST_CALLBACK_MS: AtomicI64 = AtomicI64::new(0);
static DISPATCHER_ALIVE: AtomicBool = AtomicBool::new(false);
static TRY_SEND_FAIL_COUNT: AtomicU64 = AtomicU64::new(0);
/// 鉤子回撥實際執行耗時的最大觀測值（微秒），用於確認是否接近/超過 Windows 的
/// ~200ms 靜默摘除閾值。正常情況應恆定在幾十微秒級別。
static MAX_CALLBACK_DURATION_US: AtomicU64 = AtomicU64::new(0);
/// 映象 `KeyboardHookManager.running`——獨立看門狗執行緒沒有該例項的引用，
/// 用全域性靜態量同步一份供健康快照讀取。
static HOOK_RUNNING: AtomicBool = AtomicBool::new(false);
/// reconfigure() 累計呼叫次數，用於確認"過一段時間失效"是否與設定變更相關。
static RECONFIGURE_COUNT: AtomicU64 = AtomicU64::new(0);

// ── "錄製捕獲"模式 ──
// 設定頁錄製快捷鍵時，前端會開啟這個開關。開啟後：
//  - 滑鼠鉤子把下一個側鍵(XBUTTON)按下吞掉並回報，webview 不會當成“後退”導航；
//  - 鍵盤鉤子把瀏覽器後退/前進鍵(0xA6/0xA7，羅技等改鍵滑鼠常把側鍵對映成這個)也
//    吞掉並回報；
//  - 鍵盤鉤子放行已繫結的 PTT/免持單鍵，不觸發錄音——錄製時按到舊熱鍵應當是"想把它
//    錄進來"（或看到衝突提示），而不是開始口述。
static SHORTCUT_CAPTURE: AtomicBool = AtomicBool::new(false);
/// 捕獲到側鍵 down 後，記住其 vk，好把配對的 up 也一併吞掉（否則可能殘留導航）。
static CONSUME_XUP_VK: AtomicU32 = AtomicU32::new(0);

/// 開啟/關閉錄製捕獲模式（供設定頁錄製快捷鍵時呼叫）。
pub fn set_shortcut_capture(on: bool) {
    SHORTCUT_CAPTURE.store(on, Ordering::SeqCst);
    // 只在“開始錄製”時清零，作為一次乾淨的起點。
    // 絕不在“結束錄製”時清零——因為捕獲到 down 後配對的 up 往往稍晚才到，
    // 若在結束時就清掉，這個“抬起”就會漏給 webview（後退鍵會導致頁面返回）。
    if on {
        CONSUME_XUP_VK.store(0, Ordering::SeqCst);
        // 錄製時可能按側鍵，滑鼠鉤子必須在場才能把它吞掉（見 needs_mouse_hook）。
        // 鉤子只能裝在鉤子執行緒上，所以發訊息讓那邊裝。
        #[cfg(windows)]
        {
            let tid = HOOK_THREAD_ID.load(Ordering::SeqCst);
            if tid != 0 {
                unsafe {
                    let _ = PostThreadMessageW(tid, WM_SAYIT_NEED_MOUSE_HOOK, WPARAM(0), LPARAM(0));
                }
            }
        }
    }
}

/// 鍵盤鉤子執行緒的 id，用來從別的執行緒讓它裝上滑鼠鉤子。0 = 鉤子執行緒沒在跑。
#[cfg_attr(not(windows), allow(dead_code))]
static HOOK_THREAD_ID: AtomicU32 = AtomicU32::new(0);
/// 發給鉤子執行緒的私有訊息（WM_APP + 0x51）：「現在需要滑鼠鉤子」。
#[cfg(windows)]
const WM_SAYIT_NEED_MOUSE_HOOK: u32 = 0x8000 + 0x51;
/// 發給鉤子執行緒的私有訊息（WM_APP + 0x52）：「鉤子可能被系統摘了，重灌一遍」。
#[cfg(windows)]
const WM_SAYIT_REFRESH_HOOKS: u32 = 0x8000 + 0x52;
/// 已經因為懷疑被摘而重灌過幾次（寫進 [ptt-watchdog] 日誌）。
static HOOK_REFRESH_COUNT: AtomicU32 = AtomicU32::new(0);

/// 低階鉤子回撥超過 LowLevelHooksTimeout（系統卡頓、休眠喚醒時常見）會被 Windows **靜默摘除**，
/// 不發任何通知；之後熱鍵全部失靈，直到重啟 SayIt。
///
/// 判據：系統記錄的「最後一次輸入」比鉤子最後一次被呼叫晚了 5 秒以上，說明有輸入沒經過我們。
/// 會誤報：沒綁滑鼠鍵時不裝滑鼠鉤子，只動滑鼠也算輸入。誤報的代價只是重灌一次鉤子
/// （幾微秒，在鉤子執行緒裡先裝新的再卸舊的，中間不會處理任何按鍵，所以不會漏也不會重複），
/// 所以寧可多裝，每分鐘最多一次。
#[cfg_attr(not(windows), allow(dead_code))]
fn hook_looks_removed(callback_age_ms: i64, input_age_ms: i64) -> bool {
    callback_age_ms >= 0 && input_age_ms >= 0 && callback_age_ms - input_age_ms > 5_000
}

/// 系統最後一次輸入（鍵盤或滑鼠）距今多少毫秒。
#[cfg(windows)]
fn last_input_age_ms() -> Option<i64> {
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    unsafe {
        if !GetLastInputInfo(&mut info).as_bool() {
            return None;
        }
        Some(GetTickCount().wrapping_sub(info.dwTime) as i64)
    }
}

/// 滑鼠鉤子只在用得上時才裝：某個熱鍵綁在滑鼠鍵上（側鍵 / 中鍵），或者正在錄製快捷鍵。
///
/// 原來它隨鍵盤鉤子一直掛著。回撥雖然對滑鼠移動最先放行，但每次移動仍要經過一次
/// 跨程序回撥，高回報率的遊戲滑鼠每秒上千次。絕大多數使用者根本沒綁滑鼠鍵。
/// 錄製時裝上的鉤子留到下次重配（儲存繫結後就會重配）才按繫結重新決定，
/// 這樣捕獲到側鍵後配對的那個抬起照樣能被吞掉（pitfalls #6）。
#[cfg_attr(not(windows), allow(dead_code))]
fn needs_mouse_hook(ptt: &[u32], hf: &[u32], ai_toggle: &[u32], capturing: bool) -> bool {
    capturing || ptt.iter().chain(hf).chain(ai_toggle).any(|vk| is_mouse_vk(*vk))
}

// ── 全域性 Esc 動作 ──
// overlay 不搶焦點，使用者按鍵仍發給原應用，因此必須在現有 WH_KEYBOARD_LL 中捕獲。
// 僅由前端在 recording/processing/fallback 可見期間短暫開啟，並設定硬截止時間作為安全網。
const ESCAPE_MODE_OFF: u32 = 0;
const ESCAPE_MODE_CANCEL_PROCESSING: u32 = 1;
const ESCAPE_MODE_DISMISS_FALLBACK: u32 = 2;
const ESCAPE_MODE_CANCEL_RECORDING: u32 = 3;
/// 處理已超時、正在寬限期裡等遲到結果時按 Esc：放棄等待。
/// 必須和 dismiss_fallback 分開 —— 它除了收起提示，還要讓隨後到達的遲到結果
/// **不再自動插字**（否則介面說著"失敗"，文字卻突然出現在輸入框裡）。
const ESCAPE_MODE_ABANDON_LATE_RESULT: u32 = 4;
static ESCAPE_ACTION_MODE: AtomicU32 = AtomicU32::new(ESCAPE_MODE_OFF);
static ESCAPE_ACTION_TOKEN: AtomicU64 = AtomicU64::new(0);
static ESCAPE_ACTION_DEADLINE_MS: AtomicI64 = AtomicI64::new(0);
static ESCAPE_KEY_DOWN: AtomicBool = AtomicBool::new(false);

pub fn set_escape_action_mode(mode: &str, token: u64) -> Result<(), String> {
    let (value, ttl_ms) = match mode {
        "off" => (ESCAPE_MODE_OFF, 0),
        // 錄音硬上限約 5 分鐘；留足安全餘量，但異常狀態也不會永久吞 Esc。
        "cancel_recording" => (ESCAPE_MODE_CANCEL_RECORDING, 11 * 60 * 1000),
        "cancel_processing" => (ESCAPE_MODE_CANCEL_PROCESSING, 2 * 60 * 1000),
        // 結果卡/失敗卡不再自動消失，改由前端在可見期間續期，所以這裡的 TTL 只是
        // 異常兜底（渲染端卡死時不至於永久吞 Esc）。續期間隔必須明顯小於它。
        "dismiss_fallback" => (ESCAPE_MODE_DISMISS_FALLBACK, 30 * 1000),
        // 寬限期最長 LATE_FINAL_GRACE_MS（15s），留足餘量。
        "abandon_late_result" => (ESCAPE_MODE_ABANDON_LATE_RESULT, 60 * 1000),
        _ => return Err(format!("Unknown Escape action mode: {mode}")),
    };
    // 先關閉模式，再按 token/deadline/final mode 的順序釋出一組一致快照。
    // 鉤子只有看到最終非 off 模式後才會讀取對應 token。
    ESCAPE_ACTION_MODE.store(ESCAPE_MODE_OFF, Ordering::SeqCst);
    ESCAPE_ACTION_TOKEN.store(if value == ESCAPE_MODE_OFF { 0 } else { token }, Ordering::SeqCst);
    ESCAPE_ACTION_DEADLINE_MS.store(
        if value == ESCAPE_MODE_OFF { 0 } else { now_ms() + ttl_ms },
        Ordering::SeqCst,
    );
    ESCAPE_ACTION_MODE.store(value, Ordering::SeqCst);
    Ok(())
}

fn active_escape_action() -> (u32, u64) {
    let mode = ESCAPE_ACTION_MODE.load(Ordering::SeqCst);
    if mode == ESCAPE_MODE_OFF {
        return (ESCAPE_MODE_OFF, 0);
    }
    if now_ms() > ESCAPE_ACTION_DEADLINE_MS.load(Ordering::SeqCst) {
        ESCAPE_ACTION_MODE.store(ESCAPE_MODE_OFF, Ordering::SeqCst);
        ESCAPE_ACTION_TOKEN.store(0, Ordering::SeqCst);
        ESCAPE_ACTION_DEADLINE_MS.store(0, Ordering::SeqCst);
        return (ESCAPE_MODE_OFF, 0);
    }
    (mode, ESCAPE_ACTION_TOKEN.load(Ordering::SeqCst))
}

fn escape_action_mode_name(mode: u32) -> &'static str {
    match mode {
        ESCAPE_MODE_CANCEL_RECORDING => "cancel_recording",
        ESCAPE_MODE_CANCEL_PROCESSING => "cancel_processing",
        ESCAPE_MODE_DISMISS_FALLBACK => "dismiss_fallback",
        ESCAPE_MODE_ABANDON_LATE_RESULT => "abandon_late_result",
        _ => "off",
    }
}

// ── 懸浮窗卡片快捷鍵（與卡片生命週期繫結的臨時組合鍵）──
//
// 為什麼不能用 global_shortcut 註冊 Ctrl+C：那是**永久**佔用，使用者在別的程式裡
// 就再也複製不了東西。為什麼不能在 overlay 的 webview 裡監聽 keydown：懸浮窗是
// `.focused(false)` 且顯示時帶 SWP_NOACTIVATE，按鍵始終發給使用者原來的程式，DOM
// 永遠收不到 —— Esc 當年就是因此改用這個鉤子的。
//
// 所以做成"只在卡片可見期間、且只在卡片彈出時那個前台視窗裡"生效的臨時接管，
// 三道閘門：位掩碼（哪些動作開著）、token（哪一張卡片）、前台視窗（還是不是他）。
const CARD_HOTKEY_COPY: u32 = 1 << 0;

static CARD_HOTKEY_MASK: AtomicU32 = AtomicU32::new(0);
static CARD_HOTKEY_TOKEN: AtomicU64 = AtomicU64::new(0);
static CARD_HOTKEY_DEADLINE_MS: AtomicI64 = AtomicI64::new(0);
/// 卡片彈出瞬間的前台視窗控制代碼。0 表示不校驗 —— 拿不到前台視窗時寧可少吞一次
/// （漏判只讓使用者改用滑鼠點，誤吞會搶走他在別處的複製）。
static CARD_HOTKEY_FOREGROUND: AtomicIsize = AtomicIsize::new(0);
/// 一次物理按下只投遞一個動作；同時保證配對的 keyup 也被吞掉，否則目標程式會
/// 收到一個孤立的 C 抬起事件（與 ESCAPE_KEY_DOWN 同一個理由）。
static CARD_HOTKEY_C_DOWN: AtomicBool = AtomicBool::new(false);

/// 卡片快捷鍵的硬截止。
///
/// 結果卡現在**不再自動消失**（文本還沒交給使用者，不該按"通知已讀"處理），所以
/// 前端會在卡片可見期間週期性續期。這個 TTL 只是異常兜底：渲染端卡死或程序忙死時，
/// 絕不能永久接管使用者的 Ctrl+C。續期間隔必須明顯小於它。
const CARD_HOTKEY_TTL_MS: i64 = 20 * 1000;

/// 開啟/續期/解除卡片快捷鍵。`actions` 為空即全部解除。
pub fn set_card_hotkeys(actions: &[String], token: u64) -> Result<(), String> {
    let mut mask = 0u32;
    for action in actions {
        match action.as_str() {
            "copy" => mask |= CARD_HOTKEY_COPY,
            other => return Err(format!("Unknown card hotkey action: {other}")),
        }
    }

    if mask == 0 {
        CARD_HOTKEY_MASK.store(0, Ordering::SeqCst);
        CARD_HOTKEY_TOKEN.store(0, Ordering::SeqCst);
        CARD_HOTKEY_DEADLINE_MS.store(0, Ordering::SeqCst);
        CARD_HOTKEY_FOREGROUND.store(0, Ordering::SeqCst);
        return Ok(());
    }

    // 續期（token 未變）時不要重新取前台視窗：卡片可見期間使用者可能已經切走，
    // 那正是要解除接管的情形，重新取會把新前台當成"卡片的主人"又接管回來。
    let renewing = CARD_HOTKEY_TOKEN.load(Ordering::SeqCst) == token
        && CARD_HOTKEY_MASK.load(Ordering::SeqCst) != 0;
    if !renewing {
        CARD_HOTKEY_FOREGROUND.store(current_foreground_hwnd(), Ordering::SeqCst);
    }

    // 與 set_escape_action_mode 同樣的釋出順序：先關，再寫 token/deadline，最後開 mask。
    // 鉤子只有看到非零 mask 之後才會去讀 token。
    CARD_HOTKEY_MASK.store(0, Ordering::SeqCst);
    CARD_HOTKEY_TOKEN.store(token, Ordering::SeqCst);
    CARD_HOTKEY_DEADLINE_MS.store(now_ms() + CARD_HOTKEY_TTL_MS, Ordering::SeqCst);
    CARD_HOTKEY_MASK.store(mask, Ordering::SeqCst);
    Ok(())
}

#[cfg(windows)]
fn current_foreground_hwnd() -> isize {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    unsafe { GetForegroundWindow().0 as isize }
}

/// macOS：用前台應用的 pid 代替視窗控制代碼（卡片快捷鍵只需判斷「使用者切沒切走」）。
#[cfg(target_os = "macos")]
fn current_foreground_hwnd() -> isize {
    crate::platform::macos::ax::focused_application_pid().unwrap_or(0) as isize
}

#[cfg(not(any(windows, target_os = "macos")))]
fn current_foreground_hwnd() -> isize {
    0
}

/// 讀取當前生效的卡片快捷鍵；順手做過期自清（與 active_escape_action 同構）。
fn active_card_hotkeys() -> (u32, u64) {
    let mask = CARD_HOTKEY_MASK.load(Ordering::SeqCst);
    if mask == 0 {
        return (0, 0);
    }
    if now_ms() > CARD_HOTKEY_DEADLINE_MS.load(Ordering::SeqCst) {
        CARD_HOTKEY_MASK.store(0, Ordering::SeqCst);
        CARD_HOTKEY_TOKEN.store(0, Ordering::SeqCst);
        CARD_HOTKEY_DEADLINE_MS.store(0, Ordering::SeqCst);
        CARD_HOTKEY_FOREGROUND.store(0, Ordering::SeqCst);
        return (0, 0);
    }
    (mask, CARD_HOTKEY_TOKEN.load(Ordering::SeqCst))
}

/// 前台視窗是否仍是卡片彈出時那個。記錄值為 0（取不到）時一律放過校驗。
#[cfg(any(windows, target_os = "macos"))]
fn card_hotkey_foreground_matches() -> bool {
    let expected = CARD_HOTKEY_FOREGROUND.load(Ordering::SeqCst);
    if expected == 0 {
        return true;
    }
    current_foreground_hwnd() == expected
}

/// 這一刻按下的是不是**只有** Ctrl 的 Ctrl+C。
///
/// 為什麼要排除其他修飾鍵：Ctrl+Shift+C（瀏覽器開發者工具的"檢查元素"）、Ctrl+Alt+C、
/// Win+Ctrl+C 都含著一個 Ctrl+C。只判"Ctrl 按著"會把這些一併截走，使用者在自己的程式裡
/// 突然少了一個快捷鍵，而且完全看不出是誰幹的。
///
/// 用 `GetAsyncKeyState` 是可靠的：這些修飾鍵我們從不吞（見
/// single_key_blocked_by_modifiers 的註釋）。唯一的邊界是使用者把**右 Ctrl 設成了
/// PTT 單鍵**（預設配置就是），那一路 down 會被吞掉、非同步狀態可能讀不到 —— 但那種
/// 場景下按右 Ctrl 本來就在開始錄音，不會是在複製。
#[cfg(windows)]
unsafe fn only_ctrl_is_down(kb_flags: u32) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    const VK_CONTROL: i32 = 0x11;
    const VK_SHIFT: i32 = 0x10;
    const VK_MENU: i32 = 0x12;
    const VK_LWIN: i32 = 0x5B;
    const VK_RWIN: i32 = 0x5C;

    if GetAsyncKeyState(VK_CONTROL) >= 0 {
        return false;
    }
    // Alt 優先用事件自帶的 LLKHF_ALTDOWN：那是 OS 派發這次按鍵時的視角，比事後問
    // GetAsyncKeyState 更貼近現場（與 single_key_blocked_by_modifiers 同一個理由）。
    if kb_flags & LLKHF_ALTDOWN != 0 {
        return false;
    }
    for vk in [VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN] {
        if GetAsyncKeyState(vk) < 0 {
            return false;
        }
    }
    true
}

fn card_hotkey_action_name(action: u32) -> &'static str {
    match action {
        CARD_HOTKEY_COPY => "copy",
        _ => "unknown",
    }
}

// ── 看門狗寫日誌的節流狀態 ──
// 每 60s 取樣一次是廉價的，但沒必要每次都寫日誌：正常時全是同樣的正常值，純噪音。
// 只在"狀態翻轉 / 失敗計數增加 / 距上次寫日誌超過心跳間隔"時才落一行。
static WD_LAST_LOG_MS: AtomicI64 = AtomicI64::new(0);
static WD_LAST_HOOK_RUNNING: AtomicBool = AtomicBool::new(false);
static WD_LAST_DISPATCHER_ALIVE: AtomicBool = AtomicBool::new(false);
static WD_LAST_FAIL_COUNT: AtomicU64 = AtomicU64::new(0);
/// 一切正常時的心跳間隔：每 10 分鐘落一行，證明程序還活著。
const WD_HEARTBEAT_MS: i64 = 10 * 60 * 1000;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(windows)]
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
    GetMessageW, TranslateMessage, DispatchMessageW,
    KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WM_QUIT,
    MSLLHOOKSTRUCT, WH_MOUSE_LL, WM_XBUTTONDOWN, WM_XBUTTONUP,
    WM_MBUTTONDOWN, WM_MBUTTONUP,
};

/// 單鍵熱鍵表 —— 單一資料來源。
///
/// ⚠️ 必須與前端 `client/src/lib/shortcutKeys.ts` 的 `SINGLE_KEYS` 保持一致
/// （Rust 無法直接引用 TS，新增/修改單鍵時兩邊都要改）。
/// `setting` 取值等於 DOM KeyboardEvent.code。
const SINGLE_KEY_TABLE: &[(&str, u32)] = &[
    ("AltLeft", 0xA4),
    ("AltRight", 0xA5),
    ("ControlLeft", 0xA2),
    ("ControlRight", 0xA3),
    ("ShiftLeft", 0xA0),
    ("ShiftRight", 0xA1),
    ("CapsLock", 0x14),
    ("Space", 0x20),
    ("ContextMenu", 0x5D),
    ("Pause", 0x13),
    ("ScrollLock", 0x91),
    ("Insert", 0x2D),
    // 滑鼠側鍵：走 WH_MOUSE_LL 滑鼠鉤子（非鍵盤鉤子），vk 用 Windows 的 VK_XBUTTON1/2。
    ("XButton1", 0x05),
    ("XButton2", 0x06),
    // 滑鼠中鍵：走同一個低階滑鼠鉤子，vk 用 VK_MBUTTON。
    ("MButton", 0x04),
    // 瀏覽器後退/前進鍵：羅技等改鍵滑鼠常把側鍵對映成這個（走鍵盤鉤子）。
    ("BrowserBack", 0xA6),
    ("BrowserForward", 0xA7),
    ("F1", 0x70), ("F2", 0x71), ("F3", 0x72), ("F4", 0x73),
    ("F5", 0x74), ("F6", 0x75), ("F7", 0x76), ("F8", 0x77),
    ("F9", 0x78), ("F10", 0x79), ("F11", 0x7A), ("F12", 0x7B),
    // F13–F24：物理鍵盤上沒有這些鍵，因此幾乎不與任何程式的快捷鍵衝突，
    // 適合可程式設計鍵盤 / 自製 HID 語音裝置當專用觸發鍵。
    // 這一段在幻影過濾裡有例外，見 is_injection_exempt_vk。
    ("F13", 0x7C), ("F14", 0x7D), ("F15", 0x7E), ("F16", 0x7F),
    ("F17", 0x80), ("F18", 0x81), ("F19", 0x82), ("F20", 0x83),
    ("F21", 0x84), ("F22", 0x85), ("F23", 0x86), ("F24", 0x87),
];

/// 只在 macOS 上能單獨當熱鍵的鍵，Windows 上不放進上面的公共表（Windows 行為保持不變）：
/// - 單獨的 ⌘（Windows 上是 Win 鍵，單按會彈開始選單，所以 Windows 不允許）；
///   mac 上右 ⌘ 單按沒有系統動作，是免持的預設鍵。
/// - Fn / 🌐：Windows 沒有對應的虛擬鍵；mac 的 event tap 把它翻譯成保留值 0xFF
///   （見 platform::macos::keycodes::VK_FN）。
/// ⚠️ 與前端 lib/shortcutKeys.ts 裡 IS_MAC 時追加的單鍵保持一致。
#[cfg(target_os = "macos")]
const PLATFORM_EXTRA_SINGLE_KEYS: &[(&str, u32)] = &[("MetaLeft", 0x5B), ("MetaRight", 0x5C), ("Fn", 0xFF)];
#[cfg(not(target_os = "macos"))]
const PLATFORM_EXTRA_SINGLE_KEYS: &[(&str, u32)] = &[];

/// 單鍵熱鍵表查詢（公共表 + 本平台額外的單鍵）。
fn single_key_vk(code: &str) -> Option<u32> {
    SINGLE_KEY_TABLE
        .iter()
        .chain(PLATFORM_EXTRA_SINGLE_KEYS)
        .find(|(setting, _)| *setting == code)
        .map(|(_, vk)| *vk)
}

fn ptt_modifier_family(code: &str) -> Option<&'static str> {
    if code.starts_with("Alt") {
        Some("Alt")
    } else if code.starts_with("Control") {
        Some("Control")
    } else if code.starts_with("Shift") {
        Some("Shift")
    } else if code.starts_with("Meta") {
        Some("Meta")
    } else {
        None
    }
}

fn vk_for_ptt_code(code: &str) -> Option<u32> {
    if let Some(vk) = single_key_vk(code) {
        return Some(vk);
    }

    if let Some(letter) = code.strip_prefix("Key") {
        let bytes = letter.as_bytes();
        if bytes.len() == 1 && bytes[0].is_ascii_uppercase() {
            return Some(bytes[0] as u32);
        }
    }
    if let Some(digit) = code.strip_prefix("Digit") {
        let bytes = digit.as_bytes();
        if bytes.len() == 1 && bytes[0].is_ascii_digit() {
            return Some(bytes[0] as u32);
        }
    }

    match code {
        "MetaLeft" => Some(0x5B),
        "MetaRight" => Some(0x5C),
        "Escape" => Some(0x1B),
        "Tab" => Some(0x09),
        "Enter" => Some(0x0D),
        "Backspace" => Some(0x08),
        "Delete" => Some(0x2E),
        "ArrowUp" => Some(0x26),
        "ArrowDown" => Some(0x28),
        "ArrowLeft" => Some(0x25),
        "ArrowRight" => Some(0x27),
        "Home" => Some(0x24),
        "End" => Some(0x23),
        "PageUp" => Some(0x21),
        "PageDown" => Some(0x22),
        _ => None,
    }
}

struct PttKeyConfig {
    setting: String,
    vk_codes: Vec<u32>,
    modifier_mask: u64,
}

impl PttKeyConfig {
    fn disabled() -> Self {
        Self {
            setting: String::new(),
            vk_codes: Vec::new(),
            modifier_mask: 0,
        }
    }

    fn fallback() -> Self {
        Self {
            setting: DEFAULT_PTT_SETTING.to_string(),
            vk_codes: vec![DEFAULT_PTT_VK],
            modifier_mask: 1,
        }
    }
}

/// 「按住說話」的預設鍵，同時也是配置非法時的回退。
///
/// **不能是 Shift**：長按右 Shift 約 8 秒會觸發 Windows 篩選鍵，那之後我們收不到
/// 這次「鬆開」，錄音不會隨手指抬起而結束。這裡曾經是 ShiftRight，於是每台新裝的
/// 機器開箱就綁在那個鍵上（前端也一樣，見 services/defaults.ts）。
/// 右 Ctrl 位置相近、沒有輔助功能陷阱，也不和免持的預設鍵（右 Alt）撞。
///
/// 值在 crate::defaults，那裡有與前端 defaults.ts 的對帳測試。
const DEFAULT_PTT_SETTING: &str = crate::defaults::SHORTCUT_PTT;
/// 與 DEFAULT_PTT_SETTING 對應的鍵：Windows 右 Ctrl，macOS 右 ⌥（MacBook 沒有右 Ctrl）。
#[cfg(not(target_os = "macos"))]
const DEFAULT_PTT_VK: u32 = 0xA3;
#[cfg(target_os = "macos")]
const DEFAULT_PTT_VK: u32 = 0xA5;

/// 解析向後相容的 PTT 物理鍵格式。舊單鍵仍是單成員；組合成員用 `+` 分隔。
///
/// 非法或未知非空值安全回退到預設鍵，避免整串被誤當成別的鍵。
/// 注意這裡**不禁止 Shift**：老使用者存的可能就是 ShiftRight，那些繫結必須照原樣生效。
/// 「不許再新設 Shift」是前端校驗的事（lib/shortcutKeys.ts），不是解析器的事 ——
/// 在這裡連解析都拒掉，等於使用者沒改設定說話鍵就自己變了。
fn ptt_key_config(setting: &str) -> PttKeyConfig {
    if setting.is_empty() {
        return PttKeyConfig::disabled();
    }

    let codes: Vec<&str> = setting.split('+').collect();
    if codes.is_empty() || codes.len() > 63 || codes.iter().any(|code| code.is_empty()) {
        return PttKeyConfig::fallback();
    }

    let mut vk_codes = Vec::with_capacity(codes.len());
    let mut modifier_mask = 0_u64;
    let mut modifier_families = Vec::new();
    for (index, code) in codes.iter().enumerate() {
        let Some(vk) = vk_for_ptt_code(code) else {
            return PttKeyConfig::fallback();
        };
        if vk_codes.contains(&vk) {
            return PttKeyConfig::fallback();
        }
        vk_codes.push(vk);
        if let Some(family) = ptt_modifier_family(code) {
            // WebView 的 KeyboardEvent 只能給出家族級 ctrlKey/altKey 等狀態，無法在
            // 主鍵快路徑可靠區分同族左右鍵；明確禁止左右兩側同時進入一個組合。
            if modifier_families.contains(&family) {
                return PttKeyConfig::fallback();
            }
            modifier_families.push(family);
            modifier_mask |= 1_u64 << index;
        }
    }

    let mouse_member = codes
        .iter()
        .any(|code| matches!(*code, "XButton1" | "XButton2" | "MButton"));
    let main_key_count = codes.len() - modifier_mask.count_ones() as usize;
    let valid_shape = if codes.len() == 1 {
        single_key_vk(codes[0]).is_some()
    } else {
        !mouse_member
            && main_key_count <= 1
            && ((main_key_count == 0 && codes.len() >= 2)
                || (main_key_count == 1 && modifier_mask != 0))
    };
    if !valid_shape {
        return PttKeyConfig::fallback();
    }

    PttKeyConfig {
        setting: setting.to_string(),
        vk_codes,
        modifier_mask,
    }
}

/// Virtual key code mapping for PTT settings.
fn vk_codes_for_setting(setting: &str) -> Vec<u32> {
    ptt_key_config(setting).vk_codes
}

/// Check if a shortcut setting is a single key (handled by hook) vs combo (handled by global_shortcut).
pub fn is_single_key_setting(setting: &str) -> bool {
    single_key_vk(setting).is_some()
}

/// 該設定是否為滑鼠側鍵（由低階滑鼠鉤子處理，而非鍵盤鉤子）。
#[cfg_attr(not(windows), allow(dead_code))]
fn is_mouse_button_setting(setting: &str) -> bool {
    matches!(setting, "XButton1" | "XButton2" | "MButton")
}

/// 這個 vk 是滑鼠按鍵嗎（VK_MBUTTON / VK_XBUTTON1 / VK_XBUTTON2）。
///
/// ⚠️ 滑鼠按鍵的物理狀態**查不到**：低階滑鼠鉤子把它們的 down/up 都吞掉了
/// （返回 LRESULT(1)，否則會觸發別的程式的前進/後退導航），事件因此不進系統輸入
/// 佇列，Windows 也就不為它們維護非同步鍵狀態 —— `GetAsyncKeyState` 會一直報「沒按下」。
/// 所以凡是以 `is_ptt_member_physically_down` 為判據的邏輯，遇到滑鼠按鍵都必須繞開。
///
/// 若日後往 SINGLE_KEY_TABLE 裡加第四個滑鼠鍵，這裡也要跟著加（有測試釘住）。
fn is_mouse_vk(vk: u32) -> bool {
    matches!(vk, 0x04 | 0x05 | 0x06)
}

/// 幻影過濾的例外部索引鍵：即使帶「注入」標誌或 scanCode==0，也照常當使用者按鍵處理。
///
/// 幻影過濾真正要防的是 Windows 選單導航合成出來的假 Alt（與真實按鍵幾乎無法區分，
/// 只能靠 injected 標誌和 scanCode==0 識別）。下面這些鍵都不可能是那種幻影，而它們的
/// **正常來源恰恰就是注入**，按幻影丟棄只會讓使用者綁了不生效：
///   · BrowserBack / BrowserForward（0xA6/0xA7）：羅技等滑鼠驅動把側鍵改寫成它們。
///   · F13–F24（0x7C–0x87）：物理鍵盤上不存在這些鍵，使用者要按出來只有兩條路 ——
///     可程式設計鍵盤 / HID 韌體直接發（真鍵，本來就能過），或者用 AutoHotkey、PowerToys
///     鍵盤管理器、外設驅動宏把別的鍵對映過來。後者走 SendInput，帶 injected 標誌；
///     不開例外的話這類使用者會遇到「綁得上、按下去沒反應、日誌裡也沒有任何記錄」，
///     排查起來毫無線索。誤判的代價只是某個程式模擬 F13 時多彈一次懸浮窗。
#[cfg_attr(not(windows), allow(dead_code))]
fn is_injection_exempt_vk(vk: u32) -> bool {
    matches!(vk, 0xA6 | 0xA7) || (0x7C..=0x87).contains(&vk)
}

/// `KBDLLHOOKSTRUCT.flags` 的 LLKHF_ALTDOWN 位：這次按鍵發生時 Alt 是否按住。
///
/// 這是 OS 派發事件時自己的視角，比事後問 `GetAsyncKeyState` 更貼近現場。
/// ⚠️ 不能用「msg == WM_SYSKEYDOWN」代替：F10 單獨按下也走 WM_SYSKEYDOWN。
#[cfg(windows)]
const LLKHF_ALTDOWN: u32 = 0x20;

/// 左右 Shift / Ctrl / Alt / Win 的 vk。
const MODIFIER_VK_CODES: [u32; 8] = [0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0x5B, 0x5C];

/// 這個單鍵熱鍵是否要求「裸按」—— 按下的那一刻不能有額外修飾鍵按住。
///
/// 起因：使用者把 F4 設成免持，結果按 Alt+F4 也會開始錄音（而且 F4 的 down 被我們吞掉，
/// Alt+F4 本身還失效了）。判定原來只比 vk，壓根不看修飾鍵。
///
/// **只對主鍵成立**（F1–F24、Space、Insert、CapsLock、BrowserBack…）。熱鍵本身就是
/// 修飾鍵時絕不能套這條規則，兩個原因都會讓熱鍵徹底失效：
///   · 「按住說話」預設是右 Ctrl、免持預設是右 Alt。自己就是 Ctrl，再去問
///     「有沒有 Ctrl 按住」永遠為真；
///   · 歐洲鍵盤佈局的 AltGr 會同時產生 左Ctrl + 右Alt。右 Alt 當熱鍵時，
///     這些使用者每次按都帶著一個左 Ctrl，會被永久擋住。
///
/// 滑鼠鍵也排除：它們走的是滑鼠鉤子，而且 Alt+側鍵不是任何系統快捷鍵 ——
/// 放行反而會把瀏覽器的前進/後退導航漏給別的程式（見 pitfalls 第 6 條）。
fn single_key_requires_bare_press(vk: u32) -> bool {
    !MODIFIER_VK_CODES.contains(&vk) && !is_mouse_vk(vk)
}

/// 按下這個單鍵熱鍵的時刻，是否有額外修飾鍵按住（有則整條放行、不吞、不觸發）。
///
/// Ctrl / Shift / Alt / Win 我們從不吞掉，所以它們的非同步鍵狀態是可靠的 ——
/// pitfalls 第 13 條那條「滑鼠鍵查不到物理狀態」的限制在這裡不適用。
#[cfg(windows)]
unsafe fn single_key_blocked_by_modifiers(vk: u32, kb_flags: u32) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    if !single_key_requires_bare_press(vk) {
        return false;
    }
    if kb_flags & LLKHF_ALTDOWN != 0 {
        return true;
    }
    MODIFIER_VK_CODES
        .iter()
        .any(|code| GetAsyncKeyState(*code as i32) < 0)
}

#[allow(dead_code)]
fn modifier_kind(setting: &str) -> Option<&'static str> {
    match setting {
        "AltLeft" | "AltRight" => Some("alt"),
        "ControlLeft" | "ControlRight" => Some("ctrl"),
        "ShiftLeft" | "ShiftRight" => Some("shift"),
        "MetaLeft" | "MetaRight" => Some("meta"),
        _ => None,
    }
}

#[derive(Clone, Serialize)]
struct PTTEvent {
    source: String,
    reason: String,
    #[serde(rename = "keycode")]
    vk: u32,
    #[serde(rename = "pttSetting")]
    ptt_setting: String,
    timestamp: i64,
    #[serde(rename = "altKey")]
    alt_key: bool,
    #[serde(rename = "ctrlKey")]
    ctrl_key: bool,
    #[serde(rename = "shiftKey")]
    shift_key: bool,
    #[serde(rename = "metaKey")]
    meta_key: bool,
}

/// Shared state between the hook callback and the main thread
struct HookSharedState {
    ptt_vk_codes: Vec<u32>,
    ptt_setting: String,
    /// PTT 成員的已按下點陣圖；只有點陣圖首次達到 full_mask 才開始。
    ptt_pressed_mask: AtomicU64,
    ptt_full_mask: u64,
    /// 哪些 PTT 成員是修飾鍵，用於組合鍵的事件消費策略。
    ptt_modifier_mask: u64,
    /// 普通組合中已經吞掉 down 的主鍵位；僅對應位的 up 才能繼續吞，避免孤立事件。
    ptt_consumed_down_mask: AtomicU64,
    /// 當前活動 PTT 代次；0 表示空閒，最高位表示某個釋放方已認領、正在入隊 up。
    /// 按下、釋放歸屬和“允許下一次按下”由這一個原子量決定，避免拆分 bool/代次產生競態。
    ptt_active_generation: AtomicU64,
    /// 單調遞增的 PTT 代次分配器，僅在新按下時推進。
    ptt_generation: AtomicU64,
    /// 每個成員最近一次真實 keydown（包括 repeat）的時間。
    /// 各成員獨立記錄，避免仍按住成員的 repeat 掩蓋另一個成員已經漏掉 key-up。
    ptt_last_down_ms: Vec<AtomicI64>,
    /// hook 生命週期標記，供獨立成員狀態看門狗在重配/停止時退出。
    ptt_hook_alive: AtomicBool,
    hands_free_active: AtomicBool,
    /// 免持鍵是否處於"已按下"狀態。首個真實 keydown 觸發 toggle，repeat down 被忽略；
    /// 配對 keyup（包括被標成 synthetic 的 up）負責清零，讓下一次按下可以再次觸發。
    hf_key_down: AtomicBool,
    /// VK codes for hands-free toggle key (empty = not using hook for hands-free)
    hf_vk_codes: Vec<u32>,
    hf_setting: String,
    /// AI 整理開關支援單鍵與滑鼠按鍵；組合鍵仍由 global_shortcut 處理。
    ai_toggle_key_down: AtomicBool,
    ai_toggle_vk_codes: Vec<u32>,
    ai_toggle_setting: String,
    app_handle: AppHandle,
}

/// Message sent from the hook callback (non-blocking) to the dispatcher thread.
#[cfg(any(windows, target_os = "macos"))]
#[allow(dead_code)]
enum HookAction {
    PttDown { vk: u32, gen: u64 },
    PttUp { vk: u32, gen: u64, reason: &'static str },
    HfToggle { vk: u32 },
    AiToggle { vk: u32 },
    Escape { mode: u32, token: u64 },
    /// 懸浮窗卡片上的臨時組合鍵（目前只有 Ctrl+C = 複製結果文本）。
    CardHotkey { action: u32, token: u64 },
    Diag { vk: u32, msg_name: &'static str, flags: u32, scan_code: u32 },
    Shutdown,
    /// 快捷鍵錄製期間捕獲到的滑鼠側鍵（vk=0x05/0x06），用於讓設定頁繫結側鍵。
    MouseCaptured { vk: u32 },
    /// 切換潤色模式。只有 mac 的 event tap 會發：系統快捷鍵接不住的、只帶 ⌥ / ⇧ 的組合鍵
    /// （見 keyboard/macos.rs 的 TAP_COMBOS）。Windows 上這類組合仍由 global_shortcut 處理。
    PresetSwitch { preset_id: String },
}

const PTT_RELEASING_BIT: u64 = 1 << 63;

/// 開始一次新的 PTT 代次。只有 active=0 才能釋出新代次；釋放方把舊 up 放進
/// dispatcher 佇列之前 active 始終非 0，因此不會出現 down(new) 排在 up(old) 前面。
fn begin_ptt_press(active_generation: &AtomicU64, generation: &AtomicU64) -> Option<u64> {
    if active_generation.load(Ordering::SeqCst) != 0 {
        return None;
    }

    let mut gen = generation
        .fetch_add(1, Ordering::SeqCst)
        .wrapping_add(1)
        & !PTT_RELEASING_BIT;
    if gen == 0 {
        gen = generation
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1)
            & !PTT_RELEASING_BIT;
    }

    active_generation
        .compare_exchange(0, gen, Ordering::SeqCst, Ordering::SeqCst)
        .ok()
        .map(|_| gen)
}

/// 免持鍵只在一次物理按下的首個 keydown 觸發；Windows 隨後的 repeat down 被忽略。
fn begin_hf_press(key_down: &AtomicBool) -> bool {
    !key_down.swap(true, Ordering::SeqCst)
}

/// 清除免持按下狀態。返回 true 表示確實存在一枚需要配對消費的 keyup。
fn end_hf_press(key_down: &AtomicBool) -> bool {
    key_down.swap(false, Ordering::SeqCst)
}

/// 冪等認領一次釋放，但暫不允許下一次 down。認領者必須在 up 成功入隊後呼叫
/// `complete_ptt_release`；dispatcher 斷開時也 complete，優先避免 native 狀態永久卡住。
fn claim_ptt_release(
    active_generation: &AtomicU64,
    expected_generation: Option<u64>,
) -> Option<u64> {
    loop {
        let active = active_generation.load(Ordering::SeqCst);
        if active == 0 || (active & PTT_RELEASING_BIT) != 0 {
            return None;
        }
        if expected_generation.is_some_and(|expected| expected != active) {
            return None;
        }
        if active_generation
            .compare_exchange(
                active,
                active | PTT_RELEASING_BIT,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
        {
            return Some(active);
        }
    }
}

fn complete_ptt_release(active_generation: &AtomicU64, gen: u64) -> bool {
    active_generation
        .compare_exchange(
            gen | PTT_RELEASING_BIT,
            0,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_ok()
}

fn clear_ptt_press(active_generation: &AtomicU64) -> Option<u64> {
    let gen = claim_ptt_release(active_generation, None)?;
    let _ = complete_ptt_release(active_generation, gen);
    Some(gen)
}

fn cancel_ptt_press_start(active_generation: &AtomicU64, gen: u64) {
    let _ = active_generation.compare_exchange(gen, 0, Ordering::SeqCst, Ordering::SeqCst);
}

fn ptt_member_index(vk_codes: &[u32], vk: u32) -> Option<usize> {
    vk_codes.iter().position(|configured| *configured == vk)
}

/// 記錄成員 down；僅在該成員由 up→down 且本次使完整點陣圖成立時返回 true。
fn press_ptt_member(pressed_mask: &AtomicU64, index: usize, full_mask: u64) -> bool {
    let bit = 1_u64 << index;
    let previous = pressed_mask.fetch_or(bit, Ordering::SeqCst);
    (previous & bit) == 0 && (previous | bit) == full_mask
}

/// 記錄成員 up；孤立或重複 up 返回 false。
fn release_ptt_member(pressed_mask: &AtomicU64, index: usize) -> bool {
    let bit = 1_u64 << index;
    (pressed_mask.fetch_and(!bit, Ordering::SeqCst) & bit) != 0
}

fn should_consume_combo_main_down(
    pressed_mask: u64,
    consumed_down_mask: u64,
    member_bit: u64,
    modifier_mask: u64,
) -> bool {
    if pressed_mask & member_bit != 0 {
        consumed_down_mask & member_bit != 0
    } else {
        pressed_mask & modifier_mask == modifier_mask
    }
}

fn clear_ptt_members(state: &HookSharedState) {
    state.ptt_pressed_mask.store(0, Ordering::SeqCst);
    state.ptt_consumed_down_mask.store(0, Ordering::SeqCst);
}

fn ptt_modifier_flags(setting: &str) -> (bool, bool, bool, bool) {
    let codes = setting.split('+');
    let mut alt = false;
    let mut ctrl = false;
    let mut shift = false;
    let mut meta = false;
    for code in codes {
        alt |= code.starts_with("Alt");
        ctrl |= code.starts_with("Control");
        shift |= code.starts_with("Shift");
        meta |= code.starts_with("Meta");
    }
    (alt, ctrl, shift, meta)
}

#[cfg(target_os = "macos")]
unsafe fn is_ptt_member_physically_down(vk: u32) -> bool {
    macos::is_physically_down(vk)
}

#[cfg(windows)]
unsafe fn is_ptt_member_physically_down(vk: u32) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    let configured_key_down = GetAsyncKeyState(vk as i32) < 0;
    let mapped_side_button_down = match vk {
        // 羅技等驅動常把標準側鍵改寫成 BrowserBack/Forward；同時檢查原始
        // XBUTTON，避免注入後的瀏覽器 VK 不維護非同步鍵狀態。
        0xA6 => GetAsyncKeyState(0x05) < 0,
        0xA7 => GetAsyncKeyState(0x06) < 0,
        _ => false,
    };
    configured_key_down || mapped_side_button_down
}

/// 供 WebView 聚焦回退路徑確認組合成員的真實物理狀態。
/// 返回值與輸入 code 一一對應；未知 code 安全返回 false。
pub fn ptt_physical_key_states(codes: &[String]) -> Vec<bool> {
    #[cfg(windows)]
    {
        return codes
            .iter()
            .map(|code| {
                vk_for_ptt_code(code)
                    .is_some_and(|vk| unsafe { is_ptt_member_physically_down(vk) })
            })
            .collect();
    }

    #[cfg(target_os = "macos")]
    {
        return codes
            .iter()
            .map(|code| {
                vk_for_ptt_code(code)
                    .is_some_and(|vk| unsafe { is_ptt_member_physically_down(vk) })
            })
            .collect();
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        vec![false; codes.len()]
    }
}

/// 新成員 down 到來前，先清掉其它成員中物理上已經鬆開的殘位。
/// 這既覆蓋錄音結束後的漏 up，也避免在 1.5 秒後台看門狗視窗內誤拼出完整組合。
#[cfg(any(windows, target_os = "macos"))]
unsafe fn reconcile_stale_ptt_members_before_down(
    state: &HookSharedState,
    current_index: usize,
) {
    let pressed = state.ptt_pressed_mask.load(Ordering::SeqCst);
    for (index, &member_vk) in state.ptt_vk_codes.iter().enumerate() {
        if index == current_index {
            continue;
        }
        let bit = 1_u64 << index;
        if pressed & bit != 0 && !is_ptt_member_physically_down(member_vk) {
            let _ = release_ptt_member(&state.ptt_pressed_mask, index);
            state
                .ptt_consumed_down_mask
                .fetch_and(!bit, Ordering::SeqCst);
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn queue_ptt_release(
    sender: &std::sync::mpsc::Sender<HookAction>,
    active_generation: &AtomicU64,
    expected_generation: Option<u64>,
    vk: u32,
    reason: &'static str,
) -> bool {
    let Some(gen) = claim_ptt_release(active_generation, expected_generation) else {
        return false;
    };

    if sender.send(HookAction::PttUp { vk, gen, reason }).is_ok() {
        // up 已經線上性 FIFO 中排隊；現在才允許下一次 down 釋出新代次。
        let _ = complete_ptt_release(active_generation, gen);
        true
    } else {
        // dispatcher 已斷開時不可能再投遞前端事件；至少釋放 native 狀態，避免
        // 永久拒絕後續 down/持續吞 PTT 鍵。健康看門狗會記錄 dispatcher 失效。
        let _ = complete_ptt_release(active_generation, gen);
        TRY_SEND_FAIL_COUNT.fetch_add(1, Ordering::Relaxed);
        false
    }
}

/// 獨立於“當前是否正在錄音”的 PTT 成員物理狀態看門狗。
///
/// 活動代次可能因任一成員正常 up 而結束，但其它成員的 up 仍可能丟失；如果只讓錄音
/// watchdog 工作到代次結束，殘留位會把下一次單鍵誤拼成完整組合。此執行緒存活期等於 hook，
/// 會持續清理每個已物理鬆開且 repeat 靜默足夠久的成員，並在仍有活動代次時補發一次 up。
#[cfg(any(windows, target_os = "macos"))]
fn spawn_ptt_member_watchdog(
    state: Arc<HookSharedState>,
    action_tx: std::sync::mpsc::Sender<HookAction>,
) -> Option<thread::JoinHandle<()>> {
    const POLL_MS: u64 = 100;
    const REPEAT_SILENCE_MS: i64 = 1_500;
    const RELEASED_SAMPLES_REQUIRED: u8 = 3;

    match thread::Builder::new()
        .name("ptt-member-watchdog".to_string())
        .spawn(move || {
            let mut released_samples = vec![0_u8; state.ptt_vk_codes.len()];
            while state.ptt_hook_alive.load(Ordering::SeqCst) {
                thread::sleep(std::time::Duration::from_millis(POLL_MS));
                if !state.ptt_hook_alive.load(Ordering::SeqCst) {
                    break;
                }

                for (index, &member_vk) in state.ptt_vk_codes.iter().enumerate() {
                    let bit = 1_u64 << index;
                    if state.ptt_pressed_mask.load(Ordering::SeqCst) & bit == 0 {
                        released_samples[index] = 0;
                        continue;
                    }

                    // 滑鼠按鍵必須跳過這套判據。它由「repeat 靜默」+「非同步鍵狀態」兩個
                    // 訊號組成，而這兩個對滑鼠按鍵都不存在：滑鼠按鍵沒有自動重複，
                    // 非同步鍵狀態又因為我們吞掉了事件而不更新（見 is_mouse_vk）。
                    // 於是條件恆真、只會穩定誤判。滑鼠鍵的 up 由我們自己的鉤子直接觀測，
                    // 吞掉不影響我們收到事件；真丟了 up 還有 5 分鐘硬釋放兜底。
                    if is_mouse_vk(member_vk) {
                        released_samples[index] = 0;
                        continue;
                    }

                    let repeat_silence_ms = now_ms()
                        .saturating_sub(state.ptt_last_down_ms[index].load(Ordering::SeqCst));
                    let physically_down = unsafe { is_ptt_member_physically_down(member_vk) };
                    if repeat_silence_ms >= REPEAT_SILENCE_MS && !physically_down {
                        released_samples[index] = released_samples[index].saturating_add(1);
                    } else {
                        released_samples[index] = 0;
                    }

                    if released_samples[index] < RELEASED_SAMPLES_REQUIRED {
                        continue;
                    }

                    released_samples[index] = 0;
                    if !release_ptt_member(&state.ptt_pressed_mask, index) {
                        continue;
                    }
                    state
                        .ptt_consumed_down_mask
                        .fetch_and(!bit, Ordering::SeqCst);
                    let active_before = state.ptt_active_generation.load(Ordering::SeqCst);
                    let queued_release = queue_ptt_release(
                        &action_tx,
                        &state.ptt_active_generation,
                        None,
                        member_vk,
                        "member_watchdog_release",
                    );
                    crate::commands::system::write_log_line(&format!(
                        "[RUST] [ptt] member watchdog cleared vk={} setting={} member={} repeat_silence_ms={} active_before={} queued_release={}",
                        member_vk,
                        state.ptt_setting,
                        index,
                        repeat_silence_ms,
                        active_before,
                        queued_release,
                    ));
                }
            }
        }) {
        Ok(handle) => Some(handle),
        Err(error) => {
            crate::commands::system::write_log_line(&format!(
                "[RUST] [ptt] failed to start member watchdog error={}",
                error,
            ));
            None
        }
    }
}

fn emit_ptt_release(state: &HookSharedState, vk: u32, gen: u64, reason: &'static str) {
    crate::commands::system::write_log_line(&format!(
        "[RUST] [ptt] release vk={} setting={} gen={} reason={}",
        vk, state.ptt_setting, gen, reason,
    ));
    let (alt_key, ctrl_key, shift_key, meta_key) = ptt_modifier_flags(&state.ptt_setting);
    let event = PTTEvent {
        source: "rust_hook".to_string(),
        reason: reason.to_string(),
        vk,
        ptt_setting: state.ptt_setting.clone(),
        timestamp: chrono::Utc::now().timestamp_millis(),
        alt_key,
        ctrl_key,
        shift_key,
        meta_key,
    };
    let _ = state.app_handle.emit("ptt-up", &event);
}

#[cfg(any(windows, target_os = "macos"))]
fn spawn_ptt_release_watchdog(
    state: Arc<HookSharedState>,
    action_tx: std::sync::mpsc::Sender<HookAction>,
    vk: u32,
    gen: u64,
) {
    const POLL_MS: u64 = 100;
    // Windows 可配置的首次鍵盤 repeat 延遲最長約 1 秒。留到 1.5 秒，且再要求
    // 連續三次看到物理鍵已鬆開，避免正常長按被短暫狀態抖動提前結束。
    const REPEAT_SILENCE_MS: i64 = 1_500;
    const RELEASED_SAMPLES_REQUIRED: u8 = 3;
    const WARNING_AFTER_SECS: u64 = 4 * 60;
    const HARD_RELEASE_AFTER_SECS: u64 = 5 * 60;

    let fallback_state = state.clone();
    let result = thread::Builder::new()
        .name("ptt-release-watchdog".to_string())
        .spawn(move || {
            let started = std::time::Instant::now();
            let mut released_samples = vec![0_u8; state.ptt_vk_codes.len()];
            let mut warning_sent = false;

            loop {
                thread::sleep(std::time::Duration::from_millis(POLL_MS));

                if state.ptt_active_generation.load(Ordering::SeqCst) != gen {
                    return;
                }

                // 每個成員獨立檢查“repeat 靜默 + 物理狀態”。另一個仍按住成員的
                // repeat 不會再掩蓋已經鬆開但漏掉 key-up 的成員。
                let mut recovered_member: Option<(usize, u32, i64)> = None;
                for (index, &member_vk) in state.ptt_vk_codes.iter().enumerate() {
                    let bit = 1_u64 << index;
                    if state.ptt_pressed_mask.load(Ordering::SeqCst) & bit == 0 {
                        released_samples[index] = 0;
                        continue;
                    }

                    // 見 member watchdog 裡同一處的說明：這套「repeat 靜默 + 非同步鍵狀態」
                    // 的判據對滑鼠按鍵兩個訊號都缺，必然誤判。實測按住側鍵/中鍵 1.7 秒
                    // 就會被判成「漏了 keyup」而強制結束錄音（reason=missing_keyup_release）。
                    if is_mouse_vk(member_vk) {
                        released_samples[index] = 0;
                        continue;
                    }

                    let repeat_silence_ms = now_ms()
                        .saturating_sub(state.ptt_last_down_ms[index].load(Ordering::SeqCst));
                    let physically_down = unsafe { is_ptt_member_physically_down(member_vk) };

                    if repeat_silence_ms >= REPEAT_SILENCE_MS && !physically_down {
                        released_samples[index] = released_samples[index].saturating_add(1);
                        if released_samples[index] >= RELEASED_SAMPLES_REQUIRED
                            && recovered_member.is_none()
                        {
                            recovered_member = Some((index, member_vk, repeat_silence_ms));
                        }
                    } else {
                        released_samples[index] = 0;
                    }
                }

                if let Some((index, member_vk, repeat_silence_ms)) = recovered_member {
                    // 一旦確認至少一個成員漏了 up，就同步清掉當前所有“物理已鬆開”的位。
                    // 這樣兩個成員同時漏 up 時不會留下半個組合；仍物理按住的成員保留，
                    // 避免 repeat 自動把完整組合重新拼起來。
                    let mut cleared_mask = 0_u64;
                    for (other_index, &other_vk) in state.ptt_vk_codes.iter().enumerate() {
                        let bit = 1_u64 << other_index;
                        // 同樣跳過滑鼠按鍵：它的物理狀態查不到，清掉只會誤傷。
                        // （滑鼠鍵目前只允許單鍵格式，走不到這裡，但判據要一致。）
                        if is_mouse_vk(other_vk) {
                            continue;
                        }
                        if state.ptt_pressed_mask.load(Ordering::SeqCst) & bit != 0
                            && !unsafe { is_ptt_member_physically_down(other_vk) }
                        {
                            let _ = release_ptt_member(&state.ptt_pressed_mask, other_index);
                            state
                                .ptt_consumed_down_mask
                                .fetch_and(!bit, Ordering::SeqCst);
                            cleared_mask |= bit;
                        }
                    }

                    let queued = queue_ptt_release(
                        &action_tx,
                        &state.ptt_active_generation,
                        Some(gen),
                        member_vk,
                        "missing_keyup_release",
                    );
                    if queued {
                        crate::commands::system::write_log_line(&format!(
                            "[RUST] [ptt] recovered missing keyup vk={} setting={} gen={} member={} repeat_silence_ms={} cleared_mask=0x{:X}",
                            member_vk,
                            state.ptt_setting,
                            gen,
                            index,
                            repeat_silence_ms,
                            cleared_mask,
                        ));
                    }
                    return;
                }

                let elapsed = started.elapsed();
                if !warning_sent && elapsed.as_secs() >= WARNING_AFTER_SECS {
                    warning_sent = true;
                    crate::commands::system::write_log_line(&format!(
                        "[RUST] [ptt] 4min timeout warning gen={}",
                        gen,
                    ));
                    let (alt_key, ctrl_key, shift_key, meta_key) =
                        ptt_modifier_flags(&state.ptt_setting);
                    let warn_event = PTTEvent {
                        source: "rust_hook".to_string(),
                        reason: "timeout_warning".to_string(),
                        vk,
                        ptt_setting: state.ptt_setting.clone(),
                        timestamp: chrono::Utc::now().timestamp_millis(),
                        alt_key,
                        ctrl_key,
                        shift_key,
                        meta_key,
                    };
                    let _ = state.app_handle.emit("ptt-timeout-warning", &warn_event);
                }

                if elapsed.as_secs() >= HARD_RELEASE_AFTER_SECS {
                    clear_ptt_members(&state);
                    if queue_ptt_release(
                        &action_tx,
                        &state.ptt_active_generation,
                        Some(gen),
                        vk,
                        "hard_timeout_release",
                    ) {
                        crate::commands::system::write_log_line(&format!(
                            "[RUST] [ptt] hard_timeout_release gen={}",
                            gen,
                        ));
                        return;
                    }
                }
            }
        });

    if let Err(error) = result {
        crate::commands::system::write_log_line(&format!(
            "[RUST] [ptt] failed to start release watchdog gen={} error={}",
            gen, error,
        ));
        // 無 watchdog 比立即結束本次錄音更危險。此處本來就在 dispatcher 執行緒：
        // 先 emit，再清 active，仍保證舊 up 不會排到下一次 down 後面。
        clear_ptt_members(&fallback_state);
        if let Some(claimed_gen) =
            claim_ptt_release(&fallback_state.ptt_active_generation, Some(gen))
        {
            emit_ptt_release(&fallback_state, vk, claimed_gen, "watchdog_start_failed");
            let _ = complete_ptt_release(&fallback_state.ptt_active_generation, claimed_gen);
        }
    }
}

/// RAII 計時器：測量 `low_level_keyboard_proc` 單次呼叫耗時，drop 時更新
/// `MAX_CALLBACK_DURATION_US`。用 Drop 而非在每個 return 點手寫更新，這樣
/// 無論函式從哪條路徑返回都會被覆蓋到，不會漏記。
#[cfg(any(windows, target_os = "macos"))]
struct CallbackTimer(std::time::Instant);

#[cfg(any(windows, target_os = "macos"))]
impl Drop for CallbackTimer {
    fn drop(&mut self) {
        let elapsed_us = self.0.elapsed().as_micros() as u64;
        let prev = MAX_CALLBACK_DURATION_US.load(Ordering::Relaxed);
        if elapsed_us > prev {
            MAX_CALLBACK_DURATION_US.store(elapsed_us, Ordering::Relaxed);
        }
    }
}

/// RAII 標記：dispatcher 執行緒存活狀態。構造時置 true，drop 時（無論正常退出
/// 還是 panic 展開）置 false —— 這樣即使 dispatcher 執行緒 panic，全域性健康快照
/// 也能立刻反映出"dispatcher 已死"，不依賴它自己走到正常退出的日誌行。
struct DispatcherAliveGuard;

impl DispatcherAliveGuard {
    fn new() -> Self {
        DISPATCHER_ALIVE.store(true, Ordering::SeqCst);
        Self
    }
}

impl Drop for DispatcherAliveGuard {
    fn drop(&mut self) {
        DISPATCHER_ALIVE.store(false, Ordering::SeqCst);
    }
}

/// 供看門狗執行緒讀取的健康快照，寫入 sayit.log 供事後排查。
/// 復現問題後搜尋 `[ptt-watchdog]` 即可看到當時的鉤子/dispatcher 狀態。
pub fn write_health_snapshot(reason: &str) {
    let last_cb = LAST_CALLBACK_MS.load(Ordering::SeqCst);
    let last_cb_age_ms = if last_cb == 0 { -1 } else { now_ms() - last_cb };
    let dispatcher_alive = DISPATCHER_ALIVE.load(Ordering::SeqCst);
    let hook_running = HOOK_RUNNING.load(Ordering::SeqCst);
    let fail_count = TRY_SEND_FAIL_COUNT.load(Ordering::SeqCst);
    let max_dur_us = MAX_CALLBACK_DURATION_US.load(Ordering::SeqCst);
    crate::commands::system::write_log_line(&format!(
        "[ptt-watchdog] reason={} hook_running={} dispatcher_alive={} last_callback_age_ms={} \
         try_send_fail_count={} max_callback_duration_us={} hook_refreshes={}",
        reason, hook_running, dispatcher_alive, last_cb_age_ms, fail_count, max_dur_us,
        HOOK_REFRESH_COUNT.load(Ordering::SeqCst),
    ));
}

/// 啟動一個每 60 秒取樣一次健康狀態的看門狗執行緒。整個程序生命週期內只需啟動一次
/// （在 main.rs 的 setup 階段呼叫），與具體某次 hook 的 start/stop/reconfigure 無關。
///
/// 取樣廉價、寫日誌剋制：僅在①鉤子/分發執行緒存活狀態翻轉、②投遞失敗計數增加、
/// ③距上次落盤超過心跳間隔（10 分鐘）時才寫一行，避免正常執行時刷屏。
pub fn spawn_health_watchdog() {
    let _ = thread::Builder::new()
        .name("ptt-watchdog".to_string())
        .spawn(|| {
            // last_refresh_ms 只有 Windows 的「鉤子被摘」判據用得到
            #[cfg_attr(not(windows), allow(unused_mut, unused_variables))]
            let mut last_refresh_ms: i64 = 0;
            let mut refresh_logged_at_ms: i64 = 0;
            loop {
                // 15 秒查一次：鉤子被摘後最多一刻鐘內（有輸入的前提下）就能裝回來。
                // 取樣本身只是幾次原子讀和一次 GetLastInputInfo。
                thread::sleep(std::time::Duration::from_secs(15));

                // macOS 不會靜默摘鉤子，但會在回撥超時 / 使用者輸入保護時把 tap 停用
                // （回撥裡收到 kCGEventTapDisabledBy* 時會立刻重開，這裡是兜底）。
                #[cfg(target_os = "macos")]
                if HOOK_RUNNING.load(Ordering::SeqCst) && macos::reenable_tap_if_disabled() {
                    HOOK_REFRESH_COUNT.fetch_add(1, Ordering::SeqCst);
                    if now_ms() - refresh_logged_at_ms >= 30 * 60_000 {
                        refresh_logged_at_ms = now_ms();
                        crate::commands::system::write_log_line(&format!(
                            "[ptt-watchdog] event tap was disabled by the system; re-enabled, total refreshes={}",
                            HOOK_REFRESH_COUNT.load(Ordering::SeqCst),
                        ));
                    }
                }

                #[cfg(windows)]
                if HOOK_RUNNING.load(Ordering::SeqCst) && now_ms() - last_refresh_ms >= 60_000 {
                    let last_cb = LAST_CALLBACK_MS.load(Ordering::SeqCst);
                    let callback_age = if last_cb == 0 { -1 } else { now_ms() - last_cb };
                    let tid = HOOK_THREAD_ID.load(Ordering::SeqCst);
                    if let Some(input_age) = last_input_age_ms() {
                        if tid != 0 && hook_looks_removed(callback_age, input_age) {
                            last_refresh_ms = now_ms();
                            unsafe {
                                let _ = PostThreadMessageW(tid, WM_SAYIT_REFRESH_HOOKS, WPARAM(0), LPARAM(0));
                            }
                            // 只動滑鼠時每分鐘都會走到這裡，日誌半小時最多一行。
                            if now_ms() - refresh_logged_at_ms >= 30 * 60_000 {
                                refresh_logged_at_ms = now_ms();
                                crate::commands::system::write_log_line(&format!(
                                    "[ptt-watchdog] input without hook callback (callback_age_ms={} input_age_ms={}); reinstalling hooks, total refreshes={}",
                                    callback_age, input_age, HOOK_REFRESH_COUNT.load(Ordering::SeqCst),
                                ));
                            }
                        }
                    }
                }

                let hook_running = HOOK_RUNNING.load(Ordering::SeqCst);
                let dispatcher_alive = DISPATCHER_ALIVE.load(Ordering::SeqCst);
                let fail_count = TRY_SEND_FAIL_COUNT.load(Ordering::SeqCst);

                let state_changed = hook_running != WD_LAST_HOOK_RUNNING.load(Ordering::SeqCst)
                    || dispatcher_alive != WD_LAST_DISPATCHER_ALIVE.load(Ordering::SeqCst)
                    || fail_count != WD_LAST_FAIL_COUNT.load(Ordering::SeqCst);

                let last_log = WD_LAST_LOG_MS.load(Ordering::SeqCst);
                let heartbeat_due = last_log == 0 || (now_ms() - last_log) >= WD_HEARTBEAT_MS;

                if state_changed || heartbeat_due {
                    write_health_snapshot(if state_changed { "change" } else { "heartbeat" });
                    WD_LAST_HOOK_RUNNING.store(hook_running, Ordering::SeqCst);
                    WD_LAST_DISPATCHER_ALIVE.store(dispatcher_alive, Ordering::SeqCst);
                    WD_LAST_FAIL_COUNT.store(fail_count, Ordering::SeqCst);
                    WD_LAST_LOG_MS.store(now_ms(), Ordering::SeqCst);
                }
            }
        });
}

/// Dispatcher 執行緒：日誌與 emit（可能阻塞）都在這裡做，鉤子回撥只往佇列裡投遞。
/// Windows 的低階鉤子與 macOS 的 event tap 共用這一份，事件名和載荷只有一處定義。
#[cfg(any(windows, target_os = "macos"))]
fn spawn_dispatcher(
    dispatch_state: Arc<HookSharedState>,
    action_rx: std::sync::mpsc::Receiver<HookAction>,
    watchdog_action_tx: std::sync::mpsc::Sender<HookAction>,
) -> thread::JoinHandle<()> {
        thread::Builder::new()
            .name("ptt-dispatcher".to_string())
            .spawn(move || {
            // Guard 存活期 = 本執行緒存活期；執行緒正常退出或 panic 展開都會觸發 Drop，
            // 從而讓看門狗快照立刻看到 dispatcher_alive=false。
            let _alive_guard = DispatcherAliveGuard::new();
            crate::commands::system::write_log_line("[ptt-dispatcher] thread started");
            while let Ok(action) = action_rx.recv() {
                match action {
                    HookAction::Shutdown => break,
                    HookAction::Diag { vk, msg_name, flags, scan_code } => {
                        crate::commands::system::write_log_line(
                            &format!("[RUST] [hook-diag] vk={} msg={} flags=0x{:X} scanCode=0x{:X}", vk, msg_name, flags, scan_code)
                        );
                    }
                    HookAction::PttDown { vk, gen } => {
                        let setting = &dispatch_state.ptt_setting;
                        crate::commands::system::write_log_line(
                            &format!("[RUST] [ptt] keydown vk={} setting={} gen={}", vk, setting, gen)
                        );
                        let (alt_key, ctrl_key, shift_key, meta_key) =
                            ptt_modifier_flags(setting);
                        let event = PTTEvent {
                            source: "rust_hook".to_string(),
                            reason: "keydown".to_string(),
                            vk,
                            ptt_setting: setting.clone(),
                            timestamp: chrono::Utc::now().timestamp_millis(),
                            alt_key,
                            ctrl_key,
                            shift_key,
                            meta_key,
                        };
                        let _ = dispatch_state.app_handle.emit("ptt-down", &event);
                        spawn_ptt_release_watchdog(
                            dispatch_state.clone(),
                            watchdog_action_tx.clone(),
                            vk,
                            gen,
                        );
                    }
                    HookAction::PttUp { vk, gen, reason } => {
                        emit_ptt_release(&dispatch_state, vk, gen, reason);
                    }
                    HookAction::HfToggle { vk } => {
                        crate::commands::system::write_log_line(
                            &format!("[RUST] [hf] toggle vk={} setting={}", vk, dispatch_state.hf_setting)
                        );
                        let _ = dispatch_state.app_handle.emit("toggle-hands-free", serde_json::json!({
                            "source": "rust_hook",
                            "vk": vk,
                        }));
                    }
                    HookAction::AiToggle { vk } => {
                        crate::commands::system::write_log_line(
                            &format!("[RUST] [ai-toggle] vk={} setting={}", vk, dispatch_state.ai_toggle_setting)
                        );
                        let _ = dispatch_state.app_handle.emit("toggle-ai-cleanup", serde_json::json!({
                            "source": "rust_hook",
                            "vk": vk,
                        }));
                    }
                    HookAction::Escape { mode, token } => {
                        let mode_name = escape_action_mode_name(mode);
                        crate::commands::system::write_log_line(
                            &format!("[RUST] [escape] action mode={mode_name} token={token}")
                        );
                        let _ = dispatch_state.app_handle.emit("escape-action", serde_json::json!({
                            "mode": mode_name,
                            "token": token,
                        }));
                    }
                    HookAction::CardHotkey { action, token } => {
                        let action_name = card_hotkey_action_name(action);
                        crate::commands::system::write_log_line(
                            &format!("[RUST] [card-hotkey] action={action_name} token={token}")
                        );
                        let _ = dispatch_state.app_handle.emit("card-hotkey", serde_json::json!({
                            "action": action_name,
                            "token": token,
                        }));
                    }
                    HookAction::MouseCaptured { vk } => {
                        let setting = match vk {
                            0x04 => "MButton",
                            0x05 => "XButton1",
                            0x06 => "XButton2",
                            0xA6 => "BrowserBack",
                            0xA7 => "BrowserForward",
                            // mac 的 Fn / 🌐 鍵（內部 ID 0xFF，只有 mac 會產生）
                            0xFF => "Fn",
                            _ => "",
                        };
                        crate::commands::system::write_log_line(
                            &format!("[RUST] [shortcut-capture] mouse side button vk={} setting={}", vk, setting)
                        );
                        let _ = dispatch_state.app_handle.emit("mouse-shortcut-captured", serde_json::json!({
                            "setting": setting,
                            "vk": vk,
                        }));
                    }
                    HookAction::PresetSwitch { preset_id } => {
                        crate::commands::system::write_log_line(
                            &format!("[RUST] [preset] switch preset={} (event tap)", preset_id)
                        );
                        // 載荷與 commands/shortcuts.rs 裡系統快捷鍵發的一致
                        let _ = dispatch_state.app_handle.emit("switch-preset", serde_json::json!({
                            "presetId": preset_id,
                        }));
                    }
                }
            }
            log::info!("[ptt] dispatcher thread exited");
            crate::commands::system::write_log_line(
                "[ptt-dispatcher] thread exited (shutdown or all senders dropped)"
            );
            // _alive_guard drops here, flips DISPATCHER_ALIVE back to false.
        }).expect("failed to spawn ptt-dispatcher thread")
}

// Thread-local storage for the hook callback
thread_local! {
    static HOOK_STATE: std::cell::RefCell<Option<Arc<HookSharedState>>> = std::cell::RefCell::new(None);
    /// Non-blocking channel sender for offloading work from the hook callback.
    static HOOK_ACTION_TX: std::cell::RefCell<Option<std::sync::mpsc::Sender<HookAction>>> = std::cell::RefCell::new(None);
}

pub struct KeyboardHookManager {
    hook_thread_id: Mutex<Option<u32>>,
    hook_thread: Mutex<Option<thread::JoinHandle<()>>>,
    shared_state: Mutex<Option<Arc<HookSharedState>>>,
    running: AtomicBool,
}

impl KeyboardHookManager {
    pub fn new() -> Self {
        Self {
            hook_thread_id: Mutex::new(None),
            hook_thread: Mutex::new(None),
            shared_state: Mutex::new(None),
            running: AtomicBool::new(false),
        }
    }

    /// Start the keyboard hook with PTT, hands-free, and AI-cleanup single-key settings.
    pub fn start(&self, app: &AppHandle, ptt_setting: &str, hf_setting: &str, ai_toggle_setting: &str) {
        crate::commands::system::write_log_line(&format!(
            "[ptt-lifecycle] start() called ptt_setting={} hf_setting={} ai_toggle_setting={}",
            ptt_setting, hf_setting, ai_toggle_setting,
        ));
        let has_hook_thread = self.hook_thread.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).is_some();
        if self.running.load(Ordering::SeqCst) || has_hook_thread {
            self.stop();
        }

        let ptt_config = ptt_key_config(ptt_setting);
        let hf_vk_codes = if is_single_key_setting(hf_setting) {
            vk_codes_for_setting(hf_setting)
        } else {
            vec![] // combo key — handled by global_shortcut, not hook
        };
        let ai_toggle_vk_codes = if is_single_key_setting(ai_toggle_setting) {
            vk_codes_for_setting(ai_toggle_setting)
        } else {
            vec![] // combo key — handled by global_shortcut, not hook
        };
        let ptt_full_mask = if ptt_config.vk_codes.is_empty() {
            0
        } else {
            (1_u64 << ptt_config.vk_codes.len()) - 1
        };
        let ptt_last_down_ms = (0..ptt_config.vk_codes.len())
            .map(|_| AtomicI64::new(0))
            .collect();

        let state = Arc::new(HookSharedState {
            ptt_vk_codes: ptt_config.vk_codes,
            ptt_setting: ptt_config.setting,
            ptt_pressed_mask: AtomicU64::new(0),
            ptt_full_mask,
            ptt_modifier_mask: ptt_config.modifier_mask,
            ptt_consumed_down_mask: AtomicU64::new(0),
            ptt_active_generation: AtomicU64::new(0),
            ptt_generation: AtomicU64::new(0),
            ptt_last_down_ms,
            ptt_hook_alive: AtomicBool::new(true),
            hands_free_active: AtomicBool::new(false),
            hf_key_down: AtomicBool::new(false),
            hf_vk_codes,
            hf_setting: hf_setting.to_string(),
            ai_toggle_key_down: AtomicBool::new(false),
            ai_toggle_vk_codes,
            ai_toggle_setting: ai_toggle_setting.to_string(),
            app_handle: app.clone(),
        });

        *self.shared_state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(state.clone());
        self.running.store(true, Ordering::SeqCst);

        let state_for_thread = state.clone();
        let (tx, rx) = std::sync::mpsc::channel::<u32>();

        let hook_thread = thread::spawn(move || {
            Self::hook_thread(state_for_thread, tx);
        });
        *self.hook_thread.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(hook_thread);

        // Wait for the thread to report its ID
        if let Ok(thread_id) = rx.recv_timeout(std::time::Duration::from_secs(5)) {
            *self.hook_thread_id.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(thread_id);
            HOOK_RUNNING.store(true, Ordering::SeqCst);
            log::info!("Keyboard hook started, thread_id={}", thread_id);
            crate::commands::system::write_log_line(&format!(
                "[ptt-lifecycle] hook started OK thread_id={}", thread_id,
            ));
        } else {
            log::error!("Keyboard hook thread failed to start");
            crate::commands::system::write_log_line(
                "[ptt-lifecycle] hook FAILED to start (rx.recv_timeout expired after 5s)"
            );
            self.running.store(false, Ordering::SeqCst);
            HOOK_RUNNING.store(false, Ordering::SeqCst);
        }
    }

    /// Stop the keyboard hook
    pub fn stop(&self) {
        crate::commands::system::write_log_line("[ptt-lifecycle] stop() called");
        self.running.store(false, Ordering::SeqCst);
        HOOK_RUNNING.store(false, Ordering::SeqCst);

        // 先清成員位，但不在呼叫執行緒清活動代次。hook 執行緒退出時會在最後一個 callback
        // 完成後，用同一個 sender 依次排入配對 up 和 Shutdown，避免 stop 與 down 入隊競態。
        #[cfg(windows)]
        if let Some(state) = self.shared_state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_ref() {
            let had_active = state.ptt_active_generation.load(Ordering::SeqCst) != 0;
            clear_ptt_members(state);

            if had_active {
                // 保留既有的 synthetic keyup，用於清理由舊版本遺留在 Windows 中的狀態。
                // 滑鼠側鍵不涉及鍵盤狀態，無需（也不能）補發鍵盤 keyup。
                if !is_mouse_button_setting(&state.ptt_setting) {
                    for &vk in &state.ptt_vk_codes {
                        unsafe {
                            use windows::Win32::UI::Input::KeyboardAndMouse::*;
                            let mut input = INPUT {
                                r#type: INPUT_KEYBOARD,
                                ..std::mem::zeroed()
                            };
                            input.Anonymous.ki = KEYBDINPUT {
                                wVk: VIRTUAL_KEY(vk as u16),
                                dwFlags: KEYEVENTF_KEYUP,
                                ..std::mem::zeroed()
                            };
                            SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
                        }
                    }
                    log::info!("[ptt] sent synthetic keyup on stop to clear keyboard state");
                }
            }
        }

        // macOS：我們從不吞修飾鍵，系統的鍵盤狀態不會被留住，不需要補發 keyup。
        #[cfg(target_os = "macos")]
        if let Some(state) = self.shared_state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_ref() {
            clear_ptt_members(state);
        }

        if let Some(thread_id) = self.hook_thread_id.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take() {
            #[cfg(windows)]
            unsafe {
                let _ = PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            #[cfg(target_os = "macos")]
            {
                let _ = thread_id;
                macos::request_tap_thread_stop();
            }
        }

        // 等 hook 執行緒解除安裝鉤子，並等它內部的 dispatcher 處理 Shutdown 後真正退出。
        // 這是重配的跨例項屏障：舊例項不可能在新例項 ptt-down 之後再發舊 ptt-up。
        if let Some(hook_thread) = self.hook_thread.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take() {
            if hook_thread.join().is_err() {
                crate::commands::system::write_log_line(
                    "[ptt-lifecycle] hook thread panicked while stopping",
                );
            }
        }

        #[cfg(any(windows, target_os = "macos"))]
        if let Some(state) = self.shared_state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_ref() {
            // 安裝失敗/panic 時 hook 執行緒可能沒機會排 up；至少不要把 native 狀態留住。
            clear_ptt_members(state);
            let _ = clear_ptt_press(&state.ptt_active_generation);
        }
        *self.shared_state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    /// Reconfigure with new PTT, hands-free, and AI-cleanup settings.
    pub fn reconfigure(&self, app: &AppHandle, ptt_setting: &str, hf_setting: &str, ai_toggle_setting: &str) {
        let count = RECONFIGURE_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
        crate::commands::system::write_log_line(&format!(
            "[ptt-lifecycle] reconfigure() #{} ptt_setting={} hf_setting={} ai_toggle_setting={} — waiting for old hook shutdown",
            count, ptt_setting, hf_setting, ai_toggle_setting,
        ));
        self.stop();
        // stop() 已 join 舊 hook 與 dispatcher，可直接啟動新例項。
        self.start(app, ptt_setting, hf_setting, ai_toggle_setting);
    }

    /// Set hands-free mode active (suppresses PTT up events temporarily)
    #[allow(dead_code)]
    pub fn set_hands_free(&self, active: bool) {
        if let Some(state) = self.shared_state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_ref() {
            state.hands_free_active.store(active, Ordering::SeqCst);
            if active {
                clear_ptt_members(state);
                let _ = clear_ptt_press(&state.ptt_active_generation);
            }
        }
    }

    #[cfg(windows)]
    fn hook_thread(state: Arc<HookSharedState>, tx: std::sync::mpsc::Sender<u32>) {
        use windows::Win32::System::Threading::GetCurrentThreadId;

        log::info!(
            "keyboard hook thread starting, ptt_setting={} vk_codes={:?}",
            state.ptt_setting, state.ptt_vk_codes
        );

        // 無界 mpsc::Sender::send 不會等待消費者，可安全用於低階 hook 回撥；相比原來的
        // 64 槽 sync_channel，它不會被按住熱鍵產生的診斷 repeat 填滿而丟失關鍵 up。
        let (action_tx, action_rx) = std::sync::mpsc::channel::<HookAction>();
        let watchdog_action_tx = action_tx.clone();
        let member_watchdog = spawn_ptt_member_watchdog(state.clone(), action_tx.clone());

        let dispatcher_thread = spawn_dispatcher(state.clone(), action_rx, watchdog_action_tx);

        // Set thread-local state for the callback
        HOOK_STATE.with(|s| {
            *s.borrow_mut() = Some(state.clone());
        });
        HOOK_ACTION_TX.with(|s| {
            *s.borrow_mut() = Some(action_tx);
        });

        unsafe {
            let install_hook = || -> Option<windows::Win32::UI::WindowsAndMessaging::HHOOK> {
                match SetWindowsHookExW(
                    WH_KEYBOARD_LL,
                    Some(low_level_keyboard_proc),
                    None,
                    0,
                ) {
                    Ok(h) => {
                        log::info!("SetWindowsHookExW succeeded: {:?}", h.0);
                        Some(h)
                    }
                    Err(e) => {
                        log::error!("SetWindowsHookExW failed: {}", e);
                        None
                    }
                }
            };

            let mut hook = match install_hook() {
                Some(h) => h,
                None => {
                    state.ptt_hook_alive.store(false, Ordering::SeqCst);
                    if let Some(handle) = member_watchdog {
                        let _ = handle.join();
                    }
                    HOOK_ACTION_TX.with(|tx| {
                        if let Some(sender) = tx.borrow_mut().take() {
                            let _ = sender.send(HookAction::Shutdown);
                        }
                    });
                    HOOK_STATE.with(|s| {
                        *s.borrow_mut() = None;
                    });
                    let _ = dispatcher_thread.join();
                    return;
                }
            };

            let install_mouse_hook = || -> Option<windows::Win32::UI::WindowsAndMessaging::HHOOK> {
                match SetWindowsHookExW(WH_MOUSE_LL, Some(low_level_mouse_proc), None, 0) {
                    Ok(h) => {
                        log::info!("SetWindowsHookExW(WH_MOUSE_LL) succeeded: {:?}", h.0);
                        crate::commands::system::write_log_line(
                            "[ptt-lifecycle] mouse hook installed (side-button support active)",
                        );
                        Some(h)
                    }
                    Err(e) => {
                        log::error!("SetWindowsHookExW(WH_MOUSE_LL) failed: {}", e);
                        crate::commands::system::write_log_line(&format!(
                            "[ptt-lifecycle] mouse hook FAILED to install: {}", e,
                        ));
                        None
                    }
                }
            };
            let mut mouse_hook = if needs_mouse_hook(
                &state.ptt_vk_codes,
                &state.hf_vk_codes,
                &state.ai_toggle_vk_codes,
                SHORTCUT_CAPTURE.load(Ordering::SeqCst),
            ) {
                install_mouse_hook()
            } else {
                crate::commands::system::write_log_line(
                    "[ptt-lifecycle] mouse hook not installed (no mouse button is bound)",
                );
                None
            };

            let thread_id = GetCurrentThreadId();
            let _ = tx.send(thread_id);
            HOOK_THREAD_ID.store(thread_id, Ordering::SeqCst);
            log::info!("keyboard hook message loop starting on thread {}", thread_id);
            // 訊息迴圈開始前錄製可能已經開啟（那條訊息發給了舊執行緒或還沒執行緒），補一次。
            if mouse_hook.is_none() && SHORTCUT_CAPTURE.load(Ordering::SeqCst) {
                mouse_hook = install_mouse_hook();
            }

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                if msg.hwnd.0.is_null() && msg.message == WM_SAYIT_NEED_MOUSE_HOOK {
                    if mouse_hook.is_none() {
                        mouse_hook = install_mouse_hook();
                    }
                    continue;
                }
                if msg.hwnd.0.is_null() && msg.message == WM_SAYIT_REFRESH_HOOKS {
                    // 先裝新的再卸舊的：這段程式碼跑在訊息迴圈執行緒上，期間不會處理任何鉤子回撥。
                    if let Some(new_hook) = install_hook() {
                        let _ = UnhookWindowsHookEx(hook);
                        hook = new_hook;
                    }
                    if let Some(old_mouse) = mouse_hook {
                        if let Some(new_mouse) = install_mouse_hook() {
                            let _ = UnhookWindowsHookEx(old_mouse);
                            mouse_hook = Some(new_mouse);
                        }
                    }
                    HOOK_REFRESH_COUNT.fetch_add(1, Ordering::SeqCst);
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            let _ = HOOK_THREAD_ID.compare_exchange(thread_id, 0, Ordering::SeqCst, Ordering::SeqCst);
            log::info!("keyboard hook message loop exited");
            let _ = UnhookWindowsHookEx(hook);
            if let Some(mh) = mouse_hook {
                let _ = UnhookWindowsHookEx(mh);
            }
        }

        // dispatcher 為 watchdog 持有 sender clone，不能再依賴“所有 sender drop”退出；
        // hook 訊息迴圈結束時顯式關閉，避免每次 reconfigure 洩漏執行緒和 AppHandle。
        // 與 callback 共用同一個 sender：最後一個 callback 已結束後，嚴格按 FIFO 排入
        // manager_stop up，再排 Shutdown。即使 stop 與 down callback 併發，也不會出現 up 先於 down。
        state.ptt_hook_alive.store(false, Ordering::SeqCst);
        if let Some(handle) = member_watchdog {
            if handle.join().is_err() {
                crate::commands::system::write_log_line(
                    "[ptt-lifecycle] member watchdog panicked while stopping",
                );
            }
        }
        HOOK_ACTION_TX.with(|s| {
            if let Some(sender) = s.borrow_mut().take() {
                clear_ptt_members(&state);
                let vk = state.ptt_vk_codes.first().copied().unwrap_or(0);
                let _ = queue_ptt_release(
                    &sender,
                    &state.ptt_active_generation,
                    None,
                    vk,
                    "manager_stop",
                );
                let _ = sender.send(HookAction::Shutdown);
            }
        });
        HOOK_STATE.with(|s| {
            *s.borrow_mut() = None;
        });
        if dispatcher_thread.join().is_err() {
            crate::commands::system::write_log_line(
                "[ptt-lifecycle] dispatcher thread panicked while stopping",
            );
        }
    }

    /// macOS：CGEventTap，見 keyboard/macos.rs。
    #[cfg(target_os = "macos")]
    fn hook_thread(state: Arc<HookSharedState>, tx: std::sync::mpsc::Sender<u32>) {
        macos::hook_thread(state, tx);
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    fn hook_thread(_state: Arc<HookSharedState>, tx: std::sync::mpsc::Sender<u32>) {
        let _ = tx.send(0);
        // No-op on non-Windows
    }
}

/// 低階滑鼠鉤子回撥：只處理滑鼠側鍵（XButton1/2）的按下/抬起，複用與鍵盤鉤子
/// 完全相同的 PTT/免持流水線（HookAction -> dispatcher -> emit）。
///
/// ⚠️ 效能：WH_MOUSE_LL 會在**每次滑鼠移動**都被呼叫。因此第一件事就是判斷
/// “不是側鍵的按下/抬起就立刻放行”，絕不在滑鼠移動這條最熱的路徑上做任何多餘的事。
#[cfg(windows)]
unsafe extern "system" fn low_level_mouse_proc(
    n_code: i32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    let msg = w_param.0 as u32;

    // ── 最熱路徑：移動/滾輪/其它鍵一律立刻放行，連結構體都不解引用 ──
    // 只處理側鍵(XBUTTON)和中鍵(MBUTTON)的按下/抬起。
    if n_code < 0
        || (msg != WM_XBUTTONDOWN
            && msg != WM_XBUTTONUP
            && msg != WM_MBUTTONDOWN
            && msg != WM_MBUTTONUP)
    {
        return CallNextHookEx(None, n_code, w_param, l_param);
    }

    let ms = &*(l_param.0 as *const MSLLHOOKSTRUCT);

    // 過濾注入（合成）的滑鼠事件，只認真實硬體按鍵。
    const LLMHF_INJECTED: u32 = 0x00000001;
    if (ms.flags & LLMHF_INJECTED) != 0 {
        return CallNextHookEx(None, n_code, w_param, l_param);
    }

    // 中鍵：直接是 VK_MBUTTON(0x04)，無需從 mouseData 解碼。
    // 側鍵：編號在 mouseData 高 16 位：1=XBUTTON1（後退鍵） 2=XBUTTON2（前進鍵）。
    let is_middle = msg == WM_MBUTTONDOWN || msg == WM_MBUTTONUP;
    let vk: u32 = if is_middle {
        0x04 // VK_MBUTTON
    } else {
        match (ms.mouseData >> 16) & 0xFFFF {
            1 => 0x05, // VK_XBUTTON1
            2 => 0x06, // VK_XBUTTON2
            _ => return CallNextHookEx(None, n_code, w_param, l_param),
        }
    };

    let is_down = msg == WM_XBUTTONDOWN || msg == WM_MBUTTONDOWN;
    let is_up = msg == WM_XBUTTONUP || msg == WM_MBUTTONUP;

    // ── 錄製捕獲模式：把側鍵吞掉並回報給設定頁，避免 webview 把它當成“後退”導航 ──
    if SHORTCUT_CAPTURE.load(Ordering::SeqCst) {
        if is_down {
            SHORTCUT_CAPTURE.store(false, Ordering::SeqCst);
            CONSUME_XUP_VK.store(vk, Ordering::SeqCst); // 記住 vk，好把配對的 up 也吞掉
            HOOK_ACTION_TX.with(|tx| {
                if let Some(sender) = tx.borrow().as_ref() {
                    if sender.send(HookAction::MouseCaptured { vk }).is_err() {
                        TRY_SEND_FAIL_COUNT.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
        return LRESULT(1); // 捕獲期間側鍵的 down/up 一律吞掉
    }
    // 吞掉“捕獲到的那次 down”配對的 up，避免殘留“後退”導航。
    if is_up && CONSUME_XUP_VK.load(Ordering::SeqCst) == vk {
        CONSUME_XUP_VK.store(0, Ordering::SeqCst);
        return LRESULT(1);
    }

    let mut consumed = false;

    HOOK_STATE.with(|s| {
        if let Some(state) = s.borrow().as_ref() {
            let ptt_member = ptt_member_index(&state.ptt_vk_codes, vk);
            let is_ptt_key = ptt_member.is_some();
            let is_hf_key = !state.hf_vk_codes.is_empty()
                && state.hf_vk_codes.contains(&vk)
                && !is_ptt_key; // 同一鍵時 PTT 優先
            let is_ai_toggle_key = !state.ai_toggle_vk_codes.is_empty()
                && state.ai_toggle_vk_codes.contains(&vk)
                && !is_ptt_key
                && !is_hf_key; // PTT / 免持高於 AI 開關

            if let Some(member_index) = ptt_member {
                // 滑鼠按鍵只允許舊單鍵格式；按下和抬起都吞掉，避免誤觸發其它程式導航。
                consumed = true;

                if is_down {
                    state.ptt_last_down_ms[member_index].store(now_ms(), Ordering::SeqCst);
                    let became_complete = press_ptt_member(
                        &state.ptt_pressed_mask,
                        member_index,
                        state.ptt_full_mask,
                    );
                    if became_complete && !state.hands_free_active.load(Ordering::SeqCst) {
                        if let Some(gen) = begin_ptt_press(
                            &state.ptt_active_generation,
                            &state.ptt_generation,
                        ) {
                            HOOK_ACTION_TX.with(|tx| {
                                if let Some(sender) = tx.borrow().as_ref() {
                                    if sender.send(HookAction::PttDown { vk, gen }).is_err() {
                                        let _ = release_ptt_member(
                                            &state.ptt_pressed_mask,
                                            member_index,
                                        );
                                        cancel_ptt_press_start(&state.ptt_active_generation, gen);
                                        TRY_SEND_FAIL_COUNT.fetch_add(1, Ordering::Relaxed);
                                    }
                                }
                            });
                        }
                    }
                }

                if is_up && release_ptt_member(&state.ptt_pressed_mask, member_index) {
                    HOOK_ACTION_TX.with(|tx| {
                        if let Some(sender) = tx.borrow().as_ref() {
                            let _ = queue_ptt_release(
                                sender,
                                &state.ptt_active_generation,
                                None,
                                vk,
                                "keyup",
                            );
                        }
                    });
                }
            }

            if is_hf_key {
                // 免持在首個按下時立即觸發；hf_key_down 過濾滑鼠驅動可能產生的重複 down。
                if is_down {
                    consumed = true;
                    if begin_hf_press(&state.hf_key_down) {
                        HOOK_ACTION_TX.with(|tx| {
                            if let Some(sender) = tx.borrow().as_ref() {
                                if sender.send(HookAction::HfToggle { vk }).is_err() {
                                    TRY_SEND_FAIL_COUNT.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        });
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
                        HOOK_ACTION_TX.with(|tx| {
                            if let Some(sender) = tx.borrow().as_ref() {
                                if sender.send(HookAction::AiToggle { vk }).is_err() {
                                    TRY_SEND_FAIL_COUNT.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        });
                    }
                }
                if is_up && end_hf_press(&state.ai_toggle_key_down) {
                    consumed = true;
                }
            }
        }
    });

    if consumed {
        return LRESULT(1);
    }
    CallNextHookEx(None, n_code, w_param, l_param)
}

#[cfg(windows)]
unsafe extern "system" fn low_level_keyboard_proc(
    n_code: i32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    // 計時器：無論下面走哪條 return 路徑，drop 時都會把本次呼叫耗時彙報進
    // MAX_CALLBACK_DURATION_US，用來確認回撥是否在正常情況下也逼近了 Windows
    // 靜默摘除鉤子的 ~200ms 閾值。
    let _timer = CallbackTimer(std::time::Instant::now());
    // 任意按鍵（不限於 PTT 鍵）都重新整理這個時間戳——只要這個值還在正常前進，
    // 就說明 Windows 仍在呼叫本回撥，鉤子沒有被系統摘除。
    LAST_CALLBACK_MS.store(now_ms(), Ordering::Relaxed);

    if n_code >= 0 {
        let kb = &*(l_param.0 as *const KBDLLHOOKSTRUCT);
        let vk = kb.vkCode;
        let msg = w_param.0 as u32;

        // 過濾 Windows 合成的"幻影 Alt"：滑鼠點選跨會話邊界（如從本機點進遠端桌面
        // 視窗）觸發焦點切換時，Windows 會自動合成一個 Alt keydown/keyup 用於清理
        // 選單導航狀態，與使用者真實按鍵幾乎無法區分——唯二的區別是：
        //   1) LLKHF_INJECTED (0x10) / LLKHF_LOWER_IL_INJECTED (0x02) 標誌位被置位；
        //   2) scanCode 通常為 0（真實鍵盤按鍵的 scanCode 非零）。
        // 命中任一條件即認為是系統合成按鍵，直接放行給系統處理，不當作使用者按鍵。
        const LLKHF_INJECTED: u32 = 0x10;
        const LLKHF_LOWER_IL_INJECTED: u32 = 0x02;

        // 儲存本次事件的 flags/scanCode，供下方「命中熱鍵時」的診斷日誌使用。
        let kb_flags = kb.flags.0;
        let kb_scan_code = kb.scanCode;

        let is_kdown = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let is_kup = msg == WM_KEYUP || msg == WM_SYSKEYUP;

        // ── 側鍵錄製捕獲（鍵盤側）：必須放在下面的“幻影過濾”之前，因為羅技等改鍵滑鼠
        //    發出的瀏覽器後退/前進鍵通常帶“注入”標誌，會被幻影過濾器直接丟掉。 ──
        if SHORTCUT_CAPTURE.load(Ordering::SeqCst) {
            // 捕獲模式中的任何鍵都必須先結束仍活動的 PTT；放在 BrowserBack/Forward
            // 早退之前，否則這兩個鍵可讓前端錄音只剩 5 分鐘硬截止。
            HOOK_STATE.with(|s| {
                if let Some(state) = s.borrow().as_ref() {
                    clear_ptt_members(state);
                    if let Some(&ptt_vk) = state.ptt_vk_codes.first() {
                        HOOK_ACTION_TX.with(|tx| {
                            if let Some(sender) = tx.borrow().as_ref() {
                                let _ = queue_ptt_release(
                                    sender,
                                    &state.ptt_active_generation,
                                    None,
                                    ptt_vk,
                                    "shortcut_capture",
                                );
                            }
                        });
                    }
                    state.hf_key_down.store(false, Ordering::SeqCst);
                }
            });

            // 瀏覽器後退/前進鍵 → 作為側鍵繫結，併吞掉避免 webview 後退/前進導航。
            if vk == 0xA6 || vk == 0xA7 {
                if is_kdown {
                    SHORTCUT_CAPTURE.store(false, Ordering::SeqCst);
                    HOOK_ACTION_TX.with(|tx| {
                        if let Some(sender) = tx.borrow().as_ref() {
                            let _ = sender.send(HookAction::MouseCaptured { vk });
                        }
                    });
                }
                return LRESULT(1);
            }

            // 其餘按鍵一律放行給 webview 去繫結，本輪不做任何 PTT/免持判定：
            // 錄製時按到已繫結的熱鍵（如免持的右 Alt）應該被錄進設定或提示衝突，
            // 而不是觸發錄音彈出懸浮窗。
            return CallNextHookEx(None, n_code, w_param, l_param);
        }

        // 幻影過濾：注入或 scanCode==0 視為系統合成鍵，放行、不當作使用者按鍵。
        // 例外見 is_injection_exempt_vk（側鍵改寫鍵、F13–F24）——這些鍵的正常來源就是
        // 注入，按幻影丟棄會讓使用者綁了不生效，且不留任何日誌線索。
        let is_synthetic = ((kb.flags.0 & (LLKHF_INJECTED | LLKHF_LOWER_IL_INJECTED)) != 0
            || kb.scanCode == 0)
            && !is_injection_exempt_vk(vk);
        if is_synthetic {
            // 只接受“已有真實 PTT down”的合成抬起作為釋放訊號。Windows 輔助功能、
            // 遠端桌面或驅動可能把配對 up 標成 injected/scanCode=0；此前在這裡直接
            // 早退會讓錄音永久卡住。孤立 synthetic up 因沒有活動代次仍會被忽略。
            let mut consume_paired_up = false;
            if is_kup {
                HOOK_STATE.with(|s| {
                    if let Some(state) = s.borrow().as_ref() {
                        // 真實 HF down 的配對 up 偶爾會被遠端桌面/驅動標成 synthetic。
                        // 即使不把它當作一次觸發，也必須清掉按下狀態，否則下一次真實按下
                        // 會被誤認成 repeat 而沒有反應。
                        if state.hf_vk_codes.contains(&vk) {
                            consume_paired_up = end_hf_press(&state.hf_key_down);
                        }
                        if let Some(member_index) = ptt_member_index(&state.ptt_vk_codes, vk) {
                            let member_bit = 1_u64 << member_index;
                            consume_paired_up = state
                                .ptt_consumed_down_mask
                                .fetch_and(!member_bit, Ordering::SeqCst)
                                & member_bit
                                != 0;
                            let physically_down = unsafe { is_ptt_member_physically_down(vk) };
                            if !physically_down
                                && release_ptt_member(&state.ptt_pressed_mask, member_index)
                            {
                                HOOK_ACTION_TX.with(|tx| {
                                    if let Some(sender) = tx.borrow().as_ref() {
                                        let _ = queue_ptt_release(
                                            sender,
                                            &state.ptt_active_generation,
                                            None,
                                            vk,
                                            "synthetic_keyup",
                                        );
                                    }
                                });
                            }
                        }
                    }
                });
            }
            if consume_paired_up {
                return LRESULT(1);
            }
            return CallNextHookEx(None, n_code, w_param, l_param);
        }

        // overlay 不聚焦，錄音中/處理中/兜底卡片的 Esc 必須在這裡全域性捕獲。
        // 只在前端短暫開啟的模式下吞鍵；配對的 keyup 即使模式已被前端關閉也繼續吞掉，
        // 避免目標應用只收到一個孤立的 Esc 抬起事件。
        const VK_ESCAPE: u32 = 0x1B;
        if vk == VK_ESCAPE {
            let is_kup = msg == WM_KEYUP || msg == WM_SYSKEYUP;
            if is_kdown {
                if ESCAPE_KEY_DOWN.load(Ordering::SeqCst) {
                    return LRESULT(1);
                }
                let (mode, token) = active_escape_action();
                if mode != ESCAPE_MODE_OFF {
                    let first_down = !ESCAPE_KEY_DOWN.swap(true, Ordering::SeqCst);
                    if first_down {
                        HOOK_ACTION_TX.with(|tx| {
                            if let Some(sender) = tx.borrow().as_ref() {
                                if sender.send(HookAction::Escape { mode, token }).is_err() {
                                    TRY_SEND_FAIL_COUNT.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        });
                    }
                    return LRESULT(1);
                }
            }
            if is_kup && ESCAPE_KEY_DOWN.swap(false, Ordering::SeqCst) {
                return LRESULT(1);
            }
        }

        // 懸浮窗卡片的 Ctrl+C。判據與上面的 Esc 同構（mask + token + deadline），
        // 額外兩道：這一刻 Ctrl 必須按著，且前台視窗仍是卡片彈出時那個 —— 使用者切走
        // 之後，他的 Ctrl+C 就該歸他自己。
        const VK_C: u32 = 0x43;
        if vk == VK_C {
            if is_kdown {
                if CARD_HOTKEY_C_DOWN.load(Ordering::SeqCst) {
                    // 按住不放產生的 repeat：繼續吞，但不重複投遞動作。
                    return LRESULT(1);
                }
                let (mask, token) = active_card_hotkeys();
                if mask & CARD_HOTKEY_COPY != 0
                    && only_ctrl_is_down(kb_flags)
                    && card_hotkey_foreground_matches()
                {
                    CARD_HOTKEY_C_DOWN.store(true, Ordering::SeqCst);
                    HOOK_ACTION_TX.with(|tx| {
                        if let Some(sender) = tx.borrow().as_ref() {
                            if sender
                                .send(HookAction::CardHotkey { action: CARD_HOTKEY_COPY, token })
                                .is_err()
                            {
                                TRY_SEND_FAIL_COUNT.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    });
                    return LRESULT(1);
                }
            }
            if is_kup && CARD_HOTKEY_C_DOWN.swap(false, Ordering::SeqCst) {
                return LRESULT(1);
            }
        }

        // ── CRITICAL: This callback MUST return within ~200ms or Windows
        // will silently remove the hook. NO blocking operations allowed.
        // All logging and emit are offloaded through the non-blocking action channel.

        let mut consumed = false;

        HOOK_STATE.with(|s| {
            if let Some(state) = s.borrow().as_ref() {
                let ptt_member = ptt_member_index(&state.ptt_vk_codes, vk);
                let is_ptt_key = ptt_member.is_some();
                let is_hf_key = !state.hf_vk_codes.is_empty()
                    && state.hf_vk_codes.contains(&vk)
                    && !is_ptt_key; // PTT takes priority if same key
                let is_ai_toggle_key = !state.ai_toggle_vk_codes.is_empty()
                    && state.ai_toggle_vk_codes.contains(&vk)
                    && !is_ptt_key
                    && !is_hf_key; // PTT / hands-free take priority if misconfigured

                // Alt 鍵（VK_LMENU=0xA4 / VK_RMENU=0xA5）作為單鍵熱鍵時，keyup 也必須吞掉。
                // 否則被放行的 Alt 抬起會被 Windows 判為「單擊了一下 Alt」→ 啟用前台程式的
                // 選單欄（如記事本顯示「檔案(F)」等加速鍵下劃線），搶走輸入框焦點，導致隨後
                // 插字落到選單上、實際沒插進去（而 SendInput 仍上報成功，形成假成功）。
                // keydown 既已被吞、系統自始至終沒見過這個 Alt，連 keyup 一起吞不會造成
                // 「修飾鍵卡住」。非 Alt 單鍵（F 鍵、Space、側鍵等）維持原狀：只吞 keydown。
                let is_alt_key = vk == 0xA4 || vk == 0xA5;

                // 單鍵熱鍵必須是「裸按」才算命中：Alt+F4 不該喚起綁在 F4 上的免持。
                //
                // ⚠️ **只能用在 keydown 上。** keyup 一律照原樣處理 —— 否則
                // 「按 F4 開始錄音 → 再按住 Alt → 鬆開 F4」會因為鬆開那一刻有修飾鍵
                // 而跳過釋放，錄音就再也停不下來了（只能等 5 分鐘硬截止）。
                let modifiers_block_down = single_key_blocked_by_modifiers(vk, kb_flags);

                // ── 診斷：僅在「當前配置的 PTT/免持熱鍵」被按下時記錄 flags/scanCode ──
                // 只記每個組合成員的首個 down，不把按住產生的 repeat 全部塞進無界佇列；
                // 漏 key-up 看門狗直接重新整理該成員的 last_down，不依賴診斷日誌。
                let should_log_hotkey_down = is_kdown
                    && ((ptt_member.is_some_and(|index| {
                        state.ptt_pressed_mask.load(Ordering::SeqCst) & (1_u64 << index) == 0
                    }) && !state.hands_free_active.load(Ordering::SeqCst))
                        || (is_hf_key && !state.hf_key_down.load(Ordering::SeqCst)));
                if should_log_hotkey_down {
                    // 兩種名字要分開：不然「按 Alt+F4 沒反應」和「熱鍵壓根沒送到」
                    // 在日誌裡長得一模一樣，排查時無從下手。
                    // 組合鍵 PTT 命中時 modifiers_block_down 也可能為真（它就是要修飾鍵），
                    // 那種情況按下面 bare_press_violated 的規則放行，這條日誌只作提示。
                    let msg_name = if modifiers_block_down {
                        "hotkey-down-with-modifier"
                    } else {
                        "hotkey-down"
                    };
                    HOOK_ACTION_TX.with(|tx| {
                        if let Some(sender) = tx.borrow().as_ref() {
                            if sender.send(HookAction::Diag {
                                vk, msg_name, flags: kb_flags, scan_code: kb_scan_code,
                            }).is_err() {
                                TRY_SEND_FAIL_COUNT.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    });
                }

                if let Some(member_index) = ptt_member {
                    if is_kdown {
                        reconcile_stale_ptt_members_before_down(state, member_index);
                    }
                    let member_bit = 1_u64 << member_index;
                    let is_combo = state.ptt_vk_codes.len() > 1;
                    let is_pure_modifier_combo = is_combo
                        && state.ptt_modifier_mask == state.ptt_full_mask;
                    let is_main_key = state.ptt_modifier_mask & member_bit == 0;
                    // 「裸按」規則只套單鍵 PTT。組合鍵（Alt+F4 之類）本來就要求修飾鍵
                    // 按住，套上去等於把它自己永久擋死。
                    let bare_press_violated = !is_combo && modifiers_block_down;

                    // 舊單鍵維持原消費策略。普通組合僅當主鍵首次按下時所需修飾鍵
                    // 已全部按住，才吞主鍵 down，並用點陣圖保證只吞它配對的 up。
                    // 若順序是 K→Ctrl，組合仍可開始，但 K 的 down/up 都放行，不製造孤立事件。
                    if !is_combo {
                        // 帶修飾鍵按下時整條放行：不吞，讓 Alt+F4 這類系統/程式快捷鍵
                        // 照常送到前台程式。下面的按下記帳也一併跳過。
                        if (is_kdown && !bare_press_violated) || (is_kup && is_alt_key) {
                            consumed = true;
                        }
                    } else if !is_pure_modifier_combo && is_main_key {
                        if is_kdown {
                            let pressed_before =
                                state.ptt_pressed_mask.load(Ordering::SeqCst);
                            let consume_this_press = should_consume_combo_main_down(
                                pressed_before,
                                state.ptt_consumed_down_mask.load(Ordering::SeqCst),
                                member_bit,
                                state.ptt_modifier_mask,
                            );
                            if consume_this_press {
                                state
                                    .ptt_consumed_down_mask
                                    .fetch_or(member_bit, Ordering::SeqCst);
                                consumed = true;
                            }
                        }
                        if is_kup
                            && state
                                .ptt_consumed_down_mask
                                .fetch_and(!member_bit, Ordering::SeqCst)
                                & member_bit
                                != 0
                        {
                            consumed = true;
                        }
                    }

                    // 記帳也要跳過：否則 Alt+F4 雖然沒被吞，錄音照樣開始了。
                    if is_kdown && !bare_press_violated {
                        state.ptt_last_down_ms[member_index].store(now_ms(), Ordering::SeqCst);
                        let became_complete = press_ptt_member(
                            &state.ptt_pressed_mask,
                            member_index,
                            state.ptt_full_mask,
                        );
                        if became_complete && !state.hands_free_active.load(Ordering::SeqCst) {
                            if let Some(gen) = begin_ptt_press(
                                &state.ptt_active_generation,
                                &state.ptt_generation,
                            ) {
                                HOOK_ACTION_TX.with(|tx| {
                                    if let Some(sender) = tx.borrow().as_ref() {
                                        if sender.send(HookAction::PttDown { vk, gen }).is_err() {
                                            let _ = release_ptt_member(
                                                &state.ptt_pressed_mask,
                                                member_index,
                                            );
                                            state
                                                .ptt_consumed_down_mask
                                                .fetch_and(!member_bit, Ordering::SeqCst);
                                            cancel_ptt_press_start(
                                                &state.ptt_active_generation,
                                                gen,
                                            );
                                            TRY_SEND_FAIL_COUNT.fetch_add(1, Ordering::Relaxed);
                                        }
                                    }
                                });
                            }
                        }
                    }

                    // 活動時任意成員鬆開都結束；未完成組合時的成員 up 只更新點陣圖。
                    if is_kup && release_ptt_member(&state.ptt_pressed_mask, member_index) {
                        HOOK_ACTION_TX.with(|tx| {
                            if let Some(sender) = tx.borrow().as_ref() {
                                let _ = queue_ptt_release(
                                    sender,
                                    &state.ptt_active_generation,
                                    None,
                                    vk,
                                    "member_keyup",
                                );
                            }
                        });
                    }
                }

                // 免持鍵：首次 keydown 立即觸發，後續 repeat down 忽略；keyup 只負責重新佈防。
                // 合成 keydown 仍在上方被過濾，所以遠端桌面的幻影 Alt 不會觸發錄音。
                if is_hf_key {
                    let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
                    let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
                    // 帶修飾鍵按下時整條放行。不 begin_hf_press ⇒ hf_key_down 仍是 false
                    // ⇒ 下面的 end_hf_press 也返回 false，配對的 keyup 同樣不吞。
                    if is_down && !modifiers_block_down {
                        consumed = true; // 吞掉 keydown 防止系統處理
                        if begin_hf_press(&state.hf_key_down) {
                            HOOK_ACTION_TX.with(|tx| {
                                if let Some(sender) = tx.borrow().as_ref() {
                                    if sender.send(HookAction::HfToggle { vk }).is_err() {
                                        TRY_SEND_FAIL_COUNT.fetch_add(1, Ordering::Relaxed);
                                    }
                                }
                            });
                        }
                    }
                    if is_up {
                        // 吞掉與首個 down 配對的 up，避免向目標程式留下孤立的按鍵事件。
                        if end_hf_press(&state.hf_key_down) || is_alt_key {
                            consumed = true;
                        }
                    }
                }

                if is_ai_toggle_key {
                    let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
                    let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
                    // 同免持：帶修飾鍵就整條放行（見上面 modifiers_block_down 的註釋）
                    if is_down && !modifiers_block_down {
                        consumed = true;
                        if begin_hf_press(&state.ai_toggle_key_down) {
                            HOOK_ACTION_TX.with(|tx| {
                                if let Some(sender) = tx.borrow().as_ref() {
                                    if sender.send(HookAction::AiToggle { vk }).is_err() {
                                        TRY_SEND_FAIL_COUNT.fetch_add(1, Ordering::Relaxed);
                                    }
                                }
                            });
                        }
                    }
                    if is_up && (end_hf_press(&state.ai_toggle_key_down) || is_alt_key) {
                        consumed = true;
                    }
                }
            }
        });

        if consumed {
            return LRESULT(1);
        }
    }

    CallNextHookEx(None, n_code, w_param, l_param)
}

#[cfg(test)]
mod tests {
    use super::{
        active_card_hotkeys, begin_hf_press, begin_ptt_press, card_hotkey_action_name,
        claim_ptt_release, complete_ptt_release, end_hf_press, escape_action_mode_name,
        is_injection_exempt_vk, is_mouse_button_setting, is_mouse_vk, modifier_kind,
        press_ptt_member, ptt_key_config, release_ptt_member, set_card_hotkeys,
        set_escape_action_mode, should_consume_combo_main_down, single_key_requires_bare_press,
        CARD_HOTKEY_COPY, CARD_HOTKEY_DEADLINE_MS, CARD_HOTKEY_FOREGROUND, DEFAULT_PTT_SETTING,
        DEFAULT_PTT_VK, SINGLE_KEY_TABLE, needs_mouse_hook, hook_looks_removed,
    };
    #[cfg(any(windows, target_os = "macos"))]
    use super::queue_ptt_release;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    /// 卡片快捷鍵的三道閘門與自清。
    ///
    /// 這些是程序級靜態量，所以整組斷言刻意放在**同一個** #[test] 裡 —— 拆成多個會被
    /// 並行排程互相踩掉，表現為隨機失敗。
    #[test]
    fn card_hotkeys_are_scoped_to_one_card_and_expire_on_their_own() {
        // 未知動作必須報錯，絕不能靜默開成"什麼都不接管"或"全都接管"。
        assert!(set_card_hotkeys(&["retry".to_string()], 1).is_err());

        set_card_hotkeys(&["copy".to_string()], 7).unwrap();
        assert_eq!(active_card_hotkeys(), (CARD_HOTKEY_COPY, 7));

        // 續期（同 token）絕不能重新取前台視窗：卡片可見期間使用者可能已經切走，
        // 那正是要解除接管的情形，重新取會把新前台當成"卡片的主人"又接管回來。
        CARD_HOTKEY_FOREGROUND.store(0x1234, Ordering::SeqCst);
        set_card_hotkeys(&["copy".to_string()], 7).unwrap();
        assert_eq!(
            CARD_HOTKEY_FOREGROUND.load(Ordering::SeqCst),
            0x1234,
            "renewal must keep the foreground window captured when the card appeared",
        );

        // 過了硬截止就自己關掉，不依賴前端來解除（渲染端卡死時不能永久吞 Ctrl+C）。
        CARD_HOTKEY_DEADLINE_MS.store(1, Ordering::SeqCst);
        assert_eq!(active_card_hotkeys(), (0, 0));
        assert_eq!(
            CARD_HOTKEY_FOREGROUND.load(Ordering::SeqCst),
            0,
            "expiry must also drop the captured foreground window",
        );

        // 空陣列 = 解除。
        set_card_hotkeys(&["copy".to_string()], 8).unwrap();
        set_card_hotkeys(&[], 8).unwrap();
        assert_eq!(active_card_hotkeys(), (0, 0));
    }

    /// 動作名與模式名是**跨程序契約**：前端按這兩個字串分派。
    /// 改名就會靜默失效（事件照發、前端認不出來），所以在這裡釘住。
    #[test]
    fn hotkey_and_escape_names_match_the_frontend_contract() {
        assert_eq!(card_hotkey_action_name(CARD_HOTKEY_COPY), "copy");

        for mode in [
            "off",
            "cancel_recording",
            "cancel_processing",
            "dismiss_fallback",
            "abandon_late_result",
        ] {
            set_escape_action_mode(mode, 1).unwrap();
            // off 的 token 會被清零，其餘模式都該原樣讀回自己的名字。
            let value = super::ESCAPE_ACTION_MODE.load(Ordering::SeqCst);
            assert_eq!(escape_action_mode_name(value), mode, "mode name drifted: {mode}");
        }
        set_escape_action_mode("off", 0).unwrap();
    }

    #[test]
    fn hands_free_triggers_on_first_down_and_rearms_on_up() {
        let key_down = AtomicBool::new(false);

        assert!(begin_hf_press(&key_down));
        assert!(!begin_hf_press(&key_down), "repeat down must not toggle again");
        assert!(end_hf_press(&key_down));
        assert!(!end_hf_press(&key_down), "orphan up is ignored");
        assert!(begin_hf_press(&key_down), "next physical press is re-armed");
    }

    #[test]
    fn ptt_release_is_idempotent_for_normal_or_synthetic_keyup() {
        let active_generation = AtomicU64::new(0);
        let generation = AtomicU64::new(0);

        let gen = begin_ptt_press(&active_generation, &generation).expect("first down starts a run");
        assert_eq!(gen, 1);
        assert!(begin_ptt_press(&active_generation, &generation).is_none());
        assert_eq!(claim_ptt_release(&active_generation, None), Some(gen));
        assert!(claim_ptt_release(&active_generation, None).is_none());
        assert!(complete_ptt_release(&active_generation, gen));
        assert_eq!(active_generation.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn combo_starts_only_when_all_members_are_down_and_repeat_does_not_restart() {
        let pressed_mask = AtomicU64::new(0);
        let full_mask = 0b11;

        assert!(!press_ptt_member(&pressed_mask, 0, full_mask));
        assert_eq!(pressed_mask.load(Ordering::SeqCst), 0b01);
        assert!(press_ptt_member(&pressed_mask, 1, full_mask));
        assert_eq!(pressed_mask.load(Ordering::SeqCst), full_mask);
        assert!(!press_ptt_member(&pressed_mask, 1, full_mask));
    }

    #[test]
    fn combo_main_key_is_consumed_only_when_modifiers_precede_it() {
        let modifier_mask = 0b01;
        let main_bit = 0b10;

        assert!(!should_consume_combo_main_down(
            0,
            0,
            main_bit,
            modifier_mask,
        ));
        assert!(should_consume_combo_main_down(
            modifier_mask,
            0,
            main_bit,
            modifier_mask,
        ));
        // K 先按下且當時已放行；之後再按 Ctrl，K 的 repeat 也必須繼續放行。
        assert!(!should_consume_combo_main_down(
            main_bit | modifier_mask,
            0,
            main_bit,
            modifier_mask,
        ));
        // 若最初的 K down 已吞，repeat 繼續吞，直到配對 up 清掉 consumed bit。
        assert!(should_consume_combo_main_down(
            main_bit | modifier_mask,
            main_bit,
            main_bit,
            modifier_mask,
        ));
    }

    #[test]
    fn any_combo_member_up_releases_and_can_complete_again() {
        let pressed_mask = AtomicU64::new(0);
        let full_mask = 0b11;
        assert!(!press_ptt_member(&pressed_mask, 0, full_mask));
        assert!(press_ptt_member(&pressed_mask, 1, full_mask));

        assert!(release_ptt_member(&pressed_mask, 0));
        assert_eq!(pressed_mask.load(Ordering::SeqCst), 0b10);
        assert!(!release_ptt_member(&pressed_mask, 0));
        assert!(press_ptt_member(&pressed_mask, 0, full_mask));
    }

    #[test]
    fn parses_legacy_normal_and_modifier_only_ptt_settings() {
        let legacy = ptt_key_config("ShiftRight");
        assert_eq!(legacy.setting, "ShiftRight");
        assert_eq!(legacy.vk_codes, vec![0xA1]);
        assert_eq!(legacy.modifier_mask, 0b1);

        let normal_combo = ptt_key_config("ControlLeft+KeyK");
        assert_eq!(normal_combo.vk_codes, vec![0xA2, 0x4B]);
        assert_eq!(normal_combo.modifier_mask, 0b01);

        let modifier_combo = ptt_key_config("ControlLeft+MetaLeft");
        assert_eq!(modifier_combo.vk_codes, vec![0xA2, 0x5B]);
        assert_eq!(modifier_combo.modifier_mask, 0b11);
    }

    #[test]
    fn invalid_ptt_setting_safely_falls_back_to_the_default_key() {
        let config = ptt_key_config("ControlLeft+UnknownKey");
        assert_eq!(config.setting, DEFAULT_PTT_SETTING);
        assert_eq!(config.vk_codes, vec![DEFAULT_PTT_VK]);
        assert_eq!(config.modifier_mask, 0b1);

        let duplicate_family = ptt_key_config("ControlLeft+ControlRight+KeyK");
        assert_eq!(duplicate_family.setting, DEFAULT_PTT_SETTING);
        assert_eq!(duplicate_family.vk_codes, vec![DEFAULT_PTT_VK]);
    }

    /// 預設鍵絕不能是 Shift：那正是會觸發 Windows 篩選鍵、讓錄音停不下來的鍵。
    /// 有人把預設值改回去時，這條會攔住。
    #[test]
    fn default_ptt_key_is_never_shift() {
        assert!(!DEFAULT_PTT_SETTING.contains("Shift"));
        assert_ne!(DEFAULT_PTT_VK, 0xA0);
        assert_ne!(DEFAULT_PTT_VK, 0xA1);
        // 預設鍵必須自身可解析，否則 fallback 會陷入"回退到一個非法值"
        assert_eq!(ptt_key_config(DEFAULT_PTT_SETTING).setting, DEFAULT_PTT_SETTING);
    }

    /// 老使用者存的 ShiftRight 必須繼續被解析、繼續生效。
    /// 前端不再讓人新設 Shift，但已經存在的繫結不能因為升級就失靈。
    #[test]
    fn legacy_shift_binding_still_parses() {
        let legacy = ptt_key_config("ShiftRight");
        assert_eq!(legacy.setting, "ShiftRight");
        assert_eq!(legacy.vk_codes, vec![0xA1]);
    }

    #[test]
    fn release_must_be_queued_before_the_next_ptt_run_can_start() {
        let active_generation = AtomicU64::new(0);
        let generation = AtomicU64::new(0);

        let old_gen = begin_ptt_press(&active_generation, &generation).expect("old run starts");
        assert_eq!(claim_ptt_release(&active_generation, Some(old_gen)), Some(old_gen));
        assert!(begin_ptt_press(&active_generation, &generation).is_none());
        assert!(complete_ptt_release(&active_generation, old_gen));

        let new_gen = begin_ptt_press(&active_generation, &generation).expect("new run starts");
        assert_ne!(new_gen, old_gen);
        assert!(claim_ptt_release(&active_generation, Some(old_gen)).is_none());
        assert_eq!(active_generation.load(Ordering::SeqCst), new_gen);
        assert_eq!(claim_ptt_release(&active_generation, Some(new_gen)), Some(new_gen));
        assert!(complete_ptt_release(&active_generation, new_gen));
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn disconnected_dispatcher_releases_native_ptt_state() {
        let active_generation = AtomicU64::new(0);
        let generation = AtomicU64::new(20);
        let active_gen =
            begin_ptt_press(&active_generation, &generation).expect("run starts");
        let (sender, receiver) = std::sync::mpsc::channel();
        drop(receiver);

        assert!(!queue_ptt_release(
            &sender,
            &active_generation,
            Some(active_gen),
            0xA1,
            "keyup",
        ));
        assert_eq!(active_generation.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn watchdog_release_requires_the_active_generation() {
        let active_generation = AtomicU64::new(0);
        let generation = AtomicU64::new(40);

        let active_gen =
            begin_ptt_press(&active_generation, &generation).expect("run starts");
        assert!(claim_ptt_release(&active_generation, Some(active_gen + 2)).is_none());
        assert_eq!(active_generation.load(Ordering::SeqCst), active_gen);
        assert_eq!(claim_ptt_release(&active_generation, Some(active_gen)), Some(active_gen));
        assert!(complete_ptt_release(&active_generation, active_gen));
    }

    #[test]
    fn hook_removal_is_suspected_only_when_input_outran_the_callback() {
        // 按鍵剛經過鉤子：回撥和輸入一樣新
        assert!(!hook_looks_removed(1_000, 1_000));
        // 使用者已經很久沒動：兩邊差不多一樣舊
        assert!(!hook_looks_removed(600_000, 598_000));
        // 很久沒按鍵、10 秒前動了下滑鼠：也會判成可疑（沒裝滑鼠鉤子時的已知誤報，代價只是重灌一次）
        assert!(hook_looks_removed(600_000, 590_000));
        // 2 分鐘沒回撥，但 3 秒前有輸入：可能被摘了
        assert!(hook_looks_removed(120_000, 3_000));
        // 差距在 5 秒以內不算
        assert!(!hook_looks_removed(8_000, 4_000));
        // 還沒收到過任何回撥（-1）時不判
        assert!(!hook_looks_removed(-1, 0));
    }

    #[test]
    fn mouse_hook_is_only_needed_for_mouse_bindings_or_capture() {
        let keyboard = [0xA3u32];
        assert!(!needs_mouse_hook(&keyboard, &[0xA5], &[], false));
        assert!(needs_mouse_hook(&keyboard, &[0xA5], &[], true));
        assert!(needs_mouse_hook(&[0x05], &[0xA5], &[], false));
        assert!(needs_mouse_hook(&keyboard, &[0x06], &[], false));
        assert!(needs_mouse_hook(&keyboard, &[], &[0x04], false));
    }

    /// 滑鼠按鍵清單必須與 `is_mouse_vk` 保持一致。
    ///
    /// 這兩處一旦不同步就是靜默失敗：兩個 PTT 看門狗靠 `is_mouse_vk` 繞開
    /// 「repeat 靜默 + 非同步鍵狀態」判據，漏掉一個滑鼠鍵的後果是按住它錄音會在
    /// 1.7 秒後被強制結束（0.1.5 的實測症狀），而編譯和其它測試都不會報錯。
    #[test]
    fn is_mouse_vk_covers_every_mouse_button_in_the_key_table() {
        for (code, vk) in SINGLE_KEY_TABLE {
            let listed_as_mouse = is_mouse_button_setting(code);
            assert_eq!(
                listed_as_mouse,
                is_mouse_vk(*vk),
                "{code} (vk={vk:#04x}): is_mouse_button_setting={listed_as_mouse} \
                 但 is_mouse_vk={}，兩處判定必須一致",
                is_mouse_vk(*vk),
            );
        }
    }

    /// 「裸按」規則必須覆蓋每一個主鍵，且必須放過每一個修飾鍵。
    ///
    /// 這兩條各自對應一個真實故障：漏掉主鍵 → Alt+F4 會喚起綁在 F4 上的免持，
    /// 而且 F4 的 down 被吞掉、Alt+F4 本身也失效（使用者報的就是這個）；
    /// 誤傷修飾鍵 → 「按住說話」預設的右 Ctrl、免持預設的右 Alt 會永久失效
    /// （自己就是 Ctrl，再問「Ctrl 有沒有按下」永遠為真），而且是靜默的：
    /// 編譯過、其它測試全綠，只有真機上按下去沒反應。
    #[test]
    fn bare_press_rule_covers_main_keys_and_spares_modifiers() {
        for (code, vk) in SINGLE_KEY_TABLE {
            let is_modifier = modifier_kind(code).is_some();
            let is_mouse = is_mouse_button_setting(code);
            let expected = !is_modifier && !is_mouse;
            assert_eq!(
                single_key_requires_bare_press(*vk),
                expected,
                "{code} (vk={vk:#04x})：modifier={is_modifier} mouse={is_mouse}，\
                 應當 requires_bare_press={expected}",
            );
        }
    }

    /// 幾個點名的鍵，防止上面那條迴圈因為清單本身寫錯而一起錯。
    #[test]
    fn bare_press_rule_named_cases() {
        assert!(single_key_requires_bare_press(0x73), "F4 是主鍵，必須要求裸按");
        assert!(single_key_requires_bare_press(0x20), "Space 是主鍵");
        assert!(single_key_requires_bare_press(0xA6), "BrowserBack 走鍵盤鉤子，算主鍵");
        assert!(!single_key_requires_bare_press(0xA3), "右 Ctrl 是「按住說話」預設鍵，不能套");
        assert!(!single_key_requires_bare_press(0xA5), "右 Alt 是免持預設鍵，也是 AltGr 的一半");
        assert!(!single_key_requires_bare_press(0x05), "滑鼠側鍵走滑鼠鉤子，不套這條");
    }

    /// 滑鼠按鍵只允許單鍵格式。兩個看門狗跳過滑鼠成員時依賴這一點：
    /// 單鍵設定只有一個成員，不存在「組合裡半個鍵被清掉」的情況。
    #[test]
    fn mouse_buttons_are_single_key_only() {
        for combo in ["XButton1+KeyA", "ControlLeft+XButton1", "MButton+ShiftLeft"] {
            let config = ptt_key_config(combo);
            assert_ne!(
                config.setting, combo,
                "{combo} 不該被接受為有效組合（滑鼠鍵只允許單鍵）",
            );
        }
        for single in ["XButton1", "XButton2", "MButton"] {
            let config = ptt_key_config(single);
            assert_eq!(config.setting, single, "{single} 應當是有效的單鍵設定");
            assert_eq!(config.vk_codes.len(), 1, "{single} 應當只有一個成員");
        }
    }

    /// F1–F24 必須全部在表裡、且 vk 連續等於 0x70 + (n-1)。
    ///
    /// 這張表與前端 `lib/shortcutKeys.ts` 的 `SINGLE_KEYS` 是兩份手寫副本，只改一邊
    /// 是**靜默失敗**：前端放行 F13、Rust 查不到它，`ptt_key_config` 的 valid_shape
    /// 落空後走 `fallback()` —— 使用者介面上顯示 F13，實際說話鍵悄悄變成右 Ctrl，
    /// 編譯和其它測試都不報錯。TS 側有一條對稱的斷言。
    #[test]
    fn function_keys_f1_through_f24_are_all_present_with_contiguous_vks() {
        for n in 1..=24_u32 {
            let code = format!("F{n}");
            let expected_vk = 0x70 + n - 1;
            let found = SINGLE_KEY_TABLE
                .iter()
                .find(|(setting, _)| *setting == code.as_str());
            let Some((_, vk)) = found else {
                panic!("{code} 不在 SINGLE_KEY_TABLE 裡（前端 SINGLE_KEYS 有 F1–F24，兩邊必須一致）");
            };
            assert_eq!(
                *vk, expected_vk,
                "{code} 的 vk 應當是 {expected_vk:#04x}（VK_F1..VK_F24 是連續的）",
            );

            // 解析必須真的接受它，而不是回落到預設鍵。
            let config = ptt_key_config(&code);
            assert_eq!(config.setting, code, "{code} 應當被接受為有效單鍵，而不是回落");
            assert_eq!(config.vk_codes, vec![expected_vk], "{code} 應當解析出對應 vk");
        }
    }

    /// 幻影過濾的例外範圍：F13–F24 與兩個側鍵改寫鍵在內，邊界之外不在。
    ///
    /// 這些鍵帶 injected 標誌是正常來源（改鍵軟體走 SendInput），漏掉任何一個的症狀
    /// 都是「綁得上、按下去沒反應、日誌裡也沒有記錄」。
    #[test]
    fn injection_exempt_vks_cover_f13_to_f24_and_side_button_remaps() {
        for vk in 0x7C..=0x87_u32 {
            assert!(is_injection_exempt_vk(vk), "{vk:#04x}（F13–F24 段）必須開例外");
        }
        assert!(is_injection_exempt_vk(0xA6), "BrowserBack 必須開例外");
        assert!(is_injection_exempt_vk(0xA7), "BrowserForward 必須開例外");

        // 邊界：F12 及以下是物理真鍵，注入的 F12 沒有正常來源，維持幻影過濾。
        assert!(!is_injection_exempt_vk(0x7B), "F12 不該開例外");
        assert!(!is_injection_exempt_vk(0x88), "0x88 已越過 F24，不該開例外");
        // 幻影過濾本來要防的就是合成 Alt，絕不能被例外放進來。
        assert!(!is_injection_exempt_vk(0xA4), "左 Alt 絕不能開例外");
        assert!(!is_injection_exempt_vk(0xA5), "右 Alt 絕不能開例外");
    }
}
