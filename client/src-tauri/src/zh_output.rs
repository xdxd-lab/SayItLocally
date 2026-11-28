//! [自訂] SC-67：辨識與 AI 校對輸出一律轉成正體中文（台灣）。
//!
//! 這是文字離開 Rust 的單一出口：本機辨識（`models::local_asr::assemble_transcript`）、
//! 設定頁的辨識測試（`models::test_audio::benchmark_result`）與 AI 校對
//! （`providers::registry::finalize_polish`）回傳前都經過 [`to_taiwan`]。前端的浮窗、
//! 歷史（含 ASR 原文）、剪貼簿、後備卡片與匯出拿到的都已是正體。使用者取代規則的
//! 「替換成」欄位在新增與匯入時經 [`zh_to_taiwan_texts`] 轉換（前端在 Rust 出口之後才套用規則，
//! 所以不對最終文字再轉一次，以免誤改正體與使用者寫的內容）。
//!
//! - 後端：ferrous-opencc（Apache-2.0，純 Rust，字典來自 OpenCC，編譯時內嵌，執行時不讀檔、
//!   不連網）的 `s2twp`（簡體 → 台灣正體，含台灣慣用詞，例如簡體的「軟件」寫法轉成「軟體」）。
//! - 逐句判斷：依「。！？.!?；;…」與換行切句，含假名的句子視為日文不轉；其餘句子連成一段一起轉，
//!   所以不含假名的文字與整段轉換逐位元相同。介面指定日文時整段不轉；英文、韓文照樣轉
//!   （說中文時模型仍可能輸出漢字，對英文與韓文跑轉換不會改變任何字）。
//! - OpenCC 的台灣標準會把「台」寫成「臺」；本專案一律用「台」（台灣、台北），轉換後統一改回。
//! - 產品路徑只有 ferrous-opencc，使用者介面不提供關閉。其他後端的比較在
//!   `tools/zh-crosstest`（獨立 crate，不進 App）。

use std::sync::OnceLock;

/// 轉換後端。產品只用 [`ZhBackend::FerrousOpencc`]；`Passthrough`（不轉換）只供測試對照。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZhBackend {
    FerrousOpencc,
    #[cfg(test)]
    Passthrough,
}

/// 產品使用的後端。
pub const PRODUCT_BACKEND: ZhBackend = ZhBackend::FerrousOpencc;

fn ferrous() -> Option<&'static ferrous_opencc::OpenCC> {
    static CONVERTER: OnceLock<Option<ferrous_opencc::OpenCC>> = OnceLock::new();
    CONVERTER
        .get_or_init(|| {
            match ferrous_opencc::OpenCC::from_config(ferrous_opencc::config::BuiltinConfig::S2twp) {
                Ok(converter) => Some(converter),
                Err(error) => {
                    // 字典是編譯時內嵌的，這裡失敗代表建置有問題；記下來，文字原樣送出。
                    log::error!("[zh-output] OpenCC s2twp init failed: {error}");
                    None
                }
            }
        })
        .as_ref()
}

/// 介面是否指定日文（`ja`、`ja-JP`、`ja_JP`）。只有日文整段不轉。
fn language_is_japanese(language: &str) -> bool {
    let tag = language.trim().to_ascii_lowercase();
    tag == "ja" || tag.starts_with("ja-") || tag.starts_with("ja_")
}

fn is_kana(c: char) -> bool {
    // 只認假名字母：平假名、片假名（含擴充與半形片假名）。不含中文也會用到的
    // 「・」(U+30FB)、「ー」(U+30FC) 與濁點等符號，否則帶間隔號的外國人名會被當成日文。
    matches!(c, '\u{3041}'..='\u{3096}' | '\u{30a1}'..='\u{30fa}' | '\u{31f0}'..='\u{31ff}' | '\u{ff66}'..='\u{ff6f}' | '\u{ff71}'..='\u{ff9d}')
}

fn is_han(c: char) -> bool {
    matches!(c, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}')
}

/// 句末符號：切句後逐句判斷假名。
fn is_sentence_end(c: char) -> bool {
    matches!(c, '。' | '！' | '？' | '.' | '!' | '?' | '；' | ';' | '…' | '\n')
}

