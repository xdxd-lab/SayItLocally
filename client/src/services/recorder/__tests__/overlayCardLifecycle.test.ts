import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const mocks = vi.hoisted(() => ({
  presentOverlay: vi.fn((_data: unknown) => Promise.resolve(1)),
  updateOverlay: vi.fn((_data: unknown) => Promise.resolve()),
  hideOverlay: vi.fn(() => Promise.resolve()),
  setEscapeActionMode: vi.fn((_mode: string, _token?: number) => Promise.resolve()),
  setCardHotkeys: vi.fn((_actions: string[], _token?: number) => Promise.resolve()),
}))

vi.mock('../../bridge', () => ({
  presentOverlay: mocks.presentOverlay,
  updateOverlay: mocks.updateOverlay,
  hideOverlay: mocks.hideOverlay,
  setEscapeActionMode: mocks.setEscapeActionMode,
  setCardHotkeys: mocks.setCardHotkeys,
  copyText: vi.fn(() => Promise.resolve()),
}))
vi.mock('../../debugLog', () => ({ addRuntimeEvent: vi.fn() }))
vi.mock('../../store', () => ({
  getSetting: vi.fn((_key: string, fallback: unknown) => Promise.resolve(fallback)),
}))
vi.mock('@/i18n', () => ({
  getLocale: () => 'zh-TW',
  t: (key: string) => key,
}))

// 與 overlayListeningPayload.test.ts 同樣的理由：不要用頂層 await import()。
import { OverlayService } from '../OverlayService'

/** Rust 側 CARD_HOTKEY_TTL_MS = 20s，前端續期間隔 8s。 */
const KEEPALIVE_MS = 8000
const NATIVE_TTL_MS = 20000

function newService() {
  return new OverlayService(() => 0)
}

function hotkeyCalls(): Array<[string[], number]> {
  return mocks.setCardHotkeys.mock.calls.map(([actions, token]) => [actions ?? [], token ?? 0])
}

function escapeModes(): string[] {
  return mocks.setEscapeActionMode.mock.calls.map(([mode]) => String(mode))
}

/** 取最後一次呼叫的 payload。刻意不用 `.at(-1)` —— 本倉庫的 lib 目標是 ES2020。 */
function lastPayload(fn: { mock: { calls: unknown[][] } }): Record<string, unknown> | undefined {
  const calls = fn.mock.calls
  if (calls.length === 0) return undefined
  return calls[calls.length - 1][0] as Record<string, unknown>
}

