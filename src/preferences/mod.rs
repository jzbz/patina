//! The preferences window.
//!
//! Pages are built from [`schema::ITEMS`]. Values are read and written
//! through the JSON form of [`Preferences`], so any setting in the schema
//! works without code of its own; changes are saved and applied to running
//! players at once.

pub mod schema;

use crate::app::App;
use crate::input::{Binding, Bindings, KeyCombo};
use crate::settings::Preferences;
use crate::{BindingRow, PrefRow, PreferencesWindow};
use schema::{Control, Item, Page};
use serde_json::Value;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// Common actions offered in the binding editor: (label, command, Patina action).
const PRESETS: &[(&str, &str, bool)] = &[
    ("Play / pause", "cycle pause", false),
    ("Stop", "stop", false),
    ("Seek forward 5 seconds", "seek 5", false),
    ("Seek back 5 seconds", "seek -5", false),
    ("Seek forward 1 minute", "seek 60", false),
    ("Seek back 1 minute", "seek -60", false),
    ("Next frame", "frame-step", false),
    ("Previous frame", "frame-back-step", false),
    ("Next file", "playlist-next", false),
    ("Previous file", "playlist-prev", false),
    ("Next chapter", "add chapter 1", false),
    ("Previous chapter", "add chapter -1", false),
    ("Volume up", "add volume 5", false),
    ("Volume down", "add volume -5", false),
    ("Mute", "cycle mute", false),
    ("Double the speed", "multiply speed 2", false),
    ("Halve the speed", "multiply speed 0.5", false),
    ("Reset the speed", "set speed 1", false),
    ("Toggle full screen", "cycle fullscreen", false),
    ("Float on top", "cycle ontop", false),
    ("Screenshot", "screenshot", false),
    ("A-B loop", "ab-loop", false),
    ("Loop the file", "cycle-values loop-file inf no", false),
    ("Next subtitle track", "cycle sub", false),
    ("Next audio track", "cycle audio", false),
    ("Show or hide subtitles", "cycle sub-visibility", false),
    ("Subtitle delay +0.1 s", "add sub-delay 0.1", false),
    ("Subtitle delay −0.1 s", "add sub-delay -0.1", false),
    ("Audio delay +0.1 s", "add audio-delay 0.1", false),
    ("Audio delay −0.1 s", "add audio-delay -0.1", false),
    ("Show the playback position", "show-progress", false),
    ("Open a file", "open-file", true),
    ("Open a URL", "open-url", true),
    ("Jump to a time", "jump-to", true),
    ("Playlist panel", "playlist-panel", true),
    ("Chapters panel", "chapter-panel", true),
    ("Video settings panel", "video-panel", true),
    ("Audio settings panel", "audio-panel", true),
    ("Subtitle settings panel", "sub-panel", true),
    ("Picture in picture", "toggle-pip", true),
    ("Bigger window", "bigger-window", true),
    ("Smaller window", "smaller-window", true),
    ("Fit to screen", "fit-to-screen", true),
    ("Preferences", "preferences", true),
    ("Playback history", "history", true),
    ("Inspector", "inspector", true),
    ("Download the video", "download-video", true),
    ("Downloads", "downloads", true),
    ("Find online subtitles", "find-online-subtitles", true),
    (
        "Save the downloaded subtitle",
        "save-downloaded-subtitle",
        true,
    ),
    ("New window", "new-window", true),
    ("Show in the file manager", "reveal-file", true),
    ("Move the file to the trash", "delete-current-file", true),
    ("Float on top", "toggle-ontop", true),
    ("Toggle full screen", "toggle-fullscreen", true),
    ("Save the playlist", "save-playlist", true),
    ("Keyboard shortcuts", "show-shortcuts", true),
    ("Close the window", "close-window", true),
    ("Quit Patina", "quit", true),
];

/// A readable name for a binding, falling back to its command.
fn describe(binding: &Binding) -> String {
    let command = binding.conf_command();
    let patina = matches!(binding, Binding::Patina(_));
    PRESETS
        .iter()
        .find(|(_, preset, is_patina)| *preset == command && *is_patina == patina)
        .map(|(label, ..)| (*label).to_owned())
        .unwrap_or(command)
}

