/**
 * 把底層異常翻譯成使用者能據此行動的一句話。
 *
 * 為什麼需要這個：設定頁原來直接把 `String(error)` 貼到介面上，使用者看到的是
 * 「連線失敗：TypeError: Failed to fetch」——這句話不區分「網址打錯了」「本機沒網」
 * 「服務掛了」「被 WAF 攔了」，四種情況的下一步完全不同，使用者無從下手。
 *
 * 原始文本不丟棄：`detail` 保留原文給能看懂的人排查，UI 以次要字號展示。
 */

import { t } from '@/i18n'

export type ErrorActionHint = 'reset_url' | 'check_key' | 'switch_source' | 'retry' | 'none'

export type FriendlyErrorCode =
  | 'server_timeout'
  | 'server_unreachable'
  | 'server_forbidden'
  | 'server_not_sayit'
  | 'server_internal'
  | 'provider_timeout'
  | 'provider_unreachable'
  | 'provider_bad_key'
  | 'provider_forbidden'
  | 'provider_rate_limit'
  | 'provider_insufficient_balance'
  | 'provider_no_model'
  // [自訂] SC-35：本機模型錯誤由 Rust 提供穩定碼。
  | 'local_model_missing'
  | 'local_model_invalid'
  | 'local_model_read'
  | 'model_dir_invalid'
  | 'model_dir_not_directory'
  | 'model_dir_permission'
  | 'model_dir_overlap'
  | 'model_dir_unwritable'
  | 'model_dir_save'
  | 'download_network'
  | 'download_busy'
  | 'download_source_mismatch'
  | 'download_no_space'
  | 'download_permission'
  | 'download_checksum'
  | 'download_failed'
  | 'connect_failed'

export interface FriendlyError {
  /** 穩定分類；可持久化，展示時再按當前介面語言取文案。 */
  code: FriendlyErrorCode
  /** 一句話說清發生了什麼，以及該往哪個方向查 */
  message: string
  /** 原始异常文本，供排查用；UI 以次要字号展示 */
  detail: string
  /** 建議提供的動作按鈕型別，由呼叫方決定要不要渲染 */
  action: ErrorActionHint
}

const ERROR_PROTOCOL_PREFIX = 'sayit_error:'

interface DecodedError {
  code: FriendlyErrorCode | null
  text: string
}

const FRIENDLY_ERROR_CODES = new Set<FriendlyErrorCode>([
  'server_timeout',
  'server_unreachable',
  'server_forbidden',
  'server_not_sayit',
  'server_internal',
  'provider_timeout',
  'provider_unreachable',
  'provider_bad_key',
  'provider_forbidden',
  'provider_rate_limit',
  'provider_insufficient_balance',
  'provider_no_model',
  'local_model_missing',
  'local_model_invalid',
  'local_model_read',
  'model_dir_invalid',
  'model_dir_not_directory',
  'model_dir_permission',
  'model_dir_overlap',
  'model_dir_unwritable',
  'model_dir_save',
  'download_network',
  'download_busy',
  'download_source_mismatch',
  'download_no_space',
  'download_permission',
  'download_checksum',
  'download_failed',
  'connect_failed',
])

/**
 * Rust/Tauri errors use `sayit_error:<stable-code>:<diagnostic detail>`.
 * The code is the contract; the detail may change language and remains useful for diagnostics.
 * Unknown codes deliberately fall back to legacy text classification for forward compatibility.
 */
function decodeError(error: unknown): DecodedError {
  const value = error instanceof Error ? (error.message || String(error)) : String(error)
  if (!value.startsWith(ERROR_PROTOCOL_PREFIX)) return { code: null, text: value }

  const separator = value.indexOf(':', ERROR_PROTOCOL_PREFIX.length)
  if (separator < 0) return { code: null, text: value }
  const candidate = value.slice(ERROR_PROTOCOL_PREFIX.length, separator)
  const text = value.slice(separator + 1)
  return FRIENDLY_ERROR_CODES.has(candidate as FriendlyErrorCode)
    ? { code: candidate as FriendlyErrorCode, text }
    : { code: null, text }
}

/** 去掉 `sayit_error:<code>:` 協議字首後的原始說明，給只需要「原樣告訴使用者哪裡錯了」的地方用。 */
export function errorDetailText(error: unknown): string {
  return decodeError(error).text
}

