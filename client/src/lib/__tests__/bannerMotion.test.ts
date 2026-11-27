import { afterEach, describe, expect, it, vi } from 'vitest'
import { createElement } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { bannerSourceFor, isDocumentHidden, subscribeDocumentHidden } from '../bannerMotion'
import movingSvg from '@/assets/banner.svg?raw'
import stillSvg from '@/assets/banner-static.svg?raw'

// [自訂] SC-63：橫幅在減少動態效果與視窗隱藏時用靜態版。測試環境是 node，
// 元件以伺服器端輸出檢查標記，可見性訂閱以假文件物件檢查。

function fakeDocument(state: DocumentVisibilityState) {
  const handlers = new Set<() => void>()
  return {
    visibilityState: state,
    addEventListener: vi.fn((type: string, fn: () => void) => { if (type === 'visibilitychange') handlers.add(fn) }),
    removeEventListener: vi.fn((type: string, fn: () => void) => { if (type === 'visibilitychange') handlers.delete(fn) }),
    set(next: DocumentVisibilityState) {
      this.visibilityState = next
      for (const fn of [...handlers]) fn()
    },
    get listenerCount() { return handlers.size },
  }
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('banner motion', () => {
  it('uses the still banner only while the window is hidden', () => {
    expect(bannerSourceFor(true, 'moving.svg', 'still.svg')).toBe('still.svg')
    expect(bannerSourceFor(false, 'moving.svg', 'still.svg')).toBe('moving.svg')
    expect(isDocumentHidden({ visibilityState: 'hidden' })).toBe(true)
    expect(isDocumentHidden({ visibilityState: 'visible' })).toBe(false)
    expect(isDocumentHidden(undefined)).toBe(false)
  })

  it('follows the window as it is hidden and shown again, and unsubscribes', () => {
    const doc = fakeDocument('visible')
    const seen: boolean[] = []
    const stop = subscribeDocumentHidden(doc as unknown as Document, hidden => seen.push(hidden))
    doc.set('hidden')
    doc.set('visible')
    expect(seen).toEqual([true, false])
    stop()
    expect(doc.listenerCount).toBe(0)
    doc.set('hidden')
    expect(seen).toEqual([true, false])
    expect(() => subscribeDocumentHidden(undefined, () => {})()).not.toThrow()
  })

  it('has a still banner with no animation and nothing left invisible', () => {
    const still = stillSvg
    expect(still).not.toMatch(/<animate|<set\b|<animateMotion|<animateTransform|repeatCount/)
    expect(still).not.toMatch(/(?<![-\w])opacity="0"/)
    expect(still).not.toMatch(/https?:\/\/(?!www\.w3\.org\/)/)
    expect(still).not.toMatch(/<script|foreignObject|\son\w+=/i)
    expect(movingSvg).toMatch(/<animate/)
  })

  it('renders the still banner for reduced motion, and while hidden', async () => {
    const { default: BrandBanner } = await import('@/components/BrandBanner')
    const sourceOf = (html: string) => ({
      reduced: /<source media="\(prefers-reduced-motion: reduce\)" srcSet="([^"]+)"/i.exec(html)?.[1],
      img: /<img src="([^"]+)"/.exec(html)?.[1],
    })

    vi.stubGlobal('document', fakeDocument('visible'))
    const shown = sourceOf(renderToStaticMarkup(createElement(BrandBanner)))
    expect(shown.reduced).toMatch(/banner-static\.svg/)
    expect(shown.img).toMatch(/banner\.svg/)
    expect(shown.img).not.toMatch(/static/)

    vi.stubGlobal('document', fakeDocument('hidden'))
    const hidden = sourceOf(renderToStaticMarkup(createElement(BrandBanner)))
    expect(hidden.img).toMatch(/banner-static\.svg/)
  })
})
