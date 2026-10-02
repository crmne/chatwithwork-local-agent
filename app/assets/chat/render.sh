#!/bin/bash
# Render the chat page's committed images. Needs rsvg-convert (librsvg) and
# ImageMagick 7.
#
#   app/assets/chat/render.sh
#
# icons.png: the Phosphor icons in icons/ (bold weight, as the web app uses
# them), one 64 px cell each in a single row, in the order of `ICONS` below
# and of `Icon` in app/src/ui/chat/icons.rs. Only the alpha channel counts:
# the app tints them.
#
# logotype*.png: the web app's logotype, 66 px high (the sidebar draws it 22
# points high, so it stays sharp up to 3x).
set -euo pipefail
cd "$(dirname "$0")"

ICONS="note-pencil magnifying-glass sidebar-simple x paperclip arrow-up
stop lock-simple caret-right copy check arrow-down arrow-up-right gear-six
warning-circle file-text file-pdf file-xls file-doc globe-simple app-window plug
desktop caret-up-down"

cells=()
for name in $ICONS; do
    rsvg-convert -w 64 -h 64 "icons/$name.svg" -o "cell-$name.tmp.png"
    cells+=("cell-$name.tmp.png")
done
magick "${cells[@]}" +append -channel RGB -evaluate set 100% +channel -strip PNG32:icons.png
rm cell-*.tmp.png

rsvg-convert -h 66 --keep-aspect-ratio logotype.svg -o logotype.png
rsvg-convert -h 66 --keep-aspect-ratio logotype-dark.svg -o logotype-dark.png
magick logotype.png -strip PNG32:logotype.png
magick logotype-dark.png -strip PNG32:logotype-dark.png
