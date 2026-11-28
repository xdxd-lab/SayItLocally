// Audio capture service — AudioWorklet with ScriptProcessorNode fallback.
// WebView2 on Windows has known issues where AudioWorklet's process() never
// fires despite addModule() succeeding.  We detect this and fall back to the
// deprecated-but-reliable ScriptProcessorNode.

import { addRuntimeEvent } from './debugLog'

let audioCtx: AudioContext | null = null
let workletNode: AudioWorkletNode | null = null
let scriptNode: ScriptProcessorNode | null = null
let mediaStream: MediaStream | null = null
let sourceNode: MediaStreamAudioSourceNode | null = null
let onAudioData: ((buffer: ArrayBuffer) => void) | null = null
let onVolumeChange: ((volume: number) => void) | null = null
let onPCMFrame: ((pcm: Int16Array) => void) | null = null
let actualSampleRate = 16000
let firstPCMFrameLogged = false
let firstRmsLogged = false
let usingFallback = false

const TARGET_SAMPLE_RATE = 16000

/**
 * 一個輸入端點。只留判斷"這是哪個麥克風"用得到的三個欄位，不用 MediaDeviceInfo
 * 本身 —— 那是帶方法的宿主物件，單測裡構造起來囉嗦。
 */
export interface MicEndpoint {
  deviceId: string
  groupId: string
  label: string
}

/**
 * Chromium 在真實端點之外還擺兩個「偽裝置」：deviceId 固定為 default /
 * communications，label 是 `<本地化字首> - <真實端點名>`（中文系統是「預設值 - 」）。
 * 它們不是麥克風，只是「跟著系統預設走」這條路由。
 *
 * 放在這裡 export 而不是放在 micSourceReminder 裡：設定頁的裝置列表和懸浮窗的
 * 來源提示都要認它，而 audio.ts 不能反向 import recorder 下的模組（會成環）。
 */
export function isPseudoInputDevice(deviceId: string): boolean {
  const id = deviceId.trim().toLowerCase()
  return !id || id === 'default' || id === 'communications'
}

/**
 * 規範化設定裡存的「選中的麥克風」。
 *
 * 偽裝置 id 一律折成空串，也就是「跟隨系統預設」—— 兩者語義完全一樣（`getUserMedia`
 * 收到 `deviceId: 'default'` 和收到不帶 deviceId 的約束，解析結果是同一個端點）。
 *
 * 必須折：偽裝置已經不在 listMicrophones 的結果裡了，留著會讓設定頁的下拉找不到
 * 選中項、退回佔位符文案，看起來像"沒有選擇麥克風"。實測存量資料裡確實有
 * `selectedMic = "default"` —— 老版本的下拉把偽裝置也列出來，使用者點了它。
 */
export function normalizeSelectedMicId(raw: unknown): string {
  const id = typeof raw === 'string' ? raw.trim() : ''
  return isPseudoInputDevice(id) ? '' : id
}

/**
 * 剝掉裝置名結尾的 USB 標識（`(047f:c053)` 這種 VID:PID）。
 *
 * 它對辨認裝置毫無幫助：同一型號的兩台裝置 VID:PID 完全一樣，連"區分同型號"都做不到，
 * 只是白佔寬度、把真正有用的型號名擠出可視範圍。裝置名裡剩下的兩段都要留 ——
 * 主名（`耳機式麥克風`）說明是什麼，括號裡的型號（`Plantronics Blackwire 5220 Series`）
 * 才是區分裝置的依據。
 */
export function stripUsbIds(label: string): string {
  return label.replace(/\s*\([0-9a-f]{4}:[0-9a-f]{4}\)\s*$/i, '').trim()
}

/** 列表裡真正的麥克風：排掉偽裝置，也排掉讀不到名字的條目（沒名字無從辨認）。 */
export function realInputEndpoints<T extends MicEndpoint>(devices: T[]): T[] {
  return devices.filter((d) => !isPseudoInputDevice(d.deviceId) && d.label.trim().length > 0)
}

