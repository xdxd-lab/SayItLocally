// Google Gemini 轉寫 — gemini-3.5-transcribe
//
// 走 Gemini API 的 `POST /v1beta/models/{model}:generateContent`，音訊以
// `inline_data` 內聯上傳（base64 的 WAV）。
//
// ── 為什麼是 Gemini API 而不是 Cloud Speech-to-Text（Chirp 3）──
// Chirp 3 的準確率也在第一檔，但它在 Google Cloud 那套體系裡：要建 GCP 專案、開
// Speech-to-Text API、拿服務帳號 JSON、走 OAuth2 換 access token（還要處理過期重新整理）。
// 我們這一頁的前提是「粘一把 API Key 就能用」—— 讓使用者去配服務帳號，絕大多數人到不了
// 能用的那一步。Gemini API 只要一個 `x-goog-api-key`，和別的供應商一樣。
//
// ── 為什麼用 generateContent 而不是新的 Interactions API ──
// 官方現在把 generateContent 標成 legacy、推薦 Interactions。但 legacy 仍然可用，
// 形狀簡單（一個 POST、一個 JSON），而 Interactions 換來的能力（多輪、工具）
// 我們一條都用不上。等 legacy 真要下線再遷。
//
// ── MEASURED 2026-09-16（`dev-scripts/probe_new_asr_providers.py --target gemini-file`，
//    打的是真實介面，用一個**沒有繫結結算資訊**的 AI Studio 專案）──
//   · 免費層可用：HTTP 200、`serviceTier: standard`，3.05s 中文音訊 2113ms 返回。
//     不需要綁帳單。（綁過帳單又欠費的專案反而不行：那種會 429
//     "prepayment credits are depleted"，因為它已經離開免費層了。）
//   · **響應形狀與通用模型不同**：轉寫專用模型把結果放在
//     `parts[0].audioTranscription.text`，不是 `parts[0].text`。
//     照通用模型那樣只讀 `text` 會得到「HTTP 200 但一個字都沒有」——
//     而 finishReason=STOP、promptFeedback=null，看上去毫無異常。見 extract_text。
//   · `audio/wav` + inline_data 被接受（原 note B 確認）。
//   · 模型不答話、不加字首，直接給出帶句尾標點的純轉寫（原 note C 確認）。
//     它的返回欄位本身就是專用的 audioTranscription，說明這個模型壓根不走"回答"那條路。
//   · token 計費：3.05s 音訊 = 77 audio tokens，我們這段 prompt = 66 text tokens。
//     prompt 已經和音訊一個量級了，往裡加東西前先想想值不值。
//
// ── 仍未驗證的一處 ──
//   A. **熱詞與語種有沒有專用欄位**。官方宣傳裡有 custom vocabulary biasing 與
//      語種檢測，但公開文件沒給出 generateContent 下的欄位名。這裡一律寫進 prompt
//      文本 —— 對 Gemini 這種指令跟隨的模型 prompt 本來就有效，只是不如專用欄位精確，
//      而且佔 token（見上面那條）。若實測發現有專用欄位（如 `speechConfig`），
//      改成用它更好。

use super::diag;
use super::types::{AsrProviderConfig, AsrResult, TestResult};
use std::time::Instant;

const API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const SCOPE: &str = "gemini/asr";
const DEFAULT_MODEL: &str = "gemini-3.5-transcribe";

/// 轉寫指令。
///
/// **這不是可選的潤色，缺了它模型可能去"回答"音訊內容。** 與 Whisper 那種
/// 「拿帶標點的句子做示範」不同，Gemini 是指令跟隨的，所以這裡寫的是明確要求。
///
/// 每一條都對應一種實際會出錯的行為：
///   · 「只輸出轉寫文本」—— 不然會得到「這段音訊裡說話人提到……」這樣的描述；
///   · 「不要加解釋或前字尾」—— 不然會出現「以下是轉寫內容：」這種開場白，
///     而它會被原樣插進使用者的文件；
///   · 「聽不出內容就返回空」—— 不然會編一段合理的話（幻聽），這比沒有結果更糟。
const TRANSCRIBE_PROMPT: &str = "Transcribe this audio verbatim. Output only the transcript text with natural punctuation. Do not add explanations, prefixes, quotation marks, timestamps, or speaker labels. If the audio contains no intelligible speech, output nothing at all.";

