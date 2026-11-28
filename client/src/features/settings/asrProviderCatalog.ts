// 雲 API 語音識別的供應商目錄、服務檔案（profile）解析與延遲分檔。
//
// 結構與「AI 服務」頁對齊：一張卡 = 一份完整配置（供應商 + 該平台的憑據），可以存多份，
// 同一家也能存多份（比如兩個百煉帳號、兩套豆包金鑰）。啟用哪一份由 activeProfileId 決定。
//
// 和 AI 服務的唯一實質差別：供應商只能從內建清單裡選，不能填任意地址 —— 每家 ASR 的
// 協議都要一份專門的 Rust 實現（asr_doubao / asr_qwen / asr_qwen_omni / asr_mimo），
// 不像 AI 整理那邊只要是 OpenAI 相容端點就能接。
//
// 「同平台重複粘金鑰」的問題不靠"按平台存"解決（那樣一張卡就不再是一份完整配置了），
// 而是照 AI 服務的做法：新建時若該平台已有配置，自動把金鑰帶過來，介面上說明它從哪來。

import {
  describeDoubaoMissing,
  effectiveDoubaoCredentials,
  type DoubaoConsole,
  type DoubaoCredentials,
} from '@/lib/cloudAsrCreds'

/** 憑據歸屬的平台。同平台的服務用同一種金鑰形態。 */
import { t } from '@/i18n'

export type AsrPlatform =
  | 'doubao' | 'qwen' | 'mimo' | 'groq' | 'openai' | 'google' | 'openrouter'
  // 這個不是「一家服務商」，而是「一類協議」：地址由使用者填，對面是誰我們不知道。
  | 'openai_compat'

export interface AsrPlatformInfo {
  label: string
  /** 去哪兒領金鑰 */
  consoleUrl: string
}

// label 用 getter：這是模組級常量，普通欄位會把載入那一刻的語言固化下來。
// 見 aiProviderCatalog.ts 裡 AiProvider.label 的註釋。
export const ASR_PLATFORMS: Record<AsrPlatform, AsrPlatformInfo> = {
  doubao: { get label() { return t('asrPlatform.doubao') }, consoleUrl: 'https://console.volcengine.com/speech/app' },
  qwen: { get label() { return t('asrPlatform.qwen') }, consoleUrl: 'https://bailian.console.aliyun.com' },
  mimo: { get label() { return t('asrPlatform.mimo') }, consoleUrl: 'https://xiaoai.mi.com' },
  groq: { label: 'Groq', consoleUrl: 'https://console.groq.com/keys' },
  openai: { label: 'OpenAI', consoleUrl: 'https://platform.openai.com/api-keys' },
  // Gemini API 的 key 在 AI Studio 領，不是 Google Cloud 控制台 —— 後者那套
  // （專案 + 服務帳號 + OAuth）我們刻意沒接，見 providers/asr_gemini.rs 的檔案頭。
  google: { label: 'Google Gemini', consoleUrl: 'https://aistudio.google.com/apikey' },
  openrouter: { label: 'OpenRouter', consoleUrl: 'https://openrouter.ai/keys' },
  // consoleUrl 指向協議文件而不是某家控制台 —— 金鑰去哪兒領取決於使用者對接的是誰，
  // 我們給不出那個連結，能給的是「這條協議長什麼樣」。
  openai_compat: {
    get label() { return t('asrPlatform.openaiCompat') },
    consoleUrl: 'https://platform.openai.com/docs/api-reference/audio/createTranscription',
  },
}

/**
 * 「OpenAI 相容」那張卡實際說哪種協議。
 *
 * 這兩套協議都被叫做「OpenAI 相容」，但地址、請求體、響應形狀完全不同：
 *   · `transcriptions` → `/audio/transcriptions`，multipart 傳檔案，響應 `{"text": ...}`。
 *     OpenAI 自己、Groq、矽基流動、whisper.cpp / faster-whisper / FunASR 的包裝都是這套。
 *   · `chat`           → `/chat/completions`，阿里雲百煉 data URL 字串。
 *   · `chat_standard`  → `/chat/completions`，標準 OpenAI `input_audio.data`（裸 base64）
 *     與 `input_audio.format`。相容 Gemini 等使用標準 OpenAI 格式的閘道器。
 *
 * **預設是 `auto`，讓程式碼自己試出來。** 協議是端點的屬性、不是使用者的偏好 ——
 * 要使用者先去翻對方文件才知道該選哪個，等於把我們的實現細節推給他承擔。
 * 顯式值留作退路：探測判不準時（比如填的模型名只在其中一種協議下有效，
 * 另一種的失敗就不是協議原因）使用者得能直接指定。
 */
export type AsrCompatProtocol = 'auto' | 'transcriptions' | 'chat' | 'chat_standard'

export const ASR_COMPAT_PROTOCOLS: AsrCompatProtocol[] = ['auto', 'transcriptions', 'chat', 'chat_standard']

export function parseAsrCompatProtocol(value: unknown): AsrCompatProtocol {
  return value === 'transcriptions' || value === 'chat' || value === 'chat_standard' ? value : 'auto'
}

/**
 * 一家平台下的一個可選模型。
 *
 * **關鍵在 `provider`：同一張卡裡的模型可以走完全不同的後端實現。**
 * 千問那五個就是四種協議（DashScope duplex / HTTP 一次性 / OpenAI-Realtime 風格 /
 * chat+base64），OpenAI 與 Gemini 也各是「HTTP 一次性」加「WebSocket 流式」兩種。
 *
 * 這個欄位的存在就是為了讓「介面按平台歸攏」和「後端按協議分發」兩件事同時成立 ——
 * 早先的做法是一個模型佔一張卡（`qwen_audio_stream`、`qwen_realtime`… 各一張），
 * 介面上千問一家佔了五張卡，使用者得先知道協議差異才能選。
 */
