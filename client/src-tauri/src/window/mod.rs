use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::commands::system::write_log_line;
use crate::context::{capture_foreground_monitor, MonitorBounds};

const OVERLAY_DEFAULT_BASE_WIDTH: f64 = 360.0;
// 64 而不是剛好夠用的 56：內容是底邊錨定的（根節點 items-end + pb-4），加高只是在膠囊
// 上方多留一塊透明區，底邊位置和觀感都不變（y = bottom - gap - height，高度抵消掉了），
// 純粹換取容錯。56 時餘量只有 2px（16 的 pb-4 + 38 的膠囊 = 54），任何一點縮放誤差或
// 日後給膠囊加一根邊框都會把頂部裁平——這就是 Windows「文本大小」123% 時的故障現場。
const OVERLAY_BASE_HEIGHT: f64 = 64.0;
const OVERLAY_FALLBACK_WIDTH: f64 = 520.0;
const OVERLAY_FALLBACK_HEIGHT: f64 = 224.0;
/// 識別失敗卡片：比結果卡矮，因為它沒有文本預覽區，只有標題 + 原因 + 可能一行恢復提示。
/// **必須是可互動佈局**（見 is_interactive）——它上面有關閉按鈕，而非互動佈局是點選
/// 穿透的，按鈕會點不到。
///
/// 150 → 176：原來那個值是按「原因只佔一行」算的，而供應商的錯誤文案根本不止一行 ——
/// 「帳戶餘額不足，本次請求未能發出。請在供應商後台充值後重試…」在 520px 寬的 text-xs
/// 下就是兩行，實測內容高 97（日誌 `render ack OK ... content=520x97.33`）。再補上恢復
/// 提示就是 126，而 150 的視窗只能裝 134，餘量 4px；文案再長一點標題就被 overflow:hidden
/// 從頂部裁掉。176 覆蓋「原因三行 + 恢復提示」（146）並留 14px 餘量。
/// 520 → 480：三行小字的卡片佔 520 顯得空，但 440 又太緊 —— 餘額不足那條文案（28 字）
/// 會折出「試。」這樣只剩一個字加句號的末行。480 讓它一行放完。
///
/// **必須與 Overlay.tsx 失敗卡的 max-w 一致** —— 卡片比視窗窄時，多出來的透明邊照樣吞掉
/// 下面程式的點選（這是 is_interactive 佈局）；比視窗寬則被 overflow:hidden 裁掉。
///
/// 最長的那條錯誤文案（err.provider.forbidden，64 字）在 480 下折兩行，高度 126，容量內。
const OVERLAY_FAILURE_WIDTH: f64 = 480.0;
const OVERLAY_FAILURE_HEIGHT: f64 = 176.0;
// 流式即時顯示：錄音氣泡 + 波形條堆疊，需要更寬更高的視窗
const OVERLAY_STREAMING_WIDTH: f64 = 480.0;
const OVERLAY_STREAMING_HEIGHT: f64 = 200.0;
// 輸入源狀態條顯示在膠囊上方；視窗仍錨定底部，因此只向上擴充套件。
const OVERLAY_MIC_HINT_WIDTH: f64 = 480.0;
const OVERLAY_MIC_HINT_HEIGHT: f64 = 104.0;
const OVERLAY_STREAMING_MIC_HINT_HEIGHT: f64 = 244.0;
const OVERLAY_SCREEN_MARGIN: f64 = 8.0;

/// Overlay.tsx 根節點的 `pb-4`，單位是 CSS px。佈局仍由 CSS 決定，這裡只是映象值，
/// 用於「內容裝不裝得下」的自檢與單測；改了那邊記得同步這裡，否則判據會失真。
const OVERLAY_ROOT_PADDING_BOTTOM: f64 = 16.0;

/// webview 的 CSS 縮放相對顯示器縮放的額外倍數，可接受範圍。
///
/// 為什麼需要：Tauri 按「邏輯畫素 × 顯示器 scale」定視窗大小，而 WebView2 的一個 CSS px
/// 實際是 `顯示器 scale × 額外縮放` 個裝置畫素——Windows 設定 → 輔助功能 → 文本大小
/// （滑塊 100%~225% 連續可調）會被 Edge/WebView2 當成頁面縮放疊上去。於是視窗按 56 邏輯
/// px 建出來，webview 裡只有 56/1.23 ≈ 45.6 CSS px 可用，內容從頂部溢位、被
/// overlay.html 的 `overflow: hidden` 裁平。上限給到 4.0 是留足餘量（225% 文本 × 使用者
/// 可能另外用 Ctrl+滾輪縮放過共享的 EBWebView profile）。
const OVERLAY_CSS_ZOOM_MIN: f64 = 0.5;
const OVERLAY_CSS_ZOOM_MAX: f64 = 4.0;
/// 發出 blank 之後等多久才真正 hide 視窗（見 `hide_overlay` 裡的長註釋）。
///
/// 只需要夠 webview 出一幀：60Hz 下一幀 16.7ms，80ms 給足了 setState + 重繪 + 合成。
/// 使用者看不出差別 —— blank 連膠囊底色都不畫，"提示消失"這件事在發 blank 那一刻就完成了，
/// 視窗晚 80ms 消失沒有任何視覺痕跡。
const HIDE_AFTER_BLANK_MS: u64 = 80;
const ACK_FIRST_TIMEOUT_MS: u64 = 1_200;
const ACK_SECOND_TIMEOUT_MS: u64 = 700;
const RECOVERY_ACK_TIMEOUT_MS: u64 = 2_000;

