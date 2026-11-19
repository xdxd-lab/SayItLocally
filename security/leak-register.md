# 外流風險登錄表

- 基準版本：上游 commit `4fdeae9`（2026-11-09），client `0.2.5`；強化 commit `de5a362`（2026-11-19 05:44）
- 時間皆為 UTC+08:00；最後更新：2026-11-20 00:12
- 「發現時間」是掃描中第一次確認該風險的時間；「對應」是維護者本機調查記錄的項目編號（S1～S52）。
- 第十次修正（2026-11-20 00:03，分支 `security/first-launch-hardening`，基準 `7f79001`）：處理 L-16～L-28 與新增的 L-36 起各項；commit 見本次 PR／git 歷史。**App 尚未執行。**
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
| L-07 | 2026-11-19 05:02 | 語音、文字 | **Cloud API 模式**：錄音與文字直送雲端 ASR／AI 供應商（豆包、通義、OpenAI、Gemini、Groq、OpenRouter 等） | 選用此模式後每次語音輸入，自動 | 介面移除 Cloud API 選項；不再 import `CloudAPIProvider` | 已執行（待驗證）；雲端 ASR 與串流指令已自 `invoke_handler` 註銷（2026-11-20 00:03，見 L-18） | S1 |
| L-08 | 2026-11-19 05:02 | 文字、情境 | **AI 校對（上游預設開啟）**：轉錄文字、提示詞，以及開啟情境感知時的游標附近文字，送到雲端 AI | 每次語音輸入後，自動 | `aiEnabled` 預設 false；托盤選單讀不到設定時預設關閉 | 已執行（待驗證）；L-21、L-22、L-24 已於 2026-11-20 00:03 處理（Rust 端只允許本機 Ollama） | S1 |
| L-09 | 2026-11-19 05:15 | 文字、情境、裝置資訊 | 意見回饋 `POST /api/feedback`：可附最後一段轉錄文字、App 名稱、視窗標題、處理程序名稱、裝置 ID | 使用者點擊 | `submitFeedback` 直接回傳錯誤；CSP | 已執行（待驗證）；首頁回饋區塊的外部連結見 L-25 | S2 |
| L-10 | 2026-11-19 05:15 | 診斷 | 診斷上傳 `POST /api/diagnostics`（日誌與設定的 zip） | 使用者點擊 | `uploadDiagnosticsZip` 一律拋出錯誤；CSP | 已執行（待驗證）；zip 內容審查見 L-30 | S2 |
| L-11 | 2026-11-19 05:15 | 語音、文字、裝置資訊 | ASR 校正回報 `POST /api/asr-corrections`（錄音檔、文字、裝置 ID） | 使用者點擊 | `submitAsrCorrection` 一律拋出錯誤；CSP | 已執行（待驗證）；撤回路徑漏改，見 L-17 | S2 |
| L-12 | 2026-11-19 05:16 | IP／時段 | 設定精靈的 `GET /healthz`；匯出時的 `GET /api/hotwords` | 設定精靈（Server 模式）、匯出時 | `api.ts` 的 `fetchAPI` 一律拋出錯誤；CSP | 已執行（待驗證） | S2 |
| L-13 | 2026-11-19 05:15 | （防線缺口） | CSP 原為 `connect-src … https: http: wss: ws:`，介面可連任何網址；`img-src` 允許 `https:` | — | 收緊為 `connect-src 'self' ipc: http://ipc.localhost`、`img-src 'self' data: blob:`；已確認執行檔內嵌新 CSP | 已執行（待驗證）；**只管 WebView，擋不到 Rust 連線** | S3 |
| L-14 | 2026-11-19 05:02 | 本機留存 | 歷史記錄：轉錄文字與錄音預設**永久**保存在本機 | 每次語音輸入，自動 | `historyEnabled`、`audioRetentionEnabled` 預設 false（含各處讀取時的備用值） | 已執行（待驗證） | S6 |
| L-15 | 2026-11-19 05:15 | 跨裝置 | macOS 通用剪貼板（接力）可能把貼上用的文字同步到同一 Apple ID 的其他裝置 | 系統行為 | 系統設定關閉接力；2026-11-19 05:33 檢查 `ActivityAdvertisingAllowed = 0`、`ActivityReceivingAllowed = 0` | 已執行（系統設定，已檢查） | S5 |
| L-16 | 2026-11-19 05:02 | 文字 | DeepSeek：AI 校對可設定 `api.deepseek.com` | AI 校對開啟時 | Rust `ai_openai_compat.rs` 新增 `is_forbidden_ai_host()`；2026-11-20 00:03 起改為白名單：AI 只允許本機 Ollama（見 L-23、L-40），黑名單保留為額外防線 | 已執行（待驗證）（2026-11-20 00:03） | S7 |

