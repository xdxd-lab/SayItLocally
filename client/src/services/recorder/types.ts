export type RecorderState = 'idle' | 'recording' | 'processing'
import { t } from '@/i18n'
import type { CustomWaveColors, OverlayWaveTheme } from '@/lib/overlayWaveColors'

export type { CustomWaveColors, OverlayWaveTheme }

export type OverlayWidthPreset = 'short' | 'medium' | 'long'

export interface OverlayWidthConfig {
  barCount: number
  windowWidth: number
}

export const OVERLAY_WIDTH_PRESETS: Record<OverlayWidthPreset, OverlayWidthConfig> = {
  short: { barCount: 12, windowWidth: 200 },
  medium: { barCount: 18, windowWidth: 280 },
  long: { barCount: 24, windowWidth: 360 },
}

export interface OverlayCommonPayload {
  theme: OverlayWaveTheme
  /**
   * 自定義配色。不管當前是不是 custom 主題都帶上：原生側對 payload 是整份替換，
   * 只在 custom 時才帶的話，重放底稿時這個欄位可能恰好缺席（見 OverlayService 裡
   * activeFailureCard 的說明）。三個短字串，成本可以忽略。
   */
  customWave: CustomWaveColors
  showDuration: boolean
  baseWidth?: number
  barCount?: number
  /**
   * 介面語言，隨每次 overlay 更新一起下發。
   *
   * 懸浮窗是**獨立的 React 根**，讀不到主窗的記憶體狀態；而它必須在按下快捷鍵後
   * 立刻出現，不能為了讀一次設定加一條非同步初始化（overlay 建立本身就脆，
   * 見 pitfalls 7）。所以複用這條現成通道 —— payload 在 Rust 側是不透明 JSON，
   * 多帶一個欄位是零成本的。
   */
  locale?: string
}

export interface PTTEventPayload {
  source?: string
  keycode?: number
  rawcode?: number
  altKey?: boolean
  ctrlKey?: boolean
  shiftKey?: boolean
  reason?: string
  pttSetting?: string
  timestamp?: number
}

/**
 * 單次錄音上限（秒）。到點會自動結束本次錄音並進入識別。
 *
 * ⚠️ 必須與 Rust 側 `keyboard/mod.rs` 的 HARD_RELEASE_AFTER_SECS 保持一致
 * （按住說話的硬釋放在 Rust 看門狗裡；擴音的自動停止在 RecorderOrchestrator）。
 */
export const MAX_RECORDING_SEC = 300

/** 距上限還剩多少秒時，計時區改為顯示倒計時（提醒即將到點） */
export const RECORDING_COUNTDOWN_SEC = 60

/** 上限的可讀說明（如「5 分鐘」/「5 min」）。介面文案一律用它，避免改了上限忘了改文案。 */
export function formatRecordingLimit(): string {
  if (MAX_RECORDING_SEC < 60) return t('duration.seconds', { count: MAX_RECORDING_SEC })
  const minutes = Math.floor(MAX_RECORDING_SEC / 60)
  const seconds = MAX_RECORDING_SEC % 60
  return seconds === 0
    ? t('duration.minutes', { count: minutes })
    : t('duration.minutesSeconds', { minutes, seconds })
}

/**
 * 懸浮窗計時區要顯示的內容。
 *
 * 最後一分鐘把"已錄多久"換成"還剩多久"：到點會自動結束，此時使用者更需要知道剩餘時間。
 * 文案保持緊湊（`剩餘 47s`）—— 膠囊寬度按波形條數固定，太長會被裁掉。
 */
export function formatRecordingTimer(elapsedSec: number): {
  text: string
  countdown: boolean
  remainingSec: number
} {
  const elapsed = Math.max(0, Math.floor(elapsedSec || 0))
  const remainingSec = Math.max(0, MAX_RECORDING_SEC - elapsed)
  const countdown = remainingSec <= RECORDING_COUNTDOWN_SEC
  return {
    text: countdown ? t('recording.remainingSeconds', { seconds: remainingSec }) : `${elapsed}s`,
    countdown,
    remainingSec,
  }
}
