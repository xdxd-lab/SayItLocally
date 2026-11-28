// 文本後處理 — 不依賴 AI 的客戶端文本規範化
//
// 提供三類可開關的處理：
//   1. 數字規範化（A 檔，盡力而為）：百分之 / 小數點 / 分之 / 含位值詞的結構化整數
//   2. 去除句末標點
//   3. 標點符號替換為空格
//
// 處理順序在 applyTextTransforms 中固定：數字規範化 → 使用者替換規則 → 去句末標點 → 標點轉空格。
// 數字規範化必須在標點處理之前（小數點依賴「點」，標點轉空格會吃掉標點）。

import { getSetting, setSetting } from './store'
import { applyTextReplacements } from './textReplacement'
import { segmentAsrText } from './textSegmenter'

// ── 中文數字字元表 ──
// i18n-allow-start: 中文數字解析演算法的輸入與輸出標記
const CN_DIGITS: Record<string, number> = {
  '零': 0, '〇': 0, '一': 1, '二': 2, '\u4e24': 2, '三': 3, '四': 4,
  '五': 5, '六': 6, '七': 7, '八': 8, '九': 9, '\u5e7a': 1,
}
const CN_SMALL_UNITS: Record<string, number> = { '十': 10, '百': 100, '千': 1000 }
const CN_BIG_UNITS: Record<string, number> = { '\u4e07': 10000, '\u4ebf': 100000000 }

/** 所有中文數字字元（含單位） */
const NUM_CHARS = '零〇一二\u4e24三四五六七八九十百千\u4e07\u4ebf\u5e7a'
/** 純數字字元（不含單位），用於小數部分逐位讀取 */
const DIGIT_CHARS = '零〇一二\u4e24三四五六七八九\u5e7a'
/** 位值單位字元 */
const UNIT_CHARS = '十百千\u4e07\u4ebf'

/**
 * 連續單位詞構成的固定詞/成語黑名單：這些通過了「長度≥2 且含單位」的過濾，
 * 但語義上不是數字，需排除。多數含單位的成語（千方百計、十全十美等）因為
 * 單位與非數字字元交替出現、拆分後單位單獨成段（長度<2）已被自動跳過，
 * 這裡只列舉「多個單位/數字連續」的高頻誤傷詞。可按需擴充。
 */
const INTEGER_BLACKLIST = new Set([
  '千\u4e07', '\u4e07一', '\u4e07\u4e07', '千千\u4e07\u4e07', '\u4ebf\u4e07', '\u4e07千', '十\u4e07八千',
])

/** 解析一段中文整數為數值；無法解析返回 null。支援口語省略（三千二=3200、一萬五=15000）。 */
function parseChineseInteger(s: string): number | null {
  if (!s) return null
  let total = 0
  let section = 0
  let number = 0
  let lastUnit = 0 // 最近一次使用的單位量級（大小單位皆計）
  let sawZero = false
  let hadAny = false

  for (const ch of s) {
    if (ch in CN_DIGITS) {
      number = CN_DIGITS[ch]
      if (ch === '零' || ch === '〇') sawZero = true
      hadAny = true
    } else if (ch in CN_SMALL_UNITS) {
      const unit = CN_SMALL_UNITS[ch]
      if (number === 0) number = 1 // 「十五」→ 十 隱含 1
      section += number * unit
      number = 0
      lastUnit = unit
      hadAny = true
    } else if (ch in CN_BIG_UNITS) {
      const unit = CN_BIG_UNITS[ch]
      section += number
      total += section * unit
      section = 0
      number = 0
      lastUnit = unit
      sawZero = false
      hadAny = true
    } else {
      return null
    }
  }

  if (!hadAny) return null
  // 口語省略的末位數字：無「零」時按下一量級縮放（三千二=3200、一萬五=15000、兩百五=250）
  if (number !== 0 && lastUnit >= 10 && !sawZero) {
    section += number * (lastUnit / 10)
    number = 0
  }
  return total + section + number
}

