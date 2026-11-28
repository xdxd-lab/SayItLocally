import type { ActiveAppContext } from '@/types/appContext'
import type { PromptPreset } from '@/services/store'

export interface AppPromptMatcher {
  processNames: string[]
  windowTitleIncludes?: string[]
  windowClasses?: string[]
  automationIds?: string[]
}

/**
 * 一條應用規則。
 *
 * **順序即優先順序**：陣列下標越小越先命中（見 promptRouter.resolvePromptRouting），
 * 介面上就是自上而下。以前這裡有個 `priority` 數字，介面按它排序 —— 使用者看得見順序、
 * 卻改不了順序；現在順序由使用者拖動決定，就不能再留一個會跟順序打架的欄位。
 * 舊存檔裡的 `priority` 讀取時忽略（當時存下來的陣列本身已經是按它排好序的）。
 */
export interface AppPromptRule {
  id: string
  appId: string
  name: string
  enabled: boolean
  builtin?: boolean
  presetId?: string
  promptAppend: string
  matcher: AppPromptMatcher
}

export interface UserStats {
  totalWords: number
  totalSessions: number
  domainWords: Record<string, number>
  appUsageCount: Record<string, number>
  firstUsedAt?: number
  lastUsedAt?: number
}

export interface PromptResolution {
  appId?: string
  appName?: string
  preset: PromptPreset
  matchedRule?: AppPromptRule
  systemPrompt: string
  summary: string
}

export interface PromptRoutingInput {
  appContext: ActiveAppContext | null
  presets: PromptPreset[]
  activePresetId: string
  appRules: AppPromptRule[]
  userStats: UserStats
  /** 生效的熱詞列表（用於可選的"注入 AI 提示詞"功能） */
  hotwords?: string[]
  /** 是否把熱詞注入到 AI 系統提示詞，幫助 AI 糾正專有名詞。預設關閉。 */
  injectHotwords?: boolean
}
