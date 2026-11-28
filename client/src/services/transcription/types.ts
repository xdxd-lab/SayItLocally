// 轉寫 Provider 抽象層型別定義
// 所有工作模式（伺服器 / 雲 API / 本地）共享此介面

import type { ActiveAppContext, TextContext } from '../../types/appContext'
import type { ClientRuntimeInfo } from '../../types/appApi'
import type { AiConfigSnapshot, AiPolicy } from './aiPolicy'
import type { ClientAiConfig } from './selectionAi'

export type WorkMode = 'server' | 'cloud_api' | 'local'

export type ProviderState = 'disconnected' | 'connecting' | 'connected' | 'error'

/**
 * 一次錄音進行中連線掉線的哨兵錯誤。
 *
 * 走 onError 的統一失敗通道（這樣音訊存檔/歷史/提示都複用同一條路），但錄音器要能把它
 * 單獨歸類成 connection_lost —— 這段語音是**從未送達**服務端，跟「服務端處理失敗」
 * 不是一回事，使用者看到的解釋也不該相同。
 */
export const MID_SESSION_DISCONNECT_ERROR
  = 'sayit_error:server_unreachable:websocket disconnected mid-session'

export type AiExecutionSource = 'server' | 'custom' | 'none'
export type AiExecutionStatus = 'applied' | 'skipped' | 'unavailable' | 'failed'

export interface ASRResult {
  text: string
  asrMs: number
  durationSec: number
  /** 空 ASR 會先於 final 觸發專用收尾，執行後設資料必須隨它一起落庫。 */
  aiSource?: AiExecutionSource
  aiStatus?: AiExecutionStatus
}

export interface FinalResult {
  asrText: string
  llmText: string
  asrMs: number
  llmMs: number
  durationSec: number
  asrEngine?: string
  asrModel?: string
  /** True only when the AI actually received and processed this run's editor context. */
  contextApplied?: boolean
  /** Explicit execution metadata; text equality cannot prove whether AI ran. */
  aiSource?: AiExecutionSource
  aiStatus?: AiExecutionStatus
  aiProvider?: string
  aiModel?: string
  /**
   * 跳過 / 失敗的穩定原因碼（AiReason）。
   *
   * 宣告在這裡是因為它**執行時已經**透過 `...polish` 流到錄音器了（spread 不觸發
   * TS 的多餘屬性檢查，不宣告只會讓它成為一個看不見的隱式欄位）。
   * 第 1a 批只保證日誌有它，寫進歷史記錄是第 2 批的事。
   */
  aiReason?: string
  /**
   * 服務端 AI 的執行證據，從 llm_debug 裡**只取結論性欄位**。
   *
   * 不透傳整個 llm_debug：它在伺服器開了 debug_llm 時會帶完整 prompt 與原始輸出，
   * 順著結果物件傳播出去早晚會被某處日誌整條打出來。
   * 有它才能把「服務端跑了但很快」和「服務端壓根沒跑」分開 —— 光看 llm_ms=0 分不出來。
   */
  serverAi?: { error?: string; provider?: string }
}

export interface TranscriptionCallbacks {
  onStateChange?: (state: ProviderState) => void
  onReady?: (info: { connectionId?: string; asr: boolean; llm: boolean }) => void
  /** 流式識別過程中的中間結果（即時上屏用），text 為到目前為止的累計文本 */
  onPartialASR?: (text: string) => void
  onASR?: (result: ASRResult) => void
  onFinal?: (result: FinalResult) => void
  onDone?: () => void
  onError?: (msg: string) => void
}

export interface StartOptions {
  /** 當前錄音代次；取消或開始下一次後，舊代次的非同步結果必須全部丟棄。 */
  runId: number
  /** 本次處理的日誌關聯標識。歷史重跑必須新建，不能複用被重跑那條記錄的舊標識。 */
  operationId?: string
  /**
   * 本次凍結的 AI 相關配置。凍結的理由是事後解釋：使用者錄完之後改了設定，
   * 這條記錄的原因也不能跟著變。
   */
  aiConfig?: AiConfigSnapshot
  /** Frozen selection-edit credentials for this run only. Never serialize to logs or history. */
  selectionAiConfig?: ClientAiConfig | null
  systemPrompt?: string
  disableAi?: boolean
  /** 錄音未達到該時長時只做識別，不呼叫 AI。0 / undefined = 不設門檻。 */
  aiMinDurationSec?: number
  clientMeta?: ClientRuntimeInfo | null
  appContext?: ActiveAppContext | null
  /** Bounded editor text captured at recording start. Never persisted in history/logs. */
  textContext?: TextContext | null
  source?: 'live' | 'history_reprocess'
  hotwords?: string[]
  language?: string
  /** 是否開啟流式即時顯示：識別過程中把中間結果即時推給懸浮窗 */
  streamingDisplay?: boolean
}

export interface StopOptions {
  pttHoldMs?: number
  /**
   * 松鍵時才知道的「這次別做 AI 整理」。錄音時長要等錄完才知道，而伺服器模式的 AI
   * 在服務端緊跟 ASR 執行，start 時來不及決定。
   *
   * 這是 policy 壓成的布林，只給線上協議用。**壓完的值不要再回頭當跳過原因**
   * ——自配 AI 路線也會發 true（服務端不做、客戶端做），把它解釋成"整次跳過"是錯的。
   */
  disableAi?: boolean
  /**
   * 本次的完整策略（含路由與原因）。松鍵時由錄音器用實際 PCM 時長算出，
   * 隨 stop 一起交給 Provider，保證「線上那個布林」和「日誌裡記的原因」出自同一次判斷。
   */
  aiPolicy?: AiPolicy
  audioStats?: {
    avgRms: number
    peakRms: number
    peakAmplitude: number
    silenceRatio: number
    totalFrames: number
  }
}

/**
 * 轉寫 Provider 介面。
 * RecorderOrchestrator 透過此介面與具體的轉寫實現互動，
 * 不再直接依賴 WebSocket 或任何特定的後端協議。
 */
export interface TranscriptionProvider {
  readonly mode: WorkMode

  /** 建立連線 / 初始化 */
  connect(callbacks: TranscriptionCallbacks): Promise<void>

  /** 開始一次轉寫會話 */
  start(opts: StartOptions): boolean

  /** 立即取消當前會話；允許底層任務自然結束，但之後不得再上拋任何結果。 */
  cancel(): void

  /** 傳送音訊資料（流式，PCM Int16 ArrayBuffer） */
  sendAudio(buffer: ArrayBuffer): void

  /** 結束錄音，觸發處理 */
  stop(opts?: StopOptions): boolean

  /** 斷開連線 / 釋放資源 */
  disconnect(): void

  /** 是否已就緒可以開始轉寫 */
  isReady(): boolean
}
