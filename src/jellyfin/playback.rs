//! Playing a Jellyfin item in a player window: what the load hook asks
//! the server and sets for mpv, how the server's streams map to mpv's
//! tracks, and the reports from which the server keeps the account's
//! progress. docs/jellyfin.md's Playback section is the design, and its
//! rules about the token hold here: the token goes to mpv only as a
//! header, through the setter that keeps it out of errors, and never into
//! a URL mpv opens, shows or logs.

use super::api::{Client, Error};
use super::models::*;
use super::session::{self, ItemInfo, Registry};
use crate::mpv::{Mpv, Secret, Value};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How long an entry opened at startup waits for the saved sign-ins: the
/// keyring may be asking to be unlocked meanwhile.
const SIGN_IN_WAIT: Duration = Duration::from_secs(20);

/// How often progress is reported while a file plays, paused too: the
/// server ends a session it hasn't heard from in five minutes, and a
/// transcode in one.
pub const PROGRESS_INTERVAL: Duration = Duration::from_secs(10);

/// The least time between two reports of changes (a pause, a seek, a
/// track): seeking along the bar makes one report, not one per step.
pub const CHANGE_GAP: Duration = Duration::from_secs(1);

/// The width artwork for the system's media controls is asked at.
const ART_WIDTH: u32 = 480;

/// How often a load waiting on the server looks whether mpv still wants
/// the entry.
const STILL_WANTED: Duration = Duration::from_millis(200);

// Servers.

/// A signed-in server, as the load hook needs it.
#[derive(Debug, Clone)]
pub struct Server {
    pub id: String,
    pub client: Client,
    /// The certificates mpv trusts for this server's streams, as the
    /// client trusts them for its requests.
    pub ca_file: Option<PathBuf>,
}

#[derive(Debug, Default)]
struct ServerList {
    /// The saved sign-ins are back from the keyring, or known to be gone.
    loaded: bool,
    servers: Vec<Server>,
}

/// The servers signed in now, which the session keeps up to date, for the
/// load hooks' threads. An entry reopened at startup can come before the
/// keyring has given the saved sign-ins back; it waits for them.
#[derive(Debug, Clone, Default)]
pub struct Servers(Arc<(Mutex<ServerList>, Condvar)>);

impl Servers {
    fn change(&self, change: impl FnOnce(&mut ServerList)) {
        let (list, changed) = &*self.0;
        change(&mut list.lock().unwrap_or_else(PoisonError::into_inner));
        changed.notify_all();
    }

    /// Adds a server signed in, or puts it in the place of its old sign-in.
    pub fn set(&self, server: Server) {
        self.change(|list| {
            list.servers.retain(|known| known.id != server.id);
            list.servers.push(server);
        });
    }

    pub fn remove(&self, id: &str) {
        self.change(|list| list.servers.retain(|known| known.id != id));
    }

    /// The saved sign-ins are all in, or known to be gone.
    pub fn loaded(&self) {
        self.change(|list| list.loaded = true);
    }

    /// The server `id` signed in, waiting up to `timeout` while the saved
    /// sign-ins are still loading. Blocks.
    pub fn wait_for(&self, id: &str, timeout: Duration) -> Option<Server> {
        let deadline = Instant::now() + timeout;
        let (list, changed) = &*self.0;
        let mut list = list.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(server) = list.servers.iter().find(|server| server.id == id) {
                return Some(server.clone());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if list.loaded || left.is_zero() {
                return None;
            }
            list = changed
                .wait_timeout(list, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

// What the load hook does.

/// What a window asks of an entry's next load, beyond the choices made on
/// its page: a lower quality, another version, or, for a reload, the audio
/// and subtitle streams playing, or one the server has to burn in or a
/// transcode doesn't carry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Choice {
    /// Bits per second; None plays the file as it is.
    pub max_bitrate: Option<i64>,
    /// The version chosen from the player's menu, in the place of the
    /// page's.
    pub media_source_id: Option<String>,
    /// Streams of the version that plays.
    pub audio: Option<i32>,
    /// -1 for none.
    pub subtitle: Option<i32>,
}

/// The transcoding the preferences ask of the server, for every Jellyfin
/// file: what mpv could play but the computer may not decode well. The
/// quality menu's bitrate is the window's, in `Choice`.
pub fn transcode_rules(prefs: &crate::settings::Preferences) -> TranscodeRules {
    TranscodeRules {
        max_bitrate: None,
        hevc: prefs.jellyfin_transcode_hevc,
        av1: prefs.jellyfin_transcode_av1,
        hdr: prefs.jellyfin_transcode_hdr,
        over_1080p: prefs.jellyfin_transcode_over_1080p,
    }
}

/// Why an entry didn't play, as the window says it.
#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    /// No account is signed in on the entry's server.
    SignedOut,
    /// A playlist file named the entry: only what Patina's own library
    /// and history open is asked of the server with the account's token.
    Listed,
    Request(Error),
    /// The server's `ErrorCode`: "NotAllowed", "NoCompatibleStream",
    /// "RateLimitExceeded".
    Refused(String),
    NoSource,
    /// mpv didn't take the stream.
    Player(String),
    /// The stream needs the account's token, which mpv would write into
    /// the log file it keeps (`log-file`): Patina doesn't give it then.
    Logging,
    /// The user stopped, or went on to another entry, before the server
    /// answered.
    Abandoned,
}

impl Failure {
    /// In words, naming the server where there is one to name.
    pub fn message(&self, server: Option<&str>) -> String {
        let name = server.filter(|name| !name.is_empty());
        match self {
            Failure::SignedOut => format!(
                "Sign in to {} to play this.",
                name.unwrap_or("its Jellyfin server")
            ),
            Failure::Listed => {
                "A playlist file can't open Jellyfin items; open it from the library.".into()
            }
            Failure::Request(error) => error.about(name),
            Failure::Refused(code) => match code.as_str() {
                "NotAllowed" => "The server doesn't allow this account to play this.".into(),
                "NoCompatibleStream" => {
                    "The server has no way of playing this that Patina can take.".into()
                }
                "RateLimitExceeded" => {
                    "The server is streaming as much as this account may; try again later.".into()
                }
                other => format!("The server won't play this ({other})."),
            },
            Failure::NoSource => "The server has no file for this.".into(),
            Failure::Player(error) => error.clone(),
            Failure::Logging => "This stream needs the account's sign-in, which mpv's log file \
                                 (log-file) would keep. Turn the log file off to play it."
                .into(),
            Failure::Abandoned => "Stopped before the server answered.".into(),
        }
    }
}

/// What the load hook made of the entry being opened, waiting for the
/// window: written before mpv goes on, and read once mpv has opened the
/// file or failed to, so the two can't pass each other.
pub type Resolved = Arc<Mutex<Option<(String, Result<Playing, Failure>)>>>;

/// What the load hook's thread is given for a Jellyfin entry.
#[derive(Clone)]
pub struct Hook {
    pub servers: Servers,
    pub registry: Registry,
    /// The versions chosen from the window's Version menu, by entry: a
    /// version is the item's own, while its streams are numbered by
    /// version and go only with a reload (`Reload`).
    pub versions: HashMap<String, String>,
    /// The bitrate chosen from the window's Video Quality menu, which holds
    /// for every entry the window plays until the user changes it: one
    /// lowered for the network is wanted for the next episode too.
    pub max_bitrate: Option<i64>,
    pub rules: TranscodeRules,
    pub resolved: Resolved,
}

/// What a window reloading the entry it plays (for a subtitle the server
/// burns in, another quality or another version) carries over to that one
/// load: where it was, and the streams playing, as the server numbers
/// them, so that they play on. A later load of the entry starts afresh,
/// with the choices made on the item's page and the server's.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reload {
    /// Seconds.
    pub at: f64,
    pub audio: Option<i32>,
    /// -1 for none.
    pub subtitle: Option<i32>,
}

/// The artwork and names the system's media controls show.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Shown {
    /// The show for an episode, the artists for a song.
    pub artist: String,
    /// The season for an episode, the album for a song.
    pub album: String,
    /// The server's image, which anyone may fetch: no token in it.
    pub art_url: String,
}

/// One of an item's versions, for the player's Version menu.
#[derive(Debug, Clone, PartialEq)]
pub struct Version {
    pub id: String,
    pub name: String,
}

/// One playback of an entry, from the load hook to its last report.
#[derive(Debug, Clone, PartialEq)]
pub struct Playing {
    pub entry: String,
    pub server_id: String,
    pub item_id: String,
    pub title: String,
    pub audio_only: bool,
    pub play_session_id: Option<String>,
    pub media_source_id: String,
    pub live_stream_id: Option<String>,
    pub method: PlayMethod,
    /// The source's streams, by the server's index.
    pub streams: Vec<MediaStream>,
    /// The streams given to mpv as external subtitles, in the order given.
    pub external: Vec<External>,
    /// The streams the server chose (or was asked for); -1 for no
    /// subtitles.
    pub audio: Option<i32>,
    pub subtitle: Option<i32>,
    /// Where it started, in seconds.
    pub start: f64,
    pub shown: Shown,
    /// The item's versions, the one playing among them.
    pub versions: Vec<Version>,
    /// The sizes of trickplay thumbnails the server has for the version
    /// playing, narrowest first; none when it has none.
    pub trickplay: Vec<TrickplayInfo>,
    /// How long the item is, as the server says; mpv says too once the
    /// file is open.
    pub duration: Option<f64>,
    /// The source's bitrate, in bits per second, when the server knows it:
    /// a quality at or above it plays the file as it is.
    pub bitrate: Option<i64>,
}

/// One of the server's subtitle files, given to mpv: its stream, and the
/// URL mpv names the track by (its `external-filename`). A file that
/// fails to load adds no track, so mpv's numbers don't say which is which.
#[derive(Debug, Clone, PartialEq)]
pub struct External {
    pub index: i32,
    pub url: String,
}

/// What the hook sets for an entry: the stream, as the playlist entry's
/// `stream-open-filename`, and options local to the file. The header is
/// apart, since it carries the token.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadOptions {
    pub stream: String,
    /// The `Authorization` header, for a transcode only: the server's HLS
    /// playlists and segments need the token, while its files and
    /// subtitle files are served without one (on 10.10.7, 10.11 and 12.4
    /// alike). mpv sends the header to whatever it opens for the file, and
    /// every way it could leak (a redirect, a log) is one less for the
    /// common case of playing the file as it is.
    pub header: Option<Secret>,
    /// FFmpeg's options for the file's streams, added to the user's own
    /// (`stream-lavf-o`) rather than put in their place.
    pub stream_options: Vec<(&'static str, &'static str)>,
    pub options: Vec<(&'static str, Value)>,
}

