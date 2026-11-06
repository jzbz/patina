#!/usr/bin/env bash
# Build the ffmpeg the macOS and Windows packages carry, from FFmpeg's own
# source tarball pinned in bundled-tools.env, configured to do what yt-dlp
# asks of it for Patina's downloads and nothing else: join separate video
# and audio, and the fixes yt-dlp makes to a file after downloading it, all
# by copying streams, never by encoding one. A few MB rather than the 130 MB
# and more of a full build, LGPL, with nothing linked but the system's
# libraries, and its corresponding source is that tarball and this script.
#
#   packaging/ffmpeg.sh <target> <downloads dir> <work dir> <out dir>
#   packaging/ffmpeg.sh flags
#
# <target> is one of
#   macos    on a Mac: arm64 and x86_64, each for macOS 11 like the rest of
#            Patina.app, joined with lipo into one file
#   windows  on Linux, with mingw-w64 (x86_64-w64-mingw32-gcc): a static
#            x86_64 ffmpeg.exe that needs only DLLs Windows itself carries
#   linux    this Linux machine, to try the configuration out; no package
#            carries it
#
# A tarball already in <downloads dir> is checked, not fetched again. <work
# dir> holds the unpacked source and each build, with configure's config.log,
# and is emptied first. <out dir> receives exactly two files: ffmpeg
# (ffmpeg.exe for windows) and LICENSE-ffmpeg.txt, the LGPL 2.1 text from the
# tarball, under which the build is.
#
# `flags` prints the configure options every target shares, one a line,
# which release.yml compares with the ones THIRD-PARTY.md names.
#
# What it has to do, read from yt-dlp's yt_dlp/YoutubeDL.py (process_info,
# `fixup`) and yt_dlp/postprocessor/ffmpeg.py. mpv's yt-dlp hook runs yt-dlp
# with -J, which downloads nothing and so runs no postprocessor; Patina's
# downloads (src/downloads.rs, `command`) ask for none of their own, so what
# runs is what yt-dlp runs by default:
#
#   FFmpegMergerPP       separate video and audio, as YouTube serves anything
#                        above 360p, joined with -c copy and -map; for audio
#                        from HLS (m3u8) that is AAC, -bsf:a aac_adtstoasc
#   FFmpegFixupM4aPP     a DASH m4a remuxed into an ordinary one, -f mp4
#   FFmpegFixupM3u8PP    an HLS download, MPEG-TS under an .mp4 name,
#                        remuxed into MP4, -f mp4 -bsf:a aac_adtstoasc
#   FFmpegFixupDuplicateMoovPP, FFmpegFixupDurationPP
#                        a plain stream copy, -map 0 -dn -ignore_unknown
#                        -c copy, into whatever the file's extension names
#   FFmpegFixupStretchedPP
#                        the same with -aspect, a new display aspect ratio
#   FFmpegFixupTimestampPP
#                        the same with -bsf setts=ts=TS-STARTPTS and -ss;
#                        yt-dlp looks for setts in `ffmpeg -bsfs`, and
#                        without it would ask for a filter and an encoder
#
# and, when the user's own yt-dlp options or configuration file ask for
# --embed-metadata or --embed-chapters (Patina passes on both to downloads
# that aren't from the YouTube section), FFmpegMetadataPP: -metadata options,
# chapters from an ffmetadata file as a second input, and -write_id3v1 1,
# an option of the mp3 muxer that yt-dlp passes whatever the format, and
# which ffmpeg refuses unless some enabled component has it. Both cost
# 16 KB.
#
# Each names its files file:<path>, so the file protocol is the only one.
# Patina never has yt-dlp write to standard output (`-o -`, the one use of
# the pipe protocol). A download yt-dlp hands to ffmpeg itself, which is a
# live stream's, needs network protocols and TLS, which this leaves out:
# such a download fails at once ("ffmpeg exited with code 8") rather than
# recording. The YouTube section refuses to download a live stream anyway.
# What YouTube serves goes in and out of these:
#
#   demuxers  mov (mp4, m4a, DASH's fragmented MP4), matroska (webm, mkv),
#             mpegts (HLS), ffmetadata (chapters)
#   muxers    mp4, ipod (which an .m4a name chooses), webm, matroska (yt-dlp
#             falls back on mkv for codecs that mp4 and webm can't hold
#             together), mp3 (for its option alone, as above)
#   parsers   h264, hevc, av1, vp9, aac, opus, ac3 (AC-3 and E-AC-3, which
#             YouTube serves for surround sound): mpegts parses every
#             stream, mov asks for the parsers of AC-3, E-AC-3, VP9, AV1 and
#             H.264, and matroska for every video codec but HEVC and every
#             audio codec but AAC
#   bsfs      aac_adtstoasc and setts, as above, and extract_extradata:
#             H.264 and HEVC in MPEG-TS carry their parameter sets in the
#             stream rather than in a header, and libavformat runs this
#             filter while it probes to lift them out. The mp4 muxer finds
#             them in the first packet without it, but the matroska muxer
#             needs them for its header, so without it HLS video joined
#             into mkv fails ("Could not write header"), which yt-dlp
#             chooses for itself when that video comes with Opus audio; 4 KB
#   decoders  aac: in MPEG-TS no header gives AAC's sample rate, which the
#             aac parser leaves to the decoder, and without it the mp4
#             muxer refuses the stream ("sample rate not set"); 370 KB.
#             vp9: no header and no parser gives VP9's pixel format, which
#             the mp4 muxer needs for its vpcC box. Without it ffmpeg
#             writes an empty one and still exits 0, so yt-dlp deletes the
#             parts and keeps an MP4 that nothing opens, whenever the
#             user's yt-dlp options ask for mp4 (merge-output-format or
#             remux-video) and the video is VP9, as most of YouTube's above
#             1080p is; 560 KB. Each decodes only the frame or two that
#             libavformat reads to find these out
#
# The muxers bring in vp9_superframe and pgs_frame_merge by themselves, the
# vp9 decoder vp9_superframe_split, and the ffmpeg program a few filters it
# always registers; configure prints the whole list. No encoder, and no
# subtitle or image format: whatever needs one fails with ffmpeg's own
# message, which yt-dlp reports as a
# postprocessing error and Patina shows. --extract-audio and --recode-video
# end with "Encoder not found", --embed-subs and --convert-subs with "Error
# opening input files: Invalid data found when processing input" (there is
# no WebVTT demuxer), and --embed-thumbnail with "Error splitting the
# argument list: Option not found". A user who wants those names a full
# ffmpeg as ffmpeg-location in Patina's yt-dlp options.
#
# ffprobe is not built. yt-dlp asks it for an audio stream's codec, and asks
# `ffmpeg -i` for that instead when it is missing; it asks it too whether an
# HLS download really is MPEG-TS before FFmpegFixupM3u8PP remuxes it, and
# without it remuxes anyway, which is harmless, and prints a warning Patina
# doesn't show. One more thing yt-dlp runs by default does need it: joining
# the parts of a multi_video result into one file (FFmpegConcatPP, which
# --concat-playlist leaves on for those), which asks ffprobe for each
# part's codecs and then uses the concat demuxer, neither of them here. No
# YouTube video is one; some other sites' are (a bilibili video in several
# parts, for one), and there yt-dlp downloads the parts, leaves them in the
# folder and stops with a Python traceback ending "ffprobe not found". A
# full ffmpeg named as above, with its ffprobe beside it, joins them.
#
# Runs with bash 3.2, the one macOS carries.
set -euo pipefail