export interface AsrModelOption {
  /**
   * 存進 `profile.model`，也是下拉裡的值。同一張卡內唯一。
   * 用真實模型名（不是內部代號），這樣使用者在下拉裡看到的和服務商文件裡寫的是一個東西。
   */
  id: string
  /** 走哪份後端實現 —— 即 Rust `registry.rs` 分發用的 key，會寫進 `cloudAsr.provider` */
  provider: string
  /**
   * 實際發給服務端的模型名，預設等於 `id`。
   *
   * 目前只有 Omni 那兩個用得上：它們的介面模型名帶 `-realtime` 字尾
   * （`qwen3.5-omni-plus-realtime`），而產品名是不帶的。以前這層對映在
   * asrModels.ts 的 QWEN_OMNI_MODEL_MAP 裡按 provider id 索引，現在跟著模型走。
   */
  apiModel?: string
  // ⚠️ 這裡刻意**沒有** label 欄位。
  //
  // 曾經有過，值是「千問 ASR 一次性」「千問 3.5 Omni Plus」這種 —— 那是
  // 「一張卡 = 一個模型」時代的**卡片標題**，「一次性」靠跟「流式」那張卡對比才有意義。
  // 歸攏成一卡一平台後它們成了卡內的模型名，於是「千問」字首重複、對比物件也消失了，
  // 使用者在「模型」那一欄看到的是一個既不像模型、也說不清是什麼的詞（使用者原話：
  // 「還有什麼『千問一次性』…下面不是應該顯示模型嗎」）。
  //
  // 現在一律顯示真實模型 ID：它一眼認得出是模型，還能拿去對服務商文件和帳單。
  // 「這個模型什麼時候該選」交給 blurb 說，那本來就是它的活。
  /**
   * 一句話說清「什麼時候選這個模型」，顯示在模型下拉下面。
   *
   * 平台卡的 blurb 只能說這一家整體如何，而真正要幫使用者做的決定是**在一家裡面選哪個**
   * （千問那五個的差別比千問和豆包的差別還大）。所以這些文案留在模型級。
   */
  blurb?: string
  /** 邊說邊出字（即時字幕）。**這是模型的屬性，不是平台的** */
  streaming?: boolean
  /** 識別與 AI 整理一步完成，不需要單獨配 AI 服務 */
  omni?: boolean
  /** 需要百煉「業務空間 ID」才能用 */
  needsWorkspaceId?: boolean
  /**
   * 這個模型的地址可以由使用者覆蓋（填了就用他的，空著就用實現裡內建的官方地址）。
   *
   * **這是協議的屬性，不是平台的屬性** —— 所以它掛在模型上而不是卡片上。
   * 千問那張卡裡 `qwen3.8-omni-flash` 是 HTTP（能改），`qwen3.5-omni-plus` 是
   * realtime WebSocket（改不了）：同一張卡裡兩種答案，掛在卡片上就表達不了。
   *
   * 為什麼內建卡也要開放：中轉站/反代很常見（尤其 OpenAI 和 Gemini），
   * 使用者拿的是官方協議、只是換了個域名。不給填就等於逼他們用不了。
   *
   * 安全邊界靠兩件事守住，缺一不可：
   *  1. 這個標記為假的模型，`asrEndpointUrl` 一律返回空串 —— 檔案裡即使殘留了
   *     地址也發不出去（切過供應商就會留下殘值）；
   *  2. WebSocket 那幾份實現壓根不讀 `extra.baseUrl`。
   */
  supportsCustomUrl?: boolean
  /** 地址是必填的（協議卡），不是可選覆蓋 */
  requiresCustomUrl?: boolean
}

export interface AsrProviderEntry {
  /**
   * 卡片 id，存進 `profile.provider`。**一張卡 = 一個平台**，所以它等於 `platform`。
   *
   * ⚠️ 它不再是 Rust 側的分發 key —— 那個現在由選中模型的 `provider` 決定。
   * 存量配置裡 `profile.provider` 存的是舊的分發 key（`qwen_audio_stream` 之類），
   * 由 parseAsrProfiles 遷移過來，見 LEGACY_PROVIDERS。
   */
  id: string
  /** 卡片標題，短 */
  label: string
  platform: AsrPlatform
  /** 一句定位，說清「什麼時候選這家」 */
  blurb: string
  /**
   * 當前內建端點與控制台所面向的帳號地區。
   * `global` 是海外端點，國內直連可能不穩定，卡片上要如實標出來 ——
   * 否則使用者會把「連不上」當成自己金鑰填錯。
   */
  availability: 'mainland_china' | 'global'
  /** 這家有哪些模型。**第一項是預設**（新建配置時選它） */
  models: AsrModelOption[]
  /**
   * 這是「協議卡」：對面是誰我們不知道，地址必填、模型名是自由文本。
   *
   * 兩處與內建卡不同：
   *  · 地址為空時這張卡不算配置完整（內建卡地址是可選的，空著就用官方地址）；
   *  · 模型名**自由文本**而不是下拉 —— 對面掛什麼模型無從知道，
   *    `models` 只當推薦值用，使用者填的值必須原樣保留（見 resolveAsrModelOption）。
   *
   * ⚠️ 「能不能改地址」**不由這個欄位決定**，那是模型級的 `supportsCustomUrl` ——
   * 理由見那邊的註釋。
   */
  customEndpoint?: boolean
  /** 「介面地址」輸入框的佔位示例 */
  urlPlaceholder?: string
}

/**
 * OpenRouter 上的全部轉寫模型（拉取自 `/api/v1/models?output_modalities=transcription`，
 * 2026-09-16，共 21 個）。
 *
 * **這些 slug 是從那個介面原樣抄下來的，不是照模型頁的展示名推的。**
 * 展示名和 slug 經常不是一回事（"Whisper Large V3 Turbo" 的 slug 是
 * `openai/whisper-large-v3-turbo`，但 "Grok STT 1.0" 是 `x-ai/grok-stt-1.0`、
 * "Qwen3 ASR Flash" 帶日期字尾 `qwen/qwen3-asr-flash-2026-02-10`）。
 * 猜錯的表現是 404 "model not found" —— 看著像模型下線了，其實是名字寫錯。
 * 那個介面**不需要金鑰**，要更新清單直接跑：
 *   `python dev-scripts/probe_new_asr_providers.py --target openrouter --list-models`
 *
 * ── 排序按「什麼時候選它」，不按廠商也不按價格 ──
 * 第一項是預設值（resolveAsrModel 在使用者沒選過時給它），所以第一項必須是
 * 「不知道選什麼就用這個」那一個。往後依次是：中文口述、多語種、便宜、其餘。
 *
 * ── 有一類要特別說明：Whisper 系在這裡可能沒有中文標點 ──
 * OpenRouter 的兩種請求形態都沒有 prompt 通道（它接受 prompt 欄位但忽略），
 * 而 Whisper 全靠「拿帶標點的句子做示範」才會輸出標點。所以 whisper-* 這幾個
 * 轉寫中文短句可能一個標點都沒有，排在自帶標點的那些後面。
 * 詳見 providers/asr_openrouter.rs 的檔案頭。
 */
const OPENROUTER_SLUGS = [
  // 預設：自帶標點、準確率第一檔
  'openai/gpt-transcribe',
  // 中文口述優先：qwen 這幾個覆蓋 30 語言 + 22 種中文方言，而且便宜
  'qwen/qwen3-asr-flash-2026-02-10',
  'qwen/qwen3-asr-1.7b',
  'qwen/qwen3-asr-0.6b',
  // 多語種最強的幾個：mai-transcribe-2 是 FLEURS 榜首（60 語言），chirp-3 自帶標點
  'microsoft/mai-transcribe-2',
  'google/chirp-3',
  'microsoft/mai-transcribe-1.5',
  // OpenAI 其餘幾代（token 計費的兩個更透明，whisper 系按時長）
  'openai/gpt-4o-transcribe',
  'openai/gpt-4o-mini-transcribe',
  'openai/whisper-large-v3-turbo',
  'openai/whisper-large-v3',
  'openai/whisper-1',
  // 其餘廠商
  'deepgram/nova-3',
  'meta/muse-voice-transcribe-1.0',
  'x-ai/grok-stt-1.0',
  'mistralai/voxtral-mini-transcribe',
  'mistralai/voxtral-small-24b-2507-stt',
  'mistralai/voxtral-mini-3b-2507',
  'nvidia/parakeet-tdt-0.6b-v3',
  'nvidia/nemotron-3.5-asr-streaming-multilingual-0.6b',
  'fish-audio/transcribe-1',
]

/** 這 21 個都走同一份實現（asr_openrouter.rs），只換 model 字串。 */
const OPENROUTER_MODELS: AsrModelOption[] = OPENROUTER_SLUGS.map((id) => ({
  id,
  provider: 'openrouter_transcribe',
  // OpenRouter 本身就是路由層，改地址的用處不大，但它是 HTTP 協議、
  // 自建反代也有人用，沒有理由單獨禁掉。
  supportsCustomUrl: true,
}))

