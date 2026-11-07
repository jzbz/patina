//! Downloading online videos with yt-dlp, as IINA's Online Media plugin
//! does, and files from Jellyfin servers: each download runs in the
//! background, and the Downloads window lists them with their progress.
//!
//! yt-dlp is told to print its progress, the post-processing step it is
//! on, and the file it made in fixed templates of Patina's own, so nothing
//! depends on the wording of its usual output. A Jellyfin file is fetched
//! by Patina itself (`jellyfin::download`), on a thread of its own, one at
//! a time from each server: the others wait their turn, so that a whole
//! show asked for at once doesn't open a connection for every episode. The
//! next one waits for a stopped one's thread to end too, which a server
//! that has stopped sending can hold up for a minute.
//!
//! A video from the YouTube section waits its turn too: at most two of yt-dlp
//! run while one of those waits, started some seconds apart, since a guest of
//! YouTube's is only allowed so many requests. Such a video is known by its id,
//! has its failure said by name, and goes on its list as downloaded.

use crate::app::App;
use crate::jellyfin::api::{Client, Error as JellyfinError};
use crate::jellyfin::download::{self as jellyfin, Failure, Meter, Progress, Speed, Stop};
use crate::jellyfin::session::{Change, Listener};
use crate::settings::{Preferences, read_ytdl_raw_options, ytdl_youtube_clients};
use crate::youtube::{VideoId, ytdlp};
use crate::{DownloadRow, DownloadsWindow};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const PROGRESS: &str = "patina-progress";
const PROCESS: &str = "patina-process";
const FILE: &str = "patina-file";
/// How long quitting waits for the threads of the Jellyfin downloads it
/// stops to end. Their part files are gone already (see Stop); a thread
/// that ends in time can't be making one as Patina exits.
const STOP_WAIT: Duration = Duration::from_secs(1);
/// How often the rows of the Jellyfin downloads read their meters.
const TICK: Duration = Duration::from_millis(250);
/// How long a Jellyfin download may go without a byte, or without its
/// server's answer as it starts, before its row says it waits for the
/// server, rather than show a speed it no longer has and the time that
/// would leave, or "Starting…" for as long as the server takes.
const STALLED_AFTER: Duration = Duration::from_secs(5);
/// How many of yt-dlp may run for the next video from the section to start.
const SECTION_LIMIT: usize = 2;
/// How long after the last of yt-dlp started the next video from the section
/// may. yt-dlp's own advice is 5 to 10 seconds between downloads.
const SECTION_GAP: Duration = Duration::from_secs(8);

/// A video to download: what is playing.
#[derive(Debug, Clone)]
pub struct Request {
    /// The page, which yt-dlp is given again.
    pub url: String,
    pub title: String,
    /// yt-dlp's format ids for what is playing; None leaves it to yt-dlp.
    pub format: Option<String>,
    /// The video when it comes from the YouTube section, which has its own
    /// options and waits its turn.
    pub video: Option<VideoId>,
}

/// The yt-dlp that playback, downloads and the YouTube section run: the
/// user's choice, else the newer of the package's and one the user installed,
/// else one on the `PATH`.
///
/// That is mpv's yt-dlp hook's search, with the package's copy put in: the
/// configured path (or paths), or else `yt-dlp`, `yt-dlp_x86` and
/// `youtube-dl`, each first in mpv's configuration directory and then on the
/// `PATH`. Where nothing is bundled (Linux, the Flatpak, a build run from the
/// source tree) the search is the hook's exactly. Where something is,
/// `Preferences::mpv_options` names the result to the hook, so that the two
/// can't disagree, and the user's choice comes first: the path preference,
/// then a `ytdl_path` in the hook's own `script-opts/ytdl_hook.conf` (which
/// `mpv_options` then leaves the hook to read), then a `yt-dlp` put in mpv's
/// configuration directory. A `youtube-dl` there doesn't count: it can't play
/// YouTube, and the section refuses it. With none of those, a `yt-dlp` the
/// user installed (`installed_ytdl`) is used instead of the package's copy
/// when it is the newer of the two (`ytdl_versions`), so that one kept up to
/// date wins over a bundled copy grown stale between Patina's releases,
/// without the user having to name it. This never waits for that answer: until
/// both have given their versions, the package's copy is used.
pub fn find_ytdl(prefs: &Preferences, mpv_config: &Path) -> Option<PathBuf> {
    find_ytdl_in(
        prefs,
        mpv_config,
        crate::platform::bundled("yt-dlp"),
        crate::platform::find_program,
        installed_ytdl,
        |bundled, own| crate::ytdl_versions::global().newer(bundled, own),
    )
}

/// A `yt-dlp` the user installed: on the `PATH`, as from winget, pip or a
/// shell's Homebrew, and on macOS also in Homebrew's own folders, since an app
/// opened from the Finder or the Dock has only the system's folders on its
/// `PATH`.
fn installed_ytdl(name: &str) -> Option<PathBuf> {
    crate::platform::find_program(name).or_else(|| {
        let folders: &[&str] = if cfg!(target_os = "macos") {
            &["/opt/homebrew/bin", "/usr/local/bin"]
        } else {
            &[]
        };
        folders
            .iter()
            .map(|folder| Path::new(folder).join(name))
            .find(|path| path.is_file())
    })
}

fn find_ytdl_in(
    prefs: &Preferences,
    mpv_config: &Path,
    bundled: Option<PathBuf>,
    on_path: impl Fn(&str) -> Option<PathBuf>,
    installed: impl Fn(&str) -> Option<PathBuf>,
    newer: impl FnOnce(PathBuf, Option<PathBuf>) -> PathBuf,
) -> Option<PathBuf> {
    let in_config = |name: &str| {
        let exe = if cfg!(windows) && !name.to_ascii_lowercase().ends_with(".exe") {
            format!("{name}.exe")
        } else {
            name.to_owned()
        };
        Some(mpv_config.join(exe)).filter(|path| path.is_file())
    };
    let search = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| in_config(name).or_else(|| on_path(name)))
    };
    let configured = ytdl_paths(&prefs.ytdl_path);
    if !configured.is_empty() {
        return search(&configured);
    }
    let Some(bundled) = bundled else {
        return search(&["yt-dlp", "yt-dlp_x86", "youtube-dl"]);
    };
    if let Some(hook) = hook_ytdl_path(prefs, mpv_config) {
        return search(&ytdl_paths(&hook));
    }
    const YT_DLP: [&str; 2] = ["yt-dlp", "yt-dlp_x86"];
    if let Some(own) = YT_DLP.into_iter().find_map(in_config) {
        return Some(own);
    }
    // Never youtube-dl, which can't play YouTube, and never the package's
    // copy again, should its folder be on the PATH. Asked with none
    // installed too, so that the choice moving back to the package's copy
    // when the user's is removed reaches mpv's hook.
    let own = YT_DLP
        .into_iter()
        .find_map(installed)
        .filter(|own| *own != bundled);
    Some(newer(bundled, own))
}

/// A `ytdl_path` as the hook reads it: paths split at the `PATH` separator.
fn ytdl_paths(paths: &str) -> Vec<&str> {
    let separator = if cfg!(windows) { ';' } else { ':' };
    paths
        .split(separator)
        .filter(|path| !path.trim().is_empty())
        .collect()
}

/// The `ytdl_path` in mpv's `script-opts/ytdl_hook.conf`, which mpv reads
/// when Patina hands it its configuration directory
/// (`Preferences::use_mpv_config`). Read as mpv's options.lua reads it: a
/// `key=value` a line, `#` lines and blank lines skipped, nothing trimmed,
/// the last one winning. An empty one names nothing, to the hook too.
pub(crate) fn hook_ytdl_path(prefs: &Preferences, mpv_config: &Path) -> Option<String> {
    if !prefs.use_mpv_config {
        return None;
    }
    let text =
        std::fs::read_to_string(mpv_config.join("script-opts").join("ytdl_hook.conf")).ok()?;
    text.lines()
        .filter(|line| {
            let line = line.trim_start();
            !line.is_empty() && !line.starts_with('#')
        })
        .filter_map(|line| line.split_once('='))
        .filter(|(key, _)| *key == "ytdl_path")
        .map(|(_, value)| value.to_owned())
        .next_back()
        .filter(|value| !ytdl_paths(value).is_empty())
}

/// The ffmpeg a download's yt-dlp joins video and audio with.
#[derive(Debug, Clone, PartialEq)]
pub enum Ffmpeg {
    /// The yt-dlp options name one (`ffmpeg-location`), and yt-dlp is left
    /// to it.
    Theirs,
    /// The package's own, which yt-dlp is pointed at with `--ffmpeg-location`,
    /// as it would never find it by itself: it isn't on the `PATH`, and on
    /// macOS an app opened from the Finder doesn't even have Homebrew's
    /// folders there. As an option on the command line it also wins over an
    /// `ffmpeg-location` in the user's own yt-dlp configuration file, which
    /// yt-dlp reads first; Patina's yt-dlp options are the place to name
    /// another, as the README and docs/youtube.md say.
    Bundled(PathBuf),
    /// One on the `PATH`, which yt-dlp finds the same way.
    OnPath,
    Missing,
}

impl Ffmpeg {
    /// For a download with these yt-dlp options (`ytdl-raw-options` form).
    pub fn for_options(raw: &str) -> Self {
        Self::choose(
            raw,
            crate::platform::bundled("ffmpeg"),
            crate::platform::find_program,
        )
    }

    fn choose(
        raw: &str,
        bundled: Option<PathBuf>,
        on_path: impl Fn(&str) -> Option<PathBuf>,
    ) -> Self {
        if read_ytdl_raw_options(raw)
            .iter()
            .any(|(key, value)| *key == "ffmpeg-location" && !value.trim().is_empty())
        {
            Ffmpeg::Theirs
        } else if let Some(bundled) = bundled {
            Ffmpeg::Bundled(bundled)
        } else if on_path("ffmpeg").is_some() {
            Ffmpeg::OnPath
        } else {
            Ffmpeg::Missing
        }
    }

    /// Whether yt-dlp can join separate video and audio. One the user named
    /// is taken at their word.
    pub fn found(&self) -> bool {
        !matches!(self, Ffmpeg::Missing)
    }

    /// Whether a live stream can be downloaded with it. yt-dlp hands one to
    /// ffmpeg to record, and the package's ffmpeg is built without network
    /// protocols (packaging/ffmpeg.sh), so it fails at once with "ffmpeg
    /// exited with code 8". One of the user's own may well record it, and
    /// with none it is left to yt-dlp, as it always was.
    pub fn records_live(&self) -> bool {
        !matches!(self, Ffmpeg::Bundled(_))
    }
}

