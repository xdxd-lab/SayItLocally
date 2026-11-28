use serde_json::Value;
use tauri::{AppHandle, Emitter, State};
use crate::keyboard::KeyboardHookManager;
use crate::storage::Storage;

#[tauri::command]
pub fn set_escape_action_mode(mode: String, token: u64) -> Result<(), String> {
    crate::keyboard::set_escape_action_mode(&mode, token)
}

/// 開啟/續期/解除懸浮窗卡片上的臨時組合鍵（目前只有 `copy` = Ctrl+C）。
///
/// 傳空陣列即解除。**必須與卡片生命週期繫結**：這些鍵在別的程式裡本來就有用途，
/// 絕不能永久註冊（詳見 keyboard::set_card_hotkeys 的註釋）。
#[tauri::command]
pub fn set_card_hotkeys(actions: Vec<String>, token: u64) -> Result<(), String> {
    crate::keyboard::set_card_hotkeys(&actions, token)
}

#[tauri::command]
pub fn shortcuts_changed(
    app: AppHandle,
    storage: State<Storage>,
    hook: State<KeyboardHookManager>,
) {
    // Read settings
    let ptt_setting = storage.get("shortcutPTT", None);
    let ptt_str = ptt_setting.as_str().unwrap_or(crate::defaults::SHORTCUT_PTT);
    let hf_val = storage.get("shortcutHandsFree", None);
    let hf_key = hf_val.as_str().unwrap_or(crate::defaults::SHORTCUT_HANDS_FREE);
    let ai_toggle_val = storage.get("shortcutToggleAi", None);
    let ai_toggle_key = ai_toggle_val.as_str().unwrap_or(crate::defaults::SHORTCUT_TOGGLE_AI);

    // Reconfigure PTT + hands-free + AI cleanup single-key/mouse hook
    hook.reconfigure(&app, ptt_str, hf_key, ai_toggle_key);

    // 重新註冊所有 global_shortcut（擴音組合鍵 + 各潤色模式切換快捷鍵）
    register_all_global_shortcuts(&app, storage.inner());
}

