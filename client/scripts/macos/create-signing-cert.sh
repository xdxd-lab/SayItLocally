#!/bin/zsh
# 在「登入」鑰匙圈裡建立一張自簽名的程式碼簽署憑證（預設名「SayItLocally Self-Signed」，有效期 10 年）。
#
# 為什麼要它：macOS 的「輔助功能」授權繫結在程式碼簽名上。ad-hoc 簽名每次構建都是新身份，
# 使用者每次更新後都得重新勾選；用同一張憑證籤每個版本，授權會一直保留。
# 自簽署憑證不能公證，使用者第一次開啟仍要在「隱私與安全性」裡點「仍要開啟」（見 dev-docs/macos-port/README.md）。
#
# 用法：scripts/macos/create-signing-cert.sh ["憑證名"]
# 過程中系統可能彈窗要登入密碼（把憑證設為「程式碼簽名時始終信任」）。
# ⚠️ 憑證和私鑰只在這台 Mac 的鑰匙圈裡；換機器發版前用「鑰匙圈存取」匯出 .p12 備份，
#    丟了就只能換新憑證，所有使用者要重新授權一次。
set -euo pipefail

NAME="${1:-SayItLocally Self-Signed}"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"

if security find-certificate -c "$NAME" "$KEYCHAIN" >/dev/null 2>&1; then
  echo "鑰匙圈裡已經有「$NAME」，不重複建立。"
  security find-identity -p codesigning "$KEYCHAIN" | grep "$NAME" || true
  exit 0
fi

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

cat > "$WORK/cert.cnf" <<EOF
[ req ]
distinguished_name = dn
x509_extensions = ext
prompt = no
[ dn ]
CN = $NAME
[ ext ]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
EOF

/usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
  -config "$WORK/cert.cnf" -keyout "$WORK/key.pem" -out "$WORK/cert.pem" >/dev/null 2>&1

PASS=$(/usr/bin/openssl rand -hex 16)
/usr/bin/openssl pkcs12 -export -name "$NAME" \
  -inkey "$WORK/key.pem" -in "$WORK/cert.pem" \
  -out "$WORK/cert.p12" -passout "pass:$PASS"

security import "$WORK/cert.p12" -k "$KEYCHAIN" -P "$PASS" -T /usr/bin/codesign

echo "把憑證設為「程式碼簽名時始終信任」，系統可能要求輸入登入密碼……"
security add-trusted-cert -r trustRoot -p codeSign -k "$KEYCHAIN" "$WORK/cert.pem"

echo
echo "完成。可用的簽名身份："
security find-identity -v -p codesigning "$KEYCHAIN" | grep "$NAME" || {
  echo "⚠️ 沒在有效身份裡看到「$NAME」，請開啟「鑰匙圈存取」確認這張憑證的信任設定。"
  exit 1
}
