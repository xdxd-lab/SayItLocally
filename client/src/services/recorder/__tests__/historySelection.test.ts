import { describe, expect, it, vi } from 'vitest'
import type { TextContext } from '@/types/appContext'
import { RecorderOrchestrator } from '../RecorderOrchestrator'

const mocks = vi.hoisted(() => ({
  addHistory: vi.fn(),
  getSetting: vi.fn(async (key: string, fallback: unknown) => (
    ['historyEnabled', 'contextAwareWritingEnabled', 'aiEnabled'].includes(key) ? true : fallback
  )),
}))

vi.mock('../../store', () => ({
  addHistory: mocks.addHistory,
  getSetting: mocks.getSetting,
  deleteHistory: vi.fn(),
}))
vi.mock('../../bridge', () => ({ emit: vi.fn(async () => {}) }))
vi.mock('../../debugLog', () => ({ addRuntimeEvent: vi.fn() }))
vi.mock('../../transcription', () => ({ getProvider: () => ({ mode: 'local' }) }))
vi.mock('../../textPostProcess', () => ({ applyTextTransforms: vi.fn(async (text: string) => text) }))
vi.mock('../../personalization/store', () => ({}))

// [自訂] S68：從有選取內容的辨識結果進入收尾流程，驗證寫入的歷史與交給貼上的文字。
describe('history for editing an external selection', () => {
  it.each([
    [false, 'Make it shorter'],
    [undefined, 'Make it shorter'],
    [true, 'Edited text'],
  ])('persists the intended history text when contextApplied = %s', async (contextApplied, expected) => {
    mocks.addHistory.mockClear()
    const selectedText = 'Private text selected in the editor'
    const textContext: TextContext = {
      source: 'text_pattern',
      textBefore: '',
      selectedText,
      textAfter: '',
      selectionTruncated: false,
    }
    const result = {
      asrText: 'Make it shorter',
      llmText: contextApplied ? 'Edited text' : selectedText,
      asrMs: 2,
      llmMs: 3,
      durationSec: 1,
      contextApplied,
    }
    const recorder = new RecorderOrchestrator()
    const insert = vi.fn(async () => {})
    Reflect.set(recorder, 'activeRunId', 1)
    Reflect.set(recorder, 'handleTextInsertion', insert)
    await recorder['processFinalResult'](result, {
      runId: 1,
      timedOutAt: 0,
      settled: false,
      audioDurationSec: 1,
      wallTimeSec: 1,
      promptResolution: null,
      appContext: { processName: 'editor', textContext },
      audioChunks: [],
      probeResult: null,
    }, { allowInsertionWhenIdle: true, source: 'processing' })
    expect(insert).toHaveBeenCalledWith(contextApplied ? 'Edited text' : selectedText, expect.any(Object))
    expect(mocks.addHistory).toHaveBeenCalledWith(expect.objectContaining({
      asrText: 'Make it shorter',
      llmText: expected,
      charCount: expected.length,
      isEmpty: false,
    }))
    expect(JSON.stringify(mocks.addHistory.mock.calls)).not.toContain(selectedText)
  })
})
