//! The Wayland clipboard, which Slint's text fields (see backend.rs) and
//! Patina's own Copy and Paste use on Wayland.
//!
//! arboard, which Slint and Patina use elsewhere, reaches the Wayland
//! clipboard only through the data-control protocols, which are meant for
//! clipboard managers: KWin keeps them from sandboxed clients such as a
//! Flatpak, and a Flatpak has no X11 to fall back on, so copy and paste did
//! nothing there. This is the ordinary clipboard, and the primary selection
//! a middle click pastes, which any client may use while one of its windows
//! has the keyboard focus. smithay-clipboard keeps an event queue of its own
//! on winit's connection, which is what lets it see the focus that winit's
//! windows have: a connection of its own would have none.

use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use slint::platform::{Clipboard, WindowAdapter};
use slint::winit_030::winit::window::WindowId;
use slint::winit_030::{WinitWindowAccessor, winit};
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long a paste waits for the clipboard's owner to hand its text over.
/// A program that hangs while it owns the clipboard would otherwise freeze
/// Patina at every paste.
const TIMEOUT: Duration = Duration::from_secs(2);

/// How long after it starts a thread's clipboard can't read or set anything
/// yet: until the compositor has told it which seat has the keyboard focus,
/// a round trip or two later, it has nowhere to read from or set the text
/// on. A paste meanwhile keeps trying, since a failure looks the same
/// either way; a copy waits it out. The first threads start as soon as a
/// window has the focus (`focused`), so that this is mostly over by the
/// first copy or paste.
const WARM_UP: Duration = Duration::from_millis(500);

/// How long the toast saying why a paste got nothing stays up: long
/// enough to be read.
const TOAST_SECONDS: f32 = 4.0;

/// How long quitting waits for the clipboard's threads to end.
const SHUT_DOWN_WAIT: Duration = Duration::from_secs(1);

/// The most threads pastes go to: up to two left waiting on pastes that
/// took too long, and one more kept for pasting Patina's own copies (see
/// Threads).
const PASTE_THREADS: usize = 3;

/// smithay-clipboard's handles that are alive, or about to be (`Counted`).
/// Each has a worker dispatching events on winit's display, which has to
/// stay open until the last of them is let go of (`in_use`).
static OPEN: AtomicUsize = AtomicUsize::new(0);

enum State {
    /// Not known yet: the windows made so far, which tell once winit has
    /// made a window for one of them.
    Unknown(Vec<Weak<dyn WindowAdapter>>),
    /// winit's wl_display, and the clipboard's threads. They are ended by
    /// `shut_down`, while the display is still open; they are never let go
    /// of as the thread-locals go, should Patina end some other way, since
    /// winit may have closed the display by then and the threads would
    /// still be using it.
    Wayland(Display, ManuallyDrop<Threads>),
    /// Wayland's, after `shut_down`: neither read nor set any more.
    Closed,
    /// X11, where Slint's clipboard and arboard work.
    Elsewhere,
}

thread_local! {
    static STATE: RefCell<State> = const { RefCell::new(State::Unknown(Vec::new())) };
    static FOCUS: Focus<WindowId> = const { Focus::new() };
}

/// Remembers a window Slint made, whose display tells, once winit has made
/// the window, whether Patina is on Wayland.
pub fn watch(window: &Rc<dyn WindowAdapter>) {
    STATE.with_borrow_mut(|state| {
        if let State::Unknown(windows) = state {
            windows.retain(|window| window.strong_count() > 0);
            windows.push(Rc::downgrade(window));
        }
    });
}

/// Notes that `window` has the keyboard focus, which is where a paste's
/// toast goes, and starts the clipboard's threads on Wayland, so that by
/// the first copy or paste they have heard from the compositor which seat
/// has the focus (see `WARM_UP`).
pub fn focused(window: &winit::window::Window) {
    FOCUS.with(|focus| focus.gained(window.id()));
    let display = window.display_handle().ok().map(|handle| handle.as_raw());
    STATE.with_borrow_mut(|state| learn(state, display));
    with_threads(|threads, spawn| threads.start(spawn));
}

/// Notes that `window` has lost the keyboard focus, or has gone.
pub fn unfocused(window: WindowId) {
    FOCUS.with(|focus| focus.lost(window));
}

/// Looks, once winit has handed over a batch of events, whether the
/// keyboard focus has left Patina: another program may then take the
/// clipboard and the primary selection (see Owner).
pub fn settle() {
    if FOCUS.with(Focus::left) {
        log::debug!("the keyboard focus left Patina");
        with_threads(|threads, _| threads.focus_left());
    }
}

/// Whether the clipboard is Wayland's, for this module to read and set:
/// false on X11, and until winit has made a window.
pub fn on_wayland() -> bool {
    STATE.with_borrow_mut(|state| {
        if let State::Unknown(windows) = state {
            let display = windows.iter().filter_map(Weak::upgrade).find_map(|window| {
                window
                    .window()
                    .with_winit_window(|winit| winit.display_handle().ok().map(|h| h.as_raw()))
                    .flatten()
            });
            learn(state, display);
        }
        matches!(state, State::Wayland(..) | State::Closed)
    })
}

/// Whether a worker of smithay-clipboard may still be dispatching events on
/// winit's display. After `shut_down`, that is one whose thread waits on a
/// paste the clipboard's owner has never answered. Slint's platform keeps
/// the display open then, rather than close it under the worker as the
/// process exits (see backend.rs).
pub fn in_use() -> bool {
    OPEN.load(Ordering::SeqCst) > 0
}

/// Settles what an unknown clipboard is from a window's display, when
/// there is one.
fn learn(state: &mut State, display: Option<RawDisplayHandle>) {
    if let State::Unknown(_) = state {
        match display {
            Some(RawDisplayHandle::Wayland(handle)) => {
                *state = State::Wayland(Display(handle.display), ManuallyDrop::default());
            }
            Some(_) => *state = State::Elsewhere,
            None => {}
        }
    }
}

