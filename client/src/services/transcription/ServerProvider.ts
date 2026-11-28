// 伺服器模式 Provider：伺服器負責 ASR，AI 可由伺服器或客戶端自定義供應商完成。

import { getSetting } from '../store'
import { addRuntimeEvent } from '../debugLog'
import * as ws from '../websocket'
import { polishWithClientAi } from './clientAiPolish'
import {
  resolveAiPolicy,
  resolveAndLogAiOutcome,
  type AiOutcomeContext,
  type AiPolicy,
} from './aiPolicy'
import {
  getRuntimeServerAiSource,
  loadServerAiSource,
  type ServerAiSource,
} from './serverAiSource'
import { MID_SESSION_DISCONNECT_ERROR } from './types'
import type {
  FinalResult,
  TranscriptionProvider,
  TranscriptionCallbacks,
  StartOptions,
  StopOptions,
} from './types'

/**
 * 攢夠這麼多位元組才發一個 WebSocket 包：16 kHz、16 bit 單聲道下正好 200 ms。
 *
 * 採集端每 8 ms 左右給一幀（AudioWorklet 一個渲染塊），原來收到就發，每秒 125 個
 * 256 位元組的小包，回落路徑更碎（每秒約 375 個 85 位元組）。服務端松鍵之前只是把音訊攢著，
 * 沒有流式結果，所以合批不影響識別結果和速度；雲 API 模式一直就是 200 ms 一批。
 * 松鍵時剩下的不足 200 ms 一定在 stop 之前發掉（見 flushAudio）。
 */
const SEND_BATCH_BYTES = 6400

export class ServerProvider implements TranscriptionProvider {
  readonly mode = 'server' as const

  private callbacks: TranscriptionCallbacks = {}
  private activeRunId = 0
  private activeStartOpts: Readonly<StartOptions> | undefined
  private aiSource: ServerAiSource = 'managed'
  private customAiReady = false
  private customFinalPending = false
  private serverDonePending = false
  /** 松鍵時隨 stop 傳入的本次策略；與線上那個 disable_ai 布林同出一源。 */
  private activePolicy: AiPolicy | undefined
  private activeConnectionId: string | undefined
  private pendingAudio: ArrayBuffer[] = []
  private pendingAudioBytes = 0

