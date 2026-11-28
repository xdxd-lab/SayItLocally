//! Accessibility（AXUIElement）封裝。對應 Windows 側的 UI Automation。
//!
//! 所有呼叫都是對目標程序的 IPC，目標卡住時會一直等；因此每個元素都要先設訊息超時
//! （見 `set_timeout`），別讓熱鍵路徑被別人的程式拖住。

use super::cf::{cfboolean_to_bool, cfnumber_to_i64, cfstr, cfstring_to_string, CfOwned};
use super::ffi::*;
use std::ffi::c_void;

/// AX 呼叫的預設超時。和 Windows 側 UIA 查詢的量級一致：正常目標幾毫秒就回，
/// 超過這個時間就當拿不到，寧可少一條訊號也不拖慢熱鍵。
pub const DEFAULT_TIMEOUT_SECS: f32 = 0.25;

pub fn is_text_role(role: &str) -> bool {
    matches!(role, "AXTextField" | "AXTextArea" | "AXComboBox" | "AXSearchField")
}

pub struct AxElement(CfOwned);

impl AxElement {
    /// 系統級元素：可以問「當前焦點在哪個程式 / 哪個元素」。
    pub fn system_wide() -> Option<Self> {
        unsafe { CfOwned::from_create(AXUIElementCreateSystemWide()).map(Self) }
    }

    /// 某個程序的應用元素。
    pub fn application(pid: i32) -> Option<Self> {
        if pid <= 0 {
            return None;
        }
        unsafe { CfOwned::from_create(AXUIElementCreateApplication(pid)).map(Self) }
    }

    pub fn as_ptr(&self) -> AXUIElementRef {
        self.0.as_ptr()
    }

    // [自訂] L-80：同一個 AX 元件可有不同的 CF 參照。
    pub fn equals(&self, other: &Self) -> bool {
        unsafe { CFEqual(self.as_ptr(), other.as_ptr()) != 0 }
    }

    pub fn set_timeout(&self, secs: f32) {
        unsafe {
            let _ = AXUIElementSetMessagingTimeout(self.as_ptr(), secs);
        }
    }

    pub fn pid(&self) -> Option<i32> {
        let mut pid: i32 = 0;
        let err = unsafe { AXUIElementGetPid(self.as_ptr(), &mut pid) };
        (err == kAXErrorSuccess && pid > 0).then_some(pid)
    }

    /// 讀屬性（+1 引用）。失敗、無值都返回 None。
    pub fn attr(&self, name: &str) -> Option<CfOwned> {
        let key = cfstr(name);
        let mut value: CFTypeRef = std::ptr::null();
        let err = unsafe { AXUIElementCopyAttributeValue(self.as_ptr(), key.as_ptr(), &mut value) };
        if err != kAXErrorSuccess {
            return None;
        }
        unsafe { CfOwned::from_create(value) }
    }

    /// 讀屬性，並返回 AX 錯誤碼（用來區分「沒授權」與「沒這個屬性」）。
    pub fn attr_with_error(&self, name: &str) -> Result<CfOwned, AXError> {
        let key = cfstr(name);
        let mut value: CFTypeRef = std::ptr::null();
        let err = unsafe { AXUIElementCopyAttributeValue(self.as_ptr(), key.as_ptr(), &mut value) };
        if err != kAXErrorSuccess {
            return Err(err);
        }
        unsafe { CfOwned::from_create(value) }.ok_or(err)
    }

    pub fn attr_string(&self, name: &str) -> Option<String> {
        let value = self.attr(name)?;
        unsafe { cfstring_to_string(value.as_ptr()) }
    }

    pub fn attr_bool(&self, name: &str) -> Option<bool> {
        let value = self.attr(name)?;
        unsafe { cfboolean_to_bool(value.as_ptr()) }
    }

    pub fn attr_i64(&self, name: &str) -> Option<i64> {
        let value = self.attr(name)?;
        unsafe { cfnumber_to_i64(value.as_ptr()) }
    }

