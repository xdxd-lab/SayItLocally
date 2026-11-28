import { beforeEach, describe, expect, it, vi } from 'vitest'

const mocks = vi.hoisted(() => ({
  updateOverlay: vi.fn(),
  presentOverlay: vi.fn(),
}))

vi.mock('../../bridge', () => ({
  updateOverlay: mocks.updateOverlay,
  presentOverlay: mocks.presentOverlay,
  hideOverlay: vi.fn(() => Promise.resolve()),
  setEscapeActionMode: vi.fn(() => Promise.resolve()),
  copyText: vi.fn(() => Promise.resolve()),
}))
vi.mock('../../debugLog', () => ({ addRuntimeEvent: vi.fn() }))
vi.mock('../../store', () => ({
  getSetting: vi.fn((_key: string, fallback: unknown) => Promise.resolve(fallback)),
}))
vi.mock('@/i18n', () => ({
  getLocale: () => 'zh-TW',
  t: (key: string) => key,
}))

// vi.mock 會被提升到 import 之前，所以這裡用普通 import 也拿到的是上面那些替身。
// 刻意不用 `await import()`：頂層 await 會讓 vitest 在收集階段丟掉 describe 的上下文
// （報 "Cannot read properties of undefined (reading 'config')"，看著像配置壞了）。
import { OverlayService } from '../OverlayService'

type Payload = Record<string, unknown>

function payloadsFrom(): Payload[] {
  return mocks.updateOverlay.mock.calls.map((call) => call[0] as Payload)
}

/**
 * 2026-09「開著即時字幕、不說話時懸浮窗一閃一閃」的迴歸測試。
 *
 * 機制：`streaming` 是原生側判斷懸浮窗佈局的唯一依據。低音量警告每 5 秒發一次
 * listening 更新，那幾個 payload 各自手寫、全都漏了這個欄位，於是視窗從流式尺寸
 * 收縮一幀再被下一次心跳撐回去。
 *
 * 所以這裡斷言的不是"警告能顯示"，而是"每一次 listening 更新都帶著佈局判據" ——
 * 前者在出 bug 的那個版本上也全綠。
 */
describe('listening 更新必须始终携带布局判据', () => {
  const emitters: Array<[string, (service: InstanceType<typeof OverlayService>) => void]> = [
    ['showLowVolumeWarning', (s) => s.showLowVolumeWarning()],
    ['showNoSignalWarning', (s) => s.showNoSignalWarning()],
    ['showMicMutedAlert', (s) => s.showMicMutedAlert()],
    ['clearWarning', (s) => s.clearWarning()],
    ['showTimeoutWarning', (s) => s.showTimeoutWarning()],
    ['pushListeningBars', (s) => s.pushListeningBars([3, 5, 8], true)],
  ]

  beforeEach(() => {
    mocks.updateOverlay.mockClear()
    mocks.presentOverlay.mockClear()
  })

  it.each(emitters)('%s 带上 streaming 与 streamingText', (_name, emit) => {
    const service = new OverlayService(() => 7)
    service.setStreamingActive(true)
    service.setStreamingText('实时字幕中')

    emit(service)

    const payloads = payloadsFrom()
    expect(payloads.length).toBeGreaterThan(0)
    for (const payload of payloads) {
      expect(payload).toMatchObject({
        state: 'listening',
        streaming: true,
        streamingText: '实时字幕中',
      })
    }
  })

  it.each(emitters)('%s 在未开启流式时不硬塞 streaming 字段', (_name, emit) => {
    const service = new OverlayService(() => 7)
    service.setStreamingActive(false)

    emit(service)

    for (const payload of payloadsFrom()) {
      expect(payload.state).toBe('listening')
      expect('streaming' in payload).toBe(false)
    }
  })

  /**
   * 釘的是"走了統一入口"，不只是"這一版欄位恰好對"。
   *
   * 差別在於：某個方法哪天被改回手寫字面量、當時抄得還挺全，上面那批斷言照樣全綠，
   * 但它從此不再跟著 listeningPayload 演進 —— 下次往 payload 里加欄位又會漏掉它一個。
   * 這條能當場把那種退化照出來。
   */
  it('所有 listening 出口都经由 listeningPayload，而非各自复制一份', () => {
    const service = new OverlayService(() => 7)
    const spy = vi.spyOn(
      service as unknown as { listeningPayload: (overrides?: Record<string, unknown>) => unknown },
      'listeningPayload',
    )
    service.setStreamingActive(true)

    for (const [name, emit] of emitters) {
      spy.mockClear()
      emit(service)
      expect(spy, `${name} 绕过了 listeningPayload`).toHaveBeenCalled()
    }
  })
})

