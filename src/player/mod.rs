//! One mpv core per player window, with its event thread.
//!
//! The UI thread never waits on mpv: it issues asynchronous commands and
//! learns about every change through observed properties, which the event
//! thread forwards in batches.

mod props;
pub mod state;

pub use props::Prop;

use crate::mpv::{Event, Mpv, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;

/// Startup options for an mpv core.
#[derive(Debug, Clone, Default)]
pub struct CoreOptions {
    /// Directory holding an optional `mpv.conf` and `scripts/`.
    pub config_dir: Option<PathBuf>,
    pub watch_later_dir: PathBuf,
    /// `name=value` options from the preferences, then the user's own raw
    /// mpv options, then the command line's; later ones win.
    pub options: Vec<(String, String)>,
    pub verbose: bool,
}

/// Userdata for asynchronous commands whose failure should be shown.
pub const REPLY_REPORT: u64 = 1;
/// Userdata for asynchronous commands whose failure is expected and ignored.
pub const REPLY_QUIET: u64 = 2;
/// Userdata of the `on_load` hook: mpv is about to open a file and waits
/// until the hook is continued.
pub const HOOK_LOAD: u64 = 1;
/// Its priority: lower runs first, and it has to run before mpv's yt-dlp
/// hook, which reads the `ytdl-format` Patina may set for the file. Lua
/// scripts' priorities are 50 more than they say: the yt-dlp hook's 10 is
/// -40, and this is a script's 0.
const HOOK_LOAD_PRIORITY: i32 = -50;

pub struct Player {
    mpv: Arc<Mpv>,
    hook: Arc<PendingHook>,
    events: Option<JoinHandle<()>>,
}

/// The load hook mpv is waiting in, so that shutting down can let it go:
/// mpv can't quit while it waits for Patina, and the interface thread,
/// which would let it go, may be the one shutting it down.
#[derive(Default)]
pub struct PendingHook {
    closing: AtomicBool,
    id: AtomicU64,
}

impl PendingHook {
    /// Lets mpv go on from hook `id`, unless that has been done.
    pub fn resume(&self, mpv: &Mpv, id: u64) {
        if self
            .id
            .compare_exchange(id, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
            && let Err(error) = mpv.hook_continue(id)
        {
            log::warn!("{error}");
        }
    }

    /// Notes a hook mpv now waits in; false if the core is closing, and it
    /// has been let go at once.
    fn waiting(&self, mpv: &Mpv, id: u64) -> bool {
        // Stored before closing is read, and closing set before the id is
        // taken in shutdown, so one of the two lets it go.
        self.id.store(id, Ordering::SeqCst);
        if self.closing.load(Ordering::SeqCst) {
            self.resume(mpv, id);
            return false;
        }
        true
    }
}

impl Player {
    pub fn start(
        options: &CoreOptions,
    ) -> Result<(Player, async_channel::Receiver<Vec<Event>>), String> {
        let builder = Mpv::builder()?;
        let set = |name: &str, value: Value| {
            builder.set_option(name, value).map_err(|error| {
                // MPV_ERROR_OPTION_NOT_FOUND: an old libmpv, or one built
                // without Lua, which is what provides `osc`.
                if error.code == -5 {
                    format!(
                        "this libmpv has no `{name}` option: Patina needs mpv 0.39 or newer, built with Lua"
                    )
                } else {
                    format!("could not set mpv option: {error}")
                }
            })
        };

        if let Some(dir) = &options.config_dir {
            // Read mpv.conf, input-free script and font directories from
            // Patina's own configuration directory.
            set("config-dir", dir.to_string_lossy().as_ref().into())?;
            set("config", "yes".into())?;
        }
        set("vo", "libmpv".into())?;
        set("osc", false.into())?;
        set("input-default-bindings", false.into())?;
        set("input-vo-keyboard", false.into())?;
        set("osd-level", 0i64.into())?;
        set("osd-bar", false.into())?;
        set("idle", "yes".into())?;
        set("keep-open", "yes".into())?;
        set("force-window", false.into())?;
        // A playlist made from a file's folder holds that folder's files
        // only; folders the user opens are read recursively instead (see
        // `PlayerController::load`), so subfolders never become entries.
        set("directory-mode", "ignore".into())?;
        set("audio-client-name", "Patina".into())?;
        set("title", "Patina".into())?;
        set("video-timing-offset", 0.0f64.into())?;
        set("screenshot-template", "Patina %F %wH.%wM.%wS".into())?;
        set(
            "watch-later-dir",
            options.watch_later_dir.to_string_lossy().as_ref().into(),
        )?;
        set(
            "watch-later-options",
            "start,vid,aid,sid,secondary-sid,audio-delay,sub-delay,speed,volume,mute,video-rotate,video-aspect-override,deinterlace,ab-loop-a,ab-loop-b"
                .into(),
        )?;
        // Not needed at this value (the preferences set the real one below),
        // but mpv 0.39 is the oldest with it, so an older libmpv fails here
        // with the message above rather than starting with folder playlists,
        // playlist durations and subtitle outlines quietly missing.
        set("autocreate-playlist", "no".into())?;
        set("terminal", options.verbose.into())?;
        if options.verbose {
            set("msg-level", "all=v".into())?;
        }
        for (name, value) in &options.options {
            if let Err(error) = builder.set_option(name, value.as_str()) {
                log::warn!("ignoring mpv option: {error}");
            }
        }

        let mpv = Arc::new(builder.initialize().map_err(|error| error.to_string())?);
        mpv.request_log_messages(if options.verbose { "v" } else { "warn" })
            .map_err(|error| error.to_string())?;
        for prop in Prop::ALL {
            if let Err(error) = mpv.observe_property(*prop as u64, prop.name()) {
                log::warn!("{error}");
            }
        }
        if let Err(error) = mpv.hook_add(HOOK_LOAD, "on_load", HOOK_LOAD_PRIORITY) {
            log::warn!("{error}");
        }

        let (sender, receiver) = async_channel::unbounded();
        let hook = Arc::new(PendingHook::default());
        let (thread_mpv, thread_hook) = (mpv.clone(), hook.clone());
        let events = std::thread::Builder::new()
            .name("mpv-events".into())
            .spawn(move || pump_events(&thread_mpv, &thread_hook, &sender))
            .map_err(|error| format!("could not start the mpv event thread: {error}"))?;

        Ok((
            Player {
                mpv,
                hook,
                events: Some(events),
            },
            receiver,
        ))
    }

    pub fn mpv(&self) -> &Arc<Mpv> {
        &self.mpv
    }

    /// For letting mpv go on from the load hook.
    pub fn hook(&self) -> Arc<PendingHook> {
        self.hook.clone()
    }

    /// Queues an mpv command; failures are reported back as events.
    pub fn command(&self, args: &[&str]) {
        if let Err(error) = self.mpv.command_async(REPLY_REPORT, args) {
            log::warn!("{error}");
        }
    }

    /// Queues an mpv command whose failure is not worth reporting.
    pub fn command_quiet(&self, args: &[&str]) {
        if let Err(error) = self.mpv.command_async(REPLY_QUIET, args) {
            log::debug!("{error}");
        }
    }

    pub fn set(&self, name: &str, value: impl Into<Value>) {
        if let Err(error) = self.mpv.set_property_async(REPLY_REPORT, name, value) {
            log::warn!("{error}");
        }
    }

    /// Sets a property; a failure is only logged.
    pub fn set_quiet(&self, name: &str, value: impl Into<Value>) {
        if let Err(error) = self.mpv.set_property_async(REPLY_QUIET, name, value) {
            log::debug!("{error}");
        }
    }

    /// Stops playback, saves the resume position, and ends the event thread.
    pub fn shutdown(&mut self) {
        if let Some(events) = self.events.take() {
            self.hook.closing.store(true, Ordering::SeqCst);
            let waiting = self.hook.id.load(Ordering::SeqCst);
            if waiting != 0 {
                self.hook.resume(&self.mpv, waiting);
            }
            if let Err(error) = self.mpv.command(&["quit"]) {
                log::warn!("{error}");
            }
            if events.join().is_err() {
                log::error!("the mpv event thread panicked");
            }
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn pump_events(mpv: &Mpv, hook: &PendingHook, sender: &async_channel::Sender<Vec<Event>>) {
    loop {
        let Some(first) = mpv.wait_event(-1.0) else {
            continue;
        };
        let mut batch = vec![first];
        while batch.len() < 256 {
            match mpv.wait_event(0.0) {
                Some(event) => batch.push(event),
                None => break,
            }
        }

        let shutdown = batch.iter().any(|event| matches!(event, Event::Shutdown));
        batch.retain(|event| match event {
            Event::Log(message) => {
                log_mpv_message(message);
                false
            }
            Event::Hook { id, .. } => hook.waiting(mpv, *id),
            _ => true,
        });
        coalesce_property_changes(&mut batch);
        if !batch.is_empty() && sender.send_blocking(batch).is_err() {
            break;
        }
        if shutdown {
            break;
        }
    }
}

/// Keeps only the latest change of each property within one batch.
fn coalesce_property_changes(batch: &mut Vec<Event>) {
    let mut seen = std::collections::HashSet::new();
    let mut keep = vec![true; batch.len()];
    for (index, event) in batch.iter().enumerate().rev() {
        if let Event::PropertyChange { userdata, .. } = event
            && !seen.insert(*userdata)
        {
            keep[index] = false;
        }
    }
    let mut index = 0;
    batch.retain(|_| {
        index += 1;
        keep[index - 1]
    });
}

fn log_mpv_message(message: &crate::mpv::LogMessage) {
    let text = message.text.trim_end();
    let level = match message.level.as_str() {
        "fatal" | "error" => log::Level::Error,
        "warn" => log::Level::Warn,
        "info" => log::Level::Info,
        "v" => log::Level::Debug,
        _ => log::Level::Trace,
    };
    log::log!(target: "mpv", level, "[{}] {text}", message.prefix);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mpv::Node;

    #[test]
    fn the_core_starts_with_the_default_options() {
        // Every option Player::start insists on exists in this libmpv; the
        // preferences' options are checked by the settings tests.
        if crate::mpv::testing::headless().is_none() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("patina-core-{}", std::process::id()));
        let options = CoreOptions {
            watch_later_dir: dir.join("watch-later"),
            options: crate::settings::Preferences::default()
                .mpv_options()
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value))
                .collect(),
            ..Default::default()
        };
        let (mut player, _events) = Player::start(&options).unwrap();
        player.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn change(userdata: u64, value: f64) -> Event {
        Event::PropertyChange {
            userdata,
            name: String::new(),
            value: Node::Double(value),
        }
    }

    #[test]
    fn only_the_last_change_of_each_property_survives() {
        let mut batch = vec![
            change(1, 1.0),
            Event::FileLoaded,
            change(2, 5.0),
            change(1, 2.0),
        ];
        coalesce_property_changes(&mut batch);
        assert_eq!(
            batch,
            vec![Event::FileLoaded, change(2, 5.0), change(1, 2.0)]
        );
    }
}