/**
 * 內建服務清單。順序即卡片順序，也是新建時下拉的順序：把最推薦的放前面。
 *
 * label 刻意不帶模型名字尾（模型名單獨一行小字），否則卡片標題會被
 * 「千問 3.5 Omni Plus（qwen3.5-omni-plus，ASR+AI）」這種括號串撐爆。
 *
 * ── blurb 的口徑（改文案前先讀這段）──
 * 每條只回答一件事：**什麼時候該選它**，而且七條放在一起要能相互區分。
 * 早期的文案是各寫各的優點（「準確率高、速度快」同時掛在兩家上），等於沒給出
 * 選擇依據 —— 使用者看完還是不知道該點哪一張。現在的口徑：
 *   · 準確率的結論是**有條件的**，條件要寫出來。豆包只有在**關掉即時字幕**時
 *     才是中文最準的一檔，一開字幕就掉檔；Audio 3.0 反過來，開著字幕也是第一檔。
 *     少了這個前提，兩張卡的「最準」會互相矛盾。
 *   · 不如別家的地方照實寫（千問 realtime 不及 Audio 3.0、MiMo 準確率一般），
 *     但用詞留餘地 —— 目錄裡留著它們是因為有人的帳號/網路只有那一家能用。
 *   · 引用別的卡一律用**產品名**（"Audio 3.0"、"Omni Plus"），不要用「上面那個」
 *     「同上」：順序會變，位置指代改一次順序就錯。
 *   · 這些準確率結論來自實際使用對比，不是跑分。改結論前先真用一遍。
 */
