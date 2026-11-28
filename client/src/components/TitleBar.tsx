import { useEffect, useState } from 'react'
import * as bridge from '@/services/bridge'
import { Minus, Square, X, Wand2 } from 'lucide-react'
import appIcon from '@/assets/ico-frame-48x48.png'
import { useAiEnabled, useAiEnabledReady } from '@/hooks/useAiEnabled'
import { useActivePreset } from '@/hooks/useActivePreset'
import { toggleAiEnabled } from '@/stores/aiEnabled'
import { Tooltip } from '@/components/ui/tooltip'
import { useT } from '@/i18n/useT'
import { recordedPromptPresetDisplayName } from '@/i18n/displayNames'
import { IS_MAC } from '@/lib/platform'

/**
 * 主視窗標題欄。
 *
 * Windows：無系統邊框，自繪最小化/最大化/關閉，拖拽靠 WebView2 認的 `-webkit-app-region`。
 * macOS：保留系統紅綠燈（tauri.macos.conf.json 裡 titleBarStyle=Overlay），左側給它們留位，
 * 不畫自己的視窗按鈕；WKWebView 不認 `-webkit-app-region`，拖拽改用 Tauri 的
 * `data-tauri-drag-region`（只對帶這個屬性的元素本身生效，按鈕照常可點）。
 */
export default function TitleBar() {
  const t = useT()
  const aiEnabled = useAiEnabled()
  // 冷啟動時 AI 初始值非同步讀回：就緒前開關先隱藏、不放動畫，避免自己從關跳到開
  const ready = useAiEnabledReady()
  // 同 AppearancePage：顯示與「允許過渡」必須錯開一幀，否則揭開那一刻會把
  // 開關從預設(關)到已儲存(開)真的滑動一遍。
  const [animate, setAnimate] = useState(false)
  useEffect(() => {
    if (!ready || animate) return
    const id = requestAnimationFrame(() => requestAnimationFrame(() => setAnimate(true)))
    return () => cancelAnimationFrame(id)
  }, [ready, animate])
  const activePreset = useActivePreset()
  const presetName = activePreset.name
    ? recordedPromptPresetDisplayName(activePreset.id, activePreset.name)
    : t('titleBar.defaultPreset')

  const dragRegion = IS_MAC ? { 'data-tauri-drag-region': true } : {}
  const dragStyle = IS_MAC ? undefined : ({ WebkitAppRegion: 'drag' } as React.CSSProperties)
  const noDragStyle = IS_MAC ? undefined : ({ WebkitAppRegion: 'no-drag' } as React.CSSProperties)

  return (
    <div className={IS_MAC
      ? 'flex h-10 items-center justify-end bg-titlebar border-b select-none'
      : 'flex h-10 items-center justify-between bg-titlebar border-b select-none'}
      style={dragStyle} {...dragRegion}>
      {/* mac：左邊歸系統紅綠燈，圖示和名字放在側欄最上面（見 Sidebar），標題欄只留 AI 整理開關 */}
      {!IS_MAC && (
        <div className="flex items-center gap-2.5 pl-3">
          <img src={appIcon} alt="SayItLocally" className="h-7 w-7" draggable={false} />
          <span className="text-sm text-foreground" style={{ fontFamily: "'Nunito', sans-serif", fontWeight: 800, letterSpacing: '0.01em' }}>SayItLocally</span>
        </div>
      )}
      <div className="flex items-center" {...dragRegion}>
        {/* mac：右邊留出和左側紅綠燈一樣的 16px 邊距，別貼著視窗圓角 */}
        <div className={IS_MAC ? 'flex items-center pr-4' : 'flex items-center pr-2'} style={noDragStyle}>
          <Tooltip content={t('titleBar.presetTooltip', { name: presetName })}>
            <button
              type="button"
              role="switch"
              aria-checked={aiEnabled}
              onClick={() => { void toggleAiEnabled() }}
              className="flex items-center gap-1.5 rounded-md px-2 py-1 transition-colors hover:bg-accent"
              aria-label={t('titleBar.aiCleanupToggle')}
            >
              <Wand2 className={aiEnabled ? 'h-3.5 w-3.5 text-primary' : 'h-3.5 w-3.5 text-muted-foreground'} />
              <span className={aiEnabled ? 'text-xs text-foreground' : 'text-xs text-muted-foreground'}>{t('titleBar.aiCleanup')}</span>
              <span className={`relative h-4 w-7 shrink-0 rounded-full ${animate ? 'transition-colors' : ''} ${aiEnabled ? 'bg-primary' : 'bg-muted'}`} style={ready ? undefined : { visibility: 'hidden' }}>
                <span className={`absolute left-0.5 top-0.5 h-3 w-3 rounded-full bg-card shadow ${animate ? 'transition-transform' : ''} ${aiEnabled ? 'translate-x-3' : ''}`} />
              </span>
            </button>
          </Tooltip>
        </div>
        {!IS_MAC && (
          <div className="flex" style={noDragStyle}>
            <button onClick={() => bridge.minimize()}
              className="flex h-10 w-11 items-center justify-center hover:bg-accent"
              aria-label={t('window.minimize')}>
              <Minus className="h-4 w-4" />
            </button>
            <button onClick={() => bridge.maximize()}
              className="flex h-10 w-11 items-center justify-center hover:bg-accent"
              aria-label={t('window.maximize')}>
              <Square className="h-3 w-3" />
            </button>
            <button onClick={() => bridge.close()}
              className="flex h-10 w-11 items-center justify-center hover:bg-titlebar-close-hover hover:text-titlebar-close-hover-text"
              aria-label={t('window.close')}>
              <X className="h-4 w-4" />
            </button>
          </div>
        )}
      </div>
    </div>
  )
}
