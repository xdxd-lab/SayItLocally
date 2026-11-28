/**
 * Renderer-side service for saving and loading audio files via IPC.
 */

import * as bridge from './bridge'

/**
 * Save PCM chunks (Int16 LE) as WAV file.
 * WAV header 編碼在 Rust 側完成，前端只需傳 PCM 原始資料。
 * Returns the saved file path.
 */
export async function saveRecordingAudio(
  id: string,
  chunks: ArrayBuffer[],
  sampleRate = 16000,
): Promise<string | null> {
  const totalLen = chunks.reduce((sum, c) => sum + c.byteLength, 0)
  if (totalLen <= 0) return null

  // 安全檢查：PCM 資料量是否合理（16kHz 16bit mono = 32000 bytes/sec）
  const expectedBytesPerSec = sampleRate * 2
  const durationSec = totalLen / expectedBytesPerSec
  if (durationSec > 600) {
    console.warn('[audio] saveRecordingAudio: unusually long audio', {
      totalLen,
      durationSec: durationSec.toFixed(1),
      sampleRate,
      chunkCount: chunks.length,
    })
  }

  // 合并 PCM chunks
  const merged = new Uint8Array(totalLen)
  let offset = 0
  for (const chunk of chunks) {
    merged.set(new Uint8Array(chunk), offset)
    offset += chunk.byteLength
  }

  // 原始位元組直接傳給 Rust（不再轉 base64，見 bridge.framedBody），Rust 側編碼 WAV header 並寫入檔案
  return bridge.savePcmAsWavRaw(id, merged, sampleRate)
}

/**
 * 讀錄音並生成 Blob URL 供 <audio> 播放。呼叫方用完要 URL.revokeObjectURL。
 *
 * 原來是 base64 data URL：五分鐘錄音是 13 MB 的字串，Rust 轉一遍、IPC 傳一遍、
 * 瀏覽器再解一遍。現在 Rust 直接回位元組。
 */
export async function loadAudioAsObjectUrl(filePath: string): Promise<string | null> {
  const bytes = await bridge.readAudioFileBytes(filePath)
  if (!bytes) return null
  return URL.createObjectURL(new Blob([bytes], { type: 'audio/wav' }))
}

/**
 * Delete an audio file.
 */
export async function deleteAudioFileFromRenderer(filePath: string): Promise<boolean> {
  return bridge.deleteAudioFile(filePath).then(() => true)
}
