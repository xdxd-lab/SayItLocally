//! macOS 虛擬鍵碼（kVK_*，Carbon Events.h）與 SayIt 內部鍵 ID 的互轉。
//!
//! 熱鍵狀態機（keyboard/mod.rs）和前端都以 **Windows 虛擬鍵碼** 作為鍵的內部 ID
//! （`SINGLE_KEY_TABLE`、`PTTEvent.keycode`）。mac 上不另起一套 ID，而是在事件入口把
//! macOS 鍵碼翻譯成同一套 VK，這樣狀態機、單測和前後端契約全部複用。
//!
//! Mac 鍵盤上不存在的鍵（ContextMenu / Pause / ScrollLock / BrowserBack / F21–F24 等）
//! 這裡沒有對映，按了也不會命中；前端在 mac 上不展示這些鍵。

use super::ffi::{
    CGEventFlags, CGKeyCode, kCGEventFlagMaskAlternate, kCGEventFlagMaskCommand, kCGEventFlagMaskControl,
    kCGEventFlagMaskSecondaryFn, kCGEventFlagMaskShift, NX_DEVICELALTKEYMASK, NX_DEVICELCMDKEYMASK,
    NX_DEVICELCTLKEYMASK, NX_DEVICELSHIFTKEYMASK, NX_DEVICERALTKEYMASK, NX_DEVICERCMDKEYMASK,
    NX_DEVICERCTLKEYMASK, NX_DEVICERSHIFTKEYMASK,
};

/// Fn / 🌐 鍵的內部 ID。Windows 沒有對應的虛擬鍵，取保留值 0xFF；只有 mac 會產生它。
pub const VK_FN: u32 = 0xFF;

pub const KVK_ANSI_C: CGKeyCode = 0x08;
pub const KVK_ANSI_V: CGKeyCode = 0x09;
pub const KVK_ESCAPE: CGKeyCode = 0x35;
pub const KVK_COMMAND: CGKeyCode = 0x37;
pub const KVK_FUNCTION: CGKeyCode = 0x3F;
pub const KVK_CAPS_LOCK: CGKeyCode = 0x39;

/// (Windows VK, macOS 鍵碼)。修飾鍵單獨放在 MODIFIERS 裡。
const KEYS: &[(u32, CGKeyCode)] = &[
    // 字母（ANSI 物理位置）
    (0x41, 0x00), // A
    (0x42, 0x0B), // B
    (0x43, 0x08), // C
    (0x44, 0x02), // D
    (0x45, 0x0E), // E
    (0x46, 0x03), // F
    (0x47, 0x05), // G
    (0x48, 0x04), // H
    (0x49, 0x22), // I
    (0x4A, 0x26), // J
    (0x4B, 0x28), // K
    (0x4C, 0x25), // L
    (0x4D, 0x2E), // M
    (0x4E, 0x2D), // N
    (0x4F, 0x1F), // O
    (0x50, 0x23), // P
    (0x51, 0x0C), // Q
    (0x52, 0x0F), // R
    (0x53, 0x01), // S
    (0x54, 0x11), // T
    (0x55, 0x20), // U
    (0x56, 0x09), // V
    (0x57, 0x0D), // W
    (0x58, 0x07), // X
    (0x59, 0x10), // Y
    (0x5A, 0x06), // Z
    // 數字行
    (0x30, 0x1D), // 0
    (0x31, 0x12), // 1
    (0x32, 0x13), // 2
    (0x33, 0x14), // 3
    (0x34, 0x15), // 4
    (0x35, 0x17), // 5
    (0x36, 0x16), // 6
    (0x37, 0x1A), // 7
    (0x38, 0x1C), // 8
    (0x39, 0x19), // 9
    // 功能鍵（F13–F20 有 kVK 碼，F21–F24 沒有）
    (0x70, 0x7A), // F1
    (0x71, 0x78), // F2
    (0x72, 0x63), // F3
    (0x73, 0x76), // F4
    (0x74, 0x60), // F5
    (0x75, 0x61), // F6
    (0x76, 0x62), // F7
    (0x77, 0x64), // F8
    (0x78, 0x65), // F9
    (0x79, 0x6D), // F10
    (0x7A, 0x67), // F11
    (0x7B, 0x6F), // F12
    (0x7C, 0x69), // F13
    (0x7D, 0x6B), // F14
    (0x7E, 0x71), // F15
    (0x7F, 0x6A), // F16
    (0x80, 0x40), // F17
    (0x81, 0x4F), // F18
    (0x82, 0x50), // F19
    (0x83, 0x5A), // F20
    // 其它
    (0x20, 0x31), // Space
    (0x1B, 0x35), // Escape
    (0x09, 0x30), // Tab
    (0x0D, 0x24), // Enter（Return）
    (0x08, 0x33), // Backspace（Mac 的 Delete）
    (0x2E, 0x75), // Delete（Mac 的 Forward Delete）
    (0x2D, 0x72), // Insert（PC 鍵盤接 Mac 時報成 Help）
    (0x26, 0x7E), // ArrowUp
    (0x28, 0x7D), // ArrowDown
    (0x25, 0x7B), // ArrowLeft
    (0x27, 0x7C), // ArrowRight
    (0x24, 0x73), // Home
    (0x23, 0x77), // End
    (0x21, 0x74), // PageUp
    (0x22, 0x79), // PageDown
    // 標點（ANSI 物理位置，對應 Windows 的 VK_OEM_*）：組合鍵的主鍵可能是它們
    (0xBA, 0x29), // ; Semicolon
    (0xBB, 0x18), // = Equal
    (0xBC, 0x2B), // , Comma
    (0xBD, 0x1B), // - Minus
    (0xBE, 0x2F), // . Period
    (0xBF, 0x2C), // / Slash
    (0xC0, 0x32), // ` Backquote
    (0xDB, 0x21), // [ BracketLeft
    (0xDC, 0x2A), // \ Backslash
    (0xDD, 0x1E), // ] BracketRight
    (0xDE, 0x27), // ' Quote
    // 數字小鍵盤（Enter 與主鍵盤的 Enter 在 Windows 上同一個 VK，不對映）
    (0x60, 0x52), // Numpad0
    (0x61, 0x53), // Numpad1
    (0x62, 0x54), // Numpad2
    (0x63, 0x55), // Numpad3
    (0x64, 0x56), // Numpad4
    (0x65, 0x57), // Numpad5
    (0x66, 0x58), // Numpad6
    (0x67, 0x59), // Numpad7
    (0x68, 0x5B), // Numpad8
    (0x69, 0x5C), // Numpad9
    (0x6A, 0x43), // NumpadMultiply
    (0x6B, 0x45), // NumpadAdd
    (0x6D, 0x4E), // NumpadSubtract
    (0x6E, 0x41), // NumpadDecimal
    (0x6F, 0x4B), // NumpadDivide
];