const USER_SET: &str = "My Bindings";

pub struct PreferencesController {
    app: Weak<App>,
    window: PreferencesWindow,
    page: Cell<Page>,
    query: RefCell<String>,
    /// The schema item behind each row (None for headings).
    row_items: RefCell<Vec<Option<&'static Item>>>,
    rows: Rc<VecModel<PrefRow>>,
    binding_filter: RefCell<String>,
    /// The key combinations of the listed bindings, in row order.
    binding_keys: RefCell<Vec<KeyCombo>>,
    /// The binding being edited (None when adding one).
    editing: RefCell<Option<KeyCombo>>,
    editor_combo: RefCell<Option<KeyCombo>>,
    /// What became of the last password saved, shown under its field.
    password_note: RefCell<Option<String>>,
    /// What a button's action came to, shown in place of its description.
    notes: RefCell<std::collections::HashMap<&'static str, String>>,
    /// A file type action is running; xdg-mime runs at once would overwrite
    /// each other's defaults.
    associating: Cell<bool>,
}

impl PreferencesController {
    pub fn new(app: &Rc<App>) -> Result<Rc<Self>, String> {
        let window = PreferencesWindow::new().map_err(|error| error.to_string())?;
        let rows = Rc::new(VecModel::default());
        window.set_rows(ModelRc::from(rows.clone()));
        let pages: Vec<SharedString> = Page::ALL.iter().map(|page| page.title().into()).collect();
        window.set_pages(ModelRc::new(VecModel::from(pages)));
        let mut presets: Vec<SharedString> = vec!["Choose a common action…".into()];
        presets.extend(PRESETS.iter().map(|(label, ..)| SharedString::from(*label)));
        window.set_editor_presets(ModelRc::new(VecModel::from(presets)));

        let controller = Rc::new(Self {
            app: Rc::downgrade(app),
            window,
            page: Cell::new(Page::General),
            query: RefCell::new(String::new()),
            row_items: RefCell::new(Vec::new()),
            rows,
            binding_filter: RefCell::new(String::new()),
            binding_keys: RefCell::new(Vec::new()),
            editing: RefCell::new(None),
            editor_combo: RefCell::new(None),
            password_note: RefCell::new(None),
            notes: RefCell::new(std::collections::HashMap::new()),
            associating: Cell::new(false),
        });
        controller.wire();
        controller.show_page(Page::General);
        Ok(controller)
    }

    pub fn show(&self) {
        if let Err(error) = self.window.show() {
            log::warn!("could not show the preferences: {error}");
        }
    }

    fn app(&self) -> Rc<App> {
        self.app
            .upgrade()
            .expect("the app outlives its preferences window")
    }