/// 舊版輕量 ping/pong 仍保留，便於和歷史日誌對照。
static LAST_PONG_MS: AtomicI64 = AtomicI64::new(0);
static SHOW_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn record_overlay_pong(_seq: u64) {
    LAST_PONG_MS.store(now_ms(), Ordering::SeqCst);
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[derive(Debug, Clone, PartialEq)]
enum OverlayLayout {
    Base,
    BaseWithMicHint,
    Fallback,
    /// 識別失敗卡片：帶關閉按鈕，因此同樣需要互動。
    Failure,
    /// 流式即時顯示：氣泡 + 波形，視窗更大且非互動
    Streaming,
    StreamingWithMicHint,
}

impl OverlayLayout {
    /// 該佈局是否為可互動（可點選）狀態——只有帶按鈕的卡片需要。
    ///
    /// ⚠️ 非互動佈局會 `set_ignore_cursor_events(true)`，視窗整體點選穿透。
    /// 新增任何"有按鈕的"狀態都必須在這裡返回 true，否則按鈕在介面上看得見、點不到。
    fn is_interactive(&self) -> bool {
        matches!(self, OverlayLayout::Fallback | OverlayLayout::Failure)
    }

    /// 該佈局期望的設計尺寸（寬, 高），單位是 CSS px。
    ///
    /// 注意這**不是**可以直接交給 Tauri 的邏輯尺寸：webview 的 CSS px 可能被額外縮放
    /// （見 OVERLAY_CSS_ZOOM_MIN 的說明），要先乘 css_zoom 才是視窗該有的邏輯尺寸。
    fn dimensions(&self, base_width: f64) -> (f64, f64) {
        match self {
            OverlayLayout::Base => (base_width, OVERLAY_BASE_HEIGHT),
            OverlayLayout::BaseWithMicHint => (
                base_width.max(OVERLAY_MIC_HINT_WIDTH),
                OVERLAY_MIC_HINT_HEIGHT,
            ),
            OverlayLayout::Fallback => (OVERLAY_FALLBACK_WIDTH, OVERLAY_FALLBACK_HEIGHT),
            OverlayLayout::Failure => (OVERLAY_FAILURE_WIDTH, OVERLAY_FAILURE_HEIGHT),
            OverlayLayout::Streaming => (OVERLAY_STREAMING_WIDTH, OVERLAY_STREAMING_HEIGHT),
            OverlayLayout::StreamingWithMicHint => (
                OVERLAY_STREAMING_WIDTH,
                OVERLAY_STREAMING_MIC_HINT_HEIGHT,
            ),
        }
    }
}

pub struct WindowState {
    overlay_layout: Mutex<OverlayLayout>,
    /// 最近一次真正應用到原生視窗的佈局。用於跳過重複的 set_position/set_size，
    /// 避免監聽期間每 33ms 重設一次視窗幾何導致的抖動。
    last_applied_layout: Mutex<Option<OverlayLayout>>,
    overlay_base_width: Mutex<f64>,
    /// webview 裡一個 CSS px 相當於幾個「顯示器邏輯 px」。1.0 表示兩者一致。
    /// 由渲染端上報的 devicePixelRatio 除以視窗所在顯示器的 scale 得出，見 note_css_zoom。
    overlay_css_zoom: Mutex<f64>,
    overlay_monitor: Mutex<Option<MonitorBounds>>,
    overlay_lifecycle: Mutex<()>,
    latest_overlay_payload: Mutex<Option<Value>>,
    active_show_id: AtomicU64,
    active_generation: AtomicU64,
    show_started_at_ms: AtomicI64,
    last_ack_show_id: AtomicU64,
    last_ack_generation: AtomicU64,
    last_ack_at_ms: AtomicI64,
    recovery_started_show_id: AtomicU64,
    page_load_started_at_ms: AtomicI64,
    page_load_finished_at_ms: AtomicI64,
    renderer_ready_at_ms: AtomicI64,
    /// 程序/狀態建立時間，用於日誌記錄執行時長（配合資源計數排查“長時間執行後懸浮窗失效”）
    created_at: Instant,
}
impl WindowState {
    pub fn new() -> Self {
        Self {
            overlay_layout: Mutex::new(OverlayLayout::Base),
            last_applied_layout: Mutex::new(None),
            overlay_base_width: Mutex::new(OVERLAY_DEFAULT_BASE_WIDTH),
            overlay_css_zoom: Mutex::new(1.0),
            overlay_monitor: Mutex::new(None),
            overlay_lifecycle: Mutex::new(()),
            latest_overlay_payload: Mutex::new(None),
            active_show_id: AtomicU64::new(0),
            active_generation: AtomicU64::new(0),
            show_started_at_ms: AtomicI64::new(0),
            last_ack_show_id: AtomicU64::new(0),
            last_ack_generation: AtomicU64::new(0),
            last_ack_at_ms: AtomicI64::new(0),
            recovery_started_show_id: AtomicU64::new(0),
            page_load_started_at_ms: AtomicI64::new(0),
            page_load_finished_at_ms: AtomicI64::new(0),
            renderer_ready_at_ms: AtomicI64::new(0),
            created_at: Instant::now(),
        }
    }

    /// webview 裡 1 個 CSS px 相當於幾個顯示器邏輯 px。
    pub fn css_zoom(&self) -> f64 {
        *self.overlay_css_zoom.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 記錄渲染端上報的 devicePixelRatio，換算成 css_zoom 快取起來。
    ///
    /// 為什麼不直接快取 dpr：dpr 裡同時含了顯示器縮放和 webview 的額外縮放，只有除掉
    /// 視窗所在顯示器的 scale 才是需要補償的那一份。快取 dpr 會在兩塊不同縮放的顯示器
    /// 之間串味（懸浮窗跟著前台視窗跑，換顯示器是常態）。
    ///
    /// 返回值表示快取是否有實質變化，呼叫方據此決定要不要立刻重設視窗幾何。
    fn note_css_zoom(&self, app: &AppHandle, device_pixel_ratio: Option<f64>) -> bool {
        let Some(dpr) = device_pixel_ratio.filter(|value| value.is_finite() && *value > 0.0) else {
            return false;
        };
        let monitor_scale = app
            .get_webview_window("overlay")
            .and_then(|overlay| overlay.scale_factor().ok())
            .or_else(|| app.primary_monitor().ok().flatten().map(|m| m.scale_factor()))
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(1.0);
        let zoom = (dpr / monitor_scale).clamp(OVERLAY_CSS_ZOOM_MIN, OVERLAY_CSS_ZOOM_MAX);

        let previous = {
            let mut cached = self.overlay_css_zoom.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            // 0.01 的死區：dpr 與 scale 都是浮點，每次 present 都上報，末位抖動不該觸發
            // 重設幾何（重設會帶來一幀的位置/尺寸跳動，見 apply_payload_layout 的註釋）。
            if (zoom - *cached).abs() < 0.01 {
                return false;
            }
            let previous = *cached;
            *cached = zoom;
            previous
        };

        write_log_line(&format!(
            "[overlay-scale] css zoom {:.4} -> {:.4} dpr={} monitor_scale={}",
            previous, zoom, dpr, monitor_scale,
        ));
        true
    }

    /// 在應用啟動後的空閒時段提前建立隱藏的 overlay WebView。
    ///
    /// 第一次口述若等到熱鍵按下後才建立 WebView，頁面載入和 React 初始化會額外佔用
    /// 數百毫秒。預熱只把 renderer 準備好，不顯示視窗；真正 present 時仍會重新捕獲
    /// 當前前台顯示器、應用最終佈局並置頂。
    pub fn prewarm_overlay(&self, app: &AppHandle) {
        let _lifecycle_guard = self.overlay_lifecycle.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.active_show_id.load(Ordering::SeqCst) != 0
            || app.get_webview_window("overlay").is_some()
        {
            return;
        }

        let layout = self.overlay_layout.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        let base_width = *self.overlay_base_width.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        write_log_line("[overlay-health] idle prewarm begin");
        self.create_overlay(app, layout, base_width, false);
    }

    /// 原子地儲存狀態、顯示原生視窗並請求 Overlay 確認本次渲染。
    pub fn present_overlay(&self, app: &AppHandle, data: Value) -> u64 {
        self.apply_payload_layout(&data);
        self.capture_overlay_monitor();
        *self.latest_overlay_payload.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(data);

        let show_id = SHOW_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
        self.active_show_id.store(show_id, Ordering::SeqCst);
        self.active_generation.store(0, Ordering::SeqCst);
        self.show_started_at_ms.store(now_ms(), Ordering::SeqCst);
        self.last_ack_show_id.store(0, Ordering::SeqCst);
        self.last_ack_generation.store(0, Ordering::SeqCst);
        self.last_ack_at_ms.store(0, Ordering::SeqCst);
        self.recovery_started_show_id.store(0, Ordering::SeqCst);

        let state_name = self.latest_state_name();
        let (gdi, user_obj) = gui_resource_counts();
        let overlay_visible = app.get_webview_window("overlay").and_then(|o| o.is_visible().ok());
        write_log_line(&format!(
            "[overlay-health] present show_id={} state={} generation=0 uptimeSec={} gdi={} user={} overlayVisible={:?}",
            show_id, state_name, self.created_at.elapsed().as_secs(), gdi, user_obj, overlay_visible,
        ));

        if self.ensure_visible(app, show_id) {
            self.emit_latest(app, true);
        }
        spawn_render_watchdog(app.clone(), show_id);
        spawn_topmost_keeper(app.clone(), show_id);
        show_id
    }

    /// 相容舊呼叫點；新程式碼應使用 present_overlay，將顯示和狀態更新合成一次操作。
    pub fn show_overlay(&self, app: &AppHandle) -> u64 {
        let data = self.latest_overlay_payload.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
            // 最近一次狀態可能是 hide 時寫進去的 blank（它專門用來清掉殘留幀，
            // 見 hide_overlay）。照它重放會顯示一個空視窗，所以當沒有狀態處理。
            .filter(|payload| payload.get("state").and_then(Value::as_str) != Some("blank"))
            .unwrap_or_else(|| json!({ "state": "waiting", "elapsedSec": 0 }));
        self.present_overlay(app, data)
    }

    pub fn hide_overlay(&self, app: &AppHandle) {
        self.active_show_id.store(0, Ordering::SeqCst);
        *self.overlay_monitor.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        // 下次顯示需要重新應用幾何。
        *self.last_applied_layout.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;

        let prev_layout = {
            let mut layout = self.overlay_layout.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let previous = layout.clone();
            *layout = OverlayLayout::Base;
            previous
        };

        if let Some(overlay) = app.get_webview_window("overlay") {
            // 點選穿透立刻恢復，不等下面那段延遲 —— 否則 fallback 卡片這 80ms 裡還會擋滑鼠。
            set_overlay_interactivity(&overlay, false);

            // ── 先把內容清空，再隱藏視窗 ──
            //
            // 隱藏只是 hide()，WebView 不銷燬、Overlay 元件也不解除安裝，合成器裡留著的最後
            // 一幀就是上一條提示。下次 present 時視窗先顯示、新內容要等 IPC + setState +
            // 重繪才到，於是先閃一下**上一次**的文案（使用者報的原話：切換潤色模式後再按
            // 開關 AI 整理，懸浮窗會先顯示上一次的提示內容）。
            //
            // 關鍵是**趁視窗還看得見的時候**重繪成空：視窗一旦 hide，document 變
            // hidden，rAF 被節流，還能不能出一幀就不好說了。所以順序必須是
            // 「emit blank → 等一小會兒讓它畫出來 → hide」。
            //
            // 刻意不用 render-ack 把 hide 卡住（雖然那樣更精確）：ack 走雙層 rAF，
            // 一旦某次沒回來，視窗就永遠留在螢幕上 —— 那比閃一下舊內容糟得多。
            // 固定延遲的失敗模式只是"這次沒來得及畫完，退回原來的表現"，不會更壞。
            *self.latest_overlay_payload.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(json!({ "state": "blank" }));
            self.emit_latest(app, false);
            write_log_line(&format!(
                "[overlay-health] blank before hide prev_layout={:?} delay_ms={}",
                prev_layout, HIDE_AFTER_BLANK_MS,
            ));
            spawn_deferred_hide(app.clone(), prev_layout);
        }
    }

    pub fn update_overlay_state(&self, app: &AppHandle, data: &Value) {
        self.apply_payload_layout(data);
        *self.latest_overlay_payload.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(data.clone());

        if let Some(overlay) = app.get_webview_window("overlay") {
            let layout = self.overlay_layout.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
            // 僅在佈局真正變化時才重設原生視窗幾何，避免每幀 set_position/set_size 抖動。
            let previous = self.last_applied_layout.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
            let changed = previous.as_ref() != Some(&layout);
            if changed {
                // 錄音期間唯一會改視窗幾何的地方，必須留痕：這條路徑以前一行日誌都不寫，
                // 於是「懸浮窗閃」這類幾何抖動在 sayit.log 裡完全無跡可尋，只能靠音量警告
                // 的時間戳反推。一次錄音正常只有 1~2 行；同一對 from→to 反覆出現就是抖動。
                write_log_line(&format!(
                    "[overlay-layout] {:?} -> {:?} state={} streaming={} mic_hint={}",
                    previous,
                    layout,
                    data.get("state").and_then(Value::as_str).unwrap_or("unknown"),
                    data.get("streaming").and_then(Value::as_bool).unwrap_or(false),
                    data.get("micSourceLabel")
                        .and_then(Value::as_str)
                        .map(|s| !s.trim().is_empty())
                        .unwrap_or(false),
                ));
                // 卡片類佈局可能是從"已隱藏"直接切過來的（比如處理中報錯 → 失敗卡），
                // 這時視窗還沒顯示，光改幾何是看不見的。
                let is_card = layout.is_interactive();
                self.apply_native_layout(app, &overlay, &layout);
                set_overlay_interactivity(&overlay, layout.is_interactive());
                if is_card {
                    let _ = overlay.show();
                }
            }
            self.emit_latest(app, false);
        }
    }

    /// Overlay 頁面註冊完事件監聽後呼叫。新建 WebView 時，第一次狀態事件可能早於
    /// React listener，因此在 ready 時重放最近狀態並重新請求確認。
    pub fn overlay_ready(&self, app: &AppHandle, device_pixel_ratio: Option<f64>) {
        let show_id = self.active_show_id.load(Ordering::SeqCst);
        self.renderer_ready_at_ms.store(now_ms(), Ordering::SeqCst);
        // 必須在下面 show_id == 0 的早退之前記：預熱（show_id == 0）就是我們唯一能在
        // 第一次真正顯示之前拿到 dpr 的時機。錯過它，使用者第一次口述就會看到被裁的膠囊。
        self.note_css_zoom(app, device_pixel_ratio);
        if show_id == 0 {
            if let Some(overlay) = app.get_webview_window("overlay") {
                let _ = overlay.hide();
            }
            return;
        }

        write_log_line(&format!(
            "[overlay-health] renderer ready show_id={} generation={} diagnostic={}",
            show_id,
            self.active_generation.load(Ordering::SeqCst),
            compact_json(&self.diagnostic_snapshot(app)),
        ));
        if self.ensure_visible(app, show_id) {
            self.emit_latest(app, true);
        }
    }

    pub fn record_render_ack(&self, app: &AppHandle, data: &Value) {
        let show_id = data.get("showId").and_then(Value::as_u64).unwrap_or(0);
        let generation = data.get("generation").and_then(Value::as_u64).unwrap_or(0);
        let healthy = data.get("healthy").and_then(Value::as_bool).unwrap_or(false);
        let active_show_id = self.active_show_id.load(Ordering::SeqCst);
        let active_generation = self.active_generation.load(Ordering::SeqCst);

        if show_id != active_show_id || generation != active_generation {
            write_log_line(&format!(
                "[overlay-health] stale ack show_id={} generation={} active_show_id={} active_generation={}",
                show_id, generation, active_show_id, active_generation,
            ));
            return;
        }

        // 先學縮放：這一輪的視窗已經按舊 css_zoom 建好了，學到新值就地重設幾何，讓**當前**
        // 這次顯示也能被糾正，而不是隻讓下一次受益（使用者按一次熱鍵只看到這一次）。
        if self.note_css_zoom(app, data.get("devicePixelRatio").and_then(Value::as_f64)) {
            let layout = self.overlay_layout.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
            if let Some(overlay) = app.get_webview_window("overlay") {
                self.apply_native_layout(app, &overlay, &layout);
            }
        }
        self.warn_if_content_clipped(data, show_id);

        if !healthy {
            write_log_line(&format!(
                "[overlay-health] unhealthy render ack show_id={} generation={} detail={}",
                show_id, generation, compact_json(data),
            ));
            return;
        }

        self.last_ack_show_id.store(show_id, Ordering::SeqCst);
        self.last_ack_generation.store(generation, Ordering::SeqCst);
        self.last_ack_at_ms.store(now_ms(), Ordering::SeqCst);
        write_log_line(&format!(
            "[overlay-health] render ack OK show_id={} generation={} latency_ms={} state={} content={}x{} visibility={}",
            show_id,
            generation,
            self.ack_latency_ms(),
            data.get("overlayState").and_then(Value::as_str).unwrap_or("unknown"),
            data.get("contentWidth").and_then(Value::as_f64).unwrap_or(0.0),
            data.get("contentHeight").and_then(Value::as_f64).unwrap_or(0.0),
            data.get("documentVisibility").and_then(Value::as_str).unwrap_or("unknown"),
        ));
    }

    /// 內容裝不進視窗時留一行日誌。
    ///
    /// 為什麼單獨一條：render ack 的 `healthy` 只查「寬高 > 0、沒被 display:none/visibility
    /// 隱藏」，被裁成一半照樣是 healthy。2026-09 那次「膠囊頂部被裁」全程 17 次顯示都報
    /// `render ack OK`，只能靠使用者截圖發現——這類問題必須在日誌裡能自證。
    fn warn_if_content_clipped(&self, data: &Value, show_id: u64) {
        let number = |key: &str| data.get(key).and_then(Value::as_f64).unwrap_or(0.0);
        // 兩個判據都要：clippedTop 是渲染端實測的越界量（最直接），shortfall 是拿視口高度
        // 反算的缺口（老版本前端不上報 clippedTop 時的兜底，也能反過來印證前者）。
        let clipped_top = number("clippedTop");
        let viewport_height = number("viewportHeight");
        let shortfall = if viewport_height > 0.0 {
            number("contentHeight") + OVERLAY_ROOT_PADDING_BOTTOM - viewport_height
        } else {
            0.0
        };
        // 0.5 CSS px 以下是裝置畫素取整的正常抖動，不算裁切。
        if clipped_top <= 0.5 && shortfall <= 0.5 {
            return;
        }
        write_log_line(&format!(
            "[overlay-scale] content clipped show_id={} state={} clippedTopPx={:.2} shortfallPx={:.2} dpr={} cssZoom={:.4} viewport={}x{} content={}x{}",
            show_id,
            data.get("overlayState").and_then(Value::as_str).unwrap_or("unknown"),
            clipped_top,
            shortfall,
            number("devicePixelRatio"),
            self.css_zoom(),
            number("viewportWidth"),
            viewport_height,
            number("contentWidth"),
            number("contentHeight"),
        ));
    }

    pub fn health_snapshot(&self, app: &AppHandle, show_id: u64) -> Value {
        let active_show_id = self.active_show_id.load(Ordering::SeqCst);
        let active_generation = self.active_generation.load(Ordering::SeqCst);
        let ack_show_id = self.last_ack_show_id.load(Ordering::SeqCst);
        let ack_generation = self.last_ack_generation.load(Ordering::SeqCst);
        let recovery_started = self.recovery_started_show_id.load(Ordering::SeqCst) == show_id;
        let diagnostic = self.diagnostic_snapshot(app);
        json!({
            "showId": show_id,
            "activeShowId": active_show_id,
            "activeGeneration": active_generation,
            "acked": ack_show_id == show_id && ack_generation == active_generation,
            "ackGeneration": ack_generation,
            "ackLatencyMs": if ack_show_id == show_id { self.ack_latency_ms() } else { -1 },
            "recoveryStarted": recovery_started,
            "recoverySucceeded": recovery_started && ack_show_id == show_id && ack_generation > 0,
            "failureClass": self.failure_class(app),
            // Keep the existing frontend response contract; `diagnostic` adds detail.
            "window": overlay_window_snapshot(app),
            "diagnostic": diagnostic,
        })
    }

    fn reset_renderer_lifecycle(&self) {
        self.page_load_started_at_ms.store(0, Ordering::SeqCst);
        self.page_load_finished_at_ms.store(0, Ordering::SeqCst);
        self.renderer_ready_at_ms.store(0, Ordering::SeqCst);
    }

    fn record_page_load(&self, event: PageLoadEvent, url: &str) {
        let timestamp = now_ms();
        let event_name = match event {
            PageLoadEvent::Started => {
                self.page_load_started_at_ms.store(timestamp, Ordering::SeqCst);
                "started"
            }
            PageLoadEvent::Finished => {
                self.page_load_finished_at_ms.store(timestamp, Ordering::SeqCst);
                "finished"
            }
        };
        write_log_line(&format!(
            "[overlay-health] page load {} show_id={} generation={} url={}",
            event_name,
            self.active_show_id.load(Ordering::SeqCst),
            self.active_generation.load(Ordering::SeqCst),
            url,
        ));
    }

    fn diagnostic_snapshot(&self, app: &AppHandle) -> Value {
        let page_started_at = self.page_load_started_at_ms.load(Ordering::SeqCst);
        let page_finished_at = self.page_load_finished_at_ms.load(Ordering::SeqCst);
        let renderer_ready_at = self.renderer_ready_at_ms.load(Ordering::SeqCst);
        let current_ms = now_ms();
        json!({
            "window": overlay_window_snapshot(app),
            "pageLoadStarted": page_started_at > 0,
            "pageLoadFinished": page_finished_at > 0,
            "rendererReady": renderer_ready_at > 0,
            "pageLoadStartedAgeMs": event_age_ms(current_ms, page_started_at),
            "pageLoadFinishedAgeMs": event_age_ms(current_ms, page_finished_at),
            "rendererReadyAgeMs": event_age_ms(current_ms, renderer_ready_at),
        })
    }

    fn failure_class(&self, app: &AppHandle) -> &'static str {
        classify_overlay_failure(
            &overlay_window_snapshot(app),
            self.page_load_started_at_ms.load(Ordering::SeqCst) > 0,
            self.page_load_finished_at_ms.load(Ordering::SeqCst) > 0,
            self.renderer_ready_at_ms.load(Ordering::SeqCst) > 0,
        )
    }

    fn ack_latency_ms(&self) -> i64 {
        let ack_at = self.last_ack_at_ms.load(Ordering::SeqCst);
        let started_at = self.show_started_at_ms.load(Ordering::SeqCst);
        if ack_at <= 0 || started_at <= 0 { -1 } else { ack_at - started_at }
    }

    fn is_acked(&self, show_id: u64, generation: u64) -> bool {
        self.last_ack_show_id.load(Ordering::SeqCst) == show_id
            && self.last_ack_generation.load(Ordering::SeqCst) == generation
    }

    /// 本次 show_id 是否仍是當前正在顯示的（不區分 generation，供置頂守護執行緒判斷是否繼續）。
    fn is_showing(&self, show_id: u64) -> bool {
        show_id != 0 && self.active_show_id.load(Ordering::SeqCst) == show_id
    }

    /// 懸浮窗正顯示時，把它重新頂到 topmost 帶最上層。供前台視窗切換鉤子呼叫，
    /// 讓使用者切到/開啟別的置頂程式時，懸浮窗立即回到最上面。
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn overlay_visible(&self) -> bool {
        self.active_show_id.load(Ordering::SeqCst) != 0
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn reassert_overlay_topmost_if_visible(&self, app: &AppHandle) {
        if !self.overlay_visible() {
            return;
        }
        reassert_overlay_topmost(app);
    }

    fn is_active(&self, show_id: u64, generation: u64) -> bool {
        self.active_show_id.load(Ordering::SeqCst) == show_id
            && self.active_generation.load(Ordering::SeqCst) == generation
    }

    fn capture_overlay_monitor(&self) {
        let monitor = capture_foreground_monitor();
        if let Some(value) = &monitor {
            write_log_line(&format!(
                "[overlay-position] target_monitor source={} work=({},{},{},{})",
                value.source, value.left, value.top, value.right, value.bottom,
            ));
        }
        *self.overlay_monitor.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = monitor;
    }

    fn apply_payload_layout(&self, data: &Value) {
        if let Some(width) = data.get("baseWidth").and_then(Value::as_f64) {
            if (160.0..=600.0).contains(&width) {
                *self.overlay_base_width.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = width;
            }
        }

        let state = data.get("state").and_then(Value::as_str);
        // streaming 標誌在錄音開始時即為真（氣泡從一開始就在，避免中途彈出+縮放）；
        // 相容舊邏輯：有 streamingText 也算。
        let streaming_on = data.get("streaming").and_then(Value::as_bool).unwrap_or(false)
            || data
                .get("streamingText")
                .and_then(Value::as_str)
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
        let mic_hint_on = data
            .get("micSourceLabel")
            .and_then(Value::as_str)
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        let candidate_layout = if state == Some("fallback") {
            OverlayLayout::Fallback
        } else if state == Some("failure") {
            OverlayLayout::Failure
        } else if state == Some("listening") && streaming_on && mic_hint_on {
            OverlayLayout::StreamingWithMicHint
        } else if state == Some("listening") && streaming_on {
            // 錄音中開啟了即時顯示 → 放大視窗容納氣泡（整段錄音保持該尺寸，中途不再縮放）
            OverlayLayout::Streaming
        } else if matches!(state, Some("listening") | Some("thinking")) && mic_hint_on {
            OverlayLayout::BaseWithMicHint
        } else {
            OverlayLayout::Base
        };

        let mut current_layout = self.overlay_layout.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let stable_visible_phase = matches!(
            state,
            Some("listening") | Some("thinking") | Some("toast") | Some("error")
        );
        let keep_expanded_bounds = stable_visible_phase
            && matches!(
                (&*current_layout, &candidate_layout),
                (OverlayLayout::BaseWithMicHint, OverlayLayout::Base)
                    | (OverlayLayout::StreamingWithMicHint, OverlayLayout::Streaming)
                    | (OverlayLayout::StreamingWithMicHint, OverlayLayout::BaseWithMicHint)
                    | (OverlayLayout::StreamingWithMicHint, OverlayLayout::Base)
                    // Streaming 這兩支是 2026-09 補的。前端曾有五個警告方法各自手寫
                    // payload、都漏了 `streaming` 欄位，於是低音量警告每 5 秒把佈局判成
                    // Base，視窗收縮一幀再被下一次心跳撐回去——開著即時字幕不說話時
                    // 懸浮窗"一閃一閃"。前端已收口到 listeningPayload()，這裡是兜底：
                    // 哪怕將來又有新出口漏帶欄位，視窗也不該在可見期間縮回去。
                    | (OverlayLayout::Streaming, OverlayLayout::Base)
                    | (OverlayLayout::Streaming, OverlayLayout::BaseWithMicHint)
            );

        // 提示文字 3 秒後會隱藏，無語音/錯誤也會把內容換成短 toast，但只要懸浮窗
        // 仍可見就不能順帶收縮原生視窗：視窗按底邊錨定，收縮需要重新定位，WebView2
        // 會把兩次幾何更新之間的中間幀顯示成下跳。保留透明上方空間，直到 hide 或
        // 下一次 waiting 明確開始新一輪顯示時再恢復基礎尺寸。
        if !keep_expanded_bounds {
            *current_layout = candidate_layout;
        }
    }

    fn latest_state_name(&self) -> String {
        self.latest_overlay_payload.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_ref()
            .and_then(|payload| payload.get("state"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string()
    }

    fn payload_for_emit(&self, probe: bool) -> Option<Value> {
        let mut payload = self.latest_overlay_payload.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()?;
        let show_id = self.active_show_id.load(Ordering::SeqCst);
        let generation = self.active_generation.load(Ordering::SeqCst);
        let object = payload.as_object_mut()?;
        object.insert("_overlayShowId".to_string(), json!(show_id));
        object.insert("_overlayGeneration".to_string(), json!(generation));
        object.insert("_overlayProbe".to_string(), json!(probe));
        Some(payload)
    }

    fn emit_latest(&self, app: &AppHandle, probe: bool) {
        let Some(overlay) = app.get_webview_window("overlay") else { return; };
        let Some(payload) = self.payload_for_emit(probe) else { return; };
        if let Err(error) = overlay.emit("overlay-state", payload) {
            write_log_line(&format!(
                "[overlay-health] emit FAILED show_id={} generation={} error={:?}",
                self.active_show_id.load(Ordering::SeqCst),
                self.active_generation.load(Ordering::SeqCst),
                error,
            ));
        }
    }

    fn ensure_visible(&self, app: &AppHandle, show_id: u64) -> bool {
        // WebviewWindowBuilder 的 label 註冊與 Manager 控制代碼可見性不是原子操作。
        // 多個 present 併發時若都看到 handle=MISSING，會爭搶同一 "overlay" label。
        let _lifecycle_guard = self.overlay_lifecycle.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let layout = self.overlay_layout.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        let base_width = *self.overlay_base_width.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        let Some(overlay) = app.get_webview_window("overlay") else {
            write_log_line(&format!(
                "[overlay-health] handle missing show_id={} — creating",
                show_id,
            ));
            self.create_overlay(app, layout, base_width, true);
            return false;
        };

        let position_error = self.apply_native_layout(app, &overlay, &layout);
        let show_error = show_overlay_window(&overlay).err();
        #[cfg(not(target_os = "macos"))]
        let top_error = overlay.set_always_on_top(true).err().map(|error| format!("{:?}", error));
        // macOS 不調 set_always_on_top：tao 會把層級非同步改回 NSFloatingWindowLevel（實測層級 5），
        // 和下面 reassert 的主執行緒回撥誰先執行沒有保證；層級統一由 reassert 設成 NSStatusWindowLevel。
        #[cfg(target_os = "macos")]
        let top_error: Option<String> = None;
        // set_always_on_top(true) 在狀態未變時可能是 no-op，這裡再用原生 SetWindowPos 強制
        // 把視窗頂到 topmost 帶最上層，確保即使下方已有別的置頂視窗也能壓過它。
        reassert_overlay_topmost(app);
        set_overlay_interactivity(&overlay, layout.is_interactive());

        let snapshot = overlay_window_snapshot(app);
        if position_error.is_some() || show_error.is_some() || top_error.is_some() {
            write_log_line(&format!(
                "[overlay-health] native show FAILED show_id={} position_error={:?} show_error={:?} top_error={:?} snapshot={}",
                show_id, position_error, show_error, top_error, compact_json(&snapshot),
            ));
        } else {
            write_log_line(&format!(
                "[overlay-health] native show OK show_id={} snapshot={}",
                show_id, compact_json(&snapshot),
            ));
        }
        true
    }

    fn apply_native_layout(
        &self,
        app: &AppHandle,
        overlay: &tauri::WebviewWindow,
        layout: &OverlayLayout,
    ) -> Option<String> {
        // 記錄本次應用的佈局，供 update_overlay_state 判重、跳過無謂的視窗重設。
        *self.last_applied_layout.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(layout.clone());
        let base_width = *self.overlay_base_width.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let css_zoom = self.css_zoom();
        let target_monitor = self.overlay_monitor.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();

        // macOS：各顯示器縮放可以不同，物理畫素不能跨屏比較，全程按邏輯座標算。
        #[cfg(target_os = "macos")]
        {
            let bounds = macos_overlay_bounds(app, target_monitor.as_ref(), layout, base_width, css_zoom);
            let position_error = overlay.set_position(tauri::Position::Logical(
                tauri::LogicalPosition::new(bounds.0, bounds.1),
            )).err();
            let size_error = overlay.set_size(tauri::Size::Logical(
                tauri::LogicalSize::new(bounds.2, bounds.3),
            )).err();
            // 不調 set_always_on_top：它會把層級改回 NSFloatingWindowLevel，層級交給 reassert_overlay_topmost
            return match (position_error, size_error) {
                (None, None) => None,
                (position, size) => Some(format!("position={:?} size={:?}", position, size)),
            };
        }

        #[cfg(not(target_os = "macos"))]
        if let Some(bounds) = target_monitor
            .as_ref()
            .and_then(|value| calc_monitor_overlay_bounds(app, value, layout, base_width, css_zoom))
        {
            let position_error = overlay.set_position(tauri::Position::Physical(
                tauri::PhysicalPosition::new(bounds.0, bounds.1),
            )).err();
            let size_error = overlay.set_size(tauri::Size::Physical(
                tauri::PhysicalSize::new(bounds.2, bounds.3),
            )).err();
            let _ = overlay.set_always_on_top(true);

            return match (position_error, size_error) {
                (None, None) => None,
                (position, size) => Some(format!("position={:?} size={:?}", position, size)),
            };
        }

        #[cfg(not(target_os = "macos"))]
        {
            let bounds = calc_overlay_bounds(app, layout, base_width, css_zoom);
            let position_error = overlay.set_position(tauri::Position::Logical(
                tauri::LogicalPosition::new(bounds.0, bounds.1),
            )).err();
            let size_error = overlay.set_size(tauri::Size::Logical(
                tauri::LogicalSize::new(bounds.2, bounds.3),
            )).err();
            let _ = overlay.set_always_on_top(true);

            match (position_error, size_error) {
                (None, None) => None,
                (position, size) => Some(format!("position={:?} size={:?}", position, size)),
            }
        }
    }

    fn recover_overlay(&self, app: &AppHandle, show_id: u64) {
        let _lifecycle_guard = self.overlay_lifecycle.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // 等鎖期間可能已經 hide 或開始了下一次顯示，必須重新校驗。
        if !self.is_active(show_id, 0) {
            return;
        }

        self.active_generation.store(1, Ordering::SeqCst);
        self.last_ack_show_id.store(0, Ordering::SeqCst);
        self.last_ack_generation.store(0, Ordering::SeqCst);
        self.recovery_started_show_id.store(show_id, Ordering::SeqCst);
        write_log_line(&format!(
            "[overlay-health] recovery begin show_id={} failure_class={} diagnostic={}",
            show_id,
            self.failure_class(app),
            compact_json(&self.diagnostic_snapshot(app)),
        ));

        if let Some(overlay) = app.get_webview_window("overlay") {
            if let Err(error) = overlay.destroy() {
                write_log_line(&format!(
                    "[overlay-health] destroy FAILED show_id={} error={:?}",
                    show_id, error,
                ));
            }
        }

        for _ in 0..20 {
            if app.get_webview_window("overlay").is_none() {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }

        if app.get_webview_window("overlay").is_some() {
            write_log_line(&format!(
                "[overlay-health] recovery FAILED show_id={} reason=stale_handle diagnostic={}",
                show_id,
                compact_json(&self.diagnostic_snapshot(app)),
            ));
            return;
        }

        let layout = self.overlay_layout.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        let base_width = *self.overlay_base_width.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.create_overlay(app, layout, base_width, true);
    }

    fn create_overlay(
        &self,
        app: &AppHandle,
        layout: OverlayLayout,
        base_width: f64,
        show_immediately: bool,
    ) {
        self.reset_renderer_lifecycle();
        let bounds = calc_overlay_bounds(app, &layout, base_width, self.css_zoom());
        let builder = WebviewWindowBuilder::new(
            app,
            "overlay",
            WebviewUrl::App("overlay.html".into()),
        )
        .title("SayItLocally Overlay")
        .inner_size(bounds.2, bounds.3)
        .position(bounds.0, bounds.1)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .focused(false)
        .visible(false)
        .on_page_load(|window, payload| {
            let state = window.app_handle().state::<WindowState>();
            state.record_page_load(payload.event(), payload.url().as_str());
        });

        // [自訂] L-80：錄音或辨識中的懸浮窗不可取得鍵盤焦點。
        #[cfg(target_os = "macos")]
        let builder = builder.focusable(layout.is_interactive());

        match builder.build() {
            Ok(overlay) => {
                let position_error = self.apply_native_layout(app, &overlay, &layout);
                set_overlay_interactivity(&overlay, layout.is_interactive());
                // macOS：一建出來就設好懸浮層屬性（層級、跨桌面空間、失活不隱藏）
                #[cfg(target_os = "macos")]
                reassert_overlay_topmost(app);
                let show_error = if show_immediately {
                    show_overlay_window(&overlay).err()
                } else {
                    None
                };
                write_log_line(&format!(
                    "[overlay-health] create dispatched show_id={} generation={} prewarm={} initial_bounds={:?} position_error={:?} show_error={:?} shell_visible={} diagnostic={}",
                    self.active_show_id.load(Ordering::SeqCst),
                    self.active_generation.load(Ordering::SeqCst),
                    !show_immediately,
                    bounds,
                    position_error,
                    show_error,
                    overlay.is_visible().unwrap_or(false),
                    compact_json(&self.diagnostic_snapshot(app)),
                ));
            }
            Err(error) => {
                write_log_line(&format!(
                    "[overlay-health] create FAILED show_id={} generation={} bounds={:?} error={:?}",
                    self.active_show_id.load(Ordering::SeqCst),
                    self.active_generation.load(Ordering::SeqCst),
                    bounds,
                    error,
                ));
            }
        }
    }
}
/// 把懸浮窗重新頂到 topmost 帶的最上層。
///
/// 為什麼需要：`WS_EX_TOPMOST` 只保證在普通視窗之上；多個 topmost 視窗之間，誰最後
/// 被顯示/啟用/SetWindowPos 誰就在上面。懸浮窗只在顯示那一刻搶了一次置頂，之後若有
/// 別的 topmost 視窗（PotPlayer「總在最前」、會議共享工具條、PowerToys 置頂窗等）出現
/// 或被重新啟用，就會蓋住它。這裡用原生 `SetWindowPos(HWND_TOPMOST)` 主動把自己重新頂上去。
///
/// 關鍵：
/// - 帶 `SWP_NOACTIVATE`，絕不搶焦點（懸浮窗設計上永不啟用）。
/// - 直接用原生 SetWindowPos，而不是 Tauri 的 `set_always_on_top(true)`——後者在狀態未變
///   (true→true) 時可能是 no-op，不會真正下發 SetWindowPos，起不到重新抬升的作用。
#[cfg(windows)]
fn reassert_overlay_topmost(app: &AppHandle) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };

    let Some(overlay) = app.get_webview_window("overlay") else { return; };
    // 透過 isize 中轉重建 HWND，規避 tauri 與本 crate 各自 windows 版本的 HWND 型別不一致。
    let hwnd_raw = match overlay.hwnd() {
        Ok(h) => h.0 as isize,
        Err(_) => return,
    };
    if hwnd_raw == 0 {
        return;
    }
    let hwnd = HWND(hwnd_raw as *mut _);
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

/// macOS：重設懸浮層屬性（`set_always_on_top` 會把層級改回普通置頂），並在可見時
/// 不啟用地放到最前。NSWindow 的方法只能在主執行緒呼叫。
#[cfg(target_os = "macos")]
fn reassert_overlay_topmost(app: &AppHandle) {
    let Some(overlay) = app.get_webview_window("overlay") else { return; };
    let window = overlay.clone();
    let _ = overlay.run_on_main_thread(move || {
        if let Ok(ns_window) = window.ns_window() {
            unsafe {
                crate::platform::macos::appkit::configure_overlay_window(ns_window);
                if crate::platform::macos::appkit::window_is_visible(ns_window) {
                    crate::platform::macos::appkit::order_front_regardless(ns_window);
                }
            }
        }
    });
}

#[cfg(not(any(windows, target_os = "macos")))]
fn reassert_overlay_topmost(_app: &AppHandle) {}

/// 顯示懸浮窗且不搶焦點。
///
/// Windows：`show()` + 後面的 SetWindowPos(SWP_NOACTIVATE)（見 reassert_overlay_topmost）。
/// macOS：`show()` 會走 makeKeyAndOrderFront 把它設成 key window，這裡改用
/// `orderFrontRegardless`，不改變鍵盤焦點。
fn show_overlay_window(overlay: &tauri::WebviewWindow) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let window = overlay.clone();
        overlay
            .run_on_main_thread(move || {
                if let Ok(ns_window) = window.ns_window() {
                    unsafe {
                        crate::platform::macos::appkit::configure_overlay_window(ns_window);
                        crate::platform::macos::appkit::order_front_regardless(ns_window);
                    }
                }
            })
            .map_err(|error| format!("{:?}", error))
    }
    #[cfg(not(target_os = "macos"))]
    {
        overlay.show().map_err(|error| format!("{:?}", error))
    }
}

/// 主窗網頁程序崩潰後的自動過載次數記錄（時間戳，毫秒）。
#[cfg_attr(not(windows), allow(dead_code))]
static MAIN_RENDERER_RELOADS: Mutex<Vec<i64>> = Mutex::new(Vec::new());
#[cfg_attr(not(windows), allow(dead_code))]
const MAIN_RENDERER_RELOAD_WINDOW_MS: i64 = 5 * 60 * 1000;
#[cfg_attr(not(windows), allow(dead_code))]
const MAIN_RENDERER_RELOAD_MAX: usize = 3;

/// 這次崩潰還能不能自動過載：5 分鐘內最多 3 次，超過就停手，避免反覆崩潰時無限過載。
#[cfg_attr(not(windows), allow(dead_code))]
pub fn main_renderer_reload_allowed(now_ms: i64) -> bool {
    let mut history = MAIN_RENDERER_RELOADS.lock().unwrap_or_else(|p| p.into_inner());
    reload_allowed_in_window(&mut history, now_ms, MAIN_RENDERER_RELOAD_WINDOW_MS, MAIN_RENDERER_RELOAD_MAX)
}

#[cfg_attr(not(windows), allow(dead_code))]
fn reload_allowed_in_window(history: &mut Vec<i64>, now_ms: i64, window_ms: i64, max: usize) -> bool {
    history.retain(|t| now_ms - *t < window_ms);
    if history.len() >= max {
        return false;
    }
    history.push(now_ms);
    true
}

/// 主窗藏到托盤時告訴 WebView2「記憶體可以少用」，再顯示時恢復。
///
/// 主窗藏著的時間遠多於開著的時間（常駐托盤 + 開機自啟），而它的渲染程序是 SayItLocally 記憶體的大頭。
/// 只是記憶體目標，不停 JS：錄音、WebSocket 都跑在這個程序裡，必須照常工作（所以不用 TrySuspend）。
/// 執行時太舊（沒有 ICoreWebView2_19）就什麼也不做。
pub fn set_main_window_memory_low(window: &tauri::WebviewWindow, low: bool) {
    #[cfg(windows)]
    {
        let _ = window.with_webview(move |webview| {
            use webview2_com::Microsoft::Web::WebView2::Win32::{
                ICoreWebView2_19, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW,
                COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL,
            };
            use windows_core::Interface;
            unsafe {
                let Ok(core) = webview.controller().CoreWebView2() else { return };
                let Ok(core19) = core.cast::<ICoreWebView2_19>() else { return };
                let level = if low {
                    COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW
                } else {
                    COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL
                };
                if let Err(e) = core19.SetMemoryUsageTargetLevel(level) {
                    log::warn!("[memory] SetMemoryUsageTargetLevel failed: {e:?}");
                } else {
                    log::info!("[memory] main window memory target {}", if low { "low" } else { "normal" });
                }
            }
        });
    }
    #[cfg(not(windows))]
    let _ = (window, low);
}

/// macOS：主視窗藏起來時把 Dock 圖示也收起（只留選單欄圖示，像常駐工具一樣），
/// 主視窗顯示時再放回來。Windows 上什麼都不做（工作列按鈕跟著視窗走）。
pub fn set_dock_icon_visible(app: &AppHandle, visible: bool) {
    #[cfg(target_os = "macos")]
    {
        let policy = if visible {
            tauri::ActivationPolicy::Regular
        } else {
            tauri::ActivationPolicy::Accessory
        };
        let _ = app.set_activation_policy(policy);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, visible);
}

/// 顯示主窗的統一入口：恢復記憶體目標、show、解除最小化、搶焦點。
/// 只 show 不 unminimize 會讓以最小化狀態誕生的視窗停在螢幕外（pitfalls #11）。
pub fn show_main_window(window: &tauri::WebviewWindow) {
    set_dock_icon_visible(window.app_handle(), true);
    set_main_window_memory_low(window, false);
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
}

/// 主窗藏到托盤的統一入口。
pub fn hide_main_window(window: &tauri::Window) {
    let _ = window.hide();
    set_dock_icon_visible(window.app_handle(), false);
    if let Some(webview_window) = window.app_handle().get_webview_window(window.label()) {
        set_main_window_memory_low(&webview_window, true);
    }
}

/// 懸浮窗顯示期間，週期性地把它重新頂到最上層，兜住「顯示後才出現/重申置頂的視窗」。
/// 只在本次 show_id 仍是當前顯示時執行；一旦切到下一次顯示或隱藏（active_show_id 變化）即退出。
fn spawn_topmost_keeper(app: AppHandle, show_id: u64) {
    let _ = thread::Builder::new()
        .name(format!("overlay-topmost-{}", show_id))
        .spawn(move || {
            let state = app.state::<WindowState>();
            // 首次快速重申一次（覆蓋「懸浮窗彈出時下方已有 PotPlayer 等置頂窗」的場景）。
            reassert_overlay_topmost(&app);
            loop {
                thread::sleep(Duration::from_millis(400));
                if !state.is_showing(show_id) {
                    return;
                }
                reassert_overlay_topmost(&app);
            }
        });
}

/// 真正執行 `overlay.hide()`，延後 `HIDE_AFTER_BLANK_MS` 給 webview 一幀時間畫成空白。
///
/// 中途若有新的 present 開始（`active_show_id` 不再是 0），就**放棄這次隱藏** ——
/// 那時視窗已經帶著新內容顯示出來了，再 hide 會把它吞掉。
fn spawn_deferred_hide(app: AppHandle, prev_layout: OverlayLayout) {
    let _ = thread::Builder::new()
        .name("overlay-deferred-hide".to_string())
        .spawn(move || {
            thread::sleep(Duration::from_millis(HIDE_AFTER_BLANK_MS));
            let state = app.state::<WindowState>();
            if state.active_show_id.load(Ordering::SeqCst) != 0 {
                write_log_line(
                    "[overlay-health] deferred hide skipped — a new present already started",
                );
                return;
            }
            let Some(overlay) = app.get_webview_window("overlay") else { return; };
            if let Err(error) = overlay.hide() {
                write_log_line(&format!(
                    "[overlay-diag] hide FAILED prev_layout={:?} hide_err={:?}",
                    prev_layout, error,
                ));
            }
        });
}

fn spawn_render_watchdog(app: AppHandle, show_id: u64) {
    let _ = thread::Builder::new()
        .name(format!("overlay-watchdog-{}", show_id))
        .spawn(move || {
            thread::sleep(Duration::from_millis(ACK_FIRST_TIMEOUT_MS));
            {
                let state = app.state::<WindowState>();
                if !state.is_active(show_id, 0) || state.is_acked(show_id, 0) {
                    return;
                }
                write_log_line(&format!(
                    "[overlay-health] ack timeout phase=1 show_id={} failure_class={} diagnostic={}",
                    show_id,
                    state.failure_class(&app),
                    compact_json(&state.diagnostic_snapshot(&app)),
                ));
                state.emit_latest(&app, true);
            }

            thread::sleep(Duration::from_millis(ACK_SECOND_TIMEOUT_MS));
            {
                let state = app.state::<WindowState>();
                if !state.is_active(show_id, 0) || state.is_acked(show_id, 0) {
                    return;
                }
                write_log_line(&format!(
                    "[overlay-health] ack timeout phase=2 show_id={} failure_class={} — confirmed unhealthy diagnostic={}",
                    show_id,
                    state.failure_class(&app),
                    compact_json(&state.diagnostic_snapshot(&app)),
                ));
                state.recover_overlay(&app, show_id);
            }

            thread::sleep(Duration::from_millis(RECOVERY_ACK_TIMEOUT_MS));
            let state = app.state::<WindowState>();
            if !state.is_active(show_id, 1) {
                return;
            }
            if state.is_acked(show_id, 1) {
                write_log_line(&format!(
                    "[overlay-health] recovery OK show_id={} latency_ms={} diagnostic={}",
                    show_id,
                    state.ack_latency_ms(),
                    compact_json(&state.diagnostic_snapshot(&app)),
                ));
            } else {
                write_log_line(&format!(
                    "[overlay-health] recovery FAILED show_id={} reason=no_render_ack failure_class={} diagnostic={}",
                    show_id,
                    state.failure_class(&app),
                    compact_json(&state.diagnostic_snapshot(&app)),
                ));
            }
        });
}

/// 當前程序的 GDI / USER 控制代碼數。長時間執行後若這兩個數持續增長，
/// 說明有控制代碼洩漏——是“懸浮窗用久後不再出現、重啟即好”的典型根因。
#[cfg(windows)]
fn gui_resource_counts() -> (u32, u32) {
    // GetGuiResources 在不同 windows crate 版本里的模組路徑不穩定，直接用 FFI 宣告最穩。
    // GR_GDIOBJECTS=0，GR_USEROBJECTS=1；當前程序用偽控制代碼 (HANDLE)-1。
    #[link(name = "user32")]
    extern "system" {
        fn GetGuiResources(hprocess: *mut core::ffi::c_void, uiflags: u32) -> u32;
    }
    let cur_proc = -1isize as *mut core::ffi::c_void;
    unsafe { (GetGuiResources(cur_proc, 0), GetGuiResources(cur_proc, 1)) }
}

#[cfg(not(windows))]
fn gui_resource_counts() -> (u32, u32) {
    (0, 0)
}

fn event_age_ms(current_ms: i64, event_ms: i64) -> i64 {
    if event_ms <= 0 {
        -1
    } else {
        current_ms.saturating_sub(event_ms)
    }
}

fn classify_overlay_failure(
    window: &Value,
    page_load_started: bool,
    page_load_finished: bool,
    renderer_ready: bool,
) -> &'static str {
    if window.get("handleExists").and_then(Value::as_bool) != Some(true) {
        return "handle_missing";
    }

    // Tauri can register a WebviewWindow label before wry asynchronously creates the
    // native WebView2 controller. A label with no readable position/size is therefore a
    // shell/phantom handle, not a successfully-created overlay.
    let native_window_ready = window.get("position").is_some_and(|value| !value.is_null())
        && window.get("size").is_some_and(|value| !value.is_null());
    if !native_window_ready {
        return "native_create_failed";
    }
    if !page_load_started {
        return "page_load_not_started";
    }
    if !page_load_finished {
        return "page_load_incomplete";
    }
    if !renderer_ready {
        return "renderer_not_ready";
    }

    let visible = window.get("visible").and_then(Value::as_bool) == Some(true);
    let on_screen = window
        .get("intersectsAnyMonitor")
        .and_then(Value::as_bool)
        == Some(true);
    if !visible || !on_screen {
        return "visibility_or_position";
    }

    "render_ack_missing"
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string())
}

