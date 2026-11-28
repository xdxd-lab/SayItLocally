// 「這家 ASR 拿使用者的熱詞做什麼」的唯一宣告處。
//
// ## 為什麼需要這個檔案
//
// 起因是 issue #67：FunASR 上游維護者核對原始碼後指出，經「OpenAI 相容」的
// multipart 協議接入時，使用者配的熱詞**根本沒有進請求** —— `asr_groq.rs` 的引數當時
// 寫作 `_hotwords`，而唯一能放詞表的 `prompt` 欄位被拿去做中文標點引導了。
//
// 但真正該修的不是那一家。「哪些家會用熱詞」這份知識當時存在兩個地方：實現在各
// provider 裡（用不用 hotwords 由每個檔案自己決定），宣告在前端一張手寫的表格裡
// （Dictionary.tsx 硬編碼 7 行，漏了 6 家雲服務）。兩者之間沒有任何機制關聯 ——
// 加一家 provider 時不會有東西提醒你更新那張表，改一處實現時也不會有東西告訴你
// 文案已經過期。所以漂移是必然的。
//
// 讓它能長期存在的是**漂移的靜默性**：熱詞沒參與，轉寫照樣成功，使用者只覺得
// "識別得不太準"。這和 `dev-docs/critique-語音引擎頁-2026-08-03.md` 記的那條同型 ——
// 「測試連線顯示 ASR=on 會讓使用者合理地認為配置生效了，而它沒有」。
//
// ## 為什麼宣告集中在這裡，而熱詞的加工留在各 provider
//
// 判據是**這份知識屬於誰**：
//   · 「某家協議長什麼樣」（`parameters.vocabulary` 是 {詞: 權重}、`corpus.context`
//     是一個字串、`transcription.keywords` 是陣列）—— 留在那家的檔案裡。抽成公共
//     函式會把協議知識搬離協議實現，provider 反而被掏空，加一家要改兩處。
//   · 「哪些家有這個能力」—— 集中到一處。分散就會漂移，這次的 bug 就是它。
//
// 所以這裡只有列舉和 match，不碰任何請求體。
//
// ## ⚠️ 加一家 ASR 時必須同時改的兩處
//
// 這個檔案的 match 和 `registry.rs::cloud_transcribe` 的 match **key 列表必須一致**。
// 兩個檔案刻意相鄰，但相鄰只是習慣、不是機制 —— 真正攔住漏宣告的是
// `ALL_ASR_PROVIDERS` 那份清單加下面的測試：漏了就報 `UnknownProvider`，測試當場紅。
//
// 而那條一致性測試的對照**是從 `cloud_transcribe` 的原始碼裡掃出來的**
// （`registry.rs::dispatch_keys_for_test`），不是另一份手抄清單。這個區別要緊：
// 手抄副本只能證明"我抄得一致"，改了真實分發而不動副本照樣全綠，新增和刪除分支
// 都漏得掉。掃原始碼之後，動 match 就必然反映到對照集合上。
//
// **`UnknownProvider` 絕不能回落成「不傳熱詞」**。那樣刪掉一條宣告測試照樣綠，
// 而介面會平靜地告訴使用者「這家不用熱詞」—— 把"我們沒宣告"說成"它不支援"。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 熱詞能到達協議的哪一層。
///
/// 刻意不用「支援 / 不支援」：**沒有哪一檔是精確命中的**。
/// `asr_openai_realtime.rs` 裡 `KEYWORD_LIMIT` 的註釋自己寫著「keywords 是提示不是
/// 強制，給太多反而會稀釋每一個的作用」；`dict.caveat` 那句「熱詞只提高命中機率」
/// 才是對全部檔位都成立的說法。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotwordDelivery {
    /// 協議有專門的熱詞欄位，可帶權重或條數約束。
    /// 千問 Audio 3.0 的 `parameters.vocabulary`、OpenAI Realtime 的 `keywords`。
    Vocabulary,
    /// 作為識別上下文捎帶（軟偏置）。豆包的 `corpus.context`、千問的 `corpus.text`。
    /// 注意這一檔會被**回顯**進識別結果，所以 `asr_qwen.rs` 有 `strip_hotword_echo`。
    Context,
    /// 拼進給模型的自然語言指令。最弱的一檔，模型可能把詞表當成內容處理。
    Instruction,
    /// 這條協議本身沒有放熱詞的位置。
    /// 與下面那檔分開是為了讓「將來能不能接」這個問題有答案 ——
    /// 這一檔接不了，`NotWiredUp` 那檔是我們還沒接。
    ProtocolHasNoSlot,
    /// 協議有位置，但當前實現沒有把熱詞放進去。
    /// `asr_groq.rs` 是典型：`prompt` 欄位被中文標點引導佔用了（見它的
    /// `PUNCTUATION_PROMPT`，缺了它中文短句一個標點都沒有）。
    NotWiredUp,
    /// 「OpenAI 相容」的 auto 檔：協議要探測出來才知道，探到 `chat` 有、
    /// `transcriptions` 沒有。探測結果快取在 `asr_openai_compat.rs` 的 PROTOCOL_CACHE。
    UndecidedProtocol,
    /// 這個 provider 不在宣告清單裡。**這是一個 bug 訊號，不是一種能力。**
    UnknownProvider,
}

