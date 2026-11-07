//! The YouTube section in one window: which page shows, and what fills it. The
//! lists and the feed are the app's (`session.rs`), so that saving a video in
//! one window shows in another; this only draws them, in the YouTube global.
//!
//! Saved things show at once and nothing waits on the network. What comes later
//! (an update's videos, a title for a saved video) changes the lines in place,
//! so that a list being read doesn't jump.

use super::feed::ChannelState;
use super::playing;
use super::session::{Change, ImportReport, ListKind, Listener, Session, ToolState};
use super::store::Channel;
use super::ytdlp::SearchResults;
use super::{Cancel, ChannelId, ChannelInfo, Error, Failure, Live, Video, VideoId};
use crate::app::App;
use crate::jellyfin::images::Generation;
use crate::settings::Preferences;
use crate::window::PlayerController;
use crate::{PlayerWindow, YouTube, YouTubeCard, YouTubeChannel, YouTubeLine};
use slint::{ComponentHandle, Image, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::{Rc, Weak};
use std::time::Duration;

/// A line's images stay this many screens from those in sight: a decoded image
/// for each of thousands of videos would run to gigabytes.
const IMAGE_SCREENS: usize = 2;
/// `mqdefault` is this wide, so a card this wide at most shows it sharply.
const SMALL_PIXELS: u32 = 320;
/// Past this a card looks soft on the small picture, and has `hq720` too.
const LARGE_FROM_PIXELS: u32 = 400;
/// `hq720` is this wide.
const LARGE_PIXELS: u32 = 1280;
/// The large picture is decoded in widths of this step, since each width asked
/// for is another image in memory, and a window being resized goes through many.
const LARGE_STEP_PIXELS: u32 = 160;
/// A channel's picture, at the largest it shows.
const AVATAR_WIDTH: f32 = 48.0;
/// An update's answers come one per channel, and change the feed at most this
/// often.
const UPDATE_EVERY: Duration = Duration::from_millis(500);
/// How long a message over the section stays up.
const NOTICE_SECONDS: f32 = 4.0;
/// The channels a search shows above its videos: it can name many, most of them
/// near misses, and the videos are what was looked for.
const SEARCH_CHANNELS: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Page {
    Feed,
    Search,
    Saved,
    Channels,
    Channel(ChannelId),
}

impl Page {
    fn name(&self) -> &'static str {
        match self {
            Self::Feed => "feed",
            Self::Search => "search",
            Self::Saved => "saved",
            Self::Channels => "channels",
            Self::Channel(_) => "channel",
        }
    }
}

/// The pages Back goes through, each with how far it was scrolled. The bar's
/// pages have Feed under them, whose Back leaves the section.
struct Pages(Vec<(Page, f32)>);

impl Default for Pages {
    fn default() -> Self {
        Self(vec![(Page::Feed, 0.0)])
    }
}

impl Pages {
    fn top(&self) -> Page {
        self.0.last().map_or(Page::Feed, |(page, _)| page.clone())
    }

    fn at_root(&self) -> bool {
        self.0.len() <= 1
    }

    /// The bar's entry that shows as current: a channel's page keeps the one
    /// it was reached from.
    fn nav_current(&self) -> &'static str {
        self.0
            .iter()
            .rev()
            .find(|(page, _)| !matches!(page, Page::Channel(_)))
            .map_or("feed", |(page, _)| page.name())
    }

    /// A page of the bar, keeping how far Feed was scrolled.
    fn go_to(&mut self, page: Page, scroll: f32) {
        if let Some(top) = self.0.last_mut() {
            top.1 = scroll;
        }
        self.0.truncate(1);
        if page != Page::Feed {
            self.0.push((page, 0.0));
        }
    }

    fn push(&mut self, page: Page, scroll: f32) {
        if let Some(top) = self.0.last_mut() {
            top.1 = scroll;
        }
        self.0.push((page, 0.0));
    }

    /// Goes back a page, and gives how far it was scrolled. None at the first.
    fn pop(&mut self) -> Option<f32> {
        if self.at_root() {
            return None;
        }
        self.0.pop();
        self.0.last().map(|(_, scroll)| *scroll)
    }
}

#[derive(Default)]
struct SearchState {
    /// What the results are for.
    query: String,
    results: Option<SearchResults>,
    error: Option<String>,
    working: Option<Cancel>,
}

struct ChannelPage {
    info: ChannelInfo,
    videos: Vec<Video>,
    error: Option<String>,
    working: Option<Cancel>,
}

/// How a list's content arrives.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fill {
    /// Another page, or the same one anew: its list starts over.
    Page,
    /// More of what shows, changing the lines in place.
    InPlace,
    /// The same videos in other lines, for another width. Its list stays, and
    /// its cards go.
    Lines,
}

pub struct YouTubeController {
    session: Rc<Session>,
    window: PlayerWindow,
    player: Weak<PlayerController>,
    prefs: Box<dyn Fn() -> Rc<Preferences>>,
    this: Weak<YouTubeController>,
    pages: RefCell<Pages>,
    columns: Cell<usize>,
    /// How wide the large picture is decoded, in pixels: 0 while cards are small
    /// enough for the small one. It only grows, as nothing is gained by asking
    /// for smaller ones again.
    large: Cell<u32>,
    lines: Rc<VecModel<YouTubeLine>>,
    /// The lines in sight, as the view last said.
    shown: Cell<(usize, usize)>,
    /// Where each video and channel on the page is: `(line, column)`.
    places: RefCell<HashMap<String, (usize, usize)>>,
    /// Pictures asked for since the page showed, or since they last went.
    asked: RefCell<HashSet<String>>,
    /// Channels' pictures on the page, by channel.
    avatars: RefCell<HashMap<String, String>>,
    /// Every video on the page, by id: what saving or downloading one needs.
    videos: RefCell<HashMap<String, Video>>,
    /// Channels known from the page, which subscribing to one needs.
    known: RefCell<HashMap<String, ChannelInfo>>,
    generation: Generation,
    search: RefCell<SearchState>,
    channel: RefCell<Option<ChannelPage>>,
    adding: RefCell<Option<Cancel>>,
    tool: RefCell<Option<ToolState>>,
    /// Counts the times the section asked about its yt-dlp (`ask_tool`), so
    /// that an answer about a copy no longer chosen, as the package's slower
    /// one can give after the user's that replaced it, is dropped.
    tool_asked: Cell<u64>,
    /// Things to tell the user: how an import went, a list that can't be read.
    notes: RefCell<Vec<String>>,
    update_timer: Timer,
}

/// "3 hours ago". `seconds` is how long ago.
fn ago(seconds: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const MONTH: i64 = 30 * DAY;
    const YEAR: i64 = 365 * DAY;
    let count = |number: i64, unit: &str| {
        if number == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{number} {unit}s ago")
        }
    };
    if seconds < MINUTE {
        "just now".to_owned()
    } else if seconds < HOUR {
        count(seconds / MINUTE, "minute")
    } else if seconds < DAY {
        count(seconds / HOUR, "hour")
    } else if seconds < MONTH {
        count(seconds / DAY, "day")
    } else if seconds < YEAR {
        count(seconds / MONTH, "month")
    } else {
        count(seconds / YEAR, "year")
    }
}

/// When a video came out. An approximate date is only ever a day's, so it says
/// no more than that.
fn published(video: &Video, now: i64) -> Option<String> {
    let published = video.published?;
    let since = now - published.at;
    Some(if !published.exact && since < 24 * 3600 {
        "today".to_owned()
    } else {
        ago(since)
    })
}

/// "1.2M", "12K", "950".
fn short_count(count: u64) -> String {
    let scaled = |value: f64, unit: &str| {
        if value < 10.0 {
            format!("{:.1}", (value * 10.0).floor() / 10.0)
                .trim_end_matches(".0")
                .to_owned()
                + unit
        } else {
            format!("{}{unit}", value.floor())
        }
    };
    if count < 1_000 {
        count.to_string()
    } else if count < 1_000_000 {
        scaled(count as f64 / 1e3, "K")
    } else if count < 1_000_000_000 {
        scaled(count as f64 / 1e6, "M")
    } else {
        scaled(count as f64 / 1e9, "B")
    }
}

