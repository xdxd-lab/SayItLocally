import type { AiConfigSnapshot } from '../transcription/aiPolicy'
import type { ActiveAppContext } from '../../types/appContext'
import type { PromptResolution } from '../personalization/types'
import type { ProbeResult } from './PasteService'

/**
 * 一次錄音的執行期資料，從 RecorderOrchestrator 的散欄位收攏而來（模組 6 第二步）。
 *
 * 這一步只是把欄位搬到一處：每個欄位在編排器裡什麼時候復位，和原來逐欄位寫時一模一樣。
 * 「插入中」「正在插入的文字」和遲到結果相關的欄位**不在這裡**：超時後的遲到結果會在
 * 空閒狀態下插字，那時它們屬於上一代，不能跟著新一次錄音一起復位。
 */
export class RunState {
  recordStartPerf = 0
  audioSentSamples = 0
  /** Wall time captured at stopRecording — used for history durationSec so it
   *  matches what the user saw on the overlay (not inflated by backend latency). */
  wallTimeAtStopSec = 0
  finalHandledInCurrentRun = false
  /** Esc 僅在 processing 的可逆階段允許取消；進入系統貼上前永久關閉。 */
  processingCancelable = false
  /** PTT up arrived while startRecording was still initializing — stop immediately after setup */
  pendingStopWhileStarting = false
  finalReceivedAt = 0
  recordedChunks: ArrayBuffer[] = []
  captureReadyPromise: Promise<void> | null = null
  /**
   * 本次錄音開始時凍結的 AI 配置與日誌關聯標識。
   *
   * 不直接用 cached* 那幾個欄位算策略：它們會被設定頁的改動隨時重新整理，使用者錄完之後
   * 改一下門檻，這一條的原因就跟著變了 —— 事後沒法解釋歷史記錄。
   */
  currentAiConfig: AiConfigSnapshot | undefined
  currentOperationId: string | undefined
  currentActiveAppContext: ActiveAppContext | null = null
  currentPromptResolution: PromptResolution | null = null
  /** Probe result captured at startRecording time (before audio capture begins).
   *  Used by handleTextInsertion so we inject into the window that was focused
   *  when the user started speaking, not whatever happens to be focused later. */
  cachedProbeResult: ProbeResult | null = null
}
