// WebDAV 自动备份的设置面板。
//
// 分工：**卡面回答「現在在做什麼」，彈窗回答「怎麼配」。**
//
// 這張卡片原先把兩件事混在一起——一次性配置伺服器，和日常檢視/觸發備份。前者做完
// 就再也不碰，常駐在設定頁上是最主要的噪聲來源（地址、賬號、密碼、堅果雲說明、測試
// 按鈕、測試結果，七組元素佔了半張卡，而它們一年用一次）。
//
// 現在：
// - 卡面：一句說明 + 「連到了哪、備份什麼、多久一次」一行摘要 + 一個主動作 + 一行狀態。
//   沒配置時只有說明和右上角一個按鈕 —— 絕大多數使用者永遠不會開這個功能，預設狀態
//   不該給他們一整面控制元件。
// - 彈窗：地址憑證、**備份內容**、頻率、保留份數。備份內容必須在這裡，因為首次配置
//   時使用者就該看見「會上傳哪些東西」並當場選擇，而不是點了完成之後才在卡面上發現。
//   三項（配置/歷史/錄音）全部列出，配置那一項顯示「始終包含」而不是藏起來——
//   「會同步哪些檔案」這個問題要有一個看得見的完整答案。
// - 錄音開啟時**就地**出現一行提示，不彈二級確認框：彈窗裡再套彈窗是反模式，而且
//   常駐提示比點一次就消失的確認更有用（只要錄音是開著的，代價就一直寫在那兒）。
//
// 功能一個沒少，收進彈窗的仍然可達，只是不再和日常操作搶注意力。
//
// 另外兩條與視覺無關但同樣刻意的：
// - 檔位（含不含歷史/錄音）只存在設定裡，手動備份和定時備份都讀設定，介面上勾了
//   什麼就一定是上傳的內容。
// - 錄音預設關。錄音庫能有幾個 GB，而網盤有月上傳流量額度，預設開啟等於替使用者做了
//   一個很貴的決定。

import { useEffect, useState } from 'react'
import { CloudUpload, Loader2, RotateCcw, Settings2 } from 'lucide-react'
import { cn } from '@/lib/utils'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Modal } from '@/components/ui/modal'
import { PasswordInput } from '@/components/ui/password-input'
import { Segmented } from '@/components/ui/segmented'
import { Switch } from '@/components/ui/switch'
import { Tooltip } from '@/components/ui/tooltip'
import { FormatHint } from '@/components/ui/feedback'
import { getSetting, setSetting } from '@/services/store'
import { restartApp } from '@/services/backup'
import {
  describeWebDavError,
  webDavErrorDetail,
  listWebDavBackups,
  onWebDavBackupProgress,
  restoreWebDavBackup,
  testWebDavConnection,
  DEFAULT_DAV_URL,
  type WebDavEntry,
  type WebDavLastResult,
  type WebDavProgress,
} from '@/services/webdavBackup'
import {
  onWebDavBackupChange,
  refreshLastResult,
  runBackupNow,
} from '@/features/backup/autoWebdavBackup'
import { formatBytes } from '@/lib/utils'
import { getLocale, type TranslationKey } from '@/i18n'
import { useT } from '@/i18n/useT'

const inputClass =
  'h-9 w-full rounded-md border border-input-border bg-input-bg px-3 text-sm transition-colors focus:border-input-focus-border'

/**
 * 卡上圖示按鈕，與「AI 供應商」服務卡上的那幾顆同一套樣式。
 *
 * 與那邊的一個區別：這裡**不做 hover 才顯形**。那邊網格里有三五張卡，圖示常顯會很吵；
 * 這裡只有一張卡，藏起來只會讓「立即備份」變得找不到。
 */
const cardIconButtonClass =
  'rounded p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:pointer-events-none disabled:opacity-40'

