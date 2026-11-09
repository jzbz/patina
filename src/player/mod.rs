//! One mpv core per player window, with its event thread.
//!
//! The UI thread never waits on mpv: it issues asynchronous commands and
//! learns about every change through observed properties, which the event
//! thread forwards in batches.

mod props;
pub mod state;

pub use props::Prop;

use crate::mpv::{Event, Mpv, Node, Value};
use std::borrow::Cow;
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
    /// Whether mpv keeps the audio devices listed from the start. Listing
    /// them goes through every audio output mpv has, PipeWire's included
    /// whatever `ao` says, so without a PipeWire server
    /// (platform::pipewire_missing) the window asks for the list only when
    /// it is about to show it.
    pub list_audio_devices: bool,
    /// Patina's entries for mpv's `script-opts`
    /// (`Preferences::hook_options`), added once mpv has read everything
    /// else (see `Player::set_hook_options`).
    pub hook_options: Vec<(String, String)>,
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
/// Userdata of the `on_preloaded` hook: mpv has opened a file and waits
/// before it loads it (`give_ytdl_title`), handled on the event thread.
const HOOK_PRELOADED: u64 = 2;
/// After mpv's yt-dlp hook's own (a script's 10 is -40), so that a title it
/// gives there counts.
const HOOK_PRELOADED_PRIORITY: i32 = 50;

