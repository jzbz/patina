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
and the video could later be placed anywhere in the scene (music mode, a
docked controller).

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

## Interface

The Slint files mirror IINA's window:

- `ui/app.slint`: `PlayerWindow`: video, welcome view, title bar,
  floating controller, OSD, sidebar, dialogs, auto-hide timers and mouse
  actions.
- `ui/controls.slint`: the controller, the seek preview, the title bar and
  the OSD.
- `ui/sidebar.slint`: playlist and chapters, and the video, audio and
  subtitle quick settings.
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
positions use mpv's own watch-later files.

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

## System media controls

`media_controls/` publishes what the active window plays (the window focused
most recently) and turns media keys into `Command`s: MPRIS over D-Bus on
Linux, the System Media Transport Controls on Windows, and MediaPlayer's
remote command and now-playing centers on macOS. zbus is async, so the MPRIS
service and the screen-saver inhibitor each run `zbus::block_on` on a thread
of their own. Commands arrive on other threads and reach the UI thread
through `slint::invoke_from_event_loop`. As in IINA, a media key runs the
user's binding for it (`PLAYPAUSE`, `NEXT`, ...) when one exists.

## Testing

- `cargo test` runs unit tests, plus tests that drive a real headless mpv
  core (`vo=null`, `ao=null`); those are skipped when libmpv is missing.
- Tests also check that every preference-driven mpv option is accepted by
  mpv, that the schema matches `Preferences`, and that thumbnails are made
  and cached for a generated clip (when ffmpeg is installed).
- `tools/smoke.sh` runs Patina on a private Xvfb display, sends input with
  xdotool and saves screenshots, for checking the interface end to end. Xvfb
  only has software OpenGL, so it uses software decoding.
- `PATINA_FRAME_DUMP` writes what mpv rendered to a file, which works on a
  real GPU (for example under a nested `kwin_wayland --virtual`) where
  screenshots of the display are not available.