/// 熱詞條數上限。全部塞進 prompt，給太多會稀釋每一個的作用，也會把 prompt 撐長。
///
/// ⚠️ 這是**客戶端自設**的截斷，介面必須說出來。漏說的代價是靜默：配了 150 個詞
/// 只發 100 個，而熱詞頁既不顯示上限、又夠不到 `HOTWORD_SOFT_LIMIT`（200）那條
/// 通用提醒，使用者完全看不到後 50 個被丟掉了。宣告處是 `capabilities.rs::client_cap`。
const HOTWORD_LIMIT: usize = 100;

/// 給 `capabilities.rs` 的測試用，讓那邊的說明值和這裡的請求引數釘在一起。
/// 只在測試裡存在：說明和請求引數刻意各留一份，理由見 capabilities.rs。
#[cfg(test)]
pub fn hotword_limit_for_docs() -> usize {
    HOTWORD_LIMIT
}

/// 將 16kHz 單聲道 16-bit PCM 封裝為 WAV 容器。
///
/// 與 asr_groq.rs / asr_mimo.rs 裡那兩份是同一個 WAV 頭，刻意各留一份：
/// 抽成公共函式後任何一家改取樣格式都會牽動另外兩家，而它們本來毫無關係。
fn pcm_to_wav(pcm: &[u8], sr: u32) -> Vec<u8> {
    let ds = pcm.len() as u32;
    let mut w = Vec::with_capacity(44 + pcm.len());
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + ds).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&sr.to_le_bytes());
    w.extend_from_slice(&(sr * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&ds.to_le_bytes());
    w.extend_from_slice(pcm);
    w
}

/// 流式那張卡的 provider id。它關掉即時字幕時會回落到這條 HTTP 路徑。
const LIVE_PROVIDER: &str = "gemini_live_transcribe";

/// 本次要用的模型：前端選定的優先，否則預設。
///
/// **`gemini_live_transcribe` 的選定模型在這裡一律忽略。** 那張卡選的是流式模型
/// （gemini-3.5-transcribe-live），generateContent 上沒有這個模型；關掉即時字幕
/// 回落到這條路時照發只會換來一個含義模糊的 404/400，而使用者完全不知道自己發了
/// 一個檔案端點不認識的模型。同一個取捨在 asr_groq.rs 用 honors_selected_model 表達。
fn resolve_model(config: &AsrProviderConfig) -> String {
    if config.provider == LIVE_PROVIDER {
        return DEFAULT_MODEL.to_string();
    }
    config
        .extra
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_MODEL)
        .to_string()
}

/// 識別語言：設定裡存的是 auto|zh|en|…，auto 時**整句不提語言**。
///
/// 不能寫「language: auto」之類的字樣 —— 那是給模型看的自然語言，
/// 寫一個它不認識的語言名只會讓它困惑。auto 就是讓它自己判斷。
fn language_hint(config: &AsrProviderConfig) -> Option<String> {
    let raw = config
        .extra
        .get("language")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("auto");
    match raw {
        "auto" => None,
        other => Some(format!(" The audio is in this language: {}.", other)),
    }
}

/// 組 prompt：轉寫指令 + 可選語言 + 可選熱詞。
fn build_prompt(config: &AsrProviderConfig, hotwords: &[String]) -> String {
    let mut prompt = String::from(TRANSCRIBE_PROMPT);
    if let Some(lang) = language_hint(config) {
        prompt.push_str(&lang);
    }
    let mut seen = std::collections::HashSet::new();
    let words: Vec<&str> = hotwords
        .iter()
        .map(|w| w.trim())
        .filter(|w| !w.is_empty())
        .filter(|w| seen.insert(w.to_lowercase()))
        .take(HOTWORD_LIMIT)
        .collect();
    if !words.is_empty() {
        diag::log(
            SCOPE,
            "hotwords",
            &format!("vocabulary={} of={}", words.len(), hotwords.len()),
        );
        prompt.push_str(
            " These terms may appear in the audio; spell them exactly as written here: ",
        );
        prompt.push_str(&words.join(", "));
        prompt.push('.');
    }
    prompt
}

