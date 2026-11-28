import { describe, expect, it } from 'vitest'

// 原始碼當字串讀進來（Vite 的 `?raw`）。用它而不是 node:fs 是因為本專案沒裝
// @types/node，而 `vite/client` 已經宣告了 `*?raw` —— 與 hotwordCapability.test.ts 同款。
import SOURCE from '../RecorderOrchestrator.ts?raw'

/**
 * 「識別沒出文字」不許彈卡片 —— 用讀原始碼的方式釘住。
 *
 * 為什麼不用行為測試：這個判據在 RecorderOrchestrator 裡**有三條獨立路徑**各走一遍
 * （onAsr 的空結果、onFinal 的空結果、onDone 的協議異常），而它們各自要一整套
 * provider / WebSocket / 歷史存檔的樁才能跑起來。2026-09-23 的實際代價是：只改了
 * 其中一條，另一條照舊彈卡片，使用者原樣復現 —— 而當時改動配的單測（OverlayService
 * 層面的 showNoSpeech 只發 toast）是全綠的，因為它壓根沒覆蓋到漏掉的那條路徑。
 *
 * 所以這裡直接斷言**原始碼裡不存在那種寫法**：任何新增的第四條路徑也會被它抓住。
 */
/** 彈卡片的兩個入口。`failRunWithCard` 內部就是 showFailure + 存檔。 */
const CARD_ENTRY_POINTS = ['showFailure(', 'failRunWithCard(']

/**
 * 允許彈卡片的標題 key。**加新 key 進來之前先問一句：這件事需要使用者去做點什麼嗎？**
 * 不需要的就該走 showNoSpeech。
 *
 * - emptyAfterProcessing：識別出字了、被使用者自己的替換規則清空 → 要他去改規則
 * - protocolIncomplete：服務端結束會話卻沒回結果 → 確實出錯了
 * - connectionLost / processingTimeout / recognitionFailed：同上
 */
const CARD_TITLE_KEYS = [
  'recorder.emptyAfterProcessingTitle',
  'recorder.protocolIncompleteTitle',
  'recorder.connectionLostTitle',
  'recorder.processingTimeoutTitle',
  'recorder.recognitionFailedTitle',
]

describe('識別沒出文字不彈卡片', () => {
  it('原始碼讀到了，不是空字串（否則下面幾條會假綠）', () => {
    expect(SOURCE.length).toBeGreaterThan(1000)
    expect(SOURCE).toContain('showNoSpeech')
  })

  it('卡片入口一個都沒少認', () => {
    // 入口名改了而這裡沒跟著改，下面的斷言就會形同虛設。
    for (const entry of CARD_ENTRY_POINTS) {
      expect(SOURCE, `卡片入口 ${entry} 在原始碼裡找不到了，先確認它是不是改名了`)
        .toContain(entry)
    }
  })

  it('沒有任何一處取用已刪除的「沒有取得識別結果」文案', () => {
    // 這兩個 key 已經從兩份 locale 裡刪掉了，取它只會渲染出 key 本身。
    // 只查真實呼叫形式 `t('...')`，不查裸識別符號 —— 否則註釋裡提一句名字都會誤報。
    expect(SOURCE).not.toContain("t('recorder.noResultTitle')")
    expect(SOURCE).not.toContain("t('recorder.noResultDetail')")
  })

  it('每一處卡片的標題都在白名單裡', () => {
    // 逐個卡片入口往後掃一小段，把它傳的 title key 抓出來比對。
    const titles: string[] = []
    for (const entry of CARD_ENTRY_POINTS) {
      let from = 0
      for (; ;) {
        const at = SOURCE.indexOf(entry, from)
        if (at < 0) break
        from = at + entry.length
        // 引數物件就在呼叫後面，標題是第一個 t('recorder.xxxTitle')。
        const window = SOURCE.slice(at, at + 400)
        const matched = /title:\s*t\('([^']+)'\)/.exec(window)
        if (matched) titles.push(matched[1])
      }
    }

    // 抓到 0 條說明正則和原始碼寫法不再匹配 —— 那時這條斷言等於沒跑。
    expect(titles.length).toBeGreaterThan(0)
    for (const title of titles) {
      expect(CARD_TITLE_KEYS, `卡片標題 ${title} 不在白名單裡`).toContain(title)
    }
  })

  it('兩條空結果路徑都調了 showNoSpeech，且都帶 no_text 分支', () => {
    // 漏改的那一次，原始碼裡 showNoSpeech 只出現在一條路徑上。
    const noSpeechCalls = SOURCE.match(/showNoSpeech\(/g) ?? []
    expect(noSpeechCalls.length).toBeGreaterThanOrEqual(3)
    const noTextBranches = SOURCE.match(/silenceProven \? 'silent' : 'no_text'/g) ?? []
    expect(noTextBranches.length).toBe(2)
  })
})