describe('悬浮窗卡片的生命周期', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    mocks.presentOverlay.mockClear()
    mocks.updateOverlay.mockClear()
    mocks.hideOverlay.mockClear()
    mocks.setEscapeActionMode.mockClear()
    mocks.setCardHotkeys.mockClear()
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  /**
   * 這條釘的是 issue #71 的核心改動，**沒有任何時長上限**。
   *
   * 舊行為是 10 秒自動隱藏，等於把"文本還沒交付"當成"通知已讀"：使用者去夠滑鼠的
   * 時間裡卡片就沒了，而那段文字此刻既不在輸入框、也不在剪貼簿（不可編輯那條路
   * 刻意不提前改寫剪貼簿），關了歷史的使用者更是連別處都找不回來。
   *
   * 所以斷言取半小時 —— 任何"到時自動隱藏"的實現都過不了這條。中間一次都不許 hide。
   */
  it('结果卡永不自动消失', () => {
    const service = newService()
    service.showFallback('今天天气怎么样', 'not_editable', 7)

    vi.advanceTimersByTime(30 * 60 * 1000)
    expect(mocks.hideOverlay).not.toHaveBeenCalled()
  })

  /**
   * 失敗卡沒有要交付的內容，看過就沒用了，所以它**可以**自動收起。
   *
   * 5 秒而不是原先的 60 秒：錄音已經在歷史記錄裡、原因也在日誌裡，錯過這張卡不丟東西。
   * 60 秒實測的結果是使用者手動去關它 —— 那等於把一條通知做成了待辦事項。
   */
  it('失败卡 5 秒后自动收起', () => {
    const service = newService()
    service.showFailure({ title: 'recorder.emptyAfterProcessingTitle', recovery: 'none', token: 8 })

    vi.advanceTimersByTime(4_000)
    expect(mocks.hideOverlay).not.toHaveBeenCalled()
    vi.advanceTimersByTime(2_000)
    expect(mocks.hideOverlay).toHaveBeenCalled()
  })

  /**
   * 「識別沒出文字」一律走 toast，**不許建卡片**。
   *
   * 斷言釘的是機制不是結果：只斷言"沒有 hideOverlay"或"最終隱藏了"在舊實現上也會綠
   * （舊實現是卡片，60 秒後照樣隱藏）。這裡要求 presentOverlay 收到的 state 是 toast、
   * 並且整個過程沒有註冊過任何全域性按鍵接管 —— 卡片一定會註冊 Esc。
   */
  it('识别没出文字时只弹 toast，不建卡片、不接管按键', () => {
    for (const reason of ['silent', 'no_text'] as const) {
      mocks.presentOverlay.mockClear()
      mocks.setEscapeActionMode.mockClear()
      const service = newService()

      service.showNoSpeech(reason, { runId: 1 })

      const states = mocks.presentOverlay.mock.calls
        .map((call) => (call[0] as { state?: string } | undefined)?.state)
      expect(states).toEqual(['toast'])
      expect(states).not.toContain('failure')
      // 'off' 是解除接管，不算接管；出現 dismiss_fallback 就說明建了卡片。
      const modes = mocks.setEscapeActionMode.mock.calls.map((call) => call[0])
      expect(modes.every((mode) => mode === 'off')).toBe(true)
    }
  })

  /**
   * 卡片既然不自動消失，正常生命週期就只能靠續期證明"我還在"。
   *
   * 斷言的是**機制**而不是結果：Rust 側那個 TTL 是異常兜底，只斷言"按鍵還能用"
   * 在沒有續期的實現上也會全綠（TTL 內本來就能用）。這裡要求在超過 TTL 的時刻
   * 之後仍有新的註冊呼叫發出去。
   */
  it('卡片可见期间会持续续期，续期间隔小于原生 TTL', () => {
    const service = newService()
    service.showFallback('今天天气怎么样', 'not_editable', 7)

    const initialHotkeys = hotkeyCalls().length
    const initialEscapes = escapeModes().length
    expect(KEEPALIVE_MS).toBeLessThan(NATIVE_TTL_MS)

    vi.advanceTimersByTime(NATIVE_TTL_MS + KEEPALIVE_MS)

    const renewals = hotkeyCalls().length - initialHotkeys
    expect(renewals).toBeGreaterThanOrEqual(Math.floor(NATIVE_TTL_MS / KEEPALIVE_MS))
    expect(escapeModes().length).toBeGreaterThan(initialEscapes)
    // 續期必須沿用同一張卡片的 token，否則原生側會把它當成新卡片、重新抓前台視窗。
    for (const [actions, token] of hotkeyCalls()) {
      expect(actions).toEqual(['copy'])
      expect(token).toBe(7)
    }
  })

  it('结果卡注册 Ctrl+C 与 Esc；失败卡只要 Esc', () => {
    const service = newService()
    service.showFallback('今天天气怎么样', 'not_editable', 7)
    expect(hotkeyCalls()[0]).toEqual([['copy'], 7])
    expect(escapeModes()).toContain('dismiss_fallback')

    mocks.setCardHotkeys.mockClear()
    mocks.setEscapeActionMode.mockClear()
    // 失敗卡沒有文本可交付，註冊 Ctrl+C 只會白搶使用者的複製鍵。
    service.showFailure({ title: 'recorder.recognitionFailedTitle', recovery: 'unknown', token: 8 })
    expect(hotkeyCalls().every(([actions]) => actions.length === 0)).toBe(true)
    expect(escapeModes()).toContain('dismiss_fallback')
  })

  it('没有文本的结果卡不注册 Ctrl+C', () => {
    const service = newService()
    service.showFallback('', 'not_editable', 7)
    expect(hotkeyCalls().every(([actions]) => actions.length === 0)).toBe(true)
  })

  /** PTT Lab 的預覽卡片沒有 Orchestrator 代次，絕不能開啟無人消費的全域性按鍵接管。 */
  it('token=0 的预览卡片不接管任何按键', () => {
    const service = newService()
    service.showFallback('预览文本', 'not_editable', 0)

    expect(hotkeyCalls().every(([actions]) => actions.length === 0)).toBe(true)
    expect(escapeModes().every((mode) => mode === 'off')).toBe(true)

    // 也不該留下一個續期定時器。
    mocks.setCardHotkeys.mockClear()
    vi.advanceTimersByTime(NATIVE_TTL_MS * 2)
    expect(mocks.setCardHotkeys).not.toHaveBeenCalled()
  })

  /**
   * 驗收項「複製過程中開始新錄音」的機制側：新錄音會把懸浮窗切到 waiting，
   * 那一刻卡片的按鍵接管必須當場解除。
   *
   * 不解除的症狀很隱蔽：續期定時器每 8 秒把 Esc 重新設成 dismiss_fallback，
   * 把新錄音剛設好的 cancel_recording 覆蓋掉 —— 使用者在錄音中按 Esc，取消的是
   * 一張已經不存在的卡片。
   */
  it('开始新一轮显示会解除上一张卡片的按键接管', () => {
    const service = newService()
    service.showFallback('今天天气怎么样', 'not_editable', 7)

    mocks.setCardHotkeys.mockClear()
    mocks.setEscapeActionMode.mockClear()
    service.showWaiting()

    expect(hotkeyCalls()).toContainEqual([[], 0])

    // 關鍵：續期定時器必須已經停掉，不能再覆蓋新狀態的 Esc 模式。
    mocks.setEscapeActionMode.mockClear()
    service.startListeningTicker(9)
    mocks.setEscapeActionMode.mockClear()
    vi.advanceTimersByTime(KEEPALIVE_MS * 3)
    expect(escapeModes()).not.toContain('dismiss_fallback')

    service.stopListeningTicker()
  })

  it('hide 会解除按键接管', () => {
    const service = newService()
    service.showFallback('今天天气怎么样', 'not_editable', 7)

    mocks.setCardHotkeys.mockClear()
    service.hide()

    expect(hotkeyCalls()).toContainEqual([[], 0])
    expect(escapeModes()).toContain('off')
  })

  /**
   * 迴歸釘：懸浮窗自己收起卡片之後，主窗必須停掉續期。
   *
   * 懸浮窗只是渲染端，卡片的生命週期歸這邊管。它 hideOverlay() 之後如果不回報，
   * 8 秒後續期定時器又把 Esc / Ctrl+C 註冊回來 —— 卡片早就沒了，使用者的按鍵卻還
   * 被接管著，而且原生側會重新抓一次**當前**前台視窗（可能已經是別的程式）。
   *
   * 斷言方式是"此後不再出現任何非空的註冊"，而不是"解除被呼叫過一次" ——
   * 後者在有 bug 的實現上也會全綠（它確實調過一次，只是隨後又註冊回來了）。
   */
  it('悬浮窗收起卡片后不再续期', () => {
    const service = newService()
    service.showFallback('今天天气怎么样', 'not_editable', 7)

    service.noteCardDismissed(7)
    mocks.setCardHotkeys.mockClear()
    mocks.setEscapeActionMode.mockClear()

    vi.advanceTimersByTime(KEEPALIVE_MS * 5)
    expect(hotkeyCalls().every(([actions]) => actions.length === 0)).toBe(true)
    expect(escapeModes().every((mode) => mode === 'off')).toBe(true)
  })

  /** 上一張卡片的關閉回報遲到時，不能把現在這張的按鍵接管一起撤掉。 */
  it('关闭回报只认当前卡片', () => {
    const service = newService()
    service.showFallback('第一段', 'not_editable', 7)
    service.showFallback('第二段', 'not_editable', 9)

    mocks.setCardHotkeys.mockClear()
    service.noteCardDismissed(7)

    expect(mocks.setCardHotkeys).not.toHaveBeenCalled()
    // 第二張卡片照常續期。
    vi.advanceTimersByTime(KEEPALIVE_MS + 100)
    expect(hotkeyCalls()).toContainEqual([['copy'], 9])
  })

  /**
   * 失敗卡上那句"可在歷史記錄裡重新識別"必須由實際存檔結果決定，所以分兩步下發：
   * 先 present（此刻還不知道存沒存下來），存檔有結論後再 update。
   */
  it('恢复结论是随后补发的，且只认当前卡片的 token', () => {
    const service = newService()
    service.showFailure({ title: 'recorder.protocolIncompleteTitle', recovery: 'unknown', token: 8 })
    expect(lastPayload(mocks.presentOverlay)).toMatchObject({
      state: 'failure',
      failureRecovery: 'unknown',
    })

    service.updateFailureRecovery('history', 8)
    expect(lastPayload(mocks.updateOverlay)).toMatchObject({
      state: 'failure',
      failureRecovery: 'history',
    })

    // 上一張卡片的存檔結論遲到時不能改寫現在這張。
    mocks.updateOverlay.mockClear()
    service.updateFailureRecovery('history', 7)
    expect(mocks.updateOverlay).not.toHaveBeenCalled()
  })

  /**
   * 補發恢復結論時必須**重新帶上標題和原因**。
   *
   * 原生側的 update 是整份替換 `latest_overlay_payload`，不是合併。只發
   * `{ state, failureRecovery }` 的話，那份"最近狀態"底稿會變成一份沒有標題的殘缺
   * payload，而介面此刻完全正常（渲染端是合併語義）—— 殘缺只在有人重放底稿時暴露：
   * overlay 重掛後 `overlay_ready` 會重放它。實測 2026-09-23 的現場就是這個：
   * 卡片顯示成「出錯了」+「錄音已儲存」，標題落到兜底文案，原因整行消失。
   *
   * 所以這條斷言釘的是**下發的欄位**，不是介面效果：只斷言"介面正常"在出 bug 的版本上
   * 也是綠的。
   */
  it('补发恢复结论时重新带上标题与原因，不留下残缺的最近状态', () => {
    const service = newService()
    service.showFailure({
      title: 'recorder.recognitionFailedTitle',
      detail: 'err.provider.insufficientBalance',
      recovery: 'unknown',
      token: 8,
    })
    service.updateFailureRecovery('history', 8)

    expect(lastPayload(mocks.updateOverlay)).toMatchObject({
      state: 'failure',
      failureTitle: 'recorder.recognitionFailedTitle',
      failureDetail: 'err.provider.insufficientBalance',
      failureRecovery: 'history',
    })
  })

  it('卡片收起后不再把上一张的标题带进新的下发', () => {
    const service = newService()
    service.showFailure({ title: 'recorder.recognitionFailedTitle', detail: '余额不足', recovery: 'none', token: 8 })
    service.hide()

    // 新一張卡自帶標題；關鍵是 hide 之後舊標題不許殘留。
    mocks.presentOverlay.mockClear()
    service.showFailure({ title: 'recorder.processingTimeoutTitle', recovery: 'none', token: 9 })
    expect(lastPayload(mocks.presentOverlay)).toMatchObject({
      failureTitle: 'recorder.processingTimeoutTitle',
      failureDetail: '',
    })
  })

  /** 寬限期提示是膠囊形態的 thinking 變體，Esc 的語義是"放棄等待"而不是"關卡片"。 */
  it('宽限期等待用独立的 Esc 语义', () => {
    const service = newService()
    service.showAwaitingLateResult(15, 11)

    expect(lastPayload(mocks.presentOverlay)).toMatchObject({
      state: 'thinking',
      thinkingNote: 'late',
    })
    expect(escapeModes()).toContain('abandon_late_result')
    // 這一步沒有卡片按鈕，不該接管 Ctrl+C。
    expect(hotkeyCalls().every(([actions]) => actions.length === 0)).toBe(true)
  })
})