/**
 * 錄音時懸浮窗只該收到約 30 次/秒的更新：PCM 在流動時由波形那一路驅動，
 * 定時器只在長時間沒人發過時補位。原來兩路各自 33ms，合起來接近 60 次/秒。
 *
 * 斷言必須落在「帶 bars 的幀」上，不能只數總次數：0.2.3 裡定時器與波形共用 33ms 閾值，
 * 每輪都是定時器先搶到名額，總數 30 次/秒照樣達標，波形卻一幀沒發出去（issue #90）。
 */
describe('listening 更新频率', () => {
  beforeEach(() => {
    mocks.updateOverlay.mockClear()
    mocks.presentOverlay.mockClear()
    vi.useFakeTimers()
  })

  // 3ms ≈ AudioWorklet 每 128 取樣一幀（48kHz）；8/10ms ≈ 音訊執行緒按塊批次投遞
  it.each([3, 8, 10])('PCM 每 %dms 一帧时，更新全部由波形那一路发出', (stepMs) => {
    const service = new OverlayService(() => 7)
    service.startListeningTicker(0)
    mocks.updateOverlay.mockClear()
    for (let t = 0; t < 1000; t += stepMs) {
      vi.advanceTimersByTime(stepMs)
      service.pushListeningBars([3, 5, 8])
    }
    const payloads = payloadsFrom()
    const withBars = payloads.filter((p) => Array.isArray(p.bars)).length
    expect(withBars).toBeGreaterThanOrEqual(24)
    expect(payloads.length).toBeLessThanOrEqual(32)
    // 定時器一次都不該插進來：PCM 流動時它發的幀只會佔掉波形的節流名額
    expect(payloads.length - withBars).toBe(0)
    service.stopListeningTicker()
    vi.useRealTimers()
  })

  it('没有 PCM 时定时器照样推进计时', () => {
    const service = new OverlayService(() => 7)
    service.startListeningTicker(0)
    mocks.updateOverlay.mockClear()
    vi.advanceTimersByTime(1000)
    const count = mocks.updateOverlay.mock.calls.length
    expect(count).toBeGreaterThanOrEqual(3)
    expect(count).toBeLessThanOrEqual(5)
    service.stopListeningTicker()
    vi.useRealTimers()
  })
})

/**
 * 提示音播完必須把 AudioContext 掛起：running 狀態的 AudioContext 空閒時也持續耗 CPU，
 * 正式版從第一次錄音起一直多耗約 1.8% 個核（2026-10 實測）。
 */
describe('就绪提示音的 AudioContext', () => {
  class FakeOscillator {
    onended: (() => void) | null = null
    frequency = { value: 0 }
    type = ''
    connect() {}
    start() {}
    stop() {}
  }
  class FakeAudioContext {
    static instances: FakeAudioContext[] = []
    state: 'running' | 'suspended' | 'closed' = 'running'
    currentTime = 0
    destination = {}
    oscillators: FakeOscillator[] = []
    suspend = vi.fn(async () => { this.state = 'suspended' })
    resume = vi.fn(async () => { this.state = 'running' })
    constructor() { FakeAudioContext.instances.push(this) }
    createOscillator() { const o = new FakeOscillator(); this.oscillators.push(o); return o }
    createGain() { return { connect() {}, gain: { setValueAtTime() {}, exponentialRampToValueAtTime() {} } } }
  }

  beforeEach(() => {
    FakeAudioContext.instances = []
    vi.stubGlobal('AudioContext', FakeAudioContext)
  })

  it('播完挂起，下次播放前恢复，只建一个 AudioContext', async () => {
    const service = new OverlayService(() => 0)
    service.startListeningTicker(0)
    service.stopListeningTicker()
    const ctx = FakeAudioContext.instances[0]
    expect(ctx).toBeDefined()
    ctx.oscillators[0].onended?.()
    expect(ctx.suspend).toHaveBeenCalledTimes(1)
    expect(ctx.state).toBe('suspended')

    service.startListeningTicker(0)
    service.stopListeningTicker()
    await Promise.resolve()
    await Promise.resolve()
    expect(ctx.resume).toHaveBeenCalledTimes(1)
    expect(FakeAudioContext.instances).toHaveLength(1)
    expect(ctx.oscillators).toHaveLength(2)
    vi.unstubAllGlobals()
  })
})

/**
 * 自定義配色必須隨每幀下發，不管當前主題是不是 custom：原生側對 payload 是整份替換，
 * 只在部分幀裡帶的話，重放底稿時懸浮窗會回落到預設配色。
 */
describe('自定义波形配色', () => {
  beforeEach(() => {
    mocks.updateOverlay.mockClear()
  })

  it('listening 更新带着规整过的 customWave', async () => {
    const service = new OverlayService(() => 7)
    await service.refreshSettings()
    service.pushListeningBars([3, 5, 8], true)
    const payload = payloadsFrom()[0]
    expect(payload.customWave).toEqual({ mode: 'gradient', from: '#a78bfa', to: '#f472b6' })
  })
})
