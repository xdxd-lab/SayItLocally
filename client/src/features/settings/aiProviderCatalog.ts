// 「AI 供應商」頁的資料模型與純函式。
//
// 單獨成檔案的原因：這裡每一條都曾經或可能悄悄漂移（預設值與示例不一致、金鑰長度斷言
// 過期、老資料遷移），而它們全都是純邏輯，值得被測試鎖住。元件只負責渲染與副作用。

/**
 * 供應商清單 —— 這是唯一的真相來源。
 *
 * 預設地址與候選模型原來另有三張表（DEFAULT_URLS / DEFAULT_MODELS / DEFAULT_MODEL_LISTS）
 * 宣告在元件函式體裡，於是同一個供應商在示例文字和預設值之間漂移過：
 * 千問的示例寫 `qwen-plus` 而實際種進去的是 `qwen3.6-flash`，
 * MiMo 的示例寫 `mimo-v2.5-pro` 而預設選中的是 `mimo-v2.5`。
 * 合併成一張表後，示例直接由 defaultModels[0] 派生，漂移不可能再發生。
 */
import { getLocale, t } from '@/i18n'

export interface AiProvider {
  value: string
  /**
   * 展示名。品牌名用官方英文寫法（Qwen / Doubao (Volcengine Ark) / Xiaomi MiMo），
   * 不是直譯 —— 中文語境習慣叫「通義千問」，所以它仍然要跟介面語言走。
   *
   * ⚠️ 表裡幾條用的是 **getter**，不是普通字串欄位。原因：這張表是模組級常量，
   * 普通欄位會在模組載入那一刻把當時的語言固化下來，切語言後不再變。改成 getter
   * 就能保留 `.label` 這個讀法 —— 二十多個呼叫點和三個測試檔案都不用動。
   */
  label: string
  defaultUrl: string
  /** A blank URL uses the provider's default when saving. */
  urlOptional?: boolean
  /** 建議的模型，第一個作為新建時的預設值；同時作為表單裡的快速選擇 */
  defaultModels: string[]
  /** 不需要 API Key（目前只有本机 Ollama） */
  keyless?: boolean
  /** 申請金鑰的控制台地址 */
  consoleUrl?: string
}