    fn wire(self: &Rc<Self>) {
        macro_rules! on {
            ($method:ident, |$this:ident $(, $arg:ident)*| $body:expr) => {{
                let weak = Rc::downgrade(self);
                self.window.$method(move |$($arg),*| {
                    if let Some($this) = weak.upgrade() {
                        $body
                    }
                });
            }};
        }
        on!(on_select_page, |this, index| {
            if let Some(page) = Page::ALL.get(index as usize) {
                this.query.borrow_mut().clear();
                this.show_page(*page);
            }
        });
        on!(on_search, |this, text| {
            *this.query.borrow_mut() = text.trim().to_owned();
            this.refresh();
        });
        on!(on_set_bool, |this, key, value| this
            .set(&key, Value::Bool(value)));
        on!(on_set_number, |this, key, value| this
            .set_number(&key, value as f64));
        on!(on_set_choice, |this, key, index| this
            .set_choice(&key, index));
        on!(on_set_text, |this, key, value| this.set_text(&key, &value));
        on!(on_browse, |this, key| this.browse(&key));
        on!(on_run, |this, key| this.run(&key));
        on!(on_choose_binding_set, |this, index| this.choose_set(index));
        on!(on_duplicate_binding_set, |this| this.duplicate_set());
        on!(on_delete_binding_set, |this| {
            let name = this.app().prefs().key_bindings.clone();
            if !name.is_empty() {
                this.app().delete_binding_set(&name);
                this.refresh_bindings();
            }
        });
        on!(on_reveal_binding_set, |this| {
            let name = this.app().prefs().key_bindings.clone();
            if !name.is_empty() {
                let path = this
                    .app()
                    .paths()
                    .input_conf_dir()
                    .join(format!("{name}.conf"));
                crate::platform::reveal(&path);
            }
        });
        on!(on_filter_bindings, |this, text| {
            *this.binding_filter.borrow_mut() = text.trim().to_lowercase();
            this.refresh_bindings();
        });
        on!(on_add_binding, |this| this.open_editor(None));
        on!(on_edit_binding, |this, index| {
            let combo = this.binding_keys.borrow().get(index as usize).cloned();
            if let Some(combo) = combo {
                this.open_editor(Some(combo));
            }
        });
        on!(on_remove_binding, |this, index| {
            let combo = this.binding_keys.borrow().get(index as usize).cloned();
            if let Some(combo) = combo {
                this.change_bindings(|bindings| bindings.remove(&combo));
            }
        });
        on!(on_editor_key, |this, text, shift, ctrl, alt, meta| {
            if let Some(combo) = KeyCombo::from_event(&text, shift, ctrl, alt, meta) {
                this.window.set_editor_keys(combo.label().into());
                this.window.set_editor_error(SharedString::new());
                *this.editor_combo.borrow_mut() = Some(combo);
            }
        });
        on!(on_editor_preset, |this, index| {
            if let Some((_, command, patina)) =
                (index as usize).checked_sub(1).and_then(|i| PRESETS.get(i))
            {
                this.window.set_editor_command((*command).into());
                this.window.set_editor_patina(*patina);
            }
        });
        on!(on_editor_save, |this| this.save_editor());
        on!(on_editor_cancel, |this| this.window.set_editor_open(false));
    }

    // ---- Pages ----------------------------------------------------------

    fn show_page(&self, page: Page) {
        self.page.set(page);
        self.refresh();
        if page == Page::KeyBindings {
            self.refresh_bindings();
        }
    }

    /// Rebuilds the visible rows from the schema and the current values.
    fn refresh(&self) {
        let query = self.query.borrow().to_lowercase();
        let searching = !query.is_empty();
        let page = self.page.get();
        self.window.set_searching(searching);
        self.window
            .set_page(Page::ALL.iter().position(|p| *p == page).unwrap_or(0) as i32);
        self.window.set_bindings_page(page == Page::KeyBindings);
        self.window.set_heading(
            if searching {
                "Search Results"
            } else {
                page.title()
            }
            .into(),
        );

        let prefs = self.app().prefs();
        let json = serde_json::to_value(&*prefs).unwrap_or(Value::Null);
        let items = schema::ITEMS.iter().filter(|item| {
            if !schema::available(item) {
                false
            } else if searching {
                [item.label, item.detail, item.section, item.page.title()]
                    .iter()
                    .any(|text| text.to_lowercase().contains(&query))
            } else {
                item.page == page
            }
        });

        let mut rows = Vec::new();
        let mut row_items = Vec::new();
        let mut section = String::new();
        for item in items {
            let heading = if searching {
                format!("{} › {}", item.page.title(), item.section)
            } else {
                item.section.to_owned()
            };
            if heading != section {
                rows.push(PrefRow {
                    kind: "heading".into(),
                    label: heading.to_uppercase().into(),
                    enabled: true,
                    ..Default::default()
                });
                row_items.push(None);
                section = heading;
            }
            rows.push(self.row(item, &json, &prefs));
            row_items.push(Some(item));
        }

        // Update in place when the layout is the same, so editing a value
        // doesn't reset the scroll position.
        let same = self.row_items.borrow().len() == row_items.len()
            && self
                .row_items
                .borrow()
                .iter()
                .zip(&row_items)
                .all(|(a, b)| a.map(|a| a.key) == b.map(|b| b.key));
        if same {
            for (index, row) in rows.into_iter().enumerate() {
                if self.rows.row_data(index).as_ref() != Some(&row) {
                    self.rows.set_row_data(index, row);
                }
            }
        } else {
            self.rows.set_vec(rows);
        }
        *self.row_items.borrow_mut() = row_items;
    }

