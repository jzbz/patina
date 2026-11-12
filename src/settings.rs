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

    /// The YouTube section's lists: subscriptions, Watch Later and marks.
    pub fn youtube_file(&self) -> PathBuf {
        self.data.join("youtube.json")
    }

    /// The YouTube section's saved feed, which can always be fetched again.
    pub fn youtube_feed_file(&self) -> PathBuf {
        self.data.join("youtube-feed.json")
    }

    /// The versions the package's yt-dlp and the user's said, by file, so that
    /// each is asked once (`ytdl_versions`). Only a macOS or Windows package
    /// writes it.
    pub fn ytdl_versions_file(&self) -> PathBuf {
        self.data.join("yt-dlp-versions.json")
    }

    /// Thumbnails and channel pictures. Apart from Jellyfin's, which signing out
    /// of a server empties.
    pub fn youtube_images_dir(&self) -> PathBuf {
        self.data.join("youtube-images")
    }

    /// Posters and backdrops from Jellyfin servers.
    pub fn jellyfin_images_dir(&self) -> PathBuf {
        self.data.join("jellyfin").join("images")
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

/// User preferences. Defaults follow IINA's, except where a comment at one
/// says why. Every field is a plain JSON value so the preferences window can
/// edit any of them by key.
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
    /// What the YouTube section plays and downloads: "screen" for up to the
    /// screen's resolution, "best", or the tallest video to choose.
    pub youtube_quality: String,
    pub download_dir: String,

    // Jellyfin.
    /// What a playing intro, credits, recap, preview or commercial the
    /// server marks brings: "ask" (a Skip button), "skip" or "none".
    pub jellyfin_skip_intro: String,
    pub jellyfin_skip_outro: String,
    pub jellyfin_skip_recap: String,
    pub jellyfin_skip_preview: String,
    pub jellyfin_skip_commercial: String,
    /// Have the server transcode what mpv could play but this computer
    /// may not decode well.
    pub jellyfin_transcode_hevc: bool,
    pub jellyfin_transcode_av1: bool,
    pub jellyfin_transcode_hdr: bool,
    /// Anything wider than 1920 or taller than 1080.
    pub jellyfin_transcode_over_1080p: bool,
    /// The most the servers' posters and backdrops may take on disk.
    pub jellyfin_image_cache_mb: u64,

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

/// The number of changed defaults `Preferences::move_old_defaults` knows.
const MOVED_DEFAULTS: u32 = 1;

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
            youtube_quality: "screen".into(),
            download_dir: String::new(),
            jellyfin_skip_intro: "ask".into(),
            jellyfin_skip_outro: "ask".into(),
            jellyfin_skip_recap: "ask".into(),
            jellyfin_skip_preview: "ask".into(),
            jellyfin_skip_commercial: "none".into(),
            jellyfin_transcode_hevc: false,
            jellyfin_transcode_av1: false,
            jellyfin_transcode_hdr: false,
            jellyfin_transcode_over_1080p: false,
            jellyfin_image_cache_mb: 256,
            // IINA shows or hides the controller, which the pointer does
            // anyway; a click pauses, as in YouTube's player and most
            // others, and at once (`run-mouse-action` in ui/app.slint).
            single_click: "pause".into(),
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

fn home_or_temp() -> PathBuf {
    directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .unwrap_or_else(std::env::temp_dir)
}

/// In a Flatpak whose Pictures or Downloads folder isn't set up, a folder of
/// the app's own: the sandbox grants those folders only when they are
/// configured, and could write neither the home folder nor anything else
/// Patina would fall back to.
fn sandboxed_folder(name: &str) -> Option<PathBuf> {
    crate::platform::flatpak()
        .then(|| directories::BaseDirs::new().map(|dirs| dirs.data_dir().join(name)))
        .flatten()
}

fn yes_no(value: bool) -> String {
    if value { "yes" } else { "no" }.into()
}

impl Preferences {
    /// Brings settings saved by a Patina that had made `moved` of the
    /// changed defaults to the ones that have changed since. Every setting
    /// is saved, so an old default can't be told from a choice; this runs
    /// once, so that a choice made after the move is kept. True when it
    /// changed anything.
    fn move_old_defaults(&mut self, moved: u32) -> bool {
        let mut changed = false;
        // 1: a click pauses rather than showing or hiding the controller.
        if moved < 1 && self.single_click == "toggle-osc" {
            self.single_click = "pause".into();
            changed = true;
        }
        changed
    }

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
        match directories::UserDirs::new()
            .and_then(|dirs| dirs.picture_dir().map(Path::to_path_buf))
        {
            Some(pictures) => pictures.join("Patina"),
            None => {
                sandboxed_folder("Screenshots").unwrap_or_else(|| home_or_temp().join("Patina"))
            }
        }
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
            .or_else(|| sandboxed_folder("Downloads"))
            .unwrap_or_else(home_or_temp)
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

    /// The yt-dlp options as mpv is given them, with YouTube's clients added
    /// (`ytdl_youtube_clients`). Theirs is quoted because the value has a
    /// comma, which ends an unquoted value for mpv. It stays out of
    /// `ytdl_raw_options` so that the preference holds only what the user
    /// wrote.
    ///
    /// Added to options mpv can't read, they would be lost with them or be
    /// read as part of a name that yt-dlp exits on. So those options go and
    /// the clients stay.
    fn ytdl_raw_options_for_mpv(&self) -> String {
        let own = self.ytdl_raw_options.trim_end().trim_end_matches(',');
        let readable = read_ytdl_raw_options_checked(own).1;
        if !readable {
            // Not the options themselves: they can hold a proxy's password.
            log::warn!(
                "ignoring the yt-dlp options, which mpv can't read: a flag is written flag="
            );
        }
        let own = if readable { own } else { "" };
        let clients = ytdl_youtube_clients(own).filter(|_| !self.ytdl_is_youtube_dl());
        let own = match clients {
            Some(clients) if own.is_empty() => format!("extractor-args=\"{clients}\""),
            Some(clients) => format!("{own},extractor-args=\"{clients}\""),
            None => own.to_owned(),
        };
        if readable {
            return self.ytdl_raw_options_with_proxy(&own);
        }
        // A proxy among those that went still holds, as it does everywhere else
        // (`proxy`): this must not go direct, or by another one.
        let Some(proxy) = self.proxy() else {
            return own;
        };
        let proxy = format!("proxy=%{}%{proxy}", proxy.len());
        if own.is_empty() {
            proxy
        } else {
            format!("{own},{proxy}")
        }
    }

    /// The proxy for what talks to YouTube: yt-dlp, whichever part of Patina
    /// runs it, and the YouTube section's own requests. A `proxy` in the yt-dlp
    /// options is the more particular wish and wins, an empty one meaning
    /// direct; otherwise it is the HTTP proxy preference. None leaves the
    /// system's proxy settings to apply.
    ///
    /// mpv's hook passes yt-dlp only what is in its raw options, so without this
    /// a proxy set for privacy would cover the stream but not yt-dlp's requests.
    pub fn proxy(&self) -> Option<&str> {
        let own = read_ytdl_raw_options(&self.ytdl_raw_options)
            .into_iter()
            .find(|(key, _)| *key == "proxy")
            .map(|(_, value)| value.trim());
        own.or(Some(self.http_proxy.trim()).filter(|proxy| !proxy.is_empty()))
    }

    /// yt-dlp's options for what the YouTube section plays and downloads: the
    /// clients and the proxy, and nothing of the user's. Their options or their
    /// yt-dlp config could hold cookies, which would sign a section that has no
    /// account in to Google.
    pub fn ytdl_options_for_section(&self) -> String {
        let mut options = vec!["ignore-config=".to_owned()];
        if let Some(clients) = ytdl_youtube_clients("") {
            options.push(format!("extractor-args=\"{clients}\""));
        }
        if let Some(proxy) = self.proxy() {
            options.push(format!("proxy=%{}%{proxy}", proxy.len()));
        }
        options.join(",")
    }

    /// yt-dlp options with the proxy preference added, unless they have their
    /// own. The address is given with its length, as mpv reads that form
    /// whatever the address holds, a comma or a quote among them.
    pub fn ytdl_raw_options_with_proxy(&self, own: &str) -> String {
        let has_own = read_ytdl_raw_options(own)
            .iter()
            .any(|(key, _)| *key == "proxy");
        let proxy = self.http_proxy.trim();
        if has_own || proxy.is_empty() {
            return own.to_owned();
        }
        let own = own.trim_end().trim_end_matches(',');
        let proxy = format!("proxy=%{}%{proxy}", proxy.len());
        if own.is_empty() {
            proxy
        } else {
            format!("{own},{proxy}")
        }
    }

    /// Whether the program mpv's hook will run is youtube-dl, which it still
    /// falls back to, and which stops at an option it doesn't have.
    fn ytdl_is_youtube_dl(&self) -> bool {
        let mpv_config = Paths::discover().mpv_config_dir();
        crate::downloads::find_ytdl(self, &mpv_config).is_some_and(|program| {
            program.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .to_ascii_lowercase()
                    .starts_with("youtube-dl")
            })
        })
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
            ("ytdl-raw-options", self.ytdl_raw_options_for_mpv()),
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
        options
    }

    /// What Patina tells mpv's yt-dlp hook through `script-opts`, as entries
    /// `Player::set_hook_options` adds to what mpv has there: the yt-dlp to
    /// run, when `ytdl_for_hook` names one, and to give a file only the
    /// formats asked for, unless the hook's own configuration
    /// (`script-opts/ytdl_hook.conf`) says which.
    ///
    /// mpv 0.42's hook gives every format yt-dlp lists by default, each video
    /// format an edition of the file and each audio format a track, where
    /// 0.41's gave the one asked for. Patina's Video Quality and Audio
    /// Quality menus choose a format by playing the entry again, its audio
    /// menu would list every audio format YouTube has, and the edition key
    /// would step through the video formats; and only some packages' libmpv
    /// is 0.42. So the hook goes on as 0.41's did, with every libmpv, unless
    /// the user says otherwise there or in their own `script-opts`.
    pub fn hook_options(&self) -> Vec<(String, String)> {
        let mpv_config = || Paths::discover().mpv_config_dir();
        let path = self.ytdl_for_hook(
            crate::platform::bundled("yt-dlp"),
            || crate::downloads::hook_ytdl_path(self, &mpv_config()).is_some(),
            || crate::downloads::find_ytdl(self, &mpv_config()),
        );
        // mpv's options.lua takes only yes or no for a flag, and keeps the
        // default for anything else.
        let formats_set =
            crate::downloads::hook_option(self, &mpv_config(), "all_formats", |value| {
                matches!(value, "yes" | "no")
            })
            .is_some();
        hook_entries(path, formats_set)
    }

    /// The yt-dlp mpv's hook is told to run: the path preference as it was
    /// written, or, when a package carries yt-dlp, the one `find_ytdl`
    /// settles on. Nothing otherwise, which leaves the hook to search as it
    /// always has; nor when the hook's own configuration
    /// (`script-opts/ytdl_hook.conf`) names one, which `script-opts` would
    /// override, and which `find_ytdl` then settles on too. Without this the
    /// hook would never run the bundled copy: it isn't on the `PATH`, and on
    /// macOS an app opened from the Finder has only the system's folders
    /// there.
    fn ytdl_for_hook(
        &self,
        bundled: Option<PathBuf>,
        hook_names_one: impl FnOnce() -> bool,
        find: impl FnOnce() -> Option<PathBuf>,
    ) -> Option<String> {
        if !self.ytdl_path.is_empty() {
            return Some(self.ytdl_path.clone());
        }
        bundled?;
        if hook_names_one() {
            return None;
        }
        Some(find()?.to_str()?.to_owned())
    }
}

