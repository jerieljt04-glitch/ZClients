#!/bin/bash
# Creates the stable local code-signing identity "ZClients Local Signer" that
# ./build.sh signs with. Run it once per Mac; afterwards it does nothing.
#
# Why: an ad-hoc signature has no stable identity, so macOS treats every
# rebuild as a new app — camera/microphone grants (Meet) and the vault key's
# Keychain "Always Allow" are lost each time. One self-signed certificate,
# used for every build, keeps them.
#
# The certificate lives in its own keychain
# (~/Library/Keychains/zclients-codesign.keychain-db). macOS asks you to choose
# that keychain's password; nothing stores it. build.sh never unlocks it: if a
# build says the keychain is locked, run
#   security unlock-keychain ~/Library/Keychains/zclients-codesign.keychain-db
# Releases are signed with Developer ID by scripts/release.sh, not with this.
set -euo pipefail

IDENTITY="ZClients Local Signer"
KEYCHAIN="$HOME/Library/Keychains/zclients-codesign.keychain-db"
KEYCHAIN_SHORT="zclients-codesign.keychain"

if security find-certificate -c "$IDENTITY" "$KEYCHAIN" >/dev/null 2>&1; then
  echo "'$IDENTITY' already exists — nothing to do."
  exit 0
fi

echo "==> Creating signing identity '$IDENTITY'…"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/zclients-signer.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
# The PKCS#12 file only carries the key into the keychain inside $WORK, so a
# one-time random transfer password is enough.
P12_PASS="$(openssl rand -hex 16)"

# LibreSSL (macOS's openssl) has no -addext, so the code-signing extension
# goes through a config file.
cat > "$WORK/openssl.cnf" <<CNF
[ req ]
distinguished_name = dn
x509_extensions    = v3_code_signing
prompt             = no

[ dn ]
CN = $IDENTITY
O  = ZClients

[ v3_code_signing ]
basicConstraints     = critical,CA:false
keyUsage             = critical,digitalSignature
extendedKeyUsage     = critical,codeSigning
subjectKeyIdentifier = hash
CNF

openssl req -x509 -newkey rsa:2048 -nodes -days 7300 \
  -config "$WORK/openssl.cnf" \
  -keyout "$WORK/signer.key" -out "$WORK/signer.crt" 2>/dev/null
openssl pkcs12 -export \
  -inkey "$WORK/signer.key" -in "$WORK/signer.crt" \
  -name "$IDENTITY" -out "$WORK/signer.p12" -passout "pass:$P12_PASS"

if [ ! -f "$KEYCHAIN" ]; then
  echo "    Choose a password for the signing keychain (macOS asks twice):"
  security create-keychain "$KEYCHAIN_SHORT"
fi
echo "    Unlock the signing keychain:"
security unlock-keychain "$KEYCHAIN"
# No -t and no -l: the keychain does not re-lock on a timer or on sleep, so
# rebuilds in the same login session don't stop for a password.
security set-keychain-settings "$KEYCHAIN"

# -T grants only codesign access to the private key (not -A, which would let
# any binary use it).
security import "$WORK/signer.p12" -k "$KEYCHAIN" -P "$P12_PASS" -T /usr/bin/codesign

# Without this, every signing pops an "allow access to key" panel.
echo "    Once more, the signing keychain's password (lets codesign use the key):"
security set-key-partition-list -S apple-tool:,apple:,codesign: -s "$KEYCHAIN" >/dev/null

# Add our keychain to the search list, keeping every one already there.
# `security list-keychains` prints each path indented and quoted; the quotes
# are formatting, so strip them and pass the paths as an array.
CURRENT=()
while IFS= read -r line; do
  line="${line#"${line%%[![:space:]]*}"}"
  line="${line%\"}"
  line="${line#\"}"
  [ -n "$line" ] && CURRENT+=("$line")
done < <(security list-keychains -d user)

LISTED=0
for existing in ${CURRENT+"${CURRENT[@]}"}; do
  [ "$existing" = "$KEYCHAIN" ] && LISTED=1
done
if [ "$LISTED" -eq 0 ]; then
  security list-keychains -d user -s ${CURRENT+"${CURRENT[@]}"} "$KEYCHAIN" >/dev/null
fi

echo "Created '$IDENTITY' in $KEYCHAIN. ./build.sh now signs with it."
