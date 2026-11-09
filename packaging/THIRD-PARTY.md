# Third-party software in Patina's packages

Patina itself is MIT licensed (LICENSE). Its packages carry these as well,
each under its own license.

## Everywhere

- **Lucide icons**, ISC license (a few derived from Feather, MIT); see
  LICENSE-lucide. <https://lucide.dev>
- **IBM Plex Sans** 3.005, the interface's typeface, compiled into Patina:
  Regular, Italic, SemiBold and Bold, unmodified, from IBM's
  `@ibm/plex-sans` 1.1.0 release. Copyright © 2017 IBM Corp. with Reserved
  Font Name "Plex" (the font files' own notice reads "Copyright 2018 IBM
  Corp. All rights reserved."); SIL Open Font License 1.1; see
  LICENSE-ibm-plex. <https://github.com/IBM/plex>

## Flatpak (under /app)

- **libmpv.so.2**: mpv 0.42.0, built in the manifest with its default
  options, which make it GPL; so the Flatpak as a whole is under the GPL.
  With it come libass, libplacebo, LuaJIT, uchardet, libXpresent and
  libdisplay-info, each built from its source. FFmpeg is the runtime's own
  (org.freedesktop.Platform), not part of the bundle.
- **yt-dlp**, Unlicense. <https://github.com/yt-dlp/yt-dlp>
- **yt-dlp-ejs**, Unlicense, with meriyah (ISC) and astring (MIT) built into
  it; their notices are kept in `yt_dlp_ejs/yt/solver/lib.min.js`.
- **Deno**, MIT, its own release build, which carries V8 (BSD-3-Clause) and
  other third-party code inside. <https://deno.com>

Their license texts are installed under
`/app/share/licenses/video.patina.Patina`, yt-dlp-ejs's with its Python
package.

## Windows (the Patina folder)

- **libmpv-2.dll**: mpv 0.42.0 and the FFmpeg and other libraries it links,
  mpv's own build for its release (`libmpv-v0.42.0-x86_64-w64-mingw32-lgpl.zip`,
  made by mpv's CI with `ci/build-mingw64-full.sh`), named in
  `packaging/windows-deps.env`. mpv is built as LGPL (`-Dgpl=false`), under
  LGPL 2.1 or later, whose text is beside it as LICENSE-libmpv.txt; its
  FFmpeg is configured as LGPL version 3 or later, whose text is beside it
  as LICENSE-LGPLv3.txt, with the GPLv3's as LICENSE-GPLv3.txt. mpv's source:
  <https://github.com/mpv-player/mpv>; FFmpeg's: <https://ffmpeg.org>.
- **yt-dlp\\** (yt-dlp.exe and its `_internal` folder): yt-dlp's own folder
  build, `yt-dlp_win.zip` of the version named in
  `packaging/bundled-tools.env`, unpacked unchanged. yt-dlp's own code is
  Unlicense, but the build carries Python and libraries under other
  licenses, mutagen (GPL-2.0-or-later) and certifi (MPL-2.0) among them, so
  the folder as a whole is under the GPL 3.0 or later, as yt-dlp's README
  says of its PyInstaller builds; its text is LICENSE-GPLv3.txt. It also
  carries Microsoft's Visual C++ and Universal C runtime DLLs, which
  Microsoft lets programs built with its tools redistribute. Each library's
  license and text is in THIRD-PARTY-yt-dlp.txt, yt-dlp's
  THIRD_PARTY_LICENSES.txt, which the folder also holds as
  `_internal\THIRD_PARTY_LICENSES.txt`. Its source: yt-dlp's tag of that
  version in <https://github.com/yt-dlp/yt-dlp>, whose `bundle/requirements/`
  names the exact version of each library the build carries and whose
  `.github/workflows/build.yml` and `bundle/pyinstaller.py` built it;
  mutagen's source is at <https://github.com/quodlibet/mutagen>, certifi's
  at <https://github.com/certifi/python-certifi>.
- **ffmpeg.exe**: FFmpeg 9.0.2, built for Patina as described under
  "ffmpeg" below; cross-compiled for x86_64 Windows with mingw-w64 and
  linked statically with MinGW's runtime. Under the LGPL 2.1 or later,
  whose text is LICENSE-ffmpeg.txt.

## macOS (Patina.app/Contents/Frameworks)

libmpv.2.dylib and the 18 libraries it loads, built for Patina from each
project's source release by `packaging/macos-libmpv-build.sh` in Patina's
repository: for arm64 and for x86_64, each for macOS 11, joined into one
universal file of each. None of them is used under the GPL: the one part
of Patina.app that is, yt-dlp.app below, is a program of its own that
Patina runs. Each project's licence texts are in
Contents/Resources/licenses, named `<project>-<file>`. Their
corresponding source is the tarballs named below, each pinned by its
SHA-256 in `packaging/macos-libmpv.env` (FFmpeg's in
`packaging/bundled-tools.env`), unmodified but for the one patch named
under mpv, together with `packaging/macos-libmpv-build.sh`, those two
files, `packaging/macos-build-tools.txt` and
`packaging/macos-libmpv-view-without-vulkan.patch` at the release's tag in
<https://github.com/jzbz/patina>, which fetch, check, configure and build
them; `packaging/macos-libmpv-build.sh flags` prints the options FFmpeg and
mpv are configured with.

- **mpv** 0.42.0, libmpv.2.dylib: built with `-Dgpl=false`, which leaves
  out every part of mpv that is GPL only, so libmpv is under the LGPL 2.1 or
  later (mpv-LICENSE.LGPL; mpv-Copyright says which files are under other
  licences). Patched with `packaging/macos-libmpv-view-without-vulkan.patch`,
  without which 0.42.0 does not compile without Vulkan.
  <https://github.com/mpv-player/mpv/archive/refs/tags/v0.42.0.tar.gz>
- **FFmpeg** 9.0.2, libavcodec.63, libavdevice.63, libavfilter.12,
  libavformat.63, libavutil.61, libswresample.7 and libswscale.10: from
  the same tarball as the ffmpeg described under "ffmpeg" below, and like
  it configured without `--enable-gpl`, `--enable-version3` or
  `--enable-nonfree`, which FFmpeg's configure makes LGPL 2.1 or later.
  libavformat has OpenSSL linked into it, for https (next item). OpenSSL 3
  is under the Apache License 2.0, which the FSF counts compatible with
  version 3 of the LGPL but not with version 2.1; FFmpeg's LICENSE.md calls
  OpenSSL compatible with the LGPL without naming a version. So these
  libraries are passed on under the LGPL version 3 or later, which the "or
  later" of FFmpeg's licence allows. Both LGPL texts are included
  (ffmpeg-COPYING.LGPLv3 and ffmpeg-COPYING.LGPLv2.1), with the GPLv3's
  that the LGPLv3 builds on (ffmpeg-COPYING.GPLv3) and FFmpeg's
  ffmpeg-LICENSE.md. <https://ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz>
- **OpenSSL** 3.5.9, its libssl and libcrypto linked statically into
  libavformat.63.dylib rather than shipped as files of their own: Apache
  License 2.0 (openssl-LICENSE.txt).
  <https://github.com/openssl/openssl/releases/download/openssl-3.5.9/openssl-3.5.9.tar.gz>
- **libplacebo** 7.387.0, libplacebo.387.dylib, built for OpenGL alone:
  LGPL 2.1 or later (libplacebo-LICENSE).
  <https://github.com/haasn/libplacebo/archive/refs/tags/v7.387.0.tar.gz>.
  Compiled into it are the OpenGL loader that glad generates, under glad's
  licence (libplacebo-glad-LICENSE: MIT for glad, and the Khronos licences
  of the specifications the loader is generated from), and fast_float,
  under the MIT licence (libplacebo-fast_float-LICENSE-MIT), each from the
  commit libplacebo's tag names as its submodule (macos-libmpv.env lists
  them).