/**
 * 把一條「跟隨系統預設」的偽裝置落到它當下實際指向的那個真實端點。
 *
 * 兩級判據，都與介面語言無關（**不要退化成按「預設值 - 」這類文案做匹配**，那等於
 * 給每種介面語言維護一份，Windows 改寫法還會靜默失效）：
 *  1. groupId 相同 —— 偽裝置與它指向的真實端點同組；
 *  2. 退一步按字尾匹配 —— 偽裝置的 label 就是「字首 + 真實 label」，所以真實 label
 *     一定是它的字尾。有些機器上偽裝置的 groupId 是空的，只能靠這一層。
 *
 * 兩個呼叫方：設定頁用它回答「系統預設現在是哪個裝置」，懸浮窗的來源提示用它把
 * 帶字首的名字換成真名。判據只此一份，別再各寫一遍。
 */
export function matchRealEndpoint<T extends MicEndpoint>(
  hint: MicEndpoint,
  devices: T[],
): T | null {
  const pool = realInputEndpoints(devices)
  if (pool.length === 0) return null

  if (!isPseudoInputDevice(hint.deviceId)) {
    const exact = hint.deviceId.trim()
    return pool.find((d) => d.deviceId.trim() === exact) ?? null
  }

  const groupId = hint.groupId.trim()
  if (groupId) {
    const byGroup = pool.find((d) => d.groupId.trim() === groupId)
    if (byGroup) return byGroup
  }

  const label = hint.label.trim()
  if (!label) return null
  return pool.find((d) => {
    const candidate = d.label.trim()
    return candidate.length > 0 && label.endsWith(candidate)
  }) ?? null
}

/** The microphone endpoint that getUserMedia actually opened. */
export interface ActiveMicrophoneInfo {
  deviceId: string
  groupId: string
  label: string
  /**
   * 與本次採集同一時刻的輸入端點快照。
   *
   * 為什麼要帶上它：`label` 是 Chromium 給的，"跟隨系統預設"這條路由上它是
   * `預設值 - 端點名 (父裝置名)`，兩頭都是噪音。要把它收成人能看懂的一段，需要
   * 知道同一時刻還有哪些端點（見 micSourceReminder.describeMicSource）。
   *
   * 必須在流開啟之後列舉：沒有活躍流、或沒有持久麥克風許可權時，
   * `enumerateDevices()` 返回的條目 label 全是空的（見 listMicrophones 的註釋），
   * 那種快照對同名判斷毫無用處。
   */
  devices: MicEndpoint[]
}

// HMR cleanup: tear down audio capture when module is hot-replaced
if ((import.meta as unknown as Record<string, unknown>).hot) {
  const hot = (import.meta as unknown as Record<string, unknown>).hot as { dispose: (cb: () => void) => void }
  hot.dispose(() => {
    console.log('[audio] HMR dispose: tearing down audio capture')
    if (workletNode) {
      try { workletNode.port.onmessage = null } catch { /* ignore */ }
      try { workletNode.disconnect() } catch { /* ignore */ }
      workletNode = null
    }
    if (scriptNode) {
      try { scriptNode.onaudioprocess = null } catch { /* ignore */ }
      try { scriptNode.disconnect() } catch { /* ignore */ }
      scriptNode = null
    }
    if (sourceNode) {
      try { sourceNode.disconnect() } catch { /* ignore */ }
      sourceNode = null
    }
    if (mediaStream) {
      mediaStream.getTracks().forEach((track) => track.stop())
      mediaStream = null
    }
    if (audioCtx && audioCtx.state !== 'closed') {
      audioCtx.close().catch(() => { })
    }
    audioCtx = null
    onAudioData = null
    onVolumeChange = null
    onPCMFrame = null
  })
}

export function getActualSampleRate(): number {
  return actualSampleRate
}

