import { describe, it, expect } from 'vitest'
import { isStreamingDisplayReady } from '@/lib/asrModels'
import {
  ASR_PLATFORMS,
  ASR_PROVIDERS,
  asrAvailabilityLabel,
  asrCardTitle,
  asrEndpointUrl,
  parseAsrCompatProtocol,
  asrModelsOf,
  describeAsrMissing,
  effectiveAsrCredentials,
  emptyAsrProfile,
  findAsrProvider,
  gradeAsrLatency,
  groupAsrModelsByVendor,

  parseAsrProfiles,
  parseAsrProfilesDetailed,
  providersOfPlatform,
  resolveActiveAsrProfile,
  resolveAsrApiModel,
  resolveAsrModel,
  resolveAsrModelOption,
  resolveAsrRuntimeProvider,
  type AsrModelOption,
  type AsrProfile,
} from '../asrProviderCatalog'

function profile(over: Partial<AsrProfile> = {}): AsrProfile {
  return { ...emptyAsrProfile(), ...over }
}

describe('ASR_PROVIDERS 結構性不變數', () => {
  it('id 唯一', () => {
    const ids = ASR_PROVIDERS.map((p) => p.id)
    expect(new Set(ids).size).toBe(ids.length)
  })

  it('每張卡都有標題、一句定位、至少一個模型', () => {
    for (const p of ASR_PROVIDERS) {
      expect(p.label.trim()).not.toBe('')
      expect(p.blurb.trim()).not.toBe('')
      expect(['mainland_china', 'global']).toContain(p.availability)
      expect(asrModelsOf(p).length).toBeGreaterThan(0)
    }
  })

  /**
   * 一張卡 = 一個平台，而且 id 就等於 platform。
   *
   * 這是這次重構的核心不變數：介面按平台歸攏（使用者認的是「千問」，不是
   * 「qwen_audio_stream」），協議差異下沉到模型的 provider 欄位。
   */
  it('一張卡對應一個平台，id 等於 platform', () => {
    for (const p of ASR_PROVIDERS) {
      expect(p.id).toBe(p.platform)
      expect(providersOfPlatform(p.platform)).toHaveLength(1)
    }
  })

  /**
   * 每個模型都要指明走哪份實現，而且那個 provider 必須是 Rust registry.rs 認識的。
   *
   * 這份清單是**手抄**的對照表 —— 加模型時如果忘了在 registry.rs 的兩處 match 裡
   * 登記，執行時會得到 "ASR provider not implemented"，而那時候使用者已經配好了。
   */
  it('每個模型的 provider 都在 Rust 側登記過', () => {
    const registered = new Set([
      'doubao_v2', 'qwen', 'qwen_realtime', 'qwen_audio_stream', 'qwen_omni', 'mimo',
      'groq_whisper', 'openai_transcribe', 'openai_live_transcribe',
      'gemini_transcribe', 'gemini_live_transcribe', 'openrouter_transcribe',
      // chat/completions + input_audio 那一套（asr_openai_chat_audio.rs）
      'qwen_chat_audio', 'openai_chat_audio', 'openai_chat_audio_standard',
      // /audio/transcriptions、地址由使用者填（asr_groq.rs 的 OPENAI_COMPAT 檔）
      'openai_compat_transcribe',
      // 協議卡的分發層：探測出協議後再轉給上面兩組之一（asr_openai_compat.rs）
      'openai_compat',
    ])
    for (const p of ASR_PROVIDERS) {
      for (const model of asrModelsOf(p)) {
        expect(model.provider.trim()).not.toBe('')
        expect(registered.has(model.provider)).toBe(true)
      }
    }
  })

  /**
   * 千問那張卡是「一張卡多種協議」的樣板。現在是五種實現 ——
   * qwen3.8-omni-flash 走的是非即時的 chat/completions（`qwen_chat_audio`），
   * 與另外幾個 Omni 的 realtime WebSocket 不是一套。
   */
  it('千問一張卡覆蓋它的五種協議實現', () => {
    const providers = asrModelsOf(findAsrProvider('qwen')!).map((m) => m.provider)
    expect(new Set(providers)).toEqual(
      new Set(['qwen_audio_stream', 'qwen', 'qwen_realtime', 'qwen_omni', 'qwen_chat_audio']),
    )
  })

  // 這條斷言變過兩次，記下來免得再繞回去：
  //   v1「每一家都是 mainland_china」—— 只是把當時的事實寫死，加海外供應商時它攔的是新功能；
  //   v2「每種地區都有展示文案」—— 於是七張卡上各掛一句「面向中國大陸帳號」，是純噪音；
  //   現在：國內是預設情形，不給字；只有需要海外帳號/網路的才提醒。
  //   v3 追加：地址由使用者填的「協議卡」也不給字 —— 對面可能是 localhost，
  //   掛一句「需要海外網路」只會誤導。
  it('只有海外供應商才給地區提示，國內的和協議卡都不佔字', () => {
    for (const p of ASR_PROVIDERS) {
      const label = asrAvailabilityLabel(p).trim()
      if (p.customEndpoint) expect(label).toBe('')
      else if (p.availability === 'global') expect(label).not.toBe('')
      else expect(label).toBe('')
    }
    // 至少還有一家是海外的，否則上面那半條斷言等於沒跑
    expect(ASR_PROVIDERS.some((p) => p.availability === 'global')).toBe(true)
  })

  it('平台都在 ASR_PLATFORMS 裡有定義', () => {
    for (const p of ASR_PROVIDERS) expect(ASR_PLATFORMS[p.platform]).toBeTruthy()
  })

  it('模型 id 在卡內唯一、不為空', () => {
    for (const p of ASR_PROVIDERS) {
      const ids = asrModelsOf(p).map((m) => m.id)
      expect(new Set(ids).size).toBe(ids.length)
      for (const id of ids) expect(id.trim()).not.toBe('')
    }
  })

  /**
   * OpenRouter 的 slug 必須帶廠商字首（`openai/whisper-1` 而不是 `whisper-1`）。
   *
   * 少了字首是 404 model not found —— 一個只在真呼叫時才暴露、而且報錯指向
   * 「模型不存在」而不是「你寫錯了格式」的錯誤。目錄是這些 slug 的唯一齣處，
   * 所以在這裡擋住。
   */
  it('OpenRouter 的每個模型 slug 都帶廠商字首', () => {
    const entry = findAsrProvider('openrouter')!
    expect(entry.platform).toBe('openrouter')
    const slugs = asrModelsOf(entry).map((m) => m.id)
    expect(slugs.length).toBeGreaterThan(15)
    for (const slug of slugs) {
      expect(slug).toMatch(/^[a-z0-9-]+\/.+/)
    }
    // 清單是從 /api/v1/models 原樣抄的，抽查幾個「展示名推不出來」的
    expect(slugs).toContain('openai/whisper-large-v3-turbo')
    expect(slugs).toContain('qwen/qwen3-asr-flash-2026-02-10')
    expect(slugs).toContain('x-ai/grok-stt-1.0')
    expect(slugs).toContain('meta/muse-voice-transcribe-1.0')
  })

  it('使用者反饋缺的兩個模型都在清單裡', () => {
    expect(asrModelsOf(findAsrProvider('groq')!).map((m) => m.id)).toContain('whisper-large-v3')
    expect(asrModelsOf(findAsrProvider('openai')!).map((m) => m.id)).toContain('gpt-transcribe')
  })

  /**
   * 模型上標了 streaming 的，必須真的有流式實現（isStreamingDisplayReady 認它）；
   * 反過來也必須成立。
   *
   * 兩邊都查是因為它們**是兩份獨立的清單**：目錄裡的 streaming 只是給人看的標記，
   * 執行時閘門只認 asrModels.ts 那份。只加目錄不加閘門的話，卡片上寫著支援即時字幕、
   * 實際永遠走一次性路徑 —— 不報錯，字幕就是不出來。
   *
   * 判據用**模型的 provider**，因為閘門認的就是它。
   */
  it('模型的 streaming 標記與執行時閘門一致', () => {
    let streamingCount = 0
    for (const p of ASR_PROVIDERS) {
      for (const model of asrModelsOf(p)) {
        const workspace = model.needsWorkspaceId ? 'ws-test' : undefined
        expect(isStreamingDisplayReady(model.provider, workspace)).toBe(Boolean(model.streaming))
        if (model.streaming) streamingCount += 1
      }
    }
    // 至少有一個標了 streaming，否則上面那條等於沒跑
    expect(streamingCount).toBeGreaterThan(0)
  })

  /**
   * 「檔案轉寫」與「即時字幕」現在是同一張卡里的兩個模型，各走各的實現。
   *
   * 流式模型在檔案端點上根本不存在（gpt-live-transcribe 之於 /audio/transcriptions、
   * gemini-3.5-transcribe-live 之於 generateContent），所以關掉即時字幕回落時
   * Rust 側會把模型名換掉（asr_groq 的 honors_selected_model、asr_gemini 的
   * LIVE_PROVIDER 判斷）。這條釘住兩者確實是不同的模型 + 不同的實現。
   */
  it('每家的檔案模型與流式模型各走各的實現', () => {
    for (const cardId of ['openai', 'google']) {
      const models = asrModelsOf(findAsrProvider(cardId)!)
      const file = models.find((m) => !m.streaming)!
      const live = models.find((m) => m.streaming)!
      expect(file.id).not.toBe(live.id)
      expect(file.provider).not.toBe(live.provider)
    }
  })

  /**
   * 卡片標題不該再出現「（qwen3.5-omni-plus，ASR+AI）」這種**模型名括號串** ——
   * 模型名有自己的一行。
   *
   * 判據釘的是「括號裡不能是模型名」，不是「不許有括號」：兩張協議卡的名字裡
   * 「（對話式）」是在區分兩套同名協議，那是標題必須承擔的資訊。
   */
  it('標題的括號裡不出現模型名', () => {
    const allModelIds = ASR_PROVIDERS.flatMap((p) => asrModelsOf(p)).map((m) => m.id)
    for (const p of ASR_PROVIDERS) {
      for (const id of allModelIds) {
        expect(p.label).not.toContain(id)
      }
    }
  })

  /** 卡片順序就是推薦順序，前兩名釘住 */
  it('推薦順序：豆包第一，千問第二', () => {
    expect(ASR_PROVIDERS.slice(0, 2).map((p) => p.id)).toEqual(['doubao', 'qwen'])
  })

  /**
   * 只有 qwen3-asr-flash-realtime 需要業務空間 ID。
   * Qwen-Audio-ASR-Flash-Streaming 的 3.1 與 3.0 都用通用域名就能跑（實測），
   * 照抄 needsWorkspaceId 會讓使用者以為不填就用不了 —— 這條釘住這個區別。
   */
  it('只有 qwen_realtime 那個模型需要業務空間 ID', () => {
    const needing = ASR_PROVIDERS.flatMap((p) => asrModelsOf(p))
      .filter((m) => m.needsWorkspaceId)
      .map((m) => m.id)
    expect(needing).toEqual(['qwen3-asr-flash-realtime'])
  })

  /**
   * 走 realtime 那條路的 Omni：**發出去的模型名必須帶 `-realtime`**。
   *
   * 斷言的是 resolveAsrApiModel 的結果，不是「每個都得有 apiModel」——
   * 後者曾經成立（那時產品名都不帶字尾，靠 apiModel 補），但
   * qwen3.8-omni-flash-realtime 的產品名本身就帶，給它配 apiModel 只會得到
   * `...-realtime-realtime`。把斷言釘在「實際發什麼」上，兩種情形都覆蓋，
   * 也不會因為下次加了個本來就帶字尾的模型而紅。
   */
  it('走 realtime 的 Omni，發出去的模型名都帶 -realtime', () => {
    const realtimeOmni = asrModelsOf(findAsrProvider('qwen')!)
      .filter((m) => m.omni && m.provider === 'qwen_omni')
    expect(realtimeOmni.map((m) => m.id)).toEqual([
      'qwen3.8-omni-flash-realtime', 'qwen3.5-omni-plus', 'qwen3.5-omni-flash',
    ])
    for (const model of realtimeOmni) {
      const sent = resolveAsrApiModel(profile({ provider: 'qwen', model: model.id }))
      expect(sent).toMatch(/-realtime$/)
      // 字尾只能有一份
      expect(sent.endsWith('-realtime-realtime')).toBe(false)
    }
  })

  /**
   * 3.8 那兩條是**同一代的兩條協議**，不是新舊關係。這條釘住它們各走各的實現。
   *
   * ⚠️ 這裡的結論被推翻過兩次，別按舊理由改回去：
   *  1. 最早認為「服務端沒有 qwen3.8-omni-flash-realtime」——
   *     2026-09-23 複測它存在了；
   *  2. 接著認為「它在我們的程式碼路徑上過不去」（`response.create` 時報
   *     `Voice 'Chelsie' is not supported`）—— 這個現象是真的，但**結論錯了**：
   *     當時的對照實驗裡"帶 voice"和"不發 response.create"兩個變數一起變了。
   *     固定發 response.create、只變音色欄位重測後，加一個
   *     `session.audio.output.voice` 就能用（3.5 兩代也接受同一個欄位）。
   *     那個欄位現在由 asr_qwen_omni.rs 對所有 Omni 統一傳送。
   */
  it('3.8 的兩個 Omni 各走各的協議', () => {
    const models = asrModelsOf(findAsrProvider('qwen')!)

    const http = models.find((m) => m.id === 'qwen3.8-omni-flash')
    expect(http).toBeTruthy()
    expect(http!.provider).toBe('qwen_chat_audio')
    expect(http!.apiModel).toBeUndefined()
    expect(http!.omni).toBe(true)
    // 它沒有即時字幕能力，別順手標上 streaming
    expect(http!.streaming).toBeUndefined()
    // HTTP 那條路才允許改地址
    expect(http!.supportsCustomUrl).toBe(true)

    const ws = models.find((m) => m.id === 'qwen3.8-omni-flash-realtime')
    expect(ws).toBeTruthy()
    expect(ws!.provider).toBe('qwen_omni')
    // 產品名已經帶 -realtime，補 apiModel 會變成 -realtime-realtime
    expect(ws!.apiModel).toBeUndefined()
    expect(ws!.omni).toBe(true)
    expect(ws!.streaming).toBeUndefined()
    // realtime WebSocket 改地址沒有意義，而且那幾份實現壓根不讀 extra.baseUrl
    expect(ws!.supportsCustomUrl).toBeUndefined()
    expect(asrEndpointUrl(profile({
      provider: 'qwen', model: 'qwen3.8-omni-flash-realtime', apiUrl: 'https://evil.example/v1',
    }))).toBe('')
  })

  /**
   * 「協議卡」的不變數：必須有地址佔位示例、必須只給一個預設模型。
   *
   * 佔位示例是這類卡唯一的填寫線索（我們說不出對面是誰），少了它使用者只能猜。
   */
  it('協議卡只有一張，有地址佔位示例，且地址必填', () => {
    const custom = ASR_PROVIDERS.filter((p) => p.customEndpoint)
    // 刻意只留一張：曾經按協議分成兩張讓使用者自己選，但使用者沒辦法知道對面說哪種協議。
    // 合併之後協議由 Rust 側探測（asr_openai_compat.rs）。
    expect(custom.map((p) => p.id)).toEqual(['openai_compat'])
    for (const p of custom) {
      expect(p.urlPlaceholder?.trim()).toBeTruthy()
      expect(asrModelsOf(p)).toHaveLength(1)
      for (const model of asrModelsOf(p)) {
        expect(model.supportsCustomUrl).toBe(true)
        expect(model.requiresCustomUrl).toBe(true)
      }
    }
  })

  /**
   * 「能不能改地址」是**協議**的屬性，所以必須掛在模型上、不能掛在卡片上。
   *
   * 千問那張卡就是反例的來源：`qwen3.8-omni-flash` 是 HTTP（能改），
   * `qwen3.5-omni-plus` 是 realtime WebSocket（改不了）。掛在卡片上表達不了這種混合，
   * 而弄錯的後果是「介面上讓我填了地址，實際發的還是官方地址」——靜默不一致。
   */
  it('只有 HTTP 協議的模型才允許改地址，WebSocket 的一律不給', () => {
    // 這幾個 provider 走 WebSocket，地址改不了
    const websocketProviders = new Set([
      'doubao_v2', 'qwen_audio_stream', 'qwen_realtime', 'qwen_omni',
      'openai_live_transcribe', 'gemini_live_transcribe',
    ])
    for (const entry of ASR_PROVIDERS) {
      for (const model of asrModelsOf(entry)) {
        if (websocketProviders.has(model.provider)) {
          expect(model.supportsCustomUrl).toBeFalsy()
        }
      }
    }
    // 而 HTTP 那些必須開著，否則用中轉站的使用者接不上
    const qwenModels = asrModelsOf(findAsrProvider('qwen')!)
    expect(qwenModels.find((m) => m.id === 'qwen3.8-omni-flash')!.supportsCustomUrl).toBe(true)
    expect(qwenModels.find((m) => m.id === 'qwen3.5-omni-plus')!.supportsCustomUrl).toBeFalsy()
    for (const id of ['gpt-transcribe', 'whisper-1']) {
      expect(asrModelsOf(findAsrProvider('openai')!).find((m) => m.id === id)!.supportsCustomUrl)
        .toBe(true)
    }
    expect(asrModelsOf(findAsrProvider('openai')!)
      .find((m) => m.id === 'gpt-live-transcribe')!.supportsCustomUrl).toBeFalsy()
  })

  /** requiresCustomUrl 只能出現在協議卡上：內建卡的地址是可選覆蓋，不是必填。 */
  it('內建卡的地址是可選的，不是必填', () => {
    for (const entry of ASR_PROVIDERS) {
      if (entry.customEndpoint) continue
      for (const model of asrModelsOf(entry)) {
        expect(model.requiresCustomUrl).toBeFalsy()
      }
    }
  })

  /**
   * 模型下拉里必須能看到**真實 ID**，不能只有中文短名。
   *
   * 短名說明這是什麼，真實 ID 才是能拿去對服務商文件、能在日誌和帳單裡搜到的
   * 那個東西 —— 只給短名的話使用者根本對不上號。
   */
  /**
   * 每個模型的顯示名就是它的真實 ID —— 清單裡**不允許**再出現中文短名。
   *
   * 這條釘的是一次真實的困惑：短名本來是「一張卡 = 一個模型」時代的卡片標題
   * （「千問 ASR 一次性」靠「一次性」跟流式那張卡對比），歸攏成一卡一平台之後它們
   * 成了卡內的模型名，使用者在「模型」那一欄看到的就是一個不像模型的詞。
   * 真實 ID 既一眼認得出，又能拿去對服務商文件和帳單。
   */
  it('模型清單裡只用真實模型 ID，不帶中文短名', () => {
    for (const entry of ASR_PROVIDERS) {
      for (const option of asrModelsOf(entry)) {
        expect(option.id.trim()).not.toBe('')
        // 真實模型 ID 不含中日韓字元；出現了就說明有人又把短名寫回來了
        expect(option.id).not.toMatch(/[\u4e00-\u9fff]/)
      }
    }
  })

  it('findAsrProvider 查得到也查不崩', () => {
    expect(findAsrProvider('doubao')?.platform).toBe('doubao')
    expect(findAsrProvider('nope')).toBeUndefined()
  })
})

