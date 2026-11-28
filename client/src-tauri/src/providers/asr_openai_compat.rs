// 「OpenAI 相容」那張卡的分發層：把協議探測出來，再交給真正幹活的那一份實現。
//
// ## 為什麼需要這一層
//
// 「OpenAI 相容」在語音這塊其實是**兩套完全不同的協議**（實測記錄見
// `dev-scripts/probe_bailian_openai_audio.py`）：
//   · `/audio/transcriptions`：multipart 傳檔案，響應 `{"text": ...}` —— asr_groq.rs
//   · `/chat/completions`：音訊作為 `input_audio` 塞進 messages —— asr_openai_chat_audio.rs
//     對話介面有兩種音訊形態：百鍊 data URL 字串、標準 OpenAI data/format 物件。
//
// 這裡曾經做成兩張卡、讓使用者自己選。否掉了：**使用者沒辦法知道自己要連的服務說哪種協議**，
// 他得去翻對方文件，或者兩張卡輪流試。那是把我們的實現細節推給使用者承擔。
// 所以合成一張卡，協議預設 `auto` 由這裡試出來；`extra.protocol` 的三個顯式值
// （`transcriptions` / `chat` / `chat_standard`）留作探測判不準時的手動退路。
//
// ## 探測只在程序內做一次
//
// 結果按「地址 + 模型」快取在程序裡，與 AI 潤色那邊「記住這個端點不接受某引數」
// 是同一個套路（見 `.kiro/decisions.md`「預設替所有 OpenAI 相容端點關閉思考」）。
// 所以最壞情況是每個端點每次啟動多兩個失敗請求，而不是每次轉寫都重新探測。
//
// ⚠️ 不要把探測改成「先發一個輕量請求試路由」：那會在正常路徑上憑空多一次往返。
// 現在的做法是拿**真實的那次轉寫**去試，成功了順手記住。

use super::diag;
use super::types::{AsrProviderConfig, AsrResult, TestResult};
use std::collections::HashMap;
use std::sync::Mutex;

const SCOPE: &str = "openai-compat/dispatch";

/// 內部分發 key：交給 asr_groq / asr_openai_chat_audio 時換成它們認識的 provider id。
const AS_TRANSCRIPTIONS: &str = "openai_compat_transcribe";
const AS_CHAT: &str = "openai_chat_audio";
const AS_CHAT_STANDARD: &str = "openai_chat_audio_standard";

/// 「這個地址 + 這個模型」上次是哪種協議成功的。
///
/// 只在程序內有效，重啟即忘 —— 這是刻意的：使用者換了對面的服務、或者對面升級了，
/// 快取不該跨程序留著。命中率在一次會話裡已經夠用。
static PROTOCOL_CACHE: Mutex<Option<HashMap<String, &'static str>>> = Mutex::new(None);

fn cache_key_from_extra(extra: &serde_json::Value) -> String {
    let field = |name: &str| {
        extra
            .get(name)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    format!("{}|{}", field("baseUrl"), field("model"))
}

fn cache_key(config: &AsrProviderConfig) -> String {
    cache_key_from_extra(&config.extra)
}

/// 這份配置上次探到的是哪種協議，沒探過返回 None。
///
/// 給 `capabilities.rs` 用：auto 檔的熱詞能力取決於協議，探測之前只能回答"未確定"，
/// 探過之後就該給準話。只改 `labelled()` 裡測試成功的那句提示是不夠的 ——
/// 真正的轉寫也會探測並寫快取（見下面 transcribe 裡的 remember_protocol），
/// 使用者可能根本沒點過「測試連線」就直接開始口述了。
///
/// 快取鍵含地址和模型，所以**換了配置自然不命中**，會退回"未確定"而不是給出舊答案。
pub fn detected_protocol(extra: &serde_json::Value) -> Option<&'static str> {
    cached_protocol(&cache_key_from_extra(extra))
}

fn cached_protocol(key: &str) -> Option<&'static str> {
    let guard = PROTOCOL_CACHE.lock().ok()?;
    guard.as_ref()?.get(key).copied()
}

