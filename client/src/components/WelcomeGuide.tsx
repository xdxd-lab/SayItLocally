import { useEffect, useRef, useState } from 'react'
import { Mic, Sparkles, Globe, ArrowRight, ArrowLeft, CheckCircle2, AlertCircle, Keyboard } from 'lucide-react'
import { Card, CardContent } from '@/components/ui/card'
import { getSetting, setSetting } from '@/services/store'
import { getWorkMode } from '@/services/transcription'
import { healthCheck } from '@/services/api'
import { setPttSuppressed } from '@/services/recorder'
import * as bridge from '@/services/bridge'
import {
  displayShortcut,
  getSingleKeyDisplay,
  isSingleKeySetting,
  keyEventToShortcutCandidate,
  pttShortcutConflictsWithAccelerator,
} from '@/lib/shortcutKeys'
import {
  ComboShortcutInput,
  checkShortcutBeforeCommit,
} from '@/features/settings/ShortcutInputs'
import { refreshPTTSetting, setShortcutCaptureActive } from '@/services/webviewKeyboardFallback'
import appIcon from '@/assets/icon-128.png'
import { t, type TranslationKey } from '@/i18n'
import { useT } from '@/i18n/useT'
import { IS_MAC } from '@/lib/platform'
import { getDefault } from '@/services/defaults'
import { MacPermissionsPanel } from '@/components/MacPermissionsPanel'

/** 擴音的預設鍵（Windows 右 Alt，mac 右 ⌘） */
const DEFAULT_HANDS_FREE: string = getDefault<string>('shortcutHandsFree') || 'AltRight'

/**
 * 擴音鍵與「按住說話」撞在同一個鍵上時，兩個功能會被同一次按鍵同時觸發。
 *
 * 模組級函式而不是 useCallback：它進 effect 的依賴陣列，引用一變就會把整個錄製
 * effect（連帶 begin/endShortcutCapture）重啟一遍。PTT 設定在這裡現讀現用，
 * 不需要元件 state。
 */
async function validateHandsFreeAgainstPTT(value: string): Promise<string | null> {
  if (!value) return null
  const ptt = await getSetting('shortcutPTT', 'ControlRight') as string
  return pttShortcutConflictsWithAccelerator(ptt, value)
    ? t('settings.shortcuts.conflictPtt')
    : null
}

/** 儲存擴音鍵並讓原生側立刻用上：三步缺一不可，所以只留這一個出口。 */
async function persistHandsFreeShortcut(value: string): Promise<void> {
  await setSetting('shortcutHandsFree', value)
  bridge.notifyShortcutsChanged()
  // 同步重新整理 webview 回退快取，否則嚮導內（SayItLocally 視窗聚焦）測試時新鍵不生效
  await refreshPTTSetting()
}

