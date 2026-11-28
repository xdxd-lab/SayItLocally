/** 主題系統型別定義 */

/** 全域性語義 CSS 變數 — 所有主題必須提供 */
export interface ThemeVars {
  // 基礎色
  '--background': string
  '--foreground': string
  '--card': string
  '--card-foreground': string
  '--primary': string
  '--primary-foreground': string
  '--secondary': string
  '--secondary-foreground': string
  '--muted': string
  '--muted-foreground': string
  '--accent': string
  '--accent-foreground': string
  '--destructive': string
  '--destructive-foreground': string
  '--border': string
  '--input': string
  '--ring': string
  '--radius': string

  // 區域色
  '--sidebar-bg': string
  '--sidebar-border': string
  '--sidebar-item-active-bg': string
  '--sidebar-item-hover-bg': string
  '--sidebar-text': string
  '--sidebar-text-active': string
  '--titlebar-bg': string
  '--titlebar-text': string
  '--titlebar-close-hover-bg': string
  '--titlebar-close-hover-text': string

  // 表單控制元件
  '--input-bg': string
  '--input-border': string
  '--input-focus-border': string
  '--input-focus-ring': string
  '--input-placeholder': string

  // 狀態色
  //
  // 每種狀態有三個變數，用途不同，別混用：
  //   --xxx              色塊本體（圓點、進度條填充、10% 淡底）
  //   --xxx-foreground   壓在色塊本體上的文字
  //   --xxx-strong       直接畫在頁面/卡片底色上的**文字**顏色
  //
  // 為什麼要有 -strong：--success/--warning/--info 這類中等明度的彩色在淺色主題的
  // 白底上對比度只有 2~3.3:1，遠低於正文 4.5:1 的要求（`text-warning` 那句
  // 「模型尚未下載」曾經是全卡最不可讀的一行）。-strong 是同色相壓暗/提亮到達標
  // 的版本，只用於文字與描邊。
  '--success': string
  '--success-foreground': string
  '--success-strong': string
  '--warning': string
  '--warning-foreground': string
  '--warning-strong': string
  '--info': string
  '--info-foreground': string
  '--info-strong': string
  '--destructive-strong': string
}

/** 主題擴充套件變數 — 主題獨有的特色變數（可選） */
export type ThemeExtras = Record<string, string>

/** 主題字型配置（可選） */
export interface ThemeFonts {
  /** body 正文字型 */
  body?: string
  /** 等寬字型 */
  mono?: string
}

/** 主題定義 */
export interface ThemeDefinition {
  /** 唯一標識 */
  id: string
  /** 顯示名稱 */
  name: string
  /** 是否為暗色主題（控制 Tailwind dark: 字首） */
  isDark: boolean
  /** 預覽色（用於設定頁面的主題選擇器） */
  previewColors: {
    bg: string
    sidebar: string
    primary: string
    accent: string
  }
  /** 全域性 CSS 變數 */
  vars: ThemeVars
  /** 主題獨有的擴充套件變數 */
  extras?: ThemeExtras
  /** 主題字型（不設定則使用全域性預設字型） */
  fonts?: ThemeFonts
}

/** 主題 ID 型別 */
export type ThemeId = 'light' | 'dark' | 'claude'
