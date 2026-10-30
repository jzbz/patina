//! One Patina at a time. A launch while Patina is running hands what it was
//! asked to open to the running Patina, which opens it and comes to the
//! front, and exits; so files opened from a terminal or a file manager join
//! the windows already open, as Finder's opens do in IINA.
//!
//! The running Patina, the primary, holds a lock on a file in a directory
//! only its user can reach, writes a random id beside it, and listens there
//! on a Unix socket, or on Windows on a named pipe named after the id, which
//! only its user may open. The system releases the lock however a process
//! ends, so a lock that can be taken means no Patina is running, and a
//! socket left behind by a crash is replaced. A connection carries one line
//! of JSON each way: the primary's greeting, the request, the answer.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

/// Changes when requests or answers change meaning; a launch that meets a
/// primary of another version starts on its own.
const VERSION: u32 = 1;
/// How long a launch keeps trying to reach a primary that is still starting
/// or already quitting.
const CLAIM_FOR: Duration = short_in_tests(5_000);
const RETRY_EVERY: Duration = Duration::from_millis(50);
/// How long a launch waits for the primary's greeting.
const GREETING_WITHIN: Duration = short_in_tests(10_000);
/// How long the primary waits for the interface to take a request before
/// sending the launch elsewhere. The launch waits a little longer for that
/// answer once its request has gone.
const VERDICT_WITHIN: Duration = short_in_tests(5_000);
const ANSWER_WITHIN: Duration = short_in_tests(10_000);
/// How long the primary waits for a request once a launch has connected.
const REQUEST_WITHIN: Duration = Duration::from_secs(5);
/// How long a Patina relaunching in software waits for its replacement.
const HAND_OVER_FOR: Duration = Duration::from_secs(10);
/// A request names every input, so it may be long.
const MAX_LINE: u64 = 64 << 20;
const MAX_CONNECTIONS: usize = 16;

const fn short_in_tests(millis: u64) -> Duration {
    Duration::from_millis(if cfg!(test) { millis / 10 } else { millis })
}

/// What to open, and how: from the command line, from another launch, from
/// a patina:// link or from the system.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Request {
    /// Absolute paths, URLs and patina:// links.
    pub sources: Vec<String>,
    /// Add to the newest window's playlist.
    pub enqueue: bool,
    /// Open in an idle or a new window, never in place of what one plays.
    pub new_window: bool,
    /// Open each source in a window of its own.
    pub separate_windows: bool,
    pub fullscreen: bool,
    pub pip: bool,
    /// Play in music mode (patina --music-mode).
    pub music_mode: bool,
    /// mpv options for the windows this request opens.
    pub mpv_options: Vec<(String, String)>,
    /// The desktop's token for bringing a window to the front (Wayland's
    /// XDG_ACTIVATION_TOKEN), passed on by a launch it started.
    pub activation_token: Option<String>,
}

/// What the interface made of a request.
pub enum Verdict {
    /// Open, or to be opened once startup is done.
    Opened,
    /// Patina is quitting; the launch should try the next one.
    Quitting,
    /// Nothing could open it.
    Failed(String),
}

const PENDING: u8 = 0;
const TAKEN: u8 = 1;
const WITHDRAWN: u8 = 2;

/// Where the interface answers a request. It takes the request before
/// opening it, and a request the primary has stopped waiting for can no
/// longer be taken: a launch it sent elsewhere never has its files opened
/// twice.
pub struct Reply {
    state: Arc<AtomicU8>,
    sender: mpsc::Sender<Verdict>,
}

