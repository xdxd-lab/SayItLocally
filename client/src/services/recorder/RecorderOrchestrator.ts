import * as bridge from '../bridge'
import { matchRealEndpoint, startCapture, stopCapture, type ActiveMicrophoneInfo } from '../audio'
import { isVirtualLabel, rememberWorkingMic, resolveMicPlan, type CaptureEndpoint, type MicPlan } from '../micRouting'
import { getProvider, MID_SESSION_DISCONNECT_ERROR, type TranscriptionProvider, type TranscriptionCallbacks, type FinalResult } from '../transcription'
import { resolveAiPolicy, serverShouldPolish, type AiConfigSnapshot } from '../transcription/aiPolicy'
import { getRuntimeServerAiSource } from '../transcription/serverAiSource'
import { loadSelectionAiConfig } from '../transcription/selectionAi'
import { isStreamingDisplayReady } from '@/lib/asrModels'
import {
  getActivePresetId,
  getPromptPresets,
  getSetting,
  setActivePresetId,
  type HistoryFailReasonCode,
  type HistoryRecord,
  type PromptPreset,
} from '../store'
import { setActivePresetKnown } from '../../stores/activePreset'
import { addRuntimeEvent } from '../debugLog'
import {
  BUILTIN_SET_ACTIVE_KEY,
  BUILTIN_SET_EXTRA_WORDS_KEY,
  BUILTIN_SET_WORDS_KEY,
  CUSTOM_THEME_ACTIVE_KEY,
  CUSTOM_THEMES_KEY,
  composeHotwords,
  normalizeBuiltinSetActive,
  normalizeBuiltinSetWords,
  normalizeCustomThemeActive,
  normalizeCustomThemes,
} from '../hotwords/model'
import { createWaveformBarState, computeBarsFromPCM, resetWaveformBarState } from '../waveform'
import { elapsedSecFromPerf } from '../timeModel'
import {
  captureActiveInsertionTarget,
  clearCapturedInsertionTarget,
  startInsertionTargetTracking,
  stopInsertionTargetTracking,
} from '../textInsertion'
import { applyTextTransforms } from '../textPostProcess'
import type { ActiveAppContext } from '../../types/appContext'
import type { ClientRuntimeInfo } from '../../types/appApi'
import { OverlayService, type FailureRecovery } from './OverlayService'
import { PasteService, type ProbeResult } from './PasteService'
import { AudioArchiveService } from './AudioArchiveService'
import { MicLevelMonitor } from './MicLevelMonitor'
import { HistoryWriter, type HistoryMetadata } from './HistoryWriter'
import { RunState } from './RunState'
import { InsertionController } from './InsertionController'
import { defaultRecordingSettings, type RecordingSettings } from './RecordingSettings'
import { createDefaultUserStats } from '../personalization/defaults'
import { resolvePromptRouting } from '../personalization/promptRouter'
import {
  getAppPromptRules,
  getUserStats,
  recordSessionStats,
} from '../personalization/store'
import type { PromptResolution, UserStats } from '../personalization/types'
import type { PTTEventPayload, RecorderState } from './types'
import { MAX_RECORDING_SEC, RECORDING_COUNTDOWN_SEC } from './types'
import {
  summarizeAppContext as _summarizeAppContext,
  buildStatsAppId as _buildStatsAppId,
  isModifierPTTSetting as _isModifierPTTSetting,
  computeProcessingTimeoutMs as _computeProcessingTimeoutMs,
} from './helpers'
import { t } from '@/i18n'
import { describeProviderError } from '@/lib/errorMessages'
import { cleanActiveMicLabel, describeMicSource, micSourceChanged } from './micSourceReminder'
import {
  CONTEXT_SELECTION_EDIT_PROMPT,
  CONTEXT_SELECTION_EDIT_PROMPT_SETTING_KEY,
  describeTextContextForLog,
  normalizeContextSelectionEditPrompt,
  resolveContextAwareOutput,
  usableTextContext,
  withContextAwareInstructions,
} from '../contextAware'

type StateTransition =
  | ['idle', 'recording']
  | ['recording', 'processing']
  | ['recording', 'idle']
  | ['processing', 'idle']

const VALID_TRANSITIONS: StateTransition[] = [
  ['idle', 'recording'],
  ['recording', 'processing'],
  ['recording', 'idle'],
  ['processing', 'idle'],
]

const LATE_FINAL_GRACE_MS = 15000
/** 文本插入還沒收尾時，超時定時器每次順延多久。 */
const INSERTION_TIMEOUT_EXTENSION_MS = 5000
/**
 * 插入階段專屬超時。見 armInsertionTimeout 的註釋 —— 處理超時在 onFinal 裡就被清掉了，
 * 管不到插入這一段，所以這一段必須自帶一個。
 */
const INSERTION_TIMEOUT_MS = 15000
/** 順延次數上限；用盡就強制把介面從「處理中」放出來。 */
const MAX_INSERTION_TIMEOUT_EXTENSIONS = 3
const MODIFIER_PTT_RELEASE_GUARD_MS = 200
/** [自訂] S40：停用使用統計 userStats 的保存（與 asrCorrection.ts 的 ASR_CORRECTION_DISABLED 同一寫法） */
const USER_STATS_RECORDING_DISABLED = true as boolean

function classifyHistoryProviderFailure(message: string): HistoryFailReasonCode {
  // 錄音中掉線要單列：這段語音從未送達服務端，跟「服務端處理失敗」的排查方向完全不同。
  if (message === MID_SESSION_DISCONNECT_ERROR) return 'connection_lost'
  const code = describeProviderError(message).code
  switch (code) {
    case 'provider_timeout':
    case 'provider_unreachable':
    case 'provider_bad_key':
    case 'provider_forbidden':
    case 'provider_rate_limit':
    case 'provider_no_model':
      return code
    default:
      return 'provider_failed'
  }
}

interface TimedOutProcessingContext {
  runId: number
  timedOutAt: number
  /**
   * 這一代是否已經收尾（遲到 final 已被消費並落庫）。
   *
   * 寬限期兜底定時器**只認這個標誌**，不再看 `timedOutProcessingContext` 還指著自己：
   * 新一次錄音會在 startRecording 裡把那個欄位清空，於是兜底定時器直接 return，
   * 上一段錄音的音訊與歷史一起消失（實測丟過 60 秒錄音）。使用者又按了一次熱鍵，
   * 不等於他放棄了上一段。
   */
  settled: boolean
  audioDurationSec: number
  wallTimeSec: number
  promptResolution: PromptResolution | null
  appContext: ActiveAppContext | null
  audioChunks: ArrayBuffer[]
  probeResult: ProbeResult | null
}

interface ResetToIdleOptions {
  keepOverlay?: boolean
  preserveLateFinalContext?: boolean
}



export class RecorderOrchestrator {
  private state: RecorderState = 'idle'
  /** 本次錄音的執行期資料（模組 6）。欄位的復位時機與原來逐欄位寫時完全一致。 */
  private readonly run = new RunState()
  private onStateChange: ((s: RecorderState) => void) | null = null
  private initialized = false
  private runSequence = 0
  private activeRunId = 0
  private recordingFocus: { runId: number; token: string } | null = null // [自訂] L-80

  private textInsertionInFlight = false
  /**
   * 正在插入的那段文本。
   *
   * 只為一件事存在：插入收尾卡死、順延用盡時，把文字交還給使用者（展示結果卡）。
   * 那條路上文本已經識別出來了，歷史也已落庫，唯獨沒人知道它到底進沒進輸入框 ——
   * 以前那裡直接 resetToIdle()，介面憑空恢復空閒，使用者什麼也沒拿到。
   */
  private textBeingInserted = ''
  /**
   * 使用者已明確放棄等待遲到結果（寬限期內按了 Esc，或關掉了失敗卡）。
   *
   * 置位後遲到的 final 只許落歷史，**絕不自動插字**：介面已經告訴他"失敗了"，
   * 再往輸入框裡塞字是最讓人措手不及的行為。
   */
  private lateResultAbandoned = false
  /**
   * 正在"超時後等遲到結果"的那一代。
   *
   * 不能拿 `timedOutProcessingContext` 當這個判據：遲到 final 一到達就會被
   * `consumeTimedOutProcessingContext()` 清成 null，而此後還要非同步落歷史、再插字。
   * 那段時間裡懸浮窗仍顯示"仍在等結果"，使用者按 Esc 卻因為上下文沒了被當成過期事件
   * 丟掉 —— 提示關不掉，文字隨後照樣插進去。這個欄位活到**真正收尾**為止。
   */
  private lateResultRunId = 0
  /** 遲到 final 正在完成歷史/插入；此期間禁止新錄音複用 Provider。 */
  private finalizingLateRunId = 0
  /** 當前 fallback 卡片對應的 run token；用於拒絕佇列中遲到的舊 dismiss。 */
  private activeFallbackToken = 0
  /** Guard against re-entrant startRecording calls during async setup */
  private startRecordingLock = false
  private processingTimeoutId: ReturnType<typeof setTimeout> | null = null
  /** 插入階段專屬超時；處理超時在 onFinal 裡已被清掉，管不到這一段。 */
  private insertionTimeoutId: ReturnType<typeof setTimeout> | null = null
  private timedOutProcessingContext: TimedOutProcessingContext | null = null
  /** 各代錄音的音訊存檔（停止錄音就落盤，不等識別結果）。 */
  private audioArchive = new AudioArchiveService((runId) => this.overlayService.updateFailureRecovery('history', runId))
  /** 歷史記錄的寫入、取消後的清理，以及「哪些代被使用者主動取消」。 */
  private readonly history = new HistoryWriter(this.audioArchive)

  private handsFreeMode = false
  private pttSuppressed = false
  private lastToggleTime = 0
  private lastPTTUpAt = 0
  private lastPTTUpUsedModifier = false

  /** 錄音要用的設定（麥克風、AI 整理、熱詞……），見 RecordingSettings。 */
  private settings: RecordingSettings = defaultRecordingSettings()
  /** Last successfully opened input route in this app process. Kept in memory on purpose. */
  private lastMicSourceIdentity: string | null = null
  /** 本次錄音實際開啟的麥克風：真實名字 + 是否虛擬裝置（null = 判斷不了）。每次開錄重置。 */
  private activeMic: { label: string; isVirtual: boolean | null } | null = null
  /** 標記本次錄音是否已施加系統靜音，用於配對恢復。 */
  private systemMuteApplied = false
  /** 延遲靜音定時器（等提示音播完再靜音）。 */
  private systemMuteTimerId: ReturnType<typeof setTimeout> | null = null
  private clientRuntimeInfo: ClientRuntimeInfo | null = null
  /** 客戶端執行時資訊是否已成功採集一次。它是程序內靜態後設資料（區域/記憶體/主機名等），
   *  只需啟動時取一次，不該掛在每次改設定都觸發的重新整理熱路徑上。失敗時保持 false 以便重試。 */
  private clientRuntimeInfoLoaded = false
  private userStats: UserStats = createDefaultUserStats()

  private readonly overlayWaveState = createWaveformBarState()
  private readonly overlayService = new OverlayService(() => this.getLiveElapsedSec())
  /** 音量兩檔提醒、系統靜音判定、本次錄音的音訊統計。 */
  private readonly micLevel = new MicLevelMonitor({
    overlay: this.overlayService,
    isRunCurrent: (runId) => this.isRunCurrent(runId),
    getState: () => this.state,
    isStarting: () => this.startRecordingLock,
    getActiveRunId: () => this.activeRunId,
    getSelectedMicId: () => this.settings.micId,
    cancelForMutedMic: (runId) => { void this.cancelRecording(runId, { showCanceled: false, reason: 'mic_muted_timeout' }) },
    isActiveMicVirtual: () => this.activeMic?.isVirtual === true,
    // 只記真實麥克風：系統預設被換成虛擬音效卡時，靠這條記錄挑回使用者平時用的那個。
    onVoiceDetected: () => {
      const mic = this.activeMic
      if (mic && mic.isVirtual === false && mic.label) void rememberWorkingMic(mic.label)
    },
  })
  /** 拿目標視窗、判斷能不能寫、貼上、給結果分類。狀態怎麼變仍由編排器決定。 */
  private readonly insertion = new InsertionController(new PasteService())
  private get provider(): TranscriptionProvider { return getProvider() }
  /** 5-minute auto-stop timer for hands-free mode */
  private handsFreeAutoStopId: ReturnType<typeof setTimeout> | null = null

  setStateListener(cb: (s: RecorderState) => void) {
    this.onStateChange = cb
  }

  /** 把 getUserMedia 實際開啟的裝置落到真實名字上，並對照原生端點清單判斷它是不是虛擬裝置。 */
  private describeActiveMic(
    active: ActiveMicrophoneInfo,
    endpoints: CaptureEndpoint[],
  ): { label: string; isVirtual: boolean | null } {
    const label = matchRealEndpoint(active, active.devices ?? [])?.label || cleanActiveMicLabel(active.label ?? '')
    return { label, isVirtual: endpoints.length > 0 ? isVirtualLabel(label, endpoints) : null }
  }

  getState() {
    return this.state
  }

  private isRunCurrent(runId: number) {
    return runId !== 0 && this.activeRunId === runId
  }




