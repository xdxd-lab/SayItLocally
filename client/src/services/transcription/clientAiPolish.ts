import { invoke } from '@tauri-apps/api/core'
import { addRuntimeEvent, AI_EVENT_REQUEST, AI_LOG_SOURCE } from '../debugLog'
import { getSetting } from '../store'
import type { ClientAiConfig } from './selectionAi'
import {
  resolveAndLogAiOutcome,
  type AiEvidence,
  type AiOutcomeContext,
  type AiPolicy,
} from './aiPolicy'
import type { AiExecutionSource, AiExecutionStatus, StartOptions } from './types'

interface AiResult {
  text: string
  elapsed_ms: number
}

const CLIENT_AI_TIMEOUT_MS = 8_000

export interface ClientAiPolishResult {
  llmText: string
  llmMs: number
  contextApplied?: boolean
  aiSource: AiExecutionSource
  aiStatus: AiExecutionStatus
  aiProvider?: string
  aiModel?: string
  /** 跳過 / 失敗的穩定原因碼，供日誌與歷史共用。成功時為 undefined。 */
  aiReason?: string
  /** 是否真的向 AI 發出過呼叫。用來交叉驗證「跳過確實沒呼叫」。 */
  aiAttempted: boolean
}

interface ClientAiPolishOptions {
  asrText: string
  startOptions?: Readonly<StartOptions>
  /**
   * 本次策略。**必須由呼叫方算好傳進來**，不在這裡重算：
   * 時長門檻、總開關、路由此前在五處各判一遍，結論已經漂移過。
   */
  policy: AiPolicy
  /** 落 ai.outcome 用；同一個 operationId 只會寫出一條。 */
  outcomeContext: AiOutcomeContext
  logSource: string
  /** 非同步讀設定和原生請求結束後都要驗代；false 表示結果已經屬於舊錄音。 */
  isCurrent?: () => boolean
}

async function loadClientAiConfig(): Promise<ClientAiConfig> {
  const [provider, apiUrl, apiKey, model] = await Promise.all([
    getSetting('cloudAi.provider', 'openai_compat') as Promise<string>,
    getSetting('cloudAi.apiUrl', '') as Promise<string>,
    getSetting('cloudAi.apiKey', '') as Promise<string>,
    getSetting('cloudAi.model', '') as Promise<string>,
  ])
  return { provider, apiUrl, apiKey, model }
}

/**
 * 檔案是否可用。ollama 免密是唯一豁免。
 *
 * 這條判據此前有三份：這裡、History 的雲 API 重跑（漏了 ollama 豁免，於是 ollama
 * 使用者重跑時被靜默判成配置不完整）、History 的本地重跑。收口成一處。
 */
export function isClientAiConfigComplete(config: ClientAiConfig): boolean {
  return Boolean(
    config.apiUrl.trim()
    && config.model.trim()
    && (config.apiKey.trim() || config.provider === 'ollama'),
  )
}

function safeFallbackText(asrText: string, startOptions?: Readonly<StartOptions>): string {
  // 選區編輯依賴 AI 理解口述命令。AI 沒有真正完成時，回填原選區才是安全 no-op；
  // 直接回填 ASR 會把“改成更正式”這種命令本身覆蓋進正文。
  return startOptions?.textContext?.selectedText || asrText
}

function toResult(
  policy: AiPolicy,
  evidence: AiEvidence,
  options: ClientAiPolishOptions,
  llmText?: string,
): ClientAiPolishResult {
  const outcome = resolveAndLogAiOutcome(options.outcomeContext, policy, evidence)
  const applied = outcome.status === 'applied'
  return {
    llmText: applied && llmText ? llmText : safeFallbackText(options.asrText, options.startOptions),
    llmMs: outcome.llmMs,
    contextApplied: options.startOptions?.textContext ? applied : undefined,
    aiSource: outcome.source,
    aiStatus: outcome.status,
    aiProvider: outcome.provider,
    aiModel: outcome.model,
    aiReason: outcome.reason,
    aiAttempted: outcome.attempted,
  }
}