/** 解析可能帶小數點的中文數字表達為字串；失敗返回 null。 */
function parseNumberExpr(s: string): string | null {
  const dotIdx = s.indexOf('\u70b9')
  if (dotIdx === -1) {
    const n = parseChineseInteger(s)
    return n === null ? null : String(n)
  }
  const intPart = s.slice(0, dotIdx)
  const decPart = s.slice(dotIdx + 1)
  if (!decPart) return null
  const intVal = intPart ? parseChineseInteger(intPart) : 0
  if (intVal === null) return null
  let dec = ''
  for (const ch of decPart) {
    if (ch in CN_DIGITS) dec += String(CN_DIGITS[ch])
    else return null
  }
  return `${intVal}.${dec}`
}

function containsUnit(s: string): boolean {
  for (const ch of s) if (UNIT_CHARS.includes(ch)) return true
  return false
}

/** 若替換處緊跟在英文字母之後，前面補一個空格（如 GPT五點四 → GPT 5.4）。 */
function maybeSpace(str: string, offset: number, replacement: string): string {
  const prev = offset > 0 ? str[offset - 1] : ''
  return /[A-Za-z]/.test(prev) ? ' ' + replacement : replacement
}

/**
 * 中文數字 → 阿拉伯數字（A 檔：有明確訊號才轉，盡力而為）。
 * 覆蓋：百分之 / 小數點（數字點數字）/ 分之 / 含位值詞的結構化整數。
 * 刻意不轉「孤立單字數字」和「無位值詞的逐位串」（如 一二三、三三零六），
 * 避免誤傷成語、口語與號碼。複雜/歧義場景建議開 AI 整理。
 */
export function convertChineseNumbers(text: string): string {
  if (!text) return text
  // [自訂] SC-67：辨識輸出已轉成台灣正體。下面的規則以「點、兩、萬、億、鐘、時」的簡體寫法撰寫，
  // 這些簡體字在原始碼中以 Unicode 跳脫儲存（\u70b9 點、\u4e24 兩、\u4e07 萬、\u4ebf 億、\u949f 鐘、\u65f6 時；\u5e7a 是口語的「一」，Big5 沒有此字形），
  // 正體輸入先對應成簡體再套規則，產出的標記（如「9點半」）再換回正體。
  // 逐句判斷（與 Rust 端相同）：含假名（日文）或本來就含這些簡體字的句子維持原行為，不做對應。
  // 不以半形「.」切句，避免切開小數。
  return text.split(/(?<=[。！？!?；;…\n])/).map(convertChineseNumbersSentence).join('')
}

function convertChineseNumbersSentence(text: string): string {
  if (/[\u3041-\u3096\u30a1-\u30fa\u31f0-\u31ff\uff66-\uff6f\uff71-\uff9d]/.test(text) || /[\u70b9\u4e24\u4e07\u4ebf\u949f\u65f6]/.test(text)) return convertChineseNumbersCore(text)
  const mapped = text.replace(/[點兩萬億鐘時]/g, (c) => TRAD_NUMBER_CHARS[c] ?? c)
  if (mapped === text) return convertChineseNumbersCore(text)
  return convertChineseNumbersCore(mapped).replace(/[\u70b9\u4e24\u4e07\u4ebf\u949f\u65f6]/g, (c) => SIMP_NUMBER_CHARS[c] ?? c)
}

const TRAD_NUMBER_CHARS: Record<string, string> = {
  '點': '\u70b9', '兩': '\u4e24', '萬': '\u4e07', '億': '\u4ebf', '鐘': '\u949f', '時': '\u65f6',
}
const SIMP_NUMBER_CHARS: Record<string, string> = Object.fromEntries(
  Object.entries(TRAD_NUMBER_CHARS).map(([trad, simp]) => [simp, trad]),
)

