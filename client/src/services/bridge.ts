/**
 * Tauri IPC Bridge — 所有前端程式碼透過這個模組與 Rust 後端通訊。
 */

import { invoke } from '@tauri-apps/api/core'
import { listen, emit } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'

import type { AsrHotwordCapability } from '../lib/asrModels'
import type { DiagnosticOccurrence, DiagnosticsPreview } from '../types/appApi'
import type { UpdateNotificationSnapshot } from '@/features/update/notificationTypes'

// Re-export for convenience
export { invoke, listen, emit }

export function syncUpdateNotification(snapshot: UpdateNotificationSnapshot) {
  return invoke<void>('sync_update_notification', { snapshot })
}

export function getUpdateNotification() {
  return invoke<UpdateNotificationSnapshot>('get_update_notification')
}

export function fitUpdateNotification(revision: number, width: number, height: number, devicePixelRatio: number) {
  return invoke<void>('fit_update_notification', { revision, width, height, devicePixelRatio })
}

// ─── Window Controls ───

export function minimize() {
  getCurrentWindow().minimize()
}

export function maximize() {
  getCurrentWindow().toggleMaximize()
}

export function close() {
  getCurrentWindow().close()
}

// ─── Overlay ───

/**
 * Bug 003 diagnostic helper — log overlay IPC failures to runtime events
 * (which mirror to sayit.log via appendDebugLog).
 */
function logOverlayIpcError(op: string, err: unknown) {
  try {
    // Lazy import to avoid circular dep with debugLog
    void import('./debugLog').then(({ addRuntimeEvent }) => {
      addRuntimeEvent('error', 'overlay-ipc', `${op} failed`, { error: String(err) })
    })
  } catch {
    console.error(`[overlay-ipc] ${op} failed:`, err)
  }
}

export interface OverlayHealthSnapshot {
  showId: number
  activeShowId: number
  activeGeneration: number
  acked: boolean
  ackGeneration: number
  ackLatencyMs: number
  recoveryStarted: boolean
  recoverySucceeded: boolean
  window: {
    handleExists: boolean
    visible?: boolean | null
    intersectsPrimary?: boolean
    position?: { x: number; y: number } | null
    size?: { width: number; height: number } | null
  }
}

/** 原子地更新狀態並顯示懸浮窗，返回本次顯示的關聯 id。 */
export function presentOverlay(data: unknown) {
  return invoke<number>('present_overlay', { data }).catch((err) => {
    logOverlayIpcError('present_overlay', err)
    return 0
  })
}

/** 相容舊呼叫；新顯示流程應優先使用 presentOverlay。 */
export function showOverlay() {
  return invoke<number>('show_overlay').catch((err) => {
    logOverlayIpcError('show_overlay', err)
    return 0
  })
}

export function hideOverlay() {
  return invoke<void>('hide_overlay').catch((err) => {
    logOverlayIpcError('hide_overlay', err)
  })
}

export function updateOverlay(data: unknown) {
  return invoke<void>('update_overlay_state', { data }).catch((err) => {
    logOverlayIpcError('update_overlay_state', err)
  })
}

// devicePixelRatio 在這裡就上報：預熱時（還沒有任何一次顯示）是原生側唯一能在第一次
// 顯示之前拿到 webview 真實縮放的時機，否則使用者第一次口述會看到尺寸不對的懸浮窗。
export function overlayReady(devicePixelRatio?: number) {
  return invoke<void>('overlay_ready', { devicePixelRatio })
}

export function overlayRenderAck(data: unknown) {
  return invoke<void>('overlay_render_ack', { data })
}

export function getOverlayHealth(showId: number) {
  return invoke<OverlayHealthSnapshot>('get_overlay_health', { showId })
}

export type EscapeActionMode =
  | 'off'
  | 'cancel_recording'
  | 'cancel_processing'
  | 'dismiss_fallback'
  | 'abandon_late_result'

/** 只在懸浮窗需要響應全域性 Esc 時開啟；Rust 側帶超時保險，避免異常後永久吞鍵。 */
export function setEscapeActionMode(mode: EscapeActionMode, token = 0) {
  return invoke<void>('set_escape_action_mode', { mode, token })
}

