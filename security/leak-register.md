# 外流風險登錄表

- 基準版本：上游 commit `4fdeae9`（2026-11-09），client `0.2.5`；強化 commit `de5a362`（2026-11-19 05:44）
- 時間皆為 UTC+08:00；最後更新：2026-11-19 06:10
- 「發現時間」是掃描中第一次確認該風險的時間；「對應」是維護者本機調查記錄的項目編號（S1～S29）。
- 狀態說明與防線限制見 [README](README.md)。

## 外流類型

| 類型 | 說明 |
|---|---|
| IP／時段 | 連線本身會暴露 IP 與使用時間 |
| 裝置資訊 | 平台、CPU 架構、版本、作業系統使用者名稱、主機名稱、區網 IP、裝置 ID 等 |
| 遠端內容 | 從遠端接收內容並顯示在 App 中 |
| 二進位替換 | 下載並安裝不是自己建置的執行檔（供應鏈風險） |
| 語音 | 錄音資料 |
| 文字 | 轉錄文字、校對結果、提示詞 |
| 情境 | 視窗標題、App 名稱、處理程序名稱、游標附近文字 |
| 金鑰 | API 金鑰、WebDAV 帳號密碼 |
| 診斷 | 日誌、設定 |
| 本機留存 | 寫入磁碟、可能被備份 |
| 跨裝置 | 透過系統功能同步到其他裝置 |
| 外部網站 | 以系統瀏覽器開啟外部網址 |

## 一、已執行（待驗證）

處理於 commit `de5a362`（2026-11-19 05:44），尚未實際執行驗證。

