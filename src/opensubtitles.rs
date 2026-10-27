//! Subtitles from OpenSubtitles.com, as IINA finds them: searched for by
//! the file's OpenSubtitles hash and its name in the preferred languages,
//! then downloaded one at a time.
//!
//! The REST API wants an API key, which each user makes for themselves in
//! their OpenSubtitles profile, and allows more downloads a day to a
//! signed-in account. Every call here blocks; the windows make them on
//! threads of their own.

use serde::Deserialize;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What OpenSubtitles passwords are saved under in the system's store.
pub const PASSWORD_SERVICE: &str = "Patina OpenSubtitles";

/// The API, which `PATINA_OPENSUBTITLES_API` replaces for testing.
const API: &str = "https://api.opensubtitles.com/api/v1";
/// OpenSubtitles hashes 64 KiB from each end of the file.
const HASH_CHUNK: u64 = 65536;
/// Files this small have no OpenSubtitles hash.
const HASH_MIN_SIZE: u64 = 131_072;
/// Sign-ins last 24 hours; renew a little before.
const TOKEN_LIFETIME: Duration = Duration::from_secs(23 * 3600);
/// The longest Patina waits out a rate limit before giving up.
const MAX_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    NoApiKey,
    /// OpenSubtitles turned the API key down.
    BadApiKey,
    /// Signing in failed, with OpenSubtitles' reason.
    SignIn(String),
    /// The day's downloads are used up, until the time given.
    Quota(Option<String>),
    RateLimited,
    NotFound,
    Network(String),
    /// Anything else OpenSubtitles answered with: status and message.
    Server(u16, String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoApiKey => write!(
                f,
                "Online subtitles need an OpenSubtitles API key: make one in your \
                 opensubtitles.com profile and enter it in Preferences › Subtitles."
            ),
            Error::BadApiKey => write!(
                f,
                "OpenSubtitles did not accept the API key in Preferences › Subtitles."
            ),
            Error::SignIn(reason) => write!(f, "Could not sign in to OpenSubtitles: {reason}"),
            Error::Quota(Some(reset)) => write!(
                f,
                "The day's OpenSubtitles downloads are used up; more from {reset}."
            ),
            Error::Quota(None) => write!(f, "The day's OpenSubtitles downloads are used up."),
            Error::RateLimited => write!(f, "OpenSubtitles is busy; try again in a moment."),
            Error::NotFound => write!(f, "No subtitles found."),
            Error::Network(error) => write!(f, "Could not reach OpenSubtitles: {error}"),
            Error::Server(status, message) if message.is_empty() => {
                write!(f, "OpenSubtitles answered with an error ({status}).")
            }
            Error::Server(status, message) => {
                write!(
                    f,
                    "OpenSubtitles answered with an error ({status}): {message}"
                )
            }
        }
    }
}

/// An OpenSubtitles account, for more downloads a day.
#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    pub username: String,
    pub password: String,
}

/// What to look for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Query {
    /// Language codes as in the preferences ("en", "ger", "pt-br").
    pub languages: Vec<String>,
    /// The file's OpenSubtitles hash, when it is a local file.
    pub hash: Option<String>,
    /// The file name without its extension, or a title.
    pub text: String,
}

/// One subtitle file found.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub file_id: u64,
    pub file_name: String,
    pub language: String,
    pub release: String,
    pub year: Option<i32>,
    pub fps: Option<f64>,
    pub downloads: u64,
    /// The upload date, as "2024-05-17".
    pub uploaded: String,
    /// Made for this very file: its hash matched.
    pub exact: bool,
    pub hearing_impaired: bool,
    pub machine_translated: bool,
}

/// A downloaded subtitle file.
#[derive(Debug, Clone, PartialEq)]
pub struct Downloaded {
    pub file_name: String,
    pub contents: Vec<u8>,
    /// Downloads left today, when OpenSubtitles says.
    pub remaining: Option<i64>,
}

#[derive(Deserialize)]
struct Languages {
    data: Vec<Language>,
}

#[derive(Deserialize)]
struct Language {
    language_code: String,
}

#[derive(Deserialize)]
struct SignedIn {
    token: String,
    #[serde(default)]
    base_url: Option<String>,
}

