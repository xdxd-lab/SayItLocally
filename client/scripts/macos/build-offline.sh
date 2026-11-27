#!/bin/zsh
# [自訂] SC-23（L-84～L-92）：離線建置本機自用的 SayItLocally.app／.dmg。
#
# 目的：盡量讓建置過程不連網，也不把任何東西送出本機。
#   - npm：只用本機快取（--offline），不做 audit、不查新版、不顯示 fund（L-84～L-86）
#   - esbuild：安裝時不跑任何安裝腳本（--ignore-scripts），並確認平台 binary 已在本機；
#     esbuild 的 postinstall 只是加速用，不跑也能正常使用（L-87）
#   - Cargo：CARGO_NET_OFFLINE＋--locked，不更新索引、不下載 crate（L-88、L-89）
#   - Rust：使用 src-tauri/rust-toolchain.toml 固定的版本（SC-64），確認已安裝的工具鏈與標準庫，
#     直接使用該工具鏈的 Cargo、rustc；不讓 rustup 自動下載（L-90）
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
  unset NPM_CONFIG_OFFLINE NPM_CONFIG_AUDIT NPM_CONFIG_FUND NPM_CONFIG_UPDATE_NOTIFIER
  unset NODE_OPTIONS npm_config_node_options NPM_CONFIG_NODE_OPTIONS NPM_CONFIG_SCRIPT_SHELL
  unset CARGO_BUILD_TARGET CMAKE_ARGS TRANSCRIBE_CMAKE_ARGS TAURI_CONFIG
  export npm_config_offline=true
  export npm_config_audit=false
  export npm_config_fund=false
  export npm_config_update_notifier=false
  export npm_config_script_shell=/bin/sh
  export CARGO_NET_OFFLINE=true
  export RUSTC_WRAPPER= RUSTC_WORKSPACE_WRAPPER=
  # L-92：不走 Apple 公證；清掉所有 APPLE_* 變數（簽章維持 tauri.macos.conf.json 的 ad hoc「-」）
  local name
  for name in ${(k)parameters[(I)APPLE_*]}; do
    unset "$name"
  done
}

# L-90／SC-64：工具鏈版本取自 src-tauri/rust-toolchain.toml 的 channel（不用 rustup 的預設），
# 只支援 Apple Silicon，直接選用工具鏈目錄裡的執行檔。
use_local_rust() {
  local rustup_dir="${RUSTUP_HOME:-$HOME/.rustup}" cargo_home="${CARGO_HOME:-$HOME/.cargo}"
  local channel toolchain triple=aarch64-apple-darwin toolchain_bin entry
  local -a stdlibs kept_path
  [[ "$(/usr/bin/uname -m)" == arm64 ]] || { echo "只支援 Apple Silicon（arm64）建置。" >&2; return 1; }
  channel=$(
    /usr/bin/sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)"[[:space:]]*$/\1/p' \
      src-tauri/rust-toolchain.toml 2>/dev/null | /usr/bin/head -n 1
  ) || channel=
  [[ "$channel" =~ '^[0-9]+[.][0-9]+[.][0-9]+$' ]] || { echo "src-tauri/rust-toolchain.toml 沒有固定的 Rust 版本（例如 1.100.0）。" >&2; return 1; }
  toolchain="$channel-$triple"
  toolchain_bin="$rustup_dir/toolchains/$toolchain/bin"
  toolchain_bin=${toolchain_bin:A}
  stdlibs=( "${toolchain_bin:h}/lib/rustlib/$triple/lib"/libstd-*.rlib(N.) )
  if [[ ! -f "$toolchain_bin/cargo" || ! -x "$toolchain_bin/cargo" || -L "$toolchain_bin/cargo" ||
        ! -f "$toolchain_bin/rustc" || ! -x "$toolchain_bin/rustc" || -L "$toolchain_bin/rustc" ||
        "$toolchain_bin/cargo" -ef "$cargo_home/bin/rustup" ||
        "$toolchain_bin/rustc" -ef "$cargo_home/bin/rustup" ]] || (( ! ${#stdlibs} )); then
    echo "Rust $toolchain 沒有安裝完整（缺 Cargo、rustc 或 $triple 標準庫）。本腳本不會下載；請先自行安裝。" >&2
    return 1
  fi
  for entry in "${path[@]}"; do
    [[ ${entry:A} == ${cargo_home:A}/bin || ${entry:A} == ${HOME:A}/.cargo/bin ]] || kept_path+=("$entry")
  done
  path=("$toolchain_bin" "${kept_path[@]}")
  export CARGO="$toolchain_bin/cargo" RUSTC="$toolchain_bin/rustc"
  # 萬一有子程序仍經過 rustup proxy：指定同一個已安裝的工具鏈，並關閉自動安裝。
  export RUSTUP_TOOLCHAIN="$toolchain" RUSTUP_AUTO_INSTALL=0
}

# 直接加入本機 Node 和 CMake，避免執行外部環境檔。
export PATH="$HOME/.local/node/bin:$HOME/.local/cmake/CMake.app/Contents/bin:$PATH"
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

use_local_rust

# L-91：檢查工具是否可用，只讀不補裝
missing=()
command -v node >/dev/null || missing+=(node)
command -v npm >/dev/null || missing+=(npm)
command -v cmake >/dev/null || missing+=(cmake)
/usr/bin/xcode-select -p >/dev/null 2>&1 || missing+=("Xcode Command Line Tools")
if (( ${#missing} )); then
  echo "缺少：${(j:、:)missing}。請先自行安裝；本腳本不會下載或補裝。" >&2
  exit 1
fi
export CMAKE="$(command -v cmake)"
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

# 不執行 esbuild 的 postinstall（那只是用原生 binary 取代 JS 啟動檔的加速步驟）。
# 原本用 ESBUILD_BINARY_PATH 重跑會把絕對路徑寫進 node_modules，搬目錄後就失效（REV3）。

# 直接呼叫專案內的 Tauri CLI，不經 npx（npx 可能改用全域或快取的 CLI）
TAURI_BIN="node_modules/.bin/tauri"
[[ -x "$TAURI_BIN" ]] || { echo "找不到專案內的 Tauri CLI：$TAURI_BIN" >&2; exit 1; }
"$TAURI_BIN" build "${TAURI_ARGS[@]}" -- --locked
