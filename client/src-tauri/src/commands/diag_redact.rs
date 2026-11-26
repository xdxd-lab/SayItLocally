//! [自訂] SC-59（L-97／L-98）：診斷包文字項目在寫入 ZIP 前遮蔽家目錄、使用者名稱與主機名稱。
//! 保留相對路徑尾段與檔名，診斷仍看得出是哪個檔案。截圖等二進位項目不經過這裡。

use regex::{Captures, Regex};
use serde_json::{Map, Value};
use std::sync::LazyLock;

/// 名稱短於這個長度時不做全文替換（避免把一般單字換掉），只在路徑形式中遮蔽。
/// ASCII 名稱至少 3 個字元；含非 ASCII（例如中日韓）的名稱至少 2 個字。
const MIN_FREE_TEXT_NAME: usize = 3;
const MIN_FREE_TEXT_NAME_NON_ASCII: usize = 2;
/// 帳號或主機名稱若是日誌等級或常見資料夾名稱，全文替換會毀掉診斷內容，只在路徑形式中遮蔽。
const COMMON_WORDS: &[&str] = &[
    "info", "debug", "warn", "warning", "error", "trace", "app", "apps", "application", "applications",
    "library", "logs", "log", "documents", "desktop", "downloads", "music", "pictures", "movies", "videos",
    "users", "home", "data", "temp", "tmp", "test", "dev", "server", "client",
];
/// 不當成身分的通用名稱：`whoami` 讀不到時回傳 `unknown`、`localhost`；
/// 換掉它們會讓 `localhost:11434` 或 `[unknown]` 模組名之類的診斷資訊失去意義。
const GENERIC_NAMES: &[&str] = &[
    "unknown", "localhost", "localhost.localdomain", "localdomain", "local", "root", "admin", "user",
    "mac", "macbook", "default", "owner", "administrator", "guest",
];

pub(crate) struct Identity {
    homes: Vec<String>,
    users: Vec<String>,
    hosts: Vec<String>,
}

impl Identity {
    pub(crate) fn new(home: Option<String>, users: Vec<String>, hosts: Vec<String>) -> Self {
        let mut homes = Vec::new();
        if let Some(home) = home.map(|home| home.trim_end_matches(['/', '\\']).to_string()) {
            if home.len() > 1 {
                // Windows 家目錄在 log 裡可能以正斜線或 Debug 跳脫（`\\`）出現。
                homes.push(home.replace('\\', "\\\\"));
                homes.push(home.replace('\\', "/"));
                homes.push(home.clone());
            }
        }
        let mut hosts: Vec<String> = hosts.into_iter()
            .flat_map(|host| {
                let short = host.split('.').next().unwrap_or_default().to_string();
                [host, short]
            })
            .collect();
        let mut users = users;
        for list in [&mut homes, &mut users, &mut hosts] {
            list.retain(|name| !name.trim().is_empty() && !GENERIC_NAMES.contains(&name.to_ascii_lowercase().as_str()));
            list.sort_by_key(|name| std::cmp::Reverse(name.len()));
            list.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        }
        Self { homes, users, hosts }
    }

    pub(crate) fn current() -> Self {
        let home = dirs::home_dir().map(|path| path.to_string_lossy().into_owned());
        let mut users = vec![whoami::username()];
        if let Some(name) = dirs::home_dir().and_then(|path| path.file_name().map(|name| name.to_string_lossy().into_owned())) {
            users.push(name);
        }
        let mut hosts = vec![whoami::devicename()];
        if let Ok(name) = hostname::get() {
            hosts.push(name.to_string_lossy().into_owned());
        }
        Self::new(home, users, hosts)
    }

    pub(crate) fn mask(&self, text: &str) -> String {
        let mut text = text.to_string();
        for home in &self.homes {
            text = replace_bounded(&text, home, "~", false);
        }
        text = mask_path_forms(&text);
        for host in self.hosts.iter().filter(|name| free_text_name(name)) {
            text = replace_bounded(&text, host, "<host>", true);
        }
        for user in self.users.iter().filter(|name| free_text_name(name)) {
            text = replace_bounded(&text, user, "<user>", true);
        }
        text
    }

