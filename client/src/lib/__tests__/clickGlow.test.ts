import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import en from '@/i18n/locales/en.json'
import zhTW from '@/i18n/locales/zh-TW.json'
import {
  DEFAULT_MAX_PARTICLES,
  GLOW_DURATION_MS,
  PARTICLES_PER_BURST,
  POINTER_CLICK_WINDOW_MS,
  glowTargetOf,
  installClickGlow,
} from '../clickGlow'

// [自訂] SC-63：點擊光輝。測試環境是 node，用最小的假文件物件驗證行為。
// 假元素的 closest／querySelector 依選擇器實際比對（標籤、[屬性]、[屬性="值"] 與其組合），
// 不把元件判斷 mock 掉。

const store = vi.hoisted(() => ({
  saved: new Map<string, unknown>(),
  getSetting: vi.fn(),
  setSetting: vi.fn(),
}))
vi.mock('@/services/store', () => ({
  getSetting: store.getSetting,
  setSetting: store.setSetting,
}))

interface FakeNode {
  tagName: string
  className: string
  children: FakeNode[]
  parent: FakeNode | null
  attrs: Record<string, string>
  disabled?: boolean
  rect: { left: number; top: number; width: number; height: number }
  style: Record<string, string> & { setProperty: (name: string, value: string) => void }
  readonly isConnected: boolean
  setAttribute(name: string, value: string): void
  getAttribute(name: string): string | null
  appendChild(child: FakeNode): FakeNode
  remove(): void
  matches(selector: string): boolean
  closest(selector: string): FakeNode | null
  querySelector(selector: string): FakeNode | null
  getBoundingClientRect(): FakeNode['rect']
}

const SIMPLE_SELECTOR = /^([a-z]*)((?:\[[^\]]+\])*)$/
const ATTR_PART = /\[([\w-]+)(?:="([^"]*)")?\]/g

function matchesOne(node: FakeNode, selector: string): boolean {
  if (selector === '*') return true
  const parts = SIMPLE_SELECTOR.exec(selector)
  if (!parts) throw new Error(`fake DOM cannot parse selector: ${selector}`)
  if (parts[1] && parts[1] !== node.tagName) return false
  for (const [, name, value] of parts[2].matchAll(ATTR_PART)) {
    if (!(name in node.attrs)) return false
    if (value !== undefined && node.attrs[name] !== value) return false
  }
  return true
}

let root: { body: FakeNode | null }

function el(tagName: string, attrs: Record<string, string> = {}, children: FakeNode[] = []): FakeNode {
  const style = {} as FakeNode['style']
  style.setProperty = (name, value) => { style[name] = value }
  const node: FakeNode = {
    tagName,
    className: '',
    children: [],
    parent: null,
    attrs: { ...attrs },
    rect: { left: 10, top: 20, width: 100, height: 40 },
    style,
    get isConnected() {
      let current: FakeNode | null = node
      while (current) {
        if (current === root.body) return true
        current = current.parent
      }
      return false
    },
    setAttribute(name, value) { node.attrs[name] = value },
    getAttribute(name) { return name in node.attrs ? node.attrs[name] : null },
    appendChild(child) { child.parent = node; node.children.push(child); return child },
    remove() {
      if (!node.parent) return
      node.parent.children.splice(node.parent.children.indexOf(node), 1)
      node.parent = null
    },
    matches(selector) { return selector.split(',').some(one => matchesOne(node, one.trim())) },
    closest(selector) {
      let current: FakeNode | null = node
      while (current) {
        if (current.matches(selector)) return current
        current = current.parent
      }
      return null
    },
    querySelector(selector) {
      for (const child of node.children) {
        if (child.matches(selector)) return child
        const found = child.querySelector(selector)
        if (found) return found
      }
      return null
    },
    getBoundingClientRect: () => node.rect,
  }
  for (const child of children) node.appendChild(child)
  return node
}

/** 把元件掛進 <main>，回傳最內層要點的節點。 */
function mount(tree: FakeNode, innermost: (tree: FakeNode) => FakeNode = t => t): FakeNode {
  const main = el('main', {}, [el('div', {}, [tree])])
  root.body!.appendChild(main)
  return innermost(tree)
}