    fn row(&self, item: &Item, json: &Value, prefs: &Preferences) -> PrefRow {
        let value = value_of(json, prefs, item.key);
        let enabled = item
            .requires
            .is_none_or(|key| json.get(key).and_then(Value::as_bool).unwrap_or(true));
        let mut row = PrefRow {
            key: item.key.into(),
            label: item.label.into(),
            detail: item.detail.into(),
            enabled,
            ..Default::default()
        };
        match item.control {
            Control::Switch => {
                row.kind = "switch".into();
                row.on = value.as_bool().unwrap_or(false);
            }
            Control::Number { unit, .. } => {
                row.kind = "number".into();
                row.number = value.as_f64().unwrap_or(0.0) as f32;
                row.unit = unit.into();
            }
            Control::Choice(choices) => {
                row.kind = "choice".into();
                let current = value.as_str().unwrap_or_default();
                row.choice = choices
                    .iter()
                    .position(|(choice, _)| *choice == current)
                    .map_or(-1, |index| index as i32);
                row.choices = strings(choices.iter().map(|(_, label)| *label));
            }
            Control::DynamicChoice => {
                row.kind = "choice".into();
                let current = value.as_str().unwrap_or_default().to_owned();
                let mut devices = self.app().audio_devices();
                if !devices.iter().any(|(name, _)| *name == current) {
                    devices.push((current.clone(), format!("{current} (not connected)")));
                }
                row.choice = devices
                    .iter()
                    .position(|(name, _)| *name == current)
                    .map_or(-1, |index| index as i32);
                row.choices = strings(devices.iter().map(|(_, description)| description.as_str()));
            }
            Control::Text | Control::Path(_) | Control::Lines => {
                row.kind = match item.control {
                    Control::Text => "text",
                    Control::Path(_) => "path",
                    _ => "lines",
                }
                .into();
                row.text = value.as_str().unwrap_or_default().into();
            }
            Control::Password => {
                row.kind = "password".into();
                if let Some(note) = &*self.password_note.borrow() {
                    row.detail = note.clone().into();
                }
            }
            Control::Button => {
                row.kind = "button".into();
                if let Some(note) = self.notes.borrow().get(item.key) {
                    row.detail = note.clone().into();
                } else if item.key == "clear-thumbnails" {
                    let size =
                        crate::thumbnails::cache_size(&self.app().paths().thumbnail_cache_dir());
                    row.detail = format!("Using {:.1} MB.", size as f64 / 1e6).into();
                }
            }
        }
        row
    }

    // ---- Editing --------------------------------------------------------