/// The `script-opts` entries for the hook (`Preferences::hook_options`).
fn hook_entries(path: Option<String>, formats_set: bool) -> Vec<(String, String)> {
    let path = path.map(|path| ("ytdl_hook-ytdl_path".to_owned(), path));
    let formats = (!formats_set).then(|| ("ytdl_hook-all_formats".to_owned(), "no".to_owned()));
    path.into_iter().chain(formats).collect()
}

/// The clients yt-dlp is to ask YouTube for, as `--extractor-args` takes
/// them, unless the yt-dlp options `raw` say otherwise.
///
/// A YouTube video with an ad fails to open otherwise. yt-dlp's default
/// clients are given stream addresses that answer 403 until the ad's skip
/// time has passed, which yt-dlp waits for and mpv's hook before 0.42
/// doesn't. `visionos` is given addresses that work at once. `default`
/// stays after it for the formats and the fallback it brings, and because
/// `visionos` alone chooses a format that takes merging, which fails
/// without ffmpeg.
///
/// mpv 0.42's hook waits too (its PR #18193), but shows nothing while it
/// does, and a stop or another file waits with it: for an ad that can't be
/// skipped, the whole ad. So this stays with every libmpv, and the wait is
/// what is left should YouTube change how it treats that client, which any
/// yt-dlp release can follow; a video with an ad failing to open, or slow
/// to, means looking here first. A libmpv older than 0.42, as distributions
/// carry, needs it to open such a video at all.
///
/// An `extractor-args` of the user's wins, since mpv's options keep only one
/// of a name. So do cookies, since yt-dlp skips `visionos` with them, with a
/// warning each time.
///
/// Only Patina's own yt-dlp options can be seen here, not yt-dlp's config
/// file. yt-dlp reads that first, and the argument made of this then replaces
/// its whole `youtube` entry, so a `lang` or `po_token` there is lost as well
/// as its clients. Whoever needs those writes the `extractor-args` in Patina's
/// yt-dlp options, which then wins. Cookies in that file aren't seen either,
/// which costs only the warning.
///
/// youtube-dl has no such option. mpv's hook can still run it, so mpv isn't
/// given this for it (`Preferences::ytdl_is_youtube_dl`); downloads already
/// need yt-dlp, for `--progress-template`.
pub fn ytdl_youtube_clients(raw: &str) -> Option<&'static str> {
    let own = read_ytdl_raw_options(raw)
        .iter()
        .any(|(key, _)| matches!(*key, "extractor-args" | "cookies" | "cookies-from-browser"));
    (!own).then_some("youtube:player_client=visionos,default")
}

