//! The app-wide Jellyfin session, owned by the app on the interface
//! thread: the servers Patina knows (`jellyfin.json`), the accounts signed
//! in on them with a client each, the image cache every window draws from,
//! the server chosen last, and the registry of what the library opened.
//! Windows follow it through `Listener`.
//!
//! Nothing here blocks the interface thread. The keyring and the network
//! are asked on threads of their own (`background`), and their answers come
//! back through the Slint event loop. Signing in is a flow of a few such
//! steps (`SignIn`), one per window that signs in, each step's answer
//! dropped when the user has moved on from it.

use super::api::{self, Client, Clients, Error};
use super::auth::{self, Identity};
use super::discovery;
use super::images::{self, Images};
use super::models::*;
use super::playback::{self, Report, Reporter, Servers};
use super::store::{self, Server, Store};
use slint::{Timer, TimerMode};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::{Arc, Condvar, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

/// How often a Quick Connect code is asked after, and how long it lasts:
/// the server forgets a request after ten minutes.
const QUICK_CONNECT_POLL: Duration = Duration::from_secs(5);
/// How long signing out waits for the reports on their way, the stop of
/// what played among them, before the server revokes the token they go
/// with.
const SIGN_OUT_REPORTS: Duration = Duration::from_secs(5);
const QUICK_CONNECT_LIFETIME: Duration = Duration::from_secs(10 * 60);

/// Runs `work` on a thread of its own, and then `done` with what it gave,
/// on the interface thread. Network requests and the keyring block, so
/// this is how the library asks either.
pub fn background<T: Send + 'static>(
    name: &str,
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) + 'static,
) {
    let (sender, receiver) = async_channel::bounded(1);
    let spawned = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let _ = sender.send_blocking(work());
        });
    if let Err(error) = spawned {
        log::warn!("could not start {name}: {error}");
        return;
    }
    let spawned = slint::spawn_local(async move {
        if let Ok(result) = receiver.recv().await {
            done(result);
        }
    });
    if let Err(error) = spawned {
        log::warn!("could not wait for {name}: {error}");
    }
}

/// A job for a thread that runs one after another.
type Job = Box<dyn FnOnce() + Send>;

/// Hands `work`'s answer to `done` on the interface thread, as
/// `background` does, for work run by a queue.
fn answered<T: Send + 'static>(
    name: &str,
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) + 'static,
) -> Job {
    let (sender, receiver) = async_channel::bounded(1);
    let spawned = slint::spawn_local(async move {
        if let Ok(result) = receiver.recv().await {
            done(result);
        }
    });
    if let Err(error) = spawned {
        log::warn!("could not wait for {name}: {error}");
    }
    Box::new(move || {
        let _ = sender.send_blocking(work());
    })
}

/// Runs keyring work on one thread, one job at a time in the order asked,
/// and then `done` on the interface thread. A token saved and then deleted
/// (signing out while the keyring waits to be unlocked) must not be deleted
/// first and saved after, which threads of their own allowed.
pub fn keyring<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) + 'static,
) {
    // Tests have no event loop to answer on, and can't make one off the
    // main thread on macOS; their keyring is store's stand-in in memory,
    // which doesn't block.
    if cfg!(test) {
        done(work());
        return;
    }
    static QUEUE: OnceLock<Mutex<mpsc::Sender<Job>>> = OnceLock::new();
    let queue = QUEUE.get_or_init(|| {
        let (sender, receiver) = mpsc::channel::<Job>();
        let spawned = std::thread::Builder::new()
            .name("jellyfin-keyring".into())
            .spawn(move || {
                for job in receiver {
                    job();
                }
            });
        if let Err(error) = spawned {
            log::warn!("could not start the keyring's thread: {error}");
        }
        Mutex::new(sender)
    });
    let job = answered("the keyring", work, done);
    let sent = queue
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .send(job);
    if sent.is_err() {
        log::warn!("the keyring's thread has stopped");
    }
}

/// Threads the library's page loads share, so that a stalled server and a
/// user clicking on fill a few threads rather than one per request until
/// each times out. A job whose page was left meanwhile skips its request
/// (see `LibraryController::load`), so the queue empties fast.
const LOAD_THREADS: usize = 6;

#[derive(Default)]
struct LoadQueue {
    jobs: Mutex<(VecDeque<Job>, usize)>,
    ready: Condvar,
}

/// Runs `work` on one of the shared load threads, and then `done` with
/// its answer on the interface thread.
pub fn load<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) + 'static,
) {
    static QUEUE: OnceLock<Arc<LoadQueue>> = OnceLock::new();
    let queue = QUEUE.get_or_init(Arc::default);
    let job = answered("jellyfin", work, done);
    let mut jobs = queue
        .jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    jobs.0.push_back(job);
    // Threads start as they are needed, up to the limit, and stay.
    if jobs.1 < LOAD_THREADS {
        let worker = queue.clone();
        let spawned = std::thread::Builder::new()
            .name("jellyfin".into())
            .spawn(move || {
                loop {
                    let job = {
                        let mut jobs = worker
                            .jobs
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        loop {
                            if let Some(job) = jobs.0.pop_front() {
                                break job;
                            }
                            jobs = worker
                                .ready
                                .wait(jobs)
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                        }
                    };
                    job();
                }
            });
        match spawned {
            Ok(_) => jobs.1 += 1,
            Err(error) => log::warn!("could not start a Jellyfin thread: {error}"),
        }
    }
    drop(jobs);
    queue.ready.notify_one();
}

/// What playlists, history and mpv see of a Jellyfin item: its server and
/// its id, without a token and whatever stream the server picks.
pub const SCHEME: &str = "jellyfin";

/// Ids as the server writes them (32 hex digits) go into an entry as they
/// are; anything else could break the URL, and is refused.
fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// The playlist entry for an item: `jellyfin://<server id>/<item id>`.
pub fn entry_url(server_id: &str, item_id: &str) -> Option<String> {
    (plain_id(server_id) && plain_id(item_id)).then(|| format!("{SCHEME}://{server_id}/{item_id}"))
}

/// The server and item ids of a playlist entry `entry_url` made.
pub fn parse_entry(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix(SCHEME)?.strip_prefix("://")?;
    let (server, item) = rest.split_once('/')?;
    (plain_id(server) && plain_id(item)).then_some((server, item))
}

/// An item the library opened in a player window, for the load hook that
/// resolves its entry, and for the reports and music mode after it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ItemInfo {
    pub server_id: String,
    pub id: String,
    /// As the title bar and playlist show it.
    pub title: String,
    pub kind: ItemKind,
    /// Played as audio: through music mode, from the audio stream.
    pub audio: bool,
    /// Where to start, in ticks: the server's resume position when the
    /// item was opened, or 0 to play it from the beginning.
    pub resume_ticks: i64,
    /// The user chose to play it from the beginning, for the load it was
    /// opened for: the load hook takes it, and the entry opened again
    /// later starts where the server says. Otherwise the hook starts where
    /// the server says when it opens the file, which may be later than
    /// `resume_ticks` by then.
    pub from_start: bool,
    /// The version, audio and subtitle chosen on the item's page; None
    /// leaves the choice to the server.
    pub media_source_id: Option<String>,
    pub audio_stream_index: Option<i32>,
    pub subtitle_stream_index: Option<i32>,
    /// When it was opened, as a count: past `REGISTRY_LIMIT` the oldest
    /// entries go.
    pub opened: u64,
}

/// The items opened so far, by playlist entry. Shared with the threads
/// that load files, which may only block on it briefly.
pub type Registry = Arc<Mutex<HashMap<String, ItemInfo>>>;

