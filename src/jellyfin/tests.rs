//! Tests across the client's modules, against what a real Jellyfin says:
//! the answers tools/jellyfin-server.sh saved in fixtures/, and the server
//! itself when `PATINA_TEST_JELLYFIN_URL` names one, signed in as
//! `PATINA_TEST_JELLYFIN_USER` with `PATINA_TEST_JELLYFIN_PASSWORD`.
//! Without it the live tests are skipped, unless
//! `PATINA_TEST_REQUIRE_JELLYFIN` is 1, as CI sets it.
//!
//! The live tests run against any server, and look closely at only a few
//! of its films, tracks and seasons (`SAMPLE`), so that a real library of
//! thousands costs a few dozen requests. They need a movie library with a
//! film nobody has started. What only the library that script makes is
//! sure to have (a show and a music library, an HEVC film, trickplay
//! images, timed lyrics, names in the order Rust sorts them) they assert
//! when `PATINA_TEST_JELLYFIN_LIBRARY` is "generated", as the script sets
//! it, and elsewhere only note when it is missing. Whatever they change (a
//! played mark, a favourite) they put back; playing a film from its start
//! raises only its play count, and asking for a transcode starts one on
//! the server for a moment. They take one account's sessions on device ids
//! of their own, never on one a Patina installation would use, and print
//! no token when they fail.

use super::api::{self, Client, Clients, Error, ItemsQuery, SortOrder};
use super::auth::{self, Identity};
use super::models::*;
use super::{discovery, images, playback, session, store, trickplay};
use std::collections::HashMap;
use std::net::UdpSocket;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use url::Url;

/// A fixture, parsed as whatever type the binding asks for.
macro_rules! fixture {
    ($name:literal) => {
        serde_json::from_str(include_str!(concat!("fixtures/", $name, ".json")))
            .unwrap_or_else(|error| panic!("fixtures/{}.json: {error}", $name))
    };
}

/// Every fixture, each parsed by a test below.
const FIXTURES: &[&str] = &[
    "album_artists",
    "album_tracks",
    "albums",
    "authenticate_by_name",
    "collection_items",
    "episodes",
    "genres_music",
    "item_episode",
    "item_movie",
    "items_movies",
    "items_series",
    "latest_movies",
    "lyrics",
    "lyrics_unsynced",
    "media_segments",
    "next_up",
    "playback_info",
    "playback_info_transcode",
    "playlist_items",
    "quick_connect_enabled",
    "quick_connect_initiate",
    "resume",
    "search_artists",
    "search_items",
    "seasons",
    "system_info_public",
    "user_views",
    "users_me",
    "users_public",
];

fn fixtures_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/jellyfin/fixtures")
}

fn names(items: &[BaseItemDto]) -> Vec<&str> {
    items.iter().map(|item| item.name.as_str()).collect()
}

#[test]
fn every_fixture_is_parsed_by_a_test() {
    let mut found: Vec<String> = std::fs::read_dir(fixtures_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .map(|path| path.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    found.sort();
    assert_eq!(found, FIXTURES);
}

/// Every string in a JSON value, with the key it is under.
fn strings<'a>(value: &'a serde_json::Value, key: &'a str, found: &mut Vec<(&'a str, &'a str)>) {
    match value {
        serde_json::Value::String(text) => found.push((key, text)),
        serde_json::Value::Array(values) => {
            for value in values {
                strings(value, key, found);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                strings(value, key, found);
            }
        }
        _ => {}
    }
}

/// The fixtures go into the repository, so they must not carry the
/// test server's tokens, the folder it ran in (the script puts
/// /srv/jellyfin in its place), or any address but the loopback one it
/// listened on.
#[test]
fn fixtures_hold_no_secrets_paths_or_addresses() {
    for name in FIXTURES {
        let path = fixtures_dir().join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path).unwrap();
        for local in ["/home/", "/tmp/", ":\\\\"] {
            assert!(!text.contains(local), "{name} holds a local path ({local})");
        }
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let mut found = Vec::new();
        strings(&value, "", &mut found);
        for (key, text) in found {
            if key.ends_with("Path") && !text.is_empty() {
                assert!(text.starts_with("/srv/jellyfin/"), "{name}: {text}");
            }
            match key {
                "AccessToken" => assert_eq!(text, "TOKEN", "{name}"),
                "Secret" => assert_eq!(text, "SECRET", "{name}"),
                _ => {}
            }
            for (at, _) in text.match_indices("ApiKey=") {
                assert!(text[at..].starts_with("ApiKey=TOKEN"), "{name}: {text}");
            }
            if let Ok(url) = Url::parse(text)
                && matches!(url.scheme(), "http" | "https")
            {
                assert_eq!(url.host_str(), Some("127.0.0.1"), "{name}: {text}");
            }
        }
    }
}

#[test]
fn connecting_fixtures_parse() {
    let info: PublicSystemInfo = fixture!("system_info_public");
    assert!(info.is_jellyfin() && info.supported() && info.startup_wizard_completed);
    assert_eq!(info.version_number(), Some((12, 4, 0)));
    assert_eq!(info.id.len(), 32);

    // The administrator is hidden from the sign-in screen.
    let public: Vec<UserDto> = fixture!("users_public");
    assert_eq!(public.len(), 1);
    assert_eq!(public[0].name, "viewer");
    assert!(public[0].has_password && !public[0].id.is_empty());

    let enabled: bool = fixture!("quick_connect_enabled");
    assert!(enabled);
    let started: QuickConnectResult = fixture!("quick_connect_initiate");
    assert!(!started.authenticated);
    assert_eq!(started.secret, "SECRET");
    assert!(started.code.len() == 6 && started.code.bytes().all(|byte| byte.is_ascii_digit()));
    assert_eq!(started.app_name, "Patina");

    let signed_in: AuthenticationResult = fixture!("authenticate_by_name");
    assert_eq!(signed_in.access_token, "TOKEN");
    assert_eq!(signed_in.server_id, info.id);
    assert_eq!(signed_in.user.name, "patina");
    assert!(signed_in.user.policy.is_administrator);
    let me: UserDto = fixture!("users_me");
    assert_eq!(me.id, signed_in.user.id);
    assert_eq!(me.server_id, info.id);
    assert_eq!(me.configuration.subtitle_mode, SubtitleMode::Default);
    assert_eq!(
        me.configuration.subtitle_language_preference.as_deref(),
        Some("")
    );
    assert!(me.configuration.play_default_audio_track);
    assert!(me.policy.enable_media_playback && me.policy.enable_video_playback_transcoding);
    assert_eq!(me.policy.sync_play_access, "CreateAndJoinGroups");
}

#[test]
fn home_and_library_fixtures_parse() {
    let views: QueryResult<BaseItemDto> = fixture!("user_views");
    let kinds: Vec<CollectionKind> = views
        .items
        .iter()
        .map(|view| view.collection_type)
        .collect();
    for kind in [
        CollectionKind::Movies,
        CollectionKind::TvShows,
        CollectionKind::Music,
        CollectionKind::BoxSets,
        CollectionKind::Playlists,
    ] {
        assert!(kinds.contains(&kind), "{kind:?} in {kinds:?}");
    }
    let movies = views
        .items
        .iter()
        .find(|view| view.collection_type == CollectionKind::Movies)
        .unwrap();
    assert_eq!(movies.kind, ItemKind::CollectionFolder);
    assert!(movies.image_tag(ImageType::Primary).is_some());

    // Sorted by sort name, which drops the article.
    let films: QueryResult<BaseItemDto> = fixture!("items_movies");
    assert_eq!(
        names(&films.items),
        ["The Long Field", "Night Signal", "Paper Harbor"]
    );
    assert_eq!(films.total_record_count, 3);
    assert!(films.items.iter().all(|film| film.kind == ItemKind::Movie
        && film.media_type == MediaKind::Video
        && film.primary_image_aspect_ratio.is_some()));
    let latest: Vec<BaseItemDto> = fixture!("latest_movies");
    assert_eq!(latest.len(), 3);

    let resume: QueryResult<BaseItemDto> = fixture!("resume");
    assert_eq!(names(&resume.items), ["Paper Harbor"]);
    assert_eq!(resume.items[0].resume_position(), Some(12.0));
    assert!(!resume.items[0].played());

    let next_up: QueryResult<BaseItemDto> = fixture!("next_up");
    let next = &next_up.items[0];
    assert_eq!(next.name, "Night Market");
    assert_eq!(next.kind, ItemKind::Episode);
    assert_eq!(
        (next.parent_index_number, next.index_number),
        (Some(1), Some(2))
    );
    assert_eq!(next.series_name.as_deref(), Some("Lantern Street"));

    let shows: QueryResult<BaseItemDto> = fixture!("items_series");
    let show = &shows.items[0];
    assert_eq!(show.kind, ItemKind::Series);
    assert_eq!(
        show.user_data.as_ref().unwrap().unplayed_item_count,
        Some(4)
    );
    let seasons: QueryResult<BaseItemDto> = fixture!("seasons");
    assert_eq!(names(&seasons.items), ["Season 1", "Season 2"]);
    assert!(
        seasons
            .items
            .iter()
            .all(|season| season.kind == ItemKind::Season
                && season.series_id.as_deref() == Some(show.id.as_str()))
    );
    let episodes: QueryResult<BaseItemDto> = fixture!("episodes");
    assert_eq!(
        names(&episodes.items),
        ["Opening Night", "Night Market", "Open House"]
    );
    assert!(episodes.items[0].played());
    assert!(episodes.items.iter().all(|episode| {
        episode.season_id.as_deref() == Some(seasons.items[0].id.as_str())
            && episode.parent_backdrop_item_id.is_some()
    }));

    let collection: QueryResult<BaseItemDto> = fixture!("collection_items");
    assert_eq!(names(&collection.items), ["Paper Harbor", "Night Signal"]);

    let found: QueryResult<BaseItemDto> = fixture!("search_items");
    let kinds: Vec<ItemKind> = found.items.iter().map(|item| item.kind).collect();
    assert_eq!(
        kinds,
        [
            ItemKind::Audio,
            ItemKind::Episode,
            ItemKind::Movie,
            ItemKind::Episode
        ]
    );
}

