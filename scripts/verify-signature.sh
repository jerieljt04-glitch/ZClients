#!/bin/bash
# Fail unless <app> is signed as <bundle-id>, with the hardened runtime and
# exactly the expected entitlements (camera + microphone, nothing else).
#   scripts/verify-signature.sh <app> <bundle-id> [--developer-id]
set -euo pipefail
APP="$1"; BUNDLE_ID="$2"; MODE="${3:-}"
codesign --verify --strict --deep "$APP"
INFO="$(codesign -dvv "$APP" 2>&1)"
grep -q "^Identifier=$BUNDLE_ID$" <<<"$INFO" || { echo "wrong identifier (want $BUNDLE_ID)" >&2; exit 1; }
grep -Eq "flags=.*runtime" <<<"$INFO" || { echo "hardened runtime is off" >&2; exit 1; }
PLIST_ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP/Contents/Info.plist")"
[ "$PLIST_ID" = "$BUNDLE_ID" ] || { echo "Info.plist says $PLIST_ID" >&2; exit 1; }
ENT="$(codesign -d --entitlements - --xml "$APP" 2>/dev/null | plutil -convert json -o - - 2>/dev/null || echo '{}')"
WANT='{"com.apple.security.device.audio-input":true,"com.apple.security.device.camera":true}'
GOT="$(python3 -c 'import json,sys; print(json.dumps(json.loads(sys.argv[1]), sort_keys=True, separators=(",",":")))' "$ENT")"
[ "$GOT" = "$WANT" ] || { echo "unexpected entitlements: $GOT" >&2; exit 1; }
if [ "$MODE" = "--developer-id" ]; then
  grep -q "^Authority=Developer ID Application:" <<<"$INFO" || { echo "not signed with a Developer ID certificate" >&2; exit 1; }
  grep -q "^Timestamp=" <<<"$INFO" || { echo "no secure timestamp" >&2; exit 1; }
fi
echo "signature OK: $BUNDLE_ID"
