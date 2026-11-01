//! The server's JSON, as serde structs. Jellyfin writes PascalCase names
//! and leaves out whatever is null, so every struct takes the fields it
//! knows, ignores the rest and gives the missing ones their default: a
//! newer server with fields Patina has never heard of, or an older one
//! without some, still parses. Values from a closed list (item kinds,
//! stream kinds and so on) are enums with an `Unknown` for the ones a
//! later server may add.
//!
//! Only what docs/jellyfin.md needs is here; the server's types have many
//! more fields.

use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;

/// Jellyfin counts time in ticks of 100 ns.
pub const TICKS_PER_SECOND: i64 = 10_000_000;

pub fn ticks_to_seconds(ticks: i64) -> f64 {
    ticks as f64 / TICKS_PER_SECOND as f64
}

/// Never below 0: mpv's positions can be (audio timestamps, arithmetic on
/// them), and the server refuses a stopped report with a negative position
/// (400), so the stop would never be recorded. Not a number is 0 too.
pub fn seconds_to_ticks(seconds: f64) -> i64 {
    (seconds.max(0.0) * TICKS_PER_SECOND as f64).round() as i64
}

/// A URL the server hands out, without the token it writes into it as
/// `ApiKey`, so that the token is never kept, shown or logged with the
/// models; requests carry it in their header instead.
fn without_api_key<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let url = Option::<String>::deserialize(deserializer)?;
    Ok(url.map(|url| super::auth::strip_api_key(&url)))
}

/// A map whose values the server may write as null (it leaves out null
/// fields, but not null entries in a dictionary), without those entries.
fn without_nulls<'de, D, V>(deserializer: D) -> Result<HashMap<String, V>, D::Error>
where
    D: Deserializer<'de>,
    V: Deserialize<'de>,
{
    let map = Option::<HashMap<String, Option<V>>>::deserialize(deserializer)?;
    Ok(map
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(key, value)| Some((key, value?)))
        .collect())
}

/// `ImageBlurHashes`: image type, then image tag, to blurhash.
fn blurhashes<'de, D>(deserializer: D) -> Result<HashMap<String, HashMap<String, String>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct Tags(#[serde(deserialize_with = "without_nulls")] HashMap<String, String>);
    let map: HashMap<String, Tags> = without_nulls(deserializer)?;
    Ok(map.into_iter().map(|(kind, tags)| (kind, tags.0)).collect())
}

/// `Trickplay`: media source id, then thumbnail width, to the tiles' layout.
fn trickplay_map<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, HashMap<String, TrickplayInfo>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct Widths(#[serde(deserialize_with = "without_nulls")] HashMap<String, TrickplayInfo>);
    let map: HashMap<String, Widths> = without_nulls(deserializer)?;
    Ok(map
        .into_iter()
        .map(|(source, widths)| (source, widths.0))
        .collect())
}

/// What a server tells anyone who asks, before signing in.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct PublicSystemInfo {
    pub local_address: String,
    pub server_name: String,
    /// "10.10.7", "10.11.11", "12.4.0": 12.0 dropped the leading "10.".
    pub version: String,
    /// "Jellyfin Server"; Emby, which Jellyfin forked from, says otherwise.
    pub product_name: String,
    /// The server's identity, which outlives its addresses.
    pub id: String,
    pub startup_wizard_completed: bool,
}

/// The oldest server Patina supports: one code path covers 10.10.7 to 12.x.
pub const MINIMUM_VERSION: (u32, u32, u32) = (10, 10, 7);

impl PublicSystemInfo {
    /// The version as numbers, when it reads as one.
    pub fn version_number(&self) -> Option<(u32, u32, u32)> {
        let mut parts = self.version.trim().split('.').map(|part| {
            part.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse::<u32>()
                .ok()
        });
        let major = parts.next()??;
        let minor = parts.next().flatten().unwrap_or(0);
        let patch = parts.next().flatten().unwrap_or(0);
        Some((major, minor, patch))
    }

    /// False for a server older than Patina supports; true when the
    /// version can't be read, since a server that answers like Jellyfin
    /// is more likely new than old.
    pub fn supported(&self) -> bool {
        self.version_number()
            .is_none_or(|version| version >= MINIMUM_VERSION)
    }

    pub fn is_jellyfin(&self) -> bool {
        !self.id.is_empty()
            && (self.product_name.is_empty() || self.product_name == "Jellyfin Server")
    }
}

/// An account, as `/Users/Me` and `/Users/Public` describe it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct UserDto {
    pub id: String,
    pub name: String,
    pub server_id: String,
    pub primary_image_tag: Option<String>,
    pub has_password: bool,
    pub has_configured_password: bool,
    pub last_login_date: Option<String>,
    pub last_activity_date: Option<String>,
    pub configuration: UserConfiguration,
    pub policy: UserPolicy,
}

/// How the subtitle language preference is applied, as the account chose
/// in Jellyfin's settings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub enum SubtitleMode {
    Always,
    OnlyForced,
    #[serde(rename = "None")]
    Off,
    /// Subtitles when the audio isn't in the preferred language.
    Smart,
    /// Subtitles the file marks default or forced.
    #[default]
    #[serde(other)]
    Default,
}

/// The account's playback settings. The defaults are the server's, for a
/// server that leaves the whole thing out.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct UserConfiguration {
    /// ISO 639-2 codes, as "eng"; absent for no preference.
    pub audio_language_preference: Option<String>,
    pub subtitle_language_preference: Option<String>,
    pub subtitle_mode: SubtitleMode,
    /// Play the file's default audio track rather than the preferred
    /// language's.
    pub play_default_audio_track: bool,
    pub enable_next_episode_auto_play: bool,
    pub remember_audio_selections: bool,
    pub remember_subtitle_selections: bool,
    pub display_missing_episodes: bool,
    pub hide_played_in_latest: bool,
    /// Library ids in the order the account arranged them.
    pub ordered_views: Vec<String>,
    /// Libraries left out of Latest and of My Media.
    pub latest_items_excludes: Vec<String>,
    pub my_media_excludes: Vec<String>,
}

impl Default for UserConfiguration {
    fn default() -> Self {
        Self {
            audio_language_preference: None,
            subtitle_language_preference: None,
            subtitle_mode: SubtitleMode::Default,
            play_default_audio_track: true,
            enable_next_episode_auto_play: true,
            remember_audio_selections: true,
            remember_subtitle_selections: true,
            display_missing_episodes: false,
            hide_played_in_latest: true,
            ordered_views: Vec::new(),
            latest_items_excludes: Vec::new(),
            my_media_excludes: Vec::new(),
        }
    }
}

