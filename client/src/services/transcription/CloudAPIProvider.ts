// 雲 API 模式 Provider
//
// 兩條路，按供應商分：
//   · 有流式實現的（見下面 STREAM_COMMANDS）→ 邊錄邊發 WebSocket，可出即時字幕；
//   · 其餘 → 錄完再發一次 HTTP（cloud_transcribe）。
// 流式建連失敗會自動回落到後者 —— 音訊始終在 pcmBuffers 裡留著完整一份。

import { buildAsrExtra, isQwenOmniProvider, isStreamingDisplayReady, resolveQwenOmniModel } from '@/lib/asrModels'
import { uint8ArrayToBase64 } from '@/lib/encoding'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { getSetting } from '../store'
import { restoreHotwordSpacing } from '../textPostProcess'
import { addRuntimeEvent } from '../debugLog'
import { cloudTranscribeRaw, notifyAsrCapabilityMaybeChanged } from '../bridge'
import { polishWithClientAi } from './clientAiPolish'
import { policyFromSnapshot, resolveAndLogAiOutcome, type AiOutcomeContext } from './aiPolicy'
import type {
  TranscriptionProvider,
  TranscriptionCallbacks,
  StartOptions,
  StopOptions,
  WorkMode,
} from './types'

interface AsrProviderConfig {
  provider: string
  api_key: string
  app_id: string
  extra?: Record<string, unknown>
}

interface AsrResult { text: string; elapsed_ms: number }

/**
 * 一個流式 ASR 供應商需要的全部前端資訊。
 *
 * 每個欄位都對應一處真實差異，不是可配置項 —— 各家 open 的 Rust 簽名收的引數不同，
 * 傳多了或少了都會讓 open 失敗，而失敗的症狀是**靜默退回「錄完再發」**，
 * 不報錯、只是字幕永遠不出來。
 */
interface StreamCommandSet {
  open: string
  send: string
  finish: string
  close: string
  /**
   * 關掉即時字幕時還走不走流式。
   *
   * 只有豆包是 true：它的流式協議同時也是它唯一的識別通道，realtime 只決定要不要
   * 把中間結果 emit 上來。其餘幾家關掉字幕就走「錄完再發」的一次性路徑，
   * 省一條長連線。這個不對稱是有意的，別為了整齊把它抹平。
   */
  streamWithoutRealtime?: boolean
  /** open 要帶 sampleRate（只有豆包的 Rust 簽名收它，其餘把 16000 寫死在函式體裡） */
  needsSampleRate?: boolean
  /** open 要帶 workspaceId（百鍊業務空間，只有千問那兩個用） */
  needsWorkspaceId?: boolean
  /** open 要帶 appId（只有豆包舊版控制台用得上） */
  needsAppId?: boolean
  /** open 要帶選定的模型（同一份協議下有多個模型的那幾家） */
  needsModel?: boolean
}

/**
 * 每家流式供應商的一組原生命令。**加一家只改這張表。**
 *
 * 它們是各不相同的 WebSocket 協議，不是同一個介面的幾個模型名，所以 Rust 側
 * 各有一份實現：
 *   · doubao_v2               → 火山流式語音識別 2.0（自有二進位制分幀協議）
 *   · qwen_realtime           → OpenAI-Realtime 風格（/api-ws/v1/realtime）
 *   · qwen_audio_stream       → DashScope duplex run-task（/api-ws/v1/inference）
 *   · openai_live_transcribe  → OpenAI Realtime 轉寫會話（/v1/realtime?intent=transcription）
 *   · gemini_live_transcribe  → Gemini Live API（BidiGenerateContent）
 *
 * 這裡只記住本次會話用的是哪一組，收尾時才不會去 finish 一條沒開的會話。
 *
 * 以前這套是「豆包一條硬編碼 if 分支 + 千問一個命令組 + queueNativeClose 裡第三份
 * close 清單 + runProcess 裡第四處分支」，加一家要改四處。漏掉 close 那處的後果最陰：
 * 取消錄音時新供應商的 WebSocket 不會被關，只能靠 Rust 側下一次 open 的 cleanup 兜底。
 */