#[test]
fn item_fixtures_parse_with_their_versions_and_streams() {
    let film: BaseItemDto = fixture!("item_movie");
    assert_eq!(film.name, "Night Signal");
    assert_eq!(film.production_year, Some(2021));
    assert_eq!(film.official_rating.as_deref(), Some("PG-13"));
    assert_eq!(film.duration(), Some(30.021));
    assert_eq!(film.genres.len(), 2);
    assert_eq!(film.people.len(), 3);
    assert_eq!(film.people[2].kind, "Director");
    assert_eq!(film.studios.len(), 1);
    assert_eq!(film.taglines.len(), 1);
    assert_eq!(film.location_type, LocationType::FileSystem);
    let tag = film.image_tag(ImageType::Primary).unwrap();
    assert!(film.blurhash(ImageType::Primary, tag).is_some());
    assert_eq!(film.backdrop_image_tags.len(), 1);
    assert!(
        film.blurhash(ImageType::Backdrop, &film.backdrop_image_tags[0])
            .is_some()
    );
    assert_eq!(film.chapters.len(), 3);
    assert_eq!(film.chapters[1].start_position_ticks, 100_000_000);
    assert!(
        film.chapters
            .iter()
            .all(|chapter| chapter.image_tag.is_some())
    );

    // External subtitles come first, then the file's own streams.
    let source = &film.media_sources[0];
    assert_eq!(source.container.as_deref(), Some("mkv"));
    assert!(source.etag.is_some() && source.supports_direct_play);
    let streams: Vec<(i32, StreamKind, bool)> = source
        .media_streams
        .iter()
        .map(|stream| (stream.index, stream.kind, stream.is_external))
        .collect();
    assert_eq!(
        streams,
        [
            (0, StreamKind::Subtitle, true),
            (1, StreamKind::Subtitle, true),
            (2, StreamKind::Video, false),
            (3, StreamKind::Audio, false),
            (4, StreamKind::Audio, false),
            (5, StreamKind::Subtitle, false),
        ]
    );
    assert!(source.media_streams[1].is_forced);
    assert_eq!(source.default_audio_stream_index, Some(3));
    assert_eq!(source.default_subtitle_stream_index, Some(0));
    assert_eq!(film.media_streams.len(), 6);

    let sizes = film.trickplay_sizes(&source.id);
    let tiles = trickplay::pick(&sizes, 320).unwrap();
    assert_eq!((tiles.width, tiles.height), (320, 180));
    assert_eq!((tiles.tile_width, tiles.tile_height), (10, 10));
    assert_eq!(tiles.interval, 2000);
    assert_eq!(tiles.locate(29.0), Some((0, 4 * 320, 180)));

    let episode: BaseItemDto = fixture!("item_episode");
    assert_eq!(episode.kind, ItemKind::Episode);
    assert_eq!(episode.series_name.as_deref(), Some("Lantern Street"));
    assert_eq!(
        (episode.parent_index_number, episode.index_number),
        (Some(1), Some(1))
    );
    let chapters: Vec<&str> = episode
        .chapters
        .iter()
        .filter_map(|chapter| chapter.name.as_deref())
        .collect();
    assert_eq!(chapters, ["Intro", "Part One", "Credits"]);
    assert!(!episode.media_sources.is_empty());
}

#[test]
fn music_fixtures_parse() {
    let albums: QueryResult<BaseItemDto> = fixture!("albums");
    let album = &albums.items[0];
    assert_eq!(album.kind, ItemKind::MusicAlbum);
    assert_eq!(album.name, "Low Tide");
    assert_eq!(album.album_artist.as_deref(), Some("Mira Vale"));

    let tracks: QueryResult<BaseItemDto> = fixture!("album_tracks");
    assert_eq!(
        names(&tracks.items),
        ["Low Tide", "Glass Water", "Night Ferry", "Harbor Lights"]
    );
    assert!(
        tracks
            .items
            .iter()
            .all(|track| track.kind == ItemKind::Audio
                && track.media_type == MediaKind::Audio
                && track.album_id.as_deref() == Some(album.id.as_str()))
    );
    let lyrics: Vec<bool> = tracks.items.iter().map(|track| track.has_lyrics).collect();
    assert_eq!(lyrics, [true, true, false, false]);

    let artists: QueryResult<BaseItemDto> = fixture!("album_artists");
    assert_eq!(artists.items[0].kind, ItemKind::MusicArtist);
    assert_eq!(album.album_artists[0].id, artists.items[0].id);
    let found: QueryResult<BaseItemDto> = fixture!("search_artists");
    assert_eq!(found.items[0].id, artists.items[0].id);
    let genres: QueryResult<BaseItemDto> = fixture!("genres_music");
    assert_eq!(genres.items[0].kind, ItemKind::MusicGenre);
    assert_eq!(genres.items[0].name, "Ambient");

    // A playlist keeps its own order, and each entry its entry id.
    let entries: QueryResult<BaseItemDto> = fixture!("playlist_items");
    assert_eq!(
        names(&entries.items),
        ["Night Ferry", "Low Tide", "Harbor Lights"]
    );
    assert!(
        entries
            .items
            .iter()
            .all(|entry| entry.playlist_item_id.is_some())
    );

    // Start in ticks when timed; text only, and the file's last empty
    // line, when not.
    let timed: LyricDto = fixture!("lyrics");
    assert!(timed.is_timed());
    assert_eq!(timed.lyrics.len(), 4);
    assert_eq!(timed.lyrics[1].start, Some(30_000_000));
    assert_eq!(timed.metadata, LyricMetadata::default());
    let plain: LyricDto = fixture!("lyrics_unsynced");
    assert!(!plain.is_timed());
    assert_eq!(plain.lyrics.len(), 5);
    assert_eq!(plain.lyrics[4].text, "");
}

#[test]
fn playback_fixtures_parse_and_give_urls_without_the_token() {
    let client = Client::new(
        Url::parse("http://127.0.0.1:18096").unwrap(),
        Identity::new(store::DEVICE_NAME, "fixture-device"),
        None,
    )
    .unwrap()
    .with_session("TOKEN", "u1");

    let direct: PlaybackInfoResponse = fixture!("playback_info");
    assert_eq!(direct.error_code, None);
    assert!(direct.play_session_id.is_some());
    let source = &direct.media_sources[0];
    assert!(source.supports_direct_play && source.transcoding_url.is_none());
    let film: BaseItemDto = fixture!("item_movie");
    let stream = client.static_stream_url(&film.id, false, source);
    assert_eq!(
        stream.as_str(),
        format!(
            "http://127.0.0.1:18096/Videos/{}/stream.mkv?Static=true&mediaSourceId={}\
             &deviceId=fixture-device&Tag={}",
            film.id,
            source.id,
            source.etag.as_deref().unwrap()
        )
    );
    let external: Vec<&MediaStream> = source
        .media_streams
        .iter()
        .filter(|stream| stream.is_external)
        .collect();
    assert_eq!(external.len(), 2);
    for subtitle in external {
        assert_eq!(subtitle.delivery_method, Some(DeliveryMethod::External));
        let url = client.delivery_url(subtitle).unwrap();
        assert!(!url.as_str().contains("TOKEN"), "{url}");
        assert!(url.path().ends_with("/Stream.subrip"), "{url}");
    }
    assert_eq!(
        source.media_streams[5].delivery_method,
        Some(DeliveryMethod::Embed)
    );

    // The server writes the token into the middle of the transcoding URL's
    // query; it is gone once read, and the rest is left as it was, with
    // no trickplay rendition asked for.
    let text = include_str!("fixtures/playback_info_transcode.json");
    assert!(text.contains("ApiKey=TOKEN&"));
    let transcode: PlaybackInfoResponse = fixture!("playback_info_transcode");
    let source = &transcode.media_sources[0];
    assert!(!source.supports_direct_play);
    assert_eq!(source.transcoding_sub_protocol.as_deref(), Some("hls"));
    assert_eq!(source.transcoding_container.as_deref(), Some("ts"));
    let raw = source.transcoding_url.as_deref().unwrap();
    assert!(!raw.contains("TOKEN"), "{raw}");
    let url = client.transcode_url(source).unwrap();
    assert!(!url.as_str().contains("ApiKey"), "{url}");
    assert_eq!(url.path(), raw.split('?').next().unwrap());
    assert_eq!(
        url.query().unwrap(),
        format!("{}&EnableTrickplay=false", raw.split_once('?').unwrap().1)
    );
    assert_eq!(source.media_streams[0].codec.as_deref(), Some("hevc"));
    assert!(!format!("{transcode:?}").contains("TOKEN"));
    assert!(!format!("{direct:?}").contains("TOKEN"));

    let segments: QueryResult<MediaSegmentDto> = fixture!("media_segments");
    assert!(segments.items.is_empty());
}

// The server itself.

/// The live tests take turns, since some change what the account has
/// watched and favoured and others read it.
static TURN: Mutex<()> = Mutex::new(());

/// How many films, tracks, seasons, playlists and so on the live tests
/// look at one by one.
const SAMPLE: u32 = 6;

fn skip(what: &str) {
    crate::mpv::testing::skip("PATINA_TEST_REQUIRE_JELLYFIN", what);
}

/// Whether the server's library is the one tools/jellyfin-server.sh makes.
fn generated() -> bool {
    std::env::var("PATINA_TEST_JELLYFIN_LIBRARY").is_ok_and(|library| library == "generated")
}

/// Asserts what the generated library is sure to have; of another server
/// only notes that it doesn't.
fn expect(holds: bool, what: &str) {
    if generated() {
        assert!(holds, "{what}");
    } else if !holds {
        eprintln!("not on this server: {what}");
    }
}

/// `text` with the token hidden, for a failed test's message: a run
/// against a real server must not print its account's token.
fn hide(text: &str, token: &str) -> String {
    if token.is_empty() {
        text.to_owned()
    } else {
        text.replace(token, "<token>")
    }
}

/// Fails the test with the error's own text.
fn ok<T>(result: Result<T, Error>) -> T {
    result.unwrap_or_else(|error| panic!("{error:?}: {error}"))
}

/// The server `PATINA_TEST_JELLYFIN_URL` names, probed as Patina adds
/// one, for a client calling itself `device_id`, with the test's turn;
/// None skips the test. Each test has its own device id, since signing in
/// again on one revokes the account's other tokens for it.
fn server(device_id: &str) -> Option<(Client, MutexGuard<'static, ()>)> {
    let Ok(address) = std::env::var("PATINA_TEST_JELLYFIN_URL") else {
        skip("needs a Jellyfin server in PATINA_TEST_JELLYFIN_URL");
        return None;
    };
    let turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let identity = Identity::new(store::DEVICE_NAME, device_id);
    let probed = ok(api::probe(&address, &identity, None));
    Some((ok(Client::new(probed.address, identity, None)), turn))
}

/// Signs out when dropped, so that a failed test leaves no session behind.
struct SignOut(Client);