/// The text on `clipboard`, if there is any. A paste that gets nothing
/// because the clipboard's owner doesn't answer, or may be one that hasn't
/// (see Threads), says why in the window that has the keyboard focus,
/// which is the one pasted in, rather than leave the user to wonder.
pub fn text(clipboard: Clipboard) -> Option<String> {
    let target = Target::of(&clipboard)?;
    match with_threads(|threads, spawn| threads.paste(target, TIMEOUT, spawn))? {
        Ok(text) => text,
        Err(unanswered) => {
            let window = FOCUS.with(Focus::window);
            crate::app::App::with_current(|app| {
                app.explain(window, "Can't Paste", unanswered.why(target), TOAST_SECONDS);
            });
            None
        }
    }
}

/// Puts `text` on `clipboard`.
pub fn set_text(clipboard: Clipboard, text: String) {
    if let Some(target) = Target::of(&clipboard) {
        with_threads(|threads, spawn| threads.copy(target, text, spawn));
    }
}

/// Ends the clipboard's threads as Patina quits, once the event loop has
/// ended: smithay-clipboard's worker dispatches events on winit's display
/// until its handle is let go of, so the display has to outlive the
/// handle, and Slint's platform, which closes the display, goes only after
/// this. Nothing is read or set after it.
pub fn shut_down() {
    let threads = STATE.with_borrow_mut(|state| match std::mem::replace(state, State::Closed) {
        State::Wayland(_, threads) => Some(ManuallyDrop::into_inner(threads)),
        State::Elsewhere => {
            *state = State::Elsewhere;
            None
        }
        State::Unknown(_) | State::Closed => None,
    });
    if let Some(threads) = threads {
        threads.shut_down(SHUT_DOWN_WAIT);
    }
}

/// Runs `f` with the clipboard's threads and what starts another; None off
/// Wayland.
fn with_threads<R>(
    f: impl FnOnce(&mut Threads, &mut dyn FnMut() -> std::io::Result<Thread>) -> R,
) -> Option<R> {
    STATE.with_borrow_mut(|state| {
        let State::Wayland(display, threads) = state else {
            return None;
        };
        let display = *display;
        let mut spawn = || {
            spawn_counted(move |counted| {
                // SAFETY: winit's display, which stays open for as long as
                // a handle lives. `shut_down` lets go of the handles, ending
                // their threads, before Slint's platform, which closes the
                // display, goes at exit. A thread that waits on a paste
                // (see Threads) can't be ended until the clipboard's owner
                // answers or exits, either of which wakes its worker, which
                // waits on the paste's pipe as well as on the display:
                // should one still wait at exit, or not have opened its
                // handle yet, the platform keeps the display open rather
                // than close it under the worker (`in_use`).
                unsafe { Handle::open(display, counted) }
            })
        };
        Some(f(threads, &mut spawn))
    })
}

/// winit's wl_display, for the clipboard's threads.
#[derive(Clone, Copy)]
struct Display(NonNull<c_void>);

// SAFETY: libwayland's displays may be used from any thread; each user of
// one dispatches a queue of its own.
unsafe impl Send for Display {}

impl Display {
    fn as_ptr(self) -> *mut c_void {
        self.0.as_ptr()
    }
}

/// Starts a thread with the handle `open` makes there, counted in `OPEN`
/// from now on rather than once the thread runs: should Patina quit before
/// it has, the display is kept open for it all the same.
fn spawn_counted<S: Selections>(
    open: impl FnOnce(Counted) -> S + Send + 'static,
) -> std::io::Result<Thread> {
    let counted = Counted::new();
    Thread::spawn(move || open(counted))
}

/// One in `OPEN` for as long as it lives, which for a handle's is until
/// the handle has been let go of.
pub struct Counted(());

/// Held by the tests that count on `OPEN`, which would see each other's
/// counts.
#[cfg(test)]
pub static COUNTING: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Counted {
    pub fn new() -> Self {
        OPEN.fetch_add(1, Ordering::SeqCst);
        Self(())
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        OPEN.fetch_sub(1, Ordering::SeqCst);
    }
}

/// smithay-clipboard's handle, counted in `OPEN` until it has been let go
/// of and its worker has ended.
struct Handle {
    clipboard: ManuallyDrop<smithay_clipboard::Clipboard>,
    /// Goes after the clipboard (see Drop).
    _counted: Counted,
}

impl Handle {
    /// # Safety
    ///
    /// As smithay_clipboard::Clipboard::new: `display` stays open for as
    /// long as the handle lives.
    unsafe fn open(display: Display, counted: Counted) -> Self {
        // SAFETY: as this function's.
        let clipboard = unsafe { smithay_clipboard::Clipboard::new(display.as_ptr()) };
        Self {
            clipboard: ManuallyDrop::new(clipboard),
            _counted: counted,
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: let go of once, here, and never used after. Letting go of
        // it waits for its worker to end; only then does the count go down,
        // as the fields go after this.
        unsafe { ManuallyDrop::drop(&mut self.clipboard) };
    }
}

/// Which of Patina's windows has the keyboard focus, as far as winit has
/// told, and what it told in the batch of events now being handed over.
struct Focus<Id> {
    window: Cell<Option<Id>>,
    /// Whether one had it after the last batch.
    had: Cell<bool>,
    /// The window that lost it last in this batch.
    lost: Cell<Option<Id>>,
    /// Whether that window had it back in this batch.
    regained: Cell<bool>,
}

impl<Id: Copy + PartialEq> Focus<Id> {
    const fn new() -> Self {
        Self {
            window: Cell::new(None),
            had: Cell::new(false),
            lost: Cell::new(None),
            regained: Cell::new(false),
        }
    }

    fn window(&self) -> Option<Id> {
        self.window.get()
    }

    fn gained(&self, window: Id) {
        if self.lost.get() == Some(window) {
            self.regained.set(true);
        }
        self.window.set(Some(window));
    }

