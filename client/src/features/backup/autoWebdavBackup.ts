// WebDAV 自動備份的排程。
//
// 結構照著 features/update/autoUpdate.ts 抄：模組級 inFlight 鎖 + setInterval + 一個
// 小狀態store。專案裡只有這一種週期任務 pattern，別再引第二種。
//
// 三條約束值得單獨說明：
//
// 1. **判據是時間戳，不是 tick 計數。** setInterval 只在應用執行期計時，休眠和關機
//    期間不補償。按 tick 數算「到 24 小時了嗎」，一台每天只開兩小時的機器要 12 天
//    才備份一次。所以每次 tick 都拿 `Date.now()` 和上次成功時間比。
// 2. **失敗後要退避。** 堅果雲對 WebDAV 有請求頻率限制，地址填錯時每 15 分鐘重試
//    一次除了刷日誌沒有別的作用，還可能把賬號打進限流。
// 3. **enabled 每次 tick 都重讀。** 使用者在設定裡開啟開關後不該要求重啟應用。

import { getSetting, setSetting } from '@/services/store'
import { addRuntimeEvent } from '@/services/debugLog'
import {
  runWebDavBackup,
  type WebDavBackupResult,
  type WebDavLastResult,
} from '@/services/webdavBackup'

/** 上次成功備份的時刻（ms）。間隔判定只看這個。 */
const KEY_LAST_BACKUP_AT = 'webdav.lastBackupAt'
/** 上次嘗試的時刻（ms），成功失敗都記。失敗退避用它。 */
const KEY_LAST_ATTEMPT_AT = 'webdav.lastAttemptAt'

/**
 * 輪詢週期。它**不是**備份間隔 —— 備份間隔由 webdav.intervalHours 決定，這裡只是
 * 「多久去看一眼該不該備份了」。15 分鐘足夠讓「剛過 24 小時」這件事被及時發現，
 * 每次開銷是讀三條設定。
 */
const TICK_MS = 15 * 60 * 1000

/**
 * 啟動後第一次檢查的延遲。
 *
 * 不在掛載時立刻跑：那正是載入本地模型、恢復視窗、檢查更新最擠的時候，一個可能
 * 要壓幾百 MB 的打包擠進去只會讓啟動更慢。兩分鐘後再看，使用者感知不到差別。
 */
const FIRST_CHECK_DELAY_MS = 2 * 60 * 1000

/** 失敗後至少等這麼久再試，避免撞上網盤的請求頻率限制。 */
const FAILURE_BACKOFF_MS = 60 * 60 * 1000

export interface WebDavBackupState {
  /** 正在備份。UI 用它停用按鈕、顯示進度。 */
  running: boolean
  lastResult: WebDavLastResult | null
}

let currentState: WebDavBackupState = { running: false, lastResult: null }
const listeners = new Set<(state: WebDavBackupState) => void>()
/** 正在進行中的備份，防止定時任務和使用者手動點選撞在一起傳兩份。 */
let inFlight: Promise<WebDavBackupResult | null> | null = null
let tickTimer: ReturnType<typeof setInterval> | null = null
let firstCheckTimer: ReturnType<typeof setTimeout> | null = null

function setState(patch: Partial<WebDavBackupState>) {
  currentState = { ...currentState, ...patch }
  listeners.forEach((cb) => cb(currentState))
}

export function getWebDavBackupState(): WebDavBackupState {
  return currentState
}

export function onWebDavBackupChange(cb: (state: WebDavBackupState) => void) {
  listeners.add(cb)
  return () => { listeners.delete(cb) }
}

/** 從設定讀回上次結果（Rust 側在每次備份後寫入）。 */
export async function refreshLastResult(): Promise<WebDavLastResult | null> {
  const value = await getSetting<WebDavLastResult | null>('webdav.lastResult', null).catch(() => null)
  const lastResult = value && typeof value === 'object' ? value : null
  setState({ lastResult })
  return lastResult
}

/**
 * 跑一次備份。手動按鈕和定時任務共用。
 *
 * 無論成功失敗都寫 lastAttemptAt，成功才寫 lastBackupAt —— 失敗不推進備份時間，
 * 否則一次失敗會讓下一次備份被推遲整個間隔。
 */
