#!/usr/bin/env bash
# Build the libmpv Patina.app is to carry, and every library it loads, from
# the sources pinned in macos-libmpv.env (FFmpeg's in bundled-tools.env):
# one universal (arm64 and x86_64) set of dylibs for macOS 11, both halves
# built on one Mac, whichever kind it is, and joined with lipo. Nothing comes
# from Homebrew or from anyone's prebuilt binaries; the build tools meson,
# ninja and cmake come from PyPI, pinned by digest in macos-build-tools.txt,
# and nasm and pkgconf are built here from their own pinned sources.
#
#   packaging/macos-libmpv-build.sh fetch <downloads dir>
#   packaging/macos-libmpv-build.sh tools <downloads dir> <tools dir>
#   LIBMPV_TOOLS=<tools dir> \
#   packaging/macos-libmpv-build.sh build <arm64|x86_64> <downloads dir> <work dir> <prefix>
#   packaging/macos-libmpv-build.sh universal <arm64 prefix> <x86_64 prefix> <out dir>
#   packaging/macos-libmpv-build.sh check <out dir> [<prefix>...]
#   packaging/macos-libmpv-build.sh flags [ffmpeg|mpv]
#
# fetch downloads every pinned source into <downloads dir> and checks its
# digest; a file already there is checked, not fetched again, as with every
# subcommand that downloads. It runs anywhere, Linux included, and runs
# nothing it downloads.
#
# tools makes <tools dir> afresh: a virtual environment holding meson, ninja
# and cmake, and bin/ holding nasm, pkgconf and pkg-config. meson needs
# Python 3.10 or later, which /usr/bin/python3 (the Xcode one, 3.9) is not,
# so PYTHON names the interpreter to make the environment with; it falls
# back on /usr/bin/python3 in case that is new enough.
#
# build builds one architecture into <prefix>, from nothing: it empties
# <work dir> and <prefix> first, and takes the tools from LIBMPV_TOOLS. It
# cross-compiles whichever architecture the Mac isn't, x86_64 on Apple
# silicon, and runs what meson or cmake build to test the compiler under
# Rosetta. <prefix> ends up an ordinary install tree, plus logs/ (each
# project's build output, <project>.log, and its configure log) and
# licenses/ (each project's licence files, as <project>-<file>). It fails if
# any library it installed loads one from anywhere but the system and the
# prefix.
#
# universal takes libmpv.2.dylib and every library it loads, directly or
# not, out of each prefix's lib/, as found by their install names (nothing
# else in lib/ is wanted), and writes into <out dir> one universal file of
# each, under its install name, and licenses/. Before joining it strips each
# file of its debug and local symbols, which also takes out the build paths
# the linker leaves in them, and rewrites the install names the way
# Contents/Frameworks needs them:
#   - each file's own name is @rpath/<file>, and so is each library of the
#     set it loads;
#   - every LC_RPATH is removed (mpv's link leaves the Xcode toolchain's
#     Swift directory and /usr/lib/swift in libmpv), and @loader_path is
#     added to each, so the set finds itself wherever it is: from
#     Patina.app's executable, whose own rpath also points at Frameworks
#     (build.rs), and from a test binary that loads libmpv through
#     PATINA_LIBMPV, which has no such rpath.
# The two architectures must give the same set of files. install_name_tool
# voids a signature and arm64 Macs load no dylib without a valid one, so
# each joined file is signed ad hoc; the bundle signs them again.
#
# check tells what can be told about <out dir> without loading it, and
# fails, after looking at everything, if any of it is wrong: each file has
# both architectures, each built for exactly macOS 11.0 (vtool); its install
# name is @rpath/<file>, its only LC_RPATH @loader_path, and each library it
# loads is either one of the set, by @rpath, or a system library on the list
# below; the set is exactly libmpv's closure, nothing more, and holds the
# libraries Patina relies on (LuaJIT among them, as a dylib of its own,
# which packaging/macos-sign.sh looks for); each signature verifies. Given
# the prefixes the build left, it also reads their logs for anything the
# compiler or linker says was built for, or uses an API of, a newer macOS
# than 11.0: a library built that way still says minos 11.0, and fails only
# on a Mac older than the one it needs.
#
# flags prints the configure options FFmpeg is built with (the ones every
# architecture shares), the meson options mpv is built with, or both.
#
# What is built, and why each is there, in the order it is built:
#   freetype, fribidi, harfbuzz, libass
#             subtitles. libass is mandatory for mpv, and these three are
#             its own dependencies. libass finds fonts through Core Text,
#             so no fontconfig.
#   uchardet  the subtitle charset detection that Patina's default, `auto`,
#             relies on; without it a subtitle file that isn't UTF-8 shows
#             as garbage.
#   libwebp   the encoder behind webp screenshots, which mpv looks up by
#             name (libwebp).
#   dav1d     AV1 decoding. FFmpeg's own av1 decoder only drives hardware
#             decoding, which VideoToolbox offers for AV1 only on M3 and
#             later, and YouTube serves much in AV1.
#   LuaJIT    Lua, for mpv's built-in scripts (ytdl_hook, through which
#             Patina plays online video, and the osc), without which
#             Patina refuses to start a player (src/player/mod.rs). Built
#             without LUAJIT_ENABLE_OSX_HRT, so it maps its JIT code without
#             MAP_JIT and the hardened runtime needs
#             allow-unsigned-executable-memory, as
#             packaging/macos-entitlements.plist grants.
#   OpenSSL   TLS for FFmpeg's https, linked into libavformat statically.
#             SecureTransport, macOS's own, has no TLS 1.3, and libcurl
#             stays off because macOS 11's is older than mpv's 7.88 floor.
#   FFmpeg    libav*: shared, LGPL (no --enable-gpl, --enable-version3 or
#             --enable-nonfree; config.h has to say "LGPL version 2.1 or
#             later", as packaging/ffmpeg.sh checks for the helper), every
#             component it has by default, VideoToolbox and AudioToolbox,
#             and nothing configure would find on the machine by itself
#             (--disable-autodetect). libavdevice holds only the lavfi
#             input, which av://lavfi URLs need: Patina's tests make their
#             test sound with it, and they run against this libmpv.
#   libplacebo  mpv's renderer, mandatory since 0.42; OpenGL only, no
#             Vulkan, so no MoltenVK, with its four submodules unpacked
#             into 3rdparty/.
#   mpv       libmpv only, LGPL (-Dgpl=false, which on macOS loses nothing:
#             only cdda, dvd, dvb, jack, oss, caca, direct3d and x11 are
#             GPL), with Cocoa and Swift, which the OpenGL render API needs
#             (gl-cocoa needs cocoa, and a Cocoa build without Swift fails to
#             link), and without what Patina does itself (media keys and Now
#             Playing, the touch bar) or doesn't use (JavaScript, archives,
#             Blu-ray, ICC profiles, zimg, libjpeg, rubberband). Patched
#             with macos-libmpv-view-without-vulkan.patch, without which
#             0.42.0 doesn't compile without Vulkan.
#
# Every project gets -mmacosx-version-min=11.0 (MACOSX_DEPLOYMENT_TARGET for
# those that ask the environment, -target for Swift), the macOS SDK of the
# selected Xcode, and -headerpad_max_install_names, which leaves room in
# each file's header for universal's rewriting. pkg-config sees the prefix
# and nothing else, cmake ignores /opt/homebrew and /usr/local, and PATH
# holds the tools and the system's directories only, so nothing on the
# machine finds its way in; check would catch it if it did.
#
# Runs with bash 3.2, the one macOS carries.
set -euo pipefail

