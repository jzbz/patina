//! One player window: its Slint component, its mpv core, and the glue.

use crate::app::App;
use crate::input::{Binding, KeyCombo};
use crate::mpv::{EndFileReason, Event, Mpv};
use crate::opensubtitles;
use crate::player::state::{
    AudioStatus, PlaybackState, PlaylistEntry, Track, TrackKind, file_name,
};
use crate::player::{self, Player, Prop};
use crate::settings::ResizeWindow;
use crate::subtitle_match;
use crate::thumbnails;
use crate::util::{self, format_delay, format_speed, format_time};
use crate::ytdl;
use crate::{
    Actions, ChapterItem, DeviceItem, Media, OnlineSubs, OnlineSubtitle, Osd, Platform, Playback,
    PlayerWindow, PlaylistItem, Prefs, QualityItem, RecentItem, ShortcutItem, TrackItem,
};
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Reply userdata for `show-text`, whose expanded text becomes an OSD message.
const REPLY_SHOW_TEXT: u64 = 10;
const REPLY_SCREENSHOT: u64 = 11;
/// Reply userdata for what yt-dlp said about the file, from mpv's yt-dlp hook.
const REPLY_YTDL: u64 = 12;

/// Qualities chosen from the Video Quality and Audio Quality menus for one
/// online video: None leaves that part to the preferences.
#[derive(Debug, Clone, Default, PartialEq)]
struct QualityChoice {
    video: Option<String>,
    audio: Option<String>,
    /// The yt-dlp format the two make.
    format: Option<String>,
}

const ASPECTS: [&str; 6] = ["no", "4:3", "16:9", "16:10", "21:9", "5:4"];
const CROPS: [(f64, f64); 6] = [
    (0.0, 0.0),
    (4.0, 3.0),
    (16.0, 9.0),
    (16.0, 10.0),
    (21.0, 9.0),
    (5.0, 4.0),
];
/// The speeds the controller's arrow buttons step through, as in IINA.
const ARROW_SPEEDS: [f64; 11] = [
    0.03125, 0.0625, 0.125, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0,
];
const EQ_BANDS: [u32; 10] = [32, 64, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];
/// How long after a user action property changes still count as caused by it.
const USER_ACTION_WINDOW: Duration = Duration::from_millis(1500);
/// Music mode's width and strip of controls, as in IINA's mini player, and
/// how tall its playlist drawer opens; with less of it than the minimum,
/// the drawer shuts.
const MUSIC_WIDTH: f32 = 300.0;
const MUSIC_PANEL: f32 = 72.0;
const MUSIC_DRAWER: f32 = 300.0;
const MUSIC_DRAWER_MIN: f32 = 200.0;
/// The player window's size when it opens (its preferred size in app.slint).
const DEFAULT_SIZE: slint::LogicalSize = slint::LogicalSize::new(960.0, 600.0);
/// How long a size change waits for Slint to apply new size limits: its own
/// zero-delay timer, which a later one of the same deadline would precede.
const AFTER_LIMITS: Duration = Duration::from_millis(1);

pub struct PlayerController {
    app: Weak<App>,
    window: PlayerWindow,
    player: RefCell<Option<Player>>,
    mpv: Arc<Mpv>,
    state: RefCell<PlaybackState>,
    ready: Cell<bool>,
    pending_open: RefCell<Vec<(Vec<String>, bool)>>,
    /// Bring the window to the front once it exists, with the desktop's
    /// activation token if a launch passed one on.
    pending_raise: RefCell<Option<Option<String>>>,
    /// Picture-in-picture asked for a file that is opening.
    pip_pending: Cell<PipPending>,
    /// A load asked of mpv that it hasn't started yet: the window is no
    /// longer free for another open, though mpv still reports it idle.
    load_asked: Cell<bool>,
    /// The core started with a request's own mpv options.
    own_options: Cell<bool>,
    closed: Cell<bool>,
    osd_timer: Timer,
    last_user_action: Cell<Option<Instant>>,
    loading: Cell<bool>,
    /// Fit the window to the next video size reported by mpv.
    fit_pending: Cell<bool>,
    has_fitted: Cell<bool>,
    crop_index: Cell<usize>,
    /// Counts files started, so media controls can tell tracks apart.
    track: Cell<u64>,
    eq_gains: RefCell<[f32; 10]>,
    eq_timer: Timer,
    drops: RefCell<Vec<String>>,
    drop_timer: Timer,
    wheel: Cell<(f32, f32)>,
    /// Window geometry to restore when leaving picture-in-picture.
    pip_restore: Cell<Option<(slint::LogicalSize, bool)>>,
    /// What music mode puts back when it ends.
    music_restore: RefCell<Option<MusicRestore>>,
    /// Set by a switch made by hand, and cleared by the next one: music
    /// mode doesn't switch by itself meanwhile, as in IINA.
    music_override: Cell<bool>,
    /// How tall the playlist drawer is while it is open.
    music_drawer: Cell<f32>,
    /// Snaps the drawer open or shut once the window stops being resized.
    music_snap_timer: Timer,
    /// The property a user command targeted, for an OSD message even when
    /// the value did not change (volume already at its maximum, say).
    osd_fallback: Cell<Option<Prop>>,
    osd_fallback_timer: Timer,
    /// A size we asked for, re-requested if a stale configure from the
    /// Wayland compositor undoes it: (size, deadline, attempts left).
    wanted_size: Cell<Option<(slint::LogicalSize, Instant, u8)>>,
    /// Counts the sizes asked for, so that a delayed one can tell it came
    /// too late.
    resizes: Cell<u64>,
    /// Seek-bar thumbnails for the current file, filled in as they arrive.
    thumbnails: RefCell<Vec<Option<slint::Image>>>,
    thumbnail_job: RefCell<Option<thumbnails::Job>>,
    thumbnail_path: RefCell<String>,
    /// The playlist rows, kept so file information can update one row.
    playlist_model: Rc<VecModel<PlaylistItem>>,
    /// The video renderer when the window has no OpenGL (None otherwise:
    /// the OpenGL one lives in the window's rendering notifier).
    software_video: RefCell<Option<crate::video::software::SoftwareVideo>>,
    /// The current file as yt-dlp sees it, when it came through yt-dlp.
    online: RefCell<Option<Rc<ytdl::Media>>>,
    /// Qualities chosen from the menus, by the address they were chosen
    /// for; they last as long as the window.
    qualities: RefCell<HashMap<String, QualityChoice>>,
    /// Where the file reloading for a new quality picks up again, and the
    /// message for when it has: (address, seconds), (address, title, label).
    resume_at: RefCell<Option<(String, f64)>>,
    quality_message: RefCell<Option<(String, &'static str, String)>>,
    /// Find Online Subtitles: what each row stands for, and a count that
    /// tells the answer to the latest search or download from older ones.
    online_found: RefCell<Vec<opensubtitles::Found>>,
    online_request: Cell<u64>,
    /// macOS: the transparent title bar has been applied (it can only be
    /// once winit has made the window).
    #[cfg(target_os = "macos")]
    titlebar_styled: Cell<bool>,
    /// Selected playlist rows, by mpv's entry ids.
    playlist_selection: RefCell<crate::playlist::Selection>,
    /// How much of each row's name is a series prefix shared with its
    /// neighbours, in bytes (0 for none).
    series_prefixes: RefCell<Vec<usize>>,
    this: Weak<PlayerController>,
}

impl PlayerController {
    pub fn new(app: &Rc<App>, options: &crate::player::CoreOptions) -> Result<Rc<Self>, String> {
        let window = PlayerWindow::new().map_err(|error| error.to_string())?;
        let (player, events) = Player::start(options)?;
        let mpv = player.mpv().clone();

        let controller = Rc::new_cyclic(|this| Self {
            this: this.clone(),
            osd_fallback: Cell::new(None),
            osd_fallback_timer: Timer::default(),
            wanted_size: Cell::new(None),
            resizes: Cell::new(0),
            thumbnails: RefCell::new(Vec::new()),
            thumbnail_job: RefCell::new(None),
            thumbnail_path: RefCell::new(String::new()),
            playlist_model: Rc::new(VecModel::default()),
            software_video: RefCell::new(None),
            online: RefCell::new(None),
            qualities: RefCell::new(HashMap::new()),
            resume_at: RefCell::new(None),
            quality_message: RefCell::new(None),
            online_found: RefCell::new(Vec::new()),
            online_request: Cell::new(0),
            #[cfg(target_os = "macos")]
            titlebar_styled: Cell::new(false),
            playlist_selection: RefCell::new(Default::default()),
            series_prefixes: RefCell::new(Vec::new()),
            app: Rc::downgrade(app),
            window,
            player: RefCell::new(Some(player)),
            mpv,
            state: RefCell::new(PlaybackState::default()),
            ready: Cell::new(false),
            pending_open: RefCell::new(Vec::new()),
            pending_raise: RefCell::new(None),
            pip_pending: Cell::new(PipPending::No),
            load_asked: Cell::new(false),
            own_options: Cell::new(false),
            closed: Cell::new(false),
            osd_timer: Timer::default(),
            last_user_action: Cell::new(None),
            loading: Cell::new(false),
            fit_pending: Cell::new(false),
            has_fitted: Cell::new(false),
            crop_index: Cell::new(0),
            track: Cell::new(0),
            eq_gains: RefCell::new([0.0; 10]),
            eq_timer: Timer::default(),
            drops: RefCell::new(Vec::new()),
            drop_timer: Timer::default(),
            wheel: Cell::new((0.0, 0.0)),
            pip_restore: Cell::new(None),
            music_restore: RefCell::new(None),
            music_override: Cell::new(false),
            music_drawer: Cell::new(MUSIC_DRAWER),
            music_snap_timer: Timer::default(),
        });

        controller
            .window
            .global::<Media>()
            .set_playlist(ModelRc::from(controller.playlist_model.clone()));
        // Push the initial state so the interface starts consistent with it.
        controller.sync(&Prop::ALL.iter().copied().collect());
        controller.apply_preferences();
        controller.wire_callbacks();
        controller.wire_window_events();
        controller.attach_video()?;
        controller.spawn_event_pump(events);
        controller.refresh_recent();
        controller
            .window
            .show()
            .map_err(|error| error.to_string())?;
        Ok(controller)
    }

    fn app(&self) -> Rc<App> {
        self.app.upgrade().expect("the app outlives its windows")
    }

    fn player(&self) -> std::cell::Ref<'_, Player> {
        std::cell::Ref::map(self.player.borrow(), |player| {
            player.as_ref().expect("player is running")
        })
    }

    fn is_running(&self) -> bool {
        self.player.borrow().is_some()
    }

    pub fn is_playing(&self) -> bool {
        let state = self.state.borrow();
        !self.closed.get() && !state.idle && !state.paused
    }

    pub fn is_idle(&self) -> bool {
        self.state.borrow().idle && self.pending_open.borrow().is_empty() && !self.load_asked.get()
    }

    pub fn set_own_options(&self, own: bool) {
        self.own_options.set(own);
    }

    /// Whether the core started with a request's own mpv options, which
    /// other opens shouldn't inherit.
    pub fn has_own_options(&self) -> bool {
        self.own_options.get()
    }

    /// The file playing now, as mpv was given it.
    pub fn current_path(&self) -> String {
        self.state.borrow().path.clone()
    }

    /// Reads a property for the inspector; the value arrives as a reply
    /// event, which goes to the inspector.
    pub fn request_property(&self, userdata: u64, name: &str) {
        if let Err(error) = self.mpv.get_property_async(userdata, name) {
            log::debug!("{error}");
        }
    }

    /// What the window plays, for the inspector.
    pub fn media_name(&self) -> String {
        let state = self.state.borrow();
        crate::inspector::media_name(&state.path, &state.title)
    }

    /// macOS: IINA's transparent title bar, applied as soon as the window
    /// exists, which is by its first event.
    fn style_titlebar(&self) {
        #[cfg(target_os = "macos")]
        if !self.titlebar_styled.get()
            && self.app().prefs().transparent_titlebar()
            && crate::macos::use_transparent_titlebar(self.window.window())
        {
            self.titlebar_styled.set(true);
        }
    }

    /// The title bar and controller faded in or out. On macOS the window
    /// buttons fade with them, as in IINA (not in fullscreen, where macOS
    /// shows them itself).
    /// Music mode keeps them hidden, as IINA's mini player does.
    fn chrome_visible(&self, visible: bool) {
        #[cfg(target_os = "macos")]
        if self.app().prefs().transparent_titlebar() && !self.window.window().is_fullscreen() {
            crate::macos::set_window_buttons_visible(
                self.window.window(),
                visible && !self.in_music_mode(),
            );
        }
        #[cfg(not(target_os = "macos"))]
        let _ = visible;
    }

    /// Brings the window to the front.
    pub fn raise(&self) {
        self.raise_with(None);
    }

    /// Brings the window to the front, now or once it exists. On Wayland
    /// that takes an activation token from whatever started the request
    /// (winit's focus_window does nothing there).
    pub fn raise_with(&self, token: Option<String>) {
        let window = self.window.window();
        if window.with_winit_window(|_| ()).is_none() {
            // A raise without a token keeps the one already waiting.
            let mut pending = self.pending_raise.borrow_mut();
            if token.is_some() || pending.is_none() {
                *pending = Some(token);
            }
            return;
        }
        window.set_minimized(false);
        window.with_winit_window(|winit| winit.focus_window());
        #[cfg(all(unix, not(target_os = "macos")))]
        if let Some(token) = token {
            crate::wayland::activate(window, &token);
        }
    }

    pub fn enter_fullscreen(&self) {
        if !self.window.window().is_fullscreen() {
            self.run_action("toggle-fullscreen");
        }
    }

    /// Picture-in-picture for the video showing now, if there is one.
    pub fn enter_pip(&self) {
        if !self.window.get_pip() && self.has_video() {
            self.toggle_pip();
        }
    }

    /// Picture-in-picture once the file that is opening shows its video;
    /// forgotten if it ends first (it had no video, or failed to open).
    pub fn pip_when_video(&self) {
        if !self.window.get_pip() {
            self.pip_pending.set(PipPending::Asked);
        }
    }

    pub fn has_video(&self) -> bool {
        self.state.borrow().has_video()
    }

    pub fn window(&self) -> &PlayerWindow {
        &self.window
    }

    /// Runs an mpv command for a dialog or another window.
    pub fn run_mpv_command(&self, args: &[&str]) {
        if self.is_running() {
            self.player().command(args);
        }
    }

    pub fn audio_devices(&self) -> Vec<(String, String)> {
        self.state
            .borrow()
            .audio_devices
            .iter()
            .map(|device| (device.name.clone(), device.description.clone()))
            .collect()
    }

    /// The playlist as (title, path) pairs.
    pub fn playlist_paths(&self) -> Vec<(String, String)> {
        self.state
            .borrow()
            .playlist
            .iter()
            .map(|entry| (entry.label(), entry.filename.clone()))
            .collect()
    }

    // ---- Setup ----------------------------------------------------------

    fn apply_preferences(&self) {
        self.apply_interface_preferences();
        let prefs = self.app().prefs();
        self.window.set_float_on_top(prefs.always_on_top);
        if prefs.always_on_top {
            self.player().set("ontop", true);
        }
    }

    /// Copies the interface preferences into the window; safe to repeat
    /// whenever they change.
    pub fn apply_interface_preferences(&self) {
        let app = self.app();
        let prefs = app.prefs();
        let platform = self.window.global::<Platform>();
        platform.set_macos(cfg!(target_os = "macos"));
        platform.set_windows(cfg!(target_os = "windows"));
        platform.set_custom_titlebar(prefs.custom_titlebar());
        platform.set_overlay_titlebar(prefs.custom_titlebar() || prefs.transparent_titlebar());
        platform.set_version(env!("CARGO_PKG_VERSION").into());

        let ui = self.window.global::<Prefs>();
        ui.set_show_remaining_time(prefs.show_remaining_time);
        ui.set_show_chapter_marks(prefs.show_chapter_marks);
        ui.set_osc_timeout((prefs.osc_timeout.max(0.5) * 1000.0) as i64);
        ui.set_osd_timeout((prefs.osd_timeout.max(0.3) * 1000.0) as i64);
        ui.set_osd_font_size(if prefs.osd_enabled {
            prefs.osd_font_size
        } else {
            0.0
        });
        ui.set_arrow_action(prefs.arrow_action.clone().into());
        ui.set_toolbar_buttons(string_model(&prefs.toolbar_buttons));
        ui.set_single_click(prefs.single_click.clone().into());
        ui.set_double_click(prefs.double_click.clone().into());
        ui.set_right_click(prefs.right_click.clone().into());
        ui.set_middle_click(prefs.middle_click.clone().into());
        ui.set_thumbnail_width(prefs.thumbnail_width as f32);
    }

