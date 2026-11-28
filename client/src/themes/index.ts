/**
 * 主題登錄檔 + 切換邏輯
 *
 * 用法：
 *   import { applyTheme, getTheme, themeList } from '@/themes'
 *   applyTheme('dark')
 */
import type { ThemeDefinition, ThemeId } from './types'
import light from './light'
import dark from './dark'
import claude from './claude'
import { getCurrentWindow, type Theme } from '@tauri-apps/api/window'
import { IS_MAC } from '@/lib/platform'

/** 所有已註冊主題（新增主題只需在此新增一行） */
const themes: Record<string, ThemeDefinition> = {
  light,
  dark,
  claude,
}

/** 主題列表（用於 UI 渲染） */
export const themeList: ThemeDefinition[] = Object.values(themes)

/** 獲取主題定義，找不到則回退 light */
export function getTheme(id: string): ThemeDefinition {
  return themes[id] || themes.light
}

/** 當前已應用的主題 ID */
let currentThemeId: string = 'light'

/** 獲取當前主題 ID */
export function getCurrentThemeId(): string {
  return currentThemeId
}

/** 預設字型（與 index.css body 一致） */
// mac：系統字型和蘋方優先（否則裝了 Office 的 Mac 會先命中微軟雅黑）；Windows 保持原樣。
const DEFAULT_FONT_BODY = IS_MAC
  ? '-apple-system, BlinkMacSystemFont, "PingFang SC", "Microsoft YaHei", "Segoe UI", Roboto, sans-serif'
  : '"Microsoft YaHei", "Segoe UI", -apple-system, BlinkMacSystemFont, Roboto, sans-serif'

/**
 * macOS：讓系統畫的部分和主題的深淺一致（只是渲染，不是新選項）。
 * - color-scheme：網頁裡的原生控制元件（系統捲軸、下拉框）按它取深淺色；
 * - 視窗外觀：彈出的原生選單（下拉框的選項、右鍵選單）按它取深淺色。
 * 不同步的話，深色主題裡捲軸是深色壓在深色上、彈出的是淺色選單。
 */
function syncMacAppearance(isDark: boolean): void {
  document.documentElement.style.colorScheme = isDark ? 'dark' : 'light'
  void setMainWindowAppearance(isDark ? 'dark' : 'light')
}

async function setMainWindowAppearance(theme: Theme): Promise<void> {
  try {
    await getCurrentWindow().setTheme(theme)
  } catch {
    // 設定失敗隻影響原生選單的深淺色，不影響頁面本身
  }
}

/**
 * 外觀同步只做在主視窗裡。更新通知是透明視窗：給它設 color-scheme 可能讓 WebKit 把透明底
 * 畫成深色底；它也沒有原生選單，沒有 setTheme 許可權。
 */
function isMainWindow(): boolean {
  try {
    return getCurrentWindow().label === 'main'
  } catch {
    // 不在 Tauri 裡（單測）
    return false
  }
}

/**
 * 應用主題：將 CSS 變數注入 :root，切換 dark class，設定字型
 * @returns 實際應用的主題 ID
 */
export function applyTheme(id: string): string {
  const theme = getTheme(id)
  const root = document.documentElement

  // 注入全局变量
  const allVars = { ...theme.vars, ...(theme.extras || {}) }
  for (const [key, value] of Object.entries(allVars)) {
    root.style.setProperty(key, value)
  }

  // 切換字型
  document.body.style.fontFamily = theme.fonts?.body || DEFAULT_FONT_BODY

  // 切換 dark class（Tailwind dark: 字首）
  if (theme.isDark) {
    root.classList.add('dark')
  } else {
    root.classList.remove('dark')
  }

  // 切換主題標識 class（方便主題特定 CSS）
  for (const t of themeList) {
    root.classList.remove(`theme-${t.id}`)
  }
  root.classList.add(`theme-${theme.id}`)

  if (IS_MAC && isMainWindow()) syncMacAppearance(theme.isDark)

  currentThemeId = theme.id
  return theme.id
}

export type { ThemeDefinition, ThemeId }
