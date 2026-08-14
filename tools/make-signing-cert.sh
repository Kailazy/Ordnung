#!/usr/bin/env bash
# Create a local, self-signed code-signing identity for Ordnung. Run once.
#
#   bash tools/make-signing-cert.sh
#
# Why this exists
# ---------------
# `tools/build-app.sh` used to ad-hoc sign the bundle (`codesign --sign -`).
# An ad-hoc signature has no certificate, so its designated requirement is just
# the code directory hash:
#
#     designated => cdhash H"aa04996395..."
#
# macOS keys TCC grants (Desktop / Documents / Downloads / removable volumes)
# to that requirement. Every rebuild changes the binary, which changes the
# cdhash, which invalidates every grant — so the app asks for Desktop access
# again after each `make app`.
#
# Signing with a real certificate instead makes the requirement:
#
#     designated => identifier "app.ordnung.gui" and certificate leaf H"..."
#
# The bundle id and the certificate are both stable across rebuilds, so the
# permissions stick. The certificate is self-signed and lives only in this
# login keychain — it is for local persistence, not distribution. Release DMGs
# built in CI have no keychain and fall back to ad-hoc signing as before.
#
# To undo: delete "Ordnung Local Signing" from Keychain Access (login keychain,
# both the certificate and its private key).

set -euo pipefail

name="Ordnung Local Signing"
keychain="$HOME/Library/Keychains/login.keychain-db"

if security find-identity -v -p codesigning 2>/dev/null | grep -qF "$name"; then
  echo "Identity \"$name\" already exists — nothing to do."
  security find-identity -v -p codesigning | grep -F "$name"
  exit 0
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

echo "==> Generating self-signed code-signing certificate"
openssl req -x509 -newkey rsa:2048 -sha256 -days 7300 -nodes \
  -keyout "$work/key.pem" -out "$work/cert.pem" \
  -subj "/CN=$name" \
  -addext "basicConstraints=critical,CA:false" \
  -addext "keyUsage=critical,digitalSignature" \
  -addext "extendedKeyUsage=critical,codeSigning" >/dev/null 2>&1

openssl pkcs12 -export -inkey "$work/key.pem" -in "$work/cert.pem" \
  -out "$work/cert.p12" -passout pass: -name "$name" >/dev/null

echo "==> Importing into the login keychain"
# -T /usr/bin/codesign + -A let codesign use the key without an ACL prompt.
security import "$work/cert.p12" -k "$keychain" -P "" -T /usr/bin/codesign -A

echo "==> Trusting it for code signing (may ask for your login password)"
# User-domain trust only; this does not touch the system trust store.
security add-trusted-cert -r trustRoot -p codeSign -k "$keychain" "$work/cert.pem"

# Without a partition list, codesign pops a keychain prompt on every build.
# Needs the login keychain password; skip it and just click "Always Allow" the
# first time if you'd rather not type it here.
echo
read -r -s -p "Login keychain password (blank to skip, then click Always Allow once): " pw
echo
if [[ -n "$pw" ]]; then
  security set-key-partition-list -S apple-tool:,apple:,codesign: -s \
    -k "$pw" "$keychain" >/dev/null 2>&1 && echo "==> Key partition list set"
fi
unset pw

echo
security find-identity -v -p codesigning | grep -F "$name" || {
  echo "Certificate imported but not showing as a valid signing identity." >&2
  echo "Open Keychain Access, find \"$name\", and set Trust → Code Signing to" >&2
  echo "\"Always Trust\"." >&2
  exit 1
}

echo
echo "Done. Now run:  make app"
echo "You'll be asked for Desktop access once more (the identity changed), and"
echo "that grant will survive every rebuild after it."
