//! Scan Network: Jellyfin servers answer a UDP broadcast of "who is
//! JellyfinServer?" on port 7359 with their id, name and address. Anything
//! on the network can answer, so what is found is only offered: the user
//! still connects to it, and Patina checks it then like a typed address.
//! For the same reason the answers are kept small: a few dozen servers at
//! most, names without control or direction characters, and addresses that
//! are http or https.
//!
//! The broadcast goes to 255.255.255.255, which leaves by the interface
//! of the default route only; servers on another interface's network
//! aren't found.

use serde::Deserialize;
use std::collections::HashSet;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};
use url::{Host, Url};

pub const PORT: u16 = 7359;
/// How long Scan Network listens for answers.
pub const SCAN_TIME: Duration = Duration::from_secs(2);
const QUESTION: &[u8] = b"who is JellyfinServer?";
/// No network has more servers than this, and one sender flooding the
/// scan with made-up ones gets no further.
const MAX_SERVERS: usize = 32;
/// A server's id is 32 hex digits; anything much longer is not one.
const MAX_ID: usize = 64;
/// Longer names are cut to this many characters.
const MAX_NAME: usize = 128;

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "PascalCase")]
struct Answer {
    address: String,
    id: String,
    name: String,
    endpoint_address: Option<String>,
}

/// A server that answered.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub id: String,
    pub name: String,
    /// The address the server gives for itself, when it is an http or
    /// https URL; otherwise empty. In a container it is often the
    /// container's own, which only its host can reach.
    pub address: String,
    /// The same address at the IP the answer came from, which did reach
    /// Patina; a port forwarded to another number is still not known.
    pub source_address: String,
    pub source: IpAddr,
    pub endpoint_address: Option<String>,
}

impl Found {
    /// The addresses to offer, the likelier to work first: the one at the
    /// answer's IP, except for an https address with a host name, whose
    /// certificate names the host and not the IP.
    pub fn addresses(&self) -> Vec<String> {
        let mut addresses = vec![self.source_address.clone()];
        if !self.address.is_empty() && self.address != self.source_address {
            let named_https = Url::parse(&self.address).is_ok_and(|url| {
                url.scheme() == "https" && matches!(url.host(), Some(Host::Domain(_)))
            });
            if named_https {
                addresses.insert(0, self.address.clone());
            } else {
                addresses.push(self.address.clone());
            }
        }
        addresses
    }
}

/// The servers on the local network that answer within `timeout`, each
/// once.
pub fn scan(timeout: Duration) -> std::io::Result<Vec<Found>> {
    scan_at(SocketAddr::from((Ipv4Addr::BROADCAST, PORT)), timeout)
}

/// Asks `target` (the broadcast address, or one server's) and collects
/// the answers until `timeout`, or until `MAX_SERVERS` have answered.
/// Jellyfin listens on IPv4 only.
pub fn scan_at(target: SocketAddr, timeout: Duration) -> std::io::Result<Vec<Found>> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    socket.send_to(QUESTION, target)?;
    let deadline = Instant::now() + timeout;
    let mut found: Vec<Found> = Vec::new();
    let mut ids: HashSet<String> = HashSet::new();
    let mut buffer = [0u8; 4096];
    while found.len() < MAX_SERVERS {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        socket.set_read_timeout(Some(left))?;
        match socket.recv_from(&mut buffer) {
            Ok((length, from)) => {
                if let Some(server) = parse(&buffer[..length], from.ip())
                    && ids.insert(server.id.clone())
                {
                    found.push(server);
                }
            }
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                break;
            }
            // Windows reports an earlier datagram's ICMP "unreachable" on
            // the next receive.
            Err(error) if error.kind() == ErrorKind::ConnectionReset => continue,
            Err(error) => {
                log::debug!("scanning for Jellyfin servers: {error}");
                break;
            }
        }
    }
    Ok(found)
}

fn parse(datagram: &[u8], source: IpAddr) -> Option<Found> {
    let answer: Answer = serde_json::from_slice(datagram).ok()?;
    let id = answer.id.trim();
    if id.is_empty() || id.len() > MAX_ID {
        return None;
    }
    let address = web_address(&answer.address).unwrap_or_default();
    Some(Found {
        source_address: at_source(&address, source),
        id: id.to_owned(),
        name: plain(&answer.name),
        address,
        source,
        endpoint_address: answer.endpoint_address.as_deref().and_then(web_address),
    })
}