/** 簡化鍵盤佈局，高亮當前快捷鍵（可以是一組 —— 組合鍵會同時按住幾個） */
function KeyboardHint({ activeKeys, pressed }: { activeKeys: string[]; pressed?: boolean }) {
  const rows: { code: string; label: string; w?: number }[][] = [
    [
      { code: 'ShiftLeft', label: 'Shift', w: 52 },
      { code: 'KeyZ', label: 'Z' }, { code: 'KeyX', label: 'X' },
      { code: 'KeyC', label: 'C' }, { code: 'KeyV', label: 'V' },
      { code: 'KeyB', label: 'B' }, { code: 'KeyN', label: 'N' },
      { code: 'KeyM', label: 'M' }, { code: 'Comma', label: '<' },
      { code: 'Period', label: '>' }, { code: 'Slash', label: '?' },
      { code: 'ShiftRight', label: 'Shift', w: 52 },
    ],
    IS_MAC
      ? [
        // MacBook 的最下面一行：fn ⌃ ⌥ ⌘ 空格 ⌘ ⌥（右側沒有 ⌃）
        { code: 'Fn', label: 'fn', w: 30 },
        { code: 'ControlLeft', label: '⌃', w: 30 },
        { code: 'AltLeft', label: '⌥', w: 30 },
        { code: 'MetaLeft', label: '⌘', w: 38 },
        { code: 'Space', label: t('keyName.Space'), w: 140 },
        { code: 'MetaRight', label: '⌘', w: 38 },
        { code: 'AltRight', label: '⌥', w: 30 },
      ]
      : [
        { code: 'ControlLeft', label: 'Ctrl', w: 42 },
        { code: 'MetaLeft', label: 'Win', w: 34 },
        { code: 'AltLeft', label: 'Alt', w: 34 },
        { code: 'Space', label: t('keyName.Space'), w: 148 },
        { code: 'AltRight', label: 'Alt', w: 34 },
        { code: 'ControlRight', label: 'Ctrl', w: 42 },
      ],
  ]

  return (
    <div className="flex flex-col items-center gap-1">
      {rows.map((row, ri) => (
        <div key={ri} className="flex items-center justify-center gap-1">
          {row.map((key) => {
            const isActive = activeKeys.includes(key.code)
            const isPressed = isActive && pressed
            return (
              <div
                key={key.code}
                className={`flex items-center justify-center rounded-md transition-all ${isPressed
                  ? 'bg-foreground text-background font-bold shadow-lg ring-2 ring-foreground/30 scale-110'
                  : isActive
                    ? 'bg-primary text-primary-foreground font-semibold shadow-sm ring-2 ring-primary/30'
                    : 'border border-border/60 bg-muted/30 text-muted-foreground'
                  }`}
                style={{
                  width: isActive ? (key.w || 30) + 4 : key.w || 30,
                  height: isActive ? 32 : 28,
                  fontSize: isActive ? 12 : 10,
                }}
              >
                {key.label}
              </div>
            )
          })}
        </div>
      ))}
    </div>
  )
}

const MODE_LABEL_KEYS: Record<string, TranslationKey> = {
  server: 'mode.server',
  cloud_api: 'mode.cloudApi',
  local: 'mode.local',
}

/**
 * 把帶 `{key}` 佔位的句子渲染成「文字 + 加粗鍵名 + 文字」。
 * 按佔位符切分，鍵名在句中的位置就由譯文決定（中英語序不同）。
 */
function HotkeyPrompt({ template, keyLabel }: { template: string; keyLabel: string }) {
  const [before, after = ''] = template.split('{key}')
  return (
    <>
      {before}
      <span className="font-medium text-foreground">{keyLabel}</span>
      {after}
    </>
  )
}

/** 同 HotkeyPrompt，只是鍵名用鍵帽樣式。 */
function ReadyHint({ template, keyLabel }: { template: string; keyLabel: string }) {
  const [before, after = ''] = template.split('{key}')
  return (
    <>
      {before}
      <span className="rounded border border-border bg-muted/50 px-1.5 py-0.5 font-medium text-foreground">{keyLabel}</span>
      {after}
    </>
  )
}

interface WelcomeGuideProps {
  onComplete: () => void
}

/** 嚮導步驟。mac 多一步「許可權」：沒有輔助功能授權，熱鍵和插字都用不了。 */
const STEP_IDS = ['welcome', 'features', ...(IS_MAC ? ['permissions'] : []), 'hotkey', 'try', 'done'] as const
type StepId = (typeof STEP_IDS)[number]

