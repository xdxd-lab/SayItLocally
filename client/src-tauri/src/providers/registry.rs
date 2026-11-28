// 供應商登錄檔 — Tauri commands 入口

use super::types::*;
use super::{
    ai_ollama, ai_openai_compat, asr_doubao, asr_doubao_stream, asr_gemini, asr_gemini_live,
    asr_groq, asr_mimo, asr_openai_chat_audio, asr_openai_compat, asr_openai_realtime,
    asr_openrouter, asr_qwen, asr_qwen_audio_stream, asr_qwen_omni, asr_qwen_realtime,
};
use crate::error_protocol;

/// [自訂] S34／S38／S42／S22：AI 只允許本機 Ollama，在 Rust 端強制。
///
/// provider 必須是 "ollama"，且 api_url 的主機必須是 127.0.0.1、localhost 或 ::1
/// （見 net_policy::is_loopback_ollama_url）。其餘一律拒絕，不發出任何連線。
/// 另外保留 DeepSeek 黑名單作為額外防線。前端預設值、托盤／快捷鍵的 AI 開關、
/// 匯入設定都繞不過這一關。
fn ensure_local_ollama_only(provider: &str, api_url: &str) -> Result<(), String> {
    if provider != "ollama" {
        return Err(error_protocol::encode(
            "connect_failed",
            format!(
                "AI provider '{}' is disabled in this build: only a local Ollama on 127.0.0.1/localhost/::1 is allowed",
                provider
            ),
        ));
    }
    if ai_openai_compat::is_forbidden_ai_host(api_url) {
        return Err(error_protocol::encode(
            "connect_failed",
            "DeepSeek endpoints are forbidden in this build",
        ));
    }
    if !crate::net_policy::is_loopback_ollama_url(api_url) {
        return Err(error_protocol::encode(
            "connect_failed",
            "Ollama URL must be http(s)://127.0.0.1, localhost or [::1] in this build (e.g. http://127.0.0.1:11434)",
        ));
    }
    Ok(())
}

/// 雲端 AI 校對（Tauri command）
#[tauri::command]
pub async fn cloud_polish(request: CloudPolishRequest) -> Result<AiResult, String> {
    let config = &request.ai_config;
    // [自訂] S34／S42：先過本機 Ollama 守衛，不符合就不連線。
    ensure_local_ollama_only(&config.provider, &config.api_url)?;
    match config.provider.as_str() {
        // Groq 與智譜都是標準的 OpenAI 相容 chat/completions，直接複用通用實現，
        // 不需要單獨的檔案（base_url 已帶版本段 /v1、/v4，normalize_base_url 會原樣保留）。
        //
        // ⚠️ 這個清單是白名單：前端 AI_PROVIDERS 裡新增一家卻漏了這裡，使用者會拿到
        // 「Unknown AI provider: xxx」，而那句話看不出問題在路由層。兩處必須一起改。
        //
        // [自訂] S34／S22 已註解停用雲端 AI 分支（含 provider 名稱 "deepseek"）：
        // "openai_compat" | "deepseek" | "doubao" | "qwen" | "mimo" | "groq" | "zhipu"
        // | "openrouter" => {
        //     ai_openai_compat::polish(
        //         &request.text,
        //         config,
        //         request.system_prompt.as_deref(),
        //         request.text_context.as_ref(),
        //     )
        //     .await
        // }
        // [自訂] SC-67：AI 校對的輸出同樣轉成台灣正體（含假名時視為日文不轉）。
        "ollama" => ai_ollama::polish(
            &request.text,
            config,
            request.system_prompt.as_deref(),
            request.text_context.as_ref(),
        )
        .await
        .map(finalize_polish),
        other => Err(error_protocol::encode(
            "connect_failed",
            format!("Unknown AI provider: {}", other),
        )),
    }
}

/// [自訂] SC-67：AI 校對的回傳結果。文字轉成台灣正體（含假名的句子視為日文不轉）。
fn finalize_polish(mut result: AiResult) -> AiResult {
    result.text = crate::zh_output::to_taiwan(&result.text, "auto");
    result
}

