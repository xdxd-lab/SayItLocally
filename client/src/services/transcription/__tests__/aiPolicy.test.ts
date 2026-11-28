import { beforeEach, describe, expect, it, vi } from 'vitest'

const mocks = vi.hoisted(() => ({ addRuntimeEvent: vi.fn() }))

vi.mock('../../debugLog', () => ({
  addRuntimeEvent: mocks.addRuntimeEvent,
  AI_LOG_SOURCE: 'ai',
  AI_EVENT_REQUEST: 'ai.request',
  AI_EVENT_OUTCOME: 'ai.outcome',
}))

import {
  __resetAiOutcomeLogDedupe,
  extractServerAiEvidence,
  policyFromSnapshot,
  resolveAiOutcome,
  resolveAiPolicy,
  resolveAndLogAiOutcome,
  serverShouldPolish,
  type AiConfigSnapshot,
} from '../aiPolicy'

const SERVER_MANAGED: AiConfigSnapshot = {
  workMode: 'server',
  aiEnabled: true,
  aiMinDurationSec: 0,
  serverAiSource: 'managed',
}

function policy(over: Partial<Parameters<typeof resolveAiPolicy>[0]> = {}) {
  return resolveAiPolicy({ ...SERVER_MANAGED, audioDurationSec: 10, ...over })
}

describe('resolveAiPolicy：路由與是否允許呼叫', () => {
  it('伺服器 + 內建 AI → managed，允許呼叫', () => {
    const p = policy()
    expect(p.route).toBe('managed')
    expect(p.allowCall).toBe(true)
    expect(p.reason).toBeUndefined()
  })

  it('伺服器 + 自配 AI → custom，仍然允許呼叫', () => {
    // custom 只表示"服務端不做整理"，客戶端會接著調。它永遠不是跳過原因。
    const p = policy({ serverAiSource: 'custom' })
    expect(p.route).toBe('custom')
    expect(p.allowCall).toBe(true)
    expect(p.reason).toBeUndefined()
  })

  it.each(['local', 'cloud_api'] as const)('%s 模式一律 custom 路由', (workMode) => {
    // 這兩種模式沒有服務端 AI，serverAiSource 是殘留值也不該影響路由
    expect(policy({ workMode, serverAiSource: 'managed' }).route).toBe('custom')
  })

  it('總開關關閉 → route=none / reason=ai_off', () => {
    const p = policy({ aiEnabled: false, aiMinDurationSec: 60, audioDurationSec: 4.8 })
    expect(p.route).toBe('none')
    expect(p.allowCall).toBe(false)
    // 同時低於門檻時也要報總開關：讓使用者去調一個根本不生效的門檻是誤導
    expect(p.reason).toBe('ai_off')
  })

  it('低於門檻 → reason=duration_below_min，但路由仍保留', () => {
    const p = policy({ aiMinDurationSec: 60, audioDurationSec: 4.8, serverAiSource: 'custom' })
    expect(p.allowCall).toBe(false)
    expect(p.reason).toBe('duration_below_min')
    expect(p.route).toBe('custom')
    expect(p.audioMs).toBe(4800)
    expect(p.minAudioMs).toBe(60000)
  })

  it('門檻為 0 表示不設限', () => {
    expect(policy({ aiMinDurationSec: 0, audioDurationSec: 0.2 }).allowCall).toBe(true)
  })

  it('時長恰好等於門檻時允許呼叫', () => {
    // 判據是 <，不是 <=。用毫秒整數比較，避免浮點誤差把這一檔判反。
    expect(policy({ aiMinDurationSec: 2, audioDurationSec: 2 }).allowCall).toBe(true)
    expect(policy({ aiMinDurationSec: 2, audioDurationSec: 1.999 }).allowCall).toBe(false)
  })

  it('識別引擎自帶整理 → reason=integrated_asr，不冒充使用者預設已執行', () => {
    const p = policy({ workMode: 'cloud_api', integratedAsr: true })
    expect(p.route).toBe('integrated_asr')
    expect(p.allowCall).toBe(false)
    expect(p.reason).toBe('integrated_asr')
  })
})

