//! The kinds of file Patina opens, from packaging/media-types.tsv: one
//! table for the file dialogs, subtitle matching, the system media
//! controls and the file associations on every platform.

use std::path::Path;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Video,
    Audio,
    Playlist,
}

pub const ALL: &[Kind] = &[Kind::Video, Kind::Audio, Kind::Playlist];

impl Kind {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "video" => Some(Self::Video),
            "audio" => Some(Self::Audio),
            "playlist" => Some(Self::Playlist),
            _ => None,
        }
    }
}

pub struct Format {
    pub extension: &'static str,
    pub kind: Kind,
    /// The freedesktop.org name first, then older aliases; Linux's way of
    /// naming file types.
    #[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
    pub mime_types: Vec<&'static str>,
}

const TABLE: &str = include_str!("../packaging/media-types.tsv");

/// Subtitle files, which load into a video rather than open on their own.
pub const SUBTITLE_EXTENSIONS: &[&str] = &[
    "ass", "idx", "lrc", "mks", "smi", "srt", "ssa", "sub", "sup", "vtt", "utf", "utf8", "utf-8",
    "rt", "aqt", "jss", "scc",
];

pub fn all() -> &'static [Format] {
    static FORMATS: OnceLock<Vec<Format>> = OnceLock::new();
    FORMATS.get_or_init(|| parse(TABLE))
}

fn parse(table: &'static str) -> Vec<Format> {
    table
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let extension = fields.next()?;
            let kind = Kind::parse(fields.next()?)?;
            let mime_types = fields
                .next()
                .unwrap_or_default()
                .split(';')
                .filter(|mime| !mime.is_empty())
                .collect();
            Some(Format {
                extension,
                kind,
                mime_types,
            })
        })
        .collect()
}

/// The extensions of files of `kinds`, in the table's order.
pub fn extensions(kinds: &[Kind]) -> Vec<&'static str> {
    all()
        .iter()
        .filter(|format| kinds.contains(&format.kind))
        .map(|format| format.extension)
        .collect()
}

/// Every MIME type of files of `kinds`, each once.
#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
pub fn mime_types(kinds: &[Kind]) -> Vec<&'static str> {
    let mut types = Vec::new();
    for format in all().iter().filter(|format| kinds.contains(&format.kind)) {
        for mime in &format.mime_types {
            if !types.contains(mime) {
                types.push(*mime);
            }
        }
    }
    types
}

/// What kind of media `path` is, by its extension.
pub fn kind_of(path: &Path) -> Option<Kind> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    all()
        .iter()
        .find(|format| format.extension == extension)
        .map(|format| format.kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_line_of_the_table_is_read() {
        let lines = TABLE
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .count();
        assert_eq!(all().len(), lines, "a line has an unknown kind");
        let mut seen = std::collections::HashSet::new();
        for format in all() {
            assert!(seen.insert(format.extension), "{} twice", format.extension);
            assert_eq!(format.extension, format.extension.to_ascii_lowercase());
            for mime in &format.mime_types {
                assert!(mime.contains('/'), "{mime} for {}", format.extension);
            }
        }
    }

    #[test]
    fn kinds_by_extension() {
        assert_eq!(kind_of(Path::new("/a/Film.MKV")), Some(Kind::Video));
        assert_eq!(kind_of(Path::new("song.flac")), Some(Kind::Audio));
        assert_eq!(kind_of(Path::new("list.m3u8")), Some(Kind::Playlist));
        assert_eq!(kind_of(Path::new("notes.txt")), None);
        assert!(mime_types(&[Kind::Video]).contains(&"video/matroska"));
        // Making Patina the default for one kind takes no files of another.
        assert!(
            mime_types(&[Kind::Audio])
                .iter()
                .all(|mime| mime.starts_with("audio/"))
        );
        assert!(
            mime_types(&[Kind::Video])
                .iter()
                .all(|mime| !mime.starts_with("audio/"))
        );
    }

    /// The desktop entry is committed for packagers, so it is checked here
    /// rather than generated.
    #[test]
    fn desktop_entry_lists_every_type() {
        let entry = include_str!("../packaging/linux/video.patina.Patina.desktop");
        let line = entry
            .lines()
            .find_map(|line| line.strip_prefix("MimeType="))
            .unwrap();
        let mut expected: Vec<&str> = mime_types(ALL);
        expected.push("x-scheme-handler/patina");
        assert!(
            line == format!("{};", expected.join(";")),
            "packaging/linux/video.patina.Patina.desktop needs\nMimeType={};",
            expected.join(";")
        );
    }
}