fn build_body(prompt: &str, wav_b64: String) -> serde_json::Value {
    serde_json::json!({
        "contents": [{
            "role": "user",
            "parts": [
                { "text": prompt },
                { "inline_data": { "mime_type": "audio/wav", "data": wav_b64 } }
            ]
        }],
        // temperature 0：轉寫要的是照實還原，不是多樣性。
        "generationConfig": { "temperature": 0 }
    })
}

/// 從響應裡取文本。
///
/// **兩種 part 形狀都要認**（實測 2026-09-16，見檔案頭 MEASURED）：
///   · 轉寫專用模型 `gemini-3.5-transcribe` 給的是
///     `{"audioTranscription": {"text": "語音輸入法測試成功。"}}`；
///   · 通用模型（gemini-*-flash 那些）給的是普通的 `{"text": "..."}`。
///
/// 只認後者的代價極其隱蔽：HTTP 200、`finishReason: STOP`、`promptFeedback: null`，
/// 一切正常，就是提不出字 —— 前端把它顯示成「未檢測到有效聲音」，
/// 使用者會去查麥克風（pitfalls 第 15 條）。第一次實測就是這麼栽的。
///
/// parts 可能有多段，要全部拼起來 —— 只取 `parts[0]` 會在模型分段輸出時丟掉後半截。
fn extract_text(data: &serde_json::Value) -> String {
    let Some(parts) = data
        .pointer("/candidates/0/content/parts")
        .and_then(|p| p.as_array())
    else {
        return String::new();
    };
    parts
        .iter()
        .filter_map(|p| {
            p.pointer("/audioTranscription/text")
                .and_then(|t| t.as_str())
                .or_else(|| p.get("text").and_then(|t| t.as_str()))
        })
        .collect::<Vec<_>>()
        .join("")
        .trim()
        .to_string()
}

/// 為什麼這次沒有文本 —— 內容審查攔截、模型自己停在了別的原因上，
/// 或者**響應裡有 parts 但我們一個欄位都不認識**。
///
/// 不區分的話，這些全都會顯示成「未檢測到有效聲音」，把使用者引去查麥克風。
/// 兩種要單獨說清的：
///   · safety 攔截 —— 這段音訊永遠不會有結果，重試沒有意義；
///   · 認不出的 part 欄位 —— 那是**我們的 bug**，不是使用者的音訊問題。
///     第一次實測就栽在這個上（專用模型用 audioTranscription 而不是 text），
///     而當時只報了 finishReason=STOP，看上去像一切正常但就是沒說話。
///     把 part 的鍵名列出來，下次照著改 extract_text 就行。
fn describe_empty(data: &serde_json::Value) -> String {
    if let Some(reason) = data
        .pointer("/promptFeedback/blockReason")
        .and_then(|r| r.as_str())
    {
        return format!("blocked by content filter: {}", reason);
    }
    let finish = data
        .pointer("/candidates/0/finishReason")
        .and_then(|r| r.as_str());
    if let Some(parts) = data
        .pointer("/candidates/0/content/parts")
        .and_then(|p| p.as_array())
    {
        if !parts.is_empty() {
            let keys: Vec<&str> = parts
                .iter()
                .filter_map(|p| p.as_object())
                .flat_map(|o| o.keys().map(|k| k.as_str()))
                .collect();
            return format!(
                "response had {} part(s) but no field we recognize (keys: {}) finishReason={} \
                 -- this is our parsing bug, not an audio problem",
                parts.len(),
                keys.join(","),
                finish.unwrap_or("?")
            );
        }
    }
    match finish {
        Some(reason) => format!("finishReason={}", reason),
        None => "no candidates in response".to_string(),
    }
}