const STREAM_COMMANDS: Record<string, StreamCommandSet> = {
  doubao_v2: {
    open: 'doubao_stream_open',
    send: 'doubao_stream_send',
    finish: 'doubao_stream_finish',
    close: 'doubao_stream_close',
    streamWithoutRealtime: true,
    needsSampleRate: true,
    needsAppId: true,
  },
  qwen_realtime: {
    open: 'qwen_stream_open',
    send: 'qwen_stream_send',
    finish: 'qwen_stream_finish',
    close: 'qwen_stream_close',
    needsWorkspaceId: true,
  },
  qwen_audio_stream: {
    open: 'qwen_audio_stream_open',
    send: 'qwen_audio_stream_send',
    finish: 'qwen_audio_stream_finish',
    close: 'qwen_audio_stream_close',
    needsWorkspaceId: true,
  },
  openai_live_transcribe: {
    open: 'openai_live_open',
    send: 'openai_live_send',
    finish: 'openai_live_finish',
    close: 'openai_live_close',
    needsModel: true,
  },
  gemini_live_transcribe: {
    open: 'gemini_live_open',
    send: 'gemini_live_send',
    finish: 'gemini_live_finish',
    close: 'gemini_live_close',
    needsModel: true,
  },
}

/** 所有 close 命令。取消錄音時逐個發，沒有會話時每個都是無操作。 */
const ALL_STREAM_CLOSE_COMMANDS = Object.values(STREAM_COMMANDS).map((c) => c.close)

function streamCommandsOf(provider: string): StreamCommandSet | undefined {
  return STREAM_COMMANDS[provider]
}

export class CloudAPIProvider implements TranscriptionProvider {
  readonly mode: WorkMode = 'cloud_api'

  private callbacks: TranscriptionCallbacks = {}
  private pcmBuffers: ArrayBuffer[] = []
  private sessionActive = false
  private activeRunId = 0
  private activeStartOpts: Readonly<StartOptions> | undefined
  private ready = false

  /** 本次會話用的流式命令組；沒走流式時為 null */
  private streamCommands: StreamCommandSet | null = null
  /** open 成功才置 true。僅 streamCommands 非空不代表連上了 */
  private streamReady = false
  private streamStartTime = 0
  private pendingChunks: ArrayBuffer[] = []
  private flushTimer: ReturnType<typeof setInterval> | null = null

  // 流式即時顯示：中間結果事件的取消監聽函式
  private partialUnlisten: (() => void) | null = null

  // 流式傳送序列化：所有音訊包經此鏈路順序傳送，保證收尾負包一定排在最後，
  // 避免「音訊包在負包之後到達」導致服務端報 last packet has been received already。
  private sendLock: Promise<void> = Promise.resolve()
  private streamFinishing = false

  // 原生流生命週期序列化：open / finish / close 共用 Provider 級佇列。
  // cancel 會先把 close 排入舊 open 之後；下一代 open 因而只能在舊 close 完成後執行。
  private nativeLifecycleQueue: Promise<void> = Promise.resolve()

  async connect(callbacks: TranscriptionCallbacks): Promise<void> {
    this.callbacks = callbacks
    this.ready = true
    callbacks.onStateChange?.('connected')
    callbacks.onReady?.({ asr: true, llm: true })
  }

  start(opts: StartOptions): boolean {
    if (!this.ready) {
      addRuntimeEvent('error', 'cloud_api', 'Start failed: provider is not ready')
      return false
    }

    // 新會話不得複用舊會話的任何邏輯狀態；舊 close 會先進入生命週期佇列。
    if (this.activeRunId !== 0) this.cancel()

    const runOpts = this.snapshotStartOptions(opts)
    this.pcmBuffers = []
    this.sessionActive = true
    this.activeRunId = runOpts.runId
    this.activeStartOpts = runOpts
    this.streamCommands = null
    this.streamReady = false
    this.streamStartTime = performance.now()
    this.pendingChunks = []
    this.streamFinishing = false
    this.sendLock = Promise.resolve()

    // 非同步判斷供應商並建連；所有非同步步驟只使用本次 run 的 StartOptions 快照。
    void this.tryStartRealtimeStream(runOpts.runId, runOpts)

    return true
  }

