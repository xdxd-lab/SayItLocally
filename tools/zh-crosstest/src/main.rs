//! [自訂] SC-67：中文輸出轉換後端的交叉測試（模型 × 轉換後端）。
//!
//! 用法：
//!   cargo run --release --locked -- [raw.tsv]
//!
//! - 不帶參數：只跑內建的文字樣本（台灣用語、一字多義、本來就是正體、日文、英文）。
//! - 帶 `raw.tsv`：另外讀 App 的 `cloud_smoke_zh_matrix_dump` 產生的模型原始辨識文字
//!   （欄位 model、file、elapsed_ms、raw_text），逐一用各後端轉換。模型推論只在雲端跑。
//!
//! 輸出 Markdown 表格：原始文字、轉換後、殘餘簡體字數、與預期是否一致、耗時、授權。
//! 「殘餘簡體」用獨立的簡體專用字表判斷，不用後端自己再轉一次（那樣永遠是 0）。
//! 樣本的簡體字以 Unicode 跳脫保存，原始碼本身不含簡體。

mod simplified;

use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
enum Backend {
    None,
    FerrousOpencc,
    OpenccFmmseg,
    Zhconv,
}

impl Backend {
    const ALL: [Backend; 4] = [Backend::None, Backend::FerrousOpencc, Backend::OpenccFmmseg, Backend::Zhconv];

    fn name(self) -> &'static str {
        match self {
            Backend::None => "none",
            Backend::FerrousOpencc => "ferrous-opencc 0.4.0 (s2twp)",
            Backend::OpenccFmmseg => "opencc-fmmseg 0.13.1 (s2twp)",
            Backend::Zhconv => "zhconv 0.4.2 (zh-TW)",
        }
    }

    fn license(self) -> &'static str {
        match self {
            Backend::None => "-",
            Backend::FerrousOpencc => "Apache-2.0; OpenCC dict Apache-2.0",
            Backend::OpenccFmmseg => "MIT; OpenCC dict Apache-2.0",
            Backend::Zhconv => "GPL-2.0-or-later; MediaWiki + OpenCC data",
        }
    }
}

struct Converters {
    ferrous: ferrous_opencc::OpenCC,
    fmmseg: opencc_fmmseg::OpenCC,
    init: [Duration; 4],
}

impl Converters {
    fn new() -> Self {
        let started = Instant::now();
        let ferrous = ferrous_opencc::OpenCC::from_config(ferrous_opencc::config::BuiltinConfig::S2twp)
            .expect("ferrous-opencc s2twp");
        let ferrous_init = started.elapsed();
        let started = Instant::now();
        let fmmseg = opencc_fmmseg::OpenCC::new();
        let fmmseg_init = started.elapsed();
        let started = Instant::now();
        let _ = zhconv::zhconv("\u{6d4b}", zhconv::Variant::ZhTW);
        let zhconv_init = started.elapsed();
        Converters { ferrous, fmmseg, init: [Duration::ZERO, ferrous_init, fmmseg_init, zhconv_init] }
    }

    /// 與 App 的 `zh_output::to_taiwan` 相同的閘門與「臺 → 台」處理，只有後端不同。
    fn convert(&self, backend: Backend, text: &str) -> String {
        if !should_convert(text) {
            return text.to_string();
        }
        let converted = match backend {
            Backend::None => return text.to_string(),
            Backend::FerrousOpencc => self.ferrous.convert(text),
            Backend::OpenccFmmseg => self.fmmseg.s2twp(text, false),
            Backend::Zhconv => zhconv::zhconv(text, zhconv::Variant::ZhTW),
        };
        converted.replace('\u{81fa}', "\u{53f0}")
    }
}

fn should_convert(text: &str) -> bool {
    let kana = |c: char| {
        matches!(c, '\u{3041}'..='\u{3096}' | '\u{30a1}'..='\u{30fa}' | '\u{31f0}'..='\u{31ff}' | '\u{ff66}'..='\u{ff6f}' | '\u{ff71}'..='\u{ff9d}')
    };
    let han = |c: char| matches!(c, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}');
    text.chars().any(han) && !text.chars().any(kana)
}

fn simplified_count(text: &str) -> usize {
    text.chars().filter(|c| simplified::SIMPLIFIED_ONLY.contains(*c)).count()
}

