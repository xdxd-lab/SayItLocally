// 本地 ASR 推理（GGUF / ggml）— 走 transcribe.cpp 的 Rust 繫結 transcribe-cpp。
//
// 與舊的 sherpa-onnx 路徑（local_asr.rs）相比：
//   - 模型是 GGUF 塊量化（Q8_0 等），kernel 直接吃量化權重，不像 ORT 的 INT8
//     動態量化每步都要 quantize/dequantize —— 自迴歸模型（Qwen3-ASR）差一個量級。
//   - 計算後端是執行時註冊的：有 GPU 模組就能用 GPU，沒有就自動 CPU。
//   - families 自報 max_audio_ms（Qwen3 約 87 分鐘），但「吃得下」不等於「該一次喂」：
//     整段長錄音一次推理會讓整合顯示卡單個任務超過 Windows 看門狗的 2 秒而被重置。
//     所以 local_asr 先按停頓切成 ≤30 s 的塊再調這裡（見 MAX_CHUNK_SEC）；
//     這裡的超限分段只是兜底。
//
// 執行緒模型：Session 是 Send 但不是 Sync，run 要 &mut self；同一個 Model 同時
// 只允許一個 run 在飛（庫內部用 per-model 鎖保證）。所以這裡用一個全域性
// Mutex<Option<Loaded>> 常駐，推理時拿 &mut。

#[cfg(test)]
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use transcribe_cpp::{
    Backend, DeviceType, Feature, Itn, Model, ModelOptions, Pnc, RunOptions, Session,
    SessionOptions,
};
#[cfg(test)]
use transcribe_cpp::TimestampKind;

// [自訂] S88：正式載入由 registry 核對 catalog；未登錄的探針只存在於測試。
use super::registry;
#[cfg(test)]
use super::downloader::model_dir;

/// 已載入的引擎。Session 內部持有 Model 的 Arc，所以重複推理不會重新載入權重。
///
/// 快取 key 是 (model_id, accelerator, gpu_device)：這三個變了才需要真的重建模型。
/// language 不在 key 裡 —— 它只是 RunOptions 的執行時引數，每次轉寫傳入即可，
/// 切換識別語言不應該付一次"過載 + 預熱"（1.7B 約 9 s）的錢。
pub(crate) struct Loaded {
    model_id: String,
    accelerator: String,
    /// 使用者選的顯示卡標識（`GgufDevice::id`，空 = 自動）。
    ///
    /// key 裡存的是**設定值**而不是解析出來的 registry 索引：索引會隨驅動更新
    /// 變（上游明確說明），拿它當 key 會讓同一份設定在列舉抖動時反覆過載模型。
    gpu_device: String,
    /// 實際繫結的後端字串（"cpu" / "vulkan" / …），用於診斷頁顯示。
    backend: String,
    /// 實際繫結的裝置描述（如 "NVIDIA GeForce RTX 4060"）。
    ///
    /// 必須如實回報，不能拿"使用者選了哪張"頂替：`gpu_device` 索引 0 是上游的
    /// 自動哨兵，選中 registry 索引 0 那張卡時我們傳的是 0，最終由庫的探測順序
    /// 決定用哪張（獨顯優先）。絕大多數情況兩者一致，但集顯排在索引 0、機器
    /// 又有獨顯時就會不一致 —— 那時介面必須顯示真實結果，否則使用者永遠查不出
    /// "我設了卻沒生效"。
    device: Option<String>,
    /// 本 session 能接受的最長音訊（毫秒）。0 = 無實際上限。
    max_audio_ms: i64,
    /// 這個模型族是否支援 ITN / PNC 的執行時開關。載入時探一次，之後按結果決定
    /// 要不要在 RunOptions 裡請求 —— 對不支援的族請求會讓庫每次都打一條 WARN。
    supports_itn: bool,
    supports_pnc: bool,
    /// 模型自報的語種碼。載入時抄一份，因為**各族的碼粒度不一樣**，介面上的
    /// `localAsr.language`（只有 auto/zh/en/ja/ko）不能直接透傳，得按這份清單
    /// 解析成模型真正認識的那個串。詳見 `resolve_language`。
    languages: Vec<String>,
    session: Session,
}

static CACHE: Mutex<Option<Loaded>> = Mutex::new(None);

/// 引擎狀態的輕量映象：只放"診斷 / 狀態查詢要讀的那幾個欄位"，與 CACHE 分成兩把鎖。
///
/// 為什麼必須分開：`ensure_loaded` 在**整個"卸舊 + 載入 + 預熱"期間**都持著 CACHE
/// 鎖（1.7B 約 9 s，冷盤 / 弱 GPU / 首次編譯 Vulkan pipeline 會更久）。
/// 而 `gguf_asr_diagnostics` 曾經是同步 `#[tauri::command]` —— Tauri 把同步命令放在
/// 主執行緒執行，主執行緒又是 UI 事件迴圈，於是"進設定頁選個模型"會讓整個視窗假死到
/// 載入結束，使用者只能強殺程序（0.1.4 的使用者反饋就是這個）。
///
/// 所以規矩是：**任何"只是想知道當前狀態"的讀取都走這把小鎖，絕不排在 CACHE 後面。**
/// 這把鎖只在賦值/克隆的瞬間持有，不會跨任何耗時操作。
static STATUS: Mutex<EngineStatus> = Mutex::new(EngineStatus {
    loading_model: None,
    backend: None,
    device: None,
});

struct EngineStatus {
    /// 正在載入中的模型 id；None = 當前沒有載入在進行。
    loading_model: Option<String>,
    /// 已載入引擎實際繫結的後端字串（"cpu" / "vulkan" / …）；None = 未載入。
    backend: Option<String>,
    /// 實際繫結的裝置描述；None = 未載入或庫沒報。見 `Loaded::device`。
    device: Option<String>,
}

fn mark_loading(model_id: &str) {
    if let Ok(mut status) = STATUS.lock() {
        status.loading_model = Some(model_id.to_string());
        // 舊引擎馬上就要被丟棄，先把 backend 清空：否則過載期間診斷頁會報一個
        // 已經不存在的後端。
        status.backend = None;
        status.device = None;
    }
}

fn mark_loaded(backend: &str, device: Option<&str>) {
    if let Ok(mut status) = STATUS.lock() {
        status.backend = Some(backend.to_string());
        status.device = device.map(|d| d.to_string());
    }
}

fn mark_unloaded() {
    if let Ok(mut status) = STATUS.lock() {
        status.backend = None;
        status.device = None;
    }
}

/// 離開 `ensure_loaded` 時一定清掉"載入中"標記。中途任何一個 `?` 提前返回
/// （沒下載 / 載入失敗 / 建 session 失敗）都不能讓介面永遠停在"正在載入"。
struct LoadingGuard;

impl Drop for LoadingGuard {
    fn drop(&mut self) {
        if let Ok(mut status) = STATUS.lock() {
            status.loading_model = None;
        }
    }
}

/// 後端只需註冊一次。dynamic-backends 構建下，不先註冊就載入模型會直接報
/// TRANSCRIBE_ERR_BACKEND，所以這一步是硬前置。
static INIT: std::sync::Once = std::sync::Once::new();

/// 註冊計算後端 + 把 ggml/native 的日誌接到 log facade。
///
/// **懶呼叫，絕不放回啟動路徑。** 這一步會 dlopen exe 旁邊所有 ggml 模組，其中
/// `ggml-vulkan.dll`（70 MB）一被載入就立刻 `vk::createInstance()` 建出真實的 Vulkan
/// 上下文：實測白佔約 36 MB 共享視訊記憶體、讓程序帶著 `engtype_compute` 出現在工作管理員的
/// GPU 程序列表裡，本身還要 80~520 ms。雲 API / 伺服器模式一個模型都不載入，付這筆錢
/// 收益為零（0.1.8 前的實際情況，使用者反饋"即便使用雲 API 模式也持續佔用 GPU"）。
/// 而且上游標註 idempotent 但 NOT retryable，程序內沒有反註冊路徑 —— 註冊了就撤不回來。
///
/// 生產呼叫點只有兩個，都在真正需要本地引擎的那一刻：
/// - `ensure_loaded()` —— 所有模型載入的必經之路（`preload` / `transcribe` 都走它）
/// - `describe_devices()` —— 本地模式的設定頁與使用者主動開啟的診斷頁
///
/// 目前只掃 exe 旁邊的模組目錄（build.rs 會把 transcribe.dll + ggml-*.dll 放那兒）。
/// 注意 `init_backends(dir)` 只掃**一個**目錄，不是"追加一個目錄"：所以將來做
/// GPU 加速包時，不能只把 ggml-vulkan.dll 單獨放到另一個目錄再指過去（那樣 CPU
/// 模組就不在掃描範圍內了）。屆時的做法是：首次執行把 exe 旁的 CPU 模組拷到
/// 使用者可寫目錄，加速包也下到同一個目錄，然後統一 init_backends(那個目錄)。
pub fn init_backends() {
    INIT.call_once(|| {
        transcribe_cpp::init_logging();
        match transcribe_cpp::init_backends_default() {
            Ok(()) => log::info!("transcribe backends registered"),
            Err(e) => log::error!("transcribe backends init failed: {}", e),
        }

        let devices = transcribe_cpp::devices();
        log::info!(
            "transcribe compute devices ({}): [{}]",
            devices.len(),
            devices
                .iter()
                .map(|d| format!("{}:{}", d.kind, d.name))
                .collect::<Vec<_>>()
                .join(", ")
        );
    });
}

/// [自訂] S88：測試只在 catalog 權重存在且通過驗證時啟用。
/// 只給測試用：生產路徑上"沒下載"是由 `ensure_loaded` 報錯表達的，不需要預檢。
#[cfg(test)]
pub fn model_is_downloaded(model_id: &str) -> bool {
    registry::verified_model_path(model_id).is_ok()
}

