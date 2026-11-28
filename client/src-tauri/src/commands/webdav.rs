// WebDAV 備份：把備份包上傳到使用者自己的 WebDAV 伺服器，並支援從上面恢復。
//
// 設計要點：
// - 打包**不在這裡實現**，複用 `backup::write_backup_archive`。一旦兩邊各寫一套，
//   就會出現「本地匯出能恢復、雲端備份恢復不了」這種只在換機時才暴露的問題。
// - 備份檔位：配置必備，歷史與音訊可選，預設都不含（見 `scope_from_settings`）。
//   只含配置的包也是合法的全量 zip，用現有的 `apply_full_backup` 就能恢復。
// - 只允許 https。WebDAV 用 HTTP Basic 認證，憑證隨每個請求明文傳送。
// - 先 PUT 到 `<名字>.part` 再 MOVE 成正式名：網路斷在中途留下的半個 zip
//   不能長得像一個有效備份（和本地匯出的 .part 再發布是同一個理由）。
// - 上傳完必須 PROPFIND 回讀一次並比對位元組數。「伺服器收下了但存壞了/沒存」
//   和「上傳成功」在客戶端看來一模一樣，不回讀就分不出來。
// - 每次備份的結果（成功或失敗）都寫進 `webdav.lastResult` 並落日誌。
//   一個你以為有、其實幾個月前就在靜默失敗的備份，比沒有備份更糟。

use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};
use tokio_util::io::ReaderStream;

use crate::commands::backup::{apply_full_backup, write_backup_archive, BackupScope};
use crate::providers::{diag, http_client};
use crate::storage::Storage;

const KEY_URL: &str = "webdav.url";
const KEY_USERNAME: &str = "webdav.username";
const KEY_PASSWORD: &str = "webdav.password";
const KEY_INCLUDE_HISTORY: &str = "webdav.includeHistory";
const KEY_INCLUDE_AUDIO: &str = "webdav.includeAudio";
const KEY_KEEP_COUNT: &str = "webdav.keepCount";
const KEY_LAST_RESULT: &str = "webdav.lastResult";

/// 備份檔名字首，與本地匯出一致，便於使用者對著看。
const BACKUP_PREFIX: &str = "sayit-backup-";
const DEFAULT_KEEP_COUNT: u64 = crate::defaults::WEBDAV_KEEP_COUNT;
const PROGRESS_EVENT: &str = "webdav-backup-progress";

/// 後設資料類請求（PROPFIND / MKCOL / MOVE / DELETE）的超時。
const META_TIMEOUT: Duration = Duration::from_secs(30);

// ─── 配置 ───

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDavConfig {
    pub url: String,
    pub username: String,
    pub password: String,
}

/// 已校驗的連線資訊。`dir_url` 一定是 https 且不帶末尾斜槓。
struct Target {
    dir_url: String,
    username: String,
    password: String,
}

/// 手寫 Debug 而不是 derive：`#[derive(Debug)]` 會讓任何一次 `{:?}`（包括
/// `unwrap_err()` 的 panic 訊息、以後隨手加的一條日誌）把網盤密碼打進 sayit.log，
/// 而日誌是會跟著診斷包發出來的。
impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("dir_url", &self.dir_url)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// 校驗並規範化目錄 URL。
///
/// 只接受 https：Basic 認證等於把網盤密碼隨每個請求明文發出去，走 http 就是把它
/// 廣播到鏈路上的每一跳。堅果雲等服務本身也只提供 https，所以這裡不留開關。
///
/// 返回的錯誤是**錯誤碼**而不是文案：這幾種都是可預期的使用者輸入問題，交給前端本地化
/// 才能給出針對性的提示（比如堅果雲要用「應用密碼」而不是登入密碼）。
fn normalize_dir_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("WEBDAV_URL_EMPTY".to_string());
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://") {
        return Err("WEBDAV_URL_INSECURE".to_string());
    }
    if !lower.starts_with("https://") {
        return Err("WEBDAV_URL_SCHEME".to_string());
    }
    // 末尾斜槓去掉，拼子路徑時統一再加，避免出現 `//`（部分伺服器會 404）。
    let normalized = trimmed.trim_end_matches('/');
    if normalized.len() <= "https://".len() {
        return Err("WEBDAV_URL_EMPTY".to_string());
    }
    Ok(normalized.to_string())
}

impl Target {
    fn from_config(config: &WebDavConfig) -> Result<Self, String> {
        let dir_url = normalize_dir_url(&config.url)?;
        if config.username.trim().is_empty() || config.password.is_empty() {
            return Err("WEBDAV_CREDENTIALS_EMPTY".to_string());
        }
        Ok(Self {
            dir_url,
            username: config.username.trim().to_string(),
            password: config.password.clone(),
        })
    }

    fn from_settings(storage: &Storage) -> Result<Self, String> {
        Self::from_config(&WebDavConfig {
            url: setting_string(storage, KEY_URL),
            username: setting_string(storage, KEY_USERNAME),
            password: setting_string(storage, KEY_PASSWORD),
        })
    }

