import { describe, it, expect } from 'vitest'
import { reduceBackdropDismiss, type BackdropPointerEvent } from '../useBackdropDismiss'

/** 按順序喂一串事件，返回每一步是否觸發了關閉 */
function play(events: BackdropPointerEvent[]): boolean[] {
  let armed = false
  return events.map((event) => {
    const next = reduceBackdropDismiss(armed, event)
    armed = next.armed
    return next.dismiss
  })
}

const downOn = { type: 'mousedown', onBackdrop: true } as const
const downOff = { type: 'mousedown', onBackdrop: false } as const
const clickOn = { type: 'click', onBackdrop: true } as const
const clickOff = { type: 'click', onBackdrop: false } as const

describe('點背板關閉彈窗的判據', () => {
  it('在背板空白處點一下 → 關閉', () => {
    expect(play([downOn, clickOn])).toEqual([false, true])
  })

  /**
   * 這條就是使用者報的 bug：在輸入框裡按下滑鼠拖動選字，中途移出面板才鬆開。
   * 瀏覽器把 click 派發到「按下目標與鬆開目標的最近公共祖先」= 鋪滿全屏的背板，
   * 所以 click 確實落在背板上（onBackdrop: true），但**按下**不在背板上。
   */
  it('面板內按下、拖到面板外鬆開 → 不關（草稿不能就這麼丟了）', () => {
    expect(play([downOff, clickOn])).toEqual([false, false])
  })

  it('整個互動都在面板內 → 不關', () => {
    expect(play([downOff, clickOff])).toEqual([false, false])
  })

  /**
   * 在背板上按下、拖出應用視窗外鬆開：不會產生 click，撤防的機會沒了。
   * 若不在 click 到達時無條件撤防，這個懸著的標誌會被下一次面板內的
   * 鍵盤 click（Enter 觸發的合成 click）撿走，變成一次莫名的關閉。
   */
  it('背板按下但沒等到 click，之後面板內的 click 不能關', () => {
    expect(play([downOn, clickOff, clickOff])).toEqual([false, false, false])
  })

  it('關過一次之後要重新按下才能再關', () => {
    expect(play([downOn, clickOn, clickOn])).toEqual([false, true, false])
  })

  it('mousedown 自己永遠不關閉 —— 要等鬆開，否則拖選背景文字也會關', () => {
    expect(play([downOn, downOn])).toEqual([false, false])
  })
})
