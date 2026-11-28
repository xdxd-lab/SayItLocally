// 各 ASR / AI 供應商的統一診斷日誌。
//
// 為什麼需要這個模組：有使用者報「沒有檢測到有效聲音」，而我們手上除了這句話什麼都沒有。
// 當時除兩條 realtime 路徑外，其餘 provider 在失敗點一條日誌都不寫，返回的 Err 裡也沒有
// 任何服務端資訊（HTTP 狀態、響應體、追蹤 ID），空結果更是走成功路徑、前後端都不留痕跡。
// 結果是排查只能靠猜和反覆問使用者，而真正的原因（額度耗盡、資源未開通、服務端提前關連線）
// 本來在響應裡寫得很清楚。
//
// 三條硬約束，改這裡之前先讀：
//
//   1. **只記文本長度，絕不記識別文本或校對文本。** 公開版不留「使用者說了什麼」的痕跡，
//      與審計只記長度的口徑一致。想加 text=... 的時候請停手。
//   2. **絕不記金鑰。** 只記鑑權模式名（如 api_key(new-console)）。
//   3. **截斷按字元邊界，不按位元組。** 供應商的報錯體基本都是中文，`&s[..n]` 落在多位元組
//      字元中間會直接 panic —— 而這些截斷只在出錯路徑上執行，等於「一齣錯就崩」，
//      把一個看得懂的錯誤變成一次沒有日誌的崩潰。

use crate::commands::system::write_log_line;
use crate::error_protocol;

/// 統一字首。所有 provider 日誌共用一個標籤，排查時 `Select-String '\[provider\]'`
/// 一次就能把整條呼叫鏈撈出來，不必知道使用者用的是哪家。
const TAG: &str = "[RUST] [provider]";

/// 按**字元**邊界截斷，超長補省略號；順手把控制字元換成空格。
///
/// 壓平換行是為了守住「一行一條」的結構：響應體裡的換行會把一條日誌拆成多行，
/// 之後按行 grep 就再也對不齊了。
pub fn truncate(s: &str, max_chars: usize) -> String {
    let mut out: String = s
        .chars()
        .take(max_chars)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    // 只多走一個字元就能判斷有沒有被截斷，不必再遍歷整串
    if s.chars().nth(max_chars).is_some() {
        out.push('…');
    }
    out
}

/// 記一條普通事件（請求已發出、命中了哪個資源等）。
pub fn log(scope: &str, stage: &str, detail: &str) {
    if detail.is_empty() {
        write_log_line(&format!("{} {} {}", TAG, scope, stage));
    } else {
        write_log_line(&format!("{} {} {} {}", TAG, scope, stage, detail));
    }
}

/// 記一條失敗，並把**原樣的使用者可見訊息**返回，供 `?` / `map_err` 直接用：
///
/// ```ignore
/// .map_err(|e| diag::fail(SCOPE, "send_audio", format!("傳送音訊失敗: {}", e)))?
/// ```
///
/// 刻意做成「記錄 + 返回同一條」，是為了讓漏記變成不可能：只要這個錯誤被返回過，
/// 它就一定進過日誌。否則加日誌這件事永遠是半覆蓋的 —— 總會漏掉某個 early return，
/// 而漏掉的那個偏偏就是線上真正走到的分支。
pub fn fail(scope: &str, stage: &str, user_msg: String) -> String {
    write_log_line(&format!(
        "{} {} {} FAILED {}",
        TAG,
        scope,
        stage,
        truncate(&user_msg, 400)
    ));
    error_protocol::encode(classify_failure(stage, &user_msg), user_msg)
}

/// Explicit variant for failures whose semantic category is known at the call site.
pub fn fail_code(scope: &str, stage: &str, code: &str, user_msg: String) -> String {
    write_log_line(&format!(
        "{} {} {} FAILED code={} {}",
        TAG,
        scope,
        stage,
        code,
        truncate(&user_msg, 400)
    ));
    error_protocol::encode(code, user_msg)
}

