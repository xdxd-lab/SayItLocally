import { describe, it, expect } from 'vitest'
import {
  summarizeAppContext,
  buildStatsAppId,
  isModifierPTTSetting,
  computeProcessingTimeoutMs,
  classifyMicLevel,
  judgeOsMicMute,
  hasSilenceEvidence,
  isUnconfirmedPaste,
  SLOW_SEND_INPUT_PASTE_MS,
  MIC_NO_SIGNAL_PEAK_THRESHOLD,
  MIC_LOW_RMS_THRESHOLD,
  OS_MIC_MUTE_CONFIRM_SAMPLES,
} from '../helpers'

describe('hasSilenceEvidence', () => {
  const SILENCE_RMS_THRESHOLD = 0.01

  function stats(peakAmplitude: number, silentFrames: number, totalFrames: number) {
    return { peakAmplitude, silentFrames, totalFrames, silenceRmsThreshold: SILENCE_RMS_THRESHOLD }
  }

  /**
   * 迴歸釘：判據取「且」不取「或」。
   *
   * 取或的版本會把這條判成"確實沒聲音"，於是介面告訴使用者去檢查麥克風，而真正的
   * 失敗原因（額度耗盡、服務端提前斷開、熱詞回顯被判空）被藏了起來。
   */
  it('說了一句話再長時間沉默，不算沒聲音', () => {
    expect(hasSilenceEvidence(stats(0.4, 996, 1000))).toBe(false)
  })

  it('整段幾乎全是靜音幀、峰值也幾乎為零，才算有實證', () => {
    expect(hasSilenceEvidence(stats(0.004, 999, 1000))).toBe(true)
  })

  it('峰值夠低但靜音幀不夠多時不下結論', () => {
    expect(hasSilenceEvidence(stats(0.004, 900, 1000))).toBe(false)
  })

  it('一幀都沒統計到時不下結論——沒有資料不等於沒有聲音', () => {
    expect(hasSilenceEvidence(stats(0, 0, 0))).toBe(false)
  })

  it('峰值正好等於閾值也不算靜音（閾值是嚴格小於）', () => {
    expect(hasSilenceEvidence(stats(SILENCE_RMS_THRESHOLD, 1000, 1000))).toBe(false)
  })
})

describe('judgeOsMicMute', () => {
  // 迴歸：Plantronics Blackwire 5220 停在 GetMute=true 但音訊照常流動，
  // 直接採信系統標誌會導致每次按熱鍵都先誤報一次「麥克風已被靜音」。
  it('音訊在流動時立刻丟棄系統的靜音標誌', () => {
    expect(judgeOsMicMute(0, 'voiced', 1600)).toEqual({ verdict: 'dismissed' })
    expect(judgeOsMicMute(0, 'low', 1600)).toEqual({ verdict: 'dismissed' })
  })

  it('已攢到快要確認時，一幀非零訊號也能推翻它', () => {
    expect(judgeOsMicMute(OS_MIC_MUTE_CONFIRM_SAMPLES - 1, 'low', 1600))
      .toEqual({ verdict: 'dismissed' })
  })

  it('全 0 取樣未達閾值時繼續攢證據，不彈警告', () => {
    expect(judgeOsMicMute(0, 'muted', 1600)).toEqual({ verdict: 'wait', silentSamples: 1600 })
    expect(judgeOsMicMute(1600, 'muted', 1600)).toEqual({ verdict: 'wait', silentSamples: 3200 })
  })

  it('全 0 取樣累計到閾值才確認真被靜音', () => {
    expect(judgeOsMicMute(OS_MIC_MUTE_CONFIRM_SAMPLES - 1600, 'muted', 1600))
      .toEqual({ verdict: 'confirmed' })
  })
})

describe('classifyMicLevel', () => {
  it('只有 PCM 峰值嚴格為 0 才判為 muted', () => {
    expect(classifyMicLevel(0, 0)).toBe('muted')
    expect(classifyMicLevel(0.05, MIC_NO_SIGNAL_PEAK_THRESHOLD)).toBe('muted')
  })

  it('任何非零輸入即使極小也判為 low', () => {
    expect(classifyMicLevel(Number.MIN_VALUE, Number.MIN_VALUE)).toBe('low')
    expect(classifyMicLevel(0.004, 0.05)).toBe('low')
    expect(classifyMicLevel(MIC_LOW_RMS_THRESHOLD - 0.0001, 1 / 32768)).toBe('low')
  })

  it('RMS 達到正常水平 判為 voiced', () => {
    expect(classifyMicLevel(0.03, 0.2)).toBe('voiced')
    expect(classifyMicLevel(MIC_LOW_RMS_THRESHOLD, 0.1)).toBe('voiced')
  })
})

