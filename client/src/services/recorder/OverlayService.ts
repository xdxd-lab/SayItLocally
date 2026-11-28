import { getLocale, t } from '@/i18n'
import * as bridge from '../bridge'
import { addRuntimeEvent } from '../debugLog'
import { getSetting } from '../store'
import { clampSec } from '../timeModel'
import {
  DEFAULT_CUSTOM_WAVE,
  normalizeCustomWave,
  normalizeWaveTheme,
  type CustomWaveColors,
} from '@/lib/overlayWaveColors'
import {
  formatRecordingLimit,
  OVERLAY_WIDTH_PRESETS,
  type OverlayCommonPayload,
  type OverlayWaveTheme,
  type OverlayWidthPreset,
} from './types'
import type { MicSourceMode } from './micSourceReminder'

type OverlayVisualState =
  | 'waiting'
  | 'listening'
  | 'thinking'
  | 'fallback'
  | 'failure'
  | 'error'
  | 'toast'

/**
 * 卡片快捷鍵/Esc 的續期間隔。
 *
 * 必須明顯小於 Rust 側的硬 TTL（卡片快捷鍵 20s、dismiss_fallback 30s）—— 那兩個 TTL
 * 是異常兜底（渲染端卡死時不能永久吞使用者的按鍵），不是正常生命週期。卡片現在不再
 * 自動消失，所以正常生命週期只能靠這裡續。
 */
const CARD_KEEPALIVE_INTERVAL_MS = 8000

/** listening 補位定時器的間隔與閾值。取值原因見 startListeningTicker。 */
const LISTENING_TICKER_IDLE_MS = 250

/**
 * 失敗卡的可見時長。
 *
 * 只有**失敗卡**能自動收起：它裡面沒有需要交付給使用者的東西，只有一條原因和一個
 * 操作機會，看過就沒用了。
 *
 * 結果卡刻意沒有這個上限 —— 裡面那段文字此刻既不在輸入框、也不在剪貼簿（不可編輯
 * 那條路不提前改寫剪貼簿），關了歷史的使用者更是連別處都沒有。任何"到時自動隱藏"都
 * 等於把"文本還沒交付"當成"通知已讀"：使用者離開幾分鐘回來，文字就沒了。它的收場只
 * 有三條：Esc / 點關閉 / 開始下一次口述。
 */
/**
 * 失敗卡自動收起的時間。
 *
 * 5 秒，不按內容長短分檔。這張卡不需要承擔"必須被讀到"的責任 —— 錄音已經在歷史記錄裡，
 * 原因也在日誌裡，錯過了不丟任何東西（結果卡不一樣，那上面的文字只此一份，所以它
 * 不自動消失，見 showFallback）。原先是 60 秒，實測使用者是手動關掉的，等於把一條
 * 通知做成了待辦事項。
 */
const FAILURE_CARD_VISIBLE_MS = 5 * 1000

/** 失敗卡的恢復提示：音訊到底有沒有存下來，只能由實際存檔結果決定。 */
export type FailureRecovery = 'unknown' | 'history' | 'none'

interface MicSourceHint {
  mode: MicSourceMode
  label: string
}

const MIC_SOURCE_HINT_DURATION_MS = 3000

function normalizeWidthPreset(value: unknown): OverlayWidthPreset {
  if (value === 'short' || value === 'medium' || value === 'long') return value
  return 'long'
}