describe('groupAsrModelsByVendor', () => {
  const opt = (id: string): AsrModelOption => ({ id, provider: 'openrouter_transcribe' })

  it('帶廠商字首時按廠商分組，組內保持原順序', () => {
    const groups = groupAsrModelsByVendor(
      ['openai/gpt-transcribe', 'qwen/a', 'qwen/b', 'openai/whisper-1'].map(opt),
    )!
    // openai 組排第一，因為預設模型在裡面；同組的 whisper-1 歸併進來
    expect(groups.map(([vendor, models]) => [vendor, models.map((m) => m.id)])).toEqual([
      ['openai', ['openai/gpt-transcribe', 'openai/whisper-1']],
      ['qwen', ['qwen/a', 'qwen/b']],
    ])
  })

  /**
   * 不帶字首的一律不分組。直連那幾張卡的模型名是 `whisper-large-v3` 這種裸名，
   * 硬分組會得到一個名叫空串的 optgroup。
   */
  it('模型名不帶字首、或只有一個時不分組', () => {
    expect(groupAsrModelsByVendor(['whisper-large-v3-turbo', 'whisper-large-v3'].map(opt))).toBeNull()
    expect(groupAsrModelsByVendor(['gpt-transcribe', 'openai/whisper-1'].map(opt))).toBeNull()
    expect(groupAsrModelsByVendor([opt('openai/gpt-transcribe')])).toBeNull()
    expect(groupAsrModelsByVendor([])).toBeNull()
  })

  it('目錄裡只有 OpenRouter 那張卡會分組', () => {
    const grouped = ASR_PROVIDERS
      .filter((p) => groupAsrModelsByVendor(asrModelsOf(p)) !== null)
      .map((p) => p.id)
    expect(grouped).toEqual(['openrouter'])
  })

  /** 預設模型所在的組必須排在最前面，否則下拉開啟時要往下翻才看到推薦項 */
  it('預設模型所在的組排最前', () => {
    const models = asrModelsOf(findAsrProvider('openrouter')!)
    const groups = groupAsrModelsByVendor(models)!
    expect(groups[0][1][0].id).toBe(models[0].id)
  })
})