/// 測試 AI 連線（Tauri command）
#[tauri::command]
pub async fn test_ai_connection(config: AiProviderConfig) -> Result<TestResult, String> {
    // [自訂] S20／S34：「檢查連線」同樣只允許本機 Ollama，不會把金鑰送到雲端。
    ensure_local_ollama_only(&config.provider, &config.api_url)?;
    match config.provider.as_str() {
        // 與 cloud_polish 的清單必須一致：只加一處會出現「測試通了但校對報未知供應商」
        // 或者反過來，兩種都很難從錯誤資訊看出是路由問題。
        // [自訂] S34／S22 已註解停用雲端 AI 分支：
        // "openai_compat" | "deepseek" | "doubao" | "qwen" | "mimo" | "groq" | "zhipu"
        // | "openrouter" => {
        //     Ok(ai_openai_compat::test_connection(&config).await)
        // }
        "ollama" => Ok(ai_ollama::test_connection(&config).await),
        other => Err(error_protocol::encode(
            "connect_failed",
            format!("Unknown AI provider: {}", other),
        )),
    }
}

/// 同 cloud_transcribe，但音訊以原始 PCM 位元組發過來（見 ipc_body）。
///
/// 各家介面本身要 base64（JSON 請求體），所以在這裡轉：Rust 轉 10 MB 是幾毫秒，
/// 網頁裡那個逐位元組拼字串的轉碼要 0.7 秒，還跑在主窗主執行緒上。
#[tauri::command]
pub async fn cloud_transcribe_raw(request: tauri::ipc::Request<'_>) -> Result<AsrResult, String> {
    use base64::Engine;
    let (mut meta, pcm): (CloudTranscribeRequest, Vec<u8>) = crate::ipc_body::split_framed(&request)?;
    meta.audio_b64 = base64::engine::general_purpose::STANDARD.encode(&pcm);
    cloud_transcribe(meta).await
}