// [自訂] S34：本 fork 只允許本機 Ollama（Rust 端 registry.rs 強制）。
// Ollama 移到第一個，findProvider／parseProfiles 的備用值因此是 Ollama；DeepSeek 註解停用。
export const AI_PROVIDERS: AiProvider[] = [
  {
    value: 'ollama',
    get label() { return t('aiProvider.ollama') },
    defaultUrl: 'http://127.0.0.1:11434',
    defaultModels: ['qwen2.5:7b'],
    keyless: true,
  },
  // [自訂] S34 嚴禁 DeepSeek，已註解停用：
  // {
  //   value: 'deepseek',
  //   label: 'DeepSeek',
  //   defaultUrl: 'https://api.deepseek.com',
  //   defaultModels: ['deepseek-v4-flash'],
  //   consoleUrl: 'https://platform.deepseek.com/api_keys',
  // },
  {
    value: 'qwen',
    get label() { return t('aiProvider.qwen') },
    defaultUrl: 'https://dashscope.aliyuncs.com/compatible-mode',
    defaultModels: ['qwen3.6-flash', 'qwen-plus'],
    consoleUrl: 'https://bailian.console.aliyun.com',
  },
  {
    value: 'doubao',
    get label() { return t('aiProvider.doubao') },
    defaultUrl: 'https://ark.cn-beijing.volces.com/api/v3',
    defaultModels: ['doubao-seed-2-0-lite-260215'],
    consoleUrl: 'https://console.volcengine.com/ark',
  },
  {
    value: 'mimo',
    get label() { return t('aiProvider.mimo') },
    defaultUrl: 'https://api.xiaomimimo.com/v1',
    defaultModels: ['mimo-v2.5', 'mimo-v2.5-pro'],
  },
  {
    value: 'zhipu',
    get label() { return t('aiProvider.zhipu') },
    /**
     * 版本段在 base 裡是 `/v4`，不是 `/v1`（normalize_base_url 認得這個形狀，會原樣保留）。
     *
     * 之前沒有這張卡，使用者只能選「OpenAI 相容」自己填地址，而這個地址**幾乎必然填錯**：
     * 智譜文件裡出現的是完整端點 `.../v4/chat/completions`，照抄過來會被再拼一次；
     * 只填到 `.../api/paas` 又会被补成 `/api/paas/v1`。两种都是 404，用户看到的就是
     * 「智譜接不上」。做成內建卡之後沒有可填錯的餘地。
     */
    defaultUrl: 'https://open.bigmodel.cn/api/paas/v4',
    // glm-4.7-flash 在官方「免費模型」目錄下（docs.bigmodel.cn/cn/guide/models/free/glm-4.7-flash），
    // 所以放第一位當預設值 —— 想白用的人開箱即可。
    defaultModels: ['glm-4.7-flash', 'glm-4.7'],
    consoleUrl: 'https://open.bigmodel.cn/usercenter/proj-mgmt/apikeys',
  },
  {
    value: 'groq',
    // 品牌名，中英文都寫 Groq，不需要 locale 鍵（同 DeepSeek 的處理）
    label: 'Groq',
    // 末段已经是 /v1，normalize_base_url 会原样保留、不再追加版本段
    defaultUrl: 'https://api.groq.com/openai/v1',
    defaultModels: ['openai/gpt-oss-20b', 'openai/gpt-oss-120b', 'llama-3.3-70b-versatile'],
    consoleUrl: 'https://console.groq.com/keys',
  },
  {
    value: 'openrouter',
    label: 'OpenRouter',
    defaultUrl: 'https://openrouter.ai/api/v1',
    urlOptional: true,
    // [自訂] S34：移除預設模型 deepseek/deepseek-v4.1-flash（可經 OpenRouter 繞過網址封鎖）
    defaultModels: [],
    consoleUrl: 'https://openrouter.ai/settings/keys',
  },
  {
    value: 'openai_compat',
    get label() { return t('aiProvider.openaiCompat') },
    defaultUrl: 'https://api.openai.com',
    defaultModels: ['gpt-4o-mini'],
  },
  // [自訂] S34：ollama 已移到清單第一個
]

/**
 * 新建服務時的地區預設；隻影響還沒選擇過供應商的場景。
 * 英文預設 OpenRouter，與英文介面推薦的 DeepSeek V4.1 Flash（經 OpenRouter）一致；
 * 中文不跟：國內付款和訪問 OpenRouter 都不方便，直連 DeepSeek 更合適。
 */
export function preferredAiProviderValue(): string {
  // [自訂] S34：一律本機 Ollama（原：getLocale() === 'en' ? 'openrouter' : 'deepseek'）
  return 'ollama'
}

/** 把地區預設那家排到最前，原陣列順序保持不變供遷移邏輯使用。 */
// [自訂] #18：本 fork 只允許本機 Ollama（Rust 端 registry.rs 也會拒絕其他供應商），
// AI 服務頁只列出 Ollama。AI_PROVIDERS 清單本身保留（舊設定讀取、測試仍用到）。改回 true 即恢復上游行為。
const SHOW_CLOUD_AI_PROVIDERS = false

export function aiProvidersForDisplay(): AiProvider[] {
  if (!SHOW_CLOUD_AI_PROVIDERS) return AI_PROVIDERS.filter((provider) => provider.value === 'ollama')
  const preferred = preferredAiProviderValue()
  if (AI_PROVIDERS[0]?.value === preferred) return AI_PROVIDERS
  return [
    ...AI_PROVIDERS.filter((provider) => provider.value === preferred),
    ...AI_PROVIDERS.filter((provider) => provider.value !== preferred),
  ]
}

export function findProvider(value: string): AiProvider {
  return AI_PROVIDERS.find((p) => p.value === value) ?? AI_PROVIDERS[0]
}

/** 每供應商獨立儲存的欄位 */
export type AiSettingField = 'apiUrl' | 'apiKey' | 'model' | 'models' | 'latency'