    /// The window that has the focus may have been given it already, should
    /// the compositor tell of the new window first.
    fn lost(&self, window: Id) {
        if self.window.get() == Some(window) {
            self.window.set(None);
        }
        self.lost.set(Some(window));
    }

    /// Whether the focus has left Patina since this was last asked, after a
    /// batch of events: no window of Patina's has it now, or one lost it
    /// and had it back, as when the focus went to another program and came
    /// back while Patina was busy. The focus moving from one of Patina's
    /// windows to another, a loss and a gain together, never left Patina,
    /// so no other program can have taken a selection meanwhile. (Should
    /// it go to another program and on to another of Patina's windows
    /// within one batch, that is missed: only a paste of what was copied
    /// there, refused until the focus next leaves, is lost by it.)
    fn left(&self) -> bool {
        let has = self.window.get().is_some();
        let left = (self.had.replace(has) && !has) || self.regained.replace(false);
        self.lost.set(None);
        left
    }
}

/// Wayland's two selections: the clipboard, and the primary selection,
/// which is what was selected last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Clipboard,
    Primary,
}

impl Target {
    fn of(clipboard: &Clipboard) -> Option<Self> {
        match clipboard {
            Clipboard::DefaultClipboard => Some(Self::Clipboard),
            Clipboard::SelectionClipboard => Some(Self::Primary),
            _ => None,
        }
    }

    /// Its place in what is kept for each of the two.
    fn index(self) -> usize {
        self as usize
    }
}

/// Why a paste got nothing from the clipboard's owner.
#[derive(Debug, PartialEq)]
enum Unanswered {
    /// The owner didn't answer in time, or may be one that hasn't.
    Owner,
    /// Owners from before haven't answered, and every thread there is to
    /// spare waits on them. It lasts until one answers, which it does as
    /// it exits.
    Earlier,
}

impl Unanswered {
    /// What the toast says, for a paste of `target`.
    fn why(&self, target: Target) -> &'static str {
        match (self, target) {
            (Self::Earlier, _) => {
                "An app that copied earlier isn't responding. Close it to paste again."
            }
            (Self::Owner, Target::Clipboard) => "The app that copied it isn't responding.",
            (Self::Owner, Target::Primary) => "The app it was selected in isn't responding.",
        }
    }
}

/// What Patina knows of a selection's owner. An ordinary program can take a
/// selection only while one of its windows has the keyboard focus, and one
/// that hangs can't at all, so for as long as the focus stays with Patina,
/// a selection's owner stays the one it was, unless Patina sets it. (A
/// clipboard manager, or a program such as wl-copy, can take one without
/// the focus, and smithay-clipboard tells of no change: such a copy can't
/// be pasted while the owner before it is taken to be hung, until the
/// focus has left Patina and come back.)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Owner {
    /// Any program, as far as Patina knows.
    #[default]
    Unknown,
    /// Patina itself: it set the selection since the focus last left it.
    /// Its copy thread's worker answers a paste of it at once.
    Patina,
    /// May be the one that left a paste unanswered.
    Hung,
}

/// What the threads need of smithay-clipboard, which the tests stand in
/// for.
trait Selections {
    fn read(&self, target: Target) -> std::io::Result<String>;
    fn write(&self, target: Target, text: String);
}

impl Selections for Handle {
    fn read(&self, target: Target) -> std::io::Result<String> {
        match target {
            Target::Clipboard => self.clipboard.load(),
            Target::Primary => self.clipboard.load_primary(),
        }
    }

    fn write(&self, target: Target, text: String) {
        match target {
            Target::Clipboard => self.clipboard.store(text),
            Target::Primary => self.clipboard.store_primary(text),
        }
    }
}

enum Request {
    Text(Target, mpsc::Sender<Option<String>>),
    SetText(Target, String),
}

/// The clipboard's threads, each with a handle of smithay-clipboard's, and
/// what Patina knows of the selections' owners.
///
/// A handle waits for a paste's text for as long as the owner takes, and
/// until it comes, its thread can do nothing else: smithay-clipboard waits
/// for an answer on one channel, which a paste asked later would take the
/// late answer from. A program that hangs while it owns the clipboard draws
/// that out for as long as it hangs, so a thread whose paste takes too long
/// is left waiting, and the next paste goes to another, which is started
/// if there is none. Copies go to a thread of their own, which never
/// pastes, so nothing holds them up.
///
/// A paste isn't asked of an owner that may be one that hasn't answered
/// (`Owner::Hung`), which would hold Patina up as long again and leave
/// another thread waiting. Nor is one asked, but of Patina's own copy, once
/// it could leave every paste thread waiting: the last is kept for Patina's
/// copies, so that those paste whatever else waits. So there are never
/// more than `PASTE_THREADS` of them.
///
/// No handle is let go of before `shut_down`: that withdraws what was
/// copied there and destroys its data device, which the compositor may be
/// sending a new selection to at that moment, and libwayland, which drops
/// what comes for an object it has destroyed, would then lose count of the
/// objects the compositor makes, and close the connection at the next one.
/// A thread whose paste is answered at last is used again instead.
#[derive(Default)]
struct Threads {
    /// The thread copies go to.
    copies: Option<Thread>,
    /// The threads pastes go to.
    pastes: Vec<Thread>,
    /// Each selection's owner, by `Target::index`.
    owners: [Owner; 2],
}

impl Threads {
    /// Starts the threads copies and pastes go to first, those not started
    /// yet.
    fn start(&mut self, mut spawn: impl FnMut() -> std::io::Result<Thread>) {
        self.copy_thread(&mut spawn);
        self.paste_thread(&mut spawn);
    }

    /// Puts `text` on `target`.
    fn copy(
        &mut self,
        target: Target,
        text: String,
        spawn: impl FnMut() -> std::io::Result<Thread>,
    ) {
        self.owners[target.index()] = Owner::Patina;
        if let Some(thread) = self.copy_thread(spawn) {
            thread.requests.set_text(target, text);
        }
    }

