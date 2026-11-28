import { useLayoutEffect, useRef, type RefObject } from 'react'

/**
 * 膠囊在狀態之間"變形"而不是硬切（錄音 → 處理中 → 提示）。
 *
 * 以前膠囊寬度跟著內容瞬間跳變：波形條一收、換成"處理中"，整顆膠囊一幀之內
 * 縮掉三分之一，看著像換了個視窗。現在做兩件事：
 *   1. 寬度從舊值彈簧過渡到新值（WAAPI 動 width）；
 *   2. 新內容從輕微模糊裡浮出來（CSS 類 overlay-pill-content-enter）。
 *
 * 為什麼直接動 width 而不是 transform/clip-path：膠囊是 overflow:hidden 的圓角
 * 容器，scaleX 會把圓角和文字一起壓扁；clip-path 方案要讓底色比膠囊更寬，就得把
 * overflow:hidden 挪走，而它正是"文字再長也不撐破圓角"的那道保護。這個元素只有
 * 幾十個節點、獨佔一個視窗，逐幀佈局的代價可以忽略。
 *
 * 只在**同一次顯示**之內變形：換了 presentationId 說明視窗剛被重新彈出，
 * 那時膠囊有自己的入場動畫（overlay-pill-surface-enter），再疊一層寬度動畫就亂了。
 * 錄音的 waiting → listening 共用一個 key，它們之間的展開由 clip-path 負責
 * （見 index.css 的 overlay-pill-recording-*），這裡不插手。
 */

// 阻尼比 0.76 的彈簧取樣（約 2.5% 過沖，500ms 內收斂）。生成方式：
// x(t) = 1 - e^(-ζωt)(cos(ω_d t) + ζω/ω_d · sin(ω_d t))，ω = 6/(ζ·0.5s)，取 31 个点。
const SPRING_EASING =
  'linear(0, 0.03, 0.106, 0.207, 0.32, 0.434, 0.543, 0.642, 0.728, 0.802, 0.862, 0.91, 0.948, 0.976, 0.996, 1.01, 1.019, 1.023, 1.025, 1.025, 1.024, 1.021, 1.018, 1.015, 1.013, 1.01, 1.008, 1.006, 1.004, 1.003, 1)'
const SPRING_DURATION_MS = 500
// 不認識 linear() 的舊核心（WebView2 < 113、Safari < 17.2）退回同樣快收斂、無過沖的曲線。
const FALLBACK_EASING = 'cubic-bezier(0.16, 1, 0.3, 1)'
const FALLBACK_DURATION_MS = 360

export function prefersReducedMotion(): boolean {
  return typeof window !== 'undefined'
    && typeof window.matchMedia === 'function'
    && window.matchMedia('(prefers-reduced-motion: reduce)').matches
}

function isAnimating(animation: Animation): boolean {
  return animation.playState === 'running' || animation.playState === 'paused'
}

function animateWidth(el: HTMLElement, from: number, to: number): Animation | null {
  if (typeof el.animate !== 'function') return null
  const keyframes = [{ width: `${from}px` }, { width: `${to}px` }]
  try {
    return el.animate(keyframes, { duration: SPRING_DURATION_MS, easing: SPRING_EASING })
  } catch {
    // linear() 不被支援時 animate 會直接拋 TypeError，而不是靜默忽略。
    return el.animate(keyframes, { duration: FALLBACK_DURATION_MS, easing: FALLBACK_EASING })
  }
}

/**
 * @param morphKey 寬度該不該變形的標識：變了才彈。同一狀態裡的逐幀重新整理（波形、計時）必須產出同一個 key。
 * @param contentKey 內容該不該重新入場的標識，預設等於 morphKey。兩者分開是因為有的變化只該動寬度、
 *   不該把內容重掛一遍：「處理中」→「仍在等待結果」時進度條必須接著走，重掛會讓它從零開始。
 * @returns animateContent —— 當前這份內容是否是變形換上來的（是則應播放內容入場）；
 *   enteredFrom —— 換上來時被替換掉的那份內容的 contentKey（用來決定要不要播"波形收成進度條"）。
 */
