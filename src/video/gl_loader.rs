//! Resolves OpenGL functions for mpv outside of Slint's rendering callback.
//!
//! Slint hands out a `get_proc_address` that is only valid during its
//! rendering notifier. mpv keeps the resolver for the render context's whole
//! life, so it gets a process-wide resolver backed by the platform's GL
//! libraries instead.

use libloading::Library;
use std::ffi::{CStr, c_char, c_void};
use std::sync::OnceLock;

#[cfg(target_os = "windows")]
type NativeLoader = unsafe extern "system" fn(*const c_char) -> *mut c_void;
#[cfg(not(target_os = "windows"))]
type NativeLoader = unsafe extern "C" fn(*const c_char) -> *mut c_void;

struct Resolver {
    /// Platform `*GetProcAddress` functions, tried in order.
    loaders: Vec<NativeLoader>,
    /// GL libraries whose exports are searched last (core GL 1.x functions on
    /// Windows and macOS are only found this way).
    libraries: Vec<Library>,
}

static RESOLVER: OnceLock<Result<Resolver, String>> = OnceLock::new();

fn resolver() -> Result<&'static Resolver, String> {
    RESOLVER
        .get_or_init(Resolver::load)
        .as_ref()
        .map_err(Clone::clone)
}

/// Loads the platform GL libraries; call once before creating a renderer.
pub fn initialize() -> Result<(), String> {
    resolver().map(|_| ())
}

/// The resolver passed to `mpv_render_context_create`.
pub unsafe extern "C" fn get_proc_address(_ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    if name.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: mpv passes a valid NUL-terminated symbol name.
    let name = unsafe { CStr::from_ptr(name) };
    match resolver() {
        Ok(resolver) => resolver.resolve(name),
        Err(_) => std::ptr::null_mut(),
    }
}

fn open(candidates: &[&str]) -> Option<Library> {
    // SAFETY: system GL libraries have no initialization preconditions.
    candidates
        .iter()
        .find_map(|name| unsafe { Library::new(*name) }.ok())
}

#[cfg(not(target_os = "macos"))]
fn loader(library: &Library, symbol: &[u8]) -> Option<NativeLoader> {
    // SAFETY: the named symbols are GetProcAddress-style functions with the
    // NativeLoader signature on their platforms.
    unsafe {
        library
            .get::<NativeLoader>(symbol)
            .ok()
            .map(|symbol| *symbol)
    }
}

impl Resolver {
    fn load() -> Result<Self, String> {
        // macOS resolves everything through the framework's exports.
        #[cfg_attr(target_os = "macos", allow(unused_mut))]
        let mut loaders = Vec::new();
        let mut libraries = Vec::new();

        #[cfg(all(unix, not(target_os = "macos")))]
        {
            // Slint uses EGL on Wayland and on most X11 setups; GLX is the
            // fallback. Under libglvnd both dispatch to the current context.
            let egl = open(&["libEGL.so.1", "libEGL.so"]);
            let gl = open(&["libGL.so.1", "libGL.so"]);
            loaders.extend(
                egl.as_ref()
                    .and_then(|lib| loader(lib, b"eglGetProcAddress\0")),
            );
            loaders.extend(gl.as_ref().and_then(|lib| {
                loader(lib, b"glXGetProcAddressARB\0")
                    .or_else(|| loader(lib, b"glXGetProcAddress\0"))
            }));
            libraries.extend(egl);
            libraries.extend(gl);
            libraries.extend(open(&["libGLESv2.so.2", "libGLESv2.so"]));
        }

        #[cfg(target_os = "macos")]
        {
            libraries.extend(open(&[
                "/System/Library/Frameworks/OpenGL.framework/OpenGL",
            ]));
        }

        #[cfg(target_os = "windows")]
        {
            let gl = open(&["opengl32.dll"]);
            loaders.extend(
                gl.as_ref()
                    .and_then(|lib| loader(lib, b"wglGetProcAddress\0")),
            );
            libraries.extend(gl);
        }

        if loaders.is_empty() && libraries.is_empty() {
            return Err("could not load the system OpenGL libraries".into());
        }
        Ok(Self { loaders, libraries })
    }

    fn resolve(&self, name: &CStr) -> *mut c_void {
        for loader in &self.loaders {
            // SAFETY: a platform loader called with a valid symbol name.
            let address = unsafe { loader(name.as_ptr()) };
            // wglGetProcAddress signals failure with small sentinel values.
            if !address.is_null() && !(cfg!(windows) && matches!(address as isize, 1 | 2 | 3 | -1))
            {
                return address;
            }
        }
        for library in &self.libraries {
            // SAFETY: only the address is taken; it is never called here.
            let symbol = unsafe { library.get::<*mut c_void>(name.to_bytes_with_nul()) };
            if let Ok(symbol) = symbol {
                return *symbol;
            }
        }
        std::ptr::null_mut()
    }
}