usage() {
    echo "usage: macos-libmpv-build.sh fetch <downloads dir>" >&2
    echo "       macos-libmpv-build.sh tools <downloads dir> <tools dir>" >&2
    echo "       LIBMPV_TOOLS=<tools dir> macos-libmpv-build.sh build <arm64|x86_64> <downloads dir> <work dir> <prefix>" >&2
    echo "       macos-libmpv-build.sh universal <arm64 prefix> <x86_64 prefix> <out dir>" >&2
    echo "       macos-libmpv-build.sh check <out dir> [<prefix>...]" >&2
    echo "       macos-libmpv-build.sh flags [ffmpeg|mpv]" >&2
    exit 2
}

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
pins="$here/macos-libmpv.env"
shared_pins="$here/bundled-tools.env"
requirements="$here/macos-build-tools.txt"
mpv_patch="$here/macos-libmpv-view-without-vulkan.patch"

minos=11.0

# The sources build needs, in no particular order, and the two tools'.
sources="MPV LIBPLACEBO GLAD JINJA MARKUPSAFE FAST_FLOAT VULKAN_HEADERS LIBASS FREETYPE HARFBUZZ FRIBIDI LUAJIT UCHARDET DAV1D OPENSSL LIBWEBP FFMPEG"
tool_sources="NASM PKGCONF"

# The order build builds them in: each needs only what comes before it.
projects="freetype fribidi harfbuzz libass uchardet libwebp dav1d luajit openssl ffmpeg libplacebo mpv"

error() {
    echo "::error::$*" >&2
    exit 1
}

# A pin's value, from macos-libmpv.env, or from bundled-tools.env for
# FFmpeg's, whose URL and digest are named FFMPEG_SOURCE_URL and
# FFMPEG_SOURCE_SHA256 there.
pin() {
    local file=$pins key=$1 value
    case $key in
        FFMPEG_VERSION) file=$shared_pins ;;
        FFMPEG_URL) file=$shared_pins key=FFMPEG_SOURCE_URL ;;
        FFMPEG_SHA256) file=$shared_pins key=FFMPEG_SOURCE_SHA256 ;;
    esac
    value=$(sed -n "s/^$key=//p" "$file" | tr -d '\r')
    [ -n "$value" ] || error "$(basename "$file") has no $key"
    printf '%s' "$value"
}

sha256() {
    if command -v sha256sum >/dev/null; then
        sha256sum "$@" | cut -d' ' -f1
    else
        shasum -a 256 "$@" | cut -d' ' -f1
    fi
}

absolute() {
    (cd "$1" && pwd)
}

lower() {
    printf '%s' "$1" | tr '[:upper:]' '[:lower:]'
}

need_macos() {
    [ "$(uname -s)" = Darwin ] || error "$1 runs on macOS only"
}

# fetch NAME DIR: the pinned file, downloaded into DIR unless it is already
# there, checked against its digest either way; prints its path. The file is
# named for the project as well as the URL, since GitHub's archives are
# named for nothing but the tag or commit.
fetch() {
    local name=$1 dir=$2 url want file got
    url=$(pin "${name}_URL")
    want=$(pin "${name}_SHA256")
    file="$dir/$(lower "$name")-$(basename "$url")"
    if [ ! -f "$file" ]; then
        echo "fetching $url" >&2
        # Every error is worth a retry: the digest below is what makes the
        # bytes trustworthy, not the connection.
        curl -fsSL --retry 5 --retry-all-errors -o "$file.part" "$url" || error "could not download $url"
        mv "$file.part" "$file"
    fi
    got=$(sha256 "$file")
    if [ "$got" != "$want" ]; then
        error "$url ($file) has SHA-256 $got, not the pinned $want; delete it to download it again"
    fi
    printf '%s\n' "$file"
}

# unpack TARBALL DEST: the tarball's one top-level directory, as DEST,
# which must not exist yet.
unpack() {
    local tarball=$1 dest=$2 tmp entries
    [ ! -e "$dest" ] || error "$dest is in the way"
    tmp="$dest.unpack"
    rm -rf "$tmp"
    mkdir -p "$tmp"
    tar -xf "$tarball" -C "$tmp"
    entries=$(cd "$tmp" && ls -A)
    if [ "$(printf '%s\n' "$entries" | wc -l | tr -d ' ')" != 1 ] || [ ! -d "$tmp/$entries" ]; then
        error "$tarball does not hold exactly one top-level directory"
    fi
    mv "$tmp/$entries" "$dest"
    rmdir "$tmp"
}

# --- What FFmpeg and mpv are configured with ---------------------------------

# FFmpeg's configure options, the same for both architectures; build adds
# the target, the compilers, the tools and the prefix. Every component
# FFmpeg has by default stays in: mpv's filters, demuxers, protocols and
# screenshot encoders (png, mjpeg) are all among them. --pkg-config-flags
# =--static, so that configure's link tests see what the static OpenSSL
# needs; each other library in the prefix is a dylib all the same.
# --extra-libs=-liconv: with --disable-autodetect, configure looks for
# iconv only in libc, which on macOS hasn't it, and leaves iconv on all the
# same, so libavcodec failed to link; macOS's own is /usr/lib/libiconv.
ffmpeg_common=(
    --enable-shared --disable-static
    --install-name-dir=@rpath
    --disable-programs --disable-doc --disable-debug
    --disable-autodetect
    --enable-zlib --enable-bzlib --enable-iconv --extra-libs=-liconv
    --enable-videotoolbox --enable-audiotoolbox
    --enable-openssl --enable-libdav1d --enable-libwebp
    --disable-indevs --enable-indev=lavfi --disable-outdevs
    --pkg-config-flags=--static
)

# mpv's meson options, the same for both architectures; build adds Swift's
# -target. Each feature that matters is named, on or off, rather than left
# to what meson finds.
mpv_common=(
    -Dlibmpv=true -Dcplayer=false -Dgpl=false
    -Dbuild-date=false -Dtests=false -Dfuzzers=false
    -Dmanpage-build=disabled -Dhtml-build=disabled -Dpdf-build=disabled
    -Dlua=luajit -Djavascript=disabled
    -Duchardet=enabled -Diconv=enabled -Dzlib=enabled -Dlibavdevice=enabled
    -Dlibarchive=disabled -Dlibbluray=disabled -Dlcms2=disabled -Dzimg=disabled
    -Djpeg=disabled -Drubberband=disabled -Dlibcurl=disabled -Dsubrandr=disabled
    -Dvapoursynth=disabled
    -Dcocoa=enabled -Dswift-build=enabled
    -Dgl=enabled -Dgl-cocoa=enabled -Dvideotoolbox-gl=enabled
    -Dvulkan=disabled -Dvideotoolbox-pl=disabled
    -Dcoreaudio=enabled -Davfoundation=enabled
    -Dmacos-cocoa-cb=disabled -Dmacos-media-player=disabled -Dmacos-touchbar=disabled
)

