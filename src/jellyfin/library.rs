//! The library view's controller, one per player window. It fills the
//! window's `Library` global (ui/library.slint) and answers its callbacks:
//! the pages and their navigation, the rows and grids of cards, an item's
//! page, search, signing in, and opening what the user plays in this
//! window.
//!
//! Pages stack, Back popping them, and each page is loaded afresh when it
//! shows. Every load runs on one of the session's load threads
//! (`session::load`) and comes back through the event loop with the page's
//! ticket: a load whose page the user has left by its turn asks nothing,
//! and an answer for such a page is dropped, as are the images it asked for
//! (the page's `Generation`). Models are kept and changed in place, so that
//! an image arriving, a played mark or another page of a grid changes one
//! row rather than everything the view draws.

use super::api::{Client, Error, ItemsQuery, SortOrder};
use super::images::{self, Generation};
use super::models::*;
use super::session::{
    self, Account, Change, ItemInfo, Listener, Session, SignIn, SignInEvent, SignInState,
    background,
};
use super::store::Server;
use crate::window::PlayerController;
use crate::{
    Library, LibraryCard, LibraryChoice, LibraryDetail, LibraryNav, LibraryPerson, LibraryRow,
    LibrarySignIn, LibraryTrack, PlayerWindow,
};
use slint::{ComponentHandle, Image, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Items a grid asks for at a time; it asks for more as it scrolls.
const GRID_PAGE: u32 = 100;
/// Cards in one of Home's rows, an item page's rows, and search's.
const ROW_LIMIT: u32 = 16;
const SEARCH_LIMIT: u32 = 24;
/// Typing pauses this long before a search goes out.
const SEARCH_DELAY: Duration = Duration::from_millis(300);
/// What a grid splits into until the view says how many fit.
const DEFAULT_COLUMNS: usize = 6;
/// The most entries playing a show, a season, an album, a playlist, a
/// collection, an artist or a library puts in the playlist.
const MAX_PLAYLIST: u32 = 1000;
/// The most songs an album's or playlist's page lists, and items a
/// collection's or albums an artist's page shows: their rows are made all
/// at once, and thousands of them held the interface up for seconds.
/// Playing the page still takes up to `MAX_PLAYLIST`.
const PAGE_LIMIT: u32 = 300;
/// The grid's lines this many screens above and below those in sight keep
/// their images; the rest give them up, to be asked again (from the cache,
/// mostly) when they come back.
const GRID_IMAGE_SCREENS: usize = 2;
/// How long the library's messages stay up: they explain something.
const NOTICE_SECONDS: f32 = 6.0;

/// The widths images are shown at, in logical pixels; they are asked for
/// at these times the window's scale.
const POSTER_WIDTH: f32 = 180.0;
const THUMB_WIDTH: f32 = 320.0;
const DETAIL_POSTER_WIDTH: f32 = 300.0;
const BACKDROP_WIDTH: f32 = 960.0;
const LOGO_WIDTH: f32 = 360.0;
const PERSON_WIDTH: f32 = 120.0;
const TRACK_WIDTH: f32 = 200.0;
const USER_WIDTH: f32 = 48.0;

/// The fields every card's item needs beyond the defaults.
const CARD_FIELDS: &[&str] = &["PrimaryImageAspectRatio"];
/// Cards on Home show a poster or a landscape image, whichever the item
/// has.
const CARD_IMAGES: &[ImageType] = &[ImageType::Primary, ImageType::Thumb, ImageType::Backdrop];

// What the library shows, and how it asks for it.

/// A library's kind, as the library view treats it. Books, photos and
/// Live TV aren't shown at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Collection {
    Movies,
    Shows,
    Music,
    BoxSets,
    Playlists,
    /// Anything else (home videos, music videos, mixed content): its
    /// folders and files as they are.
    Folders,
}

impl Collection {
    fn of(kind: CollectionKind) -> Option<Self> {
        match kind {
            CollectionKind::Movies => Some(Collection::Movies),
            CollectionKind::TvShows => Some(Collection::Shows),
            CollectionKind::Music => Some(Collection::Music),
            CollectionKind::BoxSets => Some(Collection::BoxSets),
            CollectionKind::Playlists => Some(Collection::Playlists),
            CollectionKind::Books | CollectionKind::Photos | CollectionKind::LiveTv => None,
            _ => Some(Collection::Folders),
        }
    }

    /// As `LibraryNav.kind` names it.
    fn nav_kind(self) -> &'static str {
        match self {
            Collection::Movies => "movies",
            Collection::Shows => "tvshows",
            Collection::Music => "music",
            Collection::BoxSets => "boxsets",
            Collection::Playlists => "playlists",
            Collection::Folders => "folders",
        }
    }

    fn tabs(self) -> &'static [Tab] {
        match self {
            Collection::Movies => &[Tab::Movies, Tab::Collections],
            Collection::Shows => &[Tab::Shows],
            Collection::Music => &[Tab::Albums, Tab::Artists, Tab::Playlists, Tab::Genres],
            Collection::BoxSets => &[Tab::Collections],
            Collection::Playlists => &[Tab::Playlists],
            Collection::Folders => &[Tab::Items],
        }
    }

    /// Whether Home shows the library's newest. A collection or playlist
    /// is made by hand, so what was added to it last says little.
    fn has_latest(self) -> bool {
        !matches!(self, Collection::BoxSets | Collection::Playlists)
    }
}

/// One of a library's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Movies,
    Collections,
    Shows,
    Albums,
    Artists,
    Playlists,
    Genres,
    /// A folder's own items, folders among them.
    Items,
}

impl Tab {
    const ALL: [Tab; 8] = [
        Tab::Movies,
        Tab::Collections,
        Tab::Shows,
        Tab::Albums,
        Tab::Artists,
        Tab::Playlists,
        Tab::Genres,
        Tab::Items,
    ];

    fn id(self) -> &'static str {
        match self {
            Tab::Movies => "movies",
            Tab::Collections => "collections",
            Tab::Shows => "shows",
            Tab::Albums => "albums",
            Tab::Artists => "artists",
            Tab::Playlists => "playlists",
            Tab::Genres => "genres",
            Tab::Items => "items",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Tab::ALL.into_iter().find(|tab| tab.id() == id)
    }

    fn title(self) -> &'static str {
        match self {
            Tab::Movies => "Movies",
            Tab::Collections => "Collections",
            Tab::Shows => "Shows",
            Tab::Albums => "Albums",
            Tab::Artists => "Artists",
            Tab::Playlists => "Playlists",
            Tab::Genres => "Genres",
            Tab::Items => "All",
        }
    }

    /// The item types the tab lists, which queries name: an untyped
    /// recursive query is slow on a large library.
    fn kinds(self) -> &'static [ItemKind] {
        match self {
            Tab::Movies => &[ItemKind::Movie],
            Tab::Collections => &[ItemKind::BoxSet],
            Tab::Shows => &[ItemKind::Series],
            Tab::Albums => &[ItemKind::MusicAlbum],
            Tab::Playlists => &[ItemKind::Playlist],
            Tab::Artists | Tab::Genres | Tab::Items => &[],
        }
    }

    fn fields(self) -> &'static [&'static str] {
        match self {
            Tab::Collections | Tab::Playlists | Tab::Items => {
                &["PrimaryImageAspectRatio", "ChildCount"]
            }
            _ => CARD_FIELDS,
        }
    }

    /// Whether the tab's items keep a watched state for the Unplayed
    /// filter to go by: music is listened to again and again, and an
    /// artist or genre has no state of its own.
    fn keeps_played(self) -> bool {
        matches!(
            self,
            Tab::Movies | Tab::Collections | Tab::Shows | Tab::Items
        )
    }

    fn sorts(self) -> &'static [Sort] {
        use Sort::*;
        match self {
            Tab::Movies => &[Name, Added, Released, Rating, Runtime, Played, Random],
            Tab::Shows => &[Name, Added, Released, Rating, Played, Random],
            Tab::Albums => &[Name, Artist, Released, Added, Random],
            Tab::Collections | Tab::Playlists | Tab::Items => &[Name, Added],
            Tab::Artists | Tab::Genres => &[Name],
        }
    }
}

/// A grid's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sort {
    Name,
    Added,
    Released,
    Rating,
    Runtime,
    Played,
    Artist,
    Random,
}

impl Sort {
    const ALL: [Sort; 8] = [
        Sort::Name,
        Sort::Added,
        Sort::Released,
        Sort::Rating,
        Sort::Runtime,
        Sort::Played,
        Sort::Artist,
        Sort::Random,
    ];

    fn id(self) -> &'static str {
        match self {
            Sort::Name => "name",
            Sort::Added => "added",
            Sort::Released => "released",
            Sort::Rating => "rating",
            Sort::Runtime => "runtime",
            Sort::Played => "played",
            Sort::Artist => "artist",
            Sort::Random => "random",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Sort::ALL.into_iter().find(|sort| sort.id() == id)
    }

    fn title(self) -> &'static str {
        match self {
            Sort::Name => "Name",
            Sort::Added => "Date Added",
            Sort::Released => "Release Date",
            Sort::Rating => "Rating",
            Sort::Runtime => "Runtime",
            Sort::Played => "Last Played",
            Sort::Artist => "Album Artist",
            Sort::Random => "Random",
        }
    }

    /// `sortBy`, with the name last to settle ties.
    fn fields(self, tab: Tab) -> &'static [&'static str] {
        match self {
            Sort::Name => &["SortName"],
            // A show is new when an episode is, not when it was first added.
            Sort::Added if tab == Tab::Shows => &["DateLastContentAdded", "SortName"],
            Sort::Added => &["DateCreated", "SortName"],
            Sort::Released => &["PremiereDate", "ProductionYear", "SortName"],
            Sort::Rating => &["CommunityRating", "SortName"],
            Sort::Runtime => &["Runtime", "SortName"],
            Sort::Played => &["DatePlayed", "SortName"],
            Sort::Artist => &["AlbumArtist", "SortName"],
            Sort::Random => &["Random"],
        }
    }
}

/// Which request a grid's items come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Endpoint {
    Items,
    AlbumArtists,
    Genres,
}

#[derive(Debug, Clone, PartialEq)]
struct GridRequest {
    endpoint: Endpoint,
    query: ItemsQuery,
}

impl GridRequest {
    fn fetch(&self, client: &Client) -> Result<QueryResult<BaseItemDto>, Error> {
        match self.endpoint {
            Endpoint::Items => client.items(&self.query),
            Endpoint::AlbumArtists => client.album_artists(&self.query),
            Endpoint::Genres => client.genres(&self.query),
        }
    }
}

/// The request for `limit` of a grid's items from `start`.
fn grid_request(page: &GridPage, start: u32, limit: u32) -> GridRequest {
    let order = if page.descending {
        SortOrder::Descending
    } else {
        SortOrder::Ascending
    };
    let mut filters = Vec::new();
    if page.unplayed && page.tab.keeps_played() {
        filters.push("IsUnplayed");
    }
    if page.favorites {
        filters.push("IsFavorite");
    }
    let mut query = ItemsQuery::new()
        .sort(page.sort.fields(page.tab), order)
        .filters(&filters)
        .fields(page.tab.fields())
        .images(&[ImageType::Primary], 1)
        .page(start, limit);
    query.enable_total_record_count = Some(true);
    let endpoint = match page.tab {
        Tab::Artists => {
            query = query.parent(&page.parent);
            Endpoint::AlbumArtists
        }
        Tab::Genres => {
            query = query.parent(&page.parent).types(&[ItemKind::MusicAlbum]);
            Endpoint::Genres
        }
        // Playlists live in a folder of their own, not in the music
        // library: its tab lists those of music.
        Tab::Playlists if page.collection == Collection::Music => {
            query = query
                .types(&[ItemKind::Playlist])
                .media_types(&["Audio"])
                .recursive();
            Endpoint::Items
        }
        Tab::Items => {
            query = query.parent(&page.parent);
            Endpoint::Items
        }
        tab => {
            query = query.parent(&page.parent).types(tab.kinds()).recursive();
            Endpoint::Items
        }
    };
    GridRequest { endpoint, query }
}

fn resume_query() -> ItemsQuery {
    ItemsQuery::new()
        .media_types(&["Video"])
        .fields(CARD_FIELDS)
        .images(CARD_IMAGES, 1)
        .limit(ROW_LIMIT)
}

fn next_up_query() -> ItemsQuery {
    ItemsQuery::new()
        .fields(CARD_FIELDS)
        .images(CARD_IMAGES, 1)
        .limit(ROW_LIMIT)
        // Started episodes are in Continue Watching already.
        .param("enableResumable", "false")
}

fn latest_query(parent: &str) -> ItemsQuery {
    ItemsQuery::new()
        .parent(parent)
        .fields(CARD_FIELDS)
        .images(CARD_IMAGES, 1)
        .limit(ROW_LIMIT)
}

fn seasons_query() -> ItemsQuery {
    ItemsQuery::new()
        .fields(CARD_FIELDS)
        .images(&[ImageType::Primary], 1)
}

fn episodes_query(season: &str) -> ItemsQuery {
    ItemsQuery::new()
        .season(season)
        .fields(&["Overview", "PrimaryImageAspectRatio"])
        .images(CARD_IMAGES, 1)
}

fn album_tracks_query(album: &str) -> ItemsQuery {
    ItemsQuery::new()
        .parent(album)
        .types(&[ItemKind::Audio])
        .recursive()
        .sort(
            &["ParentIndexNumber", "IndexNumber", "SortName"],
            SortOrder::Ascending,
        )
        .limit(PAGE_LIMIT)
}

fn playlist_query() -> ItemsQuery {
    ItemsQuery::new()
        .fields(CARD_FIELDS)
        .images(&[ImageType::Primary], 1)
        .limit(PAGE_LIMIT)
}

fn similar_query() -> ItemsQuery {
    ItemsQuery::new()
        .fields(CARD_FIELDS)
        .images(&[ImageType::Primary], 1)
        .limit(ROW_LIMIT)
}

/// Songs in the order they play: by album, oldest first, then by disc and
/// number.
fn songs_query(query: ItemsQuery) -> ItemsQuery {
    query.types(&[ItemKind::Audio]).recursive().sort(
        &[
            "PremiereDate",
            "Album",
            "ParentIndexNumber",
            "IndexNumber",
            "SortName",
        ],
        SortOrder::Ascending,
    )
}

/// An artist's songs as the artist's page lists them, in the order that
/// playing the artist plays them.
fn artist_songs_query(artist: &str) -> ItemsQuery {
    songs_query(ItemsQuery::new().artists(&[artist]))
        .fields(CARD_FIELDS)
        .images(&[ImageType::Primary], 1)
        .limit(PAGE_LIMIT)
}

fn artist_albums_query(artist: &str) -> ItemsQuery {
    ItemsQuery::new()
        .album_artists(&[artist])
        .types(&[ItemKind::MusicAlbum])
        .recursive()
        .sort(
            &["PremiereDate", "ProductionYear", "SortName"],
            SortOrder::Descending,
        )
        .fields(CARD_FIELDS)
        .images(&[ImageType::Primary], 1)
        .limit(PAGE_LIMIT)
}

fn collection_items_query(collection: &str) -> ItemsQuery {
    ItemsQuery::new()
        .parent(collection)
        .sort(
            &["PremiereDate", "ProductionYear", "SortName"],
            SortOrder::Ascending,
        )
        .fields(&["PrimaryImageAspectRatio", "ChildCount"])
        .images(&[ImageType::Primary], 1)
        .limit(PAGE_LIMIT)
}

fn genre_query(genre: &str, kind: ItemKind) -> ItemsQuery {
    ItemsQuery::new()
        .genres(&[genre])
        .types(&[kind])
        .recursive()
        .sort(&["SortName"], SortOrder::Ascending)
        .fields(CARD_FIELDS)
        .images(&[ImageType::Primary], 1)
        .limit(ROW_LIMIT * 3)
}

fn person_query(person: &str, kind: ItemKind) -> ItemsQuery {
    let mut query = ItemsQuery::new()
        .types(&[kind])
        .recursive()
        .sort(
            &["PremiereDate", "ProductionYear", "SortName"],
            SortOrder::Descending,
        )
        .fields(CARD_FIELDS)
        .images(&[ImageType::Primary], 1)
        .limit(ROW_LIMIT * 3);
    query.person_ids = vec![person.to_owned()];
    query
}

/// Search's rows: what each looks for.
const SEARCHES: [(&str, &str, ItemKind); 5] = [
    ("movies", "Movies", ItemKind::Movie),
    ("shows", "Shows", ItemKind::Series),
    ("episodes", "Episodes", ItemKind::Episode),
    ("albums", "Albums", ItemKind::MusicAlbum),
    ("songs", "Songs", ItemKind::Audio),
];

/// What a search box's text searches for; None for nothing.
fn search_term(text: &str) -> Option<String> {
    let term = text.trim();
    (!term.is_empty()).then(|| term.to_owned())
}

// Pages.

#[derive(Debug, Clone, PartialEq)]
enum Page {
    Home,
    Grid(GridPage),
    Detail(DetailPage),
    Search,
    /// This Computer: the welcome screen's Open, Open URL and Recent.
    Local,
    SignIn,
}

impl Page {
    /// As `Library.page` names it.
    fn name(&self) -> &'static str {
        match self {
            Page::Home => "home",
            Page::Grid(_) => "grid",
            Page::Detail(_) => "detail",
            Page::Search => "search",
            Page::Local => "local",
            Page::SignIn => "sign-in",
        }
    }
}

/// A library, or a folder in one, as a grid.
#[derive(Debug, Clone, PartialEq)]
struct GridPage {
    /// Whose items these are.
    parent: String,
    /// The library in the bar it is under.
    root: String,
    title: String,
    collection: Collection,
    tab: Tab,
    sort: Sort,
    descending: bool,
    unplayed: bool,
    favorites: bool,
}

impl GridPage {
    fn new(parent: &str, root: &str, title: &str, collection: Collection) -> Self {
        Self {
            parent: parent.to_owned(),
            root: root.to_owned(),
            title: title.to_owned(),
            collection,
            tab: collection.tabs()[0],
            sort: Sort::Name,
            descending: false,
            unplayed: false,
            favorites: false,
        }
    }
}

/// An item's page.
#[derive(Debug, Clone, PartialEq)]
struct DetailPage {
    id: String,
    /// Unknown until the item says, when it wasn't on the page before.
    kind: ItemKind,
    root: String,
    /// A show's season chosen, kept for when Back returns to it.
    season: Option<String>,
}

/// The pages, the one shown last, each with how far down it was scrolled
/// when another was opened over it, so that Back returns there.
#[derive(Debug, Clone, Default, PartialEq)]
struct Pages(Vec<(Page, f32)>);

impl Pages {
    fn top(&self) -> Option<&Page> {
        self.0.last().map(|(page, _)| page)
    }

    /// Starts again with these pages, each at its top.
    fn set(&mut self, pages: Vec<Page>) {
        self.0 = pages.into_iter().map(|page| (page, 0.0)).collect();
    }

    /// The bar's pages start afresh from Home, so that Back from any of
    /// them goes there.
    fn go_to(&mut self, page: Option<Page>) {
        self.set(std::iter::once(Page::Home).chain(page).collect());
    }

    /// Opens a page over the one shown, which was scrolled to `scroll`.
    fn push(&mut self, page: Page, scroll: f32) {
        if self.top() == Some(&page) {
            return;
        }
        if let Some((_, kept)) = self.0.last_mut() {
            *kept = scroll;
        }
        self.0.push((page, 0.0));
    }

    fn pop(&mut self) -> bool {
        if self.0.len() > 1 {
            self.0.pop();
            true
        } else {
            false
        }
    }

    /// Where the page on top was scrolled when it was left.
    fn scroll(&self) -> f32 {
        self.0.last().map_or(0.0, |(_, scroll)| *scroll)
    }

    fn replace_top(&mut self, page: Page) {
        if let Some((top, _)) = self.0.last_mut() {
            *top = page;
        }
    }

    fn remove_sign_in(&mut self) {
        self.0.retain(|(page, _)| *page != Page::SignIn);
    }

    fn can_go_back(&self) -> bool {
        self.0.len() > 1
    }

    /// The entry of the bar the page is under.
    fn nav_current(&self) -> String {
        match self.top() {
            Some(Page::Home) => "home".into(),
            Some(Page::Grid(page)) => page.root.clone(),
            Some(Page::Detail(page)) => page.root.clone(),
            Some(Page::Search) => "search".into(),
            Some(Page::Local) => "local".into(),
            Some(Page::SignIn) | None => String::new(),
        }
    }
}

// Items as the view shows them.

/// As `LibraryCard.kind` names an item's kind.
fn card_kind(item: &BaseItemDto) -> &'static str {
    match item.kind {
        ItemKind::Movie => "movie",
        ItemKind::Series => "series",
        ItemKind::Season => "season",
        ItemKind::Episode => "episode",
        ItemKind::BoxSet => "collection",
        ItemKind::Playlist => "playlist",
        ItemKind::MusicAlbum => "album",
        ItemKind::MusicArtist => "artist",
        ItemKind::Audio => "song",
        ItemKind::Genre | ItemKind::MusicGenre => "genre",
        ItemKind::CollectionFolder | ItemKind::UserView => "library",
        ItemKind::Person => "person",
        ItemKind::Video | ItemKind::MusicVideo | ItemKind::Trailer => "video",
        _ if item.is_folder => "folder",
        _ => "video",
    }
}

