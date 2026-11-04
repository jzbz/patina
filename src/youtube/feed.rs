//! The subscriptions feed: built from sources by `build`, and kept between runs
//! by `SavedFeed`, so that it shows at once and nothing waits on the network.
//!
//! The channel feed comes first, as it has exact dates and costs little. It is
//! unreliable, though (`rss.rs`), so a circuit breaker stops asking it after a
//! few bad answers in a row in one refresh, and the channels left go through
//! yt-dlp in batches, at low concurrency. During an outage that is the main path. A
//! channel the first source failed on goes through yt-dlp too.
//!
//! One channel's failure stays its own, and costs it nothing known: its videos
//! stay, and its state says what went wrong and when it last worked. A failure
//! that would be the same for everyone (a block, no yt-dlp, no network) ends
//! the asking at once rather than repeating it for hundreds of channels.
//!
//! This file can be lost at any time, so it is less guarded than the store.

use super::store::write_atomically;
use super::{Cancel, ChannelId, ChannelInfo, Error, Failure, Listing, Published, Video, VideoId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

/// The feed is refreshed on opening the section when it is older than this.
const REFRESH_AFTER_SECS: i64 = 30 * 60;
/// After YouTube has refused, opening the section leaves it alone for this
/// long. Asking again by hand still works.
const BLOCKED_PAUSE_SECS: i64 = 60 * 60;
/// How many videos are kept per channel. A channel can't then fill the file,
/// and the newest stay.
const KEEP_PER_CHANNEL: usize = 200;
const SCHEMA: u64 = 1;
/// How much longer a run of bad answers has to be once the first source has
/// shown that it works.
const AFTER_A_GOOD_ANSWER: usize = 3;

/// Somewhere a channel's newest videos come from.
pub trait Source: Send + Sync {
    /// How many channels one `fetch` takes.
    fn batch(&self) -> usize {
        1
    }

    /// How many `fetch`es run at once.
    fn workers(&self) -> usize;

    /// An answer for each of `channels`, in the same order.
    fn fetch(&self, channels: &[ChannelId], cancel: &Cancel) -> Vec<Result<Listing, Error>>;
}

/// A source that can't be used, which says why for every channel.
pub struct Refused(pub Error);

impl Source for Refused {
    fn batch(&self) -> usize {
        usize::MAX
    }

    fn workers(&self) -> usize {
        1
    }

    fn fetch(&self, channels: &[ChannelId], _cancel: &Cancel) -> Vec<Result<Listing, Error>> {
        channels.iter().map(|_| Err(self.0.clone())).collect()
    }
}

/// What holds a refresh back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bad answers in a row from the first source before it isn't asked any
    /// more, while it hasn't answered properly yet. After a good answer it
    /// takes `AFTER_A_GOOD_ANSWER` times as many, as dead channels can lie side
    /// by side in an old list and say nothing of the feed server.
    pub breaker: usize,
    /// The pause between one worker's batches on the second source.
    pub batch_gap: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            breaker: 3,
            batch_gap: Duration::from_secs(1),
        }
    }
}

/// One channel's answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub channel: ChannelId,
    pub result: Result<Listing, Error>,
}

/// A failure of the first source that counts towards stopping it: any that says
/// nothing about the channel itself.
fn trips_breaker(kind: Failure) -> bool {
    matches!(
        kind,
        Failure::NotFeed | Failure::Network | Failure::Timeout | Failure::Proxy
    )
}

/// A failure of the second source that will be the same for every channel, so
/// that asking on is pointless.
fn ends_asking(kind: Failure) -> bool {
    matches!(
        kind,
        Failure::Blocked | Failure::Missing | Failure::Disabled
    )
}

/// A failure which ends the asking only when a whole batch has it: on its own
/// it can be one bad channel.
fn ends_asking_as_a_batch(kind: Failure) -> bool {
    matches!(
        kind,
        Failure::Network | Failure::Timeout | Failure::Outdated | Failure::Proxy
    )
}

/// Asks about every channel once, and hands each answer to `report` as it
/// comes, on whichever thread it came on. Nothing is reported for a channel
/// that a cancel stopped.
pub fn build(
    channels: &[ChannelId],
    first: &dyn Source,
    second: &dyn Source,
    limits: Limits,
    cancel: &Cancel,
    report: &(dyn Fn(Outcome) + Sync),
) {
    let mut seen = HashSet::new();
    let channels: Vec<ChannelId> = channels
        .iter()
        .filter(|channel| seen.insert(*channel))
        .cloned()
        .collect();
    let left = ask_first(&channels, first, limits, cancel, report);
    if !cancel.cancelled() {
        ask_second(&left, second, limits, cancel, report);
    }
}

/// The first source, up to the breaker; gives the channels it didn't answer.
fn ask_first(
    channels: &[ChannelId],
    source: &dyn Source,
    limits: Limits,
    cancel: &Cancel,
    report: &(dyn Fn(Outcome) + Sync),
) -> Vec<ChannelId> {
    let next = AtomicUsize::new(0);
    // Bad answers since the last good one. Once it has been seen working, it
    // takes a longer run to say that it has stopped.
    let in_a_row = AtomicUsize::new(0);
    let worked = AtomicBool::new(false);
    let tripped = || {
        let limit = if worked.load(Ordering::Relaxed) {
            limits.breaker.saturating_mul(AFTER_A_GOOD_ANSWER)
        } else {
            limits.breaker
        };
        in_a_row.load(Ordering::Relaxed) >= limit
    };
    let failed = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..source.workers().clamp(1, channels.len().max(1)) {
            scope.spawn(|| {
                while !cancel.cancelled() && !tripped() {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(channel) = channels.get(index) else {
                        return;
                    };
                    let answer = source
                        .fetch(std::slice::from_ref(channel), cancel)
                        .pop()
                        .unwrap_or_else(|| Err(Failure::BadOutput.into()));
                    match answer {
                        Ok(listing) => {
                            worked.store(true, Ordering::Relaxed);
                            in_a_row.store(0, Ordering::Relaxed);
                            report(Outcome {
                                channel: channel.clone(),
                                result: Ok(listing),
                            });
                        }
                        Err(error) => {
                            if trips_breaker(error.kind) {
                                in_a_row.fetch_add(1, Ordering::Relaxed);
                            }
                            failed
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .push(index);
                        }
                    }
                }
            });
        }
    });
    let mut failed = failed
        .into_inner()
        .unwrap_or_else(|error| error.into_inner());
    let asked = next.load(Ordering::Relaxed).min(channels.len());
    failed.extend(asked..channels.len());
    failed.sort_unstable();
    failed
        .into_iter()
        .map(|index| channels[index].clone())
        .collect()
}

