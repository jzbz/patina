//! patina:// links, for browser extensions and web pages to open a stream
//! in Patina, after IINA's iina:// scheme:
//!
//! `patina://open?url=<percent-encoded URL>&new_window=1&enqueue=1&full_screen=1&pip=1&mpv_<option>=<value>`
//!
//! Only `url` is needed. The flags are on when their value is 1. A link
//! comes from a web page, so Patina trusts it less than IINA does: `url`
//! must be a network stream, never a local file or one of mpv's own
//! protocols, and only mpv options that touch nothing outside playback
//! are taken.

use crate::instance::Request;
use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

/// The streams links opened, and those their playlists named: a page can't
/// reach past a link's rules through a playlist it serves.
pub type LinkSources = Arc<Mutex<HashSet<String>>>;

const PREFIX: &str = "patina://";

/// True for a patina:// link.
pub fn is_link(source: &str) -> bool {
    source
        .get(..PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(PREFIX))
}

/// What a link asks to open.
pub fn parse(link: &str) -> Result<Request, String> {
    let rest = link.get(PREFIX.len()..).unwrap_or_default();
    let rest = rest.split_once('#').map_or(rest, |(rest, _)| rest);
    let (place, query) = rest.split_once('?').unwrap_or((rest, ""));
    let host = place.split('/').next().unwrap_or_default();
    // IINA's two names for the same thing.
    if !host.eq_ignore_ascii_case("open") && !host.eq_ignore_ascii_case("weblink") {
        return Err(format!("Patina doesn't know patina://{host:?}"));
    }

    let mut request = Request::default();
    let mut url = None;
    let mut seen = Vec::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let (name, value) = (decode(name)?, decode(value)?);
        // The first of a name counts, where IINA would crash.
        if seen.contains(&name) {
            log::debug!("ignoring a second {name:?} in a patina:// link");
            continue;
        }
        seen.push(name.clone());
        let on = value == "1";
        match name.as_str() {
            "url" => url = Some(value),
            "new_window" => request.new_window = on,
            "enqueue" => request.enqueue = on,
            "full_screen" => request.fullscreen = on,
            "pip" => request.pip = on,
            _ => match name.strip_prefix("mpv_") {
                Some(option) if SAFE_MPV_OPTIONS.contains(&option) => {
                    request.mpv_options.push((option.to_owned(), value));
                }
                Some(option) => log::warn!("ignoring mpv option {option:?} from a patina:// link"),
                None => log::debug!("ignoring {name:?} in a patina:// link"),
            },
        }
    }
    let url = url
        .filter(|url| !url.is_empty())
        .ok_or("the link names nothing to open")?;
    if !is_stream(&url) {
        return Err(format!("links open only streams, not {url:?}"));
    }
    request.sources = vec![url];
    Ok(request)
}

/// Whether a link may open `url`: a network stream of a kind on the list.
pub fn is_stream(url: &str) -> bool {
    url.split_once("://")
        .is_some_and(|(scheme, _)| NETWORK_SCHEMES.contains(&scheme.to_ascii_lowercase().as_str()))
}

/// Network streams of any kind mpv plays from a playlist, a link's or not:
/// the link's own list, and the multicast and other live streams of IPTV
/// playlists.
const MEDIA_STREAM_SCHEMES: &[&str] = &[
    "udp", "rtp", "srt", "srtp", "rtmpe", "rtmpt", "rtmpte", "rtmpts",
];

/// Whether `url` is a network stream, rather than a file or one of mpv's
/// own protocols.
pub fn is_network_stream(url: &str) -> bool {
    is_stream(url)
        || url.split_once("://").is_some_and(|(scheme, _)| {
            MEDIA_STREAM_SCHEMES.contains(&scheme.to_ascii_lowercase().as_str())
        })
}

/// Whether `playlist` is one a link opened, or one such a playlist named.
pub fn from_link(sources: &LinkSources, playlist: &str) -> bool {
    !playlist.is_empty()
        && sources
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(playlist)
}

