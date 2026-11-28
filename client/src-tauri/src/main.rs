// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod error_protocol;
mod locale;
mod storage;
mod window;
mod keyboard;
mod context;
mod inject;
mod providers;
mod models;
mod defaults;
mod net_policy;
mod path_guard;
mod ipc_body;
mod platform;
mod instance_lock;
mod zh_output;

use storage::Storage;
use window::WindowState;
use keyboard::KeyboardHookManager;
use context::ContextDetector;
use tauri::{Manager, Emitter};
use std::thread;

/// Environment-level WebView2 flags must be identical for every webview that shares
/// com.sayitlocally.desktop/EBWebView. Keep them global; never copy them into a single window's
/// `additionalBrowserArgs` in tauri.conf.json (WebView2 rejects the second environment
/// with ERROR_INVALID_STATE when the option sets differ).
// 注意：不要再加回 --auto-accept-camera-and-microphone-capture。
// 該引數會搶先自動處理許可權請求，使 PermissionRequested 事件不觸發，許可權停留在 "prompt"，
// 導致新版 WebView2 隱藏 enumerateDevices 的裝置名/deviceId。麥克風授權改由下面的
// PermissionRequested 處理器負責（顯式 Allow → 變為持久 granted → 恢復裝置列舉）。
// 也不要加回 --ignore-certificate-errors：它讓所有主機都不校驗證書，網路被截獲時
// 識別結果和更新清單都能被換掉。自建伺服器的自簽名證書改由主窗的
// ServerCertificateErrorDetected 處理器按地址放行（見 net_policy::cert_error_allowed）。
#[cfg_attr(not(windows), allow(dead_code))]
const WEBVIEW2_BROWSER_ARGS: &str =
    "--disable-backgrounding-occluded-windows --disable-renderer-backgrounding \
     --disable-background-timer-throttling";

#[cfg_attr(not(windows), allow(dead_code))]
fn browser_args_fingerprint(value: &str) -> String {
    // Stable FNV-1a fingerprint: enough to compare field reports without logging a
    // potentially sensitive inherited proxy argument verbatim.
    let mut hash = 0xcbf29ce484222325u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

// [自訂] S71：啟動與執行中清理都讀目前設定，永遠保留錄音仍以 -1 表示。
fn cleanup_retained_files(storage: &Storage) {
    commands::audio::cleanup_retained_recordings(recording_retention_days(storage, &commands::app_data_dir()));
    if let Ok(days) = commands::system::log_retention_days(storage) {
        commands::system::cleanup_retained_logs(days);
    }
}

// [自訂] L-72：記憶體庫及仍在保護期間的錄音不按保留天數刪除。
fn recording_retention_days(
    storage: &Storage,
    dir: &std::path::Path,
) -> Option<i64> {
    if storage.is_volatile() || storage::recovery::recording_cleanup_protected(dir) {
        return None;
    }
    match storage.retention_days("audioRetentionDays", 7) {
        Ok(days @ (7 | 30 | 90)) => Some(days),
        _ => None,
    }
}

/// 本地庫壞了、已換新庫啟動時告訴使用者：設定和歷史看起來全沒了，但舊資料在備份檔案裡。
/// 用獨立執行緒彈系統對話方塊，不擋啟動（開機自啟時也不會卡在這裡）。
fn notify_storage_recovery(recovery: storage::StorageRecovery) {
    let summary = match &recovery {
        storage::StorageRecovery::MovedAside { .. } => "started with a replacement database",
        storage::StorageRecovery::MovedAsideInMemory { .. } => "moved the database group; using in-memory storage",
        storage::StorageRecovery::IncompleteMove => "recovery move did not complete; using in-memory storage",
        storage::StorageRecovery::InMemory => "database unavailable; using in-memory storage",
    };
    commands::system::write_log_line(&format!("[storage] {summary}"));
    // [自訂] SCAN1-2（L-72）：提示改成「副本只在本機、可能含舊資料與密鑰、請自行刪除」，不再請使用者把檔案交給開發者。
    let zh = matches!(locale::system_ui_lang(), locale::Lang::ZhTw);
    let text = match &recovery {
        storage::StorageRecovery::MovedAside { backup, orphaned } => {
            let reason = match (zh, orphaned) {
                (true, true) => "SayItLocally 發現沒有配對的資料庫附屬檔案。",
                (true, false) => "SayItLocally 的本機資料庫檔案已損毀，無法讀取。",
                (false, true) => "SayItLocally found database companion files without a usable main file.",
                (false, false) => "SayItLocally could not read its local database because it is damaged.",
            };
            if zh {
                format!("{reason}\n\n已用全新的資料啟動，設定和歷史記錄暫時是空的。\n原檔案已保留，可在設定的資料庫副本清單中查看。副本路徑：\n{}\n\n這個副本只在本機，可能含有舊的設定、歷史記錄或金鑰。不需要時請自行刪除，請勿傳給他人。舊錄音已暫停自動清理，可在設定中恢復。", backup.display())
            } else {
                format!("{reason}\n\nIt has started with fresh data, so settings and history are empty for now.\nThe original files are listed under Database Copies in Settings. Copy path:\n{}\n\nThis copy stays on this computer and may contain old settings, history or keys. Delete it yourself when you no longer need it, and do not send it to anyone. Automatic recording cleanup is paused. You can resume it in Settings.", backup.display())
            }
        }
        storage::StorageRecovery::MovedAsideInMemory { backup } => if zh {
            format!("SayItLocally 已保留原資料庫檔案，但無法開啟新的資料檔案。副本路徑：\n{}\n\n這次使用記憶體，設定和歷史記錄在結束後不會保存。舊錄音已暫停自動清理。", backup.display())
        } else {
            format!("SayItLocally kept the original database files but could not open a replacement. Copy path:\n{}\n\nSettings and history will not be saved after you quit. Automatic recording cleanup is paused.", backup.display())
        },
        storage::StorageRecovery::IncompleteMove => {
            let dir = commands::app_data_dir();
            let current_files = storage::recovery::MANUAL_RECOVERY_FILES.join("\n");
            let hold = storage::recovery::RECORDING_HOLD_FILE;
            if zh {
                format!("SayItLocally 沒有完成資料庫復原。原資料庫檔案沒有被刪除，有些可能已經移入副本。這次使用記憶體，結束後不會保存這次的設定和歷史記錄。\n\nApp 資料夾：\n{}\n\n1. 結束所有正在執行的 SayItLocally。\n2. 將整個App 資料夾（包括錄音和資料庫副本）備份到另一個位置。\n3. 把下列仍在原 App 資料夾中的項目全部移到同一個新的本機資料夾，保留原檔名。已產生的資料庫副本請留在原處。\n{current_files}\n4. 保留原 App 資料夾裡的 {hold}。如果沒有這個檔案，請在原 App 資料夾中新增一個同名的空白檔案（不要加 .txt），以暫停清理舊錄音。\n5. 確認第 3 步列出的項目都已移走，再重新開啟 App。App 會建立新的資料庫，舊資料仍在備份中。如果無法備份、搬移檔案或寫入原資料夾，請先解決存取權限或磁碟空間問題。", dir.display())
            } else {
                format!("SayItLocally could not finish database recovery. No original database files were deleted. Some may already be in a recovery copy. This session uses memory, so its settings and history will be lost when you quit.\n\nData folder:\n{}\n\n1. Quit all running instances of SayItLocally.\n2. Back up the entire data folder, including recordings and database copies, to another location.\n3. Move every item listed below that is still in the original data folder into the same new local folder. Keep their names and leave any existing database copies in place.\n{current_files}\n4. Keep {hold} in the original data folder. If it is missing, create an empty file with that exact name (no .txt extension) there to pause cleanup of old recordings.\n5. Verify that none of the items in step 3 remain in the original data folder, then reopen the app. The app will create a fresh database. The backup still contains your old files. If you cannot back up, move, or write files, resolve folder access or disk space first.", dir.display())
            }
        },
        storage::StorageRecovery::InMemory => if zh {
            "SayItLocally 無法使用本機資料庫，可能是其他程式正在使用、沒有權限或磁碟已滿。App 沒有搬移原檔案。\n\n這次使用記憶體，設定和歷史記錄在結束後不會保存。".to_string()
        } else {
            "SayItLocally could not use its local database. Another program may be using it, permissions may be missing, or the disk may be full. The app left the original files in place.\n\nThis session uses memory. Settings and history will not be saved after you quit.".to_string()
        },
    };
    #[cfg(target_os = "windows")]
    thread::spawn(move || {
        use windows::core::HSTRING;
        use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND};
        unsafe {
            MessageBoxW(None, &HSTRING::from(text), &HSTRING::from("SayItLocally"), MB_OK | MB_ICONWARNING | MB_SETFOREGROUND);
        }
    });
    #[cfg(target_os = "macos")]
    crate::platform::macos::appkit::show_alert_async("SayItLocally", &text);
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let _ = text;
}

