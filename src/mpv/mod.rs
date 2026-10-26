//! A small safe wrapper over the parts of libmpv's client API Patina uses.

pub mod ffi;
mod node;
pub mod render;

pub use node::Node;

use std::ffi::{CString, c_char, c_int, c_void};
use std::fmt;
use std::ptr::NonNull;
use std::sync::Mutex;

/// An mpv API error code with a description of what was being done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub code: i32,
    pub context: String,
}

impl Error {
    fn new(code: c_int, context: impl Into<String>) -> Self {
        Self {
            code,
            context: context.into(),
        }
    }

    pub fn description(&self) -> String {
        error_string(self.code)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.context.is_empty() {
            f.write_str(&self.description())
        } else {
            write!(f, "{}: {}", self.context, self.description())
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// The text mpv gives an error code.
pub fn error_string(code: i32) -> String {
    match ffi::api() {
        // SAFETY: mpv_error_string accepts any value and returns a static string.
        Ok(api) => unsafe { node::string_from_ptr((api.mpv_error_string)(code)) },
        Err(_) => format!("mpv error {code}"),
    }
}

fn check(code: c_int, context: impl FnOnce() -> String) -> Result<()> {
    if code >= 0 {
        Ok(())
    } else {
        Err(Error::new(code, context()))
    }
}

fn cstring(value: &str) -> Result<CString> {
    CString::new(value).map_err(|_| Error::new(-4, format!("{value:?} contains a NUL byte")))
}

/// A value for an option or property.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    String(String),
    Flag(bool),
    Int(i64),
    Double(f64),
    /// A list of strings, as string-list options take.
    List(Vec<String>),
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Value::String(value.to_owned())
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Value::String(value)
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Value::Flag(value)
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Value::Int(value)
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Value::Double(value)
    }
}

/// Calls `f` with the value in the representation mpv's setters expect.
fn with_raw_value<R>(
    value: &Value,
    f: impl FnOnce(ffi::mpv_format, *mut c_void) -> R,
) -> Result<R> {
    Ok(match value {
        Value::String(text) => {
            let text = cstring(text)?;
            let mut ptr = text.as_ptr();
            f(ffi::MPV_FORMAT_STRING, (&raw mut ptr).cast())
        }
        Value::Flag(flag) => {
            let mut flag = c_int::from(*flag);
            f(ffi::MPV_FORMAT_FLAG, (&raw mut flag).cast())
        }
        Value::Int(int) => {
            let mut int = *int;
            f(ffi::MPV_FORMAT_INT64, (&raw mut int).cast())
        }
        Value::Double(double) => {
            let mut double = *double;
            f(ffi::MPV_FORMAT_DOUBLE, (&raw mut double).cast())
        }
        Value::List(items) => {
            let items = items
                .iter()
                .map(|item| cstring(item))
                .collect::<Result<Vec<_>>>()?;
            let mut values: Vec<ffi::mpv_node> = items
                .iter()
                .map(|item| ffi::mpv_node {
                    u: ffi::mpv_node_u {
                        string: item.as_ptr().cast_mut(),
                    },
                    format: ffi::MPV_FORMAT_STRING,
                })
                .collect();
            let mut list = ffi::mpv_node_list {
                num: c_int::try_from(values.len()).unwrap_or(c_int::MAX),
                values: values.as_mut_ptr(),
                keys: std::ptr::null_mut(),
            };
            let mut node = ffi::mpv_node {
                u: ffi::mpv_node_u {
                    list: &raw mut list,
                },
                format: ffi::MPV_FORMAT_NODE_ARRAY,
            };
            f(ffi::MPV_FORMAT_NODE, (&raw mut node).cast())
        }
    })
}

/// A NULL-terminated argument vector for mpv commands.
struct Args {
    _owned: Vec<CString>,
    pointers: Vec<*const c_char>,
}

impl Args {
    fn new(args: &[&str]) -> Result<Self> {
        let owned = args
            .iter()
            .map(|arg| cstring(arg))
            .collect::<Result<Vec<_>>>()?;
        let mut pointers: Vec<*const c_char> = owned.iter().map(|arg| arg.as_ptr()).collect();
        pointers.push(std::ptr::null());
        Ok(Self {
            _owned: owned,
            pointers,
        })
    }

