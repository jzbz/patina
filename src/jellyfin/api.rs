//! A blocking client for one Jellyfin server and, once signed in, one
//! account on it: the requests docs/jellyfin.md lists, typed. Every call
//! blocks, so the windows make them on worker threads; a `Client` is cheap
//! to clone and its clones share their connections.
//!
//! Requests follow redirects only on the same server: the same scheme,
//! host and port, the rule `auth::same_server` gives for the token. Another
//! port on the same host may be another program, and ureq would follow a
//! redirect elsewhere without the `Authorization` header, so that the other
//! end's 401 would read as the token being gone. `probe`, which adds a
//! server and sends no token, is the one call that follows a redirect
//! anywhere, to find the address the server is really at.

use super::auth::{self, Identity};
use super::models::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::path::Path;
use std::time::Duration;
use url::Url;

const TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REDIRECTS: usize = 5;
/// A page of items with every field can run to megabytes.
const JSON_LIMIT: u64 = 64 * 1024 * 1024;
const BYTES_LIMIT: u64 = 32 * 1024 * 1024;
/// How long a download may wait for its next bytes before it gives up. A
/// download has no limit on the whole, which a film over a slow line
/// would pass, so this is what ends one from a server that has stopped
/// sending.
const STALL_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// What was typed isn't an address; the reason is a sentence.
    Address(String),
    Network(String),
    /// The secure connection failed, mostly over the certificate.
    Tls(String),
    /// What answered at an https address speaks plain http.
    NotHttps,
    /// The token is gone: signed out elsewhere, the password changed or
    /// the device removed. Patina forgets it and asks to sign in again.
    Unauthorized,
    Forbidden,
    NotFound,
    /// Signing in was refused, with the reason.
    SignIn(String),
    /// Something answers at the address, but not Jellyfin.
    NotJellyfin,
    /// A Jellyfin older than Patina supports, with its version.
    Unsupported(String),
    /// The server sent the request on to another host, given here.
    Moved(String),
    /// The server won't hand out the file: the account may not download
    /// (403), or the item isn't a file of the server's own (400); or it
    /// hands it out so that its end can't be told from a lost connection.
    /// The reason is a sentence.
    NoDownload(&'static str),
    /// Anything else the server answered with: status and message.
    Server(u16, String),
    /// An answer that wasn't what the server should send.
    Decode(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Address(reason) => write!(f, "{reason}"),
            Error::Network(error) => write!(f, "Could not reach the server: {error}"),
            Error::Tls(error) => write!(
                f,
                "Could not make a secure connection to the server: {error}"
            ),
            Error::NotHttps => write!(
                f,
                "This server doesn't use https. Enter its address with http:// in front."
            ),
            Error::Unauthorized => write!(
                f,
                "The server no longer accepts this sign-in. Sign in again."
            ),
            Error::Forbidden => write!(f, "The server doesn't allow this account to do that."),
            Error::NotFound => write!(f, "The server doesn't have that, or no longer has it."),
            Error::SignIn(reason) => write!(f, "Could not sign in: {reason}"),
            Error::NotJellyfin => write!(f, "There is no Jellyfin server at this address."),
            Error::Unsupported(version) => write!(
                f,
                "This server runs Jellyfin {version}; Patina needs {}.{}.{} or newer.",
                MINIMUM_VERSION.0, MINIMUM_VERSION.1, MINIMUM_VERSION.2
            ),
            Error::Moved(address) => write!(
                f,
                "The server sends Patina on to {address}. Add the server again at that address."
            ),
            Error::NoDownload(reason) => write!(f, "{reason}"),
            Error::Server(503, _) => write!(
                f,
                "The server is starting up or busy; try again in a moment."
            ),
            Error::Server(status, message) if message.is_empty() => {
                write!(f, "The server answered with an error ({status}).")
            }
            Error::Server(status, message) => {
                write!(f, "The server answered with an error ({status}): {message}")
            }
            Error::Decode(error) => write!(f, "The server's answer could not be read: {error}"),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    /// The message, naming the server where it says "the server": "Could
    /// not reach Patina Test Server: …".
    pub fn about(&self, server: Option<&str>) -> String {
        let text = self.to_string();
        match (self, server.filter(|name| !name.is_empty())) {
            (Error::Network(_) | Error::Tls(_), Some(name)) => text.replacen("the server", name, 1),
            _ => text,
        }
    }
}

fn network(error: ureq::Error) -> Error {
    use std::io::ErrorKind;
    match error {
        ureq::Error::Tls(message) => Error::Tls(message.to_owned()),
        ureq::Error::Rustls(error) => Error::Tls(error.to_string()),
        ureq::Error::Pem(error) => Error::Tls(error.to_string()),
        // rustls reports a failed handshake, a refused certificate among
        // them, as invalid data wrapping its own error. A reply that isn't
        // TLS at all is a server speaking plain http, which is said in
        // words rather than in rustls's.
        ureq::Error::Io(error) if error.kind() == ErrorKind::InvalidData => match error.get_ref() {
            Some(inner) if inner.to_string().contains("InvalidContentType") => {
                log::debug!("not TLS: {inner}");
                Error::NotHttps
            }
            Some(inner) => Error::Tls(inner.to_string()),
            None => Error::Network(error.to_string()),
        },
        ureq::Error::Io(error) if error.kind() == ErrorKind::ConnectionRefused => {
            Error::Network("nothing answers at that address and port.".into())
        }
        ureq::Error::Io(error)
            if matches!(
                error.kind(),
                ErrorKind::UnexpectedEof
                    | ErrorKind::ConnectionReset
                    | ErrorKind::ConnectionAborted
                    | ErrorKind::BrokenPipe
            ) =>
        {
            log::debug!("connection lost: {error}");
            Error::Network("the connection was lost.".into())
        }
        ureq::Error::Timeout(_) => Error::Network("it did not answer in time.".into()),
        ureq::Error::HostNotFound => Error::Network("no computer by that name was found.".into()),
        // The server did answer, at more length than Patina reads.
        ureq::Error::BodyExceedsLimit(limit) => Error::Decode(format!(
            "it is longer than the {} MB Patina reads",
            limit >> 20
        )),
        other => Error::Network(other.to_string()),
    }
}

/// What an error answer says, when it says something readable: the
/// server writes plain text ("Error processing request." unless it runs
/// in development mode), a JSON string, or a problem object.
fn message(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let text = text.trim();
    if text.starts_with('<') {
        return String::new();
    }
    let text = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::String(message)) => message,
        Ok(serde_json::Value::Object(problem)) => ["detail", "title", "Message", "message"]
            .iter()
            .find_map(|key| problem.get(*key).and_then(|value| value.as_str()))
            .unwrap_or_default()
            .to_owned(),
        _ => text.to_owned(),
    };
    let text = text.trim();
    if text == "Error processing request." {
        return String::new();
    }
    text.chars().take(200).collect()
}

/// A query value: all but the unreserved characters percent-encoded, and
/// the comma that separates list items left as it is.
fn query_value(value: &str) -> String {
    auth::encode(value).replace("%2C", ",")
}

/// A path segment from an id the server gave.
fn segment(value: &str) -> String {
    auth::encode(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Ascending,
    Descending,
}

impl SortOrder {
    pub fn as_str(self) -> &'static str {
        match self {
            SortOrder::Ascending => "Ascending",
            SortOrder::Descending => "Descending",
        }
    }
}

/// The parameters of the item list requests (`/Items`, `/UserItems/Resume`,
/// `/Shows/NextUp`, `/Items/Latest`, `/Artists`, `/Genres`...), of which
/// each takes the ones it knows. Only what is set is sent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ItemsQuery {
    pub parent_id: Option<String>,
    pub include_item_types: Vec<ItemKind>,
    /// "Video", "Audio".
    pub media_types: Vec<String>,
    pub recursive: Option<bool>,
    /// "SortName", "DateCreated", "PremiereDate", "ParentIndexNumber"...
    pub sort_by: Vec<String>,
    pub sort_order: Option<SortOrder>,
    /// "IsFavorite", "IsUnplayed", "IsResumable"...
    pub filters: Vec<String>,
    /// Fields beyond the ones every item has: "Overview", "Genres",
    /// "PrimaryImageAspectRatio", "MediaSources"...
    pub fields: Vec<String>,
    /// The image types to give tags for; the server leaves out the rest.
    pub enable_image_types: Vec<ImageType>,
    pub image_type_limit: Option<u32>,
    pub start_index: Option<u32>,
    pub limit: Option<u32>,
    pub search_term: Option<String>,
    pub genre_ids: Vec<String>,
    pub artist_ids: Vec<String>,
    pub album_artist_ids: Vec<String>,
    pub person_ids: Vec<String>,
    pub ids: Vec<String>,
    pub is_favorite: Option<bool>,
    pub is_played: Option<bool>,
    /// For `/Shows/{id}/Episodes`.
    pub season_id: Option<String>,
    pub enable_user_data: Option<bool>,
    /// Off saves the server counting every match.
    pub enable_total_record_count: Option<bool>,
    /// Anything else, as name and value.
    pub extra: Vec<(String, String)>,
}

impl ItemsQuery {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn parent(mut self, id: &str) -> Self {
        self.parent_id = Some(id.to_owned());
        self
    }

    pub fn types(mut self, kinds: &[ItemKind]) -> Self {
        self.include_item_types = kinds.to_vec();
        self
    }

    pub fn media_types(mut self, kinds: &[&str]) -> Self {
        self.media_types = kinds.iter().map(|kind| kind.to_string()).collect();
        self
    }

    /// Searches below the parent, not only in it. A query that names the
    /// item types wants this: an untyped one is slow on a large library.
    pub fn recursive(mut self) -> Self {
        self.recursive = Some(true);
        self
    }

    pub fn sort(mut self, by: &[&str], order: SortOrder) -> Self {
        self.sort_by = by.iter().map(|by| by.to_string()).collect();
        self.sort_order = Some(order);
        self
    }

    pub fn filters(mut self, filters: &[&str]) -> Self {
        self.filters = filters.iter().map(|filter| filter.to_string()).collect();
        self
    }