/** 懸浮窗卡片上的臨時組合鍵。字串值是與 Rust 的契約，見 keyboard::card_hotkey_action_name。 */
export type CardHotkeyAction = 'copy'

/**
 * 開啟/續期/解除卡片快捷鍵；傳空陣列即解除。
 *
 * 為什麼要經過原生：懸浮窗不搶焦點，按鍵始終發給使用者原來的程式，webview 裡的
 * keydown 永遠不會觸發。Rust 側帶硬 TTL（20s）兜底，所以**卡片可見期間必須週期性
 * 續期**，間隔要明顯小於 TTL。
 */
export function setCardHotkeys(actions: CardHotkeyAction[], token = 0) {
  return invoke<void>('set_card_hotkeys', { actions, token })
}

// ─── Paste / Context ───

export function pasteText(text: string, hwnd?: string, focusHwnd?: string, restoreClipboard?: boolean, focusToken?: string) {
  return invoke<{
    ok: boolean
    strategy?: string
    reason?: string
    detail?: string
    attempts?: Array<{ strategy: string; ok: boolean; reason?: string; detail?: string }>
  }>('paste_text', { text, hwnd: hwnd || null, focusHwnd: focusHwnd || null, restoreClipboard: restoreClipboard ?? false, focusToken: focusToken ?? null })
}

export function getProbeResult() {
  return invoke<Record<string, unknown>>('get_probe_result')
}

/** 一次原生捕獲同時返回錄音目標上下文與插字探測結果。 */
export function getRecordingContext(includeTextContext = false) {
  return invoke<{
    appContext: Record<string, unknown>
    probe: Record<string, unknown>
  }>('get_recording_context', { includeTextContext })
}

// [自訂] L-80：釋放錄音時保留的 macOS AX 參照。
export function releaseRecordingFocus(focusToken: string) {
  return invoke<void>('release_recording_focus', { focusToken })
}

export function getActiveAppContext() {
  return invoke<Record<string, unknown> | null>('get_active_app_context')
}

/** 系統許可權狀態（macOS：輔助功能 / 麥克風 / 安全輸入；Windows 恆為已授權）。 */
export interface PlatformPermissions {
  platform: string
  accessibility: boolean
  hotkeyActive: boolean
  microphone: 'not_determined' | 'restricted' | 'denied' | 'authorized' | 'unknown'
  secureInput: boolean
  translocated: boolean
  bundlePath: string
}

export function getPlatformPermissions() {
  return invoke<PlatformPermissions>('get_platform_permissions')
}

/** 請求「輔助功能」授權（沒授權時系統會彈出引導框）。返回當前是否已授權。 */
export function requestAccessibilityPermission() {
  return invoke<boolean>('request_accessibility_permission')
}

/** 開啟系統設定的隱私面板：accessibility / microphone / input_monitoring。 */
export function openPrivacySettings(pane: 'accessibility' | 'microphone' | 'input_monitoring') {
  return invoke<void>('open_privacy_settings', { pane })
}

/** 介面語言切換後重建 mac 的原生選單（選單欄圖示選單、頂部應用選單）。Windows 上是空操作。 */
export function refreshNativeMenus() {
  return invoke<void>('refresh_native_menus')
}

export function getClientRuntimeInfo() {
  return invoke<{
    userId: string
    userName: string
    deviceId: string
    hostname: string
    clientVersion: string
    platform: string
    arch?: string
    osVersion: string
    localIp: string
    systemLocale: string
    cpuCores: number
    memoryMb: number
  }>('get_client_runtime_info')
}

/**
 * 系統**顯示語言**，回傳 `zh-TW` / `en`。
 *
 * 與 `getClientRuntimeInfo().systemLocale` 不是一回事：那個是**區域格式**
 * （日期貨幣怎麼寫），診斷用；這個是 Windows 介面本身的語言，決定介面語言。
 * 判定只在 Rust 一處（`locale::system_ui_lang`），托盤和介面才不會各說一套。
 */
export function getSystemUiLanguage() {
  return invoke<string>('get_system_ui_language')
}

