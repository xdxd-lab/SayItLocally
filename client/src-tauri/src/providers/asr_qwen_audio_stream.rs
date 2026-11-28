// 千問 Qwen-Audio-ASR-Flash 流式語音識別 — 3.0 與 3.1 兩代共用這一份
//
// ⚠️ 協議與 asr_qwen_realtime.rs **不是同一套**，別指望改個模型名就能複用：
//   · asr_qwen_realtime → `/api-ws/v1/realtime`，OpenAI-Realtime 风格事件
//     （session.update / input_audio_buffer.append，音訊 base64 塞進 JSON）；
//   · 這一份            → `/api-ws/v1/inference`，DashScope 原生 duplex 指令
//     （run-task → task-started → 裸二進位制音訊幀 → result-generated →
//      finish-task → task-finished）。
//
// 實測結論（`dev-scripts/probe_qwen_audio30_stream.py`，打的是真實介面）：
//   · 通用域名 `dashscope.aliyuncs.com` 與業務空間專屬域名都能用 —— 這個模型
//     **不需要 WorkspaceId**（qwen3-asr-flash-realtime 才需要）。填了就走專屬域名
//     （阿里官方稱更穩），沒填照樣工作，所以設定頁不把它列為必填項。
//   · `result-generated` 裡 `sentence.text` 是**當前句的全量文本**，不是增量。
//   · 開頭十幾條 `result-generated` 的 text 是空串（VAD 還沒判出語音），必須丟掉，
//     否則懸浮窗會先閃一下空字幕。
//   · 每個 `sentence_end=true` 的句子自帶句尾標點（中文「。」、英文 ". "），
//     所以分句拼接**不加分隔符**；asr_qwen_realtime 那邊補「，」是因為它的分句不帶標點。
//   · 音訊不按即時節奏發也能用（10.84s 音訊一次性推完，2.6s 出全文），
//     所以一次性識別那條路不需要 sleep 假裝即時。
//
// ## 3.1 為什麼能直接複用這份實現（2026-09-23 實測）
//
// `dev-scripts/probe_qwen_31_stream_hotwords.py` 拿同一段音訊、同一套 run-task
// 打了 `qwen-audio-3.1-asr-flash-streaming`：轉寫結果與 3.0 逐字相同，中間結果
// 同樣是 3 條、首條同樣在 1.64s，而且**接受我們發的 `parameters.vocabulary`**。
// 那個欄位是這裡唯一的風險點 —— 被拒的話整條 run-task 會失敗，於是配了熱詞的使用者
// 會**完全無法識別**，而不是"熱詞不生效"。
//
// ⚠️ 同一次實測裡，一個**編造的欄位名也被照樣接受**。所以"接受 vocabulary"只證明
// run-task 不會被拒，**不證明熱詞真的參與了偏置**。對外口徑仍是「配了有幫助、
// 不保證命中」（capabilities.rs 的 Vocabulary 檔），別升級成「3.1 支援熱詞」。
//
// 官方文件另有一處兩代不同：VAD 斷句在 3.0 叫 `max_sentence_silence`、在 3.1 改成
// `vad_model`。我們兩個都不發（用服務端預設），所以這處差異碰不到 —— 真要加斷句
// 配置時必須按模型分開發，不能共用一個欄位名。

use super::diag;
use super::types::{AsrProviderConfig, AsrResult, TestResult};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{Sink, SinkExt, StreamExt};
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

/// `extra.model` 缺失時用哪個。
///
/// 刻意留在 3.0 而不是跟著前端預設升到 3.1：這個值只在異常路徑上生效
/// （前端每次都會把選中的模型放進 extra.model），而異常路徑該落到**已知行為不變**
/// 的那一代，不該順手換成一個使用者沒選過的模型。
const DEFAULT_MODEL: &str = "qwen-audio-3.0-asr-flash-streaming";

/// 日誌字首。
///
/// 從 `qwen/audio30` 改成 `audio3x` 是因為同一份程式碼現在也跑 3.1 —— 舊名會讓日誌
/// 出現 `[qwen/audio30] model=qwen-audio-3.1-...` 這種自相矛盾的行，排查時先要
/// 懷疑一遍自己看錯了。每條 connected 日誌都帶 `model=`，那才是判斷跑了哪代的依據。
const SCOPE: &str = "qwen/audio3x";
const WS_PATH: &str = "/api-ws/v1/inference";
/// 沒配業務空間時的通用域名。實測與專屬域名同樣可用。
const GENERIC_HOST: &str = "dashscope.aliyuncs.com";

