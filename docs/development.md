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
- The Jellyfin client's live tests need a Jellyfin server, and skip
  without one. `tools/jellyfin-server.sh` runs a throwaway server on
  127.0.0.1 (x86_64 Linux only) with a small generated library, keeping
  everything it downloads and makes in the folder it is given, here `DIR`,
  which belongs outside the repository:

  ```sh
  tools/jellyfin-server.sh setup DIR    # once: 150 MB of downloads
  eval "$(tools/jellyfin-server.sh start DIR)"
  PATINA_TEST_REQUIRE_JELLYFIN=1 cargo test jellyfin::
  tools/jellyfin-server.sh stop DIR
  ```

  `start` listens on port 18096 unless given another after `DIR`, sets
  the server up the first time, and prints `export` lines for
  `PATINA_TEST_JELLYFIN_URL`, `_USER`, `_PASSWORD` and `_LIBRARY`
  (`generated`, which lets the tests count on that library). With
  `PATINA_TEST_REQUIRE_JELLYFIN=1` a live test that finds no server fails
  rather than skips, as in CI. The same variables pointed at a real server
  and a test account run the live tests there, with `_LIBRARY` unset: they
  need a film nobody has started, and put back everything they change
  except that film's play count. To try the client by hand, sign in to
  `http://127.0.0.1:18096` as `patina`, whose password is in
  `DIR/admin-password`, or as `viewer` (password `viewer-test`).
  `tools/jellyfin-server.sh fixtures DIR src/jellyfin/fixtures` saves the
  running server's answers again as the tests' fixtures. docs/jellyfin.md's
  Tests section says what the tests cover.
- The YouTube section's tests need no network and no yt-dlp, and CI must
  never reach YouTube. To try the section by hand without it, give Patina its
  own `PATINA_CONFIG_DIR` and `PATINA_DATA_DIR` and point the `ytdl-path`
  preference at a stand-in script; its JSON needs `"proxy":""`, or mpv's hook
  applies the proxy to a local stream. Keep real requests to a handful, on
  public channels and generic searches, as an address has a budget and a log
  of a real stream holds its address.
