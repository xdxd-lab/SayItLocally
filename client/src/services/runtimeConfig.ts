import * as bridge from './bridge'
import { t } from '@/i18n'

declare const __SAYIT_DEFAULT_SERVER_URL__: string

const BUILTIN_DEFAULT_SERVER_URL =
  typeof __SAYIT_DEFAULT_SERVER_URL__ === 'string' && __SAYIT_DEFAULT_SERVER_URL__.trim()
    ? __SAYIT_DEFAULT_SERVER_URL__.trim()
    : 'https://sayitapp.site'

const BACKEND_BASE_URL_STORE_KEY = 'backendBaseUrl'

interface RuntimeEnv {
  VITE_BACKEND_BASE_URL?: string
  VITE_WS_URL?: string
  DEV?: boolean
}

function getEnv(): RuntimeEnv {
  return (import.meta as unknown as { env?: RuntimeEnv }).env || {}
}

function trimSlash(url: string): string {
  return url.replace(/\/+$/, '')
}

function normalizeUrl(value: string | null | undefined): string {
  return trimSlash(String(value || '').trim())
}

function resolveBuiltinDefaultBaseUrl(): string {
  const env = getEnv()
  const value = normalizeUrl(env.VITE_BACKEND_BASE_URL)
  if (value) return value
  return trimSlash(BUILTIN_DEFAULT_SERVER_URL)
}

function resolveEnvOverrideBaseUrl(): string {
  const env = getEnv()
  return normalizeUrl(env.VITE_BACKEND_BASE_URL)
}

let backendBaseUrl = resolveBuiltinDefaultBaseUrl()

export async function initRuntimeConfig(): Promise<void> {
  // 使用者主動儲存的地址優先於環境變數
  const stored = await bridge.storeGet(BACKEND_BASE_URL_STORE_KEY)
  const normalized = normalizeUrl(typeof stored === 'string' ? stored : '')
  if (normalized) {
    backendBaseUrl = normalized
    return
  }
  const envOverride = resolveEnvOverrideBaseUrl()
  if (envOverride) {
    backendBaseUrl = envOverride
    return
  }
  backendBaseUrl = resolveBuiltinDefaultBaseUrl()
}

export function getDefaultBackendBaseUrl(): string {
  return resolveBuiltinDefaultBaseUrl()
}

export function getBackendBaseUrl(): string {
  return backendBaseUrl
}

/**
 * 檢查更新時用的地址 —— **跟隨** getBackendBaseUrl()：伺服器地址填哪個，就找哪個要更新。
 *
 * 這麼定是為了「填一個地址就能整體切到測試環境」這件事足夠簡單，不用第二個設定項。
 * 代價與配套約束（改這裡之前先看 .kiro/decisions.md 的「更新通道」一節）：
 *  · 自建後端的使用者，他的伺服器上沒有 manifest。**所以 checkVersionUpdate 必須保留
 *    回落到官方地址的那一步** —— 沒有它，這些使用者會拿到 404、被靜默當成"沒有新版"，
 *    從此永遠收不到更新。那一步不是最佳化，是這個設計能成立的前提。
 *  · 更新包沒有簽名（所以沒用 Tauri 的 updater 外掛），manifest 的 SHA-512 只證明
 *    "下載的位元組與 manifest 一致"、不證明"這個包是我們發的"。也就是說這個地址同時
 *    決定了「誰能在這台機器上裝程式」。因此非官方地址時，關於頁必須把更新來源顯示
 *    出來（見 isOfficialUpdateChannel 的用處），別讓它悄無聲息。
 */
export function getUpdateBaseUrl(): string {
  return getBackendBaseUrl()
}

/** 內建的官方更新地址。回落用，也用於判斷當前是否偏離了官方通道。 */
export function getOfficialUpdateBaseUrl(): string {
  return resolveBuiltinDefaultBaseUrl()
}

/**
 * 更新是否來自官方地址。
 * 為 false 時關於頁會把實際來源顯示出來 —— 一台從別處取更新的機器如果看起來和
 * 正常機器一樣，早晚會被當成正常機器用。
 */
export function isOfficialUpdateChannel(): boolean {
  return normalizeUrl(getUpdateBaseUrl()) === normalizeUrl(getOfficialUpdateBaseUrl())
}

export async function setBackendBaseUrl(value: string): Promise<string> {
  const normalized = normalizeUrl(value)
  if (!normalized) {
    throw new Error(t('runtimeConfig.emptyServerUrl'))
  }
  backendBaseUrl = normalized
  await bridge.storeSet(BACKEND_BASE_URL_STORE_KEY, normalized)
  return backendBaseUrl
}

export async function resetBackendBaseUrl(): Promise<string> {
  backendBaseUrl = resolveBuiltinDefaultBaseUrl()
  await bridge.storeDelete(BACKEND_BASE_URL_STORE_KEY)
  return backendBaseUrl
}

export async function getStoredBackendBaseUrl(): Promise<string> {
  const stored = await bridge.storeGet(BACKEND_BASE_URL_STORE_KEY)
  return normalizeUrl(typeof stored === 'string' ? stored : '')
}

export function getWSUrl(): string {
  const env = getEnv()
  const explicit = normalizeUrl(env.VITE_WS_URL)
  if (explicit) return explicit

  const base = getBackendBaseUrl()
  if (base.startsWith('https://')) return `${base.replace(/^https:\/\//, 'wss://')}/ws/transcribe`
  if (base.startsWith('http://')) return `${base.replace(/^http:\/\//, 'ws://')}/ws/transcribe`
  return `${resolveBuiltinDefaultBaseUrl().replace(/^https?:\/\//, 'wss://')}/ws/transcribe`
}