/// 一個已註冊的計算裝置，給前端（診斷頁 / 設定頁）展示用。
#[derive(Debug, Clone, serde::Serialize)]
pub struct GgufDevice {
    /// "cpu" / "vulkan" / …
    pub kind: String,
    pub name: String,
    /// 裝置可用記憶體（MB）。CPU 報的是系統記憶體，GPU 報的是視訊記憶體。
    pub memory_mb: u64,
    /// 設定裡用來記住"選了哪張卡"的穩定標識。
    ///
    /// 優先用後端報的 `device_id`（PCI 裝置對應的匯流排 id），拿不到就回落到
    /// `kind:name`。**絕不能存 registry 索引** —— 上游明確說索引會隨驅動更新
    /// 或換機器變動，存索引等於讓使用者的選擇在某次驅動更新後靜默指向另一張卡。
    pub id: String,
    /// ggml registry 索引，也就是 `ModelOptions::gpu_device` 的取值空間。
    /// 帶給前端只為排序與除錯展示，不作為持久化標識。
    pub index: usize,
    /// 是不是 GPU/IGPU（只有這兩類能被顯式指定）。CPU 與 BLAS 之類的 ACCEL 裝置
    /// 傳給 `gpu_device` 會被上游判成 INVALID_ARG，整次載入失敗。
    pub is_gpu: bool,
}

/// 把設定裡存的顯示卡標識解析成 `ModelOptions::gpu_device`。
///
/// 返回值是 ggml registry 索引，0 = 交給庫自動挑（獨顯優先，各檔內按註冊順序）。
///
/// 三條硬規則，違反任意一條上游都會直接 `TRANSCRIBE_ERR_INVALID_ARG`、
/// 讓整次模型載入失敗（不是回落，是報錯）：
/// 1. 請求 CPU 後端時必須是 0 —— 沒有 GPU 可選。
/// 2. 只能指向 GPU/IGPU 裝置。
/// 3. 必須在 `[0, device_count)` 範圍內。
///
/// 還有一條不是錯誤但會讓結果與使用者預期不符：**索引 0 是"自動"哨兵，沒辦法
/// 顯式選中 registry 索引 0 那張卡**。實際影響很小，因為自動探測本來就是獨顯
/// 優先、同檔按註冊順序，所以"第一張獨顯"就是自動的結果；真正落空的只有
/// "索引 0 是集顯、機器還有獨顯、使用者偏要用集顯"這一種組合。那種情況下只能
/// 靠 `GGML_VK_VISIBLE_DEVICES` 在程序啟動前限制可見裝置。
///
/// 所以載入完成後一律用 `Model::device()` 把**實際**繫結的裝置回報給介面，
/// 不拿使用者的選擇頂替 —— 見 `Loaded::device`。
fn resolve_gpu_device(selected: &str, accelerator: &str) -> i32 {
    if selected.is_empty() || selected == "auto" {
        return 0;
    }
    pick_gpu_device_index(&describe_devices(), selected, accelerator)
}

/// `resolve_gpu_device` 的決策部分，抽出來是為了能不碰真實硬體地測。
/// 所有的回落分支都在這裡，列舉裝置那一步留在呼叫方。
fn pick_gpu_device_index(devices: &[GgufDevice], selected: &str, accelerator: &str) -> i32 {
    if selected.is_empty() || selected == "auto" {
        return 0;
    }
    if accelerator == "cpu" {
        log::warn!(
            "A specific GPU ({}) is selected but the compute backend is set to CPU; ignoring the GPU selection",
            selected
        );
        return 0;
    }
    let Some(dev) = devices.iter().find(|d| d.id == selected) else {
        // 換了顯示卡、拔了外接 GPU、驅動沒起來都會走到這裡。回落到自動而不是報錯：
        // 使用者的本意是"用本地識別"，不是"用這張卡，否則別識別"。
        log::warn!(
            "The selected GPU ({}) is not among the registered compute devices; falling back to automatic selection",
            selected
        );
        return 0;
    };
    if !dev.is_gpu {
        log::warn!(
            "The selected compute device ({}) is not a GPU; falling back to automatic selection",
            dev.name
        );
        return 0;
    }
    if dev.index == 0 {
        // 不是失敗：0 就是自動，而自動的探測順序本來就會先挑這一檔裡的獨顯。
        log::info!(
            "The selected GPU ({}) sits at registry index 0, which is the automatic sentinel; the library will pick it by probe order",
            dev.name
        );
        return 0;
    }
    log::info!("Requesting GPU device {} ({})", dev.index, dev.name);
    dev.index as i32
}

/// 診斷資訊：已註冊的計算裝置 + 當前繫結的後端。
/// 用來確認到底跑在 cpu 還是 vulkan 上，避免"以為開了 GPU 其實回落了"。
#[derive(Debug, Clone, serde::Serialize)]
pub struct GgufDiagnostics {
    pub devices: Vec<GgufDevice>,
    pub current_backend: Option<String>,
    /// 實際繫結的裝置描述。多顯示卡機器上"選了哪張"和"真的用了哪張"可能不同
    /// （見 `resolve_gpu_device`），介面顯示的必須是這個。
    pub current_device: Option<String>,
    /// 正在載入中的模型 id。非 None 時 `current_backend` 一定是 None ——
    /// 介面這時該說"正在載入"，而不是"模型未載入"。
    pub loading_model: Option<String>,
    pub native_version: String,
    /// 當前程序的工作集（MB）。核顯機器上模型權重也算在這裡面，所以這個數
    /// 就是使用者在工作管理員看到的那個數。
    pub process_memory_mb: u64,
}

/// **必須是 async。** Tauri 把同步命令放在主執行緒執行，而主執行緒就是 UI 事件迴圈；
/// 這裡要做 native FFI（`devices()` 列舉計算裝置、`version()`），在主執行緒上跑一旦
/// 慢下來視窗就"無響應"。改成 async + spawn_blocking 後，即使底層卡住也只卡這一個
/// 後台任務。狀態欄位讀的是 STATUS 小鎖，不會等模型載入。
#[tauri::command]
pub async fn gguf_asr_diagnostics() -> Result<GgufDiagnostics, String> {
    tokio::task::spawn_blocking(|| GgufDiagnostics {
        devices: describe_devices(),
        current_backend: current_backend(),
        current_device: current_device(),
        loading_model: loading_model(),
        native_version: transcribe_cpp::version(),
        process_memory_mb: process_memory_mb(),
    })
    .await
    .map_err(|e| format!("Failed to read local engine state: {}", e))
}

/// 當前程序的工作集，MB。用來回答"這個模型到底吃多少記憶體"——權重體積不等於
/// 記憶體佔用（還有 KV cache、計算緩衝、mel 緩衝），實測比估算靠譜。
pub fn process_memory_mb() -> u64 {
    #[cfg(windows)]
    {
        use windows::Win32::System::ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
        };
        use windows::Win32::System::Threading::GetCurrentProcess;
        let mut counters = PROCESS_MEMORY_COUNTERS {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            ..Default::default()
        };
        let ok = unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut counters,
                std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            )
        };
        if ok.is_ok() {
            return (counters.WorkingSetSize / (1024 * 1024)) as u64;
        }
        0
    }
    // macOS：phys_footprint（與「活動監視器」的「記憶體」列一致）
    #[cfg(target_os = "macos")]
    {
        crate::platform::macos::sysinfo::process_memory_mb()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        0
    }
}

/// 列出已註冊的計算裝置，給診斷頁 / 設定頁用。
///
/// 自己負責註冊後端（啟動路徑上已經不做了），否則拿到的永遠是空列表。
///
/// 這不會讓雲 / 伺服器模式的使用者白付 Vulkan 初始化：本函式只有兩個呼叫點，
/// `LocalModeSection` 的 GPU 摘要（`VoiceEnginePage` 裡由 `workMode === 'local'`
/// 門住，只在本地模式渲染）和使用者主動開啟的診斷頁。兩者都意味著使用者正在檢視
/// 本地引擎的硬體資訊，這時列舉裝置就是他要的結果。
pub fn describe_devices() -> Vec<GgufDevice> {
    init_backends();
    transcribe_cpp::devices()
        .into_iter()
        .enumerate()
        .map(|(i, d)| {
            let name = if d.description.is_empty() {
                d.name.clone()
            } else {
                d.description.clone()
            };
            GgufDevice {
                kind: d.kind.to_string(),
                // index 由庫給（列舉時填的就是 registry 索引）；它是 Option，只有
                // `Model::device()` 取回的裝置才會是 None，而這裡一定是列舉來的。
                // 回落到 enumerate 的序號只是防禦，兩者本來就相等。
                index: d.index.unwrap_or(i),
                is_gpu: matches!(d.device_type, DeviceType::Gpu | DeviceType::Igpu),
                id: d
                    .device_id
                    .clone()
                    .unwrap_or_else(|| format!("{}:{}", d.kind, name)),
                name,
                memory_mb: d.memory_total / (1024 * 1024),
            }
        })
        .collect()
}

/// [自訂] S88 / SC-35：僅供忽略的候選架構探針使用，從未登錄的目錄取最大 GGUF。
/// 正式載入由 `registry::verified_model_path` 驗證 catalog 指定的權重。
#[cfg(test)]
fn find_probe_gguf(dir: &Path) -> Result<PathBuf, String> {
    if !dir.exists() {
        return Err("The model has not been downloaded".into());
    }
    let mut hits: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("Failed to read model directory: {}", e))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("gguf"))
        })
        .collect();
    match hits.len() {
        0 => Err("The model directory contains no .gguf file".into()),
        1 => Ok(hits.remove(0)),
        _ => {
            hits.sort_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0));
            let pick = hits.pop().unwrap();
            log::warn!(
                "The model directory contains multiple .gguf files; selected the largest: {}",
                pick.file_name().unwrap_or_default().to_string_lossy()
            );
            Ok(pick)
        }
    }
}