fn remember_protocol(key: &str, protocol: &'static str) {
    if let Ok(mut guard) = PROTOCOL_CACHE.lock() {
        guard
            .get_or_insert_with(HashMap::new)
            .insert(key.to_string(), protocol);
    }
}

/// 使用者顯式選了協議嗎（`auto` 與預設都算沒選）。
fn explicit_protocol(config: &AsrProviderConfig) -> Option<&'static str> {
    match config.extra.get("protocol").and_then(|v| v.as_str()) {
        Some("transcriptions") => Some(AS_TRANSCRIPTIONS),
        Some("chat") => Some(AS_CHAT),
        Some("chat_standard") => Some(AS_CHAT_STANDARD),
        _ => None,
    }
}

/// 把 config 換成下游認識的 provider id，其餘原樣。
fn with_provider(config: &AsrProviderConfig, provider: &str) -> AsrProviderConfig {
    AsrProviderConfig {
        provider: provider.to_string(),
        api_key: config.api_key.clone(),
        app_id: config.app_id.clone(),
        extra: config.extra.clone(),
    }
}

/// 這條錯誤像不像「路由不存在」，也就是協議猜錯了。
///
/// 判據故意窄：它唯一的用處是在兩邊都失敗時決定**報哪一條錯**，猜錯只會讓提示
/// 變差、不影響正確性。金鑰錯、模型不存在這類錯誤不該被當成協議問題 ——
/// 那會讓使用者以為「換個協議就好了」，而真正該做的是去改金鑰。
fn looks_like_wrong_route(error: &str) -> bool {
    let lower = error.to_lowercase();
    lower.contains("404")
        || lower.contains("405")
        || lower.contains("not found")
        || lower.contains("method not allowed")
}

