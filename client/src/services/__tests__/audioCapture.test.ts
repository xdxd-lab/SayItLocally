import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const runtimeEvents = vi.hoisted(() => [] as Array<[string, string, string]>)
vi.mock('../debugLog', () => ({
  addRuntimeEvent: (level: string, source: string, message: string) => { runtimeEvents.push([level, source, message]) },
}))
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async () => []) }))
vi.mock('../store', () => ({ getSetting: vi.fn(async (_k: string, d: unknown) => d), setSetting: vi.fn(async () => { }) }))

import { CLOSE_WAIT_MS, startCapture, stopCapture } from '../audio'
import { resolveMicPlan, type CaptureEndpoint } from '../micRouting'

// [自訂] SC-62：開錄時端點快照與 AudioContext 並行；停止時不等 AudioContext 關閉，
// 但音訊不遺失、停止後不再交出音訊、下一次開錄前一定先等關閉完成。

interface FakeContext {
  closeResolve: () => void
  closed: boolean
  addModule: ReturnType<typeof vi.fn>
}

const named = { kind: 'audioinput', deviceId: 'live', groupId: 'g', label: 'Local mic' }

let events: string[]
let contexts: FakeContext[]
let nodes: Array<{ port: { onmessage: ((event: { data: unknown }) => void) | null } }>
let trackStops: ReturnType<typeof vi.fn>
let enumerate: ReturnType<typeof vi.fn>
let enumerated: (list: unknown[]) => void
let scriptNodes: Array<{ onaudioprocess: unknown }>

function deferredEnumerate() {
  enumerate.mockImplementation(() => {
    events.push('enumerate')
    return new Promise(resolve => { enumerated = resolve })
  })
}

beforeEach(() => {
  events = []
  contexts = []
  nodes = []
  scriptNodes = []
  runtimeEvents.length = 0
  trackStops = vi.fn(() => { events.push('track.stop') })
  enumerate = vi.fn(async () => { events.push('enumerate'); return [named] })
  const track = { label: 'Local mic', getSettings: () => ({ deviceId: 'live', groupId: 'g' }), stop: trackStops }
  vi.stubGlobal('navigator', {
    mediaDevices: {
      getUserMedia: vi.fn(async () => {
        events.push('getUserMedia')
        return { getAudioTracks: () => [track], getTracks: () => [track] }
      }),
      enumerateDevices: enumerate,
    },
  })
  vi.stubGlobal('window', {
    AudioContext: function () {
      const index = contexts.length
      events.push(`context${index}`)
      const fake: FakeContext & Record<string, unknown> = {
        state: 'running',
        sampleRate: 16000,
        closed: false,
        closeResolve: () => { },
        addModule: vi.fn(async () => { events.push(`addModule${index}`) }),
        createMediaStreamSource: () => ({ connect: () => { }, disconnect: () => { } }),
        destination: {},
        createScriptProcessor: () => {
          events.push(`scriptProcessor${index}`)
          const node = { onaudioprocess: null, connect: () => { }, disconnect: () => { } }
          scriptNodes.push(node)
          return node
        },
        close: () => {
          events.push(`close${index}`)
          return new Promise<void>(resolve => {
            fake.closeResolve = () => { fake.closed = true; resolve() }
          })
        },
      }
      fake.audioWorklet = { addModule: fake.addModule }
      contexts.push(fake)
      return fake
    },
  })
  vi.stubGlobal('AudioWorkletNode', class extends EventTarget {
    port = { onmessage: null }
    constructor() { super(); nodes.push(this) }
    disconnect() { }
  })
})

afterEach(async () => {
  vi.useRealTimers()
  contexts.forEach(ctx => ctx.closeResolve())
  await stopCapture()
  contexts.forEach(ctx => ctx.closeResolve())
  vi.unstubAllGlobals()
})

function frame(samples: number[]) {
  return { data: { pcm: new Int16Array(samples).buffer, rms: 0.1 } }
}

