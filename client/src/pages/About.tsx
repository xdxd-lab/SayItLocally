import { useEffect, useState } from 'react'
import { Card, CardContent } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Github, Loader2, CheckCircle } from 'lucide-react'
import { open as shellOpen } from '@tauri-apps/plugin-shell'
// [自訂] 更新功能已註解停用（日後移除）：
// import { getAutoUpdateState, onAutoUpdateChange, checkForUpdateNow, installPendingUpdate, type AutoUpdateState } from '@/features/update/autoUpdate'
import { RELEASE_HIGHLIGHTS } from '@/features/update/releaseHighlights'
import appIcon from '@/assets/icon-128.png'
import { getLocale, t } from '@/i18n'
import { useT } from '@/i18n/useT'
import BrandBanner from '@/components/BrandBanner'

const currentVersion = __APP_VERSION__

const REPO_URL = 'https://github.com/crosswk/SayIt'
const RELEASES_URL = `${REPO_URL}/releases`

function formatTimestamp(value: number | null | undefined) {
  if (!value) return t('about.neverChecked')
  // 時間格式跟介面語言：中文介面用 zh-TW 的寫法，英文介面用 en 的。
  return new Date(value).toLocaleString(getLocale(), { hour12: false })
}

export default function About() {
  const t = useT()
// [自訂] 更新功能已註解停用（日後移除）：
  //   const [state, setState] = useState<AutoUpdateState>(getAutoUpdateState)

  //   useEffect(() => {
  //     return onAutoUpdateChange(setState)
  //   }, [])

  //   const { phase, versionInfo, checkedAt, error, pending } = state
  //   const checking = phase === 'checking'
  //   const downloading = phase === 'downloading'
  //   const installing = phase === 'installing'
  //   // 「有包等著裝」與 phase 正交：後台每 6 小時會跑一次檢查，那期間 phase 是 'checking'，
  //   // 但安裝按鈕該一直在。別把這個條件寫成 phase === 某個值（那正是卡住過的寫法）。
  //   const ready = !!pending && !installing
  //   const hasUpdate = !!versionInfo?.hasUpdate

  //   const updateStatusText = (() => {
  //     if (installing) return t('about.installing')
  //     // ready 排在檢查/下載之前：待安裝的包可能是上次執行下載的，此時 versionInfo 還沒回來；
  //     // 而且後台週期檢查不該把"已經下載好了"這條更重要的資訊擠掉。
  //     if (ready) return t('about.downloaded', { version: String(pending?.version) })
  //     if (checking) return t('about.checking')
  //     if (!versionInfo) return null
  //     if (versionInfo.error) return t('about.checkFailed')
  //     // String() 而不是 ?? ''：保持与改造前 `${...}` 完全一致的输出，
  //     // 這輪只做翻譯，不順手改 latestVersion 為空時的表現。
  //     if (downloading) return t('about.downloading', { version: String(versionInfo.latestVersion), percent: Math.round(state.downloadPercent ?? 0) })
  //     if (hasUpdate) return t('about.updateAvailable', { version: String(versionInfo.latestVersion) })
  //     return t('about.upToDate')
  //   })()

  return (
    <div className="mx-auto max-w-4xl">
      <h1 className="mb-4 text-2xl font-bold">{t('about.title')}</h1>

      {/* [自訂] SC-63：關於頁放完整訴求與 fork 說明 */}
      <BrandBanner appeal="none" />

      <Card>
        <CardContent className="p-6">
          {/* 品牌 */}
          <div className="flex items-center gap-4">
            <img src={appIcon} alt="SayItLocally" className="h-16 w-16 rounded-2xl" />
            <div>
              <h2 className="text-2xl font-bold tracking-tight" style={{ fontFamily: "'Nunito', sans-serif", fontWeight: 800 }}>
                SayItLocally
              </h2>
              <p className="text-sm text-muted-foreground">{t('about.tagline')}</p>
              <p className="mt-0.5 text-xs text-muted-foreground/60">by Liu Qianglong & Claude</p>
              <div className="mt-1.5 flex items-center gap-2">
                {/* 版本號做成和右邊 GitHub 圖示同一種藥丸按鈕：點開 releases 頁，
                    想看历史版本或某一版改了什么的人第一反应就是点这里的版本号。 */}
                <button
                  type="button"
                  onClick={() => void shellOpen(RELEASES_URL)}
                  className="flex h-6 items-center rounded-full bg-muted/50 px-2.5 text-xs text-muted-foreground transition-colors hover:bg-foreground/15 hover:text-foreground"
                  title={t('about.viewReleases')}
                  aria-label={t('about.viewReleases')}
                >
                  v{currentVersion}
                </button>
                <button
                  type="button"
                  onClick={() => void shellOpen(REPO_URL)}
                  className="flex h-6 w-6 items-center justify-center rounded-full bg-muted/50 text-muted-foreground transition-colors hover:bg-foreground/15 hover:text-foreground"
                  title="GitHub"
                  aria-label="GitHub"
                >
                  <Github className="h-3.5 w-3.5" aria-hidden />
                </button>
              </div>
            </div>
          </div>

          <div className="mt-6 space-y-2 border-t border-border pt-5">
            <p className="text-sm font-medium">{t('brand.appealFull')}</p>
            <p className="text-xs text-muted-foreground">{t('brand.forkNote')}</p>
          </div>

          {/* [自訂] 更新區塊已註解停用，原始 JSX 保留在本檔末尾 */}

          {/* 本次更新 */}
          {RELEASE_HIGHLIGHTS.version === currentVersion && RELEASE_HIGHLIGHTS.items.length > 0 && (
            <div className="mt-6">
              <h3 className="mb-3 text-sm font-medium">{t('about.releaseNotes', { version: RELEASE_HIGHLIGHTS.version })}</h3>
              <ul className="space-y-1.5">
                {RELEASE_HIGHLIGHTS.items.map((item, index) => (
                  <li key={index} className="flex gap-2 text-sm text-muted-foreground">
                    <span className="mt-2 h-1 w-1 shrink-0 rounded-full bg-muted-foreground/50" />
                    <span>{item}</span>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  )
}


// ===== [自訂] 以下為原「更新」區塊 JSX，已停用，日後移除 =====
//           {/* 更新 */}
//           <div className="mt-5 border-t border-border pt-5">
//             <h3 className="mb-3 text-sm font-medium">{t('about.updateSection')}</h3>
//             <div className="flex items-center justify-between">
//               <div className="space-y-0.5">
//                 {updateStatusText && (
//                   <p className={`text-sm ${hasUpdate || ready ? 'text-primary font-medium' : 'text-muted-foreground'}`}>
//                     {updateStatusText}
//                   </p>
//                 )}
//                 {error && (
//                   <p className="text-xs text-red-500">{error}</p>
//                 )}
//                 {/* 待安裝時補一句"不裝也會裝上"：使用者在這一頁才有機會知道退出兜底那條路 */}
//                 {ready && (
//                   <p className="text-xs text-muted-foreground/60">{t('about.readyHint')}</p>
//                 )}
//                 {checkedAt && (
//                   <p className="text-xs text-muted-foreground/60">{t('about.lastChecked', { time: formatTimestamp(checkedAt) })}</p>
//                 )}
//               </div>
//               <div className="flex items-center gap-2">
//                 {/* 點了就直接裝，不再疊一層確認框：使用者是被側欄那枚變綠的圖示引到這一頁、
//                     看完狀態說明之後才按下這個按鈕的，「立即安裝」四個字本身就是確認。
//                     上方 about.readyHint 已經說過"不裝的話關閉應用時也會自動完成"。 */}
//                 {ready && (
//                   <Button size="sm" onClick={() => void installPendingUpdate()}>
//                     <CheckCircle className="mr-1.5 h-3.5 w-3.5" aria-hidden />
//                     {t('about.installNow')}
//                   </Button>
//                 )}
//                 {/* 下載中。沒有單獨的「下載更新」按鈕了：發現新版就自動下載，
//                     下載失敗時按「檢查更新」會重來一遍。 */}
//                 {downloading && (
//                   <Button size="sm" disabled>
//                     <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" />
//                     {t('about.downloadingShort')}
//                   </Button>
//                 )}
//                 {installing && (
//                   <Button size="sm" disabled>
//                     <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" />
//                     {t('about.installingShort')}
//                   </Button>
//                 )}
//                 {!downloading && !installing && !ready && (
//                   <Button variant="outline" size="sm" onClick={() => void checkForUpdateNow()} disabled={checking}>
//                     {checking ? t('about.checkingShort') : t('about.checkUpdate')}
//                   </Button>
//                 )}
//               </div>
//             </div>
//           </div>
