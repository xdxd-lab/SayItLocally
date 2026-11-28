//! 錄音期間靜音系統輸出（預設揚聲器）——防止外放的聲音被麥克風回採。
//!
//! 只在採集期間靜音，鬆開熱鍵立即恢復到使用者原本的靜音狀態：
//! - `mute_system_output`：記錄當前預設輸出裝置的靜音狀態，然後靜音。
//! - `restore_system_output`：恢復到之前記錄的狀態（若使用者本來就是靜音，則保持靜音）。
//!
//! 用「儲存/恢復原狀態」而非「無腦取消靜音」，避免錄完後把使用者原本的靜音誤開成有聲。

use serde::Serialize;

#[cfg(any(windows, target_os = "macos"))]
use once_cell::sync::Lazy;
#[cfg(any(windows, target_os = "macos"))]
use std::sync::Mutex;

/// 查詢麥克風靜音狀態的結果。
/// - `matched`：是否成功定位到目標裝置（預設裝置總能定位；指定裝置需按名字唯一匹配到）。
///   為 false 時前端不應據此判定，應退回基於音訊訊號的檢測。
/// - `muted`：目標裝置在系統層面是否被靜音。
#[derive(Debug, Clone, Serialize)]
pub struct MicMuteState {
    pub matched: bool,
    pub muted: bool,
}

/// 儲存的「靜音前的原始狀態」。None 表示當前未處於我們施加的靜音中。
#[cfg(windows)]
static SAVED_MUTE_STATE: Lazy<Mutex<Option<bool>>> = Lazy::new(|| Mutex::new(None));

/// macOS：靜音前預設輸出裝置的狀態（Mute 屬性或各通道音量）。None = 當前不在我們施加的靜音中。
#[cfg(target_os = "macos")]
static SAVED_OUTPUT_STATE: Lazy<Mutex<Option<crate::platform::macos::audio::SavedOutputState>>> =
    Lazy::new(|| Mutex::new(None));

/// 獲取預設渲染端點（揚聲器）的音量控制介面，並在其上執行閉包。
#[cfg(windows)]
unsafe fn with_endpoint_volume<F, R>(f: F) -> Result<R, String>
where
    F: FnOnce(&windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume) -> Result<R, String>,
{
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{
        eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };

    let co_init_result = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    let need_co_uninit = co_init_result.is_ok();

    let result = (|| -> Result<R, String> {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| format!("CoCreateInstance MMDeviceEnumerator: {}", e))?;

        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|e| format!("GetDefaultAudioEndpoint: {}", e))?;

        let endpoint: IAudioEndpointVolume = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| format!("Activate IAudioEndpointVolume: {}", e))?;

        f(&endpoint)
    })();

    if need_co_uninit {
        CoUninitialize();
    }

    result
}

/// 記錄當前預設輸出裝置靜音狀態並將其靜音。
/// 返回 true 表示已處理（Windows），false 表示非 Windows 平台跳過。
#[tauri::command]
pub fn mute_system_output() -> Result<bool, String> {
    #[cfg(windows)]
    unsafe {
        with_endpoint_volume(|ep| {
            let was_muted = ep
                .GetMute()
                .map_err(|e| format!("GetMute: {}", e))?
                .as_bool();

            // 僅在首次靜音時儲存原狀態，避免重複呼叫覆蓋真實原值
            {
                let mut saved = SAVED_MUTE_STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                if saved.is_none() {
                    *saved = Some(was_muted);
                }
            }

            if !was_muted {
                ep.SetMute(true, std::ptr::null())
                    .map_err(|e| format!("SetMute(true): {}", e))?;
            }

            crate::commands::system::write_log_line(&format!(
                "[RUST] [audio_mute] muted output (prev_muted={})",
                was_muted
            ));
            Ok(true)
        })
    }
    #[cfg(target_os = "macos")]
    {
        let mut saved = SAVED_OUTPUT_STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // 僅在首次靜音時儲存原狀態，避免重複呼叫覆蓋真實原值
        if saved.is_some() {
            return Ok(true);
        }
        let state = crate::platform::macos::audio::mute_default_output()?;
        crate::commands::system::write_log_line(&format!("[RUST] [audio_mute] muted output (saved={:?})", state));
        *saved = Some(state);
        Ok(true)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        Ok(false)
    }
}