/// "12:34" or "1:02:03".
fn duration_text(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

fn import_note(report: &ImportReport) -> String {
    let list = match report.kind {
        ListKind::Subscriptions => "Channels",
        ListKind::WatchLater => "Watch Later",
    };
    let mut note = format!(
        "{list} imported: {} added, {} already there, {} skipped.",
        report.added, report.present, report.skipped
    );
    if report.kind == ListKind::WatchLater && report.added > 0 {
        note.push_str(" Google's file has no titles, so they are being fetched, slowly.");
    }
    note
}

/// One of the failures, and how many channels it stopped: the most common, or
/// the first of those.
fn worst_failure(failures: impl Iterator<Item = Failure>) -> Option<(Failure, usize)> {
    let mut counts: Vec<(Failure, usize)> = Vec::new();
    for failure in failures {
        match counts.iter_mut().find(|(kind, _)| *kind == failure) {
            Some((_, count)) => *count += 1,
            None => counts.push((failure, 1)),
        }
    }
    let most = counts.iter().map(|(_, count)| *count).max()?;
    counts.into_iter().find(|(_, count)| *count == most)
}

impl YouTubeController {
    pub fn new(
        session: &Rc<Session>,
        window: &PlayerWindow,
        player: Weak<PlayerController>,
        prefs: Box<dyn Fn() -> Rc<Preferences>>,
    ) -> Rc<Self> {
        let controller = Rc::new_cyclic(|this| Self {
            session: session.clone(),
            window: window.clone_strong(),
            player,
            prefs,
            this: this.clone(),
            pages: RefCell::new(Pages::default()),
            columns: Cell::new(1),
            large: Cell::new(0),
            lines: Rc::new(VecModel::default()),
            shown: Cell::new((0, 0)),
            places: RefCell::new(HashMap::new()),
            asked: RefCell::new(HashSet::new()),
            avatars: RefCell::new(HashMap::new()),
            videos: RefCell::new(HashMap::new()),
            known: RefCell::new(HashMap::new()),
            generation: Generation::default(),
            search: RefCell::new(SearchState::default()),
            channel: RefCell::new(None),
            adding: RefCell::new(None),
            tool: RefCell::new(None),
            tool_asked: Cell::new(0),
            notes: RefCell::new(Vec::new()),
            update_timer: Timer::default(),
        });
        controller
            .global()
            .set_lines(ModelRc::from(controller.lines.clone()));
        controller.wire();
        let listener: Weak<dyn Listener> = Rc::downgrade(&controller) as Weak<dyn Listener>;
        session.add_listener(listener);
        controller
    }

    fn global(&self) -> YouTube<'_> {
        self.window.global::<YouTube>()
    }

    fn wire(&self) {
        let youtube = self.global();
        macro_rules! on {
            ($method:ident, |$this:ident $(, $arg:ident)*| $body:expr) => {{
                let weak = self.this.clone();
                youtube.$method(move |$($arg),*| {
                    if let Some($this) = weak.upgrade() {
                        $body
                    }
                });
            }};
        }
        on!(on_layout, |this, columns, width| this
            .layout(usize::try_from(columns).unwrap_or(1), width));
        on!(on_lines_shown, |this, first, last| this.lines_shown(
            usize::try_from(first).unwrap_or(0),
            usize::try_from(last).unwrap_or(0)
        ));
        on!(on_navigate, |this, id| this.navigate(&id));
        on!(on_back, |this| this.back());
        on!(on_play, |this, id| this.play(&id));
        on!(on_play_from, |this, id| this.play_from(&id));
        on!(on_play_all, |this| this.play_all());
        on!(on_set_saved, |this, id, saved| this.set_saved(&id, saved));
        on!(on_download, |this, id| this.download(&id));
        on!(on_remove_download, |this, id| this.remove_download(&id));
        on!(on_set_watched, |this, id, watched| this
            .set_watched(&id, watched));
        on!(on_open_channel, |this, id| this.open_channel(&id));
        on!(on_set_subscribed, |this, id, subscribed| this
            .set_subscribed(&id, subscribed));
        on!(on_set_shorts, |this, show| this
            .session
            .set_show_shorts(show));
        on!(on_refresh, |this| this.session.refresh(&(this.prefs)()));
        on!(on_search, |this, text| this.search(&text));
        on!(on_add_channel, |this, text| this.add_channel(&text));
        on!(on_import, |this| this.import());
        on!(on_export, |this, list| this.export(&list));
        on!(on_retry, |this| this.retry());
        on!(on_dismiss_banner, |this| this.dismiss_banner());
    }

    fn top(&self) -> Page {
        self.pages.borrow().top()
    }

    pub fn is_shown(&self) -> bool {
        self.global().get_shown()
    }

    // Coming and going.

    /// Shows the section, from its first page, with what was saved. Whatever is
    /// due is fetched behind that.
    pub fn open(&self) {
        if self.is_shown() {
            return;
        }
        *self.pages.borrow_mut() = Pages::default();
        let youtube = self.global();
        youtube.set_scroll_offset(0.0);
        youtube.set_shown(true);
        self.session.entered();
        self.notes.borrow_mut().extend(self.session.take_notices());
        self.ask_tool();
        self.show_page();
        let prefs = (self.prefs)();
        self.session.section_opened(&prefs);
    }

    /// Back at the welcome screen, or the library this was opened over. What is
    /// on its way stops, if no other window shows the section.
    fn leave(&self) {
        self.stop_work();
        self.session.left();
        self.generation.bump();
        self.asked.borrow_mut().clear();
        self.lines.set_vec(Vec::new());
        self.global().set_shown(false);
    }

    fn stop_work(&self) {
        if let Some(cancel) = self.search.borrow_mut().working.take() {
            cancel.cancel();
        }
        if let Some(page) = &mut *self.channel.borrow_mut()
            && let Some(cancel) = page.working.take()
        {
            cancel.cancel();
        }
        if let Some(cancel) = self.adding.borrow_mut().take() {
            cancel.cancel();
        }
        self.global().set_adding(false);
    }

    /// Something else is about to show in this window: the library's sign-in,
    /// or the welcome screen from the title bar's Home.
    pub fn hide(&self) {
        if self.is_shown() {
            self.leave();
        }
    }

    /// The window is closing.
    pub fn closed(&self) {
        if self.is_shown() {
            self.stop_work();
            self.session.left();
        }
    }

    /// The window is idle again after playing: what played has moved progress
    /// on, and an update may be due by now.
    pub fn shown_again(&self) {
        if !self.is_shown() {
            return;
        }
        self.fill(Fill::InPlace);
        self.ask_tool();
        let prefs = (self.prefs)();
        self.session.section_opened(&prefs);
    }

    /// The preferences now name another yt-dlp, or none: what the section says
    /// of it is out of date, and it only looks when it opens otherwise.
    pub fn tool_changed(&self) {
        if self.is_shown() {
            self.ask_tool();
        }
    }

    fn ask_tool(&self) {
        let weak = self.this.clone();
        let asked = self.tool_asked.get() + 1;
        self.tool_asked.set(asked);
        self.session.tool_state(&(self.prefs)(), move |state| {
            if let Some(this) = weak.upgrade()
                && this.tool_asked.get() == asked
            {
                *this.tool.borrow_mut() = Some(state);
                this.update_notes();
            }
        });
    }

    // Pages.

    fn navigate(&self, id: &str) {
        let page = match id {
            "search" => Page::Search,
            "saved" => Page::Saved,
            "channels" => Page::Channels,
            _ => Page::Feed,
        };
        let scroll = self.global().get_scroll_offset();
        self.pages.borrow_mut().go_to(page, scroll);
        self.global().set_add_message(SharedString::new());
        self.show_page();
    }

    fn back(&self) {
        let scroll = self.pages.borrow_mut().pop();
        match scroll {
            Some(scroll) => {
                self.global().set_scroll_offset(scroll);
                self.show_page();
            }
            None => self.leave(),
        }
    }

    /// Fills the view for the page on top.
    fn show_page(&self) {
        let youtube = self.global();
        let page = self.top();
        {
            let pages = self.pages.borrow();
            youtube.set_page(page.name().into());
            youtube.set_nav_current(pages.nav_current().into());
            youtube.set_at_root(pages.at_root());
        }
        self.fill(Fill::Page);
        youtube.set_loads(youtube.get_loads() + 1);
    }

    /// Everything the page shows that isn't its list's own.
    fn update_page(&self) {
        let youtube = self.global();
        let page = self.top();
        youtube.set_show_shorts(self.session.show_shorts());
        youtube.set_refreshing(self.session.is_refreshing());
        let channels = self.session.channel_count();
        let saved = self.session.saved_count();
        let (mut loading, mut error) = (false, String::new());
        let empty;
        let (mut offers_channels, mut offers_import) = (false, false);
        match &page {
            Page::Feed => {
                youtube.set_status(self.status().into());
                if channels == 0 {
                    empty = "Add the channels you follow, or import them from a Google export, \
                             and their new videos show up here. There is no account: they are \
                             Patina's own list, and they don't appear on YouTube."
                        .to_owned();
                    offers_channels = true;
                    offers_import = true;
                } else if self.session.is_refreshing() {
                    empty = "Fetching your channels' videos…".to_owned();
                } else {
                    empty = "No videos from your channels yet.".to_owned();
                }
            }
            Page::Search => {
                let search = self.search.borrow();
                loading = search.working.is_some();
                error = search.error.clone().unwrap_or_default();
                empty = if let Some(results) = &search.results {
                    if results.videos.is_empty() || self.session.show_shorts() {
                        format!("Nothing found for “{}”.", search.query)
                    } else {
                        format!("Only Shorts found for “{}”.", search.query)
                    }
                } else {
                    "Search YouTube for videos and channels. Results come when you press Enter."
                        .to_owned()
                };
            }
            Page::Saved => {
                youtube.set_list_note(
                    format!(
                        "{}. Patina's own list: it doesn't appear on YouTube.",
                        plural(saved, "video", "videos")
                    )
                    .into(),
                );
                empty = "Videos you save show up here. Watch Later is Patina's own list, and \
                         it doesn't appear on YouTube. To bring yours, choose \
                         Watch later-videos.csv from a Google export; any playlist's file you \
                         choose joins this list."
                    .to_owned();
                offers_import = true;
            }
            Page::Channels => {
                youtube.set_list_note(
                    format!(
                        "{}. Patina's own list: they don't appear on YouTube.",
                        plural(channels, "channel", "channels")
                    )
                    .into(),
                );
                empty = "Paste a channel's link or @handle above, or import subscriptions.csv \
                         from a Google export. Channels are Patina's own list, and they don't \
                         appear on YouTube."
                    .to_owned();
                offers_import = true;
            }
            Page::Channel(_) => {
                if let Some(page) = &*self.channel.borrow() {
                    loading = page.working.is_some() && page.videos.is_empty();
                    error = page.error.clone().unwrap_or_default();
                }
                empty = "This channel has no videos to show.".to_owned();
            }
        }
        youtube.set_loading(loading);
        youtube.set_error(error.into());
        youtube.set_empty_text(empty.into());
        youtube.set_empty_offers_channels(offers_channels);
        youtube.set_empty_offers_import(offers_import);
        self.update_notes();
    }

    /// The feed's line: how far an update is, or when the last one was. Whether
    /// it reached every channel is for the message under the bar.
    fn status(&self) -> String {
        if self.session.is_refreshing() {
            let (done, total) = self.session.progress();
            return if total > 1 {
                format!("Checking your channels… {done} of {total}")
            } else {
                "Checking your channels…".to_owned()
            };
        }
        match self.session.last_refresh() {
            Some(at) if self.session.channel_count() > 0 => {
                format!("Checked {}", ago(crate::settings::now() as i64 - at))
            }
            _ => String::new(),
        }
    }

    /// The message under the bar, the first that applies: what the user should
    /// know of what they just did, then what stops everything, then how the
    /// last update went.
    fn update_notes(&self) {
        let youtube = self.global();
        // The newest first: it is about what was just done, and the rest wait
        // under it until it is dismissed.
        let newest_note = self.notes.borrow().last().cloned();
        let tool = self.tool.borrow().clone();
        let (banner, dismissable) = if let Some(note) = newest_note {
            (note, true)
        } else if let Some(ToolState::Unusable(error)) = &tool {
            (format!("{error} Your saved lists still show."), false)
        } else if let Some(problem) = self.update_problem() {
            (problem, false)
        } else if let Some(ToolState::Ready(version)) = tool
            .as_ref()
            .filter(|state| matches!(state, ToolState::Ready(version) if version.stale))
        {
            let age = version
                .age_days
                .map_or(String::new(), |days| format!(" is {days} days old and"));
            (
                format!(
                    "yt-dlp{age} looks out of date. YouTube changes often, so if videos stop \
                     working, updating it usually fixes them."
                ),
                false,
            )
        } else {
            (String::new(), false)
        };
        youtube.set_banner(banner.into());
        youtube.set_banner_dismissable(dismissable);
        let note = match &tool {
            Some(ToolState::Ready(version)) => match version.age_days {
                Some(days) => format!(
                    "yt-dlp {}, {} old.",
                    version.text,
                    plural(days.max(0) as usize, "day", "days")
                ),
                None => format!("yt-dlp {}.", version.text),
            },
            _ => String::new(),
        };
        youtube.set_tool_note(note.into());
    }

    /// How the last update went, named, when it didn't reach every channel. It
    /// stays up through the next update, or the page would move as that starts.
    fn update_problem(&self) -> Option<String> {
        let channels = self.session.channels();
        let failures = channels.iter().filter_map(|channel| {
            let state = self.session.channel_state(&channel.id)?;
            self.failure_now(&state).filter(|failure| {
                !matches!(
                    failure,
                    Failure::NoSuchChannel | Failure::ChannelUnavailable
                )
            })
        });
        let total: usize = failures.clone().count();
        let (failure, _) = worst_failure(failures)?;
        Some(format!(
            "{} {} of {} weren't updated, and show what was saved.",
            failure.message(),
            total,
            plural(channels.len(), "channel", "channels"),
        ))
    }

    /// What stopped a channel the last time, as far as it still holds. A yt-dlp
    /// that was missing and has been found since no longer is.
    fn failure_now(&self, state: &ChannelState) -> Option<Failure> {
        let ready = matches!(&*self.tool.borrow(), Some(ToolState::Ready(_)));
        state.failure.filter(|failure| match failure {
            Failure::Cancelled => false,
            Failure::Missing | Failure::Disabled => !ready,
            _ => true,
        })
    }

    // The lines.

    fn video_lines(&self, cards: Vec<YouTubeCard>) -> Vec<YouTubeLine> {
        cards
            .chunks(self.columns.get().max(1))
            .enumerate()
            .map(|(index, cards)| YouTubeLine {
                id: format!("videos-{index}").into(),
                kind: "videos".into(),
                cards: ModelRc::new(VecModel::from(cards.to_vec())),
                ..Default::default()
            })
            .collect()
    }

    fn heading(id: &str, title: &str) -> YouTubeLine {
        YouTubeLine {
            id: id.into(),
            kind: "heading".into(),
            title: title.into(),
            ..Default::default()
        }
    }

    fn channel_line(&self, channel: YouTubeChannel) -> YouTubeLine {
        YouTubeLine {
            id: format!("channel-{}", channel.id).into(),
            kind: "channel".into(),
            channel,
            ..Default::default()
        }
    }

    /// A video's card, as far as what is known of it says. `saved` is Watch
    /// Later's ids, made once for all of a page's cards: it can be long.
    fn card(
        &self,
        video: &Video,
        facts: Option<String>,
        now: i64,
        saved: &HashSet<VideoId>,
    ) -> YouTubeCard {
        let mark = self.session.mark(&video.id);
        let live = video.live_badge(now);
        let facts = facts.unwrap_or_else(|| {
            let when = match live {
                Some(Live::Live) => Some("Live now".to_owned()),
                Some(Live::Upcoming) => Some("Upcoming".to_owned()),
                None => published(video, now),
            };
            // A channel's feed says 0 for what it hasn't counted yet.
            let views = video
                .views
                .filter(|views| *views > 0)
                .map(|views| format!("{} views", short_count(views)));
            [when, views]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ")
        });
        YouTubeCard {
            id: video.id.as_str().into(),
            title: if video.title.is_empty() {
                "Untitled video".into()
            } else {
                video.title.as_str().into()
            },
            channel: video.channel.as_str().into(),
            channel_id: video
                .channel_id
                .as_ref()
                .map_or(SharedString::new(), |id| id.as_str().into()),
            facts: facts.into(),
            duration: video
                .duration
                .filter(|_| live.is_none())
                .map_or(SharedString::new(), |seconds| duration_text(seconds).into()),
            image: Image::default(),
            progress: mark
                .filter(|mark| mark.position > 0.0 && !mark.watched)
                .map_or(-1.0, |mark| mark.progress() as f32),
            watched: mark.is_some_and(|mark| mark.watched),
            live: match live {
                Some(Live::Live) => "live".into(),
                Some(Live::Upcoming) => "upcoming".into(),
                None => SharedString::new(),
            },
            shorts: video.shorts == Some(true),
            saved: saved.contains(&video.id),
            downloaded: self.session.download(&video.id).is_some(),
        }
    }

    /// Keeps what saving or subscribing needs of a video on show.
    fn keep_video(&self, video: &Video) {
        self.videos
            .borrow_mut()
            .insert(video.id.as_str().to_owned(), video.clone());
        if let Some(id) = &video.channel_id
            && !video.channel.is_empty()
        {
            self.known
                .borrow_mut()
                .entry(id.as_str().to_owned())
                .or_insert_with(|| {
                    let mut info = ChannelInfo::new(id.clone());
                    info.title = video.channel.clone();
                    info
                });
        }
    }

    fn cards_of(&self, videos: &[Video], saved: &HashSet<VideoId>) -> Vec<YouTubeCard> {
        let now = crate::settings::now() as i64;
        videos
            .iter()
            .map(|video| {
                self.keep_video(video);
                self.card(video, None, now, saved)
            })
            .collect()
    }

    /// A channel someone might subscribe to, from what said so.
    fn channel_of_info(&self, info: &ChannelInfo) -> YouTubeChannel {
        let followers = info
            .followers
            .map(|count| plural_count(count, "subscriber", "subscribers"));
        let detail = [info.handle.clone(), followers]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        if let Some(avatar) = &info.avatar {
            self.avatars
                .borrow_mut()
                .insert(info.id.as_str().to_owned(), avatar.clone());
        }
        YouTubeChannel {
            id: info.id.as_str().into(),
            title: if info.title.is_empty() {
                info.id.as_str().into()
            } else {
                info.title.as_str().into()
            },
            detail: detail.into(),
            subscribed: self.session.is_subscribed(&info.id),
            ..Default::default()
        }
    }

    /// A subscribed channel, with how its last update went.
    fn channel_of_list(&self, channel: &Channel, now: i64) -> YouTubeChannel {
        let mut info = ChannelInfo::new(channel.id.clone());
        info.title = channel.title.clone();
        info.handle = channel.handle.clone();
        info.avatar = channel.avatar.clone();
        info.followers = channel.followers;
        let mut row = self.channel_of_info(&info);
        let state = self.session.channel_state(&channel.id).unwrap_or_default();
        let updated = state.last_ok.map(|at| format!("Updated {}", ago(now - at)));
        row.state = updated
            .clone()
            .unwrap_or_else(|| "Not updated yet".to_owned())
            .into();
        if let Some(failure) = self.failure_now(&state) {
            let since =
                updated.map_or("Never updated.".to_owned(), |updated| format!("{updated}."));
            let said = Error::new(failure, state.detail.clone()).said();
            row.failure = format!("{said} {since}").into();
        }
        row
    }

    fn build_lines(&self) -> Vec<YouTubeLine> {
        self.videos.borrow_mut().clear();
        self.avatars.borrow_mut().clear();
        let now = crate::settings::now() as i64;
        let saved_ids = self.session.saved_ids();
        match self.top() {
            Page::Feed => self.video_lines(self.cards_of(&self.session.feed_videos(), &saved_ids)),
            Page::Search => {
                let shorts = self.session.show_shorts();
                let results = {
                    let search = self.search.borrow();
                    search.results.as_ref().map(|results| {
                        (
                            results
                                .videos
                                .iter()
                                .filter(|video| shorts || video.shorts != Some(true))
                                .cloned()
                                .collect::<Vec<_>>(),
                            results
                                .channels
                                .iter()
                                .take(SEARCH_CHANNELS)
                                .cloned()
                                .collect::<Vec<_>>(),
                        )
                    })
                };
                let Some((videos, channels)) = results else {
                    return Vec::new();
                };
                let mut lines = Vec::new();
                if !channels.is_empty() {
                    lines.push(Self::heading("heading-channels", "Channels"));
                    for info in &channels {
                        self.known
                            .borrow_mut()
                            .insert(info.id.as_str().to_owned(), info.clone());
                        lines.push(self.channel_line(self.channel_of_info(info)));
                    }
                    if !videos.is_empty() {
                        lines.push(Self::heading("heading-videos", "Videos"));
                    }
                }
                lines.extend(self.video_lines(self.cards_of(&videos, &saved_ids)));
                lines
            }
            Page::Saved => {
                let cards = self
                    .session
                    .saved()
                    .iter()
                    .map(|saved| {
                        let mut video = saved.video.clone();
                        let title_known = !video.title.is_empty();
                        if !title_known {
                            video.title = if saved.unavailable {
                                "This video isn't available".to_owned()
                            } else if self.session.is_fetching_title(saved) {
                                "Fetching its title…".to_owned()
                            } else {
                                "Untitled video".to_owned()
                            };
                        }
                        self.keep_video(&saved.video);
                        let added = format!("Added {}", ago(now - saved.added_at));
                        self.card(&video, Some(added), now, &saved_ids)
                    })
                    .collect();
                self.video_lines(cards)
            }
            Page::Channels => {
                let mut channels = self.session.channels();
                channels.sort_by_cached_key(|channel| {
                    (channel.title.to_lowercase(), channel.id.clone())
                });
                channels
                    .iter()
                    .map(|channel| self.channel_line(self.channel_of_list(channel, now)))
                    .collect()
            }
            Page::Channel(id) => {
                let videos = self
                    .channel
                    .borrow()
                    .as_ref()
                    .filter(|page| page.info.id == id)
                    .map_or_else(Vec::new, |page| page.videos.clone());
                self.video_lines(self.cards_of(&videos, &saved_ids))
            }
        }
    }

    /// Puts the page's lines in the view, and whatever else it shows.
    fn fill(&self, fill: Fill) {
        let new = self.build_lines();
        self.update_page();
        self.update_channel_header();
        if fill == Fill::Page {
            self.generation.bump();
            self.asked.borrow_mut().clear();
            self.places.borrow_mut().clear();
            self.lines.set_vec(new);
            // As many in sight as before, until the view says.
            let (first, last) = self.shown.get();
            self.shown.set((0, last - first));
        } else {
            self.replace_lines(new, fill);
        }
        self.index_lines();
        self.update_images();
    }

    /// Changes the lines to these, keeping the pictures they already have.
    fn replace_lines(&self, new: Vec<YouTubeLine>, fill: Fill) {
        let mut pictures: HashMap<SharedString, Image> = HashMap::new();
        for line in self.lines.iter() {
            for card in line.cards.iter() {
                if card.image.size().width > 0 {
                    pictures.insert(card.id.clone(), card.image.clone());
                }
            }
            if line.channel.image.size().width > 0 {
                pictures.insert(line.channel.id.clone(), line.channel.image.clone());
            }
        }
        let give = |line: &mut YouTubeLine| {
            if let Some(image) = pictures.get(&line.channel.id) {
                line.channel.image = image.clone();
            }
            for index in 0..line.cards.row_count() {
                if let Some(mut card) = line.cards.row_data(index)
                    && let Some(image) = pictures.get(&card.id)
                {
                    card.image = image.clone();
                    line.cards.set_row_data(index, card);
                }
            }
        };
        let old_length = self.lines.row_count();
        if fill == Fill::Lines {
            let mut new = new;
            new.iter_mut().for_each(&give);
            self.lines.set_vec(new);
            return;
        }
        let new_length = new.len();
        for (index, mut line) in new.into_iter().enumerate() {
            give(&mut line);
            match self.lines.row_data(index) {
                None => self.lines.push(line),
                Some(old) if same_shape(&old, &line) => {
                    if old.channel != line.channel || old.title != line.title {
                        self.lines.set_row_data(index, line);
                    } else {
                        for column in 0..line.cards.row_count() {
                            let card = line.cards.row_data(column);
                            if old.cards.row_data(column) != card
                                && let Some(card) = card
                            {
                                old.cards.set_row_data(column, card);
                            }
                        }
                    }
                }
                Some(_) => self.lines.set_row_data(index, line),
            }
        }
        for index in (new_length..old_length).rev() {
            self.lines.remove(index);
        }
    }

    /// Notes where each video and channel on the page is, for its picture.
    fn index_lines(&self) {
        let mut places = self.places.borrow_mut();
        places.clear();
        for (index, line) in self.lines.iter().enumerate() {
            if line.kind == "channel" {
                places.insert(line.channel.id.to_string(), (index, 0));
            }
            for (column, card) in line.cards.iter().enumerate() {
                places.insert(card.id.to_string(), (index, column));
            }
        }
    }

    fn update_channel_header(&self) {
        let youtube = self.global();
        let Page::Channel(id) = self.top() else {
            return;
        };
        let info = self
            .channel
            .borrow()
            .as_ref()
            .filter(|page| page.info.id == id)
            .map(|page| page.info.clone());
        let Some(info) = info else {
            return;
        };
        let mut row = self.channel_of_info(&info);
        let header = youtube.get_channel();
        if header.id == row.id {
            row.image = header.image;
        }
        let picture = (row.image.size().width == 0)
            .then(|| info.avatar.clone())
            .flatten();
        youtube.set_channel(row);
        if let Some(url) = picture {
            self.load_avatar(info.id.as_str(), &url);
        }
    }

    // Grids and pictures.

    fn pixels(&self, width: f32) -> u32 {
        (width * self.window.window().scale_factor())
            .round()
            .max(1.0) as u32
    }

    /// The view says how many cards fit a line, and how wide they are.
    fn layout(&self, columns: usize, width: f32) {
        let columns = columns.max(1);
        let pixels = self.pixels(width);
        let large = if pixels > LARGE_FROM_PIXELS {
            pixels.next_multiple_of(LARGE_STEP_PIXELS).min(LARGE_PIXELS)
        } else {
            0
        };
        let grew = large > self.large.get();
        if grew {
            self.large.set(large);
        }
        if columns != self.columns.replace(columns) {
            self.fill(Fill::Lines);
        } else if grew {
            self.asked.borrow_mut().clear();
            self.update_images();
        }
    }

    fn lines_shown(&self, first: usize, last: usize) {
        let shown = (first, last.max(first));
        if self.shown.replace(shown) != shown {
            self.update_images();
        }
    }

    /// Gives the lines near those in sight their pictures, and takes them from
    /// those further away.
    fn update_images(&self) {
        let (first, last) = self.shown.get();
        let margin = (last - first + 1) * IMAGE_SCREENS;
        let wanted = first.saturating_sub(margin)..last + margin + 1;
        let mut asks: Vec<(String, Option<String>)> = Vec::new();
        for (index, line) in self.lines.iter().enumerate() {
            let near = wanted.contains(&index);
            if line.kind == "channel" {
                let id = line.channel.id.to_string();
                if near {
                    if let Some(url) = self.avatars.borrow().get(&id)
                        && self.asked.borrow_mut().insert(id.clone())
                    {
                        asks.push((id, Some(url.clone())));
                    }
                } else if line.channel.image.size().width > 0 {
                    let mut line = line.clone();
                    line.channel.image = Image::default();
                    self.lines.set_row_data(index, line);
                    self.asked.borrow_mut().remove(&id);
                }
            }
            for (column, card) in line.cards.iter().enumerate() {
                let id = card.id.to_string();
                if near {
                    if self.asked.borrow_mut().insert(id.clone()) {
                        asks.push((id, None));
                    }
                } else if card.image.size().width > 0 {
                    let mut card = card.clone();
                    card.image = Image::default();
                    line.cards.set_row_data(column, card);
                    self.asked.borrow_mut().remove(&id);
                }
            }
        }
        for (id, avatar) in asks {
            match avatar {
                Some(url) => self.load_avatar(&id, &url),
                None => self.load_thumbnail(&id),
            }
        }
    }

    /// The small picture, which exists for every video and costs next to
    /// nothing, then the large one for a large card.
    fn load_thumbnail(&self, id: &str) {
        let Some(video) = VideoId::parse(id) else {
            return;
        };
        let weak = self.this.clone();
        let key = id.to_owned();
        let loaded = self.session.images(&(self.prefs)()).load(
            &self.generation,
            &video.thumbnail_url(),
            Some(SMALL_PIXELS),
            move |image| {
                if let Some(this) = weak.upgrade() {
                    this.thumbnail_arrived(&key, image);
                }
            },
        );
        if let Some(image) = loaded {
            self.thumbnail_arrived(id, image);
        }
    }

    fn thumbnail_arrived(&self, id: &str, image: Image) {
        self.set_card_image(id, image);
        let width = self.large.get();
        if width == 0 {
            return;
        }
        let Some(video) = VideoId::parse(id) else {
            return;
        };
        let weak = self.this.clone();
        let key = id.to_owned();
        let loaded = self.session.images(&(self.prefs)()).load(
            &self.generation,
            &video.large_thumbnail_url(),
            Some(width),
            move |image| {
                if let Some(this) = weak.upgrade() {
                    this.set_card_image(&key, image);
                }
            },
        );
        if let Some(image) = loaded {
            self.set_card_image(id, image);
        }
    }

    fn set_card_image(&self, id: &str, image: Image) {
        let place = self.places.borrow().get(id).copied();
        let Some((line, column)) = place else {
            return;
        };
        if let Some(line) = self.lines.row_data(line)
            && let Some(mut card) = line.cards.row_data(column)
            && card.id == id
        {
            card.image = image;
            line.cards.set_row_data(column, card);
        }
    }

    /// A channel's picture, in its line and at the head of its page. The address
    /// went through `client::is_image_url` when it was kept.
    fn load_avatar(&self, id: &str, url: &str) {
        let weak = self.this.clone();
        let key = id.to_owned();
        let pixels = self.pixels(AVATAR_WIDTH);
        let loaded = self.session.images(&(self.prefs)()).load(
            &self.generation,
            url,
            Some(pixels),
            move |image| {
                if let Some(this) = weak.upgrade() {
                    this.avatar_arrived(&key, image);
                }
            },
        );
        if let Some(image) = loaded {
            self.avatar_arrived(id, image);
        }
    }

    fn avatar_arrived(&self, id: &str, image: Image) {
        let youtube = self.global();
        let mut header = youtube.get_channel();
        if header.id == id {
            header.image = image.clone();
            youtube.set_channel(header);
        }
        let place = self.places.borrow().get(id).copied();
        if let Some((index, _)) = place
            && let Some(mut line) = self.lines.row_data(index)
            && line.channel.id == id
        {
            line.channel.image = image;
            self.lines.set_row_data(index, line);
        }
    }

    // What can be done with a video.

    /// A click on a card: this video, and then back here.
    fn play(&self, id: &str) {
        if let Some(video) = VideoId::parse(id) {
            self.play_videos(&[video], 0);
        }
    }

    /// The page's list in its order, from this video, with next and previous
    /// going through it.
    fn play_from(&self, id: &str) {
        let list = self.page_videos();
        if let Some(start) = list.iter().position(|video| video.as_str() == id) {
            self.play_videos(&list, start);
        }
    }

    fn play_all(&self) {
        self.play_videos(&self.page_videos(), 0);
    }

    /// The videos on the page, as they show.
    fn page_videos(&self) -> Vec<VideoId> {
        self.lines
            .iter()
            .flat_map(|line| line.cards.iter().collect::<Vec<_>>())
            .filter_map(|card| VideoId::parse(&card.id))
            .collect()
    }

    /// What goes to the player for a list played from `start`, each under the
    /// title it has on the page, and where `start` is among them.
    fn sources_of(&self, list: &[VideoId], start: usize) -> (Vec<String>, usize) {
        let (part, start) = playing::list_part(list.len(), start);
        let videos = self.videos.borrow();
        let sources = list[part]
            .iter()
            .map(|video| {
                let source = self.session.source_of(video);
                if let Some(known) = videos.get(video.as_str()) {
                    self.session
                        .name_source(source.clone(), known.title.clone());
                }
                source
            })
            .collect();
        (sources, start)
    }

    fn play_videos(&self, list: &[VideoId], start: usize) {
        let (sources, start) = self.sources_of(list, start);
        if sources.is_empty() {
            return;
        }
        // It would only fail further on, in fewer words. A downloaded video
        // needs none of this.
        if sources.iter().any(|source| crate::util::is_url(source))
            && let Err(error) = self.session.can_play(&(self.prefs)())
        {
            self.note(error.to_string());
            return;
        }
        let Some(player) = self.player.upgrade() else {
            return;
        };
        self.session.playing_started();
        player.play_from_section(sources, start);
    }

    fn set_saved(&self, id: &str, saved: bool) {
        let Some(video) = VideoId::parse(id) else {
            return;
        };
        if saved {
            let known = self.videos.borrow().get(id).cloned();
            self.session
                .save_video(known.unwrap_or_else(|| Video::new(video)));
        } else {
            self.session.unsave_video(&video);
        }
    }

    fn download(&self, id: &str) {
        let Some(video) = VideoId::parse(id) else {
            return;
        };
        let known = self.videos.borrow().get(id).cloned();
        // yt-dlp would record it for as long as it goes on.
        if known
            .as_ref()
            .is_some_and(|known| known.live_badge(crate::settings::now() as i64).is_some())
        {
            self.note("A live stream can be downloaded once it has ended.".to_owned());
            return;
        }
        if let Err(error) = self.session.can_play(&(self.prefs)()) {
            self.note(error.to_string());
            return;
        }
        let request = crate::downloads::Request {
            url: video.watch_url(),
            title: known.map(|known| known.title).unwrap_or_default(),
            format: self
                .player
                .upgrade()
                .map(|player| player.section_download_format()),
            video: Some(video),
        };
        App::with_current(move |app| {
            app.download(request);
        });
    }

    /// Sends a downloaded video's file to the trash, where it can be had back
    /// from if this was a slip.
    fn remove_download(&self, id: &str) {
        let Some(video) = VideoId::parse(id) else {
            return;
        };
        let title = self
            .videos
            .borrow()
            .get(id)
            .map(|known| known.title.clone())
            .unwrap_or_default();
        let weak = self.this.clone();
        self.session.trash_download(&video, move |answer| {
            let Some(this) = weak.upgrade() else {
                return;
            };
            match answer {
                Ok(()) => this.message("Moved to the Trash", &title),
                Err(why) => this.note(format!("Could not move the file to the trash: {why}")),
            }
        });
    }

    fn set_watched(&self, id: &str, watched: bool) {
        if let Some(video) = VideoId::parse(id) {
            self.session.set_watched(&video, watched);
        }
    }

    // Channels.

    fn open_channel(&self, id: &str) {
        let Some(channel) = ChannelId::parse(id) else {
            return;
        };
        if self.top() == Page::Channel(channel.clone()) {
            return;
        }
        let info = self.channel_info(&channel);
        // What is saved shows at once. A channel not followed has none.
        let videos: Vec<Video> = self
            .session
            .feed_videos()
            .into_iter()
            .filter(|video| video.channel_id.as_ref() == Some(&channel))
            .collect();
        *self.channel.borrow_mut() = Some(ChannelPage {
            info,
            videos,
            error: None,
            working: None,
        });
        let scroll = self.global().get_scroll_offset();
        self.pages
            .borrow_mut()
            .push(Page::Channel(channel.clone()), scroll);
        self.load_channel(&channel);
        self.show_page();
    }

    /// What is known of a channel: from the lists, or from the page it was
    /// reached from.
    fn channel_info(&self, id: &ChannelId) -> ChannelInfo {
        if let Some(channel) = self
            .session
            .channels()
            .into_iter()
            .find(|channel| channel.id == *id)
        {
            let mut info = ChannelInfo::new(channel.id);
            info.title = channel.title;
            info.handle = channel.handle;
            info.avatar = channel.avatar;
            info.followers = channel.followers;
            return info;
        }
        self.known
            .borrow()
            .get(id.as_str())
            .cloned()
            .unwrap_or_else(|| ChannelInfo::new(id.clone()))
    }

    fn load_channel(&self, id: &ChannelId) {
        if let Some(page) = &mut *self.channel.borrow_mut() {
            if let Some(cancel) = page.working.take() {
                cancel.cancel();
            }
            page.error = None;
            page.working = Some(Cancel::default());
        }
        let weak = self.this.clone();
        let asked = id.clone();
        let cancel = self
            .session
            .channel_page(&(self.prefs)(), id, move |answer| {
                if let Some(this) = weak.upgrade() {
                    this.channel_arrived(&asked, answer);
                }
            });
        if let Some(page) = &mut *self.channel.borrow_mut()
            && page.working.is_some()
        {
            page.working = Some(cancel);
        }
    }

    fn channel_arrived(&self, id: &ChannelId, answer: Result<super::Listing, Error>) {
        let now = crate::settings::now() as i64;
        {
            let mut page = self.channel.borrow_mut();
            let Some(page) = page.as_mut().filter(|page| page.info.id == *id) else {
                return;
            };
            page.working = None;
            match answer {
                Ok(listing) => {
                    if let Some(info) = listing.channel {
                        let mut info = info;
                        if info.title.is_empty() {
                            info.title = page.info.title.clone();
                        }
                        page.info = info;
                    }
                    // A listing's dates are approximate, and what was saved may
                    // know better.
                    let mut known: Vec<Video> = std::mem::take(&mut page.videos);
                    let mut videos = Vec::with_capacity(listing.videos.len());
                    let mut listed = listing.videos;
                    seen_now(&mut listed);
                    for video in listed {
                        match known.iter().position(|old| old.id == video.id) {
                            Some(index) => {
                                let mut old = known.remove(index);
                                old.absorb(video, now);
                                videos.push(old);
                            }
                            None => videos.push(video),
                        }
                    }
                    videos.append(&mut known);
                    page.videos = videos;
                }
                Err(error) => page.error = Some(error.said()),
            }
        }
        if self.top() == Page::Channel(id.clone()) {
            self.fill(Fill::InPlace);
        }
    }

    fn set_subscribed(&self, id: &str, subscribed: bool) {
        let Some(channel) = ChannelId::parse(id) else {
            return;
        };
        let info = self.channel_info(&channel);
        let title = info.title.clone();
        if subscribed {
            self.session.subscribe(&(self.prefs)(), info);
            self.message("Subscribed", &title);
        } else {
            self.session.unsubscribe(&channel);
            self.message("Unsubscribed", &title);
        }
    }

    fn add_channel(&self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let youtube = self.global();
        youtube.set_add_message(SharedString::new());
        youtube.set_adding(true);
        if let Some(cancel) = self.adding.borrow_mut().take() {
            cancel.cancel();
        }
        let weak = self.this.clone();
        let cancel = self
            .session
            .resolve_channel(&(self.prefs)(), text, move |answer| {
                if let Some(this) = weak.upgrade() {
                    this.resolved(answer);
                }
            });
        if self.global().get_adding() {
            *self.adding.borrow_mut() = Some(cancel);
        }
    }

    fn resolved(&self, answer: Result<ChannelInfo, Error>) {
        self.adding.borrow_mut().take();
        let youtube = self.global();
        youtube.set_adding(false);
        match answer {
            Ok(info) => {
                youtube.set_channel_text(SharedString::new());
                let title = info.title.clone();
                if self.session.is_subscribed(&info.id) {
                    youtube.set_add_message(format!("You follow {title} already.").into());
                } else {
                    self.session.subscribe(&(self.prefs)(), info);
                    self.message("Subscribed", &title);
                }
            }
            Err(error) => youtube.set_add_message(error.said().into()),
        }
    }

    // Search.

    fn search(&self, text: &str) {
        let query = text.trim().to_owned();
        if query.is_empty() {
            return;
        }
        let weak = self.this.clone();
        {
            let mut search = self.search.borrow_mut();
            if let Some(cancel) = search.working.take() {
                cancel.cancel();
            }
            search.error = None;
            search.results = None;
            search.query = query.clone();
            search.working = Some(Cancel::default());
        }
        self.fill(Fill::Page);
        self.bump_loads();
        let cancel = self.session.search(&(self.prefs)(), &query, move |answer| {
            if let Some(this) = weak.upgrade() {
                this.search_arrived(answer);
            }
        });
        let mut search = self.search.borrow_mut();
        if search.working.is_some() {
            search.working = Some(cancel);
        }
    }

    fn search_arrived(&self, answer: Result<SearchResults, Error>) {
        {
            let mut search = self.search.borrow_mut();
            search.working = None;
            match answer {
                Ok(mut results) => {
                    seen_now(&mut results.videos);
                    search.results = Some(results);
                }
                Err(error) => search.error = Some(error.said()),
            }
        }
        if self.top() == Page::Search {
            self.fill(Fill::Page);
            self.bump_loads();
        }
    }

    fn bump_loads(&self) {
        let youtube = self.global();
        youtube.set_loads(youtube.get_loads() + 1);
    }

    fn retry(&self) {
        match self.top() {
            Page::Search => {
                let query = self.search.borrow().query.clone();
                self.search(&query);
            }
            Page::Channel(id) => {
                self.load_channel(&id);
                self.fill(Fill::InPlace);
            }
            _ => {}
        }
    }

    // Files.

    /// Asks for a file from a Google export, and adds what it holds to the list
    /// its columns name.
    fn import(&self) {
        let Some(player) = self.player.upgrade() else {
            return;
        };
        let weak = self.this.clone();
        App::with_current(|app| {
            let dialog = app
                .dialog_for(Some(&player))
                .set_title("Import subscriptions.csv or Watch later-videos.csv")
                .add_filter("CSV files", &["csv"])
                .add_filter("All files", &["*"]);
            let spawned = slint::spawn_local(async move {
                let Some(file) = dialog.pick_file().await else {
                    return;
                };
                if let Some(this) = weak.upgrade() {
                    this.import_file(file.path());
                }
            });
            if let Err(error) = spawned {
                log::warn!("could not show the dialog: {error}");
            }
        });
    }

    fn import_file(&self, path: &Path) {
        let weak = self.this.clone();
        self.session.import(&(self.prefs)(), path, move |answer| {
            if let Some(this) = weak.upgrade() {
                match answer {
                    Ok(report) => this.note(import_note(&report)),
                    Err(error) => this.note(error.to_string()),
                }
            }
        });
    }

    /// Asks where to write a list, in the form Google's export has it.
    fn export(&self, list: &str) {
        let Some(player) = self.player.upgrade() else {
            return;
        };
        let (kind, name) = if list == "subscriptions" {
            (ListKind::Subscriptions, "subscriptions.csv")
        } else {
            (ListKind::WatchLater, "Watch later-videos.csv")
        };
        let weak = self.this.clone();
        App::with_current(|app| {
            let dialog = app
                .dialog_for(Some(&player))
                .set_title("Export")
                .set_file_name(name)
                .add_filter("CSV files", &["csv"]);
            let spawned = slint::spawn_local(async move {
                let Some(file) = dialog.save_file().await else {
                    return;
                };
                if let Some(this) = weak.upgrade() {
                    this.export_file(kind, file.path());
                }
            });
            if let Err(error) = spawned {
                log::warn!("could not show the dialog: {error}");
            }
        });
    }

    fn export_file(&self, kind: ListKind, path: &Path) {
        let weak = self.this.clone();
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.session.export(kind, path, move |answer| {
            if let Some(this) = weak.upgrade() {
                match answer {
                    Ok(()) => this.message("Exported", &name),
                    Err(error) => this.note(format!("Could not write the file: {error}")),
                }
            }
        });
    }

    // Messages.

    /// A message under the bar that stays until dismissed, for what was asked
    /// and didn't happen.
    pub fn note(&self, text: String) {
        {
            let mut notes = self.notes.borrow_mut();
            notes.retain(|note| *note != text);
            notes.push(text);
        }
        self.update_notes();
    }

    fn dismiss_banner(&self) {
        self.notes.borrow_mut().pop();
        self.update_notes();
    }

    /// A word over the section that something was done, long enough to read.
    fn message(&self, text: &str, detail: &str) {
        if let Some(player) = self.player.upgrade() {
            player.show_message(text, detail, NOTICE_SECONDS);
        }
    }

    /// Changes the lines soon, at most so often: an update's answers come one
    /// per channel.
    fn update_soon(&self) {
        if self.update_timer.running() {
            return;
        }
        let weak = self.this.clone();
        self.update_timer
            .start(TimerMode::SingleShot, UPDATE_EVERY, move || {
                if let Some(this) = weak.upgrade() {
                    this.fill(Fill::InPlace);
                }
            });
    }
}