| 編號 | 發現時間 | 外流類型 | 內容與目的地 | 觸發方式 | 處理方式 | 狀態 | 對應 |
|---|---|---|---|---|---|---|---|
| L-01 | 2026-11-19 05:15 | IP／時段、裝置資訊 | Google Fonts：`fonts.googleapis.com`、`fonts.gstatic.com`（請求帶 User-Agent） | 每次開啟視窗，自動 | 移除 `client/index.html` 的字型載入；CSP 移除字型網域。Nunito 字型退回系統字型 | 已執行（待驗證）；建置產物 0 處 | S2、S3 |
| L-02 | 2026-11-19 05:02 | IP／時段、遠端內容 | 啟動公告 `GET sayitapp.site/api/notice`（不帶參數，版本與語系在本機比對）；公告標題、內文、連結由遠端決定 | 首頁載入時與之後每 3 小時，自動 | `Home.tsx` 的 `NoticeBanner` 註解停用；`notice.ts` 的 `loadRaw()` 直接回傳 null；CSP | 已執行（待驗證） | S2、S15 |
| L-03 | 2026-11-19 05:16 | IP／時段、裝置資訊 | 更新檢查 `GET sayitapp.site/api/desktop-updates/darwin/arm64/manifest`（網址透露平台與 CPU 架構）；自設伺服器沒有 manifest 時改連官方 | 啟動時與之後每 6 小時，自動 | `App.tsx` 註解 `startUpdateService`；`autoUpdate.ts`、`updateChecker.ts` 函式內提前返回；`autoCheckUpdate` 預設 false；About 頁更新區塊註解；CSP | 已執行（待驗證） | S2、S15 |
| L-04 | 2026-11-19 05:20 | 二進位替換、IP／時段 | 背景下載官方安裝檔（Rust，User-Agent `SayIt/<版本>`）；SHA-512 雜湊值與安裝檔來自同一台伺服器 | 發現新版時，自動 | `download_update` 函式內一律拒絕；`download_update`、`verify_update_package`、`install_downloaded_update` 與 3 個 `update_notification` 指令自 `invoke_handler` 註解移除 | 已執行（待驗證） | S2、S15 |
| L-05 | 2026-11-19 05:20 | 二進位替換 | App 結束時靜默安裝已下載的官方安裝檔（macOS 也會執行），自建版本會被覆蓋 | 結束 App 時，自動 | `main.rs` 的 `install_pending_update_on_exit` 呼叫註解；函式內提前返回 | 已執行（待驗證） | S2、S15 |
| L-06 | 2026-11-19 05:02（裝置資訊細節 2026-11-19 05:58） | 語音、文字、裝置資訊 | **Server 模式（上游預設）**：錄音經 WebSocket 送到 `sayitapp.site`；每次開始錄音時，`client_meta` 附帶作業系統使用者名稱、主機名稱、區網 IP、裝置 ID、OS 版本、語系、CPU 核心數、記憶體 | 每次語音輸入，自動 | `workMode` 預設 local；介面移除 Server 選項；`transcription/index.ts` 一律使用 `LocalProvider`，不再 import `ServerProvider`（`websocket.ts` 已無人引用）；CSP 擋 `ws:`／`wss:` | 已執行（待驗證） | S1 |
| L-07 | 2026-11-19 05:02 | 語音、文字 | **Cloud API 模式**：錄音與文字直送雲端 ASR／AI 供應商（豆包、通義、OpenAI、Gemini、Groq、OpenRouter 等） | 選用此模式後每次語音輸入，自動 | 介面移除 Cloud API 選項；不再 import `CloudAPIProvider` | 已執行（待驗證）；Rust 指令仍註冊，見 L-18 | S1 |
| L-08 | 2026-11-19 05:02 | 文字、情境 | **AI 校對（上游預設開啟）**：轉錄文字、提示詞，以及開啟情境感知時的游標附近文字，送到雲端 AI | 每次語音輸入後，自動 | `aiEnabled` 預設 false；托盤選單讀不到設定時預設關閉 | 已執行（待驗證）；殘留風險見 L-21、L-22、L-24 | S1 |
| L-09 | 2026-11-19 05:15 | 文字、情境、裝置資訊 | 意見回饋 `POST /api/feedback`：可附最後一段轉錄文字、App 名稱、視窗標題、處理程序名稱、裝置 ID | 使用者點擊 | `submitFeedback` 直接回傳錯誤；CSP | 已執行（待驗證）；首頁回饋區塊的外部連結見 L-25 | S2 |
| L-10 | 2026-11-19 05:15 | 診斷 | 診斷上傳 `POST /api/diagnostics`（日誌與設定的 zip） | 使用者點擊 | `uploadDiagnosticsZip` 一律拋出錯誤；CSP | 已執行（待驗證）；zip 內容審查見 L-30 | S2 |
| L-11 | 2026-11-19 05:15 | 語音、文字、裝置資訊 | ASR 校正回報 `POST /api/asr-corrections`（錄音檔、文字、裝置 ID） | 使用者點擊 | `submitAsrCorrection` 一律拋出錯誤；CSP | 已執行（待驗證）；撤回路徑漏改，見 L-17 | S2 |
| L-12 | 2026-11-19 05:16 | IP／時段 | 設定精靈的 `GET /healthz`；匯出時的 `GET /api/hotwords` | 設定精靈（Server 模式）、匯出時 | `api.ts` 的 `fetchAPI` 一律拋出錯誤；CSP | 已執行（待驗證） | S2 |
| L-13 | 2026-11-19 05:15 | （防線缺口） | CSP 原為 `connect-src … https: http: wss: ws:`，介面可連任何網址；`img-src` 允許 `https:` | — | 收緊為 `connect-src 'self' ipc: http://ipc.localhost`、`img-src 'self' data: blob:`；已確認執行檔內嵌新 CSP | 已執行（待驗證）；**只管 WebView，擋不到 Rust 連線** | S3 |
| L-14 | 2026-11-19 05:02 | 本機留存 | 歷史記錄：轉錄文字與錄音預設**永久**保存在本機 | 每次語音輸入，自動 | `historyEnabled`、`audioRetentionEnabled` 預設 false（含各處讀取時的備用值） | 已執行（待驗證） | S6 |
| L-15 | 2026-11-19 05:15 | 跨裝置 | macOS 通用剪貼板（接力）可能把貼上用的文字同步到同一 Apple ID 的其他裝置 | 系統行為 | 系統設定關閉接力；2026-11-19 05:33 檢查 `ActivityAdvertisingAllowed = 0`、`ActivityReceivingAllowed = 0` | 已執行（系統設定，已檢查） | S5 |
| L-16 | 2026-11-19 05:02 | 文字 | DeepSeek：AI 校對可設定 `api.deepseek.com` | AI 校對開啟時 | Rust `ai_openai_compat.rs` 新增 `is_forbidden_ai_host()`，`polish` 與 `test_connection` 遇到網址含 `deepseek` 一律拒絕 | **部分處理**；缺口見 L-23 | S7 |

