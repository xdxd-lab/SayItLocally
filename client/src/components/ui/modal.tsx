// 彈窗外殼：Esc 關閉、焦點陷阱、關閉後還原焦點、role="dialog"
//
// 為什麼要有這個：設定頁裡有三個手搓的彈窗（離線下載指引 / 確認刪除模型 / 更改模型目錄），
// 每個都是 `fixed inset-0 + <div onClick>` 背板，沒有 role、沒有 aria-modal、按 Esc 沒反應、
// 開啟後焦點還留在背後的頁面上、Tab 能跑到彈窗外面去。確認刪除框更極端：唯一的關閉方式
// 是 Tab 到「取消」（背板點選要用滑鼠）。語義只在這裡修一次。

import { useCallback, useEffect, useLayoutEffect, useRef, type ReactNode, type RefObject } from 'react'
import { createPortal } from 'react-dom'
import { X } from 'lucide-react'
import { useT } from '@/i18n/useT'
import { useBackdropDismiss } from '@/hooks/useBackdropDismiss'
import { cn } from '@/lib/utils'

const FOCUSABLE = 'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])'

const EXIT_MS = 140

/**
 * 關閉時淡出。
 *
 * 難處在於彈窗是**呼叫方**解除安裝的（`{open && <Modal …/>}`），而且關閉的入口五花八門：
 * Esc、背板、右上角 ×，還有 children 裡各自的「取消 / 確定」直接改父元件的 state。
 * 想讓每個入口都先播完動畫再解除安裝，就得改十幾處呼叫方。
 *
 * 所以換個做法：解除安裝那一刻把整層 DOM 複製一份（真身照常被 React 移除，焦點、事件都跟著走），
 * 讓這份不可互動的複製品淡出後自己刪掉。React 解除安裝時先跑元件的 layout effect 清理、
 * 再移除宿主節點，所以清理函式裡拿到的 DOM 還是完整的。
 *
 * 複製品掛上去之前等一個 microtask：開發模式的 StrictMode 會在掛載後立刻模擬一次
 * 解除安裝再掛載，那次不是真關閉；等到 microtask 時元件已經重新掛上了，就不留複製品。
 */
function useExitGhost(rootRef: RefObject<HTMLDivElement>) {
  const mountedRef = useRef(false)
  useLayoutEffect(() => {
    mountedRef.current = true
    const root = rootRef.current
    return () => {
      mountedRef.current = false
      if (!root || !root.isConnected) return
      if (window.matchMedia?.('(prefers-reduced-motion: reduce)').matches) return
      const ghost = root.cloneNode(true) as HTMLDivElement
      ghost.removeAttribute('id')
      ghost.setAttribute('aria-hidden', 'true')
      ghost.setAttribute('inert', '')
      ghost.style.pointerEvents = 'none'
      // 複製品裡不能還帶著入場類，否則它會先再入場一次。
      ghost.classList.remove('modal-backdrop-enter')
      ghost.classList.add('modal-backdrop-exit')
      const panel = ghost.querySelector('.modal-panel-enter')
      panel?.classList.remove('modal-panel-enter')
      panel?.classList.add('modal-panel-exit')
      // 複製品不是對話方塊：去掉語義，免得讀屏或按 role 找彈窗的程式碼把它當成還開著的那個。
      panel?.removeAttribute('role')
      panel?.removeAttribute('aria-modal')
      panel?.removeAttribute('aria-labelledby')
      ghost.querySelectorAll('[id]').forEach((el) => el.removeAttribute('id'))
      // 滾動位置不隨 cloneNode 複製；長彈窗滾到底再關，複製品會先跳回頂部。
      // 必須現在讀：等 microtask 時真身已經被移除，detached 元素的 scrollTop 是 0。
      const scrollTop = root.querySelector('.modal-panel-enter')?.scrollTop ?? 0
      queueMicrotask(() => {
        if (mountedRef.current) return
        document.body.appendChild(ghost)
        if (panel) panel.scrollTop = scrollTop
        window.setTimeout(() => ghost.remove(), EXIT_MS + 40)
      })
    }
  }, [rootRef])
}

interface ModalProps {
  /** 彈窗標題，同時作為無障礙名稱 */
  title: string
  onClose: () => void
  /** 關閉中（如遷移檔案時）禁掉 Esc 與背板點選，避免中斷危險操作 */
  locked?: boolean
  /** 是否顯示右上角關閉按鈕。確認類彈窗用底部按鈕，不需要 */
  showCloseButton?: boolean
  /** 面板寬度類，如 'w-[520px]'。會自動補 max-w-[calc(100vw-2rem)] 防小窗溢位 */
  panelClassName?: string
  children: ReactNode
}