describe('serverShouldPolish：壓成線上那個布林', () => {
  it.each(['local', 'cloud_api', 'server'] as const)('routes a separate selection model through client AI in %s mode', (workMode) => {
    const result = policy({ workMode, selectionAiOverride: true, integratedAsr: true })
    expect(result).toMatchObject({ route: 'custom', allowCall: true })
    expect(serverShouldPolish(result)).toBe(false)
  })

  it('keeps the AI switch and duration threshold when a separate editing model is selected', () => {
    expect(policy({ selectionAiOverride: true, aiEnabled: false })).toMatchObject({ allowCall: false, reason: 'ai_off' })
    expect(policy({ selectionAiOverride: true, aiMinDurationSec: 60, audioDurationSec: 1 })).toMatchObject({
      route: 'custom', allowCall: false, reason: 'duration_below_min',
    })
    expect(policyFromSnapshot({ ...SERVER_MANAGED, selectionAiOverride: true }, 'server', 4)).toMatchObject({
      route: 'custom', allowCall: true,
    })
  })

  it('只有 managed 且允許呼叫時才讓服務端整理', () => {
    expect(serverShouldPolish(policy())).toBe(true)
    expect(serverShouldPolish(policy({ serverAiSource: 'custom' }))).toBe(false)
    expect(serverShouldPolish(policy({ aiEnabled: false }))).toBe(false)
    expect(serverShouldPolish(policy({ aiMinDurationSec: 60, audioDurationSec: 1 }))).toBe(false)
  })
})

describe('resolveAiOutcome：結果與原因', () => {
  it('空識別優先於一切：沒文本時答案是 empty_asr', () => {
    const o = resolveAiOutcome(policy({ aiMinDurationSec: 60, audioDurationSec: 1 }), { asrTextEmpty: true })
    expect(o).toMatchObject({ source: 'none', status: 'skipped', reason: 'empty_asr', attempted: false })
  })

  it('策略不允許呼叫 → 原因直接沿用 policy 的，且 attempted=false', () => {
    const o = resolveAiOutcome(policy({ aiMinDurationSec: 60, audioDurationSec: 4.8 }))
    expect(o).toMatchObject({ status: 'skipped', reason: 'duration_below_min', attempted: false, llmMs: 0 })
  })

  it('managed：服務端回了 error → failed，不是 unavailable', () => {
    // 這是本輪修的核心錯判：服務端異常時返回原文 + llm_ms=0，
    // 舊程式碼按耗時判斷，把"呼叫失敗"記成了"不可用"。
    const o = resolveAiOutcome(policy(), { serverError: 'HTTP 400 temperature', llmMs: 0 })
    expect(o).toMatchObject({ source: 'server', status: 'failed', reason: 'call_failed', attempted: true })
  })

  it('managed：有 provider 證據即算執行過，哪怕耗時為 0', () => {
    const o = resolveAiOutcome(policy(), { serverProvider: 'openai', llmMs: 0 })
    expect(o).toMatchObject({ status: 'applied', attempted: true, provider: 'server' })
  })

  it('managed：只有耗時 > 0 也算執行過（相容不帶 provider 的伺服器）', () => {
    expect(resolveAiOutcome(policy(), { llmMs: 1228 }).status).toBe('applied')
  })

  it('managed：既無 provider 也無耗時 → no_evidence，不得斷言 failed', () => {
    const o = resolveAiOutcome(policy(), { llmMs: 0 })
    expect(o).toMatchObject({ status: 'unavailable', reason: 'no_evidence', attempted: false })
    expect(o.status).not.toBe('failed')
  })

  it('custom：配置不完整 → unavailable / config_incomplete，未嘗試', () => {
    const o = resolveAiOutcome(policy({ serverAiSource: 'custom' }), {
      configComplete: false,
      provider: 'zhipu',
    })
    expect(o).toMatchObject({
      source: 'custom', status: 'unavailable', reason: 'config_incomplete', attempted: false,
    })
  })

  it('custom：超時與呼叫失敗分開記', () => {
    const p = policy({ serverAiSource: 'custom' })
    expect(resolveAiOutcome(p, { clientFailure: 'timeout' }).reason).toBe('call_timeout')
    expect(resolveAiOutcome(p, { clientFailure: 'error' }).reason).toBe('call_failed')
    expect(resolveAiOutcome(p, { clientFailure: 'error' }).attempted).toBe(true)
  })

  it('custom：返回空文本 → failed / empty_output', () => {
    const o = resolveAiOutcome(policy({ serverAiSource: 'custom' }), { clientOutputEmpty: true, llmMs: 90 })
    expect(o).toMatchObject({ status: 'failed', reason: 'empty_output', attempted: true, llmMs: 90 })
  })

  it('custom：成功 → applied，並帶上供應商與模型', () => {
    const o = resolveAiOutcome(policy({ serverAiSource: 'custom' }), {
      llmMs: 680, provider: 'zhipu', model: 'GLM-5.3-Flash',
    })
    expect(o).toMatchObject({
      source: 'custom', status: 'applied', attempted: true, provider: 'zhipu', model: 'GLM-5.3-Flash',
    })
    expect(o.reason).toBeUndefined()
  })

  it('不拿文本相等當判據：整理成功但文本沒變化仍算成功', () => {
    // 判據裡壓根沒有文本引數，這條釘的是"以後也不許加進來"
    const o = resolveAiOutcome(policy(), { serverProvider: 'openai', llmMs: 5 })
    expect(o.status).toBe('applied')
  })
})

