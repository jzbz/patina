# IINA parity

Patina's goal is feature parity with [IINA](https://github.com/iina/iina) on
Linux, macOS and Windows. This file tracks it area by area. Item IDs refer to
the [IINA inventory](iina-inventory.md), which lists all 682 behaviours found
in IINA 1.5.1's source.

Status: **done** (implemented and exercised), **partial** (works, with the
gaps noted), **planned**, or **adapted** (macOS-only in IINA; Patina provides
the closest cross-platform equivalent). "Done" means done in the source; it
does not mean every platform has been through release testing. So far most
testing has been on Linux (X11 and Wayland); the Windows and macOS bundles
have been run for playback (see M5).

## Milestones

| Milestone | Scope | State |
| --- | --- | --- |
| M1 Foundation | libmpv core, video output, IINA window, OSC, OSD, sidebars, menus, key bindings, welcome view, history and resume | done (the macOS menu bar and title bar tried on an Intel macOS VM) |
| M2 Everyday parity | media keys (MPRIS, SMTC, Now Playing), preferences window, key binding editor, seek thumbnails, playlist durations and drag reordering, subtitle auto-matching, history window, inspector | done; tried on Linux and on a Windows 11 VM (media keys and the media panel, sleep prevention, preferences, thumbnails, history, inspector); on macOS only sleep prevention so far |
| M3 Advanced parity | yt-dlp quality switching and downloads (as IINA's Online Media plugin), music mode, filter windows and saved filters, interactive crop, online subtitles, HDR, URL scheme, file associations, single-instance CLI, log viewer | in progress: yt-dlp quality switching and downloads, online subtitles, the single instance, the URL scheme and file associations done (tried on Linux and a Windows 11 VM, not yet on macOS) |
| M4 Plugins | JavaScript plugin runtime with IINA's `iina.*` API, web-view overlays, sidebars and windows, plugin manager | not planned (see section 18) |
| M5 Distribution | self-contained packages for all three platforms with libmpv, CI builds, updates | in progress: `bundle.yml` builds a universal Patina.app and a Windows folder, each carrying libmpv. Both play video and subtitles on machines that never had mpv or a build tool: a Windows 11 VM and an Intel macOS 26 VM, neither with a GPU, so both draw in software. Releases, signing, Flatpak, Homebrew and winget planned |

## 1. Window and chrome

| Item | IINA | Patina |
| --- | --- | --- |
| WIN-01 | Full-size-content window, transparent title bar | done. Linux/Windows: frameless window with Patina's own overlay title bar and window buttons. macOS: a transparent title bar with the video under it, Patina's title over the video and macOS's own window buttons fading with the controls (tried on an Intel macOS VM) |
| WIN-02 | Multiple player windows, idle cores reused | done: each window has its own mpv core; idle windows are reused; "open in new window" preference |
| WIN-03 | One window per file or grouped | partial: multi-file opens go into one playlist, and so do opens that arrive within 0.2 s of each other (a file manager that starts Patina once per file); `-w` opens a window for each. IINA's preference for grouping planned |
| WIN-04 | Welcome window with resume, open, open URL, recents | partial: shown inside the idle player window; no keyboard navigation of the list yet |
| WIN-05 | Action after launch | done: welcome screen or an Open dialog |
| WIN-06 | Fit window to video on open | done: fit the screen, 0.5×, 1×, 1.5× or 2×; always, on open or never (physical-resolution option planned) |
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
| WIN-17 | Full-screen auto play/pause | partial: full screen when media opens; play/pause on entering or leaving planned |
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
| OSC-04 | Configurable toolbar buttons | done: chosen in Preferences > Interface (PiP, playlist, settings, subtitles, screenshot, full screen); drag-to-reorder planned |
| OSC-05 | Subtitle-track button menu | planned |
| OSC-06 | Arrow buttons: speed ladder, playlist or seek | done (press-and-hold acceleration planned) |
| OSC-07 | Auto-hide with cursor | done |
| OSC-08 | Seek bar: cache, chapters, A-B loop, hover preview | partial: cache, chapter ticks, A-B range, and a preview with thumbnail, time and chapter; draggable loop knobs planned |
| OSC-09 | Elapsed / remaining toggle | done (precision menu planned) |
| OSC-10 | Volume slider up to the maximum volume | done |
| OSC-11 | Thumbnail preview | done |
| OSC-12 | Clock and battery in full screen | planned |
| OSC-13 | Buffering indicator | partial: spinner with percentage |

## 3. On-screen display

| Item | IINA | Patina |
| --- | --- | --- |
| OSD-01 | Enable, timeout, size | done (in `settings.json`) |
| OSD-02 | Messages with progress bars | done: volume, seek, colour adjustments |
| OSD-03 | Text messages | done: pause, speed, delays, tracks, subtitles, loops, A-B loop, chapters, aspect, crop, rotation, deinterlace, hardware decoding, screenshots |
| OSD-04 | Online subtitle messages | done: what was downloaded and how many downloads are left today; failures in the dialog |
| OSD-06 | Templates | planned |
| OSD-07 | Screenshot preview card | planned |
| OSD-08 | `show-progress` | done |
| OSD-09 | Quiet while loading | done: messages only follow user actions |

## 4. Playlist and chapters

| Item | IINA | Patina |
| --- | --- | --- |
| PLC-01 | Playlist rows | done: name, folder, current item, multi-select (Ctrl/Command and Shift), duration, watched progress, the series prefix a name shares with others folded into a "…" that unfolds, and title and artist for tagged audio files (preference). Adapted: long names are cut at the end, not in the middle (Slint elides only there); the music-mode variant of the metadata setting waits for music mode |
| PLC-02 | Durations prefetched in the background | done: a headless mpv reads each local file's length and tags; the footer shows the total, or "X of Y selected" |
| PLC-03 | Loop, shuffle, sort, add, remove, clear | partial: sort planned |
| PLC-04 | Double-click to play | done |
| PLC-05 | Drag to reorder, drop to insert | done: dragging rows (the whole selection with them) to an insertion line; cut, copy and paste of rows as paths and URLs, from the menu or Ctrl/Command+X/C/V. Adapted: files dropped while the playlist is open go after the selected rows rather than under the pointer, because winit 0.30, which Slint uses, reports no position for a drop (and no file drops at all on Wayland) |
| PLC-06 | Row context menu | done: Play Next, Play in New Window, Remove, Cut/Copy/Paste, Add Subtitle… and Wrong Subtitle, Open in Browser for streams, Show in File Manager and Move to Trash for files, Add Files/URL, Clear Playlist; Return plays and Delete removes the selection. IINA's plugin items are not planned |
| PLC-07 | Add files from the same folder | done (mpv `autocreate-playlist=same`; subfolders are left out, while an opened folder is read recursively) |
| PLC-08 | Auto-play next, repeat on open | partial: auto-play next (preference); repeat on open planned |
| PLC-10 | Save playlist | partial: writes `.m3u8`; not yet tested |
| PLC-11 | Delete current file | done: File > Move to Trash (`delete-current-file`) and the row menu move files to the trash and drop them from the playlist; IINA's binding-only hard delete is not offered |
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
| Menu tree | Global menu bar | adapted: the same File / Playback / Video / Audio / Subtitles / Window / Help tree as a context menu, opened by right click or the title-bar menu button; on macOS also the native menu bar, after Slint's own app menu (tried on an Intel macOS VM) |
| MENU-02 | Shortcuts follow the key bindings | planned |
| MENU-08 | Saved filter menus | planned |

## 7. Preferences

The preferences window has IINA's pages: General, Interface, Video, Audio,
Subtitles, Network, Control, Key Bindings, Advanced and Utilities, with a
search field that finds settings on any page (OTH-01). Changes are saved at
once and applied to running players, as IINA's option observers do
(CORE-02). Settings are tracked by their inventory IDs:

| Area | Patina |
| --- | --- |
| PREF-GEN | done: launch action, multiple windows, start paused, full screen on open, auto-play next, keep open, folder auto-add, resume, pause when minimized, keep the screen awake, history, recent files, screenshot folder, format and subtitles. Planned: pause when inactive or asleep, grouping simultaneous opens, clipboard and screenshot preview |
| PREF-UI | done: title bar, window fitting and size, float on top, controller timeout, arrow buttons, toolbar buttons, chapter marks, remaining time, OSD switch, timeout and size, thumbnails. Planned: theme, compact UI, controller position, initial geometry, PiP behaviour, animations |
| PREF-VID | done: hardware decoding (off / automatic / copy-back), deinterlacing, tone mapping, target peak. Planned: ICC profiles, HDR mode |
| PREF-AUD | done: start volume, maximum volume, device, gapless, preferred languages, ReplayGain. Planned: S/PDIF passthrough |
| PREF-SUB | done: loading mode (mpv's matching), languages, encoding, font, size, style, colors, position, letterbox, ASS override. Planned: IINA's intelligent matching, online subtitles |
| PREF-NET | done: cache, buffer size, buffering, user agent, proxy, RTSP transport, yt-dlp and its options |
| PREF-CTL | done: click and scroll actions, seek mode, scroll steps, media keys. Planned: pinch and Force Touch where available |
| PREF-KEY | done: see INP-06..08 |
| PREF-ADV | done: load mpv.conf and scripts from the mpv folder, extra mpv options. Planned: log level and log files |
| PREF-UTL | done: clear history, saved positions and thumbnails; open the settings folder; restore defaults; default-app registration, as Linux, macOS and Windows each allow (see CLI-12..15) |

## 8. Input

| Item | IINA | Patina |
| --- | --- | --- |
| INP-01..03 | input.conf format, IINA commands, key names | done: `#@iina` and `#@patina` lines, normalized keys, IINA's defaults with Ctrl in place of Command off macOS |
| INP-04 | Dispatch through menus first | adapted: bindings are dispatched directly |
| INP-05 | Intercepted commands | done: `quit`, `screenshot`, `show-progress`, `show-text`, `set window-scale` |
| INP-06..08 | Readable binding list, editor, preset sets | done: Preferences > Key Bindings lists bindings with readable names, edits them with a key recorder and common actions, and keeps user sets; the built-in set is copied on first edit. Bundled mpv/VLC/Movist presets planned |
| INP-11 | Click actions | done: single, double (with single-click delay), right, middle; all of IINA's actions plus the menu |
| INP-12 | Scroll actions | done: volume, seek and speed |
| INP-13..14, 16 | Pinch, Force Touch, haptics | planned where the platform offers them |
| INP-17 | Media keys | done: MPRIS on Linux (tested); SMTC on Windows (tested on a Windows 11 VM: the media keys, and the media panel's title and state); MediaPlayer on macOS (written, not yet tried). Media keys go through `PLAYPAUSE`/`NEXT`/... bindings when present, as in IINA |
| KB-* | Default bindings | done: all 91 IINA defaults, adapted per platform |

## 9–10. Subtitles and filters

| Item | IINA | Patina |
| --- | --- | --- |
| SUB-01 | Primary and secondary subtitles | done |
| SUB-02 | Track naming | done: title, language name, codec, format and flags |
| SUB-03 | External subtitles | done: dialog, drop |
| SUB-04 | Intelligent auto-matching | done: names, episode numbers (S01E02, 1x02, "- 02") with series-name similarity, the folder's immediate subfolders, a lone video takes the folder's subtitles, preferred languages first; matches reach mpv through its `on_load` hook so its own track selection applies. The extra search paths and priority strings are planned |
| SUB-05 | Per-file "Wrong Subtitle" overrides | done: from the row menu, Add Subtitle… gives a file subtitles of the user's choosing and Wrong Subtitle turns its matches down (Patina's and mpv's own), now if it is playing and whenever it plays later in the session |
| SUB-06..09 | Online subtitles | done for OpenSubtitles.com, with each user's own API key (Preferences › Subtitles) and optionally their account, whose password is kept in the system's password store. Subtitles › Find Online Subtitles (Ctrl/Command+Shift+D) searches by the file's OpenSubtitles hash and its name in the preferred languages and lists what it finds, the ones made for the very file first; the search text can be changed. Downloads load into the video, and Save Downloaded Subtitle copies one elsewhere. The automatic search loads a subtitle made for the very file when a local video of 20 minutes or more opens without any. Tested against a stand-in API and keyring. Not planned: assrt.net and shooter.cn |
| SUB-10 | Preferred languages | done (plain text field; ISO 639 autocompletion planned) |
| SUB-11 | Encodings | partial: default encoding preference; the per-file Encoding menu is planned |
| SUB-12..14 | Styling and font picker | partial: font, size, bold, italic, colors, outline, shadow, position, ASS override; font picker and color wells planned |
| FLT-01..06 | Filter windows, presets, saved filters | planned |
| FLT-03 | Labelled internal filters | done: `@patina-flip`, `@patina-mirror`, `@patina-eq` |

## 11–15. Music mode, PiP, history, inspector, screenshots

| Item | IINA | Patina |
| --- | --- | --- |
| MUS-01..10 | Music mode | planned |
| PIP-01..04 | Picture in picture | adapted, partial: a small always-on-top window without the title bar |
| HIS-01 | Resume through watch-later | done |
| HIS-02 | History window | done: grouped by date or folder, search, watched progress, missing files dimmed; Play, Play in New Window, Show in File Manager, Remove; Return, Ctrl+Return, Delete, Ctrl+L, Ctrl+F |
| HIS-03 | History store | adapted: JSON in the data folder, one entry per file with its position and length |
| HIS-04..05 | Recent files and resume button | done |
| HIS-06 | Clearing data | done: Preferences > Utilities clears history, saved positions and thumbnails |
| INS-01..03 | Inspector: General, Tracks, Status | done: follows the active window, refreshed every second with asynchronous property reads |
| INS-04 | Watch tab | done: any mpv property, kept in the preferences |
| INS-05 | No polling while closed | done |
| SHOT-01..02 | Screenshots | done: with or without subtitles, folder, format |
| SHOT-03 | Preview card | planned |
| SHOT-05 | Seek-bar thumbnails | done: 100 per local video from a background mpv, cached as JPEG with a size limit |

## 16–17. Network, command line, integration

| Item | IINA | Patina |
| --- | --- | --- |
| NET-01 | Open URL | done: tested with an HTTP stream (HTTP authentication planned) |
| NET-03 | Buffering display | partial |
| NET-04 | Cache and network preferences | done |
| NET-05 | yt-dlp | done: mpv's `ytdl_hook`, with what IINA's Online Media plugin adds. A quality preference (the best, or up to a height, or a yt-dlp format); Video Quality and Audio Quality menus for each video, offering its resolutions and frame rates rather than yt-dlp's raw list, which load it again where it was; Download Video (Ctrl/Command+D) in the quality playing, with a Downloads window that shows progress, speed and time left and can cancel, retry and show the file. Tested with an HLS stream with separate audio. Adapted: Patina uses the yt-dlp that is installed, where the plugin downloads and updates its own copy and Deno |
| NET-07 | Browser extensions | partial: patina:// links work (CLI-10); there is no Patina extension yet, and IINA's would need its scheme changed |
| CLI-01..02 | Command line with `--mpv-*` options | done. Beyond IINA, whose `iina-cli` starts another IINA each time: while Patina runs, a launch hands it what to open and exits (in 0.02 s), the window comes to the front (on Wayland with the launcher's activation token), and the first launch to start is the one that serves. `--mpv-*` options from such a launch apply to a new window of their own; `--new-instance` starts a separate Patina. Tested on Linux (X11, and Wayland in a virtual KWin), including two launches at once, quitting while one launches, and a crash, and on a Windows 11 VM: a launch hands over in about a quarter of a second, three at once share one window, and the window comes to the front |
| CLI-03..06 | Separate windows, stdin, keep running, music mode / PiP flags | partial: `-w` and `--pip` done; `-` is passed to mpv for standard input, in a Patina of its own (not yet tested); keep running and music mode planned |
| CLI-10 | URL scheme | done: `patina://open` (or `weblink`) with `url`, `new_window`, `enqueue`, `full_screen`, `pip` and `mpv_` options, decoded as IINA does. Stricter than IINA: `url` must be a network stream (no local files, mpv protocols, or srt:// and rtp://, which can listen), IINA's safe mpv options lose those that turn off certificate checks, raise the volume limit or use up memory and threads, a playlist a link opens may only list such streams, and the files a playlist from the network names are not read. Tested on Linux through the desktop entry and on a Windows 11 VM through the shell, where a link with a stray quote stays one link |
| CLI-12..15 | File associations | done: one table of 63 extensions for every platform. Linux: a desktop entry (`packaging/linux`), and Preferences › Utilities makes Patina the default for videos, music or playlists with `xdg-mime`, writing an entry for the running executable when none is installed. macOS: the bundle declares the types, folders and the scheme, and Apple event handlers take Finder's opens and links (not yet tried on a Mac); the same preferences make Patina the default through LaunchServices. Windows: the preferences add Patina to Open With and Default apps under the user's own registry classes, open Settings at Patina's Default apps page, and remove it all again, keys made only for Patina included (tried on a Windows 11 VM, running the registered commands as Explorer does). Opens 0.2 s apart are batched (CLI-14); IINA's "nothing to open" alert is not shown |

## 18. Plugins

PLG-* and PLG-API-* are not planned. mpv's own scripts, in Lua or
JavaScript, load from the `scripts` folder of Patina's `mpv` configuration
and cover much of what IINA's plugins are used for; the rest of IINA's
plugin system (a JavaScript runtime with the `iina.*` API, web views for
overlays, sidebar tabs and windows, and a plugin manager) would cost more
than it brings.

## 19–20. Platform, accessibility, localization

| Item | IINA | Patina |
| --- | --- | --- |
| PLAT-01..02 | Now Playing and remote commands | partial: title, artist, album, duration, position, rate, loop and shuffle are published; artwork planned |
| PLAT-03 | Touch Bar | not planned (discontinued hardware) |
| PLAT-04 | Updates | planned with packaging |
| PLAT-05..06 | Logging and log viewer | partial: `--verbose` logs Patina and mpv to stderr |
| PLAT-07 | Sleep prevention | done while anything plays: D-Bus `org.freedesktop.ScreenSaver` on Linux (tested), `SetThreadExecutionState` on Windows and IOKit assertions on macOS (tested on a Windows 11 VM and an Intel macOS VM) |
| PLAT-09 | Rendering | done: libmpv OpenGL render API into a texture composited by Slint. Where OpenGL fails (no GPU driver, a virtual machine, a remote desktop), Patina draws with Slint's software renderer and mpv's software renderer on a thread of its own. Tested on a Windows 11 VM with only the basic display adapter, an Intel macOS VM whose only OpenGL is Apple's software renderer, and Linux without GLX or EGL |
| PLAT-10 | HDR | planned |
| PLAT-11 | Live Text | not planned (no equivalent) |
| PLAT-12 | Credential storage | done: the Secret Service on Linux (GNOME Keyring, KWallet), the Keychain on macOS and Credential Manager on Windows, through the keyring crates; used for the OpenSubtitles password. HTTP authentication planned |
| A11Y-* | Accessibility and localization | partial: accessible roles and labels on controls; localization planned |

## Known issues

- With a video filter active (flip or mirror) and the video rotated, adding
  a crop can blank the picture; mpv drops the `video-crop` rectangle when
  the filter chain reinitializes. Cropping through a labelled `crop` filter,
  as IINA does, would avoid it.
- On X11, a launch that hands its files to the running Patina doesn't end
  the desktop's startup notice, so a busy cursor can linger until the
  desktop gives up on it (Wayland passes the activation token on instead).
- On the X virtual framebuffer used for automated tests (software OpenGL),
  hardware decoding shows black frames; real GPUs are unaffected.
