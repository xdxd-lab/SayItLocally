import { describe, it, expect, vi, beforeEach } from 'vitest'

// 儲存層走 Tauri IPC，換成一個記憶體 Map。用 vi.hoisted 是因為 vi.mock 的工廠會被提到
// 檔案頂部執行，普通 const 那時還沒初始化。
const { store } = vi.hoisted(() => ({ store: new Map<string, unknown>() }))
vi.mock('@/services/store', () => ({
  getSetting: (key: string, fallback: unknown) =>
    Promise.resolve(store.has(key) ? store.get(key) : fallback),
  setSetting: (key: string, value: unknown) => {
    store.set(key, value)
    return Promise.resolve()
  },
}))
vi.mock('@/services/debugLog', () => ({ addRuntimeEvent: () => { } }))

import {
  ASR_ACTIVE_PROFILE_KEY,
  ASR_PROFILES_KEY,
  loadAsrProfiles,
  saveAsrProfiles,
  topUpProfiles,
} from '../asrProfileStore'
import {
  asrModelsOf,
  emptyAsrProfile,
  findAsrProvider,
  type AsrProfile,
} from '../asrProviderCatalog'

function creds(over: Record<string, unknown> = {}) {
  return {
    apiKey: 'KEY', otherKey: '', appId: '', console: 'new' as const,
    workspaceId: '', omniPrompt: 'P', ...over,
  }
}

function profile(provider: string, over: Partial<AsrProfile> = {}): AsrProfile {
  return { ...emptyAsrProfile(provider), ...over }
}

describe('topUpProfiles', () => {
  /**
   * 這條規則變過兩次，兩次的理由都寫在 topUpProfiles 的註釋裡。現在是「每平台一張卡」，
   * 而且這次不會丟東西 —— 該平台的全部模型都在那張卡的下拉里。
   */
  it('一个平台填了密钥，就给这个平台建一张卡', () => {
    const r = topUpProfiles([], { qwen: creds({ apiKey: 'sk-1' }) })
    expect(r.profiles.map((p) => p.provider)).toEqual(['qwen'])
    expect(r.added).toEqual(['qwen'])
    // 那張卡覆蓋千問全部模型（原來佔 5 張卡的那些，加上兩個上一代 Omni），一個都沒丟
    expect(asrModelsOf(findAsrProvider('qwen')!).length).toBeGreaterThanOrEqual(5)
  })

  it('三个平台都有密钥时补出三张卡', () => {
    const r = topUpProfiles([], {
      doubao: creds(), qwen: creds({ apiKey: 'sk-1' }), mimo: creds(),
    })
    expect(r.profiles.map((p) => p.provider)).toEqual(['doubao', 'qwen', 'mimo'])
  })

  it('没填密钥的平台不建卡 —— 新用户应看到空状态引导，而不是一堆空卡', () => {
    expect(topUpProfiles([], {}).profiles).toEqual([])
    expect(topUpProfiles([], { qwen: creds({ apiKey: '   ' }) }).profiles).toEqual([])
    expect(topUpProfiles([], { mimo: creds({ apiKey: '', otherKey: '' }) }).profiles).toEqual([])
  })

  it('只补缺的平台，已有档案原样不动（幂等）', () => {
    const existing = [profile('qwen', { apiKey: 'MY-OWN' })]
    const r = topUpProfiles(existing, {
      qwen: creds({ apiKey: 'sk-1' }), doubao: creds(),
    })
    expect(r.profiles).toHaveLength(2)
    expect(r.profiles[0]).toBe(existing[0]) // 同一個物件，沒被替換
    expect(r.added).toEqual(['doubao'])
  })

  it('重复调用不会越补越多', () => {
    const once = topUpProfiles([], { doubao: creds() })
    const twice = topUpProfiles(once.profiles, { doubao: creds() })
    expect(twice.profiles).toEqual(once.profiles)
    expect(twice.added).toEqual([])
  })

  /**
   * 使用者主動刪掉的卡不該被"救回來"。
   * 記「補過誰」而不是「補完了」，就是為了同時滿足自愈與尊重刪除這兩件事。
   */
  it('已经自动补过又被删掉的卡不再重建', () => {
    const r = topUpProfiles([], { qwen: creds({ apiKey: 'sk-1' }) }, ['qwen'])
    expect(r.added).toEqual([])
  })

  /**
   * 存量的「補過誰」那份記賬裡存的是**舊的分發 key**（`qwen_audio_stream` 之類）。
   * 不換算成卡片 id 的話新程式碼認不出來，會把老使用者主動刪掉的卡在升級後補回來一次。
   */
  it('存量记账里的旧 provider id 也算已补过', () => {
    const r = topUpProfiles([], { qwen: creds({ apiKey: 'sk-1' }) }, ['qwen_audio_stream'])
    expect(r.added).toEqual([])
    const other = topUpProfiles([], { doubao: creds() }, ['doubao_v2'])
    expect(other.added).toEqual([])
  })

  it('豆包的控制台代次与两代密钥都带过去', () => {
    const { profiles: [p] } = topUpProfiles([], {
      doubao: creds({ apiKey: 'TOKEN', otherKey: 'APPKEY', appId: '123', console: 'legacy' }),
    })
    expect(p.console).toBe('legacy')
    expect(p.apiKey).toBe('TOKEN')
    expect(p.otherKey).toBe('APPKEY')
    expect(p.appId).toBe('123')
  })

  it('只有豆包侧填了 App ID + Access Token（apiKey 为空）也算有密钥', () => {
    // 舊版控制台使用者可能只有 otherKey 那一側有值，不能因此判成"沒配置"
    const r = topUpProfiles([], { doubao: creds({ apiKey: '', otherKey: 'APPKEY' }) })
    expect(r.profiles).toHaveLength(1)
  })

  /**
   * System Prompt 只給「有 Omni 模型的平台」。
   *
   * 它是 profile 級欄位、只在使用者真選了 Omni 模型時才生效，所以千問那張卡先備著；
   * 豆包那張卡沒有 Omni 模型，帶上就是一個永遠用不到的欄位。
   */
  it('System Prompt 只给有 Omni 模型的平台', () => {
    const { profiles } = topUpProfiles([], {
      qwen: creds({ apiKey: 'sk-1', omniPrompt: 'MY PROMPT' }),
      doubao: creds({ omniPrompt: 'MY PROMPT' }),
    })
    const byId = new Map(profiles.map((p) => [p.provider, p]))
    expect(byId.get('qwen')!.omniPrompt).toBe('MY PROMPT')
    expect(byId.get('doubao')!.omniPrompt).toBe('')
  })

  it('业务空间 ID 跟着千问平台走', () => {
    const { profiles } = topUpProfiles([], { qwen: creds({ apiKey: 'sk-1', workspaceId: 'ws-abc' }) })
    expect(profiles.every((p) => p.workspaceId === 'ws-abc')).toBe(true)
  })

  it('补出来的档案 id 互不相同', () => {
    const { profiles } = topUpProfiles([], { doubao: creds(), qwen: creds(), mimo: creds() })
    expect(new Set(profiles.map((p) => p.id)).size).toBe(profiles.length)
  })

  /**
   * 補出來的檔案必須**顯式帶上預設模型**，不能留空串。
   *
   * parseAsrProfiles 靠「有沒有 model」區分新老資料（`qwen`/`mimo`/`doubao` 這幾個值
   * 既是舊分發 key 又是新卡片 id，光看 provider 分不出來）。留空會讓這些新建的條目
   * 下次載入時被當成存量資料走遷移，於是模型被換成遷移表裡那個。
   */
  it('补出来的档案显式写上默认模型', () => {
    const { profiles } = topUpProfiles([], { doubao: creds(), qwen: creds() })
    const byId = new Map(profiles.map((p) => [p.provider, p]))
    expect(byId.get('doubao')!.model).toBe('Doubao-Seed-ASR-2.0')
    expect(byId.get('qwen')!.model).toBe('qwen-audio-3.1-asr-flash-streaming')
    expect(profiles.every((p) => p.model !== '')).toBe(true)
  })
})

