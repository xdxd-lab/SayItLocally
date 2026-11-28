export interface DebugMessage {
  time: number
  direction: 'sent' | 'received'
  type: string
  data: unknown
}

export interface DebugSession {
  id: string
  startTime: number
  endTime?: number
  systemPrompt?: string
  sampleRate?: number
  messages: DebugMessage[]
  audioChunks: ArrayBuffer[]  // raw PCM Int16 buffers sent to backend
}

export interface RuntimeEvent {
  time: number
  level: 'info' | 'warn' | 'error'
  source: string
  message: string
  detail?: unknown
}

const MAX_SESSIONS = 80
const MAX_RUNTIME_EVENTS = 120
const MAX_MESSAGES_PER_SESSION = 40
const DEFAULT_PCM_SAMPLE_RATE = 16000
const MAX_AUDIO_BYTES_PER_SESSION = 256 * 1024
const MAX_AUDIO_BYTES_TOTAL = 2 * 1024 * 1024
const ENABLE_INFO_CONSOLE = false

// 刻意**不**放行 'Paste decision'：它帶十幾個欄位，而插字絕大多數是成功的，常態化
// 落盤只會把日誌沖淡，真正要看的那幾行反而更難找。插字排查需要的資訊改為掛在
// 下面三條事件的 gate 欄位上（成功 / 插了但失敗 / 沒敢插各一條），失敗那條另外帶
// 完整的四層判據取值。判據是"按需詳細"：平時一行，出事那次才記全。
const RECORDER_KEY_EVENT = /(Recording started|Recording stopped|Entered processing|Final result received|External text insertion succeeded|External text insertion failed|Processing timed out|Showing fallback card)/i
const WEBSOCKET_KEY_EVENT = /(Connection closed|Connection timed out|Failed to send start|Failed to send stop|Connecting|Connected|Reconnected|Ready received|disconnect)/i
/**
 * 採集鏈路的關鍵節點。放行它們是為了能回答一個此前查不到的問題：
 * 「這次錄音，麥克風到底有沒有把資料交上來、交上來的是不是全 0」。
 *
 * 這條通道缺失過一次代價很大：使用者報「休眠喚醒後一直提示未檢測到聲音、重啟軟體無效」，
 * 而 audio 的 info 事件全被這裡過濾掉，日誌裡既看不到 track 的 muted/readyState，
 * 也看不到有沒有收到過 PCM —— 只能靠讀程式碼猜，猜不出壞在哪一層。
 * 量很小（每次錄音 4~6 條），換來的是一份日誌就能定性。
 */
const AUDIO_KEY_EVENT = /(Microphone capture started|AudioContext|First PCM frame received|First RMS received|ScriptProcessorNode fallback activated|Capture stop summary)/i
const INSERTION_EVENT = /(Paste decision|External text insertion|fallback|Target is SayIt|Target is not editable)/i

/**
 * AI 整理鏈路的固定事件名。**按事件名放行，不按英文文案正則**。
 *
 * 上面幾條走正則是歷史包袱：日誌能不能落盤取決於有沒有人改動英文文案，改一個詞
 * 就靜默失去一條診斷線索。這兩條是"沒有它就查不出為什麼沒整理"的關鍵事件，
 * 用不隨文案變化的穩定名字，並且只放行這兩個 —— 不放行整個 'ai' 來源，
 * 也不借用已有的 'backend'（借了以後就分不清是後端還是 AI 的事）。
 *
 * 量：正常跳過每次錄音 1 條；真的調了自配 AI 是 2 條。
 */
export const AI_LOG_SOURCE = 'ai'
export const AI_EVENT_REQUEST = 'ai.request'
export const AI_EVENT_OUTCOME = 'ai.outcome'
const AI_KEY_EVENTS = new Set<string>([AI_EVENT_REQUEST, AI_EVENT_OUTCOME])

let totalAudioBytes = 0

function shouldMirrorPayload(payload: unknown): boolean {
  if (!payload || typeof payload !== 'object') return false
  const value = payload as Record<string, unknown>

  if (value.kind === 'runtime') {
    const level = value.level
    const source = typeof value.source === 'string' ? value.source : ''
    const message = typeof value.message === 'string' ? value.message : ''

    if (level === 'error' || level === 'warn') return true
    if (level !== 'info') return false

    return source === 'recorder' && RECORDER_KEY_EVENT.test(message)
      || source === 'websocket' && WEBSOCKET_KEY_EVENT.test(message)
      || source === 'audio' && AUDIO_KEY_EVENT.test(message)
      || source === AI_LOG_SOURCE && AI_KEY_EVENTS.has(message)
      || source === 'backend'
      // update 全放行：整條更新鏈路是使用者**看不見**的（後台檢查、後台下載、退出時安裝），
      // 出問題時日誌是唯一線索。量也極小：啟動一次 + 每 6 小時一次。
      // 少了這條，"沒有新版本"和"更新服務壓根沒跑起來"在日誌里長得一模一樣。
      || source === 'update'
  }

  if (value.kind === 'ws_message') {
    const type = typeof value.type === 'string' ? value.type : ''
    return type === 'stop' || type === 'final' || type === 'done' || type === 'error'
  }

  return value.kind === 'session_start' || value.kind === 'session_end'
}

