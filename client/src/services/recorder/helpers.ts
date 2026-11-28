/**
 * RecorderOrchestrator 純輔助函式
 * 不依賴 this 狀態，可獨立測試
 */

import type { ActiveAppContext } from '@/types/appContext'

/** 簡化 AppContext 用於日誌輸出 */
// [自訂] S31：這份摘要會寫進 sayit.log（「Recording started」），保留 30 天並進診斷 zip。
// 只留 bundle ID／程式名稱、角色與布林值；不記視窗標題、執行檔路徑（含使用者名稱）、焦點元件名稱。
export function summarizeAppContext(context: ActiveAppContext | null) {
  if (!context) return null
  return {
    processName: context.processName,
    bundleId: context.bundleId,
    // [自訂] S31 已移除：exePath: context.exePath,
    // [自訂] S31 已移除：windowTitle: context.windowTitle,
    windowClass: context.windowClass,
    focusClass: context.focusClass,
    controlType: context.controlType,
    // [自訂] S31 已移除：focusedName: context.focusedName,
    hasCaret: context.hasCaret,
    isKeyboardFocusable: context.isKeyboardFocusable,
    isReadOnly: context.isReadOnly,
    isPassword: context.isPassword,
  }
}

/** 從 AppContext 中提取用於統計的 appId */
export function buildStatsAppId(
  appContext: ActiveAppContext | null,
  promptAppId?: string,
): string {
  const processName = String(appContext?.processName || '').trim()
  if (processName) return processName

  const exePath = String(appContext?.exePath || '').trim()
  if (exePath) {
    const segments = exePath.split(/[\\/]/).filter(Boolean)
    const lastSegment = segments[segments.length - 1]
    if (lastSegment) return lastSegment
  }

  return String(promptAppId || '').trim() || 'unknown'
}

/** 錄音時的麥克風音量分級 */
export type MicLevel = 'muted' | 'low' | 'voiced'

/** 只有峰值嚴格為 0（PCM 整幀全 0）才算「沒有訊號」。
 * 再小的非零取樣也是真實存在的輸入，只能歸為聲音偏低，不能提示「未檢測到聲音」。 */
export const MIC_NO_SIGNAL_PEAK_THRESHOLD = 0
/** 有非零訊號但 RMS（0..1）低於此值時，視為「聲音偏低」（離得遠 / 說太小聲）。 */
export const MIC_LOW_RMS_THRESHOLD = 0.008

/**
 * 按本幀的 RMS 與峰值分級：
 *  - muted ：整幀 PCM 全 0（極可能麥克風被靜音 / 選錯裝置 / 未授權）
 *  - low   ：有波動但整體偏低（距離遠 / 聲音小）
 *  - voiced：正常音量
 * 兩個入參都應歸一化到 0..1（原始 int16 除以 32768）。
 */
export function classifyMicLevel(rms: number, framePeak: number): MicLevel {
  if (framePeak <= MIC_NO_SIGNAL_PEAK_THRESHOLD) return 'muted'
  if (rms < MIC_LOW_RMS_THRESHOLD) return 'low'
  return 'voiced'
}

/** 判「整段錄音確實沒有可用語音」時，靜音幀至少要佔到這個比例。 */
export const SILENCE_EVIDENCE_MIN_RATIO = 0.995

/**
 * 這段錄音有沒有「確實沒有可用語音」的實測證據。
 *
 * 為什麼需要證據：「未檢測到有效聲音」是全軟體最容易被誤讀的一句話，它會把使用者直接
 * 引去查麥克風。而同一句話背後的真實成因可能是額度耗盡、服務端提前斷開、熱詞回顯
 * 被判定清空（見 pitfalls 15）。沒有證據時就該說「沒有取得識別結果」，別替使用者下結論。
 *
 * ⚠️ 兩個條件必須**同時**成立，絕不能取或。高靜音比例單獨什麼都證明不了 —— 說一句話
 * 再沉默一分鐘，靜音幀就能佔到 99.6%，而峰值 0.4（清清楚楚有人在說話）。取或的寫法
 * 會把這種錄音判成"沒聲音"，真正的失敗原因反而被藏起來。
 *
 * 入參都是採集側的統計量，與 ASR/網路無關；峰值歸一化到 0..1。
 */
export function hasSilenceEvidence(stats: {
  totalFrames: number
  silentFrames: number
  peakAmplitude: number
  silenceRmsThreshold: number
}): boolean {
  if (stats.totalFrames <= 0) return false
  const silenceRatio = stats.silentFrames / stats.totalFrames
  return stats.peakAmplitude < stats.silenceRmsThreshold
    && silenceRatio > SILENCE_EVIDENCE_MIN_RATIO
}

/** 系統報端點靜音後，需要累計這麼多個全 0 取樣（~300ms @16kHz）才確認麥克風真的沒在收音。
 *  取 300ms：快到仍像「即時反饋」，又足以跨過採集剛開啟時可能出現的空幀。 */
export const OS_MIC_MUTE_CONFIRM_SAMPLES = 4800