  /**
   * 一次失敗的統一收尾：**立刻**顯示失敗卡，隨後按實際存檔結果把恢復提示補上。
   *
   * 收口成一處的理由：失敗有五條路（stop 沒送出去、錄音中報錯、處理中報錯、處理超時、
   * 有錄音卻沒結果），以前每條各自決定要不要提示、顯示幾秒、要不要留懸浮窗 —— 結果
   * 其中兩條完全靜默，懸浮窗憑空消失，使用者報的正是這個。再加一條失敗路徑時，只要
   * 走這裡就不會重犯。
   *
   * 兩段式是刻意的：提示必須在失敗那一刻出現（使用者正盯著螢幕等結果），而存檔結論是
   * 非同步的、最長能等 30 秒。先說"失敗了"，再補"錄音留下了"。
   */
  private async failRunWithCard(runId: number, params: {
    title: string
    detail?: string
    audioDurationSec: number
    wallTimeSec: number
    failReason: string
    failReasonCode: HistoryFailReasonCode
    historyMeta: HistoryMetadata
    asrMs?: number
    asrDurationSec?: number
    aiSource?: HistoryRecord['aiSource']
    aiStatus?: HistoryRecord['aiStatus']
  }): Promise<void> {
    const showCard = this.isRunCurrent(runId)
    if (showCard) {
      // 必須先認領 token，再顯示卡片：Esc 的 dismiss_fallback 分支拿 activeFallbackToken
      // 比對，不設的話按 Esc 會被當成"遲到的舊關閉請求"丟掉，卡片關不掉。
      // （這個欄位只在 startRecording 裡清零，所以下面的 resetToIdle 不會把它弄丟。）
      this.activeFallbackToken = runId
      this.overlayService.showFailure({
        title: params.title,
        detail: params.detail,
        recovery: 'unknown',
        token: runId,
      })
    }

    const recovery = await this.history.writeFailedRun({
      runId,
      audioDurationSec: params.audioDurationSec,
      wallTimeSec: params.wallTimeSec,
      asrMs: params.asrMs,
      asrDurationSec: params.asrDurationSec,
      failReason: params.failReason,
      failReasonCode: params.failReasonCode,
      historyMeta: params.historyMeta,
      aiSource: params.aiSource,
      aiStatus: params.aiStatus,
    })
    if (showCard) this.overlayService.updateFailureRecovery(recovery, runId)
  }

  /**
   * 已經確定不會有結果了：存歷史 + 給出可讀提示 + 立刻回 idle。
   *
   * 用於「請求根本沒送出去」這類當場就能判定的失敗。關鍵是**不進等待** —— 讓懸浮條轉滿
   * 一分鐘再什麼都不說，是最糟的處理方式，使用者既不知道發生了什麼，也不知道錄音還在不在。
   */
  private async failRunWithoutResult(runId: number, params: {
    audioDurationSec: number
    wallTimeSec: number
    failReason: string
    failReasonCode: HistoryFailReasonCode
    title: string
    detail?: string
  }): Promise<void> {
    this.clearProcessingTimeout()
    this.run.processingCancelable = false
    const historyMeta = this.buildHistoryMetadata(
      this.run.currentPromptResolution,
      this.run.currentActiveAppContext,
    )
    addRuntimeEvent('warn', 'recorder', 'Run failed without any result', {
      runId,
      audioSec: params.audioDurationSec,
      failReasonCode: params.failReasonCode,
      mode: this.provider.mode,
    })
    this.provider.cancel()
    if (this.provider.mode === 'server') this.ensureConnection()
    await this.failRunWithCard(runId, {
      title: params.title,
      detail: params.detail,
      audioDurationSec: params.audioDurationSec,
      wallTimeSec: params.wallTimeSec,
      failReason: params.failReason,
      failReasonCode: params.failReasonCode,
      historyMeta,
    })
    if (!this.isRunCurrent(runId)) return
    this.finishRun(runId)
    this.resetToIdle({ keepOverlay: true })
  }

  private releaseRecordingFocus(runId: number) {
    if (this.recordingFocus?.runId !== runId) return
    const { token } = this.recordingFocus
    this.recordingFocus = null
    void bridge.releaseRecordingFocus(token).catch(() => {})
  }

  /** 冪等結束指定代次；所有清理都帶 runId 條件，絕不清除隨後建立的新代次。 */
  private finishRun(runId: number) {
    if (runId === 0) return
    this.releaseRecordingFocus(runId)
    this.history.clearPendingFor(runId)
    if (this.timedOutProcessingContext?.runId === runId) {
      this.timedOutProcessingContext = null
    }
    if (this.activeRunId === runId) {
      this.activeRunId = 0
      this.run.processingCancelable = false
    }
  }

  /** 若已開啟「錄音時靜音系統聲音」，在就緒提示音播放之後再靜音系統輸出。
   *  延遲略長於提示音時長（~150ms），避免把提示音也一起靜掉。 */
  private scheduleSystemMuteIfEnabled() {
    if (!this.settings.muteSystemAudio) return
    this.clearSystemMuteTimer()
    this.systemMuteTimerId = setTimeout(() => {
      this.systemMuteTimerId = null
      // 若此時已不在錄音（短按等），不再靜音，避免殘留
      if (this.state !== 'recording') return
      this.applySystemMute()
    }, 250)
  }

  private applySystemMute() {
    if (this.systemMuteApplied) return
    this.systemMuteApplied = true
    void bridge.muteSystemOutput().catch((e) => {
      this.systemMuteApplied = false
      addRuntimeEvent('warn', 'recorder', 'Failed to mute system audio', { error: String(e) })
    })
  }

  private clearSystemMuteTimer() {
    if (this.systemMuteTimerId) {
      clearTimeout(this.systemMuteTimerId)
      this.systemMuteTimerId = null
    }
  }

  /** 恢復系統輸出到靜音前的狀態，並清除掛起的靜音定時器。 */
  private restoreSystemMuteIfNeeded() {
    this.clearSystemMuteTimer()
    if (!this.systemMuteApplied) return
    this.systemMuteApplied = false
    void bridge.restoreSystemOutput().catch((e) => {
      addRuntimeEvent('warn', 'recorder', 'Failed to restore system audio', { error: String(e) })
    })
  }

  /** 未就緒時的簡短提示文案（按當前工作模式區分）。 */
  private notReadyMessage(): string {
    switch (this.provider.mode) {
      case 'server':
        return t('recorder.serverDisconnected')
      case 'local':
        return t('recorder.modelNotDownloaded')
      default:
        return t('recorder.serviceNotReady')
    }
  }

  /** 透過快捷鍵切換當前潤色模式，並在空閒時用懸浮窗提示。 */
  private async handlePresetSwitch(presetId: string) {
    try {
      // 優先用已快取的預設列表，避免 IPC 等待導致的延時
      let target = this.settings.presets.find((p) => p.id === presetId)
      if (!target) {
        const presets = await getPromptPresets()
        target = presets.find((p) => p.id === presetId)
        this.settings.presets = presets
      }
      if (!target) {
        addRuntimeEvent('warn', 'recorder', 'Failed to switch cleanup preset: preset not found', { presetId })
        return
      }
      // 立即生效：更新錄音器快取 + 通知 UI + 懸浮窗提示（均無需等待 IPC）
      this.settings.activePresetId = presetId
      setActivePresetKnown(presetId, target.name)
      if (this.state === 'idle') {
        this.overlayService.showPresetSwitched(target.name)
      }
      addRuntimeEvent('info', 'recorder', 'Cleanup preset switched by shortcut', { presetId, name: target.name })
      // 持久化放到後台，不阻塞 UI 反饋
      void setActivePresetId(presetId)
    } catch (error) {
      addRuntimeEvent('error', 'recorder', 'Cleanup preset switch failed', { error: String(error) })
    }
  }

  /** 臨時停用/啟用 PTT（用於歡迎向導熱鍵確認步驟） */
  setPttSuppressed(suppressed: boolean) {
    this.pttSuppressed = suppressed
  }

  async init() {
    if (this.initialized) return
    this.initialized = true

    startInsertionTargetTracking()
    await this.refreshRuntimeSettings()
    // 靜態機器資訊只在啟動採集一次；不 await，避免拖慢建連（clientMeta 可空，採集期間為 null 無礙）。
    void this.ensureClientRuntimeInfo()
    this.ensureConnection()

    // 快捷鍵切換潤色模式（由 Rust global_shortcut 觸發）
    void bridge.listen('switch-preset', (event: unknown) => {
      const payload = (event as { payload?: { presetId?: string } })?.payload
      const presetId = payload?.presetId
      if (presetId) void this.handlePresetSwitch(presetId)
    })

    bridge.onEscapeAction(({ mode, token }) => {
      if (mode === 'cancel_recording') {
        // Esc 可能在錄音 stop 已發起、事件尚在 dispatcher 佇列時到達；若此時已進入
        // 可逆 processing，沿用同一 run token 取消，避免使用者明明按了卻被競態吞掉。
        if (this.state === 'processing' && this.run.processingCancelable) {
          this.cancelProcessing(token)
        } else {
          void this.cancelRecording(token)
        }
        return
      }
      if (mode === 'cancel_processing') {
        this.cancelProcessing(token)
        return
      }
      if (mode === 'dismiss_fallback') {
        if (token === 0 || token !== this.activeFallbackToken) {
          addRuntimeEvent('info', 'recorder', 'Ignored stale Esc fallback dismissal', {
            token,
            activeFallbackToken: this.activeFallbackToken,
          })
          return
        }
        addRuntimeEvent('info', 'recorder', 'Card dismissed with Esc', { token })
        this.activeFallbackToken = 0
        // 關掉失敗卡也算"我不等了"：之後遲到的結果只許落歷史，不許再插字。
        if (token === this.lateResultRunId) {
          this.lateResultAbandoned = true
          this.releaseRecordingFocus(token)
        }
        this.overlayService.hide()
        return
      }
      if (mode === 'abandon_late_result') {
        // 處理已超時、正在寬限期裡等遲到結果時按了 Esc。
        //
        // 只收起提示是不夠的 —— 關鍵是讓隨後到達的 final **不再自動插字**。否則介面
        // 上剛說完"不等了"，幾秒後文字還是會自己出現在輸入框裡，那是最讓人措手不及的
        // 一種行為。音訊和歷史照常保留，使用者仍可以去歷史裡重新識別。
        // 判據是 lateResultRunId 而不是 timedOutProcessingContext：後者在遲到 final
        // 到達那一刻就被消費掉了，而落歷史 + 插字還要花時間，這段時間裡使用者的 Esc
        // 會被當成過期事件丟掉（提示關不掉、文字照樣插進去）。
        if (token === 0 || token !== this.lateResultRunId) {
          addRuntimeEvent('info', 'recorder', 'Ignored stale Esc for late-result abandonment', {
            token,
            lateResultRunId: this.lateResultRunId,
          })
          return
        }
        addRuntimeEvent('info', 'recorder', 'User abandoned waiting for the late result', { token })
        this.lateResultAbandoned = true
        this.releaseRecordingFocus(token)
        this.overlayService.hide()
      }
    })

    // 懸浮窗自己收起了卡片（點關閉按鈕、或複製完成後自動收起）。必須在這裡收尾，
    // 否則 OverlayService 的續期定時器會在 8 秒後把按鍵接管重新註冊一遍。
    bridge.onCardDismissed(({ reason, token }) => {
      addRuntimeEvent('info', 'recorder', 'Overlay reported a dismissed card', { reason, token })
      this.overlayService.noteCardDismissed(token)
      if (token !== 0 && token === this.activeFallbackToken) {
        this.activeFallbackToken = 0
      }
      // 關掉提示也算"我不等了"：隨後到達的遲到結果只許落歷史，不許再插字。
      if (token !== 0 && token === this.lateResultRunId) {
        this.lateResultAbandoned = true
        this.releaseRecordingFocus(token)
      }
    })

    bridge.onPTTDown((payload) => {
      this.notePTTDown(payload)
      this.logPTTEvent('down', payload)
      if (this.pttSuppressed || this.handsFreeMode) {
        addRuntimeEvent('info', 'ptt', 'event:down ignored', {
          ...this.getPTTEventContext(payload),
          ignoreReason: this.pttSuppressed ? 'ptt_suppressed' : 'hands_free_mode',
        })
        return
      }
      if (this.state === 'idle') {
        addRuntimeEvent('info', 'ptt', 'event:down accepted -> startRecording', this.getPTTEventContext(payload))
        void this.startRecording()
        return
      }
      addRuntimeEvent('info', 'ptt', 'event:down ignored', {
        ...this.getPTTEventContext(payload),
        ignoreReason: 'state_not_idle',
      })
    })

    bridge.onPTTUp((payload) => {
      this.notePTTUp(payload)
      this.logPTTEvent('up', payload)
      if (this.pttSuppressed || this.handsFreeMode) {
        addRuntimeEvent('info', 'ptt', 'event:up ignored', {
          ...this.getPTTEventContext(payload),
          ignoreReason: this.pttSuppressed ? 'ptt_suppressed' : 'hands_free_mode',
        })
        return
      }
      if (this.state === 'recording') {
        addRuntimeEvent('info', 'ptt', 'event:up accepted -> stopRecording', this.getPTTEventContext(payload))
        void this.stopRecording()
        return
      }
      // PTT up arrived while startRecording is still initializing (state is still 'idle')
      if (this.startRecordingLock) {
        addRuntimeEvent('info', 'ptt', 'event:up deferred — startRecording in progress', this.getPTTEventContext(payload))
        this.run.pendingStopWhileStarting = true
        return
      }
      addRuntimeEvent('info', 'ptt', 'event:up ignored', {
        ...this.getPTTEventContext(payload),
        ignoreReason: 'state_not_recording',
      })
    })

    bridge.onPTTToggle((payload) => {
      this.logPTTEvent('toggle', payload)
      if (this.pttSuppressed || this.handsFreeMode) {
        addRuntimeEvent('info', 'ptt', 'event:toggle ignored', {
          ...this.getPTTEventContext(payload),
          ignoreReason: this.pttSuppressed ? 'ptt_suppressed' : 'hands_free_mode',
        })
        return
      }
      addRuntimeEvent('info', 'ptt', 'event:toggle accepted', this.getPTTEventContext(payload))
      this.pttToggle(false)
    })

    bridge.onToggleHandsFree((payload) => {
      this.logPTTEvent('hands_free', payload)
      if (this.pttSuppressed) {
        addRuntimeEvent('info', 'ptt', 'event:hands_free ignored', {
          ...this.getPTTEventContext(payload),
          ignoreReason: 'ptt_suppressed',
        })
        return
      }
      addRuntimeEvent('info', 'ptt', 'event:hands_free accepted', this.getPTTEventContext(payload))
      this.pttToggle(true)
    })

    // 9-minute warning from Rust keyboard hook (PTT hold mode)
    bridge.onPTTTimeoutWarning(() => {
      if (this.state === 'recording') {
        addRuntimeEvent('warn', 'recorder', 'Recording is approaching the five-minute limit')
        this.overlayService.showTimeoutWarning()
      }
    })
  }

