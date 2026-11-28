import * as bridge from './bridge'
import { getBackendBaseUrl } from './runtimeConfig'
import { sanitizeObject } from '@/lib/sanitize'
import type { DiagnosticIssueType, DiagnosticOccurrence, DiagnosticsPreview } from '@/types/appApi'
import { t } from '@/i18n'

export const MAX_DIAGNOSTIC_IMAGES = 5
export const MAX_DIAGNOSTIC_IMAGE_SIZE = 5 * 1024 * 1024
export const MAX_DIAGNOSTIC_TOTAL_IMAGE_SIZE = 20 * 1024 * 1024

export interface DiagnosticsSubmission {
  /** 使用者的補充說明。選了問題型別後它就是可選的 —— 逼使用者寫作文只會換來「不好用」。 */
  description: string
  issueType: DiagnosticIssueType
  issueOccurrence: DiagnosticOccurrence
  images: File[]
}

export interface DiagnosticsValidationResult {
  valid: boolean
  errors: string[]
}

export function validateDiagnosticImages(images: File[]): DiagnosticsValidationResult {
  const errors: string[] = []

  if (images.length > MAX_DIAGNOSTIC_IMAGES) {
    errors.push(t('diagnosticsReport.maxImages', { count: MAX_DIAGNOSTIC_IMAGES }))
  }

  const totalSize = images.reduce((sum, image) => sum + image.size, 0)
  if (totalSize > MAX_DIAGNOSTIC_TOTAL_IMAGE_SIZE) {
    errors.push(t('diagnosticsReport.maxTotalSize'))
  }

  for (const image of images) {
    if (!image.type.startsWith('image/')) {
      errors.push(t('diagnosticsReport.notImage', { name: image.name }))
    }
    if (image.size > MAX_DIAGNOSTIC_IMAGE_SIZE) {
      errors.push(t('diagnosticsReport.imageTooLarge', { name: image.name }))
    }
  }

  return {
    valid: errors.length === 0,
    errors,
  }
}

export async function getDiagnosticsPreview(issueOccurrence: DiagnosticOccurrence): Promise<DiagnosticsPreview> {
  const settings = await bridge.collectSettings()
  const preview = await bridge.getDiagnosticsPreview({
    settings: settings || {},
    issueOccurrence,
  })

  if (!preview) {
    throw new Error('Failed to collect diagnostics preview')
  }

  return preview
}

/** 打包診斷：截圖以原始位元組發給 Rust（不再轉成 JSON 數字陣列）。 */
async function buildDiagnosticsZip(data: DiagnosticsSubmission): Promise<string> {
  const rawSettings = await bridge.collectSettings()
  const settings = sanitizeObject(rawSettings || {})
  const images = await Promise.all(
    data.images.map(async (file) => ({
      name: file.name,
      type: file.type,
      bytes: new Uint8Array(await file.arrayBuffer()),
    })),
  )
  return bridge.createDiagnosticsZipRaw({
    description: data.description,
    issueType: data.issueType,
    settings: settings || {},
    issueOccurrence: data.issueOccurrence,
  }, images)
}

export async function submitDiagnostics(data: DiagnosticsSubmission): Promise<string> {
  const validation = validateDiagnosticImages(data.images)
  if (!validation.valid) {
    throw new Error(validation.errors.join('\n'))
  }

  const zipPath = await buildDiagnosticsZip(data)

  if (!zipPath) {
    throw new Error('Failed to create diagnostics zip')
  }

  // [自訂] S45：上傳已停用，不論結果都刪掉暫存 zip，避免殘留在 App 資料夾。
  try {
    return await uploadDiagnosticsZip(zipPath)
  } finally {
    await bridge.discardDiagnosticsZip(zipPath).catch(() => {})
  }
}

export async function downloadDiagnostics(data: DiagnosticsSubmission): Promise<string> {
  const validation = validateDiagnosticImages(data.images)
  if (!validation.valid) {
    throw new Error(validation.errors.join('\n'))
  }

  const zipPath = await buildDiagnosticsZip(data)

  if (!zipPath) {
    throw new Error('Failed to create diagnostics zip')
  }

  return zipPath
}

async function uploadDiagnosticsZip(zipPath: string): Promise<string> {
  // [自訂] 診斷上傳已停用
  void zipPath
  throw new Error('Diagnostics upload is disabled in this build')
}

export async function _disabledUploadDiagnosticsZip(zipPath: string): Promise<string> {
  const zipData = await bridge.readDiagnosticsZipBytes(zipPath)

  if (!zipData) {
    throw new Error('Failed to read diagnostics zip')
  }

  const formData = new FormData()
  const blob = new Blob([zipData], { type: 'application/zip' })
  formData.append('diagnostics', blob, 'diagnostics.zip')

  const response = await fetch(`${getBackendBaseUrl()}/api/diagnostics`, {
    method: 'POST',
    body: formData,
  })

  if (!response.ok) {
    const message = await response.text().catch(() => '')
    throw new Error(`Upload failed: ${response.status} ${message}`.trim())
  }

  const result = await response.json()
  return result.ticket_id
}