/**
 * 每個供應商的配置鍵。
 *
 * 鍵名格式是歷史約定（`cloudAi.<provider>.<field>`），**不能隨意改**：
 * 改了等於使用者已存的地址與金鑰全部讀不到，介面上看著像被清空了。
 * 另有一組不帶 provider 的 `cloudAi.provider/apiUrl/apiKey/model`，
 * 那是執行時實際生效的"當前配置"，由本頁在儲存/切換時同步過去。
 */
export function aiSettingKey(provider: string, field: AiSettingField): string {
  return `cloudAi.${provider}.${field}`
}

export function providerLabel(value: string): string {
  return AI_PROVIDERS.find((p) => p.value === value)?.label ?? value
}

/**
 * 一条已配置的 AI 服务。
 *
 * 為什麼是這個形狀：原來的模型是「供應商」為主軸、模型只是它下面的一串字串，
 * 而地址和金鑰掛在供應商上。後果有兩個：
 *   1. 兩個不同端點的「OpenAI 相容」模型（比如一個 OpenAI、一個本地 vLLM）沒法各自
 *      配置——切了模型卻還在用同一個地址和金鑰，其中一個必然打不通；
 *   2. 想在 DeepSeek 和千問之間換一下，得走下拉、改地址、改金鑰，等於重配一遍。
 * 把「供應商 + 地址 + 金鑰 + 模型」打包成一條可命名的服務後，切換就是點一下列表。
 */
/**
 * 一次可用性檢測的結果。
 *
 * 為什麼不是單獨存一個 latencyMs：那樣介面上只能說出「多快」，說不出「行不行」。
 * 而使用者按下這個按鈕時想知道的第一件事是"這份配置能不能用"——金鑰對不對、模型開通了沒、
 * 網路通不通；耗時是可用之後的第二個維度（口述場景裡確實重要，但它不是結論）。
 * 失敗也要留痕：卡上必須能顯示「不可用」，而不是停在「未測試」裝作沒事。
 */
export interface AiProfileCheck {
  ok: boolean
  /** 檢測完成的時間戳。用來說"多久之前測的"——一次透過不等於此刻還通 */
  at?: number
  /** 成功時的一次往返耗時 */
  latencyMs?: number
  /** 失敗時的一句話原因（已由 describeProviderError 歸一化過） */
  reason?: string
}

export interface AiProfile {
  id: string
  provider: string
  apiUrl: string
  apiKey: string
  model: string
  /** 最近一次可用性檢測。沒有 = 還沒測過 */
  check?: AiProfileCheck
}

export function makeProfileId(): string {
  return `p-${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`
}

/** 新建一條服務時的初值 */
export function blankProfile(providerValue = preferredAiProviderValue()): AiProfile {
  const meta = findProvider(providerValue)
  return {
    id: makeProfileId(),
    provider: meta.value,
    apiUrl: meta.defaultUrl,
    apiKey: '',
    model: meta.defaultModels[0] ?? '',
  }
}

/**
 * 端點的主機名，且**只在它不是這家供應商的預設地址時**才返回，否則空串。
 *
 * 卡片上要不要顯示地址，取決於它能不能區分兩張卡：用 DeepSeek 官方地址的那張，
 * 寫出 `api.deepseek.com` 只是重複"DeepSeek"這三個字；而兩個都叫「OpenAI 相容」的
 * 端點，主機名是唯一能把它們分開的東西——那正是舊結構根本存不下第二份的那個場景。
 * 只給主機名不給完整 URL：路徑（/v1、/compatible-mode）在窄卡片裡會擠掉模型名，
 * 完整地址在編輯彈窗裡看。
 */
function profileCustomHost(profile: AiProfile): string {
  const hostOf = (url: string): string => {
    try {
      return new URL(url).host
    } catch {
      return url.trim()
    }
  }
  const host = hostOf(profile.apiUrl)
  const defaultHost = hostOf(findProvider(profile.provider).defaultUrl)
  return host && host !== defaultHost ? host : ''
}

/** 一條服務的一行式說法（供應商 · 主機名），用於讀屏名稱等只能給一串文字的地方 */
export function profileSubtitle(profile: AiProfile): string {
  const label = providerLabel(profile.provider)
  const host = profileCustomHost(profile)
  return host ? `${label} · ${host}` : label
}

