//! An online video as yt-dlp sees it, from the JSON that mpv's yt-dlp hook
//! got for it: the qualities it comes in, for the Video Quality and Audio
//! Quality menus, and the yt-dlp format that plays one of them.
//!
//! Video is offered by resolution and frame rate, as sites' own players do,
//! rather than as yt-dlp's list of formats (dozens of them for a YouTube
//! video, one per codec and protocol); yt-dlp picks the format within that.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Info {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    webpage_url: Option<String>,
    #[serde(default)]
    original_url: Option<String>,
    #[serde(default, rename = "_type")]
    kind: Option<String>,
    #[serde(default)]
    format_id: Option<String>,
    #[serde(default)]
    formats: Option<Vec<Format>>,
    #[serde(default)]
    requested_formats: Option<Vec<Format>>,
}

#[derive(Debug, Clone, Deserialize)]
struct Format {
    format_id: String,
    #[serde(default)]
    vcodec: Option<String>,
    #[serde(default)]
    acodec: Option<String>,
    #[serde(default)]
    height: Option<f64>,
    #[serde(default)]
    fps: Option<f64>,
    #[serde(default)]
    tbr: Option<f64>,
    #[serde(default)]
    abr: Option<f64>,
    #[serde(default)]
    dynamic_range: Option<String>,
    #[serde(default)]
    format_note: Option<String>,
    #[serde(default)]
    language: Option<String>,
}

/// Whether a format may have this kind of stream: yt-dlp writes "none"
/// when it has none, and leaves the codec out when it doesn't know (the
/// renditions of an HLS stream, say).
fn may_have(codec: &Option<String>) -> bool {
    codec.as_deref() != Some("none")
}

impl Format {
    fn is_video(&self) -> bool {
        may_have(&self.vcodec) && self.height.is_some_and(|height| height > 0.0)
    }

    /// Audio and nothing else (not the storyboard images, which have
    /// neither).
    fn is_audio(&self) -> bool {
        !may_have(&self.vcodec) && may_have(&self.acodec)
    }

    fn is_hdr(&self) -> bool {
        self.dynamic_range
            .as_deref()
            .is_some_and(|range| range != "SDR")
    }
}

/// A resolution and frame rate the video comes in.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoQuality {
    /// Stable while the video plays, for the menu: "1080p60", "2160p60-hdr".
    pub key: String,
    pub label: String,
    height: u32,
    /// The frame rate when above 30; formats at 30 or below count as one.
    high_fps: Option<u32>,
    fps_known: bool,
    /// Whether to tell HDR from SDR: only when the video has both.
    hdr: Option<bool>,
}

impl VideoQuality {
    /// yt-dlp's format filter for it.
    fn filter(&self) -> String {
        let mut filter = format!("[height={}]", self.height);
        if self.fps_known {
            filter.push_str(if self.high_fps.is_some() {
                "[fps>30]"
            } else {
                "[fps<=30]"
            });
        }
        match self.hdr {
            Some(true) => filter.push_str("[dynamic_range!=SDR]"),
            Some(false) => filter.push_str("[dynamic_range=SDR]"),
            None => {}
        }
        filter
    }
}

/// An audio format the video comes with.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioFormat {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Media {
    /// The page the video is on, which yt-dlp can be given again.
    pub url: String,
    pub title: String,
    /// Best first.
    pub video: Vec<VideoQuality>,
    /// Best first.
    pub audio: Vec<AudioFormat>,
    /// What is playing: a key of `video`, an id of `audio`.
    pub playing_video: Option<String>,
    pub playing_audio: Option<String>,
    /// yt-dlp's format ids for what is playing ("401+258"), to download
    /// the same.
    pub format_id: Option<String>,
}

impl Media {
    /// Reads yt-dlp's JSON for a single video; None for a playlist, or
    /// anything without formats to choose from.
    pub fn parse(json: &str) -> Option<Media> {
        let info: Info = match serde_json::from_str(json) {
            Ok(info) => info,
            Err(error) => {
                log::warn!("could not read yt-dlp's output: {error}");
                return None;
            }
        };
        if info.kind.as_deref().is_some_and(|kind| kind != "video") {
            return None;
        }
        let formats = info.formats.unwrap_or_default();
        if formats.is_empty() {
            return None;
        }
        let video = video_qualities(&formats);
        let audio = audio_formats(&formats);
        // A merged format's ids are joined with "+".
        let requested = info.requested_formats.unwrap_or_else(|| {
            let ids: Vec<&str> = info
                .format_id
                .as_deref()
                .unwrap_or_default()
                .split('+')
                .collect();
            formats
                .iter()
                .filter(|format| ids.contains(&format.format_id.as_str()))
                .cloned()
                .collect()
        });
        let playing_video = requested
            .iter()
            .find(|format| format.is_video())
            .and_then(|format| {
                let key = quality_of(format, &video);
                video.iter().find(|quality| quality.key == key)
            })
            .map(|quality| quality.key.clone());
        let playing_audio = requested
            .iter()
            .find(|format| format.is_audio())
            .map(|format| format.format_id.clone());
        Some(Media {
            url: info.webpage_url.or(info.original_url).unwrap_or_default(),
            title: info.title.unwrap_or_default(),
            video,
            audio,
            playing_video,
            playing_audio,
            format_id: info.format_id,
        })
    }

