#!/usr/bin/env bash
# Install, exercise and remove the .deb or .rpm in a clean container.
#
#   bash packaging/test-install.sh ubuntu:24.04 path/to/native-packages-output
#
# The binary is static, so the package must install with no dependencies at
# all and cww must run: it pairs nothing here, but reports its state, shares
# a folder and serves the control socket without a server.
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
  *) echo 'Unsupported test distribution' >&2; exit 1 ;;
esac
package_dir="$packages/packages/$target/$format"
test -d "$package_dir"
docker run --rm \
  --volume "$package_dir:/packages:ro" \
  --env "FORMAT=$format" "$image" sh -ec '
    set -- /packages/*."$FORMAT"
    test "$#" -eq 1
    if [ "$FORMAT" = deb ]; then
      apt-get update -qq
      DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "$1"
      dpkg-query -W chatwithwork-local-agent
    else
      dnf install -y --setopt=install_weak_deps=False "$1"
      rpm -q chatwithwork-local-agent
    fi
    cww --version
    test -s /usr/lib/systemd/user/cww.service
    grep -qx "ExecStart=/usr/bin/cww daemon run" /usr/lib/systemd/user/cww.service
    # Minimal Debian and Ubuntu images skip /usr/share/doc on install, so
    # check the package lists the docs rather than the disk.
    if [ "$FORMAT" = deb ]; then
      dpkg -L chatwithwork-local-agent | grep -q /usr/share/doc/chatwithwork-local-agent/README.md
    else
      test -s /usr/share/doc/chatwithwork-local-agent/README.md
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

    if [ "$FORMAT" = deb ]; then apt-get remove -y chatwithwork-local-agent; else dnf remove -y chatwithwork-local-agent; fi
    test ! -e /usr/bin/cww
    test ! -e /usr/lib/systemd/user/cww.service
    # Removing the package keeps what the user created.
    test -s /tmp/cww-home/config/config.toml
  '