export const ASR_PROVIDERS: AsrProviderEntry[] = [
  {
    id: 'doubao',
    get label() { return t('asrProvider.doubao') },
    platform: 'doubao',
    availability: 'mainland_china',
    get blurb() { return t('asrProvider.doubaoBlurb') },
    models: [
      {
        id: 'Doubao-Seed-ASR-2.0',
        provider: 'doubao_v2',
        // 它一直支援即時字幕（走火山流式語音識別 2.0，isStreamingDisplayReady 認它）。
        // blurb 裡"不開字幕時最準"是產品建議，不代表它沒有這個能力。
        streaming: true,
      },
    ],
  },
  /**
   * 千問一家八個模型，**四種不同協議** —— 這張卡是 AsrModelOption.provider
   * 存在的原因。以前它們各佔一張卡，介面上千問一家就是五張，使用者得先懂協議差異
   * 才知道該點哪張。
   *
   * 模型順序即推薦順序，第一個是預設：
   *   · Audio 3.1 流式放第一 —— 最新一代，實測與 3.0 同協議、同節奏但延遲略低；
   *     開著即時字幕時這一族比豆包更準（豆包最準的用法是關掉字幕，一開就掉一檔），
   *     而且不需要業務空間 ID，填了金鑰就能用；
   *   · Audio 3.0 流式留作退路 —— 仍完全可用，給「3.1 上有問題」的人一個落腳點；
   *   · qwen3-asr-flash 是錄完一次性出字，多語種均衡；
   *   · realtime 那個效果不及 Audio 3.x，還要業務空間 ID，所以排在後面；
   *   · 四個 Omni 是「識別+整理」一體，選它們就不用再配 AI 服務。3.8 那兩個是
   *     同一代的兩條協議（HTTP / realtime WS），不是新舊關係。
   *
   * ── 這裡**不放**哪些百煉模型，以及為什麼（2026-09-23 實測，見 dev-scripts）──
   *  · `qwen3-omni-flash` / `qwen-omni-turbo`：已從清單裡刪掉。官網標了「即將下線」，
   *    而且**實測已經在被縮容** —— 前者讀超時，後者直接回
   *    「Too many requests ... system capacity limits」。百煉的下線機制是先逐步壓
   *    QPM/TPM 再正式下線，所以這兩個的表現不是偶發抖動。存量配置的落點見
   *    RETIRED_MODELS。
   *  · `qwen-audio-3.1-asr-flash-filetrans` / `-message`：filetrans 只收音訊 URL
   *    （我們發的是 base64，回 `InvalidParameter: url error`）；message 在
   *    OpenAI 相容模式下明確 `model_not_supported`，在 duplex WS 上倒是能轉寫，
   *    但**一條中間結果都不產出**，做不了即時字幕。
   */
  {
    id: 'qwen',
    get label() { return t('asrProvider.qwenPlatform') },
    platform: 'qwen',
    availability: 'mainland_china',
    get blurb() { return t('asrProvider.qwenPlatformBlurb') },
    models: [
      {
        // 與 3.0 走同一份 Rust 實現（asr_qwen_audio_stream.rs），模型名經
        // extra.model 傳下去。實測同協議、同中間結果節奏、同樣接受熱詞欄位。
        id: 'qwen-audio-3.1-asr-flash-streaming',
        provider: 'qwen_audio_stream',
        get blurb() { return t('asrProvider.qwenAudio31StreamBlurb') },
        streaming: true,
      },
      {
        id: 'qwen-audio-3.0-asr-flash-streaming',
        provider: 'qwen_audio_stream',
        get blurb() { return t('asrProvider.qwenAudioStreamBlurb') },
        streaming: true,
      },
      {
        id: 'qwen3-asr-flash',
        provider: 'qwen',
        get blurb() { return t('asrProvider.qwenBlurb') },
      },
      {
        id: 'qwen3-asr-flash-realtime',
        provider: 'qwen_realtime',
        get blurb() { return t('asrProvider.qwenRealtimeBlurb') },
        streaming: true,
        needsWorkspaceId: true,
      },
      {
        /**
         * 最新一代 Omni，**走非即時那條路**（OpenAI 相容 chat/completions）。
         *
         * 它和下面那個 `-realtime` 是同一代的兩條協議，不是新舊關係：這個走 HTTP、
         * 地址可改；那個走 realtime WebSocket、地址固定。對 SayIt 的用法（整段發完
         * 再取文本）兩者結果一樣，差別在計費口徑和能不能換地址。
         *
         * 排在 realtime 那個前面：HTTP 那條路能配中轉站/業務空間專屬域名，
         * 適用面更廣。
         */
        id: 'qwen3.8-omni-flash',
        provider: 'qwen_chat_audio',
        get blurb() { return t('asrProvider.omni38Blurb') },
        omni: true,
        // HTTP 協議，地址可改 —— 百煉官方推薦遷到業務空間專屬域名
        // （`{WorkspaceId}.cn-beijing.maas.aliyuncs.com`），那就得靠這一欄。
        supportsCustomUrl: true,
      },
      {
        /**
         * 同一代 Omni 的 realtime 版，走 asr_qwen_omni.rs 那條 WebSocket。
         *
         * ⚠️ 它曾經被判成「加不了」，理由是它在我們的程式碼路徑上會被拒
         * （`Voice 'Chelsie' is not supported`）。那個結論**錯在沒做對照實驗**：
         * 當時"帶 voice"和"不發 response.create"兩個變數一起變了，所以分不清是哪個
         * 起的作用。固定發 `response.create`、只變音色欄位重測之後結論很乾脆 ——
         * 加一個 `session.audio.output.voice` 就能用，而且 3.5 兩代也接受同一個欄位，
         * 一份會話形狀通吃。實測指令碼：`dev-scripts/probe_qwen38_omni_voice.py`。
         *
         * 所以這裡沒有任何特殊欄位：那個音色欄位是 asr_qwen_omni.rs 對所有 Omni
         * 統一發的，不是給這個模型開的後門。
         */
        id: 'qwen3.8-omni-flash-realtime',
        // 產品名就帶 -realtime，所以**不需要** apiModel（別順手補一個
        // `qwen3.8-omni-flash-realtime-realtime`）。
        provider: 'qwen_omni',
        get blurb() { return t('asrProvider.omni38RealtimeBlurb') },
        omni: true,
      },
      {
        // 介面模型名帶 -realtime 字尾，產品名不帶 —— 這層對映以前在
        // asrModels.ts 的 QWEN_OMNI_MODEL_MAP 裡按 provider id 索引，現在跟著模型走。
        id: 'qwen3.5-omni-plus',
        apiModel: 'qwen3.5-omni-plus-realtime',
        provider: 'qwen_omni',
        get blurb() { return t('asrProvider.omniPlusBlurb') },
        omni: true,
      },
      {
        id: 'qwen3.5-omni-flash',
        apiModel: 'qwen3.5-omni-flash-realtime',
        provider: 'qwen_omni',
        get blurb() { return t('asrProvider.omniFlashBlurb') },
        omni: true,
      },
      // 上一代 Omni（qwen3-omni-flash / qwen-omni-turbo）曾經排在這裡，理由是
      // 「便宜 + 讓存量配置有地方落」。現在刪掉了：實測它們的即時端點已經被縮容到
      // 不可用（超時 / 容量限流），留著等於在下拉裡擺兩個點了就壞的選項，比沒有更糟。
      // 存量配置改由 RETIRED_MODELS 接到 qwen3.5-omni-flash 上。
    ],
  },
  {
    id: 'mimo',
    get label() { return t('asrProvider.mimo') },
    platform: 'mimo',
    availability: 'mainland_china',
    get blurb() { return t('asrProvider.mimoBlurb') },
    models: [{ id: 'mimo-v2.5-asr', provider: 'mimo' }],
  },
  {
    id: 'groq',
    get label() { return t('asrProvider.groq') },
    platform: 'groq',
    availability: 'global',
    get blurb() { return t('asrProvider.groqBlurb') },
    // 兩個都走同一份實現（asr_groq.rs 的 multipart），只換 model 字串。
    // turbo 放第一位：它是 large-v3 剪掉解碼層的版本，快得多而準確率只差一點。
    models: [
      { id: 'whisper-large-v3-turbo', provider: 'groq_whisper', supportsCustomUrl: true },
      { id: 'whisper-large-v3', provider: 'groq_whisper', supportsCustomUrl: true },
    ],
  },
  /**
   * OpenAI 一張卡，兩種實現：前四個走 `/audio/transcriptions`（HTTP multipart，
   * asr_groq.rs 那份），最後一個走 Realtime 轉寫會話（WebSocket，asr_openai_realtime.rs）。
   *
   * **流式那個模型在檔案端點上不存在**，所以它關掉即時字幕時會回落到
   * `gpt-transcribe` —— 那層換名在 asr_groq.rs 的 OPENAI_FILE_FALLBACK 檔裡做，
   * 使用者不會看到「模型不存在」。
   */
  {
    id: 'openai',
    get label() { return t('asrProvider.openai') },
    platform: 'openai',
    availability: 'global',
    get blurb() { return t('asrProvider.openaiBlurb') },
    models: [
      // 前四個是 HTTP，地址可改 —— 用中轉站/反代接 OpenAI 很常見。
      {
        id: 'gpt-transcribe',
        provider: 'openai_transcribe',
        get blurb() { return t('asrProvider.openaiFileBlurb') },
        supportsCustomUrl: true,
      },
      { id: 'gpt-4o-transcribe', provider: 'openai_transcribe', supportsCustomUrl: true },
      { id: 'gpt-4o-mini-transcribe', provider: 'openai_transcribe', supportsCustomUrl: true },
      // 最老那版，留著給「按 whisper-1 調好了 prompt、不想動」的人
      { id: 'whisper-1', provider: 'openai_transcribe', supportsCustomUrl: true },
      {
        // 這個是 WebSocket realtime，地址改不了（中轉站基本不轉發它）。
        // 它關掉即時字幕時會回落到 HTTP，但那條路也不帶使用者地址 ——
        // 否則「我沒給這個模型填地址」和「回落時卻用了別處的地址」就對不上了。
        id: 'gpt-live-transcribe',
        provider: 'openai_live_transcribe',
        get blurb() { return t('asrProvider.openaiLiveBlurb') },
        streaming: true,
      },
    ],
  },
  /** Gemini 一張卡兩種實現：generateContent（HTTP）與 Live API（WebSocket）。 */
  {
    id: 'google',
    get label() { return t('asrProvider.gemini') },
    platform: 'google',
    availability: 'global',
    get blurb() { return t('asrProvider.geminiBlurb') },
    models: [
      {
        // HTTP，地址可改（反代 Gemini 的中轉站不少）
        id: 'gemini-3.5-transcribe',
        provider: 'gemini_transcribe',
        get blurb() { return t('asrProvider.geminiFileBlurb') },
        supportsCustomUrl: true,
      },
      {
        id: 'gemini-3.5-transcribe-live',
        provider: 'gemini_live_transcribe',
        get blurb() { return t('asrProvider.geminiLiveBlurb') },
        streaming: true,
      },
    ],
  },
  /**
   * OpenRouter 是**路由層**，不是一家模型廠商 —— 一把 key 通往多家 STT 模型，
   * 所以它是這份清單裡模型選項最多的一張卡，也是唯一「換模型就等於換廠商」的一張。
   *
   * 放在最後：它多出一層中間商（價格與延遲都比直連略高），適合的場景是
   * 「不想為每家單獨開帳號」。已經有 OpenAI / Google key 的人直連更划算。
   */
  {
    id: 'openrouter',
    get label() { return t('asrProvider.openrouter') },
    platform: 'openrouter',
    availability: 'global',
    get blurb() { return t('asrProvider.openrouterBlurb') },
    models: OPENROUTER_MODELS,
  },
  /**
   * ── 「協議卡」：不是某一家服務商，而是「內建清單裡沒有你要的那家」時用的 ──
   *
   * 起因是 issue #67：內建清單窮舉不完，而自建服務（whisper.cpp / faster-whisper /
   * FunASR）和聚合閘道器的地址每台機器都不一樣，我們不可能替使用者寫進程式碼。
   *
   * **只有一張卡，協議由程式碼自己試出來。** 「OpenAI 相容」在語音這塊其實是兩套
   * 完全不同的協議（實測見 `dev-scripts/probe_bailian_openai_audio.py`）：
   *   · `/audio/transcriptions`：multipart 上傳檔案，響應 `{"text": "..."}`。
   *     OpenAI 自己、Groq、矽基流動、FunASR、whisper.cpp、faster-whisper 都是這套。
   *   · `/chat/completions`：音訊作為 `input_audio` 內容項塞進 messages。
   *     百煉（qwen3-asr-flash / Omni / livetranslate）走的是這套。
   *
   * 這裡曾經做成兩張卡、讓使用者自己選。否掉了：使用者沒辦法知道自己要連的服務說哪種
   * 協議 —— 他得去翻對方文件，或者兩張卡輪流試。那是把我們的實現細節推給他承擔。
   * 而且名字上只能靠「（對話式）」這種括號區分，加上真正的 OpenAI 卡，列表裡會有
   * 三項都以「OpenAI」開頭。協議探測放進 Rust（asr_openai_compat.rs），
   * `profile.protocol` 只作為探測判不準時的手動退路。
   *
   * 放在最後：內建卡是推薦項，這張是兜底。
   */
  {
    id: 'openai_compat',
    get label() { return t('asrProvider.openaiCompat') },
    platform: 'openai_compat',
    availability: 'global',
    customEndpoint: true,
    urlPlaceholder: 'http://127.0.0.1:8000/v1',
    get blurb() { return t('asrProvider.openaiCompatBlurb') },
    // 模型名是自由文本，這裡只給個常見預設值（whisper.cpp / faster-whisper
    // 的 OpenAI 相容服務基本都認它）。
    models: [{
      id: 'whisper-1',
      provider: 'openai_compat',
      supportsCustomUrl: true,
      requiresCustomUrl: true,
    }],
  },
]