/// 修飾鍵：(Windows VK, macOS 鍵碼, 裝置相關位, 同族的裝置相關位, 族掩碼)。
///
/// flagsChanged 事件只告訴你「修飾狀態變了」，要靠裝置相關位判斷是哪一側、按下還是抬起。
/// 少數虛擬鍵盤/驅動不填裝置相關位，此時退回看族掩碼（見 `modifier_is_down`）。
const MODIFIERS: &[(u32, CGKeyCode, CGEventFlags, CGEventFlags, CGEventFlags)] = &[
    (0xA0, 0x38, NX_DEVICELSHIFTKEYMASK, NX_DEVICELSHIFTKEYMASK | NX_DEVICERSHIFTKEYMASK, kCGEventFlagMaskShift),
    (0xA1, 0x3C, NX_DEVICERSHIFTKEYMASK, NX_DEVICELSHIFTKEYMASK | NX_DEVICERSHIFTKEYMASK, kCGEventFlagMaskShift),
    (0xA2, 0x3B, NX_DEVICELCTLKEYMASK, NX_DEVICELCTLKEYMASK | NX_DEVICERCTLKEYMASK, kCGEventFlagMaskControl),
    (0xA3, 0x3E, NX_DEVICERCTLKEYMASK, NX_DEVICELCTLKEYMASK | NX_DEVICERCTLKEYMASK, kCGEventFlagMaskControl),
    (0xA4, 0x3A, NX_DEVICELALTKEYMASK, NX_DEVICELALTKEYMASK | NX_DEVICERALTKEYMASK, kCGEventFlagMaskAlternate),
    (0xA5, 0x3D, NX_DEVICERALTKEYMASK, NX_DEVICELALTKEYMASK | NX_DEVICERALTKEYMASK, kCGEventFlagMaskAlternate),
    (0x5B, 0x37, NX_DEVICELCMDKEYMASK, NX_DEVICELCMDKEYMASK | NX_DEVICERCMDKEYMASK, kCGEventFlagMaskCommand),
    (0x5C, 0x36, NX_DEVICERCMDKEYMASK, NX_DEVICELCMDKEYMASK | NX_DEVICERCMDKEYMASK, kCGEventFlagMaskCommand),
    // Fn 只有一個，沒有裝置相關位，直接看 SecondaryFn。
    (VK_FN, KVK_FUNCTION, kCGEventFlagMaskSecondaryFn, kCGEventFlagMaskSecondaryFn, kCGEventFlagMaskSecondaryFn),
];

/// 會讓單鍵熱鍵「不算裸按」的修飾鍵族（Fn 不算：方向鍵、F 鍵本身就帶這個位）。
pub const BLOCKING_MODIFIER_MASK: CGEventFlags =
    kCGEventFlagMaskShift | kCGEventFlagMaskControl | kCGEventFlagMaskAlternate | kCGEventFlagMaskCommand;

/// 普通鍵（keyDown/keyUp）的鍵碼 → 內部 VK。
pub fn vk_for_keycode(keycode: CGKeyCode) -> Option<u32> {
    KEYS.iter().find(|(_, kc)| *kc == keycode).map(|(vk, _)| *vk)
}

/// 內部 VK → 普通鍵鍵碼。
pub fn keycode_for_vk(vk: u32) -> Option<CGKeyCode> {
    KEYS.iter()
        .find(|(v, _)| *v == vk)
        .map(|(_, kc)| *kc)
        .or_else(|| modifier_keycode_for_vk(vk))
}