    /// The yt-dlp format for a choice from the menus: a key of `video` and
    /// an id of `audio`, None for leaving it to the preferences.
    /// `default_filter` is the preferences' limit on the video, such as
    /// "[height<=1080]".
    pub fn format(&self, video: Option<&str>, audio: Option<&str>, default_filter: &str) -> String {
        if self.video.is_empty() {
            return audio.unwrap_or("bestaudio/best").to_owned();
        }
        let filter = video
            .and_then(|key| self.video.iter().find(|quality| quality.key == key))
            .map_or_else(|| default_filter.to_owned(), VideoQuality::filter);
        let audio = audio.unwrap_or("bestaudio");
        // Sites that only have video and audio together get the
        // resolution without the audio choice.
        format!("bestvideo{filter}+{audio}/best{filter}/best")
    }

    pub fn video_label(&self, key: &str) -> Option<&str> {
        self.video
            .iter()
            .find(|quality| quality.key == key)
            .map(|quality| quality.label.as_str())
    }

    pub fn audio_label(&self, id: &str) -> Option<&str> {
        self.audio
            .iter()
            .find(|format| format.id == id)
            .map(|format| format.label.as_str())
    }
}

fn key_parts(format: &Format) -> (u32, Option<u32>, bool) {
    let height = format.height.unwrap_or_default().round() as u32;
    let fps = format
        .fps
        .map(|fps| fps.round() as u32)
        .filter(|fps| *fps > 30);
    (height, fps, format.is_hdr())
}

fn quality_of(format: &Format, qualities: &[VideoQuality]) -> String {
    let (height, fps, hdr) = key_parts(format);
    let any_hdr = qualities.iter().any(|quality| quality.hdr.is_some());
    key(height, fps, any_hdr && hdr)
}

fn key(height: u32, fps: Option<u32>, hdr: bool) -> String {
    let fps = fps.map(|fps| fps.to_string()).unwrap_or_default();
    let hdr = if hdr { "-hdr" } else { "" };
    format!("{height}p{fps}{hdr}")
}

fn video_qualities(formats: &[Format]) -> Vec<VideoQuality> {
    let video: Vec<&Format> = formats.iter().filter(|format| format.is_video()).collect();
    let any_hdr = video.iter().any(|format| format.is_hdr());
    let fps_known = video.iter().all(|format| format.fps.is_some());
    let mut qualities: Vec<VideoQuality> = Vec::new();
    for format in video {
        let (height, high_fps, hdr) = key_parts(format);
        let key = key(height, high_fps, any_hdr && hdr);
        if qualities.iter().any(|quality| quality.key == key) {
            continue;
        }
        let fps = high_fps.map(|fps| fps.to_string()).unwrap_or_default();
        let hdr_label = if hdr { " HDR" } else { "" };
        qualities.push(VideoQuality {
            label: format!("{height}p{fps}{hdr_label}"),
            key,
            height,
            high_fps,
            fps_known,
            hdr: any_hdr.then_some(hdr),
        });
    }
    qualities.sort_by(|a, b| {
        (b.height, b.high_fps, b.hdr == Some(true)).cmp(&(
            a.height,
            a.high_fps,
            a.hdr == Some(true),
        ))
    });
    qualities
}

fn codec_name(codec: &str) -> String {
    let lower = codec.to_ascii_lowercase();
    let name = if lower.starts_with("mp4a") || lower == "aac" {
        "AAC"
    } else if lower == "opus" {
        "Opus"
    } else if lower == "ac-3" || lower == "ac3" {
        "AC-3"
    } else if lower == "ec-3" || lower == "eac3" {
        "E-AC-3"
    } else if lower == "vorbis" {
        "Vorbis"
    } else if lower == "mp3" {
        "MP3"
    } else if lower == "flac" {
        "FLAC"
    } else {
        return codec.to_owned();
    };
    name.to_owned()
}