/// 恢復到 `mute_system_output` 之前記錄的靜音狀態。
/// 若沒有記錄（未曾靜音），則不做任何操作。
#[tauri::command]
pub fn restore_system_output() -> Result<bool, String> {
    #[cfg(windows)]
    unsafe {
        let saved = {
            let mut s = SAVED_MUTE_STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            s.take()
        };

        match saved {
            Some(prev) => with_endpoint_volume(|ep| {
                ep.SetMute(windows::Win32::Foundation::BOOL::from(prev), std::ptr::null())
                    .map_err(|e| format!("SetMute restore: {}", e))?;
                crate::commands::system::write_log_line(&format!(
                    "[RUST] [audio_mute] restored output (mute={})",
                    prev
                ));
                Ok(true)
            }),
            None => Ok(false),
        }
    }
    #[cfg(target_os = "macos")]
    {
        let saved = SAVED_OUTPUT_STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
        match saved {
            Some(state) => {
                let restored = crate::platform::macos::audio::restore_output(state);
                crate::commands::system::write_log_line(&format!(
                    "[RUST] [audio_mute] restored output (restored={} saved={:?})",
                    restored, state
                ));
                Ok(restored)
            }
            None => Ok(false),
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        Ok(false)
    }
}

// ─────────────────────────────────────────────────────────────────────────
// 麥克風靜音狀態查詢
// ─────────────────────────────────────────────────────────────────────────

/// 瀏覽器會給預設裝置名加上 Default/Communications 字首；Windows 端點友好名沒有。
/// 這裡只做名字規範化以定位本次實際開啟的端點，不參與靜音與否的判斷。
fn normalize_mic_label(label: &str) -> String {
    let trimmed = label.trim();
    let lower = trimmed.to_lowercase();
    let without_route_prefix = ["default", "communications", "默认值", "默认", "通信"]
        .iter()
        .find_map(|prefix| {
            let prefix_lower = prefix.to_lowercase();
            lower.starts_with(&prefix_lower).then(|| {
                trimmed[prefix.len()..].trim_start_matches(|c: char| {
                    c.is_whitespace() || matches!(c, '-' | '–' | '—' | ':')
                })
            })
        })
        .unwrap_or(trimmed);

    without_route_prefix
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(windows)]
unsafe fn get_device_friendly_name(
    device: &windows::Win32::Media::Audio::IMMDevice,
) -> Result<String, String> {
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    get_device_string_property(device, &PKEY_Device_FriendlyName)
}

#[cfg(windows)]
unsafe fn get_device_string_property(
    device: &windows::Win32::Media::Audio::IMMDevice,
    key: &windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY,
) -> Result<String, String> {
    use windows::Win32::System::Com::StructuredStorage::{
        PropVariantClear, PropVariantToStringAlloc,
    };
    use windows::Win32::System::Com::{CoTaskMemFree, STGM_READ};

    let store = device
        .OpenPropertyStore(STGM_READ)
        .map_err(|e| format!("OpenPropertyStore: {}", e))?;
    let mut value = store
        .GetValue(key)
        .map_err(|e| format!("GetValue({:?},{}): {}", key.fmtid, key.pid, e))?;

    let name_result = (|| -> Result<String, String> {
        let ptr = PropVariantToStringAlloc(&value)
            .map_err(|e| format!("PropVariantToStringAlloc: {}", e))?;
        let text = ptr
            .to_string()
            .map_err(|e| format!("property to_string: {}", e));
        CoTaskMemFree(Some(ptr.0.cast()));
        text
    })();
    let clear_result = PropVariantClear(&mut value)
        .map_err(|e| format!("PropVariantClear: {}", e));
    clear_result?;
    name_result
}

// ─────────────────────────────────────────────────────────────────────────
// 錄音端點清單：前端靠它判斷「系統預設」是不是虛擬裝置
// ─────────────────────────────────────────────────────────────────────────

/// 一個處於啟用狀態的錄音端點。
///
/// `name` 是 Windows 的端点友好名（`耳机式麦克风 (Plantronics Blackwire 5220 Series)`），
/// 與 Chromium 的 MediaDeviceInfo.label 去掉結尾 `(vid:pid)` 後相同，前端按它對應。
/// macOS 上是 CoreAudio 的裝置名，與 WebKit 的 label 相同（靜音查詢也按它對應）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureEndpoint {
    pub name: String,
    /// 端點所屬音訊裝置掛在哪條匯流排上：USB / HDAUDIO / INTELAUDIO / BTHENUM / ROOT ……
    /// macOS 上是 CoreAudio 的連線型別四字元碼：bltn / usb / blue / virt ……（只用於日誌和排查）
    pub enumerator: String,
    pub is_virtual: bool,
    pub is_default: bool,
    pub is_default_communications: bool,
}