const PCM_WORKLET_CODE = `
class PCMProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const opts = options.processorOptions || {};
    this.targetRate = opts.targetRate || 16000;
    this.inputRate = opts.inputRate || sampleRate || 48000;
    this.ratio = this.inputRate / this.targetRate;
    this.tail = new Float32Array(0);
    this.phase = 0;
  }

  process(inputs) {
    const input = inputs[0]?.[0];
    if (!input || input.length === 0) {
      return true;
    }

    let sum = 0;
    for (let i = 0; i < input.length; i++) {
      const s = Math.max(-1, Math.min(1, input[i]));
      sum += s * s;
    }
    const rms = Math.sqrt(sum / input.length);

    const merged = new Float32Array(this.tail.length + input.length);
    merged.set(this.tail, 0);
    merged.set(input, this.tail.length);

    let outFloat;

    if (Math.abs(this.ratio - 1) < 0.0001) {
      outFloat = merged;
      this.tail = new Float32Array(0);
      this.phase = 0;
    } else {
      const available = merged.length - this.phase;
      const outLen = Math.floor(available / this.ratio);

      if (outLen <= 0) {
        this.tail = merged;
        this.port.postMessage({ rms, sampleRate: this.targetRate });
        return true;
      }

      outFloat = new Float32Array(outLen);
      for (let i = 0; i < outLen; i++) {
        const pos = this.phase + i * this.ratio;
        const i0 = Math.floor(pos);
        const i1 = Math.min(i0 + 1, merged.length - 1);
        const frac = pos - i0;
        outFloat[i] = merged[i0] * (1 - frac) + merged[i1] * frac;
      }

      const consumed = this.phase + outLen * this.ratio;
      const keepFrom = Math.floor(consumed);
      this.phase = consumed - keepFrom;
      this.tail = keepFrom < merged.length ? merged.slice(keepFrom) : new Float32Array(0);
    }

    const int16 = new Int16Array(outFloat.length);
    for (let i = 0; i < outFloat.length; i++) {
      const s = Math.max(-1, Math.min(1, outFloat[i]));
      int16[i] = s < 0 ? s * 32768 : s * 32767;
    }

    this.port.postMessage({ pcm: int16.buffer, rms, sampleRate: this.targetRate }, [int16.buffer]);
    return true;
  }
}

registerProcessor('pcm-processor', PCMProcessor);
`

/**
 * 輸入端點快照。**只在有活躍流時呼叫才有意義**（見 ActiveMicrophoneInfo.devices）。
 *
 * 與 listMicrophones 的區別：這裡絕不為了拿名字去開臨時流 —— 它跑在錄音啟動路徑上，
 * 那時候已經有一路採集在跑，再開一路是真實故障。失敗一律退化成空快照，
 * 呼叫方會落到"只顯示端點名"這條兜底，不影響採集。
 */
async function snapshotInputEndpoints(): Promise<MicEndpoint[]> {
  try {
    const list = await navigator.mediaDevices.enumerateDevices()
    return list
      .filter((d) => d.kind === 'audioinput')
      .map((d) => ({
        deviceId: String(d.deviceId || ''),
        groupId: String(d.groupId || ''),
        label: String(d.label || ''),
      }))
  } catch {
    return []
  }
}

// Keep Chromium's pseudo-devices: their groupId can identify the current default.
export async function listMicrophones(): Promise<MediaDeviceInfo[]> {
  return (await navigator.mediaDevices.enumerateDevices()).filter((device) => device.kind === 'audioinput')
}

// Settings refreshes its list passively when an actual recording opens a microphone.
export const microphoneOpened = new EventTarget()

function createAudioContext() {
  const AudioContextCtor = window.AudioContext || (window as Window & { webkitAudioContext?: typeof AudioContext }).webkitAudioContext
  if (!AudioContextCtor) {
    throw new Error('Current browser does not support AudioContext')
  }
  // 優先直接開 16 kHz 的 AudioContext：拿到的話瀏覽器會用自己的高質量重取樣器，
  // 我們下面那套手寫線性插值就可以整段跳過。線性插值沒有抗混疊低通，48k→16k
  // 是 3 倍抽取，8~24 kHz 的能量會折回 0~8 kHz，直接損害識別準確度。
  // 不是所有 WebView2/瀏覽器都接受 sampleRate 約束，所以拿不到就回退原路徑。
  for (const opts of [
    { latencyHint: 'interactive', sampleRate: TARGET_SAMPLE_RATE, sinkId: { type: 'none' } },
    { latencyHint: 'interactive', sampleRate: TARGET_SAMPLE_RATE },
    { latencyHint: 'interactive', sinkId: { type: 'none' } },
    { latencyHint: 'interactive' },
  ] as unknown as AudioContextOptions[]) {
    try {
      return new AudioContextCtor(opts)
    } catch {
      // 該組合不被支援，試下一個
    }
  }
  return new AudioContextCtor()
}

// [自訂] SC-62：停止錄音時不等 AudioContext 關閉（藍牙或切換取樣率時可能很久），
// 辨識可以先開始；下一次開始錄音前先等這裡的關閉完成。
let pendingClose: Promise<void> = Promise.resolve()

