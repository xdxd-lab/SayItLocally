import { describe, expect, it } from 'vitest'

// Rust 原始碼當字串讀進來（Vite 的 `?raw`）。用它而不是 node:fs 是因為本專案沒裝
// @types/node，而 `vite/client` 已經宣告瞭 `*?raw`，不需要額外的型別宣告。
import capabilitiesSource from '../../../src-tauri/src/providers/capabilities.rs?raw'

import {
  expectedClientCap,
  expectedHotwordDelivery,
  foldHotwordDelivery,
  hotwordDependsOnStreamingPath,
  hotwordUndecidedReason,
  willUseStreamingPath,
  type AsrHotwordCapability,
  type HotwordDelivery,
} from '../asrModels'
import { ASR_PROVIDERS, asrModelsOf } from '@/features/settings/asrProviderCatalog'

/**
 * issue #67 的迴歸測試。
 *
 * 被修的缺陷是：介面聲稱熱詞有效，而 6 家雲服務的實現根本沒把熱詞發出去 —— 因為
 * 「哪些家會用熱詞」的宣告在前端一張手寫表格裡，實現在 Rust 各 provider 裡，兩者
 * 無人對賬。所以這裡斷言的不是"某一家支不支援"（那種斷言在出 bug 的版本上也能寫對），
 * 而是**摺疊邏輯不會把缺失的宣告說成確定的答案**，以及**路徑差異被表達了出來**。
 */

function capability(over: Partial<AsrHotwordCapability> = {}): AsrHotwordCapability {
  return {
    streaming: 'context',
    buffered: 'context',
    hasStreamingPath: false,
    streamingClientCap: null,
    bufferedClientCap: null,
    ...over,
  }
}

describe('foldHotwordDelivery', () => {
  it('三種會進請求的傳遞方式都折成「已傳送」', () => {
    for (const delivery of ['vocabulary', 'context', 'instruction'] as HotwordDelivery[]) {
      expect(foldHotwordDelivery(delivery)).toBe('sent')
    }
  })

  it('協議沒位置和我們沒接，對使用者都是「不傳送」', () => {
    expect(foldHotwordDelivery('protocol_has_no_slot')).toBe('not_sent')
    expect(foldHotwordDelivery('not_wired_up')).toBe('not_sent')
  })

  /**
   * 這條是整個檔案裡最要緊的一條。
   *
   * `unknown_provider` 表示 Rust 側沒有宣告這個 provider —— 那是個 bug 訊號。
   * 一旦把它折成 `not_sent`，介面就會平靜地告訴使用者"這家不支援熱詞"，
   * **用一個確定的錯誤答案蓋住一個 bug**，而這正是 issue #67 能藏那麼久的機制。
   */
  it('宣告缺失絕不能顯示成「確定不傳送」', () => {
    expect(foldHotwordDelivery('unknown_provider')).toBe('undecided')
    expect(foldHotwordDelivery('unknown_provider')).not.toBe('not_sent')
    expect(foldHotwordDelivery('unknown_provider')).not.toBe('sent')
  })

  it('auto 協議在探測出來之前也是「未確定」，不能猜', () => {
    expect(foldHotwordDelivery('undecided_protocol')).toBe('undecided')
  })
})