export function copyText(text: string) {
  return invoke('copy_text', { text })
}

// ─── 系統輸出靜音（錄音期間防回採）───

/** 記錄當前預設輸出裝置靜音狀態並將其靜音。返回是否已處理。 */
export function muteSystemOutput() {
  return invoke<boolean>('mute_system_output')
}

/** 恢復到 muteSystemOutput 之前記錄的靜音狀態。 */
export function restoreSystemOutput() {
  return invoke<boolean>('restore_system_output')
}

export function appendDebugLog(payload: unknown) {
  invoke('append_debug_log', { payload })
}

// ─── Store ───

// [自訂] REV9：同一 WebView 未 await 的設定呼叫也依送出順序進入原生佇列。
const pendingSettings = new Map<string, Promise<void>>()

function queueSetting<T>(key: string, invokeSetting: () => Promise<T>): Promise<T> {
  const previous = pendingSettings.get(key)
  const result = previous ? previous.then(invokeSetting) : invokeSetting()
  const tail = result.then(() => {}, () => {})
  pendingSettings.set(key, tail)
  void tail.then(() => {
    if (pendingSettings.get(key) === tail) pendingSettings.delete(key)
  })
  return result
}

export function storeGet(key: string) {
  return queueSetting(key, () => invoke<unknown>('store_get', { key }))
}

export function storeSet(key: string, value: unknown) {
  return queueSetting(key, () => invoke('store_set', { key, value }))
}

export function storeDelete(key: string) {
  return queueSetting(key, () => invoke('store_delete', { key }))
}

// [自訂] L-72：只傳副本名稱，路徑由原生端查出。
export interface CorruptDbCopy {
  name: string
  createdAtLocal: string
  sizeBytes: number
}

export function listCorruptDbCopies() {
  return invoke<CorruptDbCopy[]>('list_corrupt_db_copies')
}

export function revealCorruptDbCopy(name: string) {
  return invoke<void>('reveal_corrupt_db_copy', { name })
}

export function deleteCorruptDbCopy(name: string) {
  return invoke<void>('delete_corrupt_db_copy', { name })
}

export function recordingCleanupProtected() {
  return invoke<boolean>('recording_cleanup_protected')
}

export function resumeRecordingCleanup() {
  return invoke<void>('resume_recording_cleanup')
}

// ─── History ───

export function historyList(query?: {
  keyword?: string
  favoriteOnly?: boolean
  limit?: number
  offset?: number
}) {
  return invoke<unknown[]>('history_list', { query })
}

export function historyCount(query?: {
  keyword?: string
  favoriteOnly?: boolean
}) {
  return invoke<number>('history_count', { query })
}

export function historyAdd(record: unknown) {
  return invoke('history_add', { record })
}

export function historyUpdate(id: string, patch: Record<string, unknown>) {
  return invoke('history_update', { id, patch })
}

export function historyDelete(id: string) {
  return invoke('history_delete', { id })
}

// [自訂] S61：查詢與清除舊歷史時也涵蓋本機孤立的錄音。
export function historyRetainedCounts() {
  return invoke<[number, number]>('history_retained_counts')
}

export function historyClearRetained(historyCount: number, audioCount: number) {
  return invoke('history_clear_retained', { historyCount, audioCount })
}

export function historySetFavorite(id: string, favorite: boolean) {
  return invoke('history_set_favorite', { id, favorite })
}

// ─── Export ───

export function saveTextExport(payload: {
  defaultPath: string
  content: string
  filters?: Array<{ name: string; extensions: string[] }>
}) {
  return invoke<string | null>('save_text_export', { payload })
}

export function saveExportBundle(payload: {
  defaultPath: string
  files: Array<{ name: string; content: string }>
}) {
  return invoke<string | null>('save_export_bundle', { payload })
}

// ─── Shortcuts ───

/** 前端內部廣播：快捷鍵設定已變化，供頁面（如首頁提示）即時重新整理顯示。 */
export const SHORTCUTS_CHANGED_EVENT = 'sayit:shortcuts-changed'

