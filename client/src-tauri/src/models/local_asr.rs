// 本地 ASR 的命令層 —— 推理本身在 gguf_asr.rs（transcribe.cpp / ggml / GGUF）。
//
// 這裡負責：解 base64 → i16 轉 f32 → Silero VAD 確認存在人聲並裁剪首尾
// → 交給引擎；以及回收上一代 ONNX 模型。原始錄音不會被修改。
//
// 歷史：0.1.3 之前本地識別走 sherpa-onnx（ONNX Runtime，純 CPU、INT8 動態量化）。
// Qwen3-ASR 這類自迴歸模型在那條路上慢一個量級，已整體換成 GGUF/ggml，
// sherpa-onnx 依賴連同 Whisper/Paraformer/FireRed/FunASR 的載入器一併刪除。
// 原因與實測數字見 dev-docs/local-asr-ggml-migration.md。

use serde::Serialize;
use std::path::Path;
use std::time::Instant;

use super::downloader;
use super::gguf_asr;

/// 取樣率固定 16 kHz —— 前端採集就重取樣到這個值，引擎也只接受這個值。
const SR: usize = 16000;

/// 一次交給模型的最長音訊。超過就在停頓處切塊、逐塊識別再拼起來。
///
/// 原來是「模型吃得下多長就一次喂多長」（Qwen3-ASR 上限約 87 分鐘），結果 220 秒的
/// 錄音在整合顯示卡上一次跑太久，觸發 Windows 顯示卡看門狗、顯示卡被重置，之後本地識別
/// 全部失敗（見 local_vad::plan_chunks 的說明）。30 秒：日常口述基本不切、結果與原來
/// 一致；實測 1.7B 在 Arc 140T 上 30 s 一塊約 5 s，單個任務離 2 秒上限很遠；
/// 也正好是 SenseVoice 的上限，那邊不再走按固定長度硬切的舊分段。
/// 代價：塊與塊之間互相看不到上下文，切口兩邊的標點、專有名詞偶爾會受影響。
const MAX_CHUNK_SEC: usize = 30;

#[derive(Debug, Clone, Serialize)]
pub struct LocalAsrResult {
    pub text: String,
    pub elapsed_ms: u64,
}

fn decode_pcm(audio_b64: &str) -> Result<Vec<f32>, String> {
    let pcm_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        audio_b64,
    )
    .map_err(|e| format!("Failed to decode base64 audio: {}", e))?;

    if pcm_bytes.len() % 2 != 0 {
        return Err(format!(
            "PCM data length must be a multiple of a 16-bit sample; received {} bytes",
            pcm_bytes.len()
        ));
    }

    Ok(pcm_bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
        .collect())
}