impl Reply {
    /// False once the launch has been told to go elsewhere.
    pub fn take(&self) -> bool {
        self.state
            .compare_exchange(PENDING, TAKEN, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub fn send(self, verdict: Verdict) {
        let _ = self.sender.send(verdict);
    }
}

#[derive(Serialize, Deserialize)]
struct Hello {
    patina: u32,
    #[serde(default)]
    id: String,
}

#[derive(Serialize, Deserialize)]
struct Answer {
    ok: bool,
    #[serde(default)]
    error: String,
    /// The primary is quitting or busy: try again, and soon become the
    /// primary or start on its own.
    #[serde(default)]
    retry: bool,
    /// Not the answer yet: the interface has taken the request and is
    /// opening it, however long that takes.
    #[serde(default)]
    taken: bool,
}

impl Answer {
    fn accepted() -> Self {
        Self {
            ok: true,
            error: String::new(),
            retry: false,
            taken: false,
        }
    }

    fn retry(error: &str) -> Self {
        Self {
            ok: false,
            error: error.into(),
            retry: true,
            taken: false,
        }
    }

    fn refused(error: String) -> Self {
        Self {
            ok: false,
            error,
            retry: false,
            taken: false,
        }
    }

    fn taken() -> Self {
        Self {
            taken: true,
            ..Self::refused(String::new())
        }
    }
}

pub enum Claim {
    /// No Patina was running; this one opens what later launches send.
    Primary(Primary),
    /// The running Patina took the request.
    Forwarded,
    /// The running Patina can't be reached, or wasn't asked; start without
    /// serving others.
    Standalone(String),
}

enum Failure {
    /// The primary may be starting or quitting: try again.
    Retry(String),
    GiveUp(String),
}

/// Becomes the primary, or hands `request` to the one running; unless
/// `forward` is false, when a Patina already running means starting
/// another. Runs before anything is shown, so a launch that forwards exits
/// without a trace.
pub fn claim(settings_dir: &Path, request: &Request, forward: bool) -> Claim {
    match Endpoint::new(settings_dir) {
        Ok(endpoint) => claim_at(endpoint, request, forward),
        Err(error) => Claim::Standalone(error),
    }
}

fn claim_at(endpoint: Endpoint, request: &Request, forward: bool) -> Claim {
    let deadline = Instant::now() + CLAIM_FOR;
    loop {
        match endpoint.try_lock() {
            Ok(Some(lock)) => {
                return match Primary::start(endpoint, lock) {
                    Ok(primary) => Claim::Primary(primary),
                    Err(error) => Claim::Standalone(error),
                };
            }
            Ok(None) if !forward => return Claim::Standalone("Patina is running".into()),
            Ok(None) => {}
            Err(error) => return Claim::Standalone(error),
        }
        match endpoint.forward(request) {
            Ok(()) => return Claim::Forwarded,
            Err(Failure::GiveUp(error)) => return Claim::Standalone(error),
            Err(Failure::Retry(error)) if Instant::now() >= deadline => {
                return Claim::Standalone(format!("the running Patina did not answer: {error}"));
            }
            Err(Failure::Retry(error)) => log::debug!("{error}; trying again"),
        }
        std::thread::sleep(RETRY_EVERY);
    }
}

/// Hands requests this Patina took to the one starting in its place (the
/// relaunch without OpenGL), once that one listens; they would otherwise go
/// with this one's windows. It doesn't touch the lock, which the new Patina
/// must be free to take.
pub fn hand_over(settings_dir: &Path, requests: &[Request]) -> Result<(), String> {
    let endpoint = Endpoint::new(settings_dir)?;
    let deadline = Instant::now() + HAND_OVER_FOR;
    let mut failed = None;
    for request in requests {
        loop {
            match endpoint.forward(request) {
                Ok(()) => break,
                Err(Failure::GiveUp(error)) => {
                    failed = Some(error);
                    break;
                }
                Err(Failure::Retry(_)) if Instant::now() < deadline => {
                    std::thread::sleep(RETRY_EVERY);
                }
                Err(Failure::Retry(_)) => return Err("the new Patina did not start in time".into()),
            }
        }
    }
    failed.map_or(Ok(()), Err)
}

/// Where the lock, the primary's id and its socket are.
#[derive(Clone)]
struct Endpoint {
    dir: PathBuf,
    /// Patinas with different settings folders run side by side.
    key: String,
}

/// How far a launch's exchange with the primary has come, so that a launch
/// that stops waiting before its request has gone can't send it later, and
/// one whose request is being opened doesn't stop waiting.
#[derive(PartialEq)]
enum Progress {
    Waiting,
    Sent,
    Taken,
    Abandoned,
}

impl Endpoint {
    fn new(settings_dir: &Path) -> Result<Self, String> {
        Ok(Self {
            key: key(settings_dir)?,
            dir: private_dir()?,
        })
    }

    fn path(&self, extension: &str) -> PathBuf {
        self.dir.join(format!("{}.{extension}", self.key))
    }

    /// The lock, or None while another Patina holds it.
    fn try_lock(&self) -> Result<Option<std::fs::File>, String> {
        let path = self.path("lock");
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        for _ in 0..3 {
            let file = options
                .open(&path)
                .map_err(|error| format!("could not open {}: {error}", path.display()))?;
            match file.try_lock() {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(format!("could not lock {}: {error}", path.display()));
                }
            }
            // A lock file removed and made again (by a cleaner of old
            // files, say) is no longer the one other launches lock.
            if !same_file(&file, &path) {
                continue;
            }
            keep(&path);
            return Ok(Some(file));
        }
        Err(format!("{} keeps changing", path.display()))
    }

    fn forward(&self, request: &Request) -> Result<(), Failure> {
        let request = serde_json::to_string(request)
            .map_err(|error| Failure::GiveUp(format!("could not send the request: {error}")))?;
        if request.len() as u64 >= MAX_LINE {
            return Err(Failure::GiveUp("too many inputs to hand over".into()));
        }
        // Written before the primary listened.
        let id = std::fs::read_to_string(self.path("id"))
            .map_err(|error| Failure::Retry(format!("no primary id: {error}")))?;
        let id = id.trim().to_owned();
        let stream = self.connect(&id)?;
        // Pipes have no read timeouts, so the exchange runs on a thread of
        // its own; one left waiting ends with the process.
        let progress = Arc::new(Mutex::new(Progress::Waiting));
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn({
            let progress = progress.clone();
            move || {
                let _ = sender.send(exchange(stream, &id, &request, &progress));
            }
        });
        let started = Instant::now();
        loop {
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(Failure::Retry("the exchange ended".into()));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            let mut progress = progress.lock().unwrap_or_else(PoisonError::into_inner);
            let waited = started.elapsed();
            match *progress {
                Progress::Waiting if waited >= GREETING_WITHIN => {
                    *progress = Progress::Abandoned;
                    return Err(Failure::GiveUp(
                        "the running Patina did not answer in time".into(),
                    ));
                }
                // A primary that is working takes or withdraws a request
                // within VERDICT_WITHIN.
                Progress::Sent if waited >= GREETING_WITHIN + ANSWER_WITHIN => {
                    return Err(Failure::GiveUp(
                        "the running Patina did not answer in time".into(),
                    ));
                }
                // Taken, the request is being opened; starting another
                // Patina now would open it twice.
                _ => {}
            }
        }
    }

    #[cfg(unix)]
    fn connect(&self, _id: &str) -> Result<std::os::unix::net::UnixStream, Failure> {
        std::os::unix::net::UnixStream::connect(self.path("sock"))
            .map_err(|error| Failure::Retry(format!("could not connect: {error}")))
    }

    #[cfg(windows)]
    fn connect(&self, id: &str) -> Result<std::fs::File, Failure> {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;
        use windows_sys::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION;
        let name = pipe_name(id);
        let open = || {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                // Lets the server learn who connected but not act as them.
                .security_qos_flags(SECURITY_IDENTIFICATION)
                .open(&name)
        };
        let opened = match open() {
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                let wide = wide(&name);
                // SAFETY: a NUL-terminated name.
                unsafe { windows_sys::Win32::System::Pipes::WaitNamedPipeW(wide.as_ptr(), 1000) };
                open()
            }
            opened => opened,
        };
        let pipe = opened.map_err(|error| Failure::Retry(format!("could not connect: {error}")))?;
        // Pipe names are visible to every user; this one must be served by
        // a process of this user's.
        if !pipe::served_by_this_user(&pipe) {
            return Err(Failure::Retry("the pipe is served by another user".into()));
        }
        Ok(pipe)
    }

