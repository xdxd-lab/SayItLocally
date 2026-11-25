import { describe, expect, it, vi } from 'vitest'
import { t } from '@/i18n'
import { describeBackupTransferError, exportConfigFile, inspectConfigExport, type ConfigExportSelection } from './backup'
import { describeWebDavError } from './webdavBackup'

describe('backup transfer errors', () => {
  it('keeps the collection and row number in the localized read error', () => {
    const message = t('backup.readCollectionRow', { category: 'feedbackQueue', row: 2 })
    expect(describeBackupTransferError('BACKUP_READ_COLLECTION|feedbackQueue|2')).toBe(message)
    expect(describeWebDavError('BACKUP_READ_COLLECTION|feedbackQueue|2')).toBe(message)
  })

  it('localizes restore and file errors and preserves unrecognized details', () => {
    expect(describeBackupTransferError(new Error('BACKUP_RESTORE_DB|commit'))).toBe(t('backup.restoreDatabase'))
    expect(describeBackupTransferError('BACKUP_EXPORT_EXISTS')).toBe(t('backup.fileExists'))
    expect(describeBackupTransferError('Unrecognized error')).toBe('Unrecognized error')
  })
})


const invoke = vi.hoisted(() => vi.fn(async () => '/backup.json'))
vi.mock('@tauri-apps/api/core', () => ({ invoke }))

it('uses the strict preview endpoint and sends only defaults for selected replacement rules', async () => {
  await inspectConfigExport()
  expect(invoke).toHaveBeenCalledWith('inspect_config_export')
  const selection: ConfigExportSelection = {
    mode: 'selected',
    hotwordGroupIds: [],
    includeTextReplacements: true,
    defaultTextReplacements: [{ id: 'default', from: 'first', to: 'second', enabled: true }],
    promptPresetIds: [],
  }
  await exportConfigFile(selection)
  expect(invoke).toHaveBeenLastCalledWith('export_config', { selection })
})
