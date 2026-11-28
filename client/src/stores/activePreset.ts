// 當前潤色模式（active preset）的全域性響應式狀態。
// 讓 AI 整理設定頁高亮、標題欄狀態等多處共享同一狀態：
// 無論是頁面點選切換，還是全域性快捷鍵切換，都能即時同步重新整理。
// 採用 useSyncExternalStore 模式（與 aiEnabled 一致，無外部依賴）。
//
// 注意：此 store 只依賴 services/store，不依賴 services/recorder，避免迴圈引用。
// 錄音器快取的重新整理由呼叫方（頁面/orchestrator）各自負責。

import { getActivePresetId, getPromptPresets } from '@/services/store'

type Listener = () => void

export interface ActivePresetState {
  id: string
  name: string
}

let current: ActivePresetState = { id: 'intent', name: '' }
let initialized = false
const listeners = new Set<Listener>()

function emitChange() {
  for (const listener of listeners) listener()
}

async function resolve(id: string): Promise<ActivePresetState> {
  try {
    const presets = await getPromptPresets()
    const found = presets.find((p) => p.id === id)
    return { id, name: found?.name || '' }
  } catch {
    return { id, name: '' }
  }
}

/** 應用啟動時呼叫，讀取初始啟用預設 */
export async function initActivePreset(): Promise<void> {
  if (initialized) return
  initialized = true
  const id = await getActivePresetId()
  current = await resolve(id)
  emitChange()
}

export function getActivePresetSnapshot(): ActivePresetState {
  return current
}

export function subscribeActivePreset(listener: Listener): () => void {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

/** 用已知的 id/name 立即更新（無 IPC，供快捷鍵切換即時重新整理 UI） */
export function setActivePresetKnown(id: string, name: string): void {
  if (id !== current.id || name !== current.name) {
    current = { id, name }
    emitChange()
  }
}

/** 重新從持久化狀態讀取當前預設（切換、改名後呼叫），有變化才通知訂閱者 */
export async function refreshActivePreset(): Promise<void> {
  const id = await getActivePresetId()
  const next = await resolve(id)
  if (next.id !== current.id || next.name !== current.name) {
    current = next
    emitChange()
  }
}
