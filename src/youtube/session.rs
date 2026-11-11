//! The app-wide YouTube state, owned by the app on the interface thread: the
//! lists (`youtube.json`), the saved feed, and the work that fills them. Each
//! window follows it through `Listener`, so that saving a video in one window
//! updates another.
//!
//! Making this does nothing: no thread, no file, no request. Files are read the
//! first time something asks, and nothing goes to the network until the section
//! is opened or the user asks. Nothing blocks the interface thread except
//! writing the small list file and reading the saved feed, which is local and
//! takes a moment at its largest, and making that feed's bytes for a worker.

use super::client::Client;
use super::feed::{self, ChannelState, Limits, Outcome, Refused, SavedFeed, Source};
use super::newpipe;
use super::playing::{self, Seen};
use super::rss::RssSource;
use super::store::{Added, Channel, Mark, SavedVideo, Store};
use super::takeout::{self, ImportError, Rows};
use super::ytdlp::{SearchResults, Version, Ytdlp};
use super::{
    Cancel, ChannelId, ChannelInfo, ChannelInput, Error, Failure, Listing, Video, VideoId,
};
use crate::jellyfin::images::{self, Fetch, Images};
use crate::settings::{HistoryEntry, Paths, Preferences};
use std::cell::{Cell, OnceCell, RefCell, RefMut};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How often a saved video's details are asked for before leaving it be. It
/// still shows, untitled, and can be played. Importing it again gives it these
/// tries anew.
const DETAIL_TRIES: u8 = 3;
/// Time limits in a row before saved videos' details stop for now: one can be
/// that video, and more are the way to YouTube.
const DETAIL_TIMEOUTS: usize = 2;
/// The pause between saved videos' details. A guest is allowed about 300
/// videos an hour, which playback shares, so a long import fills in slowly and
/// the list is usable meanwhile.
const DETAIL_GAP: Duration = Duration::from_secs(15);
/// The most the pictures' cache keeps on disk. It is only a cache, and a
/// thumbnail is about 20 KB.
const IMAGE_LIMIT: u64 = 128 << 20;
/// How often a running update's answers are saved. A slow update of a long list
/// takes minutes, and quitting or a crash shouldn't cost all of it.
const SAVE_EVERY: Duration = Duration::from_secs(30);

/// What changed, for the windows.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// The channels subscribed to.
    Channels,
    /// Watch Later, or what is known of something on it.
    Saved,
    /// Watch marks and progress.
    Marks,
    /// Which videos have been downloaded.
    Downloads,
    /// The feed is being fetched.
    RefreshStarted,
    /// A channel has answered: `done` of `total`.
    Refreshing {
        done: usize,
        total: usize,
    },
    RefreshFinished,
    /// The Shorts setting.
    ShowShorts,
    /// There is something new to tell the user (`Session::take_notices`).
    Notice,
    /// The player's history, whose YouTube videos the History page shows.
    History,
}

pub trait Listener {
    fn youtube_changed(&self, change: &Change);
}

/// Which list a file holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKind {
    Subscriptions,
    WatchLater,
}

/// A file a list can be written as. Watch Later has no NewPipe form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFile {
    /// `subscriptions.csv`, as Google's export has it.
    Subscriptions,
    /// The channels as the file NewPipe exports its subscriptions to.
    NewPipe,
    /// `Watch later-videos.csv`.
    WatchLater,
}

/// How an import went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportReport {
    pub kind: ListKind,
    pub added: usize,
    pub present: usize,
    pub skipped: usize,
}

/// A YouTube video in the player's history, as the History page shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Played {
    pub id: VideoId,
    /// What the history calls it, cleaned as others' text is; empty when that
    /// is only its address or its file's name.
    pub title: String,
    /// Seconds since the Unix epoch.
    pub played_at: i64,
    /// Its length in seconds, 0 when the player never knew.
    pub duration: f64,
}

/// yt-dlp as the section sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolState {
    Ready(Version),
    /// `Failure::Missing`, `Disabled`, `Outdated` and so on.
    Unusable(Error),
}

/// The sources to build the feed from, made afresh for each refresh, so that a
/// changed preference applies.
type Sources = Box<dyn Fn(&Preferences, &Path) -> (Box<dyn Source>, Box<dyn Source>)>;

/// Moves files to the trash, and says which it couldn't and why, on any thread.
type Trash = Box<dyn Fn(Vec<String>, Box<dyn FnOnce(Vec<(String, String)>) + Send>)>;

/// How pictures are fetched for a proxy preference. One that can't be used fails
/// each fetch rather than going direct.
type PictureSource = Box<dyn Fn(Option<&str>) -> Fetch>;

fn real_pictures(proxy: Option<&str>) -> Fetch {
    match Client::new(proxy) {
        Ok(client) => client.image_fetch(),
        Err(error) => {
            let said = error.to_string();
            std::sync::Arc::new(move |_: &str| Err(said.clone()))
        }
    }
}

fn real_sources(prefs: &Preferences, mpv_config: &Path) -> (Box<dyn Source>, Box<dyn Source>) {
    let first = Box::new(RssSource::new(prefs.proxy()));
    let second: Box<dyn Source> = match Ytdlp::from_prefs(prefs, mpv_config) {
        Ok(tool) => Box::new(tool),
        Err(error) => Box::new(Refused(error)),
    };
    (first, second)
}

pub struct Session {
    store_path: PathBuf,
    feed_path: PathBuf,
    mpv_config: PathBuf,
    store: OnceCell<RefCell<Store>>,
    feed: OnceCell<RefCell<SavedFeed>>,
    notices: RefCell<Vec<String>>,
    listeners: RefCell<Vec<Weak<dyn Listener>>>,
    sources: Sources,
    limits: Cell<Limits>,
    detail_gap: Cell<Duration>,
    /// While the feed is being fetched.
    refresh: RefCell<Option<Cancel>>,
    /// When what is being fetched is the update of every channel, when that
    /// began: it counts as a check only if it reaches its end.
    checking: Cell<Option<i64>>,
    /// When the update of every channel that changed preferences stopped had
    /// begun. It carries on from there with the new ones as it winds down,
    /// due or not, since Update Now may have started it.
    resume: Cell<Option<i64>>,
    /// Channels added while it is, to fetch after it.
    waiting: RefCell<Vec<ChannelId>>,
    /// Channels' descriptions changed since they were last saved.
    channels_changed: Cell<bool>,
    /// The feed has changed since it was last saved.
    feed_changed: Cell<bool>,
    saved_at: Cell<Instant>,
    save_every: Cell<Duration>,
    /// Numbers the feed's saves, and which has been written: they run on
    /// threads of their own, and an older one mustn't land on a newer.
    feed_saves: Cell<u64>,
    feed_written: Arc<Mutex<u64>>,
    progress: Cell<(usize, usize)>,
    /// While saved videos' details are being fetched.
    details: RefCell<Option<Cancel>>,
    /// Asked for them again meanwhile: more rows may be waiting after these.
    details_again: Cell<bool>,
    /// The preferences the newest work began with, or that the app has said
    /// changed since. A pass takes its yt-dlp and proxy as it begins, and
    /// what starts again as one winds down goes by these rather than by
    /// those, which may have been changed under it.
    latest_prefs: RefCell<Option<Preferences>>,
    /// The app is quitting, and nothing new starts.
    closing: Cell<bool>,
    /// The windows showing the section. When the last has left it, what is on
    /// its way stops.
    viewers: Cell<usize>,
    images_dir: PathBuf,
    pictures: RefCell<PictureSource>,
    /// The pictures' cache and the proxy preference it was made for.
    images: RefCell<Option<(Option<String>, Images)>>,
    /// What videos sent to a player are called, by the address or file they
    /// opened as: the player has only that until yt-dlp has answered, and a
    /// video in a list not until its turn.
    titles: RefCell<HashMap<String, String>>,
    trash: RefCell<Trash>,
    /// The player's history, handed over by the app whenever it changes. Which
    /// of its entries are videos is worked out only when the History page asks,
    /// as a downloaded file's takes the lists' file, which nothing reads until
    /// the section needs it.
    history: RefCell<Vec<HistoryEntry>>,
    /// Takes the YouTube videos out of the player's history, which is the app's.
    forget_history: RefCell<Box<dyn Fn()>>,
}

/// Seconds since the Unix epoch, as the store keeps time.
fn now() -> i64 {
    crate::settings::now() as i64
}

/// Whether the next saved video's details would meet this as well. A time limit
/// can be that video alone, so it takes several in a row (`timeouts` counts).
fn stops_details(answer: &Result<Video, Error>, timeouts: &mut usize) -> bool {
    let kind = answer.as_ref().err().map(|error| error.kind);
    *timeouts = if kind == Some(Failure::Timeout) {
        *timeouts + 1
    } else {
        0
    };
    *timeouts >= DETAIL_TIMEOUTS
        || matches!(
            kind,
            Some(
                Failure::Blocked
                    | Failure::Network
                    | Failure::Proxy
                    | Failure::Missing
                    | Failure::Disabled
                    | Failure::Outdated
                    | Failure::Cancelled
            )
        )
}

/// Writes one of the feed's saves, unless a later one was written first.
fn write_in_turn(written: &Mutex<u64>, turn: u64, path: &Path, bytes: &[u8]) {
    let mut written = written.lock().unwrap_or_else(|error| error.into_inner());
    if *written < turn {
        SavedFeed::write(path, bytes);
        *written = turn;
    }
}

/// Runs `work` on a thread of its own, and then `done` with what it gave, on
/// the interface thread. Tests have no event loop, and run it at once.
fn on_worker<T: Send + 'static>(
    name: &str,
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) + 'static,
) {
    if cfg!(test) {
        done(work());
    } else {
        crate::jellyfin::session::background(name, work, done);
    }
}

/// Runs `work` on a thread of its own and hands what it sends to `each`, in
/// order, on the interface thread, then calls `done`.
fn streaming<T: Send + 'static>(
    name: &str,
    work: impl FnOnce(&(dyn Fn(T) + Sync)) + Send + 'static,
    mut each: impl FnMut(T) + 'static,
    done: impl FnOnce() + 'static,
) {
    if cfg!(test) {
        let sent = Mutex::new(Vec::new());
        work(&|item| sent.lock().unwrap_or_else(|e| e.into_inner()).push(item));
        for item in sent.into_inner().unwrap_or_else(|e| e.into_inner()) {
            each(item);
        }
        done();
        return;
    }
    let (sender, receiver) = async_channel::unbounded();
    let spawned = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            work(&|item| {
                let _ = sender.send_blocking(item);
            });
        });
    if let Err(error) = spawned {
        log::warn!("could not start {name}: {error}");
        done();
        return;
    }
    let spawned = slint::spawn_local(async move {
        while let Ok(item) = receiver.recv().await {
            each(item);
        }
        done();
    });
    if let Err(error) = spawned {
        log::warn!("could not wait for {name}: {error}");
    }
}

impl Session {
    pub fn new(paths: &Paths) -> Rc<Self> {
        Self::with_sources(paths, Box::new(real_sources))
    }

    pub(super) fn with_sources(paths: &Paths, sources: Sources) -> Rc<Self> {
        Rc::new(Self {
            store_path: paths.youtube_file(),
            feed_path: paths.youtube_feed_file(),
            mpv_config: paths.mpv_config_dir(),
            store: OnceCell::new(),
            feed: OnceCell::new(),
            notices: RefCell::new(Vec::new()),
            listeners: RefCell::new(Vec::new()),
            sources,
            limits: Cell::new(Limits::default()),
            detail_gap: Cell::new(DETAIL_GAP),
            refresh: RefCell::new(None),
            checking: Cell::new(None),
            resume: Cell::new(None),
            waiting: RefCell::new(Vec::new()),
            channels_changed: Cell::new(false),
            feed_changed: Cell::new(false),
            saved_at: Cell::new(Instant::now()),
            save_every: Cell::new(SAVE_EVERY),
            feed_saves: Cell::new(0),
            feed_written: Arc::default(),
            progress: Cell::new((0, 0)),
            details: RefCell::new(None),
            details_again: Cell::new(false),
            latest_prefs: RefCell::new(None),
            closing: Cell::new(false),
            viewers: Cell::new(0),
            images_dir: paths.youtube_images_dir(),
            pictures: RefCell::new(Box::new(real_pictures)),
            images: RefCell::new(None),
            titles: RefCell::new(HashMap::new()),
            trash: RefCell::new(Box::new(|paths, done| {
                crate::platform::move_to_trash(paths, done);
            })),
            history: RefCell::new(Vec::new()),
            forget_history: RefCell::new(Box::new(|| {})),
        })
    }

