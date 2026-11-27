# 貢獻指南

感謝你對 SayIt 的關注！

## 關於 PR

**本專案暫不接受 Pull Request。**

整個專案都是使用 AI（Claude Opus 4.6）開發的，PR 現在大機率也是 AI 寫的，這種情況很難 review。

## 歡迎提交 Issue

我非常歡迎以下類型的 Issue：

- 🐛 **Bug 回報** — 描述問題、重現步驟、預期行為
- 💡 **功能建議** — 你希望 SayIt 增加什麼功能
- 📝 **使用回饋** — 哪裡體驗不好、哪裡不夠直覺

提交 Issue 時請盡量包含：
- 作業系統版本
- SayIt 版本號（在「關於」頁面查看）
- 使用的模式（伺服器 / 雲端 API / 本機）
- 問題的具體描述和重現步驟

## 本機開發

如果你想在本機跑起來看看：

### 用戶端

```bash
cd client
npm install
npm run tauri dev
```

前置需求：Node.js 18+、Rust 1.75+、CMake 3.20+、Vulkan SDK
（本機辨識引擎要用 CMake 編譯原生函式庫、Vulkan SDK 編譯 SPIR-V 著色器；裝完請開新的終端機，
非英文 Windows 還需設定 `CL=/utf-8`。macOS 上不需要 Vulkan SDK，安裝 Xcode Command Line Tools 即可。
詳見 README 的「開發 / 用戶端」一節。）

### 伺服器端

（本 fork：`server/` 目錄已移除；伺服器端開發請見上游倉庫 https://github.com/crosswk/SayIt 。）

## 程式碼規範

- TypeScript strict 模式
- ESLint + Prettier 自動格式化
- UI 文字使用中文

## 提交規範

使用語意化提交訊息：

- `feat:` 新功能
- `fix:` 修正 bug
- `refactor:` 重構
- `chore:` 建置/工具/依賴變更
- `docs:` 文件更新
- `perf:` 效能最佳化
