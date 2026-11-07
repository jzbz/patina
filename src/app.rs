//! Application state shared by all windows: preferences, history, key
//! bindings, and the list of open player windows.

use crate::input::Bindings;
use crate::instance::Request;
use crate::media_controls::{self, MediaControls, NowPlaying};
use crate::player::CoreOptions;
use crate::player::state::{TrackKind, file_name};
use crate::settings::{self, History, HistoryEntry, Paths, Preferences};
use crate::window::PlayerController;
use slint::ComponentHandle;
use slint::winit_030::WinitWindowAccessor;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

thread_local! {
    /// The app, for callbacks that arrive from other threads through
    /// `slint::invoke_from_event_loop` and can only carry `Send` data.
    static CURRENT: RefCell<Weak<App>> = const { RefCell::new(Weak::new()) };
}

/// Opens this close together share a window, as when a file manager starts
/// Patina once for each selected file; IINA batches Finder's opens the same
/// way.
const BATCH_WITHIN: Duration = Duration::from_millis(200);

/// mpv's audio outputs on Linux without PipeWire's own output, nor JACK,
/// which is usually PipeWire's JACK and fails the same way.
const AUDIO_OUTPUTS_WITHOUT_PIPEWIRE: &str = "pulse,alsa";

pub struct App {
    paths: Paths,
    prefs: RefCell<Rc<Preferences>>,
    history: RefCell<History>,
    bindings: RefCell<Rc<Bindings>>,
    windows: RefCell<Vec<Rc<PlayerController>>>,
    verbose: bool,
    /// mpv options given on the command line, applied after the preferences'.
    cli_mpv_options: Vec<(String, String)>,
    inhibitor: crate::inhibit::Inhibitor,
    media: RefCell<Option<MediaControls>>,
    media_started: std::cell::Cell<bool>,
    /// What the system media controls show now.
    now_playing: RefCell<NowPlaying>,
    /// The window focused most recently.
    active: RefCell<Weak<PlayerController>>,
    /// Counts the times a window was focused (`PlayerController::set_focus_serial`).
    focus_serial: Cell<u64>,
    /// The window the system's media controls show and media keys act on
    /// (`retarget_media`).
    media_target: RefCell<Weak<PlayerController>>,
    preferences: RefCell<Option<Rc<crate::preferences::PreferencesController>>>,
    history_window: RefCell<Option<Rc<crate::history::HistoryController>>>,
    downloads: RefCell<Option<Rc<crate::downloads::Downloads>>>,
    inspector: RefCell<Option<Rc<crate::inspector::InspectorController>>>,
    media_info: RefCell<Option<Rc<crate::media_info::MediaInfoController>>>,
    /// Subtitle choices made from the playlist menu, by file.
    subtitle_choices: crate::subtitle_match::Choices,
    /// What patina:// links opened, and what their playlists name, which
    /// the load hook allows only streams from.
    link_sources: crate::scheme::LinkSources,
    clipboard: RefCell<Option<arboard::Clipboard>>,
    prober: RefCell<Option<crate::probe::Prober>>,
    /// Durations and tags of local files; None for files that can't be read.
    probes: RefCell<std::collections::HashMap<String, Option<crate::probe::Probe>>>,
    probing: RefCell<std::collections::HashSet<String>>,
    /// Answers later launches while this is the Patina that runs.
    instance: RefCell<Option<crate::instance::Server>>,
    /// Set once the launch's own sources are open; requests that come
    /// sooner wait in `early`.
    started: Cell<bool>,
    /// With when each came.
    early: RefCell<Vec<(Instant, Request)>>,
    /// When this Patina started, which is when its own request came.
    started_at: Instant,
    quitting: Cell<bool>,
    /// The window the last plain open went to, and when.
    last_open: RefCell<Option<(Instant, Weak<PlayerController>)>>,
    /// Requests taken from other launches and the system while starting,
    /// when a failure to draw makes Patina start again in software; None
    /// once the first window has shown.
    accepted: RefCell<Option<Vec<Request>>>,
    /// The Jellyfin servers signed in, which every window's library shows.
    jellyfin: Rc<crate::jellyfin::session::Session>,
    /// The YouTube section's lists and feed, which every window shows.
    youtube: Rc<crate::youtube::session::Session>,
    /// The yt-dlp choice handed on last (`ytdl_choice_changed`), by
    /// `ytdl_versions::Versions::changes`.
    ytdl_handed_on: Cell<u64>,
    /// Takes the toast shown last down (`explain`).
    #[cfg(all(unix, not(target_os = "macos")))]
    toast_timer: slint::Timer,
    /// What takes it down, which a toast in another window does first.
    #[cfg(all(unix, not(target_os = "macos")))]
    toast_hide: RefCell<Option<Rc<dyn Fn()>>>,
}

impl App {
    pub fn new(paths: Paths, verbose: bool, cli_mpv_options: Vec<(String, String)>) -> Rc<Self> {
        let prefs = settings::load_preferences(&paths.settings_file());
        let history: History = settings::load_json(&paths.history_file());
        let conf = binding_conf(&paths, &prefs.key_bindings);
        let jellyfin =
            crate::jellyfin::session::Session::new(&paths.data, paths.jellyfin_images_dir());
        jellyfin.set_image_limit(prefs.jellyfin_image_cache_mb.saturating_mul(1 << 20));
        let youtube = crate::youtube::session::Session::new(&paths);
        crate::ytdl_versions::remember_in(paths.ytdl_versions_file());
        let app = Rc::new(Self {
            paths,
            prefs: RefCell::new(Rc::new(prefs)),
            history: RefCell::new(history),
            bindings: RefCell::new(Rc::new(Bindings::parse(&conf))),
            windows: RefCell::new(Vec::new()),
            verbose,
            cli_mpv_options,
            inhibitor: crate::inhibit::Inhibitor::new(),
            media: RefCell::new(None),
            media_started: std::cell::Cell::new(false),
            now_playing: RefCell::new(NowPlaying::default()),
            active: RefCell::new(Weak::new()),
            focus_serial: Cell::new(0),
            media_target: RefCell::new(Weak::new()),
            preferences: RefCell::new(None),
            history_window: RefCell::new(None),
            downloads: RefCell::new(None),
            inspector: RefCell::new(None),
            media_info: RefCell::new(None),
            subtitle_choices: Default::default(),
            link_sources: Default::default(),
            clipboard: RefCell::new(None),
            prober: RefCell::new(None),
            probes: RefCell::new(std::collections::HashMap::new()),
            probing: RefCell::new(std::collections::HashSet::new()),
            instance: RefCell::new(None),
            started: Cell::new(false),
            early: RefCell::new(Vec::new()),
            started_at: Instant::now(),
            quitting: Cell::new(false),
            last_open: RefCell::new(None),
            accepted: RefCell::new(Some(Vec::new())),
            jellyfin,
            youtube,
            ytdl_handed_on: Cell::new(0),
            #[cfg(all(unix, not(target_os = "macos")))]
            toast_timer: slint::Timer::default(),
            #[cfg(all(unix, not(target_os = "macos")))]
            toast_hide: RefCell::new(None),
        });
        CURRENT.with(|current| *current.borrow_mut() = Rc::downgrade(&app));
        // The section's History page shows the history's YouTube videos.
        app.youtube.set_history(&app.history.borrow().entries);
        let weak = Rc::downgrade(&app);
        app.youtube.on_clear_history(Box::new(move || {
            if let Some(app) = weak.upgrade() {
                app.forget_youtube_history();
            }
        }));
        // The saved sign-ins come back from the keyring while the first
        // window opens.
        app.jellyfin.start();
        app
    }