/// The most entries the registry keeps. Each Play of a big show adds up to
/// `MAX_PLAYLIST` of them; an entry dropped while still in a playlist is
/// resolved from the server again, as one from History or Recent is.
pub const REGISTRY_LIMIT: usize = 20_000;

/// Notes items opened, newest last, dropping the oldest past the limit.
pub fn remember(registry: &mut HashMap<String, ItemInfo>, items: Vec<(String, ItemInfo)>) {
    let mut next = registry.values().map(|info| info.opened).max().unwrap_or(0);
    for (entry, mut info) in items {
        next += 1;
        info.opened = next;
        registry.insert(entry, info);
    }
    if registry.len() > REGISTRY_LIMIT {
        let mut ages: Vec<u64> = registry.values().map(|info| info.opened).collect();
        ages.sort_unstable();
        let cut = ages[registry.len() - REGISTRY_LIMIT];
        registry.retain(|_, info| info.opened >= cut);
    }
}

/// A signed-in account: the server as `jellyfin.json` keeps it, and a
/// client carrying the account's token.
#[derive(Debug, Clone)]
pub struct Account {
    pub server: Server,
    pub client: Client,
}

/// What changed in the session, for the windows.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// The sign-ins saved last time are back (or known to be gone).
    Loaded,
    SignedIn(String),
    /// The account is about to be signed out, its token still working:
    /// the windows playing from the server report where they stopped, and
    /// stop. `SignedOut` follows.
    SigningOut(String),
    SignedOut(String),
    /// The server no longer takes the account's token: the windows showing
    /// it ask to sign in again.
    Expired(String),
    /// The server took the report of a playback's end: what the library
    /// shows of that server (played marks, resume positions, Next Up) may
    /// have changed, less of it when what ended was played as audio.
    Played {
        server_id: String,
        audio: bool,
    },
}

pub trait Listener {
    fn session_changed(&self, change: &Change);
}

pub struct Session {
    store: RefCell<Store>,
    clients: Clients,
    accounts: RefCell<Vec<Account>>,
    /// Servers whose sign-in was lost, by a 401 or because no token was
    /// saved for them; signing in is asked again until the user cancels.
    expired: RefCell<Vec<String>>,
    /// The server chosen last, which new windows show.
    current: RefCell<Option<String>>,
    /// The saved tokens have been asked for.
    loaded: Cell<bool>,
    images_dir: PathBuf,
    /// The most the image cache may keep on disk, in bytes, from the
    /// preferences.
    image_limit: Cell<u64>,
    /// Made when first needed: its workers start with it.
    images: RefCell<Option<Images>>,
    registry: Registry,
    /// The accounts signed in, for the load hooks' threads.
    servers: Servers,
    /// Made when first needed: its thread starts with it.
    reporter: RefCell<Option<Reporter>>,
    listeners: RefCell<Vec<Weak<dyn Listener>>>,
    /// Whether the user has heard that the keyring couldn't keep a
    /// sign-in; once is enough.
    told_unsaved: Cell<bool>,
}

impl Session {
    /// The session for the data folder (`Paths::data`), with images kept
    /// in `images_dir`. Nothing is signed in until `start` has loaded the
    /// tokens.
    pub fn new(data_dir: &Path, images_dir: PathBuf) -> Rc<Self> {
        Rc::new(Self {
            store: RefCell::new(Store::load(data_dir)),
            clients: Clients::new(),
            accounts: RefCell::new(Vec::new()),
            expired: RefCell::new(Vec::new()),
            current: RefCell::new(None),
            loaded: Cell::new(false),
            images_dir,
            image_limit: Cell::new(images::Options::default().disk_limit),
            images: RefCell::new(None),
            registry: Registry::default(),
            servers: Servers::default(),
            reporter: RefCell::new(None),
            listeners: RefCell::new(Vec::new()),
            told_unsaved: Cell::new(false),
        })
    }

    /// Asks the keyring for each saved account's token, on a thread of its
    /// own since the keyring may block or ask to be unlocked, and signs in
    /// those it has. The windows hear `Change::Loaded` when it is done.
    pub fn start(self: &Rc<Self>) {
        let saved: Vec<Server> = self
            .store
            .borrow()
            .servers()
            .iter()
            .filter(|server| server.signed_in())
            .cloned()
            .collect();
        if saved.is_empty() {
            self.loaded.set(true);
            self.servers.loaded();
            return;
        }
        let weak = Rc::downgrade(self);
        keyring(
            move || {
                saved
                    .into_iter()
                    .map(|server| {
                        let token = store::load_token(&server.id, &server.user_id);
                        (server, token)
                    })
                    .collect::<Vec<_>>()
            },
            move |tokens| {
                let Some(session) = weak.upgrade() else {
                    return;
                };
                for (server, token) in tokens {
                    session.token_loaded(server, token);
                }
                session.loaded.set(true);
                session.servers.loaded();
                session.notify(&Change::Loaded);
            },
        );
    }

    /// A saved account's token, as the keyring gave it. The keyring may
    /// answer late (locked, waiting to be unlocked), and meanwhile the
    /// user may have signed in to the server by hand: that sign-in stays,
    /// as does a change of account made since.
    fn token_loaded(&self, server: Server, token: Result<Option<String>, String>) {
        let now = self.server(&server.id);
        if self.account(&server.id).is_some() || now.is_none_or(|now| now.user_id != server.user_id)
        {
            log::debug!("{} was signed in to meanwhile", server.name);
            return;
        }
        match token {
            Ok(Some(token)) => match self.add_account(server.clone(), &token) {
                Ok(Some(replaced)) => revoke(replaced, None),
                Ok(None) => {}
                Err(error) => log::warn!("could not sign in to {}: {error}", server.name),
            },
            Ok(None) => self.expire(&server.id),
            Err(error) => {
                log::warn!("could not read the sign-in for {}: {error}", server.name);
                self.expire(&server.id);
            }
        }
    }

    #[cfg(test)]
    pub fn loaded(&self) -> bool {
        self.loaded.get()
    }

    /// Whether signed-in servers are waiting for their tokens, which the
    /// windows show as the library loading rather than as the welcome
    /// screen.
    pub fn loading(&self) -> bool {
        !self.loaded.get() && self.store.borrow().servers().iter().any(Server::signed_in)
    }

    pub fn identity(&self) -> Identity {
        self.store.borrow().identity()
    }

    #[cfg(test)]
    pub fn clients(&self) -> Clients {
        self.clients.clone()
    }

    pub fn registry(&self) -> Registry {
        self.registry.clone()
    }

    pub fn servers(&self) -> Servers {
        self.servers.clone()
    }

    /// The title an entry is known by: as the library opened it, or as the
    /// load hook found it.
    pub fn entry_title(&self, entry: &str) -> Option<String> {
        self.registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(entry)
            .map(|info| info.title.clone())
            .filter(|title| !title.is_empty())
    }

    /// Sends a report about a playback to its server, with the account's
    /// sign-in of now; nothing once it is signed out.
    pub fn report(self: &Rc<Self>, server_id: &str, report: Report) {
        let Some(account) = self.account(server_id) else {
            log::debug!("no report: signed out of {server_id}");
            return;
        };
        let mut reporter = self.reporter.borrow_mut();
        if reporter.is_none() {
            match Reporter::start() {
                Ok((started, answered)) => {
                    let weak = Rc::downgrade(self);
                    let spawned = slint::spawn_local(async move {
                        while let Ok(sent) = answered.recv().await {
                            let Some(session) = weak.upgrade() else {
                                break;
                            };
                            session.reported(sent);
                        }
                    });
                    if let Err(error) = spawned {
                        log::warn!("could not wait for Jellyfin reports: {error}");
                    }
                    *reporter = Some(started);
                }
                Err(error) => {
                    log::warn!("could not start the Jellyfin reporter: {error}");
                    return;
                }
            }
        }
        if let Some(reporter) = reporter.as_ref() {
            reporter.send(server_id, &account.client, report);
        }
    }

