//! A typed copy of the mpv state the interface displays.

use super::Prop;
use crate::mpv::Node;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrackKind {
    #[default]
    Video,
    Audio,
    Sub,
}

impl TrackKind {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "video" => Some(Self::Video),
            "audio" => Some(Self::Audio),
            "sub" => Some(Self::Sub),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Track {
    pub id: i64,
    pub kind: TrackKind,
    pub title: String,
    pub lang: String,
    pub codec: String,
    pub external: bool,
    pub external_filename: String,
    pub selected: bool,
    /// 0 for the primary selection, 1 for the secondary subtitle.
    pub main_selection: Option<i64>,
    pub default: bool,
    pub forced: bool,
    pub albumart: bool,
    pub image: bool,
    pub width: i64,
    pub height: i64,
    pub fps: f64,
    pub channels: i64,
    pub samplerate: i64,
}

impl Track {
    pub fn from_node(node: &Node) -> Option<Self> {
        let kind = TrackKind::parse(node.get("type")?.as_str()?)?;
        let int = |key| node.get(key).and_then(Node::as_i64).unwrap_or(0);
        Some(Self {
            id: node.get("id")?.as_i64()?,
            kind,
            title: node.str_or_default("title"),
            lang: node.str_or_default("lang"),
            codec: node.str_or_default("codec"),
            external: node.bool_or("external", false),
            external_filename: node.str_or_default("external-filename"),
            selected: node.bool_or("selected", false),
            main_selection: node.get("main-selection").and_then(Node::as_i64),
            default: node.bool_or("default", false),
            forced: node.bool_or("forced", false),
            albumart: node.bool_or("albumart", false),
            image: node.bool_or("image", false),
            width: int("demux-w"),
            height: int("demux-h"),
            fps: node.get("demux-fps").and_then(Node::as_f64).unwrap_or(0.0),
            channels: int("demux-channel-count"),
            samplerate: int("demux-samplerate"),
        })
    }

    /// The name shown in menus and lists.
    pub fn label(&self) -> String {
        let language = language_name(&self.lang);
        match (self.title.is_empty(), language) {
            (false, Some(language)) if !self.title.contains(language) => {
                format!("{} ({language})", self.title)
            }
            (false, _) => self.title.clone(),
            (true, Some(language)) => language.to_owned(),
            (true, None) if !self.lang.is_empty() => self.lang.clone(),
            (true, None) if self.external => file_name(&self.external_filename),
            (true, None) => format!("Track {}", self.id),
        }
    }

