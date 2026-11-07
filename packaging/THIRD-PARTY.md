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

- **libmpv.so.2**: mpv 0.41.0, built in the manifest with its default
  options, which make it GPL; so the Flatpak as a whole is under the GPL.
  With it come libass, libplacebo, LuaJIT, uchardet and libXpresent, each
  built from its source. FFmpeg is the runtime's own
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

- **libmpv-2.dll**: mpv and the FFmpeg it links, built as LGPL (mpv's
  `-Dgpl=false`) by <https://github.com/zhongfly/mpv-winbuild>; the build it
  came from is named in `packaging/windows-deps.env`. mpv's source:
  <https://github.com/mpv-player/mpv>; FFmpeg's: <https://ffmpeg.org>.
- **vulkan-1.dll**: the Khronos Vulkan loader, Apache-2.0, as shipped with
  mpv 0.41.0's own Windows release. <https://github.com/KhronosGroup/Vulkan-Loader>
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

- **libmpv.2.dylib and the libraries beside it**: mpv 0.41.0 with FFmpeg,
  libass, libplacebo, LuaJIT and the rest, as IINA builds them for its own
  app (<https://iina.io>, <https://github.com/iina/iina>). They include GPL
  parts, so Patina.app as a whole is under the GPL. Each library's name says
  which project it comes from.

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