    /// Runs `f` with the app, on the UI thread.
    pub fn with_current(f: impl FnOnce(&Rc<App>)) {
        if let Some(app) = CURRENT.with(|current| current.borrow().upgrade()) {
            f(&app);
        }
    }

    pub fn prefs(&self) -> Rc<Preferences> {
        self.prefs.borrow().clone()
    }

    pub fn update_prefs(&self, update: impl FnOnce(&mut Preferences)) {
        let old = self.prefs();
        let mut prefs = (*old).clone();
        update(&mut prefs);
        if prefs == *old {
            return;
        }
        settings::save_json(&self.paths.settings_file(), &prefs);
        *self.prefs.borrow_mut() = Rc::new(prefs);
        self.prefs_changed(&old);
    }

    /// The yt-dlp chosen between the package's and the user's installed one
    /// changed (`ytdl_versions`): the user's turned out newer once both had
    /// said their versions, or theirs was removed or replaced. mpv's hook,
    /// the YouTube section and the Downloads window move to it, as for a
    /// changed path preference. The hook is told through `script-opts`,
    /// unless the user's own mpv options set that, which would win at start
    /// and should go on winning. Once a change: `ytdl_current` may have
    /// handed it on before the call queued for it comes.
    pub fn ytdl_choice_changed(&self) {
        let changes = crate::ytdl_versions::global().changes();
        if changes == self.ytdl_handed_on.get() {
            return;
        }
        self.ytdl_handed_on.set(changes);
        let prefs = self.prefs();
        let own_script_opts = prefs
            .mpv_options
            .iter()
            .chain(&self.cli_mpv_options)
            .any(|(name, _)| name == "script-opts");
        let script_opts: Vec<(&'static str, String)> = if own_script_opts {
            Vec::new()
        } else {
            prefs
                .mpv_options()
                .into_iter()
                .filter(|(name, _)| *name == "script-opts")
                .collect()
        };
        let windows = self.windows.borrow().clone();
        for window in &windows {
            window.apply_mpv_options(&script_opts);
            window.yt_dlp_changed();
        }
        if let Some(downloads) = self.existing_downloads() {
            downloads.ytdl_changed();
        }
    }

    /// Makes the yt-dlp choice again, before a web address opens, and hands it
    /// on at once if it changed: otherwise a user's copy removed since it was
    /// chosen would be noticed only when the Downloads window or the section
    /// next looked, and mpv's hook would go on running what has gone. Only
    /// where a package carries yt-dlp, the one place the choice can change
    /// by itself.
    pub fn ytdl_current(&self) {
        if crate::platform::bundled("yt-dlp").is_none() {
            return;
        }
        crate::downloads::find_ytdl(&self.prefs(), &self.mpv_config_dir());
        self.ytdl_choice_changed();
    }

    /// Applies changed preferences to everything already running, as IINA
    /// does: interface settings at once, mpv options on every core.
    fn prefs_changed(&self, old: &Preferences) {
        let prefs = self.prefs();
        let before = old.mpv_options();
        let changed: Vec<(&'static str, String)> = prefs
            .mpv_options()
            .into_iter()
            .filter(|option| !before.contains(option))
            .collect();
        let windows = self.windows.borrow().clone();
        let playlist_tags = prefs.playlist_show_metadata != old.playlist_show_metadata
            || prefs.playlist_show_metadata_in_music_mode
                != old.playlist_show_metadata_in_music_mode;
        for window in &windows {
            window.apply_interface_preferences();
            window.apply_mpv_options(&changed);
            if playlist_tags {
                window.refresh_playlist();
            }
            if prefs.ytdl != old.ytdl || prefs.ytdl_path != old.ytdl_path {
                window.yt_dlp_changed();
            }
            // A change of the setting starts the automatic switch afresh.
            if prefs.auto_switch_to_music_mode != old.auto_switch_to_music_mode {
                window.reset_music_override();
            }
        }
        if prefs.media_keys != old.media_keys {
            if prefs.media_keys {
                self.media_started.set(false);
                if let Some(first) = windows.first() {
                    self.start_media_controls(first);
                }
            } else {
                self.media.borrow_mut().take();
            }
        }
        if prefs.key_bindings != old.key_bindings {
            self.reload_bindings();
        }
        self.playback_changed();
        if prefs.recent_limit != old.recent_limit {
            self.history_changed();
        }
        if prefs.jellyfin_image_cache_mb != old.jellyfin_image_cache_mb {
            self.jellyfin
                .set_image_limit(prefs.jellyfin_image_cache_mb.saturating_mul(1 << 20));
        }
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    pub fn jellyfin(&self) -> Rc<crate::jellyfin::session::Session> {
        self.jellyfin.clone()
    }

    pub fn youtube(&self) -> Rc<crate::youtube::session::Session> {
        self.youtube.clone()
    }

    /// A Jellyfin entry's title: as the library or the load hook knows it,
    /// or as the history kept it from the last time it played.
    pub fn entry_title(&self, entry: &str) -> Option<String> {
        let known = self
            .jellyfin
            .entry_title(entry)
            .or_else(|| self.youtube.title_of(entry));
        known.or_else(|| {
            self.history
                .borrow()
                .entries
                .iter()
                .find(|history| history.path == entry)
                .map(|history| history.title.clone())
                .filter(|title| !title.is_empty() && title != entry)
        })
    }

    /// Where a file is, as lists show it under its name: its folder, a
    /// stream's host, or a Jellyfin item's server by name.
    pub fn source_place(&self, path: &str) -> String {
        match crate::jellyfin::session::parse_entry(path) {
            Some((server, _)) => self
                .jellyfin
                .server(server)
                .map_or_else(|| "Jellyfin".to_owned(), |server| server.name),
            None => crate::util::parent_directory(path),
        }
    }

    pub fn bindings(&self) -> Rc<Bindings> {
        self.bindings.borrow().clone()
    }

    /// Reads the selected key binding set again.
    pub fn reload_bindings(&self) {
        let conf = binding_conf(&self.paths, &self.prefs().key_bindings);
        *self.bindings.borrow_mut() = Rc::new(Bindings::parse(&conf));
    }

    /// The user's key binding sets, by name.
    pub fn binding_sets(&self) -> Vec<String> {
        let mut sets: Vec<String> = std::fs::read_dir(self.paths.input_conf_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension()? != "conf" {
                    return None;
                }
                Some(path.file_stem()?.to_string_lossy().into_owned())
            })
            .collect();
        sets.sort_by(|a, b| crate::window::natural_cmp(a, b));
        sets
    }

    /// Saves `bindings` as the user set `name` and makes it the active set.
    pub fn save_binding_set(&self, name: &str, bindings: &Bindings) -> std::io::Result<()> {
        let dir = self.paths.input_conf_dir();
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(format!("{name}.conf")), bindings.to_conf())?;
        if self.prefs().key_bindings == name {
            self.reload_bindings();
        } else {
            self.update_prefs(|prefs| prefs.key_bindings = name.to_owned());
        }
        Ok(())
    }

    pub fn delete_binding_set(&self, name: &str) {
        let path = self.paths.input_conf_dir().join(format!("{name}.conf"));
        if let Err(error) = std::fs::remove_file(&path) {
            log::warn!("could not delete {}: {error}", path.display());
        }
        if self.prefs().key_bindings == name {
            self.update_prefs(|prefs| prefs.key_bindings.clear());
        }
    }

