// Google Gemini 即時轉寫 — gemini-3.5-transcribe-live
//
// 走 Gemini Live API 的 WebSocket（BidiGenerateContent）。Live API 本體是「語音對話」，
// 但這個模型是**轉寫專用**的：只出文字、不說話。
//
// 協議：
//   1. 連 wss://…/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent?key=…
//   2. 首條訊息必須是 `setup`（模型 + 只要文本 + 開啟輸入轉寫）
//   3. 等 `setupComplete`
//   4. `realtimeInput.audio` 送 base64 PCM，mimeType 裡帶取樣率
//   5. 讀 `serverContent.inputTranscription.text`（增量）
//   6. 收尾發 `realtimeInput.audioStreamEnd`
//
// ⚠️ 與 asr_openai_realtime.rs 完全不是一套事件名，別互相套用。
//
// ── MEASURED 2026-09-16（`dev-scripts/probe_new_asr_providers.py --target gemini-live`，
//    打的是真實介面，AI Studio 專案**沒有繫結結算資訊**，免費層可用）──
// 實測事件序列（3.05s 中文音訊）：
//   {"setupComplete": {}}
//   {"serverContent": {}, "voiceActivity": {"type":"ACTIVITY_START","audioOffset":"0.840s"}}
//   {"serverContent": {"interimInputTranscription": {"text": "語音輸入"}}}
//   {"serverContent": {"interimInputTranscription": {"text": "語音 輸入法"}}}
//   {"serverContent": {"interimInputTranscription": {"text": "語音輸入法測試成"}}}
//   {"serverContent": {"interimInputTranscription": {"text": "語音輸入法測試成 功。"}}}
//   {"serverContent": {"inputTranscription": {"text": "語音輸入法測試成功。"}}}
//   {"serverContent": {"generationComplete": true}}
//
// 由此定下來的四件事（前兩條是照文件寫會踩的坑）：
//   A. **即時字幕的文本在 `interimInputTranscription`**，`inputTranscription` 是
//      一輪結束時的定稿、整段只來一條。只讀後者的話「即時字幕」這個功能等於不存在 ——
//      不報錯，字幕就是永遠不出現。
//   B. **兩個欄位的 text 都是全量**，不是增量。interim 要覆蓋、定稿按輪次追加。
//      當成增量追加會得到「語音輸入語音 輸入法語音輸入法測試成…」這種疊加串。
//      注意中間態帶臨時空格（"語音 輸入法"），定稿裡沒有 —— 所以 interim 只能上屏，
//      最終結果必須用定稿那條。
//   C. `realtimeInput.audio` 與 `realtimeInput.mediaChunks` **兩種形狀都能用**，
//      轉寫結果完全一致。保留前者（當前文件形態）。
//   D. 結束訊號是 **`generationComplete`**（不是 turnComplete）。`setupComplete`
//      用的是 camelCase。`voiceActivity` 是 VAD 事件，忽略即可。
//
// ── 金鑰為什麼放在 URL 的 query 裡 ──
// 不情願，但 Live API 只接受 `?key=` 或短期 ephemeral token；它沒有
// Authorization 頭的路子（asr_gemini.rs 那條 HTTP 路徑用的是 x-goog-api-key 頭）。
// 所以**這個 URL 絕不能進日誌**：下面 diag::log 裡只記 host 與 model，不記完整 URL。

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
use tungstenite::http::header::USER_AGENT;
use tungstenite::http::HeaderValue;

const WS_HOST: &str = "generativelanguage.googleapis.com";
const WS_PATH: &str =
    "/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";
const SCOPE: &str = "gemini/live";
const DEFAULT_MODEL: &str = "gemini-3.5-transcribe-live";
const SAMPLE_RATE: u32 = 16000;

type WsStream = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;
type WsSink = SplitSink<WsStream, tungstenite::Message>;

// ─────────────────────────── 協議編解碼 ───────────────────────────

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

/// setup 裡的模型名要帶 `models/` 字首（Live API 用的是資源名，不是裸模型 ID）。
/// 使用者填了帶字首的也不重複加。
fn qualified_model(model: &str) -> String {
    if model.starts_with("models/") {
        model.to_string()
    } else {
        format!("models/{}", model)
    }
}