/** 這張卡有哪些模型（第一個是預設）。 */
export function asrModelsOf(entry: AsrProviderEntry): AsrModelOption[] {
  return entry.models
}

// 模型的顯示名就是它的 id，所以這裡沒有 asrModelLabel / asrModelShortLabel ——
// 理由寫在 AsrModelOption 上（原來那兩個函式是為 label 欄位服務的）。

/**
 * 舊 provider id → 新的（卡片 id, 模型 id）。
 *
 * **這張表是存量配置的生命線。** 改成「一張卡 = 一個平台」之後，老 profile 裡
 * `provider` 存的還是舊的分發 key（`qwen_audio_stream`、`gemini_live_transcribe`…），
 * 而那些值不再是任何卡片的 id —— 沒有這張表，parseAsrProfiles 會把它們**整條丟掉**，
 * 使用者的全部服務配置連金鑰一起消失。
 *
 * `doubao` / `aliyun` 這兩個是更早的別名（registry.rs 裡還認），一併對映過來。
 *
 * 上一代 Omni 那三個舊 id 也在表裡 —— 它們對應的模型現在作為選項留在千問卡裡
 * （走的是同一份 asr_qwen_omni.rs），所以能原地落下來。表裡沒有的值才丟棄。
 */
const LEGACY_PROVIDERS: Record<string, { provider: string; model: string }> = {
  doubao: { provider: 'doubao', model: 'Doubao-Seed-ASR-2.0' },
  doubao_v2: { provider: 'doubao', model: 'Doubao-Seed-ASR-2.0' },
  qwen: { provider: 'qwen', model: 'qwen3-asr-flash' },
  aliyun: { provider: 'qwen', model: 'qwen3-asr-flash' },
  qwen_audio_stream: { provider: 'qwen', model: 'qwen-audio-3.0-asr-flash-streaming' },
  qwen_realtime: { provider: 'qwen', model: 'qwen3-asr-flash-realtime' },
  qwen_omni_35_plus: { provider: 'qwen', model: 'qwen3.5-omni-plus' },
  qwen_omni_35_flash: { provider: 'qwen', model: 'qwen3.5-omni-flash' },
  // 上一代 Omni 的三個舊 id。它們對應的模型現在作為選項留在千問卡裡，
  // 所以這些配置能落下來而不是被丟棄（早先沒有它們，使用者的卡直接消失）。
  qwen_omni_plus: { provider: 'qwen', model: 'qwen3.5-omni-plus' },
  // 這兩個指向的模型已經從目錄裡刪掉（被百煉縮容），所以落到同族還活著的那個。
  // 目標必須和 RETIRED_MODELS 一致 —— 兩張表給出不同答案的話，同一個使用者
  // 走遷移路徑和走回落路徑會得到不同的模型。
  qwen_omni_flash: { provider: 'qwen', model: 'qwen3.5-omni-flash' },
  qwen_omni_turbo: { provider: 'qwen', model: 'qwen3.5-omni-flash' },
  mimo: { provider: 'mimo', model: 'mimo-v2.5-asr' },
  groq_whisper: { provider: 'groq', model: 'whisper-large-v3-turbo' },
  openai_transcribe: { provider: 'openai', model: 'gpt-transcribe' },
  openai_live_transcribe: { provider: 'openai', model: 'gpt-live-transcribe' },
  gemini_transcribe: { provider: 'google', model: 'gemini-3.5-transcribe' },
  gemini_live_transcribe: { provider: 'google', model: 'gemini-3.5-transcribe-live' },
  openrouter_transcribe: { provider: 'openrouter', model: 'openai/gpt-transcribe' },
  // 協議卡合併前的兩個 id。合併後都落到同一張卡上，模型名由使用者自己填過，
  // 這裡只給個兜底值 —— 真實值在 profile.model 裡，遷移不會動它。
  // 協議交給 auto 重新探測：原來分成兩張卡的時候，卡片 id 本身就是協議，
  // 而合併後那個資訊只能靠探測拿回來。
  openai_compat_transcribe: { provider: 'openai_compat', model: 'whisper-1' },
  openai_chat_audio: { provider: 'openai_compat', model: 'qwen3-asr-flash' },
}

/**
 * 已從目錄裡刪掉的模型 → 接替它的那個。
 *
 * **為什麼不能只靠"回落到卡片預設"。** migrateLegacyProvider 的第 3 種情形本來就會
 * 把認不出的模型名回落到該卡第一項，資料不會丟。但千問卡的第一項是純 ASR，而這裡
 * 退役的兩個都是 Omni（識別 + 整理一體）—— 回落過去會把使用者從「不用配 AI 服務」
 * 悄悄搬到「必須另配 AI 服務」，而他不會收到任何提示，只會發現整理效果不對了。
 * 跨語義的替換要麼明確指定落點，要麼就不該發生。
 *
 * 所以這張表只放**同族替代**：退役的 Omni → 還活著的 Omni。找不到同族替代的模型
 * 不要往這裡加，讓它走回落，那至少是個誠實的預設值。
 */
const RETIRED_MODELS: Record<string, string> = {
  // 2026-09-23：官網標「即將下線」，且實測即時端點已被縮容（超時 / 容量限流）。
  'qwen3-omni-flash': 'qwen3.5-omni-flash',
  'qwen-omni-turbo': 'qwen3.5-omni-flash',
}

/**
 * 把存量條目的 provider 遷移成（卡片 id, 模型 id）。
 *
 * ── 判據必須是「provider + model 這一對是否自洽」，不能只看其中一個 ──
 * 早先寫成「有 model 就是新資料」，結果把使用者的卡片全弄丟了：`model` 欄位是**上一個
 * 版本**就加進來的（那時它表示"同實現下選哪個模型"），所以存量資料裡
 * `provider='gemini_transcribe'` 配 `model='gemini-3.5-transcribe'` 是完全正常的組合 ——
 * 判成新資料後 `findAsrProvider('gemini_transcribe')` 返回 undefined，整條被丟掉。
 *
 * 五種情形，順序有講究：
 *   1. provider 是卡片 id 且 model 屬於這張卡 → 新資料，原樣；
 *   2. provider 在遷移表裡 → 按表遷移，但**使用者選過的模型如果新卡也有就保留**
 *      （否則在 Groq 卡上選過 large-v3 的人會被打回 turbo）；
 *   3. provider 是卡片 id 且 model 是剛退役的模型 → 換成 RETIRED_MODELS 指定的替代；
 *   4. provider 是卡片 id 但 model 不認識（新建後沒選、或模型下線）→ 回落該卡預設；
 *   5. 都不是 → 丟棄。
 *
 * 情形 3 必須排在 4 前面，否則退役的 Omni 會先被情形 4 吃掉、落到卡片預設那個
 * 純 ASR 模型上 —— 理由見 RETIRED_MODELS。
 */