cmd_flags() {
    case "${1:-}" in
        ffmpeg) printf '%s\n' "${ffmpeg_common[@]}" ;;
        mpv) printf '%s\n' "${mpv_common[@]}" ;;
        '')
            echo "# FFmpeg"
            printf '%s\n' "${ffmpeg_common[@]}"
            echo "# mpv"
            printf '%s\n' "${mpv_common[@]}"
            ;;
        *) usage ;;
    esac
}

cmd_fetch() {
    local downloads=$1 name
    mkdir -p "$downloads"
    for name in $sources $tool_sources; do
        fetch "$name" "$downloads" >/dev/null
    done
    echo "every pinned source in $downloads, as pinned"
}

# --- The environment every build runs in -------------------------------------

# Nothing from the caller's environment that a compiler, linker,
# pkg-config or cmake would read, and a PATH of the tools (if given) and the
# system's directories alone, which leaves out Homebrew (/opt/homebrew and
# /usr/local) and anything else installed on the machine. universal and check
# call it too, so that otool, lipo, strip, install_name_tool, vtool and
# codesign are the /usr/bin shims of the Xcode xcode-select chose and not
# Homebrew's, which come first on PATH on GitHub's macOS runners.
clean_environment() {
    unset CFLAGS CXXFLAGS OBJCFLAGS OBJCXXFLAGS CPPFLAGS LDFLAGS LIBS \
        CC CXX OBJC OBJCXX CPP LD AR NM RANLIB STRIP \
        CPATH C_INCLUDE_PATH CPLUS_INCLUDE_PATH OBJC_INCLUDE_PATH LIBRARY_PATH \
        PKG_CONFIG PKG_CONFIG_PATH PKG_CONFIG_LIBDIR PKG_CONFIG_SYSROOT_DIR \
        CMAKE_PREFIX_PATH CMAKE_FRAMEWORK_PATH CMAKE_APPBUNDLE_PATH \
        CMAKE_INCLUDE_PATH CMAKE_LIBRARY_PATH CMAKE_PROGRAM_PATH \
        DYLD_LIBRARY_PATH DYLD_FALLBACK_LIBRARY_PATH DYLD_FRAMEWORK_PATH \
        DYLD_INSERT_LIBRARIES PYTHONPATH PYTHONHOME \
        MACOSX_DEPLOYMENT_TARGET SDKROOT MACOS_SDK MACOS_SDK_VERSION
    local path=/usr/bin:/bin:/usr/sbin:/sbin
    if [ -n "${1:-}" ]; then
        path="$1/bin:$1/venv/bin:$path"
    fi
    export PATH=$path
}

# What builds this, for the log: CI runners change under a pinned label.
toolchain_report() {
    xcodebuild -version
    echo "SDK $(xcrun --sdk macosx --show-sdk-version) at $(xcrun --sdk macosx --show-sdk-path)"
    xcrun clang --version | head -n1
    xcrun swiftc --version 2>&1 | head -n1
    if [ -n "${1:-}" ]; then
        echo "meson $("$1/venv/bin/meson" --version)"
        echo "ninja $("$1/venv/bin/ninja" --version)"
        "$1/venv/bin/cmake" --version | head -n1
        "$1/bin/nasm" -v
        echo "pkgconf $("$1/bin/pkgconf" --version)"
        "$1/venv/bin/python" --version
    fi
}

jobs=$(sysctl -n hw.ncpu 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || echo 2)

# --- tools --------------------------------------------------------------------

# The first of PYTHON and /usr/bin/python3 that is Python 3.10 or later.
find_python() {
    local candidate
    for candidate in ${PYTHON:+"$PYTHON"} /usr/bin/python3; do
        if "$candidate" -c 'import sys; sys.exit(sys.version_info < (3, 10))' 2>/dev/null; then
            printf '%s\n' "$candidate"
            return
        fi
    done
    error "found no Python 3.10 or later for meson; set PYTHON to one (/usr/bin/python3 is $(/usr/bin/python3 --version 2>&1 || echo missing))"
}

cmd_tools() {
    local downloads=$1 tools=$2 python src
    need_macos tools
    clean_environment
    python=$(find_python)
    mkdir -p "$downloads"
    downloads=$(absolute "$downloads")
    rm -rf "${tools:?}"
    mkdir -p "$tools/bin"
    tools=$(absolute "$tools")

    echo "== meson, ninja and cmake, with $python ($("$python" --version 2>&1))"
    "$python" -m venv "$tools/venv"
    "$tools/venv/bin/python" -m pip install --disable-pip-version-check --no-cache-dir \
        --require-hashes --only-binary :all: --no-deps -r "$requirements"

    # nasm and pkgconf for this Mac, from their own sources. Only the
    # programs are kept.
    mkdir -p "$tools/src"
    echo "== nasm"
    src="$tools/src/nasm"
    unpack "$(fetch NASM "$downloads")" "$src"
    (cd "$src" && ./configure && make -j"$jobs" nasm)
    cp "$src/nasm" "$tools/bin/nasm"

    echo "== pkgconf"
    src="$tools/src/pkgconf"
    unpack "$(fetch PKGCONF "$downloads")" "$src"
    # Static, so the program needs no libpkgconf beside it. The system
    # directories are named so that it leaves -L/usr/lib and -I/usr/include
    # out of what it prints, as the system's own would.
    (cd "$src" && ./configure --prefix="$tools/src/pkgconf-install" --disable-shared \
        --with-system-libdir=/usr/lib --with-system-includedir=/usr/include \
        && make -j"$jobs" && make install)
    cp "$tools/src/pkgconf-install/bin/pkgconf" "$tools/bin/pkgconf"
    ln -sf pkgconf "$tools/bin/pkg-config"
    rm -rf "$tools/src"

    echo "== tools in $tools"
    toolchain_report "$tools"
}

# --- build --------------------------------------------------------------------

# The meson cross file for one architecture, used for the native one too, so
# that every flag comes from here and nothing from the environment. Rosetta
# runs the x86_64 programs meson builds to test the compiler, so meson is
# told it needs no wrapper to run them.
cross_file() {
    local cpu flags link
    case $arch in
        arm64) cpu=aarch64 ;;
        x86_64) cpu=x86_64 ;;
    esac
    flags="'-arch', '$arch', '-mmacosx-version-min=$minos', '-isysroot', '$SDKROOT'"
    link="$flags, '-Wl,-headerpad_max_install_names'"
    cat <<EOF
[binaries]
c = '$cc'
cpp = '$cxx'
objc = '$cc'
objcpp = '$cxx'
ar = '$ar'
strip = '$strip'
pkg-config = '$tools/bin/pkgconf'
nasm = '$tools/bin/nasm'

