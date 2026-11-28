// 「語音識別服務」列表的持久化，以及與執行時配置的同步。
//
// 兩套鍵，職責必須分清（與 aiProfileStore 同一套思路）：
//   1. 列表本身 —— `cloudAsr.profiles` + `cloudAsr.activeProfileId`，只有設定頁讀寫；
//   2. 執行時生效的那一份 —— `cloudAsr.provider / model / apiKey / appId`，外加
//      `cloudAsr.qwen.workspaceId`（流式字幕判定要用）與 `cloudAsr.omniSystemPrompt`。
//      這些扁平鍵由錄音鏈路（CloudAPIProvider、RecorderOrchestrator）、歷史記錄重跑、
//      識別測試、診斷頁共同消費，**這個契約不能動**。
//
// 規則：列表是真相，每次列表或啟用項變化都把啟用的那一份整份寫進執行時鍵。
// 「切了供應商但金鑰還是上一家的」這類 bug 就是因為過去這些鍵各自維護；現在只有一個寫入點。
//
// profiles 裡帶金鑰，會跟著配置匯出走（Rust 側 export_app_settings 是整表匯出，無需登記新鍵）。

import { getSetting, setSetting } from '@/services/store'
import { addRuntimeEvent } from '@/services/debugLog'
import { DOUBAO_KEYS, resolveDoubaoConsole } from '@/lib/cloudAsrCreds'
import {
  ASR_PLATFORMS,
  ASR_PROVIDERS,
  asrCardIdOfLegacyProvider,
  asrEndpointUrl,
  asrModelsOf,
  effectiveAsrCredentials,
  emptyAsrProfile,
  parseAsrProfilesDetailed,
  resolveActiveAsrProfile,
  resolveAsrApiModel,
  resolveAsrModelOption,
  resolveAsrRuntimeProvider,
  type AsrPlatform,
  type AsrProfile,
} from './asrProviderCatalog'

export const ASR_PROFILES_KEY = 'cloudAsr.profiles'
export const ASR_ACTIVE_PROFILE_KEY = 'cloudAsr.activeProfileId'
/**
 * 已經自動補過哪些服務（provider id 列表）。
 *
 * 為什麼不用「遷移完成」這種布林標記：那種標記只要被置上就再也不會重來，而它極容易
 * 在工作沒真正做完時就被消耗掉 —— 開發期就踩過一次：熱過載裝載了改到一半的程式碼，
 * 遷移分支因為一個過期的前置條件被整段跳過，標記卻照樣置成了 true，於是修好之後的
 * 正確邏輯永遠沒機會執行，使用者的服務一直少 3 個。
 *
 * 改成記錄「補過誰」之後：
 *  · 缺哪個補哪個，天然冪等，不怕重複執行；
 *  · 使用者主動刪掉的卡不會被"救回來"（它已在列表裡）；
 *  · 邏輯本身有 bug 修好後能自愈，不需要再造一個 v3 鍵。
 */
export const ASR_AUTO_CREATED_KEY = 'cloudAsr.autoCreatedProviders'

export interface AsrProfileState {
  profiles: AsrProfile[]
  activeId: string
}

/**
 * 本次載入時解析不出來的原始條目。落盤時原樣拼回去。
 *
 * 為什麼需要它：`parseAsrProfiles` 丟掉一個條目之後，下一次「歸一化後寫回」就會把它
 * 從磁碟上永久刪除。而這條鏈子已經真的造成過損失 —— 遷移判據寫錯時，使用者的
 * Gemini 與 OpenRouter 兩張卡連金鑰一起消失，而金鑰在儲存裡沒有第二份。
 *
 * 有了這份留存，即使解析邏輯再出錯，原始 JSON 也一直在 db 裡，改好程式碼下次載入即恢復。
 */
let orphanProfiles: unknown[] = []

/**
 * 把啟用的那份寫進執行時鍵。
 *
 * workspaceId 與 omniPrompt 也要跟著走：它們分別決定「能不能開流式字幕」和
 * 「Omni 怎麼整理」，如果只在設定頁裡改而不同步，執行時用的還是上一份的值。
 * 沒有可用服務時一律寫空串，讓下游自己判定未配置。
 *
 * `cloudAsr.model` 寫的是**解析後**的模型名（不是檔案裡那個可能為空的原始值）：
 * 執行時鏈路只會把它原樣傳給後端，回落規則不該在四個組裝點各實現一遍。
 */