/// 集中註冊所有 global_shortcut：擴音組合鍵、AI 整理開關和各潤色模式切換快捷鍵。
///
/// `unregister_all` 會清空全部已註冊的 global_shortcut，因此這兩類必須在同一處
/// 一次性重註冊，否則會互相覆蓋。單鍵（PTT/擴音單鍵）不走這裡，由底層鍵盤鉤子處理。
pub fn register_all_global_shortcuts(app: &AppHandle, storage: &Storage) {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

    let gs = app.global_shortcut();
    let _ = gs.unregister_all();

    // macOS 15 起，系統快捷鍵不觸發修飾鍵只有 ⌥ / ⇧ 的組合，這一類交給 event tap
    // （keyboard/macos.rs 的 TAP_COMBOS），其餘照舊註冊成系統快捷鍵。Windows 全部走系統快捷鍵，
    // 下面每處的 `via_tap` 在 Windows 上恆為 false。
    #[cfg(target_os = "macos")]
    let mut tap_combos: Vec<crate::keyboard::TapCombo> = Vec::new();

    // 擴音組合鍵
    let hf_val = storage.get("shortcutHandsFree", None);
    let hf_key = hf_val.as_str().unwrap_or(crate::defaults::SHORTCUT_HANDS_FREE).to_string();
    let hf_is_combo = !hf_key.is_empty() && hf_key.contains('+');
    #[cfg(target_os = "macos")]
    let via_tap = hf_is_combo
        && route_to_event_tap(&mut tap_combos, &hf_key, crate::keyboard::TapComboAction::HandsFree);
    #[cfg(not(target_os = "macos"))]
    let via_tap = false;
    if via_tap {
        log::info!("Registered hands-free shortcut in the event tap: {}", hf_key);
    } else if hf_is_combo {
        if let Err(e) = gs.on_shortcut(hf_key.as_str(), move |app, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                let _ = app.emit(
                    "toggle-hands-free",
                    serde_json::json!({ "source": "globalShortcut" }),
                );
            }
        }) {
            log::warn!("Failed to register hands-free shortcut '{}': {}", hf_key, e);
        } else {
            log::info!("Registered hands-free shortcut: {}", hf_key);
        }
    }

    // AI 整理開關快捷鍵。留空即不註冊，避免預設佔用使用者已有組合鍵。
    let ai_toggle = storage.get("shortcutToggleAi", None);
    let ai_toggle_key = ai_toggle.as_str().unwrap_or(crate::defaults::SHORTCUT_TOGGLE_AI).to_string();
    let ai_toggle_is_combo = !ai_toggle_key.is_empty() && ai_toggle_key.contains('+');
    #[cfg(target_os = "macos")]
    let via_tap = ai_toggle_is_combo
        && route_to_event_tap(&mut tap_combos, &ai_toggle_key, crate::keyboard::TapComboAction::AiToggle);
    #[cfg(not(target_os = "macos"))]
    let via_tap = false;
    if via_tap {
        log::info!("Registered AI cleanup shortcut in the event tap: {}", ai_toggle_key);
    } else if ai_toggle_is_combo {
        let ai_toggle_for_log = ai_toggle_key.clone();
        if let Err(e) = gs.on_shortcut(ai_toggle_key.as_str(), move |app, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                let _ = app.emit("toggle-ai-cleanup", serde_json::json!({ "source": "globalShortcut" }));
            }
        }) {
            log::warn!("Failed to register AI cleanup shortcut '{}': {}", ai_toggle_for_log, e);
        } else {
            log::info!("Registered AI cleanup shortcut: {}", ai_toggle_for_log);
        }
    }

    // 潤色模式切換快捷鍵：presetShortcuts = { presetId: 組合鍵 }
    let preset_map = storage.get("presetShortcuts", None);
    if let Some(obj) = preset_map.as_object() {
        for (preset_id, val) in obj {
            let accel = match val.as_str() {
                Some(s) if !s.is_empty() && s.contains('+') => s.to_string(),
                _ => continue,
            };
            #[cfg(target_os = "macos")]
            let via_tap = route_to_event_tap(
                &mut tap_combos,
                &accel,
                crate::keyboard::TapComboAction::Preset(preset_id.clone()),
            );
            #[cfg(not(target_os = "macos"))]
            let via_tap = false;
            if via_tap {
                log::info!("Registered preset shortcut in the event tap: {} for '{}'", accel, preset_id);
                continue;
            }
            let pid = preset_id.clone();
            let accel_for_log = accel.clone();
            if let Err(e) = gs.on_shortcut(accel.as_str(), move |app, _shortcut, event| {
                // 只在按下時觸發一次，避免鬆開時重複觸發
                if event.state == ShortcutState::Pressed {
                    let _ = app.emit(
                        "switch-preset",
                        serde_json::json!({ "presetId": pid.clone() }),
                    );
                }
            }) {
                log::warn!(
                    "Failed to register preset shortcut '{}' for '{}': {}",
                    accel_for_log, preset_id, e
                );
            }
        }
    }

    #[cfg(target_os = "macos")]
    crate::keyboard::set_tap_combos(tap_combos);
}

/// macOS：系統快捷鍵接不住的組合鍵（只帶 ⌥ / ⇧）放進 `list`，返回 true；其餘返回 false，照舊註冊成系統快捷鍵。
#[cfg(target_os = "macos")]
fn route_to_event_tap(
    list: &mut Vec<crate::keyboard::TapCombo>,
    accelerator: &str,
    action: crate::keyboard::TapComboAction,
) -> bool {
    match crate::keyboard::tap_combo_for_accelerator(accelerator, action) {
        Some(combo) => {
            list.push(combo);
            true
        }
        None => false,
    }
}