pub fn note_link_sources(sources: &LinkSources, opened: &[String]) {
    let mut sources = sources.lock().unwrap_or_else(PoisonError::into_inner);
    sources.extend(opened.iter().cloned());
}

/// From mpv's load hook: when the file about to open is an entry of a
/// playlist a link opened, lets only a stream a link could open itself
/// through (and screens its own entries in turn). Anything else, a local
/// file, a share or a listening srt:// address, opens as nothing. Blocks on
/// mpv.
pub fn screen_playlist_entry(mpv: &crate::mpv::Mpv, sources: &LinkSources) {
    let property = |name| {
        mpv.get_property(name)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
    };
    let Some(playlist) = property("playlist-path") else {
        return;
    };
    let mut sources = sources.lock().unwrap_or_else(PoisonError::into_inner);
    if !sources.contains(&playlist) {
        return;
    }
    let path = property("path").unwrap_or_default();
    if is_stream(&path) {
        sources.insert(path);
        return;
    }
    log::warn!("not opening {path:?}, which a link's playlist names");
    if let Err(error) = mpv.set_property("stream-open-filename", "null://") {
        log::warn!("{error}");
    }
}

/// Percent-decoding, once; a '+' stays a '+', as in IINA.
fn decode(text: &str) -> Result<String, String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let byte = text
                .get(index + 1..index + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                .ok_or_else(|| format!("bad escape in {text:?}"))?;
            decoded.push(byte);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| format!("{text:?} is not UTF-8"))
}

/// Streams a link may open. Not srt:// or rtp://, whose URLs can make
/// Patina listen for connections from anywhere.
const NETWORK_SCHEMES: &[&str] = &[
    "http", "https", "ftp", "ftps", "rtmp", "rtmps", "rtsp", "rtsps", "mms", "mmsh", "mmst",
];

