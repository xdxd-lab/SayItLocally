/**
 * 极简 i18n 运行时。
 *
 * 为什么不引 i18next：这里需要的只有「查表 + 插值」，而项目的既有做法是
 * 「纯逻辑抽成文件 + vitest 锁不变量」。自建的最大好处是 **key 类型安全**——
 * 漏 key、拼错 key 在 tsc 阶段就报错，而不是运行时静默 fallback 成 key 本身。
 * locale 文件保持平铺 JSON，将来要迁 i18next 或接 Crowdin 都没有摩擦。
 *
 * ⚠️ 语言只管界面文字，与「识别语种」是两件事：
 * `localAsr.language` / `server.language` 是送给 ASR 的语种，Preset 决定输出语种，
 * 三者都不受这里影响。改这个文件时别顺手把它们串在一起。
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
 * 切换当前界面语言。
 *
 * 同步生效、同步通知，`useT()` 依赖这一点：切语言后当帧就能重渲染，不需要重启。
 * 只写内存，不落库 —— 持久化是 `stores/language.ts` 的职责。
 */
export function setLocale(locale: Locale): void {
  if (!isLocale(locale)) return
  // <html lang> 无条件同步，即使语言没变：初始语言等于默认值时这里是唯一的写入
  // 时机（没有"变化"可言），漏掉就会出现 lang 和实际语言不一致 —— 字体栈跟着
  // lang 走，不一致时没有任何报错，只是英文界面被中文字体渲染。
  applyDocumentLocale(locale)
  if (locale === currentLocale) return
  currentLocale = locale
  listeners.forEach((listener) => listener())
}

/**
 * 把语言写到 <html lang>，字体栈由 index.css 按它切换。
 *
 * 放 CSS 而不是在 JS 里算字体：英文界面下英文字符本该用 Segoe UI 渲染，
 * 中文字体（微软雅黑）渲染英文明显更糊。见 index.css 的 html[lang] 规则。
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
 * 取一条文案。`params` 里的键按 `{name}` 占位替换。
 *
 * 缺 key 时依次回落：当前语言 → 中文源表 → key 本身。回落到 key 本身意味着
 * 界面上会出现 `nav.home` 这种东西，刺眼是故意的 —— 它比显示空字符串好排查。
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
