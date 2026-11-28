// Qwen-Omni-Realtime ASR+AI — 透過 WebSocket 即時介面
// 使用 Manual 模式：傳送音訊 → commit → create_response → 接收文本
// 同時充當 ASR 和 AI，輸出模態設為僅文本

use super::diag;
use super::types::{AsrProviderConfig, AsrResult, TestResult};
use base64::Engine;
use futures_util::{FutureExt, SinkExt, StreamExt};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite;

/// `extra.model` 缺失時用哪個。
///
/// 2026-09-23 從 `qwen3-omni-flash-realtime` 換過來：那個模型已被阿里公告下線並開始
/// 縮容，實測連它的即時端點直接讀超時。回落值指向一個點了就壞的模型，比報錯更難查
/// —— 使用者看到的是「一直轉圈然後什麼都沒有」，而日誌裡只有一個超時。
const DEFAULT_MODEL: &str = "qwen3.5-omni-flash-realtime";

/// `session.audio.output.voice` 發什麼。
///
/// 我們只要文本，這個值永遠不會被合成出來 —— 它存在的唯一原因是 3.8 會在
/// `response.create` 那一步校驗音色。取 Tina 是因為官方文件說它是 3.8 的預設音色，
/// 而且實測 3.5 兩個模型也接受它。詳見 start_session 裡 session.update 那段註釋。
const OUTPUT_VOICE: &str = "Tina";

const SCOPE: &str = "qwen/omni";

// ── 上行限速：先放一段突發，之後按固定速率發 ──
//
// 實測（2026-10-09，dev-scripts/probe_qwen_omni_long_audio.py，真實介面，plus 與 flash 都一樣）：
// DashScope realtime 對上行流量限速，超了直接關連線：
//     close code=1007 reason=Input traffic exceeds the limit of 2560KB/s
// 文件裡沒寫這條。99 秒錄音一口氣發（約 4.3MB、0.4 秒）必掛；但 80 秒以內（≤3.46MB）
// 一口氣發每次都過，按 ≤50 倍即時（2.16MB/s）發 240 秒也過 —— 服務端是帶突發容量的令牌桶。
// 上行慢於約 20Mbps 的機器 TCP 自己就壓住了速度，所以這個問題「有人遇到、有人遇不到」。
//
// 為什麼不純限速：60 秒以內的錄音本來就不會觸發，純限速會讓每一次口述都白等
// （30 秒多 0.75 秒、99 秒多 2.5 秒）。先突發後限速，60 秒以內零延遲，99 秒只多約 1 秒。
// 突發容量沒寫進文件，所以留了餘量；萬一阿里調小了，`transcribe` 裡還有一次純限速重試兜底。
// 改包大小沒用（實測 500ms 一包同樣被掐），能救的只有降速。
// 被否掉的其他方案與以後「邊錄邊發」的計劃見 `.kiro/decisions.md`「千問 Omni 傳送限速」。

/// 突發額度，按線上位元組算。2.4MB ≈ 55 秒音訊，低於每次都能過的 2.6MB（60 秒）。
const PACING_BURST_BYTES: u64 = 2_400_000;
/// 突發之後的速率。約 46 倍即時，比服務端的 2560KB/s 低 20%。
const PACING_RATE_BYTES_PER_SEC: u64 = 2_000_000;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Pacing {
    burst_bytes: u64,
    rate_bytes_per_sec: u64,
}

impl Pacing {
    const NORMAL: Pacing = Pacing {
        burst_bytes: PACING_BURST_BYTES,
        rate_bytes_per_sec: PACING_RATE_BYTES_PER_SEC,
    };
    /// 被限速掐斷之後的那次重試：不再突發，從第一包起就按速率發。
    const THROTTLE_ONLY: Pacing = Pacing {
        burst_bytes: 0,
        rate_bytes_per_sec: PACING_RATE_BYTES_PER_SEC,
    };

    fn mode(&self) -> &'static str {
        if self.burst_bytes == 0 {
            "throttle_retry"
        } else {
            "burst"
        }
    }
}

/// 下一包最早能在「開始傳送之後」的哪個時刻發出。`None` = 還在突發額度內，立刻發。
///
/// 返回的是相對傳送起點的**絕對**偏移，不是「再睡多久」：呼叫方 `sleep_until(起點 + 偏移)`。
/// 這樣醒晚了不會累積 —— Windows 定時器精度約 15.6ms，醒來時後面幾包的截止時刻
/// 往往已經過了，會直接連著發掉，平均速率仍是設定值。
/// ⚠️ 別改成「每包 sleep 一個固定間隔」：46 倍即時下一包只隔 2ms，按 15.6ms 的精度睡，
/// 990 包會被拖到 15 秒。
fn pacing_due(sent_bytes: u64, next_bytes: u64, pacing: Pacing) -> Option<Duration> {
    let needed = sent_bytes + next_bytes;
    if needed <= pacing.burst_bytes {
        return None;
    }
    let over = needed - pacing.burst_bytes;
    Some(Duration::from_secs_f64(
        over as f64 / pacing.rate_bytes_per_sec.max(1) as f64,
    ))
}

/// 這條錯誤是不是「上行超速被掐」。只認服務端的原話和 close 1007，別的錯誤一律不重試 ——
/// 金鑰錯、額度用完、超時重試一遍只會讓使用者多等一輪，結果不會變。
/// 給同一個 DashScope realtime 介面上的其它一次性路徑用（asr_qwen_realtime 的錄完再發）：
/// 限速是介面級的，不分模型，2026-10-27 實測 qwen3-asr-flash-realtime 一口氣發 89 秒
/// 同樣被 `Input traffic exceeds the limit of 2560KB/s` 掐斷。
pub(super) fn realtime_upload_due(sent_bytes: u64, next_bytes: u64) -> Option<Duration> {
    pacing_due(sent_bytes, next_bytes, Pacing::NORMAL)
}

