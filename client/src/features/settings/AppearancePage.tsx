// 外觀設定頁面 — 主題 + 懸浮窗樣式 + 預覽

import { useEffect, useMemo, useRef, useState } from 'react'
import { open as shellOpen } from '@tauri-apps/plugin-shell'
import { ExternalLink, Info } from 'lucide-react'
import { Tooltip } from '@/components/ui/tooltip'
import { Card, CardContent } from '@/components/ui/card'
import { Switch } from '@/components/ui/switch'
import { Segmented } from '@/components/ui/segmented'
import { themeList } from '@/themes'
import { switchTheme, getActiveThemeId } from '@/stores/theme'
import { getSetting, setSetting } from '@/services/store'
import { refreshOverlaySettings, setStreamingDisplayCache } from '@/services/recorder'
import { OVERLAY_WIDTH_PRESETS, type OverlayWidthPreset } from '@/services/recorder/types'
import {
  buildWavePalette,
  DEFAULT_CUSTOM_WAVE,
  getWaveTimerColor,
  normalizeCustomWave,
  normalizeWaveTheme,
  type CustomWaveColors,
  type OverlayWaveTheme,
} from '@/lib/overlayWaveColors'
import { CustomWaveEditor } from './CustomWaveEditor'
import { type TranslationKey } from '@/i18n'
import { useT } from '@/i18n/useT'
import { loadClickGlowEnabled, setClickGlowEnabled } from '@/stores/clickGlow'

const OVERLAY_OPTIONS: Array<{
  theme: Exclude<OverlayWaveTheme, 'custom'>
  labelKey: TranslationKey
  barColors: string[]
}> = [
    { theme: 'black-white', labelKey: 'appearance.wave.blackWhite', barColors: ['#e2e8f0', '#cbd5e1', '#94a3b8'] },
    { theme: 'black-blue', labelKey: 'appearance.wave.blackBlue', barColors: ['#22d3ee', '#3b82f6', '#6366f1'] },
    { theme: 'black-rainbow', labelKey: 'appearance.wave.blackRainbow', barColors: ['#4ade80', '#facc15', '#fb923c', '#f87171'] },
  ]

/** 自定義配色連續拖動取色器時，寫盤與重新整理懸浮窗設定合併到停手之後。 */
// [自訂] S58：本 fork 只用本機模式，不顯示雲端供應商控制台／申請文件連結（點擊會開外部瀏覽器）。
// 改回 true 即恢復上游行為。
const SHOW_CLOUD_CONSOLE_LINKS = false

const CUSTOM_WAVE_SAVE_DELAY_MS = 250

// 由長到短排列：從「最完整」往「最剋制」讀，比反過來更符合挑尺寸的直覺。
const WIDTH_OPTIONS: Array<{ value: OverlayWidthPreset; labelKey: TranslationKey }> = [
  { value: 'long', labelKey: 'appearance.width.long' },
  { value: 'medium', labelKey: 'appearance.width.medium' },
  { value: 'short', labelKey: 'appearance.width.short' },
]

