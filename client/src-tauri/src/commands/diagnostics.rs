use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use tauri::{AppHandle, Manager, State};
use crate::storage::Storage;

// ─── collect_settings ───

#[tauri::command]
pub fn collect_settings(storage: State<Storage>) -> Result<Value, String> {
    let keys = [
        "shortcutPTT", "shortcutPTTCombo", "shortcutHandsFree", "shortcutToggleAi",
        "aiEnabled", "aiMinDurationSec",
        "autoLaunch", "selectedMic", "hotwords", "builtinHotwordSets",
        "stats", "activePresetId", "audioRetentionDays",
        "serverUrl", "language",
    ];

    let mut settings = serde_json::Map::new();
    for key in &keys {
        let val = storage.get(key, None);
        settings.insert(key.to_string(), val);
    }

    Ok(Value::Object(settings))
}

// ─── Diagnostics Preview ───

use super::system::{log_dir, log_paths, rotated_logs};

#[derive(Deserialize)]
struct PreviewRequest {
    #[allow(dead_code)]
    settings: Value,
    #[serde(rename = "issueOccurrence")]
    issue_occurrence: String,
}

#[derive(Serialize)]
struct DiagnosticsPreview {
    #[serde(rename = "generatedAt")]
    generated_at: String,
    #[serde(rename = "retentionDays")]
    retention_days: Option<i64>,
    #[serde(rename = "filesScanned")]
    files_scanned: usize,
    #[serde(rename = "totalRawEvents")]
    total_raw_events: usize,
    #[serde(rename = "totalTimelineEntries")]
    total_timeline_entries: usize,
    #[serde(rename = "issueWindowLabel")]
    issue_window_label: String,
    #[serde(rename = "rangeStart", skip_serializing_if = "Option::is_none")]
    range_start: Option<String>,
    #[serde(rename = "rangeEnd", skip_serializing_if = "Option::is_none")]
    range_end: Option<String>,
    #[serde(rename = "systemInfo")]
    system_info: SystemInfo,
    summary: Summary,
    timeline: Vec<TimelineEntry>,
}

#[derive(Serialize)]
struct SystemInfo {
    platform: String,
    #[serde(rename = "appVersion")]
    app_version: String,
    #[serde(rename = "webviewVersion")]
    webview_version: String,
}

#[derive(Serialize)]
struct Summary {
    errors: usize,
    warnings: usize,
    modules: Vec<ModuleCount>,
    #[serde(rename = "lastError", skip_serializing_if = "Option::is_none")]
    last_error: Option<TimelineEntry>,
}

#[derive(Serialize, Clone)]
struct ModuleCount {
    module: String,
    count: usize,
}

#[derive(Serialize, Clone)]
struct TimelineEntry {
    ts: String,
    level: String,
    module: String,
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(rename = "traceId", skip_serializing_if = "Option::is_none")]
    trace_id: Option<String>,
}

/// 解析日志行，格式: [2025-03-20 14:30:00.123] [LEVEL] [module] message
fn parse_log_line(line: &str) -> Option<TimelineEntry> {
    // 最少需要 [timestamp] [LEVEL] [module] msg
    let line = line.trim();
    if !line.starts_with('[') {
        return None;
    }

    // 提取 timestamp。个别行可能是 [[2026-...]] 这种多方括号开头，去掉前导 '[' 并校验，
    // 否则带 '[' 的时间戳字符串排序会排到最后，污染「覆盖记录终点」显示。
    let ts_end = line.find(']')?;
    let ts = line[1..ts_end].trim_start_matches('[').trim();
    if ts.is_empty() || !ts.as_bytes()[0].is_ascii_digit() {
        return None;
    }

    let rest = line[ts_end + 1..].trim_start();
    if !rest.starts_with('[') {
        return None;
    }

    // 提取 level
    let level_end = rest.find(']')?;
    let level_raw = &rest[1..level_end];
    let level = level_raw.trim().to_lowercase();

    let rest2 = rest[level_end + 1..].trim_start();
    if !rest2.starts_with('[') {
        // 没有 module 标签，用 "unknown"
        return Some(TimelineEntry {
            ts: ts.to_string(),
            level,
            module: "unknown".to_string(),
            title: rest2.to_string(),
            detail: None,
            trace_id: None,
        });
    }

    // 提取 module
    let mod_end = rest2.find(']')?;
    let module = rest2[1..mod_end].trim().to_string();
    let title = rest2[mod_end + 1..].trim().to_string();

    Some(TimelineEntry {
        ts: ts.to_string(),
        level,
        module,
        title,
        detail: None,
        trace_id: None,
    })
}

