// 單選膠囊組（識別語言 / 下載源 / 計算後端 / 模型駐留 …）
//
// 為什麼抽出來：這個形態在設定頁裡被手抄了 5 遍，每一遍都是裸 <button>，選中態**只有顏色**
// （border-primary bg-primary），既沒有 role="radio" 也沒有 aria-checked。鍵盤使用者 Tab 過
// 「自動 / 中文 / 英文」時拿不到"哪個是當前值"這條資訊。收成一個元件後，語義只需修一次。
//
// 視覺與原來逐字一致（md 對應原 px-4 py-1.5 text-sm，sm 對應原 px-3 py-1 text-xs），
// 只額外給選中項加了 font-medium 作為非顏色標記。選中底色後來改成了滑塊（換選項時滑過去）。

import { useRef } from 'react'
import { cn } from '@/lib/utils'
import { useSlidingIndicator } from './useSlidingIndicator'

export interface SegmentedOption<T> {
  value: T
  label: string
  disabled?: boolean
}

interface SegmentedProps<T> {
  /** 無障礙名稱。有可見標題時傳 labelledBy 代替 */
  label?: string
  labelledBy?: string
  value: T
  options: ReadonlyArray<SegmentedOption<T>>
  onChange: (value: T) => void
  /** xs：附在一行內容末尾的次要切換（如自定義波形的單色 / 漸變），比 sm 再小一號 */
  size?: 'xs' | 'sm' | 'md'
  /** 整組停用（如切換計算後端時正在過載模型） */
  disabled?: boolean
  /** Whether option color changes should animate. */
  animated?: boolean
  className?: string
}

export function Segmented<T extends string | number>({
  label,
  labelledBy,
  value,
  options,
  onChange,
  size = 'md',
  disabled,
  animated = true,
  className,
}: SegmentedProps<T>) {
  const groupRef = useRef<HTMLDivElement>(null)
  // 選中底色是一塊獨立的滑塊（見 useSlidingIndicator），選項自己不再畫選中底色。
  // animated=false 的場合（設定值剛讀回來）滑塊也只能原地出現，否則又是一次"自己動了一下"。
  const indicatorRef = useSlidingIndicator(groupRef, value, { animated })
  return (
    <div
      ref={groupRef}
      role="radiogroup"
      aria-label={label}
      aria-labelledby={labelledBy}
      className={cn('relative flex flex-wrap items-center gap-1', className)}
    >
      <span
        ref={indicatorRef}
        aria-hidden
        className={cn('sliding-indicator bg-foreground/[0.08]', size === 'xs' ? 'rounded-md' : 'rounded-lg')}
      />
      {options.map((option) => {
        const isActive = option.value === value
        return (
          <button
            key={String(option.value)}
            type="button"
            role="radio"
            aria-checked={isActive}
            disabled={disabled || option.disabled}
            onClick={() => onChange(option.value)}
            className={cn(
              'relative inline-flex items-center justify-center rounded-lg font-medium outline-none disabled:pointer-events-none disabled:opacity-50',
              'focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background',
              animated && 'transition-colors',
              size === 'xs' ? 'h-6 rounded-md px-2 text-xs' : size === 'sm' ? 'h-7 px-2.5 text-xs' : 'h-8 px-3 text-sm',
              // 選中項的 hover 疊在滑塊上：0.08 + 0.05 ≈ 原來的 0.13。
              isActive
                ? 'bg-transparent text-foreground hover:bg-foreground/[0.05]'
                : 'bg-transparent text-muted-foreground hover:bg-foreground/[0.05] hover:text-foreground',
            )}
          >
            {option.label}
          </button>
        )
      })}
    </div>
  )
}