export function notifyShortcutsChanged() {
  invoke('shortcuts_changed')
  // 同時在前端廣播，讓依賴快捷鍵顯示的頁面無需切換路由即可重新整理
  if (typeof window !== 'undefined') {
    window.dispatchEvent(new CustomEvent(SHORTCUTS_CHANGED_EVENT))
  }
}

export function testShortcut(accelerator: string) {
  return invoke<boolean>('test_shortcut', { accelerator })
}

/** 查詢 PTT DOM code 對應按鍵當前是否真實物理按下（順序與輸入一致）。 */
export function getPTTPhysicalKeyStates(codes: string[]) {
  return invoke<boolean[]>('get_ptt_physical_key_states', { codes })
}

/**
 * 開始快捷鍵錄製捕獲（設定頁點開錄製時呼叫）。Rust 側會：
 * - 底層滑鼠鉤子吞掉下一個側鍵並透過 mouse-shortcut-captured 回報；
 * - 鍵盤鉤子在錄製期間放行 PTT/免持單鍵，不觸發錄音；
 * - 登出全部 global_shortcut，避免已繫結的組合鍵被系統搶走、錄不進來。
 */
export function beginShortcutCapture() {
  return invoke('begin_shortcut_capture').catch(() => { })
}

/** 結束快捷鍵錄製捕獲（錄製結束/取消時呼叫），並恢復 Rust 側的熱鍵註冊。 */
export function endShortcutCapture() {
  return invoke('end_shortcut_capture').catch(() => { })
}

// ─── System ───

export function getAutoLaunch() {
  return invoke<boolean>('get_auto_launch')
}

export function setAutoLaunch(enable: boolean) {
  return invoke('set_auto_launch', { enable })
}

/**
 * 拉起安裝程式並退出應用。
 * relaunch=true 裝完自動重開（使用者主動點更新時）；退出路徑上的兜底安裝走 Rust 內部，
 * 傳 false —— 使用者是要關掉 SayIt，裝完再拉起來會表現成"這軟體關不掉"。
 */
export function installDownloadedUpdate(filePath: string, relaunch: boolean) {
  return invoke('install_downloaded_update', { filePath, relaunch })
}

/** 下載安裝包到臨時目錄，返回完整路徑。傳了 sha512（Base64）就由 Rust 側校驗完整性。 */
export function downloadUpdate(url: string, sha512?: string | null) {
  return invoke<string>('download_update', { url, sha512: sha512 ?? null })
}

/** 磁碟上那個安裝包還能不能用（存在 + 雜湊對得上）。用於啟動時複用上次下載的包。 */
export function verifyUpdatePackage(filePath: string, sha512?: string | null) {
  return invoke<boolean>('verify_update_package', { filePath, sha512: sha512 ?? null })
}

// ─── Tray ───

/**
 * 把托盤右鍵選單裡「AI 整理」那一項刷成給定狀態（文字上的開/關 + 狀態圖示）。
 *
 * 托盤選單只在啟動時建一次（Tauri v2 沒有「選單即將彈出」的鉤子），
 * 所以介面裡每次切開關都要主動回寫，否則右鍵看到的是上一次的狀態。
 */
export function setTrayAiEnabled(enabled: boolean) {
  return invoke<void>('set_tray_ai_enabled', { enabled }).catch(() => { })
}

/** 托盤右鍵切換了「AI 整理」（Rust 已落庫，payload 是切換後的值）。 */
export function onAiCleanupChanged(cb: (enabled: boolean) => void) {
  const unlisten = listen<{ enabled?: boolean }>('ai-cleanup-changed', (event) => {
    cb(Boolean(event.payload?.enabled))
  })
  return () => { unlisten.then((fn) => fn()) }
}

/** AI 整理快捷鍵被觸發；前端負責更新記憶體、持久化和懸浮窗提示。 */
export function onAiCleanupToggleRequested(cb: () => void) {
  const unlisten = listen('toggle-ai-cleanup', () => cb())
  return () => { unlisten.then((fn) => fn()) }
}

export function setPTTLabConfig(data: unknown) {
  console.log('[bridge] setPTTLabConfig called', data)
  invoke('set_ptt_lab_config', { data }).catch((err) => {
    console.error('[bridge] setPTTLabConfig failed:', err)
  })
}

