# 離線建置與連線對照

- 建立：2026-11-24（SC-23，處理 L-84～L-92）；依據外部稽核 SC-20（對象 `379f24e`）。
- 腳本：[`client/scripts/macos/build-offline.sh`](../client/scripts/macos/build-offline.sh)。截至 SC-27，只做靜態檢查；第一次執行前要先取得使用者同意。
- **這個腳本只能降低連網的機會，不保證零外連。**
- 第一次執行時，請同時用 LuLu 封鎖外連，並對照下方的主機清單。LuLu 尚未安裝，尚無封鎖或實際連線紀錄。

## 腳本做了什麼

| 登錄 | 風險 | 腳本的處理 | 限制 |
|---|---|---|---|
| L-84 | npm 缺套件時，會從 `registry.npmjs.org` 下載 | `npm_config_offline=true`；`--install` 時執行 `npm ci --offline --include=dev --include=optional` | 快取不完整時會直接失敗，不會改去下載。npm 的離線設定只管 npm 本身，管不到安裝腳本自己發出的連線 |
| L-85 | `npm ci` 預設做 audit，會送出依賴清單 | `.npmrc` 設定 `audit=false`，腳本另加 `--no-audit` | — |
| L-86 | npm 會檢查自身新版本 | `.npmrc` 設定 `update-notifier=false` | — |
| L-87 | esbuild 的 postinstall 找不到 binary 時，會自行下載 | 安裝時加 `--ignore-scripts`，不執行任何安裝腳本；再依 Node 的架構確認 `@esbuild/darwin-*` 的 binary 已在本機。esbuild 的 postinstall 只是加速步驟，不執行也能正常使用 | 日後若有其他套件新增安裝腳本，要重新檢查 |
| L-88、L-89 | Cargo 會更新索引、下載 crate | `CARGO_NET_OFFLINE=true`，並把 `--locked` 傳給 cargo | 快取不完整時直接失敗 |
| L-90 | rustup 會補裝工具鏈 | SC-64 起：版本取自 `client/src-tauri/rust-toolchain.toml` 的 `channel`（目前 1.100.0），只接受三段數字的固定版本；工具鏈為 `<版本>-aarch64-apple-darwin`。確認 Cargo、rustc 與標準庫已安裝後，將工具鏈的 `bin` 放在 PATH 首位，固定 `CARGO`、`RUSTC`，另設 `RUSTUP_TOOLCHAIN` 為同一工具鏈、`RUSTUP_AUTO_INSTALL=0` | 只支援 Apple Silicon（`uname -m` 為 arm64）。工具鏈未安裝時直接停下，不會下載；rustup 的預設工具鏈與目錄 override 不參與選擇 |
| L-91 | 缺 Xcode、CLT 或 SDK 時，系統會補裝 | 檢查 `xcode-select -p`、`xcrun --show-sdk-path`、`cmake`，缺少就停下 | macOS 的背景更新不在腳本控制範圍內 |
| L-92 | 有 `APPLE_*` 變數時，會進入公證流程，把 App 上傳到 Apple | 腳本開頭清除所有 `APPLE_*` 變數；簽章維持 ad hoc（`-`） | — |

## LuLu 對照：建置時的連線主機

| 主機 | 程序 | 判讀 |
|---|---|---|
| `registry.npmjs.org` | node／npm | 用離線腳本建置時**不應出現**；出現時代表快取不完整、有設定沒生效，或是 esbuild 的備援下載 |
| `index.crates.io`、`static.crates.io` | cargo | 不應出現；出現代表 Cargo 的離線設定沒生效 |
| `static.rust-lang.org` | rustup | 正常流程不應出現；若出現，核對是哪個子程序或外部工具叫用了 rustup |
| `swscan.apple.com`、`swcdn.apple.com`、`updates.cdn-apple.com` | 軟體更新／Xcode | 系統背景更新也可能出現；要依程序和時間判斷 |
| Apple 公證相關（`appstoreconnect.apple.com`、`notary-submissions-prod.*.amazonaws.com`、`api.apple-cloudkit.com`） | notarytool／stapler | **不應出現**；出現代表公證被觸發了 |
| `ocsp.apple.com`、`valid.apple.com` 等 | trustd | macOS 系統信任查詢，須依觸發程序及時間判讀 |
| `registry.npmmirror.com`、`huggingface.co`、`sayitapp.site` | — | 建置時**都不應出現**；出現就是異常，請保留記錄 |

## 建置入口與外連限制