/// 音訊幀大小：16kHz 單聲道 16-bit 下正好 100ms。
///
/// 官方示例用的就是 3200 位元組，probe 指令碼也是按這個尺寸實測透過的 ——
/// 一次性路徑不改成"一大幀發完"是因為那個形狀沒驗證過，而這個驗證過。
const FRAME_BYTES: usize = 3200;

/// 即時熱詞權重。文件區間 [1,5]，越大越傾向輸出該詞；50 是「超級熱詞」
/// （召回大幅提升但最多 50 個）。取 4 是要偏置效果又不想把普通詞條推成超級熱詞。
const HOTWORD_WEIGHT: u32 = 4;

/// 即時熱詞條數上限。文件只對「超級熱詞」明確了 50 的上限，普通熱詞沒給數字；
/// 這裡自己設一個上限並記日誌，避免詞庫很大時 run-task 被整條拒掉 ——
/// 那會讓識別**完全不可用**，而不是"熱詞少生效幾個"。
///
/// ⚠️ 這是**客戶端自設**的，不是服務端限制。介面上要說"SayIt 最多傳送 N 條"，
/// 不能說成"千問協議只吃 N 條"（capabilities.rs 的 client_cap 就是為此區分的）。
const HOTWORD_LIMIT: usize = 100;

/// 給 `capabilities.rs` 的測試用，讓那邊的說明值和這裡的請求引數釘在一起。
/// 只在測試裡存在：說明和請求引數刻意各留一份，理由見 capabilities.rs。
#[cfg(test)]
pub fn hotword_limit_for_docs() -> usize {
    HOTWORD_LIMIT
}

type WsStream = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;
type WsSink = SplitSink<WsStream, tungstenite::Message>;

// ─────────────────────────── 協議編解碼 ───────────────────────────

fn ws_url(workspace: &str) -> String {
    let workspace = workspace.trim();
    if workspace.is_empty() {
        format!("wss://{}{}", GENERIC_HOST, WS_PATH)
    } else {
        format!("wss://{}.cn-beijing.maas.aliyuncs.com{}", workspace, WS_PATH)
    }
}

/// 這次要發的模型名，來自前端選中的那一項（`extra.model`）。
///
/// 不做白名單校驗，與 asr_qwen_omni::get_model 一致：模型名只出現在 run-task 的
/// payload 裡、不進 URL，寫錯的後果是服務端回一條指名道姓的 task-failed
/// （`InvalidParameter: ...`），比我們自己維護第二份清單更準 —— 那份清單一定會
/// 比百鍊的模型表舊。
fn get_model(config: &AsrProviderConfig) -> String {
    config
        .extra
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_MODEL)
        .to_string()
}

/// 熱詞 → `parameters.vocabulary`（`{詞: 權重}`）。
///
/// 與 asr_qwen.rs 的做法不同：那邊只能把熱詞拼成一段上下文文本，這個模型有專門的
/// 即時熱詞欄位，權重可控，也不存在「模型把整串熱詞當識別結果吐回來」的回顯風險。
fn build_vocabulary(hotwords: &[String]) -> Option<(serde_json::Value, usize, usize)> {
    let mut seen = std::collections::HashSet::new();
    let words: Vec<&str> = hotwords
        .iter()
        .map(|w| w.trim())
        .filter(|w| !w.is_empty())
        .filter(|w| seen.insert(w.to_lowercase()))
        .collect();
    if words.is_empty() {
        return None;
    }
    let total = words.len();
    let mut map = serde_json::Map::new();
    for w in words.iter().take(HOTWORD_LIMIT) {
        map.insert((*w).to_string(), serde_json::json!(HOTWORD_WEIGHT));
    }
    let used = map.len();
    Some((serde_json::Value::Object(map), used, total))
}