function OverlayPreview({ theme, customWave, showDuration, barCount, streaming }: {
  theme: OverlayWaveTheme
  customWave: CustomWaveColors
  showDuration: boolean
  barCount: number
  streaming: boolean
}) {
  const t = useT()
  const palette = useMemo(() => buildWavePalette(barCount, theme, customWave), [barCount, theme, customWave])
  const previewText = t('appearance.streamingPreviewText')
  const barRefs = useRef<Array<HTMLDivElement | null>>([])
  const [typed, setTyped] = useState('')

  // 流式預覽：迴圈把示例文字一個字一個字打出來，模擬"邊說邊出字"的動態效果
  useEffect(() => {
    if (!streaming) {
      setTyped('')
      return
    }
    let i = 0
    let timer: ReturnType<typeof setTimeout>
    const step = () => {
      if (i <= previewText.length) {
        setTyped(previewText.slice(0, i))
        i += 1
        timer = setTimeout(step, 130)
      } else {
        // 打完停頓一下再從頭迴圈
        timer = setTimeout(() => { i = 0; step() }, 1600)
      }
    }
    step()
    return () => clearTimeout(timer)
  }, [streaming, previewText])

  useEffect(() => {
    const heights = new Array(barCount).fill(3)
    let running = true
    let rafId = 0
    let lastFrame = 0
    const FRAME_INTERVAL = 1000 / 30 // 30fps 足夠流暢，且不佔滿主執行緒

    const animate = (now: number) => {
      if (!running) return
      if (now - lastFrame >= FRAME_INTERVAL) {
        lastFrame = now
        for (let i = 0; i < heights.length; i++) {
          const target = 3 + Math.random() * 15
          heights[i] = heights[i] + (target - heights[i]) * 0.15
          const el = barRefs.current[i]
          if (el) {
            const h = Math.min(18, Math.max(3, heights[i]))
            el.style.height = `${h}px`
            el.style.opacity = String(0.7 + (h / 18) * 0.3)
          }
        }
      }
      rafId = requestAnimationFrame(animate)
    }
    rafId = requestAnimationFrame(animate)
    return () => {
      running = false
      cancelAnimationFrame(rafId)
    }
  }, [barCount])

  return (
    <div className="flex flex-col items-center gap-3">
      {/* 流式即時字幕氣泡（開啟時顯示，帶打字動畫） */}
      {streaming && (
        <div className="relative w-[260px] rounded-2xl border border-slate-600 bg-black px-3.5 py-2.5 shadow-[0_6px_16px_rgba(0,0,0,0.35)]">
          <span className="mb-1 block text-[10px] font-medium tracking-[0.18em] text-slate-400">{t('appearance.streamingPreviewLabel')}</span>
          {/* 內容驅動、底部對齊，和真實懸浮窗一致 */}
          <div className="flex max-h-[40px] flex-col justify-end overflow-hidden text-left text-[13px] leading-5 text-slate-100">
            <div>
              {typed}
              <span
                className="ml-0.5 inline-block h-[1.05em] w-[2px] rounded-full align-middle"
                style={{ backgroundColor: '#f1f5f9', animation: 'caret-blink 1.1s ease-in-out infinite' }}
              />
            </div>
          </div>
          {/* 朝下尖角，指向下方錄音膠囊 */}
          <span
            className="absolute left-1/2 -translate-x-1/2"
            style={{ bottom: '-7px', width: 0, height: 0, borderLeft: '7px solid transparent', borderRight: '7px solid transparent', borderTop: '7px solid #000' }}
          />
        </div>
      )}
      {/* 1:1 還原真實懸浮窗樣式 */}
      <div className="flex items-center rounded-full border border-slate-600 bg-black px-4 py-2 shadow-[0_6px_16px_rgba(0,0,0,0.35)]">
        <div className="flex items-center gap-[2px]" style={{ height: '20px' }}>
          {Array.from({ length: barCount }, (_, index) => {
            const color = palette[index]
            return (
              <div
                key={index}
                ref={(el) => { barRefs.current[index] = el }}
                className="w-[2.5px] rounded-full"
                style={{
                  backgroundColor: color,
                  height: '3px',
                  opacity: 0.7,
                  transition: 'height 50ms ease-out, opacity 50ms ease-out',
                }}
              />
            )
          })}
        </div>
        {showDuration && (
          <span
            className="ml-1.5 min-w-[24px] text-right font-mono tabular-nums text-xs"
            style={{ color: getWaveTimerColor(theme, customWave) }}
          >
            3.2s
          </span>
        )}
      </div>
      <span className="text-xs text-muted-foreground">{t('appearance.overlayPreview')}</span>
    </div>
  )
}