/**
 * 使用當前啟用的客戶端 AI 檔案整理一段 ASR 文本。
 *
 * 伺服器（自配來源）、雲 API、本地三種識別來源共用這一處，避免 Ollama 免密、
 * 短語音門檻、選區編輯回退在三條鏈路裡逐漸漂移。返回 null 表示錄音代次已經失效。
 *
 * 本函式只負責「執行 + 收集證據」，不自己判策略 —— 是否該呼叫由 policy 決定。
 * 結果裡帶齊 status/reason/attempted，呼叫方不要再靠耗時或文本相等反推。
 */
export async function polishWithClientAi(
  options: ClientAiPolishOptions,
): Promise<ClientAiPolishResult | null> {
  const { asrText, startOptions, policy, logSource } = options
  const isCurrent = options.isCurrent ?? (() => true)

  if (!asrText.trim()) {
    return toResult(policy, { asrTextEmpty: true }, options)
  }

  // 策略不允許呼叫（總開關關閉 / 低於門檻 / 引擎自帶整理）。這裡原來是靜默 return，
  // 於是"AI 沒生效"在日誌裡一個字都沒有 —— 本輪要修的正是它。原因由 policy 給出，
  // 呼叫方只負責落日誌，不在這裡重新解釋。
  if (!policy.allowCall) {
    return toResult(policy, {}, options)
  }

  const config = startOptions?.selectionAiConfig ?? await loadClientAiConfig()
  if (!isCurrent()) return null

  if (!isClientAiConfigComplete(config)) {
    return toResult(policy, {
      configComplete: false,
      provider: config.provider || undefined,
      model: config.model || undefined,
    }, options)
  }

  addRuntimeEvent('info', AI_LOG_SOURCE, AI_EVENT_REQUEST, {
    logSource,
    route: policy.route,
    provider: config.provider,
    model: config.model,
    // 只記長度，不記正文（與 providers/diag.rs 的口徑一致）
    asrChars: asrText.length,
  })

  let timeoutId: ReturnType<typeof setTimeout> | undefined
  try {
    const request = invoke<AiResult>('cloud_polish', {
      request: {
        text: asrText,
        ai_config: {
          provider: config.provider,
          api_url: config.apiUrl,
          api_key: config.apiKey,
          model: config.model,
        },
        system_prompt: startOptions?.systemPrompt || null,
        text_context: startOptions?.textContext || null,
      },
    })
    const result = await Promise.race([
      request,
      new Promise<never>((_, reject) => {
        timeoutId = setTimeout(
          () => reject(new Error(`Custom AI timed out after ${CLIENT_AI_TIMEOUT_MS}ms`)),
          CLIENT_AI_TIMEOUT_MS,
        )
      }),
    ])
    if (!isCurrent()) return null

    const evidence: AiEvidence = {
      llmMs: result.elapsed_ms,
      provider: config.provider,
      model: config.model,
      ...(result.text ? {} : { clientOutputEmpty: true }),
    }
    return toResult(policy, evidence, options, result.text)
  } catch (error) {
    if (!isCurrent()) return null
    const message = String(error)
    // 超時和呼叫失敗要分開：前者多半是模型慢或網路，後者常常是金鑰/地址/模型名。
    // 合成一個原因會讓這兩類問題在日誌裡長得一樣。
    const isTimeout = message.includes('timed out')
    // 原因碼進 ai.outcome，**錯誤原文只在這條 warn 裡**（warn 無條件落盤）。
    // 不把它塞進結果物件：那個物件會流向歷史記錄，錯誤文本可能回顯請求內容。
    addRuntimeEvent('warn', logSource, 'Custom AI cleanup failed; using raw ASR text', {
      error: message,
      provider: config.provider,
      model: config.model,
    })
    return toResult(policy, {
      clientFailure: isTimeout ? 'timeout' : 'error',
      provider: config.provider,
      model: config.model,
    }, options)
  } finally {
    if (timeoutId !== undefined) clearTimeout(timeoutId)
  }
}