function migrateLegacyProvider(
  provider: string,
  model: string,
): { provider: string; model: string } | null {
  // 退役模型先換名，**必須在所有分支之前**。
  //
  // 踩過的坑：這段原來放在最後那個 `if (direct)` 分支裡，而 `qwen` / `doubao` /
  // `mimo` 這些值**同時**是舊分發 key 和新卡片 id —— 它們會先被下面的
  // LEGACY_PROVIDERS 攔住並直接返回，於是退役對映對最需要它的那批配置
  // （千問卡上的 Omni）壓根不執行。測試裡表現為退役 Omni 落到了 qwen3-asr-flash。
  //
  // 換完名之後由 belongsTo 承擔「替代確實還在這張卡裡」的校驗，所以這裡不必
  // 再自己查一遍：替代要是也沒了，會照常回落，不會返回一個不存在的模型名。
  const trimmed = RETIRED_MODELS[model.trim()] ?? model.trim()
  const direct = findAsrProvider(provider)
  const belongsTo = (entry: AsrProviderEntry) =>
    trimmed !== '' && asrModelsOf(entry).some((m) => m.id === trimmed)

  if (direct && belongsTo(direct)) return { provider, model: trimmed }

  const legacy = LEGACY_PROVIDERS[provider]
  if (legacy) {
    const card = findAsrProvider(legacy.provider)
    return {
      provider: legacy.provider,
      model: card && belongsTo(card) ? trimmed : legacy.model,
    }
  }

  if (direct) return { provider, model: asrModelsOf(direct)[0].id }
  return null
}

/**
 * 舊 provider id → 它現在屬於哪張卡（不認識就原樣返回）。
 *
 * 給「已經自動補建過哪些」那份記帳用（`cloudAsr.autoCreatedProviders`）：
 * 存量裡記的是舊的分發 key，不換算的話新程式碼認不出來，會把使用者主動刪掉的卡
 * 重新補回來一次。
 */
export function asrCardIdOfLegacyProvider(provider: string): string {
  return LEGACY_PROVIDERS[provider]?.provider ?? provider
}

/**
 * 把模型清單按廠商字首分組，供下拉的 optgroup 用。
 *
 * 只有 OpenRouter 用得上：它一張卡下有 20 多個模型、來自十來家廠商，平鋪成一列
 * 找起來很費勁，而 slug 的字首天然就是廠商名。返回 null 表示「不該分組」——
 * 別家的模型名不帶 `/`（`whisper-large-v3`、`gpt-transcribe`），
 * 硬分組只會得到一個名叫空串的組。
 *
 * 組內保持原清單順序（那是「什麼時候選它」的推薦順序），組的順序按各組第一個
 * 元素在原清單裡的位置 —— 這樣預設模型所在的組一定排在最前面。
 */
export function groupAsrModelsByVendor(
  models: AsrModelOption[],
): [string, AsrModelOption[]][] | null {
  if (models.length < 2 || !models.every((m) => m.id.includes('/'))) return null
  const groups: [string, AsrModelOption[]][] = []
  for (const model of models) {
    const vendor = model.id.slice(0, model.id.indexOf('/'))
    const existing = groups.find(([name]) => name === vendor)
    if (existing) existing[1].push(model)
    else groups.push([vendor, [model]])
  }
  return groups
}

export function findAsrProvider(id: string): AsrProviderEntry | undefined {
  return ASR_PROVIDERS.find((p) => p.id === id)
}

/**
 * 地區提示，**只在需要提醒時才給字**。
 *
 * `mainland_china` 刻意返回空串：內建清單裡絕大多數都是國內端點，那是預設情形，
 * 每張卡上都掛一句「面向中國大陸帳號」等於把噪音重複七遍，還把真正要緊的
 * 「這個要海外網路才連得上」沖淡了。availability 欄位本身保留 —— 它是資料，
 * 只是這一條不值得佔介面上的字。
 */
export function asrAvailabilityLabel(entry: AsrProviderEntry): string {
  // 地址由使用者填的卡不談地區：對面可能是 localhost，也可能是境外閘道器，
  // 掛一句「需要海外網路」只會誤導。
  if (entry.customEndpoint) return ''
  switch (entry.availability) {
    case 'global': return t('asrProvider.regionGlobal')
    default: return ''
  }
}

/**
 * 這次要發往的介面地址（空 = 用實現裡內建的官方地址）。
 *
 * 閘門在**選中的那個模型**上，不在卡片上：只有標了 `supportsCustomUrl` 的模型
 * （即 HTTP 協議那些）才會返回非空值。兩個理由：
 *  · 同一張卡裡兩種協議並存 —— 千問卡的 qwen3.8-omni-flash 是 HTTP、
 *    qwen3.5-omni-plus 是 realtime WebSocket，後者改地址沒有意義；
 *  · 檔案裡可能殘留地址（使用者換過供應商 / 換過模型），殘值絕不能被發出去 ——
 *    那會把音訊送到一個他早就不打算用的地方。
 */
export function asrEndpointUrl(profile: AsrProfile): string {
  if (!resolveAsrModelOption(profile)?.supportsCustomUrl) return ''
  return profile.apiUrl.trim()
}

/**
 * 介面地址裡的主機名，用於在卡片上區分同平台的多張卡。
 *
 * 用主機名而不是整條 URL：卡片標題只有一行，`http://192.168.1.9:9000/v1` 這種
 * 會把標題擠沒。解析失敗就原樣返回（使用者可能填了一半），不拋。
 */
export function asrEndpointHost(profile: AsrProfile): string {
  const url = asrEndpointUrl(profile)
  if (!url) return ''
  try {
    return new URL(url).host
  } catch {
    return url
  }
}

/**
 * 同一平台下有哪些卡。
 *
 * 現在是「一張卡 = 一個平台」，所以它恆返回 0 或 1 條 —— 保留這個函式是因為
 * 那條不變數值得有斷言看著（見 asrProviderCatalog.test.ts）。
 */
export function providersOfPlatform(platform: AsrPlatform): AsrProviderEntry[] {
  return ASR_PROVIDERS.filter((p) => p.platform === platform)
}

// ─────────────────────────── 檢測結果 ───────────────────────────

/**
 * 一次識別測試的結論。
 *
 * 存的是結論而不只是耗時：只有耗時的話，介面上能說出「多快」，說不出「行不行」。
 * 不可用也要留痕，否則使用者點完測試、切走再回來，只看到一張和沒測過一樣的卡。
 */
export interface AsrCheck {
  ok: boolean
  /** 測試時間戳，用來判斷結論是否還新鮮 */
  at: number
  /** 轉寫這段測試音訊花了多久（毫秒），只有測通才有 */
  latencyMs?: number
  /** 測試音訊時長（秒）。延遲要除以它才能橫向比較，所以一起存 */
  audioSec?: number
  /** 不可用時的原因，一句話 */
  reason?: string
}

function isFiniteNumber(v: unknown): number | undefined {
  return typeof v === 'number' && Number.isFinite(v) ? v : undefined
}

function parseCheck(raw: unknown): AsrCheck | undefined {
  if (!raw || typeof raw !== 'object') return undefined
  const v = raw as Record<string, unknown>
  if (typeof v.ok !== 'boolean') return undefined
  const at = isFiniteNumber(v.at)
  if (at === undefined) return undefined
  return {
    ok: v.ok,
    at,
    latencyMs: isFiniteNumber(v.latencyMs),
    audioSec: isFiniteNumber(v.audioSec),
    reason: typeof v.reason === 'string' && v.reason ? v.reason : undefined,
  }
}

// ─────────────────────────── 服務檔案 ───────────────────────────

