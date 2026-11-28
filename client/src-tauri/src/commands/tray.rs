//! 托盤（Windows）/ 選單欄（macOS）圖示和它的原生選單，外加 macOS 頂部的應用選單。
//!
//! Windows 上曾經用一個預建立、隱藏的 WebView 視窗自繪托盤選單（為了繞開原生選單的
//! 系統最小寬度、做出帶開關的樣式）。2026-10 去掉了：那個視窗常駐一個 WebView2 渲染程序，
//! 只為一個三項選單，記憶體不值；還額外揹著 CSS 縮放、跨屏定位、失焦收起這些自己造的輪子。
//! 現在兩個平台都用原生選單，別為了樣式再改回網頁選單（見 .kiro/decisions.md）。

use crate::storage::Storage;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

/// 托盤選單裡各項的控制代碼：「AI 整理」狀態變化時更新勾選，切換介面語言時改文字。
pub struct TrayMenu {
    open_item: tauri::menu::MenuItem<tauri::Wry>,
    ai_item: tauri::menu::CheckMenuItem<tauri::Wry>,
    quit_item: tauri::menu::MenuItem<tauri::Wry>,
}

/// 三項：開啟、AI 整理、退出。
///
/// 「AI 整理」兩套文字都要：原生勾選項關閉時只是**沒有對勾**，一眼看不出是關著還是
/// 這一項本來就沒有勾選狀態（使用者反饋）。所以文字裡再寫明狀態，勾 + 括號兩個訊號。
struct TrayLabels {
    open: &'static str,
    ai_on: &'static str,
    ai_off: &'static str,
    quit: &'static str,
}

impl TrayLabels {
    fn ai(&self, enabled: bool) -> &'static str {
        if enabled {
            self.ai_on
        } else {
            self.ai_off
        }
    }
}

const TRAY_OPEN: &str = "tray-open";
const TRAY_AI: &str = "tray-ai";
const TRAY_QUIT: &str = "tray-quit";

fn current_ai_enabled(storage: &Storage) -> bool {
    storage.get("aiEnabled", None).as_bool().unwrap_or(false) // [自訂] 預設關閉
}

fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        crate::window::show_main_window(&window);
    }
}

/// 勾選和括號裡的狀態文字一起更新，兩者必須同源，否則會出現「打著勾、寫著已關閉」。
fn sync_ai_item(app: &AppHandle, enabled: bool) {
    if let Some(menu) = app.try_state::<TrayMenu>() {
        let labels = tray_labels(app.state::<Storage>().inner());
        let _ = menu.ai_item.set_checked(enabled);
        let _ = menu.ai_item.set_text(labels.ai(enabled));
    }
}

/// 原生選單的文字跟隨介面語言設定（`ui.language`：auto 時跟系統）。
/// 原生選單不在網頁裡，前端的 i18n 管不到它，所以 Rust 自己判一次。
fn ui_is_chinese(storage: &Storage) -> bool {
    let setting = storage.get("ui.language", None);
    ui_lang_is_chinese(setting.as_str(), crate::locale::system_ui_lang)
}

/// [自訂] SC-65：設定值 zh-TW（或舊版的 zh-CN）→ 正體中文；auto、空值或不認得的值跟系統。
/// 系統語言用閉包傳入，只有需要時才讀，也讓測試不必碰真實系統設定。
fn ui_lang_is_chinese(setting: Option<&str>, system: impl FnOnce() -> crate::locale::Lang) -> bool {
    let lang = setting
        .and_then(crate::locale::Lang::from_setting)
        .unwrap_or_else(system);
    lang == crate::locale::Lang::ZhTw
}

fn tray_labels(storage: &Storage) -> TrayLabels {
    tray_labels_for(ui_is_chinese(storage), cfg!(target_os = "macos"))
}