/// The second source, for what is left, a batch at a time.
fn ask_second(
    channels: &[ChannelId],
    source: &dyn Source,
    limits: Limits,
    cancel: &Cancel,
    report: &(dyn Fn(Outcome) + Sync),
) {
    let batches: Vec<&[ChannelId]> = channels.chunks(source.batch().max(1)).collect();
    let next = AtomicUsize::new(0);
    let ended: Mutex<Option<Error>> = Mutex::new(None);
    let ended_yet = || {
        ended
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_some()
    };
    std::thread::scope(|scope| {
        for _ in 0..source.workers().clamp(1, batches.len().max(1)) {
            scope.spawn(|| {
                let mut worked = false;
                loop {
                    // The pause comes before a batch is taken: what the other
                    // worker hears meanwhile, a block above all, holds for this
                    // one, and the batch stays with those not asked.
                    if worked
                        && next.load(Ordering::Relaxed) < batches.len()
                        && !cancel.sleep(limits.batch_gap)
                    {
                        return;
                    }
                    if cancel.cancelled() || ended_yet() {
                        return;
                    }
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(batch) = batches.get(index) else {
                        return;
                    };
                    worked = true;
                    let mut answers = source.fetch(batch, cancel);
                    answers.truncate(batch.len());
                    answers.resize_with(batch.len(), || Err(Failure::BadOutput.into()));
                    let first_failure = answers.iter().find_map(|answer| answer.as_ref().err());
                    let whole = answers.iter().all(|answer| {
                        answer.as_ref().err().map(|error| error.kind)
                            == first_failure.map(|error| error.kind)
                    });
                    let end = answers
                        .iter()
                        .filter_map(|answer| answer.as_ref().err())
                        .find(|error| ends_asking(error.kind))
                        .or(first_failure
                            .filter(|error| whole && ends_asking_as_a_batch(error.kind)))
                        .cloned();
                    for (channel, result) in batch.iter().zip(answers) {
                        if !matches!(&result, Err(error) if error.kind == Failure::Cancelled) {
                            report(Outcome {
                                channel: channel.clone(),
                                result,
                            });
                        }
                    }
                    if let Some(end) = end {
                        ended
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .get_or_insert(end);
                    }
                }
            });
        }
    });
    let ended = ended
        .into_inner()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(error) = ended.filter(|_| !cancel.cancelled()) {
        let asked = next.load(Ordering::Relaxed).min(batches.len());
        for channel in batches[asked..].iter().copied().flatten() {
            report(Outcome {
                channel: channel.clone(),
                result: Err(error.clone()),
            });
        }
    }
}

/// How a channel has been doing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChannelState {
    /// When an answer last came, in seconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ok: Option<i64>,
    /// When it was last asked about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_try: Option<i64>,
    /// What went wrong the last time, if it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<Failure>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// yt-dlp said the channel doesn't exist. It is only marked: it might be
    /// back, and the user decides. Nothing else can set this.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub gone: bool,
}

/// A video in the feed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedVideo {
    #[serde(flatten)]
    pub video: Video,
    /// When this first appeared here, which orders videos with no date.
    #[serde(default)]
    pub first_seen: i64,
}

impl FeedVideo {
    /// When it came out, or the best that can be said.
    fn time(&self) -> i64 {
        self.video
            .published
            .map_or(self.first_seen, |published: Published| published.at)
    }
}

#[derive(Debug, Serialize)]
struct File<'a> {
    schema: u64,
    last_refresh: Option<i64>,
    blocked_at: Option<i64>,
    channels: &'a BTreeMap<ChannelId, ChannelState>,
    videos: Vec<&'a FeedVideo>,
}

/// The feed as last seen.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SavedFeed {
    videos: BTreeMap<VideoId, FeedVideo>,
    channels: BTreeMap<ChannelId, ChannelState>,
    last_refresh: Option<i64>,
    blocked_at: Option<i64>,
}