  private snapshotStartOptions(opts: StartOptions): Readonly<StartOptions> {
    return {
      ...opts,
      hotwords: opts.hotwords ? [...opts.hotwords] : undefined,
      textContext: opts.textContext ? { ...opts.textContext } : undefined,
    }
  }

  private isRunCurrent(runId: number): boolean {
    return runId !== 0 && this.activeRunId === runId
  }

  private completeRun(runId: number): void {
    if (!this.isRunCurrent(runId)) return

    // 先讓本次 run 失效，後續遲到結果一律丟棄；close 在佇列中等待舊傳送鏈排空。
    this.activeRunId = 0
    this.activeStartOpts = undefined
    const sendLockToDrain = this.sendLock
    this.sessionActive = false
    this.pcmBuffers = []
    this.pendingChunks = []
    this.streamFinishing = true
    this.streamCommands = null
    this.streamReady = false
    if (this.flushTimer) { clearInterval(this.flushTimer); this.flushTimer = null }
    this.teardownPartials()
    void this.queueNativeClose(sendLockToDrain)
  }

  private enqueueNativeLifecycle<T>(operation: () => Promise<T>): Promise<T> {
    const result = this.nativeLifecycleQueue.then(operation)
    this.nativeLifecycleQueue = result.then(() => undefined, () => undefined)
    return result
  }

  private queueNativeClose(sendLockToDrain: Promise<void>): Promise<void> {
    return this.enqueueNativeLifecycle(async () => {
      await sendLockToDrain.catch(() => { })
      // 逐個關：每個 close 在沒有會話時都是無操作，所以不需要先判斷開了哪一個。
      // 清單來自 STREAM_COMMANDS，加供應商時不會漏掉這裡。
      for (const close of ALL_STREAM_CLOSE_COMMANDS) {
        await invoke(close).catch(() => { })
      }
    })
  }

  private invokeNativeOpen(
    runId: number,
    command: string,
    args: Record<string, unknown>,
  ): Promise<boolean> {
    return this.enqueueNativeLifecycle(async () => {
      if (!this.isRunCurrent(runId)) return false
      try {
        await invoke(command, args)
      } catch (err) {
        if (!this.isRunCurrent(runId)) return false
        throw err
      }
      if (!this.isRunCurrent(runId)) return false
      return true
    })
  }

  private invokeNativeFinish(
    runId: number,
    command: string,
  ): Promise<string | undefined> {
    return this.enqueueNativeLifecycle(async () => {
      if (!this.isRunCurrent(runId)) return undefined
      try {
        const text = await invoke<string>(command)
        if (!this.isRunCurrent(runId)) return undefined
        return text
      } catch (err) {
        if (!this.isRunCurrent(runId)) return undefined
        throw err
      }
    })
  }

  /** 訂閱 Rust 上拋的流式中間識別結果，即時轉發給上層用於懸浮窗上屏 */
  private async subscribePartials(runId: number): Promise<void> {
    if (this.partialUnlisten) return
    let partialCount = 0
    const unlisten = await listen<{ text?: string }>('asr-partial', (event) => {
      if (!this.isRunCurrent(runId)) return
      if (!this.sessionActive && this.pcmBuffers.length === 0) return
      const text = event.payload?.text ?? ''
      partialCount++
      // 只記錄第一條，避免刷屏；證明前端確實收到了 Rust 上拋的中間結果
      if (partialCount === 1) {
        addRuntimeEvent('info', 'cloud_api', 'First streaming partial received', { textLen: text.length })
      }
      this.callbacks.onPartialASR?.(text)
    })
    if (!this.isRunCurrent(runId) || this.partialUnlisten) {
      unlisten()
      return
    }
    this.partialUnlisten = unlisten
    addRuntimeEvent('info', 'cloud_api', 'Subscribed to asr-partial events')
  }