    /// Ends the primary's wait for connections.
    fn wake(&self, id: &str) {
        #[cfg(unix)]
        let _ = self.connect(id);
        #[cfg(windows)]
        let _ = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(pipe_name(id));
    }
}

/// The name of a settings folder's files, the same however the folder is
/// spelled.
fn key(settings_dir: &Path) -> Result<String, String> {
    let path =
        canonical(settings_dir).map_err(|error| format!("{}: {error}", settings_dir.display()))?;
    // Rebuilt from its parts, without a trailing separator.
    let path: PathBuf = path.components().collect();
    let mut text = path.to_string_lossy().into_owned();
    if cfg!(windows) {
        // canonicalize's verbatim form names the same folder.
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            text = format!(r"\\{rest}");
        } else if let Some(rest) = text.strip_prefix(r"\\?\") {
            text = rest.to_owned();
        }
    }
    // The file systems macOS and Windows use ignore case.
    if cfg!(any(windows, target_os = "macos")) {
        text = text.to_lowercase();
    }
    Ok(format!("{:016x}", fnv1a(text.as_bytes())))
}

/// `path` with its links resolved, the same before and after the folder
/// exists: the deepest part that does is resolved, and the rest added.
fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut existing = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        if let Ok(mut found) = std::fs::canonicalize(existing) {
            found.extend(missing.iter().rev());
            return Ok(found);
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_owned());
                existing = parent;
            }
            _ => return Ok(absolute),
        }
    }
}

/// Whether `file` is still the file at `path`.
#[cfg(unix)]
fn same_file(file: &std::fs::File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (file.metadata(), std::fs::metadata(path)) {
        (Ok(open), Ok(named)) => open.dev() == named.dev() && open.ino() == named.ino(),
        _ => false,
    }
}

/// Windows doesn't let an open file's name go to another file.
#[cfg(windows)]
fn same_file(_file: &std::fs::File, _path: &Path) -> bool {
    true
}

/// Asks that a file in the runtime directory be kept: the XDG spec lets old
/// files there be cleaned up unless their sticky bit is set.
fn keep(path: &Path) {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(path) {
            let mut permissions = metadata.permissions();
            permissions.set_mode(permissions.mode() | 0o1000);
            let _ = std::fs::set_permissions(path, permissions);
        }
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    let _ = path;
}

/// The pipe of the primary with `id`: a name no one else can guess, so no
/// one can take it first.
#[cfg(windows)]
fn pipe_name(id: &str) -> String {
    format!(r"\\.\pipe\video.patina.Patina-{id}")
}

fn exchange(
    stream: impl Read + Write,
    id: &str,
    request: &str,
    progress: &Mutex<Progress>,
) -> Result<(), Failure> {
    // One reader for the whole exchange: what it has read past one line is
    // the next.
    let mut stream = BufReader::new(stream);
    let retry = |error: std::io::Error| Failure::Retry(error.to_string());
    let hello: Hello = serde_json::from_str(&read_line(&mut stream).map_err(retry)?)
        .map_err(|error| Failure::Retry(format!("unreadable greeting: {error}")))?;
    if hello.patina != VERSION {
        return Err(Failure::GiveUp(format!(
            "the running Patina is of another version ({})",
            hello.patina
        )));
    }
    if hello.id != id {
        return Err(Failure::Retry(
            "the endpoint does not belong to the Patina holding the lock".into(),
        ));
    }
    {
        let mut progress = progress.lock().unwrap_or_else(PoisonError::into_inner);
        if *progress == Progress::Abandoned {
            return Err(Failure::GiveUp("the launch stopped waiting".into()));
        }
        *progress = Progress::Sent;
    }
    // Windows lets a process bring a window to the front only if it was in
    // front itself; this launch lends the primary its turn.
    #[cfg(windows)]
    // SAFETY: a plain call without pointers.
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(
            windows_sys::Win32::UI::WindowsAndMessaging::ASFW_ANY,
        );
    }
    write_line(stream.get_mut(), request).map_err(retry)?;
    let mut taken = false;
    let answer = loop {
        let line = match read_line(&mut stream) {
            Ok(line) => line,
            // Taken, it is open or opening; sent again, it would open twice.
            Err(error) if taken => {
                log::debug!("the running Patina took the request, then {error}");
                return Ok(());
            }
            Err(error) => return Err(retry(error)),
        };
        let answer: Answer = serde_json::from_str(&line)
            .map_err(|error| Failure::Retry(format!("unreadable answer: {error}")))?;
        if !answer.taken {
            break answer;
        }
        taken = true;
        *progress.lock().unwrap_or_else(PoisonError::into_inner) = Progress::Taken;
    };
    match answer {
        Answer { ok: true, .. } => Ok(()),
        Answer {
            retry: true, error, ..
        } => Err(Failure::Retry(error)),
        Answer { error, .. } => Err(Failure::GiveUp(error)),
    }
}

/// One line, without its newline. An error at the end of the stream, and
/// InvalidData past `MAX_LINE`. What was read past the line stays in the
/// reader, for the next.
fn read_line(reader: &mut impl BufRead) -> std::io::Result<String> {
    let mut line = String::new();
    reader.take(MAX_LINE).read_line(&mut line)?;
    match line.strip_suffix('\n') {
        Some(line) => Ok(line.to_owned()),
        None if line.len() as u64 >= MAX_LINE => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the line is too long",
        )),
        None => Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "the connection ended",
        )),
    }
}

/// One line, written at once.
fn write_line(stream: &mut impl Write, line: &str) -> std::io::Result<()> {
    stream.write_all(format!("{line}\n").as_bytes())?;
    stream.flush()
}

