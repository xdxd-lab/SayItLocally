import { describe, expect, it } from 'vitest'
import {
  describeDownloadError,
  describeProviderError,
  describeServerError,
} from '../errorMessages'

/**
 * 這些斷言守住一條規則：設定頁裡不再出現原始異常文本作為主文案。
 * 每條錯誤都必須給出「發生了什麼 + 往哪查」，並把原文降級到 detail。
 */
describe('describeServerError', () => {
  it('把 Failed to fetch 翻譯成可行動的提示，並建議恢復預設地址', () => {
    const result = describeServerError(new TypeError('Failed to fetch'), true)
    expect(result.message).not.toContain('Failed to fetch')
    expect(result.message).toContain('連不上這個網址')
    expect(result.detail).toBe('Failed to fetch')
    expect(result.action).toBe('reset_url')
    expect(result.code).toBe('server_unreachable')
  })

  it('地址本來就是預設值時不提議恢復預設', () => {
    expect(describeServerError(new TypeError('Failed to fetch'), false).action).toBe('retry')
  })

  it('401/403 說清是許可權問題而不是網路問題', () => {
    const result = describeServerError(new Error('HTTP 403'), true)
    expect(result.message).toContain('拒絕')
    expect(result.message).toContain('403')
  })

  it('404 指出這裡要填服務根地址', () => {
    expect(describeServerError(new Error('HTTP 404'), true).message).toContain('根網址')
  })

  it('5xx 把責任指向服務端', () => {
    expect(describeServerError(new Error('HTTP 502'), true).message).toContain('伺服器端')
  })

  it('超時單獨成一類', () => {
    expect(describeServerError(new Error('超時'), true).message).toContain('逾時')
  })

  it('認不出來的錯誤也不把原文當主文案', () => {
    const result = describeServerError(new Error('weird internal thing'), false)
    expect(result.message).toBe('連線失敗。')
    expect(result.detail).toBe('weird internal thing')
  })
})

describe('describeProviderError', () => {
  it('優先使用 Rust 穩定錯誤碼，並從 detail 中剝掉協議字首', () => {
    const result = describeProviderError('sayit_error:provider_bad_key:HTTP 418 translated detail')
    expect(result.code).toBe('provider_bad_key')
    expect(result.action).toBe('check_key')
    expect(result.detail).toBe('HTTP 418 translated detail')
  })

  it('金鑰類失敗指向金鑰本身', () => {
    const result = describeProviderError('Invalid API key')
    expect(result.message).toContain('金鑰被拒絕')
    expect(result.action).toBe('check_key')
    expect(result.code).toBe('provider_bad_key')
  })

  it('限流/欠費與金鑰錯誤區分開', () => {
    expect(describeProviderError(new Error('HTTP 429 rate limit')).message).toContain('請求頻率')
  })

  /**
   * 實測（2026-09-16，OpenRouter）：餘額不足返回的是 402，響應體是
   * `{"error":{"message":"This request requires at least $0.50 in balance for audio","code":402}}`
   *
   * 以前 402 一條分類都沒命中，落到 connect_failed 顯示成「連線失敗」——
   * 而真實原因是賬戶裡沒錢，使用者完全看不出該去幹什麼。
   *
   * 它必須和限流分開：兩者給使用者的動作是相反的。限流等一會兒就好；
   * 餘額不足等到明年也還是不行，得去充值。
   */
  it('402 餘額不足單獨一類，不混進限流', () => {
    const raw = describeProviderError(new Error(
      'OpenRouter transcription error 402 Payment Required [http=402] gen=-: '
      + '{"error":{"message":"This request requires at least $0.50 in balance for audio","code":402}}',
    ))
    expect(raw.code).toBe('provider_insufficient_balance')
    expect(raw.message).toContain('餘額不足')
    expect(raw.message).not.toContain('請求頻率')
    // 動作要把使用者帶到那份配置上（充值入口和金鑰在同一個後台），而不是讓他重試
    expect(raw.action).toBe('check_key')
    // 具體金額和連結在服務商給的原文裡，必須保留在 detail 供使用者看
    expect(raw.detail).toContain('$0.50')

    const tagged = describeProviderError('sayit_error:provider_insufficient_balance:HTTP 402')
    expect(tagged.code).toBe('provider_insufficient_balance')
  })

  it('402 不會被誤判成金鑰問題，429 也不會被誤判成餘額不足', () => {
    expect(describeProviderError(new Error('HTTP 402 Payment Required')).code)
      .toBe('provider_insufficient_balance')
    expect(describeProviderError(new Error('HTTP 429 Too Many Requests: rate limit exceeded')).code)
      .toBe('provider_rate_limit')
  })

  // 實測：Groq 在中國大陸 IP 上返回 403 {"error":{"message":"Forbidden"}}，
  // 而假金鑰、真金鑰、完全不帶鑑權頭三種情況的響應**完全相同** —— 請求在邊緣節點
  // 就被拒了，金鑰從未被驗證。歸成「金鑰被拒絕」會把使用者引去反覆重建金鑰。
  it('403 不報成金鑰問題，而是指出可能是地區或許可權', () => {
    // 實際鏈路：Rust 已經分好類，前端只按錯誤碼取文案
    const tagged = describeProviderError('sayit_error:provider_forbidden:API error 403 Forbidden [http=403]')
    expect(tagged.code).toBe('provider_forbidden')
    expect(tagged.message).not.toContain('金鑰被拒絕')
    expect(tagged.action).toBe('switch_source')

    // 退路：拿不到錯誤碼時按文本分類，也不能落回「金鑰被拒絕」。
    // 這裡刻意用 `[http=403]` —— 那是 diag::http_summary 的格式，
    // 曾經因為正則認不出來而被歸成"認不出的錯誤"。
    const raw = describeProviderError(new Error('API error 403 Forbidden [http=403]: {"error":{"message":"Forbidden"}}'))
    expect(raw.code).toBe('provider_forbidden')
  })

  it('403 但服務端明確說金鑰無效時，仍然算金鑰問題', () => {
    expect(describeProviderError(new Error('HTTP 403: Invalid API key')).code).toBe('provider_bad_key')
  })

  it('模型未開通給出換供應商的方向', () => {
    expect(describeProviderError(new Error('model not found')).message).toContain('模型')
  })
})

