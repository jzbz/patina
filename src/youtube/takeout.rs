//! The files Google's data export gives for YouTube, read and written as they
//! are, so that lists can move in and out of Patina freely:
//! `subscriptions.csv` (`Channel Id,Channel Url,Channel Title`) and the
//! playlists' `…-videos.csv` (`Video ID,Playlist Video Creation Timestamp`).
//!
//! A chosen file is read once, into memory, and never opened in place. Columns
//! are found by their names, not by where they are, since nothing promises the
//! order. Nothing from a file is used without being checked: a row with a bad
//! id is counted and skipped, and every address is built again from the id. One
//! bad row never loses the rest.
//!
//! A NewPipe file comes through here too, by the same read and with the same
//! report, and `newpipe.rs` reads it: JSON is told from CSV by what the file
//! holds, not by its name.
//!
//! The names may be translated in other languages' exports, which hasn't been
//! confirmed, so a file whose header isn't known is also accepted when its
//! contents plainly have that shape.
//!
//! Nothing can stop a read once it has begun, so a wrong file has to be quick:
//! rows past `ROW_LIMIT` are only counted, which keeps it to seconds and the
//! memory to what a list needs.

use super::store::{Channel, MAX_SAVED, SavedVideo};
use super::{ChannelId, Video, VideoId, clean_text, newpipe, plausible_time};
use std::fmt;
use std::io::Read;
use std::path::Path;

/// Far more than any real list, and enough to stop a wrong file being read.
const FILE_LIMIT: u64 = 32 << 20;
/// The most rows kept of a file: twice what the longest list can hold, which
/// leaves room for duplicates and bad rows. Those past it count as skipped.
pub(super) const ROW_LIMIT: usize = 2 * MAX_SAVED;
/// How many columns a file with an unknown header is looked at for its shape.
/// The real ones have two or three, and each more is looked at for each row.
const SHAPE_COLUMNS: usize = 16;
/// The most fields kept of a row, far more than any of Google's files has.
/// Each kept field costs more than the comma it came from, so a wrong file of
/// one long row of commas would otherwise cost many times its size.
const FIELD_LIMIT: usize = 32;

const CHANNEL_ID: &str = "channel id";
const CHANNEL_TITLE: &str = "channel title";
const VIDEO_ID: &str = "video id";
const VIDEO_ADDED: &str = "playlist video creation timestamp";
const PLAYLIST_ID: &str = "playlist id";

/// What a file held.
#[derive(Debug, Clone, PartialEq)]
pub enum Rows {
    Channels(Vec<Channel>),
    WatchLater(Vec<SavedVideo>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub rows: Rows,
    /// Rows that weren't usable.
    pub skipped: usize,
}

/// Why a file wasn't read at all. A file that was read but has bad rows is not
/// an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    NotAFile,
    TooLarge,
    Read(String),
    /// A zip of the whole export, which has to be unpacked first.
    Archive,
    NotRecognized,
    /// JSON, but not NewPipe's subscriptions.
    NotNewPipe,
}

impl fmt::Display for ImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAFile => formatter.write_str("That is not a file."),
            Self::TooLarge => formatter.write_str("That file is too large to be a YouTube list."),
            Self::Read(error) => write!(formatter, "Could not read the file: {error}"),
            Self::Archive => formatter.write_str(
                "That is a zip archive. Unpack it, then choose subscriptions.csv \
                 or Watch later-videos.csv from inside.",
            ),
            Self::NotRecognized => formatter.write_str(
                "That is not a list Patina can read. Choose subscriptions.csv \
                 or Watch later-videos.csv from a Google export, or the file \
                 NewPipe exports its subscriptions to.",
            ),
            Self::NotNewPipe => formatter.write_str(
                "That JSON file is not a list Patina can read. Choose the file \
                 NewPipe exports its subscriptions to.",
            ),
        }
    }
}