  cleanup() {
    if (this.recordingFocus) this.releaseRecordingFocus(this.recordingFocus.runId)
    this.clearProcessingTimeout()
    this.micLevel.clearAutoCancelTimer()
    this.micLevel.invalidateProbes()
    this.overlayService.dispose()
    stopInsertionTargetTracking()
    void stopCapture().catch(() => { })
    this.provider.disconnect()
  }

  /** 僅更新 AI 整理開關的快取值，無 IPC/熱詞/語言等全量重新整理。
   *  供標題欄/設定頁的開關按鈕使用，避免快速切換時卡頓。 */
  setAiEnabledCache(next: boolean) {
    this.settings.aiEnabled = next
  }

  showAiEnabledToast(enabled: boolean) {
    if (this.state === 'idle') this.overlayService.showAiCleanupToggled(enabled)
  }

  /** 僅更新當前潤色模式（preset）的快取值，無 IPC/熱詞/語言等全量重新整理。
   *  供設定頁點選切換潤色模式使用，避免快速切換時卡頓。 */
  setActivePresetCache(id: string) {
    this.settings.activePresetId = id
  }

  /** 新建、編輯或刪除預設後同步最新列表，避免 active id 指向尚未進入執行時快取的新預設。 */
  setPromptPresetsCache(presets: PromptPreset[]) {
    this.settings.presets = presets.map((preset) => ({ ...preset }))
  }

  /** 熱詞變更後同步下一次錄音使用的快照；當前錄音仍使用開始時已捕獲的 StartOptions。 */
  setHotwordsCache(words: string[]) {
    this.settings.hotwords = Array.from(new Set(words.map((word) => word.trim()).filter(Boolean)))
  }

  /** 僅更新「流式即時顯示」開關快取，供外觀設定切換後立即生效。 */
  setStreamingDisplayCache(next: boolean) {
    this.settings.streamingDisplay = next
  }

  /** 錄音開始時即時判定是否啟用流式字幕氣泡（讀最新的開關/供應商/WorkspaceId，避免快取過期）。
   *  判據統一交給 isStreamingDisplayReady：doubao_v2 與 qwen_audio_stream 直接可用，
   *  qwen_realtime 還要配 WorkspaceId；qwen3-asr-flash 等非流式模型不啟用。 */
  private async applyStreamingActive(): Promise<void> {
    try {
      const [streamOn, provider, workspaceId] = await Promise.all([
        getSetting('streamingDisplayEnabled', false),
        getSetting('cloudAsr.provider', 'doubao_v2'),
        getSetting('cloudAsr.qwen.workspaceId', ''),
      ])
      if (this.state !== 'recording') return
      const active = Boolean(streamOn)
        && this.provider.mode === 'cloud_api'
        && isStreamingDisplayReady(String(provider || ''), String(workspaceId || ''))
      this.overlayService.setStreamingActive(active)
    } catch {
      /* ignore — 氣泡佔位是錦上添花，讀取失敗不影響錄音 */
    }
  }

  async refreshRuntimeSettings() {
    const [
      micId,
      muteSystemAudio,
      protectClipboard,
      presets,
      activePresetId,
      aiEnabled,
      aiMinDurationSec,
      contextAwareWriting,
      contextSelectionEditPrompt,
      appPromptRules,
      userStats,
      streamingDisplay,
      injectHotwords,
      noiseSuppression,
    ] = await Promise.all([
      getSetting('selectedMic', ''),
      getSetting('muteSystemAudioWhileRecording', false),
      getSetting('protectClipboard', true),
      getPromptPresets(),
      getActivePresetId(),
      getSetting('aiEnabled', false),
      getSetting('aiMinDurationSec', 0),
      getSetting('contextAwareWritingEnabled', false),
      getSetting(CONTEXT_SELECTION_EDIT_PROMPT_SETTING_KEY, CONTEXT_SELECTION_EDIT_PROMPT),
      getAppPromptRules(),
      getUserStats(),
      getSetting('streamingDisplayEnabled', false),
      getSetting('injectHotwordsToPrompt', false),
      getSetting('micNoiseSuppression', true),
    ])

    this.settings = {
      ...this.settings,
      noiseSuppression: Boolean(noiseSuppression),
      micId: String(micId || ''),
      muteSystemAudio: Boolean(muteSystemAudio),
      protectClipboard: Boolean(protectClipboard),
      injectHotwords: Boolean(injectHotwords),
      presets,
      activePresetId,
      aiEnabled: Boolean(aiEnabled),
      aiMinDurationSec: Math.max(0, Math.min(MAX_RECORDING_SEC, Number(aiMinDurationSec) || 0)),
      contextAwareWriting: Boolean(contextAwareWriting),
      contextSelectionEditPrompt: normalizeContextSelectionEditPrompt(contextSelectionEditPrompt),
      appPromptRules,
      streamingDisplay: Boolean(streamingDisplay),
    }
    this.userStats = userStats
    await this.overlayService.refreshSettings()

    // 熱詞 / 伺服器語言彼此獨立，並行載入而非依次 await，減少本函式的總耗時
    // （每次切換 AI 整理開關等都會觸發這裡）。
    // 客戶端執行時資訊（區域/記憶體/主機名等）是程序內靜態後設資料，已移出此熱路徑，
    // 僅在啟動時經 ensureClientRuntimeInfo() 採集一次，避免改任意設定都重新採集。
    const [hotwordsResult, languageResult] = await Promise.allSettled([
      Promise.all([
        getSetting(BUILTIN_SET_WORDS_KEY, {}),
        getSetting(BUILTIN_SET_ACTIVE_KEY, {}),
        getSetting(CUSTOM_THEMES_KEY, []),
        getSetting(CUSTOM_THEME_ACTIVE_KEY, {}),
        getSetting(BUILTIN_SET_EXTRA_WORDS_KEY, {}),
      ]).then(([rawSetWords, rawSetActive, rawCustomThemes, rawCustomThemeActive, rawSetExtraWords]) => {
        const setWords = normalizeBuiltinSetWords(rawSetWords, rawSetExtraWords)
        const setActive = normalizeBuiltinSetActive(rawSetActive as Record<string, unknown>)
        const themes = normalizeCustomThemes(rawCustomThemes)
        const themeActive = normalizeCustomThemeActive(rawCustomThemeActive as Record<string, unknown>, themes)
        return composeHotwords([], setWords, setActive, themes, themeActive)
      }),
      getSetting('server.language', 'auto').then((lang) => {
        const l = lang as string
        return l && l !== 'auto' ? l : ''
      }),
    ])

    this.settings = {
      ...this.settings,
      hotwords: hotwordsResult.status === 'fulfilled' ? hotwordsResult.value : [],
      language: languageResult.status === 'fulfilled' ? languageResult.value : '',
    }
  }

  /** 採集一次客戶端執行時資訊並快取。靜態後設資料，程序內只成功採集一次；
   *  失敗不置位 loaded，留待下次重試（避免把 unknown/0 永久快取）。
   *  clientMeta 本就可空，採集期間為 null 不影響業務。 */
  async ensureClientRuntimeInfo() {
    if (this.clientRuntimeInfoLoaded) return
    try {
      this.clientRuntimeInfo = await bridge.getClientRuntimeInfo()
      this.clientRuntimeInfoLoaded = true
    } catch {
      this.clientRuntimeInfo = null
    }
  }

  /** 工作模式或服務地址變更後強制重連 provider。
   *  先斷開舊連線再按新配置連線，確保連線狀態與新地址一致（否則改錯地址後仍顯示"已連線"）。
   *  錄音進行中不強制斷開，避免打斷當前會話。 */
  reconnectProvider() {
    if (this.state === 'idle') {
      this.provider.disconnect()
    }
    this.ensureConnection()
  }

  /** 僅重新整理 overlay 顯示設定（主題/長度/時長），不觸碰錄音相關快取。
   *  用於外觀設定頁面，避免改個顏色就觸發十幾個 IPC 呼叫。 */
  async refreshOverlaySettings() {
    await this.overlayService.refreshSettings()
  }

  // ── State machine ──

  private transition(to: RecorderState): boolean {
    const pair = [this.state, to] as [RecorderState, RecorderState]
    const valid = VALID_TRANSITIONS.some(([f, t]) => f === pair[0] && t === pair[1])
    if (!valid) {
      addRuntimeEvent('warn', 'recorder', `Ignored invalid state transition ${this.state} → ${to}`)
      return false
    }
    addRuntimeEvent('info', 'recorder', 'State changed', { from: this.state, to })
    this.state = to
    this.onStateChange?.(to)
    return true
  }

  private clearProcessingTimeout() {
    if (this.processingTimeoutId) {
      clearTimeout(this.processingTimeoutId)
      this.processingTimeoutId = null
    }
  }

  private clearInsertionTimeout() {
    if (this.insertionTimeoutId) {
      clearTimeout(this.insertionTimeoutId)
      this.insertionTimeoutId = null
    }
  }

  /**
   * 插入階段必須有自己的超時。
   *
   * 為什麼不能靠處理超時那一個：`onFinal` 收到結果時第一件事就是
   * `clearProcessingTimeout()`，而插入發生在那之後。定時器已經沒了，
   * `onProcessingTimeout` 裡那條"插入還在飛"的分支於是永遠不會被觸發 —— 貼上真卡住
   * 的時候（Rust SendInput 停在目標程序上）沒有任何人來收尾，介面停在「處理中」。
   *
   * 時長取 15 秒：Rust 側最壞是兩次 SendMessageTimeoutW（各 2s）加上幾次 sleep，
   * 正常路徑在幾百毫秒內結束。到 15 秒還沒回來就是真卡死了。
   */
  private armInsertionTimeout(runId: number, text: string) {
    this.clearInsertionTimeout()
    this.insertionTimeoutId = setTimeout(() => {
      this.insertionTimeoutId = null
      this.handleInsertionStuck(runId, text, INSERTION_TIMEOUT_MS)
    }, INSERTION_TIMEOUT_MS)
  }

  /**
   * 插入卡死的唯一收尾實現：把文字交還給使用者，而不是讓介面憑空恢復空閒。
   *
   * 兩個定時器都可能走到這裡（插入專屬超時、以及處理超時那條兜底），所以必須冪等 ——
   * `textInsertionInFlight` 就是那把鎖。
   *
   * 文案刻意是「無法確認是否已寫入」而不是「插入失敗」：SendInput 是發出即忘，卡在
   * 這裡時目標程式**可能已經收到**那段文字（pitfalls 28：UIPI 攔截是靜默的，成功與
   * 失敗在這一側長得一樣）。說"失敗了"會讓使用者再粘一遍、多出一份重複。
   */
  private handleInsertionStuck(runId: number, text: string, waitedMs: number) {
    if (!this.textInsertionInFlight) return
    if (!this.isRunCurrent(runId)) return
    addRuntimeEvent('error', 'recorder', 'Text insertion never finished; handing the text back to the user', {
      runId,
      waitedMs,
      textLen: text.length,
    })
    this.textInsertionInFlight = false
    this.clearInsertionTimeout()
    this.clearProcessingTimeout()
    if (text.trim()) {
      this.showFallbackAndReset(text, 'insertion_timeout', runId)
      return
    }
    this.finishRun(runId)
    this.resetToIdle()
  }




  private getLiveElapsedSec() {
    if (this.state === 'recording' && this.run.recordStartPerf > 0) {
      return elapsedSecFromPerf(this.run.recordStartPerf)
    }
    return this.getAudioDurationSec()
  }

  private getAudioDurationSec() {
    return this.run.audioSentSamples > 0 ? this.run.audioSentSamples / 16000 : 0
  }

  private resetToIdle(options?: ResetToIdleOptions) {
    addRuntimeEvent('info', 'recorder', 'Reset to idle', {
      fromState: this.state,
      keepOverlay: Boolean(options?.keepOverlay),
      handsFreeMode: this.handsFreeMode,
      textInsertionInFlight: this.textInsertionInFlight,
    })
    this.clearProcessingTimeout()
    this.clearInsertionTimeout()
    this.micLevel.clearAutoCancelTimer()
    this.micLevel.invalidateProbes()
    if (this.handsFreeAutoStopId) {
      clearTimeout(this.handsFreeAutoStopId)
      this.handsFreeAutoStopId = null
    }
    this.overlayService.stopListeningTicker()
    this.overlayService.resetWarnings()
    this.overlayService.resetStreamingText()
    // 安全網：若仍處於我們施加的系統靜音中，確保恢復
    this.restoreSystemMuteIfNeeded()
    this.startRecordingLock = false
    this.run.pendingStopWhileStarting = false
    this.handsFreeMode = false
    this.run.finalHandledInCurrentRun = false
    this.textInsertionInFlight = false
    this.run.processingCancelable = false
    this.run.captureReadyPromise = null
    this.run.finalReceivedAt = 0
    this.history.clearPending()
    this.run.currentActiveAppContext = null
    this.run.currentPromptResolution = null
    this.run.cachedProbeResult = null
    this.micLevel.resetOnIdle()
    clearCapturedInsertionTarget()
    this.run.recordStartPerf = 0
    if (!options?.preserveLateFinalContext) {
      this.releaseRecordingFocus(this.activeRunId)
      this.timedOutProcessingContext = null
    }
    this.transition('idle')
    if (!options?.keepOverlay) {
      this.overlayService.clearFallbackHideTimer()
      this.overlayService.hide()
    }
  }