describe('expectedHotwordDelivery', () => {
  it('沒有流式實現的服務只看一次性路徑', () => {
    const cap = capability({ buffered: 'not_wired_up', streaming: 'not_wired_up' })
    expect(expectedHotwordDelivery(cap, {
      streamingDisplayEnabled: true,
      provider: 'groq_whisper',
    })).toBe('not_wired_up')
  })

  /**
   * OpenAI live 的真實形狀：流式走 Realtime 的 keywords，關掉字幕回落到檔案端點後
   * 一條都不傳。這兩個答案必須跟著開關走，否則必有一半的使用者被告知錯誤結論。
   */
  it('OpenAI live：開著字幕傳送，關掉字幕不傳送', () => {
    const cap = capability({
      streaming: 'vocabulary',
      buffered: 'not_wired_up',
      hasStreamingPath: true,
      streamingClientCap: 100,
      bufferedClientCap: null,
    })
    expect(expectedHotwordDelivery(cap, {
      streamingDisplayEnabled: true,
      provider: 'openai_live_transcribe',
    })).toBe('vocabulary')
    expect(expectedHotwordDelivery(cap, {
      streamingDisplayEnabled: false,
      provider: 'openai_live_transcribe',
    })).toBe('not_wired_up')
  })

  /** Gemini live 與上面**方向相反** —— 這是"不能按 provider 一概而論"的現場證據。 */
  it('Gemini live：開著字幕不傳送，關掉字幕反而傳送', () => {
    const cap = capability({
      streaming: 'protocol_has_no_slot',
      buffered: 'instruction',
      hasStreamingPath: true,
    })
    expect(expectedHotwordDelivery(cap, {
      streamingDisplayEnabled: true,
      provider: 'gemini_live_transcribe',
    })).toBe('protocol_has_no_slot')
    expect(expectedHotwordDelivery(cap, {
      streamingDisplayEnabled: false,
      provider: 'gemini_live_transcribe',
    })).toBe('instruction')
  })

  /**
   * 開關開啟不等於會走流式：千問 realtime 缺業務空間 ID 時壓根連不上，會回落。
   * 判據必須複用 isStreamingDisplayReady（執行時唯一的那道閘門），
   * 不能只看 streamingDisplayEnabled。
   */
  it('千問 realtime 缺業務空間 ID 時按回落路徑算', () => {
    // 千問 realtime 兩條路徑本來同檔（都是 context），看不出閘門有沒有生效，
    // 所以這裡刻意用一個兩檔不同的能力去驗：判據必須是 isStreamingDisplayReady，
    // 而不是光看 streamingDisplayEnabled。
    const inverted = capability({
      streaming: 'vocabulary',
      buffered: 'not_wired_up',
      hasStreamingPath: true,
    })
    expect(expectedHotwordDelivery(inverted, {
      streamingDisplayEnabled: true,
      provider: 'qwen_realtime',
      qwenWorkspaceId: '',
    })).toBe('not_wired_up')
    expect(expectedHotwordDelivery(inverted, {
      streamingDisplayEnabled: true,
      provider: 'qwen_realtime',
      qwenWorkspaceId: 'ws-123',
    })).toBe('vocabulary')
  })
})

describe('hotwordDependsOnStreamingPath', () => {
  it('兩條路徑結論相同時不提醒（提醒了是噪音）', () => {
    expect(hotwordDependsOnStreamingPath(capability({
      streaming: 'context',
      buffered: 'context',
      hasStreamingPath: true,
    }))).toBe(false)
    // 傳遞方式不同但對使用者都是"已傳送"，也不該提醒
    expect(hotwordDependsOnStreamingPath(capability({
      streaming: 'vocabulary',
      buffered: 'instruction',
      hasStreamingPath: true,
    }))).toBe(false)
  })

  it('結論會被回落翻轉時必須提醒', () => {
    expect(hotwordDependsOnStreamingPath(capability({
      streaming: 'vocabulary',
      buffered: 'not_wired_up',
      hasStreamingPath: true,
    }))).toBe(true)
    expect(hotwordDependsOnStreamingPath(capability({
      streaming: 'protocol_has_no_slot',
      buffered: 'instruction',
      hasStreamingPath: true,
    }))).toBe(true)
  })

  it('沒有流式路徑的服務不會有這個不確定性', () => {
    expect(hotwordDependsOnStreamingPath(capability({
      streaming: 'not_wired_up',
      buffered: 'not_wired_up',
      hasStreamingPath: false,
    }))).toBe(false)
  })
})