export class OverlayService {
  private theme: OverlayWaveTheme = 'black-rainbow'
  private customWave: CustomWaveColors = DEFAULT_CUSTOM_WAVE
  private showDuration = true
  private readySoundEnabled = true
  private widthPreset: OverlayWidthPreset = 'medium'
  private lastFrameAt = 0
  private tickerId: ReturnType<typeof setInterval> | null = null
  private fallbackHideId: ReturnType<typeof setTimeout> | null = null
  /** Persistent warning text — included in every overlay update until cleared */
  private activeWarning = ''
  private timeoutWarningHideId: ReturnType<typeof setTimeout> | null = null
  /** 流式即時識別文本 — 錄音期間隨中間結果更新，會被併入每次 listening 更新一起下發 */
  private streamingText = ''
  /** 本次錄音是否開啟流式即時顯示：為真則從錄音一開始就顯示氣泡（佔位），中途不再縮放視窗 */
  private streamingActive = false
  private currentState: OverlayVisualState = 'waiting'
  /**
   * 當前失敗卡的標題與原因。
   *
   * 存著它只為一件事：`updateFailureRecovery` 傳送時把這兩個欄位**重新帶上**。
   * 原生側的 `update_overlay_state` 是**整份替換** `latest_overlay_payload`，不是合併
   * （見 window/mod.rs）。所以只發 `{ state, failureRecovery }` 的話，那份"最近狀態"
   * 底稿會被換成一份沒有標題、沒有原因的殘缺 payload —— 而介面此刻看起來完全正常，
   * 因為渲染端是合併語義（沒傳的欄位保留上一次的值）。
   *
   * 殘缺只在**有人重放底稿**時暴露：overlay 重掛後 `overlay_ready` 會重放最近狀態，
   * 健康恢復與 ack 超時重發也會。實測 2026-09-23：overlay 因整頁過載重掛，重放到
   * 這份殘缺 payload，卡片顯示成「出錯了」+「錄音已儲存」，標題落到兜底文案、
   * 原因整行消失（內容高度從 125.33 掉到 81.33）。
   *
   * 為什麼不去改原生那側的替換語義：錄音中的 listening 更新在不流式時**不帶**
   * `streaming` 欄位，改成合並的話它會殘留在底稿裡，重放時懸浮窗會畫出即時字幕氣泡、
   * 視窗也跟著變大 —— 那是 pitfalls 30 那一族，代價比這裡大得多。
   */
  private activeFailureCard: { title: string; detail: string } | null = null

  private activeMicSourceHint: MicSourceHint | null = null
  private micSourceHintHideId: ReturnType<typeof setTimeout> | null = null
  private micSourceHintGeneration = 0
  /** 常駐卡片的按鍵續期定時器；卡片一消失必須停掉，否則會替別人續期。 */
  private cardKeepAliveId: ReturnType<typeof setInterval> | null = null
  private activeCardToken = 0
  private activeCardHotkeys: bridge.CardHotkeyAction[] = []
  private activeCardEscapeMode: bridge.EscapeActionMode = 'off'

  constructor(private readonly getElapsedSec: () => number) { }

  async refreshSettings() {
    this.theme = normalizeWaveTheme(await getSetting('overlayWaveTheme', 'black-rainbow'), 'black-blue')
    this.customWave = normalizeCustomWave(await getSetting('overlayWaveCustom', DEFAULT_CUSTOM_WAVE))
    this.showDuration = Boolean(await getSetting('overlayShowDuration', true))
    this.readySoundEnabled = Boolean(await getSetting('readySoundEnabled', true))
    this.widthPreset = normalizeWidthPreset(await getSetting('overlayWidth', 'medium'))
  }

  private readySoundCtx: AudioContext | null = null
  private readyTonesPlaying = 0

  private playReadySound() {
    if (!this.readySoundEnabled) return
    try {
      if (!this.readySoundCtx || this.readySoundCtx.state === 'closed') {
        this.readySoundCtx = new AudioContext()
      }
      const ctx = this.readySoundCtx
      // AudioContext 新建或長時間無操作後常處於 suspended 狀態（瀏覽器自動播放/省電策略）。
      // resume() 是非同步的，若不等它完成就 start()，振盪器可能被靜默丟棄或延遲——
      // 這正是「第一次按免持鍵聽不到提示音，過一會又聽不到」的根因。
      if (ctx.state === 'suspended') {
        ctx.resume().then(() => this.emitReadyTone(ctx)).catch(() => { /* ignore */ })
      } else {
        this.emitReadyTone(ctx)
      }
    } catch { /* ignore */ }
  }

  private emitReadyTone(ctx: AudioContext) {
    try {
      const osc = ctx.createOscillator()
      const gain = ctx.createGain()
      osc.connect(gain)
      gain.connect(ctx.destination)
      osc.frequency.value = 880
      osc.type = 'sine'
      gain.gain.setValueAtTime(0.2, ctx.currentTime)
      gain.gain.exponentialRampToValueAtTime(0.001, ctx.currentTime + 0.15)
      osc.start(ctx.currentTime)
      osc.stop(ctx.currentTime + 0.15)
      // 播完就把 AudioContext 掛起。一個處於 running 的 AudioContext 哪怕什麼都不播，
      // 渲染程序和音訊服務程序也會每 10ms 跑一次音訊圖：2026-10 實測空閒 20 秒多耗約 360ms CPU
      // （約 1.8% 個核），而且從第一次錄音起一直耗到退出 —— 正式版「用久了空閒 CPU 變高」就是它。
      // 下次播放前 playReadySound 會先 resume。計數是為了上一聲沒播完時別把下一聲掐掉。
      this.readyTonesPlaying++
      osc.onended = () => {
        this.readyTonesPlaying = Math.max(0, this.readyTonesPlaying - 1)
        if (this.readyTonesPlaying === 0 && ctx.state === 'running') {
          ctx.suspend().catch(() => { /* ignore */ })
        }
      }
    } catch { /* ignore */ }
  }

