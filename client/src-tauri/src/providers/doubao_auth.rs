// 豆包（火山引擎）ASR 的鑑權頭與資源 ID —— 三條呼叫路徑（flash / nostream / 雙向流式）共用。
//
// 火山有兩代控制台，鑑權方式不同：
//
//   舊版控制台：X-Api-App-Key（App ID） + X-Api-Access-Key（Access Token）
//   新版控制台：只要 X-Api-Key（APP Key）
//
// 兩代的金鑰**不是同一個東西**，不能互換：把 Access Token 當 X-Api-Key 發，或者把
// APP Key 當 App ID 發，都只會拿到一個長得像「金鑰填錯了」的 401，使用者根本猜不到
// 是控制台代次不對。所以模式由呼叫方明確給出，這裡不做任何猜測式回退。
//
// 判定口徑：**app_id 是否為空**。前端按使用者選擇的控制台版本，把「本次生效的憑據」
// 寫進執行時配置——新版只寫 api_key、app_id 留空；舊版兩個都寫。於是線上格式是自解釋的，
// 不需要再額外傳一個模式欄位（也就不會出現某個呼叫點忘了傳、導致一半使用者報錯的情況）。
//
// 這裡刻意不保留歷史上那個 `if app_id.is_empty() { app_id = api_key }` 的回退：
// 它既發不出正確的新版請求頭，還會把金鑰當 uid 塞進請求體。

use super::types::AsrProviderConfig;

/// 豆包 Seed-ASR 2.0 流式資源 ID。小時版（包時）與併發版（按量）是兩種計費方式，
/// 同一個賬號通常只開通其中之一，所以連線時要依次嘗試。
pub const RESOURCE_ID_SAUC_DURATION: &str = "volc.seedasr.sauc.duration";
pub const RESOURCE_ID_SAUC_CONCURRENT: &str = "volc.seedasr.sauc.concurrent";

/// 流式端點按「小時版 → 併發版」依次嘗試的資源 ID。
pub const SAUC_RESOURCE_CANDIDATES: &[&str] =
    &[RESOURCE_ID_SAUC_DURATION, RESOURCE_ID_SAUC_CONCURRENT];

/// 請求體 `user.uid`。新版控制台沒有 App ID 可填，而這個欄位只是給服務端做呼叫方標識，
/// 絕不能拿金鑰頂替（歷史實現就是這麼幹的，等於把 Access Token 寫進請求體）。
const FALLBACK_UID: &str = "sayit";

/// 一次呼叫要用哪套鑑權。
pub enum DoubaoAuth<'a> {
    /// 新版控制台：只发 X-Api-Key。
    ApiKey { api_key: &'a str },
    /// 舊版控制台：App ID + Access Token。
    AppIdAccessKey { app_id: &'a str, access_key: &'a str },
}

impl<'a> DoubaoAuth<'a> {
    /// 從前端傳入的配置判定鑑權方式。空 app_id = 新版控制台。
    pub fn from_config(config: &'a AsrProviderConfig) -> Self {
        let app_id = config.app_id.trim();
        if app_id.is_empty() {
            DoubaoAuth::ApiKey { api_key: config.api_key.trim() }
        } else {
            DoubaoAuth::AppIdAccessKey {
                app_id,
                access_key: config.api_key.trim(),
            }
        }
    }

    /// 請求體裡的 `user.uid`。舊版用 App ID（它本來就是公開的賬號標識），
    /// 新版沒有可用標識，用固定佔位符，絕不回落到金鑰。
    pub fn uid(&self) -> &str {
        match self {
            DoubaoAuth::ApiKey { .. } => FALLBACK_UID,
            DoubaoAuth::AppIdAccessKey { app_id, .. } => app_id,
        }
    }