// ─── Audio Files ───

export function saveAudioFile(id: string, wavBase64: string) {
  return invoke<string>('save_audio_file', { id, wavBase64 })
}

export function savePcmAsWav(id: string, pcmBase64: string, sampleRate?: number) {
  return invoke<string>('save_pcm_as_wav', { id, pcmBase64, sampleRate: sampleRate ?? null })
}

/**
 * 二進位制請求體：`[4 位元組小端 JSON 長度][JSON 引數][原始位元組]`，與 Rust 的 ipc_body 對應。
 *
 * 音訊原來先轉 base64 再塞進 JSON。lib/encoding 那個轉碼是逐位元組拼字串，2 分鐘錄音約
 * 0.4 秒、5 分鐘約 0.7 秒，跑在主窗主執行緒上（懸浮窗那段時間也跟著卡），本地 / 雲 API
 * 模式錄完還要轉兩次。直接發原始位元組就沒有這一段了。
 */
export function framedBody(meta: unknown, payload: Uint8Array): Uint8Array {
  const json = new TextEncoder().encode(JSON.stringify(meta))
  const out = new Uint8Array(4 + json.length + payload.length)
  new DataView(out.buffer).setUint32(0, json.length, true)
  out.set(json, 4)
  out.set(payload, 4 + json.length)
  return out
}

/** 讀錄音的原始位元組。檔案不在（保留期過了、被刪了）時返回 null。 */
export async function readAudioFileBytes(filePath: string): Promise<Uint8Array<ArrayBuffer> | null> {
  const buffer = await invoke<ArrayBuffer>('read_audio_file_bytes', { filePath })
  return buffer && buffer.byteLength > 0 ? new Uint8Array(buffer) : null
}

/** 把錄音直接複製到「下載」資料夾（由 Rust 複製檔案，不經過網頁）。 */
export function saveRecordingToDownloads(filePath: string, filename: string) {
  return invoke<string>('save_recording_to_downloads', { filePath, filename })
}

export function savePcmAsWavRaw(id: string, pcm: Uint8Array, sampleRate?: number) {
  return invoke<string>('save_pcm_as_wav_raw', framedBody({ id, sampleRate: sampleRate ?? null }, pcm))
}

export function localTranscribeRaw(
  pcm: Uint8Array,
  opts: { modelId: string; language?: string; accelerator?: string; gpuDevice?: string },
) {
  return invoke<{ text: string; elapsed_ms: number }>('local_transcribe_raw', framedBody(opts, pcm))
}

export function cloudTranscribeRaw<T>(
  pcm: Uint8Array,
  request: { sample_rate: number; asr_config: unknown; hotwords: string[] },
) {
  return invoke<T>('cloud_transcribe_raw', framedBody(request, pcm))
}

export function readAudioFile(filePath: string) {
  return invoke<string | null>('read_audio_file', { filePath })
}

/**
 * 錄音檔案是否還在。
 *
 * 保留期到了之後 Rust 只刪檔案、不清歷史記錄裡的 audioFilePath，
 * 所以「記錄裡有路徑」不等於「檔案還在」，凡是依賴錄音的入口都要先問這一句。
 */
export function audioFileExists(filePath: string) {
  return invoke<boolean>('audio_file_exists', { filePath })
}

export function deleteAudioFile(filePath: string) {
  return invoke('delete_audio_file', { filePath })
}

// [自訂] S71：播放使用中的錄音延後清理。
export function protectPlayingRecording(filePath: string) {
  return invoke<string | null>('protect_playing_recording', { filePath })
}

export function renewPlayingRecording(token: string) {
  return invoke<void>('renew_playing_recording', { token })
}

export function releasePlayingRecording(token: string) {
  return invoke<void>('release_playing_recording', { token })
}

// ─── Diagnostics ───

export function collectSettings() {
  return invoke<Record<string, unknown>>('collect_settings')
}

export function getDiagnosticsPreview(data: {
  settings: Record<string, unknown>
  issueOccurrence: DiagnosticOccurrence
}) {
  return invoke<DiagnosticsPreview>('get_diagnostics_preview', { data })
}