/// The kind a card's kind stands for, when the item isn't at hand.
fn item_kind(kind: &str) -> ItemKind {
    match kind {
        "movie" => ItemKind::Movie,
        "series" => ItemKind::Series,
        "season" => ItemKind::Season,
        "episode" => ItemKind::Episode,
        "collection" => ItemKind::BoxSet,
        "playlist" => ItemKind::Playlist,
        "album" => ItemKind::MusicAlbum,
        "artist" => ItemKind::MusicArtist,
        "song" => ItemKind::Audio,
        "genre" => ItemKind::MusicGenre,
        "library" => ItemKind::CollectionFolder,
        "folder" => ItemKind::Folder,
        "person" => ItemKind::Person,
        "video" => ItemKind::Video,
        _ => ItemKind::Unknown,
    }
}

/// Items that are one file to play.
fn is_playable(kind: ItemKind) -> bool {
    matches!(
        kind,
        ItemKind::Movie
            | ItemKind::Episode
            | ItemKind::Video
            | ItemKind::MusicVideo
            | ItemKind::Trailer
            | ItemKind::Audio
    )
}

/// Items that play as the list of what they hold.
fn plays_children(kind: ItemKind) -> bool {
    matches!(
        kind,
        ItemKind::Series
            | ItemKind::Season
            | ItemKind::MusicAlbum
            | ItemKind::Playlist
            | ItemKind::BoxSet
            | ItemKind::MusicArtist
            | ItemKind::MusicGenre
            | ItemKind::Folder
            | ItemKind::CollectionFolder
    )
}

fn is_folder_kind(kind: ItemKind) -> bool {
    matches!(
        kind,
        ItemKind::Folder
            | ItemKind::AggregateFolder
            | ItemKind::PhotoAlbum
            | ItemKind::PlaylistsFolder
            | ItemKind::ManualPlaylistsFolder
    )
}

/// Width over height of an item's own image, when the server doesn't say.
fn default_aspect(kind: ItemKind) -> f32 {
    match kind {
        ItemKind::Movie
        | ItemKind::Series
        | ItemKind::Season
        | ItemKind::BoxSet
        | ItemKind::Person
        | ItemKind::Trailer => 2.0 / 3.0,
        ItemKind::MusicAlbum
        | ItemKind::MusicArtist
        | ItemKind::Audio
        | ItemKind::Playlist
        | ItemKind::MusicGenre
        | ItemKind::Genre => 1.0,
        _ => 16.0 / 9.0,
    }
}

/// A card shows a poster (or cover), or a landscape image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Art {
    Poster,
    Thumb,
}

/// Which image of which item to show, and its placeholder.
#[derive(Debug, Clone, PartialEq)]
struct Picture {
    item: String,
    kind: ImageType,
    index: Option<u32>,
    tag: String,
    blurhash: Option<String>,
    aspect: f32,
}

/// The item's own primary image.
fn primary(item: &BaseItemDto) -> Option<Picture> {
    let tag = item.image_tag(ImageType::Primary)?;
    let aspect = item
        .primary_image_aspect_ratio
        .map(|ratio| ratio as f32)
        .filter(|ratio| ratio.is_finite() && *ratio > 0.0)
        .unwrap_or_else(|| default_aspect(item.kind));
    Some(Picture {
        item: item.id.clone(),
        kind: ImageType::Primary,
        index: None,
        tag: tag.to_owned(),
        blurhash: item.blurhash(ImageType::Primary, tag).map(str::to_owned),
        aspect,
    })
}

/// An image of the item's own other than the primary, or one it borrows
/// from its show or album (`owner`); the item's blurhashes cover those
/// too.
fn borrowed(
    item: &BaseItemDto,
    owner: Option<&String>,
    tag: Option<&String>,
    kind: ImageType,
    index: Option<u32>,
    aspect: f32,
) -> Option<Picture> {
    let (owner, tag) = (owner?, tag?);
    Some(Picture {
        item: owner.clone(),
        kind,
        index,
        tag: tag.clone(),
        blurhash: item.blurhash(kind, tag).map(str::to_owned),
        aspect,
    })
}

/// The image a card shows.
fn picture(item: &BaseItemDto, art: Art) -> Option<Picture> {
    let landscape = 16.0 / 9.0;
    let id = Some(&item.id);
    let poster = || {
        primary(item).or_else(|| match item.kind {
            ItemKind::Episode | ItemKind::Season => borrowed(
                item,
                item.series_id.as_ref(),
                item.series_primary_image_tag.as_ref(),
                ImageType::Primary,
                None,
                2.0 / 3.0,
            ),
            ItemKind::Audio => borrowed(
                item,
                item.album_id.as_ref(),
                item.album_primary_image_tag.as_ref(),
                ImageType::Primary,
                None,
                1.0,
            ),
            _ => borrowed(
                item,
                item.parent_primary_image_item_id.as_ref(),
                item.parent_primary_image_tag.as_ref(),
                ImageType::Primary,
                None,
                default_aspect(item.kind),
            ),
        })
    };
    if art == Art::Poster {
        return poster();
    }
    let thumb = item.image_tags.get(ImageType::Thumb.as_str());
    let own = || {
        borrowed(item, id, thumb, ImageType::Thumb, None, landscape).or_else(|| {
            borrowed(
                item,
                id,
                item.backdrop_image_tags.first(),
                ImageType::Backdrop,
                Some(0),
                landscape,
            )
        })
    };
    let parents = || {
        borrowed(
            item,
            item.parent_thumb_item_id.as_ref(),
            item.parent_thumb_image_tag.as_ref(),
            ImageType::Thumb,
            None,
            landscape,
        )
        .or_else(|| {
            borrowed(
                item,
                item.parent_backdrop_item_id.as_ref(),
                item.parent_backdrop_image_tags.first(),
                ImageType::Backdrop,
                Some(0),
                landscape,
            )
        })
    };
    match item.kind {
        // An episode's own picture is a still from it, and says the most.
        ItemKind::Episode | ItemKind::Video | ItemKind::CollectionFolder | ItemKind::UserView => {
            primary(item).or_else(own).or_else(parents)
        }
        _ => own().or_else(parents).or_else(poster),
    }
}

/// The backdrop behind an item's page.
fn backdrop(item: &BaseItemDto) -> Option<Picture> {
    let landscape = 16.0 / 9.0;
    borrowed(
        item,
        Some(&item.id),
        item.backdrop_image_tags.first(),
        ImageType::Backdrop,
        Some(0),
        landscape,
    )
    .or_else(|| {
        borrowed(
            item,
            item.parent_backdrop_item_id.as_ref(),
            item.parent_backdrop_image_tags.first(),
            ImageType::Backdrop,
            Some(0),
            landscape,
        )
    })
}

fn logo(item: &BaseItemDto) -> Option<Picture> {
    borrowed(
        item,
        Some(&item.id),
        item.image_tags.get(ImageType::Logo.as_str()),
        ImageType::Logo,
        None,
        2.0,
    )
    .or_else(|| {
        borrowed(
            item,
            item.parent_logo_item_id.as_ref(),
            item.parent_logo_image_tag.as_ref(),
            ImageType::Logo,
            None,
            2.0,
        )
    })
}

/// How a card's title and the line under it read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lines {
    /// On Home, where an episode is its show's.
    Home,
    /// Everywhere else: the item's own name.
    Own,
    /// An artist's albums, on the artist's page or an album's: the year
    /// says more than the artist's name again.
    Artist,
}

fn count(n: Option<i32>, one: &str, many: &str) -> String {
    match n {
        Some(1) => format!("1 {one}"),
        Some(n) if n > 1 => format!("{n} {many}"),
        _ => String::new(),
    }
}

/// The parts that are there, with a dot between.
fn joined(parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// "S1 E3", "S1 E3–4", "E3", or nothing.
fn episode_number(item: &BaseItemDto) -> String {
    let Some(episode) = item.index_number else {
        return String::new();
    };
    let episode = match item.index_number_end {
        Some(end) if end > episode => format!("E{episode}–{end}"),
        _ => format!("E{episode}"),
    };
    match item.parent_index_number {
        Some(season) => format!("S{season} {episode}"),
        None => episode,
    }
}

fn year(item: &BaseItemDto) -> String {
    item.production_year
        .map(|year| year.to_string())
        .unwrap_or_default()
}

/// What a person did, as the server names it ("GuestStar"), in words:
/// "Guest Star".
fn person_kind(kind: &str) -> String {
    let mut words = String::with_capacity(kind.len() + 2);
    for (index, letter) in kind.char_indices() {
        if index > 0 && letter.is_uppercase() {
            words.push(' ');
        }
        words.push(letter);
    }
    words
}

/// "1 h 52 min", "45 min".
fn runtime(seconds: f64) -> String {
    let minutes = ((seconds / 60.0).round() as u64).max(1);
    match (minutes / 60, minutes % 60) {
        (0, minutes) => format!("{minutes} min"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    }
}

/// When an episode first aired, as "May 17, 2024".
fn air_date(item: &BaseItemDto) -> String {
    item.premiere_date
        .as_deref()
        .and_then(|date| date.get(..10))
        .and_then(|date| date.parse::<jiff::civil::Date>().ok())
        .map(|date| date.strftime("%B %-d, %Y").to_string())
        .unwrap_or_default()
}

fn artists(item: &BaseItemDto) -> String {
    if !item.artists.is_empty() {
        item.artists.join(", ")
    } else {
        item.album_artist.clone().unwrap_or_default()
    }
}

/// A card's title and the line under it.
fn card_lines(item: &BaseItemDto, lines: Lines) -> (String, String) {
    let name = item.name.clone();
    match item.kind {
        ItemKind::Episode => {
            let number = episode_number(item);
            match (lines, item.series_name.as_deref()) {
                (Lines::Home, Some(series)) => (series.to_owned(), joined(&[&number, &name])),
                (_, series) => (name, joined(&[series.unwrap_or_default(), &number])),
            }
        }
        ItemKind::Season => match item.series_name.as_deref() {
            Some(series) if lines == Lines::Home => (series.to_owned(), name),
            _ => (name, String::new()),
        },
        ItemKind::Movie
        | ItemKind::Series
        | ItemKind::Video
        | ItemKind::MusicVideo
        | ItemKind::Trailer => (name, year(item)),
        ItemKind::MusicAlbum if lines == Lines::Artist => (name, year(item)),
        ItemKind::MusicAlbum => {
            let artist = item
                .album_artist
                .clone()
                .unwrap_or_else(|| item.artists.join(", "));
            (name, artist)
        }
        ItemKind::Audio => (name, artists(item)),
        ItemKind::BoxSet => (name, count(item.child_count, "item", "items")),
        ItemKind::Playlist if item.media_type == MediaKind::Audio => {
            (name, count(item.child_count, "song", "songs"))
        }
        ItemKind::Playlist => (name, count(item.child_count, "item", "items")),
        _ => (name, String::new()),
    }
}

/// An item page's line of facts: "2021 · 1 h 52 min · PG-13 · ★ 7.4".
fn facts(item: &BaseItemDto) -> String {
    let mut parts: Vec<String> = Vec::new();
    if item.kind == ItemKind::Episode {
        parts.push(episode_number(item));
        parts.push(air_date(item));
    } else {
        parts.push(year(item));
    }
    match item.kind {
        ItemKind::Series => parts.push(count(item.child_count, "season", "seasons")),
        ItemKind::MusicAlbum => parts.push(count(item.child_count, "song", "songs")),
        ItemKind::Playlist if item.media_type == MediaKind::Audio => {
            parts.push(count(item.child_count, "song", "songs"))
        }
        ItemKind::Playlist | ItemKind::BoxSet => {
            parts.push(count(item.child_count, "item", "items"))
        }
        _ => {}
    }
    if !matches!(item.kind, ItemKind::Series | ItemKind::Season)
        && let Some(seconds) = item.duration()
    {
        parts.push(runtime(seconds));
    }
    parts.push(item.official_rating.clone().unwrap_or_default());
    if let Some(rating) = item.community_rating.filter(|rating| *rating > 0.0) {
        parts.push(format!("★ {rating:.1}"));
    }
    let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
    joined(&parts)
}

/// How much of a file is watched, 0 to 1; negative when nothing is, or
/// when it is done. A file played before and stopped part-way through
/// again has both its played mark and its progress, as Continue Watching
/// lists it.
fn progress(item: &BaseItemDto) -> f32 {
    let Some(data) = item.user_data.as_ref() else {
        return -1.0;
    };
    if !is_playable(item.kind) || data.playback_position_ticks <= 0 {
        return -1.0;
    }
    let fraction = data
        .played_percentage
        .map(|percent| percent / 100.0)
        .or_else(|| {
            item.run_time_ticks
                .filter(|ticks| *ticks > 0)
                .map(|ticks| data.playback_position_ticks as f64 / ticks as f64)
        });
    fraction.map_or(-1.0, |fraction| fraction.clamp(0.0, 1.0) as f32)
}

/// Episodes left to watch in a show or season.
fn unplayed(item: &BaseItemDto) -> i32 {
    match item.kind {
        ItemKind::Series | ItemKind::Season => item
            .user_data
            .as_ref()
            .and_then(|data| data.unplayed_item_count)
            .unwrap_or(0)
            .max(0),
        _ => 0,
    }
}

/// A card without its image.
fn card_of(item: &BaseItemDto, art: Art, lines: Lines) -> LibraryCard {
    let (title, subtitle) = card_lines(item, lines);
    let aspect = picture(item, art).map_or_else(
        || match art {
            Art::Thumb => 16.0 / 9.0,
            Art::Poster => default_aspect(item.kind),
        },
        |picture| picture.aspect,
    );
    LibraryCard {
        id: item.id.clone().into(),
        kind: card_kind(item).into(),
        title: title.into(),
        subtitle: subtitle.into(),
        image: Image::default(),
        aspect,
        progress: progress(item),
        played: item.played(),
        favorite: item.is_favorite(),
        unplayed: unplayed(item),
    }
}

/// How a list of tracks reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tracks {
    Episodes,
    Album,
    Playlist,
    /// An artist's songs, each with its album.
    Artist,
}

/// A track's number: an episode's, a song's ("1-3" on an album of several
/// discs), or its place in a playlist.
fn track_number(item: &BaseItemDto, style: Tracks, discs: bool, place: usize) -> String {
    match style {
        Tracks::Episodes => match (item.index_number, item.index_number_end) {
            (Some(first), Some(last)) if last > first => format!("{first}-{last}"),
            (Some(number), _) => number.to_string(),
            _ => String::new(),
        },
        Tracks::Album => match (item.parent_index_number, item.index_number) {
            (Some(disc), Some(track)) if discs => format!("{disc}-{track}"),
            (_, Some(track)) => track.to_string(),
            _ => String::new(),
        },
        Tracks::Playlist | Tracks::Artist => (place + 1).to_string(),
    }
}

/// The line under a track's title.
fn track_subtitle(item: &BaseItemDto, style: Tracks, album_artist: &str) -> String {
    match style {
        Tracks::Episodes => air_date(item),
        // Only the artists who aren't the album's. Each song names its
        // album's artist too, for when the songs come before the album.
        Tracks::Album => {
            let album_artist = if album_artist.is_empty() {
                item.album_artist.as_deref().unwrap_or_default()
            } else {
                album_artist
            };
            let artists = artists(item);
            if artists == album_artist {
                String::new()
            } else {
                artists
            }
        }
        Tracks::Playlist => match item.kind {
            ItemKind::Episode => card_lines(item, Lines::Own).1,
            _ => artists(item),
        },
        Tracks::Artist => item.album.clone().unwrap_or_default(),
    }
}

fn track_of(
    item: &BaseItemDto,
    style: Tracks,
    discs: bool,
    place: usize,
    album_artist: &str,
) -> LibraryTrack {
    LibraryTrack {
        id: item.id.clone().into(),
        number: track_number(item, style, discs, place).into(),
        title: item.name.clone().into(),
        subtitle: track_subtitle(item, style, album_artist).into(),
        overview: item.overview.clone().unwrap_or_default().into(),
        duration: item
            .duration()
            .map(crate::util::format_time)
            .unwrap_or_default()
            .into(),
        image: Image::default(),
        progress: progress(item),
        played: item.played(),
        favorite: item.is_favorite(),
        playing: false,
    }
}

/// The version, audio and subtitle chosen on an item's page; None for the
/// server's default.
#[derive(Debug, Clone, Default, PartialEq)]
struct Choice {
    version: Option<String>,
    audio: Option<i32>,
    subtitle: Option<i32>,
}

impl Choice {
    /// The version chosen, or the first.
    fn source<'a>(&self, item: &'a BaseItemDto) -> Option<&'a MediaSourceInfo> {
        self.version
            .as_ref()
            .and_then(|id| item.media_sources.iter().find(|source| source.id == *id))
            .or_else(|| item.media_sources.first())
    }
}

/// A version's name, as the item's page and the player's menu show it:
/// the server's, or its place among the item's versions.
pub(super) fn version_title(index: usize, source: &MediaSourceInfo) -> String {
    source
        .name
        .clone()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("Version {}", index + 1))
}

/// A track's title as the menus show it: the server's label, with the
/// codec's name as people know it and the rest in brackets ("English (SRT,
/// external)" for "English - SUBRIP - External").
pub(super) fn stream_title(stream: &MediaStream) -> String {
    let Some(label) = stream
        .display_title
        .as_deref()
        .filter(|label| !label.is_empty())
    else {
        return stream
            .title
            .clone()
            .or_else(|| stream.language.clone())
            .unwrap_or_else(|| format!("Track {}", stream.index));
    };
    let mut parts = label
        .split(" - ")
        .map(str::trim)
        .filter(|part| !part.is_empty());
    let Some(first) = parts.next() else {
        return label.to_owned();
    };
    let rest: Vec<String> = parts
        .map(|part| match part.to_ascii_uppercase().as_str() {
            "SUBRIP" | "SRT" => "SRT".to_owned(),
            "MOV_TEXT" => "MP4 text".to_owned(),
            "WEBVTT" | "VTT" => "WebVTT".to_owned(),
            "ASS" | "SSA" => part.to_ascii_uppercase(),
            "PGSSUB" | "HDMV_PGS_SUBTITLE" | "PGS" => "PGS".to_owned(),
            "DVDSUB" | "DVD_SUBTITLE" | "VOBSUB" => "VobSub".to_owned(),
            "DVBSUB" | "DVB_SUBTITLE" => "DVB".to_owned(),
            "EXTERNAL" | "DEFAULT" | "FORCED" | "STEREO" | "MONO" => part.to_lowercase(),
            _ => part.to_owned(),
        })
        .collect();
    if rest.is_empty() {
        first.to_owned()
    } else {
        format!("{first} ({})", rest.join(", "))
    }
}