#[derive(Deserialize)]
struct Subtitles {
    #[serde(default)]
    data: Vec<Subtitle>,
}

#[derive(Deserialize)]
struct Subtitle {
    #[serde(default, rename = "type")]
    kind: String,
    attributes: Attributes,
}

#[derive(Deserialize)]
struct Attributes {
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    release: Option<String>,
    #[serde(default)]
    download_count: Option<u64>,
    #[serde(default)]
    fps: Option<f64>,
    #[serde(default)]
    upload_date: Option<String>,
    #[serde(default)]
    moviehash_match: Option<bool>,
    #[serde(default)]
    hearing_impaired: Option<bool>,
    #[serde(default)]
    machine_translated: Option<bool>,
    #[serde(default)]
    ai_translated: Option<bool>,
    #[serde(default)]
    feature_details: Option<Feature>,
    #[serde(default)]
    files: Vec<File>,
}

#[derive(Deserialize)]
struct Feature {
    #[serde(default)]
    year: Option<i32>,
}

#[derive(Deserialize)]
struct File {
    file_id: u64,
    #[serde(default)]
    file_name: Option<String>,
}

#[derive(Deserialize)]
struct Link {
    link: String,
    #[serde(default)]
    file_name: Option<String>,
    #[serde(default)]
    remaining: Option<i64>,
}

/// An error body: OpenSubtitles writes `message`, sometimes `errors`, and
/// for an exhausted quota `reset_time_utc`.
#[derive(Deserialize, Default)]
struct Problem {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    errors: Option<Vec<String>>,
    #[serde(default)]
    reset_time_utc: Option<String>,
}

impl Problem {
    fn parse(body: &str) -> Self {
        serde_json::from_str(body).unwrap_or_default()
    }

    fn text(&self) -> String {
        self.message
            .clone()
            .or_else(|| self.errors.as_ref().map(|errors| errors.join("; ")))
            .unwrap_or_default()
    }
}

#[derive(Default)]
struct State {
    token: Option<(String, Instant)>,
    /// The server a sign-in said to use from then on.
    base: Option<String>,
    languages: Option<Vec<String>>,
    /// From the RateLimit headers: requests left, and when that renews.
    remaining: Option<u32>,
    renews: Option<Instant>,
}

pub struct Client {
    api: String,
    key: String,
    account: Option<Account>,
    agent: ureq::Agent,
    state: Mutex<State>,
}

struct Reply {
    status: u16,
    body: String,
}

