//! Downloading an item's file from a Jellyfin server into the download
//! folder, for the Downloads window (`crate::downloads`), which lists these
//! beside yt-dlp's. What is downloaded is the original file, as the server
//! keeps it (`GET /Items/{id}/Download`), under the name the server gives
//! it, written to a part file that takes that name only once it is
//! complete, so that the folder never holds half a film under a film's
//! name. docs/jellyfin.md has the design.

use super::api::{Client, Error};
use super::library::is_playable;
use super::models::*;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Added to a file's name while it downloads. Patina's own, not the bare
/// ".part" browsers use too, so that a part file left behind
/// (`left_behind`) is known to be one of Patina's, and a browser's paused
/// download is never taken for one.
const PART: &str = ".patina.part";
/// How long a part file no download holds has to have gone unwritten to
/// count as left behind by a Patina that ended without removing it: one
/// killed, or a computer that lost power. A download locks its part file
/// a moment after making it, which this covers many times over.
const LEFT_AFTER: Duration = Duration::from_secs(60);
/// The most bytes of a name, extension included: file systems take 255,
/// and " (9999)" and `PART` have to fit after it.
const NAME_LIMIT: usize = 200;
/// The longest extension kept whole when a name is cut to `NAME_LIMIT`.
const EXTENSION_LIMIT: usize = 16;
/// How many numbered names are tried before giving up.
const NUMBERS: u32 = 9999;
/// How many names may refuse their part file before the refusal is taken
/// for the folder's. On Windows a part file that a stopped download
/// removed keeps its name until the download's thread closes it (see
/// Stop), wherever the file system gives no POSIX deletion (FAT and exFAT,
/// some network shares), and making it again is refused rather than found
/// to be there.
const REFUSALS: u32 = 3;
const CHUNK: usize = 256 * 1024;
/// Over how long the speed is taken.
const SPEED_SPAN: Duration = Duration::from_secs(3);

/// An item to download, as the Downloads window keeps it: what it needs
/// to ask the server again, for Retry, and no token.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub server_id: String,
    pub item_id: String,
    /// What the Downloads window calls it.
    pub title: String,
    /// The file's name when the server gives none (`fallback_name`).
    pub fallback: String,
}

impl Request {
    pub fn of(server_id: &str, item: &BaseItemDto, title: String) -> Self {
        Self {
            server_id: server_id.to_owned(),
            item_id: item.id.clone(),
            title,
            fallback: fallback_name(item),
        }
    }
}

/// Items that download as the files they hold, rather than as a file of
/// their own.
pub fn downloads_children(kind: ItemKind) -> bool {
    matches!(
        kind,
        ItemKind::Season | ItemKind::Series | ItemKind::MusicAlbum | ItemKind::Playlist
    )
}

/// Whether Patina offers to download an item, as far as the item itself
/// tells: a file of its own, unless the server says it can't be
/// downloaded (`CanDownload`, when it sends it) or knows it only from
/// metadata; or a season, show, album or playlist, which the server never
/// hands out whole, for the files it holds.
pub fn offered(item: &BaseItemDto) -> bool {
    if downloads_children(item.kind) {
        return true;
    }
    is_playable(item.kind)
        && item.can_download != Some(false)
        && item.location_type != LocationType::Virtual
}

/// Whether an account may download `item`: its policy allows downloading
/// at all, which the server checks for every download (403 without it),
/// and the item is offered.
pub fn allowed(policy: &UserPolicy, item: &BaseItemDto) -> bool {
    policy.enable_content_downloading && offered(item)
}

/// The name for an item's file when the server gives none: its path's
/// last part, or else "<Name>.<Container>".
pub fn fallback_name(item: &BaseItemDto) -> String {
    let path = item
        .path
        .as_deref()
        .or_else(|| item.media_sources.first()?.path.as_deref());
    // The server's own path, written its way: either separator.
    if let Some(name) = path
        .and_then(|path| path.rsplit(['/', '\\']).next())
        .filter(|name| !name.trim().is_empty())
    {
        return name.to_owned();
    }
    let container = item
        .container
        .as_deref()
        .or_else(|| item.media_sources.first()?.container.as_deref());
    match container.and_then(|container| extension(container, item.kind == ItemKind::Audio)) {
        Some(extension) => format!("{}.{extension}", item.name),
        None => item.name.clone(),
    }
}

/// An extension for a container as the server names it, which for MP4 is
/// FFmpeg's list of the family ("mov,mp4,m4a,3gp,3g2,mj2"): the member
/// the file most likely is.
fn extension(container: &str, audio: bool) -> Option<&str> {
    let names: Vec<&str> = container
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    let preferred = if audio {
        ["m4a", "mp4"]
    } else {
        ["mp4", "m4v"]
    };
    preferred
        .into_iter()
        .find_map(|wanted| names.iter().copied().find(|name| *name == wanted))
        .or(names.first().copied())
}

/// The name a file is saved under: the one the server gives
/// (`Content-Disposition`), or else `fallback`, made fit for this
/// platform's file names.
pub fn file_name(disposition: Option<&str>, fallback: &str) -> String {
    disposition
        .and_then(disposition_name)
        .and_then(|name| sanitize(&name))
        .or_else(|| sanitize(fallback))
        .unwrap_or_else(|| "Download".to_owned())
}

/// The file name a `Content-Disposition` header gives (RFC 6266): its
/// `filename*`, which carries any character (Jellyfin sends it beside
/// `filename`, where it writes "_" for each character outside ASCII), or
/// else its `filename`.
pub fn disposition_name(header: &str) -> Option<String> {
    let parameters = parameters(header);
    let find = |wanted: &str| {
        parameters
            .iter()
            .find(|(name, _)| name == wanted)
            .map(|(_, value)| value.as_str())
    };
    find("filename*")
        .and_then(extended_value)
        .or_else(|| find("filename").map(str::to_owned))
        .filter(|name| !name.trim().is_empty())
}