impl HotwordDelivery {
    /// 熱詞這一次到底進沒進 ASR 請求。
    ///
    /// 介面只需要這個布林值：前三檔對使用者的行動沒有區別（都是"配了有幫助、
    /// 不保證命中"），只有不進請求那幾檔需要他去做別的事。
    pub fn reaches_asr(self) -> bool {
        matches!(
            self,
            HotwordDelivery::Vocabulary | HotwordDelivery::Context | HotwordDelivery::Instruction
        )
    }
}

/// 一份配置在兩條執行路徑上的熱詞行為，以及客戶端自己加的條數上限。
///
/// **兩條路徑必須分開回答，這不是錦上添花**：同一個 provider 在兩條路上的行為可以
/// 相反 —— `openai_live_transcribe` 流式走 `asr_openai_realtime`（keywords）、回落走
/// `asr_groq`（忽略）；`gemini_live_transcribe` 正好反過來，流式走 `asr_gemini_live`
/// （忽略）、回落走 `asr_gemini`（拼進 prompt）。只按 provider 給一個答案，
/// 這兩家裡必有一家被說錯。
///
/// 而且走哪條是**執行時**才定的：沒開即時字幕、這家沒有流式實現、配置沒就緒
/// （千問 realtime 缺業務空間 ID）、或者建連失敗，都會回落到一次性路徑
/// （見 `CloudAPIProvider.ts::tryStartRealtimeStream`）。所以設定頁只能展示
/// 「配置預期」，本次真正走了哪條要看日誌。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AsrHotwordCapability {
    /// 走流式路徑（`*_stream_open`）時的行為。
    /// 這家沒有流式實現時等於 `buffered`，免得前端還要判斷該看哪個欄位。
    pub streaming: HotwordDelivery,
    /// 走一次性路徑（`registry.rs::cloud_transcribe`）時的行為，
    /// 也是流式不可用或建連失敗回落後的實際行為。
    pub buffered: HotwordDelivery,
    /// 這家有沒有流式實現。為假時上面兩個欄位必然相同。
    pub has_streaming_path: bool,
    /// **SayIt 自己設的**最多傳送條數，不是服務端限制。走流式路徑時的那份。
    ///
    /// 三家都是 100，三處註釋都寫明了是客戶端保護：千問的文件「只對超級熱詞明確了
    /// 50 的上限，普通熱詞沒給數字」，OpenAI 的是怕整條 `session.update` 被拒，
    /// Gemini 的是怕 prompt 被撐長、每個詞的作用被稀釋。
    /// 介面上必須說成"SayIt 最多傳送 N 條"，不能說成"這家協議只吃 N 條"。
    pub streaming_client_cap: Option<usize>,
    /// 同上，走一次性路徑（含流式回落）時的那份。
    ///
    /// **必須和 delivery 一起按路徑分開給，一個欄位不夠。** 現場證據是
    /// `openai_live_transcribe`：流式的 `keywords` 有 100 條上限，關掉即時字幕後
    /// 回落到檔案端點則一條都不發。只給一個欄位的話，前端在關字幕時會同時顯示
    /// 「不傳送」和「最多傳送 100 個，其餘不發出」—— 兩句話互相矛盾。
    /// 反方向同理：`gemini_live_transcribe` 的上限只存在於回落那條路上。
    pub buffered_client_cap: Option<usize>,
}

