# 安全與隱私強化紀錄

本目錄記錄這個 fork（分支 `hardening/local-only`）在「資訊外流」方面的調查、處理方式與進度。

| 文件 | 內容 |
|---|---|
| [`leak-register.md`](leak-register.md) | **外流風險登錄表**：每一項的發現時間、外流類型、內容與目的地、處理方式、狀態 |
| [`scan-log.md`](scan-log.md) | **掃描記錄**：每次掃描的時間、範圍、方法與結果 |

- 基準版本：上游 [crosswk/SayIt](https://github.com/crosswk/SayIt) commit `4fdeae9`（2026-11-09），client `0.2.5`
- 強化 commit：`de5a362`（2026-11-19 05:44）
- 文件中的時間皆為 UTC+08:00；最後更新：2026-11-19 06:10
- 專案名稱：2026-11-19 10:50 由 SayIt 更名為 **SayItLocally**（App：`SayItLocally.app`，bundle ID：`com.sayitlocally.desktop`）；上游名稱與來源資訊保留

## 原則

1. 語音、文字，以及使用資訊（IP、使用時段、裝置資訊）都不離開本機。
2. 只使用 Local 模式（本機 GGUF 模型辨識）。
3. AI 校對預設關閉；日後只考慮本機 Ollama。
4. 嚴禁 DeepSeek。
5. 不保存歷史記錄與錄音。
6. 不使用官方安裝檔、不自動更新，一律從原始碼自行建置。

## 目前進度

| 狀態 | 項目數 | 編號 |
|---|---|---|
| 已執行（待驗證） | 14 | L-01～L-14 |
| 已執行（系統設定，已檢查） | 1 | L-15 |
| 部分處理 | 1 | L-16 |
| 待處理 | 14 | L-17～L-29、L-31 |
| 審查中 | 1 | L-30 |
| 僅記錄 | 4 | L-32～L-35 |

> ⚠️ **尚未實際執行 App、尚未斷網實測、尚未抓封包驗證。**「已執行」只代表程式已修改並通過建置，並對建置產物做過字串檢查。

## 狀態說明

| 狀態 | 意義 |
|---|---|
| 建議 | 已提出，尚未執行 |
| 執行中 | 正在處理 |
| 已執行（待驗證） | 程式已修改並通過建置，**尚未實際執行、斷網實測或抓封包驗證** |
| 部分處理 | 主要路徑已處理，仍有已知缺口 |
| 待處理 | 已發現，尚未處理 |
| 審查中 | 掃描或分析仍在進行 |
| 已確認 | 已用實際執行或封包驗證確認（目前尚無） |
| 僅記錄 | 分析後沒有實際外流，或風險極低 |

## 防線與限制

| 防線 | 擋得住 | 擋不住 |
|---|---|---|
| 程式碼停用（入口註解＋函式內提前返回） | 已找到的連線路徑 | 尚未發現的路徑 |
| CSP：`connect-src 'self' ipc: http://ipc.localhost`（`client/src-tauri/tauri.conf.json`） | 介面（WebView）自己發出的 `fetch`、WebSocket、圖片與字型載入 | **Rust 端的所有連線**，包括前端透過 `invoke` 呼叫、仍註冊中的 Rust 指令（登錄表 L-18） |
| 系統層防火牆（LuLu，評估中） | 整個 App 的所有對外連線，含 Rust 端 | 尚未安裝 |

> **更正**：先前曾以為收緊 CSP 後「漏改的程式路徑也會被擋」。實際上 CSP 只管 WebView，走 Rust 的連線不受 CSP 限制。

## 驗證計畫

| 項目 | 狀態 |
|---|---|
| 安裝 LuLu，模型下載完成後封鎖 SayItLocally 所有對外連線 | 建議（評估中） |
| 斷網實測：Local 模式、AI 校對關閉時辨識是否正常 | 建議 |
| 監看實際連線、抓封包，確認沒有任何對外請求 | 建議 |
| Claude Opus 5.5（effort max）全面深度複核 | 排程中 |

## 如何自行檢查

- 列出所有修改處：`git grep -n "\[自訂"`
- 與上游比較：`git diff 4fdeae9 -- client/`
- 建置後確認介面沒有外部網址：在 `client/dist` 搜尋 `fonts.googleapis`、`/api/notice`、`desktop-updates`（2026-11-19 05:40 檢查結果皆為 0 處）
- 確認執行檔內嵌的 CSP：`strings client/src-tauri/target/release/sayitlocally | grep connect-src`