/// A header's parameters after its first value, names in lower case:
/// `name=token` or `name="quoted string"`, separated by semicolons.
fn parameters(header: &str) -> Vec<(String, String)> {
    let mut parameters = Vec::new();
    let Some(at) = header.find(';') else {
        return parameters;
    };
    let mut rest = &header[at + 1..];
    loop {
        rest = rest.trim_start_matches([' ', '\t', ';']);
        let Some(at) = rest.find(['=', ';']) else {
            break;
        };
        if rest.as_bytes()[at] == b';' {
            // A parameter without a value.
            rest = &rest[at..];
            continue;
        }
        let name = rest[..at].trim().to_ascii_lowercase();
        rest = rest[at + 1..].trim_start();
        let value = if let Some(quoted) = rest.strip_prefix('"') {
            let mut value = String::new();
            let mut end = quoted.len();
            let mut escaped = false;
            for (index, c) in quoted.char_indices() {
                if escaped {
                    value.push(c);
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    end = index + 1;
                    break;
                } else {
                    value.push(c);
                }
            }
            rest = &quoted[end..];
            value
        } else {
            let end = rest.find(';').unwrap_or(rest.len());
            let value = rest[..end].trim().to_owned();
            rest = &rest[end..];
            value
        };
        parameters.push((name, value));
    }
    parameters
}

/// An RFC 8187 value, `charset'language'percent-encoded`, in UTF-8 or
/// ISO-8859-1, the two charsets the RFC names.
fn extended_value(value: &str) -> Option<String> {
    let mut parts = value.splitn(3, '\'');
    let charset = parts.next()?.trim().to_ascii_lowercase();
    let _language = parts.next()?;
    let bytes = percent_decode(parts.next()?)?;
    match charset.as_str() {
        "utf-8" => String::from_utf8(bytes).ok(),
        "iso-8859-1" => Some(bytes.into_iter().map(char::from).collect()),
        _ => None,
    }
}

fn percent_decode(text: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut input = text.bytes();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let high = char::from(input.next()?).to_digit(16)?;
            let low = char::from(input.next()?).to_digit(16)?;
            bytes.push((high * 16 + low) as u8);
        } else {
            bytes.push(byte);
        }
    }
    Some(bytes)
}

/// Whose rules a file name has to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rules {
    /// Linux and the BSDs: anything but the separator and NUL.
    Unix,
    /// The same, and no colon, which the Finder shows as a slash.
    Mac,
    /// None of `<>:"/\|?*`, no device names (`CON`, `COM1`...) whatever
    /// their extension, and no dot or space at the end, which Windows
    /// drops.
    Windows,
}

impl Rules {
    fn here() -> Self {
        if cfg!(windows) {
            Rules::Windows
        } else if cfg!(target_os = "macos") {
            Rules::Mac
        } else {
            Rules::Unix
        }
    }
}

/// `name` made fit to be a file's name in the download folder on this
/// platform; None when nothing of it is left.
pub fn sanitize(name: &str) -> Option<String> {
    sanitize_for(name, Rules::here())
}

/// `name` as one file name under `rules`: never a path, since separators
/// become "_", nor "." or "..", nor hidden, since the dots and spaces it
/// starts with go, however they mix (". .bashrc" would be hidden after a
/// dot went, then a space); control characters and those that hide
/// themselves (`unseen`) become "_" too, and a name past `NAME_LIMIT` bytes
/// is cut, keeping its extension.
fn sanitize_for(name: &str, rules: Rules) -> Option<String> {
    let replaced: String = name
        .chars()
        .map(|c| {
            let refused = c.is_control()
                || unseen(c)
                || c == '/'
                // A separator on Windows, and so in names written there.
                || c == '\\'
                || (rules == Rules::Mac && c == ':')
                || (rules == Rules::Windows && matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'));
            if refused { '_' } else { c }
        })
        .collect();
    let mut name = replaced
        .trim_start_matches(|c: char| c == '.' || c.is_whitespace())
        .trim_end();
    if rules == Rules::Windows {
        name = name.trim_end_matches(['.', ' ']);
    }
    if name.is_empty() {
        return None;
    }
    let mut name = cut(name, NAME_LIMIT);
    if rules == Rules::Windows {
        // Cutting can leave a dot at the end again.
        name.truncate(name.trim_end_matches(['.', ' ']).len());
        if reserved_on_windows(&name) {
            name.insert(0, '_');
        }
    }
    Some(name)
}

/// Characters that change how the text around them is shown without being
/// seen themselves: the bidirectional controls, with which a name ending
/// in ".exe" can read "Film\u{202E}vkm.exe" as "Filmexe.mkv", and the
/// invisible spaces, joiners of words and line breaks. The two joiners of
/// letters (U+200C and U+200D) stay, since Persian, the Indic scripts and
/// emoji need them, and they reorder nothing.
fn unseen(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
    )
}

/// Windows's device names, which no file may have, with any extension.
fn reserved_on_windows(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default().trim_end();
    let upper = stem.to_ascii_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        _ => {
            let number = upper
                .strip_prefix("COM")
                .or_else(|| upper.strip_prefix("LPT"));
            number.is_some_and(|number| {
                matches!(
                    number,
                    "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"
                ) || matches!(number, "¹" | "²" | "³")
            })
        }
    }
}

/// `name` in at most `limit` bytes, cut at a character's edge, its
/// extension kept when it is a short one.
fn cut(name: &str, limit: usize) -> String {
    if name.len() <= limit {
        return name.to_owned();
    }
    let (stem, extension) = match name.rfind('.') {
        Some(at) if at > 0 && name.len() - at <= EXTENSION_LIMIT => name.split_at(at),
        _ => (name, ""),
    };
    let mut end = limit - extension.len();
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{extension}", stem[..end].trim_end())
}

/// The `number`th name for `name`: itself for 1, and "Film (2).mkv" for
/// 2, the number before the extension.
fn numbered(name: &str, number: u32) -> String {
    if number <= 1 {
        return name.to_owned();
    }
    match name.rfind('.') {
        Some(at) if at > 0 => format!("{} ({number}){}", &name[..at], &name[at..]),
        _ => format!("{name} ({number})"),
    }
}

