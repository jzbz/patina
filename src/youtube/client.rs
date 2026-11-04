//! Plain requests to YouTube's own hosts, and only those.
//!
//! Redirects are followed here rather than by the HTTP library, so that each
//! hop is checked against the list of hosts. Short limits apply throughout,
//! since nothing in the section may wait long on one answer.
//!
//! The requests go through the same proxy as yt-dlp
//! (`Preferences::proxy`). A proxy that can't be used is an error: someone who
//! set one for their privacy must not have their subscription list sent direct.

use super::{Error, Failure};
use std::time::Duration;
use url::Url;

/// Where a channel's feed is.
const FEED_HOSTS: &[&str] = &["www.youtube.com"];
/// Where thumbnails and channel pictures are.
const IMAGE_HOSTS: &[&str] = &["i.ytimg.com", "yt3.googleusercontent.com", "yt3.ggpht.com"];
const TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_REDIRECTS: usize = 3;
/// A feed is 20 to 45 KB.
const FEED_LIMIT: u64 = 2 << 20;
const IMAGE_LIMIT: u64 = 8 << 20;

/// An image host's address: whether it may be fetched at all. A channel's
/// picture is an address yt-dlp passes on, so this is asked before keeping it.
pub fn is_image_url(url: &Url) -> bool {
    allowed(url, IMAGE_HOSTS, true)
}

/// HTTPS on the default port to a listed host, with no user name in it.
fn allowed(url: &Url, hosts: &[&str], https: bool) -> bool {
    let scheme = if https {
        url.scheme() == "https" && url.port().is_none()
    } else {
        url.scheme() == "http"
    };
    scheme
        && url.username().is_empty()
        && url.password().is_none()
        && url.host_str().is_some_and(|host| hosts.contains(&host))
}

/// The next picture to try when a video's has a 404, largest first. `hqdefault`
/// is 4:3 with black bars, but exists for videos that have nothing else.
pub fn smaller_thumbnail(url: &str) -> Option<String> {
    let (folder, name) = url.rsplit_once('/')?;
    let next = match name {
        "hq720.jpg" => "mqdefault.jpg",
        "mqdefault.jpg" => "hqdefault.jpg",
        _ => return None,
    };
    Some(format!("{folder}/{next}"))
}

/// Fetches `url`, or the next smaller thumbnail for as long as there is a 404.
fn with_smaller_thumbnails(
    url: &str,
    fetch: impl Fn(&str) -> Result<Vec<u8>, Error>,
) -> Result<Vec<u8>, Error> {
    let mut url = url.to_owned();
    loop {
        match fetch(&url) {
            Err(error) if error.detail == "HTTP 404" => match smaller_thumbnail(&url) {
                Some(next) => url = next,
                None => return Err(error),
            },
            answer => return answer,
        }
    }
}

/// An answer, whatever its status.
#[derive(Debug)]
pub struct Answer {
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
}

