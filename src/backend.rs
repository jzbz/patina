//! Patina's Slint platform: Slint's winit backend, with the Wayland
//! clipboard in place of its own on Linux and the BSDs (see clipboard.rs
//! for why). On macOS and Windows it is Slint's winit backend as it comes.

#[cfg(all(unix, not(target_os = "macos")))]
use slint::{
    PlatformError,
    platform::{Clipboard, EventLoopProxy, Platform, WindowAdapter},
};
#[cfg(all(unix, not(target_os = "macos")))]
use std::{mem::ManuallyDrop, rc::Rc, time::Duration};

/// Makes Slint draw its windows with winit, in software or with OpenGL, and
/// tell `handler` about winit's events.
pub fn select(
    software: bool,
    handler: impl slint::winit_030::CustomApplicationHandler + 'static,
) -> Result<(), slint::PlatformError> {
    let renderer = if software { "software" } else { "femtovg" };
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let inner = i_slint_backend_winit::Backend::builder()
            .with_renderer_name(renderer)
            .with_custom_application_handler(Box::new(handler))
            .build()?;
        slint::platform::set_platform(Box::new(Backend {
            inner: ManuallyDrop::new(inner),
        }))
        .map_err(slint::PlatformError::SetPlatformError)
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name(renderer.into())
        .with_winit_custom_application_handler(handler)
        .select()
}

/// Slint's winit backend, `inner`, with the clipboard going through
/// clipboard.rs on Wayland. All else is the backend's own: every method of
/// Platform passes the call on, those the backend leaves to the trait's
/// defaults too, since a method left out here would quietly take the
/// trait's default whatever the backend does (without the event loop proxy,
/// slint::invoke_from_event_loop fails, for one). Some of them name types of
/// i_slint_core, which is why Patina depends on it, at Slint's exact version
/// as on the winit backend. Those two exact requirements in Cargo.toml hold
/// Slint where it is: `cargo update` leaves it there without a word when a
/// newer Slint comes out, so moving to one means raising both by hand along
/// with slint's own requirement, and passing on here any method Platform
/// has gained.
#[cfg(all(unix, not(target_os = "macos")))]
struct Backend<P> {
    /// Let go of only once clipboard.rs has no worker left on winit's
    /// display (see Drop).
    inner: ManuallyDrop<P>,
}

#[cfg(all(unix, not(target_os = "macos")))]
impl<P> Drop for Backend<P> {
    /// Lets go of Slint's winit backend, which holds winit's event loop
    /// and with it the display, closed once the backend and its windows
    /// have gone; unless a worker of the Wayland clipboard may still be
    /// dispatching events on the display: one whose paste the clipboard's
    /// owner has never answered (clipboard.rs). Freed under it, the display
    /// would be used after it was gone should the owner answer or exit as
    /// Patina exits, at a logout, say. The backend goes with the process
    /// then, as it would a moment later anyway.
    fn drop(&mut self) {
        if !crate::clipboard::in_use() {
            // SAFETY: let go of once, here, and never used after.
            unsafe { ManuallyDrop::drop(&mut self.inner) };
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
impl<P: Platform> Platform for Backend<P> {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let window = self.inner.create_window_adapter()?;
        crate::clipboard::watch(&window);
        Ok(window)
    }

    fn run_event_loop(&self) -> Result<(), PlatformError> {
        self.inner.run_event_loop()
    }

    fn process_events(
        &self,
        timeout: Option<Duration>,
        token: i_slint_core::InternalToken,
    ) -> Result<std::ops::ControlFlow<()>, PlatformError> {
        self.inner.process_events(timeout, token)
    }

    fn bind_context(
        &self,
        context: i_slint_core::SlintContextWeak,
        token: i_slint_core::InternalToken,
    ) {
        self.inner.bind_context(context, token);
    }

    #[allow(deprecated)]
    fn set_event_loop_quit_on_last_window_closed(&self, quit_on_last_window_closed: bool) {
        self.inner
            .set_event_loop_quit_on_last_window_closed(quit_on_last_window_closed);
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        self.inner.new_event_loop_proxy()
    }

    fn duration_since_start(&self) -> Duration {
        self.inner.duration_since_start()
    }

    fn click_interval(&self) -> Duration {
        self.inner.click_interval()
    }

    fn cursor_flash_cycle(&self) -> Duration {
        self.inner.cursor_flash_cycle()
    }

    fn set_clipboard_text(&self, text: &str, clipboard: Clipboard) {
        if crate::clipboard::on_wayland() {
            crate::clipboard::set_text(clipboard, text.to_owned());
        } else {
            self.inner.set_clipboard_text(text, clipboard);
        }
    }

    fn clipboard_text(&self, clipboard: Clipboard) -> Option<String> {
        if crate::clipboard::on_wayland() {
            crate::clipboard::text(clipboard)
        } else {
            self.inner.clipboard_text(clipboard)
        }
    }

    fn debug_log(&self, arguments: std::fmt::Arguments) {
        self.inner.debug_log(arguments);
    }

    fn open_url(&self, url: &str) -> Result<(), PlatformError> {
        self.inner.open_url(url)
    }
}

#[cfg(all(test, unix, not(target_os = "macos")))]
mod tests {
    use super::*;
    use std::cell::RefCell;

    type Calls = Rc<RefCell<Vec<String>>>;

    /// A platform that notes each call, and being let go of, and answers
    /// so that the answer shows it was the one that gave it.
    struct Recorder(Calls);

    impl Drop for Recorder {
        fn drop(&mut self) {
            self.note("drop");
        }
    }

    impl Recorder {
        fn note(&self, call: impl Into<String>) {
            self.0.borrow_mut().push(call.into());
        }
    }

    struct Proxy;

    impl EventLoopProxy for Proxy {
        fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
            Err(slint::EventLoopError::EventLoopTerminated)
        }

        fn invoke_from_event_loop(
            &self,
            _: Box<dyn FnOnce() + Send>,
        ) -> Result<(), slint::EventLoopError> {
            Err(slint::EventLoopError::EventLoopTerminated)
        }
    }

    impl Platform for Recorder {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
            self.note("create_window_adapter");
            Err(PlatformError::Other("recorded".into()))
        }

        fn run_event_loop(&self) -> Result<(), PlatformError> {
            self.note("run_event_loop");
            Err(PlatformError::Other("recorded".into()))
        }

        fn process_events(
            &self,
            timeout: Option<Duration>,
            _: i_slint_core::InternalToken,
        ) -> Result<std::ops::ControlFlow<()>, PlatformError> {
            self.note(format!("process_events {timeout:?}"));
            Ok(std::ops::ControlFlow::Break(()))
        }

        fn bind_context(
            &self,
            context: i_slint_core::SlintContextWeak,
            _: i_slint_core::InternalToken,
        ) {
            self.note(format!("bind_context {}", context.upgrade().is_some()));
        }

        fn set_event_loop_quit_on_last_window_closed(&self, quit: bool) {
            self.note(format!("set_event_loop_quit_on_last_window_closed {quit}"));
        }

        fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
            self.note("new_event_loop_proxy");
            Some(Box::new(Proxy))
        }

