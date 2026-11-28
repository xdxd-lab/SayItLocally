// 側邊欄左下角"當前語音引擎"指示的全域性狀態（useSyncExternalStore 模式，同 connectionStatus）。
//
// 為什麼單獨一個 store：模式/模型資訊分散在三個設定裡（workMode、localAsr.modelId、
// cloudAsr.provider），側邊欄常駐但設定頁在別的路由改這些值，需要一個可訂閱的快照。
// 誰改了相關設定誰調 refreshModeStatus()——目前是 VoiceEnginePage（切模式）、
// LocalModeSection（換本地模型）、CloudAPISection（換雲供應商）三處 + 側邊欄掛載時。
//
// 有意從 store 直接讀 workMode 而不是 getWorkMode()：後者要等 initProviderFromStore
// 跑完才準，而側邊欄可能先掛載；讀儲存永遠是真值，也避免了迴圈依賴。

import { invoke } from '@tauri-apps/api/core'
import { getSetting } from '../services/store'
import { loadAsrProfiles } from '../features/settings/asrProfileStore'
import {
  describeAsrMissing,
  resolveActiveAsrProfile,
  resolveAsrModelOption,
  resolveAsrRuntimeProvider,
} from '../features/settings/asrProviderCatalog'
import { subscribeLocale, t } from '@/i18n'

export type ModeStatusMode = 'server' | 'cloud_api' | 'local'

export interface ModeStatus {
  mode: ModeStatusMode
  /** 簡短的引擎說明：本地 = 模型名（"SenseVoice Small"），雲 API = 供應商簡稱（"豆包"） */
  detail: string
  /**
   * 這個模式是不是**真的**能用了。
   * 伺服器模式為 null——它的可用性由 WebSocket 連線狀態決定，交給 useConnectionStatus。
   *
   * 為什麼加這個：狀態徽標原來對本地/雲 API 一律硬編碼綠點 +「就緒」，模型沒下載、
   * key 沒填也照樣綠燈。使用者據此關掉視窗、按下熱鍵、什麼都沒發生，且不知道去哪兒查。
   */
  ready: boolean | null
  /** 未就緒時缺的是什麼（一句話，直接可展示） */
  blockedReason: string
}

type Listener = () => void

let currentStatus: ModeStatus = { mode: 'local', detail: '', ready: null, blockedReason: '' }
const listeners = new Set<Listener>()

function emitChange() {
  for (const listener of listeners) listener()
}

export function getModeStatus(): ModeStatus {
  return currentStatus
}

export function subscribeModeStatus(listener: Listener): () => void {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

/** 雲 API 供應商 key → 側邊欄簡稱。完整模型名在 Tooltip 裡（resolveAsrDisplayModel）。 */
function cloudProviderShort(provider: string): string {
  switch (provider) {
    case 'doubao_v2': return t('modeStatus.doubao')
    case 'qwen': return t('modeStatus.qwen')
    case 'qwen_realtime': return t('modeStatus.qwenRealtime')
    case 'qwen_audio_stream': return t('modeStatus.qwenAudioStream')
    case 'qwen_omni_35_plus':
    case 'qwen_omni_35_flash':
    case 'qwen_omni_flash':
    case 'qwen_omni_turbo': return t('modeStatus.qwenOmni')
    case 'mimo': return 'MiMo'
    case 'groq_whisper': return 'Groq'
    case 'openai_transcribe': return 'OpenAI'
    case 'openai_live_transcribe': return 'OpenAI Live'
    case 'gemini_transcribe': return 'Gemini'
    case 'gemini_live_transcribe': return 'Gemini Live'
    case 'openrouter_transcribe': return 'OpenRouter'
    default: return provider
  }
}

/** 重新從設定裡讀一遍模式/模型並廣播。相關設定變更後呼叫。 */
export async function refreshModeStatus(): Promise<void> {
  const mode: ModeStatusMode = 'local' // [自訂 local-only]

  let detail = ''
  let ready: boolean | null = null
  let blockedReason = ''

  if (mode === 'local') {
    const modelId = await getSetting('localAsr.modelId', 'sensevoice-small-gguf') as string
    try {
      const models = await invoke<{ id: string; name: string }[]>('list_available_models')
      detail = models.find((m) => m.id === modelId)?.name ?? modelId
    } catch {
      detail = modelId
    }
    // 本地模式就緒 = 選中的模型已完整下載到本地
    try {
      const downloaded = await invoke<{ id: string; complete: boolean }[]>('list_downloaded_models')
      ready = downloaded.some((m) => m.id === modelId && m.complete)
      if (!ready) blockedReason = t('modeStatus.modelNotDownloaded')
    } catch {
      // 讀不到本地模型列表時不敢斷言就緒，按未就緒處理並說明原因
      ready = false
      blockedReason = t('modeStatus.modelListUnavailable')
    }
  } else if (mode === 'cloud_api') {
    // 就緒與否看**啟用中那份服務檔案**，而不是各平台的原始鍵：同一家可以存多份，
    // 只有被啟用的那份才決定錄音時用什麼。設定頁與這裡都從同一個列表讀，不會打架。
    const state = await loadAsrProfiles()
    const active = resolveActiveAsrProfile(state.profiles, state.activeId)
    if (!active) {
      detail = t('modeStatus.notConfigured')
      ready = false
      blockedReason = t('modeStatus.noAsrService')
    } else {
      // 側邊欄顯示的是**選中模型的分發 key**對應的簡稱，不是卡片 id ——
      // 一張卡下的模型可能來自不同實現，只說平台名看不出在用哪個
      detail = cloudProviderShort(resolveAsrRuntimeProvider(active))
      const missing = describeAsrMissing(active)
      // 流式識別缺業務空間 ID 時也算沒配好：它會直接連不上地域專屬端點。
      // 這是模型級要求（千問那五個裡只有 realtime 那個要）
      const needsWorkspace = resolveAsrModelOption(active)?.needsWorkspaceId === true
        && active.workspaceId.trim() === ''
      ready = missing === '' && !needsWorkspace
      blockedReason = missing || (needsWorkspace ? t('modeStatus.noWorkspace') : '')
    }
  }

  if (
    currentStatus.mode === mode
    && currentStatus.detail === detail
    && currentStatus.ready === ready
    && currentStatus.blockedReason === blockedReason
  ) return
  currentStatus = { mode, detail, ready, blockedReason }
  emitChange()
}

// 快照裡包含可展示字串，切換介面語言後必須重新計算，不能沿用舊語言快取。
subscribeLocale(() => { void refreshModeStatus() })
