import { getLocale, t } from '@/i18n'

/**
 * 大数字缩写（首页统计卡片用）。
 *
 * 中文分档保持原样（萬 / 千）—— 这是中文读者的习惯，不要动。
 * 英文没有"万"这一档，按 k / M 分；用 Intl 的 compact 记数法也能出结果，
 * 但它在中文下不会给"千"这一档，会改掉现有中文显示，所以这里手写分支。
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
