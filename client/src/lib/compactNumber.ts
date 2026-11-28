import { getLocale, t } from '@/i18n'

/**
 * 大數字縮寫（首頁統計卡片用）。
 *
 * 中文分檔保持原樣（萬 / 千）—— 這是中文讀者的習慣，不要動。
 * 英文沒有"萬"這一檔，按 k / M 分；用 Intl 的 compact 記數法也能出結果，
 * 但它在中文下不會給"千"這一檔，會改掉現有中文顯示，所以這裡手寫分支。
 *
 * [自訂] SC-65：中文介面（正體中文）用萬／千分檔。從 Home.tsx 抽出，供測試鎖住。
 */
export function formatCompactNumber(num: number): string {
  if (getLocale() === 'zh-TW') {
    if (num >= 10000) return t('home.compactTenThousand', { value: (num / 10000).toFixed(1) })
    if (num >= 1000) return t('home.compactThousand', { value: (num / 1000).toFixed(1) })
    return `${num}`
  }
  return new Intl.NumberFormat(getLocale(), {
    notation: 'compact',
    maximumFractionDigits: 1,
  }).format(num)
}
