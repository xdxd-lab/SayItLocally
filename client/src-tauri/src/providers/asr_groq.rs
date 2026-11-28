// OpenAI 形態的 `POST /audio/transcriptions` —— 目前接了兩家：
//   · Groq（whisper-large-v3-turbo / whisper-large-v3）
//   · OpenAI 官方（gpt-transcribe / gpt-4o-transcribe / gpt-4o-mini-transcribe / whisper-1）
//
// 兩家共用這一份實現：端點路徑、multipart 欄位、響應形狀完全一樣，只有 base URL 和
// 可用模型不同。**不要為 OpenAI 再抄一份檔案** —— 那會讓標點 prompt、WAV 封裝、
// 空結果日誌這些真正有內容的部分各自漂移。
//
// **multipart 上傳音訊檔案**，這是本專案唯一用這種形態的 ASR 供應商。其餘幾家
// （豆包 / 千問 / MiMo）要麼是 WebSocket 協議，要麼是 chat/completions + base64
// data URL，都不是這個端點。
//
// 與 asr_mimo.rs 的差異：
//   · 鑑權是 Bearer（MiMo 是 api-key 頭）；
//   · 音訊以 multipart 的 file 欄位上傳，不是塞進 JSON 的 data URL —— 十幾秒的音訊
//     用 base64 塞 JSON 會膨脹 1/3，multipart 是原始位元組；
//   · 響應是 `{"text": "..."}`，不是 OpenAI chat 的 choices[0].message.content。
//
// 端點寫死在這裡而不做成可配置項：ASR 側的約定是「供應商只能從內建清單裡選」，
// 因為每家的協議都要一份專門實現，能填任意地址只會讓使用者以為隨便填個地址就能用。
// 模型則是可選的（extra.model），因為換模型不改協議 —— 前端目錄裡那份清單說明了
// 每家有哪些，見 features/settings/asrProviderCatalog.ts 的 AsrProviderEntry.models。

use super::diag;
use super::types::{AsrProviderConfig, AsrResult, TestResult};
use std::time::Instant;

/// 一家「OpenAI 形態轉寫」的接入點。
struct Endpoint {
    url: &'static str,
    /// 前端沒指定模型時用它
    default_model: &'static str,
    /// 日誌作用域，出問題時要一眼看出是哪家
    scope: &'static str,
    /// 前端選定的 `extra.model` 在這個端點上是否可用。
    ///
    /// `openai_live_transcribe` 選的是**流式**模型（gpt-live-transcribe），而
    /// `/audio/transcriptions` 上沒有這個模型。那張卡關掉即時字幕時會回落到這條
    /// HTTP 路徑（見 registry.rs），照著把流式模型名發出去只會換來一個含義模糊的
    /// 400 —— 而使用者完全不知道自己發了個檔案端點不認識的模型。所以這一檔忽略
    /// 選定值，一律用 default_model。
    honors_selected_model: bool,
    /// 使用者填了 `extra.baseUrl` 就用他的，空著用上面的 `url`。
    ///
    /// 為什麼內建幾家也開：拿中轉站 / 反代接 OpenAI 很常見 —— 協議是官方那套，
    /// 只是換了個域名。不給填等於逼這些使用者用不了。
    /// 安全邊界在前端：只有標了 `supportsCustomUrl` 的模型才會把地址發下來，
    /// 所以檔案裡的殘留地址到不了這裡（見 asrEndpointUrl）。
    allows_custom_url: bool,
    /// 地址**必填**（協議卡）。此時 `url` 是空串，沒有可回落的內建地址。
    requires_custom_url: bool,
    /// 第三方實現對 `prompt` 的態度不一，有的直接 400。寬容檔位在失敗後去掉它重試一次。
    ///
    /// 官方那幾家不需要這層：它們都接受 prompt，而 prompt 是中文標點的唯一來源
    /// （見 PUNCTUATION_PROMPT），無條件丟掉會讓轉寫結果變差。
    retry_without_prompt: bool,
}

