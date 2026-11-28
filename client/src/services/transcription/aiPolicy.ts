// AI 整理的「策略」與「結果」判據 —— 全模式、全入口唯一一份。
//
// 為什麼要單獨抽一層：影響「這次會不會被 AI 整理、由誰整理」的有四個設定
// （aiEnabled / aiMinDurationSec / server.aiSource / workMode），它們此前在五處
// 各判一遍（錄音器停止段、ServerProvider、clientAiPolish、History 的三條重跑路徑），
// 結論已經漂移了：雲 API 重跑漏了時長門檻、對 ollama 的配置判據和本地重跑不一致、
// 重跑路徑的失敗被空 catch 吞掉既無日誌也無代次檢查。改一處永遠漏一處。
//
// 拆成兩個函式而不是一個大函式，是因為兩件事的已知資訊不同：
//   - resolveAiPolicy：鬆鍵時就能算（配置 + 音訊時長），決定路由和是否允許呼叫。
//   - resolveAiOutcome：要等執行完（識別是否為空、服務端有沒有真跑、呼叫成功與否）。
// 合成一個會迫使呼叫方在資訊不全時先猜一個結果，再回頭改——那正是現在的毛病。

import { addRuntimeEvent, AI_EVENT_OUTCOME, AI_LOG_SOURCE } from '../debugLog'
import type { AiExecutionSource, AiExecutionStatus, WorkMode } from './types'

/**
 * 誰負責這次整理。**獨立於「有沒有執行」**。
 *
 * `custom` 永遠不是跳過原因：伺服器模式選自配 AI 時，服務端只是不做整理，客戶端會
 * 繼續呼叫（見 ServerProvider.handleCustomFinal）。把它記成跳過會直接把排查帶錯方向。
 */
export type AiRoute
  = | 'managed' // 伺服器內建 AI
  | 'custom' // 客戶端呼叫使用者自配的 AI 檔案（可能是遠端服務，不一定在本機）
  | 'integrated_asr' // 識別引擎自帶整理（如 Qwen Omni），不另外調獨立 AI
  | 'none' // 總開關關閉

/** 沒整理 / 沒整理成功的原因。穩定取值，日誌與歷史共用，不要改字面量。 */
export type AiReason
  = | 'ai_off' // 總開關關閉
  | 'duration_below_min' // 低於短語音門檻
  | 'empty_asr' // 沒識別出文本，無可整理
  | 'integrated_asr' // 識別引擎已內建整理
  | 'config_incomplete' // 選了自配 AI 但檔案不完整
  | 'call_failed' // 呼叫發出去了，失敗
  | 'call_timeout' // 呼叫超時
  | 'empty_output' // 呼叫成功但返回空文本
  | 'no_evidence' // 舊響應缺少執行證據，無法確認跑沒跑

export interface AiConfigSnapshot {
  workMode: WorkMode
  aiEnabled: boolean
  /** 0 = 不設門檻。 */
  aiMinDurationSec: number
  /** 僅伺服器模式有意義；其它模式整理一律由客戶端執行。 */
  serverAiSource: 'managed' | 'custom'
  /** A selected-text edit explicitly chooses a separate client AI profile. */
  selectionAiOverride?: boolean
}

export interface AiPolicyInput extends AiConfigSnapshot {
  /**
   * 實際 PCM 時長（秒）。**不要四捨五入，也不要用熱鍵按住時長或介面顯示值**：
   * 按住時長含鬆鍵前後的空白，介面值是格式化過的，兩者都會讓「恰好等於門檻」
   * 這一檔判錯。
   */
  audioDurationSec: number
  /** 識別引擎自帶整理（Qwen Omni 一類）。 */
  integratedAsr?: boolean
}

export interface AiPolicy {
  route: AiRoute
  /** 策略是否允許發起呼叫。false 時 reason 必定有值。 */
  allowCall: boolean
  reason?: AiReason
  aiEnabled: boolean
  workMode: WorkMode
  /** 凍結進日誌與歷史，供事後解釋用：不能拿今天的設定解釋昨天的記錄。 */
  audioMs: number
  minAudioMs: number
}

