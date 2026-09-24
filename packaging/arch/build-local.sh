#!/bin/sh
# Build and check the Arch package from the current checkout, the way the AUR
# recipe builds a release: packaging/arch/chatwithwork-local-agent/PKGBUILD.in
# is rendered with the working tree's version, the committed tree (HEAD) is
# packed as the source tarball, and makepkg builds, tests and packages it.
#
#   packaging/arch/build-local.sh           # build
#   packaging/arch/build-local.sh --install # build, then pacman -U (asks for sudo)
#
# Output: packaging/arch/build/*.pkg.tar.zst
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
pkgname=chatwithwork-local-agent
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)
out="$here/build"
rm -rf "$out"
mkdir -p "$out"
git -C "$root" archive --format=tar.gz --prefix="$pkgname-$version/" \
  -o "$out/$pkgname-$version.tar.gz" HEAD
sum=$(sha256sum "$out/$pkgname-$version.tar.gz" | cut -d' ' -f1)
sed -e "s/@VERSION@/$version/g" -e "s/@PKGREL@/1/g" -e "s/@SOURCE_SHA256@/$sum/g" \
  "$here/$pkgname/PKGBUILD.in" > "$out/PKGBUILD"
cp "$here/cww.install" "$out/$pkgname.install"
cd "$out"
makepkg --force --cleanbuild "$@"
if command -v namcap >/dev/null 2>&1; then
  namcap PKGBUILD ./*.pkg.tar.zst || true
fi
