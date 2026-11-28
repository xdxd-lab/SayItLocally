import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { Trash2, VolumeX, Star, Play, Pause, RotateCcw, Loader2, Download, Copy, X, FolderOpen, Pencil, ChevronDown, ChevronUp, SpellCheck } from 'lucide-react'
import { Tooltip } from '@/components/ui/tooltip'
import { SuccessCheck } from '@/components/ui/success-check'
import { Card, CardContent } from '@/components/ui/card'
import { type HistoryRecord } from '@/services/store'
import * as bridge from '@/services/bridge'
import { pickVoiceDurationSec } from '@/services/timeModel'
import { invoke } from '@tauri-apps/api/core'
import { getLocale, t } from '@/i18n'
import { useT } from '@/i18n/useT'
import { historyFailureReasonDisplay } from '@/i18n/displayNames'
import { describeLocalModelError, errorDetailText } from '@/lib/errorMessages'
import { addRuntimeEvent } from '@/services/debugLog'
import { useRecordingPlayback } from './useRecordingPlayback'
import { AudioProgressBar } from './AudioProgressBar'
import { AsrCorrectionDialog } from './AsrCorrectionDialog'
import { groupByDay } from './dayGroups'

/** 雲 API 內部 key → 使用者友好的模型 ID */
const ASR_PROVIDER_DISPLAY: Record<string, string> = {
  doubao_v2: 'Doubao-Seed-ASR-2.0',
  doubao: 'Doubao-Seed-ASR',
  qwen: 'qwen3-asr-flash',
  qwen_omni_35_plus: 'qwen3.5-omni-plus-realtime',
  qwen_omni_35_flash: 'qwen3.5-omni-flash-realtime',
  qwen_omni_flash: 'qwen3-omni-flash-realtime',
  qwen_omni_turbo: 'qwen-omni-turbo-realtime',
  qwen_omni_plus: 'qwen3.5-omni-plus-realtime',
}

interface HistoryRecordListProps {
  records: HistoryRecord[]
  onDelete: (id: string) => Promise<void> | void
  onToggleFavorite?: (id: string, nextFavorite: boolean) => Promise<void> | void
  onReprocess?: (record: HistoryRecord) => Promise<void> | void
  /** 手工編輯轉換結果並儲存 */
  onEdit?: (id: string, nextText: string) => Promise<void> | void
  /** 記下「這條的 ASR 糾錯已提交/已撤回」，由頁面寫回本地記錄 */
  onSaveAsrCorrection?: (id: string, patch: Partial<HistoryRecord>) => Promise<void> | void
  emptyText?: string
  /** 搜尋關鍵詞：在正文與 ASR 原文裡高亮命中處 */
  highlight?: string
}

function getDayLabel(ts: number): string {
  const now = new Date()
  const date = new Date(ts)
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate())
  const yesterday = new Date(today)
  yesterday.setDate(yesterday.getDate() - 1)
  const recordDay = new Date(date.getFullYear(), date.getMonth(), date.getDate())

  // 日期格式跟介面語言：英文介面下 "August 4" 而不是 "8月4日"
  const dateStr = date.toLocaleDateString(getLocale(), { month: 'long', day: 'numeric' })
  if (recordDay.getTime() === today.getTime()) return t('record.dayPrefix', { day: t('record.today'), date: dateStr })
  if (recordDay.getTime() === yesterday.getTime()) return t('record.dayPrefix', { day: t('record.yesterday'), date: dateStr })
  return date.toLocaleDateString(getLocale(), { year: 'numeric', month: 'long', day: 'numeric' })
}

function formatTime(ts: number): string {
  const d = new Date(ts)
  return d.toLocaleTimeString(getLocale(), { hour: '2-digit', minute: '2-digit' })
}