describe('parseAsrProfiles 容錯', () => {
  it('非陣列一律當空列表', () => {
    expect(parseAsrProfiles(null)).toEqual([])
    expect(parseAsrProfiles({})).toEqual([])
    expect(parseAsrProfiles('x')).toEqual([])
  })

  it('丟掉供應商不在內建清單裡的條目', () => {
    // 留著只會渲染出一張點不動的卡
    const r = parseAsrProfiles([
      { id: 'a', provider: 'doubao_v2' },
      { id: 'b', provider: 'gpt-4-asr' },
      { id: 'c' },
    ])
    expect(r.map((p) => p.id)).toEqual(['a'])
  })

  it('丟掉重複 id', () => {
    const r = parseAsrProfiles([
      { id: 'a', provider: 'doubao_v2' },
      { id: 'a', provider: 'qwen' },
    ])
    expect(r).toHaveLength(1)
  })

  it('缺欄位補成空串，不會渲染出 undefined', () => {
    const [p] = parseAsrProfiles([{ id: 'a', provider: 'qwen' }])
    expect(p.apiKey).toBe('')
    expect(p.appId).toBe('')
    expect(p.workspaceId).toBe('')
    expect(p.omniPrompt).toBe('')
    expect(p.console).toBe('new')
    // 老配置沒有 model 欄位，會按 provider 遷移出對應的那個模型
    expect(p.provider).toBe('qwen')
    expect(p.model).toBe('qwen3-asr-flash')
  })

  /**
   * **存量配置的遷移是這次重構最要緊的一條。**
   *
   * 改成「一張卡 = 一個平台」後，老 profile 裡 provider 存的還是舊分發 key
   * （`qwen_audio_stream`、`gemini_live_transcribe`…），那些值不再是任何卡片的 id。
   * 沒有遷移，parseAsrProfiles 會把它們整條丟掉 —— 使用者的服務配置連金鑰一起消失。
   */
  it('存量條目按舊 provider 遷移成（卡片, 模型），金鑰不丟', () => {
    const legacy = [
      { id: 'a', provider: 'qwen_audio_stream', apiKey: 'sk-qwen' },
      { id: 'b', provider: 'doubao_v2', apiKey: 'KEY', appId: '123', console: 'legacy' },
      { id: 'c', provider: 'gemini_live_transcribe', apiKey: 'AIza' },
      { id: 'd', provider: 'openai_transcribe', apiKey: 'sk-oa' },
      { id: 'e', provider: 'groq_whisper', apiKey: 'gsk' },
      { id: 'f', provider: 'openrouter_transcribe', apiKey: 'sk-or' },
      // 更早的別名也要認
      { id: 'g', provider: 'aliyun', apiKey: 'sk-old' },
    ]
    const out = parseAsrProfiles(legacy)
    expect(out.map((p) => [p.provider, p.model])).toEqual([
      ['qwen', 'qwen-audio-3.0-asr-flash-streaming'],
      ['doubao', 'Doubao-Seed-ASR-2.0'],
      ['google', 'gemini-3.5-transcribe-live'],
      ['openai', 'gpt-transcribe'],
      ['groq', 'whisper-large-v3-turbo'],
      ['openrouter', 'openai/gpt-transcribe'],
      ['qwen', 'qwen3-asr-flash'],
    ])
    // 金鑰與豆包那套代次欄位都要原樣帶過來
    expect(out[0].apiKey).toBe('sk-qwen')
    expect(out[1].appId).toBe('123')
    expect(out[1].console).toBe('legacy')
  })

  /** 已經是新格式的條目（帶 model）不該被遷移邏輯改動 */
  it('新格式條目原樣保留', () => {
    const out = parseAsrProfiles([
      { id: 'a', provider: 'qwen', model: 'qwen3.5-omni-flash', apiKey: 'k' },
    ])
    expect(out[0].provider).toBe('qwen')
    expect(out[0].model).toBe('qwen3.5-omni-flash')
  })

  /**
   * **這條是那次"卡片全沒了"的迴歸測試。**
   *
   * `model` 欄位是上一個版本就加進來的（那時它表示"同實現下選哪個模型"），所以存量裡
   * `provider='gemini_transcribe'` + `model='gemini-3.5-transcribe'` 是完全正常的組合。
   * 遷移判據一度寫成「有 model 就是新資料」，於是這類條目被判成新格式、
   * `findAsrProvider('gemini_transcribe')` 返回 undefined、整條被丟掉 ——
   * 使用者開啟設定頁發現自己配的卡連金鑰一起消失了。
   */
  it('舊 provider 配著舊 model 的條目不能被丟掉', () => {
    const out = parseAsrProfiles([
      { id: 'a', provider: 'gemini_transcribe', model: 'gemini-3.5-transcribe', apiKey: 'AIza' },
      { id: 'b', provider: 'openai_live_transcribe', model: 'gpt-live-transcribe', apiKey: 'sk' },
      { id: 'c', provider: 'openrouter_transcribe', model: 'openai/gpt-transcribe', apiKey: 'or' },
    ])
    expect(out).toHaveLength(3)
    expect(out.map((p) => [p.provider, p.model])).toEqual([
      ['google', 'gemini-3.5-transcribe'],
      ['openai', 'gpt-live-transcribe'],
      ['openrouter', 'openai/gpt-transcribe'],
    ])
    expect(out[0].apiKey).toBe('AIza')
  })

  /** 使用者在舊卡上選過的模型，只要新卡也有就保留 —— 不該被打回預設 */
  it('遷移時保留使用者選過的模型', () => {
    const out = parseAsrProfiles([
      { id: 'a', provider: 'groq_whisper', model: 'whisper-large-v3', apiKey: 'gsk' },
      { id: 'b', provider: 'openai_transcribe', model: 'whisper-1', apiKey: 'sk' },
    ])
    expect(out.map((p) => [p.provider, p.model])).toEqual([
      ['groq', 'whisper-large-v3'],
      ['openai', 'whisper-1'],
    ])
  })

  /**
   * 上一代 Omni 的舊 id 也要能落下來。
   *
   * 它們指向的模型本身已經從目錄裡刪掉（被百鍊縮容），但這些卡**不能**丟，也不能
   * 落到卡片預設那個純 ASR 模型上 —— 那會把使用者從「不用配 AI 服務」悄悄搬到
   * 「必須另配 AI 服務」。所以兩個都落到同族還活著的 qwen3.5-omni-flash。
   */
  it('上一代 Omni 的舊 provider 遷到同族還活著的模型', () => {
    const out = parseAsrProfiles([
      { id: 'a', provider: 'qwen_omni_turbo', apiKey: 'k' },
      { id: 'b', provider: 'qwen_omni_flash', apiKey: 'k' },
      { id: 'c', provider: 'qwen_omni_plus', apiKey: 'k' },
    ])
    expect(out.map((p) => [p.provider, p.model])).toEqual([
      ['qwen', 'qwen3.5-omni-flash'],
      ['qwen', 'qwen3.5-omni-flash'],
      ['qwen', 'qwen3.5-omni-plus'],
    ])
    // 遷完必須還是 Omni（識別+整理一體），否則這條遷移就沒有意義
    for (const profile of out) {
      expect(resolveAsrModelOption(profile)!.omni).toBe(true)
    }
  })

  /**
   * 已經是「新資料形態」的退役模型也要接住。
   *
   * 這是上一條覆蓋不到的另一半：`provider` 已經遷成卡片 id `qwen`、`model` 裡存著
   * 退役的模型名。這種條目走的是 migrateLegacyProvider 的回落分支，而回落到卡片
   * 預設會得到一個**純 ASR** 模型 —— Omni 使用者的整理能力會無聲消失。
   */
  it('新資料形態裡的退役 Omni 也落到同族替代', () => {
    const out = parseAsrProfiles([
      { id: 'a', provider: 'qwen', model: 'qwen3-omni-flash', apiKey: 'k' },
      { id: 'b', provider: 'qwen', model: 'qwen-omni-turbo', apiKey: 'k' },
    ])
    expect(out.map((p) => p.model)).toEqual([
      'qwen3.5-omni-flash', 'qwen3.5-omni-flash',
    ])
    for (const profile of out) {
      expect(resolveAsrModelOption(profile)!.omni).toBe(true)
    }
  })

  /**
   * 認不出的模型名不丟條目，但**落點取決於 provider 值走的是哪條分支**。
   *
   * `qwen` 同時是舊分發 key 和卡片 id，所以它先命中 LEGACY_PROVIDERS，落到那張表
   * 指定的 qwen3-asr-flash，而不是卡片第一項（3.1 流式）。這不是 bug：對存量資料
   * 來說「provider 是 qwen」本來就等於「用 qwen3-asr-flash 那個模型」。
   *
   * 這條釘在這裡是因為它反過來解釋了 RETIRED_MODELS 為什麼必須在所有分支之前
   * 執行 —— 放在後面就會被這條 LEGACY 分支搶先返回。
   */
  it('認不出的模型名不丟條目，落到 LEGACY 表指定的那個', () => {
    const out = parseAsrProfiles([
      { id: 'a', provider: 'qwen', model: 'qwen-something-that-never-existed', apiKey: 'k' },
    ])
    expect(out.map((p) => p.model)).toEqual(['qwen3-asr-flash'])
  })

  /** 純卡片 id（不在 LEGACY 表裡）配認不出的模型名，才走「回落到卡片預設」 */
  it('不在遷移表裡的卡片 id 才回落到卡片預設', () => {
    const out = parseAsrProfiles([
      { id: 'a', provider: 'openrouter', model: 'nope/does-not-exist', apiKey: 'k' },
    ])
    expect(out.map((p) => p.model)).toEqual(['openai/gpt-transcribe'])
  })

  /** 真正認不出來的不進列表：provider 既不是卡片 id 也不在遷移表裡 */
  it('完全認不出的 provider 不進列表', () => {
    const out = parseAsrProfiles([
      { id: 'a', provider: 'some_removed_vendor', model: 'x', apiKey: 'k' },
      { id: 'b', provider: 'qwen', model: 'qwen3-asr-flash', apiKey: 'k' },
    ])
    expect(out.map((p) => p.id)).toEqual(['b'])
  })

  /**
   * **但它不能從磁碟上消失。**
   *
   * 這是那次資料損失的第二道防線：解析不出來的條目要原樣交回給呼叫方，
   * 由 saveAsrProfiles 拼回儲存。否則「解析失敗」會在下一次歸一化寫回時
   * 變成「永久刪除」—— 使用者丟的是配置連帶金鑰，而金鑰沒有第二份。
   */
  it('解析不出來的條目原樣交回，不丟', () => {
    const raw = [
      { id: 'a', provider: 'some_removed_vendor', model: 'x', apiKey: 'secret1' },
      { id: 'b', provider: 'qwen', model: 'qwen3-asr-flash', apiKey: 'k' },
      'not an object',
      { id: 'b', provider: 'qwen', model: 'qwen3-asr-flash' }, // 重复 id
    ]
    const { profiles, orphans } = parseAsrProfilesDetailed(raw)
    expect(profiles.map((p) => p.id)).toEqual(['b'])
    // 三種落選原因都要留痕：認不出的 provider、非物件、重複 id
    expect(orphans).toHaveLength(3)
    expect(orphans[0]).toBe(raw[0])
    expect(orphans[1]).toBe(raw[2])
    expect(orphans[2]).toBe(raw[3])
  })

  it('全都能解析時 orphans 為空', () => {
    const { orphans } = parseAsrProfilesDetailed([
      { id: 'a', provider: 'qwen', model: 'qwen3-asr-flash' },
      { id: 'b', provider: 'gemini_transcribe', model: 'gemini-3.5-transcribe' },
    ])
    expect(orphans).toEqual([])
  })

  /** provider 是卡片 id 但 model 不認識（新建沒選 / 模型下線）→ 回落該卡預設 */
  it('模型名不認識時回落到該卡預設，條目不丟', () => {
    const out = parseAsrProfiles([
      { id: 'a', provider: 'google', model: 'gemini-9-nonexistent', apiKey: 'k' },
    ])
    expect(out).toHaveLength(1)
    expect(out[0].model).toBe('gemini-3.5-transcribe')
  })

  it('console 只認 new / legacy，其它值回落到 new', () => {
    expect(parseAsrProfiles([{ id: 'a', provider: 'doubao_v2', console: 'legacy' }])[0].console).toBe('legacy')
    expect(parseAsrProfiles([{ id: 'a', provider: 'doubao_v2', console: 'garbage' }])[0].console).toBe('new')
  })

  it('壞的檢測結論被丟掉，好的保留', () => {
    const r = parseAsrProfiles([
      { id: 'a', provider: 'qwen', check: { ok: true, at: 100, latencyMs: 800, audioSec: 3 } },
      { id: 'b', provider: 'qwen', check: { ok: true } },
      { id: 'c', provider: 'qwen', check: 'nope' },
    ])
    expect(r[0].check).toEqual({ ok: true, at: 100, latencyMs: 800, audioSec: 3, reason: undefined })
    expect(r[1].check).toBeUndefined()
    expect(r[2].check).toBeUndefined()
  })

  it('沒有 id 時自動補一個，不會整條丟掉', () => {
    const r = parseAsrProfiles([{ provider: 'qwen', apiKey: 'sk-1' }])
    expect(r).toHaveLength(1)
    expect(r[0].id).not.toBe('')
  })
})

