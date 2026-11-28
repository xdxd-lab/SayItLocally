/**
 * ASR 供應商 key → 實際模型 ID 對映
 */

const QWEN_OMNI_MODEL_MAP: Record<string, string> = {
  qwen_omni_plus: 'qwen3.5-omni-plus-realtime',
  qwen_omni_35_plus: 'qwen3.5-omni-plus-realtime',
  qwen_omni_35_flash: 'qwen3.5-omni-flash-realtime',
  // 這兩個舊執行時鍵原來指向 qwen3-omni-flash-realtime / qwen-omni-turbo-realtime。
  // 2026-09-23 實測那兩個端點已被百鍊縮容（讀超時 / 「system capacity limits」），
  // 所以改指向同族還活著的 flash。**不能只是刪掉這兩行** —— 刪了以後
  // resolveQwenOmniModel 返回 undefined、extra.model 不帶值，後端會用它自己的
  // 預設模型，等於把落點交給另一處程式碼去定，兩邊一漂就查不出來。
  // 落点要和 asrProviderCatalog 的 RETIRED_MODELS / LEGACY_PROVIDERS 一致。
  qwen_omni_flash: 'qwen3.5-omni-flash-realtime',
  qwen_omni_turbo: 'qwen3.5-omni-flash-realtime',
}

/** 判斷是否為 Qwen Omni 系列模型 */
export function isQwenOmniProvider(provider: string): boolean {
  return provider.startsWith('qwen_omni')
}

/** 根據供應商 key 解析 Qwen Omni 模型 ID，非 Omni 返回 undefined */
export function resolveQwenOmniModel(provider: string): string | undefined {
  return QWEN_OMNI_MODEL_MAP[provider]
}

/** ASR 供應商 key → 顯示用的**預設**模型 ID（使用者可能選了同平台的另一個） */
const ASR_DISPLAY_MODEL_MAP: Record<string, string> = {
  doubao_v2: 'Doubao-Seed-ASR-2.0',
  qwen: 'qwen3-asr-flash',
  qwen_realtime: 'qwen3-asr-flash-realtime',
  // 3.0 與 3.1 共用 `qwen_audio_stream` 這個執行時鍵，所以這裡只能給一個**預設**值。
  // 跟著目錄的預設項走（3.1）；使用者選了 3.0 時由 selectedModel 覆蓋，
  // 那條路才是準的 —— 這也正是 resolveAsrDisplayModel 要收 selectedModel 的原因。
  qwen_audio_stream: 'qwen-audio-3.1-asr-flash-streaming',
  mimo: 'mimo-v2.5-asr',
  groq_whisper: 'whisper-large-v3-turbo',
  openai_transcribe: 'gpt-transcribe',
  openai_live_transcribe: 'gpt-live-transcribe',
  gemini_transcribe: 'gemini-3.5-transcribe',
  gemini_live_transcribe: 'gemini-3.5-transcribe-live',
  openrouter_transcribe: 'openai/gpt-transcribe',
  ...QWEN_OMNI_MODEL_MAP,
}

/**
 * 將內部供應商 key 對映為顯示用的模型名稱。
 *
 * `selectedModel` 傳的是執行時鍵 `cloudAsr.model`（使用者在服務配置裡選的那個）。
 * 只有 Groq / OpenAI 這類同協議多模型的服務會有值，其餘一律走上面的預設表 ——
 * 不接這個引數的話，診斷和測試結果裡顯示的模型會和真正發出去的那個不是一個。
 */
export function resolveAsrDisplayModel(providerKey: string, selectedModel?: string): string {
  const picked = selectedModel?.trim()
  if (picked) return picked
  return ASR_DISPLAY_MODEL_MAP[providerKey] || providerKey || 'unknown'
}

/**
 * asr_config.extra 裡我們會用到的欄位。
 *
 * 繼承 Record 是為了能直接賦給 `AsrProviderConfig.extra`（那邊宣告的是任意鍵值，
 * 因為每家供應商往裡放的東西不同）。這裡把用得上的兩個欄位顯式列出來，
 * 好讓拼錯欄位名當場報錯，而不是靜默變成一個後端讀不到的鍵。
 */
