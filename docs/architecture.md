# Architecture

Patina is one Rust binary. Each player window owns one mpv core; everything
else is shared through the `App`.

```text
            ┌──────────────── UI thread (Slint event loop) ───────────────┐
            │                                                             │
 CLI ──▶ App ── preferences, history, key bindings, list of windows       │
            │        │                                                    │
            │        ├── PlayerController ── PlayerWindow (Slint)         │
            │        │      │   ▲   │            ▲                        │
            │        │      │   │   │ property   │ BeforeRendering:       │
            │        │      │   │   │ updates    │ render into texture    │
            │        │      │   │   ▼            │                        │
            │        │      │  events   PlaybackState   video::Renderer   │
            │        │      │   │                         │               │
            └────────┼──────┼───┼─────────────────────────┼───────────────┘
                     │ async commands                     │ mpv render API (GL)
                     ▼      │   │                         ▼
                 ┌──────────┴───┴─────────────────────────────┐
                 │ mpv core (libmpv, loaded at runtime)       │
                 └──────────────┬─────────────────────────────┘
                                │ mpv_wait_event
                        mpv event thread ── batches ──▶ async channel
```

## libmpv

`src/mpv/` is Patina's own binding. `ffi.rs` declares the client and render
API (client API 2.x) and loads the library with `libloading`; nothing links
against libmpv at build time, so a missing or incompatible library becomes
a clear error message instead of a failed build or a loader crash.
`mod.rs` wraps the handle: `Mpv` is `Send + Sync` (libmpv's API is
thread-safe) and serializes `mpv_wait_event`, the one call with a
single-caller rule. Events, property values and command results are copied
into owned Rust values (`Event`, `Node`). `render.rs` wraps the OpenGL
render context.

## Talking to mpv without blocking

The UI thread never waits on mpv. It sends commands and property changes
with mpv's asynchronous calls (`Player::command`, `Player::set`); failures
come back as reply events. All state the interface shows comes from
properties observed with `mpv_observe_property` (the list is `player/props.rs`).

A dedicated thread per core waits for events, drops log messages into the
`log` crate, keeps only the newest change of each property in a batch, and
sends the batch over an `async-channel`. A task spawned on Slint's event loop
receives batches, folds property changes into `PlaybackState`
(`player/state.rs`, which turns mpv's nodes into typed tracks, chapters,
playlist entries and video parameters), and `PlayerController::sync` copies
only what changed into the Slint globals.

Patina's `on_load` hook (subtitle matching, the yt-dlp format chosen from
the quality menus, a Jellyfin entry's stream) holds mpv until a thread of
Patina's continues it. Shutting
a core down lets go of a hook it is waiting in first, and the event thread
lets any later one go at once: mpv can't quit from inside a hook, and the
thread that would continue it waits for the UI thread, which may be the one
shutting the core down.

**PipeWire.** When mpv's PipeWire audio output can't connect, PipeWire's
cleanup sometimes waits forever for its own thread (`pw_thread_loop_stop`,
with libpipewire 1.6), and mpv's core waits with it; the next call the UI
thread makes into mpv then never returns either. mpv tries that output
before every file unless `ao` says otherwise, and lists audio devices by
going through every output it has, whatever `ao` says. So on Linux without
a PipeWire socket to connect to (`platform::pipewire_missing`) a core gets
`ao=pulse,alsa`, unless the user chose an output (in their mpv options, on
the command line or in Patina's mpv.conf), and doesn't keep
`audio-device-list` observed: the window asks for it when the menu opens,
the Audio panel shows or the preferences' Audio page does
(`PlayerController::request_audio_devices`), so the menu has the list from
its second opening on. That leaves the PipeWire attempt to the moment a
device list is shown, instead of every file and every start. The Flatpak's
libmpv is built without PipeWire's output, so none of this applies there.

Commands that concern the window rather than playback are handled in Rust:
`quit` closes the window, `show-progress` and `show-text` become Patina OSD
messages, and mpv's `fullscreen`, `ontop` and `window-scale` properties are
mirrored onto the window in both directions, as IINA does.

## Video

mpv renders into an OpenGL texture that Patina owns (`video/mod.rs`), and
Slint draws that texture as an ordinary `Image` under the interface. In
Slint's `BeforeRendering` notification the renderer asks mpv whether a new
frame is ready, and renders only then or after a resize. Interface-only
redraws (hovering a button while paused) cost nothing on the video side,
and the video can be placed anywhere in the scene: music mode draws it in
the cover's place only (see Music mode).

- mpv's update callback, which runs on an mpv thread, only posts a redraw
  request to the event loop, coalesced with an atomic flag.
- Rendering never blocks on the frame's display time
  (`MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME = 0`, with
  `video-timing-offset=0`), so a slow frame can't stall the interface.
- The GL state mpv may change is saved and restored around its render call.
- Slint's `get_proc_address` is only valid during its notifier, so mpv gets
  a process-wide resolver backed by the platform's GL libraries
  (`video/gl_loader.rs`).
- On Linux the Wayland or X11 display is passed to mpv so VA-API can share
  it with OpenGL through EGL dma-buf interop.
- Files are only opened once the render context exists; until then
  `PlayerController::open` queues them. Without a render context mpv would
  disable video for the file.

Shutdown order matters: hiding the window makes Slint tear down its renderer
with the GL context current, which frees mpv's render context; then the
core is told to quit and its event thread joins. `RenderContext` holds an
`Arc<Mpv>`, so the core can never be destroyed while a render context
exists.

Where the window can't be drawn with OpenGL at all (no GPU driver, a
virtual machine, a remote desktop), Slint's software renderer draws the
interface, and `video/software.rs` runs mpv's software renderer on a thread
of its own, drawing each frame into a buffer the size of the window that
reaches it as an ordinary image. Patina gets there in one of these ways:

