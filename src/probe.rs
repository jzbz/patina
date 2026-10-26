//! Reads the duration and tags of local files for the playlist, the way
//! IINA prefetches them, without playing anything.
//!
//! One headless mpv core on a thread of its own opens each requested file
//! just far enough to know its length, then moves on. Results are sent back
//! as they come; callers cache them.

use crate::mpv::{Event, Mpv, Node};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Probe {
    pub duration: f64,
    pub title: String,
    pub artist: String,
    pub album: String,
}

pub struct Prober {
    requests: async_channel::Sender<String>,
}

impl Prober {
    /// Starts the prober; results arrive on the returned receiver as
    /// `(path, probe)`, with `None` for files that could not be read.
    pub fn start() -> (Self, async_channel::Receiver<(String, Option<Probe>)>) {
        let (requests, queue) = async_channel::unbounded::<String>();
        let (results, receiver) = async_channel::unbounded();
        let spawned = std::thread::Builder::new()
            .name("probe".into())
            .spawn(move || {
                let mpv = match headless() {
                    Ok(mpv) => mpv,
                    Err(error) => {
                        log::warn!("no playlist durations: {error}");
                        return;
                    }
                };
                while let Ok(path) = queue.recv_blocking() {
                    let probe = probe(&mpv, &path);
                    if results.send_blocking((path, probe)).is_err() {
                        break;
                    }
                }
                let _ = mpv.command(&["quit"]);
            });
        if let Err(error) = spawned {
            log::warn!("could not start the prober: {error}");
        }
        (Self { requests }, receiver)
    }

    pub fn request(&self, path: &str) {
        let _ = self.requests.try_send(path.to_owned());
    }
}

fn headless() -> Result<Mpv, String> {
    let builder = Mpv::builder()?;
    for (name, value) in [
        // Tracks stay selected (mpv won't open a file with none), but
        // nothing is shown or heard, and playback starts paused.
        ("vo", "null"),
        ("ao", "null"),
        ("sid", "no"),
        ("pause", "yes"),
        ("idle", "yes"),
        ("cache", "no"),
        ("config", "no"),
        ("load-scripts", "no"),
        ("ytdl", "no"),
        ("osc", "no"),
        ("terminal", "no"),
        ("resume-playback", "no"),
        ("save-position-on-quit", "no"),
        ("sub-auto", "no"),
        ("audio-file-auto", "no"),
        ("cover-art-auto", "no"),
        ("autocreate-playlist", "no"),
    ] {
        builder
            .set_option(name, value)
            .map_err(|error| error.to_string())?;
    }
    builder.initialize().map_err(|error| error.to_string())
}

fn probe(mpv: &Mpv, path: &str) -> Option<Probe> {
    mpv.command(&["loadfile", path, "replace"]).ok()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if Instant::now() > deadline {
            log::debug!("timed out reading {path}");
            let _ = mpv.command(&["stop"]);
            return None;
        }
        match mpv.wait_event(0.5) {
            Some(Event::FileLoaded) => break,
            Some(Event::EndFile { error: Some(_), .. }) => return None,
            _ => {}
        }
    }
    let duration = mpv.get_property("duration").ok()?.as_f64().unwrap_or(0.0);
    let metadata = mpv.get_property("metadata").unwrap_or_default();
    let tag = |keys: &[&str]| match &metadata {
        Node::Map(entries) => entries
            .iter()
            .find(|(key, _)| keys.iter().any(|wanted| key.eq_ignore_ascii_case(wanted)))
            .and_then(|(_, value)| value.as_str())
            .unwrap_or_default()
            .to_owned(),
        _ => String::new(),
    };
    let probe = Probe {
        duration,
        title: tag(&["title"]),
        artist: tag(&["artist", "album_artist"]),
        album: tag(&["album"]),
    };
    let _ = mpv.command(&["stop"]);
    Some(probe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_tags_are_read_without_playing() {
        if crate::mpv::testing::headless().is_none() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("patina-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.mka");
        let made = std::process::Command::new("ffmpeg")
            .stdin(std::process::Stdio::null())
            .args([
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=duration=3",
            ])
            .args(["-metadata", "title=Tone", "-metadata", "artist=Patina"])
            .arg(&path)
            .status()
            .is_ok_and(|status| status.success());
        if !made {
            crate::mpv::testing::skip("PATINA_TEST_REQUIRE_FFMPEG", "needs ffmpeg");
            return;
        }
        let (prober, results) = Prober::start();
        prober.request(&path.to_string_lossy());
        prober.request(&dir.join("missing.mkv").to_string_lossy());
        let (_, first) = results.recv_blocking().unwrap();
        let first = first.unwrap();
        assert!(
            (first.duration - 3.0).abs() < 0.1,
            "duration {}",
            first.duration
        );
        assert_eq!(
            (first.title.as_str(), first.artist.as_str()),
            ("Tone", "Patina")
        );
        let (_, missing) = results.recv_blocking().unwrap();
        assert!(missing.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
