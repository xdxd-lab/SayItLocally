import type { AppPromptRule, UserStats } from './types'
import { IS_MAC } from '@/lib/platform'

// 這裡的**陣列順序就是預設優先順序**（自上而下先命中先生效，見 promptRouter）。
// 使用者可以在介面上拖動調整，調整後的順序存進 appPromptRules；這份清單只決定
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
    promptAppend: '適合即時協作聊天。優先輸出可以直接傳送的短訊息，語氣自然、清晰、簡潔，避免郵件腔。', // i18n-allow: 中文口述整理 Prompt
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
    promptAppend: '適合工作郵件草稿。語氣正式、完整，必要時自然分段，但不要編造收件人、稱呼或任何事實。', // i18n-allow: 中文口述整理 Prompt
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
    promptAppend: '面向開發工具輸入。保留程式碼、命令、檔名、路徑、英文識別字和 Markdown 結構，不要把技術詞改寫成普通中文。', // i18n-allow: 中文口述整理 Prompt
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
    promptAppend: '面向 Codex 編碼工具，多為用自然語言下達程式設計指令。保留程式碼、命令、檔名、路徑和英文識別字，把要做的事說清楚，不要把技術詞改寫成普通中文。', // i18n-allow: 中文口述整理 Prompt
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
    promptAppend: '面向 VSCode 編輯區。保留程式碼、命令、檔名、API、英文術語和 Markdown 結構，不要過度潤色技術內容。', // i18n-allow: 中文口述整理 Prompt
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
    promptAppend: '面向 Cursor 編輯區。保留程式碼、命令、路徑、技術名詞和英文識別字，不要擅自解釋或補全技術內容。', // i18n-allow: 中文口述整理 Prompt
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
    promptAppend: '面向 Windows 記事本，適合隨手記錄的純文字。輸出純文字，不要使用 Markdown 標記或特殊格式符號。', // i18n-allow: 中文口述整理 Prompt
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
    promptAppend: '適合微信聊天。輸出可直接傳送的自然口語短訊息，簡潔親切，不要用書面或郵件腔。', // i18n-allow: 中文口述整理 Prompt
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
    promptAppend: '適合 QQ 聊天。輸出輕鬆自然、可直接傳送的短訊息，口語化、簡潔。', // i18n-allow: 中文口述整理 Prompt
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
  // mac 上沒有記事本，對應系統自帶的「文字編輯」
  notepad: {
    name: 'TextEdit',
    promptAppend: '面向「文字編輯」，適合隨手記錄的純文字。輸出純文字，不要使用 Markdown 標記或特殊格式符號。', // i18n-allow: 中文口述整理 Prompt
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
