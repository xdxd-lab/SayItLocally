import * as bridge from '../services/bridge'
import { useEffect, useMemo, useRef, useState, type CSSProperties } from 'react'
import { AlertCircle, Copy, MicVocal, Sparkles, X } from 'lucide-react'
import { SuccessCheck } from '@/components/ui/success-check'
import { isLocale, setLocale } from '@/i18n'
import { useT } from '@/i18n/useT'
import { addRuntimeEvent } from '../services/debugLog'
import { formatRecordingTimer } from '../services/recorder/types'
import { IS_MAC } from '@/lib/platform'
import {
  buildWavePalette,
  DEFAULT_CUSTOM_WAVE,
  getWaveThinkingColor,
  getWaveTimerColor,
  normalizeCustomWave,
  normalizeWaveTheme,
  sameCustomWave,
  type CustomWaveColors,
  type OverlayWaveTheme,
} from '@/lib/overlayWaveColors'
import { prefersReducedMotion, usePillMorph } from './pillMorph'

// [自訂] L-80：焦點核對拒絕的結果卡要說明文字未送出。
function fallbackHintKey(reason: string) {
  switch (reason) {
    case 'insertion_timeout': return 'overlay.insertionTimeoutHint'
    case 'application_changed':
    case 'window_changed':
    case 'element_changed': return 'overlay.focusChangedHintMac'
    case 'target_expired': return 'overlay.focusExpiredHintMac'
    default: return IS_MAC ? 'overlay.fallbackHintMac' : 'overlay.fallbackHint'
  }
}

/**
 * `blank` 什麼都不渲染，只在**隱藏視窗之前**用一下。
 *
 * 為什麼需要它：隱藏懸浮窗只是 `overlay.hide()`，WebView 不銷燬、這個元件也不解除安裝，
 * 所以合成器裡留著的最後一幀就是上一條提示。下次 present 時原生視窗先顯示出來、
 * 新內容要等 IPC + setState + 重繪才到，那一瞬間使用者看到的是**上一次的文案**
 * （使用者報的就是這個：切換潤色模式後再按開關 AI 整理，懸浮窗先閃一下舊提示）。
 *
 * 於是隱藏時先發一幀 blank —— 那一刻視窗還看得見，重繪是確定會發生的（視窗一旦隱藏
 * 就沒這個保證了，rAF 會被節流）。留在合成器裡的最後一幀因此是空的，下次顯示無從可閃。
 * 視覺上沒有代價：blank 連膠囊底色都不畫，看起來就是"提示消失了"，本來隱藏就是這個樣子。
 */
type OverlayState =
  | 'blank'
  | 'waiting'
  | 'listening'
  | 'thinking'
  | 'fallback'
  | 'failure'
  | 'error'
  | 'toast'
/** 失敗卡上那句恢復提示：只有確實存下來了才敢說"可以重新識別"。 */
type FailureRecovery = 'unknown' | 'history' | 'none'
type RecordingVisualPhase = 'preparing' | 'listening'
interface OverlayPayload {
  state?: OverlayState
  bars?: number[]
  elapsedSec?: number
  theme?: OverlayWaveTheme
  customWave?: CustomWaveColors
  showDuration?: boolean
  barCount?: number
  fallbackText?: string
  fallbackReason?: string
  failureTitle?: string
  failureDetail?: string
  failureRecovery?: FailureRecovery
  /** 這張卡片對應的錄音代次。按鍵事件與關閉回報都要帶上它做代次校驗。 */
  cardToken?: number
  /** thinking 的變體：'late' = 已超時、仍在寬限期裡等遲到結果。 */
  thinkingNote?: 'late'
  /** thinking 時這次會走 AI 整理：文字換成「AI 整理中」並帶星芒圖示。 */
  thinkingAi?: boolean
  errorMessage?: string
  warning?: string
  /** warning 的嚴重級別：warn=琥珀（聲音小/未檢測到），error=紅色高警（麥克風已被靜音） */
  warningTone?: 'warn' | 'error'
  toastText?: string
  /** toast 的語氣：info=中性（如切換預設），warn=琥珀色+圖示（如未檢測到聲音） */
  toastTone?: 'info' | 'warn'
  streaming?: boolean
  streamingText?: string
  micSourceMode?: 'auto' | 'fixed' | null
  micSourceLabel?: string
  /** 介面語言，由主窗隨每次更新下發（見 OverlayCommonPayload.locale）。 */
  locale?: string
  _overlayShowId?: number
  _overlayGeneration?: number
  _overlayProbe?: boolean
}

const DEFAULT_BAR_COUNT = 24
const IDLE_BARS = Array(DEFAULT_BAR_COUNT).fill(3)


