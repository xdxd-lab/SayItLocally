// Provider 管理器 — 根据 workMode 返回对应的 TranscriptionProvider

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

/** 获取当前 Provider 实例（懒初始化） */
export function getProvider(): TranscriptionProvider {
  if (!currentProvider) {
    currentProvider = createProvider(currentMode)
  }
  return currentProvider
}

/** 获取当前工作模式 */
export function getWorkMode(): WorkMode {
  return currentMode
}

/**
 * 切换工作模式。
 * 会断开旧 Provider 并创建新的。
 * 调用方需要重新 connect。
 */
export async function switchProvider(_requested: WorkMode): Promise<TranscriptionProvider> {
  const mode: WorkMode = 'local' // [自訂 local-only]
  if (mode === currentMode && currentProvider) {
    return currentProvider
  }

  addRuntimeEvent('info', 'transcription', 'Processing mode changed', { from: currentMode, to: mode })

  // 断开旧 Provider
  if (currentProvider) {
    try {
      currentProvider.disconnect()
    } catch {
      // ignore
    }
  }

  // 离开本地模式时释放 sherpa-onnx recognizer 占用的内存（几百 MB ~ 数 GB），
  // 否则切到云 API / 服务器模式后本地模型仍常驻到应用退出。
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

/** 从 store 读取保存的 workMode 并初始化 */
export async function initProviderFromStore(): Promise<void> {
  // [自訂 local-only] 不論儲存值為何，一律 local，並寫回設定。
  const stored = await getSetting('workMode', 'local')
  if (stored !== 'local') await setSetting('workMode', 'local')
  currentMode = 'local'
  currentProvider = createProvider(currentMode)
  addRuntimeEvent('info', 'transcription', 'Provider initialized', { mode: currentMode })
}
