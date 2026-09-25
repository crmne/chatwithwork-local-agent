#!/bin/bash
# Build "Chat with Work Local Agent.app" on macOS from release binaries.
#
#   packaging/macos/bundle.sh <cww-app> <cww> <output.app> <version>
#
# The app and the cww command sit side by side in Contents/MacOS, so the app
# finds cww there to start the agent, and `cww daemon install` registers
# the bundle's copy, which updates with the app.
#
# Set CODESIGN_IDENTITY to sign with a Developer ID (hardened runtime, for
# notarization). Otherwise the bundle gets the ad-hoc signature arm64 needs.
set -euo pipefail

app_binary="$1"
cww_binary="$2"
app="$3"
version="$4"
numeric_version="${version%%-*}"
here="$(cd "$(dirname "$0")" && pwd)"
assets="$here/../../app/assets"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
install -m 755 "$app_binary" "$app/Contents/MacOS/cww-app"
install -m 755 "$cww_binary" "$app/Contents/MacOS/cww"
sed "s/__VERSION__/$numeric_version/g" "$here/Info.plist" > "$app/Contents/Info.plist"

iconset="$(mktemp -d)/cww-app.iconset"
mkdir -p "$iconset"
# iconutil reads these base sizes and their @2x versions.
for size in 16 32 128 256 512; do
    sips -z $size $size "$assets/icon-1024.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z $double $double "$assets/icon-1024.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/cww-app.icns"
rm -rf "$(dirname "$iconset")"

# Sign the inner command first, then the bundle.
if [ -n "${CODESIGN_IDENTITY:-}" ]; then
    codesign --force --timestamp --options runtime --sign "$CODESIGN_IDENTITY" "$app/Contents/MacOS/cww"
    codesign --force --timestamp --options runtime --sign "$CODESIGN_IDENTITY" "$app"
else
    codesign --force --sign - "$app/Contents/MacOS/cww"
    codesign --force --sign - "$app"
fi
codesign --verify --strict --deep "$app"
plutil -lint "$app/Contents/Info.plist" >/dev/null

echo "$app"
