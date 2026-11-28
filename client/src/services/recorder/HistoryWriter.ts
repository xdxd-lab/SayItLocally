import * as bridge from '../bridge'
import { addRuntimeEvent } from '../debugLog'
import { resolveAsrDisplayModel } from '@/lib/asrModels'
import { addHistory, deleteHistory, getSetting, type HistoryFailReasonCode, type HistoryRecord } from '../store'
import type { FinalResult } from '../transcription'
import type { WorkMode } from '../transcription/types'
import { t } from '@/i18n'
import type { AudioArchiveService } from './AudioArchiveService'
import type { FailureRecovery } from './OverlayService'

export /** buildHistoryMetadata 的產物：錄音時的應用/視窗/Prompt 上下文。 */
type HistoryMetadata = Pick<
  HistoryRecord,
  | 'appId'
  | 'appName'
  | 'windowTitle'
  | 'processName'
  | 'windowClass'
  | 'promptPresetId'
  | 'promptPresetName'
  | 'promptRuleId'
  | 'promptSummary'
  | 'workMode'
>

type HistoryArtifact = { runId: number; recordId: string; audioFilePath?: string }

/**
 * 錄音歷史的寫入與清理。從 RecorderOrchestrator 拆出（模組 6），行為不變。
 *
 * 成功（有文本）和失敗（只有音訊）兩條路都寫到這裡；使用者主動取消的代次在寫入前後各驗一次，
 * 取消了就把記錄和音訊一起刪掉。
 */
export class HistoryWriter {
  /**
   * 被使用者**顯式**取消的代次（Esc / 取消錄音）。
   *
   * 與「run 失效」不是一回事：使用者在超時後立刻再按熱鍵，新 run 會讓舊 runId 失效，
   * 但上一段錄音的音訊和歷史必須留下。以前這兩種情況共用 isRunCurrent 判據，於是
   * 2026-09-07 實測丟掉了一整段 60 秒錄音（超時兜底回撥在 0.75 秒後被新錄音判為失效，
   * 直接 return，音訊、記錄、日誌三樣都沒有）。
   */
  private canceledRuns = new Set<number>()
  /** 正在寫入的那條記錄：寫到一半使用者按了 Esc，取消路徑要拿它去刪。 */
  private pending: HistoryArtifact | null = null

  constructor(private readonly audioArchive: AudioArchiveService) {}

  getPending(): HistoryArtifact | null {
    return this.pending
  }

  clearPending() {
    this.pending = null
  }

  clearPendingFor(runId: number) {
    if (this.pending?.runId === runId) {
      this.pending = null
    }
  }

  /** 標記這一代是被使用者主動取消的（只有 Esc / 取消錄音走這裡）。 */
  markRunCanceled(runId: number) {
    if (runId === 0) return
    this.canceledRuns.add(runId)
    // 長期執行下別無界增長；只保留最近若干代，足夠覆蓋仍在飛的非同步收尾。
    if (this.canceledRuns.size > 64) {
      for (const id of this.canceledRuns) {
        if (this.canceledRuns.size <= 32) break
        this.canceledRuns.delete(id)
      }
    }
  }

  /**
   * 這一代的歷史存檔該不該丟棄。
   *
   * 判據是「使用者是否主動取消」，**不是** isRunCurrent。使用者在上一段還沒收尾時又按了熱鍵，
   * 舊 run 會失效但他並沒有要求丟掉上一段——那段音訊是他剛說完的，必須留下。
   */
  isRunCanceled(runId: number) {
    return runId === 0 || this.canceledRuns.has(runId)
  }