  private teardownPartials(): void {
    if (this.partialUnlisten) {
      this.partialUnlisten()
      this.partialUnlisten = null
    }
  }

  sendAudio(buffer: ArrayBuffer): void {
    if (!this.sessionActive) return

    // 始終快取一份（用於非豆包場景 + 音訊儲存）
    this.pcmBuffers.push(buffer.slice(0))

    // 流式：攢到 pendingChunks，由定時器批次傳送。
    // 收尾階段不再接收新音訊，確保負包之後不會再有音訊包。
    if (this.streamCommands && !this.streamFinishing) {
      this.pendingChunks.push(buffer.slice(0))
    }
  }

  stop(_opts?: StopOptions): boolean {
    if (!this.sessionActive) return false
    const runId = this.activeRunId
    const runOpts = this.activeStartOpts
    if (!runOpts || runOpts.runId !== runId) return false
    this.sessionActive = false
    void this.runProcess(runId, runOpts)
    return true
  }

  cancel(): void {
    // 必須先使 run 失效，再清理前端狀態；close 最後入隊且等待此刻的舊傳送鏈。
    this.activeRunId = 0
    this.activeStartOpts = undefined
    const sendLockToDrain = this.sendLock
    this.sessionActive = false
    this.pcmBuffers = []
    this.pendingChunks = []
    this.streamFinishing = true
    this.streamCommands = null
    this.streamReady = false
    this.teardownPartials()
    if (this.flushTimer) { clearInterval(this.flushTimer); this.flushTimer = null }
    void this.queueNativeClose(sendLockToDrain)
  }

  disconnect(): void {
    this.cancel()
    this.ready = false
    this.callbacks.onStateChange?.('disconnected')
  }

  isReady(): boolean {
    return this.ready
  }


  // ── 流式建連 ──

  private async tryStartRealtimeStream(
    runId: number,
    startOpts: Readonly<StartOptions>,
  ): Promise<void> {
    try {
      const asrProvider = await getSetting('cloudAsr.provider', 'doubao') as string
      const qwenWorkspaceId = await getSetting('cloudAsr.qwen.workspaceId', '') as string
      if (!this.isRunCurrent(runId)) return

      // 是否為本次會話開啟「流式即時顯示」：直接讀設定（單一真源，避免錄音器快取過期），
      // 結合本次 start 快照兜底，並要求當前供應商在當前配置下真正就緒（qwen 需 WorkspaceId）。
      const settingOn = Boolean(await getSetting('streamingDisplayEnabled', false))
      if (!this.isRunCurrent(runId)) return
      const realtime = (settingOn || Boolean(startOpts.streamingDisplay)) && isStreamingDisplayReady(asrProvider, qwenWorkspaceId)
      addRuntimeEvent('info', 'cloud_api', 'Streaming display decision', { asrProvider, settingOn, startOpt: Boolean(startOpts.streamingDisplay), hasWorkspace: Boolean(qwenWorkspaceId), realtime, runId })
      if (realtime) {
        // 先訂閱中間結果，避免建連後、訂閱前丟幀
        await this.subscribePartials(runId)
        if (!this.isRunCurrent(runId)) return
      }

      const commands = streamCommandsOf(asrProvider)
      // 不走流式的三種情形：這家沒有流式實現、沒開即時字幕（豆包除外）、
      // 或者這份配置還不就緒（千問 realtime 缺業務空間 ID）。
      // 一律回落到「錄完再發」，音訊在 pcmBuffers 裡有完整一份。
      if (!commands || (!realtime && !commands.streamWithoutRealtime)) {
        this.teardownPartials()
        return
      }
      this.streamCommands = commands

      const asrApiKey = await getSetting('cloudAsr.apiKey', '') as string
      const asrAppId = commands.needsAppId
        ? await getSetting('cloudAsr.appId', '') as string
        : ''
      const asrModel = commands.needsModel
        ? await getSetting('cloudAsr.model', '') as string
        : ''
      if (!this.isRunCurrent(runId)) return

      // 引數按各家 Rust open 的簽名給，多給的會被忽略但少給的會讓 open 失敗
      const openArgs: Record<string, unknown> = {
        config: {
          provider: asrProvider,
          api_key: asrApiKey,
          app_id: asrAppId,
          ...(asrModel && { extra: { model: asrModel } }),
        },
        hotwords: startOpts.hotwords ?? [],
        realtime,
      }
      if (commands.needsSampleRate) openArgs.sampleRate = 16000
      if (commands.needsWorkspaceId) openArgs.workspaceId = qwenWorkspaceId

      addRuntimeEvent('info', 'cloud_api', 'Streaming: connecting', {
        asrProvider,
        realtime,
        model: asrModel || '(provider default)',
        hasWorkspace: Boolean(qwenWorkspaceId),
      })
      const opened = await this.invokeNativeOpen(runId, commands.open, openArgs)
      if (!opened || !this.isRunCurrent(runId)) return
      this.streamReady = true
      addRuntimeEvent('info', 'cloud_api', 'Streaming: ready', { asrProvider })

      // 補發建連期間已快取的音訊
      await this.flushPendingChunks(runId)
      if (!this.isRunCurrent(runId)) return

      // 啟動定時器，每 200ms 批次傳送一次（收尾階段不再觸發新傳送）
      this.flushTimer = setInterval(() => {
        if (!this.isRunCurrent(runId) || this.streamFinishing) return
        if (this.streamReady && this.pendingChunks.length > 0) {
          void this.flushPendingChunks(runId)
        }
      }, 200)
    } catch (err) {
      if (!this.isRunCurrent(runId)) return
      addRuntimeEvent('warn', 'cloud_api', 'Streaming connection failed; falling back to buffered upload', { error: String(err) })
      this.streamCommands = null
      this.streamReady = false
      this.teardownPartials()
    }
  }