/// 主窗的圖示和 WebView2 事件處理器（麥克風許可權、證書、渲染程序崩潰）。
/// 啟動時和主窗被重建時都要走一遍。
fn prepare_main_window(app: &tauri::AppHandle, main_window: &tauri::WebviewWindow) {
    // macOS：沒有 WebView2 那幾個處理器；麥克風授權由 wry 的 WKUIDelegate 預設放行，
    // 後台節流在 tauri.macos.conf.json 裡用 backgroundThrottling 關掉。
    #[cfg(not(target_os = "windows"))]
    let _ = app;
    let ico_bytes = include_bytes!("../icons/icon.ico");
    if let Ok(icon) = tauri::image::Image::from_bytes(ico_bytes) {
        let _ = main_window.set_icon(icon);
    }

    // 麥克風/攝像頭許可權：--auto-accept-camera-and-microphone-capture 只放行採集，
    // 不會把許可權置為持久 granted，導致新版 WebView2 隱藏 enumerateDevices 的裝置名與
    // deviceId（設定裡麥克風列表只剩一個通用項）。掛 PermissionRequested 顯式 Allow，
    // 使許可權變為 granted，恢復完整裝置列舉與選擇。失敗僅記錄日誌、不影響其餘功能。
    #[cfg(target_os = "windows")]
    {
        let _ = main_window.with_webview(|webview| {
            use webview2_com::Microsoft::Web::WebView2::Win32::{
                COREWEBVIEW2_PERMISSION_KIND_CAMERA,
                COREWEBVIEW2_PERMISSION_KIND_MICROPHONE,
                COREWEBVIEW2_PERMISSION_STATE_ALLOW,
            };
            use webview2_com::PermissionRequestedEventHandler;
            unsafe {
                let controller = webview.controller();
                if let Ok(core) = controller.CoreWebView2() {
                    let handler = PermissionRequestedEventHandler::create(Box::new(
                        |_sender, args| {
                            if let Some(args) = args {
                                let mut kind = Default::default();
                                args.PermissionKind(&mut kind)?;
                                if kind == COREWEBVIEW2_PERMISSION_KIND_MICROPHONE
                                    || kind == COREWEBVIEW2_PERMISSION_KIND_CAMERA
                                {
                                    args.SetState(COREWEBVIEW2_PERMISSION_STATE_ALLOW)?;
                                    log::info!(
                                        "WebView2 permission allowed: kind={:?}",
                                        kind
                                    );
                                }
                            }
                            Ok(())
                        },
                    ));
                    let mut token = Default::default();
                    match core.add_PermissionRequested(&handler, &mut token) {
                        Ok(_) => log::info!(
                            "WebView2 PermissionRequested handler attached"
                        ),
                        Err(e) => log::error!(
                            "failed to attach PermissionRequested handler: {e:?}"
                        ),
                    }
                }
            }
        });
    }

    // 證書錯誤：只對使用者自己填的、非官方的 https 伺服器放行（自建 + 自簽名證書）。
    // 2026-10 實測這個事件對 wss:// 同樣觸發，所以 WebSocket 也走這裡。
    // 每次都從資料庫讀當前地址：使用者改地址後不用重啟就生效。
    // 只掛在主窗上 —— 聯網的只有主窗；掛不上（執行時太舊）就退回"一律嚴格校驗"。
    #[cfg(target_os = "windows")]
    {
        let cert_app = app.clone();
        let _ = main_window.with_webview(move |webview| {
            use webview2_com::Microsoft::Web::WebView2::Win32::{
                ICoreWebView2_14,
                COREWEBVIEW2_SERVER_CERTIFICATE_ERROR_ACTION_ALWAYS_ALLOW,
            };
            use webview2_com::ServerCertificateErrorDetectedEventHandler;
            use windows_core::{Interface, PWSTR};
            unsafe {
                let controller = webview.controller();
                let Ok(core) = controller.CoreWebView2() else { return };
                // 主窗網頁程序崩潰後自動過載。錄音、連線、插字排程都跑在這個程序裡，
                // 它一退出就是「托盤還在、熱鍵全沒反應、沒有任何提示」，以前只能重啟 SayItLocally。
                // 只處理「已退出」：「無響應」只記日誌，匯出大量歷史這類長任務也會短暫無響應，
                // 那時過載會把使用者正在做的事打斷。5 分鐘內超過 3 次就不再過載，免得反覆崩潰時死迴圈。
                // 和證書處理器掛在同一次 with_webview 裡：單獨再調一次 with_webview 時
                // 主窗 webview 穩定建不起來（0x80070057，2026-10-12 復現 5 次），原因未查明。
                {
                    use webview2_com::Microsoft::Web::WebView2::Win32::{
                        COREWEBVIEW2_PROCESS_FAILED_KIND, COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED,
                        COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE,
                    };
                    use webview2_com::ProcessFailedEventHandler;
                    let handler = ProcessFailedEventHandler::create(Box::new(move |sender, args| {
                        let Some(args) = args else { return Ok(()) };
                        let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
                        args.ProcessFailedKind(&mut kind)?;
                        if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED {
                            let now = chrono::Utc::now().timestamp_millis();
                            let allowed = window::main_renderer_reload_allowed(now);
                            commands::system::write_log_line(&format!(
                                "[main-webview] renderer process exited; reload={}",
                                allowed
                            ));
                            if allowed {
                                if let Some(sender) = sender {
                                    sender.Reload()?;
                                }
                            }
                        } else if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE {
                            commands::system::write_log_line("[main-webview] renderer process is unresponsive");
                        } else {
                            commands::system::write_log_line(&format!("[main-webview] process failed kind={:?}", kind));
                        }
                        Ok(())
                    }));
                    let mut token = Default::default();
                    match core.add_ProcessFailed(&handler, &mut token) {
                        Ok(_) => log::info!("[main-webview] ProcessFailed handler attached"),
                        Err(e) => log::error!("[main-webview] failed to attach ProcessFailed handler: {e:?}"),
                    }
                }
                let core14: ICoreWebView2_14 = match core.cast() {
                    Ok(value) => value,
                    Err(e) => {
                        log::warn!("[cert] ServerCertificateErrorDetected unavailable, self-signed servers will be rejected: {e:?}");
                        return;
                    }
                };
                let handler = ServerCertificateErrorDetectedEventHandler::create(Box::new(
                    move |_sender, args| {
                        let Some(args) = args else { return Ok(()) };
                        let mut uri = PWSTR::null();
                        args.RequestUri(&mut uri)?;
                        let request_uri = webview2_com::take_pwstr(uri);
                        let storage: tauri::State<Storage> = cert_app.state();
                        let configured = storage
                            .get(net_policy::BACKEND_BASE_URL_KEY, None)
                            .as_str()
                            .unwrap_or("")
                            .to_string();
                        let host = reqwest::Url::parse(&request_uri)
                            .ok()
                            .and_then(|u| u.host_str().map(|h| format!("{}:{}", h, u.port_or_known_default().unwrap_or(0))))
                            .unwrap_or_default();
                        if net_policy::cert_error_allowed(&request_uri, &configured) {
                            args.SetAction(COREWEBVIEW2_SERVER_CERTIFICATE_ERROR_ACTION_ALWAYS_ALLOW)?;
                            log::info!("[cert] certificate error allowed for the configured server host={host}");
                        } else {
                            log::warn!("[cert] certificate error rejected host={host}");
                        }
                        Ok(())
                    },
                ));
                let mut token = Default::default();
                match core14.add_ServerCertificateErrorDetected(&handler, &mut token) {
                    Ok(_) => log::info!("[cert] ServerCertificateErrorDetected handler attached"),
                    Err(e) => log::error!("[cert] failed to attach ServerCertificateErrorDetected handler: {e:?}"),
                }
            }
        });
    }
}