function convertChineseNumbersCore(text: string): string {
  let result = text

  // 1. 百分之X → X%
  result = result.replace(
    new RegExp(`百分之([${NUM_CHARS}\u70b9]+)`, 'g'),
    (m: string, expr: string, offset: number, str: string) => {
      const val = parseNumberExpr(expr)
      return val === null ? m : maybeSpace(str, offset, `${val}%`)
    },
  )

  // 2. X分之Y → Y/X
  result = result.replace(
    new RegExp(`([${NUM_CHARS}]+)分之([${NUM_CHARS}]+)`, 'g'),
    (m: string, a: string, b: string) => {
      const av = parseChineseInteger(a)
      const bv = parseChineseInteger(b)
      return av === null || bv === null ? m : `${bv}/${av}`
    },
  )

  // 2.5 時間表達：必須在小數規則之前處理，避免「點」被當成小數點。
  //     時/分數字轉阿拉伯，保留「點/分/半」標記（如 九點三十二分 → 9點32分）。
  const TIME_HOUR = '零一二\u4e24三四五六七八九十'
  const TIME_MIN = '零一二\u4e24三四五六七八九十'
  const DIGITS_NO_TEN = '零一二\u4e24三四五六七八九'

  // 合法小時：單字數字，或含「十」的複合（十 / 十二 / 二十三…），範圍 0–24。
  // 借「含十」排除「一二三」這類逐位串（parseChineseInteger 對它們會給出誤導性結果）。
  const validHour = (s: string): number | null => {
    if (s.length > 1 && !s.includes('十')) return null
    const h = parseChineseInteger(s)
    return h === null || h < 0 || h > 24 ? null : h
  }

  // 2.5a 時間：X點半 / X點Y分 / X點[含十的分鐘]（九點半→9點半、九點三十二分→9點32分、九點三十→9點30）
  result = result.replace(
    new RegExp(
      `([${TIME_HOUR}]+)\u70b9(半|[${TIME_MIN}]+分(?!之)|[${DIGITS_NO_TEN}]*十[${DIGITS_NO_TEN}]*)`,
      'g',
    ),
    (m: string, hourStr: string, tail: string) => {
      const hour = validHour(hourStr)
      if (hour === null) return m
      if (tail === '半') return `${hour}\u70b9半`
      if (tail.endsWith('分')) {
        const min = parseChineseInteger(tail.slice(0, -1))
        return min === null || min < 0 || min > 59 ? m : `${hour}\u70b9${min}分`
      }
      // 含「十」的分鐘、無「分」（九點三十 → 9點30）
      const min = parseChineseInteger(tail)
      return min === null || min < 0 || min > 59 ? m : `${hour}\u70b9${min}`
    },
  )

  // 2.5b 整點：X點（後面不跟數字/點）→ 9點（如 上午九點 → 上午9點）。
  //      「點」後跟數字的（三點一四）留給小數規則，避免誤傷。
  //
  //      收緊：裸整點必須有明確「時間訊號」才轉，否則保留中文。目的是不把口語裡
  //      表「a little bit」的「一點」誤當成「1 點鐘」——如「調寬一點」「有一點」
  //      「快一點」。時間訊號（滿足其一即轉）：
  //        ① 前面緊鄰時段詞（上午 / 下午 / 晚上 …）；
  //        ② 後面緊跟「鍾」（一點鐘）；
  //        ③ 小時是含「十」的兩位（十點 / 十二點，幾乎只作時間用）。
  //      無訊號時寧可不轉，交給 AI 整理處理歧義場景（與整體「盡力而為」取向一致）。
  const TIME_OF_DAY_PREFIX = /(凌晨|清晨|早晨|早上|上午|中午|下午|傍晚|晚上|夜\u91cc|夜裡|半夜|今早|今晚|明早|明晚|昨晚)$/
  result = result.replace(
    new RegExp(`([${TIME_HOUR}]+)\u70b9(?![${DIGIT_CHARS}\u70b9])`, 'g'),
    (m: string, hourStr: string, offset: number, str: string) => {
      const hour = validHour(hourStr)
      if (hour === null) return m
      const hasTenUnit = hourStr.includes('十')
      const hasClockSuffix = (str[offset + m.length] || '') === '\u949f'
      const hasTimePrefix = TIME_OF_DAY_PREFIX.test(str.slice(0, offset))
      if (!hasTenUnit && !hasClockSuffix && !hasTimePrefix) return m
      return maybeSpace(str, offset, `${hour}\u70b9`)
    },
  )

  // 3. 小數 / 多段點分數字：數字點數字(點數字)* → N.N 或 N.N.N
  //    單段是普通小數（三點一四 → 3.14）；多段是版本號等（零點一點零 → 0.1.0），每段逐位讀。
  result = result.replace(
    new RegExp(`([${NUM_CHARS}]+)((?:\u70b9[${DIGIT_CHARS}]+)+)`, 'g'),
    (m: string, intp: string, rest: string, offset: number, str: string) => {
      // 護欄：小數尾數若緊跟位值詞/時間詞（說明被截斷，或其實是時間/量詞），不轉，保留中文。
      // 例：九點三十（無「分」）→ 匹配到「九點三」，其後是「十」→ 跳過，寧留中文也不產出 9.3十。
      const after = str[offset + m.length] || ''
      if (after && '十百千\u4e07\u4ebf分\u65f6\u70b9刻秒'.includes(after)) return m
      const intVal = parseChineseInteger(intp)
      if (intVal === null) return m
      const groups = rest.split('\u70b9').filter((g: string) => g.length > 0)
      let out = String(intVal)
      for (const g of groups) {
        let dec = ''
        for (const ch of g) dec += String(CN_DIGITS[ch])
        out += `.${dec}`
      }
      return maybeSpace(str, offset, out)
    },
  )

  // 4. 結構化整數：長度≥2、含位值詞、非黑名單
  result = result.replace(
    new RegExp(`[${NUM_CHARS}]+`, 'g'),
    (m: string, offset: number, str: string) => {
      if (m.length < 2 || !containsUnit(m) || INTEGER_BLACKLIST.has(m)) return m
      const val = parseChineseInteger(m)
      return val === null ? m : maybeSpace(str, offset, String(val))
    },
  )

  return result
}
// i18n-allow-end

