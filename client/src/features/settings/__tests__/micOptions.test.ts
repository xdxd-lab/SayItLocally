import { describe, expect, it } from 'vitest'
import type { MicEndpoint } from '@/services/audio'
import { buildMicOptions } from '../utils'

const labels = {
  systemDefault: '系統預設',
  systemDefaultWith: (device: string) => `系統預設（${device}）`,
  unnamed: (idPrefix: string) => `麥克風 ${idPrefix}`,
  unavailable: '上次選的麥克風當前不可用',
}

/**
 * `enumerateDevices()` 在 Windows 上返回的真實形狀，取樣自本機日誌：一個真實端點，
 * 外加 Chromium 自己擺的兩個偽裝置，label 是 `<本地化字首> - <真實端點名>`。
 */
const RAW = '耳機式麥克風 (Plantronics Blackwire 5220 Series) (047f:c053)'
/** 下拉里該顯示的：留型號，剝掉結尾的 USB 標識。 */
const SHOWN = '耳機式麥克風 (Plantronics Blackwire 5220 Series)'

const snapshot: MicEndpoint[] = [
  { deviceId: 'default', groupId: 'plt-group', label: `預設值 - ${RAW}` },
  { deviceId: 'communications', groupId: 'plt-group', label: `通訊裝置 - ${RAW}` },
  { deviceId: 'real-headset', groupId: 'plt-group', label: RAW },
]

describe('麥克風下拉的選項', () => {
  it('保留括號裡的型號，只剝掉結尾的 USB 標識', () => {
    // 型號是區分裝置的依據，必須留；VID:PID 連同型號都區分不了，只佔寬度。
    const options = buildMicOptions(snapshot, '', labels)

    expect(options.map((option) => option.value)).toEqual(['', 'real-headset'])
    expect(options[1].label).toBe(SHOWN)
    expect(options[1].label).not.toContain('047f')
  })

  it('「系統預設」寫明它當前指向哪個裝置', () => {
    // 只寫「系統預設」等於沒說，使用者看不出錄音會走哪個麥克風。
    const options = buildMicOptions(snapshot, '', labels)
    expect(options[0]).toEqual({
      value: '',
      label: `系統預設（${SHOWN}）`,
      title: `系統預設（${SHOWN}）`,
    })
  })

  it('偽裝置不單獨成項', () => {
    // 列出來的話一個耳麥佔掉四個選項，選哪個都一樣卻看著像四個不同的裝置。
    const options = buildMicOptions(snapshot, '', labels)
    expect(options.some((option) => option.value === 'default')).toBe(false)
    expect(options.some((option) => option.value === 'communications')).toBe(false)
  })

  it('偽裝置的 groupId 為空時靠字尾匹配解析出系統預設指向誰', () => {
    const noGroup: MicEndpoint[] = [
      { deviceId: 'default', groupId: '', label: `預設值 - ${RAW}` },
      { deviceId: 'real-headset', groupId: 'plt-group', label: RAW },
    ]
    expect(buildMicOptions(noGroup, '', labels)[0].label).toBe(`系統預設（${SHOWN}）`)
  })

  it('解析不出系統預設指向誰時，退回樸素的「系統預設」而不是顯示空括號', () => {
    const orphan: MicEndpoint[] = [
      { deviceId: 'default', groupId: 'gone', label: '預設值 - 某個已消失的裝置' },
    ]
    expect(buildMicOptions(orphan, '', labels)[0].label).toBe('系統預設')
    expect(buildMicOptions([], '', labels)[0].label).toBe('系統預設')
  })

  it('每一項都帶完整名字的 title，截斷之後還能懸停看到', () => {
    const options = buildMicOptions(snapshot, '', labels)
    expect(options.every((option) => option.title === option.label)).toBe(true)
  })

  it('讀不到名字時給一個帶 id 字首的兜底標籤，不渲染空白項', () => {
    const unnamed: MicEndpoint[] = [
      { deviceId: 'abcdef1234567890', groupId: 'g', label: '' },
    ]
    // label 為空的條目不算真實端點（無從辨認），所以列表裡只剩「系統預設」。
    expect(buildMicOptions(unnamed, '', labels).map((option) => option.value)).toEqual([''])
    // 已選裝置仍在清單內，使用無名標籤。
    const selected = buildMicOptions(unnamed, 'abcdef1234567890', labels)
    expect(selected[selected.length - 1]).toEqual({
      value: 'abcdef1234567890',
      label: '麥克風 abcdef12',
      title: '麥克風 abcdef12',
    })
  })

  it('選中的裝置不在列表裡時補一條佔位項', () => {
    // 不補的話 Select 找不到選中值、退回佔位符文案，介面看起來像"沒有選擇麥克風"，
    // 而使用者其實選過 —— 拔掉裝置、換機器、清過 WebView 快取都會走到這裡。
    const options = buildMicOptions(snapshot, 'gone-device', labels)

    expect(options.map((option) => option.value)).toEqual(['', 'real-headset', 'gone-device'])
    expect(options[2].label).toBe('上次選的麥克風當前不可用')
  })

  it('選中系統預設或列表裡的裝置時都不補佔位項', () => {
    expect(buildMicOptions(snapshot, '', labels)).toHaveLength(2)
    expect(buildMicOptions(snapshot, 'real-headset', labels)).toHaveLength(2)
  })

  it('裝置列表還沒載入出來時，系統預設仍然是可選中的', () => {
    // 進設定頁的第一幀 devices 是空陣列。此時若連繫統預設都匹配不上，介面會先空一下。
    expect(buildMicOptions([], '', labels)).toEqual([
      { value: '', label: '系統預設', title: '系統預設' },
    ])
  })
})
