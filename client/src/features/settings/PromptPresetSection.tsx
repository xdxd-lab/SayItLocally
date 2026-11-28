import { useEffect, useRef, useState } from 'react'
import { Pencil, Plus, Trash2 } from 'lucide-react'
import { useSortable, DragHandle } from '@/components/ui/sortable'
import { Card, CardContent } from '@/components/ui/card'
import { Select } from '@/components/ui/select'
import { Segmented } from '@/components/ui/segmented'
import {
  getBuiltinPromptPresets,
  type BuiltinPromptLanguage,
  type PromptPreset,
} from '@/services/store'
import { ComboShortcutInput, type ShortcutValidate } from './ShortcutInputs'
import { displayAccelerator } from './utils'
import { useT } from '@/i18n/useT'
import { promptPresetDisplayName } from '@/i18n/displayNames'
import { IS_MAC } from '@/lib/platform'
import type { TranslationKey } from '@/i18n'
import { InfoTip } from '@/components/ui/info-tip'

/** 內建潤色模式的一句話說明（按 id）。使用者改過內建提示詞也還是這個模式，說明不變。 */
const BUILTIN_PRESET_DESC: Record<string, TranslationKey> = {
  intent: 'promptPreset.builtinDesc.intent',
  faithful: 'promptPreset.builtinDesc.faithful',
  zh2en: 'promptPreset.builtinDesc.zh2en',
  casual: 'promptPreset.builtinDesc.casual',
}