pub struct Player {
    mpv: Arc<Mpv>,
    hook: Arc<PendingHook>,
    events: Option<JoinHandle<()>>,
    /// The `script-opts` entries Patina last asked for.
    hook_options: std::cell::RefCell<Vec<(String, String)>>,
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
            if *prop == Prop::AudioDeviceList && !options.list_audio_devices {
                continue;
            }
            if let Err(error) = mpv.observe_property(*prop as u64, prop.name()) {
                log::warn!("{error}");
            }
        }
        if let Err(error) = mpv.hook_add(HOOK_LOAD, "on_load", HOOK_LOAD_PRIORITY) {
            log::warn!("{error}");
        }
        if let Err(error) = mpv.hook_add(HOOK_PRELOADED, "on_preloaded", HOOK_PRELOADED_PRIORITY) {
            log::warn!("{error}");
        }

        let (sender, receiver) = async_channel::unbounded();
        let hook = Arc::new(PendingHook::default());
        let (thread_mpv, thread_hook) = (mpv.clone(), hook.clone());
        let events = std::thread::Builder::new()
            .name("mpv-events".into())
            .spawn(move || pump_events(&thread_mpv, &thread_hook, &sender))
            .map_err(|error| format!("could not start the mpv event thread: {error}"))?;

        let player = Player {
            mpv,
            hook,
            events: Some(events),
            hook_options: Default::default(),
        };
        player.set_hook_options(&options.hook_options);
        Ok((player, receiver))
    }

    /// Adds Patina's entries to mpv's `script-opts` (`entries`, from
    /// `Preferences::hook_options`) rather than setting the option, which
    /// would put them in place of every other entry: libmpv's own (0.42's
    /// keeps its select script from reading the audio devices, see
    /// `CoreOptions::list_audio_devices`), and the user's, from mpv.conf,
    /// the preferences' extra options or the command line. Those win: an
    /// entry is added only where nothing set its key, and later only an
    /// entry Patina set is changed, or removed when Patina no longer asks
    /// for it. mpv's scripts follow `script-opts` as it changes, though the
    /// hook, once told a yt-dlp, doesn't search again when told none: a
    /// cleared path preference takes effect in a new window.
    pub fn set_hook_options(&self, entries: &[(String, String)]) {
        let mut ours = self.hook_options.borrow_mut();
        let mut current = match self.mpv.get_property("script-opts") {
            Ok(Node::Map(map)) => map
                .into_iter()
                .filter_map(|(key, value)| Some((key, value.as_str()?.to_owned())))
                .collect(),
            _ => Vec::new(),
        };
        let (changed, owned) = merge_hook_options(&mut current, &ours, entries);
        if changed && let Err(error) = self.mpv.set_property("script-opts", Value::Map(current)) {
            log::warn!("{error}");
            return;
        }
        *ours = owned;
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
            // The window hears what yt-dlp's hook says went wrong, which it has
            // no other way to learn: its error names only a file not opening.
            Event::Log(message) => {
                log_mpv_message(message);
                message.prefix == "ytdl_hook" && message.level == "error"
            }
            Event::Hook {
                userdata: HOOK_PRELOADED,
                id,
                ..
            } => {
                give_ytdl_title(mpv);
                if let Err(error) = mpv.hook_continue(*id) {
                    log::warn!("{error}");
                }
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

/// Gives a web video yt-dlp's title while mpv holds it, opened but not yet
/// loaded, if mpv has only its stand-in: the address's last part, its
/// `filename`. mpv 0.41's yt-dlp hook set the title before it opened the
/// file; 0.42's gives it later, with the stream's tags, and not at all for
/// a single-format answer. Set here, the title is the file's, whatever
/// plays next, and is there when the window records the file in History.
/// Nothing changes where mpv has a title (a YouTube section video's, the
/// user's own `force-media-title`, the tags'), nor where yt-dlp didn't run
/// (a Jellyfin item, a local file), nor for its generic answer for a plain
/// media address (`ytdl::title`).
fn give_ytdl_title(mpv: &Mpv) {
    let Some(title) = mpv
        .get_property("user-data/mpv/ytdl/json-subprocess-result/stdout")
        .ok()
        .and_then(|json| json.as_str().and_then(crate::ytdl::title))
    else {
        return;
    };
    let text = |name: &str| {
        mpv.get_property(name)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default()
    };
    let media_title = text("media-title");
    if (media_title.is_empty() || media_title == text("filename"))
        && let Err(error) = mpv.set_property("file-local-options/force-media-title", title.as_str())
    {
        log::warn!("{error}");
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
    let text = trim_stream_addresses(message.text.trim_end());
    let level = match message.level.as_str() {
        "fatal" | "error" => log::Level::Error,
        "warn" => log::Level::Warn,
        "info" => log::Level::Info,
        "v" => log::Level::Debug,
        _ => log::Level::Trace,
    };
    log::log!(target: "mpv", level, "[{}] {text}", message.prefix);
}

/// A message from mpv with the addresses of YouTube's streams cut down to
/// their host's name. They are signed for this user, and carry the user's
/// public IP address, in `ip=` or `/ip/`. mpv says them when one fails to
/// open, and for every live stream, at the level Patina logs by default, so
/// they would otherwise reach a terminal or a journal, and from there a bug
/// report.
///
/// The cut goes to the next space or quote, whatever comes first, since an
/// `edl://` address holds several others, and their query strings hold commas
/// and semicolons.
fn trim_stream_addresses(text: &str) -> Cow<'_, str> {
    const HOST: &str = "googlevideo.com";
    let ends = |c: char| c.is_ascii_whitespace() || c == '\'' || c == '"';
    if !text.contains(HOST) {
        return Cow::Borrowed(text);
    }
    let mut trimmed = String::new();
    let mut rest = text;
    while let Some(found) = rest.find(HOST) {
        let start = rest[..found].rfind(ends).map_or(0, |end| end + 1);
        let end = rest[found..]
            .find(ends)
            .map_or(rest.len(), |end| found + end);
        trimmed.push_str(&rest[..start]);
        trimmed.push_str("https://….googlevideo.com/…");
        rest = &rest[end..];
    }
    trimmed.push_str(rest);
    Cow::Owned(trimmed)
}

/// `current` (mpv's `script-opts`) with `wanted` merged in, as
/// `Player::set_hook_options` says: an entry is added where its key is
/// missing, replaced only where it still holds what Patina set (`before`,
/// the entries Patina owns), and one Patina no longer wants is removed only
/// on the same terms. Whether anything changed, and the entries Patina owns
/// now: not one of someone else's that happened to hold what Patina wanted.
fn merge_hook_options(
    current: &mut Vec<(String, String)>,
    before: &[(String, String)],
    wanted: &[(String, String)],
) -> (bool, Vec<(String, String)>) {
    let ours = |key: &str, value: &str| {
        before
            .iter()
            .any(|(known, old)| known == key && old == value)
    };
    let mut changed = false;
    let mut owned = Vec::new();
    for (key, value) in wanted {
        match current.iter_mut().find(|(known, _)| known == key) {
            None => {
                current.push((key.clone(), value.clone()));
                changed = true;
            }
            Some(entry) if ours(key, &entry.1) => {
                if entry.1 != *value {
                    entry.1.clone_from(value);
                    changed = true;
                }
            }
            Some(_) => continue,
        }
        owned.push((key.clone(), value.clone()));
    }
    let before_len = current.len();
    current
        .retain(|(key, value)| wanted.iter().any(|(known, _)| known == key) || !ours(key, value));
    (changed || current.len() != before_len, owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_core_starts_with_the_default_options() {
        // Every option Player::start insists on exists in this libmpv; the
        // preferences' options are checked by the settings tests.
        if crate::mpv::testing::headless().is_none() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("patina-core-{}", std::process::id()));
        // No audio output and no device list: either can reach mpv's
        // PipeWire output, whose failing connect can hang on a machine
        // without PipeWire (platform::pipewire_missing).
        let mut mpv_options = vec![("ao".to_owned(), "null".to_owned())];
        mpv_options.extend(
            crate::settings::Preferences::default()
                .mpv_options()
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value)),
        );
        let options = CoreOptions {
            watch_later_dir: dir.join("watch-later"),
            options: mpv_options,
            ..Default::default()
        };
        let (mut player, _events) = Player::start(&options).unwrap();
        player.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn entries(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    /// Patina's entries go in beside everyone else's, and change or leave
    /// only while they hold what Patina set.
    #[test]
    fn hook_options_join_the_users() {
        let ours = entries(&[
            ("ytdl_hook-ytdl_path", "/a"),
            ("ytdl_hook-all_formats", "no"),
        ]);
        let mut current = entries(&[("select-populate_menu_data", "no"), ("osc-layout", "box")]);
        assert!(merge_hook_options(&mut current, &[], &ours).0);
        assert_eq!(
            current,
            entries(&[
                ("select-populate_menu_data", "no"),
                ("osc-layout", "box"),
                ("ytdl_hook-ytdl_path", "/a"),
                ("ytdl_hook-all_formats", "no")
            ])
        );
        // Asked again, nothing changes.
        assert!(!merge_hook_options(&mut current, &ours, &ours).0);
        // A key the user set stays theirs, whatever Patina asks for.
        let mut theirs = entries(&[("ytdl_hook-all_formats", "yes")]);
        assert!(merge_hook_options(&mut theirs, &[], &ours).0);
        assert_eq!(
            theirs,
            entries(&[
                ("ytdl_hook-all_formats", "yes"),
                ("ytdl_hook-ytdl_path", "/a")
            ])
        );
        // Patina's own entry moves with it, and goes when it is no longer
        // asked for; one the user changed since stays.
        let moved = entries(&[
            ("ytdl_hook-ytdl_path", "/b"),
            ("ytdl_hook-all_formats", "no"),
        ]);
        assert!(merge_hook_options(&mut current, &ours, &moved).0);
        assert!(current.contains(&("ytdl_hook-ytdl_path".to_owned(), "/b".to_owned())));
        let formats = entries(&[("ytdl_hook-all_formats", "no")]);
        assert!(merge_hook_options(&mut current, &moved, &formats).0);
        assert!(!current.iter().any(|(key, _)| key == "ytdl_hook-ytdl_path"));
        let mut changed = entries(&[("ytdl_hook-ytdl_path", "/mine")]);
        assert!(!merge_hook_options(&mut changed, &moved, &formats[..0]).0);
        assert_eq!(changed, entries(&[("ytdl_hook-ytdl_path", "/mine")]));
        // One of the user's that holds what Patina wants is still theirs:
        // not moved, nor removed, with Patina's next wish.
        let mut same = entries(&[("ytdl_hook-ytdl_path", "/x")]);
        let wanted = entries(&[("ytdl_hook-ytdl_path", "/x")]);
        let (changed, owned) = merge_hook_options(&mut same, &[], &wanted);
        assert!(!changed);
        assert!(owned.is_empty());
        let elsewhere = entries(&[("ytdl_hook-ytdl_path", "/bundled")]);
        assert!(!merge_hook_options(&mut same, &owned, &elsewhere).0);
        assert!(!merge_hook_options(&mut same, &owned, &[]).0);
        assert_eq!(same, wanted);
    }

    /// In a running core, the entries join libmpv's own and those of the
    /// user's mpv.conf, which mpv reads after the options Patina sets.
    #[test]
    fn hook_options_join_mpv_conf_and_libmpvs_own() {
        let Some(bare) = crate::mpv::testing::headless() else {
            return;
        };
        let script_opts = |mpv: &Mpv| match mpv.get_property("script-opts") {
            Ok(Node::Map(map)) => map
                .into_iter()
                .map(|(key, value)| (key, value.as_str().unwrap_or_default().to_owned()))
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        // libmpv's own entries (0.42 has one), which Patina keeps.
        let builtin = script_opts(&bare);
        drop(bare);
        let ours = entries(&[
            ("ytdl_hook-ytdl_path", "/a,b/yt-dlp"),
            ("ytdl_hook-all_formats", "no"),
        ]);
        let dir = std::env::temp_dir().join(format!("patina-hook-options-{}", std::process::id()));
        let start = |config_dir: Option<PathBuf>| {
            let options = CoreOptions {
                config_dir,
                watch_later_dir: dir.join("watch-later"),
                options: entries(&[("ao", "null")]),
                hook_options: ours.clone(),
                ..Default::default()
            };
            Player::start(&options).unwrap().0
        };
        let mut player = start(None);
        let mut expected = builtin.clone();
        expected.extend(ours.iter().cloned());
        assert_eq!(script_opts(&player.mpv), expected);
        player.shutdown();

        let config = dir.join("mpv");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("mpv.conf"), "script-opts=osc-layout=box\n").unwrap();
        let mut player = start(Some(config));
        assert_eq!(
            script_opts(&player.mpv),
            entries(&[
                ("osc-layout", "box"),
                ("ytdl_hook-ytdl_path", "/a,b/yt-dlp"),
                ("ytdl_hook-all_formats", "no")
            ])
        );
        // The user's own path, set while it runs, is left alone after.
        player
            .mpv
            .command(&[
                "change-list",
                "script-opts",
                "append",
                "ytdl_hook-ytdl_path=/mine",
            ])
            .unwrap();
        player.set_hook_options(&entries(&[
            ("ytdl_hook-ytdl_path", "/c"),
            ("ytdl_hook-all_formats", "no"),
        ]));
        assert!(
            script_opts(&player.mpv)
                .contains(&("ytdl_hook-ytdl_path".to_owned(), "/mine".to_owned()))
        );
        player.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A web video gets yt-dlp's title while mpv holds it, only where mpv
    /// has none of its own, and for that file only.
    #[test]
    fn a_web_video_gets_yt_dlps_title_in_time() {
        let Some(mpv) = crate::mpv::testing::headless() else {
            return;
        };
        mpv.hook_add(HOOK_PRELOADED, "on_preloaded", HOOK_PRELOADED_PRIORITY)
            .unwrap();
        let answer = |json: &str| {
            mpv.set_property(
                "user-data/mpv/ytdl/json-subprocess-result",
                Value::Map(vec![("stdout".into(), json.into())]),
            )
            .unwrap();
        };
        let play = |path: &str| -> String {
            mpv.command(&["loadfile", path]).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                assert!(std::time::Instant::now() < deadline, "{path}: no hook");
                if let Some(Event::Hook {
                    userdata: HOOK_PRELOADED,
                    id,
                    ..
                }) = mpv.wait_event(0.5)
                {
                    give_ytdl_title(&mpv);
                    mpv.hook_continue(id).unwrap();
                    break;
                }
            }
            crate::mpv::testing::wait_for(&mpv, |event| matches!(event, Event::PlaybackRestart));
            let title = mpv.get_property("media-title").unwrap();
            mpv.command(&["stop"]).unwrap();
            crate::mpv::testing::wait_for(&mpv, |event| matches!(event, Event::EndFile { .. }));
            title.as_str().unwrap_or_default().to_owned()
        };
        const VIDEO: &str = "av://lavfi:sine=frequency=440";
        answer(r#"{"_type": "video", "title": "Big Buck Bunny Trailer"}"#);
        assert_eq!(play(VIDEO), "Big Buck Bunny Trailer");
        // For that file only: the next has its own stand-in.
        mpv.set_property(
            "user-data/mpv/ytdl/json-subprocess-result",
            Value::Map(Vec::new()),
        )
        .unwrap();
        assert_ne!(play(VIDEO), "Big Buck Bunny Trailer");
        // Not yt-dlp's generic answer for a plain media address.
        answer(r#"{"title": "made up", "direct": true}"#);
        assert_ne!(play(VIDEO), "made up");
        // Nor over a title mpv has: the user's own, say.
        answer(r#"{"_type": "video", "title": "Big Buck Bunny Trailer"}"#);
        mpv.set_property("force-media-title", "Mine").unwrap();
        assert_eq!(play(VIDEO), "Mine");
    }

    #[test]
    fn stream_addresses_stay_out_of_the_log() {
        const SIGNED: &str = "https://rr3---sn-abc123.googlevideo.com/videoplayback?expire=1\
            &ip=203.0.113.7&mn=sn-abc,sn-def&sparams=expire,ip;x&sig=AOq0";
        const TRIMMED: &str = "https://….googlevideo.com/…";
        assert_eq!(
            trim_stream_addresses(&format!("Failed to open {SIGNED}")),
            format!("Failed to open {TRIMMED}")
        );
        assert_eq!(
            trim_stream_addresses(&format!("EDL: Could not open source file '{SIGNED}'.")),
            format!("EDL: Could not open source file '{TRIMMED}'.")
        );
        // An edl:// address with several in it is one address.
        let edl =
            format!("edl://!delay_open,media_type=video;%214%{SIGNED};!new_stream;%214%{SIGNED}");
        assert_eq!(
            trim_stream_addresses(&format!("Cannot open file '{edl}': x")),
            format!("Cannot open file '{TRIMMED}': x")
        );
        // Two in a message, with the words around them kept.
        assert_eq!(
            trim_stream_addresses(&format!("{SIGNED} then {SIGNED} ended")),
            format!("{TRIMMED} then {TRIMMED} ended")
        );
        // A live stream's manifest has its address in its path.
        let manifest =
            "https://manifest.googlevideo.com/api/manifest/hls_playlist/ip/203.0.113.7/x";
        let said = trim_stream_addresses(manifest);
        assert_eq!(said, TRIMMED);
        assert!(!said.contains("203.0.113.7"));
    }

    #[test]
    fn other_messages_are_logged_as_they_are() {
        for text in [
            "",
            "[ffmpeg] https: HTTP error 403 Forbidden",
            "Failed to open https://example.com/video.mp4?ip=1",
        ] {
            assert!(matches!(trim_stream_addresses(text), Cow::Borrowed(same) if same == text));
        }
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