    fn item(key: &str) -> Option<&'static Item> {
        schema::ITEMS.iter().find(|item| item.key == key)
    }

    fn set(&self, key: &str, value: Value) {
        let app = self.app();
        let prefs = app.prefs();
        match with_value(&prefs, key, value) {
            Some(updated) => app.update_prefs(|current| *current = updated),
            None => log::warn!("could not set preference {key}"),
        }
        self.refresh();
    }

    fn set_number(&self, key: &str, value: f64) {
        let Some(Item {
            control: Control::Number { min, max, step, .. },
            ..
        }) = Self::item(key)
        else {
            return;
        };
        let value = (value.clamp(*min, *max) / step).round() * step;
        // Integer preferences must stay integers in JSON.
        let current = serde_json::to_value(&*self.app().prefs())
            .ok()
            .and_then(|json| json.get(key).cloned());
        let number = match current {
            Some(Value::Number(number)) if number.is_u64() || number.is_i64() => {
                Value::from(value.round() as i64)
            }
            _ => Value::from(value),
        };
        self.set(key, number);
    }

    fn set_choice(&self, key: &str, index: i32) {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        match Self::item(key).map(|item| item.control) {
            Some(Control::Choice(choices)) => {
                if let Some((value, _)) = choices.get(index) {
                    self.set(key, Value::from(*value));
                }
            }
            Some(Control::DynamicChoice) => {
                if let Some((name, _)) = self.app().audio_devices().get(index) {
                    self.set(key, Value::from(name.as_str()));
                }
            }
            _ => {}
        }
    }

    fn set_text(self: &Rc<Self>, key: &str, text: &str) {
        if key == "opensubtitles-password" {
            self.save_password(text);
            return;
        }
        let current = value_of(
            &serde_json::to_value(&*self.app().prefs()).unwrap_or(Value::Null),
            &self.app().prefs(),
            key,
        );
        if current.as_str() != Some(text) {
            self.set(key, Value::from(text));
        }
    }

    /// Saves the OpenSubtitles password in the system's store (or removes
    /// it, when empty), on a thread of its own: the store can block, or
    /// ask to be unlocked.
    fn save_password(self: &Rc<Self>, password: &str) {
        let username = self.app().prefs().opensubtitles_username.trim().to_owned();
        if username.is_empty() {
            *self.password_note.borrow_mut() =
                Some("Enter the account first, then its password.".into());
            self.refresh();
            return;
        }
        let password = password.to_owned();
        let (sender, receiver) = async_channel::bounded(1);
        let spawned = std::thread::Builder::new()
            .name("password".into())
            .spawn(move || {
                let service = crate::opensubtitles::PASSWORD_SERVICE;
                let result = if password.is_empty() {
                    crate::credentials::delete(service, &username).map(|()| false)
                } else {
                    crate::credentials::set(service, &username, &password).map(|()| true)
                };
                crate::opensubtitles::forget_shared();
                let _ = sender.send_blocking(result);
            });
        if let Err(error) = spawned {
            log::warn!("could not save the password: {error}");
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
            let store = crate::credentials::STORE_NAME;
            *this.password_note.borrow_mut() = Some(match result {
                Ok(true) => format!("Saved in {store}."),
                Ok(false) => format!("Removed from {store}."),
                Err(error) => format!("Could not save it in {store}: {error}"),
            });
            this.refresh();
        });
        if let Err(error) = spawned {
            log::warn!("{error}");
        }
    }

    fn browse(self: &Rc<Self>, key: &str) {
        let Some(Item {
            control: Control::Path(folder),
            label,
            ..
        }) = Self::item(key)
        else {
            return;
        };
        let mut dialog = Some(rfd::AsyncFileDialog::new().set_title(*label));
        {
            use slint::winit_030::WinitWindowAccessor;
            self.window.window().with_winit_window(|winit| {
                dialog = dialog.take().map(|dialog| dialog.set_parent(winit));
            });
        }
        let dialog = dialog.unwrap_or_default();
        let (folder, key, weak) = (*folder, key.to_owned(), Rc::downgrade(self));
        let spawned = slint::spawn_local(async move {
            let chosen = if folder {
                dialog.pick_folder().await
            } else {
                dialog.pick_file().await
            };
            if let (Some(chosen), Some(this)) = (chosen, weak.upgrade()) {
                this.set_text(&key, &chosen.path().to_string_lossy());
            }
        });
        if let Err(error) = spawned {
            log::warn!("could not show the dialog: {error}");
        }
    }

    fn run(self: &Rc<Self>, key: &str) {
        let app = self.app();
        let paths = app.paths().clone();
        let clear = |dir: std::path::PathBuf| {
            if let Err(error) = std::fs::remove_dir_all(&dir)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                log::warn!("could not clear {}: {error}", dir.display());
            }
        };
        match key {
            "clear-history" => app.clear_history(),
            "clear-resume" => clear(paths.watch_later_dir()),
            "clear-thumbnails" => clear(paths.thumbnail_cache_dir()),
            "open-config-folder" => {
                let _ = std::fs::create_dir_all(&paths.config);
                crate::platform::open_path(&paths.config);
            }
            "open-mpv-folder" => {
                let dir = paths.mpv_config_dir();
                let _ = std::fs::create_dir_all(dir.join("scripts"));
                crate::platform::open_path(&dir);
            }
            #[cfg(not(windows))]
            "files-video" => self.associate("files-video", || {
                crate::associations::set_default(crate::formats::Kind::Video)
            }),
            #[cfg(not(windows))]
            "files-audio" => self.associate("files-audio", || {
                crate::associations::set_default(crate::formats::Kind::Audio)
            }),
            #[cfg(not(windows))]
            "files-playlists" => self.associate("files-playlists", || {
                crate::associations::set_default(crate::formats::Kind::Playlist)
            }),
            #[cfg(windows)]
            "files-register" => self.associate("files-register", crate::associations::register),
            #[cfg(windows)]
            "files-defaults" => crate::associations::open_default_apps(),
            #[cfg(not(target_os = "macos"))]
            "files-remove" => self.associate("files-remove", crate::associations::remove),
            "restore-defaults" => {
                let keep = app.prefs().key_bindings.clone();
                app.update_prefs(|prefs| {
                    *prefs = Preferences {
                        key_bindings: keep,
                        ..Default::default()
                    }
                });
            }
            other => log::warn!("unknown preferences action {other}"),
        }
        self.refresh();
    }

    /// Runs a file type action on a thread, since it runs programs or
    /// writes the registry, and shows how it went under its button.
    fn associate(
        self: &Rc<Self>,
        key: &'static str,
        action: impl FnOnce() -> Result<String, String> + Send + 'static,
    ) {
        if self.associating.replace(true) {
            return;
        }
        let (sender, receiver) = async_channel::bounded(1);
        let spawned = std::thread::Builder::new()
            .name("file-types".into())
            .spawn(move || {
                let _ = sender.send_blocking(action());
            });
        if let Err(error) = spawned {
            log::warn!("could not register the file types: {error}");
            self.associating.set(false);
            return;
        }
        let weak = Rc::downgrade(self);
        let spawned = slint::spawn_local(async move {
            let result = receiver.recv().await;
            let Some(this) = weak.upgrade() else {
                return;
            };
            this.associating.set(false);
            let Ok(result) = result else {
                return;
            };
            let note = result.unwrap_or_else(|error| {
                log::warn!("{error}");
                error
            });
            this.notes.borrow_mut().insert(key, note);
            this.refresh();
        });
        if let Err(error) = spawned {
            log::warn!("{error}");
        }
    }

    // ---- Key bindings ---------------------------------------------------

    fn refresh_bindings(&self) {
        let app = self.app();
        let selected = app.prefs().key_bindings.clone();
        let sets = app.binding_sets();
        let mut names: Vec<SharedString> = vec!["Patina Default".into()];
        names.extend(sets.iter().map(SharedString::from));
        let index = if selected.is_empty() {
            0
        } else {
            sets.iter()
                .position(|set| *set == selected)
                .map_or(0, |index| index + 1)
        };
        self.window
            .set_binding_sets(ModelRc::new(VecModel::from(names)));
        self.window.set_binding_set(index as i32);
        self.window.set_binding_builtin(index == 0);

        let filter = self.binding_filter.borrow().clone();
        let bindings = app.bindings();
        let mut keys = Vec::new();
        let rows: Vec<BindingRow> = bindings
            .iter()
            .filter(|(combo, binding)| {
                filter.is_empty()
                    || combo.label().to_lowercase().contains(&filter)
                    || describe(binding).to_lowercase().contains(&filter)
                    || binding.conf_command().to_lowercase().contains(&filter)
            })
            .map(|(combo, binding)| {
                keys.push(combo.clone());
                BindingRow {
                    keys: combo.label().into(),
                    action: describe(binding).into(),
                    patina: matches!(binding, Binding::Patina(_)),
                }
            })
            .collect();
        self.window.set_bindings(ModelRc::new(VecModel::from(rows)));
        *self.binding_keys.borrow_mut() = keys;
    }

    fn choose_set(&self, index: i32) {
        let app = self.app();
        let name = match usize::try_from(index) {
            Ok(0) | Err(_) => String::new(),
            Ok(index) => app
                .binding_sets()
                .get(index - 1)
                .cloned()
                .unwrap_or_default(),
        };
        app.update_prefs(|prefs| prefs.key_bindings = name);
        self.refresh_bindings();
    }

    /// A set name not taken yet: "My Bindings", "My Bindings 2", ...
    fn new_set_name(&self) -> String {
        let sets = self.app().binding_sets();
        (1..)
            .map(|n| {
                if n == 1 {
                    USER_SET.to_owned()
                } else {
                    format!("{USER_SET} {n}")
                }
            })
            .find(|name| !sets.contains(name))
            .expect("some name is free")
    }

    fn duplicate_set(&self) {
        let name = self.new_set_name();
        let bindings = self.app().bindings();
        if let Err(error) = self.app().save_binding_set(&name, &bindings) {
            log::warn!("could not save the key bindings: {error}");
        }
        self.refresh_bindings();
    }

    /// Edits the active set, copying the built-in one first if needed.
    fn change_bindings(&self, change: impl FnOnce(&mut Bindings)) {
        let app = self.app();
        let mut bindings = (*app.bindings()).clone();
        change(&mut bindings);
        let current = app.prefs().key_bindings.clone();
        let name = if current.is_empty() {
            self.new_set_name()
        } else {
            current
        };
        if let Err(error) = app.save_binding_set(&name, &bindings) {
            log::warn!("could not save the key bindings: {error}");
        }
        self.refresh_bindings();
    }

    fn open_editor(&self, combo: Option<KeyCombo>) {
        let binding = combo
            .as_ref()
            .and_then(|combo| self.app().bindings().get(combo).cloned());
        self.window.set_editor_title(
            if combo.is_some() {
                "Edit Key Binding"
            } else {
                "Add Key Binding"
            }
            .into(),
        );
        self.window.set_editor_keys(
            combo
                .as_ref()
                .map(KeyCombo::label)
                .unwrap_or_default()
                .into(),
        );
        self.window.set_editor_command(
            binding
                .as_ref()
                .map(Binding::conf_command)
                .unwrap_or_default()
                .into(),
        );
        self.window
            .set_editor_patina(matches!(binding, Some(Binding::Patina(_))));
        self.window.set_editor_error(SharedString::new());
        *self.editor_combo.borrow_mut() = combo.clone();
        *self.editing.borrow_mut() = combo;
        self.window.set_editor_open(true);
    }

    fn save_editor(&self) {
        let Some(combo) = self.editor_combo.borrow().clone() else {
            self.window
                .set_editor_error("Press the key to bind first.".into());
            return;
        };
        let command = self.window.get_editor_command();
        let Some(binding) = Binding::from_command(&command, self.window.get_editor_patina()) else {
            self.window
                .set_editor_error("That command can't be read; check its quotes.".into());
            return;
        };
        let editing = self.editing.borrow().clone();
        let bindings = self.app().bindings();
        if editing.as_ref() != Some(&combo)
            && let Some(existing) = bindings.get(&combo)
        {
            let message = format!(
                "{} already runs “{}”. Press another key.",
                combo.label(),
                describe(existing)
            );
            self.window.set_editor_error(message.into());
            return;
        }
        self.change_bindings(|bindings| match &editing {
            Some(old) => bindings.rebind(old, combo, binding),
            None => bindings.set(combo, binding),
        });
        self.window.set_editor_open(false);
    }
}

