import { useEffect, useRef, useSyncExternalStore } from 'react'
import { NavLink, useLocation } from 'react-router-dom'
import { useSlidingIndicator } from '@/components/ui/useSlidingIndicator'
import { Home, Clock, BookOpen, Settings, Info, Wifi, WifiOff, Cpu, Cloud, AudioLines, Sparkles, Wand2 } from 'lucide-react'
import { cn } from '@/lib/utils'
import { Tooltip } from '@/components/ui/tooltip'
import { useConnectionStatus } from '@/hooks/useConnectionStatus'
import { getModeStatus, refreshModeStatus, subscribeModeStatus } from '@/stores/modeStatus'
import { hasPendingUpdate } from '@/features/update/autoUpdate'
import { useUpdateState } from '@/features/update/useUpdateState'
import type { TranslationKey } from '@/i18n'
import { useT } from '@/i18n/useT'
import { IS_MAC } from '@/lib/platform'
// mac 側欄頂部的品牌圖示：從 src-tauri/icons/icon.png（256px）按精確尺寸高質量縮出來的 1 倍 / 2 倍圖。
// 讓瀏覽器把大圖現場縮到 28px 會發虛，圖示裡那行 SayItLocally 就看不清了。
import macIcon from '@/assets/app-icon-28.png'
import macIcon2x from '@/assets/app-icon-28@2x.png'

// 導航項存 key 而不是文案：切語言時這些常量不會重新求值（模組級只算一次），
// 存成中文串就會永遠停在啟動時那個語言。
const dailyNavItems = [
  { to: '/', icon: Home, labelKey: 'nav.home' },
  { to: '/history', icon: Clock, labelKey: 'nav.history' },
] as const satisfies readonly NavItemDef[]

const configNavItems = [
  { to: '/voice-engine', icon: AudioLines, labelKey: 'nav.voiceEngine' },
  { to: '/hotwords', icon: BookOpen, labelKey: 'nav.hotwords' },
  { to: '/ai-instructions', icon: Wand2, labelKey: 'nav.aiInstructions' },
  { to: '/ai-service', icon: Sparkles, labelKey: 'nav.aiService' },
] as const satisfies readonly NavItemDef[]

const footerNavItems = [
  { to: '/settings', icon: Settings, labelKey: 'nav.settings' },
  { to: '/about', icon: Info, labelKey: 'nav.about' },
] as const satisfies readonly NavItemDef[]

interface NavItemDef {
  to: string
  icon: typeof Home
  labelKey: TranslationKey
}

function NavItem({
  to,
  icon: Icon,
  label,
}: {
  to: string
  icon: typeof Home
  label: string
}) {
  return (
    <NavLink
      to={to}
      className={({ isActive }) =>
        cn(
          // 選中底色由組裡的滑塊畫（見 Sidebar 裡的 useSlidingIndicator），這裡只管文字。
          // relative z-[1]：滑塊是組裡最後一個子元素（space-y 的緣故），不抬一層會蓋住文字。
          'relative z-[1] flex items-center gap-3 rounded-lg px-3 py-2 text-sm transition-colors',
          isActive ? 'font-medium text-sidebar-text-active' : 'text-sidebar-text hover:bg-sidebar-item-hover hover:text-sidebar-text-active',
        )
      }
    >
      <Icon className="h-4 w-4" />
      {label}
    </NavLink>
  )
}

function IconOnlyNavItem({
  to,
  icon: Icon,
  label,
  iconClassName,
}: {
  to: string
  icon: typeof Home
  label: string
  /** 覆蓋圖示自身的顏色/動效（如「有更新」時的綠色閃爍） */
  iconClassName?: string
}) {
  return (
    <Tooltip content={label}>
      <NavLink
        to={to}
        // 這裡只有圖示、沒有文字，不給 aria-label 的話讀屏使用者聽到的是空按鈕
        aria-label={label}
        className={({ isActive }) =>
          cn(
            'relative z-[1] flex items-center justify-center rounded-lg p-2 transition-colors',
            isActive ? 'text-sidebar-text-active' : 'text-sidebar-text hover:bg-sidebar-item-hover hover:text-sidebar-text-active',
          )
        }
      >
        <Icon className={cn('h-4 w-4', iconClassName)} aria-hidden />
      </NavLink>
    </Tooltip>
  )
}