/// 選擇後端。Auto = 庫自己挑最好的已註冊裝置，挑不到 GPU 就用 CPU。
fn resolve_backend(pref: &str) -> Backend {
    match pref {
        "cpu" => Backend::Cpu,
        "gpu" => {
            // 只有真的註冊了 GPU 裝置才請求它，否則退回 Auto，避免載入直接失敗。
            // macOS 上是 Metal（Windows 構建沒有編入 Metal，backend_available 恆為 false）
            for b in [Backend::Cuda, Backend::Vulkan, Backend::Metal] {
                if transcribe_cpp::backend_available(b) {
                    return b;
                }
            }
            log::warn!("GPU was requested but no GPU backend is available; falling back to Auto");
            Backend::Auto
        }
        _ => Backend::Auto,
    }
}

/// 載入模型（若 key 未變則複用）。`accelerator` 取 "auto" | "cpu" | "gpu"，
/// `gpu_device` 是 `GgufDevice::id`（空 = 自動挑）。
/// 換加速器或換顯示卡都要重建：裝置是 Model::load_with 時繫結的，之後改不了。
fn ensure_loaded(model_id: &str, accelerator: &str, gpu_device: &str) -> Result<(), String> {
    // 後端註冊的**唯一生產入口之一**（另一個是 describe_devices）。啟動路徑上不再註冊，
    // 所以這裡必須自己保證前置條件：dynamic-backends 構建下不先註冊就 Model::load_with
    // 會直接報 TRANSCRIBE_ERR_BACKEND，而下面 resolve_backend 裡的 backend_available()
    // 也要靠它才能看到 GPU。
    //
    // 放在 CACHE.lock() **之前**：首次註冊要 80~520 ms，而 CACHE 鎖會被整個
    // "卸舊 + 載入 + 預熱"過程持有（見 STATUS 的說明），不該再往裡塞耗時操作。
    // init_backends 內部是 Once，重複呼叫無代價，併發呼叫由 Once 自己序列。
    init_backends();

    let mut cache = CACHE.lock().map_err(|e| format!("Failed to acquire model cache lock: {}", e))?;

    if let Some(ref c) = *cache {
        if c.model_id == model_id && c.accelerator == accelerator && c.gpu_device == gpu_device {
            // 與空閒解除安裝共用同一把鎖更新時間，保證守護執行緒拿到鎖後不會
            // 把剛被複用、即將開始推理的模型當成過期模型解除安裝。
            touch_activity();
            return Ok(());
        }
    }

    // 從這裡開始就算"載入中"了：卸舊引擎本身也要時間，而這段時間裡狀態查詢
    // 應該說"正在載入"，不是"未載入"。
    mark_loading(model_id);
    let _loading_guard = LoadingGuard;

    // 先釋放舊引擎再建新的，避免同時駐留兩份權重（GGUF 動輒幾百 MB ~ 數 GB）。
    *cache = None;

    // [自訂] S88：預載、辨識與重新載入均先驗證，再把檔案交給本機引擎。
    let path = registry::verified_model_path(model_id)?;
    log::info!("Loading GGUF ASR model: {} ({})", model_id, path.display());
    let start = Instant::now();

    let options = ModelOptions {
        backend: resolve_backend(accelerator),
        // 0 = 自動/首個匹配；非 0 = 指定的 ggml registry 索引。見 resolve_gpu_device。
        gpu_device: resolve_gpu_device(gpu_device, accelerator),
    };
    let model = Model::load_with(&path, &options)
        .map_err(|e| format!("Failed to load model ({}): {}", model_id, e))?;

    let backend = model.backend();
    // 實際繫結到哪個裝置。失敗不影響功能，只是診斷少一條資訊 —— 但多顯示卡機器上
    // 這是回答"我指定的那張卡到底用上了沒有"的唯一依據，所以失敗也要留日誌。
    let device = match model.device() {
        Ok(d) => Some(if d.description.is_empty() {
            d.name
        } else {
            d.description
        }),
        Err(e) => {
            log::warn!("Could not read the bound compute device: {}", e);
            None
        }
    };
    let caps = model.capabilities();
    let arch = model.arch();
    let languages = caps.languages.clone();
    let n_langs = languages.len();
    let supports_itn = model.supports(Feature::Itn);
    let supports_pnc = model.supports(Feature::Pnc);
    let session = model
        .session_with(&SessionOptions::default())
        .map_err(|e| format!("Failed to create session ({}): {}", model_id, e))?;

    let max_audio_ms = session
        .limits()
        .map(|l| l.effective_max_audio_ms)
        .unwrap_or(caps.max_audio_ms);

    let mut entry = Loaded {
        model_id: model_id.to_string(),
        accelerator: accelerator.to_string(),
        gpu_device: gpu_device.to_string(),
        backend,
        device,
        max_audio_ms,
        supports_itn,
        supports_pnc,
        languages,
        session,
    };
    let load_ms = start.elapsed().as_millis();

    // 預熱：見 warmup() 的說明。必須在放進快取之前做完，否則併發的第一次轉寫會
    // 搶在預熱之前跑，白付一次 pipeline 編譯。
    let warmup_ms = warmup(&mut entry);

    log::info!(
        "GGUF ASR ready in {}ms (load {}ms + warmup {}ms): {} arch={} backend={} device={} langs={} max_audio_ms={} itn={} pnc={}",
        start.elapsed().as_millis(),
        load_ms,
        warmup_ms,
        model_id,
        arch,
        entry.backend,
        entry.device.as_deref().unwrap_or("unknown"),
        n_langs,
        max_audio_ms,
        supports_itn,
        supports_pnc
    );

    mark_loaded(&entry.backend, entry.device.as_deref());
    *cache = Some(entry);
    // 必須在釋放 CACHE 鎖之前重新整理；否則空閒守護執行緒可能在這裡插入並解除安裝
    // 剛載入完成、馬上要用於推理的模型。
    touch_activity();
    Ok(())
}

/// 一小段真實中文語音，編進二進位制裡專門用於預熱（97 KB，就是 benchmark 用的那條）。
/// 用真實語音而不是靜音：Qwen3 這類自迴歸模型在靜音上可能一個 token 都不生成，
/// 那樣 decoder 側的 pipeline 就建不起來，預熱等於沒做。
const WARMUP_WAV: &[u8] = include_bytes!("../../resources/test_zh.wav");
/// 預熱只用前這麼多秒，夠把 kernel 都摸一遍就行，不必跑完整段。
const WARMUP_SEC: usize = 2;

/// 載入完權重後跑一次推理，把計算後端的 compute pipeline 建起來。
///
/// 為什麼必須做：ggml 的 Vulkan pipeline 是**首次推理時才惰性編譯**的，不在模型
/// 載入階段。不預熱的話這筆一次性開銷會落在使用者的第一次口述上 —— 實測 1.7B 在
/// Vulkan 上第一次解碼 8.7 s、第二次同樣長度只要 2.6 s。
///
/// 返回耗時（毫秒）。失敗不算錯誤：預熱只是最佳化，失敗了頂多第一次慢一點。
fn warmup(entry: &mut Loaded) -> u128 {
    let t0 = Instant::now();
    let pcm: Vec<f32> = WARMUP_WAV
        .get(44..) // 跳過 WAV header
        .unwrap_or(&[])
        .chunks_exact(2)
        .take(WARMUP_SEC * 16000)
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
        .collect();
    if pcm.is_empty() {
        return 0;
    }
    // 預熱只為建 pipeline，語言無所謂，用自動檢測即可
    let opts = run_options(
        "auto",
        &entry.languages,
        entry.supports_itn,
        entry.supports_pnc,
    );
    if let Err(e) = entry.session.run(&pcm, &opts) {
        log::warn!("Warm-up inference failed; functionality is unaffected, but the first run will be slower: {}", e);
    }
    t0.elapsed().as_millis()
}

/// 把介面上的語種設定解析成**當前模型真正接受的那個串**。
///
/// 為什麼不能直接透傳：各族自報的語種碼粒度不一樣。SenseVoice / Fun-ASR /
/// Qwen3-ASR 報的是裸碼（`zh` / `en` / `ja`），而 parakeet 族的 nemotron-3.5 報的
/// 是帶地區的 locale（`en-US` / `en-GB` / `zh-CN` / `ja-JP` …）並且**只認帶地區的
/// 形式** —— 把 `en` 傳給它會直接 `unsupported language (status 10)`，整次轉寫失敗。
/// 而 `localAsr.language` 的取值只有 `auto|zh|en|ja|ko`，永遠給不出 `en-US`。
///
/// 三級降解：精確命中 → 同語言的第一個 locale → 放棄。
///
/// 最後一級刻意回落到 `None`（自動檢測）而不是把原串硬塞過去：模型壓根沒廣告
/// 這個語種時（比如給純英文的 parakeet 選中文），讓它自己判最多是結果不理想，
/// 硬塞則是穩定報錯。這一條也順手修掉了老行為裡的一個坑 —— Fun-ASR 只支援
/// zh/en/ja，之前選「韓語」會把 `ko` 塞給它。
fn resolve_language(requested: &str, supported: &[String]) -> Option<String> {
    if matches!(requested, "" | "auto") {
        return None;
    }
    // 語種無關的模型（caps.languages 為空）沒有清單可比，按原樣傳。
    if supported.is_empty() {
        return Some(requested.to_string());
    }
    if let Some(hit) = supported.iter().find(|l| l.eq_ignore_ascii_case(requested)) {
        return Some(hit.clone());
    }
    let prefix = format!("{}-", requested.to_ascii_lowercase());
    if let Some(hit) = supported
        .iter()
        .find(|l| l.to_ascii_lowercase().starts_with(&prefix))
    {
        return Some(hit.clone());
    }
    None
}