impl Drop for SignOut {
    fn drop(&mut self) {
        let _ = self.0.logout();
    }
}

/// A signed-in account, for the length of a test.
struct Account {
    client: Client,
    user: UserDto,
    _sign_out: SignOut,
    _turn: MutexGuard<'static, ()>,
}

fn account(device_id: &str) -> Option<Account> {
    let (client, turn) = server(device_id)?;
    let (Ok(name), Ok(password)) = (
        std::env::var("PATINA_TEST_JELLYFIN_USER"),
        std::env::var("PATINA_TEST_JELLYFIN_PASSWORD"),
    ) else {
        skip("needs PATINA_TEST_JELLYFIN_USER and PATINA_TEST_JELLYFIN_PASSWORD");
        return None;
    };
    let signed_in = ok(client.authenticate_by_name(&name, &password));
    let client = client.with_session(&signed_in.access_token, &signed_in.user.id);
    Some(Account {
        _sign_out: SignOut(client.clone()),
        client,
        user: signed_in.user,
        _turn: turn,
    })
}

/// Runs when dropped: puts back what a test changed, passed or failed.
struct Restore<F: FnMut()>(F);

impl<F: FnMut()> Drop for Restore<F> {
    fn drop(&mut self) {
        (self.0)()
    }
}

/// The account's library of a kind, if it has one.
fn library(client: &Client, kind: CollectionKind) -> Option<BaseItemDto> {
    ok(client.views())
        .items
        .into_iter()
        .find(|view| view.collection_type == kind)
}

/// The movie library, which the live tests can't do without.
fn movie_library(client: &Client) -> String {
    library(client, CollectionKind::Movies)
        .expect("a movie library")
        .id
}

/// The first few films by sort name, with their user data.
fn films(client: &Client) -> Vec<BaseItemDto> {
    let movies = movie_library(client);
    ok(client.items(
        &ItemsQuery::new()
            .parent(&movies)
            .types(&[ItemKind::Movie])
            .recursive()
            .sort(&["SortName"], SortOrder::Ascending)
            .limit(SAMPLE),
    ))
    .items
}

/// A GET with no `Authorization` header, as mpv makes for a stream: the
/// status and the body.
fn anonymous_get(url: &Url, range: Option<&str>) -> (u16, Vec<u8>) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let mut request = agent.get(url.as_str());
    if let Some(range) = range {
        request = request.header("Range", range);
    }
    let mut response = request.call().unwrap();
    let status = response.status().as_u16();
    let body = response.body_mut().read_to_vec().unwrap();
    (status, body)
}

fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xD8, 0xFF])
}

#[test]
fn live_the_server_answers_before_signing_in() {
    let Some((client, _turn)) = server("patina-tests-anonymous") else {
        return;
    };
    let info = ok(client.public_info());
    assert!(info.is_jellyfin() && info.supported(), "{info:?}");
    assert!(info.startup_wizard_completed);

    // The web app's address finds the server above it, and an address
    // typed without http:// (https assumed) says to add it.
    let address = auth::display_address(client.base());
    let web = ok(api::probe(
        &format!("{address}/web/"),
        client.identity(),
        None,
    ));
    assert_eq!(web.address, *client.base());
    assert_eq!(web.info.id, info.id);
    let pasted = ok(api::probe(
        &format!("{address}/web/index.html#/home"),
        client.identity(),
        None,
    ));
    assert_eq!(pasted.address, *client.base());
    // Without a scheme, https is tried first; a server that answers in
    // plain http is then asked over http at the same port.
    if let Some(bare) = address.strip_prefix("http://") {
        let found = ok(api::probe(bare, client.identity(), None));
        assert_eq!(found.address, *client.base());
    }

    let users = ok(client.public_users());
    assert!(
        users
            .iter()
            .all(|user| !user.id.is_empty() && !user.name.is_empty()),
        "{users:?}"
    );

    // Nothing more without signing in.
    assert_eq!(client.me(), Err(Error::Unauthorized));
    assert_eq!(client.views().unwrap_err(), Error::Unauthorized);
    let refused = client.authenticate_by_name("nobody-by-this-name", "wrong");
    assert!(
        matches!(&refused, Err(Error::SignIn(reason)) if reason.contains("wrong")),
        "{refused:?}"
    );
    let unknown = client.quick_connect_connect("no-such-secret");
    assert!(
        matches!(&unknown, Err(Error::SignIn(reason)) if reason.contains("ran out")),
        "{unknown:?}"
    );
}

#[test]
fn live_a_password_sign_in_lasts_until_signing_out() {
    let Some(account) = account("patina-tests-password") else {
        return;
    };
    let client = &account.client;
    let info = ok(client.public_info());
    let me = ok(client.me());
    assert_eq!(me.id, account.user.id);
    assert_eq!(me.name, std::env::var("PATINA_TEST_JELLYFIN_USER").unwrap());
    assert_eq!(me.server_id, info.id);
    assert!(me.policy.enable_media_playback);

    // Signing in again on the same device id revokes the first token,
    // which is why each installation makes its own.
    let password = std::env::var("PATINA_TEST_JELLYFIN_PASSWORD").unwrap();
    let again = ok(client.authenticate_by_name(&me.name, &password));
    assert!(
        again.access_token != client.token().unwrap(),
        "the same token"
    );
    assert_eq!(client.me(), Err(Error::Unauthorized));
    let client = client
        .clone()
        .with_session(&again.access_token, &again.user.id);
    let _sign_out = SignOut(client.clone());
    assert_eq!(ok(client.me()).id, me.id);
    let film = films(&client).into_iter().next().expect("a film");
    let film = ok(client.item(&film.id));
    let poster = client.image_url(
        &film.id,
        ImageType::Primary,
        None,
        film.image_tag(ImageType::Primary),
        Some(160),
    );
    let stream = client.static_stream_url(&film.id, false, &film.media_sources[0]);

    ok(client.logout());
    assert_eq!(client.me(), Err(Error::Unauthorized));
    // What anyone may ask is still answered with the dead token in the
    // header, which the image cache and mpv go on sending until Patina
    // notices the sign-in is gone.
    ok(client.public_info());
    ok(client.public_users());
    if film.image_tag(ImageType::Primary).is_some() {
        assert!(is_jpeg(&ok(client.get_bytes(&poster))));
    }
    let mut response = ureq::get(stream.as_str())
        .header("Authorization", client.authorization())
        .header("Range", "bytes=0-15")
        .call()
        .unwrap();
    assert_eq!(response.status(), 206);
    assert_eq!(response.body_mut().read_to_vec().unwrap().len(), 16);
}

/// Approves a Quick Connect code as another Jellyfin app would, which
/// Patina itself doesn't.
fn approve(approver: &Client, code: &str) {
    let url = auth::api_url(
        approver.base(),
        &format!("/QuickConnect/Authorize?code={code}"),
    );
    let mut response = ureq::post(url.as_str())
        .header("Authorization", approver.authorization())
        .send_empty()
        .unwrap();
    assert_eq!(response.body_mut().read_to_string().unwrap(), "true");
}

#[test]
fn live_quick_connect_signs_in_once_approved() {
    let Some(approver) = account("patina-tests-quick-connect-approver") else {
        return;
    };
    let identity = Identity::new(store::DEVICE_NAME, "patina-tests-quick-connect");
    let client = ok(Client::new(approver.client.base().clone(), identity, None));
    if !ok(client.quick_connect_enabled()) {
        skip("Quick Connect is turned off on the server");
        return;
    }
    let started = ok(client.quick_connect_initiate());
    assert!(
        started.code.len() == 6 && started.code.bytes().all(|byte| byte.is_ascii_digit()),
        "the code {:?}",
        started.code
    );
    assert_eq!(started.device_id, "patina-tests-quick-connect");
    let waiting = ok(client.quick_connect_connect(&started.secret));
    assert!(!waiting.authenticated);
    assert_eq!(waiting.code, started.code);

    approve(&approver.client, &started.code);
    assert!(ok(client.quick_connect_connect(&started.secret)).authenticated);
    let signed_in = ok(client.quick_connect_authenticate(&started.secret));
    assert_eq!(signed_in.user.id, approver.user.id);
    assert!(
        signed_in.access_token != approver.client.token().unwrap_or_default(),
        "the approver's token"
    );
    let client = client.with_session(&signed_in.access_token, &signed_in.user.id);
    let _sign_out = SignOut(client.clone());
    assert_eq!(ok(client.me()).id, approver.user.id);
    // On a device of its own, which leaves the approver signed in.
    ok(approver.client.me());

    ok(client.logout());
    assert_eq!(client.me(), Err(Error::Unauthorized));
    ok(approver.client.me());
}