    fn as_mut_ptr(&mut self) -> *mut *const c_char {
        self.pointers.as_mut_ptr()
    }
}

/// An mpv core that has not been initialized yet; options are set here.
pub struct Builder {
    mpv: Mpv,
}

impl Builder {
    pub fn set_option(&self, name: &str, value: impl Into<Value>) -> Result<()> {
        let value = value.into();
        let cname = cstring(name)?;
        let code = with_raw_value(&value, |format, data| {
            // SAFETY: the handle is valid; `data` points to a value of `format`
            // that outlives the call, and mpv copies it.
            unsafe { (self.mpv.api.mpv_set_option)(self.mpv.ptr(), cname.as_ptr(), format, data) }
        })?;
        check(code, || format!("option {name}={value:?}"))
    }

    pub fn initialize(self) -> Result<Mpv> {
        // SAFETY: valid, not yet initialized handle.
        let code = unsafe { (self.mpv.api.mpv_initialize)(self.mpv.ptr()) };
        check(code, || "initializing mpv".to_owned())?;
        Ok(self.mpv)
    }
}

/// An initialized mpv core.
///
/// libmpv's client API is thread-safe, so `Mpv` can be shared between threads.
/// Only `wait_event` has an extra rule (one caller at a time), which an
/// internal lock enforces.
pub struct Mpv {
    handle: NonNull<ffi::mpv_handle>,
    api: &'static ffi::Api,
    event_lock: Mutex<()>,
}

// SAFETY: every mpv_handle function used here is documented as thread-safe;
// the one exception, mpv_wait_event, is serialized by `event_lock`.
unsafe impl Send for Mpv {}
// SAFETY: see above.
unsafe impl Sync for Mpv {}

impl Mpv {
    /// Creates a new core. Set options on the builder, then initialize it.
    pub fn builder() -> std::result::Result<Builder, String> {
        let api = ffi::api()?;
        // mpv parses numbers using the C locale; a different LC_NUMERIC breaks
        // option parsing. Rust programs never call setlocale themselves, so the
        // process stays in the "C" locale mpv requires.
        // SAFETY: mpv_create has no preconditions.
        let handle = unsafe { (api.mpv_create)() };
        let handle = NonNull::new(handle).ok_or("mpv_create failed (out of memory?)")?;
        Ok(Builder {
            mpv: Mpv {
                handle,
                api,
                event_lock: Mutex::new(()),
            },
        })
    }

    fn ptr(&self) -> *mut ffi::mpv_handle {
        self.handle.as_ptr()
    }

    pub(crate) fn api(&self) -> &'static ffi::Api {
        self.api
    }

    pub(crate) fn raw(&self) -> *mut ffi::mpv_handle {
        self.ptr()
    }

    /// Runs a command and waits for it to finish.
    pub fn command(&self, args: &[&str]) -> Result<()> {
        let mut argv = Args::new(args)?;
        // SAFETY: valid handle and NULL-terminated argument vector.
        let code = unsafe { (self.api.mpv_command)(self.ptr(), argv.as_mut_ptr()) };
        check(code, || args.join(" "))
    }

    /// Queues a command without waiting. The outcome arrives later as an
    /// [`Event::CommandReply`] carrying `userdata`.
    pub fn command_async(&self, userdata: u64, args: &[&str]) -> Result<()> {
        let mut argv = Args::new(args)?;
        // SAFETY: valid handle and argument vector; mpv copies the arguments.
        let code = unsafe { (self.api.mpv_command_async)(self.ptr(), userdata, argv.as_mut_ptr()) };
        check(code, || args.join(" "))
    }

