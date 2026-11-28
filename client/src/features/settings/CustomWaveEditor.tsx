// 波形主題「自定義」展開後的編輯區：一行，推薦色塊 + 自選，單色 / 漸變切換放在行尾。
//
// 刻意只有一行（使用者 2026-11-03 連著否了兩版：畫成小懸浮窗的推薦配色「更醜」，帶標題、
// 起點/終點標籤和色值輸入框的「還是複雜」）。效果看下面原有的懸浮窗預覽，這裡不再畫預覽。

import { useEffect, useId, useLayoutEffect, useRef, useState, type CSSProperties, type ReactNode } from 'react'
import { createPortal } from 'react-dom'
import { HexColorInput, HexColorPicker } from 'react-colorful'
import { Plus } from 'lucide-react'
import { Segmented } from '@/components/ui/segmented'
import { Tooltip } from '@/components/ui/tooltip'
import { cn } from '@/lib/utils'
import { useT } from '@/i18n/useT'
import {
  buildWavePalette,
  GRADIENT_WAVE_PRESETS,
  needsBrightening,
  SOLID_WAVE_PRESETS,
  visibleOnBlack,
  type CustomWaveColors,
  type CustomWaveMode,
} from '@/lib/overlayWaveColors'

/** 色塊背景：用調色盤取樣拼 linear-gradient，和懸浮窗實際畫出來的一致（深色已調亮）。 */
function swatchBackground(custom: CustomWaveColors): string {
  const stops = buildWavePalette(custom.mode === 'solid' ? 1 : 5, 'custom', custom)
  return stops.length === 1 ? stops[0] : `linear-gradient(to right, ${stops.join(', ')})`
}

/**
 * 懸停提示：用應用自己的 Tooltip，不用原生 title（WebView 畫的是系統樣式的小框，和介面不搭；使用者 2026-11-03）。
 * 寫出色值，和點開後取色面板裡的色值框是同一種寫法。
 */
function tipText(label: string, hex: string): string {
  return `${label} ${hex.toUpperCase()}`
}

// 選中只用一圈細環表示，環和色塊之間留一道底色縫。1.5px 半透明前景色：
// 原來 2px 純前景色的環使用者嫌「又粗又黑」（2026-11-03），這個粗細在淺色、深色主題下都還認得出。
const SELECTED_RING = 'ring-[1.5px] ring-foreground/50 ring-offset-2 ring-offset-background'
const ringClass = (active: boolean) => cn(
  'outline-none transition-shadow focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background',
  active ? SELECTED_RING : 'hover:ring-[1.5px] hover:ring-foreground/20 hover:ring-offset-2 hover:ring-offset-background',
)

const PANEL_GAP = 8
const VIEWPORT_MARGIN = 8

/**
 * 自己的取色彈窗：顏色區 + 色相條 + HEX 輸入框。
 *
 * 為什麼不用原生 <input type="color">：Windows 上 WebView2 彈的是 Chromium 自帶取色框，預設只顯示 RGB，
 * 切到 HEX 要點數字旁一個不起眼的箭頭；mac 上又是一塊單獨漂著的系統顏色面板。使用者 2026-11-03 指出
 * 懸停提示寫著 HEX、點進去卻只有 RGB，「完全沒有辦法發現」。兩個平台現在是同一個面板。
 *
 * 面板掛在 body 上、fixed 定位：設定卡片有 overflow，放在卡片裡會被裁掉。
 * 拖動時連續觸發 onChange，寫盤由上層合併節流。
 */