    /// Sets changed mpv options on this window's running core.
    pub fn apply_mpv_options(&self, options: &[(&'static str, String)]) {
        if !self.is_running() {
            return;
        }
        let player = self.player();
        for (name, value) in options {
            player.set_quiet(name, value.as_str());
        }
    }

    fn attach_video(self: &Rc<Self>) -> Result<(), String> {
        if !crate::video::software::enabled() {
            match crate::video::attach(&self.window, self.mpv.clone(), self.video_hooks()) {
                Ok(()) => return Ok(()),
                // Slint found no OpenGL when it made the window and draws it
                // with its software renderer instead, which has no notifier.
                Err(slint::SetRenderingNotifierError::Unsupported) => {
                    log::warn!("the window is drawn without OpenGL; drawing video in software");
                }
                Err(error) => {
                    return Err(format!("could not attach the video renderer: {error:?}"));
                }
            }
        }
        let (width, height) = self.video_size();
        let video = crate::video::software::SoftwareVideo::start(
            self.mpv.clone(),
            width,
            height,
            self.video_hooks(),
        )?;
        *self.software_video.borrow_mut() = Some(video);
        Ok(())
    }

    fn video_hooks(self: &Rc<Self>) -> crate::video::Hooks {
        let ready = Rc::downgrade(self);
        let frame = self.window.as_weak();
        let target = self.window.as_weak();
        let error = self.window.as_weak();
        crate::video::Hooks {
            set_frame: Box::new(move |image| {
                if let Some(window) = frame.upgrade() {
                    window.global::<Playback>().set_video_frame(image);
                }
            }),
            target_size: Box::new(move || target.upgrade().and_then(|window| video_area(&window))),
            ready: Box::new(move || {
                if let Some(controller) = ready.upgrade() {
                    controller.on_renderer_ready();
                }
            }),
            error: Box::new(move |message| {
                if let Some(window) = error.upgrade() {
                    window.set_error(message.into());
                }
            }),
        }
    }

    /// The size mpv draws at, in physical pixels.
    fn video_size(&self) -> (u32, u32) {
        video_area(&self.window).unwrap_or_else(|| {
            let size = self.window.window().size();
            (size.width, size.height)
        })
    }

    fn resize_software_video(&self) {
        if let Some(video) = &*self.software_video.borrow() {
            let (width, height) = self.video_size();
            video.resize(width, height);
        }
    }

    fn on_renderer_ready(&self) {
        self.ready.set(true);
        self.window.global::<Playback>().set_ready(true);
        let pending: Vec<_> = self.pending_open.borrow_mut().drain(..).collect();
        for (sources, append) in pending {
            self.load(&sources, append);
        }
    }

    fn spawn_event_pump(self: &Rc<Self>, events: async_channel::Receiver<Vec<Event>>) {
        let weak = Rc::downgrade(self);
        let spawned = slint::spawn_local(async move {
            while let Ok(batch) = events.recv().await {
                let Some(controller) = weak.upgrade() else {
                    break;
                };
                controller.handle_events(batch);
            }
        });
        if let Err(error) = spawned {
            log::error!("could not start the event pump: {error}");
        }
    }

    // ---- Opening media --------------------------------------------------

    /// Opens sources, replacing the playlist unless `append` is set.
    pub fn open(&self, sources: Vec<String>, append: bool) {
        if sources.is_empty() {
            return;
        }
        if self.ready.get() {
            self.load(&sources, append);
        } else {
            self.pending_open.borrow_mut().push((sources, append));
        }
    }

    fn load(&self, sources: &[String], append: bool) {
        let player = self.player();
        self.load_asked.set(true);
        if !append {
            player.command_quiet(&["write-watch-later-config"]);
            self.has_fitted.set(false);
        }
        for (index, source) in sources.iter().enumerate() {
            let mode = if append || index > 0 {
                "append-play"
            } else {
                "replace"
            };
            loadfile(&player, source, mode, None);
        }
        if !append {
            player.set("pause", self.app().prefs().pause_when_open);
        }
        drop(player);
        self.app().note_opened(sources);
        self.window.invoke_show_controls();
    }

    // ---- mpv events -----------------------------------------------------

    fn handle_events(&self, batch: Vec<Event>) {
        let mut changed = HashSet::new();
        for event in batch {
            match event {
                Event::PropertyChange {
                    userdata, value, ..
                } => {
                    if log::log_enabled!(log::Level::Trace) && userdata != Prop::TimePos as u64 {
                        log::trace!("{:?} = {value:?}", Prop::from_userdata(userdata));
                    }
                    if let Some(prop) = Prop::from_userdata(userdata)
                        && self.state.borrow_mut().apply(prop, &value)
                    {
                        changed.insert(prop);
                        if prop == Prop::Idle && !self.state.borrow().idle {
                            self.load_asked.set(false);
                        }
                    }
                }
                Event::StartFile { .. } => {
                    if self.pip_pending.get() == PipPending::Asked {
                        self.pip_pending.set(PipPending::Opening);
                    }
                    self.track.set(self.track.get() + 1);
                    self.reset_thumbnails();
                    self.set_online(None);
                    self.loading.set(true);
                    self.crop_index.set(0);
                    self.window.set_error(SharedString::new());
                    let resize = self.app().prefs().resize_window;
                    self.fit_pending.set(match resize {
                        ResizeWindow::Always => true,
                        ResizeWindow::OnOpen => !self.has_fitted.get(),
                        ResizeWindow::Never => false,
                    });
                }
                Event::FileLoaded => {
                    self.loading.set(false);
                    changed.insert(Prop::PausedForCache);
                    self.reapply_audio_filters();
                    let state = self.state.borrow();
                    self.app()
                        .record_history(&state.path, &state.title, state.duration);
                    if util::is_url(&state.path) {
                        self.request_property(
                            REPLY_YTDL,
                            "user-data/mpv/ytdl/json-subprocess-result/stdout",
                        );
                    }
                    let message = self
                        .quality_message
                        .take()
                        .filter(|(path, ..)| *path == state.path);
                    drop(state);
                    if let Some((_, title, label)) = message {
                        self.show_osd(title, &label, None);
                    }
                    if let Some(this) = self.this.upgrade() {
                        this.search_online_automatically();
                    }
                }
                // mpv replaced the entry, with the folder's files when it
                // makes a playlist of them; the file itself plays on.
                Event::EndFile {
                    reason: EndFileReason::Redirect,
                    ..
                } => {}
                Event::EndFile { reason, error, .. } => {
                    self.loading.set(false);
                    self.load_asked.set(false);
                    if self.pip_pending.get() == PipPending::Opening {
                        self.pip_pending.set(PipPending::No);
                    }
                    changed.insert(Prop::PausedForCache);
                    {
                        let state = self.state.borrow();
                        self.app()
                            .update_history(&state.path, state.position, state.duration);
                    }
                    if reason == EndFileReason::Error {
                        let detail = error.map(|error| error.description()).unwrap_or_default();
                        let path = self.state.borrow().path.clone();
                        let name = if path.is_empty() {
                            String::from("the file")
                        } else {
                            file_name(&path)
                        };
                        self.window
                            .set_error(format!("Could not open {name}\n{detail}").into());
                    }
                }
                Event::Seek => {
                    if self.user_acted() {
                        self.osd_seek();
                    }
                }
                Event::PlaybackRestart => {
                    // A seek finished (or playback began): tell the media
                    // controls where playback is now.
                    if let Some(app) = self.app.upgrade()
                        && app.is_active(self)
                    {
                        app.media_position(self.state.borrow().position, true);
                    }
                }
                Event::CommandReply { userdata, result } => match (userdata, result) {
                    (REPLY_SHOW_TEXT, Ok(node)) => {
                        if let Some(text) = node.as_str() {
                            self.show_osd(text, "", None);
                        }
                    }
                    (REPLY_SCREENSHOT, Ok(_)) => {
                        let dir = self.app().prefs().screenshot_dir();
                        self.show_osd("Screenshot Saved", &dir.to_string_lossy(), None);
                    }
                    (player::REPLY_QUIET, _) => {}
                    (_, Err(error)) => {
                        log::warn!("{error}");
                        self.show_osd(&error.description(), "", None);
                    }
                    _ => {}
                },
                Event::SetPropertyReply {
                    result: Err(error), ..
                } => log::warn!("{error}"),
                Event::Shutdown => {
                    // mpv quit by itself (the `quit` command, or a script).
                    self.close();
                    return;
                }
                Event::GetPropertyReply {
                    userdata: REPLY_YTDL,
                    result,
                } => {
                    let media = result
                        .ok()
                        .and_then(|(_, json)| json.as_str().and_then(ytdl::Media::parse))
                        .map(Rc::new);
                    self.set_online(media);
                }
                Event::GetPropertyReply { userdata, result }
                    if userdata >= crate::inspector::REPLY_BASE =>
                {
                    self.app().inspector_reply(userdata, result);
                }
                Event::Hook {
                    userdata: player::HOOK_LOAD,
                    id,
                    ..
                } => self.before_load(id),
                Event::QueueOverflow => log::warn!("mpv's event queue overflowed"),
                _ => {}
            }
        }
        if !changed.is_empty() {
            self.sync(&changed);
        }
    }

    fn user_acted(&self) -> bool {
        self.last_user_action
            .get()
            .is_some_and(|at| at.elapsed() < USER_ACTION_WINDOW)
    }

    fn mark_user_action(&self) {
        self.last_user_action.set(Some(Instant::now()));
    }

    /// Copies changed state into the interface.
    fn sync(&self, changed: &HashSet<Prop>) {
        let state = self.state.borrow();
        let playback = self.window.global::<Playback>();
        let media = self.window.global::<Media>();
        let any = |props: &[Prop]| props.iter().any(|prop| changed.contains(prop));

        // Music mode ends with what played, and back comes the welcome
        // screen; the next file decides afresh.
        if any(&[Prop::Idle]) && state.idle {
            self.music_override.set(false);
            self.leave_music_mode(false);
        }
        if any(&[Prop::TrackList, Prop::Idle]) && !state.idle {
            self.auto_music_mode(&state);
        }
        if any(&[Prop::Idle]) {
            playback.set_has_media(!state.idle);
            if state.idle {
                self.window.set_sidebar_open(false);
            }
        }
        if any(&[Prop::Pause]) {
            playback.set_paused(state.paused);
        }
        if any(&[Prop::Pause, Prop::Idle, Prop::TrackList])
            && let Some(app) = self.app.upgrade()
        {
            // Deferred: the app may be borrowing its window list right now.
            Timer::single_shot(Duration::ZERO, move || app.playback_changed());
        }
        if any(&[Prop::PausedForCache, Prop::CacheBuffering, Prop::Idle]) {
            playback.set_buffering(!state.idle && (state.paused_for_cache || self.loading.get()));
            playback.set_buffering_percent(state.cache_buffering as i32);
        }
        if any(&[Prop::Seekable]) {
            playback.set_seekable(state.seekable);
        }
        if any(&[Prop::TimePos]) {
            playback.set_position(state.position as f32);
            if let Some(app) = self.app.upgrade()
                && app.is_active(self)
            {
                app.media_position(state.position, false);
            }
        }
        if any(&[Prop::Duration]) {
            playback.set_duration(state.duration as f32);
        }
        if any(&[Prop::DemuxerCacheTime]) {
            playback.set_cache_end(state.cache_time as f32);
        }
        if any(&[Prop::Volume, Prop::VolumeMax, Prop::Mute]) {
            playback.set_volume(state.volume as f32);
            playback.set_volume_max(state.volume_max as f32);
            playback.set_muted(state.muted);
        }
        if any(&[Prop::Speed]) {
            playback.set_speed(state.speed as f32);
        }
        if any(&[Prop::MediaTitle, Prop::Path]) {
            // Like IINA, local files are titled by their file name.
            let title = if state.path.is_empty() || util::is_url(&state.path) {
                state.title.clone()
            } else {
                file_name(&state.path)
            };
            playback.set_title(title.into());
            playback.set_path(state.path.clone().into());
        }
        if any(&[Prop::VideoParams, Prop::TrackList, Prop::Idle]) {
            playback.set_has_video(state.has_video() && !state.is_audio_only());
            playback.set_audio_only(state.is_audio_only());
        }
        if any(&[Prop::VideoParams, Prop::TrackList, Prop::Idle, Prop::Vid])
            && let Some(picture) = state.picture_aspect()
        {
            // Music mode keeps its cover to sensible shapes; mpv letterboxes
            // anything beyond them.
            let aspect = picture.unwrap_or(1.0).clamp(0.5, 4.0) as f32;
            let resize = playback.get_has_picture() != picture.is_some()
                || (playback.get_picture_aspect() - aspect).abs() > 0.001;
            playback.set_has_picture(picture.is_some());
            playback.set_picture_aspect(aspect);
            if resize {
                self.resize_music_later();
            }
        }
        if any(&[
            Prop::MediaTitle,
            Prop::Metadata,
            Prop::ChapterList,
            Prop::Chapter,
            Prop::ChapterMetadata,
        ]) {
            let (title, artist, album) = state.music_metadata();
            playback.set_music_title(title.into());
            playback.set_artist(artist.into());
            playback.set_album(album.into());
        }
        if any(&[Prop::ChapterList]) {
            let marks: Vec<f32> = state
                .chapters
                .iter()
                .map(|chapter| chapter.time as f32)
                .collect();
            playback.set_chapter_marks(ModelRc::new(VecModel::from(marks)));
        }
        if any(&[Prop::LoopFile, Prop::LoopPlaylist, Prop::Shuffle]) {
            playback.set_loop_file(state.loop_file);
            playback.set_loop_playlist(state.loop_playlist);
            playback.set_shuffle(state.shuffle);
        }
        if any(&[Prop::AbLoopA, Prop::AbLoopB]) {
            playback.set_ab_a(state.ab_loop_a.map_or(-1.0, |a| a as f32));
            playback.set_ab_b(state.ab_loop_b.map_or(-1.0, |b| b as f32));
        }
        if any(&[Prop::VideoAspectOverride]) {
            playback.set_aspect_index(aspect_index(state.aspect_override));
        }
        if any(&[Prop::VideoCrop, Prop::VideoParams]) {
            playback.set_crop_index(if state.crop.is_empty() {
                0
            } else {
                self.crop_index.get() as i32
            });
        }
        if any(&[Prop::VideoRotate]) {
            playback.set_rotation(state.rotate as i32);
        }
        if any(&[Prop::VideoFilters]) {
            playback.set_flipped(
                state
                    .video_filters
                    .iter()
                    .any(|filter| filter.label == "patina-flip"),
            );
            playback.set_mirrored(
                state
                    .video_filters
                    .iter()
                    .any(|filter| filter.label == "patina-mirror"),
            );
        }
        if any(&[Prop::Deinterlace, Prop::Hwdec, Prop::HwdecCurrent]) {
            playback.set_deinterlace(state.deinterlace);
            playback.set_hwdec(state.hwdec != "no");
            playback.set_hwdec_current(match state.hwdec_current.as_str() {
                "" if state.idle => SharedString::new(),
                "" | "no" => "software".into(),
                other => other.into(),
            });
        }
        if any(&[
            Prop::Brightness,
            Prop::Contrast,
            Prop::Saturation,
            Prop::Gamma,
            Prop::Hue,
        ]) {
            playback.set_brightness(state.brightness as i32);
            playback.set_contrast(state.contrast as i32);
            playback.set_saturation(state.saturation as i32);
            playback.set_gamma(state.gamma as i32);
            playback.set_hue(state.hue as i32);
        }
        if any(&[
            Prop::VideoParams,
            Prop::VideoCodec,
            Prop::EstimatedVfFps,
            Prop::ContainerFps,
        ]) {
            playback.set_video_info(video_info(&state).into());
        }
        if any(&[Prop::AudioParams, Prop::AudioCodecName]) {
            playback.set_audio_info(audio_info(&state).into());
        }
        if any(&[Prop::AudioDelay]) {
            playback.set_audio_delay(state.audio_delay as f32);
        }
        if any(&[
            Prop::SubDelay,
            Prop::SecondarySubDelay,
            Prop::SubScale,
            Prop::SubPos,
            Prop::SubVisibility,
            Prop::SecondarySubVisibility,
        ]) {
            playback.set_sub_delay(state.sub_delay as f32);
            playback.set_secondary_sub_delay(state.secondary_sub_delay as f32);
            playback.set_sub_scale(state.sub_scale as f32);
            playback.set_sub_pos(state.sub_pos as f32);
            playback.set_sub_visible(state.sub_visible);
            playback.set_secondary_sub_visible(state.secondary_sub_visible);
        }

        if any(&[Prop::Duration])
            && !any(&[Prop::Playlist, Prop::PlaylistPos])
            && let Ok(index) = usize::try_from(state.playlist_pos)
            && let Some(entry) = state.playlist.get(index)
            && index < self.playlist_model.row_count()
        {
            self.playlist_model
                .set_row_data(index, self.playlist_item(index, entry));
            self.update_playlist_summary(&state);
        }
        if any(&[Prop::Playlist, Prop::PlaylistPos]) {
            self.rebuild_playlist(&state);
            media.set_playlist_pos(state.playlist_pos as i32);
        }
        if any(&[
            Prop::TrackList,
            Prop::Vid,
            Prop::Aid,
            Prop::Sid,
            Prop::SecondarySid,
        ]) {
            media.set_video_tracks(track_model(&state, TrackKind::Video, false, false));
            media.set_audio_tracks(track_model(&state, TrackKind::Audio, true, false));
            media.set_sub_tracks(track_model(&state, TrackKind::Sub, true, false));
            media.set_secondary_sub_tracks(track_model(&state, TrackKind::Sub, true, true));
            media.set_downloaded_subtitle(self.downloaded_subtitle(&state).is_some());
        }
        if any(&[Prop::ChapterList, Prop::Chapter]) {
            let chapters: Vec<ChapterItem> = state
                .chapters
                .iter()
                .enumerate()
                .map(|(index, chapter)| ChapterItem {
                    title: chapter.title.clone().into(),
                    time: chapter.time as f32,
                    time_text: format_time(chapter.time).into(),
                    current: index as i64 == state.chapter,
                })
                .collect();
            media.set_chapters(ModelRc::new(VecModel::from(chapters)));
            media.set_chapter(state.chapter as i32);
        }
        if any(&[Prop::AudioDeviceList, Prop::AudioDevice]) {
            let devices: Vec<DeviceItem> = state
                .audio_devices
                .iter()
                .map(|device| DeviceItem {
                    name: device.name.clone().into(),
                    description: if device.description.is_empty() {
                        device.name.clone()
                    } else {
                        device.description.clone()
                    }
                    .into(),
                    selected: device.name == state.audio_device,
                })
                .collect();
            media.set_audio_devices(ModelRc::new(VecModel::from(devices)));
        }

        if any(&[
            Prop::Duration,
            Prop::VideoParams,
            Prop::Path,
            Prop::TrackList,
        ]) {
            self.maybe_start_thumbnails(&state);
        }
        if any(&[
            Prop::Idle,
            Prop::Pause,
            Prop::MediaTitle,
            Prop::Path,
            Prop::Metadata,
            Prop::Duration,
            Prop::Speed,
            Prop::Volume,
            Prop::LoopFile,
            Prop::LoopPlaylist,
            Prop::Shuffle,
            Prop::Playlist,
            Prop::PlaylistPos,
            Prop::Seekable,
            Prop::TrackList,
            Prop::ChapterList,
            Prop::Chapter,
            Prop::ChapterMetadata,
        ]) && let Some(app) = self.app.upgrade()
            && app.is_active(self)
        {
            app.publish_now_playing(self.now_playing(&state));
        }

        // mpv window properties, driven by key bindings such as "cycle fullscreen".
        if any(&[Prop::Fullscreen]) && self.window.window().is_fullscreen() != state.fullscreen {
            if self.in_music_mode() && state.fullscreen {
                // Music mode has no full screen, as IINA's mini player has none.
                self.player().set("fullscreen", false);
            } else {
                self.window.window().set_fullscreen(state.fullscreen);
            }
        }
        if any(&[Prop::Ontop]) && self.window.get_float_on_top() != state.ontop {
            self.window.set_float_on_top(state.ontop);
        }
        // Music mode sizes the window to the cover rather than fitting it,
        // and has no picture-in-picture or full screen to start in.
        if any(&[Prop::VideoParams]) && self.in_music_mode() {
            // Leaving music mode sizes the window itself.
            if state.video.is_some() {
                self.fit_pending.set(false);
            }
            drop(state);
            if self.pip_pending.get() == PipPending::Opening {
                self.pip_pending.set(PipPending::No);
            }
            self.osd_for_changes(changed);
            return;
        }
        if any(&[Prop::VideoParams]) && self.fit_pending.get() && state.video.is_some() {
            self.fit_pending.set(false);
            let first = !self.has_fitted.replace(true);
            drop(state);
            let prefs = self.app().prefs();
            let scale = match prefs.window_size.as_str() {
                "fit" => f64::INFINITY,
                other => other.parse().unwrap_or(1.0),
            };
            self.fit_to_video(scale, true);
            // Picture-in-picture asked for wins over full screen on open.
            if self.pip_pending.get() == PipPending::Opening {
                self.pip_pending.set(PipPending::No);
                if !self.window.get_pip() {
                    self.toggle_pip();
                }
            } else if first && prefs.fullscreen_when_open && !self.window.window().is_fullscreen() {
                self.player().set("fullscreen", true);
            }
            self.osd_for_changes(changed);
            return;
        }
        // Without a fit to wait for (the window size preference is Never,
        // or the window has fitted once already).
        if any(&[Prop::VideoParams])
            && state.video.is_some()
            && self.pip_pending.get() == PipPending::Opening
        {
            drop(state);
            self.pip_pending.set(PipPending::No);
            if !self.window.get_pip() {
                self.toggle_pip();
            }
            self.osd_for_changes(changed);
            return;
        }
        if any(&[Prop::WindowScale]) && self.user_acted() {
            let scale = state.window_scale;
            drop(state);
            self.fit_to_video(scale, false);
            self.osd_for_changes(changed);
            return;
        }
        drop(state);
        self.osd_for_changes(changed);
    }

    // ---- On-screen display ---------------------------------------------

    pub fn show_osd(&self, text: &str, detail: &str, progress: Option<f64>) {
        self.osd_fallback.set(None);
        // Music mode shows its state in its own controls instead.
        if self.in_music_mode() {
            return;
        }
        let prefs = self.app().prefs();
        if !prefs.osd_enabled {
            return;
        }
        let osd = self.window.global::<Osd>();
        osd.set_text(text.into());
        osd.set_detail(detail.into());
        osd.set_progress(progress.map_or(-1.0, |p| p.clamp(0.0, 1.0) as f32));
        osd.set_visible(true);
        let window = self.window.as_weak();
        self.osd_timer.start(
            TimerMode::SingleShot,
            Duration::from_secs_f32(prefs.osd_timeout.max(0.3)),
            move || {
                if let Some(window) = window.upgrade() {
                    window.global::<Osd>().set_visible(false);
                }
            },
        );
    }

    fn osd_seek(&self) {
        let state = self.state.borrow();
        let progress = (state.duration > 0.0).then(|| state.position / state.duration);
        let text = format!(
            "{} / {}",
            format_time(state.position),
            format_time(state.duration)
        );
        let detail = state
            .current_chapter()
            .map(|chapter| chapter.title.clone())
            .unwrap_or_default();
        drop(state);
        self.show_osd(&text, &detail, progress);
    }

    /// IINA reports user-visible changes in the OSD; so does Patina, for
    /// changes made shortly after a user action.
    fn osd_for_changes(&self, changed: &HashSet<Prop>) {
        if !self.user_acted() {
            return;
        }
        if self.in_music_mode() {
            if changed.contains(&Prop::Volume) || changed.contains(&Prop::Mute) {
                self.window.invoke_show_volume();
            }
            return;
        }
        let state = self.state.borrow();
        let on_off = |on: bool| if on { "On" } else { "Off" };
        let message: Option<(String, String, Option<f64>)> = if changed.contains(&Prop::Mute) {
            Some((
                if state.muted { "Mute" } else { "Unmute" }.into(),
                String::new(),
                None,
            ))
        } else if changed.contains(&Prop::Volume) {
            Some((
                format!("Volume: {}", state.volume.round()),
                String::new(),
                Some(state.volume / state.volume_max.max(1.0)),
            ))
        } else if changed.contains(&Prop::Pause) && !state.idle {
            Some((
                if state.paused { "Pause" } else { "Resume" }.into(),
                String::new(),
                None,
            ))
        } else if changed.contains(&Prop::Speed) {
            Some((
                format!("Speed: {}", format_speed(state.speed)),
                String::new(),
                None,
            ))
        } else if changed.contains(&Prop::AudioDelay) {
            Some((
                format!("Audio Delay: {}", format_delay(state.audio_delay)),
                String::new(),
                None,
            ))
        } else if changed.contains(&Prop::SubDelay) {
            Some((
                format!("Subtitle Delay: {}", format_delay(state.sub_delay)),
                String::new(),
                None,
            ))
        } else if changed.contains(&Prop::SecondarySubDelay) {
            Some((
                format!(
                    "Secondary Subtitle Delay: {}",
                    format_delay(state.secondary_sub_delay)
                ),
                String::new(),
                None,
            ))
        } else if changed.contains(&Prop::SubScale) {
            Some((
                format!("Subtitle Scale: {}", format_speed(state.sub_scale)),
                String::new(),
                None,
            ))
        } else if changed.contains(&Prop::SubPos) {
            Some((
                format!("Subtitle Position: {}", state.sub_pos.round()),
                String::new(),
                None,
            ))
        } else if changed.contains(&Prop::SubVisibility) {
            Some((
                if state.sub_visible {
                    "Subtitles Shown"
                } else {
                    "Subtitles Hidden"
                }
                .into(),
                String::new(),
                None,
            ))
        } else if changed.contains(&Prop::SecondarySubVisibility) {
            Some((
                if state.secondary_sub_visible {
                    "Secondary Subtitles Shown"
                } else {
                    "Secondary Subtitles Hidden"
                }
                .into(),
                String::new(),
                None,
            ))
        } else if changed.contains(&Prop::Aid) {
            Some(track_osd("Audio", state.selected_track(TrackKind::Audio)))
        } else if changed.contains(&Prop::Sid) {
            Some(track_osd("Subtitle", state.selected_track(TrackKind::Sub)))
        } else if changed.contains(&Prop::SecondarySid) {
            let track = state
                .secondary_sid
                .and_then(|id| state.tracks(TrackKind::Sub).find(|t| t.id == id));
            Some(track_osd("Secondary Subtitle", track))
        } else if changed.contains(&Prop::Vid) {
            Some(track_osd("Video", state.selected_track(TrackKind::Video)))
        } else if changed.contains(&Prop::LoopFile) {
            Some((
                format!("Loop File: {}", on_off(state.loop_file)),
                String::new(),
                None,
            ))
        } else if changed.contains(&Prop::LoopPlaylist) {
            Some((
                format!("Loop Playlist: {}", on_off(state.loop_playlist)),
                String::new(),
                None,
            ))
        } else if changed.contains(&Prop::Shuffle) {
            Some((
                format!("Shuffle: {}", on_off(state.shuffle)),
                String::new(),
                None,
            ))
        } else if changed.contains(&Prop::AbLoopA) || changed.contains(&Prop::AbLoopB) {
            Some(match (state.ab_loop_a, state.ab_loop_b) {
                (Some(a), Some(b)) => (
                    format!("A-B Loop: {} → {}", format_time(a), format_time(b)),
                    String::new(),
                    None,
                ),
                (Some(a), None) => (
                    format!("A-B Loop: A = {}", format_time(a)),
                    String::new(),
                    None,
                ),
                _ => ("A-B Loop: Off".into(), String::new(), None),
            })
        } else if changed.contains(&Prop::Chapter) {
            state.current_chapter().map(|chapter| {
                (
                    format!("Chapter: {}", chapter.title),
                    format_time(chapter.time),
                    None,
                )
            })
        } else if changed.contains(&Prop::VideoRotate) {
            Some((format!("Rotation: {}°", state.rotate), String::new(), None))
        } else if changed.contains(&Prop::VideoAspectOverride) {
            let label = match aspect_index(state.aspect_override) {
                -1 => format!("{:.2}:1", state.aspect_override),
                index => {
                    ["Default", "4:3", "16:9", "16:10", "21:9", "5:4"][index as usize].to_owned()
                }
            };
            Some((format!("Aspect Ratio: {label}"), String::new(), None))
        } else if changed.contains(&Prop::Deinterlace) {
            Some((
                format!("Deinterlace: {}", on_off(state.deinterlace)),
                String::new(),
                None,
            ))
        } else if changed.contains(&Prop::Hwdec) {
            Some((
                format!("Hardware Decoding: {}", on_off(state.hwdec != "no")),
                String::new(),
                None,
            ))
        } else {
            [
                (Prop::Brightness, "Brightness", state.brightness),
                (Prop::Contrast, "Contrast", state.contrast),
                (Prop::Saturation, "Saturation", state.saturation),
                (Prop::Gamma, "Gamma", state.gamma),
                (Prop::Hue, "Hue", state.hue),
            ]
            .into_iter()
            .find(|(prop, ..)| changed.contains(prop))
            .map(|(_, name, value)| {
                (
                    format!("{name}: {value}"),
                    String::new(),
                    Some((value as f64 + 100.0) / 200.0),
                )
            })
        };
        drop(state);
        if let Some((text, detail, progress)) = message {
            self.show_osd(&text, &detail, progress);
        }
    }

    // ---- Interface callbacks -------------------------------------------

    fn wire_callbacks(self: &Rc<Self>) {
        let actions = self.window.global::<Actions>();

        macro_rules! handler {
            ($method:ident, |$this:ident $(, $arg:ident)*| $body:expr) => {{
                let weak = Rc::downgrade(self);
                actions.$method(move |$($arg),*| {
                    if let Some($this) = weak.upgrade() {
                        $body
                    } else {
                        Default::default()
                    }
                });
            }};
        }

        handler!(on_mpv, |this, args| {
            this.mark_user_action();
            let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
            this.run_mpv(&args);
        });
        handler!(on_set, |this, name, value| {
            this.mark_user_action();
            this.player().set(&name, value.as_str());
        });
        handler!(on_patina, |this, action| {
            this.mark_user_action();
            this.run_action(&action);
        });
        handler!(on_seek, |this, time, exact| {
            this.mark_user_action();
            let time = format!("{time:.3}");
            this.player().command_quiet(&[
                "seek",
                &time,
                if exact {
                    "absolute+exact"
                } else {
                    "absolute+keyframes"
                },
            ]);
        });
        handler!(on_key, |this, text, shift, ctrl, alt, meta, _repeat| {
            this.handle_key(&text, shift, ctrl, alt, meta)
        });
        handler!(on_wheel, |this, dx, dy| this.handle_wheel(dx, dy));
        handler!(on_seek_wheel, |this, dx, dy| this
            .wheel_action(dx, dy, "seek", "seek"));
        handler!(on_select_track, |this, kind, id| {
            this.mark_user_action();
            let property = match kind.as_str() {
                "video" => "vid",
                "audio" => "aid",
                _ => "sid",
            };
            if id > 0 {
                this.player().set(property, id as i64);
            } else {
                this.player().set(property, "no");
            }
        });
        handler!(on_select_secondary_sub, |this, id| {
            this.mark_user_action();
            if id > 0 {
                this.player().set("secondary-sid", id as i64);
            } else {
                this.player().set("secondary-sid", "no");
            }
        });
        handler!(on_play_index, |this, index| {
            this.mark_user_action();
            this.player()
                .command(&["playlist-play-index", &index.to_string()]);
            this.player().set("pause", false);
        });
        handler!(on_remove_index, |this, index| {
            this.player()
                .command(&["playlist-remove", &index.to_string()]);
        });
        handler!(on_move_index, |this, from, to| {
            this.move_playlist(from as usize, to as usize)
        });
        handler!(on_select_playlist, |this, index, toggle, extend| {
            this.select_playlist(index as usize, toggle, extend)
        });
        handler!(on_playlist_menu, |this, index| this
            .playlist_menu(index as usize));
        handler!(on_chrome_visible, |this, visible| this
            .chrome_visible(visible));
        handler!(on_select_chapter, |this, index| {
            this.mark_user_action();
            this.player().set("chapter", index as i64);
        });
        handler!(on_open_recent, |this, index| {
            let path = this.app().recent_path(index as usize);
            if let Some(path) = path {
                this.open(vec![path], false);
            }
        });
        handler!(on_set_eq, |this, band, gain| this.set_eq(band, gain));
        handler!(on_activity, |this| this.mark_user_action());
        handler!(on_preview, |this, time| this.show_preview(time as f64));

        let weak = Rc::downgrade(self);
        self.window.on_video_area_changed(move || {
            if let Some(this) = weak.upgrade() {
                this.resize_software_video();
            }
        });

        let weak = Rc::downgrade(self);
        self.window.on_dialog_confirmed(move |kind, text| {
            if let Some(this) = weak.upgrade() {
                this.dialog_confirmed(&kind, text.trim());
            }
        });

        let weak = Rc::downgrade(self);
        self.window.window().on_close_requested(move || {
            if let Some(this) = weak.upgrade() {
                this.close();
            }
            slint::CloseRequestResponse::HideWindow
        });

        let online = self.window.global::<OnlineSubs>();
        let weak = Rc::downgrade(self);
        online.on_search(move |text| {
            if let Some(this) = weak.upgrade() {
                this.search_online(text.to_string(), false);
            }
        });
        let weak = Rc::downgrade(self);
        online.on_download(move |index| {
            if let Some(this) = weak.upgrade() {
                let found = usize::try_from(index)
                    .ok()
                    .and_then(|index| this.online_found.borrow().get(index).cloned());
                if let Some(found) = found {
                    this.download_online(found, false);
                }
            }
        });
    }

    fn wire_window_events(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.window.window().on_winit_window_event(move |_, event| {
            let Some(this) = weak.upgrade() else {
                return EventResult::Propagate;
            };
            this.style_titlebar();
            this.app().start_media_controls(&this);
            // Once the window is first drawn, which is when the desktop
            // shows it: after Slint draws for this event.
            if matches!(event, winit::event::WindowEvent::RedrawRequested) {
                let raise = this.pending_raise.borrow_mut().take();
                if let Some(token) = raise {
                    let weak = Rc::downgrade(&this);
                    slint::Timer::single_shot(Duration::ZERO, move || {
                        if let Some(this) = weak.upgrade() {
                            this.raise_with(token);
                        }
                    });
                }
            }
            match event {
                winit::event::WindowEvent::HoveredFile(_) => this.window.set_drop_active(true),
                winit::event::WindowEvent::HoveredFileCancelled => {
                    this.window.set_drop_active(false)
                }
                winit::event::WindowEvent::DroppedFile(path) => {
                    this.window.set_drop_active(false);
                    this.queue_drop(path.to_string_lossy().into_owned());
                }
                winit::event::WindowEvent::Resized(size) => {
                    this.check_size(*size);
                    // Slint takes the new size after this.
                    if let Some(video) = &*this.software_video.borrow() {
                        let (width, height) =
                            video_area(&this.window).unwrap_or((size.width, size.height));
                        video.resize(width, height);
                    }
                    // winit tells of no end to a resize by hand: take a
                    // moment without one for it.
                    if this.in_music_mode() {
                        let weak = Rc::downgrade(&this);
                        this.music_snap_timer.start(
                            TimerMode::SingleShot,
                            Duration::from_millis(300),
                            move || {
                                if let Some(this) = weak.upgrade() {
                                    this.snap_music_drawer();
                                }
                            },
                        );
                    }
                    // Keep mpv's fullscreen property in step when the window
                    // manager changes the state itself.
                    let fullscreen = this.window.window().is_fullscreen();
                    if fullscreen != this.state.borrow().fullscreen && this.is_running() {
                        this.state.borrow_mut().fullscreen = fullscreen;
                        this.player().set("fullscreen", fullscreen);
                    }
                }
                winit::event::WindowEvent::Occluded(true)
                    if this.app().prefs().pause_when_minimized && this.is_running() =>
                {
                    this.player().set("pause", true);
                }
                winit::event::WindowEvent::Focused(true) => this.app().set_active(&this),
                _ => {}
            }
            EventResult::Propagate
        });
    }

    fn queue_drop(self: &Rc<Self>, path: String) {
        self.drops.borrow_mut().push(path);
        // Files of one drop arrive as separate events; take them together.
        let weak = Rc::downgrade(self);
        self.drop_timer.start(
            TimerMode::SingleShot,
            Duration::from_millis(60),
            move || {
                if let Some(this) = weak.upgrade() {
                    let paths: Vec<String> = this.drops.borrow_mut().drain(..).collect();
                    this.open_dropped(paths);
                }
            },
        );
    }

    fn open_dropped(&self, mut paths: Vec<String>) {
        paths.sort_by(|a, b| natural_cmp(a, b));
        let (subtitles, media): (Vec<_>, Vec<_>) =
            paths.into_iter().partition(|path| is_subtitle(path));
        if !self.state.borrow().idle {
            for subtitle in &subtitles {
                self.player().command(&["sub-add", subtitle, "select"]);
            }
        }
        if media.is_empty() {
            return;
        }
        // Dropped with the playlist in view, files join the playlist after
        // the selected rows rather than replacing what plays. (IINA inserts
        // them where they are dropped; winit doesn't report where that is.)
        let playlist_open =
            self.window.get_sidebar_open() && self.window.get_sidebar_tab() == "playlist";
        if playlist_open && !self.state.borrow().idle {
            self.insert_after_selection(&media);
        } else {
            self.open(media, false);
        }
    }

    // ---- Input ----------------------------------------------------------

    fn handle_key(&self, text: &str, shift: bool, ctrl: bool, alt: bool, meta: bool) -> bool {
        let Some(combo) = KeyCombo::from_event(text, shift, ctrl, alt, meta) else {
            return false;
        };
        let binding = self.app().bindings().get(&combo).cloned();
        let Some(binding) = binding else {
            log::debug!("unbound key {}", combo.label());
            return false;
        };
        self.mark_user_action();
        match binding {
            Binding::Mpv(commands) => {
                for command in commands {
                    self.run_mpv(&command);
                }
            }
            Binding::Patina(action) => self.run_action(&action),
        }
        true
    }

    fn handle_wheel(&self, dx: f32, dy: f32) {
        let prefs = self.app().prefs();
        self.wheel_action(dx, dy, &prefs.vertical_scroll, &prefs.horizontal_scroll);
    }

    /// Scrolling with these actions for its two directions.
    fn wheel_action(&self, dx: f32, dy: f32, vertical: &str, horizontal: &str) {
        // Accumulate pixel deltas into notches of 40 logical pixels, so
        // touchpads and wheels step at a similar pace.
        const NOTCH: f32 = 40.0;
        let (mut ax, mut ay) = self.wheel.get();
        ax += dx;
        ay += dy;
        let steps_y = (ay / NOTCH).trunc();
        let steps_x = (ax / NOTCH).trunc();
        ay -= steps_y * NOTCH;
        ax -= steps_x * NOTCH;
        self.wheel.set((ax, ay));
        self.mark_user_action();
        let prefs = self.app().prefs();
        for (steps, action) in [(steps_y, vertical), (-steps_x, horizontal)] {
            if steps == 0.0 {
                continue;
            }
            match action {
                "volume" => {
                    let delta = format!("{}", steps as f64 * prefs.volume_step);
                    self.player().command_quiet(&["add", "volume", &delta]);
                }
                "speed" => {
                    let factor = format!("{}", 1.1f64.powf(steps as f64));
                    self.player().command_quiet(&["multiply", "speed", &factor]);
                }
                "seek" => {
                    let delta = format!("{}", -steps as f64 * prefs.seek_step / 2.0);
                    self.player()
                        .command_quiet(&["seek", &delta, "relative+keyframes"]);
                }
                _ => {}
            }
        }
    }

    /// Runs an mpv command, handling the ones that concern the window.
    fn run_mpv(&self, args: &[String]) {
        let Some(name) = args.iter().find(|arg| !is_command_prefix(arg)) else {
            return;
        };
        let rest: Vec<&str> = args
            .iter()
            .skip_while(|arg| is_command_prefix(arg))
            .skip(1)
            .map(String::as_str)
            .collect();
        match (name.as_str(), rest.as_slice()) {
            ("quit" | "quit-watch-later", _) => self.close(),
            ("show-progress", _) => self.osd_seek(),
            ("show-text", [text, ..]) => {
                if let Err(error) = self
                    .mpv
                    .command_async(REPLY_SHOW_TEXT, &["expand-text", text])
                {
                    log::warn!("{error}");
                }
            }
            ("screenshot" | "screenshot-to-file", _) => {
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                if let Err(error) = self.mpv.command_async(REPLY_SCREENSHOT, &args) {
                    log::warn!("{error}");
                }
            }
            ("seek", [amount]) if self.app().prefs().seek_mode == "exact" => {
                self.player().command(&["seek", amount, "relative+exact"]);
            }
            ("set", ["window-scale", scale]) => {
                if let Ok(scale) = scale.parse() {
                    self.fit_to_video(scale, false);
                }
            }
            _ => {
                if let ("add" | "set" | "cycle" | "cycle-values" | "multiply", [property, ..]) =
                    (name.as_str(), rest.as_slice())
                {
                    self.expect_osd(property);
                }
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                self.player().command(&args);
            }
        }
    }

    /// Shows the OSD for `property` shortly, unless its change already did.
    fn expect_osd(&self, property: &str) {
        let Some(prop) = Prop::ALL
            .iter()
            .copied()
            .find(|prop| prop.name() == property)
        else {
            return;
        };
        self.osd_fallback.set(Some(prop));
        let this = self.this.clone();
        self.osd_fallback_timer.start(
            TimerMode::SingleShot,
            Duration::from_millis(200),
            move || {
                if let Some(this) = this.upgrade()
                    && let Some(prop) = this.osd_fallback.take()
                {
                    this.osd_for_changes(&HashSet::from([prop]));
                }
            },
        );
    }

    /// Runs a Patina action (from the interface, menus or key bindings).
    pub fn run_action(&self, action: &str) {
        let (name, argument) = action.split_once(' ').unwrap_or((action, ""));
        let argument = argument.trim();
        let app = self.app();
        match name {
            "open-file" => app.open_file_dialog(Some(self), false, false),
            "add-files" => app.open_file_dialog(Some(self), true, false),
            "open-folder" => app.open_file_dialog(Some(self), false, true),
            "open-url" | "add-url" => {
                self.window.set_dialog_text(SharedString::new());
                self.window.set_dialog(name.into());
            }
            "jump-to" => {
                self.window
                    .set_dialog_text(format_time(self.state.borrow().position).into());
                self.window.set_dialog("jump-to".into());
            }
            "about" => self.window.set_dialog("about".into()),
            "show-shortcuts" => {
                let bindings = app.bindings();
                let items: Vec<ShortcutItem> = bindings
                    .iter()
                    .map(|(combo, binding)| ShortcutItem {
                        keys: combo.label().into(),
                        action: binding.describe().into(),
                    })
                    .collect();
                self.window
                    .set_shortcuts(ModelRc::new(VecModel::from(items)));
                self.window.set_dialog("shortcuts".into());
            }
            "new-window" => {
                app.new_window();
            }
            "preferences" => app.open_preferences(),
            "history" => app.open_history(),
            "video-quality" => self.choose_quality(Some(argument), None),
            "audio-quality" => self.choose_quality(None, Some(argument)),
            "download-video" => self.download_video(),
            "downloads" => app.open_downloads(),
            "find-online-subtitles" => {
                if let Some(this) = self.this.upgrade() {
                    this.find_online_subtitles();
                }
            }
            "save-downloaded-subtitle" => {
                if let Some(this) = self.this.upgrade() {
                    this.save_downloaded_subtitle();
                }
            }
            "inspector" => app.open_inspector(),
            "close-window" => self.close(),
            "quit" => app.quit(),
            "minimize" => self.window.window().set_minimized(true),
            // Music mode has no full screen, as IINA's mini player has none.
            "toggle-fullscreen" if self.in_music_mode() => {}
            "toggle-fullscreen" => {
                let fullscreen = !self.window.window().is_fullscreen();
                self.player().set("fullscreen", fullscreen);
            }
            "toggle-music-mode" => self.toggle_music_mode(),
            "music-layout" => self.music_layout(),
            "music-previous" => {
                // The first entry starts over, as in IINA.
                if self.state.borrow().playlist_pos > 0 {
                    self.player().command(&["playlist-prev"]);
                } else {
                    self.player().command(&["seek", "0", "absolute"]);
                }
            }
            "music-next" => self.player().command(&["playlist-next"]),
            "toggle-ontop" => {
                let ontop = !self.window.get_float_on_top();
                self.player().set("ontop", ontop);
            }
            "toggle-pip" => self.toggle_pip(),
            "toolbar" => match argument {
                "pip" => self.toggle_pip(),
                "playlist" => self.window.invoke_toggle_sidebar("playlist".into()),
                "settings" => self.window.invoke_toggle_sidebar("settings".into()),
                "subtitles" => self.window.invoke_toggle_sidebar("subtitles".into()),
                "fullscreen" => self.run_action("toggle-fullscreen"),
                "screenshot" => self.run_action("screenshot"),
                "music" => self.toggle_music_mode(),
                other => log::warn!("unknown toolbar button {other}"),
            },
            "toggle-sidebar" => self.window.invoke_toggle_sidebar(argument.into()),
            "video-panel" => self.window.invoke_toggle_sidebar("video".into()),
            "audio-panel" => self.window.invoke_toggle_sidebar("audio".into()),
            "sub-panel" => self.window.invoke_toggle_sidebar("subtitles".into()),
            "playlist-panel" => self.window.invoke_toggle_sidebar("playlist".into()),
            "chapter-panel" => self.window.invoke_toggle_sidebar("chapters".into()),
            "osc-left" | "osc-right" => self.arrow_button(name == "osc-left"),
            "window-scale" => {
                if let Ok(scale) = argument.parse() {
                    self.fit_to_video(scale, false);
                }
            }
            "fit-to-screen" => self.fit_to_video(f64::INFINITY, false),
            "bigger-window" => self.scale_window(1.2),
            "smaller-window" => self.scale_window(1.0 / 1.2),
            "screenshot" => {
                let mode = match argument {
                    "video" => "video",
                    "subtitles" => "subtitles",
                    _ if app.prefs().screenshot_subtitles => "subtitles",
                    _ => "video",
                };
                self.run_mpv(&["screenshot".into(), mode.into()]);
            }
            "open-screenshot-folder" => {
                let dir = app.prefs().screenshot_dir();
                let _ = std::fs::create_dir_all(&dir);
                crate::platform::open_path(&dir);
            }
            "reveal-file" => {
                let path = self.state.borrow().path.clone();
                if !path.is_empty() && !util::is_url(&path) {
                    crate::platform::reveal(std::path::Path::new(&path));
                }
            }
            "open-website" => crate::platform::open_url(crate::WEBSITE),
            "save-playlist" => app.save_playlist_dialog(self),
            "clear-playlist" => self.player().command(&["playlist-clear"]),
            "playlist-select-all" => self.playlist_select_all(),
            "playlist-play-selected" => {
                if let Some((index, _)) = self.selected_entries().first() {
                    self.mark_user_action();
                    self.player()
                        .command(&["playlist-play-index", &index.to_string()]);
                    self.player().set("pause", false);
                }
            }
            "playlist-play-next" => self.playlist_play_next(),
            "playlist-play-new-window" => {
                let paths = self.reopenable(&self.selected_entries());
                if !paths.is_empty()
                    && let Some(window) = app.new_window()
                {
                    window.open(paths, false);
                }
            }
            "playlist-remove" => self.playlist_remove(&self.selected_entries()),
            "playlist-copy" => self.playlist_copy(false),
            "playlist-cut" => self.playlist_copy(true),
            "playlist-paste" => {
                let sources = app
                    .clipboard_text()
                    .map(|text: String| crate::playlist::pasted_sources(&text))
                    .unwrap_or_default();
                if !sources.is_empty() {
                    self.insert_after_selection(&sources);
                }
            }
            "playlist-add-subtitle" => {
                if let [(_, entry)] = self.selected_entries().as_slice() {
                    app.add_subtitles_dialog(self, &entry.filename);
                }
            }
            "playlist-wrong-subtitle" => {
                if let [(_, entry)] = self.selected_entries().as_slice() {
                    self.toggle_wrong_subtitle(&entry.filename);
                }
            }
            "playlist-open-in-browser" => {
                // Web pages only: a playlist from the network may name any
                // URL, and the system opens other kinds with other apps.
                for (_, entry) in self.selected_entries() {
                    if util::is_web(&entry.filename) {
                        crate::platform::open_url(&entry.filename);
                    }
                }
            }
            "playlist-reveal" => {
                let first = self
                    .selected_entries()
                    .into_iter()
                    .find(|(_, entry)| entry.is_local());
                if let Some((_, entry)) = first {
                    crate::platform::reveal(std::path::Path::new(&entry.filename));
                }
            }
            "playlist-trash" => {
                let local: Vec<(usize, PlaylistEntry)> = self
                    .selected_entries()
                    .into_iter()
                    .filter(|(_, entry)| entry.is_local())
                    .collect();
                self.move_to_trash(&local);
            }
            "delete-current-file" => {
                let state = self.state.borrow();
                let current = usize::try_from(state.playlist_pos)
                    .ok()
                    .and_then(|index| Some((index, state.playlist.get(index)?.clone())))
                    .filter(|(_, entry)| entry.is_local());
                drop(state);
                if let Some(current) = current {
                    self.move_to_trash(&[current]);
                }
            }
            "clear-recent" => app.clear_history(),
            "load-subtitles" => app.load_track_dialog(self, TrackKind::Sub),
            "load-audio" => app.load_track_dialog(self, TrackKind::Audio),
            "toggle-loop-file" => {
                self.player()
                    .command(&["cycle-values", "loop-file", "inf", "no"])
            }
            "toggle-loop-playlist" => {
                self.player()
                    .command(&["cycle-values", "loop-playlist", "inf", "no"])
            }
            "toggle-shuffle" => {
                let shuffle = !self.state.borrow().shuffle;
                self.player().set("shuffle", shuffle);
                self.player().command(&[if shuffle {
                    "playlist-shuffle"
                } else {
                    "playlist-unshuffle"
                }]);
            }
            "toggle-flip" => self
                .player()
                .command(&["vf", "toggle", "@patina-flip:vflip"]),
            "toggle-mirror" => self
                .player()
                .command(&["vf", "toggle", "@patina-mirror:hflip"]),
            "aspect" => {
                if let Some(aspect) = argument
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| ASPECTS.get(index))
                {
                    self.player().set("video-aspect-override", *aspect);
                }
            }
            "crop" => {
                if let Ok(index) = argument.parse::<usize>() {
                    self.set_crop(index);
                }
            }
            "hwdec" => {
                // Like IINA's switch: this player only, back to the preferred
                // decoder when turned on.
                let preferred = match app.prefs().hwdec.as_str() {
                    "no" => "auto-safe".to_owned(),
                    other => other.to_owned(),
                };
                let value = if argument == "off" {
                    "no".to_owned()
                } else {
                    preferred
                };
                self.player().set("hwdec", value.as_str());
            }
            "save-ui-prefs" => {
                let remaining = self.window.global::<Prefs>().get_show_remaining_time();
                app.update_prefs(|prefs| prefs.show_remaining_time = remaining);
            }
            other => log::warn!("unknown action: {other}"),
        }
    }

    fn dialog_confirmed(&self, kind: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        match kind {
            "open-url" => self.open(vec![text.to_owned()], false),
            "add-url" => self.open(vec![text.to_owned()], true),
            "jump-to" => match util::parse_time(text) {
                Some(util::TimeSpec::Seconds(seconds)) => {
                    self.mark_user_action();
                    self.player()
                        .command(&["seek", &seconds.to_string(), "absolute+exact"]);
                }
                Some(util::TimeSpec::Percent(percent)) => {
                    self.mark_user_action();
                    self.player().command(&[
                        "seek",
                        &percent.to_string(),
                        "absolute-percent+exact",
                    ]);
                }
                None => self.show_osd("Not a valid time", text, None),
            },
            _ => {}
        }
    }

    /// The controller's ◀◀ and ▶▶ buttons.
    fn arrow_button(&self, left: bool) {
        let action = self.app().prefs().arrow_action.clone();
        match action.as_str() {
            "playlist" => self.player().command(&[if left {
                "playlist-prev"
            } else {
                "playlist-next"
            }]),
            "seek" => self
                .player()
                .command(&["seek", if left { "-10" } else { "10" }]),
            _ => {
                let speed = self.state.borrow().speed;
                let next = if left {
                    if speed > 1.0 {
                        1.0
                    } else {
                        ARROW_SPEEDS
                            .iter()
                            .rev()
                            .copied()
                            .find(|s| *s < speed - 1e-6)
                            .unwrap_or(ARROW_SPEEDS[0])
                    }
                } else if speed < 1.0 {
                    1.0
                } else {
                    ARROW_SPEEDS
                        .iter()
                        .copied()
                        .find(|s| *s > speed + 1e-6)
                        .unwrap_or(ARROW_SPEEDS[10])
                };
                self.player().set("speed", next);
                self.player().set("pause", false);
            }
        }
    }

    fn set_crop(&self, index: usize) {
        let Some((w, h)) = CROPS.get(index).copied() else {
            return;
        };
        self.crop_index.set(index);
        let video = self.state.borrow().video.clone();
        let (Some(video), true) = (video, index > 0) else {
            self.player().set("video-crop", "");
            self.window.global::<Playback>().set_crop_index(0);
            return;
        };
        let target = w / h;
        let display = if video.display_height > 0 {
            video.display_width as f64 / video.display_height as f64
        } else {
            video.width as f64 / video.height as f64
        };
        let (cw, ch) = if display > target {
            (
                (video.width as f64 * target / display).round() as i64,
                video.height,
            )
        } else {
            (
                video.width,
                (video.height as f64 * display / target).round() as i64,
            )
        };
        let crop = format!(
            "{cw}x{ch}+{}+{}",
            (video.width - cw) / 2,
            (video.height - ch) / 2
        );
        self.player().set("video-crop", crop.as_str());
        self.window
            .global::<Playback>()
            .set_crop_index(index as i32);
        let label = ["None", "4:3", "16:9", "16:10", "21:9", "5:4"][index];
        self.show_osd(&format!("Crop: {label}"), "", None);
    }

    fn set_eq(self: &Rc<Self>, band: i32, gain: f32) {
        {
            let mut gains = self.eq_gains.borrow_mut();
            match usize::try_from(band) {
                Ok(band) if band < gains.len() => gains[band] = gain,
                _ => *gains = [0.0; 10],
            }
            let playback = self.window.global::<Playback>();
            playback.set_eq_gains(ModelRc::new(VecModel::from(gains.to_vec())));
        }
        // Rebuilding the filter chain interrupts audio briefly; wait for the
        // slider to settle.
        let weak = Rc::downgrade(self);
        self.eq_timer.start(
            TimerMode::SingleShot,
            Duration::from_millis(120),
            move || {
                if let Some(this) = weak.upgrade() {
                    this.reapply_audio_filters();
                }
            },
        );
    }

    fn reapply_audio_filters(&self) {
        let filter = eq_filter(&self.eq_gains.borrow());
        let has_eq = self
            .state
            .borrow()
            .audio_filters
            .iter()
            .any(|filter| filter.label == "patina-eq");
        if has_eq {
            self.player().command_quiet(&["af", "remove", "@patina-eq"]);
        }
        if let Some(filter) = filter {
            self.player().command(&["af", "add", &filter]);
        }
    }

    // ---- Window sizing --------------------------------------------------

    fn monitor_logical_size(&self) -> Option<(f64, f64)> {
        self.window
            .window()
            .with_winit_window(|winit| {
                let monitor = winit.current_monitor()?;
                let size = monitor.size().to_logical::<f64>(monitor.scale_factor());
                Some((size.width, size.height))
            })
            .flatten()
    }

    /// Resizes the window to the video size times `scale`, limited to the
    /// screen. An infinite scale fills the screen.
    fn fit_to_video(&self, scale: f64, initial: bool) {
        let window = self.window.window();
        if self.in_music_mode() {
            return;
        }
        if window.is_fullscreen() || window.is_maximized() {
            log::debug!("not fitting the window: it is fullscreen or maximized");
            return;
        }
        let Some(video) = self.state.borrow().video.clone() else {
            return;
        };
        let (w, h) = video.oriented_size();
        if w <= 0 || h <= 0 {
            return;
        }
        let (mut width, mut height) = (w as f64 * scale, h as f64 * scale);
        if let Some((screen_w, screen_h)) = self.monitor_logical_size() {
            // Leave room for panels; an initial fit uses at most 85% of the
            // screen, an explicit "fit to screen" all of it.
            let limit = if initial { 0.85 } else { 1.0 };
            let factor = (screen_w * limit / width).min(screen_h * limit / height);
            if factor < 1.0 || scale.is_infinite() {
                width *= factor;
                height *= factor;
            }
        } else if scale.is_infinite() {
            return;
        }
        let min = (360.0, 220.0);
        if width < min.0 || height < min.1 {
            let factor = (min.0 / width).max(min.1 / height);
            width *= factor;
            height *= factor;
        }
        log::debug!("fitting the window to {width:.0}×{height:.0} for {w}×{h} video at {scale}×");
        self.resize_window(slint::LogicalSize::new(width as f32, height as f32));
    }

    fn resize_window(&self, size: slint::LogicalSize) {
        self.resizes.set(self.resizes.get() + 1);
        let window = self.window.window();
        window.set_size(size);
        // Wayland takes a size at once, and reports nothing to check.
        if self.has_size(size) {
            self.wanted_size.set(None);
            return;
        }
        self.wanted_size
            .set(Some((size, Instant::now() + Duration::from_secs(2), 3)));
        // A window manager that held to the old size limits sends no
        // Resized for check_size to retry on.
        let this = self.this.clone();
        Timer::single_shot(Duration::from_millis(150), move || {
            let Some(this) = this.upgrade() else {
                return;
            };
            if let Some((wanted, deadline, attempts)) = this.wanted_size.get()
                && wanted == size
                && attempts > 0
                && !this.has_size(size)
            {
                this.wanted_size.set(Some((size, deadline, attempts - 1)));
                log::debug!("re-requesting window size {size:?}");
                this.window.window().set_size(size);
            }
        });
    }

    /// Whether the window is `size` already, give or take a pixel or two.
    fn has_size(&self, size: slint::LogicalSize) -> bool {
        let window = self.window.window();
        let wanted = size.to_physical(window.scale_factor());
        let actual = window.size();
        actual.width.abs_diff(wanted.width) <= 2 && actual.height.abs_diff(wanted.height) <= 2
    }

    /// Re-requests a wanted size that a compositor configure overrode.
    fn check_size(&self, actual: winit::dpi::PhysicalSize<u32>) {
        let Some((size, deadline, attempts)) = self.wanted_size.get() else {
            return;
        };
        let scale = self.window.window().scale_factor();
        let wanted = size.to_physical(scale);
        let close = |a: u32, b: u32| a.abs_diff(b) <= 2;
        if close(actual.width, wanted.width) && close(actual.height, wanted.height) {
            self.wanted_size.set(None);
            return;
        }
        if Instant::now() > deadline
            || attempts == 0
            || self.window.window().is_fullscreen()
            || self.window.window().is_maximized()
        {
            self.wanted_size.set(None);
            return;
        }
        self.wanted_size.set(Some((size, deadline, attempts - 1)));
        let this = self.this.clone();
        Timer::single_shot(Duration::from_millis(30), move || {
            // Unless another size has been asked for since.
            if let Some(this) = this.upgrade()
                && this
                    .wanted_size
                    .get()
                    .is_some_and(|(wanted, ..)| wanted == size)
            {
                log::debug!("re-requesting window size {size:?}");
                this.window.window().set_size(size);
            }
        });
    }

    fn scale_window(&self, factor: f32) {
        let window = self.window.window();
        if self.in_music_mode() {
            return;
        }
        let size = window.size().to_logical(window.scale_factor());
        self.resize_window(slint::LogicalSize::new(
            size.width * factor,
            size.height * factor,
        ));
    }

    /// A small always-on-top window, Patina's stand-in for macOS's
    /// picture-in-picture on platforms without a system PiP.
    fn toggle_pip(&self) {
        let window = self.window.window();
        // Not in music mode, as in IINA.
        if self.in_music_mode() {
            return;
        }
        if let Some((size, ontop)) = self.pip_restore.take() {
            self.window.set_pip(false);
            self.resize_window(size);
            self.player().set("ontop", ontop);
            return;
        }
        if !self.state.borrow().has_video() {
            return;
        }
        if window.is_fullscreen() {
            self.player().set("fullscreen", false);
        }
        let size = window.size().to_logical(window.scale_factor());
        self.pip_restore
            .set(Some((size, self.window.get_float_on_top())));
        self.window.set_pip(true);
        self.window.set_sidebar_open(false);
        self.player().set("ontop", true);
        let (w, h) = self
            .state
            .borrow()
            .video
            .as_ref()
            .map(|v| v.oriented_size())
            .unwrap_or((16, 9));
        let width = 400.0f32;
        self.resize_window(slint::LogicalSize::new(
            width,
            width * h as f32 / w.max(1) as f32,
        ));
    }

    // ---- Music mode -----------------------------------------------------

    pub fn in_music_mode(&self) -> bool {
        self.window.get_music_mode()
    }

    /// Whether the computer, but not the screen, is all that need stay
    /// awake for this window (the allow-screensaver-for-audio preference).
    pub fn plays_music(&self) -> bool {
        self.in_music_mode() || self.state.borrow().is_audio_only()
    }

    /// A switch by hand: it turns the automatic one off until the next
    /// switch by hand, as in IINA.
    /// Not with nothing to play, as the menu has it: IINA's mini player
    /// belongs to a player.
    fn toggle_music_mode(&self) {
        if self.in_music_mode() {
            self.leave_music_mode(true);
        } else if !self.is_idle() {
            self.enter_music_mode(true);
        }
    }

    /// Music mode asked for with what this window opens (--music-mode): a
    /// switch by hand, as IINA's, so a video stays in it.
    pub fn start_music_mode(&self) {
        self.enter_music_mode(false);
        self.music_override.set(true);
    }

    /// The automatic switch, by what the file turned out to be: in for
    /// music, out for video and streams, as IINA's.
    fn auto_music_mode(&self, state: &PlaybackState) {
        if self.music_override.get() || !self.app().prefs().auto_switch_to_music_mode {
            return;
        }
        // Picture-in-picture asked for a file with no picture at all.
        if self.pip_pending.get() != PipPending::No
            && !state.tracks.is_empty()
            && state.tracks(TrackKind::Video).next().is_none()
        {
            self.pip_pending.set(PipPending::No);
        }
        match state.audio_status() {
            // Never out of full screen or picture-in-picture.
            AudioStatus::Audio
                if !self.in_music_mode()
                    && !self.window.window().is_fullscreen()
                    && !self.window.get_pip()
                    && self.pip_pending.get() == PipPending::No =>
            {
                self.enter_music_mode(false);
            }
            AudioStatus::NotAudio if self.in_music_mode() => self.leave_music_mode(false),
            _ => {}
        }
    }

    pub fn reset_music_override(&self) {
        self.music_override.set(false);
    }

    fn enter_music_mode(&self, manual: bool) {
        if self.in_music_mode() {
            return;
        }
        if manual {
            self.music_override.set(!self.music_override.get());
        }
        // What to go back to, read before leaving picture-in-picture, full
        // screen or maximized: the window's size changes only later.
        let window = self.window.window();
        let maximized = window.is_maximized();
        let size = match self.pip_restore.get() {
            Some((size, _)) => Some(size),
            None if window.is_fullscreen()
                || maximized
                || window.with_winit_window(|_| ()).is_none() =>
            {
                None
            }
            None => Some(window.size().to_logical(window.scale_factor())),
        };
        *self.music_restore.borrow_mut() = Some(MusicRestore {
            size,
            maximized,
            sidebar_open: self.window.get_sidebar_open(),
            sidebar_tab: self.window.get_sidebar_tab(),
        });
        if self.window.get_pip() {
            self.toggle_pip();
        }
        self.pip_pending.set(PipPending::No);
        if window.is_fullscreen() {
            window.set_fullscreen(false);
            if self.is_running() {
                self.player().set("fullscreen", false);
            }
        }
        if maximized {
            window.set_maximized(false);
        }
        let prefs = self.app().prefs();
        self.window.set_music_mode(true);
        self.window.set_music_art(prefs.music_mode_show_album_art);
        if !matches!(
            self.window.get_sidebar_tab().as_str(),
            "playlist" | "chapters"
        ) {
            self.window.set_sidebar_tab("playlist".into());
        }
        self.music_drawer.set(MUSIC_DRAWER);
        self.window.set_sidebar_open(prefs.music_mode_show_playlist);
        self.window.global::<Osd>().set_visible(false);
        self.chrome_visible(false);
        log::debug!("entering music mode");
        self.music_mode_changed();
        self.resize_music_later();
    }

    fn leave_music_mode(&self, manual: bool) {
        if !self.in_music_mode() {
            return;
        }
        if manual {
            self.music_override.set(!self.music_override.get());
        }
        self.music_snap_timer.stop();
        self.window.set_music_mode(false);
        let restore = self.music_restore.borrow_mut().take();
        let restore = restore.unwrap_or_else(|| MusicRestore {
            size: None,
            maximized: false,
            sidebar_open: false,
            sidebar_tab: self.window.get_sidebar_tab(),
        });
        self.window.set_sidebar_tab(restore.sidebar_tab);
        self.window.set_sidebar_open(restore.sidebar_open);
        self.chrome_visible(true);
        self.window.invoke_show_controls();
        log::debug!("leaving music mode");
        self.music_mode_changed();
        // Once Slint has lifted music mode's size limits, unless the window
        // was sized since (fitted to the video that ended music mode).
        // Without a size from before, the video's, or the window's first.
        let this = self.this.clone();
        let resizes = self.resizes.get();
        let (size, maximized) = (restore.size, restore.maximized);
        Timer::single_shot(AFTER_LIMITS, move || {
            let Some(this) = this
                .upgrade()
                .filter(|this| !this.in_music_mode() && this.resizes.get() == resizes)
            else {
                return;
            };
            match size {
                _ if maximized => this.window.window().set_maximized(true),
                Some(size) => this.resize_window(size),
                None if this.state.borrow().video.is_some() => this.fit_to_video(1.0, true),
                None => this.resize_window(DEFAULT_SIZE),
            }
        });
    }

    /// What follows the mode: the playlist's rows (their tags may be for
    /// music mode only) and how awake the computer is kept.
    fn music_mode_changed(&self) {
        self.rebuild_playlist(&self.state.borrow());
        if let Some(app) = self.app.upgrade() {
            Timer::single_shot(Duration::ZERO, move || app.playback_changed());
        }
    }

    /// The drawer opened or shut, or the cover was shown or hidden: the
    /// window follows, and the choice is remembered, as in IINA.
    fn music_layout(&self) {
        if !self.in_music_mode() {
            return;
        }
        let playlist = self.window.get_sidebar_open();
        let art = self.window.get_music_art();
        if !playlist {
            self.music_drawer.set(MUSIC_DRAWER);
        }
        self.app().update_prefs(|prefs| {
            prefs.music_mode_show_playlist = playlist;
            prefs.music_mode_show_album_art = art;
        });
        self.resize_music_later();
    }

    /// Sizes the window to music mode's cover, controls and drawer, and
    /// while a dialog shows, tall enough for it.
    fn resize_music(&self) {
        if !self.in_music_mode() || self.window.window().is_fullscreen() {
            return;
        }
        let drawer = if self.window.get_sidebar_open() {
            self.music_drawer.get()
        } else {
            0.0
        };
        let dialog = match self.window.get_dialog().as_str() {
            "" => 0.0,
            "shortcuts" | "online-subtitles" => 540.0,
            "about" => 340.0,
            _ => 260.0,
        };
        let height = (self.window.get_music_art_height() + MUSIC_PANEL + drawer).max(dialog);
        self.resize_window(slint::LogicalSize::new(MUSIC_WIDTH, height));
    }

    /// resize_music, once Slint has applied size limits that just changed
    /// (a window manager would hold the window to the old ones).
    fn resize_music_later(&self) {
        let this = self.this.clone();
        Timer::single_shot(AFTER_LIMITS, move || {
            if let Some(this) = this.upgrade() {
                this.resize_music();
            }
        });
    }

    /// Once a resize by hand ends, as IINA does: made tall enough, the
    /// drawer opens at that height; made too short, it shuts.
    fn snap_music_drawer(&self) {
        let window = self.window.window();
        let resizing = self
            .wanted_size
            .get()
            .is_some_and(|(_, deadline, _)| Instant::now() < deadline);
        if !self.in_music_mode()
            || resizing
            || window.is_fullscreen()
            || window.is_maximized()
            || !self.window.get_dialog().is_empty()
        {
            return;
        }
        // A tiling window manager keeps its own size, which isn't the user
        // asking for the drawer.
        let size = window.size().to_logical(window.scale_factor());
        if (size.width - MUSIC_WIDTH).abs() > 2.0 {
            return;
        }
        let height = size.height;
        let extra = height - (self.window.get_music_art_height() + MUSIC_PANEL);
        if extra >= MUSIC_DRAWER_MIN {
            self.music_drawer.set(extra);
            self.window.set_sidebar_open(true);
        } else if self.window.get_sidebar_open() {
            self.window.set_sidebar_open(false);
        } else if extra.abs() > 1.0 {
            self.resize_music();
        }
    }

    // ---- System media controls ------------------------------------------

    /// This window's state as the system media controls show it.
    fn now_playing(&self, state: &PlaybackState) -> crate::media_controls::NowPlaying {
        use crate::media_controls::{LoopMode, NowPlaying, Status};
        if state.idle {
            return NowPlaying::default();
        }
        let meta = |keys: &[&str]| {
            state
                .metadata
                .iter()
                .find(|(key, _)| keys.iter().any(|wanted| key.eq_ignore_ascii_case(wanted)))
                .map(|(_, value)| value.clone())
                .unwrap_or_default()
        };
        // Streams and music are known by their title, which for an album in
        // one file follows its chapters; local videos by name.
        let audio = state.is_audio_only();
        let (music_title, music_artist, music_album) = state.music_metadata();
        let titled = util::is_url(&state.path) || audio;
        let title = if audio && !music_title.is_empty() {
            music_title
        } else if titled && !state.title.is_empty() {
            state.title.clone()
        } else {
            file_name(&state.path)
        };
        let url = if util::is_url(&state.path) {
            state.path.clone()
        } else {
            crate::platform::file_uri(std::path::Path::new(&state.path))
        };
        let count = state.playlist.len() as i64;
        NowPlaying {
            status: if state.paused {
                Status::Paused
            } else {
                Status::Playing
            },
            track: self.track.get(),
            title,
            artist: if audio {
                music_artist
            } else {
                meta(&["artist", "album_artist", "performer"])
            },
            album: if audio { music_album } else { meta(&["album"]) },
            url,
            art_url: String::new(),
            duration: state.duration,
            rate: state.speed,
            volume: state.volume / 100.0,
            loop_mode: if state.loop_file {
                LoopMode::Track
            } else if state.loop_playlist {
                LoopMode::Playlist
            } else {
                LoopMode::None
            },
            shuffle: state.shuffle,
            can_next: state.playlist_pos + 1 < count || (state.loop_playlist && count > 1),
            can_previous: state.playlist_pos > 0,
            can_seek: state.seekable,
            audio_only: state.is_audio_only(),
        }
    }

    pub fn publish_now_playing(&self) {
        if let Some(app) = self.app.upgrade() {
            let now = self.now_playing(&self.state.borrow());
            app.publish_now_playing(now);
        }
    }

    /// A media key or a request from the desktop's player controls. Keys go
    /// through their key bindings when the user has bound them, as in IINA.
    pub fn media_command(&self, command: crate::media_controls::Command) {
        use crate::media_controls::{Command, LoopMode};
        if !self.is_running() {
            return;
        }
        self.mark_user_action();
        let key = match &command {
            Command::Play => Some("PLAYONLY"),
            Command::Pause => Some("PAUSEONLY"),
            Command::PlayPause => Some("PLAYPAUSE"),
            Command::Stop => Some("STOP"),
            Command::Next => Some("NEXT"),
            Command::Previous => Some("PREV"),
            _ => None,
        };
        let binding = key
            .and_then(KeyCombo::parse)
            .and_then(|combo| self.app().bindings().get(&combo).cloned());
        if let Some(binding) = binding {
            match binding {
                Binding::Mpv(commands) => commands.iter().for_each(|command| self.run_mpv(command)),
                Binding::Patina(action) => self.run_action(&action),
            }
            return;
        }
        let player = self.player();
        match command {
            Command::Play => player.set("pause", false),
            Command::Pause => player.set("pause", true),
            Command::PlayPause => player.command(&["cycle", "pause"]),
            Command::Stop => player.command(&["stop"]),
            Command::Next => player.command(&["playlist-next"]),
            Command::Previous => player.command(&["playlist-prev"]),
            Command::Seek(seconds) => {
                player.command(&["seek", &format!("{seconds:.3}"), "relative"])
            }
            Command::SetPosition(seconds) => {
                player.command(&["seek", &format!("{seconds:.3}"), "absolute+exact"])
            }
            Command::SetVolume(volume) => player.set("volume", (volume * 100.0).round()),
            Command::SetRate(rate) => player.set("speed", rate.clamp(0.03125, 32.0)),
            Command::SetLoop(mode) => {
                player.set(
                    "loop-file",
                    if mode == LoopMode::Track { "inf" } else { "no" },
                );
                player.set(
                    "loop-playlist",
                    if mode == LoopMode::Playlist {
                        "inf"
                    } else {
                        "no"
                    },
                );
            }
            Command::SetShuffle(shuffle) => {
                drop(player);
                if shuffle != self.state.borrow().shuffle {
                    self.run_action("toggle-shuffle");
                }
            }
            Command::Raise => {
                drop(player);
                let window = self.window.window();
                window.set_minimized(false);
                let _ = window.show();
                window.with_winit_window(|winit| winit.focus_window());
            }
            Command::OpenUri(_) | Command::Quit => {}
        }
    }

    // ---- Playlist ------------------------------------------------------

    /// Rebuilds every playlist row, after the playlist itself changed.
    pub fn refresh_playlist(&self) {
        self.rebuild_playlist(&self.state.borrow());
    }

    fn rebuild_playlist(&self, state: &PlaybackState) {
        let labels: Vec<String> = state.playlist.iter().map(PlaylistEntry::label).collect();
        let names: Vec<&str> = labels.iter().map(String::as_str).collect();
        *self.series_prefixes.borrow_mut() = crate::playlist::series_prefixes(&names);
        self.playlist_selection
            .borrow_mut()
            .retain(&playlist_order(state));
        let items: Vec<PlaylistItem> = state
            .playlist
            .iter()
            .enumerate()
            .map(|(index, entry)| self.playlist_item(index, entry))
            .collect();
        self.playlist_model.set_vec(items);
        self.update_playlist_summary(state);
        self.update_selection_state(state);
    }

    /// A playlist row: the name (or an audio file's tags), its folder, its
    /// length once probed, and how far it was watched.
    fn playlist_item(&self, index: usize, entry: &PlaylistEntry) -> PlaylistItem {
        let app = self.app();
        let probe = (!entry.from_network)
            .then(|| app.probe(&entry.filename))
            .flatten();
        let state = self.state.borrow();
        let duration = if entry.current && state.duration > 0.0 {
            Some(state.duration)
        } else {
            probe
                .as_ref()
                .map(|probe| probe.duration)
                .filter(|d| *d > 0.0)
        };
        let folder = util::parent_directory(&entry.filename);
        let prefs = app.prefs();
        let tags = probe
            .as_ref()
            .filter(|probe| probe.audio_only && !probe.title.is_empty())
            .filter(|_| {
                prefs.playlist_show_metadata
                    && (self.in_music_mode() || !prefs.playlist_show_metadata_in_music_mode)
            });
        let (title, short_title, detail) = match tags {
            Some(probe) if probe.artist.is_empty() => (probe.title.clone(), String::new(), folder),
            Some(probe) => (probe.title.clone(), String::new(), probe.artist.clone()),
            None => {
                let label = entry.label();
                let prefix = self
                    .series_prefixes
                    .borrow()
                    .get(index)
                    .copied()
                    .unwrap_or(0);
                let short = label
                    .get(prefix..)
                    .filter(|_| prefix > 0)
                    .unwrap_or_default()
                    .to_owned();
                (label, short, folder)
            }
        };
        PlaylistItem {
            title: title.into(),
            short_title: short_title.into(),
            detail: detail.into(),
            duration: duration.map(format_time).unwrap_or_default().into(),
            current: entry.current,
            selected: self.playlist_selection.borrow().ids.contains(&entry.id),
            progress: app.watched(&entry.filename).map_or(-1.0, |p| p as f32),
        }
    }

    fn update_playlist_summary(&self, state: &PlaybackState) {
        let app = self.app();
        let count = state.playlist.len();
        let selected = self
            .playlist_selection
            .borrow()
            .indices(&playlist_order(state))
            .len();
        let durations: Vec<Option<f64>> = state
            .playlist
            .iter()
            .map(|entry| {
                if entry.current && state.duration > 0.0 {
                    Some(state.duration)
                } else if entry.from_network {
                    None
                } else {
                    app.probe(&entry.filename).map(|probe| probe.duration)
                }
            })
            .collect();
        let items = match count {
            0 => "Empty".to_owned(),
            1 => "1 item".to_owned(),
            n => format!("{n} items"),
        };
        // IINA shows how many rows are selected, or else the total once
        // every length is known.
        let summary = if selected > 0 {
            format!("{selected} of {count} selected")
        } else if count > 0 && durations.iter().all(Option::is_some) {
            let total: f64 = durations.iter().flatten().sum();
            format!("{items} · {}", format_time(total))
        } else {
            items
        };
        self.window
            .global::<Media>()
            .set_playlist_summary(summary.into());
    }

    /// File information for `path` arrived; refresh its rows.
    pub fn probe_ready(&self, path: &str) {
        let state = self.state.borrow();
        let mut found = false;
        for (index, entry) in state.playlist.iter().enumerate() {
            if entry.filename == path && index < self.playlist_model.row_count() {
                let item = self.playlist_item(index, entry);
                self.playlist_model.set_row_data(index, item);
                found = true;
            }
        }
        if found {
            self.update_playlist_summary(&state);
        }
    }

    /// The selected rows, in playlist order.
    fn selected_entries(&self) -> Vec<(usize, PlaylistEntry)> {
        let state = self.state.borrow();
        self.playlist_selection
            .borrow()
            .indices(&playlist_order(&state))
            .into_iter()
            .filter_map(|index| Some((index, state.playlist.get(index)?.clone())))
            .collect()
    }

    fn select_playlist(&self, index: usize, toggle: bool, extend: bool) {
        let state = self.state.borrow();
        self.playlist_selection
            .borrow_mut()
            .click(&playlist_order(&state), index, toggle, extend);
        self.selection_changed(&state);
    }

    /// A right click on row `index`: the menu acts on the selection, or on
    /// that row alone when it isn't part of it.
    fn playlist_menu(&self, index: usize) {
        let state = self.state.borrow();
        self.playlist_selection
            .borrow_mut()
            .for_menu(&playlist_order(&state), index);
        self.selection_changed(&state);
    }

    fn playlist_select_all(&self) {
        let state = self.state.borrow();
        self.playlist_selection
            .borrow_mut()
            .select_all(&playlist_order(&state));
        self.selection_changed(&state);
    }

    fn selection_changed(&self, state: &PlaybackState) {
        let ids = self.playlist_selection.borrow().ids.clone();
        for (index, entry) in state.playlist.iter().enumerate() {
            if let Some(mut row) = self.playlist_model.row_data(index) {
                let selected = ids.contains(&entry.id);
                if row.selected != selected {
                    row.selected = selected;
                    self.playlist_model.set_row_data(index, row);
                }
            }
        }
        self.update_playlist_summary(state);
        self.update_selection_state(state);
    }

    /// What the row menu offers, for the rows it will act on.
    fn update_selection_state(&self, state: &PlaybackState) {
        let indices = self
            .playlist_selection
            .borrow()
            .indices(&playlist_order(state));
        let entries: Vec<&PlaylistEntry> = indices
            .iter()
            .filter_map(|&index| state.playlist.get(index))
            .collect();
        let media = self.window.global::<Media>();
        media.set_playlist_selected(entries.len() as i32);
        media.set_selection_local(entries.iter().any(|entry| entry.is_local()));
        media.set_selection_urls(entries.iter().any(|entry| util::is_url(&entry.filename)));
        let single = match entries.as_slice() {
            [entry] if entry.is_local() && subtitle_match::takes_subtitles(&entry.filename) => {
                Some(*entry)
            }
            _ => None,
        };
        media.set_selection_subtitles(single.is_some());
        media.set_wrong_subtitle(
            single.is_some_and(|entry| self.app().subtitle_choice(&entry.filename).wrong),
        );
    }

    /// Moves entry `from` to before row `to`, taking the rest of the
    /// selection along when it is part of it.
    fn move_playlist(&self, from: usize, to: usize) {
        let state = self.state.borrow();
        let order = playlist_order(&state);
        let Some(&id) = order.get(from) else {
            return;
        };
        let selection = &self.playlist_selection.borrow().ids;
        let moving: HashSet<i64> = if selection.contains(&id) && selection.len() > 1 {
            selection.clone()
        } else {
            [id].into()
        };
        let desired = crate::playlist::moved(&order, &moving, to);
        drop(state);
        self.reorder(&order, &desired);
    }

    fn reorder(&self, current: &[i64], desired: &[i64]) {
        let player = self.player();
        for (from, to) in crate::playlist::moves(current, desired) {
            player.command(&["playlist-move", &from.to_string(), &to.to_string()]);
        }
    }

    /// Moves the selected rows to just after the one playing, as IINA's
    /// Play Next does.
    fn playlist_play_next(&self) {
        let state = self.state.borrow();
        let order = playlist_order(&state);
        let current = usize::try_from(state.playlist_pos).ok();
        let current_id = current.and_then(|index| order.get(index).copied());
        let moving: HashSet<i64> = self
            .playlist_selection
            .borrow()
            .ids
            .iter()
            .copied()
            .filter(|id| Some(*id) != current_id)
            .collect();
        let target = current.map_or(0, |index| index + 1);
        let desired = crate::playlist::moved(&order, &moving, target);
        drop(state);
        self.reorder(&order, &desired);
    }

    fn playlist_remove(&self, entries: &[(usize, PlaylistEntry)]) {
        let player = self.player();
        // From the end, so the indices of the rest stay valid.
        for (index, _) in entries.iter().rev() {
            player.command(&["playlist-remove", &index.to_string()]);
        }
    }

    /// Copies the selected rows' paths and URLs, one per line; `cut` also
    /// removes the rows.
    fn playlist_copy(&self, cut: bool) {
        let entries = self.selected_entries();
        if entries.is_empty() {
            return;
        }
        // Pasted, they open as the user's own.
        let text = self.reopenable(&entries).join("\n");
        self.app().set_clipboard_text(text);
        if cut {
            self.playlist_remove(&entries);
        }
    }

    /// The entries that may open again elsewhere (see
    /// PlaylistEntry::may_reopen). Those of a link's playlist stay a link's
    /// wherever they open, so that playlists they name are screened too.
    fn reopenable(&self, entries: &[(usize, PlaylistEntry)]) -> Vec<String> {
        let links = self.app().link_sources();
        let paths: Vec<String> = entries
            .iter()
            .filter(|(_, entry)| entry.may_reopen(&links))
            .map(|(_, entry)| entry.filename.clone())
            .collect();
        let linked: Vec<String> = entries
            .iter()
            .filter(|(_, entry)| crate::scheme::from_link(&links, &entry.playlist))
            .map(|(_, entry)| entry.filename.clone())
            .filter(|path| paths.contains(path))
            .collect();
        crate::scheme::note_link_sources(&links, &linked);
        paths
    }

    /// Adds sources to the playlist right after the last selected row, or
    /// at its end when nothing is selected.
    fn insert_after_selection(&self, sources: &[String]) {
        let at = self.selected_entries().last().map(|(index, _)| index + 1);
        let player = self.player();
        for (offset, source) in sources.iter().enumerate() {
            match at {
                Some(at) => loadfile(&player, source, "insert-at", Some(at + offset)),
                None => loadfile(&player, source, "append", None),
            }
        }
    }

    /// Removes local files from the playlist and moves them to the trash,
    /// as IINA's Delete does.
    fn move_to_trash(&self, entries: &[(usize, PlaylistEntry)]) {
        if entries.is_empty() {
            return;
        }
        self.playlist_remove(entries);
        let paths: Vec<String> = entries
            .iter()
            .map(|(_, entry)| entry.filename.clone())
            .collect();
        let detail = match paths.as_slice() {
            [path] => file_name(path),
            paths => format!("{} files", paths.len()),
        };
        crate::platform::move_to_trash(paths);
        self.show_osd("Moved to Trash", &detail, None);
    }

    /// Turns a file's matched subtitles down, as IINA's Wrong Subtitle
    /// does, or back on.
    fn toggle_wrong_subtitle(&self, path: &str) {
        let app = self.app();
        let wrong = !app.subtitle_choice(path).wrong;
        app.update_subtitle_choice(path, |choice| choice.wrong = wrong);
        let state = self.state.borrow();
        if state.path == path {
            if wrong {
                // Drop the files loaded automatically; keep the user's.
                let added = app.subtitle_choice(path).added;
                for track in state.tracks(TrackKind::Sub) {
                    if track.external && !added.contains(&track.external_filename) {
                        self.player()
                            .command(&["sub-remove", &track.id.to_string()]);
                    }
                }
            } else if app.prefs().sub_auto == "smart" {
                // Find them again; reading the folder happens off this thread.
                let languages = sub_languages(&app.prefs());
                let mpv = self.mpv.clone();
                let video = path.to_owned();
                let spawned =
                    std::thread::Builder::new()
                        .name("subtitles".into())
                        .spawn(move || {
                            for (index, file) in subtitle_match::matches(&video, &languages)
                                .iter()
                                .enumerate()
                            {
                                let flag = if index == 0 { "select" } else { "auto" };
                                if let Err(error) = mpv.command(&["sub-add", file, flag]) {
                                    log::debug!("{error}");
                                }
                            }
                        });
                if let Err(error) = spawned {
                    log::warn!("could not match subtitles: {error}");
                }
            }
        }
        self.update_selection_state(&state);
        let message = if wrong {
            "Matched Subtitles Off"
        } else {
            "Matched Subtitles On"
        };
        self.show_osd(message, &file_name(path), None);
    }

    // ---- Subtitle matching ----------------------------------------------

    /// mpv is about to open a file. With Patina's subtitle matching on
    /// (mpv's own `sub-auto` is off then), the subtitle files that belong
    /// to it are found on a thread of their own and handed to mpv first.
    fn before_load(&self, id: u64) {
        let prefs = self.app().prefs();
        let languages = (prefs.sub_auto == "smart").then(|| sub_languages(&prefs));
        let choices = self.app().subtitle_choices();
        let qualities = self.qualities.borrow().clone();
        let resume_at = self.resume_at.borrow_mut().take();
        let links = self.app().link_sources();
        let mpv = self.mpv.clone();
        let Some(hook) = self.player.borrow().as_ref().map(Player::hook) else {
            return;
        };
        let spawned = std::thread::Builder::new().name("subtitles".into()).spawn({
            let hook = hook.clone();
            move || {
                crate::scheme::screen_playlist_entry(&mpv, &links);
                apply_quality(&mpv, &qualities, resume_at);
                subtitle_match::load_for_current_file(&mpv, languages.as_deref(), &choices);
                hook.resume(&mpv, id);
            }
        });
        if let Err(error) = spawned {
            log::warn!("could not match subtitles: {error}");
            hook.resume(&self.mpv, id);
        }
    }

    // ---- Online video ---------------------------------------------------

    /// Fills the Video Quality and Audio Quality menus for what yt-dlp said
    /// about the file (or empties them).
    fn set_online(&self, media: Option<Rc<ytdl::Media>>) {
        let globals = self.window.global::<Media>();
        let path = self.state.borrow().path.clone();
        let choice = self
            .qualities
            .borrow()
            .get(&path)
            .cloned()
            .unwrap_or_default();
        let item = |key: &str, title: String, selected: bool| QualityItem {
            key: key.into(),
            title: title.into(),
            selected,
        };
        let automatic = |playing: Option<&str>| match playing {
            Some(playing) => format!("Automatic ({playing})"),
            None => "Automatic".to_owned(),
        };
        let (video, audio) = match &media {
            Some(media) => {
                let mut video = Vec::new();
                if !media.video.is_empty() {
                    let playing = choice
                        .video
                        .is_none()
                        .then(|| media.playing_video.as_deref())
                        .flatten()
                        .and_then(|key| media.video_label(key));
                    video.push(item("auto", automatic(playing), choice.video.is_none()));
                    video.extend(media.video.iter().map(|quality| {
                        item(
                            &quality.key,
                            quality.label.clone(),
                            choice.video.as_deref() == Some(quality.key.as_str()),
                        )
                    }));
                }
                let mut audio = Vec::new();
                if !media.audio.is_empty() {
                    let playing = choice
                        .audio
                        .is_none()
                        .then(|| media.playing_audio.as_deref())
                        .flatten()
                        .and_then(|id| media.audio_label(id));
                    audio.push(item("auto", automatic(playing), choice.audio.is_none()));
                    audio.extend(media.audio.iter().map(|format| {
                        item(
                            &format.id,
                            format.label.clone(),
                            choice.audio.as_deref() == Some(format.id.as_str()),
                        )
                    }));
                }
                (video, audio)
            }
            None => (Vec::new(), Vec::new()),
        };
        globals.set_video_qualities(ModelRc::new(VecModel::from(video)));
        globals.set_audio_qualities(ModelRc::new(VecModel::from(audio)));
        globals.set_online(media.is_some());
        *self.online.borrow_mut() = media;
    }

    /// A choice from the Video Quality or Audio Quality menu ("auto" to
    /// leave it to the preferences): the file loads again with it, where it
    /// was.
    fn choose_quality(&self, video: Option<&str>, audio: Option<&str>) {
        let Some(media) = self.online.borrow().clone() else {
            return;
        };
        let (path, position) = {
            let state = self.state.borrow();
            (state.path.clone(), state.position)
        };
        let pick = |key: &str| (key != "auto").then(|| key.to_owned());
        let mut choices = self.qualities.borrow_mut();
        let old = choices.get(&path).cloned().unwrap_or_default();
        let mut choice = old.clone();
        if let Some(key) = video {
            choice.video = pick(key);
        }
        if let Some(id) = audio {
            choice.audio = pick(id);
        }
        choice.format = (choice.video.is_some() || choice.audio.is_some()).then(|| {
            media.format(
                choice.video.as_deref(),
                choice.audio.as_deref(),
                &self.app().prefs().ytdl_video_filter(),
            )
        });
        if choice == old {
            return;
        }
        let (title, label) = match (video, audio) {
            (Some(_), _) => (
                "Video Quality",
                choice
                    .video
                    .as_deref()
                    .and_then(|key| media.video_label(key))
                    .unwrap_or("Automatic"),
            ),
            _ => (
                "Audio Quality",
                choice
                    .audio
                    .as_deref()
                    .and_then(|id| media.audio_label(id))
                    .unwrap_or("Automatic"),
            ),
        };
        let label = label.to_owned();
        if choice == QualityChoice::default() {
            choices.remove(&path);
        } else {
            choices.insert(path.clone(), choice);
        }
        drop(choices);
        *self.resume_at.borrow_mut() = Some((path.clone(), position));
        *self.quality_message.borrow_mut() = Some((path, title, label.clone()));
        self.player().command(&["playlist-play-index", "current"]);
        // The reload changes tracks, which isn't news.
        self.last_user_action.set(None);
        self.show_osd(title, &format!("{label}…"), None);
    }

    /// Downloads what is playing, in the quality it plays in.
    fn download_video(&self) {
        let Some(media) = self.online.borrow().clone() else {
            return;
        };
        let request = crate::downloads::Request {
            url: if media.url.is_empty() {
                self.state.borrow().path.clone()
            } else {
                media.url.clone()
            },
            title: media.title.clone(),
            format: crate::downloads::download_format(media.format_id.as_deref()),
        };
        if self.app().download(request) {
            self.show_osd("Downloading", &media.title, None);
        }
    }

    // ---- Online subtitles -----------------------------------------------

    /// Subtitles › Find Online Subtitles: the dialog, searching at once for
    /// what plays.
    fn find_online_subtitles(self: &Rc<Self>) {
        let text = self.online_search_text();
        let online = self.window.global::<OnlineSubs>();
        online.set_query(text.clone().into());
        online.set_results(ModelRc::default());
        online.set_selected(-1);
        self.online_found.borrow_mut().clear();
        self.window.set_dialog("online-subtitles".into());
        self.search_online(text, false);
    }

    /// What to search for: a local file's name, or a stream's title.
    fn online_search_text(&self) -> String {
        let state = self.state.borrow();
        if state.path.is_empty() || util::is_url(&state.path) {
            return state.title.clone();
        }
        std::path::Path::new(&state.path)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| state.title.clone())
    }