/// Reads a chosen file once. A named pipe or a device would hang or never end,
/// so only a plain file is opened.
pub fn read_file(path: &Path) -> Result<Vec<u8>, ImportError> {
    let metadata = std::fs::metadata(path).map_err(|error| ImportError::Read(error.to_string()))?;
    if !metadata.is_file() {
        return Err(ImportError::NotAFile);
    }
    if metadata.len() > FILE_LIMIT {
        return Err(ImportError::TooLarge);
    }
    let file = std::fs::File::open(path).map_err(|error| ImportError::Read(error.to_string()))?;
    let mut bytes = Vec::new();
    file.take(FILE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| ImportError::Read(error.to_string()))?;
    if bytes.len() as u64 > FILE_LIMIT {
        return Err(ImportError::TooLarge);
    }
    Ok(bytes)
}

/// Text from a file's bytes, past any byte-order mark. Spreadsheets on Windows
/// can save a list as UTF-16. Bad bytes spoil only the field they are in.
fn decode(bytes: &[u8]) -> String {
    let utf16 = |to_unit: fn([u8; 2]) -> u16| {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| to_unit(*pair))
            .collect();
        String::from_utf16_lossy(&units)
    };
    match bytes {
        [0xff, 0xfe, ..] => utf16(u16::from_le_bytes),
        [0xfe, 0xff, ..] => utf16(u16::from_be_bytes),
        [0xef, 0xbb, 0xbf, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// The file's rows up to `ROW_LIMIT`, without those holding nothing, as blank
/// lines occur, and how many more it had. The whole file goes through the CSV
/// parser, which is what lets a quoted field hold a line break. A quote left
/// open would have it swallow every row after, and an odd number of them says
/// one was, so that file goes line by line: the row with the quote keeps what
/// is on its own line.
fn rows(text: &str) -> (Vec<Vec<String>>, usize) {
    let mut kept = Vec::new();
    let mut more = 0;
    if text.bytes().filter(|byte| *byte == b'"').count() % 2 == 0 {
        read_records(text, &mut kept, &mut more);
    } else {
        let mut lines = text
            .split(['\n', '\r'])
            .filter(|line| !line.trim().is_empty());
        for line in lines.by_ref() {
            read_records(line, &mut kept, &mut more);
            if kept.len() >= ROW_LIMIT {
                break;
            }
        }
        // A parser for each line is what costs, and these are only counted.
        more += lines.count();
    }
    (kept, more)
}

/// Adds the rows in `text` to `kept`, and counts those that no longer fit.
/// It goes a field at a time, as the csv crate's reader would hold where
/// every field of a row lies, twice over once trimmed, and a row of commas
/// can have millions. Fields past `FIELD_LIMIT` are read and dropped, and a
/// row with nothing before them counts as blank. Each field is trimmed as the
/// csv crate trims, and its parser drops a byte-order mark the same way.
fn read_records(text: &str, kept: &mut Vec<Vec<String>>, more: &mut usize) {
    let mut reader = csv_core::Reader::new();
    let mut input = text.as_bytes();
    let mut output = [0; 4096];
    let mut field = Vec::new();
    let mut row: Vec<String> = Vec::new();
    loop {
        let (result, read, written) = reader.read_field(input, &mut output);
        input = &input[read..];
        if row.len() < FIELD_LIMIT {
            field.extend_from_slice(&output[..written]);
        }
        // Once the input runs out, an empty one tells the parser the text has
        // ended, and it gives the last field and then the end.
        let row_ends = match result {
            csv_core::ReadFieldResult::InputEmpty | csv_core::ReadFieldResult::OutputFull => {
                continue;
            }
            csv_core::ReadFieldResult::Field { record_end } => record_end,
            csv_core::ReadFieldResult::End => break,
        };
        if row.len() < FIELD_LIMIT {
            row.push(String::from_utf8_lossy(&field).trim().to_owned());
        }
        field.clear();
        if !row_ends {
            continue;
        }
        if row.iter().all(String::is_empty) {
            row.clear();
        } else if kept.len() < ROW_LIMIT {
            kept.push(std::mem::take(&mut row));
        } else {
            *more += 1;
            row.clear();
        }
    }
}

fn column(header: &[String], name: &str) -> Option<usize> {
    header.iter().position(|field| {
        field
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .eq_ignore_ascii_case(name)
    })
}

fn timestamp(text: &str) -> Option<i64> {
    let time: jiff::Timestamp = text.parse().ok()?;
    plausible_time(time.as_second())
}

/// A share of the rows, to tell a file's shape by: a lone bad row doesn't
/// change it, and neither does the header.
fn most(rows: &[Vec<String>], test: impl Fn(&[String]) -> bool) -> bool {
    let matching = rows.iter().filter(|row| test(row)).count();
    matching > 0 && matching * 2 >= rows.len()
}

/// A greater share, for what could be there by chance: any word of eleven
/// letters looks like a video's id.
fn nearly_all(rows: &[Vec<String>], test: impl Fn(&[String]) -> bool) -> bool {
    let matching = rows.iter().filter(|row| test(row)).count();
    matching > 0 && matching * 10 >= rows.len() * 9
}

/// The list of playlists lies beside the lists themselves, is easily chosen by
/// mistake, and has timestamps and titles of any length.
fn is_playlists_file(header: &[String], body: &[Vec<String>], width: usize) -> bool {
    header
        .iter()
        .any(|field| field.to_ascii_lowercase().contains(PLAYLIST_ID))
        || (0..width).any(|column| {
            nearly_all(body, |row| {
                let text = cell(row, column);
                text.starts_with("PL") && text.len() > 11
            })
        })
}

/// A row's field, empty when the row is too short to have it.
fn cell(row: &[String], index: usize) -> &str {
    row.get(index).map(String::as_str).unwrap_or("")
}

enum Columns {
    Channels { id: usize, title: Option<usize> },
    WatchLater { id: usize, added: Option<usize> },
}

/// The columns, by the header's names.
fn known_columns(header: &[String]) -> Option<Columns> {
    if let Some(id) = column(header, CHANNEL_ID) {
        return Some(Columns::Channels {
            id,
            title: column(header, CHANNEL_TITLE),
        });
    }
    column(header, VIDEO_ID).map(|id| Columns::WatchLater {
        id,
        added: column(header, VIDEO_ADDED),
    })
}

/// The columns of a file whose header isn't known, from what the rows hold.
fn shaped_columns(rows: &[Vec<String>]) -> Option<Columns> {
    let width = rows
        .iter()
        .map(Vec::len)
        .max()
        .unwrap_or(0)
        .min(SHAPE_COLUMNS);
    let body = rows
        .get(1..)
        .filter(|body| !body.is_empty())
        .unwrap_or(rows);
    if is_playlists_file(rows.first()?, body, width) {
        return None;
    }
    for id in 0..width {
        if most(body, |row| ChannelId::parse(cell(row, id)).is_some()) {
            // The first other column of text, which an address isn't.
            let title = (0..width).find(|&other| {
                other != id
                    && body.iter().any(|row| !cell(row, other).is_empty())
                    && body.iter().all(|row| !cell(row, other).contains("://"))
            });
            return Some(Columns::Channels { id, title });
        }
    }
    // Eleven characters alone could be anything, so a video's id needs nearly
    // every row to fit, and timestamps beside it.
    let added =
        (0..width).find(|&other| nearly_all(body, |row| timestamp(cell(row, other)).is_some()));
    (0..width)
        .find(|&id| nearly_all(body, |row| VideoId::parse(cell(row, id)).is_some()))
        .filter(|_| added.is_some())
        .map(|id| Columns::WatchLater { id, added })
}

/// Reads a list. `now` stands in for an added-at the file doesn't have or spoils.
pub fn parse(bytes: &[u8], now: i64) -> Result<Parsed, ImportError> {
    if bytes.starts_with(b"PK\x03\x04") {
        return Err(ImportError::Archive);
    }
    let text = decode(bytes);
    if newpipe::is_json(&text) {
        return newpipe::parse(&text, now);
    }
    let (rows, past_the_limit) = rows(&text);
    let header = rows.first().ok_or(ImportError::NotRecognized)?;
    let (columns, body) = match known_columns(header) {
        Some(columns) => (columns, &rows[1..]),
        None => {
            let columns = shaped_columns(&rows).ok_or(ImportError::NotRecognized)?;
            // No names at all: the first row is a row. A name of eleven letters
            // gives itself away by having no timestamp beside it.
            let row = match columns {
                Columns::Channels { id, .. } => ChannelId::parse(cell(header, id)).is_some(),
                Columns::WatchLater { id, added } => {
                    VideoId::parse(cell(header, id)).is_some()
                        && added.is_some_and(|added| timestamp(cell(header, added)).is_some())
                }
            };
            (columns, if row { &rows[..] } else { &rows[1..] })
        }
    };
    let mut skipped = past_the_limit;
    let rows = match columns {
        Columns::Channels { id, title } => Rows::Channels(
            body.iter()
                .filter_map(|row| {
                    let channel = row.get(id).and_then(|text| ChannelId::parse(text));
                    skipped += usize::from(channel.is_none());
                    Some(Channel {
                        title: title
                            .and_then(|title| row.get(title))
                            .map(|title| clean_text(title))
                            .unwrap_or_default(),
                        added_at: now,
                        ..Channel::new(channel?)
                    })
                })
                .collect(),
        ),
        Columns::WatchLater { id, added } => Rows::WatchLater(
            body.iter()
                .filter_map(|row| {
                    let video = row.get(id).and_then(|text| VideoId::parse(text));
                    skipped += usize::from(video.is_none());
                    Some(SavedVideo {
                        added_at: added
                            .and_then(|added| row.get(added))
                            .and_then(|added| timestamp(added))
                            .unwrap_or(now),
                        ..SavedVideo::new(Video::new(video?), now)
                    })
                })
                .collect(),
        ),
    };
    Ok(Parsed { rows, skipped })
}

/// A list as the export's CSV: `\n` line ends, and quotes only where needed.
fn write_csv<const N: usize>(
    header: [&str; N],
    rows: impl Iterator<Item = [String; N]>,
) -> Vec<u8> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    // Into memory, which only fails on an impossible row.
    let mut written = writer.write_record(header);
    for row in rows {
        written = written.and_then(|()| writer.write_record(&row));
    }
    if let Err(error) = written {
        log::warn!("could not write a row: {error}");
    }
    writer.into_inner().unwrap_or_default()
}

/// The channels as `subscriptions.csv`, with the `http://` addresses the real
/// file has.
pub fn write_subscriptions(channels: &[Channel]) -> Vec<u8> {
    write_csv(
        ["Channel Id", "Channel Url", "Channel Title"],
        channels.iter().map(|channel| {
            [
                channel.id.as_str().to_owned(),
                format!("http://www.youtube.com/channel/{}", channel.id.as_str()),
                channel.title.clone(),
            ]
        }),
    )
}

/// The list as `Watch later-videos.csv`, with when each was added.
pub fn write_watch_later(videos: &[SavedVideo]) -> Vec<u8> {
    write_csv(
        ["Video ID", "Playlist Video Creation Timestamp"],
        videos.iter().map(|saved| {
            [
                saved.video.id.as_str().to_owned(),
                format_timestamp(saved.added_at),
            ]
        }),
    )
}

/// In the export's own form: `2026-01-02T03:04:05+00:00`.
fn format_timestamp(seconds: i64) -> String {
    jiff::Timestamp::from_second(seconds)
        .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
        .strftime("%Y-%m-%dT%H:%M:%S+00:00")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "UCaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "UCbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "UCcccccccccccccccccccccc";
    const NOW: i64 = 1_800_000_000;

    fn channels(bytes: &[u8]) -> (Vec<(String, String)>, usize) {
        let parsed = parse(bytes, NOW).unwrap();
        let Rows::Channels(rows) = parsed.rows else {
            panic!("not channels");
        };
        (
            rows.into_iter()
                .map(|channel| (channel.id.as_str().to_owned(), channel.title))
                .collect(),
            parsed.skipped,
        )
    }

    fn watch_later(bytes: &[u8]) -> (Vec<(String, i64)>, usize) {
        let parsed = parse(bytes, NOW).unwrap();
        let Rows::WatchLater(rows) = parsed.rows else {
            panic!("not Watch Later");
        };
        (
            rows.into_iter()
                .map(|saved| (saved.video.id.as_str().to_owned(), saved.added_at))
                .collect(),
            parsed.skipped,
        )
    }

    fn pair(id: &str, title: &str) -> (String, String) {
        (id.to_owned(), title.to_owned())
    }

    #[test]
    fn subscriptions_read_with_commas_blank_lines_and_a_bom() {
        let file = format!(
            "\u{feff}Channel Id,Channel Url,Channel Title\n\
             {A},http://www.youtube.com/channel/{A},\"One, Two, Three\"\n\
             \n\
             {B},http://www.youtube.com/channel/{B},Plain\n"
        );
        let (rows, skipped) = channels(file.as_bytes());
        assert_eq!(rows, [pair(A, "One, Two, Three"), pair(B, "Plain")]);
        assert_eq!(skipped, 0);
    }

    #[test]
    fn columns_are_found_by_name_in_any_order() {
        let file = format!(
            "channel title,Something Else,  CHANNEL  ID \n\
             \"He said \"\"hi\"\"\",x,{A}\n"
        );
        let (rows, _) = channels(file.as_bytes());
        assert_eq!(rows, [pair(A, "He said \"hi\"")]);
        let file = "Playlist Video Creation Timestamp,Video ID\n\
                    2026-01-02T03:04:05+00:00,-bcdefgh_jk\n";
        let (rows, _) = watch_later(file.as_bytes());
        assert_eq!(rows, [("-bcdefgh_jk".to_owned(), 1_767_323_045)]);
    }

    #[test]
    fn a_bad_row_never_loses_the_rest() {
        let file = format!(
            "Channel Id,Channel Url,Channel Title\n\
             not-an-id,http://www.youtube.com/channel/x,Bad\n\
             {A},javascript:alert(1),Good address or not\n\
             ,,\n\
             UC-too-short,http://www.youtube.com/channel/UC-too-short,Bad\n\
             {B},http://evil.example/,\"Left open\n\
             {C},,Last\n\
             {A}\n"
        );
        let (rows, skipped) = channels(file.as_bytes());
        assert_eq!(
            rows,
            [
                pair(A, "Good address or not"),
                pair(B, "Left open"),
                pair(C, "Last"),
                pair(A, ""),
            ]
        );
        assert_eq!(skipped, 2);
    }

    /// A real CSV parser reads it as the one field it is, and what is in that
    /// field can't add a row.
    #[test]
    fn a_title_with_a_line_break_stays_a_title() {
        let file = format!("Channel Id,Channel Url,Channel Title\n{A},,\"One\nTwo\"\n{B},,Next\n");
        let (rows, skipped) = channels(file.as_bytes());
        assert_eq!(rows, [pair(A, "One Two"), pair(B, "Next")]);
        assert_eq!(skipped, 0);
        let file = format!("Channel Id,Channel Url,Channel Title\n{A},,\"Title\n{B},,Injected\"\n");
        let (rows, skipped) = channels(file.as_bytes());
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].0, A);
        assert_eq!(skipped, 0);
    }

    #[test]
    fn past_the_limit_rows_are_counted_as_skipped() {
        let row = format!("{A},,x\n");
        let file = format!(
            "Channel Id,Channel Url,Channel Title\n{}",
            row.repeat(ROW_LIMIT + 5)
        );
        let (rows, skipped) = channels(file.as_bytes());
        assert_eq!(rows.len(), ROW_LIMIT - 1);
        assert_eq!(skipped, 6);
        // The same, line by line, past a quote left open.
        let file = format!(
            "Channel Id,Channel Url,Channel Title\n{A},,\"open\n{}",
            row.repeat(ROW_LIMIT + 5)
        );
        let (rows, skipped) = channels(file.as_bytes());
        assert_eq!(rows.len(), ROW_LIMIT - 1);
        assert_eq!(skipped, 7);
    }

    /// Far from any list, and quick to say so: a read can't be stopped.
    #[test]
    fn a_wrong_file_full_of_short_lines_is_refused() {
        let file = "x\n".repeat(2 << 20);
        assert_eq!(parse(file.as_bytes(), NOW), Err(ImportError::NotRecognized));
        let file = format!("\"\n{file}");
        assert_eq!(parse(file.as_bytes(), NOW), Err(ImportError::NotRecognized));
        let wide = "abcdefghijk,".repeat(1 << 20);
        assert_eq!(
            parse(format!("{wide}\n{wide}\n").as_bytes(), NOW),
            Err(ImportError::NotRecognized)
        );
    }

    #[test]
    fn titles_are_cleaned_and_addresses_ignored() {
        let file = format!(
            "Channel Id,Channel Url,Channel Title\n\
             {A},https://evil.example/,\"a\u{202e}b\u{1b}c\"\n"
        );
        let parsed = parse(file.as_bytes(), NOW).unwrap();
        let Rows::Channels(rows) = parsed.rows else {
            panic!();
        };
        assert_eq!(rows[0].title, "abc");
        assert_eq!(
            rows[0].id.url(),
            format!("https://www.youtube.com/channel/{A}")
        );
        assert_eq!(rows[0].added_at, NOW);
    }

    #[test]
    fn watch_later_keeps_when_each_was_added() {
        let file = "Video ID,Playlist Video Creation Timestamp\n\
                    abcdefghijk,2026-01-02T03:04:05+00:00\n\
                    -bcdefgh_jk,2026-01-02T05:04:05+02:00\n\
                    bad,2026-01-02T03:04:05+00:00\n\
                    nodateatall,\n\
                    baddateatal,yesterday\n\
                    fardateatal,9999-01-01T00:00:00+00:00\n\
                    \n";
        let (rows, skipped) = watch_later(file.as_bytes());
        assert_eq!(
            rows,
            [
                ("abcdefghijk".to_owned(), 1_767_323_045),
                ("-bcdefgh_jk".to_owned(), 1_767_323_045),
                ("nodateatall".to_owned(), NOW),
                ("baddateatal".to_owned(), NOW),
                ("fardateatal".to_owned(), NOW),
            ]
        );
        assert_eq!(skipped, 1);
    }

    #[test]
    fn line_ends_of_every_kind_read() {
        let body = format!(
            "Channel Id,Channel Url,Channel Title{0}{A},,x{0}{B},,y{0}",
            "\r\n"
        );
        assert_eq!(channels(body.as_bytes()).0.len(), 2);
        let body = format!("Channel Id,Channel Url,Channel Title\r{A},,x\r{B},,y");
        assert_eq!(channels(body.as_bytes()).0.len(), 2);
    }

    #[test]
    fn utf16_from_a_spreadsheet_reads() {
        let text = format!("Channel Id,Channel Url,Channel Title\n{A},,Título\n");
        let le: Vec<u8> = [0xff, 0xfe]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let be: Vec<u8> = [0xfe, 0xff]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        assert_eq!(channels(&le).0, [pair(A, "Título")]);
        assert_eq!(channels(&be).0, [pair(A, "Título")]);
    }

    #[test]
    fn bad_bytes_spoil_only_their_field() {
        let mut file = format!("Channel Id,Channel Url,Channel Title\n{A},,").into_bytes();
        file.extend([0xff, 0xfe, b'x']);
        file.extend(format!("\n{B},,y\n").into_bytes());
        let (rows, skipped) = channels(&file);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1], pair(B, "y"));
        assert_eq!(skipped, 0);
    }

    #[test]
    fn a_translated_header_is_told_by_its_rows() {
        let file = format!("Kanal-ID,Kanal-URL,Kanaltitel\n{A},http://x/{A},Titel\nbad,,\n{B},,\n");
        let (rows, skipped) = channels(file.as_bytes());
        assert_eq!(rows, [pair(A, "Titel"), pair(B, "")]);
        assert_eq!(skipped, 1);
        let file = "Video-ID,Zeitstempel\nabcdefghijk,2026-01-02T03:04:05+00:00\n";
        let (rows, skipped) = watch_later(file.as_bytes());
        assert_eq!(rows, [("abcdefghijk".to_owned(), 1_767_323_045)]);
        assert_eq!(skipped, 0);
        // No header at all.
        let file = format!("{A},,Titel\n{B},,Other\n");
        assert_eq!(channels(file.as_bytes()).0.len(), 2);
    }

    #[test]
    fn other_files_are_not_recognized() {
        for file in [
            "",
            "\n\n",
            "hello world",
            "a,b,c\n1,2,3\n",
            // The list of playlists, whose ids are neither kind.
            "Playlist ID,Playlist Title (Original)\nPLxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx,Cooking\n",
            // Eleven-letter words with no timestamps.
            "word,other\nelevenchars,x\nabcdefghijk,y\n",
            "<?xml version=\"1.0\"?><opml></opml>",
        ] {
            assert_eq!(
                parse(file.as_bytes(), NOW),
                Err(ImportError::NotRecognized),
                "{file}"
            );
        }
        assert_eq!(parse(b"PK\x03\x04rest", NOW), Err(ImportError::Archive));
        assert_eq!(
            parse(b"{\"something\":\"else\"}", NOW),
            Err(ImportError::NotNewPipe)
        );
    }

    /// NewPipe's file is told by what it holds, past a byte-order mark too, and
    /// read as channels whatever it is called.
    #[test]
    fn a_newpipe_file_comes_through_the_same_read() {
        let file = format!(
            "\u{feff}  {{\"app_version\":\"0.29.1\",\"app_version_int\":1015,\"subscriptions\":[\
             {{\"service_id\":0,\"url\":\"https://www.youtube.com/channel/{A}\",\"name\":\"One\"}},\
             {{\"service_id\":1,\"url\":\"https://soundcloud.com/x\",\"name\":\"Two\"}}]}}"
        );
        assert_eq!(channels(file.as_bytes()), (vec![pair(A, "One")], 1));
    }

    /// `playlists.csv` lies beside the lists. A title of eleven letters must
    /// not make it one of them, whichever language it is in.
    #[test]
    fn the_list_of_playlists_adds_nothing() {
        const WHEN: &str = "2021-05-06T07:08:09+00:00";
        const ID: &str = "PLabcdefghijklmnopqrstuvwxyz01234567";
        for file in [
            format!("Playlist ID,Playlist Title (Original),Created\n{ID},Programming,{WHEN}\n"),
            format!(
                "Playlist ID,Playlist Title (Original),Created\n\
                 {ID},Programming,{WHEN}\n{ID},Documentary,{WHEN}\n{ID},Short,{WHEN}\n"
            ),
            // The names translated, and so unknown.
            format!("Kennung,Titel,Erstellt\n{ID},Programming,{WHEN}\n{ID},Documentary,{WHEN}\n"),
            // Not plainly one, and not plainly a list of videos either.
            format!(
                "Kennung,Titel,Erstellt\n\
                 x,Programming,{WHEN}\nx,Documentary,{WHEN}\nx,Short,{WHEN}\n"
            ),
        ] {
            assert_eq!(
                parse(file.as_bytes(), NOW),
                Err(ImportError::NotRecognized),
                "{file}"
            );
        }
    }

    #[test]
    fn an_eleven_letter_header_is_not_a_video() {
        let file = "Identifiant,Horodatage\nabcdefghijk,2026-01-02T03:04:05+00:00\n";
        let (rows, skipped) = watch_later(file.as_bytes());
        assert_eq!(rows, [("abcdefghijk".to_owned(), 1_767_323_045)]);
        assert_eq!(skipped, 0);
        // With no header at all, the first row is one.
        let file = "aaaaaaaaaaa,2026-01-02T03:04:05+00:00\nabcdefghijk,2026-01-02T03:04:05+00:00\n";
        assert_eq!(watch_later(file.as_bytes()).0.len(), 2);
    }

    #[test]
    fn a_header_with_no_rows_is_an_empty_list() {
        assert_eq!(
            channels(b"Channel Id,Channel Url,Channel Title\n"),
            (vec![], 0)
        );
        assert_eq!(
            watch_later(b"Video ID,Playlist Video Creation Timestamp\n"),
            (vec![], 0)
        );
    }

    #[test]
    fn a_huge_hostile_row_is_only_a_bad_row() {
        let long = "x".repeat(1 << 20);
        let file = format!("Channel Id,Channel Url,Channel Title\n{long},{long},{long}\n{A},,y\n");
        let (rows, skipped) = channels(file.as_bytes());
        assert_eq!(rows, [pair(A, "y")]);
        assert_eq!(skipped, 1);
    }

    /// A row is kept to its first fields however wide it is, which keeps a
    /// file of commas to what its few fields need.
    #[test]
    fn a_row_of_endless_commas_keeps_only_its_first_fields() {
        let commas = ",".repeat(1 << 20);
        let text = format!("Channel Id,Channel Url,Channel Title{commas}\n{A},,y{commas}\n");
        let (kept, more) = rows(&text);
        assert_eq!(more, 0);
        assert!(kept.iter().all(|row| row.len() == FIELD_LIMIT));
        let (rows, skipped) = channels(text.as_bytes());
        assert_eq!(rows, [pair(A, "y")]);
        assert_eq!(skipped, 0);
    }

    #[test]
    fn subscriptions_round_trip_exactly() {
        let file = format!(
            "Channel Id,Channel Url,Channel Title\n\
             {A},http://www.youtube.com/channel/{A},\"One, Two\"\n\
             {B},http://www.youtube.com/channel/{B},\"Say \"\"hi\"\"\"\n\
             {C},http://www.youtube.com/channel/{C},Plain\n"
        );
        let Rows::Channels(rows) = parse(file.as_bytes(), NOW).unwrap().rows else {
            panic!();
        };
        assert_eq!(String::from_utf8(write_subscriptions(&rows)).unwrap(), file);
    }

    #[test]
    fn watch_later_round_trips_exactly() {
        let file = "Video ID,Playlist Video Creation Timestamp\n\
                    abcdefghijk,2026-01-02T03:04:05+00:00\n\
                    -bcdefgh_jk,2024-12-31T23:59:59+00:00\n";
        let Rows::WatchLater(rows) = parse(file.as_bytes(), NOW).unwrap().rows else {
            panic!();
        };
        assert_eq!(String::from_utf8(write_watch_later(&rows)).unwrap(), file);
    }

    #[test]
    fn an_empty_list_exports_its_header() {
        assert_eq!(
            write_subscriptions(&[]),
            b"Channel Id,Channel Url,Channel Title\n"
        );
        assert_eq!(
            write_watch_later(&[]),
            b"Video ID,Playlist Video Creation Timestamp\n"
        );
    }

    #[test]
    fn a_chosen_file_is_read_only_if_it_is_a_plain_file() {
        let dir = std::env::temp_dir().join(format!("patina-takeout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read_file(&dir), Err(ImportError::NotAFile));
        let file = dir.join("list.csv");
        std::fs::write(&file, b"contents").unwrap();
        assert_eq!(read_file(&file).unwrap(), b"contents");
        assert!(matches!(
            read_file(&dir.join("missing")),
            Err(ImportError::Read(_))
        ));
        let large = std::fs::File::create(dir.join("large")).unwrap();
        large.set_len(FILE_LIMIT + 1).unwrap();
        assert_eq!(read_file(&dir.join("large")), Err(ImportError::TooLarge));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