/// mpv's `ytdl-raw-options` ("key=value,flag=") as names and values, so that
/// Patina's downloads read what mpv reads. A flag with no `=` and spaces
/// around a name, which mpv's hook can't use, are also read as what they
/// plainly mean, as downloads always have.
pub fn read_ytdl_raw_options(raw: &str) -> Vec<(&str, &str)> {
    read_ytdl_raw_options_checked(raw).0
}

/// Those options, and whether mpv's hook can use the text as it is, when they
/// are exactly what mpv reads. mpv reads a name up to the next `=`, past any
/// comma, not knowing flags: a flag at the end makes it refuse the whole text,
/// and one before another option makes a name with a comma in it, which yt-dlp
/// exits on, on every site. A name with a space in it or none at all does the
/// same. So does a colon after the last value: mpv takes it, between options,
/// but then nothing can be added behind it.
fn read_ytdl_raw_options_checked(raw: &str) -> (Vec<(&str, &str)>, bool) {
    let mut options = Vec::new();
    let mut usable = true;
    let mut rest = raw;
    while !rest.is_empty() {
        let key = ytdl_raw_option_part(&mut rest, &['=', ','], &mut usable);
        usable &= !key.is_empty() && !key.contains(char::is_whitespace);
        let value = match rest.strip_prefix('=') {
            Some(after) => {
                rest = after;
                ytdl_raw_option_part(&mut rest, &[','], &mut usable)
            }
            None => {
                usable = false;
                ""
            }
        };
        if let Some(after) = rest.strip_prefix(',') {
            rest = after;
        } else if let Some(after) = rest.strip_prefix(':') {
            usable &= !after.is_empty();
            rest = after;
        } else if !rest.is_empty() {
            usable = false;
            rest = rest.split_once(',').map_or("", |(_, after)| after);
        }
        let key = key.trim();
        if !key.is_empty() {
            options.push((key, value));
        }
    }
    (options, usable)
}

/// The name or value `rest` starts with, up to the next of `ends`. As for mpv,
/// one in `"…"`, `[…]` or `%length%…` can hold those. Left open it isn't
/// quoted at all, and mpv refuses it.
fn ytdl_raw_option_part<'a>(rest: &mut &'a str, ends: &[char], usable: &mut bool) -> &'a str {
    fn quoted(text: &str) -> Option<(&str, &str)> {
        let inner = text.get(1..)?;
        match text.chars().next()? {
            '"' => inner.split_once('"'),
            '[' => {
                let mut depth = 1;
                let end = inner.find(|c: char| {
                    match c {
                        '[' => depth += 1,
                        ']' => depth -= 1,
                        _ => {}
                    }
                    depth == 0
                })?;
                Some((&inner[..end], &inner[end + 1..]))
            }
            '%' => {
                let (length, after) = inner.split_once('%')?;
                let length: usize = length.parse().ok()?;
                Some((after.get(..length)?, &after[length..]))
            }
            _ => None,
        }
    }
    let text = *rest;
    let (part, after) = quoted(text).unwrap_or_else(|| {
        *usable &= !text.starts_with(['"', '[', '%']);
        text.split_at(text.find(ends).unwrap_or(text.len()))
    });
    *rest = after;
    part
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
    /// The YouTube section sent it, which is how it opens from here again: with
    /// none of the user's yt-dlp options, as a section with no account keeps to.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub section: bool,
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

    /// Gives `path`'s entry `title` in place of a name it was recorded under
    /// for want of one, whether one of `fallbacks` or one `is_untitled`
    /// knows; whether it changed. A title already there stays.
    pub fn retitle(&mut self, path: &str, title: &str, fallbacks: &[&str]) -> bool {
        let fallback =
            |name: &str| (!name.is_empty() && fallbacks.contains(&name)) || is_untitled(path, name);
        match self.entries.iter_mut().find(|entry| entry.path == path) {
            Some(entry) if fallback(&entry.title) && !fallback(title) => {
                entry.title = title.to_owned();
                true
            }
            _ => false,
        }
    }

    pub fn recent(&self, limit: usize) -> impl Iterator<Item = &HistoryEntry> {
        self.entries.iter().take(limit)
    }
}

