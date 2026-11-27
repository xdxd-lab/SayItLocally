import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import en from '@/i18n/locales/en.json'
import zhCN from '@/i18n/locales/zh-CN.json'
import {
  DEFAULT_MAX_PARTICLES,
  GLOW_DURATION_MS,
  PARTICLES_PER_BURST,
  glowTargetOf,
  installClickGlow,
} from '../clickGlow'

// [自訂] SC-63：點擊光輝。測試環境是 node，用最小的假 document 驗證行為。

interface FakeNode {
  tagName: string
  className: string
  children: FakeNode[]
  parent: FakeNode | null
  attrs: Record<string, string>
  style: Record<string, string> & { setProperty: (name: string, value: string) => void }
  readonly isConnected: boolean
  setAttribute(name: string, value: string): void
  appendChild(child: FakeNode): FakeNode
  remove(): void
}

function fakeNode(tagName: string, root: { body: FakeNode | null }): FakeNode {
  const style = {} as FakeNode['style']
  style.setProperty = (name, value) => { style[name] = value }
  const node: FakeNode = {
    tagName,
    className: '',
    children: [],
    parent: null,
    attrs: {},
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
    appendChild(child) { child.parent = node; node.children.push(child); return child },
    remove() {
      if (!node.parent) return
      node.parent.children.splice(node.parent.children.indexOf(node), 1)
      node.parent = null
    },
  }
  return node
}

/** 可被點的元件：closest 依測試給定的結果回傳。 */
function target(match: 'button' | 'plain' | 'disabled' | 'off') {
  const element = {
    disabled: match === 'disabled',
    getAttribute: () => null,
    getBoundingClientRect: () => ({ left: 10, top: 20, width: 100, height: 40 }),
    closest: (selector: string) => {
      if (selector === '[data-click-glow="off"]') return match === 'off' ? element : null
      return match === 'plain' ? null : element
    },
  }
  return element
}

let reduceMotion: boolean
let listeners: Map<string, Array<{ fn: (event: unknown) => void; options: unknown }>>
let doc: Document & { body: FakeNode }

beforeEach(() => {
  vi.useFakeTimers()
  reduceMotion = false
  listeners = new Map()
  const root = { body: null as FakeNode | null }
  root.body = fakeNode('body', root)
  doc = {
    body: root.body,
    defaultView: { matchMedia: (query: string) => ({ matches: query.includes('reduce') && reduceMotion }) },
    createElement: (tag: string) => fakeNode(tag, root),
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
})

function pointerDown(on: ReturnType<typeof target>, extra: Record<string, unknown> = {}) {
  const event = {
    target: on, clientX: 50, clientY: 60, button: 0,
    preventDefault: vi.fn(), stopPropagation: vi.fn(), stopImmediatePropagation: vi.fn(),
    ...extra,
  }
  for (const { fn } of listeners.get('pointerdown') ?? []) fn(event)
  return event
}

function sparks() {
  const layer = doc.body.children.find(node => node.className === 'click-glow-layer')
  return layer?.children ?? []
}

describe('click glow', () => {
  it('bursts at the pointer on an interactive element and fades out on its own', () => {
    const glow = installClickGlow(doc)
    pointerDown(target('button'))
    expect(glow.activeCount()).toBe(PARTICLES_PER_BURST)
    const nodes = sparks()
    expect(nodes.map(node => node.className)).toContain('click-glow-star')
    expect(nodes.every(node => node.style.left === '50px' && node.style.top === '60px')).toBe(true)
    vi.advanceTimersByTime(GLOW_DURATION_MS + 60)
    expect(glow.activeCount()).toBe(0)
    expect(sparks()).toHaveLength(0)
  })

  it('never blocks, redirects or claims the click, and its layer ignores the pointer', () => {
    installClickGlow(doc)
    const event = pointerDown(target('button'))
    expect(event.preventDefault).not.toHaveBeenCalled()
    expect(event.stopPropagation).not.toHaveBeenCalled()
    expect(event.stopImmediatePropagation).not.toHaveBeenCalled()
    for (const type of ['pointerdown', 'click']) {
      expect(listeners.get(type)?.[0].options).toEqual({ capture: true, passive: true })
    }
    const layer = doc.body.children[0]
    expect(layer.style.pointerEvents).toBe('none')
    expect(layer.attrs['aria-hidden']).toBe('true')
  })

  it('caps the particles on screen by dropping the oldest first', () => {
    const glow = installClickGlow(doc, { maxParticles: DEFAULT_MAX_PARTICLES })
    for (let i = 0; i < 20; i++) pointerDown(target('button'), { clientX: i })
    expect(glow.activeCount()).toBeLessThanOrEqual(DEFAULT_MAX_PARTICLES)
    expect(sparks()).toHaveLength(glow.activeCount())
    expect(sparks()[0].style.left).not.toBe('0px')
    expect(sparks()[sparks().length - 1].style.left).toBe('19px')
  })

  it('does nothing when the system asks to reduce motion', () => {
    reduceMotion = true
    const glow = installClickGlow(doc)
    pointerDown(target('button'))
    expect(glow.activeCount()).toBe(0)
    expect(doc.body.children).toHaveLength(0)
  })

  it('does nothing while switched off, and clears what is showing when switched off', () => {
    const glow = installClickGlow(doc)
    pointerDown(target('button'))
    glow.setEnabled(false)
    expect(glow.activeCount()).toBe(0)
    pointerDown(target('button'))
    expect(glow.activeCount()).toBe(0)
    glow.setEnabled(true)
    pointerDown(target('button'))
    expect(glow.activeCount()).toBe(PARTICLES_PER_BURST)
  })

  it('ignores plain areas, disabled controls, opted-out areas and non-primary buttons', () => {
    const glow = installClickGlow(doc)
    pointerDown(target('plain'))
    pointerDown(target('disabled'))
    pointerDown(target('off'))
    pointerDown(target('button'), { button: 2 })
    expect(glow.activeCount()).toBe(0)
    expect(glowTargetOf(null)).toBeNull()
  })

  it('glows at the centre of a control activated from the keyboard', () => {
    const glow = installClickGlow(doc)
    for (const { fn } of listeners.get('click') ?? []) fn({ target: target('button'), detail: 0 })
    expect(sparks()[0].style.left).toBe('60px')
    expect(sparks()[0].style.top).toBe('40px')
    // 滑鼠點擊的 click（detail ≥ 1）已在 pointerdown 發過光，不再重複。
    for (const { fn } of listeners.get('click') ?? []) fn({ target: target('button'), detail: 1 })
    expect(glow.activeCount()).toBe(PARTICLES_PER_BURST)
  })

  it('removes its listeners and layer on dispose', () => {
    const glow = installClickGlow(doc)
    pointerDown(target('button'))
    glow.dispose()
    expect(listeners.get('pointerdown')).toEqual([])
    expect(listeners.get('click')).toEqual([])
    expect(doc.body.children).toHaveLength(0)
  })

  it('has its texts in every locale', () => {
    const keys = ['brand.bannerAlt', 'brand.appeal', 'brand.appealFull', 'brand.forkNote', 'appearance.effects', 'appearance.clickGlow', 'appearance.clickGlowDesc']
    for (const table of [en, zhCN] as Array<Record<string, string>>) {
      for (const key of keys) expect(table[key]?.trim(), key).toBeTruthy()
    }
  })
})
