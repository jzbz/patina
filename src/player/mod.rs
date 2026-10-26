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
use std::thread::JoinHandle;

/// Startup options for an mpv core.
#[derive(Debug, Clone, Default)]
pub struct CoreOptions {
    /// Directory holding an optional `mpv.conf` and `scripts/`.
    pub config_dir: Option<PathBuf>,
    pub watch_later_dir: PathBuf,
    pub resume_playback: bool,
    pub hwdec: String,
    pub screenshot_dir: PathBuf,
    pub screenshot_format: String,
    pub ytdl_path: Option<String>,
    /// Extra `name=value` options from the user's preferences.
    pub extra_options: Vec<(String, String)>,
    pub verbose: bool,
}

/// Userdata for asynchronous commands whose failure should be shown.
pub const REPLY_REPORT: u64 = 1;
/// Userdata for asynchronous commands whose failure is expected and ignored.
pub const REPLY_QUIET: u64 = 2;

pub struct Player {
    mpv: Arc<Mpv>,
    events: Option<JoinHandle<()>>,
}

impl Player {
    pub fn start(
        options: &CoreOptions,
    ) -> Result<(Player, async_channel::Receiver<Vec<Event>>), String> {
        let builder = Mpv::builder()?;
        let set = |name: &str, value: Value| {
            builder
                .set_option(name, value)
                .map_err(|error| format!("could not set mpv option: {error}"))
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
        set("audio-client-name", "Patina".into())?;
        set("title", "Patina".into())?;
        set("video-timing-offset", 0.0f64.into())?;
        set("hwdec", options.hwdec.as_str().into())?;
        set(
            "screenshot-directory",
            options.screenshot_dir.to_string_lossy().as_ref().into(),
        )?;
        set(
            "screenshot-format",
            options.screenshot_format.as_str().into(),
        )?;
        set("screenshot-template", "Patina %F %wH.%wM.%wS".into())?;
        set(
            "watch-later-dir",
            options.watch_later_dir.to_string_lossy().as_ref().into(),
        )?;
        set("save-position-on-quit", options.resume_playback.into())?;
        set("resume-playback", options.resume_playback.into())?;
        set(
            "watch-later-options",
            "start,vid,aid,sid,secondary-sid,audio-delay,sub-delay,speed,volume,mute,video-rotate,video-aspect-override,deinterlace,ab-loop-a,ab-loop-b"
                .into(),
        )?;
        if let Some(path) = &options.ytdl_path {
            set("script-opts", format!("ytdl_hook-ytdl_path={path}").into())?;
        }
        set("terminal", options.verbose.into())?;
        if options.verbose {
            set("msg-level", "all=v".into())?;
        }
        for (name, value) in &options.extra_options {
            if let Err(error) = builder.set_option(name, value.as_str()) {
                log::warn!("ignoring mpv option from preferences: {error}");
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

        let (sender, receiver) = async_channel::unbounded();
        let thread_mpv = mpv.clone();
        let events = std::thread::Builder::new()
            .name("mpv-events".into())
            .spawn(move || pump_events(&thread_mpv, &sender))
            .map_err(|error| format!("could not start the mpv event thread: {error}"))?;

        Ok((
            Player {
                mpv,
                events: Some(events),
            },
            receiver,
        ))
    }

    pub fn mpv(&self) -> &Arc<Mpv> {
        &self.mpv
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

    /// Stops playback, saves the resume position, and ends the event thread.
    pub fn shutdown(&mut self) {
        if let Some(events) = self.events.take() {
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

fn pump_events(mpv: &Mpv, sender: &async_channel::Sender<Vec<Event>>) {
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
