# IINA parity

Patina's goal is feature parity with [IINA](https://github.com/iina/iina) on
Linux, macOS and Windows. This file tracks it area by area. Item IDs refer to
the [IINA inventory](iina-inventory.md), which lists all 682 behaviours found
in IINA 1.5.1's source.

Status: **done** (implemented and exercised), **partial** (works, with the
gaps noted), **planned**, or **adapted** (macOS-only in IINA; Patina provides
the closest cross-platform equivalent). "Done" means done in the source; it
does not mean every platform has been through release testing. So far all
testing has been on Linux (X11 and Wayland).

## Milestones

| Milestone | Scope | State |
| --- | --- | --- |
| M1 Foundation | libmpv core, video output, IINA window, OSC, OSD, sidebars, menus, key bindings, welcome view, history and resume | in progress: most of it is in place |
| M2 Everyday parity | media keys (MPRIS, SMTC, Now Playing), preferences window, key binding editor, seek thumbnails, playlist durations and drag reordering, subtitle auto-matching, encodings and styling, history window, inspector | planned |
| M3 Advanced parity | music mode, filter windows and saved filters, interactive crop, online subtitles, HDR, URL scheme, file associations, single-instance CLI, log viewer | planned |
| M4 Plugins | JavaScript plugin runtime with IINA's `iina.*` API, web-view overlays, sidebars and windows, plugin manager | planned |
| M5 Distribution | self-contained packages for all three platforms with libmpv, CI builds, updates | planned |

## 1. Window and chrome

| Item | IINA | Patina |
| --- | --- | --- |
| WIN-01 | Full-size-content window, transparent title bar | done on Linux/Windows: frameless window with Patina's own overlay title bar and window buttons. macOS: native title bar for now; transparent full-size content view planned |
| WIN-02 | Multiple player windows, idle cores reused | done: each window has its own mpv core; idle windows are reused; "open in new window" preference |
| WIN-03 | One window per file or grouped | partial: multi-file opens go into one playlist |
| WIN-04 | Welcome window with resume, open, open URL, recents | partial: shown inside the idle player window; no keyboard navigation of the list yet |
| WIN-05 | Action after launch | planned |
| WIN-06 | Fit window to video on open | partial: 1× fit limited to 85% of the screen, timing always / on open / never; scale option and physical-resolution option planned |
| WIN-07 | Initial geometry | planned |
| WIN-08 | Lock window aspect ratio | planned |
| WIN-09 | Edge-to-edge or docked controls | partial: edge-to-edge only |
| WIN-10 | Compact UI | planned |
| WIN-11 | Dark / light / system theme | partial: dark only |
| WIN-12 | Translucent materials | adapted: translucent panels without blur |
| WIN-13 | Title shows file name, fades with controls | done (path menu planned) |
| WIN-14 | Float on top | done: menu, title-bar pin, `ontop` sync, preference |
| WIN-15 | Full screen | done: borderless full screen through the window system |
| WIN-16 | Black out other monitors | planned |
| WIN-17 | Full-screen auto play/pause | planned |
| WIN-18 | Pause when minimized / inactive / asleep | partial: pause when minimized (preference; not yet tested) |
| WIN-19 | Keep open at end of file | done: `keep-open=yes` |
| WIN-20 | Remember window position | planned |
| WIN-21 | Cursor hiding, drag video to move window | done |
| WIN-22 | Idle rendering | done: mpv renders only when it has a new frame or the size changes |
| WIN-23 | Sidebars | partial: one right-hand sidebar; side and width preferences planned |
| WIN-24 | Interactive crop / delogo | planned |
| WIN-26 | Drag and drop | partial: media files and folders open, subtitle files load into the current video (not yet tested); LUT drops planned |
| WIN-29 | Subtitles in the letterbox | planned |

## 2. On-screen controller

| Item | IINA | Patina |
| --- | --- | --- |
| OSC-01 | Floating, top or bottom layout | partial: floating |
| OSC-02 | Draggable floating panel | partial: draggable; position memory and snap-to-centre planned |
| OSC-03 | Volume, transport, toolbar, seek row | done |
| OSC-04 | Configurable toolbar buttons | partial: set in `settings.json` (PiP, playlist, settings, fullscreen, screenshot, subtitles); customization sheet planned |
| OSC-05 | Subtitle-track button menu | planned |
| OSC-06 | Arrow buttons: speed ladder, playlist or seek | done (press-and-hold acceleration planned) |
| OSC-07 | Auto-hide with cursor | done |
| OSC-08 | Seek bar: cache, chapters, A-B loop, hover preview | partial: cache, chapter ticks, A-B range and hover time; thumbnails and draggable loop knobs planned |
| OSC-09 | Elapsed / remaining toggle | done (precision menu planned) |
| OSC-10 | Volume slider up to the maximum volume | done |
| OSC-11 | Thumbnail preview | planned |
| OSC-12 | Clock and battery in full screen | planned |
| OSC-13 | Buffering indicator | partial: spinner with percentage |

