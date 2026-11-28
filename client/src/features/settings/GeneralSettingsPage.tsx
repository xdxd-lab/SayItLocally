// 通用設定頁面 — 主題、快捷鍵、麥克風、懸浮窗、開機啟動、音訊保留、資料匯出

import * as bridge from '@/services/bridge'
import { refreshPTTSetting } from '@/services/webviewKeyboardFallback'
import { useCallback, useEffect, useRef, useState } from 'react'
import { Info } from 'lucide-react'
import { Card, CardContent } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Modal } from '@/components/ui/modal'
import { Tooltip } from '@/components/ui/tooltip'
import type { MicEndpoint } from '@/services/audio'
import { resolveMicPlan, type MicPlan } from '@/services/micRouting'
import { refreshRecorderSettings } from '@/services/recorder'
import { getPresetShortcuts, getSetting, setSetting } from '@/services/store'
import { getDefault } from '@/services/defaults'
import { Switch } from '@/components/ui/switch'
import { Segmented } from '@/components/ui/segmented'
import AppSection from './AppSection'
import BackupSection from './BackupSection'
import CorruptDbCopiesSection from './CorruptDbCopiesSection'
import { canConfirmHistoryClear, HISTORY_CLEAR_DELAY_MS } from './historyClearConfirmation'
// [自訂] S18／S35 WebDAV 設定區塊已註解停用：import WebDavSection from './WebDavSection'
import MicrophoneSection from './MicrophoneSection'
import { mountSettingsMicrophone, type MicVolumeLevel, type SettingsMicrophone } from './settingsMicrophone'
import { ComboShortcutInput, PTTShortcutInput } from './ShortcutInputs'
import { pttShortcutConflictsWithAccelerator } from '@/lib/shortcutKeys'
import { t, type LanguagePreference, type TranslationKey } from '@/i18n'
import { useT } from '@/i18n/useT'
import { getLanguagePreference, switchLanguage } from '@/stores/language'
import { IS_MAC } from '@/lib/platform'
import { MacPermissionsPanel } from '@/components/MacPermissionsPanel'

/** 語言名一律用該語言自己的寫法（English / 簡體中文），不做翻譯 —— 看不懂當前
 *  介面語言的人，正是靠認出自己的語言名才能切回去的。 */
const LANGUAGE_OPTIONS = [
  { value: 'auto', labelKey: 'language.auto' },
  { value: 'zh-TW', labelKey: 'language.zhTW' },
  { value: 'en', labelKey: 'language.en' },
] as const satisfies readonly { value: LanguagePreference; labelKey: TranslationKey }[]

function ShortcutLabel({ label, help }: { label: string; help: string }) {
  return (
    <span className="inline-flex items-center gap-1.5">
      <span>{label}</span>
      <Tooltip variant="light" content={help}>
        <Info
          aria-label={t('settings.helpAria', { label })}
          className="h-3.5 w-3.5 shrink-0 cursor-help text-muted-foreground/50 transition-colors hover:text-muted-foreground"
        />
      </Tooltip>
    </span>
  )
}