    /// A report's answer: a 401 signs the account out, and an ended
    /// playback the server took tells the windows.
    fn reported(&self, sent: playback::Sent) {
        match sent.result {
            Err(Error::Unauthorized) => self.unauthorized(&sent.server_id, &sent.client),
            Ok(()) if sent.stopped => {
                let audio = self.played_as_audio(&sent.server_id, &sent.item_id);
                self.notify(&Change::Played {
                    server_id: sent.server_id,
                    audio,
                });
            }
            _ => {}
        }
    }

    /// Whether an item was played as audio, as the registry has it (the
    /// load hook notes what the library didn't open); not when it is
    /// unknown, so that the library loads all it might show of it again.
    fn played_as_audio(&self, server_id: &str, item_id: &str) -> bool {
        let Some(entry) = entry_url(server_id, item_id) else {
            return false;
        };
        self.registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&entry)
            .is_some_and(|info| info.audio)
    }

    /// Waits for the reports still on their way, at most until `deadline`:
    /// quitting, so that the server hears where playback stopped.
    pub fn flush_reports(&self, deadline: Instant) {
        if let Some(reporter) = self.reporter.borrow().as_ref()
            && !reporter.flush(deadline)
        {
            log::debug!("quitting with Jellyfin reports unsent");
        }
    }

    pub fn server(&self, id: &str) -> Option<Server> {
        self.store.borrow().server(id).cloned()
    }

    pub fn accounts(&self) -> Vec<Account> {
        self.accounts.borrow().clone()
    }

    pub fn account(&self, server_id: &str) -> Option<Account> {
        self.accounts
            .borrow()
            .iter()
            .find(|account| account.server.id == server_id)
            .cloned()
    }

    #[cfg(test)]
    pub fn signed_in(&self) -> bool {
        !self.accounts.borrow().is_empty()
    }

    /// The server a window shows when it has none of its own: the one
    /// chosen last while it is signed in, or else the first.
    pub fn default_server(&self) -> Option<String> {
        let current = self.current.borrow().clone();
        current.filter(|id| self.account(id).is_some()).or_else(|| {
            self.accounts
                .borrow()
                .first()
                .map(|account| account.server.id.clone())
        })
    }

    /// Makes `server_id` the server new windows show.
    pub fn choose(&self, server_id: &str) {
        *self.current.borrow_mut() = Some(server_id.to_owned());
    }

    /// The servers that asked to be signed in again, oldest first.
    pub fn expired(&self) -> Vec<Server> {
        let store = self.store.borrow();
        self.expired
            .borrow()
            .iter()
            .filter_map(|id| store.server(id).cloned())
            .collect()
    }

    /// The image cache, started the first time it is asked for.
    pub fn images(&self) -> Images {
        self.images
            .borrow_mut()
            .get_or_insert_with(|| {
                images::Images::new(
                    images::Options {
                        dir: Some(self.images_dir.clone()),
                        disk_limit: self.image_limit.get(),
                        ..Default::default()
                    },
                    self.clients.fetch(),
                )
            })
            .clone()
    }

    /// Changes how much the image cache may keep on disk.
    pub fn set_image_limit(&self, bytes: u64) {
        self.image_limit.set(bytes);
        if let Some(images) = self.images.borrow().as_ref() {
            images.set_disk_limit(bytes);
        }
    }

    /// Forgets every poster and backdrop, in memory and on disk: they show
    /// what a library holds, which an account signed out of shouldn't
    /// leave behind. The cache keeps every server's images together, by
    /// URL, so all of it goes; the other servers' come back as they show.
    fn clear_images(&self) {
        if let Some(images) = self.images.borrow().as_ref() {
            drop(images.clear());
            return;
        }
        // No cache yet: its folder is emptied without starting one.
        let dir = self.images_dir.clone();
        let spawned = std::thread::Builder::new()
            .name("jellyfin-image-files".into())
            .spawn(move || images::clear_folder(&dir));
        if let Err(error) = spawned {
            log::warn!("could not start a thread for the Jellyfin image cache: {error}");
        }
    }

    pub fn subscribe(&self, listener: Weak<dyn Listener>) {
        self.listeners.borrow_mut().push(listener);
    }

    /// Tells every window, with nothing of the session borrowed, since a
    /// window asks the session again.
    fn notify(&self, change: &Change) {
        let listeners: Vec<Rc<dyn Listener>> = {
            let mut listeners = self.listeners.borrow_mut();
            listeners.retain(|listener| listener.strong_count() > 0);
            listeners.iter().filter_map(Weak::upgrade).collect()
        };
        for listener in listeners {
            listener.session_changed(change);
        }
    }

    /// Signs `server`'s account in with `token`: keeps the server, makes
    /// its client and chooses it. The token is the caller's to save.
    /// Another account signed in on the same server before is returned, to
    /// be signed out.
    pub fn add_account(&self, server: Server, token: &str) -> Result<Option<Account>, Error> {
        let url = server
            .url()
            .ok_or_else(|| Error::Address(format!("“{}” is not a web address.", server.address)))?;
        let client = Client::new(url, self.identity(), server.ca_file.as_deref())?
            .with_session(token, &server.user_id);
        self.clients.insert(client.clone());
        self.servers.set(playback::Server {
            id: server.id.clone(),
            client: client.clone(),
            ca_file: server.ca_file.clone(),
        });
        self.store.borrow_mut().add(server.clone());
        self.expired.borrow_mut().retain(|id| *id != server.id);
        *self.current.borrow_mut() = Some(server.id.clone());
        let mut accounts = self.accounts.borrow_mut();
        let replaced = accounts
            .iter()
            .position(|account| account.server.id == server.id)
            .map(|at| accounts.remove(at))
            .filter(|old| old.server.user_id != server.user_id);
        accounts.push(Account { server, client });
        Ok(replaced)
    }

    /// Forgets a server's account and its client. With `keep_user` the
    /// account stays in `jellyfin.json`, so that the next start asks for it
    /// again; otherwise only the server stays.
    fn remove_account(&self, server_id: &str, keep_user: bool) -> Option<Account> {
        let account = {
            let mut accounts = self.accounts.borrow_mut();
            let at = accounts
                .iter()
                .position(|account| account.server.id == server_id)?;
            accounts.remove(at)
        };
        self.clients.remove(account.client.base());
        self.servers.remove(server_id);
        if !keep_user {
            self.store.borrow_mut().update(server_id, |server| {
                server.user_id.clear();
                server.user_name.clear();
            });
        }
        Some(account)
    }

    fn expire(&self, server_id: &str) {
        let mut expired = self.expired.borrow_mut();
        if !expired.iter().any(|id| id == server_id) {
            expired.push(server_id.to_owned());
        }
    }

    /// The end of signing in: the account is in, its token goes to the
    /// keyring on a thread of its own, and the windows hear of it. When
    /// the keyring can't keep it, the token lasts as long as Patina runs,
    /// the reason is logged, and `unsaved` is called, the first time only.
    pub fn signed_in_with(
        self: &Rc<Self>,
        server: Server,
        token: String,
        unsaved: impl FnOnce() + 'static,
    ) -> Result<(), Error> {
        let replaced = self.add_account(server.clone(), &token)?;
        if let Some(old) = replaced {
            revoke(old, None);
        }
        let weak = Rc::downgrade(self);
        keyring(
            move || store::save_token(&server.id, &server.user_id, &token),
            move |saved| {
                if let Err(error) = saved {
                    log::warn!("could not save the Jellyfin sign-in: {error}");
                    if let Some(session) = weak.upgrade()
                        && !session.told_unsaved.replace(true)
                    {
                        unsaved();
                    }
                }
            },
        );
        let id = self.current.borrow().clone().unwrap_or_default();
        self.notify(&Change::SignedIn(id));
        Ok(())
    }

    /// Signs out of a server: what plays from it stops, the server revokes
    /// the token once the reports on their way have gone, the keyring
    /// forgets it, and the server stays without an account.
    pub fn sign_out(&self, server_id: &str) {
        if self.account(server_id).is_none() {
            return;
        }
        self.notify(&Change::SigningOut(server_id.to_owned()));
        let Some(account) = self.remove_account(server_id, false) else {
            return;
        };
        self.expired.borrow_mut().retain(|id| id != server_id);
        self.forget_items(server_id);
        self.clear_images();
        let reports = self.reporter.borrow().as_ref().map(Reporter::pending);
        revoke(account, reports);
        self.notify(&Change::SignedOut(server_id.to_owned()));
    }

    /// The servers Patina knows that have no account signed in now, to
    /// sign in to again.
    /// The CA file of the saved server at `address`, as typed or as
    /// Patina keeps it: what signing in to it again trusts, and keeps.
    /// There is no setting for it; it is added to the server's entry in
    /// `jellyfin.json` by hand.
    pub fn ca_file_at(&self, address: &str) -> Option<PathBuf> {
        let wanted = auth::normalize_address(address).ok()?;
        self.store
            .borrow()
            .servers()
            .iter()
            .find(|server| auth::normalize_address(&server.address).is_ok_and(|url| url == wanted))
            .and_then(|server| server.ca_file.clone())
    }

    pub fn known_servers(&self) -> Vec<Server> {
        self.store
            .borrow()
            .servers()
            .iter()
            .filter(|server| self.account(&server.id).is_none())
            .cloned()
            .collect()
    }

    /// A request made with `failed` was answered 401: the token is gone,
    /// signed out elsewhere, the password changed or the device removed.
    /// Patina forgets it and asks to sign in again. A request still under
    /// way when the user signed in again carries the old token, which the
    /// new sign-in revoked; its 401 says nothing about the new one.
    pub fn unauthorized(&self, server_id: &str, failed: &Client) {
        if !self.is_current(server_id, failed) {
            log::debug!("a 401 for a sign-in replaced since; ignored");
            return;
        }
        let Some(account) = self.remove_account(server_id, true) else {
            return;
        };
        log::info!("{} no longer accepts the sign-in", account.server.name);
        self.expire(server_id);
        forget_token(account);
        self.notify(&Change::Expired(server_id.to_owned()));
    }

    /// Whether a request made with `client` was made with the server's
    /// sign-in of now (or the server has none).
    fn is_current(&self, server_id: &str, client: &Client) -> bool {
        self.account(server_id)
            .is_none_or(|account| account.client.token() == client.token())
    }

    /// Forgets what the library opened from a server.
    fn forget_items(&self, server_id: &str) {
        self.registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, info| info.server_id != server_id);
    }

    /// The user declined signing in again: the server stays, without an
    /// account, and isn't asked about again.
    pub fn forget_expired(&self, server_id: &str) {
        let had = {
            let mut expired = self.expired.borrow_mut();
            let before = expired.len();
            expired.retain(|id| id != server_id);
            expired.len() != before
        };
        if had && self.account(server_id).is_none() {
            // The token may still be in the keyring, and valid, when it
            // was reading the keyring that failed; nothing would ever ask
            // for it again.
            if let Some(server) = self.server(server_id)
                && server.signed_in()
            {
                delete_token(server.id, server.user_id);
            }
            self.store.borrow_mut().update(server_id, |server| {
                server.user_id.clear();
                server.user_name.clear();
            });
            self.clear_images();
        }
    }

    /// A client for a server being signed in to, for its accounts'
    /// pictures. A signed-in client at the same address is left in place.
    fn add_guest(&self, client: &Client) {
        if self
            .clients
            .for_url(client.base())
            .is_none_or(|known| known.token().is_none())
        {
            self.clients.insert(client.clone());
        }
    }

    /// Takes the guest client away again when nothing signed in uses it.
    fn remove_guest(&self, client: &Client) {
        let signed_in = self
            .accounts
            .borrow()
            .iter()
            .any(|account| account.client.base() == client.base());
        if !signed_in {
            self.clients.remove(client.base());
        }
    }
}

