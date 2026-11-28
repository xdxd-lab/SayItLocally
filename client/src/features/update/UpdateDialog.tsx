/**
 * 安装中的遮罩。
 *
 * 只在 installing 那一態出現 —— 此刻應用即將退出、被安裝程式覆蓋，使用者必須知道
 * "別動它"，遮住介面是合理的。
 *
 * 下載期間**故意什麼都不顯示**：舊實現在下載時也掛這個全屏遮罩，且沒有關閉按鈕，
 * 於是開機自啟後正在按住說話的使用者會被突然糊住、接著應用自己退出。現在後台下載
 * 全程無聲，下完後由獨立桌面卡片提醒，側欄「關於」也保留更新入口。
 */

import { useEffect, useState } from 'react'
import { RefreshCw } from 'lucide-react'
import { onAutoUpdateChange, getAutoUpdateState, type AutoUpdateState } from './autoUpdate'
import { useT } from '@/i18n/useT'

export default function UpdateDialog() {
  const t = useT()
  const [state, setState] = useState<AutoUpdateState>(getAutoUpdateState)

  useEffect(() => {
    return onAutoUpdateChange(setState)
  }, [])

  if (state.phase !== 'installing') return null

  const version = state.pending?.version || ''

  return (
    <div className="fixed inset-0 z-[200] flex items-center justify-center bg-background/80 backdrop-blur-sm">
      <div className="w-full max-w-sm rounded-2xl border border-border bg-card p-6 shadow-xl">
        <div className="flex flex-col items-center text-center">
          {/* 用主題的 success 令牌而不是寫死 emerald-500：和左下角那枚更新徽標同一個綠，
              而且深浅主题下各自有合适的明度（写死的 emerald 在暗色主题上偏闷）。 */}
          <div className="mb-4 flex h-14 w-14 items-center justify-center rounded-full bg-success/10">
            <RefreshCw className="h-7 w-7 animate-spin text-success" />
          </div>
          <h3 className="text-lg font-semibold">{t('update.installingTitle', { version })}</h3>
          <p className="mt-2 text-sm text-muted-foreground">{t('update.installingDesc')}</p>
          <p className="mt-3 text-xs text-muted-foreground/60">{t('update.doNotClose')}</p>
        </div>
      </div>
    </div>
  )
}
