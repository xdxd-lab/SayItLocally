#!/bin/zsh
# 構建 macOS 釋出包：
#   - SayItLocally_<版本>_<架構>.dmg  給使用者下載安裝
#   - SayItLocally_<版本>_<架構>.zip  + latest.yml  給自動更新（放到伺服器 releases/darwin/<架構>/）
#
# 用法：scripts/macos/build-release.sh
# 環境變數：
#   SAYIT_SIGN_IDENTITY  簽署憑證名，預設「SayItLocally Self-Signed」（先跑 create-signing-cert.sh）；
#                        找不到就退回 ad-hoc 簽名（能用，但使用者每次更新都要重新授權輔助功能）。
set -euo pipefail

SCRIPT_DIR=${0:A:h}
CLIENT_DIR=${SCRIPT_DIR:h:h}
cd "$CLIENT_DIR"
[ -f "$HOME/.local/sayitlocally-dev-env.sh" ] && source "$HOME/.local/sayitlocally-dev-env.sh"

VERSION=$(node -p "require('./package.json').version")
case "$(uname -m)" in
  arm64) ARCH=arm64 ;;
  x86_64) ARCH=x64 ;;
  *) echo "不支援的架構：$(uname -m)"; exit 1 ;;
esac

IDENTITY="${SAYIT_SIGN_IDENTITY:-SayItLocally Self-Signed}"
if security find-identity -v -p codesigning | grep -q "\"$IDENTITY\""; then
  export APPLE_SIGNING_IDENTITY="$IDENTITY"
  echo "簽署憑證：$IDENTITY"
else
  export APPLE_SIGNING_IDENTITY="-"
  echo "⚠️ 沒找到簽署憑證「$IDENTITY」，改用 ad-hoc 簽名。"
  echo "   ad-hoc 簽名的版本每次更新後，使用者都要重新勾選「輔助功能」。先執行 scripts/macos/create-signing-cert.sh。"
fi

npx tauri build --bundles app,dmg

BUNDLE=src-tauri/target/release/bundle
APP="$BUNDLE/macos/SayItLocally.app"
DMG=$(ls -t "$BUNDLE"/dmg/*.dmg | head -n 1)
OUT="dist-macos/$VERSION"
ZIP_NAME="SayItLocally_${VERSION}_${ARCH}.zip"
DMG_NAME="SayItLocally_${VERSION}_${ARCH}.dmg"
mkdir -p "$OUT"

# 釋出物一律從一份不帶擴充套件屬性的乾淨拷貝生成，不直接用構建目錄裡那份 .app：
# 它簽完名以後，Finder 或 iCloud「桌面與文稿」同步還可能往 .app 根目錄寫 com.apple.FinderInfo
# 之類的擴充套件屬性（codesign 管這叫 detritus），codesign --strict 校驗不過，Tauri 打的 dmg 裡也會帶上。
# 簽名本身不依賴擴充套件屬性，去掉不影響簽名。
WORK=$(mktemp -d /tmp/sayit-release.XXXXXX)
MNT="$WORK/mnt"
MOUNTED=0
detach_dmg() {
  local i
  # Spotlight 正在索引時解除安裝會報「資源忙」，等一下再試，最後才強制解除安裝
  for i in 1 2 3; do
    if /usr/bin/hdiutil detach -quiet "$MNT"; then
      MOUNTED=0
      return 0
    fi
    sleep 2
  done
  /usr/bin/hdiutil detach -quiet -force "$MNT"
  MOUNTED=0
}
cleanup() {
  if (( MOUNTED )); then detach_dmg || true; fi
  rm -rf "$WORK"
}
trap cleanup EXIT

/usr/bin/ditto --norsrc --noextattr "$APP" "$WORK/SayItLocally.app"
/usr/bin/codesign --verify --deep --strict "$WORK/SayItLocally.app"

# zip 給自動更新用。ditto 會保留 .app 裡的符號連結和簽名；zip 命令會弄壞簽名。
# --norsrc --noextattr：不把擴充套件屬性打成 ._ 檔案，換別的解壓工具也不會在包裡多出檔案、破壞簽名。
/usr/bin/ditto -c -k --norsrc --noextattr --keepParent "$WORK/SayItLocally.app" "$OUT/$ZIP_NAME"

# dmg 沿用 Tauri 打好的那個（視窗布局、捲圖示、「應用程式」快捷方式都在裡面），
# 只就地清掉裡面 .app 的擴充套件屬性，再壓縮、簽名。就地清理不會在卷裡留下舊檔案的空閒塊，dmg 不會變大。
# xattr -c 清不掉 com.apple.provenance（系統保護，會靜默保留），codesign 也不管它。
/usr/bin/hdiutil convert -quiet "$DMG" -format UDRW -o "$WORK/rw.dmg"
mkdir "$MNT"
/usr/bin/hdiutil attach -quiet -nobrowse -noautoopen -noverify -mountpoint "$MNT" "$WORK/rw.dmg"
MOUNTED=1
/usr/bin/xattr -cr "$MNT/SayItLocally.app"
/usr/bin/codesign --verify --deep --strict "$MNT/SayItLocally.app"
# 掛載期間系統寫進卷裡的事件日誌，和 Tauri 打包時一樣刪掉（可能刪不掉，不影響使用）
rm -rf "$MNT/.fseventsd" 2>/dev/null || true
detach_dmg
/usr/bin/hdiutil convert -quiet "$WORK/rw.dmg" -format UDZO -ov -o "$OUT/$DMG_NAME"
# ad-hoc 時不籤 dmg：dmg 本身不需要簽名，要緊的是裡面的 .app
if [ "$APPLE_SIGNING_IDENTITY" != "-" ]; then
  /usr/bin/codesign --force --sign "$APPLE_SIGNING_IDENTITY" "$OUT/$DMG_NAME"
  /usr/bin/codesign --verify "$OUT/$DMG_NAME"
fi

# manifest 裡的 sha512 是 Base64（與 Windows 的 latest.yml 相同，客戶端按 Base64 比對）
SHA512=$(/usr/bin/openssl dgst -sha512 -binary "$OUT/$ZIP_NAME" | /usr/bin/base64)
SIZE=$(stat -f %z "$OUT/$ZIP_NAME")
DATE=$(date -u +%Y-%m-%dT%H:%M:%S.000Z)
cat > "$OUT/latest.yml" <<EOF
version: $VERSION
files:
  - url: $ZIP_NAME
    sha512: $SHA512
    size: $SIZE
path: $ZIP_NAME
sha512: $SHA512
releaseDate: '$DATE'
EOF

echo
echo "產物在 $CLIENT_DIR/$OUT ："
ls -la "$OUT"
echo
echo "釋出："
echo "  1. $DMG_NAME 上傳到 GitHub Release，給使用者下載"
echo "  2. $ZIP_NAME 和 latest.yml 放到伺服器的 releases/darwin/$ARCH/ 下，客戶端自動更新從這裡取"
