#!/bin/bash
# Production release: Developer ID signed, hardened runtime, secure
# timestamp, notarized and stapled. Refuses to produce anything else.
#
# Needs (from CI secrets or your login keychain — never from files or the
# command line):
#   DEVELOPER_ID     "Developer ID Application: <Name> (<TEAMID>)"
#   NOTARY_PROFILE   a `xcrun notarytool store-credentials` profile name
#   TEAM_ID          the expected 10-character Team ID
#
#   scripts/release.sh            → dist/ZClients-<version>.zip (+ .sha256, SBOM)
set -euo pipefail
cd "$(dirname "$0")/.."
: "${DEVELOPER_ID:?set DEVELOPER_ID to your Developer ID Application identity}"
: "${NOTARY_PROFILE:?set NOTARY_PROFILE to a notarytool keychain profile}"
: "${TEAM_ID:?set TEAM_ID}"
[[ "$DEVELOPER_ID" == "Developer ID Application:"* ]] || { echo "release signing needs a Developer ID Application identity" >&2; exit 1; }
security find-identity -v -p codesigning | grep -q "$DEVELOPER_ID" || { echo "identity not found / not valid: $DEVELOPER_ID" >&2; exit 1; }

BUNDLE_ID="com.zport.swift"
scripts/check.sh

STAGE="$(mktemp -d "${TMPDIR:-/tmp}/zclients-release.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT
APP="$STAGE/ZClients.app"
VERSION="$(scripts/bundle.sh "$APP" ZClients "$BUNDLE_ID" | cut -d' ' -f1)"
xattr -cr "$APP"
codesign --force --options runtime --timestamp --identifier "$BUNDLE_ID" \
  --entitlements ZClients.entitlements --sign "$DEVELOPER_ID" "$APP"
scripts/verify-signature.sh "$APP" "$BUNDLE_ID" --developer-id
codesign -dvv "$APP" 2>&1 | grep -q "^TeamIdentifier=$TEAM_ID$" || { echo "wrong Team ID" >&2; exit 1; }

mkdir -p dist
ZIP="dist/ZClients-$VERSION.zip"
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"
xcrun notarytool submit "$ZIP" --keychain-profile "$NOTARY_PROFILE" --wait --output-format json > "$STAGE/notary.json"
python3 -c 'import json,sys; s=json.load(open(sys.argv[1]))["status"]; sys.exit(0 if s=="Accepted" else f"notarization: {s}")' "$STAGE/notary.json"
xcrun stapler staple "$APP"
xcrun stapler validate "$APP"
spctl --assess --type execute --verbose=4 "$APP"
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"
shasum -a 256 "$ZIP" > "$ZIP.sha256"
scripts/sbom.sh > "dist/ZClients-$VERSION.sbom.json"
echo "==> $ZIP (notarized, stapled), $ZIP.sha256, dist/ZClients-$VERSION.sbom.json"