/// Hands a request to the interface, with where its verdict goes. Returns
/// false if it could not be handed over.
pub type Deliver = dyn Fn(Request, Reply) -> bool + Send + Sync;

/// The lock and the files beside it. Dropping it removes the files that are
/// still this primary's and then gives up the lock, so that the next launch
/// becomes the primary, whether this one served or failed to start.
struct Claimed {
    endpoint: Endpoint,
    id: String,
    /// The socket file this primary made, by device and inode.
    #[cfg(unix)]
    socket: Option<(u64, u64)>,
    lock: Option<std::fs::File>,
}

impl Claimed {
    #[cfg(unix)]
    fn socket_is_ours(&self) -> bool {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(self.endpoint.path("sock"))
            .is_ok_and(|metadata| Some((metadata.dev(), metadata.ino())) == self.socket)
    }
}

impl Drop for Claimed {
    fn drop(&mut self) {
        #[cfg(unix)]
        if self.socket_is_ours() {
            let _ = std::fs::remove_file(self.endpoint.path("sock"));
        }
        let path = self.endpoint.path("id");
        if std::fs::read_to_string(&path).is_ok_and(|id| id.trim() == self.id) {
            let _ = std::fs::remove_file(&path);
        }
        drop(self.lock.take());
    }
}

/// A Patina holding the lock, about to listen.
pub struct Primary {
    #[cfg(unix)]
    listener: std::os::unix::net::UnixListener,
    #[cfg(windows)]
    first_pipe: std::os::windows::io::OwnedHandle,
    #[cfg(windows)]
    security: Arc<pipe::Security>,
    // Last, so that the pipe is closed before the lock is given up.
    claimed: Claimed,
}

impl Primary {
    fn start(endpoint: Endpoint, lock: std::fs::File) -> Result<Self, String> {
        #[cfg_attr(windows, allow(unused_mut))]
        let mut claimed = Claimed {
            endpoint,
            id: random_id(),
            #[cfg(unix)]
            socket: None,
            lock: Some(lock),
        };
        let id_path = claimed.endpoint.path("id");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options
            .open(&id_path)
            .and_then(|mut file| file.write_all(claimed.id.as_bytes()))
            .map_err(|error| format!("could not write {}: {error}", id_path.display()))?;
        keep(&id_path);

        #[cfg(unix)]
        let listener = {
            use std::os::unix::fs::MetadataExt;
            let path = claimed.endpoint.path("sock");
            // Only a Patina that crashed leaves one: the lock is ours.
            let _ = std::fs::remove_file(&path);
            let listener = std::os::unix::net::UnixListener::bind(&path)
                .map_err(|error| format!("could not listen on {}: {error}", path.display()))?;
            claimed.socket = std::fs::symlink_metadata(&path)
                .ok()
                .map(|metadata| (metadata.dev(), metadata.ino()));
            keep(&path);
            listener
        };
        #[cfg(windows)]
        let (security, first_pipe) = {
            let security = Arc::new(pipe::Security::current_user()?);
            let name = wide(&pipe_name(&claimed.id));
            let pipe = pipe::create_pipe(&name, true, &security)
                .map_err(|error| format!("could not create the pipe: {error}"))?;
            (security, pipe)
        };
        Ok(Self {
            #[cfg(unix)]
            listener,
            #[cfg(windows)]
            first_pipe,
            #[cfg(windows)]
            security,
            claimed,
        })
    }

    /// Starts answering launches; each request goes to `deliver`.
    pub fn serve(self, deliver: impl Fn(Request, Reply) -> bool + Send + Sync + 'static) -> Server {
        let shared = Arc::new(Shared {
            closing: AtomicBool::new(false),
            connections: AtomicUsize::new(0),
            deliver: Box::new(deliver),
            id: self.claimed.id.clone(),
        });
        let accept = {
            let shared = shared.clone();
            #[cfg(unix)]
            let listener = self.listener;
            #[cfg(windows)]
            let (name, first_pipe, security) = (
                wide(&pipe_name(&self.claimed.id)),
                self.first_pipe,
                self.security,
            );
            std::thread::Builder::new()
                .name("instance".into())
                .spawn(move || {
                    #[cfg(unix)]
                    accept_connections(listener, &shared);
                    #[cfg(windows)]
                    pipe::accept_connections(&name, first_pipe, &security, &shared);
                })
        };
        let accept = match accept {
            Ok(accept) => Some(accept),
            Err(error) => {
                log::warn!("could not listen for other launches: {error}");
                None
            }
        };
        Server {
            shared,
            accept,
            claimed: self.claimed,
        }
    }
}

struct Shared {
    closing: AtomicBool,
    connections: AtomicUsize,
    deliver: Box<Deliver>,
    id: String,
}

/// The primary's listener. Dropping it stops serving and gives up the lock,
/// so that the next launch becomes the primary.
pub struct Server {
    shared: Arc<Shared>,
    accept: Option<std::thread::JoinHandle<()>>,
    claimed: Claimed,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared.closing.store(true, Ordering::SeqCst);
        // A socket at the path that isn't this primary's belongs to another
        // (after its lock file was removed); this one's accept thread then
        // ends with the process.
        #[cfg(unix)]
        if !self.claimed.socket_is_ours() {
            return;
        }
        if let Some(accept) = self.accept.take() {
            let deadline = Instant::now() + Duration::from_millis(500);
            while !accept.is_finished() && Instant::now() < deadline {
                self.claimed.endpoint.wake(&self.claimed.id);
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        // Then `claimed` removes the files and gives up the lock.
    }
}

#[cfg(unix)]
fn accept_connections(listener: std::os::unix::net::UnixListener, shared: &Arc<Shared>) {
    for stream in listener.incoming() {
        if shared.closing.load(Ordering::SeqCst) {
            break;
        }
        match stream {
            Ok(stream) => {
                let _ = stream.set_read_timeout(Some(REQUEST_WITHIN));
                let _ = stream.set_write_timeout(Some(REQUEST_WITHIN));
                spawn_connection(stream, shared);
            }
            Err(error) => {
                // Out of file descriptors, say: the launch stays queued,
                // and an immediate retry would fail the same way.
                log::debug!("could not accept a launch: {error}");
                std::thread::sleep(RETRY_EVERY);
            }
        }
    }
}

#[cfg(windows)]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A launch's connection to the primary.
trait Connection: Read + Write + Send + 'static {
    /// Waits until the launch has written something, or fails after
    /// `within`.
    fn wait_for_data(&mut self, _within: Duration) -> std::io::Result<()> {
        Ok(())
    }