/// 雲端 ASR 轉寫（Tauri command）
#[tauri::command]
pub async fn cloud_transcribe(request: CloudTranscribeRequest) -> Result<AsrResult, String> {
    let config = &request.asr_config;
    match config.provider.as_str() {
        "doubao" => {
            asr_doubao::transcribe(
                &request.audio_b64,
                request.sample_rate,
                config,
                &request.hotwords,
            )
            .await
        }
        "doubao_v2" => {
            asr_doubao_stream::transcribe(
                &request.audio_b64,
                request.sample_rate,
                config,
                &request.hotwords,
            )
            .await
        }
        "qwen" | "aliyun" => {
            asr_qwen::transcribe(
                &request.audio_b64,
                request.sample_rate,
                config,
                &request.hotwords,
            )
            .await
        }
        // qwen3-asr-flash-realtime 關掉即時字幕、或流式沒連上時的錄完再發。
        // ⚠️ 別並回上面那個分支：asr_qwen 的模型寫死 qwen3-asr-flash，使用者選 realtime
        // 卻被另一個模型識別、計費，介面上看不出來（2026-10 使用者反饋後才發現）。
        "qwen_realtime" => {
            asr_qwen_realtime::transcribe(
                &request.audio_b64,
                request.sample_rate,
                config,
                &request.hotwords,
            )
            .await
        }
        // Qwen-Audio-ASR-Flash 流式（3.0 / 3.1 共用，具體哪代看 extra.model）：
        // 關掉即時字幕、以及設定頁的識別測試都走這條一次性路徑。
        // 用的仍是同一個 duplex 協議，只是整段音訊推完再收結果。
        "qwen_audio_stream" => {
            asr_qwen_audio_stream::transcribe(
                &request.audio_b64,
                request.sample_rate,
                config,
                &request.hotwords,
            )
            .await
        }
        "qwen_omni" => {
            asr_qwen_omni::transcribe(
                &request.audio_b64,
                request.sample_rate,
                config,
                &request.hotwords,
            )
            .await
        }
        "mimo" => {
            asr_mimo::transcribe(
                &request.audio_b64,
                request.sample_rate,
                config,
                &request.hotwords,
            )
            .await
        }
        // Groq 與 OpenAI 官方共用一份 /audio/transcriptions 實現，
        // 具體打哪個域名、用哪個預設模型由 asr_groq::endpoint_for 按 provider 分。
        //
        // openai_live_transcribe 也走這裡：那是它**關掉即時字幕**時的路徑。
        // 流式模型（gpt-live-transcribe）在檔案端點上不存在，所以 endpoint_for 給它
        // 單獨一檔，把模型換成 gpt-transcribe —— 用 WebSocket 傳一段已經錄完的音訊
        // 沒有任何好處，一次 HTTP 更簡單也更快。
        //
        // openai_compat_transcribe 是同一套協議、地址由使用者填的那一檔
        // （自建 whisper.cpp / faster-whisper / FunASR、聚合閘道器）。
        "groq_whisper" | "openai_transcribe" | "openai_live_transcribe"
        | "openai_compat_transcribe" => {
            asr_groq::transcribe(
                &request.audio_b64,
                request.sample_rate,
                config,
                &request.hotwords,
            )
            .await
        }
        // 「OpenAI 相容」那張卡：協議由 asr_openai_compat 探測後再分發到上下兩組之一。
        // 使用者在設定裡手動指定過協議時它直接照辦，不探測。
        "openai_compat" => {
            asr_openai_compat::transcribe(
                &request.audio_b64,
                request.sample_rate,
                config,
                &request.hotwords,
            )
            .await
        }
        // chat/completions + input_audio 那一套。**與上面那組不是同一個協議**，
        // 別看著都叫「OpenAI 相容」就並過去（地址、請求體、響應形狀全不一樣）。
        // qwen_chat_audio 地址內建為百煉（qwen3.8-omni-flash 走這條）；
        // openai_chat_audio 是 asr_openai_compat 分發下來的內部 key。
        "qwen_chat_audio" | "openai_chat_audio" | "openai_chat_audio_standard" => {
            asr_openai_chat_audio::transcribe(
                &request.audio_b64,
                request.sample_rate,
                config,
                &request.hotwords,
            )
            .await
        }
        // Gemini 同理：流式那張卡關掉字幕就走檔案轉寫，模型換回 gemini-3.5-transcribe。
        "gemini_transcribe" | "gemini_live_transcribe" => {
            asr_gemini::transcribe(
                &request.audio_b64,
                request.sample_rate,
                config,
                &request.hotwords,
            )
            .await
        }
        // OpenRouter 是路由層：一把 key 通往多家 STT 模型，具體哪家由模型 slug 決定。
        // 它自己的 JSON 形態（input_audio + base64），不是 multipart —— 別並進上面
        // asr_groq 那一組，理由見 asr_openrouter.rs 的檔案頭。
        "openrouter_transcribe" => {
            asr_openrouter::transcribe(
                &request.audio_b64,
                request.sample_rate,
                config,
                &request.hotwords,
            )
            .await
        }
        other => Err(error_protocol::encode(
            "connect_failed",
            format!("ASR provider \"{}\" is not implemented", other),
        )),
    }
}

/// 上面那個 `cloud_transcribe` 的 match 認識的全部 key，**從它的原始碼裡讀出來**。
///
/// 存在的理由是給 `capabilities.rs` 的測試當對照：熱詞能力宣告必須覆蓋每一個分發
/// key，漏一個就會把「我們沒宣告」顯示成「這家不支援熱詞」。
///
/// ## 為什麼不是一份手寫清單
///
/// 原來這裡是手抄的 `&["doubao", "doubao_v2", …]`。那樣寫的問題是**它和真實分發之間
/// 沒有任何關係**：改 `cloud_transcribe` 的 match 而不動這份清單，「宣告與分發一致」
/// 那條測試照樣全綠 —— 它比對的是兩份手寫副本，測的是"我抄得一致嗎"，
/// 不是"宣告覆蓋了真實分發嗎"。新增和刪除分支都漏得掉。
///
/// 所以改成掃原始碼。Rust 沒有反射，但 `include_str!` 拿得到本檔案的內容，而 match 臂
/// 的形狀在 rustfmt 下是穩定的（臂頭 trim 後以 `"` 開頭，續行以 `|` 開頭）。
///
/// ## 它壞掉的時候不會靜默
///
/// 掃描器的任何失效都會讓 key 集合變得不對（多抓、少抓、空集），而呼叫方做的是
/// **集合相等**斷言 —— 一律當場紅。這是選它而不是選"宏生成 match"的原因：
/// 後者要把這段帶大量註釋的 match 塞進宏，可讀性代價遠大於收益。
#[cfg(test)]
pub fn dispatch_keys_for_test() -> Vec<&'static str> {
    let keys = match_arm_keys(fn_body(include_str!("registry.rs"), CLOUD_TRANSCRIBE_SIGNATURE));
    // 掃描器自己活著的訊號。真實 match 有十幾條臂，抓到個位數只可能是掃描邏輯壞了
    // （比如 rustfmt 換了臂頭縮排風格），這時要修的是掃描器，不是宣告清單。
    assert!(
        keys.len() >= 10,
        "從 cloud_transcribe 原始碼裡只抓到 {} 個分發 key，掃描邏輯多半已經失效：{:?}",
        keys.len(),
        keys,
    );
    keys
}