## 二、子代理掃描發現（L-17～L-31）

多數已於 2026-11-20 00:03（第十次修正）處理，尚未實際執行驗證。

| 編號 | 發現時間 | 外流類型 | 內容與目的地 | 觸發方式 | 處理方式 | 狀態 | 對應 |
|---|---|---|---|---|---|---|---|
| L-17 | 2026-11-19 05:51 | 裝置資訊、IP／時段 | 撤回 ASR 校正 `POST /api/asr-corrections/{id}/withdraw`，body 帶 `machine_id`（裝置 ID）。**漏加停用守衛**，是目前唯一仍可達且帶裝置 ID 的請求 | 使用者點擊（只有舊 Server 模式記錄才有入口） | `withdrawAsrCorrection` 加上相同停用守衛（拋錯，不送出）；對話框加 `catch`（L-49） | 已執行（待驗證）（2026-11-20 00:03） | S16 |
| L-18 | 2026-11-19 05:49 | 語音、文字、金鑰 | Rust 仍註冊雲端指令：`cloud_transcribe`、`cloud_transcribe_raw`、`cloud_polish`、`test_ai_connection`、`test_asr_connection`，以及豆包、通義、OpenAI、Gemini 即時串流的 `*_open／send／finish／close`。前端已沒有呼叫路徑，但這些連線在 Rust 執行，**不受 CSP 限制**（另：OpenRouter 請求帶 `Referer: https://sayitapp.site`） | 需前端呼叫 | 自 `invoke_handler` 註解移除 `cloud_transcribe`、`cloud_transcribe_raw`、`test_asr_connection` 與 20 個串流指令；`cloud_polish`、`test_ai_connection` 保留但 Rust 只允許本機 Ollama（L-40）。程式本體日後刪除（S13） | 已執行（待驗證）（2026-11-20 00:03） | S17、S13 |
| L-19 | 2026-11-19 05:51 | 診斷（設定）；可選：文字、語音 | WebDAV 自動備份：啟動 2 分鐘後首次檢查、之後每 15 分鐘檢查，預設每 24 小時備份一次；經 Rust 上傳到使用者設定的 WebDAV，不受 `workMode` 與 CSP 限制 | 啟用後自動（**預設關閉**） | 註解 `App.tsx` 的 `startWebDavBackupService`、設定頁 `WebDavSection`；4 個 `webdav_*` 指令自 `invoke_handler` 註銷 | 已執行（待驗證）（2026-11-20 00:03） | S18 |
| L-20 | 2026-11-19 05:51 | 金鑰、IP／時段 | 診斷頁開啟時自動執行 AI 連線測試，把 API 金鑰與固定測試提示詞送到設定的 AI 端點 | 開啟診斷頁且 AI 開啟時，自動 | 診斷頁不再自動測試 AI，只顯示已設定的供應商與模型 | 已執行（待驗證）（2026-11-20 00:03） | S19 |
| L-21 | 2026-11-19 05:51 | 金鑰、IP／時段 | AI 服務頁的「檢查連線」按鈕，不論 AI 校對是否開啟都會送出 API 金鑰與固定提示詞 | 使用者點擊 | Rust `test_ai_connection` 只允許 provider `ollama`＋位址 127.0.0.1／localhost／::1，其餘直接拒絕、不連線 | 已執行（待驗證）（2026-11-20 00:03） | S20、S14 |
| L-22 | 2026-11-19 05:51 | 文字、情境 | 托盤選單與快捷鍵（`shortcutToggleAi`）可隨時重新開啟 AI 校對；若已設定雲端 AI，之後每次語音輸入都會送出文字 | 使用者操作（可能誤觸） | 開關保留；AI 校對在 Rust 端只能連本機 Ollama，開啟後也不會送到雲端 | 已執行（待驗證）（2026-11-20 00:03） | S21 |
| L-23 | 2026-11-19 05:49 | 文字 | DeepSeek 封鎖的缺口：①只檢查設定的網址，HTTP 轉址到 DeepSeek 仍會跟隨；②用 IP 或中繼網域連線時無法辨識；③`registry.rs` 仍接受 provider 名稱 `deepseek`；④ASR 的自訂 baseUrl 沒有檢查 | AI 校對或連線測試時 | 白名單：只允許 Ollama＋loopback（以解析後主機判斷）；Ollama 用不跟隨轉址、不走代理的專用客戶端；`deepseek` 等雲端分支註解停用；ASR 自訂 baseUrl 所在的雲端 ASR 指令已註銷 | 已執行（待驗證）（2026-11-20 00:03） | S22、S7 |
| L-24 | 2026-11-19 05:51 | 文字（潛在） | `aiPolicy.ts` 的 `policyFromSnapshot` 缺少設定時預設 `aiEnabled ?? true`；目前所有呼叫端都有傳設定，尚未觸發 | 程式缺口 | 預設改為 `aiEnabled ?? false` | 已執行（待驗證）（2026-11-20 00:03） | S23 |
| L-25 | 2026-11-19 05:51 | 外部網站 | 點擊後用系統瀏覽器開啟外部網址：首頁回饋區塊開啟 `sayitapp.site/feedback.html`；About 頁 GitHub 連結；AI 服務頁供應商主控台連結；外觀頁火山引擎連結 | 使用者點擊 | 首頁回饋區塊註解停用。刻意保留：About 頁上游 GitHub 連結（來源與著作權標示）；供應商主控台與火山引擎連結維持（只在點擊時開瀏覽器） | 已執行（待驗證）（2026-11-20 00:03） | S24 |
| L-26 | 2026-11-19 05:58 | IP／時段 | 首次下載本地模型：預設來源 `hf-mirror.com`（第三方鏡像），另一來源為官方 `huggingface.co`；模型檔在作者的 HF 倉庫 `cswk/sayit-asr-gguf`。**完整性有保護**：每個檔案的 SHA-256 與大小都寫死在 `catalog.rs`，下載後比對，不符即刪除並失敗 | 使用者點擊下載（一次性） | 預設一律官方 `huggingface.co`，不論介面語言（`defaults.ts`、`language.ts`、`LocalModeSection.tsx`）；Rust 端來源名稱對不上時也先回落官方。下載後用 LuLu 封鎖仍待使用者操作 | 已執行（待驗證）（2026-11-20 00:03） | S25、S4 |
| L-27 | 2026-11-19 05:17 | 跨裝置、本機留存 | 游標位置無法輸入時（`paste.rs` 的 `not_editable` 分支），辨識結果留在剪貼簿且不會還原 | 自動 | 寫入改為暫存（transient）＋只限本機；寫入後 60 秒內剪貼簿未被改過就清空（見 L-39） | 已執行（待驗證）（2026-11-20 00:03） | S11 |
| L-28 | 2026-11-19 05:51 | （防線缺口） | 開發模式 `devCsp: null`：執行 `tauri dev` 時沒有 CSP | 僅開發時 | `devCsp` 設為與 `csp` 相同 | 已執行（待驗證）（2026-11-20 00:03） | S26 |
| L-29 | 2026-11-19 05:51 | 外部網站 | `plugins.shell.open: true`，未限制可開啟的網址範圍 | 使用者點擊 | 查證（2026-11-20 00:03）：`tauri-plugin-shell` 2.3.5 預設正規式 `^((mailto:\w+)\|(tel:\w+)\|(https?://\w+)).+`，只允許 http、https、mailto、tel，file:// 與自訂協定會被拒；前端只有主視窗有 `shell:allow-open`。不成立 | 僅記錄（2026-11-20 00:03） | S27 |
| L-30 | 2026-11-19 05:52 | 本機留存、診斷 | 日誌與本機留存：日誌保留 30 天、診斷 zip、設定檔、暫存音檔等，是否含文字、視窗標題、使用者名稱 | — | 審查完成（SC-12 2026-11-19 06:01，SC-13 複核 11:04）；結果拆為 L-37～L-55 各項（日誌：L-37、L-38、L-46、L-47；匯出與備份：L-41、L-42、L-50～L-52；其他：L-55） | 已完成（2026-11-20 00:03） | S28 |
| L-31 | 2026-11-19 05:15 | 本機留存 | Time Machine／iCloud 可能備份 App 資料夾（模型、設定、日誌） | 系統行為 | Time Machine 排除該資料夾 | 待處理（使用者操作） | S12 |