/// 探測快捷鍵是否可用。
/// - 單鍵（不含 '+'）由底層鍵盤鉤子處理，任意單鍵都可用 → 返回 true。
/// - 組合鍵透過 global_shortcut 試註冊探測：註冊成功說明系統未被其他
///   （使用 RegisterHotKey 的）程式佔用，隨即登出並返回 true；失敗返回 false。
///
/// 注意：此探測只能發現"用 RegisterHotKey 註冊的程式"造成的衝突，
/// 無法發現用底層鍵盤鉤子（如本應用自身、部分輸入法/工具）佔用的鍵——
/// 這是 Windows API 的固有限制。
#[tauri::command]
pub fn get_ptt_physical_key_states(codes: Vec<String>) -> Vec<bool> {
    crate::keyboard::ptt_physical_key_states(&codes)
}

#[tauri::command]
pub fn test_shortcut(
    app: AppHandle,
    storage: State<Storage>,
    accelerator: String,
) -> Result<bool, String> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;

    if !accelerator.contains('+') {
        return Ok(true);
    }

    let gs = app.global_shortcut();

    // 先清空本應用自己的全部註冊，避免把“自己已佔用同一組合鍵”誤判成衝突。
    // （直接用 is_registered 判斷不可靠：字串與內部 Shortcut 可能不一致。）
    let _ = gs.unregister_all();

    let available = match gs.register(accelerator.as_str()) {
        Ok(_) => {
            let _ = gs.unregister(accelerator.as_str());
            true
        }
        Err(e) => {
            log::info!("[shortcut] Failed to register '{}'; another app may already use it: {}", accelerator, e);
            false
        }
    };

    // 恢復本應用自己的全部註冊（擴音 + 各預設快捷鍵）
    register_all_global_shortcuts(&app, storage.inner());

    Ok(available)
}

/// 開啟"錄製捕獲"模式：設定頁開始錄製快捷鍵時呼叫。開啟後：
/// - 底層滑鼠鉤子把下一個側鍵按下吞掉（避免 webview 把它當"後退"導航）並透過
///   `mouse-shortcut-captured` 事件回報給前端用於繫結；
/// - 鍵盤鉤子放行 PTT/擴音單鍵，不再觸發錄音（否則按到已繫結的鍵會直接開始口述）；
/// - 登出全部 global_shortcut。RegisterHotKey 註冊的組合鍵會被系統直接吞掉、送不到
///   webview，不登出的話按已繫結的組合鍵只會觸發它自己，永遠錄不進新設定。
#[tauri::command]
pub fn begin_shortcut_capture(app: AppHandle) {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    crate::keyboard::set_shortcut_capture(true);
    let _ = app.global_shortcut().unregister_all();
}

/// 關閉錄製捕獲模式（錄製結束/取消時呼叫），並恢復全部 global_shortcut 註冊。
#[tauri::command]
pub fn end_shortcut_capture(app: AppHandle, storage: State<Storage>) {
    crate::keyboard::set_shortcut_capture(false);
    register_all_global_shortcuts(&app, storage.inner());
}