/**
 * 側欄底部那排圖示。
 *
 * 有更新待安裝時**不新增圖示** —— 讓「關於」這一枚自己變綠閃爍，懸停提示換成
 * 「新版本已下載好」。關於頁就是更新所在的地方，點它正好到達能看到版本說明和
 * 「立即安裝」的位置；多一枚圖示既擠又需要使用者先學會它是什麼意思。
 *
 * 後台下載期間**故意毫無變化**：那會兒沒有任何需要使用者知道的事，靜默才是本意。
 *
 * ⚠ 這裡用綠色不違反下面 ModeIndicator 那條"不給任何好顏色"的規矩：那條針對的是
 * 我們沒驗證過的事（配置填完了 ≠ 真能用）。而"包已下載完、雜湊校驗過、隨時可裝"
 * 是確定的事實。別順手把它改回中性色。
 */
function FooterIcons() {
  const t = useT()
  const update = useUpdateState()
  const updateReady = hasPendingUpdate(update)
  const nextVersion = update.pending?.version || ''
  const { pathname } = useLocation()
  const groupRef = useRef<HTMLDivElement>(null)
  const indicatorRef = useSlidingIndicator(groupRef, pathname, { activeSelector: '[aria-current="page"]' })

  return (
    <div ref={groupRef} className="relative flex items-center gap-1">
      <span ref={indicatorRef} aria-hidden className="sliding-indicator rounded-lg bg-sidebar-item-active" />
      {footerNavItems.map(({ to, icon, labelKey }) => {
        const highlight = updateReady && to === '/about'
        return (
          <IconOnlyNavItem
            key={to}
            to={to}
            icon={icon}
            label={highlight ? t('update.aboutTooltip', { version: nextVersion }) : t(labelKey)}
            iconClassName={highlight ? 'text-success animate-pulse' : undefined}
          />
        )
      })}
      <ModeIndicator />
    </div>
  )
}

/**
 * 伺服器模式的連線狀態。
 *
 * 只有這一個模式配得上"狀態指示"：它有一條真在跑的連線（30s 心跳），所以圖示能誠實地
 * 報出此刻通不通——訊號圖示 + 顏色，斷線換成帶斜槓的那隻。
 * 本地/雲 API 沒有這種持續探測，見 ModeIndicator 的註釋。
 */
const statusConfig = {
  connected: { icon: Wifi, color: 'text-success', labelKey: 'connection.connected' },
  connecting: { icon: Wifi, color: 'text-warning animate-pulse', labelKey: 'connection.connecting' },
  disconnected: { icon: WifiOff, color: 'text-muted-foreground', labelKey: 'connection.disconnected' },
  error: { icon: WifiOff, color: 'text-destructive', labelKey: 'connection.error' },
} as const satisfies Record<string, { icon: typeof Wifi; color: string; labelKey: TranslationKey }>

/**
 * 左下角的引擎指示：始終只佔一個圖示位，細節全在懸停提示裡。
 *
 * 這裡和「語音引擎」頁那三張模式卡**不是同一件事**，所以圖示也不必事事對齊：
 * 卡片上的圖示是在給模式起名（Cpu / Cloud / Server，一個靜態屬性）；
 * 這裡報的是"此刻怎麼樣"。於是只有伺服器模式換成訊號圖示——它有一條真在跑的連線可報。
 *
 * 本地 / 雲 API 一律是中性單色，**不給任何"好"的顏色**：我們唯一知道的事實是
 * "配置填完了沒有"，而填完 ≠ 真的能用（模型能不能載入、金鑰有沒有被吊銷，都沒驗過）。
 * 所以只有確定的壞訊息（缺東西）才轉 warning 色，其餘保持沉默——
 * 曾經在這裡點過綠燈，等於替一件沒測過的事作保。
 */
