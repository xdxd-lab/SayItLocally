import { useEffect, useMemo, useState, type ChangeEvent } from 'react'
import { AlertCircle, CheckCircle2, ChevronDown, Download, FileArchive, Image as ImageIcon, Info, RefreshCw, Send } from 'lucide-react'
import { Card, CardContent } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Tooltip } from '@/components/ui/tooltip'
import {
  getDiagnosticsPreview,
  MAX_DIAGNOSTIC_IMAGES,
  MAX_DIAGNOSTIC_IMAGE_SIZE,
  submitDiagnostics,
  downloadDiagnostics,
  validateDiagnosticImages,
} from '@/services/diagnostics'
import { getWorkMode } from '@/services/transcription'
import { save } from '@tauri-apps/plugin-dialog'
import * as bridge from '@/services/bridge'
import type { DiagnosticIssueType, DiagnosticOccurrence, DiagnosticsPreview } from '@/types/appApi'
import { t } from '@/i18n'
import { useLocale } from '@/i18n/useT'

interface DiagnosticsReportPanelProps {
  embedded?: boolean
}

function SummaryRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-start justify-between gap-4 border-b border-border/50 py-2 last:border-b-0 last:pb-0 first:pt-0">
      <span className="text-sm text-muted-foreground">{label}</span>
      <span className="max-w-[65%] break-all text-right text-sm text-foreground">{value}</span>
    </div>
  )
}

