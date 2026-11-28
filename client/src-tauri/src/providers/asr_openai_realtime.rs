// OpenAI 即時轉寫 — gpt-live-transcribe
//
// 走 Realtime API 的「轉寫會話」（`?intent=transcription`），不是語音對話那套：
// 只上傳音訊、只收文字，模型不說話。
//
// ⚠️ 與 asr_qwen_realtime.rs 長得像但**不是同一套事件**。千問那份是照 OpenAI 的
// 老形態抄的，欄位停在 `session.input_audio_format` / `sample_rate` 這一層；
// OpenAI 現在把這些收進了 `session.audio.input.*`，還多了 `keywords` / `delay`。
// 別拿千問那份的 payload 往這裡套。
//
// 協議（官方 Realtime transcription 指南）：
//   1. 連 wss://api.openai.com/v1/realtime?intent=transcription
//   2. 發 session.update，type=transcription，配模型 / 音訊格式 / 關掉自動斷句
//   3. input_audio_buffer.append 發 base64 PCM（音訊在 JSON 裡，不是二進位制幀）
//   4. input_audio_buffer.commit 結束這一輪
//   5. 收 conversation.item.input_audio_transcription.delta（增量）
//      與 .completed（該輪最終文本）
//
// ── 未經真實介面驗證的三處（沒有 OpenAI key，只能照文件實現）──
// 用 `dev-scripts/probe_openai_live_transcribe.py` 打一次真實介面即可逐條確認：
//   A. **取樣率 16000**。官方示例一律寫 24000，但格式欄位是 `{type, rate}` 這種
//      顯式宣告形態，按說 16k 應當被接受（我們整條鏈路都是 16k，見 services/audio.ts）。
//      若服務端拒絕，只能在這裡對 PCM 做 16k→24k 重取樣。
//   B. **就緒事件名**。這裡同時接受 `session.updated` 與 `transcription_session.updated`
//      （歷史上用過後者），任一到達即算就緒。
//   C. **keywords 欄位的位置**。文件把它放在 `audio.input.transcription.keywords`，
//      這也正好是我們熱詞功能最合適的落點 —— 比 Whisper 那種「拿帶標點的句子做示範」
//      可靠得多。若服務端不認，退化的表現是熱詞不生效，不影響轉寫本身。

use super::{diag, types::AsrProviderConfig};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite;
use tungstenite::client::IntoClientRequest;
use tungstenite::http::header::{AUTHORIZATION, USER_AGENT};
use tungstenite::http::HeaderValue;

const WS_URL: &str = "wss://api.openai.com/v1/realtime?intent=transcription";
const SCOPE: &str = "openai/live";

/// 流式預設模型。**別把它用在 `/audio/transcriptions`** —— 那個端點要 `gpt-transcribe`，
/// 兩者不通用（見 asr_groq.rs 的 endpoint_for，關掉即時字幕時走的是那條路）。
const DEFAULT_MODEL: &str = "gpt-live-transcribe";

/// 取樣率。整條鏈路都是 16k 單聲道 s16le（services/audio.ts 的 TARGET_SAMPLE_RATE）。
const SAMPLE_RATE: u32 = 16000;

/// 延遲檔位：minimal | low | medium | high | xhigh。
///
/// 取 `low` 而不是 `minimal`：這個值決定模型在吐字前先攢多少音訊，檔位越低越早出字、
/// 但錯字更多。我們的用法是「說完插入到游標」，即時字幕只是過程反饋，最終文本的
/// 準確率比首字延遲重要；`minimal` 換來的那點提前量不值得多出來的錯字。
const DELAY: &str = "low";

/// 熱詞條數上限。keywords 是提示不是強制，給太多反而會稀釋每一個的作用；
/// 文件要求每條單行、且不含 `<` `>` 與換行，違反會讓整個 session.update 被拒 ——
/// 那會讓識別**完全不可用**，所以這裡過濾掉而不是原樣上送。
///
/// ⚠️ 這是**客戶端自設**的，不是服務端限制（見 capabilities.rs 的 client_cap）。
const KEYWORD_LIMIT: usize = 100;