  async connect(callbacks: TranscriptionCallbacks): Promise<void> {
    this.callbacks = callbacks
    const [storedSource, customProvider, customUrl, customKey, customModel] = await Promise.all([
      loadServerAiSource(),
      getSetting('cloudAi.provider', '') as Promise<string>,
      getSetting('cloudAi.apiUrl', '') as Promise<string>,
      getSetting('cloudAi.apiKey', '') as Promise<string>,
      getSetting('cloudAi.model', '') as Promise<string>,
    ])
    this.aiSource = storedSource === 'custom' ? 'custom' : 'managed'
    this.customAiReady = Boolean(
      customUrl.trim()
      && customModel.trim()
      && (customKey.trim() || customProvider === 'ollama'),
    )

    await ws.connect({
      onStateChange: (state) => {
        callbacks.onStateChange?.(state)
        // 連線在一次錄音進行中掉了：必須當場報出去。
        //
        // 以前這裡只是轉發狀態、沒人處理，於是錄音繼續跑（音訊被 sendAudio 靜靜丟掉），
        // 停止時 sendStop 也發不出去，錄音器照舊進入 45 秒超時等待 —— 使用者看著懸浮條轉
        // 一分鐘，最後什麼提示都沒有。實測 2026-09-07 兩段長錄音都是這麼沒的。
        if (state !== 'disconnected' && state !== 'error') return
        const runId = this.activeRunId
        if (runId === 0) return
        addRuntimeEvent('error', 'server', 'Connection dropped during an active recording', {
          runId,
          state,
        })
        this.resetRun()
        callbacks.onError?.(MID_SESSION_DISCONNECT_ERROR)
      },
      onReady: (data) => {
        // 留著服務端連線標識：ai.outcome 帶上它，才能把客戶端日誌和伺服器日誌
        // （每行有 cid=/sid=）直接對起來，不必再靠時間戳手工對齊。
        this.activeConnectionId = data.connectionId
        callbacks.onReady?.({
          connectionId: data.connectionId,
          asr: data.asr,
          // 自定義 AI 在本機執行；服務端 llm=false 不應把整條模式標成不可用。
          llm: this.aiSource === 'custom' ? this.customAiReady : data.llm,
        })
      },
      onASR: (result) => {
        if (this.activeRunId === 0) return
        const isEmpty = !result.text.trim()
        callbacks.onASR?.({
          text: result.text,
          asrMs: result.asrMs,
          durationSec: result.durationSec,
          // 空識別不會執行任何 AI；它會在錄音器的專用空結果分支先於 final 落庫。
          aiSource: isEmpty ? 'none' : undefined,
          aiStatus: isEmpty ? 'skipped' : undefined,
        })
      },
      onFinal: (result) => {
        const runId = this.activeRunId
        if (runId === 0) return
        if (this.aiSource === 'custom') {
          this.customFinalPending = true
          void this.handleCustomFinal(runId, result)
          return
        }

        // 伺服器內建 AI 路線。此前這裡按 `llmMs > 0` 反推成功失敗 —— 服務端異常時
        // 返回原文 + 0ms，於是"呼叫失敗"被誤記成"不可用"，兩者在介面和日誌里長得一樣。
        // 現在用服務端回傳的證據判斷（error / provider），耗時只作輔助。
        const policy = this.resolvePolicy(result.durationSec)
        const outcome = resolveAndLogAiOutcome(this.outcomeContext(), policy, {
          asrTextEmpty: !result.asrText.trim(),
          serverError: result.serverAi?.error,
          serverProvider: result.serverAi?.provider,
          llmMs: result.llmMs,
        })
        callbacks.onFinal?.({
          asrText: result.asrText,
          // 仍原樣轉發服務端的 llm_text：跳過/失敗時服務端本來就回原文，而選區編輯的
          // 安全回退由錄音器按 contextApplied 處理。這裡改寫它會動到那條路徑。
          llmText: result.llmText,
          asrMs: result.asrMs,
          llmMs: result.llmMs,
          durationSec: result.durationSec,
          asrEngine: result.asrEngine,
          asrModel: result.asrModel,
          contextApplied: result.contextApplied,
          aiSource: outcome.source,
          aiStatus: outcome.status,
          aiProvider: outcome.provider,
        })
      },
      onDone: () => {
        const runId = this.activeRunId
        if (runId === 0) return
        if (this.customFinalPending) {
          // 服務端 final 後會立刻發 done；客戶端 AI 尚未完成時必須暫存，避免錄音器先收尾。
          this.serverDonePending = true
          return
        }
        this.finishRun(runId)
      },
      onError: (msg) => {
        const runId = this.activeRunId
        if (runId === 0) return
        callbacks.onError?.(msg)
        if (this.activeRunId === runId) this.resetRun()
      },
    })
  }

  start(opts: StartOptions): boolean {
    // 選區模型只覆蓋本次錄音；普通錄音沿用開始時凍結的來源或當前執行時設定。
    this.aiSource = opts.aiConfig?.selectionAiOverride
      ? 'custom'
      : opts.aiConfig?.serverAiSource ?? getRuntimeServerAiSource()
    this.activeRunId = opts.runId
    this.activeStartOpts = {
      ...opts,
      hotwords: opts.hotwords ? [...opts.hotwords] : undefined,
      textContext: opts.textContext ? { ...opts.textContext } : undefined,
    }
    this.customFinalPending = false
    this.serverDonePending = false
    this.dropPendingAudio()

    const wireOptions = this.aiSource === 'custom'
      ? {
        ...opts,
        // 自定義 AI 由本機呼叫：服務端只做 ASR，也不需要收到 Prompt 或編輯器正文。
        disableAi: true,
        selectionAiConfig: undefined,
        systemPrompt: undefined,
        textContext: undefined,
      }
      : opts
    const started = ws.sendStart(wireOptions)
    if (!started) this.resetRun()
    return started
  }

  cancel(): void {
    this.resetRun()
    ws.disconnect()
  }

  sendAudio(buffer: ArrayBuffer): void {
    this.pendingAudio.push(buffer)
    this.pendingAudioBytes += buffer.byteLength
    if (this.pendingAudioBytes >= SEND_BATCH_BYTES) this.flushAudio()
  }

