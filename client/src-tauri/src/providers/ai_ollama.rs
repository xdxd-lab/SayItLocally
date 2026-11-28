// Ollama AI 供應商
// 呼叫本地 Ollama 的 /api/generate 介面

use super::diag;
use super::prompt::wrap_user_text;
use super::types::{AiProviderConfig, AiResult, TestResult, TextContext};
use std::time::Instant;

const SCOPE: &str = "ai/ollama";

/// [自訂] S38／S22：Ollama 專用 HTTP 客戶端。
/// - `.no_proxy()`：不走 macOS 系統代理（ClashX、Surge、公司代理等），轉錄文字不會以明文經過代理。
/// - `redirect::Policy::none()`：不跟隨任何轉址，本機位址無法被轉到外部（例如 DeepSeek）。
/// 不用 http_client::shared()（那個會走系統代理、會跟隨轉址）。
/// 建不起來時回傳錯誤，**不**退回會走代理、會轉址的預設客戶端。
static OLLAMA_CLIENT: once_cell::sync::Lazy<Result<reqwest::Client, String>> =
    once_cell::sync::Lazy::new(|| {
        reqwest::Client::builder()
            .user_agent(super::http_client::USER_AGENT)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| format!("Failed to build the local Ollama HTTP client: {}", e))
    });

/// 呼叫 Ollama 進行文本校對
pub async fn polish(
    text: &str,
    config: &AiProviderConfig,
    system_prompt: Option<&str>,
    text_context: Option<&TextContext>,
) -> Result<AiResult, String> {
    if text.trim().is_empty() {
        return Ok(AiResult {
            text: String::new(),
            elapsed_ms: 0,
        });
    }

    let url = normalize_url(&config.api_url);
    // [自訂] SC-65：預設提示詞要求以台灣正體中文回覆。
    let sys_prompt = system_prompt.unwrap_or(super::prompt::DEFAULT_PROOFREAD_SYSTEM_PROMPT);
    // Ollama /api/generate 是單一 prompt 字串，沒有 system/user 角色區分，
    // 這裡手動拼接：system prompt 在前，user 訊息（中性標籤包裹）在後。
    let combined = format!("{}\n\n{}", sys_prompt, wrap_user_text(text, text_context));

    let model = if config.model.is_empty() {
        "qwen2.5:7b"
    } else {
        &config.model
    };

    let body = serde_json::json!({
        "model": model,
        "stream": false,
        "prompt": combined
    });

    // [自訂] S38：不走代理、不跟隨轉址
    let client = OLLAMA_CLIENT
        .as_ref()
        .map_err(|e| diag::fail(SCOPE, "client_build", e.clone()))?;
    let start = Instant::now();

    diag::log(
        SCOPE,
        "start",
        &format!("model={} chars={} url={}", model, text.chars().count(), url),
    );

    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .timeout(std::time::Duration::from_secs(90))
        .send()
        .await
        .map_err(|e| diag::fail(SCOPE, "http_send", format!("Ollama request failed: {}", e)))?;

    let elapsed_ms = start.elapsed().as_millis() as u64;
    let http_summary = diag::http_summary(resp.status(), resp.headers());

    if !resp.status().is_success() {
        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        return Err(diag::fail(
            SCOPE,
            "http_status",
            format!(
                // [自訂] S62：不把回應內容寫進日誌，只記錄長度。原：diag::truncate(&body_text, 200)
                "Ollama returned error {} [{}]: <response body omitted, {} bytes>",
                status,
                http_summary,
                body_text.len()
            ),
        ));
    }

    let body_text = resp.text().await.map_err(|e| {
        diag::fail(
            SCOPE,
            "read_body",
            format!("Failed to read response: {}", e),
        )
    })?;
    let data: serde_json::Value = serde_json::from_str(&body_text).map_err(|e| {
        diag::fail(
            SCOPE,
            "parse_json",
            format!(
                // [自訂] S62：不記錄回應片段。原：response excerpt: diag::truncate(&body_text, 200)
                "Failed to parse Ollama response: {} <response body omitted, {} bytes>",
                e,
                body_text.len()
            ),
        )
    })?;

    let result_text = data
        .get("response")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    // 空回覆會靜默回落成原文 —— 使用者以為校對跑過了，其實沒有。留證。
    if result_text.is_empty() {
        diag::log(
            SCOPE,
            "empty_response_fallback_to_input",
            &format!(
                // [自訂] SCAN1-9（L-79）：只記位元組數，不記回應的鍵名或字串（原：diag::describe_json(&body_text)）。
                "Ollama returned no content; returned the original text model={} <response body omitted, {} bytes>",
                model,
                body_text.len()
            ),
        );
    } else {
        diag::ok(SCOPE, elapsed_ms, result_text.chars().count());
    }

    Ok(AiResult {
        text: if result_text.is_empty() {
            text.to_string()
        } else {
            result_text
        },
        elapsed_ms,
    })
}

/// 測試 Ollama 連線 — 實際呼叫模型，驗證模型是否可用
pub async fn test_connection(config: &AiProviderConfig) -> TestResult {
    let url = normalize_url(&config.api_url);

    let model = if config.model.is_empty() {
        "qwen2.5:7b"
    } else {
        &config.model
    };

    let prompt = "Reply with OK only. Do not output anything else.";

    let body = serde_json::json!({
        "model": model,
        "stream": false,
        "prompt": prompt
    });

    // [自訂] S38：不走代理、不跟隨轉址
    let client = match OLLAMA_CLIENT.as_ref() {
        Ok(client) => client,
        Err(e) => {
            return TestResult {
                ok: false,
                message: diag::fail("ai/ollama-test", "client_build", e.clone()),
                elapsed_ms: 0,
                detail: String::new(),
            }
        }
    };
    let start = Instant::now();

    let result = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await;

    let elapsed_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(resp) if resp.status().is_success() => {
            let data: serde_json::Value = resp.json().await.unwrap_or_default();
            let reply = data
                .get("response")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let detail = format!(
                "Elapsed: {}ms\nModel: {}\nSent: \"{}\"\nReply: {}",
                elapsed_ms,
                model,
                prompt,
                if reply.is_empty() { "(empty)" } else { &reply }
            );
            TestResult {
                ok: true,
                message: format!("Connection successful ({}ms)", elapsed_ms),
                elapsed_ms,
                detail,
            }
        }
        Ok(resp) => {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            TestResult {
                ok: false,
                message: diag::fail(
                    "ai/ollama-test",
                    "http_status",
                    format!(
                        // [自訂] S62：不記錄回應片段。原：diag::truncate(&body_text, 100)
                        "Ollama returned {}: <response body omitted, {} bytes>",
                        status,
                        body_text.len()
                    ),
                ),
                elapsed_ms,
                detail: format!("Model: {}\nRequest URL: {}", model, url),
            }
        }
        Err(e) => TestResult {
            ok: false,
            message: diag::fail(
                "ai/ollama-test",
                "http_send",
                format!("Connection failed: {}", e),
            ),
            elapsed_ms,
            detail: format!("Model: {}\nRequest URL: {}", model, url),
        },
    }
}

fn normalize_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    if trimmed.ends_with("/api/generate") {
        trimmed.to_string()
    } else if trimmed.ends_with("/api") {
        format!("{}/generate", trimmed)
    } else {
        format!("{}/api/generate", trimmed)
    }
}