/// Whether anything is at `path`, a dangling link included.
fn taken(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

/// A part file made for a download, and the name it is to take.
#[derive(Debug)]
struct Claim {
    folder: PathBuf,
    name: String,
    number: u32,
    part: PathBuf,
}

impl Claim {
    fn target(&self, number: u32) -> PathBuf {
        self.folder.join(numbered(&self.name, number))
    }
}

/// Makes the part file for `name` in `folder`, under the first name that
/// is free: "Film.mkv", or else "Film (2).mkv" and so on, a name being free
/// when neither it nor its part file is there, or only a part file left
/// behind (`left_behind`), which goes. The part file is made only if it
/// isn't there yet, so that two downloads never write one, and is locked
/// for as long as it is written, which tells other Patinas it isn't left
/// behind.
fn claim(folder: &Path, name: &str) -> io::Result<(Claim, File)> {
    claim_with(folder, name, |part| {
        OpenOptions::new().write(true).create_new(true).open(part)
    })
}

/// `claim`, making each part file it tries with `create`, which the tests
/// stand in for.
fn claim_with(
    folder: &Path,
    name: &str,
    create: impl Fn(&Path) -> io::Result<File>,
) -> io::Result<(Claim, File)> {
    let mut refused = 0;
    for number in 1..=NUMBERS {
        let numbered = numbered(name, number);
        if taken(&folder.join(&numbered)) {
            continue;
        }
        let part = folder.join(format!("{numbered}{PART}"));
        // Once more after removing a part file left behind.
        for _ in 0..2 {
            match create(&part) {
                Ok(file) => {
                    // On a file system without locks, no part file is ever
                    // taken for left behind: its lock can't be tried
                    // either.
                    if let Err(error) = file.lock() {
                        log::debug!("could not lock {}: {error}", part.display());
                    }
                    let claim = Claim {
                        folder: folder.to_owned(),
                        name: name.to_owned(),
                        number,
                        part,
                    };
                    return Ok((claim, file));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if !left_behind(&part) {
                        break;
                    }
                    log::info!("removing {}, left behind by a download", part.display());
                    if !remove(&part) {
                        break;
                    }
                }
                // Most likely a part file still held open by a stopped
                // download's thread, on Windows (`REFUSALS`): the next name
                // is tried.
                Err(error)
                    if error.kind() == io::ErrorKind::PermissionDenied && refused < REFUSALS =>
                {
                    log::debug!(
                        "{} is refused ({error}); trying the next name",
                        part.display()
                    );
                    refused += 1;
                    break;
                }
                Err(error) => return Err(error),
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("every name for {name} is taken"),
    ))
}

/// Whether the part file at `path` was left behind by a download that no
/// longer runs, in this Patina or another: no download holds its lock, and
/// nothing has written to it for `LEFT_AFTER`.
fn left_behind(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let idle = file
        .metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|idle| idle > LEFT_AFTER);
    // The lock goes with `file`, before the part file is removed: on
    // Windows a file still open when it is removed keeps its name until it
    // is closed.
    idle && file.try_lock().is_ok()
}

/// Removes the file at `path`, which may be gone already; false when it
/// is still there.
fn remove(path: &Path) -> bool {
    match fs::remove_file(path) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(error) => {
            log::warn!("could not remove {}: {error}", path.display());
            false
        }
    }
}

/// Gives the complete part file its name, or the next free one should a
/// file of that name have appeared meanwhile: never over a file that is
/// there. A hard link refuses to replace one, where a rename would; where
/// the folder's file system has no hard links (FAT, some network shares),
/// it is a rename after looking.
fn finish(claim: &Claim) -> io::Result<PathBuf> {
    for number in claim.number..=NUMBERS {
        let target = claim.target(number);
        match fs::hard_link(&claim.part, &target) {
            Ok(()) => {
                remove(&claim.part);
                return Ok(target);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                log::debug!("no hard link for a download ({error}); renaming it");
                if taken(&target) {
                    continue;
                }
                fs::rename(&claim.part, &target)?;
                return Ok(target);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("every name for {} is taken", claim.name),
    ))
}

/// What stops a download, shared by its thread and the Downloads window:
/// the flag its thread looks at before each read, and the part file it
/// writes, which stopping removes at once. A read can wait up to a minute
/// for a server that has stopped sending (`STALL_TIMEOUT` in api.rs), and a
/// part file left until then would hold the file's name meanwhile, or for
/// good should Patina quit first.
#[derive(Debug, Default)]
pub struct Stop {
    stopped: AtomicBool,
    part: Mutex<Part>,
}

#[derive(Debug, Default)]
enum Part {
    #[default]
    NotYet,
    Writing(PathBuf),
    /// Removed, or never to be made: the download was stopped, or failed.
    Gone,
    /// It took its name.
    Saved,
}

impl Stop {
    /// Stops the download: its thread stops at its next read, and its part
    /// file goes now. False when it comes too late: the download has saved
    /// its file, which it keeps, and its thread is about to say so; a stop
    /// that comes while it saves waits for that (`save`).
    pub fn stop(&self) -> bool {
        self.stopped.store(true, Ordering::SeqCst);
        let mut part = self.part();
        match &*part {
            Part::Writing(path) => {
                remove(path);
                *part = Part::Gone;
                true
            }
            Part::NotYet => {
                *part = Part::Gone;
                true
            }
            Part::Gone => true,
            Part::Saved => false,
        }
    }

    pub fn stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// One whose download has saved its file, for the Downloads window's
    /// tests.
    #[cfg(test)]
    pub fn saved() -> Self {
        let stop = Self::default();
        *stop.part() = Part::Saved;
        stop
    }

    fn part(&self) -> MutexGuard<'_, Part> {
        self.part.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Notes the part file the download writes; Cancelled, the file
    /// removed, when it was stopped before it could be noted.
    fn writing(&self, path: &Path) -> Result<(), Failure> {
        let mut part = self.part();
        if let Part::Gone = *part {
            remove(path);
            return Err(Failure::Cancelled);
        }
        *part = Part::Writing(path.to_owned());
        Ok(())
    }

    /// Removes the part file of a download that failed.
    fn discard(&self) {
        let mut part = self.part();
        if let Part::Writing(path) = &*part {
            remove(path);
        }
        *part = Part::Gone;
    }

    /// Gives the complete part file its name (`finish`), unless the
    /// download was stopped meanwhile. A stop that comes while it does
    /// waits for it, and the file is kept.
    fn save(&self, claim: &Claim) -> Result<PathBuf, Failure> {
        let mut part = self.part();
        if !matches!(*part, Part::Writing(_)) {
            return Err(Failure::Cancelled);
        }
        match finish(claim) {
            Ok(path) => {
                *part = Part::Saved;
                Ok(path)
            }
            Err(error) => {
                remove(&claim.part);
                *part = Part::Gone;
                Err(Failure::Disk(format!(
                    "Could not save {}: {error}",
                    claim.name
                )))
            }
        }
    }
}

/// How far a download has come.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Progress {
    pub done: u64,
    /// The size, when the server gave it, or once the end has come.
    pub total: Option<u64>,
}

