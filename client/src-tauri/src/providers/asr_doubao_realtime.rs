// 豆包流式語音識別 2.0 — 邊錄邊發的會話管理
// start 時建連，send 即時傳送音訊，finish 發最後一包等結果
//
// 兩種模式：
//  - 普通模式（realtime=false）：bigmodel_nostream 端點，準確率更高（預設）。
//  - 即時顯示模式（realtime=true）：bigmodel(_async) 雙向流式端點，邊說邊返回中間結果，
//    透過 `asr-partial` 事件上拋給前端做懸浮窗即時上屏。
//
// **兩種模式都必須邊發邊讀**（官方文件：兩個端點都是「每輸入一個包返回一個包」）。
// 以前普通模式錄音期間一幀都不讀，2026-10-27 實測 120 秒錄音服務端回了 605 幀、4.76 MB，
// 全堆在接收緩衝裡；使用者網路上某個環節緩衝一滿就被流控卡死，服務端 5 秒收不到包就結束會話
// （45000081 等包超時），客戶端要到松鍵才發現（診斷包 61c93385，>90 秒錄音 4/10 失敗）。
// 所以普通模式也拆成寫半邊 + 後台 reader，只是不 emit。見 .kiro/decisions.md「豆包長錄音失敗」。

use super::diag;
use super::doubao_auth::{self, DoubaoAuth};
use super::doubao_protocol;
use super::types::AsrProviderConfig;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite;

// 流式輸入模式（非即時，準確率更高）
const WS_URL_NOSTREAM: &str = "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_nostream";
// 雙向流式模式（即時，邊說邊出字）
const WS_URL_STREAM: &str = "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel";
// 雙向流式最佳化版（官方更推薦，時延更優，只在結果變化時返回新包）
const WS_URL_STREAM_ASYNC: &str = "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async";
// 豆包 Seed-ASR 2.0 資源 ID：小時版(包時) / 併發版(按量)。
// 雙向流式端點對資源授權更嚴格，不同賬號可能只開通了其中之一，即時連線時依次嘗試。
const RESOURCE_ID_DURATION: &str = "volc.seedasr.sauc.duration";
const RESOURCE_ID_CONCURRENT: &str = "volc.seedasr.sauc.concurrent";

/// 發完最後一包後等最終結果的上限。nostream 實測松鍵後 0.6~1.3 秒出全文。
const FINISH_TIMEOUT: Duration = Duration::from_secs(15);

type WsStream = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;
type WsSink = SplitSink<WsStream, tungstenite::Message>;

/// 會話的傳送端（寫半邊）。讀半邊交給後台 reader。
static SINK: once_cell::sync::Lazy<Arc<Mutex<Option<WsSink>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(None)));

/// 後台 reader 任務控制代碼（用於中止）。
static READER: once_cell::sync::Lazy<Arc<Mutex<Option<JoinHandle<()>>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(None)));

/// 會話共享狀態：累計文本 / 是否結束 / 錯誤。
#[derive(Default)]
struct SessionState {
    text: String,
    finished: bool,
    error: Option<String>,
}

static STATE: once_cell::sync::Lazy<Arc<Mutex<SessionState>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(SessionState::default())));

/// 當前是否有會話。
static ACTIVE: AtomicBool = AtomicBool::new(false);

// ── 傳送節奏記賬：排查「服務端等包超時」時要知道是不是我們自己停發了 ──
static OPENED_AT_MS: AtomicU64 = AtomicU64::new(0);
static LAST_SEND_MS: AtomicU64 = AtomicU64::new(0);
static MAX_SEND_GAP_MS: AtomicU64 = AtomicU64::new(0);
static SENT_PACKETS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn reset_send_stats() {
    let now = now_ms();
    OPENED_AT_MS.store(now, Ordering::SeqCst);
    LAST_SEND_MS.store(now, Ordering::SeqCst);
    MAX_SEND_GAP_MS.store(0, Ordering::SeqCst);
    SENT_PACKETS.store(0, Ordering::SeqCst);
}

fn record_send() {
    let now = now_ms();
    let last = LAST_SEND_MS.swap(now, Ordering::SeqCst);
    MAX_SEND_GAP_MS.fetch_max(now.saturating_sub(last), Ordering::SeqCst);
    SENT_PACKETS.fetch_add(1, Ordering::SeqCst);
}

fn send_stats() -> String {
    let opened = OPENED_AT_MS.load(Ordering::SeqCst);
    format!(
        "packets={} max_send_gap_ms={} since_last_send_ms={} session_ms={}",
        SENT_PACKETS.load(Ordering::SeqCst),
        MAX_SEND_GAP_MS.load(Ordering::SeqCst),
        now_ms().saturating_sub(LAST_SEND_MS.load(Ordering::SeqCst)),
        now_ms().saturating_sub(opened)
    )
}

