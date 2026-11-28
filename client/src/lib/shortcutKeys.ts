/**
 * 快捷鍵定義與 PTT 物理組合鍵工具。
 *
 * PTT 設定儲存 DOM `KeyboardEvent.code`，舊單鍵保持 `ShiftRight` 等原格式，
 * 組合鍵按固定順序儲存為 `ControlLeft+MetaLeft`、`ControlLeft+KeyK`。
 *
 * ⚠️ Rust 端 `client/src-tauri/src/keyboard/mod.rs` 的按鍵表無法直接引用 TS；
 * 新增或修改 PTT code 時必須同步 Rust 對映。
 */

import { t, type TranslationKey } from '@/i18n'
import { IS_MAC } from '@/lib/platform'

export interface SingleKeyDef {
  /** 存入設定、也等於 DOM KeyboardEvent.code */
  setting: string
  /** Windows 虛擬鍵碼（供 webview 回退補發事件用） */
  vk: number
  /** 中文顯示名 */
  label: string
}

// i18n-allow-start: Windows VK 源標籤；展示層由 getSingleKeyDisplay 翻譯覆蓋
const ALL_SINGLE_KEYS: SingleKeyDef[] = [
  // 修飾鍵（左右分開）
  { setting: 'AltLeft', vk: 0xa4, label: '左 Alt' },
  { setting: 'AltRight', vk: 0xa5, label: '右 Alt' },
  { setting: 'ControlLeft', vk: 0xa2, label: '左 Ctrl' },
  { setting: 'ControlRight', vk: 0xa3, label: '右 Ctrl' },
  { setting: 'ShiftLeft', vk: 0xa0, label: '左 Shift' },
  { setting: 'ShiftRight', vk: 0xa1, label: '右 Shift' },
  // 常見低衝突鍵
  { setting: 'CapsLock', vk: 0x14, label: 'Caps Lock' },
  { setting: 'Space', vk: 0x20, label: '空格' },
  { setting: 'ContextMenu', vk: 0x5d, label: '選單鍵' },
  { setting: 'Pause', vk: 0x13, label: 'Pause' },
  { setting: 'ScrollLock', vk: 0x91, label: 'ScrollLock' },
  { setting: 'Insert', vk: 0x2d, label: 'Insert' },
  // 滑鼠側鍵（原始 XBUTTON，由 Rust 低階滑鼠鉤子處理）
  { setting: 'XButton1', vk: 0x05, label: '滑鼠側鍵1（後退）' },
  { setting: 'XButton2', vk: 0x06, label: '滑鼠側鍵2（前進）' },
  // 滑鼠中鍵（VK_MBUTTON，由 Rust 低階滑鼠鉤子處理）
  { setting: 'MButton', vk: 0x04, label: '滑鼠中鍵' },
  // 瀏覽器後退/前進鍵（羅技等改鍵滑鼠常把側鍵對映成這個，由鍵盤鉤子處理）
  { setting: 'BrowserBack', vk: 0xa6, label: '滑鼠側鍵（後退鍵）' },
  { setting: 'BrowserForward', vk: 0xa7, label: '滑鼠側鍵（前進鍵）' },
  // 功能鍵 F1–F24（VK 0x70–0x87 連續）。
  //
  // F13–F24 在 Windows 裡是正式的虛擬鍵、鍵盤佈局裡也有真實 scan code，但沒有任何
  // 量產鍵盤把它做成物理鍵 —— 所以它們幾乎不可能和別的程式的快捷鍵撞，正好適合
  // 可程式設計鍵盤、自制 HID 語音裝置這類「專用觸發鍵」的場景。
  // Rust 側的幻影過濾對這一段開了例外，見 keyboard/mod.rs 的 is_injection_exempt_vk。
  ...Array.from({ length: 24 }, (_, index) => ({
    setting: `F${index + 1}`,
    vk: 0x70 + index,
    label: `F${index + 1}`,
  })),
]

/**
 * Mac 鍵盤上不存在、或在 mac 上不能當熱鍵的單鍵。
 *
 * - ContextMenu / Pause / ScrollLock / BrowserBack / BrowserForward：Mac 鍵盤沒有
 *   （PC 鍵盤接到 Mac 上時 PrintScreen/ScrollLock/Pause 會被系統報成 F13–F15）。
 * - F21–F24：macOS 沒有對應的虛擬鍵碼。
 * - CapsLock：mac 上只報「切換」不報「鬆開」，中文使用者還常用它切換輸入法。
 */
const MAC_UNAVAILABLE_SINGLE_KEYS = new Set([
  'ContextMenu', 'Pause', 'ScrollLock', 'BrowserBack', 'BrowserForward', 'CapsLock',
  'F21', 'F22', 'F23', 'F24',
])