  private async cancelRecording(
    token: number,
    options: { showCanceled?: boolean; reason?: 'user' | 'mic_muted_timeout' } = {},
  ) {
    const showCanceled = options.showCanceled ?? true
    const reason = options.reason ?? 'user'
    if (
      this.state !== 'recording'
      || token === 0
      || token !== this.activeRunId
    ) {
      addRuntimeEvent('info', 'recorder', 'Ignored recording cancellation', {
        state: this.state,
        token,
        activeRunId: this.activeRunId,
        reason,
      })
      return
    }

    const canceledRunId = this.activeRunId
    // 在任何 await 之前離開 recording 並讓 run 失效。這樣同時到達的 PTT up 不會
    // 再進入正常 stopRecording，Provider 的遲到 partial/error 也會被 run guard 丟棄。
    if (!this.transition('processing')) return
    this.history.markRunCanceled(canceledRunId)
    this.run.finalHandledInCurrentRun = true
    this.run.processingCancelable = false
    this.overlayService.stopListeningTicker()
    this.restoreSystemMuteIfNeeded()
    this.run.recordedChunks = []
    this.finishRun(canceledRunId)
    this.provider.cancel()

    addRuntimeEvent('info', 'recorder', 'Recording canceled', {
      runId: canceledRunId,
      mode: this.provider.mode,
      reason,
    })
    if (showCanceled) this.overlayService.showCanceled()

    try {
      await stopCapture()
    } catch (error) {
      addRuntimeEvent('warn', 'recorder', 'Failed to stop capture while canceling', { error: String(error) })
    } finally {
      // state 在停止採集期間保持 processing，阻止新錄音複用同一個 capture；確認舊
      // capture 已結束後再回 idle。keepOverlay 保留短暫的“已取消”反饋。
      if (this.getState() === 'processing' && this.activeRunId === 0) {
        this.resetToIdle({ keepOverlay: showCanceled })
      }
      if (this.provider.mode === 'server') this.ensureConnection()
    }
  }

  private cancelProcessing(token: number) {
    if (
      this.state !== 'processing'
      || token === 0
      || token !== this.activeRunId
      || !this.run.processingCancelable
    ) {
      addRuntimeEvent('info', 'recorder', 'Ignored Esc processing cancellation', {
        state: this.state,
        token,
        activeRunId: this.activeRunId,
        processingCancelable: this.run.processingCancelable,
      })
      return
    }

    const canceledRunId = this.activeRunId
    const historyArtifact = this.history.getPending()
    this.clearProcessingTimeout()
    // 使用者明確按了 Esc：這一代的音訊存檔和歷史都該消失。這是唯一允許丟棄錄音的入口。
    this.history.markRunCanceled(canceledRunId)
    this.audioArchive.discard(canceledRunId)
    if (this.timedOutProcessingContext?.runId === canceledRunId) {
      this.timedOutProcessingContext.settled = true
    }
    this.timedOutProcessingContext = null
    this.run.processingCancelable = false
    this.provider.cancel()
    this.run.recordedChunks = []
    this.finishRun(canceledRunId)
    if (historyArtifact?.runId === canceledRunId) {
      void this.history.discard(historyArtifact)
    }

    // token 與當前 run 在同一同步檢查內通過後才允許提示“已取消”；舊佇列動作不會影響新代次。
    addRuntimeEvent('info', 'recorder', 'Processing canceled by user', {
      runId: canceledRunId,
      mode: this.provider.mode,
    })
    this.overlayService.showCanceled()
    this.resetToIdle({ keepOverlay: true })

    // ServerProvider.cancel 會主動斷開舊 socket，隔離可能遲到的服務端結果。
    if (this.provider.mode === 'server') this.ensureConnection()
  }

  // ── Provider callbacks ──

  private buildProviderCallbacks(): TranscriptionCallbacks {
    return {
      onPartialASR: (text) => {
        // 僅在錄音進行中把流式中間結果推給懸浮窗；下一次 listening 心跳會帶上它一起渲染
        if (this.state !== 'recording') return
        this.overlayService.setStreamingText(text)
      },

      onASR: (result) => {
        if (this.state !== 'processing') return
        if (this.run.finalHandledInCurrentRun) return
        if (result.text && result.text.trim() !== '') return

        const runId = this.activeRunId
        if (!this.isRunCurrent(runId)) return
        // 空 ASR 已是本代終態：同步阻止 timeout 與後續 final 再次收尾。
        this.run.finalHandledInCurrentRun = true
        this.clearProcessingTimeout()

        const audioDur = this.getAudioDurationSec()
        const wallSec = this.run.wallTimeAtStopSec > 0 ? this.run.wallTimeAtStopSec : audioDur
        const audioChunkCount = this.run.recordedChunks.length
        const historyMeta = this.buildHistoryMetadata(
          this.run.currentPromptResolution,
          this.run.currentActiveAppContext,
        )
        // ASR 跑完了，只是一個字都沒出。這**不等於**使用者沒說話 —— 熱詞回顯被判定
        // 清空、服務端提前收尾都會走到這裡（pitfalls 15）。所以分兩條說：
        // 採集側有近靜音實證的，才敢說「未檢測到有效聲音」；沒有實證的只說
        // 「沒有取得識別結果」，並給出可恢復的去處，別把人引去查麥克風。
        const silenceProven = this.micLevel.hasSilenceEvidence()
        const silenceDiagnostic = {
          reason: 'asr_empty',
          mode: this.provider.mode,
          audioSec: Number(audioDur.toFixed(1)),
          asrMs: result.asrMs || 0,
          audioChunks: audioChunkCount,
          silenceProven,
          peakAmplitude: this.micLevel.roundedPeakAmplitude(),
          runId,
        }
        // 識別沒出文字一律只給 toast，不彈卡片。
        //
        // ⚠️ **同一個判據在 onFinal 的空結果分支裡還有一份**（搜 showNoSpeech）。
        // 2026-09-23 實測代價：只把那一份改成 toast，這條路照舊彈卡片，使用者原樣復現。
        // 改任何一處都要同時改另一處；noTextNeverShowsCard 那條原始碼級斷言釘著這件事。
        this.overlayService.showNoSpeech(silenceProven ? 'silent' : 'no_text', silenceDiagnostic)
        if (!silenceProven) {
          addRuntimeEvent('warn', 'recorder', 'ASR returned no text and there is no silence evidence', silenceDiagnostic)
        }
        void (async () => {
          // 兩條路的存檔引數本來就完全一樣（同一個 failReasonCode），合併掉那份
          // 重複的 if/else —— 它正是上面那個漏改能發生的土壤。
          await this.history.writeFailedRun({
            runId,
            audioDurationSec: audioDur,
            wallTimeSec: wallSec,
            asrMs: result.asrMs || 0,
            asrDurationSec: result.durationSec > 0 ? result.durationSec : undefined,
            failReason: t('recorder.noTranscript'),
            failReasonCode: 'no_transcript',
            historyMeta,
            aiSource: result.aiSource,
            aiStatus: result.aiStatus,
          })

          if (!this.isRunCurrent(runId)) return
          this.finishRun(runId)
          this.resetToIdle({ keepOverlay: true })
        })()
      },

      onFinal: (result) => {
        if (this.state !== 'processing') {
          const lateContext = this.consumeTimedOutProcessingContext()
          if (!lateContext) return

          addRuntimeEvent('warn', 'recorder', 'Received a late final result for a timed-out session', {
            timedOutAt: lateContext.timedOutAt,
            lateByMs: Date.now() - lateContext.timedOutAt,
            durationSec: result.durationSec,
            asrMs: result.asrMs,
            llmMs: result.llmMs,
          })
          // final 已經快照到前端，立即廢棄舊 Provider 會話；尤其 Server 必須斷開舊 socket，
          // 防止隨後遲到的 done/error 在下一代 start 後誤清新會話。
          this.provider.cancel()
          if (this.provider.mode === 'server') this.ensureConnection()
          this.finalizingLateRunId = lateContext.runId
          // 使用者已經放棄等待（寬限期裡按了 Esc，或關掉了失敗卡）：結果照樣落歷史，
          // 但**絕不自動插字**。介面上剛說完"不等了"，幾秒後文字自己出現在輸入框裡，
          // 是最讓人措手不及的一種行為。
          const abandoned = this.lateResultAbandoned
          if (abandoned) {
            addRuntimeEvent('warn', 'recorder', 'Late final arrived after the user abandoned it; saving without inserting', {
              runId: lateContext.runId,
              lateByMs: Date.now() - lateContext.timedOutAt,
            })
          }
          void this.processFinalResult(result, lateContext, {
            allowInsertionWhenIdle: true,
            source: 'late_after_timeout',
          }).finally(() => {
            if (this.finalizingLateRunId === lateContext.runId) this.finalizingLateRunId = 0
            // 這一代徹底收尾了：再晚到的 Esc 不該還能改它的行為。
            if (this.lateResultRunId === lateContext.runId) {
              this.lateResultRunId = 0
              this.lateResultAbandoned = false
            }
          })
          return
        }
        if (this.run.finalHandledInCurrentRun) {
          addRuntimeEvent('warn', 'recorder', 'Ignored duplicate final result')
          return
        }
        this.run.finalHandledInCurrentRun = true
        // 首個 processing final 在同步回撥路徑立即撤銷 timeout，避免二者雙收尾。
        this.clearProcessingTimeout()
        this.run.finalReceivedAt = Date.now()

        const localAudioDur = this.getAudioDurationSec()
        console.log('[ptt-diag] onFinal', {
          backendDurationSec: result.durationSec,
          localAudioDurSec: localAudioDur.toFixed(2),
          audioSentSamples: this.run.audioSentSamples,
          asrMs: result.asrMs,
          llmMs: result.llmMs,
        })

        void this.processFinalResult(result, {
          runId: this.activeRunId,
          timedOutAt: 0,
          settled: true,
          audioDurationSec: this.getAudioDurationSec(),
          wallTimeSec: this.run.wallTimeAtStopSec > 0 ? this.run.wallTimeAtStopSec : this.getAudioDurationSec(),
          promptResolution: this.run.currentPromptResolution ? { ...this.run.currentPromptResolution } : null,
          appContext: this.run.currentActiveAppContext ? { ...this.run.currentActiveAppContext } : null,
          audioChunks: this.run.recordedChunks.slice(),
          probeResult: this.run.cachedProbeResult ? { ...this.run.cachedProbeResult } : null,
        }, {
          allowInsertionWhenIdle: false,
          source: 'processing',
        })
      },

      onDone: () => {
        if (this.state !== 'processing') return
        if (this.run.finalHandledInCurrentRun) return
        if (this.textInsertionInFlight) return
        const runId = this.activeRunId
        this.clearProcessingTimeout()

        // Provider 說本輪結束了，卻一個結果都沒給（協議異常：done 先於 final 到達、
        // 或服務端只發了 done）。音訊已經存檔，別讓它變成沒人認領的孤兒檔案 ——
        // 補一條帶原因的記錄，使用者至少還能回放和重新識別。
        if (this.audioArchive.has(runId)) {
          const audioDur = this.getAudioDurationSec()
          const wallSec = this.run.wallTimeAtStopSec > 0 ? this.run.wallTimeAtStopSec : audioDur
          const historyMeta = this.buildHistoryMetadata(
            this.run.currentPromptResolution,
            this.run.currentActiveAppContext,
          )
          addRuntimeEvent('warn', 'recorder', 'Provider finished without any result; saving audio to history', {
            runId,
            mode: this.provider.mode,
            audioSec: audioDur,
          })
          // 協議沒跑完（done 先於 final、或服務端只發了 done）。以前這裡顯示的是
          // 「未檢測到有效聲音」—— 歷史裡記的卻是 provider_failed，兩邊對不上，
          // 而使用者看到的那句話會把他引去查麥克風。這是呼叫失敗，不是沒說話。
          // 這條路**保留卡片**：它不是「沒識別出文字」，是協議沒跑完 —— 服務端結束了
          // 會話卻什麼都沒回，屬於確實出了錯。文案也從 noResultTitle 換成專屬的一條：
          // 兩種成因共用一個標題時，日誌裡根本分不清彈的是哪一張卡（查這個 bug 時
          // 就被它誤導過一次）。
          void (async () => {
            await this.failRunWithCard(runId, {
              title: t('recorder.protocolIncompleteTitle'),
              detail: t('recorder.protocolIncompleteDetail'),
              audioDurationSec: audioDur,
              wallTimeSec: wallSec,
              failReason: t('record.providerFailed'),
              failReasonCode: 'provider_failed',
              historyMeta,
            })
            this.finishRun(runId)
          })()
          this.resetToIdle({ keepOverlay: true })
          return
        }

        this.finishRun(runId)
        this.resetToIdle()
      },

      onError: (msg) => {
        const runId = this.activeRunId
        if (!this.isRunCurrent(runId) || (this.state !== 'recording' && this.state !== 'processing')) {
          addRuntimeEvent('warn', 'backend', 'Ignored error callback from a stale session', { msg, state: this.state, runId })
          return
        }
        const friendlyFailure = describeProviderError(msg)
        addRuntimeEvent('error', 'backend', msg)
        this.clearProcessingTimeout()
        this.run.processingCancelable = false
        void this.overlayService.disableEscapeAction()
        this.run.finalHandledInCurrentRun = true

        // 停止音訊採集，並進入不可再觸發 stopRecording 的收尾狀態。
        const failedWhileRecording = this.state === 'recording'
        if (failedWhileRecording) {
          this.overlayService.stopListeningTicker()
          void stopCapture().catch(() => { })
          this.restoreSystemMuteIfNeeded()
          this.transition('processing')
        }

        const audioDur = this.getAudioDurationSec()
        const wallSec = this.run.wallTimeAtStopSec > 0 ? this.run.wallTimeAtStopSec : audioDur
        const historyMeta = this.buildHistoryMetadata(
          this.run.currentPromptResolution,
          this.run.currentActiveAppContext,
        )
        // 錯誤在錄音進行中到達時還沒走過 stopRecording，音訊尚未開始落盤 —— 在這裡補上，
        // 否則「說到一半連線斷了」這段語音誰都沒儲存。
        if (audioDur >= 0.5 && this.run.recordedChunks.length > 0) {
          this.audioArchive.ensure(runId, this.run.recordedChunks.slice())
        }
        void (async () => {
          if (audioDur >= 0.5) {
            // 供應商/後端給的原話（額度、資源未開通、連線被斷都在這裡）。
            //
            // ⚠️ 這裡以前按 failedWhileRecording 分叉：只有**錄音中**到達的錯誤才提示，
            // 在「處理中」到達就一句話不說、懸浮窗憑空消失（resetToIdle 不帶 keepOverlay
            // 就會走到 overlayService.hide()）。而介面呼叫失敗恰恰基本都發生在處理中 ——
            // 這就是使用者報的「識別失敗沒有任何提示」。兩條路現在都顯示失敗卡。
            await this.failRunWithCard(runId, {
              title: t('recorder.recognitionFailedTitle'),
              detail: friendlyFailure.message,
              audioDurationSec: audioDur,
              wallTimeSec: wallSec,
              failReason: friendlyFailure.detail,
              failReasonCode: classifyHistoryProviderFailure(msg),
              historyMeta,
            })
          } else if (this.isRunCurrent(runId)) {
            // 不足 0.5 秒：沒有音訊可存，也沒什麼可恢復的，只給一句短提示。
            this.overlayService.showError(friendlyFailure.message)
          }

          if (!this.isRunCurrent(runId)) return
          this.finishRun(runId)
          this.resetToIdle({ keepOverlay: true })
        })()
      },
    }
  }

