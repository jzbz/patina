//! MPRIS 2 (`org.mpris.MediaPlayer2`) over the D-Bus session bus.
//!
//! zbus is async; a thread of its own drives the connection with
//! `zbus::block_on`. Properties are read from shared state that the UI
//! thread keeps current, and change signals are sent when it changes.

use super::{Command, Handler, LoopMode, NowPlaying, Status};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use zbus::object_server::SignalEmitter;
use zbus::{ObjectPath, OwnedVariant, Str, Variant, interface};

const PATH: &str = "/org/mpris/MediaPlayer2";
const NO_TRACK: &str = "/org/mpris/MediaPlayer2/TrackList/NoTrack";

#[derive(Default)]
struct Shared {
    now: NowPlaying,
    position: f64,
}

type SharedState = Arc<Mutex<Shared>>;

fn lock(shared: &SharedState) -> MutexGuard<'_, Shared> {
    shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

enum Message {
    Update(NowPlaying),
    Seeked(f64),
}

pub struct Mpris {
    sender: async_channel::Sender<Message>,
    shared: SharedState,
}

impl Mpris {
    pub fn start(handler: Handler) -> Option<Self> {
        let (sender, receiver) = async_channel::unbounded();
        let shared = SharedState::default();
        let thread_shared = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("mpris".into())
            .spawn(move || {
                if let Err(error) = zbus::block_on(serve(receiver, thread_shared, handler)) {
                    log::warn!(
                        "media keys and the desktop's player controls are unavailable: {error}"
                    );
                }
            });
        if let Err(error) = spawned {
            log::warn!("could not start MPRIS: {error}");
            return None;
        }
        Some(Self { sender, shared })
    }

    pub fn update(&self, now: &NowPlaying) {
        let _ = self.sender.try_send(Message::Update(now.clone()));
    }

    pub fn set_position(&self, seconds: f64) {
        lock(&self.shared).position = seconds;
    }

    pub fn seeked(&self, seconds: f64) {
        self.set_position(seconds);
        let _ = self.sender.try_send(Message::Seeked(seconds));
    }
}

fn micros(seconds: f64) -> i64 {
    (seconds.max(0.0) * 1_000_000.0) as i64
}

/// `org.mpris.MediaPlayer2.video.patina.Patina.instance<pid>_<token>`.
/// Several Patina processes may run, and MPRIS allows an instance suffix; no
/// part of a bus name may start with a digit, hence `instance` in front. The
/// process ID alone doesn't tell them apart in a Flatpak, where every launch
/// has a PID namespace of its own and is process 2 to itself, so a token
/// drawn for this process follows it. The application ID comes first
/// because a Flatpak sandbox lets an app own `org.mpris.MediaPlayer2.<app id>`
/// and every name below it without being granted them, and refuses other
/// names. Shells label the player by its Identity and DesktopEntry, not by
/// this name.
fn bus_name() -> String {
    use std::hash::{BuildHasher, RandomState};
    let pid = std::process::id();
    // RandomState is seeded afresh in every process.
    let token = RandomState::new().hash_one(pid) as u32;
    format!(
        "org.mpris.MediaPlayer2.{}.instance{pid}_{token:x}",
        crate::APP_ID
    )
}

fn track_path(now: &NowPlaying) -> String {
    if now.status == Status::Stopped && now.title.is_empty() {
        NO_TRACK.into()
    } else {
        format!("/video/patina/Patina/track/{}", now.track)
    }
}

async fn serve(
    receiver: async_channel::Receiver<Message>,
    shared: SharedState,
    handler: Handler,
) -> zbus::Result<()> {
    let connection = zbus::connection::Builder::session()
        .name(bus_name())
        .serve_at(
            PATH,
            Root {
                handler: handler.clone(),
            },
        )
        .serve_at(
            PATH,
            Player {
                handler,
                shared: shared.clone(),
            },
        )
        .build()
        .await?;
    let player = connection
        .object_server()
        .interface::<_, Player>(PATH)
        .await?;

    let mut shown = NowPlaying::default();
    while let Ok(message) = receiver.recv().await {
        match message {
            Message::Update(now) => {
                lock(&shared).now = now.clone();
                let emitter = player.signal_emitter();
                let iface = player.get().await;
                if now.status != shown.status {
                    iface.playback_status_changed(emitter).await?;
                }
                if now.loop_mode != shown.loop_mode {
                    iface.loop_status_changed(emitter).await?;
                }
                if now.rate != shown.rate {
                    iface.rate_changed(emitter).await?;
                }
                if now.shuffle != shown.shuffle {
                    iface.shuffle_changed(emitter).await?;
                }
                if now.volume != shown.volume {
                    iface.volume_changed(emitter).await?;
                }
                if (
                    &now.title,
                    &now.artist,
                    &now.album,
                    &now.url,
                    &now.art_url,
                    now.duration,
                    now.track,
                ) != (
                    &shown.title,
                    &shown.artist,
                    &shown.album,
                    &shown.url,
                    &shown.art_url,
                    shown.duration,
                    shown.track,
                ) {
                    iface.metadata_changed(emitter).await?;
                }
                if now.can_next != shown.can_next {
                    iface.can_go_next_changed(emitter).await?;
                }
                if now.can_previous != shown.can_previous {
                    iface.can_go_previous_changed(emitter).await?;
                }
                if now.can_seek != shown.can_seek {
                    iface.can_seek_changed(emitter).await?;
                }
                if (now.status == Status::Stopped) != (shown.status == Status::Stopped) {
                    iface.can_play_changed(emitter).await?;
                    iface.can_pause_changed(emitter).await?;
                }
                shown = now;
            }
            Message::Seeked(seconds) => {
                Player::seeked(player.signal_emitter(), micros(seconds)).await?;
            }
        }
    }
    Ok(())
}