/// Deletes an account's token from the keyring, after whatever the keyring
/// was asked before.
fn forget_token(account: Account) {
    delete_token(account.server.id, account.server.user_id);
}

fn delete_token(server_id: String, user_id: String) {
    keyring(
        move || store::delete_token(&server_id, &user_id),
        |deleted| {
            if let Err(error) = deleted {
                log::warn!("could not delete the Jellyfin sign-in: {error}");
            }
        },
    );
}

/// Signs an account out for good: the server revokes its token, and the
/// keyring forgets it. Only forgetting it would leave the token valid on
/// the server, where only an administrator could remove it. With
/// `reports`, the server is asked once they have gone (or a while has
/// passed): they carry the token too.
fn revoke(account: Account, reports: Option<playback::Pending>) {
    let client = account.client.clone();
    background(
        "jellyfin",
        move || {
            if let Some(reports) = reports
                && !reports.wait(Instant::now() + SIGN_OUT_REPORTS)
            {
                log::debug!("signing out with Jellyfin reports unsent");
            }
            if let Err(error) = client.logout() {
                log::debug!("signing out of Jellyfin: {error}");
            }
        },
        |()| {},
    );
    forget_token(account);
}

// Signing in.

/// Where signing in is up to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Step {
    /// Typing the server's address, or scanning the network for one.
    #[default]
    Address,
    /// Picking one of the accounts the server lists, or typing a name.
    Users,
    Password,
    /// Showing a code to approve in another Jellyfin app.
    QuickConnect,
}

impl Step {
    /// As `LibrarySignIn.step` names it.
    pub fn as_str(self) -> &'static str {
        match self {
            Step::Address => "address",
            Step::Users => "users",
            Step::Password => "password",
            Step::QuickConnect => "quick-connect",
        }
    }
}

/// A server Scan Network found: the address picking it fills in, and the
/// line under its name, which says where else it answered.
#[derive(Debug, Clone, PartialEq)]
pub struct FoundAddress {
    pub name: String,
    pub address: String,
    pub detail: String,
}

impl FoundAddress {
    /// One row per server. The address it fills in is the one most likely
    /// to work (`discovery::Found::addresses`); the other, when it differs,
    /// is named in the detail, as where the server says it is or where
    /// its answer came from.
    pub fn of(found: &discovery::Found) -> Self {
        let addresses = found.addresses();
        let address = addresses.first().cloned().unwrap_or_default();
        let detail = match addresses.get(1) {
            Some(other) if *other == found.source_address => {
                format!("{address} · answered from {other}")
            }
            Some(other) => format!("{address} · the server says {other}"),
            None => address.clone(),
        };
        Self {
            name: found.name.clone(),
            address,
            detail,
        }
    }
}

/// What the sign-in screen shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SignInState {
    pub step: Step,
    pub address: String,
    pub server_name: String,
    pub server_version: String,
    /// Plain http: the password and token travel unencrypted.
    pub insecure: bool,
    pub users: Vec<UserDto>,
    /// The account chosen or typed.
    pub user: String,
    pub quick_connect: bool,
    pub code: String,
    pub scanning: bool,
    pub found: Vec<FoundAddress>,
    /// Servers Patina knows without an account signed in now.
    pub saved: Vec<Server>,
    pub working: bool,
    pub message: String,
    /// Not an error: why signing in is asked again, or that the server is
    /// signed in already, as whom.
    pub note: String,
}

