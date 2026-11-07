//! The Media Info window: what the active player's file says about itself
//! (its tags, cover and location) and each of its streams' codec and
//! format.
//!
//! Unlike the inspector it polls nothing: it reads the properties it shows
//! when it opens, when it follows another window, and when the window it
//! follows says that the file, its tags or its tracks changed. The reads are
//! mpv's asynchronous ones, and their replies come back through the
//! player's event pump to `reply`; the cover is the decoded picture of the
//! cover art track, taken with `screenshot-raw`.

use crate::app::App;
use crate::inspector::{decimals, format_bytes, set_rows};
use crate::mpv::{self, Node};
use crate::player::state::{file_name, language_name};
use crate::window::PlayerController;
use crate::{InspectorRow, MediaInfoWindow};
use slint::{ComponentHandle, Image, Model, ModelRc, Rgb8Pixel, SharedPixelBuffer, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

/// Userdata of the window's reads: this, plus the round and the index of
/// the property in it, or `COVER` for the cover. The inspector's userdata
/// never sets this bit.
pub const REPLY_BASE: u64 = 1 << 41;
const COVER: u64 = 1 << 32;

/// What is read each round.
const PROPERTIES: [&str; 7] = [
    "path",
    "media-title",
    "filename",
    "metadata",
    "track-list",
    "video-params",
    "audio-params",
];

/// The longest side of the cover as kept: a cover can be thousands of pixels
/// across, and the window shows it at a few hundred.
const COVER_SIZE: u32 = 640;

/// Whether `userdata` is one of this window's reads.
pub fn is_reply(userdata: u64) -> bool {
    userdata & REPLY_BASE != 0
}

type Values = HashMap<String, Node>;

pub struct MediaInfoController {
    window: MediaInfoWindow,
    general: Rc<VecModel<InspectorRow>>,
    codec: Rc<VecModel<InspectorRow>>,
    target: RefCell<Weak<PlayerController>>,
    shown: Cell<bool>,
    round: Cell<u64>,
    pending: Cell<usize>,
    values: RefCell<Values>,
    /// The file and track of the cover shown, which isn't taken again while
    /// they stay.
    cover: RefCell<Option<(String, i64)>>,
}

impl MediaInfoController {
    pub fn new(_app: &Rc<App>) -> Result<Rc<Self>, String> {
        let window = MediaInfoWindow::new().map_err(|error| error.to_string())?;
        let general = Rc::new(VecModel::default());
        let codec = Rc::new(VecModel::default());
        window.set_general(ModelRc::from(general.clone()));
        window.set_codec(ModelRc::from(codec.clone()));
        let controller = Rc::new(Self {
            window,
            general,
            codec,
            target: RefCell::new(Weak::new()),
            shown: Cell::new(false),
            round: Cell::new(0),
            pending: Cell::new(0),
            values: RefCell::new(HashMap::new()),
            cover: RefCell::new(None),
        });
        controller.wire();
        Ok(controller)
    }

    pub fn show(&self) {
        if let Err(error) = self.window.show() {
            log::warn!("could not show the media info: {error}");
            return;
        }
        self.shown.set(true);
        self.refresh();
    }

    fn wire(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.window.on_dismiss(move || {
            if let Some(this) = weak.upgrade() {
                this.hide();
            }
        });
        // Nothing is read while the window is closed.
        let weak = Rc::downgrade(self);
        self.window.window().on_close_requested(move || {
            if let Some(this) = weak.upgrade() {
                this.shown.set(false);
            }
            slint::CloseRequestResponse::HideWindow
        });
    }

    fn hide(&self) {
        self.shown.set(false);
        if let Err(error) = self.window.hide() {
            log::warn!("could not hide the media info: {error}");
        }
    }

    /// Describes `player` from now on: the window focused most recently.
    pub fn follow(&self, player: &Rc<PlayerController>) {
        let same = self
            .target
            .borrow()
            .upgrade()
            .is_some_and(|target| Rc::ptr_eq(&target, player));
        if !same {
            *self.target.borrow_mut() = Rc::downgrade(player);
            self.refresh();
        }
    }

    /// `player`'s file, tags or tracks changed.
    pub fn changed(&self, player: &PlayerController) {
        let followed = self
            .target
            .borrow()
            .upgrade()
            .is_some_and(|target| std::ptr::eq(Rc::as_ptr(&target), player));
        if followed {
            self.refresh();
        }
    }

    /// Starts a round of reads, when the window is open.
    fn refresh(&self) {
        if !self.shown.get() {
            return;
        }
        let round = self.round.get().wrapping_add(1) & 0xff_ffff;
        self.round.set(round);
        self.values.borrow_mut().clear();
        let Some(target) = self.target.borrow().upgrade() else {
            self.pending.set(0);
            self.render();
            return;
        };
        self.pending.set(PROPERTIES.len());
        for (index, name) in PROPERTIES.iter().enumerate() {
            target.request_property(REPLY_BASE | (round << 8) | index as u64, name);
        }
    }

    /// A property read, or the cover taken, for the window arrived.
    pub fn reply(&self, userdata: u64, result: mpv::Result<Node>) {
        if userdata & COVER != 0 {
            if (userdata & 0xff_ffff) == self.round.get() {
                self.show_cover(result);
            }
            return;
        }
        let round = (userdata >> 8) & 0xff_ffff;
        let index = (userdata & 0xff) as usize;
        if round != self.round.get() || self.pending.get() == 0 {
            return;
        }
        let Some(name) = PROPERTIES.get(index) else {
            return;
        };
        // An unavailable property is shown as such.
        if let Ok(node) = result {
            self.values.borrow_mut().insert((*name).to_owned(), node);
        }
        self.pending.set(self.pending.get() - 1);
        if self.pending.get() == 0 {
            self.render();
        }
    }

    fn render(&self) {
        let target = self.target.borrow().upgrade();
        let values = self.values.borrow();
        let playing = values
            .get("path")
            .and_then(Node::as_str)
            .is_some_and(|path| !path.is_empty());
        // The window says itself when nothing plays.
        let subject = match &target {
            Some(target) if playing => target.media_name(),
            _ => String::new(),
        };
        self.window.set_subject(subject.into());
        if playing {
            put_rows(&self.window, &self.general, general(&values));
            put_rows(&self.window, &self.codec, codec(&values));
        } else {
            put_rows(&self.window, &self.general, Vec::new());
            put_rows(&self.window, &self.codec, Vec::new());
        }
        let wanted = playing
            .then(|| cover_track(&values))
            .flatten()
            .zip(values.get("path").and_then(Node::as_str))
            .map(|(track, path)| (path.to_owned(), track));
        drop(values);
        match wanted {
            None => self.clear_cover(),
            Some(wanted) if self.cover.borrow().as_ref() == Some(&wanted) => {}
            Some(_) => {
                self.clear_cover();
                // Taken again when the picture is decoded, which comes with
                // a change of `video-params`, if it isn't yet.
                if let Some(target) = target {
                    let userdata = REPLY_BASE | COVER | self.round.get();
                    target.request_command(userdata, &["screenshot-raw", "video"]);
                }
            }
        }
    }

    fn show_cover(&self, result: mpv::Result<Node>) {
        let picture = match result {
            Ok(node) => cover_picture(&node),
            Err(error) => {
                log::debug!("no cover for the media info: {}", error.description());
                None
            }
        };
        let Some(picture) = picture else {
            return;
        };
        let values = self.values.borrow();
        let path = values.get("path").and_then(Node::as_str).unwrap_or("");
        *self.cover.borrow_mut() = cover_track(&values).map(|track| (path.to_owned(), track));
        drop(values);
        self.window.set_cover(Image::from_rgb8(picture));
        self.window.set_has_cover(true);
    }

    fn clear_cover(&self) {
        if self.cover.borrow_mut().take().is_some() || self.window.get_has_cover() {
            self.window.set_has_cover(false);
            self.window.set_cover(Image::default());
        }
    }
}

/// Shows `rows` in `model`. Rows made anew, as `set_rows` makes them when
/// there are more or fewer, or a heading takes a value's place, take with
/// them a value that may have the focus, which the window's keys then take
/// back (`remade`).
fn put_rows(window: &MediaInfoWindow, model: &VecModel<InspectorRow>, rows: Vec<InspectorRow>) {
    let remade = model.row_count() != rows.len()
        || model
            .iter()
            .zip(&rows)
            .any(|(old, new)| old.section != new.section);
    set_rows(model, rows);
    if remade {
        window.set_remade(window.get_remade().wrapping_add(1));
    }
}

fn section(label: &str) -> InspectorRow {
    InspectorRow {
        section: true,
        label: label.into(),
        ..Default::default()
    }
}

fn row(label: &str, value: Option<String>) -> InspectorRow {
    InspectorRow {
        label: label.into(),
        dim: value.is_none(),
        value: value.unwrap_or_else(|| "N/A".into()).into(),
        ..Default::default()
    }
}

/// A string of a map node; None when missing or empty.
fn text(node: &Node, key: &str) -> Option<String> {
    node.get(key)
        .and_then(Node::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn number(node: &Node, key: &str) -> Option<f64> {
    node.get(key)?.as_f64().filter(|value| *value > 0.0)
}

/// The file's tags as (key, value) pairs, in mpv's order.
fn tags(values: &Values) -> Vec<(String, String)> {
    match values.get("metadata") {
        Some(Node::Map(entries)) => entries
            .iter()
            .filter_map(|(key, value)| {
                let value = match value {
                    Node::String(text) => text.trim().to_owned(),
                    other => other.as_f64().map(|n| decimals(n, 3))?,
                };
                (!value.is_empty()).then(|| (key.clone(), value))
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The tags each row of General shows, by the names containers give them,
/// whatever their case: ID3's, Vorbis comments' and MP4's as FFmpeg reads
/// them.
const TAG_ROWS: [(&str, &[&str]); 6] = [
    ("Title", &["title"]),
    ("Artist", &["artist", "album_artist", "performer"]),
    ("Album", &["album"]),
    ("Date", &["date", "year", "originaldate"]),
    ("Genre", &["genre"]),
    ("Track number", &["track", "tracknumber"]),
];

fn general(values: &Values) -> Vec<InspectorRow> {
    let tags = tags(values);
    let mut used = vec![false; tags.len()];
    let mut find = |names: &[&str]| {
        names.iter().find_map(|name| {
            let index = tags
                .iter()
                .position(|(key, _)| key.eq_ignore_ascii_case(name))?;
            used[index] = true;
            Some(tags[index].1.clone())
        })
    };
    let mut rows = vec![section("Tags")];
    for (label, names) in TAG_ROWS {
        let mut value = find(names);
        // Without a title tag, what mpv calls it: a stream's own title, or
        // the file's name.
        if label == "Title" && value.is_none() {
            value = values
                .get("media-title")
                .and_then(Node::as_str)
                .filter(|title| !title.is_empty())
                .map(str::to_owned);
        }
        rows.push(row(label, value));
    }
    let others: Vec<InspectorRow> = tags
        .iter()
        .zip(&used)
        .filter(|(_, used)| !**used)
        .map(|((key, value), _)| row(&tag_label(key), Some(value.clone())))
        .collect();
    if !others.is_empty() {
        rows.push(section("Other Tags"));
        rows.extend(others);
    }
    rows.push(section("File"));
    let path = values
        .get("path")
        .and_then(Node::as_str)
        .unwrap_or_default();
    rows.push(row(
        "Name",
        values
            .get("filename")
            .and_then(Node::as_str)
            .map(str::to_owned),
    ));
    rows.push(row("Location", Some(location(path))));
    rows
}

/// A tag's key as a label: "ENCODER" and "encoder" both as "Encoder".
fn tag_label(key: &str) -> String {
    let lower = key.replace('_', " ").to_lowercase();
    let mut chars = lower.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Where the file is: a local file's full path, or a stream's address.
/// Standard input, which mpv calls "-", is no file in the working folder.
fn location(path: &str) -> String {
    if path == "-" {
        return "Standard input".to_owned();
    }
    if crate::util::is_url(path) {
        return path.to_owned();
    }
    std::path::absolute(path)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_owned())
}

/// The id of the cover art track being shown, if one is.
fn cover_track(values: &Values) -> Option<i64> {
    values
        .get("track-list")?
        .as_array()
        .iter()
        .find_map(|track| {
            let shown = track.get("type").and_then(Node::as_str) == Some("video")
                && track.bool_or("selected", false)
                && track.bool_or("albumart", false);
            shown
                .then(|| track.get("id").and_then(Node::as_i64))
                .flatten()
        })
}

fn codec(values: &Values) -> Vec<InspectorRow> {
    let tracks = values
        .get("track-list")
        .map(Node::as_array)
        .unwrap_or_default();
    let mut rows = Vec::new();
    for track in tracks {
        let kind = track.get("type").and_then(Node::as_str).unwrap_or("");
        let selected = track.bool_or("selected", false);
        let id = track.get("id").and_then(Node::as_i64).unwrap_or(0);
        let (name, playing) = match kind {
            "video" => ("Video", "shown"),
            "audio" => ("Audio", "playing"),
            "sub" => ("Subtitle", "shown"),
            _ => continue,
        };
        let heading = if selected {
            format!("{name} Track {id} ({playing})")
        } else {
            format!("{name} Track {id}")
        };
        rows.push(section(&heading));
        let codec_name = match (text(track, "codec-desc"), text(track, "codec")) {
            (Some(desc), Some(codec)) if !desc.eq_ignore_ascii_case(&codec) => {
                Some(format!("{desc} ({codec})"))
            }
            (desc, codec) => desc.or(codec),
        };
        rows.push(row(
            if kind == "sub" { "Format" } else { "Codec" },
            codec_name,
        ));
        if let Some(profile) = text(track, "codec-profile") {
            rows.push(row("Profile", Some(profile)));
        }
        rows.push(row("Type", Some(track_type(track, name))));
        rows.push(row(
            "Language",
            text(track, "lang").map(|code| match language_name(&code) {
                Some(language) => format!("{language} ({code})"),
                None => code,
            }),
        ));
        rows.push(row("Title", text(track, "title")));
        // mpv titles an external file by its name, which needn't be said
        // twice.
        if track.bool_or("external", false)
            && let Some(file) = text(track, "external-filename").map(|file| file_name(&file))
            && text(track, "title").as_ref() != Some(&file)
        {
            rows.push(row("File", Some(file)));
        }
        match kind {
            "video" => video_rows(&mut rows, track, selected, values.get("video-params")),
            "audio" => audio_rows(&mut rows, track, selected, values.get("audio-params")),
            _ => {}
        }
    }
    if rows.is_empty() {
        rows.push(section("Tracks"));
        rows.push(row("Tracks", None));
    }
    rows
}

/// "Video", plus what kind of picture or what flags the track has.
fn track_type(track: &Node, name: &str) -> String {
    let mut parts = vec![name.to_owned()];
    for (key, label) in [
        ("albumart", "cover art"),
        ("image", "still image"),
        ("default", "default"),
        ("forced", "forced"),
        ("hearing-impaired", "for the hard of hearing"),
        ("visual-impaired", "for the visually impaired"),
        ("external", "external file"),
    ] {
        // A cover is a still image too, which says nothing more.
        if key == "image" && track.bool_or("albumart", false) {
            continue;
        }
        if track.bool_or(key, false) {
            parts.push(label.to_owned());
        }
    }
    parts.join(", ")
}

fn video_rows(rows: &mut Vec<InspectorRow>, track: &Node, selected: bool, params: Option<&Node>) {
    let size = match (number(track, "demux-w"), number(track, "demux-h")) {
        (Some(width), Some(height)) => Some(format!("{width}×{height}")),
        _ => None,
    };
    rows.push(row("Resolution", size));
    if !track.bool_or("image", false) {
        rows.push(row(
            "Frame rate",
            number(track, "demux-fps").map(|fps| format!("{} fps", decimals(fps, 3))),
        ));
    }
    // Not every mpv says a track's pixel format; of the track shown, the
    // decoded format says it then.
    let pixel_format = text(track, "format-name");
    if pixel_format.is_some() || !selected {
        rows.push(row("Pixel format", pixel_format));
    }
    // What the decoder gives and how its colors are meant is known only of
    // the track being decoded.
    let Some(params) = params.filter(|_| selected) else {
        return;
    };
    let decoded = text(params, "pixelformat").map(|format| match text(params, "hw-pixelformat") {
        Some(hardware) => format!("{format} (decoded in hardware as {hardware})"),
        None => format.clone(),
    });
    rows.push(row("Decoded format", decoded));
    rows.push(row(
        "Color primaries",
        text(params, "primaries").map(|value| color_name(&value)),
    ));
    rows.push(row(
        "Transfer",
        text(params, "gamma").map(|value| color_name(&value)),
    ));
    rows.push(row(
        "Color matrix",
        text(params, "colormatrix").map(|value| color_name(&value)),
    ));
}

fn audio_rows(rows: &mut Vec<InspectorRow>, track: &Node, selected: bool, params: Option<&Node>) {
    let params = params.filter(|_| selected);
    let rate = number(track, "demux-samplerate")
        .or_else(|| params.and_then(|params| number(params, "samplerate")));
    rows.push(row(
        "Sample rate",
        rate.map(|rate| format!("{} kHz", decimals(rate / 1000.0, 3))),
    ));
    let count = number(track, "demux-channel-count")
        .or_else(|| params.and_then(|params| number(params, "channel-count")));
    let layout = text(track, "demux-channels")
        .or_else(|| params.and_then(|params| text(params, "hr-channels")));
    let channels = match (count, layout) {
        (Some(count), Some(layout)) if layout != count.to_string() => {
            Some(format!("{count} ({layout})"))
        }
        (Some(count), _) => Some(count.to_string()),
        (None, layout) => layout,
    };
    rows.push(row("Channels", channels));
    let format =
        text(track, "format-name").or_else(|| params.and_then(|params| text(params, "format")));
    rows.push(row(
        "Sample format",
        format.map(|format| match sample_bits(&format) {
            Some(bits) => format!("{bits} ({format})"),
            None => format,
        }),
    ));
    rows.push(row(
        "Bit rate",
        number(track, "demux-bitrate").map(|bits| format_bytes(bits, "bps")),
    ));
}

/// An FFmpeg or mpv sample format, planar or not, in bits: "s16p" is
/// "16-bit integer". This is the format the decoder gives, which for a
/// 24-bit file is 32-bit.
fn sample_bits(format: &str) -> Option<&'static str> {
    let packed = format.strip_suffix('p').unwrap_or(format);
    Some(match packed {
        "u8" => "8-bit integer",
        "s16" => "16-bit integer",
        "s32" => "32-bit integer",
        "s64" => "64-bit integer",
        "flt" | "float" => "32-bit float",
        "dbl" | "double" => "64-bit float",
        _ => return None,
    })
}

/// mpv's names for color primaries, transfers and matrices, as their
/// standards write them.
fn color_name(value: &str) -> String {
    match value {
        "bt.601" => "BT.601".into(),
        "bt.709" => "BT.709".into(),
        "bt.2020" => "BT.2020".into(),
        "bt.2020-ncl" => "BT.2020 (non-constant luminance)".into(),
        "bt.2020-cl" => "BT.2020 (constant luminance)".into(),
        "bt.1886" => "BT.1886".into(),
        "bt.470m" => "BT.470 M".into(),
        "apple" => "Apple RGB".into(),
        "adobe" => "Adobe RGB".into(),
        "prophoto" => "ProPhoto RGB".into(),
        "dci-p3" => "DCI-P3".into(),
        "display-p3" => "Display P3".into(),
        "srgb" => "sRGB".into(),
        "linear" => "Linear".into(),
        "pq" => "PQ (SMPTE ST 2084), HDR".into(),
        "hlg" => "HLG (ARIB STD-B67), HDR".into(),
        "rgb" => "RGB".into(),
        "ycgco" => "YCgCo".into(),
        other => other.to_owned(),
    }
}

/// The picture `screenshot-raw` gave, made no larger than `COVER_SIZE`.
fn cover_picture(node: &Node) -> Option<SharedPixelBuffer<Rgb8Pixel>> {
    let width = u32::try_from(node.get("w")?.as_i64()?).ok()?;
    let height = u32::try_from(node.get("h")?.as_i64()?).ok()?;
    let stride = usize::try_from(node.get("stride")?.as_i64()?).ok()?;
    // The default format, which has its blue first and a byte of padding.
    if node.get("format").and_then(Node::as_str) != Some("bgr0") || width == 0 || height == 0 {
        return None;
    }
    let Node::Bytes(data) = node.get("data")? else {
        return None;
    };
    let row_bytes = width as usize * 4;
    if stride < row_bytes || data.len() < stride * (height as usize - 1) + row_bytes {
        return None;
    }
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for line in data.chunks(stride).take(height as usize) {
        for [blue, green, red, _] in line[..row_bytes].as_chunks::<4>().0 {
            rgb.extend_from_slice(&[*red, *green, *blue]);
        }
    }
    let mut picture = image::RgbImage::from_raw(width, height, rgb)?;
    let longest = width.max(height);
    if longest > COVER_SIZE {
        let scale = f64::from(COVER_SIZE) / f64::from(longest);
        let scaled = |side: u32| ((f64::from(side) * scale).round() as u32).max(1);
        picture = image::imageops::thumbnail(&picture, scaled(width), scaled(height));
    }
    Some(SharedPixelBuffer::clone_from_slice(
        picture.as_raw(),
        picture.width(),
        picture.height(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::SharedString;
    use std::time::Duration;

    fn map(entries: Vec<(&str, Node)>) -> Node {
        Node::Map(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
        )
    }

    fn string(text: &str) -> Node {
        Node::String(text.to_owned())
    }

    /// The value of `label` under the heading that starts with `heading`.
    fn value_in(rows: &[InspectorRow], heading: &str, label: &str) -> Option<String> {
        let start = rows
            .iter()
            .position(|row| row.section && row.label.starts_with(heading))?;
        value_of(
            &rows[start + 1..]
                .iter()
                .take_while(|row| !row.section)
                .cloned()
                .collect::<Vec<_>>(),
            label,
        )
    }

    fn value_of(rows: &[InspectorRow], label: &str) -> Option<String> {
        rows.iter()
            .find(|row| !row.section && row.label == label)
            .filter(|row| !row.dim)
            .map(|row| row.value.to_string())
    }

    #[test]
    fn general_shows_the_tags_whatever_their_case() {
        // A full path on each system: Windows takes /music/tone.flac to be
        // on the current drive.
        let file = if cfg!(windows) {
            r"C:\music\tone.flac"
        } else {
            "/music/tone.flac"
        };
        let mut values = Values::new();
        values.insert("path".into(), string(file));
        values.insert("filename".into(), string("tone.flac"));
        values.insert("media-title".into(), string("Tone"));
        values.insert(
            "metadata".into(),
            map(vec![
                ("TITLE", string("A Tone")),
                ("ARTIST", string("Patina")),
                ("DATE", string("2026")),
                ("track", string("3/12")),
                ("ENCODER", string("Lavf")),
                ("Empty", string(" ")),
            ]),
        );
        let rows = general(&values);
        assert_eq!(value_of(&rows, "Title").as_deref(), Some("A Tone"));
        assert_eq!(value_of(&rows, "Artist").as_deref(), Some("Patina"));
        assert_eq!(value_of(&rows, "Date").as_deref(), Some("2026"));
        assert_eq!(value_of(&rows, "Track number").as_deref(), Some("3/12"));
        assert_eq!(value_of(&rows, "Album"), None);
        assert_eq!(value_of(&rows, "Encoder").as_deref(), Some("Lavf"));
        assert!(rows.iter().all(|row| row.label != "Empty"));
        assert_eq!(value_of(&rows, "Location").as_deref(), Some(file));

        // A stream has no tags but its title, and is where it was opened.
        let mut values = Values::new();
        let address = "https://www.youtube.com/watch?v=aqz-KE-bpKQ";
        values.insert("path".into(), string(address));
        values.insert("media-title".into(), string("Big Buck Bunny"));
        let rows = general(&values);
        assert_eq!(value_of(&rows, "Title").as_deref(), Some("Big Buck Bunny"));
        assert_eq!(value_of(&rows, "Location").as_deref(), Some(address));
        assert!(rows.iter().all(|row| row.label != "Other Tags"));
    }

    #[test]
    fn codec_describes_each_stream() {
        let mut values = Values::new();
        values.insert(
            "track-list".into(),
            Node::Array(vec![
                map(vec![
                    ("id", Node::Int(1)),
                    ("type", string("video")),
                    ("selected", Node::Flag(true)),
                    ("codec", string("hevc")),
                    ("codec-desc", string("H.265 / HEVC")),
                    ("codec-profile", string("Main 10")),
                    ("demux-w", Node::Int(3840)),
                    ("demux-h", Node::Int(2160)),
                    ("demux-fps", Node::Double(23.976_023)),
                    ("format-name", string("yuv420p10le")),
                ]),
                map(vec![
                    ("id", Node::Int(1)),
                    ("type", string("audio")),
                    ("selected", Node::Flag(true)),
                    ("default", Node::Flag(true)),
                    ("codec", string("flac")),
                    ("codec-desc", string("FLAC (Free Lossless Audio Codec)")),
                    ("lang", string("jpn")),
                    ("demux-samplerate", Node::Int(48000)),
                    ("demux-channel-count", Node::Int(6)),
                    ("demux-channels", string("5.1(side)")),
                    ("format-name", string("s32")),
                ]),
                map(vec![
                    ("id", Node::Int(2)),
                    ("type", string("sub")),
                    ("codec", string("subrip")),
                    ("title", string("Signs")),
                    ("external", Node::Flag(true)),
                    ("external-filename", string("/films/a.signs.srt")),
                ]),
            ]),
        );
        values.insert(
            "video-params".into(),
            map(vec![
                ("pixelformat", string("p010")),
                ("hw-pixelformat", string("vaapi")),
                ("primaries", string("bt.2020")),
                ("gamma", string("pq")),
                ("colormatrix", string("bt.2020-ncl")),
            ]),
        );
        let rows = codec(&values);
        let sections: Vec<String> = rows
            .iter()
            .filter(|row| row.section)
            .map(|row| row.label.to_string())
            .collect();
        assert_eq!(
            sections,
            [
                "Video Track 1 (shown)",
                "Audio Track 1 (playing)",
                "Subtitle Track 2"
            ]
        );
        assert_eq!(
            value_of(&rows, "Codec").as_deref(),
            Some("H.265 / HEVC (hevc)")
        );
        assert_eq!(value_of(&rows, "Profile").as_deref(), Some("Main 10"));
        assert_eq!(value_of(&rows, "Resolution").as_deref(), Some("3840×2160"));
        assert_eq!(value_of(&rows, "Frame rate").as_deref(), Some("23.976 fps"));
        assert_eq!(
            value_of(&rows, "Decoded format").as_deref(),
            Some("p010 (decoded in hardware as vaapi)")
        );
        assert_eq!(
            value_of(&rows, "Transfer").as_deref(),
            Some("PQ (SMPTE ST 2084), HDR")
        );
        assert_eq!(
            value_in(&rows, "Audio", "Language").as_deref(),
            Some("Japanese (jpn)")
        );
        assert_eq!(value_in(&rows, "Video", "Language"), None);
        assert_eq!(value_of(&rows, "Sample rate").as_deref(), Some("48 kHz"));
        assert_eq!(
            value_of(&rows, "Channels").as_deref(),
            Some("6 (5.1(side))")
        );
        assert_eq!(
            value_of(&rows, "Sample format").as_deref(),
            Some("32-bit integer (s32)")
        );
        assert_eq!(
            value_of(&rows, "Format").as_deref(),
            Some("subrip"),
            "a subtitle's codec is its format"
        );
        assert_eq!(value_of(&rows, "File").as_deref(), Some("a.signs.srt"));
        assert_eq!(value_in(&rows, "Video", "Type").as_deref(), Some("Video"));
        assert_eq!(
            value_in(&rows, "Audio", "Type").as_deref(),
            Some("Audio, default")
        );
        assert_eq!(
            value_in(&rows, "Subtitle", "Type").as_deref(),
            Some("Subtitle, external file")
        );
        // Of a track not decoded, nothing of how it would be.
        assert_eq!(value_in(&rows, "Subtitle", "Decoded format"), None);
    }

    #[test]
    fn sample_formats_are_told_in_bits() {
        assert_eq!(sample_bits("s16p"), Some("16-bit integer"));
        assert_eq!(sample_bits("floatp"), Some("32-bit float"));
        assert_eq!(sample_bits("dbl"), Some("64-bit float"));
        assert_eq!(sample_bits("spdif"), None);
    }

    #[test]
    fn a_big_cover_is_kept_small_and_in_its_colors() {
        // 1000×500 of blue-green-red-padding, with a stride wider than a row.
        let (width, height, stride) = (1000usize, 500usize, 4096usize);
        let mut data = vec![0u8; stride * height];
        for line in data.chunks_mut(stride) {
            for pixel in line[..width * 4].as_chunks_mut::<4>().0 {
                pixel.copy_from_slice(&[10, 20, 200, 0]);
            }
        }
        let node = map(vec![
            ("w", Node::Int(width as i64)),
            ("h", Node::Int(height as i64)),
            ("stride", Node::Int(stride as i64)),
            ("format", string("bgr0")),
            ("data", Node::Bytes(data)),
        ]);
        let picture = cover_picture(&node).unwrap();
        assert_eq!((picture.width(), picture.height()), (COVER_SIZE, 320));
        assert_eq!(
            picture.as_slice()[0],
            Rgb8Pixel {
                r: 200,
                g: 20,
                b: 10
            }
        );
        // Too few bytes for what it claims to be.
        let short = map(vec![
            ("w", Node::Int(10)),
            ("h", Node::Int(10)),
            ("stride", Node::Int(40)),
            ("format", string("bgr0")),
            ("data", Node::Bytes(vec![0; 100])),
        ]);
        assert!(cover_picture(&short).is_none());
    }

    /// A file's full path, a stream's address as it is, and standard input
    /// by name rather than as a file in the working folder.
    #[test]
    fn the_location_says_where_the_file_is() {
        let folder = std::env::current_dir().unwrap();
        assert_eq!(
            location("song.flac"),
            folder.join("song.flac").to_string_lossy()
        );
        assert_eq!(
            location("https://example.com/song.flac"),
            "https://example.com/song.flac"
        );
        assert_eq!(location("-"), "Standard input");
    }

    /// Escape closes the window, whether its keys have the focus or a value
    /// clicked for its text to be selected does, after a change of tab, and
    /// after that value's row is made anew or goes with the file.
    #[test]
    fn escape_closes_the_window_wherever_the_focus_is() {
        use slint::platform::{Key, PointerEventButton, WindowEvent};
        i_slint_backend_testing::init_no_event_loop();
        let window = MediaInfoWindow::new().unwrap();
        window
            .window()
            .set_size(slint::LogicalSize::new(480.0, 600.0));
        let rows = vec![
            section("Tags"),
            row("Title", Some("A Tone".into())),
            row("Artist", Some("Patina".into())),
        ];
        let general = Rc::new(VecModel::from(rows.clone()));
        window.set_general(ModelRc::from(general.clone()));
        window.set_codec(ModelRc::new(VecModel::from(rows.clone())));
        let closed = Rc::new(Cell::new(0));
        let counted = closed.clone();
        window.on_dismiss(move || counted.set(counted.get() + 1));
        window.show().unwrap();
        let escape = || {
            let text = SharedString::from(Key::Escape);
            window
                .window()
                .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
            window
                .window()
                .dispatch_event(WindowEvent::KeyReleased { text });
        };
        let click = |x: f32, y: f32| {
            let position = slint::LogicalPosition::new(x, y);
            let button = PointerEventButton::Left;
            let window = window.window();
            window.dispatch_event(WindowEvent::PointerMoved { position });
            window.dispatch_event(WindowEvent::PointerPressed { position, button });
            window.dispatch_event(WindowEvent::PointerReleased { position, button });
        };
        assert!(
            window.get_keys_focused(),
            "the keys have the focus at first"
        );
        escape();
        assert_eq!(closed.get(), 1);

        // The title's value: below the tabs, the subject and the heading.
        click(300.0, 118.0);
        assert!(!window.get_keys_focused(), "the value took the focus");
        escape();
        assert_eq!(closed.get(), 2, "Escape from a value");

        // The Codec tab, at the right of the tabs.
        click(400.0, 26.0);
        assert_eq!(window.get_tab(), 1);
        assert!(window.get_keys_focused());
        escape();
        assert_eq!(closed.get(), 3, "Escape after a change of tab");

        // A value with the focus, then the rows as the controller shows them
        // for the same tags, another file's, and no file.
        click(26.0, 26.0);
        assert_eq!(window.get_tab(), 0);
        let mut more = rows.clone();
        more.push(row("Album", Some("Tones".into())));
        let mut expected = closed.get();
        for (case, rows) in [
            ("the same rows", rows),
            ("more rows", more),
            ("nothing playing", Vec::new()),
        ] {
            put_rows(
                &window,
                &general,
                vec![
                    section("Tags"),
                    row("Title", Some("A Tone".into())),
                    row("Artist", Some("Patina".into())),
                ],
            );
            i_slint_core::window::WindowInner::from_pub(window.window()).ensure_tree_instantiated();
            click(300.0, 118.0);
            assert!(
                !window.get_keys_focused(),
                "{case}: the value took the focus"
            );
            put_rows(&window, &general, rows);
            // A pass of the event loop: changes' handlers, then the rows
            // made and taken away.
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
            i_slint_core::window::WindowInner::from_pub(window.window()).ensure_tree_instantiated();
            escape();
            expected += 1;
            assert_eq!(
                closed.get(),
                expected,
                "{case}: Escape after the rows changed"
            );
        }
    }

    /// What a real file gives through mpv: its tags, its tracks' formats,
    /// and its cover's picture.
    #[test]
    fn a_files_tags_tracks_and_cover_come_from_mpv() {
        let Some(mpv) = crate::mpv::testing::headless() else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("patina-media-info-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.flac");
        let cover = dir.join("cover.png");
        let ffmpeg = |args: &[&str]| {
            std::process::Command::new("ffmpeg")
                .stdin(std::process::Stdio::null())
                .args(["-loglevel", "error", "-y"])
                .args(args)
                .status()
                .is_ok_and(|status| status.success())
        };
        let (cover, path) = (cover.to_string_lossy(), path.to_string_lossy());
        // The cover first: a limit on the frames of the file itself would
        // end its audio with the cover's one frame.
        let made = ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=64x48",
            "-frames:v",
            "1",
            &cover,
        ]) && ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "sine=duration=3:sample_rate=44100",
            "-i",
            &cover,
            "-map",
            "0:a",
            "-map",
            "1:v",
            "-c:a",
            "flac",
            "-sample_fmt",
            "s16",
            "-c:v",
            "copy",
            "-disposition:v:0",
            "attached_pic",
            "-metadata",
            "title=Tone",
            "-metadata",
            "album=Test Tones",
            "-metadata",
            "track=3",
            &path,
        ]);
        if !made {
            crate::mpv::testing::skip("PATINA_TEST_REQUIRE_FFMPEG", "needs ffmpeg");
            return;
        }
        mpv.set_property("pause", true).unwrap();
        mpv.command(&["loadfile", &path]).unwrap();
        crate::mpv::testing::wait_for(&mpv, |event| matches!(event, mpv::Event::FileLoaded));
        // By now the cover is decoded, paused on.
        crate::mpv::testing::wait_for(&mpv, |event| matches!(event, mpv::Event::PlaybackRestart));
        let mut values = Values::new();
        for name in PROPERTIES {
            if let Ok(node) = mpv.get_property(name) {
                values.insert(name.to_owned(), node);
            }
        }
        let rows = general(&values);
        assert_eq!(value_of(&rows, "Title").as_deref(), Some("Tone"));
        assert_eq!(value_of(&rows, "Album").as_deref(), Some("Test Tones"));
        assert_eq!(value_of(&rows, "Track number").as_deref(), Some("3"));
        let rows = codec(&values);
        assert_eq!(value_of(&rows, "Resolution").as_deref(), Some("64×48"));
        assert_eq!(value_of(&rows, "Sample rate").as_deref(), Some("44.1 kHz"));
        assert_eq!(
            value_of(&rows, "Sample format").as_deref(),
            Some("16-bit integer (s16)")
        );
        assert!(
            value_of(&rows, "Format").is_none(),
            "no subtitle, so no Format: {rows:?}"
        );
        assert_eq!(cover_track(&values), Some(1), "the cover is shown");
        let shot = mpv.command_ret(&["screenshot-raw", "video"]).unwrap();
        let picture = cover_picture(&shot).expect("the cover's picture");
        assert_eq!((picture.width(), picture.height()), (64, 48));
        let red = picture.as_slice()[0];
        assert!(red.r > 200 && red.g < 40 && red.b < 40, "{red:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