/// 全部 ASR 分發 key。**必須與 `registry.rs::cloud_transcribe` 的 match 一致。**
///
/// 含三類：
///   · 前端目錄裡選得到的（`asrProviderCatalog.ts` 各模型的 `provider`）
///   · 內部 key（`openai_compat_transcribe` / `openai_chat_audio`，由
///     `asr_openai_compat` 探測後分發）
///   · 存量配置裡的舊 key（`doubao` / `aliyun`），遷移後仍可能出現在設定裡
pub const ALL_ASR_PROVIDERS: &[&str] = &[
    "doubao",
    "doubao_v2",
    "qwen",
    "aliyun",
    "qwen_realtime",
    "qwen_audio_stream",
    "qwen_omni",
    "qwen_chat_audio",
    "mimo",
    "groq_whisper",
    "openai_transcribe",
    "openai_live_transcribe",
    "openai_compat",
    "openai_compat_transcribe",
    "openai_chat_audio",
    "openai_chat_audio_standard",
    "gemini_transcribe",
    "gemini_live_transcribe",
    "openrouter_transcribe",
];

/// 千問 Audio 3.0 與 OpenAI Realtime 各自的客戶端條數上限。
///
/// 刻意在這裡重複一遍而不是從那兩個模組 `pub` 出來：那兩個常量是**請求構造的引數**
/// （改它會改發出去的內容），這裡是**給使用者看的說明**。讓說明去 import 請求引數，
/// 會讓「改上限」這個動作悄悄改掉介面文案，反過來也一樣。兩邊各留一份、由下面的
/// 測試釘住一致性，比耦合起來安全。
const QWEN_VOCABULARY_CAP: usize = 100;
const OPENAI_KEYWORD_CAP: usize = 100;
/// Gemini 檔案轉寫把熱詞拼進 prompt，同樣有 100 條的客戶端截斷
/// （`asr_gemini.rs::build_prompt` 的 `.take(HOTWORD_LIMIT)`）。
///
/// 漏宣告這一條的代價是**完全靜默**：配 150 個詞只發 100 個，而熱詞頁既不顯示上限
/// （沒宣告就沒有那行字），又夠不到 `HOTWORD_SOFT_LIMIT`（200）那條通用提醒。
/// Gemini Live 關掉即時字幕回落到檔案轉寫時是同一條路。
const GEMINI_PROMPT_CAP: usize = 100;

/// 一次性路徑（`registry.rs::cloud_transcribe`）上的熱詞行為。
///
/// 每一條都對應 registry 裡的一個分支，註釋裡寫的是那家實現的落點。
fn buffered_delivery(provider: &str, extra: &Value) -> HotwordDelivery {
    match provider {
        // asr_doubao / asr_doubao_stream → doubao_protocol::build_hotword_context
        // → request.corpus.context
        "doubao" | "doubao_v2" => HotwordDelivery::Context,
        // asr_qwen::build_hotword_context_text → 上下文文本（帶 strip_hotword_echo）
        "qwen" | "aliyun" | "qwen_realtime" => HotwordDelivery::Context,
        // asr_qwen_audio_stream::build_vocabulary → parameters.vocabulary（詞 → 權重 4）。
        // 宣告按 provider key 而不是模型名，所以 Qwen-Audio 3.0 與 3.1 共用這一檔 ——
        // 兩代都實測接受這個欄位（見那份實現的檔案頭）。注意 Vocabulary 這一檔的口徑
        // 始終是「進了請求」，不是「一定命中」。
        "qwen_audio_stream" => HotwordDelivery::Vocabulary,
        // asr_qwen_omni → 追加到 system instructions
        "qwen_omni" => HotwordDelivery::Instruction,
        // asr_openai_chat_audio → 複用 asr_qwen 的上下文文本，追加到 instruction
        "qwen_chat_audio" | "openai_chat_audio" | "openai_chat_audio_standard" => HotwordDelivery::Instruction,
        // asr_gemini::build_prompt → 拼一段 vocabulary 提示
        "gemini_transcribe" | "gemini_live_transcribe" => HotwordDelivery::Instruction,
        // asr_mimo：OpenAI chat/completions 相容，協議有 instruction 位置
        //（asr_openai_chat_audio 就是這麼做的），只是這份實現沒接。
        "mimo" => HotwordDelivery::NotWiredUp,
        // asr_groq：`/audio/transcriptions` 的 `prompt` 欄位是熱詞的天然位置
        //（OpenAI 的 prompting 指南把專有名詞和標點列在一起），但我們拿它換了中文
        // 標點。組合兩者尚未驗證 —— 驗證工具是現成的
        //（dev-scripts 裡那兩個 groq-asr-*-probe.mjs 打真實介面）。
        "groq_whisper" | "openai_transcribe" | "openai_compat_transcribe" => {
            HotwordDelivery::NotWiredUp
        }
        // openai_live_transcribe 關掉即時字幕時走 asr_groq 的檔案端點（模型換成
        // gpt-transcribe，見 asr_groq.rs 的 OPENAI_FILE_FALLBACK），所以和上面同檔。
        "openai_live_transcribe" => HotwordDelivery::NotWiredUp,
        // asr_openrouter：檔案頭寫明它的兩種形態都沒有 prompt 通道
        //（multipart 那條「接受後忽略」，原生 JSON 的參數列裡也沒有）。
        // 依據是官方文件 + `build_body` 裡確實沒有可用欄位；**這家整體沒有真介面驗證過**
        // （見 asr_openrouter.rs 檔案頭「未經真實介面驗證」，我們沒有 OpenRouter key）。
        "openrouter_transcribe" => HotwordDelivery::ProtocolHasNoSlot,
        // 「OpenAI 相容」那張卡：協議決定有沒有熱詞。使用者顯式指定過就能確定，
        // auto 要等探測（結果在 asr_openai_compat 的 PROTOCOL_CACHE 裡）。
        "openai_compat" => match extra.get("protocol").and_then(Value::as_str) {
            // → asr_openai_chat_audio，走 instruction
            Some("chat") | Some("chat_standard") => HotwordDelivery::Instruction,
            // → asr_groq，prompt 被標點佔用
            Some("transcriptions") => HotwordDelivery::NotWiredUp,
            // auto：探過就給準話，沒探過才說"未確定"。
            // 複用真實轉寫留下的快取，而不是只認「測試連線」—— 使用者可能直接開始口述。
            // 換了地址或模型，快取鍵就不命中，自然退回"未確定"。
            _ => match super::asr_openai_compat::detected_protocol(extra) {
                Some(inner) => buffered_delivery(inner, extra),
                None => HotwordDelivery::UndecidedProtocol,
            },
        },
        _ => HotwordDelivery::UnknownProvider,
    }
}