pub(super) fn is_rate_limited(error: &str) -> bool {
    let lower = error.to_lowercase();
    lower.contains("input traffic exceeds") || lower.contains("code=1007")
}

/// 一次嘗試的失敗，帶上「值不值得換純限速再試一次」。
struct AttemptError {
    error: String,
    retryable: bool,
}

impl From<String> for AttemptError {
    /// 絕大多數失敗點都是 `diag::fail(...)?`，在這裡按錯誤原文統一判定。
    /// 判據只看服務端說的原因，所以 close 幀在傳送途中讀到、還是 commit 之後才讀到，
    /// 都走同一條路。
    fn from(error: String) -> Self {
        let retryable = is_rate_limited(&error);
        AttemptError { error, retryable }
    }
}

fn ws_url(model: &str) -> String {
    format!(
        "wss://dashscope.aliyuncs.com/api-ws/v1/realtime?model={}",
        model
    )
}

#[allow(dead_code)]
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

/// 獲取模型 ID（從 extra 欄位或使用預設值）
fn get_model(config: &AsrProviderConfig) -> String {
    config
        .extra
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_MODEL)
        .to_string()
}

/// 獲取 system prompt（從 extra 欄位）
fn get_instructions(config: &AsrProviderConfig) -> String {
    config
        .extra
        .get("instructions")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("你是一個語音轉文字助手。請將使用者的語音內容準確轉寫為文字，保持原意，適當加上標點符號，不要加上任何額外的解釋或評論。中文一律以台灣正體中文與台灣用語輸出。")
        .to_string()
}

/// 一次 Omni 會話裡累積下來的東西。
#[derive(Default)]
struct OmniTranscript {
    /// 模型的回答（response.text.* / response.audio_transcript.*）
    result_text: String,
    /// 服務端對**輸入音訊**的轉寫。模型沒給回答時用它兜底
    input_transcript: String,
}

impl OmniTranscript {
    fn is_empty(&self) -> bool {
        self.result_text.trim().is_empty() && self.input_transcript.trim().is_empty()
    }

    /// 最終文本 + 是否走了「輸入轉寫」兜底
    fn finish(self) -> (String, bool) {
        if self.result_text.trim().is_empty() && !self.input_transcript.is_empty() {
            (self.input_transcript, true)
        } else {
            (self.result_text, false)
        }
    }
}

/// 處理一條服務端事件之後該怎麼走。
#[derive(Debug, PartialEq)]
enum OmniStep {
    Continue,
    /// 收到 response.done，本輪正常結束
    Completed,
    /// 服務端自己報了錯
    Failed { code: String, message: String },
}

/// 收響應的過程是怎麼結束的。
///
/// 這個區分是本次修復的核心：空文本到底算「使用者真沒說話」還是「協議沒跑完」，
/// 全看是不是收到過 `response.done`。原來三條退出路徑（流結束 / 收到 Close 幀 /
/// response.done）一律返回 `Ok("")`，於是**任何**協議層失敗都顯示成「未檢測到有效聲音」，
/// 把使用者引去查麥克風。使用者報的「長錄音轉錄失敗提示無有效聲音」就是這個形態。
enum OmniEnd {
    Completed,
    /// 服務端關了連線，帶上 close 幀裡的說明
    Closed(String),
    /// 流直接沒了，連 close 幀都沒有
    StreamEnded,
}

impl OmniEnd {
    /// 一個字都沒拿到時：`None` = 可以當成一次空轉寫（使用者真沒說話）；
    /// `Some(stage)` = 必須報錯，stage 是寫進日誌和錯誤信封的階段名。
    ///
    /// 判據只有一條：**收到過 response.done 嗎**。沒收到就說明這輪對話沒跑完，
    /// 空文本不是結論、是症狀。
    fn empty_failure_stage(&self) -> Option<&'static str> {
        match self {
            OmniEnd::Completed => None,
            OmniEnd::Closed(_) => Some("closed_before_response"),
            OmniEnd::StreamEnded => Some("stream_ended_before_response"),
        }
    }
}

/// 把一條服務端事件並進累積結果。
///
/// 抽成獨立函式是因為它要在**兩處**跑：傳送音訊期間的邊發邊讀，和發完之後的主迴圈。
/// 順帶也就能測了 —— 這個 provider 之前一條測試都沒有。
fn apply_omni_event(event: &serde_json::Value, acc: &mut OmniTranscript) -> OmniStep {
    let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match event_type {
        "response.text.delta" | "response.audio_transcript.delta" => {
            if let Some(delta) = event.get("delta").and_then(|d| d.as_str()) {
                acc.result_text.push_str(delta);
            }
        }
        "conversation.item.input_audio_transcription.completed" => {
            if let Some(text) = event.get("transcript").and_then(|t| t.as_str()) {
                acc.input_transcript = text.to_string();
            }
        }
        "response.text.done" | "response.audio_transcript.done" => {
            // done 帶的是全量，優先用它 —— 比把 delta 拼起來更不容易缺字
            for key in ["text", "transcript"] {
                if let Some(text) = event.get(key).and_then(|t| t.as_str()) {
                    if !text.is_empty() {
                        acc.result_text = text.to_string();
                    }
                }
            }
        }
        "response.done" => return OmniStep::Completed,
        "error" => {
            let err = event.get("error");
            let field = |name: &str, fallback: &str| {
                err.and_then(|e| e.get(name))
                    .and_then(|v| v.as_str())
                    .unwrap_or(fallback)
                    .to_string()
            };
            return OmniStep::Failed {
                code: field("code", "-"),
                message: field("message", "Unknown error"),
            };
        }
        _ => {}
    }
    OmniStep::Continue
}

fn close_frame_reason(frame: Option<tungstenite::protocol::CloseFrame>) -> String {
    frame
        .map(|f| format!("code={} reason={}", f.code, diag::truncate(&f.reason, 200)))
        .unwrap_or_else(|| "no close frame details".to_string())
}