impl LoadOptions {
    #[cfg(test)]
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.options
            .iter()
            .find(|(option, _)| *option == name)
            .map(|(_, value)| value)
    }
}

/// What the server said, made into what mpv opens and what the window
/// keeps for the reports.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub playing: Playing,
    pub options: LoadOptions,
}

/// What a load asks of the server.
#[derive(Debug, Clone)]
pub struct Asked<'a> {
    pub entry: &'a str,
    pub server_id: &'a str,
    pub item: &'a BaseItemDto,
    pub info: &'a ItemInfo,
    pub choice: &'a Choice,
    /// Where to pick up again: a reload for another quality or subtitle.
    pub resume_at: Option<f64>,
}

impl Asked<'_> {
    /// The version asked for: the one chosen in the player, or else on the
    /// item's page, or with a stream chosen, the first, since the server
    /// applies stream indexes only with a version named.
    fn media_source_id(&self) -> Option<String> {
        let streams = self.audio().is_some() || self.subtitle().is_some();
        self.choice
            .media_source_id
            .clone()
            .or_else(|| self.info.media_source_id.clone())
            .or_else(|| {
                streams
                    .then(|| {
                        self.item
                            .media_sources
                            .first()
                            .map(|source| source.id.clone())
                    })
                    .flatten()
            })
    }

    /// Whether the streams chosen on the item's page hold: they are the
    /// indexes of the version chosen there, which another version chosen
    /// in the player numbers its own way.
    fn page_streams(&self) -> bool {
        self.choice
            .media_source_id
            .as_ref()
            .is_none_or(|id| self.info.media_source_id.as_ref() == Some(id))
    }

    fn audio(&self) -> Option<i32> {
        self.choice
            .audio
            .or(self.info.audio_stream_index.filter(|_| self.page_streams()))
    }

    fn subtitle(&self) -> Option<i32> {
        self.choice.subtitle.or(self
            .info
            .subtitle_stream_index
            .filter(|_| self.page_streams()))
    }

    /// The body of `/Items/{id}/PlaybackInfo`.
    pub fn request(&self, user_id: Option<&str>, rules: &TranscodeRules) -> PlaybackInfoRequest {
        let rules = TranscodeRules {
            max_bitrate: self.choice.max_bitrate.or(rules.max_bitrate),
            ..rules.clone()
        };
        let media_source_id = self.media_source_id();
        let named = media_source_id.is_some();
        PlaybackInfoRequest {
            user_id: user_id.map(str::to_owned),
            max_streaming_bitrate: rules.max_bitrate,
            audio_stream_index: self.audio().filter(|_| named),
            subtitle_stream_index: self.subtitle().filter(|_| named),
            media_source_id,
            device_profile: Some(DeviceProfile::patina(&rules)),
            ..Default::default()
        }
    }

    /// Where to start, in seconds: where a reload left off, or the
    /// server's resume position unless the user chose to play from the
    /// beginning. Music always starts at the beginning. A film watched to
    /// the end has no resume position (the server clears it then, and when
    /// it is marked played); one played before and stopped part-way
    /// through again has, and resumes there, as Continue Watching lists it.
    fn start(&self) -> Option<f64> {
        if let Some(at) = self.resume_at {
            return (at > 0.5).then_some(at);
        }
        if self.info.from_start || self.info.audio {
            return None;
        }
        self.item.resume_position()
    }
}

/// From the load hook, for an entry `session::parse_entry` reads: asks the
/// server how to play it, and sets what mpv opens, for this file only.
/// What came of it waits in `hook.resolved` for the window. Blocks.
pub fn prepare(mpv: &Mpv, entry: &str, hook: &Hook, reload: Option<Reload>) {
    // Whatever happens, mpv's yt-dlp hook leaves the entry and its stream
    // alone (it would run yt-dlp on the stream's URL), and mpv writes no
    // watch-later file for it, as it would on a quit or a replace while the
    // server is asked (`save-position-on-quit`, which Resume Playback sets):
    // read before this hook the next time the entry opens, it would compete
    // with the server's place.
    for name in ["ytdl", "save-position-on-quit"] {
        if let Err(error) = mpv.set_property(&format!("file-local-options/{name}"), false) {
            log::warn!("{error}");
        }
    }
    let listed = mpv
        .get_property("playlist-path")
        .ok()
        .and_then(|path| path.as_str().map(|path| !path.is_empty()))
        .unwrap_or(false);
    let result = if listed {
        Err(Failure::Listed)
    } else {
        resolve_while_wanted(mpv, entry, hook, reload)
            .and_then(|plan| apply(mpv, &plan.options).map(|()| plan.playing))
    };
    match &result {
        Err(Failure::Abandoned) => log::debug!("not waiting for the server: {entry}"),
        Err(failure) => log::warn!("not playing {entry}: {failure:?}"),
        Ok(_) => {}
    }
    *hook.resolved.lock().unwrap_or_else(PoisonError::into_inner) =
        Some((entry.to_owned(), result));
}

/// `resolve`, on a thread of its own, given up on once mpv no longer wants
/// the entry: mpv takes a stop, or another entry, only when the hook is
/// done, and a server that doesn't answer would hold the user up until its
/// requests time out. What the server says after that is dropped. Blocks.
fn resolve_while_wanted(
    mpv: &Mpv,
    entry: &str,
    hook: &Hook,
    reload: Option<Reload>,
) -> Result<Plan, Failure> {
    let wanted = current_entry(mpv);
    let (sender, receiver) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("jellyfin-load".into())
        .spawn({
            let entry = entry.to_owned();
            let hook = hook.clone();
            let reload = reload.clone();
            move || {
                let _ = sender.send(resolve(&entry, &hook, reload.as_ref()));
            }
        });
    if let Err(error) = spawned {
        log::warn!("could not ask the Jellyfin server: {error}");
        return resolve(entry, hook, reload.as_ref());
    }
    loop {
        match receiver.recv_timeout(STILL_WANTED) {
            Ok(result) => return result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if current_entry(mpv) != wanted {
                    return Err(Failure::Abandoned);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Failure::Abandoned);
            }
        }
    }
}

/// The playlist entry mpv is on, by its id: a stop empties the playlist,
/// and going to another entry, or replacing the playlist, changes it.
fn current_entry(mpv: &Mpv) -> Option<i64> {
    let at = mpv.get_property("playlist-pos").ok()?.as_i64()?;
    if at < 0 {
        return None;
    }
    mpv.get_property(&format!("playlist/{at}/id"))
        .ok()?
        .as_i64()
}

/// The registry's entry for `entry`, taking the user's choice to play it
/// from the beginning: that holds for the load it was made for, and the
/// entry opened again later (from History, Recent, Resume Last Playback,
/// or the playlist come round again) resumes where the server says.
fn take_known(registry: &Registry, entry: &str) -> Option<ItemInfo> {
    let mut registry = registry.lock().unwrap_or_else(PoisonError::into_inner);
    let info = registry.get_mut(entry)?;
    let known = info.clone();
    info.from_start = false;
    Some(known)
}

/// What the window asks of this load of `entry`: its quality, the version
/// chosen in its menu, and what a reload carries over.
fn choice_for(entry: &str, hook: &Hook, reload: Option<&Reload>) -> Choice {
    Choice {
        max_bitrate: hook.max_bitrate,
        media_source_id: hook.versions.get(entry).cloned(),
        audio: reload.and_then(|reload| reload.audio),
        subtitle: reload.and_then(|reload| reload.subtitle),
    }
}

/// Asks the server about an entry and plans its playback. Blocks.
fn resolve(entry: &str, hook: &Hook, reload: Option<&Reload>) -> Result<Plan, Failure> {
    let (server_id, item_id) = session::parse_entry(entry).ok_or(Failure::NoSource)?;
    let server = hook
        .servers
        .wait_for(server_id, SIGN_IN_WAIT)
        .ok_or(Failure::SignedOut)?;
    let client = &server.client;
    // Asked each time, for the resume position as it is now and for what
    // the media controls show; an entry from History or Recent, or one the
    // registry dropped, is known only from this.
    let item = client.item(item_id).map_err(Failure::Request)?;
    let info = match take_known(&hook.registry, entry) {
        Some(info) => info,
        None => {
            let info = ItemInfo {
                server_id: server_id.to_owned(),
                id: item.id.clone(),
                title: super::library::display_title(&item),
                kind: item.kind,
                audio: is_audio(&item),
                resume_ticks: item
                    .user_data
                    .as_ref()
                    .map_or(0, |data| data.playback_position_ticks.max(0)),
                ..Default::default()
            };
            session::remember(
                &mut hook.registry.lock().unwrap_or_else(PoisonError::into_inner),
                vec![(entry.to_owned(), info.clone())],
            );
            info
        }
    };
    let choice = choice_for(entry, hook, reload);
    let asked = Asked {
        entry,
        server_id,
        item: &item,
        info: &info,
        choice: &choice,
        resume_at: reload.map(|reload| reload.at),
    };
    let answer = client
        .playback_info(item_id, &asked.request(client.user_id(), &hook.rules))
        .map_err(Failure::Request)?;
    plan(client, server.ca_file.as_deref(), &asked, answer)
}

fn is_audio(item: &BaseItemDto) -> bool {
    item.media_type == MediaKind::Audio || item.kind == ItemKind::Audio
}

