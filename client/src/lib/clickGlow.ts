/**
 * [自訂] SC-63：點擊光輝。可操作元件被點擊時，在點擊位置放出幾顆與橫幅同色系的
 * 半透明光點和一顆金色星芒，約 600 ms 淡出。
 *
 * 設計約束：
 * - 只掛一個全域的 capture 階段監聽，passive，不呼叫 preventDefault／stopPropagation，
 *   不包住也不改動任何元件，焦點不受影響。
 * - 光點放在一個 `pointer-events: none`、`aria-hidden` 的覆蓋層裡，點擊會穿透。
 * - 純 CSS 動畫（index.css 的 click-glow-*），沒有 requestAnimationFrame 迴圈；閒置時光輝什麼都不跑。
 *   （這只講光輝；品牌橫幅另見 lib/bannerMotion.ts。）
 * - 一次點擊只發一次光：滑鼠點 label 時瀏覽器轉送給 input 的合成 click 不再發光；
 *   程式碼呼叫 `.click()`（isTrusted 為 false）也不發光；看不見（寬或高為 0）的元件不發光。
 * - 同時存在的光點有上限，超過就先移除最舊的。
 * - `prefers-reduced-motion: reduce` 時完全不產生；設定關閉時也不產生。
 * - 只用在主視窗；錄音懸浮窗（overlay/）不載入這個模組。
 */

export const GLOW_COLORS = ['#8b5cf6', '#3b82f6', '#22d3ee', '#f5b942'] as const
export const GLOW_DURATION_MS = 600
export const DEFAULT_MAX_PARTICLES = 30
/** 每次點擊：1 個光暈 + 4 顆光點 + 1 顆星芒。 */
export const PARTICLES_PER_BURST = 6

const INTERACTIVE_SELECTOR = [
  'button',
  'a[href]',
  'summary',
  'select',
  'input[type="checkbox"]',
  'input[type="radio"]',
  'input[type="button"]',
  'input[type="submit"]',
  '[role="button"]',
  '[role="switch"]',
  '[role="tab"]',
  '[role="menuitem"]',
  '[role="option"]',
  '[role="checkbox"]',
  '[role="radio"]',
  '[role="link"]',
  '[data-click-glow]',
].join(',')

/** 滑鼠按下後這段時間內，detail 為 0 的 click 視為同一次點擊轉送出來的，不再發光。 */
export const POINTER_CLICK_WINDOW_MS = 500

const LABELLED_CONTROL_SELECTOR = 'input,select,textarea'

export interface ClickGlowOptions {
  maxParticles?: number
  /** 測試用：亂數來源。 */
  random?: () => number
}

export interface ClickGlowController {
  setEnabled(enabled: boolean): void
  isEnabled(): boolean
  /** 目前畫面上的光點數（含光暈）。 */
  activeCount(): number
  dispose(): void
}

function prefersReducedMotion(win: Window): boolean {
  try {
    return typeof win.matchMedia === 'function' && win.matchMedia('(prefers-reduced-motion: reduce)').matches
  } catch {
    return false
  }
}

function isDisabled(element: Element): boolean {
  return (element as HTMLButtonElement).disabled === true || element.getAttribute('aria-disabled') === 'true'
}

/** 只有指向控制項（有 for）或包住控制項的 label 才算可操作；純說明文字的 label 不算。 */
function operableLabelOf(target: Element): Element | null {
  const label = target.closest('label')
  if (!label) return null
  if (label.getAttribute('for')) return label
  const control = label.querySelector(LABELLED_CONTROL_SELECTOR)
  return control && !isDisabled(control) ? label : null
}

/** 找出被點的可操作元件；停用的元件不發光。 */
export function glowTargetOf(target: EventTarget | null): Element | null {
  if (!target || typeof (target as Element).closest !== 'function') return null
  const element = (target as Element).closest(INTERACTIVE_SELECTOR) ?? operableLabelOf(target as Element)
  if (!element) return null
  if (isDisabled(element)) return null
  if (element.closest('[data-click-glow="off"]')) return null
  return element
}

