//! Keeps the screen awake while something is playing, or for music only
//! the computer.
//!
//! mpv cannot do this itself when it has no window of its own, so Patina
//! asks the desktop directly: the `org.freedesktop.ScreenSaver` D-Bus
//! interface on Linux (KDE, GNOME and most others implement it), and for
//! the computer alone, or for both in a Flatpak, the desktop portal's
//! Inhibit; `SetThreadExecutionState` on Windows, and an IOKit power
//! assertion on macOS. The calls run on a small thread of their own so a
//! slow D-Bus never stalls the interface.

/// What to keep awake, weakest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Level {
    #[default]
    None,
    /// The computer, letting the screen sleep.
    System,
    /// The screen, and with it the computer.
    Display,
}

pub struct Inhibitor {
    requests: async_channel::Sender<Level>,
}

impl Inhibitor {
    pub fn new() -> Self {
        let (requests, receiver) = async_channel::unbounded::<Level>();
        let spawned = std::thread::Builder::new()
            .name("inhibit".into())
            .spawn(move || run(receiver));
        if let Err(error) = spawned {
            log::warn!("could not start the screen saver inhibitor: {error}");
        }
        Self { requests }
    }

    pub fn set(&self, level: Level) {
        let _ = self.requests.try_send(level);
    }
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn run(receiver: async_channel::Receiver<Level>) {
    let mut backend = Backend::default();
    while let Ok(wanted) = receiver.recv_blocking() {
        backend.set(wanted);
    }
    backend.set(Level::None);
}

/// Linux: `org.freedesktop.ScreenSaver.Inhibit` for the screen, and the
/// portal's Inhibit for the computer alone (which the ScreenSaver
/// interface has no way to ask for), through zbus, which is async and
/// driven on this thread. In a Flatpak the portal keeps the screen awake
/// too: every sandbox reaches the portal, and none the screen saver unless
/// its manifest asks.
#[cfg(all(unix, not(target_os = "macos")))]
fn run(receiver: async_channel::Receiver<Level>) {
    zbus::block_on(async move {
        let connection = match zbus::Connection::session().await {
            Ok(connection) => connection,
            Err(error) => {
                log::warn!("no D-Bus session bus; the screen may sleep during playback: {error}");
                return;
            }
        };
        let mut held = linux::Held::default();
        loop {
            let next = receiver.recv().await;
            // Shutting down releases the inhibition too.
            held.set(&connection, next.unwrap_or_default()).await;
            if next.is_err() {
                break;
            }
        }
    });
}

#[cfg(all(unix, not(target_os = "macos")))]
mod linux {
    use super::Level;
    use std::collections::HashMap;
    use zbus::{Connection, OwnedObjectPath, Variant};

    const SCREENSAVER: &str = "org.freedesktop.ScreenSaver";
    const SCREENSAVER_PATH: &str = "/org/freedesktop/ScreenSaver";
    const PORTAL: &str = "org.freedesktop.portal.Desktop";
    const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
    /// The portal's flag for keeping the session from suspending.
    const SUSPEND: u32 = 4;
    /// And from going idle, which is what blanks the screen.
    const IDLE: u32 = 8;

    /// What keeps the screen awake: the screen saver's cookie, or in a
    /// Flatpak the portal's request.
    enum Screen {
        Cookie(u32),
        Request(OwnedObjectPath),
    }

    impl std::fmt::Display for Screen {
        fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            match self {
                Screen::Cookie(value) => write!(f, "cookie {value}"),
                Screen::Request(request) => write!(f, "{request}"),
            }
        }
    }

    /// What is held: the screen, and the portal's request for the computer
    /// alone.
    #[derive(Default)]
    pub struct Held {
        level: Level,
        screen: Option<Screen>,
        request: Option<OwnedObjectPath>,
    }

    impl Held {
        pub async fn set(&mut self, connection: &Connection, level: Level) {
            if level == self.level {
                return;
            }
            self.level = level;
            let mut display = level == Level::Display;
            if level == Level::System && self.request.is_none() {
                match inhibit(connection, SUSPEND, "Playing audio").await {
                    Ok(request) => {
                        log::debug!("system sleep inhibited ({request})");
                        self.request = Some(request);
                    }
                    // Kept awake all the same, screen and all.
                    Err(error) => {
                        log::warn!("could not keep the computer awake alone: {error}");
                        display = true;
                    }
                }
            }
            if display && self.screen.is_none() {
                match inhibit_screen(connection).await {
                    Ok(screen) => {
                        log::debug!("screen saver inhibited ({screen})");
                        self.screen = Some(screen);
                    }
                    Err(error) => log::warn!("could not keep the screen awake: {error}"),
                }
            }
            if !display && let Some(screen) = self.screen.take() {
                let released = match screen {
                    Screen::Cookie(value) => connection
                        .call_method(
                            Some(SCREENSAVER),
                            SCREENSAVER_PATH,
                            Some(SCREENSAVER),
                            "UnInhibit",
                            &(value,),
                        )
                        .await
                        .map(drop),
                    Screen::Request(request) => close(connection, &request).await,
                };
                match released {
                    Ok(()) => log::debug!("screen saver released"),
                    Err(error) => log::warn!("could not release the screen saver: {error}"),
                }
            }
            if level != Level::System
                && let Some(request) = self.request.take()
            {
                match close(connection, &request).await {
                    Ok(()) => log::debug!("system sleep released"),
                    Err(error) => log::warn!("could not release system sleep: {error}"),
                }
            }
        }
    }