describe('resolveActiveAsrProfile', () => {
  it('指向不存在的 id 時回落到第一條', () => {
    const list = [profile({ id: 'a' }), profile({ id: 'b' })]
    expect(resolveActiveAsrProfile(list, 'zzz')?.id).toBe('a')
    expect(resolveActiveAsrProfile(list, 'b')?.id).toBe('b')
  })

  it('空列表返回 null', () => {
    expect(resolveActiveAsrProfile([], 'a')).toBeNull()
  })
})

describe('resolveAsrModel', () => {
  it('沒選過就用該卡的預設模型', () => {
    expect(resolveAsrModel(profile({ provider: 'groq', model: '' }))).toBe('whisper-large-v3-turbo')
    expect(resolveAsrModel(profile({ provider: 'groq', model: '  ' }))).toBe('whisper-large-v3-turbo')
    expect(resolveAsrModel(profile({ provider: 'openai', model: '' }))).toBe('gpt-transcribe')
  })

  it('選過就用選的那個', () => {
    expect(resolveAsrModel(profile({ provider: 'groq', model: 'whisper-large-v3' })))
      .toBe('whisper-large-v3')
    expect(resolveAsrModel(profile({ provider: 'openai', model: 'whisper-1' })))
      .toBe('whisper-1')
  })

  /**
   * 不在清單裡的一律回落，不原樣發出去。
   *
   * 兩種來路：服務商下線了某個模型（他們淘汰模型比我們改程式碼勤），
   * 或者使用者先在 Groq 卡上選了 whisper-large-v3、再把供應商換成 OpenAI。
   * 照發只會換來一個含義模糊的 400，而使用者完全不知道自己發了什麼。
   */
  it('模型不屬於這張卡時回落到預設', () => {
    expect(resolveAsrModel(profile({ provider: 'openai', model: 'whisper-large-v3' })))
      .toBe('gpt-transcribe')
    expect(resolveAsrModel(profile({ provider: 'groq', model: 'gpt-transcribe' })))
      .toBe('whisper-large-v3-turbo')
    expect(resolveAsrModel(profile({ provider: 'doubao', model: 'whatever' })))
      .toBe('Doubao-Seed-ASR-2.0')
  })
})

