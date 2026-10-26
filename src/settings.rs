//! Preferences, playback history and the directories they live in.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Where Patina keeps its files. `PATINA_CONFIG_DIR` and `PATINA_DATA_DIR`
/// override the platform locations, which is useful for testing.
#[derive(Debug, Clone)]
pub struct Paths {
    pub config: PathBuf,
    pub data: PathBuf,
}

impl Paths {
    pub fn discover() -> Self {
        let dirs = directories::ProjectDirs::from("io.github", "jzbz", "Patina");
        let config = std::env::var_os("PATINA_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs.as_ref().map(|dirs| dirs.config_dir().to_path_buf()))
            .unwrap_or_else(|| std::env::temp_dir().join("patina"));
        let data = std::env::var_os("PATINA_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs.as_ref().map(|dirs| dirs.data_dir().to_path_buf()))
            .unwrap_or_else(|| config.join("data"));
        Self { config, data }
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.json")
    }

    pub fn input_conf(&self) -> PathBuf {
        self.config.join("input.conf")
    }

    /// mpv's own configuration directory: `mpv.conf`, `scripts/`, `fonts/`.
    pub fn mpv_config_dir(&self) -> PathBuf {
        self.config.join("mpv")
    }

    pub fn watch_later_dir(&self) -> PathBuf {
        self.data.join("watch-later")
    }

    pub fn history_file(&self) -> PathBuf {
        self.data.join("history.json")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResizeWindow {
    /// Fit the window to every new video.
    Always,
    /// Only when the window opens with its first video.
    OnOpen,
    Never,
}

/// User preferences. Defaults follow IINA's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Preferences {
    // General.
    pub open_in_new_window: bool,
    pub playlist_auto_add: bool,
    pub resume_playback: bool,
    pub record_history: bool,
    pub recent_limit: usize,
    pub pause_when_minimized: bool,

    // Interface.
    pub osc_timeout: f32,
    pub toolbar_buttons: Vec<String>,
    pub arrow_action: String,
    pub show_chapter_marks: bool,
    pub show_remaining_time: bool,
    pub osd_enabled: bool,
    pub osd_timeout: f32,
    pub osd_font_size: f32,
    pub resize_window: ResizeWindow,
    pub always_on_top: bool,
    /// Draw Patina's own title bar; `None` picks the platform default.
    pub custom_titlebar: Option<bool>,

    // Playback.
    pub hardware_decoding: bool,
    pub volume: f64,
    pub max_volume: f64,
    pub seek_step: f64,
    pub volume_step: f64,

    // Mouse.
    pub single_click: String,
    pub double_click: String,
    pub right_click: String,
    pub middle_click: String,
    pub vertical_scroll: String,
    pub horizontal_scroll: String,

    // Screenshots.
    pub screenshot_dir: Option<PathBuf>,
    pub screenshot_format: String,

    // Network.
    pub ytdl_path: Option<String>,

    // Advanced.
    pub mpv_options: Vec<(String, String)>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            open_in_new_window: true,
            playlist_auto_add: true,
            resume_playback: true,
            record_history: true,
            recent_limit: 12,
            pause_when_minimized: false,
            osc_timeout: 2.5,
            toolbar_buttons: vec!["pip".into(), "playlist".into(), "settings".into()],
            arrow_action: "speed".into(),
            show_chapter_marks: true,
            show_remaining_time: false,
            osd_enabled: true,
            osd_timeout: 1.0,
            osd_font_size: 20.0,
            resize_window: ResizeWindow::OnOpen,
            always_on_top: false,
            custom_titlebar: None,
            hardware_decoding: true,
            volume: 100.0,
            max_volume: 100.0,
            seek_step: 5.0,
            volume_step: 2.0,
            single_click: "toggle-osc".into(),
            double_click: "fullscreen".into(),
            // IINA pauses on right click; without a global menu bar, Linux
            // and Windows use it for the application menu instead.
            right_click: if cfg!(target_os = "macos") {
                "pause"
            } else {
                "context-menu"
            }
            .into(),
            middle_click: "none".into(),
            vertical_scroll: "volume".into(),
            horizontal_scroll: "seek".into(),
            screenshot_dir: None,
            screenshot_format: "png".into(),
            ytdl_path: None,
            mpv_options: Vec::new(),
        }
    }
}

impl Preferences {
    pub fn custom_titlebar(&self) -> bool {
        self.custom_titlebar.unwrap_or(!cfg!(target_os = "macos"))
    }