- Slint can't create its OpenGL renderer for the window, and uses the
  software one without saying so. The rendering notifier, which only an
  OpenGL renderer has, then reports itself unsupported, and the window
  starts the software video instead. (Windows without a GPU driver.)
- The renderer exists but OpenGL fails as Slint makes the native window.
  Slint keeps the error for when its event loop ends, and doesn't end it:
  the window never appears. Three seconds after showing its first window,
  Patina ends the loop itself if the window still has no native window.
- OpenGL works, but it is Apple's software renderer (a Mac without a GPU),
  which draws mpv's video in the wrong colors and streaked. The video
  renderer ends the loop when it sees it.
- Any other error ends the event loop.

In the last three, Patina starts itself again with
`PATINA_RENDERER=software`, since Slint chooses its renderer once per
process (`video::software::restart`).

## macOS

The menus exist twice, because Slint builds a `MenuBar` from the `Menu`
elements written inside it before it inlines components: `MainMenu` in
`ui/menus.slint`, and a copy in `ui/app.slint`'s `MenuBar`, which Slint shows
as the native menu bar on macOS (after an app menu of its own) and nowhere
else. `tools/menubar.py` makes the copy, reading `Playback` and `Media`
where `MainMenu` reads its copies of them (see Interface), and a test fails
when it is stale.
Slint's app menu quits through AppKit, from inside the event loop, so a
winit application handler closes the windows (saving where each file was)
however the loop ends.

The title bar is IINA's: `macos.rs` makes the player window's title bar
transparent with the content running under it once winit has made the
window, Patina draws the title over the video, and macOS's own window
buttons fade with the controls.

## Interface

The Slint files mirror IINA's window:

- `ui/app.slint`: `PlayerWindow`: video, welcome view, title bar,
  floating controller, OSD, sidebar, dialogs, auto-hide timers and mouse
  actions.
- `ui/controls.slint`: the controller, the seek preview, the title bar,
  the OSD, and the Skip button for a Jellyfin item's intros and credits.
- `ui/library.slint`, with `library-detail.slint`, `library-sign-in.slint`,
  `library-parts.slint` and `library-model.slint`: the Jellyfin library,
  shown where the welcome view is (see Jellyfin).
- `ui/youtube.slint`, with `youtube-card.slint` and `youtube-model.slint`:
  the YouTube section, shown there too (see YouTube section).
- `ui/sidebar.slint`: the playlist, chapters and a Jellyfin song's
  lyrics, and the video, audio and subtitle quick settings.
- `ui/music.slint`: music mode's strip of controls, its close and back
  buttons, and the artwork for audio without a cover.
- `ui/menus.slint`: the application menu.
- `ui/welcome.slint`, `ui/dialogs.slint`, `ui/widgets.slint`,
  `ui/theme.slint`, `ui/globals.slint`.

Rust and Slint meet in the globals of `ui/globals.slint`: `Playback` and
`Media` carry state into the interface; `Actions` carries user intent out,
either as mpv commands (`Actions.mpv(["cycle", "pause"])`) or as Patina
actions (`Actions.patina("toggle-sidebar playlist")`). Patina actions use
IINA's command names where IINA has one, and are the same strings key
bindings use (`#@patina KEY action`), so every action is reachable from the
interface, the menu and the keyboard alike. They're implemented in
`PlayerController::run_action`.