/// 這條錯誤像不像「金鑰/許可權被拒」。
///
/// 用處只有一個：對話介面第二次嘗試（data URL）失敗後，決定還要不要試標準格式。
/// 認證失敗與音訊形態無關，再試只會把整段錄音多傳一遍。判據同樣故意窄 ——
/// 誤判成認證失敗會讓本該成功的第三種格式沒機會試；漏判只是多一次請求。
/// 狀態碼只認 `http=401`（diag::http_summary 的形狀）和兩邊帶空格的寫法，
/// 免得追蹤 ID 裡恰好有 "401" 被誤判。
fn looks_like_auth_failure(error: &str) -> bool {
    let lower = error.to_lowercase();
    [
        "http=401",
        "http=403",
        " 401 ",
        " 403 ",
        "unauthorized",
        "invalid api key",
        "invalid_api_key",
        "incorrect api key",
        "permission denied",
        "permission_denied",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

async fn run(
    provider: &str,
    audio_pcm_b64: &str,
    sample_rate: u32,
    config: &AsrProviderConfig,
    hotwords: &[String],
) -> Result<AsrResult, String> {
    let scoped = with_provider(config, provider);
    if provider == AS_CHAT || provider == AS_CHAT_STANDARD {
        super::asr_openai_chat_audio::transcribe(audio_pcm_b64, sample_rate, &scoped, hotwords).await
    } else {
        super::asr_groq::transcribe(audio_pcm_b64, sample_rate, &scoped, hotwords).await
    }
}

pub async fn transcribe(
    audio_pcm_b64: &str,
    sample_rate: u32,
    config: &AsrProviderConfig,
    hotwords: &[String],
) -> Result<AsrResult, String> {
    // 手動指定過就直接走，不探測（這是探測判不準時的退路，必須說一不二）。
    if let Some(chosen) = explicit_protocol(config) {
        diag::log(SCOPE, "manual", &format!("protocol={}", chosen));
        return run(chosen, audio_pcm_b64, sample_rate, config, hotwords).await;
    }

    let key = cache_key(config);
    if let Some(chosen) = cached_protocol(&key) {
        diag::log(SCOPE, "cached", &format!("protocol={}", chosen));
        return run(chosen, audio_pcm_b64, sample_rate, config, hotwords).await;
    }

    // 先試 multipart 那套：它是這兩者裡更常見的一種（OpenAI 自己、Groq、
    // 以及所有自建 whisper 服务都是它）。
    diag::log(SCOPE, "probe", "trying /audio/transcriptions first");
    let first = run(AS_TRANSCRIPTIONS, audio_pcm_b64, sample_rate, config, hotwords).await;
    let first_error = match first {
        Ok(result) => {
            remember_protocol(&key, AS_TRANSCRIPTIONS);
            diag::log(SCOPE, "detected", "protocol=transcriptions");
            return Ok(result);
        }
        Err(error) => error,
    };

    diag::log(
        SCOPE,
        "probe",
        &format!(
            "/audio/transcriptions failed, trying /chat/completions: {}",
            diag::truncate(&first_error, 200)
        ),
    );
    match run(AS_CHAT, audio_pcm_b64, sample_rate, config, hotwords).await {
        Ok(result) => {
            remember_protocol(&key, AS_CHAT);
            diag::log(SCOPE, "detected", "protocol=chat");
            Ok(result)
        }
        Err(second_error) => {
            // 金鑰錯就別再試第三種：同一地址同一把鑰匙，換個音訊形態照樣被拒，
            // 而失敗不進快取 —— 不攔的話每次口述都要把整段錄音多傳一遍。
            if looks_like_auth_failure(&second_error) {
                // 兩邊都失敗：報**更可能是真正原因**的那一條。第一條像「路由不存在」
                // 而第二條不像，說明協議其實是 chat、錯在別處（金鑰、模型名），
                // 那就報第二條；否則報第一條。
                let show_second =
                    looks_like_wrong_route(&first_error) && !looks_like_wrong_route(&second_error);
                diag::log(
                    SCOPE,
                    "auth_failed",
                    &format!(
                        "reporting={} chat_standard=skipped transcriptions_error={} chat_error={}",
                        if show_second { "chat" } else { "transcriptions" },
                        diag::truncate(&first_error, 200),
                        diag::truncate(&second_error, 200),
                    ),
                );
                return Err(if show_second { second_error } else { first_error });
            }
            // 百鍊 data URL 不一定能被相容閘道器解析；換標準 OpenAI 物件再試一次。
            match run(AS_CHAT_STANDARD, audio_pcm_b64, sample_rate, config, hotwords).await {
                Ok(result) => {
                    remember_protocol(&key, AS_CHAT_STANDARD);
                    diag::log(SCOPE, "detected", "protocol=chat_standard");
                    Ok(result)
                }
                Err(standard_error) => {
                    // 優先反饋標準格式的真實錯誤；但別用它的 404
                    // 覆蓋另一個協議更具體的報錯。
                    let reporting = if !looks_like_wrong_route(&standard_error) {
                        "chat_standard"
                    } else if !looks_like_wrong_route(&second_error) {
                        "chat"
                    } else {
                        "transcriptions"
                    };
                    // 三條錯誤都要留：只記選中的那條，就看不出另外兩條協議是怎麼掛的。
                    diag::log(
                        SCOPE,
                        "all_failed",
                        &format!(
                            "reporting={} transcriptions_error={} chat_error={} \
                             chat_standard_error={}",
                            reporting,
                            diag::truncate(&first_error, 200),
                            diag::truncate(&second_error, 200),
                            diag::truncate(&standard_error, 200),
                        ),
                    );
                    Err(match reporting {
                        "chat_standard" => standard_error,
                        "chat" => second_error,
                        _ => first_error,
                    })
                }
            }
        }
    }
}

pub async fn test_connection(config: &AsrProviderConfig) -> TestResult {
    if let Some(chosen) = explicit_protocol(config) {
        return labelled(chosen, run_test(chosen, config).await);
    }

    let key = cache_key(config);
    if let Some(chosen) = cached_protocol(&key) {
        return labelled(chosen, run_test(chosen, config).await);
    }

    let first = run_test(AS_TRANSCRIPTIONS, config).await;
    if first.ok {
        remember_protocol(&key, AS_TRANSCRIPTIONS);
        return labelled(AS_TRANSCRIPTIONS, first);
    }
    let second = run_test(AS_CHAT, config).await;
    if second.ok {
        remember_protocol(&key, AS_CHAT);
        return labelled(AS_CHAT, second);
    }
    // 金鑰錯不試第三種，報錯規則回到兩條時的樣子（理由同 transcribe）。
    if looks_like_auth_failure(&second.message) {
        let show_second =
            looks_like_wrong_route(&first.message) && !looks_like_wrong_route(&second.message);
        diag::log(
            SCOPE,
            "test_auth_failed",
            &format!(
                "reporting={} chat_standard=skipped",
                if show_second { "chat" } else { "transcriptions" },
            ),
        );
        return if show_second { second } else { first };
    }
    let third = run_test(AS_CHAT_STANDARD, config).await;
    if third.ok {
        remember_protocol(&key, AS_CHAT_STANDARD);
        return labelled(AS_CHAT_STANDARD, third);
    }
    if !looks_like_wrong_route(&third.message) {
        third
    } else if !looks_like_wrong_route(&second.message) {
        second
    } else {
        first
    }
}

async fn run_test(provider: &str, config: &AsrProviderConfig) -> TestResult {
    let scoped = with_provider(config, provider);
    if provider == AS_CHAT || provider == AS_CHAT_STANDARD {
        super::asr_openai_chat_audio::test_connection(&scoped).await
    } else {
        super::asr_groq::test_connection(&scoped).await
    }
}

/// 在成功資訊裡寫明探到的是哪種協議。
///
/// 使用者點「測試」本來就是想知道「能不能用」，順手告訴他走的是哪條路 ——
/// 否則自動探測就是個黑盒，出問題時他連該查哪半邊都不知道。
fn labelled(provider: &str, mut result: TestResult) -> TestResult {
    if result.ok {
        let name = if provider == AS_CHAT_STANDARD {
            "/chat/completions (standard input_audio)"
        } else if provider == AS_CHAT {
            "/chat/completions"
        } else {
            "/audio/transcriptions"
        };
        // 順帶說明熱詞 —— 這兩條協議的答案相反（chat 能把詞表追加到 instruction，
        // transcriptions 的 prompt 被標點引導佔用了），而使用者看到「連線成功」時
        // 最容易順帶以為熱詞也生效了。issue #67 的原話是「避免連線成功後仍讓使用者
        // 誤以為熱詞已生效」。能力判定在 capabilities.rs，這裡只是把它說出來。
        let hotwords = if provider == AS_CHAT || provider == AS_CHAT_STANDARD {
            "hotwords: sent as context"
        } else {
            "hotwords: not sent on this protocol"
        };
        result.message = format!("{} [{} · {}]", result.message, name, hotwords);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(extra: serde_json::Value) -> AsrProviderConfig {
        AsrProviderConfig {
            provider: "openai_compat".to_string(),
            api_key: "k".to_string(),
            app_id: String::new(),
            extra,
        }
    }

    #[test]
    fn explicit_protocol_wins_over_detection() {
        assert_eq!(
            explicit_protocol(&config(serde_json::json!({ "protocol": "chat" }))),
            Some(AS_CHAT),
        );
        assert_eq!(
            explicit_protocol(&config(serde_json::json!({ "protocol": "chat_standard" }))),
            Some(AS_CHAT_STANDARD),
        );
        assert_eq!(
            explicit_protocol(&config(serde_json::json!({ "protocol": "transcriptions" }))),
            Some(AS_TRANSCRIPTIONS),
        );
        // auto、預設、以及任何不認識的值都算「讓程式碼自己試」
        assert_eq!(explicit_protocol(&config(serde_json::json!({ "protocol": "auto" }))), None);
        assert_eq!(explicit_protocol(&config(serde_json::json!({}))), None);
        assert_eq!(explicit_protocol(&config(serde_json::json!({ "protocol": "nope" }))), None);
    }

    /// 快取鍵必須同時含地址和模型：同一個閘道器上兩個模型可能走不同協議
    /// （聚合站尤其常見），只按地址快取會把第二個模型帶偏。
    #[test]
    fn cache_key_covers_both_address_and_model() {
        let a = cache_key(&config(serde_json::json!({ "baseUrl": "http://x/v1", "model": "m1" })));
        let b = cache_key(&config(serde_json::json!({ "baseUrl": "http://x/v1", "model": "m2" })));
        let c = cache_key(&config(serde_json::json!({ "baseUrl": "http://y/v1", "model": "m1" })));
        assert_ne!(a, b);
        assert_ne!(a, c);
        // 同樣的輸入要得到同樣的鍵，否則快取永遠不命中
        assert_eq!(
            a,
            cache_key(&config(serde_json::json!({ "baseUrl": "http://x/v1", "model": "m1" }))),
        );
    }

    #[test]
    fn the_cache_round_trips() {
        let key = "cache-round-trip|model";
        assert_eq!(cached_protocol(key), None);
        remember_protocol(key, AS_CHAT);
        assert_eq!(cached_protocol(key), Some(AS_CHAT));
    }

    /// 「路由不存在」的判据要窄。
    ///
    /// 放寬了會把「金鑰錯」也當成協議問題，於是提示變成「換個協議試試」，
    /// 而使用者真正該做的是去改金鑰。
    #[test]
    fn wrong_route_detection_stays_narrow() {
        assert!(looks_like_wrong_route("Transcription error 404 [http=404]: not found"));
        assert!(looks_like_wrong_route("error 405 Method Not Allowed"));

        assert!(!looks_like_wrong_route("Invalid API key"));
        assert!(!looks_like_wrong_route("model not_a_model does not exist"));
        assert!(!looks_like_wrong_route("Transcription error 401 [http=401]: unauthorized"));
        assert!(!looks_like_wrong_route("insufficient balance"));
    }

    /// 金鑰/許可權被拒才跳過第三次嘗試，其餘錯誤一律照試。
    #[test]
    fn auth_failure_detection_catches_rejected_keys() {
        assert!(looks_like_auth_failure(
            "Transcription error 401 Unauthorized [http=401]: {\"error\":\"bad key\"}"
        ));
        assert!(looks_like_auth_failure("Connection failed 403 Forbidden [http=403]: denied"));
        assert!(looks_like_auth_failure("Incorrect API key provided: sk-***"));
        assert!(looks_like_auth_failure(r#"{"error":{"code":"invalid_api_key"}}"#));
        assert!(looks_like_auth_failure("PERMISSION DENIED: project has no access"));

        assert!(!looks_like_auth_failure("Transcription error 404 [http=404]: not found"));
        assert!(!looks_like_auth_failure("model not_a_model does not exist"));
        // 追蹤 ID 裡恰好帶 401 不算
        assert!(!looks_like_auth_failure("error 500 [http=500 x-request-id=ab401cd]: boom"));
    }

    /// 促成 chat_standard 的那個場景：Gemini 閘道器不認 data URL，回 400。
    /// 它必須繼續去試第三種格式，絕不能被當成金鑰錯跳過。
    #[test]
    fn gemini_data_url_rejection_is_not_an_auth_failure() {
        let gemini = "Transcription error 400 Bad Request [http=400]: \
            [{\"error\":{\"code\":400,\"message\":\"* GenerateContentRequest.contents: \
            contents is not specified\\n\",\"status\":\"INVALID_ARGUMENT\"}}]";
        assert!(!looks_like_auth_failure(gemini));
        assert!(!looks_like_wrong_route(gemini));
    }

    /// 成功資訊裡要寫明走的是哪條協議；失敗資訊不加料（那會蓋住真正的錯誤）。
    #[test]
    fn the_detected_protocol_shows_up_in_a_successful_test() {
        let ok = labelled(AS_CHAT, TestResult {
            ok: true,
            message: "Connection successful".to_string(),
            elapsed_ms: 1,
            detail: String::new(),
        });
        assert!(ok.message.contains("/chat/completions"), "got {}", ok.message);

        let failed = labelled(AS_CHAT, TestResult {
            ok: false,
            message: "Invalid API key".to_string(),
            elapsed_ms: 1,
            detail: String::new(),
        });
        assert_eq!(failed.message, "Invalid API key");
    }
}