    /// Secondary details: codec and format, plus flags.
    pub fn detail(&self) -> String {
        let mut parts = Vec::new();
        if !self.codec.is_empty() {
            parts.push(self.codec.to_uppercase());
        }
        match self.kind {
            TrackKind::Video if self.width > 0 => {
                parts.push(format!("{}×{}", self.width, self.height));
                if self.fps > 0.0 {
                    parts.push(format!("{} fps", trim_float(self.fps, 3)));
                }
            }
            TrackKind::Audio if self.channels > 0 => {
                parts.push(channel_layout(self.channels));
                if self.samplerate > 0 {
                    parts.push(format!(
                        "{} kHz",
                        trim_float(self.samplerate as f64 / 1000.0, 1)
                    ));
                }
            }
            _ => {}
        }
        if self.albumart {
            parts.push("album art".into());
        }
        if self.default {
            parts.push("default".into());
        }
        if self.forced {
            parts.push("forced".into());
        }
        if self.external {
            parts.push("external".into());
        }
        parts.join(" · ")
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Chapter {
    pub title: String,
    pub time: f64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlaylistEntry {
    pub filename: String,
    pub title: String,
    pub current: bool,
    pub id: i64,
    /// The playlist that listed it, as mpv opened it; empty if none did.
    pub playlist: String,
    /// Listed by a playlist that came from the network. Such an entry may
    /// name any local path or network share, which mpv refuses to open
    /// from there, and neither may Patina.
    pub from_network: bool,
}

impl PlaylistEntry {
    /// A file of the user's own: not a stream, nor a path that a playlist
    /// from the network named.
    pub fn is_local(&self) -> bool {
        !self.from_network && !crate::util::is_url(&self.filename)
    }

    /// Whether opening it again elsewhere (another window, a paste) is
    /// safe. Reopened, an entry loads as if the user had opened it, so of
    /// what any playlist named, only what mpv would open from it: a network
    /// stream, or from a playlist on disk also a file; never one of mpv's
    /// own protocols (av://, edl:// and the rest), which it refuses from a
    /// playlist. Of a link's playlist, only a stream a link could open.
    pub fn may_reopen(&self, links: &crate::scheme::LinkSources) -> bool {
        // Opened directly, by the user.
        if self.playlist.is_empty() {
            return true;
        }
        if crate::scheme::from_link(links, &self.playlist) {
            return crate::scheme::is_stream(&self.filename);
        }
        if crate::util::is_url(&self.filename) {
            let file = !crate::util::is_remote(&self.filename) && self.filename.contains("://");
            crate::scheme::is_network_stream(&self.filename) || (file && !self.from_network)
        } else {
            !self.from_network
        }
    }

    /// Local files are listed by file name, like IINA; streams by title.
    pub fn label(&self) -> String {
        if self.title.is_empty() || !crate::util::is_url(&self.filename) {
            file_name(&self.filename)
        } else {
            self.title.clone()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct VideoParams {
    pub width: i64,
    pub height: i64,
    /// Display size, after applying the pixel aspect ratio.
    pub display_width: i64,
    pub display_height: i64,
    pub aspect: f64,
    pub rotate: i64,
    pub pixel_format: String,
    pub colormatrix: String,
    pub primaries: String,
    pub gamma: String,
}

impl VideoParams {
    fn from_node(node: &Node) -> Option<Self> {
        let int = |key| node.get(key).and_then(Node::as_i64).unwrap_or(0);
        let params = Self {
            width: int("w"),
            height: int("h"),
            display_width: int("dw"),
            display_height: int("dh"),
            aspect: node.get("aspect").and_then(Node::as_f64).unwrap_or(0.0),
            rotate: int("rotate"),
            pixel_format: node.str_or_default("pixelformat"),
            colormatrix: node.str_or_default("colormatrix"),
            primaries: node.str_or_default("primaries"),
            gamma: node.str_or_default("gamma"),
        };
        (params.width > 0 && params.height > 0).then_some(params)
    }

    /// The displayed size with rotation applied.
    pub fn oriented_size(&self) -> (i64, i64) {
        let (w, h) = if self.display_width > 0 {
            (self.display_width, self.display_height)
        } else {
            (self.width, self.height)
        };
        if self.rotate % 180 == 90 {
            (h, w)
        } else {
            (w, h)
        }
    }

    pub fn is_hdr(&self) -> bool {
        matches!(self.gamma.as_str(), "pq" | "hlg")
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AudioDevice {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Filter {
    pub name: String,
    pub label: String,
    pub enabled: bool,
    pub params: Vec<(String, String)>,
}

impl Filter {
    fn list_from_node(node: &Node) -> Vec<Self> {
        node.as_array()
            .iter()
            .map(|filter| Self {
                name: filter.str_or_default("name"),
                label: filter.str_or_default("label"),
                enabled: filter.bool_or("enabled", true),
                params: match filter.get("params") {
                    Some(Node::Map(params)) => params
                        .iter()
                        .map(|(key, value)| {
                            (key.clone(), value.as_str().unwrap_or_default().to_owned())
                        })
                        .collect(),
                    _ => Vec::new(),
                },
            })
            .collect()
    }
}

/// What [`PlaybackState::audio_status`] makes of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioStatus {
    Unknown,
    Audio,
    NotAudio,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackState {
    pub idle: bool,
    pub paused: bool,
    pub core_idle: bool,
    pub paused_for_cache: bool,
    pub cache_buffering: i64,
    pub seeking: bool,
    pub seekable: bool,
    pub eof: bool,
    pub position: f64,
    pub duration: f64,
    pub cache_time: f64,
    pub volume: f64,
    pub volume_max: f64,
    pub muted: bool,
    pub speed: f64,
    pub title: String,
    pub path: String,
    pub filename: String,
    pub file_format: String,
    pub file_size: i64,
    pub metadata: Vec<(String, String)>,
    /// The current chapter's own tags, such as its performer.
    pub chapter_metadata: Vec<(String, String)>,
    pub playlist: Vec<PlaylistEntry>,
    pub playlist_pos: i64,
    pub tracks: Vec<Track>,
    pub chapters: Vec<Chapter>,
    pub chapter: i64,
    pub vid: Option<i64>,
    pub aid: Option<i64>,
    pub sid: Option<i64>,
    pub secondary_sid: Option<i64>,
    pub audio_delay: f64,
    pub sub_delay: f64,
    pub secondary_sub_delay: f64,
    pub sub_scale: f64,
    pub sub_pos: f64,
    pub sub_visible: bool,
    pub secondary_sub_visible: bool,
    pub video: Option<VideoParams>,
    /// The aspect ratio override, or 0 for the video's own.
    pub aspect_override: f64,
    pub rotate: i64,
    pub crop: String,
    pub zoom: f64,
    pub video_filters: Vec<Filter>,
    pub audio_filters: Vec<Filter>,
    pub deinterlace: bool,
    pub hwdec: String,
    pub hwdec_current: String,
    pub brightness: i64,
    pub contrast: i64,
    pub saturation: i64,
    pub gamma: i64,
    pub hue: i64,
    pub loop_file: bool,
    pub loop_playlist: bool,
    pub ab_loop_a: Option<f64>,
    pub ab_loop_b: Option<f64>,
    pub shuffle: bool,
    pub audio_devices: Vec<AudioDevice>,
    pub audio_device: String,
    pub container_fps: f64,
    pub estimated_fps: f64,
    pub audio_samplerate: i64,
    pub audio_channels: String,
    pub audio_codec: String,
    pub video_codec: String,
    pub fullscreen: bool,
    pub ontop: bool,
    pub window_scale: f64,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            idle: true,
            paused: false,
            core_idle: true,
            paused_for_cache: false,
            cache_buffering: 0,
            seeking: false,
            seekable: false,
            eof: false,
            position: 0.0,
            duration: 0.0,
            cache_time: 0.0,
            volume: 100.0,
            volume_max: 130.0,
            muted: false,
            speed: 1.0,
            title: String::new(),
            path: String::new(),
            filename: String::new(),
            file_format: String::new(),
            file_size: 0,
            metadata: Vec::new(),
            chapter_metadata: Vec::new(),
            playlist: Vec::new(),
            playlist_pos: -1,
            tracks: Vec::new(),
            chapters: Vec::new(),
            chapter: -1,
            vid: None,
            aid: None,
            sid: None,
            secondary_sid: None,
            audio_delay: 0.0,
            sub_delay: 0.0,
            secondary_sub_delay: 0.0,
            sub_scale: 1.0,
            sub_pos: 100.0,
            sub_visible: true,
            secondary_sub_visible: true,
            video: None,
            aspect_override: 0.0,
            rotate: 0,
            crop: String::new(),
            zoom: 0.0,
            video_filters: Vec::new(),
            audio_filters: Vec::new(),
            deinterlace: false,
            hwdec: "no".into(),
            hwdec_current: String::new(),
            brightness: 0,
            contrast: 0,
            saturation: 0,
            gamma: 0,
            hue: 0,
            loop_file: false,
            loop_playlist: false,
            ab_loop_a: None,
            ab_loop_b: None,
            shuffle: false,
            audio_devices: Vec::new(),
            audio_device: String::new(),
            container_fps: 0.0,
            estimated_fps: 0.0,
            audio_samplerate: 0,
            audio_channels: String::new(),
            audio_codec: String::new(),
            video_codec: String::new(),
            fullscreen: false,
            ontop: false,
            window_scale: 1.0,
        }
    }
}

/// Stores `value` in `slot`, reporting whether it differed.
fn set<T: PartialEq>(slot: &mut T, value: T) -> bool {
    if *slot == value {
        false
    } else {
        *slot = value;
        true
    }
}

fn flag(value: &Node) -> bool {
    match value {
        Node::Flag(flag) => *flag,
        Node::String(text) => !matches!(text.as_str(), "no" | ""),
        _ => false,
    }
}

fn number(value: &Node) -> f64 {
    value.as_f64().unwrap_or(0.0)
}

fn int(value: &Node) -> i64 {
    value.as_i64().unwrap_or(0)
}

/// A string property; string lists (such as `hwdec`) are joined with commas.
fn text(value: &Node) -> String {
    match value {
        Node::Array(items) => items
            .iter()
            .filter_map(Node::as_str)
            .collect::<Vec<_>>()
            .join(","),
        _ => value.as_str().unwrap_or_default().to_owned(),
    }
}

/// A tag map such as `metadata`, as (key, value) pairs.
fn tags(value: &Node) -> Vec<(String, String)> {
    match value {
        Node::Map(entries) => entries
            .iter()
            .map(|(key, value)| (key.clone(), text(value)))
            .collect(),
        _ => Vec::new(),
    }
}

/// An aspect ratio given as a number, "w:h", or "no"/"-1" for none.
fn aspect_ratio(value: &Node) -> f64 {
    let ratio = match value {
        Node::String(text) => match text.split_once(':') {
            Some((w, h)) => w
                .trim()
                .parse::<f64>()
                .ok()
                .zip(h.trim().parse::<f64>().ok())
                .map(|(w, h)| w / h),
            None => text.trim().parse().ok(),
        },
        other => other.as_f64(),
    };
    ratio
        .filter(|ratio| ratio.is_finite() && *ratio > 0.0)
        .unwrap_or(0.0)
}

/// A track selection property: an id, or "no"/"auto" for none.
fn track_id(value: &Node) -> Option<i64> {
    value.as_i64()
}

/// "no" or a timestamp.
fn optional_time(value: &Node) -> Option<f64> {
    value.as_f64()
}

/// `loop-file`/`loop-playlist`: "inf", "force", a count, or "no".
fn loop_enabled(value: &Node) -> bool {
    match value {
        Node::String(text) => text != "no",
        Node::Int(count) => *count > 0,
        Node::Flag(flag) => *flag,
        _ => false,
    }
}

impl PlaybackState {
    /// Applies one property change. Returns false when nothing changed.
    pub fn apply(&mut self, prop: Prop, value: &Node) -> bool {
        match prop {
            Prop::Idle => set(&mut self.idle, flag(value)),
            Prop::Pause => set(&mut self.paused, flag(value)),
            Prop::CoreIdle => set(&mut self.core_idle, flag(value)),
            Prop::PausedForCache => set(&mut self.paused_for_cache, flag(value)),
            Prop::CacheBuffering => set(&mut self.cache_buffering, int(value)),
            Prop::Seeking => set(&mut self.seeking, flag(value)),
            Prop::Seekable => set(&mut self.seekable, flag(value)),
            Prop::EofReached => set(&mut self.eof, flag(value)),
            Prop::TimePos => set(&mut self.position, number(value).max(0.0)),
            Prop::Duration => set(&mut self.duration, number(value).max(0.0)),
            Prop::DemuxerCacheTime => set(&mut self.cache_time, number(value)),
            Prop::Volume => set(&mut self.volume, number(value)),
            Prop::VolumeMax => set(&mut self.volume_max, value.as_f64().unwrap_or(130.0)),
            Prop::Mute => set(&mut self.muted, flag(value)),
            Prop::Speed => set(&mut self.speed, value.as_f64().unwrap_or(1.0)),
            Prop::MediaTitle => set(&mut self.title, text(value)),
            Prop::Path => set(&mut self.path, text(value)),
            Prop::Filename => set(&mut self.filename, text(value)),
            Prop::FileFormat => set(&mut self.file_format, text(value)),
            Prop::FileSize => set(&mut self.file_size, int(value)),
            Prop::Metadata => set(&mut self.metadata, tags(value)),
            Prop::ChapterMetadata => set(&mut self.chapter_metadata, tags(value)),
            Prop::Playlist => set(
                &mut self.playlist,
                value
                    .as_array()
                    .iter()
                    .map(|entry| PlaylistEntry {
                        filename: entry.str_or_default("filename"),
                        title: entry.str_or_default("title"),
                        current: entry.bool_or("current", false),
                        id: entry.get("id").and_then(Node::as_i64).unwrap_or(0),
                        playlist: entry.str_or_default("playlist-path"),
                        from_network: crate::util::is_remote(
                            &entry.str_or_default("playlist-path"),
                        ),
                    })
                    .collect(),
            ),
            Prop::PlaylistPos => set(&mut self.playlist_pos, value.as_i64().unwrap_or(-1)),
            Prop::TrackList => set(
                &mut self.tracks,
                value
                    .as_array()
                    .iter()
                    .filter_map(Track::from_node)
                    .collect(),
            ),
            Prop::ChapterList => set(
                &mut self.chapters,
                value
                    .as_array()
                    .iter()
                    .enumerate()
                    .map(|(index, chapter)| Chapter {
                        title: Some(chapter.str_or_default("title"))
                            .filter(|title| !title.is_empty())
                            .unwrap_or_else(|| format!("Chapter {}", index + 1)),
                        time: chapter.get("time").and_then(Node::as_f64).unwrap_or(0.0),
                    })
                    .collect(),
            ),
            Prop::Chapter => set(&mut self.chapter, value.as_i64().unwrap_or(-1)),
            Prop::Vid => set(&mut self.vid, track_id(value)),
            Prop::Aid => set(&mut self.aid, track_id(value)),
            Prop::Sid => set(&mut self.sid, track_id(value)),
            Prop::SecondarySid => set(&mut self.secondary_sid, track_id(value)),
            Prop::AudioDelay => set(&mut self.audio_delay, number(value)),
            Prop::SubDelay => set(&mut self.sub_delay, number(value)),
            Prop::SecondarySubDelay => set(&mut self.secondary_sub_delay, number(value)),
            Prop::SubScale => set(&mut self.sub_scale, value.as_f64().unwrap_or(1.0)),
            Prop::SubPos => set(&mut self.sub_pos, value.as_f64().unwrap_or(100.0)),
            Prop::SubVisibility => set(&mut self.sub_visible, flag(value)),
            Prop::SecondarySubVisibility => set(&mut self.secondary_sub_visible, flag(value)),
            Prop::VideoParams => set(&mut self.video, VideoParams::from_node(value)),
            Prop::VideoAspectOverride => set(&mut self.aspect_override, aspect_ratio(value)),
            Prop::VideoRotate => set(&mut self.rotate, int(value)),
            Prop::VideoCrop => set(&mut self.crop, text(value)),
            Prop::VideoZoom => set(&mut self.zoom, number(value)),
            Prop::VideoFilters => set(&mut self.video_filters, Filter::list_from_node(value)),
            Prop::AudioFilters => set(&mut self.audio_filters, Filter::list_from_node(value)),
            Prop::Deinterlace => set(&mut self.deinterlace, flag(value)),
            Prop::Hwdec => set(&mut self.hwdec, text(value)),
            Prop::HwdecCurrent => set(&mut self.hwdec_current, text(value)),
            Prop::Brightness => set(&mut self.brightness, int(value)),
            Prop::Contrast => set(&mut self.contrast, int(value)),
            Prop::Saturation => set(&mut self.saturation, int(value)),
            Prop::Gamma => set(&mut self.gamma, int(value)),
            Prop::Hue => set(&mut self.hue, int(value)),
            Prop::LoopFile => set(&mut self.loop_file, loop_enabled(value)),
            Prop::LoopPlaylist => set(&mut self.loop_playlist, loop_enabled(value)),
            Prop::AbLoopA => set(&mut self.ab_loop_a, optional_time(value)),
            Prop::AbLoopB => set(&mut self.ab_loop_b, optional_time(value)),
            Prop::Shuffle => set(&mut self.shuffle, flag(value)),
            Prop::AudioDeviceList => set(
                &mut self.audio_devices,
                value
                    .as_array()
                    .iter()
                    .map(|device| AudioDevice {
                        name: device.str_or_default("name"),
                        description: device.str_or_default("description"),
                    })
                    .collect(),
            ),
            Prop::AudioDevice => set(&mut self.audio_device, text(value)),
            Prop::ContainerFps => set(&mut self.container_fps, number(value)),
            Prop::EstimatedVfFps => set(&mut self.estimated_fps, number(value)),
            Prop::AudioParams => {
                set(
                    &mut self.audio_samplerate,
                    value.get("samplerate").and_then(Node::as_i64).unwrap_or(0),
                ) | set(
                    &mut self.audio_channels,
                    value.str_or_default("hr-channels"),
                )
            }
            Prop::AudioCodecName => set(&mut self.audio_codec, text(value)),
            Prop::VideoCodec => set(&mut self.video_codec, text(value)),
            Prop::Fullscreen => set(&mut self.fullscreen, flag(value)),
            Prop::Ontop => set(&mut self.ontop, flag(value)),
            Prop::WindowScale => set(&mut self.window_scale, value.as_f64().unwrap_or(1.0)),
        }
    }

    pub fn tracks(&self, kind: TrackKind) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(move |track| track.kind == kind)
    }

    pub fn selected_track(&self, kind: TrackKind) -> Option<&Track> {
        let id = match kind {
            TrackKind::Video => self.vid,
            TrackKind::Audio => self.aid,
            TrackKind::Sub => self.sid,
        }?;
        self.tracks(kind).find(|track| track.id == id)
    }

    /// True for audio files, including those with only cover art.
    pub fn is_audio_only(&self) -> bool {
        !self.idle
            && self.tracks(TrackKind::Audio).next().is_some()
            && self
                .tracks(TrackKind::Video)
                .all(|track| track.albumart || track.image)
    }

    pub fn has_video(&self) -> bool {
        self.video.is_some()
    }

    /// Whether the file is music, as IINA decides it for music mode: never
    /// for a stream, not yet while its tracks aren't known, and otherwise
    /// when it has no video but covers.
    pub fn audio_status(&self) -> AudioStatus {
        if self.idle || crate::util::is_remote(&self.path) {
            return if self.idle {
                AudioStatus::Unknown
            } else {
                AudioStatus::NotAudio
            };
        }
        let mut video = self.tracks(TrackKind::Video).peekable();
        if video.peek().is_none() && self.tracks(TrackKind::Audio).next().is_none() {
            return AudioStatus::Unknown;
        }
        if video.all(|track| track.albumart) {
            AudioStatus::Audio
        } else {
            AudioStatus::NotAudio
        }
    }

    /// The aspect ratio of the picture the file shows (its video, or its
    /// cover), or None when it shows none. None of either while its tracks
    /// aren't known yet: the picture of the file before stays until then.
    pub fn picture_aspect(&self) -> Option<Option<f64>> {
        if self.idle {
            return Some(None);
        }
        if let Some(video) = &self.video {
            let (width, height) = video.oriented_size();
            if width > 0 && height > 0 {
                return Some(Some(width as f64 / height as f64));
            }
        }
        if self.tracks.is_empty() {
            return None;
        }
        // A cover not yet decoded, sized by its stream as IINA does.
        Some(self.selected_track(TrackKind::Video).map(|track| {
            if track.width > 0 && track.height > 0 {
                track.width as f64 / track.height as f64
            } else {
                1.0
            }
        }))
    }

    /// Title, artist and album as IINA shows music: an album in one file
    /// is titled by its chapters, with each chapter's performer.
    pub fn music_metadata(&self) -> (String, String, String) {
        let tag = |tags: &[(String, String)], keys: &[&str]| {
            keys.iter()
                .find_map(|wanted| {
                    tags.iter()
                        .find(|(key, value)| key.eq_ignore_ascii_case(wanted) && !value.is_empty())
                })
                .map(|(_, value)| value.clone())
        };
        let artist = tag(&self.metadata, &["artist", "album_artist", "performer"]);
        let album = tag(&self.metadata, &["album"]).unwrap_or_default();
        if let Some(chapter) = self.current_chapter() {
            let title = Some(chapter.title.clone())
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| self.title.clone());
            // Matroska names it LEAD_PERFORMER.
            let artist = tag(
                &self.chapter_metadata,
                &["performer", "lead_performer", "artist"],
            )
            .or(artist);
            return (title, artist.unwrap_or_default(), album);
        }
        (self.title.clone(), artist.unwrap_or_default(), album)
    }

    pub fn current_chapter(&self) -> Option<&Chapter> {
        usize::try_from(self.chapter)
            .ok()
            .and_then(|index| self.chapters.get(index))
    }
}

pub fn file_name(path: &str) -> String {
    if crate::util::is_url(path) {
        return path.to_owned();
    }
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned())
}

fn trim_float(value: f64, decimals: usize) -> String {
    let text = format!("{value:.decimals$}");
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text
    }
}

fn channel_layout(channels: i64) -> String {
    match channels {
        1 => "mono".into(),
        2 => "stereo".into(),
        6 => "5.1".into(),
        8 => "7.1".into(),
        n => format!("{n} ch"),
    }
}

/// English names for the ISO 639-1 and 639-2 codes media files use most.
pub fn language_name(code: &str) -> Option<&'static str> {
    let code = code.to_ascii_lowercase();
    Some(match code.as_str() {
        "en" | "eng" => "English",
        "ja" | "jpn" => "Japanese",
        "zh" | "chi" | "zho" => "Chinese",
        "chs" | "zh-hans" | "zh-cn" => "Chinese (Simplified)",
        "cht" | "zh-hant" | "zh-tw" => "Chinese (Traditional)",
        "ko" | "kor" => "Korean",
        "fr" | "fre" | "fra" => "French",
        "de" | "ger" | "deu" => "German",
        "es" | "spa" => "Spanish",
        "it" | "ita" => "Italian",
        "pt" | "por" => "Portuguese",
        "pt-br" => "Portuguese (Brazil)",
        "ru" | "rus" => "Russian",
        "ar" | "ara" => "Arabic",
        "hi" | "hin" => "Hindi",
        "nl" | "dut" | "nld" => "Dutch",
        "sv" | "swe" => "Swedish",
        "no" | "nor" | "nb" | "nob" => "Norwegian",
        "da" | "dan" => "Danish",
        "fi" | "fin" => "Finnish",
        "pl" | "pol" => "Polish",
        "cs" | "cze" | "ces" => "Czech",
        "hu" | "hun" => "Hungarian",
        "el" | "gre" | "ell" => "Greek",
        "tr" | "tur" => "Turkish",
        "he" | "heb" => "Hebrew",
        "th" | "tha" => "Thai",
        "vi" | "vie" => "Vietnamese",
        "id" | "ind" => "Indonesian",
        "ms" | "may" | "msa" => "Malay",
        "uk" | "ukr" => "Ukrainian",
        "ro" | "rum" | "ron" => "Romanian",
        "bg" | "bul" => "Bulgarian",
        "hr" | "hrv" => "Croatian",
        "sr" | "srp" => "Serbian",
        "sk" | "slo" | "slk" => "Slovak",
        "sl" | "slv" => "Slovenian",
        "ca" | "cat" => "Catalan",
        "fa" | "per" | "fas" => "Persian",
        "bn" | "ben" => "Bengali",
        "ta" | "tam" => "Tamil",
        "und" => "Undetermined",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: Vec<(&str, Node)>) -> Node {
        Node::Map(
            entries
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
        )
    }

    fn string(value: &str) -> Node {
        Node::String(value.to_owned())
    }

    #[test]
    fn track_list_parses_and_labels_tracks() {
        let mut state = PlaybackState::default();
        let tracks = Node::Array(vec![
            map(vec![
                ("id", Node::Int(1)),
                ("type", string("video")),
                ("codec", string("h264")),
                ("demux-w", Node::Int(1920)),
                ("demux-h", Node::Int(1080)),
                ("demux-fps", Node::Double(23.976)),
                ("selected", Node::Flag(true)),
            ]),
            map(vec![
                ("id", Node::Int(1)),
                ("type", string("audio")),
                ("lang", string("jpn")),
                ("codec", string("aac")),
                ("demux-channel-count", Node::Int(6)),
                ("demux-samplerate", Node::Int(48000)),
                ("default", Node::Flag(true)),
            ]),
            map(vec![
                ("id", Node::Int(2)),
                ("type", string("sub")),
                ("title", string("Signs & Songs")),
                ("lang", string("en")),
                ("forced", Node::Flag(true)),
            ]),
            map(vec![("id", Node::Int(9)), ("type", string("unknown"))]),
        ]);
        assert!(state.apply(Prop::TrackList, &tracks));
        assert_eq!(state.tracks.len(), 3);
        let video = &state.tracks[0];
        assert_eq!(video.label(), "Track 1");
        assert_eq!(video.detail(), "H264 · 1920×1080 · 23.976 fps");
        let audio = &state.tracks[1];
        assert_eq!(audio.label(), "Japanese");
        assert_eq!(audio.detail(), "AAC · 5.1 · 48 kHz · default");
        let sub = &state.tracks[2];
        assert_eq!(sub.label(), "Signs & Songs (English)");
        assert_eq!(sub.detail(), "forced");
        assert!(
            !state.apply(Prop::TrackList, &tracks),
            "unchanged values report no change"
        );
    }

    #[test]
    fn selection_and_loop_values_accept_mpv_spellings() {
        let mut state = PlaybackState::default();
        state.apply(Prop::Sid, &Node::Int(3));
        assert_eq!(state.sid, Some(3));
        state.apply(Prop::Sid, &Node::Flag(false));
        assert_eq!(state.sid, None);
        state.apply(Prop::LoopFile, &string("inf"));
        assert!(state.loop_file);
        state.apply(Prop::LoopFile, &string("no"));
        assert!(!state.loop_file);
        state.apply(Prop::AbLoopA, &Node::Double(12.5));
        assert_eq!(state.ab_loop_a, Some(12.5));
        state.apply(Prop::AbLoopA, &string("no"));
        assert_eq!(state.ab_loop_a, None);
        state.apply(Prop::Deinterlace, &string("auto"));
        assert!(state.deinterlace);
        state.apply(
            Prop::Hwdec,
            &Node::Array(vec![string("vaapi"), string("auto-safe")]),
        );
        assert_eq!(state.hwdec, "vaapi,auto-safe");
    }

    #[test]
    fn audio_files_with_cover_art_count_as_audio_only() {
        let mut state = PlaybackState::default();
        state.apply(Prop::Idle, &Node::Flag(false));
        state.apply(
            Prop::TrackList,
            &Node::Array(vec![
                map(vec![
                    ("id", Node::Int(1)),
                    ("type", string("video")),
                    ("albumart", Node::Flag(true)),
                ]),
                map(vec![("id", Node::Int(1)), ("type", string("audio"))]),
            ]),
        );
        assert!(state.is_audio_only());
    }

    #[test]
    fn music_is_told_as_iina_tells_it() {
        let mut state = PlaybackState::default();
        assert_eq!(state.audio_status(), AudioStatus::Unknown);
        state.apply(Prop::Idle, &Node::Flag(false));
        state.apply(Prop::Path, &string("/music/a.flac"));
        assert_eq!(state.audio_status(), AudioStatus::Unknown, "no tracks yet");
        assert_eq!(state.picture_aspect(), None, "the picture before stays");
        let cover = map(vec![
            ("id", Node::Int(1)),
            ("type", string("video")),
            ("albumart", Node::Flag(true)),
            ("demux-w", Node::Int(500)),
            ("demux-h", Node::Int(400)),
            ("selected", Node::Flag(true)),
        ]);
        let audio = map(vec![("id", Node::Int(1)), ("type", string("audio"))]);
        state.apply(Prop::TrackList, &Node::Array(vec![cover, audio.clone()]));
        state.apply(Prop::Vid, &Node::Int(1));
        assert_eq!(state.audio_status(), AudioStatus::Audio);
        assert_eq!(state.picture_aspect(), Some(Some(1.25)));
        state.apply(Prop::Vid, &Node::Flag(false));
        assert_eq!(state.picture_aspect(), Some(None));
        // An image that isn't a cover is a picture to look at.
        let image = map(vec![
            ("id", Node::Int(1)),
            ("type", string("video")),
            ("image", Node::Flag(true)),
        ]);
        state.apply(Prop::TrackList, &Node::Array(vec![image, audio.clone()]));
        assert_eq!(state.audio_status(), AudioStatus::NotAudio);
        // Streams never count as music.
        state.apply(Prop::TrackList, &Node::Array(vec![audio]));
        assert_eq!(state.audio_status(), AudioStatus::Audio);
        state.apply(Prop::Path, &string("https://radio.example/stream"));
        assert_eq!(state.audio_status(), AudioStatus::NotAudio);
    }

    #[test]
    fn music_titles_follow_chapters() {
        let mut state = PlaybackState::default();
        state.apply(Prop::MediaTitle, &string("The Album"));
        state.apply(
            Prop::Metadata,
            &map(vec![
                ("ARTIST", string("Band")),
                ("Album", string("The Album")),
            ]),
        );
        assert_eq!(
            state.music_metadata(),
            ("The Album".into(), "Band".into(), "The Album".into())
        );
        state.apply(
            Prop::ChapterList,
            &Node::Array(vec![map(vec![
                ("time", Node::Double(0.0)),
                ("title", string("First Song")),
            ])]),
        );
        state.apply(Prop::Chapter, &Node::Int(0));
        state.apply(
            Prop::ChapterMetadata,
            &map(vec![("LEAD_PERFORMER", string("Guest"))]),
        );
        assert_eq!(
            state.music_metadata(),
            ("First Song".into(), "Guest".into(), "The Album".into())
        );
    }

    #[test]
    fn rotation_swaps_the_displayed_size() {
        let params = VideoParams {
            width: 1920,
            height: 1080,
            display_width: 1920,
            display_height: 1080,
            rotate: 90,
            ..Default::default()
        };
        assert_eq!(params.oriented_size(), (1080, 1920));
    }

    #[test]
    fn chapters_get_fallback_titles() {
        let mut state = PlaybackState::default();
        state.apply(
            Prop::ChapterList,
            &Node::Array(vec![
                map(vec![
                    ("time", Node::Double(0.0)),
                    ("title", string("Intro")),
                ]),
                map(vec![("time", Node::Double(90.0))]),
            ]),
        );
        assert_eq!(state.chapters[1].title, "Chapter 2");
        state.apply(Prop::Chapter, &Node::Int(1));
        assert_eq!(state.current_chapter().map(|c| c.time), Some(90.0));
    }
}