## 3. On-screen display

| Item | IINA | Patina |
| --- | --- | --- |
| OSD-01 | Enable, timeout, size | done (in `settings.json`) |
| OSD-02 | Messages with progress bars | done: volume, seek, colour adjustments |
| OSD-03 | Text messages | done: pause, speed, delays, tracks, subtitles, loops, A-B loop, chapters, aspect, crop, rotation, deinterlace, hardware decoding, screenshots |
| OSD-04 | Online subtitle messages | planned with online subtitles |
| OSD-06 | Templates | planned |
| OSD-07 | Screenshot preview card | planned |
| OSD-08 | `show-progress` | done |
| OSD-09 | Quiet while loading | done: messages only follow user actions |

## 4. Playlist and chapters

| Item | IINA | Patina |
| --- | --- | --- |
| PLC-01 | Playlist rows | partial: name, folder, current item; durations and progress planned |
| PLC-03 | Loop, shuffle, sort, add, remove, clear | partial: sort planned |
| PLC-04 | Double-click to play | done |
| PLC-05 | Drag to reorder, drop to insert | partial: move up/down buttons; drag reordering planned |
| PLC-06 | Row context menu | planned |
| PLC-07 | Add files from the same folder | done (mpv `autocreate-playlist=same`) |
| PLC-08 | Auto-play next, repeat on open | partial: auto-play next |
| PLC-10 | Save playlist | partial: writes `.m3u8`; not yet tested |
| PLC-11 | Delete current file | planned |
| PLC-12 | Chapters tab | done |
| PLC-13 | Panel toggles and bindings | done |

## 5. Quick settings

| Item | IINA | Patina |
| --- | --- | --- |
| QS-L-* | Layout tab | planned |
| QS-V-01 | Video tracks | done |
| QS-V-02 | Aspect ratio | done (custom value planned) |
| QS-V-03 | Crop | done for presets; custom crop planned |
| QS-V-04 | Rotation | done, plus flip and mirror |
| QS-V-05 | Speed, log scale 0.25×–16× | done |
| QS-V-06 | Hardware decoding | done |
| QS-V-07 | HDR | planned |
| QS-V-08 | Colour equalizer | done |
| QS-A-01..03 | Tracks, external audio, delay | done |
| QS-A-04 | 10-band equalizer | done |
| QS-A-05 | Equalizer presets and profiles | planned |
| QS-S-01..03 | Primary and secondary tracks, delay, position, scale | done |
| QS-S-04 | Text style | planned |

## 6. Menus

| Item | IINA | Patina |
| --- | --- | --- |
| Menu tree | Global menu bar | adapted: the same File / Playback / Video / Audio / Subtitles / Window / Help tree as a context menu, opened by right click or the title-bar menu button. Native macOS menu bar planned |
| MENU-02 | Shortcuts follow the key bindings | planned |
| MENU-08 | Saved filter menus | planned |

## 7. Preferences

All IINA preferences are tracked by their inventory IDs (PREF-GEN, UI, VID,
AUD, SUB, NET, CTL, KEY, ADV, UTL). Patina currently reads a subset from
`settings.json`: multiple windows, folder auto-add, resume, history, recents
size, pause when minimized, controller timeout, toolbar buttons, arrow
action, chapter ticks, remaining time, OSD switch/timeout/size, window
fitting, float on top, title bar style, hardware decoding, volume and
maximum volume, seek and volume steps, mouse click and scroll actions,
screenshot folder and format, yt-dlp path, and raw mpv options. A
`mpv.conf`, `scripts/` and `fonts/` in Patina's `mpv` config folder are
loaded (PREF-ADV-06). The preferences window itself is planned for M2.

## 8. Input

| Item | IINA | Patina |
| --- | --- | --- |
| INP-01..03 | input.conf format, IINA commands, key names | done: `#@iina` and `#@patina` lines, normalized keys, IINA's defaults with Ctrl in place of Command off macOS |
| INP-04 | Dispatch through menus first | adapted: bindings are dispatched directly |
| INP-05 | Intercepted commands | done: `quit`, `screenshot`, `show-progress`, `show-text`, `set window-scale` |
| INP-06..08 | Readable binding list, editor, preset sets | partial: Help > Keyboard Shortcuts lists the bindings; editor and preset sets planned |
| INP-11 | Click actions | done: single, double (with single-click delay), right, middle |
| INP-12 | Scroll actions | done: volume and seek; speed planned |
| INP-13..14, 16 | Pinch, Force Touch, haptics | planned where the platform offers them |
| INP-17 | Media keys | planned: MPRIS (Linux), SMTC (Windows), MediaPlayer (macOS) |
| KB-* | Default bindings | done: all 91 IINA defaults, adapted per platform |