  getCommonPayload(): OverlayCommonPayload {
    const cfg = OVERLAY_WIDTH_PRESETS[this.widthPreset]
    return {
      theme: this.theme,
      customWave: this.customWave,
      showDuration: this.showDuration,
      baseWidth: cfg.windowWidth,
      barCount: cfg.barCount,
      // 每次都帶上：懸浮窗可能在語言切換之後才第一次建立，沒有"只發一次"的時機。
      locale: getLocale(),
    }
  }

  getBarCount(): number {
    return OVERLAY_WIDTH_PRESETS[this.widthPreset].barCount
  }

  private setEscapeMode(mode: bridge.EscapeActionMode, token = 0) {
    void bridge.setEscapeActionMode(mode, token).catch(() => { /* 原生鉤子不可用時不影響懸浮窗 */ })
  }

  /**
   * 切換到一個**非卡片**狀態：順手結束上一張卡片的按鍵接管。
   *
   * 所有短暫狀態都必須走這裡，不要直接調 setEscapeMode —— 否則卡片還在續期（它的
   * 定時器每 8 秒把 Esc/Ctrl+C 重新註冊一次），會把新狀態剛設好的模式覆蓋掉，
   * 表現為"新錄音開始了，Esc 卻還在關那張已經不存在的卡片"。
   */
  private enterTransientState(mode: bridge.EscapeActionMode, token = 0) {
    this.stopCardKeepAlive()
    this.setEscapeMode(mode, token)
  }

  /**
   * 失敗卡的標題與原因。**每一次 failure 狀態的下發都必須帶上它**（present 和 update
   * 都算），理由見 activeFailureCard 上的註釋。不要在呼叫點手寫這兩個欄位。
   */
  private failureCardPayload() {
    return {
      failureTitle: this.activeFailureCard?.title ?? '',
      failureDetail: this.activeFailureCard?.detail ?? '',
    }
  }

  private getMicSourceHintPayload() {
    return this.activeMicSourceHint
      ? {
        micSourceMode: this.activeMicSourceHint.mode,
        micSourceLabel: this.activeMicSourceHint.label,
      }
      : {
        micSourceMode: null,
        micSourceLabel: '',
      }
  }

  /**
   * 構造一次 listening 狀態更新。**所有 listening 出口都必須走這裡**，不要再手寫字面量。
   *
   * 原因：`streaming` 是原生側判斷懸浮窗佈局的唯一依據（window/mod.rs 的
   * apply_payload_layout）。任何一次 listening 更新漏帶它，視窗就會當場從流式尺寸
   * （480×200 CSS px）收縮回基礎尺寸，再被 33ms 後的下一幀心跳撐回去 —— 而 webview 裡的
   * DOM 完全沒變，於是氣泡被 overlay.html 的 overflow:hidden 裁掉一幀。使用者看到的就是
   * 「開著即時字幕、不說話時懸浮窗每 5 秒閃一下」（5s 是低音量警告的重彈節拍，見
   * RecorderOrchestrator.updateVolumeWarning 的 REWARN_MS）。
   *
   * 這個坑之所以值得抽一個方法：五個警告方法各自手寫了一份 payload，五份全漏了這個欄位，
   * 而漏掉它既不報錯也不進日誌（原生側的 resize 路徑一行日誌都不寫）。
   */
  private listeningPayload(overrides: Record<string, unknown> = {}) {
    return {
      state: 'listening',
      elapsedSec: clampSec(this.getElapsedSec()),
      ...(this.activeWarning ? { warning: this.activeWarning } : {}),
      ...(this.streamingActive ? { streaming: true } : {}),
      ...(this.streamingText ? { streamingText: this.streamingText } : {}),
      ...this.getMicSourceHintPayload(),
      ...this.getCommonPayload(),
      ...overrides,
    }
  }