async function syncRuntimeActive(profile: AsrProfile | null): Promise<void> {
  const creds = profile ? effectiveAsrCredentials(profile) : { apiKey: '', appId: '' }
  await Promise.all([
    // ⚠️ 寫的是**選中模型的 provider**，不是 profile.provider（那是卡片 id = 平台）。
    // 一張卡裡的模型可以走不同協議，真正的分發 key 只能從模型上取 ——
    // 寫成卡片 id 的話 Rust 會報 "ASR provider not implemented"。
    setSetting('cloudAsr.provider', profile ? resolveAsrRuntimeProvider(profile) : ''),
    setSetting('cloudAsr.model', profile ? resolveAsrApiModel(profile) : ''),
    setSetting('cloudAsr.apiKey', creds.apiKey),
    setSetting('cloudAsr.appId', creds.appId),
    setSetting('cloudAsr.qwen.workspaceId', profile?.workspaceId?.trim() ?? ''),
    setSetting('cloudAsr.omniSystemPrompt', profile?.omniPrompt ?? ''),
    // 自定義端點地址。經 asrEndpointUrl 取值，所以只有 customEndpoint 的卡會寫出
    // 非空值 —— 換成內建卡之後這裡會被清成空串，不會把音訊發到上一張卡的地址去。
    setSetting('cloudAsr.baseUrl', profile ? asrEndpointUrl(profile) : ''),
    // 「OpenAI 相容」那張卡的協議。auto 也照原樣存，由 buildAsrExtra 決定要不要下傳。
    setSetting('cloudAsr.protocol', profile?.protocol ?? 'auto'),
  ])
}

/** 某個平台已儲存的憑據（遷移時從舊的按平台鍵裡讀出來） */
interface PlatformCreds {
  apiKey: string
  otherKey: string
  appId: string
  console: 'new' | 'legacy'
  workspaceId: string
  omniPrompt: string
}

async function readPlatformCreds(platform: AsrPlatform): Promise<PlatformCreds> {
  const omniPrompt = await getSetting('cloudAsr.omniSystemPrompt', '') as string
  if (platform === 'doubao') {
    const appId = await getSetting(DOUBAO_KEYS.appId, '') as string
    const accessToken = await getSetting(DOUBAO_KEYS.accessToken, '') as string
    const consoleKey = await getSetting(DOUBAO_KEYS.consoleKey, '') as string
    const mode = resolveDoubaoConsole(await getSetting(DOUBAO_KEYS.console, '') as string, appId)
    return {
      apiKey: mode === 'new' ? consoleKey : accessToken,
      otherKey: mode === 'new' ? accessToken : consoleKey,
      appId,
      console: mode,
      workspaceId: '',
      omniPrompt,
    }
  }
  return {
    apiKey: await getSetting(`cloudAsr.${platform}.apiKey`, '') as string,
    otherKey: '',
    appId: '',
    console: 'new',
    workspaceId: platform === 'qwen'
      ? await getSetting('cloudAsr.qwen.workspaceId', '') as string
      : '',
    omniPrompt,
  }
}

function hasKey(creds: PlatformCreds): boolean {
  return creds.apiKey.trim() !== '' || creds.otherKey.trim() !== ''
}

/**
 * 按已有憑據補齊檔案：**平台填了金鑰，就給這個平台建一張卡**。
 *
 * 這條規則變過一次，兩次的理由都記下來免得再繞回去：
 *   · 最早是「每個平台一份」，但那時**一張卡 = 一個模型**，於是千問平台下 4 個變體
 *     只剩 1 張卡，使用者升級後發現"我的服務少了 3 個"——而它們本來都能用；
 *   · 於是改成「每個服務各建一份」，代價是介面上千問一家佔 5 張卡；
 *   · 現在一張卡帶模型下拉，一張卡就覆蓋了該平台全部模型，所以回到「每平台一張」——
 *     這次不會丟東西，模型都在下拉裡。
 *
 * 兩條跳過規則，缺一不可：
 *  · 已有同一張卡的檔案 —— 不重複建；
 *  · 已在 alreadyAuto 裡 —— 之前自動補過、被使用者刪掉了，不該再"救回來"。
 *    存量記的是舊分發 key，要先換算成卡片 id（asrCardIdOfLegacyProvider），
 *    不換算的話老使用者刪掉的卡會在升級後回來一次。
 *
 * 返回 added 讓呼叫方記帳，這也是它能反覆安全執行的原因。
 */
export function topUpProfiles(
  existing: AsrProfile[],
  credsByPlatform: Partial<Record<AsrPlatform, PlatformCreds>>,
  alreadyAuto: string[] = [],
): { profiles: AsrProfile[]; added: string[] } {
  const added: AsrProfile[] = []
  const autoCards = new Set(alreadyAuto.map(asrCardIdOfLegacyProvider))
  for (const entry of ASR_PROVIDERS) {
    const creds = credsByPlatform[entry.platform]
    if (!creds || !hasKey(creds)) continue
    if (existing.some((p) => p.provider === entry.id)) continue
    if (autoCards.has(entry.id)) continue
    const profile = emptyAsrProfile(entry.id)
    profile.apiKey = creds.apiKey
    profile.otherKey = creds.otherKey
    profile.appId = creds.appId
    profile.console = creds.console
    profile.workspaceId = creds.workspaceId
    // 這個平台有 Omni 模型就把 System Prompt 帶上 —— 它是 profile 級欄位，
    // 只在使用者真選了 Omni 模型時才生效，先備著不會有副作用。
    profile.omniPrompt = asrModelsOf(entry).some((m) => m.omni) ? creds.omniPrompt : ''
    added.push(profile)
  }
  return {
    // 順序跟著 ASR_PROVIDERS，保證卡片排列和內建清單一致
    profiles: [...existing, ...added],
    added: added.map((p) => p.provider),
  }
}