/**
 * 一份語音識別服務配置 = 選定的供應商 + 該平台需要的憑據。
 *
 * 憑據直接掛在檔案上（而不是按平台共享），這樣一張卡就是一份自洽的完整配置，
 * 也才可能同時存兩個帳號。重複粘金鑰的問題由「新建時自動沿用同平台上一份」解決。
 */
export interface AsrProfile {
  id: string
  /** 供應商 id，必須是 ASR_PROVIDERS 裡的一個 */
  provider: string
  /**
   * 使用者給這張卡起的名字，可空。
   *
   * 存在的理由：同一個平台可以有多張卡（兩個帳號、或兩個自建地址），而卡片標題
   * 只有平台名時它們長得一模一樣。這裡原來是自動補「金鑰末 4 位」，使用者一眼就把它
   * 認成「金鑰被顯示出來了」—— 那種區分方式不該由我們替他選，讓他自己起名。
   */
  name: string
  /**
   * 自定義介面地址。
   *
   * 協議卡必填；內建卡**可選** —— 空著就用實現裡的官方地址，填了就走使用者的
   * （中轉站 / 反代很常見，尤其 OpenAI 和 Gemini）。只對選中模型標了
   * `supportsCustomUrl` 時才會真的發出去，見 asrEndpointUrl。
   */
  apiUrl: string
  /**
   * 「OpenAI 相容」那張卡說哪種協議。其餘卡片這個值無意義（協議是寫死的）。
   * 預設 `auto`，由 Rust 側探測；兩個顯式值是探測判不準時的退路。
   */
  protocol: AsrCompatProtocol
  /**
   * 選定的模型 id（該卡 models 清單裡的一個）。
   *
   * 新寫入的條目一律顯式帶值，見 emptyAsrProfile 上的說明。讀取時空值或不認識的值
   * 由 resolveAsrModelOption 回落到該卡預設，不會讓整條配置失效。
   */
  model: string
  /** 千問 / 小米：API Key；豆包：當前控制台代次那一份金鑰 */
  apiKey: string
  /** 豆包舊版控制台的 App ID */
  appId: string
  /** 豆包控制台代次 */
  console: DoubaoConsole
  /** 豆包另一代的金鑰，切換代次時原樣保留，不用重新粘 */
  otherKey: string
  /** 百煉業務空間 ID（流式識別需要） */
  workspaceId: string
  /** Omni 模型的 System Prompt（識別與整理一體，屬於這份服務的行為） */
  omniPrompt: string
  /** 上次識別測試的結論 */
  check?: AsrCheck
}

export function makeAsrProfileId(): string {
  return `asr_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 8)}`
}

/**
 * 一份空白檔案。provider 預設給最推薦的那張卡。
 *
 * **model 一定顯式寫入**，不留空串：`parseAsrProfiles` 靠「有沒有 model」區分
 * 新老資料（`qwen` / `mimo` / `doubao` 這幾個值既是舊分發 key 又是新卡片 id，
 * 光看 provider 分不出來）。留空會讓新建的條目被當成存量資料走遷移，
 * 於是預設模型被換成遷移表裡那個 —— 使用者什麼都沒選，模型卻不是預設的那個。
 */
export function emptyAsrProfile(provider = ASR_PROVIDERS[0].id): AsrProfile {
  const entry = findAsrProvider(provider)
  return {
    id: makeAsrProfileId(),
    provider,
    name: '',
    apiUrl: '',
    protocol: 'auto',
    model: entry ? asrModelsOf(entry)[0].id : '',
    apiKey: '',
    appId: '',
    console: 'new',
    otherKey: '',
    workspaceId: '',
    omniPrompt: '',
  }
}

function str(v: unknown): string {
  return typeof v === 'string' ? v : ''
}

/**
 * 容錯解析儲存裡的檔案列表。
 *
 * 儲存可能被手改過、也可能是更早版本寫的，壞條目一律丟棄而不是讓整頁崩掉。
 * provider 不在內建清單裡的條目也丟掉 —— 留著只會渲染出一張點不動的卡。
 */
export function parseAsrProfiles(raw: unknown): AsrProfile[] {
  return parseAsrProfilesDetailed(raw).profiles
}

/**
 * 同 parseAsrProfiles，但**把解析不出來的原始條目也交回去**。
 *
 * 存在的理由是一次真實的資料損失：解析邏輯有 bug 時，被丟掉的條目會在下一次
 * 「歸一化後寫回」時從磁碟上永久消失 —— 使用者丟的是配置連帶金鑰，而金鑰沒有第二份。
 * 呼叫方（loadAsrProfiles / saveAsrProfiles）拿著 orphans 原樣寫回儲存，
 * 這樣即使解析再出錯，原始 JSON 也一直躺在 db 裡，改好程式碼就能恢復。
 */
export function parseAsrProfilesDetailed(
  raw: unknown,
): { profiles: AsrProfile[]; orphans: unknown[] } {
  if (!Array.isArray(raw)) return { profiles: [], orphans: [] }
  const out: AsrProfile[] = []
  const orphans: unknown[] = []
  const seen = new Set<string>()
  for (const item of raw) {
    if (!item || typeof item !== 'object') {
      orphans.push(item)
      continue
    }
    const v = item as Record<string, unknown>
    // 存量條目的 provider 是舊的分發 key，先遷移成（卡片 id, 模型 id）。
    // 遷移不了（未知 provider）才留作 orphan —— 絕不靜默刪除，見上面的說明。
    const migrated = migrateLegacyProvider(str(v.provider), str(v.model))
    if (!migrated || !findAsrProvider(migrated.provider)) {
      orphans.push(item)
      continue
    }
    const { provider, model } = migrated
    const id = str(v.id) || makeAsrProfileId()
    if (seen.has(id)) {
      orphans.push(item)
      continue
    }
    seen.add(id)
    const consoleRaw = str(v.console)
    out.push({
      id,
      provider,
      name: str(v.name),
      apiUrl: str(v.apiUrl),
      protocol: parseAsrCompatProtocol(v.protocol),
      // 不在這裡校驗模型名：清單會變，而校驗放在 resolveAsrModelOption 裡，
      // 存了個已下線的名字也只會回落到預設，不至於讓整條配置被丟掉
      model,
      apiKey: str(v.apiKey),
      appId: str(v.appId),
      console: consoleRaw === 'legacy' ? 'legacy' : 'new',
      otherKey: str(v.otherKey),
      workspaceId: str(v.workspaceId),
      omniPrompt: str(v.omniPrompt),
      check: parseCheck(v.check),
    })
  }
  return { profiles: out, orphans }
}

/**
 * 一份檔案實際選中的那個模型（含它走哪份實現）。
 *
 * 不在清單裡的值一律回落到該卡的預設模型，而不是原樣用 —— 存量配置可能留著一個
 * 已經下線的模型名（服務商淘汰模型比我們改程式碼勤），照發只會換來一個含義模糊的 400。
 * 卡片本身不存在時返回 undefined，由呼叫方決定怎麼處理。
 */
export function resolveAsrModelOption(profile: AsrProfile): AsrModelOption | undefined {
  const entry = findAsrProvider(profile.provider)
  if (!entry) return undefined
  const models = asrModelsOf(entry)
  const picked = profile.model.trim()
  const found = models.find((m) => m.id === picked)
  if (found) return found
  // 自定義端點的卡：模型名是使用者敲的，對面掛什麼模型我們無從校驗，
  // 不能回落到清單裡那個預設值 —— 那等於把使用者填的模型悄悄換掉。
  // 只借用第一項的 provider（分發 key），模型名由 resolveAsrApiModel 用檔案裡的原值。
  if (entry.customEndpoint && picked) return { ...models[0], id: picked }
  return models[0]
}