  private clearMicSourceHint() {
    this.micSourceHintGeneration++
    if (this.micSourceHintHideId) {
      clearTimeout(this.micSourceHintHideId)
      this.micSourceHintHideId = null
    }
    this.activeMicSourceHint = null
  }

  /** Briefly confirm the actual input route after a successful microphone open. */
  showMicSourceHint(hint: MicSourceHint) {
    this.clearMicSourceHint()
    this.activeMicSourceHint = hint
    const generation = this.micSourceHintGeneration

    void bridge.updateOverlay(this.listeningPayload({ state: this.currentState }))

    this.micSourceHintHideId = setTimeout(() => {
      if (generation !== this.micSourceHintGeneration) return
      this.micSourceHintHideId = null
      this.activeMicSourceHint = null
      void bridge.updateOverlay(this.listeningPayload({ state: this.currentState }))
    }, MIC_SOURCE_HINT_DURATION_MS)
  }

  /** 文本即將進入不可逆的系統貼上階段；先關閉全域性 Esc 取消，避免“已取消”後仍完成貼上。 */
  async disableEscapeAction(): Promise<void> {
    // 同時解除卡片按鍵接管：貼上一旦開始就不該再有任何懸浮窗快捷鍵生效。
    this.stopCardKeepAlive()
    try {
      await bridge.setEscapeActionMode('off', 0)
    } catch { /* 原生鉤子不可用時不影響文本插入 */ }
  }

  showWaiting() {
    this.currentState = 'waiting'
    this.clearMicSourceHint()
    this.enterTransientState('off', 0)
    this.clearFallbackHideTimer()
    void bridge.presentOverlay({
      state: 'waiting',
      elapsedSec: 0,
      ...this.getMicSourceHintPayload(),
      ...this.getCommonPayload(),
    })
  }

  startListeningTicker(token = 0) {
    this.currentState = 'listening'
    this.stopListeningTicker()
    // 真實錄音代次才開啟全域性 Esc；PTT Lab 等 token=0 的預覽絕不吞系統按鍵。
    this.enterTransientState(token > 0 ? 'cancel_recording' : 'off', token)
    this.playReadySound()
    // 採集鏈路已經成功啟動時立刻釋出 listening；不要再為 ticker 的首個 33ms
    // 人為保留 waiting。PCM 隨後仍透過 pushListeningBars 驅動真實波形。
    this.pushListeningBars(undefined, true)
    // 有 PCM 時波形那一路（pushListeningBars）已經按 33ms 帶著計時和流式文本在發了，
    // 這裡只在很久沒人發過時補位（採集卡住、還沒收到第一幀）。原來兩路各自 33ms，
    // 合起來每秒約 60 次 IPC 和 60 次懸浮窗重渲染，畫面卻只需要 30 幀。
    //
    // 補位閾值必須明顯大於波形那一路的 33ms 節流：兩者共用 lastFrameAt，閾值若也是 33ms，
    // 這個 33ms 的定時器每輪都剛好過線、先把名額拿走，而它發的幀不帶 bars，
    // 波形幀全被節流丟掉（0.2.3 懸浮窗波形掉幀，issue #90）。
    // 補位只為推進秒錶（一秒才變一次），250ms 足夠；間隔同步放大，免得每秒空轉 30 次。
    this.tickerId = setInterval(() => {
      const now = Date.now()
      if (now - this.lastFrameAt < LISTENING_TICKER_IDLE_MS) return
      this.lastFrameAt = now
      void bridge.updateOverlay(this.listeningPayload())
    }, LISTENING_TICKER_IDLE_MS)
  }

  /** 設定本次錄音是否開啟流式即時顯示（錄音開始時呼叫）。開啟後氣泡從一開始就顯示佔位。 */
  setStreamingActive(on: boolean) {
    this.streamingActive = on
  }

  /** 更新流式即時識別文本。下一次 listening 心跳（33ms）會把它帶給懸浮窗一起渲染。
   *  傳空字串可清除氣泡。 */
  setStreamingText(text: string) {
    this.streamingText = text || ''
  }

  /** 清除流式即時文本與啟用標誌（錄音停止 / 重置時呼叫）。 */
  resetStreamingText() {
    this.streamingText = ''
    this.streamingActive = false
  }

  stopListeningTicker() {
    if (this.tickerId) {
      clearInterval(this.tickerId)
      this.tickerId = null
    }
  }

