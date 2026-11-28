/**
 * macOS 上讓網頁視窗的行為更像原生應用。只在 mac 的視窗入口裡呼叫（main.tsx、overlay/main.tsx）。
 *
 * - 給 <html> 加上 `platform-mac`：index.css 裡 mac 專屬的樣式（系統捲軸、介面文字不可選、
 *   按鈕用箭頭游標）都掛在這個 class 下，Windows 沒有它，樣式與原來完全一致。
 * - 右鍵選單：WebKit 預設在空白處彈「重新載入」、在連結上彈「開啟連結」（會把主視窗導航到外部
 *   網頁）。只在輸入框裡、或者已經選中了文字時保留系統的右鍵選單（剪下 / 拷貝 / 貼上、查詢、翻譯）。
 */

/** 可編輯元素：右鍵要能剪下、拷貝、貼上 */
const EDITABLE_SELECTOR = 'input, textarea, select, [contenteditable]:not([contenteditable="false"])'

interface ClosestCapable {
  closest(selector: string): unknown
}

function canQueryClosest(target: unknown): target is ClosestCapable {
  return typeof target === 'object' && target !== null
    && typeof (target as { closest?: unknown }).closest === 'function'
}

/** 這次右鍵要不要交給系統彈選單。 */
export function shouldShowNativeContextMenu(target: EventTarget | null, selectedText: string): boolean {
  if (selectedText.trim().length > 0) return true
  if (!canQueryClosest(target)) return false
  return Boolean(target.closest(EDITABLE_SELECTOR))
}

let installed = false

export function installMacWebviewBehavior(): void {
  if (installed || typeof document === 'undefined') return
  installed = true
  document.documentElement.classList.add('platform-mac')
  window.addEventListener('contextmenu', (event) => {
    const selectedText = window.getSelection()?.toString() ?? ''
    if (!shouldShowNativeContextMenu(event.target, selectedText)) {
      event.preventDefault()
    }
  })
}