impl SavedFeed {
    /// The saved feed, or an empty one when there is none to read. A file from
    /// a later Patina is treated as none: this can be fetched again.
    pub fn load(path: &Path) -> Self {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("could not read {}: {error}", path.display());
                }
                return Self::default();
            }
        };
        Self::read(&bytes).unwrap_or_else(|| {
            log::warn!("starting over from an unreadable {}", path.display());
            Self::default()
        })
    }

    fn read(bytes: &[u8]) -> Option<Self> {
        let value: Value = serde_json::from_slice(bytes).ok()?;
        if value.get("schema").and_then(Value::as_u64) != Some(SCHEMA) {
            return None;
        }
        let time = |key: &str| value.get(key).and_then(Value::as_i64);
        let videos = value
            .get("videos")?
            .as_array()?
            .iter()
            .filter_map(|row| serde_json::from_value::<FeedVideo>(row.clone()).ok())
            .map(|row| (row.video.id.clone(), row))
            .collect();
        let channels = value
            .get("channels")?
            .as_object()?
            .iter()
            .filter_map(|(id, state)| {
                Some((
                    ChannelId::parse(id)?,
                    serde_json::from_value::<ChannelState>(state.clone()).ok()?,
                ))
            })
            .collect();
        Some(Self {
            videos,
            channels,
            last_refresh: time("last_refresh"),
            blocked_at: time("blocked_at"),
        })
    }

    /// The file's contents. Made on the interface thread and written on another.
    pub fn to_bytes(&self) -> Vec<u8> {
        let file = File {
            schema: SCHEMA,
            last_refresh: self.last_refresh,
            blocked_at: self.blocked_at,
            channels: &self.channels,
            videos: self.videos.values().collect(),
        };
        serde_json::to_vec(&file).unwrap_or_default()
    }

    /// Writes the contents `to_bytes` made.
    pub fn write(path: &Path, bytes: &[u8]) {
        if let Err(error) = write_atomically(path, bytes) {
            log::warn!("could not save {}: {error}", path.display());
        }
    }

    /// Whether opening the section should refresh. A clock set back makes the
    /// last refresh look to be in the future, which counts as old.
    pub fn due(&self, now: i64) -> bool {
        !self.is_blocked(now)
            && self
                .last_refresh
                .is_none_or(|last| last > now || now - last > REFRESH_AFTER_SECS)
    }

    fn is_blocked(&self, now: i64) -> bool {
        self.blocked_at
            .is_some_and(|at| at <= now && now - at < BLOCKED_PAUSE_SECS)
    }

    pub fn last_refresh(&self) -> Option<i64> {
        self.last_refresh
    }

    /// An update of every channel has begun, by hand or because it was due, so
    /// that an old refusal no longer holds it back.
    pub fn started(&mut self) {
        self.blocked_at = None;
    }

    /// An update begun at `at` reached its end. Only that counts as a check:
    /// one stopped part of the way, by leaving, quitting or a crash, leaves the
    /// file saying what is true, and opening the section again finds it due.
    pub fn finished(&mut self, at: i64) {
        self.last_refresh = Some(at);
    }

    /// Which of `channels` opening the section should ask about, and whether
    /// that amounts to the update that is due. Those answered within the update's
    /// time need no asking again, which lets an update stopped part of the way
    /// carry on from there. When none is due, it is only those never asked:
    /// their fetch was stopped before it reached them. A refusal holds all this
    /// back for its time.
    pub fn to_ask(&self, channels: &[ChannelId], now: i64) -> (Vec<ChannelId>, bool) {
        if self.is_blocked(now) {
            return (Vec::new(), false);
        }
        let due = self.due(now);
        let wanted = channels
            .iter()
            .filter(|channel| match self.channels.get(*channel) {
                None => true,
                Some(state) => {
                    due && state
                        .last_ok
                        .is_none_or(|at| at > now || now - at > REFRESH_AFTER_SECS)
                }
            })
            .cloned()
            .collect();
        (wanted, due)
    }

    pub fn state(&self, channel: &ChannelId) -> Option<&ChannelState> {
        self.channels.get(channel)
    }

    /// Takes in a channel's answer, and gives what it says of the channel.
    pub fn apply(&mut self, outcome: Outcome, now: i64) -> Option<ChannelInfo> {
        let state = self.channels.entry(outcome.channel.clone()).or_default();
        state.last_try = Some(now);
        match outcome.result {
            Ok(listing) => {
                state.last_ok = Some(now);
                state.failure = None;
                state.detail.clear();
                state.gone = false;
                self.merge(&outcome.channel, listing.videos, now);
                listing.channel
            }
            Err(error) => {
                state.failure = Some(error.kind);
                state.gone = error.kind == Failure::NoSuchChannel;
                state.detail = error.detail;
                if error.kind == Failure::Blocked {
                    self.blocked_at = Some(now);
                }
                None
            }
        }
    }

    /// Adds a channel's videos, losing nothing known of those already here. A
    /// video missing from an answer stays: feeds only give the newest.
    fn merge(&mut self, channel: &ChannelId, videos: Vec<Video>, now: i64) {
        for mut video in videos {
            // Whatever a row says, it was asked for under this channel.
            video.channel_id = Some(channel.clone());
            match self.videos.get_mut(&video.id) {
                Some(known) => known.video.absorb(video, now),
                None => {
                    if video.live.is_some() {
                        video.live_seen = Some(now);
                    }
                    self.videos.insert(
                        video.id.clone(),
                        FeedVideo {
                            video,
                            first_seen: now,
                        },
                    );
                }
            }
        }
        self.trim(channel);
    }

    fn trim(&mut self, channel: &ChannelId) {
        let mut own: Vec<&FeedVideo> = self
            .videos
            .values()
            .filter(|row| row.video.channel_id.as_ref() == Some(channel))
            .collect();
        if own.len() <= KEEP_PER_CHANNEL {
            return;
        }
        own.sort_by(|a, b| newest_first(a, b));
        let excess: Vec<VideoId> = own[KEEP_PER_CHANNEL..]
            .iter()
            .map(|row| row.video.id.clone())
            .collect();
        for id in excess {
            self.videos.remove(&id);
        }
    }

    /// Forgets channels no longer subscribed to.
    pub fn keep_channels(&mut self, subscribed: &HashSet<ChannelId>) {
        self.channels.retain(|id, _| subscribed.contains(id));
        self.videos.retain(|_, row| {
            row.video
                .channel_id
                .as_ref()
                .is_some_and(|id| subscribed.contains(id))
        });
    }

    /// The feed in order, newest first, with videos of those channels, and
    /// without Shorts unless asked for. Only a video known to be a Short is
    /// hidden: most of what is unknown is not one.
    pub fn sorted(&self, subscribed: &HashSet<ChannelId>, shorts: bool) -> Vec<&FeedVideo> {
        let mut rows: Vec<&FeedVideo> = self
            .videos
            .values()
            .filter(|row| {
                row.video
                    .channel_id
                    .as_ref()
                    .is_some_and(|id| subscribed.contains(id))
            })
            .filter(|row| shorts || row.video.shorts != Some(true))
            .collect();
        rows.sort_by(|a, b| newest_first(a, b));
        rows
    }
}