    /// Fetches pictures some other way: tests never reach the network.
    #[cfg(test)]
    pub(super) fn set_pictures(&self, pictures: PictureSource) {
        *self.pictures.borrow_mut() = pictures;
    }

    /// A pass over the saved videos' details, as far as a row can tell.
    #[cfg(test)]
    pub(super) fn begin_details_for_a_test(&self) {
        *self.details.borrow_mut() = Some(Cancel::default());
        self.notify(Change::Saved);
    }

    #[cfg(test)]
    pub(super) fn use_up_tries_for_a_test(&self, id: &VideoId) {
        for _ in 0..DETAIL_TRIES {
            self.store().saved_tried(id, false);
        }
        self.notify(Change::Saved);
    }

    #[cfg(test)]
    pub(super) fn store_channel_for_a_test(&self, id: &ChannelId) {
        self.store().add_channels(vec![Channel::new(id.clone())]);
    }

    #[cfg(test)]
    pub(super) fn feed_video_for_a_test(&self, id: &ChannelId, mut video: Video) {
        video.channel_id = Some(id.clone());
        self.feed().apply(
            Outcome {
                channel: id.clone(),
                result: Ok(Listing {
                    channel: None,
                    videos: vec![video],
                }),
            },
            now(),
        );
    }

    #[cfg(test)]
    pub(super) fn channel_failed_for_a_test(&self, id: &ChannelId, error: Error) {
        self.feed().apply(
            Outcome {
                channel: id.clone(),
                result: Err(error),
            },
            now(),
        );
    }

    /// Moves files some other way: tests never touch the real trash.
    #[cfg(test)]
    pub(super) fn set_trash(&self, trash: Trash) {
        *self.trash.borrow_mut() = trash;
    }

    /// The cache for thumbnails and channel pictures, started the first time it
    /// is asked for. A changed proxy preference starts it anew, keeping what is
    /// on disk, so that no picture goes by the old way.
    pub fn images(&self, prefs: &Preferences) -> Images {
        let proxy = prefs.proxy().map(str::to_owned);
        let mut cache = self.images.borrow_mut();
        match &*cache {
            Some((made_for, images)) if *made_for == proxy => images.clone(),
            _ => {
                let fetch = (self.pictures.borrow())(proxy.as_deref());
                let images = Images::new(
                    images::Options {
                        dir: Some(self.images_dir.clone()),
                        disk_limit: IMAGE_LIMIT,
                        ..Default::default()
                    },
                    fetch,
                );
                *cache = Some((proxy, images.clone()));
                images
            }
        }
    }

    pub fn add_listener(&self, listener: Weak<dyn Listener>) {
        self.listeners.borrow_mut().push(listener);
    }

    fn notify(&self, change: Change) {
        let listeners: Vec<Rc<dyn Listener>> = {
            let mut listeners = self.listeners.borrow_mut();
            listeners.retain(|listener| listener.strong_count() > 0);
            listeners.iter().filter_map(Weak::upgrade).collect()
        };
        for listener in listeners {
            listener.youtube_changed(&change);
        }
    }

    fn notice(&self, text: String) {
        {
            let mut notices = self.notices.borrow_mut();
            if notices.contains(&text) {
                return;
            }
            notices.push(text);
        }
        self.notify(Change::Notice);
    }

    /// What the user should be told, as the section shows it. They stay until
    /// taken.
    pub fn take_notices(&self) -> Vec<String> {
        // Reading the store is what can bring one.
        self.store();
        std::mem::take(&mut *self.notices.borrow_mut())
    }

