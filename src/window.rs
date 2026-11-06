//! One player window: its Slint component, its mpv core, and the glue.

use crate::app::App;
use crate::input::{Binding, KeyCombo};
use crate::jellyfin::lyrics::Lyrics;
use crate::jellyfin::models::StreamKind;
use crate::jellyfin::playback::{self, Playing, Report};
use crate::jellyfin::segments::{self, Segments, Span};
use crate::jellyfin::{session, trickplay};
use crate::mpv::{EndFileReason, Event, Mpv, Secret};
use crate::opensubtitles;
use crate::player::state::{
    AudioStatus, PlaybackState, PlaylistEntry, Track, TrackKind, file_name,
};
use crate::player::{self, Player, Prop};
use crate::settings::ResizeWindow;
use crate::subtitle_match;
use crate::thumbnails;
use crate::util::{self, format_delay, format_speed, format_time};
use crate::youtube::VideoId;
use crate::youtube::playing::{self, Sitting};
use crate::ytdl;
use crate::{
    Actions, ChapterItem, DeviceItem, Library, LyricItem, Media, OnlineSubs, OnlineSubtitle, Osd,
    Platform, Playback, PlayerWindow, PlaylistItem, Prefs, QualityItem, RecentItem, ShortcutItem,
    Theme, TrackItem, YouTube,
};
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Gives the keyboard back to the window when what has it can take no key.
/// Slint keeps its focus on an item that has gone and hands that every key,
/// Ctrl+Q's too, until Tab. Nothing says this has happened, and there are many
/// ways to it: a card scrolled out of a list, whose row the ListView destroys;
/// a choice from a submenu, as Slint 1.18 closes a menu before its submenus and
/// each gives the focus back to what had it when it opened, the last of them to
/// an item of a menu closed by then; a row made again. So it is looked for here,
/// ahead of each of the window's events.
///
/// Slint also drops the focus of an item clipped out of sight, a control
/// scrolled off a page, along with the key that finds it there. That is only
/// looked for ahead of a key (`key_coming`): a list scrolling to what Tab just
/// reached has it out of sight for a moment.
fn keep_keyboard(window: &PlayerWindow, key_coming: bool) {
    let inner = i_slint_core::window::WindowInner::from_pub(window.window());
    // A menu has the keyboard while it is open.
    if !inner.active_popups().is_empty() {
        return;
    }
    let lost = match inner.focus_item.borrow().upgrade() {
        Some(item) => key_coming && !item.is_visible(),
        None => true,
    };
    if lost {
        window.invoke_take_keyboard();
    }
}

/// Reply userdata for `show-text`, whose expanded text becomes an OSD message.
const REPLY_SHOW_TEXT: u64 = 10;
const REPLY_SCREENSHOT: u64 = 11;
/// Reply userdata for what yt-dlp said about the file, from mpv's yt-dlp hook.
const REPLY_YTDL: u64 = 12;
/// Reply userdata for the audio devices, asked for when about to be shown by
/// a core that doesn't keep them listed (CoreOptions::list_audio_devices).
const REPLY_AUDIO_DEVICES: u64 = 13;

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
/// How long "Could not open …" stays up, unless clicked away sooner.
const LOAD_ERROR_FOR: Duration = Duration::from_secs(8);
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
/// The qualities a Jellyfin item can be asked for below its own, in bits
/// per second: the server transcodes to them.
const JELLYFIN_QUALITIES: [(i64, &str); 5] = [
    (20_000_000, "20 Mbit/s"),
    (10_000_000, "10 Mbit/s"),
    (6_000_000, "6 Mbit/s"),
    (4_000_000, "4 Mbit/s"),
    (2_000_000, "2 Mbit/s"),
];

pub struct PlayerController {
    app: Weak<App>,
    window: PlayerWindow,
    player: RefCell<Option<Player>>,
    mpv: Arc<Mpv>,
    state: RefCell<PlaybackState>,
    ready: Cell<bool>,
    pending_open: RefCell<Vec<(Vec<String>, Placement)>>,
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
    /// mpv keeps the audio devices listed; otherwise they are asked for
    /// when about to be shown.
    lists_audio_devices: bool,
    closed: Cell<bool>,
    osd_timer: Timer,
    /// Takes a load's error down again (LOAD_ERROR_FOR).
    error_timer: Timer,
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
    /// Linux: whether this is a Wayland window has been looked at (it can
    /// only be once winit has made the window).
    #[cfg(all(unix, not(target_os = "macos")))]
    window_system_known: Cell<bool>,
    /// Selected playlist rows, by mpv's entry ids.
    playlist_selection: RefCell<crate::playlist::Selection>,
    /// How much of each row's name is a series prefix shared with its
    /// neighbours, in bytes (0 for none).
    series_prefixes: RefCell<Vec<usize>>,
    /// The Jellyfin library shown while the window is idle; None once the
    /// window has closed.
    library: RefCell<Option<Rc<crate::jellyfin::library::LibraryController>>>,
    /// What plays was opened from the YouTube section, which comes back when it
    /// ends. This, rather than the address, says so, as the same address opened
    /// any other way stays on its last frame as a stream does.
    from_section: Cell<bool>,
    /// What the section sent to play, in order.
    section_list: RefCell<Vec<String>>,
    /// The time a YouTube video has spent playing, which its mark goes by.
    sitting: RefCell<Option<Sitting>>,
    /// What yt-dlp said went wrong as mpv's hook ran it for the file opening.
    hook_failure: RefCell<Option<crate::youtube::Error>>,
    /// The YouTube section, which shows over the welcome screen and the
    /// library while the window is idle; None once the window has closed.
    youtube: RefCell<Option<Rc<crate::youtube::view::YouTubeController>>>,
    /// Counts the times the playlist was replaced, so that the library can
    /// drop a Play answer that comes after the window opened something
    /// else.
    opens: Cell<u64>,
    /// The Jellyfin playback of the file open now, for its reports and the
    /// mapping of its tracks; None for any other file.
    jellyfin: RefCell<Option<Playing>>,
    /// What the load hook made of a Jellyfin entry, until mpv has opened it
    /// or failed to.
    jellyfin_resolved: playback::Resolved,
    /// The versions chosen for Jellyfin entries from the Version menu, by
    /// entry, until the library opens the entry again with its page's.
    jellyfin_versions: RefCell<HashMap<String, String>>,
    /// The bitrate chosen from the Video Quality menu for Jellyfin items,
    /// which holds for every one the window plays: lowered for the
    /// network, it is wanted for the next episode too.
    jellyfin_quality: Cell<Option<i64>>,
    /// What the next load of an entry, reloading it for another quality,
    /// version or subtitle, carries over: where it was and its streams.
    jellyfin_reload: RefCell<Option<(String, playback::Reload)>>,
    /// The segments of the playback a reload replaces, by entry: which
    /// were skipped without asking stays so.
    jellyfin_kept_segments: RefCell<Option<(String, Segments)>>,
    jellyfin_pacer: RefCell<playback::Pacer>,
    /// Asks the pacer once a second whether progress is due.
    jellyfin_timer: Timer,
    /// Where the Jellyfin file was when last heard, for its reports: once
    /// it has ended mpv no longer says.
    jellyfin_position: Cell<f64>,
    /// The server's tracks wait to be selected until mpv lists the file's.
    jellyfin_tracks: Cell<bool>,
    /// A seek is under way; it is reported once playback restarts.
    jellyfin_seeked: Cell<bool>,
    /// The file has opened and not yet started playing: its start is
    /// reported once it has, when mpv knows where it is, whether it can
    /// seek and which tracks play.
    jellyfin_start_pending: Cell<bool>,
    /// mpv said where playback is last time it said anything: at a file's
    /// end it says nothing, which the state reads as 0, the start of an
    /// intro or recap the file ending would be sent back to skip.
    position_known: Cell<bool>,
    /// The file reached its end, where mpv keeps it open: its stop was
    /// reported then, and playing on (a seek back) starts it again.
    jellyfin_at_end: Cell<bool>,
    /// A stop was reported: the library shows the server's progress once
    /// the server has taken it (`Change::Played`), not before.
    jellyfin_stopping: Cell<bool>,
    /// The intros, credits and the like the server marks in the Jellyfin
    /// file, and the one whose Skip button shows.
    jellyfin_segments: RefCell<Segments>,
    jellyfin_offer: Cell<Option<Span>>,
    /// Seek-bar previews from the server's trickplay images, for a
    /// Jellyfin file that has them.
    jellyfin_tiles: RefCell<Option<Rc<trickplay::Tiles>>>,
    /// The lyrics of the Jellyfin song playing, once its server has sent
    /// them, and the line being sung.
    jellyfin_lyrics: RefCell<Option<Lyrics>>,
    lyric_line: Cell<Option<usize>>,
    /// The time the seek preview was asked for last, shown once a tile
    /// that was missing has come.
    preview_time: Cell<f64>,
    /// See `set_focus_serial`.
    focus_serial: Cell<u64>,
    /// The window's size when it left the library to play something, which
    /// it goes back to when it is idle again: fitted to a small video or a
    /// cover, the library's bar would hide its tabs.
    library_size: Cell<Option<slint::LogicalSize>>,
    this: Weak<PlayerController>,
}

