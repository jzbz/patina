//! The libmpv client and render C API (client API 2.x), resolved at runtime.
//!
//! Patina does not link against libmpv. It loads the shared library when the
//! first player starts, so builds need no mpv development files and packaged
//! builds can carry their own copy next to the executable.

#![allow(non_camel_case_types)]

use libloading::Library;
use std::ffi::{c_char, c_double, c_int, c_void};
use std::path::PathBuf;
use std::sync::OnceLock;

#[repr(C)]
pub struct mpv_handle {
    _private: [u8; 0],
}

#[repr(C)]
pub struct mpv_render_context {
    _private: [u8; 0],
}

pub type mpv_format = c_int;
pub const MPV_FORMAT_STRING: mpv_format = 1;
pub const MPV_FORMAT_FLAG: mpv_format = 3;
pub const MPV_FORMAT_INT64: mpv_format = 4;
pub const MPV_FORMAT_DOUBLE: mpv_format = 5;
pub const MPV_FORMAT_NODE: mpv_format = 6;
pub const MPV_FORMAT_NODE_ARRAY: mpv_format = 7;
pub const MPV_FORMAT_NODE_MAP: mpv_format = 8;
pub const MPV_FORMAT_BYTE_ARRAY: mpv_format = 9;

pub type mpv_event_id = c_int;
pub const MPV_EVENT_NONE: mpv_event_id = 0;
pub const MPV_EVENT_SHUTDOWN: mpv_event_id = 1;
pub const MPV_EVENT_LOG_MESSAGE: mpv_event_id = 2;
pub const MPV_EVENT_GET_PROPERTY_REPLY: mpv_event_id = 3;
pub const MPV_EVENT_SET_PROPERTY_REPLY: mpv_event_id = 4;
pub const MPV_EVENT_COMMAND_REPLY: mpv_event_id = 5;
pub const MPV_EVENT_START_FILE: mpv_event_id = 6;
pub const MPV_EVENT_END_FILE: mpv_event_id = 7;
pub const MPV_EVENT_FILE_LOADED: mpv_event_id = 8;
pub const MPV_EVENT_CLIENT_MESSAGE: mpv_event_id = 16;
pub const MPV_EVENT_VIDEO_RECONFIG: mpv_event_id = 17;
pub const MPV_EVENT_AUDIO_RECONFIG: mpv_event_id = 18;
pub const MPV_EVENT_SEEK: mpv_event_id = 20;
pub const MPV_EVENT_PLAYBACK_RESTART: mpv_event_id = 21;
pub const MPV_EVENT_PROPERTY_CHANGE: mpv_event_id = 22;
pub const MPV_EVENT_QUEUE_OVERFLOW: mpv_event_id = 24;
pub const MPV_EVENT_HOOK: mpv_event_id = 25;

pub const MPV_END_FILE_REASON_EOF: c_int = 0;
pub const MPV_END_FILE_REASON_STOP: c_int = 2;
pub const MPV_END_FILE_REASON_QUIT: c_int = 3;
pub const MPV_END_FILE_REASON_ERROR: c_int = 4;
pub const MPV_END_FILE_REASON_REDIRECT: c_int = 5;

