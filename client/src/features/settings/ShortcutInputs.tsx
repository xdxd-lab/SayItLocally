import * as bridge from '@/services/bridge'
import { setShortcutCaptureActive } from '@/services/webviewKeyboardFallback'
import { type ReactNode, useCallback, useEffect, useRef, useState } from 'react'
import { X } from 'lucide-react'
import { t } from '@/i18n'
import {
  displayAccelerator,
  getSingleKeyDisplay,
  keyEventToShortcutCandidate,
  resolveSingleKeyShortcut,
} from './utils'
import {
  canonicalizePTTShortcut,
  displayPTTShortcut,
  getAcceleratorShortcutValidationError,
  getPTTShortcutWarning,
  getPTTShortcutValidationError,
} from '@/lib/shortcutKeys'

/** 提交前校驗：返回錯誤文案則拒絕儲存（用於應用內部快捷鍵互斥等），返回 null 放行。 */
export type ShortcutValidate = (value: string) => Promise<string | null>

/** 衝突提示的存活時間：讀完就該消失。 */
const ERROR_VISIBLE_MS = 6000

/**
 * 一次性的錯誤提示：顯示一段時間後自動消失。
 *
 * 這類提示只說明"剛才那次按鍵為什麼沒被採納"，一旦使用者改動了衝突的另一個鍵（比如把
 * 「按住說話」換成別的），它就成了沒人再管的陳述——掛在頁面上不走，看著像設定一直有錯。
 */
function useTransientMessage() {
  const [message, setMessage] = useState('')
  const timerRef = useRef<number | undefined>(undefined)
  const show = useCallback((next: string) => {
    window.clearTimeout(timerRef.current)
    setMessage(next)
    if (next) timerRef.current = window.setTimeout(() => setMessage(''), ERROR_VISIBLE_MS)
  }, [])
  useEffect(() => () => window.clearTimeout(timerRef.current), [])
  return [message, show] as const
}

let suspendCount = 0
let activeCaptureCancel: (() => void) | null = null

function isShortcutCaptureOwner(cancel: () => void) {
  return activeCaptureCancel === cancel
}

/**
 * 錄製期間掛起本應用自己的全部熱鍵，結束後恢復。
 *
 * 不掛起的話，按到已繫結的鍵（如免持的右 Alt）會先被熱鍵鏈路吃掉：webview 回退監聽
 * 直接 emit toggle-hands-free 彈出懸浮窗開始錄音，組合鍵則被 RegisterHotKey 搶走、
 * 根本送不到 webview——兩種情況都錄不進新設定。
 *
 * `cancel` 必須是穩定引用（useCallback），否則每次渲染都會重掛一遍掛起/恢復。
 */
function useSuspendHotkeys(active: boolean, cancel: () => void) {
  useEffect(() => {
    if (!active) return
    // 全頁面只允許一個錄製 owner。開啟新框時先取消舊框，避免兩個 window 監聽器
    // 同時儲存同一次按鍵，並繞過雙方基於舊設定快照做的互斥校驗。
    if (activeCaptureCancel && activeCaptureCancel !== cancel) {
      activeCaptureCancel()
    }
    activeCaptureCancel = cancel

    // 計數保護 effect 清理的短暫交疊：舊 owner 的 cleanup 不能提前恢復熱鍵。
    suspendCount += 1
    if (suspendCount === 1) {
      setShortcutCaptureActive(true)
      bridge.beginShortcutCapture()
    }
    // 切走到別的程式就取消錄製：錄製態一直掛著的話，熱鍵會一直處於掛起狀態，
    // 使用者回頭只會覺得"熱鍵突然不管用了"。
    window.addEventListener('blur', cancel)
    return () => {
      window.removeEventListener('blur', cancel)
      if (activeCaptureCancel === cancel) activeCaptureCancel = null
      suspendCount -= 1
      if (suspendCount === 0) {
        setShortcutCaptureActive(false)
        bridge.endShortcutCapture()
      }
    }
  }, [active, cancel])
}

