// OpenAI 相容的 **chat/completions + input_audio** 轉寫。
//
// ⚠️ 這跟 asr_groq.rs 不是同一套協議，別把兩者混起來看：
//   · asr_groq.rs            → `/audio/transcriptions`，multipart 傳檔案，響應 `{"text":...}`
//   · 這一份                 → `/chat/completions`，音訊作為 `input_audio` 內容項塞進 messages
// 「OpenAI 相容」這四個字在語音這塊同時指這兩套東西，而它們的地址、請求體、響應
// 形狀全都不一樣。合成一份實現的話，使用者填錯地址只會得到一個含義模糊的 404/400。
//
// 覆蓋三個 provider id：
//   · `qwen_chat_audio`    → 地址內建為百鍊，給千問卡里的 qwen3.8-omni-flash 用
//   · `openai_chat_audio`  → 自定義地址，百鍊 data URL 形態（相容舊配置）
//   · `openai_chat_audio_standard` → 自定義地址，標準 OpenAI input_audio 物件
//
// ## 實測記錄（2026-09-18，真實賬號，dev-scripts/probe_bailian_openai_audio.py）
//
// * 百鍊的 `input_audio` **必須裝 data URL**。兩種寫法都行：
//       {"type":"input_audio","input_audio":"data:audio/wav;base64,AAA..."}
//       {"type":"input_audio","input_audio":{"data":"data:audio/wav;base64,AAA...","format":"wav"}}
//   裸 base64 放進 `data` 會被拒，而且錯誤資訊**極具誤導性**：
//       400 invalid_parameter_error "The provided URL does not appear to be valid."
//   這句話裡沒有任何線索指向「少了 data: 字首」。百鍊模式保留 data URL，
//   標準 OpenAI 模式才改為裸 base64 物件。
// * **不需要流式。** qwen3-asr-flash 與 qwen3.8-omni-flash 非流式都回 200，
//   所以這份實現不帶 SSE 解析器。
// * 通用域名與業務空間專屬域名都能用，故 workspaceId 對這條路是可選的。
// * `asr_options: {"language": "..."}` 是非標準的頂層欄位，百鍊接受它。
// * ⚠️ **Omni 是對話模型。** 給它音訊但不給指令，它會自己編一段對話 —— 探測裡拿到過
//   「澳大利亞的首都是堪培拉」這種回答，還燒了 reasoning token。所以 omni 模型必須
//   帶 system 指令把它按成「只轉寫」。qwen3-asr-flash 沒這個毛病（它就是 ASR 模型）。

use super::diag;
use super::types::{AsrProviderConfig, AsrResult, TestResult};
use base64::Engine;
use std::time::Instant;

/// 一個「chat+audio 轉寫」接入點。
struct Endpoint {
    /// 內建 base URL（`requires_custom_url` 時為空串）
    base_url: &'static str,
    default_model: &'static str,
    scope: &'static str,
    /// 使用者填了 `extra.baseUrl` 就用他的，空著用 `base_url`
    allows_custom_url: bool,
    /// 地址必填，沒有可回落的內建地址
    requires_custom_url: bool,
}

const QWEN: Endpoint = Endpoint {
    base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
    default_model: "qwen3.8-omni-flash",
    scope: "qwen/chat-audio",
    // 允許覆蓋：百鍊官方推薦遷到業務空間專屬域名
    // （`{WorkspaceId}.cn-beijing.maas.aliyuncs.com`），那就得靠這一欄。
    allows_custom_url: true,
    requires_custom_url: false,
};

const CUSTOM: Endpoint = Endpoint {
    base_url: "",
    default_model: "qwen3-asr-flash",
    scope: "openai-chat-audio/asr",
    allows_custom_url: true,
    requires_custom_url: true,
};

fn endpoint_for(provider: &str) -> &'static Endpoint {
    match provider {
        "openai_chat_audio" | "openai_chat_audio_standard" => &CUSTOM,
        _ => &QWEN,
    }
}