## 三、僅記錄（分析後沒有實際外流，或風險極低）

| 編號 | 發現時間 | 內容 | 結論 |
|---|---|---|---|
| L-32 | 2026-11-19 05:49 | `get_local_ip` 以 UDP `connect` 到 `8.8.8.8:80` 取得區網 IP | UDP connect 只查路由，**不送出封包**。結果只用在 Server 模式的 `client_meta`（已不可達，見 L-06）。啟用 LuLu 後再確認 |
| L-33 | 2026-11-19 05:49 | 內建 C++ 推論引擎（ggml）含 `ggml-rpc` 網路程式碼 | 建置預設關閉（`GGML_RPC=OFF`），不會編入 |
| L-34 | 2026-11-19 05:49 | 遙測、當機回報、分析套件 | Rust 的 Cargo.toml／Cargo.lock 未發現 sentry、posthog、mixpanel、amplitude、segment、opentelemetry、bugsnag、tauri-plugin-updater 等；前端 `package.json` 與 `src` 也未發現 |
| L-35 | 2026-11-19 05:58 | 裝置 ID：`sayit-<隨機 UUID v4>`，非硬體衍生，存在設定檔 | WebDAV 備份與設定匯出會排除它；目前唯一仍可達且帶裝置 ID 的請求是 L-17 |