fn overlay_window_snapshot(app: &AppHandle) -> Value {
    let Some(overlay) = app.get_webview_window("overlay") else {
        return json!({ "handleExists": false });
    };

    let visible = overlay.is_visible().ok();
    let position = overlay.outer_position().ok();
    let size = overlay.outer_size().ok();
    let monitors = app.available_monitors().unwrap_or_default();
    let primary_monitor = app.primary_monitor().ok().flatten();

    let intersects_any = match (&position, &size) {
        (Some(position), Some(size)) => monitors.iter().any(|monitor| {
            let monitor_position = monitor.position();
            let monitor_size = monitor.size();
            let window_right = position.x as i64 + size.width as i64;
            let window_bottom = position.y as i64 + size.height as i64;
            let monitor_right = monitor_position.x as i64 + monitor_size.width as i64;
            let monitor_bottom = monitor_position.y as i64 + monitor_size.height as i64;
            window_right > monitor_position.x as i64
                && (position.x as i64) < monitor_right
                && window_bottom > monitor_position.y as i64
                && (position.y as i64) < monitor_bottom
        }),
        _ => false,
    };

    let mut snapshot = Map::new();
    snapshot.insert("handleExists".to_string(), json!(true));
    snapshot.insert("visible".to_string(), json!(visible));
    snapshot.insert("intersectsAnyMonitor".to_string(), json!(intersects_any));
    snapshot.insert(
        "position".to_string(),
        position.map(|value| json!({ "x": value.x, "y": value.y })).unwrap_or(Value::Null),
    );
    snapshot.insert(
        "size".to_string(),
        size.map(|value| json!({ "width": value.width, "height": value.height })).unwrap_or(Value::Null),
    );
    if let Some(monitor) = primary_monitor {
        let position = monitor.position();
        let size = monitor.size();
        snapshot.insert("primaryMonitor".to_string(), json!({
            "x": position.x,
            "y": position.y,
            "width": size.width,
            "height": size.height,
            "scaleFactor": monitor.scale_factor(),
        }));
    }
    Value::Object(snapshot)
}

