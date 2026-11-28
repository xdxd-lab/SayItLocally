import { forwardRef, useState, useRef, useEffect } from 'react'
import { ChevronDown, Check } from 'lucide-react'
import { useT } from '@/i18n/useT'
import { cn } from '@/lib/utils'

export interface SelectOption {
  value: string
  label: string
  /**
   * 懸停提示。只有在 label 可能被 truncate 截掉、而完整內容對使用者有用時才設
   * （麥克風列表就是：裝置名很長，但那串字尾正是辨認裝置的依據）。
   * 不設就沒有 tooltip —— 別給所有下拉都掛上，那是噪音。
   */
  title?: string
  /** 列表裡跟在 label 後面的灰色次要資訊（如供應商名），與 label 同一行。觸發器上不顯示。 */
  hint?: string
}

export interface SelectProps {
  value: string
  onChange: (value: string) => void
  options?: SelectOption[]
  children?: React.ReactNode
  className?: string
  placeholder?: string
  disabled?: boolean
  /** 關聯可見標題的 id。觸發器是個 <button>，螢幕閱讀器只會念它內部的當前值，
   *  旁邊那行 <label> 文本不會自動關聯（沒有 htmlFor 可指）。與 Segmented 同款。 */
  labelledBy?: string
  /** sm：設定行右側那種緊湊下拉（h-8、12px 字、選項行更矮）。 */
  size?: 'default' | 'sm'
  /**
   * 彈出列表的寬度。stretch（預設）= 與觸發器同寬；end = 右對齊、按內容撐開，
   * 最寬 20rem。觸發器放在一行右側又比較窄時用 end：列表往左長，不會伸出卡片，
   * 长名字也不用截得只剩半截。
   */
  menuAlign?: 'stretch' | 'end'
}

const Select = forwardRef<HTMLDivElement, SelectProps>(
  ({ value, onChange, options, children, className, placeholder, disabled = false, labelledBy, size = 'default', menuAlign = 'stretch' }, ref) => {
    const sm = size === 'sm'
    const t = useT()
    const [isOpen, setIsOpen] = useState(false)
    const containerRef = useRef<HTMLDivElement>(null)

    // 如果傳入了 children（原生 option），則解析它們
    const parsedOptions: SelectOption[] = options || []

    if (!options && children) {
      const childArray = Array.isArray(children) ? children : [children]
      childArray.forEach((child: any) => {
        if (child?.type === 'option') {
          parsedOptions.push({
            value: child.props.value || '',
            label: child.props.children || '',
          })
        }
      })
    }

    const selectedOption = parsedOptions.find((opt) => opt.value === value)

    useEffect(() => {
      const handleClickOutside = (event: MouseEvent) => {
        if (containerRef.current && !containerRef.current.contains(event.target as Node)) {
          setIsOpen(false)
        }
      }

      if (isOpen) {
        document.addEventListener('mousedown', handleClickOutside)
        return () => document.removeEventListener('mousedown', handleClickOutside)
      }
    }, [isOpen])

    const handleSelect = (optionValue: string) => {
      onChange(optionValue)
      setIsOpen(false)
    }

    return (
      <div ref={containerRef} className={cn('relative', className)}>
        <button
          ref={ref as any}
          type="button"
          onClick={() => !disabled && setIsOpen(!isOpen)}
          disabled={disabled}
          aria-labelledby={labelledBy}
          aria-haspopup="listbox"
          aria-expanded={isOpen}
          className={cn(
            'flex w-full items-center justify-between rounded-md border border-input-border bg-input-bg text-foreground transition-colors',
            sm ? 'h-8 px-2.5 text-xs' : 'h-9 px-3 text-sm',
            'hover:border-muted-foreground/40 focus:border-input-focus-border focus:outline-none focus:ring-2 focus:ring-input-focus-ring/20',
            'disabled:cursor-not-allowed disabled:opacity-50',
            isOpen && 'border-input-focus-border ring-2 ring-input-focus-ring/20',
          )}
        >
          <span
            className={cn('min-w-0 truncate', !selectedOption && 'text-input-placeholder')}
            title={selectedOption?.title}
          >
            {/* hint 只在列表裡顯示：觸發器通常很窄，帶上它只會把次要資訊截成半截 */}
            {selectedOption?.label || placeholder || t('ui.selectPlaceholder')}
          </span>
          <ChevronDown
            className={cn(
              'shrink-0 text-muted-foreground transition-transform',
              sm ? 'ml-1.5 h-3.5 w-3.5' : 'ml-2 h-4 w-4',
              isOpen && 'rotate-180',
            )}
          />
        </button>

        {isOpen && (
          <div
            className={cn(
              'absolute z-50 mt-1 rounded-md border border-border bg-card shadow-lg',
              menuAlign === 'end' ? 'right-0 w-max min-w-full max-w-xs' : 'w-full',
            )}
          >
            <div className={cn('custom-scrollbar max-h-60 overflow-y-auto', sm ? 'p-1' : 'py-1')}>
              {parsedOptions.map((option) => (
                <button
                  key={option.value}
                  type="button"
                  onClick={() => handleSelect(option.value)}
                  className={cn(
                    'flex w-full items-center justify-between text-left transition-colors',
                    sm ? 'rounded px-2 py-1.5 text-xs' : 'px-3 py-2 text-sm',
                    option.value === value
                      ? 'bg-primary/5 text-primary font-medium'
                      : 'text-foreground hover:bg-accent',
                  )}
                >
                  <span className="min-w-0 truncate" title={option.title}>
                    {option.label}
                    {option.hint && (
                      <span className={cn('ml-1.5 font-normal text-muted-foreground', sm && 'text-[11px]')}>{option.hint}</span>
                    )}
                  </span>
                  {option.value === value && <Check className={cn('shrink-0', sm ? 'ml-2 h-3.5 w-3.5' : 'ml-2 h-4 w-4')} />}
                </button>
              ))}
            </div>
          </div>
        )}
      </div>
    )
  },
)

Select.displayName = 'Select'

export { Select }