/// The feed's order, the id settling ties so that it never shifts by itself.
fn newest_first(a: &FeedVideo, b: &FeedVideo) -> std::cmp::Ordering {
    b.time()
        .cmp(&a.time())
        .then(b.first_seen.cmp(&a.first_seen))
        .then_with(|| a.video.id.cmp(&b.video.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn channel(index: usize) -> ChannelId {
        ChannelId::parse(&format!("UC{index:022}")).unwrap()
    }

    fn channels(count: usize) -> Vec<ChannelId> {
        (0..count).map(channel).collect()
    }

    fn video_id(text: &str) -> VideoId {
        VideoId::parse(&format!("{text:0>11}")).unwrap()
    }

    fn video(text: &str) -> Video {
        Video::new(video_id(text))
    }

    fn listing(videos: Vec<Video>) -> Listing {
        Listing {
            channel: None,
            videos,
        }
    }

    /// A source that answers by a function, noting each call.
    struct Scripted<F> {
        batch: usize,
        workers: usize,
        answer: F,
        calls: Mutex<Vec<Vec<ChannelId>>>,
    }

    impl<F> Scripted<F> {
        fn new(batch: usize, workers: usize, answer: F) -> Self {
            Self {
                batch,
                workers,
                answer,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn asked(&self) -> Vec<ChannelId> {
            let mut asked: Vec<ChannelId> = self
                .calls
                .lock()
                .unwrap()
                .iter()
                .flatten()
                .cloned()
                .collect();
            asked.sort();
            asked
        }

        fn calls(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    impl<F> Source for Scripted<F>
    where
        F: Fn(&ChannelId) -> Result<Listing, Error> + Send + Sync,
    {
        fn batch(&self) -> usize {
            self.batch
        }

        fn workers(&self) -> usize {
            self.workers
        }

        fn fetch(&self, channels: &[ChannelId], _: &Cancel) -> Vec<Result<Listing, Error>> {
            self.calls.lock().unwrap().push(channels.to_vec());
            channels.iter().map(&self.answer).collect()
        }
    }

    fn ok(_: &ChannelId) -> Result<Listing, Error> {
        Ok(listing(Vec::new()))
    }

    fn no_gap() -> Limits {
        Limits {
            batch_gap: Duration::ZERO,
            ..Default::default()
        }
    }

    /// The answers `build` reported, by channel.
    fn run(
        wanted: &[ChannelId],
        first: &dyn Source,
        second: &dyn Source,
    ) -> BTreeMap<ChannelId, Result<Listing, Error>> {
        let outcomes = Mutex::new(Vec::new());
        build(
            wanted,
            first,
            second,
            no_gap(),
            &Cancel::default(),
            &|outcome| {
                outcomes.lock().unwrap().push(outcome);
            },
        );
        let outcomes = outcomes.into_inner().unwrap();
        let count = outcomes.len();
        let map: BTreeMap<_, _> = outcomes
            .into_iter()
            .map(|outcome| (outcome.channel, outcome.result))
            .collect();
        assert_eq!(map.len(), count, "a channel was reported twice");
        map
    }

    #[test]
    fn with_the_first_source_working_the_second_is_not_asked() {
        let first = Scripted::new(1, 3, ok);
        let second = Scripted::new(8, 2, ok);
        let wanted = channels(20);
        let results = run(&wanted, &first, &second);
        assert_eq!(results.len(), 20);
        assert!(results.values().all(Result::is_ok));
        assert_eq!(first.asked(), wanted);
        assert_eq!(second.calls(), 0);
    }

    #[test]
    fn the_breaker_stops_the_first_source_and_the_rest_go_to_the_second() {
        let first = Scripted::new(1, 1, |_: &ChannelId| Err(Failure::NotFeed.into()));
        let second = Scripted::new(4, 2, ok);
        let wanted = channels(20);
        let results = run(&wanted, &first, &second);
        assert_eq!(results.len(), 20);
        assert!(results.values().all(Result::is_ok));
        assert_eq!(first.calls(), 3, "it stops after a few");
        assert_eq!(second.asked(), wanted, "those it failed on go too");
        assert_eq!(second.calls(), 5, "in batches");
    }

    #[test]
    fn the_breaker_holds_with_several_workers_at_once() {
        let first = Scripted::new(1, 3, |_: &ChannelId| Err(Failure::NotFeed.into()));
        let second = Scripted::new(8, 2, ok);
        let results = run(&channels(50), &first, &second);
        assert_eq!(results.len(), 50);
        assert!(first.calls() < 3 + 3, "{} calls", first.calls());
    }

    #[test]
    fn a_few_failures_do_not_trip_it_and_are_asked_again_elsewhere() {
        let bad = channel(2);
        let answer = {
            let bad = bad.clone();
            move |id: &ChannelId| {
                if *id == bad {
                    Err(Failure::NotFeed.into())
                } else {
                    Ok(listing(Vec::new()))
                }
            }
        };
        let first = Scripted::new(1, 1, answer);
        let second = Scripted::new(8, 2, ok);
        let results = run(&channels(10), &first, &second);
        assert_eq!(results.len(), 10);
        assert_eq!(first.calls(), 10);
        assert_eq!(second.asked(), [bad]);
    }

    /// An old list holds channels whose feeds always answer 404, side by side
    /// as often as not. They go to yt-dlp, and nothing else does.
    #[test]
    fn dead_channels_leave_a_working_feed_server_in_use() {
        // With several workers the first good answer can come after any
        // others, so their dead channels aren't the very first.
        for (workers, first_dead) in [(1, 2), (3, 20)] {
            let dead = [
                channel(first_dead),
                channel(first_dead + 1),
                channel(first_dead + 2),
            ];
            let answer = {
                let dead = dead.clone();
                move |id: &ChannelId| {
                    if dead.contains(id) {
                        Err(Failure::NotFeed.into())
                    } else {
                        Ok(listing(Vec::new()))
                    }
                }
            };
            let first = Scripted::new(1, workers, answer);
            let second = Scripted::new(8, 2, ok);
            let results = run(&channels(200), &first, &second);
            assert_eq!(results.len(), 200);
            assert_eq!(first.calls(), 200, "{workers} workers");
            assert_eq!(second.asked(), dead);
            assert_eq!(second.calls(), 1);
        }
    }

    /// An outage partway still stops it: only after a longer run than at first.
    #[test]
    fn an_outage_after_good_answers_trips_it_too() {
        let answer = |id: &ChannelId| {
            if *id < channel(5) {
                Ok(listing(Vec::new()))
            } else {
                Err(Failure::NotFeed.into())
            }
        };
        let first = Scripted::new(1, 1, answer);
        let second = Scripted::new(8, 2, ok);
        let results = run(&channels(200), &first, &second);
        assert_eq!(results.len(), 200);
        assert_eq!(first.calls(), 5 + 3 * AFTER_A_GOOD_ANSWER);
        assert_eq!(second.asked().len(), 195);
    }

    /// A worker waiting for its next batch hears of a block from the other
    /// before it starts another process, and those it didn't ask still say so.
    #[test]
    fn a_block_stops_a_worker_waiting_for_its_next_batch() {
        struct Blocks {
            blocked: AtomicBool,
            asked_after: AtomicBool,
        }
        impl Source for Blocks {
            fn batch(&self) -> usize {
                1
            }
            fn workers(&self) -> usize {
                2
            }
            fn fetch(&self, channels: &[ChannelId], _: &Cancel) -> Vec<Result<Listing, Error>> {
                if self.blocked.load(Ordering::SeqCst) {
                    self.asked_after.store(true, Ordering::SeqCst);
                }
                if channels[0] == channel(1) {
                    std::thread::sleep(Duration::from_millis(100));
                    self.blocked.store(true, Ordering::SeqCst);
                    return vec![Err(Failure::Blocked.into())];
                }
                vec![Ok(listing(Vec::new()))]
            }
        }
        let source = Blocks {
            blocked: AtomicBool::new(false),
            asked_after: AtomicBool::new(false),
        };
        let wanted = channels(3);
        let limits = Limits {
            breaker: 3,
            batch_gap: Duration::from_millis(400),
        };
        let reported = Mutex::new(Vec::new());
        ask_second(&wanted, &source, limits, &Cancel::default(), &|outcome| {
            reported.lock().unwrap().push(outcome);
        });
        assert!(
            !source.asked_after.load(Ordering::SeqCst),
            "another process ran after YouTube blocked"
        );
        let reported = reported.into_inner().unwrap();
        assert_eq!(reported.len(), 3, "each channel still has an answer");
        let last = reported
            .iter()
            .find(|outcome| outcome.channel == channel(2));
        assert_eq!(
            last.unwrap().result.as_ref().unwrap_err().kind,
            Failure::Blocked
        );
    }

    #[test]
    fn a_failure_about_the_channel_does_not_count_towards_the_breaker() {
        let first = Scripted::new(1, 1, |_: &ChannelId| Err(Failure::Failed.into()));
        let second = Scripted::new(8, 2, ok);
        run(&channels(10), &first, &second);
        assert_eq!(first.calls(), 10);
    }

    #[test]
    fn one_bad_channel_in_a_batch_stays_its_own() {
        let first = Refused(Failure::NotFeed.into());
        let gone = channel(5);
        let answer = {
            let gone = gone.clone();
            move |id: &ChannelId| {
                if *id == gone {
                    Err(Failure::NoSuchChannel.into())
                } else {
                    Ok(listing(Vec::new()))
                }
            }
        };
        let second = Scripted::new(4, 1, answer);
        let results = run(&channels(12), &first, &second);
        assert_eq!(results.len(), 12);
        for (id, result) in &results {
            assert_eq!(result.is_err(), *id == gone, "{id:?}");
        }
        assert_eq!(second.calls(), 3);
    }

    #[test]
    fn a_block_ends_the_asking_and_is_said_for_everyone_left() {
        let first = Refused(Failure::NotFeed.into());
        let second = Scripted::new(4, 1, |_: &ChannelId| {
            Err(Error::new(Failure::Blocked, "said so"))
        });
        let results = run(&channels(20), &first, &second);
        assert_eq!(second.calls(), 1, "only one batch asked");
        assert_eq!(results.len(), 20);
        for result in results.values() {
            let error = result.as_ref().unwrap_err();
            assert_eq!(error.kind, Failure::Blocked);
            assert_eq!(error.detail, "said so");
        }
    }

    #[test]
    fn a_block_within_a_batch_ends_the_asking_too() {
        let first = Refused(Failure::NotFeed.into());
        let blocked = channel(1);
        let answer = {
            let blocked = blocked.clone();
            move |id: &ChannelId| {
                if *id == blocked {
                    Err(Failure::Blocked.into())
                } else {
                    Ok(listing(Vec::new()))
                }
            }
        };
        let second = Scripted::new(4, 1, answer);
        let results = run(&channels(20), &first, &second);
        assert_eq!(second.calls(), 1);
        assert_eq!(results.len(), 20);
        assert!(results[&channel(0)].is_ok());
        assert!(results[&channel(19)].is_err());
    }

    #[test]
    fn a_whole_batch_without_a_network_ends_the_asking() {
        let first = Refused(Failure::NotFeed.into());
        let second = Scripted::new(4, 1, |_: &ChannelId| Err(Failure::Network.into()));
        let results = run(&channels(20), &first, &second);
        assert_eq!(second.calls(), 1);
        assert_eq!(results.len(), 20);
        assert!(results.values().all(Result::is_err));
    }

    #[test]
    fn one_channels_timeout_in_a_batch_does_not_end_the_asking() {
        let first = Refused(Failure::NotFeed.into());
        let slow = channel(1);
        let answer = {
            let slow = slow.clone();
            move |id: &ChannelId| {
                if *id == slow {
                    Err(Failure::Timeout.into())
                } else {
                    Ok(listing(Vec::new()))
                }
            }
        };
        let second = Scripted::new(4, 1, answer);
        run(&channels(20), &first, &second);
        assert_eq!(second.calls(), 5);
    }

    #[test]
    fn no_yt_dlp_names_itself_for_what_the_first_source_could_not_do() {
        let good = channel(0);
        let answer = {
            let good = good.clone();
            move |id: &ChannelId| {
                if *id == good {
                    Ok(listing(Vec::new()))
                } else {
                    Err(Failure::NotFeed.into())
                }
            }
        };
        let first = Scripted::new(1, 1, answer);
        let second = Refused(Failure::Missing.into());
        let results = run(&channels(10), &first, &second);
        assert_eq!(results.len(), 10);
        assert!(results[&good].is_ok());
        assert_eq!(
            results[&channel(9)].as_ref().unwrap_err().kind,
            Failure::Missing
        );
    }

    #[test]
    fn a_source_giving_too_few_answers_costs_only_those() {
        struct Short;
        impl Source for Short {
            fn batch(&self) -> usize {
                4
            }
            fn workers(&self) -> usize {
                1
            }
            fn fetch(&self, _: &[ChannelId], _: &Cancel) -> Vec<Result<Listing, Error>> {
                vec![Ok(listing(Vec::new()))]
            }
        }
        let first = Refused(Failure::Failed.into());
        let results = run(&channels(4), &first, &Short);
        assert!(results[&channel(0)].is_ok());
        assert_eq!(
            results[&channel(3)].as_ref().unwrap_err().kind,
            Failure::BadOutput
        );
    }

    #[test]
    fn duplicates_are_asked_once() {
        let first = Scripted::new(1, 2, ok);
        let second = Refused(Failure::Missing.into());
        let results = run(&[channel(1), channel(1), channel(2)], &first, &second);
        assert_eq!(results.len(), 2);
        assert_eq!(first.calls(), 2);
    }

    #[test]
    fn nothing_to_ask_is_fine() {
        let first = Scripted::new(1, 3, ok);
        let second = Scripted::new(8, 2, ok);
        assert!(run(&[], &first, &second).is_empty());
        assert_eq!(first.calls() + second.calls(), 0);
    }

    #[test]
    fn a_cancel_stops_everything_and_reports_nothing_more() {
        let cancel = Cancel::default();
        let canceller = cancel.clone();
        let first = Scripted::new(1, 1, move |_: &ChannelId| {
            canceller.cancel();
            Ok(listing(Vec::new()))
        });
        let second = Scripted::new(8, 2, ok);
        let reports = AtomicUsize::new(0);
        build(&channels(20), &first, &second, no_gap(), &cancel, &|_| {
            reports.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(first.calls(), 1);
        assert_eq!(second.calls(), 0);
        assert_eq!(reports.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn concurrency_stays_within_each_sources_limit() {
        struct Counting {
            now: Arc<AtomicUsize>,
            most: Arc<AtomicUsize>,
            workers: usize,
        }
        impl Source for Counting {
            fn workers(&self) -> usize {
                self.workers
            }
            fn fetch(&self, channels: &[ChannelId], _: &Cancel) -> Vec<Result<Listing, Error>> {
                let now = self.now.fetch_add(1, Ordering::SeqCst) + 1;
                self.most.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(5));
                self.now.fetch_sub(1, Ordering::SeqCst);
                channels.iter().map(ok).collect()
            }
        }
        let most = Arc::new(AtomicUsize::new(0));
        let first = Counting {
            now: Arc::default(),
            most: most.clone(),
            workers: 3,
        };
        let second = Refused(Failure::Missing.into());
        run(&channels(30), &first, &second);
        assert_eq!(most.load(Ordering::SeqCst), 3);
    }

    fn published(at: i64, exact: bool) -> Option<Published> {
        Some(Published { at, exact })
    }

    fn all(count: usize) -> HashSet<ChannelId> {
        channels(count).into_iter().collect()
    }

    fn answer(feed: &mut SavedFeed, index: usize, videos: Vec<Video>, now: i64) {
        feed.apply(
            Outcome {
                channel: channel(index),
                result: Ok(listing(videos)),
            },
            now,
        );
    }

    fn fail(feed: &mut SavedFeed, index: usize, kind: Failure, now: i64) {
        feed.apply(
            Outcome {
                channel: channel(index),
                result: Err(Error::new(kind, "details")),
            },
            now,
        );
    }

    fn order(feed: &SavedFeed, shorts: bool) -> Vec<String> {
        feed.sorted(&all(9), shorts)
            .into_iter()
            .map(|row| row.video.id.as_str().trim_start_matches('0').to_owned())
            .collect()
    }

    #[test]
    fn newest_comes_first_and_the_id_settles_ties() {
        let mut feed = SavedFeed::default();
        let dated = |id, at, exact| Video {
            published: published(at, exact),
            ..video(id)
        };
        answer(
            &mut feed,
            0,
            vec![
                dated("a", 100, true),
                dated("b", 300, false),
                dated("c", 200, true),
            ],
            1000,
        );
        answer(
            &mut feed,
            1,
            vec![dated("e", 200, true), dated("d", 200, true)],
            1000,
        );
        assert_eq!(order(&feed, false), ["b", "c", "d", "e", "a"]);
        // A video with no date goes by when it was first seen.
        answer(&mut feed, 1, vec![video("f")], 150);
        assert_eq!(order(&feed, false), ["b", "c", "d", "e", "f", "a"]);
    }

    #[test]
    fn a_later_answer_merges_without_losing_anything() {
        let mut feed = SavedFeed::default();
        // First from yt-dlp, with a length and a rough date.
        answer(
            &mut feed,
            0,
            vec![Video {
                title: "Title".into(),
                duration: Some(60),
                published: published(1000, false),
                ..video("a")
            }],
            5000,
        );
        // Then from the feed, with an exact date and no length.
        answer(
            &mut feed,
            0,
            vec![Video {
                title: "New title".into(),
                published: published(900, true),
                shorts: Some(false),
                ..video("a")
            }],
            6000,
        );
        let row = &feed.sorted(&all(9), false)[0];
        assert_eq!(row.video.title, "New title");
        assert_eq!(row.video.duration, Some(60));
        assert_eq!(row.video.published, published(900, true));
        assert_eq!(row.first_seen, 5000);
        // yt-dlp again: the exact date stays.
        answer(
            &mut feed,
            0,
            vec![Video {
                published: published(2000, false),
                ..video("a")
            }],
            7000,
        );
        let row = &feed.sorted(&all(9), false)[0];
        assert_eq!(row.video.published, published(900, true));
        assert_eq!(row.video.title, "New title");
    }

    #[test]
    fn a_video_missing_from_an_answer_stays() {
        let mut feed = SavedFeed::default();
        answer(&mut feed, 0, vec![video("a"), video("b")], 1);
        answer(&mut feed, 0, vec![video("c")], 2);
        assert_eq!(feed.sorted(&all(9), false).len(), 3);
    }

    #[test]
    fn duplicates_in_and_across_answers_are_one_video() {
        let mut feed = SavedFeed::default();
        answer(&mut feed, 0, vec![video("a"), video("a")], 1);
        answer(&mut feed, 1, vec![video("a")], 2);
        assert_eq!(feed.sorted(&all(9), false).len(), 1);
    }

    #[test]
    fn shorts_are_hidden_unless_asked_for() {
        let mut feed = SavedFeed::default();
        answer(
            &mut feed,
            0,
            vec![
                Video {
                    shorts: Some(true),
                    ..video("a")
                },
                Video {
                    shorts: Some(false),
                    ..video("b")
                },
                video("c"),
            ],
            1,
        );
        assert_eq!(order(&feed, false), ["b", "c"]);
        assert_eq!(order(&feed, true), ["a", "b", "c"]);
    }

    #[test]
    fn a_row_is_filed_under_the_channel_asked_about() {
        let mut feed = SavedFeed::default();
        answer(
            &mut feed,
            0,
            vec![Video {
                channel_id: Some(channel(7)),
                ..video("a")
            }],
            1,
        );
        let row = &feed.sorted(&all(9), false)[0];
        assert_eq!(row.video.channel_id, Some(channel(0)));
    }

    #[test]
    fn a_404_never_touches_a_channel_or_its_videos() {
        let mut feed = SavedFeed::default();
        answer(&mut feed, 0, vec![video("a")], 10);
        for kind in [
            Failure::NotFeed,
            Failure::Network,
            Failure::Failed,
            Failure::Blocked,
        ] {
            fail(&mut feed, 0, kind, 20);
            let state = feed.state(&channel(0)).unwrap();
            assert_eq!(state.failure, Some(kind));
            assert_eq!(state.detail, "details");
            assert_eq!(state.last_ok, Some(10));
            assert_eq!(state.last_try, Some(20));
            assert!(!state.gone, "{kind:?}");
            assert_eq!(feed.sorted(&all(9), false).len(), 1);
        }
    }

    #[test]
    fn only_does_not_exist_marks_a_channel_and_only_an_answer_clears_it() {
        let mut feed = SavedFeed::default();
        answer(&mut feed, 0, vec![video("a")], 10);
        fail(&mut feed, 0, Failure::NoSuchChannel, 20);
        let state = feed.state(&channel(0)).unwrap();
        assert!(state.gone);
        assert_eq!(state.failure, Some(Failure::NoSuchChannel));
        // It is only a mark: the videos stay.
        assert_eq!(feed.sorted(&all(9), false).len(), 1);
        answer(&mut feed, 0, vec![], 30);
        let state = feed.state(&channel(0)).unwrap();
        assert!(!state.gone);
        assert_eq!(state.failure, None);
        assert_eq!(state.detail, "");
        assert_eq!(state.last_ok, Some(30));
    }

    #[test]
    fn what_an_answer_says_of_its_channel_comes_back() {
        let mut feed = SavedFeed::default();
        let info = ChannelInfo {
            title: "Name".into(),
            ..ChannelInfo::new(channel(0))
        };
        let said = feed.apply(
            Outcome {
                channel: channel(0),
                result: Ok(Listing {
                    channel: Some(info.clone()),
                    videos: Vec::new(),
                }),
            },
            1,
        );
        assert_eq!(said, Some(info));
    }

    #[test]
    fn refreshes_are_due_after_half_an_hour() {
        let mut feed = SavedFeed::default();
        assert!(feed.due(100_000));
        // One stopped part of the way doesn't count, and leaves it due.
        feed.started();
        assert!(feed.due(100_000));
        feed.finished(100_000);
        assert!(!feed.due(100_000));
        assert!(!feed.due(100_000 + REFRESH_AFTER_SECS));
        assert!(feed.due(100_000 + REFRESH_AFTER_SECS + 1));
        // A clock set back.
        assert!(feed.due(99_999));
    }

    /// An update stopped part of the way carries on from there, however it
    /// was stopped, and channels never asked are asked though none is due.
    #[test]
    fn opening_asks_only_what_an_update_has_not_reached() {
        let now = 100_000;
        let old = now - REFRESH_AFTER_SECS - 1;
        let mut feed = SavedFeed::default();
        let wanted = channels(4);
        assert_eq!(feed.to_ask(&wanted, now), (wanted.clone(), true));
        answer(&mut feed, 0, Vec::new(), now - 5);
        answer(&mut feed, 1, Vec::new(), old);
        fail(&mut feed, 2, Failure::Network, now - 5);
        let (asked, due) = feed.to_ask(&wanted, now);
        assert!(due);
        assert_eq!(asked, [channel(1), channel(2), channel(3)]);
        // Checked lately: only the one nothing has been heard of.
        feed.finished(now - 5);
        let (asked, due) = feed.to_ask(&wanted, now);
        assert!(!due);
        assert_eq!(asked, [channel(3)]);
        // A refusal holds that back as well.
        fail(&mut feed, 0, Failure::Blocked, now);
        assert_eq!(feed.to_ask(&wanted, now), (Vec::new(), false));
    }

    #[test]
    fn a_block_holds_off_the_automatic_refresh_for_a_while() {
        let mut feed = SavedFeed::default();
        feed.finished(100_000);
        fail(&mut feed, 0, Failure::Blocked, 100_000);
        assert!(!feed.due(100_000 + REFRESH_AFTER_SECS + 1));
        assert!(!feed.due(100_000 + BLOCKED_PAUSE_SECS - 1));
        assert!(feed.due(100_000 + BLOCKED_PAUSE_SECS));
        // A refresh by hand clears it.
        fail(&mut feed, 0, Failure::Blocked, 200_000);
        feed.started();
        assert!(feed.due(200_000 + REFRESH_AFTER_SECS + 1));
    }

    #[test]
    fn channels_unsubscribed_are_hidden_then_forgotten() {
        let mut feed = SavedFeed::default();
        answer(&mut feed, 0, vec![video("a")], 1);
        answer(&mut feed, 1, vec![video("b")], 1);
        let kept: HashSet<ChannelId> = [channel(1)].into();
        assert_eq!(feed.sorted(&kept, false).len(), 1);
        assert!(feed.state(&channel(0)).is_some());
        feed.keep_channels(&kept);
        assert!(feed.state(&channel(0)).is_none());
        assert_eq!(feed.sorted(&all(9), false).len(), 1);
    }

    #[test]
    fn a_channel_keeps_only_its_newest() {
        let mut feed = SavedFeed::default();
        let rows: Vec<Video> = (0..KEEP_PER_CHANNEL + 5)
            .map(|index| Video {
                published: published(index as i64 + 1000, true),
                ..video(&format!("{index}"))
            })
            .collect();
        answer(&mut feed, 0, rows, 1);
        let kept = feed.sorted(&all(9), false);
        assert_eq!(kept.len(), KEEP_PER_CHANNEL);
        assert_eq!(
            kept[0].video.id,
            video_id(&format!("{}", KEEP_PER_CHANNEL + 4))
        );
        assert_eq!(kept[KEEP_PER_CHANNEL - 1].video.id, video_id("5"));
    }

    #[test]
    fn the_saved_feed_comes_back_whole() {
        let folder = std::env::temp_dir().join(format!("patina-feed-{}", std::process::id()));
        let path = folder.join("feed.json");
        let mut feed = SavedFeed::default();
        feed.finished(50);
        answer(
            &mut feed,
            0,
            vec![Video {
                title: "Title".into(),
                published: published(5, true),
                shorts: Some(true),
                ..video("-a")
            }],
            7,
        );
        fail(&mut feed, 1, Failure::Blocked, 8);
        SavedFeed::write(&path, &feed.to_bytes());
        assert_eq!(SavedFeed::load(&path), feed);
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn a_bad_feed_file_is_only_a_lost_cache() {
        for bad in [
            &b""[..],
            b"not json",
            b"[]",
            b"{}",
            b"{\"schema\": 99, \"videos\": [], \"channels\": {}}",
            b"{\"schema\": 1, \"videos\": 3, \"channels\": {}}",
        ] {
            assert_eq!(
                SavedFeed::read(bad),
                None,
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
        assert_eq!(
            SavedFeed::load(Path::new("/nonexistent/feed.json")),
            SavedFeed::default()
        );
    }

    #[test]
    fn a_bad_row_in_the_feed_file_costs_only_itself() {
        let text = br#"{"schema": 1,
            "videos": [{"id": "abcdefghijk", "first_seen": 3}, {"id": "bad"}, 7],
            "channels": {"UCaaaaaaaaaaaaaaaaaaaaaa": {"last_ok": 1}, "bad": {}, "UCbbbbbbbbbbbbbbbbbbbbbb": 3}
        }"#;
        let feed = SavedFeed::read(text).unwrap();
        assert_eq!(feed.videos.len(), 1);
        assert_eq!(feed.channels.len(), 1);
    }
}