fn ws_url(api_key: &str) -> String {
    format!(
        "wss://{}{}?key={}",
        WS_HOST,
        WS_PATH,
        urlencoding_minimal(api_key)
    )
}

/// 只轉義 API key 裡可能出現的、會破壞 query 的字元。
///
/// 不引入一個 URL 編碼依賴：Google 的 key 是 `[A-Za-z0-9_-]` 這一類，本來無需轉義。
/// 這裡只兜住「使用者粘進來時帶了空格或換行」的情況 —— 那種情況下不轉義會讓
/// 建連報一個和金鑰毫無關係的 URL 解析錯誤，很難往回定位。
fn urlencoding_minimal(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
                vec![c]
            } else {
                format!("%{:02X}", c as u32).chars().collect()
            }
        })
        .collect()
}

/// 首條 setup 訊息。
///
/// `responseModalities: ["TEXT"]` 是必要的：Live API 預設會**合成語音回話**，
/// 那會白燒 token、也會讓 serverContent 裡塞滿我們不要的音訊資料。
fn setup_payload(model: &str) -> serde_json::Value {
    serde_json::json!({
        "setup": {
            "model": qualified_model(model),
            "generationConfig": { "responseModalities": ["TEXT"] },
            // 開啟「把輸入音訊轉成文字」。轉寫專用模型可能預設就開著，
            // 顯式給上不會有壞處（見檔案頭 C 條）。
            "inputAudioTranscription": {}
        }
    })
}

/// 音訊訊息。
///
/// mimeType 裡必須帶取樣率（`audio/pcm;rate=16000`）—— 這是 Live API 宣告取樣率的
/// 唯一途徑，沒有單獨的 sampleRate 欄位。寫錯的後果是識別出一堆亂碼而不是報錯。
///
/// 若實測發現服務端只認舊形態，把 `"audio": {...}` 換成
/// `"mediaChunks": [{...}]`（陣列），其餘不變。
fn audio_payload(pcm_b64: &str) -> serde_json::Value {
    serde_json::json!({
        "realtimeInput": {
            "audio": {
                "mimeType": format!("audio/pcm;rate={}", SAMPLE_RATE),
                "data": pcm_b64
            }
        }
    })
}

fn audio_end_payload() -> serde_json::Value {
    serde_json::json!({ "realtimeInput": { "audioStreamEnd": true } })
}

/// **進行中**的轉寫（`interimInputTranscription`）。這條才是即時字幕的來源。
///
/// 實測：說話期間每隔幾百毫秒來一條，text 是**當前的全量文本**（不是增量），
/// 所以要覆蓋而不是追加。中間態還會帶臨時空格（"語音 輸入法"），最終定稿裡沒有 ——
/// 所以它只適合上屏，不能拿來當最終結果。
fn interim_transcription(ev: &serde_json::Value) -> Option<&str> {
    ev.pointer("/serverContent/interimInputTranscription/text")
        .or_else(|| ev.pointer("/serverContent/interim_input_transcription/text"))
        .and_then(|t| t.as_str())
}

/// **定稿**的轉寫（`inputTranscription`）。一輪結束時來一條，text 也是全量。
///
/// 同時認 camelCase 與 snake_case：Live API 的 JSON 是 camelCase，
/// 但 proto 名是 snake_case，兩種寫法在不同閘道器/版本里都出現過。
fn final_transcription(ev: &serde_json::Value) -> Option<&str> {
    ev.pointer("/serverContent/inputTranscription/text")
        .or_else(|| ev.pointer("/serverContent/input_transcription/text"))
        .and_then(|t| t.as_str())
}

fn is_setup_complete(ev: &serde_json::Value) -> bool {
    ev.get("setupComplete").is_some() || ev.get("setup_complete").is_some()
}

/// 這一輪結束了。兩個訊號都認：只認一個的話，缺的那種情況會一直等到超時。
fn is_turn_done(ev: &serde_json::Value) -> bool {
    let truthy = |v: Option<&serde_json::Value>| v.map(|x| x.as_bool() != Some(false)).unwrap_or(false);
    truthy(ev.pointer("/serverContent/turnComplete"))
        || truthy(ev.pointer("/serverContent/turn_complete"))
        || truthy(ev.pointer("/serverContent/generationComplete"))
        || truthy(ev.pointer("/serverContent/generation_complete"))
}