describe('policyFromSnapshot：緩衝型 Provider 復原策略', () => {
  it('快照存在時按快照判，不讀當前設定', () => {
    const p = policyFromSnapshot(
      { workMode: 'server', aiEnabled: true, aiMinDurationSec: 60, serverAiSource: 'custom' },
      'local',
      4.8,
    )
    expect(p).toMatchObject({ route: 'custom', allowCall: false, reason: 'duration_below_min' })
    expect(p.workMode).toBe('server')
  })

  // [自訂] S23：快照缺失時預設關閉 AI（原為「開啟、無門檻」）
  it('快照缺失時兜底為關閉 AI', () => {
    const p = policyFromSnapshot(undefined, 'cloud_api', 1)
    expect(p.allowCall).toBe(false)
    expect(p.reason).toBe('ai_off')
  })
})

describe('extractServerAiEvidence：只摘結論，不帶除錯正文', () => {
  it('摘出 error 與 provider', () => {
    expect(extractServerAiEvidence({ error: 'boom', provider: 'openai' }))
      .toEqual({ error: 'boom', provider: 'openai' })
  })

  it('丟棄 messages / raw_output 等可能含 prompt 與正文的欄位', () => {
    const evidence = extractServerAiEvidence({
      provider: 'openai',
      model: 'gpt-x',
      messages: [{ role: 'system', content: '使用者的預設正文' }],
      raw_output: '整理後的正文',
    })
    expect(evidence).toEqual({ provider: 'openai' })
    expect(JSON.stringify(evidence)).not.toContain('預設')
    expect(JSON.stringify(evidence)).not.toContain('整理後')
  })

  it('空物件 / 非物件 → undefined', () => {
    expect(extractServerAiEvidence({})).toBeUndefined()
    expect(extractServerAiEvidence(undefined)).toBeUndefined()
    expect(extractServerAiEvidence('nope')).toBeUndefined()
  })
})

describe('resolveAndLogAiOutcome：每次處理恰好一條，且不含正文', () => {
  beforeEach(() => {
    mocks.addRuntimeEvent.mockClear()
    __resetAiOutcomeLogDedupe()
  })

  const ctx = { operationId: 'op-1', trigger: 'live' as const, serverRef: 'cid-abc' }

  it('落一條 ai.outcome，欄位齊全且 route 與 reason 並存', () => {
    resolveAndLogAiOutcome(
      ctx,
      policy({ serverAiSource: 'custom', aiMinDurationSec: 60, audioDurationSec: 4.8 }),
    )
    expect(mocks.addRuntimeEvent).toHaveBeenCalledTimes(1)
    const [level, source, message, detail] = mocks.addRuntimeEvent.mock.calls[0]
    expect(level).toBe('info')
    expect(source).toBe('ai')
    expect(message).toBe('ai.outcome')
    expect(detail).toMatchObject({
      operationId: 'op-1',
      trigger: 'live',
      serverRef: 'cid-abc',
      workMode: 'server',
      aiEnabled: true,
      route: 'custom',
      status: 'skipped',
      reason: 'duration_below_min',
      attempted: false,
      audioMs: 4800,
      minAudioMs: 60000,
    })
  })

  it('同一個 operationId 第二次呼叫不再落盤（asr/final/done 各寫一次的結構性防護）', () => {
    resolveAndLogAiOutcome(ctx, policy())
    const second = resolveAndLogAiOutcome(ctx, policy())
    expect(mocks.addRuntimeEvent).toHaveBeenCalledTimes(1)
    // 但返回值仍然正確，呼叫方不必關心去重
    expect(second.status).toBeDefined()
  })

  it('不同 operationId 各記一條', () => {
    resolveAndLogAiOutcome({ ...ctx, operationId: 'a' }, policy())
    resolveAndLogAiOutcome({ ...ctx, operationId: 'b' }, policy())
    expect(mocks.addRuntimeEvent).toHaveBeenCalledTimes(2)
  })

  it('日誌裡不出現任何識別或整理正文欄位', () => {
    resolveAndLogAiOutcome(ctx, policy(), { serverProvider: 'openai', llmMs: 30 })
    const detail = mocks.addRuntimeEvent.mock.calls[0][3] as Record<string, unknown>
    for (const forbidden of ['asrText', 'llmText', 'text', 'messages', 'systemPrompt', 'raw_output']) {
      expect(Object.keys(detail)).not.toContain(forbidden)
    }
  })
})