/// What a server said when it was connected to, before signing in.
pub struct Connected {
    pub client: Client,
    pub info: PublicSystemInfo,
    pub users: Vec<UserDto>,
    pub quick_connect: bool,
}

/// Finds the server at `address` (`api::probe`, which keeps the address
/// it ends at) and what its sign-in screen offers, trusting `ca_file`'s
/// certificates when given. A server may list no accounts or have Quick
/// Connect off; signing in with a typed name works either way, so neither
/// failing stops it. Blocks.
pub fn connect(
    address: &str,
    identity: &Identity,
    ca_file: Option<&Path>,
) -> Result<Connected, Error> {
    let probed = api::probe(address, identity, ca_file)?;
    let client = Client::new(probed.address, identity.clone(), ca_file)?;
    let users = client.public_users().unwrap_or_else(|error| {
        log::debug!("the server lists no accounts: {error}");
        Vec::new()
    });
    let quick_connect = client.quick_connect_enabled().unwrap_or(false);
    Ok(Connected {
        client,
        info: probed.info,
        users,
        quick_connect,
    })
}

/// The server to keep for a sign-in's answer, made through a client that
/// trusts `ca_file`.
pub fn server_for(
    client: &Client,
    info: &PublicSystemInfo,
    auth: &AuthenticationResult,
    ca_file: Option<PathBuf>,
) -> Server {
    Server {
        id: if info.id.is_empty() {
            auth.server_id.clone()
        } else {
            info.id.clone()
        },
        name: info.server_name.clone(),
        address: auth::display_address(client.base()),
        version: info.version.clone(),
        ca_file,
        user_id: auth.user.id.clone(),
        user_name: auth.user.name.clone(),
    }
}

/// What a sign-in flow tells its window.
pub enum SignInEvent {
    Changed(Box<SignInState>),
    /// Signed in to the server with this id.
    SignedIn(String),
    /// The keyring couldn't keep the sign-in (why is in the log): it lasts
    /// as long as Patina runs.
    Unsaved,
}

/// Signing in, in one window: each step's network work on a thread of its
/// own, and its answer dropped when the user has moved on (a request
/// counter, as `search_online` keeps).
pub struct SignIn {
    session: Weak<Session>,
    state: RefCell<SignInState>,
    /// The server once it has answered, with no token yet.
    server: RefCell<Option<(Client, PublicSystemInfo)>>,
    request: Cell<u64>,
    secret: RefCell<String>,
    poll: Timer,
    /// When the Quick Connect code was given.
    code_since: Cell<Option<Instant>>,
    polling: Cell<bool>,
    /// Signing in again as this account, once the server has answered
    /// (`renew`).
    renewing: RefCell<Option<String>>,
    /// The CA file of the saved server at the address connected to, which
    /// the connection trusts and the server keeps (`Session::ca_file_at`).
    ca_file: RefCell<Option<PathBuf>>,
    /// Shared with the keyring's answer, which comes after the flow has
    /// ended.
    events: Rc<dyn Fn(SignInEvent)>,
    this: Weak<SignIn>,
}

impl SignIn {
    pub fn new(session: &Rc<Session>, events: impl Fn(SignInEvent) + 'static) -> Rc<Self> {
        Rc::new_cyclic(|this| Self {
            session: Rc::downgrade(session),
            state: RefCell::new(SignInState::default()),
            server: RefCell::new(None),
            request: Cell::new(0),
            secret: RefCell::new(String::new()),
            poll: Timer::default(),
            code_since: Cell::new(None),
            polling: Cell::new(false),
            renewing: RefCell::new(None),
            ca_file: RefCell::new(None),
            events: Rc::new(events),
            this: this.clone(),
        })
    }

    pub fn state(&self) -> SignInState {
        self.state.borrow().clone()
    }

    fn change(&self, change: impl FnOnce(&mut SignInState)) {
        let state = {
            let mut state = self.state.borrow_mut();
            change(&mut state);
            state.clone()
        };
        (self.events)(SignInEvent::Changed(Box::new(state)));
    }

    /// A new request, which makes the answers to earlier ones stale.
    fn next_request(&self) -> u64 {
        let request = self.request.get() + 1;
        self.request.set(request);
        request
    }