#[tauri::command]
pub async fn local_transcribe(
    audio_b64: String,
    model_id: String,
    language: Option<String>,
    accelerator: Option<String>,
    // gpu_device：使用者指定的顯示卡（`GgufDevice::id`）。預設 / 空 = 交給庫自動挑。
    gpu_device: Option<String>,
) -> Result<LocalAsrResult, String> {
    let samples = decode_pcm(&audio_b64).map_err(|e| {
        crate::providers::diag::fail("local/asr", "decode_pcm", e)
    })?;
    transcribe_samples(samples, model_id, language, accelerator, gpu_device).await
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalTranscribeMeta {
    model_id: String,
    language: Option<String>,
    accelerator: Option<String>,
    gpu_device: Option<String>,
}

/// 同 local_transcribe，但 PCM 以原始位元組發過來（見 ipc_body），省掉網頁裡的 base64 轉碼。
#[tauri::command]
pub async fn local_transcribe_raw(request: tauri::ipc::Request<'_>) -> Result<LocalAsrResult, String> {
    let (meta, pcm): (LocalTranscribeMeta, Vec<u8>) = crate::ipc_body::split_framed(&request)?;
    let samples = pcm
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
        .collect();
    transcribe_samples(samples, meta.model_id, meta.language, meta.accelerator, meta.gpu_device).await
}

async fn transcribe_samples(
    samples: Vec<f32>,
    model_id: String,
    language: Option<String>,
    accelerator: Option<String>,
    gpu_device: Option<String>,
) -> Result<LocalAsrResult, String> {
    if samples.is_empty() {
        crate::providers::diag::empty_result("local/asr", "Input audio was empty; model inference was skipped");
        return Ok(LocalAsrResult { text: String::new(), elapsed_ms: 0 });
    }

    let accel = accelerator.unwrap_or_else(|| "auto".to_string());
    let gpu = gpu_device.unwrap_or_default();
    tokio::task::spawn_blocking(move || {
        use crate::providers::diag;
        let lang = language.as_deref().unwrap_or("auto");
        let start = Instant::now();
        let audio_sec = samples.len() as f64 / SR as f64;
        diag::log(
            "local/asr",
            "start",
            &format!(
                "model={} accel={} lang={} audio_sec={:.1}",
                model_id, accel, lang, audio_sec
            ),
        );
        // 先砍掉首尾靜音：解碼耗時基本與音訊長度成正比，兩頭的空白是純浪費。
        // 長錄音再按停頓切塊（MAX_CHUNK_SEC），短錄音只有一塊，等同於只裁首尾。
        let segments = match super::local_vad::detect_speech_segments(&samples, SR) {
            Ok(segments) if !segments.is_empty() => segments,
            Ok(_) => {
                // 這是「真的沒人說話」，與識別失敗要分得清 —— 前端兩種都顯示
                // 「未檢測到有效聲音」，只有日誌能區分
                diag::empty_result(
                    "local/asr",
                    &format!("VAD detected no speech; model inference was skipped audio_sec={:.1}", audio_sec),
                );
                return Ok(LocalAsrResult {
                    text: String::new(),
                    elapsed_ms: start.elapsed().as_millis() as u64,
                });
            }
            Err(e) => {
                // 防幻覺是硬約束：無法可靠判斷是否有人聲時，絕不能回退到會把靜音
                // 送進生成式模型的舊路徑。保留錯誤便於診斷 VAD/本機環境問題。
                return Err(diag::fail(
                    "local/asr",
                    "vad",
                    format!("Voice activity detection failed; recognition was canceled: {e}"),
                ));
            }
        };
        let chunks = super::local_vad::plan_chunks(&samples, &segments, MAX_CHUNK_SEC * SR);
        if chunks.len() > 1 {
            // 排查「長錄音為什麼慢 / 切口處丟字」時先看這一行：有它才說明走了切塊
            diag::log(
                "local/asr",
                "chunked",
                &format!(
                    "audio_sec={:.1} chunks={} longest_sec={:.1}",
                    audio_sec,
                    chunks.len(),
                    chunks.iter().map(|c| c.end - c.start).max().unwrap_or(0) as f64 / SR as f64
                ),
            );
        }
        let text = transcribe_chunks(&model_id, lang, &accel, &gpu, &samples, &chunks)
            .map_err(|e| {
                // [自訂] SC-35：保留載入端的錯誤碼，不再包一層通用錯誤。
                if e.starts_with("sayit_error:local_model_") {
                    diag::log("local/asr", "transcribe FAILED", &e);
                    e
                } else {
                    diag::fail("local/asr", "transcribe", e)
                }
            })?;
        let elapsed_ms = start.elapsed().as_millis() as u64;
        if text.trim().is_empty() {
            // VAD 認定有人聲、模型卻什麼都沒輸出 —— 模型/量化檔/加速器組合有問題的訊號
            diag::empty_result(
                "local/asr",
                &format!(
                    "VAD detected speech but the model produced no output model={} accel={} audio_sec={:.1} elapsed={}ms",
                    model_id, accel, audio_sec, elapsed_ms
                ),
            );
        } else {
            diag::ok("local/asr", elapsed_ms, text.chars().count());
        }
        Ok(LocalAsrResult { text, elapsed_ms })
    })
    .await
    .map_err(|e| {
        crate::providers::diag::fail("local/asr", "join", format!("Inference task failed: {}", e))
    })?
}

/// 逐塊識別再按順序拼接。任何一塊失敗整次算失敗：拼出半段文字插進去，
/// 比明確報錯更難讓使用者發現少了內容。
fn transcribe_chunks(
    model_id: &str,
    lang: &str,
    accel: &str,
    gpu: &str,
    samples: &[f32],
    chunks: &[std::ops::Range<usize>],
) -> Result<String, String> {
    let mut parts = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        parts.push(gguf_asr::transcribe(model_id, lang, accel, gpu, &samples[chunk.clone()], SR)?);
    }
    Ok(assemble_transcript(&parts, lang))
}

