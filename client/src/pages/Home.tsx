import { useEffect, useState } from 'react'
import { Card, CardContent } from '@/components/ui/card'
import { Mic, Clock, Type, Zap } from 'lucide-react'
import { getStats, type Stats, getSetting } from '@/services/store'
import { SHORTCUTS_CHANGED_EVENT, listen } from '@/services/bridge'
// [自訂] S24／L-25 首頁回饋區塊已註解停用（會開啟 sayitapp.site/feedback.html）：
// import FeedbackSection from '@/components/FeedbackSection'
import { displayShortcut } from '@/lib/shortcutKeys'
import { formatCompactNumber } from '@/lib/compactNumber'
import { useT } from '@/i18n/useT'
import BrandBanner from '@/components/BrandBanner'
// [自訂] 啟動公告已註解停用（日後移除）：
// import NoticeBanner from '@/components/NoticeBanner'

/**
 * 把帶 `{key}` 佔位的文案渲染成「文字 + 鍵帽 + 文字」。
 *
 * 不能把整句塞進一個 t() 就完事：鍵帽是個帶樣式的 <span>，而中英文裡鍵帽出現的
 * 位置不同（"按下 X 開始…" vs "Press X to start…"）。按佔位符切分，位置就由
 * 譯文自己決定，不用為每種語言各寫一份 JSX。
 */
function WithKeyChip({ template, keyLabel, chipClassName }: {
  template: string
  keyLabel: string
  chipClassName: string
}) {
  const [before, after = ''] = template.split('{key}')
  return (
    <>
      {before}
      <span className={chipClassName}>{keyLabel}</span>
      {after}
    </>
  )
}

export default function Home() {
  const t = useT()
  const [stats, setStats] = useState<Stats>({ totalDurationSec: 0, totalChars: 0 })
  const [handsFreeKey, setHandsFreeKey] = useState('AltRight')

  useEffect(() => {
    const loadStats = () => getStats().then(setStats)
    void loadStats()
    // 累計統計由 Rust 在寫入/刪除歷史記錄的同一事務裡更新，事件在寫完之後才發，
    // 收到時讀到的就是新值。以前只在掛載時讀一次：主視窗停在首頁、或從托盤重新開啟
    // （頁面不會重新掛載）時，口述完數字一直不動，要切走再切回來才更新。
    const unlistenHistory = listen('history-updated', () => { void loadStats() })

    const loadHandsFreeKey = () =>
      getSetting('shortcutHandsFree', 'AltRight').then((value) => setHandsFreeKey(value as string))
    void loadHandsFreeKey()
    // 快捷鍵變化時（嚮導 / 設定頁修改）即時重新整理首頁提示，無需切換路由
    window.addEventListener(SHORTCUTS_CHANGED_EVENT, loadHandsFreeKey)
    return () => {
      window.removeEventListener(SHORTCUTS_CHANGED_EVENT, loadHandsFreeKey)
      void unlistenHistory.then((fn) => fn())
    }
  }, [])

  // Format time display
  const formatTime = (seconds: number) => {
    const totalMinutes = Math.round(seconds / 60)
    if (totalMinutes >= 60) {
      const hours = Math.floor(totalMinutes / 60)
      const minutes = totalMinutes % 60
      return {
        value: `${hours}`,
        extraValue: minutes > 0 ? `${minutes}` : null,
        unit: t('home.unitHours'),
        extraUnit: minutes > 0 ? t('home.unitMinutes') : null
      }
    }
    return { value: `${totalMinutes}`, extraValue: null, unit: t('home.unitMinutes'), extraUnit: null }
  }

  const totalTime = formatTime(stats.totalDurationSec)
  const avgWordsPerMin =
    stats.totalDurationSec > 60
      ? Math.round(stats.totalChars / (stats.totalDurationSec / 60))
      : 0
  const savedTime = formatTime(Math.round(stats.totalChars / 50) * 60)

  const handsFreeKeyLabel = displayShortcut(handsFreeKey).join(' + ')

  const cards = [
    { icon: Clock, label: t('home.statTotalTime'), ...totalTime },
    { icon: Mic, label: t('home.statChars'), value: formatCompactNumber(stats.totalChars), extraValue: null, unit: t('home.unitChars'), extraUnit: null },
    { icon: Zap, label: t('home.statSavedTime'), ...savedTime },
    { icon: Type, label: t('home.statSpeed'), value: `${avgWordsPerMin}`, extraValue: null, unit: t('home.unitCharsPerMinute'), extraUnit: null },
  ]

  const isNewUser = stats.totalDurationSec === 0 && stats.totalChars === 0

  return (
    <div className="max-w-4xl mx-auto">
      {/* [自訂] SC-63：首頁頂部橫幅＋一句訴求 */}
      <BrandBanner />
      <h1 className="mb-4 text-2xl font-bold">{t('home.title')}</h1>
      <p className="mb-8 text-sm text-muted-foreground">
        <WithKeyChip
          template={t('home.subtitle')}
          keyLabel={handsFreeKeyLabel}
          chipClassName="px-1.5 py-0.5 text-muted-foreground bg-secondary border border-border rounded"
        />
      </p>

      {/* [自訂] 啟動公告已註解停用（日後移除）：<NoticeBanner /> */}

      {isNewUser && (
        <div className="mb-6 rounded-xl border border-border bg-muted/30 px-5 py-5 text-center">
          <p className="text-sm text-muted-foreground">
            <WithKeyChip
              template={t('home.newUserHint')}
              keyLabel={handsFreeKeyLabel}
              chipClassName="px-1.5 py-0.5 text-muted-foreground bg-secondary border border-border rounded text-xs"
            />
          </p>
        </div>
      )}

      <div className="grid grid-cols-2 gap-4">
        {cards.map(({ icon: Icon, label, value, extraValue, unit, extraUnit }) => (
          <Card key={label}>
            <CardContent className="flex items-center gap-4 p-6">
              <div className="rounded-lg bg-secondary p-3">
                <Icon className="h-5 w-5 text-muted-foreground" />
              </div>
              <div>
                <p className="text-sm text-muted-foreground">{label}</p>
                <p className="text-2xl font-bold">
                  {value} <span className="text-sm font-normal text-muted-foreground">{unit}</span>
                  {extraValue && (
                    <>
                      {' '}{extraValue} <span className="text-sm font-normal text-muted-foreground">{extraUnit}</span>
                    </>
                  )}
                </p>
              </div>
            </CardContent>
          </Card>
        ))}
      </div>

      {/* [自訂] S24／L-25 已註解停用：<div className="mt-6"><FeedbackSection /></div> */}
    </div>
  )
}