/** 把 text 中命中 keyword 的子串用 <mark> 高亮（大小寫不敏感，用 indexOf 避免正則特殊字元問題）。 */
function highlightText(text: string, keyword: string) {
  const kw = keyword.trim()
  if (!kw) return text
  const lower = text.toLowerCase()
  const kwLower = kw.toLowerCase()
  const parts: Array<string | JSX.Element> = []
  let from = 0
  let idx = lower.indexOf(kwLower, from)
  let key = 0
  while (idx !== -1) {
    if (idx > from) parts.push(text.slice(from, idx))
    parts.push(
      <mark key={key++} className="rounded-sm bg-amber-200/70 px-0.5 text-inherit dark:bg-amber-500/30">
        {text.slice(idx, idx + kw.length)}
      </mark>,
    )
    from = idx + kw.length
    idx = lower.indexOf(kwLower, from)
  }
  if (from < text.length) parts.push(text.slice(from))
  return parts
}

function HistoryItem({
  record,
  onDelete,
  onToggleFavorite,
  onReprocess,
  onEdit,
  onSaveAsrCorrection,
  highlight = '',
}: {
  record: HistoryRecord
  onDelete: () => void
  onToggleFavorite?: (nextFavorite: boolean) => void
  onReprocess?: () => Promise<void> | void
  onEdit?: (nextText: string) => Promise<void> | void
  onSaveAsrCorrection?: (patch: Partial<HistoryRecord>) => Promise<void> | void
  highlight?: string
}) {
  // 詳情區靠**點「展開詳情」按鈕**開合，不用 hover。
  //
  // 曾經改成過滑鼠懸停展開（進入停留 160ms 才開、離開延遲 180ms 才收，編輯/播放/
  // 鍵盤焦點時粘滯不收）。那套邏輯本身是能用的，但被撤掉了：hover 展開意味著
  // 游標只是路過列表，行高就會變化，讀一屏歷史時整個列表始終在動；而"展開某條看細節"
  // 本來就是個明確的意圖，值得使用者點一下。
  // 別再改回 hover —— 那條路已經走過一次了。
  const [expanded, setExpanded] = useState(false)
  const [editing, setEditing] = useState(false)
  const [editText, setEditText] = useState('')
  const [reprocessing, setReprocessing] = useState(false)
  const [reprocessError, setReprocessError] = useState('')
  const [downloading, setDownloading] = useState(false)
  const [downloadStatus, setDownloadStatus] = useState<'idle' | 'ok' | 'fail'>('idle')
  const [downloadPath, setDownloadPath] = useState('')
  const [copied, setCopied] = useState(false)
  const [correcting, setCorrecting] = useState(false)
  const playback = useRecordingPlayback(record.audioFilePath)

  const text = record.llmText || record.asrText
  const isEmpty = record.isEmpty || (!text && record.charCount === 0)
  const voiceDurationSec = pickVoiceDurationSec({
    holdSec: record.durationSec,
    audioSec: record.audioDurationSec,
    asrSec: record.asrDurationSec,
  })

  /**
   * 「糾正識別」入口的門檻。
   *
   * 只有伺服器模式：只有伺服器上的模型是我們能改的，雲 API / 本地模型認錯了，
   * 把使用者音訊收上來既沒用又是一次隱私意外（見 .kiro/decisions.md）。
   * 必須有錄音路徑：沒有音訊的糾正對改進識別沒有用。路徑存在但檔案已被保留期清掉
   * 這種情況留給面板去說明 —— 列表渲染時不該為每一行去摸一次磁碟。
   */
  const canCorrectAsr = Boolean(
    onSaveAsrCorrection && record.workMode === 'server' && record.asrText && !isEmpty && record.audioFilePath,
  )
  const asrCorrectionSubmitted = Boolean(record.asrCorrectionId)

  const handleReprocess = useCallback(async () => {
    if (!onReprocess || reprocessing) return
    setReprocessing(true)
    setReprocessError('')
    try {
      await onReprocess()
    } catch (error) {
      // 原來沒有 catch：失敗只在日誌裡留一行 Unhandled promise rejection，按鈕轉完就沒下文，
      // 使用者以為點了沒反應（2026-10-19 本地模型把顯示卡跑掛那次，連著點了十來次）。
      const reason = errorDetailText(error)
      // [自訂] SC-35：本機模型錯誤使用與聽寫及語音引擎測試相同的提示。
      const modelError = describeLocalModelError(error)
      setReprocessError(modelError ? `${modelError.message} ${reason}` : reason)
      addRuntimeEvent('warn', 'history', 'Transcribe again failed', { recordId: record.id, error: reason })
    } finally {
      setReprocessing(false)
    }
  }, [onReprocess, reprocessing, record.id])

  // ── 編輯：自動儲存（失焦 / 離開頁面即存，無需按鈕）──
  // 用 ref 儲存最新值，供失焦回撥與解除安裝清理讀取，避免閉包拿到舊值。
  const editTextRef = useRef('')
  const editingRef = useRef(false)
  const textRef = useRef(text)
  const onEditRef = useRef(onEdit)
  textRef.current = text
  onEditRef.current = onEdit

  const startEdit = useCallback(() => {
    editTextRef.current = text
    editingRef.current = true
    setEditText(text)
    setEditing(true)
  }, [text])

  // 提交：僅當仍在編輯且內容有變化時寫回。冪等——重複呼叫（失焦後又解除安裝）不會重複儲存。
  const commitEdit = useCallback(async () => {
    if (!editingRef.current) return
    editingRef.current = false
    setEditing(false)
    const next = editTextRef.current
    if (!onEditRef.current || next === textRef.current) return
    await onEditRef.current(next)
  }, [])

  // 取消：不儲存，直接退出（後續失焦/解除安裝因 editingRef=false 而跳過）。
  const cancelEdit = useCallback(() => {
    editingRef.current = false
    setEditing(false)
  }, [])

  // 離開頁面（元件解除安裝）時若仍在編輯，自動儲存。
  useEffect(() => () => { void commitEdit() }, [commitEdit])

  const handleDownloadAudio = useCallback(async () => {
    if (!record.audioFilePath || downloading) return
    setDownloading(true)
    setDownloadStatus('idle')
    setDownloadPath('')
    try {
      if (!(await bridge.audioFileExists(record.audioFilePath))) {
        setDownloadStatus('fail')
        setDownloadPath(t('record.audioMissing'))
        setTimeout(() => setDownloadStatus('idle'), 3000)
        return
      }

      const ts = new Date(record.timestamp)
      const dateStr = `${ts.getFullYear()}${String(ts.getMonth() + 1).padStart(2, '0')}${String(ts.getDate()).padStart(2, '0')}_${String(ts.getHours()).padStart(2, '0')}${String(ts.getMinutes()).padStart(2, '0')}${String(ts.getSeconds()).padStart(2, '0')}`
      const filename = `sayit_${dateStr}.wav`

      // Rust 直接複製檔案，不再把整段錄音讀成 base64 再發回去
      const savedPath = await bridge.saveRecordingToDownloads(record.audioFilePath, filename)

      setDownloadStatus('ok')
      setDownloadPath(savedPath)
      setTimeout(() => setDownloadStatus('idle'), 5000)
    } catch (err) {
      setDownloadStatus('fail')
      setDownloadPath(String(err))
      setTimeout(() => setDownloadStatus('idle'), 3000)
    } finally {
      setDownloading(false)
    }
  }, [record.audioFilePath, record.timestamp, downloading])

  const open = expanded

  return (
    <div
      className="group rounded-md transition-colors hover:bg-accent/50"
      // 關掉滾動錨定：Chromium 預設會在行高變化時自動調 scrollTop 去"穩住"某個錨點元素，
      // 錨點若落在展開行下方，補償的結果就是內容（含你剛點開的這行）往上躥 ——
      // 表現為"有時往上、有時往下，擠來擠去"。關掉之後展開一律向下推，行為可預期。
      style={{ overflowAnchor: 'none' }}
    >
      <div className="flex items-start gap-2 px-2 py-2">
        <span className="w-12 shrink-0 pt-0.5 text-xs text-muted-foreground">
          {formatTime(record.timestamp)}
        </span>

        <div className="grid min-w-0 flex-1 grid-cols-[minmax(0,1fr)_auto] gap-x-2">
          <div className="min-w-0">
            {editing ? (
              <div onClick={(e) => e.stopPropagation()}>
                <textarea
                  value={editText}
                  onChange={(e) => { setEditText(e.target.value); editTextRef.current = e.target.value }}
                  autoFocus
                  rows={Math.min(Math.max(editText.split('\n').length, 2), 12)}
                  className="w-full resize-y rounded-md border border-input-border bg-input-bg px-2.5 py-1.5 text-sm leading-relaxed focus:border-input-focus-border focus:outline-none"
                  onBlur={() => { void commitEdit() }}
                  onKeyDown={(e) => {
                    // Esc 取消（不保存）；Ctrl/Cmd+Enter 立即保存（失焦触发提交）
                    if (e.key === 'Escape') { e.preventDefault(); cancelEdit() }
                    if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') { e.preventDefault(); (e.target as HTMLTextAreaElement).blur() }
                  }}
                />
                <span className="mt-1 inline-block text-[11px] text-muted-foreground/50">{t('record.editHint')}</span>
              </div>
            ) : isEmpty ? (
              <div className="flex items-center gap-2">
                <VolumeX className="h-3.5 w-3.5 text-muted-foreground/40" />
                <p className="text-sm italic text-muted-foreground/60">{t('record.noSpeech')}</p>
              </div>
            ) : (
              <div
                className="cursor-pointer text-sm leading-relaxed text-foreground/75 select-text transition-colors hover:text-foreground"
                onClick={() => {
                  const selection = window.getSelection()
                  if (selection && selection.toString().trim()) return
                  void bridge.copyText(text).then(() => {
                    setCopied(true)
                    setTimeout(() => setCopied(false), 1500)
                  })
                }}
              >
                {(() => {
                  // 編輯過的記錄：在正文末尾內聯一個小鉛筆圖示（hover 提示「已編輯」），不佔額外行、不混入正文文本
                  const editedMark = record.manualEditedAt ? (
                    <Tooltip content={t('record.edited')}>
                      <Pencil className="ml-1 inline-block h-3 w-3 translate-y-[1px] text-muted-foreground/40" />
                    </Tooltip>
                  ) : null
                  if (text.includes('\n')) {
                    const paras = text.split(/\n{2,}/)
                    return paras.map((para, idx) => (
                      <p key={idx} className={idx > 0 ? 'mt-1.5' : undefined}>
                        {highlightText(para, highlight)}
                        {idx === paras.length - 1 && editedMark}
                      </p>
                    ))
                  }
                  return <p>{highlightText(text, highlight)}{editedMark}</p>
                })()}
              </div>
            )}
          </div>

          <div className={`flex h-fit shrink-0 self-start gap-0.5 transition-opacity group-hover:opacity-100 ${open ? 'opacity-100' : 'opacity-0'}`}>
            {!isEmpty && (
              <Tooltip content={copied ? t('record.copied') : t('record.copyText')} forceVisible={copied}>
                <button
                  onClick={() => {
                    void bridge.copyText(text).then(() => {
                      setCopied(true)
                      setTimeout(() => setCopied(false), 1500)
                    })
                  }}
                  className="inline-flex items-center rounded p-1 transition-colors hover:bg-accent"
                  aria-label={t('record.copy')}
                >
                  {copied ? (
                    <SuccessCheck className="h-3.5 w-3.5 text-success" />
                  ) : (
                    <Copy className="h-3.5 w-3.5 text-muted-foreground" />
                  )}
                </button>
              </Tooltip>
            )}

            {onToggleFavorite && (
              <Tooltip content={record.favorite ? t('record.unfavorite') : t('record.favorite')}>
                <button
                  onClick={() => onToggleFavorite(!record.favorite)}
                  className="rounded p-1 hover:bg-accent"
                  aria-label={record.favorite ? t('record.unfavorite') : t('record.favorite')}
                >
                  <Star className={`h-3.5 w-3.5 ${record.favorite ? 'fill-amber-400 text-amber-500' : 'text-muted-foreground'}`} />
                </button>
              </Tooltip>
            )}

            <Tooltip content={expanded ? t('record.collapse') : t('record.expand')}>
              <button
                onClick={() => setExpanded(!expanded)}
                className="rounded p-1 hover:bg-accent"
                aria-label={t('record.details')}
                aria-expanded={expanded}
              >
                {expanded
                  ? <ChevronUp className="h-3.5 w-3.5 text-muted-foreground" />
                  : <ChevronDown className="h-3.5 w-3.5 text-muted-foreground" />}
              </button>
            </Tooltip>

            <Tooltip content={t('record.deleteRecord')}>
              <button
                onClick={onDelete}
                className="rounded p-1 hover:bg-accent"
                aria-label={t('record.delete')}
              >
                <Trash2 className="h-3.5 w-3.5 text-destructive" />
              </button>
            </Tooltip>
          </div>

          {/* 展開動畫：grid 0fr→1fr，200ms ease-out。中途試過實測高度 + 自定義曲線 +
              内容淡入位移，叠得越多反而越碎，已经撤掉，别再往上加。 */}
          <div
            className="col-span-2 grid grid-cols-subgrid transition-[grid-template-rows] duration-200 ease-out motion-reduce:transition-none"
            style={{ gridTemplateRows: open ? '1fr' : '0fr' }}
            aria-hidden={!open}
          >
            <div className="col-span-2 grid grid-cols-subgrid overflow-hidden">
              <div className="col-span-2 mt-2 grid grid-cols-subgrid gap-y-2 text-xs">
                {/* 空結果的成因。摺疊行只能顯示「無有效聲音」，真實原因（額度耗盡、
                    资源未开通、服务端断连、文本处理清空）放这里。老记录没有这个字段。 */}
                {isEmpty && (record.failReasonCode || record.failReason) && (
                  <div className="col-span-2 text-muted-foreground">
                    <span className="font-medium">{t('record.reasonLabel')}</span>
                    <span className="whitespace-pre-line break-words">{historyFailureReasonDisplay(record)}</span>
                  </div>
                )}
                {!isEmpty && record.asrText && (
                  <div className="text-muted-foreground">
                    <span className="font-medium">{t('record.asrLabel')}</span>
                    <span className="whitespace-pre-line">{highlightText(record.asrText, highlight)}</span>
                  </div>
                )}
                <div className="col-span-2 flex flex-wrap items-center gap-2 text-muted-foreground">
                  {record.workMode && (
                    <>
                      <span className="rounded border border-border px-1.5 py-0.5 text-xs">
                        {record.workMode === 'server' ? t('record.modeServer') : record.workMode === 'cloud_api' ? t('record.modeCloudApi') : t('record.modeLocal')}
                      </span>
                      {record.asrProvider && (
                        <span className="text-xs">ASR: {ASR_PROVIDER_DISPLAY[record.asrProvider] || record.asrProvider}</span>
                      )}
                      {record.aiProvider && record.aiProvider !== 'server' && record.llmMs > 0 && (
                        <span className="text-xs">
                          AI: {record.aiProvider}{record.aiModel ? ` (${record.aiModel})` : ''}
                        </span>
                      )}
                      <span className="text-border">|</span>
                    </>
                  )}
                  <span>{t('record.voiceLength', { sec: voiceDurationSec.toFixed(1) })}</span>
                  <span className="text-border">|</span>
                  <span>{t('record.recognizeTime', { total: ((record.asrMs + record.llmMs) / 1000).toFixed(1), asr: record.asrMs, llm: record.llmMs })}</span>

                  {/* 四個操作按鈕單獨收成一組：它們原來和左邊的文字共用外層的 gap-2(8px)，
                      按钮之间也就被撑到 8px，比右上角那排（gap-0.5）散得多。
                      这里组内用 gap-0.5 与上排统一，组与文字之间仍由外层的 gap-2 分隔。 */}
                  <div className="flex items-center gap-0.5">
                    {!isEmpty && onEdit && !editing && (
                      <Tooltip content={t('record.editText')}>
                        <button
                          type="button"
                          onClick={startEdit}
                          className="relative top-[0.5px] flex h-7 w-7 items-center justify-center rounded p-1.5 hover:bg-accent"
                          aria-label={t('record.edit')}
                        >
                          <Pencil className="h-3.5 w-3.5 text-muted-foreground" />
                        </button>
                      </Tooltip>
                    )}
                    {record.audioFilePath && (
                      <Tooltip content={playback.playing ? t('record.pause') : t('record.play')}>
                        <button
                          type="button"
                          onClick={() => { void playback.toggle() }}
                          disabled={playback.loading}
                          className="relative top-[0.5px] flex h-7 w-7 items-center justify-center rounded p-1.5 hover:bg-accent disabled:opacity-50"
                          aria-label={playback.playing ? t('record.pause') : t('record.play')}
                        >
                          {playback.loading ? (
                            <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground" />
                          ) : playback.playing ? (
                            <Pause className="h-3.5 w-3.5 text-primary" />
                          ) : (
                            <Play className="h-3.5 w-3.5 text-muted-foreground" />
                          )}
                        </button>
                      </Tooltip>
                    )}
                    {record.audioFilePath && (
                      <Tooltip content={downloadStatus === 'ok' ? t('record.savedTo', { path: downloadPath }) : downloadStatus === 'fail' ? t('record.downloadFailed', { path: downloadPath }) : t('record.downloadAudio')}>
                        <button
                          type="button"
                          onClick={() => { void handleDownloadAudio() }}
                          disabled={downloading}
                          className="relative top-[0.5px] flex h-7 w-7 items-center justify-center rounded p-1.5 hover:bg-accent disabled:opacity-50"
                          aria-label={t('record.downloadAudio')}
                        >
                          {downloading ? (
                            <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground" />
                          ) : downloadStatus === 'ok' ? (
                            <SuccessCheck className="h-3.5 w-3.5 text-success" />
                          ) : downloadStatus === 'fail' ? (
                            <X className="h-3.5 w-3.5 text-destructive" />
                          ) : (
                            <Download className="h-3.5 w-3.5 text-muted-foreground" />
                          )}
                        </button>
                      </Tooltip>
                    )}
                    {canCorrectAsr && (
                      <Tooltip content={asrCorrectionSubmitted ? t('asrCorrection.entrySubmitted') : t('asrCorrection.entry')}>
                        <button
                          type="button"
                          onClick={() => setCorrecting(true)}
                          className="relative top-[0.5px] flex h-7 w-7 items-center justify-center rounded p-1.5 hover:bg-accent"
                          aria-label={asrCorrectionSubmitted ? t('asrCorrection.entrySubmitted') : t('asrCorrection.entry')}
                        >
                          <SpellCheck className={`h-3.5 w-3.5 ${asrCorrectionSubmitted ? 'text-success' : 'text-muted-foreground'}`} />
                        </button>
                      </Tooltip>
                    )}
                    {record.audioFilePath && onReprocess && (
                      <Tooltip content={t('record.reprocess')}>
                        <button
                          type="button"
                          onClick={() => { void handleReprocess() }}
                          disabled={reprocessing}
                          className="relative top-[0.5px] flex h-7 w-7 items-center justify-center rounded p-1.5 hover:bg-accent disabled:opacity-50"
                          aria-label={t('record.reprocess')}
                        >
                          {reprocessing ? (
                            <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground" />
                          ) : (
                            <RotateCcw className="h-3.5 w-3.5 text-muted-foreground" />
                          )}
                        </button>
                      </Tooltip>
                    )}
                  </div>
                </div>
                {downloadStatus === 'ok' && downloadPath && (
                  <div className="col-span-2 mt-1 flex items-center gap-2 text-xs text-success break-all">
                    <span className="min-w-0 truncate">{t('record.savedTo', { path: downloadPath })}</span>
                    <button
                      onClick={() => void invoke('reveal_file_in_folder', { filePath: downloadPath })}
                      className="shrink-0 rounded p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                      aria-label={t('record.openFolder')}
                    >
                      <FolderOpen className="h-3.5 w-3.5" />
                    </button>
                  </div>
                )}
                {downloadStatus === 'fail' && downloadPath && (
                  <div className="col-span-2 mt-1 text-xs text-destructive break-all">
                    {t('record.downloadFailed', { path: downloadPath })}
                  </div>
                )}
                {reprocessError && !reprocessing && (
                  <div role="alert" className="col-span-2 mt-1 text-xs text-destructive break-all">
                    {t('record.reprocessFailed', { reason: reprocessError })}
                  </div>
                )}
                {/* 音訊進度條 + 倍速 */}
                {playback.ready && <AudioProgressBar playback={playback} className="col-span-2 mt-2" />}
              </div>
            </div>
          </div>
        </div>
      </div>

      {correcting && onSaveAsrCorrection && (
        <AsrCorrectionDialog
          record={record}
          onClose={() => setCorrecting(false)}
          onSubmitted={(patch) => { void onSaveAsrCorrection(patch) }}
        />
      )}
    </div>
  )
}

