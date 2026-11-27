// [自訂] SC-63：點擊光輝開關（預設開），存在既有設定機制，App 掛載時套用到全域特效。

import { getSetting, setSetting } from '@/services/store'
import { installClickGlow, type ClickGlowController } from '@/lib/clickGlow'

export const CLICK_GLOW_SETTING_KEY = 'clickGlowEnabled'

let controller: ClickGlowController | null = null
let enabled = true

/** App 掛載時呼叫一次；回傳卸載函式。 */
export function startClickGlow(): () => void {
  controller?.dispose()
  const current = installClickGlow(document)
  controller = current
  current.setEnabled(enabled)
  void getSetting(CLICK_GLOW_SETTING_KEY, true)
    .catch(() => true)
    .then((value) => {
      enabled = value !== false
      if (controller === current) current.setEnabled(enabled)
    })
  return () => {
    current.dispose()
    if (controller === current) controller = null
  }
}

export async function loadClickGlowEnabled(): Promise<boolean> {
  const value = await getSetting(CLICK_GLOW_SETTING_KEY, true).catch(() => true)
  return value !== false
}

export async function setClickGlowEnabled(next: boolean): Promise<void> {
  enabled = next
  controller?.setEnabled(next)
  await setSetting(CLICK_GLOW_SETTING_KEY, next)
}
