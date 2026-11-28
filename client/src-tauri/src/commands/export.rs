use serde::Deserialize;
use std::fs;
use std::io::{Seek, Write};
use std::path::Path;
use tauri::Emitter;

use crate::storage::Storage;

#[derive(Deserialize)]
pub struct TextExportPayload {
    #[serde(rename = "defaultPath")]
    pub default_path: String,
    pub content: String,
    #[allow(dead_code)]
    pub filters: Option<Vec<ExportFilter>>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct ExportFilter {
    pub name: String,
    pub extensions: Vec<String>,
}

#[derive(Deserialize)]
pub struct ExportBundlePayload {
    #[serde(rename = "defaultPath")]
    pub default_path: String,
    pub files: Vec<ExportFile>,
}

#[derive(Deserialize)]
pub struct ExportFile {
    pub name: String,
    pub content: String,
}

/// 匯出命令會把前端給的內容寫到前端給的路徑上。內容和位置都由前端決定，
/// 這裡至少把副檔名釘死，免得被拿來往開機啟動目錄之類的地方寫 .bat / .exe。
fn check_export_extension(path: &std::path::Path, allowed: &[&str]) -> Result<(), String> {
    if crate::path_guard::has_extension(path, allowed) {
        Ok(())
    } else {
        Err(format!("Export file must end with .{}", allowed.join(" / .")))
    }
}

/// [自訂] SCAN1-12（L-82）：前端只給檔名（相對路徑）時，原本會寫進程序的工作目錄（位置不明）。
/// 改成固定寫到「下載／SayItLocally Exports」，和備份同一層，使用者找得到、也知道要自行管理。
fn resolve_export_path(default_path: &str) -> Result<std::path::PathBuf, String> {
    let path = std::path::PathBuf::from(default_path);
    if path.is_absolute() {
        return Ok(path);
    }
    // REV1：相對路徑只接受單一檔名，不允許 `..` 或子目錄跳出匯出資料夾。
    let mut parts = path.components();
    let name = match (parts.next(), parts.next()) {
        (Some(std::path::Component::Normal(name)), None) => name.to_owned(),
        _ => return Err("Export file name must not contain a directory".to_string()),
    };
    Ok(dirs::download_dir()
        .or_else(dirs::document_dir)
        .unwrap_or_else(|| {
            crate::commands::app_data_dir()
        })
        .join("SayItLocally Exports")
        .join(name))
}

/// [自訂] SC-56：JSON 在寫出前去除本機絕對路徑。
/// [自訂] SC-59（L-97）：CSV 依表頭找出中繼資料欄，值整格是路徑時清空該格，其餘位元組不變。
/// TXT、MD 目前沒有產生端，也沒有中繼資料欄，原樣寫出。
/// 記憶體庫的檢查在呼叫端（prepare_text_export、prepare_files），在建檔之前。
fn export_content(name: &str, content: &str) -> Result<String, String> {
    if crate::path_guard::has_extension(Path::new(name), &["csv"]) {
        return scrub_csv(content);
    }
    if !crate::path_guard::has_extension(Path::new(name), &["json"]) {
        return Ok(content.to_string());
    }
    let mut document: serde_json::Value = serde_json::from_str(content)
        .map_err(|error| format!("Export content is not valid JSON: {error}"))?;
    super::backup::scrub_export_document(&mut document);
    serde_json::to_string_pretty(&document).map_err(|error| error.to_string())
}

/// 依前端 `escapeCsv` 的寫法切格：逗號分欄、`\n` 分列，雙引號包住的格內 `""` 代表一個引號。
/// 回傳每列每格在原文中的位元組範圍。
fn csv_cells(content: &str) -> Result<Vec<Vec<std::ops::Range<usize>>>, String> {
    let bytes = content.as_bytes();
    let (mut rows, mut row, mut start, mut quoted) = (Vec::new(), Vec::new(), 0, false);
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'"' => quoted = !quoted,
            b',' if !quoted => {
                row.push(start..index);
                start = index + 1;
            }
            b'\n' if !quoted => {
                row.push(start..index);
                rows.push(std::mem::take(&mut row));
                start = index + 1;
            }
            _ => {}
        }
    }
    if quoted {
        return Err("Export content is not valid CSV: unbalanced quotes".to_string());
    }
    row.push(start..bytes.len());
    rows.push(row);
    Ok(rows)
}