    /// IINA's automatic search: when a local video of 20 minutes or more
    /// has no subtitles a moment after it opens (Patina's own matching has
    /// had its turn), a subtitle made for the very file is loaded.
    fn search_online_automatically(self: &Rc<Self>) {
        let prefs = self.app().prefs();
        if !prefs.online_sub_auto || prefs.opensubtitles_api_key.trim().is_empty() {
            return;
        }
        let path = self.state.borrow().path.clone();
        if path.is_empty() || util::is_url(&path) {
            return;
        }
        let weak = Rc::downgrade(self);
        Timer::single_shot(Duration::from_secs(1), move || {
            let Some(this) = weak.upgrade() else {
                return;
            };
            let wanted = {
                let state = this.state.borrow();
                state.path == path
                    && state.duration >= 20.0 * 60.0
                    && state.tracks(TrackKind::Sub).next().is_none()
            };
            if wanted {
                let text = this.online_search_text();
                this.search_online(text, true);
            }
        });
    }

    /// Searches OpenSubtitles on a thread of its own; the dialog shows what
    /// comes back, unless this is the automatic search.
    fn search_online(self: &Rc<Self>, text: String, automatic: bool) {
        let prefs = self.app().prefs();
        let (key, username) = (
            prefs.opensubtitles_api_key.clone(),
            prefs.opensubtitles_username.clone(),
        );
        let languages = sub_languages(&prefs);
        let path = self.state.borrow().path.clone();
        let request = self.online_request.get() + 1;
        self.online_request.set(request);
        if !automatic {
            let online = self.window.global::<OnlineSubs>();
            online.set_busy(true);
            online.set_failed(false);
            online.set_status("Searching…".into());
            online.set_results(ModelRc::default());
            online.set_selected(-1);
            self.online_found.borrow_mut().clear();
        }
        let local = (!path.is_empty() && !util::is_url(&path)).then(|| path.clone());
        let (sender, receiver) = async_channel::bounded(1);
        let spawned = std::thread::Builder::new()
            .name("opensubtitles".into())
            .spawn(move || {
                let result = opensubtitles::shared(&key, &username).and_then(|client| {
                    let hash = local.and_then(|path| {
                        opensubtitles::hash(std::path::Path::new(&path)).unwrap_or_else(|error| {
                            log::debug!("no OpenSubtitles hash for {path}: {error}");
                            None
                        })
                    });
                    client.search(&opensubtitles::Query {
                        languages,
                        hash,
                        text,
                    })
                });
                let _ = sender.send_blocking(result);
            });
        if let Err(error) = spawned {
            log::warn!("could not search for subtitles: {error}");
            return;
        }
        let weak = Rc::downgrade(self);
        let spawned = slint::spawn_local(async move {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let Some(this) = weak.upgrade() else {
                return;
            };
            if this.online_request.get() != request {
                return;
            }
            if automatic {
                // Only one made for this very file, and only if it still plays.
                let exact = result
                    .ok()
                    .and_then(|found| found.into_iter().find(|found| found.exact));
                if let Some(exact) = exact
                    && this.state.borrow().path == path
                {
                    this.download_online(exact, true);
                }
                return;
            }
            let online = this.window.global::<OnlineSubs>();
            online.set_busy(false);
            match result {
                Ok(found) => {
                    let rows: Vec<OnlineSubtitle> = found
                        .iter()
                        .map(|found| OnlineSubtitle {
                            title: found.file_name.clone().into(),
                            detail: describe_online(found).into(),
                            exact: found.exact,
                        })
                        .collect();
                    let exact = found.iter().filter(|found| found.exact).count();
                    online.set_status(
                        match (found.len(), exact) {
                            (1, 1) => "One found, made for this file.".to_owned(),
                            (1, _) => "One found.".to_owned(),
                            (count, 0) => format!("{count} found."),
                            (count, exact) => format!("{count} found, {exact} made for this file."),
                        }
                        .into(),
                    );
                    online.set_failed(false);
                    online.set_results(ModelRc::new(VecModel::from(rows)));
                    online.set_selected(0);
                    *this.online_found.borrow_mut() = found;
                }
                Err(error) => {
                    online.set_failed(error != opensubtitles::Error::NotFound);
                    online.set_status(error.to_string().into());
                }
            }
        });
        if let Err(error) = spawned {
            log::warn!("{error}");
        }
    }