fn build_client_request_json(
    uid: &str,
    sample_rate: u32,
    hotwords: &Option<Vec<String>>,
) -> String {
    let mut request_params = serde_json::json!({
        "model_name": "bigmodel",
        "enable_itn": true,
        "enable_punc": true,
        "result_type": "full",
        "show_utterances": true
    });
    if let Some(words) = hotwords {
        if let Some(ctx) = doubao_protocol::build_hotword_context(words) {
            // 熱詞須放在 request.corpus.context（非 request.context），否則流式介面不生效。
            // 注意：雙向流式模式 context 上限約 100 tokens，超出會被服務端截斷。
            request_params["corpus"] = serde_json::json!({ "context": ctx });
        }
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
    serde_json::to_string(&client_request).unwrap()
}

fn build_handshake_request(
    url: &str,
    auth: &DoubaoAuth<'_>,
    resource_id: &str,
) -> Result<tungstenite::http::Request<()>, String> {
    let mut builder = tungstenite::http::Request::builder()
        .uri(url)
        .header("Host", "openspeech.bytedance.com")
        .header("X-Api-Resource-Id", resource_id)
        // 雙向流式端點(bigmodel)要求帶 X-Api-Request-Id，否則閘道器直接 400（nostream 不帶也能過）
        .header("X-Api-Request-Id", uuid::Uuid::new_v4().to_string())
        .header("X-Api-Connect-Id", uuid::Uuid::new_v4().to_string())
        .header(
            "Sec-WebSocket-Key",
            tungstenite::handshake::client::generate_key(),
        )
        .header("Sec-WebSocket-Version", "13")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket");
    // 鑑權頭按控制台代次不同：新版只有 X-Api-Key，舊版是 App Key + Access Key
    for (name, value) in auth.headers() {
        builder = builder.header(name, value);
    }
    builder
        .body(())
        .map_err(|e| format!("Failed to build request: {}", e))
}

/// 把 WebSocket 握手錯誤展開成可讀字串；HTTP 錯誤會帶上狀態碼和響應體，方便定位 400 的真實原因。
fn describe_ws_error(err: &tungstenite::Error) -> String {
    match err {
        tungstenite::Error::Http(resp) => {
            let status = resp.status();
            let body = resp
                .body()
                .as_ref()
                .map(|b| String::from_utf8_lossy(b).trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "<empty body>".to_string());
            format!("HTTP {} body={}", status, body)
        }
        other => other.to_string(),
    }
}

/// 開啟豆包流式 ASR 會話：建立 WebSocket 連線併傳送 client request
///
/// hotwords：熱詞列表，會透過 request.corpus.context 欄位直傳給服務端（流式輸入模式最多 5000 詞）
/// realtime：是否開啟即時顯示（雙向流式 + 中間結果上拋）
#[tauri::command]
pub async fn doubao_stream_open(
    app: AppHandle,
    config: AsrProviderConfig,
    sample_rate: u32,
    hotwords: Option<Vec<String>>,
    realtime: Option<bool>,
) -> Result<(), String> {
    let realtime = realtime.unwrap_or(false);

    // 關閉遺留會話
    cleanup_session().await;

    let auth = DoubaoAuth::from_config(&config);
    if let Some(missing) = auth.missing_field() {
        return Err(diag::fail_code(
            "doubao/realtime",
            "credentials",
            "provider_bad_key",
            format!("Doubao ASR is missing {}; complete it in Settings", missing),
        ));
    }
    let uid = auth.uid().to_string();

    // 即時：按順序嘗試多個「端點 × 資源」組合，優先推薦的 async 端點 + 使用者手裡的 duration 資源。
    // 非即時：沿用一直可用的 nostream，同樣在小時版/併發版之間兜底。
    let candidates: &[(&str, &str)] = if realtime {
        &[
            (WS_URL_STREAM_ASYNC, RESOURCE_ID_DURATION),
            (WS_URL_STREAM, RESOURCE_ID_DURATION),
            (WS_URL_STREAM_ASYNC, RESOURCE_ID_CONCURRENT),
            (WS_URL_STREAM, RESOURCE_ID_CONCURRENT),
        ]
    } else {
        &[
            (WS_URL_NOSTREAM, RESOURCE_ID_DURATION),
            (WS_URL_NOSTREAM, RESOURCE_ID_CONCURRENT),
        ]
    };

    let mut connected: Option<WsStream> = None;
    let mut chosen_url = WS_URL_NOSTREAM;
    let mut last_err = String::from("Unknown error");
    for (url, resource_id) in candidates {
        let request = build_handshake_request(url, &auth, resource_id)
            .map_err(|e| diag::fail("doubao/realtime", "build_request", e))?;
        match tokio_tungstenite::connect_async(request).await {
            Ok((sock, response)) => {
                doubao_auth::log_ws_logid(
                    &format!(
                        "stream realtime={} url={} resourceId={} auth={}",
                        realtime,
                        url,
                        resource_id,
                        auth.mode_name()
                    ),
                    &response,
                );
                connected = Some(sock);
                chosen_url = url;
                break;
            }
            Err(e) => {
                last_err = describe_ws_error(&e);
                crate::commands::system::write_log_line(&format!(
                    "[RUST] [doubao_stream] connect FAILED realtime={} url={} resourceId={} err={}",
                    realtime, url, resource_id, last_err
                ));
            }
        }
    }
    let mut ws = connected.ok_or_else(|| diag::fail("doubao/realtime", "connect", format!("WebSocket connection failed: {}", last_err)))?;

    // 傳送 full client request
    let client_request = build_client_request_json(&uid, sample_rate, &hotwords);
    let frame = doubao_protocol::build_full_client_request(&client_request);
    ws.send(tungstenite::Message::Binary(frame.into()))
        .await
        .map_err(|e| diag::fail("doubao/realtime", "send_request", format!("Failed to send request: {}", e)))?;

    // 等待服務端確認（第一包 server response）
    if let Some(msg) = ws.next().await {
        let msg = msg.map_err(|e| diag::fail("doubao/realtime", "recv_ack", format!("Failed to receive acknowledgement: {}", e)))?;
        if let tungstenite::Message::Binary(data) = msg {
            let resp = doubao_protocol::parse_server_response(&data)
                .map_err(|e| diag::fail("doubao/realtime", "parse_ack", e))?;
            if resp.is_error {
                return Err(diag::fail("doubao/realtime", "server_error_on_ack", format!("Server error: {}", resp.payload)));
            }
        }
    }

    // 拆成讀寫兩半：寫半邊留給 send/finish，讀半邊交給後台 reader 一直讀。
    let (sink, stream) = ws.split();
    *STATE.lock().await = SessionState::default();
    *SINK.lock().await = Some(sink);
    reset_send_stats();
    let handle = tokio::spawn(run_reader(stream, app.clone(), STATE.clone(), realtime));
    *READER.lock().await = Some(handle);
    ACTIVE.store(true, Ordering::SeqCst);
    crate::commands::system::write_log_line(&format!(
        "[RUST] [doubao_stream] session open realtime={} endpoint={} reader spawned",
        realtime, chosen_url
    ));

    Ok(())
}

/// 後台 reader：持續讀取服務端回包，更新累計文本；即時模式下上拋中間結果。
///
/// 連線在收到最後一包結果**之前**斷開，一律記成錯誤（以前記成正常結束，finish 會帶著
/// 半截文本甚至空文本返回成功）。前端拿到錯誤後會用手裡的完整音訊再識別一次。
async fn run_reader(
    mut stream: SplitStream<WsStream>,
    app: AppHandle,
    state: Arc<Mutex<SessionState>>,
    emit_partials: bool,
) {
    let mut frame_count = 0usize;
    let mut emit_count = 0usize;
    let mut ended_cleanly = false;
    let mut abnormal: Option<String> = None;
    while let Some(msg) = stream.next().await {
        match msg {
            Ok(tungstenite::Message::Binary(data)) => {
                frame_count += 1;
                let resp = match doubao_protocol::parse_server_response(&data) {
                    Ok(r) => r,
                    Err(_) => continue, // 忽略無法解析的幀，等下一包
                };

                if resp.is_error {
                    // 「last packet has been received already」屬於良性收尾競爭：
                    // 服務端已經因為收到過負包/VAD 自動收尾而結束，此時應當用已累計文本正常收尾，
                    // 而不是把整條識別判為失敗。
                    if resp.payload.contains("last packet has been received")
                        || resp.payload.contains("last package has been received")
                    {
                        crate::commands::system::write_log_line(
                            "[RUST] [doubao_stream] benign end: last packet already received",
                        );
                        ended_cleanly = true;
                    } else {
                        abnormal = Some(format!("server error: {}", resp.payload));
                    }
                    break;
                }

                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&resp.payload) {
                    if let Some(text) = json
                        .get("result")
                        .and_then(|r| r.get("text"))
                        .and_then(|t| t.as_str())
                    {
                        if !text.is_empty() {
                            state.lock().await.text = text.to_string();
                            if emit_partials {
                                emit_count += 1;
                                if emit_count == 1 {
                                    crate::commands::system::write_log_line(
                                        "[RUST] [doubao_stream] emitted first asr-partial",
                                    );
                                }
                                let _ = app.emit(
                                    "asr-partial",
                                    serde_json::json!({ "text": text, "provider": "doubao" }),
                                );
                            }
                        }
                    }
                }

                if resp.is_last {
                    ended_cleanly = true;
                    break;
                }
            }
            Ok(tungstenite::Message::Close(frame)) => {
                let reason = frame
                    .map(|f| format!("code={} reason={}", u16::from(f.code), f.reason))
                    .unwrap_or_else(|| "no close frame".to_string());
                abnormal = Some(format!("server closed the connection before the final result ({})", reason));
                break;
            }
            Err(e) => {
                abnormal = Some(format!("connection dropped before the final result: {}", e));
                break;
            }
            Ok(_) => {}
        }
    }
    if !ended_cleanly && abnormal.is_none() {
        abnormal = Some("connection ended before the final result".to_string());
    }
    // 這一行是「誰先斷的、斷在什麼時候」的唯一證據：session_ms 是建連到此刻的時長，
    // since_last_send_ms 大說明是我們自己停發了，小說明是服務端 / 網路掐的。
    crate::commands::system::write_log_line(&format!(
        "[RUST] [doubao_stream] reader stopped clean={} frames={} emits={} {}{}",
        ended_cleanly,
        frame_count,
        emit_count,
        send_stats(),
        abnormal.as_deref().map(|a| format!(" err={}", a)).unwrap_or_default()
    ));
    let mut s = state.lock().await;
    if s.error.is_none() {
        s.error = abnormal;
    }
    s.finished = true;
}