/// 第二次啟動（交給已在執行的例項）時，是不是應該保持隱藏：帶了 `--minimized`（登入項）、
/// 又不是自動更新裝完後要跳關於頁的那次重新開啟。目前只在 macOS 上用。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn second_launch_should_stay_hidden(args: &[String]) -> bool {
    args.iter().any(|a| a == "--minimized") && !args.iter().any(|a| a == "--open-about")
}

/// 主窗 webview 建不起來時（`failed to create webview ... 0x80070057`），視窗物件沒了，
/// 托盤和懸浮窗還在，但錄音、連線全在主窗網頁裡 —— 使用者看到的是「熱鍵沒反應」，沒有任何提示。
/// 2026-10-12 開發期間撞到 7 次（多在 tauri dev 連續重啟時），正式版少見但一旦發生就是整個軟體失靈。
///
/// 啟動 5 秒後查一次，壞了就按配置重建一次；重建後還壞就彈窗請使用者重啟。
fn spawn_main_window_watchdog(app: tauri::AppHandle, launched_minimized: bool) {
    thread::spawn(move || {
        // 只在開發版：設了這個環境變數就在啟動後故意毀掉主窗，用來驗證下面的重建（真故障沒法穩定復現）。
        if cfg!(debug_assertions) && std::env::var_os("SAYIT_DEBUG_BREAK_MAIN_WINDOW").is_some() {
            thread::sleep(std::time::Duration::from_secs(2));
            let break_app = app.clone();
            let _ = app.run_on_main_thread(move || {
                if let Some(window) = break_app.get_webview_window("main") {
                    commands::system::write_log_line("[main-webview] debug: destroying the main window to test recovery");
                    let _ = window.destroy();
                }
            });
        }
        for attempt in 1..=2u32 {
            thread::sleep(std::time::Duration::from_secs(5));
            let (tx, rx) = std::sync::mpsc::channel();
            let check_app = app.clone();
            let _ = app.run_on_main_thread(move || {
                let _ = tx.send(main_window_is_broken(&check_app));
            });
            if !rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap_or(false) {
                return;
            }
            if attempt == 2 {
                commands::system::write_log_line("[main-webview] main window is still unavailable after recreating; asking the user to restart");
                notify_main_window_unavailable();
                return;
            }
            commands::system::write_log_line("[main-webview] main window webview is unavailable; recreating it");
            let destroy_app = app.clone();
            let _ = app.run_on_main_thread(move || {
                if let Some(window) = destroy_app.get_webview_window("main") {
                    let _ = window.destroy();
                }
            });
            // 等舊視窗的標籤真正釋放，否則同名重建會報「label already exists」
            thread::sleep(std::time::Duration::from_millis(800));
            let build_app = app.clone();
            let _ = app.run_on_main_thread(move || recreate_main_window(&build_app, launched_minimized));
        }
    });
}

fn main_window_is_broken(app: &tauri::AppHandle) -> bool {
    match app.get_webview_window("main") {
        None => true,
        // 正常視窗這裡一定拿得到；建失敗的那種是 Err（日誌裡的 visible=None）
        Some(window) => window.is_visible().is_err(),
    }
}

fn recreate_main_window(app: &tauri::AppHandle, launched_minimized: bool) {
    let Some(config) = app.config().app.windows.iter().find(|w| w.label == "main").cloned() else {
        commands::system::write_log_line("[main-webview] no main window in the app config; cannot recreate");
        return;
    };
    match tauri::WebviewWindowBuilder::from_config(app, &config).and_then(|builder| builder.build()) {
        Ok(window) => {
            prepare_main_window(app, &window);
            if launched_minimized {
                window::set_main_window_memory_low(&window, true);
            } else {
                window::show_main_window(&window);
            }
            commands::system::write_log_line("[main-webview] main window recreated");
        }
        Err(e) => commands::system::write_log_line(&format!("[main-webview] failed to recreate the main window: {e}")),
    }
}

fn notify_main_window_unavailable() {
    let zh = matches!(locale::system_ui_lang(), locale::Lang::ZhTw);
    // mac 上沒有托盤、也不用右鍵：退出入口是螢幕頂部選單欄裡的 SayItLocally 圖示
    let text = match (zh, cfg!(target_os = "macos")) {
        (true, false) => "SayItLocally 的主視窗沒能啟動，語音輸入暫時無法使用。\n\n請在系統匣圖示上按右鍵結束 SayItLocally，再重新開啟。如果反覆出現，請在設定裡提交診斷。",
        (false, false) => "SayItLocally's main window failed to start, so voice input is unavailable for now.\n\nPlease quit SayItLocally from the tray icon and open it again. If this keeps happening, send a diagnostics report from Settings.",
        (true, true) => "SayItLocally 的主視窗沒能啟動，語音輸入暫時無法使用。\n\n請點螢幕上方選單列裡的 SayItLocally 圖示，選「結束 SayItLocally」，再重新開啟。如果反覆出現，請在設定裡提交診斷。",
        (false, true) => "SayItLocally's main window failed to start, so voice input is unavailable for now.\n\nPlease quit SayItLocally from its icon in the menu bar at the top of the screen, then open it again. If this keeps happening, send a diagnostics report from Settings.",
    };
    #[cfg(target_os = "windows")]
    thread::spawn(move || {
        use windows::core::HSTRING;
        use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND};
        unsafe {
            MessageBoxW(None, &HSTRING::from(text), &HSTRING::from("SayItLocally"), MB_OK | MB_ICONWARNING | MB_SETFOREGROUND);
        }
    });
    #[cfg(target_os = "macos")]
    crate::platform::macos::appkit::show_alert_async("SayItLocally", &text);
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let _ = text;
}

