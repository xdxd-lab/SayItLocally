// 本地 ASR 的命令层 —— 推理本身在 gguf_asr.rs（transcribe.cpp / ggml / GGUF）。
//
// 这里负责：解 base64 → i16 转 f32 → Silero VAD 确认存在人声并裁剪首尾
// → 交给引擎；以及回收上一代 ONNX 模型。原始录音不会被修改。
//
// 历史：0.1.3 之前本地识别走 sherpa-onnx（ONNX Runtime，纯 CPU、INT8 动态量化）。
// Qwen3-ASR 这类自回归模型在那条路上慢一个量级，已整体换成 GGUF/ggml，
// sherpa-onnx 依赖连同 Whisper/Paraformer/FireRed/FunASR 的加载器一并删除。
// 原因与实测数字见 dev-docs/local-asr-ggml-migration.md。

use serde::Serialize;
use std::path::Path;
use std::time::Instant;

use super::downloader;
use super::gguf_asr;

/// 采样率固定 16 kHz —— 前端采集就重采样到这个值，引擎也只接受这个值。
const SR: usize = 16000;

/// 一次交给模型的最长音频。超过就在停顿处切块、逐块识别再拼起来。
///
/// 原来是「模型吃得下多长就一次喂多长」（Qwen3-ASR 上限约 87 分钟），结果 220 秒的
/// 录音在集成显卡上一次跑太久，触发 Windows 显卡看门狗、显卡被重置，之后本地识别
/// 全部失败（见 local_vad::plan_chunks 的说明）。30 秒：日常口述基本不切、结果与原来
/// 一致；实测 1.7B 在 Arc 140T 上 30 s 一块约 5 s，单个任务离 2 秒上限很远；
/// 也正好是 SenseVoice 的上限，那边不再走按固定长度硬切的旧分段。
/// 代价：块与块之间互相看不到上下文，切口两边的标点、专有名词偶尔会受影响。
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
    // gpu_device：用户指定的显卡（`GgufDevice::id`）。缺省 / 空 = 交给库自动挑。
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