export function installClickGlow(doc: Document = document, options: ClickGlowOptions = {}): ClickGlowController {
  const win = doc.defaultView ?? window
  const maxParticles = Math.max(PARTICLES_PER_BURST, options.maxParticles ?? DEFAULT_MAX_PARTICLES)
  const random = options.random ?? Math.random
  let enabled = true
  let layer: HTMLDivElement | null = null
  let lastPointerDownAt = Number.NEGATIVE_INFINITY
  const active: Array<{ node: HTMLElement; timer: ReturnType<typeof setTimeout> }> = []

  const ensureLayer = () => {
    if (layer && layer.isConnected) return layer
    layer = doc.createElement('div')
    layer.className = 'click-glow-layer'
    layer.setAttribute('aria-hidden', 'true')
    layer.style.pointerEvents = 'none'
    doc.body.appendChild(layer)
    return layer
  }

  const removeEntry = (entry: { node: HTMLElement; timer: ReturnType<typeof setTimeout> }) => {
    clearTimeout(entry.timer)
    entry.node.remove()
    const index = active.indexOf(entry)
    if (index >= 0) active.splice(index, 1)
  }

  const spawn = (host: HTMLElement, className: string, x: number, y: number, vars: Record<string, string>) => {
    const node = doc.createElement('span')
    node.className = className
    node.style.left = `${x}px`
    node.style.top = `${y}px`
    for (const [name, value] of Object.entries(vars)) node.style.setProperty(name, value)
    host.appendChild(node)
    const entry = { node, timer: setTimeout(() => removeEntry(entry), GLOW_DURATION_MS + 50) }
    active.push(entry)
  }

  const burst = (x: number, y: number) => {
    if (!enabled || prefersReducedMotion(win)) return
    while (active.length > 0 && active.length + PARTICLES_PER_BURST > maxParticles) removeEntry(active[0])
    const host = ensureLayer()
    spawn(host, 'click-glow-halo', x, y, { '--glow-color': GLOW_COLORS[0] })
    for (let i = 0; i < PARTICLES_PER_BURST - 2; i++) {
      const angle = (i / (PARTICLES_PER_BURST - 2)) * Math.PI * 2 + random() * 0.9
      const distance = 14 + random() * 14
      spawn(host, 'click-glow-spark', x, y, {
        '--glow-color': GLOW_COLORS[i % 3],
        '--glow-dx': `${(Math.cos(angle) * distance).toFixed(1)}px`,
        '--glow-dy': `${(Math.sin(angle) * distance).toFixed(1)}px`,
      })
    }
    spawn(host, 'click-glow-star', x, y, { '--glow-color': GLOW_COLORS[3] })
  }

  const onPointerDown = (event: PointerEvent) => {
    if (event.isTrusted === false) return
    if (event.button !== undefined && event.button !== 0) return
    lastPointerDownAt = Date.now()
    if (!glowTargetOf(event.target)) return
    burst(event.clientX, event.clientY)
  }

  // 鍵盤（Enter／Space）觸發的 click：detail 為 0，在元件中心發光。
  // 滑鼠點 label 時，瀏覽器轉送給 input 的合成 click 也是 detail 0；剛按過滑鼠就略過，避免二次發光。
  const onClick = (event: MouseEvent) => {
    if (event.detail !== 0 || event.isTrusted === false) return
    if (Date.now() - lastPointerDownAt < POINTER_CLICK_WINDOW_MS) return
    const element = glowTargetOf(event.target)
    if (!element) return
    const rect = element.getBoundingClientRect()
    if (!(rect.width > 0 && rect.height > 0)) return
    burst(rect.left + rect.width / 2, rect.top + rect.height / 2)
  }

  const listenerOptions: AddEventListenerOptions = { capture: true, passive: true }
  doc.addEventListener('pointerdown', onPointerDown, listenerOptions)
  doc.addEventListener('click', onClick, listenerOptions)

  return {
    setEnabled(next) {
      enabled = next
      if (!next) while (active.length > 0) removeEntry(active[0])
    },
    isEnabled: () => enabled,
    activeCount: () => active.length,
    dispose() {
      doc.removeEventListener('pointerdown', onPointerDown, listenerOptions)
      doc.removeEventListener('click', onClick, listenerOptions)
      while (active.length > 0) removeEntry(active[0])
      layer?.remove()
      layer = null
    },
  }
}
