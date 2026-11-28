// 「跟隨系統預設」時該開啟哪個麥克風。
//
// 起因（2026-11-07 本機日誌）：耳麥拔掉後 Windows 把預設錄音裝置換成了
// `CABLE Output (VB-Audio Virtual Cable)`，SayIt 照著跟過去，錄到的全是 0，
// 只彈了一句「未檢測到聲音」。使用者沒改過任何設定，從他的角度就是"它自己選到了虛擬音效卡"。
//
// 規則：
//  - 使用者在下拉里**明確選了**某個裝置 → 原樣使用，哪怕它是虛擬裝置（有人就是要從
//    虛擬音效卡錄：Voicemeeter、NVIDIA Broadcast 之類的處理鏈）。
//  - 跟隨系統預設、而系統預設是**虛擬裝置** → 改用一個真實麥克風：
//    上次真正錄到過人聲的那個 → 系統「預設通訊裝置」（如果是真實的）→ 第一個真實麥克風。
//  - 找不到任何真實麥克風、或者對不上號 → 維持原樣跟隨系統預設，不猜。
//
// 「是不是虛擬裝置」由原生層判斷，**不按名字**，所以不需要為每個虛擬音效卡產品維護名單：
// Windows 看裝置掛在哪條匯流排上（audio_mute.rs 的 is_virtual_enumerator），
// macOS 看 CoreAudio 報的連線型別（is_virtual_transport，BlackHole 之類報 `virt`）。

import { invoke } from '@tauri-apps/api/core'
import { isPseudoInputDevice, realInputEndpoints, stripUsbIds, type MicEndpoint } from './audio'
import { getSetting, setSetting } from './store'

/** 原生層 list_capture_endpoints 的一項。 */
export interface CaptureEndpoint {
  name: string
  enumerator: string
  isVirtual: boolean
  isDefault: boolean
  isDefaultCommunications: boolean
}

export type MicPlanReason =
  /** 使用者明確選了裝置 */
  | 'explicit'
  /** 跟隨系統預設，且預設是真實裝置（或無從判斷） */
  | 'default'
  /** 系統預設是虛擬裝置，已改用真實麥克風 */
  | 'virtual_avoided'
  /** 系統預設是虛擬裝置，但沒有可用的真實麥克風 */
  | 'virtual_no_alternative'
  /** 系統預設是虛擬裝置，但網頁側的裝置名是空的，對不上號（錄音開流後會再判一次） */
  | 'unmapped'

export interface MicPlan {
  /** 交給 getUserMedia 的 deviceId；空串 = 跟隨系統預設 */
  deviceId: string
  reason: MicPlanReason
  /** 被跳過的虛擬裝置名（reason = virtual_avoided / virtual_no_alternative / unmapped 時有值） */
  avoidedName: string
  /** 實際改用的麥克風名（reason = virtual_avoided 時有值） */
  chosenName: string
}

/** 記住「上次真正錄到人聲的真實麥克風」。存的是規範化後的名字，不是 deviceId：
 *  Chromium 的 deviceId 按 origin 加鹽，清 WebView 快取就全變號，名字更穩。 */
const LAST_GOOD_MIC_KEY = 'micLastGoodDevice'

/**
 * 把網頁側的 label 與原生側的端點友好名對到一起的鍵。
 * Chromium 會給 USB 裝置名後面加 `(047f:c053)`，Windows 的友好名裡沒有，所以先剝掉。
 */
export function endpointKey(label: string): string {
  return stripUsbIds(label).replace(/\s+/g, ' ').trim().toLocaleLowerCase()
}

/** 某個網頁側裝置名對應的原生端點是不是虛擬裝置。對不上或同名裡有真有假 → null（不下結論）。 */
export function isVirtualLabel(label: string, endpoints: CaptureEndpoint[]): boolean | null {
  const key = endpointKey(label)
  if (!key) return null
  const hits = endpoints.filter((endpoint) => endpointKey(endpoint.name) === key)
  if (hits.length === 0) return null
  if (hits.every((endpoint) => endpoint.isVirtual)) return true
  if (hits.every((endpoint) => !endpoint.isVirtual)) return false
  return null
}

function asEndpointList(raw: unknown): CaptureEndpoint[] {
  if (!Array.isArray(raw)) return []
  return raw.filter((item): item is CaptureEndpoint => (
    !!item && typeof item === 'object'
    && typeof (item as CaptureEndpoint).name === 'string'
    && typeof (item as CaptureEndpoint).isVirtual === 'boolean'
  ))
}