/// 同 local_transcribe，但 PCM 以原始字节发过来（见 ipc_body），省掉网页里的 base64 转码。
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
        // 先砍掉首尾静音：解码耗时基本与音频长度成正比，两头的空白是纯浪费。
        // 长录音再按停顿切块（MAX_CHUNK_SEC），短录音只有一块，等同于只裁首尾。
        let segments = match super::local_vad::detect_speech_segments(&samples, SR) {
            Ok(segments) if !segments.is_empty() => segments,
            Ok(_) => {
                // 这是「真的没人说话」，与识别失败要分得清 —— 前端两种都显示
                // 「未检测到有效声音」，只有日志能区分
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
                // 防幻觉是硬约束：无法可靠判断是否有人声时，绝不能回退到会把静音
                // 送进生成式模型的旧路径。保留错误便于诊断 VAD/本机环境问题。
                return Err(diag::fail(
                    "local/asr",
                    "vad",
                    format!("Voice activity detection failed; recognition was canceled: {e}"),
                ));
            }
        };
        let chunks = super::local_vad::plan_chunks(&samples, &segments, MAX_CHUNK_SEC * SR);
        if chunks.len() > 1 {
            // 排查「长录音为什么慢 / 切口处丢字」时先看这一行：有它才说明走了切块
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
            // VAD 认定有人声、模型却什么都没输出 —— 模型/量化档/加速器组合有问题的信号
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

/// 逐块识别再按顺序拼接。任何一块失败整次算失败：拼出半段文字插进去，
/// 比明确报错更难让用户发现少了内容。
fn transcribe_chunks(
    model_id: &str,
    lang: &str,
    accel: &str,
    gpu: &str,
    samples: &[f32],
    chunks: &[std::ops::Range<usize>],
) -> Result<String, String> {
    let mut text = String::new();
    for chunk in chunks {
        let part = gguf_asr::transcribe(model_id, lang, accel, gpu, &samples[chunk.clone()], SR)?;
        gguf_asr::append_segment_text(&mut text, &part);
    }
    Ok(text)
}

#[tauri::command]
pub async fn preload_local_model(
    model_id: String,
    accelerator: Option<String>,
    // gpu_device：用户指定的显卡（`GgufDevice::id`）。缺省 / 空 = 交给库自动挑。
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

/// 释放本地 ASR 模型占用的内存。切换到云 API / 服务器模式时调用，
/// 否则常驻的权重（几百 MB ~ 数 GB）会一直占用到应用退出。
#[tauri::command]
pub async fn unload_local_model() -> Result<(), String> {
    tokio::task::spawn_blocking(gguf_asr::unload)
        .await
        .map_err(|e| format!("Release task failed: {}", e))
}

/// 动态调整本地模型的空闲卸载时间。0 = 常驻；其余值由设置页限制为
/// 10 / 30 / 60 分钟。守护线程只启动一次，这里仅更新它读取的原子配置。
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

/// 供前端查询本次启动回收了多少空间（可用于提示"已释放 XXX MB"）。
#[tauri::command]
pub fn legacy_models_reclaimed_bytes() -> u64 {
    RECLAIMED.load(std::sync::atomic::Ordering::Relaxed)
}

static RECLAIMED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 启动时在后台回收旧模型（几百 MB 的删除别卡启动路径）。
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
        // 与 main.rs 保持同一启动顺序；dynamic-backends 模式必须先注册 GGML 后端。
        super::super::gguf_asr::init_backends();
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("创建测试 runtime 失败")
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
        }
    }

    /// 走真实生产路径：base64 → i16→f32 → Silero VAD → GGUF 引擎。
    #[test]
    fn end_to_end_base64_to_text() {
        let model_id = "sensevoice-small-gguf";
        if !super::super::gguf_asr::model_is_downloaded(model_id) {
            eprintln!("skip: {model_id} 未下载");
            return;
        }

        // 前后各加 2s 静音，验证 VAD 裁剪后仍能识别中间的真实语音。
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
            "VAD 后仍应识别出内容，got {:?}",
            result.text
        );
        super::super::gguf_asr::unload();
    }

    /// 220 秒的长录音走真实路径：按停顿切块、逐块上显卡，而不是一次喂整段。
    ///
    /// 2026-10-19 一段 220 秒的录音一次喂进 Qwen3-ASR 1.7B，把集成显卡跑到被 Windows
    /// 看门狗重置（LiveKernelEvent 141）。这条要几十秒、会占满显卡，平时不跑：
    /// `cargo test --bins long_audio_is_chunked -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn long_audio_is_chunked_and_transcribed_on_gpu() {
        let model_id = "qwen3-asr-1.7b-gguf";
        if !super::super::gguf_asr::model_is_downloaded(model_id) {
            eprintln!("skip: {model_id} 未下载");
            return;
        }
        let wav = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/test_zh.wav");
        let bytes = std::fs::read(&wav).unwrap();
        let speech = &bytes[44..];
        let pause = vec![0u8; SR * 2]; // 1 s 静音，够 VAD 认作停顿
        let mut pcm_bytes = Vec::new();
        while pcm_bytes.len() < 220 * SR * 2 {
            pcm_bytes.extend_from_slice(speech);
            pcm_bytes.extend_from_slice(&pause);
        }

        // 判据先钉「切块真的发生了」，再看结果：只断言有文字的话，一次喂整段侥幸没出事也会绿
        let samples: Vec<f32> = pcm_bytes
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
            .collect();
        let segments = super::super::local_vad::detect_speech_segments(&samples, SR).unwrap();
        let chunks = super::super::local_vad::plan_chunks(&samples, &segments, MAX_CHUNK_SEC * SR);
        assert!(chunks.len() >= 7, "220 s 应切成至少 7 块，实际 {}", chunks.len());
        assert!(chunks.iter().all(|c| c.end - c.start <= MAX_CHUNK_SEC * SR));

        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &pcm_bytes);
        let t0 = std::time::Instant::now();
        let result = run_local_transcribe(b64, model_id).expect("长录音识别失败");
        let backend = super::super::gguf_asr::current_backend().unwrap_or_default();
        eprintln!(
            "[long-audio] audio={:.1}s chunks={} elapsed={:.1}s chars={} backend={}",
            samples.len() as f64 / SR as f64,
            chunks.len(),
            t0.elapsed().as_secs_f64(),
            result.text.chars().count(),
            backend,
        );
        assert!(result.text.chars().count() > 200, "长录音识别结果太短：{:?}", result.text);
        // 机器上有显卡时，必须是在显卡上跑完的：落到 CPU 说明中途出了后端错误
        let has_gpu = transcribe_cpp::devices().iter().any(|d| d.kind != "cpu");
        if has_gpu {
            assert!(!backend.to_lowercase().contains("cpu"), "显卡中途失败、回落到了 CPU：{backend}");
        }
        super::super::gguf_asr::unload();
    }

    #[test]
    fn silence_short_circuits_before_loading_model() {
        let pcm_bytes = vec![0u8; SR * 2 * 3]; // 3s、16-bit PCM 纯静音
        let b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &pcm_bytes,
        );

        // 故意传不存在的模型：若 VAD 没有先短路，这里会因模型不存在而报错。
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