fn run_task_payload(
    model: &str,
    task_id: &str,
    sample_rate: u32,
    hotwords: &[String],
) -> serde_json::Value {
    let mut parameters = serde_json::json!({
        "format": "pcm",
        "sample_rate": sample_rate,
    });
    if let Some((vocabulary, used, total)) = build_vocabulary(hotwords) {
        parameters["vocabulary"] = vocabulary;
        diag::log(
            SCOPE,
            "hotwords",
            &format!("vocabulary={} of={} weight={}", used, total, HOTWORD_WEIGHT),
        );
    }
    serde_json::json!({
        "header": { "action": "run-task", "task_id": task_id, "streaming": "duplex" },
        "payload": {
            "task_group": "audio",
            "task": "asr",
            "function": "recognition",
            "model": model,
            "parameters": parameters,
            "input": {},
        }
    })
}

fn finish_task_payload(task_id: &str) -> serde_json::Value {
    serde_json::json!({
        "header": { "action": "finish-task", "task_id": task_id, "streaming": "duplex" },
        "payload": { "input": {} }
    })
}

fn event_name(ev: &serde_json::Value) -> &str {
    ev.get("header")
        .and_then(|h| h.get("event"))
        .and_then(|e| e.as_str())
        .unwrap_or("")
}

/// task-failed 的原因。錯誤碼和文案都在 header 裡，不在 payload。
fn task_failed_reason(ev: &serde_json::Value) -> String {
    let header = ev.get("header");
    let code = header
        .and_then(|h| h.get("error_code"))
        .and_then(|c| c.as_str())
        .unwrap_or("unknown");
    let message = header
        .and_then(|h| h.get("error_message"))
        .and_then(|m| m.as_str())
        .unwrap_or("Unknown error");
    format!("{}: {}", code, diag::truncate(message, 200))
}

/// `payload.output.sentence` → (當前句文本, 是否已斷句)
fn sentence_of(ev: &serde_json::Value) -> Option<(&str, bool)> {
    let sentence = ev.get("payload")?.get("output")?.get("sentence")?;
    let text = sentence.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let ended = sentence
        .get("sentence_end")
        .and_then(|e| e.as_bool())
        .unwrap_or(false);
    Some((text, ended))
}

/// 一次會話累積到的文本。committed = 已斷句的句子，partial = 進行中的那一句。
#[derive(Default)]
struct Transcript {
    committed: String,
    partial: String,
    finished: bool,
    error: Option<String>,
    /// 收到過多少條帶文本的中間結果，只用於日誌
    partial_events: usize,
}

/// 一條事件對累積狀態做了什麼，呼叫方據此決定要不要上拋 / 退出迴圈。
enum Applied {
    /// 與文本無關的事件（task-started、空文本的中間結果等）
    Ignored,
    /// 可顯示文本變了
    Updated,
    Finished,
    Failed,
}

impl Transcript {
    /// 即時展示用文本 = 已斷句 + 進行中。分句自帶標點，所以直接相接。
    fn display(&self) -> String {
        format!("{}{}", self.committed, self.partial)
    }

    fn apply(&mut self, ev: &serde_json::Value) -> Applied {
        match event_name(ev) {
            "result-generated" => {
                let Some((text, ended)) = sentence_of(ev) else {
                    return Applied::Ignored;
                };
                // 開頭那十幾條空文本的中間結果必須丟掉，否則懸浮窗先閃一下空字幕
                if text.is_empty() {
                    return Applied::Ignored;
                }
                if ended {
                    self.committed.push_str(text);
                    self.partial.clear();
                } else {
                    // text 是當前句的全量文本（不是增量），直接覆蓋
                    self.partial.clear();
                    self.partial.push_str(text);
                }
                self.partial_events += 1;
                Applied::Updated
            }
            "task-finished" => {
                self.finished = true;
                Applied::Finished
            }
            "task-failed" => {
                self.error = Some(task_failed_reason(ev));
                self.finished = true;
                Applied::Failed
            }
            _ => Applied::Ignored,
        }
    }
}

// ─────────────────────────── 建連與開任務 ───────────────────────────

struct Task {
    ws: WsStream,
    task_id: String,
}

