#!/bin/sh
# Install the Chat with Work Local Agent (cww) from the latest GitHub release.
#
#   curl --proto '=https' --tlsv1.2 -LsSf \
#     https://github.com/crmne/chatwithwork-local-agent/releases/latest/download/cww-installer.sh | sh
#
# Linux (x86_64, arm64; static binaries for any distribution) and macOS
# (universal, signed and notarized). Installs `cww` into ~/.local/bin, or
# $CWW_INSTALL_DIR. Set CWW_VERSION=1.2.3 for a specific release. The
# download is checked against the release's checksums.txt before anything
# is installed. Nothing runs as root and nothing is shared until you say so.
# CWW_DOWNLOAD_BASE points at a mirror holding a release's files.
set -eu

repo="crmne/chatwithwork-local-agent"
install_dir="${CWW_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
fail() { printf 'cww-installer: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || fail "needs $1"; }

need uname
need tar
need mkdir
if command -v curl >/dev/null 2>&1; then
  fetch() { curl --proto '=https' --tlsv1.2 -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget --https-only -q "$1" -O "$2"; }
else
  fail "needs curl or wget"
fi
if command -v sha256sum >/dev/null 2>&1; then
  sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
  sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
  fail "needs sha256sum or shasum"
fi

case "$(uname -s)" in
  Linux)
    case "$(uname -m)" in
      x86_64 | amd64) target=x86_64-unknown-linux-musl ;;
      aarch64 | arm64) target=aarch64-unknown-linux-musl ;;
      *) fail "no build for $(uname -m) Linux" ;;
    esac ;;
  Darwin) target=macos-universal ;;
  *) fail "no build for $(uname -s); on Windows use cww-installer.ps1 or the MSI" ;;
esac

if [ -n "${CWW_DOWNLOAD_BASE:-}" ]; then
  # A mirror, or a local copy of a release for testing.
  base="${CWW_DOWNLOAD_BASE%/}"
elif [ -n "${CWW_VERSION:-}" ]; then
  version="${CWW_VERSION#v}"
  base="https://github.com/$repo/releases/download/v$version"
else
  # The latest release's tag, from the redirect of /releases/latest.
  base="https://github.com/$repo/releases/latest/download"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

fetch "$base/checksums.txt" "$tmp/checksums.txt" || fail "can't download checksums.txt from $base"
asset=$(grep -o "cww-v[^ ]*-$target\.tar\.gz" "$tmp/checksums.txt" | head -n 1)
[ -n "$asset" ] || fail "the release has no build for $target"
say "Downloading $asset"
fetch "$base/$asset" "$tmp/$asset" || fail "can't download $asset"

expected=$(grep " $asset\$" "$tmp/checksums.txt" | cut -d' ' -f1 | head -n 1)
actual=$(sha256 "$tmp/$asset")
[ -n "$expected" ] && [ "$expected" = "$actual" ] || fail "checksum mismatch for $asset"

tar -xzf "$tmp/$asset" -C "$tmp"
dir="$tmp/${asset%.tar.gz}"
[ -f "$dir/cww" ] || fail "$asset has no cww binary"
mkdir -p "$install_dir"
# Replace atomically, so a running daemon keeps its old binary until restart.
cp "$dir/cww" "$install_dir/.cww.new"
chmod 755 "$install_dir/.cww.new"
mv -f "$install_dir/.cww.new" "$install_dir/cww"
say "Installed $("$install_dir/cww" --version) to $install_dir/cww"

case ":$PATH:" in
  *":$install_dir:"*) ;;
  *) say ""
     say "$install_dir is not on your PATH. Add it, for example:"
     say "  echo 'export PATH=\"$install_dir:\$PATH\"' >> ~/.profile" ;;
esac

say ""
say "Next:"
say "  cww                    # pair this computer and choose what to share"
say "  cww daemon install     # keep it running in the background"
say ""
say "To check this download against its build provenance:"
say "  gh attestation verify $asset --repo $repo"
