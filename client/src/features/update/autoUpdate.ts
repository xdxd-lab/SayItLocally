/**
 * 更新服務：檢查 → 後台靜默下載 → 等使用者點「立即更新」，或在退出時兜底安裝。
 *
 * 設計要點（改動前後的差別，別不小心改回去）：
 *
 * · **不再自動安裝。** 舊實現是"發現新版 → 下載 → 3 秒後 app.exit(0) 裝掉"，
 *   使用者正在按住說話時會被一個關不掉的全屏遮罩糊住、然後應用自己退出。
 *   現在下載完只是把包記進 pending，安裝時機交給使用者（桌面提醒卡片 / 關於頁）
 *   或退出路徑（Rust 的 install_pending_update_on_exit）。
 *
 * · **週期檢查是必需的，不是加分項。** SayIt 常駐托盤 + 開機自啟，很多使用者幾周不重啟。
 *   只在啟動時查一次，等於絕大多數人永遠發現不了新版本。
 *
 * · **下載要冪等。** 舊實現下完立刻安裝，所以"下載了但沒裝"不存在；現在這是常態，
 *   已下載的包必須記到設定裡（pendingUpdate），重啟後靠 verify_update_package 複用，
 *   否則每次開機都會把整包重下一遍。
 *
 * 全域性單例狀態：關於頁、左下角圖示、桌面提醒都訂閱同一份，共用同一把併發鎖，
 * 不會出現啟動自動檢查與使用者手動點選各下載一遍的情況。
 */

import { listen } from '@tauri-apps/api/event'
import { checkVersionUpdate, compareVersions, type VersionInfo } from './updateChecker'
import { getSetting, setSetting } from '@/services/store'
import * as bridge from '@/services/bridge'
import { addRuntimeEvent } from '@/services/debugLog'
import { getOfficialUpdateBaseUrl, getUpdateBaseUrl, isOfficialUpdateChannel } from '@/services/runtimeConfig'
import { getState as getRecorderState } from '@/services/recorder'
import { t } from '@/i18n'

/** 已下載待安裝的包。持久化到設定裡，重啟後仍然知道裝過什麼。 */
export interface PendingUpdate {
  version: string
  filePath: string
  sha512?: string | null
}

const PENDING_UPDATE_KEY = 'pendingUpdate'

/**
 * 週期檢查間隔。6 小時是"當天內一定能收到"和"別沒事就打伺服器"之間的折中：
 * 常駐使用者按天計算命中一次即可，檢查本身只是一個幾百位元組的 manifest 請求。
 */
const CHECK_INTERVAL_MS = 6 * 60 * 60 * 1000

/**
 * 當前版本號。
 *
 * `__APP_VERSION__` 是 vite 的 define 注入的編譯期常量。用 typeof 兜一層而不是直接讀：
 * 直接讀一旦注入沒生效就是 ReferenceError，而這段程式碼跑在 `void startUpdateService()`
 * 裡，異常會被 Promise 吞掉 —— 表現成"更新功能完全沒反應"，且不留任何痕跡。
 *
 * 取不到時**不能假裝成 0.0.0**：那會讓每次檢查都判定"有更新"，反覆下載同一個包。
 * 返回 null，由呼叫方記一條錯誤後停掉檢查。
 */
function readCurrentVersion(): string | null {
  const value = typeof __APP_VERSION__ === 'string' ? __APP_VERSION__ : null
  return value && /^\d+(\.\d+)*$/.test(value) ? value : null
}

/**
 * phase 只表示**此刻正在幹什麼**，不表示"有沒有包等著裝" —— 那件事由 pending 單獨表示。
 *
 * 曾經有過一個 'ready' phase，是個 bug 溫床：週期檢查一開始就把 phase 從 'ready' 改成
 * 'checking'，而"包已存在、跳過下載"那條早退分支沒人把它改回去，phase 就永久卡在
 * 'checking'（表現：關於頁一直顯示"正在檢查更新"、提醒圖示永遠不亮）。
 * 一件事只能有一個真相來源，別把 'ready' 加回來。
 */
export type AutoUpdatePhase = 'idle' | 'checking' | 'downloading' | 'installing'