// [自訂] SC-62 補充（F10b）：close() 永遠不完成時最多等這麼久就照常開錄並記一筆。
// 舊 context 的資料路徑在 releaseCapture 已同步切斷、麥克風也已停止，不會把音訊交給新的一輪。
export const CLOSE_WAIT_MS = 1500

async function teardownCapture() {
  releaseCapture()
  const waiting = pendingClose
  let timer: ReturnType<typeof setTimeout> | undefined
  const timedOut = await Promise.race([
    waiting.then(() => false),
    new Promise<boolean>(resolve => { timer = setTimeout(() => resolve(true), CLOSE_WAIT_MS) }),
  ])
  clearTimeout(timer)
  if (timedOut) {
    console.warn('[audio-diag] previous AudioContext close timed out; starting capture anyway')
    addRuntimeEvent('warn', 'audio', 'Previous AudioContext close timed out; starting capture anyway', {
      waitedMs: CLOSE_WAIT_MS,
    })
    // 不再等這一個，也不讓它一路串到之後每次開錄。
    if (pendingClose === waiting) pendingClose = Promise.resolve()
  }
}

/**
 * 立即切斷資料路徑並停止麥克風：之後不會再有 PCM 交給呼叫端。
 * AudioContext 的關閉在背景進行，由 pendingClose 追蹤。
 */
function releaseCapture() {
  if (workletNode) {
    try { workletNode.port.onmessage = null } catch { /* ignore */ }
    try { workletNode.disconnect() } catch { /* ignore */ }
    workletNode = null
  }

  if (scriptNode) {
    try { scriptNode.onaudioprocess = null } catch { /* ignore */ }
    try { scriptNode.disconnect() } catch { /* ignore */ }
    scriptNode = null
  }

  if (sourceNode) {
    try { sourceNode.disconnect() } catch { /* ignore */ }
    sourceNode = null
  }

  if (mediaStream) {
    mediaStream.getTracks().forEach((track) => track.stop())
    mediaStream = null
  }

  const ctx = audioCtx
  audioCtx = null
  actualSampleRate = TARGET_SAMPLE_RATE
  if (ctx && ctx.state !== 'closed') {
    let closing: Promise<void>
    try {
      closing = ctx.close().catch(() => { })
    } catch {
      closing = Promise.resolve()
    }
    pendingClose = Promise.all([pendingClose, closing]).then(() => { })
  }
}

// ── Resampler state for ScriptProcessorNode fallback ──
let spnTail = new Float32Array(0)
let spnPhase = 0

/** Resample + convert to Int16 (same algorithm as the AudioWorklet) */
function resampleToInt16(input: Float32Array, inputRate: number): { pcm: Int16Array; rms: number } {
  const ratio = inputRate / TARGET_SAMPLE_RATE

  let sum = 0
  for (let i = 0; i < input.length; i++) {
    const s = Math.max(-1, Math.min(1, input[i]))
    sum += s * s
  }
  const rms = Math.sqrt(sum / input.length)

  const merged = new Float32Array(spnTail.length + input.length)
  merged.set(spnTail, 0)
  merged.set(input, spnTail.length)

  let outFloat: Float32Array

  if (Math.abs(ratio - 1) < 0.0001) {
    outFloat = merged
    spnTail = new Float32Array(0)
    spnPhase = 0
  } else {
    const available = merged.length - spnPhase
    const outLen = Math.floor(available / ratio)

    if (outLen <= 0) {
      spnTail = merged
      return { pcm: new Int16Array(0), rms }
    }

    outFloat = new Float32Array(outLen)
    for (let i = 0; i < outLen; i++) {
      const pos = spnPhase + i * ratio
      const i0 = Math.floor(pos)
      const i1 = Math.min(i0 + 1, merged.length - 1)
      const frac = pos - i0
      outFloat[i] = merged[i0] * (1 - frac) + merged[i1] * frac
    }

    const consumed = spnPhase + outLen * ratio
    const keepFrom = Math.floor(consumed)
    spnPhase = consumed - keepFrom
    spnTail = keepFrom < merged.length ? merged.slice(keepFrom) : new Float32Array(0)
  }

  const int16 = new Int16Array(outFloat.length)
  for (let i = 0; i < outFloat.length; i++) {
    const s = Math.max(-1, Math.min(1, outFloat[i]))
    int16[i] = s < 0 ? s * 32768 : s * 32767
  }

  return { pcm: int16, rms }
}