export function Modal({
  title,
  onClose,
  locked,
  showCloseButton,
  panelClassName,
  children,
}: ModalProps) {
  const t = useT()
  const panelRef = useRef<HTMLDivElement>(null)
  const titleId = useRef(`modal-title-${Math.random().toString(36).slice(2, 8)}`).current

  const requestClose = useCallback(() => {
    if (!locked) onClose()
  }, [locked, onClose])

  // 點背板關閉。判據在 hook 裡，別改回「背板直接掛 onClick」—— 那樣拖選一齣面板就誤關。
  const backdropDismiss = useBackdropDismiss(requestClose)

  const rootRef = useRef<HTMLDivElement>(null)
  useExitGhost(rootRef)

  /**
   * 開啟時把焦點移進彈窗（否則讀屏與鍵盤使用者還停在背後的頁面上），關閉時還回去。
   *
   * 必須是空依賴、只跑一次：這段原來和下面的按鍵監聽寫在同一個 effect 裡，依賴 requestClose，
   * 而 requestClose 跟著呼叫方傳進來的 onClose 變。呼叫方的 onClose 只要不是 useCallback
   * 包過的（正常寫法就不是），每次重渲染都是新函式 → effect 重跑 → 焦點被搶回第一個控制元件。
   * 表現就是"在輸入框裡每打一個字，游標就跳回上面的下拉框"。
   *
   * 想指定落焦點的控制元件，在它（或它的容器）上加 data-modal-autofocus。
   */
  useEffect(() => {
    const previouslyFocused = document.activeElement as HTMLElement | null
    const panel = panelRef.current
    const preferred = panel?.querySelector<HTMLElement>('[data-modal-autofocus]')
    const target = preferred?.matches(FOCUSABLE)
      ? preferred
      : preferred?.querySelector<HTMLElement>(FOCUSABLE)
      ?? panel?.querySelector<HTMLElement>(FOCUSABLE)
      ; (target ?? panel)?.focus()

    return () => { previouslyFocused?.focus?.() }
  }, [])

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.stopPropagation()
        requestClose()
        return
      }
      if (event.key !== 'Tab') return

      // 焦點陷阱：Tab 到頭尾時在彈窗內繞回，不跑到背後的頁面上
      const focusables = Array.from(panelRef.current?.querySelectorAll<HTMLElement>(FOCUSABLE) ?? [])
      if (focusables.length === 0) return
      const firstEl = focusables[0]
      const lastEl = focusables[focusables.length - 1]
      const active = document.activeElement
      if (event.shiftKey && (active === firstEl || active === panelRef.current)) {
        event.preventDefault()
        lastEl.focus()
      } else if (!event.shiftKey && active === lastEl) {
        event.preventDefault()
        firstEl.focus()
      }
    }

    document.addEventListener('keydown', onKeyDown, true)
    return () => document.removeEventListener('keydown', onKeyDown, true)
  }, [requestClose])

  return createPortal(
    <div
      // 遮罩用固定的黑色而非 --foreground：深色主題的 foreground 是接近白的淺灰，
      // 拿它當遮罩會把背景照亮。遮罩在任何主題下都必須是暗的。
      ref={rootRef}
      className="modal-backdrop-enter fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4"
      {...backdropDismiss}
    >
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
        // 冗餘的第二道：真正的判據在 useBackdropDismiss（看那裡的註釋）。
        // 單靠這一條擋不住拖選出面板的情況 —— 那時 click 壓根不走面板這條路徑。
        onClick={(e) => e.stopPropagation()}
        className={cn(
          'modal-panel-enter relative max-h-[calc(100vh-2rem)] max-w-[calc(100vw-2rem)] overflow-y-auto rounded-xl border bg-card p-6 shadow-xl',
          panelClassName,
        )}
      >
        {showCloseButton && (
          <button
            type="button"
            onClick={requestClose}
            aria-label={t('window.close')}
            className="absolute right-3 top-3 rounded-md p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          >
            <X className="h-4 w-4" aria-hidden />
          </button>
        )}
        <h3 id={titleId} className="text-base font-semibold">{title}</h3>
        {children}
      </div>
    </div>,
    document.body,
  )
}