[built-in options]
c_args = [$flags]
c_link_args = [$link]
cpp_args = [$flags]
cpp_link_args = [$link]
objc_args = [$flags]
objc_link_args = [$link]
objcpp_args = [$flags]
objcpp_link_args = [$link]

[properties]
pkg_config_libdir = ['$prefix/lib/pkgconfig']
needs_exe_wrapper = false

[host_machine]
system = 'darwin'
subsystem = 'macos'
kernel = 'xnu'
cpu_family = '$cpu'
cpu = '$cpu'
endian = 'little'
EOF
}

# license PROJECT SRC FILE...: the project's licence files, into licenses/.
license() {
    local project=$1 src=$2 file
    shift 2
    mkdir -p "$licenses"
    for file in "$@"; do
        [ -f "$src/$file" ] || error "$project has no $file"
        cp "$src/$file" "$licenses/$project-$(basename "$file")"
    done
}

# source NAME DEST: the pinned tarball, unpacked as DEST.
source_tree() {
    unpack "$downloads/$(lower "$1")-$(basename "$(pin "$1_URL")")" "$2"
}

meson_project() {
    local src=$1 build=$2
    shift 2
    meson setup "$build" "$src" --cross-file "$cross" --prefix "$prefix" --libdir lib \
        --buildtype release --default-library shared --wrap-mode nodownload "$@"
    meson compile -C "$build"
    meson install -C "$build"
}

cmake_project() {
    local src=$1 build=$2
    shift 2
    cmake -S "$src" -B "$build" -G Ninja \
        -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_INSTALL_PREFIX="$prefix" -DCMAKE_INSTALL_LIBDIR=lib \
        -DCMAKE_C_COMPILER="$cc" -DCMAKE_CXX_COMPILER="$cxx" \
        -DCMAKE_OSX_ARCHITECTURES="$arch" -DCMAKE_OSX_DEPLOYMENT_TARGET="$minos" \
        -DCMAKE_OSX_SYSROOT="$SDKROOT" \
        -DCMAKE_SHARED_LINKER_FLAGS=-Wl,-headerpad_max_install_names \
        -DCMAKE_PREFIX_PATH="$prefix" \
        "-DCMAKE_IGNORE_PREFIX_PATH=/opt/homebrew;/usr/local;/opt/local" \
        -DCMAKE_FIND_USE_PACKAGE_REGISTRY=OFF \
        -DBUILD_SHARED_LIBS=ON "$@"
    cmake --build "$build"
    cmake --install "$build"
}

build_freetype() {
    local src="$work/src/freetype"
    source_tree FREETYPE "$src"
    license freetype "$src" LICENSE.TXT docs/FTL.TXT
    # HVF: freetype links Apple's libhvf when the SDK has its header, a
    # library macOS 11 doesn't have.
    meson_project "$src" "$work/build/freetype" \
        -Dharfbuzz=disabled -Dpng=disabled -Dbrotli=disabled -Dbzip2=disabled \
        -Dzlib=system -Dhvf=disabled -Dtests=disabled
}

build_fribidi() {
    local src="$work/src/fribidi"
    source_tree FRIBIDI "$src"
    license fribidi "$src" COPYING
    meson_project "$src" "$work/build/fribidi" -Ddocs=false -Dtests=false -Dbin=false
}

build_harfbuzz() {
    local src="$work/src/harfbuzz"
    source_tree HARFBUZZ "$src"
    license harfbuzz "$src" COPYING
    # libharfbuzz alone: raster, vector, gpu and subset are libraries of
    # their own, and on by default.
    meson_project "$src" "$work/build/harfbuzz" \
        -Dfreetype=enabled -Dglib=disabled -Dgobject=disabled -Dcairo=disabled \
        -Dchafa=disabled -Dicu=disabled -Dgraphite=disabled -Dfontations=disabled \
        -Dpng=disabled -Dzlib=disabled -Dcoretext=disabled \
        -Draster=disabled -Dvector=disabled -Dgpu=disabled -Dgpu_demo=disabled \
        -Dsubset=disabled -Dutilities=disabled -Dtests=disabled -Ddocs=disabled \
        -Dintrospection=disabled -Dbenchmark=disabled
}

build_libass() {
    local src="$work/src/libass"
    source_tree LIBASS "$src"
    license libass "$src" COPYING
    meson_project "$src" "$work/build/libass" \
        -Dfontconfig=disabled -Dcoretext=enabled -Drequire-system-font-provider=true \
        -Dlibunibreak=disabled -Dtest=disabled -Dcompare=disabled -Dprofile=disabled \
        -Dfuzz=disabled -Dcheckasm=disabled
}

build_uchardet() {
    local src="$work/src/uchardet"
    source_tree UCHARDET "$src"
    license uchardet "$src" COPYING
    # Its CMakeLists.txt asks for cmake 3.1, which cmake 4 refuses without
    # this. TARGET_ARCHITECTURE stands in for its own guess, which reads the
    # Mac's processor rather than the target's.
    cmake_project "$src" "$work/build/uchardet" \
        -DCMAKE_POLICY_VERSION_MINIMUM=3.5 \
        -DBUILD_BINARY=OFF -DBUILD_STATIC=OFF -DTARGET_ARCHITECTURE="$arch"
}

build_libwebp() {
    local src="$work/src/libwebp"
    source_tree LIBWEBP "$src"
    license libwebp "$src" COPYING PATENTS
    # The libraries alone. CMAKE_INSTALL_RPATH, set (to nothing), keeps
    # libwebp from giving its libraries an rpath of the prefix.
    cmake_project "$src" "$work/build/libwebp" \
        -DWEBP_BUILD_ANIM_UTILS=OFF -DWEBP_BUILD_CWEBP=OFF -DWEBP_BUILD_DWEBP=OFF \
        -DWEBP_BUILD_GIF2WEBP=OFF -DWEBP_BUILD_IMG2WEBP=OFF -DWEBP_BUILD_VWEBP=OFF \
        -DWEBP_BUILD_WEBPINFO=OFF -DWEBP_BUILD_WEBPMUX=OFF -DWEBP_BUILD_EXTRAS=OFF \
        -DCMAKE_INSTALL_RPATH=
}

build_dav1d() {
    local src="$work/src/dav1d"
    source_tree DAV1D "$src"
    license dav1d "$src" COPYING
    meson_project "$src" "$work/build/dav1d" \
        -Denable_tools=false -Denable_tests=false -Denable_examples=false \
        -Denable_docs=false -Dxxhash_muxer=disabled
}

