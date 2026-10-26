//! The inspector window, after IINA's: the active player's file, tracks and
//! playback status, plus any mpv properties the user chooses to watch.
//!
//! While the window is open it asks the player for the current tab's
//! properties once a second, with mpv's asynchronous property reads; the
//! replies come back through the player's event pump to `reply`.

use crate::app::App;
use crate::mpv::{self, Node};
use crate::player::state::{Track, file_name};
use crate::util::format_time;
use crate::window::PlayerController;
use crate::{InspectorRow, InspectorWindow};
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::time::Duration;

/// Userdata of the inspector's property reads: this, plus the round and
/// the index of the property in that round.
pub const REPLY_BASE: u64 = 1 << 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    General,
    Tracks,
    Status,
    Watch,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::General, Tab::Tracks, Tab::Status, Tab::Watch];

    fn properties(self) -> &'static [&'static str] {
        match self {
            Tab::General => &[
                "path",
                "media-title",
                "filename",
                "duration",
                "file-format",
                "file-size",
                "chapters",
                "editions",
                "metadata/by-key/comment",
            ],
            Tab::Tracks => &[
                "current-tracks/video/codec",
                "current-tracks/video/decoder-desc",
                "width",
                "height",
                "container-fps",
                "video-params/hw-pixelformat",
                "video-params/pixelformat",
                "video-params/primaries",
                "video-params/gamma",
                "video-params/colormatrix",
                "current-vo",
                "current-tracks/audio/codec-desc",
                "audio-params/format",
                "audio-params/hr-channels",
                "audio-params/samplerate",
                "current-ao",
                "track-list",
            ],
            Tab::Status => &[
                "hwdec-current",
                "frame-drop-count",
                "decoder-frame-drop-count",
                "mistimed-frame-count",
                "estimated-vf-fps",
                "display-fps",
                "estimated-display-fps",
                "avsync",
                "total-avsync-change",
                "video-bitrate",
                "audio-bitrate",
                "demuxer-cache-duration",
                "cache-speed",
            ],
            Tab::Watch => &[],
        }
    }
}

type Values = HashMap<String, Result<Node, String>>;

pub struct InspectorController {
    app: Weak<App>,
    window: InspectorWindow,
    rows: Rc<VecModel<InspectorRow>>,
    tab: Cell<Tab>,
    target: RefCell<Weak<PlayerController>>,
    timer: Timer,
    /// The properties asked for in the current round, by reply index.
    requested: RefCell<Vec<String>>,
    round: Cell<u64>,
    pending: Cell<usize>,
    values: RefCell<Values>,
}

impl InspectorController {
    pub fn new(app: &Rc<App>) -> Result<Rc<Self>, String> {
        let window = InspectorWindow::new().map_err(|error| error.to_string())?;
        let rows = Rc::new(VecModel::default());
        window.set_rows(ModelRc::from(rows.clone()));
        let controller = Rc::new(Self {
            app: Rc::downgrade(app),
            window,
            rows,
            tab: Cell::new(Tab::General),
            target: RefCell::new(Weak::new()),
            timer: Timer::default(),
            requested: RefCell::new(Vec::new()),
            round: Cell::new(0),
            pending: Cell::new(0),
            values: RefCell::new(HashMap::new()),
        });
        controller.wire();
        Ok(controller)
    }

    fn app(&self) -> Rc<App> {
        self.app.upgrade().expect("the app outlives its inspector")
    }

    pub fn show(self: &Rc<Self>) {
        if let Err(error) = self.window.show() {
            log::warn!("could not show the inspector: {error}");
            return;
        }
        let weak = Rc::downgrade(self);
        self.timer
            .start(TimerMode::Repeated, Duration::from_secs(1), move || {
                if let Some(this) = weak.upgrade() {
                    this.poll();
                }
            });
        self.poll();
    }

    fn wire(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.window.on_select_tab(move |index| {
            if let Some(this) = weak.upgrade() {
                this.tab.set(Tab::ALL[index.clamp(0, 3) as usize]);
                this.window.set_tab(index);
                this.window.set_watch_error(SharedString::new());
                this.values.borrow_mut().clear();
                this.render();
                this.poll();
            }
        });
        let weak = Rc::downgrade(self);
        self.window.on_add_watch(move |name| {
            if let Some(this) = weak.upgrade() {
                this.add_watch(name.trim());
            }
        });
        let weak = Rc::downgrade(self);
        self.window.on_remove_watch(move |index| {
            if let Some(this) = weak.upgrade() {
                this.remove_watch(index);
            }
        });
        // Nothing is polled while the window is closed.
        let weak = Rc::downgrade(self);
        self.window.window().on_close_requested(move || {
            if let Some(this) = weak.upgrade() {
                this.timer.stop();
            }
            slint::CloseRequestResponse::HideWindow
        });
    }

