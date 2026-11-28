import { afterEach, describe, expect, it } from 'vitest'
import {
  AI_PROVIDERS,
  aiProvidersForDisplay,
  blankProfile,
  checkAiKeyFormat,
  checkApiUrl,
  extractTestReply,
  formatCheckedAt,
  formatLatency,
  gradeLatency,
  isCheckFresh,
  isProfileComplete,
  migrateLegacyProfiles,
  parseLegacyLatencies,
  parseProfiles,
  profileSubtitle,
  profileTitle,
  preferredAiProviderValue,
  resolveActiveProfile,
  type AiProfile,
  type LegacyProviderData,
} from '../aiProviderCatalog'
import { setLocale } from '@/i18n'

/**
 * 這組斷言守住「AI 服務」頁最容易迴歸的幾件事：
 * 1. 供應商清單是唯一真相 —— 每家都必須有預設地址和至少一個候選模型，
 *    否則選中它就會落到"兩個空框 + 兩個灰按鈕 + 零說明"的死角。
 * 2. 格式校驗只做幾乎不會誤報的判斷 —— 對合法金鑰報警的代價高於漏報。
 * 3. 服務列表的讀取與啟用項解析永不拋異常 —— 這個值會經過匯入匯出和手改資料庫，
 *    一條髒資料不該讓整頁白屏，啟用項失效也不該變成"什麼都沒啟用"。
 */