export default function DiagnosticsReportPanel({ embedded = false }: DiagnosticsReportPanelProps) {
  const locale = useLocale()
  const [description, setDescription] = useState('')
  const [issueType, setIssueType] = useState<DiagnosticIssueType | null>(null)
  // 預設「今天」而不是「1 小時內」：使用者往往過了一陣才來反饋，1 小時的視窗經常把
  // 出問題那段日誌切在外面，於是包裡乾乾淨淨什麼都沒有。
  const [issueOccurrence, setIssueOccurrence] = useState<DiagnosticOccurrence>('today')
  // 時間範圍、補充說明、截圖預設收起：預設路徑就該是「選一個問題 → 點傳送」兩步。
  const [showMore, setShowMore] = useState(false)
  // 打包內容預設收起：那張表（版本、平台、掃了幾個檔案、時間範圍…）是給我們看的。
  const [showContents, setShowContents] = useState(false)
  const [images, setImages] = useState<File[]>([])
  const [submitting, setSubmitting] = useState(false)
  const [downloading, setDownloading] = useState(false)
  const [status, setStatus] = useState<'idle' | 'success' | 'download_success' | 'error'>('idle')
  const [ticketId, setTicketId] = useState('')
  const [errorMessage, setErrorMessage] = useState('')
  const [preview, setPreview] = useState<DiagnosticsPreview | null>(null)
  const [loadingPreview, setLoadingPreview] = useState(false)

  const isServerMode = getWorkMode() === 'server'
  const imageValidation = useMemo(() => validateDiagnosticImages(images), [images, locale])
  const occurrenceOptions: Array<{ value: DiagnosticOccurrence; label: string }> = [
    { value: 'within_1h', label: t('diagnosticsReport.withinHour') },
    { value: 'today', label: t('diagnosticsReport.today') },
    { value: 'older', label: t('diagnosticsReport.older') },
  ]
  // 選型別代替寫描述：使用者遇到問題時不想寫作文，硬要求描述換來的多半是「不好用」
  // 這種幫不上忙的話。型別碼還會進 manifest，收到 ticket 就知道該看日誌哪一段。
  const issueTypeOptions: Array<{ value: DiagnosticIssueType; label: string }> = [
    { value: 'insert_failed', label: t('diagnosticsReport.issue.insertFailed') },
    { value: 'no_text', label: t('diagnosticsReport.issue.noText') },
    { value: 'wrong_text', label: t('diagnosticsReport.issue.wrongText') },
    { value: 'shortcut_dead', label: t('diagnosticsReport.issue.shortcutDead') },
    { value: 'overlay', label: t('diagnosticsReport.issue.overlay') },
    { value: 'ai_result', label: t('diagnosticsReport.issue.aiResult') },
    { value: 'crash', label: t('diagnosticsReport.issue.crash') },
    { value: 'update_failed', label: t('diagnosticsReport.issue.updateFailed') },
    { value: 'other', label: t('diagnosticsReport.issue.other') },
  ]

  useEffect(() => {
    let cancelled = false

    const loadPreview = async () => {
      setLoadingPreview(true)
      try {
        const nextPreview = await getDiagnosticsPreview(issueOccurrence)
        if (!cancelled) setPreview(nextPreview)
      } catch (error) {
        if (!cancelled) setErrorMessage(String(error))
      } finally {
        if (!cancelled) setLoadingPreview(false)
      }
    }

    void loadPreview()

    return () => {
      cancelled = true
    }
  }, [issueOccurrence])

  const handleImageSelect = (event: ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(event.target.files || [])
    const nextImages = [...images, ...files.filter((file) => file.type.startsWith('image/'))].slice(0, MAX_DIAGNOSTIC_IMAGES)
    setImages(nextImages)
    const validation = validateDiagnosticImages(nextImages)
    setErrorMessage(validation.valid ? '' : validation.errors[0])
  }

  const removeImage = (index: number) => {
    const nextImages = images.filter((_, imageIndex) => imageIndex !== index)
    setImages(nextImages)
    const validation = validateDiagnosticImages(nextImages)
    setErrorMessage(validation.valid ? '' : validation.errors[0])
  }

  const refreshPreview = async () => {
    setLoadingPreview(true)
    setErrorMessage('')
    try {
      setPreview(await getDiagnosticsPreview(issueOccurrence))
    } catch (error) {
      setErrorMessage(String(error))
    } finally {
      setLoadingPreview(false)
    }
  }

  const handleSubmit = async () => {
    const blocker = describeBlocker()
    if (blocker) {
      setErrorMessage(blocker)
      return
    }
    if (!imageValidation.valid) {
      setErrorMessage(imageValidation.errors[0] || t('diagnosticsReport.imageValidationFailed'))
      return
    }

    setSubmitting(true)
    setStatus('idle')
    setErrorMessage('')
    try {
      const ticket = await submitDiagnostics({
        description: description.trim(),
        issueType: issueType as DiagnosticIssueType,
        issueOccurrence,
        images,
      })
      setTicketId(ticket)
      setStatus('success')
      setDescription('')
      setIssueType(null)
      setImages([])
    } catch (error) {
      setStatus('error')
      setErrorMessage(String(error))
    } finally {
      setSubmitting(false)
    }
  }

  const handleDownload = async () => {
    const blocker = describeBlocker()
    if (blocker) {
      setErrorMessage(blocker)
      return
    }
    if (!imageValidation.valid) {
      setErrorMessage(imageValidation.errors[0] || t('diagnosticsReport.imageValidationFailed'))
      return
    }

    setDownloading(true)
    setStatus('idle')
    setErrorMessage('')
    let zipPath = '' // [自訂] S45：取消或出錯時要刪掉暫存 zip
    try {
      zipPath = await downloadDiagnostics({
        description: description.trim(),
        issueType: issueType as DiagnosticIssueType,
        issueOccurrence,
        images,
      })

      const dest = await save({
        defaultPath: `sayit-diagnostics-${new Date().toISOString().slice(0, 10)}.zip`,
        filters: [{ name: t('diagnosticsReport.archiveFilter'), extensions: ['zip'] }],
      })

      if (!dest) {
        // 使用者取消了儲存對話方塊
        await bridge.discardDiagnosticsZip(zipPath).catch(() => {}) // [自訂] S45
        setDownloading(false)
        return
      }

      await bridge.copyDiagnosticsZip(zipPath, dest)
      zipPath = '' // 複製成功後 Rust 已刪除來源
      setStatus('download_success')
      setDescription('')
      setIssueType(null)
      setImages([])
    } catch (error) {
      if (zipPath) await bridge.discardDiagnosticsZip(zipPath).catch(() => {}) // [自訂] S45
      setStatus('error')
      setErrorMessage(String(error))
    } finally {
      setDownloading(false)
    }
  }

  const containerClassName = embedded ? '' : 'mx-auto max-w-4xl p-8'
  const busy = submitting || downloading

  /**
   * 攔住提交的原因，沒有就返回空串。選了型別即可提交 —— 只有「其他」還需要一句話，
   * 因為那時我們連該看日誌哪一段都不知道。
   */
  function describeBlocker(): string {
    if (!issueType) return t('diagnosticsReport.issueTypeRequired')
    if (issueType === 'other' && !description.trim()) return t('diagnosticsReport.descriptionRequired')
    return ''
  }

  const blocker = describeBlocker()
  // 選了「其他」必須展開：那時描述是必填的，藏在摺疊裡會讓使用者對著一個點不動的
  // 傳送按鈕找不到原因。
  const moreOpen = showMore || issueType === 'other'
  const downloadBtn = (
    <Button variant="outline" size="sm" disabled={busy || Boolean(blocker)} onClick={handleDownload}>
      <Download className="mr-2 h-4 w-4" />
      {downloading ? t('diagnosticsReport.packing') : t('diagnosticsReport.download')}
    </Button>
  )
  const sendBtn = isServerMode ? (
    <Button size="sm" disabled={busy || Boolean(blocker)} onClick={handleSubmit}>
      <Send className="mr-2 h-4 w-4" />
      {submitting ? t('diagnosticsReport.sending') : t('diagnosticsReport.send')}
    </Button>
  ) : null

  return (
    <div className={containerClassName}>
      {!embedded && <h1 className="mb-6 text-2xl font-bold">{t('diagnostics.title')}</h1>}

      <Card>
        <CardContent className="p-6">
          <div className="mb-5 flex items-start justify-between gap-4">
            <div>
              <h2 className="text-lg font-semibold">{t('diagnosticsReport.title')}</h2>
              <p className="mt-1 text-sm text-muted-foreground">
                {t('diagnosticsReport.desc')}
              </p>
            </div>
            <Button variant="outline" size="sm" onClick={refreshPreview} disabled={loadingPreview}>
              <RefreshCw className={`mr-2 h-3.5 w-3.5 ${loadingPreview ? 'animate-spin' : ''}`} />
              {t('diagnosticsReport.refresh')}
            </Button>
          </div>

          <div className="space-y-5">
            <div>
              <label className="mb-2 block text-sm font-medium">
                {t('diagnosticsReport.issueType')}<span className="ml-0.5 text-red-500">*</span>
              </label>
              {/* 用 aria-pressed 的切換按鈕組，而不是 role="radiogroup" —— 後者的互動
                  契約是「Tab 進入組、方向鍵切換」，這裡沒實現方向鍵，宣告了反而會讓
                  讀屏使用者按方向鍵無反應。aria-pressed 的 Tab 逐個遍歷是自洽的。 */}
              <div className="flex flex-wrap gap-2">
                {issueTypeOptions.map((option) => {
                  const selected = issueType === option.value
                  return (
                    <button
                      key={option.value}
                      type="button"
                      aria-pressed={selected}
                      onClick={() => setIssueType(selected ? null : option.value)}
                      className={`rounded-full border px-2.5 py-1 text-xs transition-colors ${selected
                        ? 'border-foreground bg-foreground text-background'
                        : 'border-border text-muted-foreground hover:border-muted-foreground/50 hover:text-foreground'
                        }`}
                    >
                      {option.label}
                    </button>
                  )
                })}
              </div>
            </div>

            {/* 摺疊時右側摘要寫著當前時間範圍，所以不展開也知道要發的是哪段日誌。
                選了「其他」時強制展開（描述必填），此時按鈕置灰而不是點了沒反應。 */}
            <button
              type="button"
              disabled={issueType === 'other'}
              onClick={() => setShowMore((open) => !open)}
              aria-expanded={moreOpen}
              className="flex w-full items-center gap-2 text-left text-sm font-medium text-foreground disabled:cursor-default"
            >
              <span>{t('diagnosticsReport.moreOptions')}</span>
              <span className="ml-auto truncate text-xs font-normal text-muted-foreground">
                {occurrenceOptions.find((option) => option.value === issueOccurrence)?.label}
                {images.length > 0 ? t('diagnosticsReport.moreImagesSuffix', { count: images.length }) : ''}
              </span>
              <ChevronDown className={`h-4 w-4 shrink-0 text-muted-foreground transition-transform ${moreOpen ? '' : '-rotate-90'}`} />
            </button>

            {moreOpen && (<>
              <div>
                <label className="mb-2 block text-sm font-medium">{t('diagnosticsReport.when')}</label>
                <div className="flex flex-wrap gap-4">
                  {occurrenceOptions.map((option) => (
                    <button
                      key={option.value}
                      type="button"
                      onClick={() => setIssueOccurrence(option.value)}
                      className="flex items-center gap-2 text-sm text-foreground"
                    >
                      <span className={`flex h-4 w-4 items-center justify-center rounded-full border ${issueOccurrence === option.value ? 'border-foreground' : 'border-muted-foreground/40'
                        }`}>
                        <span className={`h-2.5 w-2.5 rounded-full ${issueOccurrence === option.value ? 'bg-foreground' : 'bg-transparent'
                          }`} />
                      </span>
                      <span>{option.label}</span>
                    </button>
                  ))}
                </div>
              </div>

              <div>
                <label className="mb-2 block text-sm font-medium">
                  {t('diagnosticsReport.description')}
                  <span className="ml-1 text-xs font-normal text-muted-foreground">
                    {issueType === 'other' ? t('diagnosticsReport.required') : t('diagnosticsReport.optional')}
                  </span>
                </label>
                <textarea
                  value={description}
                  onChange={(event) => setDescription(event.target.value)}
                  rows={2}
                  placeholder={t('diagnosticsReport.descriptionPlaceholder')}
                  className="min-h-[56px] max-h-[240px] w-full resize-y rounded-md border border-input-border bg-input-bg px-3 py-2 text-sm leading-relaxed focus:border-input-focus-border focus:outline-none"
                />
              </div>

              <div>
                <div className="mb-2 flex items-center justify-between">
                  <label className="block text-sm font-medium">{t('diagnosticsReport.screenshots')}</label>
                  <span className="text-xs text-muted-foreground">{t('diagnosticsReport.imageLimits', { count: MAX_DIAGNOSTIC_IMAGES })}</span>
                </div>

                {images.length > 0 && (
                  <div className="mb-3 grid grid-cols-2 gap-3 sm:grid-cols-5">
                    {images.map((image, index) => (
                      <div key={`${image.name}-${index}`} className="group relative overflow-hidden rounded-md border bg-muted">
                        <img src={URL.createObjectURL(image)} alt={image.name} className="aspect-square w-full object-cover" />
                        <button
                          type="button"
                          onClick={() => removeImage(index)}
                          className="absolute right-2 top-2 rounded-full bg-black/65 px-2 py-1 text-xs text-white opacity-0 transition-opacity group-hover:opacity-100"
                        >
                          {t('diagnosticsReport.delete')}
                        </button>
                        <div className="truncate px-2 py-2 text-xs text-muted-foreground">{image.name}</div>
                      </div>
                    ))}
                  </div>
                )}

                {images.length < MAX_DIAGNOSTIC_IMAGES && (
                  <label className="flex cursor-pointer items-center gap-2 rounded-md border border-dashed border-border bg-muted px-4 py-3 text-sm transition-colors hover:border-muted-foreground/40 hover:bg-accent">
                    <ImageIcon className="h-4 w-4 text-muted-foreground" />
                    <span className="text-muted-foreground">{t('diagnosticsReport.upload')}</span>
                    <input type="file" accept="image/*" multiple onChange={handleImageSelect} className="hidden" />
                  </label>
                )}
              </div>
            </>)}

            <div className="rounded-md border border-border bg-muted p-4">
              <button
                type="button"
                onClick={() => setShowContents((open) => !open)}
                aria-expanded={showContents}
                className="flex w-full items-center gap-2 text-left text-sm font-medium text-foreground"
              >
                <FileArchive className="h-4 w-4 shrink-0" />
                <span>{t('diagnosticsReport.contents')}</span>
                <span className="ml-auto truncate text-xs font-normal text-muted-foreground">
                  {preview
                    ? t('diagnosticsReport.summaryValue', {
                      events: preview.totalTimelineEntries,
                      errors: preview.summary.errors,
                      warnings: preview.summary.warnings,
                    })
                    : loadingPreview ? t('diagnosticsReport.loading') : t('diagnosticsReport.unavailable')}
                </span>
                <ChevronDown className={`h-4 w-4 shrink-0 text-muted-foreground transition-transform ${showContents ? '' : '-rotate-90'}`} />
              </button>

              {showContents && (preview ? (
                <div className="mt-3 rounded-md border border-border/50 bg-card p-4 shadow-sm">
                  <SummaryRow label={t('diagnosticsReport.appVersion')} value={preview.systemInfo.appVersion} />
                  <SummaryRow label={t('diagnosticsReport.platform')} value={preview.systemInfo.platform} />
                  <SummaryRow label={t('diagnosticsReport.time')} value={preview.generatedAt} />
                  <SummaryRow
                    label={t('diagnosticsReport.range')}
                    value={occurrenceOptions.find((option) => option.value === issueOccurrence)?.label || t('diagnosticsReport.withinHour')}
                  />
                  <SummaryRow label={t('diagnosticsReport.filesScanned')} value={t('diagnosticsReport.fileCount', { count: preview.filesScanned })} />
                  <SummaryRow label={t('diagnosticsReport.rangeStart')} value={preview.rangeStart || t('diagnosticsReport.none')} />
                  <SummaryRow label={t('diagnosticsReport.rangeEnd')} value={preview.rangeEnd || t('diagnosticsReport.none')} />
                  <SummaryRow label={t('diagnosticsReport.imageCount')} value={t('diagnosticsReport.images', { count: images.length })} />
                  <SummaryRow
                    label={t('diagnosticsReport.summary')}
                    value={t('diagnosticsReport.summaryValue', {
                      events: preview.totalTimelineEntries,
                      errors: preview.summary.errors,
                      warnings: preview.summary.warnings,
                    })}
                  />
                </div>
              ) : (
                <div className="mt-3 text-sm text-muted-foreground">
                  {loadingPreview ? t('diagnosticsReport.loading') : t('diagnosticsReport.unavailable')}
                </div>
              ))}
            </div>

            {(errorMessage || !imageValidation.valid) && (
              <div className="flex items-start gap-2 rounded-md bg-destructive/10 p-3 text-sm">
                <AlertCircle className="mt-0.5 h-4 w-4 shrink-0 text-destructive" />
                <div className="text-destructive">{errorMessage || imageValidation.errors[0]}</div>
              </div>
            )}

            {status === 'success' && (
              <div className="flex items-start gap-2 rounded-md bg-success/10 p-3 text-sm">
                <CheckCircle2 className="mt-0.5 h-4 w-4 shrink-0 text-success" />
                <div>
                  <div className="font-medium text-success">{t('diagnosticsReport.sent')}</div>
                  <div className="mt-1 text-xs text-success/80">{t('diagnosticsReport.ticket', { id: ticketId })}</div>
                </div>
              </div>
            )}

            {status === 'download_success' && (
              <div className="flex items-start gap-2 rounded-md bg-success/10 p-3 text-sm">
                <CheckCircle2 className="mt-0.5 h-4 w-4 shrink-0 text-success" />
                <div>
                  <div className="font-medium text-success">{t('diagnosticsReport.saved')}</div>
                  <div className="mt-1 text-xs text-success/80">{t('diagnosticsReport.sendToSupport')}</div>
                </div>
              </div>
            )}

            {/* 非伺服器模式沒有「傳送」按鈕 —— 不解釋的話使用者只會以為按鈕壞了。
                選了問題型別才提示：一進頁面就掛一條提示純屬噪音。 */}
            {!isServerMode && issueType && (
              <div className="flex items-start gap-2 rounded-md border border-border bg-muted/40 px-3 py-2 text-xs leading-relaxed text-muted-foreground">
                <Info className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                <span>{t('diagnosticsReport.sendNeedsServerMode')}</span>
              </div>
            )}

            <div className="flex justify-end gap-2">
              <Button
                variant="outline"
                size="sm"
                disabled={busy}
                onClick={() => {
                  setDescription('')
                  setIssueType(null)
                  setImages([])
                  setStatus('idle')
                  setErrorMessage('')
                }}
              >
                {t('diagnosticsReport.clear')}
              </Button>
              {blocker ? <Tooltip content={blocker}>{downloadBtn}</Tooltip> : downloadBtn}
              {sendBtn && (blocker ? <Tooltip content={blocker}>{sendBtn}</Tooltip> : sendBtn)}
            </div>
          </div>
        </CardContent>
      </Card>

      <div className="mt-4 text-xs text-muted-foreground">
        {t('diagnosticsReport.validationStatus', {
          status: imageValidation.valid
            ? t('diagnosticsReport.validationPassed')
            : imageValidation.errors[0] || t('diagnosticsReport.validationFailed'),
          limit: (MAX_DIAGNOSTIC_IMAGE_SIZE / 1024 / 1024).toFixed(0),
        })}
      </div>
    </div>
  )
}