const GROQ: Endpoint = Endpoint {
    url: "https://api.groq.com/openai/v1/audio/transcriptions",
    default_model: "whisper-large-v3-turbo",
    scope: "groq/asr",
    honors_selected_model: true,
    allows_custom_url: true,
    requires_custom_url: false,
    retry_without_prompt: false,
};

const OPENAI: Endpoint = Endpoint {
    url: "https://api.openai.com/v1/audio/transcriptions",
    default_model: "gpt-transcribe",
    scope: "openai/asr",
    honors_selected_model: true,
    allows_custom_url: true,
    requires_custom_url: false,
    retry_without_prompt: false,
};

/// OpenAI 流式那張卡回落到檔案端點時用的檔位。
/// 端點與 OPENAI 相同，區別只有一個：不接受選定模型（那是流式專用的）。
const OPENAI_FILE_FALLBACK: Endpoint = Endpoint {
    url: OPENAI.url,
    default_model: "gpt-transcribe",
    scope: "openai/asr-fallback",
    honors_selected_model: false,
    // 流式那個模型在前端沒標 supportsCustomUrl，所以地址不會發下來；
    // 這裡也顯式關掉，免得「我沒給這個模型填地址、回落時卻用了別處的地址」。
    allows_custom_url: false,
    requires_custom_url: false,
    retry_without_prompt: false,
};

/// 「OpenAI 相容轉寫」那張卡：協議固定、地址由使用者填。
///
/// 對面可能是 whisper.cpp / faster-whisper / FunASR 的 OpenAI 相容服務、矽基流動，
/// 或者某個聚合閘道器。所以這一檔比內建幾家寬容：prompt 被拒時去掉重試，
/// 響應不是 JSON 時按純文本收（見 extract_text_or_plain）。
const OPENAI_COMPAT: Endpoint = Endpoint {
    url: "",
    default_model: "whisper-1",
    scope: "openai-compat/asr",
    honors_selected_model: true,
    allows_custom_url: true,
    requires_custom_url: true,
    retry_without_prompt: true,
};

/// provider id → 接入點。未知 id 回落到 Groq（registry 只會把這幾個 id 分發過來，
/// 加一家時這裡和 registry 要一起改）。
fn endpoint_for(provider: &str) -> &'static Endpoint {
    match provider {
        "openai_transcribe" => &OPENAI,
        "openai_live_transcribe" => &OPENAI_FILE_FALLBACK,
        "openai_compat_transcribe" => &OPENAI_COMPAT,
        _ => &GROQ,
    }
}

/// 把使用者填的 base URL 接成完整的轉寫地址。
///
/// 使用者會填成什麼樣無法預設，實際見過這三種，都要能用：
///   · `http://127.0.0.1:8000/v1`            → 補 `/audio/transcriptions`
///   · `http://127.0.0.1:8000/v1/`           → 同上，先去掉尾斜線
///   · `http://.../v1/audio/transcriptions`  → 已經是完整地址，原樣用
/// 不做第四種猜測（比如自動補 `/v1`）：猜錯會打到一個存在但語義不同的路徑上，
/// 那種失敗比「404 地址不對」難查得多。
fn join_transcriptions_url(base: &str) -> String {
    let trimmed = base.trim().trim_end_matches('/');
    if trimmed.ends_with("/audio/transcriptions") {
        trimmed.to_string()
    } else {
        format!("{}/audio/transcriptions", trimmed)
    }
}

/// 本次要打的地址。
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
        return Ok(join_transcriptions_url(supplied));
    }
    if endpoint.requires_custom_url {
        return Err(diag::fail(
            endpoint.scope,
            "missing_base_url",
            "No endpoint address configured for this service".to_string(),
        ));
    }
    Ok(endpoint.url.to_string())
}

/// 本次要用的模型：前端選定的（extra.model）優先，否則用該家的預設。
///
/// 不校驗模型名是否屬於這一家 —— 服務商上下線模型比我們改程式碼勤，硬校驗只會讓
/// 新模型必須等一次客戶端更新才能用。前端目錄已經限定了可選項，這裡只兜住空值，
/// 以及 `honors_selected_model = false` 那一檔（說明見 Endpoint 上的註釋）。
fn resolve_model(config: &AsrProviderConfig, endpoint: &Endpoint) -> String {
    if !endpoint.honors_selected_model {
        return endpoint.default_model.to_string();
    }
    config
        .extra
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(endpoint.default_model)
        .to_string()
}