/**
 * 執行時那兩個鍵 —— **這是卡片 id 與後端實現分離之後最容易搞錯的地方。**
 *
 * `cloudAsr.provider` 必須是選中模型的 provider（Rust 的分發 key），不是卡片 id；
 * 寫成卡片 id 的話 Rust 會報 "ASR provider not implemented"。
 * `cloudAsr.model` 必須是 apiModel（Omni 那兩個帶 -realtime 字尾）。
 */
describe('執行時鍵解析', () => {
  it('provider 取選中模型的實現，不是卡片 id', () => {
    expect(resolveAsrRuntimeProvider(profile({ provider: 'qwen', model: 'qwen3-asr-flash' })))
      .toBe('qwen')
    expect(resolveAsrRuntimeProvider(
      profile({ provider: 'qwen', model: 'qwen-audio-3.0-asr-flash-streaming' }),
    )).toBe('qwen_audio_stream')
    expect(resolveAsrRuntimeProvider(
      profile({ provider: 'qwen', model: 'qwen3-asr-flash-realtime' }),
    )).toBe('qwen_realtime')
    expect(resolveAsrRuntimeProvider(profile({ provider: 'qwen', model: 'qwen3.5-omni-plus' })))
      .toBe('qwen_omni')
    // 同一張 OpenAI 卡，兩個模型走兩份實現
    expect(resolveAsrRuntimeProvider(profile({ provider: 'openai', model: 'gpt-transcribe' })))
      .toBe('openai_transcribe')
    expect(resolveAsrRuntimeProvider(profile({ provider: 'openai', model: 'gpt-live-transcribe' })))
      .toBe('openai_live_transcribe')
  })

  it('model 取 apiModel —— Omni 的介面名帶 -realtime 字尾', () => {
    expect(resolveAsrApiModel(profile({ provider: 'qwen', model: 'qwen3.5-omni-plus' })))
      .toBe('qwen3.5-omni-plus-realtime')
    expect(resolveAsrApiModel(profile({ provider: 'qwen', model: 'qwen3.5-omni-flash' })))
      .toBe('qwen3.5-omni-flash-realtime')
    // 沒有 apiModel 的就用 id 本身
    expect(resolveAsrApiModel(profile({ provider: 'groq', model: 'whisper-large-v3' })))
      .toBe('whisper-large-v3')
  })

  it('卡片不存在時給空串，不拋', () => {
    expect(resolveAsrRuntimeProvider(profile({ provider: 'nope', model: 'x' }))).toBe('')
    expect(resolveAsrApiModel(profile({ provider: 'nope', model: 'x' }))).toBe('')
  })
})

