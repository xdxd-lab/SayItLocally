// 豆包流式語音識別 2.0 — 使用流式輸入模式（bigmodel_nostream）
// 錄完後一次性傳送 PCM 音訊，等最終結果返回

use super::diag;
use super::doubao_auth::{self, DoubaoAuth};
use super::doubao_protocol;
use super::types::{AsrProviderConfig, AsrResult, TestResult};
use futures_util::{SinkExt, StreamExt};
use std::time::Instant;
use tokio_tungstenite::tungstenite;

const WS_URL: &str = "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_nostream";
const SCOPE: &str = "doubao/nostream";

/// 建立 nostream 連線：按「小時版 → 併發版」依次試資源 ID。
///
/// 之前這裡寫死小時版，而雙向流式路徑早就在兩者間兜底了 —— 於是只開通併發版的賬號
/// 出現「即時字幕能用、普通錄音識別連不上」這種自相矛盾的現象。兩條路徑口徑統一。
async fn connect(
    auth: &DoubaoAuth<'_>,
    scope: &str,
) -> Result<(tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, &'static str), String> {
    let mut last_err = String::from("Unknown error");
    for resource_id in doubao_auth::SAUC_RESOURCE_CANDIDATES {
        let mut builder = tungstenite::http::Request::builder()
            .uri(WS_URL)
            .header("Host", "openspeech.bytedance.com")
            .header("X-Api-Resource-Id", *resource_id)
            .header("X-Api-Connect-Id", uuid::Uuid::new_v4().to_string())
            .header(
                "Sec-WebSocket-Key",
                tungstenite::handshake::client::generate_key(),
            )
            .header("Sec-WebSocket-Version", "13")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket");
        for (name, value) in auth.headers() {
            builder = builder.header(name, value);
        }
        let request = builder
            .body(())
            .map_err(|e| diag::fail(scope, "build_request", format!("Failed to build request: {}", e)))?;

        match tokio_tungstenite::connect_async(request).await {
            Ok((ws, response)) => {
                doubao_auth::log_ws_logid(
                    &format!("{} resourceId={} auth={}", scope, resource_id, auth.mode_name()),
                    &response,
                );
                return Ok((ws, resource_id));
            }
            Err(e) => {
                last_err = e.to_string();
                // 逐個資源都記：只開通了併發版的賬號會在小時版這一輪失敗一次再成功，
                // 看到這條不代表最終失敗，但它是判斷「賬號開的是哪種計費」的直接證據。
                diag::log(
                    scope,
                    "connect_attempt_failed",
                    &format!(
                        "resourceId={} auth={} err={}",
                        resource_id,
                        auth.mode_name(),
                        diag::truncate(&last_err, 200)
                    ),
                );
            }
        }
    }
    Err(diag::fail(
        scope,
        "connect",
        format!("WebSocket connection failed: {}", last_err),
    ))
}