Slint draws with its winit backend, which `backend.rs` sets up. On Linux
and the BSDs the backend is wrapped to put Wayland's ordinary clipboard in
place of Slint's: Slint's goes through arboard, which on Wayland speaks only
the data-control protocols meant for clipboard managers, and KWin keeps
those from a Flatpak, which has no X11 to fall back on either, so nothing
pasted into a text field there. `clipboard.rs` reaches the clipboard and the
primary selection through smithay-clipboard on winit's own connection, as
any client may while it has the keyboard focus; Patina's own Copy and Paste
use it too on Wayland. Copies go to a thread of their own, which never
pastes, so they always land. A paste waits two seconds at most for the
clipboard's owner; the thread that asked is then left waiting until the
owner answers, and the next paste goes to another. Until Patina sets a
selection itself, or the keyboard focus has left all of Patina's windows
(an ordinary program can only take a selection while it has the focus),
a paste doesn't ask an owner that may be the one that hangs, and a toast
in the window pasted in, over any dialog there, says the app isn't
responding: a hung owner costs one wait each time the focus comes back to
Patina from another program. Should a paste asked then find it hanging
still, nothing more is asked until an owner answers, but for what Patina
copied itself, which the last of three paste threads is kept for. A
clipboard manager, or wl-copy, can set a selection without the focus,
which smithay-clipboard doesn't tell of: its text isn't pasted while the
owner before it is taken to be hung, until the focus has been elsewhere.
No smithay-clipboard worker is stopped while Patina runs, since
destroying one's data device as the compositor sends it a selection
breaks the connection; and as the workers run on winit's display, Slint's
platform, which closes it, is kept as Patina exits if one still waits on
an owner that never answered, or hasn't opened its handle yet.

The application menu reads `MenuPlayback` and `MenuMedia` rather than
`Playback` and `Media`: copies of what it shows, made when it opens. Slint
(1.18) numbers a menu's entries afresh whenever anything the menu reads
changes, and an open submenu asking for its entries by an old number gets
none; a player's state changes all the time (tracks and chapters as a file
plays), which left submenus empty where Slint draws the menus itself, as on
Linux. The copies don't change while the menu is open.

## Music mode

IINA's mini player is a window of its own that the player's one video view
moves into. Patina's video can't move to another window: each Slint window
has its own OpenGL context, an mpv core takes one render context, and
Wayland destroys a hidden window. So music mode is a layout of the player
window (`music-mode` in `PlayerWindow`), as picture-in-picture is:

- The window is 300 wide. On top is the picture, 300 divided by its aspect
  ratio tall (`Playback.picture-aspect`, the video's or the cover's, kept
  between 1:2 and 4:1), or Patina's own artwork when there is none; under
  it `MusicPanel`, 72 tall; under that the sidebar, as a drawer showing
  the playlist and chapters (`sidebar-open` is the drawer being open). The
  window's minimum and maximum sizes follow the mode, and the title bar,
  controller and OSD give way.
- mpv draws at the picture's size rather than the window's: the renderer
  asks the window for a size of its own (`video::Hooks::target_size`), and
  the software renderer is resized whenever that area changes.
- `PlayerController` enters and leaves the mode, sizing the window once
  Slint has applied the mode's size limits (a zero-delay timer). Leaving
  puts back the window's size from before, unless something sized the
  window in the meantime, such as fitting it to the video that ended music
  mode. Opening or closing the drawer, or hiding the cover, resizes the
  window from Slint's `changed` handlers (`music-layout`), and a resize by
  hand opens or shuts the drawer once it ends, which winit doesn't report:
  300 ms after the last `Resized`.
- The automatic switch follows IINA's test (`PlaybackState::audio_status`):
  a stream is never music, a file whose tracks aren't known yet is left
  alone, and one is music when its only video tracks are covers. A switch
  by hand turns it off for the window until the next one, as IINA's
  override does.
- `Playback.has-picture` is false for a file without a picture, which
  hides the video image in either layout: the texture otherwise keeps the
  last frame of whatever played before.

## Input

`input.rs` parses `input.conf` files, normalizes key names the way mpv does,
maps Slint key events onto the same names, and writes binding sets back out.
The built-in set is IINA's defaults with Ctrl in place of Command outside
macOS; user sets live in the config folder's `input_conf/`. Mouse actions and
scrolling follow IINA's control preferences.

## Settings and history

`settings.rs` holds `Preferences`, the playback history, and the platform
directories. Every preference is a plain JSON value with a default, so old
or partial files keep working (an unreadable field falls back on its own),
and the preferences window can edit any of them by key.
`Preferences::mpv_options` maps them onto mpv options: they are set before
each core starts, and when preferences change, `App` sets the ones that
changed on every running core, as IINA's option observers do. Resume
positions use mpv's own watch-later files, except a Jellyfin item's, which
the server keeps.

## Preferences window

`src/preferences/schema.rs` lists every setting: page, section, label, and
control (switch, number, choice, text, path, lines or button).
`src/preferences/mod.rs` turns the schema plus the current values into rows
for `ui/preferences.slint`, and writes edits back through the JSON form of
`Preferences`. Adding a setting means adding a field with a default and a
schema entry; the window needs no other change. The Key Bindings page edits
binding sets; changing the built-in set first copies it to a user set.

## Seek-bar thumbnails

`thumbnails.rs` makes 100 thumbnails per local video on a thread of its own,
with a second, headless mpv core: the file is opened paused, and for each
thumbnail mpv seeks to the nearest keyframe and renders it through the
software render API at twice the display width. Frames are rendered as soon
as mpv announces them, because mpv holds a seek back until the previous
frame has been drawn. Thumbnails reach the window as they are made, and are
cached as JPEGs keyed by path, size and modification time, with the least
recently used files evicted past a size limit.