/// IINA's AppData.safeMPVOptions, less those with which a page could turn
/// off certificate checks (tls-verify), make Patina louder than it can
/// otherwise be (volume-max, volume-gain, volume-gain-max, volume-gain-min,
/// and replaygain with its preamp, fallback and clip, which follow the
/// stream's own tags) or use up memory and threads (demuxer-max-bytes, demuxer-max-back-bytes,
/// demuxer-lavf-probesize, demuxer-lavf-analyzeduration, vd-lavc-threads,
/// ad-lavc-threads, hwdec-threads, hwdec-extra-frames).
const SAFE_MPV_OPTIONS: &[&str] = &[
    // Track selection.
    "aid",
    "vid",
    "sid",
    "secondary-sid",
    "alang",
    "slang",
    "vlang",
    "edition",
    "track-auto-selection",
    "subs-with-matching-audio",
    "subs-match-os-language",
    "subs-fallback",
    "subs-fallback-forced",
    // Playback.
    "start",
    "end",
    "length",
    "frames",
    "speed",
    "pitch",
    "pause",
    "sstep",
    "correct-pts",
    "container-fps-override",
    "loop-file",
    "loop-playlist",
    "ab-loop-a",
    "ab-loop-b",
    "ab-loop-count",
    "play-direction",
    "rebase-start-time",
    "hr-seek",
    "hr-seek-framedrop",
    // Video.
    "deinterlace",
    "deinterlace-field-parity",
    "hwdec",
    "hwdec-codecs",
    "video-aspect-override",
    "video-aspect-method",
    "video-rotate",
    "video-crop",
    "video-zoom",
    "video-pan-x",
    "video-pan-y",
    "video-align-x",
    "video-align-y",
    "video-unscaled",
    "video-scale-x",
    "video-scale-y",
    "video-recenter",
    "video-margin-ratio-left",
    "video-margin-ratio-right",
    "video-margin-ratio-top",
    "video-margin-ratio-bottom",
    "video-output-levels",
    "panscan",
    "framedrop",
    "video-latency-hacks",
    "display-fps-override",
    "vd-lavc-skiploopfilter",
    "vd-lavc-skipidct",
    "vd-lavc-skipframe",
    "vd-lavc-framedrop",
    "vd-lavc-fast",
    "vd-lavc-film-grain",
    "vd-lavc-dr",
    "vd-apply-cropping",
    "hwdec-image-format",
    "hwdec-software-fallback",
    "vd-lavc-check-hw-profile",
    "swapchain-depth",
    "brightness",
    "contrast",
    "saturation",
    "gamma",
    "hue",
    // Audio.
    "volume",
    "mute",
    "audio-delay",
    "audio-pitch-correction",
    "audio-channels",
    "audio-display",
    "audio-samplerate",
    "audio-format",
    "audio-exclusive",
    "audio-spdif",
    "gapless-audio",
    "initial-audio-sync",
    "ad-lavc-ac3drc",
    "ad-lavc-downmix",
    "audio-stream-silence",
    "audio-wait-open",
    "audio-buffer",
    "audio-normalize-downmix",
    "audio-set-media-role",
    // Subtitles.
    "sub-delay",
    "secondary-sub-delay",
    "sub-scale",
    "sub-scale-signs",
    "sub-scale-by-window",
    "sub-scale-with-window",
    "sub-ass-scale-with-window",
    "sub-pos",
    "secondary-sub-pos",
    "sub-speed",
    "sub-visibility",
    "secondary-sub-visibility",
    "sub-ass",
    "sub-ass-justify",
    "sub-ass-override",
    "secondary-sub-ass-override",
    "sub-ass-force-margins",
    "sub-use-margins",
    "sub-ass-use-video-data",
    "sub-vsfilter-bidi-compat",
    "sub-ass-vsfilter-color-compat",
    "sub-font",
    "sub-font-size",
    "sub-color",
    "sub-outline-color",
    "sub-outline-size",
    "sub-back-color",
    "sub-shadow-offset",
    "sub-bold",
    "sub-italic",
    "sub-blur",
    "sub-margin-x",
    "sub-margin-y",
    "sub-align-x",
    "sub-align-y",
    "sub-justify",
    "sub-border-style",
    "sub-spacing",
    "sub-line-spacing",
    "sub-hinting",
    "sub-shaper",
    "sub-codepage",
    "sub-fix-timing",
    "sub-fix-timing-threshold",
    "sub-fix-timing-keep",
    "sub-stretch-durations",
    "sub-gauss",
    "sub-gray",
    "sub-forced-events-only",
    "sub-fps",
    "sub-filter-sdh",
    "sub-filter-sdh-harder",
    "sub-filter-sdh-enclosures",
    "sub-clear-on-seek",
    "sub-create-cc-track",
    "sub-past-video-end",
    "sub-font-provider",
    "sub-hdr-peak",
    "image-subs-hdr-peak",
    "sub-ass-style-overrides",
    "stretch-dvd-subs",
    "stretch-image-subs-to-screen",
    "image-subs-video-resolution",
    "embeddedfonts",
    "sub-ass-video-aspect-override",
    "sub-ass-prune-delay",
    "teletext-page",
    // Window.
    "fullscreen",
    "geometry",
    "ontop",
    "keep-open",
    "keep-open-pause",
    "image-display-duration",
    "stop-screensaver",
    // Network.
    "user-agent",
    "referrer",
    "network-timeout",
    "rtsp-transport",
    "hls-bitrate",
    "cache",
    "cache-secs",
    "cache-pause",
    "cache-pause-wait",
    "cache-pause-initial",
    "force-seekable",
    "http-header-fields",
    "cookies",
    // Demuxers, the audio resampler and others.
    "demuxer-readahead-secs",
    "demuxer-mkv-subtitle-preroll",
    "demuxer-mkv-subtitle-preroll-secs",
    "demuxer-lavf-probescore",
    "audio-resample-filter-size",
    "audio-resample-phase-shift",
    "audio-resample-cutoff",
    "audio-resample-linear",
    "audio-resample-max-output-size",
    "video-sync",
    "interpolation",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_like_iinas() {
        let request = parse(
            "patina://open?url=https%3A%2F%2Fexample.com%2Fv%3Fa%3D1%26b%3D%2520x&new_window=1&full_screen=1&enqueue=0&mpv_start=30&mpv_sub-delay=0.5",
        )
        .unwrap();
        // Decoded once: the stream's own escapes survive.
        assert_eq!(request.sources, ["https://example.com/v?a=1&b=%20x"]);
        assert!(request.new_window && request.fullscreen && !request.enqueue && !request.pip);
        assert_eq!(
            request.mpv_options,
            [
                ("start".into(), "30".into()),
                ("sub-delay".into(), "0.5".into())
            ]
        );
        assert!(
            parse("PATINA://weblink/?url=http://a/b+c&pip=1")
                .is_ok_and(|request| { request.sources == ["http://a/b+c"] && request.pip })
        );
        assert!(is_link("Patina://open?url=x") && !is_link("https://patina.video"));
    }

    /// What a playlist from the network names may open elsewhere only as
    /// mpv would open it from there, and a link's playlist only as a link
    /// could; reopened, a link's entry stays screened.
    #[test]
    fn entries_of_network_playlists_reopen_with_care() {
        use crate::player::state::PlaylistEntry;
        let entry = |filename: &str, playlist: &str| PlaylistEntry {
            filename: filename.into(),
            title: String::new(),
            current: false,
            id: 0,
            playlist: playlist.into(),
            from_network: crate::util::is_remote(playlist),
        };
        let links = LinkSources::default();
        note_link_sources(&links, &["https://a/link.m3u".into()]);
        assert!(entry("/home/a.mkv", "").may_reopen(&links));
        assert!(entry("/home/a.mkv", "file:///home/list.m3u").may_reopen(&links));
        assert!(!entry("/home/a.mkv", "https://a/list.m3u").may_reopen(&links));
        assert!(!entry(r"\\evil\share\a.mkv", "https://a/list.m3u").may_reopen(&links));
        assert!(entry("udp://@239.0.0.1:1234", "https://a/list.m3u").may_reopen(&links));
        assert!(!entry("udp://@239.0.0.1:1234", "https://a/link.m3u").may_reopen(&links));
        assert!(entry("https://a/b.m3u", "https://a/link.m3u").may_reopen(&links));
        assert!(!entry("file:///etc/passwd", "https://a/list.m3u").may_reopen(&links));
        assert!(entry("file:///home/a.mkv", "/home/list.m3u").may_reopen(&links));
        assert!(entry("https://a/v.mp4", "/home/list.m3u").may_reopen(&links));
        // mpv's own protocols, which it refuses from any playlist.
        for special in [
            "av://lavfi:sine=d=1,ametadata=mode=print:file=/home/a.conf",
            r"edl://\\evil\share\a.mkv",
            "hex://2f6574632f706173737764",
            "lavf://file:/etc/passwd",
            "memory://x",
        ] {
            for playlist in ["https://a/list.m3u", "/home/list.m3u", "https://a/link.m3u"] {
                assert!(
                    !entry(special, playlist).may_reopen(&links),
                    "{special} from {playlist}"
                );
            }
        }
    }

    #[test]
    fn links_open_only_streams_with_safe_options() {
        for url in [
            "/etc/passwd",
            "file:///home/a.mkv",
            "edl://%2Fetc",
            "av://lavfi:sine",
            "-",
            "",
            "srt://0.0.0.0:9000?mode=listener",
            "rtp://0.0.0.0:5004",
        ] {
            let link = format!("patina://open?url={url}");
            assert!(parse(&link).is_err(), "{link}");
        }
        assert!(parse("patina://open").is_err());
        assert!(parse("patina://play?url=https://a").is_err());
        assert!(parse("patina://open?url=https://a&url=file:///b").is_ok());
        assert!(parse("patina://open?url=https://a%zz").is_err());

        let request = parse(
            "patina://open?url=https://a&mpv_script=/tmp/x.lua&mpv_volume-max=1000&mpv_tls-verify=no&mpv_vd-lavc-threads=9999&mpv_volume=50",
        )
        .unwrap();
        assert_eq!(request.mpv_options, [("volume".into(), "50".into())]);
    }
}
