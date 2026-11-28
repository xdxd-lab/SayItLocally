/**
 * RecorderOrchestrator 主路徑的行為測試（模組 6.1：拆分之前先把現有行為釘住）。
 *
 * 不是讀原始碼斷言寫法，而是真的把一次錄音跑完：bridge 換成事件匯流排（可以發「按下 / 鬆開 / Esc」），
 * provider 換成可控的假物件，麥克風換成手動喂 PCM 幀的假採集。OverlayService、helpers、
 * 提示詞路由這些用真的，這樣拆分時它們被搬走也照樣受這些測試約束。
 *
 * 斷言的是使用者能感知到的結果：狀態回到哪、歷史裡記了什麼、插沒插字、彈的是卡片還是提示、
 * provider 收到了什麼呼叫。拆分前後這些必須原樣一致。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

type Handler = (payload?: unknown) => void

const h = vi.hoisted(() => {
  const handlers: Record<string, Handler[]> = {}
  const on = (name: string) => (cb: Handler) => {
    ;(handlers[name] ||= []).push(cb)
    return () => {}
  }
  const state = {
    handlers,
    pcm: null as null | ((frame: Int16Array) => void),
    data: null as null | ((buffer: ArrayBuffer) => void),
    probe: {
      editable: true,
      hwnd: '100',
      focusHwnd: '101',
      process: 'notepad.exe',
      probeId: 1,
      verdict: 'editable',
      detail: 'test',
    } as Record<string, unknown>,
    pasteResult: { ok: true, strategy: 'clipboard' } as Record<string, unknown>,
    settings: {} as Record<string, unknown>,
  }
  return { state, on }
})

vi.mock('../../bridge', () => {
  const fn = () => vi.fn(() => Promise.resolve(undefined))
  return {
    presentOverlay: fn(),
    showOverlay: fn(),
    hideOverlay: fn(),
    updateOverlay: fn(),
    setEscapeActionMode: fn(),
    setCardHotkeys: fn(),
    copyText: fn(),
    emit: fn(),
    listen: vi.fn(() => Promise.resolve(() => {})),
    muteSystemOutput: fn(),
    restoreSystemOutput: fn(),
    deleteAudioFile: fn(),
    getClientRuntimeInfo: vi.fn(() => Promise.resolve({ deviceId: 'test' })),
    getRecordingContext: vi.fn(() => Promise.resolve({
      appContext: { processName: 'notepad.exe', windowTitle: 'Untitled', reason: 'test' },
      probe: h.state.probe,
    })),
    getProbeResult: vi.fn(() => Promise.resolve(h.state.probe)),
    pasteText: vi.fn(() => Promise.resolve(h.state.pasteResult)),
    getOverlayHealth: fn(),
    onPTTDown: h.on('ptt-down'),
    onPTTUp: h.on('ptt-up'),
    onPTTToggle: h.on('ptt-toggle'),
    onToggleHandsFree: h.on('hands-free'),
    onPTTTimeoutWarning: h.on('timeout-warning'),
    onEscapeAction: h.on('escape'),
    onCardDismissed: h.on('card-dismissed'),
    onCardHotkey: h.on('card-hotkey'),
    notifyCardDismissed: fn(),
  }
})

vi.mock('../../audio', async (importOriginal) => ({
  ...(await importOriginal<Record<string, unknown>>()),
  startCapture: vi.fn(async (
    _mic: unknown,
    onData: (buffer: ArrayBuffer) => void,
    _onVolume: unknown,
    onPcm: (frame: Int16Array) => void,
  ) => {
    h.state.data = onData
    h.state.pcm = onPcm
    return { label: 'Test Mic', deviceId: 'default', groupId: 'g1', devices: [] }
  }),
  stopCapture: vi.fn(async () => {
    h.state.data = null
    h.state.pcm = null
  }),
}))

const provider = vi.hoisted(() => ({
  mode: 'server' as string,
  callbacks: null as null | Record<string, (...args: unknown[]) => void>,
  ready: true,
  stopAccepted: true,
  calls: [] as string[],
  connect: vi.fn(),
  start: vi.fn(),
  stop: vi.fn(),
  cancel: vi.fn(),
  sendAudio: vi.fn(),
  disconnect: vi.fn(),
  isReady: vi.fn(),
}))

vi.mock('../../transcription', () => ({
  getProvider: () => provider,
  MID_SESSION_DISCONNECT_ERROR: 'mid_session_disconnect',
}))
vi.mock('../../transcription/serverAiSource', () => ({ getRuntimeServerAiSource: () => 'managed' }))

const store = vi.hoisted(() => ({
  history: [] as Array<Record<string, unknown>>,
}))
vi.mock('../../store', async (importOriginal) => ({
  ...(await importOriginal<Record<string, unknown>>()),
  getSetting: vi.fn(async (key: string, fallback: unknown) => (key in h.state.settings ? h.state.settings[key] : fallback)),
  addHistory: vi.fn(async (record: Record<string, unknown>) => { store.history.push(record) }),
  updateHistoryRecord: vi.fn(async (id: string, patch: Record<string, unknown>) => {
    const rec = store.history.find((r) => r.id === id)
    if (rec) Object.assign(rec, patch)
  }),
  deleteHistory: vi.fn(async (id: string) => {
    store.history = store.history.filter((r) => r.id !== id)
  }),
  getPromptPresets: vi.fn(async () => []),
  getActivePresetId: vi.fn(async () => 'intent'),
  setActivePresetId: vi.fn(async () => {}),
}))
vi.mock('../../debugLog', () => ({ addRuntimeEvent: vi.fn() }))
vi.mock('../../audioFileService', () => ({
  saveRecordingAudio: vi.fn(async (id: string) => `C:/audio/${id}.wav`),
}))
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async () => undefined) }))
vi.mock('../../textInsertion', () => ({
  captureActiveInsertionTarget: vi.fn(() => 'external'),
  clearCapturedInsertionTarget: vi.fn(),
  startInsertionTargetTracking: vi.fn(),
  stopInsertionTargetTracking: vi.fn(),
  insertTextIntoCapturedOrActiveElement: vi.fn(() => ({ ok: false })),
}))
vi.mock('../../personalization/store', async () => ({
  getAppPromptRules: vi.fn(async () => []),
  getUserStats: vi.fn(async () => (await import('../../personalization/defaults')).createDefaultUserStats()),
  recordSessionStats: vi.fn(async () => (await import('../../personalization/defaults')).createDefaultUserStats()),
}))
vi.mock('../../../stores/activePreset', () => ({ setActivePresetKnown: vi.fn() }))
vi.mock('@/i18n', () => ({ t: (key: string) => key, getLocale: () => 'zh-TW' }))

import * as bridge from '../../bridge'
import { addRuntimeEvent } from '../../debugLog'
import { RecorderOrchestrator } from '../RecorderOrchestrator'

// ── 驅動工具 ──

function emit(name: string, payload?: unknown) {
  for (const cb of h.state.handlers[name] ?? []) cb(payload)
}

/** 讓排隊的 Promise 和 0ms 定時器都跑完。用真實定時器時會讓出一次事件迴圈。 */
async function flush(times = 20) {
  for (let i = 0; i < times; i++) await Promise.resolve()
  if (!vi.isFakeTimers()) await new Promise((resolve) => setTimeout(resolve, 0))
  for (let i = 0; i < times; i++) await Promise.resolve()
}

