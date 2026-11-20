# 掃描記錄

時間皆為 UTC+08:00；最後更新：2026-11-20 22:12

| 編號 | 開始 | 完成 | 範圍 | 方法 | 主要結果 | 相關登錄 |
|---|---|---|---|---|---|---|
| SC-01 | 2026-11-19 05:01 | — | 網路資料 | 網路搜尋 | 確認 crosswk/SayIt（AGPL-3.0）有 Local 模式；同名的 chenjackle45/SayIt 只能用雲端 | — |
| SC-02 | 2026-11-19 05:02 | — | 原始碼初查 | clone 原始碼、閱讀 README、搜尋遙測套件與網址 | 上游預設為 Server 模式、AI 校對預設開啟；有公告、更新、模型下載等連線 | L-02、L-03、L-06、L-07、L-08 |
| SC-03 | 2026-11-19 05:15 | 2026-11-19 05:16 | 前端與 Rust 連線全面審計 | 搜尋 `fetch`、WebSocket、`reqwest`、CSP、外部資源 | 新增：Google Fonts、意見回饋、診斷、ASR 校正上傳、healthz、熱詞、CSP 過寬、本機留存、剪貼簿 | L-01、L-09～L-15、L-31 |
| SC-04 | 2026-11-19 05:20 | 2026-11-19 05:20 | 公告與更新細查 | 逐行閱讀 `notice.ts`、`autoUpdate.ts`、`updateChecker.ts`、`system.rs` | 公告不帶參數，但每 3 小時一次；更新會**自動下載並在結束時靜默安裝**（macOS 也會） | L-02～L-05 |
| SC-05 | 2026-11-19 05:17 | — | 日誌與剪貼簿抽查 | 搜尋記錄轉錄文字的日誌呼叫 | 抽查到的日誌只記字數；剪貼簿 `not_editable` 分支不還原 | L-27、L-30 |
| SC-06 | 2026-11-19 05:33 | — | 系統設定 | `defaults -currentHost read com.apple.coreservices.useractivityd` | 接力（通用剪貼板）已關閉 | L-15 |
| SC-07 | 2026-11-19 05:40 | — | 建置產物驗證 | 搜尋 `client/dist` 字串；檢查執行檔內嵌 CSP | 字型、公告、更新網址皆 0 處；CSP 為新值 | L-01～L-03、L-13 |
| SC-08 | 2026-11-19 05:46 | — | 自我複核（修改內容） | 閱讀 local-only 相關 diff | 非外流問題：第一次啟動時 `workMode` 未寫入設定檔，Rust 端不會預先載入本地模型（只影響速度） | — |
| SC-09 | 2026-11-19 05:46 | 2026-11-19 05:51 | 前端所有外流路徑 | 子代理（Claude Haiku 5.5） | 新發現：撤回校正未守衛、WebDAV 自動備份、診斷頁自動 AI 測試、檢查連線按鈕、`aiPolicy` 預設值、外部連結、`devCsp`、shell 範圍 | L-17、L-19～L-22、L-24、L-25、L-28、L-29 |
| SC-10 | 2026-11-19 05:46 | 2026-11-19 05:49 | Rust 所有網路 I/O | 子代理（Claude Haiku 5.5） | 啟動流程沒有網路；雲端指令仍註冊且不受 CSP 限制；DeepSeek 封鎖有缺口；沒有遙測套件；`ggml-rpc` 關閉 | L-18、L-23、L-32～L-34 |
| SC-11 | 2026-11-19 05:58 | — | 模型來源與裝置識別碼 | 子代理（Claude Haiku 5.5） | 模型預設從 `hf-mirror.com` 下載，SHA-256 寫死且會驗證；裝置 ID 為隨機 UUID；Server 模式會送出作業系統使用者名稱、主機名稱、區網 IP 等 | L-06、L-17、L-26、L-35 |
| SC-12 | 2026-11-19 05:52 | 2026-11-19 06:01 | 日誌與本機留存 | 子代理（Claude Haiku 5.5） | 日誌會記視窗標題、執行檔路徑、麥克風名稱；轉錄文字只記字數；userStats 永久保存；匯出／備份以明文寫出金鑰；診斷 zip 取消時留存；匯出全部含錄音 | L-30、L-37、L-38、L-46、L-47、L-50～L-52、L-55 |
| SC-13 | 2026-11-19 06:11 | 2026-11-19 11:04 | 全面深度複核（06:24 因用量上限中斷，10:48 在唯讀快照 `96c2320` 上恢復） | 子代理（Claude Opus 5.5，effort max） | 新發現：自動註冊登入項目、DeepSeek 為預設 AI 供應商、WebDAV 上傳金鑰、匯入可改回強化設定、舊版遷移、Ollama 走系統代理、沒有導覽防護、剪貼簿非只限本機、強化未在 Rust 強制；更正 SC-12 與 L-26、L-27、L-29 的部分結論 | L-36、L-39～L-45、L-48、L-49 |
| SC-14 | 2026-11-19 23:43 | 2026-11-20 00:12 | 第一次開啟前的安全修正與建置驗證（分支 `security/first-launch-hardening`，基準 `3428af4`） | 修改程式碼；`npm run build`、`npm run -s i18n:check`、`npx vitest run`、`cargo check --tests`、`cargo test`、`npx tauri build --bundles app`；檢查產物字串；查證 `tauri-plugin-shell` 2.3.5 與 wry 0.54.4 原始碼 | 處理 L-16～L-28、L-36～L-46、L-48～L-50；建置通過；vitest 失敗數與修改前相同（17 個，皆為先前強化造成）；`cargo test` 370 通過、0 失敗；產物 CSP 為 `connect-src 'self' ipc: http://ipc.localhost`，`client/dist` 無公告、更新、字型、`api.deepseek.com`、`hf-mirror` 字串；shell open 預設允許 http、https、mailto、tel。**未執行 App** | L-16～L-30、L-36～L-55 |
| SC-15 | 2026-11-20 01:33 | 2026-11-20 01:33 | 外部稽核比對（對象 `99f57e9`） | 使用者提供外部稽核報告（ChatGPT GPT-6.2 Astra Max，靜態分析，未執行 App）；逐項比對既有登錄，並在本機程式碼核對新項目、檢查本機 npm 快取 | 與既有結論一致：沒有通往 DeepSeek、阿里雲、SayIt 伺服器的路徑，缺模型時不改走雲端。新增 12 項：版本同名、README 上游連結、鎖定檔指向 npmmirror（本 fork 建置已實際連線）、離線指引與重試仍用 hf-mirror、雲端控制台連結、Ollama 雲端模型、上下文感知、錄音保留期限、Ollama 錯誤片段進日誌、啟動時蒐集裝置資訊、`TRANSCRIBE_DUMP_DIR`。**未改程式** | L-56～L-67 |
| SC-16 | 2026-11-20 22:12 | 2026-11-20 22:12 | 修正 L-59～L-61（分支 `fix/s56-s58-download-source`，基準 `67764ee`） | 修改 `catalog.rs`（移除鏡像來源）、`LocalModeSection.tsx`、`AIProviderSection.tsx`、`CloudAPISection.tsx`、`AppearancePage.tsx`；`tsc --noEmit`、`npx vitest run`（與基準比對）、`cargo test --offline catalog`；另以程式碼搜尋清查所有 `shellOpen` 外部連結 | `tsc` 0 錯誤；vitest 17 失敗／618 通過，與基準完全相同；`cargo test` catalog 相關 5 通過。清查結果：雲端控制台連結共三處元件（AI 供應商、語音辨識雲端平台、外觀頁串流），已全部隱藏；其餘外部連結為關於頁的上游 GitHub、公告橫幅、意見回饋，不在本項範圍。**未執行 App、未建置安裝包** | L-59～L-61 |

## 更正

- SC-10 之後更正：先前以為收緊 CSP 後「漏改的程式路徑也會被擋」。實際上 CSP 只管 WebView，前端透過 `invoke` 呼叫的 Rust 指令不受限制（見 L-18）。
- SC-14 更正：先前（SC-13）記為「shell open 只允許 https、mailto、tel」，查證後預設正規式為 `^((mailto:\w+)|(tel:\w+)|(https?://\w+)).+`，**http 也允許**；file:// 與自訂協定仍會被拒。
- SC-15 更正：L-26 記為「模型下載改為官方來源」，但離線安裝指引與「切換來源並重試」仍會使用 `hf-mirror.com`（見 L-59、L-60）。
- 2026-11-20 01:55 歷史改寫：先前 commit 的作者資訊含本機帳號與主機名稱，已將 `v0.2.5` 之後的 commit 作者改為 GitHub noreply 身分並 force push；文件中的 commit hash 已同步更新（目前合併點 `99f57e9`）。
