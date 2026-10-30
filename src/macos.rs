//! The macOS window touches winit can't make once a window exists.

use objc2::rc::Retained;
use objc2_app_kit::{NSView, NSWindow, NSWindowButton, NSWindowStyleMask, NSWindowTitleVisibility};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::WinitWindowAccessor;

/// The window's NSWindow, once winit has made it.
fn ns_window(window: &slint::Window) -> Option<Retained<NSWindow>> {
    window
        .with_winit_window(|winit| {
            let RawWindowHandle::AppKit(handle) = winit.window_handle().ok()?.as_raw() else {
                return None;
            };
            // SAFETY: winit's content view, alive as long as its window, and
            // this runs on the main thread, where the event loop does.
            let view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
            view.window()
        })
        .flatten()
}

/// IINA's title bar: transparent, with the video running under it and only
/// macOS's window buttons left; Patina draws the title over the video.
/// False until the window exists.
pub fn use_transparent_titlebar(window: &slint::Window) -> bool {
    let Some(ns_window) = ns_window(window) else {
        return false;
    };
    ns_window.setTitlebarAppearsTransparent(true);
    ns_window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    ns_window.setStyleMask(ns_window.styleMask() | NSWindowStyleMask::FullSizeContentView);
    true
}

/// Shows or hides the close, minimize and zoom buttons, which fade with the
/// controls in IINA.
pub fn set_window_buttons_visible(window: &slint::Window, visible: bool) {
    let Some(ns_window) = ns_window(window) else {
        return;
    };
    for kind in [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ] {
        if let Some(button) = ns_window.standardWindowButton(kind) {
            button.setHidden(!visible);
        }
    }
}

// ---- Opening from Finder and links --------------------------------------

use objc2::runtime::{AnyObject, NSObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_foundation::{
    NSDictionary, NSNotification, NSNotificationCenter, NSNumber, NSString, NSUserDefaults,
    ns_string,
};

/// An Apple event class, ID or keyword: four characters.
const fn code(text: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*text)
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and OpenEvents
    // doesn't implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PatinaOpenEvents"]
    struct OpenEvents;

    impl OpenEvents {
        #[unsafe(method(willFinishLaunching:))]
        fn will_finish_launching(&self, _notification: &NSNotification) {
            self.install();
        }

        #[unsafe(method(openDocuments:withReply:))]
        fn open_documents(&self, event: &AnyObject, _reply: &AnyObject) {
            open(documents(event));
        }

        #[unsafe(method(getURL:withReply:))]
        fn get_url(&self, event: &AnyObject, _reply: &AnyObject) {
            open(direct_text(event).into_iter().collect());
        }
    }
);

impl OpenEvents {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's init.
        unsafe { msg_send![super(this), init] }
    }

    /// Takes the open-documents event from AppKit's own handler, which
    /// would ask winit's application delegate, and the get-URL event.
    fn install(&self) {
        // SAFETY: the shared manager, given this object's own methods,
        // which take the event and the reply as Apple event handlers do.
        unsafe {
            let manager: Retained<AnyObject> =
                msg_send![objc2::class!(NSAppleEventManager), sharedAppleEventManager];
            let _: () = msg_send![
                &manager,
                setEventHandler: self,
                andSelector: sel!(openDocuments:withReply:),
                forEventClass: code(b"aevt"),
                andEventID: code(b"odoc")
            ];
            let _: () = msg_send![
                &manager,
                setEventHandler: self,
                andSelector: sel!(getURL:withReply:),
                forEventClass: code(b"GURL"),
                andEventID: code(b"GURL")
            ];
        }
    }
}

/// Finder's opens and patina:// links reach a running app as Apple events,
/// which winit's application delegate leaves alone. Patina handles them
/// itself, from when the app finishes launching (after AppKit installs its
/// own handlers, and before it delivers any event), as IINA does.
pub fn listen_for_opens() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    // SAFETY: on the main thread, with objects of the types each call
    // takes; the notification center doesn't keep its observers alive, so
    // OpenEvents lives for good.
    unsafe {
        // Files named on the command line open from it, not also as an
        // open-documents event.
        let no = NSNumber::new_bool(false);
        let defaults = NSDictionary::<NSString, AnyObject>::from_slices(
            &[ns_string!("NSTreatUnknownArgumentsAsOpen")],
            &[&no],
        );
        NSUserDefaults::standardUserDefaults().registerDefaults(&defaults);
        let events = OpenEvents::new(mtm);
        NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
            &events,
            sel!(willFinishLaunching:),
            Some(objc2_app_kit::NSApplicationWillFinishLaunchingNotification),
            None,
        );
        std::mem::forget(events);
    }
}

/// The files of an open-documents event.
fn documents(event: &AnyObject) -> Vec<String> {
    // SAFETY: NSAppleEventDescriptor's methods, on the event, its list of
    // files and their descriptors.
    unsafe {
        let list: Option<Retained<AnyObject>> =
            msg_send![event, paramDescriptorForKeyword: code(b"----")];
        let Some(list) = list else {
            return Vec::new();
        };
        let count: isize = msg_send![&list, numberOfItems];
        let items: Vec<Retained<AnyObject>> = if count > 0 {
            (1..=count)
                .filter_map(|index| msg_send![&list, descriptorAtIndex: index])
                .collect()
        } else {
            vec![list]
        };
        items
            .iter()
            .filter_map(|item| {
                let url: Option<Retained<AnyObject>> = msg_send![item, fileURLValue];
                let path: Option<Retained<NSString>> = msg_send![&url?, path];
                Some(path?.to_string())
            })
            .collect()
    }
}

/// The text of an event's direct object: a get-URL event's URL.
fn direct_text(event: &AnyObject) -> Option<String> {
    // SAFETY: NSAppleEventDescriptor's methods.
    unsafe {
        let direct: Option<Retained<AnyObject>> =
            msg_send![event, paramDescriptorForKeyword: code(b"----")];
        let text: Option<Retained<NSString>> = msg_send![&direct?, stringValue];
        Some(text?.to_string())
    }
}

/// Hands what the system opened to Patina, which opens it once it has
/// started if it is only starting.
fn open(sources: Vec<String>) {
    if sources.is_empty() {
        return;
    }
    let request = crate::instance::Request {
        sources,
        ..Default::default()
    };
    let _ = slint::invoke_from_event_loop(move || {
        crate::app::App::with_current(|app| {
            app.open_request(request);
        });
    });
}