  /**
   * 把「有音訊但沒有可用文本」的一次錄音寫進歷史，供回放與重新識別。
   *
   * 空識別、供應商報錯、處理超時、連線斷開四條路共用它：音訊已經在 audioArchive.begin 裡
   * 存好了，這裡只負責補一條帶明確原因的記錄。
   *
   * 返回**能不能恢復**，而不是"有沒有寫歷史"：只有歷史記錄和音訊檔案都在，使用者才真的
   * 能去重新識別。介面上那句"可在歷史記錄重新識別"只能由這個返回值決定 —— 靠
   * historyEnabled / audioRetentionEnabled 兩個開關推斷是不夠的，寫盤還會失敗、
   * 還會超時。許一個作廢的承諾比什麼都不說更糟。
   */
  async writeFailedRun(params: {
    runId: number
    audioDurationSec: number
    wallTimeSec: number
    asrMs?: number
    asrDurationSec?: number
    failReason?: string
    failReasonCode?: HistoryFailReasonCode
    historyMeta: HistoryMetadata
    aiSource?: HistoryRecord['aiSource']
    aiStatus?: HistoryRecord['aiStatus']
  }): Promise<FailureRecovery> {
    const { runId } = params
    let artifact: { recordId: string; audioFilePath?: string; pendingLate?: boolean } | null = null
    try {
      const historyEnabled = await getSetting('historyEnabled', false)
      if (!historyEnabled) return 'none'
      artifact = await this.audioArchive.take(runId)
      if (!artifact) return 'none'
      if (this.isRunCanceled(runId)) {
        await this.discard(artifact)
        return 'none'
      }
      this.pending = { runId, ...artifact }
      await addHistory({
        ...params.historyMeta,
        id: artifact.recordId,
        timestamp: Date.now(),
        asrText: '',
        llmText: '',
        asrMs: params.asrMs ?? 0,
        llmMs: 0,
        durationSec: params.wallTimeSec,
        audioDurationSec: params.audioDurationSec > 0 ? params.audioDurationSec : undefined,
        asrDurationSec: params.asrDurationSec,
        charCount: 0,
        isEmpty: true,
        failReason: params.failReason,
        failReasonCode: params.failReasonCode,
        aiSource: params.aiSource,
        aiStatus: params.aiStatus,
        audioFilePath: artifact.audioFilePath,
      })
      // 寫入後再驗一次：取消可能發生在 addHistory 期間。
      if (this.isRunCanceled(runId)) {
        await this.discard(artifact)
        return 'none'
      }
      void bridge.emit('history-updated')
      addRuntimeEvent('info', 'recorder', 'Saved recording to history without text', {
        runId,
        recordId: artifact.recordId,
        audioSaved: Boolean(artifact.audioFilePath),
        audioSec: params.audioDurationSec,
        failReasonCode: params.failReasonCode,
      })
      // 沒有音訊檔案就沒法重新識別（歷史頁的重試按鈕同樣以 audioFilePath 為條件）。
      //
      // 三分而不是兩分：落盤還在繼續時（pendingLate）返回 'unknown'，介面什麼都不說。
      // 說成 'none'（"這次沒有留下錄音"）是**錯的** —— 那條路稍後大機率會成功並把
      // 路徑補進歷史，而使用者已經被告知錄音沒了。
      if (artifact.audioFilePath) return 'history'
      return artifact.pendingLate ? 'unknown' : 'none'
    } catch (error) {
      if (artifact) await this.discard(artifact)
      addRuntimeEvent('warn', 'recorder', 'Failed to save recording to history', {
        runId,
        error: String(error),
        failReasonCode: params.failReasonCode,
      })
      return 'none'
    }
  }

  async discard(artifact: { recordId: string; audioFilePath?: string }) {
    // [自訂] REV9：先移除歷史，錄音等備份釋放後再刪，讓聽寫收尾繼續。
    try {
      await deleteHistory(artifact.recordId)
      void bridge.emit('history-updated')
    } catch (error) {
      addRuntimeEvent('warn', 'recorder', 'Failed to discard history entry', { error: String(error) })
    }
    const audioFilePath = artifact.audioFilePath
    if (audioFilePath) {
      void Promise.resolve().then(() => bridge.deleteAudioFile(audioFilePath)).catch(error => {
        addRuntimeEvent('warn', 'recorder', 'Failed to discard archived audio', { error: String(error) })
      })
    }
  }

