#!/usr/bin/env bash
# Build the complete Linux archive consumed by installers and package managers.
# Usage: bash packaging/linux/archive.sh CWW CWW_APP VERSION GNU_TARGET OUTPUT_DIR
set -euo pipefail
cww=$(realpath "${1:?cww binary}")
app=$(realpath "${2:?cww-app binary}")
version=${3:?version}
target=${4:?GNU target}
output=$(realpath -m "${5:?output directory}")
root=$(cd "$(dirname "$0")/../.." && pwd)
case "$target" in
  x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu) ;;
  *) echo "Unsupported desktop target: $target" >&2; exit 1 ;;
esac
mkdir -p "$output"
work=$(mktemp -d "$output/.archive.XXXXXX")
trap 'rm -rf "$work"' EXIT
name="cww-app-v${version}-${target}"
mkdir -p "$work/$name/packaging/systemd"
install -m755 "$cww" "$work/$name/cww"
install -m755 "$app" "$work/$name/cww-app"
install -m644 "$root/packaging/linux/cww-app.desktop" "$work/$name/"
install -m644 "$root/app/assets/mark.svg" "$work/$name/cww-app.svg"
install -m644 "$root/packaging/systemd/cww.service" "$work/$name/packaging/systemd/"
for doc in README.md PROTOCOL.md CONTROL.md SECURITY.md LICENSE-MIT LICENSE-APACHE; do
  install -m644 "$root/$doc" "$work/$name/"
done
tar -C "$work" -czf "$output/$name.tar.gz" "$name"
