/**
 * WebView 鍵盤迴退 — 當 SayIt 自身視窗聚焦時，WH_KEYBOARD_LL 鉤子不觸發。
 *
 * 原因：WebView2 (Chromium) 在同程序內攔截鍵盤訊息，導致 LL 鉤子回撥不被呼叫。
 * 解決：在前端 document 上監聽 keydown/keyup，當 webview 聚焦時補發與 Rust 鉤子
 * 相同的 Tauri 事件（ptt-down / ptt-up / ptt-lab-event）。
 *
 * 當外部視窗聚焦時，webview 不會收到鍵盤事件，所以不會重複觸發。
 * 兩個路徑互斥：LL 鉤子處理外部視窗，此模組處理 SayIt 自身視窗。
 */

import { emit } from '@tauri-apps/api/event'
import { IS_MAC } from '@/lib/platform'
import { getPTTPhysicalKeyStates } from './bridge'
import { getSetting } from './store'
import { getDefault } from './defaults'
import {
  canonicalizePTTShortcut,
  isPTTModifierCode,
  isValidPTTShortcut,
  parsePTTShortcut,
  PTT_CODE_TO_VK,
  SETTING_TO_VK,
  settingToCode,
} from '@/lib/shortcutKeys'

// PTT Lab 固定使用右 Ctrl
const PTT_LAB_CODE = 'ControlRight'
const PTT_LAB_VK = 0xa3

let pttCodes: string[] = []
let pttSetting = ''
let pttKeyDown = false
const pttPressed = new Set<string>()
const pttConsumedCodes = new Set<string>()
let pttStartCheckToken = 0
let pttStartCheckPending = false
let hfCode = ''
let hfSetting = ''
let hfKeyDown = false
let labKeyDown = false
let labEnabled = false
let started = false
// 設定頁正在錄製快捷鍵：此時按鍵要交給錄製框去繫結，不能再當成“使用者按了熱鍵”。
// 不掛起的話，按到已繫結的鍵（如免持的右 Alt）會從這裡 emit toggle-hands-free，
// 於是彈出懸浮窗開始錄音——而不是把這個鍵錄進設定裡。
let captureActive = false

function isModifierSetting(setting: string) {
  return ['AltLeft', 'AltRight', 'ControlLeft', 'ControlRight', 'ShiftLeft', 'ShiftRight'].includes(setting)
}

function pttModifierFlags() {
  return {
    altKey: pttCodes.some((code) => code.startsWith('Alt')),
    ctrlKey: pttCodes.some((code) => code.startsWith('Control')),
    shiftKey: pttCodes.some((code) => code.startsWith('Shift')),
    metaKey: pttCodes.some((code) => code.startsWith('Meta')),
  }
}

/**
 * 舊單鍵仍按原行為吞 down。普通組合僅在主鍵首次按下時全部修飾成員已經按住，
 * 才吞主鍵並記住配對 up；K→Ctrl 順序仍可開始 PTT，但 K 的 down/up 都放行。
 */
function shouldConsumePTTDown(code: string, wasPressed: boolean) {
  if (pttCodes.length === 1) return true
  if (isPTTModifierCode(code)) return false
  if (wasPressed) return pttConsumedCodes.has(code)
  return pttCodes
    .filter(isPTTModifierCode)
    .every((modifier) => pttPressed.has(modifier))
}

function reconcileStalePTTModifiers(event: KeyboardEvent) {
  for (const code of [...pttPressed]) {
    if (code === event.code || !isPTTModifierCode(code)) continue
    const physicallyDown = code.startsWith('Control')
      ? event.ctrlKey
      : code.startsWith('Alt')
        ? event.altKey
        : code.startsWith('Shift')
          ? event.shiftKey
          : event.metaKey
    if (!physicallyDown) {
      pttPressed.delete(code)
      pttConsumedCodes.delete(code)
    }
  }
}

function invalidatePTTStartCheck() {
  pttStartCheckToken += 1
  pttStartCheckPending = false
}

/**
 * 當完整組合由修飾鍵最後按下時（K→Ctrl 或純修飾組合），KeyboardEvent 無法證明
 * 之前成員仍真實按住，也無法區分左右同族修飾鍵；用 Rust/GetAsyncKeyState 做一次確認。
 */