/// 流式路徑（前端 `STREAM_COMMANDS` 那組 `*_stream_open`）上的熱詞行為。
///
/// 返回 `None` 表示這家沒有流式實現 —— 它只會走一次性路徑。
/// 這份清單必須與 `CloudAPIProvider.ts` 的 `STREAM_COMMANDS` 一致。
fn streaming_delivery(provider: &str) -> Option<HotwordDelivery> {
    match provider {
        // asr_doubao_stream（流式 2.0）→ corpus.context
        "doubao_v2" => Some(HotwordDelivery::Context),
        // asr_qwen_realtime → session.input_audio_transcription.corpus.text
        "qwen_realtime" => Some(HotwordDelivery::Context),
        // asr_qwen_audio_stream → parameters.vocabulary
        "qwen_audio_stream" => Some(HotwordDelivery::Vocabulary),
        // asr_openai_realtime::build_keywords → transcription.keywords
        "openai_live_transcribe" => Some(HotwordDelivery::Vocabulary),
        // asr_gemini_live：Live API 沒有**專門的**熱詞欄位（見 gemini_live_open 的註釋，
        // 它收下引數只為和其他 open 簽名一致，並記一行 hotwords_ignored）。
        //
        // ⚠️ 這一檔**不是"查證過接不了"，是"沒有專門欄位且借道未驗證"**。Live API 的
        // setup 訊息支援 `systemInstruction`（我們現在只發 model / generationConfig /
        // inputAudioTranscription），而 asr_gemini 的檔案轉寫正是把熱詞拼進 prompt 的，
        // 照同樣做法拼進 systemInstruction 在形式上是可能的。
        // 沒那麼做也沒改成 NotWiredUp，是因為有一個真實的不確定性：
        // `inputAudioTranscription` 是獨立於對話生成的通道，systemInstruction 未必參與
        // 它的解碼。要改分類**必須先用真實 key 實測**，不能照著"欄位存在"就推斷。
        "gemini_live_transcribe" => Some(HotwordDelivery::ProtocolHasNoSlot),
        _ => None,
    }
}

/// 客戶端自設的條數上限（不是服務端限制）。
///
/// `path_delivery` 是**這一條路徑**上的檔位，不是這家的概括 —— 上限只在熱詞真的進了
/// 請求時才有意義。熱詞不進請求那條路上報一個上限，介面會同時說出「不傳送」和
/// 「最多傳送 100 個」。所以呼叫方必須為兩條路徑各調一次，見 `hotword_capability`。
fn client_cap(provider: &str, path_delivery: HotwordDelivery) -> Option<usize> {
    if !path_delivery.reaches_asr() {
        return None;
    }
    match provider {
        "qwen_audio_stream" => Some(QWEN_VOCABULARY_CAP),
        "openai_live_transcribe" => Some(OPENAI_KEYWORD_CAP),
        // asr_gemini::build_prompt 的 .take(HOTWORD_LIMIT)。兩個 key 都要寫：
        // gemini_live_transcribe 關掉即時字幕就走這份檔案轉寫實現（見 registry.rs
        // 裡它和 gemini_transcribe 並在同一個分支）。它流式那條路是
        // ProtocolHasNoSlot，上面的 reaches_asr 守衛會自己把上限擋掉。
        "gemini_transcribe" | "gemini_live_transcribe" => Some(GEMINI_PROMPT_CAP),
        _ => None,
    }
}