    pub fn core_options(&self) -> CoreOptions {
        let prefs = self.prefs();
        let mpv_config = self.paths.mpv_config_dir();
        let mut options: Vec<(String, String)> = prefs
            .mpv_options()
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect();
        options.push(("volume".into(), format!("{}", prefs.start_volume())));
        options.extend(prefs.mpv_options.iter().cloned());
        options.extend(self.cli_mpv_options.iter().cloned());
        let config_dir = (prefs.use_mpv_config && mpv_config.is_dir()).then_some(mpv_config);
        // Without a PipeWire server, keep mpv from trying its PipeWire
        // output before every file (see platform::pipewire_missing), unless
        // the user chose the output; first, so that a choice made in the
        // options wins anyway.
        let pipewire_missing = crate::platform::pipewire_missing();
        if pipewire_missing && !chooses_audio_output(&options, config_dir.as_deref()) {
            options.insert(0, ("ao".into(), AUDIO_OUTPUTS_WITHOUT_PIPEWIRE.into()));
        }
        CoreOptions {
            config_dir,
            watch_later_dir: self.paths.watch_later_dir(),
            options,
            verbose: self.verbose,
            list_audio_devices: !pipewire_missing,
        }
    }

    // ---- Windows --------------------------------------------------------

    pub fn new_window(self: &Rc<Self>) -> Option<Rc<PlayerController>> {
        self.new_window_with(&[])
    }

    /// A new window whose mpv core also gets `mpv_options`. Such a window
    /// keeps them, so it isn't reused for opens that didn't ask for them.
    fn new_window_with(
        self: &Rc<Self>,
        mpv_options: &[(String, String)],
    ) -> Option<Rc<PlayerController>> {
        let mut options = self.core_options();
        options.options.extend(mpv_options.iter().cloned());
        match PlayerController::new(self, &options) {
            Ok(controller) => {
                controller.set_own_options(!mpv_options.is_empty());
                self.windows.borrow_mut().push(controller.clone());
                self.set_active(&controller);
                self.start_media_controls(&controller);
                Some(controller)
            }
            Err(error) => {
                log::error!("could not open a player window: {error}");
                if self.windows.borrow().is_empty() {
                    eprintln!("patina: {error}");
                    self.stop_serving();
                    let _ = slint::quit_event_loop();
                }
                None
            }
        }
    }

    /// Windows any open may go to: not those started with a request's own
    /// mpv options.
    fn shared_windows(&self) -> Vec<Rc<PlayerController>> {
        self.windows
            .borrow()
            .iter()
            .filter(|window| !window.has_own_options())
            .cloned()
            .collect()
    }

    /// Opens sources the way IINA does: in an idle window if there is one,
    /// otherwise in a new window (or the newest one, when multiple windows
    /// are turned off). Appending always targets the newest window.
    pub fn open_sources(
        self: &Rc<Self>,
        sources: Vec<String>,
        append: bool,
    ) -> Option<Rc<PlayerController>> {
        if sources.is_empty() {
            return None;
        }
        let windows = self.shared_windows();
        let target = windows
            .iter()
            .rev()
            .find(|window| window.is_idle())
            .cloned()
            .or_else(|| {
                (append || !self.prefs().open_in_new_window)
                    .then(|| windows.last().cloned())
                    .flatten()
            });
        let target = match target {
            Some(target) => target,
            None => self.new_window()?,
        };
        target.open(sources, append);
        Some(target)
    }

    // ---- Requests -------------------------------------------------------

    /// Serves later launches from now on.
    pub fn set_instance(&self, server: crate::instance::Server) {
        *self.instance.borrow_mut() = Some(server);
    }

    /// Stops serving other launches, so that the next one becomes the
    /// running Patina; from now on requests are refused.
    fn stop_serving(&self) {
        self.quitting.set(true);
        drop(self.instance.borrow_mut().take());
    }

    /// Opens what Patina was started with, then what came from other
    /// launches or the system meanwhile. It waits for the event loop to
    /// deliver what is already queued: on macOS, a file Finder opened
    /// Patina with arrives as an event, and then takes the welcome
    /// window's place.
    pub fn launch(self: &Rc<Self>, request: Request) {
        let queued = slint::invoke_from_event_loop(|| {
            App::with_current(|app| app.start(request));
        });
        if let Err(error) = queued {
            log::warn!("could not start: {error}");
            let _ = slint::quit_event_loop();
        }
    }

    fn start(self: &Rc<Self>, request: Request) {
        self.started.set(true);
        let early = std::mem::take(&mut *self.early.borrow_mut());
        let mut first = None;
        if !request.sources.is_empty() {
            first = self.open_request_now(request);
        } else if early.iter().all(|(_, early)| early.sources.is_empty()) {
            first = self.new_window();
            if let Some(window) = &first {
                if self.prefs().launch_action == "open-dialog" {
                    window.run_action("open-file");
                }
                if request.fullscreen {
                    window.enter_fullscreen();
                }
                // The launcher's startup notice ends when the window shows.
                window.raise_with(request.activation_token);
            }
        }
        // Opens that waited for startup batch as they would have had they
        // come later: only within BATCH_WITHIN of the one before.
        let mut previous = self.started_at;
        for (came, request) in early {
            if came.saturating_duration_since(previous) >= BATCH_WITHIN {
                *self.last_open.borrow_mut() = None;
            }
            previous = came;
            let opened = self.open_request_now(request);
            first = first.or(opened);
        }
        if let Some(window) = &first {
            watch_first_window(window);
        }
    }

    /// A request from another launch, a link or the system. Opened means it
    /// is open, or will be once startup is done.
    pub fn open_request(self: &Rc<Self>, request: Request) -> crate::instance::Verdict {
        use crate::instance::Verdict;
        if self.quitting.get() {
            return Verdict::Quitting;
        }
        if let Some(accepted) = &mut *self.accepted.borrow_mut() {
            accepted.push(request.clone());
        }
        if !self.started.get() {
            self.early.borrow_mut().push((Instant::now(), request));
            return Verdict::Opened;
        }
        match self.open_request_now(request) {
            Some(_) => Verdict::Opened,
            None => Verdict::Failed("Patina could not open a window".into()),
        }
    }

    /// The requests this Patina took from other launches and the system
    /// while it started, to hand on to a Patina starting in its place.
    pub fn accepted_requests(&self) -> Vec<Request> {
        self.accepted.borrow().clone().unwrap_or_default()
    }

    fn open_request_now(self: &Rc<Self>, request: Request) -> Option<Rc<PlayerController>> {
        let (links, sources): (Vec<String>, Vec<String>) = request
            .sources
            .iter()
            .cloned()
            .partition(|source| crate::scheme::is_link(source));
        let mut first = None;
        for link in &links {
            let opened = match crate::scheme::parse(link) {
                Ok(linked) => {
                    crate::scheme::note_link_sources(&self.link_sources, &linked.sources);
                    self.open_one(Request {
                        activation_token: request.activation_token.clone(),
                        ..linked
                    })
                }
                Err(error) => {
                    // Shown escaped: a link's text is a web page's.
                    log::warn!("{link:?}: {error}");
                    let window = self.active_window().or_else(|| self.new_window());
                    if let Some(window) = &window {
                        window.show_osd("Can't Open the Link", &error, None);
                        window.raise_with(request.activation_token.clone());
                    }
                    window
                }
            };
            first = first.or(opened);
        }
        if !sources.is_empty() || links.is_empty() {
            let request = Request { sources, ..request };
            if request.separate_windows {
                for source in &request.sources {
                    let opened = self.open_one(Request {
                        sources: vec![source.clone()],
                        new_window: true,
                        ..request.clone()
                    });
                    first = first.or(opened);
                }
            } else {
                let opened = self.open_one(request);
                first = first.or(opened);
            }
        }
        first
    }