build_luajit() {
    local src="$work/src/luajit"
    source_tree LUAJIT "$src"
    license luajit "$src" COPYRIGHT
    # LuaJIT builds in its source tree, with its own make. HOST_CC builds
    # the programs that generate its VM (minilua, buildvm) for this Mac;
    # TARGET_FLAGS alone carries the target, which LuaJIT also reads to
    # choose what VM to generate. Both are 64-bit, as a cross build of it
    # requires. CFLAGS and LDFLAGS are emptied so nothing reaches the host
    # programs. Its Makefile refuses to build for macOS without
    # MACOSX_DEPLOYMENT_TARGET, which the environment has.
    local vars
    vars=(
        "CC=$cc" "HOST_CC=$cc" CFLAGS= LDFLAGS=
        "HOST_CFLAGS=-isysroot $SDKROOT" "HOST_LDFLAGS=-isysroot $SDKROOT"
        "TARGET_FLAGS=-arch $arch -mmacosx-version-min=$minos -isysroot $SDKROOT"
        "TARGET_SHLDFLAGS=-Wl,-headerpad_max_install_names"
        "PREFIX=$prefix"
    )
    make -C "$src" -j"$jobs" "${vars[@]}" amalg
    make -C "$src" "${vars[@]}" install
    # Only the dylib: a static libluajit beside it could be linked into
    # libmpv instead, which nothing would notice (macos-sign.sh looks for
    # libluajit*.dylib).
    rm -f "$prefix/lib/libluajit-5.1.a"
}

build_openssl() {
    local src="$work/src/openssl" build="$work/build/openssl"
    source_tree OPENSSL "$src"
    license openssl "$src" LICENSE.txt
    # Static and without its programs, tests, docs or loadable modules: all
    # that is wanted is libssl and libcrypto to link into libavformat. Its
    # configuration directory is macOS's own /etc/ssl, whose cert.pem
    # verifies a server if anyone turns on --tls-verify; mpv and FFmpeg
    # leave verification off by default.
    mkdir -p "$build"
    (cd "$build" && CC="$cc" "$src/Configure" "darwin64-$arch-cc" \
        --prefix="$prefix" --libdir=lib --openssldir=/private/etc/ssl \
        no-shared no-module no-tests no-docs no-apps \
        "-mmacosx-version-min=$minos" "-isysroot$SDKROOT")
    make -C "$build" -j"$jobs" build_libs
    make -C "$build" install_dev
}

build_ffmpeg() {
    local src="$work/src/ffmpeg" build="$work/build/ffmpeg" config license
    source_tree FFMPEG "$src"
    # LGPL 2.1 or later as configured, and linked with OpenSSL, whose
    # Apache License 2.0 goes with the LGPL's version 3, so both texts.
    license ffmpeg "$src" LICENSE.md COPYING.LGPLv2.1 COPYING.LGPLv3
    mkdir -p "$build"
    (cd "$build" && "$src/configure" --prefix="$prefix" \
        --enable-cross-compile --target-os=darwin "--arch=$arch" \
        "--cc=$cc -arch $arch" "--cxx=$cxx -arch $arch" "--objcc=$cc -arch $arch" \
        "--x86asmexe=$tools/bin/nasm" "--pkg-config=$tools/bin/pkgconf" \
        "--extra-cflags=-mmacosx-version-min=$minos -isysroot $SDKROOT" \
        "--extra-objcflags=-mmacosx-version-min=$minos -isysroot $SDKROOT" \
        "--extra-ldflags=-mmacosx-version-min=$minos -isysroot $SDKROOT -Wl,-headerpad_max_install_names" \
        "${ffmpeg_common[@]}") || {
        cp "$build/ffbuild/config.log" "$logs/ffmpeg.config.txt" 2>/dev/null || true
        error "FFmpeg's configure failed for $arch; see logs/ffmpeg.config.txt"
    }
    cp "$build/ffbuild/config.log" "$logs/ffmpeg.config.txt"
    # What the build is licensed under, as configure decided it from the
    # options; anything but the LGPL means an option slipped in.
    license=$(sed -n 's/^#define FFMPEG_LICENSE "\(.*\)"$/\1/p' "$build/config.h")
    if [ "$license" != "LGPL version 2.1 or later" ]; then
        error "FFmpeg for $arch would be under \"$license\", not the LGPL 2.1 or later"
    fi
    # And that what was asked for is there: configure leaves out a library
    # it was told to use only by failing, but a component can drop out
    # quietly.
    config="$build/config.h $build/config_components.h"
    for want in CONFIG_ZLIB CONFIG_BZLIB CONFIG_ICONV CONFIG_VIDEOTOOLBOX CONFIG_AUDIOTOOLBOX \
        CONFIG_OPENSSL CONFIG_LIBDAV1D CONFIG_LIBWEBP CONFIG_LIBDAV1D_DECODER \
        CONFIG_LIBWEBP_ENCODER CONFIG_PNG_ENCODER CONFIG_MJPEG_ENCODER \
        CONFIG_HTTPS_PROTOCOL CONFIG_TLS_PROTOCOL CONFIG_LAVFI_INDEV CONFIG_AVDEVICE; do
        # shellcheck disable=SC2086  # two files
        grep -qx "#define $want 1" $config || error "FFmpeg for $arch was configured without $want"
    done
    for unwanted in CONFIG_GPL CONFIG_VERSION3 CONFIG_NONFREE CONFIG_SECURETRANSPORT; do
        # shellcheck disable=SC2086
        grep -qx "#define $unwanted 0" $config || error "FFmpeg for $arch was configured with $unwanted"
    done
    make -C "$build" -j"$jobs"
    make -C "$build" install
}

build_libplacebo() {
    local src="$work/src/libplacebo" sub name
    source_tree LIBPLACEBO "$src"
    license libplacebo "$src" LICENSE
    # A tag's archive holds its submodules as empty directories.
    for sub in glad jinja markupsafe fast_float Vulkan-Headers; do
        rmdir "$src/3rdparty/$sub"
        name=$(printf '%s' "$sub" | tr '[:lower:]-' '[:upper:]_')
        source_tree "$name" "$src/3rdparty/$sub"
    done
    # glad generates the OpenGL loader that is compiled in, and fast_float
    # is a header that is; jinja and markupsafe only run at build time, and
    # Vulkan-Headers only gives the types the Vulkan stubs, built without
    # Vulkan, are declared with.
    license libplacebo-glad "$src/3rdparty/glad" LICENSE
    license libplacebo-fast_float "$src/3rdparty/fast_float" LICENSE-MIT
    meson_project "$src" "$work/build/libplacebo" \
        -Dvulkan=disabled -Dvk-proc-addr=disabled -Dopengl=enabled -Dd3d11=disabled \
        -Dglslang=disabled -Dshaderc=disabled -Dlcms=disabled -Dlibdovi=disabled \
        -Dxxhash=disabled -Dunwind=disabled -Ddemos=false -Dtests=false
}