fn csv_value(cell: &str) -> String {
    match cell.strip_prefix('"').and_then(|inner| inner.strip_suffix('"')) {
        Some(inner) => inner.replace("\"\"", "\""),
        None => cell.to_string(),
    }
}

fn scrub_csv(content: &str) -> Result<String, String> {
    let rows = csv_cells(content)?;
    let Some((header, records)) = rows.split_first() else { return Ok(content.to_string()) };
    let columns: Vec<usize> = header.iter().enumerate()
        .filter(|(_, cell)| super::backup::is_metadata_field(&csv_value(&content[(*cell).clone()])))
        .map(|(index, _)| index)
        .collect();
    let mut cleared: Vec<std::ops::Range<usize>> = records.iter()
        .flat_map(|row| columns.iter().filter_map(|column| row.get(*column)))
        .filter(|cell| super::backup::is_local_path(&csv_value(&content[(*cell).clone()])))
        .cloned()
        .collect();
    cleared.sort_by_key(|cell| cell.start);
    let mut output = String::with_capacity(content.len());
    let mut copied = 0;
    for cell in cleared {
        output.push_str(&content[copied..cell.start]);
        copied = cell.end;
    }
    output.push_str(&content[copied..]);
    Ok(output)
}

fn prepare_text_export(storage: &Storage, payload: &TextExportPayload) -> Result<(std::path::PathBuf, String), String> {
    storage.ensure_exportable()?;
    let path = resolve_export_path(&payload.default_path)?;
    check_export_extension(&path, &["json", "csv", "txt", "md"])?;
    let content = export_content(&path.to_string_lossy(), &payload.content)?;
    Ok((path, content))
}

fn prepare_files(storage: &Storage, files: &[ExportFile]) -> Result<Vec<(String, String)>, String> {
    storage.ensure_exportable()?;
    files.iter().map(|item| Ok((item.name.clone(), export_content(&item.name, &item.content)?))).collect()
}

