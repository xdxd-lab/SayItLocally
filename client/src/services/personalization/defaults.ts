import type { AppPromptRule, UserStats } from './types'
import { IS_MAC } from '@/lib/platform'

// 這裡的**陣列順序就是預設優先順序**（自上而下先命中先生效，見 promptRouter）。
// 使用者可以在介面上拖動調整，調整後的順序存進 appPromptRules；這份清單隻決定
// 「從沒動過」時的初始順序，以及後續版本新增的內建規則追加在哪。
//
// 內建規則一律只靠 processNames 判定（見 promptRouter.matchesAppPromptRule）。
// 這裡**不要**再加 windowTitleIncludes：標題是「包含即命中」，一個標題能同時命中
// 多條規則 —— 曾導致在 Outlook 裡寫標題含「Teams」的郵件被 Teams 規則搶走。
// 標題匹配只保留給「程序名區分不了」的場景（如網頁版應用，程序都是瀏覽器）。

const WINDOWS_BUILTIN_APP_RULES: AppPromptRule[] = [
  {
    id: 'teams',
    appId: 'teams',
    name: 'Teams',
    builtin: true,
    enabled: false,
    presetId: 'intent',
    promptAppend: '适合即时协作聊天。优先输出可以直接发送的短消息，语气自然、清晰、简洁，避免邮件腔。', // i18n-allow: 中文口述整理 Prompt
    matcher: {
      processNames: ['teams.exe', 'ms-teams.exe'],
    },
  },
  {
    id: 'outlook',
    appId: 'outlook',
    name: 'Outlook',
    builtin: true,
    enabled: false,
    presetId: 'intent',
    promptAppend: '适合工作邮件草稿。语气正式、完整，必要时自然分段，但不要编造收件人、称呼或任何事实。', // i18n-allow: 中文口述整理 Prompt
    matcher: {
      processNames: ['outlook.exe', 'olk.exe'],
    },
  },
  {
    id: 'kiro',
    appId: 'kiro',
    name: 'Kiro',
    builtin: true,
    enabled: false,
    presetId: 'faithful',
    promptAppend: '面向开发工具输入。保留代码、命令、文件名、路径、英文标识符和 Markdown 结构，不要把技术词改写成普通中文。', // i18n-allow: 中文口述整理 Prompt
    matcher: {
      processNames: ['kiro.exe'],
    },
  },
  {
    id: 'codex',
    appId: 'codex',
    name: 'Codex',
    builtin: true,
    enabled: false,
    presetId: 'faithful',
    promptAppend: '面向 Codex 编码工具，多为用自然语言下达编程指令。保留代码、命令、文件名、路径和英文标识符，把要做的事说清楚，不要把技术词改写成普通中文。', // i18n-allow: 中文口述整理 Prompt
    matcher: {
      processNames: ['codex.exe'],
    },
  },
  {
    id: 'vscode',
    appId: 'vscode',
    name: 'VSCode',
    builtin: true,
    enabled: false,
    presetId: 'faithful',
    promptAppend: '面向 VSCode 编辑区。保留代码、命令、文件名、API、英文术语和 Markdown 结构，不要过度润色技术内容。', // i18n-allow: 中文口述整理 Prompt
    matcher: {
      processNames: ['code.exe'],
    },
  },
  {
    id: 'cursor',
    appId: 'cursor',
    name: 'Cursor',
    builtin: true,
    enabled: false,
    presetId: 'faithful',
    promptAppend: '面向 Cursor 编辑区。保留代码、命令、路径、技术名词和英文标识符，不要擅自解释或补全技术内容。', // i18n-allow: 中文口述整理 Prompt
    matcher: {
      processNames: ['cursor.exe'],
    },
  },
  {
    id: 'notepad',
    appId: 'notepad',
    name: 'Notepad',
    builtin: true,
    enabled: false,
    presetId: 'intent',
    promptAppend: '面向 Windows 记事本，适合随手记录的纯文本。输出纯文本，不要使用 Markdown 标记或特殊格式符号。', // i18n-allow: 中文口述整理 Prompt
    matcher: {
      processNames: ['notepad.exe'],
    },
  },
  {
    id: 'weixin',
    appId: 'weixin',
    name: 'WeChat',
    builtin: true,
    enabled: false,
    presetId: 'casual',
    promptAppend: '适合微信聊天。输出可直接发送的自然口语短消息，简洁亲切，不要用书面或邮件腔。', // i18n-allow: 中文口述整理 Prompt
    matcher: {
      processNames: ['weixin.exe', 'wechat.exe'],
    },
  },
  {
    id: 'qq',
    appId: 'qq',
    name: 'QQ',
    builtin: true,
    enabled: false,
    presetId: 'casual',
    promptAppend: '适合 QQ 聊天。输出轻松自然、可直接发送的短消息，口语化、简洁。', // i18n-allow: 中文口述整理 Prompt
    matcher: {
      processNames: ['qq.exe'],
    },
  },
]

/**
 * macOS 上內建規則的匹配目標：Bundle ID（promptRouter 拿「程序名」同時比 Bundle ID）。
 * mac 的執行檔名不可靠（VS Code 的執行檔叫 Electron），所以這裡一律寫 Bundle ID；
 * Codex 的 mac 版 Bundle ID 沒核實過，先按執行檔名匹配。
 * 兩個平台各用各的列表，設定頁裡不會出現對方平台的標識。
 */
const MAC_RULE_OVERRIDES: Record<string, Partial<AppPromptRule> & { processNames: string[] }> = {
  teams: { processNames: ['com.microsoft.teams2', 'com.microsoft.teams'] },
  outlook: { processNames: ['com.microsoft.Outlook'] },
  kiro: { processNames: ['dev.kiro.desktop'] },
  codex: { processNames: ['codex'] },
  vscode: { processNames: ['com.microsoft.VSCode'] },
  cursor: { processNames: ['com.todesktop.230313mzl4w4u92'] },
  // mac 上沒有記事本，對應系統自帶的「文本編輯」
  notepad: {
    name: 'TextEdit',
    promptAppend: '面向「文本编辑」，适合随手记录的纯文本。输出纯文本，不要使用 Markdown 标记或特殊格式符号。', // i18n-allow: 中文口述整理 Prompt
    processNames: ['com.apple.TextEdit'],
  },
  weixin: { processNames: ['com.tencent.xinWeChat'] },
  qq: { processNames: ['com.tencent.qq'] },
}

function applyMacOverrides(rule: AppPromptRule): AppPromptRule {
  const override = MAC_RULE_OVERRIDES[rule.id]
  if (!override) return rule
  const { processNames, ...fields } = override
  return { ...rule, ...fields, matcher: { ...rule.matcher, processNames } }
}

export const BUILTIN_APP_RULES: AppPromptRule[] = IS_MAC
  ? WINDOWS_BUILTIN_APP_RULES.map(applyMacOverrides)
  : WINDOWS_BUILTIN_APP_RULES

export function createDefaultUserStats(): UserStats {
  return {
    totalWords: 0,
    totalSessions: 0,
    domainWords: {},
    appUsageCount: {},
  }
}