export function PTTShortcutInput({
  value,
  onChange,
  label,
  description,
  validate,
}: {
  value: string
  onChange: (value: string) => void
  label: ReactNode
  description: string
  validate?: ShortcutValidate
}) {
  const [recording, setRecording] = useState(false)
  const [tempValue, setTempValue] = useState('')
  const [validateError, showValidateError] = useTransientMessage()
  // 僅在“本次剛繫結中鍵”後提示一次；重進頁面（元件重掛載）不再顯示。
  const [showMiddleHint, setShowMiddleHint] = useState(false)
  const pressedRef = useRef(new Set<string>())
  const peakRef = useRef(new Set<string>())
  const committingRef = useRef(false)

  const resetCapture = useCallback(() => {
    pressedRef.current.clear()
    peakRef.current.clear()
    committingRef.current = false
    setTempValue('')
  }, [])

  const cancelRecording = useCallback(() => {
    setRecording(false)
    resetCapture()
  }, [resetCapture])
  useSuspendHotkeys(recording, cancelRecording)

  const commit = useCallback(async (mapped: string) => {
    const canonical = canonicalizePTTShortcut(mapped)
    const formatError = getPTTShortcutValidationError(canonical)
    if (formatError) {
      showValidateError(formatError)
      return false
    }
    const error = validate ? await validate(canonical) : null
    if (error) {
      showValidateError(error)
      return false
    }
    showValidateError('')
    onChange(canonical)
    return true
  }, [onChange, validate, showValidateError])

  const handleKeyDown = useCallback((event: KeyboardEvent) => {
    if (!isShortcutCaptureOwner(cancelRecording)) return
    event.preventDefault()
    event.stopPropagation()
    if (event.repeat || committingRef.current || pressedRef.current.has(event.code)) return

    pressedRef.current.add(event.code)
    peakRef.current.add(event.code)
    setTempValue(canonicalizePTTShortcut(peakRef.current))
  }, [cancelRecording])

  const handleKeyUp = useCallback((event: KeyboardEvent) => {
    if (!isShortcutCaptureOwner(cancelRecording)) return
    event.preventDefault()
    event.stopPropagation()
    if (!pressedRef.current.delete(event.code) || committingRef.current) return

    // 第一次鬆開時儲存本次同時按下過的峰值；之後的 keyup 不會重複提交。
    committingRef.current = true
    const candidate = canonicalizePTTShortcut(peakRef.current)
    setRecording(false)
    void commit(candidate).finally(resetCapture)
  }, [cancelRecording, commit, resetCapture])

  useEffect(() => {
    if (!recording) return
    window.addEventListener('keydown', handleKeyDown)
    window.addEventListener('keyup', handleKeyUp)
    // 滑鼠側鍵無法靠 webview 事件可靠捕獲（會被當成“後退”導航），改由 Rust 底層滑鼠鉤子
    // 在 OS 層捕獲併吞掉，再透過事件回報要繫結的側鍵（捕獲模式由 useSuspendHotkeys 開啟）。
    const off = bridge.onMouseShortcutCaptured(({ setting }) => {
      if (!isShortcutCaptureOwner(cancelRecording)) return
      if (!setting || committingRef.current) return
      committingRef.current = true
      setRecording(false)
      pressedRef.current.clear()
      peakRef.current.clear()
      setTempValue('')
      // 側鍵：延遲提交（延遲觸發鉤子重配），讓本次物理“鬆開”先被當前鉤子吞掉。
      // 否則重配鉤子的空檔期會把這次“抬起”漏給 webview——後退鍵會導致頁面返回。
      window.setTimeout(() => {
        void commit(setting).then((ok) => setShowMiddleHint(ok && setting === 'MButton'))
          .finally(() => { committingRef.current = false })
      }, 400)
    })
    return () => {
      window.removeEventListener('keydown', handleKeyDown)
      window.removeEventListener('keyup', handleKeyUp)
      off()
    }
  }, [recording, handleKeyDown, handleKeyUp, cancelRecording, commit])

  const displayValue = tempValue || value
  const keys = displayValue ? displayPTTShortcut(displayValue) : [t('shortcut.notSet')]
  const shortcutWarning = !recording ? getPTTShortcutWarning(value) : null

  return (
    <div>
      <div className="flex items-center justify-between gap-4">
        <div className="min-w-0 flex-1">
          <p className="text-sm font-medium">{label}</p>
          <p className="mt-0.5 text-xs leading-relaxed text-muted-foreground">{description}</p>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <button
            onClick={() => {
              showValidateError('')
              if (recording) {
                cancelRecording()
              } else {
                resetCapture()
                setRecording(true)
              }
            }}
            className={`flex items-center justify-center gap-1 whitespace-nowrap rounded-md border px-2 py-1.5 text-sm transition-colors ${recording
              ? 'border-primary bg-primary/5 ring-2 ring-primary/20'
              : 'border-input bg-muted hover:bg-accent'
              }`}
          >
            {recording && !tempValue ? (
              <span className="animate-pulse text-muted-foreground">{t('shortcutInput.pressKeys')}</span>
            ) : (
              keys.map((key, index) => (
                <span key={`${key}-${index}`}>
                  {index > 0 && <span className="mx-0.5 text-muted-foreground">+</span>}
                  <span className={`rounded border bg-card px-1.5 py-0.5 text-xs shadow-sm ${!displayValue ? 'text-muted-foreground' : ''}`}>
                    {key}
                  </span>
                </span>
              ))
            )}
          </button>

          {!recording && value && (
            <button
              onClick={() => onChange('')}
              className="rounded p-1 hover:bg-accent"
              title={t('shortcutInput.clear')}
              aria-label={t('shortcutInput.clear')}
            >
              <X className="h-3.5 w-3.5 text-muted-foreground" />
            </button>
          )}
        </div>
      </div>
      {validateError && (
        <p className="mt-1.5 text-xs text-destructive">{validateError}</p>
      )}
      {shortcutWarning && (
        <p className="mt-1.5 text-xs text-amber-500">{shortcutWarning}</p>
      )}
      {showMiddleHint && value === 'MButton' && (
        <p className="mt-1.5 text-xs text-amber-500">
          {t('shortcutInput.middleMouseWarning')}
        </p>
      )}
    </div>
  )
}

