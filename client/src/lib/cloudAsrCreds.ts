// 雲 API 模式的憑據歸屬與豆包控制台代次判定。
//
// 為什麼單獨一個模組：這些規則原來散在 CloudAPISection 與 stores/modeStatus 兩處，
// 後者還留著一句「與 CloudAPISection.asrKeyGroup 保持一致」的註釋——靠註釋同步的東西
// 遲早會不一致。豆包新增控制台代次後要同步的規則又多一條，所以先收成一處。
import { t } from '@/i18n'

/** 豆包控制台代次。火山有兩代控制台，鑑權方式不同，金鑰也不是同一個東西。 */
export type DoubaoConsole = 'new' | 'legacy'

/** 儲存鍵。集中放在這裡，避免各處手寫字串寫錯。 */
export const DOUBAO_KEYS = {
  /** 使用者選的控制台代次 */
  console: 'cloudAsr.doubao.console',
  /** 新版控制台的 APP Key（发 X-Api-Key） */
  consoleKey: 'cloudAsr.doubao.consoleKey',
  /** 舊版控制台的 Access Token（發 X-Api-Access-Key）——歷史鍵，語義不變 */
  accessToken: 'cloudAsr.doubao.apiKey',
  /** 舊版控制台的 App ID（發 X-Api-App-Key）——歷史鍵，語義不變 */
  appId: 'cloudAsr.doubao.appId',
} as const

/**
 * 判定當前該用哪代控制台。
 *
 * 存過就用存的值；沒存過（老版本升級上來）看有沒有 App ID —— 舊版控制台必須填 App ID，
 * 所以「有 App ID」就等於「這是個舊版使用者」，行為保持不變，不需要他重新配置。
 * 全新使用者沒有任何歷史值，預設新版：只填一個 key，接入門檻最低。
 */
export function resolveDoubaoConsole(
  saved: string | null | undefined,
  legacyAppId: string,
): DoubaoConsole {
  if (saved === 'new' || saved === 'legacy') return saved
  return legacyAppId.trim() ? 'legacy' : 'new'
}

export interface DoubaoCredentials {
  console: DoubaoConsole
  /** 新版控制台的 APP Key */
  consoleKey: string
  /** 舊版控制台的 Access Token */
  accessToken: string
  /** 舊版控制台的 App ID */
  appId: string
}

/**
 * 算出「本次生效的憑據」，寫進執行時讀的那兩個扁平鍵（cloudAsr.apiKey / cloudAsr.appId）。
 *
 * **關鍵不變數：新版模式下 appId 必須是空串。** Rust 側就是用「app_id 空不空」來決定
 * 發 X-Api-Key 還是發 X-Api-App-Key + X-Api-Access-Key 的。這裡漏掉清空，線上就會用
 * 舊版格式發新版的金鑰，拿回一個看不出原因的鑑權失敗。
 *
 * 兩代的金鑰各自留在自己的儲存鍵裡，來回切不會丟，不用重新貼上。
 */
export function effectiveDoubaoCredentials(
  creds: DoubaoCredentials,
): { apiKey: string; appId: string } {
  if (creds.console === 'new') {
    return { apiKey: creds.consoleKey.trim(), appId: '' }
  }
  return { apiKey: creds.accessToken.trim(), appId: creds.appId.trim() }
}

/** 金鑰輸入框的顯示名——兩代叫法不同，標錯了使用者會去控制台找一個不存在的東西。 */
export function doubaoKeyLabel(console: DoubaoConsole): string {
  return console === 'new' ? 'API Key' : 'Access Token'
}

/**
 * 還缺什麼才能用（空串 = 齊了）。文案直接可展示，就緒指示和設定頁共用同一句話。
 */
export function describeDoubaoMissing(creds: DoubaoCredentials): string {
  if (creds.console === 'new') {
    return creds.consoleKey.trim() ? '' : t('asrProvider.missingKey')
  }
  if (!creds.accessToken.trim()) return t('asrProvider.missingAccessToken')
  if (!creds.appId.trim()) return t('asrProvider.missingAppId')
  return ''
}