  /** 把一次批次傳送排入序列鏈路，返回可 await 的 Promise。
   *  鏈式序列保證多次 flush、以及收尾前的最終 flush 都嚴格按順序送達 Rust，
   *  絕不會出現音訊包穿插到負包之後。 */
  private flushPendingChunks(runId: number): Promise<void> {
    const run = async () => {
      if (!this.isRunCurrent(runId) || this.pendingChunks.length === 0) return
      const chunks = this.pendingChunks
      this.pendingChunks = []

      const totalLen = chunks.reduce((s, c) => s + c.byteLength, 0)
      const merged = new Uint8Array(totalLen)
      let offset = 0
      for (const chunk of chunks) {
        merged.set(new Uint8Array(chunk), offset)
        offset += chunk.byteLength
      }

      const b64 = uint8ArrayToBase64(merged)
      try {
        if (this.streamCommands) {
          await invoke(this.streamCommands.send, { pcmB64: b64 })
        }
      } catch (err) {
        addRuntimeEvent('warn', 'cloud_api', 'Streaming send failed', { error: String(err) })
      }
    }
    // 接到傳送鏈尾部，序列執行（無論前一個成功或失敗都繼續）
    this.sendLock = this.sendLock.then(run, run)
    return this.sendLock
  }

  // ── 處理邏輯 ──