    /// Runs `work` in the background, and `done` with its answer unless
    /// another request has been made since.
    fn ask<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
        done: impl FnOnce(&SignIn, T) + 'static,
    ) {
        let request = self.next_request();
        let weak = self.this.clone();
        background("jellyfin-sign-in", work, move |answer| {
            if let Some(this) = weak.upgrade()
                && this.request.get() == request
            {
                done(&this, answer);
            }
        });
    }

    /// Starts afresh at the address, listing the servers signed in to
    /// before.
    pub fn begin(&self) {
        self.stop();
        let saved = self
            .session
            .upgrade()
            .map(|session| session.known_servers())
            .unwrap_or_default();
        self.change(|state| {
            *state = SignInState {
                saved,
                ..Default::default()
            }
        });
    }

    /// Signs in to a server Patina knows again: it connects at once, and
    /// goes on to the password of the account kept for it (or to the
    /// accounts, when none is), with `note` saying why.
    pub fn renew(&self, server: &Server, note: &str) {
        self.begin();
        self.change(|state| {
            state.address = server.address.clone();
            state.user = server.user_name.clone();
            state.note = note.to_owned();
        });
        self.renewing.replace(Some(server.user_name.clone()));
        self.connect(&server.address);
    }

    /// Asks the server at `address` what it is and who may sign in.
    pub fn connect(&self, address: &str) {
        let Some(session) = self.session.upgrade() else {
            return;
        };
        let address = address.trim().to_owned();
        if address.is_empty() {
            self.change(|state| state.message = "Enter the server's address.".into());
            return;
        }
        self.change(|state| {
            state.address = address.clone();
            state.working = true;
            state.message.clear();
        });
        let identity = session.identity();
        let ca_file = session.ca_file_at(&address);
        self.ca_file.replace(ca_file.clone());
        self.ask(
            move || connect(&address, &identity, ca_file.as_deref()),
            |this, connected| match connected {
                Ok(connected) => this.connected(connected),
                Err(error) => {
                    this.renewing.take();
                    this.change(|state| {
                        state.working = false;
                        state.message = error.to_string();
                    })
                }
            },
        );
    }

    fn connected(&self, connected: Connected) {
        let session = self.session.upgrade();
        if let Some(session) = &session {
            session.add_guest(&connected.client);
        }
        let insecure = connected.client.base().scheme() == "http";
        let address = auth::display_address(connected.client.base());
        // Adding a server signed in already (perhaps at another address)
        // replaces its account; the user hears so before choosing one.
        let signed_in = session
            .as_ref()
            .and_then(|session| session.account(&connected.info.id))
            .map(|account| {
                let name = &account.server.user_name;
                format!(
                    "You're signed in to {} as {name}. Signing in as someone else signs {name} out.",
                    connected.info.server_name
                )
            });
        let renewing = self.renewing.take().filter(|user| !user.is_empty());
        self.change(|state| {
            state.step = match &renewing {
                Some(user) => {
                    state.user = user.clone();
                    Step::Password
                }
                None => Step::Users,
            };
            state.address = address;
            state.server_name = connected.info.server_name.clone();
            state.server_version = connected.info.version.clone();
            state.insecure = insecure;
            state.users = connected.users;
            state.quick_connect = connected.quick_connect;
            state.working = false;
            state.message.clear();
            if let Some(note) = signed_in {
                state.note = note;
            }
        });
        *self.server.borrow_mut() = Some((connected.client, connected.info));
    }

    /// Scan Network: what answers within two seconds is listed, to fill in
    /// the address with; the user still connects to it.
    pub fn scan(&self) {
        self.change(|state| {
            state.scanning = true;
            state.found.clear();
            state.message.clear();
        });
        // Not `ask`: a scan runs beside connecting, and neither makes the
        // other's answer stale.
        let weak = self.this.clone();
        background(
            "jellyfin-scan",
            || discovery::scan(discovery::SCAN_TIME),
            move |found| {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                this.change(|state| {
                    state.scanning = false;
                    match found {
                        Ok(servers) => {
                            state.found = servers.iter().map(FoundAddress::of).collect();
                            if state.found.is_empty() {
                                state.message =
                                    "No Jellyfin server answered on this network.".into();
                            }
                        }
                        Err(error) => {
                            state.message = format!("Could not scan the network: {error}");
                        }
                    }
                });
            },
        );
    }

    /// Fills in the address of a server Scan Network found.
    pub fn pick_found(&self, address: &str) {
        self.change(|state| {
            state.address = address.to_owned();
            state.message.clear();
        });
    }

    /// Chooses one of the listed accounts, by id, or a typed name. An
    /// account without a password signs in at once.
    pub fn pick_user(&self, id: &str) {
        let listed = self
            .state
            .borrow()
            .users
            .iter()
            .find(|user| user.id == id)
            .cloned();
        match listed {
            Some(user) if !user.has_password => self.password(&user.name, ""),
            Some(user) => self.change(|state| {
                state.step = Step::Password;
                state.user = user.name;
                state.message.clear();
            }),
            None => self.change(|state| {
                state.step = Step::Password;
                state.user = id.trim().to_owned();
                state.message.clear();
            }),
        }
    }

    /// Signs in with a name (the chosen account's when empty) and password.
    pub fn password(&self, name: &str, password: &str) {
        let name = match name.trim() {
            "" => self.state.borrow().user.clone(),
            name => name.to_owned(),
        };
        if name.is_empty() {
            self.change(|state| state.message = "Enter the account's name.".into());
            return;
        }
        let Some((client, info)) = self.server.borrow().clone() else {
            return;
        };
        self.change(|state| {
            state.user = name.clone();
            state.working = true;
            state.message.clear();
        });
        let password = password.to_owned();
        self.ask(
            move || {
                client
                    .authenticate_by_name(&name, &password)
                    .map(|auth| (client, auth))
            },
            move |this, signed_in| match signed_in {
                Ok((client, auth)) => this.finish(&client, &info, auth),
                Err(error) => this.change(|state| {
                    state.working = false;
                    state.message = error.to_string();
                }),
            },
        );
    }

    /// Asks for a Quick Connect code, then whether it has been approved
    /// every five seconds, for as long as the code lasts.
    pub fn quick_connect(&self) {
        let Some((client, _)) = self.server.borrow().clone() else {
            return;
        };
        self.change(|state| {
            state.working = true;
            state.message.clear();
        });
        self.ask(
            move || client.quick_connect_initiate(),
            |this, started| match started {
                Ok(started) => {
                    *this.secret.borrow_mut() = started.secret.clone();
                    this.code_since.set(Some(Instant::now()));
                    this.polling.set(false);
                    this.change(|state| {
                        state.step = Step::QuickConnect;
                        state.code = started.code.clone();
                        state.working = false;
                    });
                    let weak = this.this.clone();
                    this.poll
                        .start(TimerMode::Repeated, QUICK_CONNECT_POLL, move || {
                            if let Some(this) = weak.upgrade() {
                                this.poll_quick_connect();
                            }
                        });
                }
                Err(error) => this.change(|state| {
                    state.working = false;
                    state.message = error.to_string();
                }),
            },
        );
    }

    fn poll_quick_connect(&self) {
        if self
            .code_since
            .get()
            .is_none_or(|since| since.elapsed() >= QUICK_CONNECT_LIFETIME)
        {
            self.stop();
            self.change(|state| {
                state.step = Step::Users;
                state.code.clear();
                state.message = "The Quick Connect code ran out. Ask for a new one.".into();
            });
            return;
        }
        if self.polling.replace(true) {
            return;
        }
        let Some((client, info)) = self.server.borrow().clone() else {
            return;
        };
        let secret = self.secret.borrow().clone();
        let request = self.request.get();
        let weak = self.this.clone();
        background(
            "jellyfin-sign-in",
            move || {
                let approved = client.quick_connect_connect(&secret)?;
                if !approved.authenticated {
                    return Ok(None);
                }
                client
                    .quick_connect_authenticate(&secret)
                    .map(|auth| Some((client, auth)))
            },
            move |answer: Result<Option<(Client, AuthenticationResult)>, Error>| {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                this.polling.set(false);
                if this.request.get() != request {
                    return;
                }
                match answer {
                    Ok(None) => {}
                    Ok(Some((client, auth))) => {
                        this.stop();
                        this.finish(&client, &info, auth);
                    }
                    // Quick Connect turned off, or the code ran out.
                    Err(error @ Error::SignIn(_)) => {
                        this.stop();
                        this.change(|state| {
                            state.step = Step::Users;
                            state.code.clear();
                            state.message = error.to_string();
                        });
                    }
                    // Most likely a moment's trouble: asked again next time.
                    Err(error) => this.change(|state| state.message = error.to_string()),
                }
            },
        );
    }

    fn finish(&self, client: &Client, info: &PublicSystemInfo, auth: AuthenticationResult) {
        let Some(session) = self.session.upgrade() else {
            return;
        };
        let server = server_for(client, info, &auth, self.ca_file.borrow().clone());
        let id = server.id.clone();
        let events = self.events.clone();
        let signed_in = session.signed_in_with(server, auth.access_token, move || {
            events(SignInEvent::Unsaved);
        });
        match signed_in {
            Ok(()) => {
                self.server.borrow_mut().take();
                self.change(|state| {
                    state.working = false;
                    state.message.clear();
                });
                (self.events)(SignInEvent::SignedIn(id));
            }
            Err(error) => self.change(|state| {
                state.working = false;
                state.message = error.to_string();
            }),
        }
    }

    /// Steps back: from the code or the password to the accounts, and from
    /// the accounts to the address. False at the address, which leaves
    /// signing in to the caller.
    pub fn back(&self) -> bool {
        let step = self.state.borrow().step;
        match step {
            Step::Address => false,
            Step::QuickConnect | Step::Password => {
                self.stop();
                self.change(|state| {
                    state.step = Step::Users;
                    state.code.clear();
                    state.working = false;
                    state.message.clear();
                });
                true
            }
            Step::Users => {
                self.stop();
                self.forget_server();
                self.change(|state| {
                    state.step = Step::Address;
                    state.users.clear();
                    state.user.clear();
                    state.server_name.clear();
                    state.server_version.clear();
                    state.insecure = false;
                    state.quick_connect = false;
                    state.working = false;
                    state.message.clear();
                    state.note.clear();
                });
                true
            }
        }
    }

    /// Stops whatever is under way, its answers dropped.
    fn stop(&self) {
        self.next_request();
        self.renewing.take();
        self.poll.stop();
        self.polling.set(false);
        self.code_since.set(None);
        self.secret.borrow_mut().clear();
    }

    fn forget_server(&self) {
        if let Some((client, _)) = self.server.borrow_mut().take()
            && let Some(session) = self.session.upgrade()
        {
            session.remove_guest(&client);
        }
    }

    /// Gives up signing in.
    pub fn cancel(&self) {
        self.stop();
        self.forget_server();
    }
}