impl Progress {
    /// Seconds left at `speed`, in bytes a second, when both are known.
    pub fn eta(&self, speed: Option<f64>) -> Option<u64> {
        let speed = speed.filter(|speed| *speed > 0.0)?;
        let left = self.total?.saturating_sub(self.done);
        Some((left as f64 / speed).ceil() as u64)
    }
}

/// How far a download has come, and when bytes last came, which its thread
/// notes after each read and the Downloads window reads as often as it
/// shows it. The thread can't report while it waits for the server's
/// answer, which a server can draw out for half a minute (`TIMEOUT` in
/// api.rs), or for a read, which one that has stopped sending draws out
/// for a minute (`STALL_TIMEOUT`): the window tells from this that it
/// waits, and what came last before it did. Until the server has answered,
/// it reads nothing come, as of the download's start, so that a server slow
/// to answer is waited for as one slow to send is.
#[derive(Debug)]
pub struct Meter(Mutex<(Progress, Instant)>);

impl Meter {
    /// The meter of a download starting now.
    pub fn started() -> Self {
        Self(Mutex::new((Progress::default(), Instant::now())))
    }

    pub fn note(&self, progress: Progress) {
        *self.lock() = (progress, Instant::now());
    }

    /// How far the download has come, and when that was.
    pub fn read(&self) -> (Progress, Instant) {
        *self.lock()
    }

