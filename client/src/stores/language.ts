/**
 * 介面語言狀態管理
 * 負責語言偏好的持久化（存到 settings）和初始化，形狀對齊 stores/theme.ts。
 *
 * ⚠️ 這裡管的是**介面文字**。識別語種是另外三個東西（localAsr.language、
 * server.language、Preset 的輸出語種），改語言不動它們，也不要在這裡聯動。
 */
import * as bridge from '@/services/bridge'
import { getSetting, setSetting } from '@/services/store'
import {
  getLocale,
  LEGACY_ZH_LOCALE,
  normalizePreference,
  resolveLocale,
  setLocale,
  type LanguagePreference,
  type Locale,
} from '@/i18n'

const LANGUAGE_SETTING_KEY = 'ui.language'

/**
 * 系統顯示語言。優先問 Rust —— 托盤選單用的是同一個判定，
 * 走同一個來源才不會出現「托盤中文、介面英文」。
 *
 * 兜底才用 `navigator.language`：命令不可用（如純瀏覽器裡跑 vitest）時
 * 總比硬編碼一個語言好。
 */
async function detectSystemLocale(): Promise<Locale> {
  try {
    const tag = await bridge.getSystemUiLanguage()
    return resolveLocale(tag)
  } catch {
    return resolveLocale(typeof navigator === 'undefined' ? '' : navigator.language)
  }
}

/** 把偏好解析成真正要用的語言。 */
async function resolvePreference(preference: LanguagePreference): Promise<Locale> {
  return preference === 'auto' ? detectSystemLocale() : preference
}

/**
 * 初始化介面語言：讀偏好 → 解析 → 應用。
 *
 * 必須在首幀渲染之前 await（見 main.tsx），否則會先閃一幀預設語言再跳。
 */
export async function initLanguage(): Promise<Locale> {
  const stored = await getSetting<unknown>(LANGUAGE_SETTING_KEY, 'auto')
  const preference = normalizePreference(stored)
  // [自訂] SC-65：舊版存的簡體中文（zh-CN）改寫成正體中文（台灣），系統匣等 Rust 端讀到的也一致。
  if (stored === LEGACY_ZH_LOCALE) await setSetting(LANGUAGE_SETTING_KEY, preference).catch(() => {})
  const locale = await resolvePreference(preference)
  setLocale(locale)
  return locale
}

/**
 * 只在鍵從未寫入時落一次地區相關預設值；已有設定（包括空字串）絕不覆蓋。
 * 這一步必須在 initLanguage 之後呼叫，不能用尚未解析的 `auto` 偏好來猜地區。
 */
export async function initLocaleDefaults(locale: Locale): Promise<void> {
  const defaults: Record<string, string> = {
    // [自訂] S25：模型一律從官方 huggingface.co 下載，不再依介面語言改用 hf-mirror.com。
    // 原：'localAsr.downloadSource': locale === 'en' ? 'HuggingFace' : 'HuggingFace Mirror',
    'localAsr.downloadSource': 'HuggingFace',
    // [自訂] S34：第一次啟動不寫入 deepseek／openrouter；AI 只允許本機 Ollama。
    // 原：'cloudAi.provider': locale === 'en' ? 'openrouter' : 'deepseek',
    'cloudAi.provider': 'ollama',
    'ai.builtinPromptLanguage': locale === 'en' ? 'en' : 'zh-TW',
  }
  await Promise.all(Object.entries(defaults).map(async ([key, value]) => {
    const existing = await bridge.storeGet(key)
    if (existing === null || existing === undefined) await bridge.storeSet(key, value)
  }))
}

/** 切換語言並持久化。傳 'auto' 表示重新跟隨系統。 */
export async function switchLanguage(preference: LanguagePreference): Promise<Locale> {
  const normalized = normalizePreference(preference)
  const locale = await resolvePreference(normalized)
  setLocale(locale)
  await setSetting(LANGUAGE_SETTING_KEY, normalized)
  return locale
}

/** 讀回持久化的偏好（可能是 'auto'）；設定頁要用它顯示當前選中項。 */
export async function getLanguagePreference(): Promise<LanguagePreference> {
  return normalizePreference(await getSetting(LANGUAGE_SETTING_KEY, 'auto'))
}

/** 當前實際生效的語言（永遠是具體值，不會是 'auto'）。 */
export function getActiveLanguage(): Locale {
  return getLocale()
}