/** 喂 n 秒 16 kHz 的音訊幀（每幀 128 取樣，與 AudioWorklet 一致）。 */
function feedAudio(seconds: number, amplitude = 3000) {
  const frames = Math.round((seconds * 16000) / 128)
  for (let i = 0; i < frames; i++) {
    const frame = new Int16Array(128)
    for (let s = 0; s < frame.length; s++) frame[s] = s % 2 === 0 ? amplitude : -amplitude
    h.state.data?.(frame.buffer.slice(0))
    h.state.pcm?.(frame)
  }
}

function overlayStates(): string[] {
  const calls = [
    ...vi.mocked(bridge.presentOverlay).mock.calls,
    ...vi.mocked(bridge.updateOverlay).mock.calls,
  ]
  return calls.map((c) => String((c[0] as { state?: string })?.state ?? ''))
}

function finalResult(text: string, extra: Record<string, unknown> = {}) {
  return {
    asrText: text,
    llmText: text,
    text,
    asrMs: 120,
    llmMs: 0,
    durationSec: 2,
    ...extra,
  }
}

let orch: RecorderOrchestrator

async function startedOrchestrator() {
  orch = new RecorderOrchestrator()
  await orch.init()
  await flush()
  return orch
}

/** 按下 → 喂音訊 → 鬆開，停在 processing。 */
async function recordAndRelease(seconds = 2) {
  emit('ptt-down')
  await flush()
  if (orch.getState() !== 'recording') {
    const events = vi.mocked(addRuntimeEvent).mock.calls.map((c) => `${c[0]} ${c[1]} ${c[2]} ${JSON.stringify(c[3] ?? '').slice(0, 160)}`)
    throw new Error(`not recording; events:\n${events.slice(-15).join('\n')}`)
  }
  feedAudio(seconds)
  emit('ptt-up')
  await flush()
}

