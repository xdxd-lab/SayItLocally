import { describe, expect, it, vi } from 'vitest'

// 這個檔案按 macOS 跑。IS_MAC 在模組載入時就決定了鍵表和校驗口徑，所以要在匯入前替換掉。
vi.mock('@/lib/platform', () => ({ IS_MAC: true }))

import {
  displayAccelerator,
  getAcceleratorShortcutValidationError,
  getPTTShortcutValidationError,
  keyEventToShortcutCandidate,
} from '../shortcutKeys'

/** 只造出 keyEventToShortcutCandidate 會讀的那幾個欄位 */
function keyEvent(init: { code: string, key: string, alt?: boolean, ctrl?: boolean, shift?: boolean, meta?: boolean }) {
  return {
    code: init.code,
    key: init.key,
    altKey: init.alt ?? false,
    ctrlKey: init.ctrl ?? false,
    shiftKey: init.shift ?? false,
    metaKey: init.meta ?? false,
  } as KeyboardEvent
}

describe('macOS 快捷鍵校驗', () => {
  // 迴歸：單鍵也被套上了組合鍵的規則，設右 ⌥、F5、滑鼠側鍵時都報「需要帶上 ⌘ 或 ⌃」
  it('單鍵和滑鼠鍵不受組合鍵規則的限制', () => {
    for (const key of ['AltRight', 'AltLeft', 'MetaRight', 'ControlRight', 'ShiftRight', 'Fn', 'F5', 'Space', 'XButton1', 'MButton']) {
      expect(getAcceleratorShortcutValidationError(key), key).toBeNull()
    }
  })

  // 修飾鍵只有 ⌥ / ⇧ 的組合系統快捷鍵在 macOS 15 起不觸發，現在由 Rust 的 event tap 接管，可以用了
  it('只帶 ⌥、⇧ 或 ⌥⇧ 的組合鍵可以用，一個修飾鍵加會打字的鍵照樣攔下', () => {
    for (const combo of ['Alt+Shift+K', 'Alt+Shift+1', 'Alt+F5', 'Shift+F5', 'Alt+Shift+Comma']) {
      expect(getAcceleratorShortcutValidationError(combo), combo).toBeNull()
    }
    for (const combo of ['Alt+K', 'Shift+K', 'Alt+2', 'Alt+Comma', 'Shift+Space', 'Alt+Space', 'Alt+Left', 'Alt+Numpad1']) {
      expect(getAcceleratorShortcutValidationError(combo), combo).not.toBeNull()
    }
  })

  // 使用者實測時把免持設成了 ⌘W：註冊成功了，但從此所有程式裡的 ⌘W（關視窗）都被 SayIt 搶走
  it('一個修飾鍵 + 字母 / 數字 / 方向鍵被攔下，加第二個修飾鍵或換 F 鍵就放行', () => {
    for (const combo of ['CommandOrControl+W', 'CommandOrControl+C', 'CommandOrControl+1', 'CommandOrControl+,', 'CommandOrControl+Left', 'Control+A', 'Control+Right', 'CommandOrControl+Return']) {
      expect(getAcceleratorShortcutValidationError(combo), combo).not.toBeNull()
    }
    for (const combo of ['CommandOrControl+Shift+W', 'Control+Alt+W', 'Control+CommandOrControl+D', 'CommandOrControl+F5', 'Control+F9']) {
      expect(getAcceleratorShortcutValidationError(combo), combo).toBeNull()
    }
    expect(getAcceleratorShortcutValidationError('CommandOrControl+Alt+Escape')).not.toBeNull()
  })

  it('帶 ⌘ 或 ⌃ 的組合鍵放行，系統佔用的組合攔下', () => {
    expect(getAcceleratorShortcutValidationError('CommandOrControl+Shift+K')).toBeNull()
    expect(getAcceleratorShortcutValidationError('Control+Alt+K')).toBeNull()
    expect(getAcceleratorShortcutValidationError('CommandOrControl+Space')).not.toBeNull()
    expect(getAcceleratorShortcutValidationError('Control+Space')).not.toBeNull()
    expect(getAcceleratorShortcutValidationError('CommandOrControl+Tab')).not.toBeNull()
    expect(getAcceleratorShortcutValidationError('CommandOrControl+Shift+4')).not.toBeNull()
  })

  it('錄製：單按修飾鍵錄成單鍵，⌘ 與 ⌃ 分開記', () => {
    expect(keyEventToShortcutCandidate(keyEvent({ code: 'MetaRight', key: 'Meta', meta: true }))).toBe('MetaRight')
    expect(keyEventToShortcutCandidate(keyEvent({ code: 'AltRight', key: 'Alt', alt: true }))).toBe('AltRight')
    expect(keyEventToShortcutCandidate(keyEvent({ code: 'KeyK', key: 'k', meta: true }))).toBe('CommandOrControl+K')
    expect(keyEventToShortcutCandidate(keyEvent({ code: 'KeyK', key: 'k', ctrl: true }))).toBe('Control+K')
  })

  // ⌥ 和 ⇧ 會改掉 event.key：按字元錄的話會存成 Rust 端解析不了的「Alt+Shift+Î」「Control+Shift+!」
  it('錄製：組合鍵的主鍵按鍵位記，不受 ⌥ / ⇧ 打出的字元影響', () => {
    expect(keyEventToShortcutCandidate(keyEvent({ code: 'KeyD', key: 'Î', alt: true, shift: true }))).toBe('Alt+Shift+D')
    expect(keyEventToShortcutCandidate(keyEvent({ code: 'Digit1', key: '!', ctrl: true, shift: true }))).toBe('Control+Shift+1')
    expect(keyEventToShortcutCandidate(keyEvent({ code: 'Comma', key: '¯', alt: true, shift: true }))).toBe('Alt+Shift+Comma')
    expect(keyEventToShortcutCandidate(keyEvent({ code: 'Enter', key: 'Enter', meta: true, shift: true }))).toBe('CommandOrControl+Shift+Enter')
    expect(keyEventToShortcutCandidate(keyEvent({ code: 'ArrowUp', key: 'ArrowUp', ctrl: true, alt: true }))).toBe('Control+Alt+Up')
    expect(keyEventToShortcutCandidate(keyEvent({ code: 'F5', key: 'F5', shift: true }))).toBe('Shift+F5')
    // 主鍵當不了快捷鍵的（ISO 鍵盤 Z 左邊那個鍵）：還沒成型，保留上一個候選
    expect(keyEventToShortcutCandidate(keyEvent({ code: 'IntlBackslash', key: '§', alt: true, shift: true }), { comboOnly: true })).toBeNull()
  })

  it('組合鍵顯示成 mac 選單裡的寫法', () => {
    expect(displayAccelerator('Alt+Shift+Comma')).toEqual(['⌥', '⇧', ','])
    expect(displayAccelerator('CommandOrControl+Shift+Enter')).toEqual(['⌘', '⇧', '↩'])
    expect(displayAccelerator('Control+Alt+Up')).toEqual(['⌃', '⌥', '↑'])
  })

  it('按住說話：單獨的 ⌘ 可以用（mac 上單按 ⌘ 沒有系統動作）', () => {
    expect(getPTTShortcutValidationError('MetaRight')).toBeNull()
    expect(getPTTShortcutValidationError('AltRight')).toBeNull()
  })

  it('報錯文案用 mac 的修飾鍵名，不提 Ctrl / Win / Alt', () => {
    const message = getPTTShortcutValidationError('KeyK') ?? ''
    expect(message).toContain('⌘')
    expect(message).not.toMatch(/Ctrl|Win|Alt/)
  })
})
