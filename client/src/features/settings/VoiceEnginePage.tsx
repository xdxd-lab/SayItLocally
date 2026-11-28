// 語音引擎設定頁面 — 工作模式 + ASR 識別服務配置 + 識別測試

import { useEffect, useState } from 'react'
import { ChevronDown } from 'lucide-react'
import { getSetting, setSetting } from '@/services/store'
import { switchProvider, getWorkMode, type WorkMode } from '@/services/transcription'
import { refreshRecorderSettings, reconnectProvider } from '@/services/recorder'
import { refreshModeStatus } from '@/stores/modeStatus'
import { setEngineDraftDirty } from '@/stores/engineDraft'
import WorkModeSection from './WorkModeSection'
import LocalModeSection, { LocalModeAdvancedSection } from './LocalModeSection'
import AsrTestSection from './AsrTestSection'
import { useT } from '@/i18n/useT'
import { PageTitle } from '@/components/ui/info-tip'

export default function VoiceEnginePage() {
  const t = useT()
  const [workMode, setWorkMode] = useState<WorkMode>(getWorkMode)
  // 本地模式的次級設定預設收起。原來這 4 張卡與"選模型"完全等權地平鋪在一起，
  // 本地模式一屏 7 個同級標題、27 個控制元件，使用者沒法判斷哪幾個是必須做的。
  const [advancedOpen, setAdvancedOpen] = useState(false)

  useEffect(() => {
    setWorkMode('local') // [自訂 local-only]
  }, [])

  const handleWorkModeChange = async (mode: WorkMode) => {
    // 換模式會整體替換下方的配置卡，上一套裡沒儲存的輸入隨之消失——髒標記要跟著清，
    // 否則「識別測試」會一直以為有未儲存的改動
    setEngineDraftDirty(false)
    mode = 'local' // [自訂 local-only]
    setWorkMode(mode)
    await setSetting('workMode', mode)
    await switchProvider(mode)
    await refreshRecorderSettings()
    reconnectProvider()
    void refreshModeStatus() // 同步左下角的引擎指示與右上角的就緒徽標
  }

  return (
    <div className="mx-auto max-w-4xl">
      <PageTitle title={t('nav.voiceEngine')} info={t('voiceEngine.subtitle')} />

      <div className="space-y-6">
        <WorkModeSection value={workMode} onChange={(m) => void handleWorkModeChange(m)} />

        {/* id 供「工作模式」右上角的「待配置」徽標點選後滾動定位 */}
        <div id="engine-config" className="space-y-6">
          {workMode === 'local' && <LocalModeSection />}
        </div>

        <AsrTestSection workMode={workMode} />

        {workMode === 'local' && (
          <section className="space-y-6">
            <button
              type="button"
              aria-expanded={advancedOpen}
              aria-controls="local-advanced"
              onClick={() => setAdvancedOpen(!advancedOpen)}
              className="flex w-full items-center gap-2 rounded-md px-1 py-2 text-left text-sm text-muted-foreground transition-colors hover:text-foreground"
            >
              <ChevronDown
                className={`h-4 w-4 shrink-0 transition-transform ${advancedOpen ? 'rotate-180' : ''}`}
                aria-hidden
              />
              <span className="font-medium">{t('voiceEngine.advanced')}</span>
              <span className="text-xs">{t('voiceEngine.advancedDesc')}</span>
              <span className="h-px flex-1 bg-border" aria-hidden />
            </button>

            {advancedOpen && (
              <div id="local-advanced" className="space-y-6">
                <LocalModeAdvancedSection />
              </div>
            )}
          </section>
        )}
      </div>
    </div>
  )
}
