/**
 * 懸浮窗波形配色：內建三套 + 使用者自定義（單色 / 兩色漸變）。
 *
 * 懸浮窗（overlay/Overlay.tsx）和設定頁預覽（AppearancePage）都從這裡取色。以前兩邊
 * 各抄了一份一模一樣的配色函式，改一邊忘另一邊，預覽就和真實懸浮窗對不上。
 *
 * 自定義顏色有兩條約束，都是因為膠囊底色固定是黑的：
 * 1. **亮度有下限**（OKLab L ≥ MIN_VISIBLE_L）。使用者挑了深藍/純黑的話波形在黑底上
 *    完全看不見，像懸浮窗壞了。寧可替他調亮，預覽裡直接畫調亮後的效果，所見即所得。
 * 2. **漸變在 OKLCH 裡插值**。sRGB / HSL 裡插兩種互補色，中段會發灰發髒；OKLCH 沿色相走，
 *    明度感知均勻，兩色漸變能得到接近內建彩虹那種乾淨的過渡。
 */

export type OverlayWaveTheme = 'black-white' | 'black-blue' | 'black-rainbow' | 'custom'

export type CustomWaveMode = 'solid' | 'gradient'

export interface CustomWaveColors {
  mode: CustomWaveMode
  /** 單色模式的顏色 / 漸變起點（波形最左邊），#rrggbb */
  from: string
  /** 漸變終點（波形最右邊），#rrggbb。單色模式下保留不用，切回漸變時還在 */
  to: string
}

export const DEFAULT_CUSTOM_WAVE: CustomWaveColors = {
  mode: 'gradient',
  from: '#a78bfa',
  to: '#f472b6',
}

/** 推薦配色。單色給一排色塊，漸變給成對的起止色，點一下就能用。 */
export const SOLID_WAVE_PRESETS: readonly string[] = [
  '#f472b6', '#a78bfa', '#60a5fa', '#2dd4bf', '#34d399', '#a3e635', '#fbbf24', '#f87171',
]

export const GRADIENT_WAVE_PRESETS: ReadonlyArray<readonly [string, string]> = [
  ['#a78bfa', '#f472b6'],
  ['#60a5fa', '#c084fc'],
  ['#22d3ee', '#a3e635'],
  ['#2dd4bf', '#818cf8'],
  ['#fbbf24', '#f472b6'],
  ['#fb923c', '#f43f5e'],
]

/**
 * 黑底上可辨認的最低 OKLab 明度。0.62 對應相對亮度約 0.24（中性灰），與純黑的對比度約 5.7:1。
 * 內建三套的條都比這亮得多，下限只攔真正會"消失"的深色，不會把使用者挑的中等亮度色改掉。
 */
export const MIN_VISIBLE_L = 0.62

const HEX_RE = /^#?([0-9a-f]{3}|[0-9a-f]{6})$/i

/** 把使用者輸入的色值規整成小寫 #rrggbb；不是合法色值返回 null。接受 3 位簡寫和省略 #。 */
export function parseHexColor(input: unknown): string | null {
  if (typeof input !== 'string') return null
  const m = HEX_RE.exec(input.trim())
  if (!m) return null
  let hex = m[1].toLowerCase()
  if (hex.length === 3) hex = hex.split('').map((c) => c + c).join('')
  return `#${hex}`
}

export function normalizeWaveTheme(value: unknown, fallback: OverlayWaveTheme): OverlayWaveTheme {
  if (value === 'black-white' || value === 'black-blue' || value === 'black-rainbow' || value === 'custom') {
    return value
  }
  return fallback
}

/** 設定/備份/payload 裡讀回來的值都不可信，逐欄位校驗，壞的欄位回落預設值。 */
export function normalizeCustomWave(value: unknown): CustomWaveColors {
  const raw = (value && typeof value === 'object') ? value as Record<string, unknown> : {}
  return {
    mode: raw.mode === 'solid' || raw.mode === 'gradient' ? raw.mode : DEFAULT_CUSTOM_WAVE.mode,
    from: parseHexColor(raw.from) ?? DEFAULT_CUSTOM_WAVE.from,
    to: parseHexColor(raw.to) ?? DEFAULT_CUSTOM_WAVE.to,
  }
}