/** 對「系統報麥克風被靜音」這條線索的裁決結果。
 *  - wait     ：還在攢證據，silentSamples 是更新後的累計值
 *  - confirmed：系統標誌與音訊訊號一致，可以報「麥克風已被靜音」
 *  - dismissed：音訊在正常流動，系統標誌不作數，本次錄音丟棄這條線索 */
export type OsMicMuteDecision =
  | { verdict: 'wait'; silentSamples: number }
  | { verdict: 'confirmed' }
  | { verdict: 'dismissed' }

/**
 * 用真實音訊訊號裁決系統的麥克風靜音標誌。
 *
 * 為什麼需要裁決而不能直接採信系統：`IAudioEndpointVolume::GetMute` 反映的是端點上的靜音
 * **設定**，不等於採集流真被切斷。實測 Plantronics Blackwire 5220 USB 耳麥會停在
 * GetMute=true，而 getUserMedia 照常收到正常音量的音訊——直接採信就會每次按熱鍵都先彈一次
 * 紅色高警「麥克風已被靜音」，說話 0.5s 後又自己消失。
 *
 * 於是：只有全 0 的 PCM 累計到閾值（兩個證據一致）才確認；一旦出現**任何**非零取樣，就說明
 * 這塊裝置的 mute 標誌不作數，立刻永久丟棄線索——不是清零重來，否則說話的自然停頓會把它攢回來。
 */
export function judgeOsMicMute(
  silentSamples: number,
  level: MicLevel,
  sampleCount: number,
): OsMicMuteDecision {
  if (level !== 'muted') return { verdict: 'dismissed' }
  const total = silentSamples + sampleCount
  if (total >= OS_MIC_MUTE_CONFIRM_SAMPLES) return { verdict: 'confirmed' }
  return { verdict: 'wait', silentSamples: total }
}

/** 判斷 PTT 設定中是否包含修飾鍵（舊單鍵與物理組合格式都支援） */
export function isModifierPTTSetting(pttSetting?: string): boolean {
  if (!pttSetting) return false
  return pttSetting.split('+').some((code) => (
    code.startsWith('Alt')
    || code.startsWith('Control')
    || code.startsWith('Shift')
    || code.startsWith('Meta')
  ))
}

/**
 * 模擬 Ctrl+V 耗時超過這個值，就不再當作插入成功。
 *
 * 為什麼：SendInput 只是把按鍵塞進佇列，目標程式卡住時，它會在我們 400ms 後恢復剪貼簿
 * **之後**才處理 Ctrl+V —— 於是什麼都沒粘上（或粘進了使用者原來的剪貼簿），而這一側照樣
 * 報成功、不彈卡片。慢本身就說明目標或系統在卡：耗時主要花在把目標切到前台那一步，
 * 它要等目標執行緒回應。
 * 門檻取自真實日誌：數百次 send_input 最慢 631ms，出事那次 2088ms。
 */
export const SLOW_SEND_INPUT_PASTE_MS = 1200

/**
 * 這次"成功"的插入是否拿不準、需要彈兜底卡片。只針對 send_input：WM_PASTE 與控制台貼上
 * 會核實結果或同步等目標處理完，慢不代表沒粘上。
 */
export function isUnconfirmedPaste(strategy: string | undefined, pasteExecMs: number): boolean {
  return strategy === 'send_input' && pasteExecMs >= SLOW_SEND_INPUT_PASTE_MS
}

const PROCESSING_TIMEOUT_BASE_MS = 15_000
const PROCESSING_TIMEOUT_PER_AUDIO_SEC_MS = 500
const PROCESSING_TIMEOUT_MAX_EXTRA_MS = 30_000

/** 根據音訊時長和工作模式計算處理超時時間 */
export function computeProcessingTimeoutMs(
  audioDurationSec: number,
  providerMode: string,
): number {
  const safeAudioSec = Number.isFinite(audioDurationSec) ? Math.max(0, audioDurationSec) : 0
  const extraMs = Math.min(
    PROCESSING_TIMEOUT_MAX_EXTRA_MS,
    Math.ceil(safeAudioSec * PROCESSING_TIMEOUT_PER_AUDIO_SEC_MS),
  )
  let timeout = PROCESSING_TIMEOUT_BASE_MS + extraMs

  if (providerMode !== 'server') {
    timeout = Math.max(timeout, 30000)
  }
  if (providerMode === 'cloud_api') {
    const cloudTimeout = 30000 + Math.ceil(safeAudioSec * 500)
    timeout = Math.min(Math.max(timeout, cloudTimeout), 90000)
  }
  if (providerMode === 'local') {
    // 本地識別耗時跟錄音長度成正比。原來最長只等 45 秒：實測 Qwen3-ASR 1.7B 在整合顯示卡上
    // 約 0.18 倍時長（長錄音按 30 s 切塊後也是這個速度），5 分鐘的錄音要 55 秒左右，
    // 必然超時，結果只能事後進歷史。按 0.4 倍留出餘量，最多 3 分鐘。
    const localTimeout = 15000 + Math.ceil(safeAudioSec * 400)
    timeout = Math.min(Math.max(timeout, localTimeout), 180000)
  }
  return timeout
}