/// 這份配置在兩條路徑上分別怎麼對待熱詞。
///
/// `extra` 目前只用到 `protocol`（「OpenAI 相容」那張卡）。簽名收整個 Value 是留給
/// 以後的能力位 —— 語種碼粒度、標點自帶與否都會依賴配置，見 pitfalls #17。
pub fn hotword_capability(provider: &str, extra: &Value) -> AsrHotwordCapability {
    let buffered = buffered_delivery(provider, extra);
    let declared_streaming = streaming_delivery(provider);
    // 沒有流式實現時把兩條路徑填成同一個答案，免得前端還要判斷該看哪個欄位。
    let streaming = declared_streaming.unwrap_or(buffered);
    AsrHotwordCapability {
        // 兩條路徑各算一次。**不能只按其中一條算**：openai_live 的上限只存在於流式，
        // gemini_live 的只存在於回落，取錯一條就會和 delivery 自相矛盾。
        streaming_client_cap: client_cap(provider, streaming),
        buffered_client_cap: client_cap(provider, buffered),
        streaming,
        buffered,
        has_streaming_path: declared_streaming.is_some(),
    }
}

/// 供前端查詢的能力介面。
///
/// 前端不再自己維護一份「哪家支援熱詞」的表 —— 那正是 issue #67 的成因。
#[tauri::command]
pub fn asr_hotword_capability(provider: String, extra: Option<Value>) -> AsrHotwordCapability {
    hotword_capability(&provider, &extra.unwrap_or(Value::Null))
}