/// (輸入, 預期輸出)。預期是台灣正體、台灣用語；「台」不寫成「臺」。
const SAMPLES: &[(&str, &str)] = &[
    ("\u{8f6f}\u{4ef6}", "軟體"),
    ("\u{5185}\u{5b58}", "記憶體"),
    ("\u{4fe1}\u{606f}", "資訊"),
    ("\u{89c6}\u{9891}", "影片"),
    ("\u{7f51}\u{7edc}", "網路"),
    ("\u{9ed8}\u{8ba4}\u{8bbe}\u{7f6e}", "預設設定"),
    ("\u{8fd9}\u{4e2a}\u{7a0b}\u{5e8f}\u{7684}\u{754c}\u{9762}\u{5f88}\u{7b80}\u{6d01}", "這個程式的介面很簡潔"),
    ("\u{540e}\u{5929}\u{53bb}\u{7687}\u{540e}\u{5927}\u{9053}", "後天去皇后大道"),
    ("\u{5934}\u{53d1}\u{548c}\u{53d1}\u{5c55}", "頭髮和發展"),
    ("\u{5e72}\u{51c0}\u{7684}\u{5e72}\u{90e8}", "乾淨的幹部"),
    ("\u{53ea}\u{6709}\u{4e00}\u{53ea}\u{732b}", "只有一隻貓"),
    ("\u{91cc}\u{9762}\u{8d70}\u{4e86}\u{4e09}\u{516c}\u{91cc}", "裡面走了三公里"),
    ("\u{8bed}\u{97f3}\u{8f93}\u{5165}\u{6cd5}\u{6d4b}\u{8bd5}\u{6210}\u{529f}\u{3002}", "語音輸入法測試成功。"),
    ("\u{53f0}\u{5317}\u{7684}\u{5929}\u{6c14}\u{ff0c}\u{4e09}\u{53f0}\u{673a}\u{5668}", "台北的天氣，三台機器"),
    ("\u{53f0}\u{98ce}", "颱風"),
    // 本來就是正體：必須一字不變
    ("台灣", "台灣"),
    ("裡面", "裡面"),
    ("看著", "看著"),
    ("這是正體中文，著實不錯。", "這是正體中文，著實不錯。"),
    // 日文（含假名）與英文：不轉
    (
        "\u{4eca}\u{65e5}\u{306f}\u{6771}\u{4eac}\u{3067}\u{4f1a}\u{8b70}\u{304c}\u{3042}\u{308a}\u{307e}\u{3059}\u{3002}",
        "\u{4eca}\u{65e5}\u{306f}\u{6771}\u{4eac}\u{3067}\u{4f1a}\u{8b70}\u{304c}\u{3042}\u{308a}\u{307e}\u{3059}\u{3002}",
    ),
    ("Rust fn main() {}", "Rust fn main() {}"),
];

fn cell(text: &str) -> String {
    text.replace('|', "\\|")
}

fn main() {
    let converters = Converters::new();
    println!("## Backends\n");
    println!("| backend | license | cold init |");
    println!("|---|---|---|");
    for (index, backend) in Backend::ALL.iter().enumerate() {
        println!("| {} | {} | {:.2?} |", backend.name(), backend.license(), converters.init[index]);
    }

    println!("\n## Text samples\n");
    println!("| backend | input | output | expected | simplified left | time |");
    println!("|---|---|---|---|---|---|");
    let mut mismatches = [0usize; 4];
    for (input, expected) in SAMPLES {
        for (index, backend) in Backend::ALL.iter().enumerate() {
            let started = Instant::now();
            let output = converters.convert(*backend, input);
            let elapsed = started.elapsed();
            let ok = output == *expected;
            if !ok {
                mismatches[index] += 1;
            }
            println!(
                "| {} | {} | {} | {} | {} | {:.1?} |",
                backend.name(),
                cell(input),
                cell(&output),
                if ok { "ok" } else { "DIFF" },
                simplified_count(&output),
                elapsed,
            );
        }
    }
    println!("\n| backend | samples not matching expected (of {}) |", SAMPLES.len());
    println!("|---|---|");
    for (index, backend) in Backend::ALL.iter().enumerate() {
        println!("| {} | {} |", backend.name(), mismatches[index]);
    }

    let Some(path) = std::env::args().nth(1) else {
        return;
    };
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
    println!("\n## Models x backends\n");
    println!("| model | file | asr ms | backend | output | simplified left | convert time |");
    println!("|---|---|---|---|---|---|---|");
    for line in raw.lines().skip(1) {
        let fields: Vec<&str> = line.splitn(4, '\t').collect();
        let [model, file, asr_ms, text] = fields[..] else {
            continue;
        };
        for backend in Backend::ALL {
            let started = Instant::now();
            let output = converters.convert(backend, text);
            let elapsed = started.elapsed();
            println!(
                "| {model} | {file} | {asr_ms} | {} | {} | {} | {:.1?} |",
                backend.name(),
                cell(&output),
                simplified_count(&output),
                elapsed,
            );
        }
    }
}