/** Wire up ScriptProcessorNode as fallback when AudioWorklet fails */
function setupScriptProcessorFallback(ctx: AudioContext, src: MediaStreamAudioSourceNode) {
  usingFallback = true
  spnTail = new Float32Array(0)
  spnPhase = 0
  actualSampleRate = TARGET_SAMPLE_RATE

  // Disconnect worklet if it was connected
  if (workletNode) {
    try { workletNode.port.onmessage = null } catch { /* ignore */ }
    try { workletNode.disconnect() } catch { /* ignore */ }
    try { src.disconnect(workletNode) } catch { /* ignore */ }
    workletNode = null
  }

  // 4096 samples buffer — good balance between latency and efficiency
  const spn = ctx.createScriptProcessor(4096, 1, 1)
  scriptNode = spn

  let totalPCMBytes = 0
  let totalPCMFrames = 0
  let captureStartTime = performance.now()

  spn.onaudioprocess = (event) => {
    const input = event.inputBuffer.getChannelData(0)
    const { pcm, rms } = resampleToInt16(input, ctx.sampleRate)

    // 刻意不加 `rms > 0` 這個條件（worklet 路徑也沒有）：rms 一直是 0 恰恰是要抓的現象，
    // 「有聲音才記」等於把最需要的那條證據丟掉 —— 於是日誌裡「麥克風全程無聲」和
    // 「這條日誌我壓根沒實現」長得一模一樣。
    if (!firstRmsLogged) {
      firstRmsLogged = true
      addRuntimeEvent('info', 'audio', 'First RMS received (ScriptProcessor)', {
        rms: Number(rms.toFixed(6)),
        sampleRate: TARGET_SAMPLE_RATE,
      })
    }
    onVolumeChange?.(rms)

    if (pcm.length > 0) {
      const pcmBuffer = pcm.buffer as ArrayBuffer
      totalPCMBytes += pcmBuffer.byteLength
      totalPCMFrames++

      if (!firstPCMFrameLogged) {
        firstPCMFrameLogged = true
        captureStartTime = performance.now()
        let peak = 0
        for (let i = 0; i < pcm.length; i++) {
          const value = Math.abs(pcm[i])
          if (value > peak) peak = value
        }
        addRuntimeEvent('info', 'audio', 'First PCM frame received (ScriptProcessor)', {
          samples: pcm.length,
          peak,
          byteLength: pcmBuffer.byteLength,
          sampleRate: TARGET_SAMPLE_RATE,
        })
        console.log('[audio-diag] first PCM frame (ScriptProcessor)', {
          samples: pcm.length,
          byteLength: pcmBuffer.byteLength,
          contextSampleRate: ctx.sampleRate,
          targetSampleRate: TARGET_SAMPLE_RATE,
        })
      }

      if (totalPCMFrames % 100 === 0) {
        const elapsedSec = (performance.now() - captureStartTime) / 1000
        const pcmDurationSec = (totalPCMBytes / 2) / 16000
        console.log('[audio-diag] PCM total (ScriptProcessor)', {
          frames: totalPCMFrames,
          totalBytes: totalPCMBytes,
          wallTimeSec: elapsedSec.toFixed(2),
          pcmDurationSec: pcmDurationSec.toFixed(2),
        })
      }

      onPCMFrame?.(pcm)
      onAudioData?.(pcmBuffer)
    }
  }

  src.connect(spn)
  // ScriptProcessorNode requires an output connection to work
  spn.connect(ctx.destination)
  console.log('[audio-diag] ScriptProcessorNode fallback active')
  addRuntimeEvent('info', 'audio', 'ScriptProcessorNode fallback activated', {
    bufferSize: 4096,
    inputSampleRate: ctx.sampleRate,
    targetSampleRate: TARGET_SAMPLE_RATE,
  })
}