/**
 * 從 Rust 原始碼裡讀出 `capabilities::ALL_ASR_PROVIDERS` 的真實內容。
 *
 * 這裡刻意**不手抄一份**。手抄的問題不是"可能抄錯"，而是抄本和真實宣告之間沒有任何
 * 關係：Rust 那邊加一個 key、刪一個 key，前端這份副本不動，斷言照樣全綠 —— 測出來的
 * 只是"我抄得一致嗎"。讀原始碼之後，兩邊的差異必然顯形。
 *
 * 解析失敗時**拋錯而不是返回空集**：返回空集會讓下面的子集斷言全部平凡透過
 * （空集包含一切），那正是"測試變綠而什麼都沒驗"的經典形狀。
 */
function parseAllAsrProviders(rustSource: string): string[] {
  const block = /ALL_ASR_PROVIDERS:\s*&\[&str\]\s*=\s*&\[([\s\S]*?)\];/.exec(rustSource)
  if (!block) {
    throw new Error(
      '在 capabilities.rs 裡找不到 ALL_ASR_PROVIDERS 的定義 —— 常量被改名或換了寫法，'
      + '這條跨語言斷言已經失效，必須同步改這裡的解析',
    )
  }
  const keys = [...block[1].matchAll(/"([^"]+)"/g)].map((match) => match[1])
  if (keys.length === 0) {
    throw new Error('ALL_ASR_PROVIDERS 解析出 0 個 key，解析邏輯已失效')
  }
  return keys
}

function declaredProvidersInRust(): Set<string> {
  return new Set(parseAllAsrProviders(capabilitiesSource))
}

/**
 * 一段人造的 Rust 宣告，用來驗證**解析器本身**對增刪敏感。
 *
 * 為什麼要這一步：讀真實原始碼只保證"讀到的是真的"，不保證"少一個 key 會被看見"。
 * 而那正是這條跨語言斷言的全部意義 —— 解析器若在某種寫法下靜默漏讀，
 * 下面的子集斷言會平凡透過，漂移照舊發生。
 */
const FAKE_DECLARATION = `
pub const ALL_ASR_PROVIDERS: &[&str] = &[
    "alpha",
    "beta",
    "gamma",
];
`

/**
 * 跨語言的防漂移斷言。
 *
 * 前端目錄里加一張卡 / 一個模型，它的 `provider` 必須在 Rust 的
 * `capabilities::ALL_ASR_PROVIDERS` 裡有宣告；漏了的話 Rust 會返回
 * `unknown_provider`，介面顯示"未確定"—— 不會騙人，但使用者也拿不到答案。
 *
 * 斷言方向是**子集**而不是相等：Rust 那份還含前端目錄裡選不到的內部 key
 * （`openai_chat_audio` 是探測後才分發的）和存量舊 key（`doubao` / `aliyun`）。
 * 「Rust 宣告 ↔ 真實分發」那一側的相等斷言在 Rust 裡（`capabilities.rs` 的
 * `the_declaration_list_matches_the_dispatch_table`，對照掃的是 `cloud_transcribe` 原始碼）。
 */