/// Dates what an answer says is live or upcoming, which a badge goes by. These
/// answers aren't merged into anything that would.
fn seen_now(videos: &mut [Video]) {
    let now = crate::settings::now() as i64;
    for video in videos.iter_mut().filter(|video| video.live.is_some()) {
        video.live_seen = Some(now);
    }
}

/// Lines the view can change in place, rather than make again.
fn same_shape(old: &YouTubeLine, new: &YouTubeLine) -> bool {
    old.kind == new.kind && old.id == new.id && old.cards.row_count() == new.cards.row_count()
}

fn plural_count(count: u64, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{} {many}", short_count(count))
    }
}

impl Listener for YouTubeController {
    fn youtube_changed(&self, change: &Change) {
        // The player's menu says whether what plays is on the list, section or
        // no section.
        if *change == Change::Saved
            && let Some(player) = self.player.upgrade()
        {
            player.watch_later_changed();
        }
        if !self.is_shown() {
            return;
        }
        match change {
            Change::Refreshing { .. } => self.update_soon(),
            Change::RefreshStarted | Change::RefreshFinished => {
                self.update_timer.stop();
                self.fill(Fill::InPlace);
            }
            // Which videos show changes, and their order: the list starts over.
            Change::ShowShorts => {
                if matches!(self.top(), Page::Feed | Page::Search) {
                    self.global().set_scroll_offset(0.0);
                    self.fill(Fill::Page);
                    self.bump_loads();
                } else {
                    self.update_page();
                }
            }
            Change::Channels | Change::Saved | Change::Marks | Change::Downloads => {
                self.fill(Fill::InPlace);
            }
            Change::Notice => {
                let taken = self.session.take_notices();
                self.notes.borrow_mut().extend(taken);
                self.update_notes();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::feed::{Refused, Source};
    use super::*;
    use crate::settings::Paths;
    use std::sync::Arc;

    const A: &str = "UCaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "UCbbbbbbbbbbbbbbbbbbbbbb";

    struct Folder(std::path::PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("patina-youtube-view-{name}-{}", std::process::id()));
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

    /// A window with the section, in which yt-dlp can't be found and no picture
    /// can be fetched, so that nothing reaches the network.
    fn section(folder: &Folder) -> (PlayerWindow, Rc<Session>, Rc<YouTubeController>) {
        i_slint_backend_testing::init_no_event_loop();
        let window = PlayerWindow::new().unwrap();
        let paths = folder.paths();
        let session = Session::with_sources(
            &paths,
            Box::new(|_, _| {
                let refused = || Box::new(Refused(Failure::Missing.into())) as Box<dyn Source>;
                (refused(), refused())
            }),
        );
        session.set_pictures(Box::new(|_| {
            Arc::new(|_: &str| Err("no pictures".to_owned()))
        }));
        let prefs = Rc::new(Preferences {
            ytdl_path: folder.0.join("no-such-yt-dlp").display().to_string(),
            ..Default::default()
        });
        let controller = YouTubeController::new(
            &session,
            &window,
            Weak::new(),
            Box::new(move || prefs.clone()),
        );
        (window, session, controller)
    }

    fn video(id: &str, title: &str) -> Video {
        let mut video = Video::new(VideoId::parse(id).unwrap());
        video.title = title.to_owned();
        video
    }

    fn titles(controller: &YouTubeController) -> Vec<String> {
        controller
            .lines
            .iter()
            .flat_map(|line| {
                line.cards
                    .iter()
                    .map(|card| card.title.to_string())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    #[test]
    fn times_counts_and_lengths_read_as_they_should() {
        assert_eq!(ago(10), "just now");
        assert_eq!(ago(-5), "just now");
        assert_eq!(ago(60), "1 minute ago");
        assert_eq!(ago(3 * 3600), "3 hours ago");
        assert_eq!(ago(40 * 24 * 3600), "1 month ago");
        assert_eq!(ago(800 * 24 * 3600), "2 years ago");
        assert_eq!(short_count(950), "950");
        assert_eq!(short_count(1_000), "1K");
        assert_eq!(short_count(1_250), "1.2K");
        assert_eq!(short_count(12_900), "12K");
        assert_eq!(short_count(3_400_000), "3.4M");
        assert_eq!(duration_text(59), "0:59");
        assert_eq!(duration_text(754), "12:34");
        assert_eq!(duration_text(3723), "1:02:03");
    }

    #[test]
    fn an_approximate_date_says_no_more_than_a_day() {
        let mut video = video("aaaaaaaaaaa", "A");
        video.published = Some(super::super::Published {
            at: 1000,
            exact: false,
        });
        assert_eq!(published(&video, 1000 + 7200).unwrap(), "today");
        video.published = Some(super::super::Published {
            at: 1000,
            exact: true,
        });
        assert_eq!(published(&video, 1000 + 7200).unwrap(), "2 hours ago");
    }

    #[test]
    fn the_failure_most_channels_share_is_the_one_named() {
        use Failure::*;
        assert_eq!(worst_failure([].into_iter()), None);
        assert_eq!(
            worst_failure([Network, Blocked, Blocked].into_iter()),
            Some((Blocked, 2))
        );
        assert_eq!(
            worst_failure([Network, Blocked].into_iter()),
            Some((Network, 1))
        );
    }

    #[test]
    fn an_import_says_what_became_of_each_row() {
        let note = import_note(&ImportReport {
            kind: ListKind::Subscriptions,
            added: 3,
            present: 2,
            skipped: 1,
        });
        assert_eq!(
            note,
            "Channels imported: 3 added, 2 already there, 1 skipped."
        );
        let note = import_note(&ImportReport {
            kind: ListKind::WatchLater,
            added: 1,
            present: 0,
            skipped: 0,
        });
        assert!(note.contains("no titles"), "{note}");
    }

    #[test]
    fn back_goes_through_the_pages_then_leaves() {
        let folder = Folder::new("back");
        let (window, session, controller) = section(&folder);
        let youtube = window.global::<YouTube>();
        controller.open();
        assert!(youtube.get_shown());
        assert_eq!(youtube.get_page(), "feed");
        assert!(youtube.get_at_root());
        assert!(session.take_notices().is_empty());

        controller.navigate("channels");
        assert_eq!(youtube.get_page(), "channels");
        assert_eq!(youtube.get_nav_current(), "channels");
        assert!(!youtube.get_at_root());

        // A channel's page keeps the bar's entry it was reached from.
        controller.open_channel(A);
        assert_eq!(youtube.get_page(), "channel");
        assert_eq!(youtube.get_nav_current(), "channels");

        controller.back();
        assert_eq!(youtube.get_page(), "channels");
        controller.back();
        assert_eq!(youtube.get_page(), "feed");
        assert!(youtube.get_shown());
        controller.back();
        assert!(!youtube.get_shown());

        // Opened again, it starts from the first page.
        controller.navigate("saved");
        controller.open();
        assert_eq!(youtube.get_page(), "feed");
    }

    #[test]
    fn scroll_is_kept_with_the_page_left() {
        let folder = Folder::new("scroll");
        let (window, _session, controller) = section(&folder);
        let youtube = window.global::<YouTube>();
        controller.open();
        youtube.set_scroll_offset(300.0);
        controller.navigate("saved");
        youtube.set_scroll_offset(0.0);
        controller.back();
        assert_eq!(youtube.get_scroll_offset(), 300.0);
    }

    #[test]
    fn empty_pages_offer_a_way_in_and_say_the_lists_are_patinas() {
        let folder = Folder::new("empty");
        let (window, _session, controller) = section(&folder);
        let youtube = window.global::<YouTube>();
        controller.open();
        assert!(youtube.get_empty_offers_channels());
        assert!(youtube.get_empty_offers_import());
        assert!(youtube.get_empty_text().contains("don't appear on YouTube"));

        controller.navigate("saved");
        assert!(!youtube.get_empty_offers_channels());
        assert!(youtube.get_empty_offers_import());
        assert!(
            youtube
                .get_empty_text()
                .contains("doesn't appear on YouTube")
        );
        assert!(
            youtube
                .get_list_note()
                .contains("doesn't appear on YouTube")
        );

        controller.navigate("channels");
        assert!(youtube.get_list_note().contains("don't appear on YouTube"));

        controller.navigate("search");
        assert!(!youtube.get_empty_offers_import());
        assert!(youtube.get_empty_text().contains("press Enter"));
    }

    #[test]
    fn a_missing_ytdlp_is_said_and_the_saved_lists_still_show() {
        let folder = Folder::new("missing");
        let (window, session, controller) = section(&folder);
        let youtube = window.global::<YouTube>();
        session.save_video(video("aaaaaaaaaaa", "Saved Before"));
        controller.open();
        assert!(youtube.get_banner().contains("yt-dlp was not found"));
        assert!(!youtube.get_banner_dismissable());
        controller.navigate("saved");
        assert_eq!(titles(&controller), ["Saved Before"]);
    }

    #[test]
    fn saving_and_marking_change_the_card_in_place() {
        let folder = Folder::new("marks");
        let (_window, session, controller) = section(&folder);
        session.save_video(video("aaaaaaaaaaa", "One"));
        session.save_video(video("bbbbbbbbbbb", "Two"));
        controller.open();
        controller.navigate("saved");
        let line = controller.lines.row_data(0).unwrap();
        let card = line.cards.row_data(0).unwrap();
        assert!(card.saved);
        assert!(!card.watched);
        assert_eq!(card.progress, -1.0);

        // Whichever is first, it is the same model that changes.
        controller.set_watched(card.id.as_str(), true);
        let same = controller.lines.row_data(0).unwrap();
        assert!(same.cards.row_data(0).unwrap().watched);

        // Taking one off leaves fewer.
        controller.layout(1, 240.0);
        controller.set_saved(card.id.as_str(), false);
        assert_eq!(titles(&controller).len(), 1);
    }

    /// A row says a title is on its way only while it is: not with no yt-dlp,
    /// not once a block has stopped the fetching, and not after its last try.
    #[test]
    fn a_row_says_it_is_fetching_only_while_it_is() {
        let folder = Folder::new("fetching");
        let (_window, session, controller) = section(&folder);
        session.save_video(Video::new(VideoId::parse("aaaaaaaaaaa").unwrap()));
        controller.open();
        controller.navigate("saved");
        assert_eq!(titles(&controller), ["Untitled video"]);
        session.begin_details_for_a_test();
        assert_eq!(titles(&controller), ["Fetching its title…"]);
        session.use_up_tries_for_a_test(&VideoId::parse("aaaaaaaaaaa").unwrap());
        assert_eq!(titles(&controller), ["Untitled video"]);
    }

    fn short_and_video() -> SearchResults {
        let mut short = video("shortshort0", "A Short");
        short.shorts = Some(true);
        SearchResults {
            videos: vec![short, video("aaaaaaaaaaa", "A video")],
            channels: Vec::new(),
        }
    }

    #[test]
    fn search_leaves_out_shorts_until_asked() {
        let folder = Folder::new("search-shorts");
        let (window, session, controller) = section(&folder);
        let youtube = window.global::<YouTube>();
        controller.open();
        controller.search.borrow_mut().results = Some(short_and_video());
        controller.navigate("search");
        assert_eq!(titles(&controller), ["A video"]);
        assert_eq!(
            controller.page_videos().len(),
            1,
            "and so does a list played"
        );
        let loads = youtube.get_loads();
        session.set_show_shorts(true);
        assert_eq!(titles(&controller), ["A Short", "A video"]);
        assert!(youtube.get_show_shorts());
        assert_eq!(youtube.get_loads(), loads + 1, "the list starts over");
        session.set_show_shorts(false);
        assert_eq!(titles(&controller), ["A video"]);
    }

    #[test]
    fn a_search_that_found_only_shorts_says_so() {
        let folder = Folder::new("search-only-shorts");
        let (window, _session, controller) = section(&folder);
        controller.open();
        let mut results = short_and_video();
        results.videos.truncate(1);
        controller.search.borrow_mut().query = "words".to_owned();
        controller.search.borrow_mut().results = Some(results);
        controller.navigate("search");
        assert!(titles(&controller).is_empty());
        assert_eq!(
            window.global::<YouTube>().get_empty_text(),
            "Only Shorts found for “words”."
        );
    }

    /// The latest import's counts show though an earlier one's were never
    /// dismissed, and that one is still there under it.
    #[test]
    fn the_newest_message_shows_first() {
        let folder = Folder::new("notes");
        let (window, _session, controller) = section(&folder);
        let youtube = window.global::<YouTube>();
        controller.open();
        controller.note("Channels imported".to_owned());
        controller.note("Watch Later imported".to_owned());
        assert_eq!(youtube.get_banner(), "Watch Later imported");
        controller.dismiss_banner();
        assert_eq!(youtube.get_banner(), "Channels imported");
        // Said again, it comes to the front rather than staying where it was.
        controller.note("Other".to_owned());
        controller.note("Channels imported".to_owned());
        assert_eq!(youtube.get_banner(), "Channels imported");
        controller.dismiss_banner();
        assert_eq!(youtube.get_banner(), "Other");
    }

    /// A followed channel's page shows saved videos at once, so the failure
    /// to get newer ones has to reach it beside them.
    #[test]
    fn a_channels_page_keeps_its_failure_beside_saved_videos() {
        let folder = Folder::new("channel-failure");
        let (window, session, controller) = section(&folder);
        let youtube = window.global::<YouTube>();
        let id = ChannelId::parse(A).unwrap();
        session.store_channel_for_a_test(&id);
        session.feed_video_for_a_test(&id, video("aaaaaaaaaaa", "Saved"));
        controller.open();
        controller.open_channel(A);
        // yt-dlp can't be found here.
        assert_eq!(titles(&controller), ["Saved"]);
        assert!(youtube.get_error().contains("yt-dlp was not found"));
    }

    /// Found since, a yt-dlp that was missing no longer stands against the
    /// channels it stopped, or under the bar.
    #[test]
    fn a_failure_that_no_longer_holds_is_not_said() {
        let folder = Folder::new("tool-found");
        let (window, session, controller) = section(&folder);
        let youtube = window.global::<YouTube>();
        session.subscribe(
            &Preferences {
                ytdl_path: folder.0.join("no-such-yt-dlp").display().to_string(),
                ..Default::default()
            },
            ChannelInfo::new(ChannelId::parse(A).unwrap()),
        );
        controller.open();
        assert!(youtube.get_banner().contains("yt-dlp was not found"));
        controller.navigate("channels");
        let row = controller.lines.row_data(0).unwrap().channel;
        assert!(row.failure.contains("yt-dlp was not found"), "{row:?}");
        *controller.tool.borrow_mut() = Some(ToolState::Ready(super::super::ytdlp::Version {
            text: "2026.10.26".to_owned(),
            age_days: Some(7),
            stale: false,
        }));
        controller.fill(Fill::InPlace);
        assert_eq!(youtube.get_banner(), "");
        let row = controller.lines.row_data(0).unwrap().channel;
        assert_eq!(row.failure, "", "{row:?}");
    }

    /// What was said in the failure's own words shows when its name is only the
    /// catch-all.
    #[test]
    fn a_channels_row_says_what_went_wrong_in_those_words() {
        let folder = Folder::new("said");
        let (_window, session, controller) = section(&folder);
        let id = ChannelId::parse(A).unwrap();
        session.store_channel_for_a_test(&id);
        controller.open();
        controller.navigate("channels");
        session.channel_failed_for_a_test(
            &id,
            Error::new(Failure::Failed, "This channel does not have a videos tab"),
        );
        controller.fill(Fill::InPlace);
        let row = controller.lines.row_data(0).unwrap().channel;
        assert!(
            row.failure
                .starts_with("This channel does not have a videos tab"),
            "{row:?}"
        );
        session.channel_failed_for_a_test(&id, Failure::ChannelUnavailable.into());
        controller.fill(Fill::InPlace);
        let row = controller.lines.row_data(0).unwrap().channel;
        assert!(
            row.failure.contains("this channel isn't available"),
            "{row:?}"
        );
    }

    /// The preferences are another window: what is said of yt-dlp follows them.
    #[cfg(unix)]
    #[test]
    fn what_is_said_of_yt_dlp_follows_the_preferences() {
        let folder = Folder::new("tool-prefs");
        i_slint_backend_testing::init_no_event_loop();
        let window = PlayerWindow::new().unwrap();
        let session = Session::with_sources(
            &folder.paths(),
            Box::new(|_, _| {
                let refused = || Box::new(Refused(Failure::Missing.into())) as Box<dyn Source>;
                (refused(), refused())
            }),
        );
        session.set_pictures(Box::new(|_| {
            Arc::new(|_: &str| Err("no pictures".to_owned()))
        }));
        let prefs = Rc::new(RefCell::new(Rc::new(Preferences {
            ytdl_path: folder.0.join("no-such-yt-dlp").display().to_string(),
            ..Default::default()
        })));
        let reading = prefs.clone();
        let controller = YouTubeController::new(
            &session,
            &window,
            Weak::new(),
            Box::new(move || reading.borrow().clone()),
        );
        let youtube = window.global::<YouTube>();
        controller.open();
        assert!(youtube.get_banner().contains("yt-dlp was not found"));
        let fake =
            super::super::ytdlp::testing::Fake::new(&folder.0.join("bin"), "echo 2026.10.26");
        *prefs.borrow_mut() = Rc::new(Preferences {
            ytdl_path: fake.program.display().to_string(),
            ..Default::default()
        });
        controller.tool_changed();
        assert!(!youtube.get_banner().contains("yt-dlp was not found"));
        assert!(youtube.get_tool_note().contains("2026.10.26"));
        // Not shown, it has no reason to run anything.
        controller.back();
        let runs = fake.runs().len();
        controller.tool_changed();
        assert_eq!(fake.runs().len(), runs);
    }

    /// One lookup per card, however long Watch Later is.
    #[test]
    fn cards_know_whether_they_are_saved() {
        let folder = Folder::new("saved-set");
        let (_window, session, controller) = section(&folder);
        let id = ChannelId::parse(A).unwrap();
        session.store_channel_for_a_test(&id);
        session.feed_video_for_a_test(&id, video("aaaaaaaaaaa", "On it"));
        session.feed_video_for_a_test(&id, video("bbbbbbbbbbb", "Off it"));
        session.save_video(video("aaaaaaaaaaa", "On it"));
        controller.open();
        assert!(card_of(&controller, "aaaaaaaaaaa").saved);
        assert!(!card_of(&controller, "bbbbbbbbbbb").saved);
        session.save_video(video("bbbbbbbbbbb", "Off it"));
        assert!(card_of(&controller, "bbbbbbbbbbb").saved);
    }

    #[test]
    fn cards_go_in_as_many_columns_as_fit() {
        let folder = Folder::new("columns");
        let (_window, session, controller) = section(&folder);
        for (id, title) in [
            ("aaaaaaaaaaa", "1"),
            ("bbbbbbbbbbb", "2"),
            ("ccccccccccc", "3"),
        ] {
            session.save_video(video(id, title));
        }
        controller.open();
        controller.navigate("saved");
        assert_eq!(controller.lines.row_count(), 3);
        controller.layout(2, 240.0);
        assert_eq!(controller.lines.row_count(), 2);
        assert_eq!(titles(&controller).len(), 3);
        controller.layout(5, 240.0);
        assert_eq!(controller.lines.row_count(), 1);
    }

    #[test]
    fn a_saved_video_without_a_title_says_so_and_can_still_be_acted_on() {
        let folder = Folder::new("titles");
        let (_window, session, controller) = section(&folder);
        session.save_video(Video::new(VideoId::parse("-aaaaaaaaaa").unwrap()));
        controller.open();
        controller.navigate("saved");
        // yt-dlp can't be found here, so nothing is fetching it.
        assert_eq!(titles(&controller), ["Untitled video"]);
        // Keeping it again doesn't save the placeholder as its title.
        controller.set_saved("-aaaaaaaaaa", false);
        controller.set_saved("-aaaaaaaaaa", true);
        assert_eq!(session.saved()[0].video.title, "");
    }

    fn card_of(controller: &YouTubeController, id: &str) -> YouTubeCard {
        controller
            .lines
            .iter()
            .flat_map(|line| line.cards.iter().collect::<Vec<_>>())
            .find(|card| card.id == id)
            .unwrap()
    }

    fn seen(id: &str, position: f64, duration: f64, played: f64) -> playing::Seen {
        playing::Seen {
            id: VideoId::parse(id).unwrap(),
            position,
            duration,
            played,
        }
    }

    #[test]
    fn what_was_seen_shows_on_the_card_and_clearing_takes_it_off() {
        let folder = Folder::new("seen");
        let (_window, session, controller) = section(&folder);
        let prefs = Preferences::default();
        session.save_video(video("aaaaaaaaaaa", "One"));
        session.save_video(video("bbbbbbbbbbb", "Two"));
        controller.open();
        controller.navigate("saved");
        session.note_watch(&prefs, &seen("aaaaaaaaaaa", 25.0, 100.0, 25.0));
        let card = card_of(&controller, "aaaaaaaaaaa");
        assert_eq!(card.progress, 0.25);
        assert!(!card.watched);
        // Sought to the end, and not seen.
        session.note_watch(&prefs, &seen("bbbbbbbbbbb", 99.0, 100.0, 2.0));
        assert!(!card_of(&controller, "bbbbbbbbbbb").watched);
        // Seen.
        session.note_watch(&prefs, &seen("aaaaaaaaaaa", 95.0, 100.0, 70.0));
        let card = card_of(&controller, "aaaaaaaaaaa");
        assert!(card.watched);
        assert_eq!(card.progress, -1.0);
        // Clear Recent goes this way.
        session.clear_marks();
        let card = card_of(&controller, "aaaaaaaaaaa");
        assert!(!card.watched);
        assert_eq!(card.progress, -1.0);
        assert_eq!(card_of(&controller, "bbbbbbbbbbb").progress, -1.0);
    }

    #[test]
    fn a_list_plays_in_the_pages_order_under_its_titles() {
        let folder = Folder::new("list");
        let (_window, session, controller) = section(&folder);
        // Most recently added first: this one shows last.
        let file = folder.0.join("Two [bbbbbbbbbbb].mp4");
        std::fs::write(&file, b"video").unwrap();
        session.set_download(&VideoId::parse("bbbbbbbbbbb").unwrap(), &file);
        for (id, title) in [("ccccccccccc", "Three"), ("bbbbbbbbbbb", "Two")] {
            session.save_video(video(id, title));
        }
        session.save_video(Video::new(VideoId::parse("-aaaaaaaaaa").unwrap()));
        controller.open();
        controller.navigate("saved");
        controller.layout(2, 240.0);
        let list = controller.page_videos();
        assert_eq!(list.len(), 3);
        let at = list
            .iter()
            .position(|video| video.as_str() == "bbbbbbbbbbb")
            .unwrap();
        let (sources, start) = controller.sources_of(&list, at);
        assert_eq!(start, at);
        assert_eq!(sources.len(), 3);
        // What it is called while it opens, and in the player's playlist.
        let url = VideoId::parse("ccccccccccc").unwrap().watch_url();
        assert!(sources.contains(&url));
        assert_eq!(session.title_of(&url).as_deref(), Some("Three"));
        // A downloaded video plays from its file.
        assert_eq!(sources[at], file.to_str().unwrap());
        assert_eq!(session.title_of(&sources[at]).as_deref(), Some("Two"));
        // One with no title yet goes under no made-up name.
        let untitled = VideoId::parse("-aaaaaaaaaa").unwrap().watch_url();
        assert_eq!(session.title_of(&untitled), None);
    }

    #[test]
    fn a_video_that_cannot_play_says_why_at_once() {
        let folder = Folder::new("cannot");
        let (window, session, controller) = section(&folder);
        let youtube = window.global::<YouTube>();
        session.save_video(video("aaaaaaaaaaa", "One"));
        controller.open();
        controller.navigate("saved");
        controller.note("x".to_owned());
        controller.dismiss_banner();
        controller.play("aaaaaaaaaaa");
        assert!(youtube.get_banner().contains("yt-dlp was not found"));
        assert!(youtube.get_banner_dismissable());
        // Nor does this come to anything for an id it didn't make.
        controller.play("../etc/passwd");
        controller.play_from("not-on-the-page");
    }

    #[test]
    fn the_file_of_a_removed_download_says_when_it_stays() {
        let folder = Folder::new("remove");
        let (window, session, controller) = section(&folder);
        let youtube = window.global::<YouTube>();
        let id = VideoId::parse("aaaaaaaaaaa").unwrap();
        let file = folder.0.join("One.mp4");
        std::fs::write(&file, b"video").unwrap();
        session.set_download(&id, &file);
        session.save_video(video("aaaaaaaaaaa", "One"));
        controller.open();
        controller.navigate("saved");
        assert!(card_of(&controller, "aaaaaaaaaaa").downloaded);
        session.set_trash(Box::new(|paths, done| {
            done(
                paths
                    .into_iter()
                    .map(|path| (path, "it won't".to_owned()))
                    .collect(),
            );
        }));
        controller.remove_download("aaaaaaaaaaa");
        assert_eq!(
            youtube.get_banner(),
            "Could not move the file to the trash: it won't"
        );
        assert!(card_of(&controller, "aaaaaaaaaaa").downloaded);
        session.set_trash(Box::new(|_, done| done(Vec::new())));
        std::fs::remove_file(&file).unwrap();
        controller.remove_download("aaaaaaaaaaa");
        assert!(!card_of(&controller, "aaaaaaaaaaa").downloaded);
    }

    #[test]
    fn a_download_that_cannot_run_says_why_at_once() {
        let folder = Folder::new("no-tool");
        let (window, session, controller) = section(&folder);
        session.save_video(video("aaaaaaaaaaa", "One"));
        controller.open();
        controller.navigate("saved");
        controller.dismiss_banner();
        controller.note("first".to_owned());
        controller.dismiss_banner();
        controller.download("aaaaaaaaaaa");
        let banner = window.global::<YouTube>().get_banner();
        assert!(banner.contains("yt-dlp was not found"), "{banner}");
        assert!(window.global::<YouTube>().get_banner_dismissable());
    }

    #[test]
    fn a_live_stream_is_not_downloaded() {
        let folder = Folder::new("live");
        let (window, session, controller) = section(&folder);
        let mut live = video("aaaaaaaaaaa", "On Air");
        live.live = Some(Live::Live);
        live.live_seen = Some(crate::settings::now() as i64);
        session.save_video(live);
        controller.open();
        controller.navigate("saved");
        controller.download("aaaaaaaaaaa");
        assert!(
            window
                .global::<YouTube>()
                .get_banner()
                .contains("once it has ended")
        );
    }

    #[test]
    fn channels_say_how_their_last_update_went() {
        let folder = Folder::new("channels");
        let (_window, session, controller) = section(&folder);
        let prefs = Preferences {
            ytdl_path: folder.0.join("no-such-yt-dlp").display().to_string(),
            ..Default::default()
        };
        let mut first = ChannelInfo::new(ChannelId::parse(B).unwrap());
        first.title = "zebra".to_owned();
        first.handle = Some("@zebra".to_owned());
        first.followers = Some(1_500);
        session.subscribe(&prefs, first);
        let mut second = ChannelInfo::new(ChannelId::parse(A).unwrap());
        second.title = "Aardvark".to_owned();
        session.subscribe(&prefs, second);
        controller.open();
        controller.navigate("channels");
        let rows: Vec<YouTubeChannel> = controller.lines.iter().map(|line| line.channel).collect();
        assert_eq!(rows[0].title, "Aardvark");
        assert_eq!(rows[1].title, "zebra");
        assert_eq!(rows[1].detail, "@zebra · 1.5K subscribers");
        assert!(rows[1].subscribed);
        // yt-dlp can't be found, which names itself on the row.
        assert!(rows[0].failure.contains("yt-dlp was not found"), "{rows:?}");
        assert!(rows[0].failure.contains("Never updated"), "{rows:?}");
    }

    #[test]
    fn a_bad_link_says_so_where_it_was_typed() {
        let folder = Folder::new("add");
        let (window, session, controller) = section(&folder);
        let youtube = window.global::<YouTube>();
        controller.open();
        controller.navigate("channels");
        controller.add_channel("https://example.com/nothing");
        assert_eq!(
            youtube.get_add_message(),
            "That isn't a channel link or an @handle."
        );
        assert!(!youtube.get_adding());
        assert!(session.channels().is_empty());
        // Nothing to find, nothing asked.
        controller.add_channel("   ");
        assert_eq!(
            youtube.get_add_message(),
            "That isn't a channel link or an @handle."
        );
    }

    #[test]
    fn an_import_from_the_section_reports_its_counts() {
        let folder = Folder::new("import");
        let (window, session, controller) = section(&folder);
        let youtube = window.global::<YouTube>();
        controller.open();
        let file = folder.0.join("list.csv");
        std::fs::write(
            &file,
            format!(
                "Channel Id,Channel Url,Channel Title\n\
                 {A},http://www.youtube.com/channel/{A},\"Aard, the\"\n\
                 nonsense,x,y\n"
            ),
        )
        .unwrap();
        controller.import_file(&file);
        assert_eq!(session.channels().len(), 1);
        assert_eq!(
            youtube.get_banner(),
            "Channels imported: 1 added, 0 already there, 1 skipped."
        );
        assert!(youtube.get_banner_dismissable());
        controller.dismiss_banner();
        assert!(youtube.get_banner().contains("yt-dlp was not found"));
    }

    #[test]
    fn a_search_failure_names_itself_and_can_be_tried_again() {
        let folder = Folder::new("search");
        let (window, _session, controller) = section(&folder);
        let youtube = window.global::<YouTube>();
        controller.open();
        controller.navigate("search");
        controller.search("  ");
        assert!(controller.search.borrow().query.is_empty());
        controller.search("anything");
        assert!(youtube.get_error().contains("yt-dlp was not found"));
        assert!(!youtube.get_loading());
        controller.retry();
        assert!(youtube.get_error().contains("yt-dlp was not found"));
    }

    #[test]
    fn one_window_leaving_leaves_another_showing() {
        let folder = Folder::new("leave");
        let (_window, session, controller) = section(&folder);
        let second_window = PlayerWindow::new().unwrap();
        let second = YouTubeController::new(
            &session,
            &second_window,
            Weak::new(),
            Box::new(|| Rc::new(Preferences::default())),
        );
        controller.open();
        // Not opened through `open`, which would run yt-dlp as found on PATH.
        session.entered();
        second.global().set_shown(true);
        controller.back();
        assert!(second.global().get_shown());
        assert!(!controller.global().get_shown());
    }

    #[test]
    fn a_changed_list_shows_in_another_window() {
        let folder = Folder::new("windows");
        let (_window, session, controller) = section(&folder);
        controller.open();
        controller.navigate("saved");
        assert!(titles(&controller).is_empty());
        session.save_video(video("aaaaaaaaaaa", "From Elsewhere"));
        assert_eq!(titles(&controller), ["From Elsewhere"]);
    }
}