usage() {
    echo "usage: ffmpeg.sh <macos|windows|linux> <downloads dir> <work dir> <out dir>" >&2
    echo "       ffmpeg.sh flags" >&2
    exit 2
}

here="$(cd "$(dirname "$0")" && pwd)"
pins="$here/bundled-tools.env"

pin() {
    local value
    value=$(sed -n "s/^$1=//p" "$pins" | tr -d '\r')
    [ -n "$value" ] || { echo "::error::bundled-tools.env has no $1" >&2; exit 1; }
    printf '%s' "$value"
}

sha256() {
    if command -v sha256sum >/dev/null; then
        sha256sum "$@" | cut -d' ' -f1
    else
        shasum -a 256 "$@" | cut -d' ' -f1
    fi
}

# The configuration, the same for every target. --disable-everything leaves
# no component that isn't named; --disable-autodetect links nothing that
# configure would otherwise find on the machine (zlib, Apple's frameworks, a
# TLS library), and --disable-iconv the C library's iconv, which configure
# looks for even then. No --enable-gpl, --enable-version3 or
# --enable-nonfree, so the build is LGPL 2.1 or later. Assembly is left out:
# a stream copy decodes next to nothing, and without it no target needs nasm.
# --enable-small builds for size (1.2 MB less) and drops the long
# descriptions -h prints.
# shellcheck disable=SC2054  # the commas are configure's, within one option
common=(
    --disable-everything
    --disable-autodetect --disable-iconv
    --disable-network
    --disable-programs --enable-ffmpeg
    --disable-doc
    --disable-avdevice --disable-swscale --disable-swresample
    --disable-asm
    --disable-debug
    --enable-small
    --enable-protocol=file
    --enable-demuxer=mov,matroska,mpegts,ffmetadata
    --enable-muxer=mp4,ipod,webm,matroska,mp3
    --enable-parser=h264,hevc,av1,vp9,aac,opus,ac3
    --enable-bsf=aac_adtstoasc,setts,extract_extradata
    --enable-decoder=aac,vp9
)

