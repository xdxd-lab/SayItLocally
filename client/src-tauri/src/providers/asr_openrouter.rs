// OpenRouter 轉寫 — 一把 key 通往多家 STT 模型
//
// 走 OpenRouter 自己的 `POST /api/v1/audio/transcriptions`：**JSON body + base64 音訊**
// （`input_audio: { data, format }`），不是 multipart。
//
// ⚠️ 別改成複用 asr_groq.rs。OpenRouter 確實**也**接受 OpenAI 風格的 multipart
// （官方說把 base_url 指過來就能用 OpenAI SDK），但那條路上 `prompt` 欄位是
// **「接受後忽略」**的 —— 而 asr_groq.rs 全靠那個 prompt 給中文短句加標點
// （見它的 PUNCTUATION_PROMPT，缺了它中文轉寫一個標點都沒有）。
// 複用等於悄悄丟掉標點，所以這裡獨立一份，走原生 JSON 形態。
//
// ── 關於標點這件事（這是選模型的關鍵，不是小事）──
// OpenRouter 的兩種形態都沒有 prompt 通道（JSON 的參數列裡也沒有），所以
// **Whisper 系模型在這裡沒法做標點引導**。預設模型因此選 `openai/gpt-transcribe`：
// 新一代 token 計費的模型自帶標點，不依賴「拿帶標點的句子做示範」那套。
// 使用者要選 whisper-1 / whisper-large-v3 也可以，只是中文短句可能沒有句尾標點。
// 半形轉全形在前端 textPostProcess.ts 的 normalizeChinesePunctuation 裡做，
// 那解決的是寬度不是有無。
//
// ── 模型 slug 必須帶廠商字首 ──
// `openai/whisper-1` 而不是 `whisper-1`。寫錯會得到一個 404 "model not found"。
// 可用清單在前端目錄裡（asrProviderCatalog.ts 的 openrouter_transcribe.models），
// 想拿到當前完整清單跑
// `python dev-scripts/probe_new_asr_providers.py --target openrouter --list-models`
// （它打的是 /api/v1/models?output_modalities=transcription）。
//
// ── 未經真實介面驗證 ──
// 照官方文件實現，沒有 OpenRouter key。用
// `python dev-scripts/probe_new_asr_providers.py --target openrouter` 打一次真接口。

use super::diag;
use super::types::{AsrProviderConfig, AsrResult, TestResult};
use std::time::Instant;

const API_URL: &str = "https://openrouter.ai/api/v1/audio/transcriptions";
const SCOPE: &str = "openrouter/asr";

/// 預設模型。選自帶標點的那一代，理由見檔案頭「關於標點這件事」。
const DEFAULT_MODEL: &str = "openai/gpt-transcribe";

/// 我們送的音訊容器。裸 PCM 對方認不出來，要封 WAV。
const AUDIO_FORMAT: &str = "wav";

/// 可選的排名歸屬頭。OpenRouter 用它在自己站上做呼叫量榜單，純自願。
///
/// 帶上是為了讓 SayIt 的呼叫量歸到專案名下（對專案有點好處，對使用者無成本）；
/// 它們與鑑權、計費、路由都無關，去掉也一樣能用。
const REFERER: &str = "https://sayitapp.site";
const TITLE: &str = "SayIt";

/// 將 16kHz 單聲道 16-bit PCM 封裝為 WAV 容器。
///
/// 与 asr_groq / asr_mimo / asr_gemini 里那几份是同一个 WAV 头，刻意各留一份：
/// 抽成公共函式後任何一家改取樣格式都會牽動其餘幾家，而它們本來毫無關係。
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

fn resolve_model(config: &AsrProviderConfig) -> String {
    config
        .extra
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_MODEL)
        .to_string()
}

/// 識別語言：設定裡存的是 auto|zh|en|…，auto 必須**整個省略 language 欄位**。
///
/// 不能傳字串 "auto" —— 文件要的是 ISO-639-1 碼，省略才代表自動檢測。
fn resolve_language(config: &AsrProviderConfig) -> Option<String> {
    let raw = config
        .extra
        .get("language")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("auto");
    match raw {
        "auto" => None,
        other => Some(other.to_string()),
    }
}

fn build_body(
    model: &str,
    wav_b64: String,
    language: Option<&str>,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "input_audio": { "data": wav_b64, "format": AUDIO_FORMAT },
        // temperature 0：轉寫要照實還原，不要多樣性。
        "temperature": 0,
        // 只要 text 一個欄位。verbose_json 能給時間戳，但部分模型會**直接 400 拒掉**
        // （文件點名 openai/gpt-4o-transcribe 與 microsoft/mai-transcribe-1.5），
        // 而我們一個時間戳都不用 —— 為了用不上的欄位換一類模型失效不值得。
        "response_format": "json",
    });
    if let Some(lang) = language {
        body["language"] = serde_json::json!(lang);
    }
    body
}

