// AI 整理開關、快捷鍵與短語音門檻（狀態接入全域性 store）

import { Card, CardContent } from '@/components/ui/card'
import { Switch } from '@/components/ui/switch'
import { useAiEnabled } from '@/hooks/useAiEnabled'
import { toggleAiEnabled } from '@/stores/aiEnabled'
import { useT } from '@/i18n/useT'
import { ComboShortcutInput } from './ShortcutInputs'
import { useEffect, useState } from 'react'
import { getPresetShortcuts, getSetting, setSetting } from '@/services/store'
import { refreshRecorderSettings } from '@/services/recorder'
import { pttShortcutConflictsWithAccelerator } from '@/lib/shortcutKeys'
import * as bridge from '@/services/bridge'
import { MAX_RECORDING_SEC } from '@/services/recorder/types'

export default function AIProofreadToggle() {
  const t = useT()
  const aiEnabled = useAiEnabled()
  const [shortcut, setShortcut] = useState('')
  const [minDurationSec, setMinDurationSec] = useState(0)

  useEffect(() => {
    void Promise.all([
      getSetting('shortcutToggleAi', ''),
      getSetting('aiMinDurationSec', 0),
    ]).then(([savedShortcut, savedMinDuration]) => {
      setShortcut(String(savedShortcut || ''))
      setMinDurationSec(Math.max(0, Math.min(MAX_RECORDING_SEC, Number(savedMinDuration) || 0)))
    })
  }, [])

  const validateShortcut = async (value: string) => {
    const [ptt, handsFree, presetShortcuts] = await Promise.all([
      getSetting<string>('shortcutPTT', 'ControlRight'),
      getSetting<string>('shortcutHandsFree', 'AltRight'),
      getPresetShortcuts(),
    ])
    if (pttShortcutConflictsWithAccelerator(ptt, value)) return t('aiProofread.shortcutConflictPtt')
    if (handsFree === value) return t('aiProofread.shortcutConflictHandsFree')
    if (Object.values(presetShortcuts).includes(value)) return t('aiProofread.shortcutConflictPreset')
    return null
  }

  const saveShortcut = async (value: string) => {
    setShortcut(value)
    await setSetting('shortcutToggleAi', value)
    bridge.notifyShortcutsChanged()
  }

  const saveMinDuration = async (next: number) => {
    setMinDurationSec(next)
    await setSetting('aiMinDurationSec', next)
    await refreshRecorderSettings()
  }

  const handleMinDurationChange = (value: string, input: HTMLInputElement) => {
    const next = value === '' ? 0 : Math.max(0, Math.min(MAX_RECORDING_SEC, Math.round(Number(value) || 0)))
    // 數值不變時 React 會跳過重渲染，瀏覽器便會保留 `00` 之類的原始輸入；
    // 直接回寫規範值，使 0 始終只有一個，也順便限制在 0–300 內。
    input.value = String(next)
    void saveMinDuration(next)
  }

  // 數字本身不如「實際會怎樣」容易理解，所以把生效結果做成緊跟標籤的小徽標。
  // 原先它單獨佔一行右對齊，既不貼標籤也不貼輸入框，還得靠手算的 margin 才對得齊
  // （中英各一套分支）——挪到標籤後面之後那些魔法數就不需要了。
  const minDurationEffect = minDurationSec === 0
    ? t('aiProofread.minDurationDefaultHint')
    : t('aiProofread.minDurationActiveHint', { seconds: minDurationSec })
  // 只有非預設值才上主色：預設的「始終整理」是常態，不該搶注意力；
  // 使用者真的設了門檻才值得被看見（與 ServerSection 的「未儲存」徽標同一套邏輯）。
  const effectToneClass = minDurationSec === 0
    ? 'bg-muted text-muted-foreground'
    : 'bg-primary/10 text-primary'

  return (
    <Card>
      <CardContent className="p-6">
        {/* min-w-0 + gap：最小視窗下說明文字要能擠，不能把開關頂出卡片 */}
        <div className="flex items-center justify-between gap-4">
          <div className="min-w-0">
            <h2 id="ai-proofread-heading" className="text-lg font-semibold">{t('titleBar.aiCleanup')}</h2>
            <p className="mt-1 text-xs text-muted-foreground">
              {aiEnabled ? t('aiProofread.onDesc') : t('aiProofread.offDesc')}
            </p>
          </div>
          {/* 開關原來既沒有 label 也沒有 aria-label，相鄰的標題也沒關聯——讀屏唸到的是
              一個沒有名字的「切換按鈕」 */}
          <Switch
            checked={aiEnabled}
            onChange={() => { void toggleAiEnabled() }}
            labelledBy="ai-proofread-heading"
            className="shrink-0"
          />
        </div>
        <div className="mt-5 space-y-4">
          <ComboShortcutInput
            value={shortcut}
            onChange={saveShortcut}
            validate={validateShortcut}
            comboOnly
            allowMouseShortcut
            label={t('aiProofread.shortcutLabel')}
            description={t('aiProofread.shortcutDesc')}
          />
          {/* 這一項只在 AI 整理開啟時才有意義。關閉時不能只是留著讓人填 ——
              原來照樣可編輯，使用者填完一個數字，它卻根本不參與判斷，介面還一聲不響。
              所以關閉時真的 disabled（讀屏也能聽到），並把說明換成「不生效」的原因。
              上面的快捷鍵那行不跟著灰：它正是用來把 AI 整理開啟的手段。 */}
          <div>
            <div className="flex items-center justify-between gap-5">
              <div className="min-w-0">
                <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                  <p
                    id="ai-min-duration-label"
                    className={`text-sm font-medium ${aiEnabled ? '' : 'text-muted-foreground'}`}
                  >
                    {t('aiProofread.minDurationLabel')}
                  </p>
                  {aiEnabled && (
                    <span
                      id="ai-min-duration-effect"
                      className={`shrink-0 rounded-full px-2 py-0.5 text-xs font-medium ${effectToneClass}`}
                    >
                      {minDurationEffect}
                    </span>
                  )}
                </div>
                <p id="ai-min-duration-desc" className="mt-0.5 text-xs leading-relaxed text-muted-foreground">
                  {aiEnabled ? t('aiProofread.minDurationDesc') : t('aiProofread.minDurationDisabled')}
                </p>
              </div>
              <label className="mr-2 flex shrink-0 items-center gap-1.5">
                <input
                  type="number"
                  min="0"
                  max={MAX_RECORDING_SEC}
                  step="1"
                  value={minDurationSec}
                  disabled={!aiEnabled}
                  placeholder={t('aiProofread.minDurationOff')}
                  onChange={(event) => handleMinDurationChange(event.target.value, event.currentTarget)}
                  className="h-8 w-16 rounded-md border border-input bg-background px-2 text-right text-sm tabular-nums outline-none transition-colors placeholder:text-muted-foreground focus:border-primary focus:ring-2 focus:ring-primary/20 disabled:cursor-not-allowed disabled:opacity-50"
                  aria-labelledby="ai-min-duration-label"
                  aria-describedby={aiEnabled ? 'ai-min-duration-effect ai-min-duration-desc' : 'ai-min-duration-desc'}
                />
                <span className={`text-sm ${aiEnabled ? 'text-muted-foreground' : 'text-muted-foreground/50'}`}>
                  {t('aiProofread.seconds')}
                </span>
              </label>
            </div>
          </div>
        </div>
      </CardContent>
    </Card>
  )
}
