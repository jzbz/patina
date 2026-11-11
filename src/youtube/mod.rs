//! YouTube as a section of Patina's own, with no account: subscriptions and
//! Watch Later are lists Patina keeps, and nothing here ever signs in. They
//! don't appear on YouTube.
//!
//! What comes from outside is treated as untrusted, whether it is a feed, a
//! yt-dlp answer or a file the user picked. Ids are checked when they arrive,
//! and every URL is built again from a checked id, so that no address written
//! by someone else is ever fetched or opened.
//!
//! `store.rs` keeps the lists, `feed.rs` builds and saves the feed from `rss.rs`
//! and `ytdlp.rs`, `takeout.rs` reads and writes the files Google's export uses,
//! `session.rs` is the app-wide state windows follow, `view.rs` shows it in one
//! window (`lines.rs` holds its list's lines), and `playing.rs` holds what goes
//! with playing a video from it.

pub mod client;
pub mod feed;
pub mod lines;
pub mod newpipe;
pub mod playing;
pub mod rss;
pub mod session;
pub mod store;
pub mod takeout;
pub mod view;
pub mod ytdlp;

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How long a live badge is believed without being seen again. Feeds don't say
/// when a stream ends, and a badge on a finished video would be wrong for good.
const LIVE_BADGE_SECS: i64 = 12 * 3600;
/// The same for an upcoming stream, which can be announced weeks ahead.
const UPCOMING_BADGE_SECS: i64 = 30 * 24 * 3600;
/// The most of a title that is kept. Nothing real comes close.
const TITLE_LIMIT: usize = 500;
/// Timestamps past this are taken for garbage: the year 2100.
const LATEST_TIMESTAMP: i64 = 4_102_444_800;
/// Durations past this are taken for garbage: 100 days.
const LONGEST_DURATION: f64 = 8_640_000.0;