build_mpv() {
    local src="$work/src/mpv" build="$work/build/mpv" want
    source_tree MPV "$src"
    license mpv "$src" LICENSE.LGPL Copyright
    patch -p1 -N -d "$src" -i "$mpv_patch"
    grep -q '^#if HAVE_VULKAN$' "$src/video/out/mac/view.swift" \
        || error "$(basename "$mpv_patch") did not apply"
    # Swift takes the target from -target alone; its triples name the
    # architectures as macOS does.
    meson setup "$build" "$src" --cross-file "$cross" --prefix "$prefix" --libdir lib \
        --buildtype release --default-library shared --wrap-mode nodownload \
        "${mpv_common[@]}" "-Dswift-flags=-target $arch-apple-macosx$minos"
    # The features that matter, as meson decided them; config.h has every
    # one, 1 or 0. Without GPL parts, so libmpv is LGPL 2.1 or later.
    for want in COCOA SWIFT GL GL_COCOA VIDEOTOOLBOX_GL LUA UCHARDET ICONV ZLIB COREAUDIO LIBAVDEVICE; do
        grep -qx "#define HAVE_$want 1" "$build/config.h" || error "mpv for $arch was configured without $want"
    done
    for want in GPL VULKAN LIBCURL JAVASCRIPT LIBARCHIVE LIBBLURAY RUBBERBAND; do
        grep -qx "#define HAVE_$want 0" "$build/config.h" || error "mpv for $arch was configured with $want"
    done
    meson compile -C "$build"
    meson install -C "$build"
}

# Each project's build output goes to logs/<project>.log and to the
# terminal, folded into a group in GitHub's log; meson's and cmake's own
# configure logs are kept beside it, under names check doesn't read (a
# configure test may well use a newer API on purpose).
project() {
    local name=$1 log="$logs/$1.log" status
    echo "::group::$name ($arch)"
    set +e
    (set -e; "build_$name") 2>&1 | tee "$log"
    status=${PIPESTATUS[0]}
    set -e
    echo "::endgroup::"
    if [ -f "$work/build/$name/meson-logs/meson-log.txt" ]; then
        cp "$work/build/$name/meson-logs/meson-log.txt" "$logs/$name.config.txt"
    elif [ -f "$work/build/$name/CMakeFiles/CMakeConfigureLog.yaml" ]; then
        cp "$work/build/$name/CMakeFiles/CMakeConfigureLog.yaml" "$logs/$name.config.txt"
    fi
    if [ "$status" -ne 0 ]; then
        echo "--- the end of logs/$name.log:" >&2
        tail -n 60 "$log" >&2
        error "$name failed to build for $arch (exit $status); its logs are in $logs"
    fi
}

# Every library the build installed loads only the system's libraries and
# the prefix's own, by @rpath or by the prefix's path. Anything else was
# found somewhere else on the machine: mpv's meson, for one, adds
# -L/usr/local/lib to its link. Checked here, where the prefix's path is
# known; universal and check see the libraries elsewhere.
check_links() {
    local file name commands kind dep bad=0
    for file in "$prefix/lib/"*.dylib; do
        [ -L "$file" ] && continue
        name=${file##*/}
        commands=$(load_commands "$file" "$arch")
        while read -r kind dep; do
            case $kind in LC_ID_DYLIB | LC_RPATH | '') continue ;; esac
            is_system "$dep" && continue
            case $dep in
                @rpath/*) [ -e "$prefix/lib/${dep#@rpath/}" ] && continue ;;
                "$prefix/lib/"*) [ -e "$dep" ] && continue ;;
            esac
            echo "::error::$name loads $dep, which is not in $prefix/lib" >&2
            bad=1
        done <<< "$commands"
    done
    [ "$bad" = 0 ] || error "the $arch libraries load libraries from outside the build"
}

# Lines of <dir>/*.log in which the compiler or linker says something was
# built for, or uses an API of, a newer macOS than the target. clang and ld
# say 'on macOS' and Swift says 'in macOS', and in Swift 5 mode, which mpv
# compiles its Swift in, some of Swift's are only warnings.
newer_macos() {
    grep -H -n -E -e 'built for newer' -e 'is only available (on|in) macOS [0-9]' \
        -e '-Wunguarded-availability' "$1"/*.log || true
}

cmd_build() {
    local name found
    arch=$1 downloads=$2 work=$3 prefix=$4
    case $arch in
        arm64 | x86_64) ;;
        *) usage ;;
    esac
    need_macos build
    tools=${LIBMPV_TOOLS:-}
    [ -n "$tools" ] || error "LIBMPV_TOOLS has to name the directory packaging/macos-libmpv-build.sh tools made"
    for name in bin/nasm bin/pkgconf venv/bin/meson venv/bin/ninja venv/bin/cmake; do
        [ -x "$tools/$name" ] || error "$tools has no $name; run packaging/macos-libmpv-build.sh tools first"
    done
    tools=$(absolute "$tools")
    mkdir -p "$downloads"
    downloads=$(absolute "$downloads")
    rm -rf "${work:?}" "${prefix:?}"
    mkdir -p "$work/src" "$work/build" "$prefix/lib/pkgconfig"
    work=$(absolute "$work")
    prefix=$(absolute "$prefix")
    logs="$prefix/logs"
    licenses="$prefix/licenses"
    mkdir -p "$logs" "$licenses"

    clean_environment "$tools"
    export MACOSX_DEPLOYMENT_TARGET=$minos
    SDKROOT=$(xcrun --sdk macosx --show-sdk-path)
    export SDKROOT
    # mpv's meson asks these for the SDK before it asks xcrun.
    export MACOS_SDK=$SDKROOT
    MACOS_SDK_VERSION=$(xcrun --sdk macosx --show-sdk-version)
    export MACOS_SDK_VERSION
    export PKG_CONFIG="$tools/bin/pkgconf"
    export PKG_CONFIG_LIBDIR="$prefix/lib/pkgconfig"
    cc=$(xcrun --find clang)
    cxx=$(xcrun --find clang++)
    ar=$(xcrun --find ar)
    strip=$(xcrun --find strip)

    echo "== libmpv for $arch, macOS $minos, in $prefix"
    toolchain_report "$tools" | tee "$logs/toolchain.txt"

    # Every source first, so that a bad pin fails in a minute rather than
    # halfway through.
    for name in $sources; do
        fetch "$name" "$downloads" >/dev/null
    done

    cross="$work/cross-$arch.ini"
    cross_file > "$cross"
    cp "$cross" "$logs/cross-$arch.ini.txt"

    for name in $projects; do
        project "$name"
    done

    check_links

    # Reported here, and failed on by check, which is given the logs: the
    # libraries are still worth joining and loading to learn what else is
    # wrong.
    found=$(newer_macos "$logs")
    if [ -n "$found" ]; then
        printf '%s\n' "$found" | while IFS= read -r line; do
            echo "::warning::$line"
        done
    fi
    ls -l "$prefix/lib/"*.dylib
    echo "libmpv for $arch in $prefix"
}

# --- universal and check ------------------------------------------------------

# load_commands FILE ARCH: one line per load command that names a library
# or an rpath, as "<command> <path>".
load_commands() {
    otool -arch "$2" -l "$1" | awk '
        $1 == "cmd" { cmd = $2; next }
        $1 == "name" && cmd ~ /^LC_(ID|LOAD|LOAD_WEAK|REEXPORT|LAZY_LOAD|LOAD_UPWARD)_DYLIB$/ { print cmd, $2; next }
        $1 == "path" && cmd == "LC_RPATH" { print cmd, $2 }'
}

is_system() {
    case $1 in
        /usr/lib/* | /System/Library/*) return 0 ;;
    esac
    return 1
}

# gather ARCH PREFIX STAGE: libmpv.2.dylib and every library it loads,
# directly or not, copied out of PREFIX/lib into STAGE under the names they
# are loaded by, which are their install names' last parts (the build gives
# some @rpath ones, others the prefix's absolute path). Anything else,
# outside the system, is an error: it would be a library from somewhere on
# the build machine.
gather() {
    local arch=$1 prefix=$2 stage=$3 queue="libmpv.2.dylib" seen=" " name commands kind dep base
    mkdir -p "$stage"
    while [ -n "$queue" ]; do
        # shellcheck disable=SC2086  # names, one word each
        set -- $queue
        name=$1
        shift
        queue="$*"
        case $seen in *" $name "*) continue ;; esac
        seen="$seen$name "
        [ -f "$prefix/lib/$name" ] || error "$prefix/lib has no $name"
        cp -L "$prefix/lib/$name" "$stage/$name"
        chmod u+w "$stage/$name"
        commands=$(load_commands "$stage/$name" "$arch")
        while read -r kind dep; do
            case $kind in LC_ID_DYLIB | LC_RPATH | '') continue ;; esac
            is_system "$dep" && continue
            base=${dep##*/}
            case $dep in
                @rpath/* | /*)
                    if [ -f "$prefix/lib/$base" ]; then
                        queue="$queue $base"
                        continue
                    fi
                    ;;
            esac
            error "$arch $name loads $dep, which is neither the system's nor in $prefix/lib"
        done <<< "$commands"
    done
}

# rewrite FILE ARCH: stripped, named @rpath/<file>, loading the set by
# @rpath, with @loader_path its only rpath.
rewrite() {
    local file=$1 arch=$2 name commands kind dep changes rpath count=0
    name=${file##*/}
    strip -S -x "$file"
    changes=(-id "@rpath/$name")
    commands=$(load_commands "$file" "$arch")
    while read -r kind dep; do
        case $kind in LC_ID_DYLIB | LC_RPATH | '') continue ;; esac
        is_system "$dep" && continue
        if [ "$dep" != "@rpath/${dep##*/}" ]; then
            changes+=(-change "$dep" "@rpath/${dep##*/}")
        fi
    done <<< "$commands"
    install_name_tool "${changes[@]}" "$file"
    # One at a time: the same path can be there twice.
    while rpath=$(load_commands "$file" "$arch" | awk '$1 == "LC_RPATH" { print $2; exit }') && [ -n "$rpath" ]; do
        count=$((count + 1))
        [ "$count" -le 32 ] || error "could not remove the rpaths of $arch $name"
        install_name_tool -delete_rpath "$rpath" "$file"
    done
    install_name_tool -add_rpath @loader_path "$file"
}

