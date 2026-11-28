// 豆包（火山引擎）ASR 供應商
// 使用大模型錄音檔案極速版 HTTP API：一次請求即返回結果
// 接口：POST https://openspeech.bytedance.com/api/v3/auc/bigmodel/recognize/flash

use super::diag;
use super::doubao_auth::{self, DoubaoAuth};
use super::doubao_protocol;
use super::types::{AsrProviderConfig, AsrResult, TestResult};
use std::time::Instant;

const RECOGNIZE_URL: &str =
    "https://openspeech.bytedance.com/api/v3/auc/bigmodel/recognize/flash";
const RESOURCE_ID: &str = "volc.bigasr.auc_turbo";
const SCOPE: &str = "doubao/flash";

/// 將 PCM Int16 音訊轉換為 WAV 格式（火山引擎需要 WAV/MP3/OGG 格式）
fn pcm_to_wav(pcm_data: &[u8], sample_rate: u32) -> Vec<u8> {
    let num_channels: u16 = 1;
    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * u32::from(num_channels) * u32::from(bits_per_sample) / 8;
    let block_align = num_channels * bits_per_sample / 8;
    let data_size = pcm_data.len() as u32;
    let file_size = 36 + data_size;

    let mut wav = Vec::with_capacity(44 + pcm_data.len());
    // RIFF header
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&file_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    // fmt chunk
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    wav.extend_from_slice(&num_channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&bits_per_sample.to_le_bytes());
    // data chunk
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());
    wav.extend_from_slice(pcm_data);
    wav
}