  // ── Text insertion: uses pre-probed editable result ──

  private async handleTextInsertion(
    text: string,
    options: { allowWhenIdle?: boolean; runId: number; probeResult?: ProbeResult | null },
  ) {
    const { runId } = options
    const allowWhenIdle = options.allowWhenIdle === true
    if (!this.isRunCurrent(runId)) return

    // Guard: if we're no longer in processing (e.g. timeout fired), bail out unless this
    // is the explicitly retained late-final path.
    if (this.state !== 'processing' && !allowWhenIdle) {
      addRuntimeEvent('warn', 'recorder', 'Skipped text insertion because state is no longer processing', { state: this.state, runId })
      return
    }

    // 使用者放棄等待遲到結果 —— 這一刀必須紮在**真正插字之前**。
    //
    // 只在 onFinal 那一刻判一次是不夠的：結果到達之後還要落歷史，那段時間裡懸浮窗
    // 仍顯示"仍在等結果"，使用者正是在那時按 Esc 的。判早了，他的按鍵全都白按，文字
    // 照樣出現在輸入框裡。
    if (runId === this.lateResultRunId && this.lateResultAbandoned) {
      addRuntimeEvent('warn', 'recorder', 'Skipped text insertion because the user abandoned this late result', {
        runId,
        textLen: text.length,
      })
      this.overlayService.hide()
      return
    }

    // Cancel the processing timeout — we're handling the result now.
    if (this.state === 'processing') {
      this.clearProcessingTimeout()
    }

    const outcome = await this.insertion.insert(text, {
      runId,
      probeResult: options.probeResult ?? null,
      finalReceivedAt: this.run.finalReceivedAt,
      protectClipboard: this.settings.protectClipboard,
      isCurrent: () => this.isRunCurrent(runId),
      waitForModifierRelease: () => this.waitForModifierPTTReleaseIfNeeded(),
      // pasteText 一旦進入 Rust SendInput 就無法撤銷。先關閉本地取消屏障，再確認原生
      // Esc 模式已關閉，最後驗代；已排隊的舊 cancel 因 processingCancelable=false 只能被忽略。
      beforeIrreversiblePaste: async () => {
        this.run.processingCancelable = false
        await this.overlayService.disableEscapeAction()
      },
    })
    switch (outcome.kind) {
      case 'stale':
        return
      case 'fallback':
        this.showFallbackAndReset(text, outcome.reason, runId)
        return
      case 'inserted':
        this.finishRun(runId)
        if (this.state === 'processing') {
          this.resetToIdle()
        } else {
          // 遲到結果那條路進來時狀態已經是 idle，resetToIdle 不會執行 —— 而"仍在等結果"
          // 那條提示還掛在螢幕上。文字都插進去了，提示必須跟著收起來。
          this.overlayService.hide()
        }
        return
    }
  }

  /**
   * Show fallback card and transition to idle.
   * Ensures overlay layout is switched to fallback BEFORE hiding other states.
   */
  private showFallbackAndReset(text: string, reason: string, runId: number) {
    if (!this.isRunCurrent(runId)) return
    addRuntimeEvent('info', 'recorder', 'Showing fallback card', {
      runId,
      reason,
      textLen: text.length,
      stateBeforeReset: this.state,
    })
    // 先發布帶本代 token 的 fallback，再結束 run；後續舊 dismiss 只能匹配這張卡片。
    this.activeFallbackToken = runId
    this.overlayService.showFallback(text, reason, runId)
    this.finishRun(runId)
    // Then: transition to idle but keep overlay visible
    if (this.state === 'processing') {
      this.resetToIdle({ keepOverlay: true })
    }
  }

  // ── Connection management ──

  private ensureConnection() {
    if (this.provider.isReady()) return
    this.provider.connect(this.buildProviderCallbacks()).catch((err) => {
      // 伺服器模式由 websocket.ts 的 onclose 按指數退避自動重連（3s→30s）。這裡再每 5 秒
      // 補一次，斷網時（拒連秒失敗）日誌會刷屏；主動斷開（cleanup/取消）後也會被它拉回來。
      if (this.provider.mode === 'server') {
        addRuntimeEvent('warn', 'websocket', 'Preconnection failed; websocket auto-reconnect will retry', { error: String(err) })
        return
      }
      addRuntimeEvent('warn', 'websocket', 'Preconnection failed; retrying in 5s', { error: String(err) })
      setTimeout(() => this.ensureConnection(), 5000)
    })
  }

  // ── Recording lifecycle ──

  /**
   * 開始錄音的外層兜底。
   *
   * startRecordingInner 自己的 try 只包住「建連 + 開麥」那一段；在它之前還有取上下文、
   * 提示詞路由等十幾步，都發生在 startRecordingLock 置位之後。那裡拋的異常以前沒人接
   * （呼叫方是 `void this.startRecording()`），鎖永遠不釋放，之後每次按熱鍵都被當成
   * 「正在啟動」忽略 —— 熱鍵全部失靈直到重啟，懸浮窗停在「等待中」，日誌裡一個字都沒有。
   */
  private async startRecording() {
    try {
      await this.startRecordingInner()
    } catch (error) {
      const runId = this.activeRunId
      addRuntimeEvent('error', 'recorder', 'Failed to start recording', {
        error: String(error),
        stage: 'setup',
        runId,
      })
      this.startRecordingLock = false
      this.run.pendingStopWhileStarting = false
      this.restoreSystemMuteIfNeeded()
      if (runId !== 0) {
        this.finishRun(runId)
        this.provider.cancel()
      }
      this.overlayService.showError(t('recorder.startFailed'))
      this.resetToIdle({ keepOverlay: true })
    }
  }

