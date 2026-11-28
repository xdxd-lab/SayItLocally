// Model catalog

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadSource {
    pub source: String,
    pub files: Vec<ModelFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelFile {
    pub name: String,
    pub url: String,
    pub size_bytes: u64,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    /// 展示名，不帶 GGUF/量化檔等術語（那些進 `quant`，前端排進小字引數行）
    pub name: String,
    /// 一句話定位（"速度極快，日常首選"），不再混記憶體/語種——那些有各自的欄位
    pub description: String,
    pub model_type: String,
    pub total_size_bytes: u64,
    pub languages: Vec<String>,
    pub sources: Vec<DownloadSource>,
    /// 速度評級 0–10（可帶小數，10 最快）
    #[serde(default)]
    pub speed: f32,
    /// 準確度評級 0–10（可帶小數，10 最準）
    #[serde(default)]
    pub accuracy: f32,
    /// 是否為推薦（預設）模型
    #[serde(default)]
    pub recommended: bool,
    /// 載入後的常駐記憶體（MB，**實測工作集**，不是權重體積推算——權重之外還有
    /// KV cache、計算緩衝，Qwen3 這類自迴歸模型多出 700 MB ~ 1 GB。實測方法見
    /// gguf_asr.rs 的 `footprint_report`）。
    #[serde(default)]
    pub memory_mb: u64,
    /// 語種的展示文案（"中、英、日、韓、粵語" / "30+ 語種"）。`languages` 留給邏輯用，
    /// 展示不從 code 拼：Qwen3 支援 30 種，逐個列 code 沒有意義。
    #[serde(default)]
    pub languages_label: String,
    /// 量化檔（"Q8_0" / "Q4_K_M"…）。列表 UI 不再展示（普通使用者看不懂），
    /// 留作資料：離線下載指引的檔名裡有它，將來放 Tooltip 也用得上。
    #[serde(default)]
    pub quant: String,
    /// 是否直接展示在模型列表里。恰好三个：小（最快）/ 中（均衡）/ 大（最准），
    /// 其餘摺疊進「更多」——五個平鋪使用者反而不知道選哪個。
    #[serde(default)]
    pub featured: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalModelInfo {
    pub id: String,
    pub name: String,
    pub model_type: String,
    pub total_size_bytes: u64,
    pub path: String,
    pub complete: bool,
}

/// [自訂] SC-68：`revision` 為 "main" 或固定的 commit（40 位十六進位）。
fn hf(repo: &str, revision: &str, file: &str) -> String {
    format!("https://huggingface.co/{}/resolve/{}/{}", repo, revision, file)
}

// [自訂] S56：移除 hf-mirror.com 鏡像來源，只從官方 huggingface.co 下載。原：
// fn hf_mirror(repo: &str, file: &str) -> String {
//     format!("https://hf-mirror.com/{}/resolve/main/{}", repo, file)
// }

/// 一份 GGUF 權重的座標（倉庫 / 檔名 / 位元組數 / sha256）。
///
/// 抽成一個結構體是為了讓體積和校驗和**只寫一遍**：每個模型要在映象源和官方源
/// 各出現一次、`total_size_bytes` 還要再寫一次。手寫三遍的話，換量化檔時漏改
/// 其中一處會變成"只有走某個源的使用者下載失敗"——這種只發生在一半使用者身上的
/// 問題最難被發現。
struct GgufWeight {
    repo: &'static str,
    /// [自訂] SC-68：下載用的版本。上游轉存的權重沿用 "main"；新增的第三方權重固定在某個 commit。
    revision: &'static str,
    file: &'static str,
    size: u64,
    sha256: &'static str,
}

impl GgufWeight {
    /// [自訂] S56：只提供官方 HuggingFace（原為鏡像優先、官方兜底，含 "HuggingFace Mirror"）。
    /// 存儲裡的舊值（如 "HuggingFace Mirror"）對不上時，registry 與前端都回落到官方。
    fn sources(&self) -> Vec<DownloadSource> {
        vec![
            self.at("HuggingFace", hf(self.repo, self.revision, self.file)),
        ]
    }

    fn at(&self, source: &str, url: String) -> DownloadSource {
        DownloadSource {
            source: source.into(),
            files: vec![ModelFile {
                name: self.file.into(),
                url,
                size_bytes: self.size,
                sha256: Some(self.sha256.into()),
            }],
        }
    }
}

/// 本地可用模型。全部是 GGUF（ggml / transcribe.cpp）——上一代 ONNX 模型
/// 已隨 sherpa-onnx 一併移除，舊目錄由 local_asr::reclaim_legacy_models 回收。
///
/// 列表順序 = UI 展示順序（前端不排序）。按**從快到準**排：越往下越準、越慢、
/// 下載越大，使用者從上往下掃一遍就能理解這幾項的關係。
pub fn get_available_models() -> Vec<ModelInfo> {
    available_models().to_vec()
}

/// [自訂] SC-62：目錄是編譯期固定的內容（與下載或刪除哪些模型無關），只建一次；
/// 每次開錄查「已下載模型」時不再重組整份目錄與網址。
pub fn available_models() -> &'static [ModelInfo] {
    static CATALOG: std::sync::OnceLock<Vec<ModelInfo>> = std::sync::OnceLock::new();
    CATALOG.get_or_init(build_catalog)
}

fn build_catalog() -> Vec<ModelInfo> {
    vec![
        // ── Parakeet Unified EN 0.6B GGUF（NVIDIA，parakeet 族）──
        // 英文專用，但在英文上**同時**是本目錄裡最快和最準的一檔：
        // 上游 WER（LibriSpeech test-clean）Q4_K_M = 1.62%，比 Qwen3-ASR 1.7B 的
        // 高精度檔（1.65%）還略好，而本機實測 RTF 0.056（CPU）/ 0.023（Vulkan），
        // 比 SenseVoice 的 0.063 還快。也就是說它嚴格支配所有其它條目 —— 前提是
        // 只說英文。中文音訊會輸出**空字串**（實測），所以它不能當通用預設。
        // 標點和大小寫是模型固有行為，不掛 ITN/PNC 開關（兩個都報 unsupported，
        // 見 gguf_asr.rs 的 run_options）；數字會規範化（"2026" 而不是拼讀）。
        ModelInfo {
            id: "parakeet-unified-en-0.6b-gguf".into(),
            name: "Parakeet Unified EN".into(),
            description: "英文最快也最準，僅支援英文".into(),
            model_type: "parakeet-gguf".into(),
            total_size_bytes: PARAKEET_UNIFIED_EN_Q4.size,
            speed: 9.0,
            accuracy: 9.0,
            recommended: false,
            memory_mb: 900,
            featured: false,
            languages_label: "英文".into(),
            quant: "Q4_K_M".into(),
            languages: vec!["en".into()],
            sources: PARAKEET_UNIFIED_EN_Q4.sources(),
        },
        // ── SenseVoice Small GGUF（ggml 引擎）──
        // 標點靠 RunOptions 的 itn=on 開啟（預設是 <|woitn|> 分支、無標點），
        // 實測 9s 音訊純 CPU RTF 0.063，與 sherpa 版同一量級。
        ModelInfo {
            id: "sensevoice-small-gguf".into(),
            name: "SenseVoice Small".into(),
            description: "速度極快，日常首選".into(),
            model_type: "sensevoice-gguf".into(),
            total_size_bytes: SENSEVOICE.size,
            speed: 8.5,
            accuracy: 7.0,
            recommended: true,
            memory_mb: 350,
            featured: true,
            languages_label: "中、英、日、韓、粵語".into(),
            quant: "Q8_0".into(),
            languages: vec!["zh".into(), "en".into(), "ja".into(), "ko".into(), "yue".into()],
            sources: SENSEVOICE.sources(),
        },
        // ── Nemotron 3.5 ASR Streaming 0.6B GGUF（NVIDIA，parakeet 族）──
        // 存在的意義是**語種覆蓋面**：32 個 locale，含西/法/德/意/葡/荷/俄/阿/印地/
        // 土/越/烏 等 Qwen3 也沒有或更慢的語種。上游標稱原生標點與大小寫（PnC），
        // 數字保持口語形式（訓練文本就是 spoken form，不是缺陷）。
        // 準確度明顯不如 parakeet：LibriSpeech test-clean Q4_K_M = 3.28% vs 1.62%，
        // 所以只說英文的話沒有理由選它。實測 RTF 0.069（CPU）/ 0.032（Vulkan）。
        //
        // ⚠️ 這是本目錄裡第一個用**帶地區 locale**（`en-US` / `zh-CN`）自報語種的
        //    模型，且只認這種形式。介面上的 `localAsr.language` 只有 auto/zh/en/ja/ko，
        //    直接透傳會 `unsupported language (status 10)` —— 對映在
        //    gguf_asr.rs 的 `resolve_language`，別繞過它。
        ModelInfo {
            id: "nemotron-asr-streaming-0.6b-gguf".into(),
            name: "Nemotron 3.5 ASR".into(),
            description: "語種最全，適合多語言混用".into(),
            model_type: "parakeet-gguf".into(),
            total_size_bytes: NEMOTRON_STREAMING_Q4.size,
            speed: 8.0,
            accuracy: 7.5,
            recommended: false,
            memory_mb: 1050,
            featured: false,
            languages_label: "32 語種".into(),
            quant: "Q4_K_M".into(),
            languages: vec![
                "en".into(),
                "es".into(),
                "fr".into(),
                "de".into(),
                "it".into(),
                "pt".into(),
                "ru".into(),
                "zh".into(),
                "ja".into(),
                "ko".into(),
            ],
            sources: NEMOTRON_STREAMING_Q4.sources(),
        },
        // ── Fun-ASR Nano 2512 GGUF（SenseVoice 編碼器 + Qwen3 解碼器）──
        // FunAudioLLM 出的，和 SenseVoice 同一個實驗室，只做中/英/日三語。
        // 它在**速度和準確度上同時**優於 Qwen3-ASR 0.6B：
        //   上游 WER 1.79% vs 2.11%，參考機（Ryzen 4750U + Vulkan）10.5x 即時 vs 8.0x。
        // 本機實測 RTF 0.075（0.6B 是 0.094、1.7B Q5 是 0.153），記憶體 1.4 G。
        // 也就是說它拿到了接近 1.7B 的準確度（1.79 vs 1.65），但只花 1.7B 一半的時間、
        // 一半多點的記憶體。代價是語種少：只有 zh/en/ja，不像 Qwen3 有 30 種。
        // 注意 itn=true：標點和 SenseVoice 一樣掛在 ITN 開關上，不開就沒有標點。
        ModelInfo {
            id: "funasr-nano-2512-gguf".into(),
            name: "Fun-ASR Nano".into(),
            description: "又快又準，中文口述的高 CP 值之選".into(),
            model_type: "funasr-nano-gguf".into(),
            total_size_bytes: FUNASR_NANO_Q8.size,
            speed: 7.5,
            accuracy: 8.8,
            recommended: false,
            memory_mb: 1400,
            featured: true,
            languages_label: "中、英、日文".into(),
            quant: "Q8_0".into(),
            languages: vec!["zh".into(), "en".into(), "ja".into()],
            sources: FUNASR_NANO_Q8.sources(),
        },
        // ── Qwen3-ASR 0.6B GGUF（ggml 引擎，transcribe.cpp）──
        // GGUF 塊量化 + ggml 推理：實測 9s 中文音訊純 CPU RTF 0.167（同一模型走
        // ONNX INT8 慢一個量級），且自帶標點、輸出無模板字首。
        // 上游 WER（LibriSpeech test-clean）Q8_0 = 2.11%。
        ModelInfo {
            id: "qwen3-asr-0.6b-gguf".into(),
            name: "Qwen3-ASR 0.6B".into(),
            description: "較快，多語種的輕量之選".into(),
            model_type: "qwen3-asr-gguf".into(),
            total_size_bytes: QWEN3_06B_Q8.size,
            speed: 7.0,
            accuracy: 8.5,
            recommended: false,
            memory_mb: 1500,
            featured: false,
            languages_label: "30+ 語種".into(),
            quant: "Q8_0".into(),
            languages: vec!["zh".into(), "en".into(), "yue".into(), "ja".into(), "ko".into()],
            sources: QWEN3_06B_Q8.sources(),
        },
        // ── Qwen3-ASR 1.7B GGUF Q4_K_M（1.7B 的輕量檔）──
        // 存在的意義是讓"想要 1.7B 的準確度但不想下 1.4 GB / 不想佔 2 GB 記憶體"
        // 的人有得選。上游 WER 1.81%，仍明顯好於 0.6B 的 Q8_0（2.11%），
        // 下載只比 0.6B 多 448 MB。
        // 比 Q5_K_M 小 188 MB、實測快 14~20%（少一成多視訊記憶體頻寬），代價是 WER 1.65 → 1.81。
        ModelInfo {
            id: "qwen3-asr-1.7b-q4-gguf".into(),
            name: "Qwen3-ASR 1.7B".into(),
            description: "很準，高精度檔的加速版".into(),
            model_type: "qwen3-asr-gguf".into(),
            total_size_bytes: QWEN3_17B_Q4.size,
            speed: 4.5,
            accuracy: 8.8,
            recommended: false,
            memory_mb: 2300,
            featured: false,
            languages_label: "30+ 語種".into(),
            quant: "Q4_K_M".into(),
            languages: vec!["zh".into(), "en".into(), "yue".into(), "ja".into(), "ko".into()],
            sources: QWEN3_17B_Q4.sources(),
        },
        // ── Qwen3-ASR 1.7B GGUF Q5_K_M（精度上限）──
        // 上游 WER 1.65%（Q8_0 是 1.61%，但要下 2.19 GB，性价比不值）；Q5_K_M
        // 也是上游對 1.7B 的預設檔。解碼成本約 0.6B 的 2 倍。
        ModelInfo {
            id: "qwen3-asr-1.7b-gguf".into(),
            name: "Qwen3-ASR 1.7B 高精度".into(),
            description: "最準，但明顯更慢".into(),
            model_type: "qwen3-asr-gguf".into(),
            total_size_bytes: QWEN3_17B_Q5.size,
            speed: 4.0,
            accuracy: 9.0,
            recommended: false,
            memory_mb: 2600,
            featured: true,
            languages_label: "30+ 語種".into(),
            quant: "Q5_K_M".into(),
            languages: vec!["zh".into(), "en".into(), "yue".into(), "ja".into(), "ko".into()],
            sources: QWEN3_17B_Q5.sources(),
        },
        // ── Breeze ASR 25 GGUF Q8_0（台灣華語）──
        // [自訂] SC-68：可選模型，非預設、不在直接展示之列。速度、準確度與記憶體是依模型規模
        // （1.5B Whisper large-v2）估的，要在雲端實跑後更新；speed / accuracy 只用於排序與長條圖。
        ModelInfo {
            id: "breeze-asr-25-gguf".into(),
            name: "Breeze ASR 25".into(),
            description: "台灣華語與中英夾雜，輸出正體；體積大、較慢".into(),
            model_type: "whisper-gguf".into(),
            total_size_bytes: BREEZE_ASR_25_Q8.size,
            speed: 3.5,
            accuracy: 9.0,
            recommended: false,
            memory_mb: 2400,
            featured: false,
            languages_label: "中文（台灣）、英文".into(),
            quant: "Q8_0".into(),
            languages: vec!["zh".into(), "en".into()],
            sources: BREEZE_ASR_25_Q8.sources(),
        },
    ]
}

// GGUF 權重座標（倉庫 / 檔名 / 位元組數 / sha256）。
// 這幾份權重是自託管在 `cswk/sayit-asr-gguf`——從 handy-computer 那套
// **transcribe.cpp 相容**的 GGUF 原樣轉存（位元組完全一致，故體積 / sha256 不變），
// 目的是不再依賴第三方倉庫（對方刪庫我們就抓瞎）。體積 / sha256 用每個檔案的
// Git-LFS 指標核對過（`https://huggingface.co/<repo>/raw/main/<file>` 返回 oid+size）。
// 換量化檔時改這裡就夠，模型條目裡不再重複寫體積和校驗和。
// （`memory_mb` 是實測工作集，不是從這裡的體積推算的——見 ModelInfo 欄位註釋。）
//
// ⚠️ 別隨便換成"看起來同名"的其它 HF 倉庫（如 FunAudioLLM 官方、mradermacher、
//    cstr 的 GGUF）：它們是給別的執行時（llama.cpp 系）轉的，`general.architecture`
//    標成了 `sensevoice-small` / 其它，而我們的引擎 transcribe.cpp 只認 handy-computer
//    這套轉換所用的架構標籤（如 `sensevoice`）。換了會在載入時報
//    "unsupported architecture (status 5)"，下載/校驗和都正常也沒用。
//    要換源，只能轉存"transcribe.cpp 相容"的那一份（就像現在這樣），而不是另找重轉的。
//    size_bytes 會在下載前與伺服器報告值核對；sha256 會在入庫前校驗完整檔案，二者都必須準確。

const PARAKEET_UNIFIED_EN_Q4: GgufWeight = GgufWeight {
    repo: "cswk/sayit-asr-gguf",
    revision: "main",
    file: "parakeet-unified-en-0.6b-Q4_K_M.gguf",
    size: 477_274_496,
    sha256: "a8bf3de2b393bd14ead5a858c3748d5e3b07a20fdeabdd3b498fba4f463fa929",
};

const NEMOTRON_STREAMING_Q4: GgufWeight = GgufWeight {
    repo: "cswk/sayit-asr-gguf",
    revision: "main",
    file: "nemotron-3.5-asr-streaming-0.6b-Q4_K_M.gguf",
    size: 495_831_520,
    sha256: "41c99fa5fb6f3d35f68e79adc3e755eca2232a8d921178bd647b71194792b8fd",
};

const SENSEVOICE: GgufWeight = GgufWeight {
    repo: "cswk/sayit-asr-gguf",
    revision: "main",
    file: "SenseVoiceSmall-Q8_0.gguf",
    size: 252_684_608,
    sha256: "6c759ee4c9748c9b3f7a5a60ca74f0f7e685fb9d45d1378fce7cfd62f59adf29",
};

const FUNASR_NANO_Q8: GgufWeight = GgufWeight {
    repo: "cswk/sayit-asr-gguf",
    revision: "main",
    file: "Fun-ASR-Nano-2512-Q8_0.gguf",
    size: 891_270_912,
    sha256: "681caef6df15a2c0e153b40ca7fe4087fdf65751fa5e6fe605d8a75dff969e61",
};

const QWEN3_06B_Q8: GgufWeight = GgufWeight {
    repo: "cswk/sayit-asr-gguf",
    revision: "main",
    file: "Qwen3-ASR-0.6B-Q8_0.gguf",
    size: 850_423_456,
    sha256: "f081b2d5e23bd669d92cc331d722a8a0681943b8e6f34b48996fd5c319b5acd8",
};

const QWEN3_17B_Q4: GgufWeight = GgufWeight {
    repo: "cswk/sayit-asr-gguf",
    revision: "main",
    file: "Qwen3-ASR-1.7B-Q4_K_M.gguf",
    size: 1_319_830_496,
    sha256: "b7afe3674f653fa84f712ed2440353c6e7cf7f93697fef76b05a26538b24844e",
};

// [自訂] SC-68：Breeze ASR 25（MediaTek Research，Apache-2.0），以 Whisper large-v2 為基礎、
// 針對台灣華語與中英夾雜調整，輸出正體中文。權重是 handy-computer 為 transcribe.cpp 轉的 GGUF
// （`general.architecture = whisper`，`stt.whisper.*` 欄位與 transcribe-cpp 0.1.3 的 whisper
// 載入端一致）。固定在下面的 commit；體積與 sha256 已由雲端煙霧測試下載後核對相符。
const BREEZE_ASR_25_Q8: GgufWeight = GgufWeight {
    repo: "handy-computer/Breeze-ASR-25-gguf",
    revision: "c827f209c3fcb4956f49f9935ae04a0d88b7fae9",
    file: "Breeze-ASR-25-Q8_0.gguf",
    size: 1_667_964_224,
    sha256: "1650c163cd1623d13b585368ffd87d7e669c5ff23014511e0fa91bb4ce994756",
};

const QWEN3_17B_Q5: GgufWeight = GgufWeight {
    repo: "cswk/sayit-asr-gguf",
    revision: "main",
    file: "Qwen3-ASR-1.7B-Q5_K_M.gguf",
    size: 1_517_290_464,
    sha256: "034c557fe92ff8fcd9a9c041cbdaad347be0a86a58d3a348f63cf3f0180879d0",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_is_built_once_and_matches_a_fresh_build() {
        assert!(std::ptr::eq(available_models(), available_models()));
        let cached = serde_json::to_value(available_models()).unwrap();
        assert_eq!(cached, serde_json::to_value(build_catalog()).unwrap());
        assert_eq!(serde_json::to_value(get_available_models()).unwrap(), cached);
    }

    /// 目錄的結構性不變數。跑得快、不碰網路，純粹是防手寫座標時的低階錯誤。
    #[test]
    fn catalog_entries_are_internally_consistent() {
        let models = get_available_models();
        assert!(!models.is_empty());

        let mut ids = std::collections::HashSet::new();
        let mut names = std::collections::HashSet::new();
        for m in &models {
            assert!(ids.insert(m.id.clone()), "模型 id 重复: {}", m.id);
            // 展示名去掉量化檔字尾後必須仍然互不相同（兩個 1.7B 靠"高精度"區分），
            // 否則刪除確認框、當前模型提示這些只顯示 name 的地方會分不清
            assert!(names.insert(m.name.clone()), "模型展示名重复: {}", m.name);
            // id 決定權重的落盤目錄，重名會讓兩個量化檔下到同一個資料夾裡
            assert!(m.id.ends_with("-gguf"), "{} 的 id 应以 -gguf 结尾", m.id);
            assert!(!m.sources.is_empty(), "{} 没有下载源", m.id);

            // 引數行三件套：加新模型時漏填其中一個，UI 上就是一截空白
            assert!(m.memory_mb > 0, "{} 缺 memory_mb（实测值，见字段注释）", m.id);
            assert!(!m.languages_label.is_empty(), "{} 缺 languages_label", m.id);
            assert!(!m.quant.is_empty(), "{} 缺 quant", m.id);
            // 展示名不該再帶術語字尾（GGUF/量化檔都在 quant 欄位裡）
            assert!(
                !m.name.contains("GGUF") && !m.name.contains("Q8") && !m.name.contains("Q4") && !m.name.contains("Q5"),
                "{} 的展示名还带着量化档术语: {}",
                m.id,
                m.name
            );

            for s in &m.sources {
                let sum: u64 = s.files.iter().map(|f| f.size_bytes).sum();
                assert_eq!(
                    sum, m.total_size_bytes,
                    "{} 的源 {} 文件体积合计 {} 与 total_size_bytes {} 不一致",
                    m.id, s.source, sum, m.total_size_bytes
                );
                for f in &s.files {
                    assert!(f.url.ends_with(&f.name), "{} 的 URL 与文件名不匹配: {}", m.id, f.url);
                    let sha = f.sha256.as_deref().unwrap_or("");
                    assert_eq!(sha.len(), 64, "{} 的 {} 缺少或非法 sha256", m.id, f.name);
                }
            }
        }

        // 恰好一個推薦模型：前端拿它當預設高亮，多個或零個都是配置錯誤
        assert_eq!(
            models.iter().filter(|m| m.recommended).count(),
            1,
            "推荐模型必须且只能有一个"
        );

        // 直接展示的模型恰好三個（小/中/大），其餘摺疊進「更多」；
        // 推薦模型必須在直接展示之列，不能藏在摺疊裡
        assert_eq!(
            models.iter().filter(|m| m.featured).count(),
            3,
            "featured 模型应恰好三个（小/中/大）"
        );
        assert!(
            models.iter().any(|m| m.recommended && m.featured),
            "推荐模型必须是 featured"
        );
    }

    // [自訂] SC-68：Breeze 是可選模型，不是預設也不直接展示；下載網址固定在指定的 commit。
    #[test]
    fn breeze_is_optional_and_pinned_to_a_revision() {
        let models = get_available_models();
        let breeze = models.iter().find(|m| m.id == "breeze-asr-25-gguf").expect("breeze entry");
        assert!(!breeze.recommended && !breeze.featured);
        assert_eq!(breeze.total_size_bytes, 1_667_964_224);
        let [file] = breeze.sources[0].files.as_slice() else { panic!("one weight") };
        assert_eq!(
            file.url,
            "https://huggingface.co/handy-computer/Breeze-ASR-25-gguf/resolve/c827f209c3fcb4956f49f9935ae04a0d88b7fae9/Breeze-ASR-25-Q8_0.gguf"
        );
        assert_eq!(file.sha256.as_deref(), Some("1650c163cd1623d13b585368ffd87d7e669c5ff23014511e0fa91bb4ce994756"));
        // 其他模型沿用原本的 main，網址不變。
        for model in models.iter().filter(|m| m.id != breeze.id) {
            for source in &model.sources {
                for file in &source.files {
                    assert!(file.url.starts_with("https://huggingface.co/cswk/sayit-asr-gguf/resolve/main/"), "{}", file.url);
                }
            }
        }
    }

    // [自訂] SC-35：所有下載來源必須指向同一份且唯一的權重。
    #[test]
    fn every_source_has_the_same_single_weight() {
        for model in get_available_models() {
            let [expected] = model.sources.first().unwrap().files.as_slice() else {
                panic!("{} must have exactly one weight", model.id);
            };
            for source in &model.sources {
                let [file] = source.files.as_slice() else {
                    panic!("{} / {} must have exactly one weight", model.id, source.source);
                };
                assert_eq!(
                    (&file.name, file.size_bytes, &file.sha256),
                    (&expected.name, expected.size_bytes, &expected.sha256),
                    "{} / {}", model.id, source.source,
                );
            }
        }
    }

    /// 前端的下載源下拉框是拿 `models[0].sources` 生成的，然後把選中的名字用於
    /// **所有**模型的下載。所以每個模型都必須提供同一組源名，否則選了某個源之後
    /// 下載別的模型會靜默回落到第一個源。
    #[test]
    fn every_model_offers_the_same_download_sources() {
        let models = get_available_models();
        let expected: Vec<&str> = models[0].sources.iter().map(|s| s.source.as_str()).collect();
        for m in &models {
            let got: Vec<&str> = m.sources.iter().map(|s| s.source.as_str()).collect();
            assert_eq!(got, expected, "{} 的下载源与第一个模型不一致", m.id);
        }
        // defaults.ts 裡 localAsr.downloadSource 的預設值必須能對上其中一個
        // [自訂] S25/S56：只允許官方 HuggingFace（原斷言 contains "HuggingFace Mirror"）
        assert_eq!(expected, vec!["HuggingFace"]);
        for m in &models {
            for src in &m.sources {
                for f in &src.files {
                    assert!(!f.url.contains("hf-mirror"), "{} 仍含鏡像網址", m.id);
                }
            }
        }
    }

    /// 排序契約：列表順序就是 UI 順序，按 speed 從高到低。寫成測試是因為
    /// 以後加模型時很容易隨手 append 到末尾，把這個順序打亂。
    ///
    /// 只斷言 speed 單調，**不斷言 accuracy 反向單調**：Fun-ASR Nano 比
    /// Qwen3-ASR 0.6B 又快又準（嚴格支配它），所以"越往下越準"這個更強的
    /// 約定已經不成立了。留 0.6B 是因為它有 30 語種、Fun-ASR 只有 3 種。
    /// Parakeet Unified EN 把這一點推到了極端：它在英文上同時是最快和最準的
    /// （RTF 0.056 / WER 1.62%），排在最前卻拿著最高的 accuracy。列表能這麼排
    /// 是因為**語種**才是真正的取捨維度 —— 它只會英文。
    #[test]
    fn models_are_ordered_from_fast_to_slow() {
        let models = get_available_models();
        for w in models.windows(2) {
            assert!(
                w[0].speed >= w[1].speed,
                "{} 的 speed 应不低于其后的 {}",
                w[0].id,
                w[1].id
            );
        }
    }
}