    /// The text on `target`, waiting up to `timeout` for it; or why there
    /// is none to be had, when the paste took longer or wasn't asked.
    fn paste(
        &mut self,
        target: Target,
        timeout: Duration,
        spawn: impl FnMut() -> std::io::Result<Thread>,
    ) -> Result<Option<String>, Unanswered> {
        let waiting = self.waiting();
        let owner = self.owners[target.index()];
        if waiting > 0 && owner == Owner::Hung {
            log::debug!("nothing to paste: the clipboard's owner has not answered yet");
            return Err(Unanswered::Owner);
        }
        let spare = match owner {
            Owner::Patina => PASTE_THREADS,
            Owner::Unknown | Owner::Hung => PASTE_THREADS - 1,
        };
        if waiting >= spare {
            log::debug!("nothing to paste: owners from before have not answered yet");
            return Err(Unanswered::Earlier);
        }
        let Some(index) = self.paste_thread(spawn) else {
            return Ok(None);
        };
        let thread = &mut self.pastes[index];
        let answer = thread.requests.text(target);
        match answer.recv_timeout(timeout) {
            Ok(text) => Ok(text),
            Err(RecvTimeoutError::Timeout) => {
                log::warn!("the clipboard's owner did not answer");
                thread.unanswered = Some(answer);
                // What is copied in a program was most likely selected
                // there too; what Patina set is still its own.
                for owner in &mut self.owners {
                    if *owner != Owner::Patina {
                        *owner = Owner::Hung;
                    }
                }
                self.owners[target.index()] = Owner::Hung;
                Err(Unanswered::Owner)
            }
            Err(RecvTimeoutError::Disconnected) => {
                log::warn!("the clipboard stopped");
                Ok(None)
            }
        }
    }

    /// Notes that the keyboard focus has left Patina (see Owner).
    fn focus_left(&mut self) {
        self.owners = [Owner::Unknown; 2];
    }

    /// How many threads wait on a paste that took too long.
    fn waiting(&mut self) -> usize {
        self.pastes
            .iter_mut()
            .map(Thread::waits)
            .filter(|waits| *waits)
            .count()
    }

    /// The thread copies go to, started should there be none.
    fn copy_thread(&mut self, spawn: impl FnMut() -> std::io::Result<Thread>) -> Option<&Thread> {
        // Only a panic ends one before `shut_down`.
        if self
            .copies
            .as_ref()
            .is_none_or(|thread| thread.handle.is_finished())
        {
            self.copies = started(spawn);
        }
        self.copies.as_ref()
    }

    /// A paste thread that doesn't wait on a paste, or else a new one,
    /// should there be room (which `paste` sees to).
    fn paste_thread(&mut self, spawn: impl FnMut() -> std::io::Result<Thread>) -> Option<usize> {
        self.pastes.retain(|thread| !thread.handle.is_finished());
        if let Some(index) = self.pastes.iter_mut().position(|thread| !thread.waits()) {
            return Some(index);
        }
        if self.pastes.len() >= PASTE_THREADS {
            return None;
        }
        self.pastes.push(started(spawn)?);
        Some(self.pastes.len() - 1)
    }

    /// Ends the threads, waiting up to `wait` for them, except those still
    /// waiting on a paste, which can't be: their handles are never let go
    /// of (see `in_use`).
    fn shut_down(self, wait: Duration) {
        let deadline = Instant::now() + wait;
        let mut ending = Vec::new();
        for mut thread in self.copies.into_iter().chain(self.pastes) {
            if thread.waits() {
                log::debug!("the clipboard's owner never answered; leaving that clipboard be");
                std::mem::forget(thread);
            } else {
                // It ends once it has done what it was asked.
                drop(thread.requests);
                ending.push(thread.handle);
            }
        }
        for handle in ending {
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if handle.is_finished() {
                let _ = handle.join();
            } else {
                // It lets go of its handle whenever it ends.
                log::debug!("the clipboard did not stop in time");
            }
        }
    }
}

/// The thread `spawn` starts, if it could.
fn started(mut spawn: impl FnMut() -> std::io::Result<Thread>) -> Option<Thread> {
    spawn()
        .map_err(|error| log::warn!("no clipboard: {error}"))
        .ok()
}

/// A thread that owns a handle of smithay-clipboard's. The handle waits for
/// a paste's text for as long as it takes; through the thread a paste waits
/// no longer than its timeout.
///
/// What Patina copies is offered by the handle's worker for as long as the
/// handle lives: letting go of the handle withdraws it, and the clipboard
/// is left empty. So only `Threads::shut_down`, at the end, lets go of it.
struct Thread {
    requests: Requests,
    handle: JoinHandle<()>,
    /// The answer to a paste that took too long, until it comes.
    unanswered: Option<mpsc::Receiver<Option<String>>>,
}

impl Thread {
    /// Starts the thread, with the clipboard `open` makes there.
    fn spawn<S: Selections>(open: impl FnOnce() -> S + Send + 'static) -> std::io::Result<Self> {
        let (sender, requests) = mpsc::channel();
        let handle = std::thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || {
                // Not let go of should the thread panic: its worker is left
                // to go with the process, as one waiting at exit is.
                let clipboard = ManuallyDrop::new(open());
                serve(&*clipboard, requests, Instant::now() + WARM_UP);
                drop(ManuallyDrop::into_inner(clipboard));
            })?;
        Ok(Self {
            requests: Requests(sender),
            handle,
            unanswered: None,
        })
    }

    /// Whether it still waits on a paste that took too long. One that has
    /// been answered at last is free again, and the answer, too late to
    /// paste, goes unused.
    fn waits(&mut self) -> bool {
        let Some(answer) = &self.unanswered else {
            return false;
        };
        match answer.try_recv() {
            Err(TryRecvError::Empty) => true,
            answered => {
                if answered.is_ok() {
                    log::debug!("the clipboard's owner answered a paste too late");
                }
                self.unanswered = None;
                false
            }
        }
    }
}

/// Patina's end of a clipboard thread. The thread ends once it is gone.
struct Requests(mpsc::Sender<Request>);

