//! What Patina keeps for the YouTube section: `youtube.json` in the data
//! folder, with the channels subscribed to, Watch Later, how far videos were
//! watched and where downloads went. No account is involved, and these lists
//! don't appear on YouTube.
//!
//! This file is the only copy of what the user built up, so it is never
//! silently replaced. A file that can't be read whole, or that a later Patina
//! wrote, is copied beside itself first, what can be read of it is kept, row by
//! row, and the user is told (`Store::problem`). A file that can't even be
//! looked at, or copied, stops saving altogether.
//!
//! The feed is not here: it can always be fetched again (`feed.rs`).

use super::{ChannelId, ChannelInfo, Video, VideoId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The file's version, which goes up when an older Patina couldn't read it.
const SCHEMA: u64 = 1;
/// The most that lists may hold. Far past real use, it bounds what a hostile
/// import can make Patina fetch and draw.
const MAX_CHANNELS: usize = 5_000;
pub(super) const MAX_SAVED: usize = 20_000;
/// The most watch marks kept. The oldest go first.
const MAX_MARKS: usize = 20_000;
/// How much of a video has to have been seen for it to count as watched, which
/// leaves room for an end card and doesn't count merely opening it.
const WATCHED_SHARE: f64 = 0.8;
/// How many copies of bad files are kept.
const MAX_COPIES: u32 = 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Channel {
    pub id: ChannelId,
    /// Empty until something has said: an imported row can have none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// With the `@`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub followers: Option<u64>,
    #[serde(default)]
    pub added_at: i64,
}

impl Channel {
    pub fn new(id: ChannelId) -> Self {
        Self {
            id,
            title: String::new(),
            handle: None,
            avatar: None,
            followers: None,
            added_at: 0,
        }
    }
}

/// A video on Watch Later. The name `saved` keeps it apart from mpv's own
/// "watch later" files, which hold resume positions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedVideo {
    #[serde(flatten)]
    pub video: Video,
    /// Seconds since the Unix epoch, kept so that an export can give it back.
    #[serde(default)]
    pub added_at: i64,
    /// How often its details have been asked for and not come.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub tries: u8,
    /// YouTube says it can't be watched. It stays, marked, in case that changes.
    #[serde(default, skip_serializing_if = "is_false")]
    pub unavailable: bool,
}

fn is_zero(value: &u8) -> bool {
    *value == 0
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl SavedVideo {
    pub fn new(video: Video, added_at: i64) -> Self {
        Self {
            video,
            added_at,
            tries: 0,
            unavailable: false,
        }
    }
}

/// How much of a video has been seen.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Mark {
    /// Where it stopped, in seconds, for the progress strip.
    pub position: f64,
    pub duration: f64,
    /// How many seconds have actually been played, which a seek past the end
    /// doesn't add to.
    #[serde(default)]
    pub seen: f64,
    #[serde(default)]
    pub watched: bool,
    /// When this was last changed, which decides what goes first.
    #[serde(default)]
    pub at: i64,
}

impl Mark {
    /// From 0 to 1, for the progress strip.
    pub fn progress(&self) -> f64 {
        if self.watched {
            return 1.0;
        }
        if self.duration > 0.0 {
            (self.position / self.duration).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
struct Saved {
    schema: u64,
    channels: Vec<Channel>,
    saved: Vec<SavedVideo>,
    marks: BTreeMap<VideoId, Mark>,
    /// Paths as text: a path that isn't valid text would stop the whole file
    /// being written, and its row is only a pointer to a file.
    downloads: BTreeMap<VideoId, String>,
    /// Whether the feed shows Shorts.
    show_shorts: bool,
}

/// What went wrong reading the file, to tell the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// It isn't JSON, or not the shape of this file.
    Corrupt { copy: Option<PathBuf> },
    /// A later Patina wrote it.
    Newer { version: u64, copy: Option<PathBuf> },
    /// Some rows couldn't be read, and were left out.
    Partial { rows: usize, copy: Option<PathBuf> },
    /// It couldn't be read at all, so nothing is known of what it holds.
    Unreadable(String),
}

impl Problem {
    fn copy(&self) -> Option<&Path> {
        match self {
            Self::Corrupt { copy } | Self::Newer { copy, .. } | Self::Partial { copy, .. } => {
                copy.as_deref()
            }
            Self::Unreadable(_) => None,
        }
    }

    /// Nothing may be written over the file. Without a copy to fall back on,
    /// the next save would be the end of what it held.
    fn stops_saving(&self) -> bool {
        matches!(self, Self::Unreadable(_)) || self.copy().is_none()
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Corrupt { .. } => {
                formatter.write_str("Your YouTube lists could not be read, so they look empty.")?
            }
            Self::Newer { .. } => formatter.write_str(
                "Your YouTube lists were saved by a newer Patina. \
                 Some of what they hold may be missing here.",
            )?,
            Self::Partial { rows, .. } => write!(
                formatter,
                "{rows} rows of your YouTube lists could not be read and were left out."
            )?,
            Self::Unreadable(error) => {
                return write!(
                    formatter,
                    "Your YouTube lists could not be opened ({error}). \
                     Changes won't be saved until they can."
                );
            }
        }
        match self.copy() {
            Some(copy) => write!(formatter, " A copy is kept at {}.", copy.display()),
            None => formatter
                .write_str(" No copy could be made, so changes won't be saved over the file."),
        }
    }
}

/// How an addition of several rows went.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Added {
    pub added: usize,
    pub present: usize,
    /// Past the list's limit.
    pub skipped: usize,
}