- 只接受 `--install`、`--bundles app|dmg`、`--debug`、`--verbose` 這幾個參數。自訂的 Tauri config 或 runner 可能繞過離線設定，所以一律拒絕。
- SC-49 更新鎖定檔後，既有的 `client/node_modules` 仍是舊版。下次建置須加上 `--install`，才能從本機快取重裝並使用更新後的前端套件。SC-49 首輪已在暫存複本驗證本機 npm 快取足以離線安裝。
- 清除繼承的 `TAURI_CONFIG`，避免透過環境變數傳入額外的 Tauri 建置設定。
- 直接呼叫專案內的 `node_modules/.bin/tauri`，不經過 `npx`，避免改用全域或快取裡的 CLI。
- PATH 直接加入本機 Node、CMake 及已核對的工具鏈，並移除預設與自訂 Cargo home 的 `bin`，以略過常見的 rustup proxy。Tauri CLI 2.10.1 的桌面建置只有在顯式指定 target 時會執行 `rustup target list`。腳本的白名單排除了這個入口。SC-64：鎖定檔現為 CLI 2.12.6，本機快取還沒有它的套件檔，尚無法離線查證，須在補齊快取後、首次建置前再查；`tauri-build` 2.7.6、`tauri-codegen` 2.7.6 原始碼未發現 rustup 呼叫或網路存取，`tauri-utils` 2.10.4 的 `xcodebuild` 呼叫只在 iOS target 時執行。其他傳遞依賴的執行行為未查證。
- 固定 npm 的離線設定與腳本 shell，移除 Node 預載選項，停用 Cargo 的 rustc 包裝程式，並清除繼承的 `CARGO_BUILD_TARGET`、`CMAKE_ARGS`、`TRANSCRIBE_CMAKE_ARGS`。

## 需由 LuLu 封鎖的連線（L-91、L-93）

- macOS 背景更新及 `trustd` 信任查詢。
- 前端腳本、Cargo 的 `build.rs` 與 CMake 子程序自行發出的 HTTPS 或其他外連。例如依外部設定啟用需要下載元件的原生後端。
- 使用者的工具設定，例如 Cargo 設定檔中的 linker、編譯器環境變數及 npm 設定。npm／Cargo 的離線旗標只約束各自的套件取得流程，子程序的外連仍要由防火牆封鎖。
- 可接受的殘餘風險：混合大小寫的 npm 環境變數可能覆蓋巢狀 npm 的離線設定。腳本直接執行的 `npm ci` 另有 `--offline` 命令列旗標。

## 第一版自建前的狀態（SC-64，2026-11-27）

- 只建本機環境：macOS、Apple Silicon（aarch64-apple-darwin），預設只產生 `.app`（`--bundles app`）；不做 universal、Intel、Windows、Linux。
- Rust：1.100.0 已安裝；Cargo 快取以 `cargo metadata --offline --locked` 與 `cargo fetch --offline --locked` 確認涵蓋鎖定檔，不需連網。
- npm：以套件內容雜湊比對本機快取，鎖定檔中本機平台需要的 257 個套件檔有 41 筆不在快取（多為前次依賴升級後的新版本，含 Tauri CLI 2.12.6、Vite 6.4.6、Babel 相關套件）。快取的索引記錄的是舊鏡像網址，npm 依內容雜湊取用，不影響離線安裝。
- 補齊方式：須經使用者同意，用一次性的暫存目錄，依鎖定檔從 `registry.npmjs.org` 取得，不執行安裝腳本、不更新版本；完成後再以同一方法確認缺少數為 0。
- 建置仍等使用者安裝 LuLu 並同意後才執行。

## 執行期第一次連線（供 LuLu 規則參考）

| 主機 | 何時 | 判讀 |
|---|---|---|
| `huggingface.co` | 使用者在設定中下載本機辨識模型時 | 預期內；只在下載模型時出現 |
| Hugging Face 的檔案 CDN（例如 `*.hf.co`、`cdn-lfs*.huggingface.co`） | 同上，`huggingface.co` 轉址後 | 預期內；實際主機名稱以 LuLu 紀錄為準 |
| `sayitapp.site`、`hf-mirror.com`、`registry.npmmirror.com`、各雲端辨識或 AI 服務主機 | — | 本 fork 已停用公告、更新、回饋、鏡像與雲端模式，**都不應出現** |

- 模型下載以外，App 執行時不需要任何外部連線；本機辨識、錄音與資料都留在本機。