/// [自訂] SC-67：辨識文字的單一出口。每一段先各自轉成台灣正體再串接，
/// 被判成日文的一段（含假名）不會連累其他段；段內再逐句判斷（見 `zh_output::to_taiwan`）。
fn assemble_transcript(parts: &[String], lang: &str) -> String {
    let mut text = String::new();
    for part in parts {
        gguf_asr::append_segment_text(&mut text, &crate::zh_output::to_taiwan(part, lang));
    }
    text
}

#[tauri::command]
pub async fn preload_local_model(
    model_id: String,
    accelerator: Option<String>,
    // gpu_device：使用者指定的顯示卡（`GgufDevice::id`）。預設 / 空 = 交給庫自動挑。
    gpu_device: Option<String>,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let start = Instant::now();
        let accel = accelerator.as_deref().unwrap_or("auto");
        let gpu = gpu_device.as_deref().unwrap_or("");
        gguf_asr::preload(&model_id, accel, gpu)?;
        Ok(format!("Model loaded ({}ms)", start.elapsed().as_millis()))
    })
    .await
    .map_err(|e| format!("Preload task failed: {}", e))?
}

/// 釋放本地 ASR 模型佔用的記憶體。切換到雲 API / 伺服器模式時呼叫，
/// 否則常駐的權重（幾百 MB ~ 數 GB）會一直佔用到應用退出。
#[tauri::command]
pub async fn unload_local_model() -> Result<(), String> {
    tokio::task::spawn_blocking(gguf_asr::unload)
        .await
        .map_err(|e| format!("Release task failed: {}", e))
}

/// 動態調整本地模型的空閒解除安裝時間。0 = 常駐；其餘值由設定頁限制為
/// 10 / 30 / 60 分鐘。守護執行緒只啟動一次，這裡僅更新它讀取的原子配置。
#[tauri::command]
pub fn set_local_model_idle_unload(idle_minutes: u64) -> Result<(), String> {
    if !matches!(idle_minutes, 0 | 10 | 30 | 60) {
        return Err(format!("Unsupported model idle unload interval: {idle_minutes}"));
    }
    gguf_asr::set_idle_unload_minutes(idle_minutes);
    Ok(())
}

// ── 上一代 ONNX 模型的回收 ──

// Filenames from the last ONNX catalog. Older layouts without a verified
// manifest are left alone, even when they have a familiar directory name.
const LEGACY_MODEL_FILES: &[(&str, &[&str])] = &[
    ("sensevoice-small", &["model.int8.onnx", "tokens.txt"]),
    ("qwen3-asr-0.6b", &["conv_frontend.onnx", "encoder.int8.onnx", "decoder.int8.onnx"]),
];

fn real_entry(path: &Path) -> Option<std::fs::Metadata> {
    let entry = path.symlink_metadata().ok()?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if entry.file_attributes() & 0x400 != 0 { // FILE_ATTRIBUTE_REPARSE_POINT
            return None;
        }
    }
    (!entry.file_type().is_symlink()).then_some(entry)
}

fn remove_legacy_file(path: &Path) -> u64 {
    let Some(entry) = real_entry(path).filter(std::fs::Metadata::is_file) else {
        return 0;
    };
    match std::fs::remove_file(path) {
        Ok(()) => entry.len(),
        Err(error) => {
            log::warn!("Failed to remove legacy model file: {}", error);
            0
        }
    }
}

fn reclaim_legacy_models(default: &Path, is_custom: bool) -> u64 {
    if is_custom || !real_entry(default).is_some_and(|entry| entry.is_dir()) {
        return 0;
    }

    let mut freed = remove_legacy_file(&default.join("silero_vad.onnx"));
    for &(id, files) in LEGACY_MODEL_FILES {
        let dir = default.join(id);
        if !real_entry(&dir).is_some_and(|entry| entry.is_dir()) {
            continue;
        }
        let has_known_weights = files.iter().any(|file| {
            file.ends_with(".onnx") && real_entry(&dir.join(file)).is_some_and(|entry| entry.is_file())
        });
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        // If an entry is unreadable, or this directory contains GGUF weights,
        // leave the mixed layout alone.
        if !has_known_weights || entries.into_iter().any(|entry| {
            entry.map_or(true, |entry| entry.path().extension().is_some_and(|ext| ext.eq_ignore_ascii_case("gguf")))
        }) {
            continue;
        }
        if id == "qwen3-asr-0.6b" {
            let tokenizer = dir.join("tokenizer");
            if real_entry(&tokenizer).is_some_and(|entry| entry.is_dir()) {
                for file in ["merges.txt", "vocab.json", "tokenizer_config.json"] {
                    freed += remove_legacy_file(&tokenizer.join(file));
                }
                let _ = std::fs::remove_dir(tokenizer);
            }
        }
        for file in files {
            freed += remove_legacy_file(&dir.join(file));
        }
        let _ = std::fs::remove_dir(dir);
    }

    if freed > 0 {
        log::info!("Removed {:.1} MB of legacy engine models", freed as f64 / 1024.0 / 1024.0);
    }
    freed
}

