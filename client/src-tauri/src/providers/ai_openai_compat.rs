// OpenAI 相容 AI 供應商
// 覆盖所有支持 /v1/chat/completions 的服务：DeepSeek、通义、豆包（火山方舟）等

use super::diag;
use super::prompt::wrap_user_text;
use super::types::{AiProviderConfig, AiResult, TestResult, TextContext};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const SCOPE: &str = "ai/openai-compat";

/// 共享客戶端（帶 User-Agent，缺了會被 nginx/WAF 閘道器攔成 403，見 `http_client`）
fn http() -> &'static reqwest::Client {
    super::http_client::shared()
}

/// 一份「通吃」的關閉思考引數。
///
/// 為什麼是四個欄位一起發，而不是按供應商挑一個：關閉思考的寫法沒有任何統一標準
/// （`enable_thinking` / `thinking.type` / `reasoning.effort` / 頂層 `reasoning_effort`），
/// 欄位名、巢狀層級、取值各家都不同，而且新模型新格式一直在出。逐一適配意味著每出一家
/// 就要發一次版，永遠追不完 —— 智譜現在只能靠硬編碼官方域名識別就是這個成本的證據，
/// 使用者換個反代域名那行就失效。
///
/// 而 OpenAI 相容服務對**不認識的頂層欄位基本是忽略**，所以一次全發、誰認哪個哪個生效，
/// 是覆蓋面和維護成本都最划算的做法。少數嚴格的端點會因此返回 400，那條路由
/// `send_chat_with_thinking_fallback` 自動降級兜住。
fn disable_thinking_overrides() -> serde_json::Value {
    serde_json::json!({
        "enable_thinking": false,
        "thinking": { "type": "disabled" },
        "reasoning": { "effort": "none" },
        "reasoning_effort": "none",
    })
}

/// 決定這次請求該注入哪些關閉思考的欄位。`None` = 一個欄位都不發。
///
/// 三檔，從窄到寬：
///   1. 已知供應商沿用各自**精確**的那一個欄位。這些組合是實測透過的，不要順手升級成
///      通吃集合 —— 多注入未知欄位只會給本來好用的路徑引入 400 風險。
///   2. OpenAI 官方與 Azure OpenAI **顯式排除**。它們對未知頂層引數直接回 400
///      （`Unrecognized request argument supplied`），而且連自家 `reasoning_effort` 的合法
///      取值都依模型而異（老 GPT-5 認 `minimal`，新的只認 `none`，非推理模型兩個都不認），
///      猜不準。這兩家真需要控制思考強度時走使用者自定義引數，不由我們替它猜。
///   3. 其餘一切端點（自定義 OpenAI 相容、聚合閘道器、豆包、Groq…）發通吃集合。
///      這一檔才是這個函式存在的理由：在此之前它們一個欄位都不發，使用者接一個預設思考的
///      模型，我們沒有任何手段讓它別想 —— 語音輸入場景下那幾秒延遲是純虧損。
fn thinking_overrides(config: &AiProviderConfig) -> Option<serde_json::Value> {
    // 通義千問 Qwen3 使用獨立的開關。
    if config.provider == "qwen" {
        return Some(serde_json::json!({ "enable_thinking": false }));
    }

    // 已知「始終思考」的模型直接發最低強度那套，別先發一次註定被拒的 disabled。
    // 放在最前面：它是按**模型**判的，比下面那些按供應商判的更具體。
    if model_forces_thinking(&config.model) {
        return Some(forced_thinking_overrides());
    }

    // DeepSeek、小米 MiMo 和智譜 GLM 使用 thinking.type=disabled。
    // 智譜兩條都認：provider == "zhipu" 是內建卡，域名判斷留給「用 OpenAI 相容自己填
    // 智譜地址」的老配置（這種存量配置不會因為新增內建卡而自動遷移）。
    if config.provider == "deepseek"
        || config.provider == "mimo"
        || config.provider == "zhipu"
        || is_zhipu_api_url(&config.api_url)
    {
        return Some(serde_json::json!({ "thinking": { "type": "disabled" } }));
    }

    if is_strict_openai_host(&config.api_url) {
        return None;
    }

    Some(disable_thinking_overrides())
}

/// [自訂] 嚴禁 DeepSeek：任何主機名稱含 "deepseek" 的端點一律拒絕連線。
/// [自訂] S22：改為 pub(crate)，registry 的本機 Ollama 守衛也會再檢查一次（額外防線）。
pub(crate) fn is_forbidden_ai_host(api_url: &str) -> bool {
    host_of(api_url).map_or(false, |h| h.contains("deepseek"))
        || api_url.to_ascii_lowercase().contains("deepseek")
}

fn host_of(api_url: &str) -> Option<String> {
    reqwest::Url::parse(api_url.trim())
        .ok()
        .and_then(|url| url.host_str().map(|host| host.to_ascii_lowercase()))
}

fn is_zhipu_api_url(api_url: &str) -> bool {
    host_of(api_url).is_some_and(|host| host == "open.bigmodel.cn")
}