  pushListeningBars(bars?: number[], force = false) {
    const now = Date.now()
    if (!force && now - this.lastFrameAt < 33) return
    this.lastFrameAt = now
    void bridge.updateOverlay(this.listeningPayload({ bars }))
  }

  /**
   * @param options.ai 這次處理會走 AI 整理（按本次的 aiPolicy.allowCall，不按開關：
   *   開關開著但錄音短於門檻時並不整理，那時顯示「AI 整理中」就是假的）。
   */
  showThinking(elapsedSec: number, token = 0, options: { ai?: boolean } = {}) {
    this.currentState = 'thinking'
    // PTT Lab 也複用 thinking 動畫，但沒有真實錄音代次；token=0 時只顯示，絕不吞全域性 Esc。
    this.enterTransientState(token > 0 ? 'cancel_processing' : 'off', token)
    void bridge.updateOverlay({
      state: 'thinking',
      ...(options.ai && { thinkingAi: true }),
      elapsedSec: clampSec(elapsedSec),
      ...this.getMicSourceHintPayload(),
      ...this.getCommonPayload(),
    })
  }

  /**
   * 即將到達單次上限的提示。**一閃即走**，之後由計時區的倒計時持續告知剩餘時間。
   *
   * 以前這條是常駐到錄音結束的，加上 Overlay 裡"有警告就不顯示計時"的條件，
   * 導致最後一整分鐘只剩一句「單次記錄最長300s」，既不知道已錄多久、也不知道還剩多久。
   */
  showTimeoutWarning() {
    const text = t('overlay.warnMaxDuration', { limit: formatRecordingLimit() })
    this.activeWarning = text
    void bridge.updateOverlay(this.listeningPayload({ warning: text, warningTone: 'warn' }))

    if (this.timeoutWarningHideId) clearTimeout(this.timeoutWarningHideId)
    this.timeoutWarningHideId = setTimeout(() => {
      this.timeoutWarningHideId = null
      // 期間若被更高優先順序的警告（如麥克風被靜音）取代，就不要搶回來
      if (this.activeWarning !== text) return
      this.activeWarning = ''
      void bridge.updateOverlay(this.listeningPayload({ warning: '', warningTone: 'warn' }))
    }, 4000)
  }

  /** Show low volume warning on the overlay（有聲音但偏低：請靠近麥克風，琥珀色） */
  showLowVolumeWarning() {
    if (this.activeWarning) return
    void bridge.updateOverlay(this.listeningPayload({
      warning: t('overlay.warnLowVolume'),
      warningTone: 'warn',
    }))
  }

  /** Show "no signal detected" warning（持續無輸入意味著裝置不可用，使用紅色高警）。
   *  開啟的是虛擬音效卡時直接說出來：「未檢測到聲音」會把人引去查麥克風線和靜音鍵，
   *  而真正要做的是換裝置。 */
  showNoSignalWarning(options: { virtualMic?: boolean } = {}) {
    if (this.activeWarning) return
    void bridge.updateOverlay(this.listeningPayload({
      warning: t(options.virtualMic ? 'overlay.warnNoSignalVirtual' : 'overlay.warnNoSignal'),
      warningTone: 'error',
    }))
  }

  /** Show "mic is muted" high-alert（系統層面確認被靜音：紅色高警） */
  showMicMutedAlert() {
    if (this.activeWarning) return
    void bridge.updateOverlay(this.listeningPayload({
      warning: t('overlay.warnMicMuted'),
      warningTone: 'error',
    }))
  }

  /** Clear transient warnings (low volume etc.) — does NOT clear timeout warning */
  clearWarning() {
    if (this.activeWarning) return
    void bridge.updateOverlay(this.listeningPayload({ warning: '', warningTone: 'warn' }))
  }

  /**
   * 是否有常駐（sticky）警告正在擋住臨時警告的顯示/清除。
   * 排查"低音量提示不消失/不出現"這類問題時很有用：那類症狀多半就是被它擋著。
   */
  hasStickyWarning() {
    return !!this.activeWarning
  }

  /** Reset all warnings including persistent ones (called on recording stop/reset) */
  resetWarnings() {
    if (this.timeoutWarningHideId) {
      clearTimeout(this.timeoutWarningHideId)
      this.timeoutWarningHideId = null
    }
    this.activeWarning = ''
  }