/// What the account may do, of what changes what Patina offers. Defaults
/// are the server's own.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct UserPolicy {
    pub is_administrator: bool,
    pub is_disabled: bool,
    /// Without it nothing plays.
    pub enable_media_playback: bool,
    pub enable_audio_playback_transcoding: bool,
    pub enable_video_playback_transcoding: bool,
    pub enable_playback_remuxing: bool,
    /// Whether the account may download files (`GET /Items/{id}/Download`,
    /// which the server refuses with 403 without it); Patina offers no
    /// Download then.
    pub enable_content_downloading: bool,
    pub enable_live_tv_access: bool,
    /// Bits per second the server allows this account away from home; 0
    /// for no limit.
    pub remote_client_bitrate_limit: i64,
    /// "CreateAndJoinGroups", "JoinGroups" or "None".
    pub sync_play_access: String,
}

impl Default for UserPolicy {
    fn default() -> Self {
        Self {
            is_administrator: false,
            is_disabled: false,
            enable_media_playback: true,
            enable_audio_playback_transcoding: true,
            enable_video_playback_transcoding: true,
            enable_playback_remuxing: true,
            enable_content_downloading: true,
            enable_live_tv_access: true,
            remote_client_bitrate_limit: 0,
            sync_play_access: String::new(),
        }
    }
}

/// A sign-in's answer: the account and its token. Debug leaves the token
/// out, as `Client`'s does, so that it can't reach a log or a test's
/// failure message.
#[derive(Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct AuthenticationResult {
    pub user: UserDto,
    pub access_token: String,
    pub server_id: String,
}

impl std::fmt::Debug for AuthenticationResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthenticationResult")
            .field("user", &self.user)
            .field("access_token", &hidden(&self.access_token))
            .field("server_id", &self.server_id)
            .finish()
    }
}

/// What Debug shows for a secret: whether there is one.
fn hidden(secret: &str) -> Option<&'static str> {
    (!secret.is_empty()).then_some("(set)")
}

/// A Quick Connect request: the code to show, and the secret that asks
/// after it, and once approved signs in with it. Debug leaves the secret
/// out.
#[derive(Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct QuickConnectResult {
    pub authenticated: bool,
    pub secret: String,
    /// Six digits, for the user to enter in another Jellyfin app.
    pub code: String,
    pub device_id: String,
    pub device_name: String,
    pub app_name: String,
    pub app_version: String,
    pub date_added: Option<String>,
}

impl std::fmt::Debug for QuickConnectResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuickConnectResult")
            .field("authenticated", &self.authenticated)
            .field("secret", &hidden(&self.secret))
            .field("code", &self.code)
            .field("device_id", &self.device_id)
            .field("device_name", &self.device_name)
            .field("app_name", &self.app_name)
            .field("app_version", &self.app_version)
            .field("date_added", &self.date_added)
            .finish()
    }
}

/// A page of results. `/Items/Latest` answers with a bare list instead.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct QueryResult<T> {
    pub items: Vec<T>,
    /// Every match, over all pages.
    pub total_record_count: i64,
    pub start_index: i64,
}

impl<T> Default for QueryResult<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            total_record_count: 0,
            start_index: 0,
        }
    }
}

/// `BaseItemKind`: what an item is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Deserialize)]
pub enum ItemKind {
    AggregateFolder,
    Audio,
    AudioBook,
    BasePluginFolder,
    Book,
    BoxSet,
    Channel,
    ChannelFolderItem,
    /// A library.
    CollectionFolder,
    Episode,
    Folder,
    Genre,
    ManualPlaylistsFolder,
    Movie,
    LiveTvChannel,
    LiveTvProgram,
    MusicAlbum,
    MusicArtist,
    MusicGenre,
    MusicVideo,
    Person,
    Photo,
    PhotoAlbum,
    Playlist,
    PlaylistsFolder,
    Program,
    Recording,
    Season,
    Series,
    Studio,
    Trailer,
    TvChannel,
    TvProgram,
    UserRootFolder,
    /// A library, or a view the server makes of one.
    UserView,
    Video,
    Year,
    #[default]
    #[serde(other)]
    Unknown,
}

impl ItemKind {
    /// The name in queries' `includeItemTypes`.
    pub fn as_str(self) -> &'static str {
        match self {
            ItemKind::AggregateFolder => "AggregateFolder",
            ItemKind::Audio => "Audio",
            ItemKind::AudioBook => "AudioBook",
            ItemKind::BasePluginFolder => "BasePluginFolder",
            ItemKind::Book => "Book",
            ItemKind::BoxSet => "BoxSet",
            ItemKind::Channel => "Channel",
            ItemKind::ChannelFolderItem => "ChannelFolderItem",
            ItemKind::CollectionFolder => "CollectionFolder",
            ItemKind::Episode => "Episode",
            ItemKind::Folder => "Folder",
            ItemKind::Genre => "Genre",
            ItemKind::ManualPlaylistsFolder => "ManualPlaylistsFolder",
            ItemKind::Movie => "Movie",
            ItemKind::LiveTvChannel => "LiveTvChannel",
            ItemKind::LiveTvProgram => "LiveTvProgram",
            ItemKind::MusicAlbum => "MusicAlbum",
            ItemKind::MusicArtist => "MusicArtist",
            ItemKind::MusicGenre => "MusicGenre",
            ItemKind::MusicVideo => "MusicVideo",
            ItemKind::Person => "Person",
            ItemKind::Photo => "Photo",
            ItemKind::PhotoAlbum => "PhotoAlbum",
            ItemKind::Playlist => "Playlist",
            ItemKind::PlaylistsFolder => "PlaylistsFolder",
            ItemKind::Program => "Program",
            ItemKind::Recording => "Recording",
            ItemKind::Season => "Season",
            ItemKind::Series => "Series",
            ItemKind::Studio => "Studio",
            ItemKind::Trailer => "Trailer",
            ItemKind::TvChannel => "TvChannel",
            ItemKind::TvProgram => "TvProgram",
            ItemKind::UserRootFolder => "UserRootFolder",
            ItemKind::UserView => "UserView",
            ItemKind::Video => "Video",
            ItemKind::Year => "Year",
            ItemKind::Unknown => "Unknown",
        }
    }
}

/// `MediaType`: what playing an item means.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub enum MediaKind {
    Video,
    Audio,
    Photo,
    Book,
    #[default]
    #[serde(other)]
    Unknown,
}

/// A library's kind, `CollectionType`, which the server writes in lower
/// case.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CollectionKind {
    Movies,
    TvShows,
    Music,
    MusicVideos,
    Trailers,
    HomeVideos,
    BoxSets,
    Books,
    Photos,
    LiveTv,
    Playlists,
    /// A library of mixed content.
    Folders,
    #[default]
    #[serde(other)]
    Unknown,
}

/// Where an item is: a virtual one is a missing episode the server knows
/// of from metadata, with nothing to play.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub enum LocationType {
    FileSystem,
    Remote,
    Virtual,
    Offline,
    #[default]
    #[serde(other)]
    Unknown,
}

