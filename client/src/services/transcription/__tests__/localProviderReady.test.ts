import { describe, it, expect, vi, beforeEach } from 'vitest'

// 这些依赖都会走 Tauri IPC，测试里替换掉
const invokeMock = vi.fn()
const getSettingMock = vi.fn()

vi.mock('@tauri-apps/api/core', () => ({ invoke: (...args: unknown[]) => invokeMock(...args) }))
vi.mock('../../store', () => ({ getSetting: (...args: unknown[]) => getSettingMock(...args) }))
vi.mock('../../debugLog', () => ({ addRuntimeEvent: () => { } }))

import { LocalProvider } from '../LocalProvider'
import { describeProviderError } from '@/lib/errorMessages'

/** 让 getSetting 返回选中的模型 id，其余键给默认值 */
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