  /**
   * 目標視窗收不下文本時的結果卡：顯示識別結果，使用者自己取走。
   *
   * **不自動消失**（2026-09 改）。原先 10 秒就隱藏，等於把"文本還沒交付"當成
   * "通知已讀"：使用者去夠滑鼠的時間裡卡片就沒了，文字也跟著沒了（那時剪貼簿裡
   * 也沒有它——不可編輯那條路刻意不提前改寫剪貼簿）。現在靠 Esc / 關閉按鈕 /
   * Ctrl+C 顯式收場，只有 CARD_MAX_VISIBLE_MS 作為最終安全網。
   */
  showFallback(text: string, reason: string, token = 0) {
    this.currentState = 'fallback'
    this.clearMicSourceHint()
    addRuntimeEvent('info', 'overlay', 'Showing result card', {
      reason,
      textLen: text.length,
      token,
    })
    void bridge.presentOverlay({
      state: 'fallback',
      fallbackText: text,
      fallbackReason: reason,
      cardToken: token,
      ...this.getCommonPayload(),
    })
    // token=0 的 PTT Lab 預覽沒有 Orchestrator 代次，不能開啟無人消費的全域性按鍵接管。
    this.beginCardLifecycle(token, 'dismiss_fallback', text ? ['copy'] : [], { autoHide: false })
  }

  /**
   * 識別失敗（而不是"沒插進去"）時的卡片。
   *
   * 與結果卡的區別：沒有文本可交付，只有一條原因和一個關閉按鈕，所以不註冊 Ctrl+C。
   * `recovery` 決定要不要告訴使用者"可以去歷史記錄重新識別" —— 它必須來自**實際存檔
   * 結果**，不能靠設定開關猜：歷史關著、錄音保留關著、寫盤失敗，任何一條都會讓
   * 那句話變成空頭承諾。拿不到結論時傳 'unknown'，介面就什麼都不承諾。
   */
  showFailure(params: {
    title: string
    detail?: string
    recovery: FailureRecovery
    token?: number
  }) {
    const token = params.token ?? 0
    this.currentState = 'failure'
    this.clearMicSourceHint()
    this.activeFailureCard = { title: params.title, detail: params.detail || '' }
    addRuntimeEvent('warn', 'overlay', 'Showing failure card', {
      title: params.title,
      recovery: params.recovery,
      token,
    })
    void bridge.presentOverlay({
      state: 'failure',
      ...this.failureCardPayload(),
      failureRecovery: params.recovery,
      cardToken: token,
      ...this.getCommonPayload(),
    })
    // 失敗卡里沒有需要交付給使用者的內容，只有一條原因和一個操作機會，所以允許自動收起。
    this.beginCardLifecycle(token, 'dismiss_fallback', [], { autoHide: true })
  }

  /**
   * 失敗卡已經顯示之後，音訊存檔才有結論 —— 就地把那句恢復提示補上。
   *
   * 為什麼要分兩步：提示必須在失敗的**那一刻**出現（使用者正盯著螢幕等結果），而
   * 存檔是非同步的，最長可能等 30 秒（AUDIO_ARCHIVE_WAIT_MS）。寧可先說"失敗了"、
   * 稍後再補"錄音留下了"，也不要為了湊齊一句完整的話讓使用者先乾等半分鐘。
   */
  updateFailureRecovery(recovery: FailureRecovery, token: number) {
    if (this.currentState !== 'failure') return
    if (token !== this.activeCardToken) return
    void bridge.updateOverlay({
      state: 'failure',
      // 標題和原因必須**每次都重發**，見 failureCardPayload 上的註釋。
      ...this.failureCardPayload(),
      failureRecovery: recovery,
      ...this.getCommonPayload(),
    })
  }

  /**
   * 處理已超時，但還在寬限期裡等遲到結果。
   *
   * 這一步以前是**完全靜默**的：懸浮窗直接消失，而 15 秒內遲到的 final 仍會自動
   * 插字。使用者看到的是"什麼都沒發生，然後文字忽然出現"。現在明確告知仍在等待，
   * 並且給 Esc 一個放棄的機會（放棄後遲到結果不再插字）。
   */
  showAwaitingLateResult(graceSec: number, token: number) {
    this.currentState = 'thinking'
    this.clearMicSourceHint()
    this.enterTransientState(token > 0 ? 'abandon_late_result' : 'off', token)
    void bridge.presentOverlay({
      state: 'thinking',
      thinkingNote: 'late',
      elapsedSec: clampSec(graceSec),
      ...this.getCommonPayload(),
    })
    this.clearFallbackHideTimer()
  }