/// 按匯流排判斷是不是虛擬裝置，**不按裝置名**。
///
/// 真實麥克風一定掛在某條硬體匯流排下（USB、HD Audio、英特爾智音、藍牙）；VB-Cable、
/// Voicemeeter 這類純軟體驅動沒有硬體可掛，由系統直接在根上列舉，匯流排名是 `ROOT`。
/// 本機實測：CABLE Output 是 ROOT，其餘五個真實麥克風是 USB / INTELAUDIO / HDAUDIO。
///
/// 刻意沒把 `SWD`（軟體裝置 API）算進來：它也是軟體列舉，但沒有實測樣本，
/// 誤判的代價不對稱 —— 把真麥克風當成虛擬會讓 SayIt 跳過使用者要用的裝置，
/// 漏判只是退回原來「跟隨系統預設」的行為。
/// 「立體聲混音」這類回採端點掛在真實音效卡下，這條判據認不出來；它預設是停用的，暫不處理。
#[cfg(any(windows, test))]
fn is_virtual_enumerator(enumerator: &str) -> bool {
    enumerator.trim().eq_ignore_ascii_case("ROOT")
}

/// macOS 的 kAudioDeviceTransportTypeVirtual（`virt`）。
#[cfg(any(target_os = "macos", test))]
const MAC_TRANSPORT_VIRTUAL: u32 = u32::from_be_bytes(*b"virt");

/// macOS 版的同一條判據：按 CoreAudio 報的連線型別，不按名字。
///
/// BlackHole、Loopback、Teams / Zoom 自帶的虛擬音訊裝置都報 `virt`；內建麥克風是 `bltn`、
/// USB 是 `usb `、藍牙是 `blue`、iPhone 連續互通是 `ccwl`。
/// 聚合裝置（`grup` / `fgrp`）刻意不算：它是使用者在「音訊 MIDI 設定」裡自己把真實裝置拼起來的，
/// 選它多半是有意為之；拿不到型別（0）也不算，理由同 Windows 那條（誤判比漏判代價大）。
#[cfg(any(target_os = "macos", test))]
fn is_virtual_transport(transport: u32) -> bool {
    transport == MAC_TRANSPORT_VIRTUAL
}

/// 把四字元碼轉成可讀文字，寫進 `enumerator` 方便看日誌。拿不到型別時是空串。
#[cfg(any(target_os = "macos", test))]
fn fourcc_text(code: u32) -> String {
    if code == 0 {
        return String::new();
    }
    code.to_be_bytes()
        .iter()
        .map(|b| if b.is_ascii_graphic() || *b == b' ' { *b as char } else { '?' })
        .collect::<String>()
        .trim_end()
        .to_string()
}