fn set_overlay_interactivity(overlay: &tauri::WebviewWindow, interactive: bool) {
    #[cfg(target_os = "macos")]
    let _ = overlay.set_focusable(interactive);
    let _ = overlay.set_ignore_cursor_events(!interactive);
}

#[cfg(test)]
mod reload_limit_tests {
    use super::reload_allowed_in_window;

    #[test]
    fn at_most_three_reloads_in_five_minutes() {
        let mut h = Vec::new();
        let w = 5 * 60 * 1000;
        assert!(reload_allowed_in_window(&mut h, 0, w, 3));
        assert!(reload_allowed_in_window(&mut h, 1_000, w, 3));
        assert!(reload_allowed_in_window(&mut h, 2_000, w, 3));
        assert!(!reload_allowed_in_window(&mut h, 3_000, w, 3));
        // 最早那次滑出視窗之後又能過載
        assert!(reload_allowed_in_window(&mut h, w + 1, w, 3));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        classify_overlay_failure, overlay_bounds_in_work_area, OverlayLayout, WindowState,
        OVERLAY_BASE_HEIGHT, OVERLAY_DEFAULT_BASE_WIDTH, OVERLAY_ROOT_PADDING_BOTTOM,
    };
    use serde_json::json;

    /// 2560x1440 @150%，工作列佔掉底部 122px —— 報「膠囊頂部被裁」那台機器的實際佈局。
    const WORK: (i64, i64, i64, i64) = (0, 0, 2560, 1318);
    const SCALE: f64 = 1.5;
    /// Windows 設定 → 輔助功能 → 文本大小 123% 時實測的額外縮放（dpr 1.8427 / scale 1.5）。
    const TEXT_SCALE_123: f64 = 1.2285;
    /// 膠囊的 `min-height: 38px`（Overlay.tsx）。CSS 改了這裡會先紅，正是想要的效果。
    const OVERLAY_PILL_MIN_HEIGHT: f64 = 38.0;

