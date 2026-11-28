import { isPseudoInputDevice, matchRealEndpoint, realInputEndpoints, stripUsbIds } from '@/services/audio'
import type { MicEndpoint } from '@/services/audio'

export type { OverlayWaveTheme } from '@/lib/overlayWaveColors'

// 快捷鍵對映已收斂到 @/lib/shortcutKeys（單一資料來源），這裡透傳匯出以保持既有引用不變。
export {
  displayAccelerator,
  eventToAccelerator,
  getSingleKeyDisplay,
  keyEventToShortcutCandidate,
  resolveSingleKeyShortcut,
} from '@/lib/shortcutKeys'

export interface MicOption {
  value: string
  label: string
  /** 與 label 同一串。裝置名常常長到被 truncate 截掉，而截掉的那段（USB 字尾）
   *  恰好是辨認裝置的依據，所以完整名字必須能懸停看到。 */
  title: string
}

/** 只有 `default` 那條偽裝置代表「系統預設」；`communications` 是通訊用的另一路。 */
const SYSTEM_DEFAULT_PSEUDO_ID = 'default'

/**
 * 麥克風下拉的選項。第一項永遠是「系統預設」（value 為空串）。
 *
 * 三条刻意的取舍：
 *
 * 1. **裝置名保留型號，只剝掉 USB 標識。** 這裡和懸浮窗那行提示的目標不同：懸浮窗是
 *    一瞬而過的確認，只要主名；這裡是使用者主動辨認裝置的地方，括號裡的型號
 *    （`Plantronics Blackwire 5220 Series`）必須留著，那是區分裝置的依據。
 *    結尾的 `(047f:c053)` 例外 —— 見 stripUsbIds，它連同型號都區分不了。
 *    寬度不夠時截斷，完整名字放在 title 裡。
 *
 * 2. **「系統預設」要寫明它當前指向誰。** 光寫「系統預設」等於沒說，使用者看不出錄音會
 *    走哪個麥克風。Chromium 的 `default` 偽裝置就攜帶這條資訊（label 是
 *    `<本地化字首> - <真實端點名>`、groupId 指向那個端點），用 matchRealEndpoint 落地。
 *
 * 3. **偽裝置不單獨成項。** 它們和第一項是同一件事的兩種寫法，列出來的話一個麥克風會
 *    佔掉三四個選項，選哪個都一樣卻看著像不同裝置。
 *
 * 選中的 deviceId 不在列表裡時補一條佔位項：不補的話 Select 找不到選中值，會退回佔位符
 * 文案 —— 介面看起來像"沒有選擇麥克風"，而使用者其實選過，也就無從知道該改什麼。走到這裡
 * 的真實場景是裝置被拔掉、換了機器、清過 WebView 快取（Chromium 的 deviceId 按 origin
 * 加鹽，清掉之後全部變號）。刻意**不自動改回系統預設**：那會靜默替掉使用者的選擇，
 * 這裡只負責把狀態說出來。
 */
export function buildMicOptions(
  devices: MicEndpoint[],
  selectedMic: string,
  labels: {
    systemDefault: string
    systemDefaultWith: (deviceName: string) => string
    unnamed: (idPrefix: string) => string
    unavailable: string
  },
): MicOption[] {
  const pseudoDefault = devices.find(
    (device) => device.deviceId.trim().toLowerCase() === SYSTEM_DEFAULT_PSEUDO_ID,
  )
  const resolvedDefault = pseudoDefault ? matchRealEndpoint(pseudoDefault, devices) : null
  const resolvedName = stripUsbIds(resolvedDefault?.label ?? '')

  const withTitle = (value: string, label: string): MicOption => ({ value, label, title: label })

  const options: MicOption[] = [
    withTitle('', resolvedName ? labels.systemDefaultWith(resolvedName) : labels.systemDefault),
    ...realInputEndpoints(devices).map((device) => withTitle(
      device.deviceId,
      // 裝置名來自系統，不翻譯；只有"讀不到名字"時的兜底標籤跟介面語言。
      stripUsbIds(device.label) || labels.unnamed(device.deviceId.slice(0, 8)),
    )),
  ]
  if (selectedMic && !options.some((option) => option.value === selectedMic)) {
    const connected = devices.some((device) => device.deviceId === selectedMic && !isPseudoInputDevice(device.deviceId))
    options.push(withTitle(selectedMic, connected ? labels.unnamed(selectedMic.slice(0, 8)) : labels.unavailable))
  }
  return options
}