/// 收尾：把累積結果變成這次呼叫的返回值。
///
/// 單獨抽出來有兩個原因：① 早退路徑和正常路徑必須用同一套判斷，不能各寫一遍；
/// ② 這樣才測得到 —— 本次修復的行為差別全在這個函式裡（空結果到底算失敗還是算
/// 「使用者真沒說話」）。
fn finish_omni(
    acc: OmniTranscript,
    end: OmniEnd,
    elapsed_ms: u64,
    context: &str,
) -> Result<AsrResult, String> {
    if acc.is_empty() {
        match end.empty_failure_stage() {
            None => diag::empty_result(
                SCOPE,
                &format!("Conversation completed without any transcript {}", context),
            ),
            Some(stage) => {
                let detail = match &end {
                    OmniEnd::Closed(reason) => format!(
                        "Server closed the connection before returning any transcript ({})",
                        reason
                    ),
                    _ => "The connection ended before returning any transcript (no close frame)"
                        .to_string(),
                };
                return Err(diag::fail(SCOPE, stage, format!("{} {}", detail, context)));
            }
        }
    }

    let (final_text, used_input_transcript) = acc.finish();
    if !final_text.trim().is_empty() {
        diag::ok(SCOPE, elapsed_ms, final_text.chars().count());
        if used_input_transcript {
            // 模型沒給回答、只給了輸入轉寫。結果可用，但說明 instructions 可能沒生效
            diag::log(
                SCOPE,
                "used_input_transcript",
                "Model produced no output; used the input transcript fallback",
            );
        }
    }

    Ok(AsrResult {
        text: final_text,
        elapsed_ms,
    })
}

pub async fn transcribe(
    audio_pcm_b64: &str,
    _sample_rate: u32,
    config: &AsrProviderConfig,
    hotwords: &[String],
) -> Result<AsrResult, String> {
    let pcm = base64::engine::general_purpose::STANDARD
        .decode(audio_pcm_b64)
        .map_err(|e| diag::fail(SCOPE, "decode_b64", format!("Failed to decode base64 audio: {}", e)))?;

    if pcm.is_empty() {
        diag::empty_result(SCOPE, "Input audio was empty; provider request was skipped");
        return Ok(AsrResult {
            text: String::new(),
            elapsed_ms: 0,
        });
    }

    let model = get_model(config);
    let mut instructions = get_instructions(config);
    // 熱詞上下文偏置：追加到 system instructions
    if let Some(ctx) = super::asr_qwen::build_hotword_context_text(hotwords) {
        instructions.push_str("\n\n請特別注意以下專業術語/詞彙的識別：");
        instructions.push_str(&ctx);
    }
    let audio_sec = pcm.len() as f64 / 32000.0; // 16kHz / 16bit / mono
    diag::log(
        SCOPE,
        "start",
        &format!(
            "pcm_bytes={} audio_sec={:.1} model={} hotwords={}",
            pcm.len(),
            audio_sec,
            model,
            hotwords.len()
        ),
    );

    let start = Instant::now();
    let session = OmniSession {
        api_key: &config.api_key,
        model: &model,
        instructions: &instructions,
        pcm: &pcm,
        audio_sec,
        start,
    };

    match run_attempt(&session, Pacing::NORMAL).await {
        Ok(result) => Ok(result),
        // 只重試一次，而且只為「上行超速」這一種原因。重試不再突發，99 秒錄音約多等 2 秒 ——
        // 比讓使用者整段重錄強得多。重試也失敗就如實報出來，不再兜圈子。
        Err(first) if first.retryable => {
            diag::log(
                SCOPE,
                "rate_limited_retry",
                &format!(
                    "retrying once without burst: {}",
                    diag::truncate(&first.error, 200)
                ),
            );
            run_attempt(&session, Pacing::THROTTLE_ONLY)
                .await
                .map_err(|again| again.error)
        }
        Err(first) => Err(first.error),
    }
}

/// 一次轉寫裡各次嘗試共用的東西。
struct OmniSession<'a> {
    api_key: &'a str,
    model: &'a str,
    instructions: &'a str,
    pcm: &'a [u8],
    audio_sec: f64,
    /// 第一次嘗試開始的時刻。重試時不重置，返回的 elapsed 是使用者實際等的總時長。
    start: Instant,
}

