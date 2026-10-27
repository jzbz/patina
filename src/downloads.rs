//! Downloading online videos with yt-dlp, as IINA's Online Media plugin
//! does: each download runs yt-dlp in the background, and the Downloads
//! window lists them with their progress.
//!
//! yt-dlp is told to print its progress, the post-processing step it is
//! on, and the file it made in fixed templates of Patina's own, so nothing
//! depends on the wording of its usual output.

use crate::app::App;
use crate::settings::Preferences;
use crate::{DownloadRow, DownloadsWindow};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::cell::{Cell, RefCell};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PROGRESS: &str = "patina-progress";
const PROCESS: &str = "patina-process";
const FILE: &str = "patina-file";

/// A video to download: what is playing.
#[derive(Debug, Clone)]
pub struct Request {
    /// The page, which yt-dlp is given again.
    pub url: String,
    pub title: String,
    /// yt-dlp's format ids for what is playing; None leaves it to yt-dlp.
    pub format: Option<String>,
}

/// yt-dlp as mpv's yt-dlp hook looks for it: the configured path (or
/// paths), or else `yt-dlp`, `yt-dlp_x86` and `youtube-dl`, each first in
/// mpv's configuration directory and then on the `PATH`.
pub fn find_ytdl(prefs: &Preferences, mpv_config: &Path) -> Option<PathBuf> {
    let separator = if cfg!(windows) { ';' } else { ':' };
    let configured: Vec<&str> = prefs
        .ytdl_path
        .split(separator)
        .filter(|path| !path.trim().is_empty())
        .collect();
    let names = if configured.is_empty() {
        vec!["yt-dlp", "yt-dlp_x86", "youtube-dl"]
    } else {
        configured
    };
    names.into_iter().find_map(|name| {
        let exe = if cfg!(windows) && !name.to_ascii_lowercase().ends_with(".exe") {
            format!("{name}.exe")
        } else {
            name.to_owned()
        };
        let in_config = mpv_config.join(exe);
        if in_config.is_file() {
            Some(in_config)
        } else {
            crate::platform::find_program(name)
        }
    })
}

/// The format to download: what is playing, unless that takes merging
/// video and audio and there is no ffmpeg to do it, when yt-dlp picks the
/// best it can download as one file.
pub fn download_format(playing: Option<&str>) -> Option<String> {
    let playing = playing?;
    if playing.contains('+') && crate::platform::find_program("ffmpeg").is_none() {
        return None;
    }
    Some(playing.to_owned())
}

