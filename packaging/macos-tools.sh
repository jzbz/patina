#!/usr/bin/env bash
# Fetch the yt-dlp pinned in bundled-tools.env and check its digest, lay its
# folder build out as a helper app, and add the ffmpeg packaging/ffmpeg.sh
# built, for what packaging/macos-bundle.sh puts in Patina.app. Files
# already in the downloads folder are checked, not fetched again.
#
#   packaging/macos-tools.sh <downloads dir> <ffmpeg dir> <out dir>
#
# <ffmpeg dir> is what `packaging/ffmpeg.sh macos` wrote: the universal
# ffmpeg and its LICENSE-ffmpeg.txt. <out dir> receives exactly these five,
# and nothing else:
#   yt-dlp.app              yt-dlp_macos.zip's folder, laid out as below
#   ffmpeg                  universal, for macOS 11
#   THIRD-PARTY-yt-dlp.txt  the notices for what the yt-dlp build carries
#   LICENSE-GPLv3.txt       the GPLv3, which that yt-dlp build is under
#   LICENSE-ffmpeg.txt      the LGPL 2.1, which ffmpeg is under
#
# yt-dlp_macos.zip holds yt-dlp_macos, PyInstaller's launcher with yt-dlp's
# Python code inside, and _internal: Python as Python.framework, its
# extension modules and libraries, and the data files beside them. The
# launcher loads all of that from where it sits, which is what makes it
# start without unpacking anything, but _internal mixes code with data and
# keeps code in folders of its own, and a code location in a bundle may
# hold neither: data there is signed into extended attributes, which a zip
# can lose, and codesign takes a folder there whose name has a dot for a
# bundle (Apple's "Placing content in a bundle"; PyInstaller's comment on
# BUNDLE in PyInstaller/building/osx.py says what each one breaks).
#
# PyInstaller has an answer of its own, and its launcher knows it: when the
# launcher's folder ends in .app/Contents/MacOS it looks for what _internal
# holds in the bundle's Contents/Frameworks instead (bootloader/src/
# pyi_main.c in PyInstaller 6.22.0, the version yt-dlp's tag pins in
# bundle/requirements/pyinstaller.txt). So the folder becomes yt-dlp.app,
# a helper app for Contents/Helpers, Apple's place for one, laid out as
# PyInstaller lays out an app of its own:
#
#   Contents/Info.plist      written here
#   Contents/MacOS/yt-dlp    the launcher
#   Contents/Frameworks/     every Mach-O file, in its folder as in
#                            _internal, and Python.framework
#   Contents/Resources/      everything else
#
# with a link in Frameworks, under the name _internal gives it, for each
# data file and data-only folder in Resources, so that Python finds every
# file where it looks. A folder in Frameworks whose name has a dot
# (python3.14) is renamed with __dot__ for the dot, as PyInstaller does,
# and a link of its old name points at it. A folder that holds both code
# and data stops this rather than being split file by file; none does in
# 2026.10.26.
#
# yt-dlp makes the zip with `zip -r` (its .github/workflows/build.yml),
# which stores each link as a copy of what it points at: _internal/Python
# and Python.framework come as four copies of the one library and three of
# its Info.plist, 44 MB more than they are, and not as a framework codesign
# accepts (Apple's "Placing content in a bundle": a framework's top level
# holds only Versions and links). Those links are made again here, as
# PyInstaller made them, once the copies are known to be the same bytes.
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

die() { echo "::error::$*" >&2; exit 1; }

pin() {
    local value
    value=$(sed -n "s/^$1=//p" "$pins" | tr -d '\r')
    [ -n "$value" ] || die "bundled-tools.env has no $1"
    printf '%s' "$value"
}

# Fetch a pin under the name given and check it; print its path.
fetch() {
    local key=$1 name=$2 url sha256 file got
    url=$(pin "${key}_URL")
    sha256=$(pin "${key}_SHA256")
    file="$downloads/$name"
    if [ ! -f "$file" ]; then
        curl -fsSL --retry 3 -o "$file.part" "$url" \
            || die "could not download $url; if it was removed upstream, refresh packaging/bundled-tools.env"
        mv "$file.part" "$file"
    fi
    got=$(shasum -a 256 "$file" | cut -d' ' -f1)
    [ "$got" = "$sha256" ] || die "$url has SHA-256 $got, not the pinned $sha256"
    printf '%s' "$file"
}