  private async startRecordingInner() {
    if (
      this.state !== 'idle'
      || this.startRecordingLock
      || this.textInsertionInFlight
      || this.finalizingLateRunId !== 0
    ) {
      addRuntimeEvent('info', 'recorder', 'Ignored start-recording request', {
        state: this.state,
        locked: this.startRecordingLock,
        textInsertionInFlight: this.textInsertionInFlight,
        finalizingLateRunId: this.finalizingLateRunId,
      })
      return
    }

    // 未就緒（如 server 模式後端未連線）：給出告警，不進入錄音，避免懸浮窗卡住關不掉
    if (!this.provider.isReady()) {
      this.handsFreeMode = false
      addRuntimeEvent('warn', 'recorder', 'Provider not ready; ignored start-recording request', { mode: this.provider.mode })
      this.overlayService.showError(this.notReadyMessage())
      this.ensureConnection()
      return
    }

    // 新 run 建立前先廢棄 timeout 寬限期的舊 provider 代次；後續原有 connect 流程
    // 會為 ServerProvider 建立新 socket，Cloud/Local 也會因 cancel 丟棄舊回撥。
    const timedOutContext = this.timedOutProcessingContext
    if (timedOutContext) {
      this.timedOutProcessingContext = null
      this.provider.cancel()
      this.finishRun(timedOutContext.runId)
    }

    if (this.recordingFocus) this.releaseRecordingFocus(this.recordingFocus.runId)
    const runId = ++this.runSequence
    this.activeRunId = runId
    this.activeFallbackToken = 0
    this.startRecordingLock = true
    this.run.pendingStopWhileStarting = false
    this.timedOutProcessingContext = null
    this.micLevel.resetMuteState()

    // 先給使用者即時視覺反饋；overlay 不聚焦，因此後續原生上下文捕獲仍指向原目標視窗。
    // 首次 WebView 已在啟動空閒期預熱，正常情況下這裡只剩一次輕量 show + emit。
    this.overlayService.showWaiting()
    // 不等待上下文捕獲、Provider 建連或 AudioWorklet 初始化：提前拿到系統靜音標誌，
    // 好讓第一幀 PCM 一到就能立刻裁決（標誌本身不足以判定，見 pendingOsMicMuted）。
    void this.micLevel.checkConfiguredMicMuted(runId)

    const targetCapture = captureActiveInsertionTarget(undefined, {
      preserveExistingOnFailure: true,
    })
    let activeAppContext: ActiveAppContext | null = null
    let recordingContextFailed = false
    try {
      // AI is the only consumer of editor text. If cleanup is off, do not read the text even when
      // the preference remains enabled, so the privacy boundary matches actual behavior.
      const includeTextContext = this.settings.contextAwareWriting && this.settings.aiEnabled
      const recordingContext = await bridge.getRecordingContext(includeTextContext)
      const probe = recordingContext.probe as unknown as ProbeResult
      if (probe.focusToken && !this.isRunCurrent(runId)) {
        void bridge.releaseRecordingFocus(probe.focusToken).catch(() => {})
        return
      }
      activeAppContext = recordingContext.appContext as unknown as ActiveAppContext
      this.run.cachedProbeResult = probe
      if (probe.focusToken) this.recordingFocus = { runId, token: probe.focusToken }
      addRuntimeEvent('info', 'recorder', 'Insertion probe cached at recording start', {
        probeId: this.run.cachedProbeResult.probeId,
        hwnd: this.run.cachedProbeResult.hwnd,
        focusHwnd: this.run.cachedProbeResult.focusHwnd,
        editable: this.run.cachedProbeResult.editable,
        process: this.run.cachedProbeResult.process,
        verdict: this.run.cachedProbeResult.verdict,
      })
    } catch {
      activeAppContext = null
      this.run.cachedProbeResult = null
      recordingContextFailed = true
    }
    const textContext = usableTextContext(activeAppContext?.textContext)
    // 必須在下面 delete 之前算：選區太長的判據就在原始的 textContext 上
    const textContextLog = describeTextContextForLog({
      featureOn: this.settings.contextAwareWriting,
      aiOn: this.settings.aiEnabled,
      captureFailed: recordingContextFailed,
      diag: activeAppContext?.textContextDiag,
      raw: activeAppContext?.textContext,
      usable: textContext,
    })
    if (activeAppContext) {
      if (textContext) activeAppContext.textContext = textContext
      else delete activeAppContext.textContext
    }
    this.run.currentActiveAppContext = activeAppContext

    this.run.currentPromptResolution = resolvePromptRouting({
      appContext: activeAppContext,
      presets: this.settings.presets,
      activePresetId: this.settings.activePresetId,
      appRules: this.settings.appPromptRules,
      userStats: this.userStats,
      hotwords: this.settings.hotwords,
      injectHotwords: this.settings.injectHotwords,
    })
    if (textContext) {
      this.run.currentPromptResolution = {
        ...this.run.currentPromptResolution,
        systemPrompt: withContextAwareInstructions(
          this.run.currentPromptResolution.systemPrompt,
          textContext,
          this.settings.contextSelectionEditPrompt,
        ),
        summary: `${this.run.currentPromptResolution.summary} | Text context: ${textContext.selectedText ? 'selection' : 'caret'}`,
      }
    }
    // 原來這裡另發一條 'Text context captured'，但它不在落盤白名單裡，sayit.log 裡從來看不到；
    // 而且只在讀到時才有。現在讀沒讀到都並進下面那條一定落盤的 'Recording started'。

    addRuntimeEvent('info', 'recorder', 'Recording started', {
      micId: this.settings.micId || 'default',
      preset: this.run.currentPromptResolution.preset.id || this.run.currentPromptResolution.preset.name || 'none',
      targetCapture,
      appContext: this.summarizeAppContext(activeAppContext || null),
      promptRouting: {
        appId: this.run.currentPromptResolution.appId,
        appName: this.run.currentPromptResolution.appName,
        presetId: this.run.currentPromptResolution.preset.id,
        presetName: this.run.currentPromptResolution.preset.name,
        promptRuleId: this.run.currentPromptResolution.matchedRule?.id,
        summary: this.run.currentPromptResolution.summary,
      },
      // 只有原因碼和字數，見 describeTextContextForLog。查「上下文感知沒生效」grep 這個欄位。
      ...(textContextLog && { textContext: textContextLog }),
    })
    addRuntimeEvent('info', 'personalization', 'Prompt routing resolved', {
      appContext: this.summarizeAppContext(activeAppContext || null),
      appId: this.run.currentPromptResolution.appId,
      appName: this.run.currentPromptResolution.appName,
      presetId: this.run.currentPromptResolution.preset.id,
      presetName: this.run.currentPromptResolution.preset.name,
      promptRuleId: this.run.currentPromptResolution.matchedRule?.id,
      summary: this.run.currentPromptResolution.summary,
    })

    this.run.finalHandledInCurrentRun = false
    // 只在開始新一次錄音時清：resetToIdle 也會在超時路徑上跑，那時寬限期才剛開始，
    // 清掉它就等於把使用者的"我不等了"忘掉。
    this.lateResultAbandoned = false
    this.lateResultRunId = 0
    this.run.audioSentSamples = 0
    this.run.wallTimeAtStopSec = 0
    this.run.recordedChunks = []
    this.micLevel.resetLevels()
    resetWaveformBarState(this.overlayWaveState, this.overlayService.getBarCount(), 3)

    // Hands-free mode: arm a 5-minute auto-stop timer
    // is handled by the Rust keyboard hook's hard_timeout_release)
    const armHandsFreeTimer = () => {
      if (!this.handsFreeMode) return
      this.handsFreeAutoStopId = setTimeout(() => {
        if (this.state === 'recording' && this.handsFreeMode) {
          addRuntimeEvent('warn', 'recorder', 'Hands-free recording is approaching the five-minute limit')
          this.overlayService.showTimeoutWarning()
          // Auto-stop after 1 more minute
          this.handsFreeAutoStopId = setTimeout(() => {
            if (this.state === 'recording' && this.handsFreeMode) {
              addRuntimeEvent('warn', 'recorder', 'Hands-free recording reached five minutes and stopped automatically')
              void this.stopRecording()
            }
          }, RECORDING_COUNTDOWN_SEC * 1000)
        }
        // 與上限統一由常量推導，避免"改了上限忘了改提醒時機"
      }, (MAX_RECORDING_SEC - RECORDING_COUNTDOWN_SEC) * 1000)
    }

    // 凍結本次的 AI 配置。理由是事後解釋：使用者錄完之後改了設定（這次排查裡他就把
    // 門檻從 60 改成了 0），這一條的原因也不能跟著變。
    this.run.currentAiConfig = {
      workMode: this.provider.mode,
      aiEnabled: this.settings.aiEnabled,
      aiMinDurationSec: this.settings.aiMinDurationSec,
      serverAiSource: getRuntimeServerAiSource(),
    }
    this.run.currentOperationId = `${runId}-${Date.now().toString(36)}`

    // 選區編輯單獨選了模型時，這次錄音改由本機用那個模型整理（見 selectionAi.ts）。
    // 讀設定放進下面和建連、開麥並行的那一組裡，不在這裡 await：這一段處在
    // startRecordingLock 置位之後、外層兜底之外，多一個 await 就多一處讓熱鍵卡死的
    // 可能，開始錄音也會慢一拍。沒有選區時它不讀設定，直接返回 null。
    const selectionAiConfigPromise = this.settings.aiEnabled
      ? loadSelectionAiConfig(textContext)
      : Promise.resolve(null)

    const promptOpts = this.run.currentPromptResolution
      ? {
        runId,
        operationId: this.run.currentOperationId,
        aiConfig: this.run.currentAiConfig,
        systemPrompt: this.settings.aiEnabled ? this.run.currentPromptResolution.systemPrompt : undefined,
        disableAi: !this.settings.aiEnabled,
        aiMinDurationSec: this.settings.aiMinDurationSec,
        clientMeta: this.clientRuntimeInfo,
        appContext: activeAppContext,
        textContext,
        hotwords: this.settings.hotwords.length > 0 ? this.settings.hotwords : undefined,
        language: this.settings.language || undefined,
        streamingDisplay: this.settings.streamingDisplay,
      }
      : {
        runId,
        operationId: this.run.currentOperationId,
        aiConfig: this.run.currentAiConfig,
        disableAi: !this.settings.aiEnabled,
        aiMinDurationSec: this.settings.aiMinDurationSec,
        clientMeta: this.clientRuntimeInfo,
        appContext: activeAppContext,
        textContext,
        hotwords: this.settings.hotwords.length > 0 ? this.settings.hotwords : undefined,
        language: this.settings.language || undefined,
        streamingDisplay: this.settings.streamingDisplay,
      }

    // Wrap the async setup so stopRecording can wait for it
    let resolveCaptureReady: () => void
    this.run.captureReadyPromise = new Promise<void>((resolve) => { resolveCaptureReady = resolve })

    try {

      const openCapture = (deviceId: string) => startCapture(
        deviceId || undefined,
        (buffer) => {
          if (!this.isRunCurrent(runId)) return
          if (this.run.audioSentSamples === 0) {
            console.log('[ptt-diag] first onData buffer', {
              byteLength: buffer.byteLength,
              samples: buffer.byteLength / 2,
            })
          }
          this.run.recordedChunks.push(buffer.slice(0))
          this.provider.sendAudio(buffer)
        },
        undefined,
        (pcmFrame) => {
          if (!this.isRunCurrent(runId)) return
          this.run.audioSentSamples += pcmFrame.length
          const bars = computeBarsFromPCM(pcmFrame, this.overlayWaveState, {
            barCount: this.overlayService.getBarCount(),
            minHeight: 3,
            maxHeight: 18,
          })
          this.overlayService.pushListeningBars(bars)

          this.micLevel.onFrame(pcmFrame)
        },
        this.settings.noiseSuppression,
      )

      // 先決定開啟哪個麥克風（系統預設被換成虛擬音效卡時改用真實麥克風，見 micRouting.ts），
      // 再開流。整段和 WebSocket 連線並行，不拖慢啟動。
      this.activeMic = null
      const captureTask = (async () => {
        const { plan, endpoints } = await resolveMicPlan(this.settings.micId)
        let finalPlan: MicPlan = plan
        let result = await openCapture(plan.deviceId)
        // 開流前網頁側的裝置名是空的（沒有持久麥克風許可權時會這樣），對不上號。
        // 現在流開著，名字拿得到了，再判一次；要換就重開一次流，此時還沒發 start，丟不了音訊。
        if (plan.reason === 'unmapped' && this.isRunCurrent(runId)) {
          const retry = await resolveMicPlan('', { endpoints, webDevices: result.devices })
          finalPlan = retry.plan
          if (retry.plan.reason === 'virtual_avoided') {
            // 虛擬音效卡那一小段（約百毫秒的全 0）不必清掉：服務端和本地存檔拿到的是同一份
            result = await openCapture(retry.plan.deviceId)
          }
        }
        return { result, plan: finalPlan, endpoints }
      })()

      // 並行執行 WebSocket 連線和麥克風採集，減少等待時間
      const [, captureTaskResult, selectionAiConfig] = await Promise.all([
        this.provider.connect(this.buildProviderCallbacks()),
        captureTask,
        selectionAiConfigPromise,
      ])
      const { result: captureResult, plan: micPlan, endpoints: micEndpoints } = captureTaskResult
      if (this.isRunCurrent(runId)) this.activeMic = this.describeActiveMic(captureResult, micEndpoints)
      resolveCaptureReady!()
      if (!this.isRunCurrent(runId)) {
        await stopCapture().catch(() => { })
        return
      }

      // 凍結的 AI 配置要帶上 selectionAiOverride：停止錄音時 aiPolicy 靠它判成本機整理，
      // 伺服器模式下才會讓服務端只做識別。
      let startOpts: typeof promptOpts & { selectionAiConfig?: typeof selectionAiConfig } = promptOpts
      if (selectionAiConfig && this.run.currentAiConfig) {
        this.run.currentAiConfig = { ...this.run.currentAiConfig, selectionAiOverride: true }
        startOpts = { ...promptOpts, aiConfig: this.run.currentAiConfig, selectionAiConfig }
        addRuntimeEvent('info', 'ai', 'Selection edit uses its own AI service', {
          provider: selectionAiConfig.provider,
          model: selectionAiConfig.model,
        })
      }

      // Both WebSocket and mic are ready — send start command
      const started = this.provider.start(startOpts)
      if (!started) {
        throw new Error('sendStart failed')
      }
      addRuntimeEvent('info', 'recorder', 'Start sent; audio capture beginning')

      // Audio capture is now active — transition to recording state and show audio bars
      this.run.recordStartPerf = performance.now()
      if (!this.transition('recording')) {
        this.startRecordingLock = false
        this.provider.cancel()
        this.finishRun(runId)
        this.resetToIdle()
        if (this.provider.mode === 'server') this.ensureConnection()
        return
      }
      this.startRecordingLock = false
      // 若本次會走流式即時顯示，錄音一開始就讓氣泡顯示佔位，避免中途彈出+縮放導致的抖動。
      // 即時讀取供應商/WorkspaceId（避免切換供應商後快取過期，導致非即時模型也彈氣泡）。
      void this.applyStreamingActive()
      this.overlayService.startListeningTicker(runId)
      if (micPlan.reason !== 'default' && micPlan.reason !== 'explicit') {
        addRuntimeEvent(micPlan.reason === 'virtual_avoided' ? 'info' : 'warn', 'recorder',
          micPlan.reason === 'virtual_avoided'
            ? 'System default microphone is a virtual device; using a real microphone instead'
            : 'System default microphone is a virtual device; no real microphone to switch to', {
          reason: micPlan.reason,
          avoided: micPlan.avoidedName,
          chosen: micPlan.chosenName,
          endpointCount: micEndpoints.length,
        })
      }
      const micSource = describeMicSource(
        captureResult,
        this.settings.micId,
        t('mic.title'),
        captureResult.devices,
      )
      const avoidedVirtual = micPlan.reason === 'virtual_avoided'
      // 跳過了虛擬音效卡要說出來，否則使用者會奇怪"系統預設明明是 A，怎麼錄的是 B"。
      // identity 帶上這個標記，同一個麥克風「直接用」和「跳過虛擬音效卡後用」各提示一次。
      const micSourceIdentity = avoidedVirtual ? `${micSource.identity}:avoided-virtual` : micSource.identity
      const micSourceLabel = avoidedVirtual
        ? t('mic.avoidedVirtualHint', { device: micSource.label })
        : micSource.label
      if (micSourceChanged(this.lastMicSourceIdentity, micSourceIdentity)) {
        this.lastMicSourceIdentity = micSourceIdentity
        this.overlayService.showMicSourceHint({ mode: micSource.mode, label: micSourceLabel })
        // rawLabel 與 endpointCount 是判斷"這行字為什麼顯示成這樣"的唯一依據：
        // 顯示名是由系統給的原始名 + 同一時刻有幾個端點共同決定的，缺了這兩個
        // 就只能靠猜（audio.ts 裡的 trackLabel 只有原始名，沒有快照規模）。
        addRuntimeEvent('info', 'recorder', 'Input source reminder shown', {
          mode: micSource.mode,
          label: micSource.label,
          rawLabel: captureResult.label,
          endpointCount: captureResult.devices.length,
        })
      }
      // 錄音一開始就查一次麥克風是否被系統靜音，被靜音則懸浮窗即時紅色高警（不阻塞錄音）
      // 用 getUserMedia 實際開啟的裝置再複核；若系統在初始化期間切換了預設麥克風，以這裡為準。
      void this.micLevel.checkMicMuted(
        captureResult.label || null,
        runId,
        this.micLevel.nextProbeSequence(),
      )
      // 就緒提示音已觸發，稍後再靜音系統輸出（避免把提示音一起靜掉）
      this.scheduleSystemMuteIfEnabled()
      armHandsFreeTimer()

      // Check if PTT up arrived while we were initializing
      if (this.run.pendingStopWhileStarting) {
        this.run.pendingStopWhileStarting = false
        addRuntimeEvent('info', 'recorder', 'PTT released during initialization; stopping immediately')
        void this.stopRecording()
        return
      }

    } catch (error) {
      resolveCaptureReady!()
      // 這一代已被取消/替換（Esc、切換模式）時，連線隨之被關掉也會走到這裡。
      // 鎖、懸浮窗、provider 都歸新的一代所有，按上面成功分支的 stale 處理只關採集，
      // 否則會把新一代重置掉、還彈一條「啟動失敗」。
      if (!this.isRunCurrent(runId)) {
        addRuntimeEvent('info', 'recorder', 'Start failed for a stale run; ignored', { runId, error: String(error) })
        await stopCapture().catch(() => { })
        return
      }
      this.startRecordingLock = false
      this.run.pendingStopWhileStarting = false
      // 錄音啟動失敗也要恢復系統輸出，避免系統一直靜音
      this.restoreSystemMuteIfNeeded()
      addRuntimeEvent('error', 'recorder', 'Failed to start recording', { error: String(error) })
      try { await stopCapture() } catch { /* ignore */ }
      this.finishRun(runId)
      this.provider.cancel()
      // 在懸浮窗顯示錯誤資訊，讓使用者知道發生了什麼
      const errMsg = String(error)
      if (errMsg.includes('\u9ea6\u514b\u98ce') || errMsg.includes('麥克風') || errMsg.includes('microphone') || errMsg.includes('audio')) { // i18n-allow: 匹配底層中文錯誤串
        this.overlayService.showError(t('recorder.microphoneUnavailable'))
      } else {
        this.overlayService.showError(t('recorder.startFailed'))
      }
      this.resetToIdle({ keepOverlay: true })
    }
  }



