#!/usr/bin/env bash
# Fetch the yt-dlp pinned in bundled-tools.env and check its digests, and lay
# it out with the ffmpeg packaging/ffmpeg.sh built for what
# packaging/macos-bundle.sh puts in Patina.app. Files already in the
# downloads folder are checked, not fetched again.
#
#   packaging/macos-tools.sh <downloads dir> <ffmpeg dir> <out dir>
#
# <ffmpeg dir> is what `packaging/ffmpeg.sh macos` wrote: the universal
# ffmpeg and its LICENSE-ffmpeg.txt. <out dir> receives exactly these five,
# and nothing else:
#   yt-dlp                  yt-dlp_macos, universal already, renamed
#   ffmpeg                  universal, for macOS 11
#   THIRD-PARTY-yt-dlp.txt  the notices for what the yt-dlp build carries
#   LICENSE-GPLv3.txt       the GPLv3, which that yt-dlp build is under
#   LICENSE-ffmpeg.txt      the LGPL 2.1, which ffmpeg is under
#
# Runs on macOS: lipo is Apple's.
set -euo pipefail

usage='usage: macos-tools.sh <downloads dir> <ffmpeg dir> <out dir>'
downloads=${1:?$usage}
ffmpeg=${2:?$usage}
out=${3:?$usage}
pins="$(cd "$(dirname "$0")" && pwd)/bundled-tools.env"
mkdir -p "$downloads"
rm -rf "${out:?}"
mkdir -p "$out"

pin() {
    local value
    value=$(sed -n "s/^$1=//p" "$pins" | tr -d '\r')
    [ -n "$value" ] || { echo "::error::bundled-tools.env has no $1"; exit 1; }
    printf '%s' "$value"
}

# Fetch a pin under the name given and check it; print its path.
fetch() {
    local key=$1 name=$2 url sha256 file got
    url=$(pin "${key}_URL")
    sha256=$(pin "${key}_SHA256")
    file="$downloads/$name"
    if [ ! -f "$file" ]; then
        curl -fsSL --retry 3 -o "$file.part" "$url" || {
            echo "::error::could not download $url; if it was removed upstream, refresh packaging/bundled-tools.env" >&2
            exit 1
        }
        mv "$file.part" "$file"
    fi
    got=$(shasum -a 256 "$file" | cut -d' ' -f1)
    if [ "$got" != "$sha256" ]; then
        echo "::error::$url has SHA-256 $got, not the pinned $sha256" >&2
        exit 1
    fi
    printf '%s' "$file"
}

ytdlp=$(fetch YTDLP_MACOS yt-dlp_macos)
notices=$(fetch YTDLP_LICENSES THIRD_PARTY_LICENSES.txt)
license=$(fetch GPLV3_LICENSE COPYING.GPLv3)

cp "$ytdlp" "$out/yt-dlp"
chmod +x "$out/yt-dlp"
cp "$notices" "$out/THIRD-PARTY-yt-dlp.txt"
cp "$license" "$out/LICENSE-GPLv3.txt"

# ffmpeg.sh's output, as it wrote it: one file with both architectures.
if [ "$(cd "$ffmpeg" && find . -mindepth 1 | sed 's|^\./||' | LC_ALL=C sort | tr '\n' ' ')" != "LICENSE-ffmpeg.txt ffmpeg " ]; then
    echo "::error::$ffmpeg does not hold exactly ffmpeg and LICENSE-ffmpeg.txt, as packaging/ffmpeg.sh writes them" >&2
    exit 1
fi
cp "$ffmpeg/ffmpeg" "$ffmpeg/LICENSE-ffmpeg.txt" "$out/"
chmod +x "$out/ffmpeg"
for arch in x86_64 arm64; do
    lipo "$out/ffmpeg" -verify_arch "$arch"
done

echo "yt-dlp $(pin YTDLP_VERSION) as pinned, and the universal ffmpeg, in $out"