    /// Sets a property without waiting; the reply arrives as
    /// [`Event::SetPropertyReply`].
    pub fn set_property_async(
        &self,
        userdata: u64,
        name: &str,
        value: impl Into<Value>,
    ) -> Result<()> {
        let value = value.into();
        let cname = cstring(name)?;
        let code = with_raw_value(&value, |format, data| {
            // SAFETY: valid handle; mpv copies the value before returning.
            unsafe {
                (self.api.mpv_set_property_async)(
                    self.ptr(),
                    userdata,
                    cname.as_ptr(),
                    format,
                    data,
                )
            }
        })?;
        check(code, || format!("setting {name} to {value:?}"))
    }

    /// Sets a property, waiting for mpv. Never call this on the UI thread.
    pub fn set_property(&self, name: &str, value: impl Into<Value>) -> Result<()> {
        let value = value.into();
        let cname = cstring(name)?;
        let code = with_raw_value(&value, |format, data| {
            // SAFETY: valid handle; `data` matches `format` and outlives the call.
            unsafe { (self.api.mpv_set_property)(self.ptr(), cname.as_ptr(), format, data) }
        })?;
        check(code, || format!("setting {name} to {value:?}"))
    }

    /// Reads a property, waiting for mpv. Never call this on the UI thread.
    pub fn get_property(&self, name: &str) -> Result<Node> {
        let cname = cstring(name)?;
        let mut raw = std::mem::MaybeUninit::<ffi::mpv_node>::zeroed();
        // SAFETY: valid handle; MPV_FORMAT_NODE writes an mpv_node.
        let code = unsafe {
            (self.api.mpv_get_property)(
                self.ptr(),
                cname.as_ptr(),
                ffi::MPV_FORMAT_NODE,
                raw.as_mut_ptr().cast(),
            )
        };
        check(code, || format!("reading {name}"))?;
        // SAFETY: mpv initialized the node on success; copy and free it.
        unsafe {
            let mut raw = raw.assume_init();
            let node = Node::from_raw(&raw);
            (self.api.mpv_free_node_contents)(&mut raw);
            Ok(node)
        }
    }

    /// Reads a property without waiting; the value arrives as
    /// [`Event::GetPropertyReply`] carrying `userdata`.
    pub fn get_property_async(&self, userdata: u64, name: &str) -> Result<()> {
        let cname = cstring(name)?;
        // SAFETY: valid handle and property name.
        let code = unsafe {
            (self.api.mpv_get_property_async)(
                self.ptr(),
                userdata,
                cname.as_ptr(),
                ffi::MPV_FORMAT_NODE,
            )
        };
        check(code, || format!("reading {name}"))
    }

    /// Asks mpv to pause at hook `name` (such as `on_preloaded`) and send an
    /// [`Event::Hook`]; mpv waits there until [`Mpv::hook_continue`].
    pub fn hook_add(&self, userdata: u64, name: &str, priority: i32) -> Result<()> {
        let cname = cstring(name)?;
        // SAFETY: valid handle and hook name.
        let code =
            unsafe { (self.api.mpv_hook_add)(self.ptr(), userdata, cname.as_ptr(), priority) };
        check(code, || format!("adding the {name} hook"))
    }

    /// Lets mpv carry on after the hook event with this `id`.
    pub fn hook_continue(&self, id: u64) -> Result<()> {
        // SAFETY: valid handle; mpv ignores ids it doesn't know.
        let code = unsafe { (self.api.mpv_hook_continue)(self.ptr(), id) };
        check(code, || "continuing a hook".into())
    }

    /// Reports changes of `name` as [`Event::PropertyChange`] with `userdata`.
    pub fn observe_property(&self, userdata: u64, name: &str) -> Result<()> {
        let cname = cstring(name)?;
        // SAFETY: valid handle and property name.
        let code = unsafe {
            (self.api.mpv_observe_property)(
                self.ptr(),
                userdata,
                cname.as_ptr(),
                ffi::MPV_FORMAT_NODE,
            )
        };
        check(code, || format!("observing {name}"))
    }