/// The format to download: what is playing, unless that takes merging
/// video and audio and there is no ffmpeg to do it, when yt-dlp picks the
/// best it can download as one file.
pub fn download_format(playing: Option<&str>, ffmpeg: &Ffmpeg) -> Option<String> {
    let playing = playing?;
    if playing.contains('+') && !ffmpeg.found() {
        return None;
    }
    Some(playing.to_owned())
}

/// mpv's `ytdl-raw-options` ("key=value,flag=") as yt-dlp arguments, the
/// way mpv's yt-dlp hook passes them.
fn raw_options(raw: &str) -> Vec<String> {
    let mut args = Vec::new();
    for (key, value) in read_ytdl_raw_options(raw) {
        let value = value.trim();
        args.push(format!("--{key}"));
        if !value.is_empty() || key == "proxy" {
            args.push(value.to_owned());
        }
    }
    args
}

/// One line of yt-dlp's output, in Patina's templates.
#[derive(Debug, PartialEq)]
enum Line {
    Progress {
        done: Option<u64>,
        total: Option<u64>,
        speed: Option<f64>,
        eta: Option<u64>,
    },
    /// A post-processing step, such as "Merger".
    Processing(String),
    File(PathBuf),
}

fn parse_line(line: &str) -> Option<Line> {
    let line = line.trim_end_matches(['\r', '\n']);
    if let Some(rest) = line
        .strip_prefix(FILE)
        .and_then(|rest| rest.strip_prefix(' '))
    {
        return Some(Line::File(PathBuf::from(rest)));
    }
    if let Some(rest) = line
        .strip_prefix(PROCESS)
        .and_then(|rest| rest.strip_prefix(' '))
    {
        return Some(Line::Processing(rest.trim().to_owned()));
    }
    let rest = line.strip_prefix(PROGRESS)?.strip_prefix(' ')?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let number = |index: usize| {
        fields
            .get(index)
            .and_then(|field| field.parse::<f64>().ok())
    };
    let bytes = |index: usize| number(index).map(|value| value as u64);
    Some(Line::Progress {
        done: bytes(0),
        total: bytes(1).or(bytes(2)),
        speed: number(3),
        eta: bytes(4),
    })
}

fn command(
    program: &Path,
    request: &Request,
    folder: &Path,
    raw: &str,
    ffmpeg: &Ffmpeg,
) -> Command {
    let mut command = Command::new(program);
    command
        .args([
            "--newline",
            "--progress",
            "--no-playlist",
            "--encoding",
            "utf-8",
        ])
        .arg("--progress-template")
        .arg(format!(
            "download:{PROGRESS} %(progress.downloaded_bytes)s %(progress.total_bytes)s \
             %(progress.total_bytes_estimate)s %(progress.speed)s %(progress.eta)s"
        ))
        .arg("--progress-template")
        .arg(format!(
            "postprocess:{PROCESS} %(progress.postprocessor)s %(progress.status)s"
        ))
        .arg("--print")
        .arg(format!("after_move:{FILE} %(filepath)s"))
        .arg("--paths")
        .arg(folder);
    if let Ffmpeg::Bundled(path) = ffmpeg {
        command.arg("--ffmpeg-location").arg(path);
    }
    if let Some(format) = &request.format {
        command.args(["--format", format]);
    }
    // yt-dlp waits by itself for what these clients avoid, so a download
    // would work without them. With them it needn't wait, and the format
    // that is playing is one these clients offered.
    if let Some(clients) = ytdl_youtube_clients(raw) {
        command.args(["--extractor-args", clients]);
    }
    command
        .args(raw_options(raw))
        .arg("--")
        .arg(&request.url)
        // yt-dlp's file names in UTF-8 whatever the console's encoding.
        .env("PYTHONIOENCODING", "utf-8")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::platform::no_console(&mut command);
    // So that stopping it reaches what it starts (`stop_ytdl`).
    crate::platform::in_own_group(&mut command);
    command
}

#[derive(Debug, Clone, PartialEq)]
enum State {
    /// A Jellyfin download waiting for the one before it from the same
    /// server, or a video from the YouTube section for its turn.
    Queued,
    Running {
        done: u64,
        total: Option<u64>,
        speed: Option<f64>,
        eta: Option<u64>,
        merging: bool,
        /// A Jellyfin download whose server has sent nothing for a while.
        stalled: bool,
    },
    Done(PathBuf),
    Failed(String),
    Cancelled,
}

impl State {
    fn running() -> Self {
        State::Running {
            done: 0,
            total: None,
            speed: None,
            eta: None,
            merging: false,
            stalled: false,
        }
    }
}

/// A running Jellyfin download's state as of `now`, from its meter's
/// reading: `progress`, as it was when bytes last came, or the download
/// started (`moved`), and the download's `speed` so far, which this adds
/// to. A speed of nothing, from
/// a pause shorter than `STALLED_AFTER` but longer than the speed's span,
/// goes unsaid. One that has every byte waits for nobody but the disk,
/// which can take a while to hold a large file, and its row says nothing
/// of the server.
fn metered(progress: Progress, moved: Instant, now: Instant, speed: &mut Speed) -> State {
    let rate = speed.add(now, progress.done);
    let complete = progress.total.is_some_and(|total| progress.done >= total);
    let stalled = !complete && now.duration_since(moved) >= STALLED_AFTER;
    State::Running {
        done: progress.done,
        total: progress.total,
        speed: rate.filter(|rate| *rate > 0.0 && !stalled),
        eta: progress.eta(rate).filter(|_| !stalled),
        merging: false,
        stalled,
    }
}

/// What a download's thread reports to the window. A Jellyfin download
/// reports in yt-dlp's terms the file it saved and its end; the window
/// reads its progress from its meter (`Downloads::tick`).
enum Report {
    Line(Line),
    /// The server no longer takes the sign-in the download went out with.
    Unauthorized,
    Finished(Result<(), String>),
}

/// What a row downloads, and what stops it.
enum Job {
    Ytdl {
        request: Request,
        child: Option<Arc<Mutex<Child>>>,
    },
    Jellyfin {
        request: jellyfin::Request,
        /// While its thread runs: what stops it, and removes its part file.
        stop: Option<Arc<Stop>>,
        /// While its thread runs: how far it has come, which the row reads
        /// (`Downloads::tick`).
        meter: Option<Arc<Meter>>,
        speed: Speed,
        /// Until its thread has said it ended, stopped or not.
        thread: Option<JoinHandle<()>>,
        /// The client it went out with, whose 401 signs the account out.
        client: Option<Box<Client>>,
    },
}

struct Task {
    id: u64,
    job: Job,
    state: State,
    file: Option<PathBuf>,
}

impl Task {
    fn title(&self) -> String {
        match &self.job {
            Job::Ytdl { request, .. } if request.title.is_empty() => request.url.clone(),
            Job::Ytdl { request, .. } => request.title.clone(),
            Job::Jellyfin { request, .. } => request.title.clone(),
        }
    }

    /// Running or waiting to: not to be removed or cleared.
    fn active(&self) -> bool {
        matches!(self.state, State::Queued | State::Running { .. })
    }

    /// A Jellyfin download stopped whose thread hasn't ended yet: not to be
    /// removed, cleared or retried, since its server's next download waits
    /// for it.
    fn stopping(&self) -> bool {
        !self.active()
            && matches!(
                self.job,
                Job::Jellyfin {
                    thread: Some(_),
                    ..
                }
            )
    }

    /// Done with: neither running, waiting nor stopping.
    fn finished(&self) -> bool {
        !self.active() && !self.stopping()
    }

    /// Holding up its server's next download: running, or stopping.
    fn downloading(&self) -> bool {
        matches!(self.state, State::Running { .. }) || self.stopping()
    }

    /// yt-dlp running.
    fn ytdl_running(&self) -> bool {
        matches!(self.job, Job::Ytdl { .. }) && matches!(self.state, State::Running { .. })
    }

    /// The YouTube section's video it downloads.
    fn video(&self) -> Option<&VideoId> {
        match &self.job {
            Job::Ytdl { request, .. } => request.video.as_ref(),
            Job::Jellyfin { .. } => None,
        }
    }

    /// The Jellyfin server it downloads from.
    fn server(&self) -> Option<&str> {
        match &self.job {
            Job::Jellyfin { request, .. } => Some(&request.server_id),
            Job::Ytdl { .. } => None,
        }
    }

    /// Stops it: yt-dlp is killed, and a Jellyfin download's part file
    /// removed and its thread told to stop, which it does at its next read.
    /// False when that comes too late: a Jellyfin download that has saved
    /// its file, which its thread is about to report, and which stays
    /// running until then.
    fn stop(&mut self) -> bool {
        match &mut self.job {
            Job::Ytdl { child, .. } => {
                if let Some(child) = child {
                    stop_ytdl(child);
                }
                true
            }
            Job::Jellyfin { stop, .. } => stop.as_ref().is_none_or(|stop| stop.stop()),
        }
    }
}

/// Stops a download's yt-dlp with everything it started: ffmpeg may be
/// joining the files, and a yt-dlp the user installed may be one of yt-dlp's
/// single-file builds, a launcher whose real yt-dlp is a process of its own,
/// which stopping the launcher alone would leave downloading, holding the
/// output open, so the download would go on to its end. The group is asked
/// to end first, so that ffmpeg can close its file and such a launcher can
/// remove the folder it unpacked (`platform::end_group`), and killed on a
/// thread of its own if it is still running after `platform::END_GRACE`, so
/// that the window doesn't wait. One that has ended already is left alone,
/// as its id may since be someone else's; the thread that follows the
/// download waits for it.
fn stop_ytdl(child: &Arc<Mutex<Child>>) {
    let asked = match child.lock() {
        Ok(mut child) => {
            matches!(child.try_wait(), Ok(None)) && crate::platform::end_group(&mut child)
        }
        Err(_) => false,
    };
    if !asked {
        return;
    }
    let stopping = Arc::clone(child);
    let spawned = std::thread::Builder::new()
        .name("stop yt-dlp".into())
        .spawn(move || {
            let until = Instant::now() + crate::platform::END_GRACE;
            loop {
                let Ok(mut child) = stopping.lock() else {
                    return;
                };
                if !matches!(child.try_wait(), Ok(None)) {
                    return;
                }
                if Instant::now() >= until {
                    crate::platform::kill_group(&mut child);
                    return;
                }
                drop(child);
                std::thread::sleep(Duration::from_millis(20));
            }
        });
    if let Err(error) = spawned {
        log::warn!("could not wait for a download to stop: {error}");
        if let Ok(mut child) = child.lock()
            && matches!(child.try_wait(), Ok(None))
        {
            crate::platform::kill_group(&mut child);
        }
    }
}

