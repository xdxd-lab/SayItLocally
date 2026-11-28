import { describe, it, expect, vi, beforeEach } from 'vitest'

// 這些依賴都會走 Tauri IPC，測試裡替換掉
const invokeMock = vi.fn()
const getSettingMock = vi.fn()

vi.mock('@tauri-apps/api/core', () => ({ invoke: (...args: unknown[]) => invokeMock(...args) }))
vi.mock('../../store', () => ({ getSetting: (...args: unknown[]) => getSettingMock(...args) }))
vi.mock('../../debugLog', () => ({ addRuntimeEvent: () => { } }))

import { LocalProvider } from '../LocalProvider'
import { describeProviderError } from '@/lib/errorMessages'

/** 讓 getSetting 返回選中的模型 id，其餘鍵給預設值 */
function settings(modelId: string) {
  getSettingMock.mockImplementation((key: string, fallback: unknown) =>
    Promise.resolve(key === 'localAsr.modelId' ? modelId : fallback))
}

describe('LocalProvider 的就绪判定', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    getSettingMock.mockReset()
  })

  it('选中的模型已完整下载 → 就绪', async () => {
    settings('sensevoice-small-gguf')
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'list_downloaded_models') {
        return Promise.resolve([{ id: 'sensevoice-small-gguf', complete: true }])
      }
      return Promise.resolve('')
    })

    const provider = new LocalProvider()
    await provider.connect({})
    expect(provider.isReady()).toBe(true)
  })

  it('模型没下载 → 未就绪（设置页写着"按下快捷键不会有反应"，就必须真的拦住）', async () => {
    settings('sensevoice-small-gguf')
    invokeMock.mockImplementation((cmd: string) =>
      Promise.resolve(cmd === 'list_downloaded_models' ? [] : ''))

    const provider = new LocalProvider()
    await provider.connect({})
    expect(provider.isReady()).toBe(false)
  })

  it('模型下载不完整 → 未就绪', async () => {
    settings('sensevoice-small-gguf')
    invokeMock.mockImplementation((cmd: string) =>
      Promise.resolve(cmd === 'list_downloaded_models'
        ? [{ id: 'sensevoice-small-gguf', complete: false }]
        : ''))

    const provider = new LocalProvider()
    await provider.connect({})
    expect(provider.isReady()).toBe(false)
  })

  it('选中的是另一个模型（已下载的不是它）→ 未就绪', async () => {
    settings('qwen3-asr-1.7b-gguf')
    invokeMock.mockImplementation((cmd: string) =>
      Promise.resolve(cmd === 'list_downloaded_models'
        ? [{ id: 'sensevoice-small-gguf', complete: true }]
        : ''))

    const provider = new LocalProvider()
    await provider.connect({})
    expect(provider.isReady()).toBe(false)
  })

  it('读不到模型列表 → 按未就绪处理（宁可拦下，也别录完才失败）', async () => {
    settings('sensevoice-small-gguf')
    invokeMock.mockImplementation((cmd: string) =>
      cmd === 'list_downloaded_models' ? Promise.reject(new Error('ipc boom')) : Promise.resolve(''))

    const provider = new LocalProvider()
    await provider.connect({})
    expect(provider.isReady()).toBe(false)
  })

  it('已下载但预加载失败 → 仍算就绪（真正原因留给识别阶段报，不谎称"没下载"）', async () => {
    settings('sensevoice-small-gguf')
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'list_downloaded_models') {
        return Promise.resolve([{ id: 'sensevoice-small-gguf', complete: true }])
      }
      if (cmd === 'preload_local_model') return Promise.reject(new Error('vulkan boom'))
      return Promise.resolve('')
    })

    const provider = new LocalProvider()
    await provider.connect({})
    expect(provider.isReady()).toBe(true)
  })
})

// [自訂] SC-35：經真實 bridge 和 LocalProvider 傳到錄音回呼，依錯誤碼分類。
describe('LocalProvider model validation errors', () => {
  it.each([
    ['string', 'local_model_invalid', 'SHA-256 mismatch'],
    ['Error', 'local_model_invalid', 'SHA-256 mismatch'],
    ['Error', 'local_model_missing', 'file not found'],
    ['Error', 'local_model_read', 'read timed out: connection refused'],
  ] as const)('preserves a %s rejection tagged %s (%s)', async (kind, code, detail) => {
    settings('sensevoice-small-gguf')
    const encoded = `sayit_error:${code}:${detail}`
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'list_downloaded_models') {
        return [{ id: 'sensevoice-small-gguf', complete: true }]
      }
      if (command === 'local_transcribe_raw') {
        throw kind === 'Error' ? new Error(encoded) : encoded
      }
    })

    const provider = new LocalProvider()
    const onError = vi.fn()
    let done!: () => void
    const finished = new Promise<void>(resolve => { done = resolve })
    await provider.connect({ onError, onDone: done })
    expect(provider.start({ runId: 1 })).toBe(true)
    provider.sendAudio(new ArrayBuffer(16000))
    expect(provider.stop()).toBe(true)
    await finished

    expect(invokeMock).toHaveBeenCalledWith('local_transcribe_raw', expect.any(Uint8Array))
    expect(onError).toHaveBeenCalledExactlyOnceWith(encoded)
    expect(describeProviderError(onError.mock.calls[0][0])).toMatchObject({ code, detail, action: 'none' })
  })
})