    fn open_one(self: &Rc<Self>, request: Request) -> Option<Rc<PlayerController>> {
        let Request {
            sources,
            enqueue,
            new_window,
            fullscreen,
            pip,
            music_mode,
            mpv_options,
            activation_token,
            ..
        } = request;
        // mpv options are set as a window's core starts, so they take a
        // window of their own.
        let own_window = new_window || !mpv_options.is_empty();
        let plain = !sources.is_empty() && !own_window && !enqueue;
        let opening = !sources.is_empty();
        // Added to a playlist rather than opened in place.
        let mut appended = enqueue;
        let target = if sources.is_empty() {
            // Nothing to open: Patina comes to the front, as IINA does when
            // it is opened again.
            if own_window {
                self.idle_or_new_window(&mpv_options)
            } else {
                self.active_window().or_else(|| self.new_window())
            }
        } else if let Some(target) = self.batch_target().filter(|_| plain) {
            target.open(sources, true);
            appended = true;
            Some(target)
        } else if own_window {
            let target = self.idle_or_new_window(&mpv_options);
            if let Some(target) = &target {
                target.open(sources, false);
            }
            target
        } else {
            self.open_sources(sources, enqueue)
        }?;
        if plain {
            *self.last_open.borrow_mut() = Some((Instant::now(), Rc::downgrade(&target)));
        }
        // Full screen wins over picture-in-picture, as in IINA's links.
        // Picture-in-picture waits for the video that is opening, unless
        // the files only joined a playlist whose video is showing. Music
        // mode comes at once, as with IINA's --music-mode.
        if music_mode && opening {
            target.start_music_mode();
        } else if fullscreen {
            target.enter_fullscreen();
        } else if pip && opening && !(appended && target.has_video()) {
            target.pip_when_video();
        } else if pip {
            target.enter_pip();
        }
        target.raise_with(activation_token);
        Some(target)
    }

    /// Where the library opens a page for a window that plays (Go to
    /// Album): a window with nothing to play, or a new one.
    pub fn library_window(self: &Rc<Self>) -> Option<Rc<PlayerController>> {
        self.idle_or_new_window(&[])
    }

    fn idle_or_new_window(
        self: &Rc<Self>,
        mpv_options: &[(String, String)],
    ) -> Option<Rc<PlayerController>> {
        let idle = mpv_options
            .is_empty()
            .then(|| {
                let windows = self.shared_windows();
                windows
                    .iter()
                    .rev()
                    .find(|window| window.is_idle())
                    .cloned()
            })
            .flatten();
        idle.or_else(|| self.new_window_with(mpv_options))
    }

    /// The window of a plain open moments ago, if it is still open.
    fn batch_target(&self) -> Option<Rc<PlayerController>> {
        let (at, window) = self.last_open.borrow().clone()?;
        let window = window.upgrade().filter(|_| at.elapsed() < BATCH_WITHIN)?;
        self.find(&window)
    }

    /// The window used most recently, or the newest.
    fn active_window(&self) -> Option<Rc<PlayerController>> {
        let active = self.active.borrow().upgrade();
        active.or_else(|| self.windows.borrow().last().cloned())
    }

    /// Keeps the screen awake while any window is playing, as IINA does.
    /// Keeps the screen awake while a window plays, or with the
    /// allow-screensaver-for-audio preference only the computer while all
    /// that plays is music, as IINA does.
    pub fn playback_changed(&self) {
        use crate::inhibit::Level;
        let prefs = self.prefs();
        let level = if !prefs.prevent_screensaver {
            Level::None
        } else {
            self.windows
                .borrow()
                .iter()
                .filter(|window| window.is_playing())
                .map(|window| {
                    if prefs.allow_screensaver_for_audio && window.plays_music() {
                        Level::System
                    } else {
                        Level::Display
                    }
                })
                .max()
                .unwrap_or(Level::None)
        };
        self.inhibitor.set(level);
    }

    pub fn window_closed(&self, controller: &PlayerController) {
        self.windows
            .borrow_mut()
            .retain(|window| !std::ptr::eq(Rc::as_ptr(window), controller));
        self.playback_changed();
        let active_closed = self
            .active
            .borrow()
            .upgrade()
            .is_none_or(|active| std::ptr::eq(Rc::as_ptr(&active), controller));
        if active_closed {
            let next = self.windows.borrow().last().cloned();
            match next {
                Some(next) => self.set_active(&next),
                None => self.publish_now_playing(NowPlaying::default()),
            }
        } else {
            self.retarget_media();
        }
        // The last window has saved its place by now, so the next launch
        // may start afresh.
        if self.windows.borrow().is_empty() {
            self.stop_serving();
            let _ = slint::quit_event_loop();
        }
    }