function ColorPopover({ color, label, onChange, className, style, children }: {
  color: string
  label: string
  onChange: (hex: string) => void
  className?: string
  style?: CSSProperties
  children?: ReactNode
}) {
  const [open, setOpen] = useState(false)
  const [pos, setPos] = useState<{ top: number; left: number } | null>(null)
  const triggerRef = useRef<HTMLButtonElement>(null)
  const panelRef = useRef<HTMLDivElement>(null)

  // 預設放在觸發點下方居中；下面放不下就翻到上方，左右貼著視窗邊收住。
  const place = () => {
    const trigger = triggerRef.current
    const panel = panelRef.current
    if (!trigger || !panel) return
    const tr = trigger.getBoundingClientRect()
    const pw = panel.offsetWidth
    const ph = panel.offsetHeight
    let top = tr.bottom + PANEL_GAP
    if (top + ph > window.innerHeight - VIEWPORT_MARGIN) top = Math.max(VIEWPORT_MARGIN, tr.top - ph - PANEL_GAP)
    const left = Math.min(Math.max(VIEWPORT_MARGIN, tr.left + tr.width / 2 - pw / 2), window.innerWidth - pw - VIEWPORT_MARGIN)
    setPos({ top, left })
  }

  useLayoutEffect(() => {
    if (open) place()
    else setPos(null)
  }, [open])

  useEffect(() => {
    if (!open) return
    const onDown = (e: MouseEvent) => {
      const target = e.target as Node
      if (panelRef.current?.contains(target) || triggerRef.current?.contains(target)) return
      setOpen(false)
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return
      e.stopPropagation()
      setOpen(false)
      triggerRef.current?.focus()
    }
    // 設定頁在 main 裡滾動，不是 window：用捕獲階段接住任何一層的滾動，跟著重新定位
    const onMove = () => place()
    document.addEventListener('mousedown', onDown)
    document.addEventListener('keydown', onKey, true)
    window.addEventListener('scroll', onMove, true)
    window.addEventListener('resize', onMove)
    return () => {
      document.removeEventListener('mousedown', onDown)
      document.removeEventListener('keydown', onKey, true)
      window.removeEventListener('scroll', onMove, true)
      window.removeEventListener('resize', onMove)
    }
  }, [open])

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        aria-label={label}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className={className}
        style={style}
      >
        {children}
      </button>
      {open && createPortal(
        <div
          ref={panelRef}
          role="dialog"
          aria-label={label}
          className="sayit-colorful fixed z-[9999] w-56 rounded-lg border border-border bg-card p-3 text-card-foreground shadow-lg"
          style={{ top: pos ? pos.top : -9999, left: pos ? pos.left : -9999 }}
        >
          <p className="mb-2.5 text-xs font-medium">{label}</p>
          <HexColorPicker color={color} onChange={(hex) => onChange(hex.toLowerCase())} />
          <div className="mt-3 flex items-center gap-2">
            <span aria-hidden className="h-7 w-7 shrink-0 rounded-md border border-border" style={{ background: color }} />
            <HexColorInput
              prefixed
              color={color}
              onChange={(hex) => onChange(hex.toLowerCase())}
              aria-label={`${label} HEX`}
              spellCheck={false}
              className="h-7 min-w-0 flex-1 rounded-md border border-input-border bg-input-bg px-2 font-mono text-xs uppercase text-foreground transition-colors focus:border-input-focus-border focus:outline-none"
            />
          </div>
        </div>,
        document.body,
      )}
    </>
  )
}

/** 推薦色塊：一個按鈕，點一下就用這組顏色。 */
function PresetSwatch({ name, active, wide, background, onPick }: {
  name: string
  active: boolean
  /** 漸變膠囊比單色圓點寬 */
  wide?: boolean
  background: string
  onPick: () => void
}) {
  return (
    <Tooltip content={name}>
      <button
        type="button"
        aria-label={name}
        aria-pressed={active}
        onClick={onPick}
        className={cn('h-5 rounded-full', wide ? 'w-10' : 'w-5', ringClass(active))}
        style={{ background }}
      />
    </Tooltip>
  )
}

/** 單色自選：一顆色塊，點開取色彈窗。 */
function PickDot({ color, label, active, empty, onChange }: {
  color: string
  label: string
  active: boolean
  /** 還沒自選過（當前值是推薦色）時畫成虛線圈 + 加號，免得和推薦色塊混在一起 */
  empty?: boolean
  onChange: (hex: string) => void
}) {
  return (
    <Tooltip content={empty ? label : tipText(label, color)}>
      <ColorPopover
        color={color}
        label={label}
        onChange={onChange}
        className={cn(
          'inline-flex h-5 w-5 shrink-0 items-center justify-center rounded-full', ringClass(active),
          'aria-expanded:ring-[1.5px] aria-expanded:ring-foreground/50 aria-expanded:ring-offset-2 aria-expanded:ring-offset-background',
          empty && 'border border-dashed border-muted-foreground/50 text-muted-foreground',
        )}
        style={empty ? undefined : { background: color }}
      >
        {empty && <Plus className="h-3 w-3" aria-hidden />}
      </ColorPopover>
    </Tooltip>
  )
}

/**
 * 自選漸變：兩顆色點（左 = 起點，右 = 終點）用一根 2px 的漸變細線連起來。
 * 演變（使用者 2026-11-03）：「圓點—橫槓—圓點」嫌橫槓多餘 → 一整顆膠囊看不出起點終點 → 現在這樣，
 * 細線就是這兩色的漸變；6px 嫌粗，改 2px；外面原來有一圈選中框，點到框中間什麼都不發生，使用者以為不能改。
 * 所以：沒有外框，整條分成左右兩半，各是一個按鈕，點到線上也能開啟；滑鼠在哪一半，那頭的點就放大一點。
 * 選中（當前是自選）時，兩顆點各帶一圈和推薦色塊同樣的細環；正在改的那一頭放大一點並帶同樣的細環。
 * 當前是推薦配色時也照常畫出那兩色，點一下就是從這個推薦色開始改。
 */