beforeEach(() => {
  for (const key of Object.keys(h.state.handlers)) delete h.state.handlers[key]
  store.history = []
  h.state.settings = {}
  h.state.probe = {
    editable: true, hwnd: '100', focusHwnd: '101', process: 'notepad.exe', probeId: 1, verdict: 'editable', detail: 'test',
  }
  h.state.pasteResult = { ok: true, strategy: 'clipboard' }
  provider.mode = 'server'
  provider.ready = true
  provider.stopAccepted = true
  provider.callbacks = null
  provider.connect.mockReset().mockImplementation(async (cbs: Record<string, (...args: unknown[]) => void>) => { provider.callbacks = cbs })
  provider.start.mockReset().mockImplementation(() => true)
  provider.stop.mockReset().mockImplementation(() => provider.stopAccepted)
  provider.cancel.mockReset()
  provider.sendAudio.mockReset()
  provider.disconnect.mockReset()
  provider.isReady.mockReset().mockImplementation(() => provider.ready)
  vi.mocked(bridge.pasteText).mockClear()
  vi.mocked(bridge.presentOverlay).mockClear()
  vi.mocked(bridge.updateOverlay).mockClear()
  vi.mocked(bridge.hideOverlay).mockClear()
  vi.mocked(bridge.deleteAudioFile).mockClear()
})

afterEach(() => {
  orch?.cleanup()
  vi.useRealTimers()
})

describe('RecorderOrchestrator 主路徑', () => {
  it('正常一次：按住說話 → 識別 → 插字 → 存歷史 → 回到空閒', async () => {
    await startedOrchestrator()
    await recordAndRelease(2)

    expect(orch.getState()).toBe('processing')
    expect(provider.start).toHaveBeenCalledTimes(1)
    expect(provider.stop).toHaveBeenCalledTimes(1)
    expect(provider.sendAudio).toHaveBeenCalled()

    provider.callbacks!.onFinal(finalResult('你好世界'))
    await flush(60)
    provider.callbacks!.onDone()
    await flush(60)

    expect(bridge.pasteText).toHaveBeenCalledTimes(1)
    expect(vi.mocked(bridge.pasteText).mock.calls[0][0]).toBe('你好世界')
    expect(store.history).toHaveLength(1)
    expect(store.history[0]).toMatchObject({ asrText: '你好世界' })
    expect(store.history[0].audioFilePath).toMatch(/\.wav$/)
    expect(orch.getState()).toBe('idle')
    expect(overlayStates()).toContain('listening')
    expect(overlayStates()).toContain('thinking')
  })
})