    pub fn quit(&self) {
        // Refuse further requests at once, but keep the lock until the
        // windows have saved the history and settings, which the next
        // Patina reads.
        self.quitting.set(true);
        if let Some(downloads) = self.existing_downloads() {
            downloads.stop_all();
        }
        self.youtube.quit();
        // OpenSubtitles asks clients to sign out, and the Jellyfin servers
        // hear where playback stopped as the windows close; neither is worth
        // more than a moment of quitting, which they share.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let (signed_out, done) = std::sync::mpsc::channel();
        let signing_out = std::thread::Builder::new()
            .name("opensubtitles".into())
            .spawn(move || {
                crate::opensubtitles::sign_out_shared();
                let _ = signed_out.send(());
            })
            .is_ok();
        let windows = self.windows.borrow().clone();
        for window in windows {
            window.close();
        }
        if signing_out {
            let _ =
                done.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()));
        }
        self.jellyfin.flush_reports(deadline);
        self.stop_serving();
        let _ = slint::quit_event_loop();
    }

    // ---- File information -----------------------------------------------

    /// The probe of a local file, requesting it in the background the first
    /// time. `None` until it is known.
    pub fn probe(self: &Rc<Self>, path: &str) -> Option<crate::probe::Probe> {
        if let Some(known) = self.probes.borrow().get(path) {
            return known.clone();
        }
        if crate::util::is_url(path) || !self.probing.borrow_mut().insert(path.to_owned()) {
            return None;
        }
        if self.prober.borrow().is_none() {
            let (prober, results) = crate::probe::Prober::start();
            *self.prober.borrow_mut() = Some(prober);
            let weak = Rc::downgrade(self);
            let spawned = slint::spawn_local(async move {
                while let Ok((path, probe)) = results.recv().await {
                    let Some(app) = weak.upgrade() else { break };
                    app.probing.borrow_mut().remove(&path);
                    app.probes.borrow_mut().insert(path.clone(), probe);
                    let windows = app.windows.borrow().clone();
                    for window in windows {
                        window.probe_ready(&path);
                    }
                }
            });
            if let Err(error) = spawned {
                log::warn!("could not receive file information: {error}");
            }
        }
        if let Some(prober) = &*self.prober.borrow() {
            prober.request(path);
        }
        None
    }

    /// How far `path` was watched, from the history: 0..1.
    pub fn watched(&self, path: &str) -> Option<f64> {
        self.history
            .borrow()
            .entries
            .iter()
            .find(|entry| entry.path == path)
            .filter(|entry| entry.duration > 0.0 && entry.position > 0.0)
            .map(|entry| (entry.position / entry.duration).clamp(0.0, 1.0))
    }

    // ---- Preferences ----------------------------------------------------

    pub fn open_preferences(self: &Rc<Self>) {
        let existing = self.preferences.borrow().clone();
        let controller = match existing {
            Some(controller) => controller,
            None => match crate::preferences::PreferencesController::new(self) {
                Ok(controller) => {
                    *self.preferences.borrow_mut() = Some(controller.clone());
                    controller
                }
                Err(error) => {
                    log::error!("could not open the preferences: {error}");
                    return;
                }
            },
        };
        controller.show();
    }

    // ---- Playback history window ---------------------------------------

    pub fn open_history(self: &Rc<Self>) {
        let existing = self.history_window.borrow().clone();
        let controller = match existing {
            Some(controller) => controller,
            None => match crate::history::HistoryController::new(self) {
                Ok(controller) => {
                    *self.history_window.borrow_mut() = Some(controller.clone());
                    controller
                }
                Err(error) => {
                    log::error!("could not open the playback history: {error}");
                    return;
                }
            },
        };
        controller.show();
    }

    // ---- Downloads ------------------------------------------------------

    /// The downloads, made the first time they are needed.
    fn downloads(self: &Rc<Self>) -> Option<Rc<crate::downloads::Downloads>> {
        if let Some(downloads) = self.existing_downloads() {
            return Some(downloads);
        }
        match crate::downloads::Downloads::new(self) {
            Ok(downloads) => {
                *self.downloads.borrow_mut() = Some(downloads.clone());
                Some(downloads)
            }
            Err(error) => {
                log::error!("could not open the downloads: {error}");
                None
            }
        }
    }

    pub fn existing_downloads(&self) -> Option<Rc<crate::downloads::Downloads>> {
        self.downloads.borrow().clone()
    }

    pub fn open_downloads(self: &Rc<Self>) {
        if let Some(downloads) = self.downloads() {
            downloads.show();
        }
    }

    /// Starts downloading and shows the downloads, as IINA's Online Media
    /// plugin does; false when yt-dlp could not be run.
    pub fn download(self: &Rc<Self>, request: crate::downloads::Request) -> bool {
        let Some(downloads) = self.downloads() else {
            return false;
        };
        let started = downloads.start(request);
        downloads.show();
        started
    }

    /// Adds files from a Jellyfin server to the downloads and shows them,
    /// as `download` does for yt-dlp; gives which of `requests` were added,
    /// by their place, leaving out those already downloading, or None when
    /// the downloads couldn't open.
    pub fn download_jellyfin(
        self: &Rc<Self>,
        requests: Vec<crate::jellyfin::download::Request>,
    ) -> Option<Vec<usize>> {
        let downloads = self.downloads()?;
        let added = downloads.start_jellyfin(requests);
        downloads.show();
        Some(added)
    }

    /// mpv's own configuration directory, where yt-dlp may be.
    pub fn mpv_config_dir(&self) -> std::path::PathBuf {
        self.paths.mpv_config_dir()
    }

    /// A message on the window used most recently.
    pub fn notify(&self, text: &str, detail: &str) {
        let active = self.active.borrow().upgrade();
        if let Some(window) = active {
            window.show_osd(text, detail, None);
        }
    }

    /// A message that explains why something just done came to nothing,
    /// up for `seconds` in `window` (winit's), or in the window used most
    /// recently when that isn't one of Patina's, over everything else
    /// there, a dialog included: the toast, which unlike the playback
    /// messages is shown in music mode too, and in every window with a text
    /// field. Only the Wayland clipboard has one to give, for a paste in
    /// the window that has the keyboard focus.
    #[cfg(all(unix, not(target_os = "macos")))]
    pub fn explain(
        &self,
        window: Option<slint::winit_030::winit::window::WindowId>,
        text: &str,
        detail: &str,
        seconds: f32,
    ) {
        let is = |shown: &slint::Window| {
            window.is_some() && shown.with_winit_window(|winit| winit.id()) == window
        };
        let players = self.windows.borrow().clone();
        if let Some(player) = players.iter().find(|player| is(player.window().window())) {
            return self.show_toast(player.window(), text, detail, seconds);
        }
        let preferences = self.preferences.borrow().clone();
        if let Some(preferences) = preferences.filter(|shown| is(shown.window().window())) {
            return self.show_toast(preferences.window(), text, detail, seconds);
        }
        let history = self.history_window.borrow().clone();
        if let Some(history) = history.filter(|shown| is(shown.window().window())) {
            return self.show_toast(history.window(), text, detail, seconds);
        }
        let inspector = self.inspector.borrow().clone();
        if let Some(inspector) = inspector.filter(|shown| is(shown.window().window())) {
            return self.show_toast(inspector.window(), text, detail, seconds);
        }
        if let Some(player) = self.active_window() {
            self.show_toast(player.window(), text, detail, seconds);
        }
    }

    /// Shows the toast in `window`, taking down the one shown before.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn show_toast<W>(&self, window: &W, text: &str, detail: &str, seconds: f32)
    where
        W: ComponentHandle + 'static,
        for<'a> crate::Toast<'a>: slint::Global<'a, W>,
    {
        if let Some(hide) = self.toast_hide.take() {
            hide();
        }
        let toast = window.global::<crate::Toast>();
        toast.set_text(text.into());
        toast.set_detail(detail.into());
        toast.set_shown(true);
        let window = window.as_weak();
        let hide: Rc<dyn Fn()> = Rc::new(move || {
            if let Some(window) = window.upgrade() {
                window.global::<crate::Toast>().set_shown(false);
            }
        });
        self.toast_hide.replace(Some(Rc::clone(&hide)));
        self.toast_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_secs_f32(seconds),
            move || hide(),
        );
    }

    // ---- Subtitle choices -------------------------------------------------

    /// The choices, shared with the threads that load files.
    pub fn subtitle_choices(&self) -> crate::subtitle_match::Choices {
        self.subtitle_choices.clone()
    }

    pub fn subtitle_choice(&self, path: &str) -> crate::subtitle_match::Choice {
        self.subtitle_choices
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(path)
            .cloned()
            .unwrap_or_default()
    }

    pub fn update_subtitle_choice(
        &self,
        path: &str,
        update: impl FnOnce(&mut crate::subtitle_match::Choice),
    ) {
        let mut choices = self
            .subtitle_choices
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let choice = choices.entry(path.to_owned()).or_default();
        update(choice);
        if *choice == Default::default() {
            choices.remove(path);
        }
    }

    /// Picks subtitle files for one playlist entry, as IINA's Add
    /// Subtitle… does: they load whenever it plays, now too if it is.
    pub fn add_subtitles_dialog(self: &Rc<Self>, owner: &PlayerController, path: &str) {
        let dialog = self
            .dialog_for(Some(owner))
            .set_title(format!("Subtitles for {}", file_name(path)))
            .add_filter("Subtitle files", crate::formats::SUBTITLE_EXTENSIONS)
            .add_filter("All files", &["*"]);
        let target = self.find(owner);
        let app = Rc::downgrade(self);
        let path = path.to_owned();
        spawn(async move {
            let Some(files) = dialog.pick_files().await else {
                return;
            };
            let files: Vec<String> = files
                .iter()
                .map(|file| file.path().to_string_lossy().into_owned())
                .collect();
            if let Some(app) = app.upgrade() {
                app.update_subtitle_choice(&path, |choice| choice.added.extend(files.clone()));
            }
            if let Some(target) = target
                && target.current_path() == path
            {
                for (index, file) in files.iter().enumerate() {
                    let flag = if index == 0 { "select" } else { "auto" };
                    target.run_mpv_command(&["sub-add", file, flag]);
                }
            }
        });
    }

    /// What links opened, shared with the threads that load files.
    pub fn link_sources(&self) -> crate::scheme::LinkSources {
        self.link_sources.clone()
    }

    // ---- Clipboard --------------------------------------------------------

    /// Runs `use` with the system clipboard, opened on first use and kept:
    /// on X11 copied text lasts only while its owner does. On Wayland the
    /// clipboard is clipboard.rs's instead, as it is for Slint's text fields.
    fn with_clipboard<R>(
        &self,
        r#use: impl FnOnce(&mut arboard::Clipboard) -> Result<R, arboard::Error>,
    ) -> Option<R> {
        let mut clipboard = self.clipboard.borrow_mut();
        if clipboard.is_none() {
            match arboard::Clipboard::new() {
                Ok(opened) => *clipboard = Some(opened),
                Err(error) => {
                    log::warn!("no clipboard: {error}");
                    return None;
                }
            }
        }
        let clipboard = clipboard.as_mut()?;
        r#use(clipboard)
            .map_err(|error| log::warn!("clipboard: {error}"))
            .ok()
    }

    pub fn set_clipboard_text(&self, text: String) {
        #[cfg(all(unix, not(target_os = "macos")))]
        if crate::clipboard::on_wayland() {
            return crate::clipboard::set_text(slint::platform::Clipboard::DefaultClipboard, text);
        }
        self.with_clipboard(|clipboard| clipboard.set_text(text));
    }

    pub fn clipboard_text(&self) -> Option<String> {
        #[cfg(all(unix, not(target_os = "macos")))]
        if crate::clipboard::on_wayland() {
            return crate::clipboard::text(slint::platform::Clipboard::DefaultClipboard);
        }
        self.with_clipboard(|clipboard| clipboard.get_text())
    }

    // ---- Inspector -------------------------------------------------------

    pub fn open_inspector(self: &Rc<Self>) {
        let existing = self.inspector.borrow().clone();
        let inspector = match existing {
            Some(inspector) => inspector,
            None => match crate::inspector::InspectorController::new(self) {
                Ok(inspector) => {
                    *self.inspector.borrow_mut() = Some(inspector.clone());
                    inspector
                }
                Err(error) => {
                    log::error!("could not open the inspector: {error}");
                    return;
                }
            },
        };
        let active = self.active.borrow().upgrade();
        if let Some(active) = active {
            inspector.follow(&active);
        }
        inspector.show();
    }

    pub fn inspector_reply(
        &self,
        userdata: u64,
        result: crate::mpv::Result<(String, crate::mpv::Node)>,
    ) {
        let inspector = self.inspector.borrow().clone();
        if let Some(inspector) = inspector {
            inspector.reply(userdata, result);
        }
    }

    // ---- Media Info -----------------------------------------------------

    pub fn open_media_info(self: &Rc<Self>) {
        let existing = self.media_info.borrow().clone();
        let media_info = match existing {
            Some(media_info) => media_info,
            None => match crate::media_info::MediaInfoController::new(self) {
                Ok(media_info) => {
                    *self.media_info.borrow_mut() = Some(media_info.clone());
                    media_info
                }
                Err(error) => {
                    log::error!("could not open the media info: {error}");
                    return;
                }
            },
        };
        let active = self.active.borrow().upgrade();
        if let Some(active) = active {
            media_info.follow(&active);
        }
        media_info.show();
    }

    pub fn media_info_reply(&self, userdata: u64, result: crate::mpv::Result<crate::mpv::Node>) {
        let media_info = self.media_info.borrow().clone();
        if let Some(media_info) = media_info {
            media_info.reply(userdata, result);
        }
    }

    /// What `player` plays, its tags or its tracks changed.
    pub fn media_changed(&self, player: &PlayerController) {
        let media_info = self.media_info.borrow().clone();
        if let Some(media_info) = media_info {
            media_info.changed(player);
        }
    }

    /// Plays a file from the history window: in the active window, or in a
    /// new one.
    pub fn play_from_history(self: &Rc<Self>, path: String, new_window: bool) {
        let active = self.active.borrow().upgrade();
        let active = active.filter(|active| !active.has_own_options());
        let target = match active {
            Some(active) if !new_window => Some(active),
            _ => self.new_window(),
        };
        if let Some(target) = target {
            target.open_again(path);
            target.raise();
        }
    }

    /// Asks the player audio_devices reads from to list the devices, when
    /// its core doesn't keep them listed; audio_devices_changed follows.
    pub fn request_audio_devices(&self) {
        let window = self
            .active
            .borrow()
            .upgrade()
            .or_else(|| self.windows.borrow().last().cloned());
        if let Some(window) = window {
            window.request_audio_devices();
        }
    }

    /// A player's audio devices, asked for, have arrived.
    pub fn audio_devices_changed(&self) {
        let preferences = self.preferences.borrow().clone();
        if let Some(preferences) = preferences {
            preferences.audio_devices_changed();
        }
    }

    /// Audio devices as (name, description), from the active player.
    pub fn audio_devices(&self) -> Vec<(String, String)> {
        let window = self
            .active
            .borrow()
            .upgrade()
            .or_else(|| self.windows.borrow().last().cloned());
        let mut devices = window
            .map(|window| window.audio_devices())
            .unwrap_or_default();
        if devices.is_empty() {
            devices.push(("auto".into(), "Autoselect device".into()));
        }
        devices
    }

    // ---- System media controls ------------------------------------------

    /// Starts the system's media controls, once, and shows what is playing
    /// in them. Windows attaches them to a window, and Slint makes a shown
    /// window's native window only when the event loop is next idle; until
    /// then this does nothing, and the window's events call it again.
    pub fn start_media_controls(&self, first: &PlayerController) {
        if self.media_started.get() || !self.prefs().media_keys {
            return;
        }
        use raw_window_handle::HasWindowHandle;
        use slint::winit_030::WinitWindowAccessor;
        let window = first
            .window()
            .window()
            .with_winit_window(|winit| winit.window_handle().ok().map(|handle| handle.as_raw()))
            .flatten();
        if cfg!(windows) && window.is_none() {
            return;
        }
        self.media_started.set(true);
        let handler: media_controls::Handler = std::sync::Arc::new(|command| {
            let _ = slint::invoke_from_event_loop(move || {
                App::with_current(|app| app.media_command(command));
            });
        });
        *self.media.borrow_mut() = MediaControls::start(handler, window);
        // Whatever was published before they started.
        *self.now_playing.borrow_mut() = NowPlaying::default();
        let target = self.media_target.borrow().upgrade();
        if let Some(target) = target {
            target.publish_now_playing();
        }
    }

    pub fn set_active(&self, controller: &Rc<PlayerController>) {
        let serial = self.focus_serial.get() + 1;
        self.focus_serial.set(serial);
        controller.set_focus_serial(serial);
        let changed = self
            .active
            .borrow()
            .upgrade()
            .is_none_or(|active| !Rc::ptr_eq(&active, controller));
        if changed {
            *self.active.borrow_mut() = Rc::downgrade(controller);
            let inspector = self.inspector.borrow().clone();
            if let Some(inspector) = inspector {
                inspector.follow(controller);
            }
            let media_info = self.media_info.borrow().clone();
            if let Some(media_info) = media_info {
                media_info.follow(controller);
            }
        }
        self.retarget_media();
    }

    /// The window used last of those with something in their playlist,
    /// other than `except`: where Play Next and Add to Queue from the
    /// library in `except` go.
    pub fn queue_window(&self, except: &PlayerController) -> Option<Rc<PlayerController>> {
        let windows = self.windows.borrow();
        used_last(
            windows
                .iter()
                .filter(|window| !std::ptr::eq(Rc::as_ptr(window), except))
                .map(|window| (window, window.is_idle(), window.focus_serial())),
        )
        .cloned()
    }

    /// Points the media controls and media keys at the window used last
    /// of those with something in their playlist, or at the active window
    /// when none has, and shows what it plays if that is another window.
    /// The library shows only in a window with nothing to play, and Play
    /// Next and Go to Album send the user to one: browsing there must not
    /// take the controls from the window playing the music. Called when
    /// the active window changes, a window closes, and a window starts or
    /// stops having something to play.
    pub fn retarget_media(&self) {
        let target = {
            let windows = self.windows.borrow();
            used_last(
                windows
                    .iter()
                    .map(|window| (window, window.is_idle(), window.focus_serial())),
            )
            .cloned()
        }
        .or_else(|| self.active_window());
        let changed = match (&target, self.media_target.borrow().upgrade()) {
            (Some(target), Some(current)) => !Rc::ptr_eq(target, &current),
            (target, current) => target.is_some() != current.is_some(),
        };
        if !changed {
            return;
        }
        *self.media_target.borrow_mut() = target.as_ref().map_or_else(Weak::new, Rc::downgrade);
        match target {
            Some(target) => target.publish_now_playing(),
            None => self.publish_now_playing(NowPlaying::default()),
        }
    }

    /// Whether `controller` is the window the media controls show.
    pub fn is_media_target(&self, controller: &PlayerController) -> bool {
        self.media_target
            .borrow()
            .upgrade()
            .is_some_and(|target| std::ptr::eq(Rc::as_ptr(&target), controller))
    }

    /// Shows `now` in the system's media controls, if it changed.
    pub fn publish_now_playing(&self, now: NowPlaying) {
        if *self.now_playing.borrow() == now {
            return;
        }
        if let Some(media) = &*self.media.borrow() {
            let track_changed = self.now_playing.borrow().track != now.track;
            media.update(&now);
            if track_changed {
                media.seeked(0.0);
            }
        }
        *self.now_playing.borrow_mut() = now;
    }

    pub fn media_position(&self, seconds: f64, jumped: bool) {
        if let Some(media) = &*self.media.borrow() {
            if jumped {
                media.seeked(seconds);
            } else {
                media.set_position(seconds);
            }
        }
    }

    fn media_command(self: &Rc<Self>, command: media_controls::Command) {
        log::debug!("media control: {command:?}");
        match command {
            media_controls::Command::Quit => self.quit(),
            media_controls::Command::OpenUri(uri) => {
                self.open_sources(vec![uri], false);
            }
            command => {
                let target = self.media_target.borrow().upgrade();
                if let Some(target) = target.or_else(|| self.active_window()) {
                    target.media_command(command);
                }
            }
        }
    }

    // ---- History --------------------------------------------------------

    /// `section` says the YouTube section sent them. It goes by the last time,
    /// as the same address opened another way isn't the section's.
    pub fn note_opened(&self, sources: &[String], section: bool) {
        if !self.prefs().record_history {
            return;
        }
        let mut history = self.history.borrow_mut();
        for source in sources.iter().rev() {
            if let Some(first) = history.entries.first_mut()
                && &first.path == source
            {
                first.section = section;
                continue;
            }
            let previous = history
                .entries
                .iter()
                .find(|entry| &entry.path == source)
                .cloned();
            let title = self
                .jellyfin
                .entry_title(source)
                .or_else(|| self.youtube.title_of(source))
                .unwrap_or_else(|| file_name(source));
            let mut entry = previous.unwrap_or_else(|| HistoryEntry {
                path: source.clone(),
                title,
                played_at: settings::now(),
                duration: 0.0,
                position: 0.0,
                section,
            });
            entry.section = section;
            history.record(entry);
        }
        drop(history);
        self.history_changed();
    }

    pub fn record_history(&self, path: &str, title: &str, duration: f64, section: bool) {
        if path.is_empty() || !self.prefs().record_history {
            return;
        }
        let mut history = self.history.borrow_mut();
        let (position, known_duration) = history
            .entries
            .iter()
            .find(|entry| entry.path == path)
            .map_or((0.0, 0.0), |entry| (entry.position, entry.duration));
        // The duration may not be known yet when the file has just loaded.
        let duration = if duration > 0.0 {
            duration
        } else {
            known_duration
        };
        history.record(HistoryEntry {
            path: path.to_owned(),
            title: if title.is_empty() {
                file_name(path)
            } else {
                title.to_owned()
            },
            played_at: settings::now(),
            duration,
            position,
            section,
        });
        drop(history);
        self.history_changed();
    }

    pub fn update_history(&self, path: &str, position: f64, duration: f64) {
        if path.is_empty() || !self.prefs().record_history {
            return;
        }
        self.history
            .borrow_mut()
            .update_progress(path, position, duration);
        settings::save_json(&self.paths.history_file(), &*self.history.borrow());
        // The section's History page shows the length, which a file that has
        // only just loaded when it is recorded may not have had.
        self.youtube.set_history(&self.history.borrow().entries);
        self.refresh_history_window();
    }

    pub fn history_entries(&self) -> Vec<HistoryEntry> {
        self.history.borrow().entries.clone()
    }

    pub fn remove_from_history(&self, path: &str) {
        self.history
            .borrow_mut()
            .entries
            .retain(|entry| entry.path != path);
        if let Some(id) = self.youtube.video_at(path) {
            self.youtube.forget_watch(&id);
        }
        self.history_changed();
    }

    pub fn clear_history(&self) {
        self.history.borrow_mut().entries.clear();
        self.youtube.clear_marks();
        self.history_changed();
    }

    /// The YouTube section's Clear History: its videos leave the history, and
    /// every other file stays. The section clears the marks itself.
    fn forget_youtube_history(&self) {
        let youtube = self.youtube.clone();
        self.history
            .borrow_mut()
            .entries
            .retain(|entry| youtube.video_at(&entry.path).is_none());
        self.history_changed();
    }

    fn history_changed(&self) {
        settings::save_json(&self.paths.history_file(), &*self.history.borrow());
        let entries = self.history.borrow().entries.clone();
        self.youtube.set_history(&entries);
        let windows = self.windows.borrow().clone();
        for window in windows {
            window.refresh_recent();
        }
        self.refresh_history_window();
    }

    fn refresh_history_window(&self) {
        let history_window = self.history_window.borrow().clone();
        if let Some(history_window) = history_window {
            history_window.refresh();
        }
    }

    /// Recent files as (title, path), and the title offered for resuming.
    pub fn recent_items(&self) -> (Vec<(String, String)>, String) {
        let history = self.history.borrow();
        let limit = self.prefs().recent_limit;
        let items: Vec<_> = history
            .recent(limit)
            .map(|entry| (entry.title.clone(), entry.path.clone()))
            .collect();
        let resume = history
            .entries
            .first()
            .map(|entry| {
                if entry.position > 1.0 {
                    format!(
                        "{} — {}",
                        entry.title,
                        crate::util::format_time(entry.position)
                    )
                } else {
                    entry.title.clone()
                }
            })
            .unwrap_or_default();
        (items, resume)
    }

    /// Whether the history has this as sent by the YouTube section, which then
    /// hears what it was called: it may be a long time since, and yt-dlp takes a
    /// while to say.
    pub fn came_from_section(&self, path: &str) -> bool {
        let history = self.history.borrow();
        let Some(entry) = history.entries.iter().find(|entry| entry.path == path) else {
            return false;
        };
        if entry.section && entry.title != file_name(path) {
            self.youtube
                .name_source(path.to_owned(), entry.title.clone());
        }
        entry.section
    }

    pub fn recent_path(&self, index: usize) -> Option<String> {
        self.history
            .borrow()
            .entries
            .get(index)
            .map(|entry| entry.path.clone())
    }

    // ---- File dialogs ---------------------------------------------------

    pub fn dialog_for(&self, owner: Option<&PlayerController>) -> rfd::AsyncFileDialog {
        let mut dialog = Some(rfd::AsyncFileDialog::new());
        if let Some(owner) = owner {
            owner.window().window().with_winit_window(|winit| {
                dialog = dialog.take().map(|dialog| dialog.set_parent(winit));
            });
        }
        dialog.unwrap_or_default()
    }

    /// Shows an open dialog; the chosen files open in `owner` (or wherever
    /// [`App::open_sources`] puts them).
    pub fn open_file_dialog(
        self: &Rc<Self>,
        owner: Option<&PlayerController>,
        append: bool,
        folder: bool,
    ) {
        let dialog = self.dialog_for(owner).set_title(match (append, folder) {
            (_, true) => "Open Folder",
            (true, false) => "Add to Playlist",
            (false, false) => "Open Media",
        });
        let target = owner.and_then(|owner| self.find(owner));
        let app = Rc::downgrade(self);
        spawn(async move {
            let chosen = if folder {
                dialog.pick_folder().await.map(|folder| vec![folder])
            } else {
                dialog
                    .add_filter(
                        "Media files",
                        &crate::formats::extensions(crate::formats::ALL),
                    )
                    .add_filter("All files", &["*"])
                    .pick_files()
                    .await
            };
            let Some(chosen) = chosen else { return };
            let mut paths: Vec<String> = chosen
                .iter()
                .map(|file| file.path().to_string_lossy().into_owned())
                .collect();
            paths.sort_by(|a, b| crate::window::natural_cmp(a, b));
            match (target, app.upgrade()) {
                (Some(target), _) if append || target.is_idle() => target.open(paths, append),
                (_, Some(app)) => {
                    app.open_sources(paths, append);
                }
                _ => {}
            }
        });
    }

    pub fn load_track_dialog(self: &Rc<Self>, owner: &PlayerController, kind: TrackKind) {
        let (title, filter, extensions, command) = match kind {
            TrackKind::Sub => (
                "Load Subtitles",
                "Subtitle files",
                crate::formats::SUBTITLE_EXTENSIONS.to_vec(),
                "sub-add",
            ),
            _ => (
                "Load Audio",
                "Audio files",
                crate::formats::extensions(crate::formats::ALL),
                "audio-add",
            ),
        };
        let dialog = self
            .dialog_for(Some(owner))
            .set_title(title)
            .add_filter(filter, &extensions)
            .add_filter("All files", &["*"]);
        let target = self.find(owner);
        spawn(async move {
            let Some(files) = dialog.pick_files().await else {
                return;
            };
            if let Some(target) = target {
                for (index, file) in files.iter().enumerate() {
                    let path = file.path().to_string_lossy().into_owned();
                    let flag = if index == 0 { "select" } else { "auto" };
                    target.run_mpv_command(&[command, &path, flag]);
                }
            }
        });
    }

    pub fn save_playlist_dialog(self: &Rc<Self>, owner: &PlayerController) {
        let entries = owner.playlist_paths();
        let dialog = self
            .dialog_for(Some(owner))
            .set_title("Save Playlist")
            .set_file_name("Playlist.m3u8")
            .add_filter("M3U playlist", &["m3u8", "m3u"]);
        spawn(async move {
            let Some(file) = dialog.save_file().await else {
                return;
            };
            let mut text = String::from("#EXTM3U\n");
            for (title, path) in entries {
                text.push_str(&format!("#EXTINF:-1,{title}\n{path}\n"));
            }
            if let Err(error) = std::fs::write(file.path(), text) {
                log::warn!("could not save {}: {error}", file.path().display());
            }
        });
    }

    fn find(&self, controller: &PlayerController) -> Option<Rc<PlayerController>> {
        self.windows
            .borrow()
            .iter()
            .find(|window| std::ptr::eq(Rc::as_ptr(window), controller))
            .cloned()
    }
}