const targetOf = (node: FakeNode | null) => glowTargetOf(node as unknown as EventTarget)
const text = () => el('span')
const button = (attrs: Record<string, string> = {}) => el('button', attrs, [text()])

let reduceMotion: boolean
let listeners: Map<string, Array<{ fn: (event: unknown) => void; options: unknown }>>
let doc: Document & { body: FakeNode }

beforeEach(() => {
  vi.useFakeTimers()
  reduceMotion = false
  listeners = new Map()
  root = { body: null }
  root.body = el('body')
  doc = {
    body: root.body,
    defaultView: { matchMedia: (query: string) => ({ matches: query.includes('reduce') && reduceMotion }) },
    createElement: (tag: string) => el(tag),
    addEventListener: (type: string, fn: (event: unknown) => void, options: unknown) => {
      listeners.set(type, [...(listeners.get(type) ?? []), { fn, options }])
    },
    removeEventListener: (type: string, fn: (event: unknown) => void) => {
      listeners.set(type, (listeners.get(type) ?? []).filter(entry => entry.fn !== fn))
    },
  } as unknown as Document & { body: FakeNode }
})

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

function pointerDown(on: FakeNode, extra: Record<string, unknown> = {}) {
  const event = {
    target: on, clientX: 50, clientY: 60, button: 0, isTrusted: true,
    preventDefault: vi.fn(), stopPropagation: vi.fn(), stopImmediatePropagation: vi.fn(),
    ...extra,
  }
  for (const { fn } of listeners.get('pointerdown') ?? []) fn(event)
  return event
}

function click(on: FakeNode, extra: Record<string, unknown> = {}) {
  const event = { target: on, detail: 0, isTrusted: true, ...extra }
  for (const { fn } of listeners.get('click') ?? []) fn(event)
}

function sparks() {
  const layer = doc.body.children.find(node => node.className === 'click-glow-layer')
  return layer?.children ?? []
}