    /// Downloads one subtitle on a thread of its own, into Patina's own
    /// folder, and loads it into what plays.
    fn download_online(self: &Rc<Self>, found: opensubtitles::Found, automatic: bool) {
        let app = self.app();
        let prefs = app.prefs();
        let (key, username) = (
            prefs.opensubtitles_api_key.clone(),
            prefs.opensubtitles_username.clone(),
        );
        let folder = app.paths().online_subtitles_dir();
        let path = self.state.borrow().path.clone();
        let request = self.online_request.get() + 1;
        self.online_request.set(request);
        if !automatic {
            let online = self.window.global::<OnlineSubs>();
            online.set_busy(true);
            online.set_failed(false);
            online.set_status(format!("Downloading {}…", found.file_name).into());
        }
        let file_id = found.file_id;
        let (sender, receiver) = async_channel::bounded(1);
        let spawned = std::thread::Builder::new()
            .name("opensubtitles".into())
            .spawn(move || {
                let result = opensubtitles::shared(&key, &username)
                    .and_then(|client| client.download(file_id))
                    .and_then(|downloaded| {
                        // Named for the file it is, so different videos'
                        // "English.srt" don't meet.
                        let saved = folder.join(format!(
                            "{file_id} {}",
                            safe_file_name(&downloaded.file_name)
                        ));
                        std::fs::create_dir_all(&folder)
                            .and_then(|()| std::fs::write(&saved, &downloaded.contents))
                            .map_err(|error| {
                                opensubtitles::Error::Network(format!(
                                    "could not save {}: {error}",
                                    saved.display()
                                ))
                            })?;
                        Ok((saved, downloaded.file_name, downloaded.remaining))
                    });
                let _ = sender.send_blocking(result);
            });
        if let Err(error) = spawned {
            log::warn!("could not download subtitles: {error}");
            return;
        }
        let weak = Rc::downgrade(self);
        let spawned = slint::spawn_local(async move {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let Some(this) = weak.upgrade() else {
                return;
            };
            let current = this.online_request.get() == request;
            match result {
                // For the file it was found for, if that still plays.
                Ok((saved, name, remaining)) if this.state.borrow().path == path => {
                    let saved = saved.to_string_lossy().into_owned();
                    let language = found.language.clone();
                    this.player()
                        .command(&["sub-add", &saved, "select", &name, &language]);
                    if current && !automatic {
                        this.window.global::<OnlineSubs>().set_busy(false);
                        this.window.set_dialog(SharedString::new());
                    }
                    let detail = match remaining {
                        Some(1) => format!("{name} · 1 download left today"),
                        Some(left) if left >= 0 => format!("{name} · {left} downloads left today"),
                        _ => name,
                    };
                    this.show_osd("Subtitles Downloaded", &detail, None);
                }
                Ok(_) => {}
                Err(error) if automatic => log::info!("automatic subtitle download: {error}"),
                Err(error) => {
                    if current {
                        let online = this.window.global::<OnlineSubs>();
                        online.set_busy(false);
                        online.set_failed(true);
                        online.set_status(error.to_string().into());
                    }
                }
            }
        });
        if let Err(error) = spawned {
            log::warn!("{error}");
        }
    }

