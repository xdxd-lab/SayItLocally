import { Card, CardContent } from '@/components/ui/card'
import { Switch } from '@/components/ui/switch'
import { useT } from '@/i18n/useT'
import { IS_MAC } from '@/lib/platform'

/**
 * 這裡曾經還有一個「自動檢測更新」開關。撤掉了：更新現在是必走的 ——
 * 後台靜默下載，使用者點左下角圖示、或下次關閉應用時裝上。
 * 底層設定項 autoCheckUpdate 仍然被讀取（見 features/update/autoUpdate.ts），
 * 只是不再暴露給使用者，留作更新鏈路自身出故障時的遠端止血開關。
 */
export default function AppSection({
  autoLaunch,
  onToggleAutoLaunch,
  ready = true,
  animate = true,
}: {
  autoLaunch: boolean
  onToggleAutoLaunch: () => void
  ready?: boolean
  animate?: boolean
}) {
  const t = useT()
  return (
    <Card>
      <CardContent className="p-6">
        <h2 className="mb-4 text-lg font-semibold">{t('settings.app.title')}</h2>
        <div className="flex items-center justify-between">
          <div>
            {/* mac 上叫「登入時開啟」，沒有「開機」這個說法 */}
            <p className="text-sm font-medium">{t(IS_MAC ? 'settings.app.autoLaunchMac' : 'settings.app.autoLaunch')}</p>
            <p className="text-xs text-muted-foreground">{t(IS_MAC ? 'settings.app.autoLaunchDescMac' : 'settings.app.autoLaunchDesc')}</p>
          </div>
          <Switch checked={autoLaunch} onChange={onToggleAutoLaunch} noAnimation={!animate} hidden={!ready} />
        </div>
      </CardContent>
    </Card>
  )
}
