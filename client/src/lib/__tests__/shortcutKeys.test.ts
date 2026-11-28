import { describe, expect, it } from 'vitest'
import {
  canonicalizePTTShortcut,
  displayPTTShortcut,
  getAcceleratorShortcutValidationError,
  getPTTShortcutWarning,
  getPTTShortcutValidationError,
  isValidPTTShortcut,
  keyEventToShortcutCandidate,
  parsePTTShortcut,
  PTT_CODE_TO_VK,
  pttShortcutConflictsWithAccelerator,
  pttShortcutHasModifier,
  pttShortcutToAccelerator,
} from '../shortcutKeys'

describe('PTT 物理组合键', () => {
  it('保留左右位置并按固定顺序规范化', () => {
    const value = canonicalizePTTShortcut('ShiftRight+KeyK+MetaLeft+ControlLeft')
    expect(value).toBe('ControlLeft+MetaLeft+ShiftRight+KeyK')
    expect(parsePTTShortcut(value)).toEqual([
      'ControlLeft',
      'MetaLeft',
      'ShiftRight',
      'KeyK',
    ])
    expect(displayPTTShortcut('ControlLeft+MetaLeft')).toEqual(['左 Ctrl', '左 Win'])
  })

  it('兼容旧单键，并接受普通组合与纯修饰组合', () => {
    // 這裡原來用 ShiftRight 當舊單鍵的樣例，現在 Shift 已被 PTT 停用（見下方專門的用例），
    // 換成同樣是"舊單鍵"的右 Alt / CapsLock
    expect(isValidPTTShortcut('AltRight')).toBe(true)
    expect(isValidPTTShortcut('CapsLock')).toBe(true)
    expect(isValidPTTShortcut('MButton')).toBe(true)
    expect(isValidPTTShortcut('ControlLeft+KeyK')).toBe(true)
    expect(isValidPTTShortcut('ControlLeft+MetaLeft')).toBe(true)
    expect(pttShortcutHasModifier('ControlLeft+KeyK')).toBe(true)
    expect(PTT_CODE_TO_VK.MetaLeft).toBe(0x5b)
    expect(PTT_CODE_TO_VK.KeyK).toBe(0x4b)
  })

  // F13–F24 是給可程式設計鍵盤 / 自制 HID 語音裝置當專用觸發鍵用的（物理鍵盤上沒有這些鍵，
  // 因此几乎不与任何程序冲突）。
  //
  // 這條斷言與 Rust 的 function_keys_f1_through_f24_are_all_present_with_contiguous_vks
  // 對稱：兩張表是手寫副本，只改一邊是靜默失敗 —— 前端放行 F13、Rust 查不到它，
  // 使用者介面上顯示 F13，實際說話鍵會悄悄回落成右 Ctrl。
  it('F1–F24 全部可用，vk 连续且可单独作为按住说话键', () => {
    for (let n = 1; n <= 24; n += 1) {
      const code = `F${n}`
      expect(PTT_CODE_TO_VK[code], `${code} 应当在按键表里`).toBe(0x70 + n - 1)
      expect(isValidPTTShortcut(code), `${code} 应当可以单独当按住说话键`).toBe(true)
    }
    // 語言中立的鍵名不進 locale，直接用表裡的 label
    expect(displayPTTShortcut('F13')).toEqual(['F13'])
    expect(displayPTTShortcut('F24')).toEqual(['F24'])
    // 也能當組合成員；Alt+F4 那條保留組合只匹配 F4，不牽連 F13–F24
    expect(isValidPTTShortcut('ControlLeft+F13')).toBe(true)
    expect(isValidPTTShortcut('AltLeft+F13')).toBe(true)
    expect(getPTTShortcutValidationError('AltLeft+F4')).not.toBeNull()
  })

  it('拒绝单独 Win、裸字母、多主键和危险系统组合', () => {
    expect(getPTTShortcutValidationError('MetaLeft')).toContain('不能單獨')
    expect(getPTTShortcutValidationError('KeyK')).toContain('不能單獨')
    expect(getPTTShortcutValidationError('ControlLeft+KeyK+KeyL')).toContain('最多')
    expect(getPTTShortcutValidationError('ControlLeft+ControlRight+KeyK')).toContain('左右')
    expect(getPTTShortcutValidationError('MetaLeft+KeyL')).toContain('系統組合')
    expect(getPTTShortcutValidationError('AltLeft+F4')).toContain('系統組合')
    expect(getPTTShortcutValidationError('MetaLeft+KeyK')).not.toBeNull()
    expect(getPTTShortcutValidationError('AltLeft+Space')).not.toBeNull()
    expect(isValidPTTShortcut('ControlLeft+MetaLeft')).toBe(true)
  })

  // 原來是「提示風險但仍允許儲存」。警告擋不住任何人：使用者照樣綁了右 Shift，
  // 照樣踩到篩選鍵讓錄音停不下來。現在改成硬攔。
  it('按住说话一律拒绝 Shift，单键和组合成员都算', () => {
    expect(isValidPTTShortcut('ShiftRight')).toBe(false)
    expect(isValidPTTShortcut('ShiftLeft')).toBe(false)
    expect(isValidPTTShortcut('ControlLeft+ShiftRight')).toBe(false)
    expect(isValidPTTShortcut('ShiftLeft+KeyK')).toBe(false)
    expect(getPTTShortcutValidationError('ShiftRight')).toContain('Shift')
    // 別的修飾鍵不受影響
    expect(isValidPTTShortcut('AltRight')).toBe(true)
    expect(isValidPTTShortcut('ControlLeft+KeyK')).toBe(true)
  })

  // 老使用者可能在 Shift 還能選的時候綁上了它：舊繫結繼續生效，但要在設定裡提示改綁。
  // 升級時靜默換掉使用者的說話鍵，或者讓它突然失效，都比留一行提示更糟。
  it('已保存的 Shift 旧配置给出改绑提示', () => {
    expect(getPTTShortcutWarning('ShiftRight')).toContain('Shift')
    expect(getPTTShortcutWarning('ControlLeft+ShiftLeft')).toContain('Shift')
    expect(getPTTShortcutWarning('AltRight')).toBeNull()
  })

  // Shift 只在「按住說話」裡有問題（要長按、會連按）。擴音這類按一下的快捷鍵照舊可用。
  it('按一下的快捷键不受 Shift 限制', () => {
    expect(getAcceleratorShortcutValidationError('CommandOrControl+Shift+K')).toBeNull()
  })

  it('通用组合键同样拒绝 Windows 保留快捷键', () => {
    expect(getAcceleratorShortcutValidationError('Control+Alt+Delete')).not.toBeNull()
    expect(getAcceleratorShortcutValidationError('Alt+Tab')).not.toBeNull()
    expect(getAcceleratorShortcutValidationError('Alt+Space')).not.toBeNull()
    expect(getAcceleratorShortcutValidationError('Control+Shift+Escape')).not.toBeNull()
    expect(getAcceleratorShortcutValidationError('Super+K')).not.toBeNull()
    expect(getAcceleratorShortcutValidationError('CommandOrControl+K')).toBeNull()
  })

  it('可与免提 accelerator 做语义冲突比较', () => {
    expect(pttShortcutToAccelerator('ControlLeft+KeyK')).toBe('CommandOrControl+K')
    expect(
      pttShortcutConflictsWithAccelerator('ControlLeft+KeyK', 'CommandOrControl+K'),
    ).toBe(true)
    expect(pttShortcutConflictsWithAccelerator('ShiftRight', 'ShiftRight')).toBe(true)
    expect(
      pttShortcutConflictsWithAccelerator('ControlLeft+MetaLeft', 'ControlLeft'),
    ).toBe(true)
    expect(
      pttShortcutConflictsWithAccelerator('ControlLeft+MetaLeft', 'CommandOrControl+K'),
    ).toBe(false)
  })
})
describe('按键事件 → 候选快捷键', () => {
  /** 只造出 keyEventToShortcutCandidate 會讀的那幾個欄位 */
  function keyEvent(init: {
    code: string
    key: string
    ctrl?: boolean
    alt?: boolean
    shift?: boolean
    meta?: boolean
  }): KeyboardEvent {
    return {
      code: init.code,
      key: init.key,
      ctrlKey: init.ctrl ?? false,
      altKey: init.alt ?? false,
      shiftKey: init.shift ?? false,
      metaKey: init.meta ?? false,
    } as KeyboardEvent
  }

  // 使用者反饋的原始症狀：嚮導裡按 Ctrl+D 會被靜默存成「左 Ctrl」。
  // 成因是 Ctrl 的 keydown 先到、ControlLeft 恰好在單鍵白名單裡，於是提交了它、
  // D 根本沒輪到。修法是把「先組合、後單鍵」的順序釘在這裡。
  it('Ctrl+D 录成组合键，中途单按 Ctrl 只是尚未成型', () => {
    expect(keyEventToShortcutCandidate(keyEvent({ code: 'ControlLeft', key: 'Control', ctrl: true })))
      .toBe('ControlLeft')
    expect(keyEventToShortcutCandidate(keyEvent({ code: 'KeyD', key: 'd', ctrl: true })))
      .toBe('CommandOrControl+D')
  })

  it('修饰键单键仍然可用（免提默认就是右 Alt）', () => {
    expect(keyEventToShortcutCandidate(keyEvent({ code: 'AltRight', key: 'Alt', alt: true })))
      .toBe('AltRight')
    expect(keyEventToShortcutCandidate(keyEvent({ code: 'ControlRight', key: 'Control', ctrl: true })))
      .toBe('ControlRight')
  })

  // Space / Insert / F1–F24 同時出現在單鍵白名單裡，判定順序反了就會把
  // Ctrl+Space、Ctrl+F1 也吃成單鍵 —— 使用者以為設了組合鍵，實際綁的是空格。
  it('同时属于单键白名单的键，带修饰时优先当组合键', () => {
    expect(keyEventToShortcutCandidate(keyEvent({ code: 'Space', key: ' ' }))).toBe('Space')
    expect(keyEventToShortcutCandidate(keyEvent({ code: 'Space', key: ' ', ctrl: true })))
      .toBe('CommandOrControl+Space')
    expect(keyEventToShortcutCandidate(keyEvent({ code: 'F1', key: 'F1' }))).toBe('F1')
    expect(keyEventToShortcutCandidate(keyEvent({ code: 'F1', key: 'F1', ctrl: true })))
      .toBe('CommandOrControl+F1')
  })

  it('裸字母不成型；comboOnly 下单键一律不成型', () => {
    expect(keyEventToShortcutCandidate(keyEvent({ code: 'KeyD', key: 'd' }))).toBeNull()
    expect(keyEventToShortcutCandidate(keyEvent({ code: 'AltRight', key: 'Alt', alt: true }), { comboOnly: true }))
      .toBeNull()
    expect(keyEventToShortcutCandidate(keyEvent({ code: 'F1', key: 'F1' }), { comboOnly: true }))
      .toBeNull()
    expect(
      keyEventToShortcutCandidate(keyEvent({ code: 'KeyD', key: 'd', ctrl: true, shift: true }), { comboOnly: true }),
    ).toBe('CommandOrControl+Shift+D')
  })
})
