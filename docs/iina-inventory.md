# IINA feature inventory

The reference Patina's [parity matrix](parity.md) tracks against. It
describes IINA's behaviour in our own words, with item IDs (`WIN-01`,
`PREF-VID-03`, `KB-12`, ...) the matrix refers to; no IINA source is copied
here.

Source studied: shallow clone of `https://github.com/iina/iina`, default branch, commit `96f605f6d8c4a3ed7d5d0afac644d8681f766757` (2026-10-26, "Update Anime4K plugin listing to AnimeEnhancer"). Marketing version in `Configs/Deployment.xcconfig` is **1.5.1** (build 180), deployment target **macOS 12**. The checkout was only read; nothing was built or run.

Conventions used in this file:

- Source paths are relative to the repo root (almost everything is under `iina/`).
- `[mac-only]` = depends on a macOS-only technology (AppKit idiom, Touch Bar, Force Touch, Keychain, Sparkle, CoreAudio, Vision, ...). Patina must pick a cross-platform equivalent or drop it. `[mac-ish]` = the idea is portable, but the implementation leans hard on a macOS facility.
- `[hidden]` = no UI in the current build (defaults-only key, hidden menu item, or removable workaround).
- Item IDs (`WIN-01`, `PREF-VID-03`, `KB-12`, ...) exist so Patina's parity tracker can reference them. Counts per area are in the summary table at the end.
- Preference keys are quoted by their literal `UserDefaults` name (the Swift identifier is sometimes slightly different, e.g. `screenshotFolder` is stored as `screenShotFolder`). Defaults come from `Preference.defaultPreference` in `iina/Preference.swift`.
- IINA is a thin shell around **libmpv** (render API on OpenGL, `vo=libmpv`). Almost every playback feature is "set an mpv option / property / run an mpv command", which is why section "CORE" lists the mpv wiring. Anything labelled "mpv" in a row is something Patina gets for free from libmpv.
- Behaviour is described in our own words; short identifiers (keys, command names, enum cases) are quoted from source.
- Heads-up on architecture: the *current* tree has two settings UIs. The new programmatic `SettingsWindow` + `SettingsPage*` (default on, `enableNewSettings = true`) and the legacy XIB-based `Pref*ViewController` panes (still shipped; toggled via Help > "Use New Settings Window"). The new panes' labels live in `iina/en.lproj/Localizable.strings` under keys `settings.<prefKey>.label` / `.desc` / `.items.N` and `settings.$Section` (the per-page `Settings*Localizable` tables named in the Swift files are not in this checkout); the *structure* below is taken from the new `SettingsPage*.swift`, the label text from those keys.
- IINA also has an `iina-cli` shim (a separate tiny executable), `OpenInIINA` Safari extension, Chrome/Firefox WebExtensions in `browser/`, and a community `plugins.json` catalog.

---

## 1. Window & UI chrome

One player = one `PlayerCore` (mpv instance) owning three window controllers: welcome window, main window, mini player (music mode). Multiple players can exist at once (multi-window).

- **WIN-01** Main player window: full-size-content video window with transparent title bar, standard traffic-light buttons, minimum size 285x120, "no video" size 640x360 (`AppData.mainWindowMinSize`, `sizeWhenNoVideo`). Source: `iina/MainWindowController.swift`, `iina/AppData.swift`. [mac-ish]
- **WIN-02** Multiple independent player windows (each has its own mpv core, playlist, sidebar, OSC); idle cores are recycled for the next open. Pref `alwaysOpenInNewWindow` (default true) flips between "open in new window" and "reuse active window"; File > Open / Open in New Window are an inverted pair. Source: `iina/PlayerCore.swift` (`activeOrNew`, `findIdlePlayerCore`), `iina/AppDelegate.swift`.
- **WIN-03** Opening several files at once: either one window per file (skipping files already open unless `allowDuplicatePlayers`), or `groupSimultaneousOpensInPlaylist` to put them all in one new window's playlist. Source: `iina/PlayerCore.swift` `openURLs`.
- **WIN-04** Welcome (initial) window: gradient backdrop, app icon, "Resume" button for the last played file with its saved time, "Open..." and "Open URL..." buttons, recent-files table (keyboard Up/Down/Enter navigable), drag & drop target, beta-build info popover. Source: `iina/InitialWindowController.swift`.
- **WIN-05** Action after launch (`actionAfterLaunch`: welcome window / open panel / nothing); reopen-from-Dock shows the welcome window again. Source: `iina/AppDelegate.swift`. [mac-ish]
- **WIN-06** Auto-size window to the video when a file starts: sizes = fit screen / 0.5x / 1x / 1.5x / 2x (`resizeWindowOption`, default 1x); timing = always / only when opened manually (default) / never (`resizeWindowTiming`); honours physical-pixel (Retina) scaling (`usePhysicalResolution`, default true); clamps to screen visible frame and to min size; keeps centre when resizing on track change; preserves area when navigating the playlist. Source: `iina/MainWindowController.swift` `handleVideoSizeChange`.
- **WIN-07** Initial window size/position taken from mpv `geometry` syntax (`initialWindowSizePosition`, e.g. `50%+0+0`, width or height in points or % of screen, +/- offset anchors). UI is a size row + X/Y offset rows with unit (point / % of screen) and side popups. Source: `iina/SettingsPageUI.swift` (`GeometryBindings`), `iina/GeometryDef.swift`.
- **WIN-08** Window aspect ratio locked to the video by default; "Lock window aspect ratio" menu/sidebar toggle (`unlockWindowAspectRatio`, default false) lets the user freely resize with black bars; when unlocked, a title-bar button / sidebar button "fit window to video" trims the bars. Source: `iina/MainWindowController.swift` (`removeVideoViewBlackBars`), `iina/Titlebar.swift`, `iina/SidebarLayoutPane.swift`.
- **WIN-09** Edge-to-edge video (`edgeToEdgeVideo`, default true): OSC/title/sidebars float over video. When off: "docked" mode (`dockedControlBarAndTitlebar`, default false) puts title bar and bottom OSC outside the video rectangle and enlarges the window to compensate. Source: `iina/MainWindowController.swift` `setupVideoContainerConstraints`.
- **WIN-10** Compact UI (`compactUI`, default false): smaller paddings/row heights (via `LayoutValue(regular, compact)` constants) and a different toolbar style on the window. Source: `iina/LayoutValue.swift`, `iina/MainWindowController.swift` `setWindowToolbar`.
- **WIN-11** Theme: dark (default) / light / follow system (`themeMaterial`, enum values 0/2/4). Applies to window, OSC, sidebars. Source: `iina/Preference.swift`, `iina/PlayerWindowController.swift` `setMaterial`.
- **WIN-12** Liquid Glass styling on macOS 26+: separate toggles for OSD, OSC and sidebars (`useLiquidGlassOSD/OSC/Sidebar`, default true); older macOS uses NSVisualEffectView blur. Source: `iina/TranslucentView.swift`, `iina/SettingsPageUI.swift`. [mac-only]
- **WIN-13** Window title bar shows file display name + document icon (proxy icon) for local files, media title for streams; title bar fades with the OSC; right-click on the title shows the file's path hierarchy menu (reveal in Finder); double-click honours the system "minimize/zoom" preference. Source: `iina/Titlebar.swift`, `iina/MainWindowController.swift`. [mac-only]
- **WIN-14** "Float on top" (always on top): menu toggle, `ontop` mpv property sync, pref `alwaysFloatOnTop` (only while playing), optional pin icon in the title bar (`alwaysShowOnTopIcon`, default false; icon always visible when on). Source: `iina/PlayerWindowController.swift`, `iina/Titlebar.swift`.
- **WIN-15** Full screen: native (Spaces) full screen with custom enter/exit animation hooks, or "legacy full screen" (`useLegacyFullScreen`, default false) that fakes full screen on the current Space and handles Dock/menu-bar hiding and the camera-housing (notch) area. State machine `windowed / animating / fullscreen`, prior windowed frame restored on exit. Source: `iina/MainWindowController.swift` (`FullScreenState`, `legacyAnimateToFullscreen`). [mac-only]
- **WIN-16** Black out other monitors while in full screen (`blackOutMonitor`, default false): covers non-active screens with black windows and re-applies on display hot-plug. Source: `iina/MainWindowController.swift` `blackOutOtherMonitors`. [mac-ish]
- **WIN-17** Full-screen auto-play/pause behaviours: `fullScreenWhenOpen` (default false), `playWhenEnteringFullScreen` (false), `pauseWhenLeavingFullScreen` (false). Source: `iina/PlayerCore.swift`, `iina/MainWindowController.swift`.
- **WIN-18** Auto pause/resume rules: when minimized (`pauseWhenMinimized`), when app inactive (`pauseWhenInactive`), when machine sleeps (`pauseWhenGoesToSleep`, default true). Source: `iina/PlayerWindowController.swift`, `iina/MainWindowController.swift`.
- **WIN-19** Quit-when-last-window-closes (`quitWhenNoOpenedWindow`, default false) and keep-window-open-at-end-of-file/playlist (`keepOpenOnFileEnd`, default true; maps to mpv `keep-open`; becomes `always` when auto-play-next is off). Source: `iina/AppDelegate.swift`, `iina/MPVController.swift`.
- **WIN-20** Window position memory: last frame saved (`MainWindowLastPosition`) and the previous screen is re-selected on next open. Source: `iina/MainWindowController.swift`.
- **WIN-21** Mouse/cursor handling: UI and cursor hide after inactivity (see OSC), cursor re-hidden after clicks while UI hidden (macOS workaround), drag anywhere on video to move the window, drag-distance threshold distinguishes click from drag, accepts-first-mouse option (`videoViewAcceptsFirstMouse`, default false). Source: `iina/MainWindowController.swift`.
- **WIN-22** Video view lifecycle & power: render/display link is stopped when paused and re-enabled for seek, frame-step, full-screen transitions and resize (`enableDisplayIdle` internal flag); window forced redraw when occluded->visible. Source: `iina/VideoView.swift`, `iina/ViewLayer.swift`. [mac-only]
- **WIN-23** Sidebars: two slide-in panels (leading/trailing) hosting Quick Settings (tabs: Layout, Video, Audio, Subtitles), Playlist (tabs: Playlist, Chapters) and Plugins; each has its own side preference (`sidebarSettingsDisplayAtLeading` false, `sidebarPlaylistDisplayAtLeading` true, `sidebarPluginsDisplayAtLeading` true), width saved per side (`leadingSidebarWidth`/`trailingSidebarWidth`, default 360, clamped to 300-500 and to at most 80% of the window), drag-resize handle, click outside (edge-to-edge) dismisses, fade instead of slide when Reduce Motion is on. Source: `iina/SidebarController.swift`, `iina/SideBar.swift`.
- **WIN-24** Interactive crop/delogo mode: hides sidebars, pauses, shows a draggable box over the video with live pixel readout, aspect presets (4:3, 16:9, 16:10, 21:9, 3:2, 5:4) and Done/Cancel. Source: `iina/InteractiveMode.swift`.
- **WIN-25** Live Text overlay on paused frames (optional) - see PLAT section. [mac-only]
- **WIN-26** Drag & drop onto a player window / welcome window / playlist: files, folders (recursive playable scan), BD folders, m3u/pls/cue playlists, URLs, plain-text URLs/paths, subtitle files (loaded as external subs if no media in drop), single `.cube/.3dl/.dat/.m3d` file (applied as 3D LUT filter). Accepted by extension lists in `Utility.supportedFileExt`. Finder aliases are resolved to their targets before opening; the File > Open panel allows multi-select of files *and* folders. Source: `iina/DraggingDetect.swift`, `iina/Utility.swift`, `iina/AppDelegate.swift` `openFile`. Alias resolution [mac-only].
- **WIN-27** macOS Services entry "IINA: Open URL" (selected text -> open). Source: `iina/Info.plist`, `iina/AppDelegate.swift` `droppedText`. [mac-only]
- **WIN-28** Dock menu: "Open..." and "Open URL...". Source: `iina/Base.lproj/MainMenu.xib`. [mac-only]
- **WIN-29** Black-bars/letterbox area can host subtitles (`displayInLetterBox`, default true -> mpv `sub-use-margins` + `sub-ass-force-margins`). Source: `iina/MPVController.swift`.
- **WIN-30** Debug aids: Help > "Dump Debug Info" (Option-alternate) writes all player states to a text file in Downloads. Source: `iina/AppDelegate.swift` `dumpDebugInfo`. [hidden]

## 2. On-screen controller (OSC)

Three layouts, switchable live: floating (default), top (in the title bar), bottom (bar). Source: `iina/MainWindowController.swift` `setupOnScreenController`, `iina/OSCFloatingView.swift`, `iina/OSCBottomView.swift`, `iina/Titlebar.swift`, `iina/OSCToolbarButton.swift`.

- **OSC-01** Layout preference `oscPosition`: floating=0 (default), top=1, bottom=2, chosen via a 3-card visual picker in Settings > UI and in the sidebar Layout tab.
- **OSC-02** Floating OSC: fixed ~460pt wide rounded translucent panel, draggable anywhere over the video, position stored as fractions of the video area (`controlBarPositionHorizontal` 0.5, `controlBarPositionVertical` 0.1), clamped to edges, optional snap to horizontal centre within 5pt with haptic tick (`controlBarStickToCenter`, default true), re-laid-out when sidebars open/close.
- **OSC-03** Control rows: volume (mute button + slider), centre play cluster (left arrow, play/pause, right arrow, with speed labels beside the arrows), configurable toolbar buttons, and the seek row (elapsed label, seek slider, duration/remaining label). Top/bottom layouts put everything in one line and collapse volume, then toolbar, as the window narrows.
- **OSC-04** Toolbar buttons (`controlBarToolbarButtons`, ordered ints; default = plugins, pip, playlist, settings): settings(0), playlist(1), pip(2), fullScreen(3), musicMode(4), subTrack(5), screenshot(6), plugins(7), liveText(8, macOS 13+). Customised by drag-and-drop in a sheet with a "restore default" button; buttons highlight when their state is active (PiP/fullscreen/sidebar open). Floating OSC uses narrower buttons when it has 5+. Source: `iina/PrefOSCToolbarSettingsSheetController.swift`, `iina/Preference.swift` `ToolBarButton`.
- **OSC-05** Sub-track toolbar button opens a popup menu: loaded subtitle tracks (incl. none) + external subtitle files found next to the video, files "related" to the current video listed first. Source: `iina/MainWindowController.swift` `showSubChooseMenu`.
- **OSC-06** Left/right arrow button function (`arrowBtnAction`): speed (0, default), previous/next playlist item (1), or seek +-10 s (2). In speed mode a click steps through speed ladder 1/32 ... 32x (`AppData.availableSpeedValues`: 0.03125, 0.0625, 0.125, 0.25, 0.5, 1, 2, 4, 8, 16, 32), long-press/Force-Touch pressure picks faster rates, release returns to 1x, pausing resets. Speed text appears beside the arrow when speed != 1. Force-Touch accelerator part is [mac-only].
- **OSC-07** Auto-hide: OSC, title bar and window buttons fade after inactivity (`enableControlBarAutoHide` true, `controlBarAutoHideTimeout` 2.5 s), timer reset on mouse move, never hides while mouse is over OSC/title bar, while dragging the OSC, in PiP or in interactive crop mode; the cursor hides with it. Source: `iina/MainWindowController.swift` `hideUI/showUI/updateTimer`, `iina/FadeableViewController.swift`.
- **OSC-08** Seek slider details: thin knob, buffered/cache position drawn for streams, optional chapter separators (`showChapterPos`, default false), draggable A and B loop knobs that edit the mpv A-B loop live, hover tooltip with time + chapter title and a thumbnail preview, scroll-to-seek on the slider (can be disabled with `disablePlaySliderScrolling`). Source: `iina/PlaySlider.swift`, `iina/PlaySliderCell.swift`, `iina/PlaySliderLoopKnob.swift`.
- **OSC-09** Time labels: left = elapsed, right = total duration or remaining (click to toggle, persisted in `showRemainingTime`; `scaleRemainingTime` bases remaining time on playback speed); right-click offers precision 1 s / 100 ms / 10 ms / 1 ms (`timeDisplayPrecision` 0-3); UI timer runs faster at >=10 ms precision. Source: `iina/DurationDisplayTextField.swift`, `iina/PlayerCore.swift`.
- **OSC-10** Volume slider with up-to-`maxVolume` range (100..1000), haptic click at the 100% line when boost enabled, scroll-to-change (can be disabled with `disableVolumeSliderScrolling`), mute button icon reflects level; the whole volume group is hidden when the file has no audio track. Source: `iina/VolumeSlider.swift`, `iina/VolumeButton.swift`, `iina/PlayerCore.swift`. Haptics [mac-only].
- **OSC-11** Seek-time preview: thumbnail (see SHOT/Thumbnails) above (or below, near top edge) the slider with the time and chapter name; thumbnail keeps true display aspect ratio (handles anamorphic and rotation). Source: `iina/MainWindowController.swift` `updateTimePreviewAndThumbnail`, `iina/ThumbnailPeekView.swift`, `iina/TimePreviewView.swift`.
- **OSC-12** Full-screen extras: top-right panel with clock and battery percentage (`displayTimeAndBatteryInFullScreen`, default false) showing file name too. Source: `iina/AdditionalInfoView.swift`, `iina/PowerSource.swift`. Battery query [mac-only].
- **OSC-13** Buffering indicator: spinner + "Opening stream..." while connecting, then "Buffering N%" / "Seeking..." with cached bytes and download speed; two switches `showBufferingThrobber` and `showSeekingThrobber` (both default true). Source: `iina/BufferIndicatorView.swift`.
- **OSC-14** Showing the OSC/title bar is refused while Live Text selection or interactive crop/delogo mode is active (UI is force-hidden for Live Text); a persistent OSD (screenshot card) swallows Esc first. Source: `iina/MainWindowController.swift` `showUI`, `iina/LiveTextSupport.swift`.

## 3. On-screen display (OSD)

