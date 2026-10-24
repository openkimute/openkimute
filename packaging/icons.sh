#!/bin/sh
# The icon in each platform's format, from packaging/openkumite.svg (the one to edit); run again after changing it:
#   packaging/windows/openkumite.ico   the .exe's icon (app/openkumite.rc): 16 to 256 px
#   packaging/macos/openkumite.icns    the .app's icon (packaging/macos/bundle.sh): 16 to 1024 px
#   packaging/web/*.png                the installed browser version's (web/manifest.webmanifest): 192 and 512 px,
#                                      maskable 512 px (Android crops it to its own shape) and Apple's 180 px
# (Linux and the browser's tab use the SVG itself.) Runs on macOS: needs resvg (brew install resvg), python3 and
# iconutil (macOS's own).
set -e
cd "$(dirname "$0")"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
png() { resvg -w "$1" -h "$1" openkumite.svg "$2"; }
for s in 16 20 24 32 40 48 64 128 256; do png $s "$tmp/$s.png"; done
mkdir -p windows macos
# (each size as a PNG in the .ico, which Windows reads since Vista: ImageMagick would store bitmaps, 10x the size)
python3 - "$tmp" windows/openkumite.ico <<'EOF'
import struct, sys
sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
pngs = [open(f"{sys.argv[1]}/{s}.png", "rb").read() for s in sizes]
out = struct.pack("<HHH", 0, 1, len(sizes))
at = 6 + 16 * len(sizes)
for s, p in zip(sizes, pngs):
    out += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(p), at)
    at += len(p)
open(sys.argv[2], "wb").write(out + b"".join(pngs))
EOF
set_=$tmp/openkumite.iconset
mkdir "$set_"
for s in 16 32 128 256 512; do
    png $s "$set_/icon_${s}x${s}.png"
    png $((s * 2)) "$set_/icon_${s}x${s}@2x.png"
done
iconutil -c icns -o macos/openkumite.icns "$set_"
# the web's: as it is, and full bleed (the tile's background to the edges, the picture inside the middle 75%: a
# launcher's mask keeps the middle 80% circle)
mkdir -p web
png 192 web/icon-192.png
png 512 web/icon-512.png
python3 - openkumite.svg "$tmp/bleed.svg" <<'EOF'
import re, sys
s = open(sys.argv[1]).read()
tile = re.search(r'<rect width="512" height="512" rx="112" fill="url\(#tile\)"/>', s)
head, body = s[:tile.start()], s[tile.end():s.rindex('</svg>')]
open(sys.argv[2], "w").write(f'{head}<rect width="512" height="512" fill="url(#tile)"/>\n'
                             f'<g transform="translate(64 64) scale(0.75)">{body}</g>\n</svg>\n')
EOF
resvg -w 512 -h 512 "$tmp/bleed.svg" web/maskable-512.png
resvg -w 180 -h 180 "$tmp/bleed.svg" web/apple-touch-icon.png
ls -l windows/openkumite.ico macos/openkumite.icns web/*.png
