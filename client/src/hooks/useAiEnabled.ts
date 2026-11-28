import { useSyncExternalStore } from 'react'
import { subscribeAiEnabled, getAiEnabled, getAiEnabledReady } from '@/stores/aiEnabled'

export function useAiEnabled() {
  return useSyncExternalStore(subscribeAiEnabled, getAiEnabled)
}

/** AI 開關初始值是否已就緒（首幀前為 false，用於抑制啟動時的開關閃動） */
export function useAiEnabledReady() {
  return useSyncExternalStore(subscribeAiEnabled, getAiEnabledReady)
}
