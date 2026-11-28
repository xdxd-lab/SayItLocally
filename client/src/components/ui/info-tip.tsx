import type { ReactNode } from 'react'
import { Info } from 'lucide-react'
import { Tooltip } from '@/components/ui/tooltip'
import { t } from '@/i18n'

/**
 * 標題旁的 ⓘ：說明文字平時收起，滑鼠移上去才顯示。
 *
 * 介面上大段灰字壓縮以後，原來那些解釋不能直接刪（使用者遇到不懂的地方要有地方查），
 * 統一挪到這裡。提示框是 pointer-events-none 的，裡面放不了可點的連結 —— 連結要留在正文裡。
 */
export function InfoTip({ content, label }: { content: ReactNode; label: string }) {
  return (
    <Tooltip variant="light" content={content}>
      <Info
        aria-label={t('settings.helpAria', { label })}
        className="h-3.5 w-3.5 shrink-0 cursor-help text-muted-foreground/50 transition-colors hover:text-muted-foreground"
      />
    </Tooltip>
  )
}

/** 頁面大標題。原來標題下面都跟一句副標題，現在副標題收進 ⓘ。 */
export function PageTitle({ title, info }: { title: string; info?: ReactNode }) {
  return (
    <div className="mb-6 flex items-center gap-2">
      <h1 className="text-2xl font-bold">{title}</h1>
      {info && <InfoTip content={info} label={title} />}
    </div>
  )
}