    /// 膠囊在 webview 裡需要的 CSS 高度：根節點 pb-4 + 膠囊 min-height。
    fn required_css_height() -> f64 {
        OVERLAY_ROOT_PADDING_BOTTOM + OVERLAY_PILL_MIN_HEIGHT
    }

    #[test]
    fn base_window_fits_pill_at_default_scale() {
        let (_, _, _, height) = overlay_bounds_in_work_area(
            WORK,
            SCALE,
            OverlayLayout::Base.dimensions(OVERLAY_DEFAULT_BASE_WIDTH),
            1.0,
        );
        // 視窗物理高換回 CSS px（css_zoom = 1 時 dpr 就是 scale）後要裝得下內容。
        let css_height = height as f64 / SCALE;
        assert!(
            css_height >= required_css_height(),
            "base window {}px physical = {}css < required {}css",
            height,
            css_height,
            required_css_height(),
        );
    }

    /// 這條是本次故障的迴歸釘子：文本大小放大時視窗必須跟著長高，否則內容從頂部溢位、
    /// 被 overlay.html 的 overflow:hidden 裁平。修復前 height 恆為 84（56×1.5），
    /// webview 裡只有 56 CSS px，裝不下 54 + 需要的餘量。
    #[test]
    fn base_window_still_fits_pill_when_os_text_scale_enlarges_css_px() {
        let (_, _, _, height) = overlay_bounds_in_work_area(
            WORK,
            SCALE,
            OverlayLayout::Base.dimensions(OVERLAY_DEFAULT_BASE_WIDTH),
            TEXT_SCALE_123,
        );
        // 此時 1 CSS px = scale × css_zoom 個裝置畫素。
        let css_height = height as f64 / (SCALE * TEXT_SCALE_123);
        assert!(
            css_height >= required_css_height(),
            "scaled base window {}px physical = {}css < required {}css",
            height,
            css_height,
            required_css_height(),
        );
        assert!(
            height > (OVERLAY_BASE_HEIGHT * SCALE) as u32,
            "window did not grow with css_zoom: {}",
            height,
        );
    }