/// The playback the server's answer makes, and what the hook sets for it:
/// the file as it is when the server lets Patina play it directly, or its
/// transcode.
pub fn plan(
    client: &Client,
    ca_file: Option<&Path>,
    asked: &Asked,
    answer: PlaybackInfoResponse,
) -> Result<Plan, Failure> {
    if let Some(code) = answer.error_code.filter(|code| !code.is_empty()) {
        return Err(Failure::Refused(code));
    }
    let wanted = asked.media_source_id();
    let source = answer
        .media_sources
        .iter()
        .find(|source| Some(&source.id) == wanted.as_ref())
        .or_else(|| answer.media_sources.first())
        .ok_or(Failure::NoSource)?;
    let item = asked.item;
    let audio = asked.info.audio || is_audio(item);
    let direct = || client.static_stream_url(&item.id, audio, source);
    let (stream, method) = if source.supports_direct_play {
        (direct(), PlayMethod::DirectPlay)
    } else if let Some(url) = client.transcode_url(source) {
        (url, PlayMethod::Transcode)
    } else if source.supports_direct_stream {
        (direct(), PlayMethod::DirectStream)
    } else {
        // Nothing better offered: mpv plays most files as they are.
        (direct(), PlayMethod::DirectPlay)
    };
    let mut streams = source.media_streams.clone();
    streams.sort_by_key(|stream| stream.index);
    // The server's own subtitle files, never one on another host: mpv
    // sends the header to whatever it opens for a transcode. In direct
    // play the file's own subtitles are in the file already.
    let mut external = Vec::new();
    for stream in &streams {
        let delivered = stream.kind == StreamKind::Subtitle
            && stream.delivery_method == Some(DeliveryMethod::External)
            && stream.is_external_url != Some(true)
            && (stream.is_external || method == PlayMethod::Transcode);
        if let Some(url) = delivered.then(|| client.delivery_url(stream)).flatten() {
            external.push(External {
                index: stream.index,
                url: url.to_string(),
            });
        }
    }
    let start = asked.start();
    let title = if asked.info.title.is_empty() {
        super::library::display_title(item)
    } else {
        asked.info.title.clone()
    };
    let mut options: Vec<(&'static str, Value)> = vec![
        ("force-media-title", title.clone().into()),
        ("ytdl", false.into()),
        // The server keeps where to resume. mpv reads its own resume
        // position before the load hook runs, so `start` is always set:
        // to none when the server has no position, or a watch-later file
        // written for the entry would decide where it starts.
        (
            "start",
            start
                .map_or_else(|| "none".to_owned(), |at| format!("{at:.3}"))
                .into(),
        ),
        ("save-position-on-quit", false.into()),
        ("tls-verify", true.into()),
    ];
    if let Some(path) = ca_file {
        options.push(("tls-ca-file", path.to_string_lossy().into_owned().into()));
    }
    if !external.is_empty() {
        // A list: set as one string, mpv would split it at every colon.
        let urls = external.iter().map(|file| file.url.clone()).collect();
        options.push(("sub-files", Value::List(urls)));
    }
    let header =
        (method == PlayMethod::Transcode).then(|| Secret(Value::List(client.http_header_fields())));
    let mut stream_options = Vec::new();
    if header.is_some() {
        // FFmpeg sends the header on to wherever a redirect points, plain
        // http and other hosts included (the API's requests follow one
        // only on the server), and with mpv's playlist prefetching on, the
        // next entry is opened with this file's options while it plays.
        stream_options.push(("max_redirects", "0"));
        options.push(("prefetch-playlist", false.into()));
    }
    let chosen = |asked: Option<i32>, default: Option<i32>| asked.or(default);
    let playing = Playing {
        entry: asked.entry.to_owned(),
        server_id: asked.server_id.to_owned(),
        item_id: item.id.clone(),
        title,
        audio_only: audio,
        play_session_id: answer.play_session_id.clone(),
        media_source_id: source.id.clone(),
        live_stream_id: source.live_stream_id.clone(),
        method,
        streams,
        external,
        audio: chosen(asked.audio(), source.default_audio_stream_index),
        subtitle: chosen(asked.subtitle(), source.default_subtitle_stream_index),
        start: start.unwrap_or(0.0),
        shown: shown(client, item),
        versions: item
            .media_sources
            .iter()
            .enumerate()
            .map(|(index, version)| Version {
                id: version.id.clone(),
                name: super::library::version_title(index, version),
            })
            .collect(),
        trickplay: item.trickplay_sizes(&source.id),
        duration: source
            .run_time_ticks
            .or(item.run_time_ticks)
            .filter(|ticks| *ticks > 0)
            .map(ticks_to_seconds),
        bitrate: source.bitrate.filter(|bitrate| *bitrate > 0),
    };
    // The server's tracks, selected as the file opens and for this file
    // only: set while it plays, mpv would keep `aid` and `sid` for the
    // next file (subtitles off for a local file after an item played
    // without), and the first report would name the tracks before them.
    // A selection the window corrects, or the user makes, ends with the
    // file too.
    let planned = playing.planned();
    options.push(("aid", planned.aid.value()));
    options.push(("sid", planned.sid.value()));
    Ok(Plan {
        playing,
        options: LoadOptions {
            stream: stream.to_string(),
            header,
            stream_options,
            options,
        },
    })
}

/// The names and artwork the media controls show for an item: an
/// episode's still or its show's poster, a song's album cover.
fn shown(client: &Client, item: &BaseItemDto) -> Shown {
    let own = item
        .image_tag(ImageType::Primary)
        .map(|tag| (item.id.as_str(), tag));
    let (artist, album, art) = match item.kind {
        ItemKind::Episode => (
            item.series_name.clone().unwrap_or_default(),
            item.season_name.clone().unwrap_or_default(),
            own.or_else(|| {
                item.series_id
                    .as_deref()
                    .zip(item.series_primary_image_tag.as_deref())
            }),
        ),
        _ if is_audio(item) => {
            let artists = if item.artists.is_empty() {
                item.album_artist.clone().unwrap_or_default()
            } else {
                item.artists.join(", ")
            };
            let cover = item
                .album_id
                .as_deref()
                .zip(item.album_primary_image_tag.as_deref());
            (
                artists,
                item.album.clone().unwrap_or_default(),
                cover.or(own),
            )
        }
        _ => (String::new(), String::new(), own),
    };
    Shown {
        artist,
        album,
        art_url: art
            .map(|(id, tag)| {
                client
                    .image_url(id, ImageType::Primary, None, Some(tag), Some(ART_WIDTH))
                    .to_string()
            })
            .unwrap_or_default(),
    }
}

/// Sets what the hook planned, for the file about to open. The header goes
/// through the setter that leaves it out of errors, and not at all while
/// mpv writes a log file, which records every property set at a level no
/// option lowers.
pub fn apply(mpv: &Mpv, options: &LoadOptions) -> Result<(), Failure> {
    if options.header.is_some() && writes_log_file(mpv) {
        return Err(Failure::Logging);
    }
    for (name, value) in &options.options {
        if let Err(error) = mpv.set_property(&format!("file-local-options/{name}"), value.clone()) {
            log::warn!("{error}");
        }
    }
    if !options.stream_options.is_empty() {
        let merged = Value::Map(added_to(
            key_values(mpv, "stream-lavf-o"),
            &options.stream_options,
        ));
        if let Err(error) = mpv.set_property("file-local-options/stream-lavf-o", merged) {
            log::warn!("{error}");
        }
    }
    if let Some(header) = &options.header {
        quiet_ffmpeg(mpv);
        if let Err(error) = mpv.set_property_secret("file-local-options/http-header-fields", header)
        {
            log::warn!("{error}");
        }
    }
    mpv.set_property("stream-open-filename", options.stream.as_str())
        .map_err(|error| Failure::Player(error.description()))
}

/// Whether mpv writes a log file: Preferences' extra options, mpv.conf in
/// the mpv folder and `--mpv-log-file` can each ask for one.
fn writes_log_file(mpv: &Mpv) -> bool {
    mpv.get_property("log-file")
        .ok()
        .and_then(|path| path.as_str().map(|path| !path.is_empty()))
        .unwrap_or(false)
}

/// A key-value list option's value as it is now, its entries in order.
fn key_values(mpv: &Mpv, name: &str) -> Vec<(String, String)> {
    match mpv.get_property(name) {
        Ok(crate::mpv::Node::Map(entries)) => entries
            .into_iter()
            .filter_map(|(key, value)| Some((key, value.as_str()?.to_owned())))
            .collect(),
        _ => Vec::new(),
    }
}

/// `entries` with `ours` in the place of any of the same name.
fn added_to(mut entries: Vec<(String, String)>, ours: &[(&str, &str)]) -> Vec<(String, String)> {
    for (key, value) in ours {
        entries.retain(|(known, _)| known != key);
        entries.push(((*key).to_owned(), (*value).to_owned()));
    }
    entries
}

/// At its trace level FFmpeg prints every request with its headers, the
/// token's among them, to the terminal (`--verbose` with a level of trace
/// asked for). While the header goes with a file, FFmpeg's messages stop
/// at debug, for that file only, whatever else the levels say.
fn quiet_ffmpeg(mpv: &Mpv) {
    let levels = key_values(mpv, "msg-level");
    if !levels.iter().any(|(_, level)| level == "trace") {
        return;
    }
    // The last level that matches a module counts, and "ffmpeg" matches
    // FFmpeg's every module ("ffmpeg/demuxer" too).
    let capped = Value::Map(added_to(levels, &[("ffmpeg", "debug")]));
    if let Err(error) = mpv.set_property("file-local-options/msg-level", capped) {
        log::warn!("{error}");
    }
}

// Tracks.

/// An mpv track, as far as mapping needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MpvTrack {
    pub kind: StreamKind,
    pub id: i64,
    pub external: bool,
    /// What an external track was loaded from (`external-filename`).
    pub filename: Option<String>,
}

/// One kind of track to select: mpv's track by its id, none, or mpv's own
/// choice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Pick {
    Track(i64),
    Off,
    #[default]
    Auto,
}

impl Pick {
    /// As mpv's `aid` and `sid` take it.
    pub fn value(self) -> Value {
        match self {
            Pick::Track(id) => Value::Int(id),
            Pick::Off => "no".into(),
            Pick::Auto => "auto".into(),
        }
    }
}

/// The server's audio and subtitle as mpv's tracks. A stream mpv doesn't
/// have as the server said is left to mpv's own choice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Selection {
    pub aid: Pick,
    pub sid: Pick,
}

impl Playing {
    /// Whether mpv sees the source's own streams: in direct play it opens
    /// the file, in a transcode only what the server made of it (one video
    /// and the audio asked for).
    fn in_file(&self) -> bool {
        self.method != PlayMethod::Transcode
    }

    /// The file's own streams of a kind, in order, which mpv numbers from 1
    /// in the same order.
    fn embedded(&self, kind: StreamKind) -> impl Iterator<Item = &MediaStream> {
        self.streams
            .iter()
            .filter(move |stream| stream.kind == kind && !stream.is_external)
    }

    fn embedded_subtitles(&self) -> i64 {
        if self.in_file() {
            self.embedded(StreamKind::Subtitle).count() as i64
        } else {
            0
        }
    }