    fn child(&self, name: &str) -> String {
        format!("{}/{}", self.dir_url, name)
    }
}

fn setting_string(storage: &Storage, key: &str) -> String {
    storage
        .get(key, None)
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn setting_bool(storage: &Storage, key: &str, default: bool) -> bool {
    storage.get(key, None).as_bool().unwrap_or(default)
}

/// 備份檔位一律從設定讀，不接受呼叫方傳參。
///
/// 手動「立即備份」和後台定時備份必須打出一樣的包 —— 兩個入口各帶一份 scope 的話，
/// 「介面上勾了音訊但自動備份裡沒有」這類問題會很難查。
///
/// 兩個預設值都是 false：讀不到設定時寧可打一個只含配置的小包，也不要悄悄上傳
/// 幾個 GB 音訊。
fn scope_from_settings(storage: &Storage) -> BackupScope {
    BackupScope {
        include_history: setting_bool(storage, KEY_INCLUDE_HISTORY, crate::defaults::WEBDAV_INCLUDE_HISTORY),
        include_audio: setting_bool(storage, KEY_INCLUDE_AUDIO, crate::defaults::WEBDAV_INCLUDE_AUDIO),
    }
}

// ─── HTTP ───

fn dav_method(verb: &'static str) -> reqwest::Method {
    reqwest::Method::from_bytes(verb.as_bytes()).expect("WebDAV verb is static ASCII")
}

/// 構造一個帶 UA 與 Basic 認證的請求。
///
/// 必須走 `http_client::shared()`：reqwest 預設不發 User-Agent，而 nginx / WAF 類
/// 閘道器普遍按「無 UA」攔（見 pitfalls 第 1 條）。被攔掉的響應裡一個字都不提 UA，
/// 只會表現成「密碼錯誤」或「地址不對」，幾乎不可能猜到。
fn request(target: &Target, verb: &'static str, url: &str) -> reqwest::RequestBuilder {
    http_client::shared()
        .request(dav_method(verb), url)
        .basic_auth(&target.username, Some(&target.password))
}

/// 把 HTTP 層面的失敗翻成一條能定位問題的錯誤。
///
/// 401/403 單獨給碼：堅果雲上這兩個幾乎都是「用了登入密碼而不是應用密碼」，
/// 前端要能給出針對性提示。其餘帶上狀態碼和一小段響應體 —— 用 `diag::truncate`
/// 按字元截斷，直接切位元組會在中文錯誤體上 panic（見 pitfalls 第 14 條）。
async fn check_status(response: reqwest::Response, stage: &str) -> Result<reqwest::Response, String> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        log::warn!("[webdav] {} rejected: http={}", stage, status.as_u16());
        return Err("WEBDAV_UNAUTHORIZED".to_string());
    }
    let body = response.text().await.unwrap_or_default();
    let detail = diag::truncate(&body, 200);
    log::warn!(
        "[webdav] {} failed: http={} body={}",
        stage,
        status.as_u16(),
        detail
    );
    Err(format!("{} failed (HTTP {}) {}", stage, status.as_u16(), detail))
}

async fn send(target: &Target, verb: &'static str, url: &str, stage: &str) -> Result<reqwest::Response, String> {
    let response = request(target, verb, url)
        .timeout(META_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("{} failed: {}", stage, diag::truncate(&e.to_string(), 200)))?;
    check_status(response, stage).await
}

/// PROPFIND 一個目錄，返回原始 XML。
///
/// `Depth: 1` 是硬要求，不是偷懶：堅果雲只支援 depth=1，發 `infinity` 會被拒。
/// 我們也只需要列一層目錄。
async fn propfind_dir(target: &Target, url: &str) -> Result<String, String> {
    let response = request(target, "PROPFIND", url)
        .header("Depth", "1")
        .header("Content-Type", "application/xml; charset=utf-8")
        .timeout(META_TIMEOUT)
        .body(
            r#"<?xml version="1.0" encoding="utf-8"?>
<propfind xmlns="DAV:"><prop><getcontentlength/><getlastmodified/><resourcetype/></prop></propfind>"#,
        )
        .send()
        .await
        .map_err(|e| format!("PROPFIND failed: {}", diag::truncate(&e.to_string(), 200)))?;
    let response = check_status(response, "PROPFIND").await?;
    response
        .text()
        .await
        .map_err(|e| format!("Failed to read the directory listing: {}", e))
}

enum MkcolOutcome {
    Done,
    ParentMissing,
}

async fn mkcol(target: &Target, url: &str) -> Result<MkcolOutcome, String> {
    let response = request(target, "MKCOL", url)
        .timeout(META_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("MKCOL failed: {}", diag::truncate(&e.to_string(), 200)))?;
    let status = response.status();
    // 405 = 該路徑上已經有東西了。是不是目錄留給後續 PROPFIND 去發現。
    if status.is_success() || status == reqwest::StatusCode::METHOD_NOT_ALLOWED {
        return Ok(MkcolOutcome::Done);
    }
    if status == reqwest::StatusCode::CONFLICT {
        return Ok(MkcolOutcome::ParentMissing);
    }
    check_status(response, "MKCOL").await.map(|_| MkcolOutcome::Done)
}