fn audio_formats(formats: &[Format]) -> Vec<AudioFormat> {
    let mut audio: Vec<&Format> = formats.iter().filter(|format| format.is_audio()).collect();
    let mut languages: Vec<&str> = audio
        .iter()
        .filter_map(|format| format.language.as_deref())
        .collect();
    languages.sort_unstable();
    languages.dedup();
    let bitrate = |format: &Format| format.abr.or(format.tbr);
    audio.sort_by(|a, b| {
        bitrate(b)
            .unwrap_or_default()
            .total_cmp(&bitrate(a).unwrap_or_default())
    });
    audio
        .into_iter()
        .map(|format| {
            let mut parts = Vec::new();
            match bitrate(format) {
                Some(rate) => parts.push(format!("{rate:.0} kb/s")),
                None => parts.extend(format.format_note.clone()),
            }
            parts.extend(format.acodec.as_deref().map(codec_name));
            if languages.len() > 1 {
                parts.extend(format.language.clone());
            }
            let drc = format.format_id.ends_with("-drc")
                || format
                    .format_note
                    .as_deref()
                    .is_some_and(|note| note.contains("DRC"));
            if drc {
                parts.push("DRC".to_owned());
            }
            if parts.is_empty() {
                parts.push(format.format_id.clone());
            }
            AudioFormat {
                id: format.format_id.clone(),
                label: parts.join(" · "),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed-down version of what yt-dlp says about a YouTube video.
    const VIDEO: &str = r#"{
        "_type": "video",
        "title": "Big Buck Bunny",
        "webpage_url": "https://www.youtube.com/watch?v=aqz-KE-bpKQ",
        "format_id": "303+251",
        "formats": [
            {"format_id": "sb0", "vcodec": "none", "acodec": "none", "height": 180, "format_note": "storyboard"},
            {"format_id": "249", "vcodec": "none", "acodec": "opus", "abr": 49.5, "format_note": "low"},
            {"format_id": "251", "vcodec": "none", "acodec": "opus", "abr": 128.6, "format_note": "medium"},
            {"format_id": "251-drc", "vcodec": "none", "acodec": "opus", "abr": 129.3, "format_note": "medium, DRC"},
            {"format_id": "140", "vcodec": "none", "acodec": "mp4a.40.2", "abr": 129.5, "format_note": "medium"},
            {"format_id": "18", "vcodec": "avc1.42001E", "acodec": "mp4a.40.2", "height": 360, "fps": 30, "dynamic_range": "SDR"},
            {"format_id": "134", "vcodec": "avc1.4D401E", "acodec": "none", "height": 360, "fps": 30, "dynamic_range": "SDR"},
            {"format_id": "247", "vcodec": "vp9", "acodec": "none", "height": 720, "fps": 30, "dynamic_range": "SDR"},
            {"format_id": "302", "vcodec": "vp9", "acodec": "none", "height": 720, "fps": 60, "dynamic_range": "SDR"},
            {"format_id": "299", "vcodec": "avc1.64002A", "acodec": "none", "height": 1080, "fps": 60, "dynamic_range": "SDR"},
            {"format_id": "303", "vcodec": "vp9", "acodec": "none", "height": 1080, "fps": 60, "dynamic_range": "SDR"}
        ],
        "requested_formats": [
            {"format_id": "303", "vcodec": "vp9", "acodec": "none", "height": 1080, "fps": 60, "dynamic_range": "SDR"},
            {"format_id": "251", "vcodec": "none", "acodec": "opus", "abr": 128.6, "format_note": "medium"}
        ]
    }"#;

    fn keys(media: &Media) -> Vec<&str> {
        media
            .video
            .iter()
            .map(|quality| quality.key.as_str())
            .collect()
    }

    #[test]
    fn video_is_offered_by_resolution_and_frame_rate() {
        let media = Media::parse(VIDEO).unwrap();
        assert_eq!(keys(&media), ["1080p60", "720p60", "720p", "360p"]);
        assert_eq!(media.video_label("720p60"), Some("720p60"));
        assert_eq!(media.playing_video.as_deref(), Some("1080p60"));
        assert_eq!(media.title, "Big Buck Bunny");
        assert_eq!(media.url, "https://www.youtube.com/watch?v=aqz-KE-bpKQ");
        assert_eq!(media.format_id.as_deref(), Some("303+251"));
    }

    #[test]
    fn audio_is_listed_by_bit_rate_without_the_storyboards() {
        let media = Media::parse(VIDEO).unwrap();
        let labels: Vec<&str> = media.audio.iter().map(|a| a.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "130 kb/s · AAC",
                "129 kb/s · Opus · DRC",
                "129 kb/s · Opus",
                "50 kb/s · Opus"
            ]
        );
        assert_eq!(media.playing_audio.as_deref(), Some("251"));
    }

    #[test]
    fn choices_become_yt_dlp_formats() {
        let media = Media::parse(VIDEO).unwrap();
        assert_eq!(
            media.format(Some("720p60"), None, ""),
            "bestvideo[height=720][fps>30]+bestaudio/best[height=720][fps>30]/best"
        );
        assert_eq!(
            media.format(Some("720p"), Some("140"), ""),
            "bestvideo[height=720][fps<=30]+140/best[height=720][fps<=30]/best"
        );
        assert_eq!(
            media.format(None, Some("249"), "[height<=1080]"),
            "bestvideo[height<=1080]+249/best[height<=1080]/best"
        );
    }

    #[test]
    fn hdr_is_told_apart_only_when_both_kinds_exist() {
        let json = r#"{"formats": [
            {"format_id": "a", "vcodec": "vp09.02", "acodec": "none", "height": 2160, "fps": 60, "dynamic_range": "HDR10"},
            {"format_id": "b", "vcodec": "vp9", "acodec": "none", "height": 2160, "fps": 60, "dynamic_range": "SDR"}
        ], "format_id": "a"}"#;
        let media = Media::parse(json).unwrap();
        assert_eq!(keys(&media), ["2160p60-hdr", "2160p60"]);
        assert_eq!(media.video_label("2160p60-hdr"), Some("2160p60 HDR"));
        assert_eq!(media.playing_video.as_deref(), Some("2160p60-hdr"));
        assert!(
            media
                .format(Some("2160p60"), None, "")
                .starts_with("bestvideo[height=2160][fps>30][dynamic_range=SDR]+")
        );
    }

    #[test]
    fn several_languages_are_named() {
        let json = r#"{"formats": [
            {"format_id": "en", "vcodec": "none", "acodec": "opus", "abr": 130, "language": "en"},
            {"format_id": "de", "vcodec": "none", "acodec": "opus", "abr": 130, "language": "de"}
        ]}"#;
        let media = Media::parse(json).unwrap();
        let labels: Vec<&str> = media.audio.iter().map(|a| a.label.as_str()).collect();
        assert_eq!(labels, ["130 kb/s · Opus · en", "130 kb/s · Opus · de"]);
        // Nothing but audio: the choice is the format itself.
        assert_eq!(media.format(None, Some("de"), ""), "de");
        assert_eq!(media.format(None, None, ""), "bestaudio/best");
    }

    #[test]
    fn hls_renditions_count_without_codecs() {
        // What yt-dlp says about an HLS stream with separate audio.
        let json = r#"{"_type": "video", "format_id": "2597+group_aud-audio_3", "formats": [
            {"format_id": "group_aud-audio_4", "vcodec": "none", "format_note": "audio_4"},
            {"format_id": "group_aud-audio_3", "vcodec": "none", "format_note": "audio_3"},
            {"format_id": "749", "vcodec": "avc1.64001e", "acodec": "none", "height": 360, "tbr": 749.6},
            {"format_id": "2597", "vcodec": "avc1.64001f", "acodec": "none", "height": 720, "tbr": 2597.5}
        ]}"#;
        let media = Media::parse(json).unwrap();
        assert_eq!(keys(&media), ["720p", "360p"]);
        let ids: Vec<&str> = media.audio.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["group_aud-audio_4", "group_aud-audio_3"]);
        assert_eq!(media.audio_label("group_aud-audio_3"), Some("audio_3"));
        assert_eq!(media.playing_video.as_deref(), Some("720p"));
        assert_eq!(media.playing_audio.as_deref(), Some("group_aud-audio_3"));
        // No frame rates known: no frame rate filter.
        assert_eq!(
            media.format(Some("360p"), None, ""),
            "bestvideo[height=360]+bestaudio/best[height=360]/best"
        );
    }

    #[test]
    fn playlists_and_garbage_have_no_qualities() {
        assert_eq!(
            Media::parse(r#"{"_type": "playlist", "entries": []}"#),
            None
        );
        assert_eq!(Media::parse("not json"), None);
        assert_eq!(Media::parse(r#"{"title": "no formats"}"#), None);
    }
}