/** 嘗試載入 AudioWorklet 模組（data URL → Blob URL 兩種方式） */
async function tryLoadAudioWorklet(ctx: AudioContext): Promise<boolean> {
  try {
    const dataUrl = 'data:application/javascript;base64,' + btoa(PCM_WORKLET_CODE)
    await Promise.race([
      ctx.audioWorklet.addModule(dataUrl),
      new Promise<void>((_, reject) => setTimeout(() => reject(new Error('addModule timeout')), 3000)),
    ])
    console.log('[audio-diag] AudioWorklet module loaded (data URL)')
    return true
  } catch (dataUrlErr) {
    console.warn('[audio-diag] data URL addModule failed:', dataUrlErr)
  }

  try {
    const blob = new Blob([PCM_WORKLET_CODE], { type: 'application/javascript' })
    const blobUrl = URL.createObjectURL(blob)
    try {
      await Promise.race([
        ctx.audioWorklet.addModule(blobUrl),
        new Promise<void>((_, reject) => setTimeout(() => reject(new Error('addModule timeout')), 3000)),
      ])
      console.log('[audio-diag] AudioWorklet module loaded (Blob URL)')
      return true
    } finally {
      URL.revokeObjectURL(blobUrl)
    }
  } catch (blobErr) {
    console.warn('[audio-diag] Blob URL addModule also failed:', blobErr)
  }

  return false
}

/** 建立 AudioWorkletNode 並繫結 onmessage 處理 */
function setupAudioWorkletNode(ctx: AudioContext, src: MediaStreamAudioSourceNode): AudioWorkletNode {
  const node = new AudioWorkletNode(ctx, 'pcm-processor', {
    numberOfInputs: 1,
    numberOfOutputs: 0,
    processorOptions: {
      targetRate: TARGET_SAMPLE_RATE,
      inputRate: ctx.sampleRate,
    },
  })

  addRuntimeEvent('info', 'audio', 'AudioContext ready', {
    contextState: ctx.state,
    inputSampleRate: ctx.sampleRate,
    targetSampleRate: TARGET_SAMPLE_RATE,
    ratio: (ctx.sampleRate / TARGET_SAMPLE_RATE).toFixed(4),
  })

  let totalPCMBytes = 0
  let totalPCMFrames = 0
  let captureStartTime = performance.now()

  node.port.onmessage = (e) => {
    // 標記已收到 worklet 資料（用於外部靜默檢測）
    node.dispatchEvent(new Event('worklet-data'))

    if (typeof e.data.sampleRate === 'number') {
      actualSampleRate = e.data.sampleRate
    }

    if (typeof e.data.rms === 'number') {
      if (!firstRmsLogged) {
        firstRmsLogged = true
        addRuntimeEvent('info', 'audio', 'First RMS received', {
          rms: Number(e.data.rms.toFixed(6)),
          sampleRate: actualSampleRate,
        })
      }
      onVolumeChange?.(e.data.rms)
    }

    const pcmBuffer = e.data.pcm as ArrayBuffer | undefined
    if (pcmBuffer && pcmBuffer.byteLength > 0) {
      const pcmFrame = new Int16Array(pcmBuffer)
      totalPCMBytes += pcmBuffer.byteLength
      totalPCMFrames++
      if (!firstPCMFrameLogged) {
        firstPCMFrameLogged = true
        captureStartTime = performance.now()
        let peak = 0
        for (let i = 0; i < pcmFrame.length; i++) {
          const value = Math.abs(pcmFrame[i])
          if (value > peak) peak = value
        }
        addRuntimeEvent('info', 'audio', 'First PCM frame received', {
          samples: pcmFrame.length,
          peak,
          byteLength: pcmBuffer.byteLength,
          sampleRate: actualSampleRate,
        })
        console.log('[audio-diag] first PCM frame', {
          samples: pcmFrame.length,
          byteLength: pcmBuffer.byteLength,
          contextSampleRate: audioCtx?.sampleRate,
          targetSampleRate: TARGET_SAMPLE_RATE,
          actualSampleRate,
        })
      }
      if (totalPCMFrames % 5000 === 0) {
        const elapsedSec = (performance.now() - captureStartTime) / 1000
        const pcmDurationSec = (totalPCMBytes / 2) / 16000
        console.log('[audio-diag] PCM total', {
          frames: totalPCMFrames,
          totalBytes: totalPCMBytes,
          wallTimeSec: elapsedSec.toFixed(2),
          pcmDurationSec: pcmDurationSec.toFixed(2),
        })
      }
      onPCMFrame?.(pcmFrame)
      onAudioData?.(pcmBuffer)
    }
  }

  src.connect(node)
  console.log('[audio-diag] sourceNode connected to workletNode')
  return node
}