struct Root {
    handler: Handler,
}

#[interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) {
        (self.handler)(Command::Raise);
    }

    fn quit(&self) {
        (self.handler)(Command::Quit);
    }

    #[zbus(property)]
    fn can_quit(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_raise(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn identity(&self) -> String {
        "Patina".into()
    }

    #[zbus(property)]
    fn desktop_entry(&self) -> String {
        crate::APP_ID.into()
    }

    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        [
            "file", "http", "https", "ftp", "rtmp", "rtsp", "smb", "sftp", "dav", "mms",
        ]
        .map(String::from)
        .to_vec()
    }

    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        crate::formats::mime_types(crate::formats::ALL)
            .into_iter()
            .map(String::from)
            .collect()
    }
}

struct Player {
    handler: Handler,
    shared: SharedState,
}

impl Player {
    fn now(&self) -> NowPlaying {
        lock(&self.shared).now.clone()
    }
}

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn next(&self) {
        (self.handler)(Command::Next);
    }

    fn previous(&self) {
        (self.handler)(Command::Previous);
    }

    fn pause(&self) {
        (self.handler)(Command::Pause);
    }

    fn play_pause(&self) {
        (self.handler)(Command::PlayPause);
    }

    fn stop(&self) {
        (self.handler)(Command::Stop);
    }

    fn play(&self) {
        (self.handler)(Command::Play);
    }

    fn seek(&self, offset: i64) {
        (self.handler)(Command::Seek(offset as f64 / 1_000_000.0));
    }

    fn set_position(&self, track_id: ObjectPath<'_>, position: i64) {
        // The spec says to ignore requests for a track that is no longer current.
        if track_id.as_str() == track_path(&self.now()) && position >= 0 {
            (self.handler)(Command::SetPosition(position as f64 / 1_000_000.0));
        }
    }

    fn open_uri(&self, uri: &str) {
        (self.handler)(Command::OpenUri(uri.to_owned()));
    }

    #[zbus(signal)]
    async fn seeked(emitter: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;

    #[zbus(property)]
    fn playback_status(&self) -> String {
        match self.now().status {
            Status::Playing => "Playing",
            Status::Paused => "Paused",
            Status::Stopped => "Stopped",
        }
        .into()
    }

    #[zbus(property)]
    fn loop_status(&self) -> String {
        match self.now().loop_mode {
            LoopMode::None => "None",
            LoopMode::Track => "Track",
            LoopMode::Playlist => "Playlist",
        }
        .into()
    }

    #[zbus(property)]
    fn set_loop_status(&mut self, value: String) {
        let mode = match value.as_str() {
            "Track" => LoopMode::Track,
            "Playlist" => LoopMode::Playlist,
            _ => LoopMode::None,
        };
        (self.handler)(Command::SetLoop(mode));
    }

    #[zbus(property)]
    fn rate(&self) -> f64 {
        self.now().rate
    }

    #[zbus(property)]
    fn set_rate(&mut self, value: f64) {
        // A rate of 0 means pause, per the spec.
        if value <= 0.0 {
            (self.handler)(Command::Pause);
        } else {
            (self.handler)(Command::SetRate(value));
        }
    }

    #[zbus(property)]
    fn shuffle(&self) -> bool {
        self.now().shuffle
    }

    #[zbus(property)]
    fn set_shuffle(&mut self, value: bool) {
        (self.handler)(Command::SetShuffle(value));
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedVariant> {
        let now = self.now();
        let mut metadata = HashMap::new();
        let path = track_path(&now);
        if let Ok(path) = ObjectPath::try_from(path) {
            metadata.insert("mpris:trackid".into(), OwnedVariant::from(path));
        }
        if now.title.is_empty() {
            return metadata;
        }
        let text = |value: &str| OwnedVariant::from(Str::from(value.to_owned()));
        metadata.insert("xesam:title".into(), text(&now.title));
        if now.duration > 0.0 {
            metadata.insert(
                "mpris:length".into(),
                OwnedVariant::from(micros(now.duration)),
            );
        }
        if !now.artist.is_empty()
            && let Ok(artists) = Variant::new(vec![now.artist.clone()]).try_to_owned()
        {
            metadata.insert("xesam:artist".into(), artists);
        }
        if !now.album.is_empty() {
            metadata.insert("xesam:album".into(), text(&now.album));
        }
        if !now.url.is_empty() {
            metadata.insert("xesam:url".into(), text(&now.url));
        }
        if !now.art_url.is_empty() {
            metadata.insert("mpris:artUrl".into(), text(&now.art_url));
        }
        metadata
    }

    #[zbus(property)]
    fn volume(&self) -> f64 {
        self.now().volume
    }

    #[zbus(property)]
    fn set_volume(&mut self, value: f64) {
        (self.handler)(Command::SetVolume(value.clamp(0.0, 10.0)));
    }

    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        micros(lock(&self.shared).position)
    }

    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        0.03125
    }

    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        32.0
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        self.now().can_next
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.now().can_previous
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        self.now().status != Status::Stopped
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.now().status != Status::Stopped
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.now().can_seek
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn can_control(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    /// A name outside the application ID's would work run directly and be
    /// refused only inside the Flatpak, so this is where it is caught.
    #[test]
    fn the_bus_name_sits_under_the_application_id() {
        let name = super::bus_name();
        let granted = format!("org.mpris.MediaPlayer2.{}.", crate::APP_ID);
        assert!(name.starts_with(&granted), "{name} is not under {granted}");
        assert!(
            zbus::names::WellKnownName::try_from(name.as_str()).is_ok(),
            "{name} is not a valid bus name"
        );
    }
}