export default function WelcomeGuide({ onComplete }: WelcomeGuideProps) {
  const t = useT()
  const [step, setStep] = useState(0)
  const stepId: StepId = STEP_IDS[step] ?? 'welcome'
  const [hfKey, setHfKey] = useState(DEFAULT_HANDS_FREE)
  const hfLabel = displayShortcut(hfKey).join(' + ')
  const [workMode, setWorkMode] = useState('')
  const [serverOk, setServerOk] = useState<boolean | null>(null)
  const [testText, setTestText] = useState('')
  // 熱鍵確認步驟的狀態
  const [keyConfirmed, setKeyConfirmed] = useState(false)
  /** 手指還按著的那個候選值（未提交）。空串 = 沒在按 */
  const [capturing, setCapturing] = useState('')
  /** 當前物理按住的 DOM code。組合鍵會同時有多個，鍵盤圖靠它高亮 */
  const [pressedCodes, setPressedCodes] = useState<string[]>([])
  /** 這次按鍵為什麼沒被採納（系統保留組合、被別的程式佔用、和 PTT 撞） */
  const [captureError, setCaptureError] = useState('')
  const hfKeyRef = useRef(DEFAULT_HANDS_FREE)
  const settingsDirtyRef = useRef(false)

  /** 手指還按著時大號鍵帽跟著候選走，鬆開後回到已確認的那個 */
  const shownKey = capturing || hfKey
  const shownLabel = displayShortcut(shownKey).join(' + ')
  const isPressing = pressedCodes.length > 0
  /**
   * 鍵盤示意圖只在「單鍵」時留著。
   *
   * 它的用途是幫新手找到"右 Alt 在哪"，而圖上只有 Shift 那行和最下面一行 ——
   * 組合鍵的主鍵（D）本來就不在圖上，把圖補成整塊鍵盤會把這一步的卡片撐爆。
   * 已經會按組合鍵的使用者也不需要這張圖。按下過程中保持顯示，避免中途閃一下。
   */
  const showKeyboardHint = isPressing || isSingleKeySetting(shownKey)
  const hintActiveKeys = isPressing ? pressedCodes : [shownKey]

  useEffect(() => {
    getSetting('shortcutHandsFree', DEFAULT_HANDS_FREE).then((k) => {
      const key = k as string
      setHfKey(key)
      hfKeyRef.current = key
    })
    const mode = getWorkMode()
    setWorkMode(mode)
    if (mode === 'server') {
      healthCheck().then(() => setServerOk(true)).catch(() => setServerOk(false))
    }
  }, [])

  // 熱鍵確認步驟：錄製擴音熱鍵。
  //
  // **單鍵和組合鍵（Ctrl+D）都要能錄。** 判定與提交校驗都複用設定頁那兩個函式，
  // 口徑必須一致 —— 這裡以前自己寫了一份只認單鍵白名單的版本，於是按 Ctrl+D 時
  // Ctrl 的 keydown 先到、`ControlLeft` 恰好在白名單裡，擴音鍵被靜默存成「左 Ctrl」，
  // D 根本沒輪到，而使用者看到的現象是"組合鍵不支援"。
  //
  // 只依賴 step，避免 hfKey 等 state 變化導致 effect 反覆執行、鉤子反覆重啟。
  useEffect(() => {
    if (stepId !== 'hotkey') return
    setPttSuppressed(true)
    // 掛起本應用自己的全部熱鍵（同設定頁錄製）：不掛起的話，按一個已經註冊過的組合鍵
    // 會被 RegisterHotKey 搶走、webview 收不到 keydown，等於永遠錄不進來。
    // 副作用是這一步裡熱鍵都不響應 —— 正好，這一步只確認鍵位，不該開始錄音。
    setShortcutCaptureActive(true)
    void bridge.beginShortcutCapture()

    /** 當前按住的物理 code：既給鍵盤圖高亮用，也用來判斷某條事件路徑是否該讓位 */
    const pressed = new Set<string>()
    /** 本次按鍵週期的狀態。用普通物件而非 state：讀寫要同步，且不能觸發 effect 重啟 */
    const session = { candidate: '', committing: false }

    const syncPressed = () => setPressedCodes([...pressed])

    const commit = (value: string) => {
      if (session.committing) return
      session.committing = true
      void (async () => {
        const error = await checkShortcutBeforeCommit(value, validateHandsFreeAgainstPTT)
        if (error) {
          setCaptureError(error)
        } else {
          // 存進 ref：state 更新不會重啟 effect（state 不在依賴裡），ref 才是 cleanup 落盤時的真相
          hfKeyRef.current = value
          settingsDirtyRef.current = true
          setHfKey(value)
          setKeyConfirmed(true)
          setCaptureError('')
        }
        session.candidate = ''
        session.committing = false
        setCapturing('')
      })()
    }

    const isModifierKey = (event: KeyboardEvent) =>
      event.key === 'Control' || event.key === 'Alt'
      || event.key === 'Shift' || event.key === 'Meta'

    // 路徑 1：webview 收到的按鍵。捕獲模式下原生鉤子會放行已繫結的單鍵，
    // 所以右 Alt 這類平時被吞掉的鍵在這一步也走這條路。
    const onDown = (event: KeyboardEvent) => {
      if (event.repeat) return
      const candidate = keyEventToShortcutCandidate(event)
      // **錄不成快捷鍵的鍵一律放行，不要 preventDefault。**
      // 裸 Tab、裸 Enter 都不是合法的擴音鍵（candidate 為 null），攔下來只會把
      // 鍵盤使用者困在這一步：Tab 移不了焦點，Enter 點不了「下一步」。
      // 修飾鍵自己也返回 null，但它是組合鍵的一半，要照常吞掉並記賬。
      if (!candidate && !isModifierKey(event)) return
      event.preventDefault()
      pressed.add(event.code)
      syncPressed()
      // candidate 為 null（單按 Ctrl）時保留上一個候選，別覆蓋成空 ——
      // 否則 Ctrl+D 裡任何一個不成型的中間事件都會把已經錄到的組合鍵抹掉。
      if (candidate) {
        session.candidate = candidate
        setCapturing(candidate)
        setCaptureError('')
      }
    }
    const onUp = (event: KeyboardEvent) => {
      // delete 返回 false = 這個鍵的 down 被放行過（如 Tab），它的 up 也不該攔
      if (!pressed.delete(event.code)) return
      event.preventDefault()
      syncPressed()
      // 第一次鬆開就提交本次按住過的候選；session.committing 擋掉後續 keyup
      if (session.candidate) commit(session.candidate)
    }

    // 路徑 2：兜底。萬一某個鍵仍被原生鉤子吞掉（只 emit 事件、webview 收不到 keydown），
    // 就按老辦法確認一次，讓這一步不至於卡死。webview 已經在處理時讓位，避免重複提交。
    const unlistenHf = bridge.listen('toggle-hands-free', () => {
      if (pressed.size === 0) commit(hfKeyRef.current || DEFAULT_HANDS_FREE)
    })
    const unlistenPttDown = bridge.listen('ptt-down', (event: unknown) => {
      const payload = event as { payload?: { pttSetting?: string } }
      const setting = payload?.payload?.pttSetting
      if (setting && pressed.size === 0) commit(setting)
    })

    // 路徑 3：滑鼠側鍵/中鍵。webview 收不到（會被當成前進/後退導航），由 Rust 底層
    // 滑鼠鉤子在 OS 層吞掉後回報。延遲提交見 ShortcutInputs 裡的同一處說明；
    // 原生側捕到一次就自動退出捕獲模式，所以提交完要重新開啟，否則後面就錄不動了。
    const offMouse = bridge.onMouseShortcutCaptured(({ setting }) => {
      if (!setting) return
      window.setTimeout(() => {
        commit(setting)
        void bridge.beginShortcutCapture()
      }, 400)
    })

    window.addEventListener('keydown', onDown)
    window.addEventListener('keyup', onUp)
    return () => {
      window.removeEventListener('keydown', onDown)
      window.removeEventListener('keyup', onUp)
      setPttSuppressed(false)
      setShortcutCaptureActive(false)
      setCapturing('')
      setPressedCodes([])
      unlistenHf.then((fn) => fn())
      unlistenPttDown.then((fn) => fn())
      offMouse()

      // 立刻退出捕獲模式，**不等落盤**。
      //
      // 順序反過來（先 await 落盤）會留一個競態：下一步那個錄製控制元件自己會
      // beginShortcutCapture，而這邊遲到的 endShortcutCapture 會把它剛開的捕獲關掉，
      // 於是使用者在「試一試」裡按組合鍵錄不進去。
      // 兩個命令都是「從儲存讀當前值重新註冊一遍」，誰後執行都不會註冊到錯的鍵上：
      // notifyShortcutsChanged 一定排在 setSetting 之後發出。
      //
      // **退出必須無條件執行** —— 漏掉的話使用者走完嚮導會發現所有熱鍵都不響應了。
      void bridge.endShortcutCapture()
      if (settingsDirtyRef.current) {
        settingsDirtyRef.current = false
        void persistHandsFreeShortcut(hfKeyRef.current)
      }
    }
  }, [stepId])

  const canTest = workMode === 'server' && serverOk === true
  const totalSteps = STEP_IDS.length
  const isLast = step === totalSteps - 1

  const renderStep = () => {
    switch (stepId) {
      // Step 0: 歡迎
      case 'welcome':
        return (
          <div className="flex flex-col items-center text-center">
            <img src={appIcon} alt="SayItLocally" className="mb-6 h-20 w-20 rounded-2xl" />
            <h1 className="text-3xl font-bold tracking-tight" style={{ fontFamily: "'Nunito', sans-serif", fontWeight: 800 }}>{t('welcome.title')}</h1>
            <p className="mt-3 text-base text-muted-foreground">{t('welcome.tagline')}</p>
            <p className="mt-1.5 text-sm text-muted-foreground/70">{t('welcome.subtitle')}</p>
          </div>
        )

      // Step 1: 核心功能
      case 'features':
        return (
          <div>
            <h2 className="mb-5 text-center text-xl font-bold">{t('welcome.featuresTitle')}</h2>
            <div className="space-y-3">
              {[
                { icon: Mic, title: t('welcome.feature.handsFree.title'), desc: t('welcome.feature.handsFree.desc'), color: 'text-blue-500 bg-blue-500/10' },
                { icon: Sparkles, title: t('welcome.feature.ai.title'), desc: t('welcome.feature.ai.desc'), color: 'text-amber-500 bg-amber-500/10' },
                { icon: Globe, title: t('welcome.feature.deploy.title'), desc: t('welcome.feature.deploy.desc'), color: 'text-emerald-500 bg-emerald-500/10' },
              ].map(({ icon: Icon, title, desc, color }) => (
                <Card key={title}>
                  <CardContent className="flex items-center gap-4 p-4">
                    <div className={`flex h-10 w-10 shrink-0 items-center justify-center rounded-xl ${color}`}>
                      <Icon className="h-5 w-5" />
                    </div>
                    <div>
                      <p className="text-sm font-medium">{title}</p>
                      <p className="mt-0.5 text-xs text-muted-foreground">{desc}</p>
                    </div>
                  </CardContent>
                </Card>
              ))}
            </div>
          </div>
        )

      // macOS：輔助功能 + 麥克風授權（Windows 沒有這一步）
      case 'permissions':
        return (
          <div>
            <h2 className="mb-2 text-center text-xl font-bold">{t('welcome.permissionsTitle')}</h2>
            <p className="mb-5 text-center text-sm text-muted-foreground">{t('welcome.permissionsDesc')}</p>
            <MacPermissionsPanel />
          </div>
        )

      // Step 2: 熱鍵確認
      case 'hotkey':
        return (
          <div className="flex flex-col items-center text-center">
            <div className="mb-5 flex h-14 w-14 items-center justify-center rounded-2xl bg-primary/10">
              <Keyboard className="h-7 w-7 text-primary" />
            </div>
            <h2 className="text-xl font-bold">{t('welcome.hotkeyTitle')}</h2>
            <p className="mt-2 text-sm text-muted-foreground">
              {keyConfirmed
                ? t('welcome.hotkeyConfirmed')
                : <HotkeyPrompt template={t('welcome.hotkeyPrompt')} keyLabel={getSingleKeyDisplay(DEFAULT_HANDS_FREE)} />}
            </p>

            {/* 大號按鍵展示 */}
            <div className="my-6">
              <div
                className={`inline-flex items-center justify-center rounded-xl border-2 px-8 py-4 text-lg font-bold transition-all ${isPressing
                  ? 'border-foreground bg-foreground text-background scale-105 shadow-xl'
                  : keyConfirmed
                    ? 'border-emerald-500 bg-emerald-500/10 text-emerald-600'
                    : 'border-primary/40 bg-primary/5 text-primary animate-pulse'
                  }`}
              >
                {isPressing ? `${shownLabel} ⬇` : keyConfirmed ? `✓ ${shownLabel}` : shownLabel}
              </div>
            </div>

            {/* 鍵盤位置示意 */}
            {showKeyboardHint && (
              <div className="w-full rounded-xl border bg-card p-3">
                <p className="mb-2 text-[11px] text-muted-foreground">{t('welcome.keyPosition')}</p>
                <KeyboardHint activeKeys={hintActiveKeys} pressed={isPressing} />
              </div>
            )}

            {/* 按鍵沒被採納時必須說清為什麼：不然使用者只看到"按了沒變化" */}
            {captureError && (
              <p role="alert" className="mt-4 text-xs text-destructive">{captureError}</p>
            )}
            {!keyConfirmed && !captureError && (
              <p className="mt-4 text-xs text-muted-foreground/60">
                {t(IS_MAC ? 'welcome.supportedKeysMac' : 'welcome.supportedKeys')}
              </p>
            )}
            {keyConfirmed && !captureError && (
              <p className="mt-4 text-xs text-muted-foreground">
                {t('welcome.changeHint')}
              </p>
            )}
          </div>
        )

      // Step 3: 試一試
      case 'try':
        return (
          <div>
            <h2 className="mb-5 text-center text-xl font-bold">{t('welcome.tryTitle')}</h2>

            {/* 用設定頁那個錄製控制元件，不再自己寫一份：組合鍵、系統保留組合校驗、
                「已被別的程式佔用」探測全都在裡面。allowClear=false —— 嚮導的目的是
                讓使用者拿到一個能用的鍵，一個把它清成"未設定"的按鈕在這裡只會製造問題。 */}
            <Card className="mb-4">
              <CardContent className="p-4">
                <ComboShortcutInput
                  value={hfKey}
                  onChange={(value) => {
                    setHfKey(value)
                    hfKeyRef.current = value
                    void persistHandsFreeShortcut(value)
                  }}
                  label={t('welcome.handsFreeShortcut')}
                  description={t('welcome.clickToChange')}
                  allowClear={false}
                  validate={validateHandsFreeAgainstPTT}
                />
              </CardContent>
            </Card>

            <Card className="mb-4">
              <CardContent className="flex items-center justify-between p-4">
                <div>
                  <p className="text-sm font-medium">{t('welcome.currentMode')}</p>
                  <p className="mt-0.5 text-xs text-muted-foreground">{MODE_LABEL_KEYS[workMode] ? t(MODE_LABEL_KEYS[workMode]) : workMode}</p>
                </div>
                {workMode === 'server' && (
                  <div className="flex items-center gap-1.5">
                    {serverOk === null ? (
                      <span className="text-xs text-muted-foreground">{t('common.checking')}</span>
                    ) : serverOk ? (
                      <><CheckCircle2 className="h-4 w-4 text-emerald-500" /><span className="text-xs text-emerald-600">{t('welcome.connected')}</span></>
                    ) : (
                      <><AlertCircle className="h-4 w-4 text-destructive" /><span className="text-xs text-destructive">{t('welcome.notConnected')}</span></>
                    )}
                  </div>
                )}
                {workMode !== 'server' && (
                  <span className="text-xs text-muted-foreground">{t('welcome.needsSetup')}</span>
                )}
              </CardContent>
            </Card>

            <Card>
              <CardContent className="p-4">
                <p className="mb-2 text-sm font-medium">{canTest ? t('welcome.voiceTestReady') : t('welcome.voiceTest')}</p>
                {canTest && (
                  <p className="mb-2 text-xs text-muted-foreground">{t('welcome.testHint', { key: hfLabel })}</p>
                )}
                <textarea
                  value={testText}
                  onChange={(e) => setTestText(e.target.value)}
                  placeholder={canTest
                    ? t('welcome.testPlaceholder', { key: hfLabel })
                    : t('welcome.testUnavailable')}
                  className="w-full resize-none rounded-lg border border-input-border bg-input-bg p-3 text-sm leading-relaxed placeholder:text-muted-foreground/40 focus:border-input-focus-border focus:outline-none"
                  rows={4}
                  readOnly={!canTest}
                />
                {!canTest && (
                  <p className="mt-2 text-xs text-muted-foreground">
                    {t('welcome.testUnavailableHint', { key: hfLabel })}
                  </p>
                )}
              </CardContent>
            </Card>
          </div>
        )

      // Step 4: 完成
      case 'done':
        return (
          <div className="flex flex-col items-center text-center">
            <div className="mb-6 flex h-16 w-16 items-center justify-center rounded-full bg-emerald-500/10">
              <CheckCircle2 className="h-8 w-8 text-emerald-500" />
            </div>
            <h2 className="text-xl font-bold">{t('welcome.readyTitle')}</h2>
            <p className="mt-3 text-sm text-muted-foreground">
              <ReadyHint template={t('welcome.readyHint')} keyLabel={hfLabel} />
            </p>
            <p className="mt-1.5 text-xs text-muted-foreground/70">{t('welcome.readySubHint')}</p>
          </div>
        )

      default:
        return null
    }
  }

  return (
    <div className="fixed inset-0 z-[100] flex items-center justify-center bg-background/95 backdrop-blur-sm">
      <div className="custom-scrollbar w-full max-w-lg max-h-[90vh] overflow-y-auto px-6 py-8">
        {renderStep()}

        {/* 底部導航 */}
        <div className="mt-8 flex items-center justify-between">
          <div className="flex gap-1.5">
            {Array.from({ length: totalSteps }, (_, i) => (
              <div
                key={i}
                className={`h-1.5 rounded-full transition-all ${i === step ? 'w-6 bg-primary' : 'w-1.5 bg-muted-foreground/20'
                  }`}
              />
            ))}
          </div>
          <div className="flex gap-2">
            {step > 0 && (
              <button
                onClick={() => setStep(step - 1)}
                className="flex items-center gap-1 rounded-lg px-4 py-2 text-sm text-muted-foreground transition-colors hover:text-foreground"
              >
                <ArrowLeft className="h-3.5 w-3.5" />
                {t('common.back')}
              </button>
            )}
            {!isLast && step === 0 && (
              <button
                onClick={onComplete}
                className="rounded-lg px-4 py-2 text-sm text-muted-foreground transition-colors hover:text-foreground"
              >
                {t('common.skip')}
              </button>
            )}
            <button
              onClick={() => isLast ? onComplete() : setStep(step + 1)}
              className="flex items-center gap-1.5 rounded-lg bg-primary px-5 py-2 text-sm font-medium text-primary-foreground transition-colors hover:bg-primary/90"
            >
              {isLast ? t('welcome.start') : t('common.next')}
              <ArrowRight className="h-3.5 w-3.5" />
            </button>
          </div>
        </div>
      </div>
    </div>
  )
}