function toMs(sec: number): number {
  const value = Number(sec)
  return Number.isFinite(value) && value > 0 ? Math.round(value * 1000) : 0
}

/**
 * 決定路由與是否允許呼叫。
 *
 * 優先順序是刻意的：總開關關閉優先於時長門檻（使用者主動關掉 AI 時，報「低於門檻」
 * 會讓他去調一個根本不生效的設定）。配置完整性**不在這裡判** —— 它屬於執行證據，
 * 只有策略已經允許呼叫之後才有意義，否則 AI 關著也會報「配置不完整」。
 */
export function resolveAiPolicy(input: AiPolicyInput): AiPolicy {
  const audioMs = toMs(input.audioDurationSec)
  const minAudioMs = toMs(input.aiMinDurationSec)
  const base = {
    aiEnabled: input.aiEnabled,
    workMode: input.workMode,
    audioMs,
    minAudioMs,
  }

  if (!input.aiEnabled) {
    return { ...base, route: 'none', allowCall: false, reason: 'ai_off' }
  }

  // 路由只看配置，與「這次跑不跑」無關。日誌裡保留它才能回答
  // 「AI 開著但沒整理，本來該誰做」。
  const integratedAsr = input.integratedAsr && !input.selectionAiOverride
  const route: AiRoute = integratedAsr
    ? 'integrated_asr'
    : !input.selectionAiOverride && input.workMode === 'server' && input.serverAiSource === 'managed'
      ? 'managed'
      : 'custom'

  if (integratedAsr) {
    return { ...base, route, allowCall: false, reason: 'integrated_asr' }
  }
  if (minAudioMs > 0 && audioMs < minAudioMs) {
    return { ...base, route, allowCall: false, reason: 'duration_below_min' }
  }
  return { ...base, route, allowCall: true }
}

/** 執行證據。預設一律按「不知道」處理，絕不按「失敗」處理。 */
export interface AiEvidence {
  /** 識別結果是否為空。空文本時不存在「整理」這件事。 */
  asrTextEmpty?: boolean
  /** 自配 AI 檔案是否完整。僅 route=custom 時有意義。 */
  configComplete?: boolean
  /** 服務端回傳的 llm_debug.error。有值即服務端呼叫失敗。 */
  serverError?: string
  /**
   * 服務端回傳的 llm_debug.provider。有值即服務端**確實**調過 AI，
   * 哪怕 llm_ms 是 0 —— 不能再拿耗時反推成功失敗（服務端異常時返回原文 + 0ms，
   * 按耗時判會把「失敗」誤歸成「不可用」）。
   */
  serverProvider?: string
  /** 自配 AI 的失敗種類。 */
  clientFailure?: 'timeout' | 'error'
  /** 自配 AI 呼叫成功但返回空文本。 */
  clientOutputEmpty?: boolean
  llmMs?: number
  provider?: string
  model?: string
}

export interface AiOutcome {
  source: AiExecutionSource
  status: AiExecutionStatus
  reason?: AiReason
  /** 是否真的向 AI 發出過呼叫。跳過時必須為 false，用來交叉驗證日誌。 */
  attempted: boolean
  llmMs: number
  provider?: string
  model?: string
}

const NOT_ATTEMPTED = { attempted: false, llmMs: 0 } as const

/**
 * 由策略 + 執行證據得出最終結果。
 *
 * 刻意不比較文本：AI 整理成功但文本恰好沒變化（本來就規範的一句話）是正常結果，
 * 按文本相等判會把它記成沒執行。
 */