# What a folder holds, one level down, sorted and on one line.
holds() {
    (cd "$1" && find . -mindepth 1 -maxdepth 1 | sed 's|^\./||' | LC_ALL=C sort | tr '\n' ' ')
}

macho() {
    case $(file -b "$1") in
        Mach-O*) return 0 ;;
        *) return 1 ;;
    esac
}

zip=$(fetch YTDLP_MACOS yt-dlp_macos.zip)
license=$(fetch GPLV3_LICENSE COPYING.GPLv3)
version=$(pin YTDLP_VERSION)

unpacked="$out/.unpacked"
mkdir "$unpacked"
unzip -q "$zip" -d "$unpacked"
[ "$(holds "$unpacked")" = "_internal yt-dlp_macos " ] \
    || die "yt-dlp_macos.zip holds $(holds "$unpacked")rather than yt-dlp_macos and _internal"
internal="$unpacked/_internal"
if [ -n "$(find "$unpacked" -type l)" ]; then
    die "yt-dlp_macos.zip holds links now; this expects the copies zip -r makes of them (see the comment at the top)"
fi

# Python.framework as the zip has it: one version, and Current, the
# framework's top level and _internal/Python all copies of it.
fw="$internal/Python.framework"
[ -d "$fw/Versions" ] || die "yt-dlp_macos.zip has no _internal/Python.framework/Versions"
pyversion=$(cd "$fw/Versions" && find . -mindepth 1 -maxdepth 1 ! -name Current | sed 's|^\./||')
case $pyversion in
    '' | *[!0-9.]*) die "Python.framework/Versions holds $(holds "$fw/Versions")rather than one version and Current" ;;
esac
want="Python
Resources/Info.plist
Versions/$pyversion/Python
Versions/$pyversion/Resources/Info.plist
Versions/Current/Python
Versions/Current/Resources/Info.plist"
[ "$(cd "$fw" && find . -type f | sed 's|^\./||' | LC_ALL=C sort)" = "$(LC_ALL=C sort <<<"$want")" ] \
    || die "Python.framework holds other files than the library and its Info.plist, and the copies of them zip -r makes"
real="$fw/Versions/$pyversion"
for copy in "$fw/Python" "$fw/Versions/Current/Python" "$internal/Python"; do
    cmp -s "$copy" "$real/Python" || die "${copy#"$unpacked"/} is not a copy of Python.framework/Versions/$pyversion/Python"
done
for copy in "$fw/Resources/Info.plist" "$fw/Versions/Current/Resources/Info.plist"; do
    cmp -s "$copy" "$real/Resources/Info.plist" || die "${copy#"$unpacked"/} is not a copy of the framework's Info.plist"
done

app="$out/yt-dlp.app"
contents="$app/Contents"
frameworks="$contents/Frameworks"
resources="$contents/Resources"
mkdir -p "$contents/MacOS" "$frameworks" "$resources"
cp "$unpacked/yt-dlp_macos" "$contents/MacOS/yt-dlp"
chmod +x "$contents/MacOS/yt-dlp"

# The framework as PyInstaller collected it, and _internal/Python as its
# link to the library, which the launcher loads by that name.
framework="$frameworks/Python.framework"
mkdir -p "$framework/Versions/$pyversion/Resources"
cp "$real/Python" "$framework/Versions/$pyversion/Python"
cp "$real/Resources/Info.plist" "$framework/Versions/$pyversion/Resources/Info.plist"
ln -s "$pyversion" "$framework/Versions/Current"
ln -s Versions/Current/Python "$framework/Python"
ln -s Versions/Current/Resources "$framework/Resources"
ln -s "Python.framework/Versions/$pyversion/Python" "$frameworks/Python"

