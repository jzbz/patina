#!/usr/bin/env bash
# Fetch the libraries pinned in macos-libmpv.txt into a folder and check
# their digests. Files already there are checked, not fetched again, so a
# cache of the folder outlives IINA replacing them upstream.
#
#   packaging/macos-libmpv.sh <dir>
set -euo pipefail

dir=${1:?usage: macos-libmpv.sh <dir>}
list="$(cd "$(dirname "$0")" && pwd)/macos-libmpv.txt"
mkdir -p "$dir"
cd "$dir"

grep -vE '^(#|$)' "$list" | while read -r sha256 name; do
    if [ ! -f "$name" ]; then
        curl -fsSL --retry 3 -o "$name" "https://iina.io/dylibs/universal/$name" || {
            echo "::error::could not download $name; if IINA replaced it, refresh packaging/macos-libmpv.txt"
            exit 1
        }
    fi
    got=$(shasum -a 256 "$name" | cut -d' ' -f1)
    if [ "$got" != "$sha256" ]; then
        echo "::error::$name has SHA-256 $got, not the pinned $sha256; refresh packaging/macos-libmpv.txt"
        exit 1
    fi
done
echo "$(grep -cvE '^(#|$)' "$list") libraries in $dir, all as pinned"
