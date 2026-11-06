//! yt-dlp for what only it can do: search, channel listings and a video's
//! details. Each call is one short-lived process with a time limit.
//!
//! These calls never get the user's yt-dlp options, and `--ignore-config` keeps
//! yt-dlp's own config file out too. Either could hold `cookies-from-browser`,
//! which would turn a section that has no account into repeated signed-in
//! requests. The proxy is the one thing passed on, on purpose.
//!
//! One process can list several channels, which is far cheaper than one each,
//! as starting yt-dlp is most of the time a call takes. With `-J` it prints one
//! line per address: `null` for one that failed, in which case it also exits
//! with 1 while the others are fine. So lines are matched to addresses by
//! position, and the exit code alone means nothing.
//!
//! yt-dlp's errors are English text that changes between versions, so reading
//! them is a matter of finding well-known phrases, with a plain failure when
//! none is there. What is searched for must never reach the logs, and neither
//! a stream address or a proxy's password, so details are cleaned first
//! (`scrub`) and none of this is logged.

use super::client::is_image_url;
use super::feed::Source;
use super::{
    Cancel, ChannelId, ChannelInfo, ChannelInput, Error, Failure, Listing, Live, Published, Video,
    VideoId, clean_text, plausible_time,
};
use crate::settings::Preferences;
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What every call starts with. `--retries` and `--socket-timeout` keep a dead
/// connection from outlasting the call's own limit.
const COMMON: &[&str] = &[
    "--ignore-config",
    "--no-cache-dir",
    "--no-progress",
    "--encoding",
    "utf-8",
    "--socket-timeout",
    "10",
    "--retries",
    "1",
    "--extractor-retries",
    "1",
];
/// A channel's listing and search. One page gives this many in one request.
const LISTING_ITEMS: &str = "30";
const SEARCH_RESULTS: usize = 30;
/// A rough date for videos in a flat list, which otherwise have none, rounded
/// to midnight and costing no extra request.
const APPROXIMATE_DATES: &str = "youtubetab:approximate_date";
/// The web's filter for channels in search results, as `ytsearch` gives only
/// videos.
const CHANNELS_FILTER: &str = "EgIQAg==";
/// The longest search that is asked for.
const QUERY_LIMIT: usize = 200;
/// A client that gives a video's details without a JavaScript player, since
/// Patina doesn't want its streams here.
const DETAILS_CLIENT: &str = "youtube:player_client=visionos";

const SEARCH_LIMIT: Duration = Duration::from_secs(30);
const RESOLVE_LIMIT: Duration = Duration::from_secs(20);
const VIDEO_LIMIT: Duration = Duration::from_secs(30);
const VERSION_LIMIT: Duration = Duration::from_secs(10);
/// A listing's limit: starting yt-dlp, then about a second a channel, doubled
/// and then some for a bad day.
const LISTING_BASE: Duration = Duration::from_secs(20);
const LISTING_PER_CHANNEL: Duration = Duration::from_secs(4);
/// Channels asked for in one process, and processes at once. Low, as one
/// address has a budget of about 1000 page requests an hour, which playback
/// and search share.
const BATCH: usize = 8;
const WORKERS: usize = 2;

const OUTPUT_LIMIT: usize = 64 << 20;
const STDERR_KEEP: usize = 64 << 10;
/// How long the output of a process that has ended is waited for: anything it
/// started (Deno, for one) can hold its pipes open.
const PIPE_WAIT: Duration = Duration::from_millis(500);
/// Most versions are out for weeks, and 84 days has passed between two.
const STALE_AFTER_DAYS: i64 = 90;

/// A yt-dlp to run, with the proxy it goes through.
#[derive(Debug, Clone)]
pub struct Ytdlp {
    program: PathBuf,
    /// As `Preferences::proxy` gives it; empty goes direct.
    proxy: Option<String>,
}

/// What a finished process left.
struct Output {
    stdout: Vec<u8>,
    stderr: String,
    timed_out: bool,
}

impl Ytdlp {
    pub fn new(program: PathBuf, proxy: Option<String>) -> Self {
        Self { program, proxy }
    }

    /// The yt-dlp these preferences name, as playback and downloads find it,
    /// or the reason there is none to use. youtube-dl can't do this.
    pub fn from_prefs(prefs: &Preferences, mpv_config: &Path) -> Result<Self, Error> {
        if !prefs.ytdl {
            return Err(Failure::Disabled.into());
        }
        let program =
            crate::downloads::find_ytdl(prefs, mpv_config).ok_or(Error::from(Failure::Missing))?;
        let name = program
            .file_name()
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if name.starts_with("youtube-dl") {
            return Err(Error::new(
                Failure::Outdated,
                "only youtube-dl was found, and this takes yt-dlp",
            ));
        }
        Ok(Self::new(program, prefs.proxy().map(str::to_owned)))
    }