    /// Inspects `player` from now on: the window focused most recently.
    pub fn follow(self: &Rc<Self>, player: &Rc<PlayerController>) {
        let same = self
            .target
            .borrow()
            .upgrade()
            .is_some_and(|target| Rc::ptr_eq(&target, player));
        if !same {
            *self.target.borrow_mut() = Rc::downgrade(player);
            self.values.borrow_mut().clear();
            if self.timer.running() {
                self.poll();
            }
        }
    }

    fn watched(&self) -> Vec<String> {
        self.app().prefs().inspector_watch.clone()
    }

    /// Starts a round of property reads for the current tab.
    fn poll(&self) {
        let Some(target) = self.target.borrow().upgrade() else {
            self.values.borrow_mut().clear();
            self.render();
            return;
        };
        // A reply still missing from the last round counts as unavailable.
        let names: Vec<String> = match self.tab.get() {
            Tab::Watch => self.watched(),
            tab => tab
                .properties()
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
        };
        let round = self.round.get().wrapping_add(1) & 0xff_ffff;
        self.round.set(round);
        self.pending.set(names.len());
        for (index, name) in names.iter().enumerate() {
            target.request_property(REPLY_BASE | (round << 16) | index as u64, name);
        }
        *self.requested.borrow_mut() = names;
        if self.pending.get() == 0 {
            self.render();
        }
    }

    /// A property value read for the inspector arrived.
    pub fn reply(&self, userdata: u64, result: mpv::Result<(String, Node)>) {
        let round = (userdata >> 16) & 0xff_ffff;
        let index = (userdata & 0xffff) as usize;
        if round != self.round.get() {
            return;
        }
        let Some(name) = self.requested.borrow().get(index).cloned() else {
            return;
        };
        let value = result
            .map(|(_, node)| node)
            .map_err(|error| error.description());
        self.values.borrow_mut().insert(name, value);
        self.pending.set(self.pending.get().saturating_sub(1));
        if self.pending.get() == 0 {
            self.render();
        }
    }

    fn render(&self) {
        let target = self.target.borrow().upgrade();
        let subject = match &target {
            Some(target) => target.media_name(),
            None => "No player window".to_owned(),
        };
        self.window.set_subject(subject.into());
        let values = self.values.borrow();
        let rows = match self.tab.get() {
            Tab::General => general(&values),
            Tab::Tracks => tracks(&values),
            Tab::Status => status(&values),
            Tab::Watch => watch(&values, &self.watched()),
        };
        drop(values);
        if self.rows.row_count() == rows.len() {
            for (index, row) in rows.into_iter().enumerate() {
                if self.rows.row_data(index).as_ref() != Some(&row) {
                    self.rows.set_row_data(index, row);
                }
            }
        } else {
            self.rows.set_vec(rows);
        }
    }

    fn add_watch(&self, name: &str) {
        let valid = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_/.".contains(c));
        if !valid {
            self.window
                .set_watch_error("Property names use letters, digits, “-” and “/”.".into());
            return;
        }
        self.window.set_watch_error(SharedString::new());
        if self.watched().iter().any(|watched| watched == name) {
            return;
        }
        let name = name.to_owned();
        self.app()
            .update_prefs(|prefs| prefs.inspector_watch.push(name));
        self.poll();
    }

    fn remove_watch(&self, index: i32) {
        let watched = self.watched();
        // Row 0 is the heading.
        let Some(name) = usize::try_from(index - 1)
            .ok()
            .and_then(|index| watched.get(index).cloned())
        else {
            return;
        };
        self.app()
            .update_prefs(|prefs| prefs.inspector_watch.retain(|watched| *watched != name));
        self.poll();
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

fn node<'a>(values: &'a Values, name: &str) -> Option<&'a Node> {
    values.get(name)?.as_ref().ok()
}

/// A property as text; None when unavailable or empty.
fn text(values: &Values, name: &str) -> Option<String> {
    node(values, name)
        .map(describe)
        .filter(|text| !text.is_empty())
}

fn number(values: &Values, name: &str) -> Option<f64> {
    node(values, name)?.as_f64()
}