    #[test]
    fn every_layout_fits_its_content_under_os_text_scale() {
        // 各佈局在 webview 裡需要的 CSS 高度（pb-4 + 內容），內容高取實測值。
        // 兜底卡片 149、輸入源條 30 + gap 8、流式氣泡 110 + gap 8 都來自現場日誌/CSS。
        let cases = [
            ("base", OverlayLayout::Base, required_css_height()),
            ("mic_hint", OverlayLayout::BaseWithMicHint, required_css_height() + 8.0 + 30.0),
            ("fallback", OverlayLayout::Fallback, OVERLAY_ROOT_PADDING_BOTTOM + 149.0),
            // 失敗卡：py-4(32) + 標題 20 + 原因**最多三行** 60 + 恢復提示 20
            //        + 兩處間距 12 + 邊框 2 ≈ 146
            //
            // ⚠️ 這裡原來按「原因只有一行」算成 110，是錯的：供應商的錯誤文案
            // （「帳戶餘額不足，本次請求未能發出…」）在 520px 寬下就佔兩行，實測內容高 97，
            // 加上恢復提示 126 —— 舊視窗 150 只能裝 134，餘量 4px。斷言寫鬆了，
            // 於是這條自檢在真實最壞情況下照樣全綠。
            ("failure", OverlayLayout::Failure, OVERLAY_ROOT_PADDING_BOTTOM + 146.0),
            ("streaming", OverlayLayout::Streaming, required_css_height() + 8.0 + 110.0),
            (
                "streaming_mic_hint",
                OverlayLayout::StreamingWithMicHint,
                required_css_height() + 8.0 + 110.0 + 8.0 + 30.0,
            ),
        ];
        for (name, layout, required) in cases {
            let (_, _, _, height) = overlay_bounds_in_work_area(
                WORK,
                SCALE,
                layout.dimensions(OVERLAY_DEFAULT_BASE_WIDTH),
                TEXT_SCALE_123,
            );
            let css_height = height as f64 / (SCALE * TEXT_SCALE_123);
            assert!(
                css_height >= required,
                "{}: {}css available < {}css required",
                name,
                css_height,
                required,
            );
        }
    }