/// An item's artwork, as the image URLs name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageType {
    Primary,
    Backdrop,
    Logo,
    Thumb,
}

impl ImageType {
    pub fn as_str(self) -> &'static str {
        match self {
            ImageType::Primary => "Primary",
            ImageType::Backdrop => "Backdrop",
            ImageType::Logo => "Logo",
            ImageType::Thumb => "Thumb",
        }
    }
}

/// A name with the id of the item it names: a studio, an artist.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct NameIdPair {
    pub name: String,
    pub id: String,
}

/// An item: a library, a film, a show, a season, an episode, an album, a
/// track, a person.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct BaseItemDto {
    pub id: String,
    pub name: String,
    pub original_title: Option<String>,
    pub sort_name: Option<String>,
    pub server_id: String,
    #[serde(rename = "Type")]
    pub kind: ItemKind,
    pub media_type: MediaKind,
    pub is_folder: bool,
    pub collection_type: CollectionKind,
    pub parent_id: Option<String>,
    pub overview: Option<String>,
    pub taglines: Vec<String>,
    pub production_year: Option<i32>,
    /// ISO 8601, as "2024-05-17T00:00:00.0000000Z".
    pub premiere_date: Option<String>,
    pub date_created: Option<String>,
    pub run_time_ticks: Option<i64>,
    /// The episode's number in its season, the track's on its disc.
    pub index_number: Option<i32>,
    /// The last episode's number, for a file with several.
    pub index_number_end: Option<i32>,
    /// The season's number, the disc's.
    pub parent_index_number: Option<i32>,
    pub series_id: Option<String>,
    pub series_name: Option<String>,
    pub season_id: Option<String>,
    pub season_name: Option<String>,
    pub album: Option<String>,
    pub album_id: Option<String>,
    pub album_artist: Option<String>,
    pub album_artists: Vec<NameIdPair>,
    pub artists: Vec<String>,
    pub artist_items: Vec<NameIdPair>,
    pub genres: Vec<String>,
    pub studios: Vec<NameIdPair>,
    pub people: Vec<BaseItemPerson>,
    pub community_rating: Option<f32>,
    pub critic_rating: Option<f32>,
    /// The age rating, as "PG-13".
    pub official_rating: Option<String>,
    pub child_count: Option<i32>,
    pub recursive_item_count: Option<i32>,
    pub has_lyrics: bool,
    /// The entry's own id in a playlist, which removing and moving it
    /// takes.
    pub playlist_item_id: Option<String>,
    /// Image type to the tag that identifies the image's version.
    #[serde(deserialize_with = "without_nulls")]
    pub image_tags: HashMap<String, String>,
    /// One tag per backdrop; a backdrop's index is its place here.
    pub backdrop_image_tags: Vec<String>,
    #[serde(deserialize_with = "blurhashes")]
    pub image_blur_hashes: HashMap<String, HashMap<String, String>>,
    /// Artwork an episode or season borrows from its show, a track from
    /// its album.
    pub parent_backdrop_item_id: Option<String>,
    pub parent_backdrop_image_tags: Vec<String>,
    pub parent_thumb_item_id: Option<String>,
    pub parent_thumb_image_tag: Option<String>,
    pub parent_logo_item_id: Option<String>,
    pub parent_logo_image_tag: Option<String>,
    pub parent_primary_image_item_id: Option<String>,
    pub parent_primary_image_tag: Option<String>,
    pub series_primary_image_tag: Option<String>,
    pub series_thumb_image_tag: Option<String>,
    pub album_primary_image_tag: Option<String>,
    /// Width over height of the primary image: 2/3 for a poster, 16/9
    /// for an episode's still, 1 for a cover.
    pub primary_image_aspect_ratio: Option<f64>,
    pub user_data: Option<UserItemDataDto>,
    /// The versions of the item; `GET /Items/{id}` fills these.
    pub media_sources: Vec<MediaSourceInfo>,
    pub media_source_count: Option<i32>,
    pub media_streams: Vec<MediaStream>,
    pub chapters: Vec<ChapterInfo>,
    #[serde(deserialize_with = "trickplay_map")]
    pub trickplay: HashMap<String, HashMap<String, TrickplayInfo>>,
    pub container: Option<String>,
    pub location_type: LocationType,
    /// The file or folder on the server: what a download is named when
    /// the server gives no name. Sent only when asked for (`Path`).
    pub path: Option<String>,
    /// Whether the account may download this item's file: the server's
    /// own check, of the account's policy and of the item (a file of its
    /// own, not a stream from elsewhere or a disc's folder; never a
    /// folder). Sent only when asked for (`CanDownload`); `GET
    /// /Items/{id}` asks for every field.
    pub can_download: Option<bool>,
}

impl BaseItemDto {
    pub fn duration(&self) -> Option<f64> {
        self.run_time_ticks
            .filter(|ticks| *ticks > 0)
            .map(ticks_to_seconds)
    }

    /// Where the server says to resume, in seconds; None from the start.
    pub fn resume_position(&self) -> Option<f64> {
        self.user_data
            .as_ref()
            .map(|data| data.playback_position_ticks)
            .filter(|ticks| *ticks > 0)
            .map(ticks_to_seconds)
    }

    pub fn played(&self) -> bool {
        self.user_data.as_ref().is_some_and(|data| data.played)
    }

    pub fn is_favorite(&self) -> bool {
        self.user_data.as_ref().is_some_and(|data| data.is_favorite)
    }

    pub fn image_tag(&self, kind: ImageType) -> Option<&str> {
        self.image_tags.get(kind.as_str()).map(String::as_str)
    }

    /// The placeholder for an image until it arrives.
    pub fn blurhash(&self, kind: ImageType, tag: &str) -> Option<&str> {
        self.image_blur_hashes
            .get(kind.as_str())?
            .get(tag)
            .map(String::as_str)
    }

    /// The sizes of trickplay tiles the server has for a version of the
    /// item, narrowest first; none when it has none.
    pub fn trickplay_sizes(&self, media_source_id: &str) -> Vec<TrickplayInfo> {
        let mut sizes: Vec<TrickplayInfo> = self
            .trickplay
            .get(media_source_id)
            .map(|sizes| sizes.values().cloned().collect())
            .unwrap_or_default();
        sizes.sort_by_key(|info| info.width);
        sizes
    }
}

/// What the account has done with an item.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct UserItemDataDto {
    /// The resume position; 0 for none.
    pub playback_position_ticks: i64,
    pub play_count: i32,
    pub is_favorite: bool,
    pub played: bool,
    pub played_percentage: Option<f64>,
    /// Episodes left to watch, for a show or season.
    pub unplayed_item_count: Option<i32>,
    pub last_played_date: Option<String>,
    pub likes: Option<bool>,
    pub item_id: String,
}