/// PTT Lab: start/stop a dedicated keyboard hook for the lab test key (right Ctrl).
/// Completely independent from the main PTT hook — does not interfere with recording.
#[tauri::command]
pub fn set_ptt_lab_config(data: Value, app: AppHandle) -> Result<(), String> {
    let enabled = data.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
    log::info!("[ptt-lab] set_ptt_lab_config called, enabled={}, data={}", enabled, data);

    // PTT Lab 是 Windows 上排查低階鉤子用的除錯頁，mac 上不提供（前端也隱藏了入口）。
    #[cfg(not(windows))]
    let _ = &app;

    #[cfg(windows)]
    {
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        use std::sync::Mutex;
        use std::thread;

        // Static state for the lab hook thread
        static LAB_RUNNING: AtomicBool = AtomicBool::new(false);
        static LAB_THREAD_ID: AtomicU32 = AtomicU32::new(0);
        static LAB_APP: Mutex<Option<AppHandle>> = Mutex::new(None);

        // Stop existing lab hook if running
        if LAB_RUNNING.load(Ordering::SeqCst) {
            let tid = LAB_THREAD_ID.load(Ordering::SeqCst);
            if tid != 0 {
                unsafe {
                    use windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW;
                    use windows::Win32::Foundation::{WPARAM, LPARAM};
                    const WM_QUIT: u32 = 0x0012;
                    let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
                }
            }
            LAB_RUNNING.store(false, Ordering::SeqCst);
            LAB_THREAD_ID.store(0, Ordering::SeqCst);
            *LAB_APP.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
            // Small delay to let old thread exit
            thread::sleep(std::time::Duration::from_millis(100));
            log::info!("[ptt-lab] stopped lab hook");
        }

        if !enabled {
            return Ok(());
        }

        // Parse the VK code — default to right Ctrl (0xA3)
        let vk_code: u32 = data.get("vkCode")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(0xA3); // VK_RCONTROL

        *LAB_APP.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(app.clone());
        LAB_RUNNING.store(true, Ordering::SeqCst);

        thread::spawn(move || {
            use windows::Win32::UI::WindowsAndMessaging::{
                SetWindowsHookExW, UnhookWindowsHookEx, GetMessageW,
                TranslateMessage, DispatchMessageW, CallNextHookEx,
                KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL,
                WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
            };
            use windows::Win32::Foundation::{WPARAM, LPARAM, LRESULT};
            use windows::Win32::System::Threading::GetCurrentThreadId;

            // Thread-local state
            thread_local! {
                static LAB_VK: std::cell::Cell<u32> = std::cell::Cell::new(0xA3);
                static LAB_KEY_DOWN: std::cell::Cell<bool> = std::cell::Cell::new(false);
            }

            LAB_VK.with(|v| v.set(vk_code));

            unsafe extern "system" fn lab_keyboard_proc(
                n_code: i32,
                w_param: WPARAM,
                l_param: LPARAM,
            ) -> LRESULT {
                if n_code >= 0 {
                    let kb = &*(l_param.0 as *const KBDLLHOOKSTRUCT);
                    let vk = kb.vkCode;
                    let msg = w_param.0 as u32;

                    let is_lab_key = LAB_VK.with(|v| vk == v.get());
                    if is_lab_key {
                        let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
                        let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;

                        if is_down && !LAB_KEY_DOWN.with(|v| v.get()) {
                            LAB_KEY_DOWN.with(|v| v.set(true));
                            if let Some(app) = LAB_APP.lock().ok().and_then(|g| g.clone()) {
                                let _ = app.emit("ptt-lab-event", serde_json::json!({
                                    "phase": "down",
                                    "vk": vk,
                                    "timestamp": chrono::Utc::now().timestamp_millis(),
                                }));
                            }
                            return LRESULT(1); // consume
                        }

                        if is_up && LAB_KEY_DOWN.with(|v| v.get()) {
                            LAB_KEY_DOWN.with(|v| v.set(false));
                            if let Some(app) = LAB_APP.lock().ok().and_then(|g| g.clone()) {
                                let _ = app.emit("ptt-lab-event", serde_json::json!({
                                    "phase": "up",
                                    "vk": vk,
                                    "timestamp": chrono::Utc::now().timestamp_millis(),
                                }));
                            }
                            return LRESULT(1); // consume
                        }

                        // Consume repeat downs too
                        if is_down || is_up {
                            return LRESULT(1);
                        }
                    }
                }
                CallNextHookEx(None, n_code, w_param, l_param)
            }

            unsafe {
                let tid = GetCurrentThreadId();
                LAB_THREAD_ID.store(tid, Ordering::SeqCst);
                log::info!("[ptt-lab] hook thread started, tid={}, vk=0x{:X}", tid, vk_code);

                let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(lab_keyboard_proc), None, 0);
                let hook = match hook {
                    Ok(h) => h,
                    Err(e) => {
                        log::error!("[ptt-lab] SetWindowsHookExW failed: {}", e);
                        LAB_RUNNING.store(false, Ordering::SeqCst);
                        return;
                    }
                };

                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }

                let _ = UnhookWindowsHookEx(hook);
                LAB_RUNNING.store(false, Ordering::SeqCst);
                LAB_THREAD_ID.store(0, Ordering::SeqCst);
                log::info!("[ptt-lab] hook thread exited");
            }
        });
    }

    Ok(())
}
