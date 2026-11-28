import { beforeEach, describe, expect, it, vi } from 'vitest'

const bridgeState = vi.hoisted(() => ({
  values: new Map<string, unknown>(),
}))

vi.mock('@/services/bridge', () => ({
  storeGet: vi.fn(async (key: string) => bridgeState.values.get(key) ?? null),
  storeSet: vi.fn(async (key: string, value: unknown) => {
    bridgeState.values.set(key, value)
  }),
}))

import { getDefault } from '../defaults'
import legacySimplified from './fixtures/legacySimplifiedBuiltinPrompts.json'
import {
  BUILTIN_PRESETS,
  builtinPromptContentHash,
  getBuiltinPromptPresets,
  getPromptPresets,
  isLegacySimplifiedBuiltinPrompt,
  LEGACY_SIMPLIFIED_BUILTIN_PROMPTS,
  normalizeBuiltinPromptLanguage,
  savePromptPreset,
  setBuiltinPromptLanguage,
  type PromptPreset,
} from '../store'

describe('內建 Prompt 語言', () => {
  beforeEach(() => bridgeState.values.clear())

  it('預設使用中文，非法持久化值也安全回落到中文', () => {
    expect(getDefault('ai.builtinPromptLanguage')).toBe('zh-TW')
    expect(normalizeBuiltinPromptLanguage('zh-TW')).toBe('zh-TW')
    // [自訂] SC-65：舊版存的簡體中文 Prompt 語言遷移成正體中文（台灣）。
    expect(normalizeBuiltinPromptLanguage('zh-CN')).toBe('zh-TW')
    expect(normalizeBuiltinPromptLanguage('en')).toBe('en')
    for (const value of ['', 'zh', 'en-US', null, undefined, 1]) {
      expect(normalizeBuiltinPromptLanguage(value), String(value)).toBe('zh-TW')
    }
  })

  it('中英文定義的 id 和順序一致，且四份英文 Prompt 均已提供', () => {
    const zh = getBuiltinPromptPresets('zh-TW')
    const en = getBuiltinPromptPresets('en')

    expect(zh.map((preset) => preset.id)).toEqual(BUILTIN_PRESETS.map((preset) => preset.id))
    expect(en.map((preset) => preset.id)).toEqual(zh.map((preset) => preset.id))
    expect(en).toHaveLength(4)
    for (let index = 0; index < en.length; index += 1) {
      expect(en[index].systemPrompt.trim()).not.toBe('')
      expect(en[index].systemPrompt).not.toBe(zh[index].systemPrompt)
      expect(en[index].builtinPromptLanguage).toBe('en')
      expect(zh[index].builtinPromptLanguage).toBe('zh-TW')
    }
  })

  // [自訂] SC-65：中文內建 Prompt 都要求以台灣正體中文輸出。
  it('中文內建 Prompt 都要求台灣正體輸出', () => {
    for (const preset of getBuiltinPromptPresets('zh-TW')) {
      expect(preset.systemPrompt, preset.id).toContain('台灣正體中文')
    }
  })

  it('持久化選擇後按對應語言載入', async () => {
    await setBuiltinPromptLanguage('en')
    const presets = await getPromptPresets()

    expect(bridgeState.values.get('ai.builtinPromptLanguage')).toBe('en')
    expect(presets.every((preset) => !preset.builtin || preset.builtinPromptLanguage === 'en')).toBe(true)
  })

  it('中英文內建修改分開儲存，恢復預設只清理當前語言', async () => {
    const zhIntent = getBuiltinPromptPresets('zh-TW')[0]
    const enIntent = getBuiltinPromptPresets('en')[0]

    await savePromptPreset({ ...zhIntent, systemPrompt: '中文自定義 Prompt' })
    await savePromptPreset({ ...enIntent, systemPrompt: 'Custom English prompt' })

    const stored = bridgeState.values.get('promptPresets') as PromptPreset[]
    expect(stored.find((preset) => preset.builtinPromptLanguage === 'zh-TW')?.builtinPromptBaseHash)
      .toBe(builtinPromptContentHash(zhIntent.systemPrompt))
    expect(stored.find((preset) => preset.builtinPromptLanguage === 'en')?.builtinPromptBaseHash)
      .toBe(builtinPromptContentHash(enIntent.systemPrompt))

    expect((await getPromptPresets('zh-TW'))[0].systemPrompt).toBe('中文自定義 Prompt')
    expect((await getPromptPresets('en'))[0].systemPrompt).toBe('Custom English prompt')

    await savePromptPreset(zhIntent)
    expect((await getPromptPresets('zh-TW'))[0].systemPrompt).toBe(zhIntent.systemPrompt)
    expect((await getPromptPresets('en'))[0].systemPrompt).toBe('Custom English prompt')
  })

  it('舊版無語言欄位的 override 只歸入中文', async () => {
    const legacyOverride: PromptPreset = {
      id: 'intent',
      name: '舊名稱',
      systemPrompt: '舊中文 Prompt',
    }
    bridgeState.values.set('promptPresets', [legacyOverride])

    expect((await getPromptPresets('zh-TW'))[0].systemPrompt).toBe(legacyOverride.systemPrompt)
    expect((await getPromptPresets('zh-TW'))[0].builtinPromptModified).toBe(true)
    expect((await getPromptPresets('zh-TW'))[0].builtinPromptUpdateAvailable).toBe(false)
    expect((await getPromptPresets('en'))[0].systemPrompt)
      .toBe(getBuiltinPromptPresets('en')[0].systemPrompt)
  })

  it('儲存基線與當前內建定義不同時提示有更新', async () => {
    bridgeState.values.set('promptPresets', [{
      id: 'intent',
      name: 'Intent cleanup',
      systemPrompt: 'My customized prompt',
      builtinPromptLanguage: 'en',
      builtinPromptBaseHash: 'older-version',
    } satisfies PromptPreset])

    const preset = (await getPromptPresets('en'))[0]
    expect(preset.builtinPromptModified).toBe(true)
    expect(preset.builtinPromptUpdateAvailable).toBe(true)
  })

  it('讀取時清理與當前官方內容完全相同的舊快照', async () => {
    const current = getBuiltinPromptPresets('zh-TW')[0]
    bridgeState.values.set('promptPresets', [{
      id: current.id,
      name: '舊名稱',
      systemPrompt: current.systemPrompt,
    } satisfies PromptPreset])

    const presets = await getPromptPresets('zh-TW')
    expect(presets[0].builtinPromptModified).toBeUndefined()
    expect(bridgeState.values.get('promptPresets')).toEqual([])
  })

  // [自訂] SC-65：舊 store 帶 zh-CN 語言的自訂 Prompt，在 zh-TW 下要照樣套用。
  it('舊版 zh-CN 語言的 override 在正體中文下套用，英文不受影響', async () => {
    bridgeState.values.set('promptPresets', [{
      id: 'intent',
      name: '舊名稱',
      systemPrompt: '我的自訂 Prompt',
      builtinPromptLanguage: 'zh-CN' as unknown as PromptPreset['builtinPromptLanguage'],
    } satisfies PromptPreset])

    const zh = (await getPromptPresets('zh-TW'))[0]
    expect(zh.systemPrompt).toBe('我的自訂 Prompt')
    expect(zh.builtinPromptModified).toBe(true)
    expect((await getPromptPresets('en'))[0].systemPrompt)
      .toBe(getBuiltinPromptPresets('en')[0].systemPrompt)
  })

  // [自訂] SC-65：未修改的舊簡體內建 Prompt 快照要被清掉，改用新的正體內建版。
  it('指紋表與 Unicode 跳脫保存的舊簡體原文一致', () => {
    const fixture = legacySimplified as Record<string, string>
    expect(Object.keys(fixture).sort()).toEqual(BUILTIN_PRESETS.map((preset) => preset.id).sort())
    expect(Object.keys(LEGACY_SIMPLIFIED_BUILTIN_PROMPTS).sort()).toEqual(Object.keys(fixture).sort())
    for (const [id, text] of Object.entries(fixture)) {
      expect(LEGACY_SIMPLIFIED_BUILTIN_PROMPTS[id], id).toEqual({
        hash: builtinPromptContentHash(text),
        length: text.length,
      })
      // 舊原文與新的正體內建版不同，否則這張表沒有意義。
      expect(text, id).not.toBe(getBuiltinPromptPresets('zh-TW').find((preset) => preset.id === id)?.systemPrompt)
    }
  })

  it('讀取時清理未修改的舊簡體內建快照（含 zh-CN 與無語言欄位）', async () => {
    const fixture = legacySimplified as Record<string, string>
    const enIntent = getBuiltinPromptPresets('en')[0]
    bridgeState.values.set('promptPresets', [
      { id: 'intent', name: '舊', systemPrompt: fixture.intent },
      {
        id: 'faithful',
        name: '舊',
        systemPrompt: fixture.faithful,
        builtinPromptLanguage: 'zh-CN' as unknown as PromptPreset['builtinPromptLanguage'],
      },
      // 改過一個字的舊快照是使用者的修改，必須保留。
      { id: 'casual', name: '舊', systemPrompt: `${fixture.casual}。` },
      // 英文 override 即使內容剛好是舊簡體原文，也不屬於中文內建快照，保留。
      { id: 'zh2en', name: 'x', systemPrompt: fixture.zh2en, builtinPromptLanguage: 'en' },
      { id: 'intent', name: 'x', systemPrompt: 'Custom English prompt', builtinPromptLanguage: 'en' },
    ] satisfies PromptPreset[])

    // 在英文 Prompt 語言下讀取也要清掉中文舊快照。
    expect((await getPromptPresets('en'))[0].systemPrompt).toBe('Custom English prompt')
    const stored = bridgeState.values.get('promptPresets') as PromptPreset[]
    expect(stored.map((preset) => preset.id)).toEqual(['casual', 'zh2en', 'intent'])

    const zh = await getPromptPresets('zh-TW')
    const current = getBuiltinPromptPresets('zh-TW')
    expect(zh[0].systemPrompt).toBe(current[0].systemPrompt)
    expect(zh[0].builtinPromptModified).toBeUndefined()
    expect(zh[1].systemPrompt).toBe(current[1].systemPrompt)
    expect(zh.find((preset) => preset.id === 'casual')?.builtinPromptModified).toBe(true)
    expect(isLegacySimplifiedBuiltinPrompt({ id: 'intent', name: '', systemPrompt: enIntent.systemPrompt })).toBe(false)
  })
})