/**
 * 只在 mac 上可以單獨當熱鍵的鍵（Rust 側見 keyboard/mod.rs 的 PLATFORM_EXTRA_SINGLE_KEYS）：
 * 單獨的 ⌘（右 ⌘ 是 mac 擴音的預設鍵；Windows 上單按 Win 會彈開始選單，所以不允許）、Fn / 🌐。
 */
const MAC_EXTRA_SINGLE_KEYS: SingleKeyDef[] = [
  { setting: 'MetaLeft', vk: 0x5b, label: 'Left ⌘' },
  { setting: 'MetaRight', vk: 0x5c, label: 'Right ⌘' },
  { setting: 'Fn', vk: 0xff, label: 'Fn' },
]

export const SINGLE_KEYS: SingleKeyDef[] = IS_MAC
  ? [...ALL_SINGLE_KEYS.filter((key) => !MAC_UNAVAILABLE_SINGLE_KEYS.has(key.setting)), ...MAC_EXTRA_SINGLE_KEYS]
  : ALL_SINGLE_KEYS
// i18n-allow-end

/** setting → 虛擬鍵碼（舊單鍵與擴音回退使用） */
export const SETTING_TO_VK: Record<string, number> = Object.fromEntries(
  SINGLE_KEYS.map((key) => [key.setting, key.vk]),
)

const SINGLE_KEY_DISPLAY: Record<string, string> = Object.fromEntries(
  SINGLE_KEYS.map((key) => [key.setting, key.label]),
)

/** PTT 修飾鍵固定排序；左右位置會保留。 */
export const PTT_MODIFIER_CODES = [
  'ControlLeft',
  'ControlRight',
  'MetaLeft',
  'MetaRight',
  'AltLeft',
  'AltRight',
  'ShiftLeft',
  'ShiftRight',
] as const

const PTT_MODIFIER_SET = new Set<string>(PTT_MODIFIER_CODES)
const PTT_MOUSE_CODES = new Set(['XButton1', 'XButton2', 'MButton'])

/**
 * 不允許出現在「按住說話」裡的按鍵。
 *
 * Shift 與 Windows 的輔助功能快捷方式直接衝突，而衝突的觸發條件恰好就是 PTT 的
 * 使用方式：
 *   · 長按右 Shift 約 8 秒 → 篩選鍵。它一旦彈出，我們收不到那次「鬆開」，
 *     錄音不會隨手指抬起而結束（這是之前只給警告、使用者仍然踩到的那個問題）。
 *   · 連按 Shift 五次 → 粘滯鍵。短句口述本來就是一串快速的按下鬆開。
 * PTT 必須被按住數秒、還經常被連續短按，兩條正好全中。
 *
 * 所以 Shift 不做成"可以選但給個警告"——警告只能讓使用者在出問題之後回來讀一遍，
 * 擋不住任何人。左右都禁：單鍵和組合成員都算，`Ctrl+右Shift` 一樣會觸發篩選鍵。
 *
 * **只約束 PTT。** 擴音、預設切換那類"按一下"的快捷鍵（走 accelerator 那條校驗）
 * 照舊可以用 Shift —— 它們不長按也不連按，兩個觸發條件都碰不到。
 */
const PTT_FORBIDDEN_CODES = new Set(['ShiftLeft', 'ShiftRight'])

/** 這個按鍵能不能用於「按住說話」。 */
export function isPTTForbiddenCode(code: string): boolean {
  return PTT_FORBIDDEN_CODES.has(code)
}

// i18n-allow-start: Windows VK 源標籤；展示層由 getSingleKeyDisplay 翻譯覆蓋
const PTT_EXTRA_KEY_DEFS: SingleKeyDef[] = [
  { setting: 'MetaLeft', vk: 0x5b, label: '左 Win' },
  { setting: 'MetaRight', vk: 0x5c, label: '右 Win' },
  ...Array.from({ length: 26 }, (_, index) => ({
    setting: `Key${String.fromCharCode(65 + index)}`,
    vk: 0x41 + index,
    label: String.fromCharCode(65 + index),
  })),
  ...Array.from({ length: 10 }, (_, index) => ({
    setting: `Digit${index}`,
    vk: 0x30 + index,
    label: String(index),
  })),
  { setting: 'Escape', vk: 0x1b, label: 'Esc' },
  { setting: 'Tab', vk: 0x09, label: 'Tab' },
  { setting: 'Enter', vk: 0x0d, label: 'Enter' },
  { setting: 'Backspace', vk: 0x08, label: 'Backspace' },
  { setting: 'Delete', vk: 0x2e, label: 'Delete' },
  { setting: 'ArrowUp', vk: 0x26, label: '↑' },
  { setting: 'ArrowDown', vk: 0x28, label: '↓' },
  { setting: 'ArrowLeft', vk: 0x25, label: '←' },
  { setting: 'ArrowRight', vk: 0x27, label: '→' },
  { setting: 'Home', vk: 0x24, label: 'Home' },
  { setting: 'End', vk: 0x23, label: 'End' },
  { setting: 'PageUp', vk: 0x21, label: 'Page Up' },
  { setting: 'PageDown', vk: 0x22, label: 'Page Down' },
]
// i18n-allow-end