/// 構造執行引數。
///
/// ITN 只在模型族真支援時才請求：SenseVoice 的標點就掛在 ITN 上（不開會走
/// `<|woitn|>` 分支、輸出沒有標點），而 Qwen3-ASR 不支援這個開關、標點是模型
/// 固有行為。對不支援的族請求 On 不會出錯，但庫每次轉寫都會打一條
/// "does not support itn control" 的 WARN —— 按載入時探到的能力來決定，
/// 日誌才幹淨。PNC 同理（parakeet 族兩個也都不支援，標點是固有行為）。
fn run_options(
    language: &str,
    supported_languages: &[String],
    supports_itn: bool,
    supports_pnc: bool,
) -> RunOptions {
    RunOptions {
        language: resolve_language(language, supported_languages),
        itn: if supports_itn { Itn::On } else { Itn::Default },
        pnc: if supports_pnc { Pnc::On } else { Pnc::Default },
        ..Default::default()
    }
}

/// 用常駐 session 轉寫。超過 session 上限的音訊按上限切段順序解碼。
/// `language` 是每次轉寫傳入的執行時引數，不影響模型快取。
fn transcribe_with_cache(
    samples: &[f32],
    sample_rate: usize,
    language: &str,
) -> Result<String, String> {
    let mut cache = CACHE.lock().map_err(|e| format!("Failed to acquire model cache lock: {}", e))?;
    let entry = cache.as_mut().ok_or("Model is not loaded")?;
    // 守衛會在所有成功/錯誤返回路徑上、釋放 CACHE 鎖之前重新整理最後活動時間。
    let _activity_guard = ActivityGuard;
    touch_activity();
    let opts = run_options(
        language,
        &entry.languages,
        entry.supports_itn,
        entry.supports_pnc,
    );

    let limit = if entry.max_audio_ms > 0 {
        (entry.max_audio_ms as usize / 1000) * sample_rate
    } else {
        usize::MAX
    };

    if samples.len() <= limit {
        return entry
            .session
            .run(samples, &opts)
            .map(|t| t.text.trim().to_string())
            .map_err(|e| format!("Inference failed: {}", e));
    }

    // 極長音訊（超過模型上下文）才走分段。日常口述不會走到這裡。
    log::warn!(
        "Audio length {:.1}s exceeds the model limit {:.1}s; decoding in segments",
        samples.len() as f64 / sample_rate as f64,
        limit as f64 / sample_rate as f64
    );
    let mut out = String::new();
    for chunk in samples.chunks(limit) {
        let text = entry
            .session
            .run(chunk, &opts)
            .map_err(|e| format!("Inference failed: {}", e))?;
        append_segment_text(&mut out, &text.text);
    }
    Ok(out)
}

/// 把分段識別出的一段文字接到結果後面。前一段沒以標點收尾時補一個空格，
/// 免得兩段英文粘成一個詞。超上限分段和 local_asr 按停頓切塊共用這一份。
pub(crate) fn append_segment_text(out: &mut String, text: &str) {
    let t = text.trim();
    if t.is_empty() {
        return;
    }
    if !out.is_empty() && !out.ends_with(|c: char| "，。！？、,.!?".contains(c)) {
        out.push(' ');
    }
    out.push_str(t);
}

// ── 對外介面（被 local_asr.rs 的命令層分派呼叫）──

/// 顯示卡在本程序裡出過後端錯誤，之後一律改用 CPU。
///
/// 典型來源是 Windows 顯示卡看門狗把顯示卡重置了（LiveKernelEvent 141，Vulkan 報
/// DeviceLost）。重置之後本程序拿到的 Vulkan 裝置就廢了，ggml 不會重建它：
/// 2026-10-19 實測，之後不管識別多短、換哪個模型，載入和推理都立刻報
/// `backend error (status 8)`，只能重啟 SayIt。改用 CPU 至少還能用，只是慢。
/// 只記在記憶體裡：重啟後顯示卡是好的，下次照常用 GPU。
static GPU_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn effective_accelerator(requested: &str) -> &str {
    if GPU_FAILED.load(std::sync::atomic::Ordering::Relaxed) {
        "cpu"
    } else {
        requested
    }
}

/// 庫把顯示卡層面的失敗統一報成 `backend error`（status 8）。錯誤串裡看不到
/// DeviceLost 本身（那句只出現在庫自己的日誌裡），所以按這個類別判。
fn is_backend_error(error: &str) -> bool {
    error.contains("backend error")
}

/// 剛才那次是不是跑在顯示卡上（或者本來要上顯示卡、載入就失敗了）。
fn was_on_gpu(accelerator: &str) -> bool {
    match current_backend() {
        Some(backend) => !backend.to_lowercase().contains("cpu"),
        None => accelerator != "cpu",
    }
}

/// 在顯示卡上出了後端錯誤時，標記顯示卡不可用並用 CPU 重試一次。
fn with_gpu_fallback<T>(
    model_id: &str,
    accelerator: &str,
    gpu_device: &str,
    run: impl Fn(&str) -> Result<T, String>,
) -> Result<T, String> {
    let accel = effective_accelerator(accelerator);
    match run(accel) {
        Err(error) if accel != "cpu" && is_backend_error(&error) && was_on_gpu(accel) => {
            GPU_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
            log::error!(
                "GPU backend failed; local ASR switches to CPU until SayIt restarts model={} gpu_device={} error={}",
                model_id,
                gpu_device,
                error
            );
            run("cpu")
        }
        other => other,
    }
}

/// 預載入模型。
pub fn preload(model_id: &str, accelerator: &str, gpu_device: &str) -> Result<(), String> {
    with_gpu_fallback(model_id, accelerator, gpu_device, |accel| {
        ensure_loaded(model_id, accel, gpu_device)
    })
}

/// 釋放常駐引擎，交還幾百 MB ~ 數 GB 記憶體。
pub fn unload() {
    if let Ok(mut cache) = CACHE.lock() {
        if let Some(entry) = cache.take() {
            log::info!("Unloading GGUF ASR model: {}", entry.model_id);
        }
    }
    mark_unloaded();
}

/// 當前繫結的後端字串，None = 未載入。
/// 讀 STATUS 那把小鎖，**不碰 CACHE** —— 見 STATUS 的說明。
pub fn current_backend() -> Option<String> {
    STATUS.lock().ok().and_then(|s| s.backend.clone())
}

/// 當前**實際**繫結的計算裝置描述，None = 未載入或庫沒報。
/// 多顯示卡機器上這才是"用的哪張卡"的答案，別用使用者的選擇代替。
pub fn current_device() -> Option<String> {
    STATUS.lock().ok().and_then(|s| s.device.clone())
}

/// 正在載入中的模型 id，None = 沒有載入在進行。
/// 與 `current_backend` 互斥：載入中時後端一定是 None，介面該顯示"正在載入"
/// 而不是"模型未載入"。
pub fn loading_model() -> Option<String> {
    STATUS.lock().ok().and_then(|s| s.loading_model.clone())
}

/// 轉寫一段 16 kHz mono f32 PCM。
pub fn transcribe(
    model_id: &str,
    language: &str,
    accelerator: &str,
    gpu_device: &str,
    samples: &[f32],
    sample_rate: usize,
) -> Result<String, String> {
    with_gpu_fallback(model_id, accelerator, gpu_device, |accel| {
        ensure_loaded(model_id, accel, gpu_device)?;
        transcribe_with_cache(samples, sample_rate, language)
    })
}

// ── 空閒解除安裝 ──

/// 最近一次用到引擎的時刻（Unix 毫秒）。載入和轉寫都會重新整理。
static LAST_USE_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn touch_activity() {
    LAST_USE_MS.store(now_ms(), std::sync::atomic::Ordering::Relaxed);
}

/// 推理持有 CACHE 鎖期間的活動守衛。宣告在鎖變數之後，因此離開函式時會先
/// 更新時間、再釋放鎖；空閒執行緒拿到鎖時看到的一定是本次推理完成後的時間。
struct ActivityGuard;

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        touch_activity();
    }
}

/// 當前空閒解除安裝配置。0 = 永不自動解除安裝；設定頁可動態改為 10 / 30 / 60。
static IDLE_UNLOAD_MINUTES: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
static IDLE_UNLOADER_STARTED: std::sync::Once = std::sync::Once::new();

pub fn set_idle_unload_minutes(idle_minutes: u64) {
    IDLE_UNLOAD_MINUTES.store(idle_minutes, std::sync::atomic::Ordering::SeqCst);
    if idle_minutes == 0 {
        log::info!("Local model idle unload is disabled; the model will remain in memory");
    } else {
        log::info!("Local model idle unload interval set to {} minutes", idle_minutes);
    }
}