impl Requests {
    /// Asks for the text on `target`, which comes on what this gives, or
    /// doesn't if the thread has ended.
    fn text(&self, target: Target) -> mpsc::Receiver<Option<String>> {
        let (reply, answer) = mpsc::channel();
        // A thread that has ended has dropped the request, and with it
        // `reply`, which `answer` then says.
        let _ = self.0.send(Request::Text(target, reply));
        answer
    }

    fn set_text(&self, target: Target, text: String) {
        let _ = self.0.send(Request::SetText(target, text));
    }
}

/// Answers `requests` with `clipboard` until they end.
fn serve(clipboard: &impl Selections, requests: mpsc::Receiver<Request>, warm_by: Instant) {
    for request in requests {
        match request {
            Request::Text(target, reply) => {
                let text = read(clipboard, target, warm_by)
                    .map_err(|error| log::debug!("nothing to paste: {error}"))
                    .ok();
                let _ = reply.send(text);
            }
            Request::SetText(target, text) => {
                // Set before the compositor has said which seat has the
                // focus, the text would go nowhere. A read would tell
                // sooner, but it asks the clipboard's owner, which may be
                // hung: a copy doesn't wait on that.
                let now = Instant::now();
                if now < warm_by {
                    std::thread::sleep(warm_by - now);
                }
                clipboard.write(target, text);
            }
        }
    }
}