impl Client {
    /// A client for one API key, signing in to `account` when it has one.
    pub fn new(key: &str, account: Option<Account>) -> Result<Self, Error> {
        let key = key.trim();
        if key.is_empty() {
            return Err(Error::NoApiKey);
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .user_agent(format!("Patina v{}", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Ok(Self {
            api: std::env::var("PATINA_OPENSUBTITLES_API")
                .unwrap_or_else(|_| API.to_owned())
                .trim_end_matches('/')
                .to_owned(),
            key: key.to_owned(),
            account,
            agent,
            state: Mutex::new(State::default()),
        })
    }

    pub fn username(&self) -> Option<&str> {
        self.account
            .as_ref()
            .map(|account| account.username.as_str())
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn url(&self, path: &str) -> String {
        let state = self.state();
        match &state.base {
            // A test server stays the server.
            Some(host) if self.api == API => format!("https://{host}/api/v1/{path}"),
            _ => format!("{}/{path}", self.api),
        }
    }

    /// Waits out a spent rate limit, as OpenSubtitles asks clients to.
    fn wait_for_rate_limit(&self) {
        let wait = {
            let state = self.state();
            match (state.remaining, state.renews) {
                (Some(0), Some(renews)) => renews.saturating_duration_since(Instant::now()),
                _ => Duration::ZERO,
            }
        };
        if !wait.is_zero() {
            std::thread::sleep(wait.min(MAX_WAIT));
        }
    }

    fn note_rate_limit(&self, response: &ureq::http::Response<ureq::Body>) {
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u32>().ok())
        };
        if let (Some(remaining), Some(reset)) =
            (header("ratelimit-remaining"), header("ratelimit-reset"))
        {
            let mut state = self.state();
            state.remaining = Some(remaining);
            state.renews = Some(Instant::now() + Duration::from_secs(reset.into()));
        }
    }

    /// One request to the API; `body` makes it a POST.
    fn request(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, String)],
        body: Option<String>,
    ) -> Result<Reply, Error> {
        for attempt in 0..2 {
            self.wait_for_rate_limit();
            let url = self.url(path);
            let token = self.state().token.as_ref().map(|(token, _)| token.clone());
            let result = match (method, body.clone()) {
                ("POST", Some(body)) => {
                    let mut request = self
                        .agent
                        .post(&url)
                        .header("Api-Key", &self.key)
                        .header("Accept", "application/json");
                    if let Some(token) = &token {
                        request = request.header("Authorization", format!("Bearer {token}"));
                    }
                    request.content_type("application/json").send(body)
                }
                ("DELETE", _) => {
                    let mut request = self.agent.delete(&url).header("Api-Key", &self.key);
                    if let Some(token) = &token {
                        request = request.header("Authorization", format!("Bearer {token}"));
                    }
                    request.call()
                }
                _ => {
                    let mut request = self
                        .agent
                        .get(&url)
                        .header("Api-Key", &self.key)
                        .header("Accept", "application/json");
                    if let Some(token) = &token {
                        request = request.header("Authorization", format!("Bearer {token}"));
                    }
                    for (name, value) in query {
                        request = request.query_raw(*name, encode(value));
                    }
                    request.call()
                }
            };
            let mut response = result.map_err(|error| Error::Network(error.to_string()))?;
            self.note_rate_limit(&response);
            let status = response.status().as_u16();
            let body = response
                .body_mut()
                .read_to_string()
                .map_err(|error| Error::Network(error.to_string()))?;
            if status == 429 && attempt == 0 {
                let mut state = self.state();
                state.remaining = Some(0);
                state
                    .renews
                    .get_or_insert(Instant::now() + Duration::from_secs(1));
                continue;
            }
            return Ok(Reply { status, body });
        }
        Err(Error::RateLimited)
    }

    /// An error for a reply that wasn't a success.
    fn failure(reply: &Reply) -> Error {
        let problem = Problem::parse(&reply.body);
        match reply.status {
            401 | 403 => Error::BadApiKey,
            429 => Error::RateLimited,
            404 => Error::NotFound,
            status => Error::Server(status, problem.text()),
        }
    }

    fn parse<T: for<'de> Deserialize<'de>>(reply: &Reply) -> Result<T, Error> {
        serde_json::from_str(&reply.body)
            .map_err(|error| Error::Server(reply.status, format!("unexpected answer ({error})")))
    }