/// 標點引導。**這不是可選的潤色，缺了它短句中文轉寫會一個標點都沒有。**
///
/// Whisper 的 `prompt` 不是指令，而是「上文樣例」：解碼時模型會模仿它的書寫風格。
/// 所以想要標點，prompt 自己就必須是**帶標點的句子** —— 寫成「請加標點」毫無作用。
///
/// 實測（dev-scripts/groq-asr-punct-probe.mjs，打的是真實介面）：
///   · 中文短句、不帶 prompt   → "語音輸入法測試成功"     一個標點都沒有
///   · 中文短句、帶這個 prompt → "語音輸入法測試成功。"
///   · 英文                   → 兩種情況都帶標點，加了不變差
///
/// 為什麼寫成中英混排：預設語言是 auto。實測純中文 prompt 並不會把英文音訊帶偏
/// （輸出仍是純英文），而混排能讓兩種語言都拿到標點，且都不會被改寫成另一種語言。
///
/// **它管不了全形還是半形。** 另一輪實測（groq-asr-comma-probe.mjs，用一段真的需要
/// 逗號的中文語音）顯示：四種 prompt 連同完全不給 prompt，輸出一字不差 ——
/// 逗號和問號是半形、句號是全形，混著來。那是模型自身行為，改 prompt 沒有任何影響。
/// 半形轉全形在前端 textPostProcess.ts 的 normalizeChinesePunctuation 裡做。
///
/// 另外驗證過靜音不會把這段文字當成轉寫結果吐回來（那會被直接插進使用者的文件）。
const PUNCTUATION_PROMPT: &str = "以下是中文轉寫，使用全形標點，例如逗號、句號和問號。The following is an English transcript with half-width punctuation, such as commas, periods, and question marks.";

/// 將 16kHz 單聲道 16-bit PCM 封裝為 WAV 容器。
///
/// 前端一路傳下來的都是裸 PCM（見 types.rs 的 CloudTranscribeRequest），而
/// /audio/transcriptions 按檔名與內容嗅探格式，裸 PCM 它認不出來。
/// 這 18 行與 asr_mimo.rs 裡那份是同一個 WAV 頭，刻意各留一份：抽成公共函式後
/// 任何一家改取樣格式都會牽動另一家，而它們本來毫無關係。
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

/// 識別語言：設定裡存的是 auto|zh|en，Whisper 只接受 ISO-639-1。
///
/// auto 必須**整個省略 language 欄位**，不能傳字串 "auto" —— Whisper 會把它當成
/// 一個不存在的語言代碼，行為不確定（可能報 400，也可能默默按英語轉寫）。
/// 返回 None 表示讓服務端自己檢測。
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

fn build_form(
    wav: Vec<u8>,
    model: &str,
    language: Option<&str>,
    scope: &str,
    with_prompt: bool,
) -> Result<reqwest::multipart::Form, String> {
    let part = reqwest::multipart::Part::bytes(wav)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|e| diag::fail(scope, "build_form", format!("Failed to build the audio part: {}", e)))?;

    let mut form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("model", model.to_string())
        // 顯式要 json：不少第三方實現的預設 response_format 不是 json
        // （有的給純文本、有的給 srt），不寫這一行就得靠運氣。
        .text("response_format", "json");
    if with_prompt {
        form = form.text("prompt", PUNCTUATION_PROMPT);
    }
    if let Some(lang) = language {
        form = form.text("language", lang.to_string());
    }
    Ok(form)
}

