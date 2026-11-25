import { useEffect, useRef, useState } from 'react'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Modal } from '@/components/ui/modal'
import { useT } from '@/i18n/useT'
import type { TranslationKey } from '@/i18n'
import { formatBytes } from '@/lib/utils'
import * as bridge from '@/services/bridge'
import { canConfirmHistoryClear, HISTORY_CLEAR_DELAY_MS } from './historyClearConfirmation'

// [自訂] L-72：手動管理留在本機的損毀資料庫副本。
export default function CorruptDbCopiesSection() {
  const t = useT()
  const [copies, setCopies] = useState<bridge.CorruptDbCopy[]>([])
  const [selected, setSelected] = useState<bridge.CorruptDbCopy | null>(null)
  const [protectedRecordings, setProtectedRecordings] = useState(false)
  const [confirmResume, setConfirmResume] = useState(false)
  const [readyAt, setReadyAt] = useState<number | null>(null)
  const [unlocked, setUnlocked] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<TranslationKey | null>(null)
  const submitting = useRef(false)

  useEffect(() => {
    let active = true
    void bridge.listCorruptDbCopies().then(result => {
      if (active) setCopies(result)
    }).catch(() => {
      if (active) setError('settings.corruptDb.listFailed')
    })
    void bridge.recordingCleanupProtected().then(result => {
      if (active) setProtectedRecordings(result)
    }).catch(() => {
      if (active) setProtectedRecordings(true)
    })
    return () => { active = false }
  }, [])

  useEffect(() => {
    if (readyAt === null) return
    const timer = window.setTimeout(() => setUnlocked(true), Math.max(0, Math.ceil(readyAt - performance.now())))
    return () => window.clearTimeout(timer)
  }, [readyAt])

  const close = () => {
    setSelected(null)
    setConfirmResume(false)
    setReadyAt(null)
    setUnlocked(false)
  }

  const confirm = async (clickCount: number) => {
    if ((!selected && !confirmResume) || readyAt === null || submitting.current
      || !canConfirmHistoryClear(performance.now(), readyAt, clickCount)) return
    submitting.current = true
    setBusy(true)
    setError(null)
    try {
      if (selected) {
        await bridge.deleteCorruptDbCopy(selected.name)
        setCopies(current => current.filter(copy => copy.name !== selected.name))
      } else {
        await bridge.resumeRecordingCleanup()
        setProtectedRecordings(false)
      }
    } catch {
      if (selected) {
        setError('settings.corruptDb.deleteFailed')
        // A sidecar might have been removed before another file failed.
        try { setCopies(await bridge.listCorruptDbCopies()) } catch { /* keep the deletion error */ }
      } else {
        setError('settings.corruptDb.resumeFailed')
      }
    } finally {
      close()
      setBusy(false)
      submitting.current = false
    }
  }

  const reveal = async (copy: bridge.CorruptDbCopy) => {
    setError(null)
    try { await bridge.revealCorruptDbCopy(copy.name) } catch {
      setError('settings.corruptDb.revealFailed')
    }
  }

  return (
    <>
      {error && <p role="alert" className="text-sm text-destructive">{t(error)}</p>}
      {(copies.length > 0 || protectedRecordings) && (
        <Card>
          <CardContent className="space-y-4 p-6">
            <div>
              <h2 className="text-lg font-semibold">{t('settings.corruptDb.title')}</h2>
              <p className="mt-1 text-sm text-muted-foreground">{t('settings.corruptDb.description')}</p>
            </div>
            {protectedRecordings && (
              <div className="space-y-2">
                <p className="text-sm">{t('settings.corruptDb.recordingsProtected')}</p>
                <Button size="sm" variant="outline" onClick={() => setConfirmResume(true)}>
                  {t('settings.corruptDb.resumeCleanup')}
                </Button>
              </div>
            )}
            <ul className="divide-y divide-border">
              {copies.map(copy => (
                <li key={copy.name} className="flex flex-wrap items-center justify-between gap-2 py-3">
                  <div className="flex flex-wrap gap-x-3 text-sm">
                    <time dateTime={copy.createdAtLocal}>{copy.createdAtLocal.replace('T', ' ')}</time>
                    <span className="text-muted-foreground">{formatBytes(copy.sizeBytes)}</span>
                  </div>
                  <div className="flex flex-wrap gap-2">
                    <Button size="sm" variant="outline" onClick={() => void reveal(copy)}>
                      {t('settings.corruptDb.reveal')}
                    </Button>
                    <Button size="sm" variant="destructive" onClick={() => setSelected(copy)}>
                      {t('settings.corruptDb.delete')}
                    </Button>
                  </div>
                </li>
              ))}
            </ul>
          </CardContent>
        </Card>
      )}
      {(selected || confirmResume) && (
        <Modal
          key={String(readyAt !== null)}
          title={selected
            ? t(readyAt === null ? 'settings.corruptDb.deleteTitle' : 'settings.corruptDb.confirmTitle')
            : t(readyAt === null ? 'settings.corruptDb.resumeTitle' : 'settings.corruptDb.resumeConfirmTitle')}
          onClose={close}
          locked={busy}
          panelClassName="w-96"
        >
          <p className="mt-2 text-sm text-muted-foreground">
            {selected
              ? t(readyAt === null ? 'settings.corruptDb.deletePrompt' : 'settings.corruptDb.confirmPrompt', {
                date: selected.createdAtLocal.replace('T', ' '),
              })
              : t('settings.corruptDb.resumePrompt')}
          </p>
          <div className="mt-5 flex flex-wrap justify-end gap-2">
            <Button variant="outline" data-modal-autofocus disabled={busy} onClick={close}>{t('common.cancel')}</Button>
            <Button
              variant="destructive"
              disabled={busy || (readyAt !== null && !unlocked)}
              onClick={event => {
                if (readyAt !== null) void confirm(event.detail)
                else if (event.detail <= 1) setReadyAt(performance.now() + HISTORY_CLEAR_DELAY_MS)
              }}
            >
              {t(selected ? 'settings.corruptDb.delete' : 'settings.corruptDb.resumeCleanup')}
            </Button>
          </div>
        </Modal>
      )}
    </>
  )
}