/// 供前端查詢本次啟動回收了多少空間（可用於提示"已釋放 XXX MB"）。
#[tauri::command]
pub fn legacy_models_reclaimed_bytes() -> u64 {
    RECLAIMED.load(std::sync::atomic::Ordering::Relaxed)
}

static RECLAIMED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 啟動時在後台回收舊模型（幾百 MB 的刪除別卡啟動路徑）。
/// [自訂] SC-55：記憶體庫或讀不到模型目錄設定時，無法確認使用者是否自選，不刪任何檔。
/// [自訂] SC-56：本次啟動經過任何復原（含換新庫，舊的自選設定已隨舊庫移走）也跳過。
pub fn spawn_legacy_reclaim(storage: &crate::storage::Storage) -> Option<std::thread::JoinHandle<u64>> {
    if storage.is_volatile() || storage.recovered_at_startup() || storage.custom_models_dir().is_err() {
        log::info!("Skipped legacy model cleanup because the model directory setting is unavailable");
        return None;
    }
    Some(std::thread::spawn(|| {
        let Ok(_storage_lease) = downloader::acquire_model_storage() else { return 0 };
        let freed = reclaim_legacy_models(&downloader::default_models_dir(), downloader::has_custom_models_dir());
        RECLAIMED.store(freed, std::sync::atomic::Ordering::Relaxed);
        freed
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembled_transcript_converts_each_chunk() {
        // 第一段：「軟體」的簡體寫法、沒有句末標點；第二段：日文（含假名）；第三段：「測試。」的簡體寫法。
        let parts = [
            "\u{8f6f}\u{4ef6}".to_string(),
            "\u{4eca}\u{65e5}\u{306f}".to_string(),
            " \u{6d4b}\u{8bd5}\u{3002} ".to_string(),
        ];
        assert_eq!(assemble_transcript(&parts, "auto"), "軟體 \u{4eca}\u{65e5}\u{306f} 測試。");
        assert_eq!(assemble_transcript(&parts[..1], "ja"), "\u{8f6f}\u{4ef6}");
        assert_eq!(assemble_transcript(&[], "auto"), "");
    }

    #[test]
    fn legacy_reclaim_preserves_custom_and_unknown_files() {
        use std::fs;
        let root = std::env::temp_dir().join(format!("sayit-legacy-test-{}", uuid::Uuid::new_v4()));
        for dir in ["whisper-tiny", "sensevoice-small/nested", "qwen3-asr-0.6b"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        let preserved = [
            "whisper-tiny/x.onnx",
            "whisper-tiny/mine.txt",
            "sensevoice-small/mine.txt",
            "sensevoice-small/nested/model.int8.onnx",
            "qwen3-asr-0.6b/encoder.int8.onnx",
            "qwen3-asr-0.6b/new.GGUF",
        ];
        let legacy = ["silero_vad.onnx", "sensevoice-small/model.int8.onnx", "sensevoice-small/tokens.txt"];
        for file in preserved.iter().chain(&legacy) {
            fs::write(root.join(file), b"keep").unwrap();
        }
        // The root can be the very same path explicitly saved as a custom setting.
        assert_eq!(reclaim_legacy_models(&root, true), 0);
        for file in preserved.iter().chain(&legacy) {
            assert_eq!(fs::read(root.join(file)).unwrap(), b"keep");
        }
        assert_eq!(reclaim_legacy_models(&root, false), 12);
        for file in preserved {
            assert_eq!(fs::read(root.join(file)).unwrap(), b"keep");
        }
        for file in legacy {
            assert!(!root.join(file).exists());
        }
        fs::remove_dir_all(root).unwrap();
    }

    // [自訂] SC-55：啟動入口自己讀「自選」旗標與預設根；設定不可用時不刪檔。
    #[test]
    fn startup_reclaim_uses_the_custom_flag_and_skips_unreadable_settings() {
        use crate::storage::Storage;
        use std::fs;
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                downloader::set_custom_models_dir(None);
            }
        }
        let _guard = downloader::TEST_MODEL_LEASES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _reset = Reset;
        let default = downloader::default_models_dir();
        let legacy = default.join("silero_vad.onnx");
        fs::create_dir_all(&default).unwrap();
        fs::write(&legacy, b"vad").unwrap();
        let dir = std::env::temp_dir().join(format!("sayit-reclaim-entry-{}", uuid::Uuid::new_v4()));
        let disk = Storage::new(dir.join("test.db")).unwrap();
        let memory = Storage::new(std::path::PathBuf::from(":memory:")).unwrap();
        assert!(spawn_legacy_reclaim(&memory).is_none());
        for broken in ["7", "not json"] {
            disk.db.lock().unwrap().execute(
                "INSERT OR REPLACE INTO app_settings (key, value_json, updated_at) VALUES ('localAsr.modelsDir', ?1, 0)",
                [broken],
            ).unwrap();
            assert!(spawn_legacy_reclaim(&disk).is_none(), "{broken}");
        }
        disk.set("localAsr.modelsDir", &serde_json::json!("")).unwrap();
        // The same folder saved as a custom choice is never cleaned.
        downloader::set_custom_models_dir(Some(default.clone()));
        assert_eq!(spawn_legacy_reclaim(&disk).unwrap().join().unwrap(), 0);
        assert_eq!(fs::read(&legacy).unwrap(), b"vad");
        downloader::set_custom_models_dir(None);
        // A replacement database no longer knows the old custom folder.
        fs::write(dir.join("damaged.db"), b"not a database").unwrap();
        let (replacement, mode) = Storage::open_or_recover(dir.join("damaged.db"));
        assert!(matches!(mode, Some(crate::storage::StorageRecovery::MovedAside { .. })), "{mode:?}");
        assert!(!replacement.is_volatile());
        assert!(spawn_legacy_reclaim(&replacement).is_none());
        assert_eq!(fs::read(&legacy).unwrap(), b"vad");
        drop(replacement);
        let (started, mode) = Storage::open_or_recover(dir.join("damaged.db"));
        assert!(mode.is_none());
        assert_eq!(spawn_legacy_reclaim(&started).unwrap().join().unwrap(), 3);
        assert!(!legacy.exists());
        drop((disk, started));
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn legacy_reclaim_never_follows_root_or_model_directory_links() {
        use std::{fs, os::unix::fs::symlink};
        let base = std::env::temp_dir().join(format!("sayit-legacy-links-{}", uuid::Uuid::new_v4()));
        let target = base.join("target");
        let default = base.join("models");
        fs::create_dir_all(target.join("sensevoice-small")).unwrap();
        fs::write(target.join("silero_vad.onnx"), b"vad").unwrap();
        fs::write(target.join("sensevoice-small/model.int8.onnx"), b"model").unwrap();
        symlink(&target, &default).unwrap();
        assert_eq!(reclaim_legacy_models(&default, false), 0);
        fs::remove_file(&default).unwrap();
        fs::create_dir(&default).unwrap();
        symlink(target.join("sensevoice-small"), default.join("sensevoice-small")).unwrap();
        symlink(target.join("silero_vad.onnx"), default.join("silero_vad.onnx")).unwrap();
        assert_eq!(reclaim_legacy_models(&default, false), 0);
        assert_eq!(fs::read(default.join("silero_vad.onnx")).unwrap(), b"vad");
        assert_eq!(fs::read(default.join("sensevoice-small/model.int8.onnx")).unwrap(), b"model");
        fs::remove_dir_all(base).unwrap();
    }

    fn run_local_transcribe(audio_b64: String, model_id: &str) -> Result<LocalAsrResult, String> {
        // 與 main.rs 保持同一啟動順序；dynamic-backends 模式必須先註冊 GGML 後端。
        super::super::gguf_asr::init_backends();
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("建立測試 runtime 失敗")
            .block_on(local_transcribe(
                audio_b64,
                model_id.to_string(),
                Some("auto".to_string()),
                Some("auto".to_string()),
                None,
            ))
    }

    /// [自訂] SC-57：WAV 只接受 16 kHz、單聲道、16 位元 PCM；逐一讀 chunk 找 `data`，不假設 44 位元組標頭。
    fn wav_pcm16_mono_16k(bytes: &[u8]) -> Result<&[u8], String> {
        if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
            return Err("not a RIFF/WAVE file".into());
        }
        let mut offset = 12;
        let mut format_ok = false;
        while offset + 8 <= bytes.len() {
            let id = &bytes[offset..offset + 4];
            let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
            let body = offset + 8;
            let end = body.checked_add(size).filter(|end| *end <= bytes.len())
                .ok_or("truncated WAV chunk")?;
            match id {
                b"fmt " if size >= 16 => {
                    let field = |at: usize, len: usize| bytes[body + at..body + at + len]
                        .iter().rev().fold(0u32, |value, byte| value << 8 | u32::from(*byte));
                    format_ok = field(0, 2) == 1 && field(2, 2) == 1 && field(4, 4) == SR as u32 && field(14, 2) == 16;
                }
                b"data" => {
                    return if format_ok { Ok(&bytes[body..end]) } else { Err("expected 16 kHz mono 16-bit PCM before data".into()) };
                }
                _ => {}
            }
            offset = end + (size & 1);
        }
        Err("no data chunk".into())
    }

    /// [自訂] SC-57：雲端煙霧測試。只在設定 SAYIT_SMOKE_MODELS_DIR（已核對的模型目錄）時執行。
    #[test]
    #[ignore]
    fn cloud_smoke_sensevoice() {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                super::super::gguf_asr::unload();
                downloader::set_custom_models_dir(None);
            }
        }
        let Some(dir) = std::env::var_os("SAYIT_SMOKE_MODELS_DIR") else {
            eprintln!("skip: SAYIT_SMOKE_MODELS_DIR is not set");
            return;
        };
        let model_id = "sensevoice-small-gguf";
        let _guard = downloader::TEST_MODEL_LEASES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _reset = Reset;
        downloader::set_custom_models_dir(Some(std::path::PathBuf::from(dir)));
        assert!(super::super::gguf_asr::model_is_downloaded(model_id), "verified {model_id} weights are required");
        for name in ["test_zh.wav", "test_en.wav"] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources").join(name);
            let bytes = std::fs::read(&path).unwrap();
            let pcm = wav_pcm16_mono_16k(&bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
            let seconds = pcm.len() as f64 / (SR * 2) as f64;
            let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, pcm);
            let started = Instant::now();
            let result = run_local_transcribe(b64, model_id).unwrap();
            let elapsed = started.elapsed().as_secs_f64();
            eprintln!(
                "[cloud-smoke] file={name} audio_sec={seconds:.2} elapsed_sec={elapsed:.2} rtf={:.3} backend={:?} text={:?}",
                elapsed / seconds,
                super::super::gguf_asr::current_backend(),
                result.text,
            );
            assert!(!result.text.trim().is_empty(), "{name}: empty transcript");
            // [自訂] SC-67：中文輸出一律是台灣正體，且不得殘留簡體專用字。
            assert_eq!(crate::zh_output::count_simplified_only(&result.text), 0, "{name}: simplified characters remain");
            if name == "test_zh.wav" {
                assert_eq!(result.text.trim(), "語音輸入法測試成功。", "{name}");
            }
        }
    }

    /// [自訂] SC-68：雲端驗證 Breeze ASR 25 能被 transcribe-cpp 載入並辨識內建測試語音。
    /// 只在 cloud-smoke 勾選 breeze 時執行（權重約 1.6 GB）。
    #[test]
    #[ignore]
    fn cloud_smoke_breeze() {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                super::super::gguf_asr::unload();
                downloader::set_custom_models_dir(None);
            }
        }
        let Some(dir) = std::env::var_os("SAYIT_SMOKE_MODELS_DIR") else {
            eprintln!("skip: SAYIT_SMOKE_MODELS_DIR is not set");
            return;
        };
        let model_id = "breeze-asr-25-gguf";
        let _guard = downloader::TEST_MODEL_LEASES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _reset = Reset;
        downloader::set_custom_models_dir(Some(std::path::PathBuf::from(dir)));
        assert!(super::super::gguf_asr::model_is_downloaded(model_id), "verified {model_id} weights are required");
        for name in ["test_zh.wav", "test_en.wav"] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources").join(name);
            let bytes = std::fs::read(&path).unwrap();
            let pcm = wav_pcm16_mono_16k(&bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
            let seconds = pcm.len() as f64 / (SR * 2) as f64;
            let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, pcm);
            let started = Instant::now();
            let result = run_local_transcribe(b64, model_id).unwrap();
            let elapsed = started.elapsed().as_secs_f64();
            eprintln!(
                "[cloud-smoke] model={model_id} file={name} audio_sec={seconds:.2} elapsed_sec={elapsed:.2} rtf={:.3} backend={:?} memory_mb={} text={:?}",
                elapsed / seconds,
                super::super::gguf_asr::current_backend(),
                super::super::gguf_asr::process_memory_mb(),
                result.text,
            );
            assert!(!result.text.trim().is_empty(), "{name}: empty transcript");
            // [自訂] SC-67／SC-68：輸出經轉換後不得殘留簡體專用字。
            assert_eq!(crate::zh_output::count_simplified_only(&result.text), 0, "{name}: simplified characters remain");
        }
    }

    /// [自訂] SC-67：交叉測試第一段（只在雲端跑）。對 `SAYIT_SMOKE_MODELS_DIR` 裡已下載的每個模型，
    /// 把內建測試語音的**原始**辨識文字（未轉換）寫成 TSV，交給 `tools/zh-crosstest` 依各後端轉換比較。
    /// 欄位：model、file、elapsed_ms、raw_text（換行與 tab 以空白取代）。
    #[test]
    #[ignore]
    fn cloud_smoke_zh_matrix_dump() {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                super::super::gguf_asr::unload();
                downloader::set_custom_models_dir(None);
            }
        }
        let (Some(dir), Some(out)) = (std::env::var_os("SAYIT_SMOKE_MODELS_DIR"), std::env::var_os("SAYIT_ZH_MATRIX_OUT")) else {
            eprintln!("skip: SAYIT_SMOKE_MODELS_DIR or SAYIT_ZH_MATRIX_OUT is not set");
            return;
        };
        let _guard = downloader::TEST_MODEL_LEASES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _reset = Reset;
        downloader::set_custom_models_dir(Some(std::path::PathBuf::from(dir)));
        super::super::gguf_asr::init_backends();
        let mut rows = String::from("model\tfile\telapsed_ms\traw_text\n");
        let mut models = 0;
        for model in super::super::catalog::available_models() {
            if !super::super::gguf_asr::model_is_downloaded(&model.id) {
                continue;
            }
            models += 1;
            for name in ["test_zh.wav", "test_en.wav"] {
                let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources").join(name);
                let bytes = std::fs::read(&path).unwrap();
                let pcm = wav_pcm16_mono_16k(&bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
                let samples: Vec<f32> = pcm
                    .chunks_exact(2)
                    .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
                    .collect();
                let started = Instant::now();
                let raw = super::super::gguf_asr::transcribe(&model.id, "auto", "auto", "", &samples, SR)
                    .unwrap_or_else(|error| format!("ERROR: {error}"));
                let elapsed = started.elapsed().as_millis();
                let clean = raw.replace(['\t', '\n', '\r'], " ");
                eprintln!("[zh-matrix] model={} file={name} elapsed_ms={elapsed} raw={clean:?}", model.id);
                rows.push_str(&format!("{}\t{name}\t{elapsed}\t{clean}\n", model.id));
            }
            super::super::gguf_asr::unload();
        }
        std::fs::write(&out, rows).unwrap();
        assert!(models > 0, "no downloaded model found");
    }

    /// 走真實生產路徑：base64 → i16→f32 → Silero VAD → GGUF 引擎。
    #[test]
    fn end_to_end_base64_to_text() {
        let model_id = "sensevoice-small-gguf";
        if !super::super::gguf_asr::model_is_downloaded(model_id) {
            eprintln!("skip: {model_id} 未下載");
            return;
        }

        // 前後各加 2s 靜音，驗證 VAD 裁剪後仍能識別中間的真實語音。
        let wav = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/test_zh.wav");
        let bytes = std::fs::read(&wav).unwrap();
        let speech = &bytes[44..];
        let silence = vec![0u8; SR * 2 * 2]; // 2s、16-bit PCM
        let mut pcm_bytes = Vec::new();
        pcm_bytes.extend_from_slice(&silence);
        pcm_bytes.extend_from_slice(speech);
        pcm_bytes.extend_from_slice(&silence);

        let b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &pcm_bytes,
        );
        let result = run_local_transcribe(b64, model_id).unwrap();
        assert!(
            !result.text.trim().is_empty(),
            "VAD 後仍應識別出內容，got {:?}",
            result.text
        );
        super::super::gguf_asr::unload();
    }

    /// 220 秒的長錄音走真實路徑：按停頓切塊、逐塊上顯示卡，而不是一次喂整段。
    ///
    /// 2026-10-19 一段 220 秒的錄音一次喂進 Qwen3-ASR 1.7B，把整合顯示卡跑到被 Windows
    /// 看門狗重置（LiveKernelEvent 141）。這條要幾十秒、會佔滿顯示卡，平時不跑：
    /// `cargo test --bins long_audio_is_chunked -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn long_audio_is_chunked_and_transcribed_on_gpu() {
        let model_id = "qwen3-asr-1.7b-gguf";
        if !super::super::gguf_asr::model_is_downloaded(model_id) {
            eprintln!("skip: {model_id} 未下載");
            return;
        }
        let wav = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/test_zh.wav");
        let bytes = std::fs::read(&wav).unwrap();
        let speech = &bytes[44..];
        let pause = vec![0u8; SR * 2]; // 1 s 靜音，夠 VAD 認作停頓
        let mut pcm_bytes = Vec::new();
        while pcm_bytes.len() < 220 * SR * 2 {
            pcm_bytes.extend_from_slice(speech);
            pcm_bytes.extend_from_slice(&pause);
        }

        // 判據先釘「切塊真的發生了」，再看結果：只斷言有文字的話，一次喂整段僥倖沒出事也會綠
        let samples: Vec<f32> = pcm_bytes
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
            .collect();
        let segments = super::super::local_vad::detect_speech_segments(&samples, SR).unwrap();
        let chunks = super::super::local_vad::plan_chunks(&samples, &segments, MAX_CHUNK_SEC * SR);
        assert!(chunks.len() >= 7, "220 s 應切成至少 7 塊，實際 {}", chunks.len());
        assert!(chunks.iter().all(|c| c.end - c.start <= MAX_CHUNK_SEC * SR));

        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &pcm_bytes);
        let t0 = std::time::Instant::now();
        let result = run_local_transcribe(b64, model_id).expect("長錄音識別失敗");
        let backend = super::super::gguf_asr::current_backend().unwrap_or_default();
        eprintln!(
            "[long-audio] audio={:.1}s chunks={} elapsed={:.1}s chars={} backend={}",
            samples.len() as f64 / SR as f64,
            chunks.len(),
            t0.elapsed().as_secs_f64(),
            result.text.chars().count(),
            backend,
        );
        assert!(result.text.chars().count() > 200, "長錄音識別結果太短：{:?}", result.text);
        // 機器上有顯示卡時，必須是在顯示卡上跑完的：落到 CPU 說明中途出了後端錯誤
        let has_gpu = transcribe_cpp::devices().iter().any(|d| d.kind != "cpu");
        if has_gpu {
            assert!(!backend.to_lowercase().contains("cpu"), "顯示卡中途失敗、回落到了 CPU：{backend}");
        }
        super::super::gguf_asr::unload();
    }

    #[test]
    fn silence_short_circuits_before_loading_model() {
        let pcm_bytes = vec![0u8; SR * 2 * 3]; // 3s、16-bit PCM 純靜音
        let b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &pcm_bytes,
        );

        // 故意傳不存在的模型：若 VAD 沒有先短路，這裡會因模型不存在而報錯。
        let result = run_local_transcribe(b64, "model-that-must-not-be-loaded").unwrap();
        assert!(result.text.is_empty());
    }

    #[test]
    fn decode_pcm_maps_i16_to_unit_range() {
        // i16 -32768 / 0 / 32767 → -1.0 / 0.0 / ~1.0
        let bytes: Vec<u8> = [i16::MIN, 0i16, i16::MAX]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
        let pcm = decode_pcm(&b64).unwrap();
        assert_eq!(pcm.len(), 3);
        assert!((pcm[0] + 1.0).abs() < 1e-6);
        assert_eq!(pcm[1], 0.0);
        assert!((pcm[2] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn decode_pcm_rejects_incomplete_i16_sample() {
        let b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            [0u8, 1, 2],
        );
        let err = decode_pcm(&b64).unwrap_err();
        assert!(err.contains("16-bit"), "unexpected error: {err}");
    }
}