export interface AutoUpdateState {
  phase: AutoUpdatePhase
  versionInfo?: VersionInfo | null
  checkedAt?: number | null
  /** 已下載待安裝的包。與 phase 正交：後台正在做別的事時它照樣成立。 */
  pending?: PendingUpdate | null
  error?: string | null
  /** 安裝失敗獨立於後台檢查/下載錯誤，卡片只展示使用者主動安裝的結果。 */
  installError?: string | null
  /** 下載進度百分比（0-100），來自 Rust 的 update-download-progress 事件 */
  downloadPercent?: number
}

let currentState: AutoUpdateState = { phase: 'idle', versionInfo: null, checkedAt: null, pending: null }
const listeners: Set<(state: AutoUpdateState) => void> = new Set()
/** 正在進行中的檢查/下載，防止啟動自動檢查與使用者手動點選撞在一起重複下載 */
let inFlight: Promise<void> | null = null
let checkTimer: ReturnType<typeof setInterval> | null = null

// 訂閱 Rust 端下載真實進度事件，驅動進度顯示
void listen<{ downloadedBytes: number; totalBytes: number; percent: number; status: string; error: string | null }>(
  'update-download-progress',
  (event) => {
    const { percent } = event.payload
    setState({ downloadPercent: percent })
  },
)

function setState(patch: Partial<AutoUpdateState>) {
  currentState = { ...currentState, ...patch }
  listeners.forEach((cb) => cb(currentState))
}

export function getAutoUpdateState() {
  return currentState
}

export function onAutoUpdateChange(cb: (state: AutoUpdateState) => void) {
  listeners.add(cb)
  return () => { listeners.delete(cb) }
}

/**
 * 是否有已下載、等著裝的更新 —— 「關於」圖示變綠和安裝按鈕出現的條件。
 * 只看 pending，**不看 phase**：後台在跑週期檢查的時候，待安裝這件事照樣成立。
 */
export function hasPendingUpdate(state: AutoUpdateState = currentState): boolean {
  return !!state.pending
}

async function savePending(pending: PendingUpdate): Promise<void> {
  // Rust 的退出兜底安裝直接讀這一條設定，所以它必須先落盤、再進記憶體狀態。
  await setSetting(PENDING_UPDATE_KEY, pending)
  setState({ pending, downloadPercent: 100, installError: null })
}

async function clearPending(): Promise<void> {
  await setSetting(PENDING_UPDATE_KEY, null).catch(() => { })
  setState({ pending: null, installError: null })
}

/**
 * 伺服器地址變更時丟棄已下載的待安裝包（更新來源跟隨那個地址，見 getUpdateBaseUrl）。
 *
 * 必須做：`ensureDownloaded` 只按版本號判斷"這個包已經在盤上了"，而版本號相同不代表
 * 來自同一台伺服器、內容也相同。留著它的後果是"把地址指到測試伺服器驗一遍"實際裝的
 * 還是上一個來源那個包 —— 測試看起來透過了，測的卻不是目標產物。
 * versionInfo 一起清掉，否則介面還掛著舊地址那次檢查的結論。
 */
export async function discardPendingForChannelSwitch(): Promise<void> {
  await clearPending()
  setState({ versionInfo: null, checkedAt: null, downloadPercent: 0, error: null })
}

/**
 * 恢復上次已下載的包。
 *
 * 兩種情況要作廢這條記錄：
 *  · 它的版本已經不比當前版本新了（說明已經裝上了，或使用者手動裝了更新的版本）；
 *  · 檔案沒了或雜湊對不上（臨時目錄被清理軟體掃過，或下載後被替換）。
 * 不作廢的話，退出時會拿一個壞包或舊包去裝。
 */
async function restorePending(current: string): Promise<PendingUpdate | null> {
  const raw = await getSetting<PendingUpdate | null>(PENDING_UPDATE_KEY, null).catch(() => null)
  if (!raw || !raw.filePath || !raw.version) return null

  if (compareVersions(current, raw.version) <= 0) {
    addRuntimeEvent('info', 'update', 'discarding a pending package that is no longer newer', {
      pending: raw.version,
      current,
    })
    await clearPending()
    return null
  }

  const usable = await bridge.verifyUpdatePackage(raw.filePath, raw.sha512 ?? null).catch(() => false)
  if (!usable) {
    addRuntimeEvent('info', 'update', 'previously downloaded package is gone or corrupt, will re-download')
    await clearPending()
    return null
  }
  return raw
}