pub async fn transcribe(
    audio_pcm_b64: &str,
    sample_rate: u32,
    config: &AsrProviderConfig,
    hotwords: &[String],
) -> Result<AsrResult, String> {
    let pcm_data = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        audio_pcm_b64,
    )
    .map_err(|e| diag::fail(SCOPE, "decode_b64", format!("Failed to decode base64 audio: {}", e)))?;

    if pcm_data.is_empty() {
        diag::empty_result(SCOPE, "Input audio was empty; provider request was skipped");
        return Ok(AsrResult { text: String::new(), elapsed_ms: 0 });
    }

    let auth = DoubaoAuth::from_config(config);
    if let Some(missing) = auth.missing_field() {
        return Err(diag::fail_code(
            SCOPE,
            "credentials",
            "provider_bad_key",
            format!("Doubao ASR is missing {}; complete it in Settings", missing),
        ));
    }
    let uid = auth.uid().to_string();

    // 進門先記「送出去的是什麼」：音訊多長、取樣率、幾個熱詞、哪套鑑權。
    // 長音訊失敗而短音訊正常這類問題，第一步要確認的就是客戶端確實把整段音訊發了出去。
    let audio_sec = pcm_data.len() as f64 / (sample_rate.max(1) as f64 * 2.0);
    diag::log(
        SCOPE,
        "start",
        &format!(
            "pcm_bytes={} audio_sec={:.1} rate={} hotwords={} auth={}",
            pcm_data.len(),
            audio_sec,
            sample_rate,
            hotwords.len(),
            auth.mode_name()
        ),
    );

    let start = Instant::now();

    let (mut ws, resource_id) = connect(&auth, SCOPE).await?;

    // 1. 发送 full client request
    let mut request_params = serde_json::json!({
        "model_name": "bigmodel",
        "enable_itn": true,
        "enable_punc": true,
        "result_type": "full",
        "show_utterances": true
    });
    // 注入熱詞（如果有）：流式介面熱詞須放在 request.corpus.context
    if let Some(ctx) = doubao_protocol::build_hotword_context(hotwords) {
        request_params["corpus"] = serde_json::json!({ "context": ctx });
    }

    let client_request = serde_json::json!({
        "user": { "uid": uid },
        "audio": {
            "format": "pcm",
            "rate": sample_rate,
            "bits": 16,
            "channel": 1
        },
        "request": request_params
    });

    let request_frame = doubao_protocol::build_full_client_request(
        &serde_json::to_string(&client_request).unwrap(),
    );
    ws.send(tungstenite::Message::Binary(request_frame.into()))
        .await
        .map_err(|e| diag::fail(SCOPE, "send_request", format!("Failed to send request: {}", e)))?;

    // 等待服務端確認
    if let Some(msg) = ws.next().await {
        let msg =
            msg.map_err(|e| diag::fail(SCOPE, "recv_ack", format!("Failed to receive acknowledgement: {}", e)))?;
        if let tungstenite::Message::Binary(data) = msg {
            let resp = doubao_protocol::parse_server_response(&data)
                .map_err(|e| diag::fail(SCOPE, "parse_ack", e))?;
            if resp.is_error {
                // 這裡的 payload 是服務端的錯誤說明（額度/資源/鑑權），不是識別文本
                return Err(diag::fail(
                    SCOPE,
                    "server_error_on_ack",
                    format!(
                        "Server error: {}{}",
                        resp.payload,
                        doubao_auth::explain_payload(&resp.payload)
                    ),
                ));
            }
        }
    }

    // 2. 傳送音訊資料（直接發 PCM，不轉 WAV）
    // nostream 模式下服务端等最后一包才处理，一次性发完最快
    let audio_frame = doubao_protocol::build_audio_request(&pcm_data, true);
    ws.send(tungstenite::Message::Binary(audio_frame.into()))
        .await
        .map_err(|e| diag::fail(SCOPE, "send_audio", format!("Failed to send audio: {}", e)))?;

    // 3. 接收結果（bigmodel_async 雙向流式：每包輸入對應一包返回，取最終結果）
    let mut final_text = String::new();
    // 有沒有收到「最後一包」標記。這個標記是區分下面兩種情況的唯一依據：
    //   收到了 + 文本為空 = 服務端確實沒聽出內容（使用者可能真的沒說話）
    //   沒收到 + 文本為空 = 這次呼叫失敗了，服務端在給結果前就斷開
    // 以前兩種都返回 Ok(空文本)，於是失敗被顯示成「未檢測到有效聲音」，
    // 日誌裡一個字都沒有 —— 排查一次要來回問使用者好幾輪。
    let mut saw_last = false;
    let mut last_payload_desc = String::from("(no result packet received)");

    while let Some(msg) = ws.next().await {
        let msg = msg.map_err(|e| diag::fail(SCOPE, "recv", format!("Failed to receive result: {}", e)))?;
        match msg {
            tungstenite::Message::Binary(data) => {
                let resp = doubao_protocol::parse_server_response(&data)
                    .map_err(|e| diag::fail(SCOPE, "parse_result", e))?;
                if resp.is_error {
                    return Err(diag::fail(
                        SCOPE,
                        "server_error",
                        format!(
                            "Recognition error: {}{}",
                            resp.payload,
                            doubao_auth::explain_payload(&resp.payload)
                        ),
                    ));
                }

                last_payload_desc = diag::describe_json(&resp.payload);

                // 解析 JSON 結果，持續更新 final_text
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&resp.payload) {
                    if let Some(text) = json.get("result").and_then(|r| r.get("text")).and_then(|t| t.as_str()) {
                        if !text.is_empty() {
                            final_text = text.to_string();
                        }
                    }
                }

                if resp.is_last {
                    saw_last = true;
                    break;
                }
            }
            tungstenite::Message::Close(frame) => {
                let reason = frame
                    .map(|f| format!("code={} reason={}", f.code, diag::truncate(&f.reason, 200)))
                    .unwrap_or_else(|| "No details".to_string());
                if !saw_last {
                    let _ = ws.close(None).await;
                    // 服務端主動斷開且沒給最終結果 —— 額度耗盡、資源未開通、超限都長這樣。
                    // 必須報錯，不能假裝識別成功卻沒內容。
                    return Err(diag::fail(
                        SCOPE,
                        "closed_before_result",
                        format!("Doubao closed the connection before recognition completed ({})", reason),
                    ));
                }
                diag::log(SCOPE, "close", &reason);
                break;
            }
            _ => {}
        }
    }

    // 關閉連線
    let _ = ws.close(None).await;

    let elapsed_ms = start.elapsed().as_millis() as u64;

    // 流結束了卻始終沒見到最後一包，同樣是沒跑完的呼叫
    if !saw_last {
        return Err(diag::fail(
            SCOPE,
            "stream_ended_without_result",
            format!(
                "Doubao connection ended without a final result (received_chars={} elapsed={}ms)",
                final_text.chars().count(),
                elapsed_ms
            ),
        ));
    }

    if final_text.is_empty() {
        diag::empty_result(
            SCOPE,
            &format!(
                "Server returned a final result with an empty transcript resourceId={} audio_sec={:.1} elapsed={}ms {}",
                resource_id, audio_sec, elapsed_ms, last_payload_desc
            ),
        );
    } else {
        diag::ok(SCOPE, elapsed_ms, final_text.chars().count());
    }

    Ok(AsrResult {
        text: final_text,
        elapsed_ms,
    })
}

pub async fn test_connection(config: &AsrProviderConfig) -> TestResult {
    let auth = DoubaoAuth::from_config(config);
    if let Some(missing) = auth.missing_field() {
        return TestResult {
            ok: false,
            message: format!("{} has not been configured", missing),
            elapsed_ms: 0,
            detail: String::new(),
        };
    }

    let start = Instant::now();

    match connect(&auth, "doubao/nostream-test").await {
        Ok((mut ws, resource_id)) => {
            let _ = ws.close(None).await;
            let elapsed_ms = start.elapsed().as_millis() as u64;
            TestResult {
                ok: true,
                message: format!("Connection successful ({}ms)", elapsed_ms),
                elapsed_ms,
                // 資源 ID 決定計費方式（小時版/併發版），測通了順手告訴使用者命中的是哪個
                detail: format!("Resource: {}", resource_id),
            }
        }
        Err(e) => {
            let elapsed_ms = start.elapsed().as_millis() as u64;
            TestResult {
                ok: false,
                message: e,
                elapsed_ms,
                detail: String::new(),
            }
        }
    }
}