/// 啟動唯一的空閒守護執行緒。執行緒每輪讀取最新配置，因此設定頁修改後立即生效，
/// 無需重複建立執行緒。到期判斷與 cache.take 在同一次加鎖內完成；若推理正在執行，
/// 守護執行緒會等待推理釋放鎖，再看到 ActivityGuard 重新整理的時間，不會誤解除安裝。
///
/// 解除安裝後的第一次轉寫需要重新載入和預熱（大模型約數秒）；預設值 0 讓模型常駐，
/// 記憶體緊張的使用者可主動選擇空閒 10 / 30 / 60 分鐘後釋放。
pub fn spawn_idle_unloader(initial_idle_minutes: u64) {
    set_idle_unload_minutes(initial_idle_minutes);
    IDLE_UNLOADER_STARTED.call_once(|| {
        std::thread::spawn(|| loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
            // 配置為 0 時避免無意義地爭用推理鎖；拿到鎖後還會再讀一次，防止
            // 使用者恰在這段間隙切換為“不自動解除安裝”卻仍按舊配置解除安裝一次。
            if IDLE_UNLOAD_MINUTES.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                continue;
            }

            let mut cache = match CACHE.lock() {
                Ok(cache) => cache,
                Err(error) => {
                    log::warn!("Failed to acquire lock during local model idle check: {}", error);
                    continue;
                }
            };
            let idle_minutes =
                IDLE_UNLOAD_MINUTES.load(std::sync::atomic::Ordering::SeqCst);
            if idle_minutes == 0 {
                continue;
            }
            let idle_ms = idle_minutes.saturating_mul(60).saturating_mul(1000);
            if cache.is_none() {
                continue;
            }

            let last = LAST_USE_MS.load(std::sync::atomic::Ordering::Relaxed);
            if last == 0 || now_ms().saturating_sub(last) < idle_ms {
                continue;
            }

            if let Some(entry) = cache.take() {
                log::info!(
                    "Local model was idle for {} minutes; unloaded to free memory: {}",
                    idle_minutes,
                    entry.model_id
                );
                // 這裡是直接 take 而不是走 unload()（已經持著 CACHE 鎖了），
                // 所以狀態映象要自己同步，否則診斷頁會一直報著已解除安裝的後端。
                mark_unloaded();
            }
        });
    });
}

// 注：曾有個 `is_gguf_model()` 用於在 sherpa / GGUF 之間分派。1c 之後本地只剩
// 這一個引擎，分派沒有意義，已刪除。catalog 裡的 id 仍保留 `-gguf` 字尾 ——
// 它現在的作用是讓新權重目錄和上一代 ONNX 目錄不撞名，從而讓舊模型回收能靠
// 目錄名白名單精確定位（見 local_asr::LEGACY_MODEL_IDS）。

#[cfg(test)]
mod tests {
    use super::*;

    const SR_U: usize = 16000;