/**
 * 還原被 ASR 拆開加空格的「無空格熱詞」（如豆包把熱詞 "SayIt" 識別成 "Say It"）。
 *
 * 僅處理不含空格、純 ASCII 字母/數字的熱詞（中文不會被加空格）。對每個熱詞構造
 * 大小寫敏感、字元間允許空白、兩端帶詞邊界的正則；命中後若與熱詞不同（即確實被
 * 拆開過）就合併還原成熱詞原形。
 *
 * 大小寫敏感 + 詞邊界是為了避免誤傷：普通小寫短語（如句子裡的 "say it"）不會被
 * 改成 "SayIt"，"Say Item" 也不會被錯併成 "SayItem"。
 */
export function restoreHotwordSpacing(text: string, hotwords: string[]): string {
  if (!text || !hotwords || hotwords.length === 0) return text
  const targets = hotwords
    .map((w) => w.trim())
    .filter((w) => w.length >= 2 && /^[A-Za-z0-9]+$/.test(w) && /[A-Za-z]/.test(w))
  if (targets.length === 0) return text
  // 長詞優先，避免短詞先命中影響更長的熱詞
  targets.sort((a, b) => b.length - a.length)
  let result = text
  for (const hw of targets) {
    const inner = hw.split('').join('\\s*')
    // 大小寫不敏感匹配（豆包可能把 "SayIt" 識別成 "Say it"，尾字母小寫），
    // 但要求首字元大小寫與熱詞一致，避免把普通小寫短語（如 "say it"）誤併成熱詞。
    const re = new RegExp(`\\b${inner}\\b`, 'gi')
    result = result.replace(re, (m) => {
      if (m === hw) return m
      // 連寫命中（無內部空格，如 "typeless"/"sayit"）→ 直接還原成熱詞原樣（含大小寫）。
      // 帶空格命中（如 "Say it"）→ 僅當首字母大小寫一致才還原，避免把普通句子裡的
      // 小寫短語（如 "say it"）誤併成熱詞。
      if (/\s/.test(m) && m[0] !== hw[0]) return m
      return hw
    })
  }
  return result
}

// ── 中文標點寬度規範化 ──

/**
 * 半形 → 全形的對應表。
 *
 * **刻意不含句號 `.`**：它的歧義代價太高。小數（3.14）、版本號（v1.2）、域名
 * （example.com）、檔名裡全是半形句點，一旦誤轉就是破壞內容；而實測
 * whisper-large-v3-turbo 的中文句號本來就輸出全形 `。`，不需要我們兜。
 * 寧可漏一個也不能錯改一個。
 */
// i18n-allow-start: 標點符號本身就是這個函式處理的資料
const HALF_TO_FULL_WIDTH: Record<string, string> = {
  ',': '，',
  '?': '？',
  '!': '！',
  ';': '；',
  ':': '：',
}

