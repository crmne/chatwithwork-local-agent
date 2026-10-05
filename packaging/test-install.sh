#!/usr/bin/env bash
# Install, exercise and remove a .deb, .rpm or Arch package in a clean container.
#
#   bash packaging/test-install.sh ubuntu:24.04 path/to/native-packages-output
#
# Both executables and desktop integration must be installed. The GUI loads
# graphics libraries at runtime, so check those in a minimal container too.
# The daemon test pairs nothing and uses only synthetic content.
set -euo pipefail
image=${1:?Supply an Ubuntu, Debian, Fedora or Rocky container image}
packages=$(realpath "${2:?Supply a native-packages output directory}")
case "$(uname -m)" in
  x86_64) target=linux-amd64 ;;
  aarch64) target=linux-arm64 ;;
  *) echo 'Unsupported test architecture' >&2; exit 1 ;;
esac
case "$image" in
  ubuntu:*|debian:*) format=deb ;;
  fedora:*|rockylinux:*) format=rpm ;;
  archlinux:*) format=arch ;;
  *) echo 'Unsupported test distribution' >&2; exit 1 ;;
esac
package_dir="$packages/packages/$target/$format"
test -d "$package_dir"
docker run --rm --platform "linux/${target#linux-}" \
  --volume "$package_dir:/packages:ro" \
  --env "FORMAT=$format" "$image" sh -ec '
    if [ "$FORMAT" = arch ]; then set -- /packages/*.pkg.tar.zst; else set -- /packages/*."$FORMAT"; fi
    test "$#" -eq 1
    if [ "$FORMAT" = deb ]; then
      apt-get update -qq
      DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "$1"
      dpkg-query -W chatwithwork-local-agent
    elif [ "$FORMAT" = rpm ]; then
      dnf install -y --setopt=install_weak_deps=False "$1"
      rpm -q chatwithwork-local-agent
    else
      pacman -Syu --noconfirm
      pacman -U --noconfirm "$1"
      pacman -Q chatwithwork-local-agent-bin
    fi
    cww --version
    cww-app --version
    test -s /usr/share/applications/cww-app.desktop
    grep -qx "Exec=cww-app" /usr/share/applications/cww-app.desktop
    grep -qx "Icon=cww-app" /usr/share/applications/cww-app.desktop
    test -s /usr/share/icons/hicolor/scalable/apps/cww-app.svg
    # Python is a test dependency, installed only after the package itself.
    if [ "$FORMAT" = deb ]; then apt-get install -y python3
    elif [ "$FORMAT" = rpm ]; then dnf install -y python3
    else pacman -S --noconfirm python; fi
    python3 - <<"PY"
import ctypes
for name in ("libGL.so.1", "libEGL.so.1", "libX11.so.6", "libXcursor.so.1", "libXi.so.6", "libXrandr.so.2", "libxkbcommon.so.0", "libwayland-client.so.0", "libwayland-cursor.so.0", "libwayland-egl.so.1"):
    ctypes.CDLL(name)
PY
    test -s /usr/lib/systemd/user/cww.service
    grep -qx "ExecStart=/usr/bin/cww daemon run" /usr/lib/systemd/user/cww.service
    # Minimal Debian, Ubuntu and Arch images skip /usr/share/doc on install, so
    # check the package lists the docs rather than the disk.
    if [ "$FORMAT" = deb ]; then
      dpkg -L chatwithwork-local-agent | grep -q /usr/share/doc/chatwithwork-local-agent/README.md
    elif [ "$FORMAT" = rpm ]; then
      test -s /usr/share/doc/chatwithwork-local-agent/README.md
    else
      pacman -Ql chatwithwork-local-agent-bin | grep -q /usr/share/doc/chatwithwork-local-agent-bin/README.md
    fi

    # A daemon with a shared folder, no server: status, roots, audit log.
    export CWW_HOME=/tmp/cww-home CWW_SECRET_STORE=file
    mkdir -p /tmp/shared && echo "The budget is approved." > /tmp/shared/notes.md
    cww roots add /tmp/shared --label Shared
    cww daemon run &
    daemon=$!
    for _ in $(seq 1 50); do cww status | grep -q "running (pid" && break; sleep 0.2; done
    cww status | grep -q "running (pid"
    cww status | grep -q "shared"
    cww pause && cww status --json | grep -q "\"paused\": true"
    cww log -n 5 | grep -q paused
    cww daemon stop
    wait "$daemon"

    if [ "$FORMAT" = deb ]; then apt-get remove -y chatwithwork-local-agent
    elif [ "$FORMAT" = rpm ]; then dnf remove -y chatwithwork-local-agent
    else pacman -R --noconfirm chatwithwork-local-agent-bin; fi
    test ! -e /usr/bin/cww
    test ! -e /usr/bin/cww-app
    test ! -e /usr/share/applications/cww-app.desktop
    test ! -e /usr/share/icons/hicolor/scalable/apps/cww-app.svg
    test ! -e /usr/lib/systemd/user/cww.service
    # Removing the package keeps what the user created.
    test -s /tmp/cww-home/config/config.toml
  '