/** 下載安裝包。已經有同版本的待安裝包就直接跳過，不重下。 */
async function ensureDownloaded(info: VersionInfo): Promise<void> {
  const version = info.latestVersion
  if (!version || !info.downloadUrl) return
  if (currentState.pending?.version === version) {
    addRuntimeEvent('info', 'update', 'package for this version is already on disk, not downloading again', { version })
    return
  }

  setState({ phase: 'downloading', error: null, downloadPercent: 0 })
  try {
    const filePath = await bridge.downloadUpdate(info.downloadUrl, info.sha512)
    await savePending({ version, filePath, sha512: info.sha512 })
    addRuntimeEvent('info', 'update', `version ${version} downloaded and ready to install`)
  } catch (err) {
    // 下載失敗不打擾使用者：下一次週期檢查會重試。
    setState({ error: String(err) })
    addRuntimeEvent('warn', 'update', 'update download failed', { error: String(err) })
  }
}

/** 檢查一次，發現新版本就在後台下載。所有觸發路徑最終都走這裡。 */
const UPDATES_DISABLED = true as boolean // [自訂]

async function runCheckAndDownload(): Promise<void> {
  if (UPDATES_DISABLED) return // [自訂] 自動更新已停用（手動「檢查更新」也不會連網）
  if (currentState.phase === 'installing') return
  if (inFlight) { await inFlight; return }

  const current = readCurrentVersion()
  if (!current) {
    // 走到這裡說明構建期的版本號注入沒生效，檢查無從進行。必須留聲 ——
    // 靜默返回的話，"沒有新版本"和"讀不到自己的版本"在外部看起來完全一樣。
    addRuntimeEvent('error', 'update', 'cannot read the app version, update check skipped')
    return
  }

  const task = (async () => {
    setState({ phase: 'checking', error: null })
    const info = await checkVersionUpdate(current)
    setState({ versionInfo: info, checkedAt: Date.now() })

    // 無條件記一條：這是判斷"更新到底跑沒跑、看到了什麼"的唯一依據。
    // source 是**實際**取到 manifest 的地址：與配置裡的地址不一致就說明發生了回落
    // （配置的伺服器上沒有 manifest）。少了它，"回落了"和"配置的地址就是官方"
    // 在日志里长得一模一样。
    addRuntimeEvent(info.error ? 'warn' : 'info', 'update', 'update check finished', {
      current,
      latest: info.latestVersion,
      hasUpdate: info.hasUpdate,
      error: info.error,
      source: info.sourceUrl,
      configured: getUpdateBaseUrl(),
    })

    if (!info.hasUpdate || !info.downloadUrl) return
    await ensureDownloaded(info)
  })()

  inFlight = task
    .catch((err) => {
      addRuntimeEvent('error', 'update', 'update check threw', { error: String(err) })
      setState({ error: String(err) })
    })
    .finally(() => {
      inFlight = null
      // 無論走哪條分支、成功還是拋異常，活幹完就回 idle。
      // 這裡是唯一的收口點 —— 讓每條早退分支各自記得復位 phase，就是上一版
      // 永久卡在 'checking' 的原因。'installing' 不能碰：那時應用正在退出。
      if (currentState.phase === 'checking' || currentState.phase === 'downloading') {
        setState({ phase: 'idle' })
      }
    })
  await inFlight
}

/**
 * 啟動更新服務：恢復上次下載的包 → 立即檢查一次 → 之後按 CHECK_INTERVAL_MS 週期檢查。
 * 在 App.tsx 掛載時呼叫一次。
 */
export async function startUpdateService(): Promise<void> {
  // [自訂] 自動更新已停用：不檢查、不下載、不安裝。更新方式改為 git pull 後自行建置。
  addRuntimeEvent('info', 'update', 'update service disabled in this build')
  return
}