## Playlist information

`probe.rs` fills in the playlist's durations as IINA's prefetching does: one
more headless mpv core, on a thread of its own, opens each local file
(paused, with nothing shown or heard) just far enough to read its length and
tags. `App` caches the results and passes them to the windows as they come.

The playlist itself is mpv's. `playlist.rs` holds what Patina adds on top,
free of mpv so it is unit-tested: the selection, kept by mpv's entry ids so
it follows entries as they move; turning "move these rows there" into the
one-entry `playlist-move` commands mpv has; the series prefix each name
shares with another, folded in the list; and reading pasted text as paths
and URLs. The clipboard is `arboard`, or on Wayland `clipboard.rs` (see
Interface); moving files to the trash is the `trash` crate, on a thread of
its own, or in a Flatpak the desktop portal's Trash, which moves only files
the sandbox may write. Patina says it moved them once that is done, or that
it couldn't.

## Subtitle matching

`subtitle_match.rs` follows IINA's matcher. Subtitle files in the video's
folder and its immediate subfolders are scored against every video in the
folder: by name, then by episode number with the series names compared by
edit distance, so each file goes only to the video it fits best. Each
player core registers mpv's `on_load` hook; when it fires, a short-lived
thread finds the matches and sets them as that file's `sub-files` before
continuing the hook, so mpv loads and selects them exactly as it would its
own auto-loaded files (mpv's `sub-auto` is off in this mode). Choices made
from the playlist menu (subtitles added for one file, or its matches turned
down as wrong) live in `App`, shared with those threads, and apply in either
mode.

## Online video

mpv's own yt-dlp hook plays online video; Patina adds IINA's Online Media
features around it. The hook leaves what yt-dlp said about the video in
`user-data/mpv/ytdl/json-subprocess-result`, which a window reads when
the file has loaded: `ytdl.rs` turns its formats into the Video Quality
menu's resolutions and frame rates and the Audio Quality menu's formats.
Choosing one restarts the playlist entry. Patina's own `on_load` hook runs
first (its priority is below the yt-dlp hook's, which mpv's Lua scripts
shift down by 50) and sets the yt-dlp format for that file, and its start
at the position it was playing, before yt-dlp is asked. Choices are kept
per address for as long as the window is open.

mpv 0.41's hook can't open a YouTube video with an ad under yt-dlp's default
clients: their stream addresses answer 403 until the ad's skip time has
passed, and the hook doesn't wait. So Patina adds
`extractor-args=youtube:player_client=visionos,default` to what mpv and
downloads pass to yt-dlp, unless Patina's yt-dlp options have their own
`extractor-args` or cookies (`settings::ytdl_youtube_clients`, which says when
it can go). It isn't kept in the preference, and mpv needs its value quoted.
mpv isn't given it when its hook would run youtube-dl, which has no such
option. Options mpv can't read, such as a flag with no `=`, are dropped with a
warning and it goes alone, as adding to them would stop every online video;
downloads still read those as they plainly mean. yt-dlp's own config file
can't be seen from here, and the argument replaces that file's whole `youtube`
entry, `lang` or `po_token` included, so those belong in Patina's yt-dlp
options if they are to last.
mpv says the signed stream address, with the user's IP address in it, when
one fails to open, so `player/mod.rs` cuts `googlevideo.com` addresses out of
what it logs.

The HTTP proxy preference also goes to yt-dlp, from mpv's hook and from
downloads, as a `proxy` in its options, unless Patina's yt-dlp options have
their own (`Preferences::proxy`). mpv's hook gives yt-dlp only what is in those
options, so otherwise yt-dlp would go direct while the stream went through the
proxy. The YouTube section's own requests follow the same rule, and fail rather
than go direct when a proxy can't be used.

A video opened from the YouTube section is marked as such in its window
(`from_section`, set by `Placement::Section`), never by its address, so that the
same address from Open URL behaves as it did. With that mark the window goes back
to the section at the end of its last entry and on Escape, says it is waiting for
YouTube, and has the load hook give mpv its title, where it was left, a format up
to the screen's height (`youtube-quality`) and the section's own yt-dlp options:
the clients and proxy with `ignore-config` and nothing of the user's, as cookies
there would sign a section with no account in to Google. Those last as long as the
file. What yt-dlp says when it fails reaches the window as mpv's hook logs it, the
only place it is said, and is named by `youtube::ytdlp`. Watched marks go by how
far the position moved by playing (`youtube::playing::Sitting`), for any video
opened by its one address, and follow the history's preference and Clear Recent.

The section's downloads share the queue with Jellyfin's in `downloads.rs`, and
wait by rules of their own: at most two of yt-dlp running, started at least 8
seconds apart. Neither kind holds the other up.

Which yt-dlp runs is decided in one place, `downloads::find_ytdl`, for the
hook, downloads and the section alike: the path preference, else a
`ytdl_path` in the hook's own `script-opts/ytdl_hook.conf` (when mpv is given
its configuration folder), else a `yt-dlp` in that folder, all the user's
choice; then the copy the package carries (`platform::bundled`: yt-dlp's
folder build, as Patina.app/Contents/Helpers/yt-dlp.app on macOS and in a
yt-dlp folder beside patina.exe on Windows), or a
`yt-dlp` the user installed instead when that is newer; then the `PATH`. The
installed one is looked for on the `PATH` and, on macOS, in
/opt/homebrew/bin and /usr/local/bin, which an app opened from the Finder
doesn't have there. `ytdl_versions` asks each for its version (`--ignore-config
--version`) on a thread of its
own, compares the dates (a nightly's build time after its day's release; a
tie goes to the package's), and keeps the answer by path, size and
modification time, in memory and in `yt-dlp-versions.json` in the data
folder, so each copy is asked once. For a Scoop shim, which `scoop update`
leaves the same file, the size and time are those of the program its
`.shim` names. One that doesn't answer within 45
seconds, which leaves room for a first start while macOS or Windows scans
its files, or answers something that isn't a version, is passed over for the
run. The choice never waits: until both have answered the package's copy is
used. Whenever the choice changes, as when the installed one turns out newer,
or is removed or replaced by an older one,
`App::ytdl_choice_changed` hands the hook its new `script-opts` and tells the
YouTube section and the Downloads window, as a changed path preference
does; a window makes the choice again before it opens a web address, so a
copy removed since is noticed before the hook would run it. The Downloads
window and the section drop a version read for a copy no longer chosen. A `youtube-dl` is never a candidate, as it can't play YouTube. Where a package carries one,
`Preferences::mpv_options` hands the hook the result as its `ytdl_path`, quoted
with its length, since the hook would never find the bundled copy itself and
an app opened from the Finder has only the system's folders on its `PATH`;
but not when `ytdl_hook.conf` names one, which a `script-opts` would
override. Where nothing is bundled (a Linux build, the Flatpak, a build from
the source tree) the search is the hook's own, unchanged, and the hook is
told nothing unless the preference says. ffmpeg for downloads goes the same
way (`downloads::Ffmpeg`): an `ffmpeg-location` in Patina's yt-dlp options,
else the bundled copy, passed as `--ffmpeg-location` (so it wins over one in
the user's own yt-dlp configuration file, which yt-dlp reads before its
command line), else the `PATH`. The bundled ffmpeg is built in release.yml
from FFmpeg's source by `packaging/ffmpeg.sh`, for macOS 11 like the rest
of Patina.app, and can do only what Patina's downloads ask of it: join video
and audio and make yt-dlp's fixes, by copying streams. Without any, a
download that would join video and audio fetches the best single file
instead. Nor can the bundled ffmpeg record a live stream, which yt-dlp hands
to ffmpeg and which needs its network protocols, so the player's Download
refuses one while that ffmpeg is the one to be used, as the section always
does (`Ffmpeg::records_live`). `patina --version`
prints the bundled copies it finds, which release.yml checks.

Downloading runs yt-dlp on threads of its own (`downloads.rs`), with
progress, post-processing and the final file printed in templates of
Patina's, so nothing depends on the wording of yt-dlp's own output.
Running downloads are stopped when Patina quits; yt-dlp picks up their
partial files the next time. A yt-dlp is stopped as a process group, as
it may have started ffmpeg, and a single-file build the user installed is a
launcher and a worker. On Unix the group is asked to end first (SIGTERM) and
killed only if it is still running 1.5 seconds later (`platform::end_group`),
so that ffmpeg can close its file and such a launcher can remove the folder
it unpacked itself into, which it can't when killed; Windows has no such
request for a windowless console program, so there it is killed at once.
The folder builds the packages carry unpack nothing, so stopping them
leaves nothing behind either way. The same
window lists files downloaded from Jellyfin servers, which Patina fetches
itself (`jellyfin/download.rs`, described in [jellyfin.md](jellyfin.md)),
one at a time from each server, and reports in yt-dlp's terms; those
remove their part files as they fail or are stopped, and one left by a
Patina that was killed goes when a later download wants its name.

## Online subtitles

`opensubtitles.rs` is a blocking client for the OpenSubtitles.com REST
API, shared by the windows so a sign-in is made once. Each user supplies
their own API key; an account is optional and allows more downloads a day,
and its password lives in the system's password store (`credentials.rs`,
over the keyring crates), never in the settings. A search sends the file's
OpenSubtitles hash (its size plus its first and last 64 KiB as 64-bit
words) and its name, in the preferred languages turned into OpenSubtitles'
codes; results that matched the hash come first. Downloading is two steps:
the API hands out a link with the day's remaining quota, and the file comes
from the link. Requests follow OpenSubtitles' best practices: the rate
limit headers are honoured, a sign-in that has run out is renewed once,
and Patina signs out when it quits. The window runs each search and
download on a thread of its own; a newer one makes an older one's answer
moot. Downloads go to the data folder's `online-subtitles` and are added
with `sub-add`.

## Jellyfin

Patina browses and plays a Jellyfin server's library in the player window.
[jellyfin.md](jellyfin.md) is the design, with the API it uses and the
rules about the account's token; this is how it sits among the rest.

- **The session.** `App` owns one `jellyfin::session::Session`, on the UI
  thread: the servers kept in `jellyfin.json`, the accounts signed in on
  them with a ureq client each (their tokens asked of the keyring on a
  thread of its own as Patina starts), the image cache, the reporter, and
  the registry of what the library opened. Windows subscribe as
  `Listener`s and hear of sign-ins, sign-outs, tokens the server no longer
  takes, and a playback's end that the server has taken.
- **The library.** Each player window has a `LibraryController`
  (`jellyfin/library.rs`), which fills the window's `Library` global and
  answers its callbacks. A window starts on the welcome view, which lists
  the servers; one picked or signed in to puts the library in that idle
  window where the welcome view was, and its This Computer tab is the
  welcome view again. Page loads, from every
  window, share six threads (`session::load`), and an answer for a page
  the user has left is dropped.
- **Playing.** What the library plays goes into mpv's playlist as
  `jellyfin://<server id>/<item id>`, so the playlist, the history, Recent
  and the system's media controls see neither the token nor a stream's
  address. Patina's `on_load` hook resolves such an entry on its thread
  (`playback::prepare`): it asks the server for the item and how to play
  it, sets `stream-open-filename` to the stream, and sets the file's
  tracks, title and subtitles (and for a transcode, the `Authorization`
  header) as file-local options, which end with the file. The window takes
  what the hook planned once mpv has opened the file, and selects the
  server's tracks once mpv lists them.
