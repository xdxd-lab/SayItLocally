#!/bin/zsh
# 用獨立的測試目錄啟動本地構建的 SayItLocally：設定、資料庫、日誌、登入項都寫進測試目錄，
# 不碰你自己的 ~/Library。
#
# 用法：scripts/macos/run-test.sh [release|debug] [--reset]
#   release（預設）：src-tauri/target/release/bundle/macos/SayItLocally.app（build-release.sh 的產物）
#   debug：src-tauri/target/debug/bundle/macos/SayItLocally.app（npx tauri build --debug --bundles app 的產物）
#   --reset：先清空測試目錄，從全新安裝（歡迎引導）開始
#
# 測試目錄預設是倉庫旁邊的 test-home/，可以用環境變數 SAYIT_TEST_HOME 換。
# 用 open 啟動而不是直接執行二進位制：這樣「輔助功能」「麥克風」授權記在 SayItLocally 名下，
# 而不是記在終端名下。
set -euo pipefail

SCRIPT_DIR=${0:A:h}
CLIENT_DIR=${SCRIPT_DIR:h:h}
REPO_DIR=${CLIENT_DIR:h}

PROFILE=release
RESET=0
for arg in "$@"; do
  case "$arg" in
    release|debug) PROFILE=$arg ;;
    --reset) RESET=1 ;;
    *) echo "用法：$0 [release|debug] [--reset]"; exit 1 ;;
  esac
done

APP="$CLIENT_DIR/src-tauri/target/$PROFILE/bundle/macos/SayItLocally.app"
if [ ! -d "$APP" ]; then
  echo "找不到 $APP"
  echo "先构建：release 跑 scripts/macos/build-release.sh，debug 跑 npx tauri build --debug --bundles app"
  exit 1
fi

TEST_HOME="${SAYIT_TEST_HOME:-${REPO_DIR:h}/test-home}"

# 已經在跑的本地構建先退出：否則 open 只會把舊例項調到前台，新的 HOME 不生效
if pgrep -f "$APP/Contents/MacOS/sayitlocally" >/dev/null; then
  echo "退出正在執行的測試例項……"
  pkill -f "$APP/Contents/MacOS/sayitlocally" || true
  sleep 1
fi

# 標記檔案：只清空由本指令碼建的目錄，防止 SAYIT_TEST_HOME 指錯地方時誤刪
MARKER=".sayit-test-home"
if [ "$RESET" = 1 ] && [ -d "$TEST_HOME" ]; then
  if [ ! -f "$TEST_HOME/$MARKER" ]; then
    echo "$TEST_HOME 裡沒有 $MARKER，不像是測試目錄，拒絕清空"
    exit 1
  fi
  echo "清空測試目錄 $TEST_HOME"
  rm -rf -- "$TEST_HOME"
fi
mkdir -p "$TEST_HOME"
touch "$TEST_HOME/$MARKER"

open --env "HOME=$TEST_HOME" "$APP"

echo "已启动：$APP"
echo "測試目錄：$TEST_HOME"
echo "日志：$TEST_HOME/Library/Application Support/com.sayitlocally.desktop/logs/sayit.log"
echo "退出：點選單欄的 SayItLocally 圖示 → 退出"