    /// 讀 16 kHz / mono / 16-bit WAV 為 f32（只夠讀我們自己的測試音訊）。
    fn read_wav(name: &str) -> Vec<f32> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join(name);
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("resources/{name}: {e}"));
        assert_eq!(&bytes[0..4], b"RIFF");
        bytes[44..]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
            .collect()
    }

    fn read_test_wav() -> Vec<f32> {
        read_wav("test_zh.wav")
    }

    /// 英文測試音訊。純英文模型（parakeet-unified-en）在中文音訊上輸出**空字串**，
    /// 拿 test_zh.wav 測它只會得到一條看不出原因的"輸出為空"。
    /// 由 dev-scripts/make-test-en-wav.ps1 生成，可重現。
    fn read_test_en_wav() -> Vec<f32> {
        read_wav("test_en.wav")
    }

    /// 模型沒下載就跳過（CI / 乾淨機器上不該因為缺幾百 MB 權重而紅）。
    fn model_present(model_id: &str) -> bool {
        model_is_downloaded(model_id)
    }

    fn run(model_id: &str, repeats: usize) -> String {
        run_with(model_id, repeats, read_test_wav(), "auto")
    }

    /// `language` 走的是真實的 `transcribe()` 入口，所以這些測試也在過
    /// `resolve_language`（例如把 `en` 解析成 nemotron 要的 `en-US`）。
    fn run_with(model_id: &str, repeats: usize, base: Vec<f32>, language: &str) -> String {
        init_backends();
        assert!(
            !transcribe_cpp::devices().is_empty(),
            "沒有註冊到任何計算裝置：DLL 沒放到 exe 旁邊？"
        );
        let mut pcm = Vec::with_capacity(base.len() * repeats);
        for _ in 0..repeats {
            pcm.extend_from_slice(&base);
        }
        transcribe(model_id, language, "auto", "", &pcm, SR_U).expect("轉寫失敗")
    }

    /// SenseVoice：轉寫要出中文，且**必須有標點**。
    /// 標點靠 RunOptions 的 itn=on 開啟（預設走 <|woitn|> 分支、沒有標點），
    /// 這條斷言就是防止哪天有人把 itn 改回 Default 而沒人發現。
    #[test]
    fn sensevoice_gguf_has_punctuation() {
        let id = "sensevoice-small-gguf";
        if !model_present(id) {
            eprintln!("skip: {id} 未下載");
            return;
        }
        let text = run(id, 3);
        assert!(!text.trim().is_empty(), "輸出為空");
        assert!(
            text.chars().any(|c| "，。！？、".contains(c)),
            "SenseVoice 輸出沒有標點，itn 開關可能失效了: {text:?}"
        );
        // 特殊標籤不該漏到最終文本里
        assert!(!text.contains("<|"), "輸出殘留特殊標籤: {text:?}");
        unload();
    }

    /// Qwen3-ASR：自帶標點，且輸出不該帶 `<asr_text>` 這類範本字首
    /// （sherpa 的 ONNX 匯出會帶，需要額外清洗；ggml 這條路不需要）。
    fn assert_qwen3_output(id: &str) {
        if !model_present(id) {
            eprintln!("skip: {id} 未下載");
            return;
        }
        let text = run(id, 3);
        // 打出來便於人眼比對不同量化檔的輸出質量（Q4_K_M 這種激進量化最容易
        // 在這裡露出馬腳：錯字、漏字、標點亂）
        eprintln!("[text] {id}: {text}");
        assert!(!text.trim().is_empty(), "{id} 輸出為空");
        assert!(
            !text.contains("<asr_text>") && !text.contains("<|"),
            "{id} 輸出帶模板字首/特殊標籤: {text:?}"
        );
        assert!(
            text.chars().any(|c| "，。！？、".contains(c)),
            "{id} 輸出沒有標點: {text:?}"
        );
        unload();
    }

    #[test]
    fn qwen3_06b_gguf_is_clean_and_punctuated() {
        assert_qwen3_output("qwen3-asr-0.6b-gguf");
    }

    /// 1.7B 與 0.6B 同族同契約，只是更大更準更慢，行為斷言完全一樣。
    #[test]
    fn qwen3_17b_gguf_is_clean_and_punctuated() {
        assert_qwen3_output("qwen3-asr-1.7b-gguf");
    }

    /// 1.7B 的 Q4_K_M 檔。量化更狠，但契約不變 —— 這條守的是"換量化檔不該改變
    /// 輸出形態"（比如更低的量化位寬讓模型開始吐特殊標籤或丟標點）。
    #[test]
    fn qwen3_17b_q4_gguf_is_clean_and_punctuated() {
        assert_qwen3_output("qwen3-asr-1.7b-q4-gguf");
    }

    /// Parakeet Unified EN：英文專用。標點、大小寫、數字規範化都是模型固有行為
    /// （ITN/PNC 開關它都報 unsupported），所以這裡斷言的是"預設就該有"。
    ///
    /// 同時釘住 `language="en"` 這條路：parakeet 自報的語種碼是裸 `en`，
    /// `resolve_language` 應當精確命中、原樣傳過去。
    #[test]
    fn parakeet_en_gguf_is_punctuated_and_capitalized() {
        let id = "parakeet-unified-en-0.6b-gguf";
        if !model_present(id) {
            eprintln!("skip: {id} 未下載");
            return;
        }
        let text = run_with(id, 1, read_test_en_wav(), "en");
        eprintln!("[text] {id}: {text}");
        assert!(!text.trim().is_empty(), "{id} 輸出為空");
        assert!(!text.contains("<|") && !text.contains('<'), "殘留特殊標籤: {text:?}");
        assert!(
            text.contains('.') || text.contains(','),
            "{id} 英文輸出沒有標點: {text:?}"
        );
        assert!(
            text.chars().any(|c| c.is_ascii_uppercase()),
            "{id} 輸出沒有大寫字母，capitalization 失效？: {text:?}"
        );
        unload();
    }

    /// Nemotron 3.5：本目錄裡第一個只認**帶地區 locale** 的模型。
    ///
    /// 這條測試的重點不是轉寫質量，而是 `language="en"` 不會炸 —— 介面上的
    /// `localAsr.language` 只有 auto/zh/en/ja/ko，不經 `resolve_language` 對映成
    /// `en-US` 就會得到 `unsupported language (status 10)`，使用者選「英語」後
    /// 每次口述都失敗。斷言"有輸出"就足以證明對映生效了。
    #[test]
    fn nemotron_gguf_accepts_a_bare_language_code() {
        let id = "nemotron-asr-streaming-0.6b-gguf";
        if !model_present(id) {
            eprintln!("skip: {id} 未下載");
            return;
        }
        let text = run_with(id, 1, read_test_en_wav(), "en");
        eprintln!("[text] {id} (language=en): {text}");
        assert!(
            !text.trim().is_empty(),
            "{id} 在 language=en 下輸出為空 —— locale 對映可能失效了"
        );
        // auto 檢測模式會在原始 token 流裡插 <en-US> 這類標籤，庫預設會清掉。
        // 漏到最終文本里就會被當成使用者說的話插進目標程式。
        assert!(!text.contains('<') && !text.contains('>'), "殘留語種標籤: {text:?}");
        unload();
    }

    /// 多語種模型選到它不支援的語種時不該報錯。nemotron 支援 zh-CN，
    /// 所以這裡順便驗證中文也能走通（它是目錄裡唯一同時覆蓋中英的新模型）。
    #[test]
    fn nemotron_gguf_handles_chinese_via_locale_mapping() {
        let id = "nemotron-asr-streaming-0.6b-gguf";
        if !model_present(id) {
            eprintln!("skip: {id} 未下載");
            return;
        }
        let text = run_with(id, 3, read_test_wav(), "zh");
        eprintln!("[text] {id} (language=zh): {text}");
        assert!(!text.trim().is_empty(), "{id} 在 language=zh 下輸出為空");
        assert!(!text.contains('<') && !text.contains('>'), "殘留語種標籤: {text:?}");
        unload();
    }

    /// Fun-ASR Nano：和 SenseVoice 一樣，標點掛在 ITN 開關上（探針裡用
    /// `RunOptions::default()` 跑出來是**沒有標點**的）。這條走生產路徑，
    /// 斷言 `run_options()` 的 itn 門控確實把標點開出來了。
    /// 它還是個 audio-LLM（帶 chat template），所以順帶守住"別把模板字首吐出來"。
    #[test]
    fn funasr_nano_gguf_is_clean_and_punctuated() {
        let id = "funasr-nano-2512-gguf";
        if !model_present(id) {
            eprintln!("skip: {id} 未下載");
            return;
        }
        let text = run(id, 3);
        eprintln!("[text] {id}: {text}");
        assert!(!text.trim().is_empty(), "輸出為空");
        assert!(
            text.chars().any(|c| "，。！？、".contains(c)),
            "Fun-ASR Nano 輸出沒有標點，itn 門控失效了？: {text:?}"
        );
        assert!(
            !text.contains("<|") && !text.contains("<asr_text>") && !text.contains("im_start"),
            "輸出殘留模板/特殊標籤: {text:?}"
        );
        unload();
    }

    /// 不支援 ITN 的模型族不該被請求 ITN —— 否則庫每次轉寫都打一條
    /// "does not support itn control" 的 WARN，把日誌刷髒。
    #[test]
    fn itn_is_only_requested_when_the_family_supports_it() {
        let supported = run_options("auto", &[], true, false);
        assert_eq!(supported.itn, Itn::On);
        assert_eq!(supported.pnc, Pnc::Default);

        let unsupported = run_options("auto", &[], false, false);
        assert_eq!(unsupported.itn, Itn::Default);
        assert_eq!(unsupported.pnc, Pnc::Default);
    }

    /// 裸碼族（SenseVoice / Fun-ASR / Qwen3-ASR）的清單，用於下面幾條斷言。
    fn bare_codes() -> Vec<String> {
        ["zh", "yue", "en", "ja", "ko"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    /// nemotron-3.5 的真實清單（擷取，保留順序 —— `en` 要解析成 `en-US` 而不是
    /// `en-GB`，靠的就是順序）。
    fn nemotron_locales() -> Vec<String> {
        ["en-US", "en-GB", "de-DE", "ja-JP", "ko-KR", "zh-CN"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn auto_language_means_autodetect() {
        let langs = bare_codes();
        assert!(run_options("auto", &langs, false, false).language.is_none());
        assert!(run_options("", &langs, false, false).language.is_none());
        assert_eq!(
            run_options("zh", &langs, false, false).language.as_deref(),
            Some("zh")
        );
    }

    /// 裸碼族的行為必須和加 `resolve_language` 之前完全一樣，否則這個改動就把
    /// 已釋出的三個模型搞壞了。
    #[test]
    fn bare_language_codes_pass_through_unchanged() {
        let langs = bare_codes();
        for code in ["zh", "en", "ja", "ko", "yue"] {
            assert_eq!(
                resolve_language(code, &langs).as_deref(),
                Some(code),
                "{code} 在裸碼族上不該被改寫"
            );
        }
    }

    /// nemotron-3.5 只認帶地區的 locale，`en` 必須被解析成 `en-US`。
    /// 不做這層對映的話，使用者在設定裡選「英語」會得到
    /// `unsupported language (status 10)`，整次轉寫失敗。
    #[test]
    fn bare_code_maps_to_the_models_regional_locale() {
        let langs = nemotron_locales();
        assert_eq!(resolve_language("en", &langs).as_deref(), Some("en-US"));
        assert_eq!(resolve_language("zh", &langs).as_deref(), Some("zh-CN"));
        assert_eq!(resolve_language("ja", &langs).as_deref(), Some("ja-JP"));
        assert_eq!(resolve_language("ko", &langs).as_deref(), Some("ko-KR"));
        // 大小寫不該影響匹配
        assert_eq!(resolve_language("EN", &langs).as_deref(), Some("en-US"));
    }

    /// 模型沒廣告的語種回落到自動檢測，而不是把原串硬塞過去讓它報錯。
    /// 例：給只有 `["en"]` 的 parakeet-unified-en 選中文。
    #[test]
    fn unsupported_language_falls_back_to_autodetect() {
        let english_only = vec!["en".to_string()];
        assert_eq!(resolve_language("en", &english_only).as_deref(), Some("en"));
        assert!(resolve_language("zh", &english_only).is_none());
        assert!(resolve_language("ko", &english_only).is_none());
    }

    /// 語種無關的模型（清單為空）沒有可比物件，保持原樣透傳。
    #[test]
    fn empty_language_list_passes_the_request_through() {
        assert_eq!(resolve_language("zh", &[]).as_deref(), Some("zh"));
        assert!(resolve_language("auto", &[]).is_none());
    }

    // ── 後端註冊必須保持懶載入（原始碼約束）──
    //
    // 為什麼用原始碼掃描而不是行為測試：`init_backends` 是程序級 `Once`，同一個測試
    // 程序裡別的測試可能已經註冊過，所以執行時沒法觀測"啟動路徑有沒有註冊"。
    // 而這條約束一旦破了，症狀是"雲 API 使用者又開始白佔視訊記憶體"——使用者能看見，
    // 測試卻全綠。仿照 main.rs 裡 `configured_windows_do_not_override_webview2_browser_args`
    // 的同一套做法。

    /// 去掉行註釋，避免註釋裡出現的函式名被當成真實呼叫。
    fn strip_line_comments(source: &str) -> String {
        source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 啟動路徑絕不能註冊計算後端。加回去等於讓雲 API / 伺服器模式的使用者重新白付
    /// 一個 Vulkan 上下文（約 36 MB 共享視訊記憶體 + 80~520 ms 啟動阻塞），而他們一個
    /// 模型都不會載入。詳見 `init_backends` 的文件註釋。
    #[test]
    fn startup_path_does_not_register_compute_backends() {
        let main_rs = strip_line_comments(include_str!("../main.rs"));
        assert!(
            !main_rs.contains("init_backends"),
            "main.rs 又在啟動路徑上註冊計算後端了。這會 dlopen ggml-vulkan.dll 並建出\n\
             Vulkan 上下文，雲 API / 伺服器模式的使用者白付視訊記憶體與啟動時間。\n\
             註冊應當只發生在 gguf_asr 的懶路徑（ensure_loaded / describe_devices）。"
        );
    }

    /// 反向約束：懶路徑必須真的自己註冊。少了這一步，本地模式會在
    /// `Model::load_with` 直接報 TRANSCRIBE_ERR_BACKEND，診斷頁的裝置列表會空白。
    #[test]
    fn lazy_entry_points_register_compute_backends() {
        let source = strip_line_comments(include_str!("gguf_asr.rs"));
        for (entry, signature) in [
            (
                "ensure_loaded",
                "fn ensure_loaded(model_id: &str, accelerator: &str, gpu_device: &str)",
            ),
            ("describe_devices", "pub fn describe_devices()"),
        ] {
            let body_start = source
                .find(signature)
                .unwrap_or_else(|| panic!("{entry} 的簽名變了，請同步更新這條測試"));
            // 只看函式開頭這一小段：註冊是前置動作，必須在任何實際工作之前發生。
            let head = &source[body_start..(body_start + 700).min(source.len())];
            assert!(
                head.contains("init_backends()"),
                "{entry} 沒有先呼叫 init_backends()。啟動路徑上已經不註冊了，\n\
                 這裡少一步就會讓本地模式報 TRANSCRIBE_ERR_BACKEND / 裝置列表空白。"
            );
        }
    }

    /// 一台"集顯 + 獨顯 + CPU"機器的裝置清單，順序照 ggml 的 registry
    /// （GPU 在前、CPU 最後，與本機實測 `[vulkan:Vulkan0, cpu:CPU]` 一致）。
    fn fake_devices() -> Vec<GgufDevice> {
        vec![
            GgufDevice {
                kind: "vulkan".into(),
                name: "Integrated Graphics".into(),
                memory_mb: 2048,
                id: "PCI:0000:00:02.0".into(),
                index: 0,
                is_gpu: true,
            },
            GgufDevice {
                kind: "vulkan".into(),
                name: "Discrete GPU".into(),
                memory_mb: 8192,
                id: "PCI:0000:01:00.0".into(),
                index: 1,
                is_gpu: true,
            },
            GgufDevice {
                kind: "cpu".into(),
                name: "CPU".into(),
                memory_mb: 32768,
                id: "cpu:CPU".into(),
                index: 2,
                is_gpu: false,
            },
        ]
    }

    /// 選第二張卡是這個功能的主用例（反饋裡的原話：顯示卡 0 已經被別的模型佔滿，
    /// 想讓 SayIt 用顯示卡 1）。必須解析成那張卡的 registry 索引。
    #[test]
    fn selecting_a_secondary_gpu_yields_its_registry_index() {
        assert_eq!(
            pick_gpu_device_index(&fake_devices(), "PCI:0000:01:00.0", "auto"),
            1
        );
        // 顯式請求 GPU 後端時同樣成立（上游會額外校驗廠商一致，Vulkan 對 Vulkan 沒問題）
        assert_eq!(
            pick_gpu_device_index(&fake_devices(), "PCI:0000:01:00.0", "gpu"),
            1
        );
    }

    /// 四條回落，每一條都必須落到 0（自動），絕不能把非法值傳給上游 ——
    /// 上游對非法 `gpu_device` 是**直接報 INVALID_ARG 讓整次載入失敗**，
    /// 不是悄悄回落。使用者換了張顯示卡就打不開本地識別是不可接受的。
    #[test]
    fn invalid_or_unavailable_selections_fall_back_to_automatic() {
        let devs = fake_devices();
        // 沒設定 / 顯式自動
        assert_eq!(pick_gpu_device_index(&devs, "", "auto"), 0);
        assert_eq!(pick_gpu_device_index(&devs, "auto", "auto"), 0);
        // 設定裡記著一張已經不在機器上的卡（換卡、拔了 eGPU、驅動沒起來）
        assert_eq!(pick_gpu_device_index(&devs, "PCI:0000:09:00.0", "auto"), 0);
        // 指向了 CPU：上游判 "is not a GPU device" → INVALID_ARG
        assert_eq!(pick_gpu_device_index(&devs, "cpu:CPU", "auto"), 0);
        // 後端選了 CPU 時任何非零值都是 INVALID_ARG（"no GPU to select"）
        assert_eq!(pick_gpu_device_index(&devs, "PCI:0000:01:00.0", "cpu"), 0);
    }

    /// registry 索引 0 是上游的"自動"哨兵，沒有辦法顯式選中它。
    /// 這條釘住的是我們**知道**這個限制並按 0 傳 —— 傳 0 只是讓庫按探測順序挑
    /// （獨顯優先），所以介面上必須靠 `current_device` 顯示真實結果，
    /// 顯示卡被重置後庫返回的兩種錯誤（2026-10-19 sayit.log 原樣）都要被認成後端錯誤，
    /// 否則不會回落到 CPU；模型沒下載、語種不支援這類錯誤不能觸發回落。
    #[test]
    fn device_lost_errors_trigger_cpu_fallback_and_others_do_not() {
        assert!(is_backend_error("Inference failed: backend error: run: backend error (status 8)"));
        assert!(is_backend_error(
            "Failed to load model (sensevoice-small-gguf): backend error: load C:\\x\\SenseVoiceSmall-Q8_0.gguf: backend error (status 8)"
        ));
        assert!(!is_backend_error("No .gguf file found in model directory"));
        assert!(!is_backend_error("Inference failed: unsupported language (status 10)"));
    }

    /// 不能假設"選了就是用了"。詳見 `resolve_gpu_device` 的文件註釋。
    #[test]
    fn registry_index_zero_is_the_automatic_sentinel() {
        assert_eq!(
            pick_gpu_device_index(&fake_devices(), "PCI:0000:00:02.0", "auto"),
            0
        );
    }

    /// 內嵌的預熱音訊必須能解析出足夠的樣本。這條防的是 include_bytes! 的路徑
    /// 寫錯或資原始檔被換掉 —— 那樣預熱會靜默變成空操作，第一次口述又會慢回去。
    #[test]
    fn warmup_wav_yields_usable_pcm() {
        let n = WARMUP_WAV
            .get(44..)
            .unwrap_or(&[])
            .chunks_exact(2)
            .take(WARMUP_SEC * 16000)
            .count();
        assert!(
            n >= 16000,
            "預熱音訊只解析出 {n} 個樣本（不足 1 秒），include_bytes! 路徑或資原始檔有問題"
        );
    }

    /// 載入完成後引擎必須已經預熱過：緊接著的第一次轉寫不該再有 pipeline 編譯
    /// 那種量級的額外開銷。用"第一次 ≈ 第二次"來驗證，比斷言絕對耗時穩。
    #[test]
    fn first_transcribe_after_load_is_not_slower_than_the_second() {
        let id = "sensevoice-small-gguf";
        if !model_present(id) {
            eprintln!("skip: {id} 未下載");
            return;
        }
        unload();
        let base = read_test_wav();

        // 這一次含載入 + 預熱
        let t_load = std::time::Instant::now();
        let _ = transcribe(id, "auto", "auto", "", &base, SR_U).expect("轉寫失敗");
        let load_and_first = t_load.elapsed().as_secs_f64();

        let t1 = std::time::Instant::now();
        let _ = transcribe(id, "auto", "auto", "", &base, SR_U).expect("轉寫失敗");
        let first = t1.elapsed().as_secs_f64();

        let t2 = std::time::Instant::now();
        let _ = transcribe(id, "auto", "auto", "", &base, SR_U).expect("轉寫失敗");
        let second = t2.elapsed().as_secs_f64();

        eprintln!(
            "[perf] {id}: load+warmup+run={load_and_first:.3}s then {first:.3}s / {second:.3}s"
        );
        // 預熱到位時兩次穩定態解碼應該接近；給 3 倍餘量吸收排程噪聲
        assert!(
            first < second * 3.0 + 0.3,
            "載入後的第一次解碼 {first:.3}s 遠慢於第二次 {second:.3}s，預熱沒生效？"
        );
        unload();
    }

    /// 走生產路徑（accelerator="auto"）跑一遍，列印實際繫結的後端和 RTF。
    ///
    /// 不斷言"必須是 vulkan" —— 沒 GPU 的機器也得能過。斷言的是一個寬鬆的 RTF
    /// 上限：正常情況下最慢的 1.7B 純 CPU 也在 0.4 左右，超過 2.0 說明有東西
    /// 嚴重不對（回落到了病態路徑、或者 n_threads 沒生效之類）。
    #[test]
    fn report_backend_and_rtf() {
        let ids = ALL_MODELS;
        let base = read_test_wav();
        let repeats = 3;
        let audio_sec = (base.len() * repeats) as f64 / SR_U as f64;

        for id in ids {
            if !model_present(id) {
                eprintln!("skip: {id} 未下載");
                continue;
            }
            // 預熱一次（首次含圖構建/視訊記憶體分配），再計時
            let _ = run(id, repeats);
            let t0 = std::time::Instant::now();
            let text = run(id, repeats);
            let secs = t0.elapsed().as_secs_f64();
            let rtf = secs / audio_sec;
            eprintln!(
                "[perf] {id}: backend={} {:.3}s rtf={:.3} audio={:.2}s text_len={}",
                current_backend().unwrap_or_else(|| "?".into()),
                secs,
                rtf,
                audio_sec,
                text.chars().count()
            );
            assert!(
                rtf < 2.0,
                "{id} 的 RTF {rtf:.3} 異常高（backend={:?}）",
                current_backend()
            );
            unload();
        }
    }

    /// `accelerator="cpu"` 必須真的綁到 CPU 上。
    ///
    /// 這是使用者遇到 GPU 驅動問題時的逃生口：機器上有可用 Vulkan 裝置、但設定裡
    /// 選了 CPU，就不能悄悄還跑在 GPU 上。沒 GPU 的機器上這條自然也成立。
    #[test]
    fn forcing_cpu_binds_cpu_even_when_a_gpu_exists() {
        let id = "sensevoice-small-gguf";
        if !model_present(id) {
            eprintln!("skip: {id} 未下載");
            return;
        }
        init_backends();
        let has_gpu = transcribe_cpp::devices().iter().any(|d| d.kind != "cpu");
        eprintln!("[info] GPU 裝置存在: {has_gpu}");

        let base = read_test_wav();
        let text = transcribe(id, "auto", "cpu", "", &base, SR_U).expect("轉寫失敗");
        assert!(!text.trim().is_empty(), "強制 CPU 後輸出為空");
        let backend = current_backend().unwrap_or_default();
        assert!(
            backend.to_lowercase().contains("cpu"),
            "設定要求 CPU，實際繫結到了 {backend:?}"
        );
        unload();
    }

    // ── 調優探針（預設 #[ignore]，手動跑）──
    //
    // 這兩條不是迴歸測試，是"把庫暴露的旋鈕實測一遍"的實驗台，結論記在
    // dev-docs/local-asr-ggml-migration.md。留在倉庫裡是因為下次換模型/換
    // transcribe.cpp 版本時還要再跑一遍，重寫比留著貴。
    //
    //   cargo test --release footprint_report -- --ignored --nocapture --test-threads=1
    //   cargo test --release decode_knobs_report -- --ignored --nocapture --test-threads=1

    const ALL_MODELS: [&str; 5] = [
        "sensevoice-small-gguf",
        "funasr-nano-2512-gguf",
        "qwen3-asr-0.6b-gguf",
        "qwen3-asr-1.7b-q4-gguf",
        "qwen3-asr-1.7b-gguf",
    ];

    /// 每個模型載入後的真實記憶體佔用。權重體積 ≠ 記憶體佔用，catalog 裡寫給使用者看的
    /// "記憶體佔用 X" 應該照這裡的實測值寫。
    #[test]
    #[ignore]
    fn footprint_report() {
        init_backends();
        let base = read_test_wav();
        for id in ALL_MODELS {
            if !model_present(id) {
                eprintln!("skip: {id} 未下載");
                continue;
            }
            unload();
            std::thread::sleep(std::time::Duration::from_secs(2)); // 等工作集回落
            let before = process_memory_mb();
            transcribe(id, "auto", "auto", "", &base, SR_U).expect("轉寫失敗");
            let after = process_memory_mb();
            let limits = CACHE
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|e| e.session.limits().ok());
            eprintln!(
                "[mem] {id}: {before} MB -> {after} MB (delta {} MB) limits={:?}",
                after.saturating_sub(before),
                limits
            );
        }
        unload();
    }

    /// 候選新模型族的落地探針：能力、輸出形態、語言引數、速度。
    ///
    /// 加一個**新架構**（不只是新量化檔）之前必須先過這一關 —— 尤其 audio-LLM
    /// 類的族帶 chat template，很可能把模板字首吐進文本里（上一代 ONNX 的
    /// funasr-nano 就需要額外清洗）。權重放到
    /// `%LOCALAPPDATA%\com.sayitlocally.desktop\models\<dir>\` 下即可，不必先進 catalog。
    #[test]
    #[ignore]
    fn new_family_probe() {
        init_backends();
        let base = read_test_wav();
        let mut pcm = Vec::new();
        for _ in 0..3 {
            pcm.extend_from_slice(&base);
        }
        let audio_sec = pcm.len() as f64 / SR_U as f64;

        for dir in ["funasr-nano-2512-gguf"] {
            let path = match find_probe_gguf(&model_dir(dir)) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("skip: {dir} ({e})");
                    continue;
                }
            };
            let t_load = std::time::Instant::now();
            let model = Model::load_with(
                &path,
                &ModelOptions {
                    backend: Backend::Auto,
                    gpu_device: 0,
                },
            )
            .expect("載入失敗");
            let load_ms = t_load.elapsed().as_millis();
            let caps = model.capabilities();
            eprintln!(
                "[probe] {dir}: arch={} variant={} backend={} load={load_ms}ms itn={} pnc={} \
                 lang_detect={} n_langs={} max_ts={:?} max_audio={:.1}min",
                model.arch(),
                model.variant(),
                model.backend(),
                model.supports(Feature::Itn),
                model.supports(Feature::Pnc),
                caps.supports_language_detect,
                caps.languages.len(),
                caps.max_timestamp_kind,
                caps.max_audio_ms as f64 / 60_000.0,
            );

            let mut s = model.session().expect("session 失敗");
            // lang_detect=false 的族傳 None（自動）會怎樣？和顯式 "zh" 對照。
            for lang in [None, Some("zh")] {
                let opts = RunOptions {
                    language: lang.map(str::to_string),
                    ..Default::default()
                };
                let _ = s.run(&pcm, &opts); // 預熱
                let mem = process_memory_mb();
                let t = std::time::Instant::now();
                match s.run(&pcm, &opts) {
                    Ok(tr) => eprintln!(
                        "[probe] {dir} lang={lang:?}: rtf={:.3} mem={mem}MB detected={:?} text={}",
                        t.elapsed().as_secs_f64() / audio_sec,
                        tr.language,
                        tr.text.trim()
                    ),
                    Err(e) => eprintln!("[probe] {dir} lang={lang:?}: 失敗 {e}"),
                }
            }
        }
    }

    /// 語言自動檢測的穩定性探針：qwen3 在**短音訊 + auto** 下會不會誤判語種
    /// （使用者報告：識別測試裡 1.7B 把 3 秒普通話測試音訊判成了粵語）。
    /// 三組對照：冷啟動直跑 / 預熱後跑（復刻生產路徑）/ 顯式 zh。
    #[test]
    #[ignore]
    fn language_detect_report() {
        init_backends();
        // 單遍、不重複 —— 和識別測試 run_asr_benchmark 用的時長一致
        let base = read_test_wav();
        eprintln!("[lang] clip = {:.2}s", base.len() as f64 / SR_U as f64);
        for id in ["qwen3-asr-0.6b-gguf", "qwen3-asr-1.7b-q4-gguf", "qwen3-asr-1.7b-gguf"] {
            if !model_present(id) {
                eprintln!("skip: {id} 未下載");
                continue;
            }
            let path = registry::verified_model_path(id).unwrap();
            let model = Model::load_with(
                &path,
                &ModelOptions {
                    backend: Backend::Auto,
                    gpu_device: 0,
                },
            )
            .expect("載入失敗");
            let mut s = model.session().expect("session 失敗");
            for i in 0..3 {
                let tr = s.run(&base, &RunOptions::default()).expect("推理失敗");
                eprintln!(
                    "[lang] {id} auto#{i}: detected={:?} text={}",
                    tr.language,
                    tr.text.trim()
                );
            }
            // 復刻生產路徑：先用前 2 秒預熱，再跑整段
            let warm: Vec<f32> = base.iter().copied().take(2 * SR_U).collect();
            let _ = s.run(&warm, &RunOptions::default());
            let tr = s.run(&base, &RunOptions::default()).expect("推理失敗");
            eprintln!(
                "[lang] {id} after-warmup: detected={:?} text={}",
                tr.language,
                tr.text.trim()
            );
            let tr = s
                .run(
                    &base,
                    &RunOptions {
                        language: Some("zh".into()),
                        ..Default::default()
                    },
                )
                .expect("推理失敗");
            eprintln!(
                "[lang] {id} lang=zh: detected={:?} text={}",
                tr.language,
                tr.text.trim()
            );
        }
    }

    /// 解碼旋鈕掃描：投機解碼的 draft 長度、解碼上下文上限、時間戳粒度。
    /// 只掃最慢的兩個自迴歸模型 —— 它們才有可觀的解碼開銷可省。
    #[test]
    #[ignore]
    fn decode_knobs_report() {
        init_backends();
        let base = read_test_wav();
        let mut pcm = Vec::new();
        for _ in 0..3 {
            pcm.extend_from_slice(&base);
        }
        let audio_sec = pcm.len() as f64 / SR_U as f64;

        for id in ["qwen3-asr-0.6b-gguf", "qwen3-asr-1.7b-gguf"] {
            if !model_present(id) {
                eprintln!("skip: {id} 未下載");
                continue;
            }
            let path = registry::verified_model_path(id).unwrap();
            let model = Model::load_with(
                &path,
                &ModelOptions {
                    backend: Backend::Auto,
                    gpu_device: 0,
                },
            )
            .expect("載入失敗");
            let caps = model.capabilities();
            eprintln!(
                "[caps] {id}: backend={} spec_decode={} streaming={} max_ts={:?} max_audio_ms={}",
                model.backend(),
                caps.supports_spec_decode,
                caps.supports_streaming,
                caps.max_timestamp_kind,
                caps.max_audio_ms
            );

            // (1) n_ctx：0 = 模型上限。關心三件事：max_kv_bytes、實際記憶體、以及
            // **能吃多長音訊**（n_ctx 同時決定 effective_max_audio_ms，砍太小會讓
            // 長口述被迫分段）。順序故意來回走，用來排除"先跑的那個偏慢"這種
            // 升頻/快取導致的假象。
            for n_ctx in [0, 8192, 2048, 8192, 0] {
                let mut s = model
                    .session_with(&SessionOptions {
                        n_ctx,
                        ..Default::default()
                    })
                    .expect("session 失敗");
                let limits = s.limits().unwrap();
                let opts = RunOptions::default();
                let _ = s.run(&pcm, &opts); // 預熱
                let mem = process_memory_mb();
                let t = std::time::Instant::now();
                let _ = s.run(&pcm, &opts).expect("推理失敗");
                eprintln!(
                    "[n_ctx] {id} n_ctx={n_ctx}: eff={} max_audio={:.1}min max_kv={:.0}MB mem={mem}MB rtf={:.3}",
                    limits.effective_n_ctx,
                    limits.effective_max_audio_ms as f64 / 60_000.0,
                    limits.max_kv_bytes as f64 / (1024.0 * 1024.0),
                    t.elapsed().as_secs_f64() / audio_sec
                );
            }

            // (2) spec_k_drafts：-1 = 族預設，0 = 關，正數 = 每步猜幾個 token。
            let mut s = model.session().expect("session 失敗");
            for k in [-1, 0, 2, 4, 8] {
                let opts = RunOptions {
                    spec_k_drafts: k,
                    ..Default::default()
                };
                let _ = s.run(&pcm, &opts); // 預熱該配置
                let t = std::time::Instant::now();
                let out = s.run(&pcm, &opts);
                match out {
                    Ok(tr) => eprintln!(
                        "[spec] {id} k={k}: rtf={:.3} text_len={}",
                        t.elapsed().as_secs_f64() / audio_sec,
                        tr.text.chars().count()
                    ),
                    Err(e) => eprintln!("[spec] {id} k={k}: 失敗 {e}"),
                }
            }

            // (3) 時間戳粒度：我們從不用時間戳，Auto 會讓庫產出"模型支援的最細"。
            for ts in [TimestampKind::Auto, TimestampKind::None] {
                let opts = RunOptions {
                    timestamps: ts,
                    ..Default::default()
                };
                let _ = s.run(&pcm, &opts);
                let t = std::time::Instant::now();
                let _ = s.run(&pcm, &opts).expect("推理失敗");
                eprintln!(
                    "[ts] {id} {:?}: rtf={:.3}",
                    ts,
                    t.elapsed().as_secs_f64() / audio_sec
                );
            }
        }
    }

    /// 超過模型音訊上限時要能自動分段，而不是報錯或截斷。
    /// SenseVoice 的上限只有 30 s，拿它做這條最省時間。
    #[test]
    fn audio_over_model_limit_is_chunked() {
        let id = "sensevoice-small-gguf";
        if !model_present(id) {
            eprintln!("skip: {id} 未下載");
            return;
        }
        // test_zh.wav 約 3.05 s，重複 12 遍 ≈ 36.6 s > 30 s 上限
        let text = run(id, 12);
        assert!(!text.trim().is_empty(), "長音訊輸出為空");
        // 12 段內容相同，至少應該比單段長得多，說明後面的段沒被丟掉
        let single = run(id, 1);
        assert!(
            text.chars().count() > single.chars().count() * 3,
            "長音訊疑似被截斷: len={} single={}",
            text.chars().count(),
            single.chars().count()
        );
        unload();
    }

}