cmd_universal() {
    local arm=$1 intel=$2 out=$3 stage arch prefix names other name
    need_macos universal
    clean_environment
    arm=$(absolute "$arm")
    intel=$(absolute "$intel")
    stage=$(mktemp -d "${TMPDIR:-/tmp}/libmpv-universal.XXXXXX")
    for arch in arm64 x86_64; do
        prefix=$arm
        [ "$arch" = x86_64 ] && prefix=$intel
        echo "== $arch, from $prefix"
        gather "$arch" "$prefix" "$stage/$arch"
        for name in "$stage/$arch"/*; do
            lipo "$name" -verify_arch "$arch" || error "$prefix/lib/${name##*/} has no $arch"
            rewrite "$name" "$arch"
        done
    done
    names=$(cd "$stage/arm64" && printf '%s\n' * | LC_ALL=C sort)
    other=$(cd "$stage/x86_64" && printf '%s\n' * | LC_ALL=C sort)
    if [ "$names" != "$other" ]; then
        diff <(echo "$names") <(echo "$other") >&2 || true
        error "libmpv loads a different set of libraries on arm64 (<) than on x86_64 (>)"
    fi
    if ! diff -r "$arm/licenses" "$intel/licenses" >/dev/null; then
        error "the two prefixes have different licenses/"
    fi

    rm -rf "${out:?}"
    mkdir -p "$out"
    for name in $names; do
        lipo -create "$stage/arm64/$name" "$stage/x86_64/$name" -output "$out/$name"
        codesign --force --sign - "$out/$name"
    done
    cp -R "$arm/licenses" "$out/licenses"
    rm -rf "$stage"
    ls -l "$out"
    echo "$(echo "$names" | wc -l | tr -d ' ') universal libraries in $out"
}

# The system libraries a file in the set may load, every one of which macOS
# 11 has: the C, C++ and Objective-C runtimes and the few libraries FFmpeg
# and mpv take from /usr/lib; the Swift libraries mpv's Swift code links,
# those of them that macOS 11 already carried (IINA's libmpv for macOS 11
# links the same ones); and the frameworks. A weak link to any other library
# in /usr/lib/swift or any other system framework passes too: dyld skips a
# weak library that isn't there, and that is how the linker links the Swift
# libraries newer than the target. Anything else fails check, since a
# library macOS 11 lacks stops libmpv loading there, and no runner CI can
# use runs macOS 11. Add to these only what macOS 11 is known to have.
system_libraries="/usr/lib/libSystem.B.dylib /usr/lib/libc++.1.dylib /usr/lib/libobjc.A.dylib"
system_libraries="$system_libraries /usr/lib/libiconv.2.dylib /usr/lib/libz.1.dylib /usr/lib/libbz2.1.0.dylib"
swift_libraries="libswiftCore libswiftFoundation libswiftDispatch libswiftObjectiveC libswiftos"
swift_libraries="$swift_libraries libswiftDarwin libswiftCoreFoundation libswiftCoreGraphics libswiftCoreImage"
swift_libraries="$swift_libraries libswiftAppKit libswiftIOKit libswiftMetal libswiftQuartzCore libswiftXPC"
swift_libraries="$swift_libraries libswiftsimd libswiftUniformTypeIdentifiers"
system_frameworks="Accelerate AppKit ApplicationServices AudioToolbox AudioUnit AVFoundation Carbon"
system_frameworks="$system_frameworks Cocoa CoreAudio CoreFoundation CoreGraphics CoreImage CoreMedia"
system_frameworks="$system_frameworks CoreServices CoreText CoreVideo DiskArbitration Foundation IOKit"
system_frameworks="$system_frameworks IOSurface Metal OpenGL QuartzCore Security UniformTypeIdentifiers"
system_frameworks="$system_frameworks VideoToolbox"