/// 把路徑的各級祖先收集出來（從深到淺），到 `https://host` 就停。
///
/// 停止條件是「斜槓數 <= 2」：`https://` 自帶兩個，再少就意味著已經把主機名切掉了。
fn ancestor_urls(url: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = url.to_string();
    // 8 級足夠覆蓋任何合理的網盤路徑，同時給一個明確的上界。
    for _ in 0..8 {
        let Some((parent, _)) = cursor.rsplit_once('/') else {
            break;
        };
        if parent.matches('/').count() <= 2 {
            break;
        }
        out.push(parent.to_string());
        cursor = parent.to_string();
    }
    out
}

/// 確保目錄存在。已存在（405）與建立成功（201）都算成功。
///
/// 父目錄缺失（409）時把祖先從淺到深補建一遍，這樣使用者填一個多級路徑也能一次配好。
/// 寫成迴圈而不是 async 遞迴：遞迴要裝箱，而層級本來就有上界。
async fn ensure_collection(target: &Target, url: &str) -> Result<(), String> {
    if let MkcolOutcome::Done = mkcol(target, url).await? {
        return Ok(());
    }
    // 補建祖先是**盡力而為**，失敗不算失敗：堅果雲不允許在 `/dav` 根上 MKCOL，
    // 會回一個錯誤碼，但那不代表目標目錄建不出來。只有目標自己的結果算數。
    for ancestor in ancestor_urls(url).iter().rev() {
        if let Err(error) = mkcol(target, ancestor).await {
            log::info!(
                "[webdav] could not create parent {}: {}",
                ancestor,
                diag::truncate(&error, 120)
            );
        }
    }
    match mkcol(target, url).await? {
        MkcolOutcome::Done => Ok(()),
        MkcolOutcome::ParentMissing => Err("WEBDAV_MKCOL_CONFLICT".to_string()),
    }
}

// ─── PROPFIND 響應解析 ───

/// `xml[at]` 處是否為本地名等於 `local` 的標籤起始；返回名字結束後的下標。
///
/// 忽略名稱空間字首：同一份響應裡各家伺服器分別用 `d:`、`D:`、`lp1:` 或乾脆不帶字首，
/// 按字首匹配必然漏。
fn tag_name_end(xml: &str, at: usize, local: &str, closing: bool) -> Option<usize> {
    let rest = xml.get(at..)?;
    let mut cur = rest.strip_prefix('<')?;
    let mut consumed = 1;
    if closing {
        cur = cur.strip_prefix('/')?;
        consumed += 1;
    }
    let name_len = cur
        .find(|c: char| c == '>' || c == '/' || c.is_whitespace())
        .unwrap_or(cur.len());
    let name = &cur[..name_len];
    let local_name = name.rsplit(':').next().unwrap_or(name);
    if local_name.eq_ignore_ascii_case(local) {
        Some(at + consumed + name_len)
    } else {
        None
    }
}

