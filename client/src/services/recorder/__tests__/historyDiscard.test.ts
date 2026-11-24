import { beforeEach, describe, expect, it, vi } from 'vitest'
import { AudioArchiveService } from '../AudioArchiveService'
import { HistoryWriter } from '../HistoryWriter'

const mocks = vi.hoisted(() => ({
  addHistory: vi.fn(),
  deleteHistory: vi.fn(),
  deleteAudioFile: vi.fn(),
  addRuntimeEvent: vi.fn(),
}))

vi.mock('../../bridge', () => ({
  deleteAudioFile: mocks.deleteAudioFile,
  emit: vi.fn(async () => {}),
}))
vi.mock('../../store', () => ({
  addHistory: mocks.addHistory,
  deleteHistory: mocks.deleteHistory,
  getSetting: vi.fn(async (key: string, fallback: unknown) => key === 'historyEnabled' ? true : fallback),
}))
vi.mock('../../debugLog', () => ({ addRuntimeEvent: mocks.addRuntimeEvent }))

describe('discarding failed history', () => {
  const artifact = { recordId: 'entry', audioFilePath: 'recording.wav' }
  let audio: AudioArchiveService
  let writer: HistoryWriter

  beforeEach(() => {
    vi.resetAllMocks()
    audio = new AudioArchiveService(() => {})
    writer = new HistoryWriter(audio)
    mocks.deleteHistory.mockResolvedValue(undefined)
  })

  it('removes the history first and returns from write failure while recording deletion waits', async () => {
    vi.spyOn(audio, 'take').mockResolvedValue(artifact)
    mocks.addHistory.mockRejectedValue(new Error('history write failed'))
    let finishRemoval!: () => void
    mocks.deleteAudioFile.mockReturnValue(new Promise<void>(resolve => { finishRemoval = resolve }))

    try {
      await expect(writer.writeFinalRun({
        runId: 1,
        result: { asrText: 'text', llmText: 'text', asrMs: 0, llmMs: 0, durationSec: 1 },
        textToPaste: 'text',
        selectionNotApplied: false,
        audioDurationSec: 1,
        wallTimeSec: 1,
        mode: () => 'local',
        isRunCurrent: () => true,
        metadata: () => ({}),
      })).resolves.toMatchObject({ aborted: false })
      expect(mocks.deleteHistory).toHaveBeenCalledWith(artifact.recordId)
      expect(mocks.deleteAudioFile).toHaveBeenCalledWith(artifact.audioFilePath)
      expect(mocks.deleteHistory.mock.invocationCallOrder[0])
        .toBeLessThan(mocks.deleteAudioFile.mock.invocationCallOrder[0])
    } finally {
      finishRemoval()
    }
  })

  it('logs errors from both deletion operations', async () => {
    mocks.deleteHistory.mockRejectedValue(new Error('db busy'))
    mocks.deleteAudioFile.mockRejectedValue(new Error('file busy'))
    await writer.discard(artifact)
    await vi.waitFor(() => {
      for (const message of ['Failed to discard history entry', 'Failed to discard archived audio']) {
        expect(mocks.addRuntimeEvent).toHaveBeenCalledWith('warn', 'recorder', message, expect.any(Object))
      }
    })
  })
})