/// [自訂] SC-60：主程序在資料庫復原時可能久候，等超過這個時間就放棄轉交。
const STARTUP_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// [自訂] SC-60：主程序已就緒，只註冊單例外掛；外掛在 build 時把參數交給主程序並結束本程序。
/// 若 build 回來（連不到主程序的通道），不當主程序、不建立視窗，直接結束。
/// 不呼叫外掛的 destroy：連線若因其他錯誤失敗，那會刪掉主程序仍在使用的通道。
fn forward_to_running_instance(context: tauri::Context<tauri::Wry>) {
    let _ = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|_, _, _| {}))
        .build(context);
}

fn main() {
    // [自訂] S64：在初始化及啟動執行緒前關閉 transcribe-cpp 的語音特徵傾印。
    std::env::remove_var("TRANSCRIBE_DUMP_DIR");
    // Metal 的 GPU trace 也可能保存辨識運算資料。
    #[cfg(target_os = "macos")]
    std::env::remove_var("GGML_METAL_CAPTURE_COMPUTE");

    // [自訂] SC-60（L-72）：初始化日誌、開資料庫之前先決定是否為唯一實例（見 instance_lock）。
    // 後啟動的程序只轉交參數或直接結束，不寫日誌、不開資料庫、不做復原或清理。
    // generate_context! 只展開一次，前端資源才不會嵌入兩份。
    let context = tauri::generate_context!();
    let claim = instance_lock::claim(&commands::app_data_dir(), STARTUP_LOCK_WAIT);
    let lock_unavailable = match &claim {
        instance_lock::Claim::Unavailable(error) => Some(error.to_string()),
        _ => None,
    };
    let startup_lock = match instance_lock::role(claim) {
        instance_lock::Role::Run(primary) => primary.map(|primary| {
            instance_lock::hold(primary.instance);
            primary.starting
        }),
        instance_lock::Role::Forward => return forward_to_running_instance(context),
        instance_lock::Role::Exit => return,
    };

    // Mirror the Rust log facade to sayit.log as well as stderr. This is essential on
    // Windows release builds (`windows_subsystem = "windows"`), where stderr is invisible.
    // It also captures tauri-runtime-wry errors such as the original WebView2 HRESULT.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format(|buf, record| {
            use std::io::Write;
            let line = format!(
                "[RUST-LOG] level={} target={} {}",
                record.level(),
                record.target(),
                record.args()
            );
            commands::system::write_log_line(&line);
            writeln!(buf, "{line}")
        })
        .init();

    // ── 全域性 panic 鉤子（懸浮窗/熱鍵間歇性失效排查埋點）──
    // 生產環境用 windows_subsystem="windows"，沒有控制台視窗，任何執行緒 panic 的
    // 預設輸出（stderr）都會直接丟失、無跡可尋。鍵盤鉤子回撥、dispatcher 執行緒等
    // 一旦 panic，對應執行緒直接退出但主程序不受影響——表現出來就是"Alt 鍵突然
    // 沒反應了，但軟體看起來還在正常執行"，與使用者反饋的現象高度吻合。
    // 這裡把所有 panic 資訊（發生位置、執行緒名、payload）寫入 sayit.log，
    // 下次復現後可以直接搜尋 "[PANIC]" 定位是哪個執行緒掛了。
    {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            let thread = thread::current();
            let thread_name = thread.name().unwrap_or("<unnamed>");
            let location = panic_info.location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                .unwrap_or_else(|| "<unknown>".to_string());
            let payload = panic_info.payload();
            let msg = if let Some(s) = payload.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "<non-string panic payload>".to_string()
            };
            commands::system::write_log_line(&format!(
                "[PANIC] thread={} at={} msg={}",
                thread_name, location, msg,
            ));
            default_hook(panic_info);
        }));
    }

    if let Some(error) = &lock_unavailable {
        log::warn!("[instance] single-instance lock unavailable, starting without it: {error}");
    }
    log::info!(
        "SayItLocally starting version={} profile={} pid={}",
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) { "debug" } else { "release" },
        std::process::id(),
    );
    log::info!("Log file: {:?}", dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("com.sayitlocally.desktop")
        .join("logs")
        .join("sayit.log"));

    // This must be set before any WebView2 instance is created.
    // Disable Chromium's background tab/window throttling: SayItLocally keeps a long-lived
    // WebSocket connection alive via a 30s JS heartbeat even when the window is minimized
    // or not focused. Without these flags, WebView2 throttles setInterval timers in the
    // background, delaying the heartbeat well past the server's idle timeout and causing
    // spurious "idle timeout" disconnects while the app is just sitting idle in the tray.
    // macOS 沒有 WebView2；隱藏主窗的節流在 tauri.macos.conf.json 用 backgroundThrottling 關掉。
    #[cfg(windows)]
    {
    let (browser_args_source, browser_args) = match std::env::var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS") {
        Ok(value) => ("inherited", value),
        Err(_) => {
            std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", WEBVIEW2_BROWSER_ARGS);
            ("app-default", WEBVIEW2_BROWSER_ARGS.to_string())
        }
    };
    log::info!(
        "WebView2 environment userDataDir={:?} argsSource={} argsLen={} argsFingerprint={}",
        dirs::data_local_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("com.sayitlocally.desktop")
            .join("EBWebView"),
        browser_args_source,
        browser_args.len(),
        browser_args_fingerprint(&browser_args),
    );
    }

    // macOS：同樣要趕在第一個 WebView 建立之前——關掉 WKWebView 輸入框裡的智慧引號和破折號，
    // 否則 Prompt、熱詞裡打的 " 和 -- 會被換成彎引號和長破折號（見 appkit.rs）。
    #[cfg(target_os = "macos")]
    platform::macos::appkit::disable_webkit_smart_substitutions();

    let db_path = commands::app_data_dir().join("sayit.db");

    let (storage, storage_recovery) = Storage::open_or_recover(db_path);
    if let Some(recovery) = storage_recovery {
        notify_storage_recovery(recovery);
    }
    // [自訂] #20／L-70：清除資料庫裡既有的金鑰與密碼（本 fork 不保存）。
    // SCAN1-1：清除失敗時留下紀錄（只記錯誤，不記任何值），不再默默忽略。
    match storage.purge_stored_secrets() {
        Ok(0) => {}
        Ok(n) => log::info!("[storage] cleared stored secrets in {} setting(s)", n),
        Err(e) => {
            log::error!("[storage] failed to clear stored secrets: {}", e);
            commands::system::write_log_line(&format!("[storage] failed to clear stored secrets: {}", e));
        }
    }

    // One-time migration from Electron app's SQLite database
    // [自訂] S37：舊 Electron 版資料遷移已註解停用。原本每次啟動都會檢查
    // ~/Library/Application Support/{SayIt,sayit,SayIt-dev,sayit-dev}/sayit.db，
    // 若存在（例如日後裝了官方 SayIt）會覆蓋所有設定並匯入歷史。
    // if let Err(e) = storage.migrate_from_electron() {
    //     eprintln!("Warning: Electron data migration failed: {}", e);
    // }

    // Read PTT setting before moving storage into managed state
    let ptt_setting_val = storage.get("shortcutPTT", None);
    let ptt_str = ptt_setting_val.as_str().unwrap_or(defaults::SHORTCUT_PTT).to_string();
    let hf_setting_val = storage.get("shortcutHandsFree", None);
    let hf_str = hf_setting_val.as_str().unwrap_or(defaults::SHORTCUT_HANDS_FREE).to_string();
    let ai_toggle_setting_val = storage.get("shortcutToggleAi", None);
    let ai_toggle_str = ai_toggle_setting_val.as_str().unwrap_or(defaults::SHORTCUT_TOGGLE_AI).to_string();
    log::info!("PTT setting from DB: raw={:?} parsed={:?}", ptt_setting_val, ptt_str);
    log::info!("HF setting from DB: raw={:?} parsed={:?}", hf_setting_val, hf_str);

    // 自定義模型儲存目錄：啟動時讀回並設為程序內生效路徑（空串 = 用預設）。
    // 必須在任何取模型路徑的呼叫之前設定，且要在 storage 被 move 進 manage 之前讀。
    // [自訂] SC-55：讀不到設定時沿用預設路徑，但啟動回收會跳過（見 spawn_legacy_reclaim）。
    if let Ok(Some(custom_dir)) = storage.custom_models_dir() {
        log::info!("Custom models dir from DB: {}", custom_dir.display());
        models::downloader::set_custom_models_dir(Some(custom_dir));
    }

    // ⚠️ 這裡**故意不註冊** ggml 計算後端。註冊動作已下沉到 gguf_asr 的懶路徑
    // （ensure_loaded / describe_devices），別再把 init_backends() 加回啟動路徑。
    //
    // 為什麼：註冊會 dlopen exe 旁邊所有 ggml 模組，其中 ggml-vulkan.dll 一被載入就
    // 立刻 vk::createInstance() 建出真實的 Vulkan 上下文。放在這裡意味著**雲 API 和
    // 伺服器模式的使用者也要付這筆錢**：實測白佔約 36 MB 共享視訊記憶體、讓程序出現在任務
    // 管理器的 GPU 程序列表裡（連 engtype_compute 都登記上），還同步阻塞啟動 80~520 ms
    // —— 而這些模式一個模型都不會載入，收益是零。使用者反饋"即便使用雲 API 模式也持續
    // 佔用 GPU"就是這條。
    //
    // 另外 transcribe-cpp 的 init_backends 上游標註為 idempotent 但 NOT retryable，
    // 程序內也沒有反註冊路徑，所以一旦註冊就撤不回來 —— 更不該無條件先做。

    // 回收上一代 sherpa-onnx 時期的模型權重（新引擎讀不了，只白佔幾百 MB ~ 1 GB）。
    // 放後台執行緒，不讓刪除卡住啟動。
    models::local_asr::spawn_legacy_reclaim(&storage);

    // 本地模式：啟動就在後台把模型載入起來，別等使用者第一次按熱鍵才等幾百毫秒~幾秒。
    // 非本地模式不載入，避免白佔幾百 MB 記憶體。
    if storage.get("workMode", None).as_str() == Some("local") {
        let model_id = storage
            .get("localAsr.modelId", None)
            .as_str()
            .unwrap_or("sensevoice-small-gguf")
            .to_string();
        let accelerator = storage
            .get("localAsr.accelerator", None)
            .as_str()
            .unwrap_or("auto")
            .to_string();
        // 必須跟著一起讀：顯示卡選擇是快取 key 的一部分，這裡傳空（自動）而使用者設了
        // 具體某張卡的話，啟動預熱載入的引擎會在第一次口述時因 key 不符被整個丟掉
        // 過載一遍 —— 預熱白做，使用者還要多等一次"卸舊 + 載新 + 預熱"。
        let gpu_device = storage
            .get("localAsr.gpuDevice", None)
            .as_str()
            .unwrap_or("")
            .to_string();
        std::thread::spawn(move || {
            match models::gguf_asr::preload(&model_id, &accelerator, &gpu_device) {
                Ok(()) => log::info!("Startup local model warm-up completed: {}", model_id),
                // 模型沒下載是正常情況（新裝使用者），不當錯誤刷日誌
                Err(e) => log::info!("Startup local model warm-up skipped ({}): {}", model_id, e),
            }
        });
    }

    // 空閒超時解除安裝：預設讓本地模型常駐，使用者可在設定裡選擇 10 / 30 / 60 分鐘。
    // 守護執行緒始終只啟動一個，0 表示僅監聽動態配置、不執行解除安裝。
    let idle_minutes = storage
        .get("localAsr.unloadIdleMinutes", None)
        .as_u64()
        .unwrap_or(0);
    models::gguf_asr::spawn_idle_unloader(idle_minutes);

    let window_state = WindowState::new();
    let keyboard_hook = KeyboardHookManager::new();
    let context_detector = ContextDetector::new();

    tauri::Builder::default()
        // 單例項鎖：必須作為第一個外掛註冊。第二次啟動時不新開程序，
        // 而是喚起已有視窗，避免兩個客戶端同時收全域性熱鍵、各插一份文本。
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // macOS：登入項帶 --minimized 啟動時 SayItLocally 可能已經在跑（使用者先手動開啟了），
            // 這時把啟動請求交給已有例項就夠了，不能把主視窗調出來。
            #[cfg(target_os = "macos")]
            if second_launch_should_stay_hidden(&args) {
                log::info!("Second launch with --minimized while already running; staying hidden");
                return;
            }
            if let Some(w) = app.get_webview_window("main") {
                window::show_main_window(&w);
                if args.iter().any(|a| a == "--open-about") {
                    let _ = w.emit("open-about", ());
                }
            }
        }))
        .manage(storage)
        .manage(window_state)
        .manage(keyboard_hook)
        .manage(context_detector)
        .manage(commands::update_notification::UpdateNotificationState::default())
        .setup(move |app| {
            // [自訂] SC-60：外掛在 build 時初始化單例通道，放開啟動鎖讓後來的程序轉交參數。
            // [自訂] SC-61：外掛 2.5 起 macOS 的通道在背景建立，先等它出現（最多 3 秒）再放開。
            #[cfg(target_os = "macos")]
            if startup_lock.is_some() {
                let socket = instance_lock::single_instance_socket(&app.config().identifier);
                if !instance_lock::wait_for_path(&socket, std::time::Duration::from_secs(3)) {
                    log::warn!("[instance] single-instance channel not ready; releasing the startup lock anyway");
                }
            }
            drop(startup_lock);
            // [自訂] SC-51：取得單一實例後才清理，以免後啟動的程序刪除進行中的還原暫存。
            // [自訂] SC-62：啟動清理也在背景執行，不拖慢 setup。仍在這裡（已確立單一實例、
            // 資料庫復原已在建立 Builder 前完成）才開始；錄音清理以錄音寫入鎖與還原、
            // 播放、儲存互斥，診斷包清理不會刪掉本次產生的新包。
            let retention_app = app.handle().clone();
            thread::spawn(move || {
                // [自訂] S45：清掉上次殘留的診斷包。
                commands::diagnostics::purge_stale_diagnostics_zips();
                // [自訂] S71：在背景清理，不依賴 WebView 計時器或 App 重啟。
                loop {
                    cleanup_retained_files(retention_app.state::<Storage>().inner());
                    thread::sleep(std::time::Duration::from_secs(15 * 60));
                }
            });
            // macOS：登入時要不要啟動只由「登入時自動啟動」決定，不讓系統的「重新登入時重新開啟視窗」
            // 把 SayItLocally 當普通應用不帶引數地拉起來（那樣主視窗會在登入後彈出來，見 appkit.rs）。
            #[cfg(target_os = "macos")]
            platform::macos::appkit::disable_relaunch_on_login();
            // SayItLocally's main window and lazy overlay share one WebView2 user-data directory.
            // Per-window browser arguments violate WebView2's environment compatibility
            // contract. Fail loudly during startup instead of leaving a half-working app
            // whose main window works but whose overlay can never be created.
            let unsafe_windows = app
                .config()
                .app
                .windows
                .iter()
                .filter_map(|window| {
                    window.additional_browser_args.as_ref().map(|args| {
                        format!("{}(argsLen={})", window.label, args.len())
                    })
                })
                .collect::<Vec<_>>();
            if !unsafe_windows.is_empty() {
                let message = format!(
                    "per-window additionalBrowserArgs is forbidden for shared WebView2 data: {}",
                    unsafe_windows.join(", ")
                );
                log::error!("WebView2 configuration invariant failed: {message}");
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, message).into());
            }

            let hook: tauri::State<KeyboardHookManager> = app.state();
            hook.start(app.handle(), &ptt_str, &hf_str, &ai_toggle_str);

            // 每 60s 記錄一次鍵盤鉤子健康快照（[ptt-watchdog] 日誌行），
            // 用於排查"過一段時間後 Alt 說話沒反應"這類間歇性問題：
            // 復現後翻 sayit.log 找最後一次正常快照和第一次異常快照之間的時間窗。
            keyboard::spawn_health_watchdog();

            // 設定視窗圖示（用 ICO 檔案，包含多尺寸幀，Windows 自動選最合適的）
            // 並根據啟動方式決定是否顯示主視窗：
            // 開機自啟會帶 --minimized 引數 → 保持隱藏，靜默停留在托盤
            // 使用者手動開啟則正常顯示（視窗配置為 visible:false，需顯式 show）
            let launched_minimized = std::env::args().any(|arg| arg == "--minimized");
            // 自動更新安裝完成後，看門人程序會帶 --open-about 重新拉起本程式，
            // 用於自動開啟並跳轉到關於頁，讓使用者能確認更新已生效。
            let launched_open_about = std::env::args().any(|arg| arg == "--open-about");
            if let Some(main_window) = app.get_webview_window("main") {
                prepare_main_window(app.handle(), &main_window);

                if !launched_minimized {
                    // show() 只清 SW_HIDE，不解除最小化（iconic）狀態：視窗若以
                    // 最小化狀態誕生，只 show 會停在 -32000 的停車位上，看起來"沒啟動"。
                    // 托盤/單例項路徑都是 show + unminimize，這裡保持一致。
                    window::show_main_window(&main_window);
                    log::info!(
                        "Main window shown (minimized={:?} visible={:?})",
                        main_window.is_minimized().ok(),
                        main_window.is_visible().ok()
                    );
                } else {
                    log::info!("Launched with --minimized, staying hidden in tray");
                    window::set_main_window_memory_low(&main_window, true);
                    // macOS：開機自啟時只出現在選單欄，不在 Dock 裡佔位
                    window::set_dock_icon_visible(app.handle(), false);
                }
                if launched_open_about {
                    let _ = main_window.emit("open-about", ());
                }
            }
            spawn_main_window_watchdog(app.handle().clone(), launched_minimized);

            // 托盤圖示 + 原生選單（macOS：選單欄圖示）
            commands::tray::build_tray(app.handle())?;
            // macOS 頂部選單欄裡的應用選單（中文 / 英文跟隨介面語言，帶 ⌘, 開啟設定）
            #[cfg(target_os = "macos")]
            commands::tray::install_macos_app_menu(app.handle())?;

            // Start WinEvent hook for foreground window monitoring
            let detector: tauri::State<ContextDetector> = app.state();
            detector.start_winevent_hook(app.handle());

            // Register all global shortcuts (hands-free combo + preset-switch shortcuts).
            // Single keys are handled by the keyboard hook, not here.
            {
                let storage: tauri::State<Storage> = app.state();
                commands::shortcuts::register_all_global_shortcuts(app.handle(), storage.inner());
            }

            // 把首次 overlay WebView 的建立成本挪到啟動後的空閒時段。延遲執行避免與主窗
            // 首屏、托盤和鍵盤鉤子爭搶啟動資源；使用者若在此之前已開始口述，預熱會看到
            // overlay 已存在或正在顯示並安全跳過。
            {
                let overlay_app = app.handle().clone();
                let _ = thread::Builder::new()
                    .name("overlay-prewarm".to_string())
                    .spawn(move || {
                        thread::sleep(std::time::Duration::from_millis(1_500));
                        let window_state = overlay_app.state::<WindowState>();
                        window_state.prewarm_overlay(&overlay_app);
                    });
            }

            // 首次安裝時自動啟用開機自啟（僅執行一次）
            // [自訂] S30：不在第一次啟動時自動註冊「登入時啟動」（LaunchAgent）。
            // 只寫入標記，不呼叫 autostart.enable()；使用者仍可在「設定 → 一般」手動開啟。
            {
                use tauri_plugin_autostart::ManagerExt;
                let storage: tauri::State<Storage> = app.state();
                let autostart = app.autolaunch();
                let already_set = storage.get("autoLaunchInitialized", None);
                if already_set.is_null() || already_set.as_str() == Some("") {
                    // 首次執行，註冊開機自啟並標記（新註冊已帶 --minimized 引數）
                    // [自訂] S30 已註解停用：let _ = autostart.enable();
                    let flag = serde_json::json!("true");
                    let _ = storage.set("autoLaunchInitialized", &flag);
                    let _ = storage.set("autoLaunchArgsMigrated", &flag);
                    log::info!("Auto-launch left off on first run (user opt-in only)"); // [自訂] S30
                } else {
                    // 老使用者遷移：舊自啟項不帶 --minimized 引數，重新註冊一次以寫入新引數
                    let migrated = storage.get("autoLaunchArgsMigrated", None);
                    if migrated.as_str() != Some("true") {
                        if autostart.is_enabled().unwrap_or(false) {
                            let _ = autostart.disable();
                            let _ = autostart.enable();
                            log::info!("Auto-launch re-registered with --minimized arg");
                        }
                        let _ = storage.set("autoLaunchArgsMigrated", &serde_json::json!("true"));
                    }
                }
            }

            Ok(())
        })
        // 前端只用 shell(open) 和 dialog；global-shortcut、autostart 是 Rust 自己在用。
        // 更新走自己的 download_update / install_downloaded_update（system.rs），不用 updater 插件。
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        // [自訂] S39：導覽防護。外掛層的 on_navigation 對所有 WebView 生效（主視窗、重建的主視窗、
        // overlay、更新通知視窗），只允許 App 自己的來源（tauri://localhost、http(s)://tauri.localhost、
        // about:blank，debug 建置另加 devUrl）。其他導覽一律取消，包括把連結拖放到視窗
        // （dragDropEnabled:false 時由 WKWebView 原生處理拖放，預設會導覽到該網址）。
        // 新視窗（window.open／target=_blank）：未設定 on_new_window 時，wry 的 macOS
        // WKUIDelegate createWebViewWithConfiguration 直接回傳 None，即預設拒絕。
        .plugin(
            tauri::plugin::Builder::<tauri::Wry, ()>::new("nav-guard")
                .on_navigation(|webview, url| {
                    let dev_url = webview.app_handle().config().build.dev_url.clone();
                    let allowed = net_policy::app_navigation_allowed(url, dev_url.as_ref());
                    if !allowed {
                        // 只記 scheme 與主機，不記完整網址（可能含查詢字串）
                        commands::system::write_log_line(&format!(
                            "[nav-guard] blocked navigation scheme={} host={}",
                            url.scheme(),
                            url.host_str().unwrap_or("-")
                        ));
                    }
                    allowed
                })
                .build(),
        )
        .invoke_handler(commands::storage::with_ordered_settings(tauri::generate_handler![
            // [自訂] L-72
            commands::corrupt_db::list_corrupt_db_copies,
            commands::corrupt_db::reveal_corrupt_db_copy,
            commands::corrupt_db::delete_corrupt_db_copy,
            commands::corrupt_db::recording_cleanup_protected,
            commands::corrupt_db::resume_recording_cleanup,
            // History
            commands::storage::history_list,
            commands::storage::history_count,
            commands::storage::history_add,
            commands::storage::history_update,
            commands::storage::history_delete,
            commands::storage::history_retained_counts, // [自訂] S61
            commands::storage::history_clear_retained,
            commands::storage::history_set_favorite,
            // Window
            commands::window::present_overlay,
            commands::window::show_overlay,
            commands::window::hide_overlay,
            commands::window::update_overlay_state,
            commands::window::overlay_ready,
            commands::window::overlay_render_ack,
            commands::window::get_overlay_health,
            commands::window::overlay_pong,
            // Paste / Context
            commands::paste::paste_text,
            commands::paste::get_probe_result,
            commands::paste::get_recording_context,
            commands::paste::release_recording_focus, // [自訂] L-80
            commands::paste::get_active_app_context,
            commands::paste::copy_text,
            // System
            commands::system::get_client_runtime_info,
            commands::system::get_system_ui_language,
            commands::system::get_auto_launch,
            commands::system::set_auto_launch,
            // [自訂] 更新已註解停用：commands::system::install_downloaded_update,
            // [自訂] 更新已註解停用：commands::system::download_update,
            // [自訂] 更新已註解停用：commands::system::verify_update_package,
            // [自訂] 更新已註解停用：commands::update_notification::sync_update_notification,
            // [自訂] 更新已註解停用：commands::update_notification::get_update_notification,
            // [自訂] 更新已註解停用：commands::update_notification::fit_update_notification,
            commands::system::append_debug_log,
            commands::system::save_audio_to_downloads,
            commands::system::reveal_file_in_folder,
            commands::system::open_folder,
            // Tray
            commands::tray::set_tray_ai_enabled,
            commands::tray::refresh_native_menus,
            // Audio
            commands::audio::save_audio_file,
            commands::audio::save_pcm_as_wav,
            commands::audio::save_pcm_as_wav_raw,
            commands::audio::read_audio_file_bytes,
            commands::audio::save_recording_to_downloads,
            commands::audio::read_audio_file,
            commands::audio::audio_file_exists,
            commands::audio::delete_audio_file,
            commands::audio::protect_playing_recording, // [自訂] S71
            commands::audio::renew_playing_recording,
            commands::audio::release_playing_recording,
            // Audio output mute (錄音期間靜音系統輸出，防回採)
            commands::audio_mute::mute_system_output,
            commands::audio_mute::restore_system_output,
            commands::audio_mute::get_mic_mute_state,
            commands::audio_mute::list_capture_endpoints,
            // Permissions（macOS 的輔助功能 / 麥克風授權狀態與引導）
            commands::permissions::get_platform_permissions,
            commands::permissions::request_accessibility_permission,
            commands::permissions::open_privacy_settings,
            // Shortcuts
            commands::shortcuts::shortcuts_changed,
            commands::shortcuts::test_shortcut,
            commands::shortcuts::get_ptt_physical_key_states,
            commands::shortcuts::set_escape_action_mode,
            commands::shortcuts::set_card_hotkeys,
            commands::shortcuts::set_ptt_lab_config,
            commands::shortcuts::begin_shortcut_capture,
            commands::shortcuts::end_shortcut_capture,
            // Export
            commands::export::save_text_export,
            commands::export::save_export_bundle,
            commands::export::save_full_export,
            // Backup / Restore (配置與全部資料的匯入匯出)
            commands::backup::get_backup_directory,
            commands::backup::inspect_config_export,
            commands::backup::export_config,
            commands::backup::export_full,
            commands::backup::inspect_config_import,
            commands::backup::import_config,
            commands::backup::import_full,
            // [自訂] SC-67：取代規則「替換成」欄位新增時轉成台灣正體
            zh_output::zh_to_taiwan_texts,
            commands::backup::restart_app,
            // WebDAV 備份（配置必備，歷史/音訊可選）
            // [自訂] S18／S35／L-19：WebDAV 停用（會經 Rust 上傳到外部伺服器，不受 CSP 限制）
            // [自訂] 已註解停用：commands::webdav::webdav_test,
            // [自訂] 已註解停用：commands::webdav::webdav_list,
            // [自訂] 已註解停用：commands::webdav::webdav_backup_now,
            // [自訂] 已註解停用：commands::webdav::webdav_restore,
            // Diagnostics
            commands::diagnostics::collect_settings,
            commands::diagnostics::get_diagnostics_preview,
            commands::diagnostics::create_diagnostics_zip,
            commands::diagnostics::create_diagnostics_zip_raw,
            commands::diagnostics::read_diagnostics_zip_bytes,
            commands::diagnostics::read_diagnostics_zip,
            commands::diagnostics::copy_diagnostics_zip,
            commands::diagnostics::discard_diagnostics_zip, // [自訂] S45
            commands::diagnostics::read_log_file,
            commands::diagnostics::open_log_folder,
            // Providers (cloud ASR / AI)
            // [自訂] S17／L-18：cloud_polish、test_ai_connection 保留註冊，但 Rust 端只允許本機 Ollama（registry.rs）。
            providers::registry::cloud_polish,
            // [自訂] S17 已註解停用：providers::registry::cloud_transcribe,
            // [自訂] S17 已註解停用：providers::registry::cloud_transcribe_raw,
            providers::registry::test_ai_connection,
            // [自訂] S17 已註解停用：providers::registry::test_asr_connection,
            // 「這家 ASR 拿熱詞做什麼」的唯一權威來源。前端不再自己維護一份清單 ——
            // 那正是 issue #67 的成因（宣告和實現分處兩側、無人對賬）。
            providers::capabilities::asr_hotword_capability,
            // 同上，全部服務一次給全，供「各服務對熱詞的支援」對照表使用。
            // 那張表因此是實現的投影，不是第二份手寫清單。
            providers::capabilities::asr_hotword_capability_matrix,
            // Doubao realtime streaming ASR
            // [自訂] S17 已註解停用：providers::asr_doubao_realtime::doubao_stream_open,
            // [自訂] S17 已註解停用：providers::asr_doubao_realtime::doubao_stream_send,
            // [自訂] S17 已註解停用：providers::asr_doubao_realtime::doubao_stream_finish,
            // [自訂] S17 已註解停用：providers::asr_doubao_realtime::doubao_stream_close,
            // Qwen realtime streaming ASR
            // [自訂] S17 已註解停用：providers::asr_qwen_realtime::qwen_stream_open,
            // [自訂] S17 已註解停用：providers::asr_qwen_realtime::qwen_stream_send,
            // [自訂] S17 已註解停用：providers::asr_qwen_realtime::qwen_stream_finish,
            // [自訂] S17 已註解停用：providers::asr_qwen_realtime::qwen_stream_close,
            // Qwen-Audio-ASR-Flash streaming, 3.0 and 3.1 (DashScope duplex protocol)
            // [自訂] S17 已註解停用：providers::asr_qwen_audio_stream::qwen_audio_stream_open,
            // [自訂] S17 已註解停用：providers::asr_qwen_audio_stream::qwen_audio_stream_send,
            // [自訂] S17 已註解停用：providers::asr_qwen_audio_stream::qwen_audio_stream_finish,
            // [自訂] S17 已註解停用：providers::asr_qwen_audio_stream::qwen_audio_stream_close,
            // OpenAI realtime transcription (gpt-live-transcribe)
            // [自訂] S17 已註解停用：providers::asr_openai_realtime::openai_live_open,
            // [自訂] S17 已註解停用：providers::asr_openai_realtime::openai_live_send,
            // [自訂] S17 已註解停用：providers::asr_openai_realtime::openai_live_finish,
            // [自訂] S17 已註解停用：providers::asr_openai_realtime::openai_live_close,
            // Gemini Live API transcription (gemini-3.5-transcribe-live)
            // [自訂] S17 已註解停用：providers::asr_gemini_live::gemini_live_open,
            // [自訂] S17 已註解停用：providers::asr_gemini_live::gemini_live_send,
            // [自訂] S17 已註解停用：providers::asr_gemini_live::gemini_live_finish,
            // [自訂] S17 已註解停用：providers::asr_gemini_live::gemini_live_close,
            // Models (local model management)
            models::registry::list_available_models,
            models::registry::list_downloaded_models,
            models::registry::download_model,
            models::registry::delete_model,
            models::registry::open_models_folder,
            models::registry::open_model_folder,
            models::registry::get_models_dir,
            models::registry::set_models_dir,
            models::local_asr::local_transcribe,
            models::local_asr::local_transcribe_raw,
            models::local_asr::preload_local_model,
            models::local_asr::unload_local_model,
            models::local_asr::set_local_model_idle_unload,
            models::local_asr::legacy_models_reclaimed_bytes,
            models::gguf_asr::gguf_asr_diagnostics,
            models::test_audio::run_asr_benchmark,
            models::test_audio::get_test_audio_b64,
        ]))
        .on_window_event(|window, event| {
            // 點選關閉按鈕時隱藏到托盤，而不是退出
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    window::hide_main_window(window);
                }
            }
        })
        // 用 build + run 而不是直接 run：需要在 RunEvent::Exit 上掛"退出時兜底安裝更新"。
        // 使用者沒點更新圖示就直接關掉 SayItLocally 時，在這裡把已下載的新版靜默裝掉，
        // 下次開啟即是新版 —— 否則更新完全依賴使用者主動點選，不點的人永遠留在舊版。
        .build(context)
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // macOS：主窗藏起來後點 Dock 圖示，系統發 Reopen，此時把主窗顯示出來。
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                if let Some(w) = app_handle.get_webview_window("main") {
                    window::show_main_window(&w);
                }
            }
            if let tauri::RunEvent::Exit = event {
                // [自訂] 結束時自動安裝更新已註解停用：
                // commands::system::install_pending_update_on_exit(app_handle);
                let _ = app_handle;
                // [自訂] SC-62 補充（F10b）：結束前把佇列裡的前端日誌寫完（有上限，不拖住結束）。
                commands::system::flush_debug_log(commands::system::DEBUG_LOG_FLUSH_TIMEOUT);
            }
        });
}