    /// The subtitle showing, when it is one downloaded from OpenSubtitles.
    fn downloaded_subtitle<'a>(&self, state: &'a PlaybackState) -> Option<&'a Track> {
        let folder = self.app.upgrade()?.paths().online_subtitles_dir();
        state.tracks(TrackKind::Sub).find(|track| {
            track.selected
                && track.main_selection == Some(0)
                && track.external
                && std::path::Path::new(&track.external_filename).starts_with(&folder)
        })
    }

    /// Subtitles › Save Downloaded Subtitle: copies it where the user
    /// chooses, beside the video to begin with, under its own name.
    fn save_downloaded_subtitle(self: &Rc<Self>) {
        let (source, name, folder) = {
            let state = self.state.borrow();
            let Some(track) = self.downloaded_subtitle(&state) else {
                return;
            };
            let name = if track.title.is_empty() {
                file_name(&track.external_filename)
            } else {
                track.title.clone()
            };
            let folder = (!util::is_url(&state.path))
                .then(|| {
                    std::path::Path::new(&state.path)
                        .parent()
                        .map(std::path::Path::to_path_buf)
                })
                .flatten();
            (
                std::path::PathBuf::from(&track.external_filename),
                name,
                folder,
            )
        };
        let mut dialog = self
            .app()
            .dialog_for(Some(self))
            .set_title("Save Downloaded Subtitle")
            .set_file_name(&name);
        if let Some(folder) = folder {
            dialog = dialog.set_directory(folder);
        }
        let weak = Rc::downgrade(self);
        let spawned = slint::spawn_local(async move {
            let Some(target) = dialog.save_file().await else {
                return;
            };
            let target = target.path().to_path_buf();
            let Some(this) = weak.upgrade() else {
                return;
            };
            match std::fs::copy(&source, &target) {
                Ok(_) => this.show_osd("Subtitle Saved", &target.to_string_lossy(), None),
                Err(error) => {
                    this.show_osd("Could not save the subtitle", &error.to_string(), None)
                }
            }
        });
        if let Err(error) = spawned {
            log::warn!("{error}");
        }
    }

    // ---- Seek-bar thumbnails --------------------------------------------

    fn reset_thumbnails(&self) {
        self.thumbnail_job.borrow_mut().take();
        self.thumbnails.borrow_mut().clear();
        self.thumbnail_path.borrow_mut().clear();
        self.window.global::<Playback>().set_preview_ready(false);
    }

    /// Starts making thumbnails once a local video's duration and size are
    /// known, as IINA does (never for streams or audio).
    fn maybe_start_thumbnails(&self, state: &PlaybackState) {
        let app = self.app();
        let prefs = app.prefs();
        let eligible = prefs.thumbnails
            && !state.idle
            && state.duration > 1.0
            && state.video.is_some()
            && !state.is_audio_only()
            && !state.path.is_empty()
            && !util::is_url(&state.path)
            && *self.thumbnail_path.borrow() != state.path;
        if !eligible {
            return;
        }
        *self.thumbnail_path.borrow_mut() = state.path.clone();
        *self.thumbnails.borrow_mut() = vec![None; thumbnails::COUNT];
        let (job, updates) = thumbnails::start(thumbnails::Request {
            path: state.path.clone().into(),
            duration: state.duration,
            width: prefs.thumbnail_width.clamp(40, 600) * 2,
            cache_dir: Some(app.paths().thumbnail_cache_dir()),
            cache_limit_bytes: prefs.thumbnail_cache_mb.saturating_mul(1 << 20),
        });
        *self.thumbnail_job.borrow_mut() = Some(job);
        let this = self.this.clone();
        let path = state.path.clone();
        let spawned = slint::spawn_local(async move {
            while let Ok(update) = updates.recv().await {
                let Some(this) = this.upgrade() else { break };
                if *this.thumbnail_path.borrow() != path {
                    break;
                }
                match update {
                    thumbnails::Update::Ready(index, thumbnail) => {
                        let buffer =
                            slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                                &thumbnail.rgba,
                                thumbnail.width,
                                thumbnail.height,
                            );
                        if let Some(slot) = this.thumbnails.borrow_mut().get_mut(index) {
                            *slot = Some(slint::Image::from_rgba8(buffer));
                        }
                    }
                    thumbnails::Update::Finished => break,
                }
            }
        });
        if let Err(error) = spawned {
            log::warn!("could not receive thumbnails: {error}");
        }
    }

    /// Fills in the seek preview for `time`: the nearest thumbnail made so
    /// far and the chapter there.
    fn show_preview(&self, time: f64) {
        let state = self.state.borrow();
        let playback = self.window.global::<Playback>();
        let chapter = state
            .chapters
            .iter()
            .rev()
            .find(|chapter| chapter.time <= time)
            .map(|chapter| chapter.title.clone())
            .unwrap_or_default();
        playback.set_preview_chapter(chapter.into());

        let thumbnails = self.thumbnails.borrow();
        if thumbnails.is_empty() || state.duration <= 0.0 {
            playback.set_preview_ready(false);
            return;
        }
        let wanted = ((time / state.duration) * thumbnails::COUNT as f64)
            .floor()
            .clamp(0.0, (thumbnails::COUNT - 1) as f64) as usize;
        // Use the closest thumbnail available while the rest are made.
        let nearest = (0..thumbnails::COUNT)
            .flat_map(|distance| [wanted.checked_sub(distance), Some(wanted + distance)])
            .flatten()
            .find_map(|index| thumbnails.get(index).cloned().flatten());
        match nearest {
            Some(image) => {
                playback.set_preview_image(image);
                playback.set_preview_ready(true);
            }
            None => playback.set_preview_ready(false),
        }
    }

    // ---- Recent files ---------------------------------------------------

    pub fn refresh_recent(&self) {
        let app = self.app();
        let (items, resume) = app.recent_items();
        let media = self.window.global::<Media>();
        let items: Vec<RecentItem> = items
            .into_iter()
            .map(|(title, path)| RecentItem {
                detail: util::parent_directory(&path).into(),
                title: title.into(),
                path: path.into(),
            })
            .collect();
        media.set_recent(ModelRc::new(VecModel::from(items)));
        media.set_resume_title(resume.into());
    }

    // ---- Shutdown -------------------------------------------------------

    /// Closes the window and shuts down its mpv core.
    pub fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        {
            let state = self.state.borrow();
            if !state.idle {
                self.app()
                    .update_history(&state.path, state.position, state.duration);
            }
            self.app().update_prefs(|prefs| prefs.volume = state.volume);
        }
        self.reset_thumbnails();
        // Hiding the window tears down its renderer, which frees mpv's render
        // context while the GL context is still current; without OpenGL the
        // render thread frees its own.
        let _ = self.window.hide();
        self.software_video.borrow_mut().take();
        if let Some(mut player) = self.player.borrow_mut().take() {
            player.shutdown();
        }
        self.app().window_closed(self);
    }
}

