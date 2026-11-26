import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

vi.mock('../debugLog', () => ({ addRuntimeEvent: () => { } }))

import { startCapture, stopCapture } from '../audio'

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

  it('a failed close does not block the next capture', async () => {
    await startCapture(undefined, vi.fn())
    ;(contexts[0] as unknown as { close: () => Promise<void> }).close = () => Promise.reject(new Error('busy'))
    await stopCapture()
    await startCapture(undefined, vi.fn())
    expect(contexts).toHaveLength(2)
  })
})
