#!/bin/bash
# Import the Developer ID Installer certificate into a temporary keychain,
# for pkg.sh on a CI runner. Prints the keychain's path.
#
# Needs APPLE_INSTALLER_CERTIFICATE_P12 (base64 PKCS#12 with the private
# key) and APPLE_INSTALLER_CERTIFICATE_PASSWORD. The user's own keychains
# are left alone apart from adding this one to the search list.
set -euo pipefail
: "${APPLE_INSTALLER_CERTIFICATE_P12:?}" "${APPLE_INSTALLER_CERTIFICATE_PASSWORD:?}"
dir=${RUNNER_TEMP:-$(mktemp -d)}
keychain="$dir/cww-installer.keychain-db"
password=$(uuidgen)
security create-keychain -p "$password" "$keychain" >&2
security set-keychain-settings -lut 21600 "$keychain" >&2
security unlock-keychain -p "$password" "$keychain" >&2
p12="$dir/cww-installer.p12"
printf '%s' "$APPLE_INSTALLER_CERTIFICATE_P12" | base64 --decode > "$p12"
security import "$p12" -k "$keychain" -P "$APPLE_INSTALLER_CERTIFICATE_PASSWORD" \
  -T /usr/bin/productbuild -T /usr/bin/productsign -T /usr/bin/pkgbuild >&2
rm -f "$p12"
security set-key-partition-list -S apple-tool:,apple: -s -k "$password" "$keychain" >/dev/null
# shellcheck disable=SC2046
security list-keychains -d user -s "$keychain" $(security list-keychains -d user | tr -d '"')
echo "$keychain"
