#!/bin/sh
# Usage: ./release.sh
#   Build, sign with Developer ID, notarize and staple the app and its DMG
#   (`asc publish-web cdr --skip-upload`), publish the DMG as GitHub release
#   v<version>, then update the cask in the Homebrew tap (monk-studio/homebrew-tap,
#   checked out next to this repo, or at $TAP). The version is nsh.json's.
set -e
cd "$(dirname "$0")"

VERSION=$(plutil -extract version raw crates/cdr-app/nsh.json)
TAG="v$VERSION"
DMG="crates/cdr-app/.nsh/asc/website/CDR-$VERSION.dmg"
TAP="${TAP:-../homebrew-tap}"

# The release is built from HEAD, which is pushed and tagged.
if [ -n "$(git status --porcelain)" ]; then
    echo "Commit your changes first: the release is built from HEAD." >&2
    exit 1
fi
if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
    echo "$TAG already exists: bump the version in crates/cdr-app/nsh.json." >&2
    exit 1
fi
if [ ! -f "$TAP/Casks/cdr.rb" ]; then
    echo "No tap at $TAP: git clone git@github.com:monk-studio/homebrew-tap.git $TAP" >&2
    exit 1
fi
git -C "$TAP" pull -q --ff-only
git push

asc publish-web cdr --skip-upload
spctl --assess --type open --context context:primary-signature --verbose "$DMG"

gh release create "$TAG" "$DMG" --target "$(git rev-parse HEAD)" --title "CDR $VERSION" \
    --notes "Signed and notarized build for macOS 14+. Open the DMG and drag CDR to Applications."

# Update the cask in the tap and publish it
SHA=$(shasum -a 256 "$DMG" | cut -d' ' -f1)
sed -i '' -e "s/^  version \".*\"/  version \"$VERSION\"/" \
    -e "s/^  sha256 \".*\"/  sha256 \"$SHA\"/" "$TAP/Casks/cdr.rb"
git -C "$TAP" commit -q -m "Update cdr to $VERSION" Casks/cdr.rb
git -C "$TAP" push -q
echo "Released $TAG"