/// 依句末符號切句，符號留在句尾；切出的各句依序接起來等於原文。
fn sentences(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let end = rest
            .char_indices()
            .find(|&(_, c)| is_sentence_end(c))
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(rest.len());
        let (head, tail) = rest.split_at(end);
        rest = tail;
        Some(head)
    })
}

/// 單一句子是否需要轉換：含漢字且不含假名（含假名視為日文）。
fn sentence_needs_conversion(sentence: &str) -> bool {
    sentence.chars().any(is_han) && !sentence.chars().any(is_kana)
}

/// 「臺」統一寫成「台」（台灣、台北、三台機器）。颱風、檯燈等不同字不受影響。
fn prefer_tai(text: String) -> String {
    if text.contains('\u{81fa}') {
        text.replace('\u{81fa}', "\u{53f0}")
    } else {
        text
    }
}

/// 用指定後端轉換，不做語種判斷。
pub fn convert_with(backend: ZhBackend, text: &str) -> String {
    match backend {
        ZhBackend::FerrousOpencc => match ferrous() {
            Some(converter) => prefer_tai(converter.convert(text)),
            None => text.to_string(),
        },
        #[cfg(test)]
        ZhBackend::Passthrough => text.to_string(),
    }
}

/// 產品出口：逐句判斷，需要時轉成台灣正體，含假名的句子與介面指定日文時原樣回傳。
/// 連續不含假名的句子合成一段一起轉，詞組比對不會被切句打斷。
pub fn to_taiwan(text: &str, language: &str) -> String {
    if language_is_japanese(language) || !text.chars().any(is_han) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |out: &mut String, run: &mut String| {
        if sentence_needs_conversion(run) {
            out.push_str(&convert_with(PRODUCT_BACKEND, run));
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for sentence in sentences(text) {
        if sentence.chars().any(is_kana) {
            flush(&mut out, &mut run);
            out.push_str(sentence);
        } else {
            run.push_str(sentence);
        }
    }
    flush(&mut out, &mut run);
    out
}

/// [自訂] SC-67：取代規則的「替換成」欄位在新增與匯入時轉成台灣正體（規則與辨識輸出相同，
/// 語種視為自動偵測）。比對欄不經這裡。
#[tauri::command]
pub fn zh_to_taiwan_texts(texts: Vec<String>) -> Vec<String> {
    texts.iter().map(|text| to_taiwan(text, "auto")).collect()
}

/// 獨立判準：簡體專用字（414 字，以 Unicode 跳脫保存，原始碼本身不含簡體）。
/// 與轉換器無關，用來檢查辨識輸出有沒有殘留簡體；「后、里、干」這類正體也用的字不在表內。
#[cfg(test)]
pub const SIMPLIFIED_ONLY: &str = "\
    \u{4e0e}\u{4e13}\u{4e1a}\u{4e1c}\u{4e1d}\u{4e24}\u{4e25}\u{4e2a}\u{4e30}\u{4e34}\u{4e3a}\u{4e4c}\
    \u{4e50}\u{4e60}\u{4e61}\u{4e66}\u{4e70}\u{4e71}\u{4e89}\u{4e91}\u{4e9a}\u{4ea7}\u{4ec5}\u{4ece}\
    \u{4eec}\u{4ef7}\u{4f17}\u{4f18}\u{4f1a}\u{4f1e}\u{4f1f}\u{4f20}\u{4f53}\u{4f63}\u{4fa0}\u{4fa6}\
    \u{4fed}\u{503a}\u{503e}\u{507f}\u{50a8}\u{513f}\u{515a}\u{5170}\u{5173}\u{5174}\u{517b}\u{5199}\
    \u{519c}\u{51b2}\u{51b3}\u{51bb}\u{51c0}\u{51e4}\u{51ed}\u{51fb}\u{5218}\u{521b}\u{5220}\u{522b}\
    \u{5242}\u{529d}\u{529e}\u{52a1}\u{52a8}\u{52b1}\u{52bf}\u{52cb}\u{534e}\u{534f}\u{5355}\u{5356}\
    \u{536b}\u{5382}\u{5385}\u{5386}\u{538b}\u{538c}\u{53bf}\u{53c2}\u{53cc}\u{53d8}\u{53e0}\u{53f6}\
    \u{53f7}\u{53f9}\u{5413}\u{5417}\u{542f}\u{5434}\u{5458}\u{548f}\u{54cd}\u{54d7}\u{54df}\u{5524}\
    \u{5578}\u{56e2}\u{5706}\u{5723}\u{573a}\u{574f}\u{5757}\u{575a}\u{575b}\u{5784}\u{5899}\u{58ee}\
    \u{58f0}\u{58f3}\u{5904}\u{5907}\u{590d}\u{5938}\u{593a}\u{594b}\u{5956}\u{5986}\u{5987}\u{5988}\
    \u{59dc}\u{5a31}\u{5a74}\u{5b59}\u{5b66}\u{5b9d}\u{5b9e}\u{5baa}\u{5bbe}\u{5bdd}\u{5bf9}\u{5bfb}\
    \u{5bfc}\u{5c06}\u{5c18}\u{5c42}\u{5c81}\u{5c97}\u{5c9b}\u{5ce1}\u{5e01}\u{5e05}\u{5e08}\u{5e10}\
    \u{5e26}\u{5e86}\u{5e93}\u{5e94}\u{5e9f}\u{5f00}\u{5f20}\u{5f39}\u{5f3a}\u{5f52}\u{5f55}\u{5f7b}\
    \u{5fc6}\u{6000}\u{6001}\u{603b}\u{6076}\u{60ac}\u{60ca}\u{60ef}\u{620f}\u{6218}\u{621a}\u{6267}\
    \u{6269}\u{626b}\u{626c}\u{62e9}\u{6302}\u{6324}\u{6325}\u{635f}\u{6361}\u{6362}\u{636e}\u{6401}\
    \u{6444}\u{6446}\u{6447}\u{6491}\u{654c}\u{655b}\u{6570}\u{658b}\u{65ad}\u{65e0}\u{65f6}\u{663e}\
    \u{664b}\u{6652}\u{6655}\u{6682}\u{672f}\u{673a}\u{6740}\u{6742}\u{6743}\u{6761}\u{6765}\u{6784}\
    \u{67aa}\u{67dc}\u{6807}\u{680f}\u{6811}\u{6837}\u{6863}\u{68a6}\u{68c0}\u{6b22}\u{6b27}\u{6bd5}\
    \u{6c47}\u{6c49}\u{6c61}\u{6c9f}\u{6ca1}\u{6ca6}\u{6d45}\u{6d46}\u{6d4b}\u{6d4e}\u{6d4f}\u{6da6}\
    \u{6e0a}\u{6e7e}\u{6e83}\u{6eda}\u{6ede}\u{6ee1}\u{6f13}\u{6f5c}\u{706d}\u{706f}\u{70e6}\u{70e7}\
    \u{70ed}\u{7231}\u{7237}\u{72ec}\u{72ee}\u{730e}\u{732e}\u{739b}\u{73af}\u{73b0}\u{7410}\u{743c}\
    \u{76d1}\u{76d6}\u{76d8}\u{77ff}\u{7801}\u{786e}\u{788d}\u{793c}\u{79ef}\u{79f0}\u{7a83}\u{7ade}\
    \u{7b0b}\u{7b14}\u{7b3c}\u{7c7b}\u{7d27}\u{7eaa}\u{7ebf}\u{7ec4}\u{7ec7}\u{7ec8}\u{7ecd}\u{7ecf}\
    \u{7ed3}\u{7ed8}\u{7ed9}\u{7edc}\u{7edd}\u{7edf}\u{7ee7}\u{7ef4}\u{7efc}\u{7f16}\u{7f18}\u{7f29}\
    \u{7f51}\u{7f57}\u{7f5a}\u{8054}\u{8111}\u{817e}\u{8230}\u{8231}\u{827a}\u{8282}\u{84dd}\u{8681}\
    \u{8865}\u{88c5}\u{89c1}\u{89c2}\u{89c4}\u{89c6}\u{89c8}\u{89c9}\u{8ba4}\u{8bb0}\u{8bb2}\u{8bb8}\
    \u{8bba}\u{8bbe}\u{8bbf}\u{8bc1}\u{8bc4}\u{8bc9}\u{8bcd}\u{8bd1}\u{8bd5}\u{8bd7}\u{8bed}\u{8bef}\
    \u{8bf4}\u{8bf7}\u{8bfb}\u{8c01}\u{8c03}\u{8c08}\u{8d1d}\u{8d1f}\u{8d21}\u{8d22}\u{8d23}\u{8d28}\
    \u{8d29}\u{8d2d}\u{8d2f}\u{8d35}\u{8d37}\u{8d39}\u{8d44}\u{8d5e}\u{8d62}\u{8d76}\u{8dc3}\u{8df5}\
    \u{8f66}\u{8f68}\u{8f6e}\u{8f6f}\u{8f7d}\u{8f88}\u{8f93}\u{8fc7}\u{8fd8}\u{8fd9}\u{8fdb}\u{8fde}\
    \u{8fdf}\u{9009}\u{9012}\u{903b}\u{9057}\u{90ae}\u{90d1}\u{91ca}\u{9489}\u{949f}\u{94a2}\u{94a5}\
    \u{94b1}\u{94c1}\u{94ed}\u{94f6}\u{9500}\u{9501}\u{9510}\u{9519}\u{952e}\u{957f}\u{95e8}\u{95ea}\
    \u{95ed}\u{95ee}\u{95f2}\u{95f4}\u{9605}\u{961f}\u{9633}\u{9634}\u{9635}\u{9636}\u{9645}\u{9646}\
    \u{9648}\u{96fe}\u{9875}\u{9879}\u{9884}\u{9897}\u{9898}\u{989c}\u{989d}\u{98ce}\u{98de}\u{996d}\
    \u{9986}\u{9a8c}\u{9c7c}\u{9e1f}\u{9f50}\u{9f99}";

/// 文字裡簡體專用字的個數（只供測試與交叉測試）。
#[cfg(test)]
pub fn count_simplified_only(text: &str) -> usize {
    text.chars().filter(|c| SIMPLIFIED_ONLY.contains(*c)).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 簡體輸入以 Unicode 跳脫保存，讓原始碼本身不含簡體字。
    const CASES: &[(&str, &str)] = &[
        // → 軟體
        ("\u{8f6f}\u{4ef6}", "軟體"),
        // → 記憶體
        ("\u{5185}\u{5b58}", "記憶體"),
        // → 資訊
        ("\u{4fe1}\u{606f}", "資訊"),
        // → 影片
        ("\u{89c6}\u{9891}", "影片"),
        // → 預設設定
        ("\u{9ed8}\u{8ba4}\u{8bbe}\u{7f6e}", "預設設定"),
        // → 後天去皇后大道
        ("\u{540e}\u{5929}\u{53bb}\u{7687}\u{540e}\u{5927}\u{9053}", "後天去皇后大道"),
        // → 頭髮和發展
        ("\u{5934}\u{53d1}\u{548c}\u{53d1}\u{5c55}", "頭髮和發展"),
        // → 乾淨的幹部
        ("\u{5e72}\u{51c0}\u{7684}\u{5e72}\u{90e8}", "乾淨的幹部"),
        // → 只有一隻貓
        ("\u{53ea}\u{6709}\u{4e00}\u{53ea}\u{732b}", "只有一隻貓"),
        // → 裡面走了三公里
        ("\u{91cc}\u{9762}\u{8d70}\u{4e86}\u{4e09}\u{516c}\u{91cc}", "裡面走了三公里"),
        // → 語音輸入法測試成功。
        ("\u{8bed}\u{97f3}\u{8f93}\u{5165}\u{6cd5}\u{6d4b}\u{8bd5}\u{6210}\u{529f}\u{3002}", "語音輸入法測試成功。"),
        // → 台北的天氣，三台機器（「台」不改成「臺」）
        ("\u{53f0}\u{5317}\u{7684}\u{5929}\u{6c14}\u{ff0c}\u{4e09}\u{53f0}\u{673a}\u{5668}", "台北的天氣，三台機器"),
        // → 颱風
        ("\u{53f0}\u{98ce}", "颱風"),
    ];

    /// 已經是正體的文字（例如 Breeze 的輸出）轉完必須一字不變。
    const ALREADY_TRADITIONAL: &[&str] = &[
        "台灣", "台北", "裡面", "著作", "看著", "這是正體中文，著實不錯。", "軟體與記憶體", "颱風",
        "語音輸入法測試成功。", "Rust 程式碼 fn main() {}",
    ];

    #[test]
    fn simplified_becomes_taiwan_traditional() {
        for (input, expected) in CASES {
            assert_eq!(to_taiwan(input, "auto"), *expected, "{input}");
            assert_eq!(to_taiwan(input, "zh"), *expected, "{input}");
        }
    }

    #[test]
    fn traditional_text_is_unchanged() {
        for text in ALREADY_TRADITIONAL {
            assert_eq!(to_taiwan(text, "auto"), *text);
        }
        // 「臺」統一寫成「台」。
        assert_eq!(to_taiwan("臺灣", "zh"), "台灣");
    }

    /// 日文例句（含假名），其中的漢字是日文寫法，不可改成正體。
    const JAPANESE: &str = "\u{4eca}\u{65e5}\u{306f}\u{6771}\u{4eac}\u{3067}\u{4f1a}\u{8b70}\u{304c}\u{3042}\u{308a}\u{307e}\u{3059}\u{3002}";

    #[test]
    fn japanese_is_left_alone_and_other_languages_convert() {
        assert_eq!(to_taiwan(JAPANESE, "auto"), JAPANESE);
        assert_eq!(to_taiwan(JAPANESE, "zh"), JAPANESE, "kana always means Japanese");
        // 「會議」的簡體寫法：只有介面指定日文時不轉；英文、韓文、粵語、中文都轉。
        let meeting = "\u{4f1a}\u{8bae}";
        for language in ["ja", "JA", "ja-JP", "ja_JP"] {
            assert_eq!(to_taiwan(meeting, language), meeting, "{language}");
        }
        for language in ["auto", "", "zh", "zh-TW", "yue", "en", "ko", "cmn"] {
            assert_eq!(to_taiwan(meeting, language), "會議", "{language}");
        }
        assert_eq!(to_taiwan("plain English", "en"), "plain English");
        assert_eq!(to_taiwan("\u{d55c}\u{ad6d}\u{c5b4}", "ko"), "\u{d55c}\u{ad6d}\u{c5b4}");
    }

    #[test]
    fn only_sentences_with_kana_are_left_alone() {
        // 「中文簡體句。」的簡體寫法 + 日文句：只轉第一句。
        let chinese = "\u{4e2d}\u{6587}\u{7b80}\u{4f53}\u{53e5}\u{3002}";
        let mixed = format!("{chinese}{JAPANESE}");
        assert_eq!(to_taiwan(&mixed, "auto"), format!("中文簡體句。{JAPANESE}"));
        // 日文句在中間、各種句末符號與換行都能切開；沒有句末符號的結尾也照轉。
        // 「軟體！」…「測試\n」…「影片」的簡體寫法
        let text = format!("\u{8f6f}\u{4ef6}\u{ff01}{JAPANESE}\u{6d4b}\u{8bd5}\n\u{4eca}\u{65e5}\u{306f}?\u{89c6}\u{9891}");
        assert_eq!(to_taiwan(&text, "zh"), format!("軟體！{JAPANESE}測試\n\u{4eca}\u{65e5}\u{306f}?影片"));
        // 介面指定日文：整段不轉。
        assert_eq!(to_taiwan(&mixed, "ja"), mixed);
    }

    #[test]
    fn sentence_split_matches_whole_text_conversion_without_kana() {
        let samples: Vec<String> = CASES
            .windows(3)
            .map(|w| format!("{}。{}！{}\n{}?.", w[0].0, w[1].0, w[2].0, w[0].0))
            .chain(ALREADY_TRADITIONAL.iter().map(|t| format!("{t}。{t}；{t}…")))
            .collect();
        for text in &samples {
            assert_eq!(
                to_taiwan(text, "auto").as_bytes(),
                convert_with(PRODUCT_BACKEND, text).as_bytes(),
                "{text}"
            );
        }
        // 切出的句子依序接回等於原文。
        let joined: String = sentences(&samples[0]).collect();
        assert_eq!(joined, samples[0]);
        assert!(sentences(&samples[0]).count() >= 5);
    }

    #[test]
    fn replacement_texts_command_converts_each_text() {
        assert_eq!(
            zh_to_taiwan_texts(vec!["\u{8f6f}\u{4ef6}".into(), "test@example.com".into(), JAPANESE.into(), String::new()]),
            vec!["軟體".to_string(), "test@example.com".into(), JAPANESE.into(), String::new()]
        );
    }

    #[test]
    fn interpunct_and_long_mark_do_not_count_as_kana() {
        // 外國人名的間隔號「・」與長音符號「ー」中文也會用到，不能讓整句被當成日文而不轉。
        // 簡體輸入 → 約翰・史密斯說
        let name = "\u{7ea6}\u{7ff0}\u{30fb}\u{53f2}\u{5bc6}\u{65af}\u{8bf4}";
        assert_eq!(to_taiwan(name, "auto"), "約翰・史密斯說");
        // 簡體輸入 → 卡ー車：含長音符號但沒有假名字母，照樣轉換
        let long_mark = "\u{5361}\u{30fc}\u{8f66}";
        assert_eq!(to_taiwan(long_mark, "auto"), "卡ー車");
        // 真正的假名字母仍然擋下：カ（片假名）
        let katakana = "\u{5361}\u{30ab}\u{8f66}";
        assert!(!sentence_needs_conversion(katakana));
        assert_eq!(to_taiwan(katakana, "auto"), katakana);
    }

    #[test]
    fn product_backend_converts_and_passthrough_does_not() {
        assert_eq!(PRODUCT_BACKEND, ZhBackend::FerrousOpencc);
        let simplified = "\u{8f6f}\u{4ef6}";
        assert_eq!(convert_with(ZhBackend::Passthrough, simplified), simplified);
        assert_ne!(convert_with(PRODUCT_BACKEND, simplified), simplified);
    }

    #[test]
    fn converted_samples_have_no_simplified_only_characters() {
        assert!(SIMPLIFIED_ONLY.chars().count() > 400);
        let mut inputs_with_hits = 0;
        for (input, _) in CASES {
            if count_simplified_only(input) > 0 {
                inputs_with_hits += 1;
            }
            assert_eq!(count_simplified_only(&to_taiwan(input, "auto")), 0, "{input}");
        }
        // 字表抓不到「后、里、干、只」這類正體也用的字與「內」的簡體寫法（見模組說明），只要求至少一半的樣本被抓到。
        assert!(inputs_with_hits * 2 >= CASES.len(), "{inputs_with_hits}");
    }

    /// 三個出口的轉換在各自的純函式裡（`assemble_transcript`、`benchmark_result`、`finalize_polish`），
    /// 由各模組的單元測試直接驗證輸出。出口本身在需要模型或 Ollama 的路徑上跑不到，這裡檢查出口
    /// 回傳的就是純函式的結果（去掉 `//` 註解後），「呼叫了但沒用結果」或繞過純函式都會紅燈。
    #[test]
    fn all_output_exits_return_the_converted_text() {
        fn code_only(source: &str) -> String {
            source
                .lines()
                .map(|line| line.split("//").next().unwrap_or(""))
                .collect::<Vec<_>>()
                .join("\n")
        }
        for (file, source, start, end, must) in [
            (
                "local_asr.rs",
                include_str!("models/local_asr.rs"),
                "fn transcribe_chunks(",
                "fn assemble_transcript(",
                &["Ok(assemble_transcript(&parts, lang))"][..],
            ),
            (
                "local_asr.rs",
                include_str!("models/local_asr.rs"),
                "async fn transcribe_samples(",
                "fn transcribe_chunks(",
                &["let text = transcribe_chunks(", "Ok(LocalAsrResult { text, elapsed_ms })"][..],
            ),
            (
                "test_audio.rs",
                include_str!("models/test_audio.rs"),
                "let text = super::gguf_asr::transcribe(",
                "fn benchmark_result(",
                &["Ok(benchmark_result(&text, &lang, "][..],
            ),
            (
                "registry.rs",
                include_str!("providers/registry.rs"),
                "pub async fn cloud_polish(",
                "fn finalize_polish(",
                &[".map(finalize_polish)"][..],
            ),
        ] {
            let code = code_only(source);
            let from = code.find(start).unwrap_or_else(|| panic!("{file}: {start} not found"));
            let to = code[from..].find(end).map(|offset| from + offset).unwrap_or_else(|| panic!("{file}: {end} not found"));
            for needle in must {
                assert!(
                    code[from..to].contains(needle),
                    "{file}: the output exit no longer returns the Taiwan conversion ({needle})"
                );
            }
        }
    }
}
