//! One player window: its Slint component, its mpv core, and the glue.

use crate::app::App;
use crate::input::{Binding, KeyCombo};
use crate::mpv::{EndFileReason, Event, Mpv};
use crate::player::state::{PlaybackState, Track, TrackKind, file_name};
use crate::player::{self, Player, Prop};
use crate::settings::ResizeWindow;
use crate::thumbnails;
use crate::util::{self, format_delay, format_speed, format_time};
use crate::{
    Actions, ChapterItem, DeviceItem, Media, Osd, Platform, Playback, PlayerWindow, PlaylistItem,
    Prefs, RecentItem, ShortcutItem, TrackItem,
};
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Reply userdata for `show-text`, whose expanded text becomes an OSD message.
const REPLY_SHOW_TEXT: u64 = 10;
const REPLY_SCREENSHOT: u64 = 11;

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

pub struct PlayerController {
    app: Weak<App>,
    window: PlayerWindow,
    player: RefCell<Option<Player>>,
    mpv: Arc<Mpv>,
    state: RefCell<PlaybackState>,
    ready: Cell<bool>,
    pending_open: RefCell<Vec<(Vec<String>, bool)>>,
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
    /// The property a user command targeted, for an OSD message even when
    /// the value did not change (volume already at its maximum, say).
    osd_fallback: Cell<Option<Prop>>,
    osd_fallback_timer: Timer,
    /// A size we asked for, re-requested if a stale configure from the
    /// Wayland compositor undoes it: (size, deadline, attempts left).
    wanted_size: Cell<Option<(slint::LogicalSize, Instant, u8)>>,
    /// Seek-bar thumbnails for the current file, filled in as they arrive.
    thumbnails: RefCell<Vec<Option<slint::Image>>>,
    thumbnail_job: RefCell<Option<thumbnails::Job>>,
    thumbnail_path: RefCell<String>,
    this: Weak<PlayerController>,
}

impl PlayerController {
    pub fn new(app: &Rc<App>) -> Result<Rc<Self>, String> {
        let window = PlayerWindow::new().map_err(|error| error.to_string())?;
        let (player, events) = Player::start(&app.core_options())?;
        let mpv = player.mpv().clone();

        let controller = Rc::new_cyclic(|this| Self {
            this: this.clone(),
            osd_fallback: Cell::new(None),
            osd_fallback_timer: Timer::default(),
            wanted_size: Cell::new(None),
            thumbnails: RefCell::new(Vec::new()),
            thumbnail_job: RefCell::new(None),
            thumbnail_path: RefCell::new(String::new()),
            app: Rc::downgrade(app),
            window,
            player: RefCell::new(Some(player)),
            mpv,
            state: RefCell::new(PlaybackState::default()),
            ready: Cell::new(false),
            pending_open: RefCell::new(Vec::new()),
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
        });

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
        self.state.borrow().idle && self.pending_open.borrow().is_empty()
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
        platform.set_version(env!("CARGO_PKG_VERSION").into());

        let ui = self.window.global::<Prefs>();
        ui.set_show_remaining_time(prefs.show_remaining_time);
        ui.set_show_chapter_marks(prefs.show_chapter_marks);
        ui.set_osc_timeout((prefs.osc_timeout.max(0.5) * 1000.0) as i64);
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
        let ready = Rc::downgrade(self);
        let frame = self.window.as_weak();
        let error = self.window.as_weak();
        crate::video::attach(
            &self.window,
            self.mpv.clone(),
            crate::video::Hooks {
                set_frame: Box::new(move |image| {
                    if let Some(window) = frame.upgrade() {
                        window.global::<Playback>().set_video_frame(image);
                    }
                }),
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
            },
        )
        .map_err(|error| format!("could not attach the video renderer: {error:?}"))
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
            player.command(&["loadfile", source, mode]);
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
                    }
                }
                Event::StartFile { .. } => {
                    self.track.set(self.track.get() + 1);
                    self.reset_thumbnails();
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
                }
                Event::EndFile { reason, error, .. } => {
                    self.loading.set(false);
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

        if any(&[Prop::Idle]) {
            playback.set_has_media(!state.idle);
            if state.idle {
                self.window.set_sidebar_open(false);
            }
        }
        if any(&[Prop::Pause]) {
            playback.set_paused(state.paused);
        }
        if any(&[Prop::Pause, Prop::Idle])
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

        if any(&[Prop::Playlist, Prop::PlaylistPos]) {
            let items: Vec<PlaylistItem> = state
                .playlist
                .iter()
                .map(|entry| PlaylistItem {
                    title: entry.label().into(),
                    detail: util::parent_directory(&entry.filename).into(),
                    duration: SharedString::new(),
                    current: entry.current,
                })
                .collect();
            let count = items.len();
            media.set_playlist(ModelRc::new(VecModel::from(items)));
            media.set_playlist_pos(state.playlist_pos as i32);
            media.set_playlist_summary(match count {
                0 => "Empty".into(),
                1 => "1 item".into(),
                n => format!("{n} items").into(),
            });
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
        ]) && let Some(app) = self.app.upgrade()
            && app.is_active(self)
        {
            app.publish_now_playing(self.now_playing(&state));
        }

        // mpv window properties, driven by key bindings such as "cycle fullscreen".
        if any(&[Prop::Fullscreen]) && self.window.window().is_fullscreen() != state.fullscreen {
            self.window.window().set_fullscreen(state.fullscreen);
        }
        if any(&[Prop::Ontop]) && self.window.get_float_on_top() != state.ontop {
            self.window.set_float_on_top(state.ontop);
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
            if first && prefs.fullscreen_when_open && !self.window.window().is_fullscreen() {
                self.player().set("fullscreen", true);
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
            this.player()
                .command(&["playlist-move", &from.to_string(), &to.to_string()]);
        });
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
    }

    fn wire_window_events(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.window.window().on_winit_window_event(move |_, event| {
            let Some(this) = weak.upgrade() else {
                return EventResult::Propagate;
            };
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
        if !media.is_empty() {
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
        for (steps, action) in [
            (steps_y, &prefs.vertical_scroll),
            (-steps_x, &prefs.horizontal_scroll),
        ] {
            if steps == 0.0 {
                continue;
            }
            match action.as_str() {
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
            "close-window" => self.close(),
            "quit" => app.quit(),
            "minimize" => self.window.window().set_minimized(true),
            "toggle-fullscreen" => {
                let fullscreen = !self.window.window().is_fullscreen();
                self.player().set("fullscreen", fullscreen);
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
            "open-website" => crate::platform::open_url("https://github.com/jzbz/patina"),
            "save-playlist" => app.save_playlist_dialog(self),
            "clear-playlist" => self.player().command(&["playlist-clear"]),
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
        self.wanted_size
            .set(Some((size, Instant::now() + Duration::from_secs(2), 3)));
        self.window.window().set_size(size);
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
            if let Some(this) = this.upgrade() {
                log::debug!("re-requesting window size {size:?}");
                this.window.window().set_size(size);
            }
        });
    }

    fn scale_window(&self, factor: f32) {
        let window = self.window.window();
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
        // Streams and music are known by their title; local videos by name.
        let titled = util::is_url(&state.path) || state.is_audio_only();
        let title = if titled && !state.title.is_empty() {
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
            artist: meta(&["artist", "album_artist", "performer"]),
            album: meta(&["album"]),
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
        // context while the GL context is still current.
        let _ = self.window.hide();
        if let Some(mut player) = self.player.borrow_mut().take() {
            player.shutdown();
        }
        self.app().window_closed(self);
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
