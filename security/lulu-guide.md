# LuLu 安裝與封鎖指引（L-26、L-54）

整理時間：2026-11-20 22:35（UTC+08:00）。最後更新：2026-11-25 01:13。此步驟需使用者自行操作，因為要在系統設定中核准網路延伸功能。

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
   - 請先安裝並設定 LuLu，再放入 `SayItLocally.app`，或取消勾選「允許既有程式」。若重建後規則失配，「允許既有程式」可能導致 App 自動放行。這是依設定行為推論，尚待實機確認。

Homebrew 也提供 `lulu` cask（`brew install --cask lulu`），下載的是同一個 GitHub 檔案，SHA-256 也相同。

## 封鎖 SayItLocally

1. 用瀏覽器或另一台裝置從 catalog 登錄的來源下載模型。網址格式為 `https://huggingface.co/<repo>/resolve/main/<file>`。模型 ID、上游作者的 Hugging Face 倉庫（`repo`）及原始檔名（`file`）請查 [模型 catalog](../client/src-tauri/src/models/catalog.rs)。
2. 執行 `shasum -a 256 "<模型檔案>"`，比對結果與同一款模型在 catalog 的 `sha256`。兩者一致才繼續。
3. 保留 catalog 的原始檔名，將模型搬到 `~/Library/Application Support/com.sayitlocally.desktop/models/<模型 ID>/`。若曾自訂模型位置，請用自訂的模型根目錄。可用下列指令建立預設目錄，先把 `<模型 ID>` 換成實際的模型 ID。

   ```sh
   mkdir -p "$HOME/Library/Application Support/com.sayitlocally.desktop/models/<模型 ID>"
   ```

4. **第一次開啟 `SayItLocally.app` 之前**，先在 LuLu 的 Rules 視窗選 Add Rule，加入下列封鎖規則：
   - 程式路徑：`/Applications/SayItLocally.app/Contents/MacOS/sayitlocally`（依實際安裝位置調整）
   - 遠端位址：`*`，連接埠：`*`
   - 動作：**Block**
5. 開啟 App 並選取模型。App 載入前會核對登錄的檔名、大小及 SHA-256（L-94）。
   - 聽寫時如發生模型載入驗證錯誤，畫面會顯示對應的本機模型提示。**請保留 Block 規則**，到「語音引擎」頁的測試卡查看缺檔、不符或讀取失敗的詳細原因，依步驟 1～3 驗證並重新放入正確檔案。
   - 模型大小不符時，設定頁會顯示未下載或提供「下載」按鈕。**請勿按「下載」**，因為按下仍會嘗試連線。既有檔案會保留到新檔通過大小與 SHA-256 驗證，在 Block 規則下可依步驟 1～3 手動換入正確檔案。

每次重建後請確認封鎖規則仍然存在。LuLu 可能把重新 ad-hoc 簽章的版本視為新程式，再次提示時請選 Block。平常可留意 LuLu 的提示與規則清單，查看 App 是否嘗試連線（S10）。

### 備案：暫時允許 App 下載

若必須在 App 內下載，請只暫時放行。LuLu 提示時先確認目的地是 `huggingface.co` 或該模型下載所用的 CDN，再選 Allow，且**不要**勾選永久規則。下載完成後移除暫時的放行規則，並依上述步驟加入或恢復 Block 規則。

## 模式

- **Passive Mode**：不跳提示，只套用既有規則；新的連線依設定一律允許或一律封鎖。
- **Block Mode**：除了允許清單以外，全部封鎖。部分系統流量會繞過網路延伸功能，無法攔截。

## 移除

從選單列的 LuLu 圖示選 Uninstall（需要輸入密碼）。這會一併移除系統延伸功能，比直接刪除 App 乾淨。用 Homebrew 安裝的話，先用 LuLu 自己的移除功能，再執行 `brew uninstall --zap lulu`。
