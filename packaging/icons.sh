#!/bin/sh
# Render Patina's icon at every size a platform asks for, from the SVG.
# Run after changing ui/icons/patina.svg, and commit what it writes.
#
# Committed rather than rendered at build time: the macOS runner that builds
# the .icns has no SVG renderer, and a Flatpak build is offline. Every size
# comes from the vector, so the 1024 is as sharp as the 16 rather than a
# blown-up 256.
set -eu
cd "$(dirname "$0")"
svg=../ui/icons/patina.svg
# 16 to 1024 for the macOS .icns; 24 and 48 are Windows's own.
for size in 16 24 32 48 64 128 256 512 1024; do
    rsvg-convert -w "$size" -h "$size" "$svg" -o "icons/patina-$size.png"
done

# The .ico inside patina.exe. Windows picks a size per place (16 in the
# title bar, 32 in Alt-Tab, 256 in Explorer), so all of them go in; the
# entries are the PNGs themselves, which Windows has read since Vista.
python3 - <<'ICO'
import struct
sizes = [16, 24, 32, 48, 64, 128, 256]
images = [(s, open(f"icons/patina-{s}.png", "rb").read()) for s in sizes]
header = struct.pack("<HHH", 0, 1, len(images))
offset = 6 + 16 * len(images)
entries, blobs = b"", b""
for size, blob in images:
    # One byte each for width and height, so 256 is written as 0.
    entries += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32,
                           len(blob), offset + len(blobs))
    blobs += blob
open("icons/patina.ico", "wb").write(header + entries + blobs)
ICO
echo "rendered packaging/icons from $svg"