/**
 * 落盤往返。
 *
 * 為什麼必須單獨測這一路：光測 `parseAsrProfilesDetailed` 的返回值不算數 ——
 * 真正把配置從磁碟上抹掉的是隨後那次「歸一化後寫回」。寫回時不把 orphans 拼回去的話，
 * 解析側的斷言照樣全綠，而使用者的金鑰又丟一次（已經發生過一次，丟的是 Gemini 與
 * OpenRouter 兩張卡）。所以判據釘在**儲存裡最終有什麼**，不是解析函式返回了什麼。
 */
describe('loadAsrProfiles / saveAsrProfiles 的落盘往返', () => {
  beforeEach(() => {
    store.clear()
  })

  it('解析不出来的条目在写回之后依然躺在存储里', async () => {
    // 未來版本寫的卡片 / 手改壞的條目 —— 現在這份程式碼認不出來
    const alien = { id: 'p-alien', provider: 'some-future-platform', model: 'x', apiKey: 'SECRET' }
    store.set(ASR_PROFILES_KEY, [
      { id: 'p-1', provider: 'doubao', model: 'Doubao-Seed-ASR-2.0', apiKey: 'TOKEN' },
      alien,
    ])

    // load 自己就會寫一次（要落 activeId），這正是當初丟資料的那條路徑
    const state = await loadAsrProfiles()
    expect(state.profiles.map((p) => p.provider)).toEqual(['doubao'])
    expect(store.get(ASR_PROFILES_KEY)).toContainEqual(alien)

    // 使用者接著在設定頁改了點別的，再寫一次也不能吃掉它
    await saveAsrProfiles(state)
    const written = store.get(ASR_PROFILES_KEY) as unknown[]
    expect(written).toContainEqual(alien)
    expect(written).toHaveLength(2)
  })

  it('全都认得时不会往存储里塞多余条目', async () => {
    store.set(ASR_PROFILES_KEY, [
      { id: 'p-1', provider: 'doubao', model: 'Doubao-Seed-ASR-2.0', apiKey: 'TOKEN' },
    ])
    const state = await loadAsrProfiles()
    await saveAsrProfiles(state)
    expect(store.get(ASR_PROFILES_KEY)).toHaveLength(1)
    expect(store.get(ASR_ACTIVE_PROFILE_KEY)).toBe('p-1')
  })

  /**
   * 執行時鍵寫的是**選中模型**的分發 key，不是卡片 id。寫成卡片 id 的話
   * Rust 側會報 "ASR provider not implemented"（一張卡里的模型可以走不同協議）。
   */
  it('写回时把选中模型的分发 key 同步进运行时键', async () => {
    store.set(ASR_PROFILES_KEY, [
      { id: 'p-1', provider: 'qwen', model: 'qwen3-asr-flash', apiKey: 'sk-1' },
    ])
    await loadAsrProfiles()
    expect(store.get('cloudAsr.provider')).toBe('qwen')
    expect(store.get('cloudAsr.model')).toBe('qwen3-asr-flash')
    expect(store.get('cloudAsr.apiKey')).toBe('sk-1')
  })
})
