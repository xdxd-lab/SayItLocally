//! 檔案類 IPC 命令的路徑校驗。
//!
//! 這些命令（讀刪錄音、診斷包、匯出、存到下載目錄）的路徑都來自前端。前端是自己寫的，
//! 但它跑在 webview 裡：哪天混進一段惡意指令碼（依賴被投毒、渲染了不該渲染的內容），
//! 它就能借這些命令讀寫刪本機任意檔案。所以 Rust 側按"這個命令本來只該碰哪裡"收口，
//! 不信任前端給的路徑。

use std::path::{Path, PathBuf};

/// 只由字母數字、`.`、`_`、`-` 組成的單段檔名，且以指定副檔名結尾。
/// 不允許路徑分隔符、`..`、開頭的點、Windows 保留字元。
pub fn is_plain_file_name(name: &str, extension: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && !name.contains("..")
        && lower.ends_with(&format!(".{}", extension.to_ascii_lowercase()))
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// 錄音 id：前端用 `Date.now().toString(36) + 隨機 4 位` 生成，只會是字母數字。
/// 放寬到 `_`、`-` 是給以後換 uuid 留餘地。
pub fn is_record_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

pub fn has_extension(path: &Path, allowed: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| allowed.iter().any(|a| e.eq_ignore_ascii_case(a)))
}

/// 已存在的檔案是否直接位於 dir 裡（不含子目錄）。兩邊都 canonicalize，
/// 所以 `..`、符號連結、大小寫不同的寫法都會落到同一個真實路徑上再比。
pub fn existing_file_in_dir(path: &Path, dir: &Path) -> Option<PathBuf> {
    let file = std::fs::canonicalize(path).ok()?;
    let dir = std::fs::canonicalize(dir).ok()?;
    (file.is_file() && file.parent() == Some(dir.as_path())).then_some(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_file_names() {
        assert!(is_plain_file_name("sayit_20261012_101010.wav", "wav"));
        assert!(is_plain_file_name("mshhzwhhyxxw.WAV", "wav"));
        assert!(!is_plain_file_name("evil.exe", "wav"));
        assert!(!is_plain_file_name("a.wav.exe", "wav"));
        assert!(!is_plain_file_name("..\\..\\Startup\\x.wav", "wav"));
        assert!(!is_plain_file_name("../x.wav", "wav"));
        assert!(!is_plain_file_name("C:x.wav", "wav"));
        assert!(!is_plain_file_name(".wav", "wav"));
        assert!(!is_plain_file_name("a b.wav", "wav"));
    }

    #[test]
    fn record_ids() {
        assert!(is_record_id("mshhzwhhyxxw"));
        assert!(is_record_id("1b9d6bcd-bbfd-4b2d"));
        assert!(!is_record_id(""));
        assert!(!is_record_id("..\\x"));
        assert!(!is_record_id("a/b"));
        assert!(!is_record_id("a.b"));
    }

    #[test]
    fn files_outside_the_directory_are_rejected() {
        let root = std::env::temp_dir().join(format!("sayit-path-guard-{}", uuid::Uuid::new_v4()));
        let inner = root.join("audio");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("a.wav"), b"x").unwrap();
        std::fs::write(root.join("b.wav"), b"x").unwrap();

        assert!(existing_file_in_dir(&inner.join("a.wav"), &inner).is_some());
        assert!(existing_file_in_dir(&inner.join("..").join("b.wav"), &inner).is_none());
        assert!(existing_file_in_dir(&root.join("b.wav"), &inner).is_none());
        assert!(existing_file_in_dir(&inner.join("missing.wav"), &inner).is_none());
        assert!(existing_file_in_dir(&inner, &inner).is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn extensions() {
        assert!(has_extension(Path::new("x.ZIP"), &["zip"]));
        assert!(!has_extension(Path::new("x.zip.exe"), &["zip"]));
        assert!(!has_extension(Path::new("x"), &["zip"]));
    }
}