/** 列表裡一行的主標題。模型名就是標題；還沒填的時候也得有字，否則那一行看著像壞了 */
export function profileTitle(profile: AiProfile): string {
  return profile.model.trim() || t('aiProvider.noModel')
}

/** 一条服务是否填齐了能用的东西 */
export function isProfileComplete(profile: AiProfile): boolean {
  if (!profile.apiUrl.trim() || !profile.model.trim()) return false
  if (!findProvider(profile.provider).keyless && !profile.apiKey.trim()) return false
  return true
}

/**
 * 把儲存裡讀回來的東西還原成服務列表。
 *
 * 為什麼要這麼小心：這個值會經過配置匯出/匯入、使用者手改資料庫、以及將來版本的欄位增減，
 * 任何一條髒資料都不該讓整頁白屏。所以規則是"能救的救、救不了的丟"，絕不拋異常。
 * id 重複同樣要處理——單選和編輯都靠 id 定位，重複 id 會讓你點 A 改到 B。
 */
export function parseProfiles(raw: unknown): AiProfile[] {
  if (!Array.isArray(raw)) return []
  const text = (value: unknown): string => (typeof value === 'string' ? value : '')
  const finite = (value: unknown): number | undefined =>
    typeof value === 'number' && Number.isFinite(value) ? value : undefined
  const used = new Set<string>()
  const out: AiProfile[] = []

  raw.forEach((item, index) => {
    if (!item || typeof item !== 'object') return
    const source = item as Record<string, unknown>

    let id = text(source.id).trim() || `recovered-${index}`
    while (used.has(id)) id = `${id}-${index}`
    used.add(id)

    out.push({
      id,
      provider: text(source.provider) || AI_PROVIDERS[0].value,
      apiUrl: text(source.apiUrl),
      apiKey: text(source.apiKey),
      model: text(source.model),
      check: parseCheck(source.check, source.latencyMs, finite),
    })
  })

  return out
}

/** 檢測結果的容錯解析。第二個引數是更早版本平鋪在 profile 上的 latencyMs，只有測通才會寫。 */
function parseCheck(
  raw: unknown,
  legacyLatency: unknown,
  finite: (value: unknown) => number | undefined,
): AiProfileCheck | undefined {
  if (raw && typeof raw === 'object') {
    const source = raw as Record<string, unknown>
    if (typeof source.ok === 'boolean') {
      const reason = typeof source.reason === 'string' ? source.reason.trim() : ''
      return {
        ok: source.ok,
        at: finite(source.at),
        latencyMs: finite(source.latencyMs),
        reason: reason || undefined,
      }
    }
  }
  const latency = finite(legacyLatency)
  return latency === undefined ? undefined : { ok: true, latencyMs: latency }
}

/** 當前啟用的那條。id 失效（刪掉了、匯入的資料對不上）時回落到第一條，而不是"什麼都沒啟用" */
export function resolveActiveProfile(profiles: AiProfile[], activeId: string): AiProfile | null {
  if (profiles.length === 0) return null
  return profiles.find((p) => p.id === activeId) ?? profiles[0]
}

/**
 * 檢查 API Key 格式，返回提示文字（空字串表示格式正常）。
 *
 * 只保留「幾乎不會誤報」的規則：貼上時常帶進來的空白字元，以及確定的字首約定。
 * 原來還斷言 DeepSeek / 通義千問「必須 35 位」、豆包「必須是 UUID」——供應商一改金鑰
 * 長度，這裡就開始對**合法**金鑰報警，而使用者看到警告的第一反應是去重新生成一個金鑰。
 * 對合法金鑰誤報的代價高於漏報，所以長度斷言去掉了。
 */
export function checkAiKeyFormat(provider: string, key: string): string {
  if (!key.trim()) return ''
  if (/\s/.test(key)) {
    return t('aiProvider.keyHasSpace')
  }
  if ((provider === 'deepseek' || provider === 'qwen') && !/^sk-/.test(key.trim())) {
    return t('aiProvider.keyPrefix', { provider: providerLabel(provider) })
  }
  return ''
}