fn contains_http_status(message: &str, expected: &[u16]) -> bool {
    message
        .split(|ch: char| !ch.is_ascii_digit())
        .filter_map(|part| (part.len() == 3).then(|| part.parse::<u16>().ok()).flatten())
        .any(|status| expected.contains(&status))
}

fn classify_failure(stage: &str, message: &str) -> &'static str {
    let lower = message.to_ascii_lowercase();
    // 401 才是明確的鑑權失敗。**403 不是** —— 它更常見的成因是"這個請求根本不該到這兒"：
    // CDN 按 IP 地區拒絕、賬號沒開通該模型、被 WAF 攔下，響應裡一個字都不提金鑰。
    // 這兩個狀態碼曾經一起歸成 provider_bad_key，於是國內直連 Groq（Cloudflare 邊緣
    // 按 IP 拒，返回 403 {"error":{"message":"Forbidden"}}，連鑑權頭都沒被讀過）
    // 被報成「金鑰被拒絕，確認複製完整、沒有多餘空格」—— 使用者只會一遍遍重建金鑰。
    if contains_http_status(message, &[401])
        || lower.contains("unauthorized")
        || lower.contains("invalid api key")
        || lower.contains("invalid_api_key")
        || message.contains("认证失败")
        || message.contains("鉴权")
    {
        return "provider_bad_key";
    }
    // 402 排在限流之前，而且要早於任何按關鍵詞猜的分支。
    //
    // 「餘額不足」和「被限流」是兩件不同的事，給使用者的動作也相反：限流等一會兒就好，
    // 餘額不足等到明年也還是不行，得去充值。402 是這件事唯一明確的訊號 ——
    // OpenRouter 的 402 響應體裡帶 "balance" 字樣，實測（2026-09-16）會是
    // `This request requires at least $0.50 in balance for audio`。
    // 以前沒有這一檔，它落到最後的 connect_failed，介面顯示成「連線失敗」——
    // 而真實原因是賬戶裡沒錢，使用者完全看不出來該去幹什麼。
    if contains_http_status(message, &[402])
        || lower.contains("payment required")
        || lower.contains("insufficient balance")
        || lower.contains("in balance")
        || lower.contains("insufficient_quota")
        || message.contains("余额不足")
    {
        return "provider_insufficient_balance";
    }
    if contains_http_status(message, &[429])
        || lower.contains("rate limit")
        || lower.contains("quota")
        || message.contains("欠费")
        || message.contains("余额")
    {
        return "provider_rate_limit";
    }
    // 排在限流之後：403 + quota 字樣更可能是配額問題而不是許可權問題
    if contains_http_status(message, &[403]) {
        return "provider_forbidden";
    }
    if contains_http_status(message, &[404])
        || lower.contains("model not found")
        || lower.contains("model does not exist")
    {
        return "provider_no_model";
    }
    if lower.contains("timeout") || lower.contains("timed out") || message.contains("超时") {
        return "provider_timeout";
    }
    if matches!(
        stage,
        "http_send" | "connect" | "send_request" | "recv" | "recv_ack"
    ) {
        return "provider_unreachable";
    }
    "connect_failed"
}

/// 記一條「服務端返回了空結果」。
///
/// 空結果走的是成功路徑 —— 前端只會顯示「未檢測到有效聲音」，然後什麼都不記。
/// 使用者明明說了兩分鐘話卻看到這句提示時，這條日誌是唯一的線索，所以務必帶上服務端
/// 給的全部上下文（狀態碼、響應體摘要、追蹤 ID）。
pub fn empty_result(scope: &str, detail: &str) {
    write_log_line(&format!("{} {} EMPTY_RESULT {}", TAG, scope, detail));
}

/// 記一次成功。**只記字元數，不記內容**（見檔案頭約束 1）。
pub fn ok(scope: &str, elapsed_ms: u64, text_chars: usize) {
    write_log_line(&format!(
        "{} {} ok elapsed={}ms chars={}",
        TAG, scope, elapsed_ms, text_chars
    ));
}