  private async stopRecording() {
    if (this.state !== 'recording') {
      addRuntimeEvent('info', 'recorder', 'Ignored stop-recording request', { state: this.state })
      return
    }

    const runId = this.activeRunId
    this.micLevel.clearAutoCancelTimer()

    // Wait for capture setup to complete (getUserMedia + AudioWorklet can take time)
    if (this.run.captureReadyPromise) {
      try {
        await Promise.race([
          this.run.captureReadyPromise,
          new Promise<void>((resolve) => setTimeout(resolve, 3000)), // 3s max wait
        ])
      } catch { /* ignore */ }
      this.run.captureReadyPromise = null
    }
    if (this.state !== 'recording' || !this.isRunCurrent(runId)) return

    this.overlayService.stopListeningTicker()
    addRuntimeEvent('info', 'recorder', 'Recording stopped')

    try { await stopCapture() } catch (error) {
      addRuntimeEvent('error', 'recorder', 'Failed to stop audio capture', { error: String(error) })
    }
    // stopCapture 等待期間使用者可能按 Esc 使 run 失效；此時絕不能繼續 provider.stop。
    if (this.state !== 'recording' || !this.isRunCurrent(runId)) return

    // 採集已停止，恢復系統輸出到靜音前的狀態
    this.restoreSystemMuteIfNeeded()

    const audioDur = this.getAudioDurationSec()
    const pttHoldMs = elapsedSecFromPerf(this.run.recordStartPerf) * 1000
    const wallTimeSec = pttHoldMs / 1000
    this.run.wallTimeAtStopSec = wallTimeSec
    console.log('[ptt-diag] stopRecording', {
      audioSentSamples: this.run.audioSentSamples,
      audioDurSec: audioDur.toFixed(2),
      wallTimeSec: wallTimeSec.toFixed(2),
      durationRatio: pttHoldMs > 0 ? (audioDur / wallTimeSec).toFixed(3) : 'N/A',
    })

    // 資料一致性檢查：PCM 時長應接近實際按住時長
    const durationRatio = wallTimeSec > 0 ? audioDur / wallTimeSec : 1
    if (wallTimeSec > 1 && (durationRatio > 2.0 || durationRatio < 0.3)) {
      addRuntimeEvent('warn', 'recorder', 'Unexpected audio byte count; sample rate may not match', {
        audioDurSec: audioDur.toFixed(2),
        wallTimeSec: wallTimeSec.toFixed(2),
        durationRatio: durationRatio.toFixed(3),
        audioSentSamples: this.run.audioSentSamples,
        recordedChunksCount: this.run.recordedChunks.length,
        recordedChunksTotalBytes: this.run.recordedChunks.reduce((s, c) => s + c.byteLength, 0),
      })
      // 不再丟棄音訊，保留以便使用者回放和重新識別
    }
    if (audioDur < 0.5) {
      addRuntimeEvent('info', 'recorder', 'Recording shorter than 0.5s; canceled provider and discarded audio')
      this.provider.cancel()
      this.finishRun(runId)
      this.resetToIdle()
      if (this.provider.mode === 'server') this.ensureConnection()
      return
    }

    // 短語音門檻只能等到這裡才判斷：時長要錄完才知道。audioDur 用的是邊發邊累計的
    // 已傳送取樣數（不是本地攢下來的音訊），所以不影響上傳，伺服器模式照舊流式。
    //
    // 本地/雲 API 模式的 AI 是 Provider 在 ASR 之後自己另發的一次請求，它們在那時用
    // 同一個時長自行判斷；這裡這份只給伺服器模式用 —— 服務端的 AI 緊跟 ASR 執行，
    // 客戶端插不進中間，只能把結論隨 stop 一起送過去。
    // 判據收口到 aiPolicy：此前這裡、clientAiPolish、History 的三條重跑路徑各判一遍，
    // 結論已經漂移（雲 API 重跑漏了門檻、ollama 判據兩處不一致）。
    // audioDur 用的是已傳送取樣數換算的實際 PCM 秒數，不四捨五入。
    const aiPolicy = resolveAiPolicy({
      ...(this.run.currentAiConfig ?? {
        workMode: this.provider.mode,
        aiEnabled: this.settings.aiEnabled,
        aiMinDurationSec: this.settings.aiMinDurationSec,
        serverAiSource: getRuntimeServerAiSource(),
      }),
      audioDurationSec: audioDur,
    })
    // 線上那個布林只表達「服務端要不要做整理」。自配 AI 路線也會是 false，
    // 但那不是跳過——客戶端接著會調。原因一律看 ai.outcome，別從這個布林反推。
    const skipAiForShortSpeech = aiPolicy.reason === 'duration_below_min'

    // 提出成區域性變數：下面的 'Entered processing' 也要帶上它。那條日誌會落盤，
    // 而 'Stop sent' 不會 —— peakAmplitude=0 是「麥克風一個位元組都沒收到」的鐵證，
    // 必須掛在一條真的進 sayit.log 的日誌上，否則等於沒記。
    const audioStats = this.micLevel.summary()

    const stopAccepted = this.provider.stop({
      pttHoldMs,
      disableAi: serverShouldPolish(aiPolicy) ? undefined : true,
      aiPolicy,
      audioStats,
    })
    addRuntimeEvent('info', 'recorder', 'Stop sent', {
      audioSec: audioDur,
      pttHoldMs: Math.round(pttHoldMs),
      aiMinDurationSec: this.settings.aiMinDurationSec || undefined,
      skipAiForShortSpeech: skipAiForShortSpeech || undefined,
      stopAccepted,
    })

    if (!this.transition('processing')) {
      this.finishRun(runId)
      this.resetToIdle()
      return
    }
    this.run.processingCancelable = true

    // 音訊先落盤，不等識別結果。此後無論超時、斷連、還是程序被殺，這段錄音都還在磁碟上。
    this.audioArchive.begin(runId, this.run.recordedChunks.slice())

    if (!stopAccepted) {
      // 請求根本沒送出去（server 模式連線已斷、或 provider 會話已失效）。以前這裡照樣
      // 進 45 秒超時等待 + 15 秒寬限：懸浮條轉整整一分鐘「處理中」，而根本沒有任何請求
      // 在飛。實測 2026-09-07 兩次長錄音都是這條路（日誌：Failed to send stop）。
      void this.failRunWithoutResult(runId, {
        audioDurationSec: audioDur,
        wallTimeSec,
        failReason: t('recorder.connectionLost'),
        failReasonCode: 'connection_lost',
        title: t('recorder.connectionLostTitle'),
        detail: t('recorder.connectionLostDetail'),
      })
      return
    }

    const processingTimeoutMs = this.computeProcessingTimeoutMs(audioDur)
    addRuntimeEvent('info', 'recorder', 'Entered processing', {
      audioSec: audioDur,
      timeoutMs: processingTimeoutMs,
      mode: this.provider.mode,
      // 這一段音訊到底有沒有聲音，只記統計量、不記內容。讀日誌的判據：
      // peakAmplitude=0 且 silenceRatio=1 → 麥克風送來的是純靜音，與 ASR/網路無關。
      audioStats,
    })
    this.overlayService.showThinking(audioDur, runId, { ai: aiPolicy.allowCall })
    let insertionExtensions = 0
    const onProcessingTimeout = () => {
      if (this.state !== 'processing' || !this.isRunCurrent(runId)) return
      if (this.textInsertionInFlight) {
        if (insertionExtensions < MAX_INSERTION_TIMEOUT_EXTENSIONS) {
          insertionExtensions++
          // 插入已經在飛，給它一點時間；但**必須重新排一次定時器**。以前這裡直接 return，
          // 於是插入一旦卡住（Rust SendInput 停在目標程序上）就再沒有人來收尾，
          // 懸浮條無限停在「處理中」。
          addRuntimeEvent('warn', 'recorder', 'Processing timed out while text insertion is active; extending wait', {
            runId,
            extension: insertionExtensions,
            extendByMs: INSERTION_TIMEOUT_EXTENSION_MS,
          })
          this.processingTimeoutId = setTimeout(onProcessingTimeout, INSERTION_TIMEOUT_EXTENSION_MS)
          return
        }
        // 順延用盡：插入大機率卡死了。收尾走 handleInsertionStuck —— 與插入階段專屬
        // 超時共用同一個實現（那個才是正常會觸發的那條路，因為 onFinal 早就把這個
        // 處理超時清掉了；這裡只是"萬一定時器還活著"的兜底）。兩條路都冪等。
        this.handleInsertionStuck(
          runId,
          this.textBeingInserted,
          processingTimeoutMs + insertionExtensions * INSERTION_TIMEOUT_EXTENSION_MS,
        )
        return
      }
      const timedOutCtx: TimedOutProcessingContext = {
        runId,
        timedOutAt: Date.now(),
        settled: false,
        audioDurationSec: audioDur,
        wallTimeSec,
        promptResolution: this.run.currentPromptResolution ? { ...this.run.currentPromptResolution } : null,
        appContext: this.run.currentActiveAppContext ? { ...this.run.currentActiveAppContext } : null,
        audioChunks: this.run.recordedChunks.slice(),
        probeResult: this.run.cachedProbeResult ? { ...this.run.cachedProbeResult } : null,
      }
      this.timedOutProcessingContext = timedOutCtx
      this.lateResultAbandoned = false
      this.lateResultRunId = runId
      addRuntimeEvent('warn', 'recorder', 'Processing timed out; waiting out the late-final grace period', {
        audioSec: audioDur,
        timeoutMs: processingTimeoutMs,
        lateFinalGraceMs: LATE_FINAL_GRACE_MS,
      })

      // 寬限期這 15 秒以前是**完全靜默**的：懸浮窗當場消失，而遲到的 final 仍會自動
      // 插字。使用者看到的是"什麼都沒發生，然後文字忽然自己出現了"。現在明說仍在等，
      // 並且給 Esc 一個放棄的機會（放棄後遲到結果只落歷史、不插字）。
      this.overlayService.showAwaitingLateResult(LATE_FINAL_GRACE_MS / 1000, runId)

      // 安全網：寬限期內沒等到遲到的 final，就把這段錄音寫進歷史（空結果 + 明確原因），
      // 使用者可以在歷史裡回放和「重新識別」。音訊本身已在 stopRecording 時落盤。
      const historyMeta = this.buildHistoryMetadata(timedOutCtx.promptResolution, timedOutCtx.appContext)
      window.setTimeout(() => {
        // 只認 settled：遲到 final 已接手收尾時讓位，避免同一段錄音寫出兩條記錄。
        // 絕不再判 isRunCurrent —— 使用者又按了一次熱鍵不等於放棄上一段（見 settled 註釋）。
        if (timedOutCtx.settled) return
        timedOutCtx.settled = true
        this.releaseRecordingFocus(timedOutCtx.runId)
        // 仍是當前代時才動 Provider：新錄音已在 startRecording 裡換過會話，這裡再 cancel
        // 會把使用者正在錄的這一段打斷。
        if (this.timedOutProcessingContext === timedOutCtx) {
          this.timedOutProcessingContext = null
          this.provider.cancel()
          if (this.provider.mode === 'server') this.ensureConnection()
        }
        void (async () => {
          // 等待終止之後才進失敗卡 —— 寬限期裡顯示的是「仍在獲取結果」，兩者不能同時。
          // 使用者已經自己放棄（按過 Esc）時不要再彈一張卡片打擾他。
          const stillWaiting = !this.lateResultAbandoned && this.isRunCurrent(timedOutCtx.runId)
          if (stillWaiting) {
            await this.failRunWithCard(timedOutCtx.runId, {
              title: t('recorder.processingTimeoutTitle'),
              detail: t('recorder.processingTimeoutDetail'),
              audioDurationSec: timedOutCtx.audioDurationSec,
              wallTimeSec: timedOutCtx.wallTimeSec,
              failReason: t('recorder.processingTimeout'),
              failReasonCode: 'processing_timeout',
              historyMeta,
            })
          } else {
            await this.history.writeFailedRun({
              runId: timedOutCtx.runId,
              audioDurationSec: timedOutCtx.audioDurationSec,
              wallTimeSec: timedOutCtx.wallTimeSec,
              failReason: t('recorder.processingTimeout'),
              failReasonCode: 'processing_timeout',
              historyMeta,
            })
          }
          this.finishRun(timedOutCtx.runId)
          // 寬限期已經走完，這一代不會再有遲到結果了。
          if (this.lateResultRunId === timedOutCtx.runId) {
            this.lateResultRunId = 0
            this.lateResultAbandoned = false
          }
        })()
      }, LATE_FINAL_GRACE_MS)

      // keepOverlay：寬限期提示剛發出去，絕不能在這裡把懸浮窗隱藏掉。
      this.resetToIdle({ preserveLateFinalContext: true, keepOverlay: true })
    }
    this.processingTimeoutId = setTimeout(onProcessingTimeout, processingTimeoutMs)
  }