/// The player window as music mode found it.
struct MusicRestore {
    /// None to fit the window to the video instead: it was full screen, or
    /// not shown yet.
    size: Option<slint::LogicalSize>,
    maximized: bool,
    /// The sidebar, which is music mode's drawer.
    sidebar_open: bool,
    sidebar_tab: SharedString,
}

/// Where a picture-in-picture request for an opening file stands.
#[derive(Clone, Copy, PartialEq)]
enum PipPending {
    No,
    /// Asked for; the file hasn't started.
    Asked,
    /// Its file has started: enter on its video, forget at its end.
    Opening,
}

/// Music mode's cover, where mpv draws in it, in physical pixels; None for
/// the whole window. At least a pixel, so that video keeps playing while
/// the cover is hidden.
fn video_area(window: &PlayerWindow) -> Option<(u32, u32)> {
    if !window.get_music_mode() {
        return None;
    }
    let scale = window.window().scale_factor();
    let pixels = |length: f32| (length * scale).round().max(1.0) as u32;
    Some((
        pixels(window.get_video_width()),
        pixels(window.get_video_height()),
    ))
}

/// Entry ids in playlist order, which the selection is kept by.
fn playlist_order(state: &PlaybackState) -> Vec<i64> {
    state.playlist.iter().map(|entry| entry.id).collect()
}

