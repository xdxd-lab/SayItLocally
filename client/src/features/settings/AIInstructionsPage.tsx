import { useEffect, useState } from 'react'
import { BUILTIN_APP_RULES } from '@/services/personalization/defaults'
import {
  getAppPromptRules,
  saveAppPromptRules,
} from '@/services/personalization/store'
import { moveItem } from '@/components/ui/sortable'
import type { AppPromptRule } from '@/services/personalization/types'
import {
  refreshPreset,
  refreshRecorderSettings,
  setActivePresetCache,
  setPromptPresetsCache,
} from '@/services/recorder'
import * as bridge from '@/services/bridge'
import { useActivePreset } from '@/hooks/useActivePreset'
import { refreshActivePreset, setActivePresetKnown } from '@/stores/activePreset'
import {
  deletePromptPreset,
  getBuiltinPromptLanguage,
  getPromptPresets,
  moveCustomPromptPreset,
  getPresetShortcuts,
  getSetting,
  savePromptPreset,
  setActivePresetId,
  setBuiltinPromptLanguage,
  setPresetShortcuts,
  type BuiltinPromptLanguage,
  type PromptPreset,
} from '@/services/store'
import AIProofreadToggle from './AIProofreadToggle'
import HotwordPromptInjectToggle from './HotwordPromptInjectToggle'
import ContextAwareSection from './ContextAwareSection'
import AppPromptRulesSection from './AppPromptRulesSection'
import PromptPresetSection from './PromptPresetSection'
import { useT } from '@/i18n/useT'
import { PageTitle } from '@/components/ui/info-tip'