/// 取出所有本地名為 `local` 的元素的內容（含巢狀的原始 XML）。
///
/// 不引 XML 解析庫：PROPFIND 的響應結構固定，只需要 href 和 getcontentlength 兩個
/// 葉子值，一個幾十行的掃描器比配一個解析器更好讀，也更容易釘住各家的字首差異。
fn extract_elements(xml: &str, local: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = xml[cursor..].find('<') {
        let open = cursor + rel;
        let Some(name_end) = tag_name_end(xml, open, local, false) else {
            cursor = open + 1;
            continue;
        };
        let Some(gt_rel) = xml[name_end..].find('>') else {
            break;
        };
        let tag_end = name_end + gt_rel;
        if xml[name_end..tag_end].trim_end().ends_with('/') {
            cursor = tag_end + 1; // 自閉合，沒有文本
            continue;
        }
        let content_start = tag_end + 1;
        let mut scan = content_start;
        let mut close = None;
        while let Some(r) = xml[scan..].find('<') {
            let candidate = scan + r;
            if tag_name_end(xml, candidate, local, true).is_some() {
                close = Some(candidate);
                break;
            }
            scan = candidate + 1;
        }
        let Some(close) = close else { break };
        out.push(xml[content_start..close].trim().to_string());
        cursor = close + 1;
    }
    out
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDavEntry {
    pub name: String,
    pub size: u64,
}

/// 從 PROPFIND 響應裡挑出我們自己的備份檔案。
///
/// 名字裡帶的時間戳是零填充的，所以按名字倒序就是按時間從新到舊，不必解析日期。
fn parse_backup_entries(xml: &str) -> Vec<WebDavEntry> {
    let mut entries: Vec<WebDavEntry> = extract_elements(xml, "response")
        .iter()
        .filter_map(|block| {
            let href = extract_elements(block, "href").into_iter().next()?;
            let name = href
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string();
            if !name.starts_with(BACKUP_PREFIX) || !name.ends_with(".zip") {
                return None;
            }
            let size = extract_elements(block, "getcontentlength")
                .into_iter()
                .next()
                .and_then(|value| value.trim().parse::<u64>().ok())
                .unwrap_or(0);
            Some(WebDavEntry { name, size })
        })
        .collect();
    entries.sort_by(|a, b| b.name.cmp(&a.name));
    entries.dedup_by(|a, b| a.name == b.name);
    entries
}

// ─── 進度 ───

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebDavProgress {
    status: String,
    /// 'preparing' | 'packingData' | 'packingAudio' | 'finalizing' | 'uploading' | 'verifying' | 'completed' | 'failed'
    phase: String,
    file_name: String,
    current_file: Option<String>,
    processed_bytes: u64,
    total_bytes: u64,
    percent: f64,
    error: Option<String>,
}

fn emit_progress(app: &AppHandle, progress: WebDavProgress) {
    let _ = app.emit(PROGRESS_EVENT, progress);
}

/// 打包占前一半進度，上傳佔後一半。打包階段的百分比是 2~98，壓到 0~50。
fn packing_percent(archive_percent: f64) -> f64 {
    (archive_percent * 0.5).clamp(0.0, 50.0)
}

fn upload_percent(sent: u64, total: u64) -> f64 {
    if total == 0 {
        return 90.0;
    }
    (50.0 + sent as f64 / total as f64 * 40.0).min(90.0)
}

// ─── 命令 ───

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDavBackupResult {
    pub file_name: String,
    pub bytes: u64,
    pub include_history: bool,
    pub include_audio: bool,
    pub finished_at: i64,
    pub pruned: usize,
}

/// 連線測試：PROPFIND 目標目錄，目錄不存在就先建。
///
/// 走的是和真實備份完全相同的動詞與認證方式，避免「測試透過但備份失敗」。
#[tauri::command]
pub async fn webdav_test(config: WebDavConfig) -> Result<u64, String> {
    let target = Target::from_config(&config)?;
    ensure_collection(&target, &target.dir_url).await?;
    let xml = propfind_dir(&target, &target.dir_url).await?;
    let count = parse_backup_entries(&xml).len() as u64;
    log::info!(
        "[webdav] connection test ok, existing backups={}",
        count
    );
    Ok(count)
}

/// 列出伺服器上已有的備份，供恢復時選擇。
#[tauri::command]
pub async fn webdav_list(storage: State<'_, Storage>) -> Result<Vec<WebDavEntry>, String> {
    let target = Target::from_settings(storage.inner())?;
    let xml = propfind_dir(&target, &target.dir_url).await?;
    Ok(parse_backup_entries(&xml))
}

/// 立即備份一次。手動按鈕與後台定時任務共用這一個入口。
#[tauri::command]
pub async fn webdav_backup_now(
    app: AppHandle,
    storage: State<'_, Storage>,
) -> Result<WebDavBackupResult, String> {
    // [自訂] SC-56：記憶體庫不備份，也不記錄這次結果。
    storage.ensure_exportable()?;
    let scope = scope_from_settings(storage.inner());
    let file_name = format!(
        "{}{}.zip",
        BACKUP_PREFIX,
        chrono::Local::now().format("%Y-%m-%d_%H-%M-%S-%3f")
    );
    let temp_path = std::env::temp_dir().join(format!("sayit-webdav-{}.zip", uuid::Uuid::new_v4()));

    let outcome = run_backup(&app, storage.inner(), scope, &file_name, &temp_path).await;

    // 打包用的臨時檔案不留在磁碟上：含音訊時它和音訊庫一樣大。
    let _ = std::fs::remove_file(&temp_path);

    // 成功與失敗都記帳。少了失敗那一半，一個靜默失敗幾個月的備份看起來
    // 和一個正常工作的備份完全一樣。
    let finished_at = chrono::Utc::now().timestamp_millis();
    match &outcome {
        Ok(result) => {
            let _ = storage.set(
                KEY_LAST_RESULT,
                &json!({
                    "at": finished_at,
                    "ok": true,
                    "fileName": result.file_name,
                    "bytes": result.bytes,
                    "includeHistory": result.include_history,
                    "includeAudio": result.include_audio,
                    "error": Value::Null,
                }),
            );
            log::info!(
                "[webdav] backup ok name={} bytes={} history={} audio={} pruned={}",
                result.file_name,
                result.bytes,
                result.include_history,
                result.include_audio,
                result.pruned
            );
            emit_progress(
                &app,
                WebDavProgress {
                    status: "completed".to_string(),
                    phase: "completed".to_string(),
                    file_name: result.file_name.clone(),
                    current_file: None,
                    processed_bytes: result.bytes,
                    total_bytes: result.bytes,
                    percent: 100.0,
                    error: None,
                },
            );
        }
        Err(error) => {
            let _ = storage.set(
                KEY_LAST_RESULT,
                &json!({
                    "at": finished_at,
                    "ok": false,
                    "fileName": file_name,
                    "bytes": 0,
                    "includeHistory": scope.include_history,
                    "includeAudio": scope.include_audio,
                    "error": diag::truncate(error, 300),
                }),
            );
            log::warn!("[webdav] backup failed name={} error={}", file_name, diag::truncate(error, 300));
            emit_progress(
                &app,
                WebDavProgress {
                    status: "failed".to_string(),
                    phase: "failed".to_string(),
                    file_name: file_name.clone(),
                    current_file: None,
                    processed_bytes: 0,
                    total_bytes: 0,
                    percent: 0.0,
                    error: Some(error.clone()),
                },
            );
        }
    }

    outcome
}

async fn run_backup(
    app: &AppHandle,
    storage: &Storage,
    scope: BackupScope,
    file_name: &str,
    temp_path: &PathBuf,
) -> Result<WebDavBackupResult, String> {
    // 先校驗連線資訊再打包：地址填錯時沒必要先花幾分鐘壓幾個 GB。
    let target = Target::from_settings(storage)?;

    {
        let progress_app = app.clone();
        let progress_name = file_name.to_string();
        write_backup_archive(storage, scope, temp_path, &mut |progress| {
            emit_progress(
                &progress_app,
                WebDavProgress {
                    status: "running".to_string(),
                    phase: progress.phase.to_string(),
                    file_name: progress_name.clone(),
                    current_file: progress.current_file.map(ToString::to_string),
                    processed_bytes: progress.processed_bytes,
                    total_bytes: progress.total_bytes,
                    percent: packing_percent(progress.percent),
                    error: None,
                },
            );
        })?;
    }

    let local_size = std::fs::metadata(temp_path)
        .map_err(|e| format!("Failed to read the packaged backup: {}", e))?
        .len();

    ensure_collection(&target, &target.dir_url).await?;

    // 先傳成 .part，成功後再 MOVE 成正式名。斷線留下的半個包不會被當成有效備份。
    let temp_remote = target.child(&format!("{}.part", file_name));
    let final_remote = target.child(file_name);
    upload_file(app, &target, &temp_remote, temp_path, local_size, file_name).await?;
    move_file(&target, &temp_remote, &final_remote).await?;

    emit_progress(
        app,
        WebDavProgress {
            status: "running".to_string(),
            phase: "verifying".to_string(),
            file_name: file_name.to_string(),
            current_file: None,
            processed_bytes: local_size,
            total_bytes: local_size,
            percent: 95.0,
            error: None,
        },
    );

    // 回讀確認。伺服器「收下了但沒存」「存成了 0 位元組」和「上傳成功」在客戶端
    // 看來完全一樣，不比對位元組數就分不出來。
    let xml = propfind_dir(&target, &target.dir_url).await?;
    let entries = parse_backup_entries(&xml);
    let uploaded = entries
        .iter()
        .find(|entry| entry.name == file_name)
        .ok_or_else(|| "WEBDAV_VERIFY_MISSING".to_string())?;
    // 有的伺服器不報 getcontentlength（返回 0），那就只能確認檔案在。
    if uploaded.size != 0 && uploaded.size != local_size {
        log::warn!(
            "[webdav] size mismatch name={} local={} remote={}",
            file_name,
            local_size,
            uploaded.size
        );
        return Err("WEBDAV_VERIFY_SIZE".to_string());
    }

    let pruned = prune_old_backups(&target, storage, &entries, file_name).await;

    Ok(WebDavBackupResult {
        file_name: file_name.to_string(),
        bytes: local_size,
        include_history: scope.include_history,
        include_audio: scope.include_audio,
        finished_at: chrono::Utc::now().timestamp_millis(),
        pruned,
    })
}

async fn upload_file(
    app: &AppHandle,
    target: &Target,
    url: &str,
    path: &PathBuf,
    total: u64,
    file_name: &str,
) -> Result<(), String> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("Failed to open the packaged backup: {}", e))?;

    let progress_app = app.clone();
    let progress_name = file_name.to_string();
    let mut sent = 0u64;
    // 進度按時間節流：256KB 一塊，幾個 GB 的包會產生上萬次事件，全發出去
    // 光是序列化就夠拖慢上傳。
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    let stream = ReaderStream::with_capacity(file, 256 * 1024).map_ok(move |chunk| {
        sent += chunk.len() as u64;
        if last_emit.elapsed() >= Duration::from_millis(200) {
            emit_progress(
                &progress_app,
                WebDavProgress {
                    status: "running".to_string(),
                    phase: "uploading".to_string(),
                    file_name: progress_name.clone(),
                    current_file: None,
                    processed_bytes: sent,
                    total_bytes: total,
                    percent: upload_percent(sent, total),
                    error: None,
                },
            );
            last_emit = Instant::now();
        }
        chunk
    });

    let response = request(target, "PUT", url)
        .header("Content-Type", "application/zip")
        .header(reqwest::header::CONTENT_LENGTH, total)
        // 上傳不能用固定超時：幾個 GB 的包在慢上行鏈路上要幾小時。按體積給一個
        // 下限約 50KB/s 的預算，既不會把正常上傳掐斷，也不會永遠掛著。
        .timeout(Duration::from_secs(120 + total / 50_000))
        .body(reqwest::Body::wrap_stream(stream))
        .send()
        .await
        .map_err(|e| format!("Upload failed: {}", diag::truncate(&e.to_string(), 200)))?;
    check_status(response, "PUT").await.map(|_| ())
}

