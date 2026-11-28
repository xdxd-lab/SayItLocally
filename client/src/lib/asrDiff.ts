import diff from 'fast-diff'

/**
 * ASR 糾錯面板用的 diff：原始識別文本 → 使用者修正後的文本。
 *
 * 渲染層不參與計算，是為了能在沒有 jsdom 的環境裡單測（本專案 vitest 沒裝 jsdom）。
 *
 * 用 fast-diff 而不是自己寫：它就是 diff-match-patch 的演算法，自帶
 * `cleanupSemantic` / `cleanupSemanticLossless`，會把改動邊界往空白和標點上挪。
 * 這對英文很關鍵 —— 純字元級 diff 會把 "recognize"→"recognise" 切成
 * 「recogni + s/z + e」這種碎片，讀起來像亂碼。中文沒有詞邊界，天然按字，正好。
 */
export type AsrDiffKind = 'equal' | 'delete' | 'insert'

export interface AsrDiffSegment {
  kind: AsrDiffKind
  text: string
}

/** 統一行尾並去掉首尾空白：只有行尾不同不該顯示成"有改動"。 */
export function normalizeForDiff(text: string): string {
  return text.replace(/\r\n/g, '\n').replace(/\r/g, '\n').trim()
}

export function computeAsrDiff(original: string, corrected: string): AsrDiffSegment[] {
  const a = normalizeForDiff(original)
  const b = normalizeForDiff(corrected)
  if (a === b) return a ? [{ kind: 'equal', text: a }] : []

  // 第 4 個引數 = 語義整理，別省
  return diff(a, b, undefined, true)
    .filter(([, text]) => text.length > 0)
    .map(([op, text]): AsrDiffSegment => ({
      kind: op === diff.INSERT ? 'insert' : op === diff.DELETE ? 'delete' : 'equal',
      text,
    }))
}

/** 改動處數（連續的一刪一插算一處），用於給使用者一個"改了幾處"的量感。 */
export function countAsrDiffChanges(segments: AsrDiffSegment[]): number {
  let count = 0
  let inChange = false
  for (const segment of segments) {
    if (segment.kind === 'equal') {
      inChange = false
      continue
    }
    if (!inChange) {
      count += 1
      inChange = true
    }
  }
  return count
}

export function hasAsrDiffChange(segments: AsrDiffSegment[]): boolean {
  return segments.some((segment) => segment.kind !== 'equal')
}