pub struct Downloads {
    app: Weak<App>,
    window: DownloadsWindow,
    rows: Rc<VecModel<DownloadRow>>,
    tasks: RefCell<Vec<Task>>,
    next_id: Cell<u64>,
    /// The Jellyfin downloads that have ended, by server, since its last
    /// had none after it: an album says it is downloaded once, when its
    /// last song is, rather than once a song. Those that fail as they
    /// start count too.
    ended: RefCell<HashMap<String, Tally>>,
    /// Reads the Jellyfin downloads' meters while any runs or waits.
    ticker: slint::Timer,
    /// When yt-dlp last started, which spaces out the section's videos.
    ytdl_started: Cell<Option<Instant>>,
    /// Starts the section's next video when the wait between them is over.
    section_timer: slint::Timer,
    /// Counts the times the window named its yt-dlp (`describe_program`),
    /// so that a version read for a copy no longer chosen, as the package's
    /// slower one can answer after the user's that replaced it, is dropped.
    described: Arc<AtomicU64>,
}

/// Downloads of one server that ended, for the message when the last does.
#[derive(Debug, Default, Clone, PartialEq)]
struct Tally {
    saved: usize,
    failed: usize,
    /// The last one's title.
    title: String,
}

impl Tally {
    /// What the message says: the title for one file, how many for more.
    fn message(&self) -> Option<(String, String)> {
        let files = |count: usize| {
            if count == 1 {
                "1 File".to_owned()
            } else {
                format!("{count} Files")
            }
        };
        match (self.saved, self.failed) {
            (0, 0) => None,
            (1, 0) => Some(("Downloaded".into(), self.title.clone())),
            (0, 1) => Some(("Download failed".into(), self.title.clone())),
            (saved, 0) => Some((format!("Downloaded {}", files(saved)), String::new())),
            (0, failed) => Some((
                format!("{} Failed to Download", files(failed)),
                String::new(),
            )),
            (saved, failed) => Some((
                format!("Downloaded {}", files(saved)),
                format!("{failed} failed"),
            )),
        }
    }
}

/// Sizes the way file managers show them, in powers of 1000.
fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["kB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = "B";
    for next in UNITS {
        if value < 1000.0 {
            break;
        }
        value /= 1000.0;
        unit = next;
    }
    format!("{value:.1} {unit}")
}

fn detail(task: &Task) -> String {
    if task.stopping() && task.state == State::Cancelled {
        return "Cancelling…".into();
    }
    match &task.state {
        State::Queued => "Waiting for the download before it…".into(),
        State::Running { merging: true, .. } => "Joining the video and audio…".into(),
        State::Running {
            done,
            total,
            speed,
            eta,
            stalled,
            ..
        } => {
            let mut parts = Vec::new();
            match total {
                Some(total) => parts.push(format!("{} of {}", size(*done), size(*total))),
                None if *done > 0 => parts.push(size(*done)),
                None if !*stalled => parts.push("Starting…".into()),
                None => {}
            }
            if *stalled {
                parts.push("Waiting for the server…".into());
            }
            if let Some(speed) = speed {
                parts.push(format!("{}/s", size(*speed as u64)));
            }
            if let Some(eta) = eta {
                parts.push(format!("{} left", crate::util::format_time(*eta as f64)));
            }
            parts.join(" · ")
        }
        State::Done(path) => path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        State::Failed(error) => error.clone(),
        State::Cancelled => "Cancelled".into(),
    }
}

fn row(task: &Task) -> DownloadRow {
    let (state, progress) = match &task.state {
        _ if task.stopping() => ("stopping", -1.0),
        State::Queued => ("queued", -1.0),
        State::Running { done, total, .. } => (
            "running",
            match total {
                Some(total) if *total > 0 => *done as f32 / *total as f32,
                _ => -1.0,
            },
        ),
        State::Done(_) => ("done", 1.0),
        State::Failed(_) => ("failed", -1.0),
        State::Cancelled => ("cancelled", -1.0),
    };
    DownloadRow {
        title: task.title().into(),
        detail: detail(task).into(),
        progress,
        state: state.into(),
    }
}

impl Downloads {
    pub fn new(app: &Rc<App>) -> Result<Rc<Self>, String> {
        let window = DownloadsWindow::new().map_err(|error| error.to_string())?;
        let rows = Rc::new(VecModel::default());
        window.set_rows(ModelRc::from(rows.clone()));
        let downloads = Rc::new(Self {
            app: Rc::downgrade(app),
            window,
            rows,
            tasks: RefCell::new(Vec::new()),
            next_id: Cell::new(1),
            ended: RefCell::new(HashMap::new()),
            ticker: slint::Timer::default(),
            ytdl_started: Cell::new(None),
            section_timer: slint::Timer::default(),
            described: Arc::new(AtomicU64::new(0)),
        });
        downloads.wire();
        let listener: Weak<dyn Listener> = Rc::downgrade(&downloads) as Weak<dyn Listener>;
        app.jellyfin().subscribe(listener);
        Ok(downloads)
    }

    fn app(&self) -> Rc<App> {
        self.app.upgrade().expect("the app outlives its downloads")
    }

    fn wire(self: &Rc<Self>) {
        macro_rules! on {
            ($method:ident, |$this:ident $(, $arg:ident)*| $body:expr) => {{
                let weak = Rc::downgrade(self);
                self.window.$method(move |$($arg),*| {
                    if let Some($this) = weak.upgrade() {
                        $body
                    }
                });
            }};
        }
        on!(on_cancel, |this, index| this.cancel(index as usize));
        on!(on_reveal, |this, index| {
            let tasks = this.tasks.borrow();
            if let Some(State::Done(path)) = tasks.get(index as usize).map(|task| &task.state) {
                crate::platform::reveal(path);
            }
        });
        on!(on_retry, |this, index| this
            .retry(|_, at| at == index as usize));
        on!(on_retry_failed, |this| this
            .retry(|task, _| matches!(task.state, State::Failed(_))));
        on!(on_cancel_all, |this| this.cancel_all());
        on!(on_remove, |this, index| this.remove(index as usize));
        on!(on_open_folder, |this| {
            let folder = this.app().prefs().download_dir();
            if let Err(error) = std::fs::create_dir_all(&folder) {
                log::warn!("could not make {}: {error}", folder.display());
            }
            crate::platform::open_path(&folder);
        });
        on!(on_clear_finished, |this| {
            this.tasks.borrow_mut().retain(|task| !task.finished());
            this.refresh();
        });
    }

    /// Starts again the finished downloads `which` picks, given each with
    /// its row, each in a new row at the end.
    fn retry(self: &Rc<Self>, which: impl Fn(&Task, usize) -> bool) {
        let mut videos = Vec::new();
        let mut files = Vec::new();
        let mut index = 0;
        self.tasks.borrow_mut().retain(|task| {
            let picked = task.finished() && which(task, index);
            index += 1;
            if picked {
                match &task.job {
                    Job::Ytdl { request, .. } => videos.push(request.clone()),
                    Job::Jellyfin { request, .. } => files.push(request.clone()),
                }
            }
            !picked
        });
        if videos.is_empty() && files.is_empty() {
            return;
        }
        self.refresh();
        for request in videos {
            self.start(request);
        }
        if !files.is_empty() {
            self.start_jellyfin(files);
        }
    }

    pub fn show(self: &Rc<Self>) {
        self.refresh();
        self.describe_program();
        if let Err(error) = self.window.show() {
            log::warn!("could not show the downloads: {error}");
        }
    }

    /// Another yt-dlp is to be used (`App::ytdl_choice_changed`): the window
    /// names it.
    pub fn ytdl_changed(&self) {
        self.describe_program();
    }

    /// Which yt-dlp downloads, and its version, read on a thread of its own.
    fn describe_program(&self) {
        let app = self.app();
        let serial = self.described.fetch_add(1, Ordering::Relaxed) + 1;
        let Some(program) = find_ytdl(&app.prefs(), &app.mpv_config_dir()) else {
            self.window.set_program(
                "yt-dlp was not found. Install it, or set where it is in Preferences › Network."
                    .into(),
            );
            return;
        };
        self.window
            .set_program(format!("yt-dlp at {}", program.display()).into());
        let weak = self.window.as_weak();
        let described = Arc::clone(&self.described);
        let spawned = std::thread::Builder::new()
            .name("yt-dlp version".into())
            .spawn(move || {
                let mut command = Command::new(&program);
                command.arg("--version").stdin(Stdio::null());
                crate::platform::no_console(&mut command);
                let version = command
                    .output()
                    .ok()
                    .filter(|output| output.status.success())
                    .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
                if let Some(version) = version {
                    let text = format!("yt-dlp {version} at {}", program.display());
                    let _ = weak.upgrade_in_event_loop(move |window| {
                        if described.load(Ordering::Relaxed) == serial {
                            window.set_program(text.into());
                        }
                    });
                }
            });
        if let Err(error) = spawned {
            log::warn!("could not read yt-dlp's version: {error}");
        }
    }

    fn refresh(&self) {
        let rows: Vec<DownloadRow> = self.tasks.borrow().iter().map(row).collect();
        self.rows.set_vec(rows);
        self.refresh_buttons();
        self.window.set_folder(
            format!("Saving to {}", self.app().prefs().download_dir().display()).into(),
        );
    }

    fn refresh_task(&self, id: u64) {
        {
            let tasks = self.tasks.borrow();
            if let Some(index) = tasks.iter().position(|task| task.id == id) {
                self.rows.set_row_data(index, row(&tasks[index]));
            }
        }
        self.refresh_buttons();
    }

    /// The buttons under the list. Cancel All and Retry Failed are there
    /// only when they would do more than a row's own button does: for a
    /// whole show asked for at once, say, or a server's downloads failing
    /// after its sign-in expired.
    fn refresh_buttons(&self) {
        let tasks = self.tasks.borrow();
        self.window
            .set_has_finished(tasks.iter().any(Task::finished));
        self.window
            .set_can_cancel_all(tasks.iter().filter(|task| task.active()).count() > 1);
        let failed = tasks
            .iter()
            .filter(|task| task.finished() && matches!(task.state, State::Failed(_)))
            .count();
        self.window.set_can_retry_failed(failed > 1);
    }

