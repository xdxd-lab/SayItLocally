// 本地模式 Provider
// 音訊在本地積攢，stop 時呼叫 Rust 側本地 ASR 推理
// ASR 完成後可選呼叫雲端 AI 校對

import { invoke } from '@tauri-apps/api/core'
import * as bridge from '../bridge'
import { getSetting } from '../store'
import { addRuntimeEvent } from '../debugLog'
import { BufferedProvider } from './BufferedProvider'
import { polishWithClientAi } from './clientAiPolish'
import { policyFromSnapshot, resolveAndLogAiOutcome, type AiOutcomeContext } from './aiPolicy'
import type { TranscriptionCallbacks } from './types'

export class LocalProvider extends BufferedProvider {
  readonly mode = 'local' as const

  /**
   * 本地模式的"就緒"= 選中的模型已完整下載到本地。
   *
   * 以前這裡無論如何都報 `asr: true`（連預載入失敗的 catch 裡也報），於是模型被刪光了
   * 按快捷鍵照樣開始錄音，錄完才在識別階段報錯 —— 而設定頁明明寫著"按下快捷鍵不會有
   * 反應"，說到沒做到。現在如實上報，未就緒由 RecorderOrchestrator 統一攔下並提示。
   *
   * 判斷口徑與左下角引擎指示（stores/modeStatus）保持一致：都看 list_downloaded_models
   * 裡該模型是否 complete，避免同屏兩處結論不同。
   */
  protected async onConnect(callbacks: TranscriptionCallbacks): Promise<boolean> {
    const modelId = await getSetting('localAsr.modelId', 'sensevoice-small-gguf') as string
    if (!modelId) {
      addRuntimeEvent('warn', 'local', 'No local model selected; provider is not ready')
      callbacks.onReady?.({ asr: false, llm: false })
      return false
    }

    let downloaded = false
    try {
      const models = await invoke<{ id: string; complete: boolean }[]>('list_downloaded_models')
      downloaded = models.some((m) => m.id === modelId && m.complete)
    } catch (err) {
      // 讀不到列表時不敢斷言就緒：寧可攔下並提示，也別錄完才失敗
      addRuntimeEvent('warn', 'local', 'Could not read local model list; provider is not ready', { error: String(err) })
      callbacks.onReady?.({ asr: false, llm: false })
      return false
    }

    if (!downloaded) {
      addRuntimeEvent('warn', 'local', 'Selected local model is not downloaded; provider is not ready', { modelId })
      callbacks.onReady?.({ asr: false, llm: false })
      return false
    }

    // 已下載：預載入失敗（如檔案損壞、顯示卡後端異常）不代表不能用，
    // 真正的失敗會在識別階段帶著具體原因報出來，這裡仍按就緒處理。
    try {
      // [自訂] SC-62：互不依賴的設定一起送出（Rust 端仍依序處理）。
      const [accelerator, gpuDevice] = await Promise.all([
        getSetting('localAsr.accelerator', 'auto') as Promise<string>,
        getSetting('localAsr.gpuDevice', '') as Promise<string>,
      ])
      await invoke<string>('preload_local_model', { modelId, accelerator, gpuDevice })
    } catch (err) {
      addRuntimeEvent('warn', 'local', 'Local model preload failed; provider remains marked ready', { error: String(err) })
    }
    callbacks.onReady?.({ asr: true, llm: false })
    return true
  }

  protected async processAudio(pcm: Uint8Array, durationSec: number, runId: number): Promise<void> {
    if (!this.isRunCurrent(runId)) return
    const startOpts = this.startOpts
    const startTime = performance.now()

    addRuntimeEvent('info', 'local', 'Local ASR started', { durationSec, runId })
    let asrText = ''
    let asrMs = 0

    try {
      // [自訂] SC-62：四個設定原本逐一往返；改成一起送出，仍是停止時讀到的最新值。
      // Rust 端的設定佇列照舊依序處理，同一個鍵的讀寫順序不變。
      const [modelId, language, accelerator, gpuDevice] = await Promise.all([
        getSetting('localAsr.modelId', 'sensevoice-small-gguf'),
        getSetting('localAsr.language', 'auto'),
        getSetting('localAsr.accelerator', 'auto'),
        // 必須跟著傳：它是引擎快取 key 的一部分，這裡漏掉就會與預載入的那份不符，
        // 每次口述都白付一次"卸舊 + 載新 + 預熱"。
        getSetting('localAsr.gpuDevice', ''),
      ])
      if (!this.isRunCurrent(runId)) return

      const result = await bridge.localTranscribeRaw(pcm, {
        modelId,
        language,
        accelerator,
        gpuDevice,
      })
      if (!this.isRunCurrent(runId)) return
      asrText = result.text
      asrMs = result.elapsed_ms
    } catch (err) {
      if (!this.isRunCurrent(runId)) return
      addRuntimeEvent('error', 'local', 'Local ASR failed', { error: String(err) })
      // [自訂] SC-35：保留 Rust 錯誤碼的起始位置，避免 Error: 前綴。
      this.callbacks.onError?.(err instanceof Error ? err.message || String(err) : String(err))
      this.callbacks.onDone?.()
      return
    }

    if (!this.isRunCurrent(runId)) return
    this.callbacks.onASR?.({ text: asrText, asrMs, durationSec })

    // 策略與結果判據與即時錄音、其它兩種模式共用同一份；時長用本地 PCM 實際秒數，
    // 不四捨五入（"恰好等於門檻"這一檔會判錯）。
    const policy = policyFromSnapshot(startOpts?.aiConfig, 'local', durationSec)
    const outcomeContext: AiOutcomeContext = {
      operationId: startOpts?.operationId || `local-${runId}`,
      trigger: startOpts?.source === 'history_reprocess' ? 'history_reprocess' : 'live',
    }

    if (!asrText.trim()) {
      if (!this.isRunCurrent(runId)) return
      // 空識別也要有結論：以前這條路直接 return，日誌裡查不到"為什麼沒整理"。
      const outcome = resolveAndLogAiOutcome(outcomeContext, policy, { asrTextEmpty: true })
      this.callbacks.onFinal?.({
        asrText: '',
        llmText: '',
        asrMs,
        llmMs: 0,
        durationSec,
        aiSource: outcome.source,
        aiStatus: outcome.status,
      })
      this.callbacks.onDone?.()
      return
    }

    const polish = await polishWithClientAi({
      asrText,
      startOptions: startOpts,
      policy,
      outcomeContext,
      logSource: 'local',
      isCurrent: () => this.isRunCurrent(runId),
    })
    if (!polish || !this.isRunCurrent(runId)) return

    const totalMs = Math.round(performance.now() - startTime)
    addRuntimeEvent('info', 'local', 'Processing complete', {
      durationSec,
      asrMs,
      llmMs: polish.llmMs,
      totalMs,
      runId,
    })

    this.callbacks.onFinal?.({
      asrText,
      asrMs,
      durationSec,
      ...polish,
    })
    this.callbacks.onDone?.()
  }
}
