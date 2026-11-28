// 「點背板關閉彈窗」的判據。
//
// 為什麼不能直接在背板上掛 onClick：瀏覽器派發 click 的目標是 **mousedown 目標與
// mouseup 目標的最近公共祖先**。在彈窗裡的輸入框按下滑鼠、拖動選字時移出面板再鬆開，
// 兩者的公共祖先就是那個 `fixed inset-0` 鋪滿全屏的背板本身 —— click 直接落在背板上，
// 面板的 stopPropagation 根本不在這條冒泡路徑裡，壓不住，於是彈窗被關掉、草稿全丟。
// 使用者反饋的原話是「拖動全選時把滑鼠移到小視窗外，鬆開後視窗就自己關了」。
//
// 判据改成「**按下**那一刻就落在背板本身，并且 click 也落在背板本身」，两个条件都要。
//
// 用 `target === currentTarget` 而不是 `!panel.contains(target)`，是為了對 portal 天然
// 安全：portal 出去的浮層其 DOM target 不是背板，照樣判成「不在背板上」，不會誤關。

import { useRef, type MouseEvent } from 'react'

export type BackdropPointerEvent =
  | { type: 'mousedown'; onBackdrop: boolean }
  | { type: 'click'; onBackdrop: boolean }

/**
 * 判據本體，抽成純函式是為了能測 —— 這個倉庫沒有 jsdom / testing-library，
 * 掛在 DOM 上的那一版沒法跑。`armed` 是「上一次按下落在背板本身」。
 *
 * 四條要同時成立（都有對應單測）：
 *  · 點背板空白處（按下與 click 都在背板）→ 關閉；
 *  · 面板內按下、拖出面板鬆開 → **不關**（本次修的就是這條）；
 *  · 面板內的鍵盤 click（target 不是背板）→ 不關；
 *  · 背板上按下但拖出視窗外鬆開（沒有 click）之後，面板內的 click 也不能關 ——
 *    所以 click 一到就撤防，無論這次關不關。
 */
export function reduceBackdropDismiss(
  armed: boolean,
  event: BackdropPointerEvent,
): { armed: boolean; dismiss: boolean } {
  if (event.type === 'mousedown') {
    return { armed: event.onBackdrop, dismiss: false }
  }
  if (!armed) return { armed, dismiss: false }
  return { armed: false, dismiss: event.onBackdrop }
}

/**
 * 返回一對要掛在**背板元素**上的事件處理器。
 *
 * ⚠️ 必須掛在背板（那個 `fixed inset-0` 的 div）上，不是面板上 ——
 * `currentTarget` 要等于背板，判据才成立。
 */
export function useBackdropDismiss(onDismiss: () => void) {
  const armedRef = useRef(false)

  const step = (event: BackdropPointerEvent) => {
    const next = reduceBackdropDismiss(armedRef.current, event)
    armedRef.current = next.armed
    if (next.dismiss) onDismiss()
  }

  return {
    onMouseDown: (event: MouseEvent<HTMLElement>) => {
      step({ type: 'mousedown', onBackdrop: event.target === event.currentTarget })
    },
    onClick: (event: MouseEvent<HTMLElement>) => {
      step({ type: 'click', onBackdrop: event.target === event.currentTarget })
    },
  }
}
