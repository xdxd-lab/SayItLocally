import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { microphoneOpened, startCapture, stopCapture } from '@/services/audio'
import { getSetting, setSetting } from '@/services/store'
import { createSettingsMicrophone, mountSettingsMicrophone, type SettingsMicrophone } from '../settingsMicrophone'
import { buildMicOptions } from '../utils'

vi.mock('@/services/store', () => ({ getSetting: vi.fn(), setSetting: vi.fn() }))

const blank = { kind: 'audioinput', deviceId: '', groupId: '', label: '' }
const named = { ...blank, deviceId: 'live', label: 'Local mic' }

let getUserMedia: ReturnType<typeof vi.fn>
let enumerate: ReturnType<typeof vi.fn>
let stopFirst: ReturnType<typeof vi.fn<() => void>>
let stopSecond: ReturnType<typeof vi.fn<() => void>>
let stream: { getTracks: () => { stop: () => void }[] }
let closeContext: ReturnType<typeof vi.fn>
let makeAnalyser: ReturnType<typeof vi.fn>
let makeSource: ReturnType<typeof vi.fn>
let makeContext: ReturnType<typeof vi.fn>
let cancelFrame: ReturnType<typeof vi.fn>
let frameCallback: FrameRequestCallback

function microphoneCallbacks() {
  return {
    onDevices: vi.fn(),
    onTesting: vi.fn(),
    onRevealing: vi.fn(),
    onVolume: vi.fn(),
    onError: vi.fn(),
    canvas: () => null,
  }
}

function mount() {
  const callbacks = microphoneCallbacks()
  const mic = createSettingsMicrophone(callbacks)
  disposals.push(mic.dispose)
  return { ...mic, ...callbacks }
}

async function flushMicrophoneUpdates() {
  await Promise.resolve()
  await Promise.resolve()
}

const disposals: (() => void)[] = []

beforeEach(() => {
  vi.useFakeTimers()
  vi.mocked(getSetting).mockReset().mockResolvedValue('')
  vi.mocked(setSetting).mockReset().mockResolvedValue(undefined)
  stopFirst = vi.fn()
  stopSecond = vi.fn()
  stream = { getTracks: () => [{ stop: stopFirst }, { stop: stopSecond }] }
  getUserMedia = vi.fn().mockResolvedValue(stream)
  enumerate = vi.fn().mockResolvedValue([blank])
  const media = Object.assign(new EventTarget(), { getUserMedia, enumerateDevices: enumerate })
  vi.stubGlobal('navigator', { mediaDevices: media })
  closeContext = vi.fn().mockResolvedValue(undefined)
  makeSource = vi.fn().mockReturnValue({ connect: vi.fn() })
  makeAnalyser = vi.fn().mockReturnValue({
    frequencyBinCount: 128,
    getFloatTimeDomainData: vi.fn().mockImplementation((samples: Float32Array) => samples.fill(0.03)),
  })
  makeContext = vi.fn().mockImplementation(function () {
    return { close: closeContext, createAnalyser: makeAnalyser, createMediaStreamSource: makeSource }
  })
  vi.stubGlobal('AudioContext', makeContext)
  vi.stubGlobal('requestAnimationFrame', vi.fn((callback: FrameRequestCallback) => {
    frameCallback = callback
    return 123
  }))
  cancelFrame = vi.fn()
  vi.stubGlobal('cancelAnimationFrame', cancelFrame)
})