Text overlay in the top-left of the video (top-right panel is the separate clock/battery card). Implemented in IINA itself (mpv's own OSD is disabled via `osd-level=0` unless the user enables "Use mpv's OSD" under advanced settings). Source: `iina/OSDMessage.swift`, `iina/OSDView.swift`, `iina/PlayerCore.swift` `sendOSD`.

- **OSD-01** Master switch `enableOSD` (default true); auto-hide timeout `osdAutoHideTimeout` (default 1 s); text size `osdTextSize` (default 20 pt, 1-500); four suppress switches: file start, pause/resume, seek, speed (`disableOSD*Msg`).
- **OSD-02** Message with a progress bar: volume (0..maxVolume), audio delay, subtitle delay, secondary subtitle delay (+-10 s centred bar), subtitle position, secondary sub position, contrast/brightness/gamma/saturation/hue (+-100), seek bar with "current / total".
- **OSD-03** Plain text messages: file start (name or URL), pause / resume (with position/duration), speed `Nx`, aspect ratio, crop, rotate, deinterlace on/off, hardware decoding on/off, subtitle scale, subtitles shown/hidden (primary and secondary), mute / unmute, screenshot captured, stop, chapter title with `(i/n) position / duration`, track change (video/audio/sub: "#id description"), A-B loop state (cleared / A at t / B at t), file loop / playlist loop / loop off, added N files to playlist, playlist cleared, filter added/removed.
- **OSD-04** Online-subtitle workflow messages (always shown even if OSD disabled): finding subtitles from provider, N subtitles found / none found, downloading N from provider, downloaded <file>, saved, cannot log in, network error, file error, canceled, cannot connect, timed out, quota exceeded (with reset time).
- **OSD-05** Custom messages: `custom(text)` and `customWithDetail(text, detail)`, used by plugins (`core.osd`, prefixed with the plugin name as detail) and external callers.
- **OSD-06** Mustache-style templates inside messages: `{{duration}}`, `{{position}}`, `{{precisePosition}}`, `{{preciseDuration}}`, `{{percentPos}}`, `{{currChapter}}`, `{{chapterCount}}`. Time/position text is always laid out left-to-right even in RTL locales.
- **OSD-07** OSD with embedded accessory view (persistent until dismissed): screenshot preview card with REVEAL / EDIT (open in default image app) / DELETE buttons, shown 5 s, window enlarged to at least 500x300 if smaller; Esc dismisses. Source: `iina/ScreenshootOSDView.swift`.
- **OSD-08** Show-progress binding (`o` / `P`) pops the seek OSD with precise time. Source: `iina/MainWindowController.swift` `handleKeyBinding`.
- **OSD-09** OSD is suppressed while a file is loading (except the file-start message) and for 0.2 s after playback restart, to avoid noise. Source: `iina/PlayerCore.swift`.
- **OSD-10** OSD is not used in the mini player (music mode); there the volume popover and labels convey state. Source: `iina/PlayerCore.swift` `sendOSD`, `iina/MiniPlayerWindowController.swift`.

## 4. Playlist & chapters

Sidebar `PlaylistViewController` with two tabs (Playlist, Chapters); also embedded (as a slide-down panel) in the mini player. Source: `iina/PlaylistViewController.swift`, `iina/SidebarPlaylistPane.swift`, `iina/SidebarChaptersPane.swift`. The playlist itself is mpv's playlist (IINA reads `playlist/N/*` properties).

- **PLC-01** Playlist table: current item marked with a triangle (RTL-aware), multi-select, row height follows compact-UI, middle-truncated file names, per-row duration label, thin progress bar under rows with saved watch-later position, "series prefix" fold button (a common filename prefix of >= 7 chars shown as a clickable "..." that expands), optional metadata display (artist label + title) controlled by `playlistShowMetadata` (default true) and `playlistShowMetadataInMusicMode` (default true, i.e. metadata only shown in music mode if true).
- **PLC-02** Durations/progress prefetched in the background by probing files with FFmpeg when `prefetchPlaylistVideoDuration` (default true, hidden); footer shows total length, or "X of Y selected" when rows are selected.
- **PLC-03** Footer buttons: loop (cycles off -> file -> playlist; tinted when active; icon changes to repeat.1 for file), shuffle (mpv `playlist-shuffle` / `playlist-unshuffle`, tinted when on), sort (menu: filename asc/desc, path asc/desc; rewrites the mpv playlist keeping the current item), add (menu: Add File..., Add URL...), remove selected, clear whole playlist. Source: `iina/SidebarPlaylistPane.swift`.
- **PLC-04** Double-click plays that entry (re-loads it even if it is the current one, via `playlist-play-index`); navigation pauses first on manual change to avoid audio-before-video, then resumes when size is known (unless `pauseWhenOpen`). Source: `iina/PlayerCore.swift`.
- **PLC-05** Drag & drop: reorder rows (move), drop files/folders/URLs/text URLs from Finder or browsers to insert at a position (uses playable-file scan), cut/copy/paste of rows via pasteboard (paths). Source: `iina/SidebarPlaylistPane.swift`.
- **PLC-06** Row context menu: Play Next, Play in New Window, Remove; Subtitles section (shows N matched subs, "Add Subtitle...", "Wrong Subtitle" to drop auto-matched subs); Network section (Open in Browser, Copy URL(s)); File section (Delete -> moves to Trash and removes from list, Show in Finder; "delete after playback" action exists but is an empty stub); plugin-contributed items via `playlist.registerMenuItemBuilder`; Playlist section (Add File, Add URL, Clear Playlist). Source: `iina/SidebarPlaylistPane.swift` `buildMenu`. Finder items [mac-only]; Trash is [mac-ish].
- **PLC-07** Playlist autoload: when a single file is opened, all other media files in the same folder are added to the playlist in natural (localized-standard) order, placed before/after the current file correctly (`playlistAutoAdd`, default true). Opening several files disables this. Source: `iina/AutoFileMatcher.swift`, `iina/PlayerCore.swift`.
- **PLC-08** Auto-play next (`playlistAutoPlayNext`, default true); off => mpv `keep-open=always`. Auto repeat on open (`autoRepeat` false) with default mode playlist|file (`defaultRepeatMode`). Source: `iina/MPVController.swift`, `iina/PlayerCore.swift`.
- **PLC-09** Opening a folder / BD folder (`MovieObject.bdmv` + `index.bdmv`, with or without a `BDMV` parent) / `.m3u .m3u8 .pls .cue` lets mpv build the playlist; autoload is skipped. Source: `iina/DraggingDetect.swift`, `iina/PlayerCore.swift`.
- **PLC-10** File > Save Current Playlist... writes the current playlist as an `.m3u8` (one path/URL per line). Source: `iina/MainMenuActions.swift`.
- **PLC-11** "Delete current file" (move to Trash) and "delete current file directly" (hard delete) IINA commands remove the current item from the playlist and delete the file. Menu item File > "Delete Current File"; hard-delete is binding-only (`delete-current-file-hard`). Source: `iina/MainMenuActions.swift`.
- **PLC-12** Chapters tab: row per chapter with title (or "Chapter N") and a "start -> next start" range, current chapter triangle, double-click seeks and resumes, list refreshed on `chapter-list` changes. Source: `iina/SidebarChaptersPane.swift`.
- **PLC-13** Playlist/Chapters/Settings panels can be toggled individually by tab (menu items and bindings `playlist-panel`, `chapter-panel`, `video-panel`, `audio-panel`, `sub-panel`); menu titles flip between "Show X Panel" and "Hide X Panel". Source: `iina/MenuController.swift`, `iina/SidebarController.swift`.
- **PLC-14** Playlist change sync: dock/menu "Playlist" and "Chapters" submenus are generated on demand with a check mark on the current item; chapter menu items are `hh:mm:ss - title` with monospaced digits and zero-padded to the longest timestamp. Source: `iina/MenuController.swift`.
- **PLC-15** Series/prefix grouping (FileGroup) is used both for the fold button and for matching subtitles to episodes; see SUB section. Source: `iina/FileGroup.swift`.

## 5. Quick settings sidebar (Layout / Video / Audio / Subtitles)

`QuickSettingViewController` with four tabs; opened from the OSC "settings" button or menu items. Source: `iina/QuickSettingViewController.swift`, `iina/Sidebar*Pane.swift`, `iina/SidebarSliderView.swift`, `iina/SidebarTrackSelector.swift`. Default tab: Video.

### 5.1 Layout tab (`iina/SidebarLayoutPane.swift`)

- **QS-L-01** Theme switch (dark/light/system) bound to `themeMaterial`.
- **QS-L-02** Liquid Glass toggles for OSD / OSC / sidebar (macOS 26+ only). [mac-only]
- **QS-L-03** Compact interface toggle (`compactUI`).
- **QS-L-04** Edge-to-edge video toggle (`edgeToEdgeVideo`); when on shows "Lock window aspect" switch (inverse of `unlockWindowAspectRatio`); when off shows "Docked controls and title bar" switch (`dockedControlBarAndTitlebar`).
- **QS-L-05** "Fit window to video" button (removes black bars) - visible only when aspect is unlocked.
- **QS-L-06** On-screen-controller layout picker (floating / top / bottom thumbnails).
- **QS-L-07** Sidebar position: per-sidebar switch to put Settings, Playlist+Chapters, Plugins on the leading or trailing side.

### 5.2 Video tab (`iina/SidebarVideoPane.swift`)

- **QS-V-01** Video track list (id, codec, size, fps, default flag) with "<None>" row; click selects (`vid`).
- **QS-V-02** Aspect-ratio segmented control (Default, 4:3, 16:9, 16:10, 21:9, 5:4) plus free text field (validated by `^\d+(\.\d+)?:\d+(\.\d+)?$`) -> mpv `video-aspect-override` (-1 = default).
- **QS-V-03** Crop segmented control (None, 4:3, 16:9, 16:10, 21:9, 5:4, Custom...). Presets crop centred to the aspect via an IINA-labelled `crop` filter; "Custom..." enters the interactive crop box.
- **QS-V-04** Rotation segmented control 0 / 90 / 180 / 270 -> `video-rotate`.
- **QS-V-05** Playback speed: log-scale slider 0.25x-16x in 24 steps with tick labels 0.25x/1x/4x/16x, numeric input (min 0.01x, up to 6 decimals), reset button when != 1.
- **QS-V-06** Hardware decoding switch (flips mpv `hwdec` between "no" and the preferred setting live).
- **QS-V-07** HDR switch (only shown when the video is HDR and display supports EDR) -> per-file `hdrEnabled`. [mac-only]
- **QS-V-08** Colour equalizer: sliders -100..100 with per-slider reset for brightness, contrast, saturation, gamma, hue (mpv `brightness/contrast/saturation/gamma/hue`).

### 5.3 Audio tab (`iina/SidebarAudioPane.swift`, `iina/Equalizations.swift`)

- **QS-A-01** Audio track list with "<None>" row.
- **QS-A-02** "Load external audio file" button (open panel limited to playable extensions) -> `audio-add`.
- **QS-A-03** Audio delay: slider -5..+5 s (0.05 s snapping while dragging, applied on mouse-up) plus numeric input and reset.
- **QS-A-04** 10-band equalizer (32, 64, 125, 250, 500, 1k, 2k, 4k, 8k, 16k Hz; +-12 dB vertical sliders), implemented as an ffmpeg `lavfi` `equalizer` chain labelled `iina_aeq` in mpv's `af`.
- **QS-A-05** EQ presets (22): flat, acoustic, classical, dance, deep, electronic, hip hop, increase bass, increase treble, increase vocal, jazz, latin, loudness, lounge, piano, pop, R&B, reduce bass, reduce treble, rock, small speaker, spoken word; plus "Manual". User profiles: save as..., rename, delete (stored as JSON in `userEQPresets`).

### 5.4 Subtitles tab (`iina/SidebarSubtitlesPane.swift`)

- **QS-S-01** Primary subtitle track list + visibility switch; secondary subtitle track list + visibility switch (mpv `sid`, `secondary-sid`, `sub-visibility`, `secondary-sub-visibility`).
- **QS-S-02** "Load subtitle..." split button: open panel (sub + video extensions) or a menu of external subtitle files discovered next to the video; "Search online" button (runs the configured provider).
- **QS-S-03** Delay slider (-5..+5 s, applied on mouse-up) and numeric input with reset; position slider 0..100; a Primary|Secondary segmented switch retargets both.
- **QS-S-04** Text style (only shown if a non-bitmap subtitle is active): scale slider (symmetric +-5 where negative means 1/x), reset; font chooser (custom picker), font size popup 25-75 step 5, text colour, border size popup (0, 0.25, 0.5, 1, 1.5, 2, 2.5, 3, 4, 5) + border colour, shadow/background colour.
- **QS-S-05** Messages: "style settings not available for image-based subtitles" and "ASS styles may override these" shown contextually.

### 5.5 Shared sidebar behaviours

- **QS-X-01** Tab bar with icon tab buttons and a close button, per-tab scroll views with shared "container" card styling; a horizontal swipe in a pane switches to the neighbouring tab; the current tab is kept for the lifetime of the player. Source: `iina/SideBar.swift`.
- **QS-X-02** Slider rows share a component with title icon, tick labels, a floating value indicator, numeric text field, and reset button. Source: `iina/SidebarSliderView.swift`.

## 6. Menus (full tree)

Source of structure: `iina/Base.lproj/MainMenu.xib` (static items), `iina/MenuController.swift` (dynamic submenus, titles that flip, and key-equivalent binding), `iina/MainMenuActions.swift` + `iina/MainWindowMenuActions.swift` + `iina/MiniPlayerWindowMenuActions.swift` (actions). Parenthesised keys are the shortcuts the stock "IINA Default" key-binding set produces (Cmd=Meta); see section 8.3 for the rule that menu shortcuts are *derived from the active key-binding set*. `~` marks an Option-alternate (shown when Option is held). All of this is the macOS global menu bar [mac-only]; Patina needs an in-window or per-toolkit menu, and should probably keep the "menu shortcut text is generated from input.conf" idea.

Static key equivalents that are *not* from the key-binding file (hard-coded in the XIB): Cmd+, Cmd+H, Opt+Cmd+H, Cmd+Q, Cmd+O, Opt+Cmd+O, Shift+Cmd+O, Opt+Shift+Cmd+O, Cmd+N (hidden), Shift+Cmd+H, Cmd+W, Edit-menu standard items, Cmd+J, Shift+Cmd+F, Shift+Cmd+G, Cmd+I, Ctrl+Cmd+L, Cmd+M, Cmd+?.

```
IINA
- About IINA
- Check for Updates...                      (Sparkle)  [mac-only]
- Preferences...                            (Cmd+,)   -> new Settings window (or legacy Preferences window)
- Services >                                [mac-only]
- Hide IINA (Cmd+H) / Hide Others (Opt+Cmd+H) / Show All
- Quit IINA (Cmd+Q)

File
- Open...                                   (Cmd+O)
- Open in New Window...                     (Opt+Cmd+O)   [the pair swaps meaning with pref alwaysOpenInNewWindow]
- Open URL...                               (Shift+Cmd+O)
- Open URL in New Window...                 (Opt+Shift+Cmd+O)
- Open Recent >                             (system recent-documents list + "Clear Menu")
- New Window                                (Cmd+N)  [hidden unless pref enableCmdN]
- Playback History                          (Shift+Cmd+H)
- Show Current File in Finder               (Shift+Cmd+R)
- Delete Current File                       (no default key; moves to Trash)
- Save Current Playlist...
- Close                                     (Cmd+W)
- (hidden submenu) Other Actions From Key Bindings   [holds duplicate-bound shortcuts so every extra key still triggers a menu action]

Edit  [standard text-edit items; the playlist responds to Cut/Copy/Paste/Delete]
- Undo / Redo / Cut / Copy / Paste / Delete / Select All
- Transformations > Make Upper Case / Make Lower Case / Capitalize
- Speech > Start Speaking / Stop Speaking

Playback
- Pause / Resume                            (Space, p)   [title flips]
- Stop and Clear Playlists                  (Cmd+.)
- Step Forward 5s                           (Right)      [label follows the bound seek amount]
-   ~ Next Frame                            (Alt+Right)
- Step Backward 5s                          (Left)
-   ~ Previous Frame                        (Alt+Left)
- Jump to Beginning
- Jump to...                                (Cmd+J)      [time prompt, h:mm:ss.mmm]
- Speed: <current>                          [disabled indicator row]
- Speed Up to 2x                            (Cmd+])
-   ~ Speed Up to 1.1x                      (Alt+Cmd+])
- Speed Down to 0.5x                        (Cmd+[)
-   ~ Speed Down to 0.9x                    (Alt+Cmd+[)
- Reset Speed                               (Cmd+\)
- Take a Screenshot                         (Cmd+S)
- Go to Screenshot Folder
- (hidden) Advanced Screenshot...
- A-B Loop                                  (Cmd+L)      [checked while loop active]
- File Loop                                 (Shift+Cmd+L) [checked when mode=file]
- Show/Hide Playlist Panel                  (Shift+Cmd+P)
- Playlist Loop                             [checked when mode=playlist]
- Playlist >                                [generated: one item per entry, current checked]
- Next Media                                (Cmd+Right)
- Previous Media                            (Cmd+Left)
- Show/Hide Chapters Panel                  (Shift+Cmd+C)
- Chapters >                                [generated: "hh:mm:ss - title", current checked]
- Next Chapter                              (Shift+Cmd+>)
- Previous Chapter                          (Shift+Cmd+<)

Video
- Show/Hide Video Panel                     (Shift+Cmd+V)
- Cycle Video Tracks                        (Ctrl+Cmd+V)
- Video Track >                             [<None> + generated tracks]
- Half Size (Cmd+0) / Normal Size (Cmd+1) / Double Size (Cmd+2) / Fit to Screen (Cmd+3)
- (hidden) Normal Size (Retina)             (Opt+Cmd+1)
- Bigger Size (Cmd+=) / Smaller Size (Cmd+-)   [+-25pt width steps, aspect kept]
- Enter/Exit Picture in Picture             (Ctrl+Cmd+P)
- Enter/Exit Full Screen                    (Ctrl+Cmd+F; f; Enter/Esc)
- Float on Top                              (Ctrl+Cmd+T)  [checked]
- Lock Window Aspect Ratio                  [checked; disabled when edge-to-edge is off]
- Live Text                                 (Shift+Cmd+T) [macOS 13+; hidden otherwise]
- Enter/Exit Music Mode                     (Opt+Cmd+M)
- Aspect Ratio >                            Default, 4:3, 5:4, 16:9, 16:10, 1:1, 3:2, 2.21:1, 2.35:1, 2.39:1  [radio-checked]
- Crop >                                    None, (same ratios), separator, Custom... (interactive box)
- Rotation >                                0, 90, 180, 270 degrees
- Flip >                                    Horizontal (Mirror) / Vertical (Flip)  [toggle filters, checked]
- Deinterlace                               [checked]
- Delogo                                    [checked; unchecked -> interactive box]
- Video Filters...                          (Shift+Cmd+F)
- Saved Video Filters >                     [user filters, each with optional shortcut, checked if active]

Audio
- Show/Hide Audio Panel                     (Shift+Cmd+A)
- Cycle Audio Tracks                        (Ctrl+Cmd+A)
- Audio Track >                             [<None> + generated tracks]
- Load External Audio...
- Volume: N [(Muted)]                       [indicator row]
- Volume + 5% (Up) / + 1% (Alt+Up) / - 5% (Down) / - 1% (Alt+Down)
- Mute                                      (Cmd+/)       [checked]
- Audio Delay: N                            [indicator row]
- Audio Delay + 0.5s (Shift+( ) / + 0.1s (Alt+Shift+( ) / - 0.5s (Shift+) ) / - 0.1s (Alt+Shift+) )
- Reset Audio Delay                         (Shift+_)
- Audio Device >                            [from mpv audio-device-list, current checked]
- Audio Filters...                          (Shift+Cmd+G)
- Saved Audio Filters >

Subtitles
- Show/Hide Subtitles Panel                 (Shift+Cmd+S)
- Cycle Subtitles                           (Ctrl+Cmd+S)
- Subtitle >                                [<None> + generated tracks]
- Hide/Show Subtitles                       (v)
- Secondary Subtitle >                      [<None> + generated tracks]
- Hide/Show Secondary Subtitles             (Alt+v)
- Load External Subtitle...
- Find Online Subtitles...                  (Shift+Cmd+D) [title carries provider name]
- Find Online Subtitles from >              OpenSubtitles.com / assrt.net / shooter.cn / plugin-registered providers
- Save Downloaded Subtitle...               [copies the temp-folder download next to a chosen path]
- Encoding >                                Auto, separator, 43 encodings: UTF-8/16 variants plus legacy code pages (see SUB section)
- Scale Up / Scale Down / Reset Subtitle Scale   [+-10% steps; shortcuts only if bindings match multiply sub-scale]
- Subtitle Delay: N                         [indicator row]
- Subtitle Delay + 0.5s (X) / + 0.1s (Alt+X) / - 0.5s (Z) / - 0.1s (Alt+Z)
- Reset Subtitle Delay                      (C)
- Font...                                   [IINA font picker window]

Plugin   [only when plugin system enabled - always true in this build]
- Manage Plugins...
- Show/Hide Plugins Panel
- per-plugin sections: header + the plugin's menu items (first 5 inline, rest under "More..."), nested submenus, separators, check marks, optional key shortcuts
- "Conflicting key shortcuts..." badge item when a plugin shortcut collides with a key binding
- Developer Tool > one JS console per plugin instance
- Reload All Plugins

Window
- Minimize (Cmd+M) / Zoom
- (hidden) Custom Touch Bar...              [mac-only]
- Inspector                                 (Cmd+I)
- Log Viewer                                (Ctrl+Cmd+L)
- Bring All to Front
- (system) list of open windows

Help
- IINA Help                                 (Cmd+?)  -> GitHub wiki
- ~ Dump Debug Info                         (Option)
- Use New Settings Window                   [checked toggle]
- Release Highlights                        [web view with iina.io release highlights]
- GitHub / Website
```

Menu behaviours worth copying (each is its own tracked item):

- **MENU-01** Menu enable/disable and titles are refreshed lazily in `menuWillOpen` per menu (open/close panel, pause/resume, fullscreen, PiP, mini player, hide/show subs, volume/delay/speed indicator rows). Source: `iina/MenuController.swift`.
- **MENU-02** Key equivalents are *derived from the active key-binding set* (`updateKeyEquivalentsFrom`): ~70 menu items are pattern-matched against bindings (including numeric ranges, e.g. `seek 5..60` -> "Step Forward Ns"), the first matching key becomes the menu shortcut, additional keys bound to the same action are kept alive through a hidden "Other Actions From Key Bindings" submenu, and items with no binding lose their shortcut. Source: `iina/MenuController.swift`.
- **MENU-03** Menu labels that embed a number (`Step Forward Ns`, `Speed Up to Nx`, `Volume + N%`, `Audio Delay +- Ns`, ...) are rebuilt from the bound amount using localized format strings. Source: `iina/MenuController.swift` `updateMenuItem`.
- **MENU-04** Seek menu items honour exact-seek flags in the binding (`seek 5 exact`), passing the seek mode as side data. Source: `iina/MenuController.swift` `sameKeyAction`.
- **MENU-05** "Open"/"Open in New Window" titles swap based on `alwaysOpenInNewWindow`, with Option as the inverse modifier. Source: `iina/MenuController.swift` `updateOpenMenuItems`.
- **MENU-06** All menus can be disabled en masse (used while quitting and while modal alerts are up). Source: `iina/MenuController.swift` `disableAllMenus`.
- **MENU-07** Menu items act on the *active* player (`PlayerCore.active`), whichever window is main; in music mode window-specific actions (float on top, panels, mini-player toggle) are redirected to the mini player controller. Source: `iina/MainMenuActions.swift`, `iina/MiniPlayerWindowMenuActions.swift`.
- **MENU-08** Saved filter menus: each saved filter becomes a toggle item with its own shortcut key+modifiers; checked state reflects the live mpv filter chain. Source: `iina/MenuController.swift` `updateSavedFilters`.
- **MENU-09** Playlist-row context menu, title-bar path menu, time-label precision menu, plugin playlist menu builders - see sections 1, 2, 4.
- **MENU-10** Text-edit menu actions are routed to the playlist table (cut/copy/paste/delete rows) when it is first responder. Source: `iina/SidebarPlaylistPane.swift`.

## 7. Preferences (every pane, every option, defaults)

Settings window: `iina/SettingsWindow.swift` (new, default) - split view with a left sidebar of 11 pages, a global **search field** with a completion popover that jumps to and highlights the matching setting (`iina/SettingsSearch.swift`), per-page "sub-section" indicator that follows scrolling, "?" help links to the mpv manual / wiki on many rows, rows can expand to reveal dependent options. Page order: General, UI, Video, Audio, Subtitles, Network, Control, Key Bindings, Advanced, Plugins, Utilities. Legacy window (`iina/PreferenceWindowController.swift` + `iina/Pref*ViewController.swift`) has the same panes as a toolbar of tabs and is still reachable.

Storage: macOS `UserDefaults` (plist, domain `com.colliderli.iina`); all keys are registered with defaults at launch (`Preference.defaultPreference`). Changes propagate live: the mpv controller observes the keys and re-sets the mpv option on the running core. Patina needs a typed settings store with the same "defaults + live observers" shape. [mac-ish for UserDefaults]

Column legend: *Key* = defaults key. *Default* = registered default. *mpv/behaviour* = what it drives.

### 7.1 General (`iina/SettingsPageGeneral.swift`, legacy `PrefGeneralViewController`)

| ID | Key | UI control | Default | mpv / behaviour |
|---|---|---|---|---|
| PREF-GEN-01 | `actionAfterLaunch` | popup: Show welcome window / Show open panel / Do nothing | welcomeWindow (0) | launch behaviour |
| PREF-GEN-02 | `pauseWhenOpen` | switch "Pause" under "When media is opened" | false | start paused |
| PREF-GEN-03 | `fullScreenWhenOpen` | switch "Enter full screen" | false | enter FS when the video size is known |
| PREF-GEN-04 | `pauseWhenMinimized` | switch under "Pause/resume when" | false | auto-pause on minimize |
| PREF-GEN-05 | `pauseWhenInactive` | switch | false | auto-pause when app loses focus |
| PREF-GEN-06 | `playWhenEnteringFullScreen` | switch | false | resume when entering FS |
| PREF-GEN-07 | `pauseWhenLeavingFullScreen` | switch | false | pause when leaving FS |
| PREF-GEN-08 | `pauseWhenGoesToSleep` | switch | true | pause on system sleep |
| PREF-GEN-09 | `alwaysOpenInNewWindow` | switch "Allow multiple player windows" | true | see WIN-02 |
| PREF-GEN-10 | `groupSimultaneousOpensInPlaylist` | sub-switch "Group media in playlist when opening multiple files at once" | false | multi-file open -> one window |
| PREF-GEN-11 | `allowDuplicatePlayers` | sub-switch "Allow opening the same file in multiple windows" | false | allow same file in 2 windows |
| PREF-GEN-12 | `quitWhenNoOpenedWindow` | switch "Quit after all windows are closed" | false | quit after last window closes |
| PREF-GEN-13 | `keepOpenOnFileEnd` | switch "Keep window open after playback finishes" | true | mpv `keep-open` (yes / always if auto-play-next is off / no) |
| PREF-GEN-14 | `useLegacyFullScreen` | switch "Use legacy full screen" | false | legacy FS (macOS-only) |
| PREF-GEN-15 | `blackOutMonitor` | switch "Black out other monitors in full screen" | false | WIN-16 |
| PREF-GEN-16 | `preventScreenSaver` | switch "Prevent screen saver from starting while playing" | true | `ProcessInfo.beginActivity` while a player is *playing* |
| PREF-GEN-17 | `allowScreenSaverForAudio` | sub-switch "Not while in music mode or only playing audio" (desc: still prevents system sleep) | false | audio/mini-player playback only blocks system sleep, not display sleep |
| PREF-GEN-18 | `autoSwitchToMusicMode` | switch "Switch to music mode automatically when playing an audio file" | true | auto enter/leave music mode by track type |
| PREF-GEN-19 | Sparkle updater properties `automaticallyChecksForUpdates` + `updateCheckInterval` (stored in Sparkle's own defaults) | switch + popup hourly (3600 s) / daily (86400, default) / weekly / monthly | daily | Sparkle updater [mac-only] |
| PREF-GEN-20 | `receiveBetaUpdate` | sub-switch | false | switch appcast feed to beta |
| PREF-GEN-21 | `resumeLastPosition` | switch | true | mpv `save-position-on-quit` + `resume-playback`, watch-later dir in app support |
| PREF-GEN-22 | `recordPlaybackHistory` | switch "Enable playback history" | true | feeds History window |
| PREF-GEN-23 | `recordRecentFiles` | switch "Enable Open Recent menu" | true | feeds Open Recent / welcome window / "Resume" |
| PREF-GEN-24 | `trackAllFilesInRecentOpenMenu` | sub-switch "Track all played files in Open Recent menu" (otherwise only files opened manually appear) | true | adds each loaded file to Recent |
| PREF-GEN-25 | `playlistAutoAdd` | switch "Add files in the same folder automatically" (label still says restart needed, but code reads it per file) | true | PLC-07 |
| PREF-GEN-26 | `playlistAutoPlayNext` | switch "Play next item automatically" | true | continue to next item |
| PREF-GEN-27 | `playlistShowMetadata` | switch "Show artist and track name for audio files when available" | true | PLC-01 |
| PREF-GEN-28 | `playlistShowMetadataInMusicMode` | sub-switch "Only in music mode" | true | metadata only in music mode |
| PREF-GEN-29 | `autoRepeat` | switch "Loop when a file is opened manually" + popup | false | repeat on open |
| PREF-GEN-30 | `defaultRepeatMode` | popup: Loop playlist / Loop single file | playlist (0) | `loop-playlist=inf` or `loop-file=inf` |
| PREF-GEN-31 | `screenshotSaveToFile` | switch + folder chooser | true | saves file |
| PREF-GEN-32 | `screenShotFolder` (`screenshotFolder`) | text + Choose... | `~/Pictures/Screenshots` | mpv `screenshot-dir` (temp cache dir if not saving) |
| PREF-GEN-33 | `screenShotFormat` | popup (sorted by name): png, jpg, jpeg, webp, jxl, avif | png | mpv `screenshot-format`; jxl forces `screenshot-sw=yes`; avif encoder forced to libsvtav1 with `avif=1` |
| PREF-GEN-34 | `screenshotCopyToClipboard` | switch | false | copy image to pasteboard |
| PREF-GEN-35 | `screenShotIncludeSubtitle` | switch | true | `screenshot subtitles` vs `video` |
| PREF-GEN-36 | `screenshotShowPreview` | switch | true | OSD preview card with Reveal/Edit/Delete |
| PREF-GEN-37 | `screenShotTemplate` | **no UI** [hidden] | `%F-%n` | mpv `screenshot-template` |
| PREF-GEN-38 | `enableCmdN` | **no UI** [hidden] | false | un-hide File > New Window (Cmd+N) |

### 7.2 UI (`iina/SettingsPageUI.swift`, legacy `PrefUIViewController`)

| ID | Key | UI control | Default | mpv / behaviour |
|---|---|---|---|---|
| PREF-UI-01 | `themeMaterial` | popup: Dark / Light / Match system appearance | dark (0; values 0,2,4) | window appearance |
| PREF-UI-02 | `useLiquidGlassOSD`, `useLiquidGlassOSC`, `useLiquidGlassSidebar` | 3 switches (macOS 26+) | true, true, true | glass vs blur [mac-only] |
| PREF-UI-03 | `compactUI` | switch | false | compact metrics |
| PREF-UI-04 | `initialWindowSizePosition` | switches + width/height value + unit + side popups | "" | mpv `geometry` |
| PREF-UI-05 | `resizeWindowOption` | popup: Fit screen / 0.5x / 1x / 1.5x / 2x video size | 1x (2) | WIN-06 |
| PREF-UI-06 | `resizeWindowTiming` | radio: Always when playing / When media is opened manually / Do not resize | onlyWhenOpen (1) | WIN-06 |
| PREF-UI-07 | `usePhysicalResolution` | switch "Use physical resolution on Retina displays" | true | WIN-06 |
| PREF-UI-08 | `alwaysFloatOnTop` | switch | false | float while playing |
| PREF-UI-09 | `alwaysShowOnTopIcon` | switch | false | pin icon |
| PREF-UI-10 | `oscPosition` | 3-card radio: Floating / Top / Bottom | floating (0) | OSC-01 |
| PREF-UI-11 | `controlBarToolbarButtons` | "Customize..." drag sheet | [plugins, pip, playlist, settings] | OSC-04 |
| PREF-UI-12 | `arrowBtnAction` (`arrowButtonAction`) | popup "Use left/right button for": Speed (0) / Previous / Next Media (1) / Fast Forward / Rewind (2 = seek +-10 s) | speed (0) | OSC-06 |
| PREF-UI-13 | `enableControlBarAutoHide` + `controlBarAutoHideTimeout` | switch + seconds input | true, 2.5 | OSC-07 |
| PREF-UI-14 | `controlBarStickToCenter` | switch | true | OSC-02 |
| PREF-UI-15 | `showChapterPos` | switch | false | chapter ticks on slider |
| PREF-UI-16 | `showRemainingTime` + `scaleRemainingTime` | switch + sub-switch "Base remaining time on playback speed" | false, false | OSC-09 |
| PREF-UI-17 | `disablePlaySliderScrolling` | switch "Prevent scroll wheel from changing playback position" | false | wheel on seek bar |
| PREF-UI-18 | `disableVolumeSliderScrolling` | switch "Prevent scroll wheel from changing volume" | false | wheel on volume bar |
| PREF-UI-19 | `enableOSD` | switch "Enable OSD" | true | master OSD |
| PREF-UI-20 | `disableOSDFileStartMsg`, `disableOSDPauseResumeMsgs`, `disableOSDSeekMsg`, `disableOSDSpeedMsg` | 4 switches under "Suppress messages for" | false x4 | OSD filter |
| PREF-UI-21 | `osdAutoHideTimeout` | seconds input | 1 | |
| PREF-UI-22 | `osdTextSize` | pt input (1-500) | 20 | |
| PREF-UI-23 | `displayTimeAndBatteryInFullScreen` | switch | false | OSC-12 |
| PREF-UI-24 | `enableThumbnailPreview` | switch | true | thumbnails |
| PREF-UI-25 | `maxThumbnailPreviewCacheSize` | MB input | 500 | disk cache cap |
| PREF-UI-26 | `enableThumbnailForRemoteFiles` | switch "Include files on a mounted remote drive" | false | thumbnails for network volumes |
| PREF-UI-27 | `thumbnailWidth` | pt input (1-2000) | 120 | changing it clears the cache |
| PREF-UI-28 | `windowBehaviorWhenPip` | popup: Do nothing / Hide / Minimize | hide (1) | PIP-03 |
| PREF-UI-29 | `pauseWhenPip` | switch | false | pause on entering PiP |
| PREF-UI-30 | `togglePipByMinimizingWindow` + `togglePipByMinimizingWindowForVideoOnly` | switch + sub-switch "Only for video files" | false, false | minimize == enter PiP |
| PREF-UI-31 | `disableAnimations` | switch (with Apple HIG help link) | false | zero-duration UI animations |

### 7.3 Video (`iina/SettingsPageVideo.swift`, legacy `PrefCodecViewController` top half)

| ID | Key | UI control | Default | mpv / behaviour |
|---|---|---|---|---|
| PREF-VID-01 | `videoThreads` | number | 0 (auto) | `vd-lavc-threads` |
| PREF-VID-02 | `hardwareDecoder` | 3-choice with descriptions: Disabled / Auto ("most video filters won't work") / Auto (Copy) ("double the memory but works with filters") | auto (1) | `hwdec` = no / auto / auto-copy (adding a video filter while hwdec=auto pops a dialog offering to turn hwdec off or switch to auto-copy) |
| PREF-VID-03 | `forceDedicatedGPU` | switch, Intel Macs only (`#if !arch(arm64)`) | false | GL pixel format choice [mac-only] |
| PREF-VID-04 | `enableLiveText` | switch, only if Live Text available | false | PLAT Live Text [mac-only] |
| PREF-VID-05 | `loadIccProfile` | switch "Load ICC profile" (SDR only) | true | `icc-profile` from current display |
| PREF-VID-06 | `enableHdrSupport` | switch "Enable HDR mode" ("enable HDR mode by default when playing HDR videos") | true | EDR/PQ output for HDR content [mac-only] |
| PREF-VID-07 | `enableToneMapping` | switch (+ help link) | true | `tone-mapping`/`target-peak` |
| PREF-VID-08 | `enableToneMappingTargetPeakOverride` + `toneMappingTargetPeakOverride` | switch + nits (10-10000) | false, 400 | `target-peak` (auto = detected display peak) |
| PREF-VID-09 | `toneMappingAlgorithm` | popup: auto, clip, mobius, reinhard, hable, bt.2390, gamma, linear | auto (0) | `tone-mapping` |
| PREF-VID-10 | `enableToneMappingParamOverride` + `toneMappingParamOverride` | switch + float | false, 1.0 | `tone-mapping-param` |
| PREF-VID-11 | `toneMappingTargetPeak` (legacy) | none | 0 | migrated to the override pair on upgrade |

### 7.4 Audio (`iina/SettingsPageAudio.swift`, legacy `PrefCodecViewController` bottom half)

| ID | Key | UI control | Default | mpv / behaviour |
|---|---|---|---|---|
| PREF-AUD-01 | `audioThreads` | number 0-16 | 0 | `ad-lavc-threads` |
| PREF-AUD-02 | `audioDriverEnableAVFoundation` | choice CoreAudio / AVFoundation (experimental, spatial audio) | false | `ao` [mac-only] |
| PREF-AUD-03 | `audioDevice` + `audioDeviceDesc` | popup of mpv audio devices (shows a "missing" entry if saved device absent) | "auto" / "Autoselect device" | `audio-device`; auto-resets to auto if device disappears; remaps device between ao drivers |
| PREF-AUD-04 | `spdifAC3`, `spdifDTS`, `spdifDTSHD`, `spdifEAC3`, `spdifTRUEHD` | 5 switches (with warning) | false x5 | `audio-spdif` list |
| PREF-AUD-05 | `enableInitialVolume` + `initialVolume` | switch + number (0-1000) | false, 100 | start volume (otherwise last volume `softVolume`, default 100) |
| PREF-AUD-06 | `maxVolume` | number (100-1000) | 100 | `volume-max`; all volume UI/OSD scale to it |
| PREF-AUD-07 | `replayGain` | popup: No adjustment / Track gain / Album gain | no (0) | `replaygain` |
| PREF-AUD-08 | `replayGainPreamp` | dB (-150..150) | 0 | `replaygain-preamp` |
| PREF-AUD-09 | `replayGainClip` | switch | false | `replaygain-clip` |
| PREF-AUD-10 | `replayGainFallback` | dB (-200..60) | 0 | `replaygain-fallback` |
| PREF-AUD-11 | `gaplessAudio` | 3-choice Disabled / Weak / Strong | weak (1) | `gapless-audio` = no / weak / yes |
| PREF-AUD-12 | `audioLanguage` | language token field (ISO-639 autocomplete, comma list) | "" | `alang` |
| PREF-AUD-13 | `userEQPresets` | managed in Quick Settings Audio tab | {} | JSON of user EQ profiles |
| PREF-AUD-14 | `softVolume` | none (remembered last volume) | 100 | `volume` |

### 7.5 Subtitles (`iina/SettingsPageSubtitles.swift`, legacy `PrefSubViewController`)

| ID | Key | UI control | Default | mpv / behaviour |
|---|---|---|---|---|
| PREF-SUB-01 | `subAutoLoadIINA` | popup: Disabled / "Subtitles containing media filename" (mpvFuzzy) / "Detect intelligently by IINA" | iina (2) | IINA's own matcher; mpv `sub-auto` is always set to `no` |
| PREF-SUB-02 | `subAutoLoadPriorityString` | text (comma list) under "Advanced" | "" | subs whose filenames contain these strings are moved first and selected |
| PREF-SUB-03 | `subAutoLoadSearchPath` | text, colon-separated dirs, `./*`-style wildcard at end | `./*` | extra folders scanned for subs (relative to video, absolute, `~`) |
| PREF-SUB-04 | `subOverrideLevel` | segmented Primary/Secondary + 5 radios: no / yes / scale / force / strip (ordered no,yes,scale,force,strip) | scale (3) | `sub-ass-override` (primary) |
| PREF-SUB-05 | `secondarySubOverrideLevel` | same, for secondary | scale | `secondary-sub-ass-override` |
| PREF-SUB-06 | `subTextFont` | font button (custom picker window) | `sans-serif` | `sub-font` |
| PREF-SUB-07 | `subTextSize` | number | 55 | `sub-font-size` |
| PREF-SUB-08 | `subBold`, `subItalic` | toggle buttons | false, false | `sub-bold`, `sub-italic` |
| PREF-SUB-09 | `subTextColorString` | colour well (mpv `#AARRGGBB`-style string) | white | `sub-color` |
| PREF-SUB-10 | `subBorderStyle` | popup: outline-and-shadow (1) / opaque-box (3) / background-box (4) | 1 | `sub-border-style` |
| PREF-SUB-11 | `subBorderSize`, `subBorderColorString` | number + colour | 3, black | `sub-outline-size`, `sub-outline-color` |
| PREF-SUB-12 | `subShadowSize`, `subShadowColorString` | number + colour | 0, clear | `sub-shadow-offset`, `sub-back-color` |
| PREF-SUB-13 | `subBlur` | 0-20 | 0 | `sub-blur` |
| PREF-SUB-14 | `subSpacing` | -10..10 | 0 | `sub-spacing` |
| PREF-SUB-15 | `subAlignX`, `subAlignY` | popups Left/Center/Right; Top/Center/Bottom | center, bottom | `sub-align-x/y` |
| PREF-SUB-16 | `subMarginX`, `subMarginY` | numbers | 25, 22 | `sub-margin-x/y` |
| PREF-SUB-17 | `subPos` | 0-150 % | 100 | `sub-pos` |
| PREF-SUB-18 | `subScaleWithWindow` | switch | true | `sub-scale-by-window` |
| PREF-SUB-19 | `displayInLetterBox` | switch | true | `sub-use-margins` + `sub-ass-force-margins` |
| PREF-SUB-20 | `onlineSubProvider` | popup: opensubtitles.com / assrt.net / shooter.cn / plugin providers | `:opensubtitles` | selects fetcher |
| PREF-SUB-21 | `openSubUsername` (+ password in Keychain) | login UI (with legacy-account migration notice) | "" | OpenSubtitles.com REST login |
| PREF-SUB-22 | `assrtToken` | text + help button | "" (built-in shared token used if not 32 chars) | assrt.net API |
| PREF-SUB-23 | `autoSearchOnlineSub` | switch | false | auto search when no subs loaded |
| PREF-SUB-24 | `autoSearchThreshold` | minutes (no UI in new page; old UI says 20 min) | 20 | min duration for auto search |
| PREF-SUB-25 | `subLang` | language token field | "" | `slang` |
| PREF-SUB-26 | `defaultEncoding` | popup of 44 entries (Auto-detect + 43 encodings) | auto | `sub-codepage`; changing it reloads all subs |
| PREF-SUB-27 | `ignoreAssStyles` | **unused** [hidden] | false | declared but not referenced |
| PREF-SUB-28 | `onlineSubSource` (legacy) | none | 1 | migration only |

### 7.6 Network (`iina/SettingsPageNetwork.swift`, legacy `PrefNetworkViewController`)

| ID | Key | UI control | Default | mpv / behaviour |
|---|---|---|---|---|
| PREF-NET-01 | `enableCache` | switch "Prefetch and buffer network data" | true | mpv `cache` yes/no |
| PREF-NET-02 | `cachePauseInitial` | switch "Buffer before starting playback" | false | `cache-pause-initial` |
| PREF-NET-03 | `cachePauseWait` | seconds (float) "Seconds to buffer before restarting" | 1 | `cache-pause-wait` |
| PREF-NET-04 | `secPrefech` (sic) | seconds "Seconds to prefetch" | 36000 | `cache-secs` |
| PREF-NET-05 | `defaultCacheSize` | KiB "Cache buffer size (KiB)" | 153600 | `demuxer-max-bytes` |
| PREF-NET-06 | `showBufferingThrobber` | switch | true | OSC-13 |
| PREF-NET-07 | `showSeekingThrobber` | switch | true | OSC-13 |
| PREF-NET-08 | `userAgent` | text | "" | `user-agent` |
| PREF-NET-09 | `httpProxy` | text `host:port` | "" | sets `http_proxy=http://...` in the process env before mpv starts (restart needed) |
| PREF-NET-10 | `transportRTSPThrough` | popup: lavf / tcp / udp / http | tcp (1) | `rtsp-transport` |
| PREF-NET-11 | `ytdlEnabled` | switch "Enable mpv's youtube-dl script" under "Online Videos (yt-dlp)" (+ help link; whole section disabled when the "Online Media" plugin is enabled, and a banner recommends the plugin) | true | mpv `ytdl` |
| PREF-NET-12 | `ytdlSearchPath` | text | "" | prepended to `PATH` for finding youtube-dl/yt-dlp (restart needed) |
| PREF-NET-13 | `ytdlRawOptions` | text `key=value,...` | "" | `ytdl-raw-options` |

### 7.7 Control (`iina/SettingsPageControl.swift`, legacy `PrefControlViewController`)

| ID | Key | UI control | Default | behaviour |
|---|---|---|---|---|
| PREF-CTL-01 | `pinchAction` | popup (Trackpad): Adjust window size / Enter-exit fullscreen / None | windowSize (0) | magnify gesture [mac-only] |
| PREF-CTL-02 | `forceTouchAction` | popup (options None / Fullscreen / Pause-Resume / Hide OSC) | none | Force Touch second stage [mac-only] |
| PREF-CTL-03 | `singleClickAction` | popup (None / Pause-Resume / Hide OSC) | hideOSC (3) | single click on video |
| PREF-CTL-04 | `doubleClickAction` | popup "Double click to" (None / Enter-Exit full screen / Pause-Resume / Enter-Exit PiP / A-B loop / Reset speed) | fullscreen (1) | double click; a single-click action is delayed by the double-click interval when this is not "None" |
| PREF-CTL-05 | `rightClickAction` | popup (None / Pause / Hide OSC / PiP / A-B loop / Reset speed) | pause (2) | right click |
| PREF-CTL-06 | `middleClickAction` | popup (all 7 actions) | none | middle click |
| PREF-CTL-07 | `videoViewAcceptsFirstMouse` | switch | false | click that focuses the window also acts |
| PREF-CTL-08 | `verticalScrollAction` | popup: Adjust volume / Seek / None / Adjust playback speed | volume (0) | scroll direction is latched per gesture |
| PREF-CTL-09 | `horizontalScrollAction` | same list | seek (1) | |
| PREF-CTL-10 | `useExactSeek` | popup: Keyframe seek / Exact seek / Auto | relative (0) | relative seek mode; "auto" probes the first seek and uses exact if it took < 50 ms |
| PREF-CTL-11 | `relativeSeekAmount` | 4-tick slider | 3 | scroll seek sensitivity table (trackpad 0.05..0.5 s per delta, mouse 0.5..4 s) |
| PREF-CTL-12 | `volumeScrollAmount` | 4-tick slider | 3 | volume sensitivity table 0.25..1 per delta |
| PREF-CTL-13 | `playbackSpeedScrollAmount` | 4-tick slider | 3 | speed sensitivity 0.1%-1% per delta, clamp 0.05..4x |
| PREF-CTL-14 | `followGlobalSeekTypeWhenAdjustSlider` | **no UI** [hidden] | false | slider drags are always exact unless this is true |

### 7.8 Key Bindings (`iina/SettingsPageKeyBindings.swift`, legacy `PrefKeyBindingViewController`)

| ID | Key | UI control | Default | behaviour |
|---|---|---|---|---|
| PREF-KEY-01 | `useMediaKeys` | switch "Use system media control" (+ wiki link) | true | MPRemoteCommandCenter + Now Playing |
| PREF-KEY-02 | `currentInputConfigName` | popup of bundled sets (IINA Default, mpv Default, VLC Default, Movist Default, Movist v2 Default; locked) + user sets | "IINA Default" | active `input.conf` |
| PREF-KEY-03 | (config management) | buttons: new empty set, duplicate current, import existing file, show file in Finder, delete user set | | files live in `<app support>/input_conf/*.conf` |
| PREF-KEY-04 | (binding table) | search field + "+" + inline editor; columns Key / Action; double-click to edit; key-record sheet; pick action from list or type any mpv command | | edits write the `.conf` file immediately |
| PREF-KEY-05 | `displayKeyBindingRawValues` | switch "Display raw values" | false | show mpv key names/commands instead of glyphs/readable text |
| PREF-KEY-06 | `useAppleRemote` | **unused** [hidden] | false | declared, not referenced |

### 7.9 Advanced (`iina/SettingsPageAdvanced.swift`, legacy `PrefAdvancedViewController`)

| ID | Key | UI control | Default | behaviour |
|---|---|---|---|---|
| PREF-ADV-01 | `enableAdvancedSettings` | master switch (+ wiki "MPV Options and Properties" link); all other advanced controls are disabled until on | false | gate for the rows below; restart needed |
| PREF-ADV-02 | `logLevel` | popup Verbose / Debug / Warning / Error | debug (1) | log filter |
| PREF-ADV-03 | `enableLogging` | switch + "Open Log Directory" (effective only when advanced settings are on) | false | write `iina.log` and `mpv.log` per session folder under `~/Library/Logs/<bundle id>/` |
| PREF-ADV-04 | (Open Log Window) | row button | | opens Log Viewer |
| PREF-ADV-05 | `useMpvOsd` | switch "Use mpv's OSD" | false | uses mpv's native OSD instead of IINA's |
| PREF-ADV-06 | `useUserDefinedConfDir` + `userDefinedConfDir` | switch + path chooser | false, `~/.config/mpv/` | mpv `config=yes` + `config-dir` so `mpv.conf`, `scripts/`, `fonts/` etc. load |
| PREF-ADV-07 | `userOptions` | editable key/value table with +/- buttons, drag reordering, paste `key=value` splits | [] | each pair set with `mpv_set_option_string` before init; errors shown as an alert |

### 7.10 Plugins (`iina/SettingsPagePlugin.swift`, legacy `PrefPluginViewController`) - details in section 16

| ID | Control | behaviour |
|---|---|---|
| PREF-PLG-01 | "Get Plugins" (plugin store sheet) / "Install Local Package" (.iinaplgz) | install flow |
| PREF-PLG-02 | installed-plugin list: enable switch, name, version, DEV badge for external dev folders, update arrow, description, About button, "..." menu (Uninstall, Show in Finder) | |
| PREF-PLG-03 | "Check for updates" with progress/"N updates found" | GitHub-based updates |
| PREF-PLG-04 | details sheet tabs: Settings (plugin-provided HTML page), About (author, source, permissions, registered subtitle providers), Help (plugin help page or URL) | |
| PREF-PLG-05 | drag to reorder plugins (stored in `PluginOrder`) | load order |

### 7.11 Utilities (`iina/SettingsPageUtilities.swift`, legacy `PrefUtilsViewController`)

| ID | Control | behaviour |
|---|---|---|
| PREF-UTL-01 | "Set IINA as the default application..." with checkboxes Video / Audio / Playlist | `LSSetDefaultRoleHandlerForContentType` for every imported UTI [mac-only] |
| PREF-UTL-02 | "Restore suppressed alerts" | clears "don't show again" flags (currently nothing to restore) |
| PREF-UTL-03 | "Clear saved playback progress" | deletes the `watch_later` folder |
| PREF-UTL-04 | "Clear playback history" | clears history + recent documents + last-played pref |
| PREF-UTL-05 | "Clear thumbnail cache" with live size readout | wipes `thumb_cache` |
| PREF-UTL-06 | Browser extensions: Safari (opens extension prefs), Chrome, Firefox download buttons | links to store pages |

### 7.12 Non-UI / internal keys (`iina/Preference.swift`)

| ID | Key | Default | Purpose |
|---|---|---|---|
| PREF-HID-01 | `enableNewSettings` | true | select new vs legacy settings window |
| PREF-HID-02 | `iinaEnablePluginSystem` | false | legacy flag (plugin system is compiled in: `IINA_ENABLE_PLUGIN_SYSTEM = true`) |
| PREF-HID-03 | `iinaLastPlayedFilePath`, `iinaLastPlayedFilePosition` | unset | welcome-window "Resume" |
| PREF-HID-04 | `savedVideoFilters`, `savedAudioFilters` | [] | user-saved filters (name, filter string, shortcut) |
| PREF-HID-05 | `watchProperties` | [] | Inspector "Watch" rows |
| PREF-HID-06 | `musicModeShowPlaylist`, `musicModeShowAlbumArt` | false, true | mini-player panel state |
| PREF-HID-07 | `controlBarPositionHorizontal`, `controlBarPositionVertical` | 0.5, 0.1 | floating OSC position |
| PREF-HID-08 | `leadingSidebarWidth`, `trailingSidebarWidth` | 360 | sidebar widths |
| PREF-HID-09 | `sidebarSettingsDisplayAtLeading`, `sidebarPlaylistDisplayAtLeading`, `sidebarPluginsDisplayAtLeading` | false, true, true | sidebar sides |
| PREF-HID-10 | `edgeToEdgeVideo`, `unlockWindowAspectRatio`, `dockedControlBarAndTitlebar` | true, false, false | layout toggles (UI in sidebar Layout tab) |
| PREF-HID-11 | `prefetchPlaylistVideoDuration` | true | probe durations for playlist rows |
| PREF-HID-12 | `timeDisplayPrecision`, `touchbarShowRemainingTime` | 0, true | precision / Touch Bar remaining toggle (set from the UI itself) |
| PREF-HID-13 | `enableRecentDocumentsWorkaround`, `recentDocuments` | false, [] | work around macOS clearing recent items for ad-hoc signed builds |
| PREF-HID-14 | `enableFFmpegImageDecoder` | true | FFmpeg fallback to decode JXL/WebP screenshots for previews |
| PREF-HID-15 | `enableHdrWorkaround` | false | invisible 0.1pt view to avoid dimmed HDR in full screen (macOS 13) |
| PREF-HID-16 | `enableNowPlayingArtwork` | true | Now Playing artwork |
| PREF-HID-17 | `enableDisplayIdle` | true | stop display link when idle |
| PREF-HID-18 | `enableWrongScreenWorkaround` | true | AppKit wrong-screen workaround (pre-macOS 26) |
| PREF-HID-19 | `receiveBetaUpdate`, `PluginOrder`, `MainWindowLastPosition`, `firstLaunchAfter<version>`, `installedDefaultPlugins` | | misc persisted state |

## 8. Keyboard, mouse, trackpad & remote controls

### 8.1 Key-binding system

IINA's keyboard layer **is** mpv's `input.conf` language plus a few IINA extensions. The active binding set is a `.conf` file (bundled read-only presets in `iina/config/`, user sets in `<Application Support>/com.colliderli.iina/input_conf/`). On launch/change the file is parsed into a dictionary `normalized key -> KeyMapping` (last duplicate wins) and also handed to mpv as `input-conf` so mpv itself still sees media-key/remote events. Sources: `iina/KeyMapping.swift`, `iina/KeyCodeHelper.swift`, `iina/KeyBindingTranslator.swift`, `iina/PlayerCore.swift` (`loadKeyBindings/setKeyBindings`), `iina/PlayerWindowController.swift` (`keyDown`, `handleKeyBinding`).

- **INP-01** File format = mpv `input.conf` (one `KEY COMMAND ARGS...` per line, `#` comments, trailing `#comment` retained). IINA-only actions are written as `#@iina KEY iina-command` so that real mpv treats the line as a comment. `{section}` prefixes: `{default}` is stripped, any other section is skipped. `default-bindings start` is ignored.
- **INP-02** IINA-only commands (`IINACommand`): `toggle-pip`, `open-file`, `open-url`, `video-panel`, `audio-panel`, `sub-panel`, `playlist-panel`, `chapter-panel`, `toggle-music-mode`, `live-text`, `toggle-flip`, `toggle-mirror`, `bigger-window`, `smaller-window`, `fit-to-screen`, `save-playlist`, `show-current-file-in-finder`, `delete-current-file`, `delete-current-file-hard`, `find-online-subs`, `save-downloaded-sub`. Source: `iina/IINACommand.swift`.
- **INP-03** Key names use mpv's vocabulary; canonical modifier order `Ctrl`, `Alt`, `Shift`, `Meta` (Meta = Command on macOS); an upper-case letter means Shift+letter; special names `SPACE ENTER KP_ENTER ESC TAB BS DEL INS HOME END PGUP PGDWN LEFT RIGHT UP DOWN F1..F20 KP0..KP9 KP_DEC SHARP PLUS` and media/remote names (`PLAY PAUSE PLAYPAUSE PLAYONLY PAUSEONLY STOP FORWARD REWIND NEXT PREV VOLUME_UP VOLUME_DOWN MUTE POWER CLOSE_WIN`). Keys are normalised so `meta+shift+k` == `Meta+K`; reserved characters in config files are escaped. Source: `iina/KeyCodeHelper.swift`. The macOS key-code table is [mac-only]; the vocabulary is portable.
- **INP-04** Dispatch order for a key press in the player window: (a) the main menu gets first shot at key equivalents (so menu items and bindings coincide), (b) otherwise window forwards `keyDown` to the controller (so Esc/Tab are not swallowed by focus navigation), (c) plugin `input` listeners with priority high (200) run before and low (100) after the built-in handler, (d) `PlayerCore.keyBindings[normalizedKey]` is looked up and executed. Arrow keys always go to text fields when one has focus. Source: `iina/MainWindow.swift`, `iina/PlayerWindowController.swift`, `iina/PluginInputManager.swift`.
- **INP-05** Commands IINA intercepts instead of forwarding to mpv: `quit` (graceful app termination), `ab-loop` (keeps UI knobs in sync, OSD), `screenshot` (IINA's flag-aware screenshot with clipboard/preview), `show-progress` (IINA OSD). Everything else is passed verbatim as a raw mpv command string, so any mpv command (`seek` flags, `set`, `cycle`, `cycle-values`, `add`, `multiply`, `playlist-*`, `loadfile`, `loadlist`, `run`, `write-watch-later-config`, `sub-seek`, ...) works. Source: `iina/PlayerWindowController.swift` `handleKeyBinding`.
- **INP-06** Human-readable rendering of raw commands in the binding table (Mustache templates + per-command format strings + property-name translations, e.g. "Seek forward 5s", "Set Speed to 1.0", "Cycle Fullscreen", "Multiply Speed by 2.0x"); unsupported prefixes (`no-osd`, `osd-auto`, `osd-bar`, `osd-msg-bar`, `raw`, `repeatable`, `expand-properties`) are ignored when rendering; a "raw values" toggle shows the real text. Source: `iina/KeyBindingTranslator.swift`, `iina/MPVCommandFormat.strings`, `iina/en.lproj/KeyBinding.strings`.
- **INP-07** Binding editor sheet: pick an action from a categorised popup (Ignore, Seek, Revert seek, Set, Cycle, Cycle in values, Change/add, Multiply, Next/Previous frame, A-B loop, Subtitle seek, Next/Previous media, Move/Clear/Remove/Shuffle playlist, Record play history, Stop, Quit, Quit (save progress), Screenshot (to file), Load file, Load playlist, Run, plus the IINA commands) or type any mpv command, plus a "press keys to record" field. Source: `iina/KeyRecordViewController.swift`, `iina/KeyRecordView.swift`, `iina/PrefKeyBindingViewController.swift`.
- **INP-08** Config management: bundled read-only sets (IINA Default, mpv Default, VLC Default, Movist Default, Movist v2 Default), new empty user set, duplicate (needed before editing a bundled one), import an existing file, delete, reveal in Finder; invalid file => alert and fallback to IINA Default. Source: `iina/SettingsPageKeyBindings.swift`.
- **INP-09** Saved filters (video/audio) carry their own shortcut (key + modifier set) recorded with the same key-record view; the shortcut is attached to the Saved Filters menu item. Source: `iina/SavedFilter.swift`, `iina/FilterWindowController.swift`.
- **INP-10** Plugin-defined shortcuts for menu items are rejected (and listed under "Conflicting key shortcuts...") if they collide with an existing binding. Source: `iina/MenuController.swift`.

### 8.2 Mouse, trackpad, gestures, remote

- **INP-11** Click actions on the video (see PREF-CTL-03..07): single (default Hide OSC), double (default Fullscreen), right (default Pause), middle (default None). A pending single-click is cancelled by a second click inside the system double-click interval, and by the mouse re-entering the window (for hide-OSC). Actions: none, enter/exit fullscreen, pause/resume, hide OSC (UI + cursor), toggle PiP, A-B loop, reset speed. Source: `iina/PlayerWindowController.swift`, `iina/MainWindowController.swift`.
- **INP-12** Scroll wheel / two-finger scroll: vertical default = volume, horizontal default = seek; alternatives seek, volume, playback speed, none. Direction is latched at gesture start; momentum events that begin without the cursor in the window are dropped; natural-scrolling direction respected; scrolling over the seek bar always seeks and over the volume slider always changes volume (unless disabled); seek-by-scroll pauses at gesture begin and resumes at the end of a trackpad gesture. Sensitivity tables: trackpad seek factors 0.05/0.1/0.25/0.5 s per delta, mouse 0.5/1/2/4; volume 0.25/0.5/0.75/1 per delta (mouse uses raw delta); speed factor 0.1/0.2/0.5/1 %; speed clamped to 0.05-4x. Source: `iina/PlayerWindowController.swift` `scrollWheel`, `iina/AppData.swift`. Momentum/phase handling is [mac-only]; the mapping idea is portable.
- **INP-13** Pinch gesture: resize the window around its centre (respecting min size/screen) or toggle full screen (pinch-out enters, pinch-in exits) or nothing. Source: `iina/MainWindowController.swift` `handleMagnifyGesture`. [mac-only]
- **INP-14** Force Touch: second-stage pressure click performs an action (none / fullscreen / pause / hide OSC); the OSC arrow buttons use pressure levels as a speed accelerator. Source: `iina/MainWindowController.swift` `pressureChange`. [mac-only]
- **INP-15** Seek bar interaction: click/drag seeks to an absolute percentage (exact seek unless `followGlobalSeekTypeWhenAdjustSlider`), hover shows time/thumbnail, scrolling seeks, A/B loop knobs draggable. Source: `iina/PlayerWindowController.swift` `playSliderChanges`.
- **INP-16** Haptic feedback on OSC snap-to-centre and on crossing 100% volume when volume boost is on. [mac-only]
- **INP-17** System media keys / headset / Control Center / AirPods: handled through `MPRemoteCommandCenter`: play, pause, toggle, stop, next/previous track (-> playlist), skip forward/backward (interval derived from the `FORWARD`/`REWIND` bindings, default 15 s), change playback position, change rate (0.5/1/1.5/2), change repeat mode. Each command first looks for a user binding with the matching media-key name (`PLAYONLY`, `PAUSEONLY`, `PLAYPAUSE`, `STOP`, `NEXT`, `PREV`, `FORWARD`, `REWIND`) so users can rebind them. Registered only while media is open; torn down on quit. Toggle: `useMediaKeys`. Source: `iina/AppDelegate.swift` `RemoteCommandController`. [mac-only]
- **INP-18** `useAppleRemote` pref exists but is unused; mpv's own media-key handling is disabled (`input-media-keys=no`). Source: `iina/MPVController.swift`. [hidden]
- **INP-19** Touch Bar (see PLAT section). [mac-only]
- **INP-20** Keyboard focus in the mini player: clicking the window background makes the window itself first responder so bindings keep working; the volume popover is scroll-driven. Source: `iina/MiniPlayerWindowController.swift`.
- **INP-21** Plugins can register global-ish input hooks: `iina.input.onKeyDown/onKeyUp(key, cb, priority)`, `onMouseUp/Down/Drag(button, cb, priority)` for left/right/other buttons; `normalizeKeyCode`, `getAllKeyBindings`. Source: `iina/JavascriptAPIInput.swift`.

### 8.3 Menu shortcuts follow bindings

See MENU-02. A menu item shows a shortcut only if some binding's action matches the menu item's canonical action (numbers may be in a range); this is the mechanism by which "IINA Default" yields Cmd-based menu shortcuts and "mpv Default" yields almost none.

### 8.4 Default key bindings table ("IINA Default" = `iina/config/iina-default-input.conf`)

`Meta` is the Command key on macOS (suggested Patina mapping: Ctrl on Windows/Linux, or a per-platform "primary modifier"). `Alt` is Option. An upper-case single letter means Shift+letter. "IINA" in the Kind column = IINA-only command (`#@iina` line); "mpv" = ordinary mpv command. The menu column says which menu item picks up the shortcut. Lines starting `#@iina` are comments to real mpv, which is why those rows are "IINA".

| ID | Key (conf syntax) | Mac glyphs | Command | Behaviour | Kind |
|---|---|---|---|---|---|
| KB-01 | `Shift+Meta+v` | ⇧⌘V | `video-panel` | toggle Quick Settings > Video tab | IINA |
| KB-02 | `Shift+Meta+a` | ⇧⌘A | `audio-panel` | toggle Audio tab | IINA |
| KB-03 | `Shift+Meta+s` | ⇧⌘S | `sub-panel` | toggle Subtitles tab | IINA |
| KB-04 | `Shift+Meta+p` | ⇧⌘P | `playlist-panel` | toggle Playlist tab | IINA |
| KB-05 | `Shift+Meta+c` | ⇧⌘C | `chapter-panel` | toggle Chapters tab | IINA |
| KB-06 | `Meta+3` | ⌘3 | `fit-to-screen` | window to largest size fitting the screen | IINA |
| KB-07 | `Meta+-` | ⌘- | `smaller-window` | shrink window 25pt width step | IINA |
| KB-08 | `Meta+=` | ⌘= | `bigger-window` | grow window 25pt width step | IINA |
| KB-09 | `Ctrl+Meta+p` | ⌃⌘P | `toggle-pip` | enter/exit PiP | IINA |
| KB-10 | `Shift+Meta+r` | ⇧⌘R | `show-current-file-in-finder` | reveal file | IINA |
| KB-11 | `Alt+Meta+m` | ⌥⌘M | `toggle-music-mode` | enter/exit music mode | IINA |
| KB-12 | `Shift+Meta+t` | ⇧⌘T | `live-text` | toggle Live Text | IINA |
| KB-13 | `Meta+D` | ⇧⌘D | `find-online-subs` | search online subtitles | IINA |
| KB-14 | `Ctrl+Meta+v` | ⌃⌘V | `cycle video` | next video track | mpv |
| KB-15 | `Ctrl+Meta+s` | ⌃⌘S | `cycle sub` | next subtitle track | mpv |
| KB-16 | `Ctrl+Meta+a` | ⌃⌘A | `cycle audio` | next audio track | mpv |
| KB-17 | `SPACE` | Space | `cycle pause` | pause/resume | mpv |
| KB-18 | `Meta+.` | ⌘. | `stop` | stop playback | mpv |
| KB-19 | `RIGHT` | → | `seek 5` | +5 s | mpv |
| KB-20 | `LEFT` | ← | `seek -5` | -5 s | mpv |
| KB-21 | `Alt+RIGHT` | ⌥→ | `frame-step` | next frame (pauses) | mpv |
| KB-22 | `Alt+LEFT` | ⌥← | `frame-back-step` | previous frame | mpv |
| KB-23 | `Shift+LEFT` | ⇧← | `sub-seek -1` | previous subtitle line | mpv |
| KB-24 | `Shift+RIGHT` | ⇧→ | `sub-seek 1` | next subtitle line | mpv |
| KB-25 | `Meta+s` | ⌘S | `screenshot` | take screenshot | mpv |
| KB-26 | `Meta+l` | ⌘L | `ab-loop` | A-B loop cycle | mpv |
| KB-27 | `Meta+L` | ⇧⌘L | `cycle-values loop "inf" "no"` | toggle single-file loop | mpv |
| KB-28 | `Meta+RIGHT` | ⌘→ | `playlist-next` | next media | mpv |
| KB-29 | `Meta+LEFT` | ⌘← | `playlist-prev` | previous media | mpv |
| KB-30 | `Shift+Meta+>` | ⇧⌘> | `add chapter 1` | next chapter | mpv |
| KB-31 | `Shift+Meta+<` | ⇧⌘< | `add chapter -1` | previous chapter | mpv |
| KB-32 | `Meta+[` | ⌘[ | `multiply speed 0.5` | half speed | mpv |
| KB-33 | `Meta+]` | ⌘] | `multiply speed 2.0` | double speed | mpv |
| KB-34 | `Alt+Meta+[` | ⌥⌘[ | `multiply speed 1/1.1` | speed /1.1 | mpv |
| KB-35 | `Alt+Meta+]` | ⌥⌘] | `multiply speed 1.1` | speed x1.1 | mpv |
| KB-36 | `Meta+\` | ⌘\ | `set speed 1.0` | reset speed | mpv |
| KB-37 | `Meta+0` | ⌘0 | `set window-scale 0.5` | half-size window | mpv |
| KB-38 | `Meta+1` | ⌘1 | `set window-scale 1` | normal size | mpv |
| KB-39 | `Meta+2` | ⌘2 | `set window-scale 2` | double size | mpv |
| KB-40 | `Ctrl+Meta+f` | ⌃⌘F | `cycle fullscreen` | toggle fullscreen | mpv |
| KB-41 | `Ctrl+Meta+t` | ⌃⌘T | `cycle ontop` | float on top | mpv |
| KB-42 | `UP` | ↑ | `add volume 5` | volume +5 | mpv |
| KB-43 | `DOWN` | ↓ | `add volume -5` | volume -5 | mpv |
| KB-44 | `Alt+UP` | ⌥↑ | `add volume 1` | volume +1 | mpv |
| KB-45 | `Alt+DOWN` | ⌥↓ | `add volume -1` | volume -1 | mpv |
| KB-46 | `Meta+/` | ⌘/ | `cycle mute` | mute | mpv |
| KB-47 | `Shift+(` | ⇧( | `add audio-delay 0.5` | audio delay +0.5 s | mpv |
| KB-48 | `Shift+)` | ⇧) | `add audio-delay -0.5` | audio delay -0.5 s | mpv |
| KB-49 | `Alt+Shift+(` | ⌥⇧( | `add audio-delay 0.1` | +0.1 s | mpv |
| KB-50 | `Alt+Shift+)` | ⌥⇧) | `add audio-delay -0.1` | -0.1 s | mpv |
| KB-51 | `Shift+_` | ⇧_ | `set audio-delay 0` | reset audio delay | mpv |
| KB-52 | `Z` | ⇧Z | `add sub-delay -0.5` | sub delay -0.5 s | mpv |
| KB-53 | `X` | ⇧X | `add sub-delay 0.5` | sub delay +0.5 s | mpv |
| KB-54 | `Alt+Z` | ⌥⇧Z | `add sub-delay -0.1` | -0.1 s | mpv |
| KB-55 | `Alt+X` | ⌥⇧X | `add sub-delay 0.1` | +0.1 s | mpv |
| KB-56 | `C` | ⇧C | `set sub-delay 0` | reset sub delay | mpv |
| KB-57 | `ESC` | Esc | `set fullscreen no` | leave fullscreen | mpv |
| KB-58 | `ENTER` | Return | `set fullscreen yes` | enter fullscreen | mpv |
| KB-59 | `q` | Q | `quit` | quit app | mpv |
| KB-60 | `p` | P | `cycle pause` | pause/resume | mpv |
| KB-61 | `.` | . | `frame-step` | next frame | mpv |
| KB-62 | `,` | , | `frame-back-step` | previous frame | mpv |
| KB-63 | `o` | O | `show-progress` | OSD progress | mpv |
| KB-64 | `P` | ⇧P | `show-progress` | OSD progress | mpv |
| KB-65 | `m` | M | `cycle mute` | mute | mpv |
| KB-66 | `Shift+PGUP` | ⇧PgUp | `seek 600` | +10 min | mpv |
| KB-67 | `Shift+PGDWN` | ⇧PgDn | `seek -600` | -10 min | mpv |
| KB-68 | `G` | ⇧G | `add sub-scale +0.1` | bigger subtitles | mpv |
| KB-69 | `F` | ⇧F | `add sub-scale -0.1` | smaller subtitles | mpv |
| KB-70 | `r` | R | `add sub-pos -1` | subtitles up | mpv |
| KB-71 | `R` | ⇧R | `add sub-pos +1` | subtitles down | mpv |
| KB-72 | `t` | T | `add sub-pos +1` | subtitles down | mpv |
| KB-73 | `v` | V | `cycle sub-visibility` | show/hide subtitles | mpv |
| KB-74 | `Alt+v` | ⌥V | `cycle secondary-sub-visibility` | show/hide secondary subs | mpv |
| KB-75 | `f` | F | `cycle fullscreen` | toggle fullscreen | mpv |
| KB-76 | `E` | ⇧E | `cycle edition` | next edition (Matroska/DVD) | mpv |
| KB-77 | `POWER` | (media) | `quit` | quit | mpv |
| KB-78 | `PLAY` | (media) | `cycle pause` | | mpv |
| KB-79 | `PAUSE` | (media) | `cycle pause` | | mpv |
| KB-80 | `PLAYPAUSE` | (media) | `cycle pause` | | mpv |
| KB-81 | `PLAYONLY` | (media) | `set pause no` | | mpv |
| KB-82 | `PAUSEONLY` | (media) | `set pause yes` | | mpv |
| KB-83 | `STOP` | (media) | `stop` | | mpv |
| KB-84 | `FORWARD` | (media) | `seek 15` | skip forward 15 s | mpv |
| KB-85 | `REWIND` | (media) | `seek -15` | skip back 15 s | mpv |
| KB-86 | `NEXT` | (media) | `playlist-next` | | mpv |
| KB-87 | `PREV` | (media) | `playlist-prev` | | mpv |
| KB-88 | `VOLUME_UP` | (media) | `add volume 2` | | mpv |
| KB-89 | `VOLUME_DOWN` | (media) | `add volume -2` | | mpv |
| KB-90 | `MUTE` | (media) | `cycle mute` | | mpv |
| KB-91 | `CLOSE_WIN` | (system) | `quit` | | mpv |

Additional hard-coded menu shortcuts (not in any binding file): Cmd+O Open, Opt+Cmd+O Open in New Window, Shift+Cmd+O Open URL, Opt+Shift+Cmd+O Open URL in New Window, Shift+Cmd+H History, Cmd+W Close, Cmd+J Jump to..., Shift+Cmd+F Video Filters, Shift+Cmd+G Audio Filters, Cmd+I Inspector, Ctrl+Cmd+L Log Viewer, Cmd+, Preferences, Cmd+M Minimize, Cmd+Q Quit, Cmd+H Hide, Opt+Cmd+H Hide Others, Cmd+? Help, Cmd+N New Window (hidden by default), Opt+Cmd+1 Normal Size (Retina, hidden). Source: `iina/Base.lproj/MainMenu.xib`.

Other bundled sets, for reference: `iina/config/vlc-default-input.conf` (55 bindings; VLC-like Cmd+f fullscreen, Cmd+=/- speed x1.5/x0.6667, Ctrl/Alt/Shift+Cmd+arrows for 3/10/60 s jumps), `movist-default-input.conf` (67), `movist-v2-default-input.conf` (63), `input.conf` = stock mpv defaults plus IINA panel hints (126 bindings).

## 9. Subtitles (selection, auto-matching, online, encoding, styling)

- **SUB-01** Primary + secondary subtitle tracks (mpv `sid`, `secondary-sid`), independent visibility toggles, independent delay/position/ASS-override level; menus and Quick Settings expose both. Source: `iina/PlayerCore.swift`, `iina/SidebarSubtitlesPane.swift`.
- **SUB-02** Track naming: `#id [language] title (codec, WxH, fps | Nch, kHz, Default)`; "No Title" fallback, `und` language omitted; localized language names via `Locale`. Used in menus, track lists and OSD. Source: `iina/MPVTrack.swift`.
- **SUB-03** External subtitle loading: File dialog (`sub-add`), drop onto window, "Load external subtitle..." menu, sidebar split button with a list of sibling files; re-loading an already-loaded file reloads it (`sub-reload`); unsupported file shows an alert (suppressed for auto-loaded). Supported extensions: `utf utf8 utf-8 idx sub srt smi rt ssa aqt jss js ass mks vtt sup scc lrc` (plus video containers when picking embedded subs). Source: `iina/PlayerCore.swift`, `iina/Utility.swift`.
- **SUB-04** Auto-load pipeline (`AutoFileMatcher`, runs on a background queue with a cancellation ticket whenever a local file starts): (1) list media files in the folder, natural-sort; (2) collect candidate subtitle files from the folder and from the user search-path list (`subAutoLoadSearchPath`, colon-separated, `~`, relative to the video, trailing `/*` = each immediate sub-directory); (3) group videos and subs into "series" by longest common filename prefix (`FileGroup`); (4) pair series by Levenshtein distance on the prefixes (needs a group of >2 files, distance < 0.6 x combined length, mutual closest); (5) inside matched series pair episodes by the episode-number part (numeric compare when both are ints); (6) optional "contains media filename" matching; (7) move subs whose names contain more of the priority strings (`subAutoLoadPriorityString`, comma list) than the least-matching candidate to the front; (8) "force match" (only when playlist auto-add is on): remaining unmatched videos vs subs by edit distance of prefix+suffix (skipped when videos x subs >= 10,000); matches are stored per video, then loaded and the first is selected. Modes: Disabled / mpvFuzzy ("contains media filename") / IINA ("intelligent"). mpv's own `sub-auto` is forced to `no`. Source: `iina/AutoFileMatcher.swift`, `iina/FileGroup.swift`, `iina/ObjcUtils.m` (`levDistance`).
- **SUB-05** Per-playlist-entry manual overrides: "Add Subtitle..." and "Wrong Subtitle" in the playlist context menu edit the matched-subs list for that file. Source: `iina/SidebarPlaylistPane.swift`.
- **SUB-06** Online subtitle search - provider abstraction (`OnlineSubtitle.Providers`): built-in OpenSubtitles.com (REST API with JWT login, hash+title search, language filter from `subLang`, download quota/rate-limit handling, "quota exceeded, try after HH:MM" OSD, logout on quit), assrt.net (API token, shared fallback token, search then detail fetch), shooter.cn (file-hash based, chunk MD5 of 4 offsets); plus **plugin-registered providers**: a plugin lists `subtitleProviders` in its `Info.json` and calls `iina.subtitle.registerProvider(id, {search(), download(sub), description(sub)})`; `search` resolves to items made by `iina.subtitle.item(data, desc)` (or the `CUSTOM_IMPLEMENTATION` marker if the plugin shows its own UI) and `download` resolves to file URLs. The built-in OpenSubtitles code is deprecated in favour of the official "Online Subtitles" plugin. Default source pref `onlineSubProvider`; menu "Find Online Subtitles from >" overrides per search. Source: `iina/OnlineSubtitle.swift`, `iina/OpenSubClient.swift`, `iina/OpenSubSubtitle.swift`, `iina/AssrtSubtitle.swift`, `iina/ShooterSubtitle.swift`, `iina/JavascriptPluginSubtitle.swift`.
- **SUB-07** Result chooser: if several results come back, a sheet "Please click to select subtitles to download" with a multi-select table and DOWNLOAD / CANCEL buttons is shown in the player window; double-click downloads. Downloads land in a temp folder, are auto-loaded and the OSD lists file names; "Save Downloaded Subtitle..." copies the selected downloaded track to a user-chosen place (replacing existing via Trash). Source: `iina/SubChooseViewController.swift`, `iina/MainMenuActions.swift`.
- **SUB-08** Auto search online: when enabled, on file start and after 0.5 s, if the file is local, has no subtitle tracks and is >= `autoSearchThreshold` minutes (20) long, IINA runs the default provider. Source: `iina/PlayerCore.swift` `autoSearchOnlineSub`.
- **SUB-09** Credentials: OpenSubtitles username in prefs, password in the Keychain (service `openSubAccount`); HTTP-auth credentials for stream URLs in the Keychain (service `httpAuth`, keyed by host/port). Source: `iina/KeychainAccess.swift`. [mac-only]
- **SUB-10** Preferred subtitle language list (`subLang` -> mpv `slang`) and audio language list (`audioLanguage` -> `alang`) edited with an ISO-639 token field with auto-complete from `ISO639.strings`. Source: `iina/LanguageTokenField.swift`, `iina/ISO639Helper.swift`.
- **SUB-11** Encoding: `defaultEncoding` (Auto-detect or one of 43 explicit encodings: UTF-8/16/16BE/16LE, ISO-8859-x, WINDOWS-125x, KOI8-R/U, SHIFT-JIS, ISO-2022-JP-2/KR/CN-EXT, EUC-KR, CP949, GBK, GB18030, EUC-TW, BIG5/BIG5-HKSCS, TIS-620, WINDOWS-874, VISCII, LATIN1..10 incl. LATIN-9), per-file override via Subtitles > Encoding menu (`sub-codepage`) which reloads all external subtitles. Source: `iina/CharEncoding.swift`, `iina/PlayerCore.swift` `setSubEncoding/reloadAllSubs`.
- **SUB-12** Styling model (applies to text subs, ASS only subject to override level): font, size, bold, italic, colour, border style (outline+shadow / opaque box / background box), border size+colour, shadow offset+colour (mapped to `sub-back-color`), blur, spacing, X/Y alignment, X/Y margins, vertical position, scale-with-window, draw in letterbox, per-track ASS override level (`no`, `yes`, `scale`, `force`, `strip`). Colours are stored as mpv `#AARRGGBB` strings (legacy NSColor archives migrated). Source: `iina/MPVController.swift`, `iina/Preference.swift`, `iina/LegacyMigration.swift`.
- **SUB-13** Font picker: custom window listing installed font families and typefaces with live preview text, filter-as-you-type box, and a "or enter the font name" field (accepts any name libass can resolve, default `sans-serif`). Source: `iina/FontPickerWindowController.swift`, `iina/FixedFontManager.m`.
- **SUB-14** Subtitle-scale math: user-facing symmetric scale (+-5, negative = shrink as 1/x) mapped to mpv `sub-scale` (0.1-10); menu steps +-10%; reset to 1. Source: `iina/MainMenuActions.swift`, `iina/Utility.swift`.
- **SUB-15** Subtitle seek (`sub-seek +-1`), delay/position adjusters with OSD bars (see OSD-02), toggle visibility.
- **SUB-16** Image subtitle awareness: Quick Settings hides text-style controls when only bitmap subs (PGS/VobSub) are active; ASS warning shown when the active track is ASS/SSA. Source: `iina/SidebarSubtitlesPane.swift`.
- **SUB-17** Music mode shows default artwork unless subtitles are visible (subtitle presence suppresses artwork placeholder). Source: `iina/MiniPlayerWindowController.swift` `refreshArtworkVisibility`.
- **SUB-18** `Subtitles` OSD for secondary track position/delay (secondary has its own OSD messages). Source: `iina/OSDMessage.swift`.

## 10. Video & audio filters

- **FLT-01** Filters window (one for video `vf`, one for audio `af`; autosaved frame): left table of the live mpv filter chain (editable filter strings, add/remove), right table of **saved filters** with name, filter string, optional shortcut, enabled checkbox that toggles the filter on/off in the live chain, edit and delete. Source: `iina/FilterWindowController.swift`, `iina/Base.lproj/FilterWindowController.xib`.
- **FLT-02** "Add filter" sheet with preset templates (video): Crop (w,h,x,y), Expand (w,h,x,y,aspect,round), Sharpen (matrix size 3-13 odd, amount 0-1.5; via `unsharp`), Blur (same, negative amount), Delogo (x,y,w,h), Negative (`lutrgb` negval), Flip (vflip), Mirror (hflip), 3D LUT (file path or drag a `.3dl/.cube/.dat/.m3d`, interpolation nearest/trilinear/tetrahedral), Custom mpv filter (name + value), Custom FFmpeg filter (wrapped as a `lavfi` filter). Audio: Custom mpv and Custom FFmpeg only. Source: `iina/FilterPresets.swift`, `iina/MPVFilter.swift`.
- **FLT-03** Filters IINA creates and labels so it can find them later (restored from watch-later data on reload): `iina_crop`, `iina_flip`, `iina_mirror`, `iina_aeq` (10-band EQ), `iina_delogo`, plus quick-drop LUT `iina_quickl3d`. Source: `iina/AppData.swift` `Constants.FilterName`, `iina/PlayerCore.swift` `reloadSavedIINAfilters`.
- **FLT-04** Menu toggles: Flip/Mirror (vertical/horizontal), Deinterlace (mpv flag, not a filter), Crop presets and custom interactive crop, Delogo via interactive box. Source: `iina/MainMenuActions.swift`.
- **FLT-05** Hardware-decode guard: adding a video filter while `hwdec=auto` shows an alert with choices "Turn off hardware decoding", "Switch to Auto (Copy)", "Abort", and updates the preference. Source: `iina/PlayerCore.swift` `addVideoFilter`.
- **FLT-06** Saved filters appear in menus with their shortcut and check state; filters can be shared via their raw mpv string; invalid filter strings produce an alert. Source: `iina/MenuController.swift`, `iina/MainMenuActions.swift`.
- **FLT-07** Video equalizer (brightness/contrast/saturation/gamma/hue -100..100) and audio 10-band EQ are in Quick Settings (QS-V-08, QS-A-04/05). EQ implementation = ffmpeg `equalizer` biquad per band with width = f/1.2247 (Q-style) and `g=` gain, as one `lavfi` graph.
- **FLT-08** Interactive crop/delogo UI (WIN-24) produces `crop=w:h:x:y` / `delogo=...` filters from the on-screen box, coordinates converted to video pixels.

## 11. Music mode (mini player)

`MiniPlayerWindowController` (+ `MiniPlayerWindow.swift`, `MiniPlaySlider.swift`, `ScrollingTextField.swift`). The video view is re-parented into the mini window; the playlist view is re-parented into its drawer. Source: `iina/MiniPlayerWindowController.swift`, `iina/PlayerCore.swift` `switchToMiniPlayer/switchBackFromMiniPlayer`.

- **MUS-01** Compact 300-pt-wide window: album art / video on top (square default art when no cover), title + "artist - album" marquee labels (scrolling when too long), seek bar with elapsed/remaining labels, transport row: previous, play/pause, next, mute+volume, playlist toggle, album-art toggle. Controls and the close/back buttons fade in on hover (title info fades out).
- **MUS-02** Back button returns to the normal player window; close button closes the mini player (and stops playback).
- **MUS-03** Volume popover: clicking the speaker button (or scrolling when scroll-action = volume) reveals a 100-pt slider with numeric label; auto-hides after the OSD timeout or at scroll end.
- **MUS-04** Playlist drawer (Playlist + Chapters tabs) toggles open to ~300 pt tall; dragging the window taller than the threshold opens it, shorter than 200 pt of extra height snaps shut; `musicModeShowPlaylist` (false) and `musicModeShowAlbumArt` (true) remember panel states.
- **MUS-05** Auto switching: `autoSwitchToMusicMode` (default true) enters music mode when the media is audio-only (no video track or only album-art tracks), leaves it for video; a manual toggle sets a per-player override until the pref changes; never auto-enters while in full screen. Source: `iina/PlayerCore.swift` `trackListChanged`.
- **MUS-06** Metadata: title/album/artist from mpv metadata with chapter-aware title/performer; used in window title, labels and Now Playing. Source: `iina/PlayerCore.swift` `getMusicMetadata`.
- **MUS-07** Always-on-top honoured in mini player; mouse actions and scroll volume work; sleep prevention relaxed for audio (`allowScreenSaverForAudio`).
- **MUS-08** OSD is replaced by labels (no OSD in music mode); playlist `Only in music mode` metadata display.
- **MUS-09** `--music-mode` CLI flag and plugin event `iina.music-mode.changed`; IINA command `toggle-music-mode`; toolbar button `musicMode`.
- **MUS-10** Handles album art aspect ratio (uses `demuxW/H` of the cover track) and resizes the window height when art changes; audio-only files show the generic album-art image.

## 12. Picture-in-Picture

- **PIP-01** Real macOS PiP via the (private-framework) `PIPViewController`: the same `VideoView` is moved into a floating system PiP window with play/pause/skip controls (skip uses relative seek), playback state/elapsed/duration pushed to the PiP UI, closing restores the video into the main window with animation. Source: `iina/MainWindowController.swift` (extension `PIPViewControllerDelegate`), `iina/VideoPIPViewController.swift`. [mac-only]
- **PIP-02** Entry points: toolbar button, menu item, binding `toggle-pip` (`Ctrl+Cmd+P`), mouse action "Toggle PiP", `iina://open?...&pip=1`, CLI `--pip`, plugin `core.window.pip`, and optionally minimizing the window (`togglePipByMinimizingWindow`, optionally video-only). Not available for music mode.
- **PIP-03** When PiP starts the main window can do nothing / hide / minimize (`windowBehaviorWhenPip`, default hide); `pauseWhenPip` can pause automatically; entering full screen leaves PiP; the OSC/title bar are never auto-hidden while in PiP (an overlay tells the main window that video is in PiP).
- **PIP-04** Plugin event `iina.pip.changed` and OSC toolbar button highlight state.

## 13. History, resume & recent files

- **HIS-01** Resume position: delegated to mpv's *watch-later* mechanism: `save-position-on-quit` + `resume-playback`, files stored in `<Application Support>/com.colliderli.iina/watch_later/` named by the MD5 of the full path (or only the file name when mpv's `ignore-path-in-watch-later-config` is on). IINA calls `write-watch-later-config` on stop/quit/window close, so per-file settings mpv saves (position, track ids, volume, speed, filters, A-B loop...) all resume. Toggle `resumeLastPosition` (default true). Source: `iina/PlayerCore.swift` `savePlaybackPosition`, `iina/Utility.swift` (`mpvWatchLaterMd5`, `playbackProgressFromWatchLater`), `iina/MPVController.swift`.
- **HIS-02** Playback History window (`HistoryWindowController`, toolbar-style window, frame autosaved): outline view grouped by **date** (last played) or by **folder/website**, columns Media (icon, name, title/path hint), Progress (level bar from watch-later position/duration), Played-at time; search field with scope menu (Filename / Full path) and recent searches; toolbar: group-by segmented control, expand/collapse all, clear history. Keys: Return = play, Cmd+Return = play in new window, Cmd+L = show in Finder, Delete = remove (confirm), Cmd+F search, Cmd+A select all. Context menu: Play, Play in New Window, Show in Finder, Delete..., Move to Trash.... Source: `iina/HistoryWindowController.swift`.
- **HIS-03** History store (`HistoryController`): `history.plist` (NSKeyedArchiver of `PlaybackHistory` {url, name, mpv MD5, played flag, added date, duration, optional media title}), de-duplicated by MD5 (re-playing moves to top), written atomically on a background queue, quit waits for pending writes; toggle `recordPlaybackHistory`. Progress comes from the watch-later file at read time. Source: `iina/HistoryController.swift`, `iina/PlaybackHistory.swift`.
- **HIS-04** Open Recent: system recent-documents list (macOS keeps it), `recordRecentFiles` master toggle, `trackAllFilesInRecentOpenMenu` (add every loaded file vs only manually opened), Clear Menu, optional prefs-backed backup/restore workaround for unsigned builds. Source: `iina/AppDelegate.swift`.
- **HIS-05** Welcome window "Resume" button: shows last played file name + position (`iinaLastPlayedFilePath`/`Position`) when both `recordRecentFiles` and `resumeLastPosition` are on and the file still exists; recent list excludes that file. Source: `iina/InitialWindowController.swift`.
- **HIS-06** Clear actions in Utilities: saved playback progress (delete watch-later folder), playback history (+ recents + last-played), thumbnail cache. Source: `iina/SettingsPageUtilities.swift`.
- **HIS-07** On-disk "last opened date" attribute set on every local file played (a macOS Finder extended attribute so Finder's "Date Last Opened" updates). Source: `iina/PlayerCore.swift` `fileStarted`. [mac-only]
- **HIS-08** Developer helper script `other/cat_watch_later_file.sh` shows the watch-later file for a media path. Source: `other/cat_watch_later_file.sh`.
- **HIS-09** Plugins can read history and recent docs via `iina.core.getHistory()` (name, url, date, progress, duration) and `getRecentDocuments()`.

## 14. Inspector

Non-modal floating panel (`NSPanel`, hides when the app is inactive; Cmd+I) with a segmented tab switcher; refreshes on a timer while open and when a file loads; selectable fields; links for file path and metadata comment URLs. Source: `iina/InspectorWindowController.swift` + `iina/Base.lproj/InspectorWindowController.xib`.

- **INS-01** General tab: file path (link, reveals/opens), title, duration, file format, file size, chapter count, editions count, metadata comment.
- **INS-02** Tracks tab: video (format/codec, decoder description, size, fps, driver `vo`, estimated pixel format, colour primaries / transfer + "HDR/SDR" label, colourspace), audio (format/codec, channels, sample rate, bit rate, driver `ao`), subtitle (selected / default / forced / external flags), with a track selector popup per kind.
- **INS-03** Status tab (dynamic): HW decoder in use, A/V sync diff and total A/V sync change, dropped frames, mistimed frames, estimated vf FPS / display FPS / estimated display FPS, video & audio bitrate.
- **INS-04** Watch tab: user-defined list of **arbitrary mpv property names** with live values (add/remove rows, stored in `watchProperties`, errors shown inline); the escape hatch for debugging any mpv property.
- **INS-05** Timers/listeners stop when the panel closes; panel frame autosaved (`IINAInspectorPanel`).

## 15. Screenshots

- **SHOT-01** Capture through mpv's `screenshot` command (async with completion callback) with flags `subtitles` or `video` (from `screenshotIncludeSubtitle`), or the `window` flag if a key binding says so; `each-frame` and unknown flags fall back to raw mpv. Both "save to file" and "copy to clipboard" off => screenshot is ignored. Source: `iina/PlayerCore.swift` `screenshot`.
- **SHOT-02** Output: directory `screenshotFolder` (default `~/Pictures/Screenshots`, or an internal cache dir if not saving to file, deleted after clipboard copy/preview), format png/jpg/jpeg/webp/jxl/avif, filename template (mpv `screenshot-template`, default `%F-%n`, no UI). Latest file in the folder is located afterwards to build the preview/clipboard image (FFmpeg fallback decoder for formats the OS can't decode). Source: `iina/MPVController.swift`, `iina/Utility.swift` `getLatestScreenshot`.
- **SHOT-03** Preview card on the OSD (REVEAL / EDIT / DELETE) for 5 s (`screenshotShowPreview`) - see OSD-07. Menu: "Go to Screenshot Folder".
- **SHOT-04** HDR-aware: when EDR is active mpv tags the colourspace on screenshots; jxl uses software screenshot (`screenshot-sw`) so HDR data is kept. Source: `iina/VideoView.swift`, `iina/MPVController.swift`.
- **SHOT-05** Thumbnail previews on the seek bar: IINA generates **100 evenly spaced thumbnails** per local file with FFmpeg (libavformat/libswscale; width = `thumbnailWidth` x2 for retina), progressively as they are produced, then caches them on disk keyed by the watch-later MD5 plus file size + mtime, with a cap (`maxThumbnailPreviewCacheSize`, 500 MB) and "clear cache" action; disabled for streams and (by default) for files on mounted network volumes. Source: `iina/FFmpegController.m`, `iina/ThumbnailCache.swift`, `iina/CacheManager.swift`, `iina/PlayerCore.swift` `generateThumbnails`.
- **SHOT-06** Video frame capture for Live Text uses the render layer's snapshot (separate from the screenshot command). Source: `iina/ViewLayer.swift` `captureSnapshot`.
- **SHOT-07** Duration/metadata probing helper `probeVideoInfo` (FFmpeg) used for playlist durations and for artist/title/album metadata in the playlist. Source: `iina/FFmpegController.m`.

## 16. Network, streaming & yt-dlp

- **NET-01** Open URL window: a single field (adds `http://` automatically and shows the prefix hint; validates), optional HTTP Basic auth username/password (+ "remember in Keychain per host"), loading overlay with "Stop Loading", error state, "alternative" (new window) variant. Accepts any scheme mpv/ffmpeg can open (http(s), rtsp, rtmp, hls, ftp, smb via system mounts...). Source: `iina/OpenURLWindowController.swift`.
- **NET-02** Stream detection: non-file URLs and `.m3u*` are "network resources": window closes/reopens through the loading screen, progress/duration updated live, thumbnails and auto-subtitle search are skipped, UI timer stays on for buffering display. Source: `iina/PlayerCore.swift`.
- **NET-03** Buffering UI: "Opening stream...", "Buffering N%", "Seeking...", cache used + input rate (see OSC-13); seek bar draws demuxer cache time. Source: `iina/BufferIndicatorView.swift`, `iina/PlaySliderCell.swift`.
- **NET-04** Cache & network knobs (Settings > Network): cache on/off, pause-before-play, resume threshold, prefetch seconds, max bytes, user agent, HTTP proxy, RTSP transport. See PREF-NET-*.
- **NET-05** Online video via mpv's `ytdl_hook` using yt-dlp/youtube-dl found on `PATH`: IINA prepends the directory of its own executable (where a bundled yt-dlp/youtube-dl would sit) and the user-configured folder (`ytdlSearchPath`) to `PATH` before starting mpv (the plugin `utils.exec` additionally looks in `<Application Support>/.../bin`); `ytdlRawOptions` forwarded; `ytdlEnabled` toggle; whole built-in path is switched off if the **Online Media plugin** (`io.iina.ytdl`) is enabled, which adds quality switching and downloading. Source: `iina/PlayerCore.swift` `startMPV`, `iina/MPVController.swift`, `iina/JavascriptPlugin.swift` (`hasYTDL`).
- **NET-06** HTTP proxy is passed through the `http_proxy` env var; requires restart. Source: `iina/PlayerCore.swift`.
- **NET-07** Browser integration: Safari Web Extension "Open In IINA" (context-menu on links/pages), Chrome & Firefox WebExtensions in `browser/` (context menu "Open this link/video/audio in IINA", toolbar action with options: open mode = normal/fullscreen/PiP/enqueue, new window; builds an `iina://open?url=...` URL with `new_window`, `full_screen`, `pip`, `enqueue` params). Source: `OpenInIINA/`, `browser/Chrome_Open_In_IINA/`, `browser/Firefox_Open_In_IINA/`. [mac-ish]
- **NET-08** `weblink` host in the URL scheme is an alias of `open` for browser extensions. Source: `iina/AppDelegate.swift`.
- **NET-09** Streams from stdin (`-` / `--stdin`) are supported via CLI. See section 17.
- **NET-10** Network resources get "Open in Browser" and "Copy URL(s)" in the playlist context menu; playlist rows show URL or media title.

## 17. CLI, URL scheme, file associations

### 17.1 `iina-cli` (separate small executable inside the app bundle, `iina-cli/main.swift`)

- **CLI-01** Usage: `iina-cli [arguments] [files] [-- mpv_option [...]]`. Resolves relative paths against the current directory, passes URLs through, launches the main binary as a child and exits immediately (unless `--keep-running`/stdin).
- **CLI-02** `--mpv-*` options: any mpv option as `--mpv-volume=20 --mpv-resume-playback=no` (a bare `--mpv-flag` means `yes`; options starting with `--no-` are not supported in this form; special-case `--mpv-shuffle` installs a one-shot `on_before_start_file` hook that shuffles the playlist). After a literal `--`, plain mpv syntax is accepted (`--volume=20 --no-resume-playback` is rewritten to the `--mpv-` form).
- **CLI-03** `--separate-windows` / `-w`: one window per file; default groups files in one playlist.
- **CLI-04** `--stdin` / `--no-stdin` / `-`: play from standard input; auto-detected when data is piped (fragile, hence explicit flags recommended); output is wired through only in stdin mode.
- **CLI-05** `--keep-running`: stay attached until IINA exits (signals forwarded).
- **CLI-06** `--music-mode` and `--pip` (mutually exclusive; exit status EX_USAGE on conflict) applied after opening.
- **CLI-07** `--help` / `-h`.
- **CLI-08** Direct launch with CLI args also works (the app parses `--` args and prints a version line), but is unsupported.
- **CLI-09** Second helper executable `iina-plugin` (`iina-plugin/main.swift`): `new <name> [--url=template_zip]` (scaffold from the iina-plugin-template repo), `pack <dir>` (zip to `.iinaplgz`), `link <path>` / `unlink <path>` (symlink a dev folder into IINA's plugins directory so it loads as a DEV plugin).

### 17.2 `iina://` URL scheme (`iina/AppDelegate.swift` `parsePendingURL`)

- **CLI-10** `iina://open?url=<url-or-path>` (host `open` or `weblink`). Params: `url` (required), `new_window=1` (default is the active window), `enqueue=1` (append to the last active player's playlist if it has one), `full_screen=1`, `pip=1`, and `mpv_<option>=<value>` for any option on a **whitelist** (`AppData.safeMPVOptions`, 209 options covering track selection, start/end/speed/loop, A-B loop, video geometry/colour, audio, subtitles, window flags, network headers, cache; excludes anything that could read/write local files). Non-`iina` schemes (plain http etc.) are opened directly.
- **CLI-11** Plugins and the browser extensions are the primary producers of these URLs.

### 17.3 File associations, document types, drag to Dock

- **CLI-12** Registered document types (27 `CFBundleDocumentTypes`, 38 imported UTIs): Matroska (mkv mka mk3d mks), RealMedia (rm rmvb ra), ASF/WMV/WMA, AAC, Flash video (flv f4v f4p f4a f4b), GIF, WebM, 3GPP/3GPP2, MP3, Ogg audio/video (ogg oga ogm ogv), MPEG-TS (ts mts m2ts), AVI, WAV, M4A/M4B, QuickTime (qt mov), FLAC, MPEG (mpeg mpg), MP4/M4V, DivX/VOB/AMV/MXF/MCF/SWF/XVID/YUV/DV/WV (+ catch-all `*`), audio misc (aa3 ac3 acm aif aiff ape caf pcm vox tta tak opus cue), playlists (m3u8 m3u pls), folders, and IINA plugin types `.iinaplugin` (dev folder) / `.iinaplgz` (zip package). Also DSD (dsf dff), XM, Windows Recorded TV (wtv) etc. via imported UTIs. Source: `iina/Info.plist`. [mac-only mechanism; extension list is the portable asset]
- **CLI-13** Internal "playable" extension list: video `mkv mp4 avi m4v mov 3gp ts mts m2ts wmv flv f4v asf webm rm rmvb qt dv mpg mpeg mxf vob gif ogv ogm`, audio `mp3 aac mka dts flac ogg oga mogg m4a ac3 opus wav wv aiff aif ape tta tak`, subs listed in SUB-03; playlists `m3u m3u8 pls cue`; 3D LUT `3dl cube dat m3d`. Playlist/subtitle files are excluded from "playable" scans. Folder drops scan recursively (skipping hidden files) and sort by folder then natural file name. Source: `iina/Utility.swift`, `iina/DraggingDetect.swift`.
- **CLI-14** Opening files from Finder: repeated `openFile` events within 0.2 s are batched into one open; a `.iinaplgz` among them installs the plugin instead; "nothing to open" alert if no playable files. Source: `iina/AppDelegate.swift`.
- **CLI-15** "Set IINA as default" per media class (see PREF-UTL-01).

## 18. Plugins (JavaScript API)

Architecture: each plugin is a folder/zip with an `Info.json` manifest and JavaScript entry files. IINA creates **one JavaScriptCore context per (plugin, player)** plus optionally **one global context per plugin** (`globalEntry`). UI surfaces are WKWebViews (overlay, sidebar tab, standalone windows, preferences/help pages) wired to JS by a message hub. Source: `iina/JavascriptPlugin.swift`, `iina/JavascriptPluginInstance.swift`, `iina/JavascriptAPI*.swift`, `iina/JavascriptMessageHub.swift`, `iina/PluginManager.swift`, `iina/PluginViewController.swift`, `iina/PluginSidebarView.swift`, `iina/PluginOverlayView.swift`, `iina/PluginStandaloneWindow.swift`, `iina/PluginStorePanel.swift`, `iina/JavascriptDevTool.swift`, `iina/JavascriptPolyfill.swift`, `iina/EventController.swift`. [mac-only runtime: JavaScriptCore + WKWebView + Network.framework; the *API surface* is portable]

### 18.1 Package, manifest, lifecycle

- **PLG-01** Package formats: `.iinaplugin` directory (development/"external" plugins, shown with a DEV badge, can be symlinked via `iina-plugin link`), `.iinaplgz` zip archive (installed copy). Both are registered document types. Plugins live in `<Application Support>/com.colliderli.iina/plugins/`; a per-plugin private `data` dir, `tmp` dir and `.preferences` plist.
- **PLG-02** `Info.json` keys: `name`, `author{name,url,email}`, `identifier` (reverse-DNS), `version`, `entry` (required), `globalEntry`, `description`, `preferencesPage` (HTML), `helpPage` (HTML or URL), `sidebarTab{name}`, `subtitleProviders[{id,name}]`, `permissions[]`, `allowedDomains[]` (`*` = all), `preferenceDefaults{}`, `ghRepo` + `ghVersion` (enables GitHub update checks), `localized{<lang>:{name,description,sidebarTab:{name}}}`.
- **PLG-03** Permissions (confirmed by the user in a sheet when installing/updating; "dangerous" ones flagged): `network-request` (lists allowed domains), `show-osd`, `show-alert` (declared in the vocabulary but not enforced by any current API), `video-overlay`, `file-system`. Calls without the permission throw a JS error naming the missing permission (or silently no-op / return null for some `utils` calls).
- **PLG-04** Globals/polyfills: `setTimeout/setInterval/clearTimeout/clearInterval` (main-thread timers), CommonJS-style `require(relativePath)` / `module.exports` restricted to the plugin's own folder, `Promise` (native). No DOM, no `fetch`, no `XMLHttpRequest`.
- **PLG-05** Instances: per-player instance runs `entry` when the player's mpv starts (`iina.mpv-initialized`), global instance runs once at app launch and can create hidden/managed players (`iina.global.createPlayerInstance`). Plugins can be enabled/disabled live and reloaded (menu Plugin > Reload All Plugins; events re-emitted best-effort). Plugin load order is user-sortable.
- **PLG-06** Default plugins bundled in the app and installed on first run: official "Online Media" (`io.iina.ytdl`), "Userscript" (`io.iina.user-script`), "Online Subtitles" (OpenSubtitles, `io.iina.opensub`). Source: `iina/AppDelegate.swift`, `iina/PluginStorePanel.swift`.
- **PLG-07** Plugin management UI: store sheet (official defaults + community catalog fetched from `plugins.json` on the GitHub `develop` branch: 34 entries today, e.g. AirPlay, AniList Sync, Bookmarks, Danmaku, Jellyfin, Plex Sync, Trakt Scrobbler, Playlist Pro, Skip Intro, Thumbnails, WebDAV...), "install from GitHub" (URL or `user/repo`, downloads the latest release's `.iinaplgz` asset via the GitHub API), install from local package, per-plugin permission prompt, update check (compares `ghVersion` against the repo's `Info.json`), update with re-prompt when permissions change, uninstall confirm, reveal in Finder. Source: `iina/PluginManager.swift`, `iina/PluginStorePanel.swift`, `iina/SettingsPagePlugin.swift`.
- **PLG-08** Developer tooling: Plugin > Developer Tool > per-instance JS console window (REPL with history, prompts, results as tree, logs with levels, exceptions with stack), `iina.console.log/warn/error` routed to the Log Viewer subsystem for that plugin. Source: `iina/JavascriptDevTool.swift`.
- **PLG-09** Plugin menu & playlist context menu & key shortcuts: plugin menu items (title, action, selected, enabled, keyBinding, sub-items, separators) appear under the Plugin menu (max 5 inline per plugin, rest under "More..."), shortcuts that collide with key bindings are flagged. Playlist rows can get plugin items through `playlist.registerMenuItemBuilder`.

### 18.2 `iina.*` API modules and main functions

(Names are the JS-visible names. "needs X" = permission gate.)

- **PLG-API-01 `iina.core`** (per-player): `open(url)`, `osd(msg)` (needs `show-osd`), `pause()`, `resume()`, `stop()`, `seek(seconds, exact)`, `seekTo(seconds)`, `setSpeed(x)`, `getChapters()`, `playChapter(index)`, `setUIVisibility(bool)`, `getHistory()`, `getRecentDocuments()`, `getVersion()` (`{iina, build, mpv}`). Sub-objects with read/write properties: `core.window` (`loaded`, `frame`, `fullscreen`, `pip`, `ontop`, `visible`, `sidebar` (tab name or null), `screens`, `miniaturized`), `core.status` (`paused`, `idle`, `position`, `duration`, `speed`, `videoWidth`, `videoHeight`, `isNetworkResource`, `url`, `title`), `core.audio` (`id`, `delay`, `tracks`, `currentTrack`, `volume`, `muted`, `loadTrack(url)`), `core.subtitle` (`id`, `secondID`, `delay`, `tracks`, `currentTrack`, `loadTrack(url)`), `core.video` (`id`, `tracks`, `currentTrack`, `loadTrack(url)`). Track objects: `id, title, formattedTitle, lang, codec, isDefault, isForced, isSelected, isExternal, demuxW/H, demuxChannelCount, demuxChannels, demuxSamplerate, demuxFPS`.
- **PLG-API-02 `iina.mpv`**: `getFlag(name)`, `getNumber(name)`, `getString(name)`, `getNative(name)` (arbitrary mpv node), `set(name, value)`, `command(name, args[])`, `addHook(name, priority, cb)` (mpv hooks like `on_load`). Without `file-system` permission `set` is limited to `AppData.safeMPVOptions` (209 options) and `command` to `safeMPVCommands` (38 commands: seek, playlist-*, sub/audio/video add-remove-reload, show-text, screenshot-raw, osd-overlay, ...).
- **PLG-API-03 `iina.event`**: `on(name, cb) -> id`, `off(name, id)`. Names: any `mpv.<event>` (e.g. `mpv.file-loaded`, `mpv.end-file`, `mpv.seek`...), property observers `mpv.<property>.changed`, and IINA events `iina.window-loaded`, `iina.window-size-adjusted`, `iina.window-moved`, `iina.window-resized`, `iina.window-fs.changed`, `iina.window-screen.changed`, `iina.window-miniaturized`, `iina.window-deminiaturized`, `iina.window-main.changed`, `iina.window-will-close`, `iina.music-mode.changed`, `iina.pip.changed`, `iina.file-loaded`, `iina.file-started`, `iina.mpv-initialized`, `iina.thumbnails-ready`, `iina.plugin-overlay-loaded`, `iina.menu-update`.
- **PLG-API-04 `iina.menu`**: `item(title, action, {selected, enabled, keyBinding})`, `separator()`, `addItem(item)`, `items()`, `removeAt(i)`, `removeAllItems()`, `forceUpdate()`; item members `title`, `selected`, `enabled`, `keyBinding`, `addSubMenuItem(item)`.
- **PLG-API-05 `iina.playlist`**: `list()`, `count()`, `add(urlOrArray, at)`, `remove(indexOrArray)`, `move(index, to)`, `play(index)`, `playNext()`, `playPrevious()`, `registerMenuItemBuilder(fn(rowIndexes) -> items)`.
- **PLG-API-06 `iina.overlay`** (needs `video-overlay`): full-window transparent web view above the video: `show()`, `hide()`, `setOpacity(f)`, `loadFile(path)`, `simpleMode()` (built-in empty page), `setStyle(css)`, `setContent(html)`, `setClickable(bool)`, `postMessage(name, data)`, `onMessage(name, cb)`.
- **PLG-API-07 `iina.sidebar`**: the plugin's own sidebar tab (title from `sidebarTab.name`): `loadFile(path)`, `show()`, `hide()`, `postMessage(name, data)`, `onMessage(name, cb)`.
- **PLG-API-08 `iina.standaloneWindow`**: `open()`, `close()`, `isOpen()`, `loadFile(path)`, `simpleMode()`, `setStyle(css)`, `setContent(html)`, `postMessage`, `onMessage`, `setProperty({title, resizable, fullSizeContentView, hideTitleBar...})`, `setFrame(w, h, x, y)`.
- **PLG-API-09 `iina.subtitle`**: `item(data, desc)`, `registerProvider(id, {search, download, description})`, constant `CUSTOM_IMPLEMENTATION`.
- **PLG-API-10 `iina.input`**: `onKeyDown/onKeyUp(key, cb, priority)`, `onMouseDown/onMouseUp/onMouseDrag(button, cb, priority)` where the pseudo-key constants are `iina.input.MOUSE`, `RIGHT_MOUSE`, `OTHER_MOUSE`; priority constants `PRIORITY_LOW=100`, `PRIORITY_HIGH=200`; `normalizeKeyCode(code)`, `getAllKeyBindings()`. High-priority handlers run before the built-in handler and may consume the event.
- **PLG-API-11 `iina.preferences`**: `get(key)`, `set(key, value)`, `sync()`; defaults from `preferenceDefaults`; the preferences page (HTML in the plugin settings sheet) talks to it through a bridge that supports typed inputs (`int`, `float`, ...).
- **PLG-API-12 `iina.file`**: `list(dir, {includeSubDir})`, `exists`, `read(path, {encoding})`, `write(path, text)`, `trash`, `delete`, `move`, `showInFinder`, `handle(path, "read"|"write")` -> `{offset, seekTo, seekToEnd, read(n), readToEnd, write(data), close}`. Magic prefixes: `@tmp/`, `@data/` (plugin-private, always allowed), `@video/<id>`, `@audio/<id>`, `@sub/<id>` (path of a track's external file), `@current/` (folder of the current file), `~/`; anything else needs `file-system`.
- **PLG-API-13 `iina.http`** (needs `network-request`, restricted to `allowedDomains`): `get/post/put/patch/delete(url, {params, headers, data, method})` -> Promise of `{statusCode, reason, data (parsed JSON), text}`, `download(url, dest, opts)`, `xmlrpc(location).call(method, args)`.
- **PLG-API-14 `iina.utils`**: `fileInPath(name)`, `resolvePath(path)`, `exec(file, args[], cwd, stdoutHook, stderrHook)` (Promise; `fileInPath`, `resolvePath` and `exec` need `file-system`; looks for plugin-owned binaries first and will `chmod +x` them; error codes `ERROR_BINARY_NOT_FOUND=-1`, `ERROR_RUNTIME=-2`), `ask(title)`, `prompt(title)`, `chooseFile(title, {chooseDir, allowedFileTypes})`, `keychainWrite(service, name, password)`, `keychainRead(service, name)`, `open(urlOrPath)`, `preferredLocalizations()`.
- **PLG-API-15 `iina.console`**: `log`, `warn`, `error`.
- **PLG-API-16 `iina.global`** (global instance): `createPlayerInstance({url, disableWindowAnimation, disableUI, label, enablePlugins})`, `postMessage(targetOrLabel, name, data)`, `onMessage(name, cb)`; child (per-player) instance: `getLabel()`, `postMessage(name, data)`, `onMessage(name, cb)`.
- **PLG-API-17 `iina.ws`**: embedded WebSocket server (Network.framework): `createServer({port})`, `startServer()`, `onStateUpdate(cb)`, `onNewConnection(cb)`, `onConnectionStateUpdate(cb)`, `onMessage(cb)` (text/binary), `sendText(connId, text)`.
- **PLG-API-18 Web-view side**: pages loaded into overlay/sidebar/window/preferences get an `iina` bridge object with `postMessage(name, data)` / `onMessage(name, cb)` plus preference helpers; navigation to external links opens the system browser.

## 19. Platform integration (macOS-specific services)

- **PLAT-01** Now Playing (Control Center / lock-screen / media widgets): publishes title (media title, or title/album/artist for audio), media type (audio/video), asset URL, playback-queue count + index, chapter count + number, duration, elapsed, rate, default rate, playback state (stopped/playing/paused); kept in sync on pause/speed/seek/file change. Artwork preference order: front cover (embedded cover read with FFmpeg, or external cover image decoded with a memory-light thumbnailer) > QuickLook-generated file thumbnail (3 retries) > a frame-grab from the OSC thumbnails (taken at 60 s); target 768 px; internal kill-switch `enableNowPlayingArtwork`. Session is released when no media is open. Source: `iina/NowPlayingInfoManager.swift`, `iina/PlayerState.swift`. [mac-only; Linux=MPRIS, Windows=SMTC]
- **PLAT-02** Media keys / remote commands via `MPRemoteCommandCenter` - see INP-17. Toggled by `useMediaKeys` ("Use system media control"). [mac-only]
- **PLAT-03** Touch Bar: default strip = play/pause + elapsed time + scrub slider + remaining/duration; user-customisable items (Window > Customize Touch Bar...): play/pause, slider, volume up/down, rewind/fast-forward (speed), time, remaining-or-duration, +-15 s, +-30 s, previous/next media, toggle PiP, fixed spacer. The slider shows a thumbnail while touching (uses the same 100 preview frames) and resets cache when thumbnails regenerate; during full screen the Esc key slot becomes "exit full screen". The UI update timer only runs if the Touch Bar is actually showing app controls (reads the system Touch Bar presentation-mode prefs). Elapsed/remaining toggle persisted in `touchbarShowRemainingTime`. Source: `iina/TouchBarSupport.swift`, `iina/TouchBarSettings.swift`. [mac-only]
- **PLAT-04** Auto-update via Sparkle: stable feed `https://www.iina.io/appcast.xml`, beta feed `.../appcast-beta.xml` (chosen by `receiveBetaUpdate`), EdDSA + legacy DSA public keys embedded, check interval pref, "Check for Updates..." menu item, welcome window shows a BETA pill with "found a bug / want to rollback?" popover. Source: `iina/AppDelegate.swift`, `iina/Info.plist`, `iina/InitialWindowController.swift`. [mac-only]
- **PLAT-05** Logging subsystem: levels verbose/debug/warning/error; subsystems per component and per player index (`player0`, `mpv0`, `window0`, `fmatcher0`, `onlinesub`, `history`, `nowplaying`, `settings`, `logger`, plugin names...); when enabled (advanced setting) writes `iina.log` and mpv's own `mpv.log` into a per-session folder under `~/Library/Logs/com.colliderli.iina/`; at startup logs IINA/mpv/FFmpeg/libass versions, build date/Xcode/SDK/branch/commit, OS version, Mac model, CPU, RAM, thermal state, low-power mode, every screen and display (native modes, refresh), and which settings differ from their defaults (sensitive ones such as tokens and usernames are never logged). Source: `iina/Logger.swift`, `iina/Preference.swift` `logSettings`, `iina/AppDelegate.swift`, `iina/Display*.swift`, `iina/MemoryUsage.swift`.
- **PLAT-06** Log Viewer window (Window > Log Viewer): live table (level icon, time, subsystem, message), toolbar with "Now" (follow latest), log-level filter menu, subsystem multi-filter with badge count, search field, Save button; copy rows via context menu; frame autosaved. Source: `iina/LogWindowController.swift`.
- **PLAT-07** Sleep / screensaver prevention while any player is *playing* (`preventScreenSaver`): uses a Foundation activity assertion; audio-only / mini-player playback can allow display sleep but still block system sleep (`allowScreenSaverForAudio`). Source: `iina/SleepPreventer.swift`. [mac-ish]
- **PLAT-08** Hardware-decode capability probing: at launch a background task asks VideoToolbox which of AV1 / ProRes / VP9 can be hardware decoded on this Mac and removes the others from mpv's `hwdec-codecs` allow-list (unless the user set that option); on Intel Macs VP9 hwdec is removed entirely (workaround for a VideoToolbox hang). Source: `iina/HardwareDecodeCapabilities.swift`, `iina/MPVController.swift`. [mac-only]
- **PLAT-09** Rendering pipeline: libmpv render API on an OpenGL `CAOpenGLLayer` (`ViewLayer`) driven by a CVDisplayLink; dedicated draw thread with a recursive lock, snapshot capture for Live Text, forced redraw rules for paused/resizing/occluded states, optional dedicated-GPU pixel format on Intel, display-link stop when idle, re-created on screen change. Source: `iina/VideoView.swift`, `iina/ViewLayer.swift`, `iina/MPVController.swift` (`mpvInitRendering`). [mac-only] (Patina will use libmpv's render API with its own GL/Vulkan surface)
- **PLAT-10** HDR/EDR pipeline: video is "HDR" if transfer is `pq` or `hlg` and primaries are `bt.2020` or `display-p3`; if the screen supports extended dynamic range and per-file `hdrEnabled` is on, the layer is switched to an EDR PQ colourspace and mpv is told `target-prim` = source primaries and `target-trc=pq`; otherwise mpv tone-maps using user settings (`target-peak` = detected display peak luminance read from CoreDisplay info (`NonReferencePeakHDRLuminance` on Apple Silicon, `DisplayBacklight` on Intel) unless overridden, `tone-mapping` algorithm, `tone-mapping-param`); SDR path sets `icc-profile` from the display's colour space when `loadIccProfile`. Re-evaluated when the window changes screen/colour space or relevant prefs change; HDR availability drives the Quick Settings HDR switch. Source: `iina/VideoView.swift` (HDR extension), `iina/DisplayController.swift`. [mac-only]
- **PLAT-11** Live Text: when enabled and playback is paused, captures the current frame and runs VisionKit `ImageAnalyzer` to overlay selectable/copyable text and data detectors; cleared on resume/seek/track change; hides the OSC while an overlay selection is active; toggle via OSC toolbar button, `live-text` binding or Video menu; requires macOS 13+ and system Live Text enabled. Source: `iina/LiveTextSupport.swift`. [mac-only]
- **PLAT-12** Keychain storage of credentials (OpenSubtitles password, HTTP-auth user/password per host+port, plugin `utils.keychainRead/Write`). Source: `iina/KeychainAccess.swift`. [mac-only]
- **PLAT-13** Finder/Trash/NSWorkspace integration: reveal file, move to Trash, open URL in default browser, proxy icon, file icons in menus and history, default-app registration for ~100 extensions via UTIs, "last opened" metadata. [mac-only]
- **PLAT-14** App lifecycle: graceful shutdown (disable menus, log out of subtitle provider with a timeout, close windows, stop then quit every mpv core, wait for history writes, hard 10 s timeout), reopen handling, state restoration opt-in (secure coding), `NSFullScreenMenuItemEverywhere` disabled, window tabbing disabled. Source: `iina/AppDelegate.swift`.
- **PLAT-15** First-run & release notes: guide window with web view of `https://iina.io/highlights/<version>/` on the first launch of each release/beta build (and Help > Release Highlights), default plugin install on first run (see PLG-06). Source: `iina/GuideWindowController.swift`, `iina/FirstRunManager.swift`.
- **PLAT-16** About window: version/build, credits tab (contributors from the GitHub contributors page, translators, Crowdin attribution), license tab, links. Source: `iina/AboutWindowController.swift`, `iina/Credits.rtf`, `iina/Contribution.rtf`.
- **PLAT-17** Display awareness: re-validates window/full-screen/black-out layers on `didChangeScreenParameters` (filtered to real changes), logs display details (vendor/model/modes/XDR luminance); picks the screen to open on from the last saved frame. Source: `iina/MainWindowController.swift`, `iina/Display.swift`.
- **PLAT-18** Energy-efficiency design: UI sync timer (0.1 s, or 0.04 s at >=10 ms time precision) only runs when something visible needs it (OSC/OSD shown, network stream, mini player, Touch Bar); everything idles when paused. Source: `iina/PlayerCore.swift` `refreshSyncUITimer`.
- **PLAT-19** Required entitlements (hardened runtime): allow unsigned executable memory + disable library validation (for bundled dylibs/JS). Source: `iina/IINA.entitlements`.
- **PLAT-20** Bundled native libraries: libmpv, FFmpeg (libavcodec/format/util/swscale, built with SVT-AV1 for AVIF screenshots), libass, Mustache templating (swift package), Sparkle, PromiseKit, Just (HTTP), Swift-based XML-RPC. Source: `deps/`, `iina.xcodeproj`.

## 20. Accessibility & localization

- **A11Y-01** Respects macOS *Reduce Motion*: animation durations scaled to 0 or sidebars switch from slide to fade; independent in-app *Disable animations* switch (`disableAnimations`, with Apple HIG help link) zeroes every UI animation. Source: `iina/AccessibilityPreferences.swift`. [mac-ish]
- **A11Y-02** Right-to-left support: timeline, play slider, OSD position text and OSC play-cluster forced left-to-right; playlist "current" triangle flips; sidebar and resize-handle logic mirror; window title-bar layout direction override. Source: `iina/MainWindowController.swift`, `iina/CommonWindow.swift`, `iina/OSDMessage.swift`.
- **A11Y-03** Localization: 54 locales + Base (af ar az be bg bn ca cs da de el en en-GB es eu fa fi fr he hi hr hu hy id is it ja kn ko lv mk ml ne nl no pl pt pt-BR ro ru sk sl sr sr-Latn sv ta th tr ug uk ur vi zh-Hans zh-Hant) managed through Crowdin; strings split across per-window `.strings` (generated from XIBs) and `Localizable.strings` (917 keys in English: `settings.*` 416, `alert.*` 106, `osd.*` 61, `sidebar.*` 50, `menu.*` 46, `preference.*` 30, `pl_menu.*` 25, `history_window.*` 25, `eq.*` 22, `touchbar.*` 15, `logwindow.*` 14, `plugin.*`/`permissions.*`, `mini_player.*`, `osc*`, `quicksetting.*`, ...). Locale-aware time/date formatting, language names via system locale, ISO-639 list for language fields. Source: `iina/*.lproj/`, `crowdin.yml`.
- **A11Y-04** Fully keyboard-operable player (everything is bindable) and settings search; tool-tips on OSC buttons; ordinary AppKit accessibility only (no custom VoiceOver labels in the player UI - verified: no `accessibilityLabel` in the player views).
- **A11Y-05** Tool-tip/help text and "Help" buttons linking to mpv manual anchors from the settings rows.
- **A11Y-06** Color: dark/light/system theme, high-contrast not specifically handled.

## 21. Other windows & utilities not covered above

- **OTH-01** Settings window search (see section 7 intro). Source: `iina/SettingsSearch.swift`, `iina/SettingsWindow.swift`.
- **OTH-02** Font picker (SUB-13).
- **OTH-03** "Jump to..." time prompt (accepts `h:mm:ss[.mmm]`-style input via `VideoTime`) and other small prompt/confirm/alert helpers (`quickPromptPanel`, `quickAskPanel`, `quickOpenPanel`, `quickSavePanel`) with suppression checkboxes. Source: `iina/Utility.swift`, `iina/Dialog.swift`.
- **OTH-04** Add-URL prompt in playlist and EQ-profile naming prompts with live validation (empty/duplicate). Source: `iina/Utility.swift` `InputValidator`.
- **OTH-05** Key-record view reused for filter shortcuts and binding editor. Source: `iina/KeyRecordView.swift`.
- **OTH-06** Language token field with ISO-639 auto-complete. Source: `iina/LanguageTokenField.swift`.
- **OTH-07** Welcome window is also the "idle" window (reopens on Dock click).

## 22. CORE: how IINA drives libmpv (relevant to Patina because Patina uses libmpv too)

- **CORE-01** One `mpv_handle` per player. Options set *before* `mpv_initialize`: initial volume, `osd-level=0` (own OSD), optional `log-file`, screenshot dir/format/template, `input-media-keys=no`, `keep-open`, `watch-later-directory`, `save-position-on-quit`, `resume-playback`, `geometry`, `vd-lavc-threads`, `ad-lavc-threads`, `hwdec`, `alang`, `volume-max`, `audio-spdif`, `audio-device`, `replaygain*`, `gapless-audio`, avif encoder opts, `sub-auto=no`, `sub-codepage`, all `sub-*` style options, `slang`, `sub-use-margins`, `sub-ass-force-margins`, `sub-scale-by-window`, `cache*`, `demuxer-max-bytes`, `cache-secs`, `user-agent`, `rtsp-transport`, `ytdl`, `ytdl-raw-options`, `reset-on-next-file=ab-loop-a,ab-loop-b`, `ao`, optional `config`/`config-dir`, then the user's advanced `userOptions`, then `input-conf` = active binding file. After init: `vo=libmpv`, `keepaspect=yes`, `gpu-hwdec-interop=auto`; `force-window` is set to `yes` at file open and `immediate` after the render context exists. Source: `iina/MPVController.swift` `mpvInit`.
- **CORE-02** Every preference with an mpv counterpart is registered as an **option observer**: changing the pref immediately calls `mpv_set_property` on every live core (bool/int/float/string/colour or a transformer closure for derived values like `keep-open`, `audio-spdif`, `screenshot-dir`). Source: `iina/MPVController.swift` `setUserOption`, `observeValue`.
- **CORE-03** Observed mpv properties (event-driven UI): `track-list`, `vf`, `af`, `audio-device-list`, `vid/aid/sid/secondary-sid`, `pause`, `loop-playlist`, `loop-file`, `chapter`, `deinterlace`, `hwdec`, `video-rotate`, `mute`, `volume`, `audio-delay`, `speed`, `secondary-sub-delay/pos/visibility`, `sub-delay/pos/scale/visibility`, `contrast/brightness/gamma/hue/saturation`, `fullscreen`, `ontop`, `window-scale`, `media-title`, `video-params/rotate`, `video-params/primaries`, `video-params/gamma`, `idle-active`, `current-ao`; plus events START_FILE, FILE_LOADED, END_FILE, SEEK, PLAYBACK_RESTART, VIDEO_RECONFIG, COMMAND_REPLY (screenshot), SHUTDOWN, HOOK, LOG_MESSAGE, QUEUE_OVERFLOW. Source: `iina/MPVController.swift`.
- **CORE-04** Polled properties for UI (timer): `time-pos`, `duration`, `time-remaining`/`playtime-remaining`, `eof-reached`, `paused-for-cache`, `demuxer-cache-state` (fw-bytes, raw-input-rate), `demuxer-cache-time`, `cache-buffering-state`, `playlist/N/*`, `chapter-list/N/*`, `metadata/by-key/*`, `chapter-metadata/by-key/performer`, `track-list/*`, `audio-device-list`, plus everything the Inspector shows.
- **CORE-05** State machine per player: idle -> starting -> loading -> loaded -> playing/paused -> stopping -> idle, shuttingDown/shutDown; stop/quit are async in mpv so new opens are queued until the stop completes (pending URL); background-task "tickets" cancel stale auto-load work. Source: `iina/PlaybackInfo.swift`, `iina/PlayerCore.swift`.
- **CORE-06** Hooks: `on_before_start_file` (shuffle on CLI), plugin hooks via `mpv.addHook`. Source: `iina/MPVHook.swift`.
- **CORE-07** Window size sync both ways: mpv `window-scale` property updates IINA's window and window resizes write `window-scale`; mpv `fullscreen`/`ontop` property changes mirror to the window. Source: `iina/PlayerCore.swift`, `iina/MainWindowController.swift`.
- **CORE-08** Seek semantics: relative/exact/auto (see PREF-CTL-10), absolute-percent seeks (clamped to <100% to avoid jumping to next file), `frame-step`, `sub-seek`, chapter seeks, restart-from-0 when resuming at EOF, "restart if paused at end". Source: `iina/PlayerCore.swift`.
- **CORE-09** Track selection helper `setTrack(id, type)` for video/audio/sub/secondary-sub; auto work-around: if the first audio track is Apple Positional Audio (apple_apac) it selects the second one. Source: `iina/PlayerCore.swift` `fileLoaded`.
- **CORE-10** Loop modes derived from mpv `loop-file`/`loop-playlist`: off -> file -> playlist cycle. Source: `iina/PlayerCore.swift`.
- **CORE-11** Option whitelists for untrusted callers (URL scheme, plugins): `AppData.safeMPVOptions` and `safeMPVCommands`. Source: `iina/AppData.swift`.

## 23. macOS-only features and candidate cross-platform equivalents

Quick map from every `[mac-only]` / `[mac-ish]` item to a likely Patina decision. This table is analysis, not IINA source; item IDs point back into the inventory.

| Inventory item(s) | macOS facility | Cross-platform direction for Patina |
|---|---|---|
| WIN-12, WIN-01 (translucent OSC/OSD/sidebar) | NSVisualEffectView, Liquid Glass, full-size content view | Slint-drawn semi-transparent panels over a GL/Vulkan video layer; true behind-window blur only where the compositor offers it (Windows acrylic/mica, KDE blur), otherwise solid translucent fill |
| WIN-13 (title bar, proxy icon, path menu) | NSWindow title bar accessories | Custom client-side title bar (needed anyway for OSC-in-titlebar layout); path popup is trivial to reimplement |
| WIN-15 (native + legacy full screen, notch) | Spaces full screen, presentation options | Borderless fullscreen window per platform; "legacy" mode is the only sensible mode elsewhere |
| WIN-16 (black out other monitors) | extra black NSWindows | Same trick with borderless windows (Wayland: needs layer-shell/fullscreen-on-output) |
| WIN-27, WIN-28, CLI-12, PREF-UTL-01 | Services menu, Dock menu, UTIs, `LSSetDefaultRoleHandler` | `.desktop` file + MIME types and `xdg-mime` (Linux), registry ProgID/Default Apps (Windows), Info.plist (macOS) |
| OSC-06, INP-13, INP-14, INP-16 (Force Touch, pinch, haptics) | NSEvent pressure, magnification | Touchpad pinch via platform gesture APIs where available; keep as optional |
| OSC-12 / PowerSource (battery) | IOKit | `battery` crate / UPower / GetSystemPowerStatus |
| PIP-01..04 | private PIPViewController | Emulate: small always-on-top borderless window with minimal controls; no OS API on Linux/Windows |
| PLAT-01, PLAT-02, INP-17 (Now Playing, remote commands) | MediaPlayer framework | Linux MPRIS2 over D-Bus, Windows SMTC (System Media Transport Controls), macOS MediaPlayer |
| PLAT-03, INP-19 (Touch Bar) | NSTouchBar | Drop (hardware discontinued); optional "mini transport" elsewhere is not needed |
| PLAT-04 (Sparkle) | Sparkle 2 | Own updater / package managers / GitHub release check; Windows: WinSparkle-style or MSIX |
| PLAT-07 (sleep inhibit) | ProcessInfo activity | D-Bus `org.freedesktop.ScreenSaver`/logind inhibit; `SetThreadExecutionState`; macOS keep as-is |
| PLAT-08, PLAT-09, PLAT-10 (HW decode probe, GL layer, HDR/EDR, ICC) | VideoToolbox, CVDisplayLink, EDR, CoreDisplay | libmpv `hwdec=auto-safe`/vaapi/nvdec/d3d11va; mpv-owned vsync via render API; HDR requires gpu-next + Vulkan/D3D11 with HDR swapchain (Windows) or Wayland color-management (Linux, experimental); ICC via mpv `icc-profile-auto` |
| PLAT-11 (Live Text) | VisionKit | No equivalent; optional OCR plugin later |
| PLAT-12, SUB-09, NET-01 (credentials) | Keychain | Secret Service/libsecret, Windows Credential Manager, Keychain via a `keyring` abstraction |
| PLAT-13 (Trash, Reveal in Finder, xattr) | NSWorkspace | `trash` crate, `xdg-open`/`explorer /select`, ignore xattr |
| HIS-04 (Open Recent) | NSDocumentController recent items | Own MRU list (plus optional OS jump list/recent files) |
| PLG-* runtime (JavaScriptCore, WKWebView, Network.framework) | JSC + WKWebView | Embedded JS engine (QuickJS/V8) + system web view (wry/WebView2/WebKitGTK) for overlay/sidebar/windows; keep the JS API names to reuse IINA plugins |
| NET-07 (Safari extension) | Safari Web Extension | Keep Chrome/Firefox WebExtensions; they only need the custom URL scheme to be registered per OS |
| PREF-AUD-02 (`ao=avfoundation`/`coreaudio`) | CoreAudio vs AVFoundation driver | Replace with `ao` choices (pipewire/pulse/alsa/wasapi/coreaudio) |
| A11Y-01 (Reduce Motion) | `accessibilityDisplayShouldReduceMotion` | Read the equivalent OS flag (GTK `gtk-enable-animations`, Windows `SPI_GETCLIENTAREAANIMATION`) |

---

## 24. Summary: item counts per area

| Area | Items |
|---|---|
| 1. Window & UI chrome | 30 |
| 2. On-screen controller | 14 |
| 3. OSD | 10 |
| 4. Playlist & chapters | 15 |
| 5. Quick settings (layout/video/audio/subtitles/shared) | 27 |
| 6. Menus (tree entries + behaviours) | 125 |
| 7. Preferences (rows incl. hidden keys) | 192 |
| 8. Input (rules/gestures + default bindings) | 112 |
| 9. Subtitles | 18 |
| 10. Video/audio filters | 8 |
| 11. Music mode | 10 |
| 12. Picture-in-Picture | 4 |
| 13. History / resume / recent | 9 |
| 14. Inspector | 5 |
| 15. Screenshots & thumbnails | 7 |
| 16. Network / streaming / yt-dlp | 10 |
| 17. CLI, URL scheme, file associations | 15 |
| 18. Plugins (platform items + JS API modules) | 27 |
| 19. Platform integration | 20 |
| 20. Accessibility & localization | 6 |
| 21. Other windows & utilities | 7 |
| 22. CORE (libmpv wiring) | 11 |
| **Total** | **682** |

Lines tagged `[mac-only]`: 48; `[mac-ish]`: 9; `[hidden]`: 8 (counted over all lines, including this inventory's narrative lines). Static menu bar: 10 top-level menus, about 143 static items (incl. alternates and hidden ones) and ~16 programmatically populated submenus (Playlist, Chapters, Video/Audio/Subtitle/Secondary Subtitle tracks, Audio Device, Online-subtitle sources, Saved Video/Audio Filters, Plugin, Aspect, Crop, Rotation, Encoding, Other Actions From Key Bindings).