/// `youtube.json`, loaded. Changes are made in memory and written by `save`.
#[derive(Debug, Clone)]
pub struct Store {
    path: PathBuf,
    saved: Saved,
    problem: Option<Problem>,
}

impl Store {
    pub fn load(path: PathBuf) -> Self {
        let (saved, problem) = match std::fs::read(&path) {
            Ok(bytes) => read(&path, &bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Saved::default(), None),
            Err(error) => {
                log::warn!("could not read {}: {error}", path.display());
                (
                    Saved::default(),
                    Some(Problem::Unreadable(error.to_string())),
                )
            }
        };
        if let Some(problem) = &problem {
            log::warn!("{}: {problem}", path.display());
        }
        Self {
            path,
            saved,
            problem,
        }
    }

    /// What the user should be told about how the file read.
    pub fn problem(&self) -> Option<&Problem> {
        self.problem.as_ref()
    }

    /// Writes the file, whole or not at all.
    pub fn save(&mut self) -> std::io::Result<()> {
        if let Some(problem) = self
            .problem
            .as_ref()
            .filter(|problem| problem.stops_saving())
        {
            return Err(std::io::Error::other(problem.to_string()));
        }
        self.saved.schema = SCHEMA;
        let bytes = serde_json::to_vec_pretty(&self.saved).map_err(std::io::Error::other)?;
        write_atomically(&self.path, &bytes)?;
        // It reads whole from now on, and its copy has been kept.
        self.problem = None;
        Ok(())
    }

    pub fn channels(&self) -> &[Channel] {
        &self.saved.channels
    }

    pub fn is_subscribed(&self, id: &ChannelId) -> bool {
        self.saved.channels.iter().any(|channel| channel.id == *id)
    }

    /// Adds channels, or fills in what an existing row lacks. A channel keeps
    /// its place and when it was added.
    pub fn add_channels(&mut self, rows: Vec<Channel>) -> Added {
        let mut counts = Added::default();
        let mut places: HashMap<ChannelId, usize> = self
            .saved
            .channels
            .iter()
            .enumerate()
            .map(|(place, channel)| (channel.id.clone(), place))
            .collect();
        for row in rows {
            match places.get(&row.id) {
                Some(&place) => {
                    counts.present += 1;
                    let known = &mut self.saved.channels[place];
                    if known.title.is_empty() {
                        known.title = row.title;
                    }
                    known.handle = known.handle.take().or(row.handle);
                    known.avatar = known.avatar.take().or(row.avatar);
                    known.followers = known.followers.or(row.followers);
                }
                None if self.saved.channels.len() >= MAX_CHANNELS => counts.skipped += 1,
                None => {
                    counts.added += 1;
                    places.insert(row.id.clone(), self.saved.channels.len());
                    self.saved.channels.push(row);
                }
            }
        }
        counts
    }

    /// Takes in what a feed or yt-dlp says of a channel subscribed to; true if
    /// that changed anything. An empty answer changes nothing.
    pub fn update_channel(&mut self, info: &ChannelInfo) -> bool {
        let Some(known) = self
            .saved
            .channels
            .iter_mut()
            .find(|known| known.id == info.id)
        else {
            return false;
        };
        let before = known.clone();
        if !info.title.is_empty() {
            known.title.clone_from(&info.title);
        }
        known.handle = info.handle.clone().or(known.handle.take());
        known.avatar = info.avatar.clone().or(known.avatar.take());
        known.followers = info.followers.or(known.followers);
        *known != before
    }

    pub fn remove_channel(&mut self, id: &ChannelId) -> bool {
        let before = self.saved.channels.len();
        self.saved.channels.retain(|channel| channel.id != *id);
        self.saved.channels.len() != before
    }

    /// Watch Later, in the order rows were added.
    pub fn saved(&self) -> &[SavedVideo] {
        &self.saved.saved
    }

    pub fn is_saved(&self, id: &VideoId) -> bool {
        self.saved.saved.iter().any(|saved| saved.video.id == *id)
    }