/// An address an answer gave, as Patina would offer it: an http or https
/// URL with a host, without a trailing slash. Anything else (another
/// scheme, a broken URL) is not offered.
fn web_address(address: &str) -> Option<String> {
    let url = Url::parse(address.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none_or(str::is_empty) {
        return None;
    }
    Some(super::auth::display_address(&url))
}

/// A name as the list may show it: without control characters or those
/// that change the direction of the text around them, and not too long.
fn plain(name: &str) -> String {
    let direction = |c: char| {
        matches!(
            c,
            '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
        )
    };
    name.chars()
        .filter(|c| !c.is_control() && !direction(*c))
        .take(MAX_NAME)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// `address` with its host replaced by `source`: the scheme, port and
/// path the server reported, at the IP it answered from.
fn at_source(address: &str, source: IpAddr) -> String {
    let mut url = match Url::parse(address) {
        Ok(url) if url.has_host() && matches!(url.scheme(), "http" | "https") => url,
        _ => Url::parse("http://127.0.0.1:8096").expect("a valid URL"),
    };
    let _ = url.set_ip_host(source);
    super::auth::display_address(&url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_collected_once_each_with_where_they_came_from() {
        let responder = UdpSocket::bind("127.0.0.1:0").unwrap();
        let target = responder.local_addr().unwrap();
        let thread = std::thread::spawn(move || {
            let mut buffer = [0u8; 256];
            let (length, from) = responder.recv_from(&mut buffer).unwrap();
            assert_eq!(&buffer[..length], QUESTION);
            for answer in [
                r#"{"Address": "http://172.17.0.2:8096", "Id": "a1", "Name": "Den", "EndpointAddress": null}"#,
                r#"{"Address": "http://172.17.0.2:8096", "Id": "a1", "Name": "Den"}"#,
                r#"{"Address": "https://10.9.9.9:8920/jellyfin/", "Id": "b2", "Name": "Attic"}"#,
                r#"not json"#,
                r#"{"Address": "http://x", "Name": "No id"}"#,
            ] {
                responder.send_to(answer.as_bytes(), from).unwrap();
            }
        });
        let started = Instant::now();
        let found = scan_at(target, Duration::from_millis(500)).unwrap();
        thread.join().unwrap();
        assert!(started.elapsed() >= Duration::from_millis(450));
        assert_eq!(found.len(), 2, "{found:?}");
        let den = &found[0];
        assert_eq!(den.name, "Den");
        assert_eq!(den.address, "http://172.17.0.2:8096");
        assert_eq!(den.source, IpAddr::from([127, 0, 0, 1]));
        assert_eq!(den.source_address, "http://127.0.0.1:8096");
        assert_eq!(den.endpoint_address, None);
        assert_eq!(
            den.addresses(),
            ["http://127.0.0.1:8096", "http://172.17.0.2:8096"]
        );
        let attic = &found[1];
        assert_eq!(attic.address, "https://10.9.9.9:8920/jellyfin");
        assert_eq!(attic.source_address, "https://127.0.0.1:8920/jellyfin");
    }

    #[test]
    fn silence_ends_at_the_timeout() {
        let quiet = UdpSocket::bind("127.0.0.1:0").unwrap();
        let started = Instant::now();
        let found = scan_at(quiet.local_addr().unwrap(), Duration::from_millis(200)).unwrap();
        assert!(found.is_empty());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_flood_of_answers_is_cut_short() {
        let responder = UdpSocket::bind("127.0.0.1:0").unwrap();
        let target = responder.local_addr().unwrap();
        let thread = std::thread::spawn(move || {
            let mut buffer = [0u8; 256];
            let (_, from) = responder.recv_from(&mut buffer).unwrap();
            for index in 0..MAX_SERVERS * 4 {
                let answer = format!(r#"{{"Address": "http://10.0.0.1:8096", "Id": "id{index}"}}"#);
                responder.send_to(answer.as_bytes(), from).unwrap();
            }
        });
        let started = Instant::now();
        let found = scan_at(target, Duration::from_secs(5)).unwrap();
        thread.join().unwrap();
        assert_eq!(found.len(), MAX_SERVERS);
        // It stops listening once it has as many as it keeps.
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn answers_are_cleaned_before_they_are_offered() {
        let source = IpAddr::from([192, 168, 1, 5]);
        let parsed = |answer: &str| parse(answer.as_bytes(), source);
        let long_id = "a".repeat(MAX_ID + 1);
        assert_eq!(parsed(&format!(r#"{{"Id": "{long_id}"}}"#)), None);
        assert_eq!(parsed(r#"{"Id": "  "}"#), None);
        for broken in [&[0xff, 0xfe][..], b"", b"[[[[", br#"{"Id": 5}"#] {
            assert_eq!(parse(broken, source), None);
        }

        let named = parsed(&format!(
            r#"{{"Id": " a1 ", "Name": "\u202eDen\u0007 {}"}}"#,
            "x".repeat(400)
        ))
        .unwrap();
        assert_eq!(named.id, "a1");
        assert!(named.name.starts_with("Den x"), "{}", named.name);
        assert_eq!(named.name.chars().count(), MAX_NAME);

        // Only http and https addresses are offered.
        for address in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "http://[::1",
            "http://",
        ] {
            let found = parsed(&format!(
                r#"{{"Id": "b2", "Address": "{address}", "EndpointAddress": "{address}"}}"#
            ))
            .unwrap();
            assert_eq!(found.address, "", "{address}");
            assert_eq!(found.endpoint_address, None, "{address}");
            assert_eq!(found.addresses(), ["http://192.168.1.5:8096"], "{address}");
        }
    }

    #[test]
    fn an_https_address_with_a_name_is_offered_first() {
        let source = IpAddr::from([192, 168, 1, 5]);
        let found = |address: &str| {
            parse(
                format!(r#"{{"Id": "a1", "Address": "{address}"}}"#).as_bytes(),
                source,
            )
            .unwrap()
            .addresses()
        };
        // Its certificate names the host, not the IP.
        assert_eq!(
            found("https://media.example.org:8920"),
            ["https://media.example.org:8920", "https://192.168.1.5:8920"]
        );
        assert_eq!(
            found("https://10.0.0.2:8920"),
            ["https://192.168.1.5:8920", "https://10.0.0.2:8920"]
        );
        assert_eq!(
            found("http://nas.local:8096"),
            ["http://192.168.1.5:8096", "http://nas.local:8096"]
        );
    }

    #[test]
    fn an_unreadable_address_falls_back_to_the_source() {
        assert_eq!(
            at_source("", IpAddr::from([192, 168, 1, 5])),
            "http://192.168.1.5:8096"
        );
        assert_eq!(
            at_source("http://172.17.0.2", IpAddr::from([192, 168, 1, 5])),
            "http://192.168.1.5"
        );
    }
}