/// 出錯時值得記下來的欄位名。全是「服務端在解釋為什麼不行」的欄位
/// （額度、資源未開通、限流、截斷原因都在這裡），不含任何識別內容。
const SAFE_FIELDS: &[&str] = &[
    "code",
    "error_code",
    "message",
    "msg",
    "status",
    "request_id",
    "finish_reason",
];

/// 明確排除的欄位名：這些裝的是識別文本 / 校對文本，一個都不能進日誌。
const TEXT_FIELDS: &[&str] = &["text", "content", "transcript", "delta", "utterances"];

fn collect_safe_fields(value: &serde_json::Value, depth: usize, out: &mut Vec<String>) {
    // 限深既防禦深層巢狀的大響應，也讓日誌長度可控
    if depth > 4 || out.len() >= 6 {
        return;
    }
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if TEXT_FIELDS.iter().any(|t| k.eq_ignore_ascii_case(t)) {
                    continue;
                }
                // 只收標量。這一條是關鍵：OpenAI 相容響應裡的 `message` 是個**物件**，
                // 裡面裝著 content（識別/校對文本）—— 只收標量就自動把它擋在外面，
                // 同時保留 `message: "資源未開通"` 這種真正有用的字串。
                let is_scalar = v.is_string() || v.is_number() || v.is_boolean();
                if is_scalar && SAFE_FIELDS.iter().any(|s| k.eq_ignore_ascii_case(s)) {
                    let raw = match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    if !raw.is_empty() {
                        out.push(format!("{}={}", k, truncate(&raw, 120)));
                    }
                }
                collect_safe_fields(v, depth + 1, out);
            }
        }
        serde_json::Value::Array(items) => {
            for v in items.iter().take(3) {
                collect_safe_fields(v, depth + 1, out);
            }
        }
        _ => {}
    }
}

/// 描述一個 JSON 響應體，用於「拿到空結果」時留證。
///
/// **只取結構與錯誤欄位，絕不取識別文本**（見檔案頭約束 1）。排查空結果要知道的是
/// 「服務端回了個什麼形狀、有沒有說明原因」，而不是使用者說了什麼。
pub fn describe_json(raw: &str) -> String {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(raw) else {
        return format!("body_bytes={} (non-JSON)", raw.len());
    };
    let mut parts = vec![format!("body_bytes={}", raw.len())];
    if let Some(obj) = json.as_object() {
        let keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
        parts.push(format!("keys=[{}]", keys.join(",")));
    }
    let mut fields = Vec::new();
    collect_safe_fields(&json, 0, &mut fields);
    parts.extend(fields);
    parts.join(" ")
}

/// 供應商常用的服務端追蹤 ID 頭。使用者報問題時帶著它才能找供應商定位，沒有就只能猜。
const TRACE_HEADERS: &[&str] = &[
    "X-Tt-Logid",      // 火山引擎（豆包）
    "x-request-id",    // 阿里云 DashScope / OpenAI 兼容服务通用
    "x-ds-trace-id",   // DeepSeek
    "req-id",
];

