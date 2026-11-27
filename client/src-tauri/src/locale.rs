//! 界面语言：系统显示语言读取 + `ui.language` 偏好解析。
//!
//! 为什么这件事必须有 Rust 侧的一份：**托盘菜单是原生菜单、在 Rust 建**
//! （commands/tray.rs 的 build_tray），要它开机就是正确语言，就不能等前端起来再告诉它。
//!
//! 为什么不加 `sys-locale` 依赖：`Win32_Globalization` 已经在 Cargo.toml 的
//! windows feature 列表里，需要的 API 直接可用。这是 Windows 独占应用，
//! 没必要为一次读取多背一个 crate。
//!
//! ⚠️ **不要和 `commands/system.rs` 的 `get_system_locale()` 合并**，那个用的是
//! `GetUserDefaultLocaleName`（**区域格式**，如日期货币怎么写），报给诊断用；
//! 这里用 `GetUserDefaultUILanguage`（**显示语言**，Windows 界面本身什么语言）。
//! 两者可以不一致 —— 英文版 Windows 把区域设成中国是很常见的组合。
//! 界面语言只能跟显示语言，跟区域格式会把英文用户判成中文。

/// 支援的介面語言。取值與前端 `Locale` 一一對應。
///
/// [自訂] SC-65：只提供正體中文（台灣）與英文；簡體中文已移除。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    ZhTw,
    En,
}

impl Lang {
    /// 前端 `Locale` 用的標籤，也是這個模組對外唯一的字串形態。
    pub fn tag(self) -> &'static str {
        match self {
            Lang::ZhTw => "zh-TW",
            Lang::En => "en",
        }
    }

    /// `ui.language` 設定值 → 介面語言。舊版存的 `zh-CN` 視為正體中文；
    /// `auto`、空值或不認得的值回 None（跟隨系統）。
    pub fn from_setting(value: &str) -> Option<Lang> {
        match value {
            "zh-TW" | "zh-CN" => Some(Lang::ZhTw),
            "en" => Some(Lang::En),
            _ => None,
        }
    }
}

/// Windows 語言 ID（LANGID）→ 介面語言：任何中文都用正體中文（台灣），其餘英文。
#[cfg_attr(not(windows), allow(dead_code))]
fn lang_from_langid(langid: u16) -> Lang {
    const LANG_CHINESE: u16 = 0x04;
    if langid & 0x03ff == LANG_CHINESE {
        Lang::ZhTw
    } else {
        Lang::En
    }
}

/// 系统显示语言。取不到时按英文处理（英文是更安全的默认：看不懂中文的人更多）。
#[cfg(windows)]
pub fn system_ui_lang() -> Lang {
    use windows::Win32::Globalization::GetUserDefaultUILanguage;

    let langid = unsafe { GetUserDefaultUILanguage() };
    if langid == 0 {
        return Lang::En;
    }
    lang_from_langid(langid)
}

/// macOS：「系统设置 → 通用 → 语言与地区」里排第一的首选语言。
///
/// 不能读 `LANG`：从 Finder / Dock / 登录项启动的 App 一般没有这个环境变量，
/// 中文系统会被误判成英文。
#[cfg(target_os = "macos")]
pub fn system_ui_lang() -> Lang {
    lang_from_language_tag(
        crate::platform::macos::sysinfo::preferred_languages()
            .first()
            .map(String::as_str)
            .unwrap_or(""),
    )
}

/// BCP-47／POSIX 語言標籤 → 介面語言。
///
/// [自訂] SC-65：任何中文（zh、zh-CN、zh-TW、zh-Hant、zh_HK.UTF-8…）都用正體中文（台灣），
/// 其餘英文。規則與前端 `resolveLocale` 相同。
pub fn lang_from_language_tag(tag: &str) -> Lang {
    let tag = tag.trim().to_ascii_lowercase().replace('_', "-");
    if tag.split(['-', '.', '@']).next() == Some("zh") {
        Lang::ZhTw
    } else {
        Lang::En
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn system_ui_lang() -> Lang {
    // 其它平台目前只有 cargo test 会走到。
    lang_from_language_tag(&std::env::var("LANG").unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_chinese_langid_maps_to_traditional() {
        // LANGID = 子語言 << 10 | 主語言；0x0404 台灣、0x0804 中國、0x0c04 香港、0x1004 新加坡、0x1404 澳門
        for id in [0x0404, 0x0804, 0x0c04, 0x1004, 0x1404, 0x0004] {
            assert_eq!(lang_from_langid(id), Lang::ZhTw, "langid={id:#x}");
        }
    }

    #[test]
    fn other_langids_map_to_english() {
        // 0x0409 = 英文，0x0411 = 日文，0x0412 = 韓文，0x0407 = 德文
        for id in [0x0409, 0x0411, 0x0412, 0x0407, 0x0000] {
            assert_eq!(lang_from_langid(id), Lang::En, "langid={id:#x}");
        }
    }

    /// 與前端 `src/i18n/__tests__/locales.test.ts` 的 LOCALE_TAG_CASES 是同一份表，兩邊要一起改。
    const TAG_CASES: &[(&str, &str)] = &[
        ("zh-TW", "zh-TW"),
        ("zh-HK", "zh-TW"),
        ("zh-MO", "zh-TW"),
        ("zh-Hant", "zh-TW"),
        ("zh-Hant-TW", "zh-TW"),
        ("zh-CN", "zh-TW"),
        ("zh-SG", "zh-TW"),
        ("zh-Hans", "zh-TW"),
        ("zh-Hans-CN", "zh-TW"),
        ("zh", "zh-TW"),
        ("zh_TW", "zh-TW"),
        ("ZH-hk", "zh-TW"),
        ("en", "en"),
        ("en-US", "en"),
        ("en-TW", "en"),
        ("ja-JP", "en"),
        ("zht", "en"),
        ("", "en"),
    ];

    #[test]
    fn language_tags_map_like_the_frontend() {
        for (tag, expected) in TAG_CASES {
            assert_eq!(lang_from_language_tag(tag).tag(), *expected, "tag={tag}");
        }
        assert_eq!(lang_from_language_tag("zh_CN.UTF-8"), Lang::ZhTw);
        assert_eq!(lang_from_language_tag("en_US.UTF-8"), Lang::En);
    }

    #[test]
    fn settings_values_migrate_and_auto_follows_the_system() {
        assert_eq!(Lang::from_setting("zh-TW"), Some(Lang::ZhTw));
        assert_eq!(Lang::from_setting("zh-CN"), Some(Lang::ZhTw));
        assert_eq!(Lang::from_setting("en"), Some(Lang::En));
        for value in ["auto", "", "zh", "EN", "zh-HK"] {
            assert_eq!(Lang::from_setting(value), None, "value={value}");
        }
    }
}
