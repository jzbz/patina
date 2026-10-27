# Developing Patina

Patina is a Rust + Slint media player on libmpv whose goal is feature parity
with IINA on Linux, macOS and Windows. [parity.md](parity.md) is the source
of truth for what is done; check it before calling a feature complete, and
update it when a feature lands. Item IDs there refer to
[iina-inventory.md](iina-inventory.md). [architecture.md](architecture.md)
explains the design.

## Build and verify

- The toolchain is pinned in `rust-toolchain.toml`; rustup installs it on
  first use. libmpv (mpv 0.39 or newer, built with Lua) must be installed
  to run Patina or the mpv-backed tests, but not to build it.
- `cargo test`: unit tests, plus tests against a real headless mpv core.
  Tests that need libmpv or ffmpeg skip without them; set
  `PATINA_TEST_REQUIRE_MPV=1` and `PATINA_TEST_REQUIRE_FFMPEG=1` to make
  them fail instead, as CI does.
- `bundle.yml`, run by hand with a pushed commit's SHA, builds a universal
  Patina.app (with IINA's libmpv, pinned in `packaging/macos-libmpv.txt`)
  and a Windows folder (patina.exe, libmpv-2.dll and vulkan-1.dll, pinned in
  `packaging/windows-deps.env`) as artifacts, for running on machines with
  nothing else installed. `packaging/icons.sh` renders the icons they use.
- After editing `ui/menus.slint`, run `tools/menubar.py` to copy the menus
  into the macOS menu bar.
- CI (`.github/workflows/ci.yml`) runs formatting, clippy, a build and the
  tests on Ubuntu 26.04, macOS and Windows. Its header comments explain each
  choice, including how to bump the Windows libmpv pin.
- `cargo clippy --all-targets -- -D warnings` must stay clean.
- `cargo fmt` after editing Rust.
- `cargo build` also compiles the Slint UI (`build.rs`); Slint errors show
  up as a failed build script.

After touching playback, rendering or the interface, run the player and look
at it. `tools/smoke.sh OUTPUT_DIR NAME "ARGS" STEP...` runs Patina on a
private Xvfb display, sends xdotool input and saves screenshots; keep its
output outside the repository. Xvfb has only software OpenGL, so it decodes
in software. To check the GPU path (hardware decoding, Wayland), run Patina
inside `kwin_wayland --virtual` (in its own `dbus-run-session`, so it cannot
touch a running desktop session) with `PATINA_FRAME_DUMP=frame.ppm`, which
writes what mpv rendered.

Use `PATINA_CONFIG_DIR` and `PATINA_DATA_DIR` for isolated runs,
`--mpv-ao=null` to keep test runs silent, and `--verbose` or
`RUST_LOG=patina=trace` for diagnostics.

## Source map

- `src/main.rs`: command line, backend selection, startup.
- `src/app.rs`: preferences, history, key bindings, windows, file dialogs.
- `src/window.rs`: one player window: events into state, state into the
  UI, Patina actions, OSD, window sizing, picture-in-picture.
- `src/player/`: mpv core startup and options, the event thread,
  observed properties (`props.rs`), and the typed playback state
  (`state.rs`).
- `src/mpv/`: the libmpv binding: runtime-loaded FFI, handle, events,
  nodes, render context.
- `src/video/`: mpv-to-texture rendering inside Slint's rendering notifier,
  the GL function resolver, and the software fallback (`software.rs`).
- `src/macos.rs`: the transparent title bar and its window buttons.
- `src/input.rs`: input.conf parsing and writing, key normalization,
  default bindings.
- `src/settings.rs`: preferences, their mpv options, history, directories.
- `src/preferences/`: the preferences window; `schema.rs` lists every
  setting.
- `src/thumbnails.rs`: seek-bar thumbnails from a headless mpv core.
- `src/ytdl.rs`: what yt-dlp says about an online video, as qualities for
  the menus and the yt-dlp format for a choice.
- `src/downloads.rs`: downloads with yt-dlp, and the Downloads window.
- `src/probe.rs`: playlist durations and tags from a headless mpv core.
- `src/playlist.rs`: playlist selection, multi-row moves, series-prefix
  folding and pasted sources, without mpv.
- `src/subtitle_match.rs`: finding the subtitle files that belong to a
  video.
- `src/history.rs`, `src/inspector.rs`: the playback history and inspector
  windows.
- `src/media_controls/`: MPRIS, SMTC and MediaPlayer integration.
- `src/inhibit.rs`: keeping the screen awake during playback.
- `src/cli.rs`, `src/util.rs`, `src/platform.rs`.
- `ui/`: the Slint interface (`app.slint` is the window; see
  architecture.md); `ui/icons/` are Lucide's SVG icons
  (lucide-static, ISC; `ui/icons/LICENSE`), white for Slint to tint, the
  transport controls filled.

## Rules

- The UI thread never blocks on mpv: use `Player::command`/`set` (async),
  and learn state from observed properties. Add new state by observing the
  property in `player/props.rs` and handling it in `PlaybackState::apply`.
- Never call mpv from the render update callback; it may only request a
  redraw.
- Open files only after the render context exists (`PlayerController::open`
  queues them).
- New preferences are a `Preferences` field with a default plus an entry in
  `src/preferences/schema.rs`; map them to mpv in
  `Preferences::mpv_options` when mpv should see them.
- Keep mpv authoritative for playback state; the window mirrors mpv's
  `fullscreen`, `ontop` and `window-scale`.
- New user-facing actions are Patina actions in `run_action`, named after
  IINA's command when it has one, so the interface, the menu and key
  bindings share them.
- `unsafe` stays in `src/mpv/`, `src/video/` and the operating-system
  bindings (`src/macos.rs`, `src/media_controls/`, `src/inhibit.rs`), each
  block with a `SAFETY` comment.
- Follow IINA's behaviour and defaults unless the platform makes that
  wrong; record the difference as "adapted" in parity.md.
- Patina contains no IINA code, artwork or text (IINA is GPL-3.0). Describe
  and reimplement behaviour; don't copy.
- Generated files (builds, logs, screenshots, test media) stay out of the
  repository.