/// 建一次連線、發完音訊、收完結果。
async fn run_attempt(session: &OmniSession<'_>, pacing: Pacing) -> Result<AsrResult, AttemptError> {
    let OmniSession {
        api_key,
        model,
        instructions,
        pcm,
        audio_sec,
        start,
    } = *session;
    let url = ws_url(model);

    // 構建 WebSocket 請求。每次嘗試都要新建：http::Request 不能 clone。
    let request = tungstenite::http::Request::builder()
        .uri(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Sec-WebSocket-Key", tungstenite::handshake::client::generate_key())
        .header("Sec-WebSocket-Version", "13")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Host", "dashscope.aliyuncs.com")
        .body(())
        .map_err(|e| diag::fail(SCOPE, "build_request", format!("Failed to build request: {}", e)))?;

    let (mut ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| diag::fail(SCOPE, "connect", format!("WebSocket connection failed: {}", e)))?;

    // 等待 session.created
    wait_for_event(&mut ws, "session.created", SCOPE).await?;

    // 傳送 session.update — 僅輸出文本，停用 VAD（Manual 模式）
    //
    // ⚠️ `audio.output.voice` 看著是多餘的：我們 `modalities` 只要 text，壓根不要
    // 合成語音。**但它是 qwen3.8-omni-flash-realtime 能用的前提，別當冗餘清掉。**
    //
    // 實測（2026-09-23，dev-scripts/probe_qwen38_omni_voice.py）：不帶這個欄位時，
    // 3.8 在 `response.create` 那一步回 `<400> InternalError.Algo.InvalidParameter:
    // Voice 'Chelsie' is not supported.` —— 服務端拿了一個 3.5 時代的預設音色去校驗，
    // 即使這一輪不會產出音訊。3.8 把預設音色換成了 Tina，並把欄位挪到
    // `session.audio.output.voice`（官方文件稱它優先於相容欄位 `session.voice`）。
    //
    // 為什麼不按模型分支：實測 3.5 plus/flash 帶上這個欄位同樣正常，所以**一份會話
    // 形狀通吃三代**。按模型拆兩套 session 會多出一條只在某一代上跑過的程式碼路徑。
    //
    // 排查提示：錯誤資訊只提音色、不提模型，容易被當成「賬號沒開通音色」或
    // 「模型不可用」。真正的判據是 `response.create` 之後立刻來一個 error 事件。
    let session_update = serde_json::json!({
        "type": "session.update",
        "session": {
            "modalities": ["text"],
            "instructions": instructions,
            "input_audio_format": "pcm",
            "turn_detection": null,
            "audio": { "output": { "voice": OUTPUT_VOICE } }
        }
    });
    ws.send(tungstenite::Message::Text(session_update.to_string().into()))
        .await
        .map_err(|e| {
            diag::fail(SCOPE, "send_session_update", format!("Failed to send session.update: {}", e))
        })?;

    // 等待 session.updated
    wait_for_event(&mut ws, "session.updated", SCOPE).await?;

    // 傳送音訊資料（PCM 16kHz 16bit mono，分塊傳送）
    // Qwen Omni 接受原始 PCM，不需要 WAV 頭
    // 但如果取樣率不是 16kHz，需要注意
    let chunk_size = 3200; // 100ms @ 16kHz 16bit mono
    let total_chunks = pcm.len().div_ceil(chunk_size);
    let mut acc = OmniTranscript::default();
    // 傳送途中就收到了 response.done。按協議不該發生（turn_detection=null 時只有
    // response.create 能觸發響應），但真發生了必須停下來 —— 繼續發完再 commit 會開第二輪，
    // 而主接收迴圈等不到新的 response.done，只能掛滿 60 秒超時。
    let mut completed_while_sending = false;
    // 限速記賬。按線上位元組（序列化後的 JSON 文本長度）算，不按 PCM 位元組：服務端量的是
    // 收到的流量，base64 + JSON 讓每包從 3200 位元組漲到約 4.33KB。WS 幀頭每包十幾位元組，忽略。
    let send_start = Instant::now();
    let mut wire_sent: u64 = 0;
    let mut waits: u32 = 0;
    let mut waited = Duration::ZERO;
    'sending: for (idx, chunk) in pcm.chunks(chunk_size).enumerate() {
        let audio_b64 = base64::engine::general_purpose::STANDARD.encode(chunk);
        let append_event = serde_json::json!({
            "type": "input_audio_buffer.append",
            "audio": audio_b64
        });
        let frame = append_event.to_string();
        let frame_len = frame.len() as u64;
        if let Some(due) = pacing_due(wire_sent, frame_len, pacing) {
            let deadline = send_start + due;
            let now = Instant::now();
            if deadline > now {
                waits += 1;
                waited += deadline - now;
                tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
            }
        }
        ws.send(tungstenite::Message::Text(frame.into()))
            .await
            // 帶上第幾包：長音訊中途被切斷和第一包就發不出去，成因完全不同
            .map_err(|e| AttemptError {
                error: diag::fail(
                    SCOPE,
                    "send_audio",
                    format!(
                        "Failed to send audio chunk {}/{} (wire_bytes={}): {}",
                        idx + 1,
                        total_chunks,
                        wire_sent,
                        e
                    ),
                ),
                // TCP 先被重置時讀不到 close 幀，原因不會寫在錯誤裡。已經發過了突發額度
                // 還斷，最可能就是同一件事 —— 值得用純限速再試一次。
                retryable: wire_sent > pacing.burst_bytes,
            })?;
        wire_sent += frame_len;

        // ── 邊發邊讀。**不能只發不收。** ──
        //
        // 這份實現原來是「把全部音訊發完，再開始讀 socket」，而倉庫裡另外五個流式
        // provider（豆包 / Gemini Live / OpenAI realtime / 千問 audio3x / 千問 realtime）
        // 都是 split 之後併發讀的。只發不讀有兩個後果：
        //   ① tungstenite 只在**讀到** Ping 時才排一個 Pong，一路不讀就等於不迴心跳；
        //   ② 服務端在傳送期間報的 error 事件我們全錯過。隨後連線被關、下一次 send
        //      失敗，日誌裡只剩一句「第 N 包發不出去」，服務端給的真實原因丟了。
        // 5 分鐘錄音是 3000 條 append、約 13MB JSON，這段時間不短。
        //
        // now_or_never 只輪詢一次、拿不到就走，不拖慢傳送節奏。
        loop {
            let polled = ws.next().now_or_never().flatten();
            let Some(message) = polled else { break };
            match message {
                Ok(tungstenite::Message::Text(text)) => {
                    let Ok(event) = serde_json::from_str::<serde_json::Value>(&text) else {
                        continue;
                    };
                    match apply_omni_event(&event, &mut acc) {
                        OmniStep::Continue => {}
                        OmniStep::Completed => {
                            diag::log(
                                SCOPE,
                                "completed_while_sending",
                                &format!("chunks_sent={}/{}", idx + 1, total_chunks),
                            );
                            completed_while_sending = true;
                            break 'sending;
                        }
                        OmniStep::Failed { code, message } => {
                            let _ = ws.close(None).await;
                            return Err(diag::fail(
                                SCOPE,
                                "server_error_while_sending",
                                format!(
                                    "Qwen Omni error [{}] while sending chunk {}/{} (audio={:.1}s): {}",
                                    code, idx + 1, total_chunks, audio_sec, message
                                ),
                            )
                            .into());
                        }
                    }
                }
                Ok(tungstenite::Message::Close(frame)) => {
                    return Err(diag::fail(
                        SCOPE,
                        "closed_while_sending",
                        format!(
                            "Server closed the connection while sending chunk {}/{} (audio={:.1}s): {}",
                            idx + 1, total_chunks, audio_sec, close_frame_reason(frame)
                        ),
                    )
                    .into());
                }
                Ok(_) => {}
                Err(e) => {
                    return Err(diag::fail(
                        SCOPE,
                        "recv_while_sending",
                        format!(
                            "Read failed while sending chunk {}/{} (audio={:.1}s): {}",
                            idx + 1, total_chunks, audio_sec, e
                        ),
                    )
                    .into());
                }
            }
        }
    }

    // 這一行是「限速真的生效了」的證據：排查時先看它在不在、mode 是哪個、waits 是不是 >0。
    // 沒有這一行就說明跑的不是這份程式碼，別再去懷疑速率或額度。
    diag::log(
        SCOPE,
        "pacing",
        &format!(
            "mode={} wire_bytes={} chunks={} burst={} rate={} waits={} waited_ms={} send_ms={}",
            pacing.mode(),
            wire_sent,
            total_chunks,
            pacing.burst_bytes,
            pacing.rate_bytes_per_sec,
            waits,
            waited.as_millis(),
            send_start.elapsed().as_millis()
        ),
    );

    // 傳送途中這一輪就結束了：手裡已經有結果，絕不能再 commit（那會開第二輪）。
    if completed_while_sending {
        let _ = ws.close(None).await;
        let elapsed_ms = start.elapsed().as_millis() as u64;
        return Ok(finish_omni(
            acc,
            OmniEnd::Completed,
            elapsed_ms,
            &format!(
                "audio_sec={:.1} elapsed={}ms model={} chunks={} ended_while_sending=true",
                audio_sec, elapsed_ms, model, total_chunks
            ),
        )?);
    }

    // 提交音訊並請求響應
    let commit = serde_json::json!({ "type": "input_audio_buffer.commit" });
    ws.send(tungstenite::Message::Text(commit.to_string().into()))
        .await
        .map_err(|e| diag::fail(SCOPE, "send_commit", format!("Failed to send commit: {}", e)))?;

    let create_response = serde_json::json!({ "type": "response.create" });
    ws.send(tungstenite::Message::Text(create_response.to_string().into()))
        .await
        .map_err(|e| {
            diag::fail(SCOPE, "send_response_create", format!("Failed to send response.create: {}", e))
        })?;

    diag::log(SCOPE, "audio_sent", &format!("chunks={}", total_chunks));

    // 收集響應文本。`acc` 在傳送階段就已經開始收了（見上面「邊發邊讀」）。
    let timeout = tokio::time::Duration::from_secs(60);
    let end;

    loop {
        let msg = match tokio::time::timeout(timeout, ws.next()).await {
            Err(_) => {
                let _ = ws.close(None).await;
                return Err(diag::fail(
                    SCOPE,
                    "recv_timeout",
                    format!(
                        "Timed out waiting for a response (60s); audio={:.1}s received_chars={}",
                        audio_sec,
                        acc.result_text.chars().count()
                    ),
                )
                .into());
            }
            Ok(None) => {
                end = OmniEnd::StreamEnded;
                break;
            }
            Ok(Some(Err(e))) => {
                // 連線錯誤（包括對方關閉後仍發訊息）。已經拿到文本就當成功收尾。
                if !acc.is_empty() {
                    diag::log(
                        SCOPE,
                        "recv_error_after_result",
                        &format!("A result was already received; treating as success: {}", diag::truncate(&e.to_string(), 200)),
                    );
                    end = OmniEnd::Completed;
                    break;
                }
                return Err(diag::fail(SCOPE, "recv", format!("Failed to receive message: {}", e)).into());
            }
            Ok(Some(Ok(m))) => m,
        };

        match msg {
            tungstenite::Message::Text(text) => {
                let event: serde_json::Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                match apply_omni_event(&event, &mut acc) {
                    OmniStep::Continue => {}
                    OmniStep::Completed => {
                        end = OmniEnd::Completed;
                        break;
                    }
                    OmniStep::Failed { code, message } => {
                        let _ = ws.close(None).await;
                        return Err(diag::fail(
                            SCOPE,
                            "server_error",
                            format!("Qwen Omni error [{}]: {}", code, message),
                        )
                        .into());
                    }
                }
            }
            tungstenite::Message::Close(frame) => {
                let reason = close_frame_reason(frame);
                diag::log(SCOPE, "close", &reason);
                end = OmniEnd::Closed(reason);
                break;
            }
            _ => {}
        }
    }

    let _ = ws.close(None).await;
    let elapsed_ms = start.elapsed().as_millis() as u64;
    let context = format!(
        "audio_sec={:.1} elapsed={}ms model={} chunks={}",
        audio_sec, elapsed_ms, model, total_chunks
    );

    // 空結果算失敗還是算「真沒說話」，判斷全在 finish_omni 裡（那裡有長註釋）。
    //
    // 長音訊被掐斷的原因已用真實介面查清：上行超速（見檔案頭「上行限速」）。
    // 限速沒壓住時 close 幀可能在 commit 之後才讀到，於是從這裡以 closed_before_response
    // 報出來 —— 錯誤原文裡帶著 1007 的 reason，`AttemptError::from` 會把它判成可重試。
    Ok(finish_omni(acc, end, elapsed_ms, &context)?)
}