    /// JSON 先逐一遮蔽字串值與鍵再序列化，避免反斜線跳脫後比對不到。
    pub(crate) fn mask_value(&self, value: &Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.mask(text)),
            Value::Array(items) => Value::Array(items.iter().map(|item| self.mask_value(item)).collect()),
            Value::Object(map) => {
                let mut masked = Map::new();
                for (key, item) in map {
                    // 不同的鍵遮蔽後可能相同，加序號保留兩者，不丟值。
                    let base = self.mask(key);
                    let mut key = base.clone();
                    let mut suffix = 2;
                    while masked.contains_key(&key) {
                        key = format!("{base} ({suffix})");
                        suffix += 1;
                    }
                    masked.insert(key, self.mask_value(item));
                }
                Value::Object(masked)
            }
            other => other.clone(),
        }
    }
}

/// 是否在全文中替換這個名稱（不只路徑形式）。
fn free_text_name(name: &str) -> bool {
    let minimum = if name.is_ascii() { MIN_FREE_TEXT_NAME } else { MIN_FREE_TEXT_NAME_NON_ASCII };
    name.chars().count() >= minimum && !COMMON_WORDS.contains(&name.to_ascii_lowercase().as_str())
}

/// 已產生的佔位符不再被二次替換（例如帳號名稱剛好是 `host`）。
const PLACEHOLDERS: &[&str] = &["<user>", "<host>"];

fn inside_placeholder(text: &str, start: usize, end: usize) -> bool {
    PLACEHOLDERS.iter().any(|placeholder| {
        text.match_indices(placeholder).any(|(at, found)| start < at + found.len() && at < end)
    })
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
}

/// 不分大小寫替換 `needle`。前面不可緊接名稱字元；後面不可緊接名稱字元
/// （`whole_word` 為否時只看後面，家目錄本身以分隔符開頭）。
fn replace_bounded(text: &str, needle: &str, replacement: &str, whole_word: bool) -> String {
    let lower = text.to_ascii_lowercase();
    let pattern = needle.to_ascii_lowercase();
    let bytes = text.as_bytes();
    let mut output = String::with_capacity(text.len());
    let mut copied = 0;
    let mut from = 0;
    while let Some(found) = lower[from..].find(&pattern).map(|offset| from + offset) {
        let end = found + pattern.len();
        let before_ok = !whole_word || found == 0 || !is_name_byte(bytes[found - 1]);
        // 名稱（非家目錄）後接 `.` 也算邊界（`<host>.png`、`<user>.backup`）；
        // 帶 `.local` 的主機名已先以較長者替換。家目錄 `/Users/ab` 不可吃到 `/Users/ab.old`。
        let after_ok = end == bytes.len() || (whole_word && bytes[end] == b'.') || !is_name_byte(bytes[end]);
        if before_ok && after_ok && text.is_char_boundary(found) && text.is_char_boundary(end)
            && !inside_placeholder(text, found, end)
        {
            output.push_str(&text[copied..found]);
            output.push_str(replacement);
            copied = end;
            from = end;
        } else {
            from = found + lower[found..].chars().next().map_or(1, char::len_utf8);
        }
    }
    output.push_str(&text[copied..]);
    output
}

const SHARED_USER_DIRS: &[&str] = &["shared", "public", "default", "all users", "<user>"];

