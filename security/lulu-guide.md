# LuLu 安裝與封鎖指引（L-26、L-54）

整理時間：2026-11-20 22:35（UTC+08:00）。此步驟需使用者自行操作，因為要在系統設定中核准網路延伸功能。

## 為什麼需要

SayItLocally 是自行建置的 App，沒有沙盒，只有 ad-hoc 簽章。CSP 只能限制 WebView，Rust 端發出的連線（例如模型下載）不受限制。LuLu 在作業系統層攔截對外連線，可以作為最後一道防線，也可以記錄 App 是否嘗試連線。

## 版本資訊（查證日 2026-11-20）

| 項目 | 內容 |
|---|---|
| 最新版 | 4.5.7（2026-11-02） |
| 授權 | GPL-3.0，免費 |
| 官方頁面 | https://objective-see.org/products/lulu.html |
| 原始碼與發行版 | https://github.com/objective-see/LuLu/releases |
| 安裝檔 | `LuLu_4.5.7.dmg` |
| SHA-256 | `8d60f32b6972648aab9203f2f68bd3a804e2e05c23d60200d5e093d6bdb06e8c` |
| 系統需求 | 官方標示 macOS 10.15 以上；官方未提到 macOS 27，相容性未經確認 |

## 安裝

1. 從上表的 GitHub 發行頁或官方頁面下載 dmg，然後在終端機核對檔案：
   - `shasum -a 256 ~/Downloads/LuLu_4.5.7.dmg`，結果應與上表的 SHA-256 相同。
   - `spctl -a -t open --context context:primary-signature -vv ~/Downloads/LuLu_4.5.7.dmg`，應顯示已通過公證。
2. 打開 dmg，把 `LuLu.app` 拖到「應用程式」，再從「應用程式」開啟。
3. 依照提示到「系統設定」核准 LuLu 的系統延伸功能與網路過濾器（內容過濾器）。這是 LuLu 攔截連線的方式，不是 VPN。核准時需要輸入密碼。
4. 第一次設定時，建議選擇允許 Apple 程式與既有程式，以免日常程式跳出大量提示。

Homebrew 也提供 `lulu` cask（`brew install --cask lulu`），下載的是同一個 GitHub 檔案，SHA-256 也相同。

## 封鎖 SayItLocally

建議順序：先下載本機模型，再設定封鎖。

1. 第一次開啟 `SayItLocally.app` 並下載模型時，LuLu 會跳出提示。確認連線的主機是 `huggingface.co`（或它的 CDN），再選 Allow，並且**不要**勾選永久規則。
2. 模型下載完成後，開啟 LuLu 的 Rules 視窗，選 Add Rule：
   - 程式路徑：`/Applications/SayItLocally.app/Contents/MacOS/sayitlocally`（依實際安裝位置調整）
   - 遠端位址：`*`，連接埠：`*`
   - 動作：**Block**
3. 自建版本是 ad-hoc 簽章，每次重新建置簽章都會改變，LuLu 可能把它當成新程式而再次詢問。官方文件沒有說明規則是依路徑還是依簽章比對，所以每次重建後請確認規則仍然存在；再出現提示時一律選 Block（下載模型時除外）。
4. 平常可以留意 LuLu 的提示與規則清單，作為 App 是否嘗試連線的紀錄（相關項目：S10 抓封包）。

## 模式

- **Passive Mode**：不跳提示，只套用既有規則；新的連線依設定一律允許或一律封鎖。
- **Block Mode**：除了允許清單以外，全部封鎖。部分系統流量會繞過網路延伸功能，無法攔截。

## 移除

從選單列的 LuLu 圖示選 Uninstall（需要輸入密碼）。這會一併移除系統延伸功能，比直接刪除 App 乾淨。用 Homebrew 安裝的話，先用 LuLu 自己的移除功能，再執行 `brew uninstall --zap lulu`。
