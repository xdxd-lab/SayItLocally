// 所有 provider 共用的 HTTP 客户端。
//
// 為什麼一定要帶 User-Agent：**reqwest 預設一個 UA 都不發**，而 nginx / WAF 類閘道器
// 普遍把「無 UA」當爬蟲攔掉。實測 https://test-ai-api.nwcdcloud.cn/v1（LiteLLM 閘道器）：
// 同一個地址 + 金鑰 + 模型，帶 UA 返回 200，去掉 UA 頭一律 403 並回一段 nginx 的 HTML
// 錯誤頁 —— 請求在閘道器就被拒了，鑑權根本沒執行。我們自己的 ALB 上有同一條規則
// （見 pitfalls 第 1 條）。這類 403 的響應裡一個字都不提 UA，所以只會被歸成
// 「地區不可用 / 模型沒開通」，使用者換金鑰、換模型都修不掉。
//
// 因此：provider 裡**不要再寫 `reqwest::Client::new()`**，一律用這裡的單例。
// 附帶好處是連線池和 TLS 會話得以複用，不必每次呼叫重建一個客戶端。
pub const USER_AGENT: &str = concat!("SayIt/", env!("CARGO_PKG_VERSION"));

static SHARED: once_cell::sync::Lazy<reqwest::Client> = once_cell::sync::Lazy::new(|| {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        // 構建失敗只可能是 TLS 後端初始化不了，此時退回預設客戶端也不會更糟，
        // 至少不要讓整個轉寫功能因為拿不到客戶端而不可用。
        .unwrap_or_else(|_| reqwest::Client::new())
});

/// 帶 User-Agent 的共享客戶端。超時仍按各呼叫點的 `.timeout()` 單獨設定。
pub fn shared() -> &'static reqwest::Client {
    &SHARED
}
