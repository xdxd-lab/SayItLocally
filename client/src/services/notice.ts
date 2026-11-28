// 遠端公告服務 — 與更新機制完全解耦的服務端可控通知通道。
//
// 即使自動更新出問題、或不想讓使用者更新軟體，也能透過後端下發一條公告，
// 在 App 內提示使用者（例如引導去官網/GitHub 手動下載新版）。
//
// 安全約束（遠端內容視為不可信輸入）：
//   - 文案一律按純文本渲染（React 預設轉義），絕不當 HTML 注入。
//   - 連結只允許 https，且用系統瀏覽器開啟，絕不導航 WebView 自身。

import { getBackendBaseUrl } from './runtimeConfig'
import { getSetting, setSetting } from './store'
import { getLocale, isLocale, LEGACY_ZH_LOCALE, type Locale } from '@/i18n'

export type NoticeLevel = 'info' | 'warning' | 'critical'

export interface RemoteNotice {
  /** 唯一 id：使用者關閉後按 id 記憶，不再重複彈 */
  id: string
  /** 級別，決定 banner 配色與圖示 */
  level: NoticeLevel
  title: string
  body?: string
  /** 行動按鈕連結（僅 https） */
  linkUrl?: string
  linkLabel?: string
  /** 只對 >= minVersion 的客戶端顯示 */
  minVersion?: string | null
  /** 只對 <= maxVersion 的客戶端顯示（例如只提示舊版使用者去手動更新） */
  maxVersion?: string | null
  /** 生效時間窗（ISO 字串），可選 */
  startAt?: string | null
  endAt?: string | null
  /** 是否允許關閉，預設 true */
  dismissible?: boolean
}

interface RemoteNoticeTranslation {
  title?: string
  body?: string
  linkLabel?: string
}

interface RemoteNoticePayload extends RemoteNotice {
  /** Optional locale overrides. Top-level strings remain the fallback for old clients. */
  translations?: Partial<Record<Locale, RemoteNoticeTranslation>>
}

const DISMISSED_KEY = 'dismissedNoticeIds'

function compareVersions(a: string, b: string): number {
  const pa = a.split('.').map(Number)
  const pb = b.split('.').map(Number)
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const d = (pa[i] || 0) - (pb[i] || 0)
    if (d !== 0) return d
  }
  return 0
}

function isTranslation(value: unknown): value is RemoteNoticeTranslation {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false
  const item = value as Record<string, unknown>
  return ['title', 'body', 'linkLabel'].every((key) => (
    item[key] === undefined || typeof item[key] === 'string'
  ))
}

function isValidNotice(v: unknown): v is RemoteNoticePayload {
  if (!v || typeof v !== 'object') return false
  const n = v as Record<string, unknown>
  const levelOk = n.level === 'info' || n.level === 'warning' || n.level === 'critical'
  const translations = n.translations as Record<string, unknown> | undefined
  const translationsOk = translations === undefined || (
    translations !== null
    && typeof translations === 'object'
    && !Array.isArray(translations)
    && Object.entries(translations).every(([locale, value]) => (
      (isLocale(locale) || locale === LEGACY_ZH_LOCALE) && isTranslation(value)
    ))
  )
  return typeof n.id === 'string' && n.id.length > 0
    && typeof n.title === 'string' && n.title.length > 0
    && (n.body === undefined || typeof n.body === 'string')
    && (n.linkLabel === undefined || typeof n.linkLabel === 'string')
    && translationsOk
    && levelOk
}

/** Resolve a server payload into display-ready text while keeping legacy string fields valid. */
export function normalizeRemoteNotice(value: unknown, locale: Locale): RemoteNotice | null {
  if (!isValidNotice(value)) return null
  const translation = value.translations?.[locale]
  return {
    ...value,
    title: translation?.title || value.title,
    body: translation?.body ?? value.body,
    linkLabel: translation?.linkLabel ?? value.linkLabel,
  }
}

function matchesVersion(notice: RemoteNotice, current: string): boolean {
  if (notice.minVersion && compareVersions(current, notice.minVersion) < 0) return false
  if (notice.maxVersion && compareVersions(current, notice.maxVersion) > 0) return false
  return true
}

function withinTimeWindow(notice: RemoteNotice, now = Date.now()): boolean {
  if (notice.startAt) {
    const t = Date.parse(notice.startAt)
    if (!Number.isNaN(t) && now < t) return false
  }
  if (notice.endAt) {
    const t = Date.parse(notice.endAt)
    if (!Number.isNaN(t) && now > t) return false
  }
  return true
}

async function loadRaw(): Promise<unknown> {
  // [自訂] 啟動公告已停用：不發出任何網路請求。
  return null
}

export async function _disabledLoadRaw(): Promise<unknown> {
  // 開發預覽：dev 環境下可在控制台執行
  //   localStorage.setItem('__devNotice', JSON.stringify({ id:'t1', level:'warning', title:'測試公告', body:'內容…', linkUrl:'https://sayitapp.site', linkLabel:'前往官網' }))
  // 刷新即可看到 banner 效果，无需后端。清除：localStorage.removeItem('__devNotice')
  if (import.meta.env.DEV) {
    try {
      const dev = localStorage.getItem('__devNotice')
      if (dev) return JSON.parse(dev)
    } catch {
      /* ignore */
    }
  }

  const baseUrl = getBackendBaseUrl()
  const resp = await fetch(`${baseUrl}/api/notice`, {
    cache: 'no-store',
    signal: AbortSignal.timeout(8000),
  })
  if (!resp.ok) return null
  return await resp.json()
}

/** 拉取當前應展示的公告；無公告/不適用當前版本/不在時間窗/已被使用者關閉 → 返回 null。 */
export async function fetchActiveNotice(
  currentVersion: string,
  locale: Locale = getLocale(),
): Promise<RemoteNotice | null> {
  try {
    const raw = await loadRaw()
    const notice = normalizeRemoteNotice(raw, locale)
    if (!notice) return null
    if (!matchesVersion(notice, currentVersion)) return null
    if (!withinTimeWindow(notice)) return null
    if (notice.dismissible !== false) {
      const dismissed = await getSetting<string[]>(DISMISSED_KEY, [])
      if (Array.isArray(dismissed) && dismissed.includes(notice.id)) return null
    }
    return notice
  } catch {
    return null
  }
}

/** 記住某條公告已被使用者關閉（按 id）。 */
export async function dismissNotice(id: string): Promise<void> {
  const dismissed = await getSetting<string[]>(DISMISSED_KEY, [])
  const list = Array.isArray(dismissed) ? dismissed : []
  if (!list.includes(id)) {
    // 只保留最近 50 条，避免无限增长
    await setSetting(DISMISSED_KEY, [...list, id].slice(-50))
  }
}

/** 校驗是否為安全的 https 外鏈。 */
export function isSafeHttpsUrl(url: string | undefined): url is string {
  if (!url) return false
  try {
    return new URL(url).protocol === 'https:'
  } catch {
    return false
  }
}