describe('startCapture', () => {
  it('builds the AudioContext while the endpoint snapshot is still pending, and returns the snapshot', async () => {
    deferredEnumerate()
    const started = startCapture(undefined, vi.fn())
    await vi.waitFor(() => expect(events).toContain('addModule0'))
    expect(events.indexOf('enumerate')).toBeLessThan(events.indexOf('context0'))
    let settled = false
    void started.then(() => { settled = true })
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(settled).toBe(false)
    enumerated([named, { kind: 'videoinput', deviceId: 'cam', groupId: '', label: 'Cam' }])
    const info = await started
    expect(info).toMatchObject({ deviceId: 'live', groupId: 'g', label: 'Local mic' })
    expect(info.devices).toEqual([{ deviceId: 'live', groupId: 'g', label: 'Local mic' }])
  })

  it('returns the endpoint snapshot on the ScriptProcessor fallback, so a virtual default is still avoided', async () => {
    const cable = 'CABLE Output (VB-Audio Virtual Cable)'
    const real = 'Built-in Microphone'
    enumerate.mockImplementation(async () => {
      events.push('enumerate')
      return [
        { kind: 'audioinput', deviceId: 'default', groupId: 'g0', label: `Default - ${cable}` },
        { kind: 'audioinput', deviceId: 'cable', groupId: 'g1', label: cable },
        { kind: 'audioinput', deviceId: 'mic', groupId: 'g2', label: real },
      ]
    })
    const origCtor = (window as unknown as { AudioContext: new () => FakeContext }).AudioContext
    vi.stubGlobal('window', {
      AudioContext: function () {
        const ctx = new origCtor()
        ctx.addModule.mockRejectedValue(new Error('worklet unavailable'))
        return ctx
      },
    })
    const info = await startCapture(undefined, vi.fn())
    expect(events).toContain('scriptProcessor0')
    expect(nodes).toHaveLength(0)
    expect(info.devices.map(device => device.deviceId)).toEqual(['default', 'cable', 'mic'])
    const ep = (name: string, extra: Partial<CaptureEndpoint> = {}): CaptureEndpoint => ({
      name, enumerator: extra.isVirtual ? 'ROOT' : 'USB', isVirtual: false, isDefault: false, isDefaultCommunications: false, ...extra,
    })
    const endpoints = [ep(cable, { isVirtual: true, isDefault: true }), ep(real)]
    const retry = await resolveMicPlan('', { endpoints, webDevices: info.devices })
    expect(retry.plan).toMatchObject({ deviceId: 'mic', reason: 'virtual_avoided' })
  })

  it('returns an empty snapshot when enumeration fails', async () => {
    enumerate.mockRejectedValue(new Error('denied'))
    const info = await startCapture(undefined, vi.fn())
    expect(info.devices).toEqual([])
  })
})

describe('stopCapture', () => {
  it('delivers every frame received before stop, none after, and does not wait for the context to close', async () => {
    const received: number[][] = []
    await startCapture(undefined, buffer => received.push([...new Int16Array(buffer)]))
    const port = nodes[0].port
    port.onmessage!(frame([1, 2]))
    port.onmessage!(frame([3]))
    await stopCapture()
    expect(contexts[0].closed).toBe(false)
    expect(trackStops).toHaveBeenCalledOnce()
    expect(port.onmessage).toBeNull()
    expect(received).toEqual([[1, 2], [3]])
  })

  it('waits for the previous close before opening the microphone again', async () => {
    await startCapture(undefined, vi.fn())
    await stopCapture()
    events.length = 0
    let restarted = false
    const restart = startCapture(undefined, vi.fn()).then(() => { restarted = true })
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(events).toEqual([])
    expect(restarted).toBe(false)
    contexts[0].closeResolve()
    await restart
    expect(events.slice(0, 2)).toEqual(['getUserMedia', 'enumerate'])
    expect(contexts).toHaveLength(2)
  })

  it('starts anyway once the wait for a close that never finishes runs out, without reviving the old capture', async () => {
    const first: ArrayBuffer[] = []
    await startCapture(undefined, buffer => first.push(buffer))
    const oldPort = nodes[0].port
    await stopCapture()
    vi.useFakeTimers()
    events.length = 0
    const second: number[][] = []
    let restarted = false
    const restart = startCapture(undefined, buffer => second.push([...new Int16Array(buffer)])).then(() => { restarted = true })
    await vi.advanceTimersByTimeAsync(CLOSE_WAIT_MS - 1)
    expect(events).toEqual([])
    expect(restarted).toBe(false)
    await vi.advanceTimersByTimeAsync(1)
    await restart
    expect(events.slice(0, 2)).toEqual(['getUserMedia', 'enumerate'])
    expect(runtimeEvents).toContainEqual(['warn', 'audio', 'Previous AudioContext close timed out; starting capture anyway'])
    // 舊的一輪已切斷：只停過一次麥克風、舊 port 不再有處理器，新的音訊只交給新的一輪。
    expect(trackStops).toHaveBeenCalledOnce()
    expect(oldPort.onmessage).toBeNull()
    nodes[1].port.onmessage!(frame([7]))
    expect(second).toEqual([[7]])
    expect(first).toEqual([])
    // 卡住的 close 不再拖住之後的開錄。
    await stopCapture()
    contexts[1].closeResolve()
    events.length = 0
    let third = false
    const again = startCapture(undefined, vi.fn()).then(() => { third = true })
    await vi.advanceTimersByTimeAsync(0)
    expect(events[0]).toBe('getUserMedia')
    await again
    expect(third).toBe(true)
  })

  it('a failed close does not block the next capture', async () => {
    await startCapture(undefined, vi.fn())
    ;(contexts[0] as unknown as { close: () => Promise<void> }).close = () => Promise.reject(new Error('busy'))
    await stopCapture()
    await startCapture(undefined, vi.fn())
    expect(contexts).toHaveLength(2)
  })
})