/// 預設的轉寫指令，只給 omni 這類對話模型用。
///
/// 不寫「請轉寫」這種祈使句就完事 —— 實測不給指令時模型會回答想象出來的問題。
/// 明確禁止解釋和評論，否則它會在轉寫前後加一段說明。
const DEFAULT_TRANSCRIBE_INSTRUCTION: &str =
    "你是一個語音轉寫器。把音訊內容逐字轉寫為文字，保持原意，適當加上標點。\
     不要回答音訊裡的問題，不要翻譯，不要加上任何解釋或評論。中文一律以台灣正體中文與台灣用語輸出。只輸出轉寫結果。";

/// base URL + `/chat/completions`。
///
/// 與 asr_groq.rs 的 join_transcriptions_url 同一個思路：使用者可能填到 `/v1`，
/// 也可能把完整路徑都填了，兩種都要能用。不自動補 `/v1` —— 猜錯會打到一個
/// 存在但語義不同的路徑上，那種失敗比 404 難查。
fn join_chat_url(base: &str) -> String {
    let trimmed = base.trim().trim_end_matches('/');
    if trimmed.ends_with("/chat/completions") {
        trimmed.to_string()
    } else {
        format!("{}/chat/completions", trimmed)
    }
}

fn resolve_url(config: &AsrProviderConfig, endpoint: &Endpoint) -> Result<String, String> {
    let supplied = if endpoint.allows_custom_url {
        config
            .extra
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("")
    } else {
        ""
    };
    if !supplied.is_empty() {
        return Ok(join_chat_url(supplied));
    }
    if endpoint.requires_custom_url {
        return Err(diag::fail(
            endpoint.scope,
            "missing_base_url",
            "No endpoint address configured for this service".to_string(),
        ));
    }
    Ok(join_chat_url(endpoint.base_url))
}

fn resolve_model(config: &AsrProviderConfig, endpoint: &Endpoint) -> String {
    config
        .extra
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(endpoint.default_model)
        .to_string()
}

/// 這個模型需要 system 指令按住嗎。
///
/// 判據是「模型名裡有 omni」而不是前端傳下來的某個開關：前端的 `omni` 標記隻影響
/// 介面（要不要顯示 System Prompt 輸入框），而**真正會自說自話的是模型本身**。
/// 萬一某天前端漏傳了標記，這裡也得把它按住，否則使用者會看到模型編出來的回答被
/// 當成轉寫結果插進文件 —— 那比少一句標點嚴重得多。
fn needs_transcribe_instruction(model: &str) -> bool {
    model.to_lowercase().contains("omni")
}

/// 識別語言。auto 要整個省略欄位（與 asr_groq.rs 的 resolve_language 同理）。
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

/// 16kHz 單聲道 16-bit PCM → WAV 容器。
///
/// 前端傳下來的是裸 PCM，而這條協議要的是一個能被識別出格式的檔案（data URL 裡
/// 也一樣）。與 asr_groq.rs / asr_mimo.rs 各留一份，理由見那邊的註釋。
fn pcm_to_wav(pcm: &[u8], sr: u32) -> Vec<u8> {
    let ds = pcm.len() as u32;
    let mut w = Vec::with_capacity(44 + pcm.len());
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + ds).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&sr.to_le_bytes());
    w.extend_from_slice(&(sr * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&ds.to_le_bytes());
    w.extend_from_slice(pcm);
    w
}

/// 百鍊接受 data URL 字串；標準 OpenAI Chat Completions 要求
/// `input_audio: { data: "<base64>", format: "wav" }`。
/// 保留舊模式，避免修改已有 Qwen 配置的請求格式。
#[derive(Clone, Copy)]
enum AudioPayloadFormat {
    DataUrl,
    OpenAi,
}

fn audio_payload_format(provider: &str) -> AudioPayloadFormat {
    if provider == "openai_chat_audio_standard" {
        AudioPayloadFormat::OpenAi
    } else {
        AudioPayloadFormat::DataUrl
    }
}