describe('summarizeAppContext', () => {
  it('null 返回 null', () => {
    expect(summarizeAppContext(null)).toBeNull()
  })

  it('提取關鍵欄位', () => {
    const result = summarizeAppContext({
      processName: 'code.exe',
      exePath: 'C:\\Program Files\\Code\\code.exe',
      windowTitle: 'secret-doc.md',
      windowClass: 'Chrome_WidgetWin_1',
      focusClass: 'Chrome_RenderWidgetHostHWND',
      controlType: 'Edit',
    })
    expect(result?.processName).toBe('code.exe')
    // [自訂] S31：視窗標題與執行檔路徑不得進日誌摘要
    expect(result).not.toHaveProperty('windowTitle')
    expect(result).not.toHaveProperty('exePath')
  })
})

describe('buildStatsAppId', () => {
  it('優先使用 processName', () => {
    expect(buildStatsAppId({ processName: 'code.exe' } as any)).toBe('code.exe')
  })

  it('processName 為空時用 exePath 最後一段', () => {
    expect(buildStatsAppId({ exePath: 'C:\\Apps\\notepad.exe' } as any)).toBe('notepad.exe')
  })

  it('都為空時用 promptAppId', () => {
    expect(buildStatsAppId(null, 'my-app')).toBe('my-app')
  })

  it('全部為空返回 unknown', () => {
    expect(buildStatsAppId(null)).toBe('unknown')
  })
})

describe('isModifierPTTSetting', () => {
  it('識別單個修飾鍵和組合中的任意修飾鍵', () => {
    expect(isModifierPTTSetting('AltLeft')).toBe(true)
    expect(isModifierPTTSetting('ControlRight')).toBe(true)
    expect(isModifierPTTSetting('ShiftLeft')).toBe(true)
    expect(isModifierPTTSetting('ControlLeft+KeyK')).toBe(true)
    expect(isModifierPTTSetting('ControlLeft+MetaLeft')).toBe(true)
  })

  it('非修飾鍵返回 false', () => {
    expect(isModifierPTTSetting('Space')).toBe(false)
    expect(isModifierPTTSetting('F1')).toBe(false)
    expect(isModifierPTTSetting('KeyK')).toBe(false)
    expect(isModifierPTTSetting(undefined)).toBe(false)
  })
})

describe('computeProcessingTimeoutMs', () => {
  it('server 模式基礎超時', () => {
    const ms = computeProcessingTimeoutMs(5, 'server')
    expect(ms).toBeGreaterThanOrEqual(15000)
    expect(ms).toBeLessThan(20000)
  })

  it('cloud_api 模式至少 30s', () => {
    const ms = computeProcessingTimeoutMs(1, 'cloud_api')
    expect(ms).toBeGreaterThanOrEqual(30000)
  })

  it('local 模式至少 30s', () => {
    const ms = computeProcessingTimeoutMs(1, 'local')
    expect(ms).toBeGreaterThanOrEqual(30000)
  })

  it('長音訊超時更長', () => {
    const short = computeProcessingTimeoutMs(5, 'server')
    const long = computeProcessingTimeoutMs(60, 'server')
    expect(long).toBeGreaterThan(short)
  })

  it('local 長錄音按時長放寬：5 分鐘錄音不會在識別跑完前超時', () => {
    // 原來本地最多等 45 s，而 5 分鐘的錄音實測要 55 s 左右
    expect(computeProcessingTimeoutMs(300, 'local')).toBeGreaterThanOrEqual(120000)
    expect(computeProcessingTimeoutMs(60, 'local')).toBe(45000) // 中等長度維持原值
    expect(computeProcessingTimeoutMs(3000, 'local')).toBeLessThanOrEqual(180000)
  })

  it('cloud_api 上限 90s', () => {
    const ms = computeProcessingTimeoutMs(600, 'cloud_api')
    expect(ms).toBeLessThanOrEqual(90000)
  })
})

describe('isUnconfirmedPaste', () => {
  it('send_input 卡住（真實那次 2088ms）要彈兜底卡片', () => {
    expect(isUnconfirmedPaste('send_input', 2088)).toBe(true)
    expect(isUnconfirmedPaste('send_input', SLOW_SEND_INPUT_PASTE_MS)).toBe(true)
  })

  it('正常的 send_input 仍按成功（日誌裡最慢 631ms）', () => {
    expect(isUnconfirmedPaste('send_input', 631)).toBe(false)
    expect(isUnconfirmedPaste('send_input', SLOW_SEND_INPUT_PASTE_MS - 1)).toBe(false)
  })

  it('會核實結果或同步等目標的方式，慢也不彈', () => {
    expect(isUnconfirmedPaste('wm_paste', 5000)).toBe(false)
    expect(isUnconfirmedPaste('console_paste', 5000)).toBe(false)
    expect(isUnconfirmedPaste(undefined, 5000)).toBe(false)
  })
})