/// An item's page without its images.
fn detail_of(item: &BaseItemDto, choice: &Choice) -> LibraryDetail {
    let (parent_title, parent_id, parent_kind) = match item.kind {
        ItemKind::Episode | ItemKind::Season => {
            (item.series_name.clone(), item.series_id.clone(), "series")
        }
        ItemKind::MusicAlbum => item
            .album_artists
            .first()
            .map_or((None, None, ""), |artist| {
                (Some(artist.name.clone()), Some(artist.id.clone()), "artist")
            }),
        ItemKind::Audio => (item.album.clone(), item.album_id.clone(), "album"),
        _ => (None, None, ""),
    };
    let resume = match item.resume_position() {
        Some(seconds) if is_playable(item.kind) => {
            format!("Resume {}", crate::util::format_time(seconds))
        }
        _ => String::new(),
    };
    let source = choice.source(item);
    let versions: Vec<LibraryChoice> = item
        .media_sources
        .iter()
        .enumerate()
        .map(|(index, source)| LibraryChoice {
            id: source.id.clone().into(),
            title: version_title(index, source).into(),
            detail: source
                .media_streams
                .iter()
                .find(|stream| stream.kind == StreamKind::Video)
                .map(stream_title)
                .unwrap_or_default()
                .into(),
        })
        .collect();
    let streams = |kind: StreamKind| -> Vec<LibraryChoice> {
        source
            .map(|source| {
                source
                    .media_streams
                    .iter()
                    .filter(|stream| stream.kind == kind)
                    .map(|stream| LibraryChoice {
                        id: stream.index.to_string().into(),
                        title: stream_title(stream).into(),
                        detail: SharedString::new(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let audio = streams(StreamKind::Audio);
    let mut subtitles = streams(StreamKind::Subtitle);
    if !subtitles.is_empty() {
        subtitles.insert(
            0,
            LibraryChoice {
                id: "-1".into(),
                title: "Off".into(),
                detail: SharedString::new(),
            },
        );
    }
    let audio_track = choice
        .audio
        .or_else(|| source.and_then(|source| source.default_audio_stream_index))
        .filter(|_| !audio.is_empty())
        .map(|index| index.to_string())
        .unwrap_or_default();
    let subtitle_track = if subtitles.is_empty() {
        String::new()
    } else {
        choice
            .subtitle
            .or_else(|| source.and_then(|source| source.default_subtitle_stream_index))
            .unwrap_or(-1)
            .to_string()
    };
    LibraryDetail {
        id: item.id.clone().into(),
        kind: card_kind(item).into(),
        loaded: true,
        title: item.name.clone().into(),
        tagline: item.taglines.first().cloned().unwrap_or_default().into(),
        overview: item.overview.clone().unwrap_or_default().into(),
        facts: facts(item).into(),
        genres: item.genres.join(", ").into(),
        poster: Image::default(),
        poster_aspect: primary(item).map_or_else(|| default_aspect(item.kind), |p| p.aspect),
        backdrop: Image::default(),
        logo: Image::default(),
        progress: progress(item),
        played: item.played(),
        favorite: item.is_favorite(),
        can_play: is_playable(item.kind) || plays_children(item.kind),
        resume: resume.into(),
        parent_title: parent_title.unwrap_or_default().into(),
        parent_id: parent_id.unwrap_or_default().into(),
        parent_kind: parent_kind.into(),
        versions: ModelRc::new(VecModel::from(versions)),
        version: source
            .map(|source| source.id.clone())
            .unwrap_or_default()
            .into(),
        audio: ModelRc::new(VecModel::from(audio)),
        audio_track: audio_track.into(),
        subtitles: ModelRc::new(VecModel::from(subtitles)),
        subtitle_track: subtitle_track.into(),
    }
}

/// A grid's cards split into lines of `columns`.
fn lines(cards: &[LibraryCard], columns: usize) -> Vec<LibraryRow> {
    cards
        .chunks(columns.max(1))
        .enumerate()
        .map(|(line, cards)| line_row(line, cards.to_vec()))
        .collect()
}

fn line_row(line: usize, cards: Vec<LibraryCard>) -> LibraryRow {
    LibraryRow {
        id: format!("line-{line}").into(),
        title: SharedString::new(),
        cards: ModelRc::new(VecModel::from(cards)),
    }
}

/// Adds cards to a grid of `columns` that holds `start` of them: to its
/// last line first, if it isn't full, then as new lines. The lines there
/// stay as they are, with their images.
fn append_lines(
    rows: &VecModel<LibraryRow>,
    start: usize,
    cards: Vec<LibraryCard>,
    columns: usize,
) {
    let columns = columns.max(1);
    let mut cards = cards.into_iter();
    if !start.is_multiple_of(columns)
        && let Some(line) = rows.row_data(start / columns)
        && let Some(model) = line.cards.as_any().downcast_ref::<VecModel<LibraryCard>>()
    {
        for card in cards.by_ref().take(columns - start % columns) {
            model.push(card);
        }
    }
    let rest: Vec<LibraryCard> = cards.collect();
    let first_line = start.div_ceil(columns);
    for (offset, line) in rest.chunks(columns).enumerate() {
        rows.push(line_row(first_line + offset, line.to_vec()));
    }
}

/// Where the grid's `index`th card is: its line, and its place in it.
fn place(index: usize, columns: usize) -> (usize, usize) {
    let columns = columns.max(1);
    (index / columns, index % columns)
}

// Playing.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Play { from_start: bool, shuffle: bool },
    Queue { next: bool },
}

/// Where what is opened starts playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Start {
    /// At this item of the list: a song of an album's page plays the
    /// album from it, with the songs before it in the playlist too.
    At(usize),
    /// At the first, which the rest follow as they would of themselves:
    /// the next episodes after the one asked for, each resuming where the
    /// server says.
    Lead,
}

/// What a show, season, album or playlist plays: what it holds that can be
/// played, in order. Blocks.
fn children(client: &Client, item: &BaseItemDto) -> Result<Vec<BaseItemDto>, Error> {
    let songs = |query: ItemsQuery| songs_query(query).limit(MAX_PLAYLIST);
    let all = || ItemsQuery::new().limit(MAX_PLAYLIST);
    let page = match item.kind {
        ItemKind::Series => client.episodes(&item.id, &all())?,
        ItemKind::Season => match &item.series_id {
            Some(series) => client.episodes(series, &all().season(&item.id))?,
            None => client.items(
                &all()
                    .parent(&item.id)
                    .types(&[ItemKind::Episode])
                    .sort(&["IndexNumber"], SortOrder::Ascending),
            )?,
        },
        ItemKind::MusicAlbum => client.items(&album_tracks_query(&item.id).limit(MAX_PLAYLIST))?,
        ItemKind::Playlist => client.playlist_items(&item.id, &all())?,
        ItemKind::BoxSet => client.items(&collection_items_query(&item.id).limit(MAX_PLAYLIST))?,
        ItemKind::MusicArtist => client.items(&songs(ItemsQuery::new().artists(&[&item.id])))?,
        ItemKind::MusicGenre | ItemKind::Genre => {
            client.items(&songs(ItemsQuery::new().genres(&[&item.id])))?
        }
        _ => client.items(
            &ItemsQuery::new()
                .parent(&item.id)
                .media_types(&["Video", "Audio"])
                .recursive()
                .sort(&["SortName"], SortOrder::Ascending)
                .limit(MAX_PLAYLIST),
        )?,
    };
    Ok(page.items)
}

/// A show plays on from the first episode not yet watched, or from the
/// start when all are.
fn from_next_unplayed(items: &mut Vec<BaseItemDto>) {
    if let Some(first) = items.iter().position(|item| !item.played()) {
        items.drain(..first);
    }
}

/// What playing `item` plays, in order: shuffled too, since the window
/// shuffles it (`PlayerController::open_shuffled`). Blocks.
fn playable_list(
    client: &Client,
    item: &BaseItemDto,
    mode: Mode,
) -> Result<Vec<BaseItemDto>, Error> {
    let mut items = if is_playable(item.kind) {
        vec![item.clone()]
    } else {
        children(client, item)?
    };
    // Missing episodes the server knows of only from metadata.
    items.retain(|item| is_playable(item.kind) && item.location_type != LocationType::Virtual);
    if mode
        == (Mode::Play {
            from_start: false,
            shuffle: false,
        })
        && matches!(item.kind, ItemKind::Series | ItemKind::Season)
    {
        from_next_unplayed(&mut items);
    }
    Ok(items)
}

/// The episodes after `episode`, which play on after it: the rest of its
/// season and the seasons after, in the order the server gives (seasons
/// and episodes by number). Blocks.
fn next_episodes(client: &Client, episode: &BaseItemDto) -> Result<Vec<BaseItemDto>, Error> {
    let Some(series) = &episode.series_id else {
        return Ok(Vec::new());
    };
    // From the episode on; a server that doesn't know the parameter
    // gives them all, which `following` cuts.
    let page = client.episodes(
        series,
        &ItemsQuery::new()
            .param("startItemId", &episode.id)
            .limit(MAX_PLAYLIST),
    )?;
    Ok(following(episode, page.items))
}

/// Of a show's episodes, those after `episode` that can be played. Should
/// the list not hold the episode, those numbered after it.
fn following(episode: &BaseItemDto, mut episodes: Vec<BaseItemDto>) -> Vec<BaseItemDto> {
    let place = |item: &BaseItemDto| {
        (
            item.parent_index_number.unwrap_or(0),
            item.index_number.unwrap_or(0),
        )
    };
    let after = match episodes.iter().position(|item| item.id == episode.id) {
        Some(at) => episodes.split_off(at + 1),
        None => {
            episodes.sort_by_key(place);
            episodes.retain(|item| place(item) > place(episode));
            episodes
        }
    };
    after
        .into_iter()
        .filter(|item| is_playable(item.kind) && item.location_type != LocationType::Virtual)
        .collect()
}

/// The title the playlist and the title bar show for an item.
pub(super) fn display_title(item: &BaseItemDto) -> String {
    match (item.kind, item.series_name.as_deref()) {
        (ItemKind::Episode, Some(series)) => joined(&[series, &episode_number(item), &item.name]),
        _ => item.name.clone(),
    }
}

/// The registry's entry for an item opened as `mode`.
fn item_info(server_id: &str, item: &BaseItemDto, mode: Mode, choice: Option<&Choice>) -> ItemInfo {
    let audio = item.media_type == MediaKind::Audio || item.kind == ItemKind::Audio;
    let from_start = matches!(
        mode,
        Mode::Play {
            from_start: true,
            ..
        } | Mode::Play { shuffle: true, .. }
    );
    let resume_ticks = match item.user_data.as_ref() {
        Some(data) if !from_start && !audio => data.playback_position_ticks.max(0),
        _ => 0,
    };
    ItemInfo {
        server_id: server_id.to_owned(),
        id: item.id.clone(),
        title: display_title(item),
        kind: item.kind,
        audio,
        resume_ticks,
        from_start,
        media_source_id: choice.and_then(|choice| choice.version.clone()),
        audio_stream_index: choice.and_then(|choice| choice.audio),
        subtitle_stream_index: choice.and_then(|choice| choice.subtitle),
        opened: 0,
    }
}

/// A card's or track's marks, as the user data gives them.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Marks {
    played: bool,
    favorite: bool,
    progress: f32,
    unplayed: i32,
}

impl Marks {
    fn of(item: &BaseItemDto) -> Self {
        Self {
            played: item.played(),
            favorite: item.is_favorite(),
            progress: progress(item),
            unplayed: unplayed(item),
        }
    }

    /// The marks after the server's answer to a change.
    fn from_data(item: &BaseItemDto, data: &UserItemDataDto) -> Self {
        let mut item = item.clone();
        item.user_data = Some(data.clone());
        Self::of(&item)
    }
}

/// Where an image goes when it arrives. Each names the item it is for, so
/// that a row that changed meanwhile isn't given another's.
#[derive(Debug, Clone)]
enum Slot {
    /// A card of a row: Home's, search's, or one of an item page's
    /// (`detail`).
    Card {
        detail: bool,
        row: String,
        index: usize,
        id: String,
    },
    /// The grid's `index`th card, wherever its line is now.
    Grid {
        index: usize,
        id: String,
    },
    Track {
        index: usize,
        id: String,
    },
    Person {
        index: usize,
        id: String,
    },
    Poster(String),
    Backdrop(String),
    Logo(String),
    User,
}

/// The grid's items as cards, without images, and the image each shows:
/// only the lines near those in sight hold one (`window`), since a decoded
/// image for each of thousands of items ran to gigabytes.
#[derive(Debug, Default)]
struct GridState {
    cards: Vec<LibraryCard>,
    pictures: Vec<Option<Picture>>,
    total: usize,
    /// The cards in sight, as the view last said, by index.
    shown: (usize, usize),
    /// The cards that hold their images, by index: `start..end`.
    window: (usize, usize),
}

pub struct LibraryController {
    session: Rc<Session>,
    window: PlayerWindow,
    player: Weak<PlayerController>,
    this: Weak<LibraryController>,
    /// The server this window shows, by id.
    server: RefCell<Option<String>>,
    /// A playback's end changed what the page shows while the window
    /// still played: it loads again once the window shows it
    /// (`shown_again`).
    stale: Cell<bool>,
    me: RefCell<Option<UserDto>>,
    /// The account's libraries, as the bar lists them.
    views: RefCell<Vec<BaseItemDto>>,
    pages: RefCell<Pages>,
    /// Counts the pages shown: a load's answer for an earlier one is
    /// dropped. The load threads read it too, to skip a request for a page
    /// left before it went out.
    ticket: Cell<u64>,
    shared_ticket: Arc<AtomicU64>,
    /// Counts what this window was asked to play: an answer for an earlier
    /// ask, or one that comes after the window opened something else, is
    /// dropped.
    play_request: Cell<u64>,
    /// Latest rows of Home still on their way, with My Media held back
    /// until they have come, so that it doesn't move down under the
    /// pointer as each arrives above it.
    latest_pending: Cell<usize>,
    held_libraries: RefCell<Option<Vec<BaseItemDto>>>,
    /// The item of the card an item's page was opened from, to show while
    /// the page loads.
    opening: RefCell<Option<BaseItemDto>>,
    /// The same for a show's episodes, which change with its season.
    tracks_ticket: Cell<u64>,
    /// Loads of this page under way, and the first that failed.
    pending: Cell<usize>,
    failure: RefCell<Option<Error>>,
    /// The page's images, its tracks' and the bar's: each dropped when
    /// what they are for goes.
    generation: Generation,
    tracks_generation: Generation,
    bar_generation: Generation,
    columns: Cell<usize>,
    rows: Rc<VecModel<LibraryRow>>,
    detail_rows: Rc<VecModel<LibraryRow>>,
    tracks: Rc<VecModel<LibraryTrack>>,
    people: Rc<VecModel<LibraryPerson>>,
    nav: Rc<VecModel<LibraryNav>>,
    nav_end: Rc<VecModel<LibraryNav>>,
    tabs: Rc<VecModel<LibraryChoice>>,
    sorts: Rc<VecModel<LibraryChoice>>,
    servers: Rc<VecModel<LibraryChoice>>,
    /// Where rows go, by id: those of a page arrive in any order.
    rank: RefCell<HashMap<String, usize>>,
    grid: RefCell<GridState>,
    /// Every item on the page, by id: what playing or marking one needs.
    items: RefCell<HashMap<String, BaseItemDto>>,
    /// The item of the page shown, when it is an item's.
    detail: RefCell<Option<BaseItemDto>>,
    choice: RefCell<Choice>,
    search_timer: Timer,
    sign_in: RefCell<Option<Rc<SignIn>>>,
    /// The server signing in again, after its sign-in was lost.
    renewing: RefCell<Option<String>>,
}

impl LibraryController {
    pub fn new(
        session: &Rc<Session>,
        window: &PlayerWindow,
        player: Weak<PlayerController>,
    ) -> Rc<Self> {
        let controller = Rc::new_cyclic(|this| Self {
            session: session.clone(),
            window: window.clone_strong(),
            player,
            this: this.clone(),
            server: RefCell::new(None),
            stale: Cell::new(false),
            me: RefCell::new(None),
            views: RefCell::new(Vec::new()),
            pages: RefCell::new(Pages::default()),
            ticket: Cell::new(0),
            shared_ticket: Arc::new(AtomicU64::new(0)),
            play_request: Cell::new(0),
            latest_pending: Cell::new(0),
            held_libraries: RefCell::new(None),
            opening: RefCell::new(None),
            tracks_ticket: Cell::new(0),
            pending: Cell::new(0),
            failure: RefCell::new(None),
            generation: Generation::default(),
            tracks_generation: Generation::default(),
            bar_generation: Generation::default(),
            columns: Cell::new(DEFAULT_COLUMNS),
            rows: Rc::new(VecModel::default()),
            detail_rows: Rc::new(VecModel::default()),
            tracks: Rc::new(VecModel::default()),
            people: Rc::new(VecModel::default()),
            nav: Rc::new(VecModel::default()),
            nav_end: Rc::new(VecModel::default()),
            tabs: Rc::new(VecModel::default()),
            sorts: Rc::new(VecModel::default()),
            servers: Rc::new(VecModel::default()),
            rank: RefCell::new(HashMap::new()),
            grid: RefCell::new(GridState::default()),
            items: RefCell::new(HashMap::new()),
            detail: RefCell::new(None),
            choice: RefCell::new(Choice::default()),
            search_timer: Timer::default(),
            sign_in: RefCell::new(None),
            renewing: RefCell::new(None),
        });
        let library = controller.library();
        library.set_rows(ModelRc::from(controller.rows.clone()));
        library.set_detail_rows(ModelRc::from(controller.detail_rows.clone()));
        library.set_tracks(ModelRc::from(controller.tracks.clone()));
        library.set_people(ModelRc::from(controller.people.clone()));
        library.set_nav(ModelRc::from(controller.nav.clone()));
        library.set_nav_end(ModelRc::from(controller.nav_end.clone()));
        library.set_tabs(ModelRc::from(controller.tabs.clone()));
        library.set_sorts(ModelRc::from(controller.sorts.clone()));
        library.set_servers(ModelRc::from(controller.servers.clone()));
        controller.wire();
        let listener: Weak<dyn Listener> = Rc::downgrade(&controller) as Weak<dyn Listener>;
        session.subscribe(listener);
        controller.refresh_session();
        controller
    }

    fn library(&self) -> Library<'_> {
        self.window.global::<Library>()
    }

    fn account(&self) -> Option<Account> {
        let server = self.server.borrow().clone()?;
        self.session.account(&server)
    }

    fn top(&self) -> Option<Page> {
        self.pages.borrow().top().cloned()
    }

    fn wire(&self) {
        let library = self.library();
        macro_rules! on {
            ($method:ident, |$this:ident $(, $arg:ident)*| $body:expr) => {{
                let weak = self.this.clone();
                library.$method(move |$($arg),*| {
                    if let Some($this) = weak.upgrade() {
                        $body
                    }
                });
            }};
        }
        on!(on_columns, |this, columns| this
            .columns_changed(usize::try_from(columns).unwrap_or(0)));
        on!(on_load_more, |this| this.load_more());
        on!(on_lines_shown, |this, first, last| this.lines_shown(
            usize::try_from(first).unwrap_or(0),
            usize::try_from(last).unwrap_or(0)
        ));
        on!(on_navigate, |this, id| this.navigate(&id));
        on!(on_back, |this| this.back());
        on!(on_open, |this, id, kind| this.open(&id, &kind));
        on!(on_play, |this, id, from_start| this.start_playing(
            &id,
            Mode::Play {
                from_start,
                shuffle: false
            }
        ));
        on!(on_shuffle, |this, id| this.start_playing(
            &id,
            Mode::Play {
                from_start: true,
                shuffle: true
            }
        ));
        on!(on_queue, |this, id, next| this
            .start_playing(&id, Mode::Queue { next }));
        on!(on_set_played, |this, id, played| this
            .set_played(&id, played));
        on!(on_set_favorite, |this, id, favorite| this
            .set_favorite(&id, favorite));
        on!(on_select_tab, |this, id| this.select_tab(&id));
        on!(on_sort_by, |this, id, descending| this
            .sort_by(&id, descending));
        on!(on_filter, |this, unplayed, favorites| this
            .filter(unplayed, favorites));
        on!(on_search, |this, text| this
            .search_changed(text.to_string()));
        on!(on_choose, |this, what, id| this.choose(&what, &id));
        on!(on_retry, |this| this.retry());
        on!(on_connect, |this, address| {
            if let Some(flow) = this.flow() {
                flow.connect(&address);
            }
        });
        on!(on_scan, |this| {
            if let Some(flow) = this.flow() {
                flow.scan();
            }
        });
        on!(on_pick_server, |this, address| {
            if let Some(flow) = this.flow() {
                flow.pick_found(&address);
            }
        });
        on!(on_pick_saved, |this, id| {
            if let Some(server) = this.session.server(&id) {
                this.begin_sign_in_to(Some(&server), "");
            }
        });
        on!(on_pick_user, |this, id| {
            if let Some(flow) = this.flow() {
                flow.pick_user(&id);
            }
        });
        on!(on_sign_in_with_password, |this, name, password| {
            if let Some(flow) = this.flow() {
                flow.password(&name, &password);
            }
        });
        on!(on_quick_connect, |this| {
            if let Some(flow) = this.flow() {
                flow.quick_connect();
            }
        });
        on!(on_cancel_sign_in, |this| this.cancel_sign_in());
        on!(on_add_server, |this| this.begin_sign_in());
        on!(on_switch_server, |this, id| this.switch_server(&id));
        on!(on_sign_out, |this| {
            let server = this.server.borrow().clone();
            if let Some(server) = server {
                let name = this
                    .session
                    .server(&server)
                    .map(|server| server.name)
                    .unwrap_or_default();
                this.session.sign_out(&server);
                // It takes effect at once, with no confirmation; this says
                // what happened.
                this.notify(&format!("Signed Out of {name}"), "");
            }
        });
    }

    // The session.

    /// Follows the session: keeps this window's server while it is signed
    /// in, or takes the one new windows get.
    fn refresh_session(&self) {
        let current = self.server.borrow().clone();
        let kept = current
            .clone()
            .filter(|id| self.session.account(id).is_some());
        let next = kept.or_else(|| self.session.default_server());
        if next != current {
            *self.server.borrow_mut() = next;
            self.server_changed();
        } else if next.is_none() && self.sign_in.borrow().is_none() {
            // Still waiting for the saved sign-ins, or none came back.
            self.pages.borrow_mut().set(if self.session.loading() {
                vec![Page::Home]
            } else {
                Vec::new()
            });
            self.set_nav();
            self.show_page();
        }
        self.update_servers();
        self.update_shown();
    }

    /// Shows the server chosen for this window from its Home, or nothing
    /// without one, leaving signing in on top if it is under way.
    fn server_changed(&self) {
        self.bar_generation.bump();
        self.me.borrow_mut().take();
        self.views.borrow_mut().clear();
        let library = self.library();
        library.set_user_image(Image::default());
        let signing_in = self.sign_in.borrow().is_some();
        match self.account() {
            Some(account) => {
                library.set_signed_in(true);
                library.set_server_name(account.server.name.clone().into());
                library.set_user_name(account.server.user_name.clone().into());
                self.set_nav();
                self.load_me(&account);
                let mut pages = self.pages.borrow_mut();
                pages.go_to(None);
                if signing_in {
                    pages.push(Page::SignIn, 0.0);
                }
            }
            None => {
                library.set_signed_in(false);
                library.set_server_name(SharedString::new());
                library.set_user_name(SharedString::new());
                self.set_nav();
                self.pages.borrow_mut().set(if signing_in {
                    vec![Page::SignIn]
                } else if self.session.loading() {
                    vec![Page::Home]
                } else {
                    Vec::new()
                });
            }
        }
        self.show_page();
    }

    /// The library shows in place of the welcome screen while a server is
    /// signed in, while signing in, and while the saved sign-ins load.
    fn update_shown(&self) {
        let shown = self.server.borrow().is_some()
            || self.sign_in.borrow().is_some()
            || self.session.loading();
        self.library().set_shown(shown);
    }

    fn update_servers(&self) {
        let current = self.server.borrow().clone();
        let others: Vec<LibraryChoice> = self
            .session
            .accounts()
            .into_iter()
            .filter(|account| Some(&account.server.id) != current.as_ref())
            .map(|account| LibraryChoice {
                id: account.server.id.into(),
                title: account.server.name.into(),
                detail: account.server.user_name.into(),
            })
            .collect();
        self.servers.set_vec(others);
    }

    fn switch_server(&self, id: &str) {
        if self.session.account(id).is_none() {
            return;
        }
        self.session.choose(id);
        self.cancel_flow();
        *self.server.borrow_mut() = Some(id.to_owned());
        self.server_changed();
        self.update_servers();
        self.update_shown();
    }

    /// The bar: Home, the account's libraries, Search and This Computer;
    /// while the saved sign-ins load, Home and This Computer, so that a
    /// keyring slow to answer doesn't keep the user from their files; and
    /// nothing without a server.
    fn set_nav(&self) {
        let nav = |id: &str, title: &str, kind: &str| LibraryNav {
            id: id.into(),
            title: title.into(),
            kind: kind.into(),
        };
        let signed_in = self.account().is_some();
        if !signed_in && !self.session.loading() {
            self.nav.set_vec(Vec::new());
            self.nav_end.set_vec(Vec::new());
            return;
        }
        let mut entries = vec![nav("home", "Home", "home")];
        let mut end = Vec::new();
        if signed_in {
            for view in self.views.borrow().iter() {
                if let Some(collection) = Collection::of(view.collection_type) {
                    entries.push(nav(&view.id, &view.name, collection.nav_kind()));
                }
            }
            end.push(nav("search", "Search", "search"));
        }
        end.push(nav("local", "This Computer", "local"));
        self.nav.set_vec(entries);
        self.nav_end.set_vec(end);
    }

    /// The account's name and picture.
    fn load_me(&self, account: &Account) {
        let client = account.client.clone();
        let asked = account.client.clone();
        let server_id = account.server.id.clone();
        let weak = self.this.clone();
        background(
            "jellyfin",
            move || client.me(),
            move |me| {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                if this.server.borrow().as_deref() != Some(&server_id) {
                    return;
                }
                match me {
                    Ok(me) => this.me_arrived(me),
                    Err(Error::Unauthorized) => this.session.unauthorized(&server_id, &asked),
                    Err(error) => log::debug!("no Jellyfin account details: {error}"),
                }
            },
        );
    }

    fn me_arrived(&self, me: UserDto) {
        let library = self.library();
        library.set_user_name(me.name.clone().into());
        if let (Some(tag), Some(account)) = (&me.primary_image_tag, self.account()) {
            let pixels = self.pixels(USER_WIDTH);
            let url = account
                .client
                .user_image_url(&me.id, Some(tag), Some(pixels));
            let weak = self.this.clone();
            let loaded = self.session.images().load(
                &self.bar_generation,
                url.as_str(),
                Some(pixels),
                move |image| {
                    if let Some(this) = weak.upgrade() {
                        this.image_arrived(&Slot::User, image);
                    }
                },
            );
            if let Some(image) = loaded {
                library.set_user_image(image);
            }
        }
        *self.me.borrow_mut() = Some(me);
    }

    // Pages.

    /// Shows the page on top, loading it afresh. The page's list goes
    /// where `scroll-offset` says as its content arrives (`loads`).
    fn show_page(&self) {
        self.show_page_keeping(Kept::default());
    }

    /// `show_page`, keeping what `kept` holds of the page as it was.
    fn show_page_keeping(&self, kept: Kept) {
        self.next_ticket();
        let library = self.library();
        library.set_loads(library.get_loads().wrapping_add(1));
        library.set_tracks_note(SharedString::new());
        self.latest_pending.set(0);
        self.held_libraries.borrow_mut().take();
        let page = self.top();
        library.set_page(page.as_ref().map_or("", Page::name).into());
        library.set_nav_current(self.pages.borrow().nav_current().into());
        self.update_back();
        library.set_error(SharedString::new());
        library.set_title(SharedString::new());
        library.set_loading(false);
        library.set_more(false);
        library.set_tab(SharedString::new());
        library.set_detail(LibraryDetail::default());
        self.rows.set_vec(Vec::new());
        self.detail_rows.set_vec(Vec::new());
        self.tracks.set_vec(Vec::new());
        self.people.set_vec(Vec::new());
        self.tabs.set_vec(Vec::new());
        self.sorts.set_vec(Vec::new());
        self.rank.borrow_mut().clear();
        *self.grid.borrow_mut() = GridState::default();
        self.detail.borrow_mut().take();
        *self.choice.borrow_mut() = kept.choice;
        let Some(page) = page else {
            return;
        };
        if self.account().is_none() && !matches!(page, Page::SignIn | Page::Local) {
            // The saved sign-ins are still on their way.
            library.set_loading(self.session.loading());
            return;
        }
        match page {
            Page::Home => self.load_home(),
            Page::Grid(page) => self.load_grid(&page, kept.cards),
            Page::Detail(page) => self.load_detail(&page),
            Page::Search => {
                library.set_title("Search".into());
                let text = library.get_search_text().to_string();
                self.run_search(&text);
            }
            Page::Local => library.set_title("This Computer".into()),
            Page::SignIn => {
                library.set_title("Sign In".into());
                if let Some(flow) = self.flow() {
                    library.set_sign_in(sign_in_model(&flow.state()));
                }
            }
        }
    }

    /// Try Again: the spinner shows for a moment before the page loads
    /// again, so that a server that still refuses at once is seen to have
    /// been asked.
    fn retry(&self) {
        let library = self.library();
        library.set_error(SharedString::new());
        library.set_loading(true);
        let ticket = self.next_ticket();
        let weak = self.this.clone();
        Timer::single_shot(Duration::from_millis(500), move || {
            if let Some(this) = weak.upgrade()
                && this.ticket.get() == ticket
            {
                this.show_page();
            }
        });
    }

    /// Loads the page again, as it may have changed: after something
    /// played, whose progress the server now has. What the user chose on
    /// an item's page (its version, audio and subtitle) stays, and the
    /// item shows as it was until it comes again; a grid asks at once for
    /// as many cards as it had, so that it can stay where it was scrolled
    /// to.
    pub fn refresh(&self) {
        let Some(page) = self.top() else {
            return;
        };
        if !matches!(
            page,
            Page::Home | Page::Grid(_) | Page::Detail(_) | Page::Search
        ) {
            return;
        }
        let kept = Kept {
            choice: self.choice.borrow().clone(),
            cards: self.grid.borrow().cards.len(),
        };
        *self.opening.borrow_mut() = self.detail.borrow().clone();
        self.show_page_keeping(kept);
        self.opening.borrow_mut().take();
    }

    /// The window has nothing to play again, and shows the library: the
    /// page loads again, as what played may have moved on the server, once
    /// the server has taken the report of a Jellyfin item's end
    /// (`stopping`), which `Change::Played` says. That may have come while
    /// the window was still stopping, as mpv's stop takes longer than a
    /// server near by takes to answer.
    pub fn shown_again(&self, stopping: bool) {
        if self.stale.replace(false) || !stopping {
            self.refresh();
        }
    }

    fn update_back(&self) {
        let signing_in = self.top() == Some(Page::SignIn);
        let step_back = signing_in
            && self
                .flow()
                .is_some_and(|flow| flow.state().step != session::Step::Address);
        self.library()
            .set_can_go_back(self.pages.borrow().can_go_back() || step_back);
    }

    /// A new page: what was asked for the last one is dropped as it comes.
    fn next_ticket(&self) -> u64 {
        let ticket = self.ticket.get() + 1;
        self.ticket.set(ticket);
        self.shared_ticket.store(ticket, Ordering::Relaxed);
        self.tracks_ticket.set(self.tracks_ticket.get() + 1);
        self.generation.bump();
        self.tracks_generation.bump();
        self.pending.set(0);
        self.failure.borrow_mut().take();
        self.items.borrow_mut().clear();
        ticket
    }

    /// Runs `work` with this window's client on a load thread, as a load
    /// of the page shown now: the spinner shows until every load is
    /// done, and an answer that comes after the page was left (or, with
    /// `tracks`, after another season was chosen) is dropped.
    fn load<T: Send + 'static>(
        &self,
        tracks: bool,
        work: impl FnOnce(&Client) -> Result<T, Error> + Send + 'static,
        done: impl FnOnce(&LibraryController, T) + 'static,
    ) {
        let Some(account) = self.account() else {
            return;
        };
        let ticket = self.ticket.get();
        let tracks_ticket = self.tracks_ticket.get();
        self.pending.set(self.pending.get() + 1);
        self.library().set_loading(true);
        let weak = self.this.clone();
        let server_id = account.server.id;
        let client = account.client;
        let asked = client.clone();
        let current = self.shared_ticket.clone();
        session::load(
            // A page left before its turn came asks nothing.
            move || (current.load(Ordering::Relaxed) == ticket).then(|| work(&client)),
            move |result| {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                let Some(result) = result else {
                    return;
                };
                if this.ticket.get() != ticket {
                    return;
                }
                this.pending.set(this.pending.get().saturating_sub(1));
                if !tracks || this.tracks_ticket.get() == tracks_ticket {
                    match result {
                        Ok(value) => done(&this, value),
                        Err(error) => this.load_failed(&server_id, &asked, error),
                    }
                }
                if this.pending.get() == 0 {
                    this.loads_done();
                }
            },
        );
    }

    fn load_failed(&self, server_id: &str, asked: &Client, error: Error) {
        if error == Error::Unauthorized {
            self.session.unauthorized(server_id, asked);
            return;
        }
        log::warn!("Jellyfin: {error}");
        self.failure.borrow_mut().get_or_insert(error);
    }

    /// Says what went wrong when a page came to nothing, naming the server.
    fn loads_done(&self) {
        let library = self.library();
        library.set_loading(false);
        let empty = self.rows.row_count() == 0 && !library.get_detail().loaded;
        if let Some(error) = self.failure.borrow().as_ref()
            && empty
        {
            let name = self.account().map(|account| account.server.name);
            library.set_error(error.about(name.as_deref()).into());
        }
    }

    fn navigate(&self, id: &str) {
        // While the saved sign-ins load, This Computer is still there.
        if self.account().is_none() && !(self.session.loading() && matches!(id, "home" | "local")) {
            return;
        }
        let page = match id {
            "home" => None,
            "search" => Some(Page::Search),
            "local" => Some(Page::Local),
            id => {
                let view = self
                    .views
                    .borrow()
                    .iter()
                    .find(|view| view.id == id)
                    .cloned();
                let Some(view) = view else {
                    log::debug!("no library {id}");
                    return;
                };
                let collection =
                    Collection::of(view.collection_type).unwrap_or(Collection::Folders);
                Some(Page::Grid(GridPage::new(
                    &view.id, &view.id, &view.name, collection,
                )))
            }
        };
        self.cancel_flow();
        self.leave_error();
        self.pages.borrow_mut().go_to(page);
        self.show_page();
        self.update_shown();
    }

    fn back(&self) {
        if self.top() == Some(Page::SignIn) {
            if let Some(flow) = self.flow()
                && flow.back()
            {
                self.update_back();
                return;
            }
            self.cancel_sign_in();
            return;
        }
        if self.pages.borrow_mut().pop() {
            self.leave_error();
            self.restore_scroll();
            self.show_page();
        }
    }

    /// Has the page on top start where it was scrolled to when it was
    /// left (the view reads it as the page appears).
    fn restore_scroll(&self) {
        let scroll = self.pages.borrow().scroll();
        self.library().set_scroll_offset(scroll);
    }

    /// Opens a page over the one shown, keeping where that one is
    /// scrolled to for Back.
    fn push(&self, page: Page) {
        let scroll = self.library().get_scroll_offset();
        self.pages.borrow_mut().push(page, scroll);
    }

    /// Takes down what the window says about a file that failed to open
    /// once the user moves on in the library: it would stay over every
    /// page until something else is played.
    fn leave_error(&self) {
        self.window.set_error(SharedString::new());
    }

    /// Opens an item's page of a server's, over the page shown: the album
    /// or artist of a song that plays in another window (Go to Album).
    pub fn go_to(&self, server_id: &str, id: &str, kind: &str) {
        if self.server.borrow().as_deref() != Some(server_id) {
            self.switch_server(server_id);
        }
        if self.server.borrow().as_deref() != Some(server_id) {
            return;
        }
        self.open(id, kind);
        if self.views.borrow().is_empty() {
            self.load_views();
        }
    }

    /// The account's libraries for the bar, when the first page shown is
    /// not Home, which asks for them itself, and whose loads stop when it
    /// is left (a new window opened at an album).
    fn load_views(&self) {
        let Some(account) = self.account() else {
            return;
        };
        let weak = self.this.clone();
        let server_id = account.server.id;
        let client = account.client;
        session::load(
            move || client.views(),
            move |result| {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                let known = !this.views.borrow().is_empty();
                if known || this.server.borrow().as_deref() != Some(server_id.as_str()) {
                    return;
                }
                match result {
                    Ok(views) => {
                        *this.views.borrow_mut() = views
                            .items
                            .into_iter()
                            .filter(|view| Collection::of(view.collection_type).is_some())
                            .collect();
                        this.set_nav();
                    }
                    Err(error) => log::debug!("no libraries for the bar: {error}"),
                }
            },
        );
    }

    fn open(&self, id: &str, kind: &str) {
        if self.account().is_none() || id.is_empty() {
            return;
        }
        let known = self.items.borrow().get(id).cloned();
        let kind = known
            .as_ref()
            .map(|item| item.kind)
            .filter(|kind| *kind != ItemKind::Unknown)
            .unwrap_or_else(|| item_kind(kind));
        let root = self.pages.borrow().nav_current();
        let title = known
            .as_ref()
            .map(|item| item.name.clone())
            .unwrap_or_default();
        let page = match kind {
            ItemKind::CollectionFolder | ItemKind::UserView => {
                let view = self
                    .views
                    .borrow()
                    .iter()
                    .find(|view| view.id == id)
                    .cloned();
                let collection = view
                    .as_ref()
                    .and_then(|view| Collection::of(view.collection_type))
                    .unwrap_or(Collection::Folders);
                let title = view.map_or(title, |view| view.name);
                Page::Grid(GridPage::new(id, id, &title, collection))
            }
            kind if is_folder_kind(kind) => {
                Page::Grid(GridPage::new(id, &root, &title, Collection::Folders))
            }
            // A song has no page of its own.
            ItemKind::Audio => {
                self.start_playing(
                    id,
                    Mode::Play {
                        from_start: false,
                        shuffle: false,
                    },
                );
                return;
            }
            kind => Page::Detail(DetailPage {
                id: id.to_owned(),
                kind,
                root,
                season: None,
            }),
        };
        *self.opening.borrow_mut() = known;
        self.leave_error();
        self.push(page);
        self.show_page();
        self.opening.borrow_mut().take();
    }

    // Home.

    fn load_home(&self) {
        self.library().set_title("Home".into());
        {
            let mut rank = self.rank.borrow_mut();
            rank.insert("resume".into(), 0);
            rank.insert("next-up".into(), 1);
            rank.insert("libraries".into(), usize::MAX);
        }
        self.load(
            false,
            |client| client.resume(&resume_query()),
            |this, page| {
                this.add_row(
                    "resume",
                    "Continue Watching",
                    &page.items,
                    Art::Thumb,
                    Lines::Home,
                    false,
                )
            },
        );
        self.load(
            false,
            |client| client.next_up(&next_up_query()),
            |this, page| {
                this.add_row(
                    "next-up",
                    "Next Up",
                    &page.items,
                    Art::Thumb,
                    Lines::Home,
                    false,
                )
            },
        );
        self.load(
            false,
            |client| client.views(),
            |this, views| this.views_arrived(views.items),
        );
    }

    /// The libraries: the bar's entries, each library's newest on Home,
    /// and My Media.
    fn views_arrived(&self, views: Vec<BaseItemDto>) {
        let views: Vec<BaseItemDto> = views
            .into_iter()
            .filter(|view| Collection::of(view.collection_type).is_some())
            .collect();
        *self.views.borrow_mut() = views.clone();
        self.set_nav();
        let excluded: Vec<String> = self
            .me
            .borrow()
            .as_ref()
            .map(|me| me.configuration.latest_items_excludes.clone())
            .unwrap_or_default();
        *self.held_libraries.borrow_mut() = Some(views.clone());
        let mut pending = 0;
        for (index, view) in views.iter().enumerate() {
            let has_latest =
                Collection::of(view.collection_type).is_some_and(Collection::has_latest);
            if !has_latest || excluded.contains(&view.id) {
                continue;
            }
            let row = format!("latest-{}", view.id);
            self.rank.borrow_mut().insert(row.clone(), 2 + index);
            let title = format!("Latest in {}", view.name);
            let parent = view.id.clone();
            pending += 1;
            // Answered either way, so that My Media comes when the last of
            // them has.
            self.load(
                false,
                move |client| Ok::<_, Error>(client.latest(&latest_query(&parent))),
                move |this, answer| {
                    match answer {
                        Ok(items) => {
                            this.add_row(&row, &title, &items, Art::Poster, Lines::Home, false)
                        }
                        Err(error) => {
                            if let Some(account) = this.account() {
                                this.load_failed(&account.server.id, &account.client, error);
                            }
                        }
                    }
                    this.latest_pending
                        .set(this.latest_pending.get().saturating_sub(1));
                    if this.latest_pending.get() == 0 {
                        this.add_libraries();
                    }
                },
            );
        }
        self.latest_pending.set(pending);
        if pending == 0 {
            self.add_libraries();
            return;
        }
        // A server slow to give one library's newest doesn't keep My Media
        // away for long.
        let ticket = self.ticket.get();
        let weak = self.this.clone();
        Timer::single_shot(Duration::from_secs(3), move || {
            if let Some(this) = weak.upgrade()
                && this.ticket.get() == ticket
            {
                this.add_libraries();
            }
        });
    }

    /// My Media, under the Latest rows, once they are in (or late).
    fn add_libraries(&self) {
        let Some(views) = self.held_libraries.borrow_mut().take() else {
            return;
        };
        self.add_row(
            "libraries",
            "My Media",
            &views,
            Art::Thumb,
            Lines::Own,
            false,
        );
    }

    /// Adds a row of cards where its rank puts it among those there; an
    /// empty row isn't added.
    fn add_row(
        &self,
        id: &str,
        title: &str,
        items: &[BaseItemDto],
        art: Art,
        lines: Lines,
        detail: bool,
    ) {
        if items.is_empty() {
            return;
        }
        let row_id = id.to_owned();
        let cards = self.cards(items, art, lines, |index, item| Slot::Card {
            detail,
            row: row_id.clone(),
            index,
            id: item.to_owned(),
        });
        let row = LibraryRow {
            id: id.into(),
            title: title.into(),
            cards: ModelRc::new(VecModel::from(cards)),
        };
        let model = if detail {
            &self.detail_rows
        } else {
            &self.rows
        };
        let rank = self.rank_of(id);
        let at = (0..model.row_count())
            .find(|index| {
                model
                    .row_data(*index)
                    .is_some_and(|row| self.rank_of(&row.id) > rank)
            })
            .unwrap_or(model.row_count());
        model.insert(at, row);
    }

    fn rank_of(&self, id: &str) -> usize {
        self.rank
            .borrow()
            .get(id)
            .copied()
            .unwrap_or(usize::MAX - 1)
    }

    /// Cards for items, each image going to `slot(index, id)` when it
    /// arrives; the items are kept for playing and marking.
    fn cards(
        &self,
        items: &[BaseItemDto],
        art: Art,
        lines: Lines,
        slot: impl Fn(usize, &str) -> Slot,
    ) -> Vec<LibraryCard> {
        let mut known = self.items.borrow_mut();
        items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                known.insert(item.id.clone(), item.clone());
                let mut card = card_of(item, art, lines);
                let width = if card.aspect > 1.2 {
                    THUMB_WIDTH
                } else {
                    POSTER_WIDTH
                };
                card.image = self.image(
                    picture(item, art).as_ref(),
                    width,
                    &self.generation,
                    slot(index, &item.id),
                );
                card
            })
            .collect()
    }

    /// Physical pixels for a width in logical ones.
    fn pixels(&self, width: f32) -> u32 {
        (width * self.window.window().scale_factor())
            .round()
            .max(1.0) as u32
    }

    /// The image to show at once: the image itself when it is in memory,
    /// or its blurhash until it arrives in `slot`.
    fn image(
        &self,
        picture: Option<&Picture>,
        width: f32,
        generation: &Generation,
        slot: Slot,
    ) -> Image {
        let (Some(picture), Some(account)) = (picture, self.account()) else {
            return Image::default();
        };
        let pixels = self.pixels(width);
        let url = account.client.image_url(
            &picture.item,
            picture.kind,
            picture.index,
            Some(&picture.tag),
            Some(pixels),
        );
        let weak = self.this.clone();
        let loaded =
            self.session
                .images()
                .load(generation, url.as_str(), Some(pixels), move |image| {
                    if let Some(this) = weak.upgrade() {
                        this.image_arrived(&slot, image);
                    }
                });
        loaded
            .or_else(|| {
                let hash = picture.blurhash.as_deref()?;
                let height = (100.0 / picture.aspect.max(0.1)).round() as u32;
                images::placeholder(hash, 100, height)
            })
            .unwrap_or_default()
    }

    fn image_arrived(&self, slot: &Slot, image: Image) {
        let library = self.library();
        match slot {
            Slot::Card {
                detail,
                row,
                index,
                id,
            } => {
                let model = if *detail {
                    &self.detail_rows
                } else {
                    &self.rows
                };
                if let Some(cards) = row_cards(model, row) {
                    change_card(&cards, *index, id, |card| card.image = image);
                }
            }
            Slot::Grid { index, id } => {
                {
                    let grid = self.grid.borrow();
                    let (start, end) = grid.window;
                    if !(start..end).contains(index)
                        || grid
                            .cards
                            .get(*index)
                            .is_none_or(|card| card.id != id.as_str())
                    {
                        return;
                    }
                }
                let (line, column) = place(*index, self.columns.get());
                if let Some(row) = self.rows.row_data(line) {
                    change_card(&row.cards, column, id, |card| card.image = image);
                }
            }
            Slot::Track { index, id } => {
                if let Some(mut track) = self.tracks.row_data(*index)
                    && track.id == id.as_str()
                {
                    track.image = image;
                    self.tracks.set_row_data(*index, track);
                }
            }
            Slot::Person { index, id } => {
                if let Some(mut person) = self.people.row_data(*index)
                    && person.id == id.as_str()
                {
                    person.image = image;
                    self.people.set_row_data(*index, person);
                }
            }
            Slot::Poster(id) | Slot::Backdrop(id) | Slot::Logo(id) => {
                let mut detail = library.get_detail();
                if detail.id != id.as_str() {
                    return;
                }
                match slot {
                    Slot::Poster(_) => detail.poster = image,
                    Slot::Backdrop(_) => detail.backdrop = image,
                    _ => detail.logo = image,
                }
                library.set_detail(detail);
            }
            Slot::User => library.set_user_image(image),
        }
    }

    // Grids.

    /// A grid's first cards: a page of them, or as many as `cards` when
    /// it loads again.
    fn load_grid(&self, page: &GridPage, cards: usize) {
        let library = self.library();
        library.set_title(page.title.clone().into());
        let choice = |id: &str, title: &str| LibraryChoice {
            id: id.into(),
            title: title.into(),
            detail: SharedString::new(),
        };
        self.tabs.set_vec(
            page.collection
                .tabs()
                .iter()
                .map(|tab| choice(tab.id(), tab.title()))
                .collect::<Vec<_>>(),
        );
        library.set_tab(page.tab.id().into());
        self.sorts.set_vec(
            page.tab
                .sorts()
                .iter()
                .map(|sort| choice(sort.id(), sort.title()))
                .collect::<Vec<_>>(),
        );
        library.set_sort(page.sort.id().into());
        library.set_descending(page.descending);
        library.set_unplayed_filter(page.tab.keeps_played());
        library.set_unplayed_only(page.unplayed && page.tab.keeps_played());
        library.set_favorites_only(page.favorites);
        self.load_grid_page(page, 0, first_cards(cards));
    }

    fn load_grid_page(&self, page: &GridPage, start: usize, limit: u32) {
        let request = grid_request(page, start as u32, limit);
        self.load(
            false,
            move |client| request.fetch(client),
            move |this, result| this.grid_arrived(start, limit, result),
        );
    }

    fn grid_arrived(&self, start: usize, limit: u32, result: QueryResult<BaseItemDto>) {
        if self.grid.borrow().cards.len() != start {
            return;
        }
        let count = result.items.len();
        // A short page is the last, whatever the count said.
        let total = if count < limit as usize {
            start + count
        } else {
            usize::try_from(result.total_record_count)
                .unwrap_or(0)
                .max(start + count)
        };
        let (cards, pictures): (Vec<LibraryCard>, Vec<Option<Picture>>) = {
            let mut known = self.items.borrow_mut();
            result
                .items
                .iter()
                .map(|item| {
                    known.insert(item.id.clone(), item.clone());
                    (
                        card_of(item, Art::Poster, Lines::Own),
                        picture(item, Art::Poster),
                    )
                })
                .unzip()
        };
        self.append_grid(cards, pictures);
        let loaded = {
            let mut grid = self.grid.borrow_mut();
            grid.total = total;
            grid.cards.len()
        };
        self.library().set_more(loaded < total);
        self.update_grid_images();
    }

    /// Adds cards to the grid: the last line first, if it isn't full, then
    /// new lines. The lines there stay as they are.
    fn append_grid(&self, cards: Vec<LibraryCard>, pictures: Vec<Option<Picture>>) {
        let columns = self.columns.get().max(1);
        let start = {
            let mut grid = self.grid.borrow_mut();
            let start = grid.cards.len();
            grid.cards.extend(cards.iter().cloned());
            grid.pictures.extend(pictures);
            start
        };
        append_lines(&self.rows, start, cards, columns);
    }

    fn columns_changed(&self, columns: usize) {
        if columns == 0 || columns == self.columns.get() {
            return;
        }
        self.columns.set(columns);
        if matches!(self.top(), Some(Page::Grid(_))) {
            let cards = {
                let mut grid = self.grid.borrow_mut();
                grid.window = (0, 0);
                grid.cards.clone()
            };
            self.rows.set_vec(lines(&cards, columns));
            self.update_grid_images();
        }
    }

    /// The view says which of the grid's lines are in sight.
    fn lines_shown(&self, first: usize, last: usize) {
        let columns = self.columns.get().max(1);
        let shown = (first * columns, (last.max(first) + 1) * columns);
        let changed = {
            let mut grid = self.grid.borrow_mut();
            std::mem::replace(&mut grid.shown, shown) != shown
        };
        if changed {
            self.update_grid_images();
        }
    }

    /// Gives the cards near those in sight their images, and takes them
    /// from those further away.
    fn update_grid_images(&self) {
        if !matches!(self.top(), Some(Page::Grid(_))) {
            return;
        }
        let columns = self.columns.get().max(1);
        let (old, new, wanted) = {
            let mut grid = self.grid.borrow_mut();
            let (first, end) = grid.shown;
            let screen = end.saturating_sub(first).max(columns);
            let margin = screen * GRID_IMAGE_SCREENS;
            // Whole lines, so that a line's cards come and go together.
            let start = first.saturating_sub(margin) / columns * columns;
            let end = (end + margin).min(grid.cards.len());
            let new = (start, end.max(start));
            let old = std::mem::replace(&mut grid.window, new);
            let wanted: Vec<(usize, LibraryCard, Option<Picture>)> = (new.0..new.1)
                .filter(|index| !(old.0..old.1).contains(index))
                .map(|index| {
                    (
                        index,
                        grid.cards[index].clone(),
                        grid.pictures[index].clone(),
                    )
                })
                .collect();
            (old, new, wanted)
        };
        for index in (old.0..old.1).filter(|index| !(new.0..new.1).contains(index)) {
            let (line, column) = place(index, columns);
            if let Some(row) = self.rows.row_data(line)
                && let Some(mut card) = row.cards.row_data(column)
                && card.image.size().width > 0
            {
                card.image = Image::default();
                row.cards.set_row_data(column, card);
            }
        }
        for (index, card, picture) in wanted {
            let id = card.id.to_string();
            let width = if card.aspect > 1.2 {
                THUMB_WIDTH
            } else {
                POSTER_WIDTH
            };
            let image = self.image(
                picture.as_ref(),
                width,
                &self.generation,
                Slot::Grid {
                    index,
                    id: id.clone(),
                },
            );
            let (line, column) = place(index, columns);
            if let Some(row) = self.rows.row_data(line) {
                change_card(&row.cards, column, &id, |card| card.image = image);
            }
        }
    }

    fn load_more(&self) {
        let Some(Page::Grid(page)) = self.top() else {
            return;
        };
        let (loaded, total) = {
            let grid = self.grid.borrow();
            (grid.cards.len(), grid.total)
        };
        if self.pending.get() == 0 && loaded < total {
            self.load_grid_page(&page, loaded, GRID_PAGE);
        }
    }

    /// Changes the grid on top and loads it again.
    fn change_grid(&self, change: impl FnOnce(&mut GridPage) -> bool) {
        let Some(Page::Grid(mut page)) = self.top() else {
            return;
        };
        if change(&mut page) {
            self.pages.borrow_mut().replace_top(Page::Grid(page));
            self.show_page();
        }
    }

    fn select_tab(&self, id: &str) {
        match self.top() {
            Some(Page::Grid(_)) => {
                let Some(tab) = Tab::from_id(id) else {
                    return;
                };
                self.change_grid(|page| {
                    if page.tab == tab || !page.collection.tabs().contains(&tab) {
                        return false;
                    }
                    page.tab = tab;
                    page.sort = Sort::Name;
                    page.descending = false;
                    true
                });
            }
            Some(Page::Detail(mut page)) if page.kind == ItemKind::Series => {
                page.season = Some(id.to_owned());
                let series = page.id.clone();
                self.pages.borrow_mut().replace_top(Page::Detail(page));
                self.library().set_tab(id.into());
                self.load_episodes(&series, id);
            }
            _ => {}
        }
    }

    fn sort_by(&self, id: &str, descending: bool) {
        let Some(sort) = Sort::from_id(id) else {
            return;
        };
        self.change_grid(|page| {
            if !page.tab.sorts().contains(&sort)
                || (page.sort == sort && page.descending == descending)
            {
                return false;
            }
            page.sort = sort;
            page.descending = descending;
            true
        });
    }

    fn filter(&self, unplayed: bool, favorites: bool) {
        self.change_grid(|page| {
            if page.unplayed == unplayed && page.favorites == favorites {
                return false;
            }
            page.unplayed = unplayed;
            page.favorites = favorites;
            true
        });
    }

    // Item pages.

    fn load_detail(&self, page: &DetailPage) {
        let library = self.library();
        // What the card it was opened from showed, until the item comes:
        // its title, its kind's look, and its image (mostly in memory
        // already). Nothing to do with it is offered until then.
        let opening = self
            .opening
            .borrow()
            .clone()
            .filter(|item| item.id == page.id);
        let preview = match opening {
            Some(item) => {
                let picture = primary(&item).or_else(|| picture(&item, Art::Poster));
                LibraryDetail {
                    id: page.id.clone().into(),
                    kind: card_kind(&item).into(),
                    title: item.name.clone().into(),
                    poster_aspect: picture
                        .as_ref()
                        .map_or_else(|| default_aspect(item.kind), |picture| picture.aspect),
                    poster: self.image(
                        picture.as_ref(),
                        DETAIL_POSTER_WIDTH,
                        &self.generation,
                        Slot::Poster(page.id.clone()),
                    ),
                    ..Default::default()
                }
            }
            None => LibraryDetail {
                id: page.id.clone().into(),
                ..Default::default()
            },
        };
        library.set_detail(preview);
        let id = page.id.clone();
        self.load(
            false,
            move |client| client.item(&id),
            |this, item| this.detail_arrived(item),
        );
        if page.kind != ItemKind::Unknown {
            self.load_children(page.kind, &page.id, page.season.clone());
        }
    }

    /// What an item's page shows under it, as far as its id alone tells.
    fn load_children(&self, kind: ItemKind, id: &str, season: Option<String>) {
        let id = id.to_owned();
        let row = |this: &LibraryController, row: &str, title: &str, items: &[BaseItemDto]| {
            this.add_row(row, title, items, Art::Poster, Lines::Own, true)
        };
        match kind {
            ItemKind::Series => {
                let series = id.clone();
                self.load(
                    false,
                    move |client| client.seasons(&series, &seasons_query()),
                    move |this, seasons| this.seasons_arrived(seasons.items, season),
                );
                self.similar(&id);
            }
            ItemKind::Movie | ItemKind::Video | ItemKind::MusicVideo | ItemKind::Trailer => {
                self.similar(&id)
            }
            ItemKind::MusicAlbum => self.load(
                false,
                move |client| client.items(&album_tracks_query(&id)),
                |this, page| {
                    this.set_tracks(&page.items, Tracks::Album);
                    this.note_tracks(&page);
                },
            ),
            ItemKind::Playlist => self.load(
                false,
                move |client| client.playlist_items(&id, &playlist_query()),
                |this, page| {
                    this.set_tracks(&page.items, Tracks::Playlist);
                    this.note_tracks(&page);
                },
            ),
            // The artist is the page's: each album says its year instead,
            // and each song its album.
            ItemKind::MusicArtist => {
                let artist = id.clone();
                self.load(
                    false,
                    move |client| client.items(&artist_albums_query(&artist)),
                    move |this, page| {
                        this.add_row(
                            "albums",
                            "Albums",
                            &page.items,
                            Art::Poster,
                            Lines::Artist,
                            true,
                        )
                    },
                );
                self.load(
                    false,
                    move |client| client.items(&artist_songs_query(&id)),
                    |this, page| {
                        this.set_tracks(&page.items, Tracks::Artist);
                        this.note_tracks(&page);
                    },
                );
            }
            ItemKind::BoxSet => self.load(
                false,
                move |client| client.items(&collection_items_query(&id)),
                |this, page| this.collection_arrived(page.items),
            ),
            ItemKind::MusicGenre | ItemKind::Genre => {
                let rows = [
                    ("albums", "Albums", ItemKind::MusicAlbum),
                    ("movies", "Movies", ItemKind::Movie),
                    ("shows", "Shows", ItemKind::Series),
                ];
                for (index, (row_id, title, kind)) in rows.into_iter().enumerate() {
                    self.rank.borrow_mut().insert(row_id.into(), index);
                    let id = id.clone();
                    self.load(
                        false,
                        move |client| client.items(&genre_query(&id, kind)),
                        move |this, page| row(this, row_id, title, &page.items),
                    );
                }
            }
            ItemKind::Person => {
                let rows = [
                    ("movies", "Movies", ItemKind::Movie),
                    ("shows", "Shows", ItemKind::Series),
                    ("episodes", "Episodes", ItemKind::Episode),
                ];
                for (index, (row_id, title, kind)) in rows.into_iter().enumerate() {
                    self.rank.borrow_mut().insert(row_id.into(), index);
                    let id = id.clone();
                    self.load(
                        false,
                        move |client| client.items(&person_query(&id, kind)),
                        move |this, page| row(this, row_id, title, &page.items),
                    );
                }
            }
            // A season's episodes and an episode's season need the item.
            _ => {}
        }
    }

    fn similar(&self, id: &str) {
        let id = id.to_owned();
        self.load(
            false,
            move |client| client.similar(&id, &similar_query()),
            |this, page| {
                this.add_row(
                    "similar",
                    "More Like This",
                    &page.items,
                    Art::Poster,
                    Lines::Own,
                    true,
                )
            },
        );
    }

    fn detail_arrived(&self, item: BaseItemDto) {
        let library = self.library();
        library.set_title(item.name.clone().into());
        self.items
            .borrow_mut()
            .insert(item.id.clone(), item.clone());
        *self.detail.borrow_mut() = Some(item.clone());
        let mut detail = detail_of(&item, &self.choice.borrow());
        let id = item.id.clone();
        detail.poster = self.image(
            primary(&item)
                .or_else(|| picture(&item, Art::Poster))
                .as_ref(),
            DETAIL_POSTER_WIDTH,
            &self.generation,
            Slot::Poster(id.clone()),
        );
        detail.backdrop = self.image(
            backdrop(&item).as_ref(),
            BACKDROP_WIDTH,
            &self.generation,
            Slot::Backdrop(id.clone()),
        );
        detail.logo = self.image(
            logo(&item).as_ref(),
            LOGO_WIDTH,
            &self.generation,
            Slot::Logo(id.clone()),
        );
        library.set_detail(detail);
        self.set_people(&item);

        let page_kind = match self.top() {
            Some(Page::Detail(page)) => page.kind,
            _ => return,
        };
        if page_kind == ItemKind::Unknown {
            if let Some(Page::Detail(mut page)) = self.top() {
                page.kind = item.kind;
                self.pages.borrow_mut().replace_top(Page::Detail(page));
            }
            self.load_children(item.kind, &id, None);
        }
        match item.kind {
            ItemKind::Season => {
                let (series, season) = (item.series_id.clone(), item.id.clone());
                self.load(
                    false,
                    move |client| match series {
                        Some(series) => client.episodes(&series, &episodes_query(&season)),
                        None => client.items(
                            &ItemsQuery::new()
                                .parent(&season)
                                .types(&[ItemKind::Episode])
                                .fields(&["Overview", "PrimaryImageAspectRatio"]),
                        ),
                    },
                    |this, page| this.set_tracks(&page.items, Tracks::Episodes),
                );
            }
            ItemKind::Episode => {
                if let (Some(series), Some(season)) =
                    (item.series_id.clone(), item.season_id.clone())
                {
                    let title = match &item.season_name {
                        Some(name) => format!("More from {name}"),
                        None => "More from This Season".to_owned(),
                    };
                    let episode = item.id.clone();
                    self.load(
                        false,
                        move |client| client.episodes(&series, &episodes_query(&season)),
                        move |this, mut page| {
                            page.items.retain(|other| other.id != episode);
                            this.add_row(
                                "season",
                                &title,
                                &page.items,
                                Art::Thumb,
                                Lines::Own,
                                true,
                            )
                        },
                    );
                }
            }
            ItemKind::MusicAlbum => {
                if let Some(artist) = item.album_artists.first().cloned() {
                    let album = item.id.clone();
                    self.load(
                        false,
                        move |client| client.items(&artist_albums_query(&artist.id)),
                        move |this, mut page| {
                            page.items.retain(|other| other.id != album);
                            this.add_row(
                                "artist",
                                &format!("More by {}", artist.name),
                                &page.items,
                                Art::Poster,
                                Lines::Artist,
                                true,
                            )
                        },
                    );
                }
            }
            _ => {}
        }
    }

    fn set_people(&self, item: &BaseItemDto) {
        let people: Vec<LibraryPerson> = item
            .people
            .iter()
            // An album lists its artists as people, who are its page's
            // parent already, and whose ids are not their artists'.
            .filter(|person| !matches!(person.kind.as_str(), "Artist" | "AlbumArtist"))
            .take(30)
            .enumerate()
            .map(|(index, person)| {
                let picture = person.primary_image_tag.as_ref().map(|tag| Picture {
                    item: person.id.clone(),
                    kind: ImageType::Primary,
                    index: None,
                    tag: tag.clone(),
                    blurhash: person
                        .image_blur_hashes
                        .get(ImageType::Primary.as_str())
                        .and_then(|hashes| hashes.get(tag))
                        .cloned(),
                    aspect: 2.0 / 3.0,
                });
                let role = match person.role.as_deref() {
                    Some(role) if !role.is_empty() => role.to_owned(),
                    _ => person_kind(&person.kind),
                };
                LibraryPerson {
                    id: person.id.clone().into(),
                    name: person.name.clone().into(),
                    role: role.into(),
                    image: self.image(
                        picture.as_ref(),
                        PERSON_WIDTH,
                        &self.generation,
                        Slot::Person {
                            index,
                            id: person.id.clone(),
                        },
                    ),
                }
            })
            .collect();
        self.people.set_vec(people);
    }

    /// A show's seasons, as tabs; the one chosen before, or the first with
    /// something left to watch, shows its episodes.
    fn seasons_arrived(&self, seasons: Vec<BaseItemDto>, chosen: Option<String>) {
        let Some(Page::Detail(page)) = self.top() else {
            return;
        };
        self.tabs.set_vec(
            seasons
                .iter()
                .map(|season| LibraryChoice {
                    id: season.id.clone().into(),
                    title: season.name.clone().into(),
                    detail: count(
                        season
                            .user_data
                            .as_ref()
                            .and_then(|data| data.unplayed_item_count),
                        "unwatched",
                        "unwatched",
                    )
                    .into(),
                })
                .collect::<Vec<_>>(),
        );
        let season = chosen
            .filter(|id| seasons.iter().any(|season| season.id == *id))
            .or_else(|| {
                seasons
                    .iter()
                    .find(|season| unplayed(season) > 0)
                    .or(seasons.first())
                    .map(|season| season.id.clone())
            });
        let mut items = self.items.borrow_mut();
        for season in &seasons {
            items.insert(season.id.clone(), season.clone());
        }
        drop(items);
        if let Some(season) = season {
            self.library().set_tab(season.clone().into());
            self.load_episodes(&page.id, &season);
        }
    }

    fn load_episodes(&self, series: &str, season: &str) {
        self.tracks_ticket.set(self.tracks_ticket.get() + 1);
        self.tracks_generation.bump();
        self.tracks.set_vec(Vec::new());
        let (series, season) = (series.to_owned(), season.to_owned());
        self.load(
            true,
            move |client| client.episodes(&series, &episodes_query(&season)),
            |this, page| this.set_tracks(&page.items, Tracks::Episodes),
        );
    }

    /// A collection's items, a row for each kind.
    fn collection_arrived(&self, items: Vec<BaseItemDto>) {
        let groups = [
            (ItemKind::Movie, "Movies"),
            (ItemKind::Series, "Shows"),
            (ItemKind::Season, "Seasons"),
            (ItemKind::Episode, "Episodes"),
            (ItemKind::MusicAlbum, "Albums"),
            (ItemKind::Audio, "Songs"),
        ];
        for (index, (kind, title)) in groups.iter().enumerate() {
            let group: Vec<BaseItemDto> = items
                .iter()
                .filter(|item| item.kind == *kind)
                .cloned()
                .collect();
            let id = format!("kind-{}", kind.as_str());
            self.rank.borrow_mut().insert(id.clone(), index);
            self.add_row(&id, title, &group, Art::Poster, Lines::Own, true);
        }
        let rest: Vec<BaseItemDto> = items
            .into_iter()
            .filter(|item| groups.iter().all(|(kind, _)| item.kind != *kind))
            .collect();
        self.add_row("other", "Other", &rest, Art::Poster, Lines::Own, true);
    }

    /// Says so under the songs when the page lists only the first of
    /// them (`PAGE_LIMIT`).
    fn note_tracks(&self, page: &QueryResult<BaseItemDto>) {
        let total = usize::try_from(page.total_record_count).unwrap_or(0);
        if total > page.items.len() {
            self.library().set_tracks_note(
                format!("Showing the first {} of {total}.", page.items.len()).into(),
            );
        }
    }

    fn set_tracks(&self, items: &[BaseItemDto], style: Tracks) {
        let discs = items
            .iter()
            .filter_map(|item| item.parent_index_number)
            .collect::<std::collections::HashSet<_>>()
            .len()
            > 1;
        let album_artist = self
            .detail
            .borrow()
            .as_ref()
            .and_then(|album| album.album_artist.clone())
            .unwrap_or_default();
        let playing = self
            .player
            .upgrade()
            .map(|player| player.current_path())
            .and_then(|path| session::parse_entry(&path).map(|(_, id)| id.to_owned()));
        let art = match style {
            Tracks::Episodes => Some(Art::Thumb),
            Tracks::Playlist | Tracks::Artist => Some(Art::Poster),
            // The album's cover is the page's.
            Tracks::Album => None,
        };
        let mut known = self.items.borrow_mut();
        let tracks: Vec<LibraryTrack> = items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                known.insert(item.id.clone(), item.clone());
                let mut track = track_of(item, style, discs, index, &album_artist);
                track.playing = playing.as_deref() == Some(item.id.as_str());
                if let Some(art) = art {
                    track.image = self.image(
                        picture(item, art).as_ref(),
                        TRACK_WIDTH,
                        &self.tracks_generation,
                        Slot::Track {
                            index,
                            id: item.id.clone(),
                        },
                    );
                }
                track
            })
            .collect();
        drop(known);
        self.tracks.set_vec(tracks);
    }

    fn choose(&self, what: &str, id: &str) {
        let Some(item) = self.detail.borrow().clone() else {
            return;
        };
        {
            let mut choice = self.choice.borrow_mut();
            match what {
                "version" => {
                    choice.version = Some(id.to_owned());
                    choice.audio = None;
                    choice.subtitle = None;
                }
                "audio" => choice.audio = id.parse().ok(),
                "subtitle" => choice.subtitle = id.parse().ok(),
                _ => return,
            }
        }
        let fresh = detail_of(&item, &self.choice.borrow());
        let library = self.library();
        let mut detail = library.get_detail();
        detail.versions = fresh.versions;
        detail.version = fresh.version;
        detail.audio = fresh.audio;
        detail.audio_track = fresh.audio_track;
        detail.subtitles = fresh.subtitles;
        detail.subtitle_track = fresh.subtitle_track;
        library.set_detail(detail);
    }

    // Search.

    fn search_changed(&self, text: String) {
        let weak = self.this.clone();
        self.search_timer
            .start(TimerMode::SingleShot, SEARCH_DELAY, move || {
                if let Some(this) = weak.upgrade() {
                    this.run_search(&text);
                }
            });
    }

    fn run_search(&self, text: &str) {
        if self.top() != Some(Page::Search) {
            return;
        }
        self.next_ticket();
        self.rows.set_vec(Vec::new());
        self.rank.borrow_mut().clear();
        let library = self.library();
        library.set_loading(false);
        library.set_error(SharedString::new());
        let Some(term) = search_term(text) else {
            return;
        };
        for (index, (id, title, kind)) in SEARCHES.into_iter().enumerate() {
            self.rank.borrow_mut().insert(id.into(), index);
            let term = term.clone();
            let art = if kind == ItemKind::Episode {
                Art::Thumb
            } else {
                Art::Poster
            };
            self.load(
                false,
                move |client| client.search(&term, &[kind], SEARCH_LIMIT),
                move |this, page| this.add_row(id, title, &page.items, art, Lines::Own, false),
            );
        }
        self.rank
            .borrow_mut()
            .insert("artists".into(), SEARCHES.len());
        self.load(
            false,
            move |client| client.search_artists(&term, SEARCH_LIMIT),
            |this, page| {
                this.add_row(
                    "artists",
                    "Artists",
                    &page.items,
                    Art::Poster,
                    Lines::Own,
                    false,
                )
            },
        );
    }

    // Marks.

    fn set_played(&self, id: &str, played: bool) {
        self.mark(
            id,
            move |client, id| client.set_played(id, played),
            move |marks| {
                marks.played = played;
                marks.progress = -1.0;
                if played {
                    marks.unplayed = 0;
                }
            },
            move |name| {
                if played {
                    format!("Couldn't Mark {name} as Played")
                } else {
                    format!("Couldn't Mark {name} as Unplayed")
                }
            },
        );
    }

    fn set_favorite(&self, id: &str, favorite: bool) {
        self.mark(
            id,
            move |client, id| client.set_favorite(id, favorite),
            move |marks| marks.favorite = favorite,
            move |name| {
                if favorite {
                    format!("Couldn't Add {name} to Favourites")
                } else {
                    format!("Couldn't Remove {name} from Favourites")
                }
            },
        );
    }

    /// Changes a mark at once, then as the server answers; puts it back
    /// when the server refuses, saying what failed (`failed` words it for
    /// the item's name).
    fn mark(
        &self,
        id: &str,
        request: impl FnOnce(&Client, &str) -> Result<UserItemDataDto, Error> + Send + 'static,
        change: impl FnOnce(&mut Marks),
        failed: impl FnOnce(&str) -> String + 'static,
    ) {
        let Some(account) = self.account() else {
            return;
        };
        let item = self
            .items
            .borrow()
            .get(id)
            .cloned()
            .unwrap_or_else(|| BaseItemDto {
                id: id.to_owned(),
                ..Default::default()
            });
        let before = Marks::of(&item);
        let mut after = before;
        change(&mut after);
        self.apply_marks(id, after, None);
        let weak = self.this.clone();
        let client = account.client;
        let asked = client.clone();
        let server_id = account.server.id;
        let server_name = account.server.name;
        let item_id = id.to_owned();
        background(
            "jellyfin",
            move || request(&client, &item_id),
            move |answer| {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                match answer {
                    Ok(data) => {
                        this.apply_marks(&item.id, Marks::from_data(&item, &data), Some(&data))
                    }
                    Err(error) => {
                        this.apply_marks(&item.id, before, None);
                        if error == Error::Unauthorized {
                            this.session.unauthorized(&server_id, &asked);
                        } else {
                            let name = if item.name.is_empty() {
                                "It"
                            } else {
                                item.name.as_str()
                            };
                            this.notify(&failed(name), &error.about(Some(&server_name)));
                        }
                    }
                }
            },
        );
    }

    /// Shows an item's marks wherever it is on the page.
    fn apply_marks(&self, id: &str, marks: Marks, data: Option<&UserItemDataDto>) {
        if let Some(item) = self.items.borrow_mut().get_mut(id) {
            let mut user_data = data
                .cloned()
                .unwrap_or_else(|| item.user_data.clone().unwrap_or_default());
            user_data.played = marks.played;
            user_data.is_favorite = marks.favorite;
            item.user_data = Some(user_data);
        }
        let card = |card: &mut LibraryCard| {
            card.played = marks.played;
            card.favorite = marks.favorite;
            card.progress = marks.progress;
            card.unplayed = marks.unplayed;
        };
        for model in [&self.rows, &self.detail_rows] {
            for row in model.iter() {
                for index in 0..row.cards.row_count() {
                    change_card(&row.cards, index, id, card);
                }
            }
        }
        for grid_card in self.grid.borrow_mut().cards.iter_mut() {
            if grid_card.id == id {
                card(grid_card);
            }
        }
        for index in 0..self.tracks.row_count() {
            if let Some(mut track) = self.tracks.row_data(index)
                && track.id == id
            {
                track.played = marks.played;
                track.favorite = marks.favorite;
                track.progress = marks.progress;
                self.tracks.set_row_data(index, track);
            }
        }
        let library = self.library();
        let mut detail = library.get_detail();
        if detail.id == id {
            detail.played = marks.played;
            detail.favorite = marks.favorite;
            detail.progress = marks.progress;
            if marks.played {
                detail.resume = SharedString::new();
            }
            library.set_detail(detail);
        }
    }

    // Playing.

    /// The item of the page shown when `id` is one of the songs it lists,
    /// as an album's, a playlist's or an artist's page does: playing the
    /// song plays the list from it.
    fn listing(&self, id: &str) -> Option<BaseItemDto> {
        let page = match self.top() {
            Some(Page::Detail(page)) => page,
            _ => return None,
        };
        let item = self
            .detail
            .borrow()
            .clone()
            .filter(|item| item.id == page.id && item.id != id)?;
        let lists = matches!(
            item.kind,
            ItemKind::MusicAlbum | ItemKind::Playlist | ItemKind::MusicArtist
        );
        let listed = self.tracks.iter().any(|track| track.id == id);
        (lists && listed).then_some(item)
    }

    /// Plays an item in this window, or adds it to the playlist: a show,
    /// season, album or playlist as the list of what it holds, and a song
    /// of the page's list as that list, from the song on.
    fn start_playing(&self, id: &str, mode: Mode) {
        let Some(account) = self.account() else {
            return;
        };
        let listing = match mode {
            Mode::Play { shuffle: false, .. } => self.listing(id),
            _ => None,
        };
        // An answer that comes after the user asked for something else, or
        // after the window opened something else, would replace it.
        let request = self.play_request.get() + 1;
        self.play_request.set(request);
        let opened = self.player.upgrade().map(|player| player.opened());
        let known = self.items.borrow().get(id).cloned();
        let choice = self
            .detail
            .borrow()
            .as_ref()
            .filter(|item| item.id == id)
            .map(|_| self.choice.borrow().clone());
        // An episode plays on into the next ones, as the account's Jellyfin
        // setting has it (on until the account says otherwise).
        let play_on = self
            .me
            .borrow()
            .as_ref()
            .is_none_or(|me| me.configuration.enable_next_episode_auto_play)
            && matches!(mode, Mode::Play { shuffle: false, .. });
        let client = account.client;
        let asked = client.clone();
        let server_id = account.server.id;
        let server_name = account.server.name;
        let id = id.to_owned();
        let weak = self.this.clone();
        background(
            "jellyfin",
            move || {
                let item = match known {
                    Some(item) => item,
                    None => client.item(&id)?,
                };
                if let Some(listing) = listing {
                    let items = playable_list(&client, &listing, mode)?;
                    // Past the most a list plays, the song plays alone.
                    if let Some(start) = items.iter().position(|song| song.id == item.id) {
                        return Ok((item, items, Start::At(start)));
                    }
                }
                let mut items = playable_list(&client, &item, mode)?;
                let plays_on = play_on && item.kind == ItemKind::Episode && items.len() == 1;
                if plays_on {
                    match next_episodes(&client, &item) {
                        Ok(next) => items.extend(next),
                        // The episode plays all the same.
                        Err(error) => log::warn!("no next episodes: {error}"),
                    }
                }
                let start = if plays_on { Start::Lead } else { Start::At(0) };
                Ok((item, items, start))
            },
            move |result: Result<(BaseItemDto, Vec<BaseItemDto>, Start), Error>| {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                let now = this.player.upgrade().map(|player| player.opened());
                if this.play_request.get() != request || now != opened {
                    log::debug!("dropped a Jellyfin play request made before another");
                    return;
                }
                match result {
                    Ok((item, items, _)) if items.is_empty() => this.notify(
                        "Nothing to Play",
                        &format!("{} has nothing Patina can play.", item.name),
                    ),
                    Ok((item, items, start)) => {
                        let choice = choice.as_ref().map(|choice| (item.id.as_str(), choice));
                        this.open_items(&server_id, &item, &items, mode, choice, start);
                    }
                    Err(Error::Unauthorized) => this.session.unauthorized(&server_id, &asked),
                    Err(error) => this.notify("Can't Play", &error.about(Some(&server_name))),
                }
            },
        );
    }

    /// Opens items as `jellyfin://` entries in this window's playlist,
    /// noting each in the registry for the load hook, or adds them to the
    /// playlist of the window playing. The choices made on an item's page
    /// go with that item, `asked`. With `Start::Lead`, the rest play on
    /// after it (the next episodes).
    fn open_items(
        &self,
        server_id: &str,
        asked: &BaseItemDto,
        items: &[BaseItemDto],
        mode: Mode,
        choice: Option<(&str, &Choice)>,
        start: Start,
    ) {
        let registry = self.session.registry();
        let lead = matches!(start, Start::Lead).then_some(asked.id.as_str());
        let mut entries = Vec::new();
        let mut infos = Vec::new();
        let mut first = 0;
        for (index, item) in items.iter().enumerate() {
            let Some(url) = session::entry_url(server_id, &item.id) else {
                log::warn!("Jellyfin item {:?} can't be opened", item.id);
                continue;
            };
            if start == Start::At(index) {
                first = entries.len();
            }
            let choice = choice
                .filter(|(id, _)| *id == item.id)
                .map(|(_, choice)| choice);
            // What plays on after the item resumes as the server says,
            // whatever the item itself was played from.
            let mode = match mode {
                Mode::Play { shuffle, .. } if lead.is_some_and(|lead| lead != item.id) => {
                    Mode::Play {
                        from_start: false,
                        shuffle,
                    }
                }
                mode => mode,
            };
            infos.push((url.clone(), item_info(server_id, item, mode, choice)));
            entries.push(url);
        }
        session::remember(
            &mut registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            infos,
        );
        let Some(player) = self.player.upgrade() else {
            return;
        };
        match mode {
            // Shuffled by mpv, as the playlist's Shuffle does, so that the
            // playlist says it is and turning that off puts the list back
            // in order.
            Mode::Play { shuffle: true, .. } => {
                player.jellyfin_opened(&entries);
                player.open_shuffled(entries);
            }
            Mode::Play { .. } => {
                player.jellyfin_opened(&entries);
                player.open_from(entries, first);
            }
            // The library shows while this window has nothing to play, so
            // the queue is another's, when one plays.
            Mode::Queue { next } => match player.queue_window() {
                Some(playing) => {
                    playing.jellyfin_opened(&entries);
                    playing.queue(entries, next);
                    let text = if next {
                        "Playing Next"
                    } else {
                        "Added to Queue"
                    };
                    self.notify(text, &asked.name);
                }
                None => {
                    player.jellyfin_opened(&entries);
                    player.queue(entries, next);
                }
            },
        }
    }

    /// A message over the library, long enough to read.
    fn notify(&self, text: &str, detail: &str) {
        if let Some(player) = self.player.upgrade() {
            player.show_message(text, detail, NOTICE_SECONDS);
        }
    }

    // Signing in.

    fn flow(&self) -> Option<Rc<SignIn>> {
        self.sign_in.borrow().clone()
    }

    /// Starts signing in to a server in this window.
    pub fn begin_sign_in(&self) {
        self.begin_sign_in_to(None, "");
    }

    /// Starts signing in again to a server Patina knows, with `note` saying
    /// why: an entry of it didn't open for want of a sign-in.
    pub fn sign_in_to(&self, server_id: &str, note: &str) {
        if self.sign_in.borrow().is_some() {
            return;
        }
        if let Some(server) = self.session.server(server_id) {
            self.begin_sign_in_to(Some(&server), note);
        }
    }

    fn begin_sign_in_to(&self, server: Option<&Server>, message: &str) {
        let existing = self.flow();
        let flow = match existing {
            Some(flow) => flow,
            None => {
                let weak = self.this.clone();
                let flow = SignIn::new(&self.session, move |event| {
                    if let Some(this) = weak.upgrade() {
                        this.sign_in_event(event);
                    }
                });
                *self.sign_in.borrow_mut() = Some(flow.clone());
                flow
            }
        };
        *self.renewing.borrow_mut() = server
            .filter(|server| {
                self.session
                    .expired()
                    .iter()
                    .any(|expired| expired.id == server.id)
            })
            .map(|server| server.id.clone());
        match server {
            Some(server) => flow.renew(server, message),
            None => flow.begin(),
        }
        self.leave_error();
        self.push(Page::SignIn);
        self.show_page();
        self.update_shown();
    }

    fn sign_in_event(&self, event: SignInEvent) {
        match event {
            SignInEvent::Changed(state) => {
                self.library().set_sign_in(sign_in_model(&state));
                if self.top() == Some(Page::SignIn) {
                    self.update_back();
                }
            }
            SignInEvent::SignedIn(id) => {
                self.sign_in.borrow_mut().take();
                self.renewing.borrow_mut().take();
                self.pages.borrow_mut().remove_sign_in();
                // What a file that failed to open said ("Sign in again")
                // is done with.
                self.leave_error();
                if self.server.borrow().as_deref() != Some(&id) {
                    *self.server.borrow_mut() = Some(id);
                }
                self.server_changed();
                self.update_servers();
                self.update_shown();
            }
            // The reason is in the log; it is the keyring's own words.
            SignInEvent::Unsaved => self.notify(
                "Signed In Until Patina Quits",
                &format!(
                    "Patina will ask again next time: {} couldn't keep the sign-in.",
                    crate::credentials::STORE_NAME
                ),
            ),
        }
    }

    /// Stops signing in without changing pages.
    fn cancel_flow(&self) {
        let flow = self.sign_in.borrow_mut().take();
        if let Some(flow) = flow {
            flow.cancel();
            self.pages.borrow_mut().remove_sign_in();
        }
        if let Some(server) = self.renewing.borrow_mut().take() {
            self.session.forget_expired(&server);
        }
    }

    fn cancel_sign_in(&self) {
        self.cancel_flow();
        if self.account().is_some() && self.pages.borrow().top().is_none() {
            self.pages.borrow_mut().go_to(None);
        }
        self.restore_scroll();
        self.show_page();
        self.update_shown();
    }
}