    /// 供日誌使用的模式名（不含任何金鑰）。
    pub fn mode_name(&self) -> &'static str {
        match self {
            DoubaoAuth::ApiKey { .. } => "api_key(new-console)",
            DoubaoAuth::AppIdAccessKey { .. } => "app_id+access_key(legacy-console)",
        }
    }

    /// 憑據是否填全。缺東西就別發請求：閘道器只會回一個含義模糊的鑑權錯誤。
    pub fn missing_field(&self) -> Option<&'static str> {
        match self {
            DoubaoAuth::ApiKey { api_key } if api_key.is_empty() => Some("API Key"),
            DoubaoAuth::AppIdAccessKey { access_key, .. } if access_key.is_empty() => {
                Some("Access Token")
            }
            _ => None,
        }
    }

    /// 以 (名, 值) 形式返回本模式需要的鑑權頭，呼叫方逐個塞進 builder。
    pub fn headers(&self) -> Vec<(&'static str, &'a str)> {
        match self {
            DoubaoAuth::ApiKey { api_key } => vec![("X-Api-Key", api_key)],
            DoubaoAuth::AppIdAccessKey { app_id, access_key } => vec![
                ("X-Api-App-Key", app_id),
                ("X-Api-Access-Key", access_key),
            ],
        }
    }
}

/// 把火山的狀態碼翻成使用者能照著做事的一句話。
///
/// 這些碼會經歷史記錄的「原因」展示給使用者看 —— 只給一串數字等於沒說。
///
/// **只寫有實據的**：`20000003` 有本倉庫既有程式碼為證（test_connection 一直把它當靜音音訊
/// 放行），`45000030` 是拿真實憑據打線上介面實測到的（`requested resource not granted`）。
/// 其餘按字首分族給一個方向性說明，不猜具體含義 —— 猜錯了比不寫更壞，會把使用者引到
/// 錯誤的地方去排查。
pub fn explain_status_code(code: &str) -> Option<&'static str> {
    match code {
        "20000003" => Some("The audio was classified as silence; no speech was detected"),
        "45000030" => Some("This account has not enabled the requested resource or has exhausted its quota; check the Volcengine console"),
        // 以下三條出自官方《大模型流式語音識別API》錯誤碼錶（docs.volcengine.com/docs/6561/1354869）
        "45000002" => Some("The audio sent to Volcengine was empty"),
        "45000081" => Some("Volcengine stopped waiting for the next audio packet; the connection stalled while uploading"),
        "55000031" => Some("Volcengine is overloaded; try again later"),
        _ => {
            if code.starts_with("45") {
                Some("The request was rejected; check parameters, audio format, and quota in the Volcengine console")
            } else if code.starts_with("55") {
                Some("Volcengine failed to process the request; try again later")
            } else {
                None
            }
        }
    }
}

