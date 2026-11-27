// 通用设置页面 — 主题、快捷键、麦克风、悬浮窗、开机启动、音频保留、数据导出

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

/** 语言名一律用该语言自己的写法（English / 简体中文），不做翻译 —— 看不懂当前
 *  界面语言的人，正是靠认出自己的语言名才能切回去的。 */
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
  /** 跟随系统默认时录音实际会用哪个设备（系统默认是虚拟声卡时会换掉，见 micRouting.ts） */
  const [micPlan, setMicPlan] = useState<MicPlan | null>(null)
  const [testing, setTesting] = useState(false)
  const [volumeLevel, setVolumeLevel] = useState<MicVolumeLevel>('idle')
  const [revealingNames, setRevealingNames] = useState(false)
  const [micError, setMicError] = useState<TranslationKey | null>(null)
  const [muteSystemAudio, setMuteSystemAudio] = useState(false)
  const [protectClipboard, setProtectClipboard] = useState(true)
  // 兜底值统一从 defaults 取，不在这里写第二份字面量
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
  // 读到已保存值之前，开关先隐藏、不放动画：避免「默认值 → 已保存值」闪一下
  const [ready, setReady] = useState(false)
  // animate 与 ready 分开：ready 决定何时显示，animate 决定何时允许过渡。
  // 若在揭开/赋值的同一帧就把 transition 加回来，按 CSS 规范浏览器会认为
  // 「有过渡且值变了」，于是把「默认值→已保存值」真的动画一遍（看起来就是闪一下）。
  // 所以揭开那一帧仍不带过渡，隔两帧待值稳定后才开过渡。
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
    // 开关类设置：先把值全部取回，再在同一个同步块里一次性落值 + 置 ready，让 React
    // 合成一次渲染 —— 不存在「已显示但值还没到」的中间态（那正是开关闪一下的成因）。
    // 每项自带 catch 兜底：Promise.all 是 fail-fast，只要一项 reject 就会在其余项
    // 还没回来时提前放行 ready。也不用 rAF，避免 setReady 与赋值分到不同批次。
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

  // 设备列表到位后算一遍录音会用哪个麦克风：设置页的说明和「测试麦克风」都要跟录音走同一条判断
  useEffect(() => {
    if (mics.length === 0) return
    let cancelled = false
    resolveMicPlan(selectedMic, { webDevices: mics })
      .then(({ plan }) => { if (!cancelled) setMicPlan(plan) })
      .catch(() => { })
    return () => { cancelled = true }
  }, [mics, selectedMic])

  // 先切内存里的语言让界面立刻响应，再落库。失败时把选中项回滚到真实的持久化值，
  // 不留「按钮已高亮但其实没保存」这种假成功。
  const handleLanguageChange = async (next: LanguagePreference) => {
    const previous = languagePreference
    setLanguagePreference(next)
    try {
      await switchLanguage(next)
    } catch {
      setLanguagePreference(previous)
      await switchLanguage(previous).catch(() => { })
    }
    // 托盘菜单（以及 mac 的顶部应用菜单）是原生的，不会跟着网页换语言，要让 Rust 改文字
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

  // 应用内部快捷键互斥：免提和按住说话绑同一个键，两个功能会同时被触发；
  // 预设切换的组合键同理。提交前拦下来，而不是等用户按了才发现行为诡异。
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
        {/* 界面语言排在最前：看不懂界面的人做不了下面任何一件事。
            这张卡自己走 t()，所以切换后它连自己的标题一起变，用户能立刻确认生效了。 */}
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

        {/* macOS：辅助功能 / 麦克风授权。没有它们热键和插字都用不了，所以紧跟在语言后面 */}
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
            {/* ⚠ 这是「按 Esc 能取消」在整个界面上的**唯一**出处 —— 悬浮窗上那行提示已经
                去掉了（见 overlay/Overlay.tsx 的 thinking 分支）。删掉这句，这个能力就没有
                任何地方告诉用户了。
                录音上限不在这里讲 —— 最后一分钟悬浮窗会显示剩余时间、到点自动结束，界面自己会说。
                「录音也不会保留」要写明：取消和「录了但没出字」在用户眼里很容易混。
                「录音或识别处理」是两个阶段，别连写 —— Esc 在两个阶段都能按。 */}
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

        {/* 麦克风排在「偏好设置」前面：它是录音链路的入口，选错设备什么都录不到；
            而下面那三个开关（提示音、静音外放、剪贴板保护）都是可选的锦上添花。 */}
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