/// 響應形狀是 `{"text": "..."}`，比 chat/completions 簡單得多。
fn extract_text(data: &serde_json::Value) -> String {
    data.get("text")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// 同上，但響應不是 JSON 時按純文本收。
///
/// 只給「地址由使用者填」那一檔用。第三方實現即使收到 `response_format=json` 也可能
/// 回一段裸文本（whisper.cpp 的若干包裝就是這樣）。這時候硬報 parse_json 等於
/// 把一次**成功的轉寫**說成失敗，而使用者看不出問題在哪一層。
///
/// 反過來不能對內建幾家開這個口子：那邊回非 JSON 說明出了真問題（閘道器錯誤頁、
/// 被劫持的響應），當成轉寫文本插進使用者文件比報錯糟得多。
fn extract_text_or_plain(body: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(data) => extract_text(&data),
        Err(_) => body.trim().to_string(),
    }
}

/// 響應體像不像是在抱怨 `prompt` 這個欄位。
///
/// 只在這一個判據成立時才去掉 prompt 重試，不做「失敗就重試」—— 金鑰錯、模型不存在
/// 這類 400 一旦也觸發重試，每次轉寫都要白發兩遍（AI 潤色那邊的降級判定踩過同一個坑，
/// 見 decisions.md「預設替所有 OpenAI 相容端點關閉思考」）。
fn complains_about_prompt(body: &str) -> bool {
    let lower = body.to_lowercase();
    lower.contains("prompt")
        && (lower.contains("unknown")
            || lower.contains("unexpected")
            || lower.contains("not allowed")
            || lower.contains("unsupported")
            || lower.contains("extra")
            || lower.contains("invalid"))
}