/// 不知道帳號名稱時仍能認出的形式：使用者資料夾、`~name/`、指向家目錄的環境變數、
/// UNC 與 `smb://`、`afp://` 的主機名。
fn mask_path_forms(text: &str) -> String {
    static USER_DIR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)((?:[a-z]:)?[\\/]+(?:users|home)[\\/]+)([^\\/\s"'<>:|*?]+)"#).unwrap()
    });
    static OTHER_HOME: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(^|[\s"'(=:,\[])~[A-Za-z_][A-Za-z0-9._-]*([\\/])"#).unwrap()
    });
    static HOME_VARIABLE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)%(?:userprofile|homepath)%|\$\{home\}|\$home\b").unwrap()
    });
    // 主機名須以英數字開頭：`file:///x`（無主機）與家目錄換成 `~` 後的 `file://~/x` 都不動。
    static NETWORK: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(smb|afp|nfs|cifs|file)://(?:[^@/\s]+@)?([A-Za-z0-9][^/\s]*)").unwrap()
    });
    static UNC: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(^|[\s"'(=:,\[])(\\{2,})([^\\/\s"']+)"#).unwrap()
    });

    let text = USER_DIR.replace_all(text, |caps: &Captures| {
        let name = &caps[2];
        if SHARED_USER_DIRS.contains(&name.to_ascii_lowercase().as_str()) {
            caps[0].to_string()
        } else {
            format!("{}<user>", &caps[1])
        }
    });
    let text = OTHER_HOME.replace_all(&text, "${1}~<user>${2}");
    let text = HOME_VARIABLE.replace_all(&text, "~");
    let text = NETWORK.replace_all(&text, |caps: &Captures| {
        if caps[2].eq_ignore_ascii_case("localhost") {
            caps[0].to_string()
        } else {
            format!("{}://<host>", &caps[1])
        }
    });
    UNC.replace_all(&text, |caps: &Captures| {
        if matches!(caps[3].to_ascii_lowercase().as_str(), "users" | "home" | "<host>" | "?" | ".") {
            caps[0].to_string()
        } else {
            format!("{}{}<host>", &caps[1], &caps[2])
        }
    }).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Identity {
        Identity::new(
            Some("/Users/Zqxtester".into()),
            vec!["zqxtester".into(), "Zqxtester".into()],
            vec!["Zqx-Studio.local".into(), "Zqx Studio".into()],
        )
    }

    #[test]
    fn home_user_and_host_forms_are_masked_but_tails_stay() {
        let id = identity();
        let cases = [
            ("open /Users/Zqxtester/Library/Logs/a.log", "open ~/Library/Logs/a.log"),
            ("open /users/zqxtester/Library", "open ~/Library"),
            ("copy C:\\Users\\zqxtester\\AppData\\x.txt", "copy C:\\Users\\<user>\\AppData\\x.txt"),
            ("debug \"C:\\\\Users\\\\zqxtester\\\\x\"", "debug \"C:\\\\Users\\\\<user>\\\\x\""),
            ("rooted \\Users\\Bob\\x", "rooted \\Users\\<user>\\x"),
            ("linux /home/bob/.config/x", "linux /home/<user>/.config/x"),
            ("other ~bob/notes.txt", "other ~<user>/notes.txt"),
            ("vars %USERPROFILE%\\x $HOME/y ${HOME}/z", "vars ~\\x ~/y ~/z"),
            ("unc \\\\fileserver\\share\\x", "unc \\\\<host>\\share\\x"),
            ("mount smb://zqxtester@nas.lan/share", "mount smb://<host>/share"),
            ("afp://Old-Mac.local/Volume", "afp://<host>/Volume"),
            ("url file:///Users/Zqxtester/x.wav", "url file://~/x.wav"),
            ("host Zqx-Studio.local and zqx-studio up", "host <host> and <host> up"),
            ("named Zqx Studio here", "named <host> here"),
            ("user zqxtester logged", "user <user> logged"),
            ("shared /Users/Shared/x", "shared /Users/Shared/x"),
        ];
        for (input, expected) in cases {
            assert_eq!(id.mask(input), expected, "{input}");
        }
    }

    #[test]
    fn ordinary_text_is_not_masked() {
        let id = Identity::new(Some("/Users/ab".into()), vec!["ab".into()], vec!["mac".into()]);
        for text in [
            "cost $5 and 50% off", "about ~約 3 minutes", "lab and abc", "machine", "$USD price",
            "~ alone", "a/b path", "C:\\Program Files\\x", "\\\\?\\C:\\x",
        ] {
            assert_eq!(id.mask(text), text, "{text}");
        }
        // 短名稱不做全文替換，只在路徑形式中遮蔽；家目錄比對有邊界。
        assert_eq!(id.mask("/Users/ab/x and ab"), "~/x and ab");
        assert_eq!(id.mask("/Users/abc/x"), "/Users/<user>/x");
        assert_eq!(id.mask("/Users/ab.old/x"), "/Users/<user>/x");
        assert_eq!(id.mask("on mac here"), "on mac here");
    }

    #[test]
    fn file_urls_with_a_host_are_masked() {
        let id = identity();
        assert_eq!(id.mask("open file://srv/x"), "open file://<host>/x");
        assert_eq!(id.mask("open FILE://zqxtester@srv.lan/x"), "open FILE://<host>/x");
        for kept in ["open file:///x", "open file://localhost/x"] {
            assert_eq!(id.mask(kept), kept);
        }
    }

    #[test]
    fn placeholders_and_common_words_are_not_replaced_again() {
        let id = Identity::new(None, vec!["host".into(), "Info".into()], vec!["user".into(), "Logs".into()]);
        assert_eq!(id.mask("smb://nas/x"), "smb://<host>/x");
        assert_eq!(id.mask("[INFO] wrote Library/Logs/a.log"), "[INFO] wrote Library/Logs/a.log");
        let id = Identity::new(None, vec!["user".into()], vec!["zqx-mini".into()]);
        assert_eq!(id.mask("zqx-mini"), "<host>");
        let id = Identity::new(None, vec!["zqxuser".into()], vec!["host".into()]);
        assert_eq!(id.mask("zqxuser on smb://nas/x"), "<user> on smb://<host>/x");
        // 常見名稱仍在路徑形式中遮蔽。
        let id = Identity::new(Some("/home/info".into()), vec!["info".into()], vec![]);
        assert_eq!(id.mask("/home/info/x [info]"), "~/x [info]");
    }

    #[test]
    fn non_ascii_names_of_two_characters_are_masked() {
        let id = Identity::new(None, vec!["小明".into(), "王".into()], vec!["測試機".into()]);
        assert_eq!(id.mask("小明在測試機上，王先生"), "<user>在<host>上，王先生");
    }

    #[test]
    fn generic_windows_accounts_are_not_masked() {
        let id = Identity::new(None, vec!["Owner".into(), "Administrator".into(), "Guest".into()], vec![]);
        let text = "Owner Administrator guest";
        assert_eq!(id.mask(text), text);
    }

    #[test]
    fn keys_that_mask_to_the_same_text_keep_both_values() {
        let id = identity();
        let value = serde_json::json!({"zqxtester": 1, "Zqxtester": 2, "<user>": 3});
        let masked = id.mask_value(&value);
        let map = masked.as_object().unwrap();
        assert_eq!(map.len(), 3, "{masked}");
        let mut values: Vec<_> = map.values().map(|v| v.as_i64().unwrap()).collect();
        values.sort();
        assert_eq!(values, [1, 2, 3]);
        assert!(map.keys().all(|key| !key.to_ascii_lowercase().contains("zqx")));
    }

    #[test]
    fn generic_fallback_names_are_not_masked() {
        let id = Identity::new(None, vec!["unknown".into(), "Root".into()], vec!["localhost".into(), "Unknown".into()]);
        let text = "[unknown] ollama at http://localhost:11434 as root";
        assert_eq!(id.mask(text), text);
        let id = Identity::new(None, vec!["zqxtester".into()], vec!["localhost".into(), "zqx-mini.local".into()]);
        assert_eq!(id.mask("localhost zqx-mini zqxtester"), "localhost <host> <user>");
    }

    #[test]
    fn json_keys_and_values_are_masked_before_escaping() {
        let id = identity();
        let value = serde_json::json!({
            "C:\\Users\\zqxtester\\x": ["\\\\Zqx-Studio\\share", {"note": "zqxtester"}],
        });
        let masked = serde_json::to_string(&id.mask_value(&value)).unwrap().to_ascii_lowercase();
        assert!(!masked.contains("zqx"), "{masked}");
    }
}
