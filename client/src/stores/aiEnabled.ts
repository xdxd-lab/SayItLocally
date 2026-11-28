// AI 整理開關的全域性狀態 store
// 讓標題欄與 AI 整理設定頁共享同一狀態，任一處切換都會同步
// 採用 useSyncExternalStore 模式（與 connectionStatus 一致，無外部依賴）

import { getSetting, setSetting } from '@/services/store'
import { setAiEnabledCache, showAiEnabledToast } from '@/services/recorder'
import * as bridge from '@/services/bridge'

type Listener = () => void

let currentValue = false
let initialized = false
let ready = false
const listeners = new Set<Listener>()

function emitChange() {
  for (const listener of listeners) listener()
}

/** 從持久化設定讀取初始值（僅首次生效），供應用啟動時呼叫 */
export async function initAiEnabled(): Promise<void> {
  if (initialized) return
  initialized = true
  const stored = await getSetting('aiEnabled', false)
  const next = Boolean(stored)
  if (next !== currentValue) {
    currentValue = next
    emitChange()
  }
  // 托盤建選單時自己讀了一次庫，正常情況下已經對了；這裡再對齊一次是兜底
  // （兩邊的預設值口徑若哪天又走偏，至少啟動後就能自愈）。
  void bridge.setTrayAiEnabled(next)

  // 托盤右鍵的「AI 整理」由 Rust 落庫後廣播過來。監聽放在 store 而不是某個元件裡：
  // 這條狀態的所有寫入路徑都收在這一個檔案，才不會漏掉錄音器快取那一步。
  // 不做取消訂閱 —— store 與 webview 同生命週期。
  //
  // 順序有講究：**必須在讀回初值之後**才開始監聽。托盤選單在前端載入前就能點，
  // 若先監聽，一次落在「已讀到舊值、還沒賦值」視窗裡的廣播會被隨後的舊值覆蓋掉；
  // 放在後面則這次點選已經進庫，上面那次讀取拿到的就是新值。
  bridge.onAiCleanupChanged((enabled) => {
    applyExternalAiEnabled(enabled)
  })
  bridge.onAiCleanupToggleRequested(() => {
    void setAiEnabled(!currentValue, { showToast: true })
  })
  // 初始值已落定：下一幀再置 ready，讓標題欄等訂閱方在值穩定後才顯示/放動畫，
  // 避免冷啟動時開關從預設(關)跳到已儲存(開)被看見。
  const flipReady = () => { ready = true; emitChange() }
  if (typeof requestAnimationFrame === 'function') requestAnimationFrame(flipReady)
  else flipReady()
}

export function getAiEnabled(): boolean {
  return currentValue
}

/** AI 開關初始值是否已從設定讀回（供 UI 決定首幀是否顯示/放動畫） */
export function getAiEnabledReady(): boolean {
  return ready
}

export function subscribeAiEnabled(listener: Listener): () => void {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

/** 設定開關狀態：更新記憶體狀態 + 通知訂閱者 + 持久化 + 同步錄音器快取（輕量，無 IPC 全量重新整理）。
 *  持久化寫入放到後台執行，不阻塞 UI 反饋，避免快速切換時卡頓。 */
export async function setAiEnabled(next: boolean, options: { showToast?: boolean } = {}): Promise<void> {
  if (next !== currentValue) {
    currentValue = next
    emitChange()
  }
  setAiEnabledCache(next)
  void setSetting('aiEnabled', next)
  // 托盤選單建好後不再重建，狀態只能這樣推過去。
  void bridge.setTrayAiEnabled(next)
  if (options.showToast) showAiEnabledToast(next)
}

/** 切換開關狀態 */
export async function toggleAiEnabled(): Promise<void> {
  await setAiEnabled(!currentValue)
}

/** 狀態已在別處（目前只有托盤右鍵）改完並落庫：只同步記憶體、訂閱方與錄音器快取。
 *  刻意不再寫庫、也不回寫托盤 —— 那兩件事 Rust 側已經做了，重複做會出現兩個寫者。 */
function applyExternalAiEnabled(next: boolean): void {
  if (next !== currentValue) {
    currentValue = next
    emitChange()
  }
  setAiEnabledCache(next)
}