    /// 底邊貼屏距離是螢幕幾何，不該跟著 webview 的縮放變——否則調一下系統文本大小，
    /// 懸浮窗就整體往螢幕中間跑。
    #[test]
    fn bottom_edge_stays_put_regardless_of_css_zoom() {
        let bottom_of = |css_zoom: f64| {
            let (_, y, _, height) = overlay_bounds_in_work_area(
                WORK,
                SCALE,
                OverlayLayout::Base.dimensions(OVERLAY_DEFAULT_BASE_WIDTH),
                css_zoom,
            );
            y as i64 + height as i64
        };
        assert_eq!(bottom_of(1.0), bottom_of(TEXT_SCALE_123));
        assert_eq!(bottom_of(1.0), bottom_of(2.25));
    }

    /// 視窗再大也不能被推出工作區：夾取之後 y 仍須落在工作區內。
    #[test]
    fn extreme_css_zoom_stays_inside_work_area() {
        let (x, y, width, height) = overlay_bounds_in_work_area(
            WORK,
            SCALE,
            OverlayLayout::StreamingWithMicHint.dimensions(OVERLAY_DEFAULT_BASE_WIDTH),
            4.0,
        );
        assert!(x >= 0, "x={}", x);
        assert!(y >= 0, "y={}", y);
        assert!(x as i64 + width as i64 <= 2560, "right={}", x as i64 + width as i64);
        assert!(y as i64 + height as i64 <= 1318, "bottom={}", y as i64 + height as i64);
    }

    /// macOS：按整屏矩形找可用區域；Tauri 物理畫素取整帶來的半個點誤差也要能對上。
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_visible_area_is_matched_by_screen_frame() {
        use crate::platform::macos::appkit::ScreenArea;
        let areas = [
            // 主屏：頂上 25 點選單欄，底下 70 點程式塢
            ScreenArea { frame: (0.0, 0.0, 1440.0, 900.0), visible: (0.0, 25.0, 1440.0, 805.0) },
            // 右側外接屏，比主屏高出 180 點
            ScreenArea { frame: (1440.0, -180.0, 1920.0, 1080.0), visible: (1440.0, -155.0, 1920.0, 1055.0) },
        ];
        assert_eq!(
            super::visible_area_for_frame((0.0, 0.0, 1440.0, 900.0), &areas),
            Some((0.0, 25.0, 1440.0, 805.0)),
        );
        assert_eq!(
            super::visible_area_for_frame((1440.5, -180.0, 1920.0, 1079.5), &areas),
            Some((1440.0, -155.0, 1920.0, 1055.0)),
        );
        assert_eq!(super::visible_area_for_frame((0.0, 0.0, 1280.0, 800.0), &areas), None);
    }

    /// 依次喂入 payload，返回每一步之後的佈局。只驅動 apply_payload_layout，
    /// 不碰窗口，所以不需要 AppHandle。
    fn layouts_after(payloads: &[serde_json::Value]) -> Vec<OverlayLayout> {
        let state = WindowState::new();
        payloads
            .iter()
            .map(|payload| {
                state.apply_payload_layout(payload);
                state.overlay_layout.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
            })
            .collect()
    }

    /// 只有帶按鈕的卡片佈局才接收滑鼠點選，其餘一律穿透。
    ///
    /// 用 match 而不是陣列：新增佈局變體時這裡會**編譯失敗**，強制作者表態"這個佈局
    /// 有沒有按鈕"。漏設的症狀極難查 —— 按鈕在介面上畫得好好的，就是點不到，
    /// 因為整個視窗被 set_ignore_cursor_events(true) 穿透了。
    #[test]
    fn only_card_layouts_receive_mouse_clicks() {
        let expected = |layout: &OverlayLayout| match layout {
            OverlayLayout::Fallback | OverlayLayout::Failure => true,
            OverlayLayout::Base
            | OverlayLayout::BaseWithMicHint
            | OverlayLayout::Streaming
            | OverlayLayout::StreamingWithMicHint => false,
        };
        for layout in [
            OverlayLayout::Base,
            OverlayLayout::BaseWithMicHint,
            OverlayLayout::Fallback,
            OverlayLayout::Failure,
            OverlayLayout::Streaming,
            OverlayLayout::StreamingWithMicHint,
        ] {
            assert_eq!(
                layout.is_interactive(),
                expected(&layout),
                "interactivity drifted for {:?}",
                layout,
            );
        }
    }

    /// 失敗卡必須拿到自己的佈局，且不能被"可見期間別收縮"那條兜底擋住。
    ///
    /// 場景是真實的：錄音中掉線會先顯示輸入源提示條（BaseWithMicHint），隨後才失敗。
    /// 如果 failure 落進 keep_expanded_bounds 的配對裡，視窗會停在提示條的尺寸上，
    /// 卡片被 overflow:hidden 裁掉一大半。
    #[test]
    fn failure_state_gets_its_own_layout_even_after_a_mic_hint() {
        let layouts = layouts_after(&[
            json!({ "state": "listening", "micSourceLabel": "Blackwire 5220" }),
            json!({ "state": "failure", "failureTitle": "recognition failed" }),
        ]);
        assert_eq!(
            layouts,
            vec![OverlayLayout::BaseWithMicHint, OverlayLayout::Failure],
        );
    }

    #[test]
    fn streaming_flag_expands_the_window_while_listening() {
        assert_eq!(
            layouts_after(&[json!({ "state": "listening", "streaming": true })]),
            vec![OverlayLayout::Streaming],
        );
    }

    /// 迴歸釘（2026-09「開著即時字幕不說話時懸浮窗一閃一閃」）：
    ///
    /// 低音量警告每 5 秒發一次 listening 更新，那幾個 payload 曾漏帶 `streaming`。
    /// 佈局一旦被判回 Base，視窗就從 480×200 收縮、下一幀心跳再撐回去 —— 而 webview 裡
    /// 的 DOM 完全沒動，氣泡被 overflow:hidden 裁掉一幀，看起來就是閃。
    ///
    /// 前端已收口到 listeningPayload()，這條釘的是原生側的兜底：可見期間**任何**缺欄位的
    /// listening 更新都不許讓視窗縮回去。
    #[test]
    fn listening_update_without_streaming_flag_must_not_shrink_the_window() {
        let layouts = layouts_after(&[
            json!({ "state": "listening", "streaming": true }),
            // 這就是當年那份 payload：只有警告文案，沒有 streaming
            json!({ "state": "listening", "warning": "volume is low", "warningTone": "warn" }),
            json!({ "state": "listening", "streaming": true }),
        ]);
        assert_eq!(
            layouts,
            vec![
                OverlayLayout::Streaming,
                OverlayLayout::Streaming,
                OverlayLayout::Streaming,
            ],
        );
    }

    /// 同一條兜底也要覆蓋「輸入源提示還在顯示」的那一支，否則提示條一齣現就又漏一個組合。
    #[test]
    fn streaming_window_survives_a_mic_hint_only_update() {
        let layouts = layouts_after(&[
            json!({ "state": "listening", "streaming": true }),
            json!({ "state": "listening", "micSourceLabel": "Blackwire 5220" }),
        ]);
        assert_eq!(
            layouts,
            vec![OverlayLayout::Streaming, OverlayLayout::Streaming],
        );
    }