/// 清理會話資源（關閉傳送端、中止 reader、重置狀態）。
async fn cleanup_session() {
    ACTIVE.store(false, Ordering::SeqCst);
    if let Some(mut sink) = SINK.lock().await.take() {
        let _ = sink.close().await;
    }
    if let Some(handle) = READER.lock().await.take() {
        handle.abort();
    }
    *STATE.lock().await = SessionState::default();
}

/// 傳送一個音訊包（非最後一包）
#[tauri::command]
pub async fn doubao_stream_send(pcm_b64: String) -> Result<(), String> {
    let pcm = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &pcm_b64)
        .map_err(|e| diag::fail("doubao/realtime", "decode_b64", format!("Failed to decode base64 audio: {}", e)))?;

    let frame = doubao_protocol::build_audio_request(&pcm, false);

    let mut sink = SINK.lock().await;
    let s = sink.as_mut().ok_or_else(|| diag::fail("doubao/realtime", "send_without_session", "Session is not open".to_string()))?;
    s.send(tungstenite::Message::Binary(frame.into()))
        .await
        .map_err(|e| diag::fail("doubao/realtime", "send_audio", format!("Failed to send audio: {}", e)))?;
    record_send();
    Ok(())
}

/// 傳送最後一包並等待最終識別結果
///
/// 失敗（服務端報錯、連線提前斷開、超時）一律返回 Err，**不返回半截文本**：前端手裡有完整
/// 錄音，會改走一次性上傳重新識別。以前超時會把已累計的文本當成功結果返回，使用者拿到的是
/// 被截掉後半段的文字，而且不會有任何提示。
#[tauri::command]
pub async fn doubao_stream_finish() -> Result<String, String> {
    if !ACTIVE.load(Ordering::SeqCst) {
        return Err(diag::fail("doubao/realtime", "finish_without_session", "Session is not open".to_string()));
    }
    crate::commands::system::write_log_line(&format!(
        "[RUST] [doubao_stream] finish start {}",
        send_stats()
    ));

    // 若 reader 已經自然收尾（VAD/服務端已結束），就不要再發負包，避免重複末包報錯。
    let already_finished = { STATE.lock().await.finished };
    if !already_finished {
        let mut sink = SINK.lock().await;
        if let Some(s) = sink.as_mut() {
            let frame = doubao_protocol::build_audio_request(&[], true);
            // 傳送失敗不在這裡判死：連線若已斷開，reader 會記下原因，下面統一返回。
            if let Err(e) = s.send(tungstenite::Message::Binary(frame.into())).await {
                crate::commands::system::write_log_line(&format!(
                    "[RUST] [doubao_stream] finish send last-packet err={}",
                    e
                ));
            }
        }
    }

    let deadline = Instant::now() + FINISH_TIMEOUT;
    loop {
        {
            let st = STATE.lock().await;
            if st.finished {
                let result = match st.error.clone() {
                    Some(err) => Err(err),
                    None => Ok(st.text.clone()),
                };
                drop(st);
                cleanup_session().await;
                return result.map_err(|err| {
                    diag::fail("doubao/realtime", "recognition_error", format!("Recognition error: {}", err))
                });
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    cleanup_session().await;
    Err(diag::fail(
        "doubao/realtime",
        "finish_timeout",
        format!("Timed out waiting for the final result after {}s", FINISH_TIMEOUT.as_secs()),
    ))
}

/// 關閉會話（異常中斷時呼叫）
#[tauri::command]
pub async fn doubao_stream_close() -> Result<(), String> {
    cleanup_session().await;
    Ok(())
}