/** PTT 成員 code → Windows 虛擬鍵碼。 */
export const PTT_CODE_TO_VK: Record<string, number> = {
  ...SETTING_TO_VK,
  ...Object.fromEntries(PTT_EXTRA_KEY_DEFS.map((key) => [key.setting, key.vk])),
}

const PTT_CODE_DISPLAY: Record<string, string> = {
  ...SINGLE_KEY_DISPLAY,
  ...Object.fromEntries(PTT_EXTRA_KEY_DEFS.map((key) => [key.setting, key.label])),
}

/** 是否為受支援的舊單鍵設定。 */
export function isSingleKeySetting(setting: string): boolean {
  return setting in SETTING_TO_VK
}

/** 若 DOM code 對應一個受支援的舊單鍵，返回其 setting（= code）。 */
export function resolveSingleKeyShortcut(code: string): string | undefined {
  return isSingleKeySetting(code) ? code : undefined
}

/** setting → DOM code（恆等，未知返回空串）。 */
export function settingToCode(setting: string): string {
  return isSingleKeySetting(setting) ? setting : ''
}

/**
 * 需要翻譯的鍵名。
 *
 * 只列**含自然語言**的那些：`左 Alt` 在英文裡是 `Left Alt`，`空格` 是 `Space`。
 * `F1`、`Tab`、`Esc`、`Caps Lock`、字母數字、方向鍵這些本來就是語言中立的，
 * 繼續用上面表裡的 `label`，不進 locale 檔案 —— 翻譯一份和原文一樣的東西，
 * 只會多一處將來會不同步的地方。
 *
 * ⚠️ 這裡刻意**不改** `SINGLE_KEYS` 的 `label` 欄位：那張表還同時供 `vk` 對映用，
 * 動它的形狀會牽連 webview 回退補發按鍵的邏輯。這裡只加一層展示期的覆蓋。
 */
const TRANSLATED_KEY_NAMES: Record<string, TranslationKey> = {
  AltLeft: 'keyName.AltLeft',
  AltRight: 'keyName.AltRight',
  ControlLeft: 'keyName.ControlLeft',
  ControlRight: 'keyName.ControlRight',
  ShiftLeft: 'keyName.ShiftLeft',
  ShiftRight: 'keyName.ShiftRight',
  MetaLeft: 'keyName.MetaLeft',
  MetaRight: 'keyName.MetaRight',
  Space: 'keyName.Space',
  ContextMenu: 'keyName.ContextMenu',
  XButton1: 'keyName.XButton1',
  XButton2: 'keyName.XButton2',
  MButton: 'keyName.MButton',
  BrowserBack: 'keyName.BrowserBack',
  BrowserForward: 'keyName.BrowserForward',
}

/** mac 上修飾鍵的顯示名（⌥ Option、⌘ Command…），覆蓋上面的 Windows 名稱。 */
const MAC_TRANSLATED_KEY_NAMES: Record<string, TranslationKey> = {
  AltLeft: 'keyName.mac.AltLeft',
  AltRight: 'keyName.mac.AltRight',
  ControlLeft: 'keyName.mac.ControlLeft',
  ControlRight: 'keyName.mac.ControlRight',
  ShiftLeft: 'keyName.mac.ShiftLeft',
  ShiftRight: 'keyName.mac.ShiftRight',
  MetaLeft: 'keyName.mac.MetaLeft',
  MetaRight: 'keyName.mac.MetaRight',
  Fn: 'keyName.mac.Fn',
}

function translatedKeyName(code: string): TranslationKey | undefined {
  return (IS_MAC ? MAC_TRANSLATED_KEY_NAMES[code] : undefined) ?? TRANSLATED_KEY_NAMES[code]
}

/** 單鍵顯示名（未知原樣返回）。 */
export function getSingleKeyDisplay(value: string): string {
  const key = translatedKeyName(value)
  if (key) return t(key)
  return SINGLE_KEY_DISPLAY[value] || value
}