describe('click glow', () => {
  it('bursts at the pointer on an interactive element and fades out on its own', () => {
    const glow = installClickGlow(doc)
    pointerDown(mount(button(), b => b.children[0]))
    expect(glow.activeCount()).toBe(PARTICLES_PER_BURST)
    const nodes = sparks()
    expect(nodes.map(node => node.className)).toContain('click-glow-star')
    expect(nodes.every(node => node.style.left === '50px' && node.style.top === '60px')).toBe(true)
    vi.advanceTimersByTime(GLOW_DURATION_MS + 60)
    expect(glow.activeCount()).toBe(0)
    expect(sparks()).toHaveLength(0)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('never blocks, redirects or claims the click, and its layer ignores the pointer', () => {
    installClickGlow(doc)
    const event = pointerDown(mount(button()))
    expect(event.preventDefault).not.toHaveBeenCalled()
    expect(event.stopPropagation).not.toHaveBeenCalled()
    expect(event.stopImmediatePropagation).not.toHaveBeenCalled()
    for (const type of ['pointerdown', 'click']) {
      expect(listeners.get(type)?.[0].options).toEqual({ capture: true, passive: true })
    }
    const layer = doc.body.children.find(node => node.className === 'click-glow-layer')!
    expect(layer.style.pointerEvents).toBe('none')
    expect(layer.attrs['aria-hidden']).toBe('true')
  })

  it('caps the particles on screen by dropping the oldest first', () => {
    const glow = installClickGlow(doc, { maxParticles: DEFAULT_MAX_PARTICLES })
    const target = mount(button())
    for (let i = 0; i < 20; i++) pointerDown(target, { clientX: i })
    expect(glow.activeCount()).toBeLessThanOrEqual(DEFAULT_MAX_PARTICLES)
    expect(sparks()).toHaveLength(glow.activeCount())
    expect(sparks()[0].style.left).not.toBe('0px')
    expect(sparks()[sparks().length - 1].style.left).toBe('19px')
    expect(vi.getTimerCount()).toBe(glow.activeCount())
  })

  it('does nothing when the system asks to reduce motion', () => {
    reduceMotion = true
    const glow = installClickGlow(doc)
    pointerDown(mount(button()))
    expect(glow.activeCount()).toBe(0)
    expect(sparks()).toHaveLength(0)
  })

  it('does nothing while switched off, and clears what is showing when switched off', () => {
    const glow = installClickGlow(doc)
    const target = mount(button())
    pointerDown(target)
    glow.setEnabled(false)
    expect(glow.activeCount()).toBe(0)
    expect(vi.getTimerCount()).toBe(0)
    pointerDown(target)
    expect(glow.activeCount()).toBe(0)
    glow.setEnabled(true)
    pointerDown(target)
    expect(glow.activeCount()).toBe(PARTICLES_PER_BURST)
  })

  it('picks the nearest control by the real selector, not every element', () => {
    const inside = mount(el('div', { class: 'card' }, [el('p', {}, [text()])]), t => t.children[0].children[0])
    expect(targetOf(inside)).toBeNull()
    const nested = mount(button(), b => b.children[0])
    expect(targetOf(nested)).toBe(nested.parent)
    for (const control of [
      el('a', { href: '#x' }),
      el('summary'),
      el('select'),
      el('input', { type: 'checkbox' }),
      el('div', { role: 'switch' }),
      el('div', { role: 'tab' }),
      el('div', { 'data-click-glow': '' }),
    ]) {
      expect(targetOf(mount(control)), JSON.stringify([control.tagName, control.attrs])).toBe(control)
    }
    for (const passive of [el('a'), el('input', { type: 'text' }), el('div', { role: 'presentation' })]) {
      expect(targetOf(mount(passive)), JSON.stringify([passive.tagName, passive.attrs])).toBeNull()
    }
  })

  it('glows on labels that operate a control, and not on caption labels', () => {
    const wrapping = el('label', {}, [el('input', { type: 'checkbox' }), text()])
    expect(targetOf(mount(wrapping, l => l.children[1]))).toBe(wrapping)
    const pointing = el('label', { for: 'mic' }, [text()])
    expect(targetOf(mount(pointing, l => l.children[0]))).toBe(pointing)
    const caption = el('label', {}, [text()])
    expect(targetOf(mount(caption, l => l.children[0]))).toBeNull()
    const disabledControl = el('input', { type: 'checkbox' })
    disabledControl.disabled = true
    expect(targetOf(mount(el('label', {}, [disabledControl, text()]), l => l.children[1]))).toBeNull()
  })

  it('ignores plain areas, disabled controls, opted-out areas and non-primary buttons', () => {
    const glow = installClickGlow(doc)
    pointerDown(mount(el('div', {}, [text()]), d => d.children[0]))
    const disabled = button()
    disabled.disabled = true
    pointerDown(mount(disabled))
    pointerDown(mount(button({ 'aria-disabled': 'true' }), b => b.children[0]))
    pointerDown(mount(el('section', { 'data-click-glow': 'off' }, [button()]), s => s.children[0]))
    pointerDown(mount(button()), { button: 2 })
    expect(glow.activeCount()).toBe(0)
    expect(targetOf(null)).toBeNull()
    expect(targetOf(mount(button({ 'aria-disabled': 'false' })))).not.toBeNull()
  })

  it('glows at the centre of a control activated from the keyboard', () => {
    const glow = installClickGlow(doc)
    const target = mount(button())
    click(target)
    expect(sparks()[0].style.left).toBe('60px')
    expect(sparks()[0].style.top).toBe('40px')
    // 滑鼠點擊的 click（detail ≥ 1）已在 pointerdown 發過光，不再重複。
    click(target, { detail: 1 })
    expect(glow.activeCount()).toBe(PARTICLES_PER_BURST)
  })

  it('glows once when a label forwards the mouse click to its input', () => {
    const glow = installClickGlow(doc)
    const input = el('input', { type: 'checkbox' })
    const label = mount(el('label', {}, [input, text()]))
    pointerDown(label.children[1])
    click(label.children[1], { detail: 1 })
    click(input, { detail: 0 })
    expect(glow.activeCount()).toBe(PARTICLES_PER_BURST)
    expect(sparks().every(node => node.style.left === '50px')).toBe(true)
    // 過了這段時間，鍵盤觸發的 click 照常發光。
    vi.advanceTimersByTime(Math.max(POINTER_CLICK_WINDOW_MS, GLOW_DURATION_MS) + 60)
    expect(glow.activeCount()).toBe(0)
    click(input)
    expect(glow.activeCount()).toBe(PARTICLES_PER_BURST)
    expect(sparks().every(node => node.style.left === '60px')).toBe(true)
  })

  it('ignores clicks made by code and controls that take no space', () => {
    const glow = installClickGlow(doc)
    const target = mount(button())
    pointerDown(target, { isTrusted: false })
    click(target, { isTrusted: false })
    expect(glow.activeCount()).toBe(0)
    for (const rect of [{ width: 0, height: 40 }, { width: 100, height: 0 }]) {
      const hidden = mount(button())
      hidden.rect = { left: 0, top: 0, ...rect }
      click(hidden)
    }
    expect(glow.activeCount()).toBe(0)
  })

  it('removes its listeners and layer on dispose', () => {
    const glow = installClickGlow(doc)
    pointerDown(mount(button()))
    glow.dispose()
    expect(listeners.get('pointerdown')).toEqual([])
    expect(listeners.get('click')).toEqual([])
    expect(doc.body.children.some(node => node.className === 'click-glow-layer')).toBe(false)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('has its texts in every locale', () => {
    const keys = ['brand.bannerAlt', 'brand.appeal', 'brand.appealFull', 'brand.forkNote', 'appearance.effects', 'appearance.clickGlow', 'appearance.clickGlowDesc']
    for (const table of [en, zhTW] as Array<Record<string, string>>) {
      for (const key of keys) expect(table[key]?.trim(), key).toBeTruthy()
    }
  })
})

describe('click glow setting', () => {
  beforeEach(() => {
    vi.resetModules()
    store.saved.clear()
    store.getSetting.mockReset().mockImplementation(async (key: string, fallback: unknown) =>
      store.saved.has(key) ? store.saved.get(key) : fallback)
    store.setSetting.mockReset().mockImplementation(async (key: string, value: unknown) => { store.saved.set(key, value) })
    vi.stubGlobal('document', doc)
  })

  async function settle() {
    await vi.runAllTimersAsync()
  }

  it('applies the saved off switch when the app starts', async () => {
    const { CLICK_GLOW_SETTING_KEY, startClickGlow } = await import('@/stores/clickGlow')
    store.saved.set(CLICK_GLOW_SETTING_KEY, false)
    const stop = startClickGlow()
    await settle()
    pointerDown(mount(button()))
    expect(sparks()).toHaveLength(0)
    stop()
    expect(listeners.get('pointerdown')).toEqual([])
  })

  it('is on by default and when the saved setting cannot be read', async () => {
    const { startClickGlow } = await import('@/stores/clickGlow')
    store.getSetting.mockRejectedValueOnce(new Error('store unavailable'))
    const stop = startClickGlow()
    await settle()
    pointerDown(mount(button()))
    expect(sparks()).toHaveLength(PARTICLES_PER_BURST)
    stop()
  })

  it('saves the switch and applies it to the running effect at once', async () => {
    const { CLICK_GLOW_SETTING_KEY, loadClickGlowEnabled, setClickGlowEnabled, startClickGlow } = await import('@/stores/clickGlow')
    const stop = startClickGlow()
    await settle()
    await setClickGlowEnabled(false)
    expect(store.setSetting).toHaveBeenCalledWith(CLICK_GLOW_SETTING_KEY, false)
    expect(await loadClickGlowEnabled()).toBe(false)
    pointerDown(mount(button()))
    expect(sparks()).toHaveLength(0)
    await setClickGlowEnabled(true)
    pointerDown(mount(button()))
    expect(sparks()).toHaveLength(PARTICLES_PER_BURST)
    stop()
  })
})
