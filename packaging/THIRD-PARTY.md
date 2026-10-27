# Third-party software in Patina's packages

Patina itself is MIT licensed (LICENSE). Its packages carry these as well,
each under its own license.

## Everywhere

- **Lucide icons**, ISC license (a few derived from Feather, MIT); see
  LICENSE-lucide. <https://lucide.dev>

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