#[cfg(target_os = "macos")]
fn query_capture_endpoints_macos() -> Vec<CaptureEndpoint> {
    use crate::platform::macos::audio;
    let default_input = audio::default_device(true);
    audio::input_devices()
        .into_iter()
        .filter_map(|id| {
            let name = audio::device_name(id)?;
            let transport = audio::transport_type(id).unwrap_or(0);
            Some(CaptureEndpoint {
                name,
                enumerator: fourcc_text(transport),
                is_virtual: is_virtual_transport(transport),
                is_default: default_input == Some(id),
                // macOS 沒有單獨的「預設通訊裝置」
                is_default_communications: false,
            })
        })
        .collect()
}

#[cfg(windows)]
unsafe fn endpoint_id(device: &windows::Win32::Media::Audio::IMMDevice) -> Option<String> {
    use windows::Win32::System::Com::CoTaskMemFree;
    let ptr = device.GetId().ok()?;
    let id = ptr.to_string().ok();
    CoTaskMemFree(Some(ptr.0.cast()));
    id
}

#[cfg(windows)]
unsafe fn query_capture_endpoints() -> Result<Vec<CaptureEndpoint>, String> {
    use windows::core::GUID;
    use windows::Win32::Media::Audio::{
        eCapture, eCommunications, eConsole, IMMDeviceEnumerator, MMDeviceEnumerator,
        DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY;

    // PKEY_Device_EnumeratorName。windows crate 只以 DEVPROPKEY 形式匯出它，
    // IPropertyStore 要的是 PROPERTYKEY，兩者 fmtid/pid 相同，這裡直接寫出來。
    const PKEY_DEVICE_ENUMERATOR_NAME: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
        pid: 24,
    };

    let co_init_result = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    let need_co_uninit = co_init_result.is_ok();

    let result = (|| -> Result<Vec<CaptureEndpoint>, String> {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| format!("CoCreateInstance MMDeviceEnumerator: {}", e))?;
        let default_console = enumerator
            .GetDefaultAudioEndpoint(eCapture, eConsole)
            .ok()
            .and_then(|device| endpoint_id(&device));
        let default_comms = enumerator
            .GetDefaultAudioEndpoint(eCapture, eCommunications)
            .ok()
            .and_then(|device| endpoint_id(&device));

        let devices = enumerator
            .EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)
            .map_err(|e| format!("EnumAudioEndpoints(eCapture): {}", e))?;
        let count = devices
            .GetCount()
            .map_err(|e| format!("IMMDeviceCollection.GetCount: {}", e))?;

        let mut endpoints = Vec::new();
        for index in 0..count {
            let Ok(device) = devices.Item(index) else { continue };
            let Ok(name) = get_device_friendly_name(&device) else { continue };
            let enumerator_name =
                get_device_string_property(&device, &PKEY_DEVICE_ENUMERATOR_NAME).unwrap_or_default();
            let id = endpoint_id(&device);
            endpoints.push(CaptureEndpoint {
                name,
                is_virtual: is_virtual_enumerator(&enumerator_name),
                enumerator: enumerator_name,
                is_default: id.is_some() && id == default_console,
                is_default_communications: id.is_some() && id == default_comms,
            });
        }
        Ok(endpoints)
    })();

    if need_co_uninit {
        CoUninitialize();
    }

    result
}

/// 列出啟用中的錄音端點，標出哪個是系統預設、哪個是虛擬裝置。
/// 失敗或其他平台一律返回空表：前端拿到空表就照舊跟隨系統預設，不做任何替換。
/// [自訂] SC-62：同步命令在主執行緒執行，每次開錄都會卡住事件迴圈與浮動視窗。
/// 改在阻塞執行緒池查詢；查詢本身失敗或 panic 時照原本回傳的空值。
async fn off_main_thread<T: Send + 'static>(query: impl FnOnce() -> T + Send + 'static, fallback: T) -> T {
    tokio::task::spawn_blocking(query).await.unwrap_or(fallback)
}

#[tauri::command]
pub async fn list_capture_endpoints() -> Vec<CaptureEndpoint> {
    off_main_thread(capture_endpoints, Vec::new()).await
}