export async function startCapture(
  deviceId: string | undefined,
  onData: (buffer: ArrayBuffer) => void,
  onVolume?: (volume: number) => void,
  onFrame?: (pcm: Int16Array) => void,
  noiseSuppression: boolean = true,
) {
  // Always tear down previous capture to prevent stale state leaks
  const hadPriorCtx = audioCtx !== null
  const hadPriorWorklet = workletNode !== null
  const hadPriorStream = mediaStream !== null
  if (hadPriorCtx || hadPriorWorklet || hadPriorStream) {
    console.warn('[audio-diag] startCapture found stale state; cleaning up', {
      hadAudioCtx: hadPriorCtx,
      priorCtxState: audioCtx?.state,
      hadWorklet: hadPriorWorklet,
      hadStream: hadPriorStream,
    })
    addRuntimeEvent('warn', 'audio', 'startCapture cleaned up stale state', {
      hadAudioCtx: hadPriorCtx,
      priorCtxState: audioCtx?.state,
      hadWorklet: hadPriorWorklet,
      hadStream: hadPriorStream,
    })
  }
  await teardownCapture()

  onAudioData = onData
  onVolumeChange = onVolume ?? null
  onPCMFrame = onFrame ?? null
  firstPCMFrameLogged = false
  firstRmsLogged = false
  actualSampleRate = TARGET_SAMPLE_RATE
  usingFallback = false

  const constraints: MediaStreamConstraints = {
    audio: {
      channelCount: 1,
      echoCancellation: false,
      // 預設開啟降噪：部分機器麥克風底噪大（低頻電流聲），關閉降噪會原樣錄入。
      // 但瀏覽器降噪對 ASR 是個變數——它按"人聽得舒服"最佳化，可能削掉模型要的細節，
      // 所以做成開關，麥克風環境乾淨的使用者可以關掉對比。
      // 回聲消除/自動增益保持關閉，避免影響 ASR 音訊。
      noiseSuppression,
      autoGainControl: false,
      ...(deviceId ? { deviceId: { exact: deviceId } } : {}),
    },
  }

  try {
    console.log('[audio-diag] getUserMedia starting...', { deviceId: deviceId || 'default' })
    mediaStream = await navigator.mediaDevices.getUserMedia(constraints)
    microphoneOpened.dispatchEvent(new Event('change'))
    const track = mediaStream.getAudioTracks()[0] || null
    const settings = track?.getSettings?.()
    // [自訂] SC-62：流已經開著，所以這一次列舉拿得到帶名字的條目。與建立 AudioContext、
    // 載入 worklet 並行，回傳前才等它（失敗一律是空快照，不會拒絕）。
    const endpoints = snapshotInputEndpoints()
    const ready = async () => {
      activeMicrophone.devices = await endpoints
      return activeMicrophone
    }
    const activeMicrophone: ActiveMicrophoneInfo = {
      // Prefer the resolved track setting over the requested id. In system-default mode
      // these may differ, and the resolved value is what the reminder needs to describe.
      deviceId: String(settings?.deviceId || deviceId || ''),
      groupId: String(settings?.groupId || ''),
      label: String(track?.label || ''),
      // 回傳前由 ready() 填入端點快照。
      devices: [],
    }

    console.log('[audio-diag] getUserMedia success', {
      trackCount: mediaStream.getAudioTracks().length,
      trackLabel: track?.label,
      trackEnabled: track?.enabled,
      trackMuted: track?.muted,
      trackReadyState: track?.readyState,
      settings,
    })

    // muted / readyState / enabled 必須落盤：它們區分「Chromium 自己知道這路採集被靜音了」
    // 與「Chromium 以為一切正常、實際送來的全是 0」。後者是 WebView2 的已知毛病
    // （採集靜默降級成靜音且不派發 mute 事件），只能靠對照這三個值 + 後面的 peakAmplitude 判定。
    // [自訂] S32：這條會寫進 sayit.log。不記麥克風裝置名稱（藍牙耳機名稱常含本名）；
    // trackSettings 內含 deviceId／groupId，也一併不記。
    addRuntimeEvent('info', 'audio', 'Microphone capture started', {
      requestedDeviceId: deviceId || 'default',
      // [自訂] S32 已移除：trackLabel: track?.label || '',
      trackMuted: track?.muted ?? null,
      trackEnabled: track?.enabled ?? null,
      trackReadyState: track?.readyState || null,
      // [自訂] S32 已移除：trackSettings: settings || null,
    })

    audioCtx = createAudioContext()
    actualSampleRate = audioCtx.sampleRate || TARGET_SAMPLE_RATE
    const nativeSixteenK = audioCtx.sampleRate === TARGET_SAMPLE_RATE
    console.log('[audio-diag] AudioContext created', {
      state: audioCtx.state,
      sampleRate: audioCtx.sampleRate,
      nativeSixteenK,
    })
    // 記一條到執行時日誌：這是判斷"有沒有走上高質量重取樣"的唯一依據，
    // 排查識別準確度問題時先看這裡。
    addRuntimeEvent('info', 'audio', nativeSixteenK ? 'AudioContext is natively 16 kHz; resampling skipped' : 'AudioContext is not 16 kHz; using linear resampling', {
      contextState: audioCtx.state,
      contextSampleRate: audioCtx.sampleRate,
      targetSampleRate: TARGET_SAMPLE_RATE,
    })

    sourceNode = audioCtx.createMediaStreamSource(mediaStream)

    // Try AudioWorklet, with ScriptProcessor fallback
    const fallbackCtx = audioCtx
    const fallbackSrc = sourceNode
    const fallbackTimerId = setTimeout(() => {
      if (!usingFallback && fallbackCtx === audioCtx && fallbackSrc === sourceNode) {
        console.warn('[audio-diag] AudioWorklet timed out (1.5s); switching to ScriptProcessorNode')
        addRuntimeEvent('warn', 'audio', 'AudioWorklet timed out; switching to ScriptProcessorNode fallback')
        setupScriptProcessorFallback(fallbackCtx, fallbackSrc)
      }
    }, 1500)

    const workletLoaded = await tryLoadAudioWorklet(audioCtx)

    if (audioCtx.state === 'suspended') {
      console.log('[audio-diag] AudioContext is suspended, resuming...')
      await audioCtx.resume()
      console.log('[audio-diag] AudioContext resumed, state:', audioCtx.state)
    }

    if (!workletLoaded) {
      clearTimeout(fallbackTimerId)
      if (!usingFallback) {
        console.log('[audio-diag] AudioWorklet unavailable, using ScriptProcessorNode directly')
        setupScriptProcessorFallback(audioCtx, sourceNode)
      }
      return await ready()
    }

    clearTimeout(fallbackTimerId)
    if (usingFallback) {
      console.log('[audio-diag] ScriptProcessorNode already active, skipping worklet setup')
      return await ready()
    }

    // AudioWorklet loaded — set up node and monitor for data
    workletNode = setupAudioWorkletNode(audioCtx, sourceNode)

    // Secondary monitor: if worklet loaded but no data within 800ms, switch
    let gotWorkletData = false
    workletNode.addEventListener('worklet-data', () => { gotWorkletData = true }, { once: true })
    setTimeout(() => {
      if (!gotWorkletData && !usingFallback && fallbackCtx === audioCtx && fallbackSrc === sourceNode) {
        console.warn('[audio-diag] AudioWorklet produced no data (800ms); switching to ScriptProcessorNode')
        addRuntimeEvent('warn', 'audio', 'AudioWorklet was silent; switching to ScriptProcessorNode fallback')
        setupScriptProcessorFallback(fallbackCtx, fallbackSrc)
      }
    }, 800)

    return await ready()

  } catch (error) {
    await teardownCapture()
    addRuntimeEvent('error', 'audio', 'Failed to start microphone capture', {
      requestedDeviceId: deviceId || 'default',
      error: String(error),
    })
    throw error
  }
}

export async function stopCapture() {
  const finalCtxRate = audioCtx?.sampleRate
  console.log('[audio-diag] stopCapture final summary', {
    contextSampleRate: finalCtxRate,
    actualSampleRate,
    targetSampleRate: TARGET_SAMPLE_RATE,
    usingFallback,
  })
  addRuntimeEvent('info', 'audio', 'Capture stop summary', {
    contextSampleRate: finalCtxRate,
    actualSampleRate,
    targetSampleRate: TARGET_SAMPLE_RATE,
    usingFallback,
    // 「一幀 PCM 都沒到過」需要一條肯定式證據。只靠「日誌裡沒有 First PCM frame received」
    // 來推，讀日誌的人無法區分它與「這條日誌沒被映象」。
    receivedPcm: firstPCMFrameLogged,
  })

  // [自訂] SC-62：資料路徑與麥克風在這裡已停止；AudioContext 在背景關閉。
  releaseCapture()

  onAudioData = null
  onVolumeChange = null
  onPCMFrame = null
}