/// 系統匣選單文字（純函式，便於測試）。
fn tray_labels_for(zh: bool, macos: bool) -> TrayLabels {
    // mac 選單的慣例是「結束 SayItLocally」；Windows 托盤選單慣例只寫「結束」，與原來的網頁選單一致。
    let quit = match (zh, macos) {
        (true, true) => "結束 SayItLocally",
        (true, false) => "結束",
        (false, true) => "Quit SayItLocally",
        (false, false) => "Quit",
    };
    if zh {
        TrayLabels { open: "開啟 SayItLocally", ai_on: "AI 整理（已開啟）", ai_off: "AI 整理（已關閉）", quit }
    } else {
        TrayLabels { open: "Open SayItLocally", ai_on: "AI cleanup (on)", ai_off: "AI cleanup (off)", quit }
    }
}

/// 主視窗裡切換「AI 整理」後，同步托盤選單的勾選（主視窗的開關已經負責落庫）。
#[tauri::command]
pub fn set_tray_ai_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    sync_ai_item(&app, enabled);
    Ok(())
}

/// 托盤裡切換 AI 整理：落庫並通知主視窗（主視窗 store 同步記憶體與錄音器快取）。
fn toggle_ai_enabled(app: &AppHandle, storage: &Storage) -> Result<bool, String> {
    let next = !current_ai_enabled(storage);
    storage
        .set("aiEnabled", &json!(next))
        .map_err(|error| error.to_string())?;
    let _ = app.emit("ai-cleanup-changed", json!({ "enabled": next }));
    sync_ai_item(app, next);
    log::info!("[tray-menu] AI cleanup toggled to {next}");
    Ok(next)
}

/// 托盤圖示。Windows：由 256px 原圖直接生成的幾檔尺寸，按主窗所在螢幕的縮放比例選最接近
/// 托盤槽位的那張，避免 256 → 32 → 系統實際尺寸的二次縮放（小圖做過輕微銳化）。
#[cfg(not(target_os = "macos"))]
fn tray_icon(app: &AppHandle) -> tauri::Result<tauri::image::Image<'static>> {
    let tray_scale = app
        .get_webview_window("main")
        .and_then(|window| window.scale_factor().ok())
        .unwrap_or(1.0);
    let (bytes, size): (&'static [u8], u32) = if tray_scale >= 1.75 {
        (include_bytes!("../../icons/tray-32.png"), 32)
    } else if tray_scale >= 1.375 {
        (include_bytes!("../../icons/tray-24.png"), 24)
    } else if tray_scale >= 1.125 {
        (include_bytes!("../../icons/tray-20.png"), 20)
    } else {
        (include_bytes!("../../icons/tray-16.png"), 16)
    };
    log::info!("[tray] icon size={}px for scale factor {:.2}", size, tray_scale);
    tauri::image::Image::from_bytes(bytes)
}

/// macOS：單色模板圖，系統按淺色/深色選單欄自動著色。
#[cfg(target_os = "macos")]
fn tray_icon(_app: &AppHandle) -> tauri::Result<tauri::image::Image<'static>> {
    tauri::image::Image::from_bytes(include_bytes!("../../icons/tray-template.png"))
}