  /**
   * 有結果的那一次寫歷史（processFinalResult 裡那一段原樣搬來）。
   *
   * aborted=true 表示這一代已經失效或被使用者取消，呼叫方應當直接收手（原來這裡是 return）。
   * 寫入拋異常時不算 aborted：原來的 catch 之後照樣往下走去插字，artifact 保持原值。
   */
  async writeFinalRun(params: {
    runId: number
    result: FinalResult
    textToPaste: string
    selectionNotApplied: boolean
    audioDurationSec: number
    wallTimeSec: number
    /** 原來在幾次 await 之後才讀工作模式，這裡保持同一時刻取值。 */
    mode: () => WorkMode
    isRunCurrent: () => boolean
    metadata: () => HistoryMetadata
  }): Promise<{ aborted: true } | { aborted: false; artifact: HistoryArtifact | null }> {
    const { runId, result } = params
    // [自訂] S68：貼回原選取文字只用於插入，歷史與備份只保存口述內容。
    const historyText = params.selectionNotApplied ? result.asrText : params.textToPaste
    const hasText = Boolean(historyText?.trim())
    const audioDur = params.audioDurationSec
    const wallSec = params.wallTimeSec
    let historyArtifact: HistoryArtifact | null = null
    try {
      const historyEnabled = await getSetting('historyEnabled', false)
      if (!params.isRunCurrent()) return { aborted: true }
      if (historyEnabled) {
        // 音訊在 stopRecording 時就開始寫盤了；這裡只等它落定，不再寫第二份。
        const archived = await this.audioArchive.take(runId)
        const recordId = archived?.recordId
          ?? (Date.now().toString(36) + Math.random().toString(36).slice(2, 6))
        historyArtifact = { runId, recordId, audioFilePath: archived?.audioFilePath }

        if (this.isRunCanceled(runId)) {
          await this.discard(historyArtifact)
          return { aborted: true }
        }
        const providerMeta = await this.buildProviderMetadata(params.mode(), result)
        if (this.isRunCanceled(runId)) {
          await this.discard(historyArtifact)
          return { aborted: true }
        }

        this.pending = historyArtifact
        await addHistory({
          id: recordId,
          timestamp: Date.now(),
          asrText: result.asrText,
          llmText: historyText,
          asrMs: result.asrMs,
          llmMs: result.llmMs,
          durationSec: wallSec,
          audioDurationSec: audioDur > 0 ? audioDur : undefined,
          asrDurationSec: result.durationSec > 0 ? result.durationSec : undefined,
          charCount: hasText ? historyText.length : 0,
          isEmpty: !hasText,
          // 區分「ASR 本來就沒出字」和「ASR 出了字但後處理把它清空了」——
          // 後者是我們自己的問題（替換規則/格式化），別讓使用者以為是沒識別到
          failReason: hasText
            ? undefined
            : result.asrText?.trim()
              ? t('recorder.emptyAfterProcessing')
              : t('recorder.noTranscript'),
          failReasonCode: hasText
            ? undefined
            : result.asrText?.trim()
              ? 'empty_after_processing'
              : 'no_transcript',
          audioFilePath: historyArtifact.audioFilePath,
          ...params.metadata(),
          ...providerMeta,
        })
        if (this.isRunCanceled(runId)) {
          await this.discard(historyArtifact)
          return { aborted: true }
        }
        void bridge.emit('history-updated')
      }
    } catch (error) {
      if (historyArtifact) await this.discard(historyArtifact)
      addRuntimeEvent('warn', 'recorder', 'Failed to write history entry', { error: String(error), runId })
    }
    return { aborted: false, artifact: historyArtifact }
  }

  /** 非同步獲取當前模式下的 ASR/AI 供應商資訊 */
  async buildProviderMetadata(mode: WorkMode, finalResult?: FinalResult): Promise<{
    asrProvider?: string
    aiProvider?: string
    aiModel?: string
    aiSource?: FinalResult['aiSource']
    aiStatus?: FinalResult['aiStatus']
  }> {
    const executionMeta = {
      aiSource: finalResult?.aiSource,
      aiStatus: finalResult?.aiStatus,
    }
    if (mode === 'server') {
      let asrProvider = finalResult?.asrModel || finalResult?.asrEngine || 'server'
      // 後端返回 HuggingFace repo 全名如 "Qwen/Qwen3-ASR-1.7B"，只取模型名
      const slashIdx = asrProvider.lastIndexOf('/')
      if (slashIdx >= 0) asrProvider = asrProvider.slice(slashIdx + 1)
      if (finalResult?.aiSource === 'custom') {
        return {
          asrProvider,
          aiProvider: finalResult.aiProvider,
          aiModel: finalResult.aiModel,
          ...executionMeta,
        }
      }
      return {
        asrProvider,
        aiProvider: finalResult?.aiSource === 'none' ? undefined : 'server',
        ...executionMeta,
      }
    }
    if (mode === 'cloud_api') {
      const asrProviderKey = await getSetting('cloudAsr.provider', '') as string
      // 選定的模型要一起讀：Groq / OpenAI 那幾個服務同一個 id 下有多個模型，
      // 只按 id 推會把歷史記錄寫成該服務的預設模型，而不是這次真正用的那個。
      //
      // 對映表以前在這裡抄了第三份（lib/asrModels.ts、AsrTestSection 各一份），
      // 加一個供應商就得改三處、漏一處只會靜默顯示錯的模型名。現在只有一個出處。
      const asrSelectedModel = await getSetting('cloudAsr.model', '') as string
      const asrProvider = asrProviderKey
        ? resolveAsrDisplayModel(asrProviderKey, asrSelectedModel)
        : 'cloud'
      const aiProvider = finalResult?.aiProvider || await getSetting('cloudAi.provider', '') as string
      const aiModel = finalResult?.aiModel || await getSetting('cloudAi.model', '') as string
      return { asrProvider, aiProvider: aiProvider || undefined, aiModel: aiModel || undefined, ...executionMeta }
    }
    if (mode === 'local') {
      const modelId = await getSetting('localAsr.modelId', '') as string
      const aiEnabled = Boolean(await getSetting('aiEnabled', false))
      const aiProvider = finalResult?.aiProvider
        || (aiEnabled ? await getSetting('cloudAi.provider', '') as string : undefined)
      const aiModel = finalResult?.aiModel
        || (aiEnabled ? await getSetting('cloudAi.model', '') as string : undefined)
      return { asrProvider: modelId || 'local', aiProvider, aiModel: aiModel || undefined, ...executionMeta }
    }
    return executionMeta
  }
}
