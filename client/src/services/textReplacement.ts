// 文本替換服務 — 對 ASR 識別結果做客戶端側的文本替換

import { invoke } from '@tauri-apps/api/core'
import { getSetting, setSetting } from './store'

export interface TextReplacementRule {
  id: string
  from: string
  to: string
  enabled: boolean
}

const STORAGE_KEY = 'textReplacements'

/** 內建預設文本替換規則（新使用者首次使用時填充） */
// i18n-allow-start: 中文 ASR 預設替換資料，不是介面文案
export const BUILTIN_REPLACEMENTS: TextReplacementRule[] = [
  // [自訂] SC-67：辨識輸出已轉成台灣正體，內建規則跟著用正體寫法。
  { id: 'builtin_1', from: '安卓說話', to: '按住說話', enabled: true },
  { id: 'builtin_2', from: '我的郵箱', to: 'test@example.com', enabled: true },
  { id: 'builtin_3', from: 'pump', to: 'Prompt', enabled: true },
  { id: 'builtin_4', from: 'Cloud Code', to: 'Claude Code', enabled: true },
]
// i18n-allow-end

export async function getTextReplacements(): Promise<TextReplacementRule[]> {
  const rules = await getSetting<TextReplacementRule[]>(STORAGE_KEY, [])
  // 新使用者沒有任何規則時，返回內建預設
  if (!rules || rules.length === 0) {
    return BUILTIN_REPLACEMENTS.map((r) => ({ ...r }))
  }
  return rules
}

export async function saveTextReplacements(rules: TextReplacementRule[]): Promise<void> {
  await setSetting(STORAGE_KEY, rules)
}

/**
 * [自訂] SC-67：新增或批次匯入規則時，「替換成」欄位用辨識輸出同一個轉換器轉成台灣正體
 * （含假名的句子不轉）。規則在 Rust 出口之後才套用，所以不對最終文字再轉一次。
 * 比對欄（from）不經這裡。沒有漢字時不呼叫；轉換失敗時保留原文並記錄。
 */
export async function toTaiwanReplacementTargets(texts: string[]): Promise<string[]> {
  if (!texts.some((text) => /[\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff]/.test(text))) return texts
  try {
    const converted = await invoke<string[]>('zh_to_taiwan_texts', { texts })
    if (Array.isArray(converted) && converted.length === texts.length) return converted
    console.warn('[text-replacement] unexpected conversion result')
  } catch (error) {
    console.warn('[text-replacement] conversion failed', error)
  }
  return texts
}

/** 批次文本解析出的規則草稿（尚未分配 id/enabled） */
export interface ParsedReplacement {
  from: string
  to: string
}

/**
 * 解析批次輸入文本為替換規則草稿。
 * - 每行一條規則；空行忽略。
 * - 分隔符支援：製表符（從表格貼上）、`=>`、`->`、中文逗號「，」、英文逗號「,」。
 *   取整行中最靠前出現的分隔符切分：分隔符前為「原文」，其後（含其餘分隔符）為「替換為」。
 * - 無分隔符的行：整行作為「原文」，替換為留空（即刪除該文本）。
 * - 原文為空的行忽略。
 */
export function parseBatchReplacements(input: string): ParsedReplacement[] {
  const separators = ['\t', '=>', '->', '，', ','] // i18n-allow: 中文批次輸入分隔符
  const result: ParsedReplacement[] = []

  for (const rawLine of input.split(/\r?\n/)) {
    const line = rawLine.trim()
    if (!line) continue

    let sepIndex = -1
    let sepLen = 0
    for (const sep of separators) {
      const i = line.indexOf(sep)
      if (i >= 0 && (sepIndex === -1 || i < sepIndex)) {
        sepIndex = i
        sepLen = sep.length
      }
    }

    let from = line
    let to = ''
    if (sepIndex >= 0) {
      from = line.slice(0, sepIndex).trim()
      to = line.slice(sepIndex + sepLen).trim()
    }

    if (!from) continue
    result.push({ from, to })
  }

  return result
}

/** 對文本應用所有啟用的替換規則 */
export function applyReplacements(text: string, rules: TextReplacementRule[]): string {
  let result = text
  for (const rule of rules) {
    if (rule.enabled && rule.from) {
      result = result.split(rule.from).join(rule.to)
    }
  }
  return result
}

/** 載入規則並應用替換（便捷方法） */
export async function applyTextReplacements(text: string): Promise<string> {
  if (!text) return text
  const rules = await getTextReplacements()
  if (rules.length === 0) return text
  return applyReplacements(text, rules)
}