/// 給 `capabilities.rs` 的測試用，理由同 asr_qwen_audio_stream::hotword_limit_for_docs。
#[cfg(test)]
pub fn keyword_limit_for_docs() -> usize {
    KEYWORD_LIMIT
}

type WsStream = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;
type WsSink = SplitSink<WsStream, tungstenite::Message>;

// ─────────────────────────── 協議編解碼 ───────────────────────────

/// 本次要用的模型：前端選定的優先，否則預設。
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

/// 熱詞 → `keywords` 陣列。
///
/// 按文件要求剔掉帶 `<` `>` 或換行的詞條：留著會讓 session.update 整條被拒，
/// 代價是識別完全不能用，而丟掉一個熱詞只是它不生效。
fn build_keywords(hotwords: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    hotwords
        .iter()
        .map(|w| w.trim())
        .filter(|w| !w.is_empty())
        .filter(|w| !w.contains('<') && !w.contains('>'))
        .filter(|w| !w.contains('\n') && !w.contains('\r'))
        .filter(|w| seen.insert(w.to_lowercase()))
        .take(KEYWORD_LIMIT)
        .map(|w| w.to_string())
        .collect()
}

/// 首條 session.update。
///
/// `turn_detection: null` 是刻意的：我們自己知道使用者什麼時候鬆開熱鍵，由
/// `input_audio_buffer.commit` 明確收一輪。交給服務端 VAD 斷句會在說話停頓處
/// 提前收尾，把一段話切成幾輪。
fn session_update_payload(model: &str, hotwords: &[String]) -> serde_json::Value {
    let mut transcription = serde_json::json!({
        "model": model,
        "delay": DELAY,
    });
    let keywords = build_keywords(hotwords);
    if !keywords.is_empty() {
        diag::log(
            SCOPE,
            "hotwords",
            &format!("keywords={} of={}", keywords.len(), hotwords.len()),
        );
        transcription["keywords"] = serde_json::json!(keywords);
    }
    serde_json::json!({
        "type": "session.update",
        "session": {
            "type": "transcription",
            "audio": {
                "input": {
                    "format": { "type": "audio/pcm", "rate": SAMPLE_RATE },
                    "transcription": transcription,
                    "turn_detection": serde_json::Value::Null,
                }
            }
        }
    })
}

fn append_audio_payload(pcm_b64: &str) -> serde_json::Value {
    serde_json::json!({ "type": "input_audio_buffer.append", "audio": pcm_b64 })
}

fn commit_payload() -> serde_json::Value {
    serde_json::json!({ "type": "input_audio_buffer.commit" })
}

fn event_type(ev: &serde_json::Value) -> &str {
    ev.get("type").and_then(|t| t.as_str()).unwrap_or("")
}

fn error_message(ev: &serde_json::Value) -> String {
    ev.get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or("Unknown error")
        .to_string()
}

/// 就緒事件。兩個名字都接受，見檔案頭 B 條。
fn is_session_ready(event: &str) -> bool {
    event == "session.updated" || event == "transcription_session.updated"
}

// ─────────────────────────── 累積狀態 ───────────────────────────

/// 一次會話累積到的文本。
///
/// committed = 已 `.completed` 的輪次，partial = 當前輪的增量拼接。
/// 我們把 turn_detection 關了，正常只有一輪；但服務端仍可能分輪返回，
/// 所以按多輪處理 —— 只認一輪的話，多出來的會被丟掉。
#[derive(Default)]
struct Transcript {
    committed: String,
    partial: String,
    finished: bool,
    error: Option<String>,
    /// 收到過多少條 delta，只用於日誌
    delta_events: usize,
}