/** 地址是否像個能用的 base URL。原來這裡零校驗，填錯要等到測試失敗才知道。 */
export function checkApiUrl(url: string): string {
  const u = url.trim()
  if (!u) return ''
  if (!/^https?:\/\//i.test(u)) {
    return t('aiProvider.urlNeedsScheme')
  }
  try {
    const parsed = new URL(u)
    if (!parsed.hostname) return t('aiProvider.urlInvalid')
  } catch {
    return t('aiProvider.urlInvalid')
  }
  return ''
}

/**
 * 从后端 detail 里取那句「回复: xxx」。
 *
 * detail 的完整內容是 `耗時/模型/傳送/回覆` 四行（見 ai_openai_compat.rs 的
 * test_connection），其中「傳送」那行會把內部測試提示詞
 * （`system="只回復「連線正常」四個字…"`）原樣展示給使用者——那是後端的除錯資訊，
 * 不該出現在成功提示裡。這裡只取「回覆」，耗時和模型名走結構化欄位。
 */
export function extractTestReply(detail?: string): string {
  const match = detail?.match(/^回复:\s*(.*)$/m) // i18n-allow: 匹配供應商返回的中文響應欄位
  return match?.[1]?.trim() ?? ''
}

export function formatLatency(ms: number): string {
  return ms < 1000 ? `${ms}ms` : `${(ms / 1000).toFixed(1)}s`
}

/**
 * 延遲分檔。
 *
 * 口述場景的判斷標準和跑分不一樣：使用者鬆開熱鍵站在原地等文字出現在游標處，
 * 一次往返超過 1 秒就已經能感覺到停頓，超過 2 秒基本沒法用來幹活。
 * 所以純數字不夠——1.4s 和 140ms 在介面上必須長得不一樣。
 *
 * 分檔只在"需要提醒"時才給詞：夠快的時候多一個「很快」是噪音，慢下來才值得佔字。
 * 顏色只有 ok / warn 兩檔：紅色在這一頁已經被「不可用」佔了，
 * 「能用但慢」不該和「根本用不了」撞成同一個顏色。
 */
export type LatencyTier = 'instant' | 'fast' | 'normal' | 'slow' | 'tooSlow'

export interface LatencyGrade {
  tier: LatencyTier
  /** 一個詞的結論，和數字一起顯示 */
  label: string
  /** ok = 放心用，warn = 能用但拖，bad = 不建議用 */
  tone: 'ok' | 'warn' | 'bad'
}

/** 分界值集中在這裡，想調手感只改這一處 */
const LATENCY_THRESHOLDS = { instant: 200, fast: 500, normal: 1000, slow: 2000 } as const

export function gradeLatency(ms: number): LatencyGrade {
  if (ms < LATENCY_THRESHOLDS.instant) return { tier: 'instant', label: t('grade.instant'), tone: 'ok' }
  if (ms < LATENCY_THRESHOLDS.fast) return { tier: 'fast', label: t('grade.fast'), tone: 'ok' }
  if (ms < LATENCY_THRESHOLDS.normal) return { tier: 'normal', label: t('grade.normal'), tone: 'ok' }
  if (ms < LATENCY_THRESHOLDS.slow) return { tier: 'slow', label: t('grade.slow'), tone: 'warn' }
  return { tier: 'tooSlow', label: t('grade.tooSlow'), tone: 'bad' }
}

/**
 * 檢測結論的保鮮期。
 *
 * 為什麼需要這個：一次測通只是那一刻的事實。額度用光、金鑰被吊銷、模型下線、
 * 供應商限流都會讓它悄悄失效，而介面上那枚綠色的「可用」會一直掛著，
 * 變成一句無限期的承諾——三天前測的綠燈，讀起來和剛測的一模一樣。
 * 過了保鮮期就把顏色收回中性：綠色只代表"剛剛驗過，可以信"。
 */
export const CHECK_FRESH_MS = 24 * 60 * 60 * 1000

/** 結論還新鮮嗎。沒有時間戳的（更早版本遷移來的）一律當過期，重測一次就好了 */
export function isCheckFresh(check: AiProfileCheck | undefined, now = Date.now()): boolean {
  if (!check?.at) return false
  return now - check.at < CHECK_FRESH_MS
}