fn endpoint(model: &str) -> String {
    format!("{}/{}:generateContent", API_BASE, model)
}

pub async fn transcribe(
    audio_pcm_b64: &str,
    sample_rate: u32,
    config: &AsrProviderConfig,
    hotwords: &[String],
) -> Result<AsrResult, String> {
    if config.api_key.trim().is_empty() {
        return Err(diag::fail_code(
            SCOPE,
            "credentials",
            "provider_bad_key",
            "Gemini transcription is missing the API Key; complete it in Settings".to_string(),
        ));
    }

    let pcm = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, audio_pcm_b64)
        .map_err(|e| {
            diag::fail(
                SCOPE,
                "decode_b64",
                format!("Failed to decode base64 audio: {}", e),
            )
        })?;
    if pcm.is_empty() {
        diag::empty_result(SCOPE, "Input audio was empty; provider request was skipped");
        return Ok(AsrResult {
            text: String::new(),
            elapsed_ms: 0,
        });
    }

    let audio_sec = pcm.len() as f64 / (sample_rate.max(1) as f64 * 2.0);
    let model = resolve_model(config);
    let prompt = build_prompt(config, hotwords);
    let wav = pcm_to_wav(&pcm, sample_rate);
    let wav_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &wav);

    diag::log(
        SCOPE,
        "start",
        &format!(
            "model={} wav_bytes={} audio_sec={:.1} rate={} hotwords={}",
            model,
            wav.len(),
            audio_sec,
            sample_rate,
            hotwords.len()
        ),
    );

    let client = super::http_client::shared();
    let start = Instant::now();
    let resp = client
        .post(endpoint(&model))
        // 金鑰走請求頭而不是 query 引數：query 會被各層日誌、代理、錯誤頁原樣記下來。
        .header("x-goog-api-key", config.api_key.trim())
        .json(&build_body(&prompt, wav_b64))
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| diag::fail(SCOPE, "http_send", format!("Request failed: {}", e)))?;

    let elapsed_ms = start.elapsed().as_millis() as u64;
    let http_summary = diag::http_summary(resp.status(), resp.headers());

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(diag::fail(
            SCOPE,
            "http_status",
            format!(
                "Gemini transcription error {} [{}]: {}",
                status,
                http_summary,
                diag::truncate(&body, 300)
            ),
        ));
    }

    let body_text = resp
        .text()
        .await
        .map_err(|e| diag::fail(SCOPE, "read_body", format!("Failed to read response: {}", e)))?;
    let data: serde_json::Value = serde_json::from_str(&body_text).map_err(|e| {
        diag::fail(
            SCOPE,
            "parse_json",
            format!(
                "Failed to parse response: {} [{}] response excerpt: {}",
                e,
                http_summary,
                diag::truncate(&body_text, 200)
            ),
        )
    })?;

    let text = extract_text(&data);
    if text.is_empty() {
        // 空結果走成功路徑，前端只會顯示「未檢測到有效聲音」。不留這條日誌的話，
        // 「真的沒說話」和「被內容審查攔了」就再也分不開（見 pitfalls 第 15 條）。
        diag::empty_result(
            SCOPE,
            &format!(
                "Response contained no transcript audio_sec={:.1} elapsed={}ms [{}] {}",
                audio_sec,
                elapsed_ms,
                http_summary,
                describe_empty(&data)
            ),
        );
    } else {
        diag::ok(SCOPE, elapsed_ms, text.chars().count());
    }

    Ok(AsrResult { text, elapsed_ms })
}