/// 一條事件對累積狀態做了什麼。
enum Applied {
    Ignored,
    Updated,
    Finished,
    Failed,
}

impl Transcript {
    fn display(&self) -> String {
        format!("{}{}", self.committed, self.partial)
    }

    fn apply(&mut self, ev: &serde_json::Value) -> Applied {
        match event_type(ev) {
            "conversation.item.input_audio_transcription.delta" => {
                let delta = ev.get("delta").and_then(|d| d.as_str()).unwrap_or("");
                if delta.is_empty() {
                    return Applied::Ignored;
                }
                // delta 是**增量**（與千問那份的 text 全量語義相反），要追加
                self.partial.push_str(delta);
                self.delta_events += 1;
                Applied::Updated
            }
            "conversation.item.input_audio_transcription.completed" => {
                let transcript = ev.get("transcript").and_then(|t| t.as_str()).unwrap_or("");
                // completed 給的是該輪**最終**文本，用它替掉本輪攢的 delta：
                // 模型會在後續 delta 裡修正前面的字，最終文本才是它認準的那版。
                self.partial.clear();
                if !transcript.is_empty() {
                    self.committed.push_str(transcript);
                }
                self.finished = true;
                Applied::Finished
            }
            "error" => {
                self.error = Some(error_message(ev));
                self.finished = true;
                Applied::Failed
            }
            _ => Applied::Ignored,
        }
    }
}

// ─────────────────────────── 建連 ───────────────────────────

/// 建連 → 發 session.update → 等就緒。拿到 WsStream 才算可以發音訊。
async fn open_session(
    config: &AsrProviderConfig,
    hotwords: &[String],
) -> Result<(WsStream, String), String> {
    if config.api_key.trim().is_empty() {
        return Err(diag::fail_code(
            SCOPE,
            "credentials",
            "provider_bad_key",
            "OpenAI transcription is missing the API Key; complete it in Settings".to_string(),
        ));
    }

    let model = resolve_model(config);
    let mut request = WS_URL.into_client_request().map_err(|e| {
        diag::fail(
            SCOPE,
            "build_request",
            format!("Failed to build request: {}", e),
        )
    })?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", config.api_key.trim())).map_err(|e| {
            diag::fail_code(
                SCOPE,
                "authorization_header",
                "provider_bad_key",
                format!("Invalid Authorization header: {}", e),
            )
        })?,
    );
    // Realtime 的 beta 頭。GA 之後帶著它也無害，缺了則老帳號連不上。
    request
        .headers_mut()
        .insert("openai-beta", HeaderValue::from_static("realtime=v1"));
    // 所有出網請求都帶 UA：自有 ALB 與第三方閘道器會按「無 UA」直接 403，
    // 且報錯裡絕不提 UA（見 pitfalls 第 1 條）。
    request.headers_mut().insert(
        USER_AGENT,
        HeaderValue::from_static(concat!("SayIt/", env!("CARGO_PKG_VERSION"))),
    );

    let (mut ws, response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| {
            diag::fail(
                SCOPE,
                "connect",
                format!("WebSocket connection failed: {}", e),
            )
        })?;
    diag::log(
        SCOPE,
        "connected",
        &format!(
            "status={} model={} rate={}",
            response.status(),
            model,
            SAMPLE_RATE
        ),
    );

    let payload = session_update_payload(&model, hotwords);
    ws.send(tungstenite::Message::Text(
        serde_json::to_string(&payload).unwrap().into(),
    ))
    .await
    .map_err(|e| {
        diag::fail(
            SCOPE,
            "send_session_update",
            format!("Failed to send session.update: {}", e),
        )
    })?;

    // 必須等到就緒事件：配置被拒（模型沒開通、取樣率不接受、keywords 違規）時
    // 服務端會回 error 或直接關連線，都要在這裡變成 Err，讓上層回落到一次性 HTTP，
    // 而不是抱著一條不工作的連線空轉。
    let ready = tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(msg) = ws.next().await {
            let msg = msg.map_err(|e| {
                diag::fail(
                    SCOPE,
                    "recv_ack",
                    format!("Failed to receive acknowledgement: {}", e),
                )
            })?;
            match msg {
                tungstenite::Message::Text(text) => {
                    let Ok(ev) = serde_json::from_str::<serde_json::Value>(&text) else {
                        continue;
                    };
                    let name = event_type(&ev);
                    if is_session_ready(name) {
                        return Ok(true);
                    }
                    if name == "error" {
                        return Err(diag::fail(
                            SCOPE,
                            "server_error",
                            format!("Server rejected the session: {}", error_message(&ev)),
                        ));
                    }
                }
                tungstenite::Message::Close(frame) => {
                    let reason = frame
                        .map(|f| f.reason.to_string())
                        .unwrap_or_else(|| "No reason".to_string());
                    return Err(diag::fail(
                        SCOPE,
                        "closed_before_ready",
                        format!("Server closed the session: {}", reason),
                    ));
                }
                _ => {}
            }
        }
        Ok(false)
    })
    .await
    .map_err(|_| {
        diag::fail(
            SCOPE,
            "session_ready_timeout",
            "Timed out waiting for the transcription session to start".to_string(),
        )
    })??;

    if !ready {
        return Err(diag::fail(
            SCOPE,
            "closed_before_ready",
            "WebSocket closed before the session became ready".to_string(),
        ));
    }
    diag::log(SCOPE, "session_ready", &format!("model={}", model));
    Ok((ws, model))
}