describe('AI_PROVIDERS 清單', () => {
  it('每家都有預設地址和至少一個候選模型', () => {
    for (const p of AI_PROVIDERS) {
      expect(p.defaultUrl, `${p.value} 缺預設地址`).toMatch(/^https?:\/\//)
      if (p.value === 'openrouter') continue // [自訂] S34：OpenRouter 不再預設 deepseek/* 模型
      expect(p.defaultModels.length, `${p.value} 缺候選模型`).toBeGreaterThan(0)
      for (const m of p.defaultModels) expect(m.trim()).not.toBe('')
    }
  })

  it('value 唯一', () => {
    const values = AI_PROVIDERS.map((p) => p.value)
    expect(new Set(values).size).toBe(values.length)
  })

  it('只有 Ollama 是免金鑰的本機服務', () => {
    expect(AI_PROVIDERS.filter((p) => p.keyless).map((p) => p.value)).toEqual(['ollama'])
  })
})

describe('地區相關的供應商預設值', () => {
  afterEach(() => setLocale('zh-TW'))

  // [自訂] S34：不論介面語言，一律本機 Ollama；DeepSeek 不在清單中
  it.each(['en', 'zh-TW'] as const)('%s 介面一律 Ollama 優先', (locale) => {
    setLocale(locale)
    expect(preferredAiProviderValue()).toBe('ollama')
    expect(aiProvidersForDisplay()[0].value).toBe('ollama')
    expect(blankProfile().provider).toBe('ollama')
    expect(blankProfile().apiUrl).toBe('http://127.0.0.1:11434')
    expect(AI_PROVIDERS.some((p) => p.value === 'deepseek')).toBe(false)
  })
})

describe('checkAiKeyFormat', () => {
  it('空金鑰不報警', () => {
    expect(checkAiKeyFormat('deepseek', '')).toBe('')
  })

  it('貼上帶進空白字元時報警', () => {
    expect(checkAiKeyFormat('deepseek', 'sk-abc def')).toContain('空白')
    expect(checkAiKeyFormat('doubao', 'abc\ndef')).toContain('空白')
  })

  it('DeepSeek / 千問 缺 sk- 字首時報警', () => {
    expect(checkAiKeyFormat('deepseek', 'abcdef')).toContain('sk-')
    expect(checkAiKeyFormat('qwen', 'abcdef')).toContain('sk-')
  })

  it('不再對長度做斷言——供應商改長度不該讓合法金鑰報警', () => {
    expect(checkAiKeyFormat('deepseek', 'sk-' + 'a'.repeat(64))).toBe('')
    expect(checkAiKeyFormat('deepseek', 'sk-abc')).toBe('')
    // 豆包的 key 不是 UUID 也不報警
    expect(checkAiKeyFormat('doubao', 'not-a-uuid-at-all')).toBe('')
  })

  it('沒有專門規則的供應商只查空白字元', () => {
    expect(checkAiKeyFormat('mimo', 'whatever-key')).toBe('')
    expect(checkAiKeyFormat('openai_compat', 'anything')).toBe('')
  })
})

describe('checkApiUrl', () => {
  it('空地址不報警（由提交時統一處理）', () => {
    expect(checkApiUrl('')).toBe('')
  })

  it('缺協議字首時報警', () => {
    expect(checkApiUrl('api.deepseek.com')).toContain('http')
  })

  it('接受 http 與 https', () => {
    expect(checkApiUrl('https://api.deepseek.com')).toBe('')
    expect(checkApiUrl('http://127.0.0.1:11434')).toBe('')
  })

  it('非法網址報警', () => {
    expect(checkApiUrl('https://')).toContain('有效的網址')
  })
})

describe('extractTestReply', () => {
  // 與 ai_openai_compat.rs 的 test_connection 相同的四行格式
  const backendDetail = [
    'Elapsed: 1240ms',
    'Model: local-model',
    'Sent: system="只回覆「連線正常」四個字，不要輸出任何其他內容。" user="測試"',
    'Reply: 連線正常',
  ].join('\n')

  it('只取「回覆」，不把內部測試提示詞帶出來', () => {
    expect(extractTestReply(backendDetail)).toBe('連線正常')
    expect(extractTestReply(backendDetail)).not.toContain('system=')
  })

  it('沒有回覆行時返回空串', () => {
    expect(extractTestReply('Elapsed: 5ms')).toBe('')
    expect(extractTestReply(undefined)).toBe('')
  })
})

describe('formatLatency', () => {
  it('1 秒以內用 ms，超過用秒', () => {
    expect(formatLatency(840)).toBe('840ms')
    expect(formatLatency(1240)).toBe('1.2s')
  })
})

describe('parseLegacyLatencies', () => {
  it('解析 model=ms 串', () => {
    expect(parseLegacyLatencies('deepseek-v4-flash=1240,qwen-plus=2380')).toEqual({
      'deepseek-v4-flash': 1240,
      'qwen-plus': 2380,
    })
  })

  it('忽略損壞的條目，絕不拋異常', () => {
    expect(parseLegacyLatencies('a=1,,broken,b=x,c=3')).toEqual({ a: 1, c: 3 })
    expect(parseLegacyLatencies('')).toEqual({})
  })
})

function profile(patch: Partial<AiProfile> = {}): AiProfile {
  return {
    id: 'p1',
    provider: 'deepseek',
    apiUrl: 'https://api.deepseek.com',
    apiKey: 'sk-key',
    model: 'deepseek-v4-flash',
    ...patch,
  }
}

describe('parseProfiles', () => {
  it('非陣列一律當空列表（匯入了壞檔案、手改壞了資料庫）', () => {
    expect(parseProfiles(null)).toEqual([])
    expect(parseProfiles('[]')).toEqual([])
    expect(parseProfiles({ profiles: [] })).toEqual([])
  })

  it('丟掉不是物件的條目，保留能救的', () => {
    const result = parseProfiles([profile(), 'junk', null, 42])
    expect(result).toHaveLength(1)
    expect(result[0].model).toBe('deepseek-v4-flash')
  })

  it('缺欄位補空串、缺供應商回落到第一家', () => {
    const [entry] = parseProfiles([{ id: 'x' }])
    expect(entry).toEqual({
      id: 'x',
      provider: AI_PROVIDERS[0].value,
      apiUrl: '',
      apiKey: '',
      model: '',
      latencyMs: undefined,
    })
  })

  it('id 缺失或重複都要修掉：單選和編輯都靠 id 定位，重複會讓你點 A 改到 B', () => {
    const ids = parseProfiles([{ model: 'a' }, { model: 'b' }, { id: 'same' }, { id: 'same' }])
      .map((p) => p.id)
    expect(new Set(ids).size).toBe(ids.length)
  })

  it('讀回檢測結論；ok 不是布林值就當沒測過', () => {
    const ok = parseProfiles([profile({ check: { ok: true, at: 1000, latencyMs: 1200 } })])[0].check
    expect(ok).toEqual({ ok: true, at: 1000, latencyMs: 1200, reason: undefined })

    const failed = parseProfiles([profile({ check: { ok: false, at: 1000, reason: '金鑰被拒絕' } })])[0].check
    expect(failed?.ok).toBe(false)
    expect(failed?.reason).toBe('金鑰被拒絕')

    expect(parseProfiles([{ ...profile(), check: { latencyMs: 1200 } }])[0].check).toBeUndefined()
    expect(parseProfiles([{ ...profile(), check: 'ok' }])[0].check).toBeUndefined()
  })

  it('更早版本平鋪的 latencyMs 升級成"測透過"的結論（老版本只有測通才寫這個數）', () => {
    const upgraded = parseProfiles([{ ...profile(), check: undefined, latencyMs: 1200 }])[0].check
    expect(upgraded).toEqual({ ok: true, latencyMs: 1200 })
    // 沒有 at：那次是什麼時候測的沒人知道，介面上就不該聲稱"剛測過"
    expect(upgraded?.at).toBeUndefined()
  })

  it('髒的耗時不進結論', () => {
    expect(parseProfiles([{ ...profile(), latencyMs: 'fast' }])[0].check).toBeUndefined()
    expect(parseProfiles([{ ...profile(), latencyMs: Number.NaN }])[0].check).toBeUndefined()
  })
})

describe('gradeLatency', () => {
  it('五檔，按口述場景的手感切：200 / 500 / 1000 / 2000', () => {
    expect(gradeLatency(0).label).toBe('極速')
    expect(gradeLatency(199).label).toBe('極速')
    expect(gradeLatency(200).label).toBe('很快')
    expect(gradeLatency(499).label).toBe('很快')
    expect(gradeLatency(500).label).toBe('正常')
    expect(gradeLatency(999).label).toBe('正常')
    expect(gradeLatency(1000).label).toBe('偏慢')
    expect(gradeLatency(1999).label).toBe('偏慢')
    expect(gradeLatency(2000).label).toBe('太慢')
  })

  it('每一檔都有詞：極速也要說出來，使用者才知道這個數好不好', () => {
    for (const ms of [80, 300, 700, 1500, 3000]) {
      expect(gradeLatency(ms).label).not.toBe('')
    }
  })

  it('顏色分三檔，2 秒以上才升到 bad（那是"別用"，不只是"慢"）', () => {
    expect(gradeLatency(150).tone).toBe('ok')
    expect(gradeLatency(980).tone).toBe('ok')
    expect(gradeLatency(1400).tone).toBe('warn')
    expect(gradeLatency(2000).tone).toBe('bad')
    expect(gradeLatency(9000).tone).toBe('bad')
  })
})

describe('isCheckFresh', () => {
  const now = new Date('2026-08-03T12:00:00Z').getTime()

  it('保鮮期內才算新鮮——過期的結論不該繼續掛綠燈', () => {
    expect(isCheckFresh({ ok: true, at: now - 60 * 1000 }, now)).toBe(true)
    expect(isCheckFresh({ ok: true, at: now - 23 * 3600 * 1000 }, now)).toBe(true)
    expect(isCheckFresh({ ok: true, at: now - 25 * 3600 * 1000 }, now)).toBe(false)
    expect(isCheckFresh({ ok: true, at: now - 3 * 24 * 3600 * 1000 }, now)).toBe(false)
  })

  it('沒測過、或不知道什麼時候測的，都不算新鮮', () => {
    expect(isCheckFresh(undefined, now)).toBe(false)
    expect(isCheckFresh({ ok: true, latencyMs: 300 }, now)).toBe(false)
  })
})

describe('formatCheckedAt', () => {
  const now = new Date('2026-08-03T12:00:00Z').getTime()

  // 斷言"選了哪個粒度"，不寫死具體文案：相對時間現在交給 Intl.RelativeTimeFormat
  // 渲染（英文的單複數沒法用一條模板覆蓋），所以文案隨語言與 CLDR 走，不該鎖死。
  const rtf = new Intl.RelativeTimeFormat('zh-TW', { numeric: 'auto' })

  it('按最粗的合適粒度說"多久之前"', () => {
    expect(formatCheckedAt(now - 30 * 1000, now)).toBe('剛剛')
    expect(formatCheckedAt(now - 5 * 60 * 1000, now)).toBe(rtf.format(-5, 'minute'))
    expect(formatCheckedAt(now - 3 * 3600 * 1000, now)).toBe(rtf.format(-3, 'hour'))
    expect(formatCheckedAt(now - 50 * 3600 * 1000, now)).toBe(rtf.format(-2, 'day'))
  })

  it('時鐘往回跳時不說"負幾分鐘前"', () => {
    expect(formatCheckedAt(now + 60 * 1000, now)).toBe('剛剛')
  })
})

describe('resolveActiveProfile', () => {
  it('id 失效時回落到第一條，而不是"什麼都沒啟用"', () => {
    const list = [profile({ id: 'a' }), profile({ id: 'b' })]
    expect(resolveActiveProfile(list, 'b')?.id).toBe('b')
    expect(resolveActiveProfile(list, '已刪掉的 id')?.id).toBe('a')
    expect(resolveActiveProfile(list, '')?.id).toBe('a')
  })

  it('空列表返回 null', () => {
    expect(resolveActiveProfile([], 'a')).toBeNull()
  })
})

describe('一行的標題與副標題', () => {
  it('沒填模型時也有字，否則那一行看著像壞了', () => {
    expect(profileTitle(profile({ model: '  ' }))).toBe('未填寫模型')
  })

  it('預設地址不重複念，自定義端點才顯示主機名', () => {
    // [自訂] S34：DeepSeek 已移出清單，改用千問驗證「預設位址不顯示主機名」
    expect(profileSubtitle(profile({ provider: 'qwen', apiUrl: 'https://dashscope.aliyuncs.com/compatible-mode' }))).toBe('通義千問')
    expect(profileSubtitle(profile({ provider: 'openai_compat', apiUrl: 'http://127.0.0.1:8000/v1' })))
      .toBe('OpenAI 相容 · 127.0.0.1:8000')
  })

  it('兩個不同端點的 OpenAI 相容在列表裡能區分開——這正是舊結構做不到的事', () => {
    const a = profile({ id: 'a', provider: 'openai_compat', apiUrl: 'https://api.openai.com', model: 'gpt-4o-mini' })
    const b = profile({ id: 'b', provider: 'openai_compat', apiUrl: 'http://192.168.1.9:8000/v1', model: 'gpt-4o-mini' })
    expect(profileSubtitle(a)).not.toBe(profileSubtitle(b))
  })
})

describe('isProfileComplete', () => {
  it('免金鑰的 Ollama 不要求金鑰，其餘都要', () => {
    expect(isProfileComplete(profile({ provider: 'qwen', apiKey: '' }))).toBe(false) // [自訂] S34：deepseek 已移出清單
    expect(isProfileComplete(profile({ provider: 'ollama', apiKey: '', apiUrl: 'http://127.0.0.1:11434' }))).toBe(true)
  })

  it('缺地址或缺模型都算沒填完', () => {
    expect(isProfileComplete(profile({ apiUrl: '' }))).toBe(false)
    expect(isProfileComplete(profile({ model: ' ' }))).toBe(false)
    expect(isProfileComplete(profile())).toBe(true)
  })
})

describe('migrateLegacyProfiles', () => {
  const legacy: LegacyProviderData[] = [
    {
      provider: 'deepseek',
      apiUrl: 'https://api.deepseek.com',
      apiKey: 'sk-ds',
      model: 'deepseek-v4-flash',
      models: ['deepseek-v4-flash', 'deepseek-chat'],
      latencies: { 'deepseek-v4-flash': 1200 },
    },
    // 沒填金鑰的一家不該被遷成"看著能用其實打不通"的服務
    { provider: 'qwen', apiUrl: '', apiKey: '', model: '', models: [], latencies: {} },
  ]

  it('每個候選模型各成一條，帶上原來的地址、金鑰；有耗時的算測透過一次', () => {
    const { profiles } = migrateLegacyProfiles(legacy, 'deepseek', 'deepseek-chat')
    expect(profiles.map((p) => p.model)).toEqual(['deepseek-v4-flash', 'deepseek-chat'])
    expect(profiles.every((p) => p.apiKey === 'sk-ds')).toBe(true)
    expect(profiles[0].check).toEqual({ ok: true, latencyMs: 1200 })
    expect(profiles[1].check).toBeUndefined()
  })

  it('老的「當前供應商 + 當前模型」成為啟用項', () => {
    const { profiles, activeId } = migrateLegacyProfiles(legacy, 'deepseek', 'deepseek-chat')
    expect(resolveActiveProfile(profiles, activeId)?.model).toBe('deepseek-chat')
  })

  it('沒填完的供應商直接跳過', () => {
    const { profiles } = migrateLegacyProfiles(legacy, 'deepseek', '')
    expect(profiles.some((p) => p.provider === 'qwen')).toBe(false)
  })

  it('新使用者（什麼都沒配過）遷出空列表，不是一堆空殼', () => {
    const blank = AI_PROVIDERS.map((p) => ({
      provider: p.value, apiUrl: '', apiKey: '', model: '', models: [], latencies: {},
    }))
    const { profiles, activeId } = migrateLegacyProfiles(blank, '', '')
    expect(profiles).toEqual([])
    expect(activeId).toBe('')
  })

  it('id 是確定性的：重複遷移不會產生兩份', () => {
    const first = migrateLegacyProfiles(legacy, 'deepseek', '')
    const second = migrateLegacyProfiles(legacy, 'deepseek', '')
    expect(first.profiles.map((p) => p.id)).toEqual(second.profiles.map((p) => p.id))
  })
})