export function usePillMorph(
  ref: RefObject<HTMLElement | null>,
  morphKey: string,
  presentationId: number,
  contentKey: string = morphKey,
): { animateContent: boolean; enteredFrom: string | null } {
  const committedRef = useRef<{ key: string | null; contentKey: string | null; presentationId: number }>({
    key: null,
    contentKey: null,
    presentationId,
  })
  // 沒在動畫時的自然寬度。變形開始那一刻 DOM 已經換成新內容，舊寬度只能從這裡拿。
  const settledWidthRef = useRef<number | null>(null)
  const animationRef = useRef<Animation | null>(null)
  const enteredRef = useRef<{ contentKey: string; from: string } | null>(null)

  // 渲染期判定（refs 還是上一次提交的值）：contentKey 變了、且仍是同一次顯示 → 這份新內容要入場。
  // 結論按 key 記住，同一狀態裡後續的重渲染不會把入場類摘掉（摘掉會讓動畫中途跳到終點）。
  const committed = committedRef.current
  if (committed.presentationId !== presentationId) {
    enteredRef.current = null
  } else if (committed.contentKey !== null && committed.contentKey !== contentKey) {
    enteredRef.current = { contentKey, from: committed.contentKey }
  }

  useLayoutEffect(() => {
    const el = ref.current
    const prev = committedRef.current
    committedRef.current = {
      key: el ? morphKey : null,
      contentKey: el ? contentKey : null,
      presentationId,
    }

    // 打斷：上一段還沒彈完就又換了內容，從**當前畫面上的寬度**接著彈，不跳回起點。
    // 已經 finished 的不算在彈：onfinish 是非同步派發的，ref 可能還沒清，這時量到的
    // 是新內容的自然寬度，會把這次變形誤判成"寬度沒變"。
    let fromWidth = settledWidthRef.current
    if (animationRef.current) {
      if (el && isAnimating(animationRef.current)) fromWidth = el.getBoundingClientRect().width
      animationRef.current.cancel()
      animationRef.current = null
    }

    if (!el) {
      settledWidthRef.current = null
      return
    }
    const toWidth = el.getBoundingClientRect().width
    settledWidthRef.current = toWidth
    el.style.removeProperty('--pill-content-delay')

    const shouldMorph = prev.key !== null
      && prev.key !== morphKey
      && prev.presentationId === presentationId
      && fromWidth !== null
      && Math.abs(fromWidth - toWidth) >= 1
      && !prefersReducedMotion()
    if (!shouldMorph || fromWidth === null) return

    // 變寬時新內容比當前膠囊寬，早早顯形會被 overflow:hidden 切掉兩頭（實測 120ms 時
    // 「網路連線已斷開」左右各缺半個字）。所以變寬讓字等形狀先走到九成多再出來；
    // 變窄時字本來就裝得下，跟預設的 50ms 走。改動畫屬性會即時作用到已開始的 CSS 動畫上。
    if (toWidth > fromWidth) el.style.setProperty('--pill-content-delay', '150ms')
    const animation = animateWidth(el, fromWidth, toWidth)
    if (!animation) return
    animationRef.current = animation
    const clear = () => {
      if (animationRef.current === animation) animationRef.current = null
    }
    animation.onfinish = clear
    animation.oncancel = clear
  }, [ref, morphKey, contentKey, presentationId])

  // 同一狀態裡寬度也會變（低音量警告擠掉幾根波形條、倒計時出現），所以每次提交都記一下
  // 自然寬度。必須宣告在上面那個 effect 之後：同一次提交裡它後跑，不會先把舊寬度覆蓋掉。
  useLayoutEffect(() => {
    if (animationRef.current && isAnimating(animationRef.current)) return
    const el = ref.current
    settledWidthRef.current = el ? el.getBoundingClientRect().width : null
  })

  const entered = enteredRef.current
  const animateContent = entered !== null && entered.contentKey === contentKey
  return { animateContent, enteredFrom: animateContent && entered ? entered.from : null }
}