/** Tauri accelerator 的顯示名，用於逐個渲染鍵帽（mac 用 ⌘⌥⌃⇧ 符號）。 */
export function displayAccelerator(accelerator: string): string[] {
  const displayNames: Record<string, string> = IS_MAC
    ? {
      CommandOrControl: '⌘',
      Command: '⌘',
      Cmd: '⌘',
      Super: '⌘',
      Meta: '⌘',
      Control: '⌃',
      Ctrl: '⌃',
      Alt: '⌥',
      Option: '⌥',
      Shift: '⇧',
      Space: 'Space',
      Return: '↩',
      // mac 按鍵位錄製的主鍵名（見 macAcceleratorMainKey），顯示成 mac 選單裡的寫法
      Enter: '↩',
      Escape: '⎋',
      Tab: '⇥',
      Backspace: '⌫',
      Delete: '⌦',
      Up: '↑',
      Down: '↓',
      Left: '←',
      Right: '→',
      Comma: ',',
      Period: '.',
      Slash: '/',
      Semicolon: ';',
      Quote: "'",
      BracketLeft: '[',
      BracketRight: ']',
      Backslash: '\\',
      Backquote: '`',
      Minus: '-',
      Equal: '=',
    }
    : {
      CommandOrControl: 'Ctrl',
      Control: 'Ctrl',
      Ctrl: 'Ctrl',
      Alt: 'Alt',
      Shift: 'Shift',
      Space: 'Space',
      Return: 'Enter',
    }
  return accelerator.split('+')
    .map((part) => part.trim())
    .filter(Boolean)
    .map((part) => displayNames[part] || part)
}

/** 單鍵或 accelerator 的統一顯示標籤。 */
export function displayShortcut(shortcut: string): string[] {
  return isSingleKeySetting(shortcut)
    ? [getSingleKeyDisplay(shortcut)]
    : displayAccelerator(shortcut)
}

/**
 * 按鍵事件 → Tauri accelerator。只有「修飾鍵 + 主鍵」才成型，單按修飾鍵返回 null。
 *
 * 擴音與預設切換存的就是這種格式（見 defaults.ts 的 shortcutHandsFree）。
 */
export function eventToAccelerator(event: KeyboardEvent): string | null {
  const parts: string[] = []
  if (IS_MAC) {
    // mac 上 ⌘ 與 ⌃ 是兩個鍵：⌘ 存成 CommandOrControl（在 mac 上就是 ⌘），⌃ 單獨存 Control
    if (event.metaKey) parts.push('CommandOrControl')
    if (event.ctrlKey) parts.push('Control')
  } else if (event.ctrlKey || event.metaKey) {
    parts.push('CommandOrControl')
  }
  if (event.altKey) parts.push('Alt')
  if (event.shiftKey) parts.push('Shift')

  const key = event.key
  if (['Control', 'Alt', 'Shift', 'Meta'].includes(key)) return null

  // mac：主鍵按鍵位取名（⌥ / ⇧ 會改掉 event.key，見 macAcceleratorMainKey）
  if (IS_MAC) {
    const mainKey = macAcceleratorMainKey(event.code)
    if (!mainKey) return null
    parts.push(mainKey)
    return parts.length >= 2 ? parts.join('+') : null
  }

  const keyMap: Record<string, string> = {
    ' ': 'Space',
    ArrowUp: 'Up',
    ArrowDown: 'Down',
    ArrowLeft: 'Left',
    ArrowRight: 'Right',
    Escape: 'Escape',
    Enter: 'Return',
    Backspace: 'Backspace',
    Delete: 'Delete',
    Tab: 'Tab',
  }

  const mapped = keyMap[key] || (key.length === 1 ? key.toUpperCase() : key)
  parts.push(mapped)
  return parts.length >= 2 ? parts.join('+') : null
}

/**
 * 一次 keydown 該被錄成什麼：單鍵 DOM code、accelerator，或 null（還不成型）。
 *
 * **這個判斷必須只有一份。** 嚮導裡曾經自己寫過一份「只認單鍵白名單」的版本，
 * 於是按 Ctrl+D 時 Ctrl 的 keydown 先到、`ControlLeft` 恰好在白名單裡 ——
 * 擴音鍵被靜默存成「左 Ctrl」，D 根本沒輪到，而使用者看到的是"組合鍵不支援"。
 *
 * 判定順序是**先組合、後單鍵**，不能反過來：`Space`、`Insert`、`F1`–`F24` 同時
 * 出現在單鍵白名單裡，先判單鍵會把 `Ctrl+Space`、`Ctrl+F1` 也吃成單鍵。
 * 反過來是安全的 —— 純修飾鍵（右 Alt）在 eventToAccelerator 裡返回 null，
 * 自然落到單鍵分支。
 *
 * 呼叫方在 keydown 裡持續更新候選、在 keyup 時提交最後那個候選，**null 表示
 * 「這次按鍵還沒湊成東西」，要保留上一個候選**，不能覆蓋成空 —— 否則先按 Ctrl+D
 * 再鬆手的過程中，任何一次不成型的中間事件都會把已經錄到的組合鍵抹掉。
 *
 * comboOnly 用於「必須是組合鍵」的設定（預設切換）：單鍵一律不成型。
 */
