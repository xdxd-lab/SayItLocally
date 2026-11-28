/**
 * 緩衝式 Provider 基類
 * CloudAPIProvider 和 LocalProvider 共享的 PCM 緩衝、合併、生命週期邏輯。
 * 子類只需實現 onConnect() 和 processAudio()。
 */

import { addRuntimeEvent } from '../debugLog'
import type {
  TranscriptionProvider,
  TranscriptionCallbacks,
  StartOptions,
  StopOptions,
  WorkMode,
} from './types'

export abstract class BufferedProvider implements TranscriptionProvider {
  abstract readonly mode: WorkMode

  protected callbacks: TranscriptionCallbacks = {}
  protected pcmBuffers: ArrayBuffer[] = []
  protected sessionActive = false
  protected startOpts: StartOptions | undefined
  protected ready = false
  protected activeRunId = 0

  /**
   * 子類可覆蓋，用於連線時的額外初始化（如預載入模型）。
   *
   * **返回 false 表示"連上了但用不了"**，此時 isReady() 會是 false，
   * RecorderOrchestrator 那道前置攔截就會擋住錄音並給出提示。
   * 返回 void / true 視為就緒（CloudAPIProvider 等不關心的子類無需改動）。
   */
  protected async onConnect(_callbacks: TranscriptionCallbacks): Promise<void | boolean> { }

  async connect(callbacks: TranscriptionCallbacks): Promise<void> {
    this.callbacks = callbacks
    this.ready = true
    callbacks.onStateChange?.('connected')
    // 以前這裡只是 await，子類無從否決 —— 於是本地模式把模型全刪了 isReady() 仍為 true，
    // 按快捷鍵照樣開錄、錄完才在識別階段報錯（而設定頁寫著"按下快捷鍵不會有反應"）。
    const usable = await this.onConnect(callbacks)
    if (usable === false) {
      this.ready = false
      addRuntimeEvent('warn', this.mode, 'Provider connected but is unavailable; marked not ready')
    }
  }

  start(opts: StartOptions): boolean {
    if (!this.ready) {
      addRuntimeEvent('error', this.mode, 'Start failed: provider is not ready')
      return false
    }
    this.pcmBuffers = []
    this.sessionActive = true
    this.startOpts = opts
    this.activeRunId = opts.runId
    return true
  }

  cancel(): void {
    this.activeRunId = 0
    this.sessionActive = false
    this.pcmBuffers = []
    this.startOpts = undefined
  }

  sendAudio(buffer: ArrayBuffer): void {
    if (!this.sessionActive) return
    this.pcmBuffers.push(buffer.slice(0))
  }

  stop(_opts?: StopOptions): boolean {
    if (!this.sessionActive) return false
    const runId = this.activeRunId
    this.sessionActive = false
    void this.runProcessAudio(runId)
    return true
  }

  disconnect(): void {
    this.cancel()
    this.ready = false
    this.callbacks.onStateChange?.('disconnected')
  }

  isReady(): boolean {
    return this.ready
  }

  // ─── 子類實現 ───

  /**
   * 處理合併後的音訊資料。
   * @param pcm 16 kHz Int16 LE 的原始 PCM 位元組（直接走二進位制 IPC，不轉 base64）
   * @param durationSec 音訊時長（秒）
   */
  protected abstract processAudio(pcm: Uint8Array, durationSec: number, runId: number): Promise<void>

  protected isRunCurrent(runId: number): boolean {
    return runId !== 0 && this.activeRunId === runId
  }

  // ─── 內部邏輯 ───

  private async runProcessAudio(runId: number): Promise<void> {
    try {
      if (!this.isRunCurrent(runId)) return
      const totalBytes = this.pcmBuffers.reduce((sum, buf) => sum + buf.byteLength, 0)
      if (totalBytes === 0) {
        if (this.isRunCurrent(runId)) {
          this.callbacks.onDone?.()
          if (this.isRunCurrent(runId)) this.activeRunId = 0
        }
        return
      }

      const merged = new Uint8Array(totalBytes)
      let offset = 0
      for (const buf of this.pcmBuffers) {
        merged.set(new Uint8Array(buf), offset)
        offset += buf.byteLength
      }
      this.pcmBuffers = []

      const durationSec = (totalBytes / 2) / 16000
      if (durationSec < 0.3) {
      addRuntimeEvent('info', this.mode, 'Audio too short; skipped processing', { durationSec })
        if (this.isRunCurrent(runId)) {
          this.callbacks.onDone?.()
          if (this.isRunCurrent(runId)) this.activeRunId = 0
        }
        return
      }

      await this.processAudio(merged, durationSec, runId)
      if (this.isRunCurrent(runId)) this.activeRunId = 0
    } catch (err) {
      if (!this.isRunCurrent(runId)) return
      addRuntimeEvent('error', this.mode, 'Processing failed', { error: String(err) })
      this.callbacks.onError?.(String(err))
      this.callbacks.onDone?.()
      if (this.isRunCurrent(runId)) this.activeRunId = 0
    }
  }
}