/**
 * 從各種異常文本里摳出 HTTP 狀態碼（`HTTP 401` / `status: 403` / `http=403` / `(404)`）。
 *
 * `http=403` 這一種是**我們自己**的格式：Rust 側 `diag::http_summary` 寫的就是
 * `[http=403 x-request-id=…]`。原來的正則把 `[:=]?` 只掛在 status 那個分支上，
 * 於是 `http=403` 認不出來 —— 我們自己產出的診斷串，自己的解析器讀不懂。
 * 平時不顯形是因為 Rust 會把穩定錯誤碼一起帶過來（走 stableCode 分支），
 * 只有拿不到錯誤碼、退回按文本分類時才會暴露成"認不出的錯誤"。
 */
function extractHttpStatus(text: string): number | null {
  const match = text.match(/\b(?:HTTP|status(?:\s*code)?)\s*[:=]?\s*(\d{3})\b/i)
    || text.match(/\((\d{3})\)/)
  if (!match) return null
  const code = Number(match[1])
  return code >= 100 && code <= 599 ? code : null
}

function isNetworkFailure(text: string): boolean {
  return /failed to fetch|networkerror|error sending request|econnrefused|enotfound|dns|connection refused|unreachable|websocket 连接失败/i // i18n-allow: 匹配底层中文错误串
    .test(text)
}

function isTimeout(text: string): boolean {
  return /timeout|timed out|超时/i.test(text) // i18n-allow: 匹配底层中文错误串
}

/**
 * 伺服器地址類錯誤（健康檢查、WebSocket 連線）。
 * `hasCustomUrl` 為 true 時才建議「恢復預設地址」——地址本來就是預設值時這個動作沒意義。
 */
export function describeServerError(error: unknown, hasCustomUrl: boolean): FriendlyError {
  const { code: stableCode, text } = decodeError(error)
  const status = extractHttpStatus(text)

  if (stableCode === 'server_timeout' || isTimeout(text)) {
    return {
      code: 'server_timeout',
      message: t('err.server.timeout'),
      detail: text,
      action: 'retry',
    }
  }
  if (stableCode === 'server_unreachable' || isNetworkFailure(text)) {
    return {
      code: 'server_unreachable',
      message: t('err.server.unreachable'),
      detail: text,
      action: hasCustomUrl ? 'reset_url' : 'retry',
    }
  }
  if (stableCode === 'server_forbidden' || status === 401 || status === 403) {
    return {
      code: 'server_forbidden',
      message: t('err.server.forbidden', { status: String(status) }),
      detail: text,
      action: 'retry',
    }
  }
  if (stableCode === 'server_not_sayit' || status === 404) {
    return {
      code: 'server_not_sayit',
      message: t('err.server.notSayIt'),
      detail: text,
      action: hasCustomUrl ? 'reset_url' : 'retry',
    }
  }
  if (stableCode === 'server_internal' || (status && status >= 500)) {
    return {
      code: 'server_internal',
      message: t('err.server.internal', { status: String(status) }),
      detail: text,
      action: 'retry',
    }
  }
  return {
    code: 'connect_failed',
    message: t('err.connectFailed'),
    detail: text,
    action: hasCustomUrl ? 'reset_url' : 'retry',
  }
}

// [自訂] SC-35：歷史重跑也使用同一則本機模型訊息。
export function describeLocalModelError(error: unknown): FriendlyError | null {
  const { code, text } = decodeError(error)
  switch (code) {
    case 'local_model_missing':
    case 'local_model_invalid':
    case 'local_model_read':
      return { code, message: t(`err.localModel.${code}`), detail: text, action: 'none' }
    default:
      return null
  }
}

