import { afterEach, describe, expect, it } from 'vitest'
import { setLocale } from '@/i18n'
import { formatCompactNumber } from '../compactNumber'

// [自訂] SC-65：正體中文介面用萬／千分檔，英文用 Intl compact。
describe('formatCompactNumber', () => {
  afterEach(() => setLocale('zh-TW'))

  it('正體中文用萬／千分檔', () => {
    setLocale('zh-TW')
    expect(formatCompactNumber(999)).toBe('999')
    expect(formatCompactNumber(1000)).toBe('1.0千')
    expect(formatCompactNumber(12345)).toBe('1.2萬')
  })

  it('英文走 Intl compact，不出現萬／千', () => {
    setLocale('en')
    expect(formatCompactNumber(999)).toBe('999')
    expect(formatCompactNumber(1500)).toBe('1.5K')
    expect(formatCompactNumber(12345)).toBe('12.3K')
  })
})
