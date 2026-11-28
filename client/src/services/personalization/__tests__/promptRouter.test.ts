import { describe, it, expect } from 'vitest'
import { matchesAppPromptRule } from '../promptRouter'
import { BUILTIN_APP_RULES } from '../defaults'
import type { AppPromptRule } from '../types'
import type { ActiveAppContext } from '@/types/appContext'

function ruleById(id: string): AppPromptRule {
  const rule = BUILTIN_APP_RULES.find((r) => r.id === id)
  if (!rule) throw new Error(`內建規則不存在: ${id}`)
  return rule
}

describe('matchesAppPromptRule', () => {
  it('程序名匹配時命中', () => {
    const ctx: ActiveAppContext = { processName: 'outlook.exe', windowTitle: '收件箱 - Outlook' }
    expect(matchesAppPromptRule(ruleById('outlook'), ctx)).toBe(true)
  })

  it('拿到程序名時，標題不再獨立觸發：在 Outlook 裡寫含 Teams 的郵件不應命中 Teams', () => {
    // 這是改動前的真實誤判：標題「包含」teams 就命中 Teams 規則，
    // 且 Teams 優先順序(100) > Outlook(95)，正式郵件會被按即時聊天風格整理。
    const ctx: ActiveAppContext = { processName: 'outlook.exe', windowTitle: 'Teams 會議紀要 - Outlook' }
    expect(matchesAppPromptRule(ruleById('teams'), ctx)).toBe(false)
    expect(matchesAppPromptRule(ruleById('outlook'), ctx)).toBe(true)
  })

  it('短關鍵詞不再誤傷：在 VSCode 裡編輯 qq_faq.md 不應命中 QQ', () => {
    const ctx: ActiveAppContext = { processName: 'code.exe', windowTitle: 'qq_faq.md - proj - Visual Studio Code' }
    expect(matchesAppPromptRule(ruleById('qq'), ctx)).toBe(false)
    expect(matchesAppPromptRule(ruleById('vscode'), ctx)).toBe(true)
  })

  it('程序名對不上就是不命中（不再靠標題撈回來）', () => {
    const ctx: ActiveAppContext = { processName: 'chrome.exe', windowTitle: 'Outlook - Google Chrome' }
    expect(matchesAppPromptRule(ruleById('outlook'), ctx)).toBe(false)
  })

  it('規則沒寫程序名時（如網頁版應用）回落到標題匹配', () => {
    const webRule: AppPromptRule = {
      ...ruleById('outlook'),
      id: 'outlook-web',
      matcher: { processNames: [], windowTitleIncludes: ['outlook'] },
    }
    const ctx: ActiveAppContext = { processName: 'msedge.exe', windowTitle: '收件箱 - Outlook - Microsoft Edge' }
    expect(matchesAppPromptRule(webRule, ctx)).toBe(true)
  })

  it('拿不到程序名時仍回落到標題匹配（不能因為程序名為空就判不命中）', () => {
    // 內建規則已不帶標題列表，這裡顯式構造一條"兩者都寫"的規則來驗證兜底分支
    const rule: AppPromptRule = {
      ...ruleById('outlook'),
      matcher: { processNames: ['outlook.exe'], windowTitleIncludes: ['outlook'] },
    }
    const ctx: ActiveAppContext = { windowTitle: '收件箱 - Outlook' }
    expect(matchesAppPromptRule(rule, ctx)).toBe(true)
  })

  it('內建規則不再帶視窗標題列表（避免標題匹配把別的應用搶走）', () => {
    for (const rule of BUILTIN_APP_RULES) {
      expect(rule.matcher.processNames.length).toBeGreaterThan(0)
      expect(rule.matcher.windowTitleIncludes ?? []).toHaveLength(0)
    }
  })

  it('程序名從 exePath 兜底解析', () => {
    const ctx: ActiveAppContext = { exePath: 'C:\\Program Files\\Notepad\\notepad.exe' }
    expect(matchesAppPromptRule(ruleById('notepad'), ctx)).toBe(true)
  })

  it('無上下文時不命中', () => {
    expect(matchesAppPromptRule(ruleById('teams'), null)).toBe(false)
  })
})