export function resolveAiOutcome(policy: AiPolicy, evidence: AiEvidence = {}): AiOutcome {
  // 空識別排在最前：沒有文本時「為什麼沒整理」的真答案是「沒識別出東西」，
  // 報門檻或總開關都會把人引去查 AI 設定。
  if (evidence.asrTextEmpty) {
    return { ...NOT_ATTEMPTED, source: 'none', status: 'skipped', reason: 'empty_asr' }
  }

  if (!policy.allowCall) {
    return { ...NOT_ATTEMPTED, source: 'none', status: 'skipped', reason: policy.reason }
  }

  if (policy.route === 'managed') {
    if (evidence.serverError) {
      return {
        source: 'server',
        status: 'failed',
        reason: 'call_failed',
        attempted: true,
        llmMs: evidence.llmMs ?? 0,
        provider: 'server',
      }
    }
    if (evidence.serverProvider || (evidence.llmMs ?? 0) > 0) {
      return {
        source: 'server',
        status: 'applied',
        attempted: true,
        llmMs: evidence.llmMs ?? 0,
        provider: 'server',
        model: evidence.model,
      }
    }
    // 舊版伺服器不帶 llm_debug.provider，只有 llm_ms=0 這一個訊號，而它既可能是
    // 「服務端沒跑」也可能是「跑了但極快/失敗回退」。不猜，如實記成無法確認。
    return {
      source: 'server',
      status: 'unavailable',
      reason: 'no_evidence',
      attempted: false,
      llmMs: 0,
      provider: 'server',
    }
  }

  // route === 'custom'
  if (evidence.configComplete === false) {
    return {
      ...NOT_ATTEMPTED,
      source: 'custom',
      status: 'unavailable',
      reason: 'config_incomplete',
      provider: evidence.provider,
      model: evidence.model,
    }
  }
  if (evidence.clientFailure) {
    return {
      source: 'custom',
      status: 'failed',
      reason: evidence.clientFailure === 'timeout' ? 'call_timeout' : 'call_failed',
      attempted: true,
      llmMs: evidence.llmMs ?? 0,
      provider: evidence.provider,
      model: evidence.model,
    }
  }
  if (evidence.clientOutputEmpty) {
    return {
      source: 'custom',
      status: 'failed',
      reason: 'empty_output',
      attempted: true,
      llmMs: evidence.llmMs ?? 0,
      provider: evidence.provider,
      model: evidence.model,
    }
  }
  return {
    source: 'custom',
    status: 'applied',
    attempted: true,
    llmMs: evidence.llmMs ?? 0,
    provider: evidence.provider,
    model: evidence.model,
  }
}

/**
 * 從 start 時凍結的配置快照 + 本次實際時長復原策略。
 *
 * 緩衝型 Provider（本地 / 雲 API）的識別在 stop 之後才做，時長由它們自己的 PCM 緩衝
 * 算出，所以策略只能在那時就地算。共用這個入口是為了保證「算的人不同、判據同一份」。
 *
 * 快照缺失時按「AI 開啟、無門檻」兜底：寧可多調一次 AI，也不要因為一個可選欄位
 * 沒傳就靜默不整理 —— 那正是本輪要消滅的故障形態。
 */
export function policyFromSnapshot(
  snapshot: AiConfigSnapshot | undefined,
  fallbackWorkMode: WorkMode,
  audioDurationSec: number,
  integratedAsr = false,
): AiPolicy {
  return resolveAiPolicy({
    workMode: snapshot?.workMode ?? fallbackWorkMode,
    aiEnabled: snapshot?.aiEnabled ?? false, // [自訂] S23：缺設定時預設關閉（原為 ?? true）
    aiMinDurationSec: snapshot?.aiMinDurationSec ?? 0,
    serverAiSource: snapshot?.serverAiSource ?? 'managed',
    selectionAiOverride: snapshot?.selectionAiOverride,
    audioDurationSec,
    integratedAsr,
  })
}

export interface AiOutcomeContext {
  /**
   * 本次處理的唯一標識。即時錄音用 runId 派生，歷史重跑每次新建 —— **不能複用**
   * 被重跑那條記錄的舊標識，否則兩次處理在日誌裡分不開。
   */
  operationId: string
  trigger: 'live' | 'history_reprocess'
  /** 服務端連線/會話標識，用來把客戶端日誌和伺服器日誌對起來（伺服器日誌每行有 cid/sid）。 */
  serverRef?: string
}

