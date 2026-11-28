/**
 * 集中式預設配置
 *
 * 所有 getSetting() 的預設值統一在此定義。
 * 修改預設值只需改這一個檔案，無需全域性搜尋替換。
 *
 * Prompt 相關配置（內容較長，單獨存放）：
 *   - 內建潤色模式 → src/services/store.ts 的 BUILTIN_PRESETS
 *   - 應用 Prompt 規則 → src/services/personalization/defaults.ts 的 BUILTIN_APP_RULES
 *
 * macOS 上個別預設值不同，見檔案末尾的 MAC_DEFAULT_OVERRIDES。
 */

import { IS_MAC } from '@/lib/platform'
import { DEFAULT_CUSTOM_WAVE } from '@/lib/overlayWaveColors'

export const DEFAULTS: Record<string, unknown> = {

  // ── 介面語言 ──
  // 只管**介面文字**，不改識別語種。設定層已經有三個"語言"了
  // （localAsr.language / server.language 是 ASR 語種，Preset 決定輸出語種），
  // 所以這個鍵必須帶 ui. 字首，別再起名叫 language。
  'ui.language': 'auto', // 可選: 'auto'（跟隨系統）| 'zh-TW' | 'en'

  // ── 工作模式 ──
  workMode: 'local', // [自訂 local-only] 只支援 'local'

  // ── 快捷鍵 ──
  // 按住說話。舊單鍵保持 DOM code；組合鍵使用物理 code 格式，如 'ControlLeft+MetaLeft' 或 'ControlLeft+KeyK'。
  // ⚠️ 預設鍵不能用 Shift：長按右 Shift 約 8 秒會觸發 Windows 篩選鍵，導致鬆開後錄音停不下來
  // （詳見 lib/shortcutKeys.ts 的 PTT_FORBIDDEN_CODES）。這裡曾經是 'ShiftRight'，
  // 於是每台新裝的機器開箱就綁在那個鍵上。右 Ctrl 位置相近、沒有輔助功能陷阱，
  // 也不和免持的預設鍵（右 Alt）撞。
  // ⚠️ 改這裡要同步 Rust：src-tauri/src/defaults.rs 和 storage/mod.rs 的種子。
  // defaults.rs 裡有讀本檔案的對帳測試，漏改 cargo test 會紅。
  // 對帳測試按「key: '值',」逐行解析，這幾行保持單行字面量。
  shortcutPTT: 'ControlRight',
  shortcutHandsFree: 'AltRight', // 免持模式。預設右 Alt 單鍵。也支援組合鍵格式如 'Control+Shift+S'
  // AI 整理總開關。留空 = 不註冊，避免升級時意外佔用使用者已有的全域性組合鍵。
  shortcutToggleAi: '',

  // ── 麥克風 ──
  selectedMic: '', // 裝置 ID，空字串 = 系統預設
  muteSystemAudioWhileRecording: false, // 按住說話期間靜音系統其他聲音（防外放被麥克風回採）。預設關閉
  // 瀏覽器降噪。預設開（底噪大的機器需要）；降噪按"人聽得舒服"最佳化，
  // 可能削掉 ASR 要的細節，麥克風環境乾淨時可以關掉對比準確度。
  micNoiseSuppression: true,

  // ── 文本插入 ──
  protectClipboard: true, // 插入文本後自動還原剪貼簿為插入前內容，避免佔用使用者剪貼簿。預設開啟

  // ── AI 校對 ──
  aiEnabled: false, // [自訂：預設關閉] 是否開啟 AI 校對。可選: true | false
  // 0 = 每段語音都整理；大於 0 時，僅錄音達到該秒數才呼叫 AI（最多 300 秒）。
  aiMinDurationSec: 0,
  // 讀取游標附近文字/選區並交給 AI，用於上下文續寫與語音編輯。涉及正文讀取，預設關閉。
  contextAwareWritingEnabled: false,
  contextSelectionAiProfileId: '', // 空字串 = 選區編輯沿用當前 AI；否則引用已儲存的 AI 服務
  aiPromptAppend: '', // 全局附加 prompt
  'ai.builtinPromptLanguage': 'zh-TW', // 內建 Prompt 內容語言；與介面語言相互獨立

  // ── AI 供應商 ──
  // 下面這四個是**執行時生效的那一份**，錄音鏈路、歷史重跑、診斷頁、反饋上報都只認它們。
  // 它們由「AI 服務」頁在啟用/儲存時整份寫入，不要在別處單獨改其中一個
  // （歷史 bug：切了模型但地址和金鑰還是上一家的）。
  // [自訂] S34：預設改為本機 Ollama（原為 'deepseek'、apiUrl ''）。Rust 端只接受 ollama＋loopback。
  'cloudAi.provider': 'ollama', // [自訂] 本 fork 只允許 'ollama'
  'cloudAi.apiUrl': 'http://127.0.0.1:11434', // [自訂] 本機 Ollama 預設位址
  'cloudAi.apiKey': '',
  'cloudAi.model': '',
  // 「AI 服務」列表本身：一條 = 供應商 + 地址 + 金鑰 + 模型，只有設定頁讀寫。
  'cloudAi.profiles': [],
  'cloudAi.activeProfileId': '',
  // 老的「每供應商一組配置」（cloudAi.<provider>.*）只攤平遷移一次。
  // 沒有這個標記，使用者刪光列表後下次進頁面又會被"救回來"。老鍵一律不刪，方便降級。
  'cloudAi.profilesMigrated': false,

  // ── ASR（雲 API）──
  // 可選值以 features/settings/asrProviderCatalog.ts 的 ASR_PROVIDERS 為準：
  // 'doubao_v2' | 'qwen' | 'qwen_audio_stream' | 'qwen_realtime' | 'qwen_omni_35_*'
  // | 'mimo' | 'groq_whisper' | 'openai_transcribe' | 'openai_live_transcribe'
  // | 'gemini_transcribe' | 'gemini_live_transcribe' | 'openrouter_transcribe'
  'cloudAsr.provider': 'doubao_v2',
  // 啟用服務選定的模型。只有「同協議多模型」的服務（Groq / OpenAI）用得上，
  // 空串 = 讓後端用它自己的預設。寫入前已由 resolveAsrModel 歸一，見 asrProfileStore。
  'cloudAsr.model': '',
  // 執行時讀的「本次生效憑據」映象。豆包按控制台代次算出來後寫進這兩個鍵：
  // 新版控制台只有一個 API Key、appId 必為空串（Rust 側靠它區分兩代鑑權頭）。
  'cloudAsr.apiKey': '',
  'cloudAsr.appId': '', // 僅豆包舊版控制台需要
  // 豆包控制台代次與各自的金鑰（兩代的金鑰不是同一個東西，分開存，切換不丟）
  'cloudAsr.doubao.console': 'new', // 'new' = 只要 API Key | 'legacy' = App ID + Access Token
  'cloudAsr.doubao.consoleKey': '', // 新版控制台的 API Key
  'cloudAsr.omniSystemPrompt': '', // 千問 Omni 模式的 system prompt
  // 服務列表：一張卡 = 一份完整配置（供應商 + 該平台憑據），同一家可存多份
  'cloudAsr.profiles': [],
  'cloudAsr.activeProfileId': '',
  // 已經自動補建過哪些服務。刻意不是「遷移完成」那種布林標記 —— 記「補過誰」才能
  // 既在邏輯修好後自愈，又不把使用者主動刪掉的卡救回來。
  'cloudAsr.autoCreatedProviders': [],

  // ── ASR（本地）──
  // 可選值就是 catalog.rs 裡那幾個 id：'sensevoice-small-gguf'（預設，最快）
  // | 'funasr-nano-2512-gguf' | 'qwen3-asr-0.6b-gguf'
  // | 'qwen3-asr-1.7b-q4-gguf' | 'qwen3-asr-1.7b-gguf'（最準）
  'localAsr.modelId': 'sensevoice-small-gguf',
  'localAsr.language': 'auto', // 可選: 'auto' | 'zh' | 'en' | 'ja' | 'ko'
  // GGUF 權重目前只發在 HuggingFace（handy-computer 組織下），國內走映象更穩。
  // 值要和 catalog 裡 DownloadSource.source 的名字完全一致，否則會回落到第一個源。
  'localAsr.downloadSource': 'HuggingFace', // [自訂] S25：預設官方 huggingface.co（原為 'HuggingFace Mirror'）
  'localAsr.model': '',
  // GGUF 引擎的計算後端偏好。'auto' = 有 GPU 用 GPU、沒有自動用 CPU。
  // 沒裝 GPU 加速包的機器永遠是 CPU，這個值不影響功能，隻影響速度。
  'localAsr.accelerator': 'auto', // 可選: 'auto' | 'cpu' | 'gpu'
  // 多顯示卡機器上指定用哪張卡。空串 = 交給引擎自動挑（獨顯優先）。
  // 值是 GgufDevice.id（後端報的 PCI 匯流排 id，拿不到時回落成 "kind:name"），
  // **不是** registry 索引 —— 索引會隨驅動更新變動，存索引等於讓使用者的選擇
  // 在某次驅動更新後靜默指向另一張卡。設定介面只在檢測到兩張以上 GPU 時出現。
  'localAsr.gpuDevice': '',
  // 本地模型空閒多少分鐘後解除安裝（實測釋放 350 MB ~ 2.6 GB 記憶體）。0 = 從不解除安裝。
  // 預設常駐，保證下一次識別無需重新付“載入 + 預熱”的等待；記憶體緊張的使用者可在
  // 本地模式設定中選擇空閒 10 / 30 / 60 分鐘後自動釋放。
  'localAsr.unloadIdleMinutes': 0,

  // ── 伺服器 ──
  // 可選: 'auto' | 'zh' | 'en'。隨每次識別發給服務端，由 asr.py 的 _resolve_language
  // 對映成 Chinese / English；'auto' = 交給模型自檢。
  // （註釋原來寫的是 'Chinese' | 'English' | 'Cantonese'，那是服務端內部的取值，
  //   客戶端從來沒寫過這幾個字串。）
  // 伺服器模式下的 AI 來源。managed = 伺服器內建；custom = 伺服器只做 ASR，客戶端呼叫當前 AI 檔案。
  'server.aiSource': 'managed', // 可選: 'managed' | 'custom'
  'server.language': 'auto',

  // ── 懸浮窗 ──
  overlayWaveTheme: 'black-rainbow', // 可選: 'black-rainbow' | 'black-blue' | 'black-white' | 'custom'
  // 波形主題選 'custom' 時用的配色：{ mode: 'solid' | 'gradient', from: '#rrggbb', to: '#rrggbb' }。
  // 太暗的顏色在黑底膠囊上會被自動調亮，見 lib/overlayWaveColors.ts。
  overlayWaveCustom: DEFAULT_CUSTOM_WAVE,
  overlayShowDuration: true, // 是否顯示錄音時長。可選: true | false
  overlayWidth: 'short', // 可選: 'short' | 'medium' | 'long'

  // ── 流式即時顯示 ──
  // 開啟後，支援流式的 ASR 模型（豆包、千問即時）在識別階段會把即時文字顯示在懸浮窗上。
  // 識別完成後文本仍會照常交給 AI 處理。可選: true | false
  streamingDisplayEnabled: false,

  // ── 熱詞注入 AI 提示詞 ──
  // 開啟後，AI 整理時會收到你的熱詞表，幫助糾正/保留專有名詞。預設關閉。可選: true | false
  injectHotwordsToPrompt: false,

  // ── 提示音 ──
  readySoundEnabled: true, // 錄音就緒提示音。可選: true | false

  // ── 應用設定 ──
  // autoCheckUpdate 在介面上已經沒有開關了（更新是必走的：後台下載 + 使用者點選或退出時安裝）。
  // 這裡保留預設值與讀取，是給更新鏈路自己出故障時留一條不用發新版的止血通道
  // （0.0.8 那次更新器把使用者鎖在死迴圈裡，當時只能靠這個開關關掉）。
  autoCheckUpdate: false, // [自訂] 更新功能已移除；此值保留為 false // 是否檢查更新。可選: true | false
  // 已下載待安裝的更新包 { version, filePath, sha512 }。Rust 側退出時會讀它做兜底安裝。
  pendingUpdate: null,
  historyEnabled: false, // [自訂：預設關閉] 儲存歷史記錄（文本+錄音）。關閉後不再儲存新記錄，適合共享電腦。可選: true | false
  audioRetentionEnabled: false, // [自訂：預設關閉] 保留錄音檔案。可選: true | false
  audioRetentionDays: 7, // [自訂] S61：預設由 -1（永久）改為 7 天。錄音保留天數。可選: 7 | 30 | 90 | -1（永久）
  logRetentionDays: 30, // 日誌保留天數。可選: 7 | 15 | 30 | 90

  // ── WebDAV 備份 ──
  // 備份包一定含配置，歷史與錄音可選。兩個都預設 false：錄音庫能有幾個 GB，而網盤
  // （尤其堅果雲免費版）有月流量額度，預設開啟等於替使用者做了一個很貴的決定。
  // ⚠️ Rust 側在 src-tauri/src/defaults.rs 有一份（includeHistory / includeAudio / keepCount），
  // 改這裡要一起改 —— 兩份不一致時介面顯示的和實際上傳的就不是一回事了。那邊有讀本檔案的
  // 對帳測試，漏改 cargo test 會紅。
  'webdav.enabled': false, // 自動定期備份。可選: true | false
  'webdav.url': '', // 目錄地址，只接受 https（Basic 認證等於明文發密碼）
  'webdav.username': '',
  'webdav.password': '', // 堅果雲要用「應用密碼」，不是登入密碼
  'webdav.includeHistory': false, // 備份含歷史記錄
  'webdav.includeAudio': false, // 備份含錄音檔案（體積大，開啟時會提醒）
  'webdav.intervalHours': 24, // 備份間隔小時數。可選: 24 | 72 | 168
  'webdav.keepCount': 5, // 伺服器上保留的份數。可選: 3 | 5 | 10
  'webdav.lastBackupAt': 0, // 上次**成功**的時刻（ms）。間隔判定只看這個
  'webdav.lastAttemptAt': 0, // 上次嘗試的時刻（ms），失敗退避用
  // 上次備份結果 { at, ok, fileName, bytes, includeHistory, includeAudio, error }。
  // 失敗也記：靜默失敗幾個月的備份在介面上和正常備份長得一模一樣。
  'webdav.lastResult': null,

  // ── 文本後處理（不依賴 AI 的客戶端文本規範化）──
  textPostProcess: {
    autoSegment: true, // 智慧分段（僅極速模式生效），預設開啟
    normalizeNumbers: true, // 數字規範化：百分之/小數/分之/結構化整數，預設開啟
    stripTrailingPunctuation: false, // 去除句末標點
    punctuationToSpace: false, // 標點符號替換為空格
  },

  // ── 熱詞 ──
  hotwordLearning: null,

  // ── 引導 ──
  onboardingVersion: '', // 已完成引導的版本號，空字串 = 未完成

  // ── 遠端公告 ──
  dismissedNoticeIds: [], // 已被使用者關閉的公告 id 列表
}

/**
 * macOS 上覆蓋的預設值。MacBook 內建鍵盤沒有右 Ctrl，所以按住說話改成右 ⌥、免持改成右 ⌘。
 * ⚠ 同樣要和 Rust 對帳（defaults.rs 的宏），對帳測試逐行解析，保持單行字面量。
 */
export const MAC_DEFAULT_OVERRIDES: Record<string, unknown> = {
  shortcutPTT: 'AltRight',
  shortcutHandsFree: 'MetaRight',
}

/**
 * 獲取指定 key 的預設值。
 * 如果 key 不在 DEFAULTS 中，返回提供的 fallback。
 */
export function getDefault<T>(key: string, fallback?: T): T {
  if (IS_MAC && key in MAC_DEFAULT_OVERRIDES) {
    return MAC_DEFAULT_OVERRIDES[key] as T
  }
  if (key in DEFAULTS) {
    return DEFAULTS[key] as T
  }
  return fallback as T
}