## 二、待處理（子代理掃描後新發現與既有建議）

| 編號 | 發現時間 | 外流類型 | 內容與目的地 | 觸發方式 | 建議處理方式 | 狀態 | 對應 |
|---|---|---|---|---|---|---|---|
| L-17 | 2026-11-19 05:51 | 裝置資訊、IP／時段 | 撤回 ASR 校正 `POST /api/asr-corrections/{id}/withdraw`，body 帶 `machine_id`（裝置 ID）。**漏加停用守衛**，是目前唯一仍可達且帶裝置 ID 的請求 | 使用者點擊（只有舊 Server 模式記錄才有入口） | 加入與 `submitAsrCorrection` 相同的停用守衛 | 待處理（正式版 CSP 會擋；開發模式沒有 CSP） | S16 |
| L-18 | 2026-11-19 05:49 | 語音、文字、金鑰 | Rust 仍註冊雲端指令：`cloud_transcribe`、`cloud_transcribe_raw`、`cloud_polish`、`test_ai_connection`、`test_asr_connection`，以及豆包、通義、OpenAI、Gemini 即時串流的 `*_open／send／finish／close`。前端已沒有呼叫路徑，但這些連線在 Rust 執行，**不受 CSP 限制**（另：OpenRouter 請求帶 `Referer: https://sayitapp.site`） | 需前端呼叫 | 從 `main.rs` 的 `invoke_handler` 註解移除，日後連同程式本體刪除；本機 Ollama 屆時單獨開放 | 待處理 | S17、S13 |
| L-19 | 2026-11-19 05:51 | 診斷（設定）；可選：文字、語音 | WebDAV 自動備份：啟動 2 分鐘後首次檢查、之後每 15 分鐘檢查，預設每 24 小時備份一次；經 Rust 上傳到使用者設定的 WebDAV，不受 `workMode` 與 CSP 限制 | 啟用後自動（**預設關閉**） | 註解 `App.tsx` 的 `startWebDavBackupService` 與設定頁入口 | 待處理 | S18 |
| L-20 | 2026-11-19 05:51 | 金鑰、IP／時段 | 診斷頁開啟時自動執行 AI 連線測試，把 API 金鑰與固定測試提示詞送到設定的 AI 端點 | 開啟診斷頁且 AI 開啟時，自動 | 移除自動測試 | 待處理 | S19 |
| L-21 | 2026-11-19 05:51 | 金鑰、IP／時段 | AI 服務頁的「檢查連線」按鈕，不論 AI 校對是否開啟都會送出 API 金鑰與固定提示詞 | 使用者點擊 | 停用，或只允許本機 Ollama（127.0.0.1） | 待處理 | S20、S14 |
| L-22 | 2026-11-19 05:51 | 文字、情境 | 托盤選單與快捷鍵（`shortcutToggleAi`）可隨時重新開啟 AI 校對；若已設定雲端 AI，之後每次語音輸入都會送出文字 | 使用者操作（可能誤觸） | 移除托盤的 AI 開關與快捷鍵，或只允許本機 Ollama | 待處理 | S21 |
| L-23 | 2026-11-19 05:49 | 文字 | DeepSeek 封鎖的缺口：①只檢查設定的網址，HTTP 轉址到 DeepSeek 仍會跟隨；②用 IP 或中繼網域連線時無法辨識；③`registry.rs` 仍接受 provider 名稱 `deepseek`；④ASR 的自訂 baseUrl 沒有檢查 | AI 校對或連線測試時 | AI 校對改為白名單：只允許 Ollama＋loopback 位址；禁止轉址；移除 `deepseek` provider | 待處理 | S22、S7 |
| L-24 | 2026-11-19 05:51 | 文字（潛在） | `aiPolicy.ts` 的 `policyFromSnapshot` 缺少設定時預設 `aiEnabled ?? true`；目前所有呼叫端都有傳設定，尚未觸發 | 程式缺口 | 預設改為 false | 待處理 | S23 |
| L-25 | 2026-11-19 05:51 | 外部網站 | 點擊後用系統瀏覽器開啟外部網址：首頁回饋區塊開啟 `sayitapp.site/feedback.html`；About 頁 GitHub 連結；AI 服務頁供應商主控台連結；外觀頁火山引擎連結 | 使用者點擊 | 移除回饋區塊與作者網站連結；其餘連結待決定 | 待處理 | S24 |
| L-26 | 2026-11-19 05:58 | IP／時段 | 首次下載本地模型：預設來源 `hf-mirror.com`（第三方鏡像），另一來源為官方 `huggingface.co`；模型檔在作者的 HF 倉庫 `cswk/sayit-asr-gguf`。**完整性有保護**：每個檔案的 SHA-256 與大小都寫死在 `catalog.rs`，下載後比對，不符即刪除並失敗 | 使用者點擊下載（一次性） | 預設改為官方 `huggingface.co`；下載完成後用 LuLu 封鎖 | 待處理（需決定來源） | S25、S4 |
| L-27 | 2026-11-19 05:17 | 跨裝置、本機留存 | 游標位置無法輸入時（`paste.rs` 的 `not_editable` 分支），辨識結果留在剪貼簿且不會還原 | 自動 | 一段時間後清除剪貼簿，或改為不寫入剪貼簿 | 待處理 | S11 |
| L-28 | 2026-11-19 05:51 | （防線缺口） | 開發模式 `devCsp: null`：執行 `tauri dev` 時沒有 CSP | 僅開發時 | 設定與正式版相同的 `devCsp` | 待處理 | S26 |
| L-29 | 2026-11-19 05:51 | 外部網站 | `plugins.shell.open: true`，未限制可開啟的網址範圍（推測 Tauri 預設只允許 http、https、mailto、tel，尚未查證） | 使用者點擊 | 查證後限制範圍或移除 | 待處理（未查證） | S27 |
| L-30 | 2026-11-19 05:52 | 本機留存、診斷 | 日誌與本機留存：日誌保留 30 天、診斷 zip、設定檔、暫存音檔等，是否含文字、視窗標題、使用者名稱 | — | 掃描完成後決定 | 審查中 | S28 |
| L-31 | 2026-11-19 05:15 | 本機留存 | Time Machine／iCloud 可能備份 App 資料夾（模型、設定、日誌） | 系統行為 | Time Machine 排除該資料夾 | 待處理（使用者操作） | S12 |