export interface AsrConfigExtra extends Record<string, unknown> {
  model?: string
  instructions?: string
  /** 自定義端點地址。空 = 用實現裡內建的官方地址 */
  baseUrl?: string
  /**
   * 「OpenAI 相容」那張卡說哪種協議：`transcriptions` / `chat` / `chat_standard`。
   * 不帶這個欄位 = 讓 Rust 側自己探測（預設的 `auto` 就不往下傳）。
   */
  protocol?: string
}

/**
 * 組 `asr_config.extra` —— 「這次用哪個模型」的唯一齣處。
 *
 * 四個地方要組 asr_config（錄音主鏈路、歷史重跑、設定頁識別測試、診斷頁測試），
 * 以前每處都自己拼一遍 extra，於是加一個「可選模型」就得改四處、還容易漏掉一處 ——
 * 漏掉的那處會靜默用回預設模型，而使用者以為自己換了。
 *
 * ⚠️ 這裡**不再**根據 provider 去反查 Omni 的模型名。改成「一張卡 = 一個平台」之後，
 * 模型由 `resolveAsrApiModel` 解析好再傳進來，而執行時 provider 變成了 `qwen_omni`
 * 這個平台級的值 —— 老程式碼拿它去查 QWEN_OMNI_MODEL_MAP 查不到，於是
 * `extra.model` 是 undefined，後端回落到自己的預設模型。表現是**下拉里選哪個
 * Omni 都跑同一個**（都變成 qwen3-omni-flash-realtime），而介面顯示得一切正常。
 * 老表只作為存量執行時鍵的兜底保留（那時 provider 還是 `qwen_omni_35_plus` 這類）。
 */
export function buildAsrExtra(
  provider: string,
  options: {
    model?: string
    instructions?: string
    baseUrl?: string
    protocol?: string
  } = {},
): AsrConfigExtra | undefined {
  const model = options.model?.trim() || resolveQwenOmniModel(provider) || ''
  const instructions = options.instructions?.trim() ?? ''
  const baseUrl = options.baseUrl?.trim() ?? ''
  // `auto` 不往下傳：Rust 側「沒有這個欄位」和「auto」是同一個意思，
  // 少傳一個欄位就少一處兩邊要對齊的約定。
  const protocol = options.protocol?.trim() ?? ''
  const explicitProtocol = protocol === 'auto' ? '' : protocol
  if (!model && !instructions && !baseUrl && !explicitProtocol) return undefined
  return {
    ...(model ? { model } : {}),
    ...(instructions ? { instructions } : {}),
    ...(baseUrl ? { baseUrl } : {}),
    ...(explicitProtocol ? { protocol: explicitProtocol } : {}),
  }
}

/**
 * 有流式實現的供應商（不含執行時前置條件）。
 *
 * 注意 qwen3-asr-flash（`qwen`）与 `openai_transcribe` / `gemini_transcribe` 都是
 * 一次性 HTTP 模型，不在此列 —— 它們對應的流式版本是各自單獨的一張卡。
 */
const STREAMING_CAPABLE = new Set([
  'doubao_v2',
  'qwen_realtime',
  'qwen_audio_stream',
  'openai_live_transcribe',
  'gemini_live_transcribe',
])

export function isStreamingDisplayCapable(provider: string): boolean {
  return STREAMING_CAPABLE.has(provider)
}

