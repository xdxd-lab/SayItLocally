// 標準測試音訊 — 生成一段包含正弦波的 PCM 音訊用於 ASR 速度測試
// 實際使用時應該用真實的語音錄音，這裡先用靜音+提示

use serde::Serialize;
use tauri::Manager;

#[derive(Debug, Clone, Serialize)]
pub struct AsrBenchmarkResult {
    pub text: String,
    pub elapsed_ms: u64,
    pub audio_duration_sec: f64,
    pub model_id: String,
    pub rtf: f64, // Real-Time Factor: elapsed / audio_duration (越小越快)
}

/// 執行 ASR 速度測試（Tauri command）
/// 使用內建的中文測試音訊或使用者提供的音訊
#[tauri::command]
pub async fn run_asr_benchmark(
    app_handle: tauri::AppHandle,
    model_id: String,
    language: Option<String>,
    audio_b64: Option<String>,
) -> Result<AsrBenchmarkResult, String> {
    let pcm_bytes = if let Some(b64) = audio_b64 {
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &b64)
            .map_err(|e| format!("Failed to decode base64 audio: {}", e))?
    } else {
        // 讀取內建測試音訊（WAV 格式，需要跳過 44 位元組 header 提取 PCM）
        let resource_path = app_handle.path()
            .resolve("resources/test_zh.wav", tauri::path::BaseDirectory::Resource)
            .map_err(|e| format!("Could not locate test audio: {}", e))?;
        let wav_bytes = std::fs::read(&resource_path)
            .map_err(|e| format!("Failed to read test audio: {}", e))?;
        // 跳過 WAV header（44 位元組）
        if wav_bytes.len() > 44 {
            wav_bytes[44..].to_vec()
        } else {
            return Err("The test audio file is invalid".into());
        }
    };

    let audio_duration_sec = (pcm_bytes.len() as f64) / 2.0 / 16000.0;

    let samples: Vec<f32> = pcm_bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
        .collect();

    let lang = language.unwrap_or_else(|| "auto".into());

    tokio::task::spawn_blocking(move || {
        // 預熱與計時分開：這裡量的是純解碼耗時，不含模型載入
        // 加速器與顯示卡都按"自動"跑分，與這個命令原有的行為一致。
        super::gguf_asr::preload(&model_id, "auto", "")?;

        let start = std::time::Instant::now();
        let text = super::gguf_asr::transcribe(&model_id, &lang, "auto", "", &samples, 16000)?;
        let elapsed_ms = start.elapsed().as_millis() as u64;
        Ok(benchmark_result(&text, &lang, elapsed_ms, audio_duration_sec, model_id))
    })
    .await
    .map_err(|e| format!("Test failed: {}", e))?
}

/// [自訂] SC-67：辨識測試的回傳結果。文字轉成台灣正體（轉換不計入辨識耗時）。
fn benchmark_result(
    raw_text: &str,
    lang: &str,
    elapsed_ms: u64,
    audio_duration_sec: f64,
    model_id: String,
) -> AsrBenchmarkResult {
    let rtf = if audio_duration_sec > 0.0 {
        (elapsed_ms as f64 / 1000.0) / audio_duration_sec
    } else {
        0.0
    };
    AsrBenchmarkResult {
        text: crate::zh_output::to_taiwan(raw_text, lang),
        elapsed_ms,
        audio_duration_sec,
        model_id,
        rtf,
    }
}

/// 獲取內建測試音訊的 base64 WAV 資料（供前端播放）
#[tauri::command]
pub async fn get_test_audio_b64(app_handle: tauri::AppHandle) -> Result<String, String> {
    let resource_path = app_handle.path()
        .resolve("resources/test_zh.wav", tauri::path::BaseDirectory::Resource)
        .map_err(|e| format!("Could not locate test audio: {}", e))?;
    let wav_bytes = std::fs::read(&resource_path)
        .map_err(|e| format!("Failed to read test audio: {}", e))?;
    Ok(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &wav_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_result_returns_taiwan_traditional_text() {
        // 「語音輸入法測試成功。」的簡體寫法（跳脫保存）
        let raw = "\u{8bed}\u{97f3}\u{8f93}\u{5165}\u{6cd5}\u{6d4b}\u{8bd5}\u{6210}\u{529f}\u{3002}";
        let result = benchmark_result(raw, "auto", 500, 2.0, "m".into());
        assert_eq!(result.text, "語音輸入法測試成功。");
        assert_eq!(result.elapsed_ms, 500);
        assert!((result.rtf - 0.25).abs() < 1e-9);
        assert_eq!(benchmark_result(raw, "auto", 1, 0.0, "m".into()).rtf, 0.0);
    }
}
