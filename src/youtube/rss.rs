//! A channel's public feed: its 15 newest uploads, with exact dates, and with
//! Shorts told apart by their links. It gives no length and says nothing about
//! live streams.
//!
//! The feed server is unreliable. For hours at a time it answers most requests
//! with 404 or 500 and an HTML page, for channels that are fine, so such an
//! answer never means anything about the channel: it is `Failure::NotFeed`, and
//! `feed.rs` asks yt-dlp instead.

use super::client::{Answer, Client};
use super::feed::Source;
use super::{
    Cancel, ChannelId, ChannelInfo, Error, Failure, Listing, Published, Video, VideoId, clean_text,
    plausible_time,
};

const ATOM: &str = "http://www.w3.org/2005/Atom";
const YOUTUBE: &str = "http://www.youtube.com/xml/schemas/2015";
const MEDIA: &str = "http://search.yahoo.com/mrss/";

/// Asks the feed server for each channel in turn, a few at once.
pub struct RssSource {
    /// A proxy that can't be used makes every answer that failure.
    client: Result<Client, Error>,
}

impl RssSource {
    pub fn new(proxy: Option<&str>) -> Self {
        Self {
            client: Client::new(proxy),
        }
    }
}

impl Source for RssSource {
    fn workers(&self) -> usize {
        3
    }

    fn fetch(&self, channels: &[ChannelId], cancel: &Cancel) -> Vec<Result<Listing, Error>> {
        channels
            .iter()
            .map(|channel| {
                if cancel.cancelled() {
                    return Err(Failure::Cancelled.into());
                }
                let client = self.client.as_ref().map_err(Clone::clone)?;
                read(client.feed(&channel.feed_url())?, channel)
            })
            .collect()
    }
}

/// A feed server's answer as a listing, or why it isn't one.
fn read(answer: Answer, channel: &ChannelId) -> Result<Listing, Error> {
    if answer.status != 200 {
        return Err(Error::new(
            Failure::NotFeed,
            format!("HTTP {}", answer.status),
        ));
    }
    parse(&answer.body, channel)
}

fn time(text: &str) -> Option<i64> {
    let time: jiff::Timestamp = text.trim().parse().ok()?;
    plausible_time(time.as_second())
}

/// A feed's bytes as a listing. Only videos of `channel` are taken, by what the
/// entries say and whatever the feed's own heading says, and only with a good
/// id. An entry that doesn't qualify is dropped; a document that isn't a feed
/// is `Failure::NotFeed`. The XML parser refuses a document type declaration,
/// and with it entity tricks.
pub fn parse(bytes: &[u8], channel: &ChannelId) -> Result<Listing, Error> {
    let not_feed = |detail: &str| Error::new(Failure::NotFeed, detail);
    let text = std::str::from_utf8(bytes).map_err(|_| not_feed("not text"))?;
    let document = roxmltree::Document::parse(text).map_err(|_| not_feed("not XML"))?;
    let root = document.root_element();
    if !root.has_tag_name((ATOM, "feed")) {
        return Err(not_feed("not a feed"));
    }
    let child = |node: roxmltree::Node<'_, '_>, space: &str, name: &str| {
        node.children()
            .find(|child| child.has_tag_name((space, name)))
            .and_then(|child| child.text())
            .map(str::to_owned)
    };
    let title = root
        .children()
        .find(|node| node.has_tag_name((ATOM, "author")))
        .and_then(|author| child(author, ATOM, "name"))
        .or_else(|| child(root, ATOM, "title"))
        .map(|title| clean_text(&title))
        .unwrap_or_default();
    let mut videos = Vec::new();
    for entry in root
        .children()
        .filter(|node| node.has_tag_name((ATOM, "entry")))
    {
        let Some(id) = child(entry, YOUTUBE, "videoId").and_then(|id| VideoId::parse(id.trim()))
        else {
            continue;
        };
        let owner = child(entry, YOUTUBE, "channelId").and_then(|id| ChannelId::parse(id.trim()));
        if owner.as_ref() != Some(channel) {
            continue;
        }
        let shorts = entry
            .children()
            .filter(|node| node.has_tag_name((ATOM, "link")))
            .filter_map(|link| link.attribute("href"))
            .any(|href| href.contains("/shorts/"));
        let group = entry
            .children()
            .find(|node| node.has_tag_name((MEDIA, "group")));
        let views = group
            .and_then(|group| {
                group
                    .children()
                    .find(|node| node.has_tag_name((MEDIA, "community")))
            })
            .and_then(|community| {
                community
                    .children()
                    .find(|node| node.has_tag_name((MEDIA, "statistics")))
            })
            .and_then(|statistics| statistics.attribute("views"))
            .and_then(|views| views.parse().ok());
        videos.push(Video {
            title: child(entry, ATOM, "title")
                .map(|title| clean_text(&title))
                .unwrap_or_default(),
            channel_id: Some(channel.clone()),
            channel: title.clone(),
            published: child(entry, ATOM, "published")
                .and_then(|text| time(&text))
                .map(|at| Published { at, exact: true }),
            views,
            shorts: Some(shorts),
            ..Video::new(id)
        });
    }
    Ok(Listing {
        channel: Some(ChannelInfo {
            title,
            ..ChannelInfo::new(channel.clone())
        }),
        videos,
    })
}

#[cfg(test)]
mod tests {
    use super::super::client::testing::*;
    use super::*;

    const FEED: &[u8] = include_bytes!("fixtures/feed.xml");
    const OWNER: &str = "UCfixturefixturefixture0";

    fn owner() -> ChannelId {
        ChannelId::parse(OWNER).unwrap()
    }