#[test]
fn live_the_library_can_be_browsed() {
    let Some(account) = account("patina-tests-browsing") else {
        return;
    };
    let client = &account.client;
    let movies = movie_library(client);

    // Home: Continue Watching, Next Up, and Latest in each library.
    let resume = ok(client.resume(
        &ItemsQuery::new()
            .media_types(&["Video"])
            .fields(&["PrimaryImageAspectRatio"])
            .limit(12),
    ));
    assert!(
        resume
            .items
            .iter()
            .all(|item| item.media_type == MediaKind::Video && item.resume_position().is_some()),
        "{:?}",
        names(&resume.items)
    );
    let next_up = ok(client.next_up(&ItemsQuery::new().limit(12)));
    assert!(
        next_up
            .items
            .iter()
            .all(|item| item.kind == ItemKind::Episode
                && item.series_id.is_some()
                && !item.played()),
        "{:?}",
        names(&next_up.items)
    );
    for view in ok(client.views()).items {
        let latest = ok(client.latest(&ItemsQuery::new().parent(&view.id).limit(16)));
        assert!(latest.len() <= 16);
        if view.id == movies {
            assert!(!latest.is_empty());
            assert!(latest.iter().all(|item| item.kind == ItemKind::Movie));
        }
    }

    // A library in pages, sorted either way.
    let query = |order: SortOrder| {
        ItemsQuery::new()
            .parent(&movies)
            .types(&[ItemKind::Movie])
            .recursive()
            .sort(&["SortName"], order)
            .fields(&["SortName", "PrimaryImageAspectRatio"])
            .images(&[ImageType::Primary, ImageType::Backdrop], 1)
    };
    const PAGE: u32 = 50;
    let first = ok(client.items(&query(SortOrder::Ascending).page(0, PAGE)));
    let total = first.total_record_count;
    assert!(!first.items.is_empty(), "no films");
    assert_eq!(first.items.len() as i64, total.min(i64::from(PAGE)));
    let sort_names = |items: &[BaseItemDto]| -> Vec<String> {
        items
            .iter()
            .map(|item| item.sort_name.clone().expect("a sort name"))
            .collect()
    };
    // The server's collation, which Rust's order matches for plain names
    // only.
    expect(
        sort_names(&first.items).is_sorted(),
        "film names in the order Rust sorts them",
    );
    // The first page sorted the other way round is the last page's names,
    // reversed: names, since films of the same name may come either way.
    let mut reversed =
        sort_names(&ok(client.items(&query(SortOrder::Descending).page(0, PAGE))).items);
    reversed.reverse();
    let last_start = u32::try_from(total - reversed.len() as i64).unwrap();
    let last = ok(client.items(&query(SortOrder::Ascending).page(last_start, PAGE)));
    assert_eq!(reversed, sort_names(&last.items));
    for start in 0..first.items.len().min(3) {
        let page = ok(client.items(&query(SortOrder::Ascending).page(start as u32, 1)));
        assert_eq!(page.start_index, start as i64);
        assert_eq!(page.total_record_count, total);
        assert_eq!(
            sort_names(&page.items),
            sort_names(&first.items[start..=start])
        );
    }
    let past = ok(client.items(&query(SortOrder::Ascending).page(total as u32, 10)));
    assert!(past.items.is_empty());
    assert_eq!(past.total_record_count, total);

    // A few films' pages, with their versions; Jellyfin numbers every
    // stream of a version, its external subtitles first.
    let mut details = Vec::new();
    for film in first.items.iter().take(SAMPLE as usize) {
        let detail = ok(client.item(&film.id));
        assert_eq!(detail.id, film.id);
        assert!(detail.duration().is_some(), "{}", detail.name);
        assert!(!detail.media_sources.is_empty(), "{}", detail.name);
        for source in &detail.media_sources {
            let indexes: Vec<i32> = source.media_streams.iter().map(|s| s.index).collect();
            assert_eq!(indexes, (0..indexes.len() as i32).collect::<Vec<_>>());
            let external = source
                .media_streams
                .iter()
                .take_while(|stream| stream.is_external)
                .count();
            assert!(
                source.media_streams[external..]
                    .iter()
                    .all(|s| !s.is_external)
            );
            if let Some(index) = source.default_audio_stream_index {
                assert_eq!(source.media_streams[index as usize].kind, StreamKind::Audio);
            }
        }
        details.push(detail);
    }
    expect(
        details.iter().any(|film| !film.chapters.is_empty()),
        "a film with chapters",
    );
    expect(
        details.iter().any(|film| !film.people.is_empty()),
        "a film with people",
    );

    // Posters: anyone may fetch them, the image cache's fetch gets them
    // through the client, and its decoder makes them the width asked.
    let film = details
        .iter()
        .find(|film| film.image_tag(ImageType::Primary).is_some())
        .expect("a film with a poster");
    let tag = film.image_tag(ImageType::Primary).unwrap();
    let url = client.image_url(&film.id, ImageType::Primary, None, Some(tag), Some(150));
    assert_eq!(
        url.query_pairs()
            .find(|(name, _)| name == "maxWidth")
            .unwrap()
            .1,
        "160"
    );
    let poster = ok(client.get_bytes(&url));
    assert!(is_jpeg(&poster));
    assert_eq!(anonymous_get(&url, None), (200, poster.clone()));
    let clients = Clients::new();
    clients.insert(client.clone());
    let fetch = clients.fetch();
    assert_eq!(fetch(url.as_str()).unwrap(), poster);
    let pixels = images::decode(&poster, Some(150)).unwrap();
    assert!(pixels.width() <= 150);
    expect(
        pixels.height() > pixels.width(),
        "a poster taller than it is wide",
    );
    let blurhash = film.blurhash(ImageType::Primary, tag).expect("a blurhash");
    assert!(images::placeholder(blurhash, 2, 3).is_some());
    let missing = client.image_url(
        "00000000000000000000000000000000",
        ImageType::Primary,
        None,
        None,
        None,
    );
    assert!(fetch(missing.as_str()).is_err());
    if let Some(backdrop) = film.backdrop_image_tags.first() {
        let url = client.image_url(
            &film.id,
            ImageType::Backdrop,
            Some(0),
            Some(backdrop),
            Some(960),
        );
        let pixels = images::decode(&ok(client.get_bytes(&url)), Some(400)).unwrap();
        assert!(pixels.width() <= 400);
        expect(
            pixels.width() > pixels.height(),
            "a backdrop wider than it is tall",
        );
    }

    // Trickplay tiles, which only a signed-in request gets: each a grid
    // of thumbnails, the last tile holding the rest. Two films' are
    // enough.
    let mut tiled = 0;
    for film in &details {
        let source = &film.media_sources[0];
        let sizes = film.trickplay_sizes(&source.id);
        let Some(tiles) = trickplay::pick(&sizes, 320) else {
            continue;
        };
        if tiled == 2 {
            break;
        }
        tiled += 1;
        let url = client.trickplay_tile_url(&film.id, tiles.width, 0, Some(&source.id));
        assert_eq!(anonymous_get(&url, None).0, 401);
        let tile = ok(client.get_bytes(&url));
        assert!(is_jpeg(&tile));
        let pixels = images::decode(&tile, None).unwrap();
        assert!(pixels.width() <= tiles.tile_width * tiles.width);
        assert!(pixels.height() <= tiles.tile_height * tiles.height);
        assert!(pixels.width() >= tiles.width && pixels.height() >= tiles.height);
        let (last, _, _) = tiles.locate(film.duration().unwrap()).unwrap();
        let url = client.trickplay_tile_url(&film.id, tiles.width, last, Some(&source.id));
        assert!(is_jpeg(&ok(client.get_bytes(&url))));
        let url = client.trickplay_tile_url(&film.id, tiles.width, last + 1, Some(&source.id));
        assert_eq!(client.get_bytes(&url), Err(Error::NotFound));
    }
    expect(tiled > 0, "a film with trickplay images");

    // Segments: whatever the server has, in order; none without a plugin.
    for film in details.iter().take(3) {
        for segment in ok(client.media_segments(&film.id)) {
            assert!(segment.start() <= segment.end(), "{segment:?}");
            assert_eq!(segment.item_id, film.id);
        }
    }

    match library(client, CollectionKind::TvShows) {
        Some(shows) => browse_a_show(client, &shows.id),
        None => expect(false, "a TV library"),
    }
    match library(client, CollectionKind::Music) {
        Some(music) => browse_music(client, &music.id),
        None => expect(false, "a music library"),
    }

    // Playlists in their own order, and collections.
    let playlists = ok(client.items(
        &ItemsQuery::new()
            .types(&[ItemKind::Playlist])
            .recursive()
            .limit(3),
    ));
    for playlist in &playlists.items {
        let entries =
            ok(client.playlist_items(&playlist.id, &ItemsQuery::new().limit(SAMPLE))).items;
        assert!(
            entries.iter().all(|entry| entry.playlist_item_id.is_some()),
            "{}",
            playlist.name
        );
    }
    let collections = ok(client.items(
        &ItemsQuery::new()
            .types(&[ItemKind::BoxSet])
            .recursive()
            .limit(3),
    ));
    for collection in &collections.items {
        let members =
            ok(client.items(&ItemsQuery::new().parent(&collection.id).limit(SAMPLE))).items;
        expect(
            !members.is_empty(),
            &format!("films in the collection {}", collection.name),
        );
    }

    // Search: a film by its name, and nothing for nonsense.
    let film = &first.items[0];
    let found = ok(client.search(
        &film.name,
        &[
            ItemKind::Movie,
            ItemKind::Series,
            ItemKind::Episode,
            ItemKind::MusicAlbum,
            ItemKind::Audio,
        ],
        24,
    ));
    assert!(
        found.items.iter().any(|item| item.id == film.id),
        "{}: {:?}",
        film.name,
        names(&found.items)
    );
    assert!(
        ok(client.search("qqxxzz", &[ItemKind::Movie], 24))
            .items
            .is_empty()
    );

    // An item the server doesn't have, as a stale entry in History
    // would name.
    let gone = "0123456789abcdef0123456789abcdef";
    assert_eq!(client.item(gone), Err(Error::NotFound));
    assert_eq!(client.lyrics(gone), Ok(None));
}

/// A show, its first few seasons and their episodes.
fn browse_a_show(client: &Client, shows: &str) {
    let series = ok(client.items(
        &ItemsQuery::new()
            .parent(shows)
            .types(&[ItemKind::Series])
            .recursive()
            .sort(&["SortName"], SortOrder::Ascending)
            .limit(1),
    ));
    let Some(show) = series.items.first() else {
        expect(false, "a show");
        return;
    };
    let seasons = ok(client.seasons(&show.id, &ItemsQuery::new())).items;
    assert!(!seasons.is_empty());
    let numbers: Vec<Option<i32>> = seasons.iter().map(|season| season.index_number).collect();
    assert!(numbers.is_sorted(), "{numbers:?}");
    let mut counted = 0;
    for season in seasons.iter().take(3) {
        assert_eq!(season.kind, ItemKind::Season);
        assert_eq!(season.series_id.as_deref(), Some(show.id.as_str()));
        let episodes = ok(client.episodes(&show.id, &ItemsQuery::new().season(&season.id))).items;
        assert!(!episodes.is_empty(), "{}", season.name);
        assert!(episodes.iter().all(|episode| {
            episode.kind == ItemKind::Episode
                && episode.season_id.as_deref() == Some(season.id.as_str())
                && episode.series_id.as_deref() == Some(show.id.as_str())
        }));
        expect(
            episodes
                .iter()
                .all(|episode| episode.parent_index_number == season.index_number),
            "episodes numbered in their season",
        );
        let numbers: Vec<Option<i32>> = episodes.iter().map(|e| e.index_number).collect();
        assert!(numbers.is_sorted(), "{numbers:?}");
        counted += episodes.len();
    }
    // The show's episodes in one page, and how many it has in all.
    let every = ok(client.episodes(&show.id, &ItemsQuery::new().limit(SAMPLE)));
    if seasons.len() <= 3 {
        assert_eq!(every.total_record_count, counted as i64);
    }
    let episode = ok(client.item(&every.items[0].id));
    assert_eq!(episode.series_name.as_deref(), Some(show.name.as_str()));
    assert!(!episode.media_sources.is_empty());
}