    /// Signs in when there is an account and no current sign-in.
    fn sign_in(&self) -> Result<(), Error> {
        let Some(account) = &self.account else {
            return Ok(());
        };
        if self
            .state()
            .token
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() < TOKEN_LIFETIME)
        {
            return Ok(());
        }
        {
            let mut state = self.state();
            state.token = None;
            state.base = None;
        }
        let body = serde_json::json!({
            "username": account.username,
            "password": account.password,
        })
        .to_string();
        let reply = self.request("POST", "login", &[], Some(body))?;
        match reply.status {
            200 => {
                let signed_in: SignedIn = Self::parse(&reply)?;
                let mut state = self.state();
                state.token = Some((signed_in.token, Instant::now()));
                state.base = signed_in.base_url.filter(|host| !host.trim().is_empty());
                Ok(())
            }
            400 | 401 => {
                let reason = Problem::parse(&reply.body).text();
                Err(Error::SignIn(if reason.is_empty() {
                    "the username or password is wrong".to_owned()
                } else {
                    reason
                }))
            }
            _ => Err(Self::failure(&reply)),
        }
    }

    /// Asks after signing in, and once more after signing in again when
    /// OpenSubtitles says the sign-in has run out ("invalid token").
    fn signed_in(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, String)],
        body: Option<String>,
    ) -> Result<Reply, Error> {
        self.sign_in()?;
        let reply = self.request(method, path, query, body.clone())?;
        let expired = reply.status == 406
            && self.account.is_some()
            && Problem::parse(&reply.body)
                .text()
                .to_ascii_lowercase()
                .contains("token");
        if !expired {
            return Ok(reply);
        }
        self.state().token = None;
        self.sign_in()?;
        self.request(method, path, query, body)
    }

    /// The language codes OpenSubtitles knows, asked for once.
    fn supported_languages(&self) -> Result<Vec<String>, Error> {
        if let Some(languages) = self.state().languages.clone() {
            return Ok(languages);
        }
        let reply = self.request("GET", "infos/languages", &[], None)?;
        if reply.status != 200 {
            return Err(Self::failure(&reply));
        }
        let languages: Languages = Self::parse(&reply)?;
        let codes: Vec<String> = languages
            .data
            .into_iter()
            .map(|language| language.language_code.to_ascii_lowercase())
            .collect();
        self.state().languages = Some(codes.clone());
        Ok(codes)
    }

    /// Subtitles for `query`, the ones made for the file first.
    pub fn search(&self, query: &Query) -> Result<Vec<Found>, Error> {
        let supported = self.supported_languages()?;
        let mut languages = languages_for(&query.languages, &supported);
        if languages.is_empty() {
            languages.push("en".to_owned());
        }
        languages.sort();
        languages.dedup();
        // OpenSubtitles asks for the parameters in alphabetical order.
        let mut parameters = vec![("languages", languages.join(","))];
        if let Some(hash) = &query.hash {
            parameters.push(("moviehash", hash.clone()));
        }
        if !query.text.trim().is_empty() {
            parameters.push(("query", query.text.trim().to_lowercase()));
        }
        let reply = self.signed_in("GET", "subtitles", &parameters, None)?;
        if reply.status != 200 {
            return Err(Self::failure(&reply));
        }
        let subtitles: Subtitles = Self::parse(&reply)?;
        let mut found: Vec<Found> = subtitles
            .data
            .into_iter()
            .filter(|subtitle| subtitle.kind.is_empty() || subtitle.kind == "subtitle")
            .filter_map(|subtitle| {
                let attributes = subtitle.attributes;
                let file = attributes.files.into_iter().next()?;
                Some(Found {
                    file_id: file.file_id,
                    file_name: file
                        .file_name
                        .or_else(|| attributes.release.clone())
                        .unwrap_or_else(|| file.file_id.to_string()),
                    language: attributes.language.unwrap_or_default(),
                    release: attributes.release.unwrap_or_default(),
                    year: attributes
                        .feature_details
                        .and_then(|feature| feature.year)
                        .filter(|year| *year > 0),
                    fps: attributes.fps.filter(|fps| *fps > 0.0),
                    downloads: attributes.download_count.unwrap_or_default(),
                    uploaded: attributes
                        .upload_date
                        .map(|date| date.chars().take(10).collect())
                        .unwrap_or_default(),
                    exact: attributes.moviehash_match.unwrap_or(false),
                    hearing_impaired: attributes.hearing_impaired.unwrap_or(false),
                    machine_translated: attributes.machine_translated.unwrap_or(false)
                        || attributes.ai_translated.unwrap_or(false),
                })
            })
            .collect();
        // Stable: OpenSubtitles' own order otherwise.
        found.sort_by_key(|found| !found.exact);
        if found.is_empty() {
            return Err(Error::NotFound);
        }
        Ok(found)
    }

    /// Downloads one subtitle file: OpenSubtitles hands out a link, which
    /// is where the file is.
    pub fn download(&self, file_id: u64) -> Result<Downloaded, Error> {
        let body = serde_json::json!({ "file_id": file_id }).to_string();
        let reply = self.signed_in("POST", "download", &[], Some(body))?;
        if reply.status == 406 {
            return Err(Error::Quota(
                Problem::parse(&reply.body)
                    .reset_time_utc
                    .map(|time| local_time(&time)),
            ));
        }
        if reply.status != 200 {
            return Err(Self::failure(&reply));
        }
        let link: Link = Self::parse(&reply)?;
        let mut response = self
            .agent
            .get(&link.link)
            .call()
            .map_err(|error| Error::Network(error.to_string()))?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(Error::Server(
                status,
                "the subtitle file could not be fetched".into(),
            ));
        }
        let contents = response
            .body_mut()
            .with_config()
            .limit(32 * 1024 * 1024)
            .read_to_vec()
            .map_err(|error| Error::Network(error.to_string()))?;
        if contents.is_empty() {
            return Err(Error::Server(200, "the subtitle file is empty".into()));
        }
        Ok(Downloaded {
            file_name: link
                .file_name
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| format!("{file_id}.srt")),
            contents,
            remaining: link.remaining,
        })
    }

    /// Ends the sign-in, as OpenSubtitles asks clients to.
    pub fn sign_out(&self) {
        if self.state().token.is_none() {
            return;
        }
        if let Err(error) = self.request("DELETE", "logout", &[], None) {
            log::debug!("signing out of OpenSubtitles: {error}");
        }
        self.state().token = None;
    }
}

