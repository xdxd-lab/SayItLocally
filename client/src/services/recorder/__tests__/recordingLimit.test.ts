import { describe, it, expect } from 'vitest'
import {
  formatRecordingLimit,
  formatRecordingTimer,
  MAX_RECORDING_SEC,
  RECORDING_COUNTDOWN_SEC,
} from '../types'

describe('formatRecordingTimer', () => {
  it('上限之前顯示已錄秒數', () => {
    expect(formatRecordingTimer(0)).toMatchObject({ text: '0s', countdown: false })
    expect(formatRecordingTimer(12.7)).toMatchObject({ text: '12s', countdown: false })
    // 距上限還剩 61 秒，還沒進入倒計時
    expect(formatRecordingTimer(239)).toMatchObject({ text: '239s', countdown: false })
  })

  it('最後一分鐘改為顯示剩餘時間', () => {
    // 正好剩 60 秒時進入倒計時
    expect(formatRecordingTimer(240)).toMatchObject({ text: '剩餘 60s', countdown: true, remainingSec: 60 })
    expect(formatRecordingTimer(253)).toMatchObject({ text: '剩餘 47s', countdown: true, remainingSec: 47 })
    expect(formatRecordingTimer(299)).toMatchObject({ text: '剩餘 1s', countdown: true, remainingSec: 1 })
  })

  it('到點及越界都停在 0，不出現負數', () => {
    expect(formatRecordingTimer(300)).toMatchObject({ text: '剩餘 0s', remainingSec: 0 })
    expect(formatRecordingTimer(999)).toMatchObject({ text: '剩餘 0s', remainingSec: 0 })
  })

  it('異常輸入不炸', () => {
    expect(formatRecordingTimer(-5)).toMatchObject({ text: '0s', countdown: false })
    expect(formatRecordingTimer(Number.NaN)).toMatchObject({ text: '0s', countdown: false })
  })

  it('倒計時視窗必須小於上限（否則一開錄就在倒計時）', () => {
    expect(RECORDING_COUNTDOWN_SEC).toBeGreaterThan(0)
    expect(RECORDING_COUNTDOWN_SEC).toBeLessThan(MAX_RECORDING_SEC)
  })

  it('上限必須與 Rust 側的硬釋放時間一致（改一處忘另一處會讓提示與實際不符）', () => {
    // keyboard/mod.rs: const HARD_RELEASE_AFTER_SECS: u64 = 5 * 60
    expect(MAX_RECORDING_SEC).toBe(5 * 60)
  })
})

describe('formatRecordingLimit', () => {
  it('文案隨上限自動變化（避免改了上限忘了改文案）', () => {
    // 當前上限的中文說明；介面文案直接用它渲染
    expect(formatRecordingLimit()).toBe(
      MAX_RECORDING_SEC < 60
        ? `${MAX_RECORDING_SEC} 秒`
        : MAX_RECORDING_SEC % 60 === 0
          ? `${MAX_RECORDING_SEC / 60} 分鐘`
          : `${Math.floor(MAX_RECORDING_SEC / 60)} 分 ${MAX_RECORDING_SEC % 60} 秒`,
    )
  })
})