export function keyEventToShortcutCandidate(
  event: KeyboardEvent,
  options: { comboOnly?: boolean } = {},
): string | null {
  const accelerator = eventToAccelerator(event)
  if (accelerator) return accelerator
  if (options.comboOnly) return null
  return resolveSingleKeyShortcut(event.code) ?? null
}

/** 拆分 PTT 設定；不在這裡靜默丟棄未知成員，交給校驗器給出明確錯誤。 */
export function parsePTTShortcut(setting: string): string[] {
  if (!setting.trim()) return []
  return setting.split('+').map((part) => part.trim()).filter(Boolean)
}

/** 將 PTT 成員按 Ctrl、Win、Alt、Shift、普通鍵順序規範化。 */
export function canonicalizePTTShortcut(settingOrCodes: string | Iterable<string>): string {
  const codes = typeof settingOrCodes === 'string'
    ? parsePTTShortcut(settingOrCodes)
    : Array.from(settingOrCodes)
  const uniqueCodes = [...new Set(codes)]
  const modifierOrder = new Map<string, number>(
    PTT_MODIFIER_CODES.map((code, index) => [code, index]),
  )
  return uniqueCodes.sort((left, right) => {
    const leftOrder = modifierOrder.get(left) ?? PTT_MODIFIER_CODES.length
    const rightOrder = modifierOrder.get(right) ?? PTT_MODIFIER_CODES.length
    return leftOrder - rightOrder || left.localeCompare(right)
  }).join('+')
}

/** PTT 設定的按鍵標籤，用於逐個渲染鍵帽。 */
export function displayPTTShortcut(setting: string): string[] {
  return parsePTTShortcut(canonicalizePTTShortcut(setting)).map((code) => {
    const key = translatedKeyName(code)
    return key ? t(key) : PTT_CODE_DISPLAY[code] || code
  })
}

export function isPTTModifierCode(code: string): boolean {
  return PTT_MODIFIER_SET.has(code)
}

export function pttShortcutHasModifier(setting: string): boolean {
  return parsePTTShortcut(setting).some(isPTTModifierCode)
}

/**
 * 已儲存的按住說話配置需要提醒使用者的地方。
 *
 * 目前只有一種：老使用者在 Shift 還能選的時候綁上了它。
 *
 * 為什麼不在升級時自動改掉：那是在使用者不知情的情況下換掉他的說話鍵，比留著更糟。
 * 也不在這裡直接停掉它——那等於升級後按住說話突然沒反應，而使用者未必會想到來設定頁。
 * 所以舊繫結繼續生效，只是在設定裡常駐一行提示讓他換。新繫結由
 * getPTTShortcutValidationError 硬攔。
 */
export function getPTTShortcutWarning(setting: string): string | null {
  return parsePTTShortcut(canonicalizePTTShortcut(setting)).some(isPTTForbiddenCode)
    // mac 上只可能來自匯入的 Windows 配置
    ? t(IS_MAC ? 'shortcut.warning.shiftNoLongerSupportedMac' : 'shortcut.warning.shiftNoLongerSupported')
    : null
}

function normalizedAcceleratorParts(accelerator: string): string[] {
  const aliases: Record<string, string> = {
    Ctrl: 'Control',
    CommandOrControl: 'Control',
    Super: 'Meta',
    Win: 'Meta',
    Windows: 'Meta',
    Return: 'Enter',
    Up: 'ArrowUp',
    Down: 'ArrowDown',
    Left: 'ArrowLeft',
    Right: 'ArrowRight',
  }
  return accelerator.split('+')
    .map((part) => part.trim())
    .filter(Boolean)
    .map((part) => aliases[part] || part)
}

/**
 * 通用 accelerator 的 Windows 保留組合校驗。
 *
 * PTT 使用物理 code，有自己的完整校驗；擴音與預設切換使用 Tauri accelerator，
 * 也必須在試註冊前擋住系統安全介面、視窗切換等無法可靠覆蓋的組合。
 */
