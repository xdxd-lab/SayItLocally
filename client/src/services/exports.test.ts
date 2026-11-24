import { expect, it, vi } from 'vitest'
import { exportHistory } from './exports'

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