const phaseLabelKeys: Record<WebDavProgress['phase'], TranslationKey> = {
  preparing: 'backup.status.preparing',
  packingData: 'backup.status.packingData',
  packingAudio: 'backup.status.packingAudio',
  finalizing: 'backup.status.finalizing',
  uploading: 'webdav.status.uploading',
  verifying: 'webdav.status.verifying',
  completed: 'backup.status.completed',
  failed: 'backup.status.failed',
}

const intervalOptions = [
  { value: 24, labelKey: 'webdav.interval.daily' },
  { value: 72, labelKey: 'webdav.interval.every3Days' },
  { value: 168, labelKey: 'webdav.interval.weekly' },
] as const satisfies readonly { value: number; labelKey: TranslationKey }[]

/**
 * 四種檔位各有一個完整句子，而不是把「配置」「歷史記錄」拼起來。
 * 拼接在中文裡勉強能讀，換到別的語言就會出「Settings and 歷史記錄」式的畸形。
 */
const contentSummaryKeys: Record<string, TranslationKey> = {
  'false,false': 'webdav.summary.config',
  'true,false': 'webdav.summary.configHistory',
  'false,true': 'webdav.summary.configAudio',
  'true,true': 'webdav.summary.all',
}

type Busy = 'test' | 'list' | 'restore' | null