/// Someone in an item's cast or crew.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct BaseItemPerson {
    pub id: String,
    pub name: String,
    /// The character, for an actor.
    pub role: Option<String>,
    /// "Actor", "Director", "Writer", "GuestStar", "Composer"...
    #[serde(rename = "Type")]
    pub kind: String,
    pub primary_image_tag: Option<String>,
    #[serde(deserialize_with = "blurhashes")]
    pub image_blur_hashes: HashMap<String, HashMap<String, String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct ChapterInfo {
    pub start_position_ticks: i64,
    pub name: Option<String>,
    pub image_tag: Option<String>,
}

/// How a version's trickplay thumbnails are laid out: `tile_width` by
/// `tile_height` of them to a tile image, one every `interval`
/// milliseconds.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct TrickplayInfo {
    /// One thumbnail's size in pixels.
    pub width: u32,
    pub height: u32,
    /// Thumbnails across and down a tile.
    pub tile_width: u32,
    pub tile_height: u32,
    pub thumbnail_count: u32,
    pub interval: u32,
    pub bandwidth: u32,
}

impl TrickplayInfo {
    /// The thumbnail for a position in seconds: the tile's index, and the
    /// thumbnail's top left corner in it. None when there is none, or when
    /// the sizes the server gave make a tile larger than the image cache
    /// decodes: they come from the server, and are worked out in u64 so
    /// that no size overflows.
    pub fn locate(&self, seconds: f64) -> Option<(u32, u32, u32)> {
        let [width, height, across, down] =
            [self.width, self.height, self.tile_width, self.tile_height].map(u64::from);
        let largest = u64::from(super::images::MAX_SIDE);
        if self.interval == 0
            || self.thumbnail_count == 0
            || across == 0
            || down == 0
            || across * width > largest
            || down * height > largest
        {
            return None;
        }
        let per_tile = across * down;
        let milliseconds = (seconds.max(0.0) * 1000.0) as u64;
        let number =
            (milliseconds / u64::from(self.interval)).min(u64::from(self.thumbnail_count - 1));
        let within = number % per_tile;
        Some((
            u32::try_from(number / per_tile).ok()?,
            u32::try_from(within % across * width).ok()?,
            u32::try_from(within / across * height).ok()?,
        ))
    }
}

/// One version of an item, as the server can deliver it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct MediaSourceInfo {
    pub id: String,
    /// "File", "Http"...
    pub protocol: String,
    pub path: Option<String>,
    pub container: Option<String>,
    pub size: Option<i64>,
    /// The version's name, which the version menu shows.
    pub name: Option<String>,
    pub is_remote: bool,
    /// The static stream URL's `Tag`.
    #[serde(rename = "ETag")]
    pub etag: Option<String>,
    pub run_time_ticks: Option<i64>,
    pub supports_direct_play: bool,
    pub supports_direct_stream: bool,
    pub supports_transcoding: bool,
    /// Relative to the server's address; only there when the server
    /// decided to transcode. The server writes the token into it as
    /// `ApiKey`, which is removed as it is read.
    #[serde(deserialize_with = "without_api_key")]
    pub transcoding_url: Option<String>,
    /// "http" or "hls".
    pub transcoding_sub_protocol: Option<String>,
    pub transcoding_container: Option<String>,
    pub media_streams: Vec<MediaStream>,
    pub media_attachments: Vec<MediaAttachment>,
    pub bitrate: Option<i64>,
    /// Indexes into the streams: the server's choice, from the account's
    /// preferences and the request.
    pub default_audio_stream_index: Option<i32>,
    pub default_subtitle_stream_index: Option<i32>,
    pub requires_opening: bool,
    pub requires_closing: bool,
    pub live_stream_id: Option<String>,
    pub is_infinite_stream: bool,
    pub has_segments: bool,
    /// "VideoFile", "Iso", "Dvd", "BluRay".
    pub video_type: Option<String>,
    #[serde(deserialize_with = "without_nulls")]
    pub required_http_headers: HashMap<String, String>,
}

/// `MediaStreamType`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub enum StreamKind {
    Audio,
    Video,
    Subtitle,
    EmbeddedImage,
    Data,
    Lyric,
    #[default]
    #[serde(other)]
    Unknown,
}

/// How a subtitle reaches the player, once the server has decided.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub enum DeliveryMethod {
    /// Burned into the picture by a transcode.
    Encode,
    /// In the file or stream itself.
    Embed,
    /// A file of its own, at `DeliveryUrl`.
    External,
    /// In the HLS playlist.
    Hls,
    /// Not delivered.
    Drop,
    #[default]
    #[serde(other)]
    Unknown,
}

/// One stream of a version. `index` counts every stream, external
/// subtitles first.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct MediaStream {
    #[serde(rename = "Type")]
    pub kind: StreamKind,
    pub index: i32,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
    /// The server's own label, as "English - Dolby Digital - 5.1 - Default".
    pub display_title: Option<String>,
    pub is_default: bool,
    pub is_forced: bool,
    pub is_hearing_impaired: bool,
    pub is_external: bool,
    pub is_text_subtitle_stream: bool,
    pub supports_external_stream: bool,
    pub delivery_method: Option<DeliveryMethod>,
    /// Relative to the server's address unless `is_external_url`, and
    /// without the `ApiKey` the server writes into it.
    #[serde(deserialize_with = "without_api_key")]
    pub delivery_url: Option<String>,
    pub is_external_url: Option<bool>,
    pub path: Option<String>,
    pub channels: Option<i32>,
    pub channel_layout: Option<String>,
    pub sample_rate: Option<i32>,
    pub bit_rate: Option<i64>,
    pub bit_depth: Option<i32>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub average_frame_rate: Option<f32>,
    pub real_frame_rate: Option<f32>,
    pub profile: Option<String>,
    /// "SDR", "HDR".
    pub video_range: Option<String>,
    /// "SDR", "HDR10", "HLG", "DOVI", "DOVIWithHDR10"...
    pub video_range_type: Option<String>,
    pub is_interlaced: bool,
    pub aspect_ratio: Option<String>,
}

/// A file in the container other than a stream: fonts, for subtitles.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct MediaAttachment {
    pub index: i32,
    pub codec: Option<String>,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    /// Without the `ApiKey` the server writes into it.
    #[serde(deserialize_with = "without_api_key")]
    pub delivery_url: Option<String>,
}

/// The body of `POST /Items/{id}/PlaybackInfo` (the server's
/// `PlaybackInfoDto`): what Patina can play and what the user chose.
/// Stream indexes count only with `media_source_id`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlaybackInfoRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_streaming_bitrate: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time_ticks: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_stream_index: Option<i32>,
    /// -1 turns subtitles off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle_stream_index: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_audio_channels: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_source_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_stream_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_profile: Option<DeviceProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_direct_play: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_direct_stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_transcoding: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_video_stream_copy: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_audio_stream_copy: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_open_live_stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub always_burn_in_subtitle_when_transcoding: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct PlaybackInfoResponse {
    pub media_sources: Vec<MediaSourceInfo>,
    /// Goes with every report about this playback.
    pub play_session_id: Option<String>,
    /// "NotAllowed", "NoCompatibleStream" or "RateLimitExceeded".
    pub error_code: Option<String>,
}