/**
 * 提交一個「免持 / 預設切換」類快捷鍵前要過的全部關卡。返回錯誤文案，null = 可儲存。
 *
 * 抽出來是為了讓**嚮導和設定頁走同一條校驗路徑**。嚮導原先一條都沒走，於是它能存進
 * 設定頁會當場拒掉的值（系統保留組合、已被別的程式佔用的組合）。
 *
 * 只有組合鍵需要 testShortcut：單鍵走我們自己的 OS 鍵盤鉤子，不佔系統熱鍵槽位，
 * 拿去試註冊反而會得到無意義的結論。
 */
export async function checkShortcutBeforeCommit(
  value: string,
  validate?: ShortcutValidate,
): Promise<string | null> {
  const systemError = getAcceleratorShortcutValidationError(value)
  if (systemError) return systemError
  const error = validate ? await validate(value) : null
  if (error) return error
  const isSingleKey = resolveSingleKeyShortcut(value) !== undefined
  if (!isSingleKey && !(await bridge.testShortcut(value))) {
    return t('shortcutInput.conflictOther')
  }
  return null
}

export function ComboShortcutInput({
  value,
  onChange,
  label,
  description,
  comboOnly = false,
  allowMouseShortcut = false,
  allowClear = true,
  validate,
}: {
  value: string
  onChange: (value: string) => void
  label: ReactNode
  description: string
  /** 僅接受"修飾鍵+主鍵"的組合鍵，拒絕單鍵/單修飾鍵（用於預設切換快捷鍵）。 */
  comboOnly?: boolean
  /** 組合鍵為主的設定也可額外允許滑鼠側鍵/中鍵（AI 整理開關）。 */
  allowMouseShortcut?: boolean
  /**
   * 是否給出「清空」按鈕。
   *
   * 嚮導裡傳 false：那一步的目的是讓新使用者拿到一個**能用的**鍵，
   * 一個把免持功能清成"未設定"的按鈕在這裡只會製造問題。設定頁保持可清空。
   */
  allowClear?: boolean
  validate?: ShortcutValidate
}) {
  const [recording, setRecording] = useState(false)
  const [tempValue, setTempValue] = useState('')
  const [conflict, showConflict] = useTransientMessage()
  // 僅在"本次剛繫結中鍵"後提示一次；重進頁面（元件重掛載）不再顯示。
  const [showMiddleHint, setShowMiddleHint] = useState(false)
  // 一次錄製只提交/探測一次：鬆開組合鍵會產生多個 keyup，
  // 若不加守衛會對同一組合鍵重複呼叫 test_shortcut，第二次因“自己剛註冊”而誤報衝突。
  const committingRef = useRef(false)

  const cancelRecording = useCallback(() => { setRecording(false); setTempValue('') }, [])
  useSuspendHotkeys(recording, cancelRecording)

  const handleKeyDown = useCallback((event: KeyboardEvent) => {
    if (!isShortcutCaptureOwner(cancelRecording)) return
    event.preventDefault()
    event.stopPropagation()

    // 單鍵（免持的右 Alt）與組合鍵（Ctrl+D）的判定收斂在 keyEventToShortcutCandidate。
    // null = 這次按鍵還沒湊成東西（單按 Ctrl、或 comboOnly 下的單鍵），
    // 此時**保留上一個候選**：提交發生在 keyup，中途覆蓋成空會把已錄到的組合鍵抹掉。
    const candidate = keyEventToShortcutCandidate(event, { comboOnly })
    if (candidate) setTempValue(candidate)
  }, [cancelRecording, comboOnly])

  const handleKeyUp = useCallback((event: KeyboardEvent) => {
    if (!isShortcutCaptureOwner(cancelRecording)) return
    event.preventDefault()
    event.stopPropagation()
    if (!tempValue || committingRef.current) return

    // comboOnly 下錄到單鍵：不提交也不結束錄製，繼續等一個真正的組合鍵
    if (comboOnly && resolveSingleKeyShortcut(tempValue) !== undefined) return

    // 守衛避免多次 keyup 重複提交（組合鍵鬆手會產生多個 keyup，
    // 第二次會因為"自己剛註冊"而誤報衝突）
    committingRef.current = true
    void (async () => {
      const error = await checkShortcutBeforeCommit(tempValue, validate)
      if (error) {
        showConflict(error)
      } else {
        showConflict('')
        onChange(tempValue)
      }
      setRecording(false)
      setTempValue('')
    })()
  }, [cancelRecording, tempValue, onChange, comboOnly, validate, showConflict])

  useEffect(() => {
    if (!recording) return

    window.addEventListener('keydown', handleKeyDown)
    window.addEventListener('keyup', handleKeyUp)
    // 滑鼠側鍵由 Rust 底層滑鼠鉤子捕獲後回報。少數以組合鍵為主的設定（AI 整理開關）
    // 也可顯式放行滑鼠單鍵。
    // （捕獲模式由 useSuspendHotkeys 開啟）。
    let off: (() => void) | undefined
    if (!comboOnly || allowMouseShortcut) {
      off = bridge.onMouseShortcutCaptured(({ setting }) => {
        if (!isShortcutCaptureOwner(cancelRecording)) return
        if (!setting || committingRef.current) return
        committingRef.current = true
        setRecording(false)
        setTempValue('')
        // 見 PTT 處說明：延遲提交，避免重配鉤子的空檔期把側鍵“抬起”漏給 webview。
        window.setTimeout(() => {
          void (async () => {
            const error = await checkShortcutBeforeCommit(setting, validate)
            if (error) {
              showConflict(error)
              return
            }
            showConflict('')
            setShowMiddleHint(setting === 'MButton')
            onChange(setting)
          })()
        }, 400)
      })
    }
    return () => {
      window.removeEventListener('keydown', handleKeyDown)
      window.removeEventListener('keyup', handleKeyUp)
      off?.()
    }
  }, [recording, handleKeyDown, handleKeyUp, comboOnly, allowMouseShortcut, cancelRecording, onChange, validate, showConflict])

  // 顯示：單鍵用 getSingleKeyDisplay，組合鍵用 displayAccelerator
  const isSingleKey = resolveSingleKeyShortcut(tempValue || value) !== undefined
  const displayValue = tempValue || value || ''
  const keys = !displayValue
    ? [t('shortcut.notSet')]
    : isSingleKey
      ? [getSingleKeyDisplay(displayValue)]
      : displayAccelerator(displayValue)

  return (
    <div>
      <div className="flex items-center justify-between gap-4">
        <div className="min-w-0 flex-1">
          <p className="text-sm font-medium">{label}</p>
          <p className="mt-0.5 text-xs leading-relaxed text-muted-foreground">{description}</p>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <button
            onClick={() => {
              committingRef.current = false
              setRecording(!recording)
              setTempValue('')
              showConflict('')
            }}
            className={`flex items-center justify-center gap-1 whitespace-nowrap rounded-md border px-2 py-1.5 text-sm transition-colors ${recording
              ? 'border-primary bg-primary/5 ring-2 ring-primary/20'
              : 'border-input bg-muted hover:bg-accent'
              }`}
          >
            {recording && !tempValue ? (
              <span className="animate-pulse text-muted-foreground">{t('shortcutInput.pressKeys')}</span>
            ) : (
              keys.map((key, index) => (
                <span key={index}>
                  {index > 0 && <span className="mx-0.5 text-muted-foreground">+</span>}
                  <span className="rounded border bg-card px-1.5 py-0.5 text-xs shadow-sm">{key}</span>
                </span>
              ))
            )}
          </button>

          {allowClear && !recording && (
            <button
              onClick={() => onChange('')}
              className="rounded p-1 hover:bg-accent"
              aria-label={t('shortcutInput.clear')}
            >
              <X className="h-3.5 w-3.5 text-muted-foreground" />
            </button>
          )}
        </div>
      </div>
      {conflict && (
        <p className="mt-1.5 text-xs text-destructive">{conflict}</p>
      )}
      {showMiddleHint && value === 'MButton' && (
        <p className="mt-1.5 text-xs text-amber-500">{t('shortcutInput.middleMouseWarning')}</p>
      )}
    </div>
  )
}