    /// Starts downloading; false when yt-dlp could not be run, which the
    /// window shows. A video from the YouTube section waits its turn instead,
    /// unless it is on its way already, when nothing more happens.
    pub fn start(self: &Rc<Self>, request: Request) -> bool {
        if request.video.is_some() {
            self.add_section_video(request);
            return true;
        }
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        let mut task = Task {
            id,
            job: Job::Ytdl {
                request: request.clone(),
                child: None,
            },
            state: State::running(),
            file: None,
        };
        let started = self.spawn_ytdl(&request);
        let ok = match started {
            Ok(child) => {
                watch(id, child.clone());
                task.job = Job::Ytdl {
                    request,
                    child: Some(child),
                };
                true
            }
            Err(error) => {
                task.state = State::Failed(error);
                false
            }
        };
        self.tasks.borrow_mut().push(task);
        self.refresh();
        ok
    }

    fn add_section_video(&self, request: Request) {
        {
            let mut tasks = self.tasks.borrow_mut();
            let already = tasks
                .iter()
                .any(|task| task.active() && task.video() == request.video.as_ref());
            if already {
                return;
            }
            let id = self.next_id.get();
            self.next_id.set(id + 1);
            tasks.push(Task {
                id,
                job: Job::Ytdl {
                    request,
                    child: None,
                },
                state: State::Queued,
                file: None,
            });
        }
        self.refresh();
        self.start_queued();
    }

    /// Runs yt-dlp for a request, in the download folder.
    fn spawn_ytdl(&self, request: &Request) -> Result<Arc<Mutex<Child>>, String> {
        let app = self.app();
        let prefs = app.prefs();
        let folder = prefs.download_dir();
        let program = find_ytdl(&prefs, &app.mpv_config_dir()).ok_or(
            "yt-dlp was not found. Install it, or set where it is in Preferences › Network.",
        )?;
        std::fs::create_dir_all(&folder)
            .map_err(|error| format!("Could not make {}: {error}", folder.display()))?;
        let raw = if request.video.is_some() {
            prefs.ytdl_options_for_section()
        } else {
            prefs.ytdl_raw_options_with_proxy(&prefs.ytdl_raw_options)
        };
        let ffmpeg = Ffmpeg::for_options(&raw);
        let mut command = command(&program, request, &folder, &raw, &ffmpeg);
        // Not the whole command: its options can hold a proxy's password.
        log::debug!("downloading {} with {}", request.url, program.display());
        let child = command
            .spawn()
            .map_err(|error| format!("Could not run {}: {error}", program.display()))?;
        self.ytdl_started.set(Some(Instant::now()));
        Ok(Arc::new(Mutex::new(child)))
    }

    /// A line or the end from a download's thread.
    fn report(&self, id: u64, report: Report) {
        let ended = matches!(report, Report::Finished(_));
        let mut saved = None;
        let finished = {
            let mut tasks = self.tasks.borrow_mut();
            let Some(task) = tasks.iter_mut().find(|task| task.id == id) else {
                return;
            };
            match report {
                Report::Unauthorized => {
                    if let Job::Jellyfin {
                        request,
                        client: Some(client),
                        ..
                    } = &task.job
                    {
                        let (server, client) = (request.server_id.clone(), client.clone());
                        drop(tasks);
                        self.app().jellyfin().unauthorized(&server, &client);
                    }
                    return;
                }
                Report::Line(Line::Progress {
                    done,
                    total,
                    speed,
                    eta,
                }) => {
                    if let State::Running { .. } = task.state {
                        task.state = State::Running {
                            done: done.unwrap_or(0),
                            total,
                            speed,
                            eta,
                            merging: false,
                            stalled: false,
                        };
                    }
                    None
                }
                Report::Line(Line::Processing(step)) => {
                    if let State::Running { merging, .. } = &mut task.state {
                        *merging = step.starts_with("Merger") && !step.ends_with("finished");
                    }
                    None
                }
                Report::Line(Line::File(path)) => {
                    task.file = Some(path);
                    None
                }
                Report::Finished(result) => {
                    match &mut task.job {
                        Job::Ytdl { child, .. } => *child = None,
                        Job::Jellyfin {
                            stop,
                            meter,
                            thread,
                            client,
                            ..
                        } => {
                            *stop = None;
                            *meter = None;
                            *thread = None;
                            *client = None;
                        }
                    }
                    // Cancelled, or stopped by signing out, meanwhile.
                    if !matches!(task.state, State::Running { .. }) {
                        None
                    } else {
                        let video = task.video().cloned();
                        task.state = match (result, task.file.clone()) {
                            (Ok(()), Some(path)) => State::Done(path),
                            (Ok(()), None) => {
                                State::Failed("yt-dlp finished without saving a file".into())
                            }
                            (Err(error), _) if video.is_some() => {
                                State::Failed(ytdlp::failure_of(&error).said())
                            }
                            (Err(error), _) => State::Failed(error),
                        };
                        if let (Some(video), State::Done(path)) = (&video, &task.state) {
                            saved = Some((video.clone(), path.clone()));
                        }
                        Some((
                            task.title(),
                            task.state.clone(),
                            task.server().map(str::to_owned),
                        ))
                    }
                }
            }
        };
        self.refresh_task(id);
        if let Some((video, path)) = saved {
            self.app().youtube().set_download(&video, &path);
        }
        // Counted before the server's next downloads start, which may fail
        // as they do, and say what they all came to.
        if let Some((title, state, Some(server))) = &finished {
            self.tally(server, title, matches!(state, State::Done(_)));
        }
        if ended {
            // The server's next download, now that this one has ended.
            self.start_queued();
        }
        let Some((title, state, server)) = finished else {
            return;
        };
        match server {
            Some(server) => self.announce(&server),
            None => {
                let app = self.app();
                match state {
                    State::Done(_) => app.notify("Downloaded", &title),
                    State::Failed(_) => app.notify("Download failed", &title),
                    _ => {}
                }
            }
        }
    }

    /// Counts a Jellyfin download that ended towards what its server's
    /// downloads came to.
    fn tally(&self, server: &str, title: &str, saved: bool) {
        let mut ended = self.ended.borrow_mut();
        let tally = ended.entry(server.to_owned()).or_default();
        if saved {
            tally.saved += 1;
        } else {
            tally.failed += 1;
        }
        title.clone_into(&mut tally.title);
    }

    /// Says what a server's downloads came to once none of them is left
    /// running or waiting.
    fn announce(&self, server: &str) {
        let busy = self
            .tasks
            .borrow()
            .iter()
            .any(|task| task.active() && task.server() == Some(server));
        if busy {
            return;
        }
        let tally = self.ended.borrow_mut().remove(server);
        if let Some((text, detail)) = tally.as_ref().and_then(Tally::message) {
            self.app().notify(&text, &detail);
        }
    }

    /// Cancels the download in row `index`. A Jellyfin download's part
    /// file goes at once, but its server's next download waits for its
    /// thread to end, a moment later unless the server has stopped sending.
    /// One that has saved its file already is left to say so.
    fn cancel(&self, index: usize) {
        let (id, server) = {
            let mut tasks = self.tasks.borrow_mut();
            let Some(task) = tasks.get_mut(index) else {
                return;
            };
            if !task.active() || !task.stop() {
                return;
            }
            task.state = State::Cancelled;
            (task.id, task.server().map(str::to_owned))
        };
        self.refresh_task(id);
        if let Some(server) = server {
            self.announce(&server);
        }
    }

    /// Cancels every download running or waiting, but those that have
    /// saved their files already.
    fn cancel_all(&self) {
        let mut servers: Vec<String> = Vec::new();
        for task in self.tasks.borrow_mut().iter_mut() {
            if !task.active() || !task.stop() {
                continue;
            }
            task.state = State::Cancelled;
            if let Some(server) = task.server()
                && !servers.iter().any(|known| known == server)
            {
                servers.push(server.to_owned());
            }
        }
        self.refresh();
        for server in servers {
            self.announce(&server);
        }
    }

    fn remove(&self, index: usize) {
        let removable = self.tasks.borrow().get(index).is_some_and(Task::finished);
        if removable {
            self.tasks.borrow_mut().remove(index);
            self.refresh();
        }
    }

