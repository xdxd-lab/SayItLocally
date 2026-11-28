import { invoke } from '@tauri-apps/api/core'
import { addRuntimeEvent } from '../debugLog'
import type { OverlayService } from './OverlayService'
import type { RecorderState } from './types'
import {
  classifyMicLevel,
  hasSilenceEvidence,
  judgeOsMicMute,
  type MicLevel,
} from './helpers'

const MIC_MUTED_AUTO_CANCEL_MS = 3000
const SILENCE_RMS_THRESHOLD = 0.01

/** MicLevelMonitor 要從編排器拿的東西：只讀的錄音狀態，加一個「靜音超時就取消」的動作。 */
export interface MicLevelHost {
  overlay: OverlayService
  isRunCurrent(runId: number): boolean
  getState(): RecorderState
  /** startRecording 還在初始化（state 仍是 idle）。 */
  isStarting(): boolean
  getActiveRunId(): number
  getSelectedMicId(): string
  cancelForMutedMic(runId: number): void
  /** 本次錄音開啟的是虛擬裝置時返回 true（用於把「未檢測到聲音」說得更具體）。拿不準返回 false。 */
  isActiveMicVirtual?(): boolean
  /** 本次錄音第一次確認聽到正常人聲時呼叫一次。 */
  onVoiceDetected?(): void
}

/**
 * 錄音時的麥克風電平：音量兩檔提醒、系統靜音標誌的裁決、本次錄音的音訊統計。
 * 從 RecorderOrchestrator 拆出（模組 6），行為不變。
 */
export class MicLevelMonitor {
  /** 系統確認麥克風靜音後的自動取消計時器，避免懸浮窗無限停留。 */
  private micMutedAutoCancelId: ReturnType<typeof setTimeout> | null = null
  /** 靜音探測代次：實際開啟裝置的複核必須覆蓋較早發起的預檢查結果。 */
  private micMuteProbeSequence = 0
  /** 連續「非正常音量」（靜音或偏低）的取樣數 */
  private consecutiveSilentSamples = 0
  /** 當前連續非正常音量的取樣數，僅用於判斷正常說話候選之間的間隔。 */
  private consecutiveNonVoicedSamples = 0
  /** 當前這段安靜裡是否出現過任何非零取樣。
   * 只要為真，就只能提示「請靠近麥克風」，不能提示「未檢測到聲音」。 */
  private quietRunSawSignal = false
  /** 連續正常音量的取樣數——用於「持續說話一段時間才清除警告」的遲滯判斷，避免瞬時小聲音把警告瞬間清掉造成閃爍 */
  private consecutiveVoicedSamples = 0
  /** 懸浮窗當前正在顯示的音量類警告 */
  private currentVolumeWarning: 'none' | 'muted' | 'low' = 'none'
  /** 本次錄音是否已經聽到過一次正常音量的說話。一旦聽到過，說明麥克風正常，
   *  後續安靜只是正常停頓思考——只給溫和「請靠近麥克風」，絕不再報「未檢測到聲音」造成驚嚇。 */
  private hasDetectedVoiceThisSession = false
  private lastLowVolumeWarnAt = 0
  /** 麥克風被靜音已被「系統標誌 + 音訊訊號」雙重證實。為真時懸浮窗顯示紅色高警，
   *  並抑制基於訊號的琥珀提醒，直到檢測到真實說話（使用者中途取消了靜音）。 */
  private osMicMuted = false
  /** 系統報了「端點被靜音」，但還沒被音訊訊號證實。
   *
   *  為什麼不能直接相信系統：IAudioEndpointVolume::GetMute 反映的是端點上的靜音**設定**，
   *  不等於採集流真被切斷。實測 Plantronics Blackwire 5220 USB 耳麥（軟/硬靜音會與 Windows
   *  端點標誌同步）會停在 GetMute=true，而 getUserMedia 照常收到正常音量的音訊 —— 於是每次
   *  按熱鍵都先彈一次紅色高警「麥克風已被靜音」，說話 0.5s 後又自己消失。
   *  所以這個標誌只當線索：必須等 PCM 證實真的全 0 才升級成 osMicMuted。 */
  private pendingOsMicMuted = false
  /** pending 期間累計的全 0 取樣數，達到 OS_MIC_MUTE_CONFIRM_SAMPLES 才確認。 */
  private pendingOsMicMutedSamples = 0