    /// Adds videos, or fills in what an existing row lacks. A video keeps when
    /// it was first added. Importing one again that has no title yet gives it
    /// fresh tries: this is the one way to ask again for a row given up on.
    pub fn add_saved(&mut self, rows: Vec<SavedVideo>, now: i64) -> Added {
        let mut counts = Added::default();
        let mut places: HashMap<VideoId, usize> = self
            .saved
            .saved
            .iter()
            .enumerate()
            .map(|(place, saved)| (saved.video.id.clone(), place))
            .collect();
        for row in rows {
            match places.get(&row.video.id) {
                Some(&place) => {
                    counts.present += 1;
                    let known = &mut self.saved.saved[place];
                    known.video.absorb(row.video, now);
                    if known.video.title.is_empty() {
                        known.tries = 0;
                    }
                }
                None if self.saved.saved.len() >= MAX_SAVED => counts.skipped += 1,
                None => {
                    counts.added += 1;
                    places.insert(row.video.id.clone(), self.saved.saved.len());
                    self.saved.saved.push(row);
                }
            }
        }
        counts
    }

    pub fn remove_saved(&mut self, id: &VideoId) -> bool {
        let before = self.saved.saved.len();
        self.saved.saved.retain(|saved| saved.video.id != *id);
        self.saved.saved.len() != before
    }

    /// The rows still waiting for their details, up to `tries` attempts each,
    /// the newest added first: those are the ones at the top of the list. A
    /// long list takes hours, so its order is what the user sees of that.
    pub fn saved_without_details(&self, tries: u8) -> Vec<VideoId> {
        let mut rows: Vec<&SavedVideo> = self
            .saved
            .saved
            .iter()
            .filter(|saved| saved.video.title.is_empty() && !saved.unavailable)
            .filter(|saved| saved.tries < tries)
            .collect();
        rows.sort_by_key(|saved| std::cmp::Reverse(saved.added_at));
        rows.into_iter()
            .map(|saved| saved.video.id.clone())
            .collect()
    }

    /// Takes in a saved video's details, when they have come.
    pub fn saved_details(&mut self, details: Video, now: i64) {
        if let Some(saved) = self
            .saved
            .saved
            .iter_mut()
            .find(|saved| saved.video.id == details.id)
        {
            saved.video.absorb(details, now);
            saved.tries = 0;
            saved.unavailable = false;
        }
    }

    /// Notes that asking for a saved video's details failed.
    pub fn saved_tried(&mut self, id: &VideoId, unavailable: bool) {
        if let Some(saved) = self
            .saved
            .saved
            .iter_mut()
            .find(|saved| saved.video.id == *id)
        {
            saved.tries = saved.tries.saturating_add(1);
            saved.unavailable |= unavailable;
        }
    }

    pub fn mark(&self, id: &VideoId) -> Option<&Mark> {
        self.saved.marks.get(id)
    }

    /// Notes how far a video got and how many more seconds of it were really
    /// played. It counts as watched by those seconds, not by being opened or
    /// by where it stopped, and stays watched once it does.
    pub fn note_watch(
        &mut self,
        id: &VideoId,
        position: f64,
        duration: f64,
        played: f64,
        now: i64,
    ) {
        let number = |value: f64| {
            if value.is_finite() {
                value.max(0.0)
            } else {
                0.0
            }
        };
        let (position, duration, played) = (number(position), number(duration), number(played));
        let mark = self.saved.marks.entry(id.clone()).or_insert(Mark {
            position: 0.0,
            duration: 0.0,
            seen: 0.0,
            watched: false,
            at: now,
        });
        mark.position = position;
        if duration > 0.0 {
            mark.duration = duration;
        }
        mark.seen = (mark.seen + played).min(mark.duration.max(mark.seen));
        mark.watched |= mark.duration > 0.0 && mark.seen >= mark.duration * WATCHED_SHARE;
        mark.at = now;
        self.trim_marks();
    }

    /// Marks a video by hand, either way.
    pub fn set_watched(&mut self, id: &VideoId, watched: bool, now: i64) {
        if watched {
            let mark = self.saved.marks.entry(id.clone()).or_insert(Mark {
                position: 0.0,
                duration: 0.0,
                seen: 0.0,
                watched: true,
                at: now,
            });
            mark.watched = true;
            mark.at = now;
            self.trim_marks();
        } else {
            self.saved.marks.remove(id);
        }
    }

    fn trim_marks(&mut self) {
        if self.saved.marks.len() <= MAX_MARKS {
            return;
        }
        let mut ages: Vec<i64> = self.saved.marks.values().map(|mark| mark.at).collect();
        ages.sort_unstable();
        let cut = ages[self.saved.marks.len() - MAX_MARKS];
        self.saved.marks.retain(|_, mark| mark.at >= cut);
    }

    /// Forgets where videos were left off, keeping what was seen of them. False
    /// if there was nothing to forget.
    pub fn clear_positions(&mut self) -> bool {
        let mut any = false;
        for mark in self.saved.marks.values_mut() {
            any |= mark.position != 0.0;
            mark.position = 0.0;
        }
        any
    }