/// 修飾鍵（flagsChanged）的鍵碼 → 內部 VK。Caps Lock 不在表裡：mac 上它只報切換、不報鬆開，
/// 中文使用者還常用它切輸入法，不支援當熱鍵。
pub fn modifier_vk_for_keycode(keycode: CGKeyCode) -> Option<u32> {
    MODIFIERS.iter().find(|m| m.1 == keycode).map(|m| m.0)
}

fn modifier_keycode_for_vk(vk: u32) -> Option<CGKeyCode> {
    MODIFIERS.iter().find(|m| m.0 == vk).map(|m| m.1)
}

pub fn is_modifier_vk(vk: u32) -> bool {
    MODIFIERS.iter().any(|m| m.0 == vk)
}

/// 根據修飾標誌判斷某個修飾鍵此刻是否按著。`keycode` 必須是修飾鍵鍵碼。
pub fn modifier_is_down(keycode: CGKeyCode, flags: CGEventFlags) -> bool {
    let Some(&(_, _, device_bit, family_device_bits, family_mask)) = MODIFIERS.iter().find(|m| m.1 == keycode) else {
        return false;
    };
    if flags & device_bit != 0 {
        return true;
    }
    // 沒填裝置相關位的鍵盤：只要同族一個裝置位都沒有、族掩碼卻在，就按「按著」算。
    flags & family_device_bits == 0 && flags & family_mask != 0
}

/// 按內部 VK 判斷修飾鍵是否按著（給看門狗查物理狀態用）。
pub fn modifier_vk_is_down(vk: u32, flags: CGEventFlags) -> Option<bool> {
    modifier_keycode_for_vk(vk).map(|kc| modifier_is_down(kc, flags))
}

/// 滑鼠按鈕號（kCGMouseEventButtonNumber）→ 內部 VK：2 中鍵、3 後退側鍵、4 前進側鍵。
pub fn vk_for_mouse_button(button: i64) -> Option<u32> {
    match button {
        2 => Some(0x04),
        3 => Some(0x05),
        4 => Some(0x06),
        _ => None,
    }
}

pub fn mouse_button_for_vk(vk: u32) -> Option<u32> {
    match vk {
        0x04 => Some(2),
        0x05 => Some(3),
        0x06 => Some(4),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn right_option_and_right_command_map_to_the_windows_vks_used_by_settings() {
        assert_eq!(modifier_vk_for_keycode(0x3D), Some(0xA5)); // 右 ⌥ = AltRight
        assert_eq!(modifier_vk_for_keycode(0x36), Some(0x5C)); // 右 ⌘ = MetaRight
        assert_eq!(modifier_vk_for_keycode(KVK_FUNCTION), Some(VK_FN));
        assert_eq!(modifier_vk_for_keycode(KVK_CAPS_LOCK), None);
    }

    #[test]
    fn every_key_mapping_roundtrips() {
        for &(vk, kc) in KEYS {
            assert_eq!(vk_for_keycode(kc), Some(vk), "keycode 0x{kc:X}");
            assert_eq!(keycode_for_vk(vk), Some(kc), "vk 0x{vk:X}");
        }
        for m in MODIFIERS {
            assert_eq!(keycode_for_vk(m.0), Some(m.1));
        }
    }

    #[test]
    fn keycodes_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for &(_, kc) in KEYS {
            assert!(seen.insert(kc), "duplicate keycode 0x{kc:X}");
        }
        for m in MODIFIERS {
            assert!(seen.insert(m.1), "duplicate modifier keycode 0x{:X}", m.1);
        }
    }

    #[test]
    fn modifier_sides_are_told_apart_by_device_bits() {
        // 只按著左 ⌥：右 ⌥ 不算按著
        let left_only = kCGEventFlagMaskAlternate | NX_DEVICELALTKEYMASK;
        assert!(modifier_is_down(0x3A, left_only));
        assert!(!modifier_is_down(0x3D, left_only));
        // 兩側都鬆開
        assert!(!modifier_is_down(0x3D, 0));
        // 不填裝置位的鍵盤：退回族掩碼
        assert!(modifier_is_down(0x3D, kCGEventFlagMaskAlternate));
        // Fn
        assert!(modifier_is_down(KVK_FUNCTION, kCGEventFlagMaskSecondaryFn));
        assert!(!modifier_is_down(KVK_FUNCTION, kCGEventFlagMaskCommand));
    }

    #[test]
    fn f21_to_f24_and_pc_only_keys_have_no_mac_keycode() {
        for vk in [0x84, 0x85, 0x86, 0x87, 0x5D, 0x13, 0x91, 0xA6, 0xA7, 0x14] {
            assert_eq!(keycode_for_vk(vk), None, "vk 0x{vk:X}");
        }
    }

    #[test]
    fn mouse_buttons_roundtrip() {
        for vk in [0x04, 0x05, 0x06] {
            let button = mouse_button_for_vk(vk).unwrap();
            assert_eq!(vk_for_mouse_button(button as i64), Some(vk));
        }
    }
}