/// 等待指定型別的事件
async fn wait_for_event(
    ws: &mut (impl StreamExt<Item = Result<tungstenite::Message, tungstenite::Error>> + Unpin),
    expected_type: &str,
    scope: &str,
) -> Result<serde_json::Value, String> {
    let timeout = tokio::time::Duration::from_secs(10);
    let stage = format!("wait:{}", expected_type);
    loop {
        let msg = match tokio::time::timeout(timeout, ws.next()).await {
            Err(_) => {
                return Err(diag::fail(
                    scope,
                    &stage,
                    format!("Timed out waiting for {}", expected_type),
                ))
            }
            Ok(None) => {
                return Err(diag::fail(
                    scope,
                    &stage,
                    format!("Connection closed while waiting for {}", expected_type),
                ))
            }
            Ok(Some(Err(e))) => {
                return Err(diag::fail(scope, &stage, format!("Failed to receive message: {}", e)))
            }
            Ok(Some(Ok(m))) => m,
        };

        match msg {
            tungstenite::Message::Text(text) => {
                let event: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
                    diag::fail(scope, &stage, format!("Failed to parse event: {}", e))
                })?;

                let event_type = event
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("");

                if event_type == "error" {
                    let err_msg = event
                        .get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("Unknown error");
                    let err_code = event
                        .get("error")
                        .and_then(|e| e.get("code"))
                        .and_then(|c| c.as_str())
                        .unwrap_or("-");
                    return Err(diag::fail(
                        scope,
                        &stage,
                        format!("Qwen Omni error [{}]: {}", err_code, err_msg),
                    ));
                }

                if event_type == expected_type {
                    return Ok(event);
                }
            }
            tungstenite::Message::Close(frame) => {
                let reason = frame
                    .map(|f| format!("code={}, reason={}", f.code, diag::truncate(&f.reason, 200)))
                    .unwrap_or_else(|| "No details".to_string());
                return Err(diag::fail(
                    scope,
                    &stage,
                    format!("Server closed the connection while waiting for {} ({})", expected_type, reason),
                ));
            }
            _ => {}
        }
    }
}