    pub fn screenshot_dir(&self) -> PathBuf {
        self.screenshot_dir.clone().unwrap_or_else(|| {
            directories::UserDirs::new()
                .and_then(|dirs| dirs.picture_dir().map(Path::to_path_buf))
                .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()))
                .unwrap_or_else(std::env::temp_dir)
                .join("Patina")
        })
    }
}

/// One played file, newest first in the history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub path: String,
    pub title: String,
    /// Seconds since the Unix epoch.
    pub played_at: u64,
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub position: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct History {
    pub entries: Vec<HistoryEntry>,
}

const HISTORY_LIMIT: usize = 1000;

impl History {
    /// Moves `path` to the front, keeping one entry per file.
    pub fn record(&mut self, entry: HistoryEntry) {
        self.entries.retain(|existing| existing.path != entry.path);
        self.entries.insert(0, entry);
        self.entries.truncate(HISTORY_LIMIT);
    }

    pub fn update_progress(&mut self, path: &str, position: f64, duration: f64) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.path == path) {
            entry.position = position;
            if duration > 0.0 {
                entry.duration = duration;
            }
        }
    }

    pub fn recent(&self, limit: usize) -> impl Iterator<Item = &HistoryEntry> {
        self.entries.iter().take(limit)
    }
}

pub fn load_json<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> T {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            log::warn!("ignoring unreadable {}: {error}", path.display());
            T::default()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => T::default(),
        Err(error) => {
            log::warn!("could not read {}: {error}", path.display());
            T::default()
        }
    }
}

/// Writes JSON atomically: to a temporary file, then renamed into place.
pub fn save_json<T: Serialize>(path: &Path, value: &T) {
    let result = (|| -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, bytes)?;
        std::fs::rename(&temporary, path)
    })();
    if let Err(error) = result {
        log::warn!("could not save {}: {error}", path.display());
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_take_defaults() {
        let prefs: Preferences =
            serde_json::from_str(r#"{"volume": 60, "arrow-action": "seek"}"#).unwrap();
        assert_eq!(prefs.volume, 60.0);
        assert_eq!(prefs.arrow_action, "seek");
        assert_eq!(prefs.osc_timeout, 2.5);
        assert!(prefs.resume_playback);
    }

    #[test]
    fn history_keeps_one_entry_per_file_newest_first() {
        let mut history = History::default();
        let entry = |path: &str, played_at| HistoryEntry {
            path: path.into(),
            title: path.into(),
            played_at,
            duration: 0.0,
            position: 0.0,
        };
        history.record(entry("a", 1));
        history.record(entry("b", 2));
        history.record(entry("a", 3));
        let paths: Vec<_> = history
            .recent(10)
            .map(|entry| entry.path.as_str())
            .collect();
        assert_eq!(paths, ["a", "b"]);
        history.update_progress("b", 30.0, 60.0);
        assert_eq!(history.entries[1].position, 30.0);
    }

    #[test]
    fn json_round_trips_through_a_file() {
        let dir = std::env::temp_dir().join(format!("patina-settings-test-{}", std::process::id()));
        let path = dir.join("settings.json");
        let prefs = Preferences {
            volume: 42.0,
            ..Default::default()
        };
        save_json(&path, &prefs);
        let loaded: Preferences = load_json(&path);
        assert_eq!(loaded, prefs);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