## 三、僅記錄（分析後沒有實際外流，或風險極低）

| 編號 | 發現時間 | 內容 | 結論 |
|---|---|---|---|
| L-32 | 2026-11-19 05:49 | `get_local_ip` 以 UDP `connect` 到 `8.8.8.8:80` 取得區網 IP | UDP connect 只查路由，**不送出封包**。結果只用在 Server 模式的 `client_meta`（已不可達，見 L-06）。啟用 LuLu 後再確認 |
| L-33 | 2026-11-19 05:49 | 內建 C++ 推論引擎（ggml）含 `ggml-rpc` 網路程式碼 | 建置預設關閉（`GGML_RPC=OFF`），不會編入 |
| L-34 | 2026-11-19 05:49 | 遙測、當機回報、分析套件 | Rust 的 Cargo.toml／Cargo.lock 未發現 sentry、posthog、mixpanel、amplitude、segment、opentelemetry、bugsnag、tauri-plugin-updater 等；前端 `package.json` 與 `src` 也未發現 |
| L-35 | 2026-11-19 05:58 | 裝置 ID：`sayit-<隨機 UUID v4>`，非硬體衍生，存在設定檔 | WebDAV 備份與設定匯出會排除它；目前唯一仍可達且帶裝置 ID 的請求是 L-17 |