/// A query value the way OpenSubtitles asks for it, to avoid a redirect:
/// spaces as "+", and the list separator "," left alone.
fn encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b',' => {
                encoded.push(byte as char)
            }
            b' ' => encoded.push('+'),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

static SHARED: Mutex<Option<Arc<Client>>> = Mutex::new(None);

/// The client for this key and account, shared by every window so that
/// signing in happens once; made again when either changed. Blocks: the
/// account's password comes from the system's store.
pub fn shared(key: &str, username: &str) -> Result<Arc<Client>, Error> {
    let username = username.trim();
    let mut shared = SHARED.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(client) = shared.as_ref()
        && client.key == key.trim()
        && client.username().unwrap_or_default() == username
    {
        return Ok(client.clone());
    }
    let account = if username.is_empty() {
        None
    } else {
        match crate::credentials::get(PASSWORD_SERVICE, username) {
            Ok(Some(password)) => Some(Account {
                username: username.to_owned(),
                password,
            }),
            // Searching and a few downloads a day need no account.
            Ok(None) => None,
            Err(error) => {
                log::warn!("could not read the OpenSubtitles password: {error}");
                None
            }
        }
    };
    let client = Arc::new(Client::new(key, account)?);
    *shared = Some(client.clone());
    Ok(client)
}

/// Drops the shared client, for when the saved password changed.
pub fn forget_shared() {
    SHARED
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take();
}

/// Signs the shared client out, as OpenSubtitles asks clients to when
/// they are done; for quitting.
pub fn sign_out_shared() {
    let client = SHARED
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take();
    if let Some(client) = client {
        client.sign_out();
    }
}

/// "2026-10-27T13:05:02.000Z" as the hour and minute it is here, which
/// is what someone waiting for their quota wants to know.
fn local_time(utc: &str) -> String {
    match utc.parse::<jiff::Timestamp>() {
        Ok(time) => time
            .to_zoned(jiff::tz::TimeZone::system())
            .strftime("%H:%M")
            .to_string(),
        Err(_) => utc.to_owned(),
    }
}

/// The preferred languages as OpenSubtitles codes, of the ones it knows:
/// "ger" is "de"; a bare "pt" or "zh" means each kind it has.
pub fn languages_for(preferred: &[String], supported: &[String]) -> Vec<String> {
    let mut codes = Vec::new();
    for code in preferred {
        let code = code.trim().to_ascii_lowercase();
        let code = two_letter(&code).unwrap_or(&code).to_owned();
        if supported.contains(&code) {
            codes.push(code);
        } else {
            let prefix = format!("{code}-");
            codes.extend(
                supported
                    .iter()
                    .filter(|known| known.starts_with(&prefix))
                    .cloned(),
            );
        }
    }
    codes.dedup();
    codes
}

