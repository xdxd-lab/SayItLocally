import { describe, expect, it } from 'vitest'
import { normalizeRemoteNotice } from '../notice'

const base = { id: 'notice-1', level: 'info' as const }

describe('normalizeRemoteNotice', () => {
  it('keeps legacy single-language notices compatible', () => {
    const result = normalizeRemoteNotice({ ...base, title: '舊公告', body: '舊正文' }, 'en')
    expect(result?.title).toBe('舊公告')
    expect(result?.body).toBe('舊正文')
  })

  it('selects title, body, and link label for the active locale', () => {
    const payload = {
      ...base,
      title: 'Fallback title',
      body: 'Fallback body',
      linkLabel: 'Fallback link',
      translations: {
        en: {
          title: 'Maintenance notice',
          body: 'Maintenance tonight',
          linkLabel: 'Learn more',
        },
        'zh-TW': {
          title: '維護通知',
          body: '今晚維護',
          linkLabel: '查看詳細資料',
        },
      },
    }
    expect(normalizeRemoteNotice(payload, 'en')).toMatchObject({
      title: 'Maintenance notice',
      body: 'Maintenance tonight',
      linkLabel: 'Learn more',
    })
    expect(normalizeRemoteNotice(payload, 'zh-TW')).toMatchObject({
      title: '維護通知',
      body: '今晚維護',
      linkLabel: '查看詳細資料',
    })
  })

  // [自訂] SC-65：舊格式公告可能帶簡體中文（zh-CN）翻譯；整則仍有效，但不會顯示那一份。
  it('accepts but never shows a legacy simplified Chinese translation', () => {
    const payload = {
      ...base,
      title: 'Fallback title',
      translations: { 'zh-CN': { title: 'legacy-zh-cn-title' } },
    }
    expect(normalizeRemoteNotice(payload, 'zh-TW')?.title).toBe('Fallback title')
    expect(normalizeRemoteNotice({ ...payload, translations: { fr: { title: 'x' } } }, 'zh-TW')).toBeNull()
  })

  it('falls back field by field to legacy strings', () => {
    const result = normalizeRemoteNotice({
      ...base,
      title: 'Fallback title',
      body: 'Fallback body',
      translations: { en: { title: 'English title' } },
    }, 'en')
    expect(result?.title).toBe('English title')
    expect(result?.body).toBe('Fallback body')
  })

  it('rejects payloads without any usable title', () => {
    expect(normalizeRemoteNotice({ ...base, title: '', translations: { en: { title: 'English' } } }, 'en')).toBeNull()
  })
})