/// The text of key binding set `name`, or the defaults for the built-in set
/// (or a set that can no longer be read).
fn binding_conf(paths: &Paths, name: &str) -> String {
    if name.is_empty() {
        return crate::input::default_conf();
    }
    let path = paths.input_conf_dir().join(format!("{name}.conf"));
    std::fs::read_to_string(&path).unwrap_or_else(|error| {
        log::warn!(
            "could not read {}: {error}; using the default bindings",
            path.display()
        );
        crate::input::default_conf()
    })
}

/// Slint makes a shown window's native window when the event loop is next
/// idle. When it can't (OpenGL failed), it keeps the error for when the
/// loop ends but doesn't end it, so the window never appears and nothing
/// says why. This ends the loop for it.
fn watch_first_window(window: &Rc<PlayerController>) {
    use slint::winit_030::WinitWindowAccessor;
    let window = Rc::downgrade(window);
    slint::Timer::single_shot(Duration::from_secs(3), move || {
        let missing = window.upgrade().is_some_and(|window| {
            let window = window.window().window();
            window.is_visible() && window.with_winit_window(|_| ()).is_none()
        });
        if missing {
            crate::video::software::restart("the window never appeared".into());
        } else {
            // Started: nothing to hand over from here on.
            App::with_current(|app| *app.accepted.borrow_mut() = None);
        }
    });
}

