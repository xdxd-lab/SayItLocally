/**
 * 極簡 i18n 執行時。
 *
 * 為什麼不引 i18next：這裡需要的只有「查表 + 插值」，而專案的既有做法是
 * 「純邏輯抽成檔案 + vitest 鎖不變數」。自建的最大好處是 **key 型別安全**——
 * 漏 key、拼錯 key 在 tsc 階段就報錯，而不是執行時靜默 fallback 成 key 本身。
 * locale 檔案保持平鋪 JSON，將來要遷 i18next 或接 Crowdin 都沒有摩擦。
 *
 * ⚠️ 語言只管介面文字，與「識別語種」是兩件事：
 * `localAsr.language` / `server.language` 是送給 ASR 的語種，Preset 決定輸出語種，
 * 三者都不受這裡影響。改這個檔案時別順手把它們串在一起。
 */
import en from './locales/en.json'
import zhTW from './locales/zh-TW.json'

// [自訂] SC-65：介面只提供正體中文（台灣）與英文；簡體中文語系已移除。
export const LOCALES = ['zh-TW', 'en'] as const
export type Locale = (typeof LOCALES)[number]

/** 語言偏好：'auto' = 跟隨系統，解析後一定落在某個具體 Locale 上。 */
export type LanguagePreference = 'auto' | Locale

/** 舊版存下的簡體中文設定值；讀到時一律視為正體中文（台灣）。 */
export const LEGACY_ZH_LOCALE = 'zh-CN'

/** key 從中文表推導——中文表是**源**，新增文案必須先進 zh-TW.json。 */
export type TranslationKey = keyof typeof zhTW

/**
 * en 表明確標註成「與源表 key 完全一致」，少一個 key 就編譯不過。
 * 多出來的 key 由 `__tests__/locales.test.ts` 擋住（型別層面查不出多餘的鍵）。
 */
const TABLES: Record<Locale, Record<TranslationKey, string>> = {
  'zh-TW': zhTW,
  en,
}

const DEFAULT_LOCALE: Locale = 'zh-TW'

let currentLocale: Locale = DEFAULT_LOCALE
const listeners = new Set<() => void>()

export function isLocale(value: unknown): value is Locale {
  return typeof value === 'string' && (LOCALES as readonly string[]).includes(value)
}

/**
 * 把系統／瀏覽器給的 locale 字串解析成支援的介面語言。
 *
 * [自訂] SC-65：任何中文（zh、zh-CN、zh-TW、zh-HK、zh-Hans、zh-Hant…）都落到 zh-TW，
 * 其餘一律英文。規則與 Rust 端 `locale::lang_from_language_tag` 相同。
 */
export function resolveLocale(raw: string | null | undefined): Locale {
  const tag = (raw || '').trim().toLowerCase().replace(/_/g, '-')
  return tag.split('-')[0] === 'zh' ? 'zh-TW' : 'en'
}

/** 正規化存下的偏好值：舊的 zh-CN 遷移成 zh-TW；髒資料一律回落 'auto'。 */
export function normalizePreference(value: unknown): LanguagePreference {
  if (value === 'auto') return 'auto'
  if (value === LEGACY_ZH_LOCALE) return 'zh-TW'
  return isLocale(value) ? value : 'auto'
}

export function getLocale(): Locale {
  return currentLocale
}

/**
 * 切換當前介面語言。
 *
 * 同步生效、同步通知，`useT()` 依賴這一點：切語言後當幀就能重渲染，不需要重啟。
 * 只寫記憶體，不落庫 —— 持久化是 `stores/language.ts` 的職責。
 */
export function setLocale(locale: Locale): void {
  if (!isLocale(locale)) return
  // <html lang> 無條件同步，即使語言沒變：初始語言等於預設值時這裡是唯一的寫入
  // 時機（沒有"變化"可言），漏掉就會出現 lang 和實際語言不一致 —— 字型棧跟著
  // lang 走，不一致時沒有任何報錯，只是英文介面被中文字型渲染。
  applyDocumentLocale(locale)
  if (locale === currentLocale) return
  currentLocale = locale
  listeners.forEach((listener) => listener())
}

/**
 * 把語言寫到 <html lang>，字型棧由 index.css 按它切換。
 *
 * 放 CSS 而不是在 JS 裡算字型：英文介面下英文字元本該用 Segoe UI 渲染，
 * 中文字型（微軟雅黑）渲染英文明顯更糊。見 index.css 的 html[lang] 規則。
 */
function applyDocumentLocale(locale: Locale): void {
  if (typeof document === 'undefined') return
  document.documentElement.lang = locale
}

/** 供 useSyncExternalStore 使用。 */
export function subscribeLocale(listener: () => void): () => void {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

/**
 * 取一條文案。`params` 裡的鍵按 `{name}` 佔位替換。
 *
 * 缺 key 時依次回落：當前語言 → 中文源表 → key 本身。回落到 key 本身意味著
 * 介面上會出現 `nav.home` 這種東西，刺眼是故意的 —— 它比顯示空字串好排查。
 */
export function t(key: TranslationKey, params?: Record<string, string | number>): string {
  const table = TABLES[currentLocale]
  const template = table[key] ?? TABLES[DEFAULT_LOCALE][key] ?? key
  if (!params) return template
  return template.replace(/\{(\w+)\}/g, (match, name: string) => {
    const value = params[name]
    return value === undefined ? match : String(value)
  })
}