    pub fn fields(mut self, fields: &[&str]) -> Self {
        self.fields = fields.iter().map(|field| field.to_string()).collect();
        self
    }

    /// The image types the page will show, and how many of each (one
    /// backdrop rather than all of them).
    pub fn images(mut self, kinds: &[ImageType], limit: u32) -> Self {
        self.enable_image_types = kinds.to_vec();
        self.image_type_limit = Some(limit);
        self
    }

    pub fn page(mut self, start: u32, limit: u32) -> Self {
        self.start_index = Some(start);
        self.limit = Some(limit);
        self
    }

    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn search(mut self, term: &str) -> Self {
        self.search_term = Some(term.to_owned());
        self
    }

    pub fn genres(mut self, ids: &[&str]) -> Self {
        self.genre_ids = ids.iter().map(|id| id.to_string()).collect();
        self
    }

    pub fn artists(mut self, ids: &[&str]) -> Self {
        self.artist_ids = ids.iter().map(|id| id.to_string()).collect();
        self
    }

    pub fn album_artists(mut self, ids: &[&str]) -> Self {
        self.album_artist_ids = ids.iter().map(|id| id.to_string()).collect();
        self
    }

    // Only the tests list items by id, or by their marks.
    #[cfg(test)]
    pub fn ids(mut self, ids: &[&str]) -> Self {
        self.ids = ids.iter().map(|id| id.to_string()).collect();
        self
    }

    #[cfg(test)]
    pub fn favorite(mut self, favorite: bool) -> Self {
        self.is_favorite = Some(favorite);
        self
    }

    #[cfg(test)]
    pub fn played(mut self, played: bool) -> Self {
        self.is_played = Some(played);
        self
    }

    pub fn season(mut self, id: &str) -> Self {
        self.season_id = Some(id.to_owned());
        self
    }

    pub fn param(mut self, name: &str, value: &str) -> Self {
        self.extra.push((name.to_owned(), value.to_owned()));
        self
    }

    /// The query string's names and values, unencoded, in a fixed order.
    pub fn pairs(&self) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = Vec::new();
        let mut text = |name: &str, value: Option<&str>| {
            if let Some(value) = value {
                pairs.push((name.to_owned(), value.to_owned()));
            }
        };
        let join = |values: &[String]| (!values.is_empty()).then(|| values.join(","));
        let kinds = |kinds: &[ItemKind]| {
            (!kinds.is_empty()).then(|| {
                kinds
                    .iter()
                    .map(|kind| kind.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            })
        };
        let flag = |value: Option<bool>| value.map(|value| value.to_string());
        let number = |value: Option<u32>| value.map(|value| value.to_string());
        text("parentId", self.parent_id.as_deref());
        text("seasonId", self.season_id.as_deref());
        text(
            "includeItemTypes",
            kinds(&self.include_item_types).as_deref(),
        );
        text("mediaTypes", join(&self.media_types).as_deref());
        text("recursive", flag(self.recursive).as_deref());
        text("sortBy", join(&self.sort_by).as_deref());
        text("sortOrder", self.sort_order.map(SortOrder::as_str));
        text("filters", join(&self.filters).as_deref());
        text("fields", join(&self.fields).as_deref());
        let image_types: Vec<String> = self
            .enable_image_types
            .iter()
            .map(|kind| kind.as_str().to_owned())
            .collect();
        text("enableImageTypes", join(&image_types).as_deref());
        text("imageTypeLimit", number(self.image_type_limit).as_deref());
        text("startIndex", number(self.start_index).as_deref());
        text("limit", number(self.limit).as_deref());
        text("searchTerm", self.search_term.as_deref());
        text("genreIds", join(&self.genre_ids).as_deref());
        text("artistIds", join(&self.artist_ids).as_deref());
        text("albumArtistIds", join(&self.album_artist_ids).as_deref());
        text("personIds", join(&self.person_ids).as_deref());
        text("ids", join(&self.ids).as_deref());
        text("isFavorite", flag(self.is_favorite).as_deref());
        text("isPlayed", flag(self.is_played).as_deref());
        text("enableUserData", flag(self.enable_user_data).as_deref());
        text(
            "enableTotalRecordCount",
            flag(self.enable_total_record_count).as_deref(),
        );
        for (name, value) in &self.extra {
            text(name, Some(value));
        }
        pairs
    }
}

/// A file being downloaded: its bytes as they come, and what the server
/// said about them.
pub struct Download {
    /// The `Content-Disposition` header, which names the file.
    pub disposition: Option<String>,
    /// The size, when the server gives it (`Content-Length`).
    pub length: Option<u64>,
    reader: ureq::BodyReader<'static>,
}

impl Download {
    /// Reads the next bytes; 0 at the end. It fails, rather than ending
    /// early, when the connection is lost before the size the server gave
    /// or before the last chunk, and when the server sends nothing for a
    /// minute (`STALL_TIMEOUT`), with the error in the words the other
    /// requests' are in.
    pub fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Error> {
        std::io::Read::read(&mut self.reader, buffer).map_err(|error| network(error.into()))
    }
}

/// What adding a server found: the address it ended up at, after any
/// redirects, and what the server says about itself.
#[derive(Debug, Clone, PartialEq)]
pub struct Probed {
    pub address: Url,
    pub info: PublicSystemInfo,
}

/// Which redirects a request follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Follow {
    /// On the same server (scheme, host and port), with the header;
    /// anything else, an upgrade to https among it, is `Moved`.
    SameServer,
    /// Anywhere but from https to http, with the header only while on
    /// this client's server.
    Anywhere,
}

struct Reply {
    status: u16,
    body: Vec<u8>,
    /// Where the answer came from, after redirects.
    url: Url,
}

impl Reply {
    fn check(self) -> Result<Reply, Error> {
        match self.status {
            200..=299 => Ok(self),
            401 => Err(Error::Unauthorized),
            403 => Err(Error::Forbidden),
            404 => Err(Error::NotFound),
            status => Err(Error::Server(status, message(&self.body))),
        }
    }

    fn json<T: DeserializeOwned>(&self) -> Result<T, Error> {
        serde_json::from_slice(&self.body).map_err(|error| Error::Decode(error.to_string()))
    }
}

/// The client for one server, and for one account on it once
/// `with_session` gives it a token.
#[derive(Clone)]
pub struct Client {
    base: Url,
    identity: Identity,
    token: Option<String>,
    user_id: Option<String>,
    agent: ureq::Agent,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base.as_str())
            .field("user_id", &self.user_id)
            .field("token", &self.token.as_ref().map(|_| "(set)"))
            .finish()
    }
}

/// The certificates trusted for a server. Without a CA file, what the
/// system trusts. With one, that file's certificates and nothing else:
/// ureq 3.5 takes either the platform's verifier or a list of roots, not
/// both, and the Mozilla roots it could add aren't a dependency of
/// Patina's. That is enough, since requests go to that server only. The
/// file can hold the CA that signed the server's certificate, or the
/// server's own self-signed certificate, which certificate checking then
/// takes as its own root: trusted for that server alone, it has to be
/// given again when the server's certificate is replaced.
fn root_certs(ca_file: Option<&Path>) -> Result<ureq::tls::RootCerts, Error> {
    use ureq::tls::{Certificate, PemItem, RootCerts};
    let Some(path) = ca_file else {
        return Ok(RootCerts::PlatformVerifier);
    };
    let bytes = std::fs::read(path)
        .map_err(|error| Error::Tls(format!("could not read {}: {error}", path.display())))?;
    let mut certificates: Vec<Certificate<'static>> = ureq::tls::parse_pem(&bytes)
        .filter_map(|item| match item {
            Ok(PemItem::Certificate(certificate)) => Some(certificate),
            _ => None,
        })
        .collect();
    // A certificate on its own in DER, as some tools save one.
    if certificates.is_empty() && !bytes.is_empty() && !bytes.starts_with(b"-----") {
        certificates.push(Certificate::from_der(&bytes).to_owned());
    }
    if certificates.is_empty() {
        return Err(Error::Tls(format!(
            "{} holds no certificates.",
            path.display()
        )));
    }
    Ok(RootCerts::new_with_certs(&certificates))
}