function shouldKeepRuntimeEvent(event: RuntimeEvent): boolean {
  if (event.level === 'error' || event.level === 'warn') return true
  if (event.source === 'backend') return true
  // 必須和 shouldMirrorPayload 同步放行：兩套判據分開寫過一次，結果出現"記憶體裡有、
  // 日誌裡沒有"，而排查用的是日誌。
  if (event.source === AI_LOG_SOURCE) return AI_KEY_EVENTS.has(event.message)
  // 見 shouldMirrorPayload 裡的同名分支：更新鏈路使用者看不見，診斷只能靠日誌
  if (event.source === 'update') return true
  if (event.source === 'websocket') {
    return WEBSOCKET_KEY_EVENT.test(event.message)
  }
  if (event.source === 'recorder') {
    return RECORDER_KEY_EVENT.test(event.message)
  }
  if (event.source === 'audio') {
    return AUDIO_KEY_EVENT.test(event.message)
  }
  return false
}

function shouldKeepMessage(type: string): boolean {
  return type === 'start' || type === 'stop' || type === 'ready' || type === 'final' || type === 'done' || type === 'error'
}

let sessions: DebugSession[] = []
let current: DebugSession | null = null
/** current.audioChunks 的總位元組數，跟著 current 一起重置。 */
let currentAudioBytes = 0
let runtimeEvents: RuntimeEvent[] = []

import { appendDebugLog } from './bridge'

function mirrorToMainLog(payload: unknown) {
  if (!shouldMirrorPayload(payload)) return
  try {
    appendDebugLog(payload)
  } catch {
    // ignore logging errors
  }
}

function trimSessions() {
  if (sessions.length > MAX_SESSIONS) {
    const removed = sessions.slice(MAX_SESSIONS)
    totalAudioBytes -= removed.reduce((sum, session) => (
      sum + session.audioChunks.reduce((chunkSum, chunk) => chunkSum + chunk.byteLength, 0)
    ), 0)
    sessions = sessions.slice(0, MAX_SESSIONS)
  }
}

function pushRuntimeEvent(event: RuntimeEvent) {
  if (!shouldKeepRuntimeEvent(event)) return
  runtimeEvents.unshift(event)
  if (runtimeEvents.length > MAX_RUNTIME_EVENTS) {
    runtimeEvents = runtimeEvents.slice(0, MAX_RUNTIME_EVENTS)
  }
}

export function addRuntimeEvent(
  level: 'info' | 'warn' | 'error',
  source: string,
  message: string,
  detail?: unknown,
) {
  const event: RuntimeEvent = {
    time: Date.now(),
    level,
    source,
    message,
    detail,
  }

  pushRuntimeEvent(event)

  const logPrefix = `[${source}] ${message}`
  if (level === 'error') {
    console.error(logPrefix, detail)
  } else if (level === 'warn') {
    console.warn(logPrefix, detail)
  } else if (ENABLE_INFO_CONSOLE) {
    console.log(logPrefix, detail)
  } else if (source === 'recorder' && INSERTION_EVENT.test(message)) {
    // Always log paste-related info events for debugging insertion failures
    console.log(logPrefix, detail)
  }
  mirrorToMainLog({
    kind: 'runtime',
    level,
    source,
    message,
    detail,
    time: event.time,
  })

  // Keep the full runtime trail inside the active session for post-mortem debugging.
  if (current) {
    current.messages.push({
      time: event.time,
      direction: 'received',
      type: level === 'error' ? 'error' : 'runtime',
      data: {
        source,
        message,
        detail,
      },
    })
    if (current.messages.length > MAX_MESSAGES_PER_SESSION) {
      current.messages = current.messages.slice(-MAX_MESSAGES_PER_SESSION)
    }
  }
}

export function startSession(opts?: { systemPrompt?: string }) {
  const sessionId = Date.now().toString(36) + Math.random().toString(36).slice(2, 5)
  current = {
    id: sessionId,
    startTime: Date.now(),
    systemPrompt: opts?.systemPrompt,
    sampleRate: DEFAULT_PCM_SAMPLE_RATE,
    messages: [],
    audioChunks: [],
  }
  currentAudioBytes = 0

  sessions.unshift(current)
  trimSessions()
  mirrorToMainLog({
    kind: 'session_start',
    sessionId,
    hasSystemPrompt: Boolean(opts?.systemPrompt),
  })
}

export function endSession() {
  if (current) {
    const endedId = current.id
    const endTime = Date.now()
    current.endTime = Date.now()
    mirrorToMainLog({
      kind: 'session_end',
      sessionId: endedId,
      durationMs: endTime - current.startTime,
      messageCount: current.messages.length,
      audioChunks: current.audioChunks.length,
    })
    current = null
  }
}