    /// The lists, read the first time they are wanted.
    fn store(&self) -> RefMut<'_, Store> {
        self.store
            .get_or_init(|| {
                let store = Store::load(self.store_path.clone());
                if let Some(problem) = store.problem() {
                    self.notices.borrow_mut().push(problem.to_string());
                }
                RefCell::new(store)
            })
            .borrow_mut()
    }

    fn feed(&self) -> RefMut<'_, SavedFeed> {
        self.feed
            .get_or_init(|| RefCell::new(SavedFeed::load(&self.feed_path)))
            .borrow_mut()
    }

    /// Writes the lists, and says so if that failed: the change stays in memory.
    fn save(&self) {
        let saved = self.store().save();
        if let Err(error) = saved {
            log::warn!("could not save {}: {error}", self.store_path.display());
            self.notice(format!("Your YouTube lists could not be saved: {error}"));
        }
    }

    pub fn channels(&self) -> Vec<Channel> {
        self.store().channels().to_vec()
    }

    pub fn channel_count(&self) -> usize {
        self.store().channels().len()
    }

    pub fn is_subscribed(&self, id: &ChannelId) -> bool {
        self.store().is_subscribed(id)
    }

    fn subscribed(&self) -> HashSet<ChannelId> {
        self.store()
            .channels()
            .iter()
            .map(|channel| channel.id.clone())
            .collect()
    }

    /// Adds a channel, and fetches its videos.
    pub fn subscribe(self: &Rc<Self>, prefs: &Preferences, info: ChannelInfo) {
        let id = info.id.clone();
        self.store().add_channels(vec![Channel {
            id: info.id,
            title: info.title,
            handle: info.handle,
            avatar: info.avatar,
            followers: info.followers,
            added_at: now(),
        }]);
        self.save();
        self.notify(Change::Channels);
        self.fetch_channels(prefs, vec![id], None);
    }

    /// Stops following a channel. Its videos go from the feed at once, and from
    /// the saved feed at the next refresh.
    pub fn unsubscribe(&self, id: &ChannelId) {
        if self.store().remove_channel(id) {
            self.save();
            self.notify(Change::Channels);
        }
    }

    /// What a channel's page heard of a channel, kept if it is followed. Its
    /// picture is the point: the channel feeds say nothing of one, so a channel
    /// imported from a file whose feed answers has none until yt-dlp has been
    /// asked about it, and the page asks.
    pub fn channel_described(&self, info: &ChannelInfo) {
        if self.store().update_channel(info) {
            self.save();
            self.notify(Change::Channels);
        }
    }

    /// The feed, newest first: at once, from what was saved.
    pub fn feed_videos(&self) -> Vec<Video> {
        let shorts = self.store().show_shorts();
        let subscribed = self.subscribed();
        self.feed()
            .sorted(&subscribed, shorts)
            .into_iter()
            .map(|row| row.video.clone())
            .collect()
    }

    pub fn channel_state(&self, id: &ChannelId) -> Option<ChannelState> {
        self.feed().state(id).cloned()
    }

    pub fn last_refresh(&self) -> Option<i64> {
        self.feed().last_refresh()
    }

    pub fn is_refreshing(&self) -> bool {
        self.refresh.borrow().is_some()
    }

    /// How many channels have answered of how many being asked.
    pub fn progress(&self) -> (usize, usize) {
        self.progress.get()
    }

    pub fn show_shorts(&self) -> bool {
        self.store().show_shorts()
    }

    pub fn set_show_shorts(&self, show: bool) {
        self.store().set_show_shorts(show);
        self.save();
        self.notify(Change::ShowShorts);
    }

    /// A window has come to the section.
    pub fn entered(&self) {
        self.viewers.set(self.viewers.get() + 1);
    }

    /// A window has left the section. Nothing runs behind a section no window
    /// shows.
    pub fn left(&self) {
        self.viewers.set(self.viewers.get().saturating_sub(1));
        if self.viewers.get() == 0 {
            self.cancel();
        }
    }

    /// The section has been opened: the saved lists are shown, and the feed
    /// and any missing details are fetched behind them if due.
    pub fn section_opened(self: &Rc<Self>, prefs: &Preferences) {
        self.ask_what_is_due(prefs);
        self.fetch_details(prefs);
    }

    /// The preferences the section's work goes by have changed: yt-dlp turned
    /// on or off, another copy of it, or another proxy. An update or a pass
    /// over saved videos took yt-dlp and the proxy as it began, and would go
    /// on with them, the titles of a long import for an hour or more; so what
    /// runs stops, and what is due starts again with these while a window
    /// shows the section. As when a window comes back, what is still winding
    /// down is taken up as it ends.
    pub fn config_changed(self: &Rc<Self>, prefs: &Preferences) {
        *self.latest_prefs.borrow_mut() = Some(prefs.clone());
        if self.is_refreshing() && self.checking.get().is_some() {
            self.resume.set(self.checking.get());
        }
        self.cancel();
        if self.viewers.get() > 0 && !self.closing.get() {
            self.section_opened(prefs);
        }
    }

    /// The preferences to start more work with as a pass ends: the newest
    /// known, or else those it began with.
    fn newest_prefs(&self, began_with: &Preferences) -> Preferences {
        self.latest_prefs
            .borrow()
            .clone()
            .unwrap_or_else(|| began_with.clone())
    }

    /// Asks about what opening the section should: the channels an update that
    /// is due hasn't reached, or those never asked. While something still winds
    /// down, this is looked at again as it ends.
    fn ask_what_is_due(self: &Rc<Self>, prefs: &Preferences) {
        if self.is_refreshing() {
            return;
        }
        let channels: Vec<ChannelId> = self
            .store()
            .channels()
            .iter()
            .map(|channel| channel.id.clone())
            .collect();
        let time = now();
        let (wanted, due) = self.feed().to_ask(&channels, time);
        if due {
            self.feed().started();
            if wanted.is_empty() {
                // Each has answered lately: an update stopped at its very end.
                if !channels.is_empty() {
                    self.feed().finished(time);
                    self.feed_changed.set(true);
                    self.notify(Change::RefreshFinished);
                }
                return;
            }
        }
        self.fetch_channels(prefs, wanted, due.then_some(time));
    }

    /// Fetches every channel's newest videos, unless that is happening already.
    pub fn refresh(self: &Rc<Self>, prefs: &Preferences) {
        if self.is_refreshing() {
            return;
        }
        let channels: Vec<ChannelId> = self
            .store()
            .channels()
            .iter()
            .map(|channel| channel.id.clone())
            .collect();
        self.feed().started();
        self.fetch_channels(prefs, channels, Some(now()));
    }

    /// Fetches these channels. `check` says this is the update of them all, and
    /// when it began. During a refresh they wait until it ends.
    fn fetch_channels(
        self: &Rc<Self>,
        prefs: &Preferences,
        channels: Vec<ChannelId>,
        check: Option<i64>,
    ) {
        if channels.is_empty() {
            return;
        }
        if self.is_refreshing() {
            self.waiting.borrow_mut().extend(channels);
            return;
        }
        let (first, second) = (self.sources)(prefs, &self.mpv_config);
        *self.latest_prefs.borrow_mut() = Some(prefs.clone());
        let cancel = Cancel::default();
        *self.refresh.borrow_mut() = Some(cancel.clone());
        self.checking.set(check);
        self.saved_at.set(Instant::now());
        self.progress.set((0, channels.len()));
        self.notify(Change::RefreshStarted);
        let limits = self.limits.get();
        let answered = Rc::downgrade(self);
        let finished = Rc::downgrade(self);
        let prefs = prefs.clone();
        streaming(
            "youtube-feed",
            move |send| {
                feed::build(&channels, &*first, &*second, limits, &cancel, &|outcome| {
                    send(outcome);
                });
            },
            move |outcome: Outcome| {
                if let Some(session) = answered.upgrade() {
                    session.took_outcome(outcome);
                }
            },
            move || {
                if let Some(session) = finished.upgrade() {
                    session.refresh_finished(&prefs);
                }
            },
        );
    }

    fn took_outcome(&self, outcome: Outcome) {
        let said = self.feed().apply(outcome, now());
        self.feed_changed.set(true);
        if let Some(info) = said
            && self.store().update_channel(&info)
        {
            self.channels_changed.set(true);
        }
        if self.saved_at.get().elapsed() >= self.save_every.get() {
            self.save_progress();
        }
        let (done, total) = self.progress.get();
        self.progress.set((done + 1, total));
        self.notify(Change::Refreshing {
            done: done + 1,
            total,
        });
    }

    /// Writes what has changed of channels' descriptions and the feed, the
    /// feed on a worker.
    fn save_progress(&self) {
        self.saved_at.set(Instant::now());
        self.save_channels();
        if let Some((turn, bytes)) = self.feed_to_save() {
            let path = self.feed_path.clone();
            let written = self.feed_written.clone();
            on_worker(
                "youtube-feed-save",
                move || write_in_turn(&written, turn, &path, &bytes),
                |()| {},
            );
        }
    }

    fn save_channels(&self) {
        if self.channels_changed.replace(false) {
            self.save();
            self.notify(Change::Channels);
        }
    }

    /// The feed's contents and their place in line, if it has changed.
    fn feed_to_save(&self) -> Option<(u64, Vec<u8>)> {
        if !self.feed_changed.replace(false) {
            return None;
        }
        let turn = self.feed_saves.get() + 1;
        self.feed_saves.set(turn);
        Some((turn, self.feed().to_bytes()))
    }

    fn refresh_finished(self: &Rc<Self>, prefs: &Preferences) {
        let stopped = self
            .refresh
            .borrow_mut()
            .take()
            .is_some_and(|cancel| cancel.cancelled());
        let check = self.checking.take().filter(|_| !stopped);
        // Taken before the return below, so that a section left meanwhile
        // drops it.
        let resume = self.resume.take().filter(|_| stopped);
        let subscribed = self.subscribed();
        {
            let mut feed = self.feed();
            feed.keep_channels(&subscribed);
            if let Some(at) = check {
                feed.finished(at);
            }
        }
        self.feed_changed.set(true);
        self.save_progress();
        self.notify(Change::RefreshFinished);
        let waiting = std::mem::take(&mut *self.waiting.borrow_mut());
        // Nothing starts behind a section no window shows: what this leaves
        // undone is due the next time it opens.
        if self.viewers.get() == 0 || self.closing.get() {
            return;
        }
        let prefs = self.newest_prefs(prefs);
        if stopped {
            // A window came back while this was winding down, or the
            // preferences changed under it, and what that wanted was refused
            // for as long as this ran.
            match resume {
                // An update of every channel goes on with those that haven't
                // answered since it began. Going by what is due would end
                // one that Update Now started soon after the last check.
                Some(began) => {
                    let rest: Vec<ChannelId> = {
                        let feed = self.feed();
                        subscribed
                            .iter()
                            .filter(|channel| {
                                feed.state(channel)
                                    .and_then(|state| state.last_ok)
                                    .is_none_or(|at| at < began)
                            })
                            .cloned()
                            .collect()
                    };
                    if rest.is_empty() {
                        // Each has answered since: it stopped at its very end.
                        if !subscribed.is_empty() {
                            self.feed().finished(began);
                            self.feed_changed.set(true);
                            self.notify(Change::RefreshFinished);
                        }
                    } else {
                        self.fetch_channels(&prefs, rest, Some(began));
                    }
                }
                None => self.ask_what_is_due(&prefs),
            }
        } else {
            self.fetch_channels(&prefs, waiting, None);
        }
    }

    /// Stops what is being fetched, when the section is left. What has arrived
    /// is kept, and the rest is asked for the next time it opens.
    pub fn cancel(&self) {
        if let Some(cancel) = &*self.refresh.borrow() {
            cancel.cancel();
        }
        self.waiting.borrow_mut().clear();
        self.stop_details();
    }

    /// The app is quitting: what is on its way stops, nothing new starts, and
    /// what has come is written before the process goes.
    pub fn quit(&self) {
        self.closing.set(true);
        self.cancel();
        self.save_channels();
        if let Some((turn, bytes)) = self.feed_to_save() {
            write_in_turn(&self.feed_written, turn, &self.feed_path, &bytes);
        }
    }

    /// Watch Later, most recently added first.
    pub fn saved(&self) -> Vec<SavedVideo> {
        let mut rows = self.store().saved().to_vec();
        rows.sort_by_key(|row| std::cmp::Reverse(row.added_at));
        rows
    }

    pub fn is_saved(&self, id: &VideoId) -> bool {
        self.store().is_saved(id)
    }

    /// Which videos are on Watch Later, to look many up in one go.
    pub fn saved_ids(&self) -> HashSet<VideoId> {
        self.store()
            .saved()
            .iter()
            .map(|saved| saved.video.id.clone())
            .collect()
    }

    pub fn saved_count(&self) -> usize {
        self.store().saved().len()
    }

    pub fn save_video(&self, video: Video) {
        let time = now();
        self.store()
            .add_saved(vec![SavedVideo::new(video, time)], time);
        self.save();
        self.notify(Change::Saved);
    }

    pub fn unsave_video(&self, id: &VideoId) {
        if self.store().remove_saved(id) {
            self.save();
            self.notify(Change::Saved);
        }
    }

    /// Fetches the details of saved videos that lack them, one at a time and
    /// spaced out, the newest added first, and stops at anything that would stop
    /// the next one too. What is not done now is taken up the next time the
    /// section opens. Asked while that runs, it goes on when that ends, for rows
    /// added since.
    pub fn fetch_details(self: &Rc<Self>, prefs: &Preferences) {
        if self.details.borrow().is_some() {
            self.details_again.set(true);
            return;
        }
        self.details_pass(prefs, false);
    }

    /// `pause_first` when a pass has only just asked for the last of its own.
    fn details_pass(self: &Rc<Self>, prefs: &Preferences, pause_first: bool) {
        let ids = self.store().saved_without_details(DETAIL_TRIES);
        if ids.is_empty() {
            return;
        }
        // The section says this by itself, from `tool_state`.
        let Ok(tool) = Ytdlp::from_prefs(prefs, &self.mpv_config) else {
            return;
        };
        *self.latest_prefs.borrow_mut() = Some(prefs.clone());
        let cancel = Cancel::default();
        *self.details.borrow_mut() = Some(cancel.clone());
        // The rows say whether their titles are on their way.
        self.notify(Change::Saved);
        let gap = self.detail_gap.get();
        let answered = Rc::downgrade(self);
        let finished = Rc::downgrade(self);
        let prefs = prefs.clone();
        streaming(
            "youtube-details",
            move |send| {
                let mut timeouts = 0;
                for (index, id) in ids.into_iter().enumerate() {
                    if (index > 0 || pause_first) && !cancel.sleep(gap) {
                        return;
                    }
                    let answer = tool.video(&id, &cancel);
                    let stop = stops_details(&answer, &mut timeouts);
                    send((id, answer, stop));
                    if stop {
                        return;
                    }
                }
            },
            move |(id, answer, stop): (VideoId, Result<Video, Error>, bool)| {
                if let Some(session) = answered.upgrade() {
                    // Stopped by a cancel, the pass says nothing of the next
                    // video: asked for again since, the details go on.
                    let cancelled =
                        matches!(&answer, Err(error) if error.kind == Failure::Cancelled);
                    if stop && !cancelled {
                        session.details_again.set(false);
                    }
                    session.took_details(&id, answer);
                }
            },
            move || {
                if let Some(session) = finished.upgrade() {
                    session.details_finished(&prefs);
                }
            },
        );
    }

    fn details_finished(self: &Rc<Self>, prefs: &Preferences) {
        *self.details.borrow_mut() = None;
        self.notify(Change::Saved);
        if self.details_again.replace(false) && self.viewers.get() > 0 && !self.closing.get() {
            self.details_pass(&self.newest_prefs(prefs), true);
        }
    }

    fn took_details(&self, id: &VideoId, answer: Result<Video, Error>) {
        let time = now();
        {
            let mut store = self.store();
            match answer {
                Ok(video) => store.saved_details(video, time),
                // Gone for good, as far as can be told.
                Err(error)
                    if error.kind.about_the_video() || error.kind == Failure::NoSuchChannel =>
                {
                    store.saved_tried(id, true);
                }
                // Something about this video in particular, perhaps.
                Err(error) if matches!(error.kind, Failure::Failed | Failure::BadOutput) => {
                    store.saved_tried(id, false);
                }
                // Nothing about the video at all: it isn't counted against it.
                Err(_) => return,
            }
        }
        self.save();
        self.notify(Change::Saved);
    }

    /// Whether this row's title is on its way, for the row to say.
    pub fn is_fetching_title(&self, saved: &SavedVideo) -> bool {
        self.details.borrow().is_some() && saved.tries < DETAIL_TRIES
    }

    fn stop_details(&self) {
        self.details_again.set(false);
        if let Some(cancel) = &*self.details.borrow() {
            cancel.cancel();
        }
    }

    pub fn mark(&self, id: &VideoId) -> Option<Mark> {
        self.store().mark(id).copied()
    }

    /// Notes how far a video got and how many seconds of it were really played,
    /// when it stops. Like the history, this keeps nothing if asked not to.
    pub fn note_watch(&self, prefs: &Preferences, seen: &Seen) {
        if !prefs.record_history {
            return;
        }
        self.store()
            .note_watch(&seen.id, seen.position, seen.duration, seen.played, now());
        self.save();
        self.notify(Change::Marks);
    }

    /// Marks a video by hand, either way.
    pub fn set_watched(&self, id: &VideoId, watched: bool) {
        self.store().set_watched(id, watched, now());
        self.save();
        self.notify(Change::Marks);
    }

    /// A video was taken off the history, and its mark goes with it.
    pub fn forget_watch(&self, id: &VideoId) {
        if self.store().mark(id).is_some() {
            self.store().set_watched(id, false, now());
            self.save();
            self.notify(Change::Marks);
        }
    }

    /// Clear Saved Positions forgets where videos were left off, and keeps what
    /// has been seen of them.
    pub fn clear_positions(&self) {
        if self.store().clear_positions() {
            self.save();
            self.notify(Change::Marks);
        }
    }

    /// Clear Recent clears the marks with the rest of what was played.
    pub fn clear_marks(&self) {
        if self.store().clear_marks() {
            self.save();
            self.notify(Change::Marks);
        }
    }

    /// The player's history, as it is now.
    pub fn set_history(&self, entries: &[HistoryEntry]) {
        *self.history.borrow_mut() = entries.to_vec();
        self.notify(Change::History);
    }

    /// What takes the YouTube videos out of the player's history, for Clear
    /// History on the section's page.
    pub fn on_clear_history(&self, forget: Box<dyn Fn()>) {
        *self.forget_history.borrow_mut() = forget;
    }

    /// The YouTube videos in the player's history, newest first, each once
    /// though it was opened both by its address and as a downloaded file.
    /// Where it was played last says when; what an older entry knew and the
    /// newest didn't (a downloaded file's history has only its name, a file
    /// only just loaded no length) is kept. A file the section downloaded
    /// with no title known anywhere is called what its name says, which is
    /// yt-dlp's "<title> [<id>]". Both are someone else's words, from mpv's
    /// media-title or yt-dlp, and are cleaned like any title the section
    /// fetches.
    pub fn history(&self) -> Vec<Played> {
        let entries = self.history.borrow().clone();
        let mut played: Vec<Played> = Vec::new();
        let mut named: Vec<String> = Vec::new();
        let mut at: HashMap<VideoId, usize> = HashMap::new();
        for entry in entries {
            let Some(id) = self.video_at(&entry.path) else {
                continue;
            };
            let file_name = crate::player::state::file_name(&entry.path);
            let title = if entry.title == file_name {
                String::new()
            } else {
                super::clean_text(&entry.title)
            };
            let name = if crate::util::is_url(&entry.path) {
                String::new()
            } else {
                Path::new(&entry.path)
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .and_then(|stem| stem.strip_suffix(&format!(" [{}]", id.as_str())))
                    .map(super::clean_text)
                    .unwrap_or_default()
            };
            match at.get(&id) {
                Some(&index) => {
                    let kept = &mut played[index];
                    if kept.title.is_empty() {
                        kept.title = title;
                    }
                    if kept.duration < 1.0 {
                        kept.duration = entry.duration;
                    }
                    if named[index].is_empty() {
                        named[index] = name;
                    }
                }
                None => {
                    at.insert(id.clone(), played.len());
                    played.push(Played {
                        id,
                        title,
                        played_at: i64::try_from(entry.played_at).unwrap_or(i64::MAX),
                        duration: entry.duration,
                    });
                    named.push(name);
                }
            }
        }
        for (played, name) in played.iter_mut().zip(named) {
            if played.title.is_empty() {
                played.title = name;
            }
        }
        played
    }

    /// Clear History on the section's page. Its videos leave the player's
    /// history, Recent and the History window with them, as the page is only
    /// what those hold; other files stay. Every watched mark and progress goes
    /// too, as they say what was watched as much as the history does.
    pub fn clear_history(&self) {
        let kept: Vec<HistoryEntry> = {
            let entries = self.history.borrow().clone();
            entries
                .into_iter()
                .filter(|entry| self.video_at(&entry.path).is_none())
                .collect()
        };
        *self.history.borrow_mut() = kept;
        (self.forget_history.borrow())();
        self.clear_marks();
        self.notify(Change::History);
    }

    /// Where a video was downloaded to, while that file is there.
    pub fn download(&self, id: &VideoId) -> Option<String> {
        self.store().download(id).map(str::to_owned)
    }

    pub fn set_download(&self, id: &VideoId, path: &Path) {
        self.store().set_download(id, path);
        self.save();
        self.notify(Change::Downloads);
    }

    /// Sends a downloaded video's file to the trash. It stays marked if that
    /// didn't work, and `done` hears why. A file gone already just loses its
    /// mark.
    pub fn trash_download(
        self: &Rc<Self>,
        id: &VideoId,
        done: impl FnOnce(Result<(), String>) + 'static,
    ) {
        let Some(path) = self.download(id) else {
            if self.store().remove_download(id) {
                self.save();
                self.notify(Change::Downloads);
            }
            done(Ok(()));
            return;
        };
        if let Err(why) = crate::platform::can_trash(&path) {
            done(Err(why));
            return;
        }
        let (sender, receiver) = async_channel::bounded(1);
        (self.trash.borrow())(
            vec![path],
            Box::new(move |failed| {
                let _ = sender.send_blocking(failed);
            }),
        );
        let weak = Rc::downgrade(self);
        let id = id.clone();
        let finish = move |failed: Vec<(String, String)>| {
            if let Some((_, why)) = failed.into_iter().next() {
                done(Err(why));
                return;
            }
            if let Some(session) = weak.upgrade() {
                let removed = session.store().remove_download(&id);
                if removed {
                    session.save();
                    session.notify(Change::Downloads);
                }
            }
            done(Ok(()));
        };
        if cfg!(test) {
            finish(receiver.recv_blocking().unwrap_or_default());
            return;
        }
        let spawned = slint::spawn_local(async move {
            finish(receiver.recv().await.unwrap_or_default());
        });
        if let Err(error) = spawned {
            log::warn!("could not wait for the trash: {error}");
        }
    }

    /// What plays a video, in order of what costs the least: its own file if it
    /// was downloaded, otherwise the one address it opens by.
    pub fn source_of(&self, id: &VideoId) -> String {
        self.download(id).unwrap_or_else(|| id.watch_url())
    }

    /// Which video a player's entry is: its address, or a file downloaded here.
    pub fn video_at(&self, source: &str) -> Option<VideoId> {
        VideoId::from_watch_url(source).or_else(|| self.store().video_at(source))
    }

    /// Notes what a video about to go to a player is called.
    pub fn name_source(&self, source: String, title: String) {
        if !title.is_empty() {
            self.titles.borrow_mut().insert(source, title);
        }
    }

    pub fn title_of(&self, source: &str) -> Option<String> {
        self.titles.borrow().get(source).cloned()
    }

    /// Where a video picks up when opened from the section.
    pub fn resume_from(&self, prefs: &Preferences, id: &VideoId) -> Option<f64> {
        if !prefs.resume_playback {
            return None;
        }
        self.mark(id).as_ref().and_then(playing::resume_from)
    }

    /// Whether what plays a video from its address can be run, or why not.
    pub fn can_play(&self, prefs: &Preferences) -> Result<(), Error> {
        Ytdlp::from_prefs(prefs, &self.mpv_config).map(|_| ())
    }

    /// A video has started playing from the section: saved videos' details,
    /// which count against the same limit on a guest, wait until it is over.
    pub fn playing_started(&self) {
        self.stop_details();
    }

    /// Searches, on Enter and never while typing: this is a process and several
    /// requests. The answer comes to `done`, unless `cancel` was used first,
    /// which stops yt-dlp and leaves `done` hearing nothing. The caller makes
    /// `cancel` and keeps it before the search starts, so that it can be used
    /// while the search runs, even one that can't be put off to a thread.
    pub fn search(
        &self,
        prefs: &Preferences,
        query: &str,
        cancel: &Cancel,
        done: impl FnOnce(Result<SearchResults, Error>) + 'static,
    ) {
        let tool = Ytdlp::from_prefs(prefs, &self.mpv_config);
        let query = query.to_owned();
        let working = cancel.clone();
        let reading = cancel.clone();
        on_worker(
            "youtube-search",
            move || tool?.search(&query, &working),
            move |answer| {
                if !reading.cancelled() {
                    done(answer);
                }
            },
        );
    }

    /// Finds the channel a pasted link or `@handle` names.
    pub fn resolve_channel(
        &self,
        prefs: &Preferences,
        text: &str,
        done: impl FnOnce(Result<ChannelInfo, Error>) + 'static,
    ) -> Cancel {
        let cancel = Cancel::default();
        let Some(input) = ChannelInput::parse(text) else {
            done(Err(Error::new(
                Failure::Failed,
                "That isn't a channel link or an @handle.",
            )));
            return cancel;
        };
        let tool = Ytdlp::from_prefs(prefs, &self.mpv_config);
        let working = cancel.clone();
        let reading = cancel.clone();
        on_worker(
            "youtube-resolve",
            move || tool?.resolve(&input, &working),
            move |answer| {
                if !reading.cancelled() {
                    done(answer);
                }
            },
        );
        cancel
    }

    /// A channel's videos, for its page.
    pub fn channel_page(
        &self,
        prefs: &Preferences,
        id: &ChannelId,
        done: impl FnOnce(Result<Listing, Error>) + 'static,
    ) -> Cancel {
        let cancel = Cancel::default();
        let tool = Ytdlp::from_prefs(prefs, &self.mpv_config);
        let id = id.clone();
        let working = cancel.clone();
        let reading = cancel.clone();
        on_worker(
            "youtube-channel",
            move || tool?.listing(&id, &working),
            move |answer| {
                if !reading.cancelled() {
                    done(answer);
                }
            },
        );
        cancel
    }

    /// Whether yt-dlp can be used, and how old it is.
    pub fn tool_state(&self, prefs: &Preferences, done: impl FnOnce(ToolState) + 'static) {
        let tool = Ytdlp::from_prefs(prefs, &self.mpv_config);
        on_worker(
            "youtube-tool",
            move || {
                let today = jiff::Zoned::now().date();
                match tool.and_then(|tool| tool.version(today, &Cancel::default())) {
                    Ok(version) => ToolState::Ready(version),
                    Err(error) => ToolState::Unusable(error),
                }
            },
            done,
        );
    }

    /// Reads a list the user chose, once, off the interface thread, and adds
    /// what can be used. Channels added are fetched, and Watch Later's rows,
    /// which come without titles, have them filled in behind the list.
    pub fn import(
        self: &Rc<Self>,
        prefs: &Preferences,
        path: &Path,
        done: impl FnOnce(Result<ImportReport, ImportError>) + 'static,
    ) {
        let path = path.to_owned();
        let time = now();
        let session = Rc::downgrade(self);
        let prefs = prefs.clone();
        on_worker(
            "youtube-import",
            move || takeout::read_file(&path).and_then(|bytes| takeout::parse(&bytes, time)),
            move |parsed| {
                let Some(session) = session.upgrade() else {
                    return;
                };
                done(parsed.map(|parsed| session.took_import(&prefs, parsed.rows, parsed.skipped)));
            },
        );
    }

    fn took_import(
        self: &Rc<Self>,
        prefs: &Preferences,
        rows: Rows,
        skipped: usize,
    ) -> ImportReport {
        // Read off the interface thread, the list may come back after the
        // preferences it began with have changed.
        let prefs = &self.newest_prefs(prefs);
        let report = |kind, added: Added| ImportReport {
            kind,
            added: added.added,
            present: added.present,
            skipped: skipped + added.skipped,
        };
        match rows {
            Rows::Channels(rows) => {
                let known = self.subscribed();
                let added = self.store().add_channels(rows);
                self.save();
                self.notify(Change::Channels);
                let new: Vec<ChannelId> = self
                    .subscribed()
                    .into_iter()
                    .filter(|id| !known.contains(id))
                    .collect();
                self.fetch_channels(prefs, new, None);
                report(ListKind::Subscriptions, added)
            }
            Rows::WatchLater(rows) => {
                let added = self.store().add_saved(rows, now());
                self.save();
                self.notify(Change::Saved);
                self.fetch_details(prefs);
                report(ListKind::WatchLater, added)
            }
        }
    }

    /// Writes a list to a file the user chose, in the form it can be read back
    /// in. Straight to the file: the sandbox's portal lets Patina at that file
    /// alone, with no room for a temporary one beside it.
    pub fn export(
        &self,
        file: ExportFile,
        path: &Path,
        done: impl FnOnce(Result<(), String>) + 'static,
    ) {
        let bytes = match file {
            ExportFile::Subscriptions => takeout::write_subscriptions(self.store().channels()),
            ExportFile::NewPipe => newpipe::write_subscriptions(self.store().channels()),
            ExportFile::WatchLater => takeout::write_watch_later(self.store().saved()),
        };
        let path = path.to_owned();
        on_worker(
            "youtube-export",
            move || std::fs::write(&path, bytes).map_err(|error| error.to_string()),
            done,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const A: &str = "UCaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "UCbbbbbbbbbbbbbbbbbbbbbb";

    fn channel(text: &str) -> ChannelId {
        ChannelId::parse(text).unwrap()
    }

    fn video_id(text: &str) -> VideoId {
        VideoId::parse(text).unwrap()
    }

    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "patina-youtube-session-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn paths(&self) -> Paths {
            Paths {
                config: self.0.join("config"),
                data: self.0.join("data"),
            }
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Default)]
    struct Heard(RefCell<Vec<Change>>);

    impl Listener for Heard {
        fn youtube_changed(&self, change: &Change) {
            self.0.borrow_mut().push(change.clone());
        }
    }

    fn listen(session: &Session) -> Rc<Heard> {
        let heard = Rc::new(Heard::default());
        let listener: Weak<dyn Listener> = Rc::downgrade(&heard) as Weak<dyn Listener>;
        session.add_listener(listener);
        heard
    }

    /// A source answering every channel with one new video named after the
    /// count, or with a failure.
    #[derive(Clone)]
    struct Stub {
        calls: Arc<AtomicUsize>,
        failure: Option<Failure>,
    }

    impl Stub {
        fn working() -> Self {
            Self {
                calls: Arc::default(),
                failure: None,
            }
        }

        fn failing(failure: Failure) -> Self {
            Self {
                calls: Arc::default(),
                failure: Some(failure),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl Source for Stub {
        fn workers(&self) -> usize {
            1
        }

        fn fetch(&self, channels: &[ChannelId], _: &Cancel) -> Vec<Result<Listing, Error>> {
            channels
                .iter()
                .map(|channel| {
                    let call = self.calls.fetch_add(1, Ordering::SeqCst);
                    match self.failure {
                        Some(failure) => Err(failure.into()),
                        None => Ok(Listing {
                            channel: Some(ChannelInfo {
                                title: "Told".into(),
                                ..ChannelInfo::new(channel.clone())
                            }),
                            videos: vec![Video {
                                channel_id: Some(channel.clone()),
                                ..Video::new(video_id(&format!("{call:011}")))
                            }],
                        }),
                    }
                })
                .collect()
        }
    }

    fn session_with(folder: &Folder, first: Stub, second: Stub) -> Rc<Session> {
        let session = Session::with_sources(
            &folder.paths(),
            Box::new(move |_, _| {
                (
                    Box::new(first.clone()) as Box<dyn Source>,
                    Box::new(second.clone()) as Box<dyn Source>,
                )
            }),
        );
        session.limits.set(Limits {
            batch_gap: Duration::ZERO,
            ..Default::default()
        });
        session.detail_gap.set(Duration::ZERO);
        session
    }

    fn working(folder: &Folder) -> Rc<Session> {
        session_with(folder, Stub::working(), Stub::working())
    }

    fn info(id: &str) -> ChannelInfo {
        ChannelInfo {
            title: id.to_owned(),
            ..ChannelInfo::new(channel(id))
        }
    }

    #[test]
    fn making_a_session_does_nothing() {
        let folder = Folder::new("idle");
        let first = Stub::working();
        let second = Stub::working();
        let session = session_with(&folder, first.clone(), second.clone());
        drop(session);
        assert_eq!(first.calls() + second.calls(), 0);
        assert!(!folder.paths().youtube_file().exists());
        assert!(!folder.paths().data.exists());
    }

    #[test]
    fn subscribing_saves_tells_the_listeners_and_fetches_that_channel() {
        let folder = Folder::new("subscribe");
        let first = Stub::working();
        let session = session_with(&folder, first.clone(), Stub::working());
        let heard = listen(&session);
        session.subscribe(&Preferences::default(), info(A));
        assert!(session.is_subscribed(&channel(A)));
        assert_eq!(first.calls(), 1);
        assert_eq!(session.feed_videos().len(), 1);
        let changes = heard.0.borrow();
        assert!(changes.contains(&Change::RefreshStarted));
        assert!(changes.contains(&Change::Refreshing { done: 1, total: 1 }));
        assert!(changes.contains(&Change::RefreshFinished));
        drop(changes);
        // It is there next time.
        let again = working(&folder);
        assert_eq!(again.channels().len(), 1);
    }

    #[test]
    fn a_listener_that_went_away_is_dropped() {
        let folder = Folder::new("listeners");
        let session = working(&folder);
        let heard = listen(&session);
        drop(heard);
        session.save_video(Video::new(video_id("abcdefghijk")));
        assert!(session.listeners.borrow().is_empty());
    }

    #[test]
    fn the_saved_feed_shows_at_once_with_nothing_asked() {
        let folder = Folder::new("saved-feed");
        {
            let session = working(&folder);
            session.subscribe(&Preferences::default(), info(A));
            assert_eq!(session.feed_videos().len(), 1);
        }
        let first = Stub::working();
        let session = session_with(&folder, first.clone(), Stub::working());
        assert_eq!(session.feed_videos().len(), 1);
        assert_eq!(first.calls(), 0);
    }

    #[test]
    fn opening_refreshes_only_when_due() {
        let folder = Folder::new("due");
        let first = Stub::working();
        let session = session_with(&folder, first.clone(), Stub::working());
        let prefs = Preferences::default();
        session.subscribe(&prefs, info(A));
        session.subscribe(&prefs, info(B));
        let before = first.calls();
        session.section_opened(&prefs);
        assert_eq!(first.calls(), before, "both have only just answered");
        assert!(session.last_refresh().is_some(), "which makes it a check");
        session.section_opened(&prefs);
        assert_eq!(first.calls(), before, "just refreshed");
        session.refresh(&prefs);
        assert_eq!(first.calls(), before + 2, "by hand it always does");
        // Half an hour on, it is every channel's turn.
        let long_ago = now() - 3600;
        for id in [A, B] {
            session.feed().apply(
                Outcome {
                    channel: channel(id),
                    result: Ok(Listing {
                        channel: None,
                        videos: Vec::new(),
                    }),
                },
                long_ago,
            );
        }
        session.feed().finished(long_ago);
        session.section_opened(&prefs);
        assert_eq!(first.calls(), before + 4);
    }

    /// Answers for one channel, as a refresh's worker would send them.
    fn answer_for(id: &str, video: &str) -> Outcome {
        Outcome {
            channel: channel(id),
            result: Ok(Listing {
                channel: Some(ChannelInfo {
                    title: "Told".into(),
                    ..ChannelInfo::new(channel(id))
                }),
                videos: vec![Video {
                    channel_id: Some(channel(id)),
                    ..Video::new(video_id(video))
                }],
            }),
        }
    }

    fn saved_feed_on_disk(folder: &Folder) -> SavedFeed {
        SavedFeed::load(&folder.paths().youtube_feed_file())
    }

    #[test]
    fn a_long_update_is_saved_as_it_goes() {
        let folder = Folder::new("saved-as-it-goes");
        let session = working(&folder);
        session.store().add_channels(vec![Channel::new(channel(A))]);
        *session.refresh.borrow_mut() = Some(Cancel::default());
        session.took_outcome(answer_for(A, "aaaaaaaaaaa"));
        assert!(
            !folder.paths().youtube_feed_file().exists(),
            "not after every answer"
        );
        session.save_every.set(Duration::ZERO);
        session.took_outcome(answer_for(A, "bbbbbbbbbbb"));
        let subscribed = session.subscribed();
        assert_eq!(
            saved_feed_on_disk(&folder).sorted(&subscribed, false).len(),
            2
        );
        // It isn't a check until it has ended.
        assert_eq!(saved_feed_on_disk(&folder).last_refresh(), None);
        assert_eq!(working(&folder).channels()[0].title, "Told");
    }

    #[test]
    fn quitting_during_an_update_keeps_what_came() {
        let folder = Folder::new("quit");
        let first = Stub::working();
        let session = session_with(&folder, first.clone(), Stub::working());
        session.store().add_channels(vec![Channel::new(channel(A))]);
        session.entered();
        session.feed().finished(now() - 3600);
        let cancel = Cancel::default();
        *session.refresh.borrow_mut() = Some(cancel.clone());
        session.took_outcome(answer_for(A, "aaaaaaaaaaa"));
        session.quit();
        assert!(cancel.cancelled());
        let subscribed = session.subscribed();
        let saved = saved_feed_on_disk(&folder);
        assert_eq!(saved.sorted(&subscribed, false).len(), 1);
        assert_eq!(working(&folder).channels()[0].title, "Told");
        // Nothing starts from here on.
        session.refresh_finished(&Preferences::default());
        assert_eq!(first.calls(), 0);
    }

    #[test]
    fn quitting_with_nothing_new_writes_nothing() {
        let folder = Folder::new("quit-idle");
        let session = working(&folder);
        session.quit();
        assert!(!folder.paths().data.exists());
    }

    /// A slow older save can't land on a newer one.
    #[test]
    fn saves_of_the_feed_land_in_order() {
        let folder = Folder::new("save-order");
        let path = folder.0.join("feed.json");
        let written = Mutex::new(0);
        write_in_turn(&written, 2, &path, b"newer");
        write_in_turn(&written, 1, &path, b"older");
        assert_eq!(std::fs::read(&path).unwrap(), b"newer");
    }

    /// Back in the section while the update it left winds down: that wish was
    /// refused for as long as it ran, and is taken up as it ends, for what the
    /// stopped update hadn't reached.
    #[test]
    fn coming_back_while_a_stopped_update_ends_still_updates() {
        let folder = Folder::new("back");
        let first = Stub::working();
        let session = session_with(&folder, first.clone(), Stub::working());
        let prefs = Preferences::default();
        session
            .store()
            .add_channels(vec![Channel::new(channel(A)), Channel::new(channel(B))]);
        session.entered();
        let cancel = Cancel::default();
        *session.refresh.borrow_mut() = Some(cancel.clone());
        session.checking.set(Some(now()));
        session.took_outcome(answer_for(A, "aaaaaaaaaaa"));
        session.left();
        assert!(cancel.cancelled());
        session.entered();
        session.section_opened(&prefs);
        assert!(session.is_refreshing(), "still winding down");
        assert_eq!(first.calls(), 0);
        session.refresh_finished(&prefs);
        assert_eq!(first.calls(), 1, "only the channel it hadn't reached");
        assert!(session.last_refresh().is_some(), "and that makes a check");
        assert!(
            session
                .channel_state(&channel(B))
                .unwrap()
                .last_ok
                .is_some()
        );
    }

    /// Preferences changed during an update stop it, and the channels it
    /// hadn't reached are asked as it winds down, from sources made with the
    /// new ones rather than those it began with.
    #[test]
    fn changed_preferences_stop_an_update_and_ask_the_rest_with_the_new() {
        let folder = Folder::new("update-prefs");
        let made: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
        let making = made.clone();
        let session = Session::with_sources(
            &folder.paths(),
            Box::new(move |prefs, _| {
                making.borrow_mut().push(prefs.proxy().map(str::to_owned));
                (
                    Box::new(Stub::working()) as Box<dyn Source>,
                    Box::new(Stub::working()) as Box<dyn Source>,
                )
            }),
        );
        session.limits.set(Limits {
            batch_gap: Duration::ZERO,
            ..Default::default()
        });
        let old = Preferences {
            http_proxy: "http://127.0.0.1:1".into(),
            ..Default::default()
        };
        let new = Preferences {
            http_proxy: "http://127.0.0.1:2".into(),
            ..Default::default()
        };
        session
            .store()
            .add_channels(vec![Channel::new(channel(A)), Channel::new(channel(B))]);
        session.entered();
        let cancel = Cancel::default();
        *session.refresh.borrow_mut() = Some(cancel.clone());
        session.checking.set(Some(now()));
        session.took_outcome(answer_for(A, "aaaaaaaaaaa"));
        session.config_changed(&new);
        assert!(cancel.cancelled());
        assert!(made.borrow().is_empty(), "still winding down");
        session.refresh_finished(&old);
        assert_eq!(*made.borrow(), [Some("http://127.0.0.1:2".to_owned())]);
        assert!(
            session
                .channel_state(&channel(B))
                .unwrap()
                .last_ok
                .is_some()
        );
    }

    /// A list is read off the interface thread, and the preferences can
    /// change meanwhile: what the import starts goes by the new ones, and
    /// leaves them the newest known.
    #[test]
    fn an_import_that_ends_after_a_change_of_preferences_goes_by_the_new() {
        let folder = Folder::new("import-prefs");
        let made: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
        let making = made.clone();
        let session = Session::with_sources(
            &folder.paths(),
            Box::new(move |prefs, _| {
                making.borrow_mut().push(prefs.proxy().map(str::to_owned));
                (
                    Box::new(Stub::working()) as Box<dyn Source>,
                    Box::new(Stub::working()) as Box<dyn Source>,
                )
            }),
        );
        let old = Preferences {
            http_proxy: "http://127.0.0.1:1".into(),
            ..Default::default()
        };
        let new = Preferences {
            http_proxy: "http://127.0.0.1:2".into(),
            ..Default::default()
        };
        session.entered();
        session.config_changed(&new);
        session.took_import(&old, Rows::Channels(vec![Channel::new(channel(A))]), 0);
        assert_eq!(*made.borrow(), [Some("http://127.0.0.1:2".to_owned())]);
        let latest = session.latest_prefs.borrow();
        assert_eq!(
            latest.as_ref().and_then(Preferences::proxy),
            Some("http://127.0.0.1:2")
        );
    }

    /// Update Now works whether or not an update is due, so one pressed soon
    /// after the last check carries on after a change of preferences too.
    #[test]
    fn changed_preferences_carry_on_an_update_that_was_not_due() {
        let folder = Folder::new("update-now-prefs");
        let made: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
        let making = made.clone();
        let session = Session::with_sources(
            &folder.paths(),
            Box::new(move |prefs, _| {
                making.borrow_mut().push(prefs.proxy().map(str::to_owned));
                (
                    Box::new(Stub::working()) as Box<dyn Source>,
                    Box::new(Stub::working()) as Box<dyn Source>,
                )
            }),
        );
        session.limits.set(Limits {
            batch_gap: Duration::ZERO,
            ..Default::default()
        });
        let old = Preferences {
            http_proxy: "http://127.0.0.1:1".into(),
            ..Default::default()
        };
        let new = Preferences {
            http_proxy: "http://127.0.0.1:2".into(),
            ..Default::default()
        };
        session
            .store()
            .add_channels(vec![Channel::new(channel(A)), Channel::new(channel(B))]);
        // Both answered in a check that finished ten minutes ago, so no
        // update is due.
        let earlier = now() - 700;
        session.feed().apply(answer_for(A, "aaaaaaaaaaa"), earlier);
        session.feed().apply(answer_for(B, "bbbbbbbbbbb"), earlier);
        session.feed().finished(now() - 600);
        session.entered();
        let began = now();
        let cancel = Cancel::default();
        *session.refresh.borrow_mut() = Some(cancel.clone());
        session.checking.set(Some(began));
        session.took_outcome(answer_for(A, "aaaaaaaaaaa"));
        session.config_changed(&new);
        assert!(cancel.cancelled());
        session.refresh_finished(&old);
        assert_eq!(*made.borrow(), [Some("http://127.0.0.1:2".to_owned())]);
        let asked = session.channel_state(&channel(B)).unwrap().last_ok;
        assert!(asked.is_some_and(|at| at >= began));
        assert_eq!(session.last_refresh(), Some(began));
    }

    /// Stopped with no window back, an update doesn't count, and nothing runs.
    #[test]
    fn leaving_during_an_update_leaves_it_due_and_nothing_running() {
        let folder = Folder::new("left");
        let first = Stub::working();
        let session = session_with(&folder, first.clone(), Stub::working());
        let prefs = Preferences::default();
        session.store().add_channels(vec![Channel::new(channel(A))]);
        session.entered();
        *session.refresh.borrow_mut() = Some(Cancel::default());
        session.checking.set(Some(now()));
        session.left();
        session.refresh_finished(&prefs);
        assert_eq!(first.calls(), 0);
        assert_eq!(session.last_refresh(), None);
        session.entered();
        session.section_opened(&prefs);
        assert_eq!(first.calls(), 1);
    }

    /// A channel's own fetch isn't the update of them all: stopping it costs
    /// no one else a request, and the channel is asked at the next open.
    #[test]
    fn stopping_a_one_channel_fetch_keeps_the_last_check() {
        let folder = Folder::new("one");
        let first = Stub::working();
        let session = session_with(&folder, first.clone(), Stub::working());
        let prefs = Preferences::default();
        session.store().add_channels(vec![Channel::new(channel(A))]);
        session.refresh(&prefs);
        assert_eq!(first.calls(), 1);
        session.store().add_channels(vec![Channel::new(channel(B))]);
        session.entered();
        *session.refresh.borrow_mut() = Some(Cancel::default());
        session.left();
        session.refresh_finished(&prefs);
        assert!(!session.feed().due(now()), "it was checked seconds ago");
        session.entered();
        session.section_opened(&prefs);
        assert_eq!(first.calls(), 2, "only the new channel");
        assert!(session.channel_state(&channel(B)).is_some());
    }

    #[test]
    fn channels_waiting_on_an_update_do_not_start_with_no_window_showing() {
        let folder = Folder::new("waiting-left");
        let first = Stub::working();
        let session = session_with(&folder, first.clone(), Stub::working());
        let prefs = Preferences::default();
        session.entered();
        *session.refresh.borrow_mut() = Some(Cancel::default());
        session.subscribe(&prefs, info(A));
        session.left();
        session.refresh_finished(&prefs);
        assert_eq!(first.calls(), 0, "no window shows the section");
        assert!(session.waiting.borrow().is_empty());
        // They are asked as it opens again, though no update is due.
        session.feed().finished(now());
        session.entered();
        session.section_opened(&prefs);
        assert_eq!(first.calls(), 1);
    }

    #[test]
    fn what_stops_the_titles_is_what_would_stop_the_next_one() {
        let failed = |kind: Failure| Err(Error::from(kind));
        let fine = Ok(Video::new(video_id("abcdefghijk")));
        let mut timeouts = 0;
        assert!(stops_details(&failed(Failure::Blocked), &mut timeouts));
        assert!(!stops_details(&failed(Failure::BadOutput), &mut timeouts));
        assert!(!stops_details(&failed(Failure::Failed), &mut timeouts));
        assert!(!stops_details(&failed(Failure::Private), &mut timeouts));
        // One time limit can be that video, and two in a row are the way there.
        assert!(!stops_details(&failed(Failure::Timeout), &mut timeouts));
        assert!(!stops_details(&fine, &mut timeouts));
        assert!(!stops_details(&failed(Failure::Timeout), &mut timeouts));
        assert!(stops_details(&failed(Failure::Timeout), &mut timeouts));
    }

    #[test]
    fn progress_counts_the_answers() {
        let folder = Folder::new("progress");
        let session = working(&folder);
        let prefs = Preferences::default();
        session
            .store()
            .add_channels(vec![Channel::new(channel(A)), Channel::new(channel(B))]);
        session.refresh(&prefs);
        assert_eq!(session.progress(), (2, 2));
    }

    #[test]
    fn clearing_positions_keeps_what_was_seen() {
        let folder = Folder::new("positions");
        let session = working(&folder);
        let prefs = Preferences::default();
        let id = video_id("abcdefghijk");
        let other = video_id("bbbbbbbbbbb");
        session.note_watch(
            &prefs,
            &Seen {
                id: id.clone(),
                position: 300.0,
                duration: 600.0,
                played: 200.0,
            },
        );
        session.note_watch(&prefs, &seen(&other));
        assert_eq!(session.resume_from(&prefs, &id), Some(300.0));
        let heard = listen(&session);
        session.clear_positions();
        assert_eq!(*heard.0.borrow(), [Change::Marks]);
        assert_eq!(session.resume_from(&prefs, &id), None);
        let mark = session.mark(&id).unwrap();
        assert_eq!(mark.position, 0.0);
        assert_eq!(mark.seen, 200.0);
        assert!(session.mark(&other).unwrap().watched);
        // Nothing to clear, nothing to tell.
        session.clear_positions();
        assert_eq!(heard.0.borrow().len(), 1);
    }

    #[test]
    fn a_failure_of_both_sources_leaves_saved_videos_and_says_why() {
        let folder = Folder::new("failing");
        let prefs = Preferences::default();
        {
            let session = working(&folder);
            session.subscribe(&prefs, info(A));
        }
        let session = session_with(
            &folder,
            Stub::failing(Failure::NotFeed),
            Stub::failing(Failure::Blocked),
        );
        session.refresh(&prefs);
        assert_eq!(session.feed_videos().len(), 1);
        let state = session.channel_state(&channel(A)).unwrap();
        assert_eq!(state.failure, Some(Failure::Blocked));
        assert!(!state.gone);
        assert!(state.last_ok.is_some());
        assert!(session.is_subscribed(&channel(A)));
        assert!(!session.is_refreshing());
    }

    #[test]
    fn does_not_exist_marks_but_never_removes() {
        let folder = Folder::new("gone");
        let session = session_with(
            &folder,
            Stub::failing(Failure::NotFeed),
            Stub::failing(Failure::NoSuchChannel),
        );
        session.subscribe(&Preferences::default(), info(A));
        assert!(session.channel_state(&channel(A)).unwrap().gone);
        assert!(session.is_subscribed(&channel(A)));
    }

    #[test]
    fn an_answer_about_a_channel_updates_its_row() {
        let folder = Folder::new("told");
        let session = working(&folder);
        let heard = listen(&session);
        session.subscribe(&Preferences::default(), info(A));
        assert_eq!(session.channels()[0].title, "Told");
        assert!(
            heard
                .0
                .borrow()
                .iter()
                .filter(|change| **change == Change::Channels)
                .count()
                >= 2
        );
        assert_eq!(
            working(&folder).channels()[0].title,
            "Told",
            "and it is saved"
        );
    }

    #[test]
    fn channels_added_during_a_refresh_wait_for_it() {
        let folder = Folder::new("waiting");
        let first = Stub::working();
        let session = session_with(&folder, first.clone(), Stub::working());
        session.entered();
        *session.refresh.borrow_mut() = Some(Cancel::default());
        let prefs = Preferences::default();
        session.subscribe(&prefs, info(A));
        assert_eq!(first.calls(), 0);
        session.refresh_finished(&prefs);
        assert_eq!(first.calls(), 1);
    }

    #[test]
    fn unsubscribing_hides_videos_at_once_and_forgets_them_at_the_next_refresh() {
        let folder = Folder::new("unsubscribe");
        let session = working(&folder);
        let prefs = Preferences::default();
        session.subscribe(&prefs, info(A));
        session.subscribe(&prefs, info(B));
        assert_eq!(session.feed_videos().len(), 2);
        session.unsubscribe(&channel(A));
        assert_eq!(session.feed_videos().len(), 1);
        session.refresh(&prefs);
        assert!(session.channel_state(&channel(A)).is_none());
    }

    #[test]
    fn the_shorts_setting_lasts_and_filters() {
        let folder = Folder::new("shorts");
        let session = working(&folder);
        assert!(!session.show_shorts());
        session.set_show_shorts(true);
        assert!(working(&folder).show_shorts());
    }

    #[test]
    fn the_saved_list_comes_newest_first() {
        let folder = Folder::new("saved");
        let session = working(&folder);
        session.store().add_saved(
            vec![
                SavedVideo::new(Video::new(video_id("oldoldoldol")), 10),
                SavedVideo::new(Video::new(video_id("newnewnewne")), 20),
            ],
            30,
        );
        let ids: Vec<String> = session
            .saved()
            .iter()
            .map(|row| row.video.id.as_str().to_owned())
            .collect();
        assert_eq!(ids, ["newnewnewne", "oldoldoldol"]);
        assert!(session.is_saved(&video_id("oldoldoldol")));
        session.unsave_video(&video_id("oldoldoldol"));
        assert!(!session.is_saved(&video_id("oldoldoldol")));
    }

    /// A sitting that saw nearly all of a video.
    fn seen(id: &VideoId) -> Seen {
        Seen {
            id: id.clone(),
            position: 90.0,
            duration: 100.0,
            played: 90.0,
        }
    }

    #[test]
    fn a_mark_comes_from_what_was_seen_not_from_opening() {
        let folder = Folder::new("sat");
        let session = working(&folder);
        let prefs = Preferences::default();
        let id = video_id("abcdefghijk");
        // Sought to the end, having seen a minute of it.
        session.note_watch(
            &prefs,
            &Seen {
                id: id.clone(),
                position: 590.0,
                duration: 600.0,
                played: 60.0,
            },
        );
        let mark = session.mark(&id).unwrap();
        assert!(!mark.watched);
        assert_eq!(session.resume_from(&prefs, &id), None);
        // Back to the middle and on from there, and a sitting adds to the last.
        session.note_watch(
            &prefs,
            &Seen {
                id: id.clone(),
                position: 300.0,
                duration: 600.0,
                played: 200.0,
            },
        );
        assert_eq!(session.resume_from(&prefs, &id), Some(300.0));
        let off = Preferences {
            resume_playback: false,
            ..Default::default()
        };
        assert_eq!(session.resume_from(&off, &id), None);
        session.note_watch(
            &prefs,
            &Seen {
                id: id.clone(),
                position: 600.0,
                duration: 600.0,
                played: 300.0,
            },
        );
        assert!(session.mark(&id).unwrap().watched);
        assert_eq!(session.resume_from(&prefs, &id), None);
        session.forget_watch(&id);
        assert_eq!(session.mark(&id), None);
    }

    /// A stand-in for the trash that notes what came, and fails if asked to.
    fn stub_trash(session: &Session, fails: bool) -> Rc<RefCell<Vec<String>>> {
        let moved = Rc::new(RefCell::new(Vec::new()));
        let noted = moved.clone();
        session.set_trash(Box::new(move |paths, done| {
            noted.borrow_mut().extend(paths.iter().cloned());
            let failed = if fails {
                paths
                    .into_iter()
                    .map(|path| (path, "it won't".to_owned()))
                    .collect()
            } else {
                paths.iter().for_each(|path| {
                    std::fs::remove_file(path).unwrap();
                });
                Vec::new()
            };
            done(failed);
        }));
        moved
    }

    #[test]
    fn a_removed_download_goes_to_the_trash_and_loses_its_mark() {
        let folder = Folder::new("trash");
        let session = working(&folder);
        let id = video_id("abcdefghijk");
        let file = folder.0.join("Video [abcdefghijk].mp4");
        std::fs::write(&file, b"video").unwrap();
        session.set_download(&id, &file);
        assert_eq!(session.download(&id).as_deref(), file.to_str());
        assert_eq!(session.source_of(&id), file.to_str().unwrap());
        assert_eq!(session.video_at(file.to_str().unwrap()), Some(id.clone()));

        // It stays marked if the file couldn't be moved.
        stub_trash(&session, true);
        let said = Rc::new(RefCell::new(None));
        let heard = said.clone();
        session.trash_download(&id, move |answer| *heard.borrow_mut() = Some(answer));
        assert_eq!(*said.borrow(), Some(Err("it won't".to_owned())));
        assert!(session.download(&id).is_some());
        assert!(file.exists());

        let moved = stub_trash(&session, false);
        let heard = said.clone();
        session.trash_download(&id, move |answer| *heard.borrow_mut() = Some(answer));
        assert_eq!(*said.borrow(), Some(Ok(())));
        assert_eq!(*moved.borrow(), [file.to_str().unwrap().to_owned()]);
        assert_eq!(session.download(&id), None);
        assert_eq!(session.source_of(&id), id.watch_url());
        assert_eq!(session.video_at(file.to_str().unwrap()), None);
    }

    /// A file moved away by hand has nothing to send to the trash.
    #[test]
    fn a_download_whose_file_went_only_loses_its_mark() {
        let folder = Folder::new("trash-gone");
        let session = working(&folder);
        let id = video_id("abcdefghijk");
        let file = folder.0.join("gone.mp4");
        std::fs::write(&file, b"video").unwrap();
        session.set_download(&id, &file);
        std::fs::remove_file(&file).unwrap();
        assert_eq!(session.download(&id), None);
        let moved = stub_trash(&session, false);
        let said = Rc::new(RefCell::new(None));
        let heard = said.clone();
        session.trash_download(&id, move |answer| *heard.borrow_mut() = Some(answer));
        assert_eq!(*said.borrow(), Some(Ok(())));
        assert!(moved.borrow().is_empty());
        assert_eq!(session.video_at(file.to_str().unwrap()), None);
    }

    #[test]
    fn marks_follow_the_history_preference_and_clearing() {
        let folder = Folder::new("marks");
        let session = working(&folder);
        let id = video_id("abcdefghijk");
        let off = Preferences {
            record_history: false,
            ..Default::default()
        };
        session.note_watch(&off, &seen(&id));
        assert_eq!(session.mark(&id), None);
        session.note_watch(&Preferences::default(), &seen(&id));
        assert!(session.mark(&id).unwrap().watched);
        let heard = listen(&session);
        session.clear_marks();
        assert_eq!(session.mark(&id), None);
        assert_eq!(*heard.0.borrow(), [Change::Marks]);
        session.clear_marks();
        assert_eq!(
            heard.0.borrow().len(),
            1,
            "nothing to clear, nothing to tell"
        );
    }

    #[test]
    fn clearing_marks_before_anything_was_read_writes_nothing() {
        let folder = Folder::new("clear-first");
        let session = working(&folder);
        session.clear_marks();
        assert!(!folder.paths().youtube_file().exists());
    }

    #[test]
    fn a_damaged_file_is_mentioned_and_kept() {
        let folder = Folder::new("damaged");
        let paths = folder.paths();
        std::fs::create_dir_all(&paths.data).unwrap();
        std::fs::write(paths.youtube_file(), b"{broken").unwrap();
        let session = working(&folder);
        let notices = session.take_notices();
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("A copy is kept at"), "{notices:?}");
        assert!(session.take_notices().is_empty(), "said once");
        assert_eq!(
            std::fs::read(paths.data.join("youtube.json.bad")).unwrap(),
            b"{broken"
        );
    }

    #[test]
    fn a_save_that_fails_tells_the_user_and_keeps_the_change() {
        let folder = Folder::new("unsaved");
        let paths = folder.paths();
        std::fs::create_dir_all(&paths.data).unwrap();
        // A folder where the file goes can't be read or written over.
        std::fs::create_dir(paths.youtube_file()).unwrap();
        let session = working(&folder);
        session.save_video(Video::new(video_id("abcdefghijk")));
        assert!(session.is_saved(&video_id("abcdefghijk")));
        let notices = session.take_notices();
        assert!(
            notices
                .iter()
                .any(|notice| notice.contains("could not be saved")),
            "{notices:?}"
        );
    }

    fn write_list(folder: &Folder, name: &str, text: &str) -> PathBuf {
        let path = folder.0.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn importing_subscriptions_counts_adds_and_fetches() {
        let folder = Folder::new("import-subs");
        let first = Stub::working();
        let session = session_with(&folder, first.clone(), Stub::working());
        let prefs = Preferences::default();
        session.subscribe(&prefs, info(B));
        let before = first.calls();
        let path = write_list(
            &folder,
            "subscriptions.csv",
            &format!(
                "Channel Id,Channel Url,Channel Title\n\
                 {A},http://www.youtube.com/channel/{A},\"A, with a comma\"\n\
                 {B},http://www.youtube.com/channel/{B},Already here\n\
                 nonsense,http://www.youtube.com/channel/x,Bad\n"
            ),
        );
        let report = Rc::new(RefCell::new(None));
        let seen = report.clone();
        session.import(&prefs, &path, move |answer| {
            *seen.borrow_mut() = Some(answer)
        });
        assert_eq!(
            report.borrow().clone().unwrap(),
            Ok(ImportReport {
                kind: ListKind::Subscriptions,
                added: 1,
                present: 1,
                skipped: 1,
            })
        );
        assert_eq!(first.calls(), before + 1, "only the new channel is fetched");
        assert!(session.is_subscribed(&channel(A)));
        // The file isn't needed any more.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(working(&folder).channels().len(), 2);
    }

    /// A NewPipe file goes the same way as Google's, whatever it is called:
    /// the same counts, and only the new channel fetched.
    #[test]
    fn importing_a_newpipe_file_counts_adds_and_fetches() {
        let folder = Folder::new("import-newpipe");
        let first = Stub::working();
        let session = session_with(&folder, first.clone(), Stub::working());
        let prefs = Preferences::default();
        session.subscribe(&prefs, info(B));
        let before = first.calls();
        let path = write_list(
            &folder,
            "newpipe_subscriptions_202611061200.txt",
            &format!(
                r#"{{"app_version":"0.29.1","app_version_int":1015,"subscriptions":[
                {{"service_id":0,"url":"https://www.youtube.com/channel/{A}","name":"New"}},
                {{"service_id":0,"url":"https://www.youtube.com/channel/{B}","name":"Already here"}},
                {{"service_id":0,"url":"https://www.youtube.com/@handle","name":"A handle"}},
                {{"service_id":1,"url":"https://soundcloud.com/x","name":"Not YouTube"}}]}}"#
            ),
        );
        let report = Rc::new(RefCell::new(None));
        let seen = report.clone();
        session.import(&prefs, &path, move |answer| {
            *seen.borrow_mut() = Some(answer)
        });
        assert_eq!(
            report.borrow().clone().unwrap(),
            Ok(ImportReport {
                kind: ListKind::Subscriptions,
                added: 1,
                present: 1,
                skipped: 2,
            })
        );
        assert_eq!(first.calls(), before + 1, "only the new channel is fetched");
        assert!(session.is_subscribed(&channel(A)));
        assert_eq!(working(&folder).channels().len(), 2);
    }

    #[test]
    fn json_that_is_not_newpipes_changes_nothing() {
        let folder = Folder::new("import-json");
        let session = working(&folder);
        let path = write_list(&folder, "subscriptions.json", r#"{"channels":[]}"#);
        let report = Rc::new(RefCell::new(None));
        let seen = report.clone();
        session.import(&Preferences::default(), &path, move |answer| {
            *seen.borrow_mut() = Some(answer);
        });
        assert_eq!(
            report.borrow().clone().unwrap(),
            Err(ImportError::NotNewPipe)
        );
        assert!(session.channels().is_empty());
        assert!(!folder.paths().youtube_file().exists());
    }

    /// Channels go out as NewPipe's file and come back the same, into a
    /// session that had none of them.
    #[test]
    fn a_newpipe_export_reads_back_in() {
        let folder = Folder::new("export-newpipe");
        let session = session_with(
            &folder,
            Stub::failing(Failure::NotFeed),
            Stub::failing(Failure::Failed),
        );
        let prefs = Preferences {
            ytdl: false,
            ..Default::default()
        };
        let read = write_list(
            &folder,
            "subscriptions.csv",
            &format!(
                "Channel Id,Channel Url,Channel Title\n\
                 {A},http://www.youtube.com/channel/{A},\"A, with \"\"quotes\"\"\"\n\
                 {B},http://www.youtube.com/channel/{B},Ünïcode\n"
            ),
        );
        session.import(&prefs, &read, |answer| assert!(answer.is_ok()));
        let target = folder.0.join("newpipe.json");
        session.export(ExportFile::NewPipe, &target, |answer| {
            assert_eq!(answer, Ok(()))
        });
        let written = std::fs::read_to_string(&target).unwrap();
        assert!(
            written.starts_with(r#"{"subscriptions":[{"service_id":0,"#)
                && written.ends_with(r#"],"app_version":"0.29.1","app_version_int":1015}"#),
            "{written}"
        );

        let other = Folder::new("export-newpipe-other");
        let again = session_with(
            &other,
            Stub::failing(Failure::NotFeed),
            Stub::failing(Failure::Failed),
        );
        let report = Rc::new(RefCell::new(None));
        let seen = report.clone();
        again.import(&prefs, &target, move |answer| {
            *seen.borrow_mut() = Some(answer)
        });
        assert_eq!(
            report.borrow().clone().unwrap().unwrap(),
            ImportReport {
                kind: ListKind::Subscriptions,
                added: 2,
                present: 0,
                skipped: 0,
            }
        );
        let titles = |session: &Session| -> Vec<(String, String)> {
            session
                .channels()
                .into_iter()
                .map(|channel| (channel.id.as_str().to_owned(), channel.title))
                .collect()
        };
        assert_eq!(titles(&again), titles(&session));
        assert_eq!(titles(&again)[0].1, "A, with \"quotes\"");
    }

    #[test]
    fn a_file_that_is_not_a_list_changes_nothing() {
        let folder = Folder::new("import-bad");
        let session = working(&folder);
        let path = write_list(&folder, "other.txt", "hello");
        let report = Rc::new(RefCell::new(None));
        let seen = report.clone();
        session.import(&Preferences::default(), &path, move |answer| {
            *seen.borrow_mut() = Some(answer);
        });
        assert_eq!(
            report.borrow().clone().unwrap(),
            Err(ImportError::NotRecognized)
        );
        assert!(session.channels().is_empty());
        assert!(!folder.paths().youtube_file().exists());
    }

    #[test]
    fn export_gives_back_what_import_read() {
        let folder = Folder::new("export");
        // Nothing answers, so that the titles stay as the file gave them.
        let session = session_with(
            &folder,
            Stub::failing(Failure::NotFeed),
            Stub::failing(Failure::Failed),
        );
        let prefs = Preferences {
            ytdl: false,
            ..Default::default()
        };
        let subscriptions = format!(
            "Channel Id,Channel Url,Channel Title\n\
             {A},http://www.youtube.com/channel/{A},\"A, with a comma\"\n"
        );
        let watch_later = "Video ID,Playlist Video Creation Timestamp\n\
                           -bcdefgh_jk,2026-01-02T03:04:05+00:00\n";
        let read = write_list(&folder, "subscriptions.csv", &subscriptions);
        session.import(&prefs, &read, |answer| assert!(answer.is_ok()));
        let read = write_list(&folder, "watch-later.csv", watch_later);
        session.import(&prefs, &read, |answer| assert!(answer.is_ok()));
        let target = folder.0.join("exported.csv");
        session.export(ExportFile::Subscriptions, &target, |answer| {
            assert_eq!(answer, Ok(()))
        });
        assert_eq!(std::fs::read_to_string(&target).unwrap(), subscriptions);
        session.export(ExportFile::WatchLater, &target, |answer| {
            assert_eq!(answer, Ok(()))
        });
        assert_eq!(std::fs::read_to_string(&target).unwrap(), watch_later);
        let nowhere = folder.0.join("no").join("such").join("folder.csv");
        session.export(ExportFile::WatchLater, &nowhere, |answer| {
            assert!(answer.is_err())
        });
    }

    #[test]
    fn with_online_video_off_nothing_runs_and_the_section_says_why() {
        let folder = Folder::new("off");
        let session = working(&folder);
        let prefs = Preferences {
            ytdl: false,
            ..Default::default()
        };
        session.search(&prefs, "a query", &Cancel::default(), |answer| {
            assert_eq!(answer.unwrap_err().kind, Failure::Disabled);
        });
        session.channel_page(&prefs, &channel(A), |answer| {
            assert_eq!(answer.unwrap_err().kind, Failure::Disabled);
        });
        session.resolve_channel(&prefs, "@name", |answer| {
            assert_eq!(answer.unwrap_err().kind, Failure::Disabled);
        });
        session.tool_state(&prefs, |state| {
            let ToolState::Unusable(error) = state else {
                panic!("usable");
            };
            assert_eq!(error.kind, Failure::Disabled);
        });
    }

    #[test]
    fn nonsense_pasted_as_a_channel_runs_nothing() {
        let folder = Folder::new("nonsense");
        let session = working(&folder);
        let prefs = Preferences {
            ytdl_path: "/nonexistent/yt-dlp".into(),
            ..Default::default()
        };
        let heard = Rc::new(Cell::new(false));
        let seen = heard.clone();
        session.resolve_channel(&prefs, "https://evil.example/@name", move |answer| {
            seen.set(true);
            let error = answer.unwrap_err();
            assert_eq!(error.kind, Failure::Failed);
            assert!(error.detail.contains("@handle"));
        });
        assert!(heard.get());
    }

    #[test]
    fn real_sources_name_their_failures_without_asking_anyone() {
        let prefs = Preferences {
            ytdl: false,
            ..Default::default()
        };
        let (_, second) = real_sources(&prefs, Path::new("/nonexistent"));
        let id = channel(A);
        let answers = second.fetch(std::slice::from_ref(&id), &Cancel::default());
        assert_eq!(answers[0].as_ref().unwrap_err().kind, Failure::Disabled);
    }

    #[cfg(unix)]
    mod with_a_fake {
        use super::super::super::ytdlp::testing::Fake;
        use super::*;

        fn prefs(fake: &Fake) -> Preferences {
            Preferences {
                ytdl_path: fake.program.to_string_lossy().into_owned(),
                // Whatever is in here must not reach the section's calls.
                ytdl_raw_options: "cookies-from-browser=firefox".into(),
                ..Default::default()
            }
        }

        /// A script answering by the address it was given last.
        fn answering(folder: &Folder, body: &str) -> Fake {
            Fake::new(&folder.0.join("bin"), body)
        }

        const DETAILS: &str = r#"
for last; do :; done
case "$last" in
  *=aaaaaaaaaaa) echo '{"id":"aaaaaaaaaaa","title":"First title","channel":"C","timestamp":1793594400}' ;;
  *=bbbbbbbbbbb) echo null; echo 'ERROR: [youtube] bbbbbbbbbbb: Private video.' >&2; exit 1 ;;
  *=ccccccccccc) echo '{"id":"ccccccccccc","title":"Third title"}' ;;
esac
"#;

        fn import_watch_later(session: &Rc<Session>, prefs: &Preferences, folder: &Folder) {
            let path = write_list(
                folder,
                "wl.csv",
                "Video ID,Playlist Video Creation Timestamp\n\
                 aaaaaaaaaaa,2026-01-02T03:04:05+00:00\n\
                 bbbbbbbbbbb,2026-01-02T03:04:06+00:00\n\
                 ccccccccccc,2026-01-02T03:04:07+00:00\n",
            );
            session.import(prefs, &path, |answer| {
                assert_eq!(answer.unwrap().added, 3);
            });
        }

        #[test]
        fn watch_later_titles_fill_in_after_an_import() {
            let folder = Folder::new("details");
            let fake = answering(&folder, DETAILS);
            let prefs = prefs(&fake);
            let session = working(&folder);
            import_watch_later(&session, &prefs, &folder);
            let rows = session.saved();
            let row = |id: &str| rows.iter().find(|row| row.video.id.as_str() == id).unwrap();
            assert_eq!(row("aaaaaaaaaaa").video.title, "First title");
            assert_eq!(row("ccccccccccc").video.title, "Third title");
            assert_eq!(
                row("aaaaaaaaaaa").added_at,
                1_767_323_045,
                "kept from the file"
            );
            assert_eq!(
                row("aaaaaaaaaaa").video.published.unwrap().at,
                1_793_594_400
            );
            assert!(row("aaaaaaaaaaa").video.published.unwrap().exact);
            // Private: it stays, marked, and isn't asked for again.
            assert!(row("bbbbbbbbbbb").unavailable);
            assert_eq!(fake.runs().len(), 3);
            // Newest added first, as the list shows them.
            let asked: Vec<String> = fake
                .runs()
                .iter()
                .map(|run| run.last().unwrap().rsplit('=').next().unwrap().to_owned())
                .collect();
            assert_eq!(asked, ["ccccccccccc", "bbbbbbbbbbb", "aaaaaaaaaaa"]);
            session.fetch_details(&prefs);
            assert_eq!(fake.runs().len(), 3);
            for run in fake.runs() {
                assert!(!run.iter().any(|arg| arg.contains("cookies")), "{run:?}");
                assert_eq!(run[0], "--ignore-config");
            }
        }

        #[test]
        fn a_block_stops_the_titles_and_counts_against_no_video() {
            let folder = Folder::new("details-blocked");
            let fake = answering(
                &folder,
                "echo null\necho 'ERROR: HTTP Error 429: Too Many Requests' >&2\nexit 1",
            );
            let prefs = prefs(&fake);
            let session = working(&folder);
            import_watch_later(&session, &prefs, &folder);
            assert_eq!(fake.runs().len(), 1, "it stopped at the first");
            assert!(
                session
                    .saved()
                    .iter()
                    .all(|row| row.tries == 0 && !row.unavailable)
            );
            assert!(!session.details.borrow().is_some());
            // They are all still to do, next time.
            assert_eq!(session.store().saved_without_details(DETAIL_TRIES).len(), 3);
        }

        /// One row's answer that can't be read costs that row a try, and
        /// doesn't hold up those after it.
        #[test]
        fn one_unreadable_answer_does_not_hold_up_the_other_titles() {
            let folder = Folder::new("details-unreadable");
            let fake = answering(
                &folder,
                r#"
for last; do :; done
case "$last" in
  *=ccccccccccc) echo 'not json' ;;
  *=bbbbbbbbbbb) echo '{"id":"bbbbbbbbbbb","title":"Second title"}' ;;
  *=aaaaaaaaaaa) echo '{"id":"aaaaaaaaaaa","title":"First title"}' ;;