/// 服務端錯誤。Live API 把它放在頂層 `error`，也可能是 gRPC 風格的 `{code,message}`。
fn error_message(ev: &serde_json::Value) -> Option<String> {
    let err = ev.get("error")?;
    let msg = err
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("Unknown error");
    let code = err
        .get("code")
        .map(|c| format!(" code={}", c))
        .unwrap_or_default();
    Some(format!("{}{}", msg, code))
}

// ─────────────────────────── 累積狀態 ───────────────────────────

/// 一次會話累積到的文本。
///
/// committed = 已定稿的輪次（inputTranscription），partial = 進行中的那一輪
/// （interimInputTranscription）。兩個欄位的 text 都是**全量**，所以 partial 是
/// 覆蓋、committed 是按輪次追加。
///
/// 一次錄音通常只有一輪，但服務端會按語音活動自己分輪（實測有 voiceActivity 事件），
/// 所以按多輪處理 —— 只認一輪的話，說話中間停頓一下就會丟掉後半段。
#[derive(Default)]
struct Transcript {
    committed: String,
    partial: String,
    finished: bool,
    error: Option<String>,
    interim_events: usize,
    final_events: usize,
}

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
        if let Some(err) = error_message(ev) {
            self.error = Some(err);
            self.finished = true;
            return Applied::Failed;
        }
        // 定稿先判：它和 interim 不會同時出現，但定稿更要緊，不該被漏掉
        if let Some(text) = final_transcription(ev) {
            self.partial.clear();
            if text.is_empty() {
                return Applied::Ignored;
            }
            self.committed.push_str(text);
            self.final_events += 1;
            // 定稿也要上屏一次：它會把中間態裡的臨時空格修掉，
            // 不 emit 的話懸浮窗會停在"語音輸入法測試成 功。"那版
            return Applied::Updated;
        }
        if let Some(text) = interim_transcription(ev) {
            if text.is_empty() {
                return Applied::Ignored;
            }
            // **全量覆蓋**，不是追加。實測每條 interim 都帶著前面所有字
            // （"語音輸入" → "語音 輸入法" → "語音輸入法測試成"），
            // 追加會得到一串反覆疊加的亂碼。
            self.partial.clear();
            self.partial.push_str(text);
            self.interim_events += 1;
            return Applied::Updated;
        }
        if is_turn_done(ev) {
            self.finished = true;
            return Applied::Finished;
        }
        Applied::Ignored
    }
}

// ─────────────────────────── 建連 ───────────────────────────