  // ── Toggle / hands-free ──

  private pttToggle(isHandsFree = false) {
    const now = Date.now()
    if (now - this.lastToggleTime < 500) {
      addRuntimeEvent('info', 'ptt', 'Ignored toggle request', {
        isHandsFree,
        ignoreReason: 'cooldown',
        recorderState: this.state,
      })
      return
    }
    this.lastToggleTime = now

    if (this.state === 'idle') {
      if (isHandsFree) {
        this.handsFreeMode = true
        this.pttSuppressed = true
        setTimeout(() => { this.pttSuppressed = false }, 500)
      }
      addRuntimeEvent('info', 'ptt', 'toggle -> startRecording', {
        isHandsFree,
        recorderState: this.state,
      })
      void this.startRecording()
      return
    }

    if (this.state === 'recording') {
      if (isHandsFree || !this.handsFreeMode) {
        this.handsFreeMode = false
        addRuntimeEvent('info', 'ptt', 'toggle -> stopRecording', {
          isHandsFree,
          recorderState: this.state,
        })
        void this.stopRecording()
      }
      return
    }

    addRuntimeEvent('info', 'ptt', 'Ignored toggle request', {
      isHandsFree,
      ignoreReason: 'state_not_toggleable',
      recorderState: this.state,
      handsFreeMode: this.handsFreeMode,
    })
  }

  private getPTTEventContext(payload?: unknown) {
    const p = (payload && typeof payload === 'object')
      ? (payload as PTTEventPayload)
      : {}
    return {
      source: p.source || 'unknown',
      keycode: p.keycode,
      rawcode: p.rawcode,
      modifiers: {
        alt: p.altKey,
        ctrl: p.ctrlKey,
        shift: p.shiftKey,
      },
      reason: p.reason,
      pttSetting: p.pttSetting,
      timestamp: p.timestamp,
      recorderState: this.state,
      handsFreeMode: this.handsFreeMode,
      pttSuppressed: this.pttSuppressed,
    }
  }

  private logPTTEvent(event: 'down' | 'up' | 'toggle' | 'hands_free', payload?: unknown) {
    addRuntimeEvent('info', 'ptt', `event:${event}`, this.getPTTEventContext(payload))
  }

  private notePTTDown(payload?: unknown) {
    const p = (payload && typeof payload === 'object')
      ? (payload as PTTEventPayload)
      : {}
    this.lastPTTUpUsedModifier = Boolean(
      p.altKey
      || p.ctrlKey
      || p.shiftKey
      || this.isModifierPTTSetting(p.pttSetting),
    )
  }

  private notePTTUp(payload?: unknown) {
    const p = (payload && typeof payload === 'object')
      ? (payload as PTTEventPayload)
      : {}
    this.lastPTTUpAt = Date.now()
    this.lastPTTUpUsedModifier = Boolean(
      p.altKey
      || p.ctrlKey
      || p.shiftKey
      || this.isModifierPTTSetting(p.pttSetting),
    )
  }

  private isModifierPTTSetting(pttSetting?: string) {
    return _isModifierPTTSetting(pttSetting)
  }

  private async waitForModifierPTTReleaseIfNeeded() {
    if (!this.lastPTTUpUsedModifier || this.lastPTTUpAt <= 0) return
    const elapsedMs = Date.now() - this.lastPTTUpAt
    if (elapsedMs >= MODIFIER_PTT_RELEASE_GUARD_MS) return
    const waitMs = MODIFIER_PTT_RELEASE_GUARD_MS - elapsedMs
    addRuntimeEvent('info', 'recorder', 'Waiting for modifier keys to settle before inserting text', {
      waitMs,
      lastPTTUpAt: this.lastPTTUpAt,
    })
    await new Promise((resolve) => setTimeout(resolve, waitMs))
  }

  private summarizeAppContext(context: ActiveAppContext | null) {
    return _summarizeAppContext(context)
  }

  private buildStatsAppId(appContext: ActiveAppContext | null, promptResolution: PromptResolution | null) {
    return _buildStatsAppId(appContext, promptResolution?.appId)
  }

  private buildHistoryMetadata(
    promptResolution?: PromptResolution | null,
    appContext?: ActiveAppContext | null,
  ) {
    const resolved = promptResolution || this.run.currentPromptResolution || undefined
    const ctx = appContext === undefined ? this.run.currentActiveAppContext : appContext
    return {
      appId: resolved?.appId,
      appName: resolved?.appName,
      // 錄音時聚焦視窗的原始資訊（用於反饋排錯）
      windowTitle: ctx?.windowTitle || undefined,
      processName: ctx?.processName || undefined,
      windowClass: ctx?.windowClass || undefined,
      promptPresetId: resolved?.preset.id,
      promptPresetName: resolved?.preset.name,
      promptRuleId: resolved?.matchedRule?.id,
      promptSummary: resolved?.summary,
      workMode: this.provider.mode,
    }
  }


  private computeProcessingTimeoutMs(audioDurationSec: number) {
    return _computeProcessingTimeoutMs(audioDurationSec, this.provider.mode)
  }

  private consumeTimedOutProcessingContext() {
    const context = this.timedOutProcessingContext
    if (!context) return null
    if (!this.isRunCurrent(context.runId)) {
      this.timedOutProcessingContext = null
      return null
    }
    // 超過 15 秒時把 context 留給已排隊的寬限期兜底定時器，避免雙方都放棄收尾。
    if (Date.now() - context.timedOutAt > LATE_FINAL_GRACE_MS) return null
    this.timedOutProcessingContext = null
    // 遲到 final 接手收尾，兜底定時器必須讓位，否則同一段錄音會寫出兩條記錄。
    context.settled = true
    return context
  }

  private async processFinalResult(
    result: FinalResult,
    context: TimedOutProcessingContext,
    options: { allowInsertionWhenIdle: boolean; source: 'processing' | 'late_after_timeout' },
  ) {
    const runId = context.runId
    if (!this.isRunCurrent(runId)) return

    // llmText === asrText 說明這段沒經過 AI 整理（極速模式，或後端未跑 LLM 直接回 asrText）。
    // 此時文本仍是純 ASR，「格式規範」由我們兜底；AI 整理過的文本則把格式交給 AI，只做文本替換。
    // New clients may talk to an older server that does not understand text_context. In that case
    // contextApplied is absent: paste the original selection back unchanged instead of replacing it
    // with a spoken edit command.
    const { baseText, rawAsr, selectedEditWasApplied } = resolveContextAwareOutput({
      asrText: result.asrText,
      llmText: result.llmText,
      contextApplied: result.contextApplied,
      textContext: context.appContext?.textContext,
    })
    let textToPaste: string
    try {
      textToPaste = selectedEditWasApplied
        ? await applyTextTransforms(baseText, { rawAsr })
        : baseText
    } catch (error) {
      if (!this.isRunCurrent(runId)) return
      addRuntimeEvent('warn', 'recorder', 'Text post-processing failed', { error: String(error), runId })
      if (baseText && baseText.trim()) {
        this.showFallbackAndReset(baseText, 'text_transform_failed', runId)
      } else {
        if (this.state === 'processing') {
          // 後處理拋異常且沒有可交付文本：識別本身是空的，所以用 'no_text'，
          // 不能說「未檢測到有效聲音」。
          this.overlayService.showNoSpeech('no_text', {
            reason: 'text_transform_failed',
            mode: this.provider.mode,
            error: String(error),
            runId,
          })
        }
        this.finishRun(runId)
        if (this.state === 'processing') this.resetToIdle({ keepOverlay: true })
      }
      return
    }
    if (!this.isRunCurrent(runId)) return

    const hasText = Boolean(textToPaste && textToPaste.trim())
    const audioDur = context.audioDurationSec
    const wallSec = context.wallTimeSec > 0 ? context.wallTimeSec : audioDur
    const promptResolution = context.promptResolution
    const appContext = context.appContext
    let historyArtifact: { runId: number; recordId: string; audioFilePath?: string } | null = null

    addRuntimeEvent('info', 'recorder', 'Final result received', {
      runId,
      hasText,
      asrMs: result.asrMs,
      llmMs: result.llmMs,
      durationSec: result.durationSec,
      audioSec: audioDur,
      textLen: textToPaste ? textToPaste.length : 0,
      source: options.source,
      contextApplied: result.contextApplied,
    })

    const written = await this.history.writeFinalRun({
      runId,
      result,
      textToPaste,
      // [自訂] S68：原選取文字可以貼回去，但不進歷史。
      selectionNotApplied: !selectedEditWasApplied,
      audioDurationSec: audioDur,
      wallTimeSec: wallSec,
      mode: () => this.provider.mode,
      isRunCurrent: () => this.isRunCurrent(runId),
      // 與原來一樣在寫入那一刻才取：promptResolution 為空時會回落到「當前」的那份。
      metadata: () => this.buildHistoryMetadata(promptResolution, appContext),
    })
    if (written.aborted) return
    historyArtifact = written.artifact

    // 歷史已經落定；下面是 UI 與文本插入，那些必須歸當前代所有。
    if (!this.isRunCurrent(runId)) {
      if (this.history.isRunCanceled(runId) && historyArtifact) {
        await this.history.discard(historyArtifact)
      }
      return
    }

    if (!hasText) {
      // 三種成因完全不同，以前共用一句「未檢測到有效聲音」：
      //   · ASR 出了字、後處理把它清空了 —— 是我們自己的替換規則/格式化造成的
      //   · ASR 沒出字，且採集側有近靜音實證 —— 確實沒說話
      //   · ASR 沒出字，但錄到了聲音 —— 呼叫側的問題（額度、服務端提前斷開、
      //     熱詞回顯被判空），說成"沒聲音"會把使用者直接引去查麥克風
      // asrLen / llmLen 只記長度不記內容。
      const asrHadText = Boolean(result.asrText?.trim())
      const silenceProven = !asrHadText && this.micLevel.hasSilenceEvidence()
      const diagnostic = {
        reason: 'final_empty',
        mode: this.provider.mode,
        audioSec: Number(audioDur.toFixed(1)),
        asrMs: result.asrMs,
        llmMs: result.llmMs,
        asrLen: result.asrText?.length ?? 0,
        llmLen: result.llmText?.length ?? 0,
        asrHadText,
        silenceProven,
        peakAmplitude: this.micLevel.roundedPeakAmplitude(),
        runId,
      }
      // 只有「ASR 出了字、被後處理清空」才彈失敗卡：那指向使用者自己的文本替換規則配錯了，
      // 需要他去改一處設定，是一條待辦。ASR 本身沒出字（不管採集側峰值高低）一律只給
      // toast —— 拿峰值區分「沒說話」和「呼叫失敗」是行不通的，見 OverlayService.showNoSpeech。
      if (asrHadText) {
        addRuntimeEvent('warn', 'recorder', 'Text processing emptied the transcript', diagnostic)
        this.overlayService.showFailure({
          title: t('recorder.emptyAfterProcessingTitle'),
          detail: t('recorder.emptyAfterProcessingDetail'),
          // 歷史記錄在上面已經寫好了（含音訊路徑），所以這裡能直接說去哪兒找。
          recovery: historyArtifact?.audioFilePath ? 'history' : 'none',
          token: runId,
        })
        this.activeFallbackToken = runId
      } else {
        this.overlayService.showNoSpeech(silenceProven ? 'silent' : 'no_text', diagnostic)
      }
      this.finishRun(runId)
      if (this.state === 'processing') {
        this.resetToIdle({ keepOverlay: true })
      }
      return
    }

    void this.updatePersonalizationFromFinal(runId, textToPaste, promptResolution, appContext)

    this.textInsertionInFlight = true
    // 插入卡死時要把這段文字交還給使用者（見 handleInsertionStuck）。
    this.textBeingInserted = textToPaste
    this.armInsertionTimeout(runId, textToPaste)
    try {
      await this.handleTextInsertion(textToPaste, {
        allowWhenIdle: options.allowInsertionWhenIdle,
        runId,
        probeResult: context.probeResult,
      })
    } catch (error) {
      if (!this.isRunCurrent(runId)) return
      addRuntimeEvent('error', 'recorder', 'Text insertion threw; showing fallback card', { error: String(error), runId })
      this.showFallbackAndReset(textToPaste, 'paste_exception', runId)
    } finally {
      this.clearInsertionTimeout()
      this.textInsertionInFlight = false
      this.textBeingInserted = ''
    }
  }

  private async updatePersonalizationFromFinal(
    runId: number,
    finalText: string,
    promptResolution: PromptResolution | null,
    appContext: ActiveAppContext | null,
  ) {
    if (!finalText.trim() || !this.isRunCurrent(runId)) return
    // [自訂] S40：不再保存使用統計 userStats（各 App 的使用次數、字數、首次／最後使用時間）。
    // this.userStats 維持啟動時讀到的值（新安裝即預設的空統計），介面與提示詞路由都能處理空統計。
    if (USER_STATS_RECORDING_DISABLED) return

    try {
      const wordCount = finalText.length
      const appId = this.buildStatsAppId(appContext, promptResolution)

      const nextStats = await recordSessionStats(appId, wordCount)
      if (!this.isRunCurrent(runId)) return
      this.userStats = nextStats
      addRuntimeEvent('info', 'personalization', 'session stats recorded', {
        appId,
        appName: promptResolution?.appName,
        wordCount,
        totalWords: this.userStats.totalWords,
        totalSessions: this.userStats.totalSessions,
      })
    } catch (error) {
      if (!this.isRunCurrent(runId)) return
      addRuntimeEvent('warn', 'personalization', 'failed to record session stats', {
        error: String(error),
      })
    }
  }
}