function ModeIndicator() {
  const t = useT()
  const status = useConnectionStatus()
  const { mode, detail, ready, blockedReason } = useSyncExternalStore(subscribeModeStatus, getModeStatus)

  useEffect(() => { void refreshModeStatus() }, [])

  if (mode === 'server') {
    const { icon: StatusIcon, color, labelKey } = statusConfig[status]
    return (
      <Tooltip content={t('mode.tooltipDetail', { mode: t('mode.server'), detail: t(labelKey) })}>
        <div className="flex items-center justify-center rounded-lg p-2">
          <StatusIcon className={cn('h-4 w-4', color)} />
        </div>
      </Tooltip>
    )
  }

  const Icon = mode === 'local' ? Cpu : Cloud
  const title = mode === 'local' ? t('mode.local') : t('mode.cloudApi')
  const notReady = ready === false
  // blockedReason 目前是 Rust/服務層給的中文串（P2-1 會改成 code 再本地化）。
  // 這裡只保證**外殼**跟隨語言，不假裝裡面那句已經翻好了。
  const tip = notReady
    ? t('mode.tooltipNotReady', { mode: title, reason: blockedReason || t('mode.notReadyFallback') })
    : detail ? t('mode.tooltipDetail', { mode: title, detail }) : title

  return (
    <Tooltip content={tip}>
      <div className="flex items-center justify-center rounded-lg p-2">
        <Icon className={cn('h-4 w-4', notReady ? 'text-warning' : 'text-sidebar-text')} />
      </div>
    </Tooltip>
  )
}

export default function Sidebar() {
  const t = useT()
  const { pathname } = useLocation()
  const navGroupRef = useRef<HTMLDivElement>(null)
  // 當前頁在底部那排（設定 / 關於）時這一組沒有選中項，滑塊淡出；從那邊切回來時原地出現，不跨組滑。
  const navIndicatorRef = useSlidingIndicator(navGroupRef, pathname, { activeSelector: '[aria-current="page"]' })
  return (
    <nav className="flex w-48 flex-col border-r border-sidebar-border bg-sidebar py-4">
      {/* mac：標題欄左邊歸系統紅綠燈，圖示和名字放在側欄最上面、「首頁」上方（Windows 的在標題欄裡）。
          圖示中心對齊下面導航項的圖示列（距側欄左邊 32px）。 */}
      {IS_MAC && (
        <div className="mb-3 flex items-center gap-2.5 pl-[18px] pr-3">
          <img src={macIcon} srcSet={`${macIcon} 1x, ${macIcon2x} 2x`} width={28} height={28}
            alt="" aria-hidden className="h-7 w-7 shrink-0" draggable={false} />
          <span className="text-sm text-sidebar-text-active" style={{ fontFamily: "'Nunito', sans-serif", fontWeight: 800, letterSpacing: '0.01em' }}>SayItLocally</span>
        </div>
      )}
      <div ref={navGroupRef} className="relative flex-1 space-y-1 px-3">
        {dailyNavItems.map(({ to, icon, labelKey }) => (
          <NavItem key={to} to={to} icon={icon} label={t(labelKey)} />
        ))}

        <div className="px-1 py-3">
          <div className="h-px bg-[linear-gradient(to_right,transparent_0%,hsl(var(--sidebar-border))_5%,hsl(var(--sidebar-border))_95%,transparent_100%)]" />
        </div>
        {configNavItems.map(({ to, icon, labelKey }) => (
          <NavItem key={to} to={to} icon={icon} label={t(labelKey)} />
        ))}
        {/* 滑塊放最後：放第一個的話 space-y-1 會給"首頁"加一個 margin-top，整列下移 4px。 */}
        <span ref={navIndicatorRef} aria-hidden className="sliding-indicator rounded-lg bg-sidebar-item-active" />
      </div>

      <div className="space-y-3 px-3 pt-4">
        <div className="h-px bg-[linear-gradient(to_right,transparent_0%,hsl(var(--sidebar-border))_5%,hsl(var(--sidebar-border))_95%,transparent_100%)]" />
        <FooterIcons />
      </div>
    </nav>
  )
}