/**
 * 「多久之前測的」。
 *
 * 為什麼要顯示這個：一次測通不代表此刻還通（額度用光、金鑰被吊銷、模型下線都會變）。
 * 卡上那枚「可用」標籤如果不說清是什麼時候測的，就變成了一句無限期的承諾。
 * now 作為引數傳入，純函式好測。
 */
export function formatCheckedAt(at: number, now = Date.now()): string {
  const seconds = Math.max(0, Math.round((now - at) / 1000))
  if (seconds < 60) return t('time.justNow')
  // 用 Intl.RelativeTimeFormat 而不是自己拼字串：英文的單複數（"1 minute ago"
  // vs "5 minutes ago"）沒法用一條模板覆蓋，而這是最容易被忽略、也最顯業餘的細節。
  const rtf = new Intl.RelativeTimeFormat(getLocale(), { numeric: 'auto' })
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return rtf.format(-minutes, 'minute')
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return rtf.format(-hours, 'hour')
  return rtf.format(-Math.floor(hours / 24), 'day')
}

// ── 老資料遷移 ────────────────────────────────────────────────────────────

/** 老結構：每個供應商一組配置，模型是一串逗號分隔的字串 */
export interface LegacyProviderData {
  provider: string
  apiUrl: string
  apiKey: string
  /** 當時選中的模型 */
  model: string
  /** 候選模型列表 */
  models: string[]
  /** `model=ms,model=ms` 解析後的耗時表 */
  latencies: Record<string, number>
}

/**
 * 把 `model=ms,model=ms` 解析成表（老版本把整張延遲表存成一個字串）。
 *
 * 只讀不寫：新結構把耗時放在每條服務的 latencyMs 上。損壞條目一律跳過——
 * 這只是給"哪個更快"排序用的展示資料，絕不該因為一條髒資料讓遷移整體失敗。
 */
export function parseLegacyLatencies(raw: string): Record<string, number> {
  const out: Record<string, number> = {}
  for (const part of raw.split(',')) {
    const [name, ms] = part.split('=')
    const value = Number(ms)
    if (name?.trim() && ms !== undefined && ms !== '' && Number.isFinite(value)) {
      out[name.trim()] = value
    }
  }
  return out
}

/**
 * 把老的「每供應商一組配置」攤平成服務列表。
 *
 * 規則：只遷移填齊了的（有地址、且有金鑰或免金鑰）；該供應商下的每個候選模型各成一條；
 * 老的「當前供應商 + 當前模型」成為啟用項。老的儲存鍵一律不刪——降級回舊版本還能用，
 * 也不至於因為遷移出錯就丟掉使用者的金鑰。
 */
export function migrateLegacyProfiles(
  legacy: LegacyProviderData[],
  activeProvider: string,
  activeModel: string,
): { profiles: AiProfile[]; activeId: string } {
  const profiles: AiProfile[] = []

  for (const entry of legacy) {
    const meta = findProvider(entry.provider)
    const url = entry.apiUrl.trim() || meta.defaultUrl
    if (!url) continue
    if (!meta.keyless && !entry.apiKey.trim()) continue

    // 候選列表 + 當時選中的，去重後各成一條
    const models = [...entry.models]
    if (entry.model && !models.includes(entry.model)) models.unshift(entry.model)
    const unique = models.map((m) => m.trim()).filter(Boolean).filter((m, i, a) => a.indexOf(m) === i)
    if (unique.length === 0) continue

    for (const model of unique) {
      const latencyMs = entry.latencies[model]
      profiles.push({
        // 遷移用確定性 id，方便排查，也保證重複遷移不會產生兩份
        id: `legacy-${entry.provider}-${model}`,
        provider: entry.provider,
        apiUrl: url,
        apiKey: entry.apiKey,
        model,
        // 老版本只在測通之後才記耗時，所以有耗時就等於有過一次透過的檢測；
        // 但那是什麼時候測的沒人知道，所以不寫 at（介面上就不會聲稱"剛測過"）
        check: latencyMs === undefined ? undefined : { ok: true, latencyMs },
      })
    }
  }

  const matched = profiles.find((p) => p.provider === activeProvider && p.model === activeModel)
    ?? profiles.find((p) => p.provider === activeProvider)
    ?? profiles[0]

  return { profiles, activeId: matched?.id ?? '' }
}