async function confirmPhysicalPTTStart(triggerCode: string) {
  if (pttStartCheckPending) return
  pttStartCheckPending = true
  const token = ++pttStartCheckToken
  const codes = [...pttCodes]
  const setting = pttSetting

  try {
    const physicalStates = await getPTTPhysicalKeyStates(codes)
    if (token !== pttStartCheckToken) return

    codes.forEach((code, index) => {
      if (!physicalStates[index]) {
        pttPressed.delete(code)
        pttConsumedCodes.delete(code)
      }
    })

    const allStillDown = physicalStates.length === codes.length
      && physicalStates.every(Boolean)
      && codes.every((code) => pttPressed.has(code))
    if (!captureActive && !pttKeyDown && pttSetting === setting && allStillDown) {
      pttKeyDown = true
      emitPTT('down', triggerCode, 'physical_members_confirmed')
    }
  } catch (error) {
    console.warn('[webview-kb] failed to verify physical PTT state:', error)
  } finally {
    if (token === pttStartCheckToken) pttStartCheckPending = false
  }
}

function emitPTT(phase: 'down' | 'up', triggerCode: string, reason: string) {
  const vk = PTT_CODE_TO_VK[triggerCode] || 0
  console.log(`[webview-kb] ptt-${phase} (webview fallback)`, {
    code: triggerCode,
    pttSetting,
    reason,
  })
  void emit(`ptt-${phase}`, {
    source: 'webview_fallback',
    reason,
    vk,
    keycode: vk,
    pttSetting,
    timestamp: Date.now(),
    ...pttModifierFlags(),
  })
}

function releasePTT(reason: string, triggerCode = pttCodes[0] || '') {
  invalidatePTTStartCheck()
  if (pttKeyDown) {
    pttKeyDown = false
    emitPTT('up', triggerCode, reason)
  }
  pttPressed.clear()
  pttConsumedCodes.clear()
}

function handleKeyDown(event: KeyboardEvent) {
  if (captureActive) return

  if (pttCodes.includes(event.code)) {
    reconcileStalePTTModifiers(event)
    const wasPressed = pttPressed.has(event.code)
    const consumeDown = shouldConsumePTTDown(event.code, wasPressed)
    if (consumeDown) {
      if (pttCodes.length > 1) pttConsumedCodes.add(event.code)
      event.preventDefault()
    }
    pttPressed.add(event.code)
    if (!pttKeyDown && pttCodes.every((code) => pttPressed.has(code))) {
      if (pttCodes.length > 1 && isPTTModifierCode(event.code)) {
        void confirmPhysicalPTTStart(event.code)
      } else {
        invalidatePTTStartCheck()
        pttKeyDown = true
        emitPTT('down', event.code, 'all_members_down')
      }
    }
    return
  }

  // 免持鍵（HF）：首個 keydown 立即觸發；後續 repeat down 不重複切換。
  if (hfCode && event.code === hfCode && !pttCodes.includes(hfCode)) {
    if (!hfKeyDown) {
      hfKeyDown = true
      console.log('[webview-kb] toggle-hands-free (webview fallback)', {
        code: event.code,
        hfSetting,
      })
      void emit('toggle-hands-free', {
        source: 'webview_fallback',
        vk: SETTING_TO_VK[hfSetting] || 0,
      })
    }
    if (isModifierSetting(hfSetting)) event.preventDefault()
    return
  }

  // PTT Lab 鍵（右 Ctrl）
  if (labEnabled && event.code === PTT_LAB_CODE && !labKeyDown) {
    // 如果右 Ctrl 是 PTT 的成員，不重複處理 lab。
    if (pttCodes.includes(PTT_LAB_CODE)) return
    labKeyDown = true
    event.preventDefault()
    console.log('[webview-kb] ptt-lab-event down (webview fallback)')
    void emit('ptt-lab-event', {
      phase: 'down',
      vk: PTT_LAB_VK,
      timestamp: Date.now(),
    })
  }
}

function handleKeyUp(event: KeyboardEvent) {
  if (captureActive) return

  if (pttCodes.includes(event.code)) {
    invalidatePTTStartCheck()
    if (pttCodes.length === 1 || pttConsumedCodes.delete(event.code)) {
      event.preventDefault()
    }
    const wasPressed = pttPressed.delete(event.code)
    if (wasPressed && pttKeyDown) {
      pttKeyDown = false
      emitPTT('up', event.code, 'member_up')
    }
    return
  }

  // 免持鍵：keyup 只重新佈防，下一次物理按下才能再次切換。
  if (hfCode && event.code === hfCode && hfKeyDown && !pttCodes.includes(hfCode)) {
    hfKeyDown = false
    if (isModifierSetting(hfSetting)) event.preventDefault()
    return
  }

  // PTT Lab 鍵
  if (labEnabled && event.code === PTT_LAB_CODE && labKeyDown) {
    if (pttCodes.includes(PTT_LAB_CODE)) return
    labKeyDown = false
    event.preventDefault()
    console.log('[webview-kb] ptt-lab-event up (webview fallback)')
    void emit('ptt-lab-event', {
      phase: 'up',
      vk: PTT_LAB_VK,
      timestamp: Date.now(),
    })
  }
}

