import { t } from '@/i18n'
import * as bridge from '@/services/bridge'
import { getBackendBaseUrl } from '@/services/runtimeConfig'
import { getSetting, setSetting, type HistoryRecord } from '@/services/store'
import { normalizeForDiff } from '@/lib/asrDiff'

/**
 * ASR 糾錯反饋：使用者主動把「這條識別錯了，正確的是這樣」連同當次錄音提交給服務端。
 *
 * 與「服務端靜默錄音」不是一回事：這裡的每一次上傳都由使用者在歷史記錄裡點出來，
 * 首次提交前還要過一次明確的同意確認。詳見 .kiro/decisions.md。
 *
 * 走 webview 的 fetch + FormData，不走 Rust：
 * 瀏覽器會自帶 User-Agent，天然避開"reqwest 不帶 UA 被 WAF 403"那個坑（pitfalls #1）。
 */

/** 同意文案的版本。文案實質變化時 +1，老使用者會被要求重新確認。 */
export const ASR_CORRECTION_CONSENT_VERSION = '1'
const CONSENT_SETTING_KEY = 'asrCorrectionConsentVersion'

/**
 * 承諾給使用者的撤回視窗（天），只用於文案。改這裡要同時改後端的 WITHDRAW_DAYS。
 *
 * 後端**沒有**按這個天數攔撤回：承諾是"30 天內可以撤回"，那麼放寬到永遠可撤回
 * 永遠不會違背承諾，而加一道硬期限只會多出一個"點了沒反應"的失敗分支。
 */
export const ASR_CORRECTION_WITHDRAW_DAYS = 30

/** 與後端 MAX_AUDIO_BYTES 一致：五分鐘錄音約 9.6MB，10 MiB 裝得下。 */
export const MAX_CORRECTION_AUDIO_BYTES = 10 * 1024 * 1024

export type AsrCorrectionErrorCode =
  | 'audio_missing'
  | 'audio_too_large'
  | 'no_change'
  | 'rate_limited'
  | 'already_submitted'
  | 'storage_full'
  | 'network'
  /** 這台後端沒有這個介面（自建伺服器版本較舊，或官方後端還沒更新） */
  | 'not_supported'
  | 'server'

export interface AsrCorrectionResult {
  ok: boolean
  correctionId?: string
  code?: AsrCorrectionErrorCode
  message: string
}

export async function hasAsrCorrectionConsent(): Promise<boolean> {
  const stored = await getSetting<string>(CONSENT_SETTING_KEY, '')
  return stored === ASR_CORRECTION_CONSENT_VERSION
}

export async function grantAsrCorrectionConsent(): Promise<void> {
  await setSetting(CONSENT_SETTING_KEY, ASR_CORRECTION_CONSENT_VERSION)
}

/**
 * 由歷史記錄 id 推出提交編號，而不是每次隨機生成。
 *
 * 這樣重試（上一次請求超時但服務端其實收下了）用的是同一個編號，服務端按編號冪等，
 * 不會出現兩條一樣的樣本；使用者要撤回時也能憑本地記錄算出同一個編號。
 * 形狀必須匹配後端的 ^[A-Za-z0-9_-]{8,64}$。
 */
export function buildCorrectionId(recordId: string): string {
  const safe = (recordId || '').replace(/[^A-Za-z0-9_-]/g, '')
  return `c-${safe}`.padEnd(8, '0').slice(0, 64)
}

// 不寫返回型別註解：顯式寫 `Uint8Array` 會退化成 Uint8Array<ArrayBufferLike>，
// 而 BlobPart 只接受 Uint8Array<ArrayBuffer>，會在 new Blob 那行報型別錯。
// 讓它從 new Uint8Array(length) 推斷出來正好是對的。
function messageForCode(code: AsrCorrectionErrorCode, status?: number): string {
  switch (code) {
    case 'audio_missing':
      return t('asrCorrection.errorAudioMissing')
    case 'audio_too_large':
      return t('asrCorrection.errorAudioTooLarge')
    case 'no_change':
      return t('asrCorrection.errorNoChange')
    case 'rate_limited':
      return t('asrCorrection.errorRateLimited')
    case 'already_submitted':
      return t('asrCorrection.errorAlreadySubmitted')
    case 'storage_full':
      return t('asrCorrection.errorStorageFull')
    case 'network':
      return t('asrCorrection.errorNetwork')
    case 'not_supported':
      return t('asrCorrection.errorNotSupported')
    default:
      return t('asrCorrection.errorServer', { status: status ?? 0 })
  }
}

/** 後端 error code → 我們的分類。沒列到的都歸 server（介面上顯示狀態碼，便於排查）。 */
function classifyServerError(error: string): AsrCorrectionErrorCode {
  if (error === 'rate_limited') return 'rate_limited'
  if (error === 'already_submitted') return 'already_submitted'
  if (error === 'storage_full') return 'storage_full'
  if (error === 'audio_too_large') return 'audio_too_large'
  if (error === 'no_change') return 'no_change'
  return 'server'
}