// ─────────────────────────── 流式會話 ───────────────────────────

static SINK: once_cell::sync::Lazy<Arc<Mutex<Option<WsSink>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(None)));
static READER: once_cell::sync::Lazy<Arc<Mutex<Option<JoinHandle<()>>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(None)));
static STATE: once_cell::sync::Lazy<Arc<Mutex<Transcript>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(Transcript::default())));
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// 開啟流式轉寫會話。
///
/// 和 asr_qwen_audio_stream 一樣**不分「即時 / 普通」兩條程式碼路徑**：無論要不要上拋
/// 中間結果，後台 reader 都必須一直讀 —— 服務端在發音訊期間持續推 delta，
/// 沒人讀會把連線堵死。realtime 只決定要不要 emit。
#[tauri::command]
pub async fn openai_live_open(
    app: AppHandle,
    config: AsrProviderConfig,
    hotwords: Option<Vec<String>>,
    realtime: Option<bool>,
) -> Result<(), String> {
    let realtime = realtime.unwrap_or(false);
    cleanup().await;

    let hotwords = hotwords.unwrap_or_default();
    let (ws, model) = open_session(&config, &hotwords).await?;

    let (sink, stream) = ws.split();
    *STATE.lock().await = Transcript::default();
    *SINK.lock().await = Some(sink);
    let handle = tokio::spawn(run_reader(stream, app, STATE.clone(), realtime));
    *READER.lock().await = Some(handle);
    ACTIVE.store(true, Ordering::SeqCst);
    diag::log(
        SCOPE,
        "stream_open",
        &format!("realtime={} model={}", realtime, model),
    );
    Ok(())
}