/// 刻意用 `concat!` 拼出來而不是寫成一整個字面量：`fn_body` 找的是**第一次**出現，
/// 而這行常量定義本身也在同一個檔案裡。寫全了就等於在原始碼裡埋下第二個匹配點，
/// 哪天這行挪到 `cloud_transcribe` 上面去，截出來的就是這行所在的位置。
#[cfg(test)]
// 帶上左括號：同檔案裡還有 cloud_transcribe_raw，只按字首找會先找到它。
const CLOUD_TRANSCRIBE_SIGNATURE: &str = concat!("pub async fn ", "cloud_transcribe", "(");

/// 截出某個頂層函式的原始碼片段。
///
/// 結束判據是**頂格的 `}`**（`"\n}"`），不是花括號配對計數：頂層 `fn` 的收尾花括號
/// 在第 0 列，而函式體內部的每一個 `}` 都有縮排。計數法會被字串字面量裡的花括號
/// （這個函式體裡就有 `format!("… \"{}\" …")`）帶偏，判據反而更脆。
#[cfg(test)]
fn fn_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("在原始碼裡找不到 `{signature}`，簽名被改過？"));
    let rest = &source[start..];
    let end = rest.find("\n}").map(|at| at + 1).unwrap_or(rest.len());
    &rest[..end]
}

/// 從一段原始碼裡提取 match 臂上的字串字面量 key。
///
/// 只認「trim 後以 `"` 或 `|` 開頭、直到出現 `=>`」的行序列，所以：
///   · `"qwen" | "aliyun" => {` —— 一行抓兩個；
///   · 臂頭換行寫的（`"groq_whisper" | …` 換行後 `| "openai_compat_transcribe" => {`）
///     靠累積緩衝抓全；
///   · `other => Err(…)` 那條兜底臂不以引號開頭，不會被抓（它不是一個分發 key）；
///   · 臂體裡恰好頂行寫著字串的（`"connect_failed",`）會進緩衝，但它後面那行不以
///     引號或 `|` 開頭，緩衝隨即清空，不會被誤算成 key。
#[cfg(test)]
fn match_arm_keys<'a>(source: &'a str) -> Vec<&'a str> {
    let mut keys: Vec<&'a str> = Vec::new();
    // 攢的是行切片而不是拼接 String：這樣提取出來的 key 天然藉著原文的生命週期，
    // 不需要為了還原它再回原文查一次。
    let mut head: Vec<&'a str> = Vec::new();
    for raw in source.lines() {
        // 剝行註釋：註釋裡提到別家 provider 名是常事（這個 match 的註釋裡就有），
        // 不剝會誤抓。代價是函式體裡不能有含 `//` 的字串字面量（目前沒有），
        // 真有的話結果是少抓一個 key —— 呼叫方的集合相等斷言會紅，不會靜默。
        let line = match raw.find("//") {
            Some(at) => &raw[..at],
            None => raw,
        };
        let line = line.trim();
        // 空行（含被剝空的純註釋行）既不累積也不打斷：臂頭中間插一行註釋仍能抓全。
        if line.is_empty() {
            continue;
        }
        if !(line.starts_with('"') || line.starts_with('|')) {
            head.clear();
            continue;
        }
        // `=>` 之前的部分才是臂頭，之後是臂體（`"key" => Ok(…)` 那種單行臂）。
        let (fragment, complete) = match line.find("=>") {
            Some(at) => (&line[..at], true),
            None => (line, false),
        };
        head.push(fragment);
        if complete {
            for part in head.drain(..) {
                keys.extend(quoted_literals(part));
            }
        }
    }
    keys
}