    /// Waits until the launch has read what was written.
    fn finish(&mut self) {}
}

/// Sockets have read timeouts of their own.
#[cfg(unix)]
impl Connection for std::os::unix::net::UnixStream {}

fn spawn_connection(mut stream: impl Connection, shared: &Arc<Shared>) {
    if shared.connections.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
        shared.connections.fetch_sub(1, Ordering::SeqCst);
        return;
    }
    let shared = shared.clone();
    let spawned = std::thread::Builder::new()
        .name("instance-request".into())
        .spawn({
            let shared = shared.clone();
            move || {
                match serve_connection(&mut stream, &shared) {
                    Ok(()) => stream.finish(),
                    Err(error) => log::debug!("a launch's request failed: {error}"),
                }
                shared.connections.fetch_sub(1, Ordering::SeqCst);
            }
        });
    if spawned.is_err() {
        shared.connections.fetch_sub(1, Ordering::SeqCst);
    }
}

fn serve_connection(stream: &mut impl Connection, shared: &Shared) -> std::io::Result<()> {
    let hello = serde_json::to_string(&Hello {
        patina: VERSION,
        id: shared.id.clone(),
    })?;
    write_line(stream, &hello)?;
    stream.wait_for_data(REQUEST_WITHIN)?;
    let line = read_line(&mut BufReader::new(&mut *stream));
    let answer = match line {
        Ok(line) => match serde_json::from_str::<Request>(&line) {
            Ok(request) => answer(request, shared, stream),
            Err(error) => Answer::refused(format!("unreadable request: {error}")),
        },
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            Answer::refused("the request is too long or not text".into())
        }
        Err(error) => return Err(error),
    };
    write_line(stream, &serde_json::to_string(&answer)?)
}

/// The interface's verdict on `request`. A request it hasn't taken by the
/// time Patina starts quitting, or after `VERDICT_WITHIN`, is withdrawn, so
/// it can't be opened here once the launch has gone elsewhere; one it has
/// taken is reported to the launch, which then waits for the verdict.
fn answer(request: Request, shared: &Shared, stream: &mut impl Connection) -> Answer {
    if shared.closing.load(Ordering::SeqCst) {
        return Answer::retry("the running Patina is quitting");
    }
    let state = Arc::new(AtomicU8::new(PENDING));
    let (sender, verdict) = mpsc::channel();
    let reply = Reply {
        state: state.clone(),
        sender,
    };
    if !(shared.deliver)(request, reply) {
        return Answer::retry("the running Patina is quitting");
    }
    let deadline = Instant::now() + VERDICT_WITHIN;
    let mut told = false;
    loop {
        match verdict.recv_timeout(Duration::from_millis(20)) {
            Ok(Verdict::Opened) => return Answer::accepted(),
            Ok(Verdict::Quitting) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Answer::retry("the running Patina is quitting");
            }
            Ok(Verdict::Failed(error)) => return Answer::refused(error),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if !told && state.load(Ordering::SeqCst) == TAKEN {
                    told = true;
                    if let Ok(taken) = serde_json::to_string(&Answer::taken()) {
                        let _ = write_line(stream, &taken);
                    }
                }
                let closing = shared.closing.load(Ordering::SeqCst);
                // Once taken, the verdict is on its way.
                if (closing || Instant::now() >= deadline)
                    && state
                        .compare_exchange(PENDING, WITHDRAWN, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                {
                    return Answer::retry(if closing {
                        "the running Patina is quitting"
                    } else {
                        "the running Patina is busy"
                    });
                }
            }
        }
    }
}