// [自訂] SC-62：停止後的四個設定一起送出，仍用停止時讀到的值；取消後不送辨識。
describe('LocalProvider reads its settings together after stop', () => {
  function decodeMeta(body: Uint8Array) {
    const length = new DataView(body.buffer, body.byteOffset).getUint32(0, true)
    return JSON.parse(new TextDecoder().decode(body.subarray(4, 4 + length)))
  }

  async function connected() {
    getSettingMock.mockReset()
    invokeMock.mockReset()
    settings('sensevoice-small-gguf')
    invokeMock.mockImplementation(async (command: string) =>
      command === 'list_downloaded_models' ? [{ id: 'sensevoice-small-gguf', complete: true }] : '')
    const provider = new LocalProvider()
    const callbacks = { onASR: vi.fn(), onError: vi.fn(), onDone: vi.fn(), onFinal: vi.fn() }
    await provider.connect(callbacks)
    return { provider, callbacks }
  }

  function pendingSettings() {
    const pending = new Map<string, (value: unknown) => void>()
    getSettingMock.mockImplementation((key: string) => new Promise(resolve => pending.set(key, resolve)))
    return pending
  }

  const values: Record<string, string> = {
    'localAsr.modelId': 'qwen3-asr-1.7b-gguf',
    'localAsr.language': 'zh',
    'localAsr.accelerator': 'gpu',
    'localAsr.gpuDevice': 'device-2',
  }

  it('requests all four settings before any of them answers and passes the answers on', async () => {
    const { provider, callbacks } = await connected()
    let transcribed!: (body: Uint8Array) => void
    const sent = new Promise<Uint8Array>(resolve => { transcribed = resolve })
    invokeMock.mockImplementation(async (command: string, body: Uint8Array) => {
      if (command === 'local_transcribe_raw') {
        transcribed(body)
        return { text: '', elapsed_ms: 1 }
      }
      return ''
    })
    const pending = pendingSettings()
    expect(provider.start({ runId: 7 })).toBe(true)
    provider.sendAudio(new ArrayBuffer(16000))
    expect(provider.stop()).toBe(true)
    await vi.waitFor(() => expect(pending.size).toBe(4))
    expect([...pending.keys()].sort()).toEqual(Object.keys(values).sort())
    for (const [key, resolve] of pending) resolve(values[key])
    expect(decodeMeta(await sent)).toEqual({
      modelId: 'qwen3-asr-1.7b-gguf', language: 'zh', accelerator: 'gpu', gpuDevice: 'device-2',
    })
    await vi.waitFor(() => expect(callbacks.onDone).toHaveBeenCalled())
  })

  it('a repeated stop does not transcribe twice or send a second final', async () => {
    const { provider, callbacks } = await connected()
    invokeMock.mockImplementation(async (command: string) =>
      command === 'local_transcribe_raw' ? { text: '', elapsed_ms: 1 } : '')
    getSettingMock.mockImplementation((_key: string, fallback: unknown) => Promise.resolve(fallback))
    expect(provider.start({ runId: 9 })).toBe(true)
    provider.sendAudio(new ArrayBuffer(16000))
    expect(provider.stop()).toBe(true)
    expect(provider.stop()).toBe(false)
    await vi.waitFor(() => expect(callbacks.onDone).toHaveBeenCalled())
    expect(provider.stop()).toBe(false)
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(invokeMock.mock.calls.filter(([command]) => command === 'local_transcribe_raw')).toHaveLength(1)
    expect(callbacks.onASR).toHaveBeenCalledOnce()
    // 結束回呼（final 或錯誤）只有一次。
    expect(callbacks.onFinal.mock.calls.length + callbacks.onError.mock.calls.length).toBe(1)
    expect(callbacks.onDone).toHaveBeenCalledOnce()
  })

  it('does not transcribe when the run is cancelled while the settings are pending', async () => {
    const { provider, callbacks } = await connected()
    const pending = pendingSettings()
    expect(provider.start({ runId: 8 })).toBe(true)
    provider.sendAudio(new ArrayBuffer(16000))
    expect(provider.stop()).toBe(true)
    await vi.waitFor(() => expect(pending.size).toBe(4))
    provider.cancel()
    for (const [key, resolve] of pending) resolve(values[key])
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(invokeMock).not.toHaveBeenCalledWith('local_transcribe_raw', expect.anything())
    expect(callbacks.onASR).not.toHaveBeenCalled()
  })
})