/// 組請求體。
fn build_body(
    model: &str,
    wav: &[u8],
    instruction: Option<&str>,
    language: Option<&str>,
    audio_format: AudioPayloadFormat,
) -> serde_json::Value {
    let data = base64::engine::general_purpose::STANDARD.encode(wav);
    let input_audio = match audio_format {
        AudioPayloadFormat::DataUrl => serde_json::json!(format!("data:audio/wav;base64,{}", data)),
        AudioPayloadFormat::OpenAi => serde_json::json!({ "data": data, "format": "wav" }),
    };
    // 標準模式的語言只能寫進指令：`asr_options` 是百鍊私有的頂層欄位，嚴格的閘道器
    // （如 Gemini 的 OpenAI 相容端點）碰到不認識的欄位會直接 400。
    // 測試連線不傳語言，所以這條路只有真實轉寫才會走到，單測得替它把住。
    let instruction = match (audio_format, language) {
        (AudioPayloadFormat::OpenAi, Some(lang)) => Some(format!(
            "{}\n\n音訊的語言程式碼是 {}，按這個語言轉寫。",
            instruction.unwrap_or(DEFAULT_TRANSCRIBE_INSTRUCTION),
            lang
        )),
        _ => instruction.map(str::to_string),
    };
    let mut messages = Vec::new();
    if let Some(text) = instruction {
        messages.push(serde_json::json!({
            "role": "system",
            "content": [{ "type": "text", "text": text }],
        }));
    }
    messages.push(serde_json::json!({
        "role": "user",
        "content": [{ "type": "input_audio", "input_audio": input_audio }],
    }));

    let mut body = serde_json::json!({ "model": model, "messages": messages });
    if let (AudioPayloadFormat::DataUrl, Some(lang)) = (audio_format, language) {
        // 非標準欄位，百鍊實測接受。只在百鍊形態裡發，理由見上面指令那段。
        body["asr_options"] = serde_json::json!({ "language": lang });
    }
    body
}