export function getAcceleratorShortcutValidationError(accelerator: string): string | null {
  // mac：單鍵和滑鼠鍵由我們自己的事件監聽處理，不走系統快捷鍵註冊，下面那些組合鍵規則管不到它們。
  // 以前漏了這一句，單獨一個右 ⌥、F5、滑鼠側鍵也被報「需要帶上 ⌘ 或 ⌃」。
  if (IS_MAC) return isSingleKeySetting(accelerator) ? null : getMacAcceleratorValidationError(accelerator)
  const parts = normalizedAcceleratorParts(accelerator)
  const has = (key: string) => parts.includes(key)
  const mainKeys = parts.filter((part) => !['Control', 'Meta', 'Alt', 'Shift'].includes(part))
  const mainKey = mainKeys[0]

  if (has('Control') && has('Alt') && mainKey === 'Delete') {
    return t('shortcut.error.reservedCtrlAltDel')
  }
  if (has('Meta') && mainKey) {
    return t('shortcut.error.reservedWindows')
  }
  if (has('Alt') && ['F4', 'Tab', 'Escape', 'Space'].includes(mainKey)) {
    return t('shortcut.error.reservedAlt')
  }
  if (has('Control') && mainKey === 'Escape') {
    return t('shortcut.error.reservedCtrlEsc')
  }
  return null
}

/**
 * macOS 的 accelerator 校驗（擴音組合、AI 開關、潤色模式切換）。
 *
 * - 帶 ⌘ 或 ⌃ 的組合註冊成系統快捷鍵；修飾鍵只有 ⌥ / ⇧ 的組合（macOS 15 起系統快捷鍵不觸發）
 *   由 Rust 的 event tap 接管（keyboard/macos.rs 的 TAP_COMBOS），兩類都能用。
 * - ⌘Space（聚焦搜尋）、⌃Space（切換輸入法）、⌘Tab、⌘Q、⌘⇧3/4/5（截圖）被系統或所有程式佔用。
 * - 一個修飾鍵 + 會打出字元或有編輯含義的鍵，是各程式自己的快捷鍵或用來輸入特殊字元（⌥E、⌥2）。
 */
function getMacAcceleratorValidationError(accelerator: string): string | null {
  const aliases: Record<string, string> = {
    CommandOrControl: 'Meta',
    Command: 'Meta',
    Cmd: 'Meta',
    Super: 'Meta',
    Ctrl: 'Control',
    Option: 'Alt',
    Return: 'Enter',
  }
  const parts = accelerator.split('+')
    .map((part) => part.trim())
    .filter(Boolean)
    .map((part) => aliases[part] || part)
  const has = (key: string) => parts.includes(key)
  const mainKey = parts.find((part) => !['Control', 'Meta', 'Alt', 'Shift'].includes(part))
  if ((has('Meta') || has('Control')) && mainKey === 'Space' && !has('Alt') && !has('Shift')) {
    return t('shortcut.error.reservedMac')
  }
  if (has('Meta') && (mainKey === 'Tab' || mainKey === 'Q')) {
    return t('shortcut.error.reservedMac')
  }
  if (has('Meta') && has('Shift') && ['3', '4', '5'].includes(mainKey ?? '')) {
    return t('shortcut.error.reservedMac')
  }
  // ⌥⌘Esc：強制退出
  if (has('Meta') && has('Alt') && mainKey === 'Escape') {
    return t('shortcut.error.reservedMac')
  }
  // 一個修飾鍵 + 字母 / 數字 / 符號 / 空格 / 方向鍵（⌘W 關視窗、⌘C 複製、⌃A 行首、⌥E 打重音符號、
  // ⇧Space 打空格……）是各個程式和系統自己的快捷鍵，或用來輸入字元。註冊成全域性快捷鍵後，
  // 這些鍵在所有程式裡都會失靈。
  const modifierCount = ['Meta', 'Control', 'Alt', 'Shift'].filter(has).length
  if (modifierCount === 1 && mainKey && isMacCharacterOrEditingKey(mainKey)) {
    return t('shortcut.error.macSingleModifierReserved')
  }
  return null
}

/** 和一個修飾鍵組合時都有系統或文字編輯含義的非字元鍵（accelerator 裡的寫法） */
const MAC_EDITING_KEYS = new Set([
  'Up', 'Down', 'Left', 'Right', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight',
  'Return', 'Enter', 'Escape', 'Tab', 'Backspace', 'Delete', 'Home', 'End', 'PageUp', 'PageDown',
])

/** 會打出字元的鍵裡，accelerator 不用單個字元而用名字寫的那些（mac 按鍵位錄製，見 macAcceleratorMainKey） */
const MAC_NAMED_CHARACTER_KEYS = new Set([
  'Space', 'Comma', 'Period', 'Slash', 'Semicolon', 'Quote', 'BracketLeft', 'BracketRight',
  'Backslash', 'Backquote', 'Minus', 'Equal',
])

function isMacCharacterOrEditingKey(mainKey: string): boolean {
  return mainKey.length === 1
    || MAC_NAMED_CHARACTER_KEYS.has(mainKey)
    || MAC_EDITING_KEYS.has(mainKey)
    || /^Numpad/.test(mainKey)
}