/// 響應形狀是 `{"text": "...", "usage": {...}}`。
fn extract_text(data: &serde_json::Value) -> String {
    data.get("text")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// 這次呼叫花了多少、走的哪家 —— OpenRouter 會在 usage 裡回報。
///
/// 值得記：OpenRouter 是個路由層，同一個模型可能由不同上游承載，
/// 出問題時「花了多少錢」和「實際耗時」是判斷打到哪一家的線索。
/// **只記數字，不記文本。**
fn describe_usage(data: &serde_json::Value) -> String {
    let usage = match data.get("usage") {
        Some(u) => u,
        None => return "usage=absent".to_string(),
    };
    let num = |key: &str| {
        usage
            .get(key)
            .and_then(|v| v.as_f64())
            .map(|v| format!("{}={}", key, v))
    };
    let parts: Vec<String> = ["seconds", "total_tokens", "cost"]
        .iter()
        .filter_map(|k| num(k))
        .collect();
    if parts.is_empty() {
        "usage=empty".to_string()
    } else {
        parts.join(" ")
    }
}

/// 生成 ID，出問題時拿它去 OpenRouter 後台查這一次呼叫。
fn generation_id(headers: &reqwest::header::HeaderMap) -> String {
    headers
        .get("x-generation-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-")
        .to_string()
}

pub async fn transcribe(
    audio_pcm_b64: &str,
    sample_rate: u32,
    config: &AsrProviderConfig,
    hotwords: &[String],
) -> Result<AsrResult, String> {
    // OpenRouter 這條路沒有任何地方能放熱詞：原生 JSON 的參數列裡沒有 prompt，
    // 它也接受的那套 multipart 里 prompt 是「收下后忽略」（见文件头）。
    // 所以 capabilities.rs 把它記作 ProtocolHasNoSlot —— 和「我們沒接」是兩件事。
    // 留痕理由同 asr_groq.rs。
    if !hotwords.is_empty() {
        diag::log(
            SCOPE,
            "hotwords_ignored",
            &format!("count={} reason=protocol_has_no_slot", hotwords.len()),
        );
    }

    if config.api_key.trim().is_empty() {
        return Err(diag::fail_code(
            SCOPE,
            "credentials",
            "provider_bad_key",
            "OpenRouter is missing the API Key; complete it in Settings".to_string(),
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
    let language = resolve_language(config);
    let wav = pcm_to_wav(&pcm, sample_rate);
    let wav_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &wav);

    diag::log(
        SCOPE,
        "start",
        &format!(
            "model={} wav_bytes={} audio_sec={:.1} rate={} language={}",
            model,
            wav.len(),
            audio_sec,
            sample_rate,
            language.as_deref().unwrap_or("auto(omitted)")
        ),
    );

    let client = super::http_client::shared();
    let start = Instant::now();
    let resp = client
        .post(API_URL)
        .header(
            "Authorization",
            format!("Bearer {}", config.api_key.trim()),
        )
        .header("HTTP-Referer", REFERER)
        .header("X-Title", TITLE)
        .json(&build_body(&model, wav_b64, language.as_deref()))
        // 上游對每個請求有 60s 超時，我們留夠餘量再加上傳時間
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| diag::fail(SCOPE, "http_send", format!("Request failed: {}", e)))?;

    let elapsed_ms = start.elapsed().as_millis() as u64;
    let http_summary = diag::http_summary(resp.status(), resp.headers());
    let gen_id = generation_id(resp.headers());

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(diag::fail(
            SCOPE,
            "http_status",
            format!(
                "OpenRouter transcription error {} [{}] gen={}: {}",
                status,
                http_summary,
                gen_id,
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
        // 「真的沒說話」和「這次呼叫其實失敗了」就再也分不開（見 pitfalls 第 15 條）。
        // gen_id 尤其要記：OpenRouter 是路由層，拿它才能查到究竟是哪家上游返回了空。
        diag::empty_result(
            SCOPE,
            &format!(
                "Response contained no transcript model={} audio_sec={:.1} elapsed={}ms \
                 [{}] gen={} {}",
                model,
                audio_sec,
                elapsed_ms,
                http_summary,
                gen_id,
                describe_usage(&data)
            ),
        );
    } else {
        diag::log(
            SCOPE,
            "usage",
            &format!("model={} gen={} {}", model, gen_id, describe_usage(&data)),
        );
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
        .post(API_URL)
        .header(
            "Authorization",
            format!("Bearer {}", config.api_key.trim()),
        )
        .header("HTTP-Referer", REFERER)
        .header("X-Title", TITLE)
        .json(&build_body(&model, wav_b64, None))
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
            let gen_id = generation_id(resp.headers());
            let body = resp.text().await.unwrap_or_default();
            TestResult {
                ok: false,
                message: diag::fail(
                    SCOPE,
                    "http_status",
                    format!(
                        "API error {} [{}] gen={}: {}",
                        status,
                        summary,
                        gen_id,
                        diag::truncate(&body, 150)
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
            provider: "openrouter_transcribe".to_string(),
            api_key: "sk-or-test".to_string(),
            app_id: String::new(),
            extra,
        }
    }

    #[test]
    fn model_falls_back_to_a_punctuating_default() {
        assert_eq!(resolve_model(&config(serde_json::json!({}))), DEFAULT_MODEL);
        assert_eq!(
            resolve_model(&config(serde_json::json!({ "model": "  " }))),
            DEFAULT_MODEL
        );
        assert_eq!(
            resolve_model(&config(serde_json::json!({ "model": "openai/whisper-1" }))),
            "openai/whisper-1"
        );
    }

    /// 預設模型必須帶廠商字首 —— 不帶會得到 404 "model not found"。
    #[test]
    fn default_model_is_namespaced() {
        assert!(DEFAULT_MODEL.contains('/'), "slug must be vendor-prefixed");
    }

    /// body 的形狀是這份實現的全部前提。尤其 input_audio 是**物件**（data + format），
    /// 不是 OpenAI 那种 multipart 的 file 字段。
    #[test]
    fn body_matches_the_documented_shape() {
        let body = build_body(DEFAULT_MODEL, "QUJD".to_string(), None);
        assert_eq!(body["model"], DEFAULT_MODEL);
        assert_eq!(body["input_audio"]["data"], "QUJD");
        assert_eq!(body["input_audio"]["format"], "wav");
        assert_eq!(body["temperature"], 0);
        // json 而不是 verbose_json：后者会被一部分模型直接 400 拒掉
        assert_eq!(body["response_format"], "json");
        // auto 時絕不能出現 language 欄位
        assert!(body.get("language").is_none());
    }

    #[test]
    fn explicit_language_is_passed_but_auto_is_omitted() {
        assert_eq!(resolve_language(&config(serde_json::json!({}))), None);
        assert_eq!(
            resolve_language(&config(serde_json::json!({ "language": "auto" }))),
            None
        );
        assert_eq!(
            resolve_language(&config(serde_json::json!({ "language": "zh" }))),
            Some("zh".to_string())
        );
        let body = build_body(DEFAULT_MODEL, "x".to_string(), Some("ja"));
        assert_eq!(body["language"], "ja");
    }

    #[test]
    fn extracts_text_field() {
        let data = serde_json::json!({ "text": "  语音输入法测试成功。  " });
        assert_eq!(extract_text(&data), "语音输入法测试成功。");
        assert_eq!(extract_text(&serde_json::json!({})), "");
    }

    /// 實測（2026-09-16）：餘額不足時 OpenRouter 返回 402，而 402 以前一條分類都
    /// 沒命中，介面顯示成「連線失敗」—— 真實原因是賬戶沒錢，使用者看不出該幹什麼。
    /// 這條釘住 diag 側的分類（前端那半在 errorMessages.test.ts）。
    #[test]
    fn payment_required_is_classified_as_insufficient_balance() {
        let message = super::super::diag::fail(
            SCOPE,
            "http_status",
            "OpenRouter transcription error 402 Payment Required [http=402] gen=-: \
             {\"error\":{\"message\":\"This request requires at least $0.50 in balance for audio\"}}"
                .to_string(),
        );
        assert!(
            message.contains("provider_insufficient_balance"),
            "402 must not fall back to connect_failed: {}",
            message
        );
        // 服務商給的具體金額要留在訊息裡，使用者才知道要充多少
        assert!(message.contains("$0.50"));
    }

    /// usage 只記數字。這條同時擋住「有人往這裡加 text 欄位」——
    /// diag 的硬約束是絕不記識別文本。
    #[test]
    fn usage_reports_numbers_only() {
        let data = serde_json::json!({
            "text": "秘密内容",
            "usage": { "seconds": 3.05, "total_tokens": 143, "cost": 0.00012 }
        });
        let summary = describe_usage(&data);
        assert!(summary.contains("seconds=3.05"));
        assert!(summary.contains("total_tokens=143"));
        assert!(summary.contains("cost=0.00012"));
        assert!(!summary.contains("秘密"), "must never carry transcript text");

        assert_eq!(describe_usage(&serde_json::json!({})), "usage=absent");
        assert_eq!(
            describe_usage(&serde_json::json!({ "usage": {} })),
            "usage=empty"
        );
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
