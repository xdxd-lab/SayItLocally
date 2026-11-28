import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  isPseudoInputDevice,
  listMicrophones,
  matchRealEndpoint,
  normalizeSelectedMicId,
  realInputEndpoints,
} from '../audio'

/**
 * `enumerateDevices()` 在 Windows 上返回的形狀：一個真實端點，外加 Chromium
 * 自己擺的兩個偽裝置（default / communications），label 帶本地化字首。
 * 取樣自本機日誌，不是編的。
 */
const HEADSET = '耳機式麥克風 (Plantronics Blackwire 5220 Series) (047f:c053)'

function device(deviceId: string, label: string, kind = 'audioinput') {
  return { deviceId, groupId: 'plt-group', kind, label, toJSON: () => ({}) }
}

function stubEnumerate(list: ReturnType<typeof device>[]) {
  const getUserMedia = vi.fn(async () => ({ getTracks: () => [] }))
  vi.stubGlobal('navigator', {
    mediaDevices: {
      enumerateDevices: vi.fn(async () => list),
      getUserMedia,
    },
  })
  return getUserMedia
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('microphone device list', () => {
  it('recognizes the two pseudo devices Chromium adds, and nothing else', () => {
    expect(isPseudoInputDevice('default')).toBe(true)
    expect(isPseudoInputDevice('communications')).toBe(true)
    expect(isPseudoInputDevice('DEFAULT')).toBe(true)
    expect(isPseudoInputDevice('  ')).toBe(true)
    expect(isPseudoInputDevice('a1b2c3')).toBe(false)
  })

  it('folds a stored pseudo-device id back to "follow system default"', () => {
    // 實測存量資料：`selectedMic = "default"`。老版本的下拉把偽裝置也列出來，使用者點了它。
    // 不折的話，設定頁的下拉找不到選中項，顯示成"沒有選擇麥克風"。
    expect(normalizeSelectedMicId('default')).toBe('')
    expect(normalizeSelectedMicId('communications')).toBe('')
    expect(normalizeSelectedMicId('  ')).toBe('')
    expect(normalizeSelectedMicId(undefined)).toBe('')
    expect(normalizeSelectedMicId(123)).toBe('')
    // 真實 deviceId 一個字都不能動 —— 改了就等於把使用者選的裝置換掉了。
    expect(normalizeSelectedMicId('real-headset')).toBe('real-headset')
  })

  it('keeps the pseudo devices but drops non-inputs', async () => {
    // 偽裝置攜帶唯一一條「系統預設現在指向誰」的資訊，這一層不能替呼叫方丟掉；
    // 「哪些該進下拉」是產品判斷，歸 buildMicOptions。
    stubEnumerate([
      device('default', `預設值 - ${HEADSET}`),
      device('communications', `通訊裝置 - ${HEADSET}`),
      device('real-headset', HEADSET),
      device('speaker', 'Speakers', 'audiooutput'),
    ])

    const mics = await listMicrophones()

    expect(mics.map((d) => d.deviceId)).toEqual(['default', 'communications', 'real-headset'])
  })

  it('resolves which real endpoint the system default currently points at', async () => {
    const devices = [
      { deviceId: 'default', groupId: 'plt-group', label: `預設值 - ${HEADSET}` },
      { deviceId: 'real-headset', groupId: 'plt-group', label: HEADSET },
    ]

    expect(matchRealEndpoint(devices[0], devices)?.deviceId).toBe('real-headset')
    // 真實 deviceId 直接按 id 命中，不走那兩層回落。
    expect(matchRealEndpoint(devices[1], devices)?.deviceId).toBe('real-headset')
    // 指向一個已經消失的裝置時返回 null，呼叫方據此退回樸素文案，而不是顯示空括號。
    expect(matchRealEndpoint({ deviceId: 'default', groupId: 'gone', label: '預設值 - 沒了' }, devices))
      .toBeNull()
  })

  it('treats endpoints without a readable name as not identifiable', async () => {
    const devices = [
      { deviceId: 'a', groupId: 'g', label: '' },
      { deviceId: 'b', groupId: 'g', label: 'Real Mic' },
    ]
    expect(realInputEndpoints(devices).map((d) => d.deviceId)).toEqual(['b'])
  })

  it('never requests microphone access when names are missing', async () => {
    const unavailable = device('', '')
    const getUserMedia = stubEnumerate([unavailable])

    expect(await listMicrophones()).toEqual([unavailable])
    expect(getUserMedia).not.toHaveBeenCalled()
  })
})