/// OpenAI 官方 / Azure OpenAI —— 對未知請求欄位一律 400 的那一類端點。
fn is_strict_openai_host(api_url: &str) -> bool {
    host_of(api_url).is_some_and(|host| {
        host == "api.openai.com"
            || host.ends_with(".openai.azure.com")
            || host.ends_with(".cognitiveservices.azure.com")
    })
}

/// 把 overrides 合併進請求體。頂層同名欄位**整體覆蓋**，不做深合併 —— 深合併會讓
/// 兩種風格的引數混出誰也說不清的結果。
fn merge_overrides(body: &mut serde_json::Value, overrides: &serde_json::Value) {
    let (Some(target), Some(source)) = (body.as_object_mut(), overrides.as_object()) else {
        return;
    };
    for (key, value) in source {
        target.insert(key.clone(), value.clone());
    }
}

/// 記住「這個端點該發哪套思考引數」的程序內結論。
///
/// 值是**降級後真正用的那套引數**，`None` 表示一個欄位都別發。以前這裡是
/// `HashSet<端點>`，語義只有「拒絕思考引數」一種 —— 而強制思考的模型需要的不是
/// 「什麼都不發」，是「發一套更輕的」，一個布林存不下這個區別
/// （見 `forced_thinking_overrides` 的說明）。
///
/// 只放記憶體、不落盤：一次程序生命週期內夠用，而端點行為會隨供應商升級變化，
/// 持久化反而會把一條過期結論永久釘住。key 帶上模型名是因為這類拒絕是按模型判的，
/// 同一閘道器換個模型結論可能不同。
fn thinking_memo() -> &'static Mutex<HashMap<String, Option<serde_json::Value>>> {
    static MEMO: OnceLock<Mutex<HashMap<String, Option<serde_json::Value>>>> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(HashMap::new()))
}

fn endpoint_key(config: &AiProviderConfig) -> String {
    let host = host_of(&config.api_url).unwrap_or_else(|| config.api_url.trim().to_string());
    format!("{}|{}", host, config.model)
}

/// 之前降級過嗎？`Some(params)` 是那次成功的引數（內層 `None` = 一個欄位都不發）。
fn remembered_thinking(config: &AiProviderConfig) -> Option<Option<serde_json::Value>> {
    thinking_memo()
        .lock()
        .ok()
        .and_then(|memo| memo.get(&endpoint_key(config)).cloned())
}

fn remember_thinking(config: &AiProviderConfig, overrides: Option<serde_json::Value>) {
    if let Ok(mut memo) = thinking_memo().lock() {
        memo.insert(endpoint_key(config), overrides);
    }
}

/// 「強制思考」模型該發的引數：思考開著，但用最低強度。
///
/// 為什麼不能像別的端點那樣直接把引數全去掉：這類模型**沒有不思考這一檔**，
/// 去掉引數等於用它的預設強度，而智譜 GLM-5.3 的預設是 `reasoning_effort=max`
/// （深度推理）—— 對語音輸入來說那是最差的結果，比不降級還慢。官方給的遷移寫法
/// 正是把 `disabled` 換成 `enabled` + `reasoning_effort: "low"`。
fn forced_thinking_overrides() -> serde_json::Value {
    serde_json::json!({
        "thinking": { "type": "enabled" },
        "reasoning_effort": "low"
    })
}

/// 模型名一眼能認出「始終思考」的那幾個。
///
/// 只為省掉一次註定失敗的往返（否則使用者在設定頁點「測試」會先吃一個 400）。
/// 這個名單一定會過期，所以它不是唯一防線 —— 真正兜底的是 `forced_thinking_error`
/// 那條按響應內容判定的路徑，將來新出的強制思考模型走那條也能自愈。
fn model_forces_thinking(model: &str) -> bool {
    // glm-5.3 / glm-5.3-flash / glm-5.3-xxx 都算
    model.trim().to_ascii_lowercase().starts_with("glm-5.3")
}

/// 這個錯誤是不是在說「本模型不能關思考，請改用強度檔位」。
///
/// 智譜的原文是純中文 + 一個錯誤碼：
/// `{"error":{"code":"1210","message":"該模型始終思考，不支援關閉思考；請使用 low、high 或 max。"}}`
///
/// **`thinking` 這個詞一次都沒出現** —— 這正是舊判據（"響應體裡有沒有我們注入的鍵名"）
/// 對它完全失效的原因：判據沒命中 → 降級不執行 → 400 原樣報給使用者，介面上就是
/// 「連線失敗」。所以這裡必須按語義匹配，不能只認欄位名。
fn forced_thinking_error(body: &str) -> bool {
    // 智譜的錯誤碼，最穩的一條
    if body.contains("\"1210\"") || body.contains("\"code\":1210") {
        return true;
    }
    // 中文原文（措辭可能隨版本微調，取兩個最穩的片段）
    if body.contains("始终思考") || body.contains("不支持关闭思考") {
        return true;
    }
    // 英文/其它閘道器的等價說法
    let lower = body.to_ascii_lowercase();
    lower.contains("does not support disabling thinking")
        || lower.contains("thinking cannot be disabled")
        || (lower.contains("reasoning_effort") && lower.contains("low"))
}

