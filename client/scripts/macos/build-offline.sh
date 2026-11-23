#!/bin/zsh
# [自訂] SC-23（L-84～L-92）：離線建置本機自用的 SayItLocally.app／.dmg。
#
# 目的：建置過程不連網，也不把任何東西送出本機。
#   - npm：只用本機快取（--offline），不做 audit、不查新版、不顯示 fund（L-84～L-86）
#   - esbuild：保留 optional 套件，裝完後確認本機平台 binary 存在（L-87）
#   - Cargo：CARGO_NET_OFFLINE＋--locked，不更新索引、不下載 crate（L-88、L-89）
#   - rustup：RUSTUP_AUTO_INSTALL=0，不自動補裝工具鏈（L-90）
#   - Xcode／CLT、CMake：只檢查是否已安裝，缺少就停下，不觸發系統補裝（L-91）
#   - Apple 公證：清除所有 APPLE_* 變數，只用 ad hoc 簽章（L-92）
#
# 用法（在 client/ 目錄）：scripts/macos/build-offline.sh [--install] [tauri build 的其他參數]
#   --install  先用本機快取執行 npm ci（node_modules 已存在時可省略）
# 建議同時用 LuLu 封鎖外連，對照 security/build-offline.md 的主機清單。
# 快取不完整時建置會直接失敗，不會改去下載；屆時請先決定要不要連網補齊。
set -euo pipefail

SCRIPT_DIR=${0:A:h}
CLIENT_DIR=${SCRIPT_DIR:h:h}
cd "$CLIENT_DIR"
[ -f "$HOME/.local/sayitlocally-dev-env.sh" ] && source "$HOME/.local/sayitlocally-dev-env.sh"

INSTALL=0
if [[ "${1:-}" == "--install" ]]; then
  INSTALL=1
  shift
fi

# L-92：不走 Apple 公證；清掉所有 APPLE_* 變數（簽章維持 tauri.macos.conf.json 的 ad hoc「-」）
for name in ${(k)parameters[(I)APPLE_*]}; do
  unset "$name"
done

# L-84～L-86、L-88～L-90：離線與不回報設定（子程序、巢狀 npm 都會繼承）
export npm_config_offline=true
export npm_config_audit=false
export npm_config_fund=false
export npm_config_update_notifier=false
export CARGO_NET_OFFLINE=true
export RUSTUP_AUTO_INSTALL=0

# L-91：檢查工具是否已安裝，只讀不補裝
missing=()
command -v node >/dev/null || missing+=(node)
command -v npm >/dev/null || missing+=(npm)
command -v cargo >/dev/null || missing+=(cargo)
command -v cmake >/dev/null || missing+=(cmake)
/usr/bin/xcode-select -p >/dev/null 2>&1 || missing+=("Xcode Command Line Tools")
if (( ${#missing} )); then
  echo "缺少：${(j:、:)missing}。請先自行安裝；本腳本不會下載或補裝。" >&2
  exit 1
fi

if (( INSTALL )); then
  npm ci --offline --no-audit --no-fund --include=optional
fi

# L-87：esbuild 找不到平台 binary 時會自行下載；先確認它已在本機
case "$(uname -m)" in
  arm64) ESBUILD_PKG=darwin-arm64 ;;
  x86_64) ESBUILD_PKG=darwin-x64 ;;
  *) echo "不支援的架構：$(uname -m)" >&2; exit 1 ;;
esac
if [[ ! -x "node_modules/@esbuild/$ESBUILD_PKG/bin/esbuild" ]]; then
  echo "找不到 node_modules/@esbuild/$ESBUILD_PKG/bin/esbuild（optional 套件沒裝上）。" >&2
  echo "停止建置，避免 esbuild 自行連網下載。" >&2
  exit 1
fi

npx --no-install tauri build "$@" -- --locked