  /** Audio stats tracking */
  private statsRmsSum = 0
  private statsPeakRms = 0
  private statsPeakAmplitude = 0
  private statsSilentFrames = 0
  private statsTotalFrames = 0

  constructor(private readonly host: MicLevelHost) { }

  /** 錄音一開始（顯示懸浮窗之前）就清掉上一次的靜音線索。與 resetLevels 分開：兩次中間夾著非同步的靜音預檢查。 */
  resetMuteState() {
    this.clearAutoCancelTimer()
    this.osMicMuted = false
    this.pendingOsMicMuted = false
    this.pendingOsMicMutedSamples = 0
  }

  /** 取完錄音上下文、開始採集之前清掉統計和音量提醒。 */
  resetLevels() {
    this.statsRmsSum = 0
    this.statsPeakRms = 0
    this.statsPeakAmplitude = 0
    this.statsSilentFrames = 0
    this.statsTotalFrames = 0
    this.consecutiveSilentSamples = 0
    this.consecutiveNonVoicedSamples = 0
    this.quietRunSawSignal = false
    this.consecutiveVoicedSamples = 0
    this.currentVolumeWarning = 'none'
    this.hasDetectedVoiceThisSession = false
    this.lastLowVolumeWarnAt = 0
  }

  /**
   * 回到空閒時清提醒和靜音線索，**不清音訊統計**：超時寬限期裡判斷「有沒有靜音實證」
   * 還要用到這一段的統計，下一次 resetLevels 才清。
   */
  resetOnIdle() {
    this.consecutiveSilentSamples = 0
    this.consecutiveNonVoicedSamples = 0
    this.quietRunSawSignal = false
    this.consecutiveVoicedSamples = 0
    this.currentVolumeWarning = 'none'
    this.hasDetectedVoiceThisSession = false
    this.lastLowVolumeWarnAt = 0
    this.osMicMuted = false
    this.pendingOsMicMuted = false
    this.pendingOsMicMutedSamples = 0
  }

  /** 讓已發出的靜音查詢作廢（錄音結束、清理時）。 */
  invalidateProbes() {
    this.micMuteProbeSequence++
  }

  nextProbeSequence(): number {
    return ++this.micMuteProbeSequence
  }

  /** 每一幀 PCM 調一次：累計統計並推進音量提醒。 */
  onFrame(pcmFrame: Int16Array) {
    // 計算本幀 RMS 與峰值幅度（均歸一化到 0..1）。峰值嚴格為 0 才表示整幀沒有輸入；
    // 只要存在任何非零取樣，就必須和「未檢測到聲音」區分開。
    let sum = 0
    let framePeakRaw = 0
    for (let i = 0; i < pcmFrame.length; i++) {
      const s = pcmFrame[i]
      sum += s * s
      const amp = s < 0 ? -s : s
      if (amp > framePeakRaw) framePeakRaw = amp
    }
    const rms = Math.sqrt(sum / pcmFrame.length) / 32768
    const framePeak = framePeakRaw / 32768

    // Audio stats tracking
    this.statsTotalFrames++
    this.statsRmsSum += rms
    if (rms > this.statsPeakRms) this.statsPeakRms = rms
    if (framePeak > this.statsPeakAmplitude) this.statsPeakAmplitude = framePeak
    if (rms < SILENCE_RMS_THRESHOLD) this.statsSilentFrames++

    // 兩檔提醒：靜音（未檢測到聲音）/ 偏低（請靠近麥克風）
    this.updateVolumeWarning(classifyMicLevel(rms, framePeak), pcmFrame.length)
  }

  /** 隨 stop 發給服務端、並寫進日誌的統計。沒收到過幀時為 undefined。 */
  summary() {
    return this.statsTotalFrames > 0 ? {
      avgRms: Math.round((this.statsRmsSum / this.statsTotalFrames) * 10000) / 10000,
      peakRms: Math.round(this.statsPeakRms * 10000) / 10000,
      peakAmplitude: Math.round(this.statsPeakAmplitude * 10000) / 10000,
      silenceRatio: Math.round((this.statsSilentFrames / this.statsTotalFrames) * 1000) / 1000,
      totalFrames: this.statsTotalFrames,
    } : undefined
  }