/** 漢字區間。與 check-i18n.mjs 用的 CJK 判定保持一致。 */
const CJK_CHAR = /[\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff]/
// i18n-allow-end

/**
 * 把緊跟在漢字後面的半形標點轉成全形。
 *
 * 為什麼需要：Whisper（Groq 的 whisper-large-v3-turbo）轉寫中文時**混用兩種寬度** ——
 * 句號給全形 `。`，逗號和問號給半形 `,` `?`。實測四種 prompt 加不給 prompt 輸出
 * 一字不差，所以這是模型自身行為，靠請求引數改不動，只能在文本層歸一。
 *
 * 判據只有一條：**前一個字元是漢字**。這一條把所有危險場景擋在外面 ——
 *   · `3,000` / `3.14`：前一個字元是數字，不動
 *   · `Hello, world`：前一個字元是字母，不動
 *   · `example.com` / `v1.2`：同上，且句號根本不在表裡
 *   · `中文A,B`：逗號前是 `A`，不動
 * 而 `今天不錯,我們走吧` 裡兩個逗號前面都是漢字，轉成全形。
 *
 * 已知缺口（刻意保留）：標點夾在「數字/字母」和漢字之間時不轉，例如
 * `一共100,很便宜` 裡那個逗號會留半形。想覆蓋它就必須放寬到「後一個字元是漢字」，
 * 而那會連 `比例3:2中文`、`3,000元` 一起改掉。少一個全形逗號只是觀感問題，
 * 改壞數字是內容損壞 —— 寧可漏改。
 *
 * 中文供應商（豆包 / 千問 / MiMo）本來就輸出全形，純英文文本裡沒有漢字，
 * 所以對它們這個函式是徹底的空操作 —— 這也是它不需要做成開關的原因：
 * 它不是排版偏好，是把一種明確的錯誤寬度糾正過來。
 */
export function normalizeChinesePunctuation(text: string): string {
  if (!text) return text
  let result = ''
  for (let i = 0; i < text.length; i++) {
    const ch = text[i]
    const full = HALF_TO_FULL_WIDTH[ch]
    // 回看用的是原文的前一個字元：轉換不會改變「是不是漢字」，所以不必看已產出的結果
    result += full && CJK_CHAR.test(text[i - 1] ?? '') ? full : ch
  }
  return result
}

/** 去除每行末尾的標點符號（及尾隨空白）。 */
export function stripTrailingPunctuation(text: string): string {
  if (!text) return text
  return text
    .split('\n')
    .map((line) => line.replace(/[。．.，,、；;：:！!？?…⋯～~—－\s]+$/u, '')) // i18n-allow: 中文標點資料
    .join('\n')
}

/**
 * 將所有標點符號替換為空格，並摺疊多餘空格（保留換行）。
 * 保留數字間的小數點（3.14）、百分號（15%）和斜槓（2/5），避免破壞數字規範化的產物。
 */