/// 建托盤圖示和原生選單。
///
/// - Windows：左鍵開啟主視窗，右鍵彈選單（系統托盤的慣例）。
/// - macOS：點一下就彈選單（選單欄圖示的慣例），開啟主視窗走選單第一項。
pub fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    use tauri::menu::{CheckMenuItemBuilder, MenuBuilder, MenuItemBuilder};
    use tauri::tray::TrayIconBuilder;

    let storage = app.state::<Storage>();
    let labels = tray_labels(storage.inner());
    let ai_enabled = current_ai_enabled(storage.inner());
    let open = MenuItemBuilder::with_id(TRAY_OPEN, labels.open).build(app)?;
    let ai = CheckMenuItemBuilder::with_id(TRAY_AI, labels.ai(ai_enabled))
        .checked(ai_enabled)
        .build(app)?;
    let quit_builder = MenuItemBuilder::with_id(TRAY_QUIT, labels.quit);
    // mac 選單的慣例是在「退出」旁標出 ⌘Q（只在選單展開時生效，和頂部應用選單的 ⌘Q 不衝突）。
    // Windows 托盤選單沒有這個慣例，標了反而像是全域性快捷鍵。
    #[cfg(target_os = "macos")]
    let quit_builder = quit_builder.accelerator("CmdOrCtrl+Q");
    let quit = quit_builder.build(app)?;
    let menu = MenuBuilder::new(app)
        .item(&open)
        .item(&ai)
        .separator()
        .item(&quit)
        .build()?;
    app.manage(TrayMenu { open_item: open, ai_item: ai, quit_item: quit });

    let builder = TrayIconBuilder::with_id("sayit-tray")
        .icon(tray_icon(app)?)
        .tooltip("SayItLocally")
        .menu(&menu)
        .show_menu_on_left_click(cfg!(target_os = "macos"))
        .on_menu_event(|app, event| match event.id().as_ref() {
            TRAY_OPEN => show_main(app),
            TRAY_AI => {
                let storage = app.state::<Storage>();
                if let Err(error) = toggle_ai_enabled(app, storage.inner()) {
                    log::warn!("[tray-menu] toggle AI cleanup failed: {error}");
                    // 原生勾選項點一下就會自己翻轉，失敗時要翻回去
                    sync_ai_item(app, current_ai_enabled(storage.inner()));
                }
            }
            TRAY_QUIT => app.exit(0),
            _ => {}
        });

    #[cfg(target_os = "macos")]
    let builder = builder.icon_as_template(true);

    #[cfg(not(target_os = "macos"))]
    let builder = builder.on_tray_icon_event(|tray, event| {
        use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            show_main(tray.app_handle());
        }
    });

    builder.build(app)?;
    Ok(())
}

// ─── macOS 頂部選單欄裡的應用選單 ───

#[cfg(target_os = "macos")]
const APP_MENU_ABOUT: &str = "app-about";
#[cfg(target_os = "macos")]
const APP_MENU_SETTINGS: &str = "app-settings";

/// macOS 螢幕頂部選單欄裡本程式的選單（SayItLocally / 編輯 / 視窗），文字跟隨介面語言。
///
/// 不用 Tauri 的預設選單：那套是英文的，還帶著用不上的「File」「View」「Help」。
/// 「編輯」選單不能省：WKWebView 裡輸入框的 ⌘C / ⌘V / ⌘X / ⌘A / ⌘Z 要靠它的快捷鍵才生效。
/// macOS 頂部 App 選單的全部文字。
///
/// [自訂] SC-65：中文用台灣 macOS 的慣用詞（結束、還原、剪下、拷貝、貼上、縮到最小…）。
/// 抽成純函式，讓測試不必建選單就能鎖住文字與語言判斷。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct AppMenuLabels {
    about: &'static str,
    settings: &'static str,
    services: &'static str,
    hide: &'static str,
    hide_others: &'static str,
    show_all: &'static str,
    quit: &'static str,
    edit: &'static str,
    undo: &'static str,
    redo: &'static str,
    cut: &'static str,
    copy: &'static str,
    paste: &'static str,
    select_all: &'static str,
    window: &'static str,
    minimize: &'static str,
    zoom: &'static str,
    close_window: &'static str,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn app_menu_labels(zh: bool) -> AppMenuLabels {
    if zh {
        AppMenuLabels {
            about: "關於 SayItLocally",
            settings: "設定…",
            services: "服務",
            hide: "隱藏 SayItLocally",
            hide_others: "隱藏其他",
            show_all: "全部顯示",
            quit: "結束 SayItLocally",
            edit: "編輯",
            undo: "還原",
            redo: "重做",
            cut: "剪下",
            copy: "拷貝",
            paste: "貼上",
            select_all: "全選",
            window: "視窗",
            minimize: "縮到最小",
            zoom: "縮放",
            close_window: "關閉視窗",
        }
    } else {
        AppMenuLabels {
            about: "About SayItLocally",
            settings: "Settings…",
            services: "Services",
            hide: "Hide SayItLocally",
            hide_others: "Hide Others",
            show_all: "Show All",
            quit: "Quit SayItLocally",
            edit: "Edit",
            undo: "Undo",
            redo: "Redo",
            cut: "Cut",
            copy: "Copy",
            paste: "Paste",
            select_all: "Select All",
            window: "Window",
            minimize: "Minimize",
            zoom: "Zoom",
            close_window: "Close Window",
        }
    }
}