#[cfg(windows)]
mod pipe {
    use super::{Connection, Duration, Instant, RETRY_EVERY, Shared, spawn_connection};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use windows_sys::Win32::Foundation::{
        ERROR_PIPE_CONNECTED, GetLastError, INVALID_HANDLE_VALUE, LocalFree,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// A security descriptor that lets only the current user at the pipe;
    /// the default one lets everyone read it.
    pub struct Security {
        descriptor: PSECURITY_DESCRIPTOR,
    }

    // SAFETY: the descriptor is only read once made, and freed once.
    unsafe impl Send for Security {}
    unsafe impl Sync for Security {}

    impl Security {
        pub fn current_user() -> Result<Self, String> {
            let sid = current_user_sid()?;
            let sddl = super::wide(&format!("D:P(A;;GA;;;{sid})"));
            let mut descriptor = std::ptr::null_mut();
            // SAFETY: a NUL-terminated string, and a pointer for the result,
            // which Drop frees.
            let made = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    std::ptr::null_mut(),
                )
            };
            if made == 0 {
                return Err(format!(
                    "could not make the pipe's security: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(Self { descriptor })
        }

        fn attributes(&self) -> SECURITY_ATTRIBUTES {
            SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.descriptor,
                bInheritHandle: 0,
            }
        }
    }

    impl Drop for Security {
        fn drop(&mut self) {
            // SAFETY: allocated by the conversion, freed here only.
            unsafe { LocalFree(self.descriptor) };
        }
    }

    /// The SID of the user this process runs as, as text.
    fn current_user_sid() -> Result<String, String> {
        // SAFETY: the pseudo-handle of this process.
        unsafe { user_sid(GetCurrentProcess()) }
    }

    /// Whether the process serving `pipe` runs as this process's user.
    pub fn served_by_this_user(pipe: &std::fs::File) -> bool {
        use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        let mut id = 0u32;
        // SAFETY: a pipe handle this process owns, and the server's process
        // opened for queries only and closed by OwnedHandle.
        unsafe {
            if GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut id) == 0 {
                return false;
            }
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, id);
            if process.is_null() {
                return false;
            }
            let process = OwnedHandle::from_raw_handle(process);
            matches!(
                (user_sid(process.as_raw_handle()), current_user_sid()),
                (Ok(server), Ok(ours)) if server == ours
            )
        }
    }

    /// The SID of the user `process` runs as, as text.
    ///
    /// # Safety
    /// `process` must be a process handle with query rights.
    unsafe fn user_sid(process: windows_sys::Win32::Foundation::HANDLE) -> Result<String, String> {
        let error = |what: &str| format!("{what}: {}", std::io::Error::last_os_error());
        // SAFETY: the process's token, closed by OwnedHandle; buffers of
        // the size the first call asks for, aligned for TOKEN_USER; the SID
        // string freed with LocalFree as its documentation says.
        unsafe {
            let mut token = std::ptr::null_mut();
            if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
                return Err(error("could not open the process token"));
            }
            let token = OwnedHandle::from_raw_handle(token);
            let mut length = 0u32;
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                std::ptr::null_mut(),
                0,
                &mut length,
            );
            let mut buffer = vec![0u64; (length as usize).div_ceil(8)];
            if GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                buffer.as_mut_ptr().cast(),
                length,
                &mut length,
            ) == 0
            {
                return Err(error("could not read the process's user"));
            }
            let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
            let mut text = std::ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
                return Err(error("could not read the user's SID"));
            }
            let length = (0..).take_while(|&index| *text.add(index) != 0).count();
            let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
            LocalFree(text.cast());
            Ok(sid)
        }
    }

    pub fn create_pipe(
        name: &[u16],
        first: bool,
        security: &Security,
    ) -> std::io::Result<OwnedHandle> {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
        };
        use windows_sys::Win32::System::Pipes::{
            CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
            PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
        };
        // The first instance fails if another process already has the name.
        let mode = if first {
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            PIPE_ACCESS_DUPLEX
        };
        let attributes = security.attributes();
        // SAFETY: a NUL-terminated name and attributes that outlive the call.
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                4096,
                4096,
                0,
                &attributes,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: a new handle nothing else owns.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }

    pub fn accept_connections(
        name: &[u16],
        first: OwnedHandle,
        security: &Security,
        shared: &Arc<Shared>,
    ) {
        let mut listening = first;
        while !shared.closing.load(Ordering::SeqCst) {
            // SAFETY: a pipe handle this thread owns; no overlapped I/O.
            let connected = unsafe {
                windows_sys::Win32::System::Pipes::ConnectNamedPipe(
                    listening.as_raw_handle(),
                    std::ptr::null_mut(),
                ) != 0
                    || GetLastError() == ERROR_PIPE_CONNECTED
            };
            if shared.closing.load(Ordering::SeqCst) {
                break;
            }
            if !connected {
                // A launch that left before it was served (ERROR_NO_DATA)
                // leaves the instance unable to listen again; a fresh one
                // takes its place, made first so that the name stays ours.
                std::thread::sleep(RETRY_EVERY);
                match create_pipe(name, false, security) {
                    Ok(fresh) => listening = fresh,
                    Err(error) => log::debug!("could not replace a pipe instance: {error}"),
                }
                continue;
            }
            // The next instance first, so that the name always has one of
            // this primary's and no one else can make it.
            let next = match create_pipe(name, false, security) {
                Ok(next) => next,
                Err(error) => {
                    log::warn!("could not wait for other launches: {error}");
                    spawn_connection(std::fs::File::from(listening), shared);
                    return;
                }
            };
            spawn_connection(
                std::fs::File::from(std::mem::replace(&mut listening, next)),
                shared,
            );
        }
    }

    impl Connection for std::fs::File {
        /// A pipe has no read timeouts, so this looks for data now and then.
        fn wait_for_data(&mut self, within: Duration) -> std::io::Result<()> {
            let deadline = Instant::now() + within;
            loop {
                let mut available = 0u32;
                // SAFETY: a pipe handle this thread owns, and a counter to
                // fill in; nothing is read.
                let peeked = unsafe {
                    windows_sys::Win32::System::Pipes::PeekNamedPipe(
                        self.as_raw_handle(),
                        std::ptr::null_mut(),
                        0,
                        std::ptr::null_mut(),
                        &mut available,
                        std::ptr::null_mut(),
                    )
                };
                if peeked == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if available > 0 {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(std::io::ErrorKind::TimedOut.into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        /// A pipe's unread data is lost when it closes.
        fn finish(&mut self) {
            let _ = self.sync_all();
        }
    }
}

/// A directory only this user can reach: the session's runtime directory
/// on Linux (the app's own part of it in a Flatpak), a private folder in
/// the temporary folder elsewhere.
#[cfg(unix)]
fn private_dir() -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    // SAFETY: getuid can't fail.
    let uid = unsafe { libc::getuid() };
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty() && cfg!(not(target_os = "macos")))
        .map(PathBuf::from);
    let dir = match (runtime, std::env::var_os("FLATPAK_ID")) {
        (Some(runtime), Some(app)) => runtime.join("app").join(app).join("patina"),
        (Some(runtime), None) => runtime.join("patina"),
        (None, _) => std::env::temp_dir().join(format!("patina-{uid}")),
    };
    if let Err(error) = std::fs::DirBuilder::new().mode(0o700).create(&dir)
        && error.kind() != std::io::ErrorKind::AlreadyExists
    {
        return Err(format!("could not create {}: {error}", dir.display()));
    }
    // A directory someone else made, or could write to, isn't private.
    let metadata = std::fs::symlink_metadata(&dir)
        .map_err(|error| format!("could not read {}: {error}", dir.display()))?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(format!("{} is not private to this user", dir.display()));
    }
    Ok(dir)
}