export function sameCustomWave(a: CustomWaveColors, b: CustomWaveColors): boolean {
  return a.mode === b.mode && a.from === b.from && a.to === b.to
}

// ── OKLab / OKLCH（Björn Ottosson 的公式） ──────────────────────────────

interface Lch { l: number; c: number; h: number }

function srgbToLinear(c: number): number {
  return c <= 0.04045 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4)
}

function linearToSrgb(c: number): number {
  return c <= 0.0031308 ? 12.92 * c : 1.055 * Math.pow(c, 1 / 2.4) - 0.055
}

function hexToLch(hex: string): Lch {
  const n = parseInt(hex.slice(1), 16)
  const r = srgbToLinear(((n >> 16) & 255) / 255)
  const g = srgbToLinear(((n >> 8) & 255) / 255)
  const b = srgbToLinear((n & 255) / 255)
  const l_ = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b)
  const m_ = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b)
  const s_ = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b)
  const L = 0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_
  const A = 1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_
  const B = 0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_
  const h = (Math.atan2(B, A) * 180) / Math.PI
  return { l: L, c: Math.hypot(A, B), h: h < 0 ? h + 360 : h }
}

/** OKLCH → 線性前的 sRGB 三通道（可能越界，越界說明這個顏色在 sRGB 裡畫不出來）。 */
function lchToRgb({ l, c, h }: Lch): [number, number, number] {
  const rad = (h * Math.PI) / 180
  const A = c * Math.cos(rad)
  const B = c * Math.sin(rad)
  const l_ = l + 0.3963377774 * A + 0.2158037573 * B
  const m_ = l - 0.1055613458 * A - 0.0638541728 * B
  const s_ = l - 0.0894841775 * A - 1.2914855480 * B
  const L3 = l_ * l_ * l_
  const M3 = m_ * m_ * m_
  const S3 = s_ * s_ * s_
  return [
    linearToSrgb(4.0767416621 * L3 - 3.3077115913 * M3 + 0.2309699292 * S3),
    linearToSrgb(-1.2684380046 * L3 + 2.6097574011 * M3 - 0.3413193965 * S3),
    linearToSrgb(-0.0041960863 * L3 - 0.7034186147 * M3 + 1.7076147010 * S3),
  ]
}

function inGamut(rgb: [number, number, number]): boolean {
  return rgb.every((v) => v >= -0.0005 && v <= 1.0005)
}

/**
 * 轉回 hex。明度調高之後原來的彩度常常超出 sRGB（例如亮度拉高的純藍），
 * 直接截斷通道會偏色，所以保持明度和色相、二分收縮彩度直到落進色域。
 */
function lchToHex(lch: Lch): string {
  let rgb = lchToRgb(lch)
  if (!inGamut(rgb)) {
    let lo = 0
    let hi = lch.c
    for (let i = 0; i < 20; i++) {
      const mid = (lo + hi) / 2
      if (inGamut(lchToRgb({ ...lch, c: mid }))) lo = mid
      else hi = mid
    }
    rgb = lchToRgb({ ...lch, c: lo })
  }
  return `#${rgb.map((v) => Math.round(Math.min(1, Math.max(0, v)) * 255).toString(16).padStart(2, '0')).join('')}`
}

function withVisibleLightness(lch: Lch): Lch {
  return lch.l >= MIN_VISIBLE_L ? lch : { ...lch, l: MIN_VISIBLE_L }
}

/** 這個顏色放到黑底膠囊上會不會被調亮（設定頁據此提示使用者）。 */
export function needsBrightening(hex: string): boolean {
  const parsed = parseHexColor(hex)
  return parsed ? hexToLch(parsed).l < MIN_VISIBLE_L : false
}

/** 黑底上實際會畫出來的顏色：夠亮的原樣返回，太暗的提到亮度下限。 */
export function visibleOnBlack(hex: string): string {
  const parsed = parseHexColor(hex) ?? DEFAULT_CUSTOM_WAVE.from
  if (!needsBrightening(parsed)) return parsed
  return lchToHex(withVisibleLightness(hexToLch(parsed)))
}