/// 這個 400 是不是我們注入的思考引數引起的 —— 返回被點名的那個欄位名。
///
/// 判定刻意保守：**只有響應體裡出現了我們實際注入的鍵名**才算。金鑰錯、模型不存在這些
/// 400 絕不能觸發降級重試，否則每次請求都要白髮兩遍。
/// 供應商用自然語言描述、完全不提欄位名的那一類，由 `forced_thinking_error` 單獨認。
fn rejected_thinking_field(body: &str, overrides: &serde_json::Value) -> Option<String> {
    let object = overrides.as_object()?;
    object
        .keys()
        .find(|key| body.contains(key.as_str()))
        .cloned()
}

/// 一次 chat/completions 往返的結果。響應體在這裡就讀完，呼叫方不必再和 `resp` 的
/// 所有權打交道 —— 摘要必須在讀 body 之前取（讀 body 會消費掉 resp）。
struct ChatHttpResponse {
    status: reqwest::StatusCode,
    summary: String,
    body: String,
}

/// 傳送失敗和讀響應體失敗要分開報：前者是網路/DNS/TLS，後者是連線中途斷了，
/// 歸成同一條日誌會讓排查時分不清該查哪一邊。
enum ChatHttpError {
    Send(reqwest::Error),
    ReadBody(reqwest::Error),
}

