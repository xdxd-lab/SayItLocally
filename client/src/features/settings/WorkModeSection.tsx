// 工作模式切換卡片

import { useEffect, useSyncExternalStore } from 'react'
import { Card, CardContent } from '@/components/ui/card'
import { Tooltip } from '@/components/ui/tooltip'
import { useConnectionStatus } from '@/hooks/useConnectionStatus'
import { getModeStatus, refreshModeStatus, subscribeModeStatus } from '@/stores/modeStatus'
// 這三個圖示是在給模式**起名**，不是報狀態，所以選的是"這個模式是什麼"的最短表達：
// 本地在本機算 → Cpu，雲 API 用你自己的雲廠商 → Cloud，伺服器模式連的是一台伺服器 → Server。
// 比原來的 Monitor（顯示器）/ Globe（地球）/ HardDrive（硬碟）都更貼。
//
// 左下角的引擎指示裡，伺服器模式用的是訊號圖示而不是 Server——那裡報的是"此刻通不通"，
// 有一條真在跑的連線可報；這張卡不做這種斷言（連線狀態由卡組右上角那枚徽標負責）。
// 兩處對本地 / 雲 API 用同一個圖示，對伺服器模式刻意不同，因為它們說的不是同一件事。
import { Cpu, Cloud, Server, CheckCircle2, type LucideIcon } from 'lucide-react'
import type { WorkMode } from '@/services/transcription'
import type { TranslationKey } from '@/i18n'
import { useT } from '@/i18n/useT'

// 模組級常量只求值一次，所以存 key、渲染時才翻。
const modes: Array<{ value: WorkMode; labelKey: TranslationKey; descKey: TranslationKey; privacyKey: TranslationKey; icon: LucideIcon }> = [
  {
    value: 'local', labelKey: 'mode.local',
    descKey: 'workMode.local.desc',
    privacyKey: 'workMode.local.privacy',
    icon: Cpu,
  },
]

const statusConfig = {
  connected: { dot: 'bg-success', textKey: 'status.connected', bg: 'bg-success/10 text-success-strong' },
  connecting: { dot: 'bg-warning animate-pulse', textKey: 'status.connecting', bg: 'bg-warning/10 text-warning-strong' },
  disconnected: { dot: 'bg-muted-foreground', textKey: 'status.disconnected', bg: 'bg-muted text-muted-foreground' },
  error: { dot: 'bg-destructive', textKey: 'status.error', bg: 'bg-destructive/10 text-destructive-strong' },
} as const satisfies Record<string, { dot: string; textKey: TranslationKey; bg: string }>

interface Props {
  value: WorkMode
  onChange: (mode: WorkMode) => void
}