fn id_chars(text: &str) -> bool {
    text.bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// A video's id: 11 id characters. It can begin with `-`, so it never goes on
/// a command line by itself.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct VideoId(String);

impl VideoId {
    pub fn parse(text: &str) -> Option<Self> {
        (text.len() == 11 && id_chars(text)).then(|| Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The one address a video is opened by. History and progress match on
    /// the exact text, so nothing else may reach the player.
    pub fn watch_url(&self) -> String {
        format!("https://www.youtube.com/watch?v={}", self.0)
    }

    /// A 16:9 picture that exists for every video and needs no other request.
    pub fn thumbnail_url(&self) -> String {
        format!("https://i.ytimg.com/vi/{}/mqdefault.jpg", self.0)
    }

    /// A larger picture, for a card that would show the other soft. Not every
    /// video has it (`client::smaller_thumbnail`).
    pub fn large_thumbnail_url(&self) -> String {
        format!("https://i.ytimg.com/vi/{}/hq720.jpg", self.0)
    }

    /// The id in an address `watch_url` made; anything else is not one.
    pub fn from_watch_url(url: &str) -> Option<Self> {
        Self::parse(url.strip_prefix("https://www.youtube.com/watch?v=")?)
    }
}

impl std::fmt::Debug for VideoId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl TryFrom<String> for VideoId {
    type Error = String;

    fn try_from(text: String) -> Result<Self, String> {
        Self::parse(&text).ok_or_else(|| "not a video id".to_owned())
    }
}

impl From<VideoId> for String {
    fn from(id: VideoId) -> String {
        id.0
    }
}

/// A channel's id: `UC` and 22 id characters.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ChannelId(String);

impl ChannelId {
    pub fn parse(text: &str) -> Option<Self> {
        (text.len() == 24 && text.starts_with("UC") && id_chars(text))
            .then(|| Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn url(&self) -> String {
        format!("https://www.youtube.com/channel/{}", self.0)
    }

    /// The channel's uploads tab, which leaves out Shorts.
    pub fn videos_url(&self) -> String {
        format!("https://www.youtube.com/channel/{}/videos", self.0)
    }

    pub fn feed_url(&self) -> String {
        format!(
            "https://www.youtube.com/feeds/videos.xml?channel_id={}",
            self.0
        )
    }
}

impl std::fmt::Debug for ChannelId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl TryFrom<String> for ChannelId {
    type Error = String;

    fn try_from(text: String) -> Result<Self, String> {
        Self::parse(&text).ok_or_else(|| "not a channel id".to_owned())
    }
}

impl From<ChannelId> for String {
    fn from(id: ChannelId) -> String {
        id.0
    }
}

/// A channel as the user named it, which yt-dlp turns into an id unless it
/// already is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelInput {
    Id(ChannelId),
    /// Without the `@`.
    Handle(String),
    /// The old `/c/name` address.
    Custom(String),
    /// The old `/user/name` address.
    User(String),
}

impl ChannelInput {
    /// What the user typed or pasted: an id, an `@handle`, or a link to a
    /// channel on YouTube. A link is read for its parts and never used as it
    /// is. None for anything else, a video's link included.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if let Some(id) = ChannelId::parse(text) {
            return Some(Self::Id(id));
        }
        if let Some(handle) = text.strip_prefix('@') {
            return valid_name(handle).then(|| Self::Handle(handle.to_owned()));
        }
        let with_scheme;
        let link = if text.contains("://") {
            text
        } else if text.contains('/') {
            with_scheme = format!("https://{text}");
            &with_scheme
        } else {
            return None;
        };
        let url = url::Url::parse(link).ok()?;
        if !matches!(url.scheme(), "http" | "https")
            || !matches!(
                url.host_str()?,
                "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com"
            )
        {
            return None;
        }
        let mut parts = url.path_segments()?;
        let first = decode_segment(parts.next()?)?;
        let second = parts.next().and_then(decode_segment);
        if let Some(handle) = first.strip_prefix('@') {
            return valid_name(handle).then(|| Self::Handle(handle.to_owned()));
        }
        let second = second?;
        match first.as_str() {
            "channel" => ChannelId::parse(&second).map(Self::Id),
            "c" => valid_name(&second).then_some(Self::Custom(second)),
            "user" => valid_name(&second).then_some(Self::User(second)),
            _ => None,
        }
    }

    /// The channel's uploads tab, built afresh, which is what yt-dlp is given.
    pub fn url(&self) -> String {
        let segment = match self {
            Self::Id(id) => return id.videos_url(),
            Self::Handle(handle) => vec![format!("@{handle}")],
            Self::Custom(name) => vec!["c".to_owned(), name.clone()],
            Self::User(name) => vec!["user".to_owned(), name.clone()],
        };
        let mut url = url::Url::parse("https://www.youtube.com/").expect("a valid address");
        if let Ok(mut path) = url.path_segments_mut() {
            path.clear().extend(&segment).push("videos");
        }
        url.into()
    }
}

/// A handle or an old channel name: letters and digits of any script, and the
/// few marks YouTube allows. Nothing a path or an option could be made of.
fn valid_name(name: &str) -> bool {
    let length = name.chars().count();
    (1..=100).contains(&length)
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '·'))
        && !name.chars().all(|c| c == '.')
}

/// A path segment with its `%xx` undone; None when that isn't text.
fn decode_segment(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = segment.get(index + 1..index + 3)?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// Someone else's text, made safe to show and keep: no control characters, no
/// direction overrides that could reorder what is shown around it, no line
/// breaks, and a length a row can hold.
pub fn clean_text(text: &str) -> String {
    let mut clean = String::with_capacity(text.len().min(TITLE_LIMIT));
    let mut count = 0;
    let mut space = false;
    for c in text.trim().chars() {
        // Every character of Unicode's Bidi_Control: the embeddings and
        // overrides, the isolates, and the three marks (the Arabic letter
        // mark, U+061C, being a format character that is_control misses).
        if matches!(
            c,
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{061c}'
        ) || (c.is_control() && !c.is_whitespace())
        {
            continue;
        }
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if count >= TITLE_LIMIT {
            break;
        }
        if space && !clean.is_empty() {
            clean.push(' ');
            count += 1;
        }
        space = false;
        clean.push(c);
        count += 1;
    }
    clean
}

/// A timestamp that could be real.
fn plausible_time(seconds: i64) -> Option<i64> {
    (0..=LATEST_TIMESTAMP).contains(&seconds).then_some(seconds)
}

/// When a video came out, as well as that is known. An approximate date is
/// YouTube's "4 days ago" rounded to midnight, so it can be a day out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Published {
    /// Seconds since the Unix epoch.
    pub at: i64,
    pub exact: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Live {
    Live,
    Upcoming,
}

/// A video, with what is known of it so far. Anything but the id can be
/// missing: a Watch Later row imported from Google's export has only that
/// until its title has been fetched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Video {
    pub id: VideoId,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<ChannelId>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub channel: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<Published>,
    /// In seconds. The channel feed doesn't say, nor for anything still live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub views: Option<u64>,
    /// None when nothing has said: a search result, for one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shorts: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live: Option<Live>,
    /// When `live` was last seen, as the badge's age goes by that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_seen: Option<i64>,
}