impl Drop for SignIn {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of the test's own, removed when the test ends.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "patina-jellyfin-session-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn session(folder: &Folder) -> Rc<Session> {
        Session::new(&folder.0, folder.0.join("images"))
    }

    fn server(id: &str, user: &str) -> Server {
        Server {
            id: id.into(),
            name: format!("Server {id}"),
            address: format!("https://{id}.example.org"),
            version: "12.4.0".into(),
            user_id: user.into(),
            user_name: format!("name of {user}"),
            ..Default::default()
        }
    }

    #[test]
    fn entries_name_the_server_and_the_item() {
        let url = entry_url("a1b2", "c3d4").unwrap();
        assert_eq!(url, "jellyfin://a1b2/c3d4");
        assert_eq!(parse_entry(&url), Some(("a1b2", "c3d4")));
        assert_eq!(entry_url("a/b", "c"), None);
        assert_eq!(entry_url("", "c"), None);
        assert_eq!(parse_entry("jellyfin://a1b2/c3d4/more"), None);
        assert_eq!(parse_entry("http://a1b2/c3d4"), None);
        assert_eq!(parse_entry("jellyfin://a1b2"), None);
    }

    #[test]
    fn accounts_come_and_go_with_the_store() {
        let folder = Folder::new("accounts");
        let session = session(&folder);
        assert!(!session.signed_in());
        assert_eq!(session.default_server(), None);

        assert!(
            session
                .add_account(server("s1", "u1"), "token-1")
                .unwrap()
                .is_none()
        );
        assert!(
            session
                .add_account(server("s2", "u2"), "token-2")
                .unwrap()
                .is_none()
        );
        // The last signed in is the one chosen.
        assert_eq!(session.default_server().as_deref(), Some("s2"));
        session.choose("s1");
        assert_eq!(session.default_server().as_deref(), Some("s1"));
        let account = session.account("s1").unwrap();
        assert_eq!(account.client.token(), Some("token-1"));
        assert_eq!(account.client.user_id(), Some("u1"));
        let image = Url::parse("https://s1.example.org/Items/x/Images/Primary").unwrap();
        assert_eq!(
            session.clients().for_url(&image).unwrap().token(),
            Some("token-1")
        );

        // Another account on the same server replaces the first, which is
        // handed back to be signed out.
        let replaced = session.add_account(server("s1", "u9"), "token-9").unwrap();
        assert_eq!(replaced.unwrap().client.token(), Some("token-1"));
        assert_eq!(session.accounts().len(), 2);
        // Signing in again as the same account replaces nothing to sign out.
        assert!(
            session
                .add_account(server("s1", "u9"), "token-10")
                .unwrap()
                .is_none()
        );

        // A 401 keeps the account in the file, to be asked for again.
        session.remove_account("s1", true);
        session.expire("s1");
        assert!(session.account("s1").is_none());
        assert!(session.clients().for_url(&image).is_none());
        assert_eq!(session.default_server().as_deref(), Some("s2"));
        assert_eq!(session.expired()[0].user_id, "u9");
        let reloaded = Store::load(&folder.0);
        assert!(reloaded.server("s1").unwrap().signed_in());
        // Declining to sign in again forgets the account, not the server.
        session.forget_expired("s1");
        assert!(session.expired().is_empty());
        let reloaded = Store::load(&folder.0);
        assert!(!reloaded.server("s1").unwrap().signed_in());
        assert_eq!(reloaded.servers().len(), 2);

        // Signing out keeps the server without its account.
        session.remove_account("s2", false);
        assert!(!session.signed_in());
        assert!(!Store::load(&folder.0).server("s2").unwrap().signed_in());
        // None of it started an image cache, whose results come back on an
        // event loop (which macOS lets only the main thread make).
        assert!(session.images.borrow().is_none());
    }

    #[test]
    fn a_servers_ca_file_goes_with_its_address() {
        let folder = Folder::new("ca-file");
        {
            let mut store = Store::load(&folder.0);
            store.add(Server {
                address: "https://s1.example.org:8920/jellyfin".into(),
                ca_file: Some(PathBuf::from("/srv/ca.pem")),
                ..server("s1", "")
            });
            store.add(server("s2", ""));
        }
        let session = session(&folder);
        // As kept, and as the user may type it again.
        for typed in [
            "https://s1.example.org:8920/jellyfin",
            "s1.example.org:8920/jellyfin/",
            "https://S1.example.org:8920/jellyfin?x=1#/home",
        ] {
            assert_eq!(
                session.ca_file_at(typed).as_deref(),
                Some(Path::new("/srv/ca.pem")),
                "{typed}"
            );
        }
        // Another address of the same host, a server without one, and
        // what isn't an address.
        assert_eq!(session.ca_file_at("https://s1.example.org"), None);
        assert_eq!(session.ca_file_at("https://s2.example.org"), None);
        assert_eq!(session.ca_file_at(""), None);
    }

    #[test]
    fn a_saved_account_waits_for_its_token() {
        let folder = Folder::new("loading");
        {
            let mut store = Store::load(&folder.0);
            store.add(server("s1", ""));
        }
        assert!(!session(&folder).loading());
        Store::load(&folder.0).add(server("s1", "u1"));
        let session = session(&folder);
        assert!(session.loading() && !session.loaded());
    }

    #[test]
    fn guests_never_replace_a_signed_in_client() {
        let folder = Folder::new("guests");
        let session = session(&folder);
        session.add_account(server("s1", "u1"), "token").unwrap();
        let identity = session.identity();
        let guest = Client::new(
            Url::parse("https://s1.example.org").unwrap(),
            identity.clone(),
            None,
        )
        .unwrap();
        session.add_guest(&guest);
        session.remove_guest(&guest);
        let url = Url::parse("https://s1.example.org/UserImage").unwrap();
        assert_eq!(
            session.clients().for_url(&url).unwrap().token(),
            Some("token")
        );

        let other = Client::new(Url::parse("http://nas:8096").unwrap(), identity, None).unwrap();
        session.add_guest(&other);
        let url = Url::parse("http://nas:8096/UserImage").unwrap();
        assert!(session.clients().for_url(&url).is_some());
        session.remove_guest(&other);
        assert!(session.clients().for_url(&url).is_none());
    }

    fn user(id: &str, has_password: bool) -> UserDto {
        UserDto {
            id: id.into(),
            name: format!("name {id}"),
            has_password,
            ..Default::default()
        }
    }

    #[test]
    fn signing_in_steps_forward_and_back() {
        let folder = Folder::new("steps");
        let session = session(&folder);
        let events: Rc<RefCell<Vec<SignInState>>> = Rc::default();
        let seen = events.clone();
        let flow = SignIn::new(&session, move |event| {
            if let SignInEvent::Changed(state) = event {
                seen.borrow_mut().push(*state);
            }
        });
        flow.begin();
        flow.change(|state| state.address = "nas.local".into());
        assert_eq!(flow.state().address, "nas.local");
        assert!(!flow.back(), "the address is the first step");

        // As if the server had answered.
        flow.change(|state| {
            state.step = Step::Users;
            state.users = vec![user("u1", true), user("u2", false)];
        });
        flow.pick_user("u1");
        assert_eq!(flow.state().step, Step::Password);
        assert_eq!(flow.state().user, "name u1");
        assert!(flow.back());
        assert_eq!(flow.state().step, Step::Users);
        // A typed name rather than a listed account.
        flow.pick_user("  someone ");
        assert_eq!(flow.state().user, "someone");
        flow.back();
        // Without a name there is nothing to sign in as.
        flow.change(|state| state.user.clear());
        flow.password(" ", "secret");
        assert_eq!(flow.state().message, "Enter the account's name.");
        assert!(flow.back());
        assert_eq!(flow.state().step, Step::Address);
        assert!(flow.state().users.is_empty());
        assert_eq!(flow.state().address, "nas.local");
        assert_eq!(events.borrow().last().unwrap(), &flow.state());

        flow.pick_found("http://192.168.1.20:8096");
        assert_eq!(flow.state().address, "http://192.168.1.20:8096");
        flow.connect("  ");
        assert_eq!(flow.state().message, "Enter the server's address.");
        assert_eq!(Step::QuickConnect.as_str(), "quick-connect");
    }

    #[test]
    fn signing_in_again_offers_the_servers_known() {
        let folder = Folder::new("known");
        let session = session(&folder);
        session.add_account(server("s1", "u1"), "token").unwrap();
        session.store.borrow_mut().add(server("s2", "u2"));
        let flow = SignIn::new(&session, |_| {});
        flow.begin();
        let saved: Vec<String> = flow
            .state()
            .saved
            .into_iter()
            .map(|server| server.id)
            .collect();
        assert_eq!(saved, ["s2"], "the server signed in is not offered");
    }

    #[test]
    fn a_found_server_is_one_row() {
        let found = discovery::Found {
            id: "a1".into(),
            name: "Den".into(),
            address: "http://172.17.0.2:8096".into(),
            source_address: "http://192.168.1.20:8096".into(),
            source: "192.168.1.20".parse().unwrap(),
            endpoint_address: None,
        };
        let row = FoundAddress::of(&found);
        assert_eq!(row.address, "http://192.168.1.20:8096");
        assert_eq!(
            row.detail,
            "http://192.168.1.20:8096 · the server says http://172.17.0.2:8096"
        );
        let named = discovery::Found {
            address: "https://media.example.org".into(),
            ..found.clone()
        };
        let row = FoundAddress::of(&named);
        assert_eq!(row.address, "https://media.example.org");
        assert!(
            row.detail
                .ends_with("answered from http://192.168.1.20:8096")
        );
        let same = discovery::Found {
            address: found.source_address.clone(),
            ..found
        };
        assert_eq!(FoundAddress::of(&same).detail, "http://192.168.1.20:8096");
    }

    #[test]
    fn a_late_401_leaves_a_newer_sign_in_alone() {
        let folder = Folder::new("late");
        let session = session(&folder);
        session.add_account(server("s1", "u1"), "old").unwrap();
        let old = session.account("s1").unwrap().client;
        session.add_account(server("s1", "u1"), "new").unwrap();
        let new = session.account("s1").unwrap().client;
        assert!(
            !session.is_current("s1", &old),
            "the old token's 401 is ignored"
        );
        assert!(session.is_current("s1", &new));
        assert!(session.is_current("s2", &old), "no sign-in to keep");
    }

    #[test]
    fn a_slow_keyring_leaves_a_sign_in_made_meanwhile() {
        let folder = Folder::new("slow");
        let session = session(&folder);
        Store::load(&folder.0);
        session.store.borrow_mut().add(server("s1", "u1"));
        let saved = session.server("s1").unwrap();
        // Signed in by hand while the keyring was asked.
        session.add_account(server("s1", "u2"), "by-hand").unwrap();
        session.token_loaded(saved.clone(), Ok(Some("saved".into())));
        assert_eq!(
            session.account("s1").unwrap().client.token(),
            Some("by-hand")
        );
        session.token_loaded(saved, Ok(None));
        assert!(
            session.expired().is_empty(),
            "a server signed in isn't expired"
        );
    }

    #[test]
    fn the_registry_keeps_the_newest_and_forgets_a_server_signed_out() {
        let mut registry = HashMap::new();
        let info = |server: &str| ItemInfo {
            server_id: server.into(),
            ..Default::default()
        };
        remember(
            &mut registry,
            (0..REGISTRY_LIMIT + 10)
                .map(|n| (format!("jellyfin://s1/i{n}"), info("s1")))
                .collect(),
        );
        assert_eq!(registry.len(), REGISTRY_LIMIT);
        assert!(!registry.contains_key("jellyfin://s1/i0"));
        assert!(registry.contains_key(&format!("jellyfin://s1/i{}", REGISTRY_LIMIT + 9)));
        remember(&mut registry, vec![("jellyfin://s1/i0".into(), info("s1"))]);
        assert!(
            registry.contains_key("jellyfin://s1/i0"),
            "the newest stays"
        );

        // Signing out forgets the server's.
        let folder = Folder::new("registry");
        let session = session(&folder);
        remember(
            &mut session.registry().lock().unwrap(),
            vec![
                ("jellyfin://s1/a".into(), info("s1")),
                ("jellyfin://s2/b".into(), info("s2")),
            ],
        );
        session.forget_items("s1");
        let registry = session.registry();
        let left: Vec<String> = registry.lock().unwrap().keys().cloned().collect();
        assert_eq!(left, ["jellyfin://s2/b"]);
    }

    use url::Url;

    /// The local test server (tools/jellyfin-server.sh), as the tests in
    /// tests.rs reach it; None skips the test.
    fn live() -> Option<(String, String, String)> {
        let (Ok(address), Ok(name), Ok(password)) = (
            std::env::var("PATINA_TEST_JELLYFIN_URL"),
            std::env::var("PATINA_TEST_JELLYFIN_USER"),
            std::env::var("PATINA_TEST_JELLYFIN_PASSWORD"),
        ) else {
            crate::mpv::testing::skip(
                "PATINA_TEST_REQUIRE_JELLYFIN",
                "needs a Jellyfin server in PATINA_TEST_JELLYFIN_URL",
            );
            return None;
        };
        Some((address, name, password))
    }

    /// The sign-in's steps as `SignIn` takes them, against the server, and
    /// the session's bookkeeping after it. No token goes to the keyring.
    #[test]
    fn live_a_session_signs_in_and_out() {
        let Some((address, name, password)) = live() else {
            return;
        };
        let folder = Folder::new("live");
        let session = session(&folder);
        // A device id of the test's own, as tests.rs keeps them: signing in
        // again on one revokes the account's other tokens for it.
        let identity = Identity::new(store::DEVICE_NAME, "patina-tests-session");
        let connected = connect(&address, &identity, None).unwrap();
        assert!(!connected.info.id.is_empty());
        assert_eq!(connected.client.base().scheme(), "http");
        if std::env::var("PATINA_TEST_JELLYFIN_LIBRARY").is_ok_and(|library| library == "generated")
        {
            // viewer is listed, patina isn't.
            let listed: Vec<&str> = connected
                .users
                .iter()
                .map(|user| user.name.as_str())
                .collect();
            assert!(listed.contains(&"viewer"), "{listed:?}");
            assert!(!listed.contains(&"patina"), "{listed:?}");
        }
        assert!(matches!(
            connect("http://127.0.0.1:9", &identity, None),
            Err(Error::Network(_))
        ));

        let client = connected.client.clone();
        let auth = client.authenticate_by_name(&name, &password).unwrap();
        let server = server_for(&client, &connected.info, &auth, None);
        assert_eq!(server.id, connected.info.id);
        assert_eq!(server.user_name, name);
        assert!(!server.address.ends_with('/'));
        // The session's client, made from what is kept, signs in as well.
        session
            .add_account(server.clone(), &auth.access_token)
            .unwrap();
        let account = session.account(&server.id).unwrap();
        assert_eq!(account.client.me().unwrap().id, auth.user.id);
        let reloaded = Store::load(&folder.0);
        let kept = reloaded.server(&server.id).unwrap();
        assert_eq!(kept.user_id, auth.user.id);
        let text = std::fs::read_to_string(reloaded.path()).unwrap();
        assert!(
            !text.contains(&auth.access_token),
            "the token is in the file"
        );

        account.client.logout().unwrap();
        assert_eq!(account.client.me(), Err(Error::Unauthorized));
        session.remove_account(&server.id, false);
        assert!(!session.signed_in());
        assert!(
            !Store::load(&folder.0)
                .server(&server.id)
                .unwrap()
                .signed_in()
        );
    }
}
