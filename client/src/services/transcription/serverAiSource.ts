import { getSetting } from '../store'

export type ServerAiSource = 'managed' | 'custom'
export const SERVER_AI_SOURCE_KEY = 'server.aiSource'

let runtimeSource: ServerAiSource = 'managed'

function normalizeServerAiSource(value: unknown): ServerAiSource {
  return value === 'custom' ? 'custom' : 'managed'
}

/** 從持久化設定重新整理執行時值。Provider connect 時呼叫，保證冷啟動與匯入配置後正確。 */
export async function loadServerAiSource(): Promise<ServerAiSource> {
  runtimeSource = normalizeServerAiSource(await getSetting(SERVER_AI_SOURCE_KEY, 'managed'))
  return runtimeSource
}

/** start() 必須同步讀取這一份，不能依賴上一次 connect 留下的快照。 */
export function getRuntimeServerAiSource(): ServerAiSource {
  return runtimeSource
}

/** 設定頁儲存時同步更新，避免同一個 ServerProvider 例項繼續按舊來源傳送下一次請求。 */
export function setRuntimeServerAiSource(value: ServerAiSource): void {
  runtimeSource = value
}
