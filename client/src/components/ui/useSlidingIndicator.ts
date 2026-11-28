import { useLayoutEffect, useRef, type RefObject } from 'react'

/**
 * 一塊"選中底色"在選項之間滑動，而不是舊格褪色、新格變色（單選膠囊 / 歷史頁篩選 / 側欄）。
 *
 * 用法：容器 `position: relative`，里面放一个 `<span ref={indicatorRef} className="sliding-indicator ..." />`
 * （`space-y-*` 容器裡要放最後，否則它會給第一個真選項加 margin-top），
 * 選項本身不畫選中底色、`position: relative` 蓋在它上面。選中項靠 `activeSelector` 找
 * （預設 `[aria-checked="true"]`，側欄用 NavLink 自帶的 `[aria-current="page"]`）。
 *
 * 位置是**直接寫 DOM**、不走 state：寫 state 會多一次渲染，而且量尺寸必須在佈局之後，
 * 繞一圈 setState 只會讓第一幀畫在錯的地方。
 *
 * 三種情況**不能滑**，只能原地出現：
 *  1. 首次定位（掛載時從 0,0 滑過來，看著像設定被改了一次 —— pitfalls 11 同一類問題）；
 *  2. 上一次沒有可見的選中項（容器 display:none、或側欄選中項跑到了另一組）；
 *  3. 呼叫方說現在不許動畫（設定值剛非同步讀回來、`animated=false`）。
 * 視窗縮放 / 字型載入導致的位移也不滑，跟著 ResizeObserver 直接貼過去。
 */
/**
 * 選中項相對容器內邊距框的位置。用 offset* 逐級累加而不是 getBoundingClientRect：
 * 彈窗入場時整塊面板帶著 scale，rect 量出來是縮小後的值，滑塊會差幾個畫素停在那裡
 * （transform 不觸發 ResizeObserver，事後也不會被糾正）。offset* 不受 transform 影響。
 * 選項外面包了一層定位元素（如 Tooltip 的包裹）時，offsetParent 是那一層，所以要一路加上去。
 */
function offsetWithin(el: HTMLElement, container: HTMLElement): { x: number; y: number } {
  let x = 0
  let y = 0
  let node: HTMLElement | null = el
  while (node && node !== container) {
    x += node.offsetLeft
    y += node.offsetTop
    const parent = node.offsetParent as HTMLElement | null
    if (parent && parent !== container) {
      x += parent.clientLeft
      y += parent.clientTop
    }
    node = parent
  }
  if (node === container) return { x, y }
  // 容器不是 offsetParent 鏈上的一環（忘了給它 position: relative）：退回按視口座標算。
  const er = el.getBoundingClientRect()
  const cr = container.getBoundingClientRect()
  return { x: er.left - cr.left - container.clientLeft, y: er.top - cr.top - container.clientTop }
}

export function useSlidingIndicator(
  containerRef: RefObject<HTMLElement | null>,
  activeKey: unknown,
  options: { animated?: boolean; activeSelector?: string } = {},
): RefObject<HTMLSpanElement> {
  const { animated = true, activeSelector = '[aria-checked="true"]' } = options
  const indicatorRef = useRef<HTMLSpanElement>(null)
  const placedRef = useRef(false)
  const fadedOutRef = useRef(false)
  const geomRef = useRef<{ x: number; y: number; w: number; h: number } | null>(null)
  const animatedRef = useRef(animated)
  animatedRef.current = animated

  useLayoutEffect(() => {
    const container = containerRef.current
    const indicator = indicatorRef.current
    if (!container || !indicator) return

    const place = (allowSlide: boolean) => {
      // 多個匹配時取最後一個：NavLink 的 "/" 在某些路由版本下會對所有路徑都算 active，
      // 真正的當前頁總在它後面。
      const matches = container.querySelectorAll<HTMLElement>(activeSelector)
      const target = matches.length > 0 ? matches[matches.length - 1] : null
      if (!target || target.offsetWidth === 0) {
        if (placedRef.current) fadedOutRef.current = true
        indicator.style.opacity = '0'
        placedRef.current = false
        geomRef.current = null
        return
      }
      const { x, y } = offsetWithin(target, container)
      const geom = { x, y, w: target.offsetWidth, h: target.offsetHeight }
      const last = geomRef.current
      // ResizeObserver 在 observe() 時**一定**先回撥一次，哪怕什麼都沒變。那一次如果也按
      // "尺寸變了、直接貼過去"處理，會把剛開始的滑動當場掐掉（實測：滑塊一格都不動）。
      // 所以只有几何真的变了才动它。
      if (!allowSlide && last && last.x === geom.x && last.y === geom.y && last.w === geom.w && last.h === geom.h) return
      geomRef.current = geom

      const slide = allowSlide && placedRef.current && animatedRef.current
      // 從"本組沒有選中項"回來（側欄在兩組之間切換）：不滑，但淡入，否則像憑空閃出來。
      const fadeIn = !placedRef.current && fadedOutRef.current && animatedRef.current
      indicator.style.transition = slide ? '' : 'none'
      indicator.style.width = `${geom.w}px`
      indicator.style.height = `${geom.h}px`
      indicator.style.transform = `translate(${geom.x}px, ${geom.y}px)`
      if (!fadeIn) indicator.style.opacity = '1'
      if (!slide) {
        // 先讓"無過渡"的位置生效，再把過渡還回去；不讀一次佈局，瀏覽器會把兩次寫合併成一次滑動。
        void indicator.offsetWidth
        indicator.style.transition = ''
        if (fadeIn) indicator.style.opacity = '1'
      }
      placedRef.current = true
      fadedOutRef.current = false
    }

    place(true)

    if (typeof ResizeObserver === 'undefined') return
    const observer = new ResizeObserver(() => place(false))
    // 子元素也要盯：切介面語言時選項文字變寬，而撐滿一行的容器自己的尺寸可能紋絲不動。
    observer.observe(container)
    for (const child of Array.from(container.children)) {
      if (child !== indicator) observer.observe(child)
    }
    return () => observer.disconnect()
  }, [containerRef, activeKey, activeSelector])

  return indicatorRef
}