/** 接近灰色的顏色色相沒有意義（atan2 的結果是噪聲），插值時借另一端的色相，避免中段亂跳。 */
const ACHROMATIC_C = 0.02

function mixLch(a: Lch, b: Lch, t: number): Lch {
  let ha = a.h
  let hb = b.h
  if (a.c < ACHROMATIC_C) ha = hb
  if (b.c < ACHROMATIC_C) hb = ha
  // 走較短的那段色環：紫→粉應該經過品紅，不該繞一大圈穿過綠色
  let dh = hb - ha
  if (dh > 180) dh -= 360
  if (dh < -180) dh += 360
  const h = ha + dh * t
  return {
    l: a.l + (b.l - a.l) * t,
    c: a.c + (b.c - a.c) * t,
    h: h < 0 ? h + 360 : h % 360,
  }
}

/** 自定義配色在位置 t（0=最左，1=最右）的顏色，已保證黑底可見。 */
function customColorAt(custom: CustomWaveColors, t: number): string {
  const from = hexToLch(parseHexColor(custom.from) ?? DEFAULT_CUSTOM_WAVE.from)
  if (custom.mode === 'solid') return lchToHex(withVisibleLightness(from))
  const to = hexToLch(parseHexColor(custom.to) ?? DEFAULT_CUSTOM_WAVE.to)
  return lchToHex(withVisibleLightness(mixLch(from, to, t)))
}

// ── 對外的取色函式 ────────────────────────────────────────────────

/**
 * 整條波形的顏色，按條從左到右。
 *
 * 一次算完整組而不是每根條單獨算：懸浮窗錄音時 30 幀/秒重渲染，每幀二十幾根條，
 * 自定義色要走 OKLCH 換算和色域收縮；呼叫方配合 useMemo 只在配色或條數變化時重算。
 */
export function buildWavePalette(total: number, theme: OverlayWaveTheme, custom: CustomWaveColors): string[] {
  const count = Math.max(0, Math.floor(total))
  const denom = Math.max(1, count - 1)
  return Array.from({ length: count }, (_, index) => {
    const t = index / denom
    if (theme === 'black-white') return '#f1f5f9'
    if (theme === 'black-rainbow') {
      const hue = 140 - Math.round(t * 110)
      const lightness = 64 - Math.round(Math.abs(t - 0.5) * 12)
      return `hsl(${hue} 95% ${lightness}%)`
    }
    if (theme === 'custom') return customColorAt(custom, t)
    const hue = 190 + Math.round(t * 30)
    const lightness = 62 - Math.round(Math.abs(t - 0.5) * 14)
    return `hsl(${hue} 90% ${lightness}%)`
  })
}

/**
 * 計時數字的顏色。自定義配色不讓使用者單獨挑：從主色派生一個更淺、更淡的版本，
 * 和內建主題的做法一致（藍色主題的計時是淺天藍 #bae6fd），選項少一點也不容易配醜。
 */
export function getWaveTimerColor(theme: OverlayWaveTheme, custom: CustomWaveColors): string {
  if (theme === 'black-white') return '#e5e7eb'
  if (theme === 'black-rainbow') return '#fef08a'
  if (theme === 'custom') {
    const base = hexToLch(customColorAt(custom, 0.5))
    return lchToHex({ l: Math.max(base.l, 0.9), c: base.c * 0.45, h: base.h })
  }
  return '#bae6fd'
}

/** 「識別中」動畫的顏色：取主色中段，明度略高於波形，保證在黑底上醒目。 */
export function getWaveThinkingColor(theme: OverlayWaveTheme, custom: CustomWaveColors): string {
  if (theme === 'black-white') return '#e2e8f0'
  if (theme === 'black-rainbow') return '#facc15'
  if (theme === 'custom') {
    const base = hexToLch(customColorAt(custom, 0.5))
    return lchToHex({ ...base, l: Math.max(base.l, 0.72) })
  }
  return '#38bdf8'
}
