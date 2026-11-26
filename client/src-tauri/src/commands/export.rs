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

/// 导出命令会把前端给的内容写到前端给的路径上。内容和位置都由前端决定，
/// 这里至少把扩展名钉死，免得被拿来往开机启动目录之类的地方写 .bat / .exe。
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

/// [自訂] SC-56：JSON 在寫出前去除本機絕對路徑。CSV、TXT、MD 由前端只放文字欄位，不含路徑欄位，原樣寫出。
/// 記憶體庫的檢查在呼叫端（prepare_text_export、prepare_files），在建檔之前。
fn export_content(name: &str, content: &str) -> Result<String, String> {
    if !crate::path_guard::has_extension(Path::new(name), &["json"]) {
        return Ok(content.to_string());
    }
    let mut document: serde_json::Value = serde_json::from_str(content)
        .map_err(|error| format!("Export content is not valid JSON: {error}"))?;
    super::backup::scrub_export_document(&mut document);
    serde_json::to_string_pretty(&document).map_err(|error| error.to_string())
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
    let (path, content) = prepare_text_export(storage.inner(), &payload)?;
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(&path, content).map_err(|e| e.to_string())?;
    let abs = fs::canonicalize(&path).unwrap_or(path);
    let abs_str = abs.to_string_lossy().to_string();
    let clean = abs_str.strip_prefix(r"\\?\").unwrap_or(&abs_str).to_string();
    Ok(Some(clean))
}

#[tauri::command]
pub fn save_export_bundle(payload: ExportBundlePayload, storage: tauri::State<'_, Storage>) -> Result<Option<String>, String> {
    let files = prepare_files(storage.inner(), &payload.files)?;
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
    let abs = fs::canonicalize(&path).unwrap_or(path);
    let abs_str = abs.to_string_lossy().to_string();
    let clean = abs_str.strip_prefix(r"\\?\").unwrap_or(&abs_str).to_string();
    Ok(Some(clean))
}

/// 完整导出：JSON 文件 + 音频目录，带进度事件
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

        // 写入 JSON 文件
        for (name, content) in &json_files {
            current += 1;
            let _ = app_handle.emit("export-progress", ExportProgress {
                current, total, filename: name.clone(),
            });
            super::archive::write_deflated_entry(&mut zip, name, content.as_bytes())?;
        }

        // 写入音频文件
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
        let broken = vec![ExportFile { name: "x.json".into(), content: "{".into() }];
        assert!(prepare_files(&disk, &broken).is_err());
        drop(disk);
        fs::remove_dir_all(dir).unwrap();
    }
}