describe('RecorderOrchestrator 失敗與邊界路徑', () => {
  it('按住不到 0.5 秒：取消會話，不存歷史，不插字', async () => {
    await startedOrchestrator()
    await recordAndRelease(0.2)

    expect(provider.cancel).toHaveBeenCalled()
    expect(provider.stop).not.toHaveBeenCalled()
    expect(store.history).toHaveLength(0)
    expect(bridge.pasteText).not.toHaveBeenCalled()
    expect(orch.getState()).toBe('idle')
  })

  it('識別沒出文字：只給提示不彈卡片，歷史記一條空記錄（no_transcript）', async () => {
    await startedOrchestrator()
    await recordAndRelease(2)

    provider.callbacks!.onASR({ text: '', asrMs: 90, durationSec: 2 })
    await flush(60)

    expect(overlayStates()).toContain('toast')
    expect(overlayStates()).not.toContain('failure')
    expect(store.history).toHaveLength(1)
    expect(store.history[0]).toMatchObject({ isEmpty: true, failReasonCode: 'no_transcript' })
    expect(store.history[0].audioFilePath).toMatch(/\.wav$/)
    expect(bridge.pasteText).not.toHaveBeenCalled()
    expect(orch.getState()).toBe('idle')
  })

  it('處理中報錯：彈失敗卡，歷史保留音訊和失敗原因', async () => {
    await startedOrchestrator()
    await recordAndRelease(2)

    provider.callbacks!.onError('quota exceeded')
    await flush(60)

    expect(overlayStates()).toContain('failure')
    expect(store.history).toHaveLength(1)
    expect(store.history[0].isEmpty).toBe(true)
    expect(store.history[0].failReasonCode).toBeTruthy()
    expect(store.history[0].audioFilePath).toMatch(/\.wav$/)
    expect(bridge.pasteText).not.toHaveBeenCalled()
    expect(orch.getState()).toBe('idle')
  })

  it('stop 沒送出去（連線已斷）：當場判失敗，不進一分鐘的等待', async () => {
    provider.stopAccepted = false
    await startedOrchestrator()
    await recordAndRelease(2)
    await flush(60)

    expect(overlayStates()).toContain('failure')
    expect(store.history).toHaveLength(1)
    expect(store.history[0]).toMatchObject({ failReasonCode: 'connection_lost' })
    expect(orch.getState()).toBe('idle')
  })

  it('錄音中按 Esc：取消，不存歷史，不插字，提示「已取消」', async () => {
    await startedOrchestrator()
    emit('ptt-down')
    await flush()
    expect(orch.getState()).toBe('recording')
    feedAudio(1)
    emit('escape', { mode: 'cancel_recording', token: 1 })
    await flush(60)

    expect(provider.cancel).toHaveBeenCalled()
    expect(provider.stop).not.toHaveBeenCalled()
    expect(store.history).toHaveLength(0)
    expect(overlayStates()).toContain('toast')
    expect(orch.getState()).toBe('idle')

    // 取消之後的 PTT up 不應該再觸發 stop
    emit('ptt-up')
    await flush()
    expect(provider.stop).not.toHaveBeenCalled()
  })

  it('目標不可編輯：不插字，彈兜底卡片讓使用者複製，歷史照存', async () => {
    h.state.probe = { ...h.state.probe, editable: false, verdict: 'not_editable' }
    await startedOrchestrator()
    await recordAndRelease(2)

    provider.callbacks!.onFinal(finalResult('請幫我複製'))
    await flush(60)

    expect(bridge.pasteText).not.toHaveBeenCalled()
    expect(overlayStates()).toContain('fallback')
    const fallback = [...vi.mocked(bridge.presentOverlay).mock.calls, ...vi.mocked(bridge.updateOverlay).mock.calls]
      .map((c) => c[0] as Record<string, unknown>)
      .find((p) => p.state === 'fallback')
    expect(JSON.stringify(fallback)).toContain('請幫我複製')
    expect(store.history).toHaveLength(1)
    expect(store.history[0]).toMatchObject({ asrText: '請幫我複製' })
  })

  it('重複的 final 只處理一次', async () => {
    await startedOrchestrator()
    await recordAndRelease(2)

    provider.callbacks!.onFinal(finalResult('一次'))
    provider.callbacks!.onFinal(finalResult('一次'))
    await flush(60)

    expect(bridge.pasteText).toHaveBeenCalledTimes(1)
    expect(store.history).toHaveLength(1)
  })

  it('初始化還沒完成就鬆開：建好後立刻停，時長不足按取消處理', async () => {
    await startedOrchestrator()
    emit('ptt-down')
    emit('ptt-up')
    await flush(60)

    expect(provider.start).toHaveBeenCalledTimes(1)
    expect(provider.cancel).toHaveBeenCalled()
    expect(store.history).toHaveLength(0)
    expect(orch.getState()).toBe('idle')
  })

  it('provider 沒就緒：不進錄音，提示錯誤並重新建連', async () => {
    await startedOrchestrator()
    provider.ready = false
    provider.connect.mockClear()
    emit('ptt-down')
    await flush()

    expect(orch.getState()).toBe('idle')
    expect(provider.start).not.toHaveBeenCalled()
    expect(overlayStates()).toContain('error')
    expect(provider.connect).toHaveBeenCalled()
  })
})