    pub fn request_log_messages(&self, min_level: &str) -> Result<()> {
        let level = cstring(min_level)?;
        // SAFETY: valid handle and level name.
        let code = unsafe { (self.api.mpv_request_log_messages)(self.ptr(), level.as_ptr()) };
        check(code, || format!("log level {min_level}"))
    }

    /// Waits up to `timeout` seconds (negative: forever) for the next event.
    pub fn wait_event(&self, timeout: f64) -> Option<Event> {
        let _guard = self
            .event_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // SAFETY: valid handle; the lock ensures a single caller. The returned
        // event stays valid until the next call, and is copied before unlocking.
        unsafe {
            let event = (self.api.mpv_wait_event)(self.ptr(), timeout);
            if event.is_null() {
                return None;
            }
            Event::from_raw(&*event)
        }
    }
}

/// Synchronous calls the tests use to inspect a core directly. The player
/// itself never blocks on mpv.
#[cfg(test)]
impl Mpv {
    /// Runs a command, waits for it, and returns its result.
    pub fn command_ret(&self, args: &[&str]) -> Result<Node> {
        let mut argv = Args::new(args)?;
        let mut result = std::mem::MaybeUninit::<ffi::mpv_node>::zeroed();
        // SAFETY: valid handle, argument vector and output node.
        let code = unsafe {
            (self.api.mpv_command_ret)(self.ptr(), argv.as_mut_ptr(), result.as_mut_ptr())
        };
        check(code, || args.join(" "))?;
        // SAFETY: on success mpv initialized `result`; we copy, then free it.
        unsafe {
            let mut raw = result.assume_init();
            let node = Node::from_raw(&raw);
            (self.api.mpv_free_node_contents)(&mut raw);
            Ok(node)
        }
    }