/** 純函式：給定兩側的裝置清單，決定開啟哪個。 */
export function planMicDevice(input: {
  selectedMicId: string
  webDevices: MicEndpoint[]
  endpoints: CaptureEndpoint[]
  lastGoodKey: string
}): MicPlan {
  const selected = input.selectedMicId.trim()
  if (selected && !isPseudoInputDevice(selected)) {
    return { deviceId: selected, reason: 'explicit', avoidedName: '', chosenName: '' }
  }

  const followDefault: MicPlan = { deviceId: '', reason: 'default', avoidedName: '', chosenName: '' }
  const defaultEndpoint = input.endpoints.find((endpoint) => endpoint.isDefault)
  if (!defaultEndpoint || !defaultEndpoint.isVirtual) return followDefault

  const avoidedName = defaultEndpoint.name
  const named = realInputEndpoints(input.webDevices)
  if (named.length === 0) {
    return { deviceId: '', reason: 'unmapped', avoidedName, chosenName: '' }
  }

  const candidates = named.filter((device) => isVirtualLabel(device.label, input.endpoints) === false)
  if (candidates.length === 0) {
    return { deviceId: '', reason: 'virtual_no_alternative', avoidedName, chosenName: '' }
  }

  const commsKeys = new Set(
    input.endpoints
      .filter((endpoint) => endpoint.isDefaultCommunications && !endpoint.isVirtual)
      .map((endpoint) => endpointKey(endpoint.name)),
  )
  const lastGood = input.lastGoodKey.trim()
  const chosen = (lastGood && candidates.find((device) => endpointKey(device.label) === lastGood))
    || candidates.find((device) => commsKeys.has(endpointKey(device.label)))
    || candidates[0]

  return {
    deviceId: chosen.deviceId,
    reason: 'virtual_avoided',
    avoidedName,
    chosenName: stripUsbIds(chosen.label),
  }
}

export async function listCaptureEndpoints(): Promise<CaptureEndpoint[]> {
  try {
    return asEndpointList(await invoke('list_capture_endpoints'))
  } catch {
    return []
  }
}

async function snapshotWebInputs(): Promise<MicEndpoint[]> {
  try {
    const list = await navigator.mediaDevices.enumerateDevices()
    return list
      .filter((device) => device.kind === 'audioinput')
      .map((device) => ({
        deviceId: String(device.deviceId || ''),
        groupId: String(device.groupId || ''),
        label: String(device.label || ''),
      }))
  } catch {
    return []
  }
}

/**
 * 錄音啟動前呼叫。系統預設是真實裝置時只多一次原生呼叫（幾毫秒），不列舉網頁裝置。
 * 任何一步失敗都退回「跟隨系統預設」，不讓這一層擋住錄音。
 */
export async function resolveMicPlan(
  selectedMicId: string,
  options: { endpoints?: CaptureEndpoint[]; webDevices?: MicEndpoint[] } = {},
): Promise<{ plan: MicPlan; endpoints: CaptureEndpoint[] }> {
  // 明確選了裝置時也要端點清單：錄音沒聲音時要靠它判斷「是不是選了個虛擬音效卡」。
  const endpoints = options.endpoints ?? await listCaptureEndpoints()
  if (selectedMicId.trim() && !isPseudoInputDevice(selectedMicId)) {
    return { plan: planMicDevice({ selectedMicId, webDevices: [], endpoints, lastGoodKey: '' }), endpoints }
  }
  if (!endpoints.some((endpoint) => endpoint.isDefault && endpoint.isVirtual)) {
    return { plan: planMicDevice({ selectedMicId: '', webDevices: [], endpoints, lastGoodKey: '' }), endpoints }
  }
  const [webDevices, lastGoodKey] = await Promise.all([
    options.webDevices ? Promise.resolve(options.webDevices) : snapshotWebInputs(),
    getSetting<string>(LAST_GOOD_MIC_KEY, '').catch(() => ''),
  ])
  return {
    plan: planMicDevice({ selectedMicId: '', webDevices, endpoints, lastGoodKey: String(lastGoodKey || '') }),
    endpoints,
  }
}

let lastGoodCache: string | null = null

/** 一次錄音裡確認聽到了人聲、且用的是真實麥克風時呼叫。名字沒變就不寫庫。 */
export async function rememberWorkingMic(label: string): Promise<void> {
  const key = endpointKey(label)
  if (!key) return
  if (lastGoodCache === null) {
    lastGoodCache = String(await getSetting<string>(LAST_GOOD_MIC_KEY, '').catch(() => '') || '')
  }
  if (lastGoodCache === key) return
  lastGoodCache = key
  await setSetting(LAST_GOOD_MIC_KEY, key).catch(() => { })
}
