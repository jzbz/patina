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