  private async runProcess(
    runId: number,
    startOpts: Readonly<StartOptions>,
  ): Promise<void> {
    const stopTime = performance.now() // stop 時刻，用於計算流式模式的等待時間
    const startTime = this.streamStartTime || stopTime

    try {
      if (!this.isRunCurrent(runId)) return
      const totalBytes = this.pcmBuffers.reduce((sum, buf) => sum + buf.byteLength, 0)
      if (totalBytes === 0) {
        this.teardownPartials()
        this.callbacks.onDone?.()
        return
      }

      const durationSec = (totalBytes / 2) / 16000
      if (durationSec < 0.3) {
        addRuntimeEvent('info', 'cloud_api', 'Audio too short; skipped processing', { durationSec })
        this.teardownPartials()
        this.callbacks.onDone?.()
        return
      }

      // 讀取 ASR 配置
      const asrProvider = await getSetting('cloudAsr.provider', 'doubao') as string
      if (!this.isRunCurrent(runId)) return
      const isQwenOmni = isQwenOmniProvider(asrProvider)

      let asrText = ''
      let asrMs = 0
      /** 流式收尾拿到了結果；為 false 時走下面的一次性上傳 */
      let streamedOk = false

      if (this.streamCommands && this.streamReady) {
        // 流式收尾：先置收尾標誌（阻止新音訊入隊/定時器再發），停定時器，
        // flush 剩餘資料並等傳送鏈徹底排空，最後再發負包——保證負包是最後一個包。
        const commands = this.streamCommands
        this.streamFinishing = true
        if (this.flushTimer) { clearInterval(this.flushTimer); this.flushTimer = null }
        await this.flushPendingChunks(runId)
        await this.sendLock
        if (!this.isRunCurrent(runId)) return

        addRuntimeEvent('info', 'cloud_api', 'Streaming: sending finish', { command: commands.finish })
        const finishStart = performance.now()
        try {
          const text = await this.invokeNativeFinish(runId, commands.finish)
          if (text === undefined || !this.isRunCurrent(runId)) return
          asrText = text
          asrMs = Math.round(performance.now() - finishStart)
          streamedOk = true
          addRuntimeEvent('info', 'cloud_api', 'Streaming: recognition complete', { asrMs, textLen: asrText.length })
        } catch (err) {
          if (!this.isRunCurrent(runId)) return
          // 邊錄邊發的那條連線出了事（服務端提前結束會話、連線被重置、超時），但完整錄音
          // 還在 pcmBuffers 裡：直接改走下面的一次性上傳，等於自動做一遍「歷史裡重新識別」。
          // 以前這裡直接報失敗，使用者只能自己去歷史裡點重試（診斷包 61c93385：長錄音 4/10 失敗，
          // 歷史重試全部成功）。只重試一次，一次性上傳也失敗就照常報錯。
          addRuntimeEvent('warn', 'cloud_api', 'Streaming finish failed; retrying with buffered upload', {
            error: String(err),
            durationSec,
            command: commands.finish,
          })
          this.streamCommands = null
          this.streamReady = false
        }
      }

      if (!streamedOk) {
        // 沒走流式（這家不支援 / 沒開字幕 / 建連失敗），或者流式收尾失敗：錄完再發
        const merged = new Uint8Array(totalBytes)
        let offset = 0
        for (const buf of this.pcmBuffers) {
          merged.set(new Uint8Array(buf), offset)
          offset += buf.byteLength
        }
        const asrApiKey = await getSetting('cloudAsr.apiKey', '') as string
        const asrAppId = await getSetting('cloudAsr.appId', '') as string
        const asrModel = await getSetting('cloudAsr.model', '') as string
        if (!this.isRunCurrent(runId)) return

        let omniInstructions: string | undefined
        if (isQwenOmni) {
          const savedPrompt = await getSetting('cloudAsr.omniSystemPrompt', '') as string
          if (!this.isRunCurrent(runId)) return
          omniInstructions = savedPrompt || undefined
        }

        // 地址自己填的那兩張卡才有值；其餘卡片這個鍵是空串（見 asrEndpointUrl）
        const asrBaseUrl = await getSetting('cloudAsr.baseUrl', '') as string
        const asrProtocol = await getSetting('cloudAsr.protocol', 'auto') as string
        if (!this.isRunCurrent(runId)) return

        const extra = buildAsrExtra(asrProvider, {
          model: asrModel,
          instructions: omniInstructions,
          baseUrl: asrBaseUrl,
          protocol: asrProtocol,
        })
        const asrConfig: AsrProviderConfig = {
          provider: isQwenOmni ? 'qwen_omni' : asrProvider,
          api_key: asrApiKey,
          app_id: asrAppId,
          ...(extra && { extra }),
        }

        addRuntimeEvent('info', 'cloud_api', 'ASR started', {
          provider: asrProvider,
          model: extra?.model ?? '(provider default)',
          durationSec,
        })
        const asrResult = await cloudTranscribeRaw<AsrResult>(merged, {
          sample_rate: 16000,
          asr_config: asrConfig,
          hotwords: startOpts.hotwords ?? [],
        })
        // 「OpenAI 相容」的 auto 協議是在這條呼叫**內部**探測出來的（結果進
        // asr_openai_compat 的 PROTOCOL_CACHE）。熱詞頁只在掛載時查過一次能力，
        // 不廣播的話它會一直掛著"協議還沒探測出來"，而答案其實已經有了。
        // 放在代次檢查之前：探測快取是程序級的，這一輪作不作廢都不影響它已經更新。
        notifyAsrCapabilityMaybeChanged()
        if (!this.isRunCurrent(runId)) return
        asrText = asrResult.text
        asrMs = asrResult.elapsed_ms
      }

      this.pcmBuffers = []
      // ASR 已拿到最終文本，後續不再有中間結果，撤下監聽
      this.teardownPartials()

      // 還原被 ASR 拆開加空格的無空格熱詞（如豆包把 "SayIt" 識別成 "Say It"）
      asrText = restoreHotwordSpacing(asrText, startOpts.hotwords ?? [])

      // 傳送 ASR 中間結果
      this.callbacks.onASR?.({ text: asrText, asrMs, durationSec })

      // 與其它模式共用同一份判據。isQwenOmni 走 integrated_asr 路由：它表達的是
      // "識別引擎自帶整理，沒有另外調獨立 AI"，而不是"使用者的預設已經執行過"。
      const policy = policyFromSnapshot(startOpts.aiConfig, 'cloud_api', durationSec, isQwenOmni)
      const outcomeContext: AiOutcomeContext = {
        operationId: startOpts.operationId || `cloud-${runId}`,
        trigger: startOpts.source === 'history_reprocess' ? 'history_reprocess' : 'live',
      }

      if (!asrText.trim()) {
        const outcome = resolveAndLogAiOutcome(outcomeContext, policy, { asrTextEmpty: true })
        this.callbacks.onFinal?.({
          asrText: '',
          llmText: '',
          asrMs,
          llmMs: 0,
          durationSec,
          aiSource: outcome.source,
          aiStatus: outcome.status,
        })
        this.callbacks.onDone?.()
        return
      }

      // AI 校對（Qwen Omni 已內建 AI，由 policy 判成不允許呼叫，這裡走同一條返回）
      const polish = policy.route === 'integrated_asr'
        ? (() => {
          const outcome = resolveAndLogAiOutcome(outcomeContext, policy)
          return {
            llmText: startOpts.textContext?.selectedText || asrText,
            llmMs: 0,
            contextApplied: startOpts.textContext ? false : undefined,
            aiSource: outcome.source,
            aiStatus: outcome.status,
          }
        })()
        : await polishWithClientAi({
          asrText,
          startOptions: startOpts,
          policy,
          outcomeContext,
          logSource: 'cloud_api',
          isCurrent: () => this.isRunCurrent(runId),
        })
      if (!polish || !this.isRunCurrent(runId)) return

      const totalMs = Math.round(performance.now() - startTime)
      addRuntimeEvent('info', 'cloud_api', 'Processing complete', {
        durationSec,
        asrMs,
        llmMs: polish.llmMs,
        totalMs,
        runId,
      })

      const omniModel = isQwenOmni ? resolveQwenOmniModel(asrProvider) : undefined

      this.callbacks.onFinal?.({
        asrText,
        asrMs,
        durationSec,
        ...polish,
        ...(isQwenOmni && { asrEngine: 'qwen_omni', asrModel: omniModel }),
      })
      this.callbacks.onDone?.()
    } catch (err) {
      if (!this.isRunCurrent(runId)) return
      addRuntimeEvent('error', 'cloud_api', 'Processing failed', { error: String(err) })
      this.teardownPartials()
      this.callbacks.onError?.(String(err))
      this.callbacks.onDone?.()
    } finally {
      this.completeRun(runId)
    }
  }
}
