//! Bringing a window to the front on Wayland. A Wayland client can't raise
//! its own window; it hands the compositor a token from whatever the user
//! acted in (the file manager or launcher that started a Patina, which then
//! passed the token on), and winit 0.30 has no call for that on a window
//! that already exists.

use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use slint::winit_030::WinitWindowAccessor;
use std::cell::RefCell;
use std::mem::ManuallyDrop;
use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_surface::WlSurface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::xdg::activation::v1::client::xdg_activation_v1::XdgActivationV1;

/// Patina's own view of winit's connection: a queue of its own, so that
/// winit's events stay winit's, and the activation global, bound once (a
/// registry, once made, lasts as long as the connection).
struct Activation {
    connection: Connection,
    queue: EventQueue<State>,
    global: XdgActivationV1,
}

thread_local! {
    /// Never dropped: winit may have closed the display by the time this
    /// would go, at exit. None for a display without xdg_activation_v1,
    /// which is asked once: each try makes a registry the compositor keeps.
    static ACTIVATION: RefCell<Option<(usize, Option<ManuallyDrop<Activation>>)>> =
        const { RefCell::new(None) };
}

/// Activates `window` with `token`; does nothing off Wayland.
pub fn activate(window: &slint::Window, token: &str) {
    let handles = window
        .with_winit_window(|winit| {
            let display = winit.display_handle().ok()?.as_raw();
            let surface = winit.window_handle().ok()?.as_raw();
            match (display, surface) {
                (RawDisplayHandle::Wayland(display), RawWindowHandle::Wayland(surface)) => {
                    Some((display.display, surface.surface))
                }
                _ => None,
            }
        })
        .flatten();
    let Some((display, surface)) = handles else {
        return;
    };
    let result = ACTIVATION.with(|activation| {
        let mut activation = activation.borrow_mut();
        let key = display.as_ptr() as usize;
        if activation.as_ref().is_none_or(|(known, _)| *known != key) {
            // SAFETY: winit's display, which lives as long as the event
            // loop, used on the thread winit uses it on.
            let bound = unsafe { bind(display.as_ptr()) };
            if let Err(error) = &bound {
                log::debug!("no window activation: {error}");
            }
            *activation = Some((key, bound.ok().map(ManuallyDrop::new)));
        }
        let Some((_, Some(activation))) = activation.as_mut() else {
            return Ok(());
        };
        let activation: &mut Activation = activation;
        // SAFETY: the window's live wl_surface, on winit's thread.
        unsafe { send(activation, surface.as_ptr(), token) }
    });
    if let Err(error) = result {
        log::debug!("could not activate the window: {error}");
    }
}

/// # Safety
/// `display` must be a live wl_display.
unsafe fn bind(display: *mut std::ffi::c_void) -> Result<Activation, String> {
    // SAFETY: as the caller promises; the connection only borrows it.
    let backend = unsafe { Backend::from_foreign_display(display.cast()) };
    let connection = Connection::from_backend(backend);
    let (globals, queue) =
        registry_queue_init::<State>(&connection).map_err(|error| error.to_string())?;
    let global = globals
        .bind(&queue.handle(), 1..=1, ())
        .map_err(|error| format!("no xdg_activation_v1: {error}"))?;
    Ok(Activation {
        connection,
        queue,
        global,
    })
}

/// # Safety
/// `surface` must be a live wl_surface of the activation's display.
unsafe fn send(
    activation: &mut Activation,
    surface: *mut std::ffi::c_void,
    token: &str,
) -> Result<(), String> {
    // SAFETY: as the caller promises.
    let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.cast()) }
        .map_err(|error| error.to_string())?;
    let surface =
        WlSurface::from_id(&activation.connection, id).map_err(|error| error.to_string())?;
    activation.global.activate(token.to_owned(), &surface);
    activation
        .queue
        .roundtrip(&mut State)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

struct State;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgActivationV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &XdgActivationV1,
        _: <XdgActivationV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