fn general(values: &Values) -> Vec<InspectorRow> {
    let title = text(values, "media-title").filter(|title| {
        // mpv falls back on the file name; IINA shows no title then.
        text(values, "filename").as_ref() != Some(title)
    });
    vec![
        section("File"),
        row("Path", text(values, "path")),
        row("Title", title),
        row("Duration", number(values, "duration").map(format_time)),
        row("Format", text(values, "file-format")),
        row(
            "Size",
            number(values, "file-size").map(|bytes| format_bytes(bytes, "B")),
        ),
        row("Chapters", text(values, "chapters")),
        row("Editions", text(values, "editions")),
        row("Comment", text(values, "metadata/by-key/comment")),
    ]
}

fn tracks(values: &Values) -> Vec<InspectorRow> {
    let size = match (number(values, "width"), number(values, "height")) {
        (Some(width), Some(height)) => Some(format!("{width}×{height}")),
        _ => None,
    };
    let pixel_format = text(values, "video-params/hw-pixelformat")
        .map(|format| format!("{format} (hardware)"))
        .or_else(|| {
            text(values, "video-params/pixelformat").map(|format| format!("{format} (software)"))
        });
    let colors = text(values, "video-params/primaries").map(|primaries| {
        let gamma = text(values, "video-params/gamma").unwrap_or_else(|| "?".into());
        let range = if matches!(gamma.as_str(), "pq" | "hlg") {
            "HDR"
        } else {
            "SDR"
        };
        format!("{primaries} / {gamma} ({range})")
    });
    let mut rows = vec![
        section("Video"),
        row("Format", text(values, "current-tracks/video/codec")),
        row("Decoder", text(values, "current-tracks/video/decoder-desc")),
        row("Size", size),
        row(
            "Frame rate",
            number(values, "container-fps").map(|fps| format!("{} fps", decimals(fps, 3))),
        ),
        row("Pixel format", pixel_format),
        row("Colors", colors),
        row("Color matrix", text(values, "video-params/colormatrix")),
        row("Output", text(values, "current-vo")),
        section("Audio"),
        row("Format", text(values, "current-tracks/audio/codec-desc")),
        row("Sample format", text(values, "audio-params/format")),
        row("Channels", text(values, "audio-params/hr-channels")),
        row(
            "Sample rate",
            number(values, "audio-params/samplerate")
                .map(|rate| format!("{} kHz", decimals(rate / 1000.0, 1))),
        ),
        row("Output", text(values, "current-ao")),
        section("All Tracks"),
    ];
    let list: Vec<Track> = node(values, "track-list")
        .map(|list| {
            list.as_array()
                .iter()
                .filter_map(Track::from_node)
                .collect()
        })
        .unwrap_or_default();
    if list.is_empty() {
        rows.push(row("Tracks", None));
    }
    for track in list {
        let kind = match track.kind {
            crate::player::state::TrackKind::Video => "Video",
            crate::player::state::TrackKind::Audio => "Audio",
            crate::player::state::TrackKind::Sub => "Subtitle",
        };
        let mut value = track.label();
        let detail = track.detail();
        if !detail.is_empty() {
            value = format!("{value}\n{detail}");
        }
        if track.selected {
            value.push_str("\nselected");
        }
        rows.push(row(&format!("{kind} {}", track.id), Some(value)));
    }
    rows
}

fn status(values: &Values) -> Vec<InspectorRow> {
    let dropped = number(values, "frame-drop-count").map(|output| {
        let decoder = number(values, "decoder-frame-drop-count").unwrap_or(0.0);
        format!("{output} (decoder {decoder})")
    });
    let fps = |name| number(values, name).map(|fps| format!("{} fps", decimals(fps, 3)));
    let seconds = |name| number(values, name).map(|value| format!("{value:+.3} s"));
    let bitrate = |name| number(values, name).map(|bits| format_bytes(bits, "bps"));
    let hwdec = text(values, "hwdec-current").map(|hwdec| {
        if hwdec == "no" {
            "none (software)".to_owned()
        } else {
            hwdec
        }
    });
    vec![
        section("Video"),
        row("Hardware decoder", hwdec),
        row("Dropped frames", dropped),
        row("Mistimed frames", text(values, "mistimed-frame-count")),
        row("Video FPS", fps("estimated-vf-fps")),
        row("Display FPS", fps("display-fps")),
        row("Estimated display FPS", fps("estimated-display-fps")),
        section("Sync"),
        row("A/V sync", seconds("avsync")),
        row("Total correction", seconds("total-avsync-change")),
        section("Bit Rate"),
        row("Video", bitrate("video-bitrate")),
        row("Audio", bitrate("audio-bitrate")),
        section("Cache"),
        row(
            "Cached ahead",
            number(values, "demuxer-cache-duration").map(|value| format!("{value:.1} s")),
        ),
        row(
            "Speed",
            number(values, "cache-speed").map(|bytes| format_bytes(bytes, "B/s")),
        ),
    ]
}