fn strings<'a>(values: impl Iterator<Item = &'a str>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        values.map(SharedString::from).collect::<Vec<_>>(),
    ))
}

/// The JSON value behind a key, including the settings stored inside lists.
fn value_of(json: &Value, prefs: &Preferences, key: &str) -> Value {
    if let Some(button) = key.strip_prefix("toolbar.") {
        return Value::Bool(prefs.toolbar_buttons.iter().any(|b| b == button));
    }
    if key == "mpv-options" {
        let lines: Vec<String> = prefs
            .mpv_options
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        return Value::from(lines.join("\n"));
    }
    json.get(key).cloned().unwrap_or(Value::Null)
}

/// `prefs` with `key` set to `value`, or None if the value doesn't fit.
fn with_value(prefs: &Preferences, key: &str, value: Value) -> Option<Preferences> {
    let mut prefs = prefs.clone();
    if let Some(button) = key.strip_prefix("toolbar.") {
        let on = value.as_bool()?;
        let mut buttons: Vec<String> = prefs
            .toolbar_buttons
            .into_iter()
            .filter(|b| b != button)
            .collect();
        if on {
            buttons.push(button.to_owned());
        }
        // Keep IINA's order rather than the order they were switched on.
        buttons.sort_by_key(|b| schema::TOOLBAR.iter().position(|t| t == b));
        prefs.toolbar_buttons = buttons;
        return Some(prefs);
    }
    if key == "mpv-options" {
        prefs.mpv_options = value
            .as_str()?
            .lines()
            .filter_map(|line| {
                let line = line.trim().trim_start_matches("--");
                let (name, value) = line.split_once('=').unwrap_or((line, "yes"));
                (!name.is_empty()).then(|| (name.trim().to_owned(), value.trim().to_owned()))
            })
            .collect();
        return Some(prefs);
    }
    let mut json = serde_json::to_value(&prefs).ok()?;
    let object = json.as_object_mut()?;
    if !object.contains_key(key) {
        return None;
    }
    object.insert(key.to_owned(), value);
    serde_json::from_value(json).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_set_by_key() {
        let prefs = Preferences::default();
        let changed = with_value(&prefs, "osd-font-size", Value::from(30.0)).unwrap();
        assert_eq!(changed.osd_font_size, 30.0);
        let changed = with_value(&prefs, "hwdec", Value::from("no")).unwrap();
        assert_eq!(changed.hwdec, "no");
        // Wrong types and unknown keys are refused.
        assert!(with_value(&prefs, "osd-font-size", Value::from("big")).is_none());
        assert!(with_value(&prefs, "no-such-setting", Value::Bool(true)).is_none());
    }

    #[test]
    fn toolbar_buttons_keep_their_order() {
        let prefs = Preferences::default();
        let changed = with_value(&prefs, "toolbar.fullscreen", Value::Bool(true)).unwrap();
        let changed = with_value(&changed, "toolbar.pip", Value::Bool(false)).unwrap();
        assert_eq!(
            changed.toolbar_buttons,
            ["playlist", "settings", "fullscreen"]
        );
        let json = serde_json::to_value(&changed).unwrap();
        assert_eq!(value_of(&json, &changed, "toolbar.pip"), Value::Bool(false));
    }

    #[test]
    fn mpv_options_are_edited_as_lines() {
        let prefs = Preferences::default();
        let changed = with_value(
            &prefs,
            "mpv-options",
            Value::from("--hwdec=no\n\nprofile = fast\nfs"),
        )
        .unwrap();
        assert_eq!(
            changed.mpv_options,
            [
                ("hwdec".to_owned(), "no".to_owned()),
                ("profile".to_owned(), "fast".to_owned()),
                ("fs".to_owned(), "yes".to_owned())
            ]
        );
        let json = serde_json::to_value(&changed).unwrap();
        assert_eq!(
            value_of(&json, &changed, "mpv-options"),
            Value::from("hwdec=no\nprofile=fast\nfs=yes")
        );
    }

    #[test]
    fn presets_describe_bindings() {
        assert_eq!(
            describe(&Binding::Mpv(vec![vec!["cycle".into(), "pause".into()]])),
            "Play / pause"
        );
        assert_eq!(
            describe(&Binding::Patina("open-file".into())),
            "Open a file"
        );
        assert_eq!(
            describe(&Binding::Mpv(vec![vec!["seek".into(), "7".into()]])),
            "seek 7"
        );
    }
}