fn issue_window_label(occurrence: &str) -> &'static str {
    match occurrence {
        "just_now" => "Just now",
        "within_1h" => "Within 1 hour",
        "today" => "Today",
        "yesterday" => "Yesterday",
        "older" => "Earlier",
        _ => "Unknown",
    }
}

/// 根据 issue_occurrence 计算时间窗口的起始时间字符串
fn occurrence_cutoff(occurrence: &str) -> Option<String> {
    let now = chrono::Local::now();
    let cutoff = match occurrence {
        "just_now" => now - chrono::Duration::minutes(10),
        "within_1h" => now - chrono::Duration::hours(1),
        "today" => now.date_naive().and_hms_opt(0, 0, 0)
            .map(|naive| naive.and_local_timezone(chrono::Local).unwrap())?,
        "yesterday" => (now.date_naive() - chrono::Duration::days(1))
            .and_hms_opt(0, 0, 0)
            .map(|naive| naive.and_local_timezone(chrono::Local).unwrap())?,
        // "older" / "not_sure" — 不过滤
        _ => return None,
    };
    Some(cutoff.format("%Y-%m-%d %H:%M:%S").to_string())
}

// [自訂] REV9：輪替檔增加後逐檔統計，記憶體只保留需要顯示的最新事件。
fn scan_log_entries(dir: &std::path::Path, cutoff: Option<&str>, mut visit: impl FnMut(TimelineEntry)) -> (usize, usize) {
    let mut total = 0;
    let mut files_scanned = 0;
    for path in log_paths(dir) {
        files_scanned += 1;
        if let Ok(content) = std::fs::read_to_string(&path) {
            for entry in content.lines().filter_map(parse_log_line) {
                total += 1;
                if cutoff.is_none_or(|cutoff| entry.ts.as_str() >= cutoff) {
                    visit(entry);
                }
            }
        }
    }
    (total, files_scanned)
}

struct RecentEntries {
    limit: usize,
    order: usize,
    entries: BTreeMap<(String, usize), TimelineEntry>,
}

impl RecentEntries {
    fn new(limit: usize) -> Self {
        Self { limit, order: 0, entries: BTreeMap::new() }
    }

    fn push(&mut self, entry: TimelineEntry) {
        self.entries.insert((entry.ts.clone(), self.order), entry);
        self.order += 1;
        if self.entries.len() > self.limit {
            self.entries.pop_first();
        }
    }
}

#[tauri::command]
pub async fn get_diagnostics_preview(data: Value, app: AppHandle) -> Result<Value, String> {
    let req: PreviewRequest = serde_json::from_value(data).map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || {
        build_diagnostics_preview(req, app.state::<Storage>().inner())
    }).await.map_err(|e| e.to_string())?
}

fn build_diagnostics_preview(req: PreviewRequest, storage: &Storage) -> Result<Value, String> {
    let cutoff = occurrence_cutoff(&req.issue_occurrence);
    let mut errors = 0;
    let mut warnings = 0;
    let mut module_map = HashMap::<String, usize>::new();
    let mut last_error: Option<TimelineEntry> = None;
    let mut range_start: Option<String> = None;
    let mut range_end: Option<String> = None;
    let mut recent = RecentEntries::new(200);
    let (total_raw, files_scanned) = scan_log_entries(&log_dir(), cutoff.as_deref(), |entry| {
        if entry.level == "error" {
            errors += 1;
            if last_error.as_ref().is_none_or(|previous| previous.ts <= entry.ts) {
                last_error = Some(entry.clone());
            }
        } else if matches!(entry.level.as_str(), "warn" | "warning") {
            warnings += 1;
        }
        *module_map.entry(entry.module.clone()).or_insert(0) += 1;
        if range_start.as_ref().is_none_or(|earliest| *earliest > entry.ts) {
            range_start = Some(entry.ts.clone());
        }
        if range_end.as_ref().is_none_or(|latest| *latest < entry.ts) {
            range_end = Some(entry.ts.clone());
        }
        recent.push(entry);
    });

    let mut modules: Vec<ModuleCount> = module_map.into_iter()
        .map(|(module, count)| ModuleCount { module, count }).collect();
    modules.sort_by(|a, b| b.count.cmp(&a.count));
    let timeline: Vec<_> = recent.entries.into_values().collect();
    let timeline_count = timeline.len();

    let preview = DiagnosticsPreview {
        generated_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        retention_days: super::system::log_retention_days(storage).ok(), // [自訂] S71
        files_scanned,
        total_raw_events: total_raw,
        total_timeline_entries: timeline_count,
        issue_window_label: issue_window_label(&req.issue_occurrence).to_string(),
        range_start,
        range_end,
        system_info: SystemInfo {
            platform: "win32".to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            webview_version: "WebView2".to_string(),
        },
        summary: Summary {
            errors,
            warnings,
            modules,
            last_error,
        },
        timeline,
    };

    serde_json::to_value(preview).map_err(|e| e.to_string())
}

