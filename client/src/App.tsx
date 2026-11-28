import { useEffect, useState } from 'react'
import { Routes, Route, Navigate, useNavigate } from 'react-router-dom'
import { listen } from '@tauri-apps/api/event'
import Sidebar from './components/Sidebar'
import TitleBar from './components/TitleBar'
import WelcomeGuide from './components/WelcomeGuide'
import Home from './pages/Home'
import History from './pages/History'
import Dictionary from './pages/Dictionary'
import Settings from './pages/Settings'
import VoiceEnginePage from './features/settings/VoiceEnginePage'
import AIServicePage from './features/settings/AIServicePage'
import AIInstructionsPage from './features/settings/AIInstructionsPage'
import About from './pages/About'
import { initRecorder, cleanup } from './services/recorder'
import { initTheme } from './stores/theme'
import { initAiEnabled } from './stores/aiEnabled'
import { initActivePreset } from './stores/activePreset'
import { startClickGlow } from './stores/clickGlow'
import { getSetting, setSetting } from './services/store'
// [自訂] 更新已註解停用：import { startUpdateService } from './features/update/autoUpdate'
// [自訂] S18／L-19 WebDAV 自動備份已註解停用：import { startWebDavBackupService } from './features/backup/autoWebdavBackup'
// [自訂] 更新已註解停用：import UpdateDialog from './features/update/UpdateDialog'
// [自訂] 更新已註解停用：import UpdateNotificationHost from './features/update/UpdateNotificationHost'
import * as bridge from './services/bridge'

export default function App() {
  const [showWelcome, setShowWelcome] = useState(false)
  // 首次掛載時先不渲染主介面，等 onboarding 檢查完成再決定，避免"閃一下主頁再進嚮導"
  const [onboardingChecked, setOnboardingChecked] = useState(false)
  const navigate = useNavigate()

  // 初始化 + 清理：必須只在掛載/解除安裝各執行一次。
  // ⚠️ 依賴陣列務必保持為空 []！這裡的 cleanup() 會斷開 WebSocket，若把會變化的
  // 值（如 react-router 的 navigate）放進依賴，effect 會反覆解除安裝重灌，導致
  // 「連上→cleanup 斷連→重連」的死迴圈（連上幾秒就斷、從不發 start）。
  useEffect(() => {
    void initTheme()
    void initAiEnabled()
    void initActivePreset()
    initRecorder()
    // [自訂] 更新已註解停用：void startUpdateService()
    // [自訂] S18／L-19 WebDAV 自動備份已註解停用：void startWebDavBackupService()

      // 檢查是否需要顯示歡迎向導（僅首次安裝）
      ; (async () => {
        const onboardedVersion = await getSetting('onboardingVersion', '')
        if (!onboardedVersion) {
          setShowWelcome(true)
        }
        setOnboardingChecked(true)
      })()

    return () => {
      cleanup()
    }
  }, [])

  // [自訂] SC-63：點擊光輝只裝在主視窗；錄音懸浮窗是另一個入口，不載入。
  useEffect(() => startClickGlow(), [])

  // 自動更新安裝完成後，看門人程序會帶 --open-about 重新拉起本程式，
  // Rust 端據此發出 open-about 事件，這裡跳轉到關於頁方便使用者確認更新已生效。
  // 單獨一個 effect：它的清理只是取消監聽，不會斷開連線，所以依賴 navigate 無副作用。
  useEffect(() => {
    const unlistenOpenAbout = listen('open-about', () => {
      navigate('/about')
    })
    // macOS 應用選單的「設定…」（⌘,）；Windows 不會發這個事件
    const unlistenOpenSettings = listen('open-settings', () => {
      navigate('/settings')
    })
    return () => {
      void unlistenOpenAbout.then((fn) => fn())
      void unlistenOpenSettings.then((fn) => fn())
    }
  }, [navigate])

  const handleWelcomeComplete = () => {
    setShowWelcome(false)
    void setSetting('onboardingVersion', __APP_VERSION__)
    // 嚮導完成後通知 Rust 端重新載入快捷鍵配置
    bridge.notifyShortcutsChanged()
  }

  // onboarding 檢查未完成前只渲染與背景同色的空殼，避免首次安裝時主頁閃現
  if (!onboardingChecked) {
    return <div className="h-screen bg-background" />
  }

  return (
    <div className="flex h-screen flex-col">
      <TitleBar />
      <div className="flex flex-1 overflow-hidden">
        <Sidebar />
        <main className="custom-scrollbar flex-1 overflow-y-auto bg-background p-8">
          <Routes>
            <Route path="/" element={<Home />} />
            <Route path="/history" element={<History />} />
            <Route path="/favorites" element={<Navigate to="/history" replace />} />
            <Route path="/hotwords" element={<Dictionary />} />
            <Route path="/dictionary" element={<Navigate to="/hotwords" replace />} />
            <Route path="/voice-engine" element={<VoiceEnginePage />} />
            <Route path="/ai-instructions" element={<AIInstructionsPage />} />
            <Route path="/ai-service" element={<AIServicePage />} />
            <Route path="/settings" element={<Settings />} />
            <Route path="/about" element={<About />} />
          </Routes>
        </main>
      </div>
      {showWelcome && <WelcomeGuide onComplete={handleWelcomeComplete} />}
      {/* [自訂] 更新已註解停用：<UpdateDialog /> */}
      {/* [自訂] 更新已註解停用：<UpdateNotificationHost enabled=(!showWelcome) /> */}
    </div>
  )
}