impl Video {
    pub fn new(id: VideoId) -> Self {
        Self {
            id,
            title: String::new(),
            channel_id: None,
            channel: String::new(),
            published: None,
            duration: None,
            views: None,
            shorts: None,
            live: None,
            live_seen: None,
        }
    }

    /// The badge to show as of `now`, if it can still be believed.
    pub fn live_badge(&self, now: i64) -> Option<Live> {
        let live = self.live?;
        let lasts = match live {
            Live::Live => LIVE_BADGE_SECS,
            Live::Upcoming => UPCOMING_BADGE_SECS,
        };
        (now - self.live_seen.unwrap_or(0) <= lasts).then_some(live)
    }

    /// Takes in a newer answer about the same video, losing nothing known. An
    /// exact date beats an approximate one in either order. Of two approximate
    /// ones the older answer stays, since "4 days ago" only grows coarser with
    /// time, to "1 month ago".
    pub fn absorb(&mut self, new: Video, now: i64) {
        if !new.title.is_empty() {
            self.title = new.title;
        }
        if new.channel_id.is_some() {
            self.channel_id = new.channel_id;
        }
        if !new.channel.is_empty() {
            self.channel = new.channel;
        }
        self.published = match (self.published, new.published) {
            (Some(old), Some(new)) if !new.exact => Some(old),
            (old, new) => new.or(old),
        };
        // A length means it isn't live any more, whatever was seen before; a
        // feed, which gives no length, leaves the badge to age.
        if new.live.is_some() {
            self.live = new.live;
            self.live_seen = Some(now);
        } else if new.duration.is_some() {
            self.live = None;
            self.live_seen = None;
        }
        self.duration = new.duration.or(self.duration);
        self.views = new.views.or(self.views);
        self.shorts = new.shorts.or(self.shorts);
    }
}

/// A channel as yt-dlp or a feed described it.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelInfo {
    pub id: ChannelId,
    pub title: String,
    /// With the `@`.
    pub handle: Option<String>,
    pub avatar: Option<String>,
    pub followers: Option<u64>,
}

impl ChannelInfo {
    pub fn new(id: ChannelId) -> Self {
        Self {
            id,
            title: String::new(),
            handle: None,
            avatar: None,
            followers: None,
        }
    }
}

/// A channel's newest videos, from whichever source gave them.
#[derive(Debug, Clone, PartialEq)]
pub struct Listing {
    pub channel: Option<ChannelInfo>,
    pub videos: Vec<Video>,
}

/// Why something from YouTube didn't work, by the name the interface gives it.
/// These are kept with a channel's state, so a saved name mustn't change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Failure {
    /// Online video is turned off in the preferences.
    Disabled,
    /// No yt-dlp was found.
    Missing,
    /// yt-dlp is too old for what YouTube or Patina now ask of it.
    Outdated,
    /// YouTube is refusing requests from this address.
    Blocked,
    Network,
    /// The proxy preference can't be used, or the proxy refused. Nothing goes
    /// direct instead.
    Proxy,
    /// A feed that answered with anything but a feed. YouTube's feed server
    /// does this for hours at a time, and it says nothing about the channel.
    NotFeed,
    /// yt-dlp says YouTube says the channel doesn't exist.
    NoSuchChannel,
    /// YouTube won't show the channel: terminated, say. Unlike `NoSuchChannel`
    /// this marks nothing, as what it rests on is less plain.
    ChannelUnavailable,
    /// The video can't be had, and yt-dlp said nothing more of why.
    Unavailable,
    /// The uploader made it private.
    Private,
    /// YouTube wants a signed-in adult.
    AgeRestricted,
    /// Not shown in this country.
    RegionLocked,
    /// Only for those who pay the channel.
    MembersOnly,
    /// Taken down, by the uploader or YouTube, or with its account.
    Removed,
    /// A live stream or premiere that hasn't begun.
    NotStarted,
    Timeout,
    /// yt-dlp didn't answer in time where only its starting takes any, as
    /// when asked its version: slow to start, as it can be the first time a
    /// machine's malware scan sees it.
    Slow,
    /// yt-dlp answered with something Patina can't read.
    BadOutput,
    Cancelled,
    Failed,
}

