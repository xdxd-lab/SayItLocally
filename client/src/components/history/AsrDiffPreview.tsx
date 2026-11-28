import { useMemo } from 'react'
import { computeAsrDiff, countAsrDiffChanges, hasAsrDiffChange } from '@/lib/asrDiff'
import { t } from '@/i18n'

/**
 * 原始識別文本 → 修正後文本的改動預覽。
 *
 * 用 <del>/<ins> 而不是兩個 <span>：讀屏會念出"刪除/插入"，色盲使用者也能靠刪除線區分。
 * 只靠淺紅/淺綠兩種底色的話，這兩類使用者看到的是一段沒有任何標記的普通文本。
 *
 * 不在 textarea 裡做高亮 —— textarea 內部無法放富文本，只能"可編輯框 + 只讀預覽"兩塊。
 */
export function AsrDiffPreview({
  original,
  corrected,
  className = '',
}: {
  original: string
  corrected: string
  className?: string
}) {
  const segments = useMemo(() => computeAsrDiff(original, corrected), [original, corrected])
  const changed = hasAsrDiffChange(segments)
  const changeCount = countAsrDiffChanges(segments)

  return (
    <div className={className}>
      <div className="mb-1 flex items-center justify-between text-xs text-muted-foreground">
        <span className="font-medium">{t('asrCorrection.diffLabel')}</span>
        <span>{changed ? t('asrCorrection.diffCount', { count: changeCount }) : t('asrCorrection.diffNone')}</span>
      </div>
      {/* mac-selectable：mac 上介面文字預設不可選，對比預覽要能選中拷貝（index.css） */}
      <div
        className="mac-selectable max-h-40 overflow-y-auto whitespace-pre-wrap break-words rounded-md border border-border bg-muted/30 px-2.5 py-2 text-sm leading-relaxed"
        aria-live="polite"
      >
        {segments.length === 0 ? (
          <span className="italic text-muted-foreground">{t('asrCorrection.diffEmpty')}</span>
        ) : (
          segments.map((segment, index) => {
            if (segment.kind === 'delete') {
              return (
                <del
                  key={index}
                  className="rounded-sm bg-red-100 px-0.5 text-red-900 decoration-red-500/70 dark:bg-red-500/25 dark:text-red-200"
                >
                  {segment.text}
                </del>
              )
            }
            if (segment.kind === 'insert') {
              return (
                <ins
                  key={index}
                  className="rounded-sm bg-emerald-100 px-0.5 text-emerald-900 no-underline dark:bg-emerald-500/25 dark:text-emerald-200"
                >
                  {segment.text}
                </ins>
              )
            }
            return <span key={index}>{segment.text}</span>
          })
        )}
      </div>
    </div>
  )
}
