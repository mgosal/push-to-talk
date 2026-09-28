#!/usr/bin/env bash
# Create a self-signed code-signing identity in the login keychain, once.
#
# An ad-hoc signature (`codesign --sign -`) identifies the app by the hash of
# its binary, so every rebuild looks like a new app to macOS privacy controls
# and the Accessibility and Microphone grants silently stop applying. Signing
# with a certificate instead identifies it by bundle id + certificate, which
# survives rebuilds — grant permissions once and they stick.
#
# The certificate never leaves this machine and is not added to any trust
# settings; codesign only needs the private key.
set -euo pipefail

NAME="${1:-Push to Talk Local Signing}"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"

if security find-certificate -c "$NAME" "$KEYCHAIN" >/dev/null 2>&1; then
    echo "✓ Signing identity \"$NAME\" already exists"
    exit 0
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

cat >"$tmp/cert.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $NAME
[ext]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
EOF

openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
    -config "$tmp/cert.cnf" -keyout "$tmp/key.pem" -out "$tmp/cert.pem" 2>/dev/null

# -legacy: macOS `security` can't read OpenSSL 3's default PKCS#12 encryption.
pass="ptt-$(date +%s)"
openssl pkcs12 -export -legacy -inkey "$tmp/key.pem" -in "$tmp/cert.pem" \
    -name "$NAME" -passout "pass:$pass" -out "$tmp/identity.p12" 2>/dev/null \
    || openssl pkcs12 -export -inkey "$tmp/key.pem" -in "$tmp/cert.pem" \
        -name "$NAME" -passout "pass:$pass" -out "$tmp/identity.p12"

# -T lets codesign use the key without a keychain prompt on every build.
security import "$tmp/identity.p12" -k "$KEYCHAIN" -P "$pass" \
    -T /usr/bin/codesign >/dev/null

echo "✓ Created signing identity \"$NAME\""
