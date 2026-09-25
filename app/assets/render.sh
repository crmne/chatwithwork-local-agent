#!/bin/bash
# Render the committed PNG, ICO and macOS icon files from mark.svg and
# mark-template.svg. Needs rsvg-convert (librsvg) and ImageMagick 7.
#
#   app/assets/render.sh
#
# The mark is the Chat with Work logo: a white bubble with a black scribble,
# which reads on light and dark backgrounds alike. The template variant is
# the scribble alone, for the macOS menu bar, which colors it itself.
set -euo pipefail
cd "$(dirname "$0")"

square() { # svg size out
    rsvg-convert -w "$2" -h "$2" --keep-aspect-ratio "$1" -o "$3.tmp.png"
    magick "$3.tmp.png" -background none -gravity center -extent "$2x$2" -strip "$3"
    rm "$3.tmp.png"
}

# Tray and notification-area icons.
square mark.svg 32 tray-32.png
square mark.svg 64 tray-64.png
square mark-template.svg 36 tray-template-36.png

# Window icon (Windows taskbar, X11) and the Linux launcher fallback.
square mark.svg 256 icon-256.png

# Windows executable icon.
# Every entry is a PNG, which Windows has read since Vista and keeps the
# file small.
sizes="16 20 24 32 40 48 64 256"
for size in $sizes; do
    square mark.svg "$size" "ico-$size.png"
done
python3 - $sizes <<'PY'
import struct, sys
sizes = [int(s) for s in sys.argv[1:]]
images = [open(f"ico-{s}.png", "rb").read() for s in sizes]
out = struct.pack("<HHH", 0, 1, len(sizes))
offset = 6 + 16 * len(sizes)
for size, data in zip(sizes, images):
    dim = 0 if size >= 256 else size
    out += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
    offset += len(data)
open("cww-app.ico", "wb").write(out + b"".join(images))
PY
rm ico-*.png

# macOS app icon: the mark on a white rounded square, on the 1024 px grid
# with an 824 px body, as macOS draws app icons.
square mark.svg 560 mac-mark.tmp.png
magick -size 1024x1024 xc:none \
    \( -size 824x824 xc:none -fill white -draw "roundrectangle 0,0 823,823 185,185" \) \
    -gravity center -compose over -composite \
    \( +clone -alpha extract -blur 0x14 -level 0,100% -background black -alpha shape -channel A -evaluate multiply 0.28 +channel \) \
    +swap -gravity center -geometry +0+10 -compose over -composite \
    mac-mark.tmp.png -gravity center -geometry +0+6 -compose over -composite \
    -strip icon-1024.png
rm mac-mark.tmp.png