describe('目錄裡每個模型的執行時 provider 都在 Rust 宣告過熱詞行為', () => {
  const DECLARED_IN_RUST = declaredProvidersInRust()

  it('讀到的宣告清單是真實的、非空的', () => {
    // 解析器活著的訊號。真實清單有十幾個 key，讀到個位數只可能是解析失效。
    expect(DECLARED_IN_RUST.size).toBeGreaterThanOrEqual(10)
    // 錨點：這個 key 只存在於 Rust 的清單裡（前端目錄選不到它），
    // 所以它在場就說明讀的確實是 Rust 那份，不是哪個前端常量。
    expect(DECLARED_IN_RUST.has('openai_chat_audio')).toBe(true)
  })

  it('解析器認得 Rust 的清單寫法', () => {
    expect(parseAllAsrProviders(FAKE_DECLARATION)).toEqual(['alpha', 'beta', 'gamma'])
  })

  /**
   * **從 Rust 清單裡刪掉一個 key，這邊必須立刻少一個。**
   *
   * 這是稽核指出的那個洞在前端這一側：原來 `DECLARED_IN_RUST` 是手抄的，Rust 刪一個
   * key 而副本不動，斷言照樣綠。現在對照來自原始碼，刪除是可見的。
   */
  it('Rust 清單少一個 key，這邊就少一個', () => {
    const shrunk = FAKE_DECLARATION.replace('    "beta",\n', '')
    expect(shrunk).not.toBe(FAKE_DECLARATION)
    expect(parseAllAsrProviders(shrunk)).toEqual(['alpha', 'gamma'])
  })

  /** 常量被改名或換寫法時必須報錯，不能靜默返回空集（空集會讓子集斷言平凡透過）。 */
  it('解析不到時拋錯而不是返回空集', () => {
    expect(() => parseAllAsrProviders('pub const SOMETHING_ELSE: &[&str] = &["x"];')).toThrow()
    expect(() => parseAllAsrProviders('pub const ALL_ASR_PROVIDERS: &[&str] = &[];')).toThrow()
  })

  it('遍歷的是模型上的 provider，不是卡片 id', () => {
    // 卡片 id 是平台（openai / google / openai_compat），真正調 Rust 的是模型的
    // provider（openai_transcribe / openai_live_transcribe…）。按卡片 id 檢查會
    // 漏掉"同一張卡里兩個模型走不同實現"這件事，而那正是 OpenAI 與 Gemini 兩張卡
    // 的形狀 —— 它們卡內就有熱詞行為相反的兩個模型。
    const cardIds = new Set(ASR_PROVIDERS.map((entry) => entry.id))
    const modelProviders = new Set(
      ASR_PROVIDERS.flatMap((entry) => asrModelsOf(entry).map((model) => model.provider)),
    )
    expect(modelProviders.has('openai_transcribe')).toBe(true)
    expect(modelProviders.has('openai_live_transcribe')).toBe(true)
    expect(cardIds.has('openai_transcribe')).toBe(false)
  })

  it('每個模型的 provider 都有宣告', () => {
    for (const entry of ASR_PROVIDERS) {
      for (const model of asrModelsOf(entry)) {
        expect(
          DECLARED_IN_RUST.has(model.provider),
          `${entry.id} / ${model.id} 的 provider "${model.provider}" 在 Rust 的熱詞宣告清單裡找不到`,
        ).toBe(true)
      }
    }
  })
})

/**
 * 條數上限必須跟著**執行路徑**走。
 *
 * 複核時發現的缺陷：上限原來是一個欄位，按流式路徑算出來就一直用著。於是
 * OpenAI Live 配了 100 個以上熱詞、再關掉即時字幕，介面會同時說出
 * 「當前識別服務不使用熱詞」和「最多傳送 100 個，其餘不會發出」—— 兩句話互相矛盾，
 * 而使用者沒法判斷哪句是真的。
 */
