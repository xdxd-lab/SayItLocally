// 「AI 服務」列表的持久化，以及與執行時配置的同步。
//
// 這裡有兩套鍵，職責必須分清：
//   1. 列表本身 —— `cloudAi.profiles` + `cloudAi.activeProfileId`，只有本頁讀寫；
//   2. 執行時生效的那一份 —— `cloudAi.provider / apiUrl / apiKey / model`，
//      由錄音鏈路（CloudAPIProvider、LocalProvider）、歷史記錄重跑、診斷頁、反饋上報
//      六處消費。它們只認這四個扁平鍵，**這個契約不能動**。
//
// 所以規則是：列表是真相，每次列表或啟用項變化都把啟用的那條整份寫進那四個鍵。
// 「切了模型但地址/金鑰還是上一家的」這個 bug 之所以可能存在，就是因為過去這四個鍵
// 是分別維護的；現在它們只有一個寫入點。
//
// profiles 裡帶 apiKey，會跟著配置匯出走（Rust 側 export_app_settings 是整表匯出，
// 無需登記新鍵）。診斷報告不受影響：collect_settings 用的是固定白名單，不含 cloudAi.*。

import { getSetting, setSetting } from '@/services/store'
import {
  AI_PROVIDERS,
  aiSettingKey,
  migrateLegacyProfiles,
  parseLegacyLatencies,
  parseProfiles,
  resolveActiveProfile,
  type AiProfile,
  type LegacyProviderData,
} from './aiProviderCatalog'

export const AI_PROFILES_KEY = 'cloudAi.profiles'
export const AI_ACTIVE_PROFILE_KEY = 'cloudAi.activeProfileId'
/** 老資料只遷一次。沒有這個標記，使用者刪光列表後下次進頁面又會被"救回來" */
export const AI_PROFILES_MIGRATED_KEY = 'cloudAi.profilesMigrated'

export interface AiProfileState {
  profiles: AiProfile[]
  activeId: string
}

/** 把啟用的那條寫進執行時四個鍵。沒有可用的服務時寫空串，讓下游自己跳過 AI 環節 */
async function syncRuntimeActive(profile: AiProfile | null): Promise<void> {
  await Promise.all([
    setSetting('cloudAi.provider', profile?.provider ?? ''),
    setSetting('cloudAi.apiUrl', profile?.apiUrl ?? ''),
    setSetting('cloudAi.apiKey', profile?.apiKey ?? ''),
    setSetting('cloudAi.model', profile?.model ?? ''),
  ])
}

/** 讀出老的「每供應商一組配置」。鍵一律不刪：降級回舊版本還能用 */
async function readLegacyData(): Promise<LegacyProviderData[]> {
  return Promise.all(
    AI_PROVIDERS.map(async (provider): Promise<LegacyProviderData> => {
      const [apiUrl, apiKey, model, modelsRaw, latencyRaw] = await Promise.all([
        getSetting(aiSettingKey(provider.value, 'apiUrl'), '') as Promise<string>,
        getSetting(aiSettingKey(provider.value, 'apiKey'), '') as Promise<string>,
        getSetting(aiSettingKey(provider.value, 'model'), '') as Promise<string>,
        getSetting(aiSettingKey(provider.value, 'models'), '') as Promise<string>,
        getSetting(aiSettingKey(provider.value, 'latency'), '') as Promise<string>,
      ])
      return {
        provider: provider.value,
        apiUrl,
        apiKey,
        model,
        models: modelsRaw ? modelsRaw.split(',').map((m) => m.trim()).filter(Boolean) : [],
        latencies: parseLegacyLatencies(latencyRaw),
      }
    }),
  )
}

/**
 * 載入服務列表。首次進入會把老的按供應商存的配置攤平成列表。
 *
 * 返回前會把 activeId 歸一化（指向已不存在的 id 時回落到第一條），並在確實寫過東西時
 * 順手同步執行時那四個鍵——否則會出現"列表裡高亮著 A，實際在用 B"。
 */
export async function loadAiProfiles(): Promise<AiProfileState> {
  const [migrated, rawProfiles, storedActiveId] = await Promise.all([
    getSetting(AI_PROFILES_MIGRATED_KEY, false) as Promise<boolean>,
    getSetting(AI_PROFILES_KEY, [] as unknown[]) as Promise<unknown>,
    getSetting(AI_ACTIVE_PROFILE_KEY, '') as Promise<string>,
  ])

  let profiles = parseProfiles(rawProfiles)
  let activeId = storedActiveId
  let needsWrite = false

  if (!migrated) {
    // 老使用者：把每家的候選模型攤平成一條條服務；新使用者這裡得到空列表，走空狀態引導
    if (profiles.length === 0) {
      const [legacy, legacyProvider, legacyModel] = await Promise.all([
        readLegacyData(),
        getSetting('cloudAi.provider', '') as Promise<string>,
        getSetting('cloudAi.model', '') as Promise<string>,
      ])
      const result = migrateLegacyProfiles(legacy, legacyProvider, legacyModel)
      profiles = result.profiles
      activeId = result.activeId
    }
    await setSetting(AI_PROFILES_MIGRATED_KEY, true)
    needsWrite = profiles.length > 0
  }

  const active = resolveActiveProfile(profiles, activeId)
  if (active && active.id !== activeId) {
    activeId = active.id
    needsWrite = true
  }
  if (!active && activeId !== '') {
    activeId = ''
    needsWrite = true
  }

  if (needsWrite) {
    await saveAiProfiles({ profiles, activeId })
  }

  return { profiles, activeId }
}

/** 落盤列表 + 啟用項，並把啟用的那條同步進執行時四個鍵。所有寫路徑都必須走這裡 */
export async function saveAiProfiles(state: AiProfileState): Promise<void> {
  const active = resolveActiveProfile(state.profiles, state.activeId)
  await Promise.all([
    setSetting(AI_PROFILES_KEY, state.profiles),
    setSetting(AI_ACTIVE_PROFILE_KEY, active?.id ?? ''),
  ])
  await syncRuntimeActive(active)
  if (typeof window !== 'undefined') window.dispatchEvent(new Event('ai-profiles-changed'))
}