/// ISO 639-1 for an ISO 639-2 code, where OpenSubtitles has the language.
fn two_letter(code: &str) -> Option<&'static str> {
    Some(match code {
        "eng" => "en",
        "jpn" => "ja",
        "chi" | "zho" | "chs" => "zh-cn",
        "cht" => "zh-tw",
        "kor" => "ko",
        "fre" | "fra" => "fr",
        "ger" | "deu" => "de",
        "spa" => "es",
        "ita" => "it",
        "por" => "pt-pt",
        "rus" => "ru",
        "ara" => "ar",
        "hin" => "hi",
        "dut" | "nld" => "nl",
        "swe" => "sv",
        "nor" | "nob" => "no",
        "dan" => "da",
        "fin" => "fi",
        "pol" => "pl",
        "cze" | "ces" => "cs",
        "hun" => "hu",
        "gre" | "ell" => "el",
        "tur" => "tr",
        "heb" => "he",
        "tha" => "th",
        "vie" => "vi",
        "ind" => "id",
        "may" | "msa" => "ms",
        "ukr" => "uk",
        "rum" | "ron" => "ro",
        "bul" => "bg",
        "hrv" => "hr",
        "srp" => "sr",
        "slo" | "slk" => "sk",
        "slv" => "sl",
        "est" => "et",
        "lav" => "lv",
        "lit" => "lt",
        "per" | "fas" => "fa",
        "cat" => "ca",
        "baq" | "eus" => "eu",
        "glg" => "gl",
        "ice" | "isl" => "is",
        "ben" => "bn",
        "tam" => "ta",
        "tel" => "te",
        "mal" => "ml",
        "urd" => "ur",
        _ => return None,
    })
}