/**
 * 判斷當前配置下「流式即時顯示」是否真正就緒可用（含執行時前置條件）。
 *
 * **這是執行時唯一的閘門**（CloudAPIProvider 拿它決定走不走 WebSocket），
 * 光在服務目錄裡給一條加 `streaming: true` 是沒有任何效果的。
 *
 * - doubao_v2：直接可用（需在火山開通「流式語音識別 2.0」，執行時由服務端校驗）。
 * - qwen_audio_stream（qwen-audio-3.1 / 3.0-asr-flash-streaming）：直接可用。這兩代
 *   **都不需要 WorkspaceId** —— 通用域名 dashscope.aliyuncs.com 實測可用，
 *   別照抄下面那條的前置條件（見 providers/asr_qwen_audio_stream.rs 檔案頭的實測記錄）。
 * - qwen_realtime（qwen3-asr-flash-realtime）：走地域專屬即時端點，必須提供北京業務空間 WorkspaceId 才可用。
 * - openai_live_transcribe / gemini_live_transcribe：填了金鑰就能用，沒有額外前置條件。
 * - qwen（qwen3-asr-flash）：非即時模型，不支援即時字幕。
 */
export function isStreamingDisplayReady(provider: string, qwenWorkspaceId?: string): boolean {
  if (provider === 'qwen_realtime') {
    return Boolean(qwenWorkspaceId && qwenWorkspaceId.trim())
  }
  return isStreamingDisplayCapable(provider)
}

// ─────────────────────────── 熱詞能力 ───────────────────────────
//
// 宣告在 Rust（providers/capabilities.rs），這裡只做「五檔 → 介面要說的話」的摺疊。
// **前端刻意不再自己維護一份「哪家支援熱詞」的清單** —— 那正是 issue #67 的成因：
// 實現在 Rust、宣告在前端一張手寫表格，兩者無人對賬，結果 6 家雲服務被漏掉。

/** Rust `HotwordDelivery` 的序列化形態。新增檔位時 TS 這邊會因為窮盡檢查報錯。 */
export type HotwordDelivery =
  | 'vocabulary'
  | 'context'
  | 'instruction'
  | 'protocol_has_no_slot'
  | 'not_wired_up'
  | 'undecided_protocol'
  | 'unknown_provider'

/** Rust `AsrHotwordCapability` 的序列化形態。 */
export interface AsrHotwordCapability {
  /** 走流式路徑時的行為 */
  streaming: HotwordDelivery
  /** 走一次性路徑時的行為，也是流式回落後的實際行為 */
  buffered: HotwordDelivery
  /** 這家有沒有流式實現；為假時上面兩個欄位相同 */
  hasStreamingPath: boolean
  /** **SayIt 自設**的最多傳送條數（走流式路徑時），不是服務端限制 */
  streamingClientCap: number | null
  /** 同上，走一次性路徑（含流式回落）時的那份 */
  bufferedClientCap: number | null
}

/**
 * 介面上要對使用者說的那件事。
 *
 * 五檔折成三種是因為使用者在熱詞頁只想知道一件事：我配的熱詞現在有用嗎。
 * `vocabulary` / `context` / `instruction` 對他的**行動**沒有區別（都是"配了有幫助、
 * 不保證命中"），只有不進請求那幾檔需要他去做別的事。五檔的差別留給日誌和文件。
 *
 * 三種而不是兩種：`undecided` 那檔不能歸進任何一邊 —— 「OpenAI 相容」的 auto 協議
 * 探測之前，說"生效"和說"不生效"都是在編。
 */
export type HotwordUiState = 'sent' | 'not_sent' | 'undecided'

export function foldHotwordDelivery(delivery: HotwordDelivery): HotwordUiState {
  switch (delivery) {
    case 'vocabulary':
    case 'context':
    case 'instruction':
      return 'sent'
    case 'undecided_protocol':
      return 'undecided'
    case 'protocol_has_no_slot':
    case 'not_wired_up':
      return 'not_sent'
    // 宣告缺失。**不能當成 'not_sent'** —— 那會把"我們漏了宣告"顯示成
    // "這家不支援熱詞"，用錯誤的確定性蓋住一個 bug。歸入 undecided：
    // 介面會說"未確定"，而 Rust 側的測試會在漏宣告時直接紅。
    case 'unknown_provider':
      return 'undecided'
  }
}