impl Listener for LibraryController {
    fn session_changed(&self, change: &Change) {
        match change {
            Change::Loaded => {
                self.refresh_session();
                let expired = self.session.expired();
                if self.server.borrow().is_none()
                    && self.sign_in.borrow().is_none()
                    && let Some(server) = expired.first()
                {
                    self.begin_sign_in_to(
                        Some(server),
                        "Sign in again to see this server's library.",
                    );
                }
            }
            // Another window signed the server in again: this one's
            // renewal is done, and it goes back to the server's Home.
            Change::SignedIn(id) if self.renewing.borrow().as_deref() == Some(id) => {
                self.renewing.borrow_mut().take();
                if let Some(flow) = self.sign_in.borrow_mut().take() {
                    flow.cancel();
                }
                self.pages.borrow_mut().remove_sign_in();
                self.leave_error();
                *self.server.borrow_mut() = Some(id.clone());
                self.server_changed();
                self.update_servers();
                self.update_shown();
            }
            Change::SigningOut(id) => {
                if let Some(player) = self.player.upgrade() {
                    player.jellyfin_signing_out(id);
                }
            }
            Change::SignedIn(_) | Change::SignedOut(_) => self.refresh_session(),
            // A playback in some window ended, and the server has its
            // progress: an idle window showing that server shows it again,
            // when its page shows what that changes.
            Change::Played { server_id, audio } => {
                if self.server.borrow().as_deref() != Some(server_id)
                    || !self.top().is_some_and(|page| shows_played(&page, *audio))
                {
                    return;
                }
                let idle = self.player.upgrade().is_some_and(|player| player.is_idle());
                if idle {
                    self.refresh();
                } else {
                    self.stale.set(true);
                }
            }
            Change::Expired(id) => {
                let mine = self.server.borrow().as_deref() == Some(id);
                self.refresh_session();
                if mine
                    && self.sign_in.borrow().is_none()
                    && let Some(server) = self.session.server(id)
                {
                    self.begin_sign_in_to(Some(&server), &Error::Unauthorized.to_string());
                }
            }
        }
    }
}