## 四、第十次新增（L-36～L-55，對應 S30～S49）

- 來源：SC-12（日誌與本機留存，完成 2026-11-19 06:01）與 SC-13（Opus 深度複核，完成 2026-11-19 11:04）。
- 「發現時間」取該掃描的**完成時間**：日誌與本機留存類（L-37、L-38、L-46、L-47、L-50～L-52、L-55）記 SC-12 06:01；其餘記 SC-13 11:04。L-53、L-54 的發現時間未記錄，填登錄時間。
- 處理於 2026-11-20 00:03（第十次修正），**尚未實際執行 App**。

| 編號 | 發現時間 | 外流類型 | 內容與目的地 | 觸發方式 | 處理方式 | 狀態 | 對應 |
|---|---|---|---|---|---|---|---|
| L-36 | 2026-11-19 11:04 | 本機留存（系統變更） | 第一次啟動即自動註冊「登入時啟動」（LaunchAgent，隱藏啟動），未經同意；種子值 `autoLaunch=true` | 第一次啟動，自動 | 不再自動呼叫 `autostart.enable()`；種子值改 `false`；使用者可在設定手動開啟 | 已執行（待驗證）（2026-11-20 00:03） | S30 |
| L-37 | 2026-11-19 06:01 | 情境、本機留存 | 每次錄音把前景視窗**標題**與**執行檔路徑**（含使用者名稱）寫進日誌（保留 30 天、進診斷 zip） | 每次錄音，自動 | 日誌摘要只留程式名稱、bundle ID、角色與布林值 | 已執行（待驗證）（2026-11-20 00:03） | S31 |
| L-38 | 2026-11-19 06:01 | 裝置資訊、本機留存 | 每次錄音把麥克風裝置名稱（藍牙耳機名稱常含本名）與 track 設定寫進日誌 | 每次錄音，自動 | 「Microphone capture started」不再記 `trackLabel`、`trackSettings` | 已執行（待驗證）（2026-11-20 00:03） | S32 |
| L-39 | 2026-11-19 11:04 | 跨裝置、本機留存 | 剪貼簿寫入沒有「只限本機」選項；「複製」與無法輸入時的兜底是非暫存寫入，剪貼簿歷史 App 會記錄 | 插字、複製、無法輸入時 | 寫入與還原改用 `prepareForNewContentsWithOptions:`（只限本機）；複製改為暫存；兜底寫入 60 秒未變更即清除 | 已執行（待驗證）（2026-11-20 00:03） | S33 |
| L-40 | 2026-11-19 11:04 | 文字（潛在）、金鑰 | DeepSeek 是預設 AI 供應商：中文介面第一次啟動寫入 `cloudAi.provider=deepseek`，英文寫 OpenRouter（預設模型 `deepseek/…`，可繞過網址封鎖） | 第一次啟動，自動寫入設定；AI 開啟才會送出 | Rust 端 `cloud_polish`／`test_ai_connection` 只允許 provider `ollama`＋位址 127.0.0.1／localhost／::1；前端預設 Ollama；DeepSeek 自清單註解停用；OpenRouter 預設模型移除 | 已執行（待驗證）（2026-11-20 00:03） | S34 |
| L-41 | 2026-11-19 11:04 | 金鑰、診斷 | WebDAV 備份會上傳所有金鑰（含 WebDAV 密碼本身）、熱詞、使用統計 | WebDAV 啟用時（預設關閉） | WebDAV 整個停用（入口、自動備份、指令）；所有匯出／備份排除 `cloudAi.*`、`cloudAsr.*`、`webdav.*`、`userStats`、`stats` | 已執行（待驗證）（2026-11-20 00:03） | S35 |
| L-42 | 2026-11-19 11:04 | （防線缺口） | 匯入設定／完整備份／WebDAV 還原可把強化設定改回（AI、歷史、情境感知、workMode、雲端 AI、WebDAV 自動上傳等） | 手動匯入 | 所有匯入路徑不覆寫隱私開關、雲端與 WebDAV 設定、下載來源、`backendBaseUrl`、`pendingUpdate`（前綴比對） | 已執行（待驗證）（2026-11-20 00:03） | S36 |
| L-43 | 2026-11-19 11:04 | （防線缺口）、本機留存 | 舊 Electron 版資料遷移每次啟動都檢查 `~/Library/Application Support/{SayIt,sayit,SayIt-dev,sayit-dev}/sayit.db`，存在時覆蓋所有設定並匯入歷史 | 每次啟動，自動（目前不存在） | 呼叫處註解停用；函式直接返回；不再探測這些資料夾 | 已執行（待驗證）（2026-11-20 00:03） | S37 |
| L-44 | 2026-11-19 11:04 | 文字 | 本機 Ollama 連線會走 macOS 系統代理，轉錄文字可能以明文經過代理 | 接 Ollama 後每次 AI 校對 | Ollama 專用客戶端 `.no_proxy()`、不跟隨轉址 | 已執行（待驗證）（2026-11-20 00:03） | S38 |
| L-45 | 2026-11-19 11:04 | （防線缺口） | 沒有導覽防護：WebView 若被導向外部網頁，該網頁可呼叫 App 指令並使用麥克風；把連結拖進視窗會導覽 | 未發現觸發方式（拖放可觸發） | 外掛層 `on_navigation` 只允許 `tauri://localhost`、`http(s)://tauri.localhost`（僅預設埠）、`about:blank`（debug 另加 devUrl）；新視窗在 macOS 預設即被拒（wry 未設定 handler 時回傳 None）。殘留：前端有 `core:window:allow-create`，以腳本建立的新視窗應同樣受 `on_navigation` 限制，尚未實測 | 已執行（待驗證）（2026-11-20 00:03） | S39 |
| L-46 | 2026-11-19 06:01 | 本機留存、情境 | 使用統計 userStats（各 App 的使用次數與字數、首次／最後使用時間）不受歷史開關控制、永不清除、會隨匯出與備份 | 每次辨識，自動 | 不再保存（短路）；匯出也不帶 | 已執行（待驗證）（2026-11-20 00:03） | S40 |
| L-47 | 2026-11-19 06:01 | 本機留存、裝置資訊 | 日誌含 macOS 帳號名稱（檔案路徑）、鍵盤健康資訊、當機訊息 | 自動 | 視情況處理 | 僅記錄 | S41 |
| L-48 | 2026-11-19 11:04 | （防線缺口） | 強化只改了「預設值」，沒有在 Rust 端強制；托盤、快捷鍵、匯入、還原、遷移都可改回 | — | AI 在 Rust 端強制只允許本機 Ollama；雲端 ASR／串流與 WebDAV 指令註銷；匯入不覆寫；遷移停用 | 已執行（待驗證）（2026-11-20 00:03） | S42 |
| L-49 | 2026-11-19 11:04 | 診斷 | ASR 校正對話框沒有 `catch`，停用的上傳拋錯成為未處理例外並寫入日誌 | 使用者點擊（休眠） | 提交與撤回都加 `catch` | 已執行（待驗證）（2026-11-20 00:03） | S43 |
| L-50 | 2026-11-19 06:01 | 金鑰、本機留存 | 匯出設定／完整備份把 API 金鑰與 WebDAV 密碼以明文寫到「下載項目/SayItLocally Backups」，不跳確認 | 使用者點擊匯出 | 匯出排除 `cloudAi.*`、`cloudAsr.*`、`webdav.*`（含所有金鑰與密碼）；新增 Rust 測試鎖定。註：`sayit.db` 本身仍以明文保存設定 | 已執行（待驗證）（2026-11-20 00:03） | S44 |
| L-51 | 2026-11-19 06:01 | 診斷、本機留存 | 診斷 zip（完整日誌、熱詞、統計）在取消存檔時留在 App 資料夾 | 使用者建立診斷包後取消 | 建議取消時刪除暫存 zip（zip 的設定段是白名單，不含金鑰） | 待處理 | S45 |
| L-52 | 2026-11-19 06:01 | 語音、本機留存 | 「匯出全部資料」會帶上所有錄音檔 | 使用者點擊 | 錄音保留已關閉（L-14），目前沒有錄音檔；日後若開啟錄音保留再評估 | 僅記錄 | S46 |
| L-53 | 2026-11-20 00:03（登錄時間） | （倉庫內容，未讀） | 上游作者的設定匯出檔留在倉庫：`client/src-tauri/sayit-settings-2026-04-27T01-45-59-829Z.json`（上游 commit `c9510f3` 加入）。本 fork 未讀取其內容 | — | 建議自本 fork 移除；公開 git 歷史仍會保有該檔 | 建議 | S47 |
| L-54 | 2026-11-20 00:03（登錄時間） | （防線缺口） | App 無沙盒、ad-hoc 簽章、hardened runtime：作業系統層沒有任何限制對外連線的機制 | — | 以 LuLu 等系統層防火牆封鎖（同 L-26 的下載後封鎖） | 建議 | S48、S4 |
| L-55 | 2026-11-19 06:01 | 金鑰、本機留存 | 設定 `RUST_LOG=trace` 時，Gemini Live 的金鑰可能寫入日誌 | 需手動設定環境變數 | Gemini Live 指令已註銷（L-18），不可達；不要設定 `RUST_LOG` | 僅記錄 | S49 |