/// HTTP 響應裡對排查有用的那部分：狀態碼 + 追蹤 ID。
pub fn http_summary(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
) -> String {
    let mut parts = vec![format!("http={}", status.as_u16())];
    for name in TRACE_HEADERS {
        if let Some(v) = headers.get(*name).and_then(|v| v.to_str().ok()) {
            parts.push(format!("{}={}", name, v));
        }
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_provider_failures_before_the_detail_is_translated() {
        assert_eq!(classify_failure("http_status", "HTTP 401 Unauthorized"), "provider_bad_key");
        assert_eq!(classify_failure("http_status", "HTTP 429 Too Many Requests"), "provider_rate_limit");
        assert_eq!(classify_failure("http_send", "request timed out"), "provider_timeout");
        assert_eq!(classify_failure("connect", "socket closed"), "provider_unreachable");
    }

    /// 關鍵迴歸：403 不許再報成「金鑰被拒絕」。
    ///
    /// 實測 Groq 在中國大陸 IP 上的響應就是這一行，且假金鑰、真金鑰、完全不帶鑑權頭
    /// 三種情況返回的內容**完全相同** —— 說明請求在邊緣節點就被拒了，金鑰從未被驗證。
    /// 歸成金鑰問題會把使用者引去反覆重建金鑰。
    #[test]
    fn forbidden_is_not_reported_as_a_bad_key() {
        assert_eq!(
            classify_failure(
                "http_status",
                r#"API error 403 Forbidden [http=403]: {"error":{"message":"Forbidden"}}"#
            ),
            "provider_forbidden"
        );
        // 403 但服務端明確提了金鑰無效，仍然算金鑰問題
        assert_eq!(
            classify_failure("http_status", "HTTP 403: Invalid API Key"),
            "provider_bad_key"
        );
        // 403 但說的是配額，歸到限流而不是許可權
        assert_eq!(
            classify_failure("http_status", "HTTP 403: quota exceeded"),
            "provider_rate_limit"
        );
    }

    /// 關鍵迴歸：按位元組切中文會 panic。歷史上 doubao / ollama / qwen / mimo 四處
    /// 各有一份 `&s[..n]` 的實現，只要供應商回一段中文報錯就崩 —— 而那正是出錯的時刻。
    #[test]
    fn truncates_multibyte_without_panicking() {
        let body = "请求失败：资源未开通，请到控制台确认";
        // 若按位元組切，7 會落在某個漢字中間
        let out = truncate(body, 7);
        assert_eq!(out, "请求失败：资源…");
    }

    #[test]
    fn keeps_short_strings_intact() {
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("", 10), "");
    }

    /// 恰好等長時不該加省略號。
    #[test]
    fn no_ellipsis_at_exact_length() {
        assert_eq!(truncate("abcd", 4), "abcd");
        assert_eq!(truncate("abcde", 4), "abcd…");
    }

    /// 換行必須壓平，否則一條日誌會被拆成多行、破壞按行 grep。
    #[test]
    fn flattens_newlines() {
        assert_eq!(truncate("a\nb\r\nc", 20), "a b  c");
    }

    /// 關鍵隱私迴歸：OpenAI 相容響應裡 `message` 是物件、`content` 裝著文本，
    /// 描述響應體時一個字都不能帶出來。
    #[test]
    fn describe_json_never_leaks_recognized_text() {
        let body = r#"{"choices":[{"message":{"content":"我的银行卡密码是一二三四"},
            "finish_reason":"stop"}],"request_id":"req-42"}"#;
        let out = describe_json(body);
        assert!(!out.contains("银行卡"), "识别文本泄漏进日志: {}", out);
        assert!(!out.contains("密码"), "识别文本泄漏进日志: {}", out);
        // 該帶的診斷資訊要帶上
        assert!(out.contains("request_id=req-42"), "{}", out);
        assert!(out.contains("finish_reason=stop"), "{}", out);
    }

    /// `message` 是字串時它就是服務端的錯誤說明，這種要保留。
    #[test]
    fn describe_json_keeps_scalar_message() {
        let out = describe_json(r#"{"code":45000030,"message":"资源未开通"}"#);
        assert!(out.contains("code=45000030"), "{}", out);
        assert!(out.contains("message=资源未开通"), "{}", out);
    }

    /// 頂層 text / utterances 一律不取。
    #[test]
    fn describe_json_skips_text_fields() {
        let out = describe_json(r#"{"result":{"text":"这是识别出来的句子"}}"#);
        assert!(!out.contains("识别出来"), "{}", out);
        assert!(out.contains("keys=[result]"), "{}", out);
    }

    #[test]
    fn describe_json_handles_non_json() {
        let out = describe_json("<html>502 Bad Gateway</html>");
        assert!(out.contains("non-JSON"), "{}", out);
    }
}
