/**
 * 元件裡取翻譯函式。
 *
 * 訂閱 locale 而不是把整棵樹塞進 Context：切語言時靠 useSyncExternalStore 觸發
 * 重渲染，任何元件（含懸浮窗那個獨立 React 根）都能單獨用，不依賴 Provider 位置。
 */
import { useSyncExternalStore } from 'react'
import { getLocale, subscribeLocale, t, type Locale } from '.'

/** 當前介面語言，變化時元件自動重渲染。 */
export function useLocale(): Locale {
  return useSyncExternalStore(subscribeLocale, getLocale, getLocale)
}

/** 返回 `t`。函式身份穩定，重渲染由 locale 變化驅動。 */
export function useT(): typeof t {
  useLocale()
  return t
}
