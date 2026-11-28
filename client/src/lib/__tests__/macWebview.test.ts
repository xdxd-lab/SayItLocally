import { describe, expect, it } from 'vitest'
import { shouldShowNativeContextMenu } from '../macWebview'

/** 只造出 shouldShowNativeContextMenu 會用到的 closest()：命中給定選擇器裡任意一個就算 */
function fakeTarget(matches: string[]): EventTarget {
  return {
    closest(selector: string) {
      const parts = selector.split(',').map((part) => part.trim())
      return parts.some((part) => matches.includes(part)) ? {} : null
    },
  } as unknown as EventTarget
}

describe('macOS 右鍵選單', () => {
  it('輸入框、文本框和可編輯區域裡保留系統右鍵選單（剪下 / 拷貝 / 貼上）', () => {
    expect(shouldShowNativeContextMenu(fakeTarget(['input']), '')).toBe(true)
    expect(shouldShowNativeContextMenu(fakeTarget(['textarea']), '')).toBe(true)
    expect(shouldShowNativeContextMenu(
      fakeTarget(['[contenteditable]:not([contenteditable="false"])']),
      '',
    )).toBe(true)
  })

  it('空白處、按鈕和連結上不彈 WebKit 的「重新載入」「開啟連結」', () => {
    expect(shouldShowNativeContextMenu(fakeTarget([]), '')).toBe(false)
    expect(shouldShowNativeContextMenu(null, '')).toBe(false)
  })

  it('選中了文字時照常彈出，方便拷貝、查詢', () => {
    expect(shouldShowNativeContextMenu(fakeTarget([]), '識別結果')).toBe(true)
    // 只選中了空白不算
    expect(shouldShowNativeContextMenu(fakeTarget([]), '  \n')).toBe(false)
  })

  it('目標不是元素（比如文本節點、window）時按不可編輯處理', () => {
    expect(shouldShowNativeContextMenu({} as EventTarget, '')).toBe(false)
  })
})