export async function runBackupNow(trigger: 'manual' | 'schedule'): Promise<WebDavBackupResult | null> {
  if (inFlight) return inFlight

  const task = (async (): Promise<WebDavBackupResult | null> => {
    setState({ running: true })
    const startedAt = Date.now()
    await setSetting(KEY_LAST_ATTEMPT_AT, startedAt).catch(() => undefined)
    try {
      const result = await runWebDavBackup()
      await setSetting(KEY_LAST_BACKUP_AT, Date.now()).catch(() => undefined)
      addRuntimeEvent('info', 'webdav', 'backup finished', {
        trigger,
        fileName: result.fileName,
        bytes: result.bytes,
        includeHistory: result.includeHistory,
        includeAudio: result.includeAudio,
        pruned: result.pruned,
        elapsedMs: Date.now() - startedAt,
      })
      return result
    } catch (error) {
      // 失敗必須留聲，而且要能和「沒開啟備份」區分開。一個靜默失敗幾個月的備份
      // 在界面上和一个正常工作的备份长得一模一样。
      addRuntimeEvent('error', 'webdav', 'backup failed', { trigger, error: String(error) })
      return null
    }
  })()

  inFlight = task
  try {
    return await task
  } finally {
    inFlight = null
    setState({ running: false })
    await refreshLastResult()
  }
}

/** 到點了嗎。讀設定而不是快取，使用者改了間隔立刻生效。 */
async function shouldBackupNow(): Promise<boolean> {
  const enabled = await getSetting('webdav.enabled', false).catch(() => false)
  if (!enabled) return false

  // 地址或憑證沒配完就別去打包：打完才發現傳不上去純屬白燒一次 CPU 和磁碟。
  const [url, username, password] = await Promise.all([
    getSetting('webdav.url', '').catch(() => ''),
    getSetting('webdav.username', '').catch(() => ''),
    getSetting('webdav.password', '').catch(() => ''),
  ])
  if (!url.trim() || !username.trim() || !password) return false

  const now = Date.now()
  const lastAttemptAt = await getSetting(KEY_LAST_ATTEMPT_AT, 0).catch(() => 0)
  const lastBackupAt = await getSetting(KEY_LAST_BACKUP_AT, 0).catch(() => 0)

  // 上次嘗試失敗（嘗試比成功新）→ 走退避，別每 15 分鐘撞一次限流。
  if (lastAttemptAt > lastBackupAt && now - lastAttemptAt < FAILURE_BACKOFF_MS) return false

  const intervalHours = await getSetting('webdav.intervalHours', 24).catch(() => 24)
  const intervalMs = Math.max(1, intervalHours) * 60 * 60 * 1000
  // lastBackupAt 為 0 = 從沒成功備份過，開啟後第一次檢查就該跑。
  return now - lastBackupAt >= intervalMs
}

async function tick() {
  try {
    if (await shouldBackupNow()) {
      await runBackupNow('schedule')
    }
  } catch (error) {
    // tick 自己不能拋：它跑在 setInterval 裡，丟擲去就是一個沒人接的
    // unhandled rejection，而且下一次 tick 照舊 —— 表現成毫無痕跡。
    addRuntimeEvent('error', 'webdav', 'backup tick threw', { error: String(error) })
  }
}

/**
 * 啟動自動備份服務。在 App.tsx 掛載時呼叫一次。
 *
 * 即使當前沒開啟也會起 ticker：開關是每次 tick 重讀的，這樣使用者在設定裡開啟後
 * 不需要重啟應用。代價是每 15 分鐘讀一次設定。
 */
export async function startWebDavBackupService(): Promise<void> {
  // 整個函式包一層：呼叫方是 `void startWebDavBackupService()`，異常會被 Promise
  // 靜默吞掉，表現成「自動備份從來沒跑過」且不留痕跡。
  try {
    await refreshLastResult()
    const enabled = await getSetting('webdav.enabled', false).catch(() => false)
    // 無條件記一條：排查時先看這行在不在，不在就說明服務壓根沒啟動。
    addRuntimeEvent('info', 'webdav', 'backup service starting', {
      enabled,
      intervalHours: await getSetting('webdav.intervalHours', 24).catch(() => 24),
      includeHistory: await getSetting('webdav.includeHistory', false).catch(() => false),
      includeAudio: await getSetting('webdav.includeAudio', false).catch(() => false),
      lastBackupAt: await getSetting(KEY_LAST_BACKUP_AT, 0).catch(() => 0),
    })

    if (firstCheckTimer === null) {
      firstCheckTimer = setTimeout(() => { void tick() }, FIRST_CHECK_DELAY_MS)
    }
    if (tickTimer === null) {
      tickTimer = setInterval(() => { void tick() }, TICK_MS)
    }
  } catch (error) {
    addRuntimeEvent('error', 'webdav', 'backup service failed to start', { error: String(error) })
  }
}