    /// The screen saver's Inhibit, or in a Flatpak the portal's, asked to
    /// keep the computer from suspending too, as [`Level::Display`] means.
    async fn inhibit_screen(connection: &Connection) -> zbus::Result<Screen> {
        if crate::platform::flatpak() {
            return inhibit(connection, IDLE | SUSPEND, "Playing media")
                .await
                .map(Screen::Request);
        }
        connection
            .call_method(
                Some(SCREENSAVER),
                SCREENSAVER_PATH,
                Some(SCREENSAVER),
                "Inhibit",
                &("Patina", "Playing media"),
            )
            .await?
            .body()
            .deserialize::<u32>()
            .map(Screen::Cookie)
    }

    /// The portal's request handle, which holds the inhibition until it is
    /// closed or Patina disconnects.
    async fn inhibit(
        connection: &Connection,
        flags: u32,
        reason: &str,
    ) -> zbus::Result<OwnedObjectPath> {
        let options = HashMap::from([("reason", Variant::from(reason))]);
        connection
            .call_method(
                Some(PORTAL),
                PORTAL_PATH,
                Some("org.freedesktop.portal.Inhibit"),
                "Inhibit",
                &("", flags, options),
            )
            .await?
            .body()
            .deserialize::<OwnedObjectPath>()
    }

    async fn close(connection: &Connection, request: &OwnedObjectPath) -> zbus::Result<()> {
        connection
            .call_method(
                Some(PORTAL),
                request.as_str(),
                Some("org.freedesktop.portal.Request"),
                "Close",
                &(),
            )
            .await
            .map(drop)
    }
}

#[cfg(target_os = "windows")]
#[derive(Default)]
struct Backend {
    level: Level,
}

#[cfg(target_os = "windows")]
impl Backend {
    fn set(&mut self, level: Level) {
        if level == self.level {
            return;
        }
        self.level = level;
        use windows_sys::Win32::System::Power::{
            ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
        };
        let flags = match level {
            Level::Display => ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED,
            Level::System => ES_CONTINUOUS | ES_SYSTEM_REQUIRED,
            Level::None => ES_CONTINUOUS,
        };
        // SAFETY: plain flag call; the state belongs to this thread, which
        // lives as long as the inhibitor.
        if unsafe { SetThreadExecutionState(flags) } == 0 {
            log::warn!("could not change the display sleep state");
        }
    }
}

#[cfg(target_os = "macos")]
#[derive(Default)]
struct Backend {
    assertion: Option<(Level, u32)>,
}

#[cfg(target_os = "macos")]
mod iokit {
    use std::ffi::{c_char, c_void};

    pub type CFStringRef = *const c_void;
    pub const UTF8: u32 = 0x0800_0100;
    pub const ASSERTION_LEVEL_ON: u32 = 255;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        pub fn CFStringCreateWithCString(
            alloc: *const c_void,
            text: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
        pub fn CFRelease(object: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        pub fn IOPMAssertionCreateWithName(
            kind: CFStringRef,
            level: u32,
            name: CFStringRef,
            id: *mut u32,
        ) -> i32;
        pub fn IOPMAssertionRelease(id: u32) -> i32;
    }
}

#[cfg(target_os = "macos")]
impl Backend {
    fn set(&mut self, level: Level) {
        use iokit::*;
        if self.assertion.is_some_and(|(held, _)| held != level)
            && let Some((_, id)) = self.assertion.take()
        {
            // SAFETY: releases an assertion this backend created.
            unsafe { IOPMAssertionRelease(id) };
        }
        let kind = match level {
            Level::Display => c"PreventUserIdleDisplaySleep",
            Level::System => c"PreventUserIdleSystemSleep",
            Level::None => return,
        };
        if self.assertion.is_none() {
            // SAFETY: the strings are NUL-terminated literals; both CF
            // objects are released after the call, which copies them.
            unsafe {
                let kind = CFStringCreateWithCString(std::ptr::null(), kind.as_ptr(), UTF8);
                let name = CFStringCreateWithCString(
                    std::ptr::null(),
                    c"Patina is playing media".as_ptr(),
                    UTF8,
                );
                let mut id = 0;
                let result = IOPMAssertionCreateWithName(kind, ASSERTION_LEVEL_ON, name, &mut id);
                CFRelease(kind);
                CFRelease(name);
                if result == 0 {
                    self.assertion = Some((level, id));
                } else {
                    log::warn!("could not keep the computer awake (IOKit error {result})");
                }
            }
        }
    }
}
