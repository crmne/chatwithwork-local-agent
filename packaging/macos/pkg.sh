#!/bin/bash
# Build the macOS installer package.
#
#   packaging/macos/pkg.sh SIGNED_BINARY VERSION OUTPUT.pkg
#
# SIGNED_BINARY is the universal cww, already signed with Developer ID and
# the hardened runtime (native-packages notarize-macos does that).
#
# Signing and notarizing the package itself is optional and driven by the
# environment, the same all-or-nothing way native-packages treats the app
# credentials:
#   APPLE_INSTALLER_SIGNING_IDENTITY  "Developer ID Installer: Name (TEAMID)"
#   APPLE_INSTALLER_KEYCHAIN          keychain holding that identity (optional)
#   APPLE_ID, APPLE_TEAM_ID, APPLE_APP_PASSWORD   notarization credentials
# With an identity, the package is signed; with the Apple ID set as well, it
# is notarized and the ticket is stapled, so it installs offline too.
set -euo pipefail

binary=${1:?signed universal binary}
version=${2:?version}
output=${3:?output .pkg}
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/payload/usr/local/bin" "$work/resources"
cp "$binary" "$work/payload/usr/local/bin/cww"
chmod 755 "$work/payload/usr/local/bin/cww"
# Drop quarantine and other removable attributes; the code signature lives
# inside the binary. (com.apple.provenance can't be removed and shows up as
# ._ entries in the payload; Installer restores it as an attribute.)
xattr -cr "$work/payload"
cp "$here"/resources/*.html "$root/LICENSE-MIT" "$work/resources/"

pkgbuild \
  --root "$work/payload" \
  --identifier com.chatwithwork.cww \
  --version "$version" \
  --install-location / \
  --scripts "$here/scripts" \
  "$work/cww-component.pkg"

sed "s/__VERSION__/$version/" "$here/distribution.xml" > "$work/distribution.xml"

sign=()
if [ -n "${APPLE_INSTALLER_SIGNING_IDENTITY:-}" ]; then
  sign=(--sign "$APPLE_INSTALLER_SIGNING_IDENTITY" --timestamp)
  if [ -n "${APPLE_INSTALLER_KEYCHAIN:-}" ]; then
    sign+=(--keychain "$APPLE_INSTALLER_KEYCHAIN")
  fi
fi
productbuild \
  --distribution "$work/distribution.xml" \
  --package-path "$work" \
  --resources "$work/resources" \
  ${sign[@]+"${sign[@]}"} \
  "$output"

if [ ${#sign[@]} -eq 0 ]; then
  echo "Built an unsigned package (no APPLE_INSTALLER_SIGNING_IDENTITY): $output"
  exit 0
fi
pkgutil --check-signature "$output"

if [ -z "${APPLE_ID:-}" ] || [ -z "${APPLE_TEAM_ID:-}" ] || [ -z "${APPLE_APP_PASSWORD:-}" ]; then
  echo "Signed, but not notarized (no Apple ID credentials): $output"
  exit 0
fi
xcrun notarytool submit "$output" \
  --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD" \
  --wait --timeout 30m
xcrun stapler staple "$output"
xcrun stapler validate "$output"
spctl --assess --type install --verbose=2 "$output"
echo "Signed, notarized and stapled: $output"