  roundedPeakAmplitude(): number {
    return Math.round(this.statsPeakAmplitude * 10000) / 10000
  }

  /**
   * 這段錄音是否有「確實沒有可用語音」的實測證據。
   *
   * 為什麼要證據：「未檢測到有效聲音」是全軟體最容易被誤讀的一句話，它會把使用者直接
   * 引去查麥克風。而同一句話背後的真實成因可能是額度耗盡、服務端提前斷開、熱詞回顯
   * 被判定清空（見 pitfalls 15）。沒有證據時就該說「沒有取得識別結果」，別替使用者
   * 下結論。
   *
   * 判據本身抽到了 helpers.ts（純函式，帶單測釘住"兩個條件必須同時成立"那一刀）。
   */
  hasSilenceEvidence(): boolean {
    return hasSilenceEvidence({
      totalFrames: this.statsTotalFrames,
      silentFrames: this.statsSilentFrames,
      peakAmplitude: this.statsPeakAmplitude,
      silenceRmsThreshold: SILENCE_RMS_THRESHOLD,
    })
  }

  private markVoiceDetected() {
    const first = !this.hasDetectedVoiceThisSession
    this.hasDetectedVoiceThisSession = true
    if (first) this.host.onVoiceDetected?.()
  }

  clearAutoCancelTimer() {
    if (this.micMutedAutoCancelId) {
      clearTimeout(this.micMutedAutoCancelId)
      this.micMutedAutoCancelId = null
    }
  }

  private scheduleMicMutedAutoCancel(runId: number) {
    this.clearAutoCancelTimer()
    const closeWhenReady = () => {
      this.micMutedAutoCancelId = null
      if (!this.host.isRunCurrent(runId) || !this.osMicMuted) return
      if (this.host.getState() === 'idle' && this.host.isStarting()) {
        // 防禦：這個計時器現在只在收到音訊幀後才排，理論上必定已在 recording。
        // 萬一將來又從「採集就緒前」的路徑呼叫，也不要在這裡直接放棄。
        this.micMutedAutoCancelId = setTimeout(closeWhenReady, 100)
        return
      }
      if (this.host.getState() !== 'recording') return
      addRuntimeEvent('info', 'recorder', 'Muted microphone warning timed out; recording will close automatically', {
        runId,
        timeoutMs: MIC_MUTED_AUTO_CANCEL_MS,
      })
      this.host.cancelForMutedMic(runId)
    }
    this.micMutedAutoCancelId = setTimeout(closeWhenReady, MIC_MUTED_AUTO_CANCEL_MS)
  }

  /** 在採集鏈路初始化前查詢系統靜音狀態；預設裝置不經過可能很慢的裝置列舉。 */
  async checkConfiguredMicMuted(runId: number) {
    // 在任何非同步操作之前佔用代次；之後發起的實際裝置複核可以可靠淘汰本次預檢查。
    const probeSequence = ++this.micMuteProbeSequence
    // 空值與 Web Media 的 "default" 都表示跟隨 Windows 當前預設錄音端點。
    // 直接把 null 交給原生層查詢該端點的 GetMute；deviceId 只用於定位，不參與靜音判定。
    if (!this.host.getSelectedMicId() || this.host.getSelectedMicId() === 'default') {
      await this.checkMicMuted(null, runId, probeSequence)
      return
    }

    try {
      const devices = await navigator.mediaDevices.enumerateDevices()
      if (probeSequence !== this.micMuteProbeSequence || !this.host.isRunCurrent(runId)) return
      const inputs = devices.filter((device) => device.kind === 'audioinput')
      const selected = inputs.find((device) => device.deviceId === this.host.getSelectedMicId())

      const label = selected?.label?.trim() || ''
      // 指定裝置卻無法取得名稱時不能回退查系統預設端點，否則可能把另一支麥克風的狀態報過來。
      if (!label) return
      await this.checkMicMuted(label, runId, probeSequence)
    } catch {
      // 預檢查失敗不影響錄音；採集真正開啟後還會用 MediaStreamTrack.label 再複核一次。
    }
  }