/// Whether `title` is no title for `path`, only what it was called for want
/// of one: nothing, Patina's name for it (`file_name`, which for an address
/// is all of it), or for an address mpv's. With no title mpv shows its
/// `filename`, and with mpv 0.42 a web video was recorded in the history
/// under that before yt-dlp's title came (player::give_ytdl_title); the
/// entries recorded before that was mended keep it until something gives
/// them their title.
pub fn is_untitled(path: &str, title: &str) -> bool {
    title.is_empty()
        || title == crate::player::state::file_name(path)
        || (crate::util::is_url(path) && mpv_filenames(path).iter().any(|name| name == title))
}

/// mpv's `filename` for an address: the address percent-decoded, then what
/// comes after its last '/', query and all, or all of it when nothing does.
/// The part as written comes too, which is the same unless the address has
/// an escape in it, as YouTube's share codes do ("%3D").
fn mpv_filenames(url: &str) -> [String; 2] {
    let last = |text: &str| match text.rsplit('/').next() {
        Some(part) if !part.is_empty() => part.to_owned(),
        _ => text.to_owned(),
    };
    [last(&mpv_unescaped(url)), last(url)]
}

/// `text` with each `%` and two hex digits made the byte they stand for, as
/// mpv's `mp_url_unescape_inplace` makes it: a `%` with anything else after
/// it stays, with the two characters after it.
fn mpv_unescaped(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut unescaped = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escape = match bytes.get(index..index + 3) {
            Some([b'%', high, low]) => Some((*high as char, *low as char)),
            _ => None,
        };
        match escape {
            Some((high, low)) => {
                match (high.to_digit(16), low.to_digit(16)) {
                    (Some(high), Some(low)) => unescaped.push((high * 16 + low) as u8),
                    _ => unescaped.extend_from_slice(&bytes[index..index + 3]),
                }
                index += 3;
            }
            None => {
                unescaped.push(bytes[index]);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&unescaped).into_owned()
}

/// Loads the preferences, keeping every setting that can still be read
/// when others are invalid (say, after a setting changed type). Settings
/// from an older Patina get the defaults changed since
/// (`Preferences::move_old_defaults`), once.
pub fn load_preferences(path: &Path) -> Preferences {
    let mut prefs = read_preferences(path);
    // The count of changes made is kept in a file of its own rather than
    // in the settings, because an older Patina rewrites the settings from
    // its own fields and would drop it; the next move would then take back
    // a choice made since. A missing or unreadable count is 0. Settings
    // that don't exist yet are defaults, which the move leaves alone, and
    // the count saved now keeps it from moving them once they do.
    let count = moved_defaults_file(path);
    let moved = std::fs::read(&count)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(0);
    if moved < MOVED_DEFAULTS {
        if prefs.move_old_defaults(moved) {
            save_json(path, &prefs);
        }
        save_json(&count, &MOVED_DEFAULTS);
    }
    prefs
}

/// Where `load_preferences` keeps how many of the changed defaults the
/// settings at `settings` have had: beside them, in a file only it uses.
fn moved_defaults_file(settings: &Path) -> PathBuf {
    settings.with_file_name("moved-defaults.json")
}

fn read_preferences(path: &Path) -> Preferences {
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
    use crate::mpv::Node;

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

    const CLIENTS: &str = "extractor-args=\"youtube:player_client=visionos,default\"";

    fn raw_options_for_mpv_with(own: &str, program: &str) -> String {
        let prefs = Preferences {
            ytdl_raw_options: own.into(),
            ytdl_path: program.into(),
            ..Default::default()
        };
        prefs
            .mpv_options()
            .into_iter()
            .find(|(name, _)| *name == "ytdl-raw-options")
            .unwrap()
            .1
    }

    /// Named, so that these don't depend on what this machine has installed.
    fn raw_options_for_mpv(own: &str) -> String {
        raw_options_for_mpv_with(own, "yt-dlp")
    }

    /// Whatever the user has set for other sites, the section stays signed out.
    #[test]
    fn the_sections_options_take_nothing_of_the_users() {
        let prefs = Preferences {
            ytdl_raw_options: "cookies-from-browser=firefox,no-mtime=".into(),
            http_proxy: "http://user:p,w@proxy.example:8080".into(),
            ..Default::default()
        };
        let options = prefs.ytdl_options_for_section();
        let read = read_ytdl_raw_options(&options);
        assert_eq!(
            read,
            [
                ("ignore-config", ""),
                ("extractor-args", "youtube:player_client=visionos,default"),
                ("proxy", "http://user:p,w@proxy.example:8080"),
            ]
        );
        assert!(read_ytdl_raw_options_checked(&options).1, "{options}");
        // A proxy in the user's own options still says which way to go.
        let prefs = Preferences {
            ytdl_raw_options: "cookies=/a/b,proxy=http://own:1".into(),
            http_proxy: "http://other:2".into(),
            ..Default::default()
        };
        let options = prefs.ytdl_options_for_section();
        assert!(!options.contains("cookies"), "{options}");
        assert!(options.ends_with("proxy=%12%http://own:1"), "{options}");
        let plain = Preferences::default().ytdl_options_for_section();
        assert!(!plain.contains("proxy"), "{plain}");
    }

    #[test]
    fn mpv_is_told_which_clients_to_ask_youtube_for() {
        assert_eq!(raw_options_for_mpv(""), CLIENTS);
        assert_eq!(raw_options_for_mpv("  "), CLIENTS);
        assert_eq!(
            raw_options_for_mpv("no-mtime="),
            format!("no-mtime=,{CLIENTS}")
        );
        // A comma left at the end would make two in a row, which mpv refuses.
        assert_eq!(
            raw_options_for_mpv("format-sort=res,"),
            format!("format-sort=res,{CLIENTS}")
        );
    }

    #[test]
    fn the_users_own_extractor_args_or_cookies_win() {
        for own in [
            "extractor-args=\"youtube:player_client=tv,web\"",
            "no-mtime=,extractor-args=\"youtube:player_client=tv\"",
            "cookies=/home/me/cookies.txt",
            "cookies-from-browser=firefox",
            "cookies-from-browser=firefox,no-mtime=",
        ] {
            assert_eq!(raw_options_for_mpv(own), own);
            assert_eq!(ytdl_youtube_clients(own), None, "{own}");
        }
        // Only an option's name counts, not what a value happens to say.
        for own in [
            "user-agent=\"a,cookies=b\"",
            "no-cookies=",
            "add-header=\"extractor-args:1\"",
        ] {
            assert!(ytdl_youtube_clients(own).is_some(), "{own}");
        }
    }

    /// With these, yt-dlp would exit on every site, or mpv would refuse them
    /// whole. The clients must not go down with them.
    #[test]
    fn options_mpv_cant_read_give_way_to_the_clients() {
        for own in [
            "no-mtime",
            "format-sort=res,no-mtime",
            "no-mtime,format-sort=res",
            "no-mtime,cookies=a",
            " no-mtime=",
            "format-sort=res, no-mtime=",
            "=res",
            "format-sort=\"res",
            "format-sort=[res",
            "format-sort=%9%res",
            "format-sort=\"res\"fps",
            "format-sort=\"res\":",
        ] {
            assert_eq!(raw_options_for_mpv(own), CLIENTS, "{own}");
        }
    }

    #[test]
    fn youtube_dl_is_not_given_an_option_it_stops_at() {
        let dir = std::env::temp_dir().join(format!("patina-ytdl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let program = |name: &str| {
            let path = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
            std::fs::write(&path, "").unwrap();
            path.to_str().unwrap().to_owned()
        };
        let old = program("youtube-dl");
        assert_eq!(raw_options_for_mpv_with("", &old), "");
        assert_eq!(raw_options_for_mpv_with("no-mtime=", &old), "no-mtime=");
        assert_eq!(raw_options_for_mpv_with("no-mtime", &old), "");
        assert_eq!(raw_options_for_mpv_with("", &program("yt-dlp")), CLIENTS);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Text the reader calls usable is read as mpv reads it. The rest, mpv
    /// refuses or reads as a name no option has.
    #[test]
    fn the_reader_knows_what_mpv_makes_of_any_text() {
        let Some(mpv) = crate::mpv::testing::headless() else {
            return;
        };
        for raw in [
            "",
            "a=b",
            "a=b,",
            "a=",
            "a=b:c=d",
            "a=b ,c=d",
            "a=\"b\":c=d",
            "a=\"b,c\",d=e",
            "a=[x[y],z],b=c",
            "a=%3%x,y,b=c",
            "a=%0%,b=c",
            "\"a\"=b",
            "no-mtime",
            "a=b,no-mtime",
            "no-mtime,a=b",
            "a=b,,",
            " a=b",
            "a=b, c=d",
            "a =b",
            "=b",
            "a=\"b",
            "a=[b",
            "a=[b]]",
            "a=%x",
            "a=%9%b",
            "a=\"b\"c",
            "\"a\"b=c",
        ] {
            let (read, usable) = read_ytdl_raw_options_checked(raw);
            let took = mpv.set_property("ytdl-raw-options", raw).is_ok();
            if !took {
                assert!(!usable, "{raw}: mpv refuses it");
                continue;
            }
            let Node::Map(seen) = mpv.get_property("ytdl-raw-options").unwrap() else {
                panic!("{raw}: not a map");
            };
            if usable {
                let mut seen: Vec<(&str, &str)> = seen
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str().unwrap()))
                    .collect();
                seen.sort_unstable();
                let mut read = read;
                read.sort_unstable();
                assert_eq!(read, seen, "{raw}");
            } else {
                assert!(
                    seen.iter()
                        .any(|(key, _)| key.is_empty() || key.contains([',', ' '])),
                    "{raw}: mpv reads it well enough"
                );
            }
        }
    }

    /// The options Patina's downloads read are the ones mpv reads, and the
    /// clients reach mpv's yt-dlp hook in one piece.
    #[test]
    fn raw_options_read_as_mpv_reads_them() {
        let Some(mpv) = crate::mpv::testing::headless() else {
            return;
        };
        for own in [
            "",
            "no-mtime=",
            "no-mtime",
            "format-sort=res,no-mtime",
            "format-sort=res,",
            "proxy=\"http://127.0.0.1:8080\",no-mtime=",
            "extractor-args=\"youtube:player_client=tv,web\"",
            "extractor-args=[youtube:player_client=tv,web],no-mtime=",
            "\"format-sort\"=\"res,fps\"",
        ] {
            let raw = raw_options_for_mpv(own);
            mpv.set_property("ytdl-raw-options", raw.as_str())
                .unwrap_or_else(|error| panic!("{raw}: {error}"));
            let Node::Map(seen) = mpv.get_property("ytdl-raw-options").unwrap() else {
                panic!("{raw}: not a map");
            };
            let mut seen: Vec<(&str, &str)> = seen
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str().unwrap()))
                .collect();
            seen.sort_unstable();
            let mut read = read_ytdl_raw_options(&raw);
            read.sort_unstable();
            assert_eq!(read, seen, "{raw}");
        }
        assert_eq!(
            read_ytdl_raw_options(&raw_options_for_mpv("")),
            [("extractor-args", "youtube:player_client=visionos,default")]
        );
    }

    fn prefs_with_proxy(own: &str, proxy: &str) -> Preferences {
        Preferences {
            ytdl_raw_options: own.into(),
            http_proxy: proxy.into(),
            ..Default::default()
        }
    }

    fn proxy_seen_by_mpv(prefs: &Preferences) -> Option<String> {
        read_ytdl_raw_options(&prefs.ytdl_raw_options_for_mpv())
            .into_iter()
            .find(|(key, _)| *key == "proxy")
            .map(|(_, value)| value.to_owned())
    }

    /// One rule for all that talks to YouTube: the preference, unless the
    /// yt-dlp options name their own, and none leaves the system's to apply.
    #[test]
    fn the_proxy_preference_reaches_yt_dlp() {
        let none = prefs_with_proxy("", "");
        assert_eq!(none.proxy(), None);
        assert_eq!(proxy_seen_by_mpv(&none), None);

        let set = prefs_with_proxy("", " http://proxy.example:8080 ");
        assert_eq!(set.proxy(), Some("http://proxy.example:8080"));
        assert_eq!(
            proxy_seen_by_mpv(&set).as_deref(),
            Some("http://proxy.example:8080")
        );

        // What the user wrote for yt-dlp is the more particular wish.
        let own = prefs_with_proxy("proxy=socks5://a:1", "http://b:2");
        assert_eq!(own.proxy(), Some("socks5://a:1"));
        assert_eq!(proxy_seen_by_mpv(&own).as_deref(), Some("socks5://a:1"));

        // Empty means direct.
        let direct = prefs_with_proxy("proxy=,no-mtime=", "http://b:2");
        assert_eq!(direct.proxy(), Some(""));
        assert_eq!(proxy_seen_by_mpv(&direct).as_deref(), Some(""));

        // Other options stay, and options mpv can't read don't lose it.
        let other = prefs_with_proxy("no-mtime=,", "http://b:2");
        assert_eq!(
            read_ytdl_raw_options(&other.ytdl_raw_options_for_mpv())
                .iter()
                .map(|(key, _)| *key)
                .collect::<Vec<_>>(),
            ["no-mtime", "extractor-args", "proxy"]
        );
        let unreadable = prefs_with_proxy("no-mtime", "http://b:2");
        assert_eq!(
            proxy_seen_by_mpv(&unreadable).as_deref(),
            Some("http://b:2")
        );

        // A proxy among options mpv can't read is still the one that holds.
        for http in ["", "http://a.example:1"] {
            let lost = prefs_with_proxy("proxy=http://b.example:2,no-mtime", http);
            assert_eq!(lost.proxy(), Some("http://b.example:2"));
            assert_eq!(
                proxy_seen_by_mpv(&lost).as_deref(),
                Some("http://b.example:2")
            );
        }
    }

    /// An address holding what ends a value for mpv, read as mpv reads it.
    #[test]
    fn a_proxy_address_reaches_mpv_whole() {
        let Some(mpv) = crate::mpv::testing::headless() else {
            return;
        };
        for proxy in [
            "http://proxy.example:8080",
            "http://user:p,a=s\"s[w]@proxy.example:8080",
            "socks5://名前:1",
        ] {
            for own in ["", "no-mtime=", "extractor-args=\"youtube:x=1,2\""] {
                let raw = prefs_with_proxy(own, proxy).ytdl_raw_options_for_mpv();
                mpv.set_property("ytdl-raw-options", raw.as_str())
                    .unwrap_or_else(|error| panic!("{raw}: {error}"));
                let Node::Map(seen) = mpv.get_property("ytdl-raw-options").unwrap() else {
                    panic!("{raw}: not a map");
                };
                let seen = seen
                    .iter()
                    .find(|(key, _)| key == "proxy")
                    .and_then(|(_, value)| value.as_str())
                    .map(str::to_owned);
                assert_eq!(seen.as_deref(), Some(proxy), "{raw}");
            }
        }
    }

    /// The hook is told which yt-dlp to run only when the user said so or a
    /// package carries one; otherwise, as on Linux, it searches by itself.
    #[test]
    fn mpv_is_told_of_a_bundled_yt_dlp_only() {
        let bundled = || Some(PathBuf::from("/A,b/Patina.app/Contents/Helpers/yt-dlp"));
        let found = || Some(PathBuf::from("/A,b/Patina.app/Contents/Helpers/yt-dlp"));
        let none = Preferences::default();
        let no_hook = || false;
        assert_eq!(none.ytdl_for_hook(None, || panic!("read"), found), None);
        assert_eq!(
            none.ytdl_for_hook(bundled(), no_hook, found).as_deref(),
            Some("/A,b/Patina.app/Contents/Helpers/yt-dlp")
        );
        // A yt-dlp of the user's in mpv's folder is what find_ytdl gives, and
        // the hook is then pointed at that.
        assert_eq!(
            none.ytdl_for_hook(bundled(), no_hook, || Some(PathBuf::from(
                "/conf/mpv/yt-dlp"
            )))
            .as_deref(),
            Some("/conf/mpv/yt-dlp")
        );
        // A ytdl_path in the hook's own configuration is the user's too, and
        // script-opts would override it, so the hook is told nothing.
        assert_eq!(
            none.ytdl_for_hook(bundled(), || true, || panic!("searched")),
            None
        );
        // The preference wins, as written, and is never searched for here.
        let own = Preferences {
            ytdl_path: "/opt/yt-dlp".into(),
            ..Default::default()
        };
        assert_eq!(
            own.ytdl_for_hook(bundled(), || panic!("read"), || panic!("searched"))
                .as_deref(),
            Some("/opt/yt-dlp")
        );
    }

    /// The hook gives a file only the formats asked for, as 0.41's did,
    /// unless its own configuration says which; beside the yt-dlp to run,
    /// when there is one.
    #[test]
    fn the_hook_gives_only_the_formats_asked_for() {
        let entry = |key: &str, value: &str| (key.to_owned(), value.to_owned());
        assert_eq!(
            hook_entries(None, false),
            [entry("ytdl_hook-all_formats", "no")]
        );
        assert_eq!(hook_entries(None, true), []);
        assert_eq!(
            hook_entries(Some("/a,b/yt-dlp".into()), false),
            [
                entry("ytdl_hook-ytdl_path", "/a,b/yt-dlp"),
                entry("ytdl_hook-all_formats", "no")
            ]
        );
    }

    /// A yt-dlp's path reaches the hook whole, comma and all, set as an
    /// entry of `script-opts`, as `Player::set_hook_options` sets it.
    #[test]
    fn a_bundled_yt_dlp_reaches_mpv_whole() {
        let Some(mpv) = crate::mpv::testing::headless() else {
            return;
        };
        for path in [
            r"C:\Users\Ann, Bo\AppData\Local\Programs\Patina\yt-dlp.exe",
            "/Volumes/A=b [c]/Patina.app/Contents/Helpers/yt-dlp",
            "/Applications/名前/Patina.app/Contents/Helpers/yt-dlp",
            "/a%3%b/yt-dlp",
        ] {
            let entries = hook_entries(Some(path.into()), false);
            mpv.set_property("script-opts", crate::mpv::Value::Map(entries.clone()))
                .unwrap_or_else(|error| panic!("{path}: {error}"));
            let Node::Map(seen) = mpv.get_property("script-opts").unwrap() else {
                panic!("{path}: not a map");
            };
            let seen: Vec<(String, String)> = seen
                .into_iter()
                .map(|(key, value)| (key, value.as_str().unwrap_or_default().to_owned()))
                .collect();
            assert_eq!(seen, entries, "{path}");
        }
    }

    /// mpv reads the section's options as they are meant, whatever the proxy
    /// holds, and the hook then passes them on as they are read.
    #[test]
    fn the_sections_options_reach_mpv_whole() {
        let Some(mpv) = crate::mpv::testing::headless() else {
            return;
        };
        let prefs = prefs_with_proxy("cookies=/a/b", "http://user:p,a=s\"s[w]@proxy.example:8080");
        let raw = prefs.ytdl_options_for_section();
        mpv.set_property("ytdl-raw-options", raw.as_str())
            .unwrap_or_else(|error| panic!("{raw}: {error}"));
        let Node::Map(seen) = mpv.get_property("ytdl-raw-options").unwrap() else {
            panic!("{raw}: not a map");
        };
        let value = |name: &str| {
            seen.iter()
                .find(|(key, _)| key == name)
                .and_then(|(_, value)| value.as_str())
        };
        assert_eq!(seen.len(), 3, "{raw}");
        assert_eq!(value("ignore-config"), Some(""));
        assert_eq!(
            value("extractor-args"),
            Some("youtube:player_client=visionos,default")
        );
        assert_eq!(
            value("proxy"),
            Some("http://user:p,a=s\"s[w]@proxy.example:8080")
        );
    }

    /// A plain entry reads and writes as it always has, and a video the YouTube
    /// section sent stays that way across a restart.
    #[test]
    fn history_remembers_what_the_section_sent() {
        let plain = r#"{"path":"a","title":"A","played_at":1,"duration":0.0,"position":0.0}"#;
        let mut entry: HistoryEntry = serde_json::from_str(plain).unwrap();
        assert!(!entry.section);
        assert_eq!(serde_json::to_string(&entry).unwrap(), plain);
        entry.section = true;
        let text = serde_json::to_string(&entry).unwrap();
        assert!(serde_json::from_str::<HistoryEntry>(&text).unwrap().section);
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
            section: false,
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

    /// A web address recorded under its last part, for want of a title,
    /// gets the title mpv gives later; a title it already has stays.
    #[test]
    fn history_takes_a_title_that_comes_late() {
        let mut history = History::default();
        let url = "https://www.youtube.com/watch?v=5xAgp6i9lUQ";
        history.record(HistoryEntry {
            path: url.into(),
            title: "watch?v=5xAgp6i9lUQ".into(),
            played_at: 1,
            duration: 0.0,
            position: 0.0,
            section: false,
        });
        // As App::retitle_history gives them: mpv's stand-in (its
        // `filename`), then Patina's, which for an address is all of it.
        let full = crate::player::state::file_name(url);
        assert_eq!(full, url);
        let fallbacks = ["watch?v=5xAgp6i9lUQ", full.as_str()];
        assert!(!history.retitle(url, "", &fallbacks));
        assert!(!history.retitle(url, "watch?v=5xAgp6i9lUQ", &fallbacks));
        assert!(!history.retitle("https://elsewhere", "Other", &fallbacks));
        assert!(history.retitle(url, "Big Buck Bunny Trailer", &fallbacks));
        assert_eq!(history.entries[0].title, "Big Buck Bunny Trailer");
        assert!(!history.retitle(url, "Something else", &fallbacks));
        assert_eq!(history.entries[0].title, "Big Buck Bunny Trailer");
    }

    /// An entry recorded under mpv's stand-in before yt-dlp's title came in
    /// time, the address's last part, has no title, however it came to be
    /// there: mpv's `filename` is the address percent-decoded, query and
    /// all. It takes a title whatever names it was given as stand-ins,
    /// none included, and a title it has stays.
    #[test]
    fn mpvs_stand_in_from_before_the_mend_is_no_title() {
        let share = "https://www.youtube.com/watch?v=d24DYchpPpc&pp=ugUEEgJlbg%3D%3D";
        for (path, title) in [
            (share, "watch?v=d24DYchpPpc&pp=ugUEEgJlbg=="),
            (share, "watch?v=d24DYchpPpc&pp=ugUEEgJlbg%3D%3D"),
            (share, share),
            (share, ""),
            ("https://youtu.be/x6RPajPuzr8?si=abc", "x6RPajPuzr8?si=abc"),
            ("https://example.com/videos/", "https://example.com/videos/"),
            // An escaped '/' is one by the time mpv takes the last part.
            ("https://example.com/a%2Fb.mp4", "b.mp4"),
            ("/films/a film.mkv", "a film.mkv"),
        ] {
            assert!(is_untitled(path, title), "{path}: {title}");
        }
        for (path, title) in [
            (share, "Big Buck Bunny Trailer"),
            (share, "ugUEEgJlbg=="),
            ("https://example.com/videos/", "videos"),
            // Only an address has mpv's stand-in.
            ("/films/a%20film.mkv", "a film.mkv"),
            ("/films/a film.mkv", "films/a film.mkv"),
        ] {
            assert!(!is_untitled(path, title), "{path}: {title}");
        }
        assert_eq!(mpv_unescaped("%41%zz%4"), "A%zz%4");

        let mut history = History::default();
        let url = "https://www.youtube.com/watch?v=x6RPajPuzr8";
        let entry = |path: &str, title: &str| HistoryEntry {
            path: path.into(),
            title: title.into(),
            played_at: 1,
            duration: 0.0,
            position: 0.0,
            section: false,
        };
        history.record(entry(url, "watch?v=x6RPajPuzr8"));
        history.record(entry(share, "Already titled"));
        assert!(!history.retitle(url, "watch?v=x6RPajPuzr8", &[]));
        // mpv may have had no stand-in to say, or none at all was asked.
        assert!(history.retitle(url, "Big Buck Bunny Trailer", &["", url]));
        assert_eq!(history.entries[1].title, "Big Buck Bunny Trailer");
        assert!(!history.retitle(share, "Something else", &[]));
        assert_eq!(history.entries[0].title, "Already titled");
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
    fn a_click_pauses_by_default() {
        assert_eq!(Preferences::default().single_click, "pause");
        let prefs: Preferences = serde_json::from_str(r#"{"volume": 60}"#).unwrap();
        assert_eq!(prefs.single_click, "pause");
    }

    /// A settings directory of its own for the test `name`, empty.
    fn moved_defaults_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "patina-moved-defaults-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The JSON in the file at `path`.
    fn json_in(path: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    /// What `load_preferences` makes of `json` in a settings file, with a
    /// count of changes made beside it if `moved` has one, and what it
    /// leaves in the settings and in the count.
    fn loaded_and_left(
        name: &str,
        json: &str,
        moved: Option<u32>,
    ) -> (Preferences, serde_json::Value, serde_json::Value) {
        let dir = moved_defaults_dir(name);
        let path = dir.join("settings.json");
        std::fs::write(&path, json).unwrap();
        if let Some(moved) = moved {
            save_json(&moved_defaults_file(&path), &moved);
        }
        let prefs = load_preferences(&path);
        let left = json_in(&path);
        let count = json_in(&moved_defaults_file(&path));
        std::fs::remove_dir_all(&dir).unwrap();
        (prefs, left, count)
    }

    #[test]
    fn the_old_default_click_moves_to_pausing_once() {
        let (prefs, left, count) = loaded_and_left(
            "old",
            r#"{"single-click": "toggle-osc", "volume": 40}"#,
            None,
        );
        assert_eq!(prefs.single_click, "pause");
        assert_eq!(prefs.volume, 40.0);
        assert_eq!(left["single-click"], "pause", "the move is saved");
        assert_eq!(left["volume"], 40.0);
        assert_eq!(count, MOVED_DEFAULTS);

        // Chosen again after the move, which saved the count.
        let (prefs, left, _) =
            loaded_and_left("chosen", r#"{"single-click": "toggle-osc"}"#, Some(1));
        assert_eq!(prefs.single_click, "toggle-osc");
        assert_eq!(left["single-click"], "toggle-osc");

        let (prefs, _, count) = loaded_and_left("other", r#"{"single-click": "pip"}"#, None);
        assert_eq!(prefs.single_click, "pip");
        assert_eq!(count, MOVED_DEFAULTS);
    }

    #[test]
    fn a_click_chosen_after_the_move_survives_an_older_patina_saving() {
        let dir = moved_defaults_dir("older");
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"single-click": "toggle-osc", "volume": 40}"#).unwrap();
        load_preferences(&path);
        std::fs::write(&path, r#"{"single-click": "toggle-osc", "volume": 40}"#).unwrap();
        assert_eq!(load_preferences(&path).single_click, "toggle-osc");

        // An older Patina saves only the fields it has, and none of them
        // says how many changes have been made.
        std::fs::write(&path, r#"{"single-click": "toggle-osc", "volume": 55}"#).unwrap();
        let prefs = load_preferences(&path);
        assert_eq!(prefs.single_click, "toggle-osc");
        assert_eq!(prefs.volume, 55.0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_click_chosen_in_new_settings_is_not_moved_later() {
        let dir = moved_defaults_dir("new");
        let path = dir.join("settings.json");
        let mut prefs = load_preferences(&path);
        assert_eq!(prefs.single_click, "pause");
        assert!(!path.exists(), "nothing is moved, so nothing is saved");
        prefs.single_click = "toggle-osc".into();
        save_json(&path, &prefs);
        assert_eq!(load_preferences(&path).single_click, "toggle-osc");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_old_default_click_moves_though_another_setting_is_unreadable() {
        let (prefs, left, count) = loaded_and_left(
            "lenient-old",
            r#"{"single-click": "toggle-osc", "screenshot-dir": null, "volume": 40}"#,
            None,
        );
        assert_eq!(prefs.single_click, "pause");
        assert_eq!(prefs.volume, 40.0);
        assert_eq!(left["single-click"], "pause");
        assert_eq!(count, MOVED_DEFAULTS);

        let (prefs, _, _) = loaded_and_left(
            "lenient-chosen",
            r#"{"single-click": "toggle-osc", "screenshot-dir": null}"#,
            Some(1),
        );
        assert_eq!(prefs.single_click, "toggle-osc");
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
