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

for ICON in assets/ZClients.icns ../zoho-workspace-manager/build/ZPort.icns /Applications/ZClients.app/Contents/Resources/ZClients.icns backup/ZClients-Swift.app/Contents/Resources/ZClients.icns; do
  if [ -f "$ICON" ]; then cp "$ICON" "$APP/Contents/Resources/ZClients.icns"; break; fi
done

# The same icon as a compiled asset catalog. AppKit and CoreUI look for the
# app's Assets.car (the Print panel, app activation, system state dumps);
# without one every lookup logs "CUICatalog … unable to find a bundle".
ICON_NAME=""
if [ -f "$APP/Contents/Resources/ZClients.icns" ] && xcrun --find actool >/dev/null 2>&1; then
  CAT="$(mktemp -d "${TMPDIR:-/tmp}/zclients-assets.XXXXXX")"
  SET="$CAT/Assets.xcassets/AppIcon.appiconset"
  mkdir -p "$SET"
  iconutil -c iconset -o "$CAT/icon.iconset" "$APP/Contents/Resources/ZClients.icns"
  cp "$CAT"/icon.iconset/*.png "$SET/"
  IMAGES=""
  for F in "$SET"/icon_*.png; do
    B="$(basename "$F")"; SIZE="${B#icon_}"; SIZE="${SIZE%.png}"; SCALE=1x
    [[ "$SIZE" == *@2x ]] && { SIZE="${SIZE%@2x}"; SCALE=2x; }
    IMAGES="$IMAGES${IMAGES:+,}{\"idiom\":\"mac\",\"size\":\"$SIZE\",\"scale\":\"$SCALE\",\"filename\":\"$B\"}"
  done
  echo "{\"images\":[$IMAGES],\"info\":{\"version\":1,\"author\":\"xcode\"}}" > "$SET/Contents.json"
  echo '{"info":{"version":1,"author":"xcode"}}' > "$CAT/Assets.xcassets/Contents.json"
  xcrun actool "$CAT/Assets.xcassets" --compile "$APP/Contents/Resources" --platform macosx \
    --minimum-deployment-target 14.0 --app-icon AppIcon --output-partial-info-plist "$CAT/partial.plist" >/dev/null
  rm -rf "$CAT"
  [ -f "$APP/Contents/Resources/Assets.car" ] && ICON_NAME="<key>CFBundleIconName</key><string>AppIcon</string>"
else
  echo "warning: no actool (Xcode) or icon: the app ships without Assets.car" >&2
fi

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
  $ICON_NAME
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
