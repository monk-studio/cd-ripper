#!/usr/bin/env bash
# Builds the macOS release: CDR.app signed with Developer ID and the hardened
# runtime, inside a signed, notarized and stapled DMG.
#
#   dev/release-macos.sh              everything, including notarization
#   dev/release-macos.sh --no-notary  stop after signing (no Apple upload)
#
# Notarization reads Apple credentials from a keychain profile, created once
# by the developer (never stored in this repo):
#
#   xcrun notarytool store-credentials cdr-notary --apple-id <id> --team-id 5FP33J8924
#
# NOTARY_PROFILE and SIGN_IDENTITY override the defaults below.
set -euo pipefail

cd "$(dirname "$0")/../crates/cdr-app"
IDENTITY="${SIGN_IDENTITY:-Developer ID Application: Wen Duan (5FP33J8924)}"
PROFILE="${NOTARY_PROFILE:-cdr-notary}"
VERSION=$(python3 -I -c 'import json; print(json.load(open("nsh.json"))["version"])')
APP=.nsh/out/CDR.app
OUT=../../dist
DMG="$OUT/CDR-$VERSION.dmg"

echo "== package"
rm -rf "$APP"
nsh package --target macos

echo "== sign the app (hardened runtime, secure timestamp)"
codesign --force --options runtime --timestamp --sign "$IDENTITY" "$APP"
codesign --verify --strict --deep "$APP"

echo "== disk image"
mkdir -p "$OUT"
STAGE=$(mktemp -d)
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
hdiutil create -quiet -volname "CDR $VERSION" -srcfolder "$STAGE" -format UDZO "$DMG"
rm -rf "$STAGE"
codesign --force --timestamp --sign "$IDENTITY" "$DMG"

if [ "${1:-}" = "--no-notary" ]; then
  echo "Signed, not notarized: $DMG"
  exit 0
fi

echo "== notarize (uploads the DMG to Apple)"
xcrun notarytool submit "$DMG" --keychain-profile "$PROFILE" --wait
xcrun stapler staple "$DMG"

echo "== verify"
xcrun stapler validate "$DMG"
spctl --assess --type open --context context:primary-signature -vv "$DMG"
echo "Release ready: $DMG"