/// An album, its tracks in order, artists, genres, lyrics, and a track
/// played directly.
fn browse_music(client: &Client, music: &str) {
    let albums = ok(client.items(
        &ItemsQuery::new()
            .parent(music)
            .types(&[ItemKind::MusicAlbum])
            .recursive()
            .sort(&["SortName"], SortOrder::Ascending)
            .limit(1),
    ));
    let Some(album) = albums.items.first() else {
        expect(false, "an album");
        return;
    };
    let tracks = ok(client.items(
        &ItemsQuery::new()
            .parent(&album.id)
            .types(&[ItemKind::Audio])
            .recursive()
            .sort(
                &["ParentIndexNumber", "IndexNumber", "SortName"],
                SortOrder::Ascending,
            ),
    ))
    .items;
    assert!(!tracks.is_empty());
    let order: Vec<(Option<i32>, Option<i32>)> = tracks
        .iter()
        .map(|track| (track.parent_index_number, track.index_number))
        .collect();
    assert!(order.is_sorted(), "{order:?}");
    assert!(tracks.iter().all(|track| track.kind == ItemKind::Audio
        && track.media_type == MediaKind::Audio
        && track.album_id.as_deref() == Some(album.id.as_str())));
    let artists = ok(client.album_artists(&ItemsQuery::new().parent(music).limit(SAMPLE))).items;
    assert!(!artists.is_empty());
    assert!(
        artists
            .iter()
            .all(|artist| artist.kind == ItemKind::MusicArtist)
    );
    // The album's artist, found by name, which `/Items` doesn't search.
    if let Some(credit) = album.album_artists.first() {
        let found = ok(client.search_artists(&credit.name, 24));
        assert!(
            found.items.iter().any(|item| item.id == credit.id),
            "{}: {:?}",
            credit.name,
            names(&found.items)
        );
    }
    let genres = ok(client.genres(
        &ItemsQuery::new()
            .parent(music)
            .types(&[ItemKind::MusicAlbum])
            .limit(SAMPLE),
    ))
    .items;
    assert!(
        genres
            .iter()
            .all(|genre| matches!(genre.kind, ItemKind::MusicGenre | ItemKind::Genre)),
        "{:?}",
        names(&genres)
    );
    let (mut with_lyrics, mut timed) = (0, 0);
    for track in tracks.iter().take(SAMPLE as usize) {
        match ok(client.lyrics(&track.id)) {
            Some(lyrics) => {
                assert!(track.has_lyrics, "{}", track.name);
                assert!(!lyrics.lyrics.is_empty());
                with_lyrics += 1;
                timed += usize::from(lyrics.is_timed());
            }
            None => assert!(!track.has_lyrics, "{}", track.name),
        }
    }
    expect(with_lyrics > 0 && timed > 0, "a track with timed lyrics");
    if generated() {
        // The script's album: timed lyrics from an .lrc file on its first
        // track, plain ones from a .txt file on its second, as the drawer
        // shows them.
        let shown = |track: &BaseItemDto| {
            ok(client.lyrics(&track.id))
                .as_ref()
                .and_then(super::lyrics::Lyrics::new)
                .unwrap_or_else(|| panic!("no lyrics for {}", track.name))
        };
        let timed = shown(&tracks[0]);
        assert!(timed.timed);
        let lines: Vec<(&str, Option<f64>)> = timed
            .lines
            .iter()
            .map(|line| (line.text.as_str(), line.start))
            .collect();
        assert_eq!(
            lines,
            [
                ("The water leaves the harbour wall", Some(0.5)),
                ("and takes the evening light", Some(3.0)),
                ("the boats lie down on sand", Some(6.0)),
                ("until the tide comes back tonight", Some(9.0)),
            ]
        );
        assert_eq!(timed.line_at(0.2), None);
        assert_eq!(timed.line_at(4.0), Some(1));
        assert_eq!(timed.line_at(11.9), Some(3));
        let plain = shown(&tracks[1]);
        assert!(!plain.timed);
        let lines: Vec<&str> = plain.lines.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(
            lines,
            [
                "Glass water, still and clear",
                "Every stone is showing here",
                "Nothing moves and nothing sings",
                "Only light on little things",
            ]
        );
        assert_eq!(plain.line_at(4.0), None);
    }

    // A track plays directly too, from a URL without a token.
    let track = ok(client.item(&tracks[0].id));
    let info = ok(client.playback_info(
        &track.id,
        &PlaybackInfoRequest {
            device_profile: Some(DeviceProfile::patina(&TranscodeRules::default())),
            ..Default::default()
        },
    ));
    let source = &info.media_sources[0];
    assert!(source.supports_direct_play && source.transcoding_url.is_none());
    let stream = client.static_stream_url(&track.id, true, source);
    assert!(stream.path().contains("/Audio/"), "{stream}");
    let (status, head) = anonymous_get(&stream, Some("bytes=0-63"));
    assert_eq!(status, 206, "{stream}");
    if source.container.as_deref() == Some("flac") {
        assert!(head.starts_with(b"fLaC"));
    }
    ok(client.report_stopped(&PlaybackStopInfo {
        item_id: track.id.clone(),
        media_source_id: Some(source.id.clone()),
        play_session_id: info.play_session_id.clone(),
        failed: true,
        ..Default::default()
    }));
}

#[test]
fn live_transcode_rules_rule_out_only_what_they_name() {
    let Some(account) = account("patina-tests-rules") else {
        return;
    };
    let client = &account.client;
    let mut hevc = 0;
    for film in films(client) {
        let film = ok(client.item(&film.id));
        let source = &film.media_sources[0];
        let Some(video) = source
            .media_streams
            .iter()
            .find(|stream| stream.kind == StreamKind::Video)
        else {
            continue;
        };
        let play = |rules: TranscodeRules| {
            let info = ok(client.playback_info(
                &film.id,
                &PlaybackInfoRequest {
                    media_source_id: Some(source.id.clone()),
                    max_streaming_bitrate: rules.max_bitrate,
                    device_profile: Some(DeviceProfile::patina(&rules)),
                    ..Default::default()
                },
            ));
            assert_eq!(info.error_code, None);
            let delivered = info.media_sources[0].clone();
            // Nothing is playing, but the server may have started a
            // transcode for the session; the report ends it.
            ok(client.report_stopped(&PlaybackStopInfo {
                item_id: film.id.clone(),
                media_source_id: Some(delivered.id.clone()),
                play_session_id: info.play_session_id,
                failed: true,
                ..Default::default()
            }));
            delivered
        };
        // Why the server transcodes, as its URL says.
        let reasons = |source: &MediaSourceInfo| {
            client
                .transcode_url(source)
                .and_then(|url| {
                    url.query_pairs()
                        .find(|(name, _)| name == "TranscodeReasons")
                        .map(|(_, value)| value.into_owned())
                })
                .unwrap_or_default()
        };
        let codec = video.codec.as_deref();
        // An SD or HD, SDR film within the bitrate, in neither of the
        // codecs the rules name, plays directly whatever else they rule
        // out.
        let plain = video.width.is_some_and(|width| width <= 1920)
            && video.height.is_some_and(|height| height <= 1080)
            && matches!(video.video_range_type.as_deref(), None | Some("SDR"))
            && !matches!(codec, Some("hevc" | "av1"));
        if plain {
            let delivered = play(TranscodeRules {
                max_bitrate: Some(source.bitrate.unwrap_or(0) * 4 + 1_000_000),
                hevc: true,
                av1: true,
                hdr: true,
                over_1080p: true,
            });
            assert!(
                delivered.supports_direct_play,
                "{}: {}",
                film.name,
                reasons(&delivered)
            );
        }
        if codec == Some("hevc") {
            hevc += 1;
            let delivered = play(TranscodeRules::default());
            assert!(
                delivered.supports_direct_play,
                "{}: {}",
                film.name,
                reasons(&delivered)
            );
            let delivered = play(TranscodeRules {
                hevc: true,
                ..Default::default()
            });
            assert!(!delivered.supports_direct_play, "{}", film.name);
            let url = client.transcode_url(&delivered).expect("a transcode");
            let codecs = url
                .query_pairs()
                .find(|(name, _)| name == "VideoCodec")
                .map(|(_, value)| value.into_owned());
            assert_eq!(codecs.as_deref(), Some("h264"), "{url}");
        }
    }
    expect(hevc > 0, "an HEVC film");
}

/// Of the first few films, the unplayed one with the most external
/// subtitles, which the test library's Night Signal is: playing it from
/// the start and stopping there leaves nothing changed but its play count.
fn unstarted_film(client: &Client) -> BaseItemDto {
    films(client)
        .iter()
        .filter(|film| !film.played() && film.resume_position().is_none())
        .map(|film| ok(client.item(&film.id)))
        .max_by_key(|film| {
            film.media_sources.first().map_or(0, |source| {
                source
                    .media_streams
                    .iter()
                    .filter(|stream| stream.is_external)
                    .count()
            })
        })
        .expect("a film nobody has started")
}

