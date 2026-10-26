//! System media controls: media keys, headset buttons, and the desktop's
//! "now playing" display.
//!
//! Each platform has its own service: MPRIS over D-Bus on Linux, the System
//! Media Transport Controls on Windows, and MediaPlayer's remote command
//! and now-playing centers on macOS. They all report the same [`Command`]s
//! and show the same [`NowPlaying`] state.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(all(unix, not(target_os = "macos")))]
mod mpris;
#[cfg(target_os = "windows")]
mod windows;

use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    Stopped,
    Playing,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoopMode {
    #[default]
    None,
    Track,
    Playlist,
}

/// What the desktop shows about the current media.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NowPlaying {
    pub status: Status,
    /// Changes whenever a different file starts.
    pub track: u64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub url: String,
    /// A `file://` URL of the cover or a frame, if there is one.
    pub art_url: String,
    pub duration: f64,
    pub rate: f64,
    pub volume: f64,
    pub loop_mode: LoopMode,
    pub shuffle: bool,
    pub can_next: bool,
    pub can_previous: bool,
    pub can_seek: bool,
    pub audio_only: bool,
}

/// A request from the system's media controls. Not every platform sends
/// every one.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
pub enum Command {
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    /// Seek by this many seconds.
    Seek(f64),
    /// Seek to this position in seconds.
    SetPosition(f64),
    /// Volume from 0 to 1 (MPRIS) or 0 to 100.
    SetVolume(f64),
    SetRate(f64),
    SetLoop(LoopMode),
    SetShuffle(bool),
    OpenUri(String),
    Raise,
    Quit,
}

/// Receives commands on whatever thread the platform delivers them on.
pub type Handler = Arc<dyn Fn(Command) + Send + Sync>;

pub struct MediaControls {
    #[cfg(all(unix, not(target_os = "macos")))]
    backend: mpris::Mpris,
    #[cfg(target_os = "windows")]
    backend: windows::Smtc,
    #[cfg(target_os = "macos")]
    backend: macos::NowPlayingCenter,
}

impl MediaControls {
    /// Starts the platform's media controls. `window` is the native handle of
    /// a window, which Windows needs to attach its controls to.
    pub fn start(
        handler: Handler,
        window: Option<raw_window_handle::RawWindowHandle>,
    ) -> Option<Self> {
        let _ = &window;
        #[cfg(all(unix, not(target_os = "macos")))]
        let backend = mpris::Mpris::start(handler)?;
        #[cfg(target_os = "windows")]
        let backend = windows::Smtc::start(handler, window?)?;
        #[cfg(target_os = "macos")]
        let backend = macos::NowPlayingCenter::start(handler)?;
        Some(Self { backend })
    }

    pub fn update(&self, now: &NowPlaying) {
        self.backend.update(now);
    }

    /// Records the playback position without announcing it; clients
    /// extrapolate from the rate.
    pub fn set_position(&self, seconds: f64) {
        self.backend.set_position(seconds);
    }

    /// Announces a jump in position (seeking, or a new file).
    pub fn seeked(&self, seconds: f64) {
        self.backend.seeked(seconds);
    }
}
