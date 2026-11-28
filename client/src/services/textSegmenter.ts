/**
 * 純 ASR 文本智慧分段
 *
 * 僅在極速模式（無 AI 校對）下使用。
 * LLM 校對後的文本已經有自然分段，不需要再處理。
 *
 * 核心邏輯：在句末標點之後，檢測話題轉換訊號來決定是否插入換行。
 * 不以字數為主要依據，而是以語義轉換為驅動。
 */

// 話題轉換詞（出現在句末標點之後，表示新話題開始）
// 按優先順序排列，長詞優先匹配
// i18n-allow-start: 中文 ASR 分段演算法的詞表與標點資料
const TOPIC_SHIFT_PHRASES = [
  // 序号类
  '第一个', '第二个', '第三个', '第四个', '第五个',
  '第一点', '第二点', '第三点',
  '第一,', '第二,', '第三,',
  '第一，', '第二，', '第三，',
  '首先', '其次',
  // 轉折/遞進類
  '另外呢', '另外,', '另外，', '另外就是', '另外我',
  '还有就是', '还有一个', '还有呢',
  '再就是', '再一个',
  '此外',
  '接下来',
  '最后呢', '最后,', '最后，', '最后就是',
  // 總結類
  '总之', '总的来说',
  // [自訂] SC-67：辨識輸出已轉成台灣正體，正體寫法也要認得（上面的簡體寫法保留，供舊文字使用）。
  '第一個', '第二個', '第三個', '第四個', '第五個',
  '第一點', '第二點', '第三點',
  '還有就是', '還有一個', '還有呢',
  '再一個',
  '接下來',
  '最後呢', '最後,', '最後，', '最後就是',
  '總之', '總的來說',
]

// 語氣詞開頭模式：句末標點後跟 "嗯，/呃，/嗯" + 轉換訊號
// 這類需要前段足夠長（≥ 80 字）才觸發，避免誤切口頭禪
const FILLER_THEN_SHIFT = [
  '嗯，所以', '嗯，我觉得', '嗯，我发现',
  '嗯，现在', '嗯，目前', '嗯，如果',
  '嗯，这样', '嗯，还有', '嗯，另外',
  '呃，所以', '呃，我觉得', '呃，然后',
  // [自訂] SC-67：正體寫法
  '嗯，我覺得', '嗯，現在', '嗯，這樣', '嗯，還有', '呃，我覺得', '呃，然後',
]

// 句末標點
const SENTENCE_END_RE = /[。？！]/
// i18n-allow-end

/**
 * 對純 ASR 輸出的文本進行智慧分段，插入換行符。
 *
 * @param text - ASR 原始文本（一整段無換行）
 * @returns 分段后的文本（用 \n\n 分隔段落）
 */
export function segmentAsrText(text: string): string {
  if (!text || text.length < 40) return text

  const segments: string[] = []
  let currentStart = 0
  let i = 0

  while (i < text.length) {
    // 找到句末標點
    if (!SENTENCE_END_RE.test(text[i])) {
      i++
      continue
    }

    // 找到了句末標點，位置 i
    const afterPunc = i + 1
    if (afterPunc >= text.length) {
      // 文本結束，不需要切
      i++
      continue
    }

    const currentLen = afterPunc - currentStart
    const remaining = text.slice(afterPunc)

    // 檢查 1：直接話題轉換詞（前段 ≥ 20 字即可觸發）
    if (currentLen >= 20 && startsWithAny(remaining, TOPIC_SHIFT_PHRASES)) {
      segments.push(text.slice(currentStart, afterPunc))
      currentStart = afterPunc
      i = afterPunc
      continue
    }

    // 檢查 2：語氣詞 + 轉換訊號（前段 ≥ 60 字才觸發，避免誤切）
    if (currentLen >= 60 && startsWithAny(remaining, FILLER_THEN_SHIFT)) {
      segments.push(text.slice(currentStart, afterPunc))
      currentStart = afterPunc
      i = afterPunc
      continue
    }

    // 檢查 3：兜底——超過 250 字在句末標點處強制切
    if (currentLen >= 250) {
      segments.push(text.slice(currentStart, afterPunc))
      currentStart = afterPunc
      i = afterPunc
      continue
    }

    i++
  }

  // 收尾：剩餘文本作為最後一段
  if (currentStart < text.length) {
    segments.push(text.slice(currentStart))
  }

  return segments.join('\n\n')
}

function startsWithAny(text: string, prefixes: string[]): boolean {
  for (const prefix of prefixes) {
    if (text.startsWith(prefix)) return true
  }
  return false
}
