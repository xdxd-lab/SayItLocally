/**
 * 主題狀態管理
 * 負責主題的持久化（存到 settings）和初始化
 */
import { applyTheme, getCurrentThemeId } from '@/themes'
import { getSetting, setSetting } from '@/services/store'

const THEME_SETTING_KEY = 'theme'
const DEFAULT_THEME = 'teal-dark'

/** 初始化主題：從 settings 讀取並應用 */
export async function initTheme(): Promise<string> {
  const saved = await getSetting(THEME_SETTING_KEY, DEFAULT_THEME)
  const themeId = typeof saved === 'string' ? saved : DEFAULT_THEME
  return applyTheme(themeId)
}

/** 切換主題並持久化 */
export async function switchTheme(id: string): Promise<string> {
  const applied = applyTheme(id)
  await setSetting(THEME_SETTING_KEY, applied)
  return applied
}

/** 獲取當前主題 ID */
export function getActiveThemeId(): string {
  return getCurrentThemeId()
}
