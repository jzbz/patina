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

## macOS (Patina.app/Contents/Frameworks)

- **libmpv.2.dylib and the libraries beside it**: mpv 0.41.0 with FFmpeg,
  libass, libplacebo, LuaJIT and the rest, as IINA builds them for its own
  app (<https://iina.io>, <https://github.com/iina/iina>). They include GPL
  parts, so Patina.app as a whole is under the GPL. Each library's name says
  which project it comes from.