# system_allowed PATH WEAK
system_allowed() {
    local path=$1 weak=$2 name
    case " $system_libraries " in *" $path "*) return 0 ;; esac
    case $path in
        /usr/lib/swift/libswift*.dylib)
            name=${path#/usr/lib/swift/}
            name=${name%.dylib}
            case " $swift_libraries " in *" $name "*) return 0 ;; esac
            ;;
        /System/Library/Frameworks/*.framework/Versions/*)
            name=${path#/System/Library/Frameworks/}
            name=${name%%.framework/*}
            case $path in
                "/System/Library/Frameworks/$name.framework/Versions/"?"/$name")
                    case " $system_frameworks " in *" $name "*) return 0 ;; esac
                    ;;
            esac
            ;;
        *) return 1 ;;
    esac
    [ "$weak" = 1 ]
}

# closure ARCH DIR: libmpv.2.dylib and every file in DIR it loads, directly
# or not, by @rpath, sorted.
closure() {
    local arch=$1 dir=$2 queue="libmpv.2.dylib" seen=" " name kind dep
    while [ -n "$queue" ]; do
        # shellcheck disable=SC2086
        set -- $queue
        name=$1
        shift
        queue="$*"
        case $seen in *" $name "*) continue ;; esac
        seen="$seen$name "
        [ -f "$dir/$name" ] || continue
        while read -r kind dep; do
            case $kind in LC_ID_DYLIB | LC_RPATH | '') continue ;; esac
            case $dep in @rpath/*) queue="$queue ${dep#@rpath/}" ;; esac
        done <<< "$(load_commands "$dir/$name" "$arch")"
    done
    # shellcheck disable=SC2086
    printf '%s\n' $seen | LC_ALL=C sort
}

cmd_check() {
    local out=$1 failed=0 entry name file arch archs build built sdk commands kind dep \
        id rpaths weak dylibs closure_names system pattern prefix found prefixes
    shift
    prefixes=("$@")
    need_macos check
    clean_environment
    fail() {
        echo "::error::$*" >&2
        failed=1
    }
    [ -f "$out/libmpv.2.dylib" ] || error "$out has no libmpv.2.dylib"
    dylibs=$(cd "$out" && printf '%s\n' *.dylib | LC_ALL=C sort)
    system=$(mktemp "${TMPDIR:-/tmp}/libmpv-system.XXXXXX")

    for entry in $(cd "$out" && ls -A); do
        case $entry in
            *.dylib) ;;
            licenses) [ -d "$out/licenses" ] && [ -n "$(ls -A "$out/licenses")" ] || fail "$out/licenses is empty" ;;
            SHA256SUMS) ;;
            *) fail "$out holds $entry, which is neither a library nor licenses/" ;;
        esac
    done

    for name in $dylibs; do
        file="$out/$name"
        if [ -L "$file" ] || [ ! -f "$file" ]; then
            fail "$name is not a plain file"
            continue
        fi
        archs=$(lipo -archs "$file" | tr ' ' '\n' | grep . | LC_ALL=C sort | tr '\n' ' ') || archs="none "
        if [ "$archs" != "arm64 x86_64 " ]; then
            fail "$name holds $archs, not arm64 and x86_64"
            continue
        fi
        for arch in arm64 x86_64; do
            build=$(vtool -arch "$arch" -show-build "$file") || build=
            built=$(awk '$1 == "minos" { print $2; exit }' <<< "$build")
            sdk=$(awk '$1 == "sdk" { print $2; exit }' <<< "$build")
            echo "$name $arch: minos $built, sdk $sdk"
            [ "$built" = "$minos" ] || fail "$name's $arch half is built for macOS ${built:-unknown}, not $minos"
            commands=$(load_commands "$file" "$arch") || commands=
            id=$(awk '$1 == "LC_ID_DYLIB" { print $2 }' <<< "$commands")
            [ "$id" = "@rpath/$name" ] || fail "$name's $arch half is named $id, not @rpath/$name"
            rpaths=$(awk '$1 == "LC_RPATH" { print $2 }' <<< "$commands" | tr '\n' ' ')
            [ "$rpaths" = "@loader_path " ] || fail "$name's $arch half has rpaths ${rpaths:-none}, not @loader_path alone"
            while read -r kind dep; do
                case $kind in LC_ID_DYLIB | LC_RPATH | '') continue ;; esac
                weak=0
                [ "$kind" = LC_LOAD_WEAK_DYLIB ] && weak=1
                case $dep in
                    @rpath/*)
                        [ -f "$out/${dep#@rpath/}" ] || fail "$name's $arch half loads $dep, which $out lacks"
                        ;;
                    *)
                        if system_allowed "$dep" "$weak"; then
                            if [ "$weak" = 1 ]; then echo "$dep (weak)"; else echo "$dep"; fi >> "$system"
                        else
                            fail "$name's $arch half loads $dep$([ "$weak" = 1 ] && echo ' (weak)'), which is not on the list of what macOS 11 has"
                        fi
                        ;;
                esac
            done <<< "$commands"
        done
        codesign --verify --strict "$file" || fail "$name's signature does not verify"
    done

    for arch in arm64 x86_64; do
        closure_names=$(closure "$arch" "$out")
        if [ "$closure_names" != "$dylibs" ]; then
            diff <(echo "$closure_names") <(echo "$dylibs") >&2 || true
            fail "$out holds other libraries (>) than libmpv's $arch closure (<)"
        fi
    done

    # What Patina relies on, each its own file.
    for pattern in libmpv.2 'libavcodec.*' 'libavdevice.*' 'libavfilter.*' 'libavformat.*' \
        'libavutil.*' 'libswresample.*' 'libswscale.*' 'libplacebo.*' 'libass.*' \
        'libfreetype.*' 'libharfbuzz.*' 'libfribidi.*' 'libluajit-5.1.*' 'libuchardet.*' \
        'libdav1d.*' 'libwebp.*'; do
        # shellcheck disable=SC2086
        set -- "$out"/$pattern.dylib
        [ -f "$1" ] || fail "$out has no $pattern.dylib"
    done

    echo "system libraries the set loads:"
    LC_ALL=C sort -u "$system" | sed 's/^/  /'
    rm -f "$system"

    for prefix in ${prefixes[@]+"${prefixes[@]}"}; do
        [ -d "$prefix/logs" ] || {
            fail "$prefix has no logs/"
            continue
        }
        found=$(newer_macos "$prefix/logs")
        if [ -n "$found" ]; then
            printf '%s\n' "$found" >&2
            fail "the build logs in $prefix/logs say something was built for, or uses, a newer macOS than $minos"
        fi
    done

    [ "$failed" = 0 ] || error "$out failed the checks above"
    echo "$(echo "$dylibs" | wc -l | tr -d ' ') libraries in $out, all as they should be"
}

main() {
    case "${1:-}" in
        fetch)
            [ $# -eq 2 ] || usage
            cmd_fetch "$2"
            ;;
        tools)
            [ $# -eq 3 ] || usage
            cmd_tools "$2" "$3"
            ;;
        build)
            [ $# -eq 5 ] || usage
            cmd_build "$2" "$3" "$4" "$5"
            ;;
        universal)
            [ $# -eq 4 ] || usage
            cmd_universal "$2" "$3" "$4"
            ;;
        check)
            [ $# -ge 2 ] || usage
            shift
            cmd_check "$@"
            ;;
        flags)
            [ $# -le 2 ] || usage
            cmd_flags "${2:-}"
            ;;
        *) usage ;;
    esac
}

# Run unless sourced, so the functions above can be tried out on their own.
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    main "$@"
fi