async fn move_file(target: &Target, from: &str, to: &str) -> Result<(), String> {
    let response = request(target, "MOVE", from)
        .header("Destination", to)
        .header("Overwrite", "T")
        .timeout(META_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("MOVE failed: {}", diag::truncate(&e.to_string(), 200)))?;
    check_status(response, "MOVE").await.map(|_| ())
}

/// 只保留最近 N 份。刪不掉不算備份失敗 —— 包已經安全落地了，清理失敗最多是佔空間。
async fn prune_old_backups(
    target: &Target,
    storage: &Storage,
    entries: &[WebDavEntry],
    just_uploaded: &str,
) -> usize {
    let keep = storage
        .get(KEY_KEEP_COUNT, None)
        .as_u64()
        .filter(|value| *value >= 1)
        .unwrap_or(DEFAULT_KEEP_COUNT) as usize;

    // entries 是按名字倒序（= 時間從新到舊）的，但剛上傳的這一份可能還沒出現在
    // 列表裡（回讀發生在 MOVE 之後，正常都在），所以顯式合併一次再排序。
    let mut names: Vec<String> = entries.iter().map(|entry| entry.name.clone()).collect();
    if !names.iter().any(|name| name == just_uploaded) {
        names.push(just_uploaded.to_string());
    }
    names.sort_by(|a, b| b.cmp(a));
    names.dedup();

    let mut pruned = 0usize;
    for name in names.into_iter().skip(keep) {
        match send(target, "DELETE", &target.child(&name), "DELETE").await {
            Ok(_) => {
                pruned += 1;
                log::info!("[webdav] pruned old backup {}", name);
            }
            Err(error) => {
                log::warn!("[webdav] failed to prune {}: {}", name, diag::truncate(&error, 200));
            }
        }
    }
    pruned
}