/// What Patina tells the server it can play. Everything plays directly:
/// a direct-play profile with no containers or codecs accepts any. The
/// transcoding profiles say what to make when the server must, or when
/// the codec profiles rule something out.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct DeviceProfile {
    pub name: String,
    pub max_streaming_bitrate: i64,
    pub max_static_bitrate: i64,
    pub music_streaming_transcoding_bitrate: i64,
    pub direct_play_profiles: Vec<DirectPlayProfile>,
    pub transcoding_profiles: Vec<TranscodingProfile>,
    pub codec_profiles: Vec<CodecProfile>,
    pub subtitle_profiles: Vec<SubtitleProfile>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct DirectPlayProfile {
    /// "Video" or "Audio".
    #[serde(rename = "Type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_codec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_codec: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct TranscodingProfile {
    #[serde(rename = "Type")]
    pub kind: String,
    pub container: String,
    /// "hls" or "http".
    pub protocol: String,
    pub context: String,
    /// Comma-separated, the one to make first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_codec: Option<String>,
    pub audio_codec: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_audio_channels: Option<String>,
}

/// Conditions a stream of the codec (every codec, without one) must meet
/// to play directly; one that fails makes the server transcode.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct CodecProfile {
    /// "Video", "VideoAudio" or "Audio".
    #[serde(rename = "Type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    pub conditions: Vec<ProfileCondition>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ProfileCondition {
    /// "Equals", "NotEquals", "LessThanEqual", "GreaterThanEqual" or
    /// "EqualsAny" (values separated by "|").
    pub condition: String,
    /// "Width", "Height", "VideoRangeType"...
    pub property: String,
    pub value: String,
    /// Whether a stream the server knows nothing of fails it.
    pub is_required: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct SubtitleProfile {
    /// The stream's codec as the server names it: "subrip", "ass",
    /// "pgssub".
    pub format: String,
    /// "External", "Embed", "Encode" or "Hls".
    pub method: String,
}

/// What to have the server transcode though mpv could play it, for
/// computers that can't decode it well, and the bitrate a lower quality
/// asks for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TranscodeRules {
    /// Bits per second; None plays at the source's own.
    pub max_bitrate: Option<i64>,
    pub hevc: bool,
    pub av1: bool,
    pub hdr: bool,
    /// Anything over 1920 by 1080.
    pub over_1080p: bool,
}

/// High enough that no source is held back by it: direct play takes
/// whatever the file has.
const UNLIMITED_BITRATE: i64 = 1_000_000_000;

impl DeviceProfile {
    pub fn patina(rules: &TranscodeRules) -> Self {
        let any = |kind: &str| DirectPlayProfile {
            kind: kind.into(),
            container: None,
            video_codec: None,
            audio_codec: None,
        };
        let condition =
            |condition: &str, property: &str, value: &str, is_required| ProfileCondition {
                condition: condition.into(),
                property: property.into(),
                value: value.into(),
                is_required,
            };
        // No video is narrower than one pixel, so this rules a codec out;
        // required, so that a stream of unknown width is ruled out too.
        let never = || vec![condition("LessThanEqual", "Width", "0", true)];
        let mut codec_profiles = Vec::new();
        if rules.hevc {
            codec_profiles.push(CodecProfile {
                kind: "Video".into(),
                codec: Some("hevc".into()),
                conditions: never(),
            });
        }
        if rules.av1 {
            codec_profiles.push(CodecProfile {
                kind: "Video".into(),
                codec: Some("av1".into()),
                conditions: never(),
            });
        }
        let mut limits = Vec::new();
        if rules.hdr {
            // Dolby Vision with an SDR base layer plays as SDR.
            limits.push(condition(
                "EqualsAny",
                "VideoRangeType",
                "SDR|DOVIWithSDR",
                false,
            ));
        }
        if rules.over_1080p {
            limits.push(condition("LessThanEqual", "Width", "1920", false));
            limits.push(condition("LessThanEqual", "Height", "1080", false));
        }
        if !limits.is_empty() {
            codec_profiles.push(CodecProfile {
                kind: "Video".into(),
                codec: None,
                conditions: limits,
            });
        }
        let subtitle = |format: &str, method: &str| SubtitleProfile {
            format: format.into(),
            method: method.into(),
        };
        let mut subtitle_profiles: Vec<SubtitleProfile> =
            ["subrip", "srt", "ass", "ssa", "webvtt", "vtt", "pgssub"]
                .iter()
                .map(|format| subtitle(format, "External"))
                .collect();
        subtitle_profiles.extend(
            [
                "subrip", "srt", "ass", "ssa", "webvtt", "mov_text", "pgssub", "dvdsub", "dvbsub",
            ]
            .iter()
            .map(|format| subtitle(format, "Embed")),
        );
        let bitrate = rules.max_bitrate.unwrap_or(UNLIMITED_BITRATE);
        Self {
            name: "Patina".into(),
            max_streaming_bitrate: bitrate,
            max_static_bitrate: bitrate,
            music_streaming_transcoding_bitrate: 320_000,
            direct_play_profiles: vec![any("Video"), any("Audio")],
            transcoding_profiles: vec![
                TranscodingProfile {
                    kind: "Video".into(),
                    container: "ts".into(),
                    protocol: "hls".into(),
                    context: "Streaming".into(),
                    // Not into HEVC when HEVC is what can't be decoded.
                    video_codec: Some(if rules.hevc { "h264" } else { "h264,hevc" }.into()),
                    // What HLS in MPEG-TS can carry: the server drops any
                    // other codec from the list (Opus and FLAC need fMP4
                    // segments), so those are always encoded again.
                    audio_codec: "aac,ac3,eac3,mp3".into(),
                    max_audio_channels: Some("8".into()),
                },
                TranscodingProfile {
                    kind: "Audio".into(),
                    container: "mp3".into(),
                    protocol: "http".into(),
                    context: "Streaming".into(),
                    video_codec: None,
                    audio_codec: "mp3".into(),
                    max_audio_channels: Some("2".into()),
                },
            ],
            codec_profiles,
            subtitle_profiles,
        }
    }
}

/// How a playback is delivered, for the reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub enum PlayMethod {
    #[default]
    DirectPlay,
    DirectStream,
    Transcode,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct QueueItem {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playlist_item_id: Option<String>,
}