    /// Stops every download as Patina quits. yt-dlp picks up its partial
    /// files the next time; a Jellyfin download's part file goes at once,
    /// and its thread, like those of downloads stopped before that haven't
    /// ended yet, is given a moment to end.
    pub fn stop_all(&self) {
        self.section_timer.stop();
        let mut threads = Vec::new();
        for task in self.tasks.borrow_mut().iter_mut() {
            if task.active() {
                // As Patina quits, a file saved meanwhile needs no row.
                task.stop();
                task.state = State::Cancelled;
            }
            match &mut task.job {
                Job::Ytdl { child, .. } => *child = None,
                Job::Jellyfin { thread, .. } => threads.extend(thread.take()),
            }
        }
        let deadline = Instant::now() + STOP_WAIT;
        while threads.iter().any(|thread| !thread.is_finished()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Adds Jellyfin downloads, each to wait its turn from its server, and
    /// gives which of `requests` were added, by their place: one already
    /// waiting or running, or asked for twice, is not added again.
    pub fn start_jellyfin(self: &Rc<Self>, requests: Vec<jellyfin::Request>) -> Vec<usize> {
        let mut added = Vec::new();
        {
            let mut tasks = self.tasks.borrow_mut();
            for (at, request) in requests.into_iter().enumerate() {
                let already = tasks.iter().any(|task| {
                    task.active()
                        && matches!(&task.job, Job::Jellyfin { request: other, .. }
                            if other.server_id == request.server_id
                                && other.item_id == request.item_id)
                });
                if already {
                    continue;
                }
                let id = self.next_id.get();
                self.next_id.set(id + 1);
                tasks.push(Task {
                    id,
                    job: Job::Jellyfin {
                        request,
                        stop: None,
                        meter: None,
                        speed: Speed::default(),
                        thread: None,
                        client: None,
                    },
                    state: State::Queued,
                    file: None,
                });
                added.push(at);
            }
        }
        self.refresh();
        self.start_queued();
        self.follow();
        added
    }

    /// Reads the meters of the Jellyfin downloads while any runs or waits.
    fn follow(self: &Rc<Self>) {
        if self.ticker.running() {
            return;
        }
        let weak = Rc::downgrade(self);
        self.ticker
            .start(slint::TimerMode::Repeated, TICK, move || {
                if let Some(this) = weak.upgrade() {
                    this.tick();
                }
            });
    }

    /// Shows how far each running Jellyfin download has come, from its
    /// meter, and that it waits for its server once nothing has come for a
    /// while, its answer included; stops the ticker once none runs or
    /// waits.
    fn tick(&self) {
        let now = Instant::now();
        let mut busy = false;
        let mut changed = Vec::new();
        let mut tasks = self.tasks.borrow_mut();
        for (index, task) in tasks.iter_mut().enumerate() {
            let Job::Jellyfin { meter, speed, .. } = &mut task.job else {
                continue;
            };
            busy |= matches!(task.state, State::Queued | State::Running { .. });
            let (State::Running { .. }, Some(meter)) = (&task.state, meter) else {
                continue;
            };
            let (progress, moved) = meter.read();
            let state = metered(progress, moved, now, speed);
            if state != task.state {
                task.state = state;
                changed.push(index);
            }
        }
        for index in changed {
            self.rows.set_row_data(index, row(&tasks[index]));
        }
        if !busy {
            self.ticker.stop();
        }
    }

    /// Starts the first waiting download of each server that has none
    /// running or stopping, and the section's videos as their limit allows.
    fn start_queued(&self) {
        loop {
            let spaced = spaced_out(self.ytdl_started.get(), Instant::now(), SECTION_GAP);
            let next = next_queued(&self.tasks.borrow(), spaced);
            let Some(id) = next else {
                self.wait_for_section_gap();
                return;
            };
            self.launch(id);
            let waiting = self
                .tasks
                .borrow()
                .iter()
                .any(|task| task.id == id && task.state == State::Queued);
            if waiting {
                // `launch` always moves it on; were it ever not to, this
                // would go round for ever.
                return;
            }
        }
    }

    /// A video from the section that only waits for the time between them
    /// starts when that is over. One waiting for another to end starts then.
    fn wait_for_section_gap(&self) {
        let tasks = self.tasks.borrow();
        let free = tasks.iter().filter(|task| task.ytdl_running()).count() < SECTION_LIMIT;
        let waiting = tasks
            .iter()
            .any(|task| task.state == State::Queued && task.server().is_none());
        let (true, true, Some(started)) = (free, waiting, self.ytdl_started.get()) else {
            return;
        };
        let left = SECTION_GAP.saturating_sub(started.elapsed());
        self.section_timer
            .start(slint::TimerMode::SingleShot, left, || {
                App::with_current(|app| {
                    if let Some(downloads) = app.existing_downloads() {
                        downloads.start_queued();
                    }
                });
            });
    }

    /// Starts a waiting Jellyfin download on a thread of its own, or fails
    /// it at once when its server has no sign-in. One that fails counts
    /// towards its server's message as one that fails later does.
    fn launch(&self, id: u64) {
        if self.launch_ytdl(id) {
            return;
        }
        let app = self.app();
        let session = app.jellyfin();
        let folder = app.prefs().download_dir();
        let mut tasks = self.tasks.borrow_mut();
        let Some(task) = tasks.iter_mut().find(|task| task.id == id) else {
            return;
        };
        let title = task.title();
        let Job::Jellyfin {
            request,
            stop,
            meter,
            thread,
            client,
            ..
        } = &mut task.job
        else {
            return;
        };
        let server = request.server_id.clone();
        let failed = match session.account(&server) {
            None => {
                let name = session
                    .server(&server)
                    .map(|server| server.name)
                    .unwrap_or_else(|| "the server".to_owned());
                Some(format!("Sign in to {name} again to download this."))
            }
            Some(account) => {
                let stopper = Arc::new(Stop::default());
                let metering = Arc::new(Meter::started());
                let spawned = spawn_jellyfin(
                    id,
                    account.client.clone(),
                    account.server.name,
                    request.clone(),
                    folder,
                    stopper.clone(),
                    metering.clone(),
                );
                match spawned {
                    Ok(handle) => {
                        *stop = Some(stopper);
                        *meter = Some(metering);
                        *thread = Some(handle);
                        *client = Some(Box::new(account.client));
                        None
                    }
                    Err(error) => Some(format!("Could not start the download: {error}")),
                }
            }
        };
        task.state = match &failed {
            Some(reason) => State::Failed(reason.clone()),
            None => State::running(),
        };
        drop(tasks);
        self.refresh_task(id);
        if failed.is_some() {
            self.tally(&server, &title, false);
            self.announce(&server);
        }
    }
}

impl Downloads {
    /// Starts a waiting yt-dlp download; false for one that isn't that. One that
    /// can't start says why in its row.
    fn launch_ytdl(&self, id: u64) -> bool {
        let request = {
            let tasks = self.tasks.borrow();
            match tasks
                .iter()
                .find(|task| task.id == id)
                .map(|task| &task.job)
            {
                Some(Job::Ytdl { request, .. }) => request.clone(),
                _ => return false,
            }
        };
        let started = self.spawn_ytdl(&request);
        {
            let mut tasks = self.tasks.borrow_mut();
            let Some(task) = tasks.iter_mut().find(|task| task.id == id) else {
                return true;
            };
            match started {
                Ok(child) => {
                    watch(id, child.clone());
                    task.job = Job::Ytdl {
                        request,
                        child: Some(child),
                    };
                    task.state = State::running();
                }
                Err(error) => task.state = State::Failed(error),
            }
        }
        self.refresh_task(id);
        true
    }
}

/// Whether enough time has passed since yt-dlp last started.
fn spaced_out(started: Option<Instant>, now: Instant, gap: Duration) -> bool {
    started.is_none_or(|started| now.saturating_duration_since(started) >= gap)
}

/// The waiting download to start next: a Jellyfin one whose server has none
/// running or stopping, or a video from the section if the limit and the time
/// between them allow. The two don't hold each other up.
fn next_queued(tasks: &[Task], spaced: bool) -> Option<u64> {
    let room = tasks.iter().filter(|task| task.ytdl_running()).count() < SECTION_LIMIT;
    tasks
        .iter()
        .filter(|task| task.state == State::Queued)
        .find(|queued| match queued.server() {
            Some(server) => !tasks
                .iter()
                .any(|task| task.downloading() && task.server() == Some(server)),
            None => spaced && room,
        })
        .map(|task| task.id)
}

impl Listener for Downloads {
    /// Signing out of a server stops its downloads, as it stops what plays
    /// from it: the user is done with the account. They can be retried
    /// once it is signed in again.
    fn session_changed(&self, change: &Change) {
        let Change::SigningOut(server_id) = change else {
            return;
        };
        let name = self
            .app()
            .jellyfin()
            .server(server_id)
            .map(|server| server.name)
            .unwrap_or_default();
        self.ended.borrow_mut().remove(server_id);
        let mut stopped = Vec::new();
        for task in self.tasks.borrow_mut().iter_mut() {
            if task.active() && task.server() == Some(server_id.as_str()) && task.stop() {
                task.state = State::Failed(format!("Signed out of {name}."));
                stopped.push(task.id);
            }
        }
        for id in stopped {
            self.refresh_task(id);
        }
    }
}

/// A Jellyfin download's thread: the file, written as `jellyfin::run`
/// does, its progress noted on `meter` for the window to read, and its
/// end reported to the window in the event loop, as yt-dlp's is. Nothing
/// it reports holds the token: errors are the client's sentences, which
/// never name it.
fn spawn_jellyfin(
    id: u64,
    client: Client,
    server_name: String,
    request: jellyfin::Request,
    folder: PathBuf,
    stop: Arc<Stop>,
    meter: Arc<Meter>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("jellyfin download".into())
        .spawn(move || {
            let result = jellyfin::run(
                &client,
                &request.item_id,
                &request.fallback,
                &folder,
                &stop,
                |progress| meter.note(progress),
            );
            let finished = match result {
                Ok(path) => {
                    log::info!("downloaded {}", path.display());
                    send(id, Report::Line(Line::File(path)));
                    Ok(())
                }
                Err(Failure::Cancelled) => Err("Cancelled".to_owned()),
                Err(Failure::Server(error)) => {
                    log::warn!("could not download {}: {error}", request.title);
                    if error == JellyfinError::Unauthorized {
                        send(id, Report::Unauthorized);
                    }
                    Err(error.about(Some(&server_name)))
                }
                Err(Failure::Disk(error)) => {
                    log::warn!("could not download {}: {error}", request.title);
                    Err(error)
                }
            };
            send(id, Report::Finished(finished));
        })
}

/// Reports to the downloads in the event loop, from a download's thread.
fn send(id: u64, report: Report) {
    let _ = slint::invoke_from_event_loop(move || {
        App::with_current(|app| {
            if let Some(downloads) = app.existing_downloads() {
                downloads.report(id, report);
            }
        });
    });
}

/// Reads a download's output on threads of their own, reporting to the
/// window in the event loop: progress at most a few times a second.
fn watch(id: u64, child: Arc<Mutex<Child>>) {
    let (stdout, stderr) = match child.lock() {
        Ok(mut child) => (child.stdout.take(), child.stderr.take()),
        Err(_) => return,
    };
    let send = move |report: Report| send(id, report);
    // The last error yt-dlp wrote, for a failure; read apart from stdout so
    // neither pipe fills while the other is read.
    let errors = stderr.and_then(|stderr| {
        std::thread::Builder::new()
            .name("download errors".into())
            .spawn(move || {
                let mut last = String::new();
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let line = line.trim();
                    if let Some(error) = line.strip_prefix("ERROR:") {
                        last = error.trim().to_owned();
                    } else if last.is_empty() && !line.is_empty() && !line.starts_with("WARNING:") {
                        last = line.to_owned();
                    }
                }
                last
            })
            .ok()
    });
    let spawned = std::thread::Builder::new()
        .name("download".into())
        .spawn(move || {
            let mut last_progress: Option<Instant> = None;
            if let Some(stdout) = stdout {
                for line in BufReader::new(stdout).split(b'\n').map_while(Result::ok) {
                    let Some(line) = parse_line(&String::from_utf8_lossy(&line)) else {
                        continue;
                    };
                    if let Line::Progress { .. } = line {
                        if last_progress.is_some_and(|at| at.elapsed() < Duration::from_millis(250))
                        {
                            continue;
                        }
                        last_progress = Some(Instant::now());
                    }
                    send(Report::Line(line));
                }
            }
            let status = child.lock().ok().and_then(|mut child| child.wait().ok());
            let error = errors
                .and_then(|errors| errors.join().ok())
                .unwrap_or_default();
            send(Report::Finished(match status {
                Some(status) if status.success() => Ok(()),
                _ if !error.is_empty() => Err(error),
                Some(status) => Err(format!("yt-dlp stopped ({status})")),
                None => Err("yt-dlp stopped".into()),
            }));
        });
    if let Err(error) = spawned {
        log::warn!("could not follow a download: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_lines_are_read_with_missing_values() {
        assert_eq!(
            parse_line("patina-progress 1048576 4194304 NA 524288.5 6\n"),
            Some(Line::Progress {
                done: Some(1_048_576),
                total: Some(4_194_304),
                speed: Some(524_288.5),
                eta: Some(6),
            })
        );
        // Fragmented downloads only have an estimate of the total.
        assert_eq!(
            parse_line("patina-progress 2000 NA 9000.0 NA NA"),
            Some(Line::Progress {
                done: Some(2000),
                total: Some(9000),
                speed: None,
                eta: None,
            })
        );
        assert_eq!(
            parse_line("patina-process Merger started"),
            Some(Line::Processing("Merger started".into()))
        );
        assert_eq!(
            parse_line("patina-file /videos/Big Buck Bunny [aqz-KE-bpKQ].webm\r"),
            Some(Line::File(
                "/videos/Big Buck Bunny [aqz-KE-bpKQ].webm".into()
            ))
        );
        assert_eq!(parse_line("[download] Destination: x.webm"), None);
    }

    #[test]
    fn raw_options_become_arguments_as_mpv_passes_them() {
        assert_eq!(
            raw_options("cookies-from-browser=firefox, write-auto-subs=,proxy="),
            [
                "--cookies-from-browser",
                "firefox",
                "--write-auto-subs",
                "--proxy",
                ""
            ]
        );
        assert!(raw_options("").is_empty());
        // A flag with no `=`, which mpv can't read, and spaces around things.
        assert_eq!(
            raw_options("no-mtime,format-sort=res , write-subs"),
            ["--no-mtime", "--format-sort", "res", "--write-subs"]
        );
        // Quoted, as mpv needs a value with a comma to be.
        assert_eq!(
            raw_options("extractor-args=\"youtube:player_client=tv,web\",proxy=[http://a:1]"),
            [
                "--extractor-args",
                "youtube:player_client=tv,web",
                "--proxy",
                "http://a:1"
            ]
        );
    }

    /// yt-dlp's arguments for downloading with these raw options.
    fn download_args(raw: &str) -> Vec<String> {
        let request = Request {
            url: "https://example.com/v".into(),
            title: String::new(),
            format: Some("18".into()),
            video: None,
        };
        command(
            Path::new("yt-dlp"),
            &request,
            Path::new("/videos"),
            raw,
            &Ffmpeg::OnPath,
        )
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
    }

    /// A download goes through the proxy as everything else does.
    #[test]
    fn downloads_go_through_the_proxy() {
        let prefs = Preferences {
            http_proxy: "http://user:p,w@proxy.example:8080".into(),
            ..Default::default()
        };
        let args = download_args(&prefs.ytdl_raw_options_with_proxy(""));
        let at = args.iter().position(|arg| arg == "--proxy").unwrap();
        assert_eq!(args[at + 1], "http://user:p,w@proxy.example:8080");

        let own = prefs.ytdl_raw_options_with_proxy("proxy=http://own:1,no-mtime=");
        let args = download_args(&own);
        let at = args.iter().position(|arg| arg == "--proxy").unwrap();
        assert_eq!(args[at + 1], "http://own:1");
        assert_eq!(args.iter().filter(|arg| *arg == "--proxy").count(), 1);

        let none = Preferences::default().ytdl_raw_options_with_proxy("");
        assert!(!download_args(&none).iter().any(|arg| arg == "--proxy"));
    }

    /// The value after each `--extractor-args`.
    fn extractor_args(args: &[String]) -> Vec<&str> {
        args.windows(2)
            .filter(|pair| pair[0] == "--extractor-args")
            .map(|pair| pair[1].as_str())
            .collect()
    }

    #[test]
    fn downloads_ask_youtube_for_the_same_clients_as_mpv() {
        let args = download_args("");
        assert_eq!(
            extractor_args(&args),
            ["youtube:player_client=visionos,default"]
        );
        // The clients are one argument, whatever else there is, and the
        // address stays last, behind the end of the options.
        let args = download_args("no-mtime=");
        assert_eq!(
            extractor_args(&args),
            ["youtube:player_client=visionos,default"]
        );
        assert!(args.iter().any(|arg| arg == "--no-mtime"));
        assert_eq!(args[args.len() - 2..], ["--", "https://example.com/v"]);
    }

    /// A folder of its own for a test, removed afterwards.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("patina-downloads-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        /// An empty file standing in for a program, as `find_ytdl` only asks
        /// whether there is one.
        fn program(&self, name: &str) -> PathBuf {
            let name = if cfg!(windows) {
                format!("{name}.exe")
            } else {
                name.to_owned()
            };
            let path = self.0.join(name);
            std::fs::write(&path, b"").unwrap();
            path
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// `find_ytdl_in` with the `PATH` standing in for the user's installed
    /// copies too, and none of them ever compared with the package's: the
    /// user's choice, and a search with nothing bundled, come before that.
    fn find_mine(
        prefs: &Preferences,
        config: &Path,
        bundled: Option<PathBuf>,
        on_path: impl Fn(&str) -> Option<PathBuf> + Copy,
    ) -> Option<PathBuf> {
        find_ytdl_in(prefs, config, bundled, on_path, on_path, |bundled, own| {
            assert_eq!(own, None, "compared with the package's");
            bundled
        })
    }

    /// The user's own yt-dlp first (the preference, the hook's configuration,
    /// then mpv's folder), then the newer of the package's and one installed,
    /// then the `PATH`; with nothing bundled, mpv's hook's search exactly, as
    /// before.
    #[test]
    fn yt_dlp_is_looked_for_in_the_users_places_then_the_package_then_the_path() {
        let config = Folder::new("ytdl-config");
        let bundled =
            PathBuf::from("/Patina.app/Contents/Helpers/yt-dlp.app/Contents/MacOS/yt-dlp");
        let on_path = |name: &str| Some(PathBuf::from(format!("/usr/bin/{name}")));
        let nowhere = |_: &str| None;
        let none = Preferences::default();
        // The package's and one installed, compared by `newer`, which says
        // which it was given.
        let compared = |installed: fn(&str) -> Option<PathBuf>, own_newer: bool| {
            let asked = std::cell::RefCell::new(None);
            let found = find_ytdl_in(
                &none,
                &config.0,
                Some(bundled.clone()),
                nowhere,
                installed,
                |bundled, own| match own {
                    Some(own) => {
                        *asked.borrow_mut() = Some((bundled.clone(), own.clone()));
                        if own_newer { own } else { bundled }
                    }
                    None => bundled,
                },
            );
            (found, asked.into_inner())
        };

        // Only the PATH has one.
        assert_eq!(
            find_mine(&none, &config.0, None, on_path),
            Some(PathBuf::from("/usr/bin/yt-dlp"))
        );
        assert_eq!(find_mine(&none, &config.0, None, nowhere), None);
        // The package's alone, without asking anything.
        assert_eq!(
            find_mine(&none, &config.0, Some(bundled.clone()), nowhere),
            Some(bundled.clone())
        );
        // One installed as well: the newer of the two.
        let installed = |name: &str| Some(PathBuf::from(format!("/opt/homebrew/bin/{name}")));
        let homebrew = PathBuf::from("/opt/homebrew/bin/yt-dlp");
        let pair = Some((bundled.clone(), homebrew.clone()));
        assert_eq!(
            compared(installed, true),
            (Some(homebrew.clone()), pair.clone())
        );
        assert_eq!(compared(installed, false), (Some(bundled.clone()), pair));
        // yt-dlp_x86 counts, youtube-dl never does, and the package's own
        // copy found on the PATH is no second copy.
        let x86_only =
            |name: &str| (name == "yt-dlp_x86").then(|| PathBuf::from("/usr/bin/yt-dlp_x86"));
        assert_eq!(
            compared(x86_only, true).1,
            Some((bundled.clone(), PathBuf::from("/usr/bin/yt-dlp_x86")))
        );
        let youtube_dl_only =
            |name: &str| (name == "youtube-dl").then(|| PathBuf::from("/usr/bin/youtube-dl"));
        assert_eq!(
            compared(youtube_dl_only, true),
            (Some(bundled.clone()), None)
        );
        let itself = |_: &str| {
            Some(PathBuf::from(
                "/Patina.app/Contents/Helpers/yt-dlp.app/Contents/MacOS/yt-dlp",
            ))
        };
        assert_eq!(compared(itself, true), (Some(bundled.clone()), None));

        // youtube-dl in mpv's folder can't stand in for the package's yt-dlp,
        // as it can't play YouTube. With nothing bundled, the hook's own order
        // holds: yt-dlp on the PATH comes before youtube-dl anywhere, and
        // youtube-dl in mpv's folder before nothing.
        let youtube_dl = config.program("youtube-dl");
        assert_eq!(
            find_mine(&none, &config.0, Some(bundled.clone()), nowhere),
            Some(bundled.clone())
        );
        assert_eq!(
            find_mine(&none, &config.0, None, on_path),
            Some(PathBuf::from("/usr/bin/yt-dlp"))
        );
        assert_eq!(find_mine(&none, &config.0, None, nowhere), Some(youtube_dl));
        // yt-dlp_x86 there is yt-dlp, and the user's.
        let x86 = config.program("yt-dlp_x86");
        assert_eq!(
            find_mine(&none, &config.0, Some(bundled.clone()), on_path),
            Some(x86)
        );
        let in_config = config.program("yt-dlp");
        assert_eq!(
            find_mine(&none, &config.0, Some(bundled.clone()), on_path),
            Some(in_config.clone())
        );

        // The preference beats everything, and is never swapped for the
        // package's copy, even when it names nothing that exists.
        let own = Preferences {
            ytdl_path: "/opt/yt-dlp/yt-dlp".into(),
            ..Default::default()
        };
        let exists = |name: &str| (name == "/opt/yt-dlp/yt-dlp").then(|| PathBuf::from(name));
        assert_eq!(
            find_mine(&own, &config.0, Some(bundled.clone()), exists),
            Some(PathBuf::from("/opt/yt-dlp/yt-dlp"))
        );
        assert_eq!(
            find_mine(&own, &config.0, Some(bundled.clone()), nowhere),
            None
        );
        // A bare name in it is looked for in mpv's folder, then on the PATH.
        let named = Preferences {
            ytdl_path: "yt-dlp".into(),
            ..Default::default()
        };
        assert_eq!(
            find_mine(&named, &config.0, Some(bundled.clone()), on_path),
            Some(in_config.clone())
        );

        // A ytdl_path in the hook's own script-opts/ytdl_hook.conf, which mpv
        // reads when it is given mpv's folder, is the user's as well: it comes
        // before mpv's folder and the package's, after the preference.
        let mpv_config = Preferences {
            use_mpv_config: true,
            ..Default::default()
        };
        let no_mpv_config = Preferences {
            use_mpv_config: false,
            ..Default::default()
        };
        let opts = config.0.join("script-opts");
        std::fs::create_dir_all(&opts).unwrap();
        std::fs::write(
            opts.join("ytdl_hook.conf"),
            "# mine\r\nytdl_path=/old/yt-dlp\r\n  \r\nytdl_path=/opt/yt-dlp/yt-dlp\r\n",
        )
        .unwrap();
        assert_eq!(
            find_mine(&mpv_config, &config.0, Some(bundled.clone()), exists),
            Some(PathBuf::from("/opt/yt-dlp/yt-dlp"))
        );
        assert_eq!(
            hook_ytdl_path(&mpv_config, &config.0).as_deref(),
            Some("/opt/yt-dlp/yt-dlp")
        );
        // Unread when mpv isn't given the folder, as mpv doesn't read it then.
        assert_eq!(hook_ytdl_path(&no_mpv_config, &config.0), None);
        assert_eq!(
            find_mine(&no_mpv_config, &config.0, Some(bundled.clone()), exists),
            Some(in_config.clone())
        );
        // The preference still wins over it.
        let both = Preferences {
            use_mpv_config: true,
            ytdl_path: "yt-dlp".into(),
            ..Default::default()
        };
        assert_eq!(
            find_mine(&both, &config.0, Some(bundled.clone()), exists),
            Some(in_config.clone())
        );
        // An empty one, last, names nothing, and the search goes on.
        std::fs::write(
            opts.join("ytdl_hook.conf"),
            "ytdl_path=/opt/yt-dlp/yt-dlp\nytdl_path=\n",
        )
        .unwrap();
        assert_eq!(hook_ytdl_path(&mpv_config, &config.0), None);
        assert_eq!(
            find_mine(&mpv_config, &config.0, Some(bundled), exists),
            Some(in_config)
        );
    }

    /// With stand-ins that say their versions: the package's copy until the
    /// one installed has answered, then that one, being newer; the user's
    /// choice all along when there is one.
    #[cfg(unix)]
    #[test]
    fn a_newer_installed_yt_dlp_is_used_once_it_has_answered() {
        use crate::youtube::ytdlp::testing::Fake;
        let folder = Folder::new("newer-installed");
        let config = Folder::new("newer-installed-config");
        let bundled = Fake::new(&folder.0.join("bundled"), "echo 2026.10.26");
        let own = Fake::new(&folder.0.join("bin"), "echo 2026.11.03.101010");
        let versions = crate::ytdl_versions::Versions::for_tests(Duration::from_secs(10));
        let installed = |name: &str| (name == "yt-dlp").then(|| own.program.clone());
        let find = |prefs: &Preferences| {
            find_ytdl_in(
                prefs,
                &config.0,
                Some(bundled.program.clone()),
                |_: &str| None,
                installed,
                |bundled, own| versions.newer(bundled, own),
            )
        };
        let none = Preferences::default();
        assert_eq!(find(&none), Some(bundled.program.clone()));
        assert!(versions.wait_idle(Duration::from_secs(20)));
        assert_eq!(find(&none), Some(own.program.clone()));
        let chosen = Preferences {
            ytdl_path: bundled.program.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let exists = |name: &str| Some(PathBuf::from(name)).filter(|path| path.is_file());
        assert_eq!(
            find_ytdl_in(
                &chosen,
                &config.0,
                Some(bundled.program.clone()),
                exists,
                installed,
                |_, _| panic!("asked"),
            ),
            Some(bundled.program.clone())
        );
    }

    /// The user's `ffmpeg-location` first, then the package's ffmpeg, then the
    /// `PATH`; only the package's is passed to yt-dlp, which finds the others
    /// by itself.
    #[test]
    fn ffmpeg_is_the_users_then_the_packages_then_on_the_path() {
        let bundled = PathBuf::from(r"C:\Patina\ffmpeg.exe");
        let on_path = |_: &str| Some(PathBuf::from("/usr/bin/ffmpeg"));
        let nowhere = |_: &str| None;
        assert_eq!(
            Ffmpeg::choose("", Some(bundled.clone()), on_path),
            Ffmpeg::Bundled(bundled.clone())
        );
        assert_eq!(Ffmpeg::choose("", None, on_path), Ffmpeg::OnPath);
        assert_eq!(Ffmpeg::choose("no-mtime=", None, nowhere), Ffmpeg::Missing);
        assert_eq!(
            Ffmpeg::choose(
                "ffmpeg-location=/opt/ffmpeg",
                Some(bundled.clone()),
                nowhere
            ),
            Ffmpeg::Theirs
        );
        // An empty one names nothing.
        assert_eq!(
            Ffmpeg::choose("ffmpeg-location=", Some(bundled.clone()), nowhere),
            Ffmpeg::Bundled(bundled.clone())
        );

        let request = Request {
            url: "https://example.com/v".into(),
            title: String::new(),
            format: Some("137+140".into()),
            video: None,
        };
        let args_with = |ffmpeg: &Ffmpeg| -> Vec<String> {
            command(
                Path::new("yt-dlp"),
                &request,
                Path::new("/videos"),
                "",
                ffmpeg,
            )
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
        };
        let args = args_with(&Ffmpeg::Bundled(bundled.clone()));
        let at = args
            .iter()
            .position(|arg| arg == "--ffmpeg-location")
            .unwrap();
        assert_eq!(args[at + 1], bundled.to_string_lossy());
        assert!(at < args.iter().position(|arg| arg == "--").unwrap());
        for other in [Ffmpeg::OnPath, Ffmpeg::Theirs, Ffmpeg::Missing] {
            assert!(
                !args_with(&other)
                    .iter()
                    .any(|arg| arg == "--ffmpeg-location"),
                "{other:?}"
            );
        }

        // Only the package's can't record a live stream.
        assert!(!Ffmpeg::Bundled(bundled.clone()).records_live());
        for other in [Ffmpeg::OnPath, Ffmpeg::Theirs, Ffmpeg::Missing] {
            assert!(other.records_live(), "{other:?}");
        }

        // Joining video and audio waits on there being an ffmpeg.
        let joined = Some("137+140");
        assert_eq!(download_format(joined, &Ffmpeg::Missing), None);
        for found in [Ffmpeg::Bundled(bundled), Ffmpeg::OnPath, Ffmpeg::Theirs] {
            assert_eq!(download_format(joined, &found).as_deref(), joined);
        }
        assert_eq!(
            download_format(Some("18"), &Ffmpeg::Missing).as_deref(),
            Some("18")
        );
    }

    /// Starts `script` under sh in a group of its own, as a download's yt-dlp
    /// is started, with the path of a file in `folder` as `$0`; gives it and
    /// the id it writes there, that of what it started.
    #[cfg(unix)]
    fn start_group(folder: &Folder, script: &str) -> (Arc<Mutex<Child>>, u32) {
        let pid_file = folder.0.join("pid");
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(script)
            .arg(&pid_file)
            .stdout(Stdio::null());
        crate::platform::in_own_group(&mut command);
        let child = Arc::new(Mutex::new(command.spawn().unwrap()));
        let started = Instant::now();
        loop {
            if let Some(pid) = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|text| text.trim().parse::<u32>().ok())
            {
                return (child, pid);
            }
            assert!(started.elapsed() < Duration::from_secs(10), "no worker");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Whether `pid` is gone, or a zombie waiting for whoever inherited it,
    /// within `limit`.
    #[cfg(unix)]
    fn gone_within(pid: u32, limit: Duration) -> bool {
        let until = Instant::now() + limit;
        loop {
            let stat = Command::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .unwrap()
                .stdout;
            if stat.is_empty() || stat.starts_with(b"Z") {
                return true;
            }
            if Instant::now() >= until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Stopping a download stops what its yt-dlp started too, as ffmpeg, or a
    /// single-file yt-dlp's real worker, would go on otherwise; and it asks
    /// first, so that a launcher that catches the request (as a single-file
    /// yt-dlp's does, to remove what it unpacked) gets to tidy up.
    #[cfg(unix)]
    #[test]
    fn stopping_a_download_stops_what_yt_dlp_started() {
        let folder = Folder::new("stop");
        let unpacked = folder.0.join("unpacked");
        std::fs::write(&unpacked, b"").unwrap();
        let (child, worker) = start_group(
            &folder,
            "trap 'rm -f \"${0%/pid}/unpacked\"; exit 0' TERM; \
             sleep 30 & echo $! > \"$0\"; wait",
        );
        let started = Instant::now();
        stop_ytdl(&child);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "the stop waited"
        );
        let status = child.lock().unwrap().wait().unwrap();
        assert!(status.success(), "{status:?}: it was killed, not asked");
        assert!(!unpacked.exists(), "the launcher did not get to tidy up");
        assert!(
            gone_within(worker, Duration::from_secs(2)),
            "the worker {worker} is still running"
        );
    }

    /// One that won't end when asked is killed after the grace, worker and
    /// all, without the window waiting for it.
    #[cfg(unix)]
    #[test]
    fn a_download_that_will_not_stop_is_killed() {
        let folder = Folder::new("stop-ignored");
        // Ignored by the shell and so by what it starts.
        let (child, worker) =
            start_group(&folder, "trap '' TERM; sleep 30 & echo $! > \"$0\"; wait");
        let started = Instant::now();
        stop_ytdl(&child);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "the stop waited"
        );
        assert!(gone_within(
            worker,
            crate::platform::END_GRACE + Duration::from_secs(3)
        ));
        let status = child.lock().unwrap().wait().unwrap();
        assert!(!status.success(), "{status:?}");
    }

    #[test]
    fn a_downloads_own_extractor_args_or_cookies_win() {
        let args = download_args("extractor-args=\"youtube:player_client=tv,web\"");
        assert_eq!(extractor_args(&args), ["youtube:player_client=tv,web"]);
        for raw in [
            "cookies=/home/me/cookies.txt",
            "cookies-from-browser=firefox",
            "no-mtime,cookies=/home/me/cookies.txt",
        ] {
            let args = download_args(raw);
            assert!(extractor_args(&args).is_empty(), "{raw}");
        }
    }

    /// The section's options are all a video from it goes with, cookies of the
    /// user's left out, with the clients and proxy still there.
    #[test]
    fn a_video_from_the_section_goes_signed_out() {
        let prefs = Preferences {
            ytdl_raw_options: "cookies-from-browser=firefox".into(),
            http_proxy: "http://proxy.example:8080".into(),
            ..Default::default()
        };
        let args = download_args(&prefs.ytdl_options_for_section());
        assert!(args.iter().any(|arg| arg == "--ignore-config"), "{args:?}");
        assert!(!args.iter().any(|arg| arg.contains("cookies")), "{args:?}");
        assert_eq!(
            extractor_args(&args),
            ["youtube:player_client=visionos,default"]
        );
        let at = args.iter().position(|arg| arg == "--proxy").unwrap();
        assert_eq!(args[at + 1], "http://proxy.example:8080");
        assert_eq!(args[args.len() - 2..], ["--", "https://example.com/v"]);
    }

    fn section_task(id: u64, state: State) -> Task {
        Task {
            id,
            job: Job::Ytdl {
                request: Request {
                    url: "https://www.youtube.com/watch?v=aaaaaaaaaaa".into(),
                    title: String::new(),
                    format: None,
                    video: VideoId::parse("aaaaaaaaaaa"),
                },
                child: None,
            },
            state,
            file: None,
        }
    }

    fn server_task(id: u64, server: &str, state: State) -> Task {
        let mut task = jellyfin_task(None);
        task.id = id;
        task.state = state;
        if let Job::Jellyfin { request, .. } = &mut task.job {
            request.server_id = server.to_owned();
        }
        task
    }

    #[test]
    fn at_most_two_of_the_sections_videos_run() {
        let running = State::running;
        let tasks = vec![section_task(1, State::Queued)];
        assert_eq!(next_queued(&tasks, true), Some(1));
        // Not before its time.
        assert_eq!(next_queued(&tasks, false), None);

        let mut tasks = vec![section_task(1, running()), section_task(2, State::Queued)];
        assert_eq!(next_queued(&tasks, true), Some(2));
        tasks.insert(1, section_task(3, running()));
        assert_eq!(next_queued(&tasks, true), None);
        tasks[0].state = State::Done("/a".into());
        assert_eq!(next_queued(&tasks, true), Some(2));
        tasks[1].state = State::Failed("no".into());
        assert_eq!(next_queued(&tasks, true), Some(2));
        tasks[2].state = State::Cancelled;
        assert_eq!(next_queued(&tasks, true), None);
    }

    /// One downloaded from the player, which starts at once, counts as well: it
    /// is the same address asking.
    #[test]
    fn a_video_from_the_player_counts_towards_the_limit() {
        let mut other = section_task(1, State::running());
        if let Job::Ytdl { request, .. } = &mut other.job {
            request.video = None;
        }
        let mut tasks = vec![other, section_task(2, State::running())];
        tasks.push(section_task(3, State::Queued));
        assert_eq!(next_queued(&tasks, true), None);
    }

    /// A server's downloads go as they did: one at a time from each, whatever
    /// the section is doing, and it whatever they are.
    #[test]
    fn the_section_and_jellyfin_servers_do_not_hold_each_other_up() {
        let running = State::running;
        let tasks = vec![
            section_task(1, running()),
            section_task(2, running()),
            section_task(3, State::Queued),
            server_task(4, "a", State::Queued),
        ];
        assert_eq!(next_queued(&tasks, false), Some(4));
        let tasks = vec![
            server_task(1, "a", running()),
            server_task(2, "a", State::Queued),
            server_task(3, "b", State::Queued),
            section_task(4, State::Queued),
        ];
        assert_eq!(next_queued(&tasks, false), Some(3));
        let tasks = vec![
            server_task(1, "a", running()),
            server_task(2, "a", State::Queued),
            section_task(4, State::Queued),
        ];
        assert_eq!(next_queued(&tasks, true), Some(4));
        assert_eq!(next_queued(&tasks, false), None);
    }

    #[test]
    fn the_sections_videos_start_apart() {
        let start = Instant::now();
        let gap = Duration::from_secs(8);
        assert!(spaced_out(None, start, gap));
        assert!(!spaced_out(Some(start), start, gap));
        assert!(!spaced_out(
            Some(start),
            start + Duration::from_secs(7),
            gap
        ));
        assert!(spaced_out(Some(start), start + gap, gap));
    }

    #[test]
    fn a_waiting_video_says_so_and_can_be_stopped() {
        let mut task = section_task(1, State::Queued);
        assert_eq!(row(&task).state, "queued");
        assert!(task.active());
        assert!(!task.ytdl_running());
        assert!(task.stop());
        assert_eq!(task.video().unwrap().as_str(), "aaaaaaaaaaa");
    }

    #[test]
    fn a_servers_downloads_say_once_what_they_came_to() {
        let tally = |saved, failed| Tally {
            saved,
            failed,
            title: "Night Ferry".into(),
        };
        assert_eq!(tally(0, 0).message(), None);
        assert_eq!(
            tally(1, 0).message(),
            Some(("Downloaded".into(), "Night Ferry".into()))
        );
        assert_eq!(
            tally(0, 1).message(),
            Some(("Download failed".into(), "Night Ferry".into()))
        );
        assert_eq!(
            tally(4, 0).message(),
            Some(("Downloaded 4 Files".into(), String::new()))
        );
        assert_eq!(
            tally(0, 2).message(),
            Some(("2 Files Failed to Download".into(), String::new()))
        );
        assert_eq!(
            tally(3, 1).message(),
            Some(("Downloaded 3 Files".into(), "1 failed".into()))
        );
    }

    #[test]
    fn sizes_and_progress_read_like_a_file_manager() {
        assert_eq!(size(999), "999 B");
        assert_eq!(size(1_500), "1.5 kB");
        assert_eq!(size(12_345_678), "12.3 MB");
        let task = Task {
            id: 1,
            job: Job::Ytdl {
                request: Request {
                    url: "https://example.com/v".into(),
                    title: String::new(),
                    format: None,
                    video: None,
                },
                child: None,
            },
            state: State::Running {
                done: 5_000_000,
                total: Some(20_000_000),
                speed: Some(2_000_000.0),
                eta: Some(75),
                merging: false,
                stalled: false,
            },
            file: None,
        };
        assert_eq!(detail(&task), "5.0 MB of 20.0 MB · 2.0 MB/s · 01:15 left");
        let row = row(&task);
        assert_eq!(row.title, "https://example.com/v");
        assert!((row.progress - 0.25).abs() < f32::EPSILON);
    }

    fn jellyfin_task(stop: Option<Arc<Stop>>) -> Task {
        Task {
            id: 2,
            job: Job::Jellyfin {
                request: jellyfin::Request {
                    server_id: "s".into(),
                    item_id: "i".into(),
                    title: "The Long Field".into(),
                    fallback: "The Long Field.mkv".into(),
                },
                stop,
                meter: None,
                speed: Speed::default(),
                thread: None,
                client: None,
            },
            state: State::running(),
            file: None,
        }
    }

    /// A Jellyfin download's row reads its meter: its speed and time left
    /// while bytes come, and once none has come for a while, that it waits
    /// for the server, with what came before that, rather than the last
    /// speed it had.
    #[test]
    fn a_stalled_download_says_it_waits_for_the_server() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let progress = |done| Progress {
            done,
            total: Some(717_900),
        };
        let mut speed = Speed::default();
        metered(progress(0), at(0), at(0), &mut speed);
        let mut task = jellyfin_task(None);
        task.state = metered(progress(200_000), at(2000), at(2000), &mut speed);
        assert_eq!(
            detail(&task),
            "200.0 kB of 717.9 kB · 100.0 kB/s · 00:06 left"
        );
        // The last bytes came at 2.5 s, and nothing since.
        task.state = metered(progress(500_000), at(2500), at(4000), &mut speed);
        assert!(detail(&task).ends_with("left"), "{}", detail(&task));
        // Past the speed's span, before it counts as stalled: no speed.
        task.state = metered(progress(500_000), at(2500), at(6000), &mut speed);
        assert_eq!(detail(&task), "500.0 kB of 717.9 kB");
        task.state = metered(
            progress(500_000),
            at(2500),
            at(2500) + STALLED_AFTER,
            &mut speed,
        );
        assert_eq!(
            detail(&task),
            "500.0 kB of 717.9 kB · Waiting for the server…"
        );
        // Bytes again.
        task.state = metered(progress(600_000), at(8000), at(8000), &mut speed);
        assert!(!detail(&task).contains("Waiting"), "{}", detail(&task));
        // Every byte has come, and the file goes to the disk.
        task.state = metered(progress(717_900), at(9000), at(20_000), &mut speed);
        assert!(!detail(&task).contains("Waiting"), "{}", detail(&task));
    }

    /// A server slow to answer is waited for as one slow to send is: once
    /// it has been as long, the row says so, rather than "Starting…" for as
    /// long as the server takes to answer, half a minute at most.
    #[test]
    fn a_download_the_server_has_not_answered_says_it_waits_for_the_server() {
        let meter = Meter::started();
        let (progress, started) = meter.read();
        assert_eq!(progress, Progress::default());
        let mut speed = Speed::default();
        let mut task = jellyfin_task(None);
        for waited in [0, 1000, 4900] {
            let now = started + Duration::from_millis(waited);
            task.state = metered(progress, started, now, &mut speed);
            // As it was as it started, so that its row isn't redrawn.
            assert_eq!(task.state, State::running());
        }
        assert_eq!(detail(&task), "Starting…");
        task.state = metered(progress, started, started + STALLED_AFTER, &mut speed);
        assert_eq!(detail(&task), "Waiting for the server…");
        // The answer comes, with the size, and then nothing for as long.
        meter.note(Progress {
            done: 0,
            total: Some(717_900),
        });
        let (progress, answered) = meter.read();
        assert!(answered >= started);
        task.state = metered(progress, answered, answered, &mut speed);
        assert_eq!(detail(&task), "0 B of 717.9 kB");
        task.state = metered(progress, answered, answered + STALLED_AFTER, &mut speed);
        assert_eq!(detail(&task), "0 B of 717.9 kB · Waiting for the server…");
    }

    /// A cancel that comes after the file was saved stops nothing: the
    /// download stays running until its thread says it is done.
    #[test]
    fn a_download_that_saved_its_file_is_not_cancelled() {
        let mut saved = jellyfin_task(Some(Arc::new(Stop::saved())));
        assert!(!saved.stop());
        let mut writing = jellyfin_task(Some(Arc::new(Stop::default())));
        assert!(writing.stop());
        let mut queued = jellyfin_task(None);
        assert!(queued.stop());
    }
}