    /// Clearing Recent clears these too: it does what someone clearing their
    /// traces expects. False if there was nothing to clear.
    pub fn clear_marks(&mut self) -> bool {
        let any = !self.saved.marks.is_empty();
        self.saved.marks.clear();
        any
    }

    /// Where a video was downloaded to, while that file is there.
    pub fn download(&self, id: &VideoId) -> Option<&str> {
        self.saved
            .downloads
            .get(id)
            .map(String::as_str)
            .filter(|path| Path::new(path).is_file())
    }

    /// Which video a file downloaded here is.
    pub fn video_at(&self, path: &str) -> Option<VideoId> {
        self.saved
            .downloads
            .iter()
            .find(|(_, file)| *file == path)
            .map(|(id, _)| id.clone())
    }

    pub fn set_download(&mut self, id: &VideoId, path: &Path) {
        self.saved
            .downloads
            .insert(id.clone(), path.to_string_lossy().into_owned());
    }

    pub fn remove_download(&mut self, id: &VideoId) -> bool {
        self.saved.downloads.remove(id).is_some()
    }

    pub fn show_shorts(&self) -> bool {
        self.saved.show_shorts
    }

    pub fn set_show_shorts(&mut self, show: bool) {
        self.saved.show_shorts = show;
    }
}

/// The contents of a file that is there, however good, and what to say.
fn read(path: &Path, bytes: &[u8]) -> (Saved, Option<Problem>) {
    let value = match serde_json::from_slice::<Value>(bytes) {
        Ok(value @ Value::Object(_)) => value,
        Ok(_) | Err(_) => {
            let copy = keep_copy(path, bytes);
            return (Saved::default(), Some(Problem::Corrupt { copy }));
        }
    };
    let version = value
        .get("schema")
        .and_then(Value::as_u64)
        .unwrap_or(SCHEMA);
    let (saved, dropped) = rows_of(&value);
    if version > SCHEMA {
        let copy = keep_copy(path, bytes);
        return (saved, Some(Problem::Newer { version, copy }));
    }
    if dropped > 0 {
        let copy = keep_copy(path, bytes);
        return (
            saved,
            Some(Problem::Partial {
                rows: dropped,
                copy,
            }),
        );
    }
    (saved, None)
}

/// What can be read of the file, row by row, and how many rows couldn't be.
fn rows_of(value: &Value) -> (Saved, usize) {
    let mut dropped = 0;
    let mut list = |key: &str| -> Vec<Value> {
        match value.get(key) {
            Some(Value::Array(rows)) => rows.clone(),
            None | Some(Value::Null) => Vec::new(),
            Some(_) => {
                dropped += 1;
                Vec::new()
            }
        }
    };
    let channel_rows = list("channels");
    let saved_rows = list("saved");
    let mut seen = HashSet::new();
    let mut channels: Vec<Channel> = Vec::with_capacity(channel_rows.len());
    for row in channel_rows {
        match serde_json::from_value::<Channel>(row) {
            Ok(channel) => {
                if seen.insert(channel.id.clone()) {
                    channels.push(channel);
                }
            }
            Err(_) => dropped += 1,
        }
    }
    let mut seen = HashSet::new();
    let mut saved: Vec<SavedVideo> = Vec::with_capacity(saved_rows.len());
    for row in saved_rows {
        match serde_json::from_value::<SavedVideo>(row) {
            Ok(video) => {
                if seen.insert(video.video.id.clone()) {
                    saved.push(video);
                }
            }
            Err(_) => dropped += 1,
        }
    }
    let mut map = |key: &str| -> serde_json::Map<String, Value> {
        match value.get(key) {
            Some(Value::Object(rows)) => rows.clone(),
            None | Some(Value::Null) => serde_json::Map::new(),
            Some(_) => {
                dropped += 1;
                serde_json::Map::new()
            }
        }
    };
    let mark_rows = map("marks");
    let download_rows = map("downloads");
    let mut marks = BTreeMap::new();
    for (id, row) in mark_rows {
        match (VideoId::parse(&id), serde_json::from_value::<Mark>(row)) {
            (Some(id), Ok(mark)) => {
                marks.insert(id, mark);
            }
            _ => dropped += 1,
        }
    }
    let mut downloads = BTreeMap::new();
    for (id, row) in download_rows {
        match (VideoId::parse(&id), row.as_str()) {
            (Some(id), Some(path)) => {
                downloads.insert(id, path.to_owned());
            }
            _ => dropped += 1,
        }
    }
    let show_shorts = match value.get("show_shorts") {
        Some(Value::Bool(show)) => *show,
        None | Some(Value::Null) => false,
        Some(_) => {
            dropped += 1;
            false
        }
    };
    (
        Saved {
            schema: SCHEMA,
            channels,
            saved,
            marks,
            downloads,
            show_shorts,
        },
        dropped,
    )
}

