// Provider 管理器 — 根據 workMode 返回對應的 TranscriptionProvider

import { invoke } from '@tauri-apps/api/core'
import { getSetting, setSetting } from '../store'
import { addRuntimeEvent } from '../debugLog'
// [自訂 local-only] Server / Cloud API provider 不再匯入，只保留 LocalProvider。
import { LocalProvider } from './LocalProvider'
import type { TranscriptionProvider, WorkMode } from './types'

export type {
  TranscriptionProvider,
  TranscriptionCallbacks,
  StartOptions,
  StopOptions,
  FinalResult,
  ASRResult,
  WorkMode,
  ProviderState,
  AiExecutionSource,
  AiExecutionStatus,
} from './types'
export { MID_SESSION_DISCONNECT_ERROR } from './types'

let currentProvider: TranscriptionProvider | null = null
let currentMode: WorkMode = 'local'

function createProvider(mode: WorkMode): TranscriptionProvider {
  // [自訂 local-only] 任何模式一律使用本機辨識。
  if (mode !== 'local') {
    addRuntimeEvent('warn', 'transcription', `Mode "${mode}" is disabled in this build; using local mode`)
  }
  return new LocalProvider()
}

/** 獲取當前 Provider 例項（懶初始化） */
export function getProvider(): TranscriptionProvider {
  if (!currentProvider) {
    currentProvider = createProvider(currentMode)
  }
  return currentProvider
}

/** 獲取當前工作模式 */
export function getWorkMode(): WorkMode {
  return currentMode
}

/**
 * 切換工作模式。
 * 會斷開舊 Provider 並建立新的。
 * 呼叫方需要重新 connect。
 */
export async function switchProvider(_requested: WorkMode): Promise<TranscriptionProvider> {
  const mode: WorkMode = 'local' // [自訂 local-only]
  if (mode === currentMode && currentProvider) {
    return currentProvider
  }

  addRuntimeEvent('info', 'transcription', 'Processing mode changed', { from: currentMode, to: mode })

  // 斷開舊 Provider
  if (currentProvider) {
    try {
      currentProvider.disconnect()
    } catch {
      // ignore
    }
  }

  // 離開本地模式時釋放 sherpa-onnx recognizer 佔用的記憶體（幾百 MB ~ 數 GB），
  // 否則切到雲 API / 伺服器模式後本地模型仍常駐到應用退出。
  if (currentMode === 'local' && mode !== 'local') {
    try {
      await invoke('unload_local_model')
    } catch (err) {
      addRuntimeEvent('warn', 'transcription', 'Failed to release local model', { error: String(err) })
    }
  }

  currentMode = mode
  currentProvider = createProvider(mode)
  return currentProvider
}

/** 從 store 讀取儲存的 workMode 並初始化 */
export async function initProviderFromStore(): Promise<void> {
  // [自訂 local-only] 不論儲存值為何，一律 local，並寫回設定。
  // [自訂] S29：無條件寫入（原本只有「讀到的值不是 local」才寫；未設定時 getSetting 回傳預設 'local'，
  // 於是永遠不會寫入，Rust 端啟動時讀不到 workMode、不會預載本地模型）。
  // 原：const stored = await getSetting('workMode', 'local')
  // 原：if (stored !== 'local') await setSetting('workMode', 'local')
  await setSetting('workMode', 'local')
  currentMode = 'local'
  currentProvider = createProvider(currentMode)
  addRuntimeEvent('info', 'transcription', 'Provider initialized', { mode: currentMode })
}
