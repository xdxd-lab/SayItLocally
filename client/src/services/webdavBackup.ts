// WebDAV 備份的前端封裝。
//
// 備份檔位（是否含歷史/音訊）**只存在設定裡**，不作為引數傳給 Rust：手動「立即備份」
// 和後台定時備份走同一個命令、讀同一份設定，介面上勾了什麼就一定是上傳的內容。
//
// 錯誤處理約定：Rust 對可預期的使用者輸入/伺服器問題返回大寫錯誤碼（WEBDAV_*），
// 其餘返回帶 HTTP 狀態碼的英文說明。`describeWebDavError` 把前者翻成本地化文案，
// 後者原樣透出——那類資訊（狀態碼、響應體片段）才是排查時真正需要的。

import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { t, type TranslationKey } from '@/i18n'
import { describeBackupTransferError } from './backup'

/** 堅果雲的 WebDAV 根地址。 */
export const JIANGUOYUN_DAV_BASE = 'https://dav.jianguoyun.com/dav'

/**
 * 位址列的預填值。
 *
 * **必須是 `/dav` 下的一個子目錄，不能是 `/dav` 本身。** 堅果雲的 `/dav` 根只列同步
 * 資料夾，往根上 PUT 檔案會被拒 —— 各家 WebDAV 客戶端的堅果雲配置說明都要求先建
 * 一個一級目錄再把地址指向它。目錄不存在時我們自己 MKCOL 出來，所以使用者不用先去
 * 網頁上手動建。
 */
export const DEFAULT_DAV_URL = `${JIANGUOYUN_DAV_BASE}/SayIt`

export interface WebDavConfig {
  url: string
  username: string
  password: string
}

export interface WebDavEntry {
  name: string
  size: number
}

export interface WebDavBackupResult {
  fileName: string
  bytes: number
  includeHistory: boolean
  includeAudio: boolean
  finishedAt: number
  /** 本次清理掉的舊備份份數 */
  pruned: number
}

/** 上次備份的結果。失敗也會記 —— 靜默失敗的備份比沒有備份更糟。 */
export interface WebDavLastResult {
  at: number
  ok: boolean
  fileName: string
  bytes: number
  includeHistory: boolean
  includeAudio: boolean
  error: string | null
}

export interface WebDavProgress {
  status: 'running' | 'completed' | 'failed'
  phase:
  | 'preparing'
  | 'packingData'
  | 'packingAudio'
  | 'finalizing'
  | 'uploading'
  | 'verifying'
  | 'completed'
  | 'failed'
  fileName: string
  currentFile: string | null
  processedBytes: number
  totalBytes: number
  percent: number
  error: string | null
}

/** Rust 側返回的錯誤碼 → 文案鍵。沒登記的碼按原樣顯示。 */
const ERROR_KEYS: Record<string, TranslationKey> = {
  WEBDAV_URL_EMPTY: 'webdav.error.urlEmpty',
  WEBDAV_URL_INSECURE: 'webdav.error.urlInsecure',
  WEBDAV_URL_SCHEME: 'webdav.error.urlScheme',
  WEBDAV_CREDENTIALS_EMPTY: 'webdav.error.credentialsEmpty',
  WEBDAV_UNAUTHORIZED: 'webdav.error.unauthorized',
  WEBDAV_MKCOL_CONFLICT: 'webdav.error.mkcolConflict',
  WEBDAV_VERIFY_MISSING: 'webdav.error.verifyMissing',
  WEBDAV_VERIFY_SIZE: 'webdav.error.verifySize',
  WEBDAV_EMPTY_DOWNLOAD: 'webdav.error.emptyDownload',
  WEBDAV_BAD_NAME: 'webdav.error.badName',
}

/**
 * 把 Rust 丟擲的錯誤變成可以直接顯示的一句話。
 *
 * invoke 丟擲的是字串本身（Tauri 把 Err(String) 原樣傳過來），但外面可能已經被
 * 包成 Error，所以兩種都要認。
 */
export function describeWebDavError(error: unknown): string {
  const raw = error instanceof Error ? error.message : String(error)
  const code = raw.trim()
  const key = ERROR_KEYS[code]
  if (key) return t(key)
  const host = unreachableHost(code)
  return host ? t('webdav.error.unreachable', { host }) : describeBackupTransferError(raw)
}

/**
 * 請求根本沒發出去（DNS、斷網、超時、被攔）時，Rust 側是「MKCOL failed: error sending request
 * for url (https://…)」這種 reqwest 原文。這一類對使用者只有一個意思：連不上伺服器。
 * 返回地址裡的主機名；不是這一類返回 null。
 */
function unreachableHost(raw: string): string | null {
  const match = /failed: error sending request for url \(https?:\/\/([^/)\s:]+)/i.exec(raw)
  return match ? match[1] : null
}

/** 翻譯過的錯誤，原文留給「詳情」（排查時要看）；本來就按原樣顯示的返回 null。 */
export function webDavErrorDetail(error: unknown): string | null {
  const raw = (error instanceof Error ? error.message : String(error)).trim()
  return raw && describeWebDavError(raw) !== raw ? raw : null
}

export function onWebDavBackupProgress(
  handler: (progress: WebDavProgress) => void,
): Promise<UnlistenFn> {
  return listen<WebDavProgress>('webdav-backup-progress', (event) => handler(event.payload))
}

/** 連線測試。返回伺服器上已有的備份份數。 */
export function testWebDavConnection(config: WebDavConfig): Promise<number> {
  return invoke<number>('webdav_test', { config })
}

/** 列出伺服器上的備份，從新到舊。 */
export function listWebDavBackups(): Promise<WebDavEntry[]> {
  return invoke<WebDavEntry[]>('webdav_list')
}

/** 立即備份一次。檔位取自設定。 */
export function runWebDavBackup(): Promise<WebDavBackupResult> {
  return invoke<WebDavBackupResult>('webdav_backup_now')
}

/** 從伺服器恢復。完成後需要由呼叫方觸發重啟（與本地匯入一致）。 */
export function restoreWebDavBackup(name: string): Promise<void> {
  return invoke('webdav_restore', { name })
}