#[test]
fn live_a_film_plays_directly_or_transcoded_and_reports_progress() {
    let Some(account) = account("patina-tests-playback") else {
        return;
    };
    let client = &account.client;
    let token = client.token().unwrap().to_owned();
    let film = unstarted_film(client);
    let source = &film.media_sources[0];
    let request = |rules: &TranscodeRules| PlaybackInfoRequest {
        media_source_id: Some(source.id.clone()),
        max_streaming_bitrate: rules.max_bitrate,
        device_profile: Some(DeviceProfile::patina(rules)),
        ..Default::default()
    };

    // Direct play: the file as it is, which needs no token.
    let direct = ok(client.playback_info(&film.id, &request(&TranscodeRules::default())));
    assert_eq!(direct.error_code, None);
    let play_session_id = direct.play_session_id.clone().expect("a play session");
    let delivered = &direct.media_sources[0];
    assert_eq!(delivered.id, source.id);
    assert!(
        delivered.supports_direct_play,
        "{} doesn't play directly",
        film.name
    );
    assert!(delivered.transcoding_url.is_none());
    let stream = client.static_stream_url(&film.id, false, delivered);
    assert!(
        !stream.as_str().contains(&token),
        "the stream's URL carries the token"
    );
    let (status, head) = anonymous_get(&stream, Some("bytes=0-1023"));
    assert_eq!(status, 206, "{stream}");
    assert_eq!(head.len(), 1024);
    if delivered.container.as_deref() == Some("mkv") {
        assert!(head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]));
    }

    // External subtitles: the server writes the token into their URLs,
    // where it doesn't outlast reading them; without it they are still
    // served.
    for subtitle in delivered.media_streams.iter().filter(|s| s.is_external) {
        assert_eq!(subtitle.delivery_method, Some(DeliveryMethod::External));
        let raw = subtitle.delivery_url.as_deref().unwrap();
        assert!(
            !raw.contains(&token) && !raw.to_ascii_lowercase().contains("apikey"),
            "a subtitle's URL kept the token"
        );
        let url = client.delivery_url(subtitle).expect("on the server");
        let (status, text) = anonymous_get(&url, None);
        assert_eq!(status, 200, "{url}");
        assert!(String::from_utf8_lossy(&text).contains("-->"), "{url}");
    }
    if let Some(index) = delivered.default_audio_stream_index {
        assert_eq!(
            delivered.media_streams[index as usize].kind,
            StreamKind::Audio
        );
    }

    // The reports, as the reporter sends them.
    let report = PlaybackProgressInfo {
        item_id: film.id.clone(),
        media_source_id: Some(source.id.clone()),
        play_session_id: Some(play_session_id.clone()),
        position_ticks: Some(0),
        audio_stream_index: delivered.default_audio_stream_index,
        subtitle_stream_index: delivered.default_subtitle_stream_index,
        play_method: PlayMethod::DirectPlay,
        can_seek: true,
        volume_level: Some(100),
        ..Default::default()
    };
    ok(client.report_start(&report));
    ok(client.report_progress(&PlaybackProgressInfo {
        position_ticks: Some(seconds_to_ticks(0.5)),
        ..report.clone()
    }));
    ok(client.report_progress(&PlaybackProgressInfo {
        is_paused: true,
        ..report.clone()
    }));
    // A position a hair below zero, as mpv can give, still stops: the
    // server refuses a negative one.
    ok(client.report_stopped(&PlaybackStopInfo {
        item_id: film.id.clone(),
        media_source_id: Some(source.id.clone()),
        play_session_id: Some(play_session_id),
        position_ticks: seconds_to_ticks(-0.02),
        ..Default::default()
    }));
    // Stopped at the start: neither watched nor with a place to resume.
    let after = ok(client.item(&film.id));
    assert!(!after.played() && after.resume_position().is_none());
    assert!(after.user_data.unwrap().play_count > 0);

    // Streams chosen by their index, which the server applies only with
    // the version named: another audio track, and subtitles off.
    let audio: Vec<i32> = delivered
        .media_streams
        .iter()
        .filter(|stream| stream.kind == StreamKind::Audio)
        .map(|stream| stream.index)
        .collect();
    if let Some(&other) = audio
        .iter()
        .find(|index| Some(**index) != delivered.default_audio_stream_index)
    {
        let chosen = ok(client.playback_info(
            &film.id,
            &PlaybackInfoRequest {
                audio_stream_index: Some(other),
                subtitle_stream_index: Some(-1),
                ..request(&TranscodeRules::default())
            },
        ));
        let source = &chosen.media_sources[0];
        assert!(source.supports_direct_play);
        assert_eq!(source.default_audio_stream_index, Some(other));
        assert_eq!(source.default_subtitle_stream_index, Some(-1));
    }

    // A few films' files are served as they are, an MP4 under its first
    // container name ("mov,mp4,m4a,...").
    for other in films(client) {
        let other = ok(client.item(&other.id));
        let source = &other.media_sources[0];
        let stream = client.static_stream_url(&other.id, false, source);
        let (status, head) = anonymous_get(&stream, Some("bytes=0-15"));
        assert_eq!(status, 206, "{stream}");
        if source
            .container
            .as_deref()
            .is_some_and(|c| c.contains("mp4"))
        {
            assert_eq!(&head[4..8], b"ftyp", "{stream}");
        }
    }

    // A quality below the file's makes the server transcode. With the
    // header, and no token in the URL, the playlists and the first segment
    // come all the same, and no playlist names the token anywhere: the
    // trickplay rendition that would is not asked for, and subtitles are
    // not put in the playlist.
    // Low enough to be cheap for the server whatever the film.
    let low = TranscodeRules {
        max_bitrate: Some((source.bitrate.unwrap_or(1_000_000) / 4).clamp(64_000, 2_000_000)),
        ..Default::default()
    };
    let transcoded = ok(client.playback_info(&film.id, &request(&low)));
    assert_eq!(transcoded.error_code, None);
    let delivered = &transcoded.media_sources[0];
    let raw = delivered
        .transcoding_url
        .as_deref()
        .expect("a transcoding URL at a low bitrate");
    assert!(
        !raw.contains(&token) && !raw.to_ascii_lowercase().contains("apikey"),
        "the transcoding URL kept the token"
    );
    assert_eq!(delivered.transcoding_sub_protocol.as_deref(), Some("hls"));
    let master_url = client.transcode_url(delivered).unwrap();
    let (path, query) = raw.split_once('?').unwrap();
    assert_eq!(master_url.path(), path);
    assert_eq!(
        master_url.query().unwrap(),
        format!("{query}&EnableTrickplay=false")
    );
    assert_eq!(anonymous_get(&master_url, None).0, 401);
    let clean =
        |text: &str| !text.to_ascii_lowercase().contains("apikey") && !text.contains(&token);
    let master = String::from_utf8(ok(client.get_bytes(&master_url))).unwrap();
    assert!(master.starts_with("#EXTM3U"), "{}", hide(&master, &token));
    assert!(
        clean(&master) && !master.contains("EXT-X-IMAGE-STREAM-INF"),
        "the master playlist names the token:\n{}",
        hide(&master, &token)
    );
    let variant = master
        .lines()
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .expect("a variant playlist");
    let variant_url = master_url.join(variant).unwrap();
    let media = String::from_utf8(ok(client.get_bytes(&variant_url))).unwrap();
    assert!(media.starts_with("#EXTM3U"), "{}", hide(&media, &token));
    assert!(
        clean(&media),
        "the media playlist names the token:\n{}",
        hide(&media, &token)
    );
    let segment = media
        .lines()
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .expect("a segment");
    let bytes = ok(client.get_bytes(&variant_url.join(segment).unwrap()));
    // MPEG-TS packets begin with 0x47.
    assert_eq!(bytes.first(), Some(&0x47), "{segment}");
    ok(client.report_stopped(&PlaybackStopInfo {
        item_id: film.id.clone(),
        media_source_id: Some(delivered.id.clone()),
        play_session_id: transcoded.play_session_id.clone(),
        position_ticks: 0,
        ..Default::default()
    }));
    let after = ok(client.item(&film.id));
    assert!(!after.played() && after.resume_position().is_none());
}

#[test]
fn live_played_and_favourite_marks_come_and_go() {
    let Some(account) = account("patina-tests-marks") else {
        return;
    };
    let client = &account.client;
    let movies = movie_library(client);
    let film = films(client)
        .into_iter()
        .find(|film| !film.played() && film.resume_position().is_none())
        .expect("a film nobody has started");
    let before = film.user_data.clone().unwrap_or_default();
    let _restore = Restore(|| {
        let _ = client.set_played(&film.id, before.played);
        let _ = client.set_favorite(&film.id, before.is_favorite);
    });
    // Whether the library's query with a filter finds the film; asked of
    // the film alone, so that a large library isn't listed.
    let listed = |query: ItemsQuery| {
        ok(client.items(
            &query
                .parent(&movies)
                .types(&[ItemKind::Movie])
                .recursive()
                .ids(&[&film.id]),
        ))
        .items
        .iter()
        .any(|item| item.id == film.id)
    };
    assert!(listed(ItemsQuery::new()));

    let data = ok(client.set_played(&film.id, true));
    assert!(data.played);
    assert_eq!(data.item_id, film.id);
    assert!(ok(client.item(&film.id)).played());
    assert!(listed(ItemsQuery::new().played(true)));
    let data = ok(client.set_played(&film.id, false));
    assert!(!data.played && data.playback_position_ticks == 0);
    assert!(!listed(ItemsQuery::new().played(true)));

    let data = ok(client.set_favorite(&film.id, !before.is_favorite));
    assert_eq!(data.is_favorite, !before.is_favorite);
    assert_eq!(
        listed(ItemsQuery::new().favorite(true)),
        !before.is_favorite
    );
    let data = ok(client.set_favorite(&film.id, before.is_favorite));
    assert_eq!(data.is_favorite, before.is_favorite);

    let after = ok(client.item(&film.id));
    assert_eq!(after.played(), before.played);
    assert_eq!(after.is_favorite(), before.is_favorite);
    assert_eq!(after.resume_position(), None);
}