async fn run_reader(
    mut stream: SplitStream<WsStream>,
    app: AppHandle,
    state: Arc<Mutex<Transcript>>,
    realtime: bool,
) {
    let mut emitted = 0usize;
    let mut ended_cleanly = false;
    let mut close_reason = String::new();
    while let Some(msg) = stream.next().await {
        let text = match msg {
            Ok(tungstenite::Message::Text(t)) => t,
            Ok(tungstenite::Message::Close(frame)) => {
                close_reason = frame
                    .map(|f| f.reason.to_string())
                    .unwrap_or_else(|| "no reason".to_string());
                break;
            }
            Err(e) => {
                close_reason = e.to_string();
                break;
            }
            Ok(_) => continue,
        };
        let Ok(ev) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };

        let (applied, display) = {
            let mut s = state.lock().await;
            let applied = s.apply(&ev);
            (applied, s.display())
        };
        match applied {
            Applied::Updated => {
                if realtime {
                    emitted += 1;
                    if emitted == 1 {
                        diag::log(SCOPE, "first_partial", "");
                    }
                    let _ = app.emit(
                        "asr-partial",
                        serde_json::json!({ "text": display, "provider": "openai_live" }),
                    );
                }
            }
            Applied::Finished => {
                // 最終文本也要上屏一次：completed 會把 delta 裡的錯字修正掉，
                // 不 emit 的話懸浮窗會一直停在修正前那版
                if realtime {
                    let _ = app.emit(
                        "asr-partial",
                        serde_json::json!({ "text": display, "provider": "openai_live" }),
                    );
                }
                ended_cleanly = true;
                break;
            }
            Applied::Failed => {
                ended_cleanly = true;
                break;
            }
            Applied::Ignored => {}
        }
    }
    let mut s = state.lock().await;
    s.finished = true;
    // 協議沒跑完就斷了必須留痕。否則 finish 只能返回空串，而空串會被顯示成
    // 「未檢測到有效聲音」，把使用者引去查麥克風 —— 真實原因（額度、鑑權、
    // 模型沒開通）就此消失。見 pitfalls 第 15 條。
    if !ended_cleanly && s.error.is_none() {
        s.error = Some(format!(
            "connection closed before the transcript completed: {}",
            diag::truncate(&close_reason, 200)
        ));
    }
    diag::log(
        SCOPE,
        "reader_stopped",
        &format!(
            "emits={} deltas={} clean={}",
            emitted, s.delta_events, ended_cleanly
        ),
    );
}

async fn cleanup() {
    ACTIVE.store(false, Ordering::SeqCst);
    if let Some(mut sink) = SINK.lock().await.take() {
        let _ = sink.close().await;
    }
    if let Some(handle) = READER.lock().await.take() {
        handle.abort();
    }
    *STATE.lock().await = Transcript::default();
}

#[tauri::command]
pub async fn openai_live_send(pcm_b64: String) -> Result<(), String> {
    if !ACTIVE.load(Ordering::SeqCst) {
        return Err(diag::fail(
            SCOPE,
            "send_without_session",
            "Session is not open".to_string(),
        ));
    }
    // 音訊本來就是 base64 過來的，這個協議也要 base64 塞 JSON，所以不解碼 ——
    // 解一遍再編一遍純屬浪費（與千問 realtime 那份同理）。
    let payload = serde_json::to_string(&append_audio_payload(&pcm_b64)).unwrap();
    let mut sink = SINK.lock().await;
    let s = sink.as_mut().ok_or_else(|| {
        diag::fail(
            SCOPE,
            "send_without_session",
            "Session is not open".to_string(),
        )
    })?;
    s.send(tungstenite::Message::Text(payload.into()))
        .await
        .map_err(|e| diag::fail(SCOPE, "send_audio", format!("Failed to send audio: {}", e)))
}