  /**
   * 錄音時的音量提醒狀態機（兩檔）：
   *  - muted（PCM 全 0）：麥克風沒有任何輸入 → 「未檢測到聲音」
   *  - low （有聲音但 RMS 偏低）：距離遠 / 說太小聲 → 「請靠近麥克風」
   *  - voiced（正常）：連續約 0.5s 即認定恢復，清除提醒
   *
   * 關鍵準確性約束：「未檢測到聲音」只在「從錄音開始就沒聽到過正常說話，且這整段安靜裡
   * 每個取樣都為 0」時才報；任何非零輸入都只能算聲音小。這樣也避免把說話後的正常停頓
   * 思考誤判成裝置故障（那種情況只溫和提示「請靠近麥克風」）。
   * 計時/遲滯沿用原邏輯：沒聽到過說話時更早提醒（2s），
   * 聽到過後放寬（5s）；同檔每 5s 重彈；連續正常音量 0.5s 才清警告，避免瞬時小聲閃爍。
   */
  private updateVolumeWarning(level: MicLevel, sampleCount: number) {
    const REWARN_MS = 5000
    const CLEAR_VOICED = 8000 // ~0.5s @16kHz 累計正常音量才清除
    const VOICED_GAP_TOLERANCE = 4800 // ~300ms：容忍說話中字與字之間的短暫停頓，不清零已累計的清除進度
    const firstWarn = this.hasDetectedVoiceThisSession ? 80000 : 32000 // 5s / 2s @16kHz

    // 系統報了端點靜音，但那只是設定標誌，不代表採集流真被切斷（見 pendingOsMicMuted）。
    // 用真實 PCM 裁決：全 0 累計到閾值才升級成紅色高警；一旦出現任何非零取樣，
    // 就說明這塊裝置的 mute 標誌不作數，本次錄音永久丟棄這條線索。
    if (this.pendingOsMicMuted) {
      const decision = judgeOsMicMute(this.pendingOsMicMutedSamples, level, sampleCount)
      if (decision.verdict === 'confirmed') {
        this.pendingOsMicMuted = false
        this.pendingOsMicMutedSamples = 0
        this.osMicMuted = true
        addRuntimeEvent('warn', 'recorder', 'Microphone is muted by the operating system (confirmed: audio is all-zero)')
        this.host.overlay.showMicMutedAlert()
        this.scheduleMicMutedAutoCancel(this.host.getActiveRunId())
      } else if (decision.verdict === 'dismissed') {
        this.pendingOsMicMuted = false
        this.pendingOsMicMutedSamples = 0
        addRuntimeEvent('info', 'recorder', 'System reported the microphone as muted but audio is flowing; ignoring the flag')
      } else {
        this.pendingOsMicMutedSamples = decision.silentSamples
      }
    }

    // 系統已確認被靜音：保持紅色高警，不讓琥珀提醒覆蓋它；
    // 一旦收到真實說話（使用者中途取消了靜音）立即恢復正常。
    if (this.osMicMuted) {
      if (level === 'voiced') {
        this.consecutiveVoicedSamples += sampleCount
        this.consecutiveNonVoicedSamples = 0
        if (this.consecutiveVoicedSamples < CLEAR_VOICED) return
      } else {
        this.consecutiveNonVoicedSamples += sampleCount
        if (this.consecutiveNonVoicedSamples >= VOICED_GAP_TOLERANCE) this.consecutiveVoicedSamples = 0
        return
      }
      this.osMicMuted = false
      this.clearAutoCancelTimer()
      this.host.overlay.clearWarning()
      this.currentVolumeWarning = 'none'
      this.consecutiveVoicedSamples = 0
      this.consecutiveSilentSamples = 0
      this.consecutiveNonVoicedSamples = 0
      this.quietRunSawSignal = false
      this.markVoiceDetected()
      return
    }

    if (level === 'voiced') {
      // 即使還不足 0.5s、尚未確認恢復正常，也已經證明輸入不為零。
      this.quietRunSawSignal = true
      this.consecutiveVoicedSamples += sampleCount
      this.consecutiveNonVoicedSamples = 0
      if (this.consecutiveVoicedSamples >= CLEAR_VOICED) {
        this.markVoiceDetected()
        this.consecutiveSilentSamples = 0
        this.quietRunSawSignal = false
        if (this.currentVolumeWarning !== 'none') {
          this.host.overlay.clearWarning()
          this.currentVolumeWarning = 'none'
        }
      }
      return
    }

    // 非正常音量（muted / low）：累計安靜時長；任何非零輸入都永久標記本段並非「無聲音」。
    this.consecutiveSilentSamples += sampleCount
    this.consecutiveNonVoicedSamples += sampleCount
    if (level === 'low') this.quietRunSawSignal = true
    // 說話時字與字之間的短暫停頓（<~300ms）不清零已累計的正常音量，否則一句話裡的自然
    // 停頓會反覆把"清除進度"打回 0，導致明明在正常說話也湊不滿連續 0.5s、警告始終清不掉。
    // 只有持續安靜超過這個小間隙，才認為不是說話停頓、歸零重新計。
    if (this.consecutiveNonVoicedSamples >= VOICED_GAP_TOLERANCE) this.consecutiveVoicedSamples = 0

    if (this.consecutiveSilentSamples < firstWarn) return

    // 只有從未正常說過話且整段所有採樣均為 0，才允許提示「未檢測到聲音」。
    const kind: 'muted' | 'low' =
      !this.hasDetectedVoiceThisSession && !this.quietRunSawSignal ? 'muted' : 'low'

    const now = Date.now()
    // 切換到不同檔位立即重新整理文案；同檔位按節流重彈，避免頻繁催促
    if (kind === this.currentVolumeWarning && now - this.lastLowVolumeWarnAt < REWARN_MS) return
    this.lastLowVolumeWarnAt = now
    this.currentVolumeWarning = kind
    if (kind === 'muted') {
      const virtualMic = this.host.isActiveMicVirtual?.() === true
      addRuntimeEvent('warn', 'recorder', 'No microphone signal detected; device may be wrong or unavailable', { virtualMic })
      this.host.overlay.showNoSignalWarning({ virtualMic })
    } else {
      addRuntimeEvent('warn', 'recorder', 'Microphone volume is low')
      this.host.overlay.showLowVolumeWarning()
    }
  }