const ASR_CORRECTION_DISABLED = true as boolean // [自訂]

export async function submitAsrCorrection(
  record: HistoryRecord,
  correctedText: string,
): Promise<AsrCorrectionResult> {
  if (ASR_CORRECTION_DISABLED) throw new Error('ASR correction upload is disabled in this build') // [自訂] 不上傳錄音與文字
  const original = normalizeForDiff(record.asrText || '')
  const corrected = normalizeForDiff(correctedText)
  if (!original || !corrected) {
    return { ok: false, code: 'no_change', message: messageForCode('no_change') }
  }
  if (original === corrected) {
    return { ok: false, code: 'no_change', message: messageForCode('no_change') }
  }

  if (!record.audioFilePath) {
    return { ok: false, code: 'audio_missing', message: messageForCode('audio_missing') }
  }
  // 保留期到了之後 Rust 只刪檔案不清路徑，所以這裡必須按"讀不到就是沒有"處理
  const bytes = await bridge.readAudioFileBytes(record.audioFilePath).catch(() => null)
  if (!bytes) {
    return { ok: false, code: 'audio_missing', message: messageForCode('audio_missing') }
  }

  if (bytes.byteLength > MAX_CORRECTION_AUDIO_BYTES) {
    // 本地先攔一次，別讓使用者傳完 10MB 才看到失敗
    return { ok: false, code: 'audio_too_large', message: messageForCode('audio_too_large') }
  }

  const clientInfo = await bridge.getClientRuntimeInfo().catch(() => null)
  const correctionId = buildCorrectionId(record.id)

  const form = new FormData()
  form.append('audio', new Blob([bytes], { type: 'audio/wav' }), 'recording.wav')
  form.append('correction_id', correctionId)
  form.append('machine_id', clientInfo?.deviceId || 'unknown')
  form.append('original_asr_text', original)
  form.append('corrected_text', corrected)
  form.append('work_mode', record.workMode || 'server')
  form.append('app_version', clientInfo?.clientVersion || '')
  form.append('client_record_id', record.id)
  form.append('asr_provider', record.asrProvider || '')
  // 熱詞是 ASR 錯誤最常見的成因之一，沒有它樣本無法判斷成因（見 pitfalls 的熱詞幻覺）
  form.append('hotwords', JSON.stringify(record.autoAppliedHotwords || []))
  form.append('consent_version', ASR_CORRECTION_CONSENT_VERSION)

  let response: Response
  try {
    response = await fetch(`${getBackendBaseUrl()}/api/asr-corrections`, { method: 'POST', body: form })
  } catch {
    return { ok: false, code: 'network', message: messageForCode('network') }
  }

  let body: Record<string, unknown> = {}
  try {
    body = (await response.json()) as Record<string, unknown>
  } catch {
    body = {}
  }

  if (response.ok) {
    return {
      ok: true,
      correctionId: String(body.correction_id || correctionId),
      message: t('asrCorrection.submitted'),
    }
  }

  // 「這條音訊已經提交過」也算成功：本地標記成已提交，使用者不會反覆看到入口
  const errorCode = String(body.error || '')
  if (response.status === 409 && errorCode === 'already_submitted') {
    return {
      ok: true,
      correctionId: String(body.correction_id || correctionId),
      message: t('asrCorrection.alreadySubmitted'),
    }
  }

  // 後端沒有這個介面時不要報成"提交失敗(405)"。兩種真實情況都會走到這裡：
  // 使用者把後端指到自建伺服器而那邊版本較舊；官方後端還沒更新到帶這個介面的版本。
  // （SayIt 後端把靜態站點掛在 "/" 上，所以路徑不存在時拿到的是 404，方法不匹配是 405，
  //   不是 FastAPI 的標準 405 —— 見 pitfalls #23。）
  if (response.status === 404 || response.status === 405) {
    return { ok: false, code: 'not_supported', message: messageForCode('not_supported') }
  }

  const code = classifyServerError(errorCode)
  return { ok: false, code, message: messageForCode(code, response.status) }
}

/** 撤回：服務端會刪掉音訊並清掉文本。失敗返回 false，呼叫方保持"已提交"狀態不變。 */
export async function withdrawAsrCorrection(correctionId: string): Promise<boolean> {
  if (ASR_CORRECTION_DISABLED) throw new Error('ASR correction withdraw is disabled in this build') // [自訂] S16：不送出裝置 ID
  const clientInfo = await bridge.getClientRuntimeInfo().catch(() => null)
  try {
    const response = await fetch(
      `${getBackendBaseUrl()}/api/asr-corrections/${encodeURIComponent(correctionId)}/withdraw`,
      {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ machine_id: clientInfo?.deviceId || 'unknown' }),
      },
    )
    // 404 = 服務端已經沒有這條了（保留期到了或已清理），對使用者而言目的已達成
    return response.ok || response.status === 404
  } catch {
    return false
  }
}