/// 從伺服器恢復：下載到臨時檔案後走本地匯入的同一條路徑。
/// 完成後由前端呼叫 `restart_app`，與本地匯入的行為一致。
#[tauri::command]
pub async fn webdav_restore(name: String, storage: State<'_, Storage>) -> Result<(), String> {
    restore_from_server(&name, storage.inner()).await
}

/// [自訂] SC-55：連線、列目錄與下載之前的檢查。先確認不是記憶體庫，再看檔名與設定。
fn prepare_restore(name: &str, storage: &Storage) -> Result<Target, String> {
    storage.ensure_persistent()?;
    // 只接受我們自己的備份名，且不能帶路徑分隔符 —— 否則一個 `../` 就能讓
    // 下載地址跑到目錄之外去。
    if !name.starts_with(BACKUP_PREFIX)
        || !name.ends_with(".zip")
        || name.contains('/')
        || name.contains('\\')
    {
        return Err("WEBDAV_BAD_NAME".to_string());
    }
    Target::from_settings(storage)
}

async fn restore_from_server(name: &str, storage: &Storage) -> Result<(), String> {
    let target = prepare_restore(name, storage)?;

    // 先列一次目錄拿到體積：下載超時必須按體積給，含音訊的備份能有幾個 GB，
    // 套用後設資料請求那 30 秒會把正常下載掐斷，而症狀是「恢復總是失敗」。
    let listing = propfind_dir(&target, &target.dir_url).await?;
    let expected = parse_backup_entries(&listing)
        .into_iter()
        .find(|entry| entry.name == name)
        .ok_or_else(|| "WEBDAV_VERIFY_MISSING".to_string())?;

    let temp_path = std::env::temp_dir().join(name);
    let written = download_to_file(&target, &target.child(name), &temp_path, expected.size).await;
    let written = match written {
        Ok(written) => written,
        Err(error) => {
            let _ = std::fs::remove_file(&temp_path);
            return Err(error);
        }
    };

    let result = apply_full_backup(storage, &temp_path.to_string_lossy());
    let _ = std::fs::remove_file(&temp_path);
    match &result {
        Ok(()) => log::info!("[webdav] restored from {} ({} bytes)", name, written),
        Err(error) => log::warn!("[webdav] restore failed {}: {}", name, diag::truncate(error, 300)),
    }
    result
}

