import { cn } from '@/lib/utils'

interface SwitchProps {
  checked: boolean
  onChange: () => void
  /** 開關右側的可見文字，同時作為無障礙名稱 */
  label?: string
  /** 沒有可見文字時，指向承擔名稱的元素 id（如卡片標題）。
   *  兩者都不給的話，讀屏只會念出一個沒有名字的「切換按鈕」。 */
  labelledBy?: string
  disabled?: boolean
  className?: string
  /** 'sm' 適合緊湊列表行 */
  size?: 'default' | 'sm'
  /** 初始化階段傳 true：跳過顏色/位移過渡，避免「預設值→已儲存值」被當成使用者操作動畫出來 */
  noAnimation?: boolean
  /** 初始化階段傳 true：佔位但不可見（用內聯 style，不依賴 Tailwind 產出 .invisible 類） */
  hidden?: boolean
}

/**
 * 統一 Switch 開關元件
 * default: h-5 w-9 / sm: h-4 w-7
 */
export function Switch({ checked, onChange, label, labelledBy, disabled, className, size = 'default', noAnimation, hidden }: SwitchProps) {
  const sm = size === 'sm'
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-labelledby={label ? undefined : labelledBy}
      onClick={onChange}
      disabled={disabled}
      style={hidden ? { visibility: 'hidden' } : undefined}
      className={cn('inline-flex items-center gap-2 disabled:cursor-not-allowed disabled:opacity-50', className)}
    >
      <span
        className={cn(
          'relative shrink-0 rounded-full',
          !noAnimation && 'transition-colors',
          sm ? 'h-4 w-7' : 'h-5 w-9',
          checked ? 'bg-primary' : 'bg-muted',
        )}
      >
        <span
          className={cn(
            'absolute left-0.5 top-0.5 rounded-full bg-card shadow',
            !noAnimation && 'transition-transform',
            sm ? 'h-3 w-3' : 'h-4 w-4',
            checked && (sm ? 'translate-x-3' : 'translate-x-4'),
          )}
        />
      </span>
      {label && <span className="text-sm">{label}</span>}
    </button>
  )
}