/** 一份檔案實際會用哪個模型（顯示與儲存用的那個名字）。 */
export function resolveAsrModel(profile: AsrProfile): string {
  return resolveAsrModelOption(profile)?.id ?? ''
}

/**
 * 執行時 `cloudAsr.provider` 該寫什麼 —— 也就是 Rust 側的分發 key。
 *
 * **這是卡片 id 與後端實現分離之後最容易搞錯的一處。** 卡片 id 是平台
 * （`qwen`），而真正決定走哪個協議的是選中模型的 provider（`qwen_audio_stream`）。
 * 寫錯的表現是「換了模型但行為沒變」，或者更糟：發給一個不認識這個模型的端點。
 */
export function resolveAsrRuntimeProvider(profile: AsrProfile): string {
  return resolveAsrModelOption(profile)?.provider ?? ''
}

/** 執行時 `cloudAsr.model` 該寫什麼 —— 實際發給服務端的模型名。 */
export function resolveAsrApiModel(profile: AsrProfile): string {
  const option = resolveAsrModelOption(profile)
  if (!option) return ''
  return option.apiModel ?? option.id
}

/** 啟用項歸一化：指向不存在的 id 時回落到第一條 */
export function resolveActiveAsrProfile(
  profiles: AsrProfile[],
  activeId: string,
): AsrProfile | null {
  return profiles.find((p) => p.id === activeId) ?? profiles[0] ?? null
}

/**
 * 檔案裡的豆包欄位 → lib/cloudAsrCreds 的憑據結構。
 *
 * 抽出來是因為兩處都要用（算生效憑據、判斷缺什麼）。檔案裡只存「當前代次那把金鑰」
 * 加「另一代那把」，對映時按代次還原成 consoleKey / accessToken。
 */
function toDoubaoCreds(profile: AsrProfile): DoubaoCredentials {
  return {
    console: profile.console,
    consoleKey: profile.console === 'new' ? profile.apiKey : profile.otherKey,
    accessToken: profile.console === 'new' ? profile.otherKey : profile.apiKey,
    appId: profile.appId,
  }
}

/**
 * 算出一份檔案「本次生效的憑據」。
 *
 * 豆包新版控制台下 appId 必須是空串 —— Rust 側就是靠它區分兩代鑑權頭，
 * 這條不變數的說明與測試在 lib/cloudAsrCreds.ts。
 */
export function effectiveAsrCredentials(profile: AsrProfile): { apiKey: string; appId: string } {
  if (findAsrProvider(profile.provider)?.platform === 'doubao') {
    return effectiveDoubaoCredentials(toDoubaoCreds(profile))
  }
  return { apiKey: profile.apiKey.trim(), appId: '' }
}

/**
 * 這份檔案還缺什麼才能用（空串 = 齊了）。文案直接可展示。
 *
 * 豆包那套直接委託 describeDoubaoMissing，不再自己抄一遍：抄的那版曾經把
 * 「還沒填 API Key」寫成沒有空格的「還沒填API Key」，而同一句話會和左下角就緒指示
 * 同屏出現，差一個空格就露餡。同一句使用者可見文案只該有一個出處。
 */
export function describeAsrMissing(profile: AsrProfile): string {
  const entry = findAsrProvider(profile.provider)
  if (entry?.platform === 'doubao') {
    return describeDoubaoMissing(toDoubaoCreds(profile))
  }
  // 協議卡：地址先於金鑰。沒有地址這張卡壓根不知道該往哪兒發，
  // 而自建服務很多是不校驗金鑰的，所以金鑰在這裡不是必填。
  if (resolveAsrModelOption(profile)?.requiresCustomUrl) {
    if (!profile.apiUrl.trim()) return t('asrProvider.missingUrl')
    return ''
  }
  return profile.apiKey.trim() ? '' : t('asrProvider.missingKey')
}

/**
 * 卡片標題。
 *
 * ⚠️ **絕不要在這裡拼金鑰的任何部分。** 這裡原來的做法是：同平台有多張卡時補上
 * 「•• + 金鑰末 4 位」當區分標記。當時的理由是兩張卡只差憑據、否則長得一模一樣；
 * 但使用者看到的第一反應是「我的 API Key 怎麼被顯示出來了」—— 在一個專門用眼睛圖示
 * 遮住金鑰的介面裡，標題上卻掛著金鑰的尾巴，這個自相矛盾比它解決的問題更嚴重。
 *
 * 現在的區分方式，按優先順序：
 *  1. 使用者自己起的名字（`profile.name`）—— 他最清楚這兩張卡差在哪；
 *  2. 自定義端點的主機名 —— 非機密，而且正好是自建卡之間真正的差別；
 *  3. 都沒有就只顯示平台名。此時兩張卡靠下面那行「模型」區分，
 *     模型也相同的話讓使用者去起個名字，我們不替他編一個。
 */
export function asrCardTitle(profile: AsrProfile, siblings: number): string {
  const named = profile.name.trim()
  if (named) return named
  const entry = findAsrProvider(profile.provider)
  const base = entry?.label ?? profile.provider
  if (siblings <= 1) return base
  const host = asrEndpointHost(profile)
  return host ? `${base} · ${host}` : base
}

// ─────────────────────────── 延遲分檔 ───────────────────────────

/**
 * 為什麼不直接複用 AI 服務那套 gradeLatency：口徑完全不同。
 *
 * 那邊測的是一次極小的 LLM 往返，1 秒就算慢。這邊測的是**把一段音訊轉寫成文字**，
 * 一段 3 秒的話花 1 秒返回是正常水平。照搬閾值會把所有 ASR 服務標成「太慢」。
 *
 * 所以按 RTF（耗時 ÷ 音訊時長）分檔，介面上仍顯示毫秒數（使用者看得懂的是這個），
 * 但結論詞來自 RTF —— 這樣即使將來換了測試音訊，檔位也不會跟著漂。
 */
export type AsrLatencyTier = 'instant' | 'fast' | 'normal' | 'slow' | 'tooSlow'

export interface AsrLatencyGrade {
  tier: AsrLatencyTier
  label: string
  /** ok = 放心用，warn = 能用但拖，bad = 不建議用於口述 */
  tone: 'ok' | 'warn' | 'bad'
}

/** RTF 分界值集中在這裡，想調手感只改這一處 */
const RTF_THRESHOLDS = { instant: 0.15, fast: 0.3, normal: 0.5, slow: 0.8 } as const

export function gradeAsrLatency(latencyMs: number, audioSec: number): AsrLatencyGrade {
  // 音訊時長缺失或不合理時不硬猜檔位，只說「已測通」，避免給出一個憑空的結論
  if (!Number.isFinite(audioSec) || audioSec <= 0) {
    return { tier: 'normal', label: t('grade.tested'), tone: 'ok' }
  }
  const rtf = latencyMs / (audioSec * 1000)
  if (rtf < RTF_THRESHOLDS.instant) return { tier: 'instant', label: t('grade.instant'), tone: 'ok' }
  if (rtf < RTF_THRESHOLDS.fast) return { tier: 'fast', label: t('grade.fast'), tone: 'ok' }
  if (rtf < RTF_THRESHOLDS.normal) return { tier: 'normal', label: t('grade.normal'), tone: 'ok' }
  if (rtf < RTF_THRESHOLDS.slow) return { tier: 'slow', label: t('grade.slow'), tone: 'warn' }
  return { tier: 'tooSlow', label: t('grade.tooSlow'), tone: 'bad' }
}
