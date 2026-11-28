// 伺服器模式配置 — 服務地址 + 連線狀態

import { useEffect, useState } from 'react'
import { Info } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Tooltip } from '@/components/ui/tooltip'
import { Feedback, type FeedbackTone } from '@/components/ui/feedback'
import { Segmented } from '@/components/ui/segmented'
import { InfoTip } from '@/components/ui/info-tip'
import {
  getBackendBaseUrl,
  getDefaultBackendBaseUrl,
  resetBackendBaseUrl,
  setBackendBaseUrl as persistBackendBaseUrl,
} from '@/services/runtimeConfig'
import { reconnectProvider } from '@/services/recorder'
import { checkForUpdateNow, discardPendingForChannelSwitch } from '@/features/update/autoUpdate'
import { getSetting, setSetting } from '@/services/store'
import { setEngineDraftDirty } from '@/stores/engineDraft'
import { describeServerError } from '@/lib/errorMessages'
import { t } from '@/i18n'
import { useT } from '@/i18n/useT'

interface ServiceResult {
  tone: FeedbackTone
  message: string
  detail?: string
}

export default function ServerSection() {
  const t = useT()
  const [backendBaseUrl, setBackendBaseUrl] = useState('')
  /** 已保存的地址。输入框与它不一致就是「未保存」 */
  const [savedBaseUrl, setSavedBaseUrl] = useState('')
  const [defaultBaseUrl, setDefaultBaseUrl] = useState('')
  const [result, setResult] = useState<ServiceResult | null>(null)
  const [busy, setBusy] = useState(false)
  const [asrLanguage, setAsrLanguage] = useState('auto')

  useEffect(() => {
    const current = getBackendBaseUrl()
    setBackendBaseUrl(current)
    setSavedBaseUrl(current)
    setDefaultBaseUrl(getDefaultBackendBaseUrl())
    void getSetting('server.language', 'auto').then((v) => setAsrLanguage(String(v || 'auto')))
    // 切走路由時把"有未儲存改動"復位，別把髒狀態留給下一次進入
    return () => setEngineDraftDirty(false)
  }, [])

  const normalize = (v: string) => v.trim().replace(/\/+$/, '')

  const isDirty = normalize(backendBaseUrl) !== normalize(savedBaseUrl)
  const isCustom = normalize(savedBaseUrl) !== normalize(defaultBaseUrl)

  function handleUrlChange(value: string) {
    setBackendBaseUrl(value)
    setResult(null)
    setEngineDraftDirty(normalize(value) !== normalize(savedBaseUrl))
  }

  /** 探一次 /healthz。成功返回後端上報的 ASR/LLM 開關，失敗丟擲原始異常。 */
  async function probeHealth(url: string): Promise<{ asr?: boolean; llm?: boolean }> {
    const response = await fetch(`${url}/healthz`, { cache: 'no-store' })
    if (!response.ok) throw new Error(`HTTP ${response.status}`)
    return await response.json() as { asr?: boolean; llm?: boolean }
  }

  /** 把 /healthz 的 asr/llm 布林量翻譯成人話。原來直接顯示「ASR=on，LLM=off」，
   *  那是把後端的 JSON 欄位原樣貼給使用者。 */
  function describeHealth(payload: { asr?: boolean; llm?: boolean }, prefix: string): ServiceResult {
    if (payload.asr === false) {
      return {
        tone: 'warning',
        message: t('server.asrNotReady', { prefix }),
      }
    }
    if (payload.llm === false) {
      return {
        tone: 'success',
        message: t('server.noAi', { prefix }),
      }
    }
    return { tone: 'success', message: t('server.allGood', { prefix }) }
  }

  /**
   * 儲存並測試。
   *
   * 這裡原來是兩個同權重的按鈕：「測試連線」只測不存（用輸入框裡的值），「儲存」存了再測。
   * 使用者點前者看到「連線成功」，合理地以為配置生效了——它沒有。兩個動作合併成一個之後，
   * 介面上就不再存在"測試透過但沒儲存"這種狀態。
   */
  async function handleSaveAndTest() {
    if (busy) return
    const normalized = normalize(backendBaseUrl)
    if (!normalized) {
      setResult({ tone: 'warning', message: t('server.urlEmpty') })
      return
    }
    try {
      new URL(normalized)
    } catch {
      setResult({
        tone: 'warning',
        message: t('server.urlInvalid'),
      })
      return
    }

    setBusy(true)
    setResult(null)
    try {
      const next = await persistBackendBaseUrl(normalized)
      setBackendBaseUrl(next)
      setSavedBaseUrl(next)
      setEngineDraftDirty(false)
      // 地址已變更：無論下方健康檢查成功與否，都按新地址強制重連，
      // 讓左下角連線狀態反映新配置（改成錯誤地址後應顯示未連線，而非仍舊"已連線"）
      reconnectProvider()
      // 更新檢查跟隨這個地址，所以換了地址就要重新查一次（見 getUpdateBaseUrl）。
      // 必須先丟棄已下載的包：ensureDownloaded 只按版本號判斷"已經在盤上了"，
      // 版本號相同不代表來自同一台伺服器，留著會讓"指到測試伺服器驗一遍"
      // 實際裝的還是上一個來源那個包。
      await discardPendingForChannelSwitch()
      void checkForUpdateNow()
    } catch (error) {
      setResult({ tone: 'error', message: t('server.saveFailed'), detail: String(error) })
      setBusy(false)
      return
    }

    try {
      const payload = await probeHealth(normalized)
      setResult(describeHealth(payload, t('server.savedPrefix')))
    } catch (error) {
      const friendly = describeServerError(error, normalize(normalized) !== normalize(defaultBaseUrl))
      setResult({
        tone: 'error',
        message: t('server.savedButUnreachable', { message: friendly.message }),
        detail: friendly.detail,
      })
    } finally {
      setBusy(false)
    }
  }

  /** 恢復到內建預設地址並立刻重連，省得使用者自己回憶預設值是什麼 */
  async function handleResetDefault() {
    if (busy) return
    setBusy(true)
    setResult(null)
    try {
      const next = await resetBackendBaseUrl()
      setBackendBaseUrl(next)
      setSavedBaseUrl(next)
      setEngineDraftDirty(false)
      reconnectProvider()
      await discardPendingForChannelSwitch()
      void checkForUpdateNow()
      const payload = await probeHealth(next)
      setResult(describeHealth(payload, t('server.restoredPrefix', { url: next })))
    } catch (error) {
      const friendly = describeServerError(error, false)
      setResult({
        tone: 'error',
        message: t('server.restoredButUnreachable', { message: friendly.message }),
        detail: friendly.detail,
      })
    } finally {
      setBusy(false)
    }
  }

  return (
    <>
      <Card>
        <CardContent className="p-6">
          <h2 className="text-lg font-semibold">{t('server.title')}</h2>
          <p className="mt-1 flex items-center gap-1 text-sm text-muted-foreground">
            {t('server.desc')}
            <Tooltip
              variant="light"
              content={t('server.help')}
            >
              <Info className="h-3.5 w-3.5 shrink-0 cursor-help text-muted-foreground transition-colors hover:text-foreground" />
            </Tooltip>
          </p>

          <div className="mt-4">
            <div className="mb-1.5 flex items-center gap-2">
              <label htmlFor="server-base-url" className="text-sm text-muted-foreground">
                {t('server.title')}
              </label>
              {/* 輸入框內容只活在 local state 裡，切頁就沒了。原來這件事完全無提示，
                  用户会以为改完就生效了。 */}
              {isDirty && (
                <span className="rounded-full bg-warning/10 px-2 py-0.5 text-xs font-medium text-warning-strong">
                  {t('server.unsaved')}
                </span>
              )}
            </div>
            {/* flex-wrap：800×600 最小視窗下側欄佔掉 192px，輸入框 + 按鈕擠在一行會溢位 */}
            <div className="flex flex-wrap items-center gap-2">
              <input
                id="server-base-url"
                type="url"
                inputMode="url"
                value={backendBaseUrl}
                onChange={(e) => handleUrlChange(e.target.value)}
                onKeyDown={(e) => { if (e.key === 'Enter') void handleSaveAndTest() }}
                placeholder={defaultBaseUrl || 'https://sayitapp.site'}
                className="h-9 min-w-[16rem] flex-1 rounded-md border border-input-border bg-input-bg px-3 text-sm transition-colors focus:border-input-focus-border"
              />
              <Button size="sm" className="h-9 shrink-0" onClick={() => void handleSaveAndTest()} disabled={busy}>
                {busy ? t('server.savingAndTesting') : t('server.saveAndTest')}
              </Button>
              {isCustom && (
                <Button size="sm" variant="ghost" className="h-9 shrink-0" onClick={() => void handleResetDefault()} disabled={busy}>
                  {t('server.restoreDefault')}
                </Button>
              )}
            </div>
          </div>

          {/* 這裡原來在失敗提示裡再掛一個「恢復預設地址（https://…）」按鈕：
              一是把整条 URL 塞进按钮文字，全应用没有第二处这么写；
              二是它和输入框旁边那个「恢复默认」完全同义——而后者在地址被改过时一直都在，
              正好覆盖会出现这条失败提示的全部情况。留一个就够。 */}
          {result && (
            <Feedback
              className="mt-3"
              tone={result.tone}
              message={result.message}
              detail={result.detail}
            />
          )}
        </CardContent>
      </Card>

      <Card>
        <CardContent className="p-6">
          <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
            <div className="min-w-0">
              <div className="flex items-center gap-1.5">
                <h2 id="server-language-heading" className="text-lg font-semibold">{t('server.languageTitle')}</h2>
                <InfoTip content={t('server.languageNote')} label={t('server.languageTitle')} />
              </div>
              <p className="mt-1 text-xs text-muted-foreground">{t('server.languageShortNote')}</p>
            </div>
            <Segmented
              labelledBy="server-language-heading"
              value={asrLanguage}
              options={[
                { value: 'auto', label: t('common.auto') },
                { value: 'zh', label: t('local.lang.zh') },
                { value: 'en', label: t('local.lang.en') },
              ]}
              onChange={(value) => { setAsrLanguage(value); void setSetting('server.language', value) }}
              className="shrink-0 justify-end"
            />
          </div>
        </CardContent>
      </Card>
    </>
  )
}