export default function AIInstructionsPage() {
  const t = useT()
  const [presets, setPresets] = useState<PromptPreset[]>([])
  const [promptLanguage, setPromptLanguage] = useState<BuiltinPromptLanguage>('zh-TW')
  const [promptLanguageChanging, setPromptLanguageChanging] = useState(false)
  const activePreset = useActivePreset()
  const activePresetId = activePreset.id
  const [editingPreset, setEditingPreset] = useState<PromptPreset | null>(null)
  const [appPromptRules, setAppPromptRules] = useState<AppPromptRule[]>([])
  // 編輯中的快捷鍵草稿：儲存時才落庫（取消不留痕，見 PromptPresetSection 的說明）
  const [editingShortcut, setEditingShortcut] = useState('')
  const [presetShortcuts, setPresetShortcutsState] = useState<Record<string, string>>({})

  useEffect(() => {
    void getBuiltinPromptLanguage().then(async (language) => {
      const loadedPresets = await getPromptPresets(language)
      setPromptLanguage(language)
      setPresets(loadedPresets)
    })
    getAppPromptRules().then(setAppPromptRules)
    getPresetShortcuts().then(setPresetShortcutsState)
    void refreshActivePreset()
  }, [])

  const handlePromptLanguageChange = async (language: BuiltinPromptLanguage) => {
    if (language === promptLanguage || promptLanguageChanging) return

    const previousLanguage = promptLanguage
    setPromptLanguageChanging(true)
    try {
      await setBuiltinPromptLanguage(language)
      const nextPresets = await getPromptPresets(language)
      setPromptLanguage(language)
      setPresets(nextPresets)
      setPromptPresetsCache(nextPresets)
      setEditingPreset(null)
      setEditingShortcut('')
      await refreshActivePreset()
    } catch (error) {
      console.error('[AIInstructionsPage] Failed to switch built-in prompt language:', error)
      try {
        await setBuiltinPromptLanguage(previousLanguage)
      } catch (rollbackError) {
        console.error('[AIInstructionsPage] Failed to restore built-in prompt language:', rollbackError)
      }
      setPromptLanguage(previousLanguage)
    } finally {
      setPromptLanguageChanging(false)
    }
  }

  // 預設切換快捷鍵不能和錄音熱鍵（免持 / 按住說話）相同，否則一次按鍵觸發兩個功能。
  // 預設之間的重複不在這裡攔：handleSetPresetShortcut 會自動把舊的清掉（後設的贏）。
  const validatePresetShortcut = async (value: string): Promise<string | null> => {
    if (!value) return null
    const ptt = await getSetting('shortcutPTT', 'AltRight') as string
    const handsFree = await getSetting('shortcutHandsFree', 'Alt+L') as string
    if (value === ptt) return t('aiInstructions.conflictPtt')
    if (value === handsFree) return t('aiInstructions.conflictHandsFree')
    return null
  }

  const handleSetPresetShortcut = async (presetId: string, accel: string) => {
    const next: Record<string, string> = { ...presetShortcuts }
    if (!accel) {
      delete next[presetId]
    } else {
      // 保證組合鍵唯一：若其它預設已佔用同一組合鍵，先清除，避免註冊衝突
      for (const key of Object.keys(next)) {
        if (next[key] === accel) delete next[key]
      }
      next[presetId] = accel
    }
    setPresetShortcutsState(next)
    await setPresetShortcuts(next)
    bridge.notifyShortcutsChanged()
  }

  const handleSelectPreset = (id: string) => {
    // 立即更新 UI 與錄音器快取（無 IPC），持久化寫入放到後台，避免快速切換時卡頓
    const target = presets.find((p) => p.id === id)
    setActivePresetKnown(id, target?.name || '')
    setActivePresetCache(id)
    void setActivePresetId(id)
  }

  const handleSavePreset = async (preset: PromptPreset) => {
    await savePromptPreset(preset)
    // 快捷鍵在這一刻才寫入：草稿期間不動庫，取消就當沒發生過
    if ((presetShortcuts[preset.id] || '') !== editingShortcut) {
      await handleSetPresetShortcut(preset.id, editingShortcut)
    }
    const nextPresets = await getPromptPresets()
    setPresets(nextPresets)
    setPromptPresetsCache(nextPresets)
    setEditingPreset(null)
    setEditingShortcut('')
    // 名稱可能已修改，重新整理當前預設狀態（標題欄/高亮）
    await refreshActivePreset()
  }

  const handleDeletePreset = async (id: string) => {
    await deletePromptPreset(id)
    const nextPresets = await getPromptPresets()
    setPresets(nextPresets)
    setPromptPresetsCache(nextPresets)
    // 清除該預設的快捷鍵對映，避免殘留註冊
    if (presetShortcuts[id]) {
      const next = { ...presetShortcuts }
      delete next[id]
      setPresetShortcutsState(next)
      await setPresetShortcuts(next)
      bridge.notifyShortcutsChanged()
    }
    if (id === activePresetId) {
      await setActivePresetId('intent')
      await refreshPreset()
      await refreshActivePreset()
    }
  }

  const handleNewPreset = () => {
    setEditingPreset({
      id: Date.now().toString(36),
      name: '',
      systemPrompt: '',
    })
    setEditingShortcut('')
  }

  /** 開始編輯：把該模式已有的快捷鍵讀進草稿 */
  const handleStartEditing = (preset: PromptPreset) => {
    setEditingPreset(preset)
    setEditingShortcut(presetShortcuts[preset.id] || '')
  }

  const handleCancelEditing = () => {
    setEditingPreset(null)
    setEditingShortcut('')
  }

  const handleMovePreset = async (from: number, to: number) => {
    await moveCustomPromptPreset(from, to)
    const nextPresets = await getPromptPresets()
    setPresets(nextPresets)
    setPromptPresetsCache(nextPresets)
  }

  /**
   * 應用規則的唯一寫入口：先更新介面，再落庫，最後重新整理錄音器快取。
   * 陣列順序就是命中優先順序（見 AppPromptRule 的註釋），所以這裡絕不排序。
   */
  const applyAppRules = async (nextRules: AppPromptRule[]) => {
    setAppPromptRules(nextRules)
    await saveAppPromptRules(nextRules)
    await refreshRecorderSettings()
  }

  const handleSaveAppRule = async (rule: AppPromptRule) => {
    await applyAppRules(appPromptRules.map((item) => (item.id === rule.id ? rule : item)))
  }

  /**
   * 開關即時生效，並且**啟用時把規則挪到最前面**。
   *
   * 一是它這就成了優先順序最高的規則（剛開啟的那條最該說話）；二是內建規則有 9 條、
   * 預設全關，使用者啟用靠底部的一條（比如 QQ）後，它仍埋在一堆沒啟用的規則中間，
   * 得翻半頁才找得到。置頂後"啟用中的規則"自然聚在頂部。
   * 關閉不挪位置：讓剛關掉的那條留在原處，方便反悔。
   */
  const handleToggleAppRule = async (ruleId: string, enabled: boolean) => {
    const target = appPromptRules.find((rule) => rule.id === ruleId)
    if (!target) return
    const updated = { ...target, enabled }
    await applyAppRules(enabled
      ? [updated, ...appPromptRules.filter((rule) => rule.id !== ruleId)]
      : appPromptRules.map((rule) => (rule.id === ruleId ? updated : rule)))
  }

  const handleMoveAppRule = async (from: number, to: number) => {
    const nextRules = moveItem(appPromptRules, from, to)
    if (nextRules === appPromptRules) return
    await applyAppRules(nextRules)
  }

  const handleCreateAppRule = async (draft: {
    name: string
    processNames: string[]
    presetId?: string
    promptAppend: string
  }) => {
    const id = `custom_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 6)}`
    const rule: AppPromptRule = {
      id,
      appId: id,
      name: draft.name,
      builtin: false,
      enabled: true,
      presetId: draft.presetId,
      promptAppend: draft.promptAppend,
      matcher: { processNames: draft.processNames, windowTitleIncludes: [], windowClasses: [], automationIds: [] },
    }
    // 新建即啟用，因此放到最前面（與"啟用置頂"一致），也免得新規則被內建規則搶先命中
    await applyAppRules([rule, ...appPromptRules])
  }

  const handleDeleteAppRule = async (ruleId: string) => {
    await applyAppRules(appPromptRules.filter((rule) => rule.id !== ruleId))
  }

  const handleResetAppRule = async (ruleId: string) => {
    const fallback = BUILTIN_APP_RULES.find((rule) => rule.id === ruleId)
    if (!fallback) return
    // 只恢復內容，不動它在列表裡的位置 —— 使用者排好的順序不該被"恢復預設"順手打亂
    await applyAppRules(appPromptRules.map((rule) => (
      rule.id === ruleId ? { ...fallback, matcher: { ...fallback.matcher } } : rule
    )))
  }

  return (
    <div className="mx-auto max-w-4xl">
      <PageTitle title={t('nav.aiInstructions')} info={t('aiInstructions.subtitle')} />

      <div className="space-y-6">
        <AIProofreadToggle />
        <ContextAwareSection />
        <HotwordPromptInjectToggle />

        <PromptPresetSection
          presets={presets}
          activePresetId={activePresetId}
          editingPreset={editingPreset}
          presetShortcuts={presetShortcuts}
          editingShortcut={editingShortcut}
          promptLanguage={promptLanguage}
          promptLanguageChanging={promptLanguageChanging}
          validateShortcut={validatePresetShortcut}
          onSelectPreset={handleSelectPreset}
          onPromptLanguageChange={handlePromptLanguageChange}
          onStartNewPreset={handleNewPreset}
          onStartEditing={handleStartEditing}
          onEditingPresetChange={setEditingPreset}
          onEditingShortcutChange={setEditingShortcut}
          onCancelEditing={handleCancelEditing}
          onSavePreset={handleSavePreset}
          onDeletePreset={handleDeletePreset}
          onMovePreset={handleMovePreset}
        />

        <AppPromptRulesSection
          presets={presets}
          rules={appPromptRules}
          onSaveRule={handleSaveAppRule}
          onToggleRule={handleToggleAppRule}
          onMoveRule={handleMoveAppRule}
          onResetRule={handleResetAppRule}
          onCreateRule={handleCreateAppRule}
          onDeleteRule={handleDeleteAppRule}
        />
      </div>
    </div>
  )
}