impl Client {
    /// `proxy` is as `Preferences::proxy` gives it: None leaves the system's
    /// proxy settings to apply, as they do for yt-dlp, and empty goes direct.
    pub fn new(proxy: Option<&str>) -> Result<Self, Error> {
        let tls = ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let mut config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(TIMEOUT))
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .user_agent(format!("Patina/{}", env!("CARGO_PKG_VERSION")))
            .max_redirects(0)
            .tls_config(tls);
        match proxy {
            None => {}
            Some("") => config = config.proxy(None),
            Some(proxy) => {
                // Its error could quote the address, with a password in it.
                let proxy = ureq::Proxy::new(proxy)
                    .map_err(|_| Error::new(Failure::Proxy, "not a proxy address"))?;
                config = config.proxy(Some(proxy));
            }
        }
        Ok(Self {
            agent: config.build().into(),
        })
    }

    /// A channel's feed page.
    pub fn feed(&self, url: &str) -> Result<Answer, Error> {
        self.get(url, FEED_HOSTS, true, FEED_LIMIT)
    }

    /// A picture's bytes. Anything but a 2xx is an error, so that an error
    /// page, which YouTube sends with a picture in it, is never kept.
    pub fn image(&self, url: &str) -> Result<Vec<u8>, Error> {
        let answer = self.get(url, IMAGE_HOSTS, true, IMAGE_LIMIT)?;
        if (200..300).contains(&answer.status) {
            Ok(answer.body)
        } else {
            Err(Error::new(
                Failure::Failed,
                format!("HTTP {}", answer.status),
            ))
        }
    }

    /// The image cache's way of fetching, which falls back to a smaller
    /// picture when YouTube has none of the size asked for.
    pub fn image_fetch(self) -> crate::jellyfin::images::Fetch {
        std::sync::Arc::new(move |url: &str| {
            with_smaller_thumbnails(url, |url| self.image(url)).map_err(|error| error.to_string())
        })
    }

    fn get(&self, url: &str, hosts: &[&str], https: bool, limit: u64) -> Result<Answer, Error> {
        let refused = || Error::new(Failure::NotFeed, "an address off YouTube");
        let mut url = Url::parse(url).map_err(|_| refused())?;
        for _ in 0..=MAX_REDIRECTS {
            if !allowed(&url, hosts, https) {
                return Err(refused());
            }
            let response = self.agent.get(url.as_str()).call().map_err(request_error)?;
            let status = response.status().as_u16();
            if (300..400).contains(&status) {
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|location| location.to_str().ok())
                    .ok_or_else(refused)?;
                url = url.join(location).map_err(|_| refused())?;
                continue;
            }
            let body = response
                .into_body()
                .with_config()
                .limit(limit)
                .read_to_vec()
                .map_err(request_error)?;
            return Ok(Answer { status, body });
        }
        Err(Error::new(Failure::NotFeed, "too many redirects"))
    }
}

/// A failed request by name. The library's own text can quote addresses, and
/// through them a proxy's password, so it isn't passed on.
fn request_error(error: ureq::Error) -> Error {
    match error {
        ureq::Error::Timeout(_) => Failure::Timeout.into(),
        ureq::Error::ConnectProxyFailed(_) | ureq::Error::InvalidProxyUrl => Failure::Proxy.into(),
        ureq::Error::BodyExceedsLimit(_) => Error::new(Failure::NotFeed, "too large an answer"),
        ureq::Error::HostNotFound => Error::new(Failure::Network, "host not found"),
        ureq::Error::ConnectionFailed => Error::new(Failure::Network, "connection failed"),
        ureq::Error::Io(error) => Error::new(Failure::Network, error.kind().to_string()),
        _ => Failure::Network.into(),
    }
}

#[cfg(test)]
pub mod testing {
    //! A little HTTP server on this machine, so that no test reaches YouTube.

    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    /// Answers each request with the next of `answers` (the whole response,
    /// head and all), and notes the request lines it was sent.
    pub struct Server {
        pub port: u16,
        pub requests: Arc<Mutex<Vec<String>>>,
    }