## 9–10. Subtitles and filters

| Item | IINA | Patina |
| --- | --- | --- |
| SUB-01 | Primary and secondary subtitles | done |
| SUB-02 | Track naming | done: title, language name, codec, format and flags |
| SUB-03 | External subtitles | done: dialog, drop |
| SUB-04..05 | Intelligent auto-matching | planned (mpv's own exact-name matching applies today) |
| SUB-06..09 | Online subtitles | planned |
| SUB-10 | Preferred languages | planned (mpv `slang`/`alang` work through raw mpv options) |
| SUB-11 | Encodings | planned |
| SUB-12..14 | Styling and font picker | planned |
| FLT-01..06 | Filter windows, presets, saved filters | planned |
| FLT-03 | Labelled internal filters | done: `@patina-flip`, `@patina-mirror`, `@patina-eq` |

## 11–15. Music mode, PiP, history, inspector, screenshots

| Item | IINA | Patina |
| --- | --- | --- |
| MUS-01..10 | Music mode | planned |
| PIP-01..04 | Picture in picture | adapted, partial: a small always-on-top window without the title bar |
| HIS-01 | Resume through watch-later | done |
| HIS-02..03 | History window | partial: history is recorded with position and duration; the window is planned |
| HIS-04..05 | Recent files and resume button | done |
| HIS-06 | Clearing data | partial: Clear Menu clears history |
| INS-01..05 | Inspector | planned (video and audio details show in Quick Settings) |
| SHOT-01..02 | Screenshots | done: with or without subtitles, folder, format |
| SHOT-03 | Preview card | planned |
| SHOT-05 | Seek-bar thumbnails | planned |

## 16–17. Network, command line, integration

| Item | IINA | Patina |
| --- | --- | --- |
| NET-01 | Open URL | done: tested with an HTTP stream (HTTP authentication planned) |
| NET-03 | Buffering display | partial |
| NET-04 | Cache and network preferences | planned (available through raw mpv options) |
| NET-05 | yt-dlp | partial: mpv's `ytdl_hook` with a path preference; not yet tested |
| NET-07 | Browser extensions | planned with the URL scheme |
| CLI-01..02 | Command line with `--mpv-*` options | done |
| CLI-03..06 | Separate windows, stdin, keep running, music mode / PiP flags | partial: `-` is passed to mpv for standard input (not yet tested) |
| CLI-10 | URL scheme | planned |
| CLI-12..15 | File associations | planned with packaging |

## 18. Plugins

PLG-* and PLG-API-* are planned for M4: an embedded JavaScript engine that
implements IINA's `iina.*` modules, so existing IINA plugins can run with
few or no changes, and web views for overlays, sidebar tabs and windows.

## 19–20. Platform, accessibility, localization

| Item | IINA | Patina |
| --- | --- | --- |
| PLAT-01..02 | Now Playing and remote commands | planned |
| PLAT-03 | Touch Bar | not planned (discontinued hardware) |
| PLAT-04 | Updates | planned with packaging |
| PLAT-05..06 | Logging and log viewer | partial: `--verbose` logs Patina and mpv to stderr |
| PLAT-07 | Sleep prevention | done while anything plays: D-Bus `org.freedesktop.ScreenSaver` on Linux (tested), `SetThreadExecutionState` on Windows and IOKit assertions on macOS (not yet tested) |
| PLAT-09 | Rendering | done: libmpv OpenGL render API into a texture composited by Slint |
| PLAT-10 | HDR | planned |
| PLAT-11 | Live Text | not planned (no equivalent) |
| PLAT-12 | Credential storage | planned with online subtitles |
| A11Y-* | Accessibility and localization | partial: accessible roles and labels on controls; localization planned |

## Known issues

- With a video filter active (flip or mirror) and the video rotated, adding
  a crop can blank the picture; mpv drops the `video-crop` rectangle when
  the filter chain reinitializes. Cropping through a labelled `crop` filter,
  as IINA does, would avoid it.
- On the X virtual framebuffer used for automated tests (software OpenGL),
  hardware decoding shows black frames; real GPUs are unaffected.