/// The body of `POST /Sessions/Playing/Progress`, and of
/// `/Sessions/Playing`, which takes the same fields.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlaybackProgressInfo {
    pub item_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_source_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub play_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_stream_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position_ticks: Option<i64>,
    pub is_paused: bool,
    pub is_muted: bool,
    /// 0 to 100.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume_level: Option<i32>,
    /// The server's stream indexes, not mpv's track ids.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_stream_index: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle_stream_index: Option<i32>,
    pub play_method: PlayMethod,
    pub can_seek: bool,
    /// "RepeatNone", "RepeatAll" or "RepeatOne".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeat_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playlist_item_id: Option<String>,
    /// When playback started, in ticks since the Unix epoch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playback_start_time_ticks: Option<i64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub now_playing_queue: Vec<QueueItem>,
}

pub type PlaybackStartInfo = PlaybackProgressInfo;

/// The body of `POST /Sessions/Playing/Stopped`, which also ends a
/// transcode.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlaybackStopInfo {
    pub item_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_source_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub play_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_stream_id: Option<String>,
    /// Where playback stopped, always sent: without it the server takes
    /// the item as played to the end and marks it played. From it the
    /// server decides between watched, a resume position and neither.
    pub position_ticks: i64,
    /// Playback failed: the server then changes nothing of the account's.
    pub failed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playlist_item_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub now_playing_queue: Vec<QueueItem>,
}

/// `MediaSegmentType`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Deserialize)]
pub enum SegmentKind {
    Intro,
    Outro,
    Recap,
    Preview,
    Commercial,
    #[default]
    #[serde(other)]
    Unknown,
}

/// A span of an item worth skipping.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct MediaSegmentDto {
    pub id: String,
    pub item_id: String,
    #[serde(rename = "Type")]
    pub kind: SegmentKind,
    pub start_ticks: i64,
    pub end_ticks: i64,
}

impl MediaSegmentDto {
    pub fn start(&self) -> f64 {
        ticks_to_seconds(self.start_ticks)
    }

    pub fn end(&self) -> f64 {
        ticks_to_seconds(self.end_ticks)
    }
}

/// A track's lyrics, timed when each line has a start.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct LyricDto {
    pub metadata: LyricMetadata,
    pub lyrics: Vec<LyricLine>,
}