#[cfg(test)]
#[test]
fn recovery_session_preserves_recordings_even_with_a_fresh_database() {
    let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
    let disk = Storage::new(dir.join("sayit.db")).unwrap();
    let memory = Storage::new(std::path::PathBuf::from(":memory:")).unwrap();
    assert_eq!(recording_retention_days(&disk, &dir), Some(7));
    assert_eq!(recording_retention_days(&memory, &dir), None);
    for storage in [&disk, &memory] {
        storage.set("audioRetentionDays", &serde_json::json!(30)).unwrap();
    }
    assert_eq!(recording_retention_days(&disk, &dir), Some(30));
    assert_eq!(recording_retention_days(&memory, &dir), None);
    drop(disk);
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(test)]
mod second_launch_tests {
    use super::second_launch_should_stay_hidden;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn login_item_launch_while_running_stays_hidden() {
        assert!(second_launch_should_stay_hidden(&args(&["/Applications/SayIt.app/Contents/MacOS/sayit", "--minimized"])));
    }

    #[test]
    fn manual_launch_and_post_update_relaunch_show_the_window() {
        assert!(!second_launch_should_stay_hidden(&args(&["/Applications/SayIt.app/Contents/MacOS/sayit"])));
        assert!(!second_launch_should_stay_hidden(&args(&["sayit", "--minimized", "--open-about"])));
        assert!(!second_launch_should_stay_hidden(&args(&["sayit", "--open-about"])));
    }
}

#[cfg(test)]
mod config_tests {
    use serde_json::Value;

    #[test]
    fn configured_windows_do_not_override_webview2_browser_args() {
        let config: Value = serde_json::from_str(include_str!("../tauri.conf.json"))
            .expect("tauri.conf.json must be valid JSON");
        let windows = config
            .pointer("/app/windows")
            .and_then(Value::as_array)
            .expect("tauri.conf.json must define app.windows");

        let offenders = windows
            .iter()
            .filter_map(|window| {
                window
                    .get("additionalBrowserArgs")
                    .filter(|value| !value.is_null())
                    .map(|_| {
                        window
                            .get("label")
                            .and_then(Value::as_str)
                            .unwrap_or("<unknown>")
                    })
            })
            .collect::<Vec<_>>();

        assert!(
            offenders.is_empty(),
            "WebView2 args must be global; per-window overrides found on: {offenders:?}"
        );
    }

    #[test]
    fn webview2_args_keep_certificate_validation() {
        assert!(
            !super::WEBVIEW2_BROWSER_ARGS.contains("ignore-certificate-errors"),
            "certificate errors are handled per host by ServerCertificateErrorDetected; do not disable validation globally"
        );
    }
}