impl Failure {
    /// What the interface says.
    pub fn message(self) -> &'static str {
        match self {
            Self::Disabled => "Online video is turned off in Preferences › Network.",
            Self::Missing => {
                "yt-dlp was not found. Install it, or set where it is in Preferences › Network."
            }
            Self::Outdated => "yt-dlp looks out of date. Updating it usually fixes this.",
            Self::Blocked => "YouTube is blocking requests from this address for now.",
            Self::Network => "Could not reach YouTube. Check the network connection.",
            Self::Proxy => "The proxy in Preferences › Network could not be used.",
            Self::NotFeed => "YouTube's feed server did not answer properly.",
            Self::NoSuchChannel => "YouTube says this channel does not exist.",
            Self::ChannelUnavailable => "YouTube says this channel isn't available.",
            Self::Unavailable => "This video is not available.",
            Self::Private => "This video is private.",
            Self::AgeRestricted => "YouTube only shows this video to signed-in adults.",
            Self::RegionLocked => "This video isn't available in this country.",
            Self::MembersOnly => "This video is only for the channel's members.",
            Self::Removed => "This video has been removed.",
            Self::NotStarted => "This video hasn't started yet.",
            Self::Timeout => "YouTube took too long to answer.",
            Self::Slow => {
                "yt-dlp was slow to answer, as it can be the first time it starts. \
                 Opening the section again asks again."
            }
            Self::BadOutput => "yt-dlp gave an answer Patina can't read. It may be out of date.",
            Self::Cancelled => "Stopped.",
            Self::Failed => "yt-dlp could not do that.",
        }
    }

    /// Whether this says something of the video rather than of the way to it:
    /// another video may well work, and asking this one again won't.
    pub fn about_the_video(self) -> bool {
        matches!(
            self,
            Self::Unavailable
                | Self::Private
                | Self::AgeRestricted
                | Self::RegionLocked
                | Self::MembersOnly
                | Self::Removed
                | Self::NotStarted
        )
    }
}

/// A failure, and what the thing that failed said in its own words, for
/// showing beside the name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub kind: Failure,
    pub detail: String,
}