# The rest of _internal, each entry by what it holds.
while IFS= read -r -d '' entry; do
    name=${entry##*/}
    case $name in
        Python | Python.framework) continue ;;
    esac
    if [ -f "$entry" ]; then
        if macho "$entry"; then
            cp "$entry" "$frameworks/$name"
        else
            cp "$entry" "$resources/$name"
            ln -s "../Resources/$name" "$frameworks/$name"
        fi
    elif [ -d "$entry" ]; then
        code=0
        data=0
        while IFS= read -r -d '' file; do
            if macho "$file"; then code=$((code + 1)); else data=$((data + 1)); fi
        done < <(find "$entry" -type f -print0)
        if [ "$code" -eq 0 ]; then
            cp -R "$entry" "$resources/$name"
            ln -s "../Resources/$name" "$frameworks/$name"
        elif [ "$data" -eq 0 ]; then
            cp -R "$entry" "$frameworks/$name"
        else
            die "_internal/$name holds both code and data, which this does not know how to lay out (see the comment at the top)"
        fi
    else
        die "_internal/$name is neither a file nor a folder"
    fi
done < <(find "$internal" -mindepth 1 -maxdepth 1 -print0)

# Folders in Frameworks with a dot in their names, deepest first, so that
# renaming one never moves another still to be renamed. Not the framework,
# whose name and version are meant to have one; not the links to
# Resources, which find leaves out as links.
while IFS= read -r -d '' dir; do
    name=${dir##*/}
    mv "$dir" "${dir%/*}/${name//./__dot__}"
    ln -s "${name//./__dot__}" "$dir"
done < <(find "$frameworks" -depth -mindepth 1 -type d -name '*.*' \
    ! -name '*.framework' ! -path '*.framework/*' -print0)

# What came out: code in Frameworks only, both architectures in each, and
# nothing in Frameworks that isn't code but the framework's Info.plist.
count=0
while IFS= read -r -d '' file; do
    if macho "$file"; then
        for arch in x86_64 arm64; do
            lipo "$file" -verify_arch "$arch" || die "${file#"$out"/} has no $arch half"
        done
        count=$((count + 1))
    elif [ "$file" != "$framework/Versions/$pyversion/Resources/Info.plist" ]; then
        die "${file#"$out"/} is in Frameworks and is not code"
    fi
done < <(find "$frameworks" -type f -print0)
while IFS= read -r -d '' file; do
    if macho "$file"; then die "${file#"$out"/} is code in Resources"; fi
done < <(find "$resources" -type f -print0)
for arch in x86_64 arm64; do
    lipo "$contents/MacOS/yt-dlp" -verify_arch "$arch" || die "the launcher has no $arch half"
done

# A helper app's Info.plist: its identifier under Patina's, which
# macos-sign.sh signs it with; its version, yt-dlp's; macOS 11, as for the
# rest of Patina.app; and no Dock icon or menu bar, should anything open it
# as an app rather than run it as Patina does.
cat >"$contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>yt-dlp</string>
  <key>CFBundleIdentifier</key><string>video.patina.Patina.yt-dlp</string>
  <key>CFBundleExecutable</key><string>yt-dlp</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSBackgroundOnly</key><true/>
</dict>
</plist>
PLIST

cp "$internal/THIRD_PARTY_LICENSES.txt" "$out/THIRD-PARTY-yt-dlp.txt"
cp "$license" "$out/LICENSE-GPLv3.txt"
rm -rf "$unpacked"

# ffmpeg.sh's output, as it wrote it: one file with both architectures.
[ "$(holds "$ffmpeg")" = "LICENSE-ffmpeg.txt ffmpeg " ] \
    || die "$ffmpeg does not hold exactly ffmpeg and LICENSE-ffmpeg.txt, as packaging/ffmpeg.sh writes them"
cp "$ffmpeg/ffmpeg" "$ffmpeg/LICENSE-ffmpeg.txt" "$out/"
chmod +x "$out/ffmpeg"
for arch in x86_64 arm64; do
    lipo "$out/ffmpeg" -verify_arch "$arch"
done

echo "yt-dlp $version as pinned, as yt-dlp.app ($count Mach-O files in its Frameworks), and the universal ffmpeg, in $out"