describe('expectedClientCap', () => {
  const openaiLive = capability({
    streaming: 'vocabulary',
    buffered: 'not_wired_up',
    hasStreamingPath: true,
    streamingClientCap: 100,
    bufferedClientCap: null,
  })

  it('OpenAI live：開著字幕有上限，關掉字幕不該報上限', () => {
    expect(expectedClientCap(openaiLive, {
      streamingDisplayEnabled: true,
      provider: 'openai_live_transcribe',
    })).toBe(100)
    expect(expectedClientCap(openaiLive, {
      streamingDisplayEnabled: false,
      provider: 'openai_live_transcribe',
    })).toBeNull()
  })

  /** Gemini live 方向相反：上限只存在於關掉字幕後的檔案轉寫路徑上。 */
  it('Gemini live：開著字幕沒有上限，關掉字幕才有', () => {
    const geminiLive = capability({
      streaming: 'protocol_has_no_slot',
      buffered: 'instruction',
      hasStreamingPath: true,
      streamingClientCap: null,
      bufferedClientCap: 100,
    })
    expect(expectedClientCap(geminiLive, {
      streamingDisplayEnabled: true,
      provider: 'gemini_live_transcribe',
    })).toBeNull()
    expect(expectedClientCap(geminiLive, {
      streamingDisplayEnabled: false,
      provider: 'gemini_live_transcribe',
    })).toBe(100)
  })

  /** 上限和 delivery 必須由同一個路徑判斷得出，否則就會出現"不傳送 + 有上限"。 */
  it('上限和 delivery 永遠來自同一條路徑', () => {
    for (const streamingDisplayEnabled of [true, false]) {
      const opts = { streamingDisplayEnabled, provider: 'openai_live_transcribe' }
      const delivery = expectedHotwordDelivery(openaiLive, opts)
      const cap = expectedClientCap(openaiLive, opts)
      // 不進請求的路徑上不能報上限 —— 這是那句自相矛盾文案的判據
      if (delivery === 'not_wired_up' || delivery === 'protocol_has_no_slot') {
        expect(cap).toBeNull()
      }
      expect(willUseStreamingPath(openaiLive, opts)).toBe(streamingDisplayEnabled)
    }
  })

  it('沒有流式實現的服務永遠取一次性路徑那份', () => {
    const gemini = capability({
      streaming: 'instruction',
      buffered: 'instruction',
      hasStreamingPath: false,
      streamingClientCap: 100,
      bufferedClientCap: 100,
    })
    // 開關開著也不走流式（這家沒有流式實現）
    expect(willUseStreamingPath(gemini, {
      streamingDisplayEnabled: true,
      provider: 'gemini_transcribe',
    })).toBe(false)
    expect(expectedClientCap(gemini, {
      streamingDisplayEnabled: true,
      provider: 'gemini_transcribe',
    })).toBe(100)
  })
})

/**
 * 「未確定」的三種成因要能分開。
 *
 * 展示狀態共用是對的，但說明不能共用：原來三種都套「等第一次識別或測試連線之後就能
 * 確定」，而那句只對 auto 協議成立。宣告缺失是 SayIt 的 bug、查詢失敗是命令沒調通，
 * 這兩種重複轉寫永遠不會變 —— 那句話會讓使用者一直等一個不會來的結果。
 */
describe('hotwordUndecidedReason', () => {
  it('auto 協議未探測是唯一能靠"再用一次"解決的那種', () => {
    expect(hotwordUndecidedReason('undecided_protocol')).toBe('protocol')
  })

  it('宣告缺失要單獨指認，不能說成"等下次識別"', () => {
    expect(hotwordUndecidedReason('unknown_provider')).toBe('declaration_missing')
    expect(hotwordUndecidedReason('unknown_provider')).not.toBe('protocol')
  })

  it('確定的那些檔位沒有未確定原因', () => {
    for (const delivery of [
      'vocabulary', 'context', 'instruction', 'not_wired_up', 'protocol_has_no_slot',
    ] as HotwordDelivery[]) {
      expect(hotwordUndecidedReason(delivery)).toBeNull()
    }
  })

  /** 兩個函式對同一批檔位的判斷必須自洽：折成 undecided 的必須給得出原因。 */
  it('凡是折成「未確定」的檔位都能給出原因', () => {
    const all: HotwordDelivery[] = [
      'vocabulary', 'context', 'instruction',
      'protocol_has_no_slot', 'not_wired_up',
      'undecided_protocol', 'unknown_provider',
    ]
    for (const delivery of all) {
      const isUndecided = foldHotwordDelivery(delivery) === 'undecided'
      expect(hotwordUndecidedReason(delivery) !== null).toBe(isUndecided)
    }
  })
})