async fn send_chat_once(
    url: &str,
    config: &AiProviderConfig,
    body: &serde_json::Value,
    timeout: Duration,
) -> Result<ChatHttpResponse, ChatHttpError> {
    let mut req = http()
        .post(url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .header("Content-Type", "application/json");
    // 小米 MiMo 規範鑑權頭為 api-key（同時相容 Bearer），兩個都帶最穩妥
    if config.provider == "mimo" {
        req = req.header("api-key", config.api_key.clone());
    }

    let resp = req
        .json(body)
        .timeout(timeout)
        .send()
        .await
        .map_err(ChatHttpError::Send)?;

    let status = resp.status();
    let summary = diag::http_summary(status, resp.headers());
    let body = resp.text().await.map_err(ChatHttpError::ReadBody)?;

    Ok(ChatHttpResponse {
        status,
        summary,
        body,
    })
}

/// 帶關閉思考引數發一次；被端點拒絕就去掉引數重試一次，並記住這個結論。
///
/// 為什麼要自動降級而不是讓使用者去設定裡勾一個開關：語音輸入的立場是「越快越好」，
/// 模型偷偷思考帶來的幾秒延遲是純虧損，本來就不該等使用者自己發現再去翻設定。代價是
/// 嚴格端點首次會多一次往返（400 一般是閘道器層秒回，約多 0.3~1 秒），之後命中快取不再發生。
///
/// 校對和「測試連線」共用這一條：使用者在設定頁點過測試，結論就已經快取好了，
/// 正式校對時直接命中 —— 那次多出來的往返基本落在測試上，使用者感知不到。
/// 順帶也保證了「測試通了」和「實際請求」用的是同一組引數。
async fn send_chat_with_thinking_fallback(
    url: &str,
    config: &AiProviderConfig,
    base_body: &serde_json::Value,
    timeout: Duration,
    scope: &str,
) -> Result<ChatHttpResponse, ChatHttpError> {
    // 之前降級過就直接用那次的結論，不再重複撞牆
    let overrides = match remembered_thinking(config) {
        Some(remembered) => remembered,
        None => thinking_overrides(config),
    };

    let Some(overrides) = overrides else {
        return send_chat_once(url, config, base_body, timeout).await;
    };

    let mut body = base_body.clone();
    merge_overrides(&mut body, &overrides);
    let resp = send_chat_once(url, config, &body, timeout).await?;

    if resp.status.is_success() {
        return Ok(resp);
    }

    // 不限定狀態碼：多數端點回 400，也見過閘道器把引數校驗失敗報成 500
    // （見 pitfalls「max_tokens 下限」那條）。真正的把關是下面這兩句 —— 必須有一條
    // 明確訊號指向思考引數，否則金鑰錯、模型不存在這些 400 會被白重試一遍。
    //
    // 兩種訊號分別對應兩種降級目標：
    //   · 「這個模型不能關思考」→ 換成最低強度（去掉引數會落到 max，反而更慢）
    //   · 「不認識這個欄位」    → 一個都不發
    let (fallback, reason) = if forced_thinking_error(&resp.body) {
        (Some(forced_thinking_overrides()), "forced_thinking".to_string())
    } else if let Some(field) = rejected_thinking_field(&resp.body, &overrides) {
        (None, format!("unknown_field={}", field))
    } else {
        return Ok(resp);
    };

    // 已經在發最低強度那套了還被拒，就別再原地重試同一份
    if fallback.as_ref() == Some(&overrides) {
        return Ok(resp);
    }

    diag::log(
        scope,
        "thinking_params_rejected",
        &format!(
            "Endpoint rejected our thinking parameters; retrying with fallback reason={} fallback={} status={} model={}",
            reason,
            if fallback.is_some() { "low_effort" } else { "none" },
            resp.status.as_u16(),
            config.model
        ),
    );

    let retried = match &fallback {
        Some(params) => {
            let mut body = base_body.clone();
            merge_overrides(&mut body, params);
            send_chat_once(url, config, &body, timeout).await?
        }
        None => send_chat_once(url, config, base_body, timeout).await?,
    };

    // 只在降級真的成功時才記住它。失敗還記的話會把一條錯結論釘在這個端點上，
    // 後面每次請求都按錯的那套發（而真正的原因可能只是金鑰錯）。
    if retried.status.is_success() {
        remember_thinking(config, fallback);
    }

    Ok(retried)
}

/// 呼叫 OpenAI 相容介面進行文本校對
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

    if is_forbidden_ai_host(&config.api_url) {
        return Err("DeepSeek endpoints are forbidden in this build".to_string());
    }
    let base_url = normalize_base_url(&config.api_url);
    let url = format!("{}/chat/completions", base_url);

    let sys_prompt = system_prompt.unwrap_or(super::prompt::DEFAULT_PROOFREAD_SYSTEM_PROMPT);
    let user_content = wrap_user_text(text, text_context);

    // 思考引數不寫在這裡：由 send_chat_with_thinking_fallback 合併進來，
    // 這樣被端點拒絕時它手上還有一份「乾淨的 body」可以直接重試。
    let base_body = serde_json::json!({
        "model": config.model,
        "temperature": 0.2,
        "max_tokens": 1024,
        "messages": [
            { "role": "system", "content": sys_prompt },
            { "role": "user", "content": user_content },
        ]
    });

    let start = Instant::now();

    // 只記長度和模型，不記待校對的文本本身
    diag::log(
        SCOPE,
        "start",
        &format!(
            "provider={} model={} chars={} url={}",
            config.provider,
            config.model,
            text.chars().count(),
            url
        ),
    );

    let resp = send_chat_with_thinking_fallback(
        &url,
        config,
        &base_body,
        Duration::from_secs(60),
        SCOPE,
    )
    .await
    .map_err(|e| match e {
        ChatHttpError::Send(e) => diag::fail(
            SCOPE,
            "http_send",
            format!("HTTP request failed: {}", describe_reqwest_error(&e)),
        ),
        ChatHttpError::ReadBody(e) => diag::fail(
            SCOPE,
            "read_body",
            format!("Failed to read response: {}", e),
        ),
    })?;

    let elapsed_ms = start.elapsed().as_millis() as u64;
    let ChatHttpResponse {
        status,
        summary: http_summary,
        body: body_text,
    } = resp;

    if !status.is_success() {
        return Err(diag::fail(
            SCOPE,
            "http_status",
            format!(
                "API returned error {} [{}]: {}",
                status,
                http_summary,
                diag::truncate(&body_text, 200)
            ),
        ));
    }

    let data: serde_json::Value = serde_json::from_str(&body_text).map_err(|e| {
        diag::fail(
            SCOPE,
            "parse_json",
            format!(
                "Failed to parse response: {} [{}] response excerpt: {}",
                e,
                http_summary,
                diag::truncate(&body_text, 200)
            ),
        )
    })?;

    // 取不到內容就回落成原文。這是**靜默失敗**：使用者看到有字、以為校對生效了，
    // 實際上 AI 那一步等於沒跑。必須留證，否則「AI 好像沒起作用」永遠查不下去。
    let result_text = match extract_chat_completion_text(&data) {
        Some(t) => t,
        None => {
            diag::log(
                SCOPE,
                "no_content_fallback_to_input",
                &format!(
                    "Response contained no usable content; returned the original text [{}] {}",
                    http_summary,
                    diag::describe_json(&body_text)
                ),
            );
            text.to_string()
        }
    };

    // 去除 <think>...</think> 標籤（部分模型如 Qwen3 會輸出思考過程）
    let cleaned = strip_thinking(&result_text);

    if cleaned.is_empty() {
        diag::log(
            SCOPE,
            "empty_after_strip_thinking",
            &format!(
                "Output was empty after removing the reasoning block; returned the original text model={} raw_chars={}",
                config.model,
                result_text.chars().count()
            ),
        );
    } else {
        diag::ok(SCOPE, elapsed_ms, cleaned.chars().count());
    }

    Ok(AiResult {
        text: if cleaned.is_empty() {
            text.to_string()
        } else {
            cleaned
        },
        elapsed_ms,
    })
}

