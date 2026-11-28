import { useEffect, useRef, useState } from 'react'
import { Feedback } from '@/components/ui/feedback'
import { Select, type SelectOption } from '@/components/ui/select'
import { useT } from '@/i18n/useT'
import { getSetting, setSetting } from '@/services/store'
import { CONTEXT_SELECTION_AI_PROFILE_KEY } from '@/services/transcription/selectionAi'
import { loadAiProfiles } from './aiProfileStore'
import { isProfileComplete, profileSubtitle, profileTitle, type AiProfile } from './aiProviderCatalog'

export default function ContextSelectionModelSetting() {
  const t = useT()
  const [profiles, setProfiles] = useState<AiProfile[]>([])
  const [profileId, setProfileId] = useState('')
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<'load' | 'save' | null>(null)
  const revision = useRef(0)
  const savingRef = useRef(false)

  useEffect(() => {
    let disposed = false
    const load = async () => {
      if (savingRef.current) return
      const current = ++revision.current
      try {
        const [state, selectedId] = await Promise.all([
          loadAiProfiles(),
          getSetting(CONTEXT_SELECTION_AI_PROFILE_KEY, ''),
        ])
        if (disposed || current !== revision.current) return
        setProfiles(state.profiles)
        setProfileId(selectedId)
        setError(null)
      } catch {
        if (!disposed && current === revision.current) setError('load')
      } finally {
        if (!disposed && current === revision.current) setLoading(false)
      }
    }
    void load()
    window.addEventListener('ai-profiles-changed', load)
    window.addEventListener('focus', load)
    return () => {
      disposed = true
      window.removeEventListener('ai-profiles-changed', load)
      window.removeEventListener('focus', load)
    }
  }, [])

  const save = async (nextId: string) => {
    if (savingRef.current) return
    savingRef.current = true
    revision.current += 1
    setSaving(true)
    try {
      await setSetting(CONTEXT_SELECTION_AI_PROFILE_KEY, nextId)
      setProfileId(nextId)
      setError(null)
    } catch {
      setError('save')
    } finally {
      savingRef.current = false
      setSaving(false)
    }
  }
  const selected = profiles.find((profile) => profile.id === profileId)
  const missing = Boolean(profileId && !selected)
  const usable = profiles.filter(isProfileComplete)

  // 用應用自己的 Select，不用原生 <select>：原生下拉在 WebView2 裡彈的是系統列表，
  // 寬度跟著最長那一項走（「模型名 · 供應商 · 主機名」很長），會伸出卡片甚至視窗。
  // 這個元件的列表與觸發器同寬，長名字截斷，懸停看全稱。
  // 它不支援停用單項，所以沒填齊的服務乾脆不列；只有已經選中的那條例外，
  // 否則觸發器上什麼都不顯示，使用者看不出自己選過什麼（下面另有警告說明原因）。
  const options: SelectOption[] = [
    { value: '', label: t('settings.contextAware.sameModel') },
    ...(missing ? [{ value: profileId, label: t('settings.contextAware.modelMissingOption') }] : []),
    ...profiles
      .filter((profile) => isProfileComplete(profile) || profile.id === profileId)
      .map((profile) => {
        const incomplete = isProfileComplete(profile) ? '' : ` · ${t('ai.status.incomplete')}`
        const hint = `${profileSubtitle(profile)}${incomplete}`
        return { value: profile.id, label: profileTitle(profile), hint, title: `${profileTitle(profile)} · ${hint}` }
      }),
  ]

  // 和卡片標題那一行同一個結構：左邊名稱 + 一行說明，右邊控制元件。下拉框固定窄寬度，
  // 列表右對齊往左展開（menuAlign="end"），長模型名不會把控制元件撐寬。
  return (
    <div className="mt-4 border-t border-border pt-4">
      <div className="flex flex-wrap items-center justify-between gap-x-6 gap-y-2">
        <div className="min-w-0 flex-[1_1_16rem]">
          <p id="context-selection-ai-label" className="text-sm text-foreground">
            {t('settings.contextAware.model')}
          </p>
          <p className="mt-0.5 text-xs leading-relaxed text-muted-foreground">
            {!loading && !usable.length && !error
              ? t('settings.contextAware.modelEmpty')
              : t('settings.contextAware.modelDesc')}
          </p>
        </div>
        <Select
          size="sm"
          menuAlign="end"
          labelledBy="context-selection-ai-label"
          value={profileId}
          disabled={loading || saving || error === 'load'}
          onChange={(value) => void save(value)}
          options={options}
          className="w-56 shrink-0"
        />
      </div>
      {missing && !error && (
        <Feedback className="mt-3" tone="warning" message={t('settings.contextAware.modelMissing')} />
      )}
      {selected && !isProfileComplete(selected) && !error && (
        <Feedback className="mt-3" tone="warning" message={t('settings.contextAware.modelIncomplete')} />
      )}
      {error && (
        <Feedback className="mt-3" tone="error" message={t(error === 'load'
          ? 'settings.contextAware.modelLoadError'
          : 'settings.contextAware.modelSaveError')} />
      )}
    </div>
  )
}