    /// 兜底不能兜過頭：新一輪顯示從 waiting 開始，那時必須真的回到基礎尺寸，
    /// 否則上一輪的大視窗會一直留著。
    #[test]
    fn a_new_waiting_phase_resets_to_the_base_layout() {
        let layouts = layouts_after(&[
            json!({ "state": "listening", "streaming": true }),
            json!({ "state": "waiting", "elapsedSec": 0 }),
        ]);
        assert_eq!(layouts, vec![OverlayLayout::Streaming, OverlayLayout::Base]);
    }

    fn native_window() -> serde_json::Value {
        json!({
            "handleExists": true,
            "visible": true,
            "intersectsAnyMonitor": true,
            "position": { "x": 10, "y": 20 },
            "size": { "width": 200, "height": 56 }
        })
    }

    #[test]
    fn classifies_phantom_tauri_handle_as_native_create_failure() {
        let window = json!({
            "handleExists": true,
            "visible": null,
            "intersectsAnyMonitor": false,
            "position": null,
            "size": null
        });
        assert_eq!(
            classify_overlay_failure(&window, false, false, false),
            "native_create_failed"
        );
    }

    #[test]
    fn classifies_page_and_renderer_stages() {
        let window = native_window();
        assert_eq!(
            classify_overlay_failure(&window, false, false, false),
            "page_load_not_started"
        );
        assert_eq!(
            classify_overlay_failure(&window, true, false, false),
            "page_load_incomplete"
        );
        assert_eq!(
            classify_overlay_failure(&window, true, true, false),
            "renderer_not_ready"
        );
        assert_eq!(
            classify_overlay_failure(&window, true, true, true),
            "render_ack_missing"
        );
    }

    #[test]
    fn classifies_visible_but_offscreen_window() {
        let mut window = native_window();
        window["intersectsAnyMonitor"] = json!(false);
        assert_eq!(
            classify_overlay_failure(&window, true, true, true),
            "visibility_or_position"
        );
    }
}

fn monitor_info(app: &AppHandle) -> (f64, f64, f64) {
    if let Some(monitor) = app.primary_monitor().ok().flatten() {
        let size = monitor.size();
        let scale = monitor.scale_factor();
        (size.width as f64 / scale, size.height as f64 / scale, scale)
    } else {
        (1920.0, 1080.0, 1.0)
    }
}

fn calc_overlay_bounds(
    app: &AppHandle,
    layout: &OverlayLayout,
    base_width: f64,
    css_zoom: f64,
) -> (f64, f64, f64, f64) {
    let (design_width, design_height) = layout.dimensions(base_width);
    // 設計尺寸是 CSS px；乘 css_zoom 之後才是能交給 Tauri 的邏輯尺寸。
    let desired_width = design_width * css_zoom;
    let desired_height = design_height * css_zoom;
    let (screen_width, screen_height, _) = monitor_info(app);
    let width = desired_width.min(screen_width - 40.0).max(160.0);
    let height = desired_height
        .min(screen_height - 40.0)
        .max(OVERLAY_BASE_HEIGHT * css_zoom);
    let x = ((screen_width - width) / 2.0).round();
    let y = (screen_height - height - 72.0).max(8.0);
    (x, y, width, height)
}

/// 顯示器內定位的純數學部分，全程物理畫素，便於單測釘住縮放行為。
///
/// - `work`：目標顯示器工作區 `(left, top, right, bottom)`，物理畫素
/// - `scale`：該顯示器的縮放
/// - `design`：佈局的設計尺寸，CSS px
/// - `css_zoom`：webview 裡 1 CSS px 相當於幾個邏輯 px
fn overlay_bounds_in_work_area(
    work: (i64, i64, i64, i64),
    scale: f64,
    design: (f64, f64),
    css_zoom: f64,
) -> (i32, i32, u32, u32) {
    let (left, top, right, bottom) = work;
    let work_width = (right - left).max(1);
    let work_height = (bottom - top).max(1);
    // margin 與 bottom_gap 描述「視窗離螢幕邊多遠」，是螢幕幾何，**不跟 css_zoom 走**：
    // webview 內部怎麼排版不該改變懸浮窗貼屏底的距離。
    let margin = (OVERLAY_SCREEN_MARGIN * scale).round().max(1.0) as i64;
    let bottom_gap = (72.0 * scale).round().max(margin as f64) as i64;
    let available_width = (work_width - margin * 2).max(1);
    let available_height = (work_height - margin * 2).max(1);
    // 用 ceil 而不是 round：寧可多出一個物理畫素的透明邊，也不要向下取整把本就不多的
    // 垂直餘量吃掉——少一個畫素就是膠囊頂部被裁一條。
    let width = ((design.0 * css_zoom * scale).ceil().max(1.0) as i64).min(available_width);
    let height = ((design.1 * css_zoom * scale).ceil().max(1.0) as i64).min(available_height);
    let x = left + ((work_width - width) / 2);
    let min_y = top + margin;
    // height 已被 available_height 夾住，所以 max_y >= min_y 恆成立，clamp 不會 panic。
    let max_y = bottom - margin - height;
    let y = (bottom - bottom_gap - height).clamp(min_y, max_y);
    (x as i32, y as i32, width as u32, height as u32)
}

/// macOS：在前台視窗所在顯示器（取不到就用主顯示器）的工作區底部居中放置，全程邏輯座標。
///
/// `target` 是 context::capture_foreground_monitor 給的前台視窗矩形（邏輯點）。
/// Tauri 的顯示器位置/尺寸是「邏輯點 × 該屏縮放」，各自除以自己的縮放就回到統一的邏輯座標系。
#[cfg(target_os = "macos")]
fn macos_overlay_bounds(
    app: &AppHandle,
    target: Option<&MonitorBounds>,
    layout: &OverlayLayout,
    base_width: f64,
    css_zoom: f64,
) -> (f64, f64, f64, f64) {
    let monitors = app.available_monitors().unwrap_or_default();
    let logical_rect = |monitor: &tauri::Monitor| {
        let scale = monitor.scale_factor().max(0.1);
        let position = monitor.position();
        let size = monitor.size();
        (
            position.x as f64 / scale,
            position.y as f64 / scale,
            size.width as f64 / scale,
            size.height as f64 / scale,
        )
    };
    let by_target = target.and_then(|t| {
        let cx = (t.left as f64 + t.right as f64) / 2.0;
        let cy = (t.top as f64 + t.bottom as f64) / 2.0;
        monitors.iter().find(|monitor| {
            let (x, y, w, h) = logical_rect(monitor);
            cx >= x && cx < x + w && cy >= y && cy < y + h
        })
    });
    let primary = app.primary_monitor().ok().flatten();
    let Some(monitor) = by_target.or(primary.as_ref()) else {
        return calc_overlay_bounds(app, layout, base_width, css_zoom);
    };
    let scale = monitor.scale_factor().max(0.1);
    let work = macos_work_area(monitor);
    let left = work.position.x as f64 / scale;
    let top = work.position.y as f64 / scale;
    let right = left + work.size.width as f64 / scale;
    let bottom = top + work.size.height as f64 / scale;
    let (x, y, width, height) = overlay_bounds_in_work_area(
        (left.round() as i64, top.round() as i64, right.round() as i64, bottom.round() as i64),
        1.0,
        layout.dimensions(base_width),
        css_zoom,
    );
    (x as f64, y as f64, width as f64, height as f64)
}

/// macOS：顯示器工作區（去掉選單欄和程式塢），單位與 Tauri 相同（邏輯點 × 該屏縮放）。
///
/// 不直接用 `Monitor::work_area()`：Tauri 在 macOS 上只按程式塢修正了 x，y 仍取整屏頂邊，
/// 高度卻已扣掉選單欄和程式塢，整塊工作區因此上移了一個選單欄的高度。這裡按
/// NSScreen.visibleFrame 重算；對不上 NSScreen 時退回 Tauri 的值。
#[cfg(target_os = "macos")]
pub(crate) fn macos_work_area(monitor: &tauri::Monitor) -> tauri::PhysicalRect<i32, u32> {
    let scale = monitor.scale_factor().max(0.1);
    let position = monitor.position();
    let size = monitor.size();
    let frame = (
        position.x as f64 / scale,
        position.y as f64 / scale,
        size.width as f64 / scale,
        size.height as f64 / scale,
    );
    let areas = crate::platform::macos::appkit::screen_areas();
    match visible_area_for_frame(frame, &areas) {
        Some((x, y, width, height)) => tauri::PhysicalRect {
            position: tauri::PhysicalPosition::new((x * scale).round() as i32, (y * scale).round() as i32),
            size: tauri::PhysicalSize::new(
                (width * scale).round().max(1.0) as u32,
                (height * scale).round().max(1.0) as u32,
            ),
        },
        None => *monitor.work_area(),
    }
}

/// 按整屏矩形找到對應顯示器的可用區域。容差 2 個邏輯點，吸收 Tauri 物理畫素取整帶來的誤差。
#[cfg(target_os = "macos")]
fn visible_area_for_frame(
    frame: (f64, f64, f64, f64),
    areas: &[crate::platform::macos::appkit::ScreenArea],
) -> Option<(f64, f64, f64, f64)> {
    const TOLERANCE: f64 = 2.0;
    let close = |a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)| {
        (a.0 - b.0).abs() <= TOLERANCE
            && (a.1 - b.1).abs() <= TOLERANCE
            && (a.2 - b.2).abs() <= TOLERANCE
            && (a.3 - b.3).abs() <= TOLERANCE
    };
    areas.iter().find(|area| close(area.frame, frame)).map(|area| area.visible)
}

/// 在錄音開始時的前台視窗所在顯示器底部居中放置。
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn calc_monitor_overlay_bounds(
    app: &AppHandle,
    target: &MonitorBounds,
    layout: &OverlayLayout,
    base_width: f64,
    css_zoom: f64,
) -> Option<(i32, i32, u32, u32)> {
    let center_x = target.left as i64 + (target.right as i64 - target.left as i64) / 2;
    let center_y = target.top as i64 + (target.bottom as i64 - target.top as i64) / 2;
    let monitors = app.available_monitors().ok()?;
    let monitor = monitors.iter().find(|monitor| {
        let position = monitor.position();
        let size = monitor.size();
        center_x >= position.x as i64
            && center_x < position.x as i64 + size.width as i64
            && center_y >= position.y as i64
            && center_y < position.y as i64 + size.height as i64
    })?;

    let scale = monitor.scale_factor();
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }

    Some(overlay_bounds_in_work_area(
        (
            target.left as i64,
            target.top as i64,
            target.right as i64,
            target.bottom as i64,
        ),
        scale,
        layout.dimensions(base_width),
        css_zoom,
    ))
}