        fn duration_since_start(&self) -> Duration {
            self.note("duration_since_start");
            Duration::from_secs(7)
        }

        fn click_interval(&self) -> Duration {
            self.note("click_interval");
            Duration::from_millis(123)
        }

        fn cursor_flash_cycle(&self) -> Duration {
            self.note("cursor_flash_cycle");
            Duration::from_millis(456)
        }

        fn set_clipboard_text(&self, text: &str, clipboard: Clipboard) {
            self.note(format!("set_clipboard_text {text} {clipboard:?}"));
        }

        fn clipboard_text(&self, clipboard: Clipboard) -> Option<String> {
            self.note(format!("clipboard_text {clipboard:?}"));
            Some("pasted".into())
        }

        fn debug_log(&self, arguments: std::fmt::Arguments) {
            self.note(format!("debug_log {arguments}"));
        }

        fn open_url(&self, url: &str) -> Result<(), PlatformError> {
            self.note(format!("open_url {url}"));
            Err(PlatformError::Other("recorded".into()))
        }
    }

    #[test]
    #[allow(deprecated)]
    fn every_call_reaches_the_winit_backend() {
        let calls = Calls::default();
        let backend = Backend {
            inner: ManuallyDrop::new(Recorder(Rc::clone(&calls))),
        };
        let recorded = |result: Result<(), PlatformError>| match result {
            Err(PlatformError::Other(message)) => message == "recorded",
            _ => false,
        };
        assert!(recorded(backend.create_window_adapter().map(|_| ())));
        assert!(recorded(backend.run_event_loop()));
        assert_eq!(
            backend
                .process_events(Some(Duration::ZERO), i_slint_core::InternalToken)
                .unwrap(),
            std::ops::ControlFlow::Break(())
        );
        backend.set_event_loop_quit_on_last_window_closed(false);
        let proxy = backend.new_event_loop_proxy().unwrap();
        assert_eq!(
            proxy.quit_event_loop(),
            Err(slint::EventLoopError::EventLoopTerminated)
        );
        assert_eq!(backend.duration_since_start(), Duration::from_secs(7));
        assert_eq!(backend.click_interval(), Duration::from_millis(123));
        assert_eq!(backend.cursor_flash_cycle(), Duration::from_millis(456));
        // No window has shown that this is Wayland, so the clipboard is
        // the backend's too.
        backend.set_clipboard_text("copied", Clipboard::SelectionClipboard);
        assert_eq!(
            backend
                .clipboard_text(Clipboard::DefaultClipboard)
                .as_deref(),
            Some("pasted")
        );
        backend.debug_log(format_args!("logged"));
        assert!(recorded(backend.open_url("https://patina.video/")));

        // A context hands itself to its platform as it is made.
        drop(i_slint_core::SlintContext::new(Box::new(backend)));

        assert_eq!(
            *calls.borrow(),
            [
                "create_window_adapter",
                "run_event_loop",
                "process_events Some(0ns)",
                "set_event_loop_quit_on_last_window_closed false",
                "new_event_loop_proxy",
                "duration_since_start",
                "click_interval",
                "cursor_flash_cycle",
                "set_clipboard_text copied SelectionClipboard",
                "clipboard_text DefaultClipboard",
                "debug_log logged",
                "open_url https://patina.video/",
                "bind_context true",
            ]
        );

        // The winit backend goes with Patina's, but not while a worker of
        // the Wayland clipboard may still be on winit's display, which it
        // would close. (Here rather than in a test of its own, which would
        // run beside this one; clipboard.rs's count too, under the lock.)
        let _counting = crate::clipboard::COUNTING
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let backend = |calls: &Calls| Backend {
            inner: ManuallyDrop::new(Recorder(Rc::clone(calls))),
        };
        let let_go = Calls::default();
        drop(backend(&let_go));
        assert_eq!(*let_go.borrow(), ["drop"]);
        let kept = Calls::default();
        let held = crate::clipboard::Counted::new();
        drop(backend(&kept));
        drop(held);
        assert!(kept.borrow().is_empty());
    }
}