/// Reads `target`, trying again while it fails until `warm_by`.
fn read(clipboard: &impl Selections, target: Target, warm_by: Instant) -> std::io::Result<String> {
    loop {
        let result = clipboard.read(target);
        if result.is_ok() || Instant::now() >= warm_by {
            return result;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Condvar, Mutex};

    type Written = Arc<Mutex<Vec<(Target, String, Instant)>>>;

    /// A paste's wait in these tests, past which its owner counts as hung.
    const SHORT: Duration = Duration::from_millis(50);

    /// A clipboard that has heard from the compositor after `ready_after`
    /// reads, and whose reads each take `delay`, or wait for `gate` to
    /// open: those of every target, or of `hangs` only. Whether it was let
    /// go of is `dropped`.
    struct Fake {
        reads: Cell<u32>,
        ready_after: u32,
        delay: Duration,
        gate: Option<Mutex<mpsc::Receiver<()>>>,
        hangs: Option<Target>,
        written: Written,
        dropped: Dropped,
    }

    /// Set once the Fake holding it is let go of.
    #[derive(Default)]
    struct Dropped(Arc<AtomicBool>);

    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    impl Fake {
        fn new(ready_after: u32) -> Self {
            Self {
                reads: Cell::new(0),
                ready_after,
                delay: Duration::ZERO,
                gate: None,
                hangs: None,
                written: Arc::default(),
                dropped: Dropped::default(),
            }
        }

        /// One whose reads each wait until the sender given with it sends,
        /// as a hung owner's do.
        fn gated() -> (Self, mpsc::Sender<()>) {
            let (open, gate) = mpsc::channel();
            let fake = Self {
                gate: Some(Mutex::new(gate)),
                ..Self::new(0)
            };
            (fake, open)
        }

        /// One whose reads of `target` only wait so.
        fn hanging(target: Target) -> (Self, mpsc::Sender<()>) {
            let (fake, open) = Self::gated();
            let fake = Self {
                hangs: Some(target),
                ..fake
            };
            (fake, open)
        }
    }

    impl Selections for Fake {
        fn read(&self, target: Target) -> std::io::Result<String> {
            std::thread::sleep(self.delay);
            if let Some(gate) = &self.gate
                && self.hangs.is_none_or(|hangs| hangs == target)
            {
                let _ = gate.lock().unwrap().recv();
            }
            self.reads.set(self.reads.get() + 1);
            if self.reads.get() > self.ready_after {
                Ok(format!("{target:?}"))
            } else {
                Err(std::io::Error::other("no events received on any seat"))
            }
        }

        fn write(&self, target: Target, text: String) {
            self.written
                .lock()
                .unwrap()
                .push((target, text, Instant::now()));
        }
    }

    fn texts(written: &Written) -> Vec<String> {
        written
            .lock()
            .unwrap()
            .iter()
            .map(|(_, text, _)| text.clone())
            .collect()
    }

    /// Waits until `written` holds `count` copies.
    fn wait_for_copies(written: &Written, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while written.lock().unwrap().len() < count {
            assert!(Instant::now() < deadline, "no copy");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Waits until no more than `count` threads wait on a paste.
    fn wait_for_answers(threads: &mut Threads, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while threads.waiting() > count {
            assert!(Instant::now() < deadline, "never answered");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Starts threads with `fakes`, the last first, counting them in
    /// `started`; failing the test should it want one more.
    fn spawner<'a>(
        fakes: &'a mut Vec<Fake>,
        started: &'a Cell<usize>,
    ) -> impl FnMut() -> std::io::Result<Thread> + 'a {
        move || {
            let fake = fakes.pop().expect("one thread too many");
            started.set(started.get() + 1);
            Thread::spawn(move || fake)
        }
    }

    #[test]
    fn a_paste_waits_for_the_compositor_at_first() {
        let thread = Thread::spawn(|| Fake::new(3)).unwrap();
        assert_eq!(
            thread
                .requests
                .text(Target::Clipboard)
                .recv_timeout(TIMEOUT),
            Ok(Some("Clipboard".to_owned()))
        );
        assert_eq!(
            thread.requests.text(Target::Primary).recv_timeout(TIMEOUT),
            Ok(Some("Primary".to_owned()))
        );
    }

    /// A copy never asks the clipboard's owner for anything, which would
    /// hold it up for as long as a hung owner hangs: at first it waits the
    /// compositor out.
    #[test]
    fn a_copy_waits_for_the_compositor_without_reading() {
        let (sender, requests) = mpsc::channel();
        sender
            .send(Request::SetText(Target::Primary, "selected".into()))
            .unwrap();
        drop(sender);
        let fake = Fake::new(u32::MAX);
        let warm_by = Instant::now() + Duration::from_millis(100);
        serve(&fake, requests, warm_by);
        assert_eq!(fake.reads.get(), 0);
        let written = fake.written.lock().unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(
            (written[0].0, written[0].1.as_str()),
            (Target::Primary, "selected")
        );
        assert!(written[0].2 >= warm_by);
    }

    #[test]
    fn a_paste_after_that_does_not_wait() {
        let (sender, requests) = mpsc::channel();
        let (reply, answer) = mpsc::channel();
        sender
            .send(Request::Text(Target::Clipboard, reply))
            .unwrap();
        drop(sender);
        let fake = Fake::new(3);
        serve(&fake, requests, Instant::now());
        assert_eq!(answer.recv(), Ok(None));
        assert_eq!(fake.reads.get(), 1);
    }

    #[test]
    fn a_paste_waits_only_so_long() {
        let fake = Fake {
            delay: Duration::from_secs(5),
            ..Fake::new(0)
        };
        let mut fakes = vec![fake];
        let started = Cell::new(0);
        let mut threads = Threads::default();
        let asked = Instant::now();
        assert_eq!(
            threads.paste(Target::Clipboard, SHORT, spawner(&mut fakes, &started)),
            Err(Unanswered::Owner)
        );
        assert!(asked.elapsed() < Duration::from_secs(2));
    }

    /// The first window to have the focus starts a thread for copies and
    /// one for pastes, which later ones leave be.
    #[test]
    fn the_focus_starts_a_thread_for_copies_and_one_for_pastes() {
        let (copier, paster) = (Fake::new(0), Fake::new(0));
        let written = Arc::clone(&copier.written);
        let mut fakes = vec![paster, copier];
        let started = Cell::new(0);
        let mut spawn = spawner(&mut fakes, &started);
        let mut threads = Threads::default();
        threads.start(&mut spawn);
        threads.start(&mut spawn);
        assert_eq!(started.get(), 2);
        threads.copy(Target::Clipboard, "copied".into(), &mut spawn);
        wait_for_copies(&written, 1);
        assert_eq!(
            threads.paste(Target::Primary, TIMEOUT, &mut spawn),
            Ok(Some("Primary".into()))
        );
        drop(spawn);
        assert_eq!(started.get(), 2);
        threads.shut_down(Duration::from_secs(5));
    }

    /// Once a paste has waited too long, the owner that may still hang
    /// isn't asked again, which would hold Patina up as long and leave
    /// another thread waiting, while copies land. What Patina copies
    /// itself is asked for, and once the focus has left Patina, what
    /// another program may have copied since. The first thread, answered
    /// at last, stands by, its handle kept, and its answer, too late, is
    /// never pasted.
    #[test]
    fn a_hung_owner_is_not_asked_again_until_the_focus_has_left() {
        let (hung, thaw) = Fake::gated();
        let (copier, second) = (Fake::new(0), Fake::new(0));
        let (hung_written, hung_dropped) = (Arc::clone(&hung.written), Arc::clone(&hung.dropped.0));
        let copied = Arc::clone(&copier.written);
        let mut fakes = vec![second, copier, hung];
        let started = Cell::new(0);
        let mut spawn = spawner(&mut fakes, &started);
        let mut threads = Threads::default();

        let asked = Instant::now();
        let pasted = threads.paste(Target::Clipboard, SHORT, &mut spawn);
        assert_eq!(pasted, Err(Unanswered::Owner));
        assert!(asked.elapsed() >= SHORT);

        // Neither selection is asked of it again, nor a thread started.
        for target in [Target::Clipboard, Target::Primary] {
            let asked = Instant::now();
            assert_eq!(
                threads.paste(target, Duration::from_secs(5), &mut spawn),
                Err(Unanswered::Owner)
            );
            assert!(asked.elapsed() < Duration::from_secs(1));
        }
        assert_eq!(started.get(), 1);

        // Patina's own copy lands, and is asked for, on a second thread.
        threads.copy(Target::Clipboard, "own".into(), &mut spawn);
        wait_for_copies(&copied, 1);
        assert_eq!(
            threads.paste(Target::Clipboard, TIMEOUT, &mut spawn),
            Ok(Some("Clipboard".into()))
        );
        assert_eq!(
            threads.paste(Target::Primary, TIMEOUT, &mut spawn),
            Err(Unanswered::Owner)
        );
        threads.focus_left();
        assert_eq!(
            threads.paste(Target::Primary, TIMEOUT, &mut spawn),
            Ok(Some("Primary".into()))
        );

        // The owner answers at last; copies go on to the same thread.
        thaw.send(()).unwrap();
        wait_for_answers(&mut threads, 0);
        threads.copy(Target::Primary, "after".into(), &mut spawn);
        wait_for_copies(&copied, 2);
        drop(spawn);
        assert_eq!(texts(&copied), ["own", "after"]);
        assert!(texts(&hung_written).is_empty());
        assert_eq!(started.get(), 3);
        assert!(!hung_dropped.load(Ordering::SeqCst));
        threads.shut_down(Duration::from_secs(5));
        assert!(hung_dropped.load(Ordering::SeqCst));
    }

    /// A paste of one selection that times out takes the other's owner to
    /// be hung too, since what a program copies was most likely selected
    /// there, unless that one is Patina's own copy, which pastes.
    #[test]
    fn a_timed_out_paste_leaves_patinas_own_copy_be() {
        let (first, _thaw_first) = Fake::hanging(Target::Primary);
        let (second, _thaw_second) = Fake::hanging(Target::Primary);
        let copier = Fake::new(0);
        let copied = Arc::clone(&copier.written);
        let mut fakes = vec![second, first, copier];
        let started = Cell::new(0);
        let mut spawn = spawner(&mut fakes, &started);
        let mut threads = Threads::default();

        threads.copy(Target::Clipboard, "own".into(), &mut spawn);
        wait_for_copies(&copied, 1);
        assert_eq!(
            threads.paste(Target::Primary, SHORT, &mut spawn),
            Err(Unanswered::Owner)
        );
        let asked = Instant::now();
        assert_eq!(
            threads.paste(Target::Clipboard, TIMEOUT, &mut spawn),
            Ok(Some("Clipboard".into()))
        );
        assert!(asked.elapsed() < Duration::from_secs(1));
        assert_eq!(
            threads.paste(Target::Primary, TIMEOUT, &mut spawn),
            Err(Unanswered::Owner)
        );

        // Once the focus has been elsewhere, the clipboard may be anyone's;
        // a paste of the primary selection that times out then takes it to
        // be the hung program's as well.
        threads.focus_left();
        assert_eq!(
            threads.paste(Target::Clipboard, TIMEOUT, &mut spawn),
            Ok(Some("Clipboard".into()))
        );
        assert_eq!(
            threads.paste(Target::Primary, SHORT, &mut spawn),
            Err(Unanswered::Owner)
        );
        assert_eq!(
            threads.paste(Target::Clipboard, TIMEOUT, &mut spawn),
            Err(Unanswered::Owner)
        );
        drop(spawn);
        assert_eq!(started.get(), 3);
        threads.shut_down(Duration::from_secs(5));
    }

    /// Asking again after the focus has left Patina may find the owner that
    /// hangs still there: then nothing more is asked but Patina's own
    /// copies, which still paste, from the paste thread kept for them, and
    /// copies still land. Once a paste is answered, its thread is free
    /// again for the next.
    #[test]
    fn two_pastes_left_waiting_leave_a_thread_for_patinas_own_copies() {
        let (first, open_first) = Fake::gated();
        let (second, _never) = Fake::gated();
        let (copier, third) = (Fake::new(0), Fake::new(0));
        let (copied, second_dropped) = (Arc::clone(&copier.written), Arc::clone(&second.dropped.0));
        let mut fakes = vec![third, copier, second, first];
        let started = Cell::new(0);
        let mut spawn = spawner(&mut fakes, &started);
        let mut threads = Threads::default();

        let pasted = threads.paste(Target::Clipboard, SHORT, &mut spawn);
        assert_eq!(pasted, Err(Unanswered::Owner));
        threads.focus_left();
        let pasted = threads.paste(Target::Clipboard, SHORT, &mut spawn);
        assert_eq!(pasted, Err(Unanswered::Owner));
        assert_eq!(started.get(), 2);

        threads.focus_left();
        for target in [Target::Clipboard, Target::Primary] {
            assert_eq!(
                threads.paste(target, Duration::from_secs(5), &mut spawn),
                Err(Unanswered::Earlier)
            );
        }
        assert_eq!(started.get(), 2);

        // Patina copies: the copy lands, and pastes, from a third paste
        // thread, the only one to spare.
        threads.copy(Target::Clipboard, "kept".into(), &mut spawn);
        wait_for_copies(&copied, 1);
        let asked = Instant::now();
        assert_eq!(
            threads.paste(Target::Clipboard, TIMEOUT, &mut spawn),
            Ok(Some("Clipboard".into()))
        );
        assert!(asked.elapsed() < Duration::from_secs(1));
        assert_eq!(
            threads.paste(Target::Primary, Duration::from_secs(5), &mut spawn),
            Err(Unanswered::Earlier)
        );
        threads.copy(Target::Primary, "selected".into(), &mut spawn);
        wait_for_copies(&copied, 2);
        assert_eq!(
            threads.paste(Target::Primary, TIMEOUT, &mut spawn),
            Ok(Some("Primary".into()))
        );
        assert_eq!(started.get(), 4);
        assert_eq!(threads.pastes.len(), PASTE_THREADS);

        // One is answered, and its owner answers from now on: there is a
        // thread to spare again.
        threads.focus_left();
        drop(open_first);
        wait_for_answers(&mut threads, 1);
        assert_eq!(
            threads.paste(Target::Primary, TIMEOUT, &mut spawn),
            Ok(Some("Primary".into()))
        );
        drop(spawn);
        assert_eq!(started.get(), 4);
        assert_eq!(texts(&copied), ["kept", "selected"]);
        threads.shut_down(Duration::from_secs(5));
        assert!(!second_dropped.load(Ordering::SeqCst));
    }

    /// At the end each thread does what it was asked and lets go of its
    /// handle before the display goes, except one still waiting on a paste,
    /// which keeps its handle (and so the display open) until the process
    /// ends.
    #[test]
    fn shutting_down_lets_go_of_every_handle_but_a_waiting_one() {
        let copier = Fake::new(0);
        let (written, dropped) = (Arc::clone(&copier.written), Arc::clone(&copier.dropped.0));
        let (stuck, _never) = Fake::gated();
        let stuck_dropped = Arc::clone(&stuck.dropped.0);
        let mut fakes = vec![copier, stuck];
        let started = Cell::new(0);
        let mut spawn = spawner(&mut fakes, &started);
        let mut threads = Threads::default();
        let pasted = threads.paste(Target::Clipboard, SHORT, &mut spawn);
        assert_eq!(pasted, Err(Unanswered::Owner));
        threads.copy(Target::Clipboard, "kept".into(), &mut spawn);
        drop(spawn);
        let asked = Instant::now();
        threads.shut_down(Duration::from_secs(5));
        assert!(asked.elapsed() < Duration::from_secs(4));
        assert_eq!(texts(&written), ["kept"]);
        assert!(dropped.load(Ordering::SeqCst));
        assert!(!stuck_dropped.load(Ordering::SeqCst));
    }

    /// A handle is counted from before its thread starts, so that quitting
    /// before the thread has run still keeps the display open for it, and
    /// until it has been let go of.
    #[test]
    fn a_handle_is_counted_before_its_thread_runs() {
        struct Holding(Fake, Counted);

        impl Selections for Holding {
            fn read(&self, target: Target) -> std::io::Result<String> {
                self.0.read(target)
            }

            fn write(&self, target: Target, text: String) {
                self.0.write(target, text);
            }
        }

        let _counting = COUNTING.lock().unwrap_or_else(|error| error.into_inner());
        assert!(!in_use());
        let (run, wait) = mpsc::channel::<()>();
        let thread = spawn_counted(move |counted| {
            let _ = wait.recv();
            Holding(Fake::new(0), counted)
        })
        .unwrap();
        assert!(in_use());
        run.send(()).unwrap();
        let mut threads = Threads {
            copies: Some(thread),
            ..Threads::default()
        };
        threads.copy(Target::Clipboard, "copied".into(), || unreachable!());
        assert!(in_use());
        threads.shut_down(Duration::from_secs(5));
        assert!(!in_use());
    }

    /// The focus moving between Patina's own windows, in either order, is
    /// not the focus leaving Patina, which only a batch of events after
    /// which no window of Patina's has it is.
    #[test]
    fn the_focus_leaves_only_when_no_window_of_patinas_has_it() {
        let focus = Focus::new();
        assert!(!focus.left());
        focus.gained(1);
        assert!(!focus.left());
        assert_eq!(focus.window(), Some(1));

        // From the player to Preferences, the old window first or the new.
        focus.lost(1);
        focus.gained(2);
        assert!(!focus.left());
        focus.gained(1);
        focus.lost(2);
        assert!(!focus.left());
        assert_eq!(focus.window(), Some(1));

        // A window that goes without the focus changes nothing.
        focus.lost(3);
        assert!(!focus.left());

        // To another program and back, all while Patina was busy.
        focus.lost(1);
        focus.gained(1);
        assert!(focus.left());
        assert!(!focus.left());
        assert_eq!(focus.window(), Some(1));

        // Another program's window: told once.
        focus.lost(1);
        assert_eq!(focus.window(), None);
        assert!(focus.left());
        assert!(!focus.left());
        focus.gained(2);
        assert!(!focus.left());
    }

    /// The compositor's two selections as every handle sees them, and the
    /// programs that set them, by number: Patina is 0, which answers at
    /// once, and any other may hang until it is thawed.
    struct World {
        selections: Mutex<[(usize, String); 2]>,
        hung: Mutex<Vec<bool>>,
        thawed: Condvar,
    }

    struct Seen(Arc<World>);

    impl Selections for Seen {
        fn read(&self, target: Target) -> std::io::Result<String> {
            let (owner, text) = self.0.selections.lock().unwrap()[target.index()].clone();
            let mut hung = self.0.hung.lock().unwrap();
            while hung[owner] {
                hung = self.0.thawed.wait(hung).unwrap();
            }
            Ok(text)
        }

        fn write(&self, target: Target, text: String) {
            self.0.selections.lock().unwrap()[target.index()] = (0, text);
        }
    }

    /// Hangs, answers, copies, focus changes and pastes, many of them, in
    /// a seeded order: copies always land, a paste gets the selection's
    /// text or nothing, never text from before, and always Patina's own
    /// copy, at once; and there are never more threads than there should
    /// be.
    #[test]
    fn copies_land_and_pastes_are_never_stale() {
        let world = Arc::new(World {
            selections: Mutex::new([(1, "first".into()), (1, "first".into())]),
            hung: Mutex::new(vec![false, false]),
            thawed: Condvar::new(),
        });
        let started = Cell::new(0);
        let mut spawn = || {
            started.set(started.get() + 1);
            let world = Arc::clone(&world);
            Thread::spawn(move || Seen(world))
        };
        let mut threads = Threads::default();
        threads.start(&mut spawn);
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let current = |target: Target| world.selections.lock().unwrap()[target.index()].clone();
        for round in 0..300 {
            let target = if next() % 2 == 0 {
                Target::Clipboard
            } else {
                Target::Primary
            };
            match next() % 8 {
                // Patina copies, and pastes what it copied, at once.
                0 => {
                    let text = format!("patina {round}");
                    threads.copy(target, text.clone(), &mut spawn);
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while current(target) != (0, text.clone()) {
                        assert!(
                            Instant::now() < deadline,
                            "round {round}: the copy never landed"
                        );
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    let asked = Instant::now();
                    let pasted = threads.paste(target, TIMEOUT, &mut spawn);
                    assert_eq!(pasted, Ok(Some(text)), "round {round}");
                    assert!(asked.elapsed() < Duration::from_secs(1), "round {round}");
                }
                // The focus leaves for another program, which copies, and
                // may hang; then it comes back.
                1 => {
                    let mut hung = world.hung.lock().unwrap();
                    hung.push(next() % 2 == 0);
                    let program = hung.len() - 1;
                    drop(hung);
                    world.selections.lock().unwrap()[target.index()] =
                        (program, format!("program {program}"));
                    threads.focus_left();
                }
                // Now and then, a hung program answers at last.
                2 if next() % 3 == 0 => {
                    let mut hung = world.hung.lock().unwrap();
                    if let Some(program) = hung.iter().rposition(|hung| *hung) {
                        hung[program] = false;
                        world.thawed.notify_all();
                    }
                }
                // The focus goes elsewhere and back, and nothing changes.
                3 => threads.focus_left(),
                _ => {
                    let before = current(target);
                    if let Ok(text) = threads.paste(target, SHORT, &mut spawn) {
                        assert_eq!(text, Some(before.1), "round {round}");
                    }
                }
            }
            assert!(threads.pastes.len() <= PASTE_THREADS, "round {round}");
            assert!(started.get() <= PASTE_THREADS + 1, "round {round}");
        }
        world.hung.lock().unwrap().fill(false);
        world.thawed.notify_all();
        wait_for_answers(&mut threads, 0);
        let asked = Instant::now();
        threads.shut_down(Duration::from_secs(5));
        assert!(asked.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn nothing_without_a_window() {
        assert!(!on_wayland());
        assert_eq!(text(Clipboard::DefaultClipboard), None);
        settle();
        shut_down();
        assert_eq!(text(Clipboard::DefaultClipboard), None);
    }
}