export default function GeneralSettingsPage() {
  const t = useT()
  const [languagePreference, setLanguagePreference] = useState<LanguagePreference>('auto')
  const [autoLaunch, setAutoLaunch] = useState(false)
  const [mics, setMics] = useState<MicEndpoint[]>([])
  const [selectedMic, setSelectedMic] = useState('')
  /** 跟隨系統預設時錄音實際會用哪個裝置（系統預設是虛擬音效卡時會換掉，見 micRouting.ts） */
  const [micPlan, setMicPlan] = useState<MicPlan | null>(null)
  const [testing, setTesting] = useState(false)
  const [volumeLevel, setVolumeLevel] = useState<MicVolumeLevel>('idle')
  const [revealingNames, setRevealingNames] = useState(false)
  const [micError, setMicError] = useState<TranslationKey | null>(null)
  const [muteSystemAudio, setMuteSystemAudio] = useState(false)
  const [protectClipboard, setProtectClipboard] = useState(true)
  // 兜底值統一從 defaults 取，不在這裡寫第二份字面量
  const [pttKey, setPttKey] = useState(() => getDefault<string>('shortcutPTT', ''))
  const [handsFreeKey, setHandsFreeKey] = useState('AltRight')
  const [aiToggleKey, setAiToggleKey] = useState('')
  const [historyEnabled, setHistoryEnabled] = useState(true)
  const [historyBusy, setHistoryBusy] = useState(false)
  const [historyError, setHistoryError] = useState<TranslationKey | null>(null)
  const [retainedHistory, setRetainedHistory] = useState<[number, number] | null>(null)
  const [historyClearReadyAt, setHistoryClearReadyAt] = useState<number | null>(null)
  const [historyClearUnlocked, setHistoryClearUnlocked] = useState(false)
  const historyClearInFlight = useRef(false)
  const confirmHistoryClear = historyClearReadyAt !== null

  useEffect(() => {
    if (historyClearReadyAt === null) return
    const timer = window.setTimeout(
      () => setHistoryClearUnlocked(true),
      Math.max(0, Math.ceil(historyClearReadyAt - performance.now())),
    )
    return () => window.clearTimeout(timer)
  }, [historyClearReadyAt])
  const [audioRetentionEnabled, setAudioRetentionEnabled] = useState(true)
  const [audioRetentionDays, setAudioRetentionDays] = useState(30)
  const [logRetentionDays, setLogRetentionDays] = useState(30)
  const [readySoundEnabled, setReadySoundEnabled] = useState(true)
  // 讀到已儲存值之前，開關先隱藏、不放動畫：避免「預設值 → 已儲存值」閃一下
  const [ready, setReady] = useState(false)
  // animate 與 ready 分開：ready 決定何時顯示，animate 決定何時允許過渡。
  // 若在揭開/賦值的同一幀就把 transition 加回來，按 CSS 規範瀏覽器會認為
  // 「有過渡且值變了」，於是把「預設值→已儲存值」真的動畫一遍（看起來就是閃一下）。
  // 所以揭開那一幀仍不帶過渡，隔兩幀待值穩定後才開過渡。
  const [animate, setAnimate] = useState(false)

  const canvasRef = useRef<HTMLCanvasElement | null>(null)
  const microphone = useRef<SettingsMicrophone | null>(null)
  useEffect(() => mountSettingsMicrophone(microphone, {
    onDevices: setMics,
    onSelectedMic: setSelectedMic,
    onTesting: setTesting,
    onRevealing: setRevealingNames,
    onVolume: setVolumeLevel,
    onError: setMicError,
    canvas: () => canvasRef.current,
  }), [])

  useEffect(() => {
    let cancelled = false
    // 開關類設定：先把值全部取回，再在同一個同步塊裡一次性落值 + 置 ready，讓 React
    // 合成一次渲染 —— 不存在「已顯示但值還沒到」的中間態（那正是開關閃一下的成因）。
    // 每項自帶 catch 兜底：Promise.all 是 fail-fast，只要一項 reject 就會在其餘項
    // 還沒回來時提前放行 ready。也不用 rAF，避免 setReady 與賦值分到不同批次。
    void (async () => {
      const [launch, mute, clip, history, retention, readySound, audioDays, logDays] = await Promise.all([
        bridge.getAutoLaunch().catch(() => false),
        getSetting('muteSystemAudioWhileRecording', false).catch(() => false),
        getSetting('protectClipboard', true).catch(() => true),
        getSetting('historyEnabled', false).catch(() => false),
        getSetting('audioRetentionEnabled', false).catch(() => false),
        getSetting('readySoundEnabled', true).catch(() => true),
        getSetting('audioRetentionDays', -1).catch(() => -1),
        getSetting('logRetentionDays', 30).catch(() => 30),
      ])
      if (cancelled) return
      setAutoLaunch(Boolean(launch))
      setMuteSystemAudio(Boolean(mute))
      setProtectClipboard(Boolean(clip))
      setHistoryEnabled(Boolean(history))
      setAudioRetentionEnabled(Boolean(retention))
      setReadySoundEnabled(Boolean(readySound))
      const ad = Number(audioDays)
      if (ad === 7 || ad === 30 || ad === 90 || ad === -1) setAudioRetentionDays(ad)
      const ld = Number(logDays)
      if (ld === 7 || ld === 15 || ld === 30 || ld === 90) setLogRetentionDays(ld)
      setReady(true)
      requestAnimationFrame(() => requestAnimationFrame(() => {
        if (!cancelled) setAnimate(true)
      }))
    })()
    getLanguagePreference().then(setLanguagePreference).catch(() => { })
    getSetting<string>('shortcutPTT').then((value) => setPttKey(value))
    getSetting('shortcutHandsFree', 'AltRight').then((value) => setHandsFreeKey(value as string))
    getSetting('shortcutToggleAi', '').then((value) => setAiToggleKey(value as string))
    return () => { cancelled = true }
  }, [])

  // 裝置列表到位後算一遍錄音會用哪個麥克風：設定頁的說明和「測試麥克風」都要跟錄音走同一條判斷
  useEffect(() => {
    if (mics.length === 0) return
    let cancelled = false
    resolveMicPlan(selectedMic, { webDevices: mics })
      .then(({ plan }) => { if (!cancelled) setMicPlan(plan) })
      .catch(() => { })
    return () => { cancelled = true }
  }, [mics, selectedMic])

  // 先切記憶體裡的語言讓介面立刻響應，再落庫。失敗時把選中項回滾到真實的持久化值，
  // 不留「按鈕已高亮但其實沒儲存」這種假成功。
  const handleLanguageChange = async (next: LanguagePreference) => {
    const previous = languagePreference
    setLanguagePreference(next)
    try {
      await switchLanguage(next)
    } catch {
      setLanguagePreference(previous)
      await switchLanguage(previous).catch(() => { })
    }
    // 托盤選單（以及 mac 的頂部應用選單）是原生的，不會跟著網頁換語言，要讓 Rust 改文字
    void bridge.refreshNativeMenus().catch(() => { })
  }
  const toggleAutoLaunch = async () => { const next = !autoLaunch; setAutoLaunch(next); await bridge.setAutoLaunch(next) }
  const handleMicChange = async (deviceId: string) => {
    if (!microphone.current) return
    await microphone.current.select(deviceId)
    await refreshRecorderSettings()
  }
  const toggleMuteSystemAudio = async () => { const next = !muteSystemAudio; setMuteSystemAudio(next); await setSetting('muteSystemAudioWhileRecording', next); await refreshRecorderSettings() }
  const toggleProtectClipboard = async () => { const next = !protectClipboard; setProtectClipboard(next); await setSetting('protectClipboard', next); await refreshRecorderSettings() }
  // [自訂] S61：關閉後預設保留舊資料。清除必須再確認一次。
  const toggleHistoryEnabled = async () => {
    const next = !historyEnabled
    setHistoryError(null)
    setHistoryBusy(true)
    try {
      await setSetting('historyEnabled', next)
      setHistoryEnabled(next)
      if (!next) {
        const counts = await bridge.historyRetainedCounts()
        if (counts.some(count => count > 0)) setRetainedHistory(counts)
      }
    } catch {
      setHistoryError('settings.history.changeFailed')
    } finally {
      setHistoryBusy(false)
    }
  }
  const closeHistoryPrompt = () => {
    setRetainedHistory(null)
    setHistoryClearReadyAt(null)
    setHistoryClearUnlocked(false)
  }
  const clearRetainedHistory = async (clickCount: number) => {
    if (
      !retainedHistory || historyClearInFlight.current || historyClearReadyAt === null
      || !canConfirmHistoryClear(performance.now(), historyClearReadyAt, clickCount)
    ) return
    historyClearInFlight.current = true
    setHistoryBusy(true)
    try {
      await bridge.historyClearRetained(...retainedHistory)
      void bridge.emit('history-updated')
      closeHistoryPrompt()
    } catch {
      setHistoryError('settings.history.clearFailed')
      closeHistoryPrompt()
      void bridge.emit('history-updated')
    } finally {
      historyClearInFlight.current = false
      setHistoryBusy(false)
    }
  }
  const toggleAudioRetention = async () => { const next = !audioRetentionEnabled; setAudioRetentionEnabled(next); await setSetting('audioRetentionEnabled', next) }
  const toggleReadySound = async () => { const next = !readySoundEnabled; setReadySoundEnabled(next); await setSetting('readySoundEnabled', next); await refreshRecorderSettings() }
  const handleAudioRetentionDaysChange = async (value: number) => { setAudioRetentionDays(value); await setSetting('audioRetentionDays', value) }
  const handleLogRetentionDaysChange = async (value: number) => { setLogRetentionDays(value); await setSetting('logRetentionDays', value) }
  const handlePTTChange = async (value: string) => { setPttKey(value); await setSetting('shortcutPTT', value); bridge.notifyShortcutsChanged(); refreshPTTSetting() }
  const handleHandsFreeChange = async (value: string) => { setHandsFreeKey(value); await setSetting('shortcutHandsFree', value); bridge.notifyShortcutsChanged(); refreshPTTSetting() }

  // 應用內部快捷鍵互斥：免持和按住說話綁同一個鍵，兩個功能會同時被觸發；
  // 預設切換的組合鍵同理。提交前攔下來，而不是等使用者按了才發現行為詭異。
  const validatePTT = useCallback(async (value: string) => {
    if (!value) return null
    if (pttShortcutConflictsWithAccelerator(value, handsFreeKey)) {
      return t('settings.shortcuts.conflictHandsFree')
    }
    if (pttShortcutConflictsWithAccelerator(value, aiToggleKey)) {
      return t('settings.shortcuts.conflictAiToggle')
    }
    const presetShortcuts = await getPresetShortcuts()
    if (Object.values(presetShortcuts).some(
      (shortcut) => pttShortcutConflictsWithAccelerator(value, shortcut),
    )) return t('settings.shortcuts.conflictPreset')
    return null
  }, [handsFreeKey, aiToggleKey])
  const validateHandsFree = useCallback(async (value: string) => {
    if (!value) return null
    if (pttShortcutConflictsWithAccelerator(pttKey, value)) {
      return t('settings.shortcuts.conflictPtt')
    }
    if (value === aiToggleKey) return t('settings.shortcuts.conflictAiToggle')
    const presetShortcuts = await getPresetShortcuts()
    if (Object.values(presetShortcuts).includes(value)) return t('settings.shortcuts.conflictPreset')
    return null
  }, [pttKey, aiToggleKey])

  const testMic = () => {
    const deviceId = selectedMic || (micPlan?.reason === 'virtual_avoided' ? micPlan.deviceId : '')
    void microphone.current?.test(deviceId)
  }

  return (
    <div className="mx-auto max-w-4xl p-8">
      <h1 className="mb-6 text-2xl font-bold">{t('settings.title')}</h1>
      <div className="space-y-6">
        {/* 介面語言排在最前：看不懂介面的人做不了下面任何一件事。
            這張卡自己走 t()，所以切換後它連自己的標題一起變，使用者能立刻確認生效了。 */}
        <Card>
          <CardContent className="p-6">
            <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
              <div className="min-w-0">
                <h2 className="text-lg font-semibold">{t('settings.general.language.title')}</h2>
                <p className="mt-1 text-sm text-muted-foreground">{t('settings.general.language.hint')}</p>
              </div>
              <div className="shrink-0" style={ready ? undefined : { visibility: 'hidden' }}>
                <Segmented
                  label={t('settings.general.language.title')}
                  value={languagePreference}
                  options={LANGUAGE_OPTIONS.map((opt) => ({ value: opt.value, label: t(opt.labelKey) }))}
                  onChange={(value) => void handleLanguageChange(value)}
                  animated={animate}
                  className="justify-end"
                />
              </div>
            </div>
          </CardContent>
        </Card>

        {/* macOS：輔助功能 / 麥克風授權。沒有它們熱鍵和插字都用不了，所以緊跟在語言後面 */}
        {IS_MAC && (
          <Card>
            <CardContent className="space-y-4 p-6">
              <h2 className="text-lg font-semibold">{t('settings.macPermissions.title')}</h2>
              <MacPermissionsPanel compact />
            </CardContent>
          </Card>
        )}

        <Card>
          <CardContent className="p-6">
            <h2 className="text-lg font-semibold">{t('settings.shortcuts.title')}</h2>
            {/* ⚠ 這是「按 Esc 能取消」在整個介面上的**唯一**出處 —— 懸浮窗上那行提示已經
                去掉了（見 overlay/Overlay.tsx 的 thinking 分支）。刪掉這句，這個能力就沒有
                任何地方告訴使用者了。
                錄音上限不在這裡講 —— 最後一分鐘懸浮窗會顯示剩餘時間、到點自動結束，介面自己會說。
                「錄音也不會保留」要寫明：取消和「錄了但沒出字」在使用者眼裡很容易混。
                「錄音或識別處理」是兩個階段，別連寫 —— Esc 在兩個階段都能按。 */}
            <p className="mb-4 mt-1 text-xs text-muted-foreground">
              {t('settings.shortcuts.escHint')}
            </p>
            <div className="space-y-4">
              <ComboShortcutInput
                value={handsFreeKey}
                onChange={handleHandsFreeChange}
                validate={validateHandsFree}
                label={<ShortcutLabel label={t('settings.shortcuts.handsFree')} help={t(IS_MAC ? 'settings.shortcuts.handsFreeHelpMac' : 'settings.shortcuts.handsFreeHelp')} />}
                description={t('settings.shortcuts.handsFreeDesc')}
              />
              <PTTShortcutInput
                value={pttKey}
                onChange={handlePTTChange}
                validate={validatePTT}
                label={<ShortcutLabel label={t('settings.shortcuts.ptt')} help={t(IS_MAC ? 'settings.shortcuts.pttHelpMac' : 'settings.shortcuts.pttHelp')} />}
                description={t('settings.shortcuts.pttDesc')}
              />
            </div>
          </CardContent>
        </Card>

        {/* 麥克風排在「偏好設定」前面：它是錄音鏈路的入口，選錯裝置什麼都錄不到；
            而下面那三個開關（提示音、靜音外放、剪貼簿保護）都是可選的錦上添花。 */}
        <MicrophoneSection mics={mics} selectedMic={selectedMic} testing={testing} volumeLevel={volumeLevel}
          virtualDefaultNote={!selectedMic && micPlan?.reason === 'virtual_avoided'
            ? t('mic.virtualDefaultNote', { virtual: micPlan.avoidedName, device: micPlan.chosenName })
            : ''}
          onCanvasRef={(node) => { canvasRef.current = node }}
          onMicChange={handleMicChange} onTestMic={testMic}
          onRevealNames={() => void microphone.current?.revealNames()} revealingNames={revealingNames}
          errorMessage={micError ? t(micError) : ''} />

        <Card>
          <CardContent className="space-y-4 p-6">
            <h2 className="text-lg font-semibold">{t('settings.prefs.title')}</h2>
            <div className="flex items-center justify-between">
              <div>
                <p className="text-sm font-medium">{t('settings.prefs.readySound')}</p>
                <p className="text-xs text-muted-foreground">{t('settings.prefs.readySoundDesc')}</p>
              </div>
              <Switch checked={readySoundEnabled} onChange={() => void toggleReadySound()} noAnimation={!animate} hidden={!ready} />
            </div>
            <div className="flex items-center justify-between border-t border-border pt-4">
              <div>
                <p className="text-sm font-medium">{t('settings.prefs.muteSystemAudio')}</p>
                <p className="text-xs text-muted-foreground">{t('settings.prefs.muteSystemAudioDesc')}</p>
              </div>
              <Switch checked={muteSystemAudio} onChange={() => void toggleMuteSystemAudio()} noAnimation={!animate} hidden={!ready} />
            </div>
            <div className="flex items-center justify-between border-t border-border pt-4">
              <div>
                <p className="text-sm font-medium">{t('settings.prefs.protectClipboard')}</p>
                <p className="text-xs text-muted-foreground">{t('settings.prefs.protectClipboardDesc')}</p>
              </div>
              <Switch checked={protectClipboard} onChange={() => void toggleProtectClipboard()} noAnimation={!animate} hidden={!ready} />
            </div>
          </CardContent>
        </Card>

        <AppSection autoLaunch={autoLaunch} onToggleAutoLaunch={toggleAutoLaunch} ready={ready} animate={animate} />

        <Card>
          <CardContent className="p-6">
            <div className="flex items-start justify-between gap-4">
              <div>
                <h2 className="text-lg font-semibold">{t('settings.history.title')}</h2>
                <p className="mt-1 text-sm text-muted-foreground">{t('settings.history.desc')}</p>
              </div>
              <Switch checked={historyEnabled} onChange={() => void toggleHistoryEnabled()} disabled={historyBusy || retainedHistory !== null} noAnimation={!animate} hidden={!ready} />
            </div>
            {historyError && <p role="alert" className="mt-3 text-sm text-destructive">{t(historyError)}</p>}
            {retainedHistory && (
              <Modal
                key={String(confirmHistoryClear)}
                title={t(confirmHistoryClear ? 'settings.history.clearTitle' : 'settings.history.retainedTitle')}
                onClose={closeHistoryPrompt}
                locked={historyBusy}
                panelClassName="w-96"
              >
                <p className="mt-2 text-sm text-muted-foreground">
                  {t(confirmHistoryClear ? 'settings.history.clearConfirm' : 'settings.history.retainedCount', {
                    records: retainedHistory[0],
                    recordings: retainedHistory[1],
                  })}
                </p>
                <div className="mt-5 flex justify-end gap-2">
                  <Button variant="outline" data-modal-autofocus disabled={historyBusy} onClick={closeHistoryPrompt}>
                    {t('settings.history.keep')}
                  </Button>
                  <Button
                    variant="destructive"
                    disabled={historyBusy || (confirmHistoryClear && !historyClearUnlocked)}
                    onClick={(event) => {
                      if (confirmHistoryClear) {
                        void clearRetainedHistory(event.detail)
                      } else if (event.detail <= 1) {
                        setHistoryClearReadyAt(performance.now() + HISTORY_CLEAR_DELAY_MS)
                      }
                    }}
                  >
                    {t('settings.history.clear')}
                  </Button>
                </div>
              </Modal>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardContent className="p-6">
            <div className="flex items-start justify-between gap-4">
              <div>
                <h2 className="text-lg font-semibold">{t('settings.audio.title')}</h2>
                <p className="mt-1 text-sm text-muted-foreground">{t('settings.audio.desc')}</p>
              </div>
              <Switch checked={audioRetentionEnabled} onChange={() => void toggleAudioRetention()} noAnimation={!animate} hidden={!ready} />
            </div>
            {audioRetentionEnabled && (
              <div className="mt-4 flex flex-col gap-2 border-t border-border pt-4 sm:flex-row sm:items-center sm:justify-between">
                <label className="text-sm text-muted-foreground">{t('settings.audio.retentionLabel')}</label>
                <div className="shrink-0" style={ready ? undefined : { visibility: 'hidden' }}>
                  <Segmented
                    label={t('settings.audio.retentionLabel')}
                    value={audioRetentionDays}
                    options={([{ value: 7, labelKey: 'settings.retention.7d' }, { value: 30, labelKey: 'settings.retention.1m' }, { value: 90, labelKey: 'settings.retention.3m' }, { value: -1, labelKey: 'settings.retention.forever' }] as const satisfies readonly { value: number; labelKey: TranslationKey }[]).map((opt) => ({ value: opt.value, label: t(opt.labelKey) }))}
                    onChange={(value) => void handleAudioRetentionDaysChange(value)}
                    animated={animate}
                    className="justify-end"
                  />
                </div>
              </div>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardContent className="p-6">
            <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
              <div className="min-w-0">
                <h2 className="text-lg font-semibold">{t('settings.log.title')}</h2>
                <p className="mt-1 text-sm text-muted-foreground">{t('settings.log.desc')}</p>
              </div>
              <div className="shrink-0" style={ready ? undefined : { visibility: 'hidden' }}>
                <Segmented
                  label={t('settings.log.title')}
                  value={logRetentionDays}
                  options={([{ value: 7, labelKey: 'settings.retention.7d' }, { value: 15, labelKey: 'settings.retention.15d' }, { value: 30, labelKey: 'settings.retention.1m' }, { value: 90, labelKey: 'settings.retention.3m' }] as const satisfies readonly { value: number; labelKey: TranslationKey }[]).map((opt) => ({ value: opt.value, label: t(opt.labelKey) }))}
                  onChange={(value) => void handleLogRetentionDaysChange(value)}
                  animated={animate}
                  className="justify-end"
                />
              </div>
            </div>
          </CardContent>
        </Card>

        <CorruptDbCopiesSection />
        <BackupSection />

        {/* [自訂] S18／S35 WebDAV 設定區塊已註解停用：<WebDavSection /> */}
      </div>
    </div>
  )
}
