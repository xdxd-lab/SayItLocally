//! 本地 ASR 前置語音檢測。
//!
//! Qwen3-ASR 這類生成式模型遇到純靜音或環境底噪時，可能生成並不存在的文字。
//! 這裡參考 Handy 的做法，用獨立的 Silero VAD 在推理前確認音訊裡確實有人聲。
//! 與 Handy 不同的是，我們只用 VAD 確定首尾邊界，保留句子中間的停頓；歷史記錄
//! 儲存的也仍然是未經裁剪的原始錄音。

use std::ops::Range;

use silero_vad_crs::{
    get_timestamps_from_probs_with_config, SileroVad, TimestampConfig,
};

/// 與 Handy 一致使用較寬鬆的 0.3，優先避免漏掉輕聲說話。
const SPEECH_THRESHOLD: f32 = 0.3;
/// 過濾總時長不足約 60ms 的候選語音段，抑制短促點選聲和提示音。
/// 這與 Handy 的“連續兩幀起聲”目標相同，但 Silero 時間戳演算法帶滯回，行為並非逐幀等價。
const MIN_SPEECH_MS: usize = 60;
/// 至少持續 450ms 的非語音才切分語音段，減少短暫停頓導致的誤切分。
const MIN_SILENCE_MS: usize = 450;
/// 起止各留約 450ms，避免切掉子音、輕聲開頭和句尾。
const SPEECH_PAD_MS: usize = 450;

/// 返回每一段人聲（已含首尾留白），按時間排序、互不重疊。
///
/// 空列表明確表示沒有檢測到人聲，呼叫方應跳過 ASR；`Err` 表示無法可靠判斷是否
/// 有人聲，為避免生成式模型對靜音產生幻覺，呼叫方也必須停止本次 ASR。
/// 交給模型前再經 `plan_chunks` 拼塊：短錄音就是首段開頭到末段結尾一整塊。
pub fn detect_speech_segments(
    samples: &[f32],
    sample_rate: usize,
) -> Result<Vec<Range<usize>>, String> {
    if samples.is_empty() {
        return Ok(Vec::new());
    }

    let mut vad = SileroVad::with_sample_rate(sample_rate)
        .map_err(|e| format!("Failed to create Silero VAD: {e}"))?;
    let window_size_samples = vad.source_window_samples();
    let probabilities = vad
        .forward_audio(samples)
        .map_err(|e| format!("Silero VAD inference failed: {e}"))?;

    let segments = get_timestamps_from_probs_with_config(
        &probabilities,
        samples.len(),
        TimestampConfig {
            sampling_rate: sample_rate,
            threshold: SPEECH_THRESHOLD,
            min_speech_duration_ms: MIN_SPEECH_MS,
            min_silence_duration_ms: MIN_SILENCE_MS,
            speech_pad_ms: SPEECH_PAD_MS,
            window_size_samples,
            ..Default::default()
        },
    );

    let ranges = normalize_segments(
        segments.iter().map(|s| s.start..s.end),
        samples.len(),
    );

    if let (Some(first), Some(last)) = (ranges.first(), ranges.last()) {
        log::debug!(
            "Silero VAD: {:.2}s -> {:.2}s, speech_segments={}",
            samples.len() as f64 / sample_rate as f64,
            (last.end - first.start) as f64 / sample_rate as f64,
            ranges.len(),
        );
    }
    Ok(ranges)
}

