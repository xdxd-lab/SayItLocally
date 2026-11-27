import { beforeEach, describe, expect, it, vi } from 'vitest'

const bridgeState = vi.hoisted(() => ({
  values: new Map<string, unknown>(),
  systemLanguage: 'en',
}))

vi.mock('@/services/bridge', () => ({
  storeGet: vi.fn(async (key: string) => bridgeState.values.get(key) ?? null),
  storeSet: vi.fn(async (key: string, value: unknown) => {
    bridgeState.values.set(key, value)
  }),
  getSystemUiLanguage: vi.fn(async () => bridgeState.systemLanguage),
}))

import { getLocale, setLocale } from '@/i18n'
import { initLanguage, initLocaleDefaults } from '../language'

describe('首次运行的地区默认值', () => {
  beforeEach(() => bridgeState.values.clear())

  // [自訂] S25／S34：不論語言，官方 Hugging Face 與本機 Ollama
  it('英文环境使用官方 Hugging Face 与 Ollama', async () => {
    await initLocaleDefaults('en')
    expect(bridgeState.values.get('localAsr.downloadSource')).toBe('HuggingFace')
    expect(bridgeState.values.get('cloudAi.provider')).toBe('ollama')
    expect(bridgeState.values.get('ai.builtinPromptLanguage')).toBe('en')
  })

  it('中文环境同样使用官方 Hugging Face 与 Ollama', async () => {
    await initLocaleDefaults('zh-TW')
    expect(bridgeState.values.get('localAsr.downloadSource')).toBe('HuggingFace')
    expect(bridgeState.values.get('cloudAi.provider')).toBe('ollama')
    expect(bridgeState.values.get('ai.builtinPromptLanguage')).toBe('zh-TW')
  })

  it('已有设置不会被界面语言覆盖', async () => {
    bridgeState.values.set('localAsr.downloadSource', 'Custom source')
    bridgeState.values.set('cloudAi.provider', '')
    bridgeState.values.set('ai.builtinPromptLanguage', 'zh-TW')
    await initLocaleDefaults('en')
    expect(bridgeState.values.get('localAsr.downloadSource')).toBe('Custom source')
    expect(bridgeState.values.get('cloudAi.provider')).toBe('')
    expect(bridgeState.values.get('ai.builtinPromptLanguage')).toBe('zh-TW')
  })
})

// [自訂] SC-65：簡體中文語系已移除。
describe('介面語言遷移', () => {
  beforeEach(() => {
    bridgeState.values.clear()
    bridgeState.systemLanguage = 'en'
    setLocale('en')
  })

  it('舊版存的 zh-CN 改用並改寫成 zh-TW', async () => {
    bridgeState.values.set('ui.language', 'zh-CN')
    expect(await initLanguage()).toBe('zh-TW')
    expect(getLocale()).toBe('zh-TW')
    expect(bridgeState.values.get('ui.language')).toBe('zh-TW')
  })

  it('跟隨系統時，簡體中文系統也用 zh-TW，且不寫入設定', async () => {
    bridgeState.systemLanguage = 'zh-Hans-CN'
    expect(await initLanguage()).toBe('zh-TW')
    expect(bridgeState.values.has('ui.language')).toBe(false)
  })

  it('其他設定值照舊', async () => {
    bridgeState.values.set('ui.language', 'en')
    bridgeState.systemLanguage = 'zh-TW'
    expect(await initLanguage()).toBe('en')
    expect(bridgeState.values.get('ui.language')).toBe('en')
  })
})