describe('effectiveAsrCredentials', () => {
  // 核心不變數：Rust 側靠 app_id 空不空來選兩代鑑權頭
  it('豆包新版控制台必須把 appId 清空', () => {
    const p = profile({
      provider: 'doubao', console: 'new', apiKey: 'APPKEY', otherKey: 'TOKEN', appId: '1234567890',
    })
    expect(effectiveAsrCredentials(p)).toEqual({ apiKey: 'APPKEY', appId: '' })
  })

  it('豆包舊版控制台用 Access Token + App ID', () => {
    const p = profile({
      provider: 'doubao', console: 'legacy', apiKey: 'TOKEN', otherKey: 'APPKEY', appId: '1234567890',
    })
    expect(effectiveAsrCredentials(p)).toEqual({ apiKey: 'TOKEN', appId: '1234567890' })
  })

  it('非豆包平台不帶 appId', () => {
    const p = profile({ provider: 'qwen', apiKey: ' sk-1 ', appId: '999' })
    expect(effectiveAsrCredentials(p)).toEqual({ apiKey: 'sk-1', appId: '' })
  })
})

describe('describeAsrMissing', () => {
  it('豆包新版只要金鑰', () => {
    expect(describeAsrMissing(profile({ provider: 'doubao', console: 'new' }))).toBe('還沒填 API Key')
    expect(describeAsrMissing(profile({ provider: 'doubao', console: 'new', apiKey: 'k' }))).toBe('')
  })

  it('豆包舊版兩個都要，且先報金鑰', () => {
    expect(describeAsrMissing(profile({ provider: 'doubao', console: 'legacy' }))).toBe('還沒填 Access Token')
    expect(describeAsrMissing(profile({ provider: 'doubao', console: 'legacy', apiKey: 't' }))).toBe('還沒填 App ID')
    expect(describeAsrMissing(profile({ provider: 'doubao', console: 'legacy', apiKey: 't', appId: '1' }))).toBe('')
  })

  it('其它平台只要 API Key', () => {
    expect(describeAsrMissing(profile({ provider: 'mimo' }))).toBe('還沒填 API Key')
    expect(describeAsrMissing(profile({ provider: 'mimo', apiKey: 'k' }))).toBe('')
  })
})