pub async fn test_connection(config: &AsrProviderConfig) -> TestResult {
    let model = get_model(config);
    let url = ws_url(&model);

    let request = tungstenite::http::Request::builder()
        .uri(&url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .header("Sec-WebSocket-Key", tungstenite::handshake::client::generate_key())
        .header("Sec-WebSocket-Version", "13")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Host", "dashscope.aliyuncs.com")
        .body(())
        .unwrap();

    let start = Instant::now();

    match tokio_tungstenite::connect_async(request).await {
        Ok((mut ws, _)) => {
            // 嘗試等待 session.created
            let result = wait_for_event(&mut ws, "session.created", "qwen/omni-test").await;
            let _ = ws.close(None).await;
            let elapsed_ms = start.elapsed().as_millis() as u64;
            match result {
                Ok(_) => TestResult {
                    ok: true,
                    message: format!("Connection successful, model: {} ({}ms)", model, elapsed_ms),
                    elapsed_ms,
                    detail: String::new(),
                },
                Err(e) => TestResult {
                    ok: false,
                    // wait_for_event already returns a stable sayit_error envelope. Do not
                    // bury it inside another sentence or the frontend can no longer decode it.
                    message: e,
                    elapsed_ms,
                    detail: format!("Protocol handshake failed after {}ms", elapsed_ms),
                },
            }
        }
        Err(e) => {
            let elapsed_ms = start.elapsed().as_millis() as u64;
            TestResult {
                ok: false,
                message: diag::fail("qwen/omni-test", "connect", format!("Connection failed: {}", e)),
                elapsed_ms,
                detail: String::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{apply_omni_event, OmniStep, OmniTranscript};
    use serde_json::json;

    fn feed(events: &[serde_json::Value]) -> (OmniTranscript, Vec<OmniStep>) {
        let mut acc = OmniTranscript::default();
        let steps = events
            .iter()
            .map(|event| apply_omni_event(event, &mut acc))
            .collect();
        (acc, steps)
    }

    #[test]
    fn text_deltas_are_appended_and_done_overrides_with_the_full_text() {
        let (acc, steps) = feed(&[
            json!({ "type": "response.text.delta", "delta": "語音" }),
            json!({ "type": "response.text.delta", "delta": "輸入" }),
            json!({ "type": "response.text.done", "text": "語音輸入法測試成功。" }),
            json!({ "type": "response.done" }),
        ]);
        // done 帶全量，優先用它 —— 拼 delta 更容易缺字
        assert_eq!(acc.result_text, "語音輸入法測試成功。");
        assert_eq!(steps.last(), Some(&OmniStep::Completed));
    }

    #[test]
    fn audio_transcript_events_feed_the_same_accumulator() {
        let (acc, _) = feed(&[
            json!({ "type": "response.audio_transcript.delta", "delta": "abc" }),
            json!({ "type": "response.audio_transcript.done", "transcript": "abcdef" }),
        ]);
        assert_eq!(acc.result_text, "abcdef");
    }

    /// 空 done 不能把已經攢好的 delta 清掉。
    #[test]
    fn an_empty_done_payload_keeps_the_accumulated_deltas() {
        let (acc, _) = feed(&[
            json!({ "type": "response.text.delta", "delta": "有內容" }),
            json!({ "type": "response.text.done", "text": "" }),
        ]);
        assert_eq!(acc.result_text, "有內容");
    }

    /// 模型沒回答、只有服務端對輸入音訊的轉寫時走兜底。
    #[test]
    fn falls_back_to_the_input_transcription_when_the_model_says_nothing() {
        let (acc, _) = feed(&[json!({
            "type": "conversation.item.input_audio_transcription.completed",
            "transcript": "只有輸入轉寫",
        })]);
        assert!(!acc.is_empty());
        let (text, used_fallback) = acc.finish();
        assert_eq!(text, "只有輸入轉寫");
        assert!(used_fallback);
    }

    #[test]
    fn the_model_answer_wins_over_the_input_transcription() {
        let (acc, _) = feed(&[
            json!({ "type": "conversation.item.input_audio_transcription.completed", "transcript": "輸入" }),
            json!({ "type": "response.text.delta", "delta": "回答" }),
        ]);
        let (text, used_fallback) = acc.finish();
        assert_eq!(text, "回答");
        assert!(!used_fallback);
    }

    #[test]
    fn server_errors_carry_code_and_message_through() {
        let (_, steps) = feed(&[json!({
            "type": "error",
            "error": { "code": "quota_exceeded", "message": "insufficient balance" },
        })]);
        assert_eq!(
            steps[0],
            OmniStep::Failed {
                code: "quota_exceeded".to_string(),
                message: "insufficient balance".to_string(),
            }
        );
    }

    /// 報錯事件缺欄位時也不能 panic —— 報錯路徑本來就是最沒人測的那條。
    #[test]
    fn a_bare_error_event_still_produces_a_failure() {
        let (_, steps) = feed(&[json!({ "type": "error" })]);
        assert_eq!(
            steps[0],
            OmniStep::Failed { code: "-".to_string(), message: "Unknown error".to_string() },
        );
    }

    #[test]
    fn unknown_events_are_ignored_and_leave_the_result_empty() {
        let (acc, steps) = feed(&[
            json!({ "type": "session.updated" }),
            json!({ "type": "input_audio_buffer.committed" }),
            json!({ "type": "rate_limits.updated" }),
        ]);
        assert!(acc.is_empty());
        assert!(steps.iter().all(|s| *s == OmniStep::Continue));
    }

    /// 只有空白字元不算有內容 —— 否則「協議沒跑完」會被當成一次成功的空轉寫，
    /// 又回到「顯示未檢測到有效聲音」那個坑裡。
    #[test]
    fn whitespace_only_text_counts_as_empty() {
        let (acc, _) = feed(&[json!({ "type": "response.text.done", "text": "   \n  " })]);
        assert!(acc.is_empty());
    }
}

#[cfg(test)]
mod empty_ending_tests {
    use super::{finish_omni, OmniEnd, OmniTranscript};

    fn transcript(result: &str) -> OmniTranscript {
        OmniTranscript {
            result_text: result.to_string(),
            input_transcript: String::new(),
        }
    }

    /// 空文本 + 服務端掐斷 ⇒ 必須是 Err。
    ///
    /// 這條是本次修復的核心斷言。改回 `Ok("")` 它就紅 —— 而 `Ok("")` 正是使用者看到
    /// 「長錄音失敗提示無有效聲音」的原因（前端拿到空文本只會顯示那一句）。
    #[test]
    fn an_aborted_conversation_with_no_text_is_an_error() {
        let err = finish_omni(
            OmniTranscript::default(),
            OmniEnd::Closed("code=1011 reason=internal".to_string()),
            1234,
            "audio_sec=180.0",
        )
        .expect_err("協議沒跑完又一個字都沒拿到，必須報錯");
        assert!(err.contains("code=1011"), "錯誤資訊要帶上 close code，否則等於沒說原因：{err}");
    }

    #[test]
    fn a_stream_that_just_ends_with_no_text_is_also_an_error() {
        assert!(finish_omni(OmniTranscript::default(), OmniEnd::StreamEnded, 1, "ctx").is_err());
    }

    /// 對話正常走完但確實沒有文本 —— 這才是「使用者沒說話」，要返回空文本讓前端提示。
    #[test]
    fn a_completed_conversation_with_no_text_stays_a_successful_empty_result() {
        let result = finish_omni(OmniTranscript::default(), OmniEnd::Completed, 1, "ctx")
            .expect("正常走完的空結果不是錯誤");
        assert_eq!(result.text, "");
    }

    /// 已經拿到文本時，連線怎麼斷的都不影響 —— 保留原來的寬容。
    #[test]
    fn text_already_received_survives_an_abrupt_close() {
        let result = finish_omni(
            transcript("已經識別出來的內容"),
            OmniEnd::Closed("code=1006".to_string()),
            1,
            "ctx",
        )
        .expect("有文本就算成功");
        assert_eq!(result.text, "已經識別出來的內容");
    }

    /// 這三條就是本次修復的全部行為差別。
    ///
    /// 使用者報的是「千問 Omni 長錄音轉錄失敗，提示無有效聲音」。原來三條退出路徑
    /// 一律返回 `Ok("")`，前端只能顯示「未檢測到有效聲音」，把人引去查麥克風 ——
    /// 真實原因（服務端掐斷連線）在介面上完全看不到。
    ///
    /// ⚠️ 若把這三條改成同一個結果，就等於把那個 bug 裝回去了。
    #[test]
    fn only_a_completed_conversation_may_report_an_empty_transcript() {
        assert_eq!(
            OmniEnd::Completed.empty_failure_stage(),
            None,
            "收到過 response.done，空文本才可以解釋成「使用者沒說話」",
        );
        assert_eq!(
            OmniEnd::Closed("code=1011 reason=internal".to_string()).empty_failure_stage(),
            Some("closed_before_response"),
            "服務端掐斷連線必須報錯，而不是顯示「未檢測到有效聲音」",
        );
        assert_eq!(
            OmniEnd::StreamEnded.empty_failure_stage(),
            Some("stream_ended_before_response"),
            "連 close 幀都沒有同樣是協議沒跑完",
        );
    }
}

#[cfg(test)]
mod pacing_tests {
    use super::*;

    /// 一包 100ms 音訊線上上的真實大小（base64 + JSON），和傳送迴圈裡算的是同一個東西。
    fn wire_len_of_one_chunk() -> u64 {
        let audio_b64 = base64::engine::general_purpose::STANDARD.encode([0u8; 3200]);
        serde_json::json!({ "type": "input_audio_buffer.append", "audio": audio_b64 })
            .to_string()
            .len() as u64
    }

    /// 按傳送迴圈的演算法走一遍，返回最後一包的截止時刻（不真睡）。
    fn last_due(seconds: u64, pacing: Pacing) -> Option<Duration> {
        let frame = wire_len_of_one_chunk();
        let mut sent = 0;
        let mut last = None;
        for _ in 0..seconds * 10 {
            last = pacing_due(sent, frame, pacing);
            sent += frame;
        }
        last
    }

    #[test]
    fn nothing_waits_inside_the_burst_budget() {
        assert_eq!(pacing_due(0, 4_000, Pacing::NORMAL), None);
        assert_eq!(pacing_due(PACING_BURST_BYTES - 4_000, 4_000, Pacing::NORMAL), None);
    }

    #[test]
    fn after_the_burst_the_deadline_follows_the_rate() {
        // 超出突發額度整整 2MB = 按 2MB/s 要在第 1 秒發出
        assert_eq!(
            pacing_due(PACING_BURST_BYTES, PACING_RATE_BYTES_PER_SEC, Pacing::NORMAL),
            Some(Duration::from_secs(1))
        );
    }

    /// 截止時刻是相對傳送起點的絕對偏移：只取決於已發位元組，跟上一次醒得早還是晚無關。
    /// 這是「醒晚了不累積」的前提 —— 改成返回增量間隔，這條就紅。
    #[test]
    fn deadlines_are_absolute_offsets_that_grow_linearly() {
        let rate = PACING_RATE_BYTES_PER_SEC;
        let at = |sent: u64| pacing_due(PACING_BURST_BYTES + sent, rate, Pacing::NORMAL).unwrap();
        assert_eq!(at(0), Duration::from_secs(1));
        assert_eq!(at(rate), Duration::from_secs(2));
        assert_eq!(at(3 * rate), Duration::from_secs(4));
    }

    #[test]
    fn the_retry_paces_from_the_first_byte() {
        assert_eq!(
            pacing_due(0, PACING_RATE_BYTES_PER_SEC, Pacing::THROTTLE_ONLY),
            Some(Duration::from_secs(1))
        );
        assert_eq!(Pacing::THROTTLE_ONLY.mode(), "throttle_retry");
        assert_eq!(Pacing::NORMAL.mode(), "burst");
    }

    /// 選這組引數的理由：55 秒以內一點不等；使用者報的 99 秒只多 1 秒左右。
    #[test]
    fn short_recordings_never_wait_and_99_seconds_waits_about_one_second() {
        assert_eq!(last_due(55, Pacing::NORMAL), None);
        let due = last_due(99, Pacing::NORMAL).expect("99 秒超出了突發額度");
        assert!(
            due > Duration::from_millis(800) && due < Duration::from_millis(1100),
            "99 秒錄音的傳送應在 1 秒左右結束，實際 {:?}",
            due
        );
    }

    /// 平均速率必須低於服務端的 2560KB/s，否則限速等於沒加。
    #[test]
    fn the_paced_rate_stays_under_the_server_limit() {
        const SERVER_LIMIT: u64 = 2560 * 1024;
        assert!(PACING_RATE_BYTES_PER_SEC * 10 <= SERVER_LIMIT * 8, "至少留 20% 餘量");
        // 突發額度要低於實測每次都能過的 2.6MB
        assert!(PACING_BURST_BYTES < 2_600_000);
    }

    #[test]
    fn only_the_rate_limit_close_is_retried() {
        let real = "Server closed the connection while sending chunk 936/990 (audio=99.0s): \
                    code=1007 reason=Input traffic exceeds the limit of 2560KB/s";
        assert!(is_rate_limited(real));
        assert!(is_rate_limited("reason=INPUT TRAFFIC EXCEEDS the limit"));
        assert!(is_rate_limited(
            "Server closed the connection before returning any transcript (code=1007 reason=x)"
        ));

        assert!(!is_rate_limited("Qwen Omni error [InvalidApiKey]: Invalid API-key provided."));
        assert!(!is_rate_limited("Qwen Omni error [quota_exceeded]: insufficient balance"));
        assert!(!is_rate_limited("Timed out waiting for a response (60s)"));
        assert!(!is_rate_limited("code=1011 reason=internal"));
    }

    #[test]
    fn attempt_errors_built_from_messages_use_the_classifier() {
        let retry: AttemptError = "code=1007 reason=Input traffic exceeds".to_string().into();
        assert!(retry.retryable);
        let stop: AttemptError = "code=1011 reason=internal".to_string().into();
        assert!(!stop.retryable);
    }

    /// 真實介面驗證，預設不跑（要金鑰、要花錢）。
    ///
    ///     $env:SAYIT_QWEN_KEY=...   $env:SAYIT_LONG_WAV=<16k mono s16 wav>
    ///     $env:SAYIT_LONG_SECONDS=99   （可選）$env:SAYIT_QWEN_MODEL=...
    ///     cargo test --bins live_long_audio -- --ignored --nocapture
    ///
    /// 判據不止「成功」：還要去 sayit.log 看這次的 `pacing mode=burst ... waits=` 那一行，
    /// 確認是限速讓它成功的，而不是別的原因。
    #[tokio::test(flavor = "current_thread")]
    #[ignore]
    async fn live_long_audio_against_the_real_endpoint() {
        let key = std::env::var("SAYIT_QWEN_KEY").expect("SAYIT_QWEN_KEY");
        let wav = std::fs::read(std::env::var("SAYIT_LONG_WAV").expect("SAYIT_LONG_WAV"))
            .expect("read wav");
        let seconds: usize = std::env::var("SAYIT_LONG_SECONDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(99);
        let model = std::env::var("SAYIT_QWEN_MODEL")
            .unwrap_or_else(|_| "qwen3.5-omni-plus-realtime".to_string());

        let data_at = wav
            .windows(4)
            .position(|w| w == b"data")
            .expect("wav has a data chunk")
            + 8;
        let pcm = &wav[data_at..];
        let pcm = &pcm[..pcm.len().min(seconds * 32000)];

        let config = AsrProviderConfig {
            provider: "qwen_omni".to_string(),
            api_key: key,
            app_id: String::new(),
            extra: serde_json::json!({ "model": model }),
        };
        let b64 = base64::engine::general_purpose::STANDARD.encode(pcm);
        let result = transcribe(&b64, 16000, &config, &[]).await;
        match &result {
            Ok(r) => println!(
                "LIVE seconds={} model={} chars={} elapsed_ms={}",
                seconds,
                model,
                r.text.chars().count(),
                r.elapsed_ms
            ),
            Err(e) => println!("LIVE seconds={} model={} error={}", seconds, model, e),
        }
        let result = result.expect("long audio must transcribe");
        assert!(!result.text.trim().is_empty());
    }
}
