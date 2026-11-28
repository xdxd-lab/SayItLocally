import { beforeEach, describe, expect, it, vi } from 'vitest'

const mocks = vi.hoisted(() => ({ appendDebugLog: vi.fn() }))

// 只替換真正的落盤出口。debugLog 本體用真實實現 —— 這條測試要驗的就是它裡面那層
// 白名單過濾，mock 掉它等於什麼都沒驗。
vi.mock('../bridge', () => ({ appendDebugLog: mocks.appendDebugLog }))

import {
  addRuntimeEvent,
  AI_EVENT_OUTCOME,
  AI_EVENT_REQUEST,
  AI_LOG_SOURCE,
  clearRuntimeEvents,
  getRuntimeEvents,
} from '../debugLog'

interface MirroredPayload {
  kind?: string
  level?: string
  source?: string
  message?: string
  detail?: unknown
}

function mirrored(): MirroredPayload[] {
  return mocks.appendDebugLog.mock.calls.map((call) => call[0] as MirroredPayload)
}

function mirroredMessages(): string[] {
  return mirrored()
    .filter((p) => p.kind === 'runtime')
    .map((p) => p.message ?? '')
}

/**
 * 這次排查真正卡住的地方：`polishWithClientAi` 裡那條"低於門檻所以跳過"的 info 日誌
 * **執行了但沒落盤** —— 它的 source 是 'server' / 'local' / 'cloud_api'，三個都不在
 * debugLog 的白名單裡，於是既不進 sayit.log 也不進記憶體環形緩衝。
 *
 * 所以只斷言"我調了 addRuntimeEvent"是不夠的（那在出 bug 的版本上也全綠）。
 * 這裡斷言的是"它確實穿過了過濾層"。
 *
 * 變異驗證：把 debugLog.ts 裡 `source === AI_LOG_SOURCE && AI_KEY_EVENTS.has(...)`
 * 那兩處放行條件去掉，本檔案的前兩條用例必須失敗。若仍全綠，說明測試沒覆蓋過濾機制。
 */
describe('AI 事件必須真的穿過落盤過濾層', () => {
  beforeEach(() => {
    mocks.appendDebugLog.mockClear()
    clearRuntimeEvents()
  })

  it.each([AI_EVENT_OUTCOME, AI_EVENT_REQUEST])('%s 會被映象到主日誌', (event) => {
    addRuntimeEvent('info', AI_LOG_SOURCE, event, { operationId: 'op-x' })
    expect(mirroredMessages()).toContain(event)
  })

  it.each([AI_EVENT_OUTCOME, AI_EVENT_REQUEST])('%s 也會進記憶體執行事件（兩處判據必須同步）', (event) => {
    addRuntimeEvent('info', AI_LOG_SOURCE, event, { operationId: 'op-y' })
    const kept = getRuntimeEvents().filter((e) => e.message === event)
    expect(kept.length).toBeGreaterThan(0)
  })

  it('同來源的其它 info 事件仍然被過濾掉（放行面只有這兩個，不是整個 ai 來源）', () => {
    addRuntimeEvent('info', AI_LOG_SOURCE, 'ai.some.debug.chatter', { noise: true })
    expect(mirroredMessages()).not.toContain('ai.some.debug.chatter')
  })

  it('舊路徑那條按 provider 命名的 info 依然不落盤（說明本輪的修法是必要的）', () => {
    // 這幾個 source 就是改之前 polishWithClientAi 用的 logSource
    for (const source of ['server', 'local', 'cloud_api', 'history']) {
      addRuntimeEvent('info', source, 'AI cleanup skipped below duration threshold', {})
    }
    expect(mirroredMessages()).not.toContain('AI cleanup skipped below duration threshold')
  })

  it('warn 級別不受白名單限制（自配 AI 失敗詳情靠它落盤）', () => {
    addRuntimeEvent('warn', 'server', 'Custom AI cleanup failed; using raw ASR text', { error: 'boom' })
    expect(mirroredMessages()).toContain('Custom AI cleanup failed; using raw ASR text')
  })
})
