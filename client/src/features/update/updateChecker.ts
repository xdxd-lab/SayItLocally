// 前端版本檢查 — 直接請求後端 manifest API 比較版本號

import { getOfficialUpdateBaseUrl, getUpdateBaseUrl, isOfficialUpdateChannel } from '@/services/runtimeConfig'
import { getClientRuntimeInfo } from '@/services/bridge'
import { IS_MAC } from '@/lib/platform'

export interface VersionInfo {
  hasUpdate: boolean
  currentVersion: string
  latestVersion: string | null
  downloadUrl: string | null
  releaseDate: string | null
  /** 安裝包的 SHA-512（Base64）。下載後由 Rust 側校驗，manifest 沒給就是 null。 */
  sha512: string | null
  error: string | null
  /** 實際取到 manifest 的地址。回落發生時它是官方地址，不等於伺服器設定裡那個。 */
  sourceUrl: string | null
}

/**
 * 返回 >0 表示 latest 比 current 新。
 *
 * 只認純數字的點分段。非數字段（預釋出字尾之類）按 0 處理而不是讓 NaN 傳下去：
 * NaN 參與減法永遠得 NaN，`NaN !== 0` 為真，會讓迴圈在第一段就返回 NaN，
 * 而 `NaN > 0` 是 false —— 結果是"有更新也不報"，且沒有任何報錯。
 */
export function compareVersions(current: string, latest: string): number {
  const parse = (value: string) => value.split('.').map((segment) => {
    const parsed = Number.parseInt(segment, 10)
    return Number.isFinite(parsed) ? parsed : 0
  })
  const a = parse(current)
  const b = parse(latest)
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    const diff = (b[i] || 0) - (a[i] || 0)
    if (diff !== 0) return diff
  }
  return 0
}

/**
 * 檢查更新。先問伺服器設定裡那個地址，拿不到有效 manifest 就回落到官方地址。
 *
 * **回落不是最佳化，是「更新地址跟隨伺服器地址」這個設計能成立的前提。**
 * 把伺服器指向自建後端的使用者，那台機器上不會有 manifest；沒有這一步，他們每次檢查都
 * 拿到 404、被當成"已是最新版"，從此永遠收不到更新，而且毫無徵兆。
 *
 * 只在「沒拿到版本號」時回落 —— 包括 404、網路失敗、以及返回了 JSON 但沒有 version
 * 欄位。已經拿到版本號就用它，哪怕版本比當前的舊（那是測試指向舊通道的正常情況，
 * 不該悄悄換成官方的答案）。
 */
export async function checkVersionUpdate(currentVersion: string): Promise<VersionInfo> {
  const configured = await fetchManifest(currentVersion, getUpdateBaseUrl())
  if (configured.latestVersion || isOfficialUpdateChannel()) return configured

  const fallback = await fetchManifest(currentVersion, getOfficialUpdateBaseUrl())
  // 兩邊都沒拿到就報第一次的結果：錯誤資訊應該指向使用者實際配置的那個地址。
  return fallback.latestVersion ? fallback : configured
}

/**
 * 安裝包地址只能是 manifest 所在伺服器上的路徑。
 *
 * 原來是 `${baseUrl}${download_path}` 直接拼字串：download_path 寫成 `.attacker.example/x.exe`，
 * 拼出來就是 `https://sayitapp.site.attacker.example/x.exe`，域名已經換了。
 * 現在按 URL 解析後比 origin，不同源就當沒有下載地址。Rust 的 download_update 下載前還會再查一次。
 */
export function resolveDownloadUrl(baseUrl: string, downloadPath: string | undefined | null): string | null {
  if (!downloadPath || !downloadPath.startsWith('/') || downloadPath.startsWith('//')) return null
  try {
    const base = new URL(baseUrl)
    const resolved = new URL(downloadPath, base)
    return resolved.origin === base.origin ? resolved.toString() : null
  } catch {
    return null
  }
}

/**
 * 更新通道 `{platform}/{arch}`，與服務端 releases 目錄對應（server/backend/app/releases.py）。
 * Windows 只發 x64；mac 按 CPU 架構分 arm64 / x64（WKWebView 的 UA 在 Apple 晶片上也寫 Intel，
 * 所以架構問 Rust）。
 */
async function updateChannel(): Promise<string> {
  if (!IS_MAC) return 'win32/x64'
  const info = await getClientRuntimeInfo().catch(() => null)
  return info?.arch === 'x86_64' ? 'darwin/x64' : 'darwin/arm64'
}

const UPDATE_CHECK_DISABLED = true as boolean // [自訂]

async function fetchManifest(currentVersion: string, baseUrl: string): Promise<VersionInfo> {
  const base: VersionInfo = {
    hasUpdate: false,
    currentVersion,
    latestVersion: null,
    downloadUrl: null,
    releaseDate: null,
    sha512: null,
    error: null,
    sourceUrl: baseUrl,
  }
  // [自訂] 更新檢查已停用：不連網
  if (UPDATE_CHECK_DISABLED) return base

  try {
    const channel = await updateChannel()
    const resp = await fetch(`${baseUrl}/api/desktop-updates/${channel}/manifest`, {
      cache: 'no-store',
      signal: AbortSignal.timeout(10000),
    })
    if (!resp.ok) {
      base.error = resp.status === 404 ? null : `HTTP ${resp.status}`
      return base
    }
    const manifest = await resp.json() as {
      version?: string
      releaseDate?: string
      download_path?: string
      sha512?: string
    }
    const latestVersion = manifest.version
    if (!latestVersion) return base

    base.latestVersion = latestVersion
    base.releaseDate = manifest.releaseDate || null
    base.sha512 = manifest.sha512 || null
    base.downloadUrl = resolveDownloadUrl(baseUrl, manifest.download_path)
    if (manifest.download_path && !base.downloadUrl) {
      base.error = 'manifest download_path is not on this server'
    }
    base.hasUpdate = compareVersions(currentVersion, latestVersion) > 0
    return base
  } catch (err) {
    base.error = String(err)
    return base
  }
}
