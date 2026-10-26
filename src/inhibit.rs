//! Keeps the screen awake while something is playing.
//!
//! mpv cannot do this itself when it has no window of its own, so Patina
//! asks the desktop directly: the `org.freedesktop.ScreenSaver` D-Bus
//! interface on Linux (KDE, GNOME and most others implement it),
//! `SetThreadExecutionState` on Windows, and an IOKit power assertion on
//! macOS. The calls run on a small thread of their own so a slow D-Bus
//! never stalls the interface.

pub struct Inhibitor {
    requests: async_channel::Sender<bool>,
}

impl Inhibitor {
    pub fn new() -> Self {
        let (requests, receiver) = async_channel::unbounded::<bool>();
        let spawned = std::thread::Builder::new()
            .name("inhibit".into())
            .spawn(move || run(receiver));
        if let Err(error) = spawned {
            log::warn!("could not start the screen saver inhibitor: {error}");
        }
        Self { requests }
    }

    /// Requests the screen to stay awake (true) or allows it to sleep.
    pub fn set(&self, inhibit: bool) {
        let _ = self.requests.try_send(inhibit);
    }
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn run(receiver: async_channel::Receiver<bool>) {
    let mut backend = Backend::default();
    while let Ok(wanted) = receiver.recv_blocking() {
        backend.set(wanted);
    }
    backend.set(false);
}

/// Linux: `org.freedesktop.ScreenSaver.Inhibit`, through zbus, which is
/// async and driven on this thread.
#[cfg(all(unix, not(target_os = "macos")))]
fn run(receiver: async_channel::Receiver<bool>) {
    const SERVICE: &str = "org.freedesktop.ScreenSaver";
    const PATH: &str = "/org/freedesktop/ScreenSaver";

    zbus::block_on(async move {
        let connection = match zbus::Connection::session().await {
            Ok(connection) => connection,
            Err(error) => {
                log::warn!("no D-Bus session bus; the screen may sleep during playback: {error}");
                return;
            }
        };
        let mut cookie: Option<u32> = None;
        let mut wanted = false;
        loop {
            let next = receiver.recv().await;
            // Shutting down releases the inhibition too.
            let inhibit = *next.as_ref().unwrap_or(&false);
            if inhibit != wanted || next.is_err() {
                wanted = inhibit;
                if inhibit && cookie.is_none() {
                    let reply = connection
                        .call_method(
                            Some(SERVICE),
                            PATH,
                            Some(SERVICE),
                            "Inhibit",
                            &("Patina", "Playing media"),
                        )
                        .await;
                    match reply.and_then(|reply| reply.body().deserialize::<u32>()) {
                        Ok(value) => {
                            log::debug!("screen saver inhibited (cookie {value})");
                            cookie = Some(value);
                        }
                        Err(error) => log::warn!("could not keep the screen awake: {error}"),
                    }
                } else if !inhibit && let Some(value) = cookie.take() {
                    match connection
                        .call_method(Some(SERVICE), PATH, Some(SERVICE), "UnInhibit", &(value,))
                        .await
                    {
                        Ok(_) => log::debug!("screen saver released"),
                        Err(error) => log::warn!("could not release the screen saver: {error}"),
                    }
                }
            }
            if next.is_err() {
                break;
            }
        }
    });
}

#[cfg(target_os = "windows")]
#[derive(Default)]
struct Backend {
    inhibiting: bool,
}

#[cfg(target_os = "windows")]
impl Backend {
    fn set(&mut self, inhibit: bool) {
        if inhibit == self.inhibiting {
            return;
        }
        self.inhibiting = inhibit;
        use windows_sys::Win32::System::Power::{
            ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
        };
        let flags = if inhibit {
            ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED
        } else {
            ES_CONTINUOUS
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
    assertion: Option<u32>,
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
    fn set(&mut self, inhibit: bool) {
        use iokit::*;
        if inhibit && self.assertion.is_none() {
            // SAFETY: the strings are NUL-terminated literals; both CF
            // objects are released after the call, which copies them.
            unsafe {
                let kind = CFStringCreateWithCString(
                    std::ptr::null(),
                    c"PreventUserIdleDisplaySleep".as_ptr(),
                    UTF8,
                );
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
                    self.assertion = Some(id);
                } else {
                    log::warn!("could not keep the display awake (IOKit error {result})");
                }
            }
        } else if !inhibit && let Some(id) = self.assertion.take() {
            // SAFETY: releases an assertion this backend created.
            unsafe { IOPMAssertionRelease(id) };
        }
    }
}