/// 截到音訊長度以內、丟掉空段、按起點排序，並把重疊或首尾相接的段合併。
/// 加了留白之后相邻两段可能互相覆盖，后面切块要求段与段之间不重叠。
fn normalize_segments(
    segments: impl IntoIterator<Item = Range<usize>>,
    len: usize,
) -> Vec<Range<usize>> {
    let mut ranges: Vec<Range<usize>> = segments
        .into_iter()
        .map(|r| r.start.min(len)..r.end.min(len))
        .filter(|r| r.start < r.end)
        .collect();
    ranges.sort_by_key(|r| r.start);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for r in ranges {
        match merged.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    merged
}

/// 把人聲段拼成一塊塊不超過 `max_len` 個取樣的識別塊，切口落在停頓上。
///
/// 為什麼要切：模型一次吃整段長錄音，交給顯示卡的單個任務會隨長度變長（Qwen3-ASR
/// 的注意力計算隨長度平方增長，實測 1.7B 在整合顯示卡上 30 s 的錄音 0.18 倍時長，
/// 102 s 就到了 0.29 倍）。一個任務超過 Windows 顯示卡看門狗的 2 秒，顯示卡就被重置
/// （LiveKernelEvent 141，Vulkan 報 DeviceLost），本次和之後的本地識別全部失敗。
/// 2026-10-19 一條 220 秒的錄音就是這樣把顯示卡搞掛的。
///
/// 規則：
/// - 相鄰人聲段貪心地並進同一塊，塊內的短停頓原樣保留（短錄音結果與切塊之前
///   只裁首尾時一致：總長不超過上限時只有一塊，就是首段開頭到末段結尾）；
/// - 兩塊之間的長停頓直接丟掉，那裡本來就是靜音；
/// - 單獨一段話就超過上限（一口氣說了很久、中間沒有 0.45 s 以上的停頓）時，
///   在這一塊的後三分之一里找最安靜的 100 ms 切開，找不到更好的位置才硬切。
pub fn plan_chunks(samples: &[f32], segments: &[Range<usize>], max_len: usize) -> Vec<Range<usize>> {
    let segments = normalize_segments(segments.iter().cloned(), samples.len());
    if max_len == 0 {
        return segments;
    }
    let mut chunks: Vec<Range<usize>> = Vec::new();
    let mut current: Option<Range<usize>> = None;
    for seg in segments {
        current = match current {
            Some(cur) if seg.end - cur.start <= max_len => Some(cur.start..seg.end),
            Some(cur) => {
                chunks.push(cur);
                Some(seg)
            }
            None => Some(seg),
        };
    }
    if let Some(cur) = current {
        chunks.push(cur);
    }

    let mut out = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        split_overlong(samples, chunk, max_len, &mut out);
    }
    out
}