/// From the load hook, before mpv's yt-dlp hook: the yt-dlp format chosen
/// for the file from the quality menus, and where to start it when it is
/// reloading for one.
fn apply_quality(
    mpv: &Mpv,
    qualities: &HashMap<String, QualityChoice>,
    resume_at: Option<(String, f64)>,
) {
    let Ok(path) = mpv.get_property("path") else {
        return;
    };
    let Some(path) = path.as_str() else {
        return;
    };
    if let Some(format) = qualities
        .get(path)
        .and_then(|choice| choice.format.as_deref())
        && let Err(error) = mpv.set_property("file-local-options/ytdl-format", format)
    {
        log::warn!("{error}");
    }
    if let Some((resume_path, at)) = resume_at
        && resume_path == path
        && at > 0.5
        && let Err(error) =
            mpv.set_property("file-local-options/start", format!("{at:.3}").as_str())
    {
        log::warn!("{error}");
    }
}

/// One line about a subtitle found online: its language, year, frame rate,
/// downloads and date, and whether it is for the hard of hearing or
/// machine-translated.
fn describe_online(found: &opensubtitles::Found) -> String {
    let mut parts = vec![
        crate::player::state::language_name(&found.language)
            .map(str::to_owned)
            .unwrap_or_else(|| found.language.clone()),
    ];
    if let Some(year) = found.year {
        parts.push(year.to_string());
    }
    if let Some(fps) = found.fps {
        parts.push(format!("{} fps", (fps * 1000.0).round() / 1000.0));
    }
    parts.push(match found.downloads {
        1 => "1 download".to_owned(),
        count => format!("{count} downloads"),
    });
    if !found.uploaded.is_empty() {
        parts.push(found.uploaded.clone());
    }
    if found.hearing_impaired {
        parts.push("hearing impaired".to_owned());
    }
    if found.machine_translated {
        parts.push("machine translated".to_owned());
    }
    parts.join(" · ")
}

