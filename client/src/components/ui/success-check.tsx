import { Check, type LucideProps } from 'lucide-react'
import { cn } from '@/lib/utils'

/**
 * "已複製 / 已儲存"那一下的對勾：描線畫出來，輕輕彈一下（動畫見 index.css 的 .success-check）。
 *
 * 只在**狀態剛變成成功**時掛載它：動畫跟著掛載播一次。所以呼叫處保持
 * `copied ? <SuccessCheck/> : <Copy/>` 這種二選一的寫法就對了，別讓它常駐再切 class。
 */
export function SuccessCheck({ className, ...props }: LucideProps) {
  return <Check aria-hidden {...props} className={cn('success-check', className)} />
}