  /**
   * 進入一張常駐卡片的生命週期：註冊按鍵接管，並在可見期間週期性續期。
   *
   * Rust 側那兩個 TTL 是異常兜底，不是正常生命週期（見 CARD_KEEPALIVE_INTERVAL_MS）。
   * 卡片既然不再自動消失，就必須由這裡持續證明"我還在"。
   */
  private beginCardLifecycle(
    token: number,
    escapeMode: bridge.EscapeActionMode,
    hotkeys: bridge.CardHotkeyAction[],
    options: { autoHide: boolean },
  ) {
    this.stopCardKeepAlive()
    this.activeCardToken = token
    this.activeCardHotkeys = token > 0 ? hotkeys : []
    this.activeCardEscapeMode = token > 0 ? escapeMode : 'off'

    this.setEscapeMode(this.activeCardEscapeMode, token)
    this.setCardHotkeys(this.activeCardHotkeys, token)

    if (token > 0) {
      this.cardKeepAliveId = setInterval(() => {
        this.setEscapeMode(this.activeCardEscapeMode, this.activeCardToken)
        this.setCardHotkeys(this.activeCardHotkeys, this.activeCardToken)
      }, CARD_KEEPALIVE_INTERVAL_MS)
    }

    this.clearFallbackHideTimer()
    if (options.autoHide) {
      this.fallbackHideId = setTimeout(() => {
        addRuntimeEvent('info', 'overlay', 'Card hit its visible time limit; hiding', {
          token,
          visibleMs: FAILURE_CARD_VISIBLE_MS,
        })
        this.hide()
      }, FAILURE_CARD_VISIBLE_MS)
    }
  }

  /**
   * 懸浮窗那邊自己收起了卡片（點關閉按鈕、或複製完成後自動收起）。
   *
   * 卡片的生命週期歸這裡管，懸浮窗只是渲染端。不收到這個通知的話，續期定時器會在
   * 8 秒後把 Esc / Ctrl+C 重新註冊一遍 —— 卡片早已不在，使用者的按鍵卻還被接管，
   * 而且原生側會重新抓一次前台視窗（那時可能已經是別的程式）。
   */
  noteCardDismissed(token: number) {
    if (token !== this.activeCardToken) {
      addRuntimeEvent('info', 'overlay', 'Ignored a dismissal for a card that is no longer active', {
        token,
        activeCardToken: this.activeCardToken,
      })
      return
    }
    this.currentState = 'toast'
    this.clearFallbackHideTimer()
    // 這裡**不**再調 hideOverlay：懸浮窗自己已經隱藏了，重複下發只會多一次 IPC。
    this.enterTransientState('off', 0)
  }

  private stopCardKeepAlive() {
    if (this.cardKeepAliveId) {
      clearInterval(this.cardKeepAliveId)
      this.cardKeepAliveId = null
    }
    if (this.activeCardHotkeys.length > 0) {
      this.setCardHotkeys([], 0)
    }
    this.activeCardToken = 0
    this.activeCardHotkeys = []
    this.activeCardEscapeMode = 'off'
  }

  private setCardHotkeys(actions: bridge.CardHotkeyAction[], token: number) {
    void bridge.setCardHotkeys(actions, token).catch(() => { /* 原生鉤子不可用時不影響卡片 */ })
  }

  clearFallbackHideTimer() {
    if (this.fallbackHideId) {
      clearTimeout(this.fallbackHideId)
      this.fallbackHideId = null
    }
  }

  hide() {
    this.clearMicSourceHint()
    // 卡片沒了，標題和原因就不該再被任何一次下發帶出去。
    this.activeFailureCard = null
    this.clearFallbackHideTimer()
    // enterTransientState 內部會解除卡片的按鍵接管並停掉續期定時器。
    this.enterTransientState('off', 0)
    void bridge.hideOverlay()
  }

  /** 快捷鍵切換潤色模式後，用懸浮窗短暫提示當前模式名，約 1.6s 後自動隱藏。 */
  showPresetSwitched(name: string) {
    this.currentState = 'toast'
    this.clearMicSourceHint()
    this.enterTransientState('off', 0)
    void bridge.presentOverlay({
      state: 'toast',
      toastText: t('overlay.toastPresetSwitched', { name }),
      toastTone: 'info',
      ...this.getCommonPayload(),
    })
    this.clearFallbackHideTimer()
    this.fallbackHideId = setTimeout(() => this.hide(), 1600)
  }