describe('asrEndpointUrl', () => {
  /**
   * ⚠️ 這一組守的是「音訊不會被髮到使用者沒打算用的地址」。
   *
   * 閘門必須看**選中的模型**，不能看卡片：檔案裡的 apiUrl 會殘留（換過供應商、
   * 換過模型都會留下），而殘值一旦被髮出去，音訊就去了一個使用者早就不用的地方。
   */
  it('模型不支援改地址時，殘留的 apiUrl 一律不發出去', () => {
    // qwen3.5-omni-plus 是 realtime WebSocket，地址改不了
    const ws = profile({
      provider: 'qwen',
      model: 'qwen3.5-omni-plus',
      apiUrl: 'http://leftover.example/v1',
    })
    expect(asrEndpointUrl(ws)).toBe('')

    // 同一張卡換成 HTTP 的那個模型，同一個地址就該生效了
    const http = { ...ws, model: 'qwen3.8-omni-flash' }
    expect(asrEndpointUrl(http)).toBe('http://leftover.example/v1')
  })

  it('地址留空時返回空串，讓後端用官方地址', () => {
    const p = profile({ provider: 'openai', model: 'gpt-transcribe', apiUrl: '   ' })
    expect(asrEndpointUrl(p)).toBe('')
  })

  it('內建卡填了地址就生效 —— 中轉站 / 反代要靠這個', () => {
    const p = profile({
      provider: 'openai',
      model: 'gpt-transcribe',
      apiUrl: 'https://relay.example/v1',
    })
    expect(asrEndpointUrl(p)).toBe('https://relay.example/v1')
  })
})

