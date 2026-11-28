import type { ActiveAppContext } from '@/types/appContext'
import { BUILTIN_PRESETS } from '@/services/store'
import type { AppPromptRule, PromptResolution, PromptRoutingInput } from './types'
import { buildDynamicIdentityPrompt, summarizeDomainScenes } from './userStats'

function normalizeText(value?: string) {
  return String(value || '').trim().toLowerCase()
}

function normalizeProcessName(context: ActiveAppContext | null) {
  const processName = normalizeText(context?.processName)
  if (processName) return processName

  const exePath = String(context?.exePath || '')
  const segments = exePath.split(/[\\/]/)
  return normalizeText(segments[segments.length - 1])
}

function includesAny(value: string, patterns?: string[]) {
  if (!value || !patterns?.length) return false
  return patterns.some((pattern) => value.includes(normalizeText(pattern)))
}

/**
 * 判斷一條應用規則是否命中當前視窗。
 *
 * 程序名是硬證據，視窗標題只是軟線索 —— 所以**只要規則寫了程序名、且這次確實拿到了
 * 程序名，就只按程序名判定**，標題不再獨立觸發。
 *
 * 原來兩者是「或」的關係，而標題是「包含即命中」，一個標題能同時命中多條規則：
 * 在 Outlook 裡寫一封標題為「Teams 會議紀要 - Outlook」的郵件時，Teams 規則也會命中，
 * 又因為 Teams 優先順序更高，最終按"即時聊天短訊息"來整理這封正式郵件。
 *
 * 保留標題/類名/控制元件 ID 的能力，是給"程序名區分不了"的場景兜底：
 *   · 規則本身沒寫程序名（如網頁版應用，程序都是瀏覽器，只能靠標題區分）；
 *   · 少數視窗拿不到程序名（此時不能因為程序名為空就直接判不命中）。
 */
export function matchesAppPromptRule(rule: AppPromptRule, context: ActiveAppContext | null) {
  if (!context) return false

  const processName = normalizeProcessName(context)
  // macOS 的執行檔名不可靠（VS Code 叫 Electron、很多程式同名），規則裡的
  // 「程序名」同時拿來比 Bundle ID，內建規則因此可以把 Windows 程序名和 mac Bundle ID 寫在一起。
  const bundleId = normalizeText(context.bundleId)

  if (rule.matcher.processNames.length > 0 && (processName || bundleId)) {
    return rule.matcher.processNames.some((candidate) => {
      const normalized = normalizeText(candidate)
      return normalized === processName || (bundleId !== '' && normalized === bundleId)
    })
  }

  const windowTitle = normalizeText(context.windowTitle)
  const windowClass = normalizeText(context.windowClass)
  const automationId = normalizeText(context.automationId)
  if (includesAny(windowTitle, rule.matcher.windowTitleIncludes)) return true
  if (includesAny(windowClass, rule.matcher.windowClasses)) return true
  if (includesAny(automationId, rule.matcher.automationIds)) return true
  return false
}

function pickPreset(presetId: string | undefined, presets: PromptRoutingInput['presets'], activePresetId: string) {
  const fallback = presets.find((preset) => preset.id === activePresetId)
    || BUILTIN_PRESETS.find((preset) => preset.id === activePresetId)
    || presets[0]
    || BUILTIN_PRESETS[0]

  if (!presetId) return fallback
  return presets.find((preset) => preset.id === presetId)
    || BUILTIN_PRESETS.find((preset) => preset.id === presetId)
    || fallback
}

function summarizeResolution(parts: string[]) {
  return parts.filter(Boolean).join(' | ')
}

/** 構造"熱詞注入 AI 提示詞"的段落；無熱詞時返回 null。供即時與重新識別兩條路徑共用。 */
export function buildHotwordInjectionPart(hotwords: string[] | undefined): string | null {
  if (!hotwords || hotwords.length === 0) return null
  const terms = Array.from(new Set(hotwords.map((w) => w.trim()).filter(Boolean)))
  if (terms.length === 0) return null
  return `使用者術語表：以下是使用者常用的專有名詞/術語，整理時若遇到讀音相近或明顯誤識的詞，請優先糾正為下列正確寫法（保持其大小寫），並原樣保留：\n${terms.join('、')}` // i18n-allow: 中文口述整理 Prompt
}

export function resolvePromptRouting(input: PromptRoutingInput): PromptResolution {
  // 順序即優先順序：取列表裡第一條「已啟用且命中」的規則，也就是介面上自上而下的第一條。
  // 順序完全由使用者在介面上排（啟用一條會自動置頂），這裡不要再重排。
  const matchedRule = input.appRules
    .find((rule) => rule.enabled && matchesAppPromptRule(rule, input.appContext))

  const preset = pickPreset(matchedRule?.presetId, input.presets, input.activePresetId)
  const systemPromptParts = [preset.systemPrompt.trim()]
  const dominantScene = summarizeDomainScenes(input.userStats, 1)[0]

  if (matchedRule?.promptAppend) {
    systemPromptParts.push(`應用場景補充：\n${matchedRule.promptAppend.trim()}`) // i18n-allow: 中文口述整理 Prompt
  }

  const dynamicIdentityPrompt = buildDynamicIdentityPrompt(input.userStats)
  if (dynamicIdentityPrompt) {
    systemPromptParts.push(`使用者畫像補充：\n${dynamicIdentityPrompt}`) // i18n-allow: 中文口述整理 Prompt
  }

  // 可選：把熱詞表注入系統提示詞，幫助 AI 在整理時糾正/保留專有名詞（預設關閉）。
  let hotwordInjected = false
  if (input.injectHotwords) {
    const part = buildHotwordInjectionPart(input.hotwords)
    if (part) {
      systemPromptParts.push(part)
      hotwordInjected = true
    }
  }

  const summaryParts = [
    `Base preset id: ${preset.id}`,
    matchedRule ? `App rule id: ${matchedRule.id}` : 'App rule: no match',
    dynamicIdentityPrompt && dominantScene ? `User profile: ${dominantScene.label}` : '',
    hotwordInjected ? 'Hotwords injected: yes' : '',
  ]

  return {
    appId: matchedRule?.appId,
    appName: matchedRule?.name,
    preset,
    matchedRule,
    systemPrompt: systemPromptParts.join('\n\n'),
    summary: summarizeResolution(summaryParts),
  }
}