export function createDiagnosticsZip(data: unknown) {
  return invoke<string>('create_diagnostics_zip', { data })
}

/** 截圖走二進位制：引數裡每張圖只帶 name / type / size，位元組按順序拼在後面（見 framedBody）。 */
export function createDiagnosticsZipRaw(
  meta: { description: string; issueType: string; settings: unknown; issueOccurrence: string },
  images: Array<{ name: string; type: string; bytes: Uint8Array }>,
) {
  const total = images.reduce((sum, image) => sum + image.bytes.length, 0)
  const payload = new Uint8Array(total)
  let offset = 0
  for (const image of images) {
    payload.set(image.bytes, offset)
    offset += image.bytes.length
  }
  const imageMeta = images.map((image) => ({ name: image.name, type: image.type, size: image.bytes.length }))
  return invoke<string>('create_diagnostics_zip_raw', framedBody({ ...meta, images: imageMeta }, payload))
}

/** 讀診斷包的原始位元組；檔案不在時返回 null。 */
export async function readDiagnosticsZipBytes(path: string): Promise<Uint8Array<ArrayBuffer> | null> {
  const buffer = await invoke<ArrayBuffer>('read_diagnostics_zip_bytes', { path })
  return buffer && buffer.byteLength > 0 ? new Uint8Array(buffer) : null
}

export function readDiagnosticsZip(path: string) {
  return invoke<number[] | null>('read_diagnostics_zip', { path })
}

export function copyDiagnosticsZip(source: string, destination: string) {
  return invoke<void>('copy_diagnostics_zip', { source, destination })
}

/** [自訂] S45：刪掉診斷目錄裡的暫存 zip（取消另存或出錯時）。 */
export function discardDiagnosticsZip(path: string) {
  return invoke<void>('discard_diagnostics_zip', { path })
}

export function readLogFile(logType: string) {
  return invoke<string | null>('read_log_file', { logType })
}

export function openLogFolder() {
  return invoke('open_log_folder')
}

// ─── Event Listeners ───

export function onOverlayState(cb: (data: unknown) => void) {
  const unlisten = listen<unknown>('overlay-state', (event) => cb(event.payload))
  return () => { unlisten.then((fn) => fn()) }
}

export function onPTTDown(cb: (data?: unknown) => void) {
  const unlisten = listen<unknown>('ptt-down', (event) => cb(event.payload))
  return () => { unlisten.then((fn) => fn()) }
}

export function onPTTUp(cb: (data?: unknown) => void) {
  const unlisten = listen<unknown>('ptt-up', (event) => cb(event.payload))
  return () => { unlisten.then((fn) => fn()) }
}

export function onPTTToggle(cb: (data?: unknown) => void) {
  const unlisten = listen<unknown>('ptt-toggle', (event) => cb(event.payload))
  return () => { unlisten.then((fn) => fn()) }
}

export function onPTTTimeoutWarning(cb: (data?: unknown) => void) {
  const unlisten = listen<unknown>('ptt-timeout-warning', (event) => cb(event.payload))
  return () => { unlisten.then((fn) => fn()) }
}

export function onToggleHandsFree(cb: (data?: unknown) => void) {
  const unlisten = listen<unknown>('toggle-hands-free', (event) => cb(event.payload))
  return () => { unlisten.then((fn) => fn()) }
}

export function onEscapeAction(cb: (data: { mode: EscapeActionMode; token: number }) => void) {
  const unlisten = listen<{ mode: EscapeActionMode; token: number }>('escape-action', (event) => cb(event.payload))
  return () => { unlisten.then((fn) => fn()) }
}

export function onCardHotkey(cb: (data: { action: CardHotkeyAction; token: number }) => void) {
  const unlisten = listen<{ action: CardHotkeyAction; token: number }>('card-hotkey', (event) => cb(event.payload))
  return () => { unlisten.then((fn) => fn()) }
}

/** 事件名：懸浮窗自己收起了卡片（點關閉、或複製完成後自動收起）。 */
export const OVERLAY_CARD_DISMISSED = 'overlay-card-dismissed'

