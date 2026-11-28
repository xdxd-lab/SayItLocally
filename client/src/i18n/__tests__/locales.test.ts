import { describe, expect, it, afterEach } from 'vitest'
import en from '../locales/en.json'
import zhTW from '../locales/zh-TW.json'
import {
  getLocale,
  isLocale,
  normalizePreference,
  resolveLocale,
  setLocale,
  t,
  LOCALES,
} from '..'
import {
  appPromptRuleDisplayName,
  historyFailureReasonDisplay,
  localModelDisplayDescription,
  localModelDisplayLanguages,
  localModelDisplayName,
  promptPresetDisplayName,
  recordedAppDisplayName,
  recordedPromptPresetDisplayName,
} from '../displayNames'

/**
 * 這些測試鎖的是 i18n 的**不變數**，不是具體文案。
 * 斷言裡出現任何一句人類語言時都要先問：這條會不會在文案改一個字之後就掛？
 */
describe('locale 表', () => {
  it('en 与 zh-TW 的 key 集合完全一致', () => {
    // 型別層面已經保證 en 不缺 key（i18n/index.ts 的 Record<TranslationKey, string>），
    // 這裡補的是另一半：en 裡**多出來**的 key 型別查不出來，只能靠測試。
    const zhKeys = Object.keys(zhTW).sort()
    const enKeys = Object.keys(en).sort()
    expect(enKeys).toEqual(zhKeys)
  })

  it('没有空文案', () => {
    for (const [locale, table] of [['zh-TW', zhTW], ['en', en]] as const) {
      for (const [key, value] of Object.entries(table)) {
        expect(value.trim(), `${locale} 的 ${key} 是空的`).not.toBe('')
      }
    }
  })

  it('两边的插值占位符一一对应', () => {
    // 漏掉一個 {name} 不會報錯，只會在介面上少顯示一個東西 —— 典型的靜默失敗。
    const placeholders = (text: string) => (text.match(/\{(\w+)\}/g) ?? []).sort()
    for (const key of Object.keys(zhTW) as (keyof typeof zhTW)[]) {
      expect(placeholders(en[key]), `key=${key}`).toEqual(placeholders(zhTW[key]))
    }
  })
})

// [自訂] SC-65：與 Rust 端 locale.rs 的 TAG_CASES 是同一份表，兩邊要一起改。
const LOCALE_TAG_CASES: Array<[string | null | undefined, 'zh-TW' | 'en']> = [
  ['zh-TW', 'zh-TW'],
  ['zh-HK', 'zh-TW'],
  ['zh-MO', 'zh-TW'],
  ['zh-Hant', 'zh-TW'],
  ['zh-Hant-TW', 'zh-TW'],
  ['zh-CN', 'zh-TW'],
  ['zh-SG', 'zh-TW'],
  ['zh-Hans', 'zh-TW'],
  ['zh-Hans-CN', 'zh-TW'],
  ['zh', 'zh-TW'],
  ['zh_TW', 'zh-TW'],
  ['ZH-hk', 'zh-TW'],
  ['en', 'en'],
  ['en-US', 'en'],
  ['en-TW', 'en'],
  ['ja-JP', 'en'],
  ['zht', 'en'],
  ['', 'en'],
  [null, 'en'],
  [undefined, 'en'],
]

describe('resolveLocale', () => {
  it('所有中文（含簡體地區與字形標記）都落到正體中文（台灣），其餘落到英文', () => {
    for (const [tag, expected] of LOCALE_TAG_CASES) {
      expect(resolveLocale(tag), String(tag)).toBe(expected)
    }
  })
})

describe('normalizePreference', () => {
  it('保留合法值', () => {
    expect(normalizePreference('auto')).toBe('auto')
    expect(normalizePreference('en')).toBe('en')
    expect(normalizePreference('zh-TW')).toBe('zh-TW')
  })

  it('舊版的簡體中文設定遷移成正體中文（台灣）', () => {
    expect(normalizePreference('zh-CN')).toBe('zh-TW')
  })

  it('脏数据回落 auto', () => {
    for (const value of ['zh', 'EN', 'zh-HK', '', null, undefined, 42, {}]) {
      expect(normalizePreference(value), String(value)).toBe('auto')
    }
  })
})

describe('只有正體中文與英文', () => {
  it('語系清單不含簡體中文', () => {
    expect([...LOCALES].sort()).toEqual(['en', 'zh-TW'])
    expect(isLocale('zh-CN')).toBe(false)
  })
})