/// 從服務端返回的 JSON 裡取出狀態碼並翻成人話，拼成可直接展示的字尾。
/// 拿不到碼就返回空串，呼叫方直接拼接即可。
pub fn explain_payload(payload: &str) -> String {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(payload) else {
        return String::new();
    };
    // code 可能是數字也可能是字串，兩種都取
    let code = json
        .get("code")
        .map(|v| match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default();
    match explain_status_code(&code) {
        Some(text) => format!("（{}）", text),
        None => String::new(),
    }
}

/// 從 WebSocket 握手響應裡取 X-Tt-Logid 並寫日誌。
///
/// 火山文件明確「強烈建議記錄 logid 作為排錯線索」：這是服務端給的唯一追蹤 ID，
/// 使用者報「豆包連不上」時帶著它才能找火山定位，沒有就只能猜。
pub fn log_ws_logid(scope: &str, response: &tokio_tungstenite::tungstenite::handshake::client::Response) {
    let logid = response
        .headers()
        .get("X-Tt-Logid")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("<none>");
    crate::commands::system::write_log_line(&format!(
        "[RUST] [doubao] {} handshake ok logid={}",
        scope, logid
    ));
}

/// 從 HTTP 響應裡取 X-Tt-Logid 並寫日誌。
pub fn log_http_logid(scope: &str, headers: &reqwest::header::HeaderMap) {
    let logid = headers
        .get("X-Tt-Logid")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("<none>");
    crate::commands::system::write_log_line(&format!(
        "[RUST] [doubao] {} response logid={}",
        scope, logid
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(api_key: &str, app_id: &str) -> AsrProviderConfig {
        AsrProviderConfig {
            provider: "doubao_v2".into(),
            api_key: api_key.into(),
            app_id: app_id.into(),
            extra: serde_json::Value::Null,
        }
    }

    /// 空 app_id = 新版控制台，只发 X-Api-Key。
    #[test]
    fn empty_app_id_selects_new_console() {
        let c = cfg("APPKEY", "");
        let auth = DoubaoAuth::from_config(&c);
        assert_eq!(auth.headers(), vec![("X-Api-Key", "APPKEY")]);
    }

    /// 有 app_id = 舊版控制台，發 App Key + Access Key 兩個頭。
    #[test]
    fn app_id_selects_legacy_console() {
        let c = cfg("TOKEN", "1234567890");
        let auth = DoubaoAuth::from_config(&c);
        assert_eq!(
            auth.headers(),
            vec![
                ("X-Api-App-Key", "1234567890"),
                ("X-Api-Access-Key", "TOKEN"),
            ]
        );
    }

    /// 關鍵迴歸：金鑰絕不能出現在 uid 裡。歷史實現在 app_id 為空時用 api_key 頂替，
    /// 於是 Access Token 被寫進請求體。
    #[test]
    fn uid_never_leaks_the_secret() {
        let c = cfg("SECRET", "");
        let auth = DoubaoAuth::from_config(&c);
        assert_eq!(auth.uid(), FALLBACK_UID);
        assert_ne!(auth.uid(), "SECRET");
    }

    /// 舊版下 uid 用 App ID —— 它是賬號標識，不是金鑰。
    #[test]
    fn uid_uses_app_id_on_legacy() {
        let c = cfg("TOKEN", "1234567890");
        assert_eq!(DoubaoAuth::from_config(&c).uid(), "1234567890");
    }

    /// 兩種模式各自缺料時要能報出缺的是什麼。
    #[test]
    fn reports_the_missing_credential() {
        assert_eq!(DoubaoAuth::from_config(&cfg("", "")).missing_field(), Some("API Key"));
        assert_eq!(
            DoubaoAuth::from_config(&cfg("", "123")).missing_field(),
            Some("Access Token")
        );
        assert_eq!(DoubaoAuth::from_config(&cfg("k", "")).missing_field(), None);
        assert_eq!(DoubaoAuth::from_config(&cfg("k", "123")).missing_field(), None);
    }

    /// 有實據的兩個碼要給出具體說明。
    #[test]
    fn explains_the_codes_we_have_evidence_for() {
        assert!(explain_status_code("20000003").unwrap().contains("silence"));
        assert!(explain_status_code("45000030").unwrap().contains("quota"));
    }

    /// 沒實據的碼按字首給方向，不編造具體含義。
    #[test]
    fn falls_back_to_code_family() {
        assert!(explain_status_code("45000001").unwrap().contains("rejected"));
        assert!(explain_status_code("55000031").unwrap().contains("Volcengine"));
    }

    /// 成功碼和無法識別的碼不該產生任何提示。
    #[test]
    fn no_hint_for_success_or_unknown() {
        assert_eq!(explain_status_code("20000000"), None);
        assert_eq!(explain_status_code(""), None);
        assert_eq!(explain_status_code("abc"), None);
    }

    /// 返回體裡的 code 可能是數字也可能是字串，兩種都要認。
    #[test]
    fn explains_payload_with_either_code_type() {
        assert!(explain_payload(r#"{"code":45000030}"#).contains("quota"));
        assert!(explain_payload(r#"{"code":"45000030"}"#).contains("quota"));
    }

    /// 拿不到碼就返回空串，呼叫方直接拼接不會多出括號。
    #[test]
    fn explains_payload_returns_empty_when_no_code() {
        assert_eq!(explain_payload("not json"), "");
        assert_eq!(explain_payload(r#"{"message":"x"}"#), "");
        assert_eq!(explain_payload(r#"{"code":20000000}"#), "");
    }

    /// 前後空白是貼上時常見的髒資料，判定與傳送都應按 trim 後的值。
    #[test]
    fn trims_whitespace_around_credentials() {
        let c = cfg("  KEY  ", "  ");
        let auth = DoubaoAuth::from_config(&c);
        assert_eq!(auth.headers(), vec![("X-Api-Key", "KEY")]);
    }
}

#[cfg(test)]
mod status_code_tests {
    use super::explain_status_code;

    /// 文件裡寫明的碼要有專門的說明，不能落進「45 開頭 = 請求被拒」的籠統分支 ——
    /// 45000081 是網路卡住，讓使用者去查引數和額度就是把人往錯的方向引。
    #[test]
    fn documented_codes_get_specific_explanations() {
        assert!(explain_status_code("45000081").unwrap().contains("next audio packet"));
        assert!(explain_status_code("45000002").unwrap().contains("empty"));
        assert!(explain_status_code("55000031").unwrap().contains("overloaded"));
    }
}
