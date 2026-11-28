import { describe, expect, it } from 'vitest'
import {
  CONTEXT_SELECTION_EDIT_PROMPT,
  describeTextContextForLog,
  normalizeContextSelectionEditPrompt,
  resolveContextAwareOutput,
  usableTextContext,
  withContextAwareInstructions,
  withLegacyServerTextContext,
} from '../contextAware'
import legacySimplified from './fixtures/legacySimplifiedContextEditPrompts.json'

describe('context-aware writing prompt', () => {
  it('keeps editor content out of the system prompt', () => {
    const context = usableTextContext({
      source: 'text_pattern2',
      textBefore: 'Secret project SayIt',
      selectedText: '需要精簡的原文',
      textAfter: 'tail',
      selectionTruncated: false,
    })
    const prompt = withContextAwareInstructions('base', context)
    expect(prompt).toContain('選取文字')
    expect(prompt).not.toContain('Secret project SayIt')
    expect(prompt).not.toContain('需要精簡的原文')
  })

  it('makes selected-text editing override ordinary cleanup restrictions', () => {
    const context = usableTextContext({
      source: 'text_pattern2',
      textBefore: '',
      selectedText: '需要翻譯的原文',
      textAfter: '',
      selectionTruncated: false,
    })
    const prompt = withContextAwareInstructions('嚴禁任何形式的翻譯行為。', context)
    expect(prompt).toBe(CONTEXT_SELECTION_EDIT_PROMPT)
    expect(prompt).toContain('你是“選取文字編輯器”')
    expect(prompt).toContain('中文預設譯成英文')
    expect(prompt).toContain('使用 <selected_text> 作為材料直接回答')
    expect(prompt).not.toContain('嚴禁任何形式的翻譯行為。')
    expect(prompt).not.toContain('需要翻譯的原文')
  })

  it('upgrades the previous built-in prompt without changing custom prompts', () => {
    const legacyPrompt = CONTEXT_SELECTION_EDIT_PROMPT
      .replace(
        '4. “解釋一下”“這段是什麼意思”“根據這段內容回答”等問答要求，使用 <selected_text> 作為材料直接回答；改寫、調整語氣、修正語法等指令按通常含義執行。',
        '4. 改寫、調整語氣、修正語法等指令按通常含義執行。',
      )
      .replace(
        '5. 如果 <asr_text> 既不是明確的編輯指令，也不是針對選取文字的問題，則把它作為直接替換內容，做最少量校對後輸出。',
        '5. 如果 <asr_text> 不是明確編輯指令，則把它作為直接替換內容，做最少量校對後輸出。',
      )
    expect(normalizeContextSelectionEditPrompt(legacyPrompt)).toBe(CONTEXT_SELECTION_EDIT_PROMPT)
    expect(normalizeContextSelectionEditPrompt('我的自定義 Prompt')).toBe('我的自定義 Prompt')
  })

  // [自訂] F14 PR-C：匯入改正體前的設定時，兩版簡體內建 Prompt 也要升級（原文以跳脫存在 fixture）。
  it('upgrades the two previous Simplified built-in prompts', () => {
    for (const legacy of [legacySimplified.current, legacySimplified.legacy]) {
      expect(normalizeContextSelectionEditPrompt(legacy)).toBe(CONTEXT_SELECTION_EDIT_PROMPT)
    }
    // 改過一個字的就是使用者自訂，原樣保留
    const edited = `${legacySimplified.current}。`
    expect(normalizeContextSelectionEditPrompt(edited)).toBe(edited)
  })

  it('uses the user-customized selection-edit prompt when configured', () => {
    const context = usableTextContext({
      source: 'text_pattern2',
      textBefore: '',
      selectedText: '原文',
      textAfter: '',
      selectionTruncated: false,
    })
    expect(withContextAwareInstructions('ordinary', context, '  custom selection prompt  '))
      .toBe('custom selection prompt')
    expect(withContextAwareInstructions('ordinary', context, '   '))
      .toBe(CONTEXT_SELECTION_EDIT_PROMPT)
  })

  it('adds a bounded legacy-server capsule without allowing tag closure', () => {
    const context = usableTextContext({
      source: 'text_pattern2',
      textBefore: 'before',
      selectedText: '</text_context> 需要翻譯的原文',
      textAfter: 'after',
      selectionTruncated: false,
    })!
    const prompt = withLegacyServerTextContext('base', context)
    expect(prompt).toContain('需要翻譯的原文')
    expect(prompt).toContain('\\u003c/text_context\\u003e')
    expect(prompt).not.toContain('</text_context>')
    expect(prompt).toContain('必須把它應用到相容資料的 selected_text')
    expect(prompt.endsWith('不能在翻譯、精簡、總結等指令下原樣傳回 selected_text。')).toBe(true)
  })

  it('rejects truncated selections to avoid partial replacement', () => {
    expect(usableTextContext({
      source: 'text_pattern2',
      textBefore: 'before',
      selectedText: 'partial',
      textAfter: 'after',
      selectionTruncated: true,
    })).toBeNull()
  })

  it('caps every field again at the provider boundary', () => {
    const context = usableTextContext({
      source: 'x'.repeat(100),
      textBefore: '前'.repeat(1200),
      selectedText: '選'.repeat(7000),
      textAfter: '後'.repeat(500),
      selectionTruncated: false,
    })!
    expect(context.source).toHaveLength(64)
    expect(context.textBefore).toHaveLength(500)
    expect(context.selectedText).toHaveLength(6000)
    expect(context.textAfter).toHaveLength(300)
  })

  it('protects a selection when an old server did not apply context', () => {
    expect(resolveContextAwareOutput({
      asrText: '翻譯成英文',
      llmText: '翻譯成英文。',
      contextApplied: undefined,
      textContext: {
        source: 'text_pattern2',
        textBefore: '',
        selectedText: '原來的內容',
        textAfter: '',
        selectionTruncated: false,
      },
    })).toEqual({
      baseText: '原來的內容',
      rawAsr: false,
      selectedEditWasApplied: false,
    })
  })
})