/// 全部分發 key 的熱詞行為，給「各服務對熱詞的支援」那張對照表用。
///
/// ## 為什麼要有這個，而不是在介面裡寫一張表
///
/// 介面本來就有一張手寫的 7 行表（熱詞頁的 ⓘ 裡），它漏了 6 家雲服務、還把「不支援」
/// 說成只是本地模型的問題 —— issue #67 就是撞在這上面。刪掉它之後又缺了「完整對照」
/// 這個真實需求（選型時要比較），而文件在應用之外、使用者點不到。
///
/// 所以表還要有，但**必須是實現的投影而不是另一份手寫清單**：這個命令把
/// `ALL_ASR_PROVIDERS` 整個過一遍 `hotword_capability`，前端只負責把 key 翻成
/// 使用者看得懂的名字（那部分要 i18n，本來就該留在前端）。
/// 這樣「改了實現但忘了改表」在結構上不可能發生。
///
/// 返回 map 而不是陣列：顯示順序由前端目錄決定（它才知道推薦順序和平台分組），
/// 這裡只提供事實，不摻和排版。
///
/// ⚠️ `openai_compat` 在這裡必然是 `UndecidedProtocol` —— 對照表沒有具體配置，
/// 而它的答案取決於探測出的協議。這是正確的：通用參考表就該說"取決於協議"，
/// 使用者當前那份配置的準話由 `asr_hotword_capability` 帶 extra 查。
#[tauri::command]
pub fn asr_hotword_capability_matrix() -> std::collections::HashMap<String, AsrHotwordCapability> {
    ALL_ASR_PROVIDERS
        .iter()
        .map(|provider| {
            (
                (*provider).to_string(),
                hotword_capability(provider, &Value::Null),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cap(provider: &str) -> AsrHotwordCapability {
        hotword_capability(provider, &Value::Null)
    }

    /// 每個分發 key 都要有明確宣告。
    ///
    /// 這條是防漂移的主力：加一家 provider 忘了在這裡宣告，它會落到
    /// `UnknownProvider`，測試當場紅。**判據刻意不是「不等於某個具體檔位」**，
    /// 而是「不是 UnknownProvider」——`_ =>` 回落成 NotWiredUp 的話，刪掉一條
    /// 宣告測試照樣會綠，那種測試等於沒寫。
    #[test]
    fn every_dispatch_key_declares_its_hotword_behaviour() {
        for provider in ALL_ASR_PROVIDERS {
            let c = cap(provider);
            assert_ne!(
                c.buffered,
                HotwordDelivery::UnknownProvider,
                "{} 沒有宣告一次性路徑的熱詞行為",
                provider,
            );
            assert_ne!(
                c.streaming,
                HotwordDelivery::UnknownProvider,
                "{} 沒有宣告流式路徑的熱詞行為",
                provider,
            );
        }
    }

    /// 不認識的 provider 必須和「明確宣告不傳熱詞」區分開。
    ///
    /// 混在一起的後果是把"我們漏了宣告"顯示成"這家不支援熱詞"——
    /// 用錯誤的確定性覆蓋掉一個 bug。
    #[test]
    fn an_unknown_provider_is_not_silently_reported_as_unsupported() {
        let unknown = cap("some_provider_we_never_heard_of");
        assert_eq!(unknown.buffered, HotwordDelivery::UnknownProvider);
        assert_ne!(unknown.buffered, HotwordDelivery::NotWiredUp);
        assert_ne!(unknown.buffered, HotwordDelivery::ProtocolHasNoSlot);
        // 也不能被 reaches_asr 說成"會傳"
        assert!(!unknown.buffered.reaches_asr());
    }

    /// issue #67 那條路徑：multipart 不傳熱詞。
    #[test]
    fn the_multipart_transcriptions_path_does_not_carry_hotwords() {
        for provider in [
            "groq_whisper",
            "openai_transcribe",
            "openai_compat_transcribe",
        ] {
            let c = cap(provider);
            assert_eq!(c.buffered, HotwordDelivery::NotWiredUp, "{}", provider);
            assert!(!c.buffered.reaches_asr(), "{}", provider);
        }
    }

    /// 兩家 live 供應商的熱詞行為在兩條路徑上是**相反**的。
    ///
    /// 這是「能力必須按執行路徑判斷、不能按 provider 一概而論」的現場證據：
    /// 只按 provider 宣告，這兩家裡必有一家被說錯。
    #[test]
    fn the_two_live_providers_are_inverted_between_paths() {
        let openai = cap("openai_live_transcribe");
        assert_eq!(openai.streaming, HotwordDelivery::Vocabulary);
        assert_eq!(openai.buffered, HotwordDelivery::NotWiredUp);
        assert!(openai.streaming.reaches_asr() && !openai.buffered.reaches_asr());

        let gemini = cap("gemini_live_transcribe");
        assert_eq!(gemini.streaming, HotwordDelivery::ProtocolHasNoSlot);
        assert_eq!(gemini.buffered, HotwordDelivery::Instruction);
        assert!(!gemini.streaming.reaches_asr() && gemini.buffered.reaches_asr());
    }

    /// 沒有流式實現的那些家，兩個欄位要給同一個答案。
    #[test]
    fn providers_without_a_streaming_path_report_the_same_answer_twice() {
        for provider in ["qwen", "mimo", "groq_whisper", "openrouter_transcribe"] {
            let c = cap(provider);
            assert!(!c.has_streaming_path, "{}", provider);
            assert_eq!(c.streaming, c.buffered, "{}", provider);
        }
    }

    /// 「協議沒位置」和「我們沒接」不能混：前者回答"將來能不能接"是不能，後者是能。
    #[test]
    fn a_missing_protocol_slot_is_distinct_from_an_unwired_one() {
        // OpenRouter 兩種形態都沒有 prompt 通道
        assert_eq!(
            cap("openrouter_transcribe").buffered,
            HotwordDelivery::ProtocolHasNoSlot,
        );
        // asr_groq 有 prompt 欄位，只是被標點引導佔用了
        assert_eq!(
            cap("openai_transcribe").buffered,
            HotwordDelivery::NotWiredUp,
        );
    }

    /// 「OpenAI 相容」的三種協議設定各對應一個確定答案，auto 不能假裝知道。
    #[test]
    fn the_compat_card_follows_the_protocol_setting() {
        let auto = hotword_capability("openai_compat", &json!({ "protocol": "auto" }));
        assert_eq!(auto.buffered, HotwordDelivery::UndecidedProtocol);
        assert!(!auto.buffered.reaches_asr());
        // 預設等於 auto
        assert_eq!(
            hotword_capability("openai_compat", &Value::Null).buffered,
            HotwordDelivery::UndecidedProtocol,
        );

        assert_eq!(
            hotword_capability("openai_compat", &json!({ "protocol": "chat" })).buffered,
            HotwordDelivery::Instruction,
        );
        assert_eq!(
            hotword_capability("openai_compat", &json!({ "protocol": "chat_standard" })).buffered,
            HotwordDelivery::Instruction,
        );
        assert_eq!(
            hotword_capability("openai_compat", &json!({ "protocol": "transcriptions" })).buffered,
            HotwordDelivery::NotWiredUp,
        );
    }

    /// 條數上限只在熱詞真的會進請求時才有意義。
    #[test]
    fn the_client_cap_is_only_reported_where_it_actually_applies() {
        let qwen = cap("qwen_audio_stream");
        assert_eq!(qwen.streaming_client_cap, Some(100));
        assert_eq!(qwen.buffered_client_cap, Some(100));
        // 豆包走 corpus.context，沒有客戶端截斷
        assert_eq!(cap("doubao_v2").buffered_client_cap, None);
        // 不進請求的那幾家不該顯示上限（"最多發 100 條"對一條都不發的路徑是誤導）
        assert_eq!(cap("groq_whisper").buffered_client_cap, None);
        assert_eq!(cap("openrouter_transcribe").buffered_client_cap, None);
    }

    /// 上限必須跟著**路徑**走，不能按 provider 給一個數。
    ///
    /// 只給一個欄位時的現場症狀：`openai_live_transcribe` 關掉即時字幕，介面同時
    /// 顯示「當前識別服務不使用熱詞」和「最多傳送 100 個，其餘不會發出」。
    /// 這兩家方向相反，任何"按 provider 取一個上限"的寫法必有一家說錯。
    #[test]
    fn the_cap_follows_the_execution_path_not_the_provider() {
        // 流式有 keywords（100 條），回落到檔案端點一條都不發 → 回落那條不該有上限
        let openai = cap("openai_live_transcribe");
        assert_eq!(openai.streaming_client_cap, Some(100));
        assert_eq!(
            openai.buffered_client_cap, None,
            "回落路徑一條熱詞都不發，卻報了傳送上限",
        );

        // Gemini live 反向：流式沒有熱詞欄位，回落到檔案轉寫才有 100 條截斷
        let gemini_live = cap("gemini_live_transcribe");
        assert_eq!(
            gemini_live.streaming_client_cap, None,
            "Live API 沒有熱詞欄位，卻報了傳送上限",
        );
        assert_eq!(gemini_live.buffered_client_cap, Some(100));
    }

    /// Gemini 檔案轉寫的 100 條截斷必須被宣告出來。
    ///
    /// 漏了它是**完全靜默**的一條：`asr_gemini.rs::build_prompt` 照樣 `.take(100)`，
    /// 而熱詞頁既不顯示上限（沒宣告就沒有那行字），又夠不到 `HOTWORD_SOFT_LIMIT`
    /// （200）那條通用提醒 —— 配 150 個詞的使用者看不到後 50 個被丟掉了。
    #[test]
    fn gemini_file_transcription_declares_its_truncation() {
        let gemini = cap("gemini_transcribe");
        assert_eq!(gemini.buffered, HotwordDelivery::Instruction);
        assert_eq!(gemini.buffered_client_cap, Some(100));
        // 沒有流式實現的那家，兩個欄位給同一個答案
        assert!(!gemini.has_streaming_path);
        assert_eq!(gemini.streaming_client_cap, gemini.buffered_client_cap);
    }

    /// 凡是宣告了上限的路徑，檔位必須是"熱詞真的進了請求"那三檔。
    ///
    /// 反過來說：`reaches_asr()` 為假卻帶著上限，就是一次自相矛盾的輸出。
    /// 遍歷全清單，新加 provider 時自動納入。
    #[test]
    fn no_path_reports_a_cap_without_actually_sending_hotwords() {
        for provider in ALL_ASR_PROVIDERS {
            let c = cap(provider);
            if !c.streaming.reaches_asr() {
                assert_eq!(
                    c.streaming_client_cap, None,
                    "{} 的流式路徑不發熱詞，卻報了傳送上限",
                    provider,
                );
            }
            if !c.buffered.reaches_asr() {
                assert_eq!(
                    c.buffered_client_cap, None,
                    "{} 的一次性路徑不發熱詞，卻報了傳送上限",
                    provider,
                );
            }
        }
    }

    /// 這裡的上限值必須和真正構造請求的那兩個常量一致。
    ///
    /// 兩邊各留一份是刻意的（見 QWEN_VOCABULARY_CAP 的註釋），代價是要這條測試
    /// 盯著。改了 asr_qwen_audio_stream.rs 的 HOTWORD_LIMIT 就會在這裡紅。
    #[test]
    fn the_documented_caps_match_the_request_builders() {
        assert_eq!(
            QWEN_VOCABULARY_CAP,
            crate::providers::asr_qwen_audio_stream::hotword_limit_for_docs(),
        );
        assert_eq!(
            OPENAI_KEYWORD_CAP,
            crate::providers::asr_openai_realtime::keyword_limit_for_docs(),
        );
        assert_eq!(
            GEMINI_PROMPT_CAP,
            crate::providers::asr_gemini::hotword_limit_for_docs(),
        );
    }

    /// 這份清單和 `registry.rs::cloud_transcribe` 實際認識的 key 必須是同一個集合。
    ///
    /// 兩個檔案相鄰只是習慣，不是機制 —— 真正攔住漂移的是這條。少一個：新 provider
    /// 的熱詞說明會是 UnknownProvider；多一個：清單裡留著已經刪掉的 key，
    /// 上面那條遍歷測試會替一個不存在的 provider 站崗，給人虛假的安全感。
    ///
    /// **對照來自原始碼，不是手抄副本。** `dispatch_keys_for_test` 掃的是
    /// `cloud_transcribe` 的 match 臂（見那個函式的註釋）。這條是整套機制的落點：
    /// 對照本身若還是手寫的，那就只能測出"兩份副本抄得一致"，改真實分發照樣全綠。
    #[test]
    fn the_declaration_list_matches_the_dispatch_table() {
        let mut declared: Vec<&str> = ALL_ASR_PROVIDERS.to_vec();
        let mut dispatched: Vec<&str> = crate::providers::registry::dispatch_keys_for_test();
        declared.sort_unstable();
        dispatched.sort_unstable();
        assert_eq!(
            declared, dispatched,
            "capabilities.rs 的宣告清單和 registry.rs 的分發 key 不一致",
        );
    }

    /// 序列化出去的欄位名要是前端認的 camelCase / snake_case。
    #[test]
    fn it_serializes_in_the_shape_the_frontend_expects() {
        let json = serde_json::to_value(cap("qwen_audio_stream")).unwrap();
        assert_eq!(json["streaming"], "vocabulary");
        assert_eq!(json["buffered"], "vocabulary");
        assert_eq!(json["hasStreamingPath"], true);
        assert_eq!(json["streamingClientCap"], 100);
        assert_eq!(json["bufferedClientCap"], 100);

        let groq = serde_json::to_value(cap("groq_whisper")).unwrap();
        assert_eq!(groq["buffered"], "not_wired_up");
        assert_eq!(groq["bufferedClientCap"], Value::Null);

        // 兩條路徑上限不同的那家，序列化出去必須是兩個不同的值
        let openai = serde_json::to_value(cap("openai_live_transcribe")).unwrap();
        assert_eq!(openai["streamingClientCap"], 100);
        assert_eq!(openai["bufferedClientCap"], Value::Null);
    }

    /// 對照表覆蓋每一個分發 key，且沒有一行是 `UnknownProvider`。
    ///
    /// 這是「表格是實現的投影」這件事的落點：表不是手寫的，所以不可能漏一家。
    /// 上面那條 `every_dispatch_key_...` 保的是宣告完整，這條保的是**匯出**完整 ——
    /// 兩者分開，因為矩陣是另一條程式碼路徑（漏掉某個 key 不會讓前一條紅）。
    #[test]
    fn the_reference_matrix_covers_every_provider() {
        let matrix = asr_hotword_capability_matrix();
        assert_eq!(matrix.len(), ALL_ASR_PROVIDERS.len());
        for provider in ALL_ASR_PROVIDERS {
            let row = matrix
                .get(*provider)
                .unwrap_or_else(|| panic!("對照表缺了 {}", provider));
            assert_ne!(row.buffered, HotwordDelivery::UnknownProvider, "{}", provider);
            assert_ne!(row.streaming, HotwordDelivery::UnknownProvider, "{}", provider);
        }
    }

    /// 對照表裡的 `openai_compat` 必須是「取決於協議」。
    ///
    /// 它沒有具體配置可依，只能這麼說。這條釘的是**不要圖省事給它填一個具體答案** ——
    /// 填 `NotWiredUp`（多數自建服務確實走 transcriptions）會讓接百煉那類走 chat 的
    /// 使用者在表裡讀到錯的結論。
    #[test]
    fn the_matrix_leaves_the_compat_card_undecided() {
        let matrix = asr_hotword_capability_matrix();
        assert_eq!(
            matrix["openai_compat"].buffered,
            HotwordDelivery::UndecidedProtocol,
        );
    }
}