    /// mpv's track for a stream of the source, by the server's index, as
    /// mpv numbers it when every file the hook adds loads: counted among
    /// the file's own streams of its kind, and an external subtitle after
    /// them, in the order the hook added them.
    pub fn track_id(&self, index: i32) -> Option<(StreamKind, i64)> {
        let stream = self.streams.iter().find(|stream| stream.index == index)?;
        let place = |kind| {
            self.embedded(kind)
                .position(|stream| stream.index == index)
                .map(|at| at as i64 + 1)
        };
        let id = match stream.kind {
            StreamKind::Audio if self.in_file() => place(StreamKind::Audio)?,
            // A transcode carries the audio asked for, and only that.
            StreamKind::Audio if self.audio == Some(index) => 1,
            StreamKind::Subtitle if self.in_file() && !stream.is_external => {
                place(StreamKind::Subtitle)?
            }
            StreamKind::Subtitle => {
                let at = self
                    .external
                    .iter()
                    .position(|added| added.index == index)?;
                self.embedded_subtitles() + at as i64 + 1
            }
            _ => return None,
        };
        Some((stream.kind, id))
    }

    /// mpv's track for a stream of the source among the tracks mpv has: an
    /// external subtitle by the URL it came from, since one that failed to
    /// load moves those after it down; any other by its place, where mpv
    /// has it as the server said (embedded where it should be).
    pub fn track_of(&self, index: i32, tracks: &[MpvTrack]) -> Option<i64> {
        if let Some(file) = self.external.iter().find(|file| file.index == index) {
            return tracks
                .iter()
                .find(|track| {
                    track.kind == StreamKind::Subtitle
                        && track.external
                        && track.filename.as_deref() == Some(file.url.as_str())
                })
                .map(|track| track.id);
        }
        let (kind, id) = self.track_id(index)?;
        tracks
            .iter()
            .any(|track| track.kind == kind && track.id == id && !track.external)
            .then_some(id)
    }

    /// The server's index for one of mpv's tracks; None for a track the
    /// server doesn't know, as a subtitle the user added.
    pub fn stream_index(&self, track: &MpvTrack) -> Option<i32> {
        let nth = |kind, id: i64| {
            let at = usize::try_from(id.checked_sub(1)?).ok()?;
            self.embedded(kind).nth(at).map(|stream| stream.index)
        };
        match track.kind {
            StreamKind::Audio if self.in_file() => nth(StreamKind::Audio, track.id),
            StreamKind::Audio => (track.id == 1).then_some(self.audio).flatten(),
            StreamKind::Subtitle if track.external => {
                let url = track.filename.as_deref()?;
                self.external
                    .iter()
                    .find(|file| file.url == url)
                    .map(|file| file.index)
            }
            StreamKind::Subtitle if track.id <= self.embedded_subtitles() => {
                nth(StreamKind::Subtitle, track.id)
            }
            _ => None,
        }
    }

    /// The subtitle the server burns into the picture, if it does.
    pub fn burned_in(&self) -> Option<i32> {
        let index = self.subtitle.filter(|index| *index >= 0)?;
        self.streams
            .iter()
            .any(|stream| {
                stream.index == index && stream.delivery_method == Some(DeliveryMethod::Encode)
            })
            .then_some(index)
    }

    /// A stream's name as the server gives it, for mpv's tracks that come
    /// with none (an external subtitle is named by its URL's file name, a
    /// transcode's audio not at all).
    pub fn stream_label(&self, index: i32) -> Option<String> {
        self.streams
            .iter()
            .find(|stream| stream.index == index)
            .map(super::library::stream_title)
    }

    /// The subtitles only the server can show, by burning them in: the
    /// Subtitles menu offers them apart from mpv's tracks.
    pub fn burnable(&self) -> impl Iterator<Item = &MediaStream> {
        self.streams.iter().filter(|stream| {
            stream.kind == StreamKind::Subtitle
                && stream.delivery_method == Some(DeliveryMethod::Encode)
        })
    }

    /// The source's audio streams a transcode doesn't carry (it carries
    /// one): the Audio menu offers them apart from mpv's track, and the
    /// server transcodes another when one is chosen.
    pub fn other_audio(&self) -> impl Iterator<Item = &MediaStream> {
        let transcoded = !self.in_file();
        self.streams.iter().filter(move |stream| {
            transcoded && stream.kind == StreamKind::Audio && Some(stream.index) != self.audio
        })
    }

    /// The selection the hook asks for as the file opens, before mpv has
    /// its tracks: the server's streams as `track_id` numbers them.
    pub fn planned(&self) -> Selection {
        self.select(|index| self.track_id(index).map(|(_, id)| id))
    }

    /// The server's audio and subtitle among the tracks mpv has.
    pub fn selection(&self, tracks: &[MpvTrack]) -> Selection {
        self.select(|index| self.track_of(index, tracks))
    }

    fn select(&self, id: impl Fn(i32) -> Option<i64>) -> Selection {
        let pick = |index| id(index).map_or(Pick::Auto, Pick::Track);
        let aid = self.audio.map_or(Pick::Auto, pick);
        let sid = match self.subtitle {
            None => Pick::Off,
            Some(index) if index < 0 || self.burned_in().is_some() => Pick::Off,
            Some(index) => pick(index),
        };
        Selection { aid, sid }
    }

    /// The streams to report for mpv's selection: the server's indexes, the
    /// subtitle burned in, or -1 for no subtitles.
    pub fn reported_streams(
        &self,
        tracks: &[MpvTrack],
        aid: Option<i64>,
        sid: Option<i64>,
    ) -> (Option<i32>, Option<i32>) {
        let index = |kind, id| {
            let track = tracks
                .iter()
                .find(|track| track.kind == kind && track.id == id)?;
            self.stream_index(track)
        };
        let audio = aid.and_then(|id| index(StreamKind::Audio, id));
        let subtitle = match (self.burned_in(), sid) {
            (Some(index), _) => Some(index),
            (None, None) => Some(-1),
            (None, Some(id)) => index(StreamKind::Subtitle, id),
        };
        (audio, subtitle)
    }
}

// Reports.

/// The player's state, as a report gives it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Moment {
    pub position: f64,
    pub paused: bool,
    pub muted: bool,
    /// mpv's volume: 100 for full, more when boosted.
    pub volume: f64,
    pub seekable: bool,
    pub aid: Option<i64>,
    pub sid: Option<i64>,
    /// The tracks mpv has, which say what `aid` and `sid` are.
    pub tracks: Vec<MpvTrack>,
    pub loop_file: bool,
    pub loop_playlist: bool,
}

impl Playing {
    /// The body of a start or progress report.
    pub fn progress(&self, at: &Moment) -> PlaybackProgressInfo {
        let (audio, subtitle) = self.reported_streams(&at.tracks, at.aid, at.sid);
        PlaybackProgressInfo {
            item_id: self.item_id.clone(),
            media_source_id: Some(self.media_source_id.clone()),
            play_session_id: self.play_session_id.clone(),
            live_stream_id: self.live_stream_id.clone(),
            position_ticks: Some(seconds_to_ticks(at.position)),
            is_paused: at.paused,
            is_muted: at.muted,
            volume_level: Some(at.volume.round().clamp(0.0, 100.0) as i32),
            audio_stream_index: audio,
            subtitle_stream_index: subtitle,
            play_method: self.method,
            can_seek: at.seekable,
            repeat_mode: Some(
                if at.loop_file {
                    "RepeatOne"
                } else if at.loop_playlist {
                    "RepeatAll"
                } else {
                    "RepeatNone"
                }
                .into(),
            ),
            ..Default::default()
        }
    }

    /// The body of the stop report. `failed` when the file never played:
    /// the server then changes nothing of the account's.
    pub fn stopped(&self, position: f64, failed: bool) -> PlaybackStopInfo {
        PlaybackStopInfo {
            item_id: self.item_id.clone(),
            media_source_id: Some(self.media_source_id.clone()),
            play_session_id: self.play_session_id.clone(),
            live_stream_id: self.live_stream_id.clone(),
            position_ticks: seconds_to_ticks(position),
            failed,
            ..Default::default()
        }
    }
}

/// When progress is reported: every `PROGRESS_INTERVAL`, and soon after a
/// change, changes close together making one report.
#[derive(Debug, Clone, Default)]
pub struct Pacer {
    last: Option<Instant>,
    /// A change waits to be reported.
    changed: bool,
}

impl Pacer {
    /// A report went out.
    pub fn sent(&mut self, now: Instant) {
        self.last = Some(now);
        self.changed = false;
    }

    /// Something worth reporting changed: whether a report may go now;
    /// otherwise the next `due` makes it.
    pub fn change(&mut self, now: Instant) -> bool {
        let free = self
            .last
            .is_none_or(|last| now.saturating_duration_since(last) >= CHANGE_GAP);
        self.changed = !free;
        free
    }

    /// Asked about once a second: whether a report is due.
    pub fn due(&self, now: Instant) -> bool {
        let since = self
            .last
            .map_or(Duration::MAX, |last| now.saturating_duration_since(last));
        since >= PROGRESS_INTERVAL || (self.changed && since >= CHANGE_GAP)
    }
}

/// A report for the reporter to send.
#[derive(Debug, Clone, PartialEq)]
pub enum Report {
    Start(PlaybackStartInfo),
    Progress(PlaybackProgressInfo),
    Stopped(PlaybackStopInfo),
}

impl Report {
    fn play_session(&self) -> (&str, Option<&str>) {
        match self {
            Report::Start(info) | Report::Progress(info) => {
                (&info.item_id, info.play_session_id.as_deref())
            }
            Report::Stopped(info) => (&info.item_id, info.play_session_id.as_deref()),
        }
    }
}

/// What became of a report, for the session.
#[derive(Debug, Clone)]
pub struct Sent {
    pub server_id: String,
    /// The client it went with: a 401 means only that sign-in is gone.
    pub client: Client,
    pub stopped: bool,
    pub result: Result<(), Error>,
}

/// A report on its way, with the server and the client it goes to.
#[derive(Debug)]
struct Job {
    server_id: String,
    client: Client,
    report: Report,
}

/// Drops progress reports overtaken in the queue by a later report of the
/// same playback: a server slow to answer gets the latest position, not
/// every one since. How many were dropped.
fn coalesce(queue: &mut VecDeque<Job>) -> usize {
    let before = queue.len();
    let mut keep = vec![true; queue.len()];
    for (at, job) in queue.iter().enumerate() {
        if let Report::Progress(_) = job.report {
            let session = job.report.play_session();
            keep[at] = !queue.iter().skip(at + 1).any(|later| {
                later.server_id == job.server_id
                    && later.report.play_session() == session
                    && !matches!(later.report, Report::Start(_))
            });
        }
    }
    let mut at = 0;
    queue.retain(|_| {
        at += 1;
        keep[at - 1]
    });
    before - queue.len()
}