/**
 * 「未確定」的三種成因。展示狀態共用，**給使用者的說明必須分開**。
 *
 * 折成同一個 `undecided` 是對的（三種都不能說"生效"或"不生效"），但原來三種都套用
 * 同一句文案「等第一次識別或測試連線之後才能確定」—— 那句話只對 `protocol` 成立。
 * 另兩種再錄一百次也不會變：
 *   · `declaration_missing`：Rust 返回 `unknown_provider`，是 SayIt 漏了声明（bug），
 *     該讓使用者知道這不是他的配置問題、可以反饋；
 *   · `query_failed`：命令本身沒調通（舊版本二進位制沒有這個命令、IPC 異常），
 *     該提示重啟或更新。
 * 告訴使用者"再錄一次就知道了"而實際永遠不會變，是在把一個可指認的故障說成正常等待。
 */
export type HotwordUndecidedReason = 'protocol' | 'declaration_missing' | 'query_failed'

/** 這個檔位落進「未確定」的原因；不是未確定則返回 null。 */
export function hotwordUndecidedReason(delivery: HotwordDelivery): HotwordUndecidedReason | null {
  switch (delivery) {
    case 'undecided_protocol':
      return 'protocol'
    case 'unknown_provider':
      return 'declaration_missing'
    default:
      return null
  }
}

/**
 * 本次錄音**實際**會走哪條路徑 —— 熱詞能力只能按它判斷，不能按 provider 一概而論。
 *
 * 現場證據是這兩家的行為在兩條路徑上相反：
 *   · `openai_live_transcribe` 流式有 keywords、回落到檔案端點後一條都不傳
 *   · `gemini_live_transcribe` 流式一條都不傳、回落到檔案轉寫反而會拼進 prompt
 *
 * 所以設定頁展示的是**配置預期**（按當前開關算出來的那條路），而真正走了哪條要看
 * 日誌 —— 沒開字幕、配置沒就緒、或者建連失敗都會回落（見 CloudAPIProvider 的
 * tryStartRealtimeStream）。
 */
export interface HotwordPathOptions {
  streamingDisplayEnabled: boolean
  provider: string
  qwenWorkspaceId?: string
}

/**
 * 這次會走流式還是一次性路徑。
 *
 * **只有這一處判斷。** delivery 和條數上限都得按同一條路徑取值，各判一次就會漂
 * —— 上限曾經按 provider 取一個數，於是 OpenAI live 關掉字幕時同時顯示
 * 「不发送」和「最多发送 100 个」。
 */
export function willUseStreamingPath(
  capability: AsrHotwordCapability,
  opts: HotwordPathOptions,
): boolean {
  if (!capability.hasStreamingPath) return false
  return opts.streamingDisplayEnabled
    && isStreamingDisplayReady(opts.provider, opts.qwenWorkspaceId)
}

export function expectedHotwordDelivery(
  capability: AsrHotwordCapability,
  opts: HotwordPathOptions,
): HotwordDelivery {
  return willUseStreamingPath(capability, opts) ? capability.streaming : capability.buffered
}

/**
 * 這條路徑上的傳送條數上限，沒有則為 null。
 *
 * 必須和 `expectedHotwordDelivery` 走同一個路徑判斷 —— 兩家 live 供應商的上限只存在
 * 於其中一條路上（OpenAI 在流式、Gemini 在回落），取錯一條介面就會自相矛盾。
 */
export function expectedClientCap(
  capability: AsrHotwordCapability,
  opts: HotwordPathOptions,
): number | null {
  return willUseStreamingPath(capability, opts)
    ? capability.streamingClientCap
    : capability.bufferedClientCap
}

/**
 * 回落會不會改變熱詞行為。為真時介面要說明"這取決於即時字幕能否連上"，
 * 否則使用者看到的結論會在建連失敗那次悄悄失效。
 */
export function hotwordDependsOnStreamingPath(capability: AsrHotwordCapability): boolean {
  return capability.hasStreamingPath
    && foldHotwordDelivery(capability.streaming) !== foldHotwordDelivery(capability.buffered)
}