# What a target adds to `common`, one slice at a time; the arguments are the
# slice's name, for macos.
target_flags() {
    case "$1" in
        macos-arm64 | macos-x86_64)
            local arch=${1#macos-}
            # For macOS 11, as Patina's own binary and its libraries are, and
            # cross-compiled whichever kind of Mac this is.
            printf '%s\n' --enable-cross-compile --target-os=darwin "--arch=$arch" \
                "--cc=clang -arch $arch" \
                --extra-cflags=-mmacosx-version-min=11.0 \
                --extra-ldflags=-mmacosx-version-min=11.0
            ;;
        windows)
            # -static: libgcc and the rest of MinGW's runtime inside the exe,
            # beside the system DLLs it imports (KERNEL32, msvcrt, SHELL32
            # for CommandLineToArgvW, through which it reads Unicode file
            # names). Win32 threads, which configure prefers for MinGW,
            # rather than winpthreads.
            printf '%s\n' --enable-cross-compile --target-os=mingw32 --arch=x86_64 \
                --cross-prefix=x86_64-w64-mingw32- --extra-ldflags=-static
            ;;
        linux) ;;
        *) usage ;;
    esac
}

slices() {
    case "$1" in
        macos) echo macos-arm64 macos-x86_64 ;;
        windows | linux) echo "$1" ;;
        *) usage ;;
    esac
}

if [ "${1:-}" = flags ]; then
    printf '%s\n' "${common[@]}"
    exit 0
fi

[ $# -eq 4 ] || usage
target=$1 downloads=$2 work=$3 out=$4
slices "$target" >/dev/null

version=$(pin FFMPEG_VERSION)
url=$(pin FFMPEG_SOURCE_URL)
want=$(pin FFMPEG_SOURCE_SHA256)
mkdir -p "$downloads"
tarball="$downloads/$(basename "$url")"
if [ ! -f "$tarball" ]; then
    # ffmpeg.org resets a connection now and then; the digest below is what
    # makes the bytes trustworthy, so every error is worth a retry.
    curl -fsSL --retry 5 --retry-all-errors -o "$tarball.part" "$url" || {
        echo "::error::could not download $url" >&2
        exit 1
    }
    mv "$tarball.part" "$tarball"
fi
got=$(sha256 "$tarball")
if [ "$got" != "$want" ]; then
    echo "::error::$url has SHA-256 $got, not the pinned $want" >&2
    exit 1
fi

rm -rf "${work:?}"
mkdir -p "$work"
work=$(cd "$work" && pwd)
tar -xJf "$tarball" -C "$work"
src="$work/ffmpeg-$version"
[ -x "$src/configure" ] || { echo "::error::$tarball holds no ffmpeg-$version/configure" >&2; exit 1; }

jobs=$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 2)
exe=ffmpeg
[ "$target" = windows ] && exe=ffmpeg.exe

built=()
for slice in $(slices "$target"); do
    build="$work/build-$slice"
    mkdir -p "$build"
    flags=()
    while IFS= read -r flag; do
        [ -n "$flag" ] && flags+=("$flag")
    done < <(target_flags "$slice")
    # Spelt out so that an empty list passes `set -u` in bash 3.2.
    echo "== $slice: configure ${common[*]} ${flags[*]+${flags[*]}}"
    (cd "$build" && "$src/configure" "${common[@]}" ${flags[@]+"${flags[@]}"}) || {
        echo "::error::configure failed for $slice; see $build/ffbuild/config.log" >&2
        exit 1
    }
    # What the build is licensed under, as configure decided it from the
    # options above; anything but the LGPL means an option slipped in.
    license=$(sed -n 's/^#define FFMPEG_LICENSE "\(.*\)"$/\1/p' "$build/config.h")
    if [ "$license" != "LGPL version 2.1 or later" ]; then
        echo "::error::the $slice build would be under \"$license\", not the LGPL 2.1 or later" >&2
        exit 1
    fi
    make -C "$build" -j"$jobs" "$exe"
    built+=("$build/$exe")
done

rm -rf "${out:?}"
mkdir -p "$out"
if [ "$target" = macos ]; then
    lipo -create "${built[@]}" -output "$out/ffmpeg"
    for arch in arm64 x86_64; do
        lipo "$out/ffmpeg" -verify_arch "$arch"
    done
else
    cp "${built[0]}" "$out/$exe"
fi
chmod +x "$out/$exe"
cp "$src/COPYING.LGPLv2.1" "$out/LICENSE-ffmpeg.txt"
ls -l "$out"
echo "ffmpeg $version for $target in $out"