- **Reports.** The window reports the playback's start, progress and stop
  from mpv's changes, and one reporter thread for the app sends them in
  order, dropping progress that a later report of the same playback has
  overtaken; quitting waits for them, sharing the moment the OpenSubtitles
  sign-out has. The server keeps a Jellyfin item's resume position, so no
  watch-later file is written for one.
- **Images.** The image cache's workers fetch posters and backdrops
  through the client of the server they come from and decode them into
  pixel buffers, which become `slint::Image`s on the UI thread; they are
  kept in memory and in the data folder's `jellyfin/images`, the least
  recently used removed past a size limit, as thumbnails are. A playing
  item's trickplay tiles (the seek bar's previews), its media segments
  (the Skip button) and a song's lyrics (the playlist's Lyrics tab) are
  fetched on the load threads.

## YouTube section

Patina shows YouTube in the player window with no account: the lists are its
own, in the data folder. [youtube.md](youtube.md) is the design, with what
leaves the machine and what the terms say; this is how it sits among the rest.

- **The session.** `App` owns one `youtube::session::Session`, on the UI
  thread: `youtube.json` and the saved feed, each read the first time it is
  wanted, the threads requests run on, and the picture cache. Nothing in it
  asks the network as Patina starts. Windows subscribe as `Listener`s, and
  count themselves in and out with `entered` and `left`: when the last has
  left, what is on its way stops. Quitting stops it too.
