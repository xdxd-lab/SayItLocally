#!/bin/zsh
# [自訂] SC-23（L-84～L-92）：離線建置本機自用的 SayItLocally.app／.dmg。
#
# 目的：盡量讓建置過程不連網，也不把任何東西送出本機。
#   - npm：只用本機快取（--offline），不做 audit、不查新版、不顯示 fund（L-84～L-86）
#   - esbuild：安裝時先不跑任何安裝腳本，確認平台 binary 已在本機後，才用
#     ESBUILD_BINARY_PATH 跑 esbuild 的 postinstall，讓它不會自行下載（L-87）
#   - Cargo：CARGO_NET_OFFLINE＋--locked，不更新索引、不下載 crate（L-88、L-89）
#   - rustup：要求 1.28.1 以上，並設定 RUSTUP_AUTO_INSTALL=0，不自動補裝工具鏈（L-90）
#   - Xcode／CLT、SDK、CMake：只檢查是否可用，缺少就停下，不觸發系統補裝（L-91）
#   - Apple 公證：清除所有 APPLE_* 變數，只用 ad hoc 簽章（L-92）
# 這個腳本只降低連網的機會，不保證零外連；第一次執行請同時用 LuLu 封鎖外連，
# 並對照 security/build-offline.md 的主機清單（REV2 覆核意見）。
#
# 用法（在 client/ 目錄）：scripts/macos/build-offline.sh [--install] [--bundles <app,dmg>] [--debug] [--verbose]
#   --install  先用本機快取安裝 node_modules（npm ci）
# 其他 tauri build 參數一律拒絕：自訂 config 或 runner 可能繞過這裡的離線設定。
# 快取不完整時建置會直接失敗，不會改去下載；屆時請先決定要不要連網補齊。
set -euo pipefail

SCRIPT_DIR=${0:A:h}
CLIENT_DIR=${SCRIPT_DIR:h:h}
cd "$CLIENT_DIR"

# L-84～L-86、L-88～L-90：離線與不回報設定（子程序、巢狀 npm 都會繼承）
apply_offline_env() {
  export npm_config_offline=true
  export npm_config_audit=false
  export npm_config_fund=false
  export npm_config_update_notifier=false
  export CARGO_NET_OFFLINE=true
  export RUSTUP_AUTO_INSTALL=0
  # L-92：不走 Apple 公證；清掉所有 APPLE_* 變數（簽章維持 tauri.macos.conf.json 的 ad hoc「-」）
  local name
  for name in ${(k)parameters[(I)APPLE_*]}; do
    unset "$name"
  done
}

# 先套用離線設定，再讀本機環境檔（只應設定 PATH），讀完再套用一次，避免被環境檔改掉。
apply_offline_env
[ -f "$HOME/.local/sayitlocally-dev-env.sh" ] && source "$HOME/.local/sayitlocally-dev-env.sh"
apply_offline_env

INSTALL=0
TAURI_ARGS=()
while (( $# )); do
  case "$1" in
    --install) INSTALL=1 ;;
    --bundles|-b)
      (( $# >= 2 )) || { echo "$1 後面要接 bundle 種類，例如 app 或 app,dmg" >&2; exit 2; }
      [[ "$2" == (app|dmg|app,dmg|dmg,app) ]] || { echo "只接受 app、dmg：$2" >&2; exit 2; }
      TAURI_ARGS+=("$1" "$2"); shift ;;
    --debug|-d|--verbose|-v) TAURI_ARGS+=("$1") ;;
    *) echo "不接受的參數：$1（自訂 config、runner 等可能繞過離線設定）" >&2; exit 2 ;;
  esac
  shift
done

# L-90、L-91：檢查工具是否可用，只讀不補裝
missing=()
command -v node >/dev/null || missing+=(node)
command -v npm >/dev/null || missing+=(npm)
command -v rustup >/dev/null || missing+=(rustup)
command -v cmake >/dev/null || missing+=(cmake)
/usr/bin/xcode-select -p >/dev/null 2>&1 || missing+=("Xcode Command Line Tools")
if (( ${#missing} )); then
  echo "缺少：${(j:、:)missing}。請先自行安裝；本腳本不會下載或補裝。" >&2
  exit 1
fi
rustup_ver=$(rustup --version 2>/dev/null | awk '{print $2}')
if ! printf '1.28.1\n%s\n' "$rustup_ver" | sort -V -C; then
  echo "rustup $rustup_ver 太舊（需要 1.28.1 以上才認得 RUSTUP_AUTO_INSTALL=0），停止以免自動補裝工具鏈。" >&2
  exit 1
fi
rustc --version >/dev/null 2>&1 || { echo "目前的 Rust 工具鏈不可用（不會自動補裝）。" >&2; exit 1; }
cargo --version >/dev/null 2>&1 || { echo "cargo 不可用（不會自動補裝）。" >&2; exit 1; }
/usr/bin/xcrun --sdk macosx --show-sdk-path >/dev/null 2>&1 || { echo "找不到 macOS SDK（不會自動補裝）。" >&2; exit 1; }

# L-87：esbuild 平台套件名稱依 Node 的架構判斷（不是 uname，避免 Rosetta 下不一致）
case "$(node -p process.arch)" in
  arm64) ESBUILD_PKG=darwin-arm64 ;;
  x64) ESBUILD_PKG=darwin-x64 ;;
  *) echo "不支援的 Node 架構：$(node -p process.arch)" >&2; exit 1 ;;
esac
ESBUILD_BIN="node_modules/@esbuild/$ESBUILD_PKG/bin/esbuild"

if (( INSTALL )); then
  # 先不跑任何安裝腳本，避免 esbuild 的 postinstall 在找不到 binary 時自行下載
  npm ci --offline --no-audit --no-fund --include=dev --include=optional --ignore-scripts
fi

if [[ ! -x "$ESBUILD_BIN" ]]; then
  echo "找不到 $ESBUILD_BIN（optional 套件沒裝上）。停止建置，避免 esbuild 自行連網下載。" >&2
  exit 1
fi

if (( INSTALL )); then
  # binary 已確認在本機；指定 ESBUILD_BINARY_PATH 後，esbuild 的 postinstall 不會下載
  ESBUILD_BINARY_PATH="$PWD/$ESBUILD_BIN" npm rebuild esbuild --offline
fi

# 直接呼叫專案內的 Tauri CLI，不經 npx（npx 可能改用全域或快取的 CLI）
TAURI_BIN="node_modules/.bin/tauri"
[[ -x "$TAURI_BIN" ]] || { echo "找不到專案內的 Tauri CLI：$TAURI_BIN" >&2; exit 1; }
"$TAURI_BIN" build "${TAURI_ARGS[@]}" -- --locked