export function hasActiveSession() {
  return !!current
}

export function addMsg(direction: 'sent' | 'received', type: string, data: unknown) {
  const time = Date.now()
  // 對 start 訊息中的敏感欄位脫敏後再記錄
  const sanitizedData = (type === 'start' && data && typeof data === 'object')
    ? sanitizeStartMessage(data as Record<string, unknown>)
    : data
  if (current && shouldKeepMessage(type)) {
    current.messages.push({ time, direction, type, data: sanitizedData })
    if (current.messages.length > MAX_MESSAGES_PER_SESSION) {
      current.messages = current.messages.slice(-MAX_MESSAGES_PER_SESSION)
    }
  }
  mirrorToMainLog({
    kind: 'ws_message',
    direction,
    type,
    time,
    data: sanitizedData,
  })
}

export function addAudioChunk(buffer: ArrayBuffer) {
  if (!current) return
  if (totalAudioBytes >= MAX_AUDIO_BYTES_TOTAL) return
  // 計數而不是每幀把已存的塊重新加一遍：錄音期間這裡每發一個音訊包就調一次。
  const nextBytes = currentAudioBytes + buffer.byteLength
  if (nextBytes > MAX_AUDIO_BYTES_PER_SESSION) return
  const copy = buffer.slice(0)
  current.audioChunks.push(copy)
  currentAudioBytes = nextBytes
  totalAudioBytes += copy.byteLength
}

export function getSessions(): DebugSession[] {
  return sessions
}

export function clearSessions() {
  sessions = []
  current = null
  totalAudioBytes = 0
}

export function getRuntimeEvents(): RuntimeEvent[] {
  return runtimeEvents
}

export function clearRuntimeEvents() {
  runtimeEvents = []
}

export function getSessionAudioBlob(session: DebugSession): Blob | null {
  if (session.audioChunks.length === 0) return null

  const totalLen = session.audioChunks.reduce((s, c) => s + c.byteLength, 0)
  if (totalLen === 0) return null

  const pcm = new Uint8Array(totalLen)
  let offset = 0
  for (const chunk of session.audioChunks) {
    pcm.set(new Uint8Array(chunk), offset)
    offset += chunk.byteLength
  }

  const sessionRate = Number(session.sampleRate || DEFAULT_PCM_SAMPLE_RATE)
  const sampleRate = Number.isFinite(sessionRate) && sessionRate >= 8000
    ? Math.round(sessionRate)
    : DEFAULT_PCM_SAMPLE_RATE

  return createWavBlob(pcm, sampleRate, 16, 1)
}

function createWavBlob(pcmData: Uint8Array, sampleRate: number, bitsPerSample: number, channels: number): Blob {
  const dataLen = pcmData.byteLength
  const buffer = new ArrayBuffer(44 + dataLen)
  const view = new DataView(buffer)

  writeStr(view, 0, 'RIFF')
  view.setUint32(4, 36 + dataLen, true)
  writeStr(view, 8, 'WAVE')

  writeStr(view, 12, 'fmt ')
  view.setUint32(16, 16, true)
  view.setUint16(20, 1, true)
  view.setUint16(22, channels, true)
  view.setUint32(24, sampleRate, true)
  view.setUint32(28, sampleRate * channels * bitsPerSample / 8, true)
  view.setUint16(32, channels * bitsPerSample / 8, true)
  view.setUint16(34, bitsPerSample, true)

  writeStr(view, 36, 'data')
  view.setUint32(40, dataLen, true)
  new Uint8Array(buffer, 44).set(pcmData)

  return new Blob([buffer], { type: 'audio/wav' })
}

function writeStr(view: DataView, offset: number, str: string) {
  for (let i = 0; i < str.length; i++) {
    view.setUint8(offset + i, str.charCodeAt(i))
  }
}

/** 對 WebSocket start 訊息中的敏感欄位脫敏 */
function sanitizeStartMessage(data: Record<string, unknown>): Record<string, unknown> {
  const result = { ...data }

  // system_prompt 截斷到 50 字元
  if (typeof result.system_prompt === 'string' && result.system_prompt.length > 50) {
    result.system_prompt = result.system_prompt.slice(0, 50) + '...[truncated]'
  }

  // client_meta 中移除敏感字段
  if (result.client_meta && typeof result.client_meta === 'object') {
    const meta = { ...(result.client_meta as Record<string, unknown>) }
    delete meta.local_ip
    delete meta.user_name
    delete meta.hostname
    result.client_meta = meta
  }

  // Editor text is ephemeral and must never enter runtime logs/debug exports. Keep only enough
  // metadata to verify whether capture worked on a user's machine.
  if (result.text_context && typeof result.text_context === 'object') {
    const context = result.text_context as Record<string, unknown>
    result.text_context = {
      source: String(context.source || ''),
      before_len: String(context.text_before || '').length,
      selected_len: String(context.selected_text || '').length,
      after_len: String(context.text_after || '').length,
    }
  }

  return result
}
