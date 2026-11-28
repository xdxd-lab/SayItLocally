import { describe, expect, it } from 'vitest'
import type { MicEndpoint } from '../../audio'
import { buildMicOptions } from '@/features/settings/utils'
import {
  cleanActiveMicLabel,
  describeMicSource,
  micSourceChanged,
} from '../micSourceReminder'

/** 本機日誌裡的真實形狀：結尾還掛著 USB 的 VID:PID。 */
const HEADSET_RAW = '耳機式麥克風 (Plantronics Blackwire 5220 Series) (047f:c053)'
/** 兩處都該顯示的這一串：剝掉本地化字首與 USB 標識，括號裡的型號保留。 */
const HEADSET_SHOWN = '耳機式麥克風 (Plantronics Blackwire 5220 Series)'

const headsetSnapshot: MicEndpoint[] = [
  { deviceId: 'default', groupId: 'plt-group', label: `預設值 - ${HEADSET_RAW}` },
  { deviceId: 'communications', groupId: 'plt-group', label: `通訊裝置 - ${HEADSET_RAW}` },
  { deviceId: 'real-headset', groupId: 'plt-group', label: HEADSET_RAW },
]

function followingSystemDefault(devices = headsetSnapshot) {
  return {
    deviceId: 'default',
    groupId: 'plt-group',
    label: `預設值 - ${HEADSET_RAW}`,
    devices,
  }
}

describe('microphone source reminder', () => {
  it('describes the actual endpoint selected by system-default mode', () => {
    const source = describeMicSource({
      deviceId: 'default',
      groupId: 'plantronics-group',
      label: 'Default - Headset Microphone (Plantronics Blackwire 5220 Series)',
      devices: [],
    }, '', 'Microphone')

    expect(source).toEqual({
      identity: 'auto:group:plantronics-group:headset microphone (plantronics blackwire 5220 series)',
      mode: 'auto',
      // 拿不到快照時只能靠字面清理剝字首；括號裡的型號照樣保留。
      label: 'Headset Microphone (Plantronics Blackwire 5220 Series)',
    })
  })

  it('treats fixed and auto routing as different even on the same endpoint', () => {
    const endpoint = {
      deviceId: 'physical-device-1',
      groupId: 'group-1',
      label: 'USB Microphone',
      devices: [],
    }

    const automatic = describeMicSource(endpoint, '', 'Microphone')
    const fixed = describeMicSource(endpoint, 'physical-device-1', 'Microphone')

    expect(automatic.identity).toBe('auto:device:physical-device-1')
    expect(fixed.identity).toBe('fixed:device:physical-device-1')
    expect(micSourceChanged(automatic.identity, fixed.identity)).toBe(true)
  })

  it('only reminds again when the input route changes', () => {
    expect(micSourceChanged(null, 'auto:device:a')).toBe(true)
    expect(micSourceChanged('auto:device:a', 'auto:device:a')).toBe(false)
    expect(micSourceChanged('auto:device:a', 'auto:device:b')).toBe(true)
    expect(micSourceChanged('auto:device:b', 'auto:device:a')).toBe(true)
  })

  it('cleans only technical label noise and keeps model names intact', () => {
    expect(cleanActiveMicLabel('Communications: Studio Mic (1234:abcd)'))
      .toBe('Studio Mic')
    expect(cleanActiveMicLabel('Mic (Plantronics Blackwire 5220 Series)'))
      .toBe('Mic (Plantronics Blackwire 5220 Series)')
  })

  describe('the overlay hint and the settings dropdown must agree', () => {
    it('shows the exact same string as the settings dropdown', () => {
      // 同一個裝置在兩處叫不同的名字，使用者得先在腦子裡對一遍才能確認錄音走的是他選的
      // 那個 —— 這條斷言就是為了把兩邊釘在一起，改任何一邊都會在這裡失敗。
      const fromOverlay = describeMicSource(followingSystemDefault(), '', 'Microphone').label
      const fromSettings = buildMicOptions(headsetSnapshot, '', {
        systemDefault: '系統預設',
        systemDefaultWith: (device) => `系統預設（${device}）`,
        unnamed: (idPrefix) => `麥克風 ${idPrefix}`,
        unavailable: '上次選的麥克風當前不可用',
      }).find((option) => option.value === 'real-headset')?.label

      expect(fromOverlay).toBe(HEADSET_SHOWN)
      expect(fromSettings).toBe(HEADSET_SHOWN)
      expect(fromOverlay).toBe(fromSettings)
    })

    it('drops the localized pseudo-device prefix without matching it by text', () => {
      // 「預設值 - 」剝掉不是靠正則認中文，而是靠 groupId 找到那個真實端點。
      expect(describeMicSource(followingSystemDefault(), '', 'Microphone').label)
        .toBe(HEADSET_SHOWN)
    })

    it('drops the trailing USB vid:pid, which identifies nothing', () => {
      // 同型號兩台裝置的 VID:PID 完全一樣，留著只佔寬度。
      const label = describeMicSource(followingSystemDefault(), '', 'Microphone').label
      expect(label).not.toContain('047f')
    })

    it('falls back to suffix matching when the pseudo device carries no group id', () => {
      const snapshot: MicEndpoint[] = [
        { deviceId: 'default', groupId: '', label: `預設值 - ${HEADSET_RAW}` },
        { deviceId: 'real-headset', groupId: 'plt-group', label: HEADSET_RAW },
      ]
      const source = describeMicSource(
        { ...followingSystemDefault(snapshot), groupId: '' },
        '',
        'Microphone',
      )
      expect(source.label).toBe(HEADSET_SHOWN)
      // 解析到真實端點之後 identity 就是穩定的物理標識，不再摻 label ——
      // 否則系統在 eConsole / eCommunications 之間切預設裝置就會被當成換了麥克風。
      expect(source.identity).toBe('auto:device:real-headset')
    })

    it('keeps the parent device name even when it is a sound-card driver name', () => {
      // issue #72 那台機器。這一串長到會被截斷，但兩處一致比短更重要。
      const array = 'Microphone Array (2- Intel(R) Smart Sound Technology for Digital Microphones)'
      const snapshot: MicEndpoint[] = [
        { deviceId: 'real-array', groupId: 'intel-group', label: array },
      ]
      expect(describeMicSource(
        { deviceId: 'real-array', groupId: 'intel-group', label: array, devices: snapshot },
        'real-array',
        'Microphone',
      ).label).toBe(array)
    })

    it('never returns an empty label, whatever the system reports', () => {
      expect(describeMicSource(
        { deviceId: '', groupId: '', label: '', devices: [] },
        '',
        '麥克風',
      ).label).toBe('麥克風')
    })

    it('still strips the usb id when the device snapshot is unavailable', () => {
      expect(describeMicSource(
        { deviceId: 'real-headset', groupId: 'plt-group', label: HEADSET_RAW, devices: [] },
        'real-headset',
        'Microphone',
      ).label).toBe(HEADSET_SHOWN)
    })
  })
})