/**
 * mac 錄組合鍵時的主鍵：按物理鍵位（event.code）取名，不看 event.key。
 *
 * ⌥ 會改變 event.key（⌥⇧D 得到「Î」，⌥1 得到「¡」），⇧ 會把數字變成符號（⇧1 → 「!」），
 * 非美式鍵盤上字母的位置也不同；這些都會錄成 Rust 端（global-hotkey）解析不了的組合。
 * 系統快捷鍵和 event tap 本來就按鍵位匹配，所以這裡也按鍵位取名，寫法用 global-hotkey 認得的。
 * 返回 null 表示這個鍵不能當組合鍵的主鍵。
 */
function macAcceleratorMainKey(code: string): string | null {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3)
  if (/^Digit[0-9]$/.test(code)) return code.slice(5)
  if (/^F([1-9]|1[0-9]|20)$/.test(code)) return code
  if (/^Numpad([0-9]|Add|Subtract|Multiply|Divide|Decimal)$/.test(code)) return code
  const names: Record<string, string> = {
    ArrowUp: 'Up',
    ArrowDown: 'Down',
    ArrowLeft: 'Left',
    ArrowRight: 'Right',
  }
  if (names[code]) return names[code]
  return MAC_NAMED_CHARACTER_KEYS.has(code) || [
    'Enter', 'Escape', 'Tab', 'Backspace', 'Delete', 'Home', 'End', 'PageUp', 'PageDown',
  ].includes(code)
    ? code
    : null
}

export interface PTTValidationOptions {
  /**
   * 是否放行「歷史上曾經能選、現在已停用」的按鍵（目前只有 Shift）。
   *
   * 讀取**已儲存的配置**時必須傳 true。否則老使用者的 Shift 繫結會被判成非法，
   * 而幾個呼叫點碰到非法值的做法是回落到預設鍵 —— 使用者一個設定都沒改，
   * 說話鍵卻自己變了，這比讓他繼續用 Shift 更糟。
   *
   * 校驗**使用者新輸入**時保持 false（預設），這樣 Shift 綁不上去。
   */
  allowLegacyReservedKeys?: boolean
}

/** 返回本地化的錯誤提示；null 表示可儲存。 */
export function getPTTShortcutValidationError(
  setting: string,
  options: PTTValidationOptions = {},
): string | null {
  const rawCodes = setting.split('+').map((part) => part.trim())
  const codes = rawCodes.filter(Boolean)
  if (codes.length === 0) return t('shortcut.error.empty')
  if (rawCodes.length !== codes.length || new Set(codes).size !== codes.length) {
    return t('shortcut.error.invalidFormat')
  }

  const unsupported = codes.find((code) => !(code in PTT_CODE_TO_VK))
  if (unsupported) return t('shortcut.error.unsupportedKey', { key: unsupported })

  // 排在單鍵/組合的分支之前：Shift 無論單獨用還是當組合成員都不行
  if (!options.allowLegacyReservedKeys && codes.some(isPTTForbiddenCode)) {
    return t(IS_MAC ? 'shortcut.error.shiftReservedMac' : 'shortcut.error.shiftReserved')
  }

  if (codes.length === 1) {
    const code = codes[0]
    // mac 上單獨的 ⌘ 沒有系統動作，可以當按住說話鍵（Windows 上單按 Win 會彈開始選單）
    if (!IS_MAC && (code === 'MetaLeft' || code === 'MetaRight')) {
      return t('shortcut.error.metaAlone')
    }
    if (!isSingleKeySetting(code)) {
      return t(IS_MAC ? 'shortcut.error.plainKeyAloneMac' : 'shortcut.error.plainKeyAlone')
    }
    return null
  }

  if (codes.some((code) => PTT_MOUSE_CODES.has(code))) {
    return t('shortcut.error.mouseCombo')
  }

  const modifiers = codes.filter(isPTTModifierCode)
  const mainKeys = codes.filter((code) => !isPTTModifierCode(code))
  const modifierFamilies = modifiers.map((code) => code.replace(/(?:Left|Right)$/, ''))
  if (new Set(modifierFamilies).size !== modifierFamilies.length) {
    return t('shortcut.error.sameModifierFamily')
  }
  if (mainKeys.length > 1) return t('shortcut.error.tooManyMainKeys')
  if (mainKeys.length === 1 && modifiers.length === 0) {
    return t(IS_MAC ? 'shortcut.error.needModifierMac' : 'shortcut.error.needModifier')
  }
  if (mainKeys.length === 0 && modifiers.length < 2) {
    return t('shortcut.error.needTwoModifiers')
  }

  const hasFamily = (prefix: string) => modifiers.some((code) => code.startsWith(prefix))
  const mainKey = mainKeys[0]
  if (IS_MAC) {
    // 按住說話走 event tap，不受 Carbon 快捷鍵的 ⌥ 限制；只擋系統佔用的組合
    if ((hasFamily('Meta') || hasFamily('Control')) && mainKey === 'Space') {
      return t('shortcut.error.reservedMac')
    }
    if (hasFamily('Meta') && (mainKey === 'Tab' || mainKey === 'KeyQ')) {
      return t('shortcut.error.reservedMac')
    }
    return null
  }
  if (hasFamily('Control') && hasFamily('Alt') && mainKey === 'Delete') {
    return t('shortcut.error.reservedCtrlAltDel')
  }
  // 帶主鍵的 Win 組合由 Windows Shell 保留；Ctrl + Win 這類純修飾鍵組合仍可用。
  if (hasFamily('Meta') && mainKey) {
    return t('shortcut.error.reservedWindows')
  }
  if (hasFamily('Alt') && ['F4', 'Tab', 'Escape', 'Space'].includes(mainKey)) {
    return t('shortcut.error.reservedAlt')
  }
  if (hasFamily('Control') && mainKey === 'Escape') {
    return t('shortcut.error.reservedCtrlEsc')
  }

  return null
}