/// 建連 → 發 run-task → 等 task-started。拿到 Task 才算可以發音訊。
async fn start_task(
    config: &AsrProviderConfig,
    workspace: &str,
    sample_rate: u32,
    hotwords: &[String],
) -> Result<Task, String> {
    if config.api_key.trim().is_empty() {
        return Err(diag::fail_code(
            SCOPE,
            "credentials",
            "provider_bad_key",
            "Qwen ASR is missing the API Key; complete it in Settings".to_string(),
        ));
    }

    let url = ws_url(workspace);
    let mut request = url.as_str().into_client_request().map_err(|e| {
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
    // UA 不是這個端點的硬要求（實測不帶也能連），但我們所有出網請求都帶：
    // 自有 ALB 與第三方 nginx 閘道器會按「無 UA」直接 403，且報錯裡絕不提 UA。
    request.headers_mut().insert(
        USER_AGENT,
        HeaderValue::from_static(concat!("SayIt/", env!("CARGO_PKG_VERSION"))),
    );
    let workspace = workspace.trim();
    if !workspace.is_empty() {
        if let Ok(value) = HeaderValue::from_str(workspace) {
            request.headers_mut().insert("X-DashScope-WorkSpace", value);
        }
    }

    let (mut ws, response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| {
            diag::fail(
                SCOPE,
                "connect",
                format!("WebSocket connection failed: {}", e),
            )
        })?;

    let task_id = uuid::Uuid::new_v4().to_string();
    let model = get_model(config);
    // model= 是判斷「這次到底跑了哪一代」的唯一依據。3.0 和 3.1 共用這份程式碼、
    // 共用 SCOPE，日誌裡除了這個欄位沒有別的地方能區分它們。
    diag::log(
        SCOPE,
        "connected",
        &format!(
            "status={} model={} host={} rate={}",
            response.status(),
            model,
            if workspace.is_empty() { GENERIC_HOST } else { "workspace" },
            sample_rate
        ),
    );

    let payload = run_task_payload(&model, &task_id, sample_rate, hotwords);
    ws.send(tungstenite::Message::Text(
        serde_json::to_string(&payload).unwrap().into(),
    ))
    .await
    .map_err(|e| {
        diag::fail(
            SCOPE,
            "send_run_task",
            format!("Failed to send run-task: {}", e),
        )
    })?;

    // 必須等到 task-started 才算就緒。額度耗盡 / 模型未開通時服務端回 task-failed
    // 或直接關連線，都要在這裡變成 Err，讓上層回退到別的路徑而不是空轉。
    let deadline = Duration::from_secs(15);
    let started = tokio::time::timeout(deadline, async {
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
                    match event_name(&ev) {
                        "task-started" => return Ok(true),
                        "task-failed" => {
                            return Err(diag::fail(
                                SCOPE,
                                "task_failed",
                                format!("Server rejected the task: {}", task_failed_reason(&ev)),
                            ))
                        }
                        _ => {}
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
            "task_started_timeout",
            "Timed out waiting for the task to start".to_string(),
        )
    })??;

    if !started {
        return Err(diag::fail(
            SCOPE,
            "closed_before_ready",
            "WebSocket closed before the task started".to_string(),
        ));
    }
    diag::log(SCOPE, "task_started", "");
    Ok(Task { ws, task_id })
}

/// 把 PCM 切成 100ms 的二進位制幀發出去。
async fn send_frames<S>(sink: &mut S, pcm: &[u8], stage: &'static str) -> Result<(), String>
where
    S: Sink<tungstenite::Message, Error = tungstenite::Error> + Unpin,
{
    for frame in pcm.chunks(FRAME_BYTES) {
        sink.send(tungstenite::Message::Binary(frame.to_vec().into()))
            .await
            .map_err(|e| diag::fail(SCOPE, stage, format!("Failed to send audio: {}", e)))?;
    }
    Ok(())
}

// ─────────────────────────── 一次性識別 ───────────────────────────

/// 錄完再發：整段 PCM 推完 → finish-task → 收全文。
///
/// 設定頁的「識別測試」和關掉即時字幕時的錄音都走這裡，而它們都不傳業務空間 ID，
/// 所以這條路必須能用通用域名 —— 已實測可用。
pub async fn transcribe(
    audio_pcm_b64: &str,
    sample_rate: u32,
    config: &AsrProviderConfig,
    hotwords: &[String],
) -> Result<AsrResult, String> {
    let pcm =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, audio_pcm_b64).map_err(
            |e| {
                diag::fail(
                    SCOPE,
                    "decode_b64",
                    format!("Failed to decode base64 audio: {}", e),
                )
            },
        )?;
    if pcm.is_empty() {
        diag::empty_result(SCOPE, "Input audio was empty; provider request was skipped");
        return Ok(AsrResult {
            text: String::new(),
            elapsed_ms: 0,
        });
    }

    let workspace = config
        .extra
        .get("workspaceId")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let audio_sec = pcm.len() as f64 / (sample_rate.max(1) as f64 * 2.0);
    diag::log(
        SCOPE,
        "start",
        &format!(
            "pcm_bytes={} audio_sec={:.1} rate={} hotwords={}",
            pcm.len(),
            audio_sec,
            sample_rate,
            hotwords.len()
        ),
    );

    let start = Instant::now();
    let Task { mut ws, task_id } = start_task(config, workspace, sample_rate, hotwords).await?;
    send_frames(&mut ws, &pcm, "send_audio").await?;
    ws.send(tungstenite::Message::Text(
        serde_json::to_string(&finish_task_payload(&task_id))
            .unwrap()
            .into(),
    ))
    .await
    .map_err(|e| {
        diag::fail(
            SCOPE,
            "send_finish",
            format!("Failed to send finish-task: {}", e),
        )
    })?;

    let mut transcript = Transcript::default();
    // 上限按音訊長度放寬：10.8s 音訊實測 2.6s 收全，長音訊線性長一些。
    let collect_budget = Duration::from_secs(20) + Duration::from_secs_f64(audio_sec.min(300.0));
    let closed_early = tokio::time::timeout(collect_budget, async {
        while let Some(msg) = ws.next().await {
            let msg = msg.map_err(|e| {
                diag::fail(SCOPE, "recv", format!("Failed to receive result: {}", e))
            })?;
            match msg {
                tungstenite::Message::Text(text) => {
                    let Ok(ev) = serde_json::from_str::<serde_json::Value>(&text) else {
                        continue;
                    };
                    match transcript.apply(&ev) {
                        Applied::Finished => return Ok(false),
                        Applied::Failed => {
                            return Err(diag::fail(
                                SCOPE,
                                "task_failed",
                                format!(
                                    "Recognition failed: {}",
                                    transcript.error.clone().unwrap_or_default()
                                ),
                            ))
                        }
                        _ => {}
                    }
                }
                // 服務端提前關連線：協議沒跑完，不能當成"識別到空文本"上報成功，
                // 否則真實原因（額度、許可權、協議錯）會被顯示成「未檢測到有效聲音」。
                tungstenite::Message::Close(frame) => {
                    let reason = frame
                        .map(|f| f.reason.to_string())
                        .unwrap_or_else(|| "No reason".to_string());
                    diag::log(SCOPE, "closed_before_finished", &reason);
                    return Ok(true);
                }
                _ => {}
            }
        }
        Ok(true)
    })
    .await
    .map_err(|_| {
        diag::fail(
            SCOPE,
            "collect_timeout",
            "Timed out waiting for the recognition result".to_string(),
        )
    })??;

    let _ = ws.close(None).await;
    let text = transcript.display().trim().to_string();
    let elapsed_ms = start.elapsed().as_millis() as u64;

    if closed_early && text.is_empty() {
        return Err(diag::fail(
            SCOPE,
            "closed_before_finished",
            "The server closed the connection before returning a result".to_string(),
        ));
    }
    if text.is_empty() {
        diag::empty_result(
            SCOPE,
            &format!(
                "Task finished with no transcript audio_sec={:.1} elapsed={}ms events={}",
                audio_sec, elapsed_ms, transcript.partial_events
            ),
        );
    } else {
        diag::ok(SCOPE, elapsed_ms, text.chars().count());
    }
    Ok(AsrResult { text, elapsed_ms })
}

pub async fn test_connection(config: &AsrProviderConfig) -> TestResult {
    let start = Instant::now();
    // 只握手 + 開任務 + 立刻收尾：能開出任務就說明金鑰、模型、地域都對得上。
    // 不發音訊（這個命令只回答"能不能連"，真實轉寫由設定頁的識別測試負責）。
    let result = async {
        let workspace = config
            .extra
            .get("workspaceId")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let Task { mut ws, task_id } = start_task(config, workspace, 16000, &[]).await?;
        let _ = ws
            .send(tungstenite::Message::Text(
                serde_json::to_string(&finish_task_payload(&task_id))
                    .unwrap()
                    .into(),
            ))
            .await;
        let _ = ws.close(None).await;
        Ok::<(), String>(())
    }
    .await;

    let elapsed_ms = start.elapsed().as_millis() as u64;
    match result {
        Ok(()) => TestResult {
            ok: true,
            message: format!("Connection successful ({}ms)", elapsed_ms),
            elapsed_ms,
            // 報選中的那個模型，不是 DEFAULT_MODEL —— 兩代共用這份實現，寫死一個
            // 名字會讓「測試連線」對著 3.1 的配置顯示 3.0。
            detail: format!("model: {}", get_model(config)),
        },
        Err(message) => TestResult {
            ok: false,
            message,
            elapsed_ms,
            detail: String::new(),
        },
    }
}

// ─────────────────────────── 流式會話 ───────────────────────────

static SINK: once_cell::sync::Lazy<Arc<Mutex<Option<WsSink>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(None)));
static READER: once_cell::sync::Lazy<Arc<Mutex<Option<JoinHandle<()>>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(None)));
static STATE: once_cell::sync::Lazy<Arc<Mutex<Transcript>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(Transcript::default())));
static TASK_ID: once_cell::sync::Lazy<Arc<Mutex<String>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(String::new())));
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// 開啟流式會話。
///
/// 與 asr_qwen_realtime 不同，這裡**不分「即時 / 普通」兩條程式碼路徑**：無論要不要
/// 上拋中間結果，都必須有後台 reader 一直讀。這個協議在發音訊期間每 ~100ms 就推一條
/// result-generated，沒人讀就會把連線堵死；realtime 只決定要不要 emit。
#[tauri::command]
pub async fn qwen_audio_stream_open(
    app: AppHandle,
    config: AsrProviderConfig,
    hotwords: Option<Vec<String>>,
    realtime: Option<bool>,
    workspace_id: Option<String>,
) -> Result<(), String> {
    let realtime = realtime.unwrap_or(false);
    cleanup().await;

    let hotwords = hotwords.unwrap_or_default();
    let workspace = workspace_id.unwrap_or_default();
    let Task { ws, task_id } = start_task(&config, &workspace, 16000, &hotwords).await?;

    let (sink, stream) = ws.split();
    *STATE.lock().await = Transcript::default();
    *TASK_ID.lock().await = task_id;
    *SINK.lock().await = Some(sink);
    let handle = tokio::spawn(run_reader(stream, app, STATE.clone(), realtime));
    *READER.lock().await = Some(handle);
    ACTIVE.store(true, Ordering::SeqCst);
    diag::log(SCOPE, "stream_open", &format!("realtime={}", realtime));
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
                        serde_json::json!({ "text": display, "provider": "qwen_audio_stream" }),
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
    // 協議沒跑完就斷了要記下來。否則 finish 只能返回一個空字串，而空字串會被
    // 顯示成「未檢測到有效聲音」，把使用者引去查麥克風 —— 真實原因（額度、鑑權、
    // 網路）就此消失。見 pitfalls 第 15 條。
    if !ended_cleanly && s.error.is_none() {
        s.error = Some(format!(
            "connection closed before task-finished: {}",
            diag::truncate(&close_reason, 200)
        ));
    }
    diag::log(
        SCOPE,
        "reader_stopped",
        &format!(
            "emits={} events={} clean={}",
            emitted, s.partial_events, ended_cleanly
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
    TASK_ID.lock().await.clear();
}

#[tauri::command]
pub async fn qwen_audio_stream_send(pcm_b64: String) -> Result<(), String> {
    if !ACTIVE.load(Ordering::SeqCst) {
        return Err(diag::fail(
            SCOPE,
            "send_without_session",
            "Session is not open".to_string(),
        ));
    }
    let pcm = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &pcm_b64)
        .map_err(|e| {
            diag::fail(
                SCOPE,
                "decode_b64",
                format!("Failed to decode base64 audio: {}", e),
            )
        })?;
    let mut sink = SINK.lock().await;
    let s = sink.as_mut().ok_or_else(|| {
        diag::fail(
            SCOPE,
            "send_without_session",
            "Session is not open".to_string(),
        )
    })?;
    send_frames(s, &pcm, "send_audio").await
}

#[tauri::command]
pub async fn qwen_audio_stream_finish() -> Result<String, String> {
    if !ACTIVE.load(Ordering::SeqCst) {
        return Err(diag::fail(
            SCOPE,
            "finish_without_session",
            "Session is not open".to_string(),
        ));
    }
    let task_id = TASK_ID.lock().await.clone();
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
                serde_json::to_string(&finish_task_payload(&task_id))
                    .unwrap()
                    .into(),
            ))
            .await
        {
            diag::log(SCOPE, "finish_send_err_ignored", &e.to_string());
        }
    }

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        {
            let st = STATE.lock().await;
            if st.finished {
                let err = st.error.clone();
                let text = st.display().trim().to_string();
                let events = st.partial_events;
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
                        format!("Recognition failed: {}", err),
                    ));
                }
                diag::empty_result(
                    SCOPE,
                    &format!("Task finished with no transcript events={}", events),
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
pub async fn qwen_audio_stream_close() -> Result<(), String> {
    cleanup().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 沒配業務空間也要能工作：設定頁的識別測試就不傳它。
    #[test]
    fn ws_url_falls_back_to_the_generic_host() {
        assert_eq!(
            ws_url(""),
            "wss://dashscope.aliyuncs.com/api-ws/v1/inference"
        );
        assert_eq!(
            ws_url("  "),
            "wss://dashscope.aliyuncs.com/api-ws/v1/inference"
        );
        assert_eq!(
            ws_url("ws-abc"),
            "wss://ws-abc.cn-beijing.maas.aliyuncs.com/api-ws/v1/inference"
        );
    }

    #[test]
    fn run_task_payload_matches_the_documented_shape() {
        let payload = run_task_payload(DEFAULT_MODEL, "tid", 16000, &[]);
        assert_eq!(payload["header"]["action"], "run-task");
        assert_eq!(payload["header"]["task_id"], "tid");
        assert_eq!(payload["header"]["streaming"], "duplex");
        assert_eq!(payload["payload"]["task_group"], "audio");
        assert_eq!(payload["payload"]["task"], "asr");
        assert_eq!(payload["payload"]["function"], "recognition");
        assert_eq!(payload["payload"]["model"], DEFAULT_MODEL);
        assert_eq!(payload["payload"]["parameters"]["format"], "pcm");
        assert_eq!(payload["payload"]["parameters"]["sample_rate"], 16000);
        // input 是必填，沒有上下文時也要是一個空物件
        assert!(payload["payload"]["input"].is_object());
        assert!(payload["payload"]["parameters"].get("vocabulary").is_none());
    }

    /// 選中的模型必須真的出現在 run-task 裡。
    ///
    /// 這條釘的是 3.1 上線時最容易出的那個錯：前端把模型名放進 `extra.model`，
    /// Rust 這邊卻還在用常量 —— 介面顯示 3.1、實際跑 3.0，兩邊都不報錯。
    #[test]
    fn the_selected_model_is_what_gets_sent() {
        let picked = "qwen-audio-3.1-asr-flash-streaming";
        let payload = run_task_payload(picked, "tid", 16000, &[]);
        assert_eq!(payload["payload"]["model"], picked);

        let config = AsrProviderConfig {
            provider: "qwen_audio_stream".to_string(),
            api_key: "sk-test".to_string(),
            app_id: String::new(),
            extra: serde_json::json!({ "model": picked }),
        };
        assert_eq!(get_model(&config), picked);
    }

    /// extra 裡沒有模型名時回落到 3.0，而不是空串。
    ///
    /// 空串會被服務端當成缺引數、回一條含義模糊的 InvalidParameter，
    /// 看起來像憑據或地域出了問題。
    #[test]
    fn a_missing_or_blank_model_falls_back_to_the_default() {
        let bare = AsrProviderConfig {
            provider: "qwen_audio_stream".to_string(),
            api_key: "sk-test".to_string(),
            app_id: String::new(),
            extra: serde_json::json!({}),
        };
        assert_eq!(get_model(&bare), DEFAULT_MODEL);

        let blank = AsrProviderConfig {
            provider: "qwen_audio_stream".to_string(),
            api_key: "sk-test".to_string(),
            app_id: String::new(),
            extra: serde_json::json!({ "model": "   " }),
        };
        assert_eq!(get_model(&blank), DEFAULT_MODEL);
    }

    #[test]
    fn hotwords_become_weighted_instant_vocabulary() {
        let words = vec!["SayIt".to_string(), " Kiro ".to_string()];
        let payload = run_task_payload(DEFAULT_MODEL, "tid", 16000, &words);
        let vocab = &payload["payload"]["parameters"]["vocabulary"];
        assert_eq!(vocab["SayIt"], HOTWORD_WEIGHT);
        assert_eq!(vocab["Kiro"], HOTWORD_WEIGHT);
    }

    #[test]
    fn hotwords_are_deduped_and_capped() {
        let (_, used, total) = build_vocabulary(&[
            "SayIt".to_string(),
            "sayit".to_string(),
            "".to_string(),
            "  ".to_string(),
        ])
        .unwrap();
        assert_eq!((used, total), (1, 1));

        let many: Vec<String> = (0..HOTWORD_LIMIT + 20).map(|i| format!("w{}", i)).collect();
        let (_, used, total) = build_vocabulary(&many).unwrap();
        assert_eq!(used, HOTWORD_LIMIT);
        assert_eq!(total, HOTWORD_LIMIT + 20);

        assert!(build_vocabulary(&[]).is_none());
        assert!(build_vocabulary(&["   ".to_string()]).is_none());
    }

    fn result_event(text: &str, ended: bool) -> serde_json::Value {
        serde_json::json!({
            "header": { "event": "result-generated" },
            "payload": { "output": { "sentence": { "text": text, "sentence_end": ended } } }
        })
    }

    /// 實測形狀：中間結果是**當前句的全量文本**，不是增量。
    /// 按增量累加過一次就會得到「語音輸入語音輸入法語音輸入法測試」這種疊字。
    #[test]
    fn partials_replace_rather_than_append() {
        let mut t = Transcript::default();
        t.apply(&result_event("语音输入", false));
        t.apply(&result_event("语音输入法测试", false));
        assert_eq!(t.display(), "语音输入法测试");
    }

    /// 開頭那批空文本的中間結果不能產生一次「更新」，否則懸浮窗會先閃一下空字幕。
    #[test]
    fn empty_partials_are_ignored() {
        let mut t = Transcript::default();
        assert!(matches!(t.apply(&result_event("", false)), Applied::Ignored));
        assert_eq!(t.partial_events, 0);
        assert_eq!(t.display(), "");
    }

    /// 分句自帶句尾標點，所以拼接不補分隔符。
    #[test]
    fn finished_sentences_are_joined_without_a_separator() {
        let mut t = Transcript::default();
        t.apply(&result_event("The quick brown fox. ", true));
        t.apply(&result_event("Second one", false));
        assert_eq!(t.display(), "The quick brown fox. Second one");
        t.apply(&result_event("Second one here.", true));
        assert_eq!(t.display(), "The quick brown fox. Second one here.");
        assert_eq!(t.partial, "");
    }

    #[test]
    fn task_finished_and_failed_are_terminal() {
        let mut t = Transcript::default();
        assert!(matches!(
            t.apply(&serde_json::json!({ "header": { "event": "task-finished" } })),
            Applied::Finished
        ));
        assert!(t.finished);

        let mut t = Transcript::default();
        let failed = serde_json::json!({
            "header": { "event": "task-failed", "error_code": "InvalidParameter", "error_message": "bad model" }
        });
        assert!(matches!(t.apply(&failed), Applied::Failed));
        assert!(t.finished);
        assert_eq!(
            t.error.as_deref(),
            Some("InvalidParameter: bad model")
        );
    }

    /// task-started 之類的事件不該被當成文本更新。
    #[test]
    fn unrelated_events_are_ignored() {
        let mut t = Transcript::default();
        assert!(matches!(
            t.apply(&serde_json::json!({ "header": { "event": "task-started" } })),
            Applied::Ignored
        ));
        assert!(!t.finished);
        assert_eq!(t.display(), "");
    }
}