export default function PromptPresetSection({
  presets,
  activePresetId,
  editingPreset,
  presetShortcuts,
  editingShortcut,
  promptLanguage,
  promptLanguageChanging,
  onSelectPreset,
  onPromptLanguageChange,
  onStartNewPreset,
  onStartEditing,
  onEditingPresetChange,
  onEditingShortcutChange,
  onCancelEditing,
  onSavePreset,
  onDeletePreset,
  onMovePreset,
  validateShortcut,
}: {
  presets: PromptPreset[]
  activePresetId: string
  editingPreset: PromptPreset | null
  presetShortcuts: Record<string, string>
  /** 編輯中的快捷鍵草稿。**不即時落庫** —— 見下方 onEditingShortcutChange 的說明 */
  editingShortcut: string
  promptLanguage: BuiltinPromptLanguage
  promptLanguageChanging?: boolean
  onSelectPreset: (id: string) => void
  onPromptLanguageChange: (language: BuiltinPromptLanguage) => void
  onStartNewPreset: () => void
  onStartEditing: (preset: PromptPreset) => void
  onEditingPresetChange: (preset: PromptPreset) => void
  /**
   * 只改草稿，儲存時才寫庫。
   * 以前這裡是即時寫庫的，於是：給還沒儲存的新模式設了快捷鍵再點取消，會留下一條指向
   * 不存在模式的快捷鍵；而且寫入時為保證組合鍵唯一會把佔用同一組合鍵的舊模式清掉，
   * 取消後那個舊模式的快捷鍵也回不來了。
   */
  onEditingShortcutChange: (accelerator: string) => void
  onCancelEditing: () => void
  onSavePreset: (preset: PromptPreset) => void
  onDeletePreset: (id: string) => void
  /** 拖拽調整自定義模式的順序（下標是在"自定義"這一段裡的下標；內建順序固定） */
  onMovePreset: (from: number, to: number) => void
  validateShortcut?: ShortcutValidate
}) {
  const t = useT()
  const presetSortable = useSortable({ onMove: onMovePreset })
  const nameInputRef = useRef<HTMLInputElement>(null)
  const [templateId, setTemplateId] = useState('')
  const editingPresetName = editingPreset ? promptPresetDisplayName(editingPreset) : ''

  // 是否在"新建"（草稿的 id 還不在已有列表裡）
  const isCreating = !!editingPreset && !presets.some((preset) => preset.id === editingPreset.id)

  // 開啟表單時把焦點放到名稱框。
  // 依賴**只能**是 id / builtin 這類原始值：editingPreset 每次輸入都是新物件，
  // 把它列進依賴會導致每敲一個字都重新聚焦，游標從提示詞框被拽回名稱框。
  const editingId = editingPreset?.id ?? null
  const editingBuiltin = !!editingPreset?.builtin
  useEffect(() => {
    if (!editingId) {
      setTemplateId('')
      return
    }
    if (!editingBuiltin) nameInputRef.current?.focus()
  }, [editingId, editingBuiltin])

  const applyTemplate = (id: string) => {
    setTemplateId(id)
    if (!id || !editingPreset) return
    const source = presets.find((preset) => preset.id === id)
    if (source) onEditingPresetChange({ ...editingPreset, systemPrompt: source.systemPrompt })
  }

  const missingName = !!editingPreset && !editingPreset.name.trim()
  const missingPrompt = !!editingPreset && !editingPreset.systemPrompt.trim()

  return (
    <Card>
      <CardContent className="p-6">
        <div className="mb-4 flex flex-wrap items-start justify-between gap-3">
          <div className="min-w-0 grow basis-[18rem]">
            <div className="flex items-center gap-1.5">
              <h2 className="text-lg font-semibold">{t('promptPreset.title')}</h2>
              <InfoTip content={t('promptPreset.desc')} label={t('promptPreset.title')} />
            </div>
          </div>
          <div className="flex shrink-0 flex-wrap items-center gap-2">
            <Segmented
              label={t('promptPreset.promptLanguage')}
              value={promptLanguage}
              options={[
                { value: 'zh-TW', label: t('promptPreset.languageZh') },
                { value: 'en', label: t('promptPreset.languageEn') },
              ]}
              onChange={onPromptLanguageChange}
              size="sm"
              disabled={promptLanguageChanging}
              className="gap-1"
            />
            <button
              onClick={onStartNewPreset}
              className="flex items-center gap-1 rounded-md border px-2 py-1 text-xs transition-colors hover:bg-accent"
            >
              <Plus className="h-3 w-3" /> {t('promptPreset.new')}
            </button>
          </div>
        </div>

        {/* 編輯/新建表單放在列表**上方**：以前在整個列表下面，列表一長就在螢幕外，
            點了「新建」看著像沒反應。 */}
        {editingPreset && (
          <div className="mb-4 space-y-3 rounded-lg border border-primary/30 bg-muted p-4">
            <p className="text-sm font-medium">{isCreating ? t('promptPreset.createTitle') : t('promptPreset.editTitle', { name: editingPresetName })}</p>

            <div>
              <label className="mb-1.5 block text-xs font-medium text-foreground">{t('promptPreset.name')}</label>
              <input
                ref={nameInputRef}
                value={editingPreset.builtin ? editingPresetName : editingPreset.name}
                onChange={(event) => onEditingPresetChange({ ...editingPreset, name: event.target.value })}
                placeholder={t('promptPreset.namePlaceholder')}
                className="h-9 w-full rounded-md border border-input-border bg-input-bg px-3 text-sm"
                disabled={editingPreset.builtin}
              />
            </div>

            {/* 新建時給個起點：從零寫 System Prompt 門檻太高，內建模式本身就是好模板 */}
            {isCreating && (
              <div>
                <label className="mb-1.5 block text-xs font-medium text-foreground">{t('promptPreset.template')}</label>
                <Select
                  value={templateId}
                  onChange={applyTemplate}
                  options={[
                    { value: '', label: t('promptPreset.noTemplate') },
                    ...presets.map((preset) => ({
                      value: preset.id,
                      label: preset.builtin
                        ? t('promptPreset.builtinSuffix', { name: promptPresetDisplayName(preset) })
                        : preset.name,
                    })),
                  ]}
                />
                <p className="mt-1.5 text-xs text-muted-foreground">
                  {t('promptPreset.templateHint')}
                </p>
              </div>
            )}

            <div>
              <label className="mb-1.5 block text-xs font-medium text-foreground">{t('promptPreset.systemPrompt')}</label>
              <p className="mb-2 text-xs text-muted-foreground">
                {t('promptPreset.systemPromptHint')}
              </p>
              <textarea
                value={editingPreset.systemPrompt}
                onChange={(event) => onEditingPresetChange({ ...editingPreset, systemPrompt: event.target.value })}
                placeholder={t('promptPreset.systemPromptPlaceholder')}
                rows={8}
                className="w-full resize-y rounded-md border border-input-border bg-input-bg px-3 py-2 text-xs leading-normal"
              />
            </div>

            <div className="border-t border-border/60 pt-3">
              <ComboShortcutInput
                value={editingShortcut}
                onChange={onEditingShortcutChange}
                validate={validateShortcut}
                label={t('promptPreset.shortcut')}
                description={t(IS_MAC ? 'promptPreset.shortcutHintMac' : 'promptPreset.shortcutHint')}
                comboOnly
              />
            </div>

            <div className="flex items-center justify-end gap-2">
              {/* 儲存不可點時說明原因，而不是幹灰著 */}
              {(missingName || missingPrompt) && (
                <span className="mr-auto text-xs text-muted-foreground">
                  {missingName && missingPrompt
                    ? t('promptPreset.missingBoth')
                    : missingName ? t('promptPreset.missingName') : t('promptPreset.missingPrompt')}
                </span>
              )}

              {editingPreset.builtin && (
                <button
                  onClick={() => {
                    const language = editingPreset.builtinPromptLanguage ?? promptLanguage
                    const original = getBuiltinPromptPresets(language)
                      .find((builtin) => builtin.id === editingPreset.id)
                    if (original) onEditingPresetChange({ ...original })
                  }}
                  className="px-2 py-1 text-xs text-muted-foreground transition-colors hover:text-foreground"
                >
                  {t('promptPreset.restoreDefault')}
                </button>
              )}

              <button
                onClick={onCancelEditing}
                className="rounded-md border px-3 py-1 text-xs transition-colors hover:bg-accent"
              >
                {t('promptPreset.cancel')}
              </button>
              <button
                disabled={missingName || missingPrompt}
                onClick={() => onSavePreset(editingPreset)}
                className="rounded-md bg-primary px-3 py-1 text-xs text-primary-foreground transition-colors hover:bg-primary/90 disabled:opacity-50"
              >
                {t('promptPreset.save')}
              </button>
            </div>
          </div>
        )}

        <div className="space-y-2">
          {presets.map((preset) => {
            const customPresets = presets.filter((item) => !item.builtin)
            const customIndex = customPresets.findIndex((item) => item.id === preset.id)
            // 只有自定義模式能拖，但一旦出現拖拽柄，內建行也要留出同寬的位置，
            // 否則自定義行的單選圈和名字會比內建行整體右移一截，上下對不齊。
            const sortEnabled = customPresets.length > 1
            const canMove = !preset.builtin && sortEnabled
            const presetName = promptPresetDisplayName(preset)
            return (
              <div
                key={preset.id}
                {...(canMove ? presetSortable.rowProps(customIndex) : {})}
                className={`group cursor-pointer rounded-lg border p-3 transition-colors ${activePresetId === preset.id
                  ? 'border-primary/40 bg-primary/5'
                  : 'border-border hover:bg-accent/50'
                  } ${canMove ? presetSortable.rowClassName(customIndex) : ''}`}
                onClick={() => onSelectPreset(preset.id)}
              >
                <div className="flex items-center justify-between gap-2">
                  <div className="flex min-w-0 items-center gap-3">
                    {/* -ml-1 抵掉手柄自帶的 p-1，讓圖示左緣貼住卡片內邊距，柄列不至於太寬 */}
                    {canMove && (
                      <DragHandle
                        className="-ml-1"
                        {...presetSortable.handleProps(customIndex, t('promptPreset.drag', { name: presetName }))}
                        onClick={(event) => event.stopPropagation()}
                      />
                    )}
                    {!canMove && sortEnabled && (
                      <div aria-hidden className="-ml-1 box-content h-3.5 w-3.5 shrink-0 p-1" />
                    )}
                    <div
                      className={`flex h-4 w-4 shrink-0 items-center justify-center rounded-full border-2 ${activePresetId === preset.id ? 'border-primary' : 'border-muted-foreground/40'
                        }`}
                    >
                      {activePresetId === preset.id && <div className="h-2 w-2 rounded-full bg-primary" />}
                    </div>

                    <div className="min-w-0">
                      <div className="flex items-center gap-2">
                        <p className="text-sm font-medium">{presetName}</p>
                        {preset.builtin && (
                          <span className="rounded border px-1 text-xs text-muted-foreground">{t('promptPreset.builtin')}</span>
                        )}
                        {preset.builtinPromptModified && (
                          <span className="rounded border border-warning/40 bg-warning/10 px-1 text-xs text-warning-strong">
                            {preset.builtinPromptUpdateAvailable
                              ? t('promptPreset.updateAvailable')
                              : t('promptPreset.modified')}
                          </span>
                        )}
                        {presetShortcuts[preset.id] && (
                          <span className="rounded border border-primary/30 bg-primary/5 px-1.5 text-xs text-muted-foreground">
                            {displayAccelerator(presetShortcuts[preset.id]).join('+')}
                          </span>
                        )}
                      </div>
                      {/* 原來顯示提示詞原文的前 60 個字（「你是語音文本精煉助手。輸入是 ASR……」），
                          看不出幾個模式的區別。內建模式改成一句說明；自建模式沒有說明，只顯示名字。 */}
                      {BUILTIN_PRESET_DESC[preset.id] && (
                        <p className="truncate text-xs text-muted-foreground">{t(BUILTIN_PRESET_DESC[preset.id])}</p>
                      )}
                    </div>
                  </div>

                  <div className="ml-2 flex shrink-0 items-center gap-1">
                    <button
                      onClick={(event) => {
                        event.stopPropagation()
                        onStartEditing({ ...preset })
                      }}
                      className="rounded p-1.5 hover:bg-accent"
                      aria-label={t('promptPreset.edit')}
                    >
                      <Pencil className="h-3.5 w-3.5 text-muted-foreground" />
                    </button>

                    {!preset.builtin && (
                      <button
                        onClick={(event) => {
                          event.stopPropagation()
                          onDeletePreset(preset.id)
                        }}
                        className="rounded p-1.5 hover:bg-accent"
                        aria-label={t('promptPreset.delete')}
                      >
                        <Trash2 className="h-3.5 w-3.5 text-destructive" />
                      </button>
                    )}
                  </div>
                </div>
              </div>
            )
          })}
        </div>
      </CardContent>
    </Card>
  )
}