export default function WorkModeSection({ value, onChange }: Props) {
  const t = useT()
  const wsStatus = useConnectionStatus()
  const { ready, blockedReason } = useSyncExternalStore(subscribeModeStatus, getModeStatus)

  // 徽標反映真實就緒狀態，所以本元件也要在掛載時拉一次（頁面可能是直接深鏈進來的）
  useEffect(() => { void refreshModeStatus() }, [])

  /**
   * 狀態徽標。
   *
   * 這裡曾經是三個三元：伺服器模式看真實連線狀態，本地/雲 API **一律**綠點 +「就緒」。
   * 結果是模型沒下載、金鑰沒填也顯示綠燈，而卡片正下方同時寫著「模型尚未下載」——
   * 同屏自相矛盾。現在本地/雲 API 的就緒判斷來自 modeStatus（見該 store 的註釋），
   * 未就緒時顯示「待配置」並可點選滾到對應的配置卡。
   */
  const badge = value === 'server'
    ? { dot: statusConfig[wsStatus].dot, bg: statusConfig[wsStatus].bg, text: t(statusConfig[wsStatus].textKey), hint: '' }
    : ready === false
      ? {
        dot: 'bg-warning',
        text: t('workMode.badge.needsSetup'),
        bg: 'bg-warning/10 text-warning-strong',
        // blockedReason 仍是服務層給的中文串（P2-1 會 code 化），這裡只保證外殼跟隨語言
        hint: blockedReason ? t('workMode.hintBlocked', { reason: blockedReason }) : t('workMode.hintIncomplete'),
      }
      : ready === true
        ? { dot: 'bg-success', text: t('workMode.badge.ready'), bg: 'bg-success/10 text-success-strong', hint: '' }
        : { dot: 'bg-muted-foreground', text: t('workMode.badge.checking'), bg: 'bg-muted text-muted-foreground', hint: '' }

  const scrollToConfig = () => {
    document.getElementById('engine-config')?.scrollIntoView({ behavior: 'smooth', block: 'start' })
  }

  const badgeBody = (
    <>
      <span className={`inline-block h-1.5 w-1.5 shrink-0 rounded-full ${badge.dot}`} aria-hidden />
      {badge.text}
    </>
  )
  const badgeClass = `inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-xs font-medium ${badge.bg}`

  return (
    <Card>
      <CardContent className="p-6">
        <div className="mb-4 flex items-center justify-between gap-3">
          <h2 id="work-mode-heading" className="text-lg font-semibold">{t('workMode.title')}</h2>

          {badge.hint ? (
            <Tooltip variant="light" content={t('workMode.tooltipJump', { hint: badge.hint })}>
              <button
                type="button"
                onClick={scrollToConfig}
                className={`${badgeClass} transition-colors hover:bg-warning/20`}
              >
                {badgeBody}
              </button>
            </Tooltip>
          ) : (
            <span className={badgeClass} role="status">{badgeBody}</span>
          )}
        </div>

        <div
          role="radiogroup"
          aria-labelledby="work-mode-heading"
          className="grid gap-3 sm:grid-cols-3"
        >
          {modes.map((m) => {
            const isActive = value === m.value
            const Icon = m.icon
            return (
              <button
                key={m.value}
                type="button"
                role="radio"
                aria-checked={isActive}
                onClick={() => onChange(m.value)}
                className={`relative rounded-lg border p-4 text-left transition-colors ${isActive
                  ? 'border-primary bg-primary/5'
                  : 'border-border hover:bg-accent'
                  }`}
              >
                <Icon
                  className={`absolute right-3 top-3 h-5 w-5 transition-colors ${isActive ? 'text-primary' : 'text-muted-foreground'}`}
                  aria-hidden
                />
                {/* 選中態原來只有顏色（1px 邊框 + 圖示變色 + 5% 的淡底）。加一個對勾，
                    让"当前是哪个"不依赖颜色感知。
                    用 CheckCircle2 + success-strong 而不是裸 Check + primary，是为了和
                    「语音识别服务」卡片上的「使用中」标记完全一致 —— 两页都是在选一张卡，
                    同一个含义不该长成两个样子。别顺手把它统一回 primary。
                    图标保持 aria-hidden：选中状态已经由外层 role="radio" 的 aria-checked
                    如实报给读屏，再给图标加一个 label 只会让它念两遍。 */}
                <div className="flex items-center gap-1.5 pr-7 text-sm font-medium">
                  {isActive && <CheckCircle2 className="h-4 w-4 shrink-0 text-success-strong" aria-hidden />}
                  {t(m.labelKey)}
                </div>
                <div className="mt-1 text-xs text-muted-foreground">{t(m.descKey)}</div>
              </button>
            )
          })}
        </div>
        {/* 「資料去哪兒」原來三張卡各寫一段，一屏六段字。只寫當前選中的那個，放在卡組下面 */}
        <p className="mt-3 text-xs leading-relaxed text-muted-foreground">
          {t(modes.find((m) => m.value === value)?.privacyKey ?? 'workMode.server.privacy')}
        </p>
      </CardContent>
    </Card>
  )
}