pub async fn test_connection(config: &AsrProviderConfig) -> TestResult {
    let model = resolve_model(config);
    // 0.5s 靜音：能過鑑權與模型解析就夠了，轉寫結果為空是預期的。
    let silence = vec![0u8; 16000];
    let wav = pcm_to_wav(&silence, 16000);
    let wav_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &wav);

    let client = super::http_client::shared();
    let start = Instant::now();
    let result = client
        .post(endpoint(&model))
        .header("x-goog-api-key", config.api_key.trim())
        .json(&build_body(TRANSCRIBE_PROMPT, wav_b64))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await;

    let elapsed_ms = start.elapsed().as_millis() as u64;
    match result {
        Ok(resp) if resp.status().is_success() => TestResult {
            ok: true,
            message: format!("Connection successful ({}ms)", elapsed_ms),
            elapsed_ms,
            detail: format!("model: {}", model),
        },
        Ok(resp) => {
            let status = resp.status();
            let summary = diag::http_summary(status, resp.headers());
            let body = resp.text().await.unwrap_or_default();
            TestResult {
                ok: false,
                message: diag::fail(
                    SCOPE,
                    "http_status",
                    format!(
                        "API error {} [{}]: {}",
                        status,
                        summary,
                        diag::truncate(&body, 100)
                    ),
                ),
                elapsed_ms,
                detail: String::new(),
            }
        }
        Err(e) => TestResult {
            ok: false,
            message: diag::fail(SCOPE, "http_send", format!("Connection failed: {}", e)),
            elapsed_ms,
            detail: String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(extra: serde_json::Value) -> AsrProviderConfig {
        AsrProviderConfig {
            provider: "gemini_transcribe".to_string(),
            api_key: "key".to_string(),
            app_id: String::new(),
            extra,
        }
    }

    #[test]
    fn model_falls_back_to_the_default() {
        assert_eq!(resolve_model(&config(serde_json::json!({}))), DEFAULT_MODEL);
        assert_eq!(
            resolve_model(&config(serde_json::json!({ "model": "  " }))),
            DEFAULT_MODEL
        );
        assert_eq!(
            resolve_model(&config(serde_json::json!({ "model": "gemini-3.5-flash" }))),
            "gemini-3.5-flash"
        );
    }

    /// 流式那張卡關掉字幕會回落到這條 HTTP 路徑，而它選的模型
    /// （gemini-3.5-transcribe-live）在 generateContent 上不存在 —— 必須被換掉。
    #[test]
    fn live_card_falling_back_ignores_the_streaming_model() {
        let mut cfg = config(serde_json::json!({ "model": "gemini-3.5-transcribe-live" }));
        cfg.provider = LIVE_PROVIDER.to_string();
        assert_eq!(resolve_model(&cfg), DEFAULT_MODEL);
    }

    #[test]
    fn endpoint_targets_generate_content() {
        assert_eq!(
            endpoint("gemini-3.5-transcribe"),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-3.5-transcribe:generateContent"
        );
    }

    /// auto 時**一個字都不能提語言** —— 寫個模型不認識的語言名只會讓它困惑。
    #[test]
    fn auto_language_adds_no_hint() {
        assert_eq!(language_hint(&config(serde_json::json!({}))), None);
        assert_eq!(
            language_hint(&config(serde_json::json!({ "language": "auto" }))),
            None
        );
        assert!(language_hint(&config(serde_json::json!({ "language": "zh" })))
            .unwrap()
            .contains("zh"));
    }

    /// prompt 裡那三條禁令是防「模型答話而不是轉寫」的唯一手段，被人圖省事刪掉時這條會攔住。
    #[test]
    fn prompt_forbids_commentary_and_hallucination() {
        let prompt = build_prompt(&config(serde_json::json!({})), &[]);
        assert!(prompt.contains("Output only the transcript"));
        assert!(prompt.contains("Do not add explanations"));
        assert!(prompt.contains("output nothing at all"));
    }

    #[test]
    fn hotwords_go_into_the_prompt_deduped_and_capped() {
        let words = vec!["SayIt".to_string(), "sayit".to_string(), " Kiro ".to_string()];
        let prompt = build_prompt(&config(serde_json::json!({})), &words);
        assert!(prompt.contains("SayIt, Kiro"));

        let many: Vec<String> = (0..HOTWORD_LIMIT + 20).map(|i| format!("w{}", i)).collect();
        let capped = build_prompt(&config(serde_json::json!({})), &many);
        assert!(capped.contains("w0"));
        assert!(!capped.contains(&format!("w{}", HOTWORD_LIMIT + 5)));
    }

    #[test]
    fn body_carries_audio_inline() {
        let body = build_body("say it", "QUJD".to_string());
        let parts = &body["contents"][0]["parts"];
        assert_eq!(parts[0]["text"], "say it");
        assert_eq!(parts[1]["inline_data"]["mime_type"], "audio/wav");
        assert_eq!(parts[1]["inline_data"]["data"], "QUJD");
        assert_eq!(body["generationConfig"]["temperature"], 0);
    }

    /// parts 可能分段，只取 parts[0] 會丟後半截。
    #[test]
    fn extracts_and_joins_all_text_parts() {
        let data = serde_json::json!({
            "candidates": [{ "content": { "parts": [{ "text": "  你好" }, { "text": "，世界。 " }] } }]
        });
        assert_eq!(extract_text(&data), "你好，世界。");
        assert_eq!(extract_text(&serde_json::json!({})), "");
    }

    /// 實測響應的原樣形狀（2026-09-16）。轉寫專用模型用 audioTranscription 包一層，
    /// 只讀 `text` 的話這條會失敗 —— 而線上表現是「HTTP 200 但一個字都沒有」。
    #[test]
    fn extracts_the_dedicated_audio_transcription_field() {
        let data = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [{ "audioTranscription": { "text": "語音輸入法測試成功。" } }],
                    "role": "model"
                },
                "finishReason": "STOP"
            }],
            "modelVersion": "gemini-3.5-transcribe"
        });
        assert_eq!(extract_text(&data), "語音輸入法測試成功。");
    }

    /// 兩種形狀混在一起也要按順序拼全（真出現分段時不至於丟半截）。
    #[test]
    fn handles_both_part_shapes_together() {
        let data = serde_json::json!({
            "candidates": [{ "content": { "parts": [
                { "audioTranscription": { "text": "前半" } },
                { "text": "後半" }
            ] } }]
        });
        assert_eq!(extract_text(&data), "前半後半");
    }

    /// 空結果的原因必須能區分：被審查攔掉是「重試也沒用」，和「沒說話」完全不同。
    #[test]
    fn empty_reason_distinguishes_safety_block() {
        let blocked = serde_json::json!({ "promptFeedback": { "blockReason": "SAFETY" } });
        assert!(describe_empty(&blocked).contains("content filter"));
        assert!(describe_empty(&blocked).contains("SAFETY"));

        let stopped = serde_json::json!({ "candidates": [{ "finishReason": "MAX_TOKENS" }] });
        assert!(describe_empty(&stopped).contains("MAX_TOKENS"));

        assert!(describe_empty(&serde_json::json!({})).contains("no candidates"));
    }

    /// 認不出欄位時必須直說「這是我們的解析 bug」並列出鍵名。
    ///
    /// 第一次實測就卡在這裡：當時只報 finishReason=STOP，看上去像使用者沒說話，
    /// 而真相是響應裡躺著一段完美的轉寫、只是包在一個我們沒讀的欄位裡。
    #[test]
    fn empty_reason_calls_out_unrecognized_part_fields() {
        let data = serde_json::json!({
            "candidates": [{
                "content": { "parts": [{ "somethingNew": { "text": "hi" } }] },
                "finishReason": "STOP"
            }]
        });
        let why = describe_empty(&data);
        assert!(why.contains("somethingNew"), "must name the unknown key: {}", why);
        assert!(why.contains("parsing bug"), "must say it is our bug: {}", why);
        assert!(why.contains("STOP"));
    }

    #[test]
    fn wav_header_is_well_formed() {
        let pcm = vec![0u8; 320];
        let wav = pcm_to_wav(&pcm, 16000);
        assert_eq!(wav.len(), 44 + pcm.len());
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        let data_size = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]);
        assert_eq!(data_size as usize, pcm.len());
    }
}
