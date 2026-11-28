// 金鑰輸入框（帶「顯示/隱藏」切換）
//
// 為什麼抽出來：設定頁裡有兩處各自實現了一遍，兩處的眼睛按鈕都是 tabIndex={-1} 且沒有
// 可訪問名稱——純鍵盤使用者無法確認自己貼上的金鑰對不對，等於配不了雲 API / AI 供應商。
// 收成一個元件後，這條語義只需修一次。

import { useState } from 'react'
import { Eye, EyeOff } from 'lucide-react'
import { useT } from '@/i18n/useT'
import { cn } from '@/lib/utils'

interface PasswordInputProps {
  /** 必填：與外部 <label htmlFor> 配對，讀屏才能念出這個框叫什麼 */
  id: string
  /** 用於組裝眼睛按鈕的名稱，如 'API Key' → 「顯示 API Key」 */
  label: string
  value: string
  onChange: (value: string) => void
  /** 回車提交。省略則回車無行為 */
  onSubmit?: () => void
  placeholder?: string
  className?: string
}

export function PasswordInput({
  id,
  label,
  value,
  onChange,
  onSubmit,
  placeholder,
  className,
}: PasswordInputProps) {
  const t = useT()
  const [visible, setVisible] = useState(false)

  return (
    <div className="relative">
      <input
        id={id}
        type={visible ? 'text' : 'password'}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => { if (e.key === 'Enter' && onSubmit) onSubmit() }}
        placeholder={placeholder}
        // 右內邊距由元件自己兜住，**不能交給呼叫方**：眼睛按鈕是絕對定位的，佔掉右側
        // 8~26px；呼叫方給的 inputClass 只有 px-3（右 12px），金鑰文字會壓在圖示下面。
        // 四個呼叫點傳的是各自定義、字串卻一模一樣的 inputClass，靠呼叫方補等於四處都要記得。
        // pr-9 = 36px，給按鈕 26px 再留 10px 間隙。twMerge 會丟掉呼叫方自帶的 pr-*，
        // 而 px-* 由 Tailwind 輸出順序（px 在 pr 之前）讓 pr-9 勝出。
        className={cn(className, 'pr-9')}
      />
      <button
        type="button"
        onClick={() => setVisible(!visible)}
        aria-label={t(visible ? 'ui.hidePassword' : 'ui.showPassword', { label })}
        aria-pressed={visible}
        className="absolute right-2 top-1/2 -translate-y-1/2 rounded p-0.5 text-muted-foreground transition-colors hover:text-foreground"
      >
        {visible ? <EyeOff className="h-3.5 w-3.5" aria-hidden /> : <Eye className="h-3.5 w-3.5" aria-hidden />}
      </button>
    </div>
  )
}
