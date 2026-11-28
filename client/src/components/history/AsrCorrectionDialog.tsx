import { useCallback, useEffect, useRef, useState } from 'react'
import { AlertTriangle, Check, Info, Loader2, Pause, Play, RotateCcw } from 'lucide-react'
import { Modal } from '@/components/ui/modal'
import { Button } from '@/components/ui/button'
import * as bridge from '@/services/bridge'
import { type HistoryRecord } from '@/services/store'
import {
  ASR_CORRECTION_WITHDRAW_DAYS,
  grantAsrCorrectionConsent,
  hasAsrCorrectionConsent,
  submitAsrCorrection,
  withdrawAsrCorrection,
} from '@/services/asrCorrection'
import { normalizeForDiff } from '@/lib/asrDiff'
import { t } from '@/i18n'
import { useT } from '@/i18n/useT'
import { useRecordingPlayback } from './useRecordingPlayback'
import { AudioProgressBar } from './AudioProgressBar'
import { AsrDiffPreview } from './AsrDiffPreview'

/**
 * 「糾正識別」面板。
 *
 * 三個刻意的設計，改之前先看 .kiro/decisions.md：
 * 1. 可編輯的那一欄初值是 **record.asrText（識別原文）**，絕不是 llmText。
 *    使用者平時看到的是 AI 整理後的文本，如果照著它改（補標點、分段、改書面語），
 *    拿到的標註對 ASR 訓練是噪聲，因此糾正面板不展示 AI 整理後的文本。
 * 2. 音訊讀不到就不讓提交。沒有音訊的樣本對識別改進沒有用。
 * 3. 首次提交必須過一次明確同意（不是一個資訊圖示）。上傳的是使用者的錄音，
 *    比文本敏感得多。
 */
/**
 * 可編輯框的高度上限（px）。上面那塊「識別原文」是 max-h-40（160px），這裡給到 200px：
 * 兩塊的行高、字號、內邊距完全一致，所以同樣的文字在兩塊裡佔一樣的高度，
 * 只是"要動手改"的那塊允許多長一點。超過之後內部滾動，而不是把彈窗撐到螢幕外。
 */
const EDITOR_MAX_HEIGHT_PX = 200

