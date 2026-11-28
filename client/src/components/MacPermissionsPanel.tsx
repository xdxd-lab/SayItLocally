import { useCallback, useEffect, useState } from 'react'
import { CheckCircle2, AlertCircle, Mic, Keyboard, ShieldAlert } from 'lucide-react'
import { Card, CardContent } from '@/components/ui/card'
import * as bridge from '@/services/bridge'
import type { PlatformPermissions } from '@/services/bridge'
import { useT } from '@/i18n/useT'

/**
 * macOS 許可權面板：輔助功能（全域性快捷鍵、插字、讀游標上下文）與麥克風。
 *
 * 輔助功能系統不會彈窗直接授予，只能帶使用者去系統設定勾選；勾選後 Rust 側的 event tap
 * 會在 2 秒內自動裝上，不用重啟。這裡每 1.5 秒輪詢一次狀態，使用者在系統設定裡勾完
 * 切回來就能看到變綠。
 */
export function MacPermissionsPanel({ compact = false }: { compact?: boolean }) {
  const t = useT()
  const [status, setStatus] = useState<PlatformPermissions | null>(null)

  const refresh = useCallback(async () => {
    const next = await bridge.getPlatformPermissions().catch(() => null)
    if (next) setStatus(next)
  }, [])

  useEffect(() => {
    void refresh()
    const timer = window.setInterval(() => { void refresh() }, 1500)
    return () => window.clearInterval(timer)
  }, [refresh])

  const requestAccessibility = async () => {
    const trusted = await bridge.requestAccessibilityPermission().catch(() => false)
    if (!trusted) await bridge.openPrivacySettings('accessibility').catch(() => {})
    void refresh()
  }

  const requestMicrophone = async () => {
    if (status?.microphone === 'denied' || status?.microphone === 'restricted') {
      await bridge.openPrivacySettings('microphone').catch(() => {})
      return
    }
    // 第一次呼叫 getUserMedia 會觸發系統的麥克風授權彈窗；拿到流立刻關掉
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true })
      stream.getTracks().forEach((track) => track.stop())
    } catch {
      await bridge.openPrivacySettings('microphone').catch(() => {})
    }
    void refresh()
  }

  if (!status || status.platform !== 'macos') return null

  const micGranted = status.microphone === 'authorized'
  const rows = [
    {
      key: 'accessibility',
      icon: Keyboard,
      title: t('permissions.accessibility.title'),
      desc: t('permissions.accessibility.desc'),
      granted: status.accessibility,
      action: t('permissions.openSettings'),
      onClick: requestAccessibility,
    },
    {
      key: 'microphone',
      icon: Mic,
      title: t('permissions.microphone.title'),
      desc: t('permissions.microphone.desc'),
      granted: micGranted,
      action: status.microphone === 'not_determined' ? t('permissions.allowMicrophone') : t('permissions.openSettings'),
      onClick: requestMicrophone,
    },
  ]

  return (
    <div className="space-y-3">
      {rows.map(({ key, icon: Icon, title, desc, granted, action, onClick }) => (
        <Card key={key}>
          <CardContent className={compact ? 'flex items-center gap-3 p-3' : 'flex items-center gap-4 p-4'}>
            <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-primary/10 text-primary">
              <Icon className="h-5 w-5" />
            </div>
            <div className="min-w-0 flex-1 text-left">
              <p className="text-sm font-medium">{title}</p>
              <p className="mt-0.5 text-xs text-muted-foreground">{desc}</p>
            </div>
            {granted ? (
              <span className="flex shrink-0 items-center gap-1 text-xs text-emerald-600">
                <CheckCircle2 className="h-4 w-4" />
                {t('permissions.granted')}
              </span>
            ) : (
              <button
                type="button"
                onClick={() => { void onClick() }}
                className="shrink-0 rounded-lg bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground transition-colors hover:bg-primary/90"
              >
                {action}
              </button>
            )}
          </CardContent>
        </Card>
      ))}

      {status.accessibility && !status.hotkeyActive && (
        <p className="flex items-start gap-1.5 text-left text-xs text-muted-foreground">
          <AlertCircle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
          {t('permissions.restartHint')}
        </p>
      )}
      {status.secureInput && (
        <p className="flex items-start gap-1.5 text-left text-xs text-amber-600">
          <ShieldAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" />
          {t('permissions.secureInput')}
        </p>
      )}
      {status.translocated && (
        <p className="flex items-start gap-1.5 text-left text-xs text-amber-600">
          <AlertCircle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
          {t('permissions.translocated')}
        </p>
      )}
    </div>
  )
}

export default MacPermissionsPanel