    fn lock(&self) -> MutexGuard<'_, (Progress, Instant)> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// How a download ended, when it didn't end with its file.
#[derive(Debug, PartialEq)]
pub enum Failure {
    Cancelled,
    /// The server refused it, or the connection failed.
    Server(Error),
    /// The folder or the file couldn't be written; a sentence.
    Disk(String),
}

/// The speed over the last `SPEED_SPAN`, from what had arrived when: bytes
/// a second, once there is a second's worth to go by.
#[derive(Debug, Default)]
pub struct Speed(std::collections::VecDeque<(Instant, u64)>);

impl Speed {
    /// Notes that `done` bytes had arrived `at`, and gives the speed.
    pub fn add(&mut self, at: Instant, done: u64) -> Option<f64> {
        self.0.push_back((at, done));
        while self
            .0
            .front()
            .is_some_and(|(then, _)| at.duration_since(*then) > SPEED_SPAN)
        {
            self.0.pop_front();
        }
        let (then, before) = *self.0.front()?;
        let seconds = at.duration_since(then).as_secs_f64();
        (seconds >= 1.0).then(|| (done - before) as f64 / seconds)
    }
}

/// Downloads item `item_id`'s file into `folder`, named as the server
/// says or else `fallback`, and gives where it is. Blocks until it is
/// done. `stop` stops it (see Stop); a download that stops for any reason
/// removes its part file, and one that fails once stopped, a read cut
/// short among them, was cancelled. `progress` hears how far it has come:
/// at its start, and after each read, so that what it heard last is what
/// came before a read that waits (see Meter).
pub fn run(
    client: &Client,
    item_id: &str,
    fallback: &str,
    folder: &Path,
    stop: &Stop,
    progress: impl FnMut(Progress),
) -> Result<PathBuf, Failure> {
    match download(client, item_id, fallback, folder, stop, progress) {
        Err(_) if stop.stopped() => Err(Failure::Cancelled),
        result => result,
    }
}

fn download(
    client: &Client,
    item_id: &str,
    fallback: &str,
    folder: &Path,
    stop: &Stop,
    mut progress: impl FnMut(Progress),
) -> Result<PathBuf, Failure> {
    let mut download = client.download(item_id).map_err(Failure::Server)?;
    if stop.stopped() {
        return Err(Failure::Cancelled);
    }
    let name = file_name(download.disposition.as_deref(), fallback);
    let total = download.length;
    fs::create_dir_all(folder)
        .map_err(|error| Failure::Disk(format!("Could not make {}: {error}", folder.display())))?;
    let (claim, mut file) = claim(folder, &name).map_err(|error| {
        Failure::Disk(format!("Could not save in {}: {error}", folder.display()))
    })?;
    stop.writing(&claim.part)?;
    progress(Progress { done: 0, total });
    let mut copy = || -> Result<(), Failure> {
        let mut buffer = vec![0; CHUNK];
        let mut done = 0u64;
        loop {
            if stop.stopped() {
                return Err(Failure::Cancelled);
            }
            let read = download.read(&mut buffer).map_err(Failure::Server)?;
            if read == 0 {
                // The size, now that the end has come, for an answer that
                // didn't give it.
                if total.is_none() {
                    progress(Progress {
                        done,
                        total: Some(done),
                    });
                }
                break;
            }
            file.write_all(&buffer[..read]).map_err(|error| {
                Failure::Disk(format!("Could not write {}: {error}", claim.part.display()))
            })?;
            done += read as u64;
            progress(Progress { done, total });
        }
        if total.is_some_and(|total| done < total) {
            return Err(Failure::Server(Error::Network(
                "the connection was lost.".into(),
            )));
        }
        // On the disk before it takes its name, so that a crash leaves no
        // empty file under a finished one's name.
        file.sync_all().map_err(|error| {
            Failure::Disk(format!("Could not write {}: {error}", claim.part.display()))
        })
    };
    let copied = copy();
    drop(file);
    if let Err(failure) = copied {
        stop.discard();
        return Err(failure);
    }
    stop.save(&claim)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(json: &str) -> BaseItemDto {
        serde_json::from_str(json).unwrap()
    }

    /// A folder of the test's own, emptied first.
    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "patina-jellyfin-download-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_name_comes_from_the_header_then_the_path_then_the_item() {
        // As Jellyfin writes it: "_" in `filename` for what isn't ASCII.
        let header = "attachment; filename=\"Am_lie (2001).mkv\"; \
                      filename*=UTF-8''Am%C3%A9lie%20%282001%29.mkv";
        assert_eq!(
            disposition_name(header).as_deref(),
            Some("Amélie (2001).mkv")
        );
        assert_eq!(file_name(Some(header), "fallback.mkv"), "Amélie (2001).mkv");
        assert_eq!(
            disposition_name("attachment; filename=\"The Long Field (2017).mkv\"").as_deref(),
            Some("The Long Field (2017).mkv")
        );
        assert_eq!(
            disposition_name("attachment;filename=plain.flac;size=12").as_deref(),
            Some("plain.flac")
        );
        assert_eq!(
            disposition_name(r#"attachment; filename="a \"quoted\" name.mp4""#).as_deref(),
            Some("a \"quoted\" name.mp4")
        );
        assert_eq!(
            disposition_name("attachment; FILENAME*=iso-8859-1'en'caf%E9.mp3").as_deref(),
            Some("café.mp3")
        );
        // A charset nobody sends, or broken escapes: the plain name.
        assert_eq!(
            disposition_name("attachment; filename*=koi8-r''x; filename=\"y.mkv\"").as_deref(),
            Some("y.mkv")
        );
        assert_eq!(
            disposition_name("attachment; filename*=UTF-8''%E2%28; filename=z.mkv").as_deref(),
            Some("z.mkv")
        );
        assert_eq!(disposition_name("attachment"), None);
        assert_eq!(disposition_name("attachment; filename=\"\""), None);
        assert_eq!(
            disposition_name("inline; junk; filename=ok.mkv").as_deref(),
            Some("ok.mkv")
        );
        assert_eq!(file_name(None, "fallback.mkv"), "fallback.mkv");
        assert_eq!(
            file_name(Some("attachment; filename=\"..\""), "f.mkv"),
            "f.mkv"
        );
        assert_eq!(
            file_name(Some("attachment; filename=\". .bashrc\""), "f.mkv"),
            "bashrc"
        );
        assert_eq!(file_name(None, "..."), "Download");

        let film = item(
            r#"{"Id": "m1", "Name": "Paper Harbor", "Type": "Movie",
                "Path": "/srv/media/Movies/Paper Harbor (2019)/Paper Harbor (2019).mp4",
                "Container": "mov,mp4,m4a,3gp,3g2,mj2"}"#,
        );
        assert_eq!(fallback_name(&film), "Paper Harbor (2019).mp4");
        let windows = item(
            r#"{"Id": "m2", "Name": "X", "Type": "Movie", "Path": "D:\\Films\\X (1999).mkv"}"#,
        );
        assert_eq!(fallback_name(&windows), "X (1999).mkv");
        let source = item(
            r#"{"Id": "m3", "Name": "Y", "Type": "Movie",
                "MediaSources": [{"Id": "s", "Path": "/m/Y.avi", "Container": "avi"}]}"#,
        );
        assert_eq!(fallback_name(&source), "Y.avi");
        let pathless = item(
            r#"{"Id": "m4", "Name": "Paper Harbor", "Type": "Movie", "Container": "mov,mp4,m4a"}"#,
        );
        assert_eq!(fallback_name(&pathless), "Paper Harbor.mp4");
        let song = item(
            r#"{"Id": "a1", "Name": "Low Tide", "Type": "Audio", "Container": "mov,mp4,m4a"}"#,
        );
        assert_eq!(fallback_name(&song), "Low Tide.m4a");
        let flac =
            item(r#"{"Id": "a2", "Name": "Night Ferry", "Type": "Audio", "Container": "flac"}"#);
        assert_eq!(fallback_name(&flac), "Night Ferry.flac");
        let bare = item(r#"{"Id": "a3", "Name": "Untitled", "Type": "Audio"}"#);
        assert_eq!(fallback_name(&bare), "Untitled");
    }

    #[test]
    fn names_are_made_fit_for_each_platform() {
        let unix = |name: &str| sanitize_for(name, Rules::Unix);
        let mac = |name: &str| sanitize_for(name, Rules::Mac);
        let windows = |name: &str| sanitize_for(name, Rules::Windows);
        // Never a path, nor hidden, nor "." or "..".
        assert_eq!(unix("../../.bashrc").as_deref(), Some("_.._.bashrc"));
        assert_eq!(unix("a/b\\c.mkv").as_deref(), Some("a_b_c.mkv"));
        assert_eq!(unix(".hidden.mkv").as_deref(), Some("hidden.mkv"));
        assert_eq!(unix(".."), None);
        assert_eq!(unix("  "), None);
        // Dots and spaces mixed: none of them hides the name either.
        assert_eq!(unix(". .bashrc").as_deref(), Some("bashrc"));
        assert_eq!(unix(". .."), None);
        assert_eq!(unix(" . . x.mkv").as_deref(), Some("x.mkv"));
        assert_eq!(unix(".\u{3000}.profile").as_deref(), Some("profile"));
        assert_eq!(windows(". .bash_profile").as_deref(), Some("bash_profile"));
        assert_eq!(unix("tab\there\n.mkv").as_deref(), Some("tab_here_.mkv"));
        assert_eq!(
            unix("What? <Now>: \"Yes\" | *.mkv").as_deref(),
            Some("What? <Now>: \"Yes\" | *.mkv")
        );
        assert_eq!(mac("Act 1: Dawn.mkv").as_deref(), Some("Act 1_ Dawn.mkv"));
        assert_eq!(
            windows("What? <Now>: \"Yes\" | *.mkv").as_deref(),
            Some("What_ _Now__ _Yes_ _ _.mkv")
        );
        assert_eq!(
            windows("Ends with a dot. ").as_deref(),
            Some("Ends with a dot")
        );
        assert_eq!(windows("CON.mkv").as_deref(), Some("_CON.mkv"));
        assert_eq!(windows("com1").as_deref(), Some("_com1"));
        assert_eq!(windows("lpt³.mp3").as_deref(), Some("_lpt³.mp3"));
        assert_eq!(windows("Console.mkv").as_deref(), Some("Console.mkv"));
        assert_eq!(windows("COM10.mkv").as_deref(), Some("COM10.mkv"));
        assert_eq!(unix("CON.mkv").as_deref(), Some("CON.mkv"));
        // Long names are cut at a character's edge, keeping the extension.
        let long = format!("{}.mkv", "é".repeat(150));
        let cut = unix(&long).unwrap();
        assert!(cut.len() <= NAME_LIMIT, "{}", cut.len());
        assert!(cut.ends_with("é.mkv"), "{cut}");
        let no_extension = "x".repeat(300);
        assert_eq!(unix(&no_extension).unwrap().len(), NAME_LIMIT);
        assert_eq!(
            sanitize("Amélie (2001).mkv").as_deref(),
            Some("Amélie (2001).mkv")
        );
        // A right-to-left override would show "Film\u{202E}vkm.exe" as
        // "Filmexe.mkv"; it, and what can't be seen, shows as "_".
        for rules in [Rules::Unix, Rules::Mac, Rules::Windows] {
            assert_eq!(
                sanitize_for("Film\u{202E}vkm.exe", rules).as_deref(),
                Some("Film_vkm.exe")
            );
            assert_eq!(
                sanitize_for("a\u{200B}b\u{2066}c\u{FEFF}.mkv", rules).as_deref(),
                Some("a_b_c_.mkv")
            );
        }
        // The joiners Persian and emoji need stay.
        assert_eq!(
            unix("Mi\u{200C}xvaham \u{1F469}\u{200D}\u{1F467}.mp3").as_deref(),
            Some("Mi\u{200C}xvaham \u{1F469}\u{200D}\u{1F467}.mp3")
        );
        // Windows drops a dot at the end, which a cut can leave there.
        let dotted = format!("{}.{}", "a".repeat(199), "b".repeat(40));
        let cut = windows(&dotted).unwrap();
        assert_eq!(cut, "a".repeat(199));
        assert!(!numbered(&cut, 2).ends_with('.'));
        assert!(unix(&dotted).unwrap().ends_with('.'));
    }

    #[test]
    fn a_name_taken_is_numbered_and_never_written_over() {
        assert_eq!(numbered("Film (2021).mkv", 1), "Film (2021).mkv");
        assert_eq!(numbered("Film (2021).mkv", 2), "Film (2021) (2).mkv");
        assert_eq!(numbered("archive.tar.gz", 3), "archive.tar (3).gz");
        assert_eq!(numbered("README", 2), "README (2)");
        assert_eq!(numbered(".mkv", 2), ".mkv (2)");

        let dir = folder("claim");
        fs::write(dir.join("Film.mkv"), b"mine").unwrap();
        // Another download's part file holds its name too.
        fs::write(dir.join("Film (2).mkv.patina.part"), b"").unwrap();
        let (claim, mut file) = super::claim(&dir, "Film.mkv").unwrap();
        assert_eq!(claim.part, dir.join("Film (3).mkv.patina.part"));
        file.write_all(b"new").unwrap();
        drop(file);
        // A file that appeared under the name meanwhile stays as it is.
        fs::write(dir.join("Film (3).mkv"), b"appeared").unwrap();
        let saved = finish(&claim).unwrap();
        assert_eq!(saved, dir.join("Film (4).mkv"));
        assert_eq!(fs::read(&saved).unwrap(), b"new");
        assert_eq!(fs::read(dir.join("Film.mkv")).unwrap(), b"mine");
        assert_eq!(fs::read(dir.join("Film (3).mkv")).unwrap(), b"appeared");
        assert!(!claim.part.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A part file nothing has written to for a while and no download
    /// holds was left by a Patina that ended without removing it, and its
    /// name is free; one a download holds, however old, isn't, nor is a
    /// browser's.
    #[test]
    fn a_part_file_left_behind_gives_its_name_back() {
        let dir = folder("left");
        let old = std::time::SystemTime::now() - LEFT_AFTER * 2;
        let aged = |name: &str| {
            let file = File::create(dir.join(name)).unwrap();
            file.set_modified(old).unwrap();
            file
        };
        drop(aged("Film.mkv.patina.part"));
        aged("Film.mkv.part");
        let held = aged("Song.flac.patina.part");
        held.lock().unwrap();
        // Written a moment ago: not left behind, though nothing holds it.
        fs::write(dir.join("Show.mkv.patina.part"), b"").unwrap();

        let (claim, file) = super::claim(&dir, "Film.mkv").unwrap();
        assert_eq!(claim.part, dir.join("Film.mkv.patina.part"));
        // The new part file is held, so not left behind however old.
        file.set_modified(old).unwrap();
        assert!(!left_behind(&claim.part));
        let (claim, _) = super::claim(&dir, "Song.flac").unwrap();
        assert_eq!(claim.part, dir.join("Song (2).flac.patina.part"));
        let (claim, _) = super::claim(&dir, "Show.mkv").unwrap();
        assert_eq!(claim.part, dir.join("Show (2).mkv.patina.part"));
        // A browser's part file is its own.
        assert!(dir.join("Film.mkv.part").exists());
        drop(held);
        let _ = fs::remove_dir_all(&dir);
    }

    /// A part file that a stopped download removed can keep its name on
    /// Windows until the download's thread closes it, and making it again
    /// is refused: the next name is taken. A folder that refuses everything
    /// still fails, with its own error.
    #[test]
    fn a_name_refused_is_passed_over_a_few_times() {
        let dir = folder("refused");
        let pending = dir.join("Film.mkv.patina.part");
        let refusing = |part: &Path| {
            if part == pending {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            } else {
                OpenOptions::new().write(true).create_new(true).open(part)
            }
        };
        let (claim, _) = claim_with(&dir, "Film.mkv", refusing).unwrap();
        assert_eq!(claim.part, dir.join("Film (2).mkv.patina.part"));
        let tries = std::cell::Cell::new(0);
        let refused = claim_with(&dir, "Song.flac", |_| {
            tries.set(tries.get() + 1);
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        });
        assert_eq!(
            refused.map(|_| ()).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(tries.get(), REFUSALS + 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_what_the_account_and_the_item_allow_is_offered() {
        let policy = |downloading: bool| UserPolicy {
            enable_content_downloading: downloading,
            ..UserPolicy::default()
        };
        let film = item(r#"{"Id": "m", "Type": "Movie"}"#);
        let refused = item(r#"{"Id": "m", "Type": "Movie", "CanDownload": false}"#);
        let granted = item(r#"{"Id": "s", "Type": "Audio", "CanDownload": true}"#);
        let missing = item(r#"{"Id": "e", "Type": "Episode", "LocationType": "Virtual"}"#);
        // The server never offers a folder itself, which downloads what it holds.
        let album = item(r#"{"Id": "a", "Type": "MusicAlbum", "CanDownload": false}"#);
        let person = item(r#"{"Id": "p", "Type": "Person"}"#);
        let collection = item(r#"{"Id": "c", "Type": "BoxSet"}"#);
        assert!(allowed(&policy(true), &film));
        assert!(allowed(&policy(true), &granted));
        assert!(allowed(&policy(true), &album));
        for (kind, offered) in [
            ("Series", true),
            ("Season", true),
            ("Playlist", true),
            ("Episode", true),
            ("Video", true),
            ("MusicArtist", false),
            ("Genre", false),
        ] {
            let item = item(&format!(r#"{{"Id": "x", "Type": "{kind}"}}"#));
            assert_eq!(allowed(&policy(true), &item), offered, "{kind}");
        }
        assert!(!allowed(&policy(true), &refused));
        assert!(!allowed(&policy(true), &missing));
        assert!(!allowed(&policy(true), &person));
        assert!(!allowed(&policy(true), &collection));
        for item in [&film, &granted, &album] {
            assert!(!allowed(&policy(false), item));
        }
        // The server's own default, for an account whose policy it left out.
        assert!(UserPolicy::default().enable_content_downloading);
    }

    /// A stand-in server that answers each connection with `head` and
    /// then `body`, from a thread, and closes it.
    fn serve(answers: Vec<(String, Vec<u8>)>) -> String {
        serve_holding(
            answers
                .into_iter()
                .map(|(head, body)| (head, body, Duration::ZERO))
                .collect(),
        )
    }

    /// The same, holding each connection open for `hold` after its body,
    /// as a server that stops sending does.
    fn serve_holding(answers: Vec<(String, Vec<u8>, Duration)>) -> String {
        use std::io::{BufRead, BufReader};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for (head, body, hold) in answers {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                    line.clear();
                }
                let _ = stream.write_all(head.as_bytes());
                // In pieces, as a network gives it.
                for piece in body.chunks(64 * 1024) {
                    if stream.write_all(piece).is_err() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                std::thread::sleep(hold);
            }
        });
        address
    }

    fn head(length: usize, name: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nContent-Type: video/x-matroska\r\n\
             Content-Disposition: attachment; filename=\"{name}\"\r\nConnection: close\r\n\r\n"
        )
    }

    fn client(address: &str) -> Client {
        let identity = super::super::auth::Identity {
            device: "Patina on Linux".into(),
            device_id: "download-test".into(),
            version: "9.9.9".into(),
        };
        Client::new(url::Url::parse(address).unwrap(), identity, None)
            .unwrap()
            .with_session("token", "user")
    }

    fn listing(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_download_is_saved_under_a_free_name_and_a_stopped_one_leaves_nothing() {
        let film: Vec<u8> = (0..700_000u32).map(|n| (n % 251) as u8).collect();
        let address = serve(vec![
            (head(film.len(), "Film (2021).mkv"), film.clone()),
            (head(film.len(), "Film (2021).mkv"), film.clone()),
            (head(film.len(), "Film (2021).mkv"), film.clone()),
            // The connection lost before the size the server gave.
            (
                head(film.len(), "Film (2021).mkv"),
                film[..100_000].to_vec(),
            ),
        ]);
        let client = client(&address);
        let dir = folder("run");
        let never = Stop::default();
        let first = Stop::default();
        let mut reports = Vec::new();
        let saved = run(&client, "m1", "fallback.mkv", &dir, &first, |progress| {
            reports.push(progress)
        })
        .unwrap();
        assert_eq!(saved, dir.join("Film (2021).mkv"));
        assert_eq!(fs::read(&saved).unwrap(), film);
        assert_eq!(reports[0].done, 0);
        assert_eq!(reports[0].total, Some(700_000));
        assert!(reports.len() >= 2 && reports[1].done > 0, "{reports:?}");
        assert_eq!(reports.last().unwrap().done, 700_000);
        // Stopped once saved, it keeps its file, and says it came too late.
        assert!(!first.stop());
        assert_eq!(fs::read(&saved).unwrap(), film);

        let saved = run(&client, "m1", "fallback.mkv", &dir, &never, |_| {}).unwrap();
        assert_eq!(saved, dir.join("Film (2021) (2).mkv"));

        // Cancelled once the first bytes are on the disk.
        let cancel = Stop::default();
        let mut part_seen = 0;
        let stopped = run(&client, "m1", "fallback.mkv", &dir, &cancel, |progress| {
            if progress.done > 0 && part_seen == 0 {
                part_seen = fs::metadata(dir.join("Film (2021) (3).mkv.patina.part"))
                    .map(|meta| meta.len())
                    .unwrap_or(0);
                assert!(cancel.stop());
            }
        });
        assert_eq!(stopped, Err(Failure::Cancelled));
        assert!(part_seen > 0);
        let lost = run(&client, "m1", "fallback.mkv", &dir, &never, |_| {});
        assert!(
            matches!(lost, Err(Failure::Server(Error::Network(_)))),
            "{lost:?}"
        );
        assert_eq!(listing(&dir), ["Film (2021) (2).mkv", "Film (2021).mkv"]);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Stopping a download whose server has stopped sending removes its
    /// part file at once, rather than when the read gives up, and it ends
    /// as cancelled rather than failed. While the read waits, its meter
    /// holds everything that came before it, and when that was.
    #[test]
    fn a_stalled_download_stops_at_once() {
        let film = vec![7u8; 300_000];
        let address = serve_holding(vec![(
            head(700_000, "Stalled.mkv"),
            film,
            Duration::from_secs(2),
        )]);
        let dir = folder("stall");
        let stop = std::sync::Arc::new(Stop::default());
        let meter = std::sync::Arc::new(Meter::started());
        let (started, first) = std::sync::mpsc::channel();
        let running = {
            let (dir, stop, meter) = (dir.clone(), stop.clone(), meter.clone());
            std::thread::spawn(move || {
                let client = client(&address);
                run(&client, "m1", "f.mkv", &dir, &stop, |progress| {
                    meter.note(progress);
                    if progress.done > 0 {
                        let _ = started.send(());
                    }
                })
            })
        };
        first.recv_timeout(Duration::from_secs(10)).unwrap();
        // Everything sent is read by now, and the next read waits.
        std::thread::sleep(Duration::from_millis(300));
        let (progress, moved) = meter.read();
        assert_eq!(
            progress,
            Progress {
                done: 300_000,
                total: Some(700_000)
            }
        );
        assert!(moved.elapsed() >= Duration::from_millis(200));
        let part = dir.join("Stalled.mkv.patina.part");
        assert!(part.exists());
        assert!(stop.stop());
        assert!(!part.exists());
        // The name is free while the read still waits; on Windows it can
        // stay taken until the thread closes the file, and `claim` passes
        // over it (see `REFUSALS`).
        assert!(!running.is_finished());
        let (claim, _) = super::claim(&dir, "Stalled.mkv").unwrap();
        assert_eq!(claim.part, part);
        drop(claim);
        assert_eq!(running.join().unwrap(), Err(Failure::Cancelled));
        // Its thread never removes what isn't its own any more.
        assert!(part.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Without a size or chunks, a connection lost would look like the
    /// file's end, so such an answer isn't saved at all; a chunked one is,
    /// and one cut short fails.
    #[test]
    fn a_download_that_could_end_early_unseen_is_refused() {
        let film: Vec<u8> = (0..100_000u32).map(|n| (n % 241) as u8).collect();
        let chunked = |body: &[u8], last: bool| {
            let mut out = Vec::new();
            for piece in body.chunks(30_000) {
                out.extend_from_slice(format!("{:x}\r\n", piece.len()).as_bytes());
                out.extend_from_slice(piece);
                out.extend_from_slice(b"\r\n");
            }
            if last {
                out.extend_from_slice(b"0\r\n\r\n");
            }
            out
        };
        let unsized_head = |extra: &str| {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: video/x-matroska\r\n{extra}\
                 Content-Disposition: attachment; filename=\"Open.mkv\"\r\nConnection: close\r\n\r\n"
            )
        };
        let address = serve(vec![
            (unsized_head(""), film.clone()),
            (
                unsized_head("Transfer-Encoding: chunked\r\n"),
                chunked(&film, true),
            ),
            (
                unsized_head("Transfer-Encoding: chunked\r\n"),
                chunked(&film[..60_000], false),
            ),
        ]);
        let client = client(&address);
        let dir = folder("framing");
        let never = Stop::default();
        let refused = run(&client, "m1", "f.mkv", &dir, &never, |_| {});
        assert!(
            matches!(refused, Err(Failure::Server(Error::NoDownload(_)))),
            "{refused:?}"
        );
        let mut last = None;
        let saved = run(&client, "m1", "f.mkv", &dir, &never, |progress| {
            last = Some(progress)
        })
        .unwrap();
        assert_eq!(fs::read(&saved).unwrap(), film);
        // A chunked answer's size is known once its end has come.
        assert_eq!(
            last,
            Some(Progress {
                done: 100_000,
                total: Some(100_000)
            })
        );
        let lost = run(&client, "m1", "f.mkv", &dir, &never, |_| {});
        assert!(
            matches!(lost, Err(Failure::Server(Error::Network(_)))),
            "{lost:?}"
        );
        assert_eq!(listing(&dir), ["Open.mkv"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn progress_tells_the_time_left_and_the_speed_settles() {
        let progress = Progress {
            done: 25,
            total: Some(100),
        };
        assert_eq!(progress.eta(Some(10.0)), Some(8));
        assert_eq!(progress.eta(None), None);
        assert_eq!(progress.eta(Some(0.0)), None);
        assert_eq!(
            Progress {
                total: None,
                ..progress
            }
            .eta(Some(10.0)),
            None
        );
        let mut speed = Speed::default();
        let start = Instant::now();
        assert_eq!(speed.add(start, 0), None);
        assert_eq!(speed.add(start + Duration::from_millis(500), 50), None);
        assert_eq!(speed.add(start + Duration::from_secs(2), 400), Some(200.0));
        // Only the last few seconds count.
        assert_eq!(speed.add(start + Duration::from_secs(4), 500), Some(50.0));
    }
}