/// mpv's `ytdl-raw-options` ("key=value,flag=") as yt-dlp arguments, the
/// way mpv's yt-dlp hook passes them.
fn raw_options(raw: &str) -> Vec<String> {
    let mut args = Vec::new();
    for option in raw
        .split(',')
        .map(str::trim)
        .filter(|option| !option.is_empty())
    {
        let (key, value) = option.split_once('=').unwrap_or((option, ""));
        args.push(format!("--{}", key.trim()));
        if !value.is_empty() || key.trim() == "proxy" {
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

fn command(program: &Path, request: &Request, folder: &Path, raw: &str) -> Command {
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
    if let Some(format) = &request.format {
        command.args(["--format", format]);
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
    command
}

#[derive(Debug, Clone, PartialEq)]
enum State {
    Running {
        done: u64,
        total: Option<u64>,
        speed: Option<f64>,
        eta: Option<u64>,
        merging: bool,
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
        }
    }
}

/// What a download's thread reports to the window.
enum Report {
    Line(Line),
    Finished(Result<(), String>),
}

struct Task {
    id: u64,
    request: Request,
    state: State,
    child: Option<Arc<Mutex<Child>>>,
    file: Option<PathBuf>,
}

pub struct Downloads {
    app: Weak<App>,
    window: DownloadsWindow,
    rows: Rc<VecModel<DownloadRow>>,
    tasks: RefCell<Vec<Task>>,
    next_id: Cell<u64>,
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
    match &task.state {
        State::Running { merging: true, .. } => "Joining the video and audio…".into(),
        State::Running {
            done,
            total,
            speed,
            eta,
            ..
        } => {
            let mut parts = vec![match total {
                Some(total) => format!("{} of {}", size(*done), size(*total)),
                None if *done > 0 => size(*done),
                None => "Starting…".into(),
            }];
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
        title: if task.request.title.is_empty() {
            task.request.url.clone().into()
        } else {
            task.request.title.clone().into()
        },
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
        });
        downloads.wire();
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
        on!(on_retry, |this, index| {
            let request = this
                .tasks
                .borrow()
                .get(index as usize)
                .map(|task| task.request.clone());
            if let Some(request) = request {
                this.remove(index as usize);
                this.start(request);
            }
        });
        on!(on_remove, |this, index| this.remove(index as usize));
        on!(on_open_folder, |this| {
            let folder = this.app().prefs().download_dir();
            if let Err(error) = std::fs::create_dir_all(&folder) {
                log::warn!("could not make {}: {error}", folder.display());
            }
            crate::platform::open_path(&folder);
        });
        on!(on_clear_finished, |this| {
            this.tasks
                .borrow_mut()
                .retain(|task| matches!(task.state, State::Running { .. }));
            this.refresh();
        });
    }

    pub fn show(self: &Rc<Self>) {
        self.refresh();
        self.describe_program();
        if let Err(error) = self.window.show() {
            log::warn!("could not show the downloads: {error}");
        }
    }

    /// Which yt-dlp downloads, and its version, read on a thread of its own.
    fn describe_program(&self) {
        let app = self.app();
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
                        window.set_program(text.into());
                    });
                }
            });
        if let Err(error) = spawned {
            log::warn!("could not read yt-dlp's version: {error}");
        }
    }

    fn refresh(&self) {
        let tasks = self.tasks.borrow();
        let rows: Vec<DownloadRow> = tasks.iter().map(row).collect();
        self.rows.set_vec(rows);
        self.window.set_has_finished(
            tasks
                .iter()
                .any(|task| !matches!(task.state, State::Running { .. })),
        );
        self.window.set_folder(
            format!("Saving to {}", self.app().prefs().download_dir().display()).into(),
        );
    }

    fn refresh_task(&self, id: u64) {
        let tasks = self.tasks.borrow();
        if let Some(index) = tasks.iter().position(|task| task.id == id) {
            self.rows.set_row_data(index, row(&tasks[index]));
        }
    }

    /// Starts downloading; false when yt-dlp could not be run, which the
    /// window shows.
    pub fn start(self: &Rc<Self>, request: Request) -> bool {
        let app = self.app();
        let prefs = app.prefs();
        let folder = prefs.download_dir();
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        let mut task = Task {
            id,
            request: request.clone(),
            state: State::running(),
            child: None,
            file: None,
        };
        let started = (|| -> Result<Arc<Mutex<Child>>, String> {
            let program = find_ytdl(&prefs, &app.mpv_config_dir()).ok_or(
                "yt-dlp was not found. Install it, or set where it is in Preferences › Network.",
            )?;
            std::fs::create_dir_all(&folder)
                .map_err(|error| format!("Could not make {}: {error}", folder.display()))?;
            let mut command = command(&program, &request, &folder, &prefs.ytdl_raw_options);
            log::debug!("downloading: {command:?}");
            let child = command
                .spawn()
                .map_err(|error| format!("Could not run {}: {error}", program.display()))?;
            Ok(Arc::new(Mutex::new(child)))
        })();
        let ok = match started {
            Ok(child) => {
                watch(id, child.clone());
                task.child = Some(child);
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

    /// A line or the end from a download's thread.
    fn report(&self, id: u64, report: Report) {
        let finished = {
            let mut tasks = self.tasks.borrow_mut();
            let Some(task) = tasks.iter_mut().find(|task| task.id == id) else {
                return;
            };
            match report {
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
                    task.child = None;
                    if task.state == State::Cancelled {
                        None
                    } else {
                        task.state = match (result, task.file.clone()) {
                            (Ok(()), Some(path)) => State::Done(path),
                            (Ok(()), None) => {
                                State::Failed("yt-dlp finished without saving a file".into())
                            }
                            (Err(error), _) => State::Failed(error),
                        };
                        Some((task.request.title.clone(), task.state.clone()))
                    }
                }
            }
        };
        self.refresh_task(id);
        if let Some((title, state)) = finished {
            self.window.set_has_finished(true);
            let app = self.app();
            match state {
                State::Done(_) => app.notify("Downloaded", &title),
                State::Failed(_) => app.notify("Download failed", &title),
                _ => {}
            }
        }
    }

    fn cancel(&self, index: usize) {
        {
            let mut tasks = self.tasks.borrow_mut();
            let Some(task) = tasks.get_mut(index) else {
                return;
            };
            if let Some(child) = &task.child {
                let _ = child.lock().map(|mut child| child.kill());
                task.state = State::Cancelled;
            }
        }
        if let Some(id) = self.tasks.borrow().get(index).map(|task| task.id) {
            self.refresh_task(id);
        }
        self.window.set_has_finished(true);
    }

    fn remove(&self, index: usize) {
        let removable = self
            .tasks
            .borrow()
            .get(index)
            .is_some_and(|task| !matches!(task.state, State::Running { .. }));
        if removable {
            self.tasks.borrow_mut().remove(index);
            self.refresh();
        }
    }

    /// Stops every running download; yt-dlp picks up its partial files the
    /// next time.
    pub fn stop_all(&self) {
        for task in self.tasks.borrow_mut().iter_mut() {
            if let Some(child) = task.child.take() {
                let _ = child.lock().map(|mut child| child.kill());
                task.state = State::Cancelled;
            }
        }
    }
}

/// Reads a download's output on threads of their own, reporting to the
/// window in the event loop: progress at most a few times a second.
fn watch(id: u64, child: Arc<Mutex<Child>>) {
    let (stdout, stderr) = match child.lock() {
        Ok(mut child) => (child.stdout.take(), child.stderr.take()),
        Err(_) => return,
    };
    let send = move |report: Report| {
        let _ = slint::invoke_from_event_loop(move || {
            App::with_current(|app| {
                if let Some(downloads) = app.existing_downloads() {
                    downloads.report(id, report);
                }
            });
        });
    };
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
    }

    #[test]
    fn sizes_and_progress_read_like_a_file_manager() {
        assert_eq!(size(999), "999 B");
        assert_eq!(size(1_500), "1.5 kB");
        assert_eq!(size(12_345_678), "12.3 MB");
        let task = Task {
            id: 1,
            request: Request {
                url: "https://example.com/v".into(),
                title: String::new(),
                format: None,
            },
            state: State::Running {
                done: 5_000_000,
                total: Some(20_000_000),
                speed: Some(2_000_000.0),
                eta: Some(75),
                merging: false,
            },
            child: None,
            file: None,
        };
        assert_eq!(detail(&task), "5.0 MB of 20.0 MB · 2.0 MB/s · 01:15 left");
        let row = row(&task);
        assert_eq!(row.title, "https://example.com/v");
        assert!((row.progress - 0.25).abs() < f32::EPSILON);
    }
}