impl PlayerController {
    pub fn new(app: &Rc<App>, options: &crate::player::CoreOptions) -> Result<Rc<Self>, String> {
        let window = PlayerWindow::new().map_err(|error| error.to_string())?;
        crate::fonts::lead_sans_serif(&window.global::<Theme>().get_font_family());
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
            #[cfg(all(unix, not(target_os = "macos")))]
            window_system_known: Cell::new(false),
            playlist_selection: RefCell::new(Default::default()),
            series_prefixes: RefCell::new(Vec::new()),
            library: RefCell::new(None),
            from_section: Cell::new(false),
            section_list: RefCell::new(Vec::new()),
            sitting: RefCell::new(None),
            hook_failure: RefCell::new(None),
            youtube: RefCell::new(None),
            opens: Cell::new(0),
            jellyfin: RefCell::new(None),
            jellyfin_resolved: playback::Resolved::default(),
            jellyfin_versions: RefCell::new(HashMap::new()),
            jellyfin_quality: Cell::new(None),
            jellyfin_reload: RefCell::new(None),
            jellyfin_kept_segments: RefCell::new(None),
            jellyfin_pacer: RefCell::new(playback::Pacer::default()),
            jellyfin_timer: Timer::default(),
            jellyfin_position: Cell::new(0.0),
            jellyfin_tracks: Cell::new(false),
            jellyfin_seeked: Cell::new(false),
            jellyfin_start_pending: Cell::new(false),
            position_known: Cell::new(false),
            jellyfin_at_end: Cell::new(false),
            jellyfin_stopping: Cell::new(false),
            jellyfin_segments: RefCell::new(Segments::default()),
            jellyfin_offer: Cell::new(None),
            jellyfin_tiles: RefCell::new(None),
            jellyfin_lyrics: RefCell::new(None),
            lyric_line: Cell::new(None),
            preview_time: Cell::new(0.0),
            library_size: Cell::new(None),
            focus_serial: Cell::new(0),
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
            lists_audio_devices: options.list_audio_devices,
            closed: Cell::new(false),
            osd_timer: Timer::default(),
            error_timer: Timer::default(),
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
        *controller.library.borrow_mut() = Some(crate::jellyfin::library::LibraryController::new(
            &app.jellyfin(),
            &controller.window,
            Rc::downgrade(&controller),
        ));
        let prefs_of = controller.app.clone();
        *controller.youtube.borrow_mut() = Some(crate::youtube::view::YouTubeController::new(
            &app.youtube(),
            &controller.window,
            Rc::downgrade(&controller),
            Box::new(move || {
                prefs_of
                    .upgrade()
                    .map(|app| app.prefs())
                    .unwrap_or_default()
            }),
        ));
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

    /// Shows why a file didn't open, for a while: it says what happened, and
    /// would otherwise stay over the welcome screen or the library until
    /// something else plays. A click takes it down sooner.
    fn show_load_error(&self, text: String) {
        self.window.set_error(text.clone().into());
        let this = self.this.clone();
        self.error_timer
            .start(TimerMode::SingleShot, LOAD_ERROR_FOR, move || {
                if let Some(this) = this.upgrade()
                    && this.window.get_error() == text.as_str()
                {
                    this.window.set_error(SharedString::new());
                }
            });
    }

    /// Asks mpv for the audio devices, which a menu, the Audio panel or the
    /// preferences are about to show, when the core doesn't keep them
    /// listed. Each ask goes through all of mpv's audio outputs, PipeWire's
    /// too, so it happens only on the way to showing them.
    pub fn request_audio_devices(&self) {
        if self.lists_audio_devices || !self.is_running() {
            return;
        }
        if let Err(error) = self
            .mpv
            .get_property_async(REPLY_AUDIO_DEVICES, "audio-device-list")
        {
            log::debug!("{error}");
        }
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

    /// Takes Float on Top away from a Wayland window, which can't float:
    /// the pin, the menu item and the preference go, for this window and
    /// every later one.
    fn check_window_system(&self) {
        #[cfg(all(unix, not(target_os = "macos")))]
        if !self.window_system_known.replace(true)
            && crate::wayland::is_wayland(self.window.window())
        {
            crate::platform::windows_cannot_float();
            self.window.global::<Platform>().set_can_float(false);
        }
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
            .map(|entry| (self.entry_label(entry), entry.filename.clone()))
            .collect()
    }

    /// A playlist entry's name: a Jellyfin item's title, as the library or
    /// the history knows it, a video's from the YouTube section, and otherwise
    /// as `PlaylistEntry::label` has it.
    fn entry_label(&self, entry: &PlaylistEntry) -> String {
        if session::parse_entry(&entry.filename).is_some()
            && let Some(title) = self.app().entry_title(&entry.filename)
        {
            return title;
        }
        if let Some(title) = self.app().youtube().title_of(&entry.filename) {
            return title;
        }
        entry.label()
    }

    /// What a file is called in messages: a Jellyfin item by its title.
    fn display_name(&self, path: &str) -> String {
        if session::parse_entry(path).is_some()
            && let Some(title) = self.app().entry_title(path)
        {
            return title;
        }
        if let Some(title) = self.app().youtube().title_of(path) {
            return title;
        }
        if path.is_empty() {
            String::from("the file")
        } else {
            file_name(path)
        }
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
        platform.set_can_float(crate::platform::windows_float());
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
        for (sources, placement) in pending {
            self.load(&sources, placement);
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
        let placement = if append {
            Placement::Append
        } else {
            Placement::Replace
        };
        self.open_at(sources, placement);
    }

    /// Opens sources in place of the playlist and in their order, playing
    /// from the one at `start` (those before it are in the playlist too,
    /// before it), with the playlist's Shuffle off: the library's Play of
    /// an album, or of one of its songs.
    pub fn open_from(&self, sources: Vec<String>, start: usize) {
        self.open_at(sources, Placement::InOrder(start));
    }

    /// Plays videos from the YouTube section, in their list's order from the
    /// one at `start`. When they end the window goes back to the section, which
    /// has stayed where it was, under them.
    pub fn play_from_section(&self, sources: Vec<String>, start: usize) {
        self.section_list.borrow_mut().clone_from(&sources);
        self.open_at(sources, Placement::Section(start));
    }

    /// Opens something from the history, in place of the playlist. A video the
    /// YouTube section sent goes the way it went then, with none of the user's
    /// yt-dlp options: they can hold cookies, and the section has no account.
    /// Reached from Recent, this is too easy to do without meaning to.
    pub fn open_again(&self, path: String) {
        if self.app().came_from_section(&path) {
            self.play_from_section(vec![path], 0);
        } else {
            self.open(vec![path], false);
        }
    }

    /// The preferences now name another yt-dlp, or none.
    pub fn yt_dlp_changed(&self) {
        if let Some(youtube) = self.youtube() {
            youtube.tool_changed();
        }
    }

    /// Opens sources in place of the playlist in a random order, with the
    /// playlist's Shuffle on: the library's Shuffle.
    pub fn open_shuffled(&self, sources: Vec<String>) {
        self.open_at(sources, Placement::Shuffled);
    }

    fn open_at(&self, sources: Vec<String>, placement: Placement) {
        if sources.is_empty() {
            return;
        }
        if placement != Placement::Append {
            self.opens.set(self.opens.get() + 1);
            self.from_section
                .set(matches!(placement, Placement::Section(_)));
        }
        if self.ready.get() {
            self.load(&sources, placement);
        } else {
            self.pending_open.borrow_mut().push((sources, placement));
        }
    }

    /// Where Play Next and Add to Queue from this window's library go: the
    /// window used last of those with something to play, since the
    /// library shows only while this one has nothing. None when no other
    /// has anything.
    pub fn queue_window(&self) -> Option<Rc<PlayerController>> {
        self.app.upgrade()?.queue_window(self)
    }

    /// Counts the times the window was focused, in the order of all the
    /// windows': the window with the highest was used last.
    pub fn set_focus_serial(&self, serial: u64) {
        self.focus_serial.set(serial);
    }

    pub fn focus_serial(&self) -> u64 {
        self.focus_serial.get()
    }

    /// Adds sources to the playlist: right after the file playing with
    /// `next`, otherwise at the end. A window with nothing to play opens
    /// them as a new playlist (`queueing`).
    pub fn queue(&self, sources: Vec<String>, next: bool) {
        let playing = usize::try_from(self.state.borrow().playlist_pos).ok();
        match queueing(self.is_idle(), next && self.ready.get(), playing) {
            Queueing::Open => self.open(sources, false),
            Queueing::After(at) => {
                let player = self.player();
                for (index, mode, place) in queue_steps(sources.len(), at) {
                    loadfile(&player, &sources[index], mode, place);
                }
            }
            Queueing::End => self.open(sources, true),
        }
    }

    fn library(&self) -> Option<Rc<crate::jellyfin::library::LibraryController>> {
        self.library.borrow().clone()
    }

    fn youtube(&self) -> Option<Rc<crate::youtube::view::YouTubeController>> {
        self.youtube.borrow().clone()
    }

    /// How many times the playlist was replaced so far (see `opens`).
    pub fn opened(&self) -> u64 {
        self.opens.get()
    }

    fn load(&self, sources: &[String], placement: Placement) {
        let append = placement == Placement::Append;
        if !append
            && self.is_idle()
            && (self.window.global::<Library>().get_shown()
                || self.window.global::<YouTube>().get_shown())
        {
            self.remember_library_size();
        }
        if sources.iter().any(|source| util::is_web(source)) {
            self.app().ytdl_current();
        }
        let player = self.player();
        self.load_asked.set(true);
        if !append {
            // The server keeps a Jellyfin item's place, opened or still
            // opening: a watch-later file for its entry would be read
            // before the load hook the next time it opens.
            if session::parse_entry(&self.state.borrow().path).is_none() {
                player.command_quiet(&["write-watch-later-config"]);
            }
            self.has_fitted.set(false);
        }
        if placement == Placement::Shuffled {
            // What is left of a playlist that ended.
            player.command(&["playlist-clear"]);
        }
        for (index, mode, place) in load_steps(sources.len(), placement) {
            loadfile(&player, &sources[index], mode, place);
        }
        match placement {
            // Shuffled as the playlist's Shuffle does it, which keeps their
            // order for turning it off again, and only then played: nothing
            // starts loading before.
            Placement::Shuffled => {
                player.set("shuffle", true);
                player.command(&["playlist-shuffle"]);
                player.command(&["playlist-play-index", "0"]);
            }
            Placement::InOrder(_) | Placement::Section(_) => player.set("shuffle", false),
            Placement::Replace | Placement::Append => {}
        }
        if !append {
            player.set("pause", self.app().prefs().pause_when_open);
        }
        drop(player);
        match placement {
            // A list stays out of the history until its videos have played:
            // Recent would otherwise be two hundred videos nobody has seen.
            Placement::Section(start) => {
                if let Some(source) = sources.get(start) {
                    self.app().note_opened(std::slice::from_ref(source), true);
                }
            }
            _ => self.app().note_opened(sources, false),
        }
        self.window.invoke_show_controls();
    }

    // ---- mpv events -----------------------------------------------------

    fn handle_events(&self, batch: Vec<Event>) {
        let mut changed = HashSet::new();
        let mut restarted = false;
        for event in batch {
            match event {
                Event::PropertyChange {
                    userdata, value, ..
                } => {
                    if log::log_enabled!(log::Level::Trace) && userdata != Prop::TimePos as u64 {
                        log::trace!("{:?} = {value:?}", Prop::from_userdata(userdata));
                    }
                    // The last position mpv gave: at a file's end it gives
                    // none, which the state reads as 0.
                    if userdata == Prop::TimePos as u64 {
                        let position = value.as_f64();
                        self.position_known.set(position.is_some());
                        if let Some(position) = position
                            && self.jellyfin.borrow().is_some()
                        {
                            self.jellyfin_position.set(position.max(0.0));
                        }
                        if let (Some(position), Some(sitting)) =
                            (position, &mut *self.sitting.borrow_mut())
                        {
                            sitting.at(position);
                        }
                    }
                    if userdata == Prop::Duration as u64
                        && let (Some(duration), Some(sitting)) =
                            (value.as_f64(), &mut *self.sitting.borrow_mut())
                    {
                        sitting.lasts(duration);
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
                    self.reset_segments();
                    self.clear_lyrics(true);
                    self.window
                        .global::<Media>()
                        .set_video_versions(ModelRc::new(VecModel::default()));
                    self.jellyfin_tracks.set(false);
                    self.jellyfin_seeked.set(false);
                    self.jellyfin_start_pending.set(false);
                    self.jellyfin_stopping.set(false);
                    self.hook_failure.borrow_mut().take();
                    self.loading.set(true);
                    changed.insert(Prop::PausedForCache);
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
                    self.jellyfin_loaded();
                    self.start_sitting();
                    if self.jellyfin.borrow().is_some() {
                        // Whether it is music is the server's to say, which
                        // the window knows only now.
                        changed.insert(Prop::TrackList);
                    } else {
                        self.clear_lyrics(false);
                    }
                    let state = self.state.borrow();
                    // mpv may not have said the title it was given yet.
                    let title = match &*self.jellyfin.borrow() {
                        Some(playing) => playing.title.clone(),
                        None => self
                            .section_title(&state.path)
                            .unwrap_or_else(|| state.title.clone()),
                    };
                    let section =
                        self.from_section.get() && self.section_list.borrow().contains(&state.path);
                    self.app()
                        .record_history(&state.path, &title, state.duration, section);
                    // A Jellyfin item's stream is the server's, which mpv's
                    // yt-dlp hook leaves alone.
                    if util::is_url(&state.path) && self.jellyfin.borrow().is_none() {
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
                    let opened = !self.loading.replace(false);
                    self.load_asked.set(false);
                    if self.pip_pending.get() == PipPending::Opening {
                        self.pip_pending.set(PipPending::No);
                    }
                    changed.insert(Prop::PausedForCache);
                    // A file that never opened (a Jellyfin item while
                    // signed out, a drive not mounted) keeps where History
                    // says it was.
                    if opened {
                        let state = self.state.borrow();
                        self.app()
                            .update_history(&state.path, state.position, state.duration);
                    }
                    let failure = self.jellyfin_ended();
                    self.end_sitting();
                    if reason == EndFileReason::Error {
                        let path = self.state.borrow().path.clone();
                        let name = self.display_name(&path);
                        let hook = self.hook_failure.borrow_mut().take();
                        if self.from_section.get() {
                            let mpv_said = error.map(|error| error.description());
                            self.section_video_failed(&name, hook, mpv_said);
                        } else {
                            // Only its one address: what yt-dlp says of other
                            // sites isn't in these words.
                            let named = hook
                                .filter(|_| VideoId::from_watch_url(&path).is_some())
                                .map(|hook| hook.said());
                            let detail = failure
                                .or(named)
                                .or_else(|| error.map(|error| error.description()))
                                .unwrap_or_default();
                            self.show_load_error(format!("Could not open {name}\n{detail}"));
                        }
                    }
                }
                Event::Log(message) => self.hook_said(&message.text),
                Event::Seek => {
                    if self.user_acted() {
                        self.osd_seek();
                    }
                    if self.jellyfin.borrow().is_some() {
                        self.jellyfin_seeked.set(true);
                    }
                    if let Some(sitting) = &mut *self.sitting.borrow_mut() {
                        sitting.seeked();
                    }
                }
                Event::PlaybackRestart => {
                    restarted = true;
                    if self.jellyfin_seeked.replace(false)
                        && !self.jellyfin_at_end.get()
                        && !self.jellyfin_start_pending.get()
                    {
                        self.jellyfin_changed_now();
                    }
                    // A seek finished (or playback began): tell the media
                    // controls where playback is now.
                    if let Some(app) = self.app.upgrade()
                        && app.is_media_target(self)
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
                Event::GetPropertyReply {
                    userdata: REPLY_AUDIO_DEVICES,
                    result,
                } => match result {
                    Ok((_, value)) => {
                        if self.state.borrow_mut().apply(Prop::AudioDeviceList, &value) {
                            changed.insert(Prop::AudioDeviceList);
                            if let Some(app) = self.app.upgrade() {
                                // Deferred: the app may be borrowing its
                                // windows right now.
                                Timer::single_shot(Duration::ZERO, move || {
                                    app.audio_devices_changed()
                                });
                            }
                        }
                    }
                    Err(error) => log::debug!("{error}"),
                },
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
        if self.jellyfin.borrow().is_some() {
            self.jellyfin_changes(&changed);
            // Reported once mpv has said, in this batch, where playback
            // began and what plays: at the file's load it knows neither.
            if restarted && self.jellyfin_start_pending.replace(false) {
                self.jellyfin_started();
                self.check_segments();
            }
        }
        if self.from_section.get() && self.section_video_over(&changed) && self.on_last_entry() {
            self.player().command(&["stop"]);
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
            self.clear_lyrics(false);
            self.leave_music_mode(false);
            self.restore_library_size();
            // What played may have moved on the server's progress, which
            // the library shows again now; after a Jellyfin item, once the
            // server has taken the report of its end. Deferred: it reads
            // the state borrowed here.
            if let Some(library) = self.library() {
                let stopping = self.jellyfin_stopping.get();
                Timer::single_shot(Duration::ZERO, move || library.shown_again(stopping));
            }
            if let Some(youtube) = self.youtube() {
                Timer::single_shot(Duration::ZERO, move || youtube.shown_again());
            }
            self.from_section.set(false);
            self.section_list.borrow_mut().clear();
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
            let idle_changed = any(&[Prop::Idle]);
            Timer::single_shot(Duration::ZERO, move || {
                app.playback_changed();
                // The media controls follow a window that has something
                // to play over one that has nothing.
                if idle_changed {
                    app.retarget_media();
                }
            });
        }
        if any(&[Prop::PausedForCache, Prop::CacheBuffering, Prop::Idle]) {
            playback.set_buffering(!state.idle && (state.paused_for_cache || self.loading.get()));
            playback.set_buffering_percent(state.cache_buffering as i32);
        }
        if any(&[Prop::PausedForCache, Prop::Path, Prop::Idle]) {
            // A Jellyfin item opens once its server has answered.
            let waiting = session::parse_entry(&state.path)
                .filter(|_| self.loading.get() && !state.idle)
                .and_then(|(server, _)| self.app().jellyfin().server(server))
                .map(|server| format!("Waiting for {}…", server.name))
                .or_else(|| {
                    // yt-dlp's answer takes seconds, longer on a bad day.
                    (self.from_section.get()
                        && self.loading.get()
                        && !state.idle
                        && util::is_url(&state.path))
                    .then(|| "Waiting for YouTube…".to_owned())
                })
                .unwrap_or_default();
            playback.set_loading_text(waiting.into());
        }
        if any(&[Prop::Seekable]) {
            playback.set_seekable(state.seekable);
        }
        if any(&[Prop::TimePos]) {
            playback.set_position(state.position as f32);
            if let Some(app) = self.app.upgrade()
                && app.is_media_target(self)
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
            // Like IINA, local files are titled by their file name. A
            // Jellyfin item by its own, which mpv has only once the load
            // hook has asked the server: before, it would show the item's
            // id.
            let jellyfin = session::parse_entry(&state.path)
                .and_then(|_| self.app().entry_title(&state.path))
                .or_else(|| self.section_title(&state.path));
            let title = if let Some(title) = jellyfin {
                title
            } else if state.path.is_empty() || util::is_url(&state.path) {
                state.title.clone()
            } else {
                file_name(&state.path)
            };
            playback.set_title(title.into());
            playback.set_path(state.path.clone().into());
        }
        if any(&[Prop::Path, Prop::Idle]) {
            self.show_watch_later(&state);
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
            media.set_audio_tracks(self.audio_track_model(&state));
            media.set_sub_tracks(self.sub_track_model(&state));
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
            && app.is_media_target(self)
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
        let timeout = self.app().prefs().osd_timeout;
        self.show_osd_for(text, detail, progress, timeout);
    }

    /// An OSD message shown for at least `seconds`: one that explains
    /// something, such as a sign-in that won't be kept, can't be read in
    /// the second a playback message lasts.
    pub fn show_message(&self, text: &str, detail: &str, seconds: f32) {
        let timeout = self.app().prefs().osd_timeout.max(seconds);
        self.show_osd_for(text, detail, None, timeout);
    }

    fn show_osd_for(&self, text: &str, detail: &str, progress: Option<f64>, timeout: f32) {
        self.osd_fallback.set(None);
        // Music mode shows its state in its own controls instead.
        if self.in_music_mode() {
            return;
        }
        if !self.app().prefs().osd_enabled {
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
            Duration::from_secs_f32(timeout.max(0.3)),
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
            if property == "sid" && this.choose_jellyfin_subtitle(id) {
                return;
            }
            // One of the source's audio streams a transcode doesn't carry.
            if property == "aid" && id < 0 {
                this.choose_jellyfin_audio(-id - 1);
                return;
            }
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
                this.open_again(path);
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
            keep_keyboard(
                &this.window,
                matches!(event, winit::event::WindowEvent::KeyboardInput { .. }),
            );
            this.style_titlebar();
            this.check_window_system();
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
        if self.leaves_item(&combo, &binding) {
            self.player().command(&["stop"]);
            return true;
        }
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

    /// Whether a key goes back to the library from a Jellyfin item playing
    /// or opening, or to the YouTube section from a video opened there, as
    /// their own Back would: Escape, outside full screen, where as bound by
    /// default it does nothing. Playing from them replaces them, and Stop was
    /// the only way back.
    fn leaves_item(&self, combo: &KeyCombo, binding: &Binding) -> bool {
        let state = self.state.borrow();
        !state.fullscreen
            && !state.idle
            && (session::parse_entry(&state.path).is_some() || self.from_section.get())
            && leaves_full_screen(combo, binding)
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
        // A step up or to the right is positive: louder, faster, later.
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
                    let delta = format!("{}", steps as f64 * prefs.seek_step / 2.0);
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
            "video-version" => self.choose_jellyfin_version(argument),
            "skip-segment" => self.skip_segment(),
            "audio-quality" => self.choose_quality(None, Some(argument)),
            "download-video" => self.download_video(),
            "toggle-watch-later" => self.toggle_watch_later(),
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
            // The library shows only in an idle window: while this one
            // plays, signing in starts in a new window rather than behind
            // the video, where it would come up when the playlist ends.
            "jellyfin-sign-in" => {
                let library = if self.is_idle() {
                    if let Some(youtube) = self.youtube() {
                        youtube.hide();
                    }
                    self.library()
                } else {
                    app.new_window().and_then(|window| window.library())
                };
                if let Some(library) = library {
                    library.begin_sign_in();
                }
            }
            // The same for the YouTube section.
            "youtube" => {
                let youtube = if self.is_idle() {
                    self.youtube()
                } else {
                    app.new_window().and_then(|window| window.youtube())
                };
                if let Some(youtube) = youtube {
                    youtube.open();
                }
            }
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
            "audio-devices" => self.request_audio_devices(),
            "dismiss-error" => {
                self.error_timer.stop();
                self.window.set_error(SharedString::new());
            }
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
            "playlist-go-to-album" => self.go_to_music(false),
            "playlist-go-to-artist" => self.go_to_music(true),
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

    /// Notes the window's size as it leaves the library to play something.
    fn remember_library_size(&self) {
        let window = self.window.window();
        let size = (!window.is_fullscreen() && !window.is_maximized() && !self.in_music_mode())
            .then(|| window.size().to_logical(window.scale_factor()));
        self.library_size.set(size);
    }

    /// The window is idle again after playing from the library: back to the
    /// size it had there, after music mode's own restoring (which waits for
    /// Slint to lift its size limits) and in its place.
    fn restore_library_size(&self) {
        let Some(size) = self.library_size.take() else {
            return;
        };
        let this = self.this.clone();
        Timer::single_shot(AFTER_LIMITS + Duration::from_millis(4), move || {
            let Some(this) = this.upgrade() else {
                return;
            };
            let window = this.window.window();
            if this.is_idle() && !window.is_fullscreen() && !window.is_maximized() {
                this.resize_window(size);
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
        match self.audio_status(state) {
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

    /// Whether the file is music, as `PlaybackState::audio_status` has it,
    /// except for a Jellyfin item: its stream is remote, which the state
    /// never counts as music, and the server says what it is.
    fn audio_status(&self, state: &PlaybackState) -> AudioStatus {
        if state.idle || session::parse_entry(&state.path).is_none() {
            return state.audio_status();
        }
        match &*self.jellyfin.borrow() {
            Some(playing) if playing.audio_only => AudioStatus::Audio,
            Some(_) => AudioStatus::NotAudio,
            None => AudioStatus::Unknown,
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
            "playlist" | "chapters" | "lyrics"
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
        // The dialog's own layout says how tall it is at this width, so
        // About's credits or a longer message still fit; it is 0 while
        // none shows.
        let dialog = self.window.get_dialog_height().ceil();
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
        // A Jellyfin item as the server describes it, with its artwork from
        // the server (whose image URLs carry no token).
        let jellyfin = self.jellyfin.borrow();
        let shown = jellyfin
            .as_ref()
            .filter(|playing| playing.entry == state.path)
            .map(|playing| &playing.shown);
        let (artist, album) = match shown {
            Some(shown) => (shown.artist.clone(), shown.album.clone()),
            None if audio => (music_artist, music_album),
            None => (
                meta(&["artist", "album_artist", "performer"]),
                meta(&["album"]),
            ),
        };
        NowPlaying {
            status: if state.paused {
                Status::Paused
            } else {
                Status::Playing
            },
            track: self.track.get(),
            title,
            artist,
            album,
            url,
            art_url: shown.map(|shown| shown.art_url.clone()).unwrap_or_default(),
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

    /// Shows what the window plays in the media controls, and where: they
    /// show this window from now on.
    pub fn publish_now_playing(&self) {
        if let Some(app) = self.app.upgrade() {
            let (now, position) = {
                let state = self.state.borrow();
                (self.now_playing(&state), state.position)
            };
            app.publish_now_playing(now);
            app.media_position(position, true);
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
        let labels: Vec<String> = state
            .playlist
            .iter()
            .map(|entry| self.entry_label(entry))
            .collect();
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
        let folder = app.source_place(&entry.filename);
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
                let label = self.entry_label(entry);
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
        media.set_selection_web(entries.iter().any(|entry| util::is_web(&entry.filename)));
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
        media.set_selection_song(match entries.as_slice() {
            [entry] => self.is_jellyfin_song(&entry.filename),
            _ => false,
        });
    }

    /// Whether a playlist entry is a Jellyfin song: as it plays, or as the
    /// library opened it.
    fn is_jellyfin_song(&self, entry: &str) -> bool {
        if session::parse_entry(entry).is_none() {
            return false;
        }
        let playing = self
            .jellyfin
            .borrow()
            .as_ref()
            .filter(|playing| playing.entry == entry)
            .map(|playing| playing.audio_only);
        playing.unwrap_or_else(|| {
            self.app()
                .jellyfin()
                .registry()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(entry)
                .is_some_and(|info| info.audio)
        })
    }

    /// Go to Album and Go to Artist: the library's page of the selected
    /// Jellyfin song's album, or its artist, in an idle window or a new
    /// one, since this one plays. The song says which they are.
    fn go_to_music(&self, artist: bool) {
        let entries = self.selected_entries();
        let [(_, entry)] = entries.as_slice() else {
            return;
        };
        let Some((server_id, item_id)) = session::parse_entry(&entry.filename) else {
            return;
        };
        let session = self.app().jellyfin();
        let Some(account) = session.account(server_id) else {
            return;
        };
        let server_id = server_id.to_owned();
        let weak = self.this.clone();
        let client = account.client;
        let asked = client.clone();
        let item_id = item_id.to_owned();
        session::load(
            move || asked.item(&item_id),
            move |result| {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                let song = match result {
                    Ok(song) => song,
                    Err(crate::jellyfin::api::Error::Unauthorized) => {
                        this.app().jellyfin().unauthorized(&server_id, &client);
                        return;
                    }
                    Err(error) => {
                        let name = this
                            .app()
                            .jellyfin()
                            .server(&server_id)
                            .map(|server| server.name);
                        this.show_message(
                            "Can't Open the Library",
                            &error.about(name.as_deref()),
                            4.0,
                        );
                        return;
                    }
                };
                let page = if artist {
                    song.album_artists
                        .first()
                        .or(song.artist_items.first())
                        .map(|artist| (artist.id.clone(), "artist"))
                } else {
                    song.album_id.clone().map(|album| (album, "album"))
                };
                let Some((id, kind)) = page else {
                    return;
                };
                let app = this.app();
                if let Some(window) = app.library_window() {
                    if let Some(library) = window.library() {
                        library.go_to(&server_id, &id, kind);
                    }
                    window.raise();
                }
            },
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
        let count = entries.len();
        // Files that can't go (in a Flatpak, those Patina may only read) stay
        // in the playlist.
        let mut refused = Vec::new();
        let movable: Vec<(usize, PlaylistEntry)> = entries
            .iter()
            .filter(
                |(_, entry)| match crate::platform::can_trash(&entry.filename) {
                    Ok(()) => true,
                    Err(error) => {
                        refused.push((entry.filename.clone(), error));
                        false
                    }
                },
            )
            .cloned()
            .collect();
        self.playlist_remove(&movable);
        let paths: Vec<String> = movable
            .iter()
            .map(|(_, entry)| entry.filename.clone())
            .collect();
        let detail = match paths.as_slice() {
            [path] => file_name(path),
            paths => format!("{} files", paths.len()),
        };
        // Said once it is done.
        crate::platform::move_to_trash(paths, move |mut failed| {
            failed.splice(0..0, refused);
            let _ = slint::invoke_from_event_loop(move || {
                App::with_current(|app| match failed.as_slice() {
                    [] => app.notify("Moved to Trash", &detail),
                    [(path, error)] if count == 1 => app.notify(
                        "Could Not Move to Trash",
                        &format!("{}: {error}", file_name(path)),
                    ),
                    failed => app.notify(
                        "Could Not Move to Trash",
                        &format!("{} of {count} files", failed.len()),
                    ),
                });
            });
        });
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
        let reload = self.jellyfin_reload.borrow_mut().take();
        let section = self.section_load();
        let links = self.app().link_sources();
        let session = self.app().jellyfin();
        let jellyfin = playback::Hook {
            servers: session.servers(),
            registry: session.registry(),
            versions: self.jellyfin_versions.borrow().clone(),
            max_bitrate: self.jellyfin_quality.get(),
            rules: playback::transcode_rules(&prefs),
            resolved: self.jellyfin_resolved.clone(),
        };
        let mpv = self.mpv.clone();
        let Some(hook) = self.player.borrow().as_ref().map(Player::hook) else {
            return;
        };
        let spawned = std::thread::Builder::new().name("subtitles".into()).spawn({
            let hook = hook.clone();
            move || {
                crate::scheme::screen_playlist_entry(&mpv, &links);
                let path = mpv
                    .get_property("path")
                    .ok()
                    .and_then(|path| path.as_str().map(str::to_owned))
                    .unwrap_or_default();
                // A Jellyfin entry is a stream the server picks, asked for
                // here: it may block, as the hook waits on this thread.
                if session::parse_entry(&path).is_some() {
                    let reload = reload
                        .filter(|(entry, _)| *entry == path)
                        .map(|(_, reload)| reload);
                    playback::prepare(&mpv, &path, &jellyfin, reload);
                } else {
                    if let Some(section) = &section {
                        apply_section(&mpv, &path, section);
                    }
                    apply_quality(&mpv, &qualities, resume_at);
                    subtitle_match::load_for_current_file(&mpv, languages.as_deref(), &choices);
                }
                hook.resume(&mpv, id);
            }
        });
        if let Err(error) = spawned {
            log::warn!("could not match subtitles: {error}");
            hook.resume(&self.mpv, id);
        }
    }

    // ---- Jellyfin -------------------------------------------------------

    /// mpv opened a file: a Jellyfin entry's playback, as the load hook
    /// planned it. Its start is reported once it has started playing.
    fn jellyfin_loaded(&self) {
        let path = self.state.borrow().path.clone();
        let kept = self
            .jellyfin_kept_segments
            .borrow_mut()
            .take()
            .filter(|(entry, _)| *entry == path)
            .map(|(_, segments)| segments);
        let resolved = self
            .jellyfin_resolved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let playing = match resolved {
            Some((entry, Ok(playing))) if entry == path => playing,
            Some((entry, _)) => {
                log::debug!("the load hook's answer for {entry} came for {path}");
                return;
            }
            None => return,
        };
        self.jellyfin_position.set(playing.start);
        self.start_trickplay(&playing);
        *self.jellyfin.borrow_mut() = Some(playing);
        self.jellyfin_tracks.set(true);
        self.jellyfin_at_end.set(false);
        self.jellyfin_start_pending.set(true);
        self.set_jellyfin_qualities();
        self.set_jellyfin_versions();
        self.fetch_segments(kept);
        self.fetch_lyrics();
    }

    /// Reports the playback's start, and its progress from then on.
    fn jellyfin_started(&self) {
        self.jellyfin_report(true);
        let weak = self.this.clone();
        self.jellyfin_timer
            .start(TimerMode::Repeated, Duration::from_secs(1), move || {
                if let Some(this) = weak.upgrade() {
                    this.jellyfin_tick();
                }
            });
    }

    /// A file ended, or the window closed: a Jellyfin playback reports its
    /// stop. For an entry that never opened, what went wrong, in words.
    fn jellyfin_ended(&self) -> Option<String> {
        self.jellyfin_timer.stop();
        self.jellyfin_tracks.set(false);
        self.reset_segments();
        let session = self.app().jellyfin();
        let never_played = self.jellyfin_start_pending.replace(false);
        if let Some(playing) = self.jellyfin.borrow_mut().take() {
            // Reported when it reached the end.
            if self.jellyfin_at_end.replace(false) {
                return None;
            }
            // Opened and stopped before it began playing, the server never
            // heard of it: its session ends, and nothing of the account's
            // changes.
            let position = self.jellyfin_position.get();
            session.report(
                &playing.server_id,
                Report::Stopped(playing.stopped(position, never_played)),
            );
            self.jellyfin_stopping.set(true);
            return None;
        }
        let path = self.state.borrow().path.clone();
        let resolved = self
            .jellyfin_resolved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        match resolved {
            // Asked of the server, and never opened: the server ends its
            // session (and any transcode) and changes nothing of the
            // account's.
            Some((entry, Ok(playing))) if entry == path => {
                session.report(
                    &playing.server_id,
                    Report::Stopped(playing.stopped(0.0, true)),
                );
                None
            }
            Some((entry, Err(failure))) if entry == path => {
                let server_id = session::parse_entry(&entry).map(|(server, _)| server.to_owned());
                let server = server_id.as_deref().and_then(|id| session.server(id));
                if let (
                    playback::Failure::Request(crate::jellyfin::api::Error::Unauthorized),
                    Some(id),
                ) = (&failure, server_id.as_deref())
                    && let Some(account) = session.account(id)
                {
                    session.unauthorized(id, &account.client);
                }
                if failure == playback::Failure::SignedOut
                    && let Some(id) = server_id.as_deref()
                {
                    self.sign_in_to_play(id, &entry);
                }
                Some(failure.message(server.map(|server| server.name).as_deref()))
            }
            _ => None,
        }
    }

    /// An entry didn't open for want of its server's sign-in: once the
    /// window is idle, the library asks for it, saying why, rather than
    /// leaving a message with nothing to do about it.
    fn sign_in_to_play(&self, server_id: &str, entry: &str) {
        let note = format!("Sign in to play {}.", self.display_name(entry));
        let server_id = server_id.to_owned();
        let weak = self.this.clone();
        Timer::single_shot(Duration::ZERO, move || {
            if let Some(this) = weak.upgrade()
                && this.is_idle()
                && let Some(library) = this.library()
            {
                library.sign_in_to(&server_id, &note);
            }
        });
    }

    /// The account the file playing comes from is being signed out (in
    /// another window): playback reports where it stopped, while the token
    /// still works, and stops, since nothing more of it would reach the
    /// server and what the playlist holds of it couldn't open.
    pub fn jellyfin_signing_out(&self, server_id: &str) {
        let playing = self
            .jellyfin
            .borrow()
            .as_ref()
            .is_some_and(|playing| playing.server_id == server_id);
        if !playing {
            return;
        }
        self.jellyfin_ended();
        self.player().command(&["stop"]);
        let name = self
            .app()
            .jellyfin()
            .server(server_id)
            .map(|server| server.name)
            .unwrap_or_default();
        self.show_message(&format!("Signed Out of {name}"), "", 3.0);
    }

    /// The library opened these entries, with what was chosen on their
    /// pages: a version chosen in this window's menu gives way to it.
    pub fn jellyfin_opened(&self, entries: &[String]) {
        let mut versions = self.jellyfin_versions.borrow_mut();
        for entry in entries {
            versions.remove(entry);
        }
    }

    /// The player's state, for a report.
    fn jellyfin_moment(&self) -> playback::Moment {
        let state = self.state.borrow();
        playback::Moment {
            position: self.jellyfin_position.get(),
            paused: state.paused,
            muted: state.muted,
            volume: state.volume,
            seekable: state.seekable,
            aid: state.aid,
            sid: state.sid,
            tracks: mpv_tracks(&state),
            loop_file: state.loop_file,
            loop_playlist: state.loop_playlist,
        }
    }

    /// Reports the playback's start, or its progress.
    fn jellyfin_report(&self, start: bool) {
        let Some(playing) = self.jellyfin.borrow().clone() else {
            return;
        };
        let progress = playing.progress(&self.jellyfin_moment());
        let report = if start {
            Report::Start(progress)
        } else {
            Report::Progress(progress)
        };
        self.app().jellyfin().report(&playing.server_id, report);
        self.jellyfin_pacer.borrow_mut().sent(Instant::now());
    }

    /// Something worth reporting changed (a pause, a seek, a track): its
    /// report goes now, or with the next tick when one went just now.
    /// Before the start has gone, the start says it.
    fn jellyfin_changed_now(&self) {
        if self.jellyfin.borrow().is_some()
            && !self.jellyfin_start_pending.get()
            && self.jellyfin_pacer.borrow_mut().change(Instant::now())
        {
            self.jellyfin_report(false);
        }
    }

    fn jellyfin_tick(&self) {
        if self.jellyfin.borrow().is_some()
            && !self.jellyfin_at_end.get()
            && !self.jellyfin_start_pending.get()
            && self.jellyfin_pacer.borrow().due(Instant::now())
        {
            self.jellyfin_report(false);
        }
    }

    /// A batch of mpv's changes while a Jellyfin file plays.
    fn jellyfin_changes(&self, changed: &HashSet<Prop>) {
        if self.jellyfin_tracks.get() && !self.state.borrow().tracks.is_empty() {
            self.select_jellyfin_tracks();
        }
        if changed.contains(&Prop::TimePos) {
            self.check_segments();
            self.follow_lyrics();
        }
        // mpv keeps a file open at its end, which for the server is where
        // playback stopped: it counts it watched (or not) from that. A seek
        // back from there plays on, as a new start.
        let at_end = self.state.borrow().eof;
        if changed.contains(&Prop::EofReached) && at_end != self.jellyfin_at_end.get() {
            self.jellyfin_at_end.set(at_end);
            if at_end {
                self.jellyfin_timer.stop();
                if let Some(playing) = self.jellyfin.borrow().clone() {
                    let position = self.jellyfin_position.get();
                    self.app().jellyfin().report(
                        &playing.server_id,
                        Report::Stopped(playing.stopped(position, false)),
                    );
                }
                self.leave_finished_playlist();
            } else {
                self.jellyfin_started();
            }
            return;
        }
        if self.jellyfin_at_end.get() {
            return;
        }
        if [Prop::Pause, Prop::Aid, Prop::Sid]
            .iter()
            .any(|prop| changed.contains(prop))
        {
            self.jellyfin_changed_now();
        }
    }

    /// The last entry of the playlist reached its end: the window goes back
    /// to the library (or the welcome view) rather than staying on the last
    /// frame, which mpv keeps; the server has where it stopped. The library
    /// shows the server's progress once the server has taken the report.
    fn leave_finished_playlist(&self) {
        if self.on_last_entry() {
            self.jellyfin_stopping.set(true);
            self.player().command(&["stop"]);
        }
    }

    /// Whether what plays is the playlist's last, and nothing goes round.
    fn on_last_entry(&self) -> bool {
        let state = self.state.borrow();
        usize::try_from(state.playlist_pos).is_ok_and(|at| at + 1 >= state.playlist.len())
            && !state.loop_file
            && !state.loop_playlist
    }

    /// Selects the server's audio and subtitle among the tracks mpv found,
    /// once per file, where the hook's selection didn't: a subtitle file
    /// that failed to load moves the others' numbers, and mpv drops a
    /// choice made for another file's tracks. What is set here ends with
    /// the file, as the hook's own does.
    fn select_jellyfin_tracks(&self) {
        self.jellyfin_tracks.set(false);
        let (selection, planned, aid, sid) = {
            let state = self.state.borrow();
            let Some(playing) = self.jellyfin.borrow().clone() else {
                return;
            };
            (
                playing.selection(&mpv_tracks(&state)),
                playing.planned(),
                state.aid,
                state.sid,
            )
        };
        let wanted = |pick, planned, current: Option<i64>| match pick {
            playback::Pick::Track(id) => (current != Some(id)).then_some(pick),
            playback::Pick::Off => current.is_some().then_some(pick),
            // The hook asked for a track mpv doesn't have as the server
            // said: mpv chooses, rather than keep what that number names.
            playback::Pick::Auto => matches!(planned, playback::Pick::Track(_)).then_some(pick),
        };
        let player = self.player();
        if let Some(pick) = wanted(selection.aid, planned.aid, aid) {
            player.set("aid", pick.value());
        }
        if let Some(pick) = wanted(selection.sid, planned.sid, sid) {
            player.set("sid", pick.value());
        }
    }

    /// The Audio menu: mpv's tracks, and for a Jellyfin transcode, which
    /// carries one audio stream that mpv knows by no name, its name and the
    /// source's other audio streams, which the server transcodes instead
    /// when one is chosen. Those have negative ids, -1 for the server's
    /// stream 0.
    fn audio_track_model(&self, state: &PlaybackState) -> ModelRc<TrackItem> {
        let model = track_model(state, TrackKind::Audio, true, false);
        let jellyfin = self.jellyfin.borrow();
        let Some(playing) = jellyfin
            .as_ref()
            .filter(|playing| playing.method == crate::jellyfin::models::PlayMethod::Transcode)
        else {
            return model;
        };
        let mut items: Vec<TrackItem> = model.iter().collect();
        if let Some(label) = playing.audio.and_then(|index| playing.stream_label(index)) {
            for item in items.iter_mut().filter(|item| item.id == 1) {
                item.title = label.clone().into();
            }
        }
        items.extend(playing.other_audio().map(|stream| {
            TrackItem {
                id: -(stream.index + 1),
                title: playing
                    .stream_label(stream.index)
                    .unwrap_or_default()
                    .into(),
                detail: "transcoded by the server".into(),
                selected: false,
            }
        }));
        ModelRc::new(VecModel::from(items))
    }

    /// A choice of one of the source's audio streams a Jellyfin transcode
    /// doesn't carry: the server is asked for it, and the file loads again
    /// where it was.
    fn choose_jellyfin_audio(&self, index: i32) {
        let Some(playing) = self.jellyfin.borrow().clone() else {
            return;
        };
        let label = playing.stream_label(index).unwrap_or_default();
        self.reload_jellyfin(
            &playing,
            |reload| reload.audio = Some(index),
            "Audio",
            label,
        );
    }

    /// The Subtitles menu: mpv's tracks, and for a Jellyfin item also the
    /// subtitles only the server can show, by burning them in. Those have
    /// negative ids, -1 for the server's stream 0.
    fn sub_track_model(&self, state: &PlaybackState) -> ModelRc<TrackItem> {
        let model = track_model(state, TrackKind::Sub, true, false);
        let jellyfin = self.jellyfin.borrow();
        let Some(playing) = jellyfin.as_ref() else {
            return model;
        };
        let burned = playing.burned_in();
        let tracks = mpv_tracks(state);
        let mut items: Vec<TrackItem> = model.iter().collect();
        for item in &mut items {
            if burned.is_some() {
                item.selected = false;
            }
            // The server's subtitle files are named for what they hold.
            let file = tracks.iter().find(|track| {
                track.kind == StreamKind::Subtitle
                    && track.external
                    && track.id == i64::from(item.id)
            });
            if let Some(label) = file
                .and_then(|track| playing.stream_index(track))
                .and_then(|index| playing.stream_label(index))
            {
                item.title = label.into();
            }
        }
        items.extend(playing.burnable().map(|stream| {
            TrackItem {
                id: -(stream.index + 1),
                title: playing
                    .stream_label(stream.index)
                    .unwrap_or_default()
                    .into(),
                detail: "burned in by the server".into(),
                selected: burned == Some(stream.index),
            }
        }));
        ModelRc::new(VecModel::from(items))
    }

    /// A choice from the Subtitles menu while a Jellyfin item plays: one
    /// the server has to burn in, or any other while it burns one in, is
    /// asked of the server, and the file loads again where it was. False
    /// for a choice mpv makes by itself.
    fn choose_jellyfin_subtitle(&self, id: i32) -> bool {
        let Some(playing) = self.jellyfin.borrow().clone() else {
            return false;
        };
        let index = if id < 0 {
            -id - 1
        } else if playing.burned_in().is_some() {
            // None, or one the user added, which mpv shows once the
            // server burns in nothing.
            let state = self.state.borrow();
            mpv_tracks(&state)
                .iter()
                .find(|track| track.kind == StreamKind::Subtitle && track.id == i64::from(id))
                .and_then(|track| playing.stream_index(track))
                .filter(|_| id > 0)
                .unwrap_or(-1)
        } else {
            return false;
        };
        if playing.burned_in() == Some(index) {
            return true;
        }
        let label = self
            .sub_track_model(&self.state.borrow())
            .iter()
            .find(|item| item.id == id)
            .map(|item| item.title.to_string())
            .unwrap_or_else(|| "None".to_owned());
        self.reload_jellyfin(
            &playing,
            |reload| reload.subtitle = Some(index),
            "Subtitle",
            label,
        );
        true
    }

    /// Fills the Video Quality menu for a Jellyfin item: Automatic, which
    /// plays the file as it is, and the bitrates below the file's that the
    /// server can transcode it to (one above it would play the file as it
    /// is all the same). The item checked says which of the two plays.
    fn set_jellyfin_qualities(&self) {
        let Some(playing) = self.jellyfin.borrow().clone() else {
            return;
        };
        if playing.audio_only {
            return;
        }
        let chosen = self.jellyfin_quality.get();
        let how = match playing.method {
            crate::jellyfin::models::PlayMethod::Transcode => "transcoded",
            _ => "original",
        };
        let title = |label: &str, selected: bool| -> SharedString {
            if selected {
                format!("{label} ({how})").into()
            } else {
                label.into()
            }
        };
        let mut items = vec![QualityItem {
            key: "auto".into(),
            title: title("Automatic", chosen.is_none()),
            selected: chosen.is_none(),
        }];
        items.extend(
            JELLYFIN_QUALITIES
                .iter()
                // The one chosen stays, whatever this file's bitrate: it is
                // the window's, for every item.
                .filter(|(bitrate, _)| {
                    chosen == Some(*bitrate) || playing.bitrate.is_none_or(|own| *bitrate < own)
                })
                .map(|(bitrate, label)| QualityItem {
                    key: bitrate.to_string().into(),
                    title: title(label, chosen == Some(*bitrate)),
                    selected: chosen == Some(*bitrate),
                }),
        );
        let globals = self.window.global::<Media>();
        globals.set_video_qualities(ModelRc::new(VecModel::from(items)));
        globals.set_audio_qualities(ModelRc::new(VecModel::default()));
    }

    /// A choice from the Video Quality menu for a Jellyfin item: it holds
    /// for every item the window plays from now on, and this one loads
    /// again where it was.
    fn choose_jellyfin_quality(&self, key: &str) {
        let Some(playing) = self.jellyfin.borrow().clone() else {
            return;
        };
        let bitrate = key.parse::<i64>().ok();
        if bitrate == self.jellyfin_quality.get() {
            return;
        }
        self.jellyfin_quality.set(bitrate);
        let label = JELLYFIN_QUALITIES
            .iter()
            .find(|(rate, _)| Some(*rate) == bitrate)
            .map_or("Automatic", |(_, label)| label);
        self.reload_jellyfin(&playing, |_| {}, "Video Quality", label.to_owned());
    }

    /// Loads a Jellyfin item again where it was, with the streams playing
    /// (as `change` leaves them) for that load only.
    fn reload_jellyfin(
        &self,
        playing: &Playing,
        change: impl FnOnce(&mut playback::Reload),
        title: &'static str,
        label: String,
    ) {
        let mut reload = {
            let state = self.state.borrow();
            let (audio, subtitle) =
                playing.reported_streams(&mpv_tracks(&state), state.aid, state.sid);
            playback::Reload {
                at: state.position,
                audio,
                subtitle,
            }
        };
        change(&mut reload);
        *self.jellyfin_reload.borrow_mut() = Some((playing.entry.clone(), reload));
        *self.jellyfin_kept_segments.borrow_mut() = Some((
            playing.entry.clone(),
            self.jellyfin_segments.borrow().clone(),
        ));
        *self.quality_message.borrow_mut() = Some((playing.entry.clone(), title, label.clone()));
        self.player().command(&["playlist-play-index", "current"]);
        // The reload changes tracks, which isn't news.
        self.last_user_action.set(None);
        self.show_osd(title, &format!("{label}…"), None);
    }

    /// Fills the Version menu with the item's versions, when it has more
    /// than one.
    fn set_jellyfin_versions(&self) {
        let Some(playing) = self.jellyfin.borrow().clone() else {
            return;
        };
        let items: Vec<QualityItem> = playing
            .versions
            .iter()
            .map(|version| QualityItem {
                key: version.id.clone().into(),
                title: version.name.clone().into(),
                selected: version.id == playing.media_source_id,
            })
            .collect();
        self.window
            .global::<Media>()
            .set_video_versions(ModelRc::new(VecModel::from(items)));
    }

    /// A choice from the Version menu: the item loads again where it was,
    /// in that version, with the server's choice of its tracks (the
    /// tracks playing now are numbered as the old version numbers them).
    fn choose_jellyfin_version(&self, id: &str) {
        let Some(playing) = self.jellyfin.borrow().clone() else {
            return;
        };
        let Some(version) = playing
            .versions
            .iter()
            .find(|version| version.id == id && version.id != playing.media_source_id)
        else {
            return;
        };
        self.jellyfin_versions
            .borrow_mut()
            .insert(playing.entry.clone(), version.id.clone());
        self.reload_jellyfin(
            &playing,
            |reload| {
                reload.audio = None;
                reload.subtitle = None;
            },
            "Version",
            version.name.clone(),
        );
    }

    // ---- Jellyfin media segments ----------------------------------------

    fn reset_segments(&self) {
        *self.jellyfin_segments.borrow_mut() = Segments::default();
        self.offer_segment(None);
    }

    /// Asks the server for the Jellyfin file's media segments: whatever a
    /// plugin on it found. A video's only. `kept` are the segments of the
    /// playback this one reloads, whose spans skipped without asking stay
    /// skipped: one the user went back into isn't skipped again.
    fn fetch_segments(&self, kept: Option<Segments>) {
        let Some(playing) = self.jellyfin.borrow().clone() else {
            return;
        };
        let session = self.app().jellyfin();
        let Some(account) = session
            .account(&playing.server_id)
            .filter(|_| !playing.audio_only)
        else {
            return;
        };
        let track = self.track.get();
        let weak = self.this.clone();
        let client = account.client;
        session::load(
            {
                let client = client.clone();
                let item_id = playing.item_id.clone();
                move || client.media_segments(&item_id)
            },
            move |result| {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                // Another file (or the same again) has started since.
                if this.track.get() != track || this.jellyfin.borrow().is_none() {
                    return;
                }
                match result {
                    Ok(found) => {
                        let duration = playing.duration.or_else(|| {
                            let duration = this.state.borrow().duration;
                            (duration > 0.0).then_some(duration)
                        });
                        let mut segments = Segments::new(&found, duration);
                        if let Some(kept) = &kept {
                            segments.keep_skipped(kept);
                        }
                        *this.jellyfin_segments.borrow_mut() = segments;
                        this.check_segments();
                    }
                    Err(crate::jellyfin::api::Error::Unauthorized) => {
                        this.app()
                            .jellyfin()
                            .unauthorized(&playing.server_id, &client);
                    }
                    Err(error) => log::debug!("no media segments for {}: {error}", playing.entry),
                }
            },
        );
    }

    /// The position moved: a segment playing shows its Skip button, or is
    /// skipped, as the preferences say for its kind. Not before the file
    /// has started playing (mpv can't seek it yet), nor as it ends, when
    /// mpv no longer says where it is.
    fn check_segments(&self) {
        if self.jellyfin_segments.borrow().is_empty()
            || self.jellyfin_start_pending.get()
            || !self.position_known.get()
        {
            return;
        }
        let position = self.state.borrow().position;
        let prefs = self.app().prefs();
        let now = self
            .jellyfin_segments
            .borrow_mut()
            .at(position, |kind| segments::Action::of(&prefs, kind));
        match now {
            segments::Now::Nothing => self.offer_segment(None),
            segments::Now::Offer(span) => self.offer_segment(Some(span)),
            segments::Now::Skip(span) => {
                log::debug!("skipping {:?} to {:.1}", span.kind, span.end);
                self.offer_segment(None);
                self.seek_past(span);
            }
        }
    }

    fn offer_segment(&self, span: Option<Span>) {
        if self.jellyfin_offer.get() == span {
            return;
        }
        self.jellyfin_offer.set(span);
        let label = span.map_or("", |span| segments::button_label(span.kind));
        // The key that does what the button does, which nothing else shows.
        let key = span
            .and_then(|_| {
                self.app()
                    .bindings()
                    .iter()
                    .find(|(_, binding)| {
                        matches!(binding, Binding::Patina(action) if action == "skip-segment")
                    })
                    .map(|(combo, _)| combo.label())
            })
            .unwrap_or_default();
        let playback = self.window.global::<Playback>();
        playback.set_skip_key(key.into());
        playback.set_skip_label(label.into());
    }

    /// The Skip button, or a key bound to it: playback goes on at the end
    /// of the segment it offers.
    fn skip_segment(&self) {
        if let Some(span) = self.jellyfin_offer.get() {
            self.offer_segment(None);
            self.seek_past(span);
        }
    }

    /// Exact, since a seek to the keyframe before the end would land in
    /// the segment again. Credits that run to the end end the file, and the
    /// playlist plays on. The OSD says what was skipped, in the place of
    /// the seek's (the position before it, a key having been pressed just
    /// now) or the chapter's.
    fn seek_past(&self, span: Span) {
        self.last_user_action.set(None);
        self.player()
            .command_quiet(&["seek", &format!("{:.3}", span.end), "absolute+exact"]);
        self.show_osd(segments::skipped_label(span.kind), "", None);
    }

    // ---- Jellyfin lyrics -------------------------------------------------

    /// Empties the playlist's Lyrics tab, and with `keep_tab` leaves the
    /// tab, while the next file opens: between two songs with lyrics, the
    /// playlist would show for a moment in their place.
    fn clear_lyrics(&self, keep_tab: bool) {
        self.jellyfin_lyrics.borrow_mut().take();
        self.lyric_line.set(None);
        let media = self.window.global::<Media>();
        if media.get_lyrics().row_count() > 0 {
            media.set_lyrics(ModelRc::default());
        }
        media.set_lyric(-1);
        if !keep_tab {
            media.set_has_lyrics(false);
        }
    }

    /// Asks the server for the Jellyfin song's lyrics, when it has some.
    /// The Lyrics tab shows from now, its lines once they have come.
    fn fetch_lyrics(&self) {
        let Some(playing) = self.jellyfin.borrow().clone() else {
            return;
        };
        let session = self.app().jellyfin();
        let Some(account) = session
            .account(&playing.server_id)
            .filter(|_| playing.has_lyrics)
        else {
            self.clear_lyrics(false);
            return;
        };
        self.window.global::<Media>().set_has_lyrics(true);
        let track = self.track.get();
        let weak = self.this.clone();
        let client = account.client;
        session::load(
            {
                let client = client.clone();
                let item_id = playing.item_id.clone();
                move || client.lyrics(&item_id)
            },
            move |result| {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                // Another file (or the same again) has started since.
                if this.track.get() != track || this.jellyfin.borrow().is_none() {
                    return;
                }
                let lyrics = match result {
                    Ok(found) => found.as_ref().and_then(Lyrics::new),
                    Err(crate::jellyfin::api::Error::Unauthorized) => {
                        this.app()
                            .jellyfin()
                            .unauthorized(&playing.server_id, &client);
                        None
                    }
                    Err(error) => {
                        log::debug!("no lyrics for {}: {error}", playing.entry);
                        None
                    }
                };
                this.show_lyrics(lyrics);
            },
        );
    }

    fn show_lyrics(&self, lyrics: Option<Lyrics>) {
        let Some(lyrics) = lyrics else {
            self.clear_lyrics(false);
            return;
        };
        let lines: Vec<LyricItem> = lyrics
            .lines
            .iter()
            .enumerate()
            .map(|(index, line)| LyricItem {
                text: line.text.clone().into(),
                time: lyrics.plays_from(index).map_or(-1.0, |start| start as f32),
            })
            .collect();
        let media = self.window.global::<Media>();
        media.set_lyrics_timed(lyrics.timed);
        media.set_lyrics(ModelRc::new(VecModel::from(lines)));
        media.set_lyric(-1);
        media.set_has_lyrics(true);
        self.lyric_line.set(None);
        *self.jellyfin_lyrics.borrow_mut() = Some(lyrics);
        self.follow_lyrics();
    }

    /// The position moved: timed lyrics follow it to the line being sung.
    /// Not at the file's end, where mpv no longer says where it is.
    fn follow_lyrics(&self) {
        let line = match &*self.jellyfin_lyrics.borrow() {
            Some(lyrics) if self.position_known.get() => {
                lyrics.line_at(self.state.borrow().position)
            }
            _ => return,
        };
        if self.lyric_line.replace(line) != line {
            self.window
                .global::<Media>()
                .set_lyric(line.map_or(-1, |line| line as i32));
        }
    }

    /// Seek-bar previews from the server's trickplay images, when the
    /// server has them for the version playing and previews are on.
    fn start_trickplay(&self, playing: &Playing) {
        let prefs = self.app().prefs();
        if !prefs.thumbnails || playing.audio_only {
            return;
        }
        let Some(account) = self.app().jellyfin().account(&playing.server_id) else {
            return;
        };
        let width = prefs.thumbnail_width.clamp(40, 600) * 2;
        *self.jellyfin_tiles.borrow_mut() = trickplay::Tiles::new(account.client, playing, width);
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
        if let Some(key) = video
            && self.jellyfin.borrow().is_some()
        {
            self.choose_jellyfin_quality(key);
            return;
        }
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
                &if self.from_section.get() {
                    self.section_filter()
                } else {
                    self.app().prefs().ytdl_video_filter()
                },
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
        // One opened from the section goes as it does from there.
        let video = self
            .from_section
            .get()
            .then(|| VideoId::from_watch_url(&self.state.borrow().path))
            .flatten();
        // Only an `ffmpeg-location` of the user's matters here, and a video
        // from the section takes none of their options.
        let prefs = self.app().prefs();
        let raw = if video.is_some() {
            prefs.ytdl_options_for_section()
        } else {
            prefs.ytdl_raw_options.clone()
        };
        let ffmpeg = crate::downloads::Ffmpeg::for_options(&raw);
        // Refused as the section refuses it, though only for the package's
        // ffmpeg, which can't record one; the user's own may.
        if media.live && !ffmpeg.records_live() {
            log::info!("not downloading {}: a live stream", media.title);
            self.show_message(
                "Can't Download a Live Stream",
                "It can be downloaded once it has ended.",
                4.0,
            );
            return;
        }
        let request = crate::downloads::Request {
            url: if media.url.is_empty() {
                self.state.borrow().path.clone()
            } else {
                media.url.clone()
            },
            title: media.title.clone(),
            format: crate::downloads::download_format(media.format_id.as_deref(), &ffmpeg),
            video,
        };
        if self.app().download(request) {
            self.show_osd("Downloading", &media.title, None);
        }
    }

    // ---- The YouTube section's videos -----------------------------------

    fn section_title(&self, path: &str) -> Option<String> {
        if !self.from_section.get() {
            return None;
        }
        self.app().youtube().title_of(path)
    }

    /// The YouTube video playing, if it is one: by its one address however it
    /// was opened, and by its file when the section opened that.
    fn video_playing(&self, state: &PlaybackState) -> Option<VideoId> {
        if state.idle {
            return None;
        }
        VideoId::from_watch_url(&state.path).or_else(|| {
            self.from_section
                .get()
                .then(|| self.app().youtube().video_at(&state.path))
                .flatten()
        })
    }

    fn start_sitting(&self) {
        let video = self.video_playing(&self.state.borrow());
        *self.sitting.borrow_mut() = video.map(Sitting::new);
    }

    /// A video's time in the player is over: its mark and progress strip learn
    /// how it went.
    fn end_sitting(&self) {
        let seen = self.sitting.borrow_mut().take().and_then(Sitting::end);
        if let Some(seen) = seen {
            let app = self.app();
            app.youtube().note_watch(&app.prefs(), &seen);
        }
    }

    fn section_video_over(&self, changed: &HashSet<Prop>) -> bool {
        let live = self
            .online
            .borrow()
            .as_ref()
            .is_some_and(|media| media.live);
        video_over(&self.state.borrow(), changed, live)
    }

    /// One thing mpv's yt-dlp hook said went wrong. The first that says anything
    /// is what happened.
    fn hook_said(&self, text: &str) {
        let mut failure = self.hook_failure.borrow_mut();
        if failure.is_none() {
            *failure = crate::youtube::ytdlp::hook_failure(text);
        }
    }

    /// A video from the section didn't open. The section says why as it comes
    /// back. What is wrong with one video leaves the next in a list to try;
    /// what stops this one, a block or the network, stops those too, and there
    /// is no use in asking for each.
    fn section_video_failed(
        &self,
        name: &str,
        hook: Option<crate::youtube::Error>,
        mpv_said: Option<String>,
    ) {
        let stop = hook.as_ref().is_some_and(|hook| {
            !hook.kind.about_the_video() && hook.kind != crate::youtube::Failure::Failed
        });
        let detail = hook
            .map(|hook| hook.said())
            .or(mpv_said)
            .unwrap_or_default();
        // Opened from Recent, with no section here to say it.
        match self.youtube().filter(|youtube| youtube.is_shown()) {
            Some(youtube) => youtube.note(format!("Could not play {name}. {detail}")),
            None => self.show_load_error(format!("Could not open {name}\n{detail}")),
        }
        if stop {
            self.player().command(&["stop"]);
        } else if !self.on_last_entry() {
            self.show_osd("Could not play", name, None);
        }
    }

    /// The largest the section's default goes, for the screen the window is on
    /// now.
    fn section_filter(&self) -> String {
        let screen = self
            .window
            .window()
            .with_winit_window(|winit| {
                let size = winit.current_monitor()?.size();
                Some((size.width, size.height))
            })
            .flatten();
        playing::video_filter(&self.app().prefs().youtube_quality, screen)
    }

    /// The format a video from the section downloads in: the yt-dlp format
    /// preference, or else the quality it plays in. It always says, or yt-dlp
    /// would fetch every video at its largest.
    pub fn section_download_format(&self) -> String {
        let prefs = self.app().prefs();
        let own = prefs.ytdl_format.trim();
        if !own.is_empty() {
            return own.to_owned();
        }
        playing::download_format(
            &self.section_filter(),
            crate::downloads::Ffmpeg::for_options(&prefs.ytdl_options_for_section()).found(),
        )
    }

    /// What the load hook needs for an entry the section sent, gathered here
    /// on the interface thread, where the marks and the screen are.
    fn section_load(&self) -> Option<SectionLoad> {
        if !self.from_section.get() {
            return None;
        }
        let app = self.app();
        let prefs = app.prefs();
        let session = app.youtube();
        let entries = self
            .section_list
            .borrow()
            .iter()
            .map(|source| {
                let start = session
                    .video_at(source)
                    .and_then(|id| session.resume_from(&prefs, &id));
                (
                    source.clone(),
                    SectionEntry {
                        title: session.title_of(source),
                        start,
                    },
                )
            })
            .collect();
        Some(SectionLoad {
            format: prefs
                .ytdl_format
                .trim()
                .is_empty()
                .then(|| playing::play_format(&self.section_filter())),
            options: Secret(prefs.ytdl_options_for_section().as_str().into()),
            entries,
        })
    }

    /// Whether what plays is on Watch Later, for the menu's item.
    fn show_watch_later(&self, state: &PlaybackState) {
        let media = self.window.global::<Media>();
        let video = self.video_playing(state);
        media.set_youtube_video(video.is_some());
        media.set_on_watch_later(video.is_some_and(|video| self.app().youtube().is_saved(&video)));
    }

    /// The YouTube section's lists have changed.
    pub fn watch_later_changed(&self) {
        self.show_watch_later(&self.state.borrow());
    }

    /// File › Save to Watch Later, for the video playing, with what yt-dlp said
    /// of it.
    fn toggle_watch_later(&self) {
        let Some(id) = self.video_playing(&self.state.borrow()) else {
            return;
        };
        let session = self.app().youtube();
        let title = self.state.borrow().title.clone();
        if session.is_saved(&id) {
            session.unsave_video(&id);
            self.show_osd("Removed from Watch Later", &title, None);
            return;
        }
        let mut video = crate::youtube::Video::new(id);
        video.title = crate::youtube::clean_text(&title);
        if let Some(media) = self.online.borrow().as_ref() {
            video.channel = media
                .channel
                .as_deref()
                .map(crate::youtube::clean_text)
                .unwrap_or_default();
            video.channel_id = media
                .channel_id
                .as_deref()
                .and_then(crate::youtube::ChannelId::parse);
        }
        let duration = self.state.borrow().duration;
        if duration > 0.0 {
            video.duration = Some(duration as u64);
        }
        session.save_video(video);
        self.show_osd("Saved to Watch Later", &title, None);
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
        self.jellyfin_tiles.borrow_mut().take();
        self.window.global::<Playback>().set_preview_ready(false);
    }

    /// Starts making thumbnails once a local video's duration and size are
    /// known, as IINA does (never for streams or audio). A Jellyfin item's
    /// come from the server, as its trickplay images (`start_trickplay`).
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

        self.preview_time.set(time);
        let tiles = self.jellyfin_tiles.borrow().clone();
        if let Some(tiles) = tiles {
            let weak = self.this.clone();
            let thumbnail = tiles.thumbnail(time, move || {
                if let Some(this) = weak.upgrade() {
                    this.show_preview(this.preview_time.get());
                }
            });
            match thumbnail {
                Some(image) => {
                    playback.set_preview_image(image);
                    playback.set_preview_ready(true);
                }
                None => playback.set_preview_ready(false),
            }
            return;
        }
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
                detail: app.source_place(&path).into(),
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
        self.end_sitting();
        {
            let state = self.state.borrow();
            if !state.idle {
                self.app()
                    .update_history(&state.path, state.position, state.duration);
            }
            self.app().update_prefs(|prefs| prefs.volume = state.volume);
        }
        // The window's end is the playback's: mpv says nothing more once
        // it is shut down.
        self.jellyfin_ended();
        self.reset_thumbnails();
        self.library.borrow_mut().take();
        if let Some(youtube) = self.youtube.borrow_mut().take() {
            youtube.closed();
        }
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

/// A video paused this close to its end has reached it.
const SECTION_END_MARGIN: f64 = 0.25;

/// Whether what plays has ended, mpv keeping its last frame. Seeking past the
/// end doesn't always say so: mpv can go back a step for a frame to show and
/// only pause there. A live stream is always that close to its end, so pausing
/// one is only pausing.
fn video_over(state: &PlaybackState, changed: &HashSet<Prop>, live: bool) -> bool {
    let ended = changed.contains(&Prop::EofReached) && state.eof;
    let stuck = (changed.contains(&Prop::Pause) || changed.contains(&Prop::TimePos))
        && !live
        && state.paused
        && state.duration > 0.0
        && state.position >= state.duration - SECTION_END_MARGIN;
    ended || stuck
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

/// What the load hook gives entries the YouTube section sent.
struct SectionLoad {
    /// None when the yt-dlp format preference takes the place of a quality.
    format: Option<String>,
    /// Secret, as it can hold a proxy's password.
    options: Secret,
    entries: HashMap<String, SectionEntry>,
}

struct SectionEntry {
    title: Option<String>,
    /// Where it was left, to pick up from.
    start: Option<f64>,
}

/// From the load hook, before mpv's yt-dlp hook: how an entry of the section's
/// opens. All of it lasts as long as the file. A choice from the quality menus
/// goes on top of this (`apply_quality`).
fn apply_section(mpv: &Mpv, path: &str, section: &SectionLoad) {
    let Some(entry) = section.entries.get(path) else {
        return;
    };
    let set = |name: &str, value: &str| {
        if let Err(error) = mpv.set_property(name, value) {
            log::warn!("{error}");
        }
    };
    // mpv's hook leaves a title it finds there, which then shows from the start
    // rather than an address until yt-dlp has answered.
    if let Some(title) = &entry.title {
        set("file-local-options/force-media-title", title);
    }
    if let Some(start) = entry.start {
        set("file-local-options/start", &format!("{start:.3}"));
    }
    if util::is_url(path) {
        if let Err(error) =
            mpv.set_property_secret("file-local-options/ytdl-raw-options", &section.options)
        {
            log::warn!("{error}");
        }
        if let Some(format) = &section.format {
            set("file-local-options/ytdl-format", format);
        }
    } else {
        // A downloaded video is one of the list, not the first of a folder.
        set("file-local-options/autocreate-playlist", "no");
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

/// Where `PlayerController::open_at` puts sources in mpv's playlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    /// In place of the playlist, from the first.
    Replace,
    /// In place of the playlist, from the one at this index, with Shuffle
    /// off.
    InOrder(usize),
    /// The same for what the YouTube section sends, which it comes back to.
    Section(usize),
    /// In place of the playlist, shuffled, with Shuffle on.
    Shuffled,
    /// At the end, playing if nothing does.
    Append,
}

/// How `count` sources go into mpv's playlist, as (source, `loadfile`
/// mode, index). Appended, at the end, playing if nothing does. In place of
/// the playlist, from the one at `start`: it replaces the playlist, and
/// those before it go in ahead of it, so that mpv, which takes them all
/// before it opens anything, starts loading no other. To be shuffled: at
/// the end of a playlist cleared before, in their order, none playing.
fn load_steps(count: usize, placement: Placement) -> Vec<(usize, &'static str, Option<usize>)> {
    let start = match placement {
        Placement::Append => {
            return (0..count)
                .map(|index| (index, "append-play", None))
                .collect();
        }
        Placement::Shuffled => return (0..count).map(|index| (index, "append", None)).collect(),
        Placement::Replace => 0,
        Placement::InOrder(start) | Placement::Section(start) => start,
    };
    let Some(last) = count.checked_sub(1) else {
        return Vec::new();
    };
    let start = start.min(last);
    let mut steps = vec![(start, "replace", None)];
    steps.extend((0..start).map(|index| (index, "insert-at", Some(index))));
    steps.extend((start + 1..count).map(|index| (index, "append", None)));
    steps
}

/// How Play Next (`next`) and Add to Queue add to a window's playlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Queueing {
    /// As a new playlist, as if played.
    Open,
    /// Right after the entry at this index.
    After(usize),
    /// At the end.
    End,
}

/// How what is queued goes into a window's playlist, whose entry playing is
/// at `playing`. A window with nothing to play (`idle`) opens it as a new
/// playlist, as Play would: added to the end of what ended, it would start
/// paused when that was stopped paused, and the library would come back at
/// the size the window was fitted to for it rather than its own.
fn queueing(idle: bool, next: bool, playing: Option<usize>) -> Queueing {
    match playing {
        _ if idle => Queueing::Open,
        Some(at) if next => Queueing::After(at),
        _ => Queueing::End,
    }
}

/// How `count` sources go in after the entry playing, at `at`, in their
/// order: Play Next.
fn queue_steps(count: usize, at: usize) -> Vec<(usize, &'static str, Option<usize>)> {
    (0..count)
        .map(|index| (index, "insert-at", Some(at + 1 + index)))
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

/// Escape as bound by default, which only leaves full screen.
fn leaves_full_screen(combo: &KeyCombo, binding: &Binding) -> bool {
    combo.mods == 0
        && combo.key == "ESC"
        && matches!(binding, Binding::Mpv(commands)
            if commands == &[vec!["set".to_owned(), "fullscreen".to_owned(), "no".to_owned()]])
}

/// mpv's tracks, as a Jellyfin playback maps them to the server's streams.
fn mpv_tracks(state: &PlaybackState) -> Vec<playback::MpvTrack> {
    state
        .tracks
        .iter()
        .map(|track| playback::MpvTrack {
            kind: match track.kind {
                TrackKind::Video => StreamKind::Video,
                TrackKind::Audio => StreamKind::Audio,
                TrackKind::Sub => StreamKind::Subtitle,
            },
            id: track.id,
            external: track.external,
            filename: track
                .external
                .then(|| track.external_filename.clone())
                .filter(|name| !name.is_empty()),
        })
        .collect()
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
    fn a_video_is_over_at_its_end_however_it_got_there() {
        let changed = |props: &[Prop]| props.iter().copied().collect::<HashSet<Prop>>();
        let mut state = PlaybackState {
            duration: 20.0,
            position: 10.0,
            ..Default::default()
        };
        assert!(!video_over(&state, &changed(&[Prop::TimePos]), false));
        // Paused anywhere else is only paused.
        state.paused = true;
        assert!(!video_over(&state, &changed(&[Prop::Pause]), false));
        // Sought past the end, when mpv pauses there without saying it ended.
        state.position = 20.0;
        assert!(video_over(&state, &changed(&[Prop::Pause]), false));
        assert!(video_over(&state, &changed(&[Prop::TimePos]), false));
        assert!(!video_over(&state, &changed(&[Prop::Volume]), false));
        // A live stream is always at its end.
        assert!(!video_over(&state, &changed(&[Prop::Pause]), true));
        // Played to the end.
        state.eof = true;
        assert!(video_over(&state, &changed(&[Prop::EofReached]), true));
        // Nothing is known of how long it is.
        let unknown = PlaybackState {
            paused: true,
            ..Default::default()
        };
        assert!(!video_over(&unknown, &changed(&[Prop::Pause]), false));
    }

    /// A list from the YouTube section goes in as one from the library does.
    #[test]
    fn a_list_from_the_section_keeps_its_order() {
        assert_eq!(
            load_steps(5, Placement::Section(2)),
            load_steps(5, Placement::InOrder(2))
        );
    }

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
    fn escape_bound_by_default_only_leaves_full_screen() {
        let bindings = crate::input::Bindings::parse(&crate::input::default_conf());
        let escape = KeyCombo::parse("ESC").unwrap();
        assert!(leaves_full_screen(&escape, bindings.get(&escape).unwrap()));
        // Bound otherwise, it keeps doing what the user asked.
        let quit = Binding::Mpv(vec![vec!["quit".to_owned()]]);
        assert!(!leaves_full_screen(&escape, &quit));
        let enter = KeyCombo::parse("ENTER").unwrap();
        assert!(!leaves_full_screen(&enter, bindings.get(&enter).unwrap()));
    }

    /// The library takes the keyboard's focus as it shows. Once it has gone,
    /// for the welcome screen or for a file, the window's shortcuts still
    /// reach the window: after signing out they reached nothing, Ctrl+Q
    /// included, until Tab gave the focus back.
    #[test]
    fn shortcuts_reach_the_window_after_the_library_goes() {
        use slint::platform::{Key, WindowEvent};
        i_slint_backend_testing::init_no_event_loop();
        let window = PlayerWindow::new().unwrap();
        let quits = Rc::new(Cell::new(0));
        let counted = quits.clone();
        window
            .global::<Actions>()
            .on_key(move |text, _, ctrl, _, _, _| {
                if text == "q" && ctrl {
                    counted.set(counted.get() + 1);
                }
                true
            });
        window.show().unwrap();
        let ctrl_q = || {
            let control = SharedString::from(Key::Control);
            let q = SharedString::from("q");
            for text in [&control, &q] {
                let text = text.clone();
                window
                    .window()
                    .dispatch_event(WindowEvent::KeyPressed { text });
            }
            for text in [q, control] {
                window
                    .window()
                    .dispatch_event(WindowEvent::KeyReleased { text });
            }
        };
        for how in ["signed out", "a file opened"] {
            window.global::<Playback>().set_has_media(false);
            window.global::<Library>().set_shown(true);
            let before = quits.get();
            ctrl_q();
            assert_eq!(quits.get(), before + 1, "in the library, before {how}");
            if how == "signed out" {
                window.global::<Library>().set_shown(false);
            } else {
                window.global::<Playback>().set_has_media(true);
            }
            ctrl_q();
            assert_eq!(quits.get(), before + 2, "after the library went: {how}");
        }
    }

    /// The YouTube section takes the keyboard as it shows, from the welcome
    /// screen or from the library. However it goes, and whatever comes in its
    /// place, the window's shortcuts still reach the window.
    #[test]
    fn shortcuts_reach_the_window_as_the_youtube_section_comes_and_goes() {
        use slint::platform::{Key, WindowEvent};
        i_slint_backend_testing::init_no_event_loop();
        let window = PlayerWindow::new().unwrap();
        let quits = Rc::new(Cell::new(0));
        let counted = quits.clone();
        window
            .global::<Actions>()
            .on_key(move |text, _, ctrl, _, _, _| {
                let quit = text == "q" && ctrl;
                if quit {
                    counted.set(counted.get() + 1);
                }
                quit
            });
        window.show().unwrap();
        let ctrl_q = || {
            let keys = [SharedString::from(Key::Control), SharedString::from("q")];
            for text in &keys {
                let text = text.clone();
                window
                    .window()
                    .dispatch_event(WindowEvent::KeyPressed { text });
            }
            for text in keys.iter().rev() {
                let text = text.clone();
                window
                    .window()
                    .dispatch_event(WindowEvent::KeyReleased { text });
            }
        };
        let works = |how: &str| {
            let before = quits.get();
            ctrl_q();
            assert_eq!(quits.get(), before + 1, "{how}");
        };
        let youtube = window.global::<YouTube>();
        let library = window.global::<Library>();
        youtube.set_page("feed".into());
        works("on the welcome screen");

        youtube.set_shown(true);
        works("in the section");
        youtube.set_shown(false);
        works("back on the welcome screen");

        youtube.set_shown(true);
        window.global::<Playback>().set_has_media(true);
        works("a video opened from the section");
        window.global::<Playback>().set_has_media(false);
        works("back in the section after the video");

        // Over the library, This Computer included, which comes back as it was.
        youtube.set_shown(false);
        library.set_page("local".into());
        library.set_shown(true);
        works("in the library");
        youtube.set_shown(true);
        works("in the section, over the library");
        for page in ["search", "saved", "channels", "feed", "channel"] {
            youtube.set_page(page.into());
            works(page);
        }
        youtube.set_page("feed".into());
        youtube.set_shown(false);
        works("back in the library");
        library.set_shown(false);
        works("on the welcome screen again");
    }

    /// A page of the section that loads again, or a list that loses lines as an
    /// update ends, makes its cards again. Slint would leave the keyboard's
    /// focus on a gone card, which keep_keyboard finds. A field being typed in
    /// keeps it through all that, and a dialog over the section keeps it too.
    #[test]
    fn shortcuts_reach_the_window_after_the_section_makes_its_cards_again() {
        use crate::{YouTubeCard, YouTubeLine};
        use slint::platform::Key;
        i_slint_backend_testing::init_no_event_loop();
        let window = PlayerWindow::new().unwrap();
        let resize = |width| {
            window
                .window()
                .set_size(slint::PhysicalSize::new(width, 800))
        };
        resize(1280);
        let press = |keys: &[SharedString]| press_keys(&window, keys);
        let ctrl_q = [SharedString::from(Key::Control), "q".into()];
        let quits = Rc::new(Cell::new(0));
        let counted = quits.clone();
        window
            .global::<Actions>()
            .on_key(move |text, _, ctrl, _, _, _| {
                let quit = text == "q" && ctrl;
                if quit {
                    counted.set(counted.get() + 1);
                }
                quit
            });
        let youtube = window.global::<YouTube>();
        let played = Rc::new(Cell::new(0));
        let counted = played.clone();
        youtube.on_play(move |_| counted.set(counted.get() + 1));
        let line = || {
            let card = YouTubeCard {
                id: "aaaaaaaaaaa".into(),
                title: "A Video".into(),
                progress: -1.0,
                ..Default::default()
            };
            YouTubeLine {
                id: "videos-0".into(),
                kind: "videos".into(),
                cards: ModelRc::new(VecModel::from(vec![card; 3])),
                ..Default::default()
            }
        };
        let lines = Rc::new(VecModel::from(vec![line()]));
        youtube.set_lines(lines.clone().into());
        // As the controller does, the lines are made again for other columns.
        let remade = lines.clone();
        youtube.on_layout(move |_, _| remade.set_vec(vec![line()]));
        youtube.set_page("feed".into());
        youtube.set_shown(true);
        window.show().unwrap();

        for how in ["loaded again", "resized", "lost lines"] {
            resize(1280);
            // Tab until a card has the focus, which Enter on it shows.
            let before = played.get();
            for _ in 0..12 {
                press(&[Key::Tab.into()]);
                press(&[Key::Return.into()]);
                if played.get() > before {
                    break;
                }
            }
            assert_eq!(played.get(), before + 1, "no card took the focus: {how}");
            match how {
                "loaded again" => {
                    lines.set_vec(vec![line()]);
                    youtube.set_loads(youtube.get_loads() + 1);
                }
                "resized" => resize(700),
                _ => lines.set_vec(vec![line()]),
            }
            let before = quits.get();
            press(&ctrl_q);
            assert_eq!(quits.get(), before + 1, "once made again: {how}");
        }

        // Search has its field take the keyboard, which results don't move.
        youtube.set_page("search".into());
        press(&["x".into()]);
        assert_eq!(youtube.get_search_text(), "x", "the field has the focus");
        lines.set_vec(vec![line()]);
        youtube.set_loads(youtube.get_loads() + 1);
        press(&["y".into()]);
        assert_eq!(youtube.get_search_text(), "xy", "after the results");

        // A list changing behind the Channels page's field leaves it the keys.
        youtube.set_wants_field(true);
        youtube.set_page("channels".into());
        press(&["a".into()]);
        press(&["b".into()]);
        lines.set_vec(vec![line()]);
        press(&["c".into()]);
        assert_eq!(
            youtube.get_channel_text(),
            "abc",
            "the field kept the focus"
        );

        // A dialog over the section keeps the keyboard through a load.
        youtube.set_page("feed".into());
        window.set_dialog("open-url".into());
        window.set_dialog_text(SharedString::new());
        press(&["a".into()]);
        lines.set_vec(vec![line()]);
        youtube.set_loads(youtube.get_loads() + 1);
        press(&["b".into()]);
        assert_eq!(window.get_dialog_text(), "ab", "the dialog kept the focus");
    }

    /// The mouse's back button goes back in the section, and leaves the library
    /// under it as it was: the library keeps its pages, on This Computer two of
    /// them, and a press that popped one would go unseen.
    #[test]
    fn the_back_button_goes_back_in_what_shows() {
        use slint::platform::{PointerEventButton, WindowEvent};
        i_slint_backend_testing::init_no_event_loop();
        let window = PlayerWindow::new().unwrap();
        window
            .window()
            .set_size(slint::PhysicalSize::new(1280, 800));
        window.show().unwrap();
        let library = window.global::<Library>();
        let youtube = window.global::<YouTube>();
        let library_backs = Rc::new(Cell::new(0));
        let counted = library_backs.clone();
        library.on_back(move || counted.set(counted.get() + 1));
        let youtube_backs = Rc::new(Cell::new(0));
        let counted = youtube_backs.clone();
        youtube.on_back(move || counted.set(counted.get() + 1));
        let back = || {
            let position = slint::LogicalPosition::new(600.0, 500.0);
            let button = PointerEventButton::Back;
            window
                .window()
                .dispatch_event(WindowEvent::PointerMoved { position });
            window
                .window()
                .dispatch_event(WindowEvent::PointerPressed { position, button });
            window
                .window()
                .dispatch_event(WindowEvent::PointerReleased { position, button });
        };
        library.set_page("local".into());
        library.set_can_go_back(true);
        library.set_shown(true);
        youtube.set_page("feed".into());
        youtube.set_shown(true);
        back();
        assert_eq!((library_backs.get(), youtube_backs.get()), (0, 1));
        youtube.set_shown(false);
        back();
        assert_eq!((library_backs.get(), youtube_backs.get()), (1, 1));
    }

    /// A page that loads again in place (a server switched to, Home or a
    /// tab picked again) makes its cards again, and so does a grid split
    /// into lines anew for another width. Slint left the keyboard's focus
    /// on the card that had it, gone, and every key went nowhere, Ctrl+Q's
    /// too, until Tab gave the focus back. keep_keyboard has the library take
    /// it back, and Search gives it to its field.
    #[test]
    fn shortcuts_reach_the_window_after_a_page_makes_its_cards_again() {
        use crate::{LibraryCard, LibraryRow};
        use slint::platform::Key;
        i_slint_backend_testing::init_no_event_loop();
        let window = PlayerWindow::new().unwrap();
        let resize = |width| {
            window
                .window()
                .set_size(slint::PhysicalSize::new(width, 800))
        };
        resize(1280);
        let press = |keys: &[SharedString]| press_keys(&window, keys);
        let ctrl_q = [SharedString::from(Key::Control), "q".into()];
        let quits = Rc::new(Cell::new(0));
        let counted = quits.clone();
        window
            .global::<Actions>()
            .on_key(move |text, _, ctrl, _, _, _| {
                let quit = text == "q" && ctrl;
                if quit {
                    counted.set(counted.get() + 1);
                }
                quit
            });
        let library = window.global::<Library>();
        let opened = Rc::new(Cell::new(0));
        let counted = opened.clone();
        library.on_open(move |_, _| counted.set(counted.get() + 1));
        let row = || {
            let card = LibraryCard {
                id: "show".into(),
                kind: "series".into(),
                title: "A Show".into(),
                aspect: 2.0 / 3.0,
                progress: -1.0,
                ..Default::default()
            };
            LibraryRow {
                id: "row".into(),
                title: "Shows".into(),
                cards: ModelRc::new(VecModel::from(vec![card; 3])),
            }
        };
        let rows = Rc::new(VecModel::from(vec![row()]));
        library.set_rows(rows.clone().into());
        // As the controller does, a grid's lines are made again for
        // another number of columns.
        let lines = rows.clone();
        library.on_columns(move |_| lines.set_vec(vec![row()]));
        library.set_shown(true);
        window.show().unwrap();

        for how in ["home", "grid", "grid resized", "search"] {
            resize(1280);
            let page = how.split(' ').next().unwrap();
            library.set_page(page.into());
            library.set_loads(library.get_loads() + 1);
            // Tab until a card has the focus, which Enter on it shows.
            let before = opened.get();
            for _ in 0..8 {
                press(&[Key::Tab.into()]);
                press(&[Key::Return.into()]);
                if opened.get() > before {
                    break;
                }
            }
            assert_eq!(opened.get(), before + 1, "no card took the focus: {how}");
            if how == "grid resized" {
                resize(700);
            } else {
                rows.set_vec(vec![row()]);
                library.set_loads(library.get_loads() + 1);
            }
            let before = quits.get();
            press(&ctrl_q);
            assert_eq!(quits.get(), before + 1, "once made again: {how}");
            if how == "search" {
                library.set_search_text(SharedString::new());
                press(&["x".into()]);
                assert_eq!(library.get_search_text(), "x", "the field has the focus");
            }
        }

        // A dialog over the library keeps the keyboard through a load.
        library.set_page("home".into());
        window.set_dialog("open-url".into());
        window.set_dialog_text(SharedString::new());
        press(&["a".into()]);
        rows.set_vec(vec![row()]);
        library.set_loads(library.get_loads() + 1);
        press(&["b".into()]);
        assert_eq!(window.get_dialog_text(), "ab", "the dialog kept the focus");
    }

    /// Presses keys as a window's event hook has them reach Slint: each after
    /// keep_keyboard, as winit's are.
    fn press_keys(window: &PlayerWindow, keys: &[SharedString]) {
        use slint::platform::WindowEvent;
        for text in keys {
            keep_keyboard(window, true);
            let text = text.clone();
            window
                .window()
                .dispatch_event(WindowEvent::KeyPressed { text });
        }
        for text in keys.iter().rev() {
            keep_keyboard(window, true);
            let text = text.clone();
            window
                .window()
                .dispatch_event(WindowEvent::KeyReleased { text });
        }
    }

    fn press_key(window: &PlayerWindow, key: impl Into<SharedString>) {
        press_keys(window, &[key.into()]);
    }

    /// What reaches the window's own keys, each with " ctrl" if it was held.
    fn keys_reaching_the_window(window: &PlayerWindow) -> Rc<RefCell<Vec<String>>> {
        let keys: Rc<RefCell<Vec<String>>> = Rc::default();
        let seen = keys.clone();
        window
            .global::<Actions>()
            .on_key(move |text, _, ctrl, _, _, _| {
                // Tab goes on to move the focus, which is no shortcut.
                if text == "\t" {
                    return false;
                }
                seen.borrow_mut()
                    .push(format!("{text}{}", if ctrl { " ctrl" } else { "" }));
                true
            });
        keys
    }

    /// Ctrl+Q, which has to reach the window's keys wherever the focus is.
    fn press_ctrl_q(window: &PlayerWindow, keys: &Rc<RefCell<Vec<String>>>) -> bool {
        keys.borrow_mut().clear();
        press_keys(
            window,
            &[
                SharedString::from(slint::platform::Key::Control),
                "q".into(),
            ],
        );
        keys.borrow_mut().drain(..).any(|key| key == "q ctrl")
    }

    /// Lines of a library's page, a row or a grid's line of cards.
    fn library_rows(count: usize) -> ModelRc<crate::LibraryRow> {
        let card = crate::LibraryCard {
            id: "show".into(),
            kind: "series".into(),
            title: "A Show".into(),
            aspect: 2.0 / 3.0,
            progress: -1.0,
            ..Default::default()
        };
        let row = crate::LibraryRow {
            id: "row".into(),
            title: "Shows".into(),
            cards: ModelRc::new(VecModel::from(vec![card; 3])),
        };
        ModelRc::new(VecModel::from(vec![row; count]))
    }

    /// Lines of a page of the YouTube section.
    fn youtube_lines(count: usize) -> ModelRc<crate::YouTubeLine> {
        let card = crate::YouTubeCard {
            id: "aaaaaaaaaaa".into(),
            title: "A Video".into(),
            progress: -1.0,
            ..Default::default()
        };
        let line = crate::YouTubeLine {
            id: "videos".into(),
            kind: "videos".into(),
            cards: ModelRc::new(VecModel::from(vec![card; 3])),
            ..Default::default()
        };
        ModelRc::new(VecModel::from(vec![line; count]))
    }

    /// A page of the library or of the YouTube section, or what shows without
    /// one, to look for the keyboard on.
    fn show_page(window: &PlayerWindow, view: &str, page: &str) {
        let library = window.global::<Library>();
        let youtube = window.global::<YouTube>();
        let playback = window.global::<Playback>();
        youtube.set_shown(false);
        library.set_shown(false);
        playback.set_has_media(false);
        window.set_music_mode(false);
        match view {
            "library" => {
                library.set_rows(library_rows(40));
                library.set_page(page.into());
                library.set_loads(library.get_loads() + 1);
                library.set_shown(true);
            }
            "youtube" => {
                youtube.set_lines(youtube_lines(40));
                youtube.set_page(page.into());
                youtube.set_loads(youtube.get_loads() + 1);
                youtube.set_shown(true);
            }
            "player" => playback.set_has_media(true),
            "music" => {
                playback.set_has_media(true);
                window.set_music_mode(true);
            }
            _ => {}
        }
        // Each page's keys take the focus as they are made.
        i_slint_core::window::WindowInner::from_pub(window.window()).ensure_tree_instantiated();
    }

    fn window_for_keys() -> PlayerWindow {
        i_slint_backend_testing::init_no_event_loop();
        let window = PlayerWindow::new().unwrap();
        window
            .window()
            .set_size(slint::PhysicalSize::new(1280, 800));
        window.show().unwrap();
        window
    }

    /// Chooses Subtitles, then Bigger, from the application menu, as the mouse
    /// does: a submenu's item, which leaves the focus on an item of a closed
    /// menu.
    fn choose_from_a_submenu(window: &PlayerWindow) {
        use slint::platform::Key;
        let chosen = Rc::new(Cell::new(false));
        let seen = chosen.clone();
        window.global::<Actions>().on_mpv(move |args| {
            seen.set(args.iter().any(|arg| arg == "sub-scale"));
        });
        window.invoke_show_menu_at(10.0, 10.0);
        for _ in 0..5 {
            press_key(window, Key::DownArrow);
        }
        press_key(window, Key::RightArrow);
        // Past the separator, which the arrows skip.
        for _ in 0..10 {
            press_key(window, Key::DownArrow);
        }
        press_key(window, Key::Return);
        assert!(chosen.get(), "the menu's item wasn't chosen");
    }

    /// Every key but Tab reached nothing after a choice from a submenu, in the
    /// views' keys: Slint leaves the focus on an item of a closed menu.
    #[test]
    fn keys_work_after_a_choice_from_a_submenu() {
        let window = window_for_keys();
        let keys = keys_reaching_the_window(&window);
        for (view, page) in [
            ("library", "home"),
            ("library", "grid"),
            ("library", "local"),
            ("youtube", "feed"),
            ("youtube", "saved"),
            ("youtube", "channel"),
            ("welcome", ""),
            ("player", ""),
            ("music", ""),
        ] {
            show_page(&window, view, page);
            assert!(press_ctrl_q(&window, &keys), "before: {view} {page}");
            choose_from_a_submenu(&window);
            assert!(press_ctrl_q(&window, &keys), "after: {view} {page}");
        }
    }

    /// A field goes on being typed in after a choice from a submenu.
    #[test]
    fn a_field_keeps_the_typing_after_a_choice_from_a_submenu() {
        let window = window_for_keys();
        let keys = keys_reaching_the_window(&window);
        let library = window.global::<Library>();
        let youtube = window.global::<YouTube>();

        show_page(&window, "library", "search");
        press_key(&window, "a");
        choose_from_a_submenu(&window);
        press_key(&window, "q");
        assert_eq!(library.get_search_text(), "aq");

        show_page(&window, "youtube", "search");
        press_key(&window, "a");
        choose_from_a_submenu(&window);
        press_key(&window, "q");
        assert_eq!(youtube.get_search_text(), "aq");

        youtube.set_wants_field(true);
        show_page(&window, "youtube", "channels");
        press_key(&window, "a");
        choose_from_a_submenu(&window);
        press_key(&window, "q");
        assert_eq!(youtube.get_channel_text(), "aq");
        assert!(keys.borrow().is_empty(), "{:?}", keys.borrow());
    }

    /// A focused card scrolled out of a list: its row is destroyed, and every
    /// key went nowhere until Tab. The view takes the keyboard, and Tab goes
    /// on from there.
    #[test]
    fn keys_work_after_a_focused_card_is_scrolled_out_of_its_list() {
        use slint::platform::{Key, WindowEvent};
        let window = window_for_keys();
        let keys = keys_reaching_the_window(&window);
        let opened = Rc::new(Cell::new(0));
        let library = window.global::<Library>();
        let counted = opened.clone();
        library.on_open(move |_, _| counted.set(counted.get() + 1));
        let youtube = window.global::<YouTube>();
        let counted = opened.clone();
        youtube.on_play(move |_| counted.set(counted.get() + 1));
        let backs = Rc::new(Cell::new(0));
        let counted = backs.clone();
        library.on_back(move || counted.set(counted.get() + 1));
        library.set_can_go_back(true);
        let counted = backs.clone();
        youtube.on_back(move || counted.set(counted.get() + 1));
        let position = slint::LogicalPosition::new(600.0, 500.0);
        let tab_to_a_card = || {
            let before = opened.get();
            for _ in 0..24 {
                press_key(&window, Key::Tab);
                press_key(&window, Key::Return);
                if opened.get() > before {
                    return;
                }
            }
            panic!("no card took the focus");
        };
        for (view, page) in [
            ("library", "home"),
            ("library", "grid"),
            ("library", "search"),
            ("youtube", "feed"),
            ("youtube", "search"),
            ("youtube", "saved"),
            ("youtube", "channel"),
        ] {
            show_page(&window, view, page);
            tab_to_a_card();
            window
                .window()
                .dispatch_event(WindowEvent::PointerMoved { position });
            for _ in 0..40 {
                window
                    .window()
                    .dispatch_event(WindowEvent::PointerScrolled {
                        position,
                        delta_x: 0.0,
                        delta_y: -120.0,
                    });
            }
            assert!(press_ctrl_q(&window, &keys), "Ctrl+Q: {view} {page}");
            // The view has it, where Enter stops and Backspace goes back; on
            // Search, the field would take the Backspace.
            let before = (opened.get(), backs.get());
            press_key(&window, Key::Return);
            press_key(&window, Key::Backspace);
            assert_eq!(opened.get(), before.0, "the card kept it: {view} {page}");
            assert_eq!(
                backs.get(),
                before.1 + 1,
                "the view hasn't it: {view} {page}"
            );
            assert!(keys.borrow().is_empty(), "{:?}", keys.borrow());
            tab_to_a_card();
        }
    }

    /// A control scrolled off a page that keeps it, out of sight: Slint drops
    /// the focus, and the key that finds it there.
    #[test]
    fn keys_work_after_a_focused_control_is_clipped_out_of_its_page() {
        use slint::platform::{Key, WindowEvent};
        let window = window_for_keys();
        window
            .window()
            .set_size(slint::PhysicalSize::new(1280, 300));
        let keys = keys_reaching_the_window(&window);
        let library = window.global::<Library>();
        let plays = Rc::new(Cell::new(0));
        let counted = plays.clone();
        library.on_play(move |_, _| counted.set(counted.get() + 1));
        library.set_detail(crate::LibraryDetail {
            id: "film".into(),
            kind: "movie".into(),
            title: "A Film".into(),
            loaded: true,
            can_play: true,
            ..Default::default()
        });
        show_page(&window, "library", "detail");
        let before = plays.get();
        press_key(&window, Key::Tab);
        press_key(&window, Key::Return);
        assert_eq!(plays.get(), before + 1, "Play took no focus");
        let position = slint::LogicalPosition::new(600.0, 250.0);
        window
            .window()
            .dispatch_event(WindowEvent::PointerMoved { position });
        window
            .window()
            .dispatch_event(WindowEvent::PointerScrolled {
                position,
                delta_x: 0.0,
                delta_y: -2000.0,
            });
        // One key on its own: of Ctrl+Q, Slint would only drop the Ctrl.
        press_key(&window, "f");
        assert_eq!(*keys.borrow(), ["f"], "after scrolling");
    }

    /// Closing a dialog gave the keyboard to the window's keys, wherever it
    /// had been: on Search, "q" then quit Patina, and Backspace reset the
    /// player's speed. A field being typed in has it back, and otherwise the
    /// view.
    #[test]
    fn a_view_has_the_keyboard_back_when_a_dialog_closes() {
        use slint::platform::Key;
        let window = window_for_keys();
        let keys = keys_reaching_the_window(&window);
        let library = window.global::<Library>();
        let youtube = window.global::<YouTube>();
        let backs = Rc::new(Cell::new(0));
        let counted = backs.clone();
        library.on_back(move || counted.set(counted.get() + 1));
        library.set_can_go_back(true);
        let counted = backs.clone();
        youtube.on_back(move || counted.set(counted.get() + 1));
        let cover = |dialog: &str| {
            window.set_dialog(dialog.into());
            window.set_dialog_text(SharedString::new());
            i_slint_core::window::WindowInner::from_pub(window.window()).ensure_tree_instantiated();
        };
        // Each way out of a dialog, each ending in the same place.
        let close = |dialog: &str| match dialog {
            "jump-to" => {
                press_key(&window, "1");
                press_key(&window, Key::Return);
            }
            "clear-recent" => press_key(&window, Key::Return),
            _ => press_key(&window, Key::Escape),
        };
        for dialog in ["shortcuts", "about", "open-url", "jump-to", "clear-recent"] {
            // In a field: typing goes on in it.
            show_page(&window, "library", "search");
            library.set_search_text(SharedString::new());
            press_key(&window, "a");
            cover(dialog);
            close(dialog);
            assert_eq!(window.get_dialog(), "", "{dialog}");
            press_key(&window, "q");
            assert_eq!(library.get_search_text(), "aq", "library search: {dialog}");
            press_key(&window, Key::Backspace);
            assert_eq!(library.get_search_text(), "a", "library search: {dialog}");

            show_page(&window, "youtube", "search");
            youtube.set_search_text(SharedString::new());
            press_key(&window, "a");
            cover(dialog);
            close(dialog);
            press_key(&window, "q");
            assert_eq!(youtube.get_search_text(), "aq", "youtube search: {dialog}");
            press_key(&window, Key::Backspace);
            assert_eq!(youtube.get_search_text(), "a", "youtube search: {dialog}");

            youtube.set_channel_text(SharedString::new());
            youtube.set_wants_field(true);
            show_page(&window, "youtube", "channels");
            press_key(&window, "a");
            cover(dialog);
            close(dialog);
            press_key(&window, "q");
            assert_eq!(youtube.get_channel_text(), "aq", "channels: {dialog}");
            assert!(keys.borrow().is_empty(), "{dialog}: {:?}", keys.borrow());

            // Not in one: Backspace goes back and Enter stops at the view, and
            // what the view doesn't take goes on to the window.
            for (view, page) in [("library", "home"), ("youtube", "feed")] {
                show_page(&window, view, page);
                cover(dialog);
                close(dialog);
                keys.borrow_mut().clear();
                let before = backs.get();
                press_key(&window, Key::Backspace);
                assert_eq!(backs.get(), before + 1, "{view}: {dialog}");
                press_key(&window, Key::Return);
                assert!(keys.borrow().is_empty(), "{dialog}: {:?}", keys.borrow());
                press_key(&window, "f");
                assert_eq!(*keys.borrow(), ["f"], "{view}: {dialog}");
                keys.borrow_mut().clear();
            }

            // Tabbed out of the field: it hasn't it back, and "q" is the
            // window's again.
            show_page(&window, "library", "search");
            library.set_search_text(SharedString::new());
            press_key(&window, Key::Tab);
            cover(dialog);
            close(dialog);
            press_key(&window, "q");
            assert_eq!(library.get_search_text(), "", "tabbed out: {dialog}");
            assert_eq!(*keys.borrow(), ["q"], "tabbed out: {dialog}");
            keys.borrow_mut().clear();

            show_page(&window, "youtube", "search");
            youtube.set_search_text(SharedString::new());
            press_key(&window, Key::Tab);
            cover(dialog);
            close(dialog);
            press_key(&window, "q");
            assert_eq!(youtube.get_search_text(), "", "tabbed out: {dialog}");
            assert_eq!(*keys.borrow(), ["q"], "tabbed out: {dialog}");
            keys.borrow_mut().clear();
        }

        // Signing in, a step's field has it back.
        let signed = Rc::new(RefCell::new(String::new()));
        let seen = signed.clone();
        library.on_connect(move |text| *seen.borrow_mut() = text.to_string());
        library.set_sign_in(crate::LibrarySignIn {
            step: "address".into(),
            ..Default::default()
        });
        show_page(&window, "library", "sign-in");
        press_key(&window, "a");
        cover("shortcuts");
        press_key(&window, Key::Escape);
        press_key(&window, "b");
        press_key(&window, Key::Return);
        assert_eq!(*signed.borrow(), "ab");
    }

    /// A dialog keeps the keyboard while it is open: a choice from a submenu
    /// that opens one doesn't leave it dead, and nothing takes the keyboard
    /// from one through keep_keyboard.
    #[test]
    fn a_dialog_opened_from_a_submenu_has_the_keyboard() {
        use slint::platform::Key;
        let window = window_for_keys();
        let weak = window.as_weak();
        window.global::<Actions>().on_patina(move |name| {
            if name == "open-url" {
                weak.unwrap().set_dialog("open-url".into());
            }
        });
        show_page(&window, "youtube", "feed");
        window.invoke_show_menu_at(10.0, 10.0);
        press_key(&window, Key::DownArrow);
        press_key(&window, Key::RightArrow);
        // File: Open, then Open URL.
        press_key(&window, Key::DownArrow);
        press_key(&window, Key::DownArrow);
        press_key(&window, Key::Return);
        assert_eq!(window.get_dialog(), "open-url");
        press_key(&window, "a");
        keep_keyboard(&window, true);
        press_key(&window, "b");
        assert_eq!(window.get_dialog_text(), "ab");
        press_key(&window, Key::Escape);
        assert_eq!(window.get_dialog(), "");
    }

    /// Opens a dialog with a field, and types into it.
    fn open_a_prompt(window: &PlayerWindow) {
        window.set_dialog("open-url".into());
        window.set_dialog_text(SharedString::new());
        i_slint_core::window::WindowInner::from_pub(window.window()).ensure_tree_instantiated();
        press_key(window, "a");
    }

    /// What arrives under a dialog leaves it the keyboard: the dialog goes on
    /// being typed in and Escape still closes it, with nothing reaching the
    /// window's shortcuts behind it. Once it has closed, what arrived has the
    /// keyboard: a field, or else the view, where Backspace goes back.
    #[test]
    fn a_dialog_keeps_the_keyboard_from_what_arrives_under_it() {
        use slint::platform::Key;
        let window = window_for_keys();
        let keys = keys_reaching_the_window(&window);
        let library = window.global::<Library>();
        let youtube = window.global::<YouTube>();
        let backs = Rc::new(Cell::new(0));
        let counted = backs.clone();
        library.on_back(move || counted.set(counted.get() + 1));
        library.set_can_go_back(true);
        let counted = backs.clone();
        youtube.on_back(move || counted.set(counted.get() + 1));
        let sent = Rc::new(RefCell::new(String::new()));
        let seen = sent.clone();
        library.on_connect(move |text| *seen.borrow_mut() = text.to_string());
        let seen = sent.clone();
        library.on_pick_user(move |text| *seen.borrow_mut() = text.to_string());
        let seen = sent.clone();
        library.on_sign_in_with_password(move |_, text| *seen.borrow_mut() = text.to_string());
        let step = |step: &str, working: bool, message: &str| {
            library.set_sign_in(crate::LibrarySignIn {
                step: step.into(),
                working,
                message: message.into(),
                ..Default::default()
            });
        };
        // Types on in the dialog with `arrived` come, and closes it.
        let check_and_close = |what: &str| {
            i_slint_core::window::WindowInner::from_pub(window.window()).ensure_tree_instantiated();
            press_key(&window, "b");
            assert_eq!(window.get_dialog_text(), "ab", "{what}");
            press_key(&window, "q");
            assert_eq!(window.get_dialog_text(), "abq", "{what}");
            press_key(&window, Key::Escape);
            assert_eq!(window.get_dialog(), "", "{what}");
            assert!(keys.borrow().is_empty(), "{what}: {:?}", keys.borrow());
        };
        // Pages of a view, none with a field.
        for (view, page) in [
            ("library", "home"),
            ("library", "grid"),
            ("library", "detail"),
            ("library", "local"),
            ("youtube", "feed"),
            ("youtube", "saved"),
            ("youtube", "channel"),
        ] {
            let what = format!("{view} {page}");
            show_page(
                &window,
                view,
                if view == "library" { "search" } else { "feed" },
            );
            open_a_prompt(&window);
            if view == "library" {
                library.set_page(page.into());
            } else {
                youtube.set_page(page.into());
            }
            check_and_close(&what);
            let before = backs.get();
            press_key(&window, Key::Backspace);
            assert_eq!(backs.get(), before + 1, "the view hasn't it: {what}");
        }
        // Each view as it comes, under the dialog.
        for (view, page) in [("library", "home"), ("youtube", "feed")] {
            let what = format!("{view} made");
            show_page(&window, "welcome", "");
            open_a_prompt(&window);
            show_page(&window, view, page);
            check_and_close(&what);
            let before = backs.get();
            press_key(&window, Key::Backspace);
            assert_eq!(backs.get(), before + 1, "the view hasn't it: {what}");
        }
        // An error in the library's place.
        show_page(&window, "library", "home");
        open_a_prompt(&window);
        library.set_error("It went wrong".into());
        check_and_close("library failed");
        library.set_error(SharedString::new());

        // A field, which has the keyboard once the dialog closes.
        show_page(&window, "library", "home");
        library.set_search_text(SharedString::new());
        open_a_prompt(&window);
        library.set_page("search".into());
        check_and_close("library search");
        press_key(&window, "q");
        assert_eq!(library.get_search_text(), "q", "library search");

        show_page(&window, "youtube", "feed");
        youtube.set_search_text(SharedString::new());
        open_a_prompt(&window);
        youtube.set_page("search".into());
        check_and_close("youtube search");
        press_key(&window, "q");
        assert_eq!(youtube.get_search_text(), "q", "youtube search");

        show_page(&window, "youtube", "feed");
        youtube.set_channel_text(SharedString::new());
        youtube.set_wants_field(true);
        open_a_prompt(&window);
        youtube.set_page("channels".into());
        check_and_close("channels");
        press_key(&window, "q");
        assert_eq!(youtube.get_channel_text(), "q", "channels");

        // Signing in, each step with a field as it comes.
        for name in ["address", "users", "password"] {
            step("address", true, "");
            show_page(&window, "library", "sign-in");
            open_a_prompt(&window);
            step(name, false, "");
            check_and_close(name);
            press_key(&window, "q");
            press_key(&window, Key::Return);
            assert_eq!(*sent.borrow(), "q", "{name}");
            sent.borrow_mut().clear();
        }
        // And what a step does as an answer comes: signing in ends, or a
        // password is refused.
        for (name, message) in [("address", ""), ("password", ""), ("password", "No")] {
            let what = format!("{name} answered: {message}");
            step(name, false, "");
            show_page(&window, "library", "sign-in");
            press_key(&window, Key::Tab);
            step(name, true, "");
            open_a_prompt(&window);
            step(name, false, message);
            check_and_close(&what);
            press_key(&window, "q");
            press_key(&window, Key::Return);
            assert_eq!(*sent.borrow(), "q", "{what}");
            sent.borrow_mut().clear();
        }
    }

    /// A field disabled while its page works ignores keys, which then went on
    /// to the window's shortcuts, "q" quitting Patina, and a dialog closed over
    /// it left it without the keyboard for good. What is typed stops at the view
    /// until the field can take it, and Backspace doesn't go back.
    #[test]
    fn a_field_has_the_keyboard_back_after_waiting() {
        use slint::platform::Key;
        let window = window_for_keys();
        let keys = keys_reaching_the_window(&window);
        let library = window.global::<Library>();
        let youtube = window.global::<YouTube>();
        let backs = Rc::new(Cell::new(0));
        let counted = backs.clone();
        youtube.on_back(move || counted.set(counted.get() + 1));
        let weak = window.as_weak();
        youtube.on_add_channel(move |_| weak.unwrap().global::<YouTube>().set_adding(true));
        let settle = || {
            i_slint_core::window::WindowInner::from_pub(window.window()).ensure_tree_instantiated()
        };
        let start = || {
            youtube.set_adding(false);
            youtube.set_channel_text(SharedString::new());
            youtube.set_wants_field(true);
            // The page is made again, and takes the keyboard as it comes.
            show_page(&window, "welcome", "");
            show_page(&window, "youtube", "channels");
            press_key(&window, "a");
            press_key(&window, Key::Return);
            assert!(youtube.get_adding(), "the channel wasn't added");
        };
        let typed_stops = |what: &str| {
            press_key(&window, "q");
            press_key(&window, Key::Backspace);
            press_key(&window, " ");
            press_key(&window, Key::Return);
            assert!(keys.borrow().is_empty(), "{what}: {:?}", keys.borrow());
            assert_eq!(backs.get(), 0, "{what}");
            assert_eq!(youtube.get_channel_text(), "a", "{what}");
        };

        // The keys while adding, and the field's afterwards. A modifier's still
        // reach the window.
        start();
        typed_stops("adding");
        assert!(press_ctrl_q(&window, &keys), "Ctrl+Q while adding");
        youtube.set_adding(false);
        settle();
        press_key(&window, "b");
        assert_eq!(youtube.get_channel_text(), "ab", "added");

        // A dialog closed while adding, then the add ending.
        start();
        for dialog in ["shortcuts", "open-url"] {
            window.set_dialog(dialog.into());
            settle();
            press_key(&window, Key::Escape);
            assert_eq!(window.get_dialog(), "", "{dialog}");
            typed_stops("a dialog closed while adding");
        }
        youtube.set_adding(false);
        settle();
        press_key(&window, "b");
        assert_eq!(youtube.get_channel_text(), "ab", "closed while adding");

        // The add ending under a dialog, which keeps the keyboard until it closes.
        start();
        open_a_prompt(&window);
        youtube.set_adding(false);
        settle();
        press_key(&window, "b");
        assert_eq!(window.get_dialog_text(), "ab", "ended under a dialog");
        press_key(&window, Key::Escape);
        press_key(&window, "b");
        assert_eq!(youtube.get_channel_text(), "ab", "ended under a dialog");
        assert!(keys.borrow().is_empty(), "{:?}", keys.borrow());

        // Moved on from the field: the window's keys work, and the add's end
        // doesn't bring the keyboard back.
        start();
        press_key(&window, Key::Tab);
        press_key(&window, "q");
        assert_eq!(*keys.borrow(), ["q"], "tabbed out");
        keys.borrow_mut().clear();
        youtube.set_adding(false);
        settle();
        press_key(&window, "q");
        assert_eq!(*keys.borrow(), ["q"], "tabbed out, added");
        keys.borrow_mut().clear();
        assert_eq!(youtube.get_channel_text(), "a", "tabbed out, added");

        // Another page: an add going on behind it takes nothing from it.
        start();
        youtube.set_page("feed".into());
        settle();
        press_key(&window, "q");
        assert_eq!(*keys.borrow(), ["q"], "another page");
        keys.borrow_mut().clear();
        // Nor from another page's field, which passes on what it doesn't take.
        youtube.set_page("search".into());
        settle();
        press_key(&window, Key::F1);
        assert_eq!(keys.borrow().len(), 1, "another page's field");
        keys.borrow_mut().clear();
        youtube.set_adding(false);

        // Signing in: a dialog closed while a step's field waits, then the
        // answer leaving the step as it was.
        let sent = Rc::new(RefCell::new(String::new()));
        let seen = sent.clone();
        library.on_connect(move |text| *seen.borrow_mut() = text.to_string());
        let seen = sent.clone();
        library.on_pick_user(move |text| *seen.borrow_mut() = text.to_string());
        let seen = sent.clone();
        library.on_sign_in_with_password(move |_, text| *seen.borrow_mut() = text.to_string());
        let cancelled = Rc::new(Cell::new(false));
        let seen = cancelled.clone();
        library.on_cancel_sign_in(move || seen.set(true));
        for name in ["address", "users", "password"] {
            let step = |working: bool| {
                library.set_sign_in(crate::LibrarySignIn {
                    step: name.into(),
                    working,
                    ..Default::default()
                });
            };
            step(false);
            show_page(&window, "library", "sign-in");
            press_key(&window, "a");
            step(true);
            settle();
            window.set_dialog("shortcuts".into());
            settle();
            press_key(&window, Key::Escape);
            assert_eq!(window.get_dialog(), "", "{name}");
            // The keyboard stays off the buttons, or Enter would cancel.
            press_key(&window, "q");
            press_key(&window, Key::Return);
            assert!(!cancelled.get(), "{name}");
            assert!(keys.borrow().is_empty(), "{name}: {:?}", keys.borrow());
            step(false);
            settle();
            press_key(&window, "b");
            press_key(&window, Key::Return);
            assert_eq!(*sent.borrow(), "ab", "{name}");
            sent.borrow_mut().clear();
        }
    }

    /// A chip that loads the page again keeps the keyboard. The view used to
    /// take it from what had it on each load, so Space only pressed the chip
    /// once and the next one went elsewhere.
    #[test]
    fn a_chip_keeps_the_keyboard_as_it_loads_the_page_again() {
        use slint::platform::Key;
        let window = window_for_keys();
        let keys = keys_reaching_the_window(&window);
        let library = window.global::<Library>();
        let youtube = window.global::<YouTube>();

        let toggles = Rc::new(Cell::new(0));
        let counted = toggles.clone();
        let weak = window.as_weak();
        // Fewer lines, or more made again.
        youtube.on_set_shorts(move |show| {
            let window = weak.unwrap();
            let youtube = window.global::<YouTube>();
            counted.set(counted.get() + 1);
            youtube.set_show_shorts(show);
            youtube.set_lines(youtube_lines(if show { 40 } else { 2 }));
            youtube.set_loads(youtube.get_loads() + 1);
        });
        let counted = toggles.clone();
        let weak = window.as_weak();
        library.on_filter(move |_, favorites| {
            let window = weak.unwrap();
            let library = window.global::<Library>();
            counted.set(counted.get() + 1);
            library.set_favorites_only(favorites);
            library.set_rows(library_rows(if favorites { 2 } else { 40 }));
            library.set_loads(library.get_loads() + 1);
        });
        // Over a page's cards and the bar, Space may press other things; they
        // do nothing here.
        let tab_to_the_chip = |what: &str| {
            let before = toggles.get();
            for _ in 0..24 {
                press_key(&window, Key::Tab);
                press_key(&window, " ");
                if toggles.get() > before {
                    return;
                }
            }
            panic!("no chip took the focus: {what}");
        };
        for (view, page) in [
            ("youtube", "search"),
            ("youtube", "feed"),
            ("library", "grid"),
        ] {
            let what = format!("{view} {page}");
            show_page(&window, view, page);
            tab_to_the_chip(&what);
            for press in 1..=3 {
                let before = toggles.get();
                press_key(&window, " ");
                assert_eq!(toggles.get(), before + 1, "press {press}: {what}");
            }
            assert!(keys.borrow().is_empty(), "{what}: {:?}", keys.borrow());
            // Search's field wasn't given the Space.
            assert_eq!(youtube.get_search_text(), "", "{what}");
        }
    }

    /// Escape closes Keyboard Shortcuts and About, as it does the prompt and
    /// confirm dialogs, in each of the window's views.
    #[test]
    fn escape_closes_the_shortcuts_and_about_dialogs() {
        use slint::platform::Key;
        let window = window_for_keys();
        keys_reaching_the_window(&window);
        for (view, page) in [
            ("welcome", ""),
            ("player", ""),
            ("library", "home"),
            ("youtube", "feed"),
        ] {
            show_page(&window, view, page);
            for dialog in ["shortcuts", "about"] {
                window.set_dialog(dialog.into());
                press_key(&window, Key::Tab);
                press_key(&window, Key::Escape);
                assert_eq!(window.get_dialog(), "", "{dialog} over {view}");
            }
        }
    }

    /// The playlist and the entry playing after `steps`, as mpv makes
    /// them: `insert-at` past the end appends.
    fn played(
        mut playlist: Vec<String>,
        mut current: Option<usize>,
        sources: &[&str],
        steps: &[(usize, &str, Option<usize>)],
    ) -> (Vec<String>, Option<usize>) {
        for &(index, mode, place) in steps {
            let source = sources[index].to_owned();
            match (mode, place) {
                ("replace", _) => {
                    playlist = vec![source];
                    current = Some(0);
                }
                ("insert-at", Some(at)) if at <= playlist.len() => {
                    playlist.insert(at, source);
                    current = current.map(|current| current + usize::from(at <= current));
                }
                ("append", _) => playlist.push(source),
                _ => {
                    playlist.push(source);
                    current = current.or(Some(playlist.len() - 1));
                }
            }
        }
        (playlist, current)
    }

    #[test]
    fn sources_go_where_they_play_in_the_playlist() {
        let songs = ["1", "2", "3", "4", "5"];
        let all: Vec<String> = songs.iter().map(|song| song.to_string()).collect();
        let before = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        // Played from the third song: all of them, the third playing,
        // and it the first mpv is given.
        let steps = load_steps(5, Placement::InOrder(2));
        assert_eq!(steps[0], (2, "replace", None));
        assert_eq!(
            played(before.clone(), Some(1), &songs, &steps),
            (all.clone(), Some(2))
        );
        assert_eq!(
            played(
                before.clone(),
                Some(1),
                &songs,
                &load_steps(5, Placement::Replace)
            ),
            (all.clone(), Some(0))
        );
        assert_eq!(
            played(
                before.clone(),
                Some(1),
                &songs,
                &load_steps(5, Placement::InOrder(4))
            ),
            (all.clone(), Some(4))
        );
        // Past the end, the last.
        assert_eq!(
            load_steps(5, Placement::InOrder(9))[0],
            (4, "replace", None)
        );
        assert_eq!(load_steps(0, Placement::Replace), []);
        // To be shuffled: in their order after the playlist is cleared,
        // none of them playing until mpv has shuffled them.
        assert_eq!(
            played(
                Vec::new(),
                None,
                &songs,
                &load_steps(5, Placement::Shuffled)
            ),
            (all.clone(), None)
        );
        // Added to the queue: at the end, after what is there.
        let (playlist, current) = played(
            before.clone(),
            Some(1),
            &songs,
            &load_steps(5, Placement::Append),
        );
        assert_eq!(playlist[..3], before[..]);
        assert_eq!(playlist[3..], all[..]);
        assert_eq!(current, Some(1));
        // Play Next: right after the entry playing, in their order.
        let (playlist, current) = played(before.clone(), Some(1), &songs[..2], &queue_steps(2, 1));
        assert_eq!(playlist, ["a", "b", "1", "2", "c"]);
        assert_eq!(current, Some(1));
        let (playlist, _) = played(before.clone(), Some(2), &songs[..2], &queue_steps(2, 2));
        assert_eq!(playlist, ["a", "b", "c", "1", "2"]);
    }

    #[test]
    fn a_window_with_nothing_to_play_opens_what_is_queued() {
        assert_eq!(queueing(true, true, None), Queueing::Open);
        assert_eq!(queueing(true, false, None), Queueing::Open);
        // A playlist that ended keeps no entry playing, but mpv may still
        // say where it was.
        assert_eq!(queueing(true, true, Some(3)), Queueing::Open);
        assert_eq!(queueing(false, true, Some(3)), Queueing::After(3));
        assert_eq!(queueing(false, false, Some(3)), Queueing::End);
        // Opening, before mpv says what plays: after what is on its way.
        assert_eq!(queueing(false, true, None), Queueing::End);
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
