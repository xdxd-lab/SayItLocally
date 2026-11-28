import { RecorderOrchestrator } from './recorder/RecorderOrchestrator'
import type { RecorderState } from './recorder/types'
import type { PromptPreset } from './store'

let orchestrator = new RecorderOrchestrator()

// HMR cleanup: dispose old orchestrator when module is hot-replaced
if ((import.meta as unknown as Record<string, unknown>).hot) {
  const hot = (import.meta as unknown as Record<string, unknown>).hot as { dispose: (cb: () => void) => void }
  hot.dispose(() => {
    console.log('[recorder] HMR dispose: cleaning up old orchestrator')
    orchestrator.cleanup()
  })
}

export function setStateListener(cb: (s: RecorderState) => void) {
  orchestrator.setStateListener(cb)
}

export function getState() {
  return orchestrator.getState()
}

export async function initRecorder() {
  await orchestrator.init()
}

export function cleanup() {
  orchestrator.cleanup()
}

/** Call after changing settings that recorder caches (preset, mic, overlay display) */
export async function refreshRecorderSettings() {
  await orchestrator.refreshRuntimeSettings()
}

/** 輕量：僅同步 AI 整理開關到錄音器快取，無 IPC，避免快速切換時卡頓 */
export function setAiEnabledCache(next: boolean) {
  orchestrator.setAiEnabledCache(next)
}

/** AI 開關由全域性快捷鍵切換後，在空閒狀態用懸浮窗確認結果。 */
export function showAiEnabledToast(enabled: boolean) {
  orchestrator.showAiEnabledToast(enabled)
}

/** 輕量：僅同步當前潤色模式到錄音器快取，無 IPC，避免快速切換時卡頓 */
export function setActivePresetCache(id: string) {
  orchestrator.setActivePresetCache(id)
}

/** 輕量：同步最新潤色模式列表，保證新建/編輯後的預設可被下一次錄音解析 */
export function setPromptPresetsCache(presets: PromptPreset[]) {
  orchestrator.setPromptPresetsCache(presets)
}

/** 輕量：同步下一次錄音使用的熱詞快照，無需切換語音引擎或全量重新整理設定 */
export function setHotwordsCache(words: string[]) {
  orchestrator.setHotwordsCache(words)
}

/** 輕量：僅同步「流式即時顯示」開關到錄音器快取，無 IPC，切換後立即生效 */
export function setStreamingDisplayCache(next: boolean) {
  orchestrator.setStreamingDisplayCache(next)
}

/** 僅重新整理 overlay 顯示設定（主題/長度/時長）— 輕量，避免觸發全量錄音快取重新整理 */
export async function refreshOverlaySettings() {
  await orchestrator.refreshOverlaySettings()
}

/** 工作模式切換後重新連線 */
export function reconnectProvider() {
  orchestrator.reconnectProvider()
}

/** Backward-compatible alias */
export async function refreshPreset() {
  await orchestrator.refreshRuntimeSettings()
}

/** 臨時停用/啟用 PTT（用於歡迎向導熱鍵確認步驟） */
export function setPttSuppressed(suppressed: boolean) {
  orchestrator.setPttSuppressed(suppressed)
}

