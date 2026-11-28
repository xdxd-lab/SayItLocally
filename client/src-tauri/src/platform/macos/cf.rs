//! CoreFoundation 小工具：擁有所有權的 CF 引用（Drop 時 CFRelease）、CFString 互轉。

use super::ffi::*;
use std::ffi::c_void;

/// 持有一份 +1 引用的 CF 物件，離開作用域自動 CFRelease。
/// 只用於 Create/Copy 規則返回的物件（以及我們自己 CFRetain 過的）。
pub struct CfOwned(CFTypeRef);

impl CfOwned {
    /// 接管一個 Create/Copy 返回的引用；空指標返回 None。
    ///
    /// # Safety
    /// `ptr` 必須是呼叫方擁有的 +1 引用。
    pub unsafe fn from_create(ptr: CFTypeRef) -> Option<Self> {
        if ptr.is_null() {
            None
        } else {
            Some(Self(ptr))
        }
    }

    /// 對一個借來的引用 CFRetain 一次後持有。
    ///
    /// # Safety
    /// `ptr` 必須是有效的 CF 物件。
    pub unsafe fn retain(ptr: CFTypeRef) -> Option<Self> {
        if ptr.is_null() {
            None
        } else {
            CFRetain(ptr);
            Some(Self(ptr))
        }
    }

    pub fn as_ptr(&self) -> CFTypeRef {
        self.0
    }

    pub fn type_id(&self) -> CFTypeID {
        unsafe { CFGetTypeID(self.0) }
    }
}

impl Drop for CfOwned {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}

// CF 物件本身是執行緒安全的引用計數物件；這裡只在持有期間讀取。
unsafe impl Send for CfOwned {}
unsafe impl Sync for CfOwned {}

/// Rust 字符串 → CFString（+1）。
pub fn cfstr(s: &str) -> CfOwned {
    unsafe {
        let ptr = CFStringCreateWithBytes(
            std::ptr::null(),
            s.as_ptr(),
            s.len() as CFIndex,
            kCFStringEncodingUTF8,
            0,
        );
        CfOwned::from_create(ptr).expect("CFStringCreateWithBytes returned NULL")
    }
}

/// CFString → Rust String。不是 CFString 時返回 None。
///
/// # Safety
/// `s` 必須是有效的 CF 物件或空指標。
pub unsafe fn cfstring_to_string(s: CFTypeRef) -> Option<String> {
    if s.is_null() || CFGetTypeID(s) != CFStringGetTypeID() {
        return None;
    }
    let len = CFStringGetLength(s);
    let cap = CFStringGetMaximumSizeForEncoding(len, kCFStringEncodingUTF8) + 1;
    if cap <= 0 {
        return Some(String::new());
    }
    let mut buf = vec![0u8; cap as usize];
    if CFStringGetCString(s, buf.as_mut_ptr() as *mut _, cap, kCFStringEncodingUTF8) == 0 {
        return None;
    }
    let nul = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    buf.truncate(nul);
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// CFBoolean → bool。不是 CFBoolean 時返回 None。
///
/// # Safety
/// `b` 必須是有效的 CF 物件或空指標。
pub unsafe fn cfboolean_to_bool(b: CFTypeRef) -> Option<bool> {
    if b.is_null() || CFGetTypeID(b) != CFBooleanGetTypeID() {
        return None;
    }
    Some(CFBooleanGetValue(b) != 0)
}

/// CFNumber → i64。
///
/// # Safety
/// `n` 必須是有效的 CF 物件或空指標。
pub unsafe fn cfnumber_to_i64(n: CFTypeRef) -> Option<i64> {
    if n.is_null() || CFGetTypeID(n) != CFNumberGetTypeID() {
        return None;
    }
    let mut value: i64 = 0;
    if CFNumberGetValue(n, kCFNumberSInt64Type, &mut value as *mut i64 as *mut c_void) != 0 {
        Some(value)
    } else {
        None
    }
}

/// 遍歷 CFArray 裡的字串。
///
/// # Safety
/// `array` 必須是有效的 CFArray 或空指標。
pub unsafe fn cfarray_strings(array: CFTypeRef) -> Vec<String> {
    if array.is_null() || CFGetTypeID(array) != CFArrayGetTypeID() {
        return Vec::new();
    }
    let count = CFArrayGetCount(array);
    (0..count)
        .filter_map(|i| cfstring_to_string(CFArrayGetValueAtIndex(array, i)))
        .collect()
}

/// `{ key: true }` 這樣只有一個布林值的字典（給 AXIsProcessTrustedWithOptions 用）。
///
/// # Safety
/// `key` 必須是有效的 CFString。
pub unsafe fn single_bool_dictionary(key: CFStringRef, value: bool) -> Option<CfOwned> {
    let keys = [key as *const c_void];
    let values = [if value { kCFBooleanTrue } else { kCFBooleanFalse } as *const c_void];
    CfOwned::from_create(CFDictionaryCreate(
        std::ptr::null(),
        keys.as_ptr(),
        values.as_ptr(),
        1,
        &kCFTypeDictionaryKeyCallBacks,
        &kCFTypeDictionaryValueCallBacks,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cfstring_roundtrip_keeps_unicode() {
        let s = cfstr("说话 SayIt ✓");
        assert_eq!(unsafe { cfstring_to_string(s.as_ptr()) }.as_deref(), Some("说话 SayIt ✓"));
    }

    #[test]
    fn empty_string_roundtrip() {
        let s = cfstr("");
        assert_eq!(unsafe { cfstring_to_string(s.as_ptr()) }.as_deref(), Some(""));
    }
}
