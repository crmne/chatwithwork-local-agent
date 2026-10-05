#!/bin/sh
# Install the Chat with Work Local Agent (cww) from the latest GitHub release.
#
#   curl --proto '=https' --tlsv1.2 -LsSf \
#     https://github.com/crmne/chatwithwork-local-agent/releases/latest/download/cww-installer.sh | sh
#
# Linux (x86_64, arm64; glibc 2.35+) and macOS (universal, signed and
# notarized). Installs the GUI, terminal interface and daemon. Commands go
# in ~/.local/bin, or $CWW_INSTALL_DIR. Set CWW_VERSION=1.2.3 for a release. The
# download is checked against the release's checksums.txt before anything
# is installed. No elevated privileges are needed; nothing is shared until you say so.
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

platform=$(uname -s)
case "$platform" in
  Linux)
    prefix=cww-app
    case "$(uname -m)" in
      x86_64 | amd64) target=x86_64-unknown-linux-gnu ;;
      aarch64 | arm64) target=aarch64-unknown-linux-gnu ;;
      *) fail "no build for $(uname -m) Linux" ;;
    esac ;;
  Darwin) target=macos-universal; prefix=cww; need ditto ;;
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

cache_dir="${XDG_CACHE_HOME:-$HOME/.cache}/cww-installer"
mkdir -p "$cache_dir"
tmp=$(mktemp -d "$cache_dir/download.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM

fetch "$base/checksums.txt" "$tmp/checksums.txt" || fail "can't download checksums.txt from $base"
asset=$(awk -v pattern="^${prefix}-v[^ ]*-${target}\\.tar\\.gz$" '$2 ~ pattern { print $2; exit }' "$tmp/checksums.txt")
[ -n "$asset" ] || fail "the release has no build for $target"
say "Downloading $asset"
fetch "$base/$asset" "$tmp/$asset" || fail "can't download $asset"

expected=$(grep " $asset\$" "$tmp/checksums.txt" | cut -d' ' -f1 | head -n 1)
actual=$(sha256 "$tmp/$asset")
[ -n "$expected" ] && [ "$expected" = "$actual" ] || fail "checksum mismatch for $asset"

tar -xzf "$tmp/$asset" -C "$tmp"
dir="$tmp/${asset%.tar.gz}"
[ -f "$dir/cww" ] || fail "$asset has no cww binary"
if [ "$platform" = Linux ]; then
  [ -f "$dir/cww-app.desktop" ] && [ -f "$dir/cww-app.svg" ] || fail "$asset has no desktop launcher or icon"
  "$dir/cww-app" --version >/dev/null || fail "the desktop app cannot run; Linux needs glibc 2.35 or newer"
else
  [ -f "$dir/Chat with Work.app/Contents/MacOS/cww-app" ] || fail "$asset has no desktop app"
fi
mkdir -p "$install_dir"
# Use absolute paths in launchers, even with a relative CWW_INSTALL_DIR.
install_dir=$(cd "$install_dir" && pwd)
# Replace atomically, so a running daemon keeps its old binary until restart.
cp "$dir/cww" "$install_dir/.cww.new"
chmod 755 "$install_dir/.cww.new"
mv -f "$install_dir/.cww.new" "$install_dir/cww"
say "Installed $("$install_dir/cww" --version) to $install_dir/cww"

if [ "$platform" = Linux ]; then
  cp "$dir/cww-app" "$install_dir/.cww-app.new"
  chmod 755 "$install_dir/.cww-app.new"
  mv -f "$install_dir/.cww-app.new" "$install_dir/cww-app"
  data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
  mkdir -p "$data_dir/applications" "$data_dir/icons/hicolor/scalable/apps"
  # Desktop Exec has its own quoting rules, including percent field codes.
  CWW_DESKTOP_EXEC="$install_dir/cww-app" awk '
    /^Exec=/ {
      path = ENVIRON["CWW_DESKTOP_EXEC"]
      quoted = ""
      for (i = 1; i <= length(path); i++) {
        c = substr(path, i, 1)
        if (c == "%") quoted = quoted "%%"
        else if (c == "\\") quoted = quoted "\\\\\\\\"
        else if (c == "\"" || c == "`" || c == "$") quoted = quoted "\\\\" c
        else quoted = quoted c
      }
      print "Exec=\"" quoted "\""
      next
    }
    { print }
  ' "$dir/cww-app.desktop" > "$data_dir/applications/cww-app.desktop"
  cp "$dir/cww-app.svg" "$data_dir/icons/hicolor/scalable/apps/cww-app.svg"
  if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$data_dir/applications" || true
  fi
else
  app_dir="$HOME/Applications"
  mkdir -p "$app_dir"
  staged_app=$(mktemp -d "$app_dir/.cww-install.XXXXXX")
  ditto "$dir/Chat with Work.app" "$staged_app/Chat with Work.app"
  rm -rf "$app_dir/Chat with Work.app"
  mv "$staged_app/Chat with Work.app" "$app_dir/Chat with Work.app"
  rmdir "$staged_app"
  ln -sf "$app_dir/Chat with Work.app/Contents/MacOS/cww-app" "$install_dir/cww-app"
fi

case ":$PATH:" in
  *":$install_dir:"*) ;;
  *) say ""
     say "$install_dir is not on your PATH. Add it, for example:"
     say "  echo 'export PATH=\"$install_dir:\$PATH\"' >> ~/.profile" ;;
esac

say ""
if [ "$(id -u)" -ne 0 ] && "$install_dir/cww" daemon install; then
  say "The background agent is running and will start at every login."
else
  say "The background agent could not be started for your account."
  say "Choose Start Local Agent in Chat with Work, or run as your own user (without sudo):"
  say "  \"$install_dir/cww\" daemon install"
  say "Without a user service manager: \"$install_dir/cww\" daemon run"
fi
say "Open Chat with Work from your applications menu, or run cww for the terminal interface."
say "Check the agent with cww status. Nothing is shared until you pair and choose a folder."
say ""
say "To check this download against its build provenance:"
say "  gh attestation verify $asset --repo $repo"