- **The view.** Each player window has a `YouTubeController`
  (`youtube/view.rs`), which makes no thread or request until the section
  opens. It fills the window's `YouTube` global and answers its callbacks. The
  section is a third view where the welcome view is, and `ui/app.slint`'s
  `youtube-open` and `library-open` allow one of them at a time, each giving
  the focus back as it goes (Slint keeps it on items that have gone, and keys
  then reach nothing). `keep_keyboard` in `window.rs` looks for that ahead of
  each of the window's events, as a card scrolled out of a list and a choice
  from a submenu leave it so with nothing to say it has happened.
- **Sources.** `youtube/feed.rs` builds the feed through its `Source` trait,
  from `rss.rs` and then `ytdlp.rs`. `client.rs` is the only plain HTTP, to a
  short list of YouTube's hosts, checked on every redirect. Each yt-dlp call
  is a short-lived process with a time limit, `--ignore-config` and none of
  the user's yt-dlp options, as those could hold cookies and sign the section
  in.
- **Images.** The section uses `jellyfin/images.rs`'s cache with a pool of its
  own, kept in `youtube-images`, so that a picture never waits on a poster.
- **Playing and downloading.** These are in Online video above: `from_section`
  in the window, and the shared queue in `downloads.rs`.

## History and inspector windows

`history.rs` and `inspector.rs` each drive a window of their own, created on
first use and kept by `App`. The history window rebuilds its rows from the
history whenever it changes, and checks for missing files on another thread.
The inspector follows the active player; while open, it reads the current
tab's properties once a second with `mpv_get_property_async`, and the
player's event pump hands the replies back to it.

## Opening, one Patina at a time