fn spawn(future: impl std::future::Future<Output = ()> + 'static) {
    if let Err(error) = slint::spawn_local(future) {
        log::error!("could not show the dialog: {error}");
    }
}

/// Whether the user chose mpv's audio output: in their own mpv options or on
/// the command line (`options`), or in the mpv.conf Patina reads.
fn chooses_audio_output(
    options: &[(String, String)],
    config_dir: Option<&std::path::Path>,
) -> bool {
    options.iter().any(|(name, _)| name == "ao")
        || config_dir
            .and_then(|dir| std::fs::read_to_string(dir.join("mpv.conf")).ok())
            .is_some_and(|conf| sets_audio_output(&conf))
}

/// Whether an mpv.conf sets `ao`, as `ao=…` or `--ao=…`, anywhere in it.
fn sets_audio_output(conf: &str) -> bool {
    conf.lines().any(|line| {
        let line = line.trim().trim_start_matches("--");
        line.strip_prefix("ao")
            .is_some_and(|rest| rest.trim_start().starts_with('='))
    })
}

/// Of windows as (window, whether it has nothing to play, focus serial),
/// the one focused last of those with something to play.
fn used_last<T>(windows: impl IntoIterator<Item = (T, bool, u64)>) -> Option<T> {
    windows
        .into_iter()
        .filter(|(_, idle, _)| !idle)
        .max_by_key(|(_, _, serial)| *serial)
        .map(|(window, _, _)| window)
}

#[cfg(test)]
mod tests {
    use super::{sets_audio_output, used_last};

    #[test]
    fn an_mpv_conf_that_sets_ao_is_told_from_one_that_does_not() {
        assert!(sets_audio_output("vo=gpu\nao=alsa\n"));
        assert!(sets_audio_output("  --ao = pulse"));
        assert!(!sets_audio_output("audio-device=alsa/default\n# ao=pulse"));
        assert!(!sets_audio_output("aoc=1\n"));
    }

    #[test]
    fn media_follow_the_window_used_last_that_plays() {
        // The library's window, focused last, plays nothing: the window
        // playing keeps the media controls.
        assert_eq!(
            used_last([("playing", false, 3), ("library", true, 5)]),
            Some("playing")
        );
        assert_eq!(
            used_last([("first", false, 3), ("second", false, 7), ("idle", true, 9)]),
            Some("second")
        );
        // None plays: the caller takes the active window.
        assert_eq!(used_last([("a", true, 3), ("b", true, 5)]), None);
        assert_eq!(used_last::<&str>([]), None);
    }
}