  /** 全域性快捷鍵切換 AI 整理後的輕量確認，不打斷正在錄音的狀態。 */
  showAiCleanupToggled(enabled: boolean) {
    this.currentState = 'toast'
    this.clearMicSourceHint()
    this.enterTransientState('off', 0)
    void bridge.presentOverlay({
      state: 'toast',
      toastText: t(enabled ? 'overlay.toastAiCleanupOn' : 'overlay.toastAiCleanupOff'),
      toastTone: 'info',
      ...this.getCommonPayload(),
    })
    this.clearFallbackHideTimer()
    this.fallbackHideId = setTimeout(() => this.hide(), 1400)
  }

  /**
   * 識別結果為空（未檢測到有效聲音）時的提示。
   *
   * `diagnostic` 由呼叫方補充成因上下文。日誌刻意放在這裡、而不是各個呼叫點：
   * 這句提示是全軟體最容易被誤讀的一條 —— 真實成因可能是使用者沒說話、供應商額度
   * 耗盡、服務端提前斷開、文本後處理失敗，使用者看到的卻是同一句話。放在這個唯一
   * 出口，就保證「使用者看到這句」和「日誌裡有記錄」永遠一一對應，不會因為將來
   * 新增一個呼叫點而漏掉。
   */
  /**
   * 識別沒出文字時的短提示。兩種成因共用這一條路，**都不彈失敗卡**。
   *
   * - `'silent'`：採集側有近靜音實證，確實沒人說話。
   * - `'no_text'`：錄到了聲音，但識別沒返回任何文字。
   *
   * 為什麼 `'no_text'` 也只給 toast：原先拿採集側峰值（> 0.01）區分「沒說話」和
   * 「呼叫失敗」，這個判據兩頭都穿。實測日誌裡連著三次都是什麼都沒說，一次彈了失敗卡
   * 兩次只彈 toast，差別僅僅是底噪峰值（0.019 / 0.0001 / 0.0015）—— 一次呼吸、碰一下
   * 桌子就能超過 0.01；反過來額度耗盡同樣會返回空文本、峰值卻很低。用它當判據只會
   * 讓同一個動作時而彈卡片時而不彈。
   *
   * 真正的故障都有各自的路徑（連線斷開 / stop 發不出去 / 處理超時 / 後處理把文字清空），
   * 失敗卡留給那幾條。
   */
  showNoSpeech(reason: 'silent' | 'no_text', diagnostic?: Record<string, unknown>) {
    this.currentState = 'toast'
    this.clearMicSourceHint()
    addRuntimeEvent('warn', 'recorder', 'Showing no-speech warning', { ...(diagnostic ?? {}), reason })
    this.enterTransientState('off', 0)
    void bridge.presentOverlay({
      state: 'toast',
      // 'no_text' 不說「未檢測到有效聲音」：聲音是收到了，那句話會把使用者直接引去查麥克風。
      toastText: t(reason === 'silent' ? 'overlay.toastNoSpeech' : 'overlay.toastNoText'),
      toastTone: 'warn',
      ...this.getCommonPayload(),
    })
    this.clearFallbackHideTimer()
    this.fallbackHideId = setTimeout(() => this.hide(), 1500)
  }

  /** 使用者主動取消處理後的短提示。 */
  showCanceled() {
    this.currentState = 'toast'
    this.clearMicSourceHint()
    this.enterTransientState('off', 0)
    void bridge.presentOverlay({
      state: 'toast',
      toastText: t('overlay.toastCanceled'),
      toastTone: 'info',
      ...this.getCommonPayload(),
    })
    this.clearFallbackHideTimer()
    this.fallbackHideId = setTimeout(() => this.hide(), 900)
  }

  /** 顯示錯誤資訊，幾秒後自動隱藏 */
  showError(message: string) {
    this.currentState = 'error'
    this.clearMicSourceHint()
    this.enterTransientState('off', 0)
    void bridge.presentOverlay({
      state: 'error',
      errorMessage: message,
      ...this.getCommonPayload(),
    })
    this.clearFallbackHideTimer()
    this.fallbackHideId = setTimeout(() => this.hide(), 4000)
  }

  dispose() {
    this.stopListeningTicker()
    this.resetStreamingText()
    this.hide()
  }
}