    pub fn get_f64(&self, name: &str) -> Result<f64> {
        let node = self.get_property(name)?;
        node.as_f64()
            .ok_or_else(|| Error::new(-9, format!("{name} is {node:?}, not a number")))
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        // SAFETY: the handle is valid and this is its last use. Render contexts
        // hold an Arc<Mpv>, so none can outlive the core.
        unsafe { (self.api.mpv_terminate_destroy)(self.ptr()) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndFileReason {
    Eof,
    Stop,
    Quit,
    Error,
    Redirect,
    Unknown(i32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogMessage {
    pub prefix: String,
    pub level: String,
    pub text: String,
}

/// An owned copy of an mpv event.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Shutdown,
    Log(LogMessage),
    GetPropertyReply {
        userdata: u64,
        result: Result<(String, Node)>,
    },
    SetPropertyReply {
        userdata: u64,
        result: Result<()>,
    },
    CommandReply {
        userdata: u64,
        result: Result<Node>,
    },
    StartFile {
        playlist_entry_id: i64,
    },
    EndFile {
        reason: EndFileReason,
        error: Option<Error>,
        playlist_entry_id: i64,
    },
    FileLoaded,
    ClientMessage(Vec<String>),
    VideoReconfig,
    AudioReconfig,
    Seek,
    PlaybackRestart,
    PropertyChange {
        userdata: u64,
        name: String,
        value: Node,
    },
    QueueOverflow,
    Hook {
        userdata: u64,
        name: String,
        id: u64,
    },
    Other(i32),
}

impl Event {
    /// # Safety
    /// `event` must be a valid event returned by `mpv_wait_event`.
    unsafe fn from_raw(event: &ffi::mpv_event) -> Option<Event> {
        let status = |context: &str| check(event.error, || context.to_owned());
        // SAFETY (whole block): the `data` pointer's type is determined by
        // `event_id` as documented in client.h, and is valid for this event.
        unsafe {
            Some(match event.event_id {
                ffi::MPV_EVENT_NONE => return None,
                ffi::MPV_EVENT_SHUTDOWN => Event::Shutdown,
                ffi::MPV_EVENT_LOG_MESSAGE => {
                    let message = &*event.data.cast::<ffi::mpv_event_log_message>();
                    Event::Log(LogMessage {
                        prefix: node::string_from_ptr(message.prefix),
                        level: node::string_from_ptr(message.level),
                        text: node::string_from_ptr(message.text),
                    })
                }
                ffi::MPV_EVENT_GET_PROPERTY_REPLY => {
                    let property = &*event.data.cast::<ffi::mpv_event_property>();
                    let name = node::string_from_ptr(property.name);
                    Event::GetPropertyReply {
                        userdata: event.reply_userdata,
                        result: status(&name).map(|()| (name.clone(), property_value(property))),
                    }
                }
                ffi::MPV_EVENT_SET_PROPERTY_REPLY => Event::SetPropertyReply {
                    userdata: event.reply_userdata,
                    result: status("setting property"),
                },
                ffi::MPV_EVENT_COMMAND_REPLY => {
                    let command = &*event.data.cast::<ffi::mpv_event_command>();
                    Event::CommandReply {
                        userdata: event.reply_userdata,
                        result: status("command").map(|()| Node::from_raw(&command.result)),
                    }
                }
                ffi::MPV_EVENT_START_FILE => {
                    let start = &*event.data.cast::<ffi::mpv_event_start_file>();
                    Event::StartFile {
                        playlist_entry_id: start.playlist_entry_id,
                    }
                }
                ffi::MPV_EVENT_END_FILE => {
                    let end = &*event.data.cast::<ffi::mpv_event_end_file>();
                    let reason = match end.reason {
                        ffi::MPV_END_FILE_REASON_EOF => EndFileReason::Eof,
                        ffi::MPV_END_FILE_REASON_STOP => EndFileReason::Stop,
                        ffi::MPV_END_FILE_REASON_QUIT => EndFileReason::Quit,
                        ffi::MPV_END_FILE_REASON_ERROR => EndFileReason::Error,
                        ffi::MPV_END_FILE_REASON_REDIRECT => EndFileReason::Redirect,
                        other => EndFileReason::Unknown(other),
                    };
                    Event::EndFile {
                        reason,
                        error: (reason == EndFileReason::Error)
                            .then(|| Error::new(end.error, "playback")),
                        playlist_entry_id: end.playlist_entry_id,
                    }
                }
                ffi::MPV_EVENT_FILE_LOADED => Event::FileLoaded,
                ffi::MPV_EVENT_CLIENT_MESSAGE => {
                    let message = &*event.data.cast::<ffi::mpv_event_client_message>();
                    let args = if message.args.is_null() {
                        Vec::new()
                    } else {
                        std::slice::from_raw_parts(message.args, message.num_args.max(0) as usize)
                            .iter()
                            .map(|arg| node::string_from_ptr(*arg))
                            .collect()
                    };
                    Event::ClientMessage(args)
                }
                ffi::MPV_EVENT_VIDEO_RECONFIG => Event::VideoReconfig,
                ffi::MPV_EVENT_AUDIO_RECONFIG => Event::AudioReconfig,
                ffi::MPV_EVENT_SEEK => Event::Seek,
                ffi::MPV_EVENT_PLAYBACK_RESTART => Event::PlaybackRestart,
                ffi::MPV_EVENT_PROPERTY_CHANGE => {
                    let property = &*event.data.cast::<ffi::mpv_event_property>();
                    Event::PropertyChange {
                        userdata: event.reply_userdata,
                        name: node::string_from_ptr(property.name),
                        value: property_value(property),
                    }
                }
                ffi::MPV_EVENT_QUEUE_OVERFLOW => Event::QueueOverflow,
                ffi::MPV_EVENT_HOOK => {
                    let hook = &*event.data.cast::<ffi::mpv_event_hook>();
                    Event::Hook {
                        userdata: event.reply_userdata,
                        name: node::string_from_ptr(hook.name),
                        id: hook.id,
                    }
                }
                other => Event::Other(other),
            })
        }
    }
}

/// # Safety
/// `property` must come from a live property event.
unsafe fn property_value(property: &ffi::mpv_event_property) -> Node {
    // SAFETY: with MPV_FORMAT_NODE, `data` points to an mpv_node; other formats
    // are only produced by observers registered with them, which Patina does
    // not use.
    unsafe {
        match property.format {
            ffi::MPV_FORMAT_NODE if !property.data.is_null() => {
                Node::from_raw(&*property.data.cast::<ffi::mpv_node>())
            }
            _ => Node::None,
        }
    }
}

/// Helpers for tests that need a real (headless) mpv core.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::time::{Duration, Instant};

    /// A headless core, or None when libmpv is not installed.
    pub fn headless() -> Option<Mpv> {
        let builder = match Mpv::builder() {
            Ok(builder) => builder,
            Err(error) => {
                eprintln!("skipping: {error}");
                return None;
            }
        };
        for (name, value) in [
            ("vo", "null"),
            ("ao", "null"),
            ("idle", "yes"),
            ("config", "no"),
            ("terminal", "no"),
        ] {
            builder.set_option(name, value).unwrap();
        }
        Some(builder.initialize().unwrap())
    }

    pub fn wait_for(mpv: &Mpv, mut matches: impl FnMut(&Event) -> bool) -> Event {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some(event) = mpv.wait_event(0.5)
                && matches(&event)
            {
                return event;
            }
        }
        panic!("timed out waiting for an mpv event");
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{headless, wait_for};
    use super::*;

    #[test]
    fn properties_round_trip_and_errors_are_described() {
        let Some(mpv) = headless() else { return };
        mpv.set_property("volume", 42.5).unwrap();
        assert_eq!(mpv.get_f64("volume").unwrap(), 42.5);
        mpv.set_property("pause", true).unwrap();
        assert_eq!(mpv.get_property("pause").unwrap(), Node::Flag(true));

        let error = mpv.get_property("no-such-property").unwrap_err();
        assert_eq!(error.code, -8);
        assert!(error.to_string().contains("no-such-property"));
        assert!(mpv.command(&["definitely-not-a-command"]).is_err());
    }

    #[test]
    fn observed_properties_and_async_replies_arrive_as_events() {
        let Some(mpv) = headless() else { return };
        mpv.observe_property(7, "speed").unwrap();
        // The initial value is reported right after observing.
        let initial = wait_for(&mpv, |event| {
            matches!(event, Event::PropertyChange { userdata: 7, .. })
        });
        assert_eq!(
            initial,
            Event::PropertyChange {
                userdata: 7,
                name: "speed".into(),
                value: Node::Double(1.0)
            }
        );

        mpv.set_property_async(11, "speed", 1.5).unwrap();
        let reply = wait_for(&mpv, |event| {
            matches!(event, Event::SetPropertyReply { .. })
        });
        assert_eq!(
            reply,
            Event::SetPropertyReply {
                userdata: 11,
                result: Ok(())
            }
        );
        let changed = wait_for(&mpv, |event| {
            matches!(event, Event::PropertyChange { userdata: 7, .. })
        });
        assert!(
            matches!(changed, Event::PropertyChange { value: Node::Double(speed), .. } if speed == 1.5)
        );

        mpv.command_async(12, &["expand-text", "${speed}"]).unwrap();
        let reply = wait_for(&mpv, |event| matches!(event, Event::CommandReply { .. }));
        let Event::CommandReply {
            userdata: 12,
            result: Ok(Node::String(text)),
        } = reply
        else {
            panic!("unexpected reply {reply:?}");
        };
        assert_eq!(text.parse::<f64>(), Ok(1.5));
    }

    #[test]
    fn structured_properties_convert_to_nodes() {
        let Some(mpv) = headless() else { return };
        let node = mpv.command_ret(&["expand-path", "~~/"]).unwrap();
        assert!(node.as_str().is_some());
        let options = mpv.get_property("options/vo").unwrap();
        assert_eq!(
            options.as_array().first().and_then(|vo| vo.get("name")),
            Some(&Node::String("null".into()))
        );
    }
}
