import { beforeEach, describe, expect, it, vi } from 'vitest'
import { RecorderOrchestrator } from '../RecorderOrchestrator'

const mocks = vi.hoisted(() => ({
  addHistory: vi.fn(async () => {}),
  copyText: vi.fn(async () => {}),
  pasteText: vi.fn(),
  postToTarget: vi.fn(),
  releaseRecordingFocus: vi.fn(async () => {}),
  presentOverlay: vi.fn(async () => {}),
  clipboard: 'original clipboard',
  target: 'file-a',
  scheduledClipboardRestores: 0,
}))

vi.mock('../../bridge', () => ({
  pasteText: mocks.pasteText,
  copyText: mocks.copyText,
  releaseRecordingFocus: mocks.releaseRecordingFocus,
  presentOverlay: mocks.presentOverlay,
  hideOverlay: vi.fn(async () => {}),
  emit: vi.fn(async () => {}),
  setEscapeActionMode: vi.fn(async () => {}),
  setCardHotkeys: vi.fn(async () => {}),
}))
vi.mock('../../store', () => ({
  addHistory: mocks.addHistory,
  getSetting: vi.fn(async (key: string, fallback: unknown) => key === 'historyEnabled' ? true : fallback),
  deleteHistory: vi.fn(),
}))
vi.mock('../../debugLog', () => ({ addRuntimeEvent: vi.fn() }))
vi.mock('../../transcription', () => ({ getProvider: () => ({ mode: 'local' }) }))
vi.mock('../../textPostProcess', () => ({ applyTextTransforms: vi.fn(async (text: string) => text) }))
vi.mock('../../personalization/store', () => ({}))

describe('recording focus guard', () => {
  const probe = {
    editable: true,
    hwnd: '100',
    focusHwnd: '100',
    process: 'fake editor',
    detail: 'fixture',
    focusToken: 'original-recording-token',
  }

  beforeEach(() => {
    vi.clearAllMocks()
    mocks.target = 'file-a'
    mocks.clipboard = 'original clipboard'
    mocks.scheduledClipboardRestores = 0
    mocks.pasteText.mockImplementation(async (
      text: string, _pid: string, _focused: string, protect: boolean, token?: string,
    ) => {
      // Native IPC test double. Keep native AX, keyboard events and the system clipboard out of the test.
      if (token === probe.focusToken && mocks.target !== 'file-a') {
        return { ok: false, reason: 'window_changed', strategy: 'cmd_v' }
      }
      const previous = mocks.clipboard
      mocks.clipboard = text
      mocks.postToTarget(text)
      if (protect) {
        mocks.clipboard = previous
        mocks.scheduledClipboardRestores++
      }
      return { ok: true, strategy: 'cmd_v' }
    })
  })

  it.each([true, false])('shows the result card without copying or inserting on mismatch (clipboard protect %s)', async (protect) => {
    const recorder = new RecorderOrchestrator()
    recorder['activeRunId'] = 1
    recorder['recordingFocus'] = { runId: 1, token: probe.focusToken }
    recorder['settings'].protectClipboard = protect
    mocks.target = 'file-b'

    try {
      await recorder['processFinalResult']({
        asrText: 'dictated result',
        llmText: 'dictated result',
        asrMs: 1,
        llmMs: 0,
        durationSec: 1,
      }, {
        runId: 1,
        timedOutAt: 0,
        settled: true,
        audioDurationSec: 1,
        wallTimeSec: 1,
        promptResolution: null,
        appContext: null,
        audioChunks: [],
        probeResult: probe,
      }, { allowInsertionWhenIdle: true, source: 'late_after_timeout' })

      expect(mocks.pasteText).toHaveBeenCalledWith('dictated result', '100', '100', protect, probe.focusToken)
      expect(mocks.presentOverlay).toHaveBeenCalledWith(expect.objectContaining({
        state: 'fallback',
        fallbackReason: 'window_changed',
        fallbackText: 'dictated result',
      }))
      expect(mocks.addHistory).toHaveBeenCalledWith(expect.objectContaining({ asrText: 'dictated result' }))
      expect(mocks.releaseRecordingFocus).toHaveBeenCalledWith(probe.focusToken)
      expect(mocks.postToTarget).not.toHaveBeenCalled()
      expect(mocks.copyText).not.toHaveBeenCalled()
      expect(mocks.clipboard).toBe('original clipboard')
      expect(mocks.scheduledClipboardRestores).toBe(0)
    } finally {
      recorder['overlayService'].dispose()
    }
  })

  it('uses the normal native clipboard protection option if the target stays the same', async () => {
    const recorder = new RecorderOrchestrator()
    recorder['activeRunId'] = 1
    recorder['recordingFocus'] = { runId: 1, token: probe.focusToken }
    recorder['settings'].protectClipboard = true
    try {
      await recorder['handleTextInsertion']('dictated result', { runId: 1, allowWhenIdle: true, probeResult: probe })
      expect(mocks.postToTarget).toHaveBeenCalledWith('dictated result')
      expect(mocks.clipboard).toBe('original clipboard')
      expect(mocks.scheduledClipboardRestores).toBe(1)
      expect(mocks.presentOverlay).not.toHaveBeenCalled()
    } finally {
      recorder['overlayService'].dispose()
    }
  })
})