function DayGroup({
  label,
  records,
  onDelete,
  onToggleFavorite,
  onReprocess,
  onEdit,
  onSaveAsrCorrection,
  highlight,
}: {
  label: string
  records: HistoryRecord[]
  onDelete: (id: string) => void
  onToggleFavorite?: (id: string, nextFavorite: boolean) => Promise<void> | void
  onReprocess?: (record: HistoryRecord) => Promise<void> | void
  onEdit?: (id: string, nextText: string) => Promise<void> | void
  onSaveAsrCorrection?: (id: string, patch: Partial<HistoryRecord>) => Promise<void> | void
  highlight?: string
}) {
  return (
    <Card>
      <CardContent className="p-4">
        <h2 className="mb-2 text-sm font-medium text-muted-foreground">{label}</h2>
        <div className="divide-y">
          {records.map((record) => (
            <HistoryItem
              key={record.id}
              record={record}
              onDelete={() => onDelete(record.id)}
              onToggleFavorite={onToggleFavorite ? (next) => onToggleFavorite(record.id, next) : undefined}
              onReprocess={onReprocess ? () => onReprocess(record) : undefined}
              onEdit={onEdit ? (nextText) => onEdit(record.id, nextText) : undefined}
              onSaveAsrCorrection={onSaveAsrCorrection ? (patch) => onSaveAsrCorrection(record.id, patch) : undefined}
              highlight={highlight}
            />
          ))}
        </div>
      </CardContent>
    </Card>
  )
}

export default function HistoryRecordList({
  records,
  onDelete,
  onToggleFavorite,
  onReprocess,
  onEdit,
  onSaveAsrCorrection,
  emptyText,
  highlight,
}: HistoryRecordListProps) {
  useT()
  // 預設值不能寫在引數裡：預設引數在模組作用域求值不了 t()，寫死中文串又會在英文介面漏出來。
  const resolvedEmptyText = emptyText ?? t('history.empty')
  const dayGroups = useMemo(() => groupByDay(records, getDayLabel), [records])

  if (records.length === 0) {
    return <p className="py-12 text-center text-muted-foreground">{resolvedEmptyText}</p>
  }

  return (
    <div className="space-y-3">
      {dayGroups.map((group) => (
        <DayGroup
          key={group.dayStart}
          label={group.label}
          records={group.records}
          onDelete={onDelete}
          onToggleFavorite={onToggleFavorite}
          onReprocess={onReprocess}
          onEdit={onEdit}
          onSaveAsrCorrection={onSaveAsrCorrection}
          highlight={highlight}
        />
      ))}
    </div>
  )
}