/// 呼叫豆包極速版 ASR
pub async fn transcribe(
    audio_pcm_b64: &str,
    sample_rate: u32,
    config: &AsrProviderConfig,
    hotwords: &[String],
) -> Result<AsrResult, String> {
    // 解碼 base64 PCM 資料
    let pcm_data = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        audio_pcm_b64,
    )
    .map_err(|e| diag::fail(SCOPE, "decode_b64", format!("Failed to decode base64 audio: {}", e)))?;

    if pcm_data.is_empty() {
        diag::empty_result(SCOPE, "Input audio was empty; provider request was skipped");
        return Ok(AsrResult {
            text: String::new(),
            elapsed_ms: 0,
        });
    }

    // 轉換為 WAV 格式並 base64 編碼
    let wav_data = pcm_to_wav(&pcm_data, sample_rate);
    let wav_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &wav_data);

    let auth = DoubaoAuth::from_config(config);
    if let Some(missing) = auth.missing_field() {
        return Err(diag::fail_code(
            SCOPE,
            "credentials",
            "provider_bad_key",
            format!("Doubao ASR is missing {}; complete it in Settings", missing),
        ));
    }

    let audio_sec = pcm_data.len() as f64 / (sample_rate.max(1) as f64 * 2.0);
    diag::log(
        SCOPE,
        "start",
        &format!(
            "pcm_bytes={} audio_sec={:.1} rate={} hotwords={} auth={} resourceId={}",
            pcm_data.len(),
            audio_sec,
            sample_rate,
            hotwords.len(),
            auth.mode_name(),
            RESOURCE_ID
        ),
    );

    let mut request_params = serde_json::json!({
        "model_name": "bigmodel"
    });
    // 注入熱詞（如果有）：熱詞須放在 request.corpus.context
    if let Some(ctx) = doubao_protocol::build_hotword_context(hotwords) {
        request_params["corpus"] = serde_json::json!({ "context": ctx });
    }

    let body = serde_json::json!({
        "user": {
            "uid": auth.uid()
        },
        "audio": {
            "data": wav_b64
        },
        "request": request_params
    });

    let client = super::http_client::shared();
    let start = Instant::now();

    let mut req = client
        .post(RECOGNIZE_URL)
        .header("X-Api-Resource-Id", RESOURCE_ID)
        .header("X-Api-Request-Id", uuid::Uuid::new_v4().to_string())
        .header("X-Api-Sequence", "-1")
        .header("Content-Type", "application/json");
    for (name, value) in auth.headers() {
        req = req.header(name, value);
    }

    let resp = req
        .json(&body)
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await
        .map_err(|e| diag::fail(SCOPE, "http_send", format!("HTTP request failed: {}", e)))?;

    let elapsed_ms = start.elapsed().as_millis() as u64;
    let http_summary = diag::http_summary(resp.status(), resp.headers());
    doubao_auth::log_http_logid(
        &format!("flash auth={}", auth.mode_name()),
        resp.headers(),
    );

    // 檢查響應頭中的狀態碼
    let status_code = resp
        .headers()
        .get("X-Api-Status-Code")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let api_message = resp
        .headers()
        .get("X-Api-Message")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if !resp.status().is_success() {
        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        return Err(diag::fail(
            SCOPE,
            "http_status",
            format!(
                "Doubao ASR request failed with HTTP {} [{} status={} msg={}]: {}",
                status,
                http_summary,
                if status_code.is_empty() { "-" } else { &status_code },
                if api_message.is_empty() { "-" } else { &api_message },
                diag::truncate(&body_text, 200)
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

    // 檢查 API 狀態碼。
    // 之前的寫法是 `status_code != "20000000" && !status_code.is_empty()` —— 狀態頭缺失時
    // 整個檢查被跳過，一個沒有 result 的響應就這樣一路走到「文本為空」，對外表現成
    // 「未檢測到有效聲音」。現在頭缺失就記一條，後面靠 result 是否存在來判定成敗。
    if !status_code.is_empty() && status_code != "20000000" {
        // 附上人話解釋：這條訊息會經歷史記錄的「原因」直接給使用者看
        let hint = doubao_auth::explain_status_code(&status_code)
            .map(|t| format!("（{}）", t))
            .unwrap_or_default();
        return Err(diag::fail(
            SCOPE,
            "api_status",
            format!(
                "Doubao ASR error {}{}: {} [{}]",
                status_code, hint, api_message, http_summary
            ),
        ));
    }
    if status_code.is_empty() {
        diag::log(SCOPE, "missing_status_header", &http_summary);
    }

    // 提取識別文本。result 整個不存在 = 這不是一個正常的成功響應，
    // 不能靜默當成「沒識別出內容」。
    let Some(result) = data.get("result") else {
        return Err(diag::fail(
            SCOPE,
            "no_result_field",
            format!(
                "Doubao response contained no recognition result [{} status={}] {}",
                http_summary,
                if status_code.is_empty() { "-" } else { &status_code },
                diag::describe_json(&body_text)
            ),
        ));
    };

    let text = result
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    if text.is_empty() {
        diag::empty_result(
            SCOPE,
            &format!(
                "Result contained an empty transcript audio_sec={:.1} elapsed={}ms [{} status={} msg={}]",
                audio_sec,
                elapsed_ms,
                http_summary,
                if status_code.is_empty() { "-" } else { &status_code },
                if api_message.is_empty() { "-" } else { &api_message }
            ),
        );
    } else {
        diag::ok(SCOPE, elapsed_ms, text.chars().count());
    }

    Ok(AsrResult { text, elapsed_ms })
}

/// 測試豆包 ASR 連線（傳送一段極短的靜音音訊）
pub async fn test_connection(config: &AsrProviderConfig) -> TestResult {
    // 生成 0.5 秒靜音 PCM（16kHz, 16bit）
    let silence = vec![0u8; 16000]; // 0.5s * 16000 * 2 bytes = 16000 bytes
    let wav = pcm_to_wav(&silence, 16000);
    let wav_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &wav);

    let auth = DoubaoAuth::from_config(config);
    if let Some(missing) = auth.missing_field() {
        return TestResult {
            ok: false,
            message: format!("{} has not been configured", missing),
            elapsed_ms: 0,
            detail: String::new(),
        };
    }

    let body = serde_json::json!({
        "user": { "uid": auth.uid() },
        "audio": { "data": wav_b64 },
        "request": { "model_name": "bigmodel" }
    });

    let client = super::http_client::shared();
    let start = Instant::now();

    let mut req = client
        .post(RECOGNIZE_URL)
        .header("X-Api-Resource-Id", RESOURCE_ID)
        .header("X-Api-Request-Id", uuid::Uuid::new_v4().to_string())
        .header("X-Api-Sequence", "-1")
        .header("Content-Type", "application/json");
    for (name, value) in auth.headers() {
        req = req.header(name, value);
    }

    let result = req
        .json(&body)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await;

    let elapsed_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(resp) => {
            doubao_auth::log_http_logid(
                &format!("flash test auth={}", auth.mode_name()),
                resp.headers(),
            );
            let status_code = resp
                .headers()
                .get("X-Api-Status-Code")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();

            if status_code == "20000000" || status_code == "20000003" {
                // 20000003 = 靜音音訊，也算連線成功
                TestResult {
                    ok: true,
                    message: format!("Connection successful ({}ms)", elapsed_ms),
                    elapsed_ms,
                    detail: String::new(),
                }
            } else if resp.status().is_success() {
                TestResult {
                    ok: true,
                    message: format!("Connection successful ({}ms), status code: {}", elapsed_ms, status_code),
                    elapsed_ms,
                    detail: String::new(),
                }
            } else {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                TestResult {
                    ok: false,
                    message: diag::fail(
                        "doubao/flash-test",
                        "http_status",
                        format!("API error {}: {}", status, diag::truncate(&body, 100)),
                    ),
                    elapsed_ms,
                    detail: String::new(),
                }
            }
        }
        Err(e) => TestResult {
            ok: false,
            message: diag::fail(
                "doubao/flash-test",
                "http_send",
                format!("Connection failed: {}", e),
            ),
            elapsed_ms,
            detail: String::new(),
        },
    }
}