esac
"#,
            );
            let prefs = prefs(&fake);
            let session = working(&folder);
            import_watch_later(&session, &prefs, &folder);
            let rows = session.saved();
            let row = |id: &str| rows.iter().find(|row| row.video.id.as_str() == id).unwrap();
            assert_eq!(row("aaaaaaaaaaa").video.title, "First title");
            assert_eq!(row("bbbbbbbbbbb").video.title, "Second title");
            assert_eq!(row("ccccccccccc").tries, 1);
            // It runs out of tries, and is no longer asked for.
            session.fetch_details(&prefs);
            session.fetch_details(&prefs);
            assert_eq!(session.saved()[0].tries, 3);
            let runs = fake.runs().len();
            session.fetch_details(&prefs);
            assert_eq!(fake.runs().len(), runs);
            // Importing it again gives it those tries anew.
            let path = write_list(
                &folder,
                "again.csv",
                "Video ID,Playlist Video Creation Timestamp\n\
                 ccccccccccc,2026-01-02T03:04:07+00:00\n",
            );
            session.import(&prefs, &path, |answer| {
                assert_eq!(answer.unwrap().present, 1);
            });
            assert_eq!(fake.runs().len(), runs + 1);
        }

        /// Rows imported while a pass runs have their titles fetched after it,
        /// in the same sitting.
        #[test]
        fn rows_added_during_a_pass_are_fetched_after_it() {
            let folder = Folder::new("details-second");
            let fake = answering(&folder, DETAILS);
            let prefs = prefs(&fake);
            let session = working(&folder);
            session.entered();
            let running = Cancel::default();
            *session.details.borrow_mut() = Some(running);
            import_watch_later(&session, &prefs, &folder);
            assert_eq!(fake.runs().len(), 0, "one pass at a time");
            session.details_finished(&prefs);
            assert_eq!(fake.runs().len(), 3);
            assert!(!session.details_again.get());
            assert!(session.details.borrow().is_none());
        }

        /// Whatever was asked for before a video starts or the section is
        /// left isn't taken up by itself: coming back asks for it.
        #[test]
        fn a_pass_stopped_on_purpose_does_not_go_on_by_itself() {
            let folder = Folder::new("details-stopped");
            let fake = answering(&folder, DETAILS);
            let prefs = prefs(&fake);
            let session = working(&folder);
            session.entered();
            *session.details.borrow_mut() = Some(Cancel::default());
            import_watch_later(&session, &prefs, &folder);
            session.playing_started();
            session.details_finished(&prefs);
            assert_eq!(fake.runs().len(), 0);
            // Back from the video while it winds down.
            *session.details.borrow_mut() = Some(Cancel::default());
            session.stop_details();
            session.fetch_details(&prefs);
            session.details_finished(&prefs);
            assert_eq!(fake.runs().len(), 3);
        }

        /// Preferences changed while a pass runs stop it, and what starts
        /// again as it winds down goes by the new ones: another proxy for
        /// every call after it, and no call at all once yt-dlp is off.
        #[test]
        fn changed_preferences_stop_the_titles_and_go_on_with_the_new() {
            let folder = Folder::new("details-prefs");
            let fake = answering(&folder, DETAILS);
            let old = Preferences {
                http_proxy: "http://127.0.0.1:1".into(),
                ..prefs(&fake)
            };
            let new = Preferences {
                http_proxy: "http://127.0.0.1:2".into(),
                ..prefs(&fake)
            };
            let session = working(&folder);
            session.entered();
            let running = Cancel::default();
            *session.details.borrow_mut() = Some(running.clone());
            import_watch_later(&session, &old, &folder);
            session.config_changed(&new);
            assert!(running.cancelled());
            assert_eq!(fake.runs().len(), 0, "the old pass is still winding down");
            session.details_finished(&old);
            assert_eq!(fake.runs().len(), 3);
            for run in fake.runs() {
                let proxy = run.iter().position(|arg| arg == "--proxy").unwrap();
                assert_eq!(run[proxy + 1], "http://127.0.0.1:2", "{run:?}");
            }

            let folder = Folder::new("details-off");
            let fake = answering(&folder, DETAILS);
            let prefs = prefs(&fake);
            let session = working(&folder);
            session.entered();
            *session.details.borrow_mut() = Some(Cancel::default());
            import_watch_later(&session, &prefs, &folder);
            session.config_changed(&Preferences {
                ytdl: false,
                ..prefs.clone()
            });
            session.details_finished(&prefs);
            assert_eq!(fake.runs().len(), 0);
        }

        #[test]
        fn the_real_sources_fall_back_to_yt_dlp_with_the_proxy() {
            let folder = Folder::new("real");
            let listing = serde_json::json!({
                "channel_id": A,
                "channel": "From yt-dlp",
                "entries": [{"id": "abcdefghijk", "title": "Listed", "ie_key": "Youtube"}],
            });
            let file = folder.0.join("listing.json");
            std::fs::write(&file, format!("{listing}\n")).unwrap();
            let fake = answering(&folder, &format!("cat '{}'", file.display()));
            let mut prefs = prefs(&fake);
            // Nothing listens there, so the feed fails at once without going
            // anywhere else: this is how the test stays off the network.
            prefs.http_proxy = "http://127.0.0.1:9".into();
            let session = Session::new(&folder.paths());
            session.subscribe(&prefs, info(A));
            let videos = session.feed_videos();
            assert_eq!(videos.len(), 1);
            assert_eq!(videos[0].title, "Listed");
            assert_eq!(session.channels()[0].title, "From yt-dlp");
            let state = session.channel_state(&channel(A)).unwrap();
            assert_eq!(state.failure, None);
            let run = &fake.runs()[0];
            let proxy = run.iter().position(|arg| arg == "--proxy").unwrap();
            assert_eq!(run[proxy + 1], "http://127.0.0.1:9");
            assert!(!run.iter().any(|arg| arg.contains("cookies")), "{run:?}");
        }

        #[test]
        fn yt_dlp_says_its_age_and_when_it_looks_stale() {
            let folder = Folder::new("version");
            let fake = answering(&folder, "echo 2020.01.01");
            let session = working(&folder);
            let state = Rc::new(RefCell::new(None));
            let seen = state.clone();
            session.tool_state(&prefs(&fake), move |answer| {
                *seen.borrow_mut() = Some(answer)
            });
            let ToolState::Ready(version) = state.borrow().clone().unwrap() else {
                panic!("not ready");
            };
            assert!(version.stale);
            assert!(version.age_days.unwrap() > 90);
        }

        /// A search stopped while yt-dlp runs ends it at once, and its answer,
        /// which would come long after, never arrives.
        #[test]
        fn a_search_stopped_on_its_way_ends_and_answers_nothing() {
            let folder = Folder::new("search-stopped");
            let started = folder.0.join("started");
            let fake = answering(
                &folder,
                &format!(
                    "touch '{}'\nsleep 30\nprintf 'null\\nnull\\n'",
                    started.display()
                ),
            );
            let session = working(&folder);
            let cancel = Cancel::default();
            let stopping = cancel.clone();
            let waiting = started.clone();
            std::thread::spawn(move || {
                while !waiting.exists() {
                    std::thread::sleep(Duration::from_millis(10));
                }
                stopping.cancel();
            });
            let heard = Rc::new(Cell::new(false));
            let seen = heard.clone();
            let began = Instant::now();
            session.search(&prefs(&fake), "a query", &cancel, move |_| seen.set(true));
            assert!(started.exists());
            assert!(began.elapsed() < Duration::from_secs(20));
            assert!(!heard.get());
        }

        #[test]
        fn a_search_runs_yt_dlp_and_gets_its_answer() {
            let folder = Folder::new("search");
            let fake = answering(&folder, "printf 'null\\nnull\\n'\nexit 1");
            let session = working(&folder);
            let heard = Rc::new(Cell::new(false));
            let seen = heard.clone();
            session.search(
                &prefs(&fake),
                "a query",
                &Cancel::default(),
                move |answer| {
                    seen.set(true);
                    assert!(answer.is_err());
                },
            );
            assert!(heard.get());
            assert_eq!(fake.runs().len(), 1);
            assert!(!fake.runs()[0].iter().any(|arg| arg.contains("cookies")));
        }
    }
}
