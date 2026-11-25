import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { microphoneOpened } from '@/services/audio'
import { createSettingsMicrophone } from '../settingsMicrophone'
import { buildMicOptions } from '../utils'

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

function mount() {
  const callbacks = {
    onDevices: vi.fn(),
    onTesting: vi.fn(),
    onRevealing: vi.fn(),
    onVolume: vi.fn(),
    onError: vi.fn(),
    canvas: () => null,
  }
  const mic = createSettingsMicrophone(callbacks)
  disposals.push(mic.dispose)
  return { ...mic, ...callbacks }
}

const disposals: (() => void)[] = []

beforeEach(() => {
  vi.useFakeTimers()
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
    expect(mic.onError).toHaveBeenLastCalledWith('mic.error.failed')
    finishListing([named])
    await reveal
    expect(mic.onDevices).not.toHaveBeenCalledWith([named])
  })

  it('reports denial without prompting again, including from the other button', async () => {
    getUserMedia.mockRejectedValue(new DOMException('blocked', 'NotAllowedError'))
    const mic = mount()
    await mic.revealNames()
    expect(mic.onError).toHaveBeenLastCalledWith('mic.error.denied')
    await mic.revealNames()
    await mic.test('')
    expect(getUserMedia).toHaveBeenCalledOnce()
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
