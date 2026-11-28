import { describe, it, expect } from 'vitest'
import { clampSec, elapsedSecFromPerf, normalizeDurations, pickVoiceDurationSec } from '../timeModel'

describe('clampSec', () => {
  it('正數不變', () => {
    expect(clampSec(5.5)).toBe(5.5)
  })

  it('負數變 0', () => {
    expect(clampSec(-1)).toBe(0)
  })

  it('NaN 變 0', () => {
    expect(clampSec(NaN)).toBe(0)
  })

  it('Infinity 變 0', () => {
    expect(clampSec(Infinity)).toBe(0)
  })
})

describe('elapsedSecFromPerf', () => {
  it('計算正確的時間差', () => {
    const result = elapsedSecFromPerf(1000, 2500)
    expect(result).toBeCloseTo(1.5)
  })

  it('startPerf 為 0 返回 0', () => {
    expect(elapsedSecFromPerf(0)).toBe(0)
  })

  it('startPerf 為負數返回 0', () => {
    expect(elapsedSecFromPerf(-100)).toBe(0)
  })

  it('startPerf 為 NaN 返回 0', () => {
    expect(elapsedSecFromPerf(NaN)).toBe(0)
  })
})

describe('normalizeDurations', () => {
  it('正常值不變', () => {
    const result = normalizeDurations({ holdSec: 5, audioSec: 4.8, asrSec: 4.5 })
    expect(result.holdSec).toBe(5)
    expect(result.audioSec).toBe(4.8)
    expect(result.asrSec).toBe(4.5)
  })

  it('負值和 0 被清理', () => {
    const result = normalizeDurations({ holdSec: -1, audioSec: 0, asrSec: -5 })
    expect(result.holdSec).toBe(0)
    expect(result.audioSec).toBeUndefined()
    expect(result.asrSec).toBeUndefined()
  })
})

describe('pickVoiceDurationSec', () => {
  it('優先使用 holdSec', () => {
    expect(pickVoiceDurationSec({ holdSec: 5, audioSec: 4, asrSec: 3 })).toBe(5)
  })

  it('holdSec 為 0 時回退到 asrSec', () => {
    expect(pickVoiceDurationSec({ holdSec: 0, audioSec: 4, asrSec: 3 })).toBe(3)
  })

  it('holdSec 和 asrSec 都為 0 時回退到 audioSec', () => {
    expect(pickVoiceDurationSec({ holdSec: 0, audioSec: 4 })).toBe(4)
  })

  it('全部為 0 返回 0', () => {
    expect(pickVoiceDurationSec({ holdSec: 0 })).toBe(0)
  })
})
