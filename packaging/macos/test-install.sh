#!/usr/bin/env bash
# Run only on an isolated macOS CI runner, after building the unsigned .pkg.
set -euo pipefail
package=${1:?installer package}
sudo installer -pkg "$package" -target /
test -x "/Applications/Chat with Work.app/Contents/MacOS/cww-app"
test -x "/Applications/Chat with Work.app/Contents/MacOS/cww"
/usr/local/bin/cww --version
/usr/local/bin/cww-app --version
codesign --verify --strict --deep "/Applications/Chat with Work.app"
# Hosted runners may not have a console user, so exercise the documented
# fallback in the runner's own login session too.
/usr/local/bin/cww daemon install
running=false
for _ in {1..30}; do
  if /usr/local/bin/cww status | grep -q 'running (pid'; then running=true; break; fi
  sleep 1
done
/usr/local/bin/cww daemon uninstall
test "$running" = true