impl Client {
    /// A client for the server at `base` (an address `normalize_address`
    /// made), not signed in, trusting `ca_file`'s certificates when the
    /// server has one.
    pub fn new(base: Url, identity: Identity, ca_file: Option<&Path>) -> Result<Self, Error> {
        let tls = ureq::tls::TlsConfig::builder()
            .root_certs(root_certs(ca_file)?)
            .build();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(TIMEOUT))
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .user_agent(format!("Patina/{}", env!("CARGO_PKG_VERSION")))
            // Followed here instead, to keep to the same server.
            .max_redirects(0)
            .tls_config(tls)
            .build()
            .into();
        Ok(Self {
            base,
            identity,
            token: None,
            user_id: None,
            agent,
        })
    }

    /// The same client, signed in as an account.
    pub fn with_session(mut self, token: &str, user_id: &str) -> Self {
        self.token = Some(token.to_owned());
        self.user_id = Some(user_id.to_owned());
        self
    }

    pub fn base(&self) -> &Url {
        &self.base
    }

    #[cfg(test)]
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    pub fn user_id(&self) -> Option<&str> {
        self.user_id.as_deref()
    }

    /// The `Authorization` header's value.
    pub fn authorization(&self) -> String {
        self.identity.header(self.token.as_deref())
    }

    /// mpv's `http-header-fields` for this server's streams: the
    /// `Authorization` header, as a list of one. The option is a string
    /// list, and mpv splits a string set on it at every comma, which the
    /// header is full of, so it must be set as a list (`mpv::Value::List`),
    /// and through a setter that leaves the value out of its errors.
    pub fn http_header_fields(&self) -> Vec<String> {
        vec![format!("Authorization: {}", self.authorization())]
    }

    fn url(&self, path: &str, query: &[(String, String)]) -> Url {
        let mut url = auth::api_url(&self.base, path);
        if !query.is_empty() {
            let encoded: Vec<String> = query
                .iter()
                .map(|(name, value)| format!("{}={}", query_value(name), query_value(value)))
                .collect();
            url.set_query(Some(&encoded.join("&")));
        }
        url
    }

    /// One request, following redirects as `follow` says. The header goes
    /// only to this client's server. `json` asks for JSON; otherwise the
    /// answer is bytes, an image.
    fn send(
        &self,
        method: &str,
        url: Url,
        body: Option<Vec<u8>>,
        follow: Follow,
        json: bool,
    ) -> Result<Reply, Error> {
        let (mut response, url) = self.exchange(method, url, body, follow, json, false)?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .with_config()
            .limit(if json { JSON_LIMIT } else { BYTES_LIMIT })
            .read_to_vec()
            .map_err(network)?;
        Ok(Reply { status, body, url })
    }

    /// Runs one request, with the timeouts of a download when `download`
    /// says: none on the whole, which a large file over a slow line would
    /// pass, but one on waiting for the answer and on each read.
    fn run<S: ureq::AsSendBody>(
        &self,
        request: ureq::http::Request<S>,
        download: bool,
    ) -> Result<ureq::http::Response<ureq::Body>, Error> {
        let request = if download {
            self.agent
                .configure_request(request)
                .timeout_global(None)
                .timeout_recv_response(Some(TIMEOUT))
                .timeout_per_read(Some(STALL_TIMEOUT))
                .build()
        } else {
            request
        };
        self.agent.run(request).map_err(network)
    }

    /// `send` up to the answer's head, after any redirects, and where it
    /// came from: the body is left for the caller to read, whole or as it
    /// comes.
    fn exchange(
        &self,
        method: &str,
        url: Url,
        body: Option<Vec<u8>>,
        follow: Follow,
        json: bool,
        download: bool,
    ) -> Result<(ureq::http::Response<ureq::Body>, Url), Error> {
        let mut method = method.to_owned();
        let mut url = url;
        let mut body = body;
        let mut authorized = auth::same_server(&self.base, &url);
        for _ in 0..=MAX_REDIRECTS {
            let mut request = ureq::http::Request::builder()
                .method(method.as_str())
                .uri(url.as_str());
            if authorized {
                request = request.header("Authorization", self.authorization());
            }
            if json {
                request = request.header("Accept", "application/json");
            }
            let result = match &body {
                Some(bytes) => request
                    .header("Content-Type", "application/json")
                    .body(bytes.clone())
                    .map_err(|error| Error::Network(error.to_string()))
                    .and_then(|request| self.run(request, download)),
                None => request
                    .body(())
                    .map_err(|error| Error::Network(error.to_string()))
                    .and_then(|request| self.run(request, download)),
            };
            let response = result?;
            let status = response.status().as_u16();
            let location = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            if let (301 | 302 | 303 | 307 | 308, Some(location)) = (status, location) {
                let next = url
                    .join(&location)
                    .map_err(|error| Error::Network(format!("a bad redirect ({error})")))?;
                if !auth::same_server(&url, &next) {
                    let shown = next.origin().ascii_serialization();
                    if follow == Follow::SameServer {
                        return Err(Error::Moved(shown));
                    }
                    if url.scheme() == "https" && next.scheme() == "http" {
                        return Err(Error::Address(format!(
                            "The server sends Patina on to {shown}, which isn't encrypted. \
                             Enter that address if it is the one meant."
                        )));
                    }
                }
                // Decided again at every hop, against this client's server.
                authorized = authorized && auth::same_server(&self.base, &next);
                // As browsers do: what is answered with 301, 302 or 303 is
                // asked again as a GET, without its body. 307 and 308 ask
                // for the same request again.
                if matches!(status, 301..=303) && method != "HEAD" {
                    method = "GET".into();
                    body = None;
                }
                url = next;
                continue;
            }
            return Ok((response, url));
        }
        Err(Error::Network(
            "the server redirected the request too many times.".into(),
        ))
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        query: &[(String, String)],
        body: Option<Vec<u8>>,
    ) -> Result<Reply, Error> {
        self.send(
            method,
            self.url(path, query),
            body,
            Follow::SameServer,
            true,
        )?
        .check()
    }

    fn get<T: DeserializeOwned>(&self, path: &str, query: &[(String, String)]) -> Result<T, Error> {
        self.request("GET", path, query, None)?.json()
    }

    fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(String, String)],
        body: &impl Serialize,
    ) -> Result<T, Error> {
        let body = serde_json::to_vec(body).map_err(|error| Error::Decode(error.to_string()))?;
        self.request("POST", path, query, Some(body))?.json()
    }

    /// A POST whose answer has nothing to read (204).
    fn post_only(
        &self,
        path: &str,
        query: &[(String, String)],
        body: Option<&impl Serialize>,
    ) -> Result<(), Error> {
        let body = match body {
            Some(body) => {
                serde_json::to_vec(body).map_err(|error| Error::Decode(error.to_string()))?
            }
            None => Vec::new(),
        };
        self.request("POST", path, query, Some(body)).map(|_| ())
    }

    fn query(query: &ItemsQuery) -> Vec<(String, String)> {
        query.pairs()
    }

    /// GET `/System/Info/Public` under `base`, following redirects
    /// anywhere, and where it ended.
    fn probe_at(&self, base: &Url) -> Result<Probed, (Error, Option<Url>)> {
        let url = auth::api_url(base, "/System/Info/Public");
        let reply = self
            .send("GET", url, None, Follow::Anywhere, true)
            .map_err(|error| (error, None))?;
        let ended = reply.url.clone();
        let reply = match reply.check() {
            Ok(reply) => reply,
            Err(Error::NotFound) => return Err((Error::NotJellyfin, Some(ended))),
            Err(error) => return Err((error, Some(ended))),
        };
        let info: PublicSystemInfo = reply
            .json()
            .map_err(|_| (Error::NotJellyfin, Some(ended.clone())))?;
        if !info.is_jellyfin() {
            return Err((Error::NotJellyfin, Some(ended)));
        }
        if !info.supported() {
            return Err((Error::Unsupported(info.version.clone()), Some(ended)));
        }
        let mut address = ended;
        address.set_query(None);
        address.set_fragment(None);
        let path = address.path().to_owned();
        let suffix = "/System/Info/Public";
        let base_path = match path.len().checked_sub(suffix.len()) {
            Some(at) if path.is_char_boundary(at) && path[at..].eq_ignore_ascii_case(suffix) => {
                path[..at].to_owned()
            }
            // Answered from somewhere unexpected: keep what was asked.
            _ => base.path().trim_end_matches('/').to_owned(),
        };
        address.set_path(&base_path);
        Ok(Probed { address, info })
    }

    // Connecting.

    /// The tests' check that a server answers; `probe` is the app's.
    #[cfg(test)]
    pub fn public_info(&self) -> Result<PublicSystemInfo, Error> {
        self.get("/System/Info/Public", &[])
    }

    /// The accounts the server shows on its sign-in screen; from outside
    /// the server's network, only those allowed in from there.
    pub fn public_users(&self) -> Result<Vec<UserDto>, Error> {
        self.get("/Users/Public", &[])
    }

    /// A sign-in's answer, which must hold a token; a 403 is the server's
    /// policy refusing the account. The server says which rule refused it
    /// only in development mode, so the sentence names them all, the
    /// commonest first: an account not allowed in from outside the
    /// server's network, which is where a client over the internet or
    /// through a proxy comes from.
    fn signed_in(
        result: Result<AuthenticationResult, Error>,
    ) -> Result<AuthenticationResult, Error> {
        let result = result.map_err(|error| match error {
            Error::Forbidden => Error::SignIn(
                "the server doesn't let this account sign in from here. It may not be \
                 allowed in from outside the server's network or on this device, or be \
                 disabled, at its limit of devices, or outside its allowed hours."
                    .into(),
            ),
            error => error,
        })?;
        if result.access_token.is_empty() || result.user.id.is_empty() {
            return Err(Error::Decode("the sign-in gave no token".into()));
        }
        Ok(result)
    }

    pub fn authenticate_by_name(
        &self,
        name: &str,
        password: &str,
    ) -> Result<AuthenticationResult, Error> {
        let body = serde_json::json!({ "Username": name, "Pw": password });
        Self::signed_in(
            self.post("/Users/AuthenticateByName", &[], &body)
                .map_err(|error| match error {
                    Error::Unauthorized => Error::SignIn("the name or password is wrong.".into()),
                    error => error,
                }),
        )
    }

    pub fn quick_connect_enabled(&self) -> Result<bool, Error> {
        self.get("/QuickConnect/Enabled", &[])
    }

    /// What the Quick Connect requests' errors mean: the server answers 401
    /// when Quick Connect is off and 404 for a request that ran out.
    fn quick_connect_error(error: Error) -> Error {
        match error {
            Error::Unauthorized => {
                Error::SignIn("Quick Connect is turned off on this server.".into())
            }
            Error::NotFound => {
                Error::SignIn("the Quick Connect code ran out. Ask for a new one.".into())
            }
            error => error,
        }
    }

    /// Starts a Quick Connect sign-in: the answer's code is for the user
    /// to enter in another Jellyfin app, its secret for asking after it.
    pub fn quick_connect_initiate(&self) -> Result<QuickConnectResult, Error> {
        self.request("POST", "/QuickConnect/Initiate", &[], Some(Vec::new()))
            .and_then(|reply| reply.json())
            .map_err(Self::quick_connect_error)
    }

    /// Whether the code has been approved yet; asked every five seconds.
    /// A request runs out after ten minutes.
    pub fn quick_connect_connect(&self, secret: &str) -> Result<QuickConnectResult, Error> {
        self.get(
            "/QuickConnect/Connect",
            &[("secret".to_owned(), secret.to_owned())],
        )
        .map_err(Self::quick_connect_error)
    }

    /// Signs in with an approved Quick Connect request.
    pub fn quick_connect_authenticate(&self, secret: &str) -> Result<AuthenticationResult, Error> {
        let body = serde_json::json!({ "Secret": secret });
        Self::signed_in(
            self.post("/Users/AuthenticateWithQuickConnect", &[], &body)
                .map_err(Self::quick_connect_error),
        )
    }

    /// Ends the sign-in: the server revokes the token.
    pub fn logout(&self) -> Result<(), Error> {
        self.post_only("/Sessions/Logout", &[], None::<&()>)
    }

    pub fn me(&self) -> Result<UserDto, Error> {
        self.get("/Users/Me", &[])
    }

    // Browsing.

    /// The account's libraries.
    pub fn views(&self) -> Result<QueryResult<BaseItemDto>, Error> {
        self.get("/UserViews", &[])
    }

    /// Continue Watching; `mediaTypes=Video` keeps music out of it.
    pub fn resume(&self, query: &ItemsQuery) -> Result<QueryResult<BaseItemDto>, Error> {
        self.get("/UserItems/Resume", &Self::query(query))
    }

    pub fn next_up(&self, query: &ItemsQuery) -> Result<QueryResult<BaseItemDto>, Error> {
        self.get("/Shows/NextUp", &Self::query(query))
    }

    /// The newest in a library (`parent`): a list, not a page.
    pub fn latest(&self, query: &ItemsQuery) -> Result<Vec<BaseItemDto>, Error> {
        self.get("/Items/Latest", &Self::query(query))
    }

    pub fn items(&self, query: &ItemsQuery) -> Result<QueryResult<BaseItemDto>, Error> {
        self.get("/Items", &Self::query(query))
    }

    /// One item with every field: media sources, chapters, people.
    pub fn item(&self, id: &str) -> Result<BaseItemDto, Error> {
        self.get(&format!("/Items/{}", segment(id)), &[])
    }

    pub fn seasons(
        &self,
        series_id: &str,
        query: &ItemsQuery,
    ) -> Result<QueryResult<BaseItemDto>, Error> {
        self.get(
            &format!("/Shows/{}/Seasons", segment(series_id)),
            &Self::query(query),
        )
    }

    /// A show's episodes, of one season with `season`.
    pub fn episodes(
        &self,
        series_id: &str,
        query: &ItemsQuery,
    ) -> Result<QueryResult<BaseItemDto>, Error> {
        self.get(
            &format!("/Shows/{}/Episodes", segment(series_id)),
            &Self::query(query),
        )
    }

    /// Items like this one, for an item page's More Like This.
    pub fn similar(&self, id: &str, query: &ItemsQuery) -> Result<QueryResult<BaseItemDto>, Error> {
        self.get(
            &format!("/Items/{}/Similar", segment(id)),
            &Self::query(query),
        )
    }

    /// A music library's artists. Deprecated in 12.0 for `/Persons`, which
    /// lists artists only after a rescan; jellyfin-web still uses this.
    pub fn album_artists(&self, query: &ItemsQuery) -> Result<QueryResult<BaseItemDto>, Error> {
        self.get("/Artists/AlbumArtists", &Self::query(query))
    }

    pub fn artists(&self, query: &ItemsQuery) -> Result<QueryResult<BaseItemDto>, Error> {
        self.get("/Artists", &Self::query(query))
    }

    /// Genres; with `types(&[ItemKind::MusicAlbum])`, a music library's.
    pub fn genres(&self, query: &ItemsQuery) -> Result<QueryResult<BaseItemDto>, Error> {
        self.get("/Genres", &Self::query(query))
    }

    /// A playlist's entries in the playlist's own order, which is not to
    /// be sorted.
    pub fn playlist_items(
        &self,
        playlist_id: &str,
        query: &ItemsQuery,
    ) -> Result<QueryResult<BaseItemDto>, Error> {
        self.get(
            &format!("/Playlists/{}/Items", segment(playlist_id)),
            &Self::query(query),
        )
    }

    /// Items of the given kinds whose names match `term`.
    pub fn search(
        &self,
        term: &str,
        kinds: &[ItemKind],
        limit: u32,
    ) -> Result<QueryResult<BaseItemDto>, Error> {
        self.items(
            &ItemsQuery::new()
                .search(term)
                .types(kinds)
                .recursive()
                .limit(limit)
                // Whether its card's menu offers Download.
                .fields(&["PrimaryImageAspectRatio", "CanDownload"])
                .images(
                    &[ImageType::Primary, ImageType::Thumb, ImageType::Backdrop],
                    1,
                ),
        )
    }

    /// Artists whose names match `term`, which `/Items` doesn't search.
    pub fn search_artists(
        &self,
        term: &str,
        limit: u32,
    ) -> Result<QueryResult<BaseItemDto>, Error> {
        self.artists(
            &ItemsQuery::new()
                .search(term)
                .limit(limit)
                .fields(&["PrimaryImageAspectRatio"])
                .images(&[ImageType::Primary], 1),
        )
    }

    /// Marks an item played (which also clears its resume position) or
    /// unplayed.
    pub fn set_played(&self, id: &str, played: bool) -> Result<UserItemDataDto, Error> {
        let path = format!("/UserPlayedItems/{}", segment(id));
        let reply = if played {
            self.request("POST", &path, &[], Some(Vec::new()))?
        } else {
            self.request("DELETE", &path, &[], None)?
        };
        reply.json()
    }

    pub fn set_favorite(&self, id: &str, favorite: bool) -> Result<UserItemDataDto, Error> {
        let path = format!("/UserFavoriteItems/{}", segment(id));
        let reply = if favorite {
            self.request("POST", &path, &[], Some(Vec::new()))?
        } else {
            self.request("DELETE", &path, &[], None)?
        };
        reply.json()
    }

    // Playback.

    /// How the server will deliver an item: the media sources, with the
    /// streams it chose and a transcoding URL when it won't play directly,
    /// and the play session id the reports carry. The answer's
    /// `error_code` says why nothing can play.
    pub fn playback_info(
        &self,
        id: &str,
        request: &PlaybackInfoRequest,
    ) -> Result<PlaybackInfoResponse, Error> {
        self.post(
            &format!("/Items/{}/PlaybackInfo", segment(id)),
            &[],
            request,
        )
    }

    pub fn report_start(&self, info: &PlaybackStartInfo) -> Result<(), Error> {
        self.post_only("/Sessions/Playing", &[], Some(info))
    }

    pub fn report_progress(&self, info: &PlaybackProgressInfo) -> Result<(), Error> {
        self.post_only("/Sessions/Playing/Progress", &[], Some(info))
    }

    pub fn report_stopped(&self, info: &PlaybackStopInfo) -> Result<(), Error> {
        self.post_only("/Sessions/Playing/Stopped", &[], Some(info))
    }

    /// The intro, credits, recap, preview and commercial spans; none for an
    /// item the server has none for.
    pub fn media_segments(&self, id: &str) -> Result<Vec<MediaSegmentDto>, Error> {
        match self
            .get::<QueryResult<MediaSegmentDto>>(&format!("/MediaSegments/{}", segment(id)), &[])
        {
            Ok(result) => Ok(result.items),
            Err(Error::NotFound) => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    /// A track's lyrics; None when it has none.
    pub fn lyrics(&self, id: &str) -> Result<Option<LyricDto>, Error> {
        match self.get(&format!("/Audio/{}/Lyrics", segment(id)), &[]) {
            Ok(lyrics) => Ok(Some(lyrics)),
            Err(Error::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }

    // Downloading.

    /// An item's file as the server keeps it (`GET /Items/{id}/Download`),
    /// to be read as it comes rather than whole, since it can run to
    /// gigabytes. It needs the account's token, in the header as every
    /// request has it, and the account's permission to download
    /// (`Policy.EnableContentDownloading`); a redirect is followed only on
    /// this server, as every request's is. The server refuses an account
    /// without that permission with 403, and an item that isn't a file of
    /// its own (a folder, a stream from elsewhere, a disc's folder) with
    /// 400.
    pub fn download(&self, id: &str) -> Result<Download, Error> {
        let url = self.url(&format!("/Items/{}/Download", segment(id)), &[]);
        let (mut response, url) =
            self.exchange("GET", url, None, Follow::SameServer, false, true)?;
        let status = response.status().as_u16();
        if !(200..=299).contains(&status) {
            // What the error says, which is short; a body that can't be
            // read says nothing.
            let body = response
                .body_mut()
                .with_config()
                .limit(64 * 1024)
                .read_to_vec()
                .unwrap_or_default();
            let error = match (Reply { status, body, url }).check() {
                Err(Error::Forbidden) => {
                    Error::NoDownload("The server doesn't allow this account to download.")
                }
                Err(Error::Server(400, _)) => {
                    Error::NoDownload("The server doesn't offer this for download.")
                }
                Err(error) => error,
                Ok(_) => Error::Server(status, String::new()),
            };
            return Err(error);
        }
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let disposition = header("content-disposition");
        let length = header("content-length").and_then(|length| length.trim().parse().ok());
        // Neither a size nor chunks, the file ends where the connection
        // does, lost or not: a proxy in front of the server may send it so.
        // Jellyfin itself always gives the size.
        let chunked = header("transfer-encoding")
            .is_some_and(|coding| coding.to_ascii_lowercase().contains("chunked"));
        if length.is_none() && !chunked {
            return Err(Error::NoDownload(
                "The server sends this file without its size, so a download cut short \
                 couldn't be told from a whole one.",
            ));
        }
        Ok(Download {
            disposition,
            length,
            reader: response.into_body().into_reader(),
        })
    }

    // URLs.

    /// An item's image: anyone may fetch it, so the URL carries no token.
    /// `tag` (from `ImageTags`) changes when the image does, so the image
    /// cache can keep what it fetched by URL. `max_width` is rounded up to
    /// one of a few sizes (`image_width`). JPEG, which Patina decodes,
    /// whatever the original is.
    pub fn image_url(
        &self,
        item_id: &str,
        kind: ImageType,
        index: Option<u32>,
        tag: Option<&str>,
        max_width: Option<u32>,
    ) -> Url {
        let mut path = format!("/Items/{}/Images/{}", segment(item_id), kind.as_str());
        if let Some(index) = index {
            path.push_str(&format!("/{index}"));
        }
        let mut query = Vec::new();
        if let Some(tag) = tag {
            query.push(("tag".to_owned(), tag.to_owned()));
        }
        if let Some(width) = max_width {
            query.push(("maxWidth".to_owned(), image_width(width).to_string()));
        }
        query.push(("format".to_owned(), "Jpg".to_owned()));
        self.url(&path, &query)
    }

    /// An account's picture, for the sign-in screen.
    pub fn user_image_url(&self, user_id: &str, tag: Option<&str>, max_width: Option<u32>) -> Url {
        let mut query = vec![("userId".to_owned(), user_id.to_owned())];
        if let Some(tag) = tag {
            query.push(("tag".to_owned(), tag.to_owned()));
        }
        if let Some(width) = max_width {
            query.push(("maxWidth".to_owned(), width.to_string()));
        }
        query.push(("format".to_owned(), "Jpg".to_owned()));
        self.url("/UserImage", &query)
    }

    /// One tile of a version's trickplay thumbnails, which only a
    /// signed-in request gets: fetch it with `get_bytes`.
    pub fn trickplay_tile_url(
        &self,
        item_id: &str,
        width: u32,
        index: u32,
        media_source_id: Option<&str>,
    ) -> Url {
        let query: Vec<(String, String)> = media_source_id
            .map(|id| ("mediaSourceId".to_owned(), id.to_owned()))
            .into_iter()
            .collect();
        self.url(
            &format!("/Videos/{}/Trickplay/{width}/{index}.jpg", segment(item_id)),
            &query,
        )
    }

    /// A version's file as it is, which mpv can seek in and which needs
    /// no token.
    pub fn static_stream_url(&self, item_id: &str, audio: bool, source: &MediaSourceInfo) -> Url {
        // "mov,mp4,m4a,3gp,3g2,mj2" for an MP4: the first will do.
        let container = source
            .container
            .as_deref()
            .and_then(|container| container.split(',').next())
            .map(str::trim)
            .filter(|container| !container.is_empty());
        let path = format!(
            "/{}/{}/stream{}",
            if audio { "Audio" } else { "Videos" },
            segment(item_id),
            container
                .map(|container| format!(".{}", segment(container)))
                .unwrap_or_default()
        );
        let mut query = vec![
            ("Static".to_owned(), "true".to_owned()),
            ("mediaSourceId".to_owned(), source.id.clone()),
            ("deviceId".to_owned(), self.identity.device_id.clone()),
        ];
        if let Some(tag) = &source.etag {
            query.push(("Tag".to_owned(), tag.clone()));
        }
        if let Some(live) = &source.live_stream_id {
            query.push(("LiveStreamId".to_owned(), live.clone()));
        }
        self.url(&path, &query)
    }

    /// A version's transcode, without the token the server wrote into it.
    /// An HLS playlist also asks for no trickplay rendition: 12.4 adds one
    /// by default and writes the request's token into its URI, taken from
    /// the header when that is where the token came, so the token would be
    /// in the playlist mpv reads and in mpv's log. Patina fetches trickplay
    /// tiles itself, and a server that doesn't know the parameter ignores
    /// it. None for a transcode on another host (a plugin's, a proxy's),
    /// which must not get the header mpv sends with it.
    pub fn transcode_url(&self, source: &MediaSourceInfo) -> Option<Url> {
        let url = source.transcoding_url.as_deref()?;
        let mut url = auth::resolve(&self.base, &auth::strip_api_key(url));
        if !auth::same_server(&self.base, &url) {
            return None;
        }
        let hls = source
            .transcoding_sub_protocol
            .as_deref()
            .is_some_and(|protocol| protocol.eq_ignore_ascii_case("hls"))
            || url.path().to_ascii_lowercase().ends_with(".m3u8");
        if hls {
            set_param(&mut url, "EnableTrickplay", "false");
        }
        Some(url)
    }

    /// An external subtitle's file, without the token; None for one on
    /// another host, which must not get the header mpv sends.
    pub fn delivery_url(&self, stream: &MediaStream) -> Option<Url> {
        let url = stream.delivery_url.as_deref()?;
        let url = auth::resolve(&self.base, &auth::strip_api_key(url));
        auth::same_server(&self.base, &url).then_some(url)
    }

    /// A signed-in GET of an image or trickplay tile. The header goes only
    /// to this server.
    pub fn get_bytes(&self, url: &Url) -> Result<Vec<u8>, Error> {
        Ok(self
            .send("GET", url.clone(), None, Follow::SameServer, false)?
            .check()?
            .body)
    }

    /// Whether `url` is under this client's address: on its server, and
    /// below its base path.
    fn serves(&self, url: &Url) -> bool {
        let base = self.base.path().trim_end_matches('/');
        auth::same_server(&self.base, url)
            && url
                .path()
                .strip_prefix(base)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    }
}

/// `url` with the query parameter `name` (in any case) set to `value`, the
/// rest of the query left byte for byte, as `auth::strip_api_key` leaves it.
fn set_param(url: &mut Url, name: &str, value: &str) {
    let query = url.query().unwrap_or_default().to_owned();
    let mut pairs: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            let key = pair.split('=').next().unwrap_or_default();
            !key.is_empty() && !key.eq_ignore_ascii_case(name)
        })
        .collect();
    let set = format!("{name}={value}");
    pairs.push(&set);
    url.set_query(Some(&pairs.join("&")));
}

/// The clients of the servers Patina knows, one per server, for the image
/// cache: it is app-wide and holds every server's images, so each URL is
/// fetched through the client of the server it is on, with that server's
/// token and certificates. A server being signed in to has its client
/// here too, without a token, for the accounts' pictures. Clones share the
/// list.
#[derive(Clone, Default)]
pub struct Clients(std::sync::Arc<std::sync::RwLock<Vec<Client>>>);

impl Clients {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a client, or replaces the one at the same address.
    pub fn insert(&self, client: Client) {
        let mut clients = self.0.write().unwrap_or_else(|error| error.into_inner());
        clients.retain(|known| known.base != client.base);
        clients.push(client);
    }

    /// Forgets the client at `base`, once its server is signed out.
    pub fn remove(&self, base: &Url) {
        let mut clients = self.0.write().unwrap_or_else(|error| error.into_inner());
        clients.retain(|known| known.base != *base);
    }

    /// The client whose server `url` is on; of two servers under one host
    /// name (behind a proxy, at different paths), the one with the longer
    /// path.
    pub fn for_url(&self, url: &Url) -> Option<Client> {
        let clients = self.0.read().unwrap_or_else(|error| error.into_inner());
        clients
            .iter()
            .filter(|client| client.serves(url))
            .max_by_key(|client| client.base.path().trim_end_matches('/').len())
            .cloned()
    }

    /// The image cache's way of fetching: through the client of the URL's
    /// server, and refused for a URL on none of them. Anything but a 2xx
    /// answer is an error, so an error page is never decoded or kept.
    pub fn fetch(&self) -> super::images::Fetch {
        let clients = self.clone();
        std::sync::Arc::new(move |url: &str| {
            let url = Url::parse(url).map_err(|error| error.to_string())?;
            let client = clients.for_url(&url).ok_or_else(|| {
                format!(
                    "{} is no server Patina knows",
                    url.origin().ascii_serialization()
                )
            })?;
            client.get_bytes(&url).map_err(|error| error.to_string())
        })
    }
}

/// The widths images are asked for. The server keeps each size it makes
/// and the image cache each URL it fetched, so asking for a few sizes
/// rather than every width a layout comes to lets both serve an image
/// again; the cache scales it down to the width it is shown at.
const IMAGE_WIDTHS: [u32; 9] = [160, 240, 320, 480, 640, 960, 1280, 1920, 3840];

/// The smallest of `IMAGE_WIDTHS` at least `width` wide, or the largest.
pub fn image_width(width: u32) -> u32 {
    IMAGE_WIDTHS
        .iter()
        .copied()
        .find(|size| *size >= width)
        .unwrap_or(IMAGE_WIDTHS[IMAGE_WIDTHS.len() - 1])
}

/// Finds the server at what the user typed, for adding it: asks
/// `/System/Info/Public` there without a token, following redirects
/// wherever they go, and keeps the address the answer came from. The
/// address of Jellyfin's web app (…/web/…), typed or redirected to, means
/// the server just above it.
pub fn probe(address: &str, identity: &Identity, ca_file: Option<&Path>) -> Result<Probed, Error> {
    let base = auth::normalize_address(address)?;
    let client = Client::new(base.clone(), identity.clone(), ca_file)?;
    let (error, ended) = match client.probe_at(&base) {
        Ok(probed) => return Ok(probed),
        Err(failure) => failure,
    };
    if error == Error::NotHttps && !address.contains("://") {
        // https was only assumed, and what answered speaks http, as most
        // servers at home do (Jellyfin's own port, 8096, is http): it is
        // asked again over http, at the same port. The sign-in says the
        // address is plain http.
        let mut plain = base.clone();
        let port = base.port_or_known_default();
        if plain.set_scheme("http").is_ok() && plain.set_port(port).is_ok() {
            let client = Client::new(plain.clone(), identity.clone(), ca_file)?;
            return client.probe_at(&plain).map_err(|(error, _)| error);
        }
    }
    if let Error::Tls(reason) = &error
        && !address.contains("://")
    {
        // https was only assumed, and many servers at home speak http.
        return Err(Error::Tls(format!(
            "{reason}. If the server doesn't use https, enter its address with http:// in front."
        )));
    }
    if error != Error::NotJellyfin {
        return Err(error);
    }
    let Some(parent) = above_web(&base).or_else(|| ended.as_ref().and_then(above_web)) else {
        return Err(error);
    };
    let client = Client::new(parent.clone(), identity.clone(), ca_file)?;
    client.probe_at(&parent).map_err(|(error, _)| error)
}

/// The address above the last "web" in a URL's path, if it has one.
fn above_web(url: &Url) -> Option<Url> {
    let segments: Vec<&str> = url.path().split('/').collect();
    let web = segments
        .iter()
        .rposition(|segment| segment.eq_ignore_ascii_case("web"))?;
    let mut parent = url.clone();
    parent.set_query(None);
    parent.set_fragment(None);
    parent.set_path(&segments[..web].join("/"));
    Some(parent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    /// A reply the stand-in server gives.
    struct Canned {
        status: u16,
        headers: Vec<(&'static str, String)>,
        body: String,
    }

    fn reply(status: u16, body: &str) -> Canned {
        Canned {
            status,
            headers: Vec::new(),
            body: body.to_owned(),
        }
    }

    fn redirect(status: u16, location: &str) -> Canned {
        Canned {
            status,
            headers: vec![("Location", location.to_owned())],
            body: String::new(),
        }
    }

    /// A stand-in for a server: answers each request with the next reply
    /// made for its address, and reports the requests it got, head and
    /// body.
    fn serve(
        replies: impl FnOnce(&str) -> Vec<Canned>,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let replies = replies(&address);
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for canned in replies {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap_or(0);
                    }
                    request.push_str(&line);
                }
                let mut payload = vec![0; length];
                let _ = reader.read_exact(&mut payload);
                request.push_str(&String::from_utf8_lossy(&payload));
                let _ = sender.send(request);
                let mut head = format!(
                    "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                    canned.status,
                    canned.body.len()
                );
                for (name, value) in &canned.headers {
                    head.push_str(&format!("{name}: {value}\r\n"));
                }
                let mut stream = stream;
                let _ = write!(stream, "{head}\r\n{}", canned.body);
            }
        });
        (address, receiver)
    }

    fn identity() -> Identity {
        Identity {
            device: "Patina on Linux".into(),
            device_id: "0011aabb".into(),
            version: "9.9.9".into(),
        }
    }

    fn client(address: &str) -> Client {
        Client::new(Url::parse(address).unwrap(), identity(), None).unwrap()
    }

    /// A request's header, by name in any case.
    fn header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
        request.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then_some(value.trim())
        })
    }

    const PAGE: &str = r#"{"Items": [{"Id": "m1", "Name": "A Film", "Type": "Movie", "ImageTags": {"Primary": "t1"}}],
                           "TotalRecordCount": 1, "StartIndex": 0}"#;

    #[test]
    fn requests_carry_the_header_and_the_query() {
        let (address, requests) = serve(|_| vec![reply(200, PAGE)]);
        let client = client(&format!("{address}/jf")).with_session("tok+en", "u1");
        let page = client
            .items(
                &ItemsQuery::new()
                    .parent("lib1")
                    .types(&[ItemKind::Movie, ItemKind::Series])
                    .recursive()
                    .sort(&["SortName", "ProductionYear"], SortOrder::Ascending)
                    .fields(&["Overview", "PrimaryImageAspectRatio"])
                    .images(&[ImageType::Primary, ImageType::Backdrop], 1)
                    .page(100, 50)
                    .search("fish & chips")
                    .param("collapseBoxSetItems", "false"),
            )
            .unwrap();
        assert_eq!(page.items[0].kind, ItemKind::Movie);
        assert_eq!(page.items[0].image_tag(ImageType::Primary), Some("t1"));
        let request = requests.recv().unwrap();
        let line = request.lines().next().unwrap();
        assert_eq!(
            line,
            "GET /jf/Items?parentId=lib1&includeItemTypes=Movie,Series&recursive=true\
             &sortBy=SortName,ProductionYear&sortOrder=Ascending&fields=Overview,PrimaryImageAspectRatio\
             &enableImageTypes=Primary,Backdrop&imageTypeLimit=1&startIndex=100&limit=50\
             &searchTerm=fish%20%26%20chips&collapseBoxSetItems=false HTTP/1.1"
        );
        assert_eq!(
            header(&request, "authorization"),
            Some(
                "MediaBrowser Client=\"Patina\", Device=\"Patina%20on%20Linux\", \
                 DeviceId=\"0011aabb\", Version=\"9.9.9\", Token=\"tok%2Ben\""
            )
        );
        assert_eq!(
            header(&request, "user-agent"),
            Some(format!("Patina/{}", env!("CARGO_PKG_VERSION")).as_str())
        );
        assert_eq!(header(&request, "accept"), Some("application/json"));
    }

    #[test]
    fn statuses_become_errors_a_user_can_read() {
        let (address, _requests) = serve(|_| {
            vec![
                reply(401, ""),
                reply(403, ""),
                reply(404, ""),
                reply(500, "Error processing request."),
                reply(500, "\"Something specific\""),
                reply(502, "<html><body>Bad gateway</body></html>"),
                reply(503, ""),
                reply(200, "not json"),
            ]
        });
        let client = client(&address).with_session("t", "u");
        assert_eq!(client.me(), Err(Error::Unauthorized));
        assert_eq!(client.me(), Err(Error::Forbidden));
        assert_eq!(client.item("gone"), Err(Error::NotFound));
        assert_eq!(client.me(), Err(Error::Server(500, String::new())));
        assert_eq!(
            client.me(),
            Err(Error::Server(500, "Something specific".into()))
        );
        assert_eq!(client.me(), Err(Error::Server(502, String::new())));
        let busy = client.me().unwrap_err();
        assert!(busy.to_string().contains("starting up"), "{busy}");
        assert!(matches!(client.me(), Err(Error::Decode(_))));
        // An answer past the limit is the server's, not a network failure.
        let long = network(ureq::Error::BodyExceedsLimit(JSON_LIMIT));
        assert_eq!(
            long.to_string(),
            "The server's answer could not be read: it is longer than the 64 MB Patina reads"
        );
        for error in [
            Error::Unauthorized,
            Error::NotJellyfin,
            Error::Moved("https://elsewhere".into()),
            Error::Unsupported("10.8.13".into()),
        ] {
            assert!(error.to_string().ends_with('.'), "{error}");
        }
    }

    #[test]
    fn nothing_listening_is_a_network_error() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        assert!(matches!(client(&address).me(), Err(Error::Network(_))));
    }

    #[test]
    fn a_server_that_isnt_speaking_tls_says_so_in_words() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.write_all(
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
                std::thread::sleep(Duration::from_millis(200));
            }
        });
        let result = client(&format!("https://127.0.0.1:{port}")).public_info();
        assert_eq!(result, Err(Error::NotHttps));
    }

    #[test]
    fn signing_in_sends_no_token_and_says_why_it_failed() {
        let (address, requests) = serve(|_| {
            vec![
                reply(
                    200,
                    r#"{"User": {"Id": "u1", "Name": "viewer"}, "AccessToken": "fresh", "ServerId": "s"}"#,
                ),
                reply(401, "Error processing request."),
                reply(403, ""),
            ]
        });
        let client = client(&address);
        let result = client.authenticate_by_name("viewer", "p\"w").unwrap();
        assert_eq!(result.access_token, "fresh");
        let request = requests.recv().unwrap();
        assert!(
            request.starts_with("POST /Users/AuthenticateByName "),
            "{request}"
        );
        assert!(
            !header(&request, "authorization")
                .unwrap()
                .contains("Token=")
        );
        assert_eq!(header(&request, "content-type"), Some("application/json"));
        assert!(request.contains(r#""Username":"viewer""#), "{request}");
        assert!(request.contains(r#""Pw":"p\"w""#), "{request}");
        assert!(matches!(
            client.authenticate_by_name("viewer", "wrong"),
            Err(Error::SignIn(reason)) if reason.contains("wrong")
        ));
        // The commonest refusal, an account kept to the server's network,
        // is named.
        assert!(matches!(
            client.authenticate_by_name("viewer", "x"),
            Err(Error::SignIn(reason)) if reason.contains("outside the server's network")
        ));
    }

    #[test]
    fn quick_connect_starts_polls_and_signs_in() {
        let (address, requests) = serve(|_| {
            vec![
                reply(200, "true"),
                reply(
                    200,
                    r#"{"Authenticated": false, "Secret": "sec", "Code": "123456"}"#,
                ),
                reply(
                    200,
                    r#"{"Authenticated": true, "Secret": "sec", "Code": "123456"}"#,
                ),
                reply(200, r#"{"User": {"Id": "u2"}, "AccessToken": "qc"}"#),
                reply(404, "\"Unknown secret\""),
                reply(401, "\"Quick connect is disabled\""),
            ]
        });
        let client = client(&address);
        assert_eq!(client.quick_connect_enabled(), Ok(true));
        let started = client.quick_connect_initiate().unwrap();
        assert_eq!(started.code, "123456");
        assert!(
            client
                .quick_connect_connect(&started.secret)
                .unwrap()
                .authenticated
        );
        assert_eq!(
            client
                .quick_connect_authenticate("sec")
                .unwrap()
                .access_token,
            "qc"
        );
        assert!(matches!(
            client.quick_connect_connect("sec"),
            Err(Error::SignIn(reason)) if reason.contains("ran out")
        ));
        assert!(matches!(
            client.quick_connect_initiate(),
            Err(Error::SignIn(reason)) if reason.contains("turned off")
        ));
        let lines: Vec<String> = requests
            .iter()
            .take(5)
            .map(|request| request.lines().next().unwrap().to_owned())
            .collect();
        assert_eq!(
            lines,
            [
                "GET /QuickConnect/Enabled HTTP/1.1",
                "POST /QuickConnect/Initiate HTTP/1.1",
                "GET /QuickConnect/Connect?secret=sec HTTP/1.1",
                "POST /Users/AuthenticateWithQuickConnect HTTP/1.1",
                "GET /QuickConnect/Connect?secret=sec HTTP/1.1",
            ]
        );
    }

    #[test]
    fn redirects_on_the_same_server_keep_the_header() {
        let (address, requests) = serve(|_| {
            vec![
                redirect(302, "/moved/Users/Me"),
                reply(200, r#"{"Id": "u1", "Name": "viewer"}"#),
            ]
        });
        let user = client(&address).with_session("t", "u1").me().unwrap();
        assert_eq!(user.name, "viewer");
        let first = requests.recv().unwrap();
        let second = requests.recv().unwrap();
        assert!(second.starts_with("GET /moved/Users/Me "), "{second}");
        assert!(
            header(&first, "authorization")
                .unwrap()
                .contains("Token=\"t\"")
        );
        assert!(
            header(&second, "authorization")
                .unwrap()
                .contains("Token=\"t\"")
        );
    }

    #[test]
    fn redirects_to_another_host_are_not_followed_with_the_token() {
        let (other, other_requests) =
            serve(|_| vec![reply(200, r#"{"Id": "u1", "Name": "viewer"}"#)]);
        let other = other.replace("127.0.0.1", "localhost");
        let (address, requests) = serve(|_| vec![redirect(307, &format!("{other}/Users/Me?x=1"))]);
        let result = client(&address).with_session("t", "u1").me();
        assert_eq!(result, Err(Error::Moved(other.clone())));
        requests.recv().unwrap();
        assert!(
            other_requests
                .recv_timeout(Duration::from_millis(300))
                .is_err()
        );
    }

    /// Two stand-ins on 127.0.0.1, which differ only by port: the first
    /// redirects to the second.
    fn redirect_to_another_port(
        status: u16,
    ) -> (
        String,
        String,
        std::sync::mpsc::Receiver<String>,
        std::sync::mpsc::Receiver<String>,
    ) {
        let (other, other_requests) =
            serve(|_| vec![reply(200, r#"{"Id": "u1", "Name": "viewer"}"#)]);
        let location = format!("{other}/Users/Me");
        let (address, requests) = serve(|_| vec![redirect(status, &location)]);
        (address, other, requests, other_requests)
    }

    #[test]
    fn redirects_to_another_port_on_the_same_host_are_another_server() {
        let (address, other, requests, other_requests) = redirect_to_another_port(307);
        let result = client(&address).with_session("t", "u1").me();
        assert_eq!(result, Err(Error::Moved(other)));
        requests.recv().unwrap();
        assert!(
            other_requests
                .recv_timeout(Duration::from_millis(300))
                .is_err()
        );

        // Where redirects are followed anywhere, the header stays behind.
        let (address, _other, requests, other_requests) = redirect_to_another_port(302);
        let client = client(&address).with_session("t", "u1");
        let url = auth::api_url(client.base(), "/Users/Me");
        let reply = client
            .send("GET", url, None, Follow::Anywhere, true)
            .unwrap();
        assert_eq!(reply.status, 200);
        let first = requests.recv().unwrap();
        assert!(header(&first, "authorization").unwrap().contains("Token"));
        let second = other_requests.recv().unwrap();
        assert_eq!(header(&second, "authorization"), None);
    }

    #[test]
    fn an_upgrade_to_https_is_a_new_address() {
        let (address, _requests) =
            serve(|address| vec![redirect(308, &address.replace("http://", "https://"))]);
        let result = client(&address).with_session("t", "u1").me();
        assert_eq!(
            result,
            Err(Error::Moved(address.replace("http://", "https://")))
        );
    }

    #[test]
    fn a_post_answered_with_303_is_asked_again_as_a_get() {
        let (address, requests) = serve(|_| {
            vec![
                redirect(303, "/Elsewhere"),
                reply(200, r#"{"Played": true}"#),
            ]
        });
        let data = client(&address)
            .with_session("t", "u")
            .set_played("i1", true)
            .unwrap();
        assert!(data.played);
        assert!(
            requests
                .recv()
                .unwrap()
                .starts_with("POST /UserPlayedItems/i1 ")
        );
        assert!(requests.recv().unwrap().starts_with("GET /Elsewhere "));
    }

    const INFO: &str = r#"{"ServerName": "Den", "Version": "12.4.0", "ProductName": "Jellyfin Server",
                           "Id": "8d1ce7b7a96c4e2e9c4a3b0ff0e2d7a1", "StartupWizardCompleted": true}"#;

    #[test]
    fn probing_follows_redirects_anywhere_without_the_header() {
        let (other, other_requests) = serve(|_| vec![reply(200, INFO)]);
        let other = other.replace("127.0.0.1", "localhost");
        let (address, requests) = serve(|_| {
            vec![redirect(
                301,
                &format!("{other}/jellyfin/System/Info/Public"),
            )]
        });
        let probed = probe(&address, &identity(), None).unwrap();
        assert_eq!(probed.info.server_name, "Den");
        assert_eq!(probed.address.as_str(), format!("{other}/jellyfin"));
        let first = requests.recv().unwrap();
        assert!(first.starts_with("GET /System/Info/Public "), "{first}");
        assert!(!header(&first, "authorization").unwrap().contains("Token"));
        let second = other_requests.recv().unwrap();
        assert!(
            second.starts_with("GET /jellyfin/System/Info/Public "),
            "{second}"
        );
        assert_eq!(header(&second, "authorization"), None);
    }

    #[test]
    fn probing_the_web_apps_address_finds_the_server_above_it() {
        let (address, requests) = serve(|address| {
            vec![
                redirect(302, &format!("{address}/web/index.html")),
                reply(200, "<!DOCTYPE html><html></html>"),
                reply(200, INFO),
            ]
        });
        let probed = probe(&format!("{address}/web/"), &identity(), None).unwrap();
        assert_eq!(probed.address.as_str(), format!("{address}/"));
        let paths: Vec<String> = requests
            .iter()
            .take(3)
            .map(|request| request.lines().next().unwrap().to_owned())
            .collect();
        assert_eq!(
            paths,
            [
                "GET /web/System/Info/Public HTTP/1.1",
                "GET /web/index.html HTTP/1.1",
                "GET /System/Info/Public HTTP/1.1",
            ]
        );
    }

    #[test]
    fn probing_turns_down_what_isnt_a_supported_jellyfin() {
        let (address, _requests) = serve(|_| {
            vec![
                reply(
                    200,
                    r#"{"ServerName": "Old", "Version": "10.8.13", "Id": "abc"}"#,
                ),
                reply(
                    200,
                    r#"{"ServerName": "E", "Version": "4.8.0", "ProductName": "Emby Server", "Id": "abc"}"#,
                ),
                reply(404, ""),
                reply(200, r#"{"hello": "world"}"#),
            ]
        });
        assert_eq!(
            probe(&address, &identity(), None),
            Err(Error::Unsupported("10.8.13".into()))
        );
        assert_eq!(probe(&address, &identity(), None), Err(Error::NotJellyfin));
        assert_eq!(probe(&address, &identity(), None), Err(Error::NotJellyfin));
        assert_eq!(probe(&address, &identity(), None), Err(Error::NotJellyfin));
        assert!(matches!(
            probe("ftp://example.org", &identity(), None),
            Err(Error::Address(_))
        ));
    }

    #[test]
    fn an_address_without_a_scheme_falls_back_to_plain_http() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            // The https tried first gets what a plain http server answers
            // to a TLS hello.
            if let Ok((mut stream, _)) = listener.accept() {
                let mut hello = [0u8; 4096];
                let _ = stream.read(&mut hello);
                let _ = stream.write_all(
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
            // Then the same port over http.
            if let Ok((stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                    line.clear();
                }
                let mut stream = stream;
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{INFO}",
                    INFO.len()
                );
            }
        });
        let probed = probe(&format!("127.0.0.1:{port}"), &identity(), None).unwrap();
        assert_eq!(probed.info.server_name, "Den");
        assert_eq!(probed.address.scheme(), "http");
        assert_eq!(probed.address.port(), Some(port));
    }

    #[test]
    fn network_failures_read_as_words() {
        let not_tls = std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "received corrupt message of type InvalidContentType",
        );
        assert_eq!(network(ureq::Error::Io(not_tls)), Error::NotHttps);
        assert!(!Error::NotHttps.to_string().contains("InvalidContentType"));
        let lost = std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "Peer disconnected");
        let lost = network(ureq::Error::Io(lost));
        assert_eq!(
            lost.to_string(),
            "Could not reach the server: the connection was lost."
        );
        assert_eq!(
            lost.about(Some("Den")),
            "Could not reach Den: the connection was lost."
        );
        assert_eq!(
            Error::NotFound.about(Some("Den")),
            Error::NotFound.to_string()
        );
    }

    #[test]
    fn urls_carry_no_token() {
        let client = client("https://example.org/jf").with_session("secret", "u");
        assert_eq!(
            client
                .image_url("i1", ImageType::Primary, None, Some("t a"), Some(400))
                .as_str(),
            "https://example.org/jf/Items/i1/Images/Primary?tag=t%20a&maxWidth=480&format=Jpg"
        );
        assert_eq!(
            [1, 160, 161, 300, 1921, 10_000].map(image_width),
            [160, 160, 240, 320, 3840, 3840]
        );
        assert_eq!(
            client
                .image_url("i1", ImageType::Backdrop, Some(2), None, None)
                .as_str(),
            "https://example.org/jf/Items/i1/Images/Backdrop/2?format=Jpg"
        );
        assert_eq!(
            client
                .trickplay_tile_url("i1", 320, 4, Some("ms1"))
                .as_str(),
            "https://example.org/jf/Videos/i1/Trickplay/320/4.jpg?mediaSourceId=ms1"
        );
        assert_eq!(
            client.user_image_url("u1", None, Some(96)).as_str(),
            "https://example.org/jf/UserImage?userId=u1&maxWidth=96&format=Jpg"
        );
        let source = MediaSourceInfo {
            id: "ms1".into(),
            container: Some("mov,mp4,m4a".into()),
            etag: Some("e1".into()),
            transcoding_url: Some(
                "/videos/i1/master.m3u8?DeviceId=0011aabb&MediaSourceId=ms1&ApiKey=secret&PlaySessionId=p".into(),
            ),
            ..Default::default()
        };
        assert_eq!(
            client.static_stream_url("i1", false, &source).as_str(),
            "https://example.org/jf/Videos/i1/stream.mov?Static=true&mediaSourceId=ms1&deviceId=0011aabb&Tag=e1"
        );
        // An HLS playlist asks for no trickplay rendition, whose URI the
        // server would write the token into.
        assert_eq!(
            client.transcode_url(&source).unwrap().as_str(),
            "https://example.org/jf/videos/i1/master.m3u8?DeviceId=0011aabb&MediaSourceId=ms1\
             &PlaySessionId=p&EnableTrickplay=false"
        );
        let asked = MediaSourceInfo {
            transcoding_url: Some(
                "/videos/i1/master.m3u8?enableTrickplay=true&VideoCodec=h264%2Chevc".into(),
            ),
            ..Default::default()
        };
        assert_eq!(
            client.transcode_url(&asked).unwrap().as_str(),
            "https://example.org/jf/videos/i1/master.m3u8?VideoCodec=h264%2Chevc&EnableTrickplay=false"
        );
        let progressive = MediaSourceInfo {
            transcoding_url: Some("/audio/a1/stream.mp3?AudioCodec=mp3&ApiKey=secret".into()),
            transcoding_sub_protocol: Some("http".into()),
            ..Default::default()
        };
        assert_eq!(
            client.transcode_url(&progressive).unwrap().as_str(),
            "https://example.org/jf/audio/a1/stream.mp3?AudioCodec=mp3"
        );
        // A transcode elsewhere would get the header: not played.
        for elsewhere in [
            "https://cdn.example.com/videos/i1/master.m3u8?MediaSourceId=ms1",
            "http://example.org/jf/videos/i1/master.m3u8",
            "https://example.org:8920/jf/videos/i1/master.m3u8",
        ] {
            let source = MediaSourceInfo {
                transcoding_url: Some(elsewhere.into()),
                ..Default::default()
            };
            assert_eq!(client.transcode_url(&source), None, "{elsewhere}");
        }
        // For mpv, as a list of one: set as a string, mpv would split the
        // header at its commas.
        assert_eq!(
            client.http_header_fields(),
            [format!("Authorization: {}", client.authorization())]
        );
        let subtitle = |url: &str| MediaStream {
            delivery_url: Some(url.into()),
            ..Default::default()
        };
        assert_eq!(
            client
                .delivery_url(&subtitle(
                    "/Videos/i1/ms1/Subtitles/0/0/Stream.srt?ApiKey=secret"
                ))
                .unwrap()
                .as_str(),
            "https://example.org/jf/Videos/i1/ms1/Subtitles/0/0/Stream.srt"
        );
        assert_eq!(
            client.delivery_url(&subtitle("https://subs.example.com/a.srt")),
            None
        );
        assert!(!format!("{client:?}").contains("secret"));
    }

    #[test]
    fn bytes_get_the_header_only_on_the_server() {
        let (other, other_requests) = serve(|_| vec![reply(200, "elsewhere")]);
        let other = other.replace("127.0.0.1", "localhost");
        let (address, requests) = serve(|_| vec![reply(200, "tile")]);
        let client = client(&address).with_session("t", "u");
        let tile = client
            .get_bytes(&client.trickplay_tile_url("i1", 320, 0, None))
            .unwrap();
        assert_eq!(tile, b"tile");
        assert!(header(&requests.recv().unwrap(), "authorization").is_some());
        let bytes = client
            .get_bytes(&Url::parse(&format!("{other}/a.jpg")).unwrap())
            .unwrap();
        assert_eq!(bytes, b"elsewhere");
        assert_eq!(
            header(&other_requests.recv().unwrap(), "authorization"),
            None
        );
    }

    #[test]
    fn a_download_comes_as_it_is_read_and_its_refusals_say_why() {
        let (address, requests) = serve(|address| {
            vec![
                redirect(302, &format!("{address}/elsewhere/Items/m1/Download")),
                Canned {
                    status: 200,
                    headers: vec![(
                        "Content-Disposition",
                        "attachment; filename=\"Film.mkv\"".to_owned(),
                    )],
                    body: "the film's bytes".into(),
                },
                reply(403, ""),
                reply(400, "Error processing request."),
                reply(401, ""),
                redirect(302, "http://localhost:1/Items/m1/Download"),
            ]
        });
        let client = client(&address).with_session("tok", "u1");
        let mut download = client.download("m1").ok().unwrap();
        assert_eq!(
            download.disposition.as_deref(),
            Some("attachment; filename=\"Film.mkv\"")
        );
        assert_eq!(download.length, Some(16));
        let mut bytes = Vec::new();
        let mut buffer = [0; 5];
        loop {
            let read = download.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
        }
        assert_eq!(bytes, b"the film's bytes");
        // Both requests, the redirect's on the same server too, with the
        // header and without asking for JSON.
        for path in ["/Items/m1/Download", "/elsewhere/Items/m1/Download"] {
            let request = requests.recv().unwrap();
            assert!(
                request.starts_with(&format!("GET {path} HTTP/1.1")),
                "{request}"
            );
            assert!(
                header(&request, "authorization")
                    .unwrap()
                    .contains("Token=\"tok\"")
            );
            assert_ne!(header(&request, "accept"), Some("application/json"));
        }
        let refusal = || client.download("m1").err().map(|error| error.to_string());
        assert_eq!(
            refusal().as_deref(),
            Some("The server doesn't allow this account to download.")
        );
        assert_eq!(
            refusal().as_deref(),
            Some("The server doesn't offer this for download.")
        );
        assert_eq!(client.download("m1").err(), Some(Error::Unauthorized));
        assert!(matches!(client.download("m1").err(), Some(Error::Moved(_))));
    }

    #[test]
    fn images_are_fetched_through_their_own_servers_client() {
        let (first, first_requests) = serve(|_| vec![reply(200, "first")]);
        let (second, second_requests) = serve(|_| vec![reply(200, "second")]);
        let (unknown, unknown_requests) = serve(|_| vec![reply(200, "unknown")]);
        let clients = Clients::new();
        clients.insert(client(&first).with_session("token-1", "u"));
        clients.insert(client(&second).with_session("token-2", "u"));
        let fetch = clients.fetch();

        assert_eq!(
            fetch(&format!("{first}/Items/i1/Images/Primary")).unwrap(),
            b"first"
        );
        assert!(
            header(&first_requests.recv().unwrap(), "authorization")
                .unwrap()
                .contains("Token=\"token-1\"")
        );
        assert_eq!(
            fetch(&format!("{second}/Videos/i2/Trickplay/320/0.jpg")).unwrap(),
            b"second"
        );
        assert!(
            header(&second_requests.recv().unwrap(), "authorization")
                .unwrap()
                .contains("Token=\"token-2\"")
        );
        // A URL on no server Patina knows is not fetched at all.
        assert!(fetch(&format!("{unknown}/a.jpg")).is_err());
        assert!(
            unknown_requests
                .recv_timeout(Duration::from_millis(300))
                .is_err()
        );
        clients.remove(&Url::parse(&second).unwrap());
        assert!(
            clients
                .for_url(&Url::parse(&format!("{second}/x")).unwrap())
                .is_none()
        );

        // Two servers behind one host name, at different paths.
        let proxy = Clients::new();
        proxy.insert(client("https://example.org/jf").with_session("a", "u"));
        proxy.insert(client("https://example.org/jf/other").with_session("b", "u"));
        let token = |url: &str| {
            proxy
                .for_url(&Url::parse(url).unwrap())
                .and_then(|client| client.token().map(str::to_owned))
        };
        assert_eq!(
            token("https://example.org/jf/Items/1").as_deref(),
            Some("a")
        );
        assert_eq!(
            token("https://example.org/jf/other/Items/1").as_deref(),
            Some("b")
        );
        assert_eq!(token("https://example.org/jfx/Items/1"), None);
        assert_eq!(token("https://example.org:8443/jf/Items/1"), None);
    }

    #[test]
    fn playback_requests_send_their_bodies() {
        let (address, requests) = serve(|_| {
            vec![
                reply(
                    200,
                    r#"{"MediaSources": [{"Id": "ms1", "SupportsDirectPlay": true}], "PlaySessionId": "ps"}"#,
                ),
                reply(204, ""),
                reply(204, ""),
                reply(
                    200,
                    r#"{"Items": [{"Type": "Outro", "StartTicks": 1, "EndTicks": 2}]}"#,
                ),
                reply(404, ""),
                reply(200, r#"{"IsFavorite": false}"#),
                reply(204, ""),
            ]
        });
        let client = client(&address).with_session("t", "u");
        let info = client
            .playback_info(
                "i1",
                &PlaybackInfoRequest {
                    device_profile: Some(DeviceProfile::patina(&TranscodeRules::default())),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(info.play_session_id.as_deref(), Some("ps"));
        let request = requests.recv().unwrap();
        assert!(
            request.starts_with("POST /Items/i1/PlaybackInfo "),
            "{request}"
        );
        assert!(request.contains(r#""DirectPlayProfiles":[{"Type":"Video"},{"Type":"Audio"}]"#));
        client
            .report_progress(&PlaybackProgressInfo {
                item_id: "i1".into(),
                play_session_id: Some("ps".into()),
                position_ticks: Some(10),
                ..Default::default()
            })
            .unwrap();
        let request = requests.recv().unwrap();
        assert!(request.starts_with("POST /Sessions/Playing/Progress "));
        assert!(request.contains(r#""PlaySessionId":"ps""#));
        client
            .report_stopped(&PlaybackStopInfo {
                item_id: "i1".into(),
                ..Default::default()
            })
            .unwrap();
        let stopped = requests.recv().unwrap();
        assert!(stopped.starts_with("POST /Sessions/Playing/Stopped "));
        // Without a position the server would mark the item played.
        assert!(stopped.contains(r#""PositionTicks":0"#), "{stopped}");
        assert_eq!(
            client.media_segments("i1").unwrap()[0].kind,
            SegmentKind::Outro
        );
        assert!(
            requests
                .recv()
                .unwrap()
                .starts_with("GET /MediaSegments/i1 ")
        );
        assert_eq!(client.lyrics("a1"), Ok(None));
        assert!(
            requests
                .recv()
                .unwrap()
                .starts_with("GET /Audio/a1/Lyrics ")
        );
        assert!(!client.set_favorite("i1", false).unwrap().is_favorite);
        assert!(
            requests
                .recv()
                .unwrap()
                .starts_with("DELETE /UserFavoriteItems/i1 ")
        );
        client.logout().unwrap();
        let logout = requests.recv().unwrap();
        assert!(logout.starts_with("POST /Sessions/Logout "));
        assert_eq!(header(&logout, "content-length"), Some("0"));
    }
}
