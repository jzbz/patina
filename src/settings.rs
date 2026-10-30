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
        // Each platform's own convention, after patina.video: a reverse-DNS
        // name on macOS (video.patina.Patina), the application's name on
        // Windows (%APPDATA%\Patina), lower case on Linux (~/.config/patina).
        let name = if cfg!(target_os = "macos") {
            crate::APP_ID
        } else if cfg!(windows) {
            "Patina"
        } else {
            "patina"
        };
        let dirs = directories::ProjectDirs::from_path(PathBuf::from(name));
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

    /// User key binding sets, one `.conf` file each.
    pub fn input_conf_dir(&self) -> PathBuf {
        self.config.join("input_conf")
    }

    /// mpv's own configuration directory: `mpv.conf`, `scripts/`, `fonts/`.
    pub fn mpv_config_dir(&self) -> PathBuf {
        self.config.join("mpv")
    }

    pub fn watch_later_dir(&self) -> PathBuf {
        self.data.join("watch-later")
    }

    pub fn thumbnail_cache_dir(&self) -> PathBuf {
        self.data.join("thumbnails")
    }

    /// Subtitles downloaded from OpenSubtitles.
    pub fn online_subtitles_dir(&self) -> PathBuf {
        self.data.join("online-subtitles")
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

/// User preferences. Defaults follow IINA's. Every field is a plain JSON
/// value so the preferences window can edit any of them by key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Preferences {
    // General.
    /// "welcome" or "open-dialog".
    pub launch_action: String,
    pub open_in_new_window: bool,
    pub pause_when_open: bool,
    pub fullscreen_when_open: bool,
    pub pause_when_minimized: bool,
    pub keep_open: bool,
    pub auto_play_next: bool,
    pub playlist_auto_add: bool,
    /// Show audio files by their title and artist tags in the playlist.
    pub playlist_show_metadata: bool,
    /// ...in music mode only.
    pub playlist_show_metadata_in_music_mode: bool,
    pub resume_playback: bool,
    pub record_history: bool,
    pub recent_limit: usize,
    pub prevent_screensaver: bool,
    /// Keep only the computer awake for audio and music mode, letting the
    /// screen sleep.
    pub allow_screensaver_for_audio: bool,
    /// Enter music mode for audio files, and leave it for video.
    pub auto_switch_to_music_mode: bool,
    /// Music mode shows its playlist drawer, and the cover.
    pub music_mode_show_playlist: bool,
    pub music_mode_show_album_art: bool,
    /// Empty for the platform's Pictures folder.
    pub screenshot_dir: String,
    pub screenshot_format: String,
    pub screenshot_subtitles: bool,

    // Interface.
    /// "auto", "custom" (Patina's title bar) or "system".
    pub titlebar: String,
    pub resize_window: ResizeWindow,
    /// "fit", "0.5", "1", "1.5" or "2".
    pub window_size: String,
    pub always_on_top: bool,
    pub osc_timeout: f32,
    pub toolbar_buttons: Vec<String>,
    pub arrow_action: String,
    pub show_chapter_marks: bool,
    pub show_remaining_time: bool,
    pub osd_enabled: bool,
    pub osd_timeout: f32,
    pub osd_font_size: f32,
    pub thumbnails: bool,
    /// Width in logical pixels; generated at twice that for HiDPI screens.
    pub thumbnail_width: u32,
    pub thumbnail_cache_mb: u64,

    // Video.
    /// mpv `hwdec`: "no", "auto-safe" or "auto-copy-safe".
    pub hwdec: String,
    pub deinterlace: String,
    pub tone_mapping: String,
    /// Peak brightness in nits for tone mapping; 0 for automatic.
    pub target_peak: f64,

    // Audio.
    /// The volume when Patina last closed.
    pub volume: f64,
    pub use_initial_volume: bool,
    pub initial_volume: f64,
    pub max_volume: f64,
    pub audio_device: String,
    pub audio_language: String,
    pub replaygain: String,
    pub replaygain_preamp: f64,
    pub replaygain_clip: bool,
    pub gapless_audio: String,

    // Subtitles.
    pub sub_auto: String,
    pub sub_language: String,
    /// The user's own OpenSubtitles API key, and their account (whose
    /// password is in the system's store, not here).
    pub opensubtitles_api_key: String,
    pub opensubtitles_username: String,
    /// Search online when a long enough local video opens without subtitles.
    pub online_sub_auto: bool,
    pub sub_codepage: String,
    pub sub_font: String,
    pub sub_font_size: f64,
    pub sub_bold: bool,
    pub sub_italic: bool,
    pub sub_color: String,
    pub sub_border_color: String,
    pub sub_border_size: f64,
    pub sub_shadow_offset: f64,
    pub sub_back_color: String,
    pub sub_pos: f64,
    pub sub_scale_by_window: bool,
    pub sub_use_margins: bool,
    pub sub_ass_override: String,

    // Network.
    pub cache: bool,
    pub cache_secs: f64,
    pub demuxer_max_mb: f64,
    pub cache_pause_initial: bool,
    pub cache_pause_wait: f64,
    pub user_agent: String,
    pub http_proxy: String,
    pub rtsp_transport: String,
    pub ytdl: bool,
    pub ytdl_path: String,
    pub ytdl_raw_options: String,
    /// "best", or the tallest video to choose: "2160", "1080", "720"...
    pub ytdl_quality: String,
    /// A yt-dlp format (`-f`) that takes the place of `ytdl_quality`.
    pub ytdl_format: String,
    pub download_dir: String,

    // Control.
    pub single_click: String,
    pub double_click: String,
    pub right_click: String,
    pub middle_click: String,
    pub vertical_scroll: String,
    pub horizontal_scroll: String,
    pub seek_step: f64,
    pub volume_step: f64,
    /// "keyframes" or "exact", for relative seeks.
    pub seek_mode: String,
    /// Media keys and the desktop's now-playing controls.
    pub media_keys: bool,

    // Key bindings: a set in the input_conf folder, or empty for the default.
    pub key_bindings: String,

    // Advanced.
    pub use_mpv_config: bool,
    pub mpv_options: Vec<(String, String)>,

    // Inspector.
    /// mpv properties shown in the inspector's Watch tab.
    pub inspector_watch: Vec<String>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            launch_action: "welcome".into(),
            open_in_new_window: true,
            pause_when_open: false,
            fullscreen_when_open: false,
            pause_when_minimized: false,
            keep_open: true,
            auto_play_next: true,
            playlist_auto_add: true,
            playlist_show_metadata: true,
            playlist_show_metadata_in_music_mode: true,
            resume_playback: true,
            record_history: true,
            recent_limit: 12,
            prevent_screensaver: true,
            allow_screensaver_for_audio: false,
            auto_switch_to_music_mode: true,
            music_mode_show_playlist: false,
            music_mode_show_album_art: true,
            screenshot_dir: String::new(),
            screenshot_format: "png".into(),
            screenshot_subtitles: true,
            titlebar: "auto".into(),
            resize_window: ResizeWindow::OnOpen,
            window_size: "1".into(),
            always_on_top: false,
            osc_timeout: 2.5,
            toolbar_buttons: vec!["pip".into(), "playlist".into(), "settings".into()],
            arrow_action: "speed".into(),
            show_chapter_marks: true,
            show_remaining_time: false,
            osd_enabled: true,
            osd_timeout: 1.0,
            osd_font_size: 20.0,
            thumbnails: true,
            thumbnail_width: 120,
            thumbnail_cache_mb: 500,
            hwdec: "auto-safe".into(),
            deinterlace: "no".into(),
            tone_mapping: "auto".into(),
            target_peak: 0.0,
            volume: 100.0,
            use_initial_volume: false,
            initial_volume: 100.0,
            max_volume: 100.0,
            audio_device: "auto".into(),
            audio_language: String::new(),
            replaygain: "no".into(),
            replaygain_preamp: 0.0,
            replaygain_clip: false,
            gapless_audio: "weak".into(),
            sub_auto: "smart".into(),
            sub_language: String::new(),
            opensubtitles_api_key: String::new(),
            opensubtitles_username: String::new(),
            online_sub_auto: false,
            sub_codepage: "auto".into(),
            sub_font: "sans-serif".into(),
            sub_font_size: 55.0,
            sub_bold: false,
            sub_italic: false,
            sub_color: "#FFFFFF".into(),
            sub_border_color: "#000000".into(),
            sub_border_size: 3.0,
            sub_shadow_offset: 0.0,
            sub_back_color: "#00000000".into(),
            sub_pos: 100.0,
            sub_scale_by_window: true,
            sub_use_margins: true,
            sub_ass_override: "scale".into(),
            cache: true,
            cache_secs: 36000.0,
            demuxer_max_mb: 150.0,
            cache_pause_initial: false,
            cache_pause_wait: 1.0,
            user_agent: String::new(),
            http_proxy: String::new(),
            rtsp_transport: "tcp".into(),
            ytdl: true,
            ytdl_path: String::new(),
            ytdl_raw_options: String::new(),
            ytdl_quality: "best".into(),
            ytdl_format: String::new(),
            download_dir: String::new(),
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
            seek_step: 5.0,
            volume_step: 2.0,
            seek_mode: "keyframes".into(),
            media_keys: true,
            key_bindings: String::new(),
            use_mpv_config: true,
            mpv_options: Vec::new(),
            inspector_watch: Vec::new(),
        }
    }
}