/// 一段連續人聲超過上限時，在 [上限的 2/3, 上限] 裡找最安靜的 100 ms 切開。
fn split_overlong(samples: &[f32], chunk: Range<usize>, max_len: usize, out: &mut Vec<Range<usize>>) {
    const FRAME: usize = 1600; // 16 kHz 下 100 ms
    let mut start = chunk.start;
    while chunk.end - start > max_len {
        let search_from = start + max_len * 2 / 3;
        let search_to = start + max_len;
        let mut best_cut = search_to;
        let mut best_level = f32::MAX;
        let mut frame_start = search_from;
        while frame_start + FRAME <= search_to {
            let frame = &samples[frame_start..frame_start + FRAME];
            let level = frame.iter().map(|s| s.abs()).sum::<f32>() / FRAME as f32;
            if level < best_level {
                best_level = level;
                best_cut = frame_start + FRAME / 2;
            }
            frame_start += FRAME;
        }
        out.push(start..best_cut);
        start = best_cut;
    }
    out.push(start..chunk.end);
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: usize = 16_000;

    fn read_test_speech() -> Vec<f32> {
        let bytes = include_bytes!("../../resources/test_zh.wav");
        bytes[44..]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
            .collect()
    }

    #[test]
    fn pure_silence_is_rejected() {
        let audio = vec![0.0; SR * 3];
        assert!(detect_speech_segments(&audio, SR).unwrap().is_empty());
    }

    #[test]
    fn steady_background_noise_is_rejected() {
        let mut state = 0x1234_5678u32;
        let audio: Vec<f32> = (0..SR * 3)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((state >> 8) as f32 / 16_777_215.0 - 0.5) * 0.02
            })
            .collect();
        assert!(detect_speech_segments(&audio, SR).unwrap().is_empty());
    }

    #[test]
    fn short_transient_noise_is_rejected() {
        let mut audio = vec![0.0; SR];
        // 模擬一次約 32ms 的敲擊/提示音；不應滿足持續人聲條件。
        for (i, sample) in audio[SR / 2..SR / 2 + 512].iter_mut().enumerate() {
            let t = i as f32 / SR as f32;
            *sample = 0.4 * (2.0 * std::f32::consts::PI * 1_500.0 * t).sin();
        }
        assert!(detect_speech_segments(&audio, SR).unwrap().is_empty());
    }

    #[test]
    fn bundled_speech_is_detected() {
        let speech = read_test_speech();
        assert!(
            !detect_speech_segments(&speech, SR).unwrap().is_empty(),
            "真实中文语音不应被 VAD 拒绝",
        );
    }

    #[test]
    fn quiet_speech_is_detected() {
        let speech: Vec<f32> = read_test_speech().into_iter().map(|s| s * 0.2).collect();
        assert!(
            !detect_speech_segments(&speech, SR).unwrap().is_empty(),
            "降低约 14dB 的轻声语音不应被 VAD 拒绝",
        );
    }

    const S: usize = SR; // 一秒的取樣數，讀起來更直觀

    #[test]
    fn short_audio_stays_one_chunk_with_its_pauses() {
        // 總跨度 20 s < 30 s：一塊，從首段開頭到末段結尾，中間的停頓保留
        let audio = vec![0.1; 25 * S];
        let segments = vec![S..5 * S, 8 * S..12 * S, 15 * S..21 * S];
        assert_eq!(plan_chunks(&audio, &segments, 30 * S), vec![S..21 * S]);
    }

    #[test]
    fn long_audio_is_cut_at_pauses_and_every_chunk_fits() {
        // 220 s：每 10 s 一段話、段間 2 s 停頓
        let audio = vec![0.1; 220 * S];
        let segments: Vec<_> = (0..18).map(|i| (i * 12) * S..(i * 12 + 10) * S).collect();
        let chunks = plan_chunks(&audio, &segments, 30 * S);
        assert!(chunks.len() >= 8, "220 s 应该切成多块，实际 {}", chunks.len());
        for c in &chunks {
            assert!(c.end - c.start <= 30 * S, "块超长：{:?}", c);
            // 切口都落在某段話的邊界上，不會把話切成兩半
            assert!(segments.iter().any(|s| s.start == c.start));
            assert!(segments.iter().any(|s| s.end == c.end));
        }
        // 所有人声都被覆盖，一段不漏
        for s in &segments {
            assert!(chunks.iter().any(|c| c.start <= s.start && s.end <= c.end));
        }
    }

    #[test]
    fn one_long_breathless_segment_is_split_at_its_quietest_point() {
        // 一口氣說 70 s，中間 45~45.1 s 有一處很輕的地方，沒到 VAD 認定的停頓
        let mut audio = vec![0.3; 70 * S];
        for s in &mut audio[45 * S..45 * S + 1600] {
            *s = 0.01;
        }
        let chunks = plan_chunks(&audio, &[0..70 * S], 30 * S);
        assert!(chunks.iter().all(|c| c.end - c.start <= 30 * S), "{chunks:?}");
        assert_eq!(chunks.first().unwrap().start, 0);
        assert_eq!(chunks.last().unwrap().end, 70 * S);
        // 首尾相接，沒有丟音訊
        for pair in chunks.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
        }
        // 搜尋窗是每塊的 [20 s, 30 s]。安靜點落在搜尋窗裡時，必須正好切在那 100 ms 的中間
        let mut audio2 = vec![0.3; 50 * S];
        for s in &mut audio2[25 * S..25 * S + 1600] {
            *s = 0.01;
        }
        let chunks2 = plan_chunks(&audio2, &[0..50 * S], 30 * S);
        assert_eq!(chunks2[0].end, 25 * S + 800, "{chunks2:?}");
    }

    #[test]
    fn overlapping_padded_segments_are_merged() {
        let audio = vec![0.1; 10 * S];
        let chunks = plan_chunks(&audio, &[3 * S..6 * S, S..4 * S, 9 * S..12 * S], 30 * S);
        // 截到音訊長度、合併重疊，再併成一塊
        assert_eq!(chunks, vec![S..10 * S]);
    }

    #[test]
    fn internal_pause_is_preserved() {
        let speech = read_test_speech();
        let half = speech.len() / 2;
        let mut audio = vec![0.0; SR];
        audio.extend_from_slice(&speech[..half]);
        let pause_start = audio.len();
        audio.extend(std::iter::repeat(0.0).take(SR * 2));
        let pause_end = audio.len();
        audio.extend_from_slice(&speech[half..]);
        audio.extend(std::iter::repeat(0.0).take(SR));

        // 走生產路徑：分段 → 拼塊。幾秒的錄音只有一塊，句中 2 s 的停頓留在塊裡
        let segments = detect_speech_segments(&audio, SR).unwrap();
        assert!(!segments.is_empty(), "前后两段真实语音应被检测到");
        let chunks = plan_chunks(&audio, &segments, 30 * SR);
        assert_eq!(chunks.len(), 1, "短录音应只有一块：{chunks:?}");
        let span = chunks[0].clone();
        assert!(span.start < pause_start);
        assert!(span.end > pause_end);
        assert!(audio[span.clone()][pause_start - span.start..pause_end - span.start]
            .iter()
            .all(|sample| *sample == 0.0));
    }
}