#[tauri::command]
pub async fn openai_live_finish() -> Result<String, String> {
    if !ACTIVE.load(Ordering::SeqCst) {
        return Err(diag::fail(
            SCOPE,
            "finish_without_session",
            "Session is not open".to_string(),
        ));
    }
    {
        let mut sink = SINK.lock().await;
        let s = sink.as_mut().ok_or_else(|| {
            diag::fail(
                SCOPE,
                "finish_without_session",
                "Session is not open".to_string(),
            )
        })?;
        // 傳送失敗不致命：可能服務端已經收尾，繼續讀已累計的文本即可。
        if let Err(e) = s
            .send(tungstenite::Message::Text(
                serde_json::to_string(&commit_payload()).unwrap().into(),
            ))
            .await
        {
            diag::log(SCOPE, "commit_send_err_ignored", &e.to_string());
        }
    }

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        {
            let st = STATE.lock().await;
            if st.finished {
                let err = st.error.clone();
                let text = st.display().trim().to_string();
                let deltas = st.delta_events;
                drop(st);
                cleanup().await;
                // 已經識別出文字就交回去，哪怕收尾時斷了 —— 丟掉它等於讓使用者白說一遍。
                // 但斷連原因仍要留痕。
                if !text.is_empty() {
                    if let Some(err) = &err {
                        diag::log(SCOPE, "finished_with_error", err);
                    }
                    return Ok(text);
                }
                if let Some(err) = err {
                    return Err(diag::fail(
                        SCOPE,
                        "task_failed",
                        format!("Transcription failed: {}", err),
                    ));
                }
                diag::empty_result(
                    SCOPE,
                    &format!("Session finished with no transcript deltas={}", deltas),
                );
                return Ok(String::new());
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    // 超時也把已拿到的文本交回去：丟掉它等於讓使用者白說一遍。
    let text = STATE.lock().await.display().trim().to_string();
    diag::log(
        SCOPE,
        "finish_timeout",
        &format!("returning partial chars={}", text.chars().count()),
    );
    cleanup().await;
    Ok(text)
}

#[tauri::command]
pub async fn openai_live_close() -> Result<(), String> {
    cleanup().await;
    Ok(())
}

// ─────────────────────────── 連通性測試 ───────────────────────────

/// 只建連 + 開會話 + 立刻關：能開出轉寫會話就說明金鑰和模型都對得上。
///
/// 不發音訊 —— 這個命令只回答「能不能連」，真實轉寫由設定頁的識別測試負責，
/// 而那條走的是 HTTP 一次性路徑（見 registry.rs）。
pub async fn test_connection(config: &AsrProviderConfig) -> super::types::TestResult {
    let start = Instant::now();
    let model = resolve_model(config);
    let result = async {
        let (mut ws, _) = open_session(config, &[]).await?;
        let _ = ws.close(None).await;
        Ok::<(), String>(())
    }
    .await;

    let elapsed_ms = start.elapsed().as_millis() as u64;
    match result {
        Ok(()) => super::types::TestResult {
            ok: true,
            message: format!("Connection successful ({}ms)", elapsed_ms),
            elapsed_ms,
            detail: format!("model: {}", model),
        },
        Err(message) => super::types::TestResult {
            ok: false,
            message,
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
            provider: "openai_live_transcribe".to_string(),
            api_key: "sk-test".to_string(),
            app_id: String::new(),
            extra,
        }
    }

    #[test]
    fn model_falls_back_to_the_live_default() {
        assert_eq!(resolve_model(&config(serde_json::json!({}))), DEFAULT_MODEL);
        assert_eq!(
            resolve_model(&config(serde_json::json!({ "model": "   " }))),
            DEFAULT_MODEL
        );
        assert_eq!(
            resolve_model(&config(serde_json::json!({ "model": "gpt-transcribe" }))),
            "gpt-transcribe"
        );
    }

    /// session.update 的形狀是這份實現的全部前提，寫錯一個層級就是「連上了但沒有字」。
    /// 尤其 turn_detection 必須是 **JSON null**，不是預設、也不是字串 "null"。
    #[test]
    fn session_update_matches_the_documented_shape() {
        let payload = session_update_payload(DEFAULT_MODEL, &[]);
        assert_eq!(payload["type"], "session.update");
        assert_eq!(payload["session"]["type"], "transcription");
        let input = &payload["session"]["audio"]["input"];
        assert_eq!(input["format"]["type"], "audio/pcm");
        assert_eq!(input["format"]["rate"], SAMPLE_RATE);
        assert_eq!(input["transcription"]["model"], DEFAULT_MODEL);
        assert_eq!(input["transcription"]["delay"], DELAY);
        assert!(input["turn_detection"].is_null());
        // 沒有熱詞時不帶 keywords 欄位（空陣列也可能被判成"限定只認這些詞"）
        assert!(input["transcription"].get("keywords").is_none());
    }

    #[test]
    fn hotwords_become_keywords() {
        let words = vec!["SayIt".to_string(), " Kiro ".to_string()];
        let payload = session_update_payload(DEFAULT_MODEL, &words);
        let keywords = &payload["session"]["audio"]["input"]["transcription"]["keywords"];
        assert_eq!(keywords[0], "SayIt");
        assert_eq!(keywords[1], "Kiro");
    }

    /// 帶 `<` `>` 或換行的詞條會讓服務端拒掉整條 session.update —— 那是「識別完全不可用」，
    /// 比「這個熱詞不生效」嚴重得多，所以必須在這裡剔掉。
    #[test]
    fn illegal_keywords_are_dropped_not_forwarded() {
        let words = vec![
            "good".to_string(),
            "bad<tag>".to_string(),
            "two\nlines".to_string(),
            "carriage\rreturn".to_string(),
        ];
        assert_eq!(build_keywords(&words), vec!["good".to_string()]);
    }

    #[test]
    fn keywords_are_deduped_and_capped() {
        let words = vec!["SayIt".to_string(), "sayit".to_string()];
        assert_eq!(build_keywords(&words).len(), 1);
        let many: Vec<String> = (0..KEYWORD_LIMIT + 50).map(|i| format!("w{}", i)).collect();
        assert_eq!(build_keywords(&many).len(), KEYWORD_LIMIT);
    }

    /// delta 是**增量**（要追加），completed 給的是該輪最終文本（要替掉本輪的 delta）。
    /// 把 delta 當全量覆蓋會只剩最後一個字；把 completed 當增量追加會讓文本重複一遍。
    #[test]
    fn deltas_accumulate_and_completed_replaces_them() {
        let mut t = Transcript::default();
        t.apply(&serde_json::json!({
            "type": "conversation.item.input_audio_transcription.delta",
            "delta": "Hello,"
        }));
        t.apply(&serde_json::json!({
            "type": "conversation.item.input_audio_transcription.delta",
            "delta": " how are"
        }));
        assert_eq!(t.display(), "Hello, how are");
        assert_eq!(t.delta_events, 2);

        t.apply(&serde_json::json!({
            "type": "conversation.item.input_audio_transcription.completed",
            "transcript": "Hello, how are you?"
        }));
        assert_eq!(t.display(), "Hello, how are you?");
        assert!(t.finished);
    }

    #[test]
    fn empty_deltas_are_ignored() {
        let mut t = Transcript::default();
        assert!(matches!(
            t.apply(&serde_json::json!({
                "type": "conversation.item.input_audio_transcription.delta",
                "delta": ""
            })),
            Applied::Ignored
        ));
        assert_eq!(t.delta_events, 0);
        assert!(matches!(
            t.apply(&serde_json::json!({ "type": "input_audio_buffer.committed" })),
            Applied::Ignored
        ));
    }

    #[test]
    fn server_error_is_captured() {
        let mut t = Transcript::default();
        assert!(matches!(
            t.apply(&serde_json::json!({
                "type": "error",
                "error": { "message": "insufficient_quota" }
            })),
            Applied::Failed
        ));
        assert_eq!(t.error.as_deref(), Some("insufficient_quota"));
        assert!(t.finished);
    }

    /// 歷史上用過 transcription_session.updated，兩個名字都要認 —— 只認一個的症狀是
    /// 建連超時後靜默回落到一次性識別，字幕永遠不出來。
    #[test]
    fn both_ready_event_names_are_accepted() {
        assert!(is_session_ready("session.updated"));
        assert!(is_session_ready("transcription_session.updated"));
        assert!(!is_session_ready("session.created"));
    }
}
