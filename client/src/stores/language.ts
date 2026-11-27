/**
 * 界面语言状态管理
 * 负责语言偏好的持久化（存到 settings）和初始化，形状对齐 stores/theme.ts。
 *
 * ⚠️ 这里管的是**界面文字**。识别语种是另外三个东西（localAsr.language、
 * server.language、Preset 的输出语种），改语言不动它们，也不要在这里联动。
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
 * 系统显示语言。优先问 Rust —— 托盘菜单用的是同一个判定，
 * 走同一个来源才不会出现「托盘中文、界面英文」。
 *
 * 兜底才用 `navigator.language`：命令不可用（如纯浏览器里跑 vitest）时
 * 总比硬编码一个语言好。
 */
async function detectSystemLocale(): Promise<Locale> {
  try {
    const tag = await bridge.getSystemUiLanguage()
    return resolveLocale(tag)
  } catch {
    return resolveLocale(typeof navigator === 'undefined' ? '' : navigator.language)
  }
}

/** 把偏好解析成真正要用的语言。 */
async function resolvePreference(preference: LanguagePreference): Promise<Locale> {
  return preference === 'auto' ? detectSystemLocale() : preference
}

/**
 * 初始化界面语言：读偏好 → 解析 → 应用。
 *
 * 必须在首帧渲染之前 await（见 main.tsx），否则会先闪一帧默认语言再跳。
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
 * 只在键从未写入时落一次地区相关默认值；已有设置（包括空字符串）绝不覆盖。
 * 这一步必须在 initLanguage 之后调用，不能用尚未解析的 `auto` 偏好来猜地区。
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

/** 切换语言并持久化。传 'auto' 表示重新跟随系统。 */
export async function switchLanguage(preference: LanguagePreference): Promise<Locale> {
  const normalized = normalizePreference(preference)
  const locale = await resolvePreference(normalized)
  setLocale(locale)
  await setSetting(LANGUAGE_SETTING_KEY, normalized)
  return locale
}

/** 读回持久化的偏好（可能是 'auto'）；设置页要用它显示当前选中项。 */
export async function getLanguagePreference(): Promise<LanguagePreference> {
  return normalizePreference(await getSetting(LANGUAGE_SETTING_KEY, 'auto'))
}

/** 当前实际生效的语言（永远是具体值，不会是 'auto'）。 */
export function getActiveLanguage(): Locale {
  return getLocale()
}