describe('RecorderOrchestrator 超時與遲到結果', () => {
  // 只假造定時器和 Date；performance.now 保持真實，錄音時長按喂進去的取樣數算。
  // 超時兜底裡用的是 window.setTimeout；測試環境沒有 window，指向全域性物件即可拿到假定時器。
  const fake = () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval', 'Date'] })
    vi.stubGlobal('window', globalThis)
  }
  afterEach(() => { vi.unstubAllGlobals() })

  it('超時後寬限期內等到遲到結果：照樣存歷史並插字', async () => {
    await startedOrchestrator()
    fake()
    await recordAndRelease(2)

    // 2 秒音訊的處理超時 = 15s + 1s
    await vi.advanceTimersByTimeAsync(16_100)
    expect(orch.getState()).toBe('idle')
    expect(overlayStates()).toContain('thinking') // 寬限期提示「仍在獲取結果」

    provider.callbacks!.onFinal(finalResult('遲到了'))
    await vi.advanceTimersByTimeAsync(100)
    await flush(60)

    expect(bridge.pasteText).toHaveBeenCalledTimes(1)
    expect(store.history).toHaveLength(1)
    expect(store.history[0]).toMatchObject({ asrText: '遲到了' })

    // 寬限期結束後不應再補一條超時記錄
    await vi.advanceTimersByTimeAsync(16_000)
    expect(store.history).toHaveLength(1)
  })

  it('超時且寬限期內沒等到：彈超時卡，歷史記 processing_timeout', async () => {
    await startedOrchestrator()
    fake()
    await recordAndRelease(2)

    await vi.advanceTimersByTimeAsync(16_100 + 15_100)
    await flush(60)

    expect(overlayStates()).toContain('failure')
    expect(store.history).toHaveLength(1)
    expect(store.history[0]).toMatchObject({ failReasonCode: 'processing_timeout' })
    expect(bridge.pasteText).not.toHaveBeenCalled()
  })

  it('寬限期裡按 Esc 放棄：遲到結果只存歷史，不插字', async () => {
    await startedOrchestrator()
    fake()
    await recordAndRelease(2)

    await vi.advanceTimersByTimeAsync(16_100)
    emit('escape', { mode: 'abandon_late_result', token: 1 })
    provider.callbacks!.onFinal(finalResult('不要插'))
    await vi.advanceTimersByTimeAsync(100)
    await flush(60)

    expect(bridge.pasteText).not.toHaveBeenCalled()
    expect(store.history).toHaveLength(1)
    expect(store.history[0]).toMatchObject({ asrText: '不要插' })
  })
})