- `release.yml` builds every package: the Flatpak bundles
  (`packaging/video.patina.Patina.yml`), a universal Patina.app (with IINA's
  libmpv, pinned in `packaging/macos-libmpv.txt`), and a Windows folder
  (patina.exe, libmpv-2.dll and vulkan-1.dll, pinned in
  `packaging/windows-deps.env`) with its installer
  (`packaging/windows/patina.iss`). Patina.app and the Windows folder also
  carry yt-dlp and ffmpeg, pinned for both in `packaging/bundled-tools.env`
  (`packaging/macos-tools.sh` and `windows-deps.ps1 -Tools` fetch yt-dlp;
  `packaging/ffmpeg.sh` builds ffmpeg from FFmpeg's source tarball, and
  `packaging/ffmpeg.sh linux <downloads> <work> <out>` builds the same
  configuration on Linux, outside the tree, to try a change to it). Run
  by hand with a pushed commit's SHA (`gh workflow run release.yml --repo
  jzbz/patina --ref master -f ref=<sha>`), it builds and tests them all and
  publishes nothing, which is how to try a change to packaging; never pass
  it a `v*` tag by hand.
  `packaging/icons.sh` renders the icons the packages use.
- After editing `ui/menus.slint`, run `tools/menubar.py` to copy the menus
  into the macOS menu bar. The menus read `MenuPlayback` and `MenuMedia`
  in `ui/globals.slint`, which `open-menu` in `ui/app.slint` fills in from
  `Playback` and `Media` when the menu opens: state a menu item needs goes
  in all three.
- `packaging/media-types.tsv` is the one list of file types. After
  changing it, update the MimeType line of
  `packaging/linux/video.patina.Patina.desktop`; a test fails with the line
  it expects until then.
- CI (`.github/workflows/ci.yml`) runs formatting, clippy, a build and the
  tests on Ubuntu 26.04, macOS and Windows, and on Ubuntu the Jellyfin
  tests against the test server above. Its comments explain each choice,
  including how to bump the Windows libmpv pin; the Jellyfin server's pins
  are in `tools/jellyfin-server.sh`, and CI caches its downloads by them.
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

Use `PATINA_CONFIG_DIR` and `PATINA_DATA_DIR` for isolated runs (a run
with its own settings folder is also its own instance, so it neither hands
its files to the Patina you use nor receives that one's; `--new-instance`
skips the single instance altogether),
`--mpv-ao=null` to keep test runs silent, and `--verbose` or
`RUST_LOG=patina=trace` for diagnostics. `PATINA_OPENSUBTITLES_API` points
online subtitles at a stand-in for the OpenSubtitles API (the URL that
`/subtitles` and the rest hang off). Run inside `dbus-run-session`, Patina
uses whatever Secret Service that session has, so a stand-in keeps a test's
password, or a test server's Jellyfin token, out of your own keyring.

## Releases

The runbook is the header of `.github/workflows/release.yml` and
`checksums.yml`; in short:

1. Before tagging: the version in `Cargo.toml`, and a
   `<release version="X.Y.Z" date="YYYY-MM-DD">` in
   `packaging/linux/video.patina.Patina.metainfo.xml` (the workflow's
   version job checks both); the screenshot URLs there pinned to a commit;
   and live pins in `packaging/windows-deps.env` (zhongfly's builds expire
   after about a month) and `packaging/macos-libmpv.txt`, since release
   builds download them afresh. And yt-dlp in `packaging/bundled-tools.env`
   moved to its newest release, every time: the macOS and Windows packages
   carry it, Patina doesn't update it, and YouTube breaks old ones; and the
   Flatpak manifest's yt-dlp with it. The comment at the top of that file
   says how, and how FFmpeg's source pin moves. The version job fails a tag
   whose commit pins a yt-dlp older than the newest there was when the
   commit was made, and warns on any other build.
2. A signed `vX.Y.Z` tag starts `release.yml`, which attaches the Flatpaks,
   the Windows installer and the portable zip to a draft release.
3. The macOS bundle comes from the run as an unsigned artifact; it is signed
   and notarised by hand with `packaging/macos-sign.sh`, then uploaded to the
   draft.
4. `checksums.yml` hashes every asset into `SHA256SUMS`, which is signed by
   hand; then the draft is published.
5. Afterwards, and only when wanted: the Homebrew cask in jzbz/homebrew-tap
   (`packaging/homebrew/README.md`) and the winget manifests
   (`packaging/winget/README.md`).

The Flatpak manifest builds the release bundles only. It is not a Flathub
submission; one for Flathub would be written for Flathub's repository by
hand. Flathub, winget and the metainfo also want https://patina.video/ (and
its `/privacy/` page) to answer. That page has to say what the code does:
docs/youtube.md's What leaves the machine and the metainfo's content rating
comment are the same statement, and a change to one goes in the others.

## Source map

- `src/main.rs`: command line, the single instance, startup.
- `src/backend.rs`: the Slint platform: Slint's winit backend, with the
  Wayland clipboard in place of its own on Linux and the BSDs.
- `src/clipboard.rs`: the Wayland clipboard and primary selection,
  through smithay-clipboard on winit's connection.
- `src/instance.rs`: one Patina at a time: the lock, the socket or pipe,
  and requests from later launches.
- `src/scheme.rs`: patina:// links and the mpv options they may set.
- `src/formats.rs`: the file types, from `packaging/media-types.tsv`.
- `src/associations.rs`: registering for file types and links on each
  system.
- `src/wayland.rs`: bringing a window to the front with an activation
  token.
- `src/app.rs`: preferences, history, key bindings, windows, file dialogs,
  the Jellyfin and YouTube sessions.
- `src/window.rs`: one player window: events into state, state into the
  UI, Patina actions, OSD, window sizing, picture-in-picture, the window's
  side of Jellyfin playback (the load hook, reports, tracks, quality and
  versions, skipping segments, trickplay previews, lyrics), and of a video
  from the YouTube section (the load hook's options, time seen, the way back
  to the section).
- `src/player/`: mpv core startup and options, the event thread,
  observed properties (`props.rs`), and the typed playback state
  (`state.rs`).
- `src/mpv/`: the libmpv binding: runtime-loaded FFI, handle, events,
  nodes, render context.
- `src/video/`: mpv-to-texture rendering inside Slint's rendering notifier,
  the GL function resolver, and the software fallback (`software.rs`).
- `src/macos.rs`: the transparent title bar and its window buttons, and
  the Apple events that Finder's opens and links arrive as.
- `src/input.rs`: input.conf parsing and writing, key normalization,
  default bindings.
- `src/settings.rs`: preferences, their mpv options, history, directories.
- `src/preferences/`: the preferences window; `schema.rs` lists every
  setting.
- `src/thumbnails.rs`: seek-bar thumbnails from a headless mpv core.
- `src/ytdl.rs`: what yt-dlp says about an online video, as qualities for
  the menus and the yt-dlp format for a choice.
- `src/downloads.rs`: downloads with yt-dlp and from Jellyfin servers, the
  YouTube section's turn in the queue, and the Downloads window.
- `src/opensubtitles.rs`: the OpenSubtitles.com client: hash, search,
  download, sign-in.
- `src/credentials.rs`: passwords and Jellyfin tokens in the system's
  password store.
- `src/jellyfin/`: the Jellyfin client ([jellyfin.md](jellyfin.md)):
  `session.rs` the app-wide state, signing in and the threads requests
  run on; `api.rs` the HTTP client; `models.rs` the server's JSON;
  `auth.rs` addresses and the `Authorization` header; `store.rs`
  `jellyfin.json` and the tokens; `discovery.rs` Scan Network; `images.rs`
  the poster cache; `library.rs` the library view's controller;
  `playback.rs` streams, the load hook, tracks and the reports;
  `download.rs` downloading an item's file;
  `segments.rs` skipping intros and credits; `trickplay.rs` the seek bar's
  previews; `lyrics.rs` a song's lyrics and the line being sung; `tests.rs`
  and `fixtures/` the tests across them.
- `src/youtube/`: the YouTube section, with no account
  ([youtube.md](youtube.md)): `store.rs` `youtube.json`, the lists kept here;
  `feed.rs` building and saving the feed; `rss.rs` channel feeds; `ytdlp.rs` search, listings and details from
  yt-dlp, with its errors by name; `client.rs` requests to YouTube's hosts
  alone; `takeout.rs` reading and writing the lists in Google's export
  format; `session.rs` the app-wide state; `view.rs` the per-window controller
  that fills `ui/youtube-model.slint`'s global, for `ui/youtube.slint` and
  `ui/youtube-card.slint` to draw; `playing.rs` what goes with playing a video
  from it, apart from the window: time seen, where it picks up, its default
  quality; `fixtures/` the tests' answers. The tests need
  no network or real yt-dlp: they use saved answers, a local server and a
  stand-in script.
- `src/probe.rs`: playlist durations and tags from a headless mpv core.
- `src/playlist.rs`: playlist selection, multi-row moves, series-prefix
  folding and pasted sources, without mpv.
- `src/subtitle_match.rs`: finding the subtitle files that belong to a
  video.
- `src/history.rs`, `src/inspector.rs`: the playback history and inspector
  windows.
- `src/media_controls/`: MPRIS, SMTC and MediaPlayer integration.
- `src/inhibit.rs`: keeping the screen awake during playback.
- `src/fonts.rs`: puts the interface's typeface (`Theme.font-family`)
  first among the sans-serif fonts, for the menus Slint draws itself on
  Linux, which inherit no `default-font-family`.
- `src/cli.rs`, `src/util.rs`, `src/platform.rs`.
- `tools/`: `menubar.py` and `smoke.sh` (see above), and
  `jellyfin-server.sh`, the Jellyfin test server, with
  `jellyfin-media.sh`, which makes its library.
- `ui/`: the Slint interface (`app.slint` is the window, `library*.slint`
  the Jellyfin library, `youtube*.slint` the YouTube section; see
  architecture.md); `ui/icons/` are Lucide's
  SVG icons (lucide-static, ISC; `ui/icons/LICENSE`), white for Slint to
  tint, the transport controls filled; `ui/fonts/` is IBM Plex Sans
  (OFL 1.1; `ui/fonts/LICENSE`), the static Regular, Italic, SemiBold and
  Bold files that `theme.slint` imports, which compiles them into Patina.
  A weight or style the interface starts to use needs its file added
  there; otherwise one of these four is drawn in its place.

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
- The same holds for Jellyfin's own clients, which are GPL too: none of
  their code is copied. Nor is any other YouTube client's.
- The interface thread doesn't wait on the network or the keyring either:
  the Jellyfin client asks them through `session::background`,
  `session::load` and `session::keyring`, the reporter and the image
  cache's workers, and drops an answer the user has moved on from.
- A Jellyfin token lives in the keyring, in memory and in the
  `Authorization` header of its own server's requests, and nowhere else:
  not in a URL, a log, a file or any mpv option but the header, which goes
  through `Mpv::set_property_secret` (docs/jellyfin.md, Keeping the token
  secret).
- The YouTube section has no account and never gets one: its yt-dlp calls
  take `--ignore-config` and none of the user's yt-dlp options, the proxy
  aside. No request starts at Patina's startup or by itself in the background.
  Whatever comes from outside is checked, and every address is built from a
  checked id. Search text stays out of logs (docs/youtube.md). A new request,
  or a change to what one carries, goes into that page's What leaves the
  machine, and into the metainfo's comment if it changes that.
- Generated files (builds, logs, screenshots, test media) stay out of the
  repository.