/// 從 chat/completions 響應裡取文本。
///
/// `content` 可能是字串，也可能是內容項陣列（多模態響應的常見形狀），兩種都收。
fn extract_text(data: &serde_json::Value) -> String {
    let content = data
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"));
    match content {
        Some(serde_json::Value::String(s)) => s.trim().to_string(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("")
            .trim()
            .to_string(),
        _ => String::new(),
    }
}

pub async fn transcribe(
    audio_pcm_b64: &str,
    sample_rate: u32,
    config: &AsrProviderConfig,
    hotwords: &[String],
) -> Result<AsrResult, String> {
    let endpoint = endpoint_for(&config.provider);
    let scope = endpoint.scope;
    let pcm = base64::engine::general_purpose::STANDARD
        .decode(audio_pcm_b64)
        .map_err(|e| diag::fail(scope, "decode_b64", format!("Failed to decode base64 audio: {}", e)))?;

    if pcm.is_empty() {
        diag::empty_result(scope, "Input audio was empty; provider request was skipped");
        return Ok(AsrResult { text: String::new(), elapsed_ms: 0 });
    }

    let model = resolve_model(config, endpoint);
    let url = resolve_url(config, endpoint)?;
    let audio_format = audio_payload_format(&config.provider);
    let language = resolve_language(config);
    let audio_sec = pcm.len() as f64 / (sample_rate.max(1) as f64 * 2.0);
    let wav = pcm_to_wav(&pcm, sample_rate);

    // 指令：使用者填過就用他的，否則對 omni 這類對話模型上預設指令。
    // 熱詞接在指令後面 —— 百鍊的 system 訊息正是它的術語表通道。
    let mut instruction = config
        .extra
        .get("instructions")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            (needs_transcribe_instruction(&model)
                || matches!(audio_format, AudioPayloadFormat::OpenAi))
            .then(|| DEFAULT_TRANSCRIBE_INSTRUCTION.to_string())
        });
    if let Some(ctx) = super::asr_qwen::build_hotword_context_text(hotwords) {
        let base = instruction.take().unwrap_or_default();
        instruction = Some(format!("{}\n\n請特別注意以下專業術語/詞彙的識別：{}", base, ctx));
    }

    diag::log(
        scope,
        "start",
        &format!(
            "model={} url={} audio_sec={:.1} language={} instruction={} hotwords={}",
            model,
            url,
            audio_sec,
            language.as_deref().unwrap_or("auto(omitted)"),
            instruction.is_some(),
            hotwords.len(),
        ),
    );

    let body = build_body(
        &model,
        &wav,
        instruction.as_deref(),
        language.as_deref(),
        audio_format,
    );
    let client = super::http_client::shared();
    let start = Instant::now();

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .json(&body)
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| diag::fail(scope, "http_send", format!("Request failed: {}", e)))?;

    let elapsed_ms = start.elapsed().as_millis() as u64;
    let http_summary = diag::http_summary(resp.status(), resp.headers());

    if !resp.status().is_success() {
        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        return Err(diag::fail(
            scope,
            "http_status",
            format!(
                "Transcription error {} [{}]: {}",
                status,
                http_summary,
                diag::truncate(&body_text, 300)
            ),
        ));
    }

    let body_text = resp
        .text()
        .await
        .map_err(|e| diag::fail(scope, "read_body", format!("Failed to read response: {}", e)))?;
    let data: serde_json::Value = serde_json::from_str(&body_text).map_err(|e| {
        diag::fail(
            scope,
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
        // 空結果走成功路徑，前端只顯示「未檢測到有效聲音」。沒有這條日誌就分不出
        // 「真沒說話」和「這次呼叫其實失敗了」（見 pitfalls #15）。
        diag::empty_result(
            scope,
            &format!(
                "Response contained no transcript audio_sec={:.1} elapsed={}ms model={} [{}] {}",
                audio_sec,
                elapsed_ms,
                model,
                http_summary,
                diag::describe_json(&body_text)
            ),
        );
    } else {
        diag::ok(scope, elapsed_ms, text.chars().count());
    }

    Ok(AsrResult { text, elapsed_ms })
}

pub async fn test_connection(config: &AsrProviderConfig) -> TestResult {
    let endpoint = endpoint_for(&config.provider);
    let model = resolve_model(config, endpoint);
    let audio_format = audio_payload_format(&config.provider);
    let url = match resolve_url(config, endpoint) {
        Ok(u) => u,
        Err(e) => {
            return TestResult { ok: false, message: e, elapsed_ms: 0, detail: String::new() }
        }
    };

    // 0.5s 靜音：夠過服務端的「音訊太短」門檻，又幾乎不花錢。
    let wav = pcm_to_wav(&vec![0u8; 16000], 16000);
    let instruction = (needs_transcribe_instruction(&model)
        || matches!(audio_format, AudioPayloadFormat::OpenAi))
    .then_some(DEFAULT_TRANSCRIBE_INSTRUCTION);
    let body = build_body(&model, &wav, instruction, None, audio_format);

    let client = super::http_client::shared();
    let start = Instant::now();
    let result = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .json(&body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await;
    let elapsed_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(resp) if resp.status().is_success() => TestResult {
            ok: true,
            message: format!("Connection successful, model: {} ({}ms)", model, elapsed_ms),
            elapsed_ms,
            detail: String::new(),
        },
        Ok(resp) => {
            let status = resp.status();
            let summary = diag::http_summary(status, resp.headers());
            let body_text = resp.text().await.unwrap_or_default();
            TestResult {
                ok: false,
                message: diag::fail(
                    endpoint.scope,
                    "http_status",
                    format!("Connection failed {} [{}]: {}", status, summary, diag::truncate(&body_text, 300)),
                ),
                elapsed_ms,
                detail: String::new(),
            }
        }
        Err(e) => TestResult {
            ok: false,
            message: diag::fail(endpoint.scope, "http_send", format!("Request failed: {}", e)),
            elapsed_ms,
            detail: String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_for(provider: &str, extra: serde_json::Value) -> AsrProviderConfig {
        AsrProviderConfig {
            provider: provider.to_string(),
            api_key: String::new(),
            app_id: String::new(),
            extra,
        }
    }

    #[test]
    fn base_url_is_joined_into_a_chat_completions_path() {
        let expected = "https://example.com/compatible-mode/v1/chat/completions";
        assert_eq!(join_chat_url("https://example.com/compatible-mode/v1"), expected);
        assert_eq!(join_chat_url("https://example.com/compatible-mode/v1/"), expected);
        assert_eq!(join_chat_url(expected), expected);
    }

    /// 千問那一檔：空著用百鍊通用域名，填了用使用者的（業務空間專屬域名就靠這個）。
    #[test]
    fn the_qwen_endpoint_defaults_to_bailian_and_honors_an_override() {
        let endpoint = endpoint_for("qwen_chat_audio");

        let bare = config_for("qwen_chat_audio", serde_json::json!({}));
        assert_eq!(
            resolve_url(&bare, endpoint).unwrap(),
            "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions",
        );

        let workspace = config_for("qwen_chat_audio", serde_json::json!({
            "baseUrl": "https://ws-abc.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
        }));
        assert_eq!(
            resolve_url(&workspace, endpoint).unwrap(),
            "https://ws-abc.cn-beijing.maas.aliyuncs.com/compatible-mode/v1/chat/completions",
        );
    }

    #[test]
    fn the_custom_endpoint_requires_an_address() {
        let missing = config_for("openai_chat_audio", serde_json::json!({}));
        assert!(resolve_url(&missing, endpoint_for("openai_chat_audio")).is_err());
    }

    /// 音訊必須是 data URL。裸 base64 會被服務端拒，而報錯說的是「URL 無效」，
    /// 完全看不出少了字首 —— 所以這條得釘住。
    #[test]
    fn legacy_audio_is_sent_as_a_data_url() {
        let body = build_body(
            "qwen3-asr-flash",
            &pcm_to_wav(&[0, 0, 0, 0], 16000),
            None,
            None,
            AudioPayloadFormat::DataUrl,
        );
        let audio = body["messages"][0]["content"][0]["input_audio"]
            .as_str()
            .expect("input_audio must be a string");
        assert!(audio.starts_with("data:audio/wav;base64,"), "got {}", &audio[..40.min(audio.len())]);
    }

    #[test]
    fn standard_audio_is_sent_as_an_openai_object_with_raw_base64() {
        let wav = pcm_to_wav(&[1, 2, 3, 4], 16000);
        let body = build_body(
            "gemini-example",
            &wav,
            Some(DEFAULT_TRANSCRIBE_INSTRUCTION),
            None,
            AudioPayloadFormat::OpenAi,
        );
        let audio = &body["messages"][1]["content"][0]["input_audio"];
        assert_eq!(audio["format"], "wav");
        let data = audio["data"].as_str().expect("input_audio.data must be a string");
        assert!(!data.starts_with("data:"), "OpenAI input_audio.data needs raw base64");
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(data).unwrap(), wav);
        assert_eq!(body["messages"][0]["role"], "system");
    }

    /// 標準模式不許帶百鍊私有的 `asr_options`（嚴格閘道器會 400），語言改走指令。
    #[test]
    fn standard_body_carries_language_in_the_instruction_not_asr_options() {
        let body = build_body(
            "gemini-example",
            &[],
            Some(DEFAULT_TRANSCRIBE_INSTRUCTION),
            Some("ja"),
            AudioPayloadFormat::OpenAi,
        );
        assert!(body.get("asr_options").is_none(), "got {}", body);
        assert_eq!(body["messages"][0]["role"], "system");
        let text = body["messages"][0]["content"][0]["text"]
            .as_str()
            .expect("system text must be a string");
        assert!(text.starts_with(DEFAULT_TRANSCRIBE_INSTRUCTION), "got {}", text);
        assert!(text.contains("ja"), "got {}", text);
    }

    /// 百鍊形態保持原樣：語言照舊走 `asr_options`，指令不被改寫。
    #[test]
    fn data_url_body_still_sends_asr_options_language() {
        let body = build_body(
            "qwen3.8-omni-flash",
            &[],
            Some(DEFAULT_TRANSCRIBE_INSTRUCTION),
            Some("zh"),
            AudioPayloadFormat::DataUrl,
        );
        assert_eq!(body["asr_options"]["language"], "zh");
        assert_eq!(
            body["messages"][0]["content"][0]["text"],
            DEFAULT_TRANSCRIBE_INSTRUCTION
        );
    }

    #[test]
    fn payload_format_is_selected_by_provider() {
        assert!(matches!(
            audio_payload_format("openai_chat_audio_standard"),
            AudioPayloadFormat::OpenAi
        ));
        assert!(matches!(
            audio_payload_format("openai_chat_audio"),
            AudioPayloadFormat::DataUrl
        ));
        assert!(matches!(
            audio_payload_format("qwen_chat_audio"),
            AudioPayloadFormat::DataUrl
        ));
    }

    /// Omni 是對話模型，不給指令它會回答想象出來的問題（實測拿到過
    /// 「澳大利亞的首都是堪培拉」）。這條防的就是那種輸出被當成轉寫插進文件。
    #[test]
    fn omni_models_get_a_transcribe_instruction() {
        assert!(needs_transcribe_instruction("qwen3.8-omni-flash"));
        assert!(needs_transcribe_instruction("qwen3.5-omni-plus"));
        assert!(needs_transcribe_instruction("Qwen3-Omni-Flash"));
        // 純 ASR 模型不需要，給了反而多花 token
        assert!(!needs_transcribe_instruction("qwen3-asr-flash"));
        assert!(!needs_transcribe_instruction("whisper-1"));

        let omni = build_body(
            "qwen3.8-omni-flash",
            &[],
            Some(DEFAULT_TRANSCRIBE_INSTRUCTION),
            None,
            AudioPayloadFormat::DataUrl,
        );
        assert_eq!(omni["messages"][0]["role"], "system");
        assert_eq!(omni["messages"][1]["role"], "user");

        let asr =
            build_body("qwen3-asr-flash", &[], None, None, AudioPayloadFormat::DataUrl);
        assert_eq!(asr["messages"][0]["role"], "user");
        assert!(asr["messages"].as_array().unwrap().len() == 1);
    }

    #[test]
    fn auto_language_is_omitted_and_explicit_language_is_passed() {
        let auto = config_for("qwen_chat_audio", serde_json::json!({}));
        assert_eq!(resolve_language(&auto), None);
        let explicit = config_for("qwen_chat_audio", serde_json::json!({ "language": "zh" }));
        assert_eq!(resolve_language(&explicit).as_deref(), Some("zh"));

        let body =
            build_body("qwen3-asr-flash", &[], None, Some("zh"), AudioPayloadFormat::DataUrl);
        assert_eq!(body["asr_options"]["language"], "zh");
        let without =
            build_body("qwen3-asr-flash", &[], None, None, AudioPayloadFormat::DataUrl);
        assert!(without.get("asr_options").is_none());
    }

    /// content 兩種形狀都要收：字串，和多模態的內容項陣列。
    #[test]
    fn text_is_extracted_from_both_content_shapes() {
        let as_string = serde_json::json!({
            "choices": [{ "message": { "content": "  語音輸入法測試成功。 " } }]
        });
        assert_eq!(extract_text(&as_string), "語音輸入法測試成功。");

        let as_array = serde_json::json!({
            "choices": [{ "message": { "content": [
                { "type": "text", "text": "語音輸入" },
                { "type": "text", "text": "法測試成功。" },
            ] } }]
        });
        assert_eq!(extract_text(&as_array), "語音輸入法測試成功。");

        assert_eq!(extract_text(&serde_json::json!({})), "");
        assert_eq!(extract_text(&serde_json::json!({ "choices": [] })), "");
    }
}