/**
 * 載入服務列表。首次進入會把舊的「按平台一套憑據」攤平成列表。
 *
 * 返回前會歸一化 activeId（指向已不存在的 id 時回落到第一條），並在確實寫過東西時
 * 順手同步執行時鍵 —— 否則會出現「列表裡高亮著 A，實際在用 B」。
 */
export async function loadAsrProfiles(): Promise<AsrProfileState> {
  const [rawProfiles, storedActiveId, rawAuto] = await Promise.all([
    getSetting(ASR_PROFILES_KEY, [] as unknown[]) as Promise<unknown>,
    getSetting(ASR_ACTIVE_PROFILE_KEY, '') as Promise<string>,
    getSetting(ASR_AUTO_CREATED_KEY, [] as unknown[]) as Promise<unknown>,
  ])

  const parsed = parseAsrProfilesDetailed(rawProfiles)
  // 解析不出來的條目原樣記下來，落盤時拼回去。**絕不靜默刪除** ——
  // 一次解析 bug 曾經把使用者的服務配置連金鑰一起從磁碟上抹掉（金鑰沒有第二份）。
  orphanProfiles = parsed.orphans
  if (orphanProfiles.length > 0) {
    addRuntimeEvent('warn', 'settings', 'ASR profiles could not be parsed; kept as-is in storage', {
      count: orphanProfiles.length,
    })
  }
  let profiles = parsed.profiles
  let activeId = storedActiveId
  let needsWrite = false
  const alreadyAuto = Array.isArray(rawAuto) ? rawAuto.filter((x): x is string => typeof x === 'string') : []

  // 每次載入都按已有憑據補一次缺的服務。因為記的是「補過誰」而不是「補完了」，
  // 反覆執行是安全的：已有的不動、刪過的不回來。
  const credsByPlatform: Partial<Record<AsrPlatform, PlatformCreds>> = {}
  for (const platform of Object.keys(ASR_PLATFORMS) as AsrPlatform[]) {
    credsByPlatform[platform] = await readPlatformCreds(platform)
  }
  const topUp = topUpProfiles(profiles, credsByPlatform, alreadyAuto)
  if (topUp.added.length > 0) {
    profiles = topUp.profiles
    needsWrite = true
    // 先記帳再落盤：這一步失敗也只會導致下次重來，不會造成"補過了卻沒記"的空檔
    await setSetting(ASR_AUTO_CREATED_KEY, [...alreadyAuto, ...topUp.added])
  }

  // 啟用項：沒有記錄時沿用執行時那個 provider 對應的檔案（升級前用的就是它）。
  // 執行時鍵存的是舊分發 key（`qwen_audio_stream`），要換算成卡片 id 才找得到。
  if (!activeId && profiles.length > 0) {
    const legacyProvider = await getSetting('cloudAsr.provider', 'doubao_v2') as string
    const card = asrCardIdOfLegacyProvider(legacyProvider)
    activeId = profiles.find((p) => p.provider === card)?.id ?? profiles[0].id
    needsWrite = true
  }

  const active = resolveActiveAsrProfile(profiles, activeId)
  if (active && active.id !== activeId) {
    activeId = active.id
    needsWrite = true
  }
  if (!active && activeId !== '') {
    activeId = ''
    needsWrite = true
  }

  if (needsWrite) await saveAsrProfiles({ profiles, activeId })

  return { profiles, activeId }
}

/**
 * 落盤列表 + 啟用項，並同步執行時鍵。所有寫路徑都必須走這裡。
 *
 * ⚠️ 寫入時會把 `orphanProfiles` 拼回去，所以**必須先經過 loadAsrProfiles**
 * （既有約定，activeId 也依賴它）。直接調這個函式會把解析不出來的條目丟掉。
 */
export async function saveAsrProfiles(state: AsrProfileState): Promise<void> {
  const active = resolveActiveAsrProfile(state.profiles, state.activeId)
  await Promise.all([
    setSetting(ASR_PROFILES_KEY, [...state.profiles, ...orphanProfiles]),
    setSetting(ASR_ACTIVE_PROFILE_KEY, active?.id ?? ''),
  ])
  await syncRuntimeActive(active)
}