impl Drop for LibraryController {
    fn drop(&mut self) {
        self.generation.bump();
        self.tracks_generation.bump();
        self.bar_generation.bump();
        if let Some(flow) = self.sign_in.borrow_mut().take() {
            flow.cancel();
        }
    }
}

/// What `LibraryController::refresh` keeps of a page it loads again.
#[derive(Default)]
struct Kept {
    choice: Choice,
    /// The grid's cards loaded so far.
    cards: usize,
}

/// How many cards a grid asks for first: a page, or as many as it had,
/// in whole pages.
fn first_cards(had: usize) -> u32 {
    let had = u32::try_from(had).unwrap_or(u32::MAX);
    had.div_ceil(GRID_PAGE).max(1).saturating_mul(GRID_PAGE)
}

/// Whether `page` may show what the end of a playback changed: an item's
/// played mark, resume position and play count, and what Home lists to
/// resume or watch next. After something played as audio, only a song's
/// card can: Continue Watching and Next Up list videos alone, an album's,
/// artist's or playlist's songs show no played mark, and the server marks
/// no album, artist or playlist played. Songs show as cards in search
/// results, a folder's items and a collection's. Songs end every few
/// minutes, and loading a page again empties it until it comes.
fn shows_played(page: &Page, audio: bool) -> bool {
    match page {
        Page::Local | Page::SignIn => false,
        _ if !audio => true,
        Page::Search => true,
        Page::Grid(grid) => grid.tab == Tab::Items,
        Page::Detail(detail) => matches!(detail.kind, ItemKind::BoxSet | ItemKind::Unknown),
        Page::Home => false,
    }
}

