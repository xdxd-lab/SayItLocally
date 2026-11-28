import { describe, it, expect } from 'vitest'
import { segmentAsrText } from '../textSegmenter'

describe('segmentAsrText', () => {
  it('短文本不分段', () => {
    expect(segmentAsrText('你好世界')).toBe('你好世界')
    expect(segmentAsrText('')).toBe('')
  })

  it('不足 40 字不分段', () => {
    const text = '這是一段不到四十個字的文本。'
    expect(segmentAsrText(text)).toBe(text)
  })

  it('在話題轉換詞處分段', () => {
    // 前段需要 ≥ 20 字才會在話題轉換詞處切分
    const text = '我今天做了很多事情，完成了專案的第一個版本，還做了很多測試工作。另外我還修復了幾個重要的 bug，提交了程式碼。'
    const result = segmentAsrText(text)
    expect(result).toContain('\n\n')
    expect(result.split('\n\n')).toHaveLength(2)
  })

  it('超過 250 字在句末標點處強制切', () => {
    // 構造一段超過 250 字的無話題轉換詞文本
    const sentence = '這是一段很長的文本內容用來測試強制分段的邏輯'
    let text = ''
    while (text.length < 260) {
      text += sentence
    }
    text += '。後面還有一些內容。'
    const result = segmentAsrText(text)
    expect(result).toContain('\n\n')
  })

  it('null/undefined 安全處理', () => {
    expect(segmentAsrText(null as unknown as string)).toBeFalsy()
    expect(segmentAsrText(undefined as unknown as string)).toBeFalsy()
  })
})

// [自訂] SC-67：辨識輸出已轉成台灣正體，分段詞的正體寫法也要觸發分段。
describe('segmentAsrText：正體輸入', () => {
  it('正體的話題轉換詞與簡體一樣分段', () => {
    const lead = '今天我們先把登入頁面的錯誤修好然後推到測試環境讓大家確認一下。'
    for (const shift of ['還有就是', '接下來', '最後就是', '總的來說', '第二個']) {
      const text = `${lead}${shift}要把文件補齊，順便整理一下待辦清單裡的項目。`
      expect(segmentAsrText(text), shift).toBe(`${lead}\n\n${shift}要把文件補齊，順便整理一下待辦清單裡的項目。`)
    }
  })
})