afterEach(() => {
  disposals.splice(0).forEach((dispose) => dispose())
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

describe('settings microphone names', () => {
  it('mounts, refreshes on recording and remounts using passive enumeration only', async () => {
    const first = mount()
    await Promise.resolve()
    await Promise.resolve()
    expect(first.onDevices).toHaveBeenCalledWith([blank])
    enumerate.mockResolvedValue([named])
    microphoneOpened.dispatchEvent(new Event('change'))
    await Promise.resolve()
    await Promise.resolve()
    expect(first.onDevices).toHaveBeenLastCalledWith([named])
    first.dispose()
    const second = mount()
    await Promise.resolve()
    await Promise.resolve()
    expect(second.onDevices).toHaveBeenLastCalledWith([named])
    expect(getUserMedia).not.toHaveBeenCalled()
  })

  it('prompts once on an explicit click and stops every track as soon as names arrive', async () => {
    const mic = mount()
    let finishListing!: (devices: unknown[]) => void
    enumerate.mockReturnValueOnce(new Promise((resolve) => { finishListing = resolve }))
    const reveal = mic.revealNames()
    await mic.revealNames()
    expect(getUserMedia).toHaveBeenCalledExactlyOnceWith({ audio: true })
    expect(stopFirst).not.toHaveBeenCalled()
    expect(stopSecond).not.toHaveBeenCalled()
    expect(makeContext).not.toHaveBeenCalled()
    finishListing([named])
    await reveal
    expect(stopFirst).toHaveBeenCalledOnce()
    expect(stopSecond).toHaveBeenCalledOnce()
    expect(mic.onDevices).toHaveBeenLastCalledWith([named])
    expect(mic.onRevealing.mock.calls).toEqual([[true], [false]])
  })

  it('stops name lookup if enumeration hangs and drops its late result', async () => {
    const mic = mount()
    let finishListing!: (devices: unknown[]) => void
    enumerate.mockReturnValueOnce(new Promise((resolve) => { finishListing = resolve }))
    const reveal = mic.revealNames()
    await Promise.resolve()
    vi.advanceTimersByTime(1000)
    expect(stopFirst).toHaveBeenCalledOnce()
    expect(stopSecond).toHaveBeenCalledOnce()
    expect(mic.onError).toHaveBeenLastCalledWith('mic.error.namesUnavailable')
    finishListing([named])
    await reveal
    expect(mic.onDevices).not.toHaveBeenCalledWith([named])
  })

  it('reports denial and allows an explicit retry without prompting on a device refresh', async () => {
    getUserMedia.mockRejectedValueOnce(new DOMException('blocked', 'NotAllowedError'))
    const mic = mount()
    await mic.revealNames()
    expect(mic.onError).toHaveBeenLastCalledWith('mic.error.denied')
    navigator.mediaDevices.dispatchEvent(new Event('devicechange'))
    await flushMicrophoneUpdates()
    expect(getUserMedia).toHaveBeenCalledOnce()
    enumerate.mockResolvedValue([named])
    await mic.revealNames()
    expect(getUserMedia).toHaveBeenCalledTimes(2)
    expect(mic.onDevices).toHaveBeenLastCalledWith([named])
    expect(mic.onError).toHaveBeenLastCalledWith(null)
  })

  it.each(['unnamed entries', 'failed enumeration'])('reports unavailable names after %s', async (scenario) => {
    const mic = mount()
    if (scenario === 'failed enumeration') enumerate.mockRejectedValueOnce(new Error('enumeration failed'))
    await mic.revealNames()
    expect(mic.onError).toHaveBeenLastCalledWith('mic.error.namesUnavailable')
    expect(stopFirst).toHaveBeenCalledOnce()
    expect(stopSecond).toHaveBeenCalledOnce()
  })

  it('keeps names through concurrent and later passive refreshes, matching by id only', async () => {
    const mic = mount()
    let finishListing!: (devices: unknown[]) => void
    enumerate.mockReturnValueOnce(new Promise((resolve) => { finishListing = resolve }))
    const reveal = mic.revealNames()
    await flushMicrophoneUpdates()
    navigator.mediaDevices.dispatchEvent(new Event('devicechange'))
    await flushMicrophoneUpdates()
    finishListing([named])
    await reveal
    expect(mic.onDevices).toHaveBeenLastCalledWith([named])
    const same = { ...blank, deviceId: 'live', groupId: 'new-group' }
    const other = { ...blank, deviceId: 'other' }
    enumerate.mockResolvedValue([same, other])
    navigator.mediaDevices.dispatchEvent(new Event('devicechange'))
    await flushMicrophoneUpdates()
    expect(mic.onDevices).toHaveBeenLastCalledWith([
      { deviceId: 'live', label: named.label, groupId: 'new-group' }, other,
    ])
  })

  it('lets a newer device list win over an older name lookup and keeps its names', async () => {
    const mic = mount()
    await flushMicrophoneUpdates()
    let finishListing!: (devices: unknown[]) => void
    enumerate.mockReturnValueOnce(new Promise((resolve) => { finishListing = resolve }))
    const reveal = mic.revealNames()
    await flushMicrophoneUpdates()
    // The named microphone is unplugged while the lookup is still enumerating.
    const other = { ...blank, deviceId: 'other' }
    enumerate.mockResolvedValueOnce([other])
    navigator.mediaDevices.dispatchEvent(new Event('devicechange'))
    await flushMicrophoneUpdates()
    expect(mic.onDevices).toHaveBeenLastCalledWith([other])
    finishListing([named, { ...other, label: 'Other mic' }])
    await reveal
    expect(mic.onDevices).toHaveBeenLastCalledWith([{ deviceId: 'other', groupId: '', label: 'Other mic' }])
    expect(mic.onDevices).not.toHaveBeenCalledWith(expect.arrayContaining([named]))
  })

  it('applies a newer refresh that finishes after the lookup, and drops an older one', async () => {
    let finishMount!: (devices: unknown[]) => void
    enumerate.mockReturnValueOnce(new Promise((resolve) => { finishMount = resolve }))
    const mic = mount()
    let finishLookup!: (devices: unknown[]) => void
    enumerate.mockReturnValueOnce(new Promise((resolve) => { finishLookup = resolve }))
    const reveal = mic.revealNames()
    await flushMicrophoneUpdates()
    let finishNewer!: (devices: unknown[]) => void
    enumerate.mockReturnValueOnce(new Promise((resolve) => { finishNewer = resolve }))
    navigator.mediaDevices.dispatchEvent(new Event('devicechange'))
    finishLookup([named])
    await reveal
    expect(mic.onDevices).toHaveBeenLastCalledWith([named])
    finishMount([blank])
    await flushMicrophoneUpdates()
    expect(mic.onDevices).toHaveBeenLastCalledWith([named])
    finishNewer([])
    await flushMicrophoneUpdates()
    expect(mic.onDevices).toHaveBeenLastCalledWith([])
  })

  it('still applies a pending refresh when a name lookup fails', async () => {
    let finishMount!: (devices: unknown[]) => void
    enumerate.mockReturnValueOnce(new Promise((resolve) => { finishMount = resolve }))
    const mic = mount()
    enumerate.mockRejectedValueOnce(new Error('enumeration failed'))
    await mic.revealNames()
    expect(mic.onError).toHaveBeenLastCalledWith('mic.error.namesUnavailable')
    finishMount([named])
    await flushMicrophoneUpdates()
    expect(mic.onDevices).toHaveBeenLastCalledWith([named])
  })

  it('retains an unavailable selection in the options while its labels are hidden', () => {
    const options = buildMicOptions([blank], 'prior-device', {
      systemDefault: 'Default',
      systemDefaultWith: (label) => label,
      unnamed: (id) => id,
      unavailable: 'Unavailable',
    })
    expect(options.map(({ value, label }) => [value, label])).toEqual([
      ['', 'Default'], ['prior-device', 'Unavailable'],
    ])
  })
})

describe('settings microphone test', () => {
  it('samples volume, cleans up on completion and permits another test', async () => {
    const mic = mount()
    await mic.test('chosen')
    await mic.test('ignored-while-busy')
    expect(getUserMedia).toHaveBeenCalledExactlyOnceWith({ audio: { deviceId: { exact: 'chosen' } } })
    expect(vi.getTimerCount()).toBe(2)
    vi.advanceTimersByTime(5000)
    expect(mic.onVolume).toHaveBeenLastCalledWith('normal')
    expect(mic.onTesting.mock.calls).toEqual([[true], [false]])
    expect(cancelFrame).toHaveBeenCalledExactlyOnceWith(123)
    expect(vi.getTimerCount()).toBe(0)
    expect(stopFirst).toHaveBeenCalledOnce()
    expect(stopSecond).toHaveBeenCalledOnce()
    expect(closeContext).toHaveBeenCalledOnce()
    await mic.test('')
    expect(getUserMedia).toHaveBeenLastCalledWith({ audio: true })
  })

  it.each(['constructor', 'analyser', 'source'])('cleans up after %s failure', async (where) => {
    const stage = where === 'constructor' ? makeContext : where === 'analyser' ? makeAnalyser : makeSource
    stage.mockImplementationOnce(() => { throw new Error('Web Audio failure') })
    const mic = mount()
    await mic.test('')
    expect(mic.onError).toHaveBeenLastCalledWith('mic.error.failed')
    expect(mic.onTesting.mock.calls).toEqual([[true], [false]])
    expect(stopFirst).toHaveBeenCalledOnce()
    expect(stopSecond).toHaveBeenCalledOnce()
    expect(closeContext).toHaveBeenCalledTimes(where === 'constructor' ? 0 : 1)
    expect(vi.getTimerCount()).toBe(0)
    await mic.test('')
    expect(getUserMedia).toHaveBeenCalledTimes(2)
  })

  it('cleans up on unmount and cancels even an already queued frame', async () => {
    const mic = mount()
    await mic.test('')
    const samples = mic.onVolume.mock.calls.length
    mic.dispose()
    expect(cancelFrame).toHaveBeenCalledExactlyOnceWith(123)
    expect(vi.getTimerCount()).toBe(0)
    frameCallback(0)
    vi.advanceTimersByTime(6000)
    expect(requestAnimationFrame).toHaveBeenCalledOnce()
    expect(mic.onTesting.mock.calls).toEqual([[true]])
    expect(mic.onVolume).toHaveBeenCalledTimes(samples)
    expect(stopFirst).toHaveBeenCalledOnce()
    expect(stopSecond).toHaveBeenCalledOnce()
    expect(closeContext).toHaveBeenCalledOnce()
  })

  it.each(['test', 'revealNames'] as const)('stops the late %s stream after leaving during permission', async (action) => {
    let finishOpening!: (opened: typeof stream) => void
    getUserMedia.mockReturnValueOnce(new Promise((resolve) => { finishOpening = resolve }))
    const first = mount()
    const oldTest = first[action]('')
    first.dispose()
    const second = mount()
    finishOpening(stream)
    await oldTest
    expect(stopFirst).toHaveBeenCalledOnce()
    expect(stopSecond).toHaveBeenCalledOnce()
    expect(makeContext).not.toHaveBeenCalled()
    expect(first.onDevices).not.toHaveBeenCalled()
    expect(first.onError.mock.calls).toEqual([[null]])
    expect(first.onTesting.mock.calls).toEqual(action === 'test' ? [[true]] : [])
    expect(first.onRevealing.mock.calls).toEqual(action === 'revealNames' ? [[true]] : [])
    expect(second.onTesting).not.toHaveBeenCalled()
  })
})


describe('settings microphone page binding', () => {
  it.each([['default', ''], ['prior-device', 'prior-device']])(
    'loads %s at entry and re-entry without updating storage or losing the selection on refresh',
    async (stored, expected) => {
      vi.mocked(getSetting).mockResolvedValue(stored)
      const ref: { current: SettingsMicrophone | null } = { current: null }
      const callbacks = { ...microphoneCallbacks(), onSelectedMic: vi.fn() }
      const leave = mountSettingsMicrophone(ref, callbacks)
      await flushMicrophoneUpdates()
      expect(callbacks.onSelectedMic).toHaveBeenCalledExactlyOnceWith(expected)
      expect(setSetting).not.toHaveBeenCalled()
      enumerate.mockResolvedValue([named])
      navigator.mediaDevices.dispatchEvent(new Event('devicechange'))
      await flushMicrophoneUpdates()
      enumerate.mockResolvedValue([])
      navigator.mediaDevices.dispatchEvent(new Event('devicechange'))
      await flushMicrophoneUpdates()
      expect(callbacks.onSelectedMic).toHaveBeenCalledExactlyOnceWith(expected)
      expect(setSetting).not.toHaveBeenCalled()
      leave()
      const leaveAgain = mountSettingsMicrophone(ref, callbacks)
      await flushMicrophoneUpdates()
      expect(callbacks.onSelectedMic.mock.calls).toEqual([[expected], [expected]])
      expect(setSetting).not.toHaveBeenCalled()
      await ref.current!.select('live')
      expect(setSetting).toHaveBeenCalledExactlyOnceWith('selectedMic', 'live')
      leaveAgain()
    },
  )

  it('cleans up both registered listeners and an active stream when the page leaves', async () => {
    const addDevice = vi.spyOn(navigator.mediaDevices, 'addEventListener')
    const removeDevice = vi.spyOn(navigator.mediaDevices, 'removeEventListener')
    const addRecording = vi.spyOn(microphoneOpened, 'addEventListener')
    const removeRecording = vi.spyOn(microphoneOpened, 'removeEventListener')
    try {
      const ref: { current: SettingsMicrophone | null } = { current: null }
      const leave = mountSettingsMicrophone(ref, { ...microphoneCallbacks(), onSelectedMic: vi.fn() })
      await ref.current!.test('')
      leave()
      expect(ref.current).toBeNull()
      expect(addDevice).toHaveBeenCalledExactlyOnceWith('devicechange', expect.any(Function))
      expect(removeDevice.mock.calls).toEqual(addDevice.mock.calls)
      expect(addRecording).toHaveBeenCalledExactlyOnceWith('change', expect.any(Function))
      expect(removeRecording.mock.calls).toEqual(addRecording.mock.calls)
      expect(stopFirst).toHaveBeenCalledOnce()
      expect(stopSecond).toHaveBeenCalledOnce()
    } finally {
      addDevice.mockRestore()
      removeDevice.mockRestore()
      addRecording.mockRestore()
      removeRecording.mockRestore()
    }
  })

  it('refreshes through the event produced when recorder capture succeeds', async () => {
    const mic = mount()
    await flushMicrophoneUpdates()
    expect(mic.onDevices).toHaveBeenLastCalledWith([blank])
    const recordingStream = { ...stream, getAudioTracks: () => [] }
    getUserMedia.mockResolvedValueOnce(recordingStream)
    enumerate.mockResolvedValue([named])
    vi.stubGlobal('window', {
      AudioContext: function () {
        return {
          state: 'running',
          sampleRate: 16000,
          close: closeContext,
          createMediaStreamSource: makeSource,
          audioWorklet: { addModule: vi.fn().mockResolvedValue(undefined) },
        }
      },
    })
    vi.stubGlobal('AudioWorkletNode', class extends EventTarget { port = {}; disconnect() {} })
    try {
      await startCapture(undefined, vi.fn())
      expect(mic.onDevices).toHaveBeenLastCalledWith([named])
      expect(getUserMedia).toHaveBeenCalledOnce()
    } finally {
      await stopCapture()
    }
  })
})