pub async fn transcribe(
    audio_pcm_b64: &str,
    sample_rate: u32,
    config: &AsrProviderConfig,
    hotwords: &[String],
) -> Result<AsrResult, String> {
    let endpoint = endpoint_for(&config.provider);
    let scope = endpoint.scope;

    // 這條路不把熱詞發給服務端 —— `prompt` 是它唯一能放詞表的位置，而我們拿它做了
    // 中文標點引導（見 PUNCTUATION_PROMPT，缺了它中文短句一個標點都沒有）。
    //
    // 留這行日誌的理由：熱詞被忽略時轉寫照樣成功，使用者只覺得"識別得不太準"，
    // 而在這行存在之前**日誌裡一個字都查不到**。issue #67 是 FunASR 上游維護者
    // 讀原始碼發現的，不是從日誌裡看出來的 —— 那說明這個缺口對我們自己也是不可見的。
    // 能力宣告在 providers/capabilities.rs，這裡只負責留痕。
    if !hotwords.is_empty() {
        diag::log(
            scope,
            "hotwords_ignored",
            &format!(
                "count={} reason=prompt_field_used_for_punctuation",
                hotwords.len()
            ),
        );
    }
    let pcm = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        audio_pcm_b64,
    )
    .map_err(|e| diag::fail(scope, "decode_b64", format!("Failed to decode base64 audio: {}", e)))?;

    if pcm.is_empty() {
        diag::empty_result(scope, "Input audio was empty; provider request was skipped");
        return Ok(AsrResult { text: String::new(), elapsed_ms: 0 });
    }

    let audio_sec = pcm.len() as f64 / (sample_rate.max(1) as f64 * 2.0);
    let language = resolve_language(config);
    let model = resolve_model(config, endpoint);
    let url = resolve_url(config, endpoint)?;
    let wav = pcm_to_wav(&pcm, sample_rate);

    diag::log(
        scope,
        "start",
        &format!(
            "model={} url={} wav_bytes={} audio_sec={:.1} rate={} language={}",
            model,
            url,
            wav.len(),
            audio_sec,
            sample_rate,
            language.as_deref().unwrap_or("auto(omitted)")
        ),
    );

    let client = super::http_client::shared();
    let start = Instant::now();

    // 第一次帶 prompt（中文標點全靠它）。只有「地址由使用者填」那一檔、且服務端明確
    // 抱怨 prompt 時，才去掉它重試一次 —— 判據見 complains_about_prompt。
    let mut with_prompt = true;
    let (body_text, http_summary, elapsed_ms) = loop {
        let form = build_form(wav.clone(), &model, language.as_deref(), scope, with_prompt)?;
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", config.api_key))
            .multipart(form)
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await
            .map_err(|e| diag::fail(scope, "http_send", format!("Request failed: {}", e)))?;

        let elapsed_ms = start.elapsed().as_millis() as u64;
        let http_summary = diag::http_summary(resp.status(), resp.headers());

        if resp.status().is_success() {
            let body_text = resp.text().await.map_err(|e| {
                diag::fail(scope, "read_body", format!("Failed to read response: {}", e))
            })?;
            break (body_text, http_summary, elapsed_ms);
        }

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if with_prompt && endpoint.retry_without_prompt && complains_about_prompt(&body) {
            diag::log(
                scope,
                "retry_without_prompt",
                &format!(
                    "The endpoint rejected the prompt field; retrying without it \
                     (Chinese punctuation may be missing). {}",
                    diag::truncate(&body, 200)
                ),
            );
            with_prompt = false;
            continue;
        }
        return Err(diag::fail(
            scope,
            "http_status",
            format!(
                "Transcription error {} [{}]: {}",
                status,
                http_summary,
                diag::truncate(&body, 300)
            ),
        ));
    };

    // 只有**協議卡**容忍裸文本響應，內建幾家仍然要求 JSON（理由見函式註釋）。
    //
    // ⚠️ 這裡判的是 requires_custom_url 而不是 allows_custom_url：後者現在對
    // Groq / OpenAI 也是真的（它們支援換成中轉站地址），拿它當判據會讓這兩家
    // 也開始把非 JSON 響應當轉寫文本收 —— 那種響應在官方端點上意味著真出了問題
    // （閘道器錯誤頁、被劫持的響應），當成文本插進使用者文件比報錯糟得多。
    let text = if endpoint.requires_custom_url {
        extract_text_or_plain(&body_text)
    } else {
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
        extract_text(&data)
    };
    if text.is_empty() {
        // 空結果走成功路徑，前端只會顯示「未檢測到有效聲音」。不留這條日誌的話，
        // 「真的沒說話」和「這次呼叫其實失敗了」就再也分不開（見 pitfalls #15）。
        diag::empty_result(
            scope,
            &format!(
                "Response contained no transcript audio_sec={:.1} elapsed={}ms [{}] {}",
                audio_sec,
                elapsed_ms,
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
    // 0.5s 靜音：這些模型會返回空文本但 HTTP 200，足以驗證金鑰與網路是否可用。
    // 音訊再短會被服務端以「too short」拒掉，那就分不清是金鑰問題還是音訊問題了。
    let silence = vec![0u8; 16000];
    let wav = pcm_to_wav(&silence, 16000);
    let model = resolve_model(config, endpoint);

    // 地址沒填時在這裡就給出明確結論，而不是拿一個空 URL 去發請求 ——
    // 那會換來一條含義模糊的傳輸層錯誤。
    let url = match resolve_url(config, endpoint) {
        Ok(u) => u,
        Err(e) => {
            return TestResult {
                ok: false,
                message: e,
                elapsed_ms: 0,
                detail: String::new(),
            }
        }
    };

    let form = match build_form(wav, &model, None, endpoint.scope, true) {
        Ok(f) => f,
        Err(e) => {
            return TestResult {
                ok: false,
                message: e,
                elapsed_ms: 0,
                detail: String::new(),
            }
        }
    };

    let client = super::http_client::shared();
    let start = Instant::now();

    let result = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .multipart(form)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await;

    let elapsed_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(resp) if resp.status().is_success() => TestResult {
            ok: true,
            message: format!("Connection successful ({}ms)", elapsed_ms),
            elapsed_ms,
            detail: String::new(),
        },
        Ok(resp) => {
            let status = resp.status();
            let summary = diag::http_summary(status, resp.headers());
            let body = resp.text().await.unwrap_or_default();
            TestResult {
                ok: false,
                message: diag::fail(
                    &format!("{}-test", endpoint.scope),
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
            message: diag::fail(
                &format!("{}-test", endpoint.scope),
                "http_send",
                format!("Connection failed: {}", e),
            ),
            elapsed_ms,
            detail: String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_language(value: serde_json::Value) -> AsrProviderConfig {
        config_for("groq_whisper", value)
    }

    fn config_for(provider: &str, extra: serde_json::Value) -> AsrProviderConfig {
        AsrProviderConfig {
            provider: provider.to_string(),
            api_key: String::new(),
            app_id: String::new(),
            extra,
        }
    }

    /// 使用者填地址的三種常見寫法都要接對。
    ///
    /// 接錯的表現是 404，而 404 在這個場景下歧義極大 —— 使用者會以為自己的服務沒起來、
    /// 或者模型名寫錯了，很難想到是客戶端把路徑拼壞了。
    #[test]
    fn base_url_is_joined_into_a_transcriptions_path() {
        let expected = "http://127.0.0.1:8000/v1/audio/transcriptions";
        assert_eq!(join_transcriptions_url("http://127.0.0.1:8000/v1"), expected);
        assert_eq!(join_transcriptions_url("http://127.0.0.1:8000/v1/"), expected);
        assert_eq!(join_transcriptions_url("  http://127.0.0.1:8000/v1  "), expected);
        // 已經是完整地址就別再接一層
        assert_eq!(join_transcriptions_url(expected), expected);
        assert_eq!(join_transcriptions_url(&format!("{expected}/")), expected);
    }

    /// 地址沒填時必須當場失敗，而不是拿空 URL 去發請求。
    #[test]
    fn a_custom_endpoint_without_an_address_fails_before_sending() {
        let missing = config_for("openai_compat_transcribe", serde_json::json!({}));
        assert!(resolve_url(&missing, endpoint_for("openai_compat_transcribe")).is_err());
        let blank = config_for("openai_compat_transcribe", serde_json::json!({ "baseUrl": "   " }));
        assert!(resolve_url(&blank, endpoint_for("openai_compat_transcribe")).is_err());

        let filled = config_for(
            "openai_compat_transcribe",
            serde_json::json!({ "baseUrl": "http://localhost:9000/v1" }),
        );
        assert_eq!(
            resolve_url(&filled, endpoint_for("openai_compat_transcribe")).unwrap(),
            "http://localhost:9000/v1/audio/transcriptions",
        );
    }

    /// 內建幾家：地址**空著用官方的、填了用使用者的**。
    ///
    /// 允許覆蓋是因為中轉站 / 反代很常見（協議是官方那套，只換域名）。
    /// 安全邊界不在這裡，在前端：只有標了 supportsCustomUrl 的模型才會把地址發下來，
    /// 所以檔案裡的殘留地址到不了這一層（見 asrEndpointUrl 的註釋）。
    #[test]
    fn builtin_endpoints_default_to_official_and_honor_an_override() {
        for (provider, official) in [
            ("groq_whisper", "api.groq.com"),
            ("openai_transcribe", "api.openai.com"),
        ] {
            let endpoint = endpoint_for(provider);

            let bare = config_for(provider, serde_json::json!({}));
            assert!(resolve_url(&bare, endpoint).unwrap().contains(official));
            let blank = config_for(provider, serde_json::json!({ "baseUrl": "   " }));
            assert!(resolve_url(&blank, endpoint).unwrap().contains(official));

            let relayed = config_for(
                provider,
                serde_json::json!({ "baseUrl": "https://relay.example/v1" }),
            );
            assert_eq!(
                resolve_url(&relayed, endpoint).unwrap(),
                "https://relay.example/v1/audio/transcriptions",
            );
        }
    }

    /// 流式那張卡回落到檔案端點時**不**用使用者地址。
    ///
    /// 前端沒給那個模型標 supportsCustomUrl，所以正常路徑上地址根本不會發下來；
    /// 這裡再關一道，免得「我沒給這個模型填地址、它回落時卻用了別處的地址」。
    #[test]
    fn the_live_card_fallback_never_uses_a_custom_url() {
        let config = config_for(
            "openai_live_transcribe",
            serde_json::json!({ "baseUrl": "https://relay.example/v1" }),
        );
        let url = resolve_url(&config, endpoint_for("openai_live_transcribe")).unwrap();
        assert!(url.contains("api.openai.com"), "got {url}");
    }

    /// 去掉 prompt 重試的判據要窄：只在服務端確實在抱怨這個欄位時才重試。
    ///
    /// 放寬成「失敗就重試」會讓金鑰錯、模型不存在這類 400 每次都白發兩遍請求。
    #[test]
    fn prompt_retry_only_triggers_on_complaints_about_prompt() {
        assert!(complains_about_prompt(
            r#"{"error":{"message":"Unknown parameter: 'prompt'"}}"#
        ));
        assert!(complains_about_prompt(
            r#"{"detail":"Extra inputs are not permitted: prompt"}"#
        ));
        assert!(complains_about_prompt("unsupported field prompt"));

        assert!(!complains_about_prompt(r#"{"error":{"message":"Invalid API key"}}"#));
        assert!(!complains_about_prompt(r#"{"error":{"message":"model not found"}}"#));
        assert!(!complains_about_prompt("Internal Server Error"));
        // 只提到 prompt、但沒說它有問題的，也不該觸發重試
        assert!(!complains_about_prompt("your prompt was transcribed"));
    }

    /// 只有**協議卡**才容忍裸文本響應，內建幾家必須要求 JSON。
    ///
    /// 判據是 requires_custom_url，不是 allows_custom_url —— 後者現在對 Groq /
    /// OpenAI 也是真的（它們支援換中轉站地址）。搞混的後果是這兩家也開始把
    /// 閘道器錯誤頁當成轉寫文本插進使用者文件。
    #[test]
    fn only_the_protocol_card_tolerates_a_plain_text_body() {
        assert!(endpoint_for("openai_compat_transcribe").requires_custom_url);
        assert!(!endpoint_for("groq_whisper").requires_custom_url);
        assert!(!endpoint_for("openai_transcribe").requires_custom_url);
        assert!(!endpoint_for("openai_live_transcribe").requires_custom_url);

        assert_eq!(extract_text_or_plain(r#"{"text":"來自 JSON"}"#), "來自 JSON");
        assert_eq!(extract_text_or_plain("  裸文本響應  "), "裸文本響應");
    }

    /// 兩家共用這份實現，分發只靠 provider id —— 認錯一家就會把請求打到另一家的
    /// 域名上，而拿到的只會是一個 401，看不出是路由錯了。
    #[test]
    fn provider_id_picks_the_right_endpoint() {
        let groq = endpoint_for("groq_whisper");
        assert!(groq.url.contains("api.groq.com"));
        assert_eq!(groq.default_model, "whisper-large-v3-turbo");

        let openai = endpoint_for("openai_transcribe");
        assert!(openai.url.contains("api.openai.com"));
        assert_eq!(openai.default_model, "gpt-transcribe");

        // 兩家的日誌作用域必須能區分，否則日誌裡看不出這次是打給誰的
        assert_ne!(groq.scope, openai.scope);
    }

    /// OpenAI 流式那張卡關掉字幕會回落到檔案端點，而它選的模型
    /// （gpt-live-transcribe）在那個端點上不存在 —— 必須被換成 gpt-transcribe。
    /// 這條釘住的是「回落之後靜默 400」這個坑。
    #[test]
    fn live_card_falling_back_ignores_the_streaming_model() {
        let endpoint = endpoint_for("openai_live_transcribe");
        assert!(endpoint.url.contains("api.openai.com"));
        assert!(!endpoint.honors_selected_model);
        assert_eq!(
            resolve_model(
                &config_for(
                    "openai_live_transcribe",
                    serde_json::json!({ "model": "gpt-live-transcribe" })
                ),
                endpoint
            ),
            "gpt-transcribe"
        );
    }

    /// 前端選定的模型要真的被用上。這條釘住的是使用者報的那個缺口：
    /// 目錄裡加了 whisper-large-v3，但如果這裡仍用常量，選了也還是發 turbo。
    #[test]
    fn selected_model_overrides_the_default() {
        let groq = endpoint_for("groq_whisper");
        assert_eq!(
            resolve_model(&config_for("groq_whisper", serde_json::json!({"model": "whisper-large-v3"})), groq),
            "whisper-large-v3"
        );
        let openai = endpoint_for("openai_transcribe");
        assert_eq!(
            resolve_model(&config_for("openai_transcribe", serde_json::json!({"model": "gpt-4o-mini-transcribe"})), openai),
            "gpt-4o-mini-transcribe"
        );
    }

    /// 沒選、空串、只有空白，一律回落到該家的預設，絕不把空模型名發出去
    /// （服務端只會回一個含義模糊的 400）。
    #[test]
    fn blank_model_falls_back_to_default() {
        let groq = endpoint_for("groq_whisper");
        assert_eq!(
            resolve_model(&config_for("groq_whisper", serde_json::json!({})), groq),
            "whisper-large-v3-turbo"
        );
        assert_eq!(
            resolve_model(&config_for("groq_whisper", serde_json::json!({"model": "   "})), groq),
            "whisper-large-v3-turbo"
        );
        assert_eq!(
            resolve_model(&config_for("openai_transcribe", serde_json::json!({"model": ""})), endpoint_for("openai_transcribe")),
            "gpt-transcribe"
        );
    }

    /// auto 必須省略 language 欄位，而不是把 "auto" 當語言代碼發出去。
    #[test]
    fn auto_language_is_omitted() {
        assert_eq!(resolve_language(&config_with_language(serde_json::json!({}))), None);
        assert_eq!(
            resolve_language(&config_with_language(serde_json::json!({"language": "auto"}))),
            None
        );
        assert_eq!(
            resolve_language(&config_with_language(serde_json::json!({"language": "  "}))),
            None
        );
    }

    #[test]
    fn explicit_language_is_passed_through() {
        assert_eq!(
            resolve_language(&config_with_language(serde_json::json!({"language": "zh"}))),
            Some("zh".to_string())
        );
    }

    /// WAV 頭必須是 44 位元組，且長度欄位要跟資料長度對得上 —— 寫錯的話服務端只會回
    /// 一個含義模糊的 400，很難往回定位到這裡。
    #[test]
    fn wav_header_is_well_formed() {
        let pcm = vec![0u8; 320];
        let wav = pcm_to_wav(&pcm, 16000);
        assert_eq!(wav.len(), 44 + pcm.len());
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        let riff_size = u32::from_le_bytes([wav[4], wav[5], wav[6], wav[7]]);
        assert_eq!(riff_size as usize, 36 + pcm.len());
        let data_size = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]);
        assert_eq!(data_size as usize, pcm.len());
    }

    /// 這個 prompt 是靠「示範」生效的：它自己不帶標點就一點作用都沒有，
    /// 而少了其中一種文字，auto 路徑下那種語言就會失去引導。
    /// 有人圖省事把它改短時，這條會攔住。
    ///
    /// 注意它**不負責**全形/半形 —— 實測改 prompt 對寬度毫無影響，
    /// 那件事在前端 normalizeChinesePunctuation 裡做。
    #[test]
    fn punctuation_prompt_demonstrates_punctuation_in_both_scripts() {
        assert!(PUNCTUATION_PROMPT.contains('。'), "缺中文句號，中文轉寫會沒有標點");
        assert!(PUNCTUATION_PROMPT.contains('，'), "缺中文逗號");
        assert!(PUNCTUATION_PROMPT.contains('.'), "缺英文句號");
        assert!(PUNCTUATION_PROMPT.contains(','), "缺英文逗號");
        assert!(
            PUNCTUATION_PROMPT.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "缺中文字符"
        );
        assert!(
            PUNCTUATION_PROMPT.chars().any(|c| c.is_ascii_alphabetic()),
            "缺英文字符"
        );
    }

    #[test]
    fn extracts_text_field() {
        let data = serde_json::json!({ "text": "  hello there  " });
        assert_eq!(extract_text(&data), "hello there");
        assert_eq!(extract_text(&serde_json::json!({})), "");
    }
}
