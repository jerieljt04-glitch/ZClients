#!/bin/bash
# Build ZClients (Rust) into a signed, universal .app.
#
#   ./build.sh            build, sign and install to /Applications/ZClients.app
#   ./build.sh --no-install   build and sign only (.build/ZClients.app)
#   ./build.sh --test     build "ZClients Test" (bundle id com.zport.swift.test,
#                         its own data) into .build/ — never touches real data
#
# The real build keeps the Swift app's bundle id (com.zport.swift) and signing
# identity, so every existing profile, tab and sign-in carries over.
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
VERSION="1.1.0"
BUILD_NUMBER="10"
OUT=".build"
# Assemble and sign off the Desktop: iCloud/File Provider attaches extended
# attributes there that codesign rejects.
STAGE="$(mktemp -d "${TMPDIR:-/tmp}/zclients-build.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT
APP="$STAGE/$APP_NAME.app"
INSTALL_APP="${ZCLIENTS_INSTALL_APP:-/Applications/ZClients.app}"

echo "==> Running tests…"
cargo test --quiet 2>&1 | grep -E "test result|FAILED|panicked" || true
if ! cargo test --quiet >/dev/null 2>&1; then
  echo "Tests failed; not building." >&2
  exit 1
fi

echo "==> Compiling (Apple Silicon + Intel)…"
rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null 2>&1 || true
export MACOSX_DEPLOYMENT_TARGET=14.0
cargo build --release --quiet --target aarch64-apple-darwin
cargo build --release --quiet --target x86_64-apple-darwin
mkdir -p "$OUT"
lipo -create target/aarch64-apple-darwin/release/zclients-rs target/x86_64-apple-darwin/release/zclients-rs -output "$STAGE/ZClients"

echo "==> Assembling $APP_NAME.app…"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
mv "$STAGE/ZClients" "$APP/Contents/MacOS/ZClients"

# Keep the app's familiar icon: from the Electron tree, the Swift install, or
# the Swift release zips, whichever is still around.
for ICON in ../zoho-workspace-manager/build/ZPort.icns "$INSTALL_APP/Contents/Resources/ZClients.icns" backup/ZClients-Swift.app/Contents/Resources/ZClients.icns; do
  if [ -f "$ICON" ]; then cp "$ICON" "$APP/Contents/Resources/ZClients.icns"; break; fi
done

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$APP_NAME</string>
  <key>CFBundleDisplayName</key><string>$APP_NAME</string>
  <key>CFBundleExecutable</key><string>ZClients</string>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$BUILD_NUMBER</string>
  <key>CFBundleIconFile</key><string>ZClients</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.business</string>
  <key>NSPrincipalClass</key><string>NSApplication</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticTermination</key><false/>
  <key>NSSupportsSuddenTermination</key><false/>
  <key>NSCameraUsageDescription</key><string>ZClients needs camera access when a Zoho page or Google Meet asks for it.</string>
  <key>NSMicrophoneUsageDescription</key><string>ZClients needs microphone access when a Zoho page or Google Meet asks for it.</string>
</dict>
</plist>
PLIST

# Same stable identity as the Swift build (./setup_signing_identity.sh in
# ../zport-swift). Signing with it every time keeps one code identity, so
# camera/mic grants survive updates and the Keychain asks at most once.
SIGN_IDENTITY="ZClients Local Signer"
SIGN_KEYCHAIN="$HOME/Library/Keychains/zclients-codesign.keychain-db"
SIGN_PASSWORD_FILE="$HOME/Library/Application Support/ZClients Signing/keychain-password"
if security find-certificate -c "$SIGN_IDENTITY" "$SIGN_KEYCHAIN" >/dev/null 2>&1; then
  [ -f "$SIGN_PASSWORD_FILE" ] && security unlock-keychain -p "$(cat "$SIGN_PASSWORD_FILE")" "$SIGN_KEYCHAIN" 2>/dev/null || true
  SIGN_ARGS=(--keychain "$SIGN_KEYCHAIN" --sign "$SIGN_IDENTITY")
  echo "==> Signing as '$SIGN_IDENTITY' (hardened runtime)…"
else
  SIGN_ARGS=(--sign -)
  echo "==> Ad-hoc signing (run ../zport-swift/setup_signing_identity.sh for a stable identity)…"
fi
xattr -cr "$APP" 2>/dev/null || true
codesign --force --options runtime --timestamp=none --identifier "$BUNDLE_ID" \
  --entitlements ZClients.entitlements "${SIGN_ARGS[@]}" "$APP"
codesign --verify --strict "$APP"
echo "    $(du -sh "$APP" | cut -f1) app, binary $(du -h "$APP/Contents/MacOS/ZClients" | cut -f1)"

rm -rf "$OUT/$APP_NAME.app"
ditto --norsrc "$APP" "$OUT/$APP_NAME.app"
if [ "$MODE" != "install" ]; then
  echo "==> Built $OUT/$APP_NAME.app"
  exit 0
fi

if pgrep -x ZClients >/dev/null; then
  echo "ZClients is running. Quit it (⌘Q) and run ./build.sh again." >&2
  exit 1
fi

# Keep the previous app once, so going back is a single copy.
if [ -d "$INSTALL_APP" ] && [ ! -d backup/ZClients-Swift.app ] && \
   /usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$INSTALL_APP/Contents/Info.plist" 2>/dev/null | grep -q '^0\.'; then
  mkdir -p backup
  ditto "$INSTALL_APP" backup/ZClients-Swift.app
  echo "==> Saved the Swift app to backup/ZClients-Swift.app"
fi

echo "==> Installing to ${INSTALL_APP}…"
rm -rf "$INSTALL_APP"
ditto --norsrc "$APP" "$INSTALL_APP"
codesign --verify --strict "$INSTALL_APP"
echo "==> Done. Launch with:  open '$INSTALL_APP'"
