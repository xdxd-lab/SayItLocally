// 「語音引擎」頁裡是否有尚未儲存的配置改動（useSyncExternalStore 模式）。
//
// 為什麼需要跨元件共享：服務地址（ServerSection）和 API Key（CloudAPISection）都只活在
// 各自元件的 local state 裡，而「識別測試」（AsrTestSection）是它們的兄弟元件、讀的是
// **已儲存**的值。使用者粘完 key 直接點「開始測試」必然失敗，然後誤以為 key 是壞的。
// 這個標記讓測試入口能在有未儲存改動時把自己禁掉並說明原因。
//
// 誰改誰負責清：ServerSection / CloudAPISection 在輸入變化與儲存成功後各調一次，
// 解除安裝時（切走路由）復位，避免把"髒"狀態留給下一次進入。

type Listener = () => void

let dirty = false
const listeners = new Set<Listener>()

export function getEngineDraftDirty(): boolean {
  return dirty
}

export function subscribeEngineDraft(listener: Listener): () => void {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

export function setEngineDraftDirty(next: boolean): void {
  if (dirty === next) return
  dirty = next
  for (const listener of listeners) listener()
}
