# 離線建置與連線對照

- 建立：2026-11-24（SC-23，處理 L-84～L-92）；依據外部稽核 SC-20（對象 `379f24e`）。
- 腳本：[`client/scripts/macos/build-offline.sh`](../client/scripts/macos/build-offline.sh)。**截至建立時尚未實際執行**，第一次執行前要先取得使用者同意。
- **這個腳本只能降低連網的機會，不保證零外連。** 第一次執行時，請同時用 LuLu 封鎖外連，並對照下方的主機清單。（2026-11-24 01:33 依外部覆核 SC-24 修改）

## 腳本做了什麼

| 登錄 | 風險 | 腳本的處理 | 限制 |
|---|---|---|---|
| L-84 | npm 缺套件時，會從 `registry.npmjs.org` 下載 | `npm_config_offline=true`；`--install` 時執行 `npm ci --offline --include=dev --include=optional` | 快取不完整時會直接失敗，不會改去下載。npm 的離線設定只管 npm 本身，管不到安裝腳本自己發出的連線 |
| L-85 | `npm ci` 預設做 audit，會送出依賴清單 | `.npmrc` 設定 `audit=false`，腳本另加 `--no-audit` | — |
| L-86 | npm 會檢查自身新版本 | `.npmrc` 設定 `update-notifier=false` | — |
| L-87 | esbuild 的 postinstall 找不到 binary 時，會自行下載 | 安裝時加 `--ignore-scripts`，不執行任何安裝腳本；再依 Node 的架構確認 `@esbuild/darwin-*` 的 binary 已在本機。esbuild 的 postinstall 只是加速步驟，不執行也能正常使用 | 日後若有其他套件新增安裝腳本，要重新檢查 |
| L-88、L-89 | Cargo 會更新索引、下載 crate | `CARGO_NET_OFFLINE=true`，並把 `--locked` 傳給 cargo | 快取不完整時直接失敗 |
| L-90 | rustup 會補裝工具鏈 | 在呼叫任何 rustup／rustc 之前，先直接讀檔確認預設工具鏈與它的標準庫都已安裝，並拒絕 `RUSTUP_TOOLCHAIN`、`rust-toolchain` 檔與目錄 override；接著要求 rustup 1.28.1 以上，並設定 `RUSTUP_AUTO_INSTALL=0` | 只支援預設工具鏈 |
| L-91 | 缺 Xcode、CLT 或 SDK 時，系統會補裝 | 檢查 `xcode-select -p`、`xcrun --show-sdk-path`、`cmake`，缺少就停下 | macOS 的背景更新不在腳本控制範圍內 |
| L-92 | 有 `APPLE_*` 變數時，會進入公證流程，把 App 上傳到 Apple | 腳本開頭清除所有 `APPLE_*` 變數；簽章維持 ad hoc（`-`） | — |

## LuLu 對照：建置時的連線主機

| 主機 | 程序 | 判讀 |
|---|---|---|
| `registry.npmjs.org` | node／npm | 用離線腳本建置時**不應出現**；出現時代表快取不完整、有設定沒生效，或是 esbuild 的備援下載 |
| `index.crates.io`、`static.crates.io` | cargo | 不應出現；出現代表 Cargo 的離線設定沒生效 |
| `static.rust-lang.org` | rustup | 不應出現 |
| `swscan.apple.com`、`swcdn.apple.com`、`updates.cdn-apple.com` | 軟體更新／Xcode | 系統背景更新也可能出現；要依程序和時間判斷 |
| Apple 公證相關（`appstoreconnect.apple.com`、`notary-submissions-prod.*.amazonaws.com`、`api.apple-cloudkit.com`） | notarytool／stapler | **不應出現**；出現代表公證被觸發了 |
| `ocsp.apple.com`、`valid.apple.com` 等 | trustd | 系統憑證驗證，和這次建置無直接關係 |
| `registry.npmmirror.com`、`huggingface.co`、`sayitapp.site` | — | 建置時**都不應出現**；出現就是異常，請保留記錄 |

## 其他限制（SC-24）

- 只接受 `--install`、`--bundles app|dmg`、`--debug`、`--verbose` 這幾個參數。自訂的 Tauri config 或 runner 可能繞過離線設定，所以一律拒絕。
- 直接呼叫專案內的 `node_modules/.bin/tauri`，不經過 `npx`，避免改用全域或快取裡的 CLI。
- 不執行外部的環境檔（它也可能連網），改為直接把本機的 Node、CMake、Rust 加進 PATH（SC-25）。
