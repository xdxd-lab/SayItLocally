import { isPseudoInputDevice, matchRealEndpoint, stripUsbIds } from '../audio'
import type { ActiveMicrophoneInfo, MicEndpoint } from '../audio'

export type MicSourceMode = 'auto' | 'fixed'

export interface MicSourceDescriptor {
  identity: string
  mode: MicSourceMode
  label: string
}

/**
 * 字面清理，**只在拿不到裝置快照時兜底**。
 *
 * 它只認英文字首，所以中文系統上剝不掉「預設值 - 」—— 那個字首是 Chromium 按介面
 * 語言生成的，按字面匹配等於給每種語言維護一份，Windows 改寫法還會靜默失效。
 * 正路是 matchRealEndpoint：直接去快照裡取那個不帶字首的真實端點。
 */
export function cleanActiveMicLabel(label: string): string {
  return stripUsbIds(label.replace(/^\s*(?:default|communications)\s*[-\u2013\u2014:]\s*/i, ''))
}

export function describeMicSource(
  active: ActiveMicrophoneInfo,
  requestedDeviceId: string,
  fallbackLabel: string,
  devices: MicEndpoint[] = active.devices ?? [],
): MicSourceDescriptor {
  const mode: MicSourceMode = requestedDeviceId ? 'fixed' : 'auto'
  const real = matchRealEndpoint(active, devices)

  // 顯示名與設定頁的下拉**必須是同一串**：同一個裝置在兩處叫不同的名字，使用者得先
  // 在腦子裡對一遍才能確認錄音走的是他選的那個，這比名字長更費勁。所以這裡只做
  // 設定頁也做的兩件事 —— 解析掉本地化字首（matchRealEndpoint）、剝掉 USB 標識
  // （stripUsbIds），括号里的型号一律保留。
  //
  // 曾經在這裡按「同台機器上有沒有別人同名」折掉括號裡的型號，沒有保留：它讓兩處的
  // 名字不一致，而省下的寬度換不來這個代價。懸浮窗那行放不下時截斷即可。
  //
  // 拿不到裝置快照（列舉失敗）時退回 cleanActiveMicLabel 兜底 —— 它只認英文字首，
  // 中文系統上剝不掉「預設值 - 」，所以只是兜底，不是正路。
  const label = stripUsbIds(real?.label ?? '') || cleanActiveMicLabel(active.label) || fallbackLabel

  const deviceId = real?.deviceId.trim() || active.deviceId.trim()
  const groupId = real?.groupId.trim() || active.groupId.trim()

  // 解析到真實端點時 deviceId 是穩定的物理標識。解析不到才退回 groupId / 名字 ——
  // 那兩條都摻了 label，而偽裝置的 label 會隨系統在 eConsole / eCommunications 之間
  // 切預設裝置而變（「預設值 -」↔「通訊裝置 -」），於是同一個麥克風被當成換了裝置。
  const physicalIdentity = !isPseudoInputDevice(deviceId)
    ? `device:${deviceId}`
    : groupId
      ? `group:${groupId}:${label.toLocaleLowerCase()}`
      : `label:${label.toLocaleLowerCase()}`

  // The routing mode is intentional user-facing information. Switching between
  // auto-detect and a fixed endpoint should be confirmed even if both resolve to
  // the same physical microphone.
  return {
    identity: `${mode}:${physicalIdentity}`,
    mode,
    label,
  }
}

export function micSourceChanged(previousIdentity: string | null, nextIdentity: string): boolean {
  return previousIdentity !== nextIdentity
}
