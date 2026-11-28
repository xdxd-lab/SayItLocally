import { addRuntimeEvent } from '../debugLog'
import { isUnconfirmedPaste } from './helpers'
import type { PasteService, ProbeResult } from './PasteService'

/** 插字的結果。stale = 這一代中途失效了，呼叫方什麼都不用做。 */
export type InsertionOutcome =
  | { kind: 'stale' }
  | { kind: 'inserted' }
  | { kind: 'fallback'; reason: string }

const STALE: InsertionOutcome = { kind: 'stale' }

export interface InsertionOptions {
  runId: number
  /** 錄音開始時拍的目標視窗快照；沒有才現查（現查拿到的可能已經是別的視窗）。 */
  probeResult: ProbeResult | null
  finalReceivedAt: number
  protectClipboard: boolean
  /** 每次 await 之後都要驗：這一代還是不是當前代。 */
  isCurrent: () => boolean
  waitForModifierRelease: () => Promise<void>
  /** 真正貼上之前的最後一步（關掉 Esc 取消）。貼上一旦發出就撤不回來了。 */
  beforeIrreversiblePaste: () => Promise<void>
}

/**
 * 把識別出的文字送進目標視窗：拿探測結果、判斷能不能寫、等修飾鍵鬆開、貼上、給結果分類。
 * 從 RecorderOrchestrator.handleTextInsertion 拆出（模組 6），行為不變；
 * 根據結果改狀態、彈卡片仍由編排器做。
 */
export class InsertionController {
  constructor(private readonly paste: PasteService) {}

  async insert(text: string, opts: InsertionOptions): Promise<InsertionOutcome> {
    const insertionStartedAt = Date.now()
    const { runId } = opts
    // 始終使用本次 final 攜帶的 probe 快照，不能讀取可能已被下一次錄音覆蓋的例項欄位。
    const capturedProbe = opts.probeResult
    const probe = capturedProbe ?? await this.paste.getProbeResult()
    if (!opts.isCurrent()) return STALE

    const usedCapturedProbe = capturedProbe !== null
    const probeAgeMs = typeof probe.completedAt === 'number' ? Date.now() - probe.completedAt : undefined
    const probeDurationMs = (
      typeof probe.completedAt === 'number'
      && typeof probe.startedAt === 'number'
    )
      ? probe.completedAt - probe.startedAt
      : undefined
    addRuntimeEvent('info', 'recorder', 'Paste decision', {
      runId,
      probeId: probe.probeId,
      editable: probe.editable,
      gate: probe.gate,
      hwnd: probe.hwnd,
      focusHwnd: probe.focusHwnd,
      pid: probe.pid,
      process: probe.process,
      verdict: probe.verdict,
      isCurrentAppProcess: probe.isCurrentAppProcess,
      windowClass: probe.windowClass,
      focusClass: probe.focusClass,
      usedCapturedProbe,
      probeAgeMs,
      probeDurationMs,
      finalToDecisionMs: opts.finalReceivedAt > 0 ? insertionStartedAt - opts.finalReceivedAt : undefined,
      detail: probe.detail,
      textLen: text.length,
    })

    if (probe.isCurrentAppProcess) {
      // SayIt 自身也是 Chromium 視窗，renderer 直插對 React 受控元件無效
      // （DOM value 被设置但 React state 不同步，下次 re-render 会覆盖）。
      // 所以不走 renderer 直插，而是和外部窗口一样走 Rust paste（Ctrl+V）。
      addRuntimeEvent('info', 'recorder', 'Target is SayIt; using native paste instead of renderer insertion', {
        probeId: probe.probeId,
        editable: probe.editable,
        hwnd: probe.hwnd,
        focusHwnd: probe.focusHwnd,
      })
    }

    if (!probe.editable) {
      // 目標不可編輯時只展示主動操作卡片，不提前改寫使用者剪貼簿。
      addRuntimeEvent('info', 'recorder', 'Target is not editable; showing fallback card', {
        probeId: probe.probeId,
        pid: probe.pid,
        process: probe.process,
        verdict: probe.verdict,
        gate: probe.gate,
        isCurrentAppProcess: probe.isCurrentAppProcess,
        detail: probe.detail,
      })
      if (!opts.isCurrent()) return STALE
      return { kind: 'fallback', reason: 'not_editable' }
    }

    await opts.waitForModifierRelease()
    if (!opts.isCurrent()) return STALE

    await opts.beforeIrreversiblePaste()
    if (!opts.isCurrent()) return STALE

    // Target was editable → paste (pass the captured probe so Rust uses the original hwnd)
    const pasteStartedAt = Date.now()
    const result = await this.paste.pasteText(text, probe, opts.protectClipboard)
    if (!opts.isCurrent()) return STALE
    const pasteExecMs = Date.now() - pasteStartedAt

    // 報成功但拿不準的：交給兜底卡片，文字可能已經進去了，也可能沒有（判據見 isUnconfirmedPaste）。
    if (result.ok && isUnconfirmedPaste(result.strategy, pasteExecMs)) {
      addRuntimeEvent('warn', 'recorder', 'External text insertion unconfirmed; showing fallback card', {
        strategy: result.strategy,
        gate: probe.gate,
        detail: result.detail,
        finalToPasteDoneMs: opts.finalReceivedAt > 0 ? Date.now() - opts.finalReceivedAt : undefined,
        pasteExecMs,
      })
      return { kind: 'fallback', reason: 'paste_unconfirmed' }
    }

    if (result.ok) {
      addRuntimeEvent('info', 'recorder', 'External text insertion succeeded', {
        strategy: result.strategy,
        // 成功側也要留 gate：SendInput 返回成功並不代表文本真的落進了輸入框
        // （UIPI 攔截是靜默的），使用者報「顯示成功但沒插進去」時，這一個欄位就能
        // 說明當初是靠哪一層判據放行的，不必再讓他復現一次。
        gate: probe.gate,
        detail: result.detail,
        attempts: result.attempts,
        finalToPasteDoneMs: opts.finalReceivedAt > 0 ? Date.now() - opts.finalReceivedAt : undefined,
        pasteExecMs,
      })

      return { kind: 'inserted' }
    }

    // Paste command failed (SendInput error, timeout, etc.). The card now lets the user
    // explicitly copy or dismiss; do not overwrite the clipboard automatically here.
    const level = result.reason === 'paste_exception' ? 'error' : 'warn'
    addRuntimeEvent(level, 'recorder', 'External text insertion failed; showing fallback card', {
      strategy: result.strategy,
      reason: result.reason,
      gate: probe.gate,
      detail: result.detail,
      attempts: result.attempts,
      finalToPasteDoneMs: opts.finalReceivedAt > 0 ? Date.now() - opts.finalReceivedAt : undefined,
      pasteExecMs: Date.now() - pasteStartedAt,
    })
    return { kind: 'fallback', reason: result.reason || 'paste_failed' }
  }
}