/** 卡面上只需要認出「連到了哪」。協議頭永遠是 https，顯示它等於八個字元的噪聲。 */
function shortServer(url: string): string {
  return url.trim().replace(/^https:\/\//i, '').replace(/\/+$/, '')
}

/** 摘要行裡的時間。完整的 toLocaleString 在 11px 一行裡太佔地方，年份也沒人看。 */
function shortTime(ms: number): string {
  // [自訂] SC-65：跟介面語言走，不跟系統語系（簡體系統不會帶出簡體字樣）。
  return new Date(ms).toLocaleString(getLocale(), {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  })
}

export default function WebDavSection() {
  const t = useT()

  // ready 控制「值到位才顯示」，animate 控制「隔兩幀才允許過渡」。
  // 合成一個標誌修不掉自己動一下的問題：揭開的同一幀加回 transition，瀏覽器會把
  // 「預設值 → 已儲存值」真的動畫一遍（見 pitfalls 裡設定類控制元件那條）。
  const [ready, setReady] = useState(false)
  const [animate, setAnimate] = useState(false)

  const [enabled, setEnabled] = useState(false)
  const [url, setUrl] = useState('')
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [includeHistory, setIncludeHistory] = useState(false)
  const [includeAudio, setIncludeAudio] = useState(false)
  const [intervalHours, setIntervalHours] = useState(24)
  const [keepCount, setKeepCount] = useState(5)

  const [busy, setBusy] = useState<Busy>(null)
  const [running, setRunning] = useState(false)
  const [progress, setProgress] = useState<WebDavProgress | null>(null)
  const [lastResult, setLastResult] = useState<WebDavLastResult | null>(null)
  const [testMessage, setTestMessage] = useState('')
  const [actionError, setActionError] = useState('')

  const [serverOpen, setServerOpen] = useState(false)
  const [restoreList, setRestoreList] = useState<WebDavEntry[] | null>(null)
  const [restoreTarget, setRestoreTarget] = useState<WebDavEntry | null>(null)
  const [restoreDone, setRestoreDone] = useState(false)

  useEffect(() => {
    let cancelled = false
    void (async () => {
      const [en, u, name, pass, hist, audio, interval, keep] = await Promise.all([
        getSetting('webdav.enabled', false).catch(() => false),
        getSetting('webdav.url', '').catch(() => ''),
        getSetting('webdav.username', '').catch(() => ''),
        getSetting('webdav.password', '').catch(() => ''),
        getSetting('webdav.includeHistory', false).catch(() => false),
        getSetting('webdav.includeAudio', false).catch(() => false),
        getSetting('webdav.intervalHours', 24).catch(() => 24),
        getSetting('webdav.keepCount', 5).catch(() => 5),
      ])
      if (cancelled) return
      setEnabled(en)
      // 地址預填堅果雲，但**必須同時落庫**：Rust 側備份時是直接讀庫的，而 getSetting
      // 的預設值只活在前端記憶體裡。使用者只填了賬號密碼、沒碰過位址列時，庫裡就沒有這
      // 一行，備份會以「地址為空」失敗 —— 而介面上明明顯示著地址。
      if (u.trim()) {
        setUrl(u)
      } else {
        setUrl(DEFAULT_DAV_URL)
        await setSetting('webdav.url', DEFAULT_DAV_URL).catch(() => undefined)
      }
      setUsername(name)
      setPassword(pass)
      setIncludeHistory(hist)
      setIncludeAudio(audio)
      if (interval === 24 || interval === 72 || interval === 168) setIntervalHours(interval)
      if (keep === 3 || keep === 5 || keep === 10) setKeepCount(keep)
      setLastResult(await refreshLastResult())
      setReady(true)
      // 隔兩幀再允許過渡：第一幀用來把已儲存值畫上去。
      requestAnimationFrame(() => requestAnimationFrame(() => { if (!cancelled) setAnimate(true) }))
    })()
    return () => { cancelled = true }
  }, [])

  useEffect(() => {
    const unlisten = onWebDavBackupProgress(setProgress)
    return () => { void unlisten.then((fn) => fn()) }
  }, [])

  // 後台定時備份也會改狀態，面板要跟著動，否則使用者看到的是開啟設定那一刻的快照。
  useEffect(() => {
    const off = onWebDavBackupChange((state) => {
      setRunning(state.running)
      setLastResult(state.lastResult)
    })
    return off
  }, [])

  // 三樣都填了就算配好了。彈窗裡邊打邊存，所以這個值也正好是「測試連線」能不能點。
  const configured = Boolean(url.trim() && username.trim() && password)
  const isBusy = busy !== null || running

  /**
   * 位址列的就地告警。
   *
   * 說明文字裡刻意不寫「必須是 https」——填對的人不需要被告知規則，填錯的人才需要，
   * 而那時一句針對性的告警比一條常駐的規則有用。所以這裡要覆蓋**所有**非 https 的
   * 情況，而不只是 `http://`：只查 http 的話，粘一個裸域名（`dav.jianguoyun.com/dav`）
   * 會一路沒有提示，直到點測試連線才報錯。
   */
  const urlWarning = (() => {
    const value = url.trim().toLowerCase()
    if (!value) return null
    if (value.startsWith('http://')) return t('webdav.error.urlInsecure')
    if (!value.startsWith('https://')) return t('webdav.error.urlScheme')
    return null
  })()

  const patch = async <T,>(key: string, value: T, apply: (value: T) => void) => {
    apply(value)
    await setSetting(key, value)
  }

  const closeServerDialog = () => {
    setServerOpen(false)
    setTestMessage('')
    setActionError('')
  }

  const handleTest = async () => {
    setBusy('test')
    setTestMessage('')
    setActionError('')
    try {
      const count = await testWebDavConnection({ url, username, password })
      setTestMessage(t('webdav.testOk', { count }))
    } catch (error) {
      setActionError(describeWebDavError(error))
    } finally {
      setBusy(null)
    }
  }

  const handleOpenRestore = async () => {
    setBusy('list')
    setActionError('')
    try {
      setRestoreList(await listWebDavBackups())
    } catch (error) {
      setActionError(describeWebDavError(error))
    } finally {
      setBusy(null)
    }
  }

  const handleRestore = async () => {
    if (!restoreTarget) return
    const name = restoreTarget.name
    setRestoreTarget(null)
    setRestoreList(null)
    setBusy('restore')
    setActionError('')
    try {
      await restoreWebDavBackup(name)
      setRestoreDone(true)
      // 與本地匯入一致：略作停留讓使用者看到提示，再重啟使更改生效。
      setTimeout(() => { void restartApp() }, 1500)
    } catch (error) {
      setActionError(describeWebDavError(error))
      setBusy(null)
    }
  }

  const uploading = progress?.status === 'running'
  const percent = Math.max(0, Math.min(100, progress?.percent ?? 0))
  const contentSummary = t(contentSummaryKeys[`${includeHistory},${includeAudio}`])
  const intervalLabel = t(
    intervalOptions.find((opt) => opt.value === intervalHours)?.labelKey ?? 'webdav.interval.daily',
  )

  /**
   * 狀態徽標，寫法沿用「AI 供應商」卡片上的那一枚（同樣的圓角、字號、配色語彙）。
   * 備份中只顯示百分比 —— 階段名（「正在打包錄音檔案…」）太長，塞進徽標會把地址擠掉，
   * 它落在下面的進度行裡。
   */
  const chip = uploading
    ? { text: `${Math.round(percent)}%`, box: 'bg-warning/10 text-warning-strong' }
    : lastResult?.ok
      ? { text: t('webdav.state.ok'), box: 'bg-success/10 text-success-strong' }
      : lastResult
        ? { text: t('webdav.state.failed'), box: 'bg-destructive/10 text-destructive-strong' }
        : { text: t('webdav.state.never'), box: 'bg-muted text-muted-foreground' }

  // 摘要：備份什麼 · 多久一次（僅自動備份開啟時）· 上次什麼時候 · 多大
  const metaParts = [contentSummary]
  if (enabled) metaParts.push(intervalLabel)
  if (lastResult) metaParts.push(shortTime(lastResult.at))
  if (lastResult?.ok) metaParts.push(formatBytes(lastResult.bytes))

  return (
    <Card>
      <CardContent className="p-6">
        {/* items-center 是本頁卡片頭部的既有寫法（音訊卡、日誌卡都是），右側控制元件對著
            标题+说明整块居中。用 items-start 会让 32px 高的按钮压着 28px 的标题行，
            视觉上往下坠；而且「配置服务器」按钮和「自动备份」开关高度不同，
            顶对齐时两个状态的右侧内容会错开。 */}
        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div className="min-w-0">
            <h2 className="text-lg font-semibold">{t('webdav.title')}</h2>
            <p className="mt-1 text-sm text-muted-foreground">{t('webdav.desc')}</p>
          </div>
          {/* 右上角這一格是這張卡的開關位：沒配置時是唯一的入口，配置完就換成自動備份開關。
              入口按钮单独放在说明下面会显得像一个孤立的动作，和卡片其它内容对不上。 */}
          <div className="flex shrink-0 items-center gap-2">
            {configured ? (
              <>
                <span className="text-sm text-muted-foreground" id="webdav-enabled-label">
                  {t('webdav.autoLabel')}
                </span>
                <Switch
                  checked={enabled}
                  labelledBy="webdav-enabled-label"
                  onChange={() => void patch('webdav.enabled', !enabled, setEnabled)}
                  noAnimation={!animate}
                  hidden={!ready}
                />
              </>
            ) : (
              <Button variant="outline" size="sm" className="gap-1.5" onClick={() => setServerOpen(true)}>
                <Settings2 className="h-4 w-4" />
                {t('webdav.configure')}
              </Button>
            )}
          </div>
        </div>

        {configured && (
          <>
            {/* 備份目標做成一張有邊框的小卡，寫法沿用「AI 供應商」的服務卡：
                地址 + 状态徽标一行，摘要一行。原先这些是三段没有容器的散排文字，
                夹在标题和按钮之间，读起来像还在写说明，而不是「这是已经配好的那个东西」。
                自动备份开启时用 border-primary + bg-primary/5 —— 这是本仓库表示
                「这一份正在被使用」的既有语汇，不额外增加元素。 */}
            <div
              className={cn(
                'mt-4 rounded-lg border p-3',
                enabled ? 'border-primary bg-primary/5' : 'border-border',
              )}
            >
              <div className="flex items-center gap-2">
                <span className="min-w-0 flex-1 truncate text-xs font-medium" title={url}>
                  {shortServer(url)}
                </span>
                <span
                  className={cn(
                    'inline-flex shrink-0 items-center rounded-md px-1.5 py-0.5 text-[11px] font-medium tabular-nums',
                    chip.box,
                  )}
                >
                  {chip.text}
                </span>
              </div>

              {/* 三個動作收成卡上的圖示 + tooltip，和「AI 供應商」服務卡一致。
                  原先是卡片下面三颗并排的文字按钮：一个实心加两个 ghost，宽窄不齐、
                  又和卡片抢层级，而它们本来就是「对这台服务器做的三件事」，属于这张卡。 */}
              <div className="mt-1 flex items-center gap-2">
                <span className="min-w-0 flex-1 truncate text-[11px] text-muted-foreground">
                  {uploading && progress ? t(phaseLabelKeys[progress.phase]) : metaParts.join(' · ')}
                </span>
                <div className="flex shrink-0 items-center gap-0.5">
                  <Tooltip content={running ? t('webdav.backingUp') : t('webdav.backupNow')}>
                    <button
                      type="button"
                      onClick={() => void runBackupNow('manual')}
                      disabled={isBusy}
                      aria-label={running ? t('webdav.backingUp') : t('webdav.backupNow')}
                      className={cardIconButtonClass}
                    >
                      {running
                        ? <Loader2 className="h-3.5 w-3.5 animate-spin" aria-hidden />
                        : <CloudUpload className="h-3.5 w-3.5" aria-hidden />}
                    </button>
                  </Tooltip>
                  <Tooltip content={t('webdav.restore')}>
                    <button
                      type="button"
                      onClick={() => void handleOpenRestore()}
                      disabled={isBusy}
                      aria-label={t('webdav.restore')}
                      className={cardIconButtonClass}
                    >
                      {busy === 'list' || busy === 'restore'
                        ? <Loader2 className="h-3.5 w-3.5 animate-spin" aria-hidden />
                        : <RotateCcw className="h-3.5 w-3.5" aria-hidden />}
                    </button>
                  </Tooltip>
                  <Tooltip content={t('webdav.settings')}>
                    <button
                      type="button"
                      onClick={() => setServerOpen(true)}
                      aria-label={t('webdav.settings')}
                      className={cardIconButtonClass}
                    >
                      <Settings2 className="h-3.5 w-3.5" aria-hidden />
                    </button>
                  </Tooltip>
                </div>
              </div>

              {uploading && progress && (
                <div
                  className="mt-1.5 h-1 overflow-hidden rounded-full bg-muted"
                  role="progressbar"
                  aria-label={t('webdav.progressAria')}
                  aria-valuenow={Math.round(percent)}
                  aria-valuemin={0}
                  aria-valuemax={100}
                >
                  <div
                    className="h-full rounded-full bg-primary transition-[width]"
                    style={{ width: `${percent}%` }}
                  />
                </div>
              )}

              {/* 失敗原因單獨一行：它可能很長，擠進摘要會把前面的資訊頂掉。
                  成功和失败都要能看见 —— 少了失败那一半，一个静默失败几个月的
                  备份在界面上和一个正常工作的备份长得一模一样。 */}
              {!uploading && lastResult && !lastResult.ok && (
                <p className="mt-1.5 text-[11px] text-destructive">
                  {describeWebDavError(lastResult.error ?? '')}
                  {/* 翻成中文後原始報錯放進「詳情」，排查時還能看到 */}
                  {webDavErrorDetail(lastResult.error ?? '') && (
                    <Tooltip variant="light" content={webDavErrorDetail(lastResult.error ?? '')}>
                      <span className="ml-1.5 cursor-help text-muted-foreground underline underline-offset-2">{t('webdav.errorDetail')}</span>
                    </Tooltip>
                  )}
                </p>
              )}

              {actionError && <p className="mt-1.5 text-[11px] text-destructive">{actionError}</p>}
            </div>
          </>
        )}
      </CardContent>

      {serverOpen && (
        <Modal
          title={t('webdav.settingsTitle')}
          onClose={closeServerDialog}
          showCloseButton
          panelClassName="w-[520px]"
        >
          <div className="space-y-4">
            <div>
              <label htmlFor="webdav-url" className="mb-1 block text-sm text-muted-foreground">
                {t('webdav.url')}
              </label>
              <input
                id="webdav-url"
                type="url"
                inputMode="url"
                value={url}
                onChange={(e) => void patch('webdav.url', e.target.value, setUrl)}
                placeholder={DEFAULT_DAV_URL}
                className={inputClass}
              />
              {urlWarning
                ? <FormatHint text={urlWarning} />
                : <p className="mt-1 text-[11px] text-muted-foreground">{t('webdav.urlHint')}</p>}
            </div>

            <div className="grid gap-4 sm:grid-cols-2">
              <div>
                <label htmlFor="webdav-username" className="mb-1 block text-sm text-muted-foreground">
                  {t('webdav.username')}
                </label>
                <input
                  id="webdav-username"
                  type="text"
                  autoComplete="off"
                  value={username}
                  onChange={(e) => void patch('webdav.username', e.target.value, setUsername)}
                  className={inputClass}
                />
              </div>
              <div data-modal-autofocus>
                <label htmlFor="webdav-password" className="mb-1 block text-sm text-muted-foreground">
                  {t('webdav.password')}
                </label>
                <PasswordInput
                  id="webdav-password"
                  label={t('webdav.password')}
                  value={password}
                  onChange={(value) => void patch('webdav.password', value, setPassword)}
                  onSubmit={() => void handleTest()}
                  className={inputClass}
                />
              </div>
            </div>

            {/* 堅果雲是目前驗證過的服務商，它那個必踩的點（要用應用密碼）值得佔一行。 */}
            <p className="text-[11px] text-muted-foreground">{t('webdav.jianguoyunHint')}</p>
          </div>

          {/* ── 備份內容 ──
              三项全部列出，配置那一项标「始终包含」而不是不显示：「会同步哪些文件」
              这个问题需要一个看得见的完整答案，而不是从两个开关反推。 */}
          <div className="mt-5 border-t border-border pt-4">
            <p className="text-sm font-medium">{t('webdav.contentTitle')}</p>

            <div className="mt-3 flex items-center justify-between gap-3">
              <span className="text-sm">{t('webdav.includeConfig')}</span>
              <span className="shrink-0 text-xs text-muted-foreground">{t('webdav.always')}</span>
            </div>

            <div className="mt-3 flex items-center justify-between gap-3">
              <label className="text-sm" id="webdav-history-label">{t('webdav.includeHistory')}</label>
              <Switch
                checked={includeHistory}
                labelledBy="webdav-history-label"
                onChange={() => void patch('webdav.includeHistory', !includeHistory, setIncludeHistory)}
                noAnimation={!animate}
              />
            </div>

            <div className="mt-3 flex items-center justify-between gap-3">
              <label className="text-sm" id="webdav-audio-label">{t('webdav.includeAudio')}</label>
              <Switch
                checked={includeAudio}
                labelledBy="webdav-audio-label"
                onChange={() => void patch('webdav.includeAudio', !includeAudio, setIncludeAudio)}
                noAnimation={!animate}
              />
            </div>
            {/* 就地提示而不是二級確認框：彈窗裡再套彈窗是反模式，而且只要錄音是開著的，
                代价就应该一直写在那儿，而不是点一次就消失。 */}
            {includeAudio && <FormatHint text={t('webdav.audioOnHint')} />}
          </div>

          {/* ── 頻率與保留 ── */}
          <div className="mt-4 space-y-3 border-t border-border pt-4">
            <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
              <label className="text-sm text-muted-foreground">{t('webdav.intervalLabel')}</label>
              <Segmented
                label={t('webdav.intervalLabel')}
                value={intervalHours}
                options={intervalOptions.map((opt) => ({ value: opt.value, label: t(opt.labelKey) }))}
                onChange={(value) => void patch('webdav.intervalHours', value, setIntervalHours)}
                className="shrink-0 justify-end"
              />
            </div>
            <div>
              <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
                <label className="text-sm text-muted-foreground">{t('webdav.keepLabel')}</label>
                <Segmented
                  label={t('webdav.keepLabel')}
                  value={keepCount}
                  options={[3, 5, 10].map((value) => ({ value, label: String(value) }))}
                  onChange={(value) => void patch('webdav.keepCount', value, setKeepCount)}
                  className="shrink-0 justify-end"
                />
              </div>
              {/* 「保留份數」本身說不清會不會刪東西。每次備份是一個新檔案、不覆蓋舊的，
                  所以必须明说超出的那些会被删掉 —— 用户拿这个数做决定，就得知道代价。 */}
              <p className="mt-1 text-[11px] text-muted-foreground">{t('webdav.keepHint')}</p>
            </div>
          </div>

          <div className="mt-5 flex items-center justify-end gap-2">
            {/* 測試結果和報錯共用左邊這一格，兩者不會同時有意義。 */}
            <span className={`min-w-0 flex-1 text-xs ${actionError ? 'text-destructive' : 'text-success-strong'}`}>
              {actionError || testMessage}
            </span>
            <Button
              variant="outline"
              size="sm"
              className="shrink-0 gap-1.5"
              onClick={() => void handleTest()}
              disabled={busy === 'test' || !configured}
            >
              {busy === 'test' && <Loader2 className="h-4 w-4 animate-spin" />}
              {t('webdav.test')}
            </Button>
            <Button size="sm" className="shrink-0" onClick={closeServerDialog}>
              {t('webdav.done')}
            </Button>
          </div>
        </Modal>
      )}

      {restoreList && (
        <Modal title={t('webdav.restoreTitle')} onClose={() => setRestoreList(null)} panelClassName="w-[460px]">
          {restoreList.length === 0 ? (
            <p className="text-sm text-muted-foreground">{t('webdav.restoreEmpty')}</p>
          ) : (
            <ul className="max-h-64 space-y-1 overflow-y-auto">
              {restoreList.map((entry) => (
                <li key={entry.name}>
                  <button
                    type="button"
                    onClick={() => setRestoreTarget(entry)}
                    className="flex w-full items-center justify-between gap-3 rounded-md px-3 py-2 text-left text-sm transition-colors hover:bg-accent"
                  >
                    <span className="min-w-0 truncate">{entry.name}</span>
                    <span className="shrink-0 text-xs text-muted-foreground">{formatBytes(entry.size)}</span>
                  </button>
                </li>
              ))}
            </ul>
          )}
          <div className="mt-5 flex justify-end">
            <Button variant="outline" size="sm" onClick={() => setRestoreList(null)}>
              {t('common.cancel')}
            </Button>
          </div>
        </Modal>
      )}

      {restoreTarget && (
        <Modal title={t('webdav.restoreConfirmTitle')} onClose={() => setRestoreTarget(null)} showCloseButton={false}>
          <p className="text-sm text-muted-foreground">
            {t('webdav.restoreConfirmBody', { name: restoreTarget.name })}
          </p>
          <div className="mt-5 flex justify-end gap-2">
            <Button variant="outline" size="sm" onClick={() => setRestoreTarget(null)}>
              {t('common.cancel')}
            </Button>
            <Button size="sm" onClick={() => void handleRestore()}>
              {t('webdav.restoreConfirmOk')}
            </Button>
          </div>
        </Modal>
      )}

      {restoreDone && (
        <Modal title={t('webdav.restoreDoneTitle')} onClose={() => undefined} locked showCloseButton={false}>
          <p className="text-sm text-muted-foreground">{t('webdav.restoreDoneBody')}</p>
        </Modal>
      )}
    </Card>
  )
}