#[repr(C)]
#[derive(Clone, Copy)]
pub union mpv_node_u {
    pub string: *mut c_char,
    pub flag: c_int,
    pub int64: i64,
    pub double_: c_double,
    pub list: *mut mpv_node_list,
    pub ba: *mut mpv_byte_array,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct mpv_node {
    pub u: mpv_node_u,
    pub format: mpv_format,
}

#[repr(C)]
pub struct mpv_node_list {
    pub num: c_int,
    pub values: *mut mpv_node,
    pub keys: *mut *mut c_char,
}

#[repr(C)]
pub struct mpv_byte_array {
    pub data: *mut c_void,
    pub size: usize,
}

#[repr(C)]
pub struct mpv_event {
    pub event_id: mpv_event_id,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct mpv_event_property {
    pub name: *const c_char,
    pub format: mpv_format,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct mpv_event_log_message {
    pub prefix: *const c_char,
    pub level: *const c_char,
    pub text: *const c_char,
    pub log_level: c_int,
}

#[repr(C)]
pub struct mpv_event_start_file {
    pub playlist_entry_id: i64,
}

#[repr(C)]
pub struct mpv_event_end_file {
    pub reason: c_int,
    pub error: c_int,
    pub playlist_entry_id: i64,
    pub playlist_insert_id: i64,
    pub playlist_insert_num_entries: c_int,
}

#[repr(C)]
pub struct mpv_event_client_message {
    pub num_args: c_int,
    pub args: *const *const c_char,
}

#[repr(C)]
pub struct mpv_event_hook {
    pub name: *const c_char,
    pub id: u64,
}

#[repr(C)]
pub struct mpv_event_command {
    pub result: mpv_node,
}

pub type mpv_render_param_type = c_int;
pub const MPV_RENDER_PARAM_INVALID: mpv_render_param_type = 0;
pub const MPV_RENDER_PARAM_API_TYPE: mpv_render_param_type = 1;
pub const MPV_RENDER_PARAM_OPENGL_INIT_PARAMS: mpv_render_param_type = 2;
pub const MPV_RENDER_PARAM_OPENGL_FBO: mpv_render_param_type = 3;
pub const MPV_RENDER_PARAM_FLIP_Y: mpv_render_param_type = 4;
pub const MPV_RENDER_PARAM_X11_DISPLAY: mpv_render_param_type = 8;
pub const MPV_RENDER_PARAM_WL_DISPLAY: mpv_render_param_type = 9;
pub const MPV_RENDER_PARAM_ADVANCED_CONTROL: mpv_render_param_type = 10;
pub const MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME: mpv_render_param_type = 12;
pub const MPV_RENDER_PARAM_SW_SIZE: mpv_render_param_type = 17;
pub const MPV_RENDER_PARAM_SW_FORMAT: mpv_render_param_type = 18;
pub const MPV_RENDER_PARAM_SW_STRIDE: mpv_render_param_type = 19;
pub const MPV_RENDER_PARAM_SW_POINTER: mpv_render_param_type = 20;

pub const MPV_RENDER_API_TYPE_OPENGL: &[u8] = b"opengl\0";
pub const MPV_RENDER_API_TYPE_SW: &[u8] = b"sw\0";
pub const MPV_RENDER_UPDATE_FRAME: u64 = 1;

#[repr(C)]
pub struct mpv_render_param {
    pub type_: mpv_render_param_type,
    pub data: *mut c_void,
}

pub type GetProcAddress =
    unsafe extern "C" fn(ctx: *mut c_void, name: *const c_char) -> *mut c_void;

#[repr(C)]
pub struct mpv_opengl_init_params {
    pub get_proc_address: Option<GetProcAddress>,
    pub get_proc_address_ctx: *mut c_void,
}

#[repr(C)]
pub struct mpv_opengl_fbo {
    pub fbo: c_int,
    pub w: c_int,
    pub h: c_int,
    pub internal_format: c_int,
}

pub type RenderUpdateFn = unsafe extern "C" fn(cb_ctx: *mut c_void);

macro_rules! mpv_api {
    ($($name:ident: fn($($arg:ty),*) $(-> $ret:ty)?;)*) => {
        /// Function table for the loaded libmpv.
        // Some entries are only used by the tests' synchronous helpers.
        #[cfg_attr(not(test), allow(dead_code))]
        pub struct Api {
            $(pub $name: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)*
            _library: Library,
        }

        impl Api {
            /// # Safety
            /// `library` must be a libmpv build whose exported symbols match the
            /// client API 2.x signatures declared in this module.
            unsafe fn from_library(library: Library) -> Result<Self, String> {
                $(
                    // SAFETY: the caller guarantees the symbol has this signature.
                    let $name = unsafe {
                        *library
                            .get::<unsafe extern "C" fn($($arg),*) $(-> $ret)?>(
                                concat!(stringify!($name), "\0").as_bytes(),
                            )
                            .map_err(|error| format!("{}: {error}", stringify!($name)))?
                    };
                )*
                Ok(Self { $($name,)* _library: library })
            }
        }
    };
}

mpv_api! {
    mpv_client_api_version: fn() -> std::ffi::c_ulong;
    mpv_error_string: fn(c_int) -> *const c_char;
    mpv_create: fn() -> *mut mpv_handle;
    mpv_initialize: fn(*mut mpv_handle) -> c_int;
    mpv_terminate_destroy: fn(*mut mpv_handle);
    mpv_free_node_contents: fn(*mut mpv_node);
    mpv_set_option: fn(*mut mpv_handle, *const c_char, mpv_format, *mut c_void) -> c_int;
    mpv_command: fn(*mut mpv_handle, *mut *const c_char) -> c_int;
    mpv_command_ret: fn(*mut mpv_handle, *mut *const c_char, *mut mpv_node) -> c_int;
    mpv_command_async: fn(*mut mpv_handle, u64, *mut *const c_char) -> c_int;
    mpv_set_property: fn(*mut mpv_handle, *const c_char, mpv_format, *mut c_void) -> c_int;
    mpv_set_property_async: fn(*mut mpv_handle, u64, *const c_char, mpv_format, *mut c_void) -> c_int;
    mpv_get_property: fn(*mut mpv_handle, *const c_char, mpv_format, *mut c_void) -> c_int;
    mpv_get_property_async: fn(*mut mpv_handle, u64, *const c_char, mpv_format) -> c_int;
    mpv_observe_property: fn(*mut mpv_handle, u64, *const c_char, mpv_format) -> c_int;
    mpv_request_log_messages: fn(*mut mpv_handle, *const c_char) -> c_int;
    mpv_hook_add: fn(*mut mpv_handle, u64, *const c_char, c_int) -> c_int;
    mpv_hook_continue: fn(*mut mpv_handle, u64) -> c_int;
    mpv_wait_event: fn(*mut mpv_handle, c_double) -> *mut mpv_event;
    mpv_render_context_create: fn(*mut *mut mpv_render_context, *mut mpv_handle, *mut mpv_render_param) -> c_int;
    mpv_render_context_set_update_callback: fn(*mut mpv_render_context, Option<RenderUpdateFn>, *mut c_void);
    mpv_render_context_update: fn(*mut mpv_render_context) -> u64;
    mpv_render_context_render: fn(*mut mpv_render_context, *mut mpv_render_param) -> c_int;
    mpv_render_context_report_swap: fn(*mut mpv_render_context);
    mpv_render_context_free: fn(*mut mpv_render_context);
}

static API: OnceLock<Result<Api, String>> = OnceLock::new();

/// The process-wide libmpv function table, loading the library on first use.
pub fn api() -> Result<&'static Api, String> {
    API.get_or_init(load).as_ref().map_err(Clone::clone)
}

fn load() -> Result<Api, String> {
    let mut tried = Vec::new();
    for candidate in library_candidates() {
        // SAFETY: loading libmpv runs only its library constructors, which have
        // no preconditions.
        match unsafe { Library::new(&candidate) } {
            Ok(library) => {
                // SAFETY: the library was found under a libmpv name; symbol
                // lookup fails cleanly if it is something else.
                let api = unsafe { Api::from_library(library) }.map_err(|error| {
                    format!("{} is not a usable libmpv: {error}", candidate.display())
                })?;
                // SAFETY: takes no arguments and only returns a constant.
                let version = unsafe { (api.mpv_client_api_version)() };
                let major = version >> 16;
                if major != 2 {
                    return Err(format!(
                        "{} provides client API {}.{}, but Patina needs 2.x",
                        candidate.display(),
                        major,
                        version & 0xffff
                    ));
                }
                log::info!(
                    "loaded {} (client API {}.{})",
                    candidate.display(),
                    major,
                    version & 0xffff
                );
                return Ok(api);
            }
            Err(error) => tried.push(format!("{}: {error}", candidate.display())),
        }
    }
    Err(format!(
        "could not load libmpv. Install mpv (libmpv 2), or set PATINA_LIBMPV to its path.\n  {}",
        tried.join("\n  ")
    ))
}

fn library_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("PATINA_LIBMPV") {
        candidates.push(PathBuf::from(path));
    }

    let names: &[&str] = if cfg!(target_os = "windows") {
        &["libmpv-2.dll", "mpv-2.dll"]
    } else if cfg!(target_os = "macos") {
        &["libmpv.2.dylib", "libmpv.dylib"]
    } else {
        &["libmpv.so.2", "libmpv.so"]
    };

    // A packaged build keeps its libmpv beside the executable (Windows), in the
    // bundle's Frameworks directory (macOS), or in a lib directory (Linux).
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
    {
        let bundled = if cfg!(target_os = "macos") {
            dir.join("../Frameworks")
        } else if cfg!(target_os = "windows") {
            dir.clone()
        } else {
            dir.join("lib")
        };
        candidates.extend(names.iter().map(|name| bundled.join(name)));
    }

    if cfg!(target_os = "macos") {
        for prefix in ["/opt/homebrew/lib", "/usr/local/lib", "/opt/local/lib"] {
            candidates.extend(names.iter().map(|name| PathBuf::from(prefix).join(name)));
        }
    }

    // Finally the platform's normal search path.
    candidates.extend(names.iter().map(PathBuf::from));
    candidates
}