export async function _disabledStartUpdateService(): Promise<void> {
  // 整個函式包一層：呼叫方是 `void startUpdateService()`，沒有 catch，
  // 這裡任何一處拋異常都會被 Promise 靜默吞掉，表現成"更新功能毫無反應"。
  try {
    const current = readCurrentVersion()
    // 第一條日誌無條件寫，且帶上我們認為自己是哪個版本 —— 排查時先看這行在不在，
    // 不在就說明服務壓根沒啟動，在就往下看 check finished 那行看到了什麼。
    //
    // channel 也必須記：曾經排查過一次"新版沒被檢測到"，真相是包發到了 dev 通道而
    // 客戶端查的是生產通道，而當時的日誌裡完全看不出查的是哪個地址，只能靠手動
    // curl 兩個域名對比才發現（見 pitfalls #26）。
    addRuntimeEvent(current ? 'info' : 'error', 'update', 'update service starting', {
      currentVersion: current ?? '(unreadable)',
      channel: getUpdateBaseUrl(),
    })

    // 更新來源跟隨伺服器地址，被帶到非官方地址時留一條痕跡。
    // 這不一定是錯的（自建後端、或者故意指到測試伺服器都會走到這裡），但它是
    // "為什麼沒收到新版"最常見的原因，所以必須能從日誌裡一眼看到。
    if (!isOfficialUpdateChannel()) {
      addRuntimeEvent('info', 'update', 'update source follows a custom server address, not the official channel', {
        channel: getUpdateBaseUrl(),
        official: getOfficialUpdateBaseUrl(),
      })
    }

    // 這個開關的 UI 已經撤掉（更新不再讓使用者關），但**保留讀取**：
    // 更新鏈路自己出故障時（0.0.8 那次把使用者鎖在死迴圈裡），這是唯一不用發新版
    // 就能遠端指導使用者止血的通道。預設 true，絕大多數使用者根本不知道它存在。
    const enabled = await getSetting('autoCheckUpdate', true).catch(() => true)
    if (!enabled) {
      addRuntimeEvent('warn', 'update', 'update checks disabled by the autoCheckUpdate setting')
      return
    }

    if (current) {
      const pending = await restorePending(current)
      if (pending) {
        addRuntimeEvent('info', 'update', 'reusing a package downloaded earlier', { version: pending.version })
        setState({ pending, downloadPercent: 100 })
      }
    }

    await runCheckAndDownload()

    if (checkTimer === null) {
      checkTimer = setInterval(() => { void runCheckAndDownload() }, CHECK_INTERVAL_MS)
    }
  } catch (err) {
    addRuntimeEvent('error', 'update', 'update service failed to start', { error: String(err) })
  }
}

/**
 * 手動檢查（關於頁「檢查更新」按鈕）。
 * 發現新版本同樣會在後台開始下載 —— 與自動路徑同一套行為，不再是"檢查完直接裝掉"。
 */
export async function checkForUpdateNow(): Promise<VersionInfo | null> {
  await runCheckAndDownload()
  return currentState.versionInfo ?? null
}

/**
 * 使用者主動安裝：應用會立刻關閉、靜默安裝、再自動開啟。
 * 呼叫方透過卡片或關於頁的安裝按鈕說明重啟行為；錄音和轉寫期間禁止安裝。
 */
export async function installPendingUpdate(): Promise<void> {
  const pending = currentState.pending
  if (!pending || currentState.phase === 'installing') return
  if (getRecorderState() !== 'idle') {
    setState({ installError: t('update.finishRecording'), error: t('update.finishRecording') })
    return
  }
  setState({ phase: 'installing', error: null, installError: null })
  try {
    // relaunch=true：這是使用者當下主動要求的更新，裝完把應用重新拉起來。
    await bridge.installDownloadedUpdate(pending.filePath, true)
  } catch (err) {
    // 裝不起來就回 idle：pending 還在，使用者可以再點一次，退出時也仍會兜底
    setState({ phase: 'idle', error: String(err), installError: String(err) })
    addRuntimeEvent('error', 'update', 'failed to launch the installer', { error: String(err) })
  }
}