/** 本機辨識與雲端供應商錯誤 */
export function describeProviderError(error: unknown): FriendlyError {
  const localModelError = describeLocalModelError(error)
  if (localModelError) return localModelError

  const { code: stableCode, text } = decodeError(error)
  const status = extractHttpStatus(text)

  if (stableCode === 'provider_timeout' || isTimeout(text)) {
    return { code: 'provider_timeout', message: t('err.provider.timeout'), detail: text, action: 'retry' }
  }
  if (stableCode === 'provider_unreachable' || isNetworkFailure(text)) {
    return {
      code: 'provider_unreachable',
      message: t('err.provider.unreachable'),
      detail: text,
      action: 'retry',
    }
  }
  // 401 才是明確的鑑權失敗。403 單獨一類（見下方分支）：403 的常見成因是
  // 「這個請求根本不該到這兒」——CDN 按地區拒絕、賬號沒開通該模型，響應裡
  // 一個字都不提金鑰。把它報成金鑰問題只會讓使用者反覆重建金鑰。
  if (stableCode === 'provider_bad_key' || status === 401 || /invalid.*(key|token)|unauthorized|认证失败|鉴权/i.test(text)) { // i18n-allow: 匹配底层中文错误串
    return {
      code: 'provider_bad_key',
      message: t('err.provider.badKey'),
      detail: text,
      action: 'check_key',
    }
  }
  // 402 必須排在限流之前，且不能和它合併：兩者給使用者的動作是相反的 ——
  // 限流等一會兒就好，餘額不足等下去永遠不會好，得去充值。
  // action 用 check_key 而不是 retry：它會把使用者帶到那份配置上，
  // 而充值入口就在同一個服務商後台（detail 裡帶著服務商給的具體金額和連結）。
  if (stableCode === 'provider_insufficient_balance'
    || status === 402
    || /payment required|insufficient balance|in balance|insufficient_quota|余额不足/i.test(text)) { // i18n-allow: 匹配底层中文错误串
    return {
      code: 'provider_insufficient_balance',
      message: t('err.provider.insufficientBalance'),
      detail: text,
      action: 'check_key',
    }
  }
  if (stableCode === 'provider_rate_limit' || status === 429 || /rate.?limit|quota|欠费|余额/i.test(text)) { // i18n-allow: 匹配底层中文错误串
    return {
      code: 'provider_rate_limit',
      message: t('err.provider.rateLimit'),
      detail: text,
      action: 'retry',
    }
  }
  // 排在限流之後：403 + quota 字樣更可能是配額問題。
  // action 用 switch_source 而不是 retry —— 地區不可用重試一萬次也是同一個結果，
  // 能解決問題的動作是換一家供應商。
  if (stableCode === 'provider_forbidden' || status === 403) {
    return {
      code: 'provider_forbidden',
      message: t('err.provider.forbidden'),
      detail: text,
      action: 'switch_source',
    }
  }
  if (stableCode === 'provider_no_model' || status === 404 || /model.*not.*(found|exist)/i.test(text)) {
    return {
      code: 'provider_no_model',
      message: t('err.provider.noModel'),
      detail: text,
      action: 'retry',
    }
  }
  return { code: 'connect_failed', message: t('err.connectFailed'), detail: text, action: 'retry' }
}

export function describeModelsDirError(error: unknown): { message: string; detail: string } {
  const { code, text } = decodeError(error)
  switch (code) {
    case 'model_dir_invalid':
    case 'model_dir_not_directory':
    case 'model_dir_permission':
    case 'model_dir_overlap':
    case 'model_dir_unwritable':
    case 'model_dir_save':
      return { message: t(`local.${code}`), detail: text }
    case 'download_busy':
      return { message: t('local.dirBusy'), detail: text }
    default:
      return { message: t('local.dirOpFailed'), detail: text }
  }
}

/** 模型下載類錯誤 */
export function describeDownloadError(error: unknown): FriendlyError {
  const { code: stableCode, text } = decodeError(error)

  if (stableCode === 'download_busy') {
    return {
      code: 'download_busy',
      message: t('err.download.generic'),
      detail: text,
      action: 'retry',
    }
  }
  if (stableCode === 'download_source_mismatch') {
    return {
      code: 'download_source_mismatch',
      message: t('err.download.checksum'),
      detail: text,
      action: 'switch_source',
    }
  }
  if (stableCode === 'download_network' || isTimeout(text) || isNetworkFailure(text)) {
    return {
      code: 'download_network',
      message: t('err.download.network'),
      detail: text,
      action: 'switch_source',
    }
  }
  if (stableCode === 'download_no_space' || /no space|磁盘|disk full|not enough space/i.test(text)) { // i18n-allow: 匹配底层中文错误串
    return {
      code: 'download_no_space',
      message: t('err.download.noSpace'),
      detail: text,
      action: 'none',
    }
  }
  if (stableCode === 'download_permission' || /permission|denied|access is denied|拒绝访问/i.test(text)) { // i18n-allow: 匹配底层中文错误串
    return {
      code: 'download_permission',
      message: t('err.download.permission'),
      detail: text,
      action: 'none',
    }
  }
  if (stableCode === 'download_checksum' || /checksum|sha256|校验/i.test(text)) { // i18n-allow: 匹配底层中文错误串
    return {
      code: 'download_checksum',
      message: t('err.download.checksum'),
      detail: text,
      action: 'switch_source',
    }
  }
  return {
    code: 'download_failed',
    message: t('err.download.generic'),
    detail: text,
    action: 'switch_source',
  }
}
