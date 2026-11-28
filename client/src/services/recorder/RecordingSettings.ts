import type { PromptPreset } from '../store'
import { CONTEXT_SELECTION_EDIT_PROMPT } from '../contextAware'
import type { AppPromptRule } from '../personalization/types'

/**
 * 錄音器每次錄音要讀的設定。由 refreshRuntimeSettings 從儲存整份讀回，
 * 設定頁的快速開關（AI 整理、潤色模式、熱詞、流式顯示）直接改這裡，不必全量重新整理。
 */
export interface RecordingSettings {
  micId: string
  /**
   * 是否讓瀏覽器對麥克風做降噪。預設開（底噪大的機器需要），
   * 但降噪是按"人聽得舒服"最佳化的，可能削掉 ASR 要的細節，所以可關。
   */
  noiseSuppression: boolean
  /** 是否在錄音期間靜音系統輸出（防外放被麥克風回採）。預設關閉。 */
  muteSystemAudio: boolean
  /** 插入文本後是否自動還原剪貼簿為插入前內容。預設開啟。 */
  protectClipboard: boolean
  presets: PromptPreset[]
  activePresetId: string
  aiEnabled: boolean
  /** 0 = 所有語音都用 AI；正數 = 達到門檻才呼叫 AI。 */
  aiMinDurationSec: number
  /** Reads bounded editor text only when explicitly enabled; default is false. */
  contextAwareWriting: boolean
  /** User-visible selection-edit prompt. */
  contextSelectionEditPrompt: string
  appPromptRules: AppPromptRule[]
  hotwords: string[]
  /** 是否把熱詞注入 AI 提示詞（預設關，使用者可在熱詞頁開啟）。 */
  injectHotwords: boolean
  /** 伺服器識別語言；空串 = 自動。 */
  language: string
  /** 是否開啟流式即時顯示（識別過程中把中間結果即時顯示在懸浮窗）。預設關閉。 */
  streamingDisplay: boolean
}

/** 第一次讀回儲存之前用的值。 */
export function defaultRecordingSettings(): RecordingSettings {
  return {
    micId: '',
    noiseSuppression: true,
    muteSystemAudio: false,
    protectClipboard: true,
    presets: [],
    activePresetId: 'intent',
    aiEnabled: true,
    aiMinDurationSec: 0,
    contextAwareWriting: false,
    contextSelectionEditPrompt: CONTEXT_SELECTION_EDIT_PROMPT,
    appPromptRules: [],
    hotwords: [],
    injectHotwords: false,
    language: '',
    streamingDisplay: false,
  }
}