export function isValidPTTShortcut(
  setting: string,
  options: PTTValidationOptions = {},
): boolean {
  return getPTTShortcutValidationError(setting, options) === null
}

function pttMainCodeToAccelerator(code: string): string {
  if (code.startsWith('Key')) return code.slice(3)
  if (code.startsWith('Digit')) return code.slice(5)
  const aliases: Record<string, string> = {
    Enter: 'Return',
    ArrowUp: 'Up',
    ArrowDown: 'Down',
    ArrowLeft: 'Left',
    ArrowRight: 'Right',
  }
  return aliases[code] || code
}

/**
 * 將可由 Tauri accelerator 表示的 PTT 組合轉換為 accelerator。
 * 純修飾組合以及同時包含 Ctrl 和 Win 的組合沒有無損表示，返回 undefined。
 */
export function pttShortcutToAccelerator(setting: string): string | undefined {
  if (!isValidPTTShortcut(setting)) return undefined
  const codes = parsePTTShortcut(canonicalizePTTShortcut(setting))
  const mainKeys = codes.filter((code) => !isPTTModifierCode(code))
  if (mainKeys.length !== 1) return undefined

  const hasControl = codes.some((code) => code.startsWith('Control'))
  const hasMeta = codes.some((code) => code.startsWith('Meta'))
  if (hasControl && hasMeta) return undefined

  const parts: string[] = []
  // 現有擴音錄製將 Windows 的 Ctrl/Meta 都儲存為 CommandOrControl。
  if (hasControl || hasMeta) parts.push('CommandOrControl')
  if (codes.some((code) => code.startsWith('Alt'))) parts.push('Alt')
  if (codes.some((code) => code.startsWith('Shift'))) parts.push('Shift')
  parts.push(pttMainCodeToAccelerator(mainKeys[0]))
  return parts.join('+')
}

function normalizeAccelerator(accelerator: string): string {
  const order: Record<string, number> = { CommandOrControl: 0, Alt: 1, Shift: 2 }
  const aliases: Record<string, string> = {
    Ctrl: 'CommandOrControl',
    Control: 'CommandOrControl',
    Command: 'CommandOrControl',
    Meta: 'CommandOrControl',
    Enter: 'Return',
    ArrowUp: 'Up',
    ArrowDown: 'Down',
    ArrowLeft: 'Left',
    ArrowRight: 'Right',
  }
  return accelerator.split('+')
    .map((part) => aliases[part] || part)
    .sort((left, right) => (order[left] ?? 3) - (order[right] ?? 3) || left.localeCompare(right))
    .join('+')
}

/** 比較 PTT 物理組合與擴音/預設 accelerator 是否會觸發同一組合。 */
export function pttShortcutConflictsWithAccelerator(
  pttSetting: string,
  otherShortcut: string,
): boolean {
  if (!pttSetting || !otherShortcut) return false
  const pttCodes = parsePTTShortcut(canonicalizePTTShortcut(pttSetting))
  // 擴音單鍵若同時是 PTT 組合成員，會被原生 hook 的“PTT 優先”規則完全遮蔽。
  if (isSingleKeySetting(otherShortcut) && pttCodes.includes(otherShortcut)) return true
  if (canonicalizePTTShortcut(pttSetting) === canonicalizePTTShortcut(otherShortcut)) return true
  const accelerator = pttShortcutToAccelerator(pttSetting)
  return accelerator !== undefined
    && normalizeAccelerator(accelerator) === normalizeAccelerator(otherShortcut)
}