/// The OpenSubtitles hash of a local file: its size plus the sums, as
/// 64-bit little-endian words, of its first and last 64 KiB. None for a
/// file too small to have one.
pub fn hash(path: &Path) -> std::io::Result<Option<String>> {
    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    if size < HASH_MIN_SIZE {
        return Ok(None);
    }
    let mut hash = size;
    let mut chunk = vec![0u8; HASH_CHUNK as usize];
    for offset in [0, size - HASH_CHUNK] {
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut chunk)?;
        for word in chunk.as_chunks::<8>().0 {
            hash = hash.wrapping_add(u64::from_le_bytes(*word));
        }
    }
    Ok(Some(format!("{hash:016x}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    #[test]
    fn hashes_add_the_size_and_both_ends() {
        let dir = std::env::temp_dir().join(format!("patina-opensub-hash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 64 KiB of words 1, 64 KiB of zeros, 64 KiB of words 2: the
        // middle is never read.
        let path = dir.join("video.mkv");
        let mut bytes = Vec::new();
        for word in [1u64, 0, 2] {
            for _ in 0..HASH_CHUNK / 8 {
                bytes.extend_from_slice(&word.to_le_bytes());
            }
        }
        std::fs::write(&path, &bytes).unwrap();
        let size = bytes.len() as u64;
        let expected = size + HASH_CHUNK / 8 + 2 * (HASH_CHUNK / 8);
        assert_eq!(hash(&path).unwrap(), Some(format!("{expected:016x}")));
        std::fs::write(&path, [0u8; 1000]).unwrap();
        assert_eq!(hash(&path).unwrap(), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn preferred_languages_become_opensubtitles_codes() {
        let supported: Vec<String> = ["en", "de", "pt-pt", "pt-br", "zh-cn", "zh-tw"]
            .iter()
            .map(|code| code.to_string())
            .collect();
        let preferred: Vec<String> = ["ger", "pt", "eng", "xx", "zh-TW"]
            .iter()
            .map(|code| code.to_string())
            .collect();
        assert_eq!(
            languages_for(&preferred, &supported),
            ["de", "pt-pt", "pt-br", "en", "zh-tw"]
        );
    }

    /// A stand-in for the API: answers each request with the next of the
    /// replies (status and body) made for its address, and reports the
    /// requests it got.
    fn serve(
        replies: impl FnOnce(&str) -> Vec<(u16, String)>,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let replies = replies(&address);
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for (status, body) in replies {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap_or(0);
                    }
                    request.push_str(&line);
                }
                let mut payload = vec![0; length];
                reader.read_exact(&mut payload).unwrap();
                request.push_str(&String::from_utf8_lossy(&payload));
                let _ = sender.send(request);
                let mut stream = stream;
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        (address, receiver)
    }

    fn client(api: &str, account: Option<Account>) -> Client {
        let mut client = Client::new("test-key", account).unwrap();
        client.api = api.to_owned();
        client
    }

    const LANGUAGES: &str = r#"{"data": [{"language_code": "en", "language_name": "English"}, {"language_code": "de", "language_name": "German"}]}"#;

    #[test]
    fn searches_send_the_key_and_list_exact_matches_first() {
        let results = r#"{"total_count": 2, "data": [
            {"id": "1", "type": "subtitle", "attributes": {"language": "en", "download_count": 120,
             "release": "Show.S01E01.720p", "upload_date": "2024-05-17T10:00:00Z", "fps": 23.976,
             "feature_details": {"year": 2024, "title": "Show"},
             "files": [{"file_id": 11, "file_name": "show.s01e01.srt"}]}},
            {"id": "2", "type": "subtitle", "attributes": {"language": "de", "download_count": 3,
             "moviehash_match": true, "hearing_impaired": true,
             "files": [{"file_id": 22, "file_name": "show.de.srt"}]}}
        ]}"#;
        let (api, requests) = serve(|_| vec![(200, LANGUAGES.into()), (200, results.into())]);
        let found = client(&api, None)
            .search(&Query {
                languages: vec!["ger".into(), "eng".into()],
                hash: Some("0123456789abcdef".into()),
                text: "Show S01E01".into(),
            })
            .unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].file_id, 22);
        assert!(found[0].exact && found[0].hearing_impaired);
        assert_eq!(found[1].file_name, "show.s01e01.srt");
        assert_eq!(found[1].year, Some(2024));
        assert_eq!(found[1].uploaded, "2024-05-17");
        let first = requests.recv().unwrap();
        assert!(first.starts_with("GET /infos/languages"), "{first}");
        assert!(first.to_ascii_lowercase().contains("api-key: test-key"));
        let search = requests.recv().unwrap();
        assert!(
            search.starts_with(
                "GET /subtitles?languages=de,en&moviehash=0123456789abcdef&query=show+s01e01"
            ),
            "{search}"
        );
    }

    #[test]
    fn downloads_follow_the_link_after_signing_in() {
        let (api, requests) = serve(|address| {
            vec![
                (
                    200,
                    r#"{"token": "jwt", "status": 200, "user": {"allowed_downloads": 20}}"#.into(),
                ),
                (
                    200,
                    format!(
                        r#"{{"link": "{address}/file/11", "file_name": "show.srt", "remaining": 19}}"#
                    ),
                ),
                (200, "1\n00:00:01,000 --> 00:00:02,000\nHello\n".into()),
            ]
        });
        let account = Account {
            username: "someone".into(),
            password: "secret".into(),
        };
        let downloaded = client(&api, Some(account))
            .download(11)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(downloaded.file_name, "show.srt");
        assert_eq!(downloaded.remaining, Some(19));
        assert!(String::from_utf8_lossy(&downloaded.contents).contains("Hello"));
        let login = requests.recv().unwrap();
        assert!(login.starts_with("POST /login"), "{login}");
        assert!(login.contains(r#""username":"someone""#), "{login}");
        let download = requests.recv().unwrap();
        assert!(download.starts_with("POST /download"), "{download}");
        assert!(
            download
                .to_ascii_lowercase()
                .contains("authorization: bearer jwt"),
            "{download}"
        );
        assert!(download.contains(r#""file_id":11"#), "{download}");
        let file = requests.recv().unwrap();
        assert!(file.starts_with("GET /file/11"), "{file}");
    }

    #[test]
    fn spent_quotas_and_bad_keys_say_so() {
        let (api, _requests) = serve(|_| {
            vec![
                (406, r#"{"message": "You have downloaded your allowed 5 subtitles", "reset_time_utc": "not a time"}"#.into()),
                (401, r#"{"message": "Error, invalid api key"}"#.into()),
            ]
        });
        let client = client(&api, None);
        assert_eq!(
            client.download(1),
            Err(Error::Quota(Some("not a time".into())))
        );
        assert_eq!(client.download(1), Err(Error::BadApiKey));
        assert!(matches!(Client::new("  ", None), Err(Error::NoApiKey)));
    }
}