#[tauri::command]
pub fn save_text_export(payload: TextExportPayload, storage: tauri::State<'_, Storage>) -> Result<Option<String>, String> {
    let path = write_text_export(storage.inner(), &payload)?;
    let abs = fs::canonicalize(&path).unwrap_or(path);
    let abs_str = abs.to_string_lossy().to_string();
    let clean = abs_str.strip_prefix(r"\\?\").unwrap_or(&abs_str).to_string();
    Ok(Some(clean))
}

// [自訂] SC-56：指令層需要 tauri::State，寫檔流程放在這裡，記憶體庫的檢查在建資料夾之前並可測試。
fn write_text_export(storage: &Storage, payload: &TextExportPayload) -> Result<std::path::PathBuf, String> {
    let (path, content) = prepare_text_export(storage, payload)?;
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(&path, content).map_err(|e| e.to_string())?;
    Ok(path)
}

fn write_export_bundle(storage: &Storage, payload: &ExportBundlePayload) -> Result<std::path::PathBuf, String> {
    let files = prepare_files(storage, &payload.files)?;
    let path = resolve_export_path(&payload.default_path)?;
    check_export_extension(&path, &["zip"])?;
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let file = fs::File::create(&path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    for (name, content) in &files {
        super::archive::write_deflated_entry(&mut zip, name, content.as_bytes())?;
    }

    zip.finish().map_err(|e| e.to_string())?;
    Ok(path)
}

#[tauri::command]
pub fn save_export_bundle(payload: ExportBundlePayload, storage: tauri::State<'_, Storage>) -> Result<Option<String>, String> {
    let path = write_export_bundle(storage.inner(), &payload)?;
    let abs = fs::canonicalize(&path).unwrap_or(path);
    let abs_str = abs.to_string_lossy().to_string();
    let clean = abs_str.strip_prefix(r"\\?\").unwrap_or(&abs_str).to_string();
    Ok(Some(clean))
}

/// 完整匯出：JSON 檔案 + 音訊目錄，帶進度事件
#[derive(Deserialize)]
pub struct FullExportPayload {
    #[serde(rename = "defaultPath")]
    pub default_path: String,
    pub files: Vec<ExportFile>,
}

#[derive(serde::Serialize, Clone)]
struct ExportProgress {
    current: usize,
    total: usize,
    filename: String,
}

#[tauri::command]
pub async fn save_full_export(
    app: tauri::AppHandle,
    payload: FullExportPayload,
    storage: tauri::State<'_, Storage>,
) -> Result<Option<String>, String> {
    let json_files = prepare_files(storage.inner(), &payload.files)?;
    let out_path = std::path::PathBuf::from(&payload.default_path);
    check_export_extension(&out_path, &["zip"])?;
    if let Some(parent) = out_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let app_handle = app.clone();

    // [自訂] S71：列舉、讀檔及封裝在同一錄音鎖內。
    tokio::task::spawn_blocking(move || {
        let _recordings = super::audio::lock_recordings();
        let audio_files = super::audio::retained_recordings()?;
        let total = json_files.len() + audio_files.len();
        let file = fs::File::create(&out_path).map_err(|e| e.to_string())?;
        let mut zip = zip::ZipWriter::new(file);
        let mut current = 0usize;

        // 寫入 JSON 檔案
        for (name, content) in &json_files {
            current += 1;
            let _ = app_handle.emit("export-progress", ExportProgress {
                current, total, filename: name.clone(),
            });
            super::archive::write_deflated_entry(&mut zip, name, content.as_bytes())?;
        }

        // 寫入音訊檔案
        for audio_path in &audio_files {
            current += 1;
            let fname = audio_path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".into());
            let _ = app_handle.emit("export-progress", ExportProgress {
                current, total, filename: fname.clone(),
            });
            write_export_recording(&mut zip, &fname, audio_path)?;
        }

        zip.finish().map_err(|e| e.to_string())?;
        let abs = fs::canonicalize(&out_path).unwrap_or(out_path);
        let abs_str = abs.to_string_lossy().to_string();
        let clean = abs_str.strip_prefix(r"\\?\").unwrap_or(&abs_str).to_string();
        Ok(Some(clean))
    })
    .await
    .map_err(|e| format!("Export task failed: {}", e))?
}

fn write_export_recording<W: Write + Seek>(
    zip: &mut zip::ZipWriter<W>,
    name: &str,
    path: &Path,
) -> Result<(), String> {
    let read_error = |error| format!("Failed to read audio file {name}: {error}");
    let mut source = fs::File::open(path).map_err(read_error)?;
    let length = source.metadata().map_err(read_error)?.len();
    let options = super::archive::file_options(zip::CompressionMethod::Stored, length);
    zip.start_file(format!("audio/{name}"), options).map_err(|e| e.to_string())?;
    std::io::copy(&mut source, zip).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read};

    #[test]
    fn full_export_recording_keeps_the_audio_name_and_stored_method() {
        let source = std::env::temp_dir().join(format!("sayit-export-test-{}.wav", uuid::Uuid::new_v4()));
        fs::write(&source, b"recording").unwrap();
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        write_export_recording(&mut writer, "example.wav", &source).unwrap();
        let mut archive = zip::ZipArchive::new(writer.finish().unwrap()).unwrap();
        let mut recording = archive.by_name("audio/example.wav").unwrap();
        assert_eq!(recording.compression(), zip::CompressionMethod::Stored);
        let mut bytes = Vec::new();
        recording.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"recording");
        fs::remove_file(source).unwrap();
    }
}

#[cfg(test)]
mod sc56_tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn exports_refuse_volatile_storage_and_drop_local_paths_from_json() {
        let dir = std::env::temp_dir().join(format!("sayit-export-test-{}", uuid::Uuid::new_v4()));
        let disk = Storage::new(dir.join("test.db")).unwrap();
        let memory = Storage::new(std::path::PathBuf::from(":memory:")).unwrap();
        let history = json!({
            "total": 1,
            "filters": { "keyword": "/home/someone" },
            "records": [{ "id": "abc123", "audioFilePath": "C:\\x\\abc123.wav", "windowTitle": "~/Desktop", "asrText": "say /tmp/x" }],
        }).to_string();
        let text = TextExportPayload { default_path: dir.join("h.json").to_string_lossy().into(), content: history.clone(), filters: None };
        let files = vec![
            ExportFile { name: "history.json".into(), content: history.clone() },
            ExportFile { name: "notes.csv".into(), content: "id,text\nx,/tmp/kept".into() },
        ];
        for result in [
            prepare_text_export(&memory, &text).map(|_| ()),
            prepare_files(&memory, &files).map(|_| ()),
        ] {
            assert_eq!(result, Err(crate::storage::VOLATILE_EXPORT_ERROR.into()));
        }
        assert!(!dir.join("h.json").exists());

        let expected = json!({
            "total": 1, "filters": { "keyword": "/home/someone" },
            "records": [{ "id": "abc123", "audioFilePath": "abc123.wav", "asrText": "say /tmp/x" }],
        });
        let (path, content) = prepare_text_export(&disk, &text).unwrap();
        assert_eq!(path, dir.join("h.json"));
        assert_eq!(serde_json::from_str::<Value>(&content).unwrap(), expected);
        let prepared = prepare_files(&disk, &files).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&prepared[0].1).unwrap(), expected);
        assert_eq!(prepared[1].1, files[1].content);
        // Nothing is created for an in-memory database, not even the missing folder.
        let missing = dir.join("missing");
        let text = TextExportPayload { default_path: missing.join("h.json").to_string_lossy().into(), content: history.clone(), filters: None };
        let bundle = ExportBundlePayload { default_path: missing.join("b.zip").to_string_lossy().into(), files: vec![] };
        assert_eq!(write_text_export(&memory, &text), Err(crate::storage::VOLATILE_EXPORT_ERROR.into()));
        assert_eq!(write_export_bundle(&memory, &bundle), Err(crate::storage::VOLATILE_EXPORT_ERROR.into()));
        assert!(!missing.exists());
        assert!(write_text_export(&disk, &text).unwrap().is_file());
        assert!(write_export_bundle(&disk, &bundle).unwrap().is_file());

        // The hotword export keeps every word, even one that is a whole path.
        let hotwords = json!({
            "exportedAt": "2026-11-26T00:00:00Z",
            "activeWords": ["/dev/null", "kept"],
            "manualHotwords": ["~/notes", "kept"],
            "builtinSetWords": { "set1": ["/usr/bin", "kept"] },
            "builtinSetExtraWords": { "set1": ["C:\\Tools", "kept"] },
            "customThemes": [{ "id": "t", "name": "/dev", "words": ["/opt"] }],
            "hotwordLearning": { "candidates": [{ "word": "/etc/hosts", "count": 3 }], "learned": ["~/bin"] },
        });
        let files = vec![ExportFile { name: "hotwords.json".into(), content: hotwords.to_string() }];
        let prepared = prepare_files(&disk, &files).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&prepared[0].1).unwrap(), hotwords);
        let text = TextExportPayload { default_path: dir.join("hotwords.json").to_string_lossy().into(), content: hotwords.to_string(), filters: None };
        assert_eq!(serde_json::from_str::<Value>(&prepare_text_export(&disk, &text).unwrap().1).unwrap(), hotwords);

        let broken = vec![ExportFile { name: "x.json".into(), content: "{".into() }];
        assert!(prepare_files(&disk, &broken).is_err());
        drop(disk);
        fs::remove_dir_all(dir).unwrap();
    }

    // [自訂] SC-59：與前端 historyToCsv 的測試共用 fixture。
    #[test]
    fn csv_exports_clear_only_metadata_cells_that_are_paths() {
        let source = include_str!("testdata/history-export.csv");
        let expected = include_str!("testdata/history-export.scrubbed.csv");
        assert_eq!(super::export_content("history.csv", source).unwrap(), expected);
        assert_eq!(super::export_content("HISTORY.CSV", source).unwrap(), expected);
        // 沒有路徑時逐位元組不變；使用者文字欄即使整格是路徑也保留。
        assert_eq!(super::export_content("history.csv", expected).unwrap(), expected);
        let content = "asrText,appName\n/tmp/kept,\"Notes, Inc\"\n\"$HOME\",smb://nas/x";
        assert_eq!(super::export_content("a.csv", content).unwrap(), "asrText,appName\n/tmp/kept,\"Notes, Inc\"\n\"$HOME\",");
        assert!(super::export_content("a.csv", "appName\n\"unterminated").is_err());
        assert_eq!(super::export_content("a.txt", "appName\n/tmp/x").unwrap(), "appName\n/tmp/x");
    }
}