async fn open_session(config: &AsrProviderConfig) -> Result<(WsStream, String), String> {
    if config.api_key.trim().is_empty() {
        return Err(diag::fail_code(
            SCOPE,
            "credentials",
            "provider_bad_key",
            "Gemini live transcription is missing the API Key; complete it in Settings".to_string(),
        ));
    }

    let model = resolve_model(config);
    let url = ws_url(&config.api_key);
    let mut request = url.as_str().into_client_request().map_err(|e| {
        // 刻意不把 e 裡可能帶的完整 URL 拼進訊息：URL 裡有金鑰。
        let _ = e;
        diag::fail(
            SCOPE,
            "build_request",
            "Failed to build the request; check the API Key for stray characters".to_string(),
        )
    })?;
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
    // 只記 host 與 model —— URL 裡帶金鑰，絕不整條落盤。
    diag::log(
        SCOPE,
        "connected",
        &format!(
            "status={} host={} model={} rate={}",
            response.status(),
            WS_HOST,
            model,
            SAMPLE_RATE
        ),
    );

    ws.send(tungstenite::Message::Text(
        serde_json::to_string(&setup_payload(&model))
            .unwrap()
            .into(),
    ))
    .await
    .map_err(|e| diag::fail(SCOPE, "send_setup", format!("Failed to send setup: {}", e)))?;

    // 必須等到 setupComplete：金鑰無效、模型沒開通、setup 欄位不認時服務端會回
    // error 或直接關連線，都要在這裡變成 Err，讓上層回落到一次性 HTTP 路徑。
    let ready = tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(msg) = ws.next().await {
            let msg = msg.map_err(|e| {
                diag::fail(
                    SCOPE,
                    "recv_ack",
                    format!("Failed to receive acknowledgement: {}", e),
                )
            })?;
            // Live API 的服務端訊息可能是文本也可能是二進位制裡的 JSON，兩種都試著解
            let text = match msg {
                tungstenite::Message::Text(t) => t.to_string(),
                tungstenite::Message::Binary(b) => match String::from_utf8(b.to_vec()) {
                    Ok(s) => s,
                    Err(_) => continue,
                },
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
                _ => continue,
            };
            let Ok(ev) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            if is_setup_complete(&ev) {
                return Ok(true);
            }
            if let Some(err) = error_message(&ev) {
                return Err(diag::fail(
                    SCOPE,
                    "server_error",
                    format!("Server rejected the session: {}", err),
                ));
            }
        }
        Ok(false)
    })
    .await
    .map_err(|_| {
        diag::fail(
            SCOPE,
            "setup_timeout",
            "Timed out waiting for the live session to start".to_string(),
        )
    })??;

    if !ready {
        return Err(diag::fail(
            SCOPE,
            "closed_before_ready",
            "WebSocket closed before setup completed".to_string(),
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

/// 開啟流式會話。熱詞引數收下但目前不用 —— Live API 這條路沒有公開的熱詞欄位，
/// 保留引數是為了和其他流式供應商的 open 簽名一致（前端統一傳）。
#[tauri::command]
pub async fn gemini_live_open(
    app: AppHandle,
    config: AsrProviderConfig,
    hotwords: Option<Vec<String>>,
    realtime: Option<bool>,
) -> Result<(), String> {
    let realtime = realtime.unwrap_or(false);
    cleanup().await;

    let hotword_count = hotwords.as_ref().map(|h| h.len()).unwrap_or(0);
    let (ws, model) = open_session(&config).await?;

    let (sink, stream) = ws.split();
    *STATE.lock().await = Transcript::default();
    *SINK.lock().await = Some(sink);
    let handle = tokio::spawn(run_reader(stream, app, STATE.clone(), realtime));
    *READER.lock().await = Some(handle);
    ACTIVE.store(true, Ordering::SeqCst);
    diag::log(
        SCOPE,
        "stream_open",
        &format!(
            "realtime={} model={} hotwords_ignored={}",
            realtime, model, hotword_count
        ),
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
            Ok(tungstenite::Message::Text(t)) => t.to_string(),
            Ok(tungstenite::Message::Binary(b)) => match String::from_utf8(b.to_vec()) {
                Ok(s) => s,
                Err(_) => continue,
            },
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
                        serde_json::json!({ "text": display, "provider": "gemini_live" }),
                    );
                }
            }
            Applied::Finished | Applied::Failed => {
                ended_cleanly = true;
                break;
            }
            Applied::Ignored => {}
        }
    }
    let mut s = state.lock().await;
    s.finished = true;
    // 協議沒跑完就斷了必須留痕，否則 finish 只能返回空串，而空串會被顯示成
    // 「未檢測到有效聲音」，把使用者引去查麥克風（見 pitfalls 第 15 條）。
    if !ended_cleanly && s.error.is_none() {
        s.error = Some(format!(
            "connection closed before the turn completed: {}",
            diag::truncate(&close_reason, 200)
        ));
    }
    diag::log(
        SCOPE,
        "reader_stopped",
        &format!(
            "emits={} interim={} final={} clean={}",
            emitted, s.interim_events, s.final_events, ended_cleanly
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
pub async fn gemini_live_send(pcm_b64: String) -> Result<(), String> {
    if !ACTIVE.load(Ordering::SeqCst) {
        return Err(diag::fail(
            SCOPE,
            "send_without_session",
            "Session is not open".to_string(),
        ));
    }
    // 音訊本來就是 base64 過來的，這個協議也要 base64 塞 JSON，所以不解碼。
    let payload = serde_json::to_string(&audio_payload(&pcm_b64)).unwrap();
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
pub async fn gemini_live_finish() -> Result<String, String> {
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
                serde_json::to_string(&audio_end_payload()).unwrap().into(),
            ))
            .await
        {
            diag::log(SCOPE, "audio_end_send_err_ignored", &e.to_string());
        }
    }

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        {
            let st = STATE.lock().await;
            if st.finished {
                let err = st.error.clone();
                let text = st.display().trim().to_string();
                let chunks = st.final_events;
                drop(st);
                cleanup().await;
                // 已經識別出文字就交回去，哪怕收尾時斷了 —— 丟掉它等於讓使用者白說一遍。
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
                    &format!("Session finished with no transcript chunks={}", chunks),
                );
                return Ok(String::new());
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    // 超時也把已拿到的文本交回去。見檔案頭 D 條：turnComplete 不來時就靠這裡兜底。
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
pub async fn gemini_live_close() -> Result<(), String> {
    cleanup().await;
    Ok(())
}

// ─────────────────────────── 連通性測試 ───────────────────────────

pub async fn test_connection(config: &AsrProviderConfig) -> super::types::TestResult {
    let start = Instant::now();
    let model = resolve_model(config);
    let result = async {
        let (mut ws, _) = open_session(config).await?;
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
            provider: "gemini_live_transcribe".to_string(),
            api_key: "AIza-test".to_string(),
            app_id: String::new(),
            extra,
        }
    }

    #[test]
    fn model_falls_back_to_the_live_default() {
        assert_eq!(resolve_model(&config(serde_json::json!({}))), DEFAULT_MODEL);
        assert_eq!(
            resolve_model(&config(serde_json::json!({ "model": " " }))),
            DEFAULT_MODEL
        );
    }

    /// setup 要的是資源名（models/xxx），給裸模型 ID 會被拒；已經帶字首的不能再加一層。
    #[test]
    fn model_is_qualified_exactly_once() {
        assert_eq!(
            qualified_model("gemini-3.5-transcribe-live"),
            "models/gemini-3.5-transcribe-live"
        );
        assert_eq!(
            qualified_model("models/gemini-3.5-transcribe-live"),
            "models/gemini-3.5-transcribe-live"
        );
    }

    /// responseModalities 必須限定成 TEXT —— 不限定的話 Live API 會合成語音回話，
    /// 白燒 token 還把 serverContent 塞滿我們不要的音訊。
    #[test]
    fn setup_asks_for_text_only_and_input_transcription() {
        let payload = setup_payload(DEFAULT_MODEL);
        assert_eq!(payload["setup"]["model"], "models/gemini-3.5-transcribe-live");
        assert_eq!(
            payload["setup"]["generationConfig"]["responseModalities"][0],
            "TEXT"
        );
        assert!(payload["setup"]["inputAudioTranscription"].is_object());
    }

    /// 取樣率只能透過 mimeType 宣告，沒有單獨欄位。寫錯不會報錯，只會識別出亂碼。
    #[test]
    fn audio_mime_type_carries_the_sample_rate() {
        let payload = audio_payload("QUJD");
        assert_eq!(
            payload["realtimeInput"]["audio"]["mimeType"],
            "audio/pcm;rate=16000"
        );
        assert_eq!(payload["realtimeInput"]["audio"]["data"], "QUJD");
        assert_eq!(audio_end_payload()["realtimeInput"]["audioStreamEnd"], true);
    }

    /// 金鑰裡混進空格/換行時（貼上常見）不轉義會得到一個和金鑰毫無關係的 URL 解析錯誤。
    /// 同時確認 URL 拼裝本身沒寫錯。
    #[test]
    fn ws_url_strips_whitespace_from_the_key() {
        let url = ws_url(" AIza_abc-123 \n");
        assert!(url.starts_with("wss://generativelanguage.googleapis.com/ws/"));
        assert!(url.ends_with("?key=AIza_abc-123"));
        assert!(!url.contains(' '));
        assert!(!url.contains('\n'));
    }

    #[test]
    fn setup_complete_accepts_both_spellings() {
        assert!(is_setup_complete(&serde_json::json!({ "setupComplete": {} })));
        assert!(is_setup_complete(&serde_json::json!({ "setup_complete": {} })));
        assert!(!is_setup_complete(&serde_json::json!({ "serverContent": {} })));
    }

    /// 兩種命名、兩種結束訊號都要認：只認一個的話，缺的那種會一直等到超時才收尾。
    #[test]
    fn transcript_reads_both_namings_and_turn_signals() {
        let camel = serde_json::json!({
            "serverContent": { "inputTranscription": { "text": "你好" } }
        });
        assert_eq!(final_transcription(&camel), Some("你好"));
        let snake = serde_json::json!({
            "serverContent": { "input_transcription": { "text": "hi" } }
        });
        assert_eq!(final_transcription(&snake), Some("hi"));
        let interim = serde_json::json!({
            "serverContent": { "interimInputTranscription": { "text": "進行中" } }
        });
        assert_eq!(interim_transcription(&interim), Some("進行中"));
        // 兩個欄位必須分得開：interim 不能被當成定稿，否則中間態那些臨時空格會進最終文本
        assert_eq!(final_transcription(&interim), None);
        assert_eq!(interim_transcription(&camel), None);

        // 實測來的是 generationComplete；turnComplete 也保留著以防換法
        assert!(is_turn_done(&serde_json::json!({
            "serverContent": { "generationComplete": true }
        })));
        assert!(is_turn_done(&serde_json::json!({
            "serverContent": { "turnComplete": true }
        })));
        assert!(!is_turn_done(&serde_json::json!({
            "serverContent": { "turnComplete": false }
        })));
    }

    /// 用**實測的原樣事件序列**跑一遍（2026-09-16 那次探測的完整回放）。
    ///
    /// 這條同時釘住兩件照文件寫會做錯的事：interim 是全量（要覆蓋，不是追加），
    /// 以及定稿要替掉中間態 —— 注意實測的中間態裡有臨時空格"語音輸入法測試成 功。"，
    /// 最終必須是沒有空格的那版。
    #[test]
    fn replays_the_measured_event_stream() {
        let mut t = Transcript::default();
        let interims = ["語音輸入", "語音 輸入法", "語音輸入法測試成", "語音輸入法測試成 功。"];
        for text in interims {
            assert!(matches!(
                t.apply(&serde_json::json!({
                    "serverContent": { "interimInputTranscription": { "text": text } }
                })),
                Applied::Updated
            ));
            // 每一步都只顯示當前那一條，絕不疊加
            assert_eq!(t.display(), text);
        }
        assert_eq!(t.interim_events, 4);

        assert!(matches!(
            t.apply(&serde_json::json!({
                "serverContent": { "inputTranscription": { "text": "語音輸入法測試成功。" } }
            })),
            Applied::Updated
        ));
        assert_eq!(t.display(), "語音輸入法測試成功。");
        assert_eq!(t.final_events, 1);

        assert!(matches!(
            t.apply(&serde_json::json!({ "serverContent": { "generationComplete": true } })),
            Applied::Finished
        ));
        assert!(t.finished);
    }

    /// 服務端按語音活動自己分輪時，定稿要按輪次累加 —— 只留最後一輪會丟掉前半段。
    #[test]
    fn multiple_final_turns_accumulate() {
        let mut t = Transcript::default();
        t.apply(&serde_json::json!({
            "serverContent": { "inputTranscription": { "text": "第一句。" } }
        }));
        t.apply(&serde_json::json!({
            "serverContent": { "interimInputTranscription": { "text": "第二" } }
        }));
        assert_eq!(t.display(), "第一句。第二");
        t.apply(&serde_json::json!({
            "serverContent": { "inputTranscription": { "text": "第二句。" } }
        }));
        assert_eq!(t.display(), "第一句。第二句。");
    }

    #[test]
    fn empty_chunks_and_unknown_events_are_ignored() {
        let mut t = Transcript::default();
        assert!(matches!(
            t.apply(&serde_json::json!({
                "serverContent": { "interimInputTranscription": { "text": "" } }
            })),
            Applied::Ignored
        ));
        assert!(matches!(
            t.apply(&serde_json::json!({ "usageMetadata": { "totalTokenCount": 3 } })),
            Applied::Ignored
        ));
        // 實測會來的 VAD 事件，必須當無關事件放過
        assert!(matches!(
            t.apply(&serde_json::json!({
                "serverContent": {},
                "voiceActivity": { "type": "ACTIVITY_START", "audioOffset": "0.840s" }
            })),
            Applied::Ignored
        ));
        assert_eq!(t.interim_events, 0);
        assert_eq!(t.final_events, 0);
    }

    #[test]
    fn server_error_is_captured_with_code() {
        let mut t = Transcript::default();
        assert!(matches!(
            t.apply(&serde_json::json!({
                "error": { "code": 429, "message": "RESOURCE_EXHAUSTED" }
            })),
            Applied::Failed
        ));
        let err = t.error.unwrap();
        assert!(err.contains("RESOURCE_EXHAUSTED"));
        assert!(err.contains("429"));
    }
}