/// A file name from elsewhere made safe to save: no folders, nothing the
/// file systems refuse.
fn safe_file_name(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let name = name.trim().trim_start_matches('.').to_owned();
    if name.is_empty() {
        "subtitle.srt".to_owned()
    } else {
        name
    }
}

/// The preferred subtitle languages, most preferred first.
fn sub_languages(prefs: &crate::settings::Preferences) -> Vec<String> {
    prefs
        .sub_language
        .split(',')
        .map(str::trim)
        .filter(|code| !code.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Queues `loadfile` for one source, at `index` for the modes that take
/// one. Folders are read recursively, every media file inside, as IINA
/// opens them.
fn loadfile(player: &Player, source: &str, mode: &str, index: Option<usize>) {
    let index = index.map_or_else(|| "-1".to_owned(), |index| index.to_string());
    if std::path::Path::new(source).is_dir() {
        player.command(&["loadfile", source, mode, &index, "directory-mode=recursive"]);
    } else {
        player.command(&["loadfile", source, mode, &index]);
    }
}

fn is_command_prefix(arg: &str) -> bool {
    matches!(
        arg,
        "no-osd"
            | "osd-bar"
            | "osd-msg"
            | "osd-msg-bar"
            | "osd-auto"
            | "raw"
            | "expand-properties"
            | "repeatable"
            | "nonrepeatable"
            | "async"
            | "sync"
    )
}

fn track_osd(kind: &str, track: Option<&Track>) -> (String, String, Option<f64>) {
    match track {
        Some(track) => (format!("{kind}: {}", track.label()), track.detail(), None),
        None => (format!("{kind}: None"), String::new(), None),
    }
}

fn track_model(
    state: &PlaybackState,
    kind: TrackKind,
    with_none: bool,
    secondary: bool,
) -> ModelRc<TrackItem> {
    let selected = if secondary {
        state.secondary_sid
    } else {
        match kind {
            TrackKind::Video => state.vid,
            TrackKind::Audio => state.aid,
            TrackKind::Sub => state.sid,
        }
    };
    let mut items = Vec::new();
    if with_none {
        items.push(TrackItem {
            id: 0,
            title: "None".into(),
            detail: SharedString::new(),
            selected: selected.is_none(),
        });
    }
    items.extend(state.tracks(kind).map(|track| TrackItem {
        id: track.id as i32,
        title: track.label().into(),
        detail: track.detail().into(),
        selected: selected == Some(track.id),
    }));
    ModelRc::new(VecModel::from(items))
}

/// The aspect menu entry for a ratio: 0 for none, -1 for a custom ratio.
fn aspect_index(ratio: f64) -> i32 {
    if ratio <= 0.0 {
        return 0;
    }
    CROPS
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, (w, h))| (w / h - ratio).abs() < 0.01)
        .map_or(-1, |(index, _)| index as i32)
}

/// The labelled audio filter for the 10-band equalizer, or None when flat.
fn eq_filter(gains: &[f32; 10]) -> Option<String> {
    let bands: Vec<String> = EQ_BANDS
        .iter()
        .zip(gains)
        .filter(|(_, gain)| **gain != 0.0)
        .map(|(frequency, gain)| format!("equalizer=f={frequency}:t=o:w=1:g={gain}"))
        .collect();
    (!bands.is_empty()).then(|| format!("@patina-eq:lavfi=[{}]", bands.join(",")))
}

fn video_info(state: &PlaybackState) -> String {
    let Some(video) = &state.video else {
        return String::new();
    };
    let mut parts = vec![format!("{}×{}", video.width, video.height)];
    if !state.video_codec.is_empty() {
        parts.push(
            state
                .video_codec
                .split(' ')
                .next()
                .unwrap_or_default()
                .to_uppercase(),
        );
    }
    let fps = if state.estimated_fps > 0.0 {
        state.estimated_fps
    } else {
        state.container_fps
    };
    if fps > 0.0 {
        parts.push(format!("{fps:.3} fps").replace(".000", ""));
    }
    if !video.pixel_format.is_empty() {
        parts.push(video.pixel_format.clone());
    }
    if video.is_hdr() {
        parts.push(format!("HDR ({})", video.gamma.to_uppercase()));
    }
    parts.join(" · ")
}

fn audio_info(state: &PlaybackState) -> String {
    let mut parts = Vec::new();
    if !state.audio_codec.is_empty() {
        parts.push(state.audio_codec.to_uppercase());
    }
    if state.audio_samplerate > 0 {
        parts.push(format!("{} kHz", state.audio_samplerate as f64 / 1000.0));
    }
    if !state.audio_channels.is_empty() {
        parts.push(state.audio_channels.clone());
    }
    parts.join(" · ")
}

fn string_model(values: &[String]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        values.iter().map(SharedString::from).collect::<Vec<_>>(),
    ))
}

pub fn is_subtitle(path: &str) -> bool {
    const EXTENSIONS: [&str; 11] = [
        "ass", "ssa", "srt", "sub", "idx", "vtt", "sup", "smi", "lrc", "mks", "pgs",
    ];
    std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// Compares strings with runs of digits ordered numerically ("2" < "10").
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |chars: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut digits = String::new();
                    while let Some(c) = chars.peek().copied().filter(char::is_ascii_digit) {
                        digits.push(c);
                        chars.next();
                    }
                    digits
                };
                let (da, db) = (take(&mut a), take(&mut b));
                let (ta, tb) = (da.trim_start_matches('0'), db.trim_start_matches('0'));
                let ordering = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ordering != std::cmp::Ordering::Equal {
                    return ordering;
                }
            }
            (Some(x), Some(y)) => {
                let ordering = x.to_lowercase().cmp(y.to_lowercase());
                if ordering != std::cmp::Ordering::Equal {
                    return ordering;
                }
                a.next();
                b.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aspect_ratios_map_to_menu_entries() {
        assert_eq!(aspect_index(0.0), 0);
        assert_eq!(aspect_index(16.0 / 9.0), 2);
        assert_eq!(aspect_index(1.777778), 2);
        assert_eq!(aspect_index(2.35), -1);
    }

    #[test]
    fn equalizer_filters_are_accepted_by_mpv() {
        let mut gains = [0.0; 10];
        assert_eq!(eq_filter(&gains), None);
        gains[0] = 3.0;
        gains[9] = -4.5;
        let filter = eq_filter(&gains).unwrap();
        assert_eq!(
            filter,
            "@patina-eq:lavfi=[equalizer=f=32:t=o:w=1:g=3,equalizer=f=16000:t=o:w=1:g=-4.5]"
        );

        // While audio plays, mpv builds the graph inside `af add` and fails
        // the command if lavfi rejects it; a bogus graph is the control.
        let Some(mpv) = crate::mpv::testing::headless() else {
            return;
        };
        mpv.command(&["loadfile", "av://lavfi:sine=frequency=440:duration=30"])
            .unwrap();
        crate::mpv::testing::wait_for(&mpv, |event| matches!(event, Event::PlaybackRestart));
        assert!(
            mpv.command(&["af", "add", "@patina-eq:lavfi=[equalizer=nonsense=1]"])
                .is_err()
        );
        mpv.command(&["af", "add", &filter]).unwrap();
        let chain = mpv.get_property("af").unwrap();
        let labels: Vec<_> = chain
            .as_array()
            .iter()
            .map(|f| f.str_or_default("label"))
            .collect();
        assert_eq!(labels, ["patina-eq"]);
    }

    #[test]
    fn natural_order_sorts_episode_numbers() {
        let mut names = vec!["ep10.mkv", "ep2.mkv", "Ep1.mkv", "ep02b.mkv"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["Ep1.mkv", "ep2.mkv", "ep02b.mkv", "ep10.mkv"]);
    }

    #[test]
    fn subtitle_files_are_recognized_by_extension() {
        assert!(is_subtitle("/a/b.SRT"));
        assert!(is_subtitle("movie.en.ass"));
        assert!(!is_subtitle("movie.mkv"));
    }
}
