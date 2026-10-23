#!/bin/bash
# Local development builds of ZClients. Production releases are built by
# scripts/release.sh (Developer ID, notarized) — never by this script.
#
#   ./build.sh                build, sign with the local identity, install to
#                             /Applications/ZClients.app (asks first)
#   ./build.sh --no-install   build and sign only (.build/ZClients.app)
#   ./build.sh --test         "ZClients Test" (bundle id com.zport.swift.test,
#                             its own data), ad-hoc signed, into .build/ —
#                             never touches real data or any keychain
#
# The real local build keeps the bundle id com.zport.swift and the
# "ZClients Local Signer" identity, so profiles, sign-ins and camera grants
# carry over. This script never unlocks a keychain and never reads a
# password: if the signing keychain is locked, unlock it yourself first
# (`security unlock-keychain ~/Library/Keychains/zclients-codesign.keychain-db`
# prompts for the password) or the build is signed ad hoc.
set -euo pipefail
cd "$(dirname "$0")"
source "$HOME/.cargo/env" 2>/dev/null || true

MODE="install"
case "${1:-}" in
  --test) MODE="test" ;;
  --no-install) MODE="build" ;;
  "") ;;
  *) echo "unknown option $1"; exit 2 ;;
esac

APP_NAME="ZClients"
BUNDLE_ID="com.zport.swift"
if [ "$MODE" = "test" ]; then
  APP_NAME="ZClients Test"
  BUNDLE_ID="com.zport.swift.test"
fi
OUT=".build"
# Assemble and sign off the Desktop: iCloud/File Provider attaches extended
# attributes there that codesign rejects.
STAGE="$(mktemp -d "${TMPDIR:-/tmp}/zclients-build.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT
APP="$STAGE/$APP_NAME.app"
INSTALL_APP="${ZCLIENTS_INSTALL_APP:-/Applications/ZClients.app}"

echo "==> Running tests…"
if ! cargo test --locked --quiet; then
  echo "Tests failed; not building." >&2
  exit 1
fi

echo "==> Compiling (Apple Silicon + Intel) and assembling $APP_NAME.app…"
VERSION="$(scripts/bundle.sh "$APP" "$APP_NAME" "$BUNDLE_ID")"
echo "    version $VERSION"

SIGN_IDENTITY="ZClients Local Signer"
SIGN_KEYCHAIN="$HOME/Library/Keychains/zclients-codesign.keychain-db"
SIGN_ARGS=(--sign -)
if [ "$MODE" = "test" ]; then
  echo "==> Ad-hoc signing the test app (hardened runtime)…"
# The local identity is self-signed, so it is never "valid" (-v) to
# find-identity; its certificate being present is what counts.
elif security find-certificate -c "$SIGN_IDENTITY" "$SIGN_KEYCHAIN" >/dev/null 2>&1; then
  SIGN_ARGS=(--keychain "$SIGN_KEYCHAIN" --sign "$SIGN_IDENTITY")
  echo "==> Signing as '$SIGN_IDENTITY' (hardened runtime, local development identity)…"
else
  echo "==> The local signing keychain is locked or missing: signing ad hoc." >&2
  echo "    (macOS will treat this build as a new app for camera/Keychain access.)" >&2
fi
xattr -cr "$APP" 2>/dev/null || true
codesign --force --options runtime --timestamp=none --identifier "$BUNDLE_ID" \
  --entitlements ZClients.entitlements "${SIGN_ARGS[@]}" "$APP"
scripts/verify-signature.sh "$APP" "$BUNDLE_ID"
echo "    $(du -sh "$APP" | cut -f1) app, binary $(du -h "$APP/Contents/MacOS/ZClients" | cut -f1)"

rm -rf "${OUT:?}/$APP_NAME.app"
mkdir -p "$OUT"
ditto --norsrc "$APP" "$OUT/$APP_NAME.app"
if [ "$MODE" != "install" ]; then
  echo "==> Built $OUT/$APP_NAME.app"
  exit 0
fi

if pgrep -x ZClients >/dev/null; then
  echo "ZClients is running. Quit it (⌘Q) and run ./build.sh again." >&2
  exit 1
fi
read -r -p "Replace $INSTALL_APP with $VERSION? [y/N] " ANSWER
[[ "$ANSWER" =~ ^[Yy]$ ]] || { echo "Not installed. The build is at $OUT/$APP_NAME.app"; exit 0; }

# Keep the app being replaced, so going back is a single copy.
if [ -d "$INSTALL_APP" ]; then
  OLD="$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$INSTALL_APP/Contents/Info.plist" 2>/dev/null || echo unknown)"
  mkdir -p backup
  [ -d "backup/ZClients-$OLD.app" ] || ditto "$INSTALL_APP" "backup/ZClients-$OLD.app"
  echo "==> Kept the previous app at backup/ZClients-$OLD.app"
fi

echo "==> Installing to ${INSTALL_APP}…"
rm -rf "$INSTALL_APP"
ditto --norsrc "$APP" "$INSTALL_APP"
scripts/verify-signature.sh "$INSTALL_APP" "$BUNDLE_ID"
echo "==> Done. Launch with:  open '$INSTALL_APP'"