- **libass** 0.17.5, libass.9.dylib: ISC (libass-COPYING).
  <https://github.com/libass/libass/releases/download/0.17.5/libass-0.17.5.tar.xz>
- **FreeType** 2.15.0, libfreetype.6.dylib: under the FreeType License
  (freetype-FTL.TXT), one of the two licences FreeType offers
  (freetype-LICENSE.TXT); as it asks, portions of this software are
  copyright © 2026 The FreeType Project (<https://freetype.org>). All rights
  reserved. Its BDF and PCF drivers and fthash, which are compiled in, are
  under an X11-style licence (freetype-bdf-README, freetype-pcf-README).
  <https://download.savannah.gnu.org/releases/freetype/freetype-2.15.0.tar.xz>
- **HarfBuzz** 15.1.0, libharfbuzz.0.dylib: the "Old MIT" licence
  (harfbuzz-COPYING); its Universal Shaping Engine table is generated from
  Microsoft's data files, under the MIT licence (harfbuzz-ms-use-COPYING).
  <https://github.com/harfbuzz/harfbuzz/releases/download/15.1.0/harfbuzz-15.1.0.tar.xz>
- **GNU FriBidi** 1.0.18, libfribidi.0.dylib: LGPL 2.1 or later
  (fribidi-COPYING).
  <https://github.com/fribidi/fribidi/releases/download/v1.0.18/fribidi-1.0.18.tar.xz>
- **LuaJIT** 2.1, at commit 8e6520a7aecd0517e792b359afbbfd7274791f5f,
  libluajit-5.1.2.dylib: MIT (luajit-COPYRIGHT).
  <https://github.com/LuaJIT/LuaJIT/archive/8e6520a7aecd0517e792b359afbbfd7274791f5f.tar.gz>
- **uchardet** 0.0.8, libuchardet.0.dylib: the MPL 1.1, the GPL 2.0 or
  later or the LGPL 2.1 or later, at the recipient's choice, as its source
  files say; uchardet-COPYING is the MPL's text.
  <https://www.freedesktop.org/software/uchardet/releases/uchardet-0.0.8.tar.xz>
- **dav1d** 1.5.4, libdav1d.7.dylib: BSD 2-Clause (dav1d-COPYING).
  <https://downloads.videolan.org/pub/videolan/dav1d/1.5.4/dav1d-1.5.4.tar.xz>
- **libwebp** 1.6.0, libwebp.7.dylib, libwebpmux.3.dylib and
  libsharpyuv.0.dylib: BSD 3-Clause (libwebp-COPYING), with Google's patent
  grant (libwebp-PATENTS).
  <https://storage.googleapis.com/downloads.webmproject.org/releases/webp/libwebp-1.6.0.tar.gz>

## macOS (Patina.app/Contents/Helpers)

- **yt-dlp.app**: yt-dlp's own folder build for macOS, `yt-dlp_macos.zip`
  of the version named in `packaging/bundled-tools.env`, its files
  unchanged but moved into a helper app's places, as
  `packaging/macos-tools.sh` says: the launcher in Contents/MacOS, Python,
  its framework and every other library and extension module in
  Contents/Frameworks, the rest in Contents/Resources, and links to put each
  where Python looks for it; signed with Patina's Developer ID. yt-dlp's own
  code is Unlicense, but the build carries Python and libraries under other
  licenses, mutagen (GPL-2.0-or-later) and certifi (MPL-2.0) among them, so
  it is as a whole under the GPL 3.0 or later, as yt-dlp's README says of
  its PyInstaller builds; its text is Contents/Resources/LICENSE-GPLv3.txt.
  Each library's license and text is in
  Contents/Resources/THIRD-PARTY-yt-dlp.txt, yt-dlp's
  THIRD_PARTY_LICENSES.txt, which yt-dlp.app also holds in its own
  Contents/Resources. Its source: yt-dlp's tag of that version in
  <https://github.com/yt-dlp/yt-dlp>, whose `bundle/requirements/` names the
  exact version of each library the build carries and whose
  `.github/workflows/build.yml` and `bundle/pyinstaller.py` built it;
  mutagen's source is at <https://github.com/quodlibet/mutagen>, certifi's
  at <https://github.com/certifi/python-certifi>.
- **ffmpeg**: FFmpeg 9.0.2, built for Patina as described under "ffmpeg"
  below, once for arm64 and once for x86_64, each for macOS 11, joined into
  one file. Under the LGPL 2.1 or later, whose text is
  Contents/Resources/LICENSE-ffmpeg.txt.

## ffmpeg (macOS and Windows)

The ffmpeg in Patina.app and in the Windows folder is built from FFmpeg's
own source release, with no other library, by `packaging/ffmpeg.sh` in
Patina's repository, which also says why each part is enabled. Its
corresponding source is these two together:

- FFmpeg's source tarball `ffmpeg-9.0.2.tar.xz`,
  <https://ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz>, SHA-256
  `8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e`,
  signed by FFmpeg's release key
  (`FCF986EA15E6E293A5644F10B4322F04D67658D8`), unmodified;
- `packaging/ffmpeg.sh` and `packaging/bundled-tools.env` at the release's
  tag in <https://github.com/jzbz/patina>, which fetch that tarball and
  configure and build it.

It is configured with

    --disable-everything --disable-autodetect --disable-iconv
    --disable-network --disable-programs --enable-ffmpeg --disable-doc
    --disable-avdevice --disable-swscale --disable-swresample --disable-asm
    --disable-debug --enable-small --enable-protocol=file
    --enable-demuxer=mov,matroska,mpegts,ffmetadata
    --enable-muxer=mp4,ipod,webm,matroska,mp3
    --enable-parser=h264,hevc,av1,vp9,aac,opus,ac3
    --enable-bsf=aac_adtstoasc,setts,extract_extradata
    --enable-decoder=aac,vp9

and, for Windows, `--enable-cross-compile --target-os=mingw32 --arch=x86_64
--cross-prefix=x86_64-w64-mingw32- --extra-ldflags=-static`; for macOS,
each architecture in turn, `--enable-cross-compile --target-os=darwin
--arch=<arm64 or x86_64> --cc="clang -arch <arm64 or x86_64>"
--extra-cflags=-mmacosx-version-min=11.0
--extra-ldflags=-mmacosx-version-min=11.0`. `ffmpeg -version` prints the
line a given binary was configured with, and `ffmpeg -L` its license. With
no `--enable-gpl`, `--enable-version3` or `--enable-nonfree`, FFmpeg's
configure makes the build LGPL 2.1 or later; LICENSE-ffmpeg.txt is the
tarball's `COPYING.LGPLv2.1`.
