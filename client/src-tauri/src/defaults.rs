//! 前端 `src/services/defaults.ts` 裡也有、Rust 側要用到的預設值，Rust 這邊只在這裡寫一份。
//!
//! 原來同一個預設鍵在 Rust 裡抄了四處（storage 種子、main.rs 兜底、shortcuts.rs 兩處、
//! keyboard 的 fallback），改漏一處就是"介面顯示一個鍵、實際監聽另一個鍵"。
//! 兩種語言沒法共用一個常量，所以下面的測試直接讀 defaults.ts 的原始碼對賬：
//! 任何一邊改了、另一邊沒跟上，`cargo test` 就紅。

// 兩個快捷鍵預設值按平台不同，先用宏給出字面量：storage 的種子是 JSON 字面量，
// `concat!` 只吃字面量，用不了下面的 const。
//
// Windows：按住說話 = 右 Ctrl，擴音 = 右 Alt。
// macOS：MacBook 內建鍵盤沒有右 Ctrl，改成按住說話 = 右 ⌥（AltRight），擴音 = 右 ⌘（MetaRight）。
// 前端對應 services/defaults.ts 的 DEFAULTS 與 MAC_DEFAULT_OVERRIDES，下面的測試會對賬。
#[cfg(not(target_os = "macos"))]
#[macro_export]
macro_rules! default_shortcut_ptt {
    () => {
        "ControlRight"
    };
}
#[cfg(target_os = "macos")]
#[macro_export]
macro_rules! default_shortcut_ptt {
    () => {
        "AltRight"
    };
}
#[cfg(not(target_os = "macos"))]
#[macro_export]
macro_rules! default_shortcut_hands_free {
    () => {
        "AltRight"
    };
}
#[cfg(target_os = "macos")]
#[macro_export]
macro_rules! default_shortcut_hands_free {
    () => {
        "MetaRight"
    };
}

/// 按住說話的預設鍵。不能是 Shift：長按右 Shift 約 8 秒會觸發 Windows 篩選鍵，
/// 鬆開後錄音停不下來（前端 lib/shortcutKeys.ts 的 PTT_FORBIDDEN_CODES）。
pub const SHORTCUT_PTT: &str = crate::default_shortcut_ptt!();
/// 擴音模式的預設鍵。
pub const SHORTCUT_HANDS_FREE: &str = crate::default_shortcut_hands_free!();
/// AI 整理開關的快捷鍵。留空 = 不註冊，避免升級時意外佔用使用者已有的全域性組合鍵。
pub const SHORTCUT_TOGGLE_AI: &str = "";

/// WebDAV 備份預設只含配置：錄音庫能有幾個 GB，網盤有流量額度，預設開啟等於替使用者做了很貴的決定。
pub const WEBDAV_INCLUDE_HISTORY: bool = false;
pub const WEBDAV_INCLUDE_AUDIO: bool = false;
pub const WEBDAV_KEEP_COUNT: u64 = 5;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    const DEFAULTS_TS: &str = include_str!("../../src/services/defaults.ts");

    /// macOS 上覆蓋預設值的那一段（`MAC_DEFAULT_OVERRIDES = { … }`）。
    fn mac_overrides_block() -> &'static str {
        let start = DEFAULTS_TS
            .find("export const MAC_DEFAULT_OVERRIDES")
            .expect("defaults.ts has no MAC_DEFAULT_OVERRIDES block");
        let rest = &DEFAULTS_TS[start..];
        let end = rest.find("\n}").expect("MAC_DEFAULT_OVERRIDES block is not closed");
        &rest[..end]
    }

    fn find_line<'a>(source: &'a str, key: &str) -> Option<&'a str> {
        let bare = format!("{key}:");
        let quoted = format!("'{key}':");
        source
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with(&bare) || l.starts_with(&quoted))
    }

    /// 取 defaults.ts 裡 `key: 值,` 那一行的值，按 JSON 解析（只支援字串、布林、數字這類單行字面量）。
    /// macOS 上先看 MAC_DEFAULT_OVERRIDES，那裡沒有才用通用 DEFAULTS。
    fn ts_default(key: &str) -> Value {
        let mac_line = if cfg!(target_os = "macos") {
            find_line(mac_overrides_block(), key)
        } else {
            None
        };
        let line = mac_line
            .or_else(|| find_line(DEFAULTS_TS, key))
            .unwrap_or_else(|| panic!("defaults.ts has no `{key}` entry"));
        let raw = line.splitn(2, ':').nth(1).unwrap();
        let raw = raw.split("//").next().unwrap().trim().trim_end_matches(',').trim();
        let as_json = if raw.starts_with('\'') && raw.ends_with('\'') {
            format!("\"{}\"", &raw[1..raw.len() - 1])
        } else {
            raw.to_string()
        };
        serde_json::from_str(&as_json).unwrap_or_else(|e| panic!("cannot parse `{key}` = {raw}: {e}"))
    }

    #[test]
    fn rust_defaults_match_frontend_defaults() {
        assert_eq!(ts_default("shortcutPTT"), json!(SHORTCUT_PTT));
        assert_eq!(ts_default("shortcutHandsFree"), json!(SHORTCUT_HANDS_FREE));
        assert_eq!(ts_default("shortcutToggleAi"), json!(SHORTCUT_TOGGLE_AI));
        assert_eq!(ts_default("webdav.includeHistory"), json!(WEBDAV_INCLUDE_HISTORY));
        assert_eq!(ts_default("webdav.includeAudio"), json!(WEBDAV_INCLUDE_AUDIO));
        assert_eq!(ts_default("webdav.keepCount"), json!(WEBDAV_KEEP_COUNT));
        assert!(!SHORTCUT_PTT.contains("Shift"));
    }

    /// storage 的種子是 JSON 字面量，用不了常量，所以單獨對一遍。
    #[test]
    fn storage_seed_matches_defaults() {
        let seed = |key: &str| -> Value {
            let (_, raw) = crate::storage::DEFAULT_SETTINGS
                .iter()
                .find(|(k, _)| *k == key)
                .unwrap_or_else(|| panic!("storage seed has no `{key}`"));
            serde_json::from_str(raw).unwrap()
        };
        assert_eq!(seed("shortcutPTT"), json!(SHORTCUT_PTT));
        assert_eq!(seed("shortcutHandsFree"), json!(SHORTCUT_HANDS_FREE));
        assert_eq!(seed("shortcutToggleAi"), json!(SHORTCUT_TOGGLE_AI));
        assert_eq!(seed("selectedMic"), ts_default("selectedMic"));
    }
}