function GradientPick({ value, active, labels, onChange }: {
  value: CustomWaveColors
  active: boolean
  labels: { from: string; to: string }
  onChange: (next: CustomWaveColors) => void
}) {
  const half = (side: 'from' | 'to') => (
    <Tooltip className="h-full flex-1" content={tipText(labels[side], value[side])}>
      <ColorPopover
        color={value[side]}
        label={labels[side]}
        onChange={(hex) => onChange({ ...value, [side]: hex })}
        className={cn('group/stop flex h-full w-full items-center rounded-full outline-none', side === 'from' ? 'justify-start' : 'justify-end')}
      >
        <span
          aria-hidden
          className={cn(
            'h-4 w-4 rounded-full shadow-sm transition-transform duration-150 ease-out group-hover/stop:scale-[1.15]',
            active && SELECTED_RING,
            'group-focus-visible/stop:ring-2 group-focus-visible/stop:ring-ring group-focus-visible/stop:ring-offset-2 group-focus-visible/stop:ring-offset-background',
            'group-aria-expanded/stop:scale-[1.15] group-aria-expanded/stop:ring-[1.5px] group-aria-expanded/stop:ring-foreground/50 group-aria-expanded/stop:ring-offset-2 group-aria-expanded/stop:ring-offset-background',
          )}
          style={{ background: visibleOnBlack(value[side]) }}
        />
      </ColorPopover>
    </Tooltip>
  )
  return (
    <span className="relative inline-flex h-6 w-[4.5rem] shrink-0 items-center">
      {/* 2px：再粗就成了一根搶眼的色條（使用者 2026-11-03 嫌 6px 太粗），細線只負責把兩頭連起來 */}
      <span aria-hidden className="pointer-events-none absolute inset-x-2 top-1/2 h-0.5 -translate-y-1/2 rounded-full" style={{ background: swatchBackground(value) }} />
      {half('from')}
      {half('to')}
    </span>
  )
}

export function CustomWaveEditor({ value, onChange, animate }: {
  value: CustomWaveColors
  onChange: (next: CustomWaveColors) => void
  animate: boolean
}) {
  const t = useT()
  const presetsId = useId()
  const gradient = value.mode === 'gradient'
  const brightened = needsBrightening(value.from) || (gradient && needsBrightening(value.to))

  const solidIsPreset = SOLID_WAVE_PRESETS.includes(value.from)
  const gradientIsPreset = GRADIENT_WAVE_PRESETS.some(([from, to]) => from === value.from && to === value.to)

  return (
    <div className="mt-3 space-y-2">
      {/* 色塊從左邊起排，和上面的主題卡對齊；單色 / 漸變是次要切換，縮成 xs 放在行尾（使用者 2026-11-03：放在最前面偏大、位置彆扭） */}
      <div className="flex flex-wrap items-center gap-x-4 gap-y-3">
        <span id={presetsId} className="sr-only">{t('appearance.wave.presets')}</span>
        <div role="group" aria-labelledby={presetsId} className="flex flex-wrap items-center gap-2.5">
          {gradient
            ? GRADIENT_WAVE_PRESETS.map(([from, to]) => {
              const preset: CustomWaveColors = { mode: 'gradient', from, to }
              return (
                <PresetSwatch
                  key={`${from}-${to}`}
                  name={`${from.toUpperCase()} → ${to.toUpperCase()}`}
                  active={from === value.from && to === value.to}
                  wide
                  background={swatchBackground(preset)}
                  onPick={() => onChange(preset)}
                />
              )
            })
            : SOLID_WAVE_PRESETS.map((from) => (
              <PresetSwatch
                key={from}
                name={from.toUpperCase()}
                active={from === value.from}
                background={swatchBackground({ ...value, mode: 'solid', from })}
                onPick={() => onChange({ ...value, from })}
              />
            ))}

          {/* 自選：和推薦色塊之間一道細豎線隔開 */}
          <span aria-hidden className="mx-0.5 h-4 w-px bg-border" />
          {gradient ? (
            <GradientPick
              value={value}
              active={!gradientIsPreset}
              labels={{ from: t('appearance.wave.from'), to: t('appearance.wave.to') }}
              onChange={onChange}
            />
          ) : (
            <PickDot color={value.from} label={t('appearance.wave.color')} active={!solidIsPreset} empty={solidIsPreset} onChange={(from) => onChange({ ...value, from })} />
          )}
        </div>

        <Segmented<CustomWaveMode>
          label={t('appearance.wave.customStyle')}
          size="xs"
          value={value.mode}
          options={[
            { value: 'solid', label: t('appearance.wave.solid') },
            { value: 'gradient', label: t('appearance.wave.gradient') },
          ]}
          onChange={(mode) => onChange({ ...value, mode })}
          animated={animate}
          className="ml-auto shrink-0 gap-0.5"
        />
      </div>

      {/* 面板裡是使用者選的原色，這裡把調亮後的實際顏色畫出來，免得以為沒生效 */}
      {brightened && (
        <p className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
          <span>{t('appearance.wave.brightened')}</span>
          <span
            aria-hidden
            className={cn('inline-block h-3 rounded-full', gradient ? 'w-10' : 'w-3')}
            style={{ background: swatchBackground(value) }}
          />
        </p>
      )}
    </div>
  )
}