`instance.rs` makes the first Patina to start the one that runs, the
primary, as soon as `main` has read the command line and before anything is
shown or libmpv is loaded. It locks a file in a folder only the user can
reach (`$XDG_RUNTIME_DIR/patina` on Linux, the app's own part of it in a
Flatpak, a private folder in the temporary folder on macOS, the user's
local application data on Windows), writes a random
id beside it, and listens there on a Unix socket, or on Windows on a named
pipe named after the id, whose security lets only the user open it. The
file names are a hash of the settings folder's canonical path, so a test run
with `PATINA_CONFIG_DIR` has an instance of its own. The system releases
the lock however a process ends: a lock that can be taken means no Patina
runs, and a socket left by a crash is replaced. A lock file removed under a
running Patina (by a cleaner of old files) is noticed by the next launch,
which becomes a primary of its own, and the first one, quitting, leaves the
new one's files alone.

A later launch finds the lock taken, reads the id and connects instead,
reads the primary's greeting (its protocol version and id), checks the id,
sends one line of JSON (the sources, made absolute in its own folder with
file:// URIs turned into paths, the flags, any `--mpv-*` options and the
desktop's activation token) and exits once the primary has opened them. On
Windows the pipe's name can't be guessed or taken first, the launch checks
that the process serving it runs as the same user, and it connects at the
identification level, so that a pipe's server can't act as the user. A primary that is still starting, busy or already quitting makes
the launch try again for five seconds; another protocol version, no
greeting within ten seconds, `--new-instance` or standard input (which
can't be handed over) start a separate Patina that serves no one. On macOS
a Patina that LaunchServices started (`open -n`, or a Finder open while
another copy runs) doesn't forward either: what it is to open arrives as
Apple events after startup.

The listener hands each request to the UI thread with
`slint::invoke_from_event_loop` and waits for `App::open_request`'s verdict.
The interface takes a request before opening it, and the listener withdraws
one not taken within five seconds, or once Patina starts quitting, and sends
the launch elsewhere; once taken, the launch is told so and waits however
long opening takes. A launch that stops waiting before its request has gone
never sends it. So a request is opened once, and a launch is only told it
was taken if it opened (or waits for startup to finish). Startup itself
waits for the event loop to deliver what is already queued: requests that
come before the launch's own sources are open follow them, and when there
were none of its own (Finder starting Patina with a file on macOS), the
welcome window and the Open dialog are skipped. Quitting (a signal quits
from inside the event loop too) refuses requests at once but keeps the lock
until the windows have saved the history and settings; then the server is
dropped: it stops accepting, removes the socket and id and gives up the
lock, so the next launch becomes the primary. If OpenGL fails while it
starts (the only time it starts again in software), it hands what it took
meanwhile to the new Patina.

The request then opens as IINA would: plain opens go through
`App::open_sources` (an idle window, a new one or the newest, as the
multiple-windows preference says), and those within 0.2 s of the last one
join its playlist, as IINA batches Finder's opens (Explorer starts one
process per selected file). `-e` appends, `-w` gives each source an idle or
new window (a window counts as busy from the moment a load is asked of it),
`--mpv-*` options from a launch (and a link's `mpv_` options) are set as a
new window's core starts, and that window is kept from other opens, which
would inherit them. `-f` and `--music-mode` apply to the window that
opened, `--pip` once its video shows, and a launch with nothing to open brings the active window
forward. Coming to the front needs the desktop's consent: on Wayland the
launch passes on `XDG_ACTIVATION_TOKEN` and `wayland.rs` hands it to
xdg_activation_v1 on winit's connection (winit 0.30's `focus_window` does
nothing there; a new window is raised once it first draws); on Windows the
launch lets the primary take the foreground with `AllowSetForegroundWindow`;
on macOS winit activates the app.

### patina:// links

`scheme.rs` reads IINA's iina:// form under Patina's name:
`patina://open?url=…` (or `weblink`) with `new_window`, `enqueue`,
`full_screen`, `pip` and `mpv_<option>`. Values are decoded once and `+`
stays a `+`, as Foundation's URLComponents leaves them, so a stream's own
escapes survive; a repeated parameter counts once. Links come from web
pages, so Patina trusts them less than IINA does: `url` must be a network
stream, never a local file, standard input or one of mpv's own protocols,
and mpv options come only from IINA's list of safe ones, less those that
turn off certificate checks, raise the volume limit, or use up memory and
threads. srt:// and rtp:// are refused too, since their URLs can make
Patina listen for connections. A playlist a link opens may only list such
streams too: the load hook refuses anything else, a local file, a share or
a listening address, and screens playlists it names in turn. Entries of any
playlist from the network may name local paths and shares, which mpv won't
open from there; Patina doesn't read them either (no probing, subtitle
matching, revealing or trashing). Copied or played in a new window, an
entry of any playlist opens as the user's own, so only what mpv would open
from that playlist may go: a network stream, or from a playlist on disk a
file, never one of mpv's own protocols (av://, edl:// and the rest). Entries
of a link's playlist reopened that way stay screened, and Open in Browser
opens only web pages. Messages show a link's text escaped. Links arrive as an argument
after `--` from the desktop entry and, after `--open-link`, from Windows'
protocol key, which makes whatever follows one link even if a quote split
it; on macOS they arrive as a get-URL Apple event.

