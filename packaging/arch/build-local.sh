#!/bin/sh
# Build the Arch package from the current checkout instead of a GitHub tag.
# The committed tree is packed as the tarball makepkg expects, so makepkg
# uses it instead of downloading. Output: packaging/arch/*.pkg.tar.zst
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
pkgname=$(sed -n 's/^pkgname=//p' "$here/PKGBUILD")
pkgver=$(sed -n 's/^pkgver=//p' "$here/PKGBUILD")
git -C "$root" archive --format=tar.gz --prefix="$pkgname-$pkgver/" \
  -o "$here/$pkgname-$pkgver.tar.gz" HEAD
cd "$here"
makepkg --force --cleanbuild "$@"