  /**
   * 錄音開始時查詢系統麥克風的靜音標誌，作為「麥克風已被靜音」紅色高警的**線索**記錄下來；
   * 真正彈警告要等音訊幀證實 PCM 全 0（見 pendingOsMicMuted）——系統標誌會騙人。
   * 僅在能可靠定位裝置時才採信（系統預設裝置，或按名字唯一匹配到選中裝置），
   * 否則（如選了特定裝置但拿不到名字/同名多個）返回不判定，交給基於訊號的琥珀提醒兜底。
   */
  async checkMicMuted(
    activeDeviceLabel: string | null,
    runId: number,
    probeSequence: number,
  ) {
    try {
      // 查詢本次 getUserMedia 實際開啟的端點，而不是根據設定值或 deviceId 猜測。
      // 裝置名無法唯一匹配時 Rust 返回 matched=false，繼續交給訊號檢測兜底。
      const res = await invoke<{ matched: boolean; muted: boolean }>('get_mic_mute_state', {
        deviceLabel: activeDeviceLabel,
      })
      // 後發的實際裝置複核優先；舊探測、舊錄音或已恢復正常說話時都不能再彈。
      if (probeSequence !== this.micMuteProbeSequence || !this.host.isRunCurrent(runId)) return
      if (this.host.getState() !== 'recording' && !(this.host.getState() === 'idle' && this.host.isStarting())) return
      if (this.hasDetectedVoiceThisSession || !res.matched) return

      if (res.muted) {
        // 只記線索、不彈 UI：等音訊幀證實真的全 0（原因見 pendingOsMicMuted 註釋）。
        // 已確認、或已在等待證即時都不要重置累計進度（預檢查與實際裝置複核都會走到這裡）。
        if (this.osMicMuted || this.pendingOsMicMuted) return
        this.pendingOsMicMuted = true
        this.pendingOsMicMutedSamples = 0
        addRuntimeEvent('info', 'recorder', 'System reports the microphone endpoint is muted; waiting for audio to confirm')
      } else {
        // 實際開啟的端點與預檢查不一致，或使用者在初始化期間取消了靜音。
        this.pendingOsMicMuted = false
        this.pendingOsMicMutedSamples = 0
        if (this.osMicMuted) {
          this.osMicMuted = false
          this.clearAutoCancelTimer()
          this.host.overlay.clearWarning()
        }
      }
    } catch { /* 查詢失敗不影響錄音，交給訊號檢測兜底 */ }
  }
}