/// A copy of a file about to be saved over: `youtube.json.bad`, or a numbered
/// one if an earlier copy, which may hold something else, is in the way. The
/// same contents are copied only once, since a bad file is read every start.
fn keep_copy(path: &Path, bytes: &[u8]) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    for number in 0..MAX_COPIES {
        let copy = path.with_file_name(if number == 0 {
            format!("{name}.bad")
        } else {
            format!("{name}.bad.{number}")
        });
        match std::fs::read(&copy) {
            Ok(existing) if existing == bytes => return Some(copy),
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                log::warn!("could not look at {}: {error}", copy.display());
                continue;
            }
        }
        return match write_atomically(&copy, bytes) {
            Ok(()) => Some(copy),
            Err(error) => {
                log::warn!("could not keep a copy as {}: {error}", copy.display());
                None
            }
        };
    }
    log::warn!("too many copies of {} are kept already", path.display());
    None
}

/// Writes a file so that it is either as it was or wholly new, even if Patina
/// or the computer stops halfway: to a temporary file beside it, brought to the
/// disk, then renamed into place.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    let written = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lists aren't all a save would wipe: these are counted as well, which
    /// brings the copy and the notice with them.
    #[test]
    fn every_part_of_the_wrong_kind_counts() {
        let folder =
            std::env::temp_dir().join(format!("patina-store-kinds-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("youtube.json");
        let text = r#"{"schema":1,"marks":["a"],"downloads":7,"show_shorts":"yes"}"#;
        std::fs::write(&path, text).unwrap();
        let store = Store::load(path.clone());
        let Some(Problem::Partial {
            rows,
            copy: Some(copy),
        }) = store.problem()
        else {
            panic!("{:?}", store.problem());
        };
        assert_eq!(*rows, 3);
        assert_eq!(std::fs::read_to_string(copy).unwrap(), text);
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn details_are_asked_for_the_newest_added_first() {
        let folder =
            std::env::temp_dir().join(format!("patina-store-order-{}", std::process::id()));
        let mut store = Store::load(folder.join("youtube.json"));
        let row = |id: &str, at| SavedVideo::new(Video::new(VideoId::parse(id).unwrap()), at);
        store.add_saved(
            vec![
                row("oldestaaaaa", 1),
                row("newestaaaaa", 3),
                row("middleaaaaa", 2),
            ],
            9,
        );
        let ids: Vec<String> = store
            .saved_without_details(3)
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect();
        assert_eq!(ids, ["newestaaaaa", "middleaaaaa", "oldestaaaaa"]);
    }

    const A: &str = "UCaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "UCbbbbbbbbbbbbbbbbbbbbbb";

    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "patina-youtube-store-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn file(&self) -> PathBuf {
            self.0.join("youtube.json")
        }

        fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn channel(id: &str, title: &str) -> Channel {
        Channel {
            title: title.into(),
            added_at: 5,
            ..Channel::new(ChannelId::parse(id).unwrap())
        }
    }

    fn video_id(id: &str) -> VideoId {
        VideoId::parse(id).unwrap()
    }

    fn saved(id: &str, added_at: i64) -> SavedVideo {
        SavedVideo::new(Video::new(video_id(id)), added_at)
    }

    #[test]
    fn no_file_is_an_empty_store_with_nothing_to_say() {
        let folder = Folder::new("empty");
        let store = Store::load(folder.file());
        assert!(store.channels().is_empty());
        assert!(store.saved().is_empty());
        assert_eq!(store.problem(), None);
        assert!(folder.names().is_empty(), "nothing is written by loading");
    }

    #[test]
    fn everything_comes_back_after_saving() {
        let folder = Folder::new("round-trip");
        let mut store = Store::load(folder.file());
        store.add_channels(vec![channel(A, "A, with a comma"), channel(B, "")]);
        store.add_saved(vec![saved("-bcdefgh_jk", 7)], 7);
        store.note_watch(&video_id("abcdefghijk"), 30.0, 100.0, 30.0, 9);
        store.set_download(&video_id("abcdefghijk"), Path::new("/videos/a.mkv"));
        store.set_show_shorts(true);
        store.save().unwrap();
        assert_eq!(folder.names(), ["youtube.json"], "no temporary file stays");

        let loaded = Store::load(folder.file());
        assert_eq!(loaded.problem(), None);
        assert_eq!(loaded.saved, store.saved);
        assert_eq!(loaded.saved.schema, SCHEMA);
        assert!(loaded.show_shorts());
    }

    #[test]
    fn a_corrupt_file_is_kept_and_mentioned() {
        let folder = Folder::new("corrupt");
        std::fs::write(folder.file(), b"{\"channels\": [truncated").unwrap();
        let mut store = Store::load(folder.file());
        assert!(store.channels().is_empty());
        let problem = store.problem().unwrap().clone();
        assert!(matches!(problem, Problem::Corrupt { copy: Some(_) }));
        assert!(problem.to_string().contains("youtube.json.bad"));
        assert_eq!(
            std::fs::read(folder.0.join("youtube.json.bad")).unwrap(),
            b"{\"channels\": [truncated"
        );
        // Not written over until something changes, and the copy stays then.
        assert_eq!(
            std::fs::read(folder.file()).unwrap(),
            b"{\"channels\": [truncated"
        );
        store.add_channels(vec![channel(A, "New")]);
        store.save().unwrap();
        assert_eq!(store.problem(), None);
        assert!(folder.0.join("youtube.json.bad").exists());
        assert_eq!(Store::load(folder.file()).channels().len(), 1);
    }

    #[test]
    fn json_of_another_shape_is_corrupt_too() {
        for bad in [&b"[1,2,3]"[..], b"null", b"\"text\"", b"", b"\xff\xfe"] {
            let folder = Folder::new("shape");
            std::fs::write(folder.file(), bad).unwrap();
            let store = Store::load(folder.file());
            assert!(
                matches!(store.problem(), Some(Problem::Corrupt { copy: Some(_) })),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_second_bad_file_does_not_replace_the_first_copy() {
        let folder = Folder::new("copies");
        std::fs::write(folder.file(), b"first").unwrap();
        Store::load(folder.file());
        // Read again, unchanged: no new copy.
        Store::load(folder.file());
        assert_eq!(folder.names(), ["youtube.json", "youtube.json.bad"]);
        std::fs::write(folder.file(), b"second").unwrap();
        Store::load(folder.file());
        assert_eq!(
            folder.names(),
            ["youtube.json", "youtube.json.bad", "youtube.json.bad.1"]
        );
        assert_eq!(
            std::fs::read(folder.0.join("youtube.json.bad")).unwrap(),
            b"first"
        );
        assert_eq!(
            std::fs::read(folder.0.join("youtube.json.bad.1")).unwrap(),
            b"second"
        );
    }

    #[test]
    fn a_newer_file_is_kept_and_read_as_far_as_it_goes() {
        let folder = Folder::new("newer");
        let text = format!(
            r#"{{"schema": 99, "future": {{"x": 1}},
                "channels": [{{"id": "{A}", "title": "Kept", "future": true}}]}}"#
        );
        std::fs::write(folder.file(), &text).unwrap();
        let store = Store::load(folder.file());
        assert_eq!(store.channels()[0].title, "Kept");
        let problem = store.problem().unwrap();
        assert!(matches!(
            problem,
            Problem::Newer {
                version: 99,
                copy: Some(_)
            }
        ));
        assert!(problem.to_string().contains("newer Patina"));
        assert_eq!(
            std::fs::read(folder.0.join("youtube.json.bad")).unwrap(),
            text.as_bytes()
        );
        assert_eq!(std::fs::read(folder.file()).unwrap(), text.as_bytes());
    }

    #[test]
    fn a_bad_row_loses_only_itself() {
        let folder = Folder::new("rows");
        let text = format!(
            r#"{{"schema": 1,
                "channels": [
                    {{"id": "{A}", "title": "Good"}},
                    {{"id": "../../etc/passwd", "title": "Bad"}},
                    7,
                    {{"id": "{A}", "title": "Duplicate"}},
                    {{"id": "{B}", "followers": "many"}}
                ],
                "saved": [
                    {{"id": "abcdefghijk", "added_at": 3, "title": "Good"}},
                    {{"id": "short"}}
                ],
                "marks": {{
                    "abcdefghijk": {{"position": 1.0, "duration": 2.0}},
                    "bad": {{"position": 1.0, "duration": 2.0}},
                    "-bcdefgh_jk": "bad"
                }},
                "downloads": {{"abcdefghijk": "/a.mkv", "x": "/b.mkv", "-bcdefgh_jk": 3}},
                "show_shorts": "yes"}}"#
        );
        std::fs::write(folder.file(), &text).unwrap();
        let store = Store::load(folder.file());
        assert_eq!(store.channels().len(), 1);
        assert_eq!(store.channels()[0].title, "Good");
        assert_eq!(store.saved().len(), 1);
        assert_eq!(store.saved.marks.len(), 1);
        assert_eq!(store.saved.downloads.len(), 1);
        assert!(!store.show_shorts());
        // Three channels, one video, two marks, two downloads, and the switch.
        assert!(matches!(
            store.problem(),
            Some(Problem::Partial {
                rows: 9,
                copy: Some(_)
            })
        ));
        assert!(folder.0.join("youtube.json.bad").exists());
    }

    #[test]
    fn a_list_of_the_wrong_kind_is_mentioned() {
        let folder = Folder::new("wrong-kind");
        std::fs::write(folder.file(), br#"{"channels": "none", "saved": null}"#).unwrap();
        let store = Store::load(folder.file());
        assert!(matches!(
            store.problem(),
            Some(Problem::Partial { rows: 1, .. })
        ));
    }

    #[test]
    fn a_file_that_cannot_be_opened_is_not_written_over() {
        let folder = Folder::new("unreadable");
        // A folder in its place makes reading fail with something other than
        // "not found", as a permission would.
        std::fs::create_dir(folder.file()).unwrap();
        let mut store = Store::load(folder.file());
        assert!(matches!(store.problem(), Some(Problem::Unreadable(_))));
        store.add_channels(vec![channel(A, "New")]);
        let error = store.save().unwrap_err();
        assert!(error.to_string().contains("could not be opened"));
        assert!(folder.file().is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn a_file_with_no_copy_is_not_written_over() {
        use std::os::unix::fs::PermissionsExt;
        let folder = Folder::new("no-copy");
        std::fs::write(folder.file(), b"bad").unwrap();
        std::fs::set_permissions(&folder.0, std::fs::Permissions::from_mode(0o500)).unwrap();
        let writable = std::fs::File::create(folder.0.join("probe")).is_ok();
        if !writable {
            let mut store = Store::load(folder.file());
            assert!(matches!(
                store.problem(),
                Some(Problem::Corrupt { copy: None })
            ));
            store.add_channels(vec![channel(A, "New")]);
            assert!(store.save().is_err());
            assert_eq!(std::fs::read(folder.file()).unwrap(), b"bad");
        }
        std::fs::set_permissions(&folder.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn a_failed_save_leaves_the_old_file_whole() {
        let folder = Folder::new("failed-save");
        let mut store = Store::load(folder.file());
        store.add_channels(vec![channel(A, "One")]);
        store.save().unwrap();
        let before = std::fs::read(folder.file()).unwrap();
        // The temporary file's place is taken by a folder.
        std::fs::create_dir(folder.0.join("youtube.json.tmp")).unwrap();
        store.add_channels(vec![channel(B, "Two")]);
        assert!(store.save().is_err());
        assert_eq!(std::fs::read(folder.file()).unwrap(), before);
    }

    #[test]
    fn adding_counts_and_fills_in() {
        let folder = Folder::new("counts");
        let mut store = Store::load(folder.file());
        let counts = store.add_channels(vec![
            channel(A, ""),
            channel(A, "Duplicate in the file"),
            channel(B, "B"),
        ]);
        assert_eq!(
            counts,
            Added {
                added: 2,
                present: 1,
                skipped: 0
            }
        );
        assert_eq!(store.channels()[0].title, "Duplicate in the file");
        let counts = store.add_channels(vec![channel(B, "Other")]);
        assert_eq!(
            counts,
            Added {
                added: 0,
                present: 1,
                skipped: 0
            }
        );
        assert_eq!(store.channels()[1].title, "B", "a known title stays");

        let counts = store.add_saved(vec![saved("abcdefghijk", 10), saved("abcdefghijk", 20)], 30);
        assert_eq!(
            counts,
            Added {
                added: 1,
                present: 1,
                skipped: 0
            }
        );
        assert_eq!(store.saved()[0].added_at, 10, "the first date stays");
        assert!(store.is_saved(&video_id("abcdefghijk")));
        assert!(store.remove_saved(&video_id("abcdefghijk")));
        assert!(!store.remove_saved(&video_id("abcdefghijk")));
    }

    #[test]
    fn lists_stop_at_their_limits() {
        let folder = Folder::new("limits");
        let mut store = Store::load(folder.file());
        let rows: Vec<Channel> = (0..MAX_CHANNELS + 3)
            .map(|index| channel(&format!("UC{index:022}"), ""))
            .collect();
        let counts = store.add_channels(rows);
        assert_eq!(counts.added, MAX_CHANNELS);
        assert_eq!(counts.skipped, 3);
    }

    #[test]
    fn a_channel_learns_what_an_answer_says() {
        let folder = Folder::new("update");
        let mut store = Store::load(folder.file());
        store.add_channels(vec![channel(A, "Old")]);
        let id = ChannelId::parse(A).unwrap();
        assert!(
            !store.update_channel(&ChannelInfo::new(id.clone())),
            "nothing to take in"
        );
        let info = ChannelInfo {
            title: "New".into(),
            handle: Some("@new".into()),
            ..ChannelInfo::new(id.clone())
        };
        assert!(store.update_channel(&info));
        assert!(!store.update_channel(&info));
        assert_eq!(store.channels()[0].title, "New");
        // An answer about a channel not subscribed to changes nothing.
        let other = ChannelInfo::new(ChannelId::parse(B).unwrap());
        assert!(!store.update_channel(&other));
        assert_eq!(store.channels().len(), 1);
        assert!(store.remove_channel(&id));
        assert!(!store.is_subscribed(&id));
    }

    #[test]
    fn details_arrive_and_failures_count() {
        let folder = Folder::new("details");
        let mut store = Store::load(folder.file());
        store.add_saved(vec![saved("abcdefghijk", 1), saved("-bcdefgh_jk", 2)], 3);
        assert_eq!(store.saved_without_details(3).len(), 2);
        store.saved_details(
            Video {
                title: "Title".into(),
                ..Video::new(video_id("abcdefghijk"))
            },
            4,
        );
        assert_eq!(store.saved_without_details(3), [video_id("-bcdefgh_jk")]);
        for _ in 0..3 {
            store.saved_tried(&video_id("-bcdefgh_jk"), false);
        }
        assert!(store.saved_without_details(3).is_empty());
        assert_eq!(store.saved_without_details(4), [video_id("-bcdefgh_jk")]);
        store.saved_tried(&video_id("-bcdefgh_jk"), true);
        assert!(store.saved_without_details(200).is_empty());
        assert!(store.saved()[1].unavailable);
        // The row stays, and details coming later clear the mark.
        store.saved_details(
            Video {
                title: "Back".into(),
                ..Video::new(video_id("-bcdefgh_jk"))
            },
            5,
        );
        assert!(!store.saved()[1].unavailable);
        assert_eq!(store.saved()[1].added_at, 2);
    }

    #[test]
    fn watched_goes_by_what_was_seen() {
        let folder = Folder::new("marks");
        let mut store = Store::load(folder.file());
        let id = video_id("abcdefghijk");
        // Opening it does nothing, nor does jumping to the end.
        store.note_watch(&id, 0.0, 100.0, 0.0, 1);
        assert!(!store.mark(&id).unwrap().watched);
        store.note_watch(&id, 99.0, 100.0, 2.0, 2);
        let mark = store.mark(&id).unwrap();
        assert!(!mark.watched);
        assert!((mark.progress() - 0.99).abs() < 1e-9);
        // Over several sittings it adds up.
        store.note_watch(&id, 40.0, 100.0, 40.0, 3);
        assert!(!store.mark(&id).unwrap().watched);
        store.note_watch(&id, 80.0, 100.0, 40.0, 4);
        let mark = *store.mark(&id).unwrap();
        assert!(mark.watched);
        assert_eq!(mark.progress(), 1.0);
        // Once watched it stays so, until the user says otherwise.
        store.note_watch(&id, 1.0, 100.0, 1.0, 5);
        assert!(store.mark(&id).unwrap().watched);
        store.set_watched(&id, false, 6);
        assert!(store.mark(&id).is_none());
        store.set_watched(&id, true, 7);
        assert!(store.mark(&id).unwrap().watched);
    }

    #[test]
    fn nonsense_numbers_do_not_become_marks() {
        let folder = Folder::new("nonsense");
        let mut store = Store::load(folder.file());
        let id = video_id("abcdefghijk");
        store.note_watch(&id, f64::NAN, f64::INFINITY, -5.0, 1);
        let mark = *store.mark(&id).unwrap();
        assert_eq!(
            (mark.position, mark.duration, mark.seen, mark.watched),
            (0.0, 0.0, 0.0, false)
        );
        assert_eq!(mark.progress(), 0.0);
        store.save().unwrap();
        assert_eq!(Store::load(folder.file()).problem(), None);
    }

    #[test]
    fn clearing_marks_says_whether_there_were_any() {
        let folder = Folder::new("clear");
        let mut store = Store::load(folder.file());
        assert!(!store.clear_marks());
        store.set_watched(&video_id("abcdefghijk"), true, 1);
        assert!(store.clear_marks());
        assert!(store.mark(&video_id("abcdefghijk")).is_none());
    }

    #[test]
    fn the_oldest_marks_go_first() {
        let folder = Folder::new("trim");
        let mut store = Store::load(folder.file());
        for index in 0..MAX_MARKS + 2 {
            store.set_watched(&video_id(&format!("{index:011}")), true, index as i64);
        }
        assert_eq!(store.saved.marks.len(), MAX_MARKS);
        assert!(store.mark(&video_id(&format!("{:011}", 0))).is_none());
        assert!(
            store
                .mark(&video_id(&format!("{:011}", MAX_MARKS + 1)))
                .is_some()
        );
    }

    #[test]
    fn a_download_counts_while_its_file_is_there() {
        let folder = Folder::new("download");
        let mut store = Store::load(folder.file());
        let id = video_id("abcdefghijk");
        let file = folder.0.join("video.mkv");
        store.set_download(&id, &file);
        assert_eq!(store.download(&id), None);
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(store.download(&id), Some(file.to_string_lossy().as_ref()));
        std::fs::remove_file(&file).unwrap();
        assert_eq!(store.download(&id), None);
        assert!(store.remove_download(&id));
        assert!(!store.remove_download(&id));
    }

    #[test]
    fn an_unknown_future_field_does_not_lose_a_row() {
        let folder = Folder::new("unknown");
        let text =
            format!(r#"{{"schema": 1, "channels": [{{"id": "{A}", "wholly_new": [1, 2]}}]}}"#);
        std::fs::write(folder.file(), text).unwrap();
        let store = Store::load(folder.file());
        assert_eq!(store.problem(), None);
        assert_eq!(store.channels().len(), 1);
    }
}