// ─── Diagnostics ZIP ───

#[derive(Deserialize)]
struct DiagnosticsZipRequest {
    description: String,
    /// 用户选的问题类型码（insert_failed / no_text / …，定义在 types/appApi.d.ts）。
    /// 写进 manifest 是为了让收到 ticket 的人不必读完整份日志就知道该看哪一段。
    /// 老版本客户端不发这个字段，所以要能缺省。
    #[serde(rename = "issueType", default)]
    issue_type: String,
    settings: Value,
    #[serde(rename = "issueOccurrence")]
    issue_occurrence: String,
    images: Vec<DiagImage>,
}

#[derive(Deserialize)]
struct DiagImage {
    name: String,
    #[serde(rename = "type")]
    mime_type: String,
    size: usize,
    /// 旧命令里是 JSON 数字数组；create_diagnostics_zip_raw 里缺省，由二进制载荷按 size 切出来。
    #[serde(default)]
    data: Vec<u8>,
}

#[tauri::command]
pub async fn create_diagnostics_zip(data: Value) -> Result<String, String> {
    let req: DiagnosticsZipRequest = serde_json::from_value(data).map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || build_diagnostics_zip(req))
        .await.map_err(|e| e.to_string())?
}

/// 同 create_diagnostics_zip，但截图以原始字节发过来（见 ipc_body），按 images[].size 依次切开。
///
/// 原来截图在前端被 `Array.from(new Uint8Array(...))` 转成 JSON 数字数组，一张 20 MB 的图
/// 变成六七十 MB 的字符串再解析一遍。
#[tauri::command]
pub async fn create_diagnostics_zip_raw(request: tauri::ipc::Request<'_>) -> Result<String, String> {
    let (mut req, payload): (DiagnosticsZipRequest, Vec<u8>) = crate::ipc_body::split_framed(&request)?;
    tokio::task::spawn_blocking(move || {
        attach_image_bytes(&mut req.images, &payload)?;
        build_diagnostics_zip(req)
    }).await.map_err(|e| e.to_string())?
}

