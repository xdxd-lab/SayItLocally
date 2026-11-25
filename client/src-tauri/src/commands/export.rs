use serde::Deserialize;
use std::fs;
use std::io::{Seek, Write};
use std::path::Path;
use tauri::Emitter;

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

#[tauri::command]
pub fn save_text_export(payload: TextExportPayload) -> Result<Option<String>, String> {
    let path = resolve_export_path(&payload.default_path)?;
    check_export_extension(&path, &["json", "csv", "txt", "md"])?;
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(&path, &payload.content).map_err(|e| e.to_string())?;
    let abs = fs::canonicalize(&path).unwrap_or(path);
    let abs_str = abs.to_string_lossy().to_string();
    let clean = abs_str.strip_prefix(r"\\?\").unwrap_or(&abs_str).to_string();
    Ok(Some(clean))
}

#[tauri::command]
pub fn save_export_bundle(payload: ExportBundlePayload) -> Result<Option<String>, String> {
    let path = resolve_export_path(&payload.default_path)?;
    check_export_extension(&path, &["zip"])?;
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let file = fs::File::create(&path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    for item in &payload.files {
        super::archive::write_deflated_entry(&mut zip, &item.name, item.content.as_bytes())?;
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
) -> Result<Option<String>, String> {
    let out_path = std::path::PathBuf::from(&payload.default_path);
    check_export_extension(&out_path, &["zip"])?;
    if let Some(parent) = out_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let json_files = payload.files;
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
        for item in &json_files {
            current += 1;
            let _ = app_handle.emit("export-progress", ExportProgress {
                current, total, filename: item.name.clone(),
            });
            super::archive::write_deflated_entry(&mut zip, &item.name, item.content.as_bytes())?;
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