export function AsrCorrectionDialog({
  record,
  onClose,
  onSubmitted,
}: {
  record: HistoryRecord
  onClose: () => void
  onSubmitted: (patch: Partial<HistoryRecord>) => void
}) {
  useT()
  const originalText = record.asrText || ''
  const [correctedText, setCorrectedText] = useState(record.asrCorrectedText || originalText)
  const [audioState, setAudioState] = useState<'checking' | 'ok' | 'missing'>('checking')
  const [consentNeeded, setConsentNeeded] = useState(false)
  const [consentChecked, setConsentChecked] = useState(false)
  const [submitting, setSubmitting] = useState(false)
  const [withdrawing, setWithdrawing] = useState(false)
  const [error, setError] = useState('')
  const editorRef = useRef<HTMLTextAreaElement | null>(null)
  // 用記錄裡存的音訊時長做初值，這樣沒點播放也能看到總長（audioDurationSec 是音訊真實
  // 長度，durationSec 是按住快捷鍵的時長，只在老記錄缺前者時兜底）
  const playback = useRecordingPlayback(
    record.audioFilePath,
    record.audioDurationSec || record.durationSec || 0,
  )

  const submitted = Boolean(record.asrCorrectionId)

  useEffect(() => {
    let alive = true
    void (async () => {
      const path = record.audioFilePath
      if (!path) {
        if (alive) setAudioState('missing')
        return
      }
      const exists = await bridge.audioFileExists(path).catch(() => false)
      if (alive) setAudioState(exists ? 'ok' : 'missing')
    })()
    void (async () => {
      const granted = await hasAsrCorrectionConsent().catch(() => false)
      if (alive) setConsentNeeded(!granted)
    })()
    return () => { alive = false }
  }, [record.audioFilePath])

  /**
   * 讓可編輯框跟著內容長高，行為對齊上面那塊只讀的「識別原文」：一行內容就是一行高，
   * 到上限後內部滾動。
   *
   * 不用 `rows={行數}` 那種演算法：`rows` 只數換行符，一段沒有換行但會折行的長文本
   * 仍然只算一行，框會比內容矮一大截。所以按 scrollHeight 實測。
   *
   * `+ extra` 是上下邊框：box-sizing 是 border-box，height 含邊框而 scrollHeight 不含，
   * 少了它每次都會矮 2px，於是內容一進來就立刻出現捲軸。
   */
  useEffect(() => {
    const el = editorRef.current
    if (!el) return
    el.style.height = 'auto'
    const extra = el.offsetHeight - el.clientHeight
    el.style.height = `${Math.min(el.scrollHeight + extra, EDITOR_MAX_HEIGHT_PX)}px`
  }, [correctedText])

  const changed = normalizeForDiff(originalText) !== normalizeForDiff(correctedText)
  const canSubmit =
    !submitted &&
    audioState === 'ok' &&
    changed &&
    !submitting &&
    (!consentNeeded || consentChecked)

  const handleSubmit = useCallback(async () => {
    setError('')
    setSubmitting(true)
    try {
      if (consentNeeded) await grantAsrCorrectionConsent()
      const result = await submitAsrCorrection(record, correctedText)
      if (!result.ok) {
        setError(result.message)
        return
      }
      setConsentNeeded(false)
      onSubmitted({
        asrCorrectionId: result.correctionId,
        asrCorrectionSubmittedAt: Date.now(),
        asrCorrectedText: normalizeForDiff(correctedText),
      })
    } catch {
      // [自訂] S43：本 fork 停用上傳，submitAsrCorrection 會拋錯；在這裡接住，不成為未處理例外
      setError(t('asrCorrection.errorNetwork'))
    } finally {
      setSubmitting(false)
    }
  }, [consentNeeded, correctedText, onSubmitted, record])

  const handleWithdraw = useCallback(async () => {
    if (!record.asrCorrectionId) return
    setError('')
    setWithdrawing(true)
    try {
      const ok = await withdrawAsrCorrection(record.asrCorrectionId)
      if (!ok) {
        setError(t('asrCorrection.errorNetwork'))
        return
      }
      // 本地也清掉，入口回到"可提交"。
      // 必須寫 null 而不是 undefined：patch 要過 JSON.stringify，undefined 的鍵會被丟掉，
      // 結果是欄位沒被清、介面一直顯示"已提交"（見 HistoryRecord 上的註釋）。
      onSubmitted({
        asrCorrectionId: null,
        asrCorrectionSubmittedAt: null,
        asrCorrectedText: null,
      })
    } catch {
      // [自訂] S43／S16：撤回同樣停用，拋錯在這裡接住
      setError(t('asrCorrection.errorNetwork'))
    } finally {
      setWithdrawing(false)
    }
  }, [onSubmitted, record.asrCorrectionId])

  return (
    <Modal title={t('asrCorrection.title')} onClose={onClose} showCloseButton panelClassName="w-[620px]">
      <div className="mt-3 space-y-3">
        {/* 錄音 */}
        {audioState === 'missing' ? (
          <div className="flex items-start gap-2 rounded-md border border-border bg-muted/40 px-2.5 py-2 text-xs text-muted-foreground">
            <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-amber-500" aria-hidden />
            <span>{t('asrCorrection.audioMissing')}</span>
          </div>
        ) : (
          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={() => { void playback.toggle() }}
              disabled={playback.loading || audioState !== 'ok'}
              className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md border border-border hover:bg-accent disabled:opacity-50"
              aria-label={playback.playing ? t('record.pause') : t('record.play')}
            >
              {playback.loading ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground" aria-hidden />
              ) : playback.playing ? (
                <Pause className="h-3.5 w-3.5 text-primary" aria-hidden />
              ) : (
                <Play className="h-3.5 w-3.5 text-muted-foreground" aria-hidden />
              )}
            </button>
            <AudioProgressBar playback={playback} className="min-w-0 flex-1" />
          </div>
        )}

        {/* 識別原文 + 改動標記合成一塊。
            原本是"只讀原文"和"改動預覽"兩個框，但 diff 本身就包含完整原文（刪掉的部分
            帶刪除線），兩個框擺在一起是同一段話看兩遍 —— 連上可編輯框一共三個框，
            光是找"我該在哪兒打字"就要費一下。合成一塊之後只剩「看」和「改」兩塊。 */}
        <AsrDiffPreview original={originalText} corrected={correctedText} />

        {/* 正確文本（可編輯） */}
        <div>
          <div className="mb-1 flex items-center justify-between gap-2">
            <label htmlFor="asr-correction-input" className="block text-xs font-medium text-foreground">
              {t('asrCorrection.correctedLabel')}
            </label>
            {/* 改亂了想重來時不用手抄一遍原文 */}
            <button
              type="button"
              onClick={() => setCorrectedText(originalText)}
              disabled={!changed || submitted || submitting}
              className="flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:pointer-events-none disabled:opacity-40"
            >
              <RotateCcw className="h-3 w-3" aria-hidden />
              {t('asrCorrection.reset')}
            </button>
          </div>
          {/* 內邊距/字號/行高與「識別原文」那塊保持一致，兩塊才對得齊；
              高度交給上面那個 effect 算，所以關掉手動拖拽（拖完一打字就會被算回去）。 */}
          <textarea
            ref={editorRef}
            id="asr-correction-input"
            data-modal-autofocus
            value={correctedText}
            onChange={(e) => setCorrectedText(e.target.value)}
            disabled={submitted || submitting}
            rows={1}
            className="w-full resize-none overflow-y-auto rounded-md border border-input-border bg-input-bg px-2.5 py-2 text-sm leading-relaxed focus:border-input-focus-border focus:outline-none disabled:opacity-60"
          />
          <p className="mt-1 text-[11px] leading-relaxed text-muted-foreground">{t('asrCorrection.correctedHint')}</p>
        </div>

        {/* 同意：首次提交必須勾；之後只留一個資訊圖示 */}
        {!submitted && (consentNeeded ? (
          <label className="flex cursor-pointer items-start gap-2 rounded-md border border-border bg-muted/30 px-2.5 py-2 text-xs leading-relaxed">
            <input
              type="checkbox"
              checked={consentChecked}
              onChange={(e) => setConsentChecked(e.target.checked)}
              className="mt-0.5 h-3.5 w-3.5 shrink-0 accent-primary"
            />
            <span className="text-muted-foreground">
              {t('asrCorrection.consent', { days: ASR_CORRECTION_WITHDRAW_DAYS })}
            </span>
          </label>
        ) : (
          <div className="flex items-start gap-1.5 text-[11px] leading-relaxed text-muted-foreground">
            <Info className="mt-0.5 h-3 w-3 shrink-0" aria-hidden />
            <span>{t('asrCorrection.consent', { days: ASR_CORRECTION_WITHDRAW_DAYS })}</span>
          </div>
        ))}

        {error && <p className="text-xs text-destructive">{error}</p>}

        <div className="flex items-center justify-end gap-2 pt-1">
          {submitted ? (
            <>
              <span className="mr-auto flex items-center gap-1.5 text-xs text-success">
                <Check className="h-3.5 w-3.5" aria-hidden />
                {t('asrCorrection.submittedWithId', { id: record.asrCorrectionId || '' })}
              </span>
              <Button variant="outline" size="sm" onClick={() => { void handleWithdraw() }} disabled={withdrawing}>
                {withdrawing && <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" aria-hidden />}
                {t('asrCorrection.withdraw')}
              </Button>
              <Button size="sm" onClick={onClose}>{t('common.close')}</Button>
            </>
          ) : (
            <>
              <Button variant="ghost" size="sm" onClick={onClose} disabled={submitting}>
                {t('common.cancel')}
              </Button>
              <Button size="sm" onClick={() => { void handleSubmit() }} disabled={!canSubmit}>
                {submitting && <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" aria-hidden />}
                {t('asrCorrection.submit')}
              </Button>
            </>
          )}
        </div>
      </div>
    </Modal>
  )
}
