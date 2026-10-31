//! Who Patina says it is to a server, and where the server is: the
//! `Authorization` header every request carries, the address the user
//! typed made into a base URL, and the URLs built on it.

use super::api::Error;
use url::Url;

/// The client's name, which the server shows in its dashboard and keys
/// sessions by.
pub const CLIENT: &str = "Patina";

/// The installation, as the header names it. `device` and `version` must
/// stay the same from one request to the next: when either changes, the
/// server overwrites what it stored for the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub device: String,
    /// Made once per configuration: signing in again on the same device
    /// id revokes the account's other tokens for it.
    pub device_id: String,
    pub version: String,
}

impl Identity {
    pub fn new(device: &str, device_id: &str) -> Self {
        Self {
            device: device.to_owned(),
            device_id: device_id.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    /// The `Authorization` header's value. The server reads the keys case
    /// by case, so they are spelled exactly so, and URL-decodes the
    /// values, so they are percent-encoded: a "+" would otherwise become
    /// a space.
    pub fn header(&self, token: Option<&str>) -> String {
        let mut header = format!(
            "MediaBrowser Client=\"{}\", Device=\"{}\", DeviceId=\"{}\", Version=\"{}\"",
            encode(CLIENT),
            encode(&self.device),
            encode(&self.device_id),
            encode(&self.version),
        );
        if let Some(token) = token.filter(|token| !token.is_empty()) {
            header.push_str(&format!(", Token=\"{}\"", encode(token)));
        }
        header
    }
}

/// Percent-encodes all but the unreserved characters, which neither a
/// URL decoder nor the header's quoting changes.
pub fn encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// What the user typed as a server's address, as the base URL requests
/// are made under: https when no scheme is given, port 8096 (Jellyfin's
/// own) for http without a port, and any path kept, since a server behind
/// a reverse proxy often lives under one. The query, the fragment and a
/// trailing slash go.
pub fn normalize_address(input: &str) -> Result<Url, Error> {
    let input = input.trim();
    if input.is_empty() {
        return Err(Error::Address("Enter the server's address.".into()));
    }
    // Without "://", "host:8096" would read as the scheme "host".
    let (text, rest) = match input.find("://") {
        Some(at) => (input.to_owned(), &input[at + 3..]),
        None => (format!("https://{input}"), input),
    };
    let mut url = Url::parse(&text)
        .map_err(|error| Error::Address(format!("“{input}” is not a web address ({error}).")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Error::Address(format!(
            "Jellyfin servers are reached over http or https, not {}.",
            url.scheme()
        )));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(Error::Address(format!("“{input}” has no host name.")));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::Address(
            "Leave the name and password out of the address: Patina asks for them when signing in."
                .into(),
        ));
    }
    // The url crate forgets a default port ("http://host:80" has none),
    // so whether one was typed is read from the text.
    if url.scheme() == "http" && !has_port(rest) {
        let _ = url.set_port(Some(8096));
    }
    url.set_query(None);
    url.set_fragment(None);
    let mut path = String::new();
    for segment in url.path().split('/').filter(|segment| !segment.is_empty()) {
        path.push('/');
        path.push_str(segment);
    }
    url.set_path(&path);
    Ok(url)
}

/// Whether the authority at the start of `rest` (what follows "://")
/// names a port.
fn has_port(rest: &str) -> bool {
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    // After an IPv6 address's closing bracket, or anywhere in a name.
    let after_host = match host_port.rfind(']') {
        Some(bracket) => &host_port[bracket + 1..],
        None => host_port,
    };
    after_host.contains(':')
}

/// The address as Patina shows and keeps it: without the slash the url
/// crate puts after a bare host.
pub fn display_address(url: &Url) -> String {
    url.as_str().trim_end_matches('/').to_owned()
}

/// An API path under the server's base URL, keeping the base's own path:
/// "/Items" under "https://host/jellyfin" is "https://host/jellyfin/Items".
/// A query in `path` is kept as it is.
pub fn api_url(base: &Url, path: &str) -> Url {
    let (path, query) = match path.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (path, None),
    };
    let mut url = base.clone();
    url.set_query(None);
    url.set_fragment(None);
    url.set_path(&format!(
        "{}/{}",
        base.path().trim_end_matches('/'),
        path.trim_start_matches('/')
    ));
    url.set_query(query.filter(|query| !query.is_empty()));
    url
}

/// A URL the server handed out: relative ones (`TranscodingUrl`, a
/// subtitle's `DeliveryUrl`) are under the base URL, absolute ones stay as
/// they are.
pub fn resolve(base: &Url, url: &str) -> Url {
    match Url::parse(url) {
        Ok(absolute) => absolute,
        Err(_) => api_url(base, url),
    }
}

/// Whether `url` is on the server at `base`: same scheme, host and port.
/// Only such URLs may carry the server's token.
pub fn same_server(base: &Url, url: &Url) -> bool {
    base.scheme() == url.scheme()
        && base.host_str() == url.host_str()
        && base.port_or_known_default() == url.port_or_known_default()
}