#[cfg(target_os = "macos")]
fn build_macos_app_menu(app: &AppHandle) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder};

    let l = app_menu_labels(ui_is_chinese(app.state::<Storage>().inner()));

    let about = MenuItemBuilder::with_id(APP_MENU_ABOUT, l.about).build(app)?;
    let settings = MenuItemBuilder::with_id(APP_MENU_SETTINGS, l.settings)
        .accelerator("CmdOrCtrl+,")
        .build(app)?;
    let app_menu = SubmenuBuilder::new(app, "SayItLocally")
        .item(&about)
        .separator()
        .item(&settings)
        .separator()
        .item(&PredefinedMenuItem::services(app, Some(l.services))?)
        .separator()
        .item(&PredefinedMenuItem::hide(app, Some(l.hide))?)
        .item(&PredefinedMenuItem::hide_others(app, Some(l.hide_others))?)
        .item(&PredefinedMenuItem::show_all(app, Some(l.show_all))?)
        .separator()
        .item(&PredefinedMenuItem::quit(app, Some(l.quit))?)
        .build()?;
    let edit_menu = SubmenuBuilder::new(app, l.edit)
        .item(&PredefinedMenuItem::undo(app, Some(l.undo))?)
        .item(&PredefinedMenuItem::redo(app, Some(l.redo))?)
        .separator()
        .item(&PredefinedMenuItem::cut(app, Some(l.cut))?)
        .item(&PredefinedMenuItem::copy(app, Some(l.copy))?)
        .item(&PredefinedMenuItem::paste(app, Some(l.paste))?)
        .item(&PredefinedMenuItem::select_all(app, Some(l.select_all))?)
        .build()?;
    let window_menu = SubmenuBuilder::new(app, l.window)
        .item(&PredefinedMenuItem::minimize(app, Some(l.minimize))?)
        .item(&PredefinedMenuItem::maximize(app, Some(l.zoom))?)
        .separator()
        .item(&PredefinedMenuItem::close_window(app, Some(l.close_window))?)
        .build()?;
    MenuBuilder::new(app)
        .items(&[&app_menu, &edit_menu, &window_menu])
        .build()
}

/// 裝上 macOS 應用選單，並處理「關於」「設定…」兩項（其餘是系統自帶的標準項）。
#[cfg(target_os = "macos")]
pub fn install_macos_app_menu(app: &AppHandle) -> tauri::Result<()> {
    app.set_menu(build_macos_app_menu(app)?)?;
    app.on_menu_event(|app, event| {
        let page_event = match event.id().as_ref() {
            APP_MENU_ABOUT => "open-about",
            APP_MENU_SETTINGS => "open-settings",
            _ => return,
        };
        show_main(app);
        let _ = app.emit(page_event, ());
    });
    Ok(())
}