    #[test]
    fn a_feed_reads() {
        let listing = parse(FEED, &owner()).unwrap();
        let info = listing.channel.unwrap();
        assert_eq!(info.id, owner());
        assert_eq!(info.title, "Fixture Channel");
        let ids: Vec<&str> = listing
            .videos
            .iter()
            .map(|video| video.id.as_str())
            .collect();
        assert_eq!(ids, ["-ideoFixtur", "shortFixtur", "oldFixtureV"]);

        let first = &listing.videos[0];
        assert_eq!(first.title, "A video, with a comma & an entity");
        assert_eq!(first.channel, "Fixture Channel");
        assert_eq!(first.channel_id, Some(owner()));
        assert_eq!(
            first.published,
            Some(Published {
                at: 1_793_594_400,
                exact: true,
            })
        );
        assert_eq!(first.views, Some(1234));
        assert_eq!(first.shorts, Some(false));
        assert_eq!(first.duration, None);

        assert_eq!(listing.videos[1].shorts, Some(true));
        // A bad date or count loses that, not the entry.
        assert_eq!(listing.videos[2].published, None);
        assert_eq!(listing.videos[2].views, None);
    }

    #[test]
    fn entries_with_bad_ids_or_another_owner_are_dropped() {
        // The fixture holds one of each, past the three that read.
        let text = String::from_utf8_lossy(FEED);
        assert!(text.contains("bad/id"));
        assert!(text.contains("UCsomeoneelsesomeoneelse"));
        let listing = parse(FEED, &owner()).unwrap();
        assert_eq!(listing.videos.len(), 3);
        // Asked for another channel, the same document gives only that one's.
        let other = ChannelId::parse("UCsomeoneelsesomeoneelse").unwrap();
        let listing = parse(FEED, &other).unwrap();
        assert_eq!(listing.videos.len(), 1);
        assert_eq!(listing.videos[0].id.as_str(), "otherChanne");
    }

    #[test]
    fn a_feed_with_no_entries_is_still_a_feed() {
        let empty = br#"<feed xmlns="http://www.w3.org/2005/Atom"><title>Empty</title></feed>"#;
        let listing = parse(empty, &owner()).unwrap();
        assert!(listing.videos.is_empty());
        assert_eq!(listing.channel.unwrap().title, "Empty");
    }

    #[test]
    fn anything_else_is_not_a_feed() {
        for bad in [
            &b""[..],
            b"not xml",
            b"\xff\xfe\x00",
            b"<!DOCTYPE html>\n<html lang=en><title>Error 404 (Not Found)!!1</title>",
            b"<html><body>Sorry</body></html>",
            // The right name in no namespace is not a feed.
            b"<feed><entry/></feed>",
            b"<feed xmlns=\"http://www.w3.org/2005/Atom\"><entry>",
            include_bytes!("fixtures/feed-entities.xml"),
        ] {
            let error = parse(bad, &owner()).unwrap_err();
            assert_eq!(
                error.kind,
                Failure::NotFeed,
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn hostile_titles_are_cleaned() {
        let text = format!(
            r#"<feed xmlns="http://www.w3.org/2005/Atom"
                     xmlns:yt="http://www.youtube.com/xml/schemas/2015">
                 <entry>
                   <yt:videoId>abcdefghijk</yt:videoId>
                   <yt:channelId>{OWNER}</yt:channelId>
                   <title>  a&#x202e;b
                   c </title>
                 </entry>
               </feed>"#
        );
        let listing = parse(text.as_bytes(), &owner()).unwrap();
        assert_eq!(listing.videos[0].title, "ab c");
    }

    #[test]
    fn deep_nesting_does_no_harm() {
        let text = format!(
            "<feed xmlns=\"http://www.w3.org/2005/Atom\">{}{}</feed>",
            "<a>".repeat(100_000),
            "</a>".repeat(100_000)
        );
        // Either answer will do, as long as nothing overflows.
        if let Ok(listing) = parse(text.as_bytes(), &owner()) {
            assert!(listing.videos.is_empty());
        }
    }

    #[test]
    fn answers_become_failures_by_status() {
        let answer = |status, body: &[u8]| Answer {
            status,
            body: body.to_vec(),
        };
        for status in [404, 500, 503, 204, 301] {
            let error = read(answer(status, FEED), &owner()).unwrap_err();
            assert_eq!(error.kind, Failure::NotFeed);
            assert_eq!(error.detail, format!("HTTP {status}"));
        }
        assert!(read(answer(200, FEED), &owner()).is_ok());
        let error = read(answer(200, b"<html></html>"), &owner()).unwrap_err();
        assert_eq!(error.kind, Failure::NotFeed);
    }

    #[test]
    fn a_feed_comes_over_http_end_to_end() {
        let server = Server::start(vec![response(
            200,
            "Content-Type: text/xml\r\n",
            std::str::from_utf8(FEED).unwrap(),
        )]);
        let client = Client::new(Some("")).unwrap();
        let answer = client.local(&server.url("/feeds/videos.xml")).unwrap();
        assert_eq!(read(answer, &owner()).unwrap().videos.len(), 3);
    }

    #[test]
    fn a_proxy_that_cannot_be_used_fails_every_channel_by_name() {
        let source = RssSource::new(Some("http://bad host:x"));
        let channels = [owner(), owner()];
        let answers = source.fetch(&channels, &Cancel::default());
        assert_eq!(answers.len(), 2);
        for answer in answers {
            assert_eq!(answer.unwrap_err().kind, Failure::Proxy);
        }
    }

    #[test]
    fn a_cancelled_refresh_asks_nothing() {
        let source = RssSource::new(Some(""));
        let cancel = Cancel::default();
        cancel.cancel();
        let answers = source.fetch(&[owner()], &cancel);
        assert_eq!(answers[0].as_ref().unwrap_err().kind, Failure::Cancelled);
    }
}