/**
 * 已落過最終摘要的 operationId。
 *
 * 「每次處理恰好一條 ai.outcome」如果只靠呼叫方自覺，早晚會在某條分支上漏掉或寫兩條
 * （asr / final / done 各寫一次是最容易發生的）。這裡做成結構保證：同一個 operationId
 * 第二次呼叫直接丟棄。容量有上限，避免長時間執行後無限增長。
 */
const loggedOutcomes = new Set<string>()
const MAX_LOGGED_OUTCOMES = 200

function markLogged(operationId: string): boolean {
  if (loggedOutcomes.has(operationId)) return false
  if (loggedOutcomes.size >= MAX_LOGGED_OUTCOMES) {
    // Set 保持插入序，刪最早的那個即可
    const oldest = loggedOutcomes.values().next().value
    if (oldest !== undefined) loggedOutcomes.delete(oldest)
  }
  loggedOutcomes.add(operationId)
  return true
}

/** 僅供測試重置。 */
export function __resetAiOutcomeLogDedupe(): void {
  loggedOutcomes.clear()
}

/**
 * 判定結果並落唯一一條 `ai.outcome`。
 *
 * 所有終結點都必須走這裡，不要自己拼日誌：這條日誌是「為什麼沒整理」的唯一答案，
 * 分散拼寫會讓欄位在各條路徑上不一致，而排查時是按欄位 grep 的。
 *
 * 只記長度與結論，不記識別正文、整理正文、prompt 正文。
 */
export function resolveAndLogAiOutcome(
  context: AiOutcomeContext,
  policy: AiPolicy,
  evidence: AiEvidence = {},
): AiOutcome {
  const outcome = resolveAiOutcome(policy, evidence)
  if (markLogged(context.operationId)) {
    addRuntimeEvent('info', AI_LOG_SOURCE, AI_EVENT_OUTCOME, {
      operationId: context.operationId,
      trigger: context.trigger,
      ...(context.serverRef && { serverRef: context.serverRef }),
      workMode: policy.workMode,
      aiEnabled: policy.aiEnabled,
      // route 始終獨立於 reason：custom 表示"服務端不做、客戶端做"，不是跳過。
      route: policy.route,
      audioMs: policy.audioMs,
      minAudioMs: policy.minAudioMs,
      status: outcome.status,
      ...(outcome.reason && { reason: outcome.reason }),
      attempted: outcome.attempted,
      llmMs: outcome.llmMs,
      ...(outcome.provider && { provider: outcome.provider }),
      ...(outcome.model && { model: outcome.model }),
    })
  }
  return outcome
}

/**
 * 從服務端 final 幀的 llm_debug 裡摘出結論性證據。
 *
 * 放在這裡而不是各解析點各寫一遍：即時 WebSocket 和歷史重跑是兩條獨立的解析路徑，
 * 分開寫必然漂移（本倉庫已經因此出過一次「重跑不帶執行狀態」）。
 * 只取 error 與 provider —— llm_debug 在伺服器開了 debug_llm 時含完整 prompt 與
 * 原始輸出，整體透傳出去早晚會被某處日誌打出來。
 */
export function extractServerAiEvidence(
  llmDebug: unknown,
): { error?: string; provider?: string } | undefined {
  if (!llmDebug || typeof llmDebug !== 'object') return undefined
  const source = llmDebug as Record<string, unknown>
  const error = typeof source.error === 'string' && source.error ? source.error : undefined
  const provider = typeof source.provider === 'string' && source.provider ? source.provider : undefined
  if (!error && !provider) return undefined
  return { ...(error && { error }), ...(provider && { provider }) }
}

/**
 * 服務端只需要一個布林：這次它要不要做整理。
 *
 * 客戶端自配 AI 時也要發 true —— 服務端不做、客戶端做，不是「整次跳過」。
 * 這是唯一允許把 policy 壓成布林的地方，壓完的值不要再回頭當原因用。
 */
export function serverShouldPolish(policy: AiPolicy): boolean {
  return policy.allowCall && policy.route === 'managed'
}