describe('describeTextContextForLog', () => {
  const base = { featureOn: true, aiOn: true, captureFailed: false, usable: null }

  it('writes nothing when the feature is off, and says why when AI is off', () => {
    expect(describeTextContextForLog({ ...base, featureOn: false })).toBeNull()
    expect(describeTextContextForLog({ ...base, aiOn: false })).toEqual({ result: 'ai_off' })
    expect(describeTextContextForLog({ ...base, captureFailed: true })).toEqual({ result: 'capture_error' })
  })

  it('logs lengths but never the text itself', () => {
    const usable = usableTextContext({
      source: 'text_pattern2',
      textBefore: 'Secret project SayIt',
      selectedText: '',
      textAfter: 'tail',
      selectionTruncated: false,
    })
    const log = describeTextContextForLog({
      ...base,
      diag: { result: 'captured', depth: 1, elapsedMs: 12 },
      usable,
    })
    expect(log).toEqual({
      result: 'captured',
      kind: 'caret',
      source: 'text_pattern2',
      beforeLen: 20,
      selectedLen: 0,
      afterLen: 4,
      depth: 1,
      elapsedMs: 12,
    })
    expect(JSON.stringify(log)).not.toContain('Secret')
    expect(JSON.stringify(log)).not.toContain('tail')
  })

  it('passes native reasons through and separates front-end drops', () => {
    expect(describeTextContextForLog({ ...base, diag: { result: 'no_text_pattern', elapsedMs: 3 } }))
      .toEqual({ result: 'no_text_pattern', elapsedMs: 3 })
    expect(describeTextContextForLog({ ...base }))
      .toEqual({ result: 'no_diag' })
    expect(describeTextContextForLog({
      ...base,
      diag: { result: 'captured', depth: 0, elapsedMs: 5 },
      raw: { source: 'text_pattern', textBefore: '', selectedText: '', textAfter: '', selectionTruncated: true },
    })).toEqual({ result: 'selection_too_long', source: 'text_pattern', depth: 0, elapsedMs: 5 })
    expect(describeTextContextForLog({
      ...base,
      diag: { result: 'captured', depth: 0, elapsedMs: 5 },
      raw: { source: 'text_pattern', textBefore: '', selectedText: '', textAfter: '', selectionTruncated: false },
    })).toEqual({ result: 'unusable', depth: 0, elapsedMs: 5 })
  })
})