describe('t', () => {
  afterEach(() => setLocale('zh-TW'))

  // 斷言對照 JSON 表本身，不寫死具體文案：改一個字就掛的測試沒人願意維護，
  // 而這裡要鎖的是「t 有沒有按當前語言查表」這個機制。
  it('按当前语言取文案', () => {
    setLocale('zh-TW')
    expect(t('nav.home')).toBe(zhTW['nav.home'])
    setLocale('en')
    expect(t('nav.home')).toBe(en['nav.home'])
    expect(en['nav.home']).not.toBe(zhTW['nav.home'])
  })

  it('替换插值占位符', () => {
    setLocale('en')
    const rendered = t('titleBar.presetTooltip', { name: 'Faithful' })
    expect(rendered).toBe(en['titleBar.presetTooltip'].replace('{name}', 'Faithful'))
    expect(rendered).not.toContain('{name}')
  })

  it('未提供的占位符原样保留，不吐 undefined', () => {
    setLocale('en')
    expect(t('titleBar.presetTooltip')).toContain('{name}')
    expect(t('titleBar.presetTooltip')).not.toContain('undefined')
  })
})

describe('内置名称显示', () => {
  afterEach(() => setLocale('zh-TW'))

  it('内置 preset 与应用规则按稳定 id 切换语言', () => {
    const preset = { id: 'intent', name: 'legacy name', builtin: true }
    const rule = { id: 'notepad', appId: 'notepad', name: 'legacy name', builtin: true }

    setLocale('zh-TW')
    expect(promptPresetDisplayName(preset)).toBe(zhTW['builtinPreset.intent'])
    expect(appPromptRuleDisplayName(rule)).toBe(zhTW['builtinApp.notepad'])

    setLocale('en')
    expect(promptPresetDisplayName(preset)).toBe(en['builtinPreset.intent'])
    expect(appPromptRuleDisplayName(rule)).toBe(en['builtinApp.notepad'])
    expect(recordedPromptPresetDisplayName('intent', 'legacy name')).toBe(en['builtinPreset.intent'])
    expect(recordedAppDisplayName('notepad', 'legacy name')).toBe(en['builtinApp.notepad'])
  })

  it('用户自建名称与未知历史名称保持原样', () => {
    setLocale('en')
    expect(promptPresetDisplayName({ id: 'custom', name: 'My preset', builtin: false })).toBe('My preset')
    expect(appPromptRuleDisplayName({ id: 'slack', appId: 'slack', name: 'My Slack', builtin: false })).toBe('My Slack')
    expect(recordedPromptPresetDisplayName('custom', 'My preset')).toBe('My preset')
    expect(recordedAppDisplayName('slack', 'My Slack')).toBe('My Slack')
  })
})

describe('本地模型目录显示', () => {
  afterEach(() => setLocale('zh-TW'))

  it('已知模型按稳定 id 切换名称、简介和语种', () => {
    const model = {
      id: 'qwen3-asr-1.7b-gguf',
      name: 'raw name',
      description: 'raw description',
      languages_label: 'raw languages',
    }

    setLocale('zh-TW')
    expect(localModelDisplayName(model)).toBe(zhTW['localModel.qwen17Accurate.name'])
    expect(localModelDisplayDescription(model)).toBe(zhTW['localModel.qwen17Accurate.description'])
    expect(localModelDisplayLanguages(model)).toBe(zhTW['localModel.qwen17Accurate.languages'])

    setLocale('en')
    expect(localModelDisplayName(model)).toBe(en['localModel.qwen17Accurate.name'])
    expect(localModelDisplayDescription(model)).toBe(en['localModel.qwen17Accurate.description'])
    expect(localModelDisplayLanguages(model)).toBe(en['localModel.qwen17Accurate.languages'])
  })

  it('未知或用户侧载模型保留目录原文', () => {
    setLocale('en')
    const model = {
      id: 'custom-local-model',
      name: 'Custom model',
      description: 'Custom description',
      languages_label: 'Custom languages',
    }
    expect(localModelDisplayName(model)).toBe(model.name)
    expect(localModelDisplayDescription(model)).toBe(model.description)
    expect(localModelDisplayLanguages(model)).toBe(model.languages_label)
  })
})

describe('历史失败原因显示', () => {
  afterEach(() => setLocale('zh-TW'))

  it('新记录按稳定 code 跟随当前界面语言', () => {
    const record = { failReasonCode: 'provider_bad_key' as const, failReason: '底层原文' }

    setLocale('zh-TW')
    expect(historyFailureReasonDisplay(record)).toBe(zhTW['err.provider.badKey'])
    setLocale('en')
    expect(historyFailureReasonDisplay(record)).toBe(en['err.provider.badKey'])
  })

  it('老记录没有 code 时原样显示', () => {
    setLocale('en')
    expect(historyFailureReasonDisplay({ failReason: '旧版失败原因' })).toBe('旧版失败原因')
  })
})

describe('locale 状态', () => {
  afterEach(() => setLocale('zh-TW'))

  it('isLocale 只认受支持的值', () => {
    expect(LOCALES.every(isLocale)).toBe(true)
    for (const value of ['zh', 'zh-CN', 'en-US', '', null, 1]) {
      expect(isLocale(value), String(value)).toBe(false)
    }
  })

  it('setLocale 忽略非法值', () => {
    setLocale('en')
    // @ts-expect-error 故意傳非法值：執行時來源（overlay payload）不受型別保護
    setLocale('fr')
    expect(getLocale()).toBe('en')
  })
})