    fn answer(mut stream: TcpStream, text: &str, requests: &Mutex<Vec<String>>) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        requests.lock().unwrap().push(line.trim_end().to_owned());
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
                break;
            }
        }
        let _ = stream.write_all(text.as_bytes());
        let _ = stream.flush();
        let _ = stream.shutdown(Shutdown::Both);
    }

    impl Server {
        pub fn start(answers: Vec<String>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let noted = requests.clone();
            std::thread::spawn(move || {
                for text in answers {
                    let Ok((stream, _)) = listener.accept() else {
                        return;
                    };
                    answer(stream, &text, &noted);
                }
            });
            Self { port, requests }
        }

        pub fn seen(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }

        pub fn url(&self, path: &str) -> String {
            format!("http://127.0.0.1:{}{path}", self.port)
        }
    }

    pub fn response(status: u16, extra: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",
            body.len()
        )
    }

    impl Client {
        /// `feed`, for this machine rather than YouTube.
        pub fn local(&self, url: &str) -> Result<Answer, Error> {
            self.get(url, &["127.0.0.1"], false, FEED_LIMIT)
        }

        /// The same, under a limit of the test's.
        pub fn local_limited(&self, url: &str, limit: u64) -> Result<Answer, Error> {
            self.get(url, &["127.0.0.1"], false, limit)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn only_listed_hosts_over_https_are_allowed() {
        for good in [
            "https://www.youtube.com/feeds/videos.xml?channel_id=x",
            "https://www.youtube.com:443/feeds",
        ] {
            assert!(allowed(&url(good), FEED_HOSTS, true), "{good}");
        }
        for bad in [
            "http://www.youtube.com/feeds",
            "https://www.youtube.com:8443/feeds",
            "https://youtube.com/feeds",
            "https://www.youtube.com.evil.example/feeds",
            "https://evil.example/www.youtube.com",
            "https://www.youtube.com@evil.example/feeds",
            "https://user@www.youtube.com/feeds",
            "https://user:pw@www.youtube.com/feeds",
            "ftp://www.youtube.com/feeds",
            "file:///etc/passwd",
        ] {
            assert!(!allowed(&url(bad), FEED_HOSTS, true), "{bad}");
        }
        assert!(is_image_url(&url("https://i.ytimg.com/vi/x/mqdefault.jpg")));
        assert!(is_image_url(&url("https://yt3.ggpht.com/x=s88")));
        assert!(!is_image_url(&url("https://evil.example/x.jpg")));
        assert!(!is_image_url(&url("https://www.youtube.com/x.jpg")));
    }

    /// What docs/youtube.md says a plain request carries. A header that shows up
    /// here, after a change to the HTTP library say, belongs in that page first.
    #[test]
    fn a_request_says_who_is_asking_and_nothing_of_the_user() {
        use std::io::{BufRead, BufReader, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let reading = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut headers = Vec::new();
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                match line.trim().split_once(':') {
                    Some((name, value)) => {
                        headers.push((name.to_lowercase(), value.trim().to_owned()));
                    }
                    None => break,
                }
            }
            stream.write_all(response(200, "", "").as_bytes()).unwrap();
            headers
        });
        let client = Client::new(Some("")).unwrap();
        client
            .local(&format!("http://127.0.0.1:{port}/feed"))
            .unwrap();
        let mut headers = reading.join().unwrap();
        headers.sort();
        let names: Vec<&str> = headers.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["accept", "host", "user-agent"]);
        assert_eq!(
            headers[2].1,
            format!("Patina/{}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn a_feed_off_youtube_is_never_requested() {
        let client = Client::new(Some("")).unwrap();
        for bad in [
            "https://evil.example/",
            "http://www.youtube.com/",
            "not a url",
        ] {
            let error = client.feed(bad).unwrap_err();
            assert_eq!(error.kind, Failure::NotFeed, "{bad}");
        }
        let error = client.image("https://evil.example/x.jpg").unwrap_err();
        assert_eq!(error.kind, Failure::NotFeed);
    }

    #[test]
    fn redirects_are_followed_on_the_list_and_refused_off_it() {
        let last = Server::start(vec![response(200, "", "fine")]);
        let first = Server::start(vec![response(
            302,
            &format!("Location: {}\r\n", last.url("/last")),
            "",
        )]);
        let client = Client::new(Some("")).unwrap();
        let answer = client.local(&first.url("/first")).unwrap();
        assert_eq!((answer.status, answer.body.as_slice()), (200, &b"fine"[..]));
        assert_eq!(last.seen(), ["GET /last HTTP/1.1"]);

        let off = Server::start(vec![response(
            302,
            "Location: https://evil.example/\r\n",
            "",
        )]);
        let error = client.local(&off.url("/")).unwrap_err();
        assert_eq!(error.kind, Failure::NotFeed);

        let no_location = Server::start(vec![response(302, "", "")]);
        let error = client.local(&no_location.url("/")).unwrap_err();
        assert_eq!(error.kind, Failure::NotFeed);
    }

    #[test]
    fn a_redirect_loop_ends() {
        let server = Server::start(vec![
            response(302, "Location: /a\r\n", ""),
            response(302, "Location: /a\r\n", ""),
            response(302, "Location: /a\r\n", ""),
            response(302, "Location: /a\r\n", ""),
            response(200, "", "too far"),
        ]);
        let error = Client::new(Some(""))
            .unwrap()
            .local(&server.url("/a"))
            .unwrap_err();
        assert_eq!(error.kind, Failure::NotFeed);
        assert_eq!(server.seen().len(), MAX_REDIRECTS + 1);
    }

    #[test]
    fn statuses_come_back_as_they_are() {
        let server = Server::start(vec![response(404, "", "<!DOCTYPE html>")]);
        let answer = Client::new(Some(""))
            .unwrap()
            .local(&server.url("/"))
            .unwrap();
        assert_eq!(answer.status, 404);
    }

    #[test]
    fn a_huge_answer_is_cut_off() {
        let server = Server::start(vec![response(200, "", &"x".repeat(5000))]);
        let error = Client::new(Some(""))
            .unwrap()
            .local_limited(&server.url("/"), 1000)
            .unwrap_err();
        assert_eq!(error.kind, Failure::NotFeed);
    }

    #[test]
    fn a_refused_connection_is_a_network_failure() {
        let server = Server::start(Vec::new());
        // Nothing is accepted past the empty list, and the listener is gone.
        std::thread::sleep(Duration::from_millis(50));
        let error = Client::new(Some(""))
            .unwrap()
            .local(&server.url("/"))
            .unwrap_err();
        assert_eq!(error.kind, Failure::Network);
    }

    #[test]
    fn requests_go_through_the_proxy_and_never_direct() {
        // The target would answer, if anything reached it.
        let target = Server::start(vec![response(200, "", "direct")]);
        let proxy = Server::start(vec![response(502, "", "")]);
        let client = Client::new(Some(&proxy.url(""))).unwrap();
        let error = client.local(&target.url("/x")).unwrap_err();
        assert_eq!(error.kind, Failure::Proxy);
        assert!(target.seen().is_empty(), "the request went direct");
        let seen = proxy.seen();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].starts_with("CONNECT 127.0.0.1:"), "{seen:?}");
    }

    #[test]
    fn a_dead_proxy_does_not_fall_back_to_direct() {
        let target = Server::start(vec![response(200, "", "direct")]);
        let dead = Server::start(Vec::new());
        std::thread::sleep(Duration::from_millis(50));
        let client = Client::new(Some(&dead.url(""))).unwrap();
        assert!(client.local(&target.url("/x")).is_err());
        assert!(target.seen().is_empty(), "the request went direct");
    }

    #[test]
    fn a_bad_proxy_address_is_an_error_not_a_direct_connection() {
        let error = Client::new(Some("http://user:secret@bad host:port"))
            .err()
            .expect("a bad address");
        assert_eq!(error.kind, Failure::Proxy);
        assert!(!format!("{error:?} {error}").contains("secret"));
    }

    #[test]
    fn proxy_failures_do_not_quote_the_address() {
        let dead = Server::start(Vec::new());
        std::thread::sleep(Duration::from_millis(50));
        let proxy = format!("http://user:secret@127.0.0.1:{}", dead.port);
        let client = Client::new(Some(&proxy)).unwrap();
        let error = client.local("http://127.0.0.1:9/x").unwrap_err();
        assert!(!format!("{error:?} {error}").contains("secret"));
    }

    #[test]
    fn a_thumbnail_with_a_404_falls_back_to_a_smaller_one_and_nothing_else_does() {
        let tried = std::cell::RefCell::new(Vec::new());
        let answer = with_smaller_thumbnails("https://i.ytimg.com/vi/x/hq720.jpg", |url| {
            tried.borrow_mut().push(url.to_owned());
            if url.ends_with("/hqdefault.jpg") {
                Ok(vec![1])
            } else {
                Err(Error::new(Failure::Failed, "HTTP 404"))
            }
        });
        assert_eq!(answer.unwrap(), [1]);
        assert_eq!(
            tried.into_inner(),
            [
                "https://i.ytimg.com/vi/x/hq720.jpg",
                "https://i.ytimg.com/vi/x/mqdefault.jpg",
                "https://i.ytimg.com/vi/x/hqdefault.jpg",
            ]
        );

        // Any other failure says nothing about the size, and there is no more
        // to try after the smallest or for a channel's picture.
        let tried = std::cell::Cell::new(0);
        let answer = with_smaller_thumbnails("https://i.ytimg.com/vi/x/hq720.jpg", |_| {
            tried.set(tried.get() + 1);
            Err(Error::new(Failure::Failed, "HTTP 500"))
        });
        assert!(answer.is_err());
        assert_eq!(tried.get(), 1);
        assert_eq!(
            smaller_thumbnail("https://i.ytimg.com/vi/x/hqdefault.jpg"),
            None
        );
        assert_eq!(
            smaller_thumbnail("https://yt3.googleusercontent.com/picture=s176"),
            None
        );
    }
}
