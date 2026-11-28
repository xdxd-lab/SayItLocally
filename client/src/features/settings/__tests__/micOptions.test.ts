import { describe, expect, it } from 'vitest'
import type { MicEndpoint } from '@/services/audio'
import { buildMicOptions } from '../utils'

const labels = {
  systemDefault: '系统默认',
  systemDefaultWith: (device: string) => `系统默认（${device}）`,
  unnamed: (idPrefix: string) => `麦克风 ${idPrefix}`,
  unavailable: '上次选的麦克风当前不可用',
}

/**
 * `enumerateDevices()` 在 Windows 上返回的真實形狀，取樣自本機日誌：一個真實端點，
 * 外加 Chromium 自己擺的兩個偽裝置，label 是 `<本地化字首> - <真實端點名>`。
 */
const RAW = '耳机式麦克风 (Plantronics Blackwire 5220 Series) (047f:c053)'
/** 下拉里該顯示的：留型號，剝掉結尾的 USB 標識。 */
const SHOWN = '耳机式麦克风 (Plantronics Blackwire 5220 Series)'

const snapshot: MicEndpoint[] = [
  { deviceId: 'default', groupId: 'plt-group', label: `默认值 - ${RAW}` },
  { deviceId: 'communications', groupId: 'plt-group', label: `通信设备 - ${RAW}` },
  { deviceId: 'real-headset', groupId: 'plt-group', label: RAW },
]

describe('麦克风下拉的选项', () => {
  it('保留括号里的型号，只剥掉结尾的 USB 标识', () => {
    // 型號是區分裝置的依據，必須留；VID:PID 連同型號都區分不了，只佔寬度。
    const options = buildMicOptions(snapshot, '', labels)

    expect(options.map((option) => option.value)).toEqual(['', 'real-headset'])
    expect(options[1].label).toBe(SHOWN)
    expect(options[1].label).not.toContain('047f')
  })

  it('「系统默认」写明它当前指向哪个设备', () => {
    // 只寫「系統預設」等於沒說，使用者看不出錄音會走哪個麥克風。
    const options = buildMicOptions(snapshot, '', labels)
    expect(options[0]).toEqual({
      value: '',
      label: `系统默认（${SHOWN}）`,
      title: `系统默认（${SHOWN}）`,
    })
  })

  it('伪设备不单独成项', () => {
    // 列出來的話一個耳麥佔掉四個選項，選哪個都一樣卻看著像四個不同的裝置。
    const options = buildMicOptions(snapshot, '', labels)
    expect(options.some((option) => option.value === 'default')).toBe(false)
    expect(options.some((option) => option.value === 'communications')).toBe(false)
  })

  it('伪设备的 groupId 为空时靠后缀匹配解析出系统默认指向谁', () => {
    const noGroup: MicEndpoint[] = [
      { deviceId: 'default', groupId: '', label: `默认值 - ${RAW}` },
      { deviceId: 'real-headset', groupId: 'plt-group', label: RAW },
    ]
    expect(buildMicOptions(noGroup, '', labels)[0].label).toBe(`系统默认（${SHOWN}）`)
  })

  it('解析不出系统默认指向谁时，退回朴素的「系统默认」而不是显示空括号', () => {
    const orphan: MicEndpoint[] = [
      { deviceId: 'default', groupId: 'gone', label: '默认值 - 某个已消失的设备' },
    ]
    expect(buildMicOptions(orphan, '', labels)[0].label).toBe('系统默认')
    expect(buildMicOptions([], '', labels)[0].label).toBe('系统默认')
  })

  it('每一项都带完整名字的 title，截断之后还能悬停看到', () => {
    const options = buildMicOptions(snapshot, '', labels)
    expect(options.every((option) => option.title === option.label)).toBe(true)
  })

  it('读不到名字时给一个带 id 前缀的兜底标签，不渲染空白项', () => {
    const unnamed: MicEndpoint[] = [
      { deviceId: 'abcdef1234567890', groupId: 'g', label: '' },
    ]
    // label 為空的條目不算真實端點（無從辨認），所以列表裡只剩「系統預設」。
    expect(buildMicOptions(unnamed, '', labels).map((option) => option.value)).toEqual([''])
    // 已選裝置仍在清單內，使用無名標籤。
    const selected = buildMicOptions(unnamed, 'abcdef1234567890', labels)
    expect(selected[selected.length - 1]).toEqual({
      value: 'abcdef1234567890',
      label: '麦克风 abcdef12',
      title: '麦克风 abcdef12',
    })
  })

  it('选中的设备不在列表里时补一条占位项', () => {
    // 不補的話 Select 找不到選中值、退回佔位符文案，介面看起來像"沒有選擇麥克風"，
    // 而使用者其實選過 —— 拔掉裝置、換機器、清過 WebView 快取都會走到這裡。
    const options = buildMicOptions(snapshot, 'gone-device', labels)

    expect(options.map((option) => option.value)).toEqual(['', 'real-headset', 'gone-device'])
    expect(options[2].label).toBe('上次选的麦克风当前不可用')
  })

  it('选中系统默认或列表里的设备时都不补占位项', () => {
    expect(buildMicOptions(snapshot, '', labels)).toHaveLength(2)
    expect(buildMicOptions(snapshot, 'real-headset', labels)).toHaveLength(2)
  })

  it('设备列表还没加载出来时，系统默认仍然是可选中的', () => {
    // 進設定頁的第一幀 devices 是空陣列。此時若連繫統預設都匹配不上，介面會先空一下。
    expect(buildMicOptions([], '', labels)).toEqual([
      { value: '', label: '系统默认', title: '系统默认' },
    ])
  })
})