/// 介面語言切換後重新整理原生選單：托盤選單（兩個平台），以及 mac 的頂部應用選單。
#[tauri::command]
pub fn refresh_native_menus(app: AppHandle) {
    if let Some(menu) = app.try_state::<TrayMenu>() {
        let storage = app.state::<Storage>();
        let labels = tray_labels(storage.inner());
        let _ = menu.open_item.set_text(labels.open);
        let _ = menu.ai_item.set_text(labels.ai(current_ai_enabled(storage.inner())));
        let _ = menu.quit_item.set_text(labels.quit);
    }
    #[cfg(target_os = "macos")]
    match build_macos_app_menu(&app) {
        Ok(menu) => {
            let _ = app.set_menu(menu);
        }
        Err(error) => log::warn!("[app-menu] rebuild failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locale::Lang;

    /// 簡體專用字（以 Unicode 跳脫保存，讓原始碼本身不含簡體）：舊版選單用過的字，加上常見的
    /// 介面用字。中文標籤不得出現其中任何一個。
    const SIMPLIFIED_ONLY: &str = "\u{5173}\u{4e8e}\u{8bbe}\u{52a1}\u{9690}\u{663e}\u{7f16}\u{8f91}\u{9500}\u{8d1d}\u{8d34}\u{7f29}\u{95ed}\u{5f00}\u{542f}\u{9009}\u{62e9}\u{5f55}\u{952e}\u{8bc6}\u{522b}\u{8bed}\u{8f93}\u{65f6}\u{95f4}\u{8bf7}\u{8fd9}\u{4e3a}\u{6267}\u{4ece}\u{53d1}\u{5e94}\u{603b}\u{7ed3}\u{94ae}\u{5904}\u{56fe}\u{6807}\u{9875}\u{9879}\u{7075}";

    fn app_menu_texts(l: &AppMenuLabels) -> Vec<&'static str> {
        vec![
            l.about, l.settings, l.services, l.hide, l.hide_others, l.show_all, l.quit, l.edit, l.undo,
            l.redo, l.cut, l.copy, l.paste, l.select_all, l.window, l.minimize, l.zoom, l.close_window,
        ]
    }

    fn tray_texts(l: &TrayLabels) -> Vec<&'static str> {
        vec![l.open, l.ai_on, l.ai_off, l.quit]
    }

    fn assert_no_simplified(texts: &[&str]) {
        for text in texts {
            assert!(
                !text.chars().any(|c| SIMPLIFIED_ONLY.contains(c)),
                "label contains a simplified-only character: {text}"
            );
        }
    }

    #[test]
    fn app_menu_uses_taiwan_macos_wording() {
        let zh = app_menu_labels(true);
        assert_no_simplified(&app_menu_texts(&zh));
        assert_eq!(
            app_menu_texts(&zh),
            [
                "關於 SayItLocally", "設定…", "服務", "隱藏 SayItLocally", "隱藏其他", "全部顯示",
                "結束 SayItLocally", "編輯", "還原", "重做", "剪下", "拷貝", "貼上", "全選", "視窗",
                "縮到最小", "縮放", "關閉視窗",
            ]
        );
        // 與系統匣的結束項一致。
        assert_eq!(zh.quit, tray_labels_for(true, true).quit);
    }

    #[test]
    fn english_labels_have_no_chinese() {
        let en_menu = app_menu_texts(&app_menu_labels(false));
        let mut texts = en_menu.clone();
        for macos in [true, false] {
            texts.extend(tray_texts(&tray_labels_for(false, macos)));
        }
        for text in texts {
            assert!(text.is_ascii() || text.ends_with('…'), "{text}");
            assert!(!text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)), "{text}");
        }
        assert_eq!(app_menu_labels(false).quit, "Quit SayItLocally");
    }

    #[test]
    fn tray_labels_are_traditional_chinese() {
        for macos in [true, false] {
            let zh = tray_labels_for(true, macos);
            assert_no_simplified(&tray_texts(&zh));
            assert_eq!(zh.open, "開啟 SayItLocally");
            assert_eq!(zh.ai(true), "AI 整理（已開啟）");
            assert_eq!(zh.ai(false), "AI 整理（已關閉）");
        }
        assert_eq!(tray_labels_for(true, false).quit, "結束");
        assert_eq!(tray_labels_for(false, false).quit, "Quit");
    }

    #[test]
    fn menu_language_follows_setting_then_system() {
        let never = || -> Lang { panic!("system language must not be read when the setting decides") };
        assert!(ui_lang_is_chinese(Some("zh-TW"), never));
        assert!(ui_lang_is_chinese(Some("zh-CN"), never), "legacy zh-CN setting stays Chinese");
        assert!(!ui_lang_is_chinese(Some("en"), never));
        for auto in [None, Some("auto"), Some(""), Some("zh-HK")] {
            assert!(ui_lang_is_chinese(auto, || Lang::ZhTw), "{auto:?}");
            assert!(!ui_lang_is_chinese(auto, || Lang::En), "{auto:?}");
        }
    }
}