/// 測試 AI 連線 — 傳送一個簡短的聊天請求，驗證地址、Key、模型是否都可用
pub async fn test_connection(config: &AiProviderConfig) -> TestResult {
    if is_forbidden_ai_host(&config.api_url) {
        return TestResult { ok: false, message: "DeepSeek endpoints are forbidden in this build".to_string(), elapsed_ms: 0, detail: String::new() };
    }
    let base_url = normalize_base_url(&config.api_url);
    let url = format!("{}/chat/completions", base_url);

    let system_prompt = "Reply with OK only. Do not output anything else.";
    let user_prompt = "Connection test";

    // max_tokens 不能貼著 "OK" 兩個字省：
    //   1) 有閘道器直接規定下限 —— 實測某內網 LiteLLM 閘道器上的 gpt-5 系模型給 10 會返回
    //      HTTP 500 `integer_below_min_value ... Expected a value >= 16`，看起來像服務壞了，
    //      其實是我們把上限壓得太低；
    //   2) 推理型模型會先花掉一部分 output token 想事情，額度太小時 content 是空的，
    //      測試就會顯示「連線成功，回覆：(空)」，等於白測。
    // 曾經是 64。改成 512 是因為**強制思考的模型**（智譜 GLM-5.3 那一檔）連最低強度
    // 也要先想一轮，64 个 token 基本全花在 reasoning 上、content 什么都不剩 ——
    // 那種「連上了但回覆是空的」比報錯更難判斷。max_tokens 只是上限，不按它計費。
    let base_body = serde_json::json!({
        "model": config.model,
        "temperature": 0,
        "max_tokens": 512,
        "messages": [
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": user_prompt }
        ]
    });

    let start = Instant::now();

    // 走和校對完全相同的這一條：測試通了就意味著實際校對用的引數也通，
    // 而且「該端點接不接受思考引數」的結論在這裡就快取好了，校對時不必再撞一次。
    let result = send_chat_with_thinking_fallback(
        &url,
        config,
        &base_body,
        Duration::from_secs(30),
        "ai/openai-compat-test",
    )
    .await;

    let elapsed_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(resp) if resp.status.is_success() => {
            let data: serde_json::Value =
                serde_json::from_str(&resp.body).unwrap_or(serde_json::Value::Null);
            let raw_reply = data
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let reply = strip_thinking(&raw_reply);
            let detail = format!(
                "Elapsed: {}ms\nModel: {}\nSent: system=\"{}\" user=\"{}\"\nReply: {}",
                elapsed_ms,
                config.model,
                system_prompt,
                user_prompt,
                if reply.is_empty() { "(empty)" } else { &reply }
            );
            TestResult {
                ok: true,
                message: format!("Connection successful ({}ms)", elapsed_ms),
                elapsed_ms,
                detail,
            }
        }
        Ok(resp) => TestResult {
            ok: false,
            message: diag::fail(
                "ai/openai-compat-test",
                "http_status",
                format!(
                    "API returned {} [{}]: {}",
                    resp.status,
                    resp.summary,
                    diag::truncate(&resp.body, 100)
                ),
            ),
            elapsed_ms,
            detail: format!("Model: {}\nRequest URL: {}", config.model, url),
        },
        Err(ChatHttpError::Send(e)) => TestResult {
            ok: false,
            message: diag::fail(
                "ai/openai-compat-test",
                "http_send",
                format!("Connection failed: {}", describe_reqwest_error(&e)),
            ),
            elapsed_ms,
            detail: format!("Model: {}\nRequest URL: {}", config.model, url),
        },
        Err(ChatHttpError::ReadBody(e)) => TestResult {
            ok: false,
            message: diag::fail(
                "ai/openai-compat-test",
                "read_body",
                format!("Failed to read response: {}", e),
            ),
            elapsed_ms,
            detail: format!("Model: {}\nRequest URL: {}", config.model, url),
        },
    }
}

/// Convert reqwest errors into concise diagnostic details.
fn describe_reqwest_error(e: &reqwest::Error) -> String {
    let raw = format!("{}", e);
    if e.is_timeout() {
        return "Request timed out; check the network connection and API URL".to_string();
    }
    if e.is_connect() {
        // 嘗試區分 DNS / TLS / 連線拒絕
        let lower = raw.to_lowercase();
        if lower.contains("dns") || lower.contains("resolve") || lower.contains("getaddrinfo") {
            return format!(
                "DNS lookup failed; the host may not exist or the network may be unavailable: {}",
                raw
            );
        }
        if lower.contains("ssl")
            || lower.contains("tls")
            || lower.contains("certificate")
            || lower.contains("handshake")
            || lower.contains("schannel")
        {
            return format!("TLS/SSL handshake failed; check the certificate: {}", raw);
        }
        if lower.contains("refused") {
            return format!(
                "Connection refused; the service may not be running: {}",
                raw
            );
        }
        return format!("Could not connect to the server: {}", raw);
    }
    raw
}

/// 規範化 base URL
fn normalize_base_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    // 使用者把文件裡的**完整端點**整條粘進來是最常見的填法 —— 各家文件（智譜、DeepSeek、
    // 矽基流動…）示例裡給的都是 `.../chat/completions`，而這一欄要的是它前面那一截。
    // 不剝掉的話下面會再拼一次，得到 `.../chat/completions/v1/chat/completions` 然後 404，
    // 而 404 的響應體只說路徑不存在，使用者完全看不出是自己多填了一段。
    // ASR 那條路早就這麼處理了（asr_openai_chat_audio.rs 的 chat_completions_url）。
    let trimmed = trimmed
        .strip_suffix("/chat/completions")
        .unwrap_or(trimmed)
        .trim_end_matches('/');
    let has_version_suffix = trimmed
        .rsplit('/')
        .next()
        .and_then(|segment| segment.strip_prefix('v'))
        .is_some_and(|version| !version.is_empty() && version.chars().all(|c| c.is_ascii_digit()));

    // 已經以 /v1、/v3、/v4 等版本路徑結尾時直接使用
    if has_version_suffix {
        trimmed.to_string()
    } else if trimmed.ends_with("/api") {
        // 豆包等：https://ark.cn-beijing.volces.com/api → 加 /v3
        format!("{}/v3", trimmed)
    } else {
        format!("{}/v1", trimmed)
    }
}