/// 流式下載到檔案。不用 `response.bytes()`：那會把整個備份讀進記憶體，
/// 含音訊時等於按備份大小申請一塊幾 GB 的堆。
async fn download_to_file(
    target: &Target,
    url: &str,
    dest: &std::path::Path,
    expected_size: u64,
) -> Result<u64, String> {
    let response = request(target, "GET", url)
        // 和上傳同一套預算：下限約 50KB/s，既不掐斷正常下載也不會永遠掛著。
        .timeout(Duration::from_secs(120 + expected_size / 50_000))
        .send()
        .await
        .map_err(|e| format!("GET failed: {}", diag::truncate(&e.to_string(), 200)))?;
    let mut response = check_status(response, "GET").await?;

    let mut file = std::fs::File::create(dest)
        .map_err(|e| format!("Failed to write the downloaded backup: {}", e))?;
    let mut written = 0u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("Download interrupted: {}", diag::truncate(&e.to_string(), 200)))?
    {
        file.write_all(&chunk)
            .map_err(|e| format!("Failed to write the downloaded backup: {}", e))?;
        written += chunk.len() as u64;
    }
    file.flush()
        .map_err(|e| format!("Failed to write the downloaded backup: {}", e))?;

    if written == 0 {
        return Err("WEBDAV_EMPTY_DOWNLOAD".to_string());
    }
    // 伺服器報了體積就核對一次。截斷的下載解壓時會報「不是有效的備份檔」，
    // 那條資訊會把人引去懷疑備份檔案本身，而真正的問題是這次傳輸。
    if expected_size != 0 && written != expected_size {
        return Err(format!(
            "Download incomplete: got {} of {} bytes",
            written, expected_size
        ));
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_plain_http_and_other_schemes() {
        assert_eq!(
            normalize_dir_url("http://dav.jianguoyun.com/dav/SayIt").unwrap_err(),
            "WEBDAV_URL_INSECURE"
        );
        assert_eq!(
            normalize_dir_url("dav.jianguoyun.com/dav").unwrap_err(),
            "WEBDAV_URL_SCHEME"
        );
        assert_eq!(normalize_dir_url("   ").unwrap_err(), "WEBDAV_URL_EMPTY");
        assert_eq!(normalize_dir_url("https://").unwrap_err(), "WEBDAV_URL_EMPTY");
    }

    /// Runs the whole restore. Loopback stands in for the server, so a missing check
    /// shows up as a connection instead of reaching any real host.
    #[test]
    fn volatile_storage_rejects_a_restore_before_connecting_or_creating_a_file() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let url = format!("https://{}/dav", server.local_addr().unwrap());
        let name = format!("{BACKUP_PREFIX}{}.zip", uuid::Uuid::new_v4());
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();

        let storage = Storage::new(std::path::PathBuf::from(":memory:")).unwrap();
        // Storage::set refuses these secrets, so write the rows an older version could have left.
        let configure = |storage: &Storage| {
            for (key, value) in [(KEY_URL, url.as_str()), (KEY_USERNAME, "user"), (KEY_PASSWORD, "password")] {
                storage.db.lock().unwrap().execute(
                    "INSERT OR REPLACE INTO app_settings (key, value_json, updated_at) VALUES (?1, ?2, 0)",
                    rusqlite::params![key, serde_json::json!(value).to_string()],
                ).unwrap();
            }
        };
        configure(&storage);
        for requested in [name.as_str(), "not-a-backup"] {
            assert_eq!(
                runtime.block_on(restore_from_server(requested, &storage)),
                Err(crate::storage::VOLATILE_STORAGE_ERROR.into()),
            );
        }
        assert_eq!(server.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
        assert!(!std::env::temp_dir().join(&name).exists());

        // A disk database still reaches the existing name and settings checks.
        let dir = std::env::temp_dir().join(format!("sayit-webdav-restore-{}", uuid::Uuid::new_v4()));
        let disk = Storage::new(dir.join("test.db")).unwrap();
        assert_eq!(prepare_restore("not-a-backup", &disk).err().as_deref(), Some("WEBDAV_BAD_NAME"));
        assert_eq!(prepare_restore(&name, &disk).err().as_deref(), Some("WEBDAV_URL_EMPTY"));
        configure(&disk);
        assert!(prepare_restore(&name, &disk).is_ok());
        drop(disk);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn strips_trailing_slashes_so_children_do_not_double_up() {
        let url = normalize_dir_url("https://dav.jianguoyun.com/dav/SayIt/").unwrap();
        assert_eq!(url, "https://dav.jianguoyun.com/dav/SayIt");
        let target = Target {
            dir_url: url,
            username: "u".into(),
            password: "p".into(),
        };
        assert_eq!(
            target.child("sayit-backup-x.zip"),
            "https://dav.jianguoyun.com/dav/SayIt/sayit-backup-x.zip"
        );
    }

    #[test]
    fn credentials_must_not_be_blank() {
        let err = Target::from_config(&WebDavConfig {
            url: "https://dav.jianguoyun.com/dav/SayIt".into(),
            username: "  ".into(),
            password: "p".into(),
        })
        .unwrap_err();
        assert_eq!(err, "WEBDAV_CREDENTIALS_EMPTY");
    }

    /// 堅果雲用 `d:` 字首。
    #[test]
    fn parses_listing_with_namespace_prefix() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<d:multistatus xmlns:d="DAV:">
  <d:response>
    <d:href>/dav/SayIt/</d:href>
    <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat>
  </d:response>
  <d:response>
    <d:href>/dav/SayIt/sayit-backup-2026-08-26_10-00-00-000.zip</d:href>
    <d:propstat><d:prop><d:getcontentlength>4096</d:getcontentlength></d:prop></d:propstat>
  </d:response>
  <d:response>
    <d:href>/dav/SayIt/sayit-backup-2026-08-25_10-00-00-000.zip</d:href>
    <d:propstat><d:prop><d:getcontentlength>2048</d:getcontentlength></d:prop></d:propstat>
  </d:response>
</d:multistatus>"#;
        let entries = parse_backup_entries(xml);
        assert_eq!(entries.len(), 2);
        // 倒序 = 從新到舊，保留策略直接 skip(keep) 就是刪舊的。
        assert_eq!(entries[0].name, "sayit-backup-2026-08-26_10-00-00-000.zip");
        assert_eq!(entries[0].size, 4096);
        assert_eq!(entries[1].size, 2048);
    }

    /// 大寫字首 / 無字首 / 其他字首都必須能解析：各家伺服器寫法不統一，
    /// 按字首匹配的那一版會在換伺服器時靜默返回空列表。
    #[test]
    fn parses_listing_regardless_of_prefix_case_or_absence() {
        let xml = r#"<D:multistatus xmlns:D="DAV:">
  <D:response><D:href>/dav/SayIt/sayit-backup-a.zip</D:href>
    <D:propstat><D:prop><lp1:getcontentlength>11</lp1:getcontentlength></D:prop></D:propstat>
  </D:response>
</D:multistatus>"#;
        let entries = parse_backup_entries(xml);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].size, 11);

        let no_prefix = r#"<multistatus xmlns="DAV:">
  <response><href>/dav/SayIt/sayit-backup-b.zip</href>
    <propstat><prop><getcontentlength>22</getcontentlength></prop></propstat>
  </response>
