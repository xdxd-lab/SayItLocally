//! 网络来源的判定规则：哪些证书错误可以放行、哪些地址可以下载更新。
//!
//! 两条规则共用同一个前提 —— 「用户在服务器设置里保存的地址」与「官方地址」是仅有的两个可信来源。
//! 写成纯函数单独放，是为了能逐条钉测试：这里判错一次，要么自建用户连不上，要么官方域名被降级成不校验。

use reqwest::Url;

/// 内置官方地址。与 vite.config.ts 的 `SAYIT_DEFAULT_SERVER_URL` 同一个来源：
/// 打包时两边读同一个环境变量，不设就都是 sayitapp.site。
pub const OFFICIAL_SERVER_URL: &str = match option_env!("SAYIT_DEFAULT_SERVER_URL") {
    Some(value) => value,
    None => "https://sayitapp.site",
};

/// 服务器设置在数据库里的 key（前端 runtimeConfig.ts 的 BACKEND_BASE_URL_STORE_KEY）。
pub const BACKEND_BASE_URL_KEY: &str = "backendBaseUrl";

#[cfg_attr(not(windows), allow(dead_code))]
const OFFICIAL_DOMAIN: &str = "sayitapp.site";

/// 官方域名及其子域。这些主机**永远**严格校验证书，哪怕用户把服务器地址填成了它们。
#[cfg_attr(not(windows), allow(dead_code))]
pub fn is_official_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == OFFICIAL_DOMAIN || host.ends_with(&format!(".{OFFICIAL_DOMAIN}"))
}

fn parse(value: &str) -> Option<Url> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Url::parse(value).ok()
}

/// (主机, 端口)。wss 与 https 同端口默认值，ws 与 http 同端口默认值，
/// 所以证书放行时 `https://h` 和 `wss://h` 视为同一个目标。
#[cfg_attr(not(windows), allow(dead_code))]
fn host_port(url: &Url) -> Option<(String, u16)> {
    let host = url.host_str()?.to_ascii_lowercase();
    let port = url.port_or_known_default()?;
    Some((host, port))
}

/// 证书错误能不能放行。
///
/// 只有一种情况放行：请求的主机和端口正好是用户保存的服务器地址，那个地址是 https，
/// 且不是官方域名。这是给"自建服务器 + 自签名证书"留的口子（仓库的 Docker 镜像默认就是自签名）。
/// 其余一律拒绝 —— 这条原来是 `--ignore-certificate-errors`，对所有主机都不校验。
#[cfg_attr(not(windows), allow(dead_code))]
pub fn cert_error_allowed(request_uri: &str, configured_backend: &str) -> bool {
    let (Some(request), Some(configured)) = (parse(request_uri), parse(configured_backend)) else {
        return false;
    };
    if configured.scheme() != "https" {
        return false;
    }
    if !matches!(request.scheme(), "https" | "wss") {
        return false;
    }
    let (Some(target), Some(allowed)) = (host_port(&request), host_port(&configured)) else {
        return false;
    };
    !is_official_host(&allowed.0) && target == allowed
}

/// 更新包的下载地址是否可信：必须与用户保存的服务器地址同源，或与官方地址同源。
///
/// 官方地址也要放进来：自建服务器上没有 manifest 时前端会回落到官方 manifest
/// （见 updateChecker.ts 的 checkVersionUpdate），那时下载地址是官方的。
/// 只认 https，自建服务器用 http 时更新也只能走官方回落。
pub fn update_url_allowed(download_url: &str, configured_backend: &str) -> bool {
    let Some(url) = parse(download_url) else {
        return false;
    };
    if url.scheme() != "https" {
        return false;
    }
    let candidates = [parse(configured_backend), parse(OFFICIAL_SERVER_URL)];
    candidates
        .iter()
        .flatten()
        .any(|base| base.scheme() == "https" && base.origin() == url.origin())
}

/// [自訂] S34／S38／S42／S22：AI 只允許「本機 Ollama」。
///
/// 位址必須是 http(s)，主機必須**正好**是 127.0.0.1、localhost 或 ::1（以解析後的主機判斷，
/// 不用字串比對，所以 `http://127.0.0.1@evil.example`、`http://127.0.0.1.evil.example` 都不會過）。
/// 不允許帳號密碼段。這是 Rust 端的強制檢查，前端預設值、托盤、快捷鍵、匯入都改不掉它。
pub fn is_loopback_ollama_url(api_url: &str) -> bool {
    let Some(url) = parse(api_url) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    // host_str() 是解析後的主機；IPv6 會帶方括號（"[::1]"），先去掉再當 IP 解析。
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    match bare.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip == std::net::Ipv4Addr::LOCALHOST,
        Ok(std::net::IpAddr::V6(ip)) => ip == std::net::Ipv6Addr::LOCALHOST,
        Err(_) => false,
    }
}

