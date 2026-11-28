/**
 * 訂閱 autoUpdate 的全域性單例狀態。
 *
 * autoUpdate.ts 的狀態是模組級單例 + 手寫訂閱（不是 React store），所以這裡用
 * useSyncExternalStore 橋一下。getAutoUpdateState 返回的物件只在 setState 時換引用，
 * 滿足 getSnapshot 的穩定性要求。
 */

import { useSyncExternalStore } from 'react'
import { getAutoUpdateState, onAutoUpdateChange, type AutoUpdateState } from './autoUpdate'

export function useUpdateState(): AutoUpdateState {
  return useSyncExternalStore(onAutoUpdateChange, getAutoUpdateState)
}