/// Sends the reports, in order, on a thread of its own: one per app, since
/// requests block and the interface thread mustn't wait on a server.
pub struct Reporter {
    sender: async_channel::Sender<Job>,
    pending: Pending,
}

/// The reports sent and not yet answered, which another thread can wait
/// for: signing out waits for them before the token stops working.
#[derive(Debug, Clone, Default)]
pub struct Pending(Arc<(Mutex<usize>, Condvar)>);

impl Pending {
    /// Waits until every report sent so far has been answered, or until
    /// `deadline`: whether they all were.
    pub fn wait(&self, deadline: Instant) -> bool {
        let (count, changed) = &*self.0;
        let mut count = count.lock().unwrap_or_else(PoisonError::into_inner);
        while *count > 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            count = changed
                .wait_timeout(count, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }

    fn add(&self) {
        *self.0.0.lock().unwrap_or_else(PoisonError::into_inner) += 1;
    }

    fn done(&self, reports: usize) {
        if reports == 0 {
            return;
        }
        let (count, changed) = &*self.0;
        let mut count = count.lock().unwrap_or_else(PoisonError::into_inner);
        *count = count.saturating_sub(reports);
        changed.notify_all();
    }
}

impl Reporter {
    /// Starts the reporter. What became of each report comes out of the
    /// receiver.
    pub fn start() -> std::io::Result<(Reporter, async_channel::Receiver<Sent>)> {
        let (sender, jobs) = async_channel::unbounded::<Job>();
        let (answers, answered) = async_channel::unbounded();
        let pending = Pending::default();
        let counted = pending.clone();
        std::thread::Builder::new()
            .name("jellyfin-reports".into())
            .spawn(move || {
                let mut queue = VecDeque::new();
                while let Ok(job) = jobs.recv_blocking() {
                    queue.push_back(job);
                    while let Ok(job) = jobs.try_recv() {
                        queue.push_back(job);
                    }
                    let dropped = coalesce(&mut queue);
                    counted.done(dropped);
                    while let Some(job) = queue.pop_front() {
                        let result = match &job.report {
                            Report::Start(info) => job.client.report_start(info),
                            Report::Progress(info) => job.client.report_progress(info),
                            Report::Stopped(info) => job.client.report_stopped(info),
                        };
                        if let Err(error) = &result {
                            log::debug!("reporting to Jellyfin: {error}");
                        }
                        let _ = answers.try_send(Sent {
                            server_id: job.server_id.clone(),
                            stopped: matches!(job.report, Report::Stopped(_)),
                            client: job.client,
                            result,
                        });
                        counted.done(1);
                    }
                }
            })?;
        Ok((Reporter { sender, pending }, answered))
    }

    pub fn send(&self, server_id: &str, client: &Client, report: Report) {
        self.pending.add();
        let job = Job {
            server_id: server_id.to_owned(),
            client: client.clone(),
            report,
        };
        if self.sender.try_send(job).is_err() {
            log::warn!("the Jellyfin reporter has stopped");
            self.pending.done(1);
        }
    }

    /// Waits until every report sent so far has been answered, or until
    /// `deadline`: whether they all were.
    pub fn flush(&self, deadline: Instant) -> bool {
        self.pending.wait(deadline)
    }

    pub fn pending(&self) -> Pending {
        self.pending.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jellyfin::auth::Identity;
    use crate::jellyfin::store;
    use std::io::{Read, Write};
    use url::Url;

    macro_rules! fixture {
        ($name:literal) => {
            serde_json::from_str(include_str!(concat!("fixtures/", $name, ".json"))).unwrap()
        };
    }

    fn client(address: &str) -> Client {
        Client::new(
            Url::parse(address).unwrap(),
            Identity::new(store::DEVICE_NAME, "fixture-device"),
            None,
        )
        .unwrap()
        .with_session("TOKEN", "u1")
    }

    fn item(json: &str) -> BaseItemDto {
        serde_json::from_str(json).unwrap()
    }

    fn info(title: &str) -> ItemInfo {
        ItemInfo {
            server_id: "srv".into(),
            id: "film".into(),
            title: title.into(),
            ..Default::default()
        }
    }

    fn asked<'a>(item: &'a BaseItemDto, info: &'a ItemInfo, choice: &'a Choice) -> Asked<'a> {
        Asked {
            entry: "jellyfin://srv/film",
            server_id: "srv",
            item,
            info,
            choice,
            resume_at: None,
        }
    }

    fn direct() -> Playing {
        let client = client("http://127.0.0.1:18096");
        let film = item(r#"{"Id": "film", "Name": "Night Signal", "Type": "Movie"}"#);
        let info = info("Night Signal");
        let choice = Choice::default();
        plan(
            &client,
            None,
            &asked(&film, &info, &choice),
            fixture!("playback_info"),
        )
        .unwrap()
        .playing
    }

    #[test]
    fn a_film_plays_directly_with_the_servers_subtitles_and_the_header_apart() {
        let client = client("http://127.0.0.1:18096");
        let film = item(
            r#"{"Id": "film", "Name": "Night Signal", "Type": "Movie", "ImageTags": {"Primary": "p1"},
                "UserData": {"PlaybackPositionTicks": 123456789, "Played": false}}"#,
        );
        let info = info("Night Signal");
        let choice = Choice::default();
        let planned = plan(
            &client,
            Some(Path::new("/certs/ca.pem")),
            &asked(&film, &info, &choice),
            fixture!("playback_info"),
        )
        .unwrap();
        let options = &planned.options;
        let answer: PlaybackInfoResponse = fixture!("playback_info");
        let source = &answer.media_sources[0];
        assert_eq!(
            options.stream,
            client.static_stream_url("film", false, source).as_str()
        );
        assert_eq!(
            options.get("force-media-title"),
            Some(&Value::from("Night Signal"))
        );
        assert_eq!(options.get("start"), Some(&Value::from("12.346")));
        assert_eq!(options.get("ytdl"), Some(&Value::Flag(false)));
        assert_eq!(
            options.get("save-position-on-quit"),
            Some(&Value::Flag(false))
        );
        assert_eq!(options.get("tls-verify"), Some(&Value::Flag(true)));
        assert_eq!(
            options.get("tls-ca-file"),
            Some(&Value::from("/certs/ca.pem"))
        );
        let Some(Value::List(subtitles)) = options.get("sub-files") else {
            panic!("no sub-files: {options:?}");
        };
        assert_eq!(subtitles.len(), 2);
        for url in subtitles {
            assert!(url.starts_with("http://127.0.0.1:18096/Videos/"), "{url}");
            assert!(!url.contains("TOKEN") && !url.contains("ApiKey"), "{url}");
        }
        // The file and the server's subtitle files need no token: mpv gets
        // no header, so none of it can go anywhere else.
        let shown = format!("{planned:?}");
        assert!(!shown.contains("TOKEN"), "{shown}");
        assert_eq!(options.header, None);
        assert!(options.stream_options.is_empty());
        assert_eq!(options.get("prefetch-playlist"), None);
        // The server's tracks are selected as the file opens, for it only:
        // English audio, and the first external subtitles, after the
        // file's own.
        assert_eq!(options.get("aid"), Some(&Value::Int(1)));
        assert_eq!(options.get("sid"), Some(&Value::Int(2)));

        let playing = &planned.playing;
        assert_eq!(playing.method, PlayMethod::DirectPlay);
        let external: Vec<i32> = playing.external.iter().map(|file| file.index).collect();
        assert_eq!(external, [0, 1]);
        assert_eq!(
            Some(&Value::List(
                playing
                    .external
                    .iter()
                    .map(|file| file.url.clone())
                    .collect()
            )),
            options.get("sub-files")
        );
        assert_eq!((playing.audio, playing.subtitle), (Some(3), Some(0)));
        assert_eq!(
            playing.play_session_id.as_deref(),
            answer.play_session_id.as_deref()
        );
        assert_eq!(playing.media_source_id, source.id);
        assert!((playing.start - 12.3456789).abs() < 1e-9);
        assert!(playing.shown.art_url.contains("/Items/film/Images/Primary"));
        assert!(!playing.shown.art_url.contains("TOKEN"));
    }