/// A film downloaded as Patina downloads one: the smallest of the sample,
/// whole, whose bytes are the server's file's (read where the server keeps
/// it when that is on this computer, as the generated library's is, and
/// otherwise as the server streams it), and the largest cancelled after
/// its first bytes, which leaves no part file. The server notes each
/// download in its activity log.
#[test]
fn live_a_film_downloads_as_its_file_and_a_cancelled_one_leaves_nothing() {
    use super::download::{self, Failure};
    let Some(account) = account("patina-tests-download") else {
        return;
    };
    let client = &account.client;
    if !account.user.policy.enable_content_downloading {
        expect(false, "the account may download");
        return;
    }
    let size = |film: &BaseItemDto| {
        film.media_sources
            .first()
            .and_then(|source| source.size)
            .unwrap_or(i64::MAX)
    };
    let mut sample: Vec<BaseItemDto> = films(client)
        .iter()
        .map(|film| ok(client.item(&film.id)))
        .filter(|film| film.can_download == Some(true))
        .collect();
    sample.sort_by_key(size);
    let (Some(smallest), Some(largest)) = (sample.first(), sample.last()) else {
        panic!("no film the server offers for download");
    };
    let dir = std::env::temp_dir().join(format!("patina-live-download-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let never = download::Stop::default();
    let fallback = download::fallback_name(smallest);
    let saved = download::run(client, &smallest.id, &fallback, &dir, &never, |_| {})
        .unwrap_or_else(|failure| panic!("{failure:?}"));
    let bytes = std::fs::read(&saved).unwrap();
    let local = smallest
        .path
        .as_deref()
        .map(Path::new)
        .filter(|path| path.is_file());
    expect(
        local.is_some(),
        "the server's media folder is on this computer",
    );
    let original = match local {
        Some(path) => {
            // Named as the server's file is.
            assert_eq!(saved.file_name(), path.file_name());
            std::fs::read(path).unwrap()
        }
        None => {
            let url = client.static_stream_url(&smallest.id, false, &smallest.media_sources[0]);
            anonymous_get(&url, None).1
        }
    };
    assert_eq!(bytes.len() as i64, size(smallest));
    assert!(
        bytes == original,
        "the download differs from the server's file"
    );

    let cancel = download::Stop::default();
    let mut written = 0;
    let mut part = 0;
    let fallback = download::fallback_name(largest);
    let stopped = download::run(client, &largest.id, &fallback, &dir, &cancel, |progress| {
        if progress.done > 0 && !cancel.stopped() {
            written = progress.done;
            part = std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".part"))
                .filter_map(|entry| entry.metadata().ok())
                .map(|meta| meta.len())
                .sum();
            cancel.stop();
        }
    });
    assert_eq!(stopped, Err(Failure::Cancelled));
    assert!(part > 0, "the part file had nothing in it");
    expect(
        (written as i64) < size(largest),
        "the cancel came before the film's end",
    );
    let left: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(left, [saved]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Scan Network against a stand-in that answers as Jellyfin 12.4 does,
/// since the test server's own discovery is off. The server reports the
/// address it thinks it has, with the port it listens on: from a
/// container, an address only its host can reach and the container's own
/// port, not the one mapped to it, which Scan Network can't learn. The
/// same address at the IP the answer came from is offered first; when the
/// live server listens on that IP, Patina connects to it there and finds
/// the server that answered.
#[test]
fn discovery_offers_the_address_the_answer_came_from() {
    let live = std::env::var("PATINA_TEST_JELLYFIN_URL")
        .ok()
        .map(|address| ok(auth::normalize_address(&address)));
    let responder = UdpSocket::bind("127.0.0.1:0").unwrap();
    let target = responder.local_addr().unwrap();
    // A live server at the stand-in's IP really listens on its port; any
    // other is taken to be in a container, as a NAS's often is.
    let here = live
        .as_ref()
        .filter(|address| address.host() == Some(url::Host::Ipv4(std::net::Ipv4Addr::LOCALHOST)));
    let port = here.and_then(Url::port_or_known_default).unwrap_or(8096);
    let identity = Identity::new(store::DEVICE_NAME, "patina-tests-discovery");
    let info = match &live {
        Some(address) => ok(ok(Client::new(address.clone(), identity.clone(), None)).public_info()),
        None => fixture!("system_info_public"),
    };

    // As the server serializes its answer: every property, null ones too.
    let answer = format!(
        r#"{{"Address":"http://192.0.2.10:{port}","Id":"{}","Name":"{}","EndpointAddress":null}}"#,
        info.id, info.server_name
    );
    let thread = std::thread::spawn(move || {
        let mut buffer = [0u8; 512];
        let (length, from) = responder.recv_from(&mut buffer).unwrap();
        let question = String::from_utf8_lossy(&buffer[..length]).to_ascii_lowercase();
        assert!(question.contains("who is jellyfinserver?"), "{question}");
        responder.send_to(answer.as_bytes(), from).unwrap();
    });
    let found = discovery::scan_at(target, Duration::from_millis(500)).unwrap();
    thread.join().unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    let server = &found[0];
    assert_eq!((&server.id, &server.name), (&info.id, &info.server_name));
    assert_eq!(server.endpoint_address, None);
    assert_eq!(
        server.addresses(),
        [
            format!("http://127.0.0.1:{port}"),
            format!("http://192.0.2.10:{port}")
        ]
    );

    if here.is_some() {
        let _turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let probed = ok(api::probe(&server.addresses()[0], &identity, None));
        assert_eq!(probed.info.id, server.id);
    }
}

/// mpv takes the header as one field only when it is set as a list: set as
/// one string, mpv splits it at its commas, and the server gets an
/// `Authorization` header without the token.
#[test]
fn mpv_takes_the_header_as_one_field() {
    let Some(mpv) = crate::mpv::testing::headless() else {
        return;
    };
    let client = Client::new(
        Url::parse("http://127.0.0.1:18096").unwrap(),
        Identity::new(store::DEVICE_NAME, "patina-tests-mpv"),
        None,
    )
    .unwrap()
    .with_session("a-token", "u1");
    let fields = client.http_header_fields();
    mpv.set_property(
        "http-header-fields",
        crate::mpv::Value::List(fields.clone()),
    )
    .unwrap();
    let set = mpv.get_property("http-header-fields").unwrap();
    let set: Vec<&str> = set
        .as_array()
        .iter()
        .filter_map(|field| field.as_str())
        .collect();
    assert_eq!(set, fields);
    assert!(set[0].ends_with("Token=\"a-token\""));

    mpv.set_property("http-header-fields", fields[0].as_str())
        .unwrap();
    let split = mpv.get_property("http-header-fields").unwrap();
    assert!(split.as_array().len() > 1);
}

/// The device profile tools/jellyfin-server.sh asks for the playback
/// fixtures with is the one Patina sends.
#[test]
fn the_fixtures_device_profile_is_patinas() {
    let script = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/jellyfin-server.sh"),
    )
    .unwrap();
    let (_, after) = script
        .split_once("DEVICE_PROFILE='")
        .expect("DEVICE_PROFILE in the script");
    let (profile, _) = after.split_once('\'').unwrap();
    let profile: serde_json::Value = serde_json::from_str(profile).unwrap();
    assert_eq!(
        profile,
        serde_json::to_value(DeviceProfile::patina(&TranscodeRules::default())).unwrap()
    );
}

// Playback through the load hook.

/// What a load hook needs to play from the account's server, as a window
/// gives it.
fn hook(account: &Account, max_bitrate: Option<i64>) -> playback::Hook {
    let client = &account.client;
    let servers = playback::Servers::default();
    servers.set(playback::Server {
        id: account.user.server_id.clone(),
        client: client.clone(),
        ca_file: None,
    });
    servers.loaded();
    playback::Hook {
        servers,
        registry: session::Registry::default(),
        versions: HashMap::new(),
        max_bitrate,
        rules: TranscodeRules::default(),
        resolved: playback::Resolved::default(),
    }
}

/// Opens `entry` in a headless mpv through the load hook, as a player
/// window does, and waits for mpv to have opened the file: what the hook
/// made of it. What mpv logged meanwhile goes into `log`.
fn play_entry(
    mpv: &crate::mpv::Mpv,
    hook: &playback::Hook,
    entry: &str,
    log: &mut Vec<String>,
) -> playback::Playing {
    use crate::mpv::{EndFileReason, Event};
    mpv.command(&["loadfile", entry]).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    while std::time::Instant::now() < deadline {
        match mpv.wait_event(0.5) {
            Some(Event::Log(message)) => log.push(message.text),
            Some(Event::Hook { id, .. }) => {
                playback::prepare(mpv, entry, hook, None);
                mpv.hook_continue(id).unwrap();
            }
            Some(Event::FileLoaded) => {
                let resolved = hook.resolved.lock().unwrap().take();
                match resolved {
                    Some((resolved, Ok(playing))) if resolved == entry => return playing,
                    other => panic!("the hook resolved {other:?}"),
                }
            }
            Some(Event::EndFile {
                reason: EndFileReason::Error,
                error,
                ..
            }) => {
                let resolved = hook.resolved.lock().unwrap().take();
                panic!("{entry} didn't open: {error:?}, {resolved:?}");
            }
            _ => {}
        }
    }
    panic!("{entry} took too long to open");
}

/// A server that takes the connection and never answers holds a load up
/// only until the user stops: the hook gives up on it, well before the
/// requests time out, and says why.
#[test]
fn a_load_waiting_on_a_silent_server_gives_way_to_a_stop() {
    use crate::mpv::Event;
    let Some(mpv) = hooked_mpv() else {
        return;
    };
    // The listener's backlog takes the connection; nothing reads it.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", silent.local_addr().unwrap());
    let client = Client::new(
        Url::parse(&address).unwrap(),
        Identity::new(store::DEVICE_NAME, "patina-tests-silent"),
        None,
    )
    .unwrap()
    .with_session("a-token", "u1");
    let servers = playback::Servers::default();
    servers.set(playback::Server {
        id: "srv".into(),
        client,
        ca_file: None,
    });
    servers.loaded();
    let hook = playback::Hook {
        servers,
        registry: session::Registry::default(),
        versions: HashMap::new(),
        max_bitrate: None,
        rules: TranscodeRules::default(),
        resolved: playback::Resolved::default(),
    };
    let entry = session::entry_url("srv", "item").unwrap();
    // As Resume Playback has it.
    mpv.set_property("save-position-on-quit", true).unwrap();
    mpv.command(&["loadfile", &entry]).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let id = loop {
        assert!(std::time::Instant::now() < deadline, "no load hook");
        if let Some(Event::Hook { id, .. }) = mpv.wait_event(0.5) {
            break id;
        }
    };
    let started = std::time::Instant::now();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(500));
            // A replace or a quit while the server is asked writes no
            // watch-later file for the entry.
            assert_eq!(
                mpv.get_property("save-position-on-quit").unwrap(),
                crate::mpv::Node::Flag(false)
            );
            mpv.command(&["stop"]).unwrap();
        });
        playback::prepare(&mpv, &entry, &hook, None);
    });
    let waited = started.elapsed();
    assert!(waited < Duration::from_secs(5), "waited {waited:?}");
    mpv.hook_continue(id).unwrap();
    let resolved = hook.resolved.lock().unwrap().take();
    assert_eq!(resolved, Some((entry, Err(playback::Failure::Abandoned))));
    drop(silent);
}

/// A headless mpv that asks Patina before opening a file, as a player
/// window's does, and logs as much as Patina asks of it when verbose.
fn hooked_mpv() -> Option<crate::mpv::Mpv> {
    let mpv = crate::mpv::testing::headless()?;
    mpv.hook_add(1, "on_load", -50).unwrap();
    mpv.request_log_messages("v").unwrap();
    Some(mpv)
}

/// mpv's log names the streams it opens, never the token.
fn assert_unlogged(log: &[String], token: &str) {
    assert!(!log.is_empty(), "mpv logged nothing");
    for line in log {
        assert!(
            !line.contains(token) && !line.to_ascii_lowercase().contains("apikey"),
            "mpv logged the token: {}",
            hide(line, token)
        );
    }
}

/// The account's session on the server for this test's device, as
/// `GET /Sessions` gives it.
fn session_of(client: &Client) -> Option<serde_json::Value> {
    let mut url = auth::api_url(client.base(), "/Sessions");
    url.set_query(Some(&format!(
        "deviceId={}",
        auth::encode(&client.identity().device_id)
    )));
    let sessions: serde_json::Value = serde_json::from_slice(&ok(client.get_bytes(&url))).unwrap();
    sessions.as_array()?.first().cloned()
}