fn watch(values: &Values, watched: &[String]) -> Vec<InspectorRow> {
    let mut rows = vec![section("Watched Properties")];
    if watched.is_empty() {
        rows.push(row(
            "",
            Some("Add any mpv property below to see its value live.".into()),
        ));
        if let Some(hint) = rows.last_mut() {
            hint.dim = true;
        }
    }
    for name in watched {
        let (value, dim) = match values.get(name) {
            Some(Ok(node)) => (describe(node), false),
            Some(Err(error)) => (error.clone(), true),
            None => ("…".into(), true),
        };
        rows.push(InspectorRow {
            label: name.as_str().into(),
            value: value.into(),
            dim,
            removable: true,
            ..Default::default()
        });
    }
    rows
}

/// Any mpv value as text.
fn describe(node: &Node) -> String {
    match node {
        Node::None => String::new(),
        Node::String(text) => text.clone(),
        Node::Flag(flag) => if *flag { "yes" } else { "no" }.into(),
        Node::Int(int) => int.to_string(),
        Node::Double(double) => decimals(*double, 3),
        Node::Array(items) => {
            let items: Vec<String> = items.iter().map(describe).collect();
            format!("[{}]", items.join(", "))
        }
        Node::Map(entries) => {
            let entries: Vec<String> = entries
                .iter()
                .map(|(key, value)| format!("{key}: {}", describe(value)))
                .collect();
            format!("{{{}}}", entries.join(", "))
        }
        Node::Bytes(bytes) => format!("{} bytes", bytes.len()),
    }
}

/// At most `places` decimals, without trailing zeros: 23.976, 48, 0.5.
fn decimals(value: f64, places: usize) -> String {
    let text = format!("{value:.places$}");
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text
    }
}

/// 1532000 → "1.53 MB" (or "Mbps"), in powers of 1000.
fn format_bytes(value: f64, unit: &str) -> String {
    let mut value = value;
    let mut prefix = "";
    for next in ["k", "M", "G", "T"] {
        if value.abs() < 1000.0 {
            break;
        }
        value /= 1000.0;
        prefix = next;
    }
    let digits = if prefix.is_empty() || value >= 100.0 {
        0
    } else {
        2
    };
    format!("{} {prefix}{unit}", decimals(value, digits))
}

pub fn media_name(path: &str, title: &str) -> String {
    if !title.is_empty() {
        title.to_owned()
    } else if !path.is_empty() {
        file_name(path)
    } else {
        "Nothing playing".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_formatted_for_people() {
        assert_eq!(format_bytes(532.0, "B"), "532 B");
        assert_eq!(format_bytes(11_210_123.0, "B"), "11.21 MB");
        assert_eq!(format_bytes(2_400_000.0, "bps"), "2.4 Mbps");
        assert_eq!(decimals(23.976_023, 3), "23.976");
        assert_eq!(decimals(48.0, 1), "48");
        let node = Node::Map(vec![
            ("w".into(), Node::Int(640)),
            ("hdr".into(), Node::Flag(false)),
            ("list".into(), Node::Array(vec![Node::Double(0.5)])),
        ]);
        assert_eq!(describe(&node), "{w: 640, hdr: no, list: [0.5]}");
    }

    #[test]
    fn tabs_show_what_mpv_reported() {
        let mut values = Values::new();
        values.insert("path".into(), Ok(Node::String("/media/clip.mkv".into())));
        values.insert("media-title".into(), Ok(Node::String("clip.mkv".into())));
        values.insert("filename".into(), Ok(Node::String("clip.mkv".into())));
        values.insert("duration".into(), Ok(Node::Double(75.5)));
        values.insert("file-size".into(), Err("property unavailable".into()));
        let rows = general(&values);
        let value = |label: &str| {
            let row = rows.iter().find(|row| row.label == label).unwrap();
            (row.value.to_string(), row.dim)
        };
        assert_eq!(value("Path"), ("/media/clip.mkv".into(), false));
        assert_eq!(value("Title"), ("N/A".into(), true));
        assert_eq!(value("Duration"), ("01:15".into(), false));
        assert_eq!(value("Size"), ("N/A".into(), true));

        let mut values = Values::new();
        values.insert("avsync".into(), Ok(Node::Double(-0.0123)));
        values.insert("hwdec-current".into(), Ok(Node::String("no".into())));
        let rows = status(&values);
        let find = |label: &str| {
            rows.iter()
                .find(|row| row.label == label)
                .unwrap()
                .value
                .to_string()
        };
        assert_eq!(find("A/V sync"), "-0.012 s");
        assert_eq!(find("Hardware decoder"), "none (software)");
    }
}