fn capture_endpoints() -> Vec<CaptureEndpoint> {
    #[cfg(windows)]
    unsafe {
        match query_capture_endpoints() {
            Ok(endpoints) => endpoints,
            Err(e) => {
                crate::commands::system::write_log_line(&format!(
                    "[RUST] [audio_devices] list_capture_endpoints failed: {}",
                    e
                ));
                Vec::new()
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        query_capture_endpoints_macos()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        Vec::new()
    }
}

/// 查詢採集裝置的靜音狀態。
///
/// 前端傳入 getUserMedia 實際開啟的 MediaStreamTrack.label。這裡列舉 Windows 採集端點，
/// 按規範化後的友好名唯一匹配，再讀取該端點真實的 IAudioEndpointVolume::GetMute 狀態。
/// 找不到或出現同名多個端點時返回 matched=false，絕不猜測靜音。
#[cfg(windows)]
unsafe fn query_mic_mute(label: Option<&str>) -> Result<MicMuteState, String> {
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{
        eCapture, eConsole, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
        DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };

    let co_init_result = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    let need_co_uninit = co_init_result.is_ok();

    let result = (|| -> Result<MicMuteState, String> {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| format!("CoCreateInstance MMDeviceEnumerator: {}", e))?;

        let device: IMMDevice = if let Some(target_label) =
            label.map(str::trim).filter(|s| !s.is_empty())
        {
            let target = normalize_mic_label(target_label);
            let devices = enumerator
                .EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)
                .map_err(|e| format!("EnumAudioEndpoints(eCapture): {}", e))?;
            let count = devices
                .GetCount()
                .map_err(|e| format!("IMMDeviceCollection.GetCount: {}", e))?;
            let mut matched: Option<IMMDevice> = None;

            for index in 0..count {
                let candidate = devices
                    .Item(index)
                    .map_err(|e| format!("IMMDeviceCollection.Item({}): {}", index, e))?;
                let friendly_name = match get_device_friendly_name(&candidate) {
                    Ok(name) => name,
                    Err(_) => continue,
                };
                if normalize_mic_label(&friendly_name) != target {
                    continue;
                }
                if matched.is_some() {
                    // 同名裝置無法可靠區分；寧可回退訊號檢測，也不查詢錯端點。
                    return Ok(MicMuteState {
                        matched: false,
                        muted: false,
                    });
                }
                matched = Some(candidate);
            }

            match matched {
                Some(device) => device,
                None => {
                    return Ok(MicMuteState {
                        matched: false,
                        muted: false,
                    })
                }
            }
        } else {
            enumerator
                .GetDefaultAudioEndpoint(eCapture, eConsole)
                .map_err(|e| format!("GetDefaultAudioEndpoint(eCapture): {}", e))?
        };

        let endpoint: IAudioEndpointVolume = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| format!("Activate IAudioEndpointVolume: {}", e))?;
        let muted = endpoint
            .GetMute()
            .map_err(|e| format!("GetMute: {}", e))?
            .as_bool();
        Ok(MicMuteState { matched: true, muted })
    })();

    if need_co_uninit {
        CoUninitialize();
    }

    result
}

/// 查詢麥克風是否被系統靜音。
/// `device_label`：本次 MediaStreamTrack 實際開啟的裝置名；傳 None/空才回退查詢系統預設麥克風。
/// 返回 `matched=false` 時表示無法可靠定位裝置（如選了特定裝置但同名多個），前端應退回訊號檢測。
#[tauri::command]
pub async fn get_mic_mute_state(device_label: Option<String>) -> MicMuteState {
    off_main_thread(move || mic_mute_state(device_label), MicMuteState { matched: false, muted: false }).await
}