fn mpv_string(mpv: &crate::mpv::Mpv, name: &str) -> String {
    mpv.get_property(name)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// A film resolves to its file, plays in mpv with the server's subtitles,
/// and the server follows its start, progress and stop.
#[test]
fn live_a_film_entry_plays_through_the_hook_and_is_reported() {
    let Some(account) = account("patina-tests-hook-film") else {
        return;
    };
    let Some(mpv) = hooked_mpv() else {
        return;
    };
    let client = &account.client;
    let token = client.token().unwrap().to_owned();
    let film = unstarted_film(client);
    let entry = session::entry_url(&account.user.server_id, &film.id).unwrap();
    let hook = hook(&account, None);
    let mut log = Vec::new();
    let playing = play_entry(&mpv, &hook, &entry, &mut log);
    assert_unlogged(&log, &token);

    assert_eq!(playing.item_id, film.id);
    assert_eq!(playing.method, PlayMethod::DirectPlay);
    // Played as it is, the file and its subtitles need no token: mpv
    // holds no header for it.
    assert!(
        mpv.get_property("http-header-fields")
            .unwrap()
            .as_array()
            .is_empty(),
        "mpv has a header for a file it plays as it is"
    );
    assert_eq!(playing.start, 0.0, "{} was never started", film.name);
    // mpv knows the entry, its title and the file, and the token only as
    // the header.
    assert_eq!(mpv_string(&mpv, "path"), entry);
    assert_eq!(mpv_string(&mpv, "media-title"), film.name);
    let stream = mpv_string(&mpv, "stream-path");
    assert!(
        stream.contains(&format!("/Videos/{}/stream", film.id)),
        "{stream}"
    );
    assert!(
        !stream.contains(&token),
        "the stream's URL carries the token"
    );
    // The hook registered what it found for the playlist's titles.
    assert_eq!(
        hook.registry
            .lock()
            .unwrap()
            .get(&entry)
            .map(|info| info.title.clone()),
        Some(film.name.clone())
    );

    let tracks = mpv.get_property("track-list").unwrap();
    let tracks: Vec<playback::MpvTrack> = tracks
        .as_array()
        .iter()
        .filter_map(|track| {
            let kind = match track.get("type")?.as_str()? {
                "video" => StreamKind::Video,
                "audio" => StreamKind::Audio,
                "sub" => StreamKind::Subtitle,
                _ => return None,
            };
            let external = track
                .get("external")
                .and_then(|external| match external {
                    crate::mpv::Node::Flag(flag) => Some(*flag),
                    _ => None,
                })
                .unwrap_or(false);
            Some(playback::MpvTrack {
                kind,
                id: track.get("id")?.as_i64()?,
                external,
                filename: track
                    .get("external-filename")
                    .and_then(|name| name.as_str())
                    .filter(|_| external)
                    .map(str::to_owned),
            })
        })
        .collect();
    let external = tracks
        .iter()
        .filter(|track| track.kind == StreamKind::Subtitle && track.external)
        .count();
    assert_eq!(external, playing.external.len());
    if generated() && film.name == "Night Signal" {
        assert_eq!(external, 2, "Night Signal's two external subtitles");
    }
    // Every stream of the source is the track mapping says, kind for kind:
    // the external ones by the URL mpv loaded them from.
    for stream in &playing.streams {
        if let Some((kind, id)) = playing.track_id(stream.index) {
            assert_eq!(playing.track_of(stream.index, &tracks), Some(id));
            let track = tracks
                .iter()
                .find(|track| track.kind == kind && track.id == id)
                .unwrap_or_else(|| panic!("stream {} as track {id}", stream.index));
            assert_eq!(playing.stream_index(track), Some(stream.index));
        }
    }
    // The hook selected the server's tracks as the file opened: nothing
    // is left for the window to change.
    let selection = playing.selection(&tracks);
    assert_eq!(selection, playing.planned());
    let aid = mpv.get_property("aid").ok().and_then(|id| id.as_i64());
    let sid = mpv.get_property("sid").ok().and_then(|id| id.as_i64());
    if let playback::Pick::Track(id) = selection.aid {
        assert_eq!(aid, Some(id), "the server's audio is playing");
    }
    match selection.sid {
        playback::Pick::Track(id) => assert_eq!(sid, Some(id)),
        playback::Pick::Off => assert_eq!(sid, None),
        playback::Pick::Auto => {}
    }
    assert_eq!(
        playing.reported_streams(&tracks, aid, sid).0,
        playing.audio,
        "the server's audio is reported"
    );

    // The reports, as the window sends them, and what the server makes of
    // them.
    let (reporter, answered) = playback::Reporter::start().unwrap();
    let send = |report| {
        reporter.send(&playing.server_id, client, report);
        assert!(reporter.flush(std::time::Instant::now() + Duration::from_secs(20)));
        let sent = answered.recv_blocking().unwrap();
        ok(sent.result);
    };
    let at = playback::Moment {
        position: 1.0,
        volume: 100.0,
        seekable: true,
        aid,
        sid,
        tracks: tracks.clone(),
        ..Default::default()
    };
    send(playback::Report::Start(playing.progress(&at)));
    let session = session_of(client).expect("a session for the device");
    assert_eq!(session["NowPlayingItem"]["Id"], film.id.as_str());
    assert_eq!(session["PlayState"]["PlayMethod"], "DirectPlay");
    assert_eq!(
        session["PlayState"]["MediaSourceId"],
        playing.media_source_id.as_str()
    );
    let paused = playback::Moment {
        position: 2.5,
        paused: true,
        ..at.clone()
    };
    send(playback::Report::Progress(playing.progress(&paused)));
    let session = session_of(client).unwrap();
    assert_eq!(session["PlayState"]["PositionTicks"], 25_000_000);
    assert_eq!(session["PlayState"]["IsPaused"], true);
    if let Some(audio) = playing.audio {
        assert_eq!(session["PlayState"]["AudioStreamIndex"], audio);
    }
    // Stopped at the start, so that the film stays one nobody has
    // started: neither watched nor with a place to resume.
    send(playback::Report::Stopped(playing.stopped(0.0, false)));
    let session = session_of(client).unwrap();
    assert!(
        session
            .get("NowPlayingItem")
            .is_none_or(|item| item.is_null()),
        "still playing after the stop: {}",
        hide(&session.to_string(), &token)
    );
    let after = ok(client.item(&film.id));
    assert!(!after.played() && after.resume_position().is_none());
}

/// An episode plays with its show in its title, and a quality below the
/// file's plays the server's transcode, with the header and no token.
#[test]
fn live_an_episode_and_a_transcode_play_through_the_hook() {
    let Some(account) = account("patina-tests-hook-episode") else {
        return;
    };
    let Some(mpv) = hooked_mpv() else {
        return;
    };
    let client = &account.client;
    let token = client.token().unwrap().to_owned();
    let episodes = ok(client.items(
        &ItemsQuery::new()
            .types(&[ItemKind::Episode])
            .recursive()
            .sort(
                &["SeriesSortName", "ParentIndexNumber", "IndexNumber"],
                SortOrder::Ascending,
            )
            .limit(SAMPLE),
    ))
    .items;
    let with_subtitles = episodes
        .iter()
        .map(|episode| ok(client.item(&episode.id)))
        .find(|episode| {
            episode.media_sources.first().is_some_and(|source| {
                source
                    .media_streams
                    .iter()
                    .any(|stream| stream.is_external && stream.kind == StreamKind::Subtitle)
            })
        });
    if with_subtitles.is_none() {
        eprintln!("no episode with external subtitles here; their mapping is checked with a film");
    }
    match with_subtitles.or_else(|| episodes.first().cloned()) {
        Some(episode) => {
            let entry = session::entry_url(&account.user.server_id, &episode.id).unwrap();
            let hook = hook(&account, None);
            let playing = play_entry(&mpv, &hook, &entry, &mut Vec::new());
            let series = episode.series_name.clone().unwrap_or_default();
            assert!(playing.title.starts_with(&series), "{}", playing.title);
            assert!(playing.title.ends_with(&episode.name), "{}", playing.title);
            assert_eq!(mpv_string(&mpv, "media-title"), playing.title);
            assert_eq!(playing.shown.artist, series);
            assert!(!playing.shown.art_url.contains(&token));
            let subtitles = mpv
                .get_property("track-list")
                .unwrap()
                .as_array()
                .iter()
                .filter(|track| {
                    track.get("type").and_then(|kind| kind.as_str()) == Some("sub")
                        && track.get("external") == Some(&crate::mpv::Node::Flag(true))
                })
                .count();
            assert_eq!(subtitles, playing.external.len());
            mpv.command(&["stop"]).unwrap();
        }
        None => expect(false, "an episode"),
    }

    // A film at the lowest quality, 2 Mbit/s, or lower still for a file
    // already below it (the generated ones are): the transcode's
    // playlist, which only the header opens.
    let film = unstarted_film(client);
    let entry = session::entry_url(&account.user.server_id, &film.id).unwrap();
    let bitrate = film.media_sources[0].bitrate.unwrap_or(8_000_000);
    let hook = hook(&account, Some((bitrate / 4).clamp(64_000, 2_000_000)));
    let mut log = Vec::new();
    let playing = play_entry(&mpv, &hook, &entry, &mut log);
    assert_eq!(playing.method, PlayMethod::Transcode);
    // Its playlists need the token: mpv holds the header for this file,
    // follows no redirect with it, and opens no next entry with it.
    assert_eq!(
        mpv.get_property("http-header-fields")
            .unwrap()
            .as_array()
            .len(),
        1
    );
    assert_eq!(
        mpv.get_property("stream-lavf-o")
            .unwrap()
            .get("max_redirects")
            .and_then(|value| value.as_str()),
        Some("0")
    );
    assert_eq!(
        mpv.get_property("prefetch-playlist").unwrap(),
        crate::mpv::Node::Flag(false)
    );
    // The playlists, which mpv reads and logs, carry no token either.
    assert_unlogged(&log, &token);
    let stream = mpv_string(&mpv, "stream-path");
    assert!(stream.contains("master.m3u8"), "{}", hide(&stream, &token));
    assert!(
        !stream.contains(&token) && !stream.to_ascii_lowercase().contains("apikey"),
        "the transcode's URL carries the token"
    );
    assert!(stream.contains("EnableTrickplay=false"), "{stream}");
    assert!(stream.contains("VideoBitrate="), "{stream}");
    let (reporter, answered) = playback::Reporter::start().unwrap();
    reporter.send(
        &playing.server_id,
        client,
        playback::Report::Stopped(playing.stopped(0.0, false)),
    );
    assert!(reporter.flush(std::time::Instant::now() + Duration::from_secs(20)));
    ok(answered.recv_blocking().unwrap().result);
    mpv.command(&["stop"]).unwrap();
}