/// On Windows, the user's local application data, which other users can't
/// reach; not the temporary folder, which may be a shared one.
#[cfg(windows)]
fn private_dir() -> Result<PathBuf, String> {
    let dir = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or("LOCALAPPDATA is not set")?
        .join("Patina")
        .join("instance");
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("could not create {}: {error}", dir.display()))?;
    Ok(dir)
}

/// FNV-1a: a stable hash, the same in every build, unlike std's.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// 128 bits from the system's random source, which seeds std's hashers.
fn random_id() -> String {
    use std::hash::BuildHasher;
    let state = std::collections::hash_map::RandomState::new();
    let first = state.hash_one(std::process::id());
    let second = std::collections::hash_map::RandomState::new().hash_one(first);
    format!("{first:016x}{second:016x}")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Removes a test's folder when the test ends.
    struct Folder(PathBuf);

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Endpoints in a folder of the test's own, so that tests run side by
    /// side and apart from a Patina that is running. The folder goes when
    /// the returned guard does, which is declared first so as to go last.
    fn endpoint(name: &str) -> (Folder, Endpoint) {
        let dir = std::env::temp_dir().join(format!(
            "patina-instance-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let endpoint = Endpoint {
            dir: dir.clone(),
            key: "test".into(),
        };
        (Folder(dir), endpoint)
    }

    fn primary(claim: Claim) -> Primary {
        match claim {
            Claim::Primary(primary) => primary,
            Claim::Forwarded => panic!("forwarded"),
            Claim::Standalone(error) => panic!("standalone: {error}"),
        }
    }

    fn request(source: &str) -> Request {
        Request {
            sources: vec![source.into()],
            mpv_options: vec![("start".into(), "30".into())],
            ..Default::default()
        }
    }

    /// Opens every request it is handed, noting it.
    fn opener(received: &Arc<Mutex<Vec<Request>>>) -> impl Fn(Request, Reply) -> bool + use<> {
        let received = received.clone();
        move |request, reply| {
            if reply.take() {
                received.lock().unwrap().push(request);
                reply.send(Verdict::Opened);
            }
            true
        }
    }

    #[test]
    fn a_second_launch_forwards_to_the_first() {
        let (_folder, endpoint) = endpoint("forward");
        let received = Arc::new(Mutex::new(Vec::new()));
        let server =
            primary(claim_at(endpoint.clone(), &request("/a.mkv"), true)).serve(opener(&received));
        assert!(matches!(
            claim_at(endpoint.clone(), &request("/b.mkv"), true),
            Claim::Forwarded
        ));
        assert_eq!(*received.lock().unwrap(), [request("/b.mkv")]);
        // Asked not to forward, a launch starts on its own.
        assert!(matches!(
            claim_at(endpoint.clone(), &request("/c.mkv"), false),
            Claim::Standalone(_)
        ));

        // Once the primary is gone, the next launch takes its place, though
        // its socket file was left behind.
        drop(server);
        std::fs::write(endpoint.path("sock"), "").unwrap();
        let _next = primary(claim_at(endpoint.clone(), &request("/d.mkv"), true));
    }

    #[test]
    fn a_primary_that_never_serves_cleans_up() {
        let (_folder, endpoint) = endpoint("unserved");
        let first = primary(claim_at(endpoint.clone(), &request("/a.mkv"), true));
        assert!(endpoint.path("sock").exists() && endpoint.path("id").exists());
        drop(first);
        assert!(!endpoint.path("sock").exists() && !endpoint.path("id").exists());
        assert!(endpoint.try_lock().unwrap().is_some());
    }

    /// A lock file removed while its Patina runs: the next launch becomes a
    /// primary of its own, and the first, quitting, leaves its files alone.
    #[test]
    fn a_removed_lock_file_leaves_the_new_primary_alone() {
        let (_folder, endpoint) = endpoint("removed");
        let first = primary(claim_at(endpoint.clone(), &request("/a.mkv"), true));
        std::fs::remove_file(endpoint.path("lock")).unwrap();
        let second = primary(claim_at(endpoint.clone(), &request("/b.mkv"), true));
        drop(first);
        assert!(endpoint.path("sock").exists() && endpoint.path("id").exists());
        drop(second);
        assert!(!endpoint.path("sock").exists());
    }

    #[test]
    fn launches_at_once_make_one_primary() {
        let (_folder, endpoint) = endpoint("race");
        let forwarded = Arc::new(AtomicUsize::new(0));
        let servers = Arc::new(Mutex::new(Vec::new()));
        let launches: Vec<_> = (0..6)
            .map(|index| {
                let (endpoint, forwarded, servers) =
                    (endpoint.clone(), forwarded.clone(), servers.clone());
                std::thread::spawn(move || {
                    match claim_at(endpoint, &request(&format!("/{index}.mkv")), true) {
                        Claim::Primary(primary) => {
                            let received = Arc::new(Mutex::new(Vec::new()));
                            let server = primary.serve(opener(&received));
                            servers.lock().unwrap().push(server);
                        }
                        Claim::Forwarded => {
                            forwarded.fetch_add(1, Ordering::SeqCst);
                        }
                        Claim::Standalone(error) => panic!("standalone: {error}"),
                    }
                })
            })
            .collect();
        for launch in launches {
            launch.join().unwrap();
        }
        assert_eq!(servers.lock().unwrap().len(), 1);
        assert_eq!(forwarded.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn a_quitting_primary_hands_over() {
        let (_folder, endpoint) = endpoint("quitting");
        let server = Arc::new(Mutex::new(None));
        let first = primary(claim_at(endpoint.clone(), &request("/a.mkv"), true));
        // The interface refuses, as it does once Patina is quitting, and
        // the primary then goes; the launch keeps trying and takes over.
        let served = first.serve({
            let server = server.clone();
            move |_, reply| {
                let server = server.clone();
                std::thread::spawn(move || {
                    reply.send(Verdict::Quitting);
                    std::thread::sleep(Duration::from_millis(100));
                    drop(server.lock().unwrap().take());
                });
                true
            }
        });
        *server.lock().unwrap() = Some(served);
        let _next = primary(claim_at(endpoint.clone(), &request("/b.mkv"), true));
    }

    /// An interface too busy to take a request in time: the launch starts
    /// on its own, and the request can no longer be opened here.
    #[test]
    fn a_busy_primary_withdraws_the_request() {
        let (_folder, endpoint) = endpoint("busy");
        let replies = Arc::new(Mutex::new(Vec::new()));
        let _server = primary(claim_at(endpoint.clone(), &request("/a.mkv"), true)).serve({
            let replies = replies.clone();
            move |_, reply| {
                replies.lock().unwrap().push(reply);
                true
            }
        });
        assert!(matches!(
            claim_at(endpoint.clone(), &request("/b.mkv"), true),
            Claim::Standalone(_)
        ));
        let replies = replies.lock().unwrap();
        assert!(!replies.is_empty() && replies.iter().all(|reply| !reply.take()));
    }

    /// An interface that takes a request and opens it slowly (many windows
    /// at once): the launch waits for it rather than open it again.
    #[test]
    fn a_slow_open_is_waited_for() {
        let (_folder, endpoint) = endpoint("slow");
        let opened = Arc::new(AtomicUsize::new(0));
        let _server = primary(claim_at(endpoint.clone(), &request("/a.mkv"), true)).serve({
            let opened = opened.clone();
            move |_, reply| {
                let opened = opened.clone();
                std::thread::spawn(move || {
                    if reply.take() {
                        std::thread::sleep(GREETING_WITHIN + ANSWER_WITHIN + VERDICT_WITHIN);
                        opened.fetch_add(1, Ordering::SeqCst);
                        reply.send(Verdict::Opened);
                    }
                });
                true
            }
        });
        assert!(matches!(
            claim_at(endpoint.clone(), &request("/b.mkv"), true),
            Claim::Forwarded
        ));
        assert_eq!(opened.load(Ordering::SeqCst), 1);
    }

    /// A primary that greets too late: the launch starts on its own and its
    /// request never reaches the primary.
    #[test]
    fn a_launch_that_gave_up_sends_nothing() {
        let (_folder, endpoint) = endpoint("late");
        let first = primary(claim_at(endpoint.clone(), &request("/a.mkv"), true));
        assert!(matches!(
            endpoint.forward(&request("/b.mkv")),
            Err(Failure::GiveUp(_))
        ));
        let received = Arc::new(Mutex::new(Vec::new()));
        let _server = first.serve(opener(&received));
        std::thread::sleep(Duration::from_millis(300));
        assert!(received.lock().unwrap().is_empty());
    }

    #[test]
    fn too_much_to_hand_over_is_refused_at_once() {
        let (_folder, endpoint) = endpoint("long");
        let _first = primary(claim_at(endpoint.clone(), &request("/a.mkv"), true));
        let many = Request {
            sources: vec!["x".repeat(MAX_LINE as usize)],
            ..Default::default()
        };
        // Given up on, not retried for CLAIM_FOR.
        assert!(matches!(endpoint.forward(&many), Err(Failure::GiveUp(_))));
    }

    /// The taken notice and the answer arriving in one read: the answer
    /// counts, and the request isn't sent again.
    #[test]
    fn an_answer_that_comes_with_the_taken_notice_counts() {
        let (_folder, endpoint) = endpoint("together");
        let lock = endpoint.try_lock().unwrap().unwrap();
        std::fs::write(endpoint.path("id"), "right").unwrap();
        let listener = std::os::unix::net::UnixListener::bind(endpoint.path("sock")).unwrap();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let (mut stream, _) = listener.accept().unwrap();
                write_line(&mut stream, r#"{"patina":1,"id":"right"}"#).unwrap();
                read_line(&mut BufReader::new(&mut stream)).unwrap();
                stream
                    .write_all(b"{\"ok\":false,\"taken\":true}\n{\"ok\":true}\n")
                    .unwrap();
            });
            assert!(endpoint.forward(&request("/a.mkv")).is_ok());
        });
        drop(lock);
    }

    #[test]
    fn another_version_or_endpoint_is_not_trusted() {
        let (_folder, endpoint) = endpoint("version");
        let lock = endpoint.try_lock().unwrap().unwrap();
        std::fs::write(endpoint.path("id"), "right").unwrap();
        let listener = std::os::unix::net::UnixListener::bind(endpoint.path("sock")).unwrap();
        let greet = |hello: &'static str| {
            let (mut stream, _) = listener.accept().unwrap();
            writeln!(stream, "{hello}").unwrap();
        };
        std::thread::scope(|scope| {
            scope.spawn(|| greet(r#"{"patina":2,"id":"right"}"#));
            assert!(matches!(
                endpoint.forward(&request("/a.mkv")),
                Err(Failure::GiveUp(_))
            ));
            scope.spawn(|| greet(r#"{"patina":1,"id":"wrong"}"#));
            assert!(matches!(
                endpoint.forward(&request("/a.mkv")),
                Err(Failure::Retry(_))
            ));
        });
        drop(lock);
    }

    #[test]
    fn stable_keys() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_ne!(random_id(), random_id());
        let absolute = std::env::current_dir().unwrap().join("settings");
        assert_eq!(key(Path::new("settings")), key(&absolute));
        assert_eq!(key(&absolute), key(&absolute.join("")));
    }

    /// A settings folder reached through a link has the same key before
    /// Patina first makes it as after.
    #[test]
    fn keys_hold_once_the_folder_exists() {
        let (folder, _) = endpoint("canonical");
        let real = folder.0.join("real");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, folder.0.join("link")).unwrap();
        let settings = folder.0.join("link/patina");
        let before = key(&settings);
        std::fs::create_dir(&settings).unwrap();
        assert_eq!(before, key(&settings));
        assert_eq!(key(&settings), key(&real.join("patina")));
    }
}