    /// Runs yt-dlp to its end, a limit or a cancel, and gives what it left.
    fn run(&self, args: &[String], limit: Duration, cancel: &Cancel) -> Result<Output, Error> {
        let mut command = Command::new(&self.program);
        command.args(COMMON);
        if let Some(proxy) = &self.proxy {
            command.arg("--proxy").arg(proxy);
        }
        command
            .args(args)
            // Its text in UTF-8 whatever the console's encoding.
            .env("PYTHONIOENCODING", "utf-8")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        crate::platform::no_console(&mut command);
        crate::platform::in_own_group(&mut command);
        let mut child = command.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::from(Failure::Missing)
            } else {
                Error::new(
                    Failure::Failed,
                    format!("could not run yt-dlp: {}", error.kind()),
                )
            }
        })?;
        let stdout = child
            .stdout
            .take()
            .map(|pipe| Reader::start(pipe, OUTPUT_LIMIT, false));
        let stderr = child
            .stderr
            .take()
            .map(|pipe| Reader::start(pipe, STDERR_KEEP, true));
        let until = Instant::now() + limit;
        let mut timed_out = false;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {}
                Err(error) => {
                    log::warn!("could not wait for yt-dlp: {}", error.kind());
                    crate::platform::kill_group(&mut child);
                    break;
                }
            }
            // Asked to end first, so that a standalone yt-dlp's launcher
            // removes what it unpacked (`platform::end_group`).
            if cancel.cancelled() {
                crate::platform::stop_group(&mut child);
                let _ = child.wait();
                return Err(Failure::Cancelled.into());
            }
            let full = stdout.as_ref().is_some_and(Reader::overflowed);
            timed_out = Instant::now() >= until;
            if timed_out || full {
                crate::platform::stop_group(&mut child);
                let _ = child.wait();
                if full {
                    return Err(Error::new(Failure::BadOutput, "too much output"));
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(Output {
            stdout: stdout.map(Reader::finish).unwrap_or_default(),
            stderr: stderr
                .map(Reader::finish)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default(),
            timed_out,
        })
    }

    /// Asks about several addresses in one process, and gives each one's
    /// answer or error, in order. `tags` are what each address's errors can be
    /// told apart by.
    fn ask(
        &self,
        args: &[String],
        urls: &[String],
        tags: &[&str],
        limit: Duration,
        cancel: &Cancel,
        secrets: &[&str],
    ) -> Result<Vec<Result<Value, Error>>, Error> {
        let mut all = args.to_vec();
        // A query or address can begin with what looks like an option.
        all.push("--".to_owned());
        all.extend_from_slice(urls);
        let output = self.run(&all, limit, cancel)?;
        let secrets: Vec<&str> = secrets
            .iter()
            .copied()
            .chain(self.proxy.as_deref())
            .collect();
        Ok(answers(&output, urls.len(), tags, &secrets))
    }

    /// Lists channels' uploads in one process. Each channel's result stands
    /// alone.
    pub fn listings(&self, channels: &[ChannelId], cancel: &Cancel) -> Vec<Result<Listing, Error>> {
        let urls: Vec<String> = channels.iter().map(ChannelId::videos_url).collect();
        let tags: Vec<&str> = channels.iter().map(ChannelId::as_str).collect();
        let limit = LISTING_BASE + LISTING_PER_CHANNEL * channels.len() as u32;
        match self.ask(&flat_args(), &urls, &tags, limit, cancel, &[]) {
            Ok(answers) => answers
                .into_iter()
                .zip(channels)
                .map(|(answer, channel)| {
                    parse_listing(&answer.map_err(of_a_channel)?, Some(channel), Some(false))
                })
                .collect(),
            Err(error) => channels.iter().map(|_| Err(error.clone())).collect(),
        }
    }

    /// One channel's uploads, for its page.
    pub fn listing(&self, channel: &ChannelId, cancel: &Cancel) -> Result<Listing, Error> {
        self.listings(std::slice::from_ref(channel), cancel)
            .pop()
            .unwrap_or_else(|| Err(Failure::BadOutput.into()))
    }

    /// Videos and channels for a query, in one process. Either kind can fail
    /// alone; only both failing is an error.
    pub fn search(&self, query: &str, cancel: &Cancel) -> Result<SearchResults, Error> {
        let query: String = clean_text(query).chars().take(QUERY_LIMIT).collect();
        if query.is_empty() {
            return Ok(SearchResults::default());
        }
        let encoded: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
        let filter: String =
            url::form_urlencoded::byte_serialize(CHANNELS_FILTER.as_bytes()).collect();
        let urls = [
            format!("ytsearch{SEARCH_RESULTS}:{query}"),
            format!("https://www.youtube.com/results?search_query={encoded}&sp={filter}"),
        ];
        let mut answers = self
            .ask(
                &flat_args(),
                &urls,
                &["ytsearch", "search_query"],
                SEARCH_LIMIT,
                cancel,
                &[&query, &encoded],
            )?
            .into_iter();
        let videos = answers
            .next()
            .unwrap_or_else(|| Err(Failure::BadOutput.into()));
        let channels = answers
            .next()
            .unwrap_or_else(|| Err(Failure::BadOutput.into()));
        if let (Err(error), Err(_)) = (&videos, &channels) {
            return Err(error.clone());
        }
        Ok(SearchResults {
            videos: videos
                .ok()
                .map(|value| {
                    entries(&value)
                        .iter()
                        .filter_map(|entry| video_of(entry, None, None))
                        .collect()
                })
                .unwrap_or_default(),
            channels: channels
                .ok()
                .map(|value| {
                    entries(&value)
                        .iter()
                        .filter_map(channel_of_entry)
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    /// Finds the channel the user named.
    pub fn resolve(&self, input: &ChannelInput, cancel: &Cancel) -> Result<ChannelInfo, Error> {
        let url = input.url();
        let mut args = flat_args();
        args.extend(["--playlist-items".to_owned(), "0".to_owned()]);
        let answer = self
            .ask(
                &args,
                std::slice::from_ref(&url),
                &[],
                RESOLVE_LIMIT,
                cancel,
                &[],
            )?
            .pop()
            .unwrap_or_else(|| Err(Failure::BadOutput.into()))
            .map_err(of_a_channel)?;
        parse_listing(&answer, None, None)?
            .channel
            .ok_or_else(|| Error::new(Failure::BadOutput, "no channel in the answer"))
    }

    /// A video's details: the one way to learn a title from just an id. This
    /// costs a page and a player request, so it is used sparingly.
    pub fn video(&self, id: &VideoId, cancel: &Cancel) -> Result<Video, Error> {
        let args: Vec<String> = [
            "-J",
            "--no-playlist",
            "--ignore-no-formats-error",
            "--extractor-args",
            DETAILS_CLIENT,
        ]
        .map(str::to_owned)
        .into();
        let url = id.watch_url();
        let answer = self
            .ask(
                &args,
                std::slice::from_ref(&url),
                &[id.as_str()],
                VIDEO_LIMIT,
                cancel,
                &[],
            )?
            .pop()
            .unwrap_or_else(|| Err(Failure::BadOutput.into()))?;
        parse_video(&answer, id)
    }

    /// Which yt-dlp this is. It touches no network.
    pub fn version(&self, today: jiff::civil::Date, cancel: &Cancel) -> Result<Version, Error> {
        let output = self.run(&["--version".to_owned()], VERSION_LIMIT, cancel)?;
        let text = String::from_utf8_lossy(&output.stdout);
        let first = text.lines().next().unwrap_or_default();
        if first.is_empty() {
            return Err(Error::from(Failure::BadOutput));
        }
        Ok(Version::read(first, today))
    }
}

/// Listing several channels through a source for the feed.
impl Source for Ytdlp {
    fn batch(&self) -> usize {
        BATCH
    }

    fn workers(&self) -> usize {
        WORKERS
    }

    fn fetch(&self, channels: &[ChannelId], cancel: &Cancel) -> Vec<Result<Listing, Error>> {
        self.listings(channels, cancel)
    }
}

fn flat_args() -> Vec<String> {
    [
        "--flat-playlist",
        "-J",
        "--playlist-end",
        LISTING_ITEMS,
        "--extractor-args",
        APPROXIMATE_DATES,
    ]
    .map(str::to_owned)
    .into()
}

/// Reads a process's pipe on a thread of its own, as a full pipe would stop the
/// process. `tail` keeps the end of too much rather than saying so.
struct Reader {
    bytes: Arc<Mutex<Vec<u8>>>,
    ended: mpsc::Receiver<()>,
    limit: usize,
    tail: bool,
}

impl Reader {
    fn start(mut pipe: impl Read + Send + 'static, limit: usize, tail: bool) -> Self {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let filling = bytes.clone();
        let (sender, ended) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("youtube-pipe".into())
            .spawn(move || {
                let mut chunk = [0u8; 16 << 10];
                while let Ok(count) = pipe.read(&mut chunk) {
                    if count == 0 {
                        break;
                    }
                    let mut bytes = filling.lock().unwrap_or_else(|error| error.into_inner());
                    if tail || bytes.len() <= limit {
                        bytes.extend_from_slice(&chunk[..count]);
                    }
                    if tail && bytes.len() > limit {
                        let cut = bytes.len() - limit;
                        bytes.drain(..cut);
                    }
                }
                let _ = sender.send(());
            });
        if let Err(error) = spawned {
            log::warn!("could not read from yt-dlp: {}", error.kind());
        }
        Self {
            bytes,
            ended,
            limit,
            tail,
        }
    }

    fn overflowed(&self) -> bool {
        !self.tail
            && self
                .bytes
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .len()
                > self.limit
    }

    /// What has been read, once the pipe has closed or `PIPE_WAIT` has passed.
    fn finish(self) -> Vec<u8> {
        let _ = self.ended.recv_timeout(PIPE_WAIT);
        std::mem::take(&mut *self.bytes.lock().unwrap_or_else(|error| error.into_inner()))
    }
}

/// Each address's line, or its error from what yt-dlp said on stderr.
fn answers(
    output: &Output,
    count: usize,
    tags: &[&str],
    secrets: &[&str],
) -> Vec<Result<Value, Error>> {
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.split('\n').filter(|line| !line.trim().is_empty());
    let mut errors: Vec<Option<&str>> = output
        .stderr
        .lines()
        .filter(|line| line.starts_with("ERROR:") || line.starts_with("yt-dlp: error:"))
        .map(Some)
        .collect();
    (0..count)
        .map(|index| {
            let line = lines.next();
            match line.map(serde_json::from_str::<Value>) {
                Some(Ok(Value::Null)) | None => {}
                Some(Ok(value)) => return Ok(value),
                Some(Err(_)) => {
                    return Err(Error::new(Failure::BadOutput, "a line that isn't JSON"));
                }
            }
            // Its own error by what it mentions, or else the next one that
            // mentions no other address.
            let tag = tags.get(index).copied().unwrap_or("");
            let own = errors.iter().position(|error| {
                error.is_some_and(|error| !tag.is_empty() && error.contains(tag))
            });
            let error = own.or_else(|| {
                errors.iter().position(|error| {
                    error.is_some_and(|error| {
                        !tags
                            .iter()
                            .any(|other| !other.is_empty() && error.contains(other))
                    })
                })
            });
            match error.and_then(|position| errors[position].take()) {
                Some(error) => Err(classify(error, secrets)),
                None if output.timed_out => Err(Failure::Timeout.into()),
                None if line.is_none() => {
                    Err(Error::new(Failure::BadOutput, "no answer for this address"))
                }
                None => Err(Failure::Failed.into()),
            }
        })
        .collect()
}

/// yt-dlp's error line, by its name.
fn classify(line: &str, secrets: &[&str]) -> Error {
    let lower = line.to_lowercase();
    let has = |phrases: &[&str]| phrases.iter().any(|phrase| lower.contains(phrase));
    // Only YouTube's own word on it, as this marks a channel: a missing file
    // would say the same words.
    let kind = if has(&["does not exist"]) && has(&["channel", "youtube said"]) {
        Failure::NoSuchChannel
    } else if has(&[
        "not a bot",
        "http error 429",
        "too many requests",
        "rate-limit",
        "rate limit",
        "try again later",
        "http error 403",
    ]) {
        Failure::Blocked
    } else if let Some(kind) = about_the_video(&lower) {
        // Before the proxy: yt-dlp's advice for a region lock mentions one.
        kind
    } else if has(&["proxy", "tunnel connection failed"]) {
        Failure::Proxy
    } else if has(&[
        "getaddrinfo",
        "name or service not known",
        "temporary failure in name resolution",
        "nodename nor servname",
        "network is unreachable",
        "no route to host",
        "connection refused",
        "connection reset",
        "connection aborted",
        "timed out",
        "ssl:",
        "certificate verify failed",
        "unable to connect",
        "failed to resolve",
    ]) {
        Failure::Network
    } else if has(&[
        "unable to extract",
        "failed to parse",
        "unsupported url",
        "please report this issue",
        "update to the latest",
        "out of date",
        "nsig",
        "no such option",
        "unrecognized arguments",
        "yt-dlp: error:",
        "unable to handle request",
    ]) {
        Failure::Outdated
    } else {
        Failure::Failed
    };
    Error::new(kind, scrub(line, secrets))
}

/// An error from asking about a channel. The phrases are read as of a video,
/// and said of a channel they don't mean that video has gone, or which one.
fn of_a_channel(error: Error) -> Error {
    if error.kind.about_the_video() {
        Error::new(Failure::ChannelUnavailable, error.detail)
    } else {
        error
    }
}

/// What YouTube's words, as yt-dlp passes them on, say of a video. They change
/// between versions, and what isn't known here ends as the generic message.
fn about_the_video(lower: &str) -> Option<Failure> {
    let has = |phrases: &[&str]| phrases.iter().any(|phrase| lower.contains(phrase));
    Some(if has(&["private video", "this video is private"]) {
        Failure::Private
    } else if has(&[
        "confirm your age",
        "age-restricted",
        "age restricted",
        "inappropriate for some users",
    ]) {
        Failure::AgeRestricted
    } else if has(&["members-only", "channel's members", "join this channel"]) {
        Failure::MembersOnly
    } else if has(&[
        "not available in your country",
        "not made this video available",
        "blocked it in your country",
        "not available from your location",
        "not available in your region",
    ]) {
        Failure::RegionLocked
    } else if has(&[
        "has been removed",
        "removed by",
        "been terminated",
        "no longer available",
        "account associated with this video",
        "recording is not available",
    ]) {
        Failure::Removed
    } else if has(&[
        "live event will begin",
        "premieres in",
        "premiere will begin",
    ]) {
        Failure::NotStarted
    } else if has(&[
        "video unavailable",
        "video is unavailable",
        "video is not available",
    ]) {
        Failure::Unavailable
    } else {
        return None;
    })
}

/// A download's or the player's last word from yt-dlp, by name.
pub fn failure_of(line: &str) -> Error {
    classify(line, &[])
}

/// What mpv's hook said when yt-dlp failed, one message at a time: yt-dlp's
/// error, or the hook's own word that it couldn't run it.
pub fn hook_failure(said: &str) -> Option<Error> {
    let line = said.lines().map(str::trim).find(|line| !line.is_empty())?;
    if line.contains("not found or not enough permissions") {
        return Some(Failure::Missing.into());
    }
    // The hook always ends with this, which says nothing that came before
    // didn't.
    if line.starts_with("youtube-dl failed") {
        return None;
    }
    Some(classify(line, &[]))
}

/// yt-dlp's words, safe to show and keep: without the prefix, stream addresses
/// (which hold the user's address), or anything else that shouldn't be repeated.
fn scrub(line: &str, secrets: &[&str]) -> String {
    let line = line.strip_prefix("ERROR:").unwrap_or(line);
    let mut text: String = line
        .split_whitespace()
        .map(|word| {
            if word.contains("googlevideo.com") || word.contains("ip=") {
                "[address]"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        text = text.replace(secret, "[…]");
    }
    let text = clean_text(&text);
    text.chars().take(300).collect()
}

/// A number that could be a count.
fn count(value: Option<&Value>) -> Option<u64> {
    let number = value?.as_f64()?;
    (number.is_finite() && (0.0..=1e15).contains(&number)).then_some(number as u64)
}

fn seconds(value: Option<&Value>) -> Option<u64> {
    let number = value?.as_f64()?;
    (number.is_finite() && (0.0..=super::LONGEST_DURATION).contains(&number))
        .then_some(number as u64)
}

fn time(value: Option<&Value>) -> Option<i64> {
    let number = value?.as_f64()?;
    number
        .is_finite()
        .then_some(number as i64)
        .and_then(plausible_time)
}

fn text(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .map(clean_text)
        .unwrap_or_default()
}

fn live(value: Option<&Value>) -> Option<Live> {
    match value?.as_str()? {
        "is_live" => Some(Live::Live),
        "is_upcoming" => Some(Live::Upcoming),
        _ => None,
    }
}

/// The rows of a list. Far more than a page's worth is someone else's mistake.
fn entries(value: &Value) -> &[Value] {
    let rows = value
        .get("entries")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    &rows[..rows.len().min(1000)]
}

/// A video from a row of a flat list, or None for anything else in the list.
/// Its fields are read one by one, so that one of a strange type costs only
/// itself.
fn video_of(entry: &Value, owner: Option<&ChannelInfo>, shorts: Option<bool>) -> Option<Video> {
    let field = |name: &str| entry.get(name);
    if field("ie_key")
        .and_then(Value::as_str)
        .is_some_and(|key| key != "Youtube")
    {
        return None;
    }
    let id = VideoId::parse(field("id")?.as_str()?)?;
    let title = text(field("title"));
    if matches!(title.as_str(), "[Private video]" | "[Deleted video]") {
        return None;
    }
    let is_short = field("url")
        .and_then(Value::as_str)
        .is_some_and(|url| url.contains("/shorts/"));
    let channel = [text(field("channel")), text(field("uploader"))]
        .into_iter()
        .find(|name| !name.is_empty())
        .or_else(|| owner.map(|owner| owner.title.clone()))
        .unwrap_or_default();
    Some(Video {
        title,
        channel_id: field("channel_id")
            .and_then(Value::as_str)
            .and_then(ChannelId::parse)
            .or_else(|| owner.map(|owner| owner.id.clone())),
        channel,
        // In a flat list, dates are rounded from text such as "4 days ago".
        published: time(field("timestamp")).map(|at| Published { at, exact: false }),
        duration: seconds(field("duration")),
        views: count(field("view_count")),
        shorts: if is_short { Some(true) } else { shorts },
        live: live(field("live_status")),
        ..Video::new(id)
    })
}

/// A checked channel's description from a list's heading or a row.
fn channel_of(value: &Value, id: ChannelId) -> ChannelInfo {
    let field = |name: &str| value.get(name);
    let title = [text(field("channel")), text(field("uploader"))]
        .into_iter()
        .find(|name| !name.is_empty())
        .unwrap_or_default();
    ChannelInfo {
        title,
        handle: field("uploader_id")
            .and_then(Value::as_str)
            .filter(|handle| handle.starts_with('@') && handle.len() <= 101)
            .map(clean_text),
        avatar: avatar(value),
        followers: count(field("channel_follower_count")),
        id,
    }
}

/// A channel from a row of a search, or None for anything else.
fn channel_of_entry(entry: &Value) -> Option<ChannelInfo> {
    let id = entry
        .get("channel_id")
        .or_else(|| entry.get("id"))
        .and_then(Value::as_str)
        .and_then(ChannelId::parse)?;
    if entry
        .get("ie_key")
        .and_then(Value::as_str)
        .is_some_and(|key| key != "YoutubeTab")
    {
        return None;
    }
    Some(channel_of(entry, id))
}

/// The channel's picture: square, and on an image host. The smallest at least
/// `AVATAR_SIZE` wide, as a heading lists sizes up to 900 and a cropped
/// `=s0` of unknown size. Rows write their addresses with no scheme.
fn avatar(value: &Value) -> Option<String> {
    const AVATAR_SIZE: u64 = 160;
    let thumbnails = value.get("thumbnails")?.as_array()?;
    let mut choices: Vec<(u64, url::Url)> = thumbnails
        .iter()
        .filter_map(|thumbnail| {
            let width = count(thumbnail.get("width"))?;
            if Some(width) != count(thumbnail.get("height")) {
                return None;
            }
            let address = thumbnail.get("url")?.as_str()?;
            let address = match address.strip_prefix("//") {
                Some(rest) => format!("https://{rest}"),
                None => address.to_owned(),
            };
            let url = url::Url::parse(&address).ok().filter(is_image_url)?;
            Some((width, url))
        })
        .collect();
    choices.sort_by_key(|(width, _)| (*width < AVATAR_SIZE, width.abs_diff(AVATAR_SIZE)));
    choices.into_iter().next().map(|(_, url)| url.into())
}

/// A channel's listing from yt-dlp's answer. When a particular channel was
/// asked for, an answer about another one is refused: one line off would put
/// someone's videos under the wrong channel.
fn parse_listing(
    value: &Value,
    expected: Option<&ChannelId>,
    shorts: Option<bool>,
) -> Result<Listing, Error> {
    if !value.is_object() || !value.get("entries").is_some_and(Value::is_array) {
        return Err(Error::new(Failure::BadOutput, "not a list"));
    }
    let said = value
        .get("channel_id")
        .and_then(Value::as_str)
        .and_then(ChannelId::parse);
    if let (Some(said), Some(expected)) = (&said, expected)
        && said != expected
    {
        return Err(Error::new(
            Failure::BadOutput,
            "an answer about another channel",
        ));
    }
    let channel = said
        .or_else(|| expected.cloned())
        .map(|id| channel_of(value, id));
    let videos = entries(value)
        .iter()
        .filter_map(|entry| video_of(entry, channel.as_ref(), shorts))
        .collect();
    Ok(Listing { channel, videos })
}

/// A video's details from yt-dlp's full answer, whose date is exact.
fn parse_video(value: &Value, id: &VideoId) -> Result<Video, Error> {
    if value.get("id").and_then(Value::as_str) != Some(id.as_str()) {
        return Err(Error::new(
            Failure::BadOutput,
            "an answer about another video",
        ));
    }
    let field = |name: &str| value.get(name);
    let title = text(field("title"));
    // Told to carry on without formats, yt-dlp answers for a video YouTube
    // doesn't have with a title of its own making.
    if title == format!("youtube video #{}", id.as_str()) {
        return Err(Failure::Unavailable.into());
    }
    Ok(Video {
        title,
        channel_id: field("channel_id")
            .and_then(Value::as_str)
            .and_then(ChannelId::parse),
        channel: [text(field("channel")), text(field("uploader"))]
            .into_iter()
            .find(|name| !name.is_empty())
            .unwrap_or_default(),
        published: time(field("timestamp"))
            .or_else(|| time(field("release_timestamp")))
            .map(|at| Published { at, exact: true }),
        duration: seconds(field("duration")),
        views: count(field("view_count")),
        live: live(field("live_status")),
        ..Video::new(id.clone())
    })
}

/// What a search found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchResults {
    pub videos: Vec<Video>,
    pub channels: Vec<ChannelInfo>,
}

/// yt-dlp's version, which is its release date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub text: String,
    /// None when the version isn't a date.
    pub age_days: Option<i64>,
    /// Old enough that YouTube may well have moved on from it.
    pub stale: bool,
}

impl Version {
    /// Reads `2026.10.26`, or a nightly's `2026.10.26.233045`.
    fn read(text: &str, today: jiff::civil::Date) -> Self {
        let text = clean_text(text);
        let mut parts = text.split('.').map(str::parse::<i16>);
        let date = match (parts.next(), parts.next(), parts.next()) {
            (Some(Ok(year)), Some(Ok(month)), Some(Ok(day))) => month
                .try_into()
                .ok()
                .zip(day.try_into().ok())
                .and_then(|(month, day)| jiff::civil::Date::new(year, month, day).ok()),
            _ => None,
        };
        let age_days = date
            .and_then(|date| today.since(date).ok())
            .map(|span| i64::from(span.get_days()));
        Self {
            text,
            age_days,
            stale: age_days.is_some_and(|days| days > STALE_AFTER_DAYS),
        }
    }
}

#[cfg(all(test, unix))]
pub mod testing {
    //! A stand-in for yt-dlp: a script that prints what it is told to, and
    //! notes the arguments it was given. It is a shell script, so tests using
    //! it are for Unix; nothing here needs the real yt-dlp or a network.

    use std::path::{Path, PathBuf};
    use std::process::Command;

    pub struct Fake {
        pub program: PathBuf,
        args: PathBuf,
    }

    impl Fake {
        /// `body` is the script's shell code after the arguments are noted.
        pub fn new(folder: &Path, body: &str) -> Self {
            use std::os::unix::fs::PermissionsExt;
            std::fs::create_dir_all(folder).unwrap();
            let program = folder.join("yt-dlp");
            let args = folder.join("args");
            let script = format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = --test-ready ]; then exit 0; fi\n\
                 printf '%s\\n' \"$@\" >> '{}'\n\
                 printf -- '---\\n' >> '{}'\n\
                 {body}\n",
                args.display(),
                args.display(),
            );
            std::fs::write(&program, script).unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            // A program just written can't run while another thread's fork
            // still holds it open for writing, for a moment.
            for _ in 0..200 {
                if Command::new(&program).arg("--test-ready").status().is_ok() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Self { program, args }
        }

        /// The arguments of each run so far.
        pub fn runs(&self) -> Vec<Vec<String>> {
            let text = std::fs::read_to_string(&self.args).unwrap_or_default();
            text.split("---\n")
                .filter(|run| !run.is_empty())
                .map(|run| run.lines().map(str::to_owned).collect())
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Stopping a yt-dlp stops what it started: a launcher's worker or a
    /// JavaScript runtime, which would go on making requests otherwise.
    #[cfg(unix)]
    #[test]
    fn stopping_it_stops_what_it_started() {
        let folder = std::env::temp_dir().join(format!("patina-ytdlp-kill-{}", std::process::id()));
        let pids = folder.join("pids");
        let fake = testing::Fake::new(
            &folder.join("bin"),
            &format!(
                "sleep 60 &\necho $! >> '{0}'\nsleep 60 &\necho $! >> '{0}'\nwait",
                pids.display()
            ),
        );
        let tool = Ytdlp::new(fake.program.clone(), None);
        let cancel = Cancel::default();
        let stopping = cancel.clone();
        let pids_written = pids.clone();
        std::thread::spawn(move || {
            while std::fs::read_to_string(&pids_written).map_or(0, |text| text.lines().count()) < 2
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            stopping.cancel();
        });
        let answer = tool.run(&[], Duration::from_secs(30), &cancel);
        assert_eq!(
            answer.err().map(|error| error.kind),
            Some(Failure::Cancelled)
        );
        for pid in std::fs::read_to_string(&pids).unwrap().lines() {
            let mut gone = false;
            for _ in 0..200 {
                let stat = Command::new("ps")
                    .args(["-o", "stat=", "-p", pid])
                    .output()
                    .unwrap()
                    .stdout;
                if stat.is_empty() || stat.starts_with(b"Z") {
                    gone = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(gone, "{pid} is still running");
        }
        let _ = std::fs::remove_dir_all(&folder);
    }

    /// Said of a channel, a video's phrases don't mean that a video has gone.
    #[test]
    fn a_channels_errors_are_named_as_a_channels() {
        let terminated = classify(
            "ERROR: [youtube:tab] x: This account has been terminated",
            &[],
        );
        assert_eq!(terminated.kind, Failure::Removed);
        let named = of_a_channel(terminated);
        assert_eq!(named.kind, Failure::ChannelUnavailable);
        assert!(named.detail.contains("terminated"));
        assert!(named.to_string().contains("channel"));
        // What says more than its name keeps its words for the row.
        let other = classify(
            "ERROR: [youtube:tab] x: This channel does not have a videos tab",
            &[],
        );
        let other = of_a_channel(other);
        assert_eq!(other.kind, Failure::Failed);
        assert!(other.said().contains("does not have a videos tab"));
        // And what isn't about what is asked for stays what it is.
        let blocked = classify("ERROR: HTTP Error 429: Too Many Requests", &[]);
        assert_eq!(of_a_channel(blocked).kind, Failure::Blocked);
    }

    const VIDEOS: &str = include_str!("fixtures/channel-videos.json");
    const SEARCH_VIDEOS: &str = include_str!("fixtures/search-videos.json");
    const SEARCH_CHANNELS: &str = include_str!("fixtures/search-channels.json");
    const FULL: &str = include_str!("fixtures/video-full.json");
    const OWNER: &str = "UCfixturefixturefixture0";

    fn owner() -> ChannelId {
        ChannelId::parse(OWNER).unwrap()
    }

    fn value(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    fn output(stdout: &str, stderr: &str) -> Output {
        Output {
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.to_owned(),
            timed_out: false,
        }
    }

    #[test]
    fn a_listing_reads() {
        let listing = parse_listing(&value(VIDEOS), Some(&owner()), Some(false)).unwrap();
        let info = listing.channel.unwrap();
        assert_eq!(info.title, "Fixture Channel");
        assert_eq!(info.handle.as_deref(), Some("@fixture"));
        assert_eq!(info.followers, Some(21_400_000));
        assert_eq!(
            info.avatar.as_deref(),
            Some("https://yt3.googleusercontent.com/picture=s176-c-k-c0x00ffffff-no-rj")
        );
        let ids: Vec<&str> = listing
            .videos
            .iter()
            .map(|video| video.id.as_str())
            .collect();
        assert_eq!(
            ids,
            ["-ideoFixtur", "upcomingVid", "liveVideoId", "strangeType"]
        );

        let first = &listing.videos[0];
        assert_eq!(first.title, "A video");
        assert_eq!(first.channel, "Fixture Channel");
        assert_eq!(first.channel_id, Some(owner()));
        assert_eq!(first.duration, Some(654));
        assert_eq!(first.views, Some(11_000_000));
        assert_eq!(first.shorts, Some(false));
        assert_eq!(
            first.published,
            Some(Published {
                at: 1_793_318_400,
                exact: false,
            })
        );
        assert_eq!(listing.videos[1].live, Some(Live::Upcoming));
        assert_eq!(listing.videos[2].live, Some(Live::Live));
        // Fields of strange types cost only themselves.
        let strange = &listing.videos[3];
        assert_eq!(strange.title, "");
        assert_eq!(strange.duration, None);
        assert_eq!(strange.views, None);
        assert_eq!(strange.published, None);
    }

    #[test]
    fn a_listing_about_another_channel_is_refused() {
        let other = ChannelId::parse("UCsomeoneelsesomeoneelse").unwrap();
        let error = parse_listing(&value(VIDEOS), Some(&other), None).unwrap_err();
        assert_eq!(error.kind, Failure::BadOutput);
    }

    #[test]
    fn what_is_not_a_list_is_refused() {
        for bad in [
            json!(null),
            json!([]),
            json!("x"),
            json!({}),
            json!({"entries": "x"}),
        ] {
            assert_eq!(
                parse_listing(&bad, None, None).unwrap_err().kind,
                Failure::BadOutput,
                "{bad}"
            );
        }
    }

    #[test]
    fn hostile_rows_are_dropped() {
        let listing = parse_listing(
            &json!({"channel_id": OWNER, "entries": [
                null, 7, "x", [], {},
                {"id": "bad"},
                {"id": 12345678901u64},
                {"id": "../../etc/x"},
                {"id": "abcdefghijk", "ie_key": "YoutubeTab"},
                {"id": "abcdefghijk", "title": "[Private video]"},
                {"id": "abcdefghijk", "title": "[Deleted video]"},
                {"id": "good0000000", "title": "a\u{202e}b\n\tc",
                 "duration": -1, "view_count": 1e300, "timestamp": 1e300,
                 "channel_id": "evil/../..", "url": "https://evil.example/shorts/x"},
            ]}),
            Some(&owner()),
            None,
        )
        .unwrap();
        assert_eq!(listing.videos.len(), 1);
        let video = &listing.videos[0];
        assert_eq!(video.title, "ab c");
        assert_eq!(video.duration, None);
        assert_eq!(video.views, None);
        assert_eq!(video.published, None);
        assert_eq!(video.channel_id, Some(owner()));
        // The address is never used; it only marks a Short.
        assert_eq!(video.shorts, Some(true));
    }

    #[test]
    fn a_huge_list_stops_at_a_limit() {
        let rows: Vec<Value> = (0..5000)
            .map(|index| json!({"id": format!("{index:011}")}))
            .collect();
        let listing = parse_listing(&json!({"entries": rows}), None, None).unwrap();
        assert_eq!(listing.videos.len(), 1000);
    }

    #[test]
    fn avatars_come_only_from_image_hosts() {
        let with = |url: &str| json!({"thumbnails": [{"url": url, "width": 176, "height": 176}]});
        for bad in [
            "https://evil.example/a.jpg",
            "http://yt3.ggpht.com/a",
            "//evil.example/a",
            "file:///etc/passwd",
            "not a url",
        ] {
            assert_eq!(avatar(&with(bad)), None, "{bad}");
        }
        assert_eq!(
            avatar(&with("//yt3.ggpht.com/a=s176")).as_deref(),
            Some("https://yt3.ggpht.com/a=s176")
        );
        // A banner isn't square.
        let banner = json!({"thumbnails": [{"url": "https://yt3.ggpht.com/b", "width": 1060, "height": 175}]});
        assert_eq!(avatar(&banner), None);
        assert_eq!(avatar(&json!({"thumbnails": 3})), None);
    }

    #[test]
    fn search_rows_read() {
        let videos: Vec<Video> = entries(&value(SEARCH_VIDEOS))
            .iter()
            .filter_map(|entry| video_of(entry, None, None))
            .collect();
        assert_eq!(videos.len(), 2);
        assert_eq!(videos[0].channel, "Someone");
        assert_eq!(
            videos[0].channel_id,
            ChannelId::parse("UCsomeoneelsesomeoneelse")
        );
        assert_eq!(videos[0].shorts, None, "search doesn't say");
        assert_eq!(videos[0].published, None);

        let channels: Vec<ChannelInfo> = entries(&value(SEARCH_CHANNELS))
            .iter()
            .filter_map(channel_of_entry)
            .collect();
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].title, "Someone");
        assert_eq!(channels[0].handle.as_deref(), Some("@someone"));
        assert_eq!(channels[0].followers, Some(322_000));
        assert_eq!(
            channels[0].avatar.as_deref(),
            Some("https://yt3.ggpht.com/picture=s176-c-k-c0x00ffffff-no-rj-mo")
        );
    }

    #[test]
    fn a_full_answer_reads_with_an_exact_date() {
        let id = VideoId::parse("-ideoFixtur").unwrap();
        let video = parse_video(&value(FULL), &id).unwrap();
        assert_eq!(video.title, "A video");
        assert_eq!(video.channel, "Fixture Channel");
        assert_eq!(video.channel_id, Some(owner()));
        assert_eq!(video.duration, Some(654));
        assert_eq!(
            video.published,
            Some(Published {
                at: 1_793_594_400,
                exact: true,
            })
        );
        let other = VideoId::parse("abcdefghijk").unwrap();
        assert_eq!(
            parse_video(&value(FULL), &other).unwrap_err().kind,
            Failure::BadOutput
        );
    }

    #[test]
    fn a_video_youtube_does_not_have_is_unavailable_and_gets_no_made_up_title() {
        let id = VideoId::parse("-abcdefghij").unwrap();
        let answer =
            r#"{"id": "-abcdefghij", "title": "youtube video #-abcdefghij", "formats": []}"#;
        assert_eq!(
            parse_video(&value(answer), &id).unwrap_err().kind,
            Failure::Unavailable
        );
    }

    #[test]
    fn lines_are_matched_to_addresses_by_position() {
        let tags = ["UCfirst", "UCbad", "UCthird"];
        let out = output(
            "{\"a\":1}\nnull\n{\"a\":3}\n",
            "WARNING: [youtube:tab] YouTube said: This channel does not exist.\n\
             ERROR: [youtube:tab] UCbad: YouTube said: This channel does not exist.\n",
        );
        let found = answers(&out, 3, &tags, &[]);
        assert_eq!(found[0], Ok(json!({"a": 1})));
        assert_eq!(found[1].as_ref().unwrap_err().kind, Failure::NoSuchChannel);
        assert_eq!(found[2], Ok(json!({"a": 3})));
    }

    #[test]
    fn errors_go_to_the_address_they_mention() {
        let tags = ["UCone", "UCtwo", "UCthree"];
        let out = output(
            "null\nnull\nnull\n",
            "ERROR: [youtube:tab] UCtwo: YouTube said: This channel does not exist.\n\
             ERROR: [youtube:tab] UCthree: Unable to download API page: timed out\n\
             ERROR: Sign in to confirm you're not a bot\n",
        );
        let found = answers(&out, 3, &tags, &[]);
        assert_eq!(found[0].as_ref().unwrap_err().kind, Failure::Blocked);
        assert_eq!(found[1].as_ref().unwrap_err().kind, Failure::NoSuchChannel);
        assert_eq!(found[2].as_ref().unwrap_err().kind, Failure::Network);
    }

    #[test]
    fn too_few_lines_are_not_shifted_onto_other_addresses() {
        let found = answers(&output("{\"a\":1}\n", ""), 3, &["a", "b", "c"], &[]);
        assert_eq!(found[0], Ok(json!({"a": 1})));
        assert_eq!(found[1].as_ref().unwrap_err().kind, Failure::BadOutput);
        assert_eq!(found[2].as_ref().unwrap_err().kind, Failure::BadOutput);
    }

    #[test]
    fn too_many_lines_and_garbage_cost_only_themselves() {
        let found = answers(
            &output("not json\n{\"a\":2}\n{\"a\":3}\n", ""),
            2,
            &["a", "b"],
            &[],
        );
        assert_eq!(found[0].as_ref().unwrap_err().kind, Failure::BadOutput);
        assert_eq!(found[1], Ok(json!({"a": 2})));
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn a_null_with_nothing_said_is_a_plain_failure() {
        let found = answers(&output("null\n", ""), 1, &["a"], &[]);
        assert_eq!(found[0].as_ref().unwrap_err().kind, Failure::Failed);
        let mut timed = output("", "");
        timed.timed_out = true;
        let found = answers(&timed, 1, &["a"], &[]);
        assert_eq!(found[0].as_ref().unwrap_err().kind, Failure::Timeout);
    }

    #[test]
    fn errors_get_their_names() {
        // Lines as yt-dlp has printed them, or shaped like them.
        for (line, kind) in [
            (
                "ERROR: [youtube:tab] UCxxxxxxxxxxxxxxxxxxxxxx: YouTube said: This channel does not exist.",
                Failure::NoSuchChannel,
            ),
            (
                "ERROR: [youtube] abc: Sign in to confirm you're not a bot. Use --cookies-from-browser",
                Failure::Blocked,
            ),
            (
                "ERROR: [youtube] abc: Sign in to confirm your age. This video may be inappropriate",
                Failure::AgeRestricted,
            ),
            (
                "ERROR: [youtube:tab] x: HTTP Error 429: Too Many Requests",
                Failure::Blocked,
            ),
            (
                "ERROR: [youtube] abc: The current session has been rate-limited by YouTube for up to an hour.",
                Failure::Blocked,
            ),
            (
                "ERROR: unable to download video data: HTTP Error 403: Forbidden",
                Failure::Blocked,
            ),
            (
                "ERROR: [youtube:tab] x: Unable to download API page: <urlopen error [Errno -2] Name or service not known>",
                Failure::Network,
            ),
            (
                "ERROR: [youtube:tab] x: Unable to download webpage: <urlopen error [Errno 101] Network is unreachable>",
                Failure::Network,
            ),
            (
                "ERROR: x: Read timed out. (read timeout=10)",
                Failure::Network,
            ),
            (
                "ERROR: x: Unable to download webpage: [SSL: CERTIFICATE_VERIFY_FAILED] certificate verify failed",
                Failure::Network,
            ),
            (
                "ERROR: x: Unable to download webpage: Tunnel connection failed: 407 Proxy Authentication Required",
                Failure::Proxy,
            ),
            (
                "ERROR: [youtube] abc: Private video. Sign in if you've been granted access",
                Failure::Private,
            ),
            (
                "ERROR: [youtube] abc: Video unavailable",
                Failure::Unavailable,
            ),
            (
                "ERROR: [youtube] abc: Join this channel to get access to members-only content",
                Failure::MembersOnly,
            ),
            (
                "ERROR: [youtube] abc: This live event will begin in 3 hours.",
                Failure::NotStarted,
            ),
            (
                "ERROR: [youtube] abc: Unable to extract player response",
                Failure::Outdated,
            ),
            (
                "ERROR: Unable to handle request: Unsupported url scheme: \"ytsearchdate5\" (requests, urllib)",
                Failure::Outdated,
            ),
            (
                "yt-dlp: error: no such option: --flat-playlist",
                Failure::Outdated,
            ),
            ("ERROR: something nobody has seen before", Failure::Failed),
            (
                "ERROR: The certificate file does not exist",
                Failure::Failed,
            ),
        ] {
            assert_eq!(classify(line, &[]).kind, kind, "{line}");
        }
    }

    /// Each line of the fixture is a failure's name and what yt-dlp says for it,
    /// as near as could be had without such a video to hand.
    #[test]
    fn what_ytdlp_says_of_a_video_is_named() {
        let mut seen = Vec::new();
        for row in include_str!("fixtures/video-errors.txt").lines() {
            if row.is_empty() || row.starts_with('#') {
                continue;
            }
            let (name, line) = row.split_once('|').unwrap();
            let kind = match name {
                "private" => Failure::Private,
                "age" => Failure::AgeRestricted,
                "region" => Failure::RegionLocked,
                "members" => Failure::MembersOnly,
                "removed" => Failure::Removed,
                "not-started" => Failure::NotStarted,
                "unavailable" => Failure::Unavailable,
                "blocked" => Failure::Blocked,
                "generic" => Failure::Failed,
                _ => panic!("{name}"),
            };
            if !seen.contains(&kind) {
                seen.push(kind);
            }
            let error = failure_of(line);
            assert_eq!(error.kind, kind, "{line}");
            if kind == Failure::Failed {
                // The catch-all says what yt-dlp said, without its prefix.
                assert!(!error.said().starts_with("ERROR"), "{}", error.said());
                assert!(error.said().contains("something"), "{}", error.said());
            } else {
                // Not yt-dlp's advice, which is about cookies and proxies.
                assert_eq!(error.said(), kind.message());
                assert!(!error.said().contains("cookies"));
            }
        }
        assert_eq!(seen.len(), 9, "a failure has no fixture: {seen:?}");
    }

    #[test]
    fn what_the_hook_says_is_read_a_message_at_a_time() {
        assert_eq!(hook_failure(""), None);
        assert_eq!(
            hook_failure("youtube-dl failed: unexpected error occurred"),
            None
        );
        assert_eq!(
            hook_failure("youtube-dl failed: not found or not enough permissions")
                .unwrap()
                .kind,
            Failure::Missing
        );
        assert_eq!(
            hook_failure("ERROR: [youtube] abc: Private video. Sign in\nmore")
                .unwrap()
                .kind,
            Failure::Private
        );
    }

    #[test]
    fn details_keep_nothing_they_should_not() {
        let line = "ERROR: failed https://r1---sn.googlevideo.com/videoplayback?ip=203.0.113.9&x=1 \
                    and ip=203.0.113.9 through http://user:secret@proxy:1 for my private search";
        let error = classify(line, &["http://user:secret@proxy:1", "my private search"]);
        assert_eq!(
            error.detail,
            "failed [address] and [address] through […] for […]",
        );
        let error = classify("ERROR: for my private search", &["my private search"]);
        assert_eq!(error.detail, "for […]");
        assert!(
            classify(&format!("ERROR: {}", "x".repeat(9000)), &[])
                .detail
                .len()
                <= 300
        );
    }

    #[test]
    fn versions_tell_their_age() {
        let today = jiff::civil::date(2026, 11, 2);
        let version = Version::read("2026.10.26\n", today);
        assert_eq!(version.text, "2026.10.26");
        assert_eq!(version.age_days, Some(7));
        assert!(!version.stale);
        let nightly = Version::read("2026.10.26.233045", today);
        assert_eq!(nightly.age_days, Some(7));
        let old = Version::read("2026.01.29", today);
        assert_eq!(old.age_days, Some(277));
        assert!(old.stale);
        // At the edge, then just past it.
        assert!(!Version::read("2026.08.04", today).stale);
        assert!(Version::read("2026.08.03", today).stale);
        // Anything else has no age, and isn't called stale.
        for text in ["not a version", "", "9999", "2026.13.40", "2026.1"] {
            let version = Version::read(text, today);
            assert_eq!(version.age_days, None, "{text}");
            assert!(!version.stale);
        }
    }

    #[test]
    fn preferences_find_yt_dlp_or_say_why_not() {
        let folder =
            std::env::temp_dir().join(format!("patina-ytdlp-prefs-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let off = Preferences {
            ytdl: false,
            ..Default::default()
        };
        assert_eq!(
            Ytdlp::from_prefs(&off, &folder).unwrap_err().kind,
            Failure::Disabled
        );
        let missing = Preferences {
            ytdl_path: "/nonexistent/yt-dlp".into(),
            ..Default::default()
        };
        assert_eq!(
            Ytdlp::from_prefs(&missing, &folder).unwrap_err().kind,
            Failure::Missing
        );
        let name = if cfg!(windows) {
            "youtube-dl.exe"
        } else {
            "youtube-dl"
        };
        std::fs::write(folder.join(name), b"").unwrap();
        let old = Preferences {
            ytdl_path: "youtube-dl".into(),
            ..Default::default()
        };
        assert_eq!(
            Ytdlp::from_prefs(&old, &folder).unwrap_err().kind,
            Failure::Outdated
        );
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn a_program_that_is_not_there_is_named() {
        let tool = Ytdlp::new(PathBuf::from("/nonexistent/yt-dlp"), None);
        let error = tool.search("anything", &Cancel::default()).unwrap_err();
        assert_eq!(error.kind, Failure::Missing);
        for answer in tool.listings(&[owner(), owner()], &Cancel::default()) {
            assert_eq!(answer.unwrap_err().kind, Failure::Missing);
        }
    }

    #[cfg(unix)]
    mod with_a_fake {
        use super::super::testing::Fake;
        use super::*;

        const RESOLVE: &str = include_str!("fixtures/resolve.json");

        struct Folder(PathBuf);

        impl Folder {
            fn new(name: &str) -> Self {
                let path = std::env::temp_dir()
                    .join(format!("patina-ytdlp-{name}-{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&path);
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }
        }

        impl Drop for Folder {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        fn tool(fake: &Fake, proxy: Option<&str>) -> Ytdlp {
            Ytdlp::new(fake.program.clone(), proxy.map(str::to_owned))
        }

        /// Prints the file's text as the fake's output.
        fn printing(folder: &Folder, text: &str) -> String {
            let file = folder.0.join("output");
            std::fs::write(&file, text).unwrap();
            format!("cat '{}'", file.display())
        }

        #[test]
        fn a_batch_maps_channels_by_position_despite_exit_1() {
            let folder = Folder::new("batch");
            let videos = value(VIDEOS).to_string();
            let body = printing(&folder, &format!("{videos}\nnull\n"));
            let fake = Fake::new(
                &folder.0,
                &format!(
                    "{body}\necho 'ERROR: [youtube:tab] UCmissingmissingmissing0: YouTube said: \
                     This channel does not exist.' >&2\nexit 1"
                ),
            );
            let gone = ChannelId::parse("UCmissingmissingmissing0").unwrap();
            let results = tool(&fake, None).listings(&[owner(), gone], &Cancel::default());
            assert_eq!(results[0].as_ref().unwrap().videos.len(), 4);
            assert_eq!(
                results[1].as_ref().unwrap_err().kind,
                Failure::NoSuchChannel
            );
            let runs = fake.runs();
            assert_eq!(runs.len(), 1, "one process for the batch");
            let args = &runs[0];
            let dashes = args.iter().position(|arg| arg == "--").unwrap();
            assert_eq!(
                args[dashes + 1..],
                [
                    format!("https://www.youtube.com/channel/{OWNER}/videos"),
                    "https://www.youtube.com/channel/UCmissingmissingmissing0/videos".to_owned(),
                ]
            );
        }

        #[test]
        fn calls_never_carry_the_users_options_and_do_carry_the_proxy() {
            let folder = Folder::new("args");
            let fake = Fake::new(&folder.0, "exit 1");
            let runner = tool(&fake, Some("http://proxy.example:8080"));
            let cancel = Cancel::default();
            let _ = runner.search("a query", &cancel);
            let _ = runner.listing(&owner(), &cancel);
            let _ = runner.resolve(&ChannelInput::Handle("name".into()), &cancel);
            let _ = runner.video(&VideoId::parse("-ideoFixtur").unwrap(), &cancel);
            let runs = fake.runs();
            assert_eq!(runs.len(), 4);
            for args in &runs {
                assert_eq!(args[0], "--ignore-config", "{args:?}");
                let proxy = args.iter().position(|arg| arg == "--proxy").unwrap();
                assert_eq!(args[proxy + 1], "http://proxy.example:8080");
                assert!(args.iter().any(|arg| arg == "--"));
                assert!(!args.iter().any(|arg| arg.contains("cookies")), "{args:?}");
            }
            // With no proxy set, no option at all, so that the system's apply.
            let _ = tool(&fake, None).listing(&owner(), &cancel);
            assert!(!fake.runs()[4].iter().any(|arg| arg == "--proxy"));
            // Empty goes direct, past the system's.
            let _ = tool(&fake, Some("")).listing(&owner(), &cancel);
            let last = &fake.runs()[5];
            let proxy = last.iter().position(|arg| arg == "--proxy").unwrap();
            assert_eq!(last[proxy + 1], "");
        }

        #[test]
        fn what_looks_like_an_option_stays_behind_the_dashes() {
            let folder = Folder::new("dashes");
            let fake = Fake::new(&folder.0, "exit 1");
            let runner = tool(&fake, None);
            let cancel = Cancel::default();
            let _ = runner.search("--exec rm", &cancel);
            let _ = runner.video(&VideoId::parse("-ideoFixtur").unwrap(), &cancel);
            let runs = fake.runs();
            let dashes = runs[0].iter().position(|arg| arg == "--").unwrap();
            assert_eq!(runs[0][dashes + 1], "ytsearch30:--exec rm");
            assert_eq!(
                runs[0][dashes + 2],
                "https://www.youtube.com/results?search_query=--exec+rm&sp=EgIQAg%3D%3D"
            );
            let dashes = runs[1].iter().position(|arg| arg == "--").unwrap();
            assert_eq!(
                runs[1][dashes + 1],
                "https://www.youtube.com/watch?v=-ideoFixtur"
            );
        }

        #[test]
        fn search_gives_videos_and_channels() {
            let folder = Folder::new("search");
            let body = printing(
                &folder,
                &format!("{}\n{}\n", value(SEARCH_VIDEOS), value(SEARCH_CHANNELS)),
            );
            let fake = Fake::new(&folder.0, &body);
            let results = tool(&fake, None)
                .search("a query", &Cancel::default())
                .unwrap();
            assert_eq!(results.videos.len(), 2);
            assert_eq!(results.channels.len(), 1);
        }

        #[test]
        fn search_survives_one_half_failing_but_not_both() {
            let folder = Folder::new("search-half");
            let body = printing(&folder, &format!("null\n{}\n", value(SEARCH_CHANNELS)));
            let fake = Fake::new(&folder.0, &body);
            let results = tool(&fake, None)
                .search("a query", &Cancel::default())
                .unwrap();
            assert!(results.videos.is_empty());
            assert_eq!(results.channels.len(), 1);

            let fake = Fake::new(
                &folder.0,
                "printf 'null\\nnull\\n'\necho 'ERROR: HTTP Error 429: Too Many Requests' >&2\nexit 1",
            );
            let error = tool(&fake, None)
                .search("a query", &Cancel::default())
                .unwrap_err();
            assert_eq!(error.kind, Failure::Blocked);
        }

        #[test]
        fn an_empty_search_asks_nothing() {
            let folder = Folder::new("search-empty");
            let fake = Fake::new(&folder.0, "exit 1");
            let results = tool(&fake, None)
                .search(" \n\t ", &Cancel::default())
                .unwrap();
            assert_eq!(results, SearchResults::default());
            assert!(fake.runs().is_empty());
        }

        #[test]
        fn a_query_is_not_repeated_in_errors() {
            let folder = Folder::new("search-secret");
            let fake = Fake::new(
                &folder.0,
                "echo 'ERROR: nothing for my private search' >&2\nexit 1",
            );
            let error = tool(&fake, None)
                .search("my private search", &Cancel::default())
                .unwrap_err();
            assert!(!format!("{error:?}").contains("private"), "{error:?}");
        }

        #[test]
        fn resolving_gives_the_channel() {
            let folder = Folder::new("resolve");
            let body = printing(&folder, &format!("{}\n", value(RESOLVE)));
            let fake = Fake::new(&folder.0, &body);
            let input = ChannelInput::parse("@fixture").unwrap();
            let info = tool(&fake, None)
                .resolve(&input, &Cancel::default())
                .unwrap();
            assert_eq!(info.id, owner());
            assert_eq!(info.title, "Fixture Channel");
            let args = &fake.runs()[0];
            assert!(args.iter().any(|arg| arg == "--playlist-items"));
            assert_eq!(
                args.last().unwrap(),
                "https://www.youtube.com/@fixture/videos"
            );
        }

        #[test]
        fn resolving_a_channel_with_a_bad_id_fails() {
            let folder = Folder::new("resolve-bad");
            let body = printing(&folder, "{\"channel_id\":\"evil\",\"entries\":[]}\n");
            let fake = Fake::new(&folder.0, &body);
            let input = ChannelInput::parse("@fixture").unwrap();
            let error = tool(&fake, None)
                .resolve(&input, &Cancel::default())
                .unwrap_err();
            assert_eq!(error.kind, Failure::BadOutput);
        }

        #[test]
        fn details_come_for_a_video() {
            let folder = Folder::new("video");
            let body = printing(&folder, &format!("{}\n", value(FULL)));
            let fake = Fake::new(&folder.0, &body);
            let id = VideoId::parse("-ideoFixtur").unwrap();
            let video = tool(&fake, None).video(&id, &Cancel::default()).unwrap();
            assert_eq!(video.title, "A video");
        }

        #[test]
        fn a_slow_program_is_stopped_at_its_limit() {
            let folder = Folder::new("slow");
            let fake = Fake::new(&folder.0, "printf 'partial'\nexec sleep 30");
            let started = Instant::now();
            let output = tool(&fake, None)
                .run(&[], Duration::from_millis(100), &Cancel::default())
                .unwrap();
            assert!(output.timed_out);
            assert!(started.elapsed() < Duration::from_secs(10));
        }

        #[test]
        fn a_cancel_stops_a_program() {
            let folder = Folder::new("cancel");
            let fake = Fake::new(&folder.0, "exec sleep 30");
            let cancel = Cancel::default();
            let canceller = cancel.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                canceller.cancel();
            });
            let started = Instant::now();
            let results = tool(&fake, None).listings(&[owner()], &cancel);
            assert_eq!(results[0].as_ref().unwrap_err().kind, Failure::Cancelled);
            assert!(started.elapsed() < Duration::from_secs(10));
        }

        #[test]
        fn a_child_holding_the_pipes_open_does_not_hold_up_the_answer() {
            let folder = Folder::new("held");
            // What Deno could do: outlive yt-dlp, with its pipes.
            let fake = Fake::new(&folder.0, "sleep 8 &\nprintf 'null\\n'\nexit 1");
            let started = Instant::now();
            let results = tool(&fake, None).listings(&[owner()], &Cancel::default());
            assert!(results[0].is_err());
            assert!(started.elapsed() < Duration::from_secs(5));
        }

        #[test]
        fn endless_output_stops_at_a_limit() {
            let folder = Folder::new("endless");
            let fake = Fake::new(&folder.0, "exec yes");
            let results = tool(&fake, None).listings(&[owner()], &Cancel::default());
            assert_eq!(results[0].as_ref().unwrap_err().kind, Failure::BadOutput);
        }

        #[test]
        fn the_version_is_asked_without_a_network() {
            let folder = Folder::new("version");
            let fake = Fake::new(&folder.0, "echo 2026.10.26");
            let version = tool(&fake, None)
                .version(jiff::civil::date(2026, 11, 2), &Cancel::default())
                .unwrap();
            assert_eq!(version.text, "2026.10.26");
            assert_eq!(version.age_days, Some(7));
        }
    }
}