### File types

`packaging/media-types.tsv` lists the extensions Patina opens, with their
kind and MIME types. `formats.rs` reads it for the file dialogs, subtitle
matching and MPRIS; a test keeps the MimeType line of
`packaging/linux/video.patina.Patina.desktop` in step with it; and
`packaging/macos-bundle.sh` makes the bundle's document types from it.
`associations.rs` does what IINA's "Set IINA as the default application"
does, as far as each system allows. On Linux, Preferences › Utilities runs
`xdg-mime default` for videos, music or playlists and patina:// links,
after writing a desktop entry for the running executable when no package
installed one; a Flatpak shows none of this, since the system's own settings
choose a sandboxed app's file types, from what its desktop entry declares.
On macOS it asks LaunchServices to make Patina the default
for the media types each extension maps to (not another app's type that
shares it, such as TypeScript's .ts), and for patina:// links with videos. On Windows, where
no app may make itself the default, it writes ProgIDs, Open With entries,
the `patina` protocol and Default apps capabilities under the user's own
classes, opens Default apps for the user to choose Patina, and takes it all
out again on request. The installer registers none of it, but its
uninstaller runs `patina.exe --remove-file-types`, which takes out what
Patina wrote, so nothing is left pointing at a deleted program.

winit's macOS application delegate handles no opens, so `macos.rs`
installs Apple event handlers for open-documents and get-URL as the app
finishes launching (after AppKit's own, before any event is delivered) and
turns them into requests. `NSTreatUnknownArgumentsAsOpen` is off, so files
named on the command line don't arrive a second time as an event.

## System media controls

`media_controls/` publishes what a window plays and turns media keys into
`Command`s for it: the window focused most recently of those with something
to play, or the one focused most recently when none has
(`App::retarget_media`), so that a window opened to browse or queue in
doesn't take the controls from the one playing. It uses MPRIS over D-Bus
on Linux, the System Media Transport Controls on Windows, and
MediaPlayer's remote command and now-playing centers on macOS. zbus is async, so the MPRIS
service and the screen-saver inhibitor each run `zbus::block_on` on a thread
of their own. The MPRIS name is
`org.mpris.MediaPlayer2.video.patina.Patina.instance<pid>_<token>`: under the
application ID, which a Flatpak may own without being granted it, and with a
token drawn per process, since every Flatpak launch is process 2 in a PID
namespace of its own. In a Flatpak the screen is kept awake through the
desktop portal's Inhibit rather than `org.freedesktop.ScreenSaver`, and
revealing a file asks `org.freedesktop.FileManager1` over D-Bus. Commands arrive on other threads and reach the UI thread
through `slint::invoke_from_event_loop`. As in IINA, a media key runs the
user's binding for it (`PLAYPAUSE`, `NEXT`, ...) when one exists.

## Testing

- `cargo test` runs unit tests, plus tests that drive real mpv cores with no
  window and no audio output; those are skipped when libmpv is missing,
  unless `PATINA_TEST_REQUIRE_MPV=1` (and `PATINA_TEST_REQUIRE_FFMPEG=1` for
  the ones that make clips with ffmpeg) turns a skip into a failure. CI sets
  both on Linux, macOS and Windows.
- A test starts a full player core with the default options, so a libmpv
  older than Patina's minimum (mpv 0.39) fails a test, not only the app.
- Tests also check that every preference-driven mpv option is accepted by
  mpv, that the schema matches `Preferences`, and that thumbnails are made
  and cached for a generated clip (when ffmpeg is installed).
- The Jellyfin client's tests read answers saved from a real server and
  talk to stand-ins of their own; its live tests need a server, which
  `tools/jellyfin-server.sh` runs on 127.0.0.1 with a library it makes,
  and skip without one unless `PATINA_TEST_REQUIRE_JELLYFIN=1`. CI's Linux
  job runs that server and the Jellyfin tests against it
  (development.md has the commands, jellyfin.md what the tests cover).
- The YouTube section's tests need no network or real yt-dlp, and CI doesn't
  reach YouTube: they read made-up answers, and talk to a small server on
  127.0.0.1 and on Unix to a stand-in script (youtube.md, Tests).
- `tools/smoke.sh` runs Patina on a private Xvfb display, sends input with
  xdotool and saves screenshots, for checking the interface end to end. Xvfb
  only has software OpenGL, so it uses software decoding.
- `PATINA_FRAME_DUMP` writes what mpv rendered to a file, which works on a
  real GPU (for example under a nested `kwin_wayland --virtual`) where
  screenshots of the display are not available.