    #[test]
    fn where_to_start() {
        let client = client("http://127.0.0.1:18096");
        let started =
            r#"{"Id": "film", "Type": "Movie", "UserData": {"PlaybackPositionTicks": 600000000}}"#;
        let film = item(started);
        let choice = Choice::default();
        let start = |info: &ItemInfo, resume_at| {
            let asked = Asked {
                resume_at,
                ..asked(&film, info, &choice)
            };
            plan(&client, None, &asked, fixture!("playback_info"))
                .unwrap()
                .options
                .get("start")
                .cloned()
        };
        // No start of the server's is none rather than unset: a watch-later
        // file mpv wrote for the entry (read before the hook runs) would
        // decide otherwise.
        let none = Some(Value::from("none"));
        assert_eq!(start(&info(""), None), Some(Value::from("60.000")));
        let from_start = ItemInfo {
            from_start: true,
            ..info("")
        };
        assert_eq!(start(&from_start, None), none);
        // A reload picks up where it was, whatever the server says.
        assert_eq!(
            start(&from_start, Some(754.25)),
            Some(Value::from("754.250"))
        );
        assert_eq!(start(&info(""), Some(0.2)), none);
        let song = ItemInfo {
            audio: true,
            ..info("")
        };
        assert_eq!(start(&song, None), none);
        // A film watched to the end starts again from the beginning; one
        // watched again and stopped part-way through resumes there.
        let plain = info("");
        let start_of = |json: &str| {
            let film = item(json);
            plan(
                &client,
                None,
                &asked(&film, &plain, &choice),
                fixture!("playback_info"),
            )
            .unwrap()
            .options
            .get("start")
            .cloned()
        };
        assert_eq!(
            start_of(
                r#"{"Id": "film", "Type": "Movie",
                    "UserData": {"PlaybackPositionTicks": 0, "Played": true}}"#
            ),
            none
        );
        assert_eq!(
            start_of(
                r#"{"Id": "film", "Type": "Movie",
                    "UserData": {"PlaybackPositionTicks": 600000000, "Played": true}}"#
            ),
            Some(Value::from("60.000"))
        );
    }

    #[test]
    fn a_lower_quality_plays_the_transcode_without_the_token() {
        let client = client("http://127.0.0.1:18096");
        let film =
            item(r#"{"Id": "853911ffcbd536db7ac23dc25f283bde", "Name": "Hevc", "Type": "Movie"}"#);
        let info = ItemInfo {
            media_source_id: Some("853911ffcbd536db7ac23dc25f283bde".into()),
            ..info("Hevc")
        };
        let choice = Choice {
            max_bitrate: Some(2_000_000),
            ..Default::default()
        };
        let asked = asked(&film, &info, &choice);
        let request = asked.request(Some("u1"), &TranscodeRules::default());
        assert_eq!(request.max_streaming_bitrate, Some(2_000_000));
        assert_eq!(
            request
                .device_profile
                .as_ref()
                .unwrap()
                .max_streaming_bitrate,
            2_000_000
        );
        assert_eq!(request.user_id.as_deref(), Some("u1"));
        let planned = plan(&client, None, &asked, fixture!("playback_info_transcode")).unwrap();
        let stream = Url::parse(&planned.options.stream).unwrap();
        assert!(stream.path().ends_with("/master.m3u8"), "{stream}");
        let query = stream.query().unwrap();
        assert!(
            !query.contains("ApiKey") && !query.contains("TOKEN"),
            "{query}"
        );
        assert!(query.ends_with("&EnableTrickplay=false"), "{query}");
        // Only the transcode's playlists need the header, which mpv gets
        // for this file alone: it follows no redirect with it, and opens
        // no next entry with it ahead of time.
        let options = &planned.options;
        let Some(Secret(Value::List(header))) = &options.header else {
            panic!("no header for the transcode");
        };
        assert_eq!(header, &client.http_header_fields());
        assert_eq!(options.stream_options, [("max_redirects", "0")]);
        assert_eq!(options.get("prefetch-playlist"), Some(&Value::Flag(false)));
        let playing = &planned.playing;
        assert_eq!(playing.method, PlayMethod::Transcode);
        // The transcode carries one audio stream, the one asked for.
        assert_eq!(playing.audio, Some(1));
        assert_eq!(playing.track_id(1), Some((StreamKind::Audio, 1)));
        assert_eq!(playing.track_id(2), None);
        let track = MpvTrack {
            kind: StreamKind::Audio,
            id: 1,
            external: false,
            filename: None,
        };
        assert_eq!(playing.stream_index(&track), Some(1));
        assert_eq!(planned.options.get("sub-files"), None);
        assert_eq!(options.get("aid"), Some(&Value::Int(1)));
        // The source's other audio streams aren't in the transcode; the
        // Audio menu offers them, and the server makes another for one.
        let others: Vec<i32> = playing.other_audio().map(|stream| stream.index).collect();
        assert!(!others.contains(&1), "{others:?}");
        assert_eq!(
            others.len(),
            playing
                .streams
                .iter()
                .filter(|stream| stream.kind == StreamKind::Audio)
                .count()
                - 1
        );
    }

    #[test]
    fn choices_name_the_version_and_refusals_say_why() {
        let film = item(
            r#"{"Id": "film", "Type": "Movie", "MediaSources": [{"Id": "v1"}, {"Id": "v2"}]}"#,
        );
        let plain = info("");
        let none = Choice::default();
        let request = asked(&film, &plain, &none).request(None, &TranscodeRules::default());
        assert_eq!(request.media_source_id, None);
        assert_eq!(request.max_streaming_bitrate, None);
        // A stream chosen counts only with the version named: the first.
        let subtitle = Choice {
            subtitle: Some(4),
            ..Default::default()
        };
        let request = asked(&film, &plain, &subtitle).request(None, &TranscodeRules::default());
        assert_eq!(request.media_source_id.as_deref(), Some("v1"));
        assert_eq!(request.subtitle_stream_index, Some(4));
        let chosen = ItemInfo {
            media_source_id: Some("v2".into()),
            audio_stream_index: Some(2),
            ..info("")
        };
        let request = asked(&film, &chosen, &none).request(None, &TranscodeRules::default());
        assert_eq!(request.media_source_id.as_deref(), Some("v2"));
        assert_eq!(request.audio_stream_index, Some(2));

        let client = client("http://127.0.0.1:18096");
        let refused = PlaybackInfoResponse {
            error_code: Some("NotAllowed".into()),
            ..Default::default()
        };
        let failure = plan(&client, None, &asked(&film, &plain, &none), refused).unwrap_err();
        assert_eq!(failure, Failure::Refused("NotAllowed".into()));
        assert!(failure.message(None).contains("doesn't allow"));
        let empty = PlaybackInfoResponse::default();
        assert_eq!(
            plan(&client, None, &asked(&film, &plain, &none), empty).unwrap_err(),
            Failure::NoSource
        );
        assert_eq!(
            Failure::SignedOut.message(Some("Patina Test Server")),
            "Sign in to Patina Test Server to play this."
        );
    }

    #[test]
    fn a_version_chosen_in_the_player_drops_the_pages_streams() {
        let film = item(
            r#"{"Id": "film", "Type": "Movie", "MediaSources": [
                {"Id": "v1", "Name": "1080p"}, {"Id": "v2"}]}"#,
        );
        // Chosen on the page: the second version, its audio stream 2.
        let page = ItemInfo {
            media_source_id: Some("v2".into()),
            audio_stream_index: Some(2),
            subtitle_stream_index: Some(5),
            ..info("")
        };
        let rules = TranscodeRules::default();
        let first = Choice {
            media_source_id: Some("v1".into()),
            ..Default::default()
        };
        let request = asked(&film, &page, &first).request(None, &rules);
        assert_eq!(request.media_source_id.as_deref(), Some("v1"));
        // Indexes of v2's streams, which v1 numbers its own way.
        assert_eq!(request.audio_stream_index, None);
        assert_eq!(request.subtitle_stream_index, None);
        // The page's own version keeps its streams; streams chosen in the
        // player for the version it plays count either way.
        let same = Choice {
            media_source_id: Some("v2".into()),
            ..Default::default()
        };
        let request = asked(&film, &page, &same).request(None, &rules);
        assert_eq!(request.audio_stream_index, Some(2));
        let tracks = Choice {
            audio: Some(1),
            ..first.clone()
        };
        let request = asked(&film, &page, &tracks).request(None, &rules);
        assert_eq!(
            (request.audio_stream_index, request.subtitle_stream_index),
            (Some(1), None)
        );

        // The menu lists the item's versions, named as the page names them.
        let client = client("http://127.0.0.1:18096");
        let playing = plan(
            &client,
            None,
            &asked(&film, &page, &first),
            fixture!("playback_info"),
        )
        .unwrap()
        .playing;
        assert_eq!(
            playing.versions,
            [
                Version {
                    id: "v1".into(),
                    name: "1080p".into()
                },
                Version {
                    id: "v2".into(),
                    name: "Version 2".into()
                }
            ]
        );
    }

    #[test]
    fn the_preferences_name_what_the_server_converts() {
        let prefs = crate::settings::Preferences::default();
        assert_eq!(transcode_rules(&prefs), TranscodeRules::default());
        let prefs = crate::settings::Preferences {
            jellyfin_transcode_hevc: true,
            jellyfin_transcode_over_1080p: true,
            ..Default::default()
        };
        let rules = transcode_rules(&prefs);
        assert!(rules.hevc && rules.over_1080p && !rules.av1 && !rules.hdr);
        // The quality menu's bitrate goes with the rules into the profile.
        let film = item(r#"{"Id": "film", "Type": "Movie"}"#);
        let plain = info("");
        let low = Choice {
            max_bitrate: Some(4_000_000),
            ..Default::default()
        };
        let request = asked(&film, &plain, &low).request(None, &rules);
        let profile = serde_json::to_value(request.device_profile.unwrap()).unwrap();
        assert_eq!(profile["MaxStreamingBitrate"], 4_000_000);
        assert_eq!(
            profile["CodecProfiles"],
            serde_json::json!([
                {"Type": "Video", "Codec": "hevc", "Conditions": [
                    {"Condition": "LessThanEqual", "Property": "Width", "Value": "0", "IsRequired": true}]},
                {"Type": "Video", "Conditions": [
                    {"Condition": "LessThanEqual", "Property": "Width", "Value": "1920", "IsRequired": false},
                    {"Condition": "LessThanEqual", "Property": "Height", "Value": "1080", "IsRequired": false}]}
            ])
        );
    }

    #[test]
    fn trickplay_sizes_go_with_the_version_playing() {
        let client = client("http://127.0.0.1:18096");
        let answer: PlaybackInfoResponse = fixture!("playback_info");
        let source = answer.media_sources[0].id.clone();
        let film = item(&format!(
            r#"{{"Id": "film", "Type": "Movie", "RunTimeTicks": 300000000, "Trickplay": {{
                "{source}": {{
                    "640": {{"Width": 640, "Height": 360, "TileWidth": 10, "TileHeight": 10,
                             "ThumbnailCount": 15, "Interval": 2000}},
                    "320": {{"Width": 320, "Height": 180, "TileWidth": 10, "TileHeight": 10,
                             "ThumbnailCount": 15, "Interval": 2000}}}},
                "other": {{
                    "160": {{"Width": 160, "Height": 90, "TileWidth": 10, "TileHeight": 10,
                             "ThumbnailCount": 15, "Interval": 2000}}}}}}}}"#
        ));
        let plain = info("");
        let none = Choice::default();
        let playing = plan(&client, None, &asked(&film, &plain, &none), answer)
            .unwrap()
            .playing;
        let widths: Vec<u32> = playing.trickplay.iter().map(|info| info.width).collect();
        assert_eq!(widths, [320, 640]);
        // The version's own length, which the item's may round.
        assert!((playing.duration.unwrap() - 30.021).abs() < 1e-9);
    }

    /// mpv's tracks for the fixture's file as mpv opens it with both of
    /// the server's subtitle files loaded.
    fn tracks_of(playing: &Playing) -> Vec<MpvTrack> {
        let track = |kind, id, file: Option<&External>| MpvTrack {
            kind,
            id,
            external: file.is_some(),
            filename: file.map(|file| file.url.clone()),
        };
        vec![
            track(StreamKind::Video, 1, None),
            track(StreamKind::Audio, 1, None),
            track(StreamKind::Audio, 2, None),
            track(StreamKind::Subtitle, 1, None),
            track(StreamKind::Subtitle, 2, Some(&playing.external[0])),
            track(StreamKind::Subtitle, 3, Some(&playing.external[1])),
        ]
    }

    #[test]
    fn server_streams_map_to_mpvs_tracks_and_back() {
        // The fixture: external English and French subtitles (0, 1), then
        // the file's video (2), English and French audio (3, 4) and Spanish
        // subtitles (5).
        let playing = direct();
        assert_eq!(playing.track_id(3), Some((StreamKind::Audio, 1)));
        assert_eq!(playing.track_id(4), Some((StreamKind::Audio, 2)));
        assert_eq!(playing.track_id(5), Some((StreamKind::Subtitle, 1)));
        assert_eq!(playing.track_id(0), Some((StreamKind::Subtitle, 2)));
        assert_eq!(playing.track_id(1), Some((StreamKind::Subtitle, 3)));
        assert_eq!(playing.track_id(2), None);
        assert_eq!(playing.track_id(9), None);
        let tracks = tracks_of(&playing);
        for index in [0, 1, 3, 4, 5] {
            let id = playing.track_of(index, &tracks).unwrap();
            let kind = playing.track_id(index).unwrap().0;
            let track = tracks
                .iter()
                .find(|track| track.kind == kind && track.id == id)
                .unwrap();
            assert_eq!(playing.stream_index(track), Some(index));
        }
        // A subtitle the user added has no index on the server.
        let added = MpvTrack {
            kind: StreamKind::Subtitle,
            id: 4,
            external: true,
            filename: Some("/media/films/film.en.srt".into()),
        };
        assert_eq!(playing.stream_index(&added), None);
        let audio = |id| MpvTrack {
            kind: StreamKind::Audio,
            id,
            external: false,
            filename: None,
        };
        assert_eq!(playing.stream_index(&audio(3)), None);
        assert_eq!(playing.stream_index(&audio(0)), None);

        // As the hook asks for them, and as mpv has them.
        let english = Selection {
            aid: Pick::Track(1),
            sid: Pick::Track(2),
        };
        assert_eq!(playing.planned(), english);
        assert_eq!(playing.selection(&tracks), english);
        // Subtitles off on the server are off in mpv; a track mpv doesn't
        // have as the server said is left to mpv.
        let off = Playing {
            subtitle: Some(-1),
            audio: Some(4),
            ..playing.clone()
        };
        assert_eq!(
            off.selection(&tracks[..2]),
            Selection {
                aid: Pick::Auto,
                sid: Pick::Off
            }
        );
        assert_eq!(
            Playing {
                subtitle: None,
                ..playing.clone()
            }
            .planned()
            .sid,
            Pick::Off
        );
        let french = Playing {
            subtitle: Some(1),
            ..playing.clone()
        };
        assert_eq!(french.selection(&tracks[..5]).sid, Pick::Auto);
        assert_eq!(french.selection(&tracks).sid, Pick::Track(3));
        assert_eq!(Pick::Track(3).value(), Value::Int(3));
        assert_eq!(Pick::Off.value(), Value::from("no"));
        assert_eq!(Pick::Auto.value(), Value::from("auto"));

        assert_eq!(
            playing.reported_streams(&tracks, Some(2), Some(1)),
            (Some(4), Some(5))
        );
        assert_eq!(
            playing.reported_streams(&tracks, Some(1), None),
            (Some(3), Some(-1))
        );
        assert_eq!(
            playing.reported_streams(&tracks, None, Some(7)),
            (None, None)
        );
    }

    #[test]
    fn a_subtitle_file_that_failed_to_load_moves_no_other() {
        // English (0) didn't load: mpv's second subtitle is the French
        // file, which by its place the server would call English.
        let playing = direct();
        let mut tracks = tracks_of(&playing);
        tracks.remove(4);
        tracks[4].id = 2;
        let french = &tracks[4];
        assert_eq!(playing.stream_index(french), Some(1));
        assert_eq!(playing.track_of(1, &tracks), Some(2));
        assert_eq!(playing.track_of(0, &tracks), None);
        // The server's default, English, isn't there: mpv chooses, rather
        // than French in its place.
        assert_eq!(playing.planned().sid, Pick::Track(2));
        assert_eq!(playing.selection(&tracks).sid, Pick::Auto);
        assert_eq!(
            playing.reported_streams(&tracks, Some(1), Some(2)),
            (Some(3), Some(1))
        );
    }

    #[test]
    fn a_burned_in_subtitle_is_reported_and_kept_out_of_mpv() {
        let mut playing = direct();
        let pgs = MediaStream {
            kind: StreamKind::Subtitle,
            index: 6,
            codec: Some("pgssub".into()),
            delivery_method: Some(DeliveryMethod::Encode),
            ..Default::default()
        };
        playing.streams.push(pgs);
        assert_eq!(playing.burned_in(), None);
        assert_eq!(
            playing
                .burnable()
                .map(|stream| stream.index)
                .collect::<Vec<_>>(),
            [6]
        );
        playing.subtitle = Some(6);
        playing.method = PlayMethod::Transcode;
        assert_eq!(playing.burned_in(), Some(6));
        assert_eq!(playing.selection(&[]).sid, Pick::Off);
        assert_eq!(playing.planned().sid, Pick::Off);
        assert_eq!(playing.reported_streams(&[], Some(1), None).1, Some(6));
    }

    #[test]
    fn reports_carry_the_session_the_streams_and_the_position_in_ticks() {
        let playing = direct();
        let at = Moment {
            position: 61.5,
            paused: true,
            volume: 115.0,
            seekable: true,
            aid: Some(2),
            sid: Some(3),
            tracks: tracks_of(&playing),
            loop_playlist: true,
            ..Default::default()
        };
        let progress = playing.progress(&at);
        assert_eq!(progress.item_id, "film");
        assert_eq!(progress.position_ticks, Some(615_000_000));
        assert!(progress.is_paused && progress.can_seek && !progress.is_muted);
        assert_eq!(progress.volume_level, Some(100));
        assert_eq!(progress.audio_stream_index, Some(4));
        assert_eq!(progress.subtitle_stream_index, Some(1));
        assert_eq!(progress.play_method, PlayMethod::DirectPlay);
        assert_eq!(progress.repeat_mode.as_deref(), Some("RepeatAll"));
        assert_eq!(progress.play_session_id, playing.play_session_id);
        let json = serde_json::to_value(&progress).unwrap();
        assert_eq!(json["PositionTicks"], 615_000_000);
        assert_eq!(json["PlayMethod"], "DirectPlay");
        assert_eq!(json["MediaSourceId"], playing.media_source_id.as_str());

        let stopped = playing.stopped(-0.01, false);
        assert_eq!(stopped.position_ticks, 0);
        assert!(!stopped.failed);
        let json = serde_json::to_value(playing.stopped(90.0, true)).unwrap();
        assert_eq!(json["PositionTicks"], 900_000_000);
        assert_eq!(json["Failed"], true);
    }

    #[test]
    fn progress_goes_every_ten_seconds_and_soon_after_changes() {
        let start = Instant::now();
        let at = |seconds: f64| start + Duration::from_secs_f64(seconds);
        let mut pacer = Pacer::default();
        assert!(pacer.due(start));
        pacer.sent(start);
        assert!(!pacer.due(at(9.9)));
        assert!(pacer.due(at(10.0)));
        pacer.sent(at(10.0));
        // A pause long after the last report goes at once.
        assert!(pacer.change(at(12.0)));
        pacer.sent(at(12.0));
        // Seeks in a row: the first within a second waits, and the rest
        // join it in one report a second after the last.
        assert!(!pacer.change(at(12.3)));
        assert!(!pacer.change(at(12.6)));
        assert!(!pacer.due(at(12.9)));
        assert!(pacer.due(at(13.0)));
        pacer.sent(at(13.0));
        assert!(!pacer.due(at(14.0)));
        assert!(pacer.due(at(23.0)));
    }

    fn job(server: &str, report: Report) -> Job {
        Job {
            server_id: server.into(),
            client: client("http://127.0.0.1:18096"),
            report,
        }
    }

    #[test]
    fn a_slow_server_gets_the_latest_progress_only() {
        let progress = |session: &str, ticks| {
            Report::Progress(PlaybackProgressInfo {
                item_id: "film".into(),
                play_session_id: Some(session.into()),
                position_ticks: Some(ticks),
                ..Default::default()
            })
        };
        let stopped = |session: &str| {
            Report::Stopped(PlaybackStopInfo {
                item_id: "film".into(),
                play_session_id: Some(session.into()),
                ..Default::default()
            })
        };
        let start = |session: &str| {
            Report::Start(PlaybackProgressInfo {
                item_id: "film".into(),
                play_session_id: Some(session.into()),
                ..Default::default()
            })
        };
        let mut queue: VecDeque<Job> = [
            job("a", start("s1")),
            job("a", progress("s1", 1)),
            job("b", progress("s1", 2)),
            job("a", progress("s1", 3)),
            job("a", progress("s2", 4)),
            job("a", stopped("s1")),
            job("a", start("s2")),
        ]
        .into_iter()
        .collect();
        assert_eq!(coalesce(&mut queue), 2);
        let left: Vec<(&str, &Report)> = queue
            .iter()
            .map(|job| (job.server_id.as_str(), &job.report))
            .collect();
        assert_eq!(
            left,
            [
                ("a", &start("s1")),
                ("b", &progress("s1", 2)),
                ("a", &progress("s2", 4)),
                ("a", &stopped("s1")),
                ("a", &start("s2")),
            ]
        );
    }

    /// Answers `count` requests with 204, and hands over what each asked.
    fn canned(count: usize) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let thread = std::thread::spawn(move || {
            let mut asked = Vec::new();
            for _ in 0..count {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let read = stream.read(&mut buffer).unwrap();
                    request.extend_from_slice(&buffer[..read]);
                    let text = String::from_utf8_lossy(&request);
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let length = head
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|value| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if body.len() >= length {
                            break;
                        }
                    }
                    if read == 0 {
                        break;
                    }
                }
                stream
                    .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                    .unwrap();
                asked.push(String::from_utf8_lossy(&request).into_owned());
            }
            asked
        });
        (address, thread)
    }

    #[test]
    fn the_reporter_sends_in_order_and_says_when_it_is_done() {
        let (address, server) = canned(3);
        let answering = client(&address);
        let (reporter, answered) = Reporter::start().unwrap();
        let playing = direct();
        let at = Moment {
            position: 5.0,
            ..Default::default()
        };
        // One at a time: queued together, the progress report would give
        // way to the stop after it.
        for report in [
            Report::Start(playing.progress(&at)),
            Report::Progress(playing.progress(&at)),
            Report::Stopped(playing.stopped(7.0, false)),
        ] {
            reporter.send("srv", &answering, report);
            assert!(reporter.flush(Instant::now() + Duration::from_secs(10)));
        }
        let asked = server.join().unwrap();
        let lines: Vec<&str> = asked
            .iter()
            .map(|request| request.lines().next().unwrap())
            .collect();
        assert_eq!(
            lines,
            [
                "POST /Sessions/Playing HTTP/1.1",
                "POST /Sessions/Playing/Progress HTTP/1.1",
                "POST /Sessions/Playing/Stopped HTTP/1.1",
            ]
        );
        assert!(
            asked[2].contains("\"PositionTicks\":70000000"),
            "{}",
            asked[2]
        );
        for request in &asked {
            let header = request
                .lines()
                .find(|line| line.to_ascii_lowercase().starts_with("authorization:"))
                .unwrap();
            assert!(header.contains("Token=\"TOKEN\""));
        }
        let sent: Vec<Sent> = (0..3).map(|_| answered.recv_blocking().unwrap()).collect();
        assert!(
            sent.iter()
                .all(|sent| sent.result.is_ok() && sent.server_id == "srv")
        );
        assert_eq!(
            sent.iter().map(|sent| sent.stopped).collect::<Vec<_>>(),
            [false, false, true]
        );

        // Nothing listening: the report fails, and is done all the same.
        let gone = client("http://127.0.0.1:9");
        reporter.send("srv", &gone, Report::Stopped(playing.stopped(1.0, false)));
        assert!(reporter.flush(Instant::now() + Duration::from_secs(15)));
        assert!(answered.recv_blocking().unwrap().result.is_err());
    }

    #[test]
    fn playing_from_the_beginning_holds_for_one_load() {
        let registry = Registry::default();
        let entry = "jellyfin://srv/film";
        session::remember(
            &mut registry.lock().unwrap(),
            vec![(
                entry.to_owned(),
                ItemInfo {
                    from_start: true,
                    media_source_id: Some("v2".into()),
                    ..info("Night Signal")
                },
            )],
        );
        let first = take_known(&registry, entry).unwrap();
        assert!(first.from_start);
        // Opened again from History or Recent: where the server says, with
        // the rest of what the library knew.
        let again = take_known(&registry, entry).unwrap();
        assert!(!again.from_start);
        assert_eq!(again.media_source_id.as_deref(), Some("v2"));
        assert_eq!(take_known(&registry, "jellyfin://srv/other"), None);
    }

    #[test]
    fn a_reloads_streams_go_with_that_load_and_the_quality_with_every_one() {
        let hook = Hook {
            servers: Servers::default(),
            registry: Registry::default(),
            versions: HashMap::from([("jellyfin://srv/film".to_owned(), "v2".to_owned())]),
            max_bitrate: Some(2_000_000),
            rules: TranscodeRules::default(),
            resolved: Resolved::default(),
        };
        let reload = Reload {
            at: 61.0,
            audio: Some(4),
            subtitle: Some(5),
        };
        assert_eq!(
            choice_for("jellyfin://srv/film", &hook, Some(&reload)),
            Choice {
                max_bitrate: Some(2_000_000),
                media_source_id: Some("v2".into()),
                audio: Some(4),
                subtitle: Some(5),
            }
        );
        // Played again, the streams chosen on its page (or the server's)
        // count; the next episode keeps the quality.
        assert_eq!(
            choice_for("jellyfin://srv/film", &hook, None),
            Choice {
                max_bitrate: Some(2_000_000),
                media_source_id: Some("v2".into()),
                ..Default::default()
            }
        );
        assert_eq!(
            choice_for("jellyfin://srv/next", &hook, None),
            Choice {
                max_bitrate: Some(2_000_000),
                ..Default::default()
            }
        );
        let film = item(r#"{"Id": "film", "Type": "Movie", "MediaSources": [{"Id": "v1"}]}"#);
        let page = ItemInfo {
            subtitle_stream_index: Some(0),
            ..info("")
        };
        let played_again = choice_for("jellyfin://srv/film", &hook, None);
        let request = asked(&film, &page, &played_again).request(None, &TranscodeRules::default());
        assert_eq!(request.subtitle_stream_index, None, "another version's");
        let reloaded = choice_for("jellyfin://srv/film", &hook, Some(&reload));
        let request = asked(&film, &page, &reloaded).request(None, &TranscodeRules::default());
        assert_eq!(request.subtitle_stream_index, Some(5));
    }

    #[test]
    fn our_stream_options_join_the_users() {
        let users = vec![
            ("reconnect".to_owned(), "1".to_owned()),
            ("max_redirects".to_owned(), "5".to_owned()),
        ];
        assert_eq!(
            added_to(users, &[("max_redirects", "0")]),
            [
                ("reconnect".to_owned(), "1".to_owned()),
                ("max_redirects".to_owned(), "0".to_owned())
            ]
        );
    }

    /// Opens a file in `mpv` through its load hook, with `options` applied
    /// as the hook applies a Jellyfin entry's: what `apply` said.
    fn open_with(mpv: &Mpv, options: &LoadOptions) -> Result<(), Failure> {
        use crate::mpv::Event;
        mpv.command(&["loadfile", "jellyfin://srv/film"]).unwrap();
        let mut applied = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match mpv.wait_event(0.5) {
                Some(Event::Hook { id, .. }) => {
                    let result = apply(mpv, options);
                    let failed = result.is_err();
                    applied = Some(result);
                    mpv.hook_continue(id).unwrap();
                    if failed {
                        break;
                    }
                }
                Some(Event::PlaybackRestart) => break,
                Some(Event::EndFile { .. }) if applied.is_some() => break,
                _ => {}
            }
        }
        applied.expect("no load hook")
    }

    fn stop(mpv: &Mpv) {
        use crate::mpv::{Event, testing};
        mpv.command(&["stop"]).unwrap();
        testing::wait_for(mpv, |event| matches!(event, Event::EndFile { .. }));
    }

    fn header_options() -> LoadOptions {
        LoadOptions {
            stream: "av://lavfi:sine=frequency=440:duration=30".into(),
            header: Some(Secret(Value::List(vec![
                "Authorization: MediaBrowser Token=\"secret-123\"".into(),
            ]))),
            stream_options: vec![("max_redirects", "0")],
            options: vec![
                ("aid", Value::Int(1)),
                ("sid", "no".into()),
                ("prefetch-playlist", false.into()),
            ],
        }
    }

    /// What `apply` sets ends with the file: the header, FFmpeg's options
    /// and levels, and the tracks, whatever was switched while it played.
    #[test]
    fn what_the_hook_sets_ends_with_the_file() {
        let Some(mpv) = crate::mpv::testing::headless() else {
            return;
        };
        mpv.hook_add(1, "on_load", -50).unwrap();
        mpv.set_property(
            "stream-lavf-o",
            Value::Map(vec![("reconnect".into(), "1".into())]),
        )
        .unwrap();
        mpv.set_property("msg-level", "all=trace").unwrap();
        open_with(&mpv, &header_options()).unwrap();
        let map = |name| key_values(&mpv, name);
        assert_eq!(
            map("stream-lavf-o"),
            [
                ("reconnect".to_owned(), "1".to_owned()),
                ("max_redirects".to_owned(), "0".to_owned())
            ]
        );
        // FFmpeg would print the header at trace.
        assert_eq!(
            map("msg-level"),
            [
                ("all".to_owned(), "trace".to_owned()),
                ("ffmpeg".to_owned(), "debug".to_owned())
            ]
        );
        assert_eq!(
            mpv.get_property("http-header-fields")
                .unwrap()
                .as_array()
                .len(),
            1
        );
        assert_eq!(
            mpv.get_property("prefetch-playlist").unwrap(),
            crate::mpv::Node::Flag(false)
        );
        // The user picks another track while it plays.
        mpv.set_property("sid", "auto").unwrap();
        mpv.set_property("aid", "no").unwrap();
        stop(&mpv);
        assert_eq!(
            map("stream-lavf-o"),
            [("reconnect".to_owned(), "1".to_owned())]
        );
        assert_eq!(map("msg-level"), [("all".to_owned(), "trace".to_owned())]);
        assert!(
            mpv.get_property("http-header-fields")
                .unwrap()
                .as_array()
                .is_empty()
        );
        for track in ["aid", "sid"] {
            assert_eq!(
                mpv.get_property(&format!("options/{track}"))
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .as_deref(),
                Some("auto"),
                "{track}"
            );
        }
    }

    /// mpv's log file records every property set, whatever the levels: a
    /// stream that needs the header doesn't get it while mpv writes one.
    #[test]
    fn the_header_never_goes_to_mpv_while_it_writes_a_log_file() {
        let Some(mpv) = crate::mpv::testing::headless() else {
            return;
        };
        mpv.hook_add(1, "on_load", -50).unwrap();
        let dir = std::env::temp_dir().join(format!("patina-log-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("mpv.log");
        mpv.set_property("log-file", log.to_string_lossy().as_ref())
            .unwrap();
        assert_eq!(open_with(&mpv, &header_options()), Err(Failure::Logging));
        assert!(Failure::Logging.message(None).contains("log file"));
        stop(&mpv);
        // Played as it is, a file needs no header and plays on.
        let direct = LoadOptions {
            header: None,
            stream_options: Vec::new(),
            ..header_options()
        };
        open_with(&mpv, &direct).unwrap();
        stop(&mpv);
        mpv.set_property("log-file", "").unwrap();
        let written = std::fs::read_to_string(&log).unwrap();
        assert!(written.contains("stream-open-filename"), "nothing logged");
        assert!(
            !written.contains("secret-123"),
            "the log file has the token"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_hook_waits_for_saved_sign_ins_only_while_they_load() {
        let servers = Servers::default();
        let server = Server {
            id: "srv".into(),
            client: client("http://127.0.0.1:18096"),
            ca_file: None,
        };
        let waiting = {
            let servers = servers.clone();
            std::thread::spawn(move || servers.wait_for("srv", Duration::from_secs(10)))
        };
        std::thread::sleep(Duration::from_millis(50));
        servers.set(server.clone());
        assert_eq!(
            waiting.join().unwrap().map(|server| server.id),
            Some(server.id.clone())
        );
        assert!(
            servers
                .wait_for("other", Duration::from_millis(20))
                .is_none()
        );
        servers.loaded();
        let asked = Instant::now();
        assert!(servers.wait_for("other", Duration::from_secs(10)).is_none());
        assert!(asked.elapsed() < Duration::from_secs(1));
        servers.remove("srv");
        assert!(servers.wait_for("srv", Duration::from_secs(10)).is_none());
    }
}