export default function AppearancePage() {
  const t = useT()
  const [activeTheme, setActiveTheme] = useState(getActiveThemeId)
  const [overlayWaveTheme, setOverlayWaveTheme] = useState<OverlayWaveTheme>('black-rainbow')
  const [customWave, setCustomWave] = useState<CustomWaveColors>(DEFAULT_CUSTOM_WAVE)
  const customWaveSaveRef = useRef<{ timer: ReturnType<typeof setTimeout>; value: CustomWaveColors } | null>(null)
  const [overlayShowDuration, setOverlayShowDuration] = useState(true)
  const [overlayWidth, setOverlayWidth] = useState<OverlayWidthPreset>('medium')
  const [streamingDisplay, setStreamingDisplay] = useState(false)
  // 讀到已儲存值之前，控制元件先隱藏、且不帶過渡：避免「預設值 → 已儲存值」閃一下。
  // 注意過渡也必須一起關掉 —— visibility:hidden 只是看不見，CSS 過渡照樣會跑；
  // 隱藏期間選中項仍是預設值，揭開的同一刻顏色會從舊值過渡到新值，看起來就是
  // 「預設項高亮淡出、已儲存項淡入」（懸浮窗長度從「中」閃到「短」正是如此）。
  // 隱藏一律用內聯 style，不用 Tailwind 的 invisible 類：類要靠掃描原始碼產出，
  // 一旦跑的是舊的 CSS 產物這層保護就整條失效（曾因此反覆排查很久）。
  const [ready, setReady] = useState(false)
  // animate 與 ready 必須分開：
  // 按 CSS 過渡規範，瀏覽器看的是「變化之後」的樣式裡有沒有 transition。若在揭開、
  // 賦值的同一幀把 transition 加上，瀏覽器就會認為「有過渡且顏色變了」，於是把
  // 「預設項高亮淡出、已儲存項淡入」真的動畫一遍 —— 那就是反覆沒修掉的那個「閃」。
  // 因此：揭開的那一幀仍然不帶過渡，等再下一幀才允許過渡（此時顏色已無變化）。
  const [animate, setAnimate] = useState(false)
  const [clickGlow, setClickGlow] = useState(true)

  useEffect(() => {
    // 先把所有值取回，再在同一個同步塊裡一次性落值 + 置 ready：React 會把這批更新
    // 合成一次渲染，因此不存在「已顯示但值還沒到」的中間態（那正是閃一下的成因）。
    //
    // 兩個坑都在這裡躲掉了：
    //  1. 每項自帶 catch 兜底 —— Promise.all 是 fail-fast，只要有一項 reject 就會
    //     立刻往下走，那時其它項還沒回來，ready 會在值到位前就被置上（並留下一個
    //     未处理的 rejection）。
    //  2. 不用 requestAnimationFrame —— 那會把 setReady 丟到與賦值不同的批次裡，
    //     同樣可能先提交出一幀「已顯示但值是預設值」。
    let cancelled = false
    void (async () => {
      const [showDuration, streaming, waveTheme, waveCustom, width, glow] = await Promise.all([
        getSetting('overlayShowDuration', true).catch(() => true),
        getSetting('streamingDisplayEnabled', false).catch(() => false),
        getSetting('overlayWaveTheme', 'black-rainbow').catch(() => 'black-rainbow'),
        getSetting<unknown>('overlayWaveCustom', DEFAULT_CUSTOM_WAVE).catch(() => DEFAULT_CUSTOM_WAVE),
        getSetting('overlayWidth', 'medium').catch(() => 'medium'),
        loadClickGlowEnabled(),
      ])
      if (cancelled) return
      setOverlayShowDuration(Boolean(showDuration))
      setStreamingDisplay(Boolean(streaming))
      setClickGlow(glow)
      setOverlayWaveTheme(normalizeWaveTheme(waveTheme, 'black-rainbow'))
      setCustomWave(normalizeCustomWave(waveCustom))
      const w = width as OverlayWidthPreset
      if (w === 'short' || w === 'medium' || w === 'long') setOverlayWidth(w)
      setReady(true)
      requestAnimationFrame(() => requestAnimationFrame(() => {
        if (!cancelled) setAnimate(true)
      }))
    })()
    return () => { cancelled = true }
  }, [])

  const handleThemeChange = async (themeId: string) => {
    await switchTheme(themeId)
    setActiveTheme(themeId)
  }

  const themeCards: Array<{ theme: OverlayWaveTheme; labelKey: TranslationKey; barColors: string[] }> = [
    ...OVERLAY_OPTIONS,
    // 自定義卡的色條直接取當前配色，改了顏色卡片上也跟著變
    { theme: 'custom', labelKey: 'appearance.wave.custom', barColors: buildWavePalette(4, 'custom', customWave) },
  ]

  const handleOverlayThemeChange = async (theme: OverlayWaveTheme) => {
    setOverlayWaveTheme(theme)
    await setSetting('overlayWaveTheme', theme)
    await refreshOverlaySettings()
  }

  const persistCustomWave = async (value: CustomWaveColors) => {
    await setSetting('overlayWaveCustom', value)
    await refreshOverlaySettings()
  }

  // 預覽立刻跟手，寫盤合併到停手之後：拖動取色器每秒會觸發幾十次 onChange，
  // 每次都寫庫 + 讓錄音器重讀設定沒有意義。
  const handleCustomWaveChange = (next: CustomWaveColors) => {
    setCustomWave(next)
    if (customWaveSaveRef.current) clearTimeout(customWaveSaveRef.current.timer)
    const timer = setTimeout(() => {
      customWaveSaveRef.current = null
      void persistCustomWave(next)
    }, CUSTOM_WAVE_SAVE_DELAY_MS)
    customWaveSaveRef.current = { timer, value: next }
  }

  // 停手不到 250ms 就切走頁面時，把還沒寫盤的那次補上，否則最後一下改動會丟。
  useEffect(() => () => {
    const pending = customWaveSaveRef.current
    if (!pending) return
    clearTimeout(pending.timer)
    customWaveSaveRef.current = null
    void persistCustomWave(pending.value)
  }, [])

  const handleToggleDuration = async () => {
    const next = !overlayShowDuration
    setOverlayShowDuration(next)
    await setSetting('overlayShowDuration', next)
    await refreshOverlaySettings()
  }

  const handleOverlayWidthChange = async (preset: OverlayWidthPreset) => {
    setOverlayWidth(preset)
    await setSetting('overlayWidth', preset)
    await refreshOverlaySettings()
  }

  const handleToggleClickGlow = () => {
    const next = !clickGlow
    setClickGlow(next)
    void setClickGlowEnabled(next)
  }

  const handleToggleStreamingDisplay = () => {
    const next = !streamingDisplay
    setStreamingDisplay(next)
    setStreamingDisplayCache(next) // 立即同步錄音器快取，無需重啟即可生效
    void setSetting('streamingDisplayEnabled', next)
  }

  return (
    <div className="mx-auto max-w-4xl p-8">
      <h1 className="mb-6 text-2xl font-bold">{t('appearance.title')}</h1>

      <div className="space-y-6">
        <Card>
          <CardContent className="p-6">
            <h2 className="mb-4 text-lg font-semibold">{t('appearance.appTheme')}</h2>
            {/* 按最小寬度自動分列：固定三列時，視窗拉到最窄（800）英文介面的「Light / Dark / Warm」會撐出卡片外 */}
            <div className="grid grid-cols-[repeat(auto-fit,minmax(11rem,1fr))] gap-3">
              {themeList.map((theme) => (
                <button
                  key={theme.id}
                  type="button"
                  aria-pressed={activeTheme === theme.id}
                  onClick={() => void handleThemeChange(theme.id)}
                  className={`flex items-center gap-3 rounded-lg border p-3 outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background ${activeTheme === theme.id
                    ? 'border-primary bg-primary/5'
                    : 'border-border hover:bg-accent'
                    }`}
                >
                  <div className="flex gap-1">
                    {Object.values(theme.previewColors).map((color, i) => (
                      <span
                        key={i}
                        className="h-5 w-5 rounded-full border border-border/50"
                        style={{ backgroundColor: color }}
                      />
                    ))}
                  </div>
                  <span className="text-sm font-medium">{theme.name}</span>
                </button>
              ))}
            </div>
          </CardContent>
        </Card>

        {/* [自訂] SC-63：點擊光輝特效開關（預設開） */}
        <Card>
          <CardContent className="p-6">
            <h2 className="mb-4 text-lg font-semibold">{t('appearance.effects')}</h2>
            <div className="flex items-center justify-between">
              <div className="pr-3">
                <p className="text-sm font-medium">{t('appearance.clickGlow')}</p>
                <p className="text-xs text-muted-foreground">{t('appearance.clickGlowDesc')}</p>
              </div>
              <Switch checked={clickGlow} onChange={handleToggleClickGlow} noAnimation={!animate} hidden={!ready} />
            </div>
          </CardContent>
        </Card>

        <Card>
          <CardContent className="p-6">
            <h2 className="mb-4 text-lg font-semibold">{t('appearance.overlayStyle')}</h2>

            {/* 整塊一起等值到位再顯示。除了選擇器，**懸浮窗預覽**那顆膠囊的長度也跟著
                overlayWidth 变（它有可见边框），只盖住选择器的话，仍会看到预览从「中」
                的长度一下缩到「短」——那正是之前反复没修掉的那个「闪」。 */}
            <div className="space-y-4" style={ready ? undefined : { visibility: 'hidden' }}>
              <div>
                <p className="mb-2 text-sm text-muted-foreground">{t('appearance.waveTheme')}</p>
                {/* 四張卡：預設視窗寬度下排成一行；視窗拉窄到 lg 以下時兩兩一行，免得「黑底彩色」和色條擠在一起 */}
                <div className="grid grid-cols-2 gap-2 lg:grid-cols-4" style={ready ? undefined : { visibility: 'hidden' }}>
                  {themeCards.map((option) => (
                    <button
                      key={option.theme}
                      type="button"
                      aria-pressed={overlayWaveTheme === option.theme}
                      onClick={() => void handleOverlayThemeChange(option.theme)}
                      className={`flex items-center justify-between gap-2 rounded-md border px-3 py-2 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background ${animate ? 'transition-colors' : ''} ${overlayWaveTheme === option.theme
                        ? 'border-primary bg-primary/5'
                        : 'border-border hover:bg-accent'
                        }`}
                    >
                      {/* 選中只靠邊框和底色表示；原來的圓點和邊框是同一個意思說了兩遍 */}
                      <span className={overlayWaveTheme === option.theme ? 'font-medium' : ''}>{t(option.labelKey)}</span>
                      <span className="flex shrink-0 gap-0.5">
                        {option.barColors.map((c, i) => (
                          <span key={i} className="h-3 w-1 rounded-sm" style={{ backgroundColor: c }} />
                        ))}
                      </span>
                    </button>
                  ))}
                </div>

                {overlayWaveTheme === 'custom' && (
                  <CustomWaveEditor value={customWave} onChange={handleCustomWaveChange} animate={animate} />
                )}

                {/* 只有一兩個字的選項用分段按鈕（和「識別語言」一致）；帶圖樣的波形主題才用卡片 */}
                <div className="mt-4 flex items-center justify-between gap-4">
                  <p id="overlay-width-label" className="text-sm font-medium">{t('appearance.overlayWidth')}</p>
                  <Segmented
                    labelledBy="overlay-width-label"
                    value={overlayWidth}
                    options={WIDTH_OPTIONS.map((option) => ({ value: option.value, label: t(option.labelKey) }))}
                    onChange={(value) => void handleOverlayWidthChange(value)}
                    animated={animate}
                    className="shrink-0 justify-end"
                  />
                </div>

                <div className="mt-4 flex items-center justify-between">
                  <div>
                    <p className="text-sm font-medium">{t('appearance.showDuration')}</p>
                    <p className="text-xs text-muted-foreground">{t('appearance.showDurationDesc')}</p>
                  </div>
                  <Switch checked={overlayShowDuration} onChange={handleToggleDuration} noAnimation={!animate} hidden={!ready} />
                </div>

                <div className="mt-4 flex items-center justify-between">
                  <div className="pr-3">
                    <div className="flex items-center gap-1.5">
                      <p className="text-sm font-medium">{t('appearance.streaming')}</p>
                      <Tooltip
                        variant="light"
                        content={`${t('appearance.streamingHelp')}
${t('appearance.streamingSetupNote')}`}
                      >
                        <Info className="h-3.5 w-3.5 shrink-0 cursor-help text-muted-foreground/50 transition-colors hover:text-muted-foreground" />
                      </Tooltip>
                    </div>
                    <p className="text-xs text-muted-foreground">
                      {t('appearance.streamingDesc')}{t('appearance.streamingShortNote')}
                      {SHOW_CLOUD_CONSOLE_LINKS && (
                      <button
                        type="button"
                        onClick={() => void shellOpen('https://console.volcengine.com/speech/new/setting/activate?projectName=default')}
                        className="inline-flex items-center gap-0.5 text-primary underline underline-offset-2 decoration-primary/50 transition-colors hover:decoration-primary"
                      >
                        {t('appearance.streamingActivate')}
                        <ExternalLink className="h-3 w-3" />
                      </button>
                      )}
                    </p>
                  </div>
                  <Switch checked={streamingDisplay} onChange={handleToggleStreamingDisplay} noAnimation={!animate} hidden={!ready} />
                </div>

                <div className="mt-4 flex justify-center">
                  <OverlayPreview theme={overlayWaveTheme} customWave={customWave} showDuration={overlayShowDuration} barCount={OVERLAY_WIDTH_PRESETS[overlayWidth].barCount} streaming={streamingDisplay} />
                </div>
              </div>
            </div>
          </CardContent>
        </Card>
      </div>
    </div>
  )
}