export function replacePunctuationWithSpace(text: string): string {
  if (!text) return text
  let result = text.replace(
    /(?<!\d)\.(?!\d)|[，。、；：！？…⋯""''‘’“”（）《》〈〉「」『』【】—－~～·!?,;:"'`()\[\]{}<>_=+|\\@#^&*-]/gu, // i18n-allow: 中文標點資料
    ' ',
  )
  result = result.replace(/[^\S\n]+/g, ' ').replace(/ *\n */g, '\n').trim()
  return result
}

// ── 設定持久化 ──

export interface TextPostProcessOptions {
  /** 智慧分段（僅在關閉 AI 整理的極速模式下生效），預設開啟 */
  autoSegment: boolean
  /** 數字規範化 */
  normalizeNumbers: boolean
  /** 去除句末標點 */
  stripTrailingPunctuation: boolean
  /** 標點符號替換為空格 */
  punctuationToSpace: boolean
}

/**
 * 每一項在「開啟 AI 整理」時是否仍然執行。
 *
 * 這是**唯一的事實來源**：`applyTextTransforms` 的門控和介面的置灰都讀它。
 * 以前生效條件只寫在 applyTextTransforms 裡，介面完全不知道，於是開著 AI 時
 * 四個開關照樣亮著可點、點了什麼都不發生 —— 使用者會認為功能壞了，然後去提一個
 * 我們已經實現過的需求（這就是加這張表的起因）。兩邊讀同一份才不會再騙人。
 *
 * 目前四項都是 false，與改動前的行為完全一致。若將來決定讓「去除句末標點」
 * 「標點替換為空格」穿透 AI（理由：它們是減法，不是和 AI 爭搶的另一種排版，
 * 使用者說不要標點就是不要，跟標點是 ASR 還是 AI 加的無關 —— 和
 * applyTextReplacements 不受門控是同一個道理），只需把這裡改成 true，
 * 介面的置灰範圍會自動跟著變。
 */
export const APPLIES_WITH_AI: Record<keyof TextPostProcessOptions, boolean> = {
  autoSegment: false,
  normalizeNumbers: false,
  stripTrailingPunctuation: false,
  punctuationToSpace: false,
}

const STORAGE_KEY = 'textPostProcess'

export const DEFAULT_POST_PROCESS: TextPostProcessOptions = {
  autoSegment: true,
  normalizeNumbers: true,
  stripTrailingPunctuation: false,
  punctuationToSpace: false,
}

export async function getTextPostProcessOptions(): Promise<TextPostProcessOptions> {
  const saved = await getSetting<Partial<TextPostProcessOptions>>(STORAGE_KEY, {})
  return { ...DEFAULT_POST_PROCESS, ...(saved || {}) }
}

export async function saveTextPostProcessOptions(opts: TextPostProcessOptions): Promise<void> {
  await setSetting(STORAGE_KEY, opts)
}

export interface ApplyTransformsOptions {
  /**
   * 這段文本是不是**未經 AI 整理的純 ASR 結果**。
   *
   * 「格式規範」（智慧分段 / 數字規範化 / 去句末標點 / 標點轉空格）是我們在沒有 AI 時
   * 兜底做的排版。一旦 AI 整理介入，格式就該由 AI 的規則統一負責——兩邊都做會打架
   * （比如 AI 已經排好版，我們又去掉了它的句末句號）。所以這幾項預設只在 rawAsr 時生效，
   * 逐項的例外由 APPLIES_WITH_AI 宣告（介面置灰讀同一張表）。
   *
   * 文本替換不受此開關影響，任何時候都執行：它是使用者點名要替換的固定詞
   * （改公司名、術語糾正等），和"誰來排版"無關，AI 也未必會照做。
   *
   * 預設 true：呼叫方沒說明時按純 ASR 處理（更安全，不會漏掉該做的規範化）。
   */
  rawAsr?: boolean
}

/**
 * 統一入口。順序：中文標點寬度歸一 → 智慧分段 → 數字規範化 → 使用者替換規則
 * → 去句末標點 → 標點轉空格。
 * 其中除「使用者替換規則」外都屬於「格式規範」，預設僅在 rawAsr（無 AI 整理）時執行；
 * 逐項的例外見 APPLIES_WITH_AI。
 */
export async function applyTextTransforms(
  text: string,
  options: ApplyTransformsOptions = {},
): Promise<string> {
  if (!text) return text
  const opts = await getTextPostProcessOptions()
  // AI 整理過的文本，格式交給 AI；我們只保留文本替換。
  const ownFormat = options.rawAsr ?? true
  // 生效判據收在這裡，逐項查 APPLIES_WITH_AI —— 介面置灰讀的是同一張表，
  // 所以"介面說生效"和"實際執行"不可能再走偏。
  const active = (key: keyof TextPostProcessOptions) =>
    opts[key] && (ownFormat || APPLIES_WITH_AI[key])
  let result = text
  // 排在最前面：後面幾步都在讀標點做判斷（分段找句末、去句末標點），
  // 讓它們看到統一寬度的輸入，比讓每一步各自相容兩種寬度可靠。
  // 沒有對應開關：它糾正的是一種明確的錯誤寬度，不是可選的排版偏好。
  if (ownFormat) result = normalizeChinesePunctuation(result)
  if (active('autoSegment')) result = segmentAsrText(result)
  if (active('normalizeNumbers')) result = convertChineseNumbers(result)
  result = await applyTextReplacements(result)
  if (active('stripTrailingPunctuation')) result = stripTrailingPunctuation(result)
  if (active('punctuationToSpace')) result = replacePunctuationWithSpace(result)
  return result
}