describe('RecorderOrchestrator 免持與中途斷線', () => {
  it('免持：按一下開始、期間的按住說話事件被忽略、再按一下結束並正常出字', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval', 'Date'] })
    await startedOrchestrator()
    emit('hands-free')
    await flush()
    expect(orch.getState()).toBe('recording')

    feedAudio(2)
    // 免持期間的 PTT 鬆開不能把錄音停掉
    emit('ptt-up')
    await flush()
    expect(orch.getState()).toBe('recording')

    await vi.advanceTimersByTimeAsync(600) // 過 500ms 冷卻
    emit('hands-free')
    await flush()
    expect(orch.getState()).toBe('processing')
    expect(provider.stop).toHaveBeenCalledTimes(1)

    provider.callbacks!.onFinal(finalResult('免提一句'))
    await vi.advanceTimersByTimeAsync(50)
    await flush(60)
    expect(bridge.pasteText).toHaveBeenCalledTimes(1)
    expect(store.history).toHaveLength(1)
  })

  it('錄音中途連線斷開：立刻報失敗，已經說的那段音訊存進歷史', async () => {
    await startedOrchestrator()
    emit('ptt-down')
    await flush()
    feedAudio(3)
    provider.callbacks!.onError('mid_session_disconnect')
    await flush(60)

    expect(overlayStates()).toContain('failure')
    expect(store.history).toHaveLength(1)
    expect(store.history[0].audioFilePath).toMatch(/\.wav$/)
    expect(orch.getState()).toBe('idle')

    // 失敗收尾之後再鬆開，不能再發 stop
    emit('ptt-up')
    await flush()
    expect(provider.stop).not.toHaveBeenCalled()
  })
})

describe('RecorderOrchestrator 麥克風電平提醒', () => {
  function warnings(): string[] {
    return vi.mocked(bridge.updateOverlay).mock.calls
      .map((c) => (c[0] as { warning?: string }).warning)
      .filter((w): w is string => Boolean(w))
  }

  it('從開始就全是 0：2 秒後提示「未檢測到聲音」，停止時統計帶 peakAmplitude=0', async () => {
    await startedOrchestrator()
    emit('ptt-down')
    await flush()
    feedAudio(2.5, 0)
    expect(warnings()).toContain('overlay.warnNoSignal')
    expect(warnings()).not.toContain('overlay.warnLowVolume')

    emit('ptt-up')
    await flush()
    const stopOpts = provider.stop.mock.calls[0][0] as { audioStats?: { peakAmplitude: number; silenceRatio: number } }
    expect(stopOpts.audioStats).toMatchObject({ peakAmplitude: 0, silenceRatio: 1 })
  })

  it('有聲音但很小：只提示「請靠近麥克風」，不說「未檢測到聲音」', async () => {
    await startedOrchestrator()
    emit('ptt-down')
    await flush()
    feedAudio(2.5, 40)
    expect(warnings()).toContain('overlay.warnLowVolume')
    expect(warnings()).not.toContain('overlay.warnNoSignal')
  })

  it('正常說話後停頓：不報任何電平提醒', async () => {
    await startedOrchestrator()
    emit('ptt-down')
    await flush()
    feedAudio(1)
    feedAudio(1.5, 0)
    expect(warnings()).not.toContain('overlay.warnNoSignal')
  })

  it('系統報靜音且音訊確實全 0：紅色高警，3 秒後自動取消錄音，不存歷史', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval', 'Date'] })
    const { invoke } = await import('@tauri-apps/api/core')
    vi.mocked(invoke).mockImplementation(async (cmd: string) =>
      cmd === 'get_mic_mute_state' ? { matched: true, muted: true } : undefined)
    await startedOrchestrator()
    emit('ptt-down')
    await flush()
    expect(orch.getState()).toBe('recording')
    feedAudio(0.5, 0)
    expect(warnings()).toContain('overlay.warnMicMuted')

    await vi.advanceTimersByTimeAsync(3100)
    await flush(40)
    expect(orch.getState()).toBe('idle')
    expect(provider.cancel).toHaveBeenCalled()
    expect(store.history).toHaveLength(0)
    vi.mocked(invoke).mockImplementation(async () => undefined)
  })

  it('系統報靜音但音訊在流動：忽略這個標誌，不彈紅色高警', async () => {
    const { invoke } = await import('@tauri-apps/api/core')
    vi.mocked(invoke).mockImplementation(async (cmd: string) =>
      cmd === 'get_mic_mute_state' ? { matched: true, muted: true } : undefined)
    await startedOrchestrator()
    emit('ptt-down')
    await flush()
    feedAudio(1)
    expect(warnings()).not.toContain('overlay.warnMicMuted')
    expect(orch.getState()).toBe('recording')
    vi.mocked(invoke).mockImplementation(async () => undefined)
  })
})