</multistatus>"#;
        let entries = parse_backup_entries(no_prefix);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "sayit-backup-b.zip");
        assert_eq!(entries[0].size, 22);
    }

    #[test]
    fn ignores_directories_and_foreign_files() {
        let xml = r#"<d:multistatus xmlns:d="DAV:">
  <d:response><d:href>/dav/SayIt/</d:href></d:response>
  <d:response><d:href>/dav/SayIt/notes.txt</d:href></d:response>
  <d:response><d:href>/dav/SayIt/sayit-config-2026.json</d:href></d:response>
  <d:response><d:href>/dav/SayIt/sayit-backup-x.zip.part</d:href></d:response>
</d:multistatus>"#;
        assert!(parse_backup_entries(xml).is_empty());
    }

    /// 打包 0~50、上傳 50~90，兩段不能重疊也不能倒退。
    #[test]
    fn progress_phases_do_not_overlap() {
        assert_eq!(packing_percent(2.0), 1.0);
        assert_eq!(packing_percent(98.0), 49.0);
        assert!(packing_percent(200.0) <= 50.0);
        assert_eq!(upload_percent(0, 1000), 50.0);
        assert_eq!(upload_percent(1000, 1000), 90.0);
        // 體積未知時不會算出 NaN 或負數
        assert_eq!(upload_percent(0, 0), 90.0);
    }

    /// 設定沒配過時，scope 必須是「只有配置」。
    ///
    /// 這條要用真的 Storage 跑：讀不到設定時靠 `crate::defaults` 的兜底（與 defaults.ts 有對帳測試）。
    /// 一旦有人把兜底改成 true，
    /// 後果是每台機器悄悄開始上傳整個音訊庫，而介面上的勾還是沒勾的。
    #[test]
    fn scope_defaults_to_config_only_when_unset() {
        let dir = std::env::temp_dir().join(format!("sayit-webdav-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage = Storage::new(dir.join("test.db")).unwrap();

        let scope = scope_from_settings(&storage);
        assert!(!scope.include_history);
        assert!(!scope.include_audio);

        storage.set(KEY_INCLUDE_AUDIO, &Value::Bool(true)).unwrap();
        assert!(scope_from_settings(&storage).include_audio);
        assert!(!scope_from_settings(&storage).include_history);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ancestors_stop_at_the_host() {
        assert_eq!(
            ancestor_urls("https://host.com/dav/a/b"),
            vec!["https://host.com/dav/a", "https://host.com/dav"]
        );
        // 目錄就在根下時沒有需要補建的祖先
        assert!(ancestor_urls("https://host.com/dav").is_empty());
    }

    /// 恢復只接受我們自己的備份名：一個 `../` 就能把下載地址帶到目錄之外。
    #[test]
    fn restore_rejects_names_with_separators() {
        for bad in [
            "../../etc/passwd",
            "sayit-backup-../x.zip",
            "sayit-backup-a.zip/..",
            "other.zip",
            "sayit-backup-a.txt",
        ] {
            let ok = bad.starts_with(BACKUP_PREFIX)
                && bad.ends_with(".zip")
                && !bad.contains('/')
                && !bad.contains('\\');
            assert!(!ok, "should have been rejected: {}", bad);
        }
        assert!("sayit-backup-2026-08-26_10-00-00-000.zip".starts_with(BACKUP_PREFIX));
    }
}