export default function Overlay() {
  const t = useT()
  // 初始 blank 而不是 waiting：這個頁面會被**預熱**（提前建好 WebView 並保持隱藏），
  // 首個狀態事件到達之前它就已經畫過一幀了。預設 waiting 的話那一幀是"準備中"膠囊，
  // 於是第一次真正顯示懸浮窗時會先閃一下它 —— 和隱藏後殘留舊提示是同一個毛病。
  // 沒有狀態就什麼都不畫，才是誠實的。
  const [state, setState] = useState<OverlayState>('blank')
  const [recordingVisualPhase, setRecordingVisualPhase] = useState<RecordingVisualPhase>('preparing')
  const [bars, setBars] = useState<number[]>(IDLE_BARS)
  const [elapsedSec, setElapsedSec] = useState(0)
  const [theme, setTheme] = useState<OverlayWaveTheme>('black-blue')
  const [customWave, setCustomWave] = useState<CustomWaveColors>(DEFAULT_CUSTOM_WAVE)
  const [showDuration, setShowDuration] = useState(true)
  const [barCount, setBarCount] = useState(DEFAULT_BAR_COUNT)
  const [presentationId, setPresentationId] = useState(0)
  const [fallbackText, setFallbackText] = useState('')
  const [failureTitle, setFailureTitle] = useState('')
  const [failureDetail, setFailureDetail] = useState('')
  const [failureRecovery, setFailureRecovery] = useState<FailureRecovery>('unknown')
  const [cardToken, setCardToken] = useState(0)
  const [fallbackReason, setFallbackReason] = useState('')
  const [thinkingNote, setThinkingNote] = useState<'late' | null>(null)
  const [thinkingAi, setThinkingAi] = useState(false)
  const [copyError, setCopyError] = useState(false)
  const [errorMessage, setErrorMessage] = useState('')
  const [toastText, setToastText] = useState('')
  const [toastTone, setToastTone] = useState<'info' | 'warn'>('info')
  const [streamingText, setStreamingText] = useState('')
  const [streamingOn, setStreamingOn] = useState(false)
  const [copied, setCopied] = useState(false)
  const [thinkingDuration, setThinkingDuration] = useState(0)
  const [warning, setWarning] = useState('')
  const [warningTone, setWarningTone] = useState<'warn' | 'error'>('warn')
  const [micSourceMode, setMicSourceMode] = useState<'auto' | 'fixed' | null>(null)
  const [micSourceLabel, setMicSourceLabel] = useState('')
  const rootRef = useRef<HTMLDivElement | null>(null)
  const pillRef = useRef<HTMLDivElement | null>(null)
  const hideTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  /**
   * 給 Ctrl+C 用的當前卡片快照。
   *
   * 原生鉤子發來的 card-hotkey 事件在 effect 裡處理，而那個 effect 只跑一次（[]），
   * 拿不到後續的 state。用 ref 映象一份，同時帶上 presentationId —— 舊卡片的事件
   * （來自上一次顯示）必須丟掉，不能拿新卡片的文本去響應。
   */
  const cardRef = useRef<{
    presentationId: number
    state: OverlayState
    text: string
    token: number
  }>({
    presentationId: 0,
    state: 'blank',
    text: '',
    token: 0,
  })
  const preparingPaintFrameRef = useRef<number | null>(null)
  const pendingListeningVisualRef = useRef(false)
  const elapsedSecRef = useRef(0)

  // 根據錄音時長計算預估處理時間（秒）
  const calculateThinkingDuration = (recordingSec: number): number => {
    if (recordingSec <= 5) return 2
    if (recordingSec <= 15) return 3
    if (recordingSec <= 30) return 4
    if (recordingSec <= 60) return 5
    if (recordingSec <= 120) return 7
    if (recordingSec <= 180) return 9
    if (recordingSec <= 240) return 11
    return 13 // 240秒以上（4-5分钟）
  }

  useEffect(() => {
    let disposed = false
    let removeOverlayListener: (() => void) | null = null

    const cancelPreparingPaint = () => {
      if (preparingPaintFrameRef.current !== null) {
        cancelAnimationFrame(preparingPaintFrameRef.current)
        preparingPaintFrameRef.current = null
      }
    }

    const beginPreparingVisual = () => {
      cancelPreparingPaint()
      pendingListeningVisualRef.current = false
      setRecordingVisualPhase('preparing')
      // listening 可能在 WebView 的首幀之前就到達。跨兩個 rAF 才放行視覺切換，
      // 保證準備膠囊至少真正繪製一幀；只延後視覺形變，不延後錄音採集。
      preparingPaintFrameRef.current = requestAnimationFrame(() => {
        preparingPaintFrameRef.current = requestAnimationFrame(() => {
          preparingPaintFrameRef.current = null
          if (disposed || !pendingListeningVisualRef.current) return
          pendingListeningVisualRef.current = false
          setRecordingVisualPhase('listening')
        })
      })
    }

    const handleOverlayState = (data: unknown) => {
      const payload = data as OverlayPayload
      if (typeof payload._overlayShowId === 'number') setPresentationId(payload._overlayShowId)
      // 語言先落地再渲染本幀：懸浮窗沒有自己的初始化時機，語言只能隨 payload 來。
      // setLocale 對同值是空操作，所以每幀都調也不會造成額外重渲染。
      if (isLocale(payload.locale)) setLocale(payload.locale)
      const nextElapsedSec = typeof payload.elapsedSec === 'number'
        ? payload.elapsedSec
        : elapsedSecRef.current

      if (payload.state) {
        if (payload.state === 'waiting') {
          beginPreparingVisual()
        } else if (payload.state === 'listening') {
          if (preparingPaintFrameRef.current !== null) {
            pendingListeningVisualRef.current = true
          } else {
            setRecordingVisualPhase('listening')
          }
        } else {
          cancelPreparingPaint()
          pendingListeningVisualRef.current = false
        }
        setState(payload.state)
        if (payload.state !== 'listening') {
          setBars((prev) => Array(prev.length).fill(3))
          // 離開錄音狀態即清空即時文字氣泡
          setStreamingText('')
          setStreamingOn(false)
        }
        setCopied(false)
        setCopyError(false)
        if (payload.state !== 'fallback' && hideTimerRef.current) {
          clearTimeout(hideTimerRef.current)
          hideTimerRef.current = null
        }
        if (payload.state === 'thinking') {
          setThinkingDuration(calculateThinkingDuration(nextElapsedSec))
          setThinkingNote(payload.thinkingNote ?? null)
          setThinkingAi(payload.thinkingAi === true)
        } else {
          setThinkingNote(null)
          setThinkingAi(false)
        }
        if (payload.state !== 'failure') {
          // 離開失敗態就把恢復結論清回 unknown，否則下一張卡片會先閃一下上一次的
          // "可在歷史記錄重新識別"——那句話在新的失敗裡可能根本不成立。
          setFailureRecovery('unknown')
        }
      }

      if (Array.isArray(payload.bars) && payload.bars.length > 0) setBars(payload.bars)
      if (typeof payload.elapsedSec === 'number') {
        elapsedSecRef.current = payload.elapsedSec
        setElapsedSec(payload.elapsedSec)
      }
      if (typeof payload.showDuration === 'boolean') setShowDuration(payload.showDuration)
      if (payload.theme) setTheme(normalizeWaveTheme(payload.theme, 'black-blue'))
      if (payload.customWave) {
        // 每幀 payload 都會帶一個新物件；內容沒變就保留舊引用，否則下面按配色 memo 的
        // 調色盤每幀都得重算一遍。
        const next = normalizeCustomWave(payload.customWave)
        setCustomWave((prev) => (sameCustomWave(prev, next) ? prev : next))
      }
      if (typeof payload.barCount === 'number' && payload.barCount > 0) setBarCount(payload.barCount)
      if (typeof payload.fallbackText === 'string') setFallbackText(payload.fallbackText)
      if (typeof payload.fallbackReason === 'string') setFallbackReason(payload.fallbackReason)
      if (typeof payload.cardToken === 'number') setCardToken(payload.cardToken)
      if (typeof payload.failureTitle === 'string') setFailureTitle(payload.failureTitle)
      if (typeof payload.failureDetail === 'string') setFailureDetail(payload.failureDetail)
      if (
        payload.failureRecovery === 'unknown'
        || payload.failureRecovery === 'history'
        || payload.failureRecovery === 'none'
      ) {
        setFailureRecovery(payload.failureRecovery)
      }
      if (typeof payload.errorMessage === 'string') setErrorMessage(payload.errorMessage)
      if (typeof payload.toastText === 'string') setToastText(payload.toastText)
      if (payload.toastTone === 'info' || payload.toastTone === 'warn') setToastTone(payload.toastTone)
      if (typeof payload.warning === 'string') setWarning(payload.warning)
      if (payload.warningTone === 'warn' || payload.warningTone === 'error') setWarningTone(payload.warningTone)
      if (typeof payload.streamingText === 'string') setStreamingText(payload.streamingText)
      if (typeof payload.streaming === 'boolean') setStreamingOn(payload.streaming)
      if (payload.micSourceMode === 'auto' || payload.micSourceMode === 'fixed' || payload.micSourceMode === null) {
        setMicSourceMode(payload.micSourceMode)
      }
      if (typeof payload.micSourceLabel === 'string') setMicSourceLabel(payload.micSourceLabel)
      if (payload.state === 'waiting') {
        setElapsedSec(0)
        setWarning('')
        setWarningTone('warn')
        setStreamingText('')
        setStreamingOn(false)
        setMicSourceMode(null)
        setMicSourceLabel('')
        setBars((prev) => Array(prev.length).fill(3))
      }

      if (!payload._overlayProbe) return
      const showId = payload._overlayShowId
      const generation = payload._overlayGeneration
      if (typeof showId !== 'number' || typeof generation !== 'number') return

      requestAnimationFrame(() => {
        requestAnimationFrame(() => {
          if (disposed) return
          const root = rootRef.current
          const content = root?.querySelector<HTMLElement>('[data-overlay-content]') ?? null
          const rootRect = root?.getBoundingClientRect()
          const contentRect = content?.getBoundingClientRect()
          const style = content ? window.getComputedStyle(content) : null
          // 內容頂部越到視口上方多少 CSS px。overlay.html 給 html/body/#root 設了
          // overflow:hidden，所以這一段是**真的被裁掉**，不是滾出去了還能拉回來。
          // healthy 不看它（懸浮窗被裁一條仍然在工作），但必須上報：否則被裁成什麼樣
          // 日志里都只有一行 render ack OK。
          const clippedTop = contentRect ? Math.max(0, -contentRect.top) : 0
          const healthy = Boolean(
            rootRect && contentRect
            && rootRect.width > 0 && rootRect.height > 0
            && contentRect.width > 0 && contentRect.height > 0
            && style?.display !== 'none'
            && style?.visibility !== 'hidden'
            && Number(style?.opacity ?? '1') > 0
          )
          void bridge.overlayRenderAck({
            showId,
            generation,
            healthy,
            overlayState: payload.state ?? 'unknown',
            documentVisibility: document.visibilityState,
            rootWidth: rootRect?.width ?? 0,
            rootHeight: rootRect?.height ?? 0,
            contentWidth: contentRect?.width ?? 0,
            contentHeight: contentRect?.height ?? 0,
            clippedTop,
            // 原生側按「邏輯畫素 × 顯示器 scale」定視窗大小，但 webview 的 1 CSS px 可能是
            // `顯示器 scale × 額外縮放` 個裝置畫素（Windows 的文本大小設定會被 WebView2
            // 當頁面縮放疊上來）。把 dpr 和真實視口報上去，原生側才能把視窗開夠。
            devicePixelRatio: window.devicePixelRatio,
            viewportWidth: window.innerWidth,
            viewportHeight: window.innerHeight,
            display: style?.display ?? 'missing',
            visibility: style?.visibility ?? 'missing',
            opacity: style?.opacity ?? 'missing',
          }).catch(() => { })
        })
      })
    }

    void bridge.listen<unknown>('overlay-state', (event) => handleOverlayState(event.payload))
      .then((unlisten) => {
        if (disposed) {
          unlisten()
          return
        }
        removeOverlayListener = unlisten
        void bridge.overlayReady(window.devicePixelRatio).catch(() => { })
      })

    return () => {
      disposed = true
      cancelPreparingPaint()
      pendingListeningVisualRef.current = false
      removeOverlayListener?.()
      if (hideTimerRef.current) {
        clearTimeout(hideTimerRef.current)
        hideTimerRef.current = null
      }
    }
  }, [])

  // 讓只跑一次的 card-hotkey 監聽能看到最新的卡片內容與代次。
  const copyHandlerRef = useRef<(source: 'button' | 'hotkey') => void>(() => { })
  copyHandlerRef.current = (source) => { void handleCopyFallback(source) }
  cardRef.current = { presentationId, state, text: fallbackText, token: cardToken }

  useEffect(() => {
    // Ctrl+C 只能從原生鉤子來：懸浮窗不搶焦點，按鍵始終發給使用者原來的程式，
    // 這個 webview 裡的 keydown 永遠不會觸發（與 Esc 同一個原因）。
    const removeCardHotkey = bridge.onCardHotkey(({ action, token }) => {
      if (action !== 'copy') return
      const card = cardRef.current
      // 代次校驗必須在這個入口就做：上一張卡片的按鍵事件遲到時，只要新卡也是
      // fallback，不校驗就會把**新卡的內容**複製出去。handleCopyFallback 裡的
      // presentationId 只保護"複製開始之後"，補不上這一刀。
      if (card.state !== 'fallback' || !card.text || token !== card.token || token === 0) {
        addRuntimeEvent('info', 'overlay', 'Ignored a card hotkey that does not match the current card', {
          action,
          eventToken: token,
          cardToken: card.token,
          state: card.state,
        })
        return
      }
      copyHandlerRef.current('hotkey')
    })
    return removeCardHotkey
  }, [])

  const recordingPhase = state === 'waiting' || state === 'listening'
  // 膠囊"換了一種內容"的標識。waiting/listening 共用一個：錄音中的逐幀重新整理不算換內容，
  // 兩者之間的展開另有 clip-path 負責。提示類把文案也算進去，連著兩條不同的提示同樣要變形。
  // 錄音中出現/撤掉警告、進入倒計時都會讓波形讓出條數（見下面的 barBudget），寬度跟著變，
  // 所以它們算進寬度的 key；但錄音內容本身不重掛，contentKey 仍是同一個。
  // 「處理中」→「仍在等待結果」同理：寬度要彈，內容不能重掛（進度條會從零重來）。
  const pillContentKey = recordingPhase ? 'recording' : state === 'thinking' ? 'thinking' : null
  const pillMorphKey = recordingPhase
    ? `recording:${warningTone}:${warning}:${formatRecordingTimer(elapsedSec).countdown ? 1 : 0}`
    : state === 'thinking'
      ? `thinking:${thinkingNote ?? ''}:${thinkingAi ? 'ai' : ''}`
      : state === 'toast'
        ? `toast:${toastTone}:${toastText}`
        : state === 'error'
          ? `error:${errorMessage}`
          : state
  const { animateContent: animatePillContent, enteredFrom: pillEnteredFrom } = usePillMorph(
    pillRef,
    pillMorphKey,
    presentationId,
    pillContentKey ?? pillMorphKey,
  )
  const visuallyListening = state === 'listening' && recordingVisualPhase === 'listening'
  const showStreamingBubble = visuallyListening && (streamingOn || streamingText.trim().length > 0)
  const hasStreamingText = streamingText.trim().length > 0
  const showMicSourceHint = Boolean(
    micSourceMode
    && micSourceLabel.trim()
    && (visuallyListening || state === 'thinking'),
  )

  const { text: timerText, countdown: inCountdown, remainingSec } = useMemo(
    () => formatRecordingTimer(elapsedSec),
    [elapsedSec],
  )
  // 越接近上限越醒目，但不用刺眼的純紅：琥珀 → 暖橙，最後 10 秒再疊一層脈動。
  // 懸浮窗是黑底小尺寸，純紅（#f87171）在這上面又豔又跳，和整體配色不搭。
  const urgent = inCountdown && remainingSec <= 10
  // 膠囊寬度按波形條數固定，文字擠進來時按需讓出條數：
  //   · 同時有警告文字 + 倒計時 → 只畫一半（最少 4 根，太少就不像波形了）
  //   · 只有其中一种 → 画三分之二
  // 這樣波形始終是完整的整根條，不會被裁成半截。
  const normalizedBars = barCount === bars.length
    ? bars
    : Array.from({ length: barCount }, (_, index) => bars[index] ?? 3)
  const barBudget = warning && inCountdown
    ? Math.max(4, Math.floor(normalizedBars.length / 2))
    : (warning || inCountdown
      ? Math.max(6, Math.floor((normalizedBars.length * 2) / 3))
      : normalizedBars.length)
  const visibleBars = barBudget >= normalizedBars.length
    ? normalizedBars
    : normalizedBars.slice(0, barBudget)

  // 錄音時每秒重渲染約 30 次，配色只在主題/條數變化時才變，沒必要每幀重算（自定義色要做 OKLCH 換算）。
  const palette = useMemo(
    () => buildWavePalette(normalizedBars.length, theme, customWave),
    [normalizedBars.length, theme, customWave],
  )
  const themeTimerColor = useMemo(() => getWaveTimerColor(theme, customWave), [theme, customWave])
  const thinkingColor = useMemo(() => getWaveThinkingColor(theme, customWave), [theme, customWave])
  const timerColor = inCountdown
    ? (urgent ? '#fb923c' : '#fbbf24')
    : themeTimerColor

  // 「波形收成進度條」要用錄音結束前的最後一幀波形。離開錄音的那一刻 bars 已經被清成 3，
  // 所以只能在錄音中逐幀記下來；靜音高警時畫的是紅字不是波形，那就沒有可收的東西。
  const waveSnapshotRef = useRef<{ heights: number[]; colors: string[] } | null>(null)
  if (visuallyListening) {
    waveSnapshotRef.current = warning && warningTone === 'error'
      ? null
      : { heights: visibleBars, colors: palette }
  } else if (state === 'waiting') {
    // 新一次錄音開始就清掉。否則按一下就松（還沒畫出波形）時，收進進度條的會是上一次的波形。
    waveSnapshotRef.current = null
  }
  const waveSnapshot = waveSnapshotRef.current
  // 減少動態效果時不收：全域性規則會把動畫壓成 0.01ms，但處理中內容的 170ms 延遲還在，
  // 等於白白讓文字晚出來。
  const showWaveCollapse = state === 'thinking' && pillEnteredFrom === 'recording' && waveSnapshot !== null
    && waveSnapshot.heights.length > 0 && !prefersReducedMotion()
  // 進度條寬 48px（w-12）。波形行寬 = 條寬 2.5px × n + 間距 2px × (n - 1)。
  const waveCollapseScale = waveSnapshot && waveSnapshot.heights.length > 0
    ? 48 / (waveSnapshot.heights.length * 2.5 + (waveSnapshot.heights.length - 1) * 2)
    : 1

  const handleCopyFallback = async (source: 'button' | 'hotkey' = 'button') => {
    if (!fallbackText) return
    // 複製期間可能開始新一次錄音（present 會換掉 presentationId）。舊回撥絕不能
    // 關掉新懸浮窗 —— 那是"複製完上一張卡，正在錄的這一次提示突然消失"的成因。
    const cardAtStart = presentationId

    try {
      await bridge.copyText(fallbackText)
      if (cardAtStart !== cardRef.current.presentationId) {
        addRuntimeEvent('info', 'overlay', 'Ignored a copy result from a superseded card', {
          cardAtStart,
          currentCard: cardRef.current.presentationId,
        })
        return
      }
      setCopied(true)
      setCopyError(false)
      addRuntimeEvent('info', 'overlay', 'Result card copied', {
        textLen: fallbackText.length,
        source,
      })

      if (hideTimerRef.current) {
        clearTimeout(hideTimerRef.current)
      }
      hideTimerRef.current = setTimeout(() => {
        if (cardAtStart !== cardRef.current.presentationId) return
        dismissCard('copied')
        hideTimerRef.current = null
      }, 500)
    } catch (error) {
      // 複製失敗**保留卡片**：文本此刻既不在輸入框裡、也不在剪貼簿裡，把卡片關掉
      // 等於把它徹底丟掉。剪貼簿被安全軟體佔用是真實場景（見 pitfalls 16）。
      addRuntimeEvent('error', 'overlay', 'Result card copy failed', {
        error: String(error),
        source,
      })
      if (cardAtStart !== cardRef.current.presentationId) return
      setCopied(false)
      setCopyError(true)
    }
  }

  /**
   * 收起當前卡片。
   *
   * 三件事必須一起做，缺一件都留下真實故障：
   *  1. 解除原生按鍵接管（否則使用者的 Esc / Ctrl+C 繼續被吞）；
   *  2. 隱藏視窗；
   *  3. **回報主窗**。卡片的生命週期歸主窗的 OverlayService 管，它持有續期定時器；
   *     不回報的話 8 秒後它又把按鍵註冊回來，而且原生側會重新抓當前前台視窗。
   */
  const dismissCard = (reason: string) => {
    const token = cardRef.current.token
    addRuntimeEvent('info', 'overlay', 'Card dismissed', { reason, state: cardRef.current.state, token })
    void bridge.setEscapeActionMode('off')
    void bridge.setCardHotkeys([])
    void bridge.hideOverlay()
    void bridge.notifyCardDismissed(reason, token).catch(() => { /* 主窗不在也不該卡住關閉 */ })
  }

  const handleDismissCard = () => { dismissCard('user_closed') }

  return (
    <div
      ref={rootRef}
      className="pointer-events-none flex h-full items-end justify-center pb-4"
    >
      {/* blank：一個子節點都不要（連膠囊底色都不畫）。見 OverlayState 上的註釋 ——
          它的整个用途就是让"隐藏前留在合成器里的那一帧"是空的。
          ⚠️ 别顺手给它补个占位元素，那就白做了。 */}
      {state === 'blank' ? null : state === 'fallback' ? (
        <div
          data-overlay-content
          className="pointer-events-auto flex w-full max-w-[520px] flex-col rounded-xl border px-4 py-4"
          style={{
            background: 'var(--overlay-bg)',
            color: 'var(--overlay-text)',
            borderColor: 'var(--overlay-border)',
          }}
        >
          <div className="flex items-start justify-between gap-3">
            <div className="space-y-1">
              <span className="block text-xs font-medium tracking-[0.16em]" style={{ color: 'var(--overlay-text-muted)' }}>{t('overlay.recognizedText')}</span>
              {/* 卡片必須說清它為什麼在這裡。reason 以前只進日誌，介面一律顯示
                  「当前目标不支持直接写入」—— 而插入卡死那条路上，文字**可能已经
                  写进去了**，那句话是错的，会让用户再粘一遍。 */}
              <span className="block text-xs" style={{ color: copyError ? '#fca5a5' : 'var(--overlay-text-dim)' }}>
                {copyError
                  ? t('overlay.copyFailedHint')
                  : t(fallbackHintKey(fallbackReason))}
              </span>
            </div>
            <div className="flex shrink-0 items-center gap-2">
              <button
                type="button"
                onClick={() => { void handleCopyFallback('button') }}
                // 懸浮窗的 bundle 裡沒有 Tooltip 元件，用原生 title 帶出快捷鍵。
                title={copied ? t('overlay.copied') : t(IS_MAC ? 'overlay.copyTextWithHotkeyMac' : 'overlay.copyTextWithHotkey')}
                aria-keyshortcuts={IS_MAC ? 'Meta+C' : 'Control+C'}
                className={`inline-flex h-8 w-8 items-center justify-center rounded-lg border transition-colors ${copied
                  ? 'border-emerald-400/40 bg-emerald-500/15 text-emerald-200'
                  : 'border-white/10 bg-white/10 text-white/90 hover:bg-white/20'
                  }`}
              >
                {copied ? <SuccessCheck className="h-4 w-4" /> : <Copy className="h-4 w-4" />}
              </button>
              <button
                type="button"
                onClick={handleDismissCard}
                title={t('window.close')}
                aria-label={t('overlay.dismissAria')}
                className="inline-flex h-8 w-8 items-center justify-center rounded-lg border border-white/10 bg-white/5 text-white/60 transition-colors hover:bg-white/15 hover:text-white"
              >
                <X className="h-4 w-4" />
              </button>
            </div>
          </div>
          <div className="mt-4 flex-1 overflow-hidden rounded-lg px-3 py-3" style={{ background: 'var(--overlay-surface)' }}>
            <p className="max-h-[108px] overflow-auto pr-1 text-sm leading-6 select-text">
              {fallbackText || t('overlay.noText')}
            </p>
          </div>
        </div>
      ) : state === 'failure' ? (
        /* 識別失敗卡片。沒有文本可交付，所以只有關閉按鈕，也不註冊 Ctrl+C。
         *
         * 兩列網格：第 1 列只放圖示，三段文字全部落在第 2 列。**別改回「圖示 + 文字」
         * 巢狀 flex** —— 那份實現裡恢復提示是卡片根節點的直接子元素，左邊緣對齊的是
         * 卡片內邊距，比標題和原因少縮排整整一個圖示寬（24px），看著像掉出去了。
         * 對齊交給網格線，不要靠手寫 padding 去湊。
         *
         * 關閉按鈕刻意脫離網格流（absolute）：它 32px 高，留在流裡會把第一行整體撐到
         * 32px，於是"標題↔原因 4px"這個緊湊間距實際變成 16px，分組節奏就沒了。
         *
         * 尺寸與 window/mod.rs 的 OVERLAY_FAILURE_WIDTH / _HEIGHT 是一對，改一邊就要改另一邊：
         * 下面的 max-w 比視窗窄時，多出來的透明邊照樣吞掉下面程式的點選（這是 is_interactive
         * 佈局）；比視窗寬則被 overflow:hidden 裁掉。高度上限另見那條佈局自檢單測。 */
        <div
          data-overlay-content
          className="pointer-events-auto relative grid w-full max-w-[480px] grid-cols-[auto_1fr] items-start gap-x-2 rounded-xl border px-4 py-4"
          style={{
            background: 'var(--overlay-bg)',
            color: 'var(--overlay-text)',
            borderColor: 'var(--overlay-border)',
          }}
        >
          {/* mt-0.5 是光學對齊，不是隨手加的：圖示 16px、標題行高 20px，(20-16)/2 = 2px。 */}
          <AlertCircle className="mt-0.5 h-4 w-4 shrink-0 text-red-400" aria-hidden />
          {/* pr-9 給絕對定位的關閉按鈕讓位（32px 按鈕 + 4px 間隙），否則長標題會壓到它下面。 */}
          <span className="min-w-0 pr-9 text-sm font-medium leading-5">
            {failureTitle || t('overlay.genericError')}
          </span>
          <button
            type="button"
            onClick={handleDismissCard}
            title={t('window.close')}
            aria-label={t('overlay.dismissAria')}
            className="absolute right-3 top-3 inline-flex h-8 w-8 items-center justify-center rounded-lg border border-white/10 bg-white/5 text-white/60 transition-colors hover:bg-white/15 hover:text-white"
          >
            <X className="h-4 w-4" />
          </button>
          {/* 間距節奏是刻意的：原因緊跟標題（4px，同一件事），恢復提示隔開一檔
              （8px，另一类信息）。两段都用 --overlay-text-muted 而不是 -dim ——
              #666 在 #0b0b0c 上只有 3.4:1，夠不上正文 4.5:1 的無障礙底線；#999 是 6.9:1。
              层级交给顺序和间距表达，不靠把其中一段压暗。

              textWrap: 'pretty' 治孤字末行（只剩「试。」这种）。用内联 style 而不是
              Tailwind 的 text-pretty：那个类要扫源码才产出，跑旧 CSS 产物时这层保护会
              静默失效（见 pitfalls 11 同款教训）。它只调整断行，不增加行数，所以不会
              顶到高度上限。 */}
          {failureDetail && (
            <span
              className="col-start-2 mt-1 pr-9 text-xs leading-5 select-text"
              style={{ color: 'var(--overlay-text-muted)', textWrap: 'pretty' }}
            >
              {failureDetail}
            </span>
          )}
          {/* 只有確實存下來了才提恢復。'unknown' 什麼都不說 —— 存檔是非同步的，
              与其在这里许一个可能作废的承诺，不如等结论到了再补上（见
              OverlayService.updateFailureRecovery）。 */}
          {failureRecovery !== 'unknown' && (
            <span
              className="col-start-2 mt-2 text-xs leading-5"
              style={{ color: 'var(--overlay-text-muted)', textWrap: 'pretty' }}
            >
              {failureRecovery === 'history'
                ? t('overlay.failureRecoverFromHistory')
                : t('overlay.failureNoRecording')}
            </span>
          )}
        </div>
      ) : (
        <div
          data-overlay-content
          className="flex flex-col items-center gap-2"
        >
          {showMicSourceHint && (
            <div
              className="pointer-events-none flex min-w-0 max-w-[calc(100vw-16px)] items-center gap-1.5 overflow-hidden whitespace-nowrap rounded-full border px-3 py-1.5 font-normal"
              style={{
                background: 'rgba(11, 11, 12, 0.94)',
                color: 'var(--overlay-text)',
                borderColor: 'var(--overlay-border)',
                boxShadow: '0 2px 10px rgba(0, 0, 0, 0.2)',
                backdropFilter: 'blur(8px)',
              }}
            >
              <MicVocal
                className="h-3.5 w-3.5 shrink-0"
                strokeWidth={1.8}
                style={{ color: '#ffffff' }}
                aria-hidden
              />
              <span className="min-w-0 truncate text-xs font-normal" style={{ color: '#ffffff' }}>
                {micSourceLabel}
              </span>
            </div>
          )}
          {showStreamingBubble && (
            <div
              className="pointer-events-none relative flex max-w-[440px] flex-col rounded-2xl border px-4 py-2.5"
              style={{
                background: 'var(--overlay-bg)',
                color: 'var(--overlay-text)',
                borderColor: 'var(--overlay-border)',
              }}
            >
              <span
                className="mb-1 text-[10px] font-medium tracking-[0.18em]"
                style={{ color: 'var(--overlay-text-muted)' }}
              >
                {t('overlay.liveCaption')}
              </span>
              {/* 內容驅動尺寸：小預設，隨文字增行慢慢變大，超過 3 行才滾動，只顯示最新內容 */}
              <div
                className="flex max-h-[72px] flex-col justify-end overflow-hidden text-left text-sm leading-6"
                style={{ color: hasStreamingText ? 'var(--overlay-text)' : 'var(--overlay-text-dim)' }}
              >
                <div>
                  {hasStreamingText ? streamingText : t('overlay.listening')}
                  {hasStreamingText && (
                    <span
                      className="ml-0.5 inline-block h-[1.05em] w-[2px] rounded-full align-middle"
                      style={{
                        backgroundColor: 'var(--overlay-text)',
                        animation: 'caret-blink 1.1s ease-in-out infinite',
                      }}
                    />
                  )}
                </div>
              </div>
              {/* 底部朝下尖角：提示文字來自下方的錄音膠囊 */}
              <span
                className="absolute left-1/2 h-0 w-0 -translate-x-1/2"
                style={{
                  bottom: '-7px',
                  borderLeft: '7px solid transparent',
                  borderRight: '7px solid transparent',
                  borderTop: '7px solid var(--overlay-bg)',
                }}
              />
            </div>
          )}
          {/* 膠囊：視窗寬度按波形條數固定，所以這裡必須禁止換行 —— 否則文字一多
              （如「剩余 28s」+「请靠近麦克风」同时出现）就会折成两行，把圆角撑破。
              超出时让**波形**让位（见下方 bars 容器的 min-w-0 + overflow-hidden），
              文字保持完整：文字是信息，波形只是陪衬。 */}
          {/* max-w 是配套的另一半：根容器是 justify-center，膠囊內容一寬就向兩側溢位、
              被窗口裁掉，看着像"两端被切平的圆角矩形"。给了宽度上限，压缩压力才会
              传到波形上（bars 的 min-w-0），胶囊本身始终保持完整的胶囊形状。
              用 100vw 而不是 max-w-full：父层宽度也是按内容算的，撑不出约束。 */}
          <div
            ref={pillRef}
            className={`relative flex max-w-[calc(100vw-8px)] items-center overflow-hidden whitespace-nowrap rounded-full px-4 py-2${recordingPhase && recordingVisualPhase === 'preparing' ? ' overlay-pill-recording-waiting' : ''}${recordingPhase && recordingVisualPhase === 'listening' ? ' overlay-pill-recording-listening' : ''}`}
            style={{ minHeight: '38px' }}
          >
            <span
              key={presentationId}
              aria-hidden
              className="overlay-pill-surface overlay-pill-surface-enter absolute inset-0 rounded-full border"
              style={{
                background: 'var(--overlay-bg)',
                borderColor: 'var(--overlay-border)',
              }}
            />
            {/* 最後一幀波形，壓平併攏成進度條後淡出（index.css 的 overlay-wave-collapse）。
                放在处理中内容**外面**：那一层带着入场的透明度动画，放进去会一起被藏住。
                left 16px = 胶囊 px-4，与波形、进度条的起笔对齐；收完留在 DOM 里是透明的，不碍事。 */}
            {showWaveCollapse && waveSnapshot && (
              <div
                aria-hidden
                className="overlay-wave-collapse pointer-events-none absolute z-[1] flex items-center gap-[2px]"
                style={{
                  left: '16px',
                  top: '50%',
                  height: '20px',
                  marginTop: '-10px',
                  '--collapse-scale': String(waveCollapseScale),
                } as CSSProperties}
              >
                {waveSnapshot.heights.map((height, index) => {
                  const h = `${Math.min(18, Math.max(3, height))}px`
                  return (
                    <span
                      key={index}
                      className="overlay-wave-collapse-bar block w-[2.5px] rounded-full"
                      style={{ backgroundColor: waveSnapshot.colors[index], height: h, '--from-h': h } as CSSProperties}
                    />
                  )
                })}
              </div>
            )}
            {recordingPhase ? (
              <div
                className="overlay-recording-stage relative z-[1] min-w-0"
                style={{ color: 'var(--overlay-text)' }}
              >
                <div className="overlay-preparing-indicator" aria-hidden>
                  {[0, 1, 2].map((index) => (
                    <span
                      key={index}
                      className="overlay-preparing-dot rounded-full"
                      style={{ animationDelay: `${index * 110}ms` }}
                    />
                  ))}
                </div>
                <div
                  className="overlay-recording-content flex min-w-0 items-center"
                  aria-hidden={recordingVisualPhase !== 'listening'}
                >
                  {warning && warningTone === 'error' ? (
                    // 靜音高警：波形此時是平的、無意義，直接在膠囊里居中顯示紅字，
                    // 既更醒目、也避免波形+長文字撐破固定寬度把膠囊圓角裁掉。
                    <div
                      key={warning}
                      className="overlay-attention flex items-center whitespace-nowrap px-1 text-xs font-semibold text-red-500"
                      style={{ height: '20px' }}
                    >
                      {warning}
                    </div>
                  ) : (
                    <>
                      {/* 有文字要佔位時**少畫幾根**，而不是讓 overflow 把波形裁一半 —— 裁出來的
                          半根条子看着像坏了。少画是"看起来就是这么设计的"。
                          min-w-0 仍然留着兜底：万一文字特别长，宁可裁波形也不折行。 */}
                      <div className="flex min-w-0 items-center gap-[2px] overflow-hidden" style={{ height: '20px' }}>
                        {visibleBars.map((height, index) => {
                          const color = palette[index]
                          return (
                            <div
                              key={index}
                              className="w-[2.5px] rounded-full"
                              style={{
                                backgroundColor: color,
                                boxShadow: 'none',
                                height: `${Math.min(18, Math.max(3, height))}px`,
                                opacity: 0.7 + (Math.min(18, height) / 18) * 0.3,
                                transition: 'height 50ms ease-out, opacity 50ms ease-out',
                              }}
                            />
                          )
                        })}
                      </div>
                      {/* 計時**不再**被警告頂掉：警告是一閃而過的提示，計時是持續狀態。
                          以前条件里带 !warning，导致 4 分钟提示一出现，剩下一整分钟都看不到秒数。 */}
                      {showDuration && (
                        <span
                          className={`ml-1.5 shrink-0 whitespace-nowrap text-right font-mono tabular-nums text-xs${inCountdown ? ' font-semibold' : ''}${urgent ? ' overlay-breathe' : ''}`}
                          style={{ color: timerColor }}
                        >
                          {timerText}
                        </span>
                      )}
                      {warning && (
                        <span key={warning} className="overlay-attention ml-2 shrink-0 whitespace-nowrap text-xs text-amber-400">
                          {warning}
                        </span>
                      )}
                    </>
                  )}
                </div>
              </div>
            ) : (
              // key 讓每種內容各自掛載一次，入場動畫才會在換內容時重新播放。
              // 波形在收的時候，處理中內容等它收到進度條那個位置（約 170ms）再出來。
              <div
                key={pillContentKey ?? pillMorphKey}
                className={`relative z-[1] flex min-w-0 items-center${animatePillContent ? ' overlay-pill-content-enter' : ''}`}
                style={{
                  color: 'var(--overlay-text)',
                  animationDelay: showWaveCollapse ? '170ms' : undefined,
                }}
              >
                {state === 'thinking' && (
                  <div className="flex items-center gap-2">
                    {/* 三段式進度（估計段 → 慢蹭段 → 流光），動畫定義與原因見 index.css 的
                        overlay-progress-*。--estimate 是估计时长，后两段都以它为延迟起点。 */}
                    <div
                      className="relative h-1 w-12 overflow-hidden rounded-full bg-white/10"
                      style={{ '--estimate': `${thinkingDuration}s` } as CSSProperties}
                    >
                      <div className="overlay-progress-estimate absolute inset-0">
                        <div
                          className="overlay-progress-creep absolute inset-0 rounded-full"
                          style={{ backgroundColor: thinkingColor }}
                        />
                      </div>
                      <div className="overlay-progress-sheen absolute inset-y-0 left-0 rounded-full" aria-hidden />
                    </div>
                    {/* AI 整理時帶一枚星芒。顏色跟處理中文字走（也就是使用者的波形主題），
                        不另配彩虹渐变：胶囊配色是用户自己选的，叠一层流行色会撞。
                        「已超时仍在等」那句不带：那时要传达的是"慢了"，不是"在做 AI"。 */}
                    <span className="flex items-center gap-1 text-xs whitespace-nowrap" style={{ color: thinkingColor }}>
                      {thinkingAi && thinkingNote !== 'late' && (
                        <Sparkles aria-hidden className="overlay-ai-sparkle h-3.5 w-3.5 shrink-0" strokeWidth={2.2} />
                      )}
                      {thinkingNote === 'late'
                        ? t('overlay.awaitingLateResult')
                        : t(thinkingAi ? 'overlay.processingAi' : 'overlay.processing')}
                    </span>
                    {/* 這裡**故意**不寫「Esc 取消」。Esc 取消的能力照常生效（原生鉤子 +
                        escape-action，与本组件无关），只是这行提示不该出现在悬浮窗上：
                        悬浮窗是贴在光标附近、每次口述都会闪一下的东西，越安静越好，而
                        「按 Esc 能取消」是知道一次就一直知道的事实，不需要每次复述。
                        这件事已在 设置 → 键盘快捷键 的说明里静态交代（见 GeneralSettingsPage）。
                        要加回来之前先想清楚：它每次处理都出现，收益只有第一次。 */}
                  </div>
                )}

                {state === 'error' && (
                  <div className="flex items-center gap-2">
                    <span className="text-xs text-red-400">{errorMessage || t('overlay.genericError')}</span>
                  </div>
                )}

                {state === 'toast' && (
                  <div className="flex items-center gap-2">
                    {toastTone === 'warn' ? (
                      <span className="whitespace-nowrap text-xs text-amber-400">{toastText}</span>
                    ) : (
                      <span className="whitespace-nowrap text-xs" style={{ color: 'var(--overlay-text)' }}>
                        {toastText}
                      </span>
                    )}
                  </div>
                )}
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  )
}
