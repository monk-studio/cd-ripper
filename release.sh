#!/bin/sh
# Usage: ./release.sh
#   Build, sign with Developer ID, notarize and staple the app and its DMG
#   (`asc publish-web cdr --skip-upload`), publish the DMG as GitLab release
#   v<version>, then update the Homebrew cask. The version is nsh.json's.
set -e
cd "$(dirname "$0")"

VERSION=$(plutil -extract version raw crates/cdr-app/nsh.json)
TAG="v$VERSION"
DMG="crates/cdr-app/.nsh/asc/website/CDR-$VERSION.dmg"

# The release is built from HEAD, which must be on GitLab for --ref.
if [ -n "$(git status --porcelain)" ]; then
    echo "Commit your changes first: the release is built from HEAD." >&2
    exit 1
fi
if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
    echo "$TAG already exists: bump the version in crates/cdr-app/nsh.json." >&2
    exit 1
fi
git push

asc publish-web cdr --skip-upload
spctl --assess --type open --context context:primary-signature --verbose "$DMG"

glab release create "$TAG" "$DMG" --name "CDR $VERSION" --ref "$(git rev-parse HEAD)" \
    --notes "Signed and notarized build for macOS 14+. Open the DMG and drag CDR to Applications."

# Update the Homebrew cask (this repo doubles as the tap) and publish it
SHA=$(shasum -a 256 "$DMG" | cut -d' ' -f1)
sed -i '' -e "s/^  version \".*\"/  version \"$VERSION\"/" \
    -e "s/^  sha256 \".*\"/  sha256 \"$SHA\"/" Casks/cdr.rb
git add Casks/cdr.rb
git commit -m "Update cask to $VERSION"
git push
echo "Released $TAG"
