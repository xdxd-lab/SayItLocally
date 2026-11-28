// 統一的操作結果反饋塊（成功 / 失敗 / 警告 / 提示）
//
// 為什麼要有這個元件：設定頁原來把成功和失敗塞進同一個 `text-muted-foreground` 的
// <p> 裡，靠讀文字才能分辨——「已儲存，連線成功」和「已儲存，但連線失敗」長得一模一樣。
// 而另一處（AIProviderSection）又自己手搓了紅/綠框並硬編碼 red-50/green-50，
// 第三個主題拿到的是從沒為它設計過的顏色。這裡收斂成一個走 token 的元件。
//
// 顏色規則見 src/themes/types.ts：底用 --xxx 的 10% 淡色，文字用 --xxx-strong
// （對比度達標版），描邊用 --xxx 的 30%。

import type { ReactNode } from 'react'
import { CheckCircle2, XCircle, AlertTriangle, Info } from 'lucide-react'
import { cn } from '@/lib/utils'

export type FeedbackTone = 'success' | 'error' | 'warning' | 'info'

/**
 * 每個 tone 的完整類名寫成字面量：Tailwind 的 content 掃描不認識拼接出來的類名。
 *
 * 配色為什麼這麼淡：這個應用整體是中性灰白，頁面上最重的色塊只是一枚狀態圓點。
 * 一開始這裡用的是 10% 飽和度色鋪滿整塊（info 更是純天藍），結果它比頁面上任何
 * 東西都響，跟周圍格格不入。現在的分工是：
 *   - 底和描邊儘量中性，沿用應用裡既有的提示塊寫法（border-border + bg-muted/40）；
 *   - 只有需要被立刻注意到的 warning / error 才帶一點點色（5% 底 + 25% 描邊）；
 *   - 「是成功還是失敗」由**圖示的顏色和形狀**承擔，不靠整塊底色喊。
 * 文字統一用 foreground，保證在任何主題下都讀得清。
 */
const TONE_STYLES: Record<
  FeedbackTone,
  { box: string; text: string; icon: string; Icon: typeof Info }
> = {
  success: {
    box: 'border-border bg-muted/40',
    text: 'text-foreground',
    icon: 'text-success-strong',
    Icon: CheckCircle2,
  },
  error: {
    box: 'border-destructive/25 bg-destructive/5',
    text: 'text-foreground',
    icon: 'text-destructive-strong',
    Icon: XCircle,
  },
  warning: {
    box: 'border-warning/25 bg-warning/5',
    text: 'text-foreground',
    icon: 'text-warning-strong',
    Icon: AlertTriangle,
  },
  info: {
    box: 'border-border bg-muted/40',
    text: 'text-muted-foreground',
    icon: 'text-muted-foreground',
    Icon: Info,
  },
}

export interface FeedbackAction {
  label: string
  onClick: () => void
  disabled?: boolean
}

interface FeedbackProps {
  tone: FeedbackTone
  /** 主文案：說清發生了什麼，以及下一步該往哪查 */
  message: ReactNode
  /** 原始异常等排查信息，次要字号；能看懂的人需要它，看不懂的人可以忽略 */
  detail?: string
  /** 直接可点的下一步，最多两个 */
  actions?: FeedbackAction[]
  className?: string
}

/**
 * 行內格式提示（貼在輸入框下面的一行小字）。
 *
 * 原來兩處各寫了一遍 `text-amber-500`：白底約 2.15:1，是全頁最不可讀的一行，
 * 而它偏偏是"你可能粘錯了"這種最該被看見的話；同時繞過了主題系統，
 * 第三個註冊主題拿到的是從沒為它設計過的顏色。
 */
export function FormatHint({ text }: { text: string }) {
  return (
    <p className="mt-1.5 flex items-start gap-1 text-xs text-warning-strong">
      <AlertTriangle className="mt-0.5 h-3 w-3 shrink-0" aria-hidden />
      {text}
    </p>
  )
}

export function Feedback({ tone, message, detail, actions, className }: FeedbackProps) {
  const style = TONE_STYLES[tone]
  const Icon = style.Icon

  return (
    <div
      // 失敗要讓讀屏立刻播報；成功/提示用 polite 排隊，不打斷使用者正在做的事
      role={tone === 'error' ? 'alert' : 'status'}
      aria-live={tone === 'error' ? 'assertive' : 'polite'}
      className={cn('rounded-md border px-3 py-2.5', style.box, className)}
    >
      <div className="flex gap-2">
        <Icon className={cn('mt-0.5 h-4 w-4 shrink-0', style.icon)} aria-hidden />
        <div className="min-w-0 flex-1">
          <p className={cn('text-sm leading-relaxed', style.text)}>{message}</p>

          {/* detail 原來是 font-mono：在中文 Windows 上 CJK 會落到宋體，一整塊提示裡
              突然冒出一段宋体，比等宽带来的好处难看得多（而且它现在承载的多是中文短句，
              不只是原始异常）。改回正文字体 + 保留换行，长 URL 用 break-words 兜住。 */}
          {detail && (
            <p className="mt-1.5 whitespace-pre-line break-words text-xs leading-relaxed text-muted-foreground">
              {detail}
            </p>
          )}

          {actions && actions.length > 0 && (
            <div className="mt-2 flex flex-wrap gap-2">
              {actions.map((action) => (
                <button
                  key={action.label}
                  type="button"
                  onClick={action.onClick}
                  disabled={action.disabled}
                  // 動作按鈕走中性描邊，和應用裡的 outline 按鈕一致，不跟著 tone 變色
                  className="rounded-md border border-border bg-card px-2.5 py-1 text-xs font-medium text-foreground transition-colors hover:bg-accent disabled:pointer-events-none disabled:opacity-50"
                >
                  {action.label}
                </button>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  )
}