fn mic_mute_state(device_label: Option<String>) -> MicMuteState {
    #[cfg(windows)]
    unsafe {
        match query_mic_mute(device_label.as_deref()) {
            Ok(state) => state,
            Err(e) => {
                crate::commands::system::write_log_line(&format!(
                    "[RUST] [audio_mute] get_mic_mute_state failed: {}",
                    e
                ));
                MicMuteState { matched: false, muted: false }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        query_mic_mute_macos(device_label.as_deref())
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = device_label;
        MicMuteState { matched: false, muted: false }
    }
}

/// macOS：按名字唯一匹配輸入裝置，讀輸入方向的 Mute 屬性。
/// 多數內建麥克風沒有 Mute 屬性，這時返回 matched=false，前端退回訊號檢測。
#[cfg(target_os = "macos")]
fn query_mic_mute_macos(label: Option<&str>) -> MicMuteState {
    use crate::platform::macos::audio;
    let not_matched = MicMuteState { matched: false, muted: false };
    let device = match label.map(str::trim).filter(|s| !s.is_empty()) {
        Some(target_label) => {
            let target = normalize_mic_label(target_label);
            let mut matched = None;
            for id in audio::input_devices() {
                let Some(name) = audio::device_name(id) else { continue };
                if normalize_mic_label(&name) != target {
                    continue;
                }
                if matched.is_some() {
                    // 同名裝置無法可靠區分；寧可回退訊號檢測，也不查詢錯裝置。
                    return not_matched;
                }
                matched = Some(id);
            }
            match matched {
                Some(id) => id,
                None => return not_matched,
            }
        }
        None => match audio::default_device(true) {
            Some(id) => id,
            None => return not_matched,
        },
    };
    match audio::device_muted(device, true) {
        Some(muted) => MicMuteState { matched: true, muted },
        None => not_matched,
    }
}

#[cfg(test)]
mod tests {
    use super::{fourcc_text, is_virtual_enumerator, is_virtual_transport, normalize_mic_label};

    #[test]
    fn device_queries_run_off_the_calling_thread() {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let caller = std::thread::current().id();
        let ran_on = runtime.block_on(super::off_main_thread(|| std::thread::current().id(), caller));
        assert_ne!(ran_on, caller);
        // 查詢 panic 時回傳原本的空值，不讓命令失敗。
        let failed = runtime.block_on(super::off_main_thread(|| -> Vec<u8> { panic!("query failed") }, vec![7]));
        assert_eq!(failed, [7]);
    }

    #[test]
    fn only_virt_transport_counts_as_virtual_on_macos() {
        let code = |s: &[u8; 4]| u32::from_be_bytes(*s);
        assert!(is_virtual_transport(code(b"virt")));
        // 內建、USB、藍牙、連續互通、聚合裝置、拿不到型別：都不算
        for real in [b"bltn", b"usb ", b"blue", b"ccwl", b"grup", b"fgrp"] {
            assert!(!is_virtual_transport(code(real)));
        }
        assert!(!is_virtual_transport(0));
        assert_eq!(fourcc_text(code(b"usb ")), "usb");
        assert_eq!(fourcc_text(code(b"virt")), "virt");
        assert_eq!(fourcc_text(0), "");
    }

    #[test]
    fn only_root_enumerated_endpoints_count_as_virtual() {
        // 本機實測值：CABLE Output 是 ROOT，真實麥克風是 USB / INTELAUDIO / HDAUDIO。
        assert!(is_virtual_enumerator("ROOT"));
        assert!(is_virtual_enumerator(" root "));
        for real in ["USB", "INTELAUDIO", "HDAUDIO", "BTHENUM", "BTHHFENUM", ""] {
            assert!(!is_virtual_enumerator(real), "{real} must not be treated as virtual");
        }
        // SWD 刻意不算（見 is_virtual_enumerator 的註釋）
        assert!(!is_virtual_enumerator("SWD"));
    }

    #[test]
    fn browser_route_prefix_does_not_change_endpoint_identity() {
        assert_eq!(
            normalize_mic_label("Default - Headset Microphone (Plantronics Blackwire 5220 Series)"),
            normalize_mic_label("Headset Microphone (Plantronics Blackwire 5220 Series)")
        );
        assert_eq!(
            normalize_mic_label("Communications: USB Microphone"),
            "usb microphone"
        );
    }

    #[test]
    fn ordinary_device_name_is_preserved_for_matching() {
        assert_eq!(normalize_mic_label("  Studio   Mic  "), "studio mic");
    }
}
