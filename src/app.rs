//! Application state shared by all windows: preferences, history, key
//! bindings, and the list of open player windows.

use crate::input::Bindings;
use crate::player::CoreOptions;
use crate::player::state::{TrackKind, file_name};
use crate::settings::{self, History, HistoryEntry, Paths, Preferences};
use crate::window::PlayerController;
use slint::ComponentHandle;
use slint::winit_030::WinitWindowAccessor;
use std::cell::RefCell;
use std::rc::Rc;

pub const MEDIA_EXTENSIONS: &[&str] = &[
    "3g2", "3gp", "aac", "ac3", "aif", "aiff", "alac", "ape", "asf", "avi", "dts", "eac3", "f4v",
    "flac", "flv", "h264", "hevc", "m2ts", "m3u", "m3u8", "m4a", "m4v", "mk3d", "mka", "mkv",
    "mov", "mp2", "mp3", "mp4", "mpeg", "mpg", "mts", "mxf", "oga", "ogg", "ogm", "ogv", "opus",
    "pls", "rm", "rmvb", "ts", "vob", "wav", "webm", "wma", "wmv", "wv", "y4m",
];
pub const SUBTITLE_EXTENSIONS: &[&str] = &[
    "ass", "idx", "lrc", "mks", "smi", "srt", "ssa", "sub", "sup", "vtt",
];

pub struct App {
    paths: Paths,
    prefs: RefCell<Rc<Preferences>>,
    history: RefCell<History>,
    bindings: Rc<Bindings>,
    windows: RefCell<Vec<Rc<PlayerController>>>,
    verbose: bool,
    /// mpv options given on the command line, applied after the preferences'.
    cli_mpv_options: Vec<(String, String)>,
    inhibitor: crate::inhibit::Inhibitor,
}

impl App {
    pub fn new(paths: Paths, verbose: bool, cli_mpv_options: Vec<(String, String)>) -> Rc<Self> {
        let prefs: Preferences = settings::load_json(&paths.settings_file());
        let history: History = settings::load_json(&paths.history_file());
        let conf = match std::fs::read_to_string(paths.input_conf()) {
            Ok(conf) => {
                log::info!("using key bindings from {}", paths.input_conf().display());
                conf
            }
            Err(_) => crate::input::default_conf(),
        };
        Rc::new(Self {
            paths,
            prefs: RefCell::new(Rc::new(prefs)),
            history: RefCell::new(history),
            bindings: Rc::new(Bindings::parse(&conf)),
            windows: RefCell::new(Vec::new()),
            verbose,
            cli_mpv_options,
            inhibitor: crate::inhibit::Inhibitor::new(),
        })
    }

    pub fn prefs(&self) -> Rc<Preferences> {
        self.prefs.borrow().clone()
    }

    pub fn update_prefs(&self, update: impl FnOnce(&mut Preferences)) {
        let mut prefs = (*self.prefs()).clone();
        update(&mut prefs);
        if prefs != *self.prefs() {
            settings::save_json(&self.paths.settings_file(), &prefs);
            *self.prefs.borrow_mut() = Rc::new(prefs);
        }
    }

    pub fn bindings(&self) -> Rc<Bindings> {
        self.bindings.clone()
    }

    pub fn core_options(&self) -> CoreOptions {
        let prefs = self.prefs();
        let mpv_config = self.paths.mpv_config_dir();
        CoreOptions {
            config_dir: mpv_config.is_dir().then_some(mpv_config),
            watch_later_dir: self.paths.watch_later_dir(),
            resume_playback: prefs.resume_playback,
            hwdec: if prefs.hardware_decoding {
                "auto-safe"
            } else {
                "no"
            }
            .into(),
            screenshot_dir: prefs.screenshot_dir(),
            screenshot_format: prefs.screenshot_format.clone(),
            ytdl_path: prefs.ytdl_path.clone(),
            extra_options: prefs
                .mpv_options
                .iter()
                .chain(&self.cli_mpv_options)
                .cloned()
                .collect(),
            verbose: self.verbose,
        }
    }

    // ---- Windows --------------------------------------------------------

    pub fn new_window(self: &Rc<Self>) -> Option<Rc<PlayerController>> {
        match PlayerController::new(self) {
            Ok(controller) => {
                self.windows.borrow_mut().push(controller.clone());
                Some(controller)
            }
            Err(error) => {
                log::error!("could not open a player window: {error}");
                if self.windows.borrow().is_empty() {
                    eprintln!("patina: {error}");
                    let _ = slint::quit_event_loop();
                }
                None
            }
        }
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
        let windows = self.windows.borrow().clone();
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

    /// Keeps the screen awake while any window is playing, as IINA does.
    pub fn playback_changed(&self) {
        let playing = self
            .windows
            .borrow()
            .iter()
            .any(|window| window.is_playing());
        self.inhibitor.set(playing);
    }

    pub fn window_closed(&self, controller: &PlayerController) {
        self.windows
            .borrow_mut()
            .retain(|window| !std::ptr::eq(Rc::as_ptr(window), controller));
        self.playback_changed();
        if self.windows.borrow().is_empty() {
            let _ = slint::quit_event_loop();
        }
    }

    pub fn quit(&self) {
        let windows = self.windows.borrow().clone();
        for window in windows {
            window.close();
        }
        let _ = slint::quit_event_loop();
    }

    // ---- History --------------------------------------------------------

    pub fn note_opened(&self, sources: &[String]) {
        if !self.prefs().record_history {
            return;
        }
        let mut history = self.history.borrow_mut();
        for source in sources.iter().rev() {
            if history
                .entries
                .first()
                .is_some_and(|entry| &entry.path == source)
            {
                continue;
            }
            let previous = history
                .entries
                .iter()
                .find(|entry| &entry.path == source)
                .cloned();
            history.record(previous.unwrap_or_else(|| HistoryEntry {
                path: source.clone(),
                title: file_name(source),
                played_at: settings::now(),
                duration: 0.0,
                position: 0.0,
            }));
        }
        drop(history);
        self.history_changed();
    }

    pub fn record_history(&self, path: &str, title: &str, duration: f64) {
        if path.is_empty() || !self.prefs().record_history {
            return;
        }
        let mut history = self.history.borrow_mut();
        let position = history
            .entries
            .iter()
            .find(|entry| entry.path == path)
            .map_or(0.0, |entry| entry.position);
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
    }

    pub fn clear_history(&self) {
        self.history.borrow_mut().entries.clear();
        self.history_changed();
    }

    fn history_changed(&self) {
        settings::save_json(&self.paths.history_file(), &*self.history.borrow());
        let windows = self.windows.borrow().clone();
        for window in windows {
            window.refresh_recent();
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

    pub fn recent_path(&self, index: usize) -> Option<String> {
        self.history
            .borrow()
            .entries
            .get(index)
            .map(|entry| entry.path.clone())
    }

    // ---- File dialogs ---------------------------------------------------

    fn dialog_for(&self, owner: Option<&PlayerController>) -> rfd::AsyncFileDialog {
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
                    .add_filter("Media files", MEDIA_EXTENSIONS)
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
                SUBTITLE_EXTENSIONS,
                "sub-add",
            ),
            _ => ("Load Audio", "Audio files", MEDIA_EXTENSIONS, "audio-add"),
        };
        let dialog = self
            .dialog_for(Some(owner))
            .set_title(title)
            .add_filter(filter, extensions)
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

fn spawn(future: impl std::future::Future<Output = ()> + 'static) {
    if let Err(error) = slint::spawn_local(future) {
        log::error!("could not show the dialog: {error}");
    }
}