function handleWindowBlur() {
  // 焦點轉移後，keyup 可能送到別的視窗；立即釋放，避免前端 fallback 留下活動錄音。
  releasePTT('window_blur')
  hfKeyDown = false
  labKeyDown = false
}

/** 重新整理 PTT 設定（設定頁面改鍵後呼叫） */
export async function refreshPTTSetting() {
  try {
    // 不傳字面量兜底值：預設鍵只在 services/defaults.ts 裡定義一處，getSetting 會去讀它。
    const setting = await getSetting<string>('shortcutPTT')
    const loadedSetting = String(setting ?? '')
    if (!loadedSetting) {
      pttSetting = ''
      pttCodes = []
    } else {
      const canonical = canonicalizePTTShortcut(loadedSetting)
      // allowLegacyReservedKeys：老使用者存的可能是 Shift。那類繫結不再允許新設，
      // 但已經存在的必須照原樣監聽 —— 判成非法就會走下面的回落，等於使用者沒改設定
      // 卻換了說話鍵，而且會開始響應一個他沒綁過的鍵。
      pttSetting = isValidPTTShortcut(canonical, { allowLegacyReservedKeys: true })
        ? canonical
        : getDefault<string>('shortcutPTT', '')
      pttCodes = parsePTTShortcut(pttSetting)
      if (pttSetting !== canonical) {
        console.warn('[webview-kb] invalid PTT setting, falling back to the default:', loadedSetting)
      }
    }
  } catch (error) {
    pttSetting = getDefault<string>('shortcutPTT', '')
    pttCodes = parsePTTShortcut(pttSetting)
    console.warn('[webview-kb] failed to load PTT setting, using fallback:', error)
  }
  releasePTT('setting_refreshed')

  try {
    const hfSettingVal = await getSetting('shortcutHandsFree', 'AltRight')
    hfSetting = String(hfSettingVal ?? 'AltRight')
  } catch {
    hfSetting = 'AltRight'
  }
  hfCode = settingToCode(hfSetting)
  hfKeyDown = false

  console.log('[webview-kb] PTT setting refreshed:', pttSetting, '→ codes:', pttCodes)
  console.log('[webview-kb] HF setting refreshed:', hfSetting, '→ code:', hfCode)
}

/**
 * 掛起/恢復本回退監聽（設定頁開始/結束錄製快捷鍵時呼叫）。
 * 進出時都清掉“按住”狀態：錄製期間被忽略的 keyup 不會留下卡住的按下標記。
 */
export function setShortcutCaptureActive(active: boolean) {
  if (active) releasePTT('shortcut_capture')
  captureActive = active
  pttPressed.clear()
  hfKeyDown = false
  labKeyDown = false
}

/** PTT Lab 啟用/停用 */
export function setLabEnabled(enabled: boolean) {
  labEnabled = enabled
  if (!enabled) labKeyDown = false
}

/** 啟動 webview 鍵盤迴退監聽 */
export async function startWebviewKeyboardFallback() {
  if (started) return
  // macOS 的 CGEventTap 在 SayIt 自己聚焦時也能收到按鍵（不像 WebView2 會吞掉低階鉤子），
  // 再從 DOM 補發一遍就會重複觸發 ptt-down / toggle-hands-free。
  if (IS_MAC) return
  started = true

  await refreshPTTSetting()

  document.addEventListener('keydown', handleKeyDown, { capture: true })
  document.addEventListener('keyup', handleKeyUp, { capture: true })
  window.addEventListener('blur', handleWindowBlur)
  console.log('[webview-kb] started, pttSetting:', pttSetting, 'codes:', pttCodes)
}

/** 停止監聽 */
export function stopWebviewKeyboardFallback() {
  if (!started) return
  started = false
  releasePTT('fallback_stopped')
  hfKeyDown = false
  labKeyDown = false
  document.removeEventListener('keydown', handleKeyDown, { capture: true })
  document.removeEventListener('keyup', handleKeyUp, { capture: true })
  window.removeEventListener('blur', handleWindowBlur)
  console.log('[webview-kb] stopped')
}