describe('RecorderOrchestrator 插入卡住與各模式超時', () => {
  const fake = () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval', 'Date'] })
    vi.stubGlobal('window', globalThis)
  }
  afterEach(() => { vi.unstubAllGlobals() })

  it('插入一直不返回：15 秒後把文字交還給使用者（兜底卡片），回到空閒，能開始下一次', async () => {
    vi.mocked(bridge.pasteText).mockImplementationOnce(() => new Promise(() => {}))
    await startedOrchestrator()
    fake()
    await recordAndRelease(2)
    provider.callbacks!.onFinal(finalResult('卡住的這句'))
    await vi.advanceTimersByTimeAsync(100)
    expect(bridge.pasteText).toHaveBeenCalledTimes(1)
    expect(overlayStates()).not.toContain('fallback')

    await vi.advanceTimersByTimeAsync(15_100)
    expect(overlayStates()).toContain('fallback')
    const fallback = [...vi.mocked(bridge.presentOverlay).mock.calls, ...vi.mocked(bridge.updateOverlay).mock.calls]
      .map((c) => c[0] as Record<string, unknown>)
      .find((p) => p.state === 'fallback')
    expect(JSON.stringify(fallback)).toContain('卡住的這句')
    expect(orch.getState()).toBe('idle')
    expect(store.history).toHaveLength(1)

    await vi.advanceTimersByTimeAsync(600)
    emit('ptt-down')
    for (let i = 0; i < 5; i++) {
      await vi.advanceTimersByTimeAsync(20)
      await flush(60)
    }
    if (orch.getState() !== 'recording') {
      const events = vi.mocked(addRuntimeEvent).mock.calls.slice(-8).map((c) => `${c[1]} ${c[2]} ${JSON.stringify(c[3] ?? '').slice(0, 200)}`)
      throw new Error(events.join(' | '))
    }
  })

  it.each([
    ['server', 16_000],
    ['local', 30_000],
    ['cloud_api', 31_000],
  ])('%s 模式 2 秒音訊的處理超時是 %i ms', async (mode, timeoutMs) => {
    provider.mode = mode
    await startedOrchestrator()
    fake()
    await recordAndRelease(2)
    await vi.advanceTimersByTimeAsync(timeoutMs - 200)
    expect(orch.getState()).toBe('processing')
    await vi.advanceTimersByTimeAsync(400)
    expect(orch.getState()).toBe('idle')
  })
})

describe('RecorderOrchestrator 開始錄音時出錯', () => {
  it('取上下文和路由那一段拋錯：報「開始錄音失敗」，鎖釋放，下一次能正常開始', async () => {
    const personalization = await import('../../personalization/store')
    const defaults = await import('../../personalization/defaults')
    // 統計資料格式壞掉時，提示詞路由會拋錯（2026-10 補測試時撞到的真實路徑）
    vi.mocked(personalization.getUserStats).mockResolvedValueOnce({} as never)
    await startedOrchestrator()

    emit('ptt-down')
    await flush(40)
    expect(orch.getState()).toBe('idle')
    expect(overlayStates()).toContain('error')

    vi.mocked(personalization.getUserStats).mockResolvedValue(defaults.createDefaultUserStats())
    await orch.refreshRuntimeSettings()
    emit('ptt-down')
    await flush(40)
    expect(orch.getState()).toBe('recording')
  })
})
