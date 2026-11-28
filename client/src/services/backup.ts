// 備份 / 恢復：配置（JSON）與全部資料（zip，含音訊）的匯入匯出。
//
// 配置匯出支援完整配置或熱片語、文本替換、潤色模式的自定義選擇。
// 配置匯入先由 Rust 識別檔案並生成變更預覽，再由使用者二次確認。
// 全量匯出與匯入保持原有覆蓋語義；匯入完成後重啟應用使執行時狀態過載。

import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { open } from '@tauri-apps/plugin-dialog'
import type { PromptPreset } from './store'
import type { TextReplacementRule } from './textReplacement'
import { t } from '@/i18n'

export function describeBackupTransferError(error: unknown): string {
  const raw = error instanceof Error ? error.message : String(error)
  const [code, category, row] = raw.split('|')
  switch (code) {
    case 'BACKUP_AUDIO_DIRECTORY_UNSAFE':
      return t('backup.audioDirectoryUnsafe')
    case 'BACKUP_AUDIO_DESTINATION_UNSAFE':
      return t('backup.audioDestinationUnsafe')
    case 'BACKUP_READ_DATABASE':
      return t('backup.readDatabase')
    case 'BACKUP_READ_SETTINGS':
      return category ? t('backup.readSetting', { key: category }) : t('backup.readSettings')
    case 'BACKUP_READ_COLLECTION':
      return row
        ? t('backup.readCollectionRow', { category, row })
        : t('backup.readCollection', { category })
    case 'BACKUP_READ_RECORDINGS':
      return t('backup.readRecordings')
    case 'BACKUP_RECORDING_NAME_CONFLICT':
      return t('backup.recordingNameConflict')
    case 'BACKUP_RECORDING_CHANGED':
      return t('backup.recordingChanged')
    case 'BACKUP_EXPORT_IO':
      return t('backup.writeFile')
    case 'BACKUP_EXPORT_EXISTS':
      return t('backup.fileExists')
    case 'BACKUP_VOLATILE_EXPORT':
      return t('backup.volatileExport')
    case 'BACKUP_VOLATILE_STORAGE':
      return t('backup.volatileStorage')
    case 'BACKUP_RESTORE_DB':
      return t('backup.restoreDatabase')
    case 'BACKUP_NOTHING_SELECTED':
      return t('configTransfer.nothingSelected')
    default:
      return raw
  }
}

export interface ExportResult {
  canceled: boolean
  filePath: string | null
}

export type ConfigExportSelection =
  | { mode: 'full' }
  | {
    mode: 'selected'
    hotwordGroupIds: string[]
    includeTextReplacements: boolean
    defaultTextReplacements?: TextReplacementRule[]
    promptPresetIds: string[]
  }

export interface ConfigExportChoices {
  hotwordThemes: unknown
  textReplacements?: TextReplacementRule[] | null
  promptPresets: PromptPreset[]
}

export function inspectConfigExport(): Promise<ConfigExportChoices> {
  return invoke('inspect_config_export')
}

export interface ConfigImportSectionPreview {
  kind: string
  total: number
  added: number
  updated: number
  skipped: number
}

export interface ConfigImportWarning {
  code: 'hotwordLimit' | 'fullOverwrite' | string
  current: number | null
  limit: number | null
}

export interface ConfigImportPreview {
  scope: 'full' | 'selected'
  formatVersion: number
  importToken: string
  sections: ConfigImportSectionPreview[]
  warnings: ConfigImportWarning[]
  requiresRestart: boolean
}

export interface ConfigImportResult {
  changedSections: string[]
  added: number
  updated: number
  skipped: number
  requiresRestart: boolean
}

export interface BackupExportProgress {
  status: 'running' | 'completed' | 'failed'
  phase: 'preparing' | 'packingData' | 'packingAudio' | 'finalizing' | 'completed' | 'failed'
  filePath: string
  currentFile: string | null
  processedFiles: number
  totalFiles: number
  processedBytes: number
  totalBytes: number
  percent: number
  error: string | null
}

export function getBackupDirectory(): Promise<string> {
  return invoke<string>('get_backup_directory')
}

export function onBackupExportProgress(
  handler: (progress: BackupExportProgress) => void,
): Promise<UnlistenFn> {
  return listen<BackupExportProgress>('backup-export-progress', (event) => handler(event.payload))
}

/** 匯出完整配置，或匯出選中的熱片語、文本替換和自定義潤色模式。 */
export async function exportConfigFile(selection: ConfigExportSelection): Promise<ExportResult> {
  const path = await invoke<string>('export_config', { selection })
  return { canceled: false, filePath: path }
}

/**
 * 本地「全部資料」匯出：配置 + 歷史 + 錄音，直接寫入預設備份目錄。
 *
 * scope 顯式傳全 true —— Rust 側刻意不給這兩個欄位 serde 預設值，漏傳就反序列化失敗。
 * 寧可報一條看得見的錯，也不要讓「預設全都要」在 WebDAV 自動備份那邊變成
 * 每天悄悄上傳幾個 GB 音訊。
 */
export async function exportFullFile(): Promise<ExportResult> {
  const path = await invoke<string>('export_full', {
    scope: { includeHistory: true, includeAudio: true },
  })
  return { canceled: false, filePath: path }
}

/** 讓使用者選擇要匯入的備份檔案；返回路徑，取消則返回 null。檔案選擇走系統原生對話方塊。 */
export async function pickImportFile(kind: 'config' | 'full'): Promise<string | null> {
  const filters =
    kind === 'config'
      ? [{ name: t('configTransfer.fileConfig'), extensions: ['json'] }]
      : [{ name: t('configTransfer.fileBackup'), extensions: ['zip'] }]
  const picked = await open({ multiple: false, directory: false, filters })
  return typeof picked === 'string' ? picked : null
}

/** 檢查配置檔案並按當前本地資料計算新增、更新和跳過數量，不寫入任何資料。 */
export function inspectConfigImport(inPath: string): Promise<ConfigImportPreview> {
  return invoke<ConfigImportPreview>('inspect_config_import', { inPath })
}

/** 執行已確認的匯入；配置返回合併結果，全部資料保持原有流程。 */
export async function runImport(
  kind: 'config' | 'full',
  inPath: string,
  expectedImportToken?: string,
): Promise<ConfigImportResult | null> {
  if (kind === 'config') {
    if (!expectedImportToken) throw new Error(t('configTransfer.missingConfirmation'))
    return invoke<ConfigImportResult>('import_config', { inPath, expectedImportToken })
  }
  await invoke('import_full', { inPath })
  return null
}

/** 重啟應用（Tauri 內建 app.restart()）。 */
export function restartApp(): Promise<void> {
  return invoke('restart_app')
}