describe('parseAsrCompatProtocol', () => {
  it('只認三個顯式值，其餘一律當自動', () => {
    expect(parseAsrCompatProtocol('transcriptions')).toBe('transcriptions')
    expect(parseAsrCompatProtocol('chat')).toBe('chat')
    expect(parseAsrCompatProtocol('chat_standard')).toBe('chat_standard')
    for (const junk of ['auto', '', 'nope', undefined, null, 42, {}]) {
      expect(parseAsrCompatProtocol(junk)).toBe('auto')
    }
  })
})

describe('asrCardTitle', () => {
  /**
   * ⚠️ 這一組的核心是**標題裡絕不能出現金鑰的任何部分**。
   *
   * 這裡原來的做法是同平台多張卡時補「•• + 金鑰末 4 位」。使用者看到的第一反應是
   * 「我的 API Key 怎麼被顯示出來了」——在一個專門用眼睛圖示遮住金鑰的介面裡，
   * 標題上卻掛著金鑰尾巴，這個自相矛盾比它解決的問題更嚴重。
   * 把它改回去，這組測試會紅。
   */
  it('絕不把金鑰的任何片段拼進標題', () => {
    const key = 'sk-abcdef1234'
    const p = profile({ provider: 'qwen', apiKey: key })
    for (const siblings of [1, 2, 5]) {
      const title = asrCardTitle(p, siblings)
      expect(title).not.toContain('1234')
      expect(title).not.toContain(key.slice(-4))
      expect(title).not.toContain('••')
    }
  })

  it('使用者起了名字就用名字', () => {
    expect(asrCardTitle(profile({ provider: 'qwen', name: '公司帳號' }), 2)).toBe('公司帳號')
    // 只有一張卡時也用，名字是使用者的選擇、不是區分手段
    expect(asrCardTitle(profile({ provider: 'qwen', name: '公司帳號' }), 1)).toBe('公司帳號')
    // 純空白當沒填
    expect(asrCardTitle(profile({ provider: 'qwen', name: '   ' }), 1))
      .toBe(findAsrProvider('qwen')!.label)
  })

  it('沒起名字時只顯示平台名', () => {
    expect(asrCardTitle(profile({ provider: 'qwen' }), 1)).toBe(findAsrProvider('qwen')!.label)
    expect(asrCardTitle(profile({ provider: 'qwen' }), 3)).toBe(findAsrProvider('qwen')!.label)
  })

  /** 自建卡之間真正的差別是「連的是哪台」，主機名非機密，拿它區分正好。 */
  it('自定義端點的多張卡用主機名區分', () => {
    const p = profile({ provider: 'openai_compat', apiUrl: 'http://192.168.1.9:9000/v1' })
    expect(asrCardTitle(p, 2)).toContain('192.168.1.9:9000')
    // 只有一張卡時不加字尾，標題保持乾淨
    expect(asrCardTitle(p, 1)).toBe(findAsrProvider('openai_compat')!.label)
  })

  /** 內建卡即使檔案裡殘留了 apiUrl 也不能顯示它 —— 那個值不會被使用。 */
  it('內建卡不顯示殘留的 apiUrl', () => {
    const p = profile({ provider: 'qwen', apiUrl: 'http://leftover.example/v1' })
    expect(asrCardTitle(p, 2)).not.toContain('leftover')
  })
})

describe('gradeAsrLatency', () => {
  // 口徑是 RTF，不是絕對毫秒。3 秒音訊花 1 秒是正常水平，
  // 若照搬 AI 服務那套「1 秒就算慢」的閾值，所有 ASR 都會被標成太慢。
  it('3 秒音訊花 1 秒算正常，不算慢', () => {
    expect(gradeAsrLatency(1000, 3).tone).toBe('ok')
    expect(gradeAsrLatency(1000, 3).label).toBe('正常')
  })

  it('按 RTF 分檔', () => {
    expect(gradeAsrLatency(300, 3).label).toBe('極速')
    expect(gradeAsrLatency(700, 3).label).toBe('很快')
    expect(gradeAsrLatency(1200, 3).label).toBe('正常')
    expect(gradeAsrLatency(2000, 3).label).toBe('偏慢')
    expect(gradeAsrLatency(3000, 3).label).toBe('太慢')
  })

  it('同樣的毫秒數，音訊越長檔位越好', () => {
    // 刻意不取邊界值：閾值是「小於」，1500/10s 正好等於 0.15 會落進 fast 而非 instant
    expect(gradeAsrLatency(1000, 2).tier).toBe('slow')
    expect(gradeAsrLatency(1000, 10).tier).toBe('instant')
  })

  it('閾值是「小於」，邊界值落進下一檔', () => {
    expect(gradeAsrLatency(150, 1).tier).toBe('fast')
    expect(gradeAsrLatency(149, 1).tier).toBe('instant')
  })

  it('只有太慢才給 bad —— 紅色留給「不可用」', () => {
    expect(gradeAsrLatency(2000, 3).tone).toBe('warn')
    expect(gradeAsrLatency(9000, 3).tone).toBe('bad')
  })

  it('音訊時長缺失時不硬猜檔位', () => {
    expect(gradeAsrLatency(1000, 0).label).toBe('已測通')
    expect(gradeAsrLatency(1000, NaN).label).toBe('已測通')
  })
})