  private flushAudio(): void {
    if (this.pendingAudio.length === 0) return
    const chunks = this.pendingAudio
    const total = this.pendingAudioBytes
    this.pendingAudio = []
    this.pendingAudioBytes = 0
    if (chunks.length === 1) {
      ws.sendAudio(chunks[0])
      return
    }
    const merged = new Uint8Array(total)
    let offset = 0
    for (const chunk of chunks) {
      merged.set(new Uint8Array(chunk), offset)
      offset += chunk.byteLength
    }
    ws.sendAudio(merged.buffer)
  }

  private dropPendingAudio(): void {
    this.pendingAudio = []
    this.pendingAudioBytes = 0
  }

  stop(opts?: StopOptions): boolean {
    // 留住完整策略，別再把它壓成 activeStartOpts.disableAi —— 那樣一來"路由是自配"
    // 和"低於門檻"就都變成同一個布林，結束時再也分不出這次為什麼沒整理。
    if (opts?.aiPolicy) this.activePolicy = opts.aiPolicy
    // 尾巴必須先於 stop 發出：服務端收到 stop 就開始識別，之後到的音訊不算數。
    this.flushAudio()
    return ws.sendStop(this.aiSource === 'custom' ? { ...opts, disableAi: true } : opts)
  }

  /**
   * 取本次策略。正常路徑由 stop 傳入；缺失時（理論上不該發生）用 start 凍結的配置
   * 快照就地重算一份，仍然走同一個判據函式，絕不退回舊的布林推斷。
   */
  private resolvePolicy(durationSec: number): AiPolicy {
    if (this.activePolicy) return this.activePolicy
    const snapshot = this.activeStartOpts?.aiConfig
    const fallback = resolveAiPolicy({
      workMode: 'server',
      aiEnabled: snapshot?.aiEnabled ?? !(this.activeStartOpts?.disableAi ?? false),
      aiMinDurationSec: snapshot?.aiMinDurationSec ?? this.activeStartOpts?.aiMinDurationSec ?? 0,
      serverAiSource: snapshot?.serverAiSource ?? this.aiSource,
      selectionAiOverride: snapshot?.selectionAiOverride,
      audioDurationSec: durationSec,
    })
    this.activePolicy = fallback
    return fallback
  }

  private outcomeContext(): AiOutcomeContext {
    return {
      operationId: this.activeStartOpts?.operationId || `run-${this.activeRunId}`,
      trigger: this.activeStartOpts?.source === 'history_reprocess' ? 'history_reprocess' : 'live',
      serverRef: this.activeConnectionId,
    }
  }

  disconnect(): void {
    this.resetRun()
    ws.disconnect()
  }

  isReady(): boolean {
    return ws.isConnected()
  }

  private async handleCustomFinal(runId: number, result: FinalResult): Promise<void> {
    const startOptions = this.activeStartOpts
    // 自配路線：服務端的 final 只代表識別完成，整次整理還沒結束，最終狀態必須取
    // 客戶端潤色的結果，不能被服務端的 llm_ms=0 覆蓋。
    const polished = await polishWithClientAi({
      asrText: result.asrText,
      startOptions,
      policy: this.resolvePolicy(result.durationSec),
      outcomeContext: this.outcomeContext(),
      logSource: 'server',
      isCurrent: () => this.activeRunId === runId,
    })
    if (!polished || this.activeRunId !== runId) return

    this.callbacks.onFinal?.({
      ...result,
      ...polished,
    })
    this.customFinalPending = false

    if (this.serverDonePending) {
      this.finishRun(runId)
    }
  }

  private finishRun(runId: number): void {
    if (this.activeRunId !== runId) return
    this.callbacks.onDone?.()
    if (this.activeRunId === runId) this.resetRun()
  }

  private resetRun(): void {
    this.dropPendingAudio()
    if (this.activeRunId !== 0 && this.customFinalPending) {
      addRuntimeEvent('info', 'server', 'Discarded pending custom AI result for stale run', {
        runId: this.activeRunId,
      })
    }
    this.activeRunId = 0
    this.activeStartOpts = undefined
    this.activePolicy = undefined
    this.customFinalPending = false
    this.serverDonePending = false
  }
}