/// The cards of the row with this id.
fn row_cards(model: &VecModel<LibraryRow>, id: &str) -> Option<ModelRc<LibraryCard>> {
    model.iter().find(|row| row.id == id).map(|row| row.cards)
}

/// Changes the `index`th card when it is still the item `id`'s.
fn change_card(
    cards: &ModelRc<LibraryCard>,
    index: usize,
    id: &str,
    change: impl FnOnce(&mut LibraryCard),
) {
    if let Some(mut card) = cards.row_data(index)
        && card.id == id
    {
        change(&mut card);
        cards.set_row_data(index, card);
    }
}

fn sign_in_model(state: &SignInState) -> LibrarySignIn {
    let users: Vec<LibraryChoice> = state
        .users
        .iter()
        .map(|user| LibraryChoice {
            id: user.id.clone().into(),
            title: user.name.clone().into(),
            detail: if user.has_password {
                SharedString::new()
            } else {
                "No password".into()
            },
        })
        .collect();
    let found: Vec<LibraryChoice> = state
        .found
        .iter()
        .map(|found| LibraryChoice {
            id: found.address.clone().into(),
            title: found.name.clone().into(),
            detail: found.detail.clone().into(),
        })
        .collect();
    let saved: Vec<LibraryChoice> = state
        .saved
        .iter()
        .map(|server| LibraryChoice {
            id: server.id.clone().into(),
            title: server.name.clone().into(),
            detail: if server.user_name.is_empty() {
                server.address.clone()
            } else {
                format!("{} · {}", server.user_name, server.address)
            }
            .into(),
        })
        .collect();
    LibrarySignIn {
        step: state.step.as_str().into(),
        address: state.address.clone().into(),
        server_name: state.server_name.clone().into(),
        server_version: state.server_version.clone().into(),
        insecure: state.insecure,
        users: ModelRc::new(VecModel::from(users)),
        user: state.user.clone().into(),
        quick_connect: state.quick_connect,
        code: state.code.clone().into(),
        scanning: state.scanning,
        found: ModelRc::new(VecModel::from(found)),
        saved: ModelRc::new(VecModel::from(saved)),
        working: state.working,
        message: state.message.clone().into(),
        note: state.note.clone().into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(query: &ItemsQuery) -> HashMap<String, String> {
        query.pairs().into_iter().collect()
    }

    fn grid(collection: Collection) -> GridPage {
        GridPage::new("lib", "lib", "Library", collection)
    }

    #[test]
    fn each_tab_asks_for_its_own_items() {
        let movies = grid(Collection::Movies);
        assert_eq!(movies.tab, Tab::Movies);
        let request = grid_request(&movies, 0, 100);
        assert_eq!(request.endpoint, Endpoint::Items);
        let query = pairs(&request.query);
        assert_eq!(query["parentId"], "lib");
        assert_eq!(query["includeItemTypes"], "Movie");
        assert_eq!(query["recursive"], "true");
        assert_eq!(query["sortBy"], "SortName");
        assert_eq!(query["sortOrder"], "Ascending");
        assert_eq!(query["fields"], "PrimaryImageAspectRatio");
        assert_eq!(query["enableImageTypes"], "Primary");
        assert_eq!(query["startIndex"], "0");
        assert_eq!(query["limit"], "100");
        assert_eq!(query["enableTotalRecordCount"], "true");
        assert!(!query.contains_key("filters"));

        let collections = GridPage {
            tab: Tab::Collections,
            sort: Sort::Added,
            descending: true,
            unplayed: true,
            favorites: true,
            ..movies.clone()
        };
        let query = pairs(&grid_request(&collections, 200, 100).query);
        assert_eq!(query["includeItemTypes"], "BoxSet");
        assert_eq!(query["sortBy"], "DateCreated,SortName");
        assert_eq!(query["sortOrder"], "Descending");
        assert_eq!(query["filters"], "IsUnplayed,IsFavorite");
        assert_eq!(query["fields"], "PrimaryImageAspectRatio,ChildCount");
        assert_eq!(query["startIndex"], "200");

        let shows = GridPage {
            sort: Sort::Added,
            ..grid(Collection::Shows)
        };
        let query = pairs(&grid_request(&shows, 0, 100).query);
        assert_eq!(query["includeItemTypes"], "Series");
        assert_eq!(query["sortBy"], "DateLastContentAdded,SortName");

        let music = grid(Collection::Music);
        assert_eq!(music.tab, Tab::Albums);
        let artists = grid_request(
            &GridPage {
                tab: Tab::Artists,
                ..music.clone()
            },
            0,
            100,
        );
        assert_eq!(artists.endpoint, Endpoint::AlbumArtists);
        assert_eq!(pairs(&artists.query)["parentId"], "lib");
        assert!(!pairs(&artists.query).contains_key("includeItemTypes"));
        let genres = grid_request(
            &GridPage {
                tab: Tab::Genres,
                ..music.clone()
            },
            0,
            100,
        );
        assert_eq!(genres.endpoint, Endpoint::Genres);
        assert_eq!(pairs(&genres.query)["includeItemTypes"], "MusicAlbum");
        // Music keeps no watched state to filter by.
        let unplayed_albums = GridPage {
            unplayed: true,
            favorites: true,
            ..music.clone()
        };
        let query = pairs(&grid_request(&unplayed_albums, 0, 100).query);
        assert_eq!(query["filters"], "IsFavorite");
        // Playlists aren't in the music library; its tab lists music's.
        let playlists = pairs(
            &grid_request(
                &GridPage {
                    tab: Tab::Playlists,
                    ..music
                },
                0,
                100,
            )
            .query,
        );
        assert!(!playlists.contains_key("parentId"));
        assert_eq!(playlists["includeItemTypes"], "Playlist");
        assert_eq!(playlists["mediaTypes"], "Audio");
        let library = pairs(&grid_request(&grid(Collection::Playlists), 0, 100).query);
        assert_eq!(library["parentId"], "lib");
        assert_eq!(library["includeItemTypes"], "Playlist");

        // A folder's own items, not everything below it.
        let folder = pairs(&grid_request(&grid(Collection::Folders), 0, 100).query);
        assert_eq!(folder["parentId"], "lib");
        assert!(!folder.contains_key("recursive"));
        assert!(!folder.contains_key("includeItemTypes"));
    }

    #[test]
    fn libraries_have_their_tabs_and_sorts() {
        assert_eq!(Collection::of(CollectionKind::Books), None);
        assert_eq!(Collection::of(CollectionKind::Photos), None);
        assert_eq!(Collection::of(CollectionKind::LiveTv), None);
        assert_eq!(
            Collection::of(CollectionKind::HomeVideos),
            Some(Collection::Folders)
        );
        assert_eq!(
            Collection::of(CollectionKind::TvShows).unwrap().nav_kind(),
            "tvshows"
        );
        let music: Vec<&str> = Collection::Music
            .tabs()
            .iter()
            .map(|tab| tab.id())
            .collect();
        assert_eq!(music, ["albums", "artists", "playlists", "genres"]);
        assert!(!Collection::BoxSets.has_latest() && Collection::Movies.has_latest());
        for tab in Tab::ALL {
            assert_eq!(Tab::from_id(tab.id()), Some(tab));
            assert_eq!(tab.sorts()[0], Sort::Name, "{tab:?} starts by name");
        }
        for sort in Sort::ALL {
            assert_eq!(Sort::from_id(sort.id()), Some(sort));
        }
        assert_eq!(Sort::from_id("nonsense"), None);
    }

    #[test]
    fn home_and_item_queries_name_what_they_show() {
        let resume = pairs(&resume_query());
        assert_eq!(resume["mediaTypes"], "Video");
        assert_eq!(resume["enableImageTypes"], "Primary,Thumb,Backdrop");
        assert_eq!(resume["imageTypeLimit"], "1");
        assert_eq!(pairs(&next_up_query())["enableResumable"], "false");
        assert_eq!(pairs(&latest_query("v1"))["parentId"], "v1");
        let tracks = pairs(&album_tracks_query("a1"));
        assert_eq!(tracks["sortBy"], "ParentIndexNumber,IndexNumber,SortName");
        assert_eq!(tracks["includeItemTypes"], "Audio");
        assert_eq!(pairs(&episodes_query("s1"))["seasonId"], "s1");
        assert_eq!(pairs(&artist_albums_query("ar"))["albumArtistIds"], "ar");
        assert_eq!(
            pairs(&person_query("p1", ItemKind::Movie))["personIds"],
            "p1"
        );
        assert_eq!(
            pairs(&genre_query("g1", ItemKind::MusicAlbum))["genreIds"],
            "g1"
        );
        assert_eq!(search_term("  night "), Some("night".into()));
        assert_eq!(search_term(" \t"), None);
    }

    #[test]
    fn pages_stack_and_go_back() {
        let mut pages = Pages::default();
        assert_eq!(pages.nav_current(), "");
        assert!(!pages.can_go_back());
        pages.go_to(None);
        assert_eq!(pages.nav_current(), "home");
        assert!(!pages.pop(), "Home stays");

        let movies = Page::Grid(grid(Collection::Movies));
        pages.go_to(Some(movies.clone()));
        assert_eq!(pages.nav_current(), "lib");
        assert!(pages.can_go_back());
        let film = Page::Detail(DetailPage {
            id: "m1".into(),
            kind: ItemKind::Movie,
            root: pages.nav_current(),
            season: None,
        });
        pages.push(film.clone(), 120.0);
        // Opening what is open already adds nothing.
        pages.push(film.clone(), 40.0);
        assert_eq!(pages.0.len(), 3);
        assert_eq!(pages.scroll(), 0.0, "a page opened starts at its top");
        assert_eq!(pages.nav_current(), "lib");
        assert_eq!(pages.top().unwrap().name(), "detail");

        pages.push(Page::SignIn, 0.0);
        assert_eq!(pages.nav_current(), "");
        pages.remove_sign_in();
        assert_eq!(pages.top(), Some(&film));
        assert!(pages.pop());
        assert_eq!(pages.top(), Some(&movies));
        assert_eq!(pages.scroll(), 120.0, "Back returns where the grid was");
        pages.replace_top(Page::Search);
        assert_eq!(pages.nav_current(), "search");

        // The bar's pages start again from Home.
        pages.go_to(Some(Page::Local));
        assert_eq!(pages.0, [(Page::Home, 0.0), (Page::Local, 0.0)]);
        assert_eq!(pages.top().unwrap().name(), "local");
    }

    fn card(id: &str) -> LibraryCard {
        LibraryCard {
            id: id.into(),
            ..Default::default()
        }
    }

    #[test]
    fn more_of_a_grid_fills_its_last_line_first() {
        let rows = VecModel::from(lines(
            &(0..4).map(|n| card(&n.to_string())).collect::<Vec<_>>(),
            3,
        ));
        let first = rows.row_data(0).unwrap().cards;
        let second = rows.row_data(1).unwrap().cards;
        append_lines(&rows, 4, (4..9).map(|n| card(&n.to_string())).collect(), 3);
        let ids: Vec<Vec<String>> = rows
            .iter()
            .map(|row| row.cards.iter().map(|card| card.id.to_string()).collect())
            .collect();
        assert_eq!(
            ids,
            [
                vec!["0", "1", "2"],
                vec!["3", "4", "5"],
                vec!["6", "7", "8"]
            ]
        );
        // The lines that were there are the same models, filled in.
        assert!(rows.row_data(0).unwrap().cards == first);
        assert!(rows.row_data(1).unwrap().cards == second);
        assert_eq!(rows.row_data(2).unwrap().id, "line-2");
        // A full last line is left alone.
        append_lines(&rows, 9, vec![card("9")], 3);
        assert_eq!(rows.row_count(), 4);
        assert_eq!(rows.row_data(2).unwrap().cards.row_count(), 3);
    }

    #[test]
    fn grids_split_into_lines() {
        let cards: Vec<LibraryCard> = (0..7).map(|n| card(&n.to_string())).collect();
        let rows = lines(&cards, 3);
        let ids: Vec<Vec<String>> = rows
            .iter()
            .map(|row| row.cards.iter().map(|card| card.id.to_string()).collect())
            .collect();
        assert_eq!(ids, [vec!["0", "1", "2"], vec!["3", "4", "5"], vec!["6"]]);
        assert_eq!(rows[2].id, "line-2");
        assert_eq!(lines(&cards, 0).len(), 7, "no columns is one a line");
        assert!(lines(&[], 4).is_empty());
        assert_eq!(place(0, 3), (0, 0));
        assert_eq!(place(5, 3), (1, 2));
        assert_eq!(place(6, 3), (2, 0));
        assert_eq!(place(4, 0), (4, 0));
    }

    fn item(json: &str) -> BaseItemDto {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn cards_read_as_their_kind() {
        let episode = item(
            r#"{"Id": "e1", "Name": "Night Market", "Type": "Episode", "SeriesName": "Lantern Street",
                "SeriesId": "s1", "ParentIndexNumber": 1, "IndexNumber": 2, "SeriesPrimaryImageTag": "st",
                "ParentBackdropItemId": "s1", "ParentBackdropImageTags": ["bt"],
                "ImageBlurHashes": {"Backdrop": {"bt": "LEHV6nWB2yk8pyo0adR*.7kCMdnj"}},
                "UserData": {"PlaybackPositionTicks": 300000000, "PlayedPercentage": 25.0, "Played": false},
                "RunTimeTicks": 1200000000}"#,
        );
        assert_eq!(
            card_lines(&episode, Lines::Home),
            ("Lantern Street".into(), "S1 E2 · Night Market".into())
        );
        assert_eq!(
            card_lines(&episode, Lines::Own),
            ("Night Market".into(), "Lantern Street · S1 E2".into())
        );
        let card = card_of(&episode, Art::Thumb, Lines::Home);
        assert_eq!(card.kind, "episode");
        assert!((card.progress - 0.25).abs() < 1e-6);
        // Without a still of its own, a landscape card takes the show's
        // backdrop, and a poster the show's poster.
        let thumb = picture(&episode, Art::Thumb).unwrap();
        assert_eq!(
            (thumb.item.as_str(), thumb.kind, thumb.index),
            ("s1", ImageType::Backdrop, Some(0))
        );
        assert!(thumb.blurhash.is_some());
        let poster = picture(&episode, Art::Poster).unwrap();
        assert_eq!((poster.item.as_str(), poster.tag.as_str()), ("s1", "st"));
        assert!((poster.aspect - 2.0 / 3.0).abs() < 1e-6);

        let series = item(
            r#"{"Id": "s1", "Name": "Lantern Street", "Type": "Series", "ProductionYear": 2019,
                "ImageTags": {"Primary": "p"}, "PrimaryImageAspectRatio": 0.6667,
                "UserData": {"PlayedPercentage": 20, "UnplayedItemCount": 4, "Played": false}}"#,
        );
        let card = card_of(&series, Art::Poster, Lines::Own);
        assert_eq!(
            (card.title.as_str(), card.subtitle.as_str()),
            ("Lantern Street", "2019")
        );
        // A show's percentage counts episodes: it gets no progress bar.
        assert_eq!(card.progress, -1.0);
        assert_eq!(card.unplayed, 4);

        let album = item(
            r#"{"Id": "a1", "Name": "Tides", "Type": "MusicAlbum", "AlbumArtist": "Mira Vale",
                "ImageTags": {"Primary": "p"}}"#,
        );
        let card = card_of(&album, Art::Poster, Lines::Own);
        assert_eq!(
            (card.kind.as_str(), card.subtitle.as_str()),
            ("album", "Mira Vale")
        );
        assert_eq!(card.aspect, 1.0);
        let playlist = item(
            r#"{"Id": "p1", "Name": "Mix", "Type": "Playlist", "MediaType": "Audio", "ChildCount": 3}"#,
        );
        assert_eq!(card_lines(&playlist, Lines::Own).1, "3 songs");
        let collection = item(r#"{"Id": "b1", "Name": "Set", "Type": "BoxSet", "ChildCount": 1}"#);
        assert_eq!(card_lines(&collection, Lines::Own).1, "1 item");
        let folder = item(r#"{"Id": "f1", "Name": "Clips", "Type": "Folder", "IsFolder": true}"#);
        assert_eq!(card_kind(&folder), "folder");
        assert_eq!(
            card_of(&folder, Art::Poster, Lines::Own).image.size().width,
            0
        );
        let library = item(
            r#"{"Id": "v1", "Name": "Movies", "Type": "CollectionFolder", "CollectionType": "movies"}"#,
        );
        assert_eq!(card_kind(&library), "library");
        for kind in [
            "movie",
            "series",
            "season",
            "episode",
            "collection",
            "playlist",
            "album",
            "artist",
            "song",
            "library",
            "folder",
            "person",
            "video",
        ] {
            assert_ne!(item_kind(kind), ItemKind::Unknown, "{kind}");
        }
    }

    #[test]
    fn facts_and_numbers_read_naturally() {
        assert_eq!(runtime(112.0 * 60.0), "1 h 52 min");
        assert_eq!(runtime(45.0 * 60.0 + 10.0), "45 min");
        assert_eq!(runtime(3600.0), "1 h");
        assert_eq!(runtime(10.0), "1 min");
        let film = item(
            r#"{"Id": "m1", "Name": "Night Signal", "Type": "Movie", "ProductionYear": 2021,
                "RunTimeTicks": 67200000000, "OfficialRating": "PG-13", "CommunityRating": 7.14}"#,
        );
        assert_eq!(facts(&film), "2021 · 1 h 52 min · PG-13 · ★ 7.1");
        let episode = item(
            r#"{"Id": "e1", "Name": "Pilot", "Type": "Episode", "ParentIndexNumber": 2, "IndexNumber": 3,
                "IndexNumberEnd": 4, "PremiereDate": "2024-05-17T00:00:00.0000000Z", "RunTimeTicks": 27000000000}"#,
        );
        assert_eq!(facts(&episode), "S2 E3–4 · May 17, 2024 · 45 min");
        assert_eq!(person_kind("GuestStar"), "Guest Star");
        assert_eq!(person_kind("Director"), "Director");
        assert_eq!(track_number(&episode, Tracks::Episodes, false, 0), "3-4");
        let song = item(
            r#"{"Id": "t1", "Name": "Drift", "Type": "Audio", "ParentIndexNumber": 2, "IndexNumber": 5,
                "Artists": ["Mira Vale", "Guest"], "AlbumArtist": "Mira Vale", "RunTimeTicks": 2050000000}"#,
        );
        assert_eq!(track_number(&song, Tracks::Album, true, 0), "2-5");
        assert_eq!(track_number(&song, Tracks::Album, false, 0), "5");
        assert_eq!(track_number(&song, Tracks::Playlist, false, 6), "7");
        assert_eq!(track_subtitle(&song, Tracks::Album, "Mira Vale, Guest"), "");
        assert_eq!(
            track_subtitle(&song, Tracks::Album, "Mira Vale"),
            "Mira Vale, Guest"
        );
        let track = track_of(&song, Tracks::Album, false, 0, "");
        assert_eq!(track.duration, "03:25");
        assert_eq!(track.subtitle, "Mira Vale, Guest");
        let solo = item(
            r#"{"Id": "t2", "Name": "Ebb", "Type": "Audio", "Artists": ["Mira Vale"],
                "AlbumArtist": "Mira Vale"}"#,
        );
        assert_eq!(track_subtitle(&solo, Tracks::Album, ""), "");
        // An artist's songs: their place in the list, and their albums.
        let listed = item(
            r#"{"Id": "t3", "Name": "Swell", "Type": "Audio", "IndexNumber": 9, "Album": "Tides",
                "Artists": ["Mira Vale"]}"#,
        );
        assert_eq!(track_number(&listed, Tracks::Artist, false, 2), "3");
        assert_eq!(track_subtitle(&listed, Tracks::Artist, ""), "Tides");
        assert_eq!(display_title(&song), "Drift");
        let episode = item(
            r#"{"Id": "e1", "Name": "Pilot", "Type": "Episode", "SeriesName": "Show",
                "ParentIndexNumber": 1, "IndexNumber": 1}"#,
        );
        assert_eq!(display_title(&episode), "Show · S1 E1 · Pilot");
    }

    #[test]
    fn an_items_page_offers_its_versions_and_tracks() {
        let movie: BaseItemDto =
            serde_json::from_str(include_str!("fixtures/item_movie.json")).unwrap();
        let detail = detail_of(&movie, &Choice::default());
        assert_eq!(detail.title, "Night Signal");
        assert_eq!(detail.tagline, "Someone is still transmitting.");
        assert_eq!(detail.genres, "Mystery, Drama");
        assert!(detail.can_play);
        assert_eq!(detail.versions.row_count(), 1);
        assert_eq!(detail.version, movie.media_sources[0].id.as_str());
        let audio: Vec<String> = detail
            .audio
            .iter()
            .map(|choice| choice.id.to_string())
            .collect();
        assert_eq!(audio, ["3", "4"]);
        assert_eq!(detail.audio_track, "3");
        let subtitles: Vec<String> = detail
            .subtitles
            .iter()
            .map(|choice| choice.id.to_string())
            .collect();
        assert_eq!(subtitles, ["-1", "0", "1", "5"]);
        assert_eq!(detail.subtitles.row_data(0).unwrap().title, "Off");
        let chosen = detail_of(
            &movie,
            &Choice {
                audio: Some(4),
                subtitle: Some(-1),
                ..Default::default()
            },
        );
        assert_eq!(
            (chosen.audio_track.as_str(), chosen.subtitle_track.as_str()),
            ("4", "-1")
        );

        let episode: BaseItemDto =
            serde_json::from_str(include_str!("fixtures/item_episode.json")).unwrap();
        let detail = detail_of(&episode, &Choice::default());
        assert_eq!(detail.parent_kind, "series");
        assert_eq!(
            detail.parent_id.as_str(),
            episode.series_id.as_deref().unwrap()
        );

        let series = item(r#"{"Id": "s1", "Name": "Show", "Type": "Series"}"#);
        let detail = detail_of(&series, &Choice::default());
        assert!(detail.can_play);
        assert_eq!(detail.versions.row_count(), 0);
        assert_eq!(detail.subtitle_track, "");
        let person = item(r#"{"Id": "p1", "Name": "Someone", "Type": "Person"}"#);
        assert!(!detail_of(&person, &Choice::default()).can_play);
    }

    #[test]
    fn a_song_ending_loads_again_only_pages_that_show_song_cards() {
        let detail = |kind: ItemKind| {
            Page::Detail(DetailPage {
                id: "i1".into(),
                kind,
                root: "lib".into(),
                season: None,
            })
        };
        let folders = grid(Collection::Folders);
        assert_eq!(folders.tab, Tab::Items);
        let mut music = grid(Collection::Music);
        // After a video, any page with the server's items.
        for page in [
            Page::Home,
            Page::Search,
            Page::Grid(music.clone()),
            detail(ItemKind::Movie),
            detail(ItemKind::MusicAlbum),
        ] {
            assert!(shows_played(&page, false), "{page:?}");
        }
        // After a song, only those with song cards.
        for page in [
            Page::Search,
            Page::Grid(folders),
            detail(ItemKind::BoxSet),
            detail(ItemKind::Unknown),
        ] {
            assert!(shows_played(&page, true), "{page:?}");
        }
        for tab in [Tab::Albums, Tab::Artists, Tab::Playlists, Tab::Genres] {
            music.tab = tab;
            assert!(!shows_played(&Page::Grid(music.clone()), true), "{tab:?}");
        }
        for kind in [
            ItemKind::MusicAlbum,
            ItemKind::MusicArtist,
            ItemKind::Playlist,
            ItemKind::MusicGenre,
            ItemKind::Movie,
            ItemKind::Series,
            ItemKind::Episode,
        ] {
            assert!(!shows_played(&detail(kind), true), "{kind:?}");
        }
        assert!(!shows_played(&Page::Home, true));
        for audio in [false, true] {
            assert!(!shows_played(&Page::Local, audio));
            assert!(!shows_played(&Page::SignIn, audio));
        }
    }

    #[test]
    fn a_grid_loaded_again_asks_for_the_cards_it_had() {
        assert_eq!(first_cards(0), GRID_PAGE);
        assert_eq!(first_cards(37), GRID_PAGE);
        assert_eq!(first_cards(100), GRID_PAGE);
        assert_eq!(first_cards(101), 2 * GRID_PAGE);
        assert_eq!(first_cards(450), 5 * GRID_PAGE);
    }

    #[test]
    fn playing_notes_where_to_start() {
        let started = item(
            r#"{"Id": "m1", "Name": "Film", "Type": "Movie", "MediaType": "Video",
                "UserData": {"PlaybackPositionTicks": 1200000000, "Played": false}}"#,
        );
        let resume = Mode::Play {
            from_start: false,
            shuffle: false,
        };
        let info = item_info("srv", &started, resume, None);
        assert_eq!(info.resume_ticks, 1_200_000_000);
        assert_eq!(
            (info.server_id.as_str(), info.id.as_str(), info.audio),
            ("srv", "m1", false)
        );
        let again = Mode::Play {
            from_start: true,
            shuffle: false,
        };
        assert_eq!(item_info("srv", &started, again, None).resume_ticks, 0);
        assert_eq!(
            item_info("srv", &started, Mode::Queue { next: true }, None).resume_ticks,
            1_200_000_000
        );
        let choice = Choice {
            version: Some("v2".into()),
            audio: Some(3),
            subtitle: None,
        };
        let info = item_info("srv", &started, resume, Some(&choice));
        assert_eq!(info.media_source_id.as_deref(), Some("v2"));
        assert_eq!(
            (info.audio_stream_index, info.subtitle_stream_index),
            (Some(3), None)
        );
        let song = item(
            r#"{"Id": "t1", "Name": "Song", "Type": "Audio", "MediaType": "Audio",
                "UserData": {"PlaybackPositionTicks": 50000000}}"#,
        );
        let info = item_info("srv", &song, resume, None);
        assert!(info.audio);
        assert_eq!(info.resume_ticks, 0, "songs start at the start");

        // A show plays on from the first episode left to watch.
        let episode = |id: &str, played: bool| {
            item(&format!(
                r#"{{"Id": "{id}", "Type": "Episode", "UserData": {{"Played": {played}}}}}"#
            ))
        };
        let mut episodes = vec![
            episode("1", true),
            episode("2", true),
            episode("3", false),
            episode("4", false),
        ];
        from_next_unplayed(&mut episodes);
        let ids: Vec<&str> = episodes.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["3", "4"]);
        let mut watched = vec![episode("1", true), episode("2", true)];
        from_next_unplayed(&mut watched);
        assert_eq!(watched.len(), 2, "all watched plays from the start");
        assert!(is_playable(ItemKind::Episode) && !is_playable(ItemKind::Series));
        assert!(plays_children(ItemKind::Series) && !plays_children(ItemKind::Person));
    }

    #[test]
    fn an_episode_plays_on_into_the_rest_of_the_show() {
        let episode = |id: &str, season: i32, number: i32| {
            item(&format!(
                r#"{{"Id": "{id}", "Type": "Episode", "ParentIndexNumber": {season},
                    "IndexNumber": {number}}}"#
            ))
        };
        let ids = |items: &[BaseItemDto]| -> Vec<String> {
            items.iter().map(|item| item.id.clone()).collect()
        };
        let show = vec![
            episode("s0e1", 0, 1),
            episode("s1e1", 1, 1),
            episode("s1e2", 1, 2),
            episode("s1e3", 1, 3),
            episode("s2e1", 2, 1),
            episode("s2e2", 2, 2),
        ];
        // As the server orders them, from the episode on or from the start.
        assert_eq!(
            ids(&following(&show[2], show[2..].to_vec())),
            ["s1e3", "s2e1", "s2e2"]
        );
        assert_eq!(
            ids(&following(&show[2], show.clone())),
            ["s1e3", "s2e1", "s2e2"]
        );
        assert!(following(&show[5], show.clone()).is_empty());
        // Without the episode in the list, by number; missing episodes the
        // server knows only from metadata are left out.
        let mut missing = episode("s2e1", 2, 1);
        missing.location_type = LocationType::Virtual;
        let shuffled = vec![
            episode("s2e2", 2, 2),
            missing,
            episode("s1e1", 1, 1),
            episode("s1e3", 1, 3),
        ];
        assert_eq!(
            ids(&following(&episode("s1e2", 1, 2), shuffled)),
            ["s1e3", "s2e2"]
        );
    }

    #[test]
    fn marks_follow_the_servers_answer() {
        let film = item(
            r#"{"Id": "m1", "Type": "Movie", "RunTimeTicks": 1000,
                "UserData": {"PlaybackPositionTicks": 500, "PlayedPercentage": 50, "Played": false}}"#,
        );
        let marks = Marks::of(&film);
        assert!(!marks.played && (marks.progress - 0.5).abs() < 1e-6);
        let data = UserItemDataDto {
            played: true,
            is_favorite: true,
            ..Default::default()
        };
        let after = Marks::from_data(&film, &data);
        assert!(after.played && after.favorite);
        assert_eq!(after.progress, -1.0);
        // Watched again, and stopped part-way through.
        let again = UserItemDataDto {
            played: true,
            playback_position_ticks: 250,
            played_percentage: Some(25.0),
            ..Default::default()
        };
        let marks = Marks::from_data(&film, &again);
        assert!(marks.played && (marks.progress - 0.25).abs() < 1e-6);
    }

    #[test]
    fn the_registry_is_keyed_by_entry() {
        let registry = session::Registry::default();
        let url = session::entry_url("srv", "m1").unwrap();
        let film = item(r#"{"Id": "m1", "Name": "Film", "Type": "Movie"}"#);
        registry.lock().unwrap().insert(
            url.clone(),
            item_info(
                "srv",
                &film,
                Mode::Play {
                    from_start: false,
                    shuffle: false,
                },
                None,
            ),
        );
        let found = registry
            .lock()
            .unwrap()
            .get("jellyfin://srv/m1")
            .cloned()
            .unwrap();
        assert_eq!(
            (found.title.as_str(), found.kind),
            ("Film", ItemKind::Movie)
        );
    }

    // Against the local test server (tools/jellyfin-server.sh).

    fn generated() -> bool {
        std::env::var("PATINA_TEST_JELLYFIN_LIBRARY").is_ok_and(|library| library == "generated")
    }

    /// Signs out when dropped, so that a failed test leaves no session.
    struct SignOut(Client);

    impl Drop for SignOut {
        fn drop(&mut self) {
            let _ = self.0.logout();
        }
    }

    /// A client signed in on the test server, on a device id of the test's
    /// own; None skips the test.
    fn live(device_id: &str) -> Option<(Client, SignOut)> {
        let (Ok(address), Ok(name), Ok(password)) = (
            std::env::var("PATINA_TEST_JELLYFIN_URL"),
            std::env::var("PATINA_TEST_JELLYFIN_USER"),
            std::env::var("PATINA_TEST_JELLYFIN_PASSWORD"),
        ) else {
            crate::mpv::testing::skip(
                "PATINA_TEST_REQUIRE_JELLYFIN",
                "needs a Jellyfin server in PATINA_TEST_JELLYFIN_URL",
            );
            return None;
        };
        let identity =
            super::super::auth::Identity::new(super::super::store::DEVICE_NAME, device_id);
        let connected = session::connect(&address, &identity, None).unwrap();
        let auth = connected
            .client
            .authenticate_by_name(&name, &password)
            .unwrap();
        let client = connected
            .client
            .with_session(&auth.access_token, &auth.user.id);
        Some((client.clone(), SignOut(client)))
    }

    fn titles(cards: &[LibraryCard]) -> Vec<String> {
        cards.iter().map(|card| card.title.to_string()).collect()
    }

    /// Home's loads and every library's grid, with the cards they make.
    #[test]
    fn live_home_and_grids_make_cards() {
        let Some((client, _sign_out)) = live("patina-tests-library-home") else {
            return;
        };
        let views: Vec<BaseItemDto> = client
            .views()
            .unwrap()
            .items
            .into_iter()
            .filter(|view| Collection::of(view.collection_type).is_some())
            .collect();
        assert!(!views.is_empty());
        let resume = client.resume(&resume_query()).unwrap().items;
        let next_up = client.next_up(&next_up_query()).unwrap().items;
        let resume_cards: Vec<LibraryCard> = resume
            .iter()
            .map(|item| card_of(item, Art::Thumb, Lines::Home))
            .collect();
        let next_cards: Vec<LibraryCard> = next_up
            .iter()
            .map(|item| card_of(item, Art::Thumb, Lines::Home))
            .collect();
        for view in &views {
            let collection = Collection::of(view.collection_type).unwrap();
            if collection.has_latest() {
                let latest = client.latest(&latest_query(&view.id)).unwrap();
                for item in &latest {
                    assert!(!card_of(item, Art::Poster, Lines::Home).title.is_empty());
                }
            }
            for tab in collection.tabs() {
                let page = GridPage {
                    tab: *tab,
                    ..GridPage::new(&view.id, &view.id, &view.name, collection)
                };
                let request = grid_request(&page, 0, 2);
                let result = request
                    .fetch(&client)
                    .unwrap_or_else(|error| panic!("{} {:?}: {error}", view.name, tab));
                assert!(result.items.len() <= 2);
                let cards: Vec<LibraryCard> = result
                    .items
                    .iter()
                    .map(|item| card_of(item, Art::Poster, Lines::Own))
                    .collect();
                if generated() {
                    assert!(!cards.is_empty(), "{} {tab:?} is empty", view.name);
                }
                for card in &cards {
                    assert!(!card.title.is_empty() && card.aspect > 0.0, "{card:?}");
                }
            }
        }
        if generated() {
            // patina has watched half of Paper Harbor and the first episode
            // of Lantern Street.
            assert!(titles(&resume_cards).contains(&"Paper Harbor".to_owned()));
            let paper = resume_cards
                .iter()
                .find(|card| card.title == "Paper Harbor")
                .unwrap();
            assert!(paper.progress > 0.0 && paper.progress < 1.0);
            let next = next_cards
                .iter()
                .find(|card| card.title == "Lantern Street")
                .unwrap();
            assert!(next.subtitle.starts_with("S1 E2"), "{next:?}");
            let kinds: Vec<Collection> = views
                .iter()
                .filter_map(|view| Collection::of(view.collection_type))
                .collect();
            for kind in [Collection::Movies, Collection::Shows, Collection::Music] {
                assert!(kinds.contains(&kind), "{kind:?}");
            }
        }
    }

    /// An item's page as its loads fill it, and what playing each plays.
    #[test]
    fn live_item_pages_and_what_they_play() {
        let Some((client, _sign_out)) = live("patina-tests-library-items") else {
            return;
        };
        let views = client.views().unwrap().items;
        let first = |collection: Collection, tab: Tab| {
            let view = views
                .iter()
                .find(|view| Collection::of(view.collection_type) == Some(collection))?;
            let page = GridPage {
                tab,
                ..GridPage::new(&view.id, &view.id, &view.name, collection)
            };
            grid_request(&page, 0, 1)
                .fetch(&client)
                .unwrap()
                .items
                .into_iter()
                .next()
        };
        let resume = Mode::Play {
            from_start: false,
            shuffle: false,
        };

        if let Some(film) = first(Collection::Movies, Tab::Movies) {
            let film = client.item(&film.id).unwrap();
            let detail = detail_of(&film, &Choice::default());
            assert!(detail.can_play && !detail.facts.is_empty());
            assert!(detail.versions.row_count() >= 1);
            client.similar(&film.id, &similar_query()).unwrap();
            let list = playable_list(&client, &film, resume).unwrap();
            assert_eq!(list.len(), 1);
        }
        if let Some(show) = first(Collection::Shows, Tab::Shows) {
            let seasons = client.seasons(&show.id, &seasons_query()).unwrap().items;
            assert!(!seasons.is_empty());
            let episodes = client
                .episodes(&show.id, &episodes_query(&seasons[0].id))
                .unwrap()
                .items;
            assert!(!episodes.is_empty());
            let tracks: Vec<LibraryTrack> = episodes
                .iter()
                .enumerate()
                .map(|(index, episode)| track_of(episode, Tracks::Episodes, false, index, ""))
                .collect();
            assert_eq!(tracks[0].number, "1");
            assert!(!tracks[0].duration.is_empty());
            let all = playable_list(
                &client,
                &show,
                Mode::Play {
                    from_start: true,
                    shuffle: false,
                },
            )
            .unwrap();
            let from_next = playable_list(&client, &show, resume).unwrap();
            assert!(from_next.len() <= all.len());
            if generated() {
                // Two seasons, the first episode watched.
                assert_eq!(seasons.len(), 2);
                assert_eq!(all.len() - from_next.len(), 1);
            }
            let shuffled = playable_list(
                &client,
                &show,
                Mode::Play {
                    from_start: true,
                    shuffle: true,
                },
            )
            .unwrap();
            // All of them in order: the window shuffles them.
            assert!(
                shuffled
                    .iter()
                    .map(|item| &item.id)
                    .eq(all.iter().map(|item| &item.id))
            );
        }
        // A song of a page's list plays the list from it: the page lists
        // the songs in the order the list plays them.
        let ids = |items: &[BaseItemDto]| -> Vec<String> {
            items.iter().map(|item| item.id.clone()).collect()
        };
        if let Some(album) = first(Collection::Music, Tab::Albums) {
            let songs = client.items(&album_tracks_query(&album.id)).unwrap().items;
            assert!(!songs.is_empty());
            assert_eq!(
                ids(&playable_list(&client, &album, resume).unwrap()),
                ids(&songs)
            );
            let artist = first(Collection::Music, Tab::Artists).unwrap();
            let albums = client
                .items(&artist_albums_query(&artist.id))
                .unwrap()
                .items;
            assert!(!albums.is_empty());
            let listed = client.items(&artist_songs_query(&artist.id)).unwrap().items;
            assert!(!listed.is_empty());
            assert_eq!(
                ids(&playable_list(&client, &artist, resume).unwrap()),
                ids(&listed)
            );
            if generated() {
                // The artist's one album, its songs by number.
                assert_eq!(ids(&listed), ids(&songs));
                let tracks: Vec<LibraryTrack> = listed
                    .iter()
                    .enumerate()
                    .map(|(index, song)| track_of(song, Tracks::Artist, false, index, ""))
                    .collect();
                assert_eq!(tracks[1].number, "2");
                assert_eq!(tracks[1].subtitle, "Low Tide");
            }
            let genre = first(Collection::Music, Tab::Genres).unwrap();
            assert_eq!(card_kind(&genre), "genre");
            let by_genre = client
                .items(&genre_query(&genre.id, ItemKind::MusicAlbum))
                .unwrap()
                .items;
            assert!(!by_genre.is_empty());
        }
        if let Some(playlist) = first(Collection::Music, Tab::Playlists) {
            let entries = client
                .playlist_items(&playlist.id, &playlist_query())
                .unwrap()
                .items;
            assert_eq!(
                ids(&playable_list(&client, &playlist, resume).unwrap()),
                ids(&entries)
            );
        }
        if let Some(collection) = first(Collection::BoxSets, Tab::Collections) {
            let items = client
                .items(&collection_items_query(&collection.id))
                .unwrap()
                .items;
            assert!(!items.is_empty());
        }
        // Search finds what the grids list.
        if generated() {
            let found = client
                .search("Night", &[ItemKind::Movie], SEARCH_LIMIT)
                .unwrap();
            assert!(found.items.iter().any(|item| item.name == "Night Signal"));
            let artists = client.search_artists("Mira", SEARCH_LIMIT).unwrap();
            assert!(!artists.items.is_empty());
        }
    }

    /// An episode played on its own plays on into the rest of the show.
    #[test]
    fn live_an_episode_plays_on_into_the_rest_of_the_show() {
        let Some((client, _sign_out)) = live("patina-tests-library-next") else {
            return;
        };
        let episodes = client
            .items(
                &ItemsQuery::new()
                    .types(&[ItemKind::Episode])
                    .recursive()
                    .sort(
                        &["SeriesSortName", "ParentIndexNumber", "IndexNumber"],
                        SortOrder::Ascending,
                    )
                    .limit(50),
            )
            .unwrap()
            .items;
        let Some(second) = episodes.get(1) else {
            assert!(!generated(), "the generated show has episodes");
            return;
        };
        let next = next_episodes(&client, second).unwrap();
        let mut numbers: Vec<(i32, i32)> = next
            .iter()
            .map(|item| {
                (
                    item.parent_index_number.unwrap_or(0),
                    item.index_number.unwrap_or(0),
                )
            })
            .collect();
        assert!(next.iter().all(|item| item.series_id == second.series_id));
        assert!(!next.iter().any(|item| item.id == second.id));
        let sorted = {
            numbers.dedup();
            let mut sorted = numbers.clone();
            sorted.sort_unstable();
            sorted
        };
        assert_eq!(numbers, sorted, "in the order they air");
        if generated() {
            assert_eq!(numbers, [(1, 3), (2, 1), (2, 2)]);
            assert_eq!(
                display_title(second),
                "Lantern Street · S1 E2 · Night Market"
            );
        }
    }
}