/// 從 chat completion 響應中提取文本
fn extract_chat_completion_text(data: &serde_json::Value) -> Option<String> {
    let content = data
        .get("choices")?
        .get(0)?
        .get("message")?
        .get("content")?;

    match content {
        serde_json::Value::String(s) => Some(s.trim().to_string()),
        serde_json::Value::Array(arr) => {
            let text: String = arr
                .iter()
                .filter_map(|item| {
                    if item.get("type")?.as_str()? == "text" {
                        item.get("text")?.as_str().map(String::from)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("");
            Some(text.trim().to_string())
        }
        _ => None,
    }
}

/// 去除 <think>...</think> 標籤
fn strip_thinking(text: &str) -> String {
    let re = regex::Regex::new(r"(?is)<think>.*?</think>").unwrap_or_else(|_| {
        // fallback: 不做处理
        regex::Regex::new(r"^$").unwrap()
    });
    let cleaned = re.replace_all(text, "");
    let cleaned = cleaned.trim();

    // 如果有"最終答案"標記，取其後面的內容
    if let Some(pos) = cleaned.find("最终答案") {
        let after = &cleaned[pos + "最终答案".len()..];
        let after = after.trim_start_matches(|c: char| c == ':' || c == '：' || c.is_whitespace());
        return after.trim().to_string();
    }

    cleaned.to_string()
}
#[cfg(test)]
mod tests {
    use super::*;

    fn config(provider: &str, api_url: &str, model: &str) -> AiProviderConfig {
        AiProviderConfig {
            provider: provider.to_string(),
            api_url: api_url.to_string(),
            api_key: "sk-test".to_string(),
            model: model.to_string(),
            extra: serde_json::Value::Null,
        }
    }

    fn keys(value: &serde_json::Value) -> Vec<String> {
        let mut out: Vec<String> = value
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        out.sort();
        out
    }

    #[test]
    fn known_providers_keep_their_exact_field() {
        // 這些組合是實測透過的，別順手升級成通吃集合——多注入未知欄位只會給
        // 本來好用的路徑引入 400 風險。
        let qwen = thinking_overrides(&config(
            "qwen",
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
            "qwen-plus",
        ))
        .expect("qwen should disable thinking");
        assert_eq!(keys(&qwen), vec!["enable_thinking"]);
        assert_eq!(qwen["enable_thinking"], serde_json::json!(false));

        for provider in ["deepseek", "mimo"] {
            let overrides = thinking_overrides(&config(provider, "https://example.com/v1", "m"))
                .expect("should disable thinking");
            assert_eq!(keys(&overrides), vec!["thinking"]);
            assert_eq!(overrides["thinking"]["type"], serde_json::json!("disabled"));
        }

        // 智譜走通用 provider，只能靠官方域名識別
        let zhipu = thinking_overrides(&config(
            "openai_compat",
            "https://open.bigmodel.cn/api/paas/v4",
            "glm-4",
        ))
        .expect("zhipu should disable thinking");
        assert_eq!(keys(&zhipu), vec!["thinking"]);
    }

    /// 使用者手填地址的幾種真實形態都要落到同一個端點上。
    ///
    /// 智譜那幾條是照 B 站反饋復現的：使用者填 `.../api/paas/v4` 曾經被補成 `/v4/v1`
    /// （已修），填 `.../api/paas` 會被補成 `/api/paas/v1`，而把文件裡的完整端點
    /// 整條粘進來會被再拼一次。三種都返回 404，而 404 只說路徑不存在。
    #[test]
    fn normalizes_user_typed_base_urls() {
        let cases = [
            // 智譜：版本段是 /v4，必須原樣保留
            ("https://open.bigmodel.cn/api/paas/v4", "https://open.bigmodel.cn/api/paas/v4"),
            ("https://open.bigmodel.cn/api/paas/v4/", "https://open.bigmodel.cn/api/paas/v4"),
            // 文件裡給的完整端點，整條粘進來
            (
                "https://open.bigmodel.cn/api/paas/v4/chat/completions",
                "https://open.bigmodel.cn/api/paas/v4",
            ),
            ("https://api.deepseek.com/v1/chat/completions", "https://api.deepseek.com/v1"),
            // 只給主機名 → 補 /v1（OpenAI 的形狀）
            ("https://api.deepseek.com", "https://api.deepseek.com/v1"),
            // 豆包：以 /api 結尾的補 /v3
            (
                "https://ark.cn-beijing.volces.com/api",
                "https://ark.cn-beijing.volces.com/api/v3",
            ),
            // 已帶 /v1 的原樣保留（Groq、MiMo 的預設地址就是這個形狀）
            ("https://api.groq.com/openai/v1", "https://api.groq.com/openai/v1"),
        ];
        for (input, expected) in cases {
            assert_eq!(normalize_base_url(input), expected, "input={}", input);
        }
    }

    /// 智譜的 `thinking.type=disabled` 兩條路都要認：內建卡靠 provider，
    /// 「OpenAI 相容 + 自己填智譜地址」的存量配置靠域名（那種配置不會自動遷移）。
    #[test]
    fn zhipu_disables_thinking_via_provider_and_via_host() {
        let by_provider = thinking_overrides(&config(
            "zhipu",
            "https://open.bigmodel.cn/api/paas/v4",
            "glm-4.7-flash",
        ))
        .expect("zhipu card should disable thinking");
        assert_eq!(keys(&by_provider), vec!["thinking"]);
        assert_eq!(by_provider["thinking"]["type"], serde_json::json!("disabled"));

        // 存量配置：provider 還是 openai_compat，只能靠域名認出來
        let by_host = thinking_overrides(&config(
            "openai_compat",
            "https://open.bigmodel.cn/api/paas/v4",
            "glm-4.7-flash",
        ))
        .expect("legacy zhipu config should disable thinking");
        assert_eq!(keys(&by_host), vec!["thinking"]);

        // 內建卡即便被改成中轉地址（域名對不上）也仍然按智譜的欄位發
        let relayed = thinking_overrides(&config("zhipu", "https://relay.example/v4", "glm-4.7"))
            .expect("relayed zhipu card should still disable thinking");
        assert_eq!(keys(&relayed), vec!["thinking"]);
    }

    /// 智譜 GLM-5.3 的真實錯誤體（2026-09-20 使用者實測）。
    ///
    /// 它是這條鏈路上最有教育意義的一個樣本：**整條訊息裡沒有 "thinking" 這個詞**，
    /// 所以舊判據（響應體裡有沒有我們注入的鍵名）一次都不命中 —— 降級不執行、
    /// 400 原樣拋給使用者、介面顯示「連線失敗」。這條測試就是釘住"按語義認"這件事。
    const ZHIPU_5_3_FORCED: &str =
        r#"{"error":{"code":"1210","message":"该模型始终思考，不支持关闭思考；请使用 low、high 或 max。"}}"#;

    #[test]
    fn zhipu_forced_thinking_error_is_recognized_without_the_word_thinking() {
        // 先確認前提：這個錯誤體確實不含 "thinking"，否則這條測試證明不了什麼
        assert!(
            !ZHIPU_5_3_FORCED.to_ascii_lowercase().contains("thinking"),
            "样本里出现了 thinking，这条测试的前提不成立了，请换一个真实样本"
        );
        // 舊判據對它無效 —— 這就是使用者看到「連線失敗」的直接原因
        let old_signal = rejected_thinking_field(
            ZHIPU_5_3_FORCED,
            &serde_json::json!({ "thinking": { "type": "disabled" } }),
        );
        assert!(old_signal.is_none(), "旧判据本来就认不出它");
        // 新判據必須認出來
        assert!(forced_thinking_error(ZHIPU_5_3_FORCED));
    }

    #[test]
    fn forced_thinking_error_does_not_fire_on_unrelated_failures() {
        // 這些都不能觸發降級重試，否則每次請求白髮兩遍
        for body in [
            r#"{"error":{"message":"Incorrect API key provided","code":"invalid_api_key"}}"#,
            r#"{"error":{"message":"The model `glm-9` does not exist","code":"model_not_found"}}"#,
            r#"{"error":{"message":"Rate limit reached","code":"rate_limit_exceeded"}}"#,
            r#"{"error":{"message":"余额不足","code":"1113"}}"#,
        ] {
            assert!(!forced_thinking_error(body), "不该命中: {}", body);
        }
    }

    /// 强制思考的模型要发「最低强度」，不是「什么都不发」。
    ///
    /// 直接不發引數會落到 GLM-5.3 的預設 `reasoning_effort=max`（深度推理），
    /// 對語音輸入是最差結果 —— 比不降級還慢。
    #[test]
    fn forced_thinking_models_get_low_effort_not_empty_params() {
        assert!(model_forces_thinking("glm-5.3"));
        assert!(model_forces_thinking("glm-5.3-flash"));
        assert!(model_forces_thinking("GLM-5.3-Flash"));
        assert!(!model_forces_thinking("glm-4.7-flash"));
        assert!(!model_forces_thinking("glm-4.7"));

        let overrides = thinking_overrides(&config(
            "zhipu",
            "https://open.bigmodel.cn/api/paas/v4",
            "glm-5.3",
        ))
        .expect("forced-thinking model still needs parameters");
        assert_eq!(keys(&overrides), vec!["reasoning_effort", "thinking"]);
        assert_eq!(overrides["thinking"]["type"], serde_json::json!("enabled"));
        assert_eq!(overrides["reasoning_effort"], serde_json::json!("low"));

        // 同一家的非強制模型仍然走 disabled（那是最快的一檔）
        let flash = thinking_overrides(&config(
            "zhipu",
            "https://open.bigmodel.cn/api/paas/v4",
            "glm-4.7-flash",
        ))
        .expect("glm-4.7-flash can disable thinking");
        assert_eq!(flash["thinking"]["type"], serde_json::json!("disabled"));
    }

    #[test]
    fn strict_openai_hosts_get_no_thinking_params() {
        // OpenAI / Azure 對未知頂層欄位直接 400，且 reasoning_effort 的合法取值依模型而異，
        // 猜不準就別猜——這兩家保持「一個欄位都不發」的現狀。
        for url in [
            "https://api.openai.com/v1",
            "https://my-resource.openai.azure.com/openai/deployments/gpt5",
            "https://my-resource.cognitiveservices.azure.com/openai/v1",
        ] {
            assert!(is_strict_openai_host(url), "{} should be strict", url);
            assert!(
                thinking_overrides(&config("openai_compat", url, "gpt-5")).is_none(),
                "{} should not receive thinking params",
                url
            );
        }
    }

    #[test]
    fn unknown_endpoints_get_the_catch_all_set() {
        // 這一檔是整個改動的理由：在此之前這些端點一個欄位都不發，模型預設思考就關不掉。
        for (provider, url) in [
            ("openai_compat", "https://openrouter.ai/api/v1"),
            ("openai_compat", "http://192.168.1.10:3000/v1"),
            ("doubao", "https://ark.cn-beijing.volces.com/api/v3"),
            ("groq", "https://api.groq.com/openai/v1"),
        ] {
            let overrides = thinking_overrides(&config(provider, url, "m"))
                .unwrap_or_else(|| panic!("{} should receive thinking params", url));
            assert_eq!(
                keys(&overrides),
                vec![
                    "enable_thinking",
                    "reasoning",
                    "reasoning_effort",
                    "thinking"
                ],
                "{}",
                url
            );
        }
    }

    #[test]
    fn merge_overrides_replaces_top_level_keys_only() {
        let mut body = serde_json::json!({
            "model": "m",
            "messages": [],
            "thinking": { "type": "enabled", "budget": 100 },
        });
        merge_overrides(&mut body, &disable_thinking_overrides());

        // 整體覆蓋，不深合併：殘留一個 budget 欄位會讓請求變成誰也說不清的狀態
        assert_eq!(body["thinking"], serde_json::json!({"type": "disabled"}));
        assert_eq!(body["model"], serde_json::json!("m"));
        assert!(body["messages"].is_array());
    }

    #[test]
    fn rejection_detection_only_fires_when_our_field_is_named() {
        let overrides = disable_thinking_overrides();

        let named = r#"{"error":{"message":"Unrecognized request argument supplied: thinking"}}"#;
        assert_eq!(
            rejected_thinking_field(named, &overrides).as_deref(),
            Some("thinking")
        );

        let unsupported_value =
            r#"{"error":{"code":"unsupported_value","param":"reasoning_effort"}}"#;
        assert!(rejected_thinking_field(unsupported_value, &overrides).is_some());

        // 金鑰錯、模型不存在這些 400 絕不能觸發降級重試，否則每次請求都白髮兩遍
        for body in [
            r#"{"error":{"message":"Incorrect API key provided"}}"#,
            r#"{"error":{"message":"The model `gpt-9` does not exist"}}"#,
            r#"{"error":{"message":"max_tokens is too small"}}"#,
        ] {
            assert!(
                rejected_thinking_field(body, &overrides).is_none(),
                "should not retry for: {}",
                body
            );
        }
    }

    #[test]
    fn rejection_cache_is_scoped_to_host_and_model() {
        // 用獨特的 host 名，避免和同程序裡其他測試共用那個全域性表時互相干擾
        let a = config("openai_compat", "https://cache-test.invalid/v1", "model-a");
        let b = config("openai_compat", "https://cache-test.invalid/v1", "model-b");

        assert!(remembered_thinking(&a).is_none());
        remember_thinking(&a, None);
        assert_eq!(remembered_thinking(&a), Some(None));
        // 同一閘道器換個模型結論可能不同（unsupported_value 是按模型判的），不能共用
        assert!(remembered_thinking(&b).is_none());
    }

    /// 快取要存「降級成什麼」，不只是「降級過」。
    ///
    /// 強制思考的模型降級目標是「最低強度」，別的端點是「什麼都不發」。舊結構是
    /// HashSet，只存得下後者 —— 於是 GLM-5.3 這類模型第二次請求會被當成"不發引數"，
    /// 落回它的預設 reasoning_effort=max，比不降級還慢。
    #[test]
    fn cache_remembers_which_fallback_worked() {
        let forced = config("zhipu", "https://cache-forced.invalid/v4", "glm-5.3");
        remember_thinking(&forced, Some(forced_thinking_overrides()));

        let remembered = remembered_thinking(&forced).expect("should be cached");
        let params = remembered.expect("forced-thinking endpoints still need parameters");
        assert_eq!(params["reasoning_effort"], serde_json::json!("low"));
        assert_eq!(params["thinking"]["type"], serde_json::json!("enabled"));
    }
}