describe('describeDownloadError', () => {
  it('錯誤分類不依賴 Rust detail 使用哪種語言', () => {
    const result = describeDownloadError('sayit_error:download_no_space:write failed')
    expect(result.code).toBe('download_no_space')
    expect(result.action).toBe('none')
    expect(result.detail).toBe('write failed')
  })

  it('網路中斷建議換下載源', () => {
    const result = describeDownloadError('error sending request for url (https://hf-mirror.com/...)')
    expect(result.message).toContain('換一個下載來源')
    expect(result.action).toBe('switch_source')
    expect(result.code).toBe('download_network')
    expect(result.detail).toContain('hf-mirror.com')
  })

  it('磁碟空間不足單獨成一類，不建議換源', () => {
    const result = describeDownloadError('No space left on device')
    expect(result.message).toContain('磁碟空間不足')
    expect(result.action).toBe('none')
  })

  it('校驗失敗建議換源重下', () => {
    expect(describeDownloadError('sha256 mismatch').action).toBe('switch_source')
  })

  it('重複下載只建議稍後重試，不誤導使用者切換下載源', () => {
    const result = describeDownloadError('sayit_error:download_busy:already downloading')
    expect(result.code).toBe('download_busy')
    expect(result.action).toBe('retry')
  })

  it('遠端大小與目錄不一致時建議切換下載源', () => {
    const result = describeDownloadError('sayit_error:download_source_mismatch:size changed')
    expect(result.code).toBe('download_source_mismatch')
    expect(result.action).toBe('switch_source')
  })
})

// [自訂] F14 PR-C：底層錯誤可能是簡體原文，也可能是正體；兩種都要認得。
// 簡體以 Unicode 跳脫保存（原始碼不含簡體字），註解寫的是對應的正體。
describe('簡體與正體的底層錯誤字串都能分類', () => {
  it.each([
    ['\u8ba4\u8bc1失\u8d25', 'provider_bad_key'], // 認證失敗
    ['認證失敗', 'provider_bad_key'],
    ['\u9274\u6743', 'provider_bad_key'], // 鑑權
    ['鑑權', 'provider_bad_key'],
    ['\u4f59\u989d\u4e0d\u8db3', 'provider_insufficient_balance'], // 餘額不足
    ['餘額不足', 'provider_insufficient_balance'],
    ['欠\u8d39', 'provider_rate_limit'], // 欠費
    ['欠費', 'provider_rate_limit'],
  ])('供應商錯誤 %s → %s', (text, code) => {
    expect(describeProviderError(text).code).toBe(code)
  })

  it.each([
    ['磁\u76d8已\u6ee1', 'download_no_space'], // 磁碟已滿
    ['磁碟已滿', 'download_no_space'],
    ['拒\u7edd\u8bbf\u95ee', 'download_permission'], // 拒絕存取
    ['拒絕存取', 'download_permission'],
    ['校\u9a8c失\u8d25', 'download_checksum'], // 校驗失敗
    ['校驗失敗', 'download_checksum'],
  ])('下載錯誤 %s → %s', (text, code) => {
    expect(describeDownloadError(text).code).toBe(code)
  })

  it.each(['\u8d85\u65f6', '超時'])('逾時 %s', (text) => {
    expect(describeServerError(new Error(text), true).message).toContain('逾時')
  })
})