impl Error {
    pub fn new(kind: Failure, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

impl From<Failure> for Error {
    fn from(kind: Failure) -> Self {
        Self::new(kind, "")
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.kind.message())
    }
}

impl std::error::Error for Error {}

impl Error {
    /// What the user is told: the name, except where the name is the catch-all
    /// and what failed said more.
    pub fn said(&self) -> String {
        if self.kind == Failure::Failed && !self.detail.is_empty() {
            self.detail.clone()
        } else {
            self.to_string()
        }
    }
}

/// Stops work that is on its way: a refresh the user left, a search the next
/// one replaced.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Waits, but not past a cancel; false when cancelled.
    pub fn sleep(&self, length: Duration) -> bool {
        let until = Instant::now() + length;
        while !self.cancelled() {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return true;
            }
            std::thread::sleep(left.min(Duration::from_millis(50)));
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANNEL: &str = "UCabcdefghijklmnopqrstuv";

    fn video(id: &str) -> Video {
        Video::new(VideoId::parse(id).unwrap())
    }

    #[test]
    fn video_ids_are_eleven_id_characters() {
        assert!(VideoId::parse("abcdefghijk").is_some());
        assert!(VideoId::parse("-bcdefgh_jk").is_some());
        for bad in [
            "",
            "abcdefghij",
            "abcdefghijkl",
            "abc def ghi",
            "abcdefgh/jk",
            "abcdefgh?jk",
            "abcdefgh\njk",
            "abcdéfghij",
            "../../../..",
        ] {
            assert!(VideoId::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn channel_ids_are_uc_and_22_id_characters() {
        assert!(ChannelId::parse(CHANNEL).is_some());
        assert!(ChannelId::parse("UC_bcdefghijklmnopqrst-v").is_some());
        for bad in [
            "",
            "UC",
            "XXabcdefghijklmnopqrstuv",
            "ucabcdefghijklmnopqrstuv",
            "UCabcdefghijklmnopqrstu",
            "UCabcdefghijklmnopqrstuvw",
            "UCabcdefghijklmnopqrst/v",
            "UCabcdefghijklmnopqrst v",
        ] {
            assert!(ChannelId::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn addresses_are_built_from_the_id() {
        let id = VideoId::parse("-bcdefgh_jk").unwrap();
        assert_eq!(
            id.watch_url(),
            "https://www.youtube.com/watch?v=-bcdefgh_jk"
        );
        assert_eq!(
            id.thumbnail_url(),
            "https://i.ytimg.com/vi/-bcdefgh_jk/mqdefault.jpg"
        );
        assert_eq!(VideoId::from_watch_url(&id.watch_url()), Some(id));
        for other in [
            "https://www.youtube.com/watch?v=abcdefghijk&t=3",
            "https://youtu.be/abcdefghijk",
            "http://www.youtube.com/watch?v=abcdefghijk",
            "https://evil.example/watch?v=abcdefghijk",
        ] {
            assert_eq!(VideoId::from_watch_url(other), None, "{other}");
        }
        let channel = ChannelId::parse(CHANNEL).unwrap();
        assert_eq!(
            channel.feed_url(),
            format!("https://www.youtube.com/feeds/videos.xml?channel_id={CHANNEL}")
        );
        assert_eq!(
            channel.videos_url(),
            format!("https://www.youtube.com/channel/{CHANNEL}/videos")
        );
    }

    #[test]
    fn channels_are_read_from_what_was_pasted() {
        let handle = |name: &str| Some(ChannelInput::Handle(name.into()));
        let id = Some(ChannelInput::Id(ChannelId::parse(CHANNEL).unwrap()));
        assert_eq!(ChannelInput::parse(CHANNEL), id);
        assert_eq!(
            ChannelInput::parse("  @Some_Name-1.2\n"),
            handle("Some_Name-1.2")
        );
        assert_eq!(
            ChannelInput::parse("https://www.youtube.com/@Name/videos?x=1#y"),
            handle("Name")
        );
        assert_eq!(ChannelInput::parse("youtube.com/@Name"), handle("Name"));
        assert_eq!(
            ChannelInput::parse("http://m.youtube.com/@Name"),
            handle("Name")
        );
        assert_eq!(
            ChannelInput::parse("https://www.youtube.com/@%E5%90%8D%E5%89%8D"),
            handle("名前")
        );
        assert_eq!(ChannelInput::parse("@名前"), handle("名前"));
        assert_eq!(
            ChannelInput::parse(&format!("http://www.youtube.com/channel/{CHANNEL}")),
            id
        );
        assert_eq!(
            ChannelInput::parse("https://www.youtube.com/c/Old"),
            Some(ChannelInput::Custom("Old".into()))
        );
        assert_eq!(
            ChannelInput::parse("https://www.youtube.com/user/Old/featured"),
            Some(ChannelInput::User("Old".into()))
        );
    }

    #[test]
    fn hostile_channel_input_is_refused() {
        for bad in [
            "",
            "@",
            "name",
            "@a b",
            "@../../etc",
            "@a/b",
            "@--exec=rm",
            "@a\nb",
            "@...",
            "--exec=rm",
            "https://evil.example/@name",
            "https://www.youtube.com.evil.example/@name",
            "https://evil.example/www.youtube.com/@name",
            "ftp://www.youtube.com/@name",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "https://www.youtube.com/",
            "https://www.youtube.com/watch?v=abcdefghijk",
            "https://youtu.be/abcdefghijk",
            "https://www.youtube.com/channel/not-an-id",
            "https://www.youtube.com/c/a%2Fb",
            "https://www.youtube.com/c/%FF",
            "https://www.youtube.com/@%2E%2E%2Fa",
        ] {
            assert_eq!(ChannelInput::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn channel_addresses_are_rebuilt() {
        let url = |text| ChannelInput::parse(text).unwrap().url();
        assert_eq!(url("@Name"), "https://www.youtube.com/@Name/videos");
        assert_eq!(
            url("https://user:pw@www.youtube.com/@Name/live?evil=1"),
            "https://www.youtube.com/@Name/videos"
        );
        assert_eq!(
            url("@名前"),
            "https://www.youtube.com/@%E5%90%8D%E5%89%8D/videos"
        );
        assert_eq!(
            url("https://www.youtube.com/user/Old"),
            "https://www.youtube.com/user/Old/videos"
        );
        assert_eq!(
            url(CHANNEL),
            format!("https://www.youtube.com/channel/{CHANNEL}/videos")
        );
    }

    #[test]
    fn text_is_cleaned() {
        assert_eq!(clean_text("  a\t b\r\n\nc  "), "a b c");
        assert_eq!(clean_text("a\u{0}b\u{7f}c\u{1b}[31m"), "abc[31m");
        assert_eq!(clean_text("safe\u{202e}txt.exe\u{2066}"), "safetxt.exe");
        assert_eq!(clean_text("a\u{061c}b\u{200f}c"), "abc");
        assert_eq!(clean_text("名前 ok, \"quoted\""), "名前 ok, \"quoted\"");
        assert_eq!(clean_text(""), "");
        assert_eq!(clean_text(&"x".repeat(9000)).len(), TITLE_LIMIT);
    }

    #[test]
    fn an_exact_date_beats_an_approximate_one_in_either_order() {
        let exact = Published {
            at: 1000,
            exact: true,
        };
        let rough = Published {
            at: 2000,
            exact: false,
        };
        let coarser = Published {
            at: 500,
            exact: false,
        };
        let with = |published| Video {
            published,
            ..video("abcdefghijk")
        };
        let mut seen = with(Some(rough));
        seen.absorb(with(Some(exact)), 0);
        assert_eq!(seen.published, Some(exact));
        seen.absorb(with(Some(rough)), 0);
        assert_eq!(seen.published, Some(exact));
        seen.absorb(with(None), 0);
        assert_eq!(seen.published, Some(exact));
        let mut seen = with(Some(rough));
        seen.absorb(with(Some(coarser)), 0);
        assert_eq!(seen.published, Some(rough));
        let mut seen = with(None);
        seen.absorb(with(Some(coarser)), 0);
        assert_eq!(seen.published, Some(coarser));
        // A corrected exact date replaces the old one.
        let mut seen = with(Some(exact));
        let later = Published {
            at: 1500,
            exact: true,
        };
        seen.absorb(with(Some(later)), 0);
        assert_eq!(seen.published, Some(later));
    }

    #[test]
    fn absorbing_loses_nothing_known() {
        let mut seen = Video {
            title: "Title".into(),
            channel: "Channel".into(),
            channel_id: ChannelId::parse(CHANNEL),
            duration: Some(10),
            views: Some(5),
            shorts: Some(true),
            ..video("abcdefghijk")
        };
        let before = seen.clone();
        seen.absorb(video("abcdefghijk"), 0);
        assert_eq!(seen, before);
        seen.absorb(
            Video {
                title: "New".into(),
                views: Some(6),
                ..video("abcdefghijk")
            },
            0,
        );
        assert_eq!(seen.title, "New");
        assert_eq!(seen.views, Some(6));
        assert_eq!(seen.duration, Some(10));
    }

    #[test]
    fn live_badges_age_and_end() {
        let mut seen = video("abcdefghijk");
        assert_eq!(seen.live_badge(0), None);
        seen.absorb(
            Video {
                live: Some(Live::Live),
                ..video("abcdefghijk")
            },
            1000,
        );
        assert_eq!(seen.live_badge(1000), Some(Live::Live));
        assert_eq!(seen.live_badge(1000 + LIVE_BADGE_SECS), Some(Live::Live));
        assert_eq!(seen.live_badge(1001 + LIVE_BADGE_SECS), None);
        // Seen again in a feed, which says nothing of it: the badge stays.
        seen.absorb(video("abcdefghijk"), 2000);
        assert_eq!(seen.live_badge(2000), Some(Live::Live));
        // A length means it has ended.
        seen.absorb(
            Video {
                duration: Some(60),
                ..video("abcdefghijk")
            },
            3000,
        );
        assert_eq!(seen.live_badge(3000), None);
    }

    #[test]
    fn cancelling_stops_a_wait() {
        let cancel = Cancel::default();
        assert!(cancel.sleep(Duration::ZERO));
        cancel.cancel();
        assert!(!cancel.sleep(Duration::from_secs(60)));
    }
}