impl LyricDto {
    pub fn is_timed(&self) -> bool {
        self.metadata.is_synced.unwrap_or(false)
            || (!self.lyrics.is_empty() && self.lyrics.iter().all(|line| line.start.is_some()))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct LyricMetadata {
    pub artist: Option<String>,
    pub album: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    /// In ticks.
    pub length: Option<i64>,
    /// In ticks.
    pub offset: Option<i64>,
    pub is_synced: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct LyricLine {
    pub text: String,
    /// In ticks.
    pub start: Option<i64>,
    /// Word by word timing, from 10.11.
    pub cues: Vec<LyricLineCue>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct LyricLineCue {
    /// Characters into the line.
    pub position: i32,
    pub end_position: i32,
    /// In ticks.
    pub start: i64,
    pub end: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_are_ten_million_a_second() {
        assert_eq!(seconds_to_ticks(1.5), 15_000_000);
        assert_eq!(ticks_to_seconds(25_000_000), 2.5);
        // The server refuses a stop at a negative position.
        assert_eq!(seconds_to_ticks(-0.02), 0);
        assert_eq!(seconds_to_ticks(f64::NAN), 0);
    }

    #[test]
    fn public_info_reads_both_version_schemes() {
        let info: PublicSystemInfo = serde_json::from_str(
            r#"{"LocalAddress": "http://10.0.0.2:8096", "ServerName": "Den", "Version": "12.4.0",
                "ProductName": "Jellyfin Server", "OperatingSystem": "", "Id": "8d1ce7b7a96c4e2e9c4a3b0ff0e2d7a1",
                "StartupWizardCompleted": true}"#,
        )
        .unwrap();
        assert_eq!(info.version_number(), Some((12, 4, 0)));
        assert!(info.supported() && info.is_jellyfin());
        let old = PublicSystemInfo {
            version: "10.9.11".into(),
            ..info.clone()
        };
        assert!(!old.supported());
        let oldest = PublicSystemInfo {
            version: "10.10.7".into(),
            ..info.clone()
        };
        assert!(oldest.supported());
        let emby = PublicSystemInfo {
            product_name: "Emby Server".into(),
            ..info
        };
        assert!(!emby.is_jellyfin());
    }

    #[test]
    fn users_take_the_servers_defaults_for_what_is_missing() {
        let user: UserDto = serde_json::from_str(
            r#"{"Name": "viewer", "Id": "f6a1", "HasPassword": true,
                "Configuration": {"SubtitleMode": "Smart", "AudioLanguagePreference": "jpn",
                                  "PlayDefaultAudioTrack": false, "SomethingNew": [1, 2]},
                "Policy": {"EnableVideoPlaybackTranscoding": false, "EnableContentDownloading": false}}"#,
        )
        .unwrap();
        assert_eq!(user.configuration.subtitle_mode, SubtitleMode::Smart);
        assert_eq!(
            user.configuration.audio_language_preference.as_deref(),
            Some("jpn")
        );
        assert!(!user.configuration.play_default_audio_track);
        assert!(user.configuration.enable_next_episode_auto_play);
        assert!(user.configuration.remember_subtitle_selections);
        assert!(!user.policy.enable_video_playback_transcoding);
        assert!(user.policy.enable_media_playback);
        assert!(!user.policy.enable_content_downloading);
        let public: Vec<UserDto> = serde_json::from_str(
            r#"[{"Name": "a", "Id": "1", "Configuration": {"SubtitleMode": "Sometimes"}}]"#,
        )
        .unwrap();
        assert_eq!(public[0].configuration.subtitle_mode, SubtitleMode::Default);
    }

    const EPISODE: &str = r#"{
        "Name": "Pilot", "ServerId": "srv", "Id": "e1", "Type": "Episode", "MediaType": "Video",
        "IsFolder": false, "RunTimeTicks": 26000000000, "IndexNumber": 1, "ParentIndexNumber": 1,
        "SeriesId": "s1", "SeriesName": "Show", "SeasonId": "se1", "SeasonName": "Season 1",
        "UserData": {"PlaybackPositionTicks": 6000000000, "PlayCount": 0, "IsFavorite": true,
                     "Played": false, "Key": "k", "ItemId": "e1"},
        "ImageTags": {"Primary": "tagp", "Thumb": null},
        "BackdropImageTags": [],
        "ParentBackdropItemId": "s1", "ParentBackdropImageTags": ["bd0"],
        "ImageBlurHashes": {"Primary": {"tagp": "LEHV6nWB2yk8pyo0adR*.7kCMdnj", "gone": null},
                            "Backdrop": {"bd0": "L00000fQfQfQfQfQfQfQfQfQfQfQ"}},
        "Trickplay": {"ms1": {"320": {"Width": 320, "Height": 180, "TileWidth": 10, "TileHeight": 10,
                                      "ThumbnailCount": 260, "Interval": 10000, "Bandwidth": 1000},
                              "640": {"Width": 640, "Height": 360, "TileWidth": 10, "TileHeight": 10,
                                      "ThumbnailCount": 260, "Interval": 10000, "Bandwidth": 4000}},
                      "ms2": null},
        "Chapters": [{"StartPositionTicks": 0, "Name": "Opening", "ImageDateModified": "0001-01-01T00:00:00.0000000Z"}],
        "LocationType": "FileSystem",
        "CollectionType": "tvshows",
        "SomeFieldFromTheFuture": {"Nested": true}
    }"#;

    #[test]
    fn items_parse_and_ignore_what_they_dont_know() {
        let item: BaseItemDto = serde_json::from_str(EPISODE).unwrap();
        assert_eq!(item.kind, ItemKind::Episode);
        assert_eq!(item.media_type, MediaKind::Video);
        assert_eq!(item.collection_type, CollectionKind::TvShows);
        assert_eq!(item.duration(), Some(2600.0));
        assert_eq!(item.resume_position(), Some(600.0));
        assert!(item.is_favorite() && !item.played());
        assert_eq!(item.image_tag(ImageType::Primary), Some("tagp"));
        assert_eq!(item.image_tag(ImageType::Thumb), None);
        assert_eq!(
            item.blurhash(ImageType::Primary, "tagp"),
            Some("LEHV6nWB2yk8pyo0adR*.7kCMdnj")
        );
        assert_eq!(item.blurhash(ImageType::Primary, "gone"), None);
        assert_eq!(item.parent_backdrop_image_tags, ["bd0"]);
        assert_eq!(item.chapters[0].name.as_deref(), Some("Opening"));
        assert_eq!(item.location_type, LocationType::FileSystem);
        let widths = |source| -> Vec<u32> {
            item.trickplay_sizes(source)
                .iter()
                .map(|info| info.width)
                .collect()
        };
        assert_eq!(widths("ms1"), [320, 640]);
        assert!(widths("ms2").is_empty());

        let odd: BaseItemDto =
            serde_json::from_str(r#"{"Id": "x", "Type": "SomethingNew", "MediaType": "Hologram"}"#)
                .unwrap();
        assert_eq!(odd.kind, ItemKind::Unknown);
        assert_eq!(odd.media_type, MediaKind::Unknown);
        assert_eq!(odd.duration(), None);
        assert!(odd.user_data.is_none());
    }

    #[test]
    fn trickplay_finds_the_tile_and_its_corner() {
        let info = TrickplayInfo {
            width: 320,
            height: 180,
            tile_width: 10,
            tile_height: 10,
            thumbnail_count: 260,
            interval: 10_000,
            bandwidth: 0,
        };
        assert_eq!(info.locate(0.0), Some((0, 0, 0)));
        // The 13th thumbnail: second row, third column.
        assert_eq!(info.locate(125.0), Some((0, 2 * 320, 180)));
        // The 101st begins the second tile.
        assert_eq!(info.locate(1000.0), Some((1, 0, 0)));
        // Past the end, the last one.
        assert_eq!(info.locate(1e9), Some((2, 9 * 320, 5 * 180)));
        assert_eq!(TrickplayInfo::default().locate(5.0), None);

        // Sizes no tile has, from a server: no overflow, and no thumbnail.
        let huge = TrickplayInfo {
            tile_width: 70_000,
            tile_height: 70_000,
            ..info.clone()
        };
        assert_eq!(huge.locate(1.0), None);
        let wide = TrickplayInfo {
            width: 1 << 31,
            height: 1,
            tile_width: 4,
            tile_height: 1,
            thumbnail_count: 4,
            interval: 1000,
            bandwidth: 0,
        };
        assert_eq!(wide.locate(2.5), None);
        let many = TrickplayInfo {
            thumbnail_count: u32::MAX,
            interval: 1,
            ..info
        };
        assert_eq!(many.locate(1e12), Some((42_949_672, 4 * 320, 9 * 180)));
    }

    #[test]
    fn query_results_and_sources_parse() {
        let page: QueryResult<BaseItemDto> = serde_json::from_str(&format!(
            r#"{{"Items": [{EPISODE}], "TotalRecordCount": 120, "StartIndex": 100}}"#
        ))
        .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.total_record_count, 120);
        let info: PlaybackInfoResponse = serde_json::from_str(
            r#"{"MediaSources": [{"Protocol": "File", "Id": "ms1", "Container": "mkv", "ETag": "abc",
                "SupportsDirectPlay": true, "SupportsTranscoding": true, "DefaultAudioStreamIndex": 2,
                "DefaultSubtitleStreamIndex": 0, "RequiredHttpHeaders": {"X": null},
                "MediaStreams": [
                  {"Codec": "subrip", "Language": "eng", "Type": "Subtitle", "Index": 0, "IsExternal": true,
                   "DeliveryMethod": "External", "DeliveryUrl": "/Videos/e1/ms1/Subtitles/0/0/Stream.srt?ApiKey=t",
                   "IsExternalUrl": false, "IsTextSubtitleStream": true},
                  {"Codec": "h264", "Type": "Video", "Index": 1, "Width": 1920, "Height": 1080, "VideoRangeType": "SDR"},
                  {"Codec": "ac3", "Type": "Audio", "Index": 2, "Channels": 6, "IsDefault": true},
                  {"Codec": "dvdsub", "Type": "Subtitle", "Index": 3, "DeliveryMethod": "Teleport"}
                ]}],
                "PlaySessionId": "ps1"}"#,
        )
        .unwrap();
        let source = &info.media_sources[0];
        assert_eq!(source.etag.as_deref(), Some("abc"));
        assert!(source.required_http_headers.is_empty());
        assert_eq!(source.media_streams[0].kind, StreamKind::Subtitle);
        assert_eq!(
            source.media_streams[0].delivery_method,
            Some(DeliveryMethod::External)
        );
        assert_eq!(source.media_streams[2].channels, Some(6));
        assert_eq!(
            source.media_streams[3].delivery_method,
            Some(DeliveryMethod::Unknown)
        );
        assert_eq!(info.play_session_id.as_deref(), Some("ps1"));

        // The token the server writes into the URLs is not kept.
        assert_eq!(
            source.media_streams[0].delivery_url.as_deref(),
            Some("/Videos/e1/ms1/Subtitles/0/0/Stream.srt")
        );
        let transcoded: MediaSourceInfo = serde_json::from_str(
            r#"{"Id": "ms1", "TranscodingUrl": "/videos/e1/master.m3u8?DeviceId=d&ApiKey=secret&PlaySessionId=p",
                "MediaAttachments": [{"Index": 0, "DeliveryUrl": "/Videos/e1/ms1/Attachments/0?api_key=secret"}],
                "MediaStreams": [{"Type": "Subtitle", "DeliveryUrl": null}]}"#,
        )
        .unwrap();
        assert_eq!(
            transcoded.transcoding_url.as_deref(),
            Some("/videos/e1/master.m3u8?DeviceId=d&PlaySessionId=p")
        );
        assert_eq!(
            transcoded.media_attachments[0].delivery_url.as_deref(),
            Some("/Videos/e1/ms1/Attachments/0")
        );
        assert_eq!(transcoded.media_streams[0].delivery_url, None);
        assert!(!format!("{transcoded:?}").contains("secret"));
    }

    #[test]
    fn requests_leave_out_what_is_unset() {
        let request = PlaybackInfoRequest {
            media_source_id: Some("ms1".into()),
            audio_stream_index: Some(2),
            subtitle_stream_index: Some(-1),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({"MediaSourceId": "ms1", "AudioStreamIndex": 2, "SubtitleStreamIndex": -1})
        );
        let progress = PlaybackProgressInfo {
            item_id: "e1".into(),
            position_ticks: Some(seconds_to_ticks(12.0)),
            play_method: PlayMethod::Transcode,
            ..Default::default()
        };
        let value = serde_json::to_value(&progress).unwrap();
        assert_eq!(value["PositionTicks"], 120_000_000);
        assert_eq!(value["PlayMethod"], "Transcode");
        assert_eq!(value["IsPaused"], false);
        assert!(value.get("NowPlayingQueue").is_none());
        assert!(value.get("PlaySessionId").is_none());
    }

    #[test]
    fn the_profile_plays_anything_and_rules_out_what_is_asked() {
        let value =
            serde_json::to_value(DeviceProfile::patina(&TranscodeRules::default())).unwrap();
        assert_eq!(
            value["DirectPlayProfiles"],
            serde_json::json!([{"Type": "Video"}, {"Type": "Audio"}])
        );
        assert_eq!(value["TranscodingProfiles"][0]["Protocol"], "hls");
        assert_eq!(value["TranscodingProfiles"][0]["VideoCodec"], "h264,hevc");
        // Only what MPEG-TS segments carry, which the server keeps to.
        assert_eq!(
            value["TranscodingProfiles"][0]["AudioCodec"],
            "aac,ac3,eac3,mp3"
        );
        // No subtitles in the HLS playlist, whose URIs would carry the
        // token.
        assert!(
            value["SubtitleProfiles"]
                .as_array()
                .unwrap()
                .iter()
                .all(|profile| profile["Method"] != "Hls")
        );
        assert!(
            value["TranscodingProfiles"][0]
                .get("EnableSubtitlesInManifest")
                .is_none()
        );
        assert_eq!(value["CodecProfiles"], serde_json::json!([]));
        assert!(
            value["SubtitleProfiles"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!({"Format": "ass", "Method": "External"}))
        );
        let strict = DeviceProfile::patina(&TranscodeRules {
            max_bitrate: Some(4_000_000),
            hevc: true,
            av1: false,
            hdr: true,
            over_1080p: true,
        });
        assert_eq!(strict.max_streaming_bitrate, 4_000_000);
        assert_eq!(
            strict.transcoding_profiles[0].video_codec.as_deref(),
            Some("h264")
        );
        assert_eq!(strict.codec_profiles.len(), 2);
        assert_eq!(strict.codec_profiles[0].codec.as_deref(), Some("hevc"));
        assert!(strict.codec_profiles[0].conditions[0].is_required);
        let limits = &strict.codec_profiles[1];
        assert_eq!(limits.codec, None);
        assert_eq!(limits.conditions.len(), 3);
        let value = serde_json::to_value(&strict).unwrap();
        assert!(value["CodecProfiles"][1].get("Codec").is_none());
        assert_eq!(
            value["CodecProfiles"][1]["Conditions"][0]["Property"],
            "VideoRangeType"
        );
    }

    #[test]
    fn segments_and_lyrics_parse() {
        let segments: QueryResult<MediaSegmentDto> = serde_json::from_str(
            r#"{"Items": [{"Id": "1", "ItemId": "e1", "Type": "Intro", "StartTicks": 50000000, "EndTicks": 950000000},
                          {"Id": "2", "ItemId": "e1", "Type": "Interlude", "StartTicks": 0, "EndTicks": 1}],
                "TotalRecordCount": 2}"#,
        )
        .unwrap();
        assert_eq!(segments.items[0].kind, SegmentKind::Intro);
        assert_eq!(segments.items[0].start(), 5.0);
        assert_eq!(segments.items[0].end(), 95.0);
        assert_eq!(segments.items[1].kind, SegmentKind::Unknown);

        let lyrics: LyricDto = serde_json::from_str(
            r#"{"Metadata": {"Artist": "Band", "IsSynced": true},
                "Lyrics": [{"Text": "First", "Start": 10000000},
                           {"Text": "Second", "Start": 30000000,
                            "Cues": [{"Position": 0, "EndPosition": 6, "Start": 30000000}]}]}"#,
        )
        .unwrap();
        assert!(lyrics.is_timed());
        assert_eq!(lyrics.lyrics[1].cues[0].end_position, 6);
        let plain: LyricDto =
            serde_json::from_str(r#"{"Metadata": {}, "Lyrics": [{"Text": "Just words"}]}"#)
                .unwrap();
        assert!(!plain.is_timed());
    }

    #[test]
    fn quick_connect_and_sign_in_answers_parse() {
        let pending: QuickConnectResult = serde_json::from_str(
            r#"{"Authenticated": false, "Secret": "s3cr3t", "Code": "123456", "DeviceId": "d",
                "DeviceName": "Patina on Linux", "AppName": "Patina", "AppVersion": "0.1.0",
                "DateAdded": "2026-10-30T10:00:00.0000000Z"}"#,
        )
        .unwrap();
        assert_eq!(pending.code, "123456");
        assert!(!pending.authenticated);
        let signed_in: AuthenticationResult = serde_json::from_str(
            r#"{"User": {"Name": "viewer", "Id": "u1"}, "SessionInfo": {"Id": "x"},
                "AccessToken": "tok3n", "ServerId": "srv"}"#,
        )
        .unwrap();
        assert_eq!(signed_in.user.id, "u1");
        assert_eq!(signed_in.access_token, "tok3n");

        // Debug, as a log line or a failed test would print them, leaves
        // the secrets out.
        let shown = format!("{signed_in:?}");
        assert!(
            !shown.contains("tok3n") && shown.contains("(set)"),
            "{shown}"
        );
        let shown = format!("{pending:?}");
        assert!(
            !shown.contains("s3cr3t") && shown.contains("123456"),
            "{shown}"
        );
        assert!(format!("{:?}", AuthenticationResult::default()).contains("access_token: None"));
    }
}