/// A URL without the token the server writes into the URLs it hands out
/// (`ApiKey`, or the legacy `api_key`): the header carries it instead, and
/// a URL is kept and shown in places a token must not be. Everything else
/// is left byte for byte, since an HLS playlist passes its own query on to
/// the URLs in it. Works on relative URLs too.
pub fn strip_api_key(url: &str) -> String {
    let (before_fragment, fragment) = match url.split_once('#') {
        Some((before, fragment)) => (before, Some(fragment)),
        None => (url, None),
    };
    let Some((path, query)) = before_fragment.split_once('?') else {
        return url.to_owned();
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            let name = pair.split('=').next().unwrap_or_default();
            !name.is_empty()
                && !name.eq_ignore_ascii_case("apikey")
                && !name.eq_ignore_ascii_case("api_key")
        })
        .collect();
    let mut stripped = path.to_owned();
    if !kept.is_empty() {
        stripped.push('?');
        stripped.push_str(&kept.join("&"));
    }
    if let Some(fragment) = fragment {
        stripped.push('#');
        stripped.push_str(fragment);
    }
    stripped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalized(input: &str) -> String {
        match normalize_address(input) {
            Ok(url) => url.to_string(),
            Err(error) => panic!("{input}: {error}"),
        }
    }

    #[test]
    fn the_header_spells_the_keys_and_encodes_the_values() {
        let identity = Identity {
            device: "Patina on Linux".into(),
            device_id: "0123abcd".into(),
            version: "1.2.3+dev".into(),
        };
        assert_eq!(
            identity.header(None),
            "MediaBrowser Client=\"Patina\", Device=\"Patina%20on%20Linux\", \
             DeviceId=\"0123abcd\", Version=\"1.2.3%2Bdev\""
        );
        assert_eq!(
            identity.header(Some("a\"b,c")),
            "MediaBrowser Client=\"Patina\", Device=\"Patina%20on%20Linux\", \
             DeviceId=\"0123abcd\", Version=\"1.2.3%2Bdev\", Token=\"a%22b%2Cc\""
        );
        assert_eq!(identity.header(Some("")), identity.header(None));
        assert_eq!(Identity::new("d", "i").version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn addresses_get_a_scheme_a_port_and_lose_the_trailing_slash() {
        assert_eq!(
            normalized("media.example.org"),
            "https://media.example.org/"
        );
        assert_eq!(normalized(" http://nas.local "), "http://nas.local:8096/");
        assert_eq!(normalized("http://nas.local:80"), "http://nas.local/");
        assert_eq!(normalized("nas.local:8920"), "https://nas.local:8920/");
        assert_eq!(normalized("https://nas.local:443/"), "https://nas.local/");
        assert_eq!(
            normalized("https://example.org/jellyfin/"),
            "https://example.org/jellyfin"
        );
        assert_eq!(
            normalized("HTTP://Example.org//a//b/?x=1#frag"),
            "http://example.org:8096/a/b"
        );
        assert_eq!(
            normalized("192.168.1.20:8096"),
            "https://192.168.1.20:8096/"
        );
        assert_eq!(normalized("http://[::1]"), "http://[::1]:8096/");
        assert_eq!(normalized("http://[::1]:9000/jf"), "http://[::1]:9000/jf");
        assert_eq!(
            display_address(&normalize_address("https://example.org/").unwrap()),
            "https://example.org"
        );
    }

    #[test]
    fn garbage_is_refused_with_a_reason() {
        for input in [
            "",
            "   ",
            "ftp://example.org",
            "my server",
            "http://",
            "https://user:pw@example.org",
            "http://example.org:99999",
        ] {
            match normalize_address(input) {
                Err(Error::Address(reason)) => assert!(!reason.is_empty(), "{input}"),
                other => panic!("{input}: {other:?}"),
            }
        }
    }

    #[test]
    fn api_paths_go_under_the_base_path() {
        let root = normalize_address("https://example.org").unwrap();
        let nested = normalize_address("https://example.org/jellyfin").unwrap();
        assert_eq!(
            api_url(&root, "/System/Info/Public").as_str(),
            "https://example.org/System/Info/Public"
        );
        assert_eq!(
            api_url(&nested, "/System/Info/Public").as_str(),
            "https://example.org/jellyfin/System/Info/Public"
        );
        assert_eq!(
            api_url(&nested, "videos/1/master.m3u8?DeviceId=x&MediaSourceId=y").as_str(),
            "https://example.org/jellyfin/videos/1/master.m3u8?DeviceId=x&MediaSourceId=y"
        );
        assert_eq!(
            resolve(&nested, "/Videos/1/ms/Subtitles/2/0/Stream.srt").as_str(),
            "https://example.org/jellyfin/Videos/1/ms/Subtitles/2/0/Stream.srt"
        );
        let elsewhere = resolve(&nested, "https://subs.example.com/a.srt");
        assert_eq!(elsewhere.as_str(), "https://subs.example.com/a.srt");
        assert!(!same_server(&nested, &elsewhere));
        assert!(same_server(&nested, &api_url(&nested, "/Items")));
        assert!(same_server(
            &root,
            &Url::parse("https://example.org:443/x").unwrap()
        ));
        assert!(!same_server(
            &root,
            &Url::parse("http://example.org/x").unwrap()
        ));
    }

    #[test]
    fn api_keys_come_out_of_urls_and_nothing_else_changes() {
        assert_eq!(
            strip_api_key(
                "/videos/1/master.m3u8?DeviceId=d&MediaSourceId=m&VideoCodec=h264%2Chevc&ApiKey=secret&TranscodeReasons=x"
            ),
            "/videos/1/master.m3u8?DeviceId=d&MediaSourceId=m&VideoCodec=h264%2Chevc&TranscodeReasons=x"
        );
        assert_eq!(
            strip_api_key("/Videos/1/m/Subtitles/0/0/Stream.srt?ApiKey=secret"),
            "/Videos/1/m/Subtitles/0/0/Stream.srt"
        );
        assert_eq!(
            strip_api_key("https://h/a?api_key=s&apikey=t&b=1#top"),
            "https://h/a?b=1#top"
        );
        assert_eq!(strip_api_key("/no/query"), "/no/query");
        assert_eq!(strip_api_key("/a?ApiKeyName=1"), "/a?ApiKeyName=1");
    }
}