fn yes_no(value: bool) -> String {
    if value { "yes" } else { "no" }.into()
}

impl Preferences {
    /// Patina's title bar over the video beside macOS's own window buttons,
    /// in a transparent title bar: the default on macOS, as in IINA.
    pub fn transparent_titlebar(&self) -> bool {
        cfg!(target_os = "macos") && self.titlebar == "auto"
    }

    pub fn custom_titlebar(&self) -> bool {
        match self.titlebar.as_str() {
            "custom" => true,
            "system" => false,
            _ => !cfg!(target_os = "macos"),
        }
    }

    pub fn screenshot_dir(&self) -> PathBuf {
        if !self.screenshot_dir.is_empty() {
            return PathBuf::from(&self.screenshot_dir);
        }
        directories::UserDirs::new()
            .and_then(|dirs| dirs.picture_dir().map(Path::to_path_buf))
            .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()))
            .unwrap_or_else(std::env::temp_dir)
            .join("Patina")
    }

    /// The volume a new window starts at.
    pub fn start_volume(&self) -> f64 {
        let volume = if self.use_initial_volume {
            self.initial_volume
        } else {
            self.volume
        };
        volume.clamp(0.0, self.max_volume.max(100.0))
    }

    /// Where downloaded videos go.
    pub fn download_dir(&self) -> PathBuf {
        if !self.download_dir.is_empty() {
            return PathBuf::from(&self.download_dir);
        }
        directories::UserDirs::new()
            .and_then(|dirs| dirs.download_dir().map(Path::to_path_buf))
            .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()))
            .unwrap_or_else(std::env::temp_dir)
    }

    /// The yt-dlp format filter for the video quality preference, such as
    /// "[height<=1080]" (empty for the best).
    pub fn ytdl_video_filter(&self) -> String {
        match self.ytdl_quality.parse::<u32>() {
            Ok(height) => format!("[height<={height}]"),
            Err(_) => String::new(),
        }
    }

    /// mpv's `ytdl-format`: empty leaves the choice to yt-dlp, whose own
    /// default is the best video with the best audio.
    pub fn ytdl_format(&self) -> String {
        if !self.ytdl_format.trim().is_empty() {
            return self.ytdl_format.trim().to_owned();
        }
        let filter = self.ytdl_video_filter();
        if filter.is_empty() {
            return String::new();
        }
        format!("bestvideo{filter}+bestaudio/best{filter}/best")
    }

    /// The mpv options these preferences set, in the order they apply. They
    /// are set before a core starts and again on running cores when the
    /// preferences change, so each must also work as a runtime property.
    pub fn mpv_options(&self) -> Vec<(&'static str, String)> {
        let mut options = vec![
            ("hwdec", self.hwdec.clone()),
            ("deinterlace", self.deinterlace.clone()),
            ("tone-mapping", self.tone_mapping.clone()),
            (
                "target-peak",
                if self.target_peak > 0.0 {
                    format!("{}", self.target_peak)
                } else {
                    "auto".into()
                },
            ),
            ("volume-max", format!("{}", self.max_volume.max(100.0))),
            ("audio-device", self.audio_device.clone()),
            ("alang", self.audio_language.clone()),
            ("replaygain", self.replaygain.clone()),
            ("replaygain-preamp", format!("{}", self.replaygain_preamp)),
            ("replaygain-clip", yes_no(self.replaygain_clip)),
            ("gapless-audio", self.gapless_audio.clone()),
            // Patina's own matching adds subtitle files itself.
            (
                "sub-auto",
                if self.sub_auto == "smart" {
                    "no".into()
                } else {
                    self.sub_auto.clone()
                },
            ),
            ("slang", self.sub_language.clone()),
            ("sub-codepage", self.sub_codepage.clone()),
            ("sub-font", self.sub_font.clone()),
            ("sub-font-size", format!("{}", self.sub_font_size)),
            ("sub-bold", yes_no(self.sub_bold)),
            ("sub-italic", yes_no(self.sub_italic)),
            ("sub-color", self.sub_color.clone()),
            ("sub-outline-color", self.sub_border_color.clone()),
            ("sub-outline-size", format!("{}", self.sub_border_size)),
            ("sub-shadow-offset", format!("{}", self.sub_shadow_offset)),
            ("sub-back-color", self.sub_back_color.clone()),
            ("sub-pos", format!("{}", self.sub_pos)),
            ("sub-scale-by-window", yes_no(self.sub_scale_by_window)),
            ("sub-use-margins", yes_no(self.sub_use_margins)),
            ("sub-ass-force-margins", yes_no(self.sub_use_margins)),
            ("sub-ass-override", self.sub_ass_override.clone()),
            ("cache", if self.cache { "auto" } else { "no" }.into()),
            ("cache-secs", format!("{}", self.cache_secs)),
            (
                "demuxer-max-bytes",
                format!("{}MiB", self.demuxer_max_mb.max(1.0)),
            ),
            ("cache-pause-initial", yes_no(self.cache_pause_initial)),
            ("cache-pause-wait", format!("{}", self.cache_pause_wait)),
            ("http-proxy", self.http_proxy.clone()),
            ("rtsp-transport", self.rtsp_transport.clone()),
            ("ytdl", yes_no(self.ytdl)),
            ("ytdl-raw-options", self.ytdl_raw_options.clone()),
            ("ytdl-format", self.ytdl_format()),
            (
                "keep-open",
                if !self.auto_play_next {
                    "always"
                } else if self.keep_open {
                    "yes"
                } else {
                    "no"
                }
                .into(),
            ),
            (
                "autocreate-playlist",
                if self.playlist_auto_add { "same" } else { "no" }.into(),
            ),
            ("screenshot-format", self.screenshot_format.clone()),
            (
                "screenshot-directory",
                self.screenshot_dir().to_string_lossy().into_owned(),
            ),
            ("save-position-on-quit", yes_no(self.resume_playback)),
            ("resume-playback", yes_no(self.resume_playback)),
        ];
        if !self.user_agent.is_empty() {
            options.push(("user-agent", self.user_agent.clone()));
        }
        if !self.ytdl_path.is_empty() {
            options.push((
                "script-opts",
                format!("ytdl_hook-ytdl_path={}", self.ytdl_path),
            ));
        }
        options
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

/// Loads the preferences, keeping every setting that can still be read
/// when others are invalid (say, after a setting changed type).
pub fn load_preferences(path: &Path) -> Preferences {
    let value: serde_json::Value = match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(error) => {
                log::warn!("ignoring unreadable {}: {error}", path.display());
                return Preferences::default();
            }
        },
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!("could not read {}: {error}", path.display());
            }
            return Preferences::default();
        }
    };
    if let Ok(prefs) = serde_json::from_value(value.clone()) {
        return prefs;
    }
    let mut prefs = Preferences::default();
    let Some(entries) = value.as_object() else {
        return prefs;
    };
    for (key, entry) in entries {
        let mut merged = serde_json::to_value(&prefs).unwrap_or_default();
        if let Some(object) = merged.as_object_mut() {
            object.insert(key.clone(), entry.clone());
        }
        match serde_json::from_value(merged) {
            Ok(updated) => prefs = updated,
            Err(_) => log::warn!("ignoring the setting {key} = {entry} in {}", path.display()),
        }
    }
    prefs
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
    fn every_option_is_accepted_by_mpv() {
        let Some(mpv) = crate::mpv::testing::headless() else {
            return;
        };
        let prefs = Preferences {
            user_agent: "Patina".into(),
            ytdl_path: "/usr/bin/yt-dlp".into(),
            ..Default::default()
        };
        for (name, value) in prefs.mpv_options() {
            if let Err(error) = mpv.set_property(name, value.as_str()) {
                panic!("{name}={value}: {error}");
            }
        }
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
    fn unreadable_settings_keep_the_readable_ones() {
        let dir = std::env::temp_dir().join(format!("patina-lenient-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{"volume": 40, "screenshot-dir": null, "osd-timeout": 3}"#,
        )
        .unwrap();
        let prefs = load_preferences(&path);
        assert_eq!(prefs.volume, 40.0);
        assert_eq!(prefs.osd_timeout, 3.0);
        assert_eq!(prefs.screenshot_dir, "");
        std::fs::remove_dir_all(&dir).unwrap();
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
