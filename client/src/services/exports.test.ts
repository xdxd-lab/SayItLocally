import { expect, it, vi } from 'vitest'
// 專案沒裝 @types/node，fixture 用 Vite 的 `?raw` 讀入。
import fixture from '../../src-tauri/src/commands/testdata/history-export.csv?raw'
import { describeExportError, exportHistory, historyToCsv } from './exports'
import type { HistoryRecord } from './store'

const mocks = vi.hoisted(() => ({
  confirm: vi.fn(async () => false),
  saveTextExport: vi.fn(),
  listHistory: vi.fn(async () => [{ id: 'favorite' }]),
  countHistory: vi.fn(async () => 10),
}))

vi.mock('@tauri-apps/plugin-dialog', () => ({ confirm: mocks.confirm }))
vi.mock('./bridge', () => ({ saveTextExport: mocks.saveTextExport }))
vi.mock('./store', () => ({ listHistory: mocks.listHistory, countHistory: mocks.countHistory }))
vi.mock('./api', () => ({}))
vi.mock('./personalization/store', () => ({}))
vi.mock('@/i18n', () => ({
  t: (key: string, params?: { count: number }) => params ? `${key} ${params.count}` : key,
}))

it('confirms the filtered snapshot count and does not write when canceled', async () => {
  await expect(exportHistory({ favoriteOnly: true }, 'csv'))
    .resolves.toEqual({ canceled: true, filePath: null })
  expect(mocks.listHistory).toHaveBeenCalledWith({ favoriteOnly: true })
  expect(mocks.confirm).toHaveBeenCalledWith('history.exportConfirmCount 1', expect.any(Object))
  expect(mocks.saveTextExport).not.toHaveBeenCalled()
})

it('explains only the in-memory export refusal and leaves other errors to the caller', () => {
  expect(describeExportError('BACKUP_VOLATILE_EXPORT')).toBe('backup.volatileExport')
  expect(describeExportError(new Error('BACKUP_VOLATILE_EXPORT'))).toBe('backup.volatileExport')
  expect(describeExportError(new Error('disk full'))).toBeNull()
})

// [自訂] SC-59：與 Rust 的 CSV 去路徑測試共用同一份 fixture；欄名或寫法一改，兩邊都會失敗。
it('writes history CSV exactly as the shared Rust fixture expects', () => {
  const records = [
    {
      id: 'r1', timestamp: 1700000000000, favorite: true, appName: '/Applications/Notes.app',
      durationSec: 3, charCount: 10, asrText: 'Open /Volumes/Work/a.txt, "quoted"\nline2', llmText: '',
      promptPresetName: 'Default', promptSummary: '%APPDATA%\\SayIt\\p.txt', styleSummary: 'casual',
    },
    {
      id: 'r2', timestamp: 1700000001000, favorite: false, appName: 'Notes',
      durationSec: 4, charCount: 7, asrText: '~/notes', llmText: 'D:\\Data\\someone\\x',
      promptPresetName: '', promptSummary: '/Volumes/My "Disk"/p.txt', styleSummary: '\\\\server\\share\\s',
    },
  ] as unknown as HistoryRecord[]
  expect(historyToCsv(records)).toBe(fixture)
})