/**
 * 懸浮窗收起卡片後回報主窗。
 *
 * 為什麼必須回報：卡片的**生命週期歸主窗的 OverlayService 管**（它持有續期定時器）。
 * 懸浮窗那邊只是一個渲染端，它 hideOverlay() 之後主窗毫不知情，8 秒後續期定時器
 * 又會把 Esc / Ctrl+C 重新註冊一遍 —— 卡片早就沒了，使用者的按鍵卻還被接管著，
 * 而且原生側會重新抓一次前台視窗（此時可能已經是別的程式）。
 */
export function notifyCardDismissed(reason: string, token: number) {
  return emit(OVERLAY_CARD_DISMISSED, { reason, token })
}

export function onCardDismissed(cb: (data: { reason: string; token: number }) => void) {
  const unlisten = listen<{ reason: string; token: number }>(OVERLAY_CARD_DISMISSED, (event) => cb(event.payload))
  return () => { unlisten.then((fn) => fn()) }
}

export function onMouseShortcutCaptured(cb: (data: { setting: string; vk: number }) => void) {
  const unlisten = listen<{ setting: string; vk: number }>('mouse-shortcut-captured', (event) => cb(event.payload))
  return () => { unlisten.then((fn) => fn()) }
}

export function onPTTLabEvent(cb: (data?: unknown) => void) {
  const unlisten = listen<unknown>('ptt-lab-event', (event) => cb(event.payload))
  return () => { unlisten.then((fn) => fn()) }
}

/**
 * 這家 ASR 拿使用者的熱詞做什麼。**權威宣告在 Rust**（providers/capabilities.rs）。
 *
 * 前端刻意不自己維護一份「哪家支援熱詞」的清單：issue #67 的成因正是實現在 Rust、
 * 宣告在前端一張手寫表格，兩者無人對帳，結果 6 家雲服務被漏掉，介面平靜地告訴
 * 使用者熱詞生效。
 *
 * 失敗時返回 null（而不是編一個"不支援"）—— 讓呼叫方能把"查不到"和"確定不傳"
 * 分開顯示。老版本 Rust 沒有這個 command 時也走這條。
 */
export function asrHotwordCapability(provider: string, extra?: Record<string, unknown>) {
  return invoke<AsrHotwordCapability>('asr_hotword_capability', { provider, extra })
    .catch(() => null)
}

/**
 * 全部服務的熱詞行為，供「各服務對熱詞的支援」對照表使用。
 *
 * 這張表因此是**實現的投影**，不是介面裡的第二份手寫清單 —— 後者就是 issue #67：
 * 表格漏了 6 家、還把「不支援」說成只是本地模型的問題，而沒人會在改實現時想起它。
 *
 * 失敗返回 null，呼叫方據此顯示"暫時取不到"，不要退化成一張空表（空表看起來像
 * "所有服務都不支援"）。
 */
export function asrHotwordCapabilityMatrix() {
  return invoke<Record<string, AsrHotwordCapability>>('asr_hotword_capability_matrix')
    .catch(() => null)
}

/**
 * 前端內部廣播：剛走完一次一次性雲端轉寫，熱詞能力的答案可能已經變了。
 *
 * 只有「OpenAI 相容」的 auto 協議會這樣：它的答案在探測之前是 `undecided_protocol`，
 * 而探測就發生在 `cloud_transcribe` 內部（結果進 `asr_openai_compat` 的 PROTOCOL_CACHE）。
 * 熱詞頁開著不動的時候完成首次錄音，頁面上那句"還沒探測出來"會一直掛著 —— 它的查詢
 * 只在掛載時跑過一次。
 *
 * 廣播點刻意只放在 `cloud_transcribe` 之後，不放在錄音結束：流式路徑不碰協議探測
 * （`openai_compat` 沒有流式實現），本地與伺服器模式更不會。
 */
export const ASR_CAPABILITY_MAYBE_CHANGED_EVENT = 'sayit:asr-capability-maybe-changed'

export function notifyAsrCapabilityMaybeChanged() {
  if (typeof window !== 'undefined') {
    window.dispatchEvent(new CustomEvent(ASR_CAPABILITY_MAYBE_CHANGED_EVENT))
  }
}