/// 按每张图声明的 size 依次从载荷里切出字节；总数对不上就拒绝（宁可报错也别把图切错位）。
fn attach_image_bytes(images: &mut [DiagImage], payload: &[u8]) -> Result<(), String> {
    let mut offset = 0usize;
    for image in images.iter_mut() {
        let end = offset
            .checked_add(image.size)
            .filter(|end| *end <= payload.len())
            .ok_or_else(|| "Screenshot sizes do not match the uploaded bytes".to_string())?;
        image.data = payload[offset..end].to_vec();
        offset = end;
    }
    if offset != payload.len() {
        return Err("Screenshot sizes do not match the uploaded bytes".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod raw_upload_tests {
    use super::{attach_image_bytes, DiagImage};

    fn image(size: usize) -> DiagImage {
        DiagImage { name: String::new(), mime_type: "image/png".into(), size, data: Vec::new() }
    }

    #[test]
    fn splits_payload_by_declared_sizes() {
        let mut images = vec![image(2), image(3)];
        attach_image_bytes(&mut images, &[1, 2, 3, 4, 5]).unwrap();
        assert_eq!(images[0].data, vec![1, 2]);
        assert_eq!(images[1].data, vec![3, 4, 5]);
    }

    #[test]
    fn mismatched_sizes_are_rejected() {
        assert!(attach_image_bytes(&mut [image(4)], &[1, 2]).is_err());
        assert!(attach_image_bytes(&mut [image(1)], &[1, 2]).is_err());
        assert!(attach_image_bytes(&mut [image(usize::MAX), image(2)], &[1, 2]).is_err());
    }

    #[test]
    fn timeline_keeps_latest_entries_across_legacy_and_dated_logs() {
        use std::fs;
        let dir = std::env::temp_dir().join(format!("sayit-diag-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("sayit.log"), "[2026-11-25 01:00:00.000] [INFO] [test] current\n").unwrap();
        fs::write(dir.join("sayit.2026-11-24.0000000001.log"),
            "[2026-11-24 02:00:00.000] [WARN] [test] dated\n").unwrap();
        fs::write(dir.join("sayit.1.log"),
            "[2026-11-23 01:00:00.000] [ERROR] [test] expired\n[2026-11-24 01:00:00.000] [INFO] [test] legacy\n").unwrap();
        let mut recent = super::RecentEntries::new(2);
        let (total, files) = super::scan_log_entries(&dir, Some("2026-11-24"), |entry| recent.push(entry));
        assert_eq!((total, files), (4, 3));
        assert_eq!(recent.order, 3);
        let messages: Vec<_> = recent.entries.into_values().map(|entry| entry.title).collect();
        assert_eq!(messages, ["dated", "current"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_diagnostics_write_does_not_publish_drop_finalized_zip() {
        use std::fs;
        let dir = std::env::temp_dir().join(format!("sayit-diag-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let output = dir.join("diagnostics.zip");
        let partial = dir.join("diagnostics.zip.partial");
        let err = super::publish_diagnostics_zip(&partial, &output, |path| {
            let mut writer = zip::ZipWriter::new(fs::File::create(path).unwrap());
            super::super::archive::write_deflated_entry(&mut writer, "manifest.json", b"{}").unwrap();
            drop(writer);
            let archive = zip::ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
            assert_eq!(archive.len(), 1);
            Err("simulated write failure".into())
        }).unwrap_err();
        assert_eq!(err, "simulated write failure");
        assert!(!partial.exists());
        assert!(!output.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn diagnostics_keeps_all_screenshots_and_preserves_nonconflicting_names() {
        use std::fs;
        use std::io::Read;
        let names = ["Screen.png", "screen.png", "screen-2.png", "Screen.png", "Screenshot-6.png", ""];
        let images = names.iter().enumerate().map(|(index, name)| DiagImage {
            name: (*name).into(),
            mime_type: "image/png".into(),
            size: 1,
            data: vec![index as u8],
        }).collect();
        let req = super::DiagnosticsZipRequest {
            description: String::new(),
            issue_type: String::new(),
            settings: serde_json::json!({}),
            issue_occurrence: String::new(),
            images,
        };
        let path = super::build_diagnostics_zip(req).unwrap();
        let mut zip = zip::ZipArchive::new(fs::File::open(&path).unwrap()).unwrap();
        let expected = ["Screen.png", "screen-3.png", "screen-2.png", "Screen-4.png", "Screenshot-6.png", "screenshot-6-2.png"];
        for (index, name) in expected.iter().enumerate() {
            let mut file = zip.by_name(&format!("images/{name}")).unwrap();
            let mut data = Vec::new();
            file.read_to_end(&mut data).unwrap();
            assert_eq!(data, [index as u8]);
        }
        assert!(zip.by_name("manifest.json").is_ok());
        assert!(zip.by_name("settings.json").is_ok());
        assert!(zip.by_name("timeline.json").is_ok());
        drop(zip);
        fs::remove_file(path).unwrap();
    }

}

fn build_diagnostics_zip(req: DiagnosticsZipRequest) -> Result<String, String> {
    // [自訂] REV9：封裝在工作執行緒執行，序列化建立與刪除前一份診斷包。
    static ZIP_BUILD: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _build = ZIP_BUILD.lock().unwrap_or_else(|error| error.into_inner());
    let diag_dir = diagnostics_dir();
    std::fs::create_dir_all(&diag_dir).map_err(|e| e.to_string())?;
    // [自訂] S45：產生新包前先清掉舊包（含日誌、詞彙、截圖），診斷目錄最多只留一份。
    purge_diagnostics_zips();

    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
    // [自訂] REV9：前一份被清除後，尚未關閉的另存對話框不會指向新請求的包。
    let stem = format!("diagnostics-{ts}-{}", uuid::Uuid::new_v4());
    let zip_path = diag_dir.join(format!("{stem}.zip"));
    // [自訂] SCAN1-7（L-77）：先寫成暫存檔，完整寫完才改名成 .zip；中途出錯就刪掉暫存檔，不留半個包。
    let partial_path = diag_dir.join(format!("{stem}.zip.partial"));
    let identity = super::diag_redact::Identity::current();
    publish_diagnostics_zip(&partial_path, &zip_path, |path| write_diagnostics_zip(path, &req, &log_dir(), &identity))?;
    Ok(zip_path.to_string_lossy().to_string())
}

// [自訂] L-77：writer 即使在 Drop 中收尾，仍只有成功的暫存檔可以發布。
fn publish_diagnostics_zip(
    partial_path: &std::path::Path,
    zip_path: &std::path::Path,
    write_zip: impl FnOnce(&std::path::Path) -> Result<(), String>,
) -> Result<(), String> {
    let result = write_zip(partial_path)
        .and_then(|()| std::fs::rename(partial_path, zip_path).map_err(|error| error.to_string()));
    if result.is_err() {
        remove_partial_zip(partial_path);
    }
    result
}

/// [自訂] SCAN1-7：刪除暫存檔；刪不掉只記錄錯誤，不記包的內容。
fn remove_partial_zip(path: &std::path::Path) {
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            crate::commands::system::write_log_line(&format!("[diagnostics] failed to remove partial zip: {}", e));
        }
    }
}

/// [自訂] SC-59（L-97）：ZIP 內各項目的處理——
/// manifest.json（含使用者描述）、settings.json、timeline.json：逐一遮蔽字串值與鍵後再序列化；
/// logs/*：依時段過濾後遮蔽全文；images/*：二進位，原樣寫入。
/// 遮蔽內容：家目錄換成 `~`，使用者名稱換成 `<user>`，主機名稱換成 `<host>`（見 diag_redact）。
fn write_diagnostics_zip(
    zip_path: &std::path::Path,
    req: &DiagnosticsZipRequest,
    log_d: &std::path::Path,
    identity: &super::diag_redact::Identity,
) -> Result<(), String> {
    let file = std::fs::File::create(zip_path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    use super::archive::write_deflated_entry;
    let masked_json = |value: &Value| serde_json::to_string_pretty(&identity.mask_value(value))
        .map_err(|error| error.to_string());

    // 1. manifest.json
    let manifest = serde_json::json!({
        "generatedAt": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        "issueType": if req.issue_type.is_empty() { "unspecified" } else { req.issue_type.as_str() },
        "description": req.description,
        "issueOccurrence": req.issue_occurrence,
        "appVersion": env!("CARGO_PKG_VERSION"),
        "platform": "win32",
        "imageCount": req.images.len(),
    });
    let json = masked_json(&manifest)?;
    write_deflated_entry(&mut zip, "manifest.json", json.as_bytes())?;

    // 2. settings.json
    let json = masked_json(&req.settings)?;
    write_deflated_entry(&mut zip, "settings.json", json.as_bytes())?;

    // 3. 日志文件
    // [自訂] SCAN1-5（L-75）：原始日誌也依使用者選的問題時段過濾，和預覽一致；
    // 原本是逐位元組收錄全部日誌（含更早的路徑與事件）。時段外的檔案整個不放。
    let cutoff = occurrence_cutoff(&req.issue_occurrence);
    for path in log_paths(log_d) {
        if let Some(filename) = path.file_name().and_then(|name| name.to_str()) {
            // REV1：用 lossy 讀取，非 UTF-8 的日誌也照樣依時段過濾，不會整份略過。
            if let Ok(bytes) = std::fs::read(&path) {
                let content = String::from_utf8_lossy(&bytes);
                let kept = filter_raw_log_since(&content, cutoff.as_deref());
                if kept.is_empty() {
                    continue;
                }
                let zip_name = format!("logs/{}", filename);
                write_deflated_entry(&mut zip, &zip_name, identity.mask(&kept).as_bytes())?;
            }
        }
    }

    // 4. 截图
    let image_names = req.images.iter().enumerate().map(|(i, img)| {
        let ext = match img.mime_type.as_str() {
            "image/png" => "png",
            "image/jpeg" | "image/jpg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            _ => "bin",
        };
        if img.name.is_empty() {
            format!("screenshot-{}.{}", i + 1, ext)
        } else {
            // [自訂] SC-59：截圖檔名也可能含帳號或主機名稱；在去重之前遮蔽。
            identity.mask(&img.name)
        }
    }).collect::<Vec<_>>();
    let reserved: HashSet<_> = image_names.iter().map(|name| name.to_lowercase()).collect();
    let mut used = HashSet::new();
    for (name, img) in image_names.into_iter().zip(&req.images) {
        let name = unique_image_name(name, &reserved, &mut used);
        let zip_name = format!("images/{name}");
        write_deflated_entry(&mut zip, &zip_name, &img.data)?;
    }

    // 5. diagnostics preview (filtered by occurrence)
    let mut recent = RecentEntries::new(500);
    scan_log_entries(log_d, cutoff.as_deref(), |entry| recent.push(entry));
    let timeline: Vec<_> = recent.entries.into_values().rev().collect();
    let json = masked_json(&serde_json::to_value(&timeline).map_err(|error| error.to_string())?)?;
    write_deflated_entry(&mut zip, "timeline.json", json.as_bytes())?;

    zip.finish().map_err(|e| e.to_string())?;

    Ok(())
}

// [自訂] SC-51：只替撞名的截圖加序號，保留其他截圖原本的檔名。
fn unique_image_name(name: String, reserved: &HashSet<String>, used: &mut HashSet<String>) -> String {
    if used.insert(name.to_lowercase()) {
        return name;
    }
    let stem_and_ext = std::path::Path::new(&name).extension().and_then(|ext| ext.to_str())
        .and_then(|ext| Some((name.strip_suffix(ext)?.strip_suffix('.')?, ext)));
    let mut suffix = 2usize;
    loop {
        let candidate = match stem_and_ext {
            Some((stem, ext)) => format!("{stem}-{suffix}.{ext}"),
            None => format!("{name}-{suffix}"),
        };
        let key = candidate.to_lowercase();
        if !reserved.contains(&key) && used.insert(key) {
            return candidate;
        }
        suffix += 1;
    }
}

/// REV1：取出行首 `[時間戳]`（允許多個前導 `[`）；不是數字開頭就視為沒有時間戳。
fn leading_timestamp(line: &str) -> Option<&str> {
    let line = line.trim();
    if !line.starts_with('[') {
        return None;
    }
    let end = line.find(']')?;
    let ts = line[1..end].trim_start_matches('[').trim();
    if ts.is_empty() || !ts.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    Some(ts)
}

/// [自訂] SCAN1-5：只保留時間戳 >= cutoff 的日誌行。沒有時間戳的續行（例如多行錯誤）
/// 跟隨上一個有時間戳的行；檔案開頭沒有時間戳的行一律不收。cutoff 為 None（不限時段）時全收。
pub(super) fn filter_raw_log_since(content: &str, cutoff: Option<&str>) -> String {
    let Some(cutoff) = cutoff else {
        return content.to_string();
    };
    let mut out = String::new();
    let mut keep = false;
    for line in content.lines() {
        // REV1：只要行首有時間戳就依它判斷（不要求後面有 [LEVEL]），避免格式不同的舊行沿用前一行的結果。
        if let Some(ts) = leading_timestamp(line) {
            keep = ts >= cutoff;
        }
        if keep {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

// ─── Read helpers ───

fn diagnostics_dir() -> std::path::PathBuf {
    crate::commands::app_data_dir()
        .join("diagnostics")
}

/// 诊断包只会是 create_diagnostics_zip 生成在诊断目录里的 .zip。
/// 读命令把内容整个交给前端，复制命令复制完还会删掉源文件，所以两边都不能接受任意路径。
fn diagnostics_zip_in_dir(path: &str) -> Option<std::path::PathBuf> {
    let path = std::path::PathBuf::from(path);
    if !crate::path_guard::has_extension(&path, &["zip"]) {
        return None;
    }
    crate::path_guard::existing_file_in_dir(&path, &diagnostics_dir())
}

#[tauri::command]
pub fn read_diagnostics_zip(path: String) -> Result<Option<Vec<u8>>, String> {
    let Some(path) = diagnostics_zip_in_dir(&path) else {
        return Ok(None);
    };
    let data = std::fs::read(&path).map_err(|e| e.to_string())?;
    Ok(Some(data))
}

/// 同 read_diagnostics_zip，但直接回字节（前端拿到 ArrayBuffer）。
/// 原来是 `Vec<u8>` 走 JSON，诊断包每个字节变成数组里的一个数字，最大 52 MB 的包是一两百 MB 的字符串。
/// 文件不在时返回空字节。
#[tauri::command]
pub fn read_diagnostics_zip_bytes(path: String) -> Result<tauri::ipc::Response, String> {
    let bytes = match diagnostics_zip_in_dir(&path) {
        Some(path) => std::fs::read(&path).map_err(|e| e.to_string())?,
        None => Vec::new(),
    };
    Ok(tauri::ipc::Response::new(bytes))
}

/// [自訂] S45：刪除診斷目錄裡所有 .zip（取消另存、產生失敗、上傳停用、上次異常結束都會留下）。
fn purge_diagnostics_zips() {
    let Ok(entries) = std::fs::read_dir(diagnostics_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // [自訂] SCAN1-7：一併清掉中途失敗留下的 .partial 暫存檔。
        if path.is_file() && crate::path_guard::has_extension(&path, &["zip", "partial"]) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// [自訂] S45：啟動時清掉上次殘留的診斷包。
pub fn purge_stale_diagnostics_zips() {
    purge_diagnostics_zips();
}

/// [自訂] S45：前端取消另存或出錯時呼叫，只刪診斷目錄內的 .zip。
#[tauri::command]
pub fn discard_diagnostics_zip(path: String) -> Result<(), String> {
    if let Some(path) = diagnostics_zip_in_dir(&path) {
        let _ = std::fs::remove_file(&path);
    }
    Ok(())
}

#[tauri::command]
pub fn copy_diagnostics_zip(source: String, destination: String) -> Result<(), String> {
    let Some(src) = diagnostics_zip_in_dir(&source) else {
        return Err("The diagnostics file does not exist".to_string());
    };
    // 目标来自保存对话框（只给了 zip 过滤器），这里再确认一次扩展名，免得被拿来往任意位置写可执行文件。
    if !crate::path_guard::has_extension(std::path::Path::new(&destination), &["zip"]) {
        return Err("The diagnostics file must be saved as a .zip".to_string());
    }
    std::fs::copy(&src, &destination).map_err(|e| format!("Failed to copy diagnostics file: {}", e))?;
    // 清理临时文件
    let _ = std::fs::remove_file(&src);
    Ok(())
}

#[tauri::command]
pub fn read_log_file(log_type: String) -> Result<Option<String>, String> {
    let log_d = log_dir();

    // [自訂] REV9：舊版檢視器的 1～3 代表最近三個輪替檔案。
    let path = match log_type.as_str() {
        "1" | "2" | "3" => {
            let index = log_type.parse::<usize>().map_err(|error| error.to_string())? - 1;
            let Some(path) = rotated_logs(&log_d).into_iter().nth(index) else {
                return Ok(None);
            };
            path
        }
        _ => log_d.join("sayit.log"),
    };
    if !std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
        return Ok(None);
    }

    // 最多读 200KB
    let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
    let size = meta.len();
    const MAX_READ: u64 = 200 * 1024;

    if size <= MAX_READ {
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    } else {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(&path).map_err(|e| e.to_string())?;
        f.seek(SeekFrom::End(-(MAX_READ as i64))).map_err(|e| e.to_string())?;
        let mut bytes = Vec::with_capacity(MAX_READ as usize);
        f.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        let mut content = String::from_utf8_lossy(&bytes).into_owned();
        if let Some(pos) = content.find('\n') {
            content = content[pos + 1..].to_string();
        }
        Ok(Some(format!("... (showing last ~200KB) ...\n{}", content)))
    }
}

#[tauri::command]
pub fn open_log_folder() -> Result<(), String> {
    let dir = log_dir();
    if !dir.exists() {
        let _ = std::fs::create_dir_all(&dir);
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(dir.to_string_lossy().to_string())
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(dir.to_string_lossy().to_string())
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(dir.to_string_lossy().to_string())
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }
    Ok(())
}

#[cfg(test)]
mod sc59_redaction_tests {
    // [自訂] SC-59（L-97）：以虛構身分產生診斷包，掃描所有文字項目無殘留。
    use super::{write_diagnostics_zip, DiagImage, DiagnosticsZipRequest};
    use crate::commands::diag_redact::Identity;
    use std::{fs, io::Read};

    const SECRETS: &[&str] = &["zqxtester", "zqx-studio", "zqx studio", "fileserver", "nas.lan", "old-mac"];

    fn assert_clean(name: &str, text: &str) {
        let lower = text.to_ascii_lowercase();
        for secret in SECRETS {
            assert!(!lower.contains(secret), "{name} still contains {secret}: {text}");
        }
    }

    fn walk(name: &str, value: &serde_json::Value) {
        match value {
            serde_json::Value::String(text) => assert_clean(name, text),
            serde_json::Value::Array(items) => items.iter().for_each(|item| walk(name, item)),
            serde_json::Value::Object(map) => map.iter().for_each(|(key, item)| {
                assert_clean(name, key);
                walk(name, item);
            }),
            _ => {}
        }
    }

    #[test]
    fn every_text_entry_hides_home_user_and_host() {
        let dir = std::env::temp_dir().join(format!("sayit-diag-redact-{}", uuid::Uuid::new_v4()));
        let logs = dir.join("logs");
        fs::create_dir_all(&logs).unwrap();
        let identity = Identity::new(
            Some("/Users/Zqxtester".into()),
            vec!["zqxtester".into()],
            vec!["Zqx-Studio.local".into(), "Zqx Studio".into()],
        );
        let lines = [
            "[2026-11-26 10:00:00.000] [INFO] [app] log at /Users/Zqxtester/Library/Logs/SayItLocally/sayit.log",
            "[2026-11-26 10:00:01.000] [WARN] [fs] \"C:\\\\Users\\\\ZQXTESTER\\\\AppData\\\\x\" and \\Users\\zqxtester\\y",
            "[2026-11-26 10:00:02.000] [ERROR] [net] smb://zqxtester@nas.lan/share afp://Old-Mac.local/v \\\\fileserver\\s",
            "[2026-11-26 10:00:03.000] [INFO] [app] file:///users/zqxtester/a.wav on Zqx-Studio.local (zqx-studio) by zqxtester",
            "  continuation ~zqxtester/notes $HOME/x %USERPROFILE%\\y host Zqx Studio",
        ];
        fs::write(logs.join("sayit.log"), lines.join("\n")).unwrap();
        let req = DiagnosticsZipRequest {
            description: "Broke in /Users/Zqxtester/Desktop on Zqx-Studio".into(),
            issue_type: "insert_failed".into(),
            settings: serde_json::json!({"/Users/Zqxtester/k": ["C:\\Users\\zqxtester\\v"], "host": "zqx-studio.local"}),
            issue_occurrence: "older".into(),
            images: vec![DiagImage {
                name: "zqxtester on Zqx-Studio.png".into(),
                mime_type: "image/png".into(),
                size: 9,
                data: b"zqxtester".to_vec(),
            }],
        };
        let zip_path = dir.join("diagnostics.zip");
        write_diagnostics_zip(&zip_path, &req, &logs, &identity).unwrap();

        let mut zip = zip::ZipArchive::new(fs::File::open(&zip_path).unwrap()).unwrap();
        let names: Vec<String> = (0..zip.len()).map(|index| zip.by_index(index).unwrap().name().unwrap().to_string()).collect();
        assert_eq!(names.len(), 5, "{names:?}");
        let mut log_text = String::new();
        for name in &names {
            assert_clean("entry name", name);
            let mut bytes = Vec::new();
            zip.by_name(name).unwrap().read_to_end(&mut bytes).unwrap();
            if name.starts_with("images/") {
                assert_eq!(bytes, b"zqxtester", "binary entries are written unchanged");
                continue;
            }
            let text = String::from_utf8(bytes).unwrap();
            assert_clean(name, &text);
            if name.ends_with(".json") {
                walk(name, &serde_json::from_str(&text).unwrap());
            } else {
                log_text = text;
            }
        }
        // 診斷仍可用：相對路徑尾段與檔名保留。
        assert!(log_text.contains("~/Library/Logs/SayItLocally/sayit.log"), "{log_text}");
        assert!(log_text.contains("\\Users\\<user>\\y"), "{log_text}");
        assert!(log_text.contains("smb://<host>/share"), "{log_text}");
        drop(zip);
        fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod scan1_log_filter_tests {
    // [自訂] SCAN1-5（L-75）
    use super::filter_raw_log_since;

    #[test]
    fn raw_log_is_trimmed_to_the_selected_window() {
        let log = "orphan line without timestamp\n\
[2026-11-20 10:00:00.000] [INFO] [app] old event /Users/someone/old\n\
  continuation of old event\n\
[2026-11-24 09:00:00.000] [ERROR] [app] recent event\n\
  continuation of recent event\n\
[2026-11-20 08:00:00] odd old line without level\n\
[2026-11-24 09:05:00.000] [INFO] [app] later event\n";
        let kept = filter_raw_log_since(log, Some("2026-11-24 00:00:00"));
        assert!(!kept.contains("old event"));
        assert!(!kept.contains("continuation of old event"));
        assert!(!kept.contains("orphan"));
        assert!(kept.contains("recent event"));
        assert!(kept.contains("continuation of recent event"));
        assert!(kept.contains("later event"));
        assert!(!kept.contains("odd old line"));

        assert_eq!(filter_raw_log_since(log, None), log);
        assert!(filter_raw_log_since(log, Some("2026-12-01 00:00:00")).is_empty());
    }
}