/// 取出一段文本裡所有雙引號包住的內容。
#[cfg(test)]
fn quoted_literals<'a>(text: &'a str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('"') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find('"') else { break };
        if close > 0 {
            out.push(&rest[..close]);
        }
        rest = &rest[close + 1..];
    }
    out
}

/// 測試 ASR 連線（Tauri command）
#[tauri::command]
pub async fn test_asr_connection(config: AsrProviderConfig) -> Result<TestResult, String> {
    match config.provider.as_str() {
        "doubao" => Ok(asr_doubao::test_connection(&config).await),
        "doubao_v2" => Ok(asr_doubao_stream::test_connection(&config).await),
        "qwen" | "aliyun" => Ok(asr_qwen::test_connection(&config).await),
        "qwen_realtime" => Ok(asr_qwen_realtime::test_connection(&config).await),
        "qwen_audio_stream" => Ok(asr_qwen_audio_stream::test_connection(&config).await),
        "qwen_omni" => Ok(asr_qwen_omni::test_connection(&config).await),
        "mimo" => Ok(asr_mimo::test_connection(&config).await),
        "groq_whisper" | "openai_transcribe" | "openai_compat_transcribe" => {
            Ok(asr_groq::test_connection(&config).await)
        }
        "qwen_chat_audio" | "openai_chat_audio" | "openai_chat_audio_standard" => {
            Ok(asr_openai_chat_audio::test_connection(&config).await)
        }
        "openai_compat" => Ok(asr_openai_compat::test_connection(&config).await),
        "gemini_transcribe" => Ok(asr_gemini::test_connection(&config).await),
        "openrouter_transcribe" => Ok(asr_openrouter::test_connection(&config).await),
        // 流式那兩家的連通性測試打的是**流式那條連線**（而不是它們回落用的 HTTP）：
        // 這個命令要回答的是"這份配置能不能用"，而它們平時用的就是 WebSocket。
        // HTTP 那條路由設定頁的識別測試覆蓋（見 cloud_transcribe 的分支）。
        "openai_live_transcribe" => Ok(asr_openai_realtime::test_connection(&config).await),
        "gemini_live_transcribe" => Ok(asr_gemini_live::test_connection(&config).await),
        other => Err(error_protocol::encode(
            "connect_failed",
            format!("ASR provider \"{}\" is not implemented", other),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalize_polish_returns_taiwan_traditional_text() {
        // 「軟體」的簡體寫法 + 日文句子：只轉中文句。
        let result = finalize_polish(AiResult {
            text: "\u{8f6f}\u{4ef6}\u{3002}\u{4eca}\u{65e5}\u{306f}\u{3002}".into(),
            elapsed_ms: 7,
        });
        assert_eq!(result.text, "軟體。\u{4eca}\u{65e5}\u{306f}\u{3002}");
        assert_eq!(result.elapsed_ms, 7);
    }

    /// 一段人造的分發函式，形狀覆蓋真實 match 裡出現過的每一種臂。
    ///
    /// 用人造原始碼而不是真實原始碼來驗掃描器本身：真實原始碼會變，而"刪一條臂就該少一個
    /// key"這件事必須在一個可控的輸入上斷言，否則測試會跟著業務改動一起漂。
    const FAKE_DISPATCH: &str = r#"pub async fn fake_dispatch(x: &str) -> Result<(), String> {
    match x {
        "alpha" => Ok(()),
        // 註釋裡提到 "ghost" 是常事，不能被當成分發 key
        "beta" | "gamma" => Ok(()),
        "delta"
        | "epsilon" => Ok(()),
        other => Err(encode(
            "connect_failed",
            format!("no {}", other),
        )),
    }
}
"#;

    fn fake_keys(source: &str) -> Vec<&str> {
        match_arm_keys(fn_body(source, "pub async fn fake_dispatch"))
    }

    /// 掃描器要認得真實 match 裡出現過的全部臂形狀，並且只認臂頭。
    #[test]
    fn the_scanner_handles_every_arm_shape_and_nothing_else() {
        assert_eq!(
            fake_keys(FAKE_DISPATCH),
            vec!["alpha", "beta", "gamma", "delta", "epsilon"],
        );
    }

    /// 註釋裡的 provider 名不能被抓走。
    ///
    /// 這不是假想：`cloud_transcribe` 的註釋裡就寫著別家的 key（"openai_compat_transcribe
    /// 是同一套協議…"）。不剝註釋的話對照集合會憑空多出幾個，而多出來的那幾個
    /// 會讓「宣告清單」被迫去宣告不存在的分發分支。
    #[test]
    fn the_scanner_ignores_provider_names_inside_comments() {
        assert!(!fake_keys(FAKE_DISPATCH).contains(&"ghost"));
    }

    /// 兜底臂和臂體裡的字串都不是分發 key。
    #[test]
    fn the_scanner_skips_the_fallback_arm_and_arm_bodies() {
        // `other => Err(…)` 不是一個 provider
        assert!(!fake_keys(FAKE_DISPATCH).contains(&"other"));
        // `"connect_failed"` 頂行寫在臂體裡，形狀上很像臂頭
        assert!(!fake_keys(FAKE_DISPATCH).contains(&"connect_failed"));
    }

    /// **刪掉一條分發分支，對照集合必須立刻少掉那些 key。**
    ///
    /// 這是稽核指出的那個洞：原來的對照是另一份手寫清單，刪掉真實分支而保留清單，
    /// 「宣告與分發一致」那條測試照樣綠。現在對照來自原始碼，刪除是可見的。
    #[test]
    fn removing_a_dispatch_branch_shrinks_the_reference_set() {
        let without = FAKE_DISPATCH.replace("        \"beta\" | \"gamma\" => Ok(()),\n", "");
        assert_ne!(without, FAKE_DISPATCH, "替換沒生效，測試自己失效了");

        let keys = fake_keys(&without);
        assert!(!keys.contains(&"beta"));
        assert!(!keys.contains(&"gamma"));
        assert_eq!(keys, vec!["alpha", "delta", "epsilon"]);
    }

    /// **新增一條分發分支，對照集合必須立刻多出那個 key。**
    ///
    /// 配合 `capabilities.rs` 的集合相等斷言，效果是：加一家 provider 只改
    /// `cloud_transcribe` 而忘了宣告熱詞行為，測試當場紅。
    #[test]
    fn adding_a_dispatch_branch_grows_the_reference_set() {
        let with = FAKE_DISPATCH.replace(
            "        \"alpha\" => Ok(()),\n",
            "        \"alpha\" => Ok(()),\n        \"zeta\" => Ok(()),\n",
        );
        assert_ne!(with, FAKE_DISPATCH, "替換沒生效，測試自己失效了");
        assert!(fake_keys(&with).contains(&"zeta"));
    }

    /// 真實原始碼上的行為錨點。
    ///
    /// 刻意**不寫成"等於某份清單"** —— 那就又變成手抄副本了。這裡只釘三件
    /// 掃描器容易做錯的事：跨行臂頭要抓全、單 key 臂要抓到、臂體裡的字串不能混進來。
    #[test]
    fn the_real_dispatch_table_is_read_from_source() {
        let keys = dispatch_keys_for_test();
        // 跨行臂頭：這個 key 寫在 `| "openai_compat_transcribe" => {` 那行的續行上
        assert!(
            keys.contains(&"openai_compat_transcribe"),
            "跨行寫的臂頭沒抓全：{keys:?}",
        );
        // 單 key 臂
        assert!(keys.contains(&"openai_compat"), "{keys:?}");
        // 臂體裡的字串不是分發 key
        assert!(!keys.contains(&"connect_failed"), "{keys:?}");
        // 同一個 key 不該被抓兩次（集合相等斷言會因為重複項失敗，先在這裡定性）
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len(), "抓到了重複的分發 key：{keys:?}");
    }
}