/// [自訂] S39：WebView 只能導覽到 App 自己的來源。
///
/// 允許：`tauri://localhost`（macOS 正式版）、`http(s)://tauri.localhost`（Windows 正式版）、
/// `about:blank`（WebKit 初始空白頁，不含任何遠端內容），以及 debug 建置時的 devUrl 同源。
/// 其他一律拒絕（含拖放連結到視窗造成的導覽）。
pub fn app_navigation_allowed(url: &Url, dev_url: Option<&Url>) -> bool {
    match url.scheme() {
        "tauri" => return url.host_str() == Some("localhost"),
        // 只認預設埠：WebKit 會把 *.localhost 解析到本機，帶埠號的就可能是本機其他服務
        "http" | "https" if url.host_str() == Some("tauri.localhost") => return url.port().is_none(),
        "about" => return url.path() == "blank",
        _ => {}
    }
    match dev_url {
        Some(dev) => cfg!(debug_assertions) && url.origin() == dev.origin(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // [自訂] S34／S38：AI 只允許本機 Ollama
    #[test]
    fn only_loopback_ollama_urls_are_allowed() {
        assert!(is_loopback_ollama_url("http://127.0.0.1:11434"));
        assert!(is_loopback_ollama_url("http://localhost:11434/api/generate"));
        assert!(is_loopback_ollama_url("http://[::1]:11434"));
        assert!(!is_loopback_ollama_url(""));
        assert!(!is_loopback_ollama_url("http://127.0.0.2:11434"));
        assert!(!is_loopback_ollama_url("http://192.168.1.10:11434"));
        assert!(!is_loopback_ollama_url("http://127.0.0.1.evil.example:11434"));
        assert!(!is_loopback_ollama_url("http://127.0.0.1@evil.example:11434"));
        assert!(!is_loopback_ollama_url("http://user:pw@127.0.0.1:11434"));
        assert!(!is_loopback_ollama_url("https://api.deepseek.com"));
        assert!(!is_loopback_ollama_url("ftp://127.0.0.1"));
        assert!(!is_loopback_ollama_url("localhost:11434"));
    }

    // [自訂] S39：導覽防護
    #[test]
    fn navigation_is_limited_to_the_app_origin() {
        let u = |s: &str| Url::parse(s).unwrap();
        assert!(app_navigation_allowed(&u("tauri://localhost/index.html"), None));
        assert!(app_navigation_allowed(&u("http://tauri.localhost/"), None));
        assert!(app_navigation_allowed(&u("about:blank"), None));
        assert!(!app_navigation_allowed(&u("https://sayitapp.site/feedback.html"), None));
        assert!(!app_navigation_allowed(&u("https://tauri.localhost.evil.example/"), None));
        assert!(!app_navigation_allowed(&u("http://tauri.localhost:8080/"), None));
        assert!(!app_navigation_allowed(&u("tauri://evil/"), None));
        assert!(!app_navigation_allowed(&u("file:///etc/passwd"), None));
        assert!(!app_navigation_allowed(&u("http://localhost:1420/"), None));
        let dev = u("http://localhost:1420");
        assert_eq!(
            app_navigation_allowed(&u("http://localhost:1420/overlay.html"), Some(&dev)),
            cfg!(debug_assertions)
        );
        assert!(!app_navigation_allowed(&u("http://localhost:1421/"), Some(&dev)));
    }

    #[test]
    fn official_hosts_are_recognized() {
        assert!(is_official_host("sayitapp.site"));
        assert!(is_official_host("dev.sayitapp.site"));
        assert!(is_official_host("DEV.SayItApp.site."));
        assert!(!is_official_host("sayitapp.site.attacker.example"));
        assert!(!is_official_host("notsayitapp.site"));
    }

    #[test]
    fn self_signed_custom_server_is_allowed_for_https_and_wss() {
        let cfg = "https://192.168.1.10:8443";
        assert!(cert_error_allowed("https://192.168.1.10:8443/healthz", cfg));
        assert!(cert_error_allowed("wss://192.168.1.10:8443/ws/transcribe", cfg));
    }

    #[test]
    fn default_ports_match_between_https_and_wss() {
        let cfg = "https://asr.example.com";
        assert!(cert_error_allowed("wss://asr.example.com/ws/transcribe", cfg));
        assert!(cert_error_allowed("https://asr.example.com:443/x", cfg));
    }

    #[test]
    fn other_hosts_and_ports_are_rejected() {
        let cfg = "https://192.168.1.10:8443";
        assert!(!cert_error_allowed("https://192.168.1.10:9443/", cfg));
        assert!(!cert_error_allowed("https://192.168.1.11:8443/", cfg));
        assert!(!cert_error_allowed("https://fonts.googleapis.com/", cfg));
    }

    #[test]
    fn official_domain_is_never_downgraded() {
        assert!(!cert_error_allowed("https://sayitapp.site/api", "https://sayitapp.site"));
        assert!(!cert_error_allowed("wss://dev.sayitapp.site/ws", "https://dev.sayitapp.site"));
    }

    #[test]
    fn http_or_empty_configuration_allows_nothing() {
        assert!(!cert_error_allowed("https://10.0.0.2/", "http://10.0.0.2"));
        assert!(!cert_error_allowed("https://10.0.0.2/", ""));
        assert!(!cert_error_allowed("not a url", "https://10.0.0.2"));
    }

    #[test]
    fn update_url_must_share_origin() {
        let cfg = "https://my.server:8443";
        assert!(update_url_allowed("https://my.server:8443/api/desktop-updates/win32/x64/a.exe", cfg));
        assert!(update_url_allowed("https://sayitapp.site/api/desktop-updates/win32/x64/a.exe", cfg));
        // 拼接攻击：`${base}.attacker.example/x.exe`
        assert!(!update_url_allowed("https://sayitapp.site.attacker.example/x.exe", "https://sayitapp.site"));
        assert!(!update_url_allowed("https://my.server:9443/a.exe", cfg));
        assert!(!update_url_allowed("http://sayitapp.site/a.exe", "https://sayitapp.site"));
        assert!(!update_url_allowed("https://user@evil.example/a.exe", cfg));
    }

    #[test]
    fn http_custom_server_can_only_use_official_updates() {
        let cfg = "http://10.0.0.2:8000";
        assert!(!update_url_allowed("http://10.0.0.2:8000/a.exe", cfg));
        assert!(update_url_allowed("https://sayitapp.site/a.exe", cfg));
    }
}
