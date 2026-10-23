#!/bin/bash
# Shared by build.sh and scripts/release.sh: build the universal binary and
# assemble an unsigned ZClients .app.
#
#   scripts/bundle.sh <out-app-path> <app-name> <bundle-id>
#
# The version comes from Cargo.toml only: CFBundleShortVersionString is the
# package version, CFBundleVersion is [package.metadata.zclients] build.
set -euo pipefail
cd "$(dirname "$0")/.."
APP="$1"; APP_NAME="$2"; BUNDLE_ID="$3"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
BUILD_NUMBER="$(awk '/^\[package.metadata.zclients\]/{s=1;next} /^\[/{s=0} s && /^build *=/{gsub(/[^0-9]/,"");print}' Cargo.toml)"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "bad version '$VERSION' in Cargo.toml" >&2; exit 1; }
[[ "$BUILD_NUMBER" =~ ^[0-9]+$ ]] || { echo "bad build number in Cargo.toml" >&2; exit 1; }

export MACOSX_DEPLOYMENT_TARGET=14.0
for T in aarch64-apple-darwin x86_64-apple-darwin; do
  rustup target list --installed | grep -qx "$T" || rustup target add "$T" >/dev/null
  cargo build --locked --release --quiet --target "$T"
done
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create target/aarch64-apple-darwin/release/zclients-rs target/x86_64-apple-darwin/release/zclients-rs \
  -output "$APP/Contents/MacOS/ZClients"
# Universal-binary validation: both architectures must be present.
ARCHS="$(lipo -archs "$APP/Contents/MacOS/ZClients")"
[[ "$ARCHS" == *arm64* && "$ARCHS" == *x86_64* ]] || { echo "not universal: $ARCHS" >&2; exit 1; }

for ICON in ../zoho-workspace-manager/build/ZPort.icns /Applications/ZClients.app/Contents/Resources/ZClients.icns backup/ZClients-Swift.app/Contents/Resources/ZClients.icns; do
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
  <key>NSCameraUsageDescription</key><string>ZClients uses the camera when Google Meet, Zoho Meeting or Cliq — or a site you allow — asks for it.</string>
  <key>NSMicrophoneUsageDescription</key><string>ZClients uses the microphone when Google Meet, Zoho Meeting or Cliq — or a site you allow — asks for it.</string>
</dict>
</plist>
PLIST
plutil -lint "$APP/Contents/Info.plist" >/dev/null
echo "$VERSION ($BUILD_NUMBER)"