    pub fn attr_element(&self, name: &str) -> Option<AxElement> {
        let value = self.attr(name)?;
        if value.type_id() != unsafe { AXUIElementGetTypeID() } {
            return None;
        }
        Some(AxElement(value))
    }

    fn attr_ax_value<T: Default>(&self, name: &str, value_type: AXValueType) -> Option<T> {
        let value = self.attr(name)?;
        if value.type_id() != unsafe { AXValueGetTypeID() } {
            return None;
        }
        let mut out = T::default();
        let ok = unsafe { AXValueGetValue(value.as_ptr(), value_type, &mut out as *mut T as *mut c_void) };
        (ok != 0).then_some(out)
    }

    pub fn attr_range(&self, name: &str) -> Option<CFRange> {
        self.attr_ax_value::<CFRange>(name, kAXValueTypeCFRange)
    }

    pub fn attr_point(&self, name: &str) -> Option<CGPoint> {
        self.attr_ax_value::<CGPoint>(name, kAXValueTypeCGPoint)
    }

    pub fn attr_size(&self, name: &str) -> Option<CGSize> {
        self.attr_ax_value::<CGSize>(name, kAXValueTypeCGSize)
    }

    pub fn is_settable(&self, name: &str) -> Option<bool> {
        let key = cfstr(name);
        let mut settable: Boolean = 0;
        let err = unsafe { AXUIElementIsAttributeSettable(self.as_ptr(), key.as_ptr(), &mut settable) };
        (err == kAXErrorSuccess).then_some(settable != 0)
    }

    /// `AXStringForRange`：按字元區間取文本（UTF-16 下標，與 CFString 一致）。
    pub fn string_for_range(&self, range: CFRange) -> Option<String> {
        if range.length <= 0 || range.location < 0 {
            return None;
        }
        let param = unsafe {
            CfOwned::from_create(AXValueCreate(
                kAXValueTypeCFRange,
                &range as *const CFRange as *const c_void,
            ))
        }?;
        let key = cfstr("AXStringForRange");
        let mut value: CFTypeRef = std::ptr::null();
        let err = unsafe {
            AXUIElementCopyParameterizedAttributeValue(self.as_ptr(), key.as_ptr(), param.as_ptr(), &mut value)
        };
        if err != kAXErrorSuccess {
            return None;
        }
        let owned = unsafe { CfOwned::from_create(value) }?;
        unsafe { cfstring_to_string(owned.as_ptr()) }
    }

    pub fn set_bool(&self, name: &str, value: bool) -> bool {
        let key = cfstr(name);
        let v = if value { unsafe { kCFBooleanTrue } } else { unsafe { kCFBooleanFalse } };
        unsafe { AXUIElementSetAttributeValue(self.as_ptr(), key.as_ptr(), v) == kAXErrorSuccess }
    }

    pub fn set_string(&self, name: &str, value: &str) -> bool {
        let key = cfstr(name);
        let v = cfstr(value);
        unsafe { AXUIElementSetAttributeValue(self.as_ptr(), key.as_ptr(), v.as_ptr()) == kAXErrorSuccess }
    }
}

/// 本程序是否已獲得「輔助功能」授權。
pub fn is_trusted() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}

/// 檢查授權；`prompt = true` 時如果還沒授權，系統會彈出引導使用者去「系統設定」的對話方塊。
pub fn is_trusted_with_prompt(prompt: bool) -> bool {
    unsafe {
        match super::cf::single_bool_dictionary(kAXTrustedCheckOptionPrompt, prompt) {
            Some(options) => AXIsProcessTrustedWithOptions(options.as_ptr()) != 0,
            None => AXIsProcessTrusted() != 0,
        }
    }
}

/// 當前擁有鍵盤焦點的應用的 pid（經由輔助功能，任何執行緒都可以調）。
pub fn focused_application_pid() -> Option<i32> {
    let system = AxElement::system_wide()?;
    system.set_timeout(DEFAULT_TIMEOUT_SECS);
    let app = system.attr_element("AXFocusedApplication")?;
    app.pid()
}
