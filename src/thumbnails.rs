//! Seek-bar preview thumbnails.
//!
//! Like IINA, Patina makes 100 evenly spaced thumbnails per local video. A
//! second, headless mpv core opens the file paused, seeks to each keyframe
//! and renders it with mpv's software renderer, on a thread of its own.
//! Thumbnails are delivered as they are made, and saved as JPEGs in a disk
//! cache keyed by the file's path, size and modification time.

use crate::mpv::render::RenderContext;
use crate::mpv::{Event, Mpv, Node};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// Thumbnails per file.
pub const COUNT: usize = 100;
const CACHE_MAGIC: &[u8; 8] = b"PATTHMB1";

#[derive(Debug, Clone, PartialEq)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug)]
pub enum Update {
    Ready(usize, Thumbnail),
    Finished,
}

pub struct Request {
    pub path: PathBuf,
    pub duration: f64,
    /// Width in pixels.
    pub width: u32,
    pub cache_dir: Option<PathBuf>,
    pub cache_limit_bytes: u64,
}

/// A running generation; dropping it stops the work.
pub struct Job {
    cancel: Arc<AtomicBool>,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub fn start(request: Request) -> (Job, async_channel::Receiver<Update>) {
    let cancel = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = async_channel::unbounded();
    let thread_cancel = cancel.clone();
    let spawned = std::thread::Builder::new()
        .name("thumbnails".into())
        .spawn(move || {
            let started = Instant::now();
            match run(&request, &thread_cancel, &sender) {
                Ok(made) => log::debug!(
                    "{made} thumbnails for {} in {:.1} s",
                    request.path.display(),
                    started.elapsed().as_secs_f64()
                ),
                Err(error) => log::info!("no thumbnails for {}: {error}", request.path.display()),
            }
            let _ = sender.send_blocking(Update::Finished);
        });
    if let Err(error) = spawned {
        log::warn!("could not start the thumbnail thread: {error}");
    }
    (Job { cancel }, receiver)
}

fn run(
    request: &Request,
    cancel: &AtomicBool,
    sender: &async_channel::Sender<Update>,
) -> Result<usize, String> {
    let metadata = std::fs::metadata(&request.path).map_err(|error| error.to_string())?;
    let cache_file = request
        .cache_dir
        .as_ref()
        .map(|dir| dir.join(format!("{:016x}.thumbs", cache_key(request, &metadata))));

    if let Some(file) = &cache_file
        && let Some(thumbnails) = read_cache(file)
    {
        // Mark it recently used, for eviction.
        if let Ok(handle) = std::fs::File::options().write(true).open(file) {
            let _ = handle.set_modified(SystemTime::now());
        }
        let count = thumbnails.iter().flatten().count();
        for (index, thumbnail) in thumbnails.into_iter().enumerate() {
            if let Some(thumbnail) = thumbnail
                && sender
                    .send_blocking(Update::Ready(index, thumbnail))
                    .is_err()
            {
                break;
            }
        }
        return Ok(count);
    }

    let thumbnails = generate(request, cancel, sender)?;
    let count = thumbnails.iter().flatten().count();
    if let (Some(file), false) = (&cache_file, cancel.load(Ordering::Relaxed))
        && count > 0
    {
        write_cache(file, &thumbnails);
        if let Some(dir) = &request.cache_dir {
            evict(dir, request.cache_limit_bytes);
        }
    }
    Ok(count)
}

/// Signals from mpv's render update callback that a frame is waiting.
#[derive(Default)]
struct FrameSignal {
    ready: Mutex<bool>,
}

impl FrameSignal {
    fn notify(&self) {
        *self.ready.lock().unwrap_or_else(|e| e.into_inner()) = true;
    }

    /// Returns whether a frame arrived since the last call.
    fn take(&self) -> bool {
        std::mem::replace(
            &mut *self.ready.lock().unwrap_or_else(|e| e.into_inner()),
            false,
        )
    }
}

fn generate(
    request: &Request,
    cancel: &AtomicBool,
    sender: &async_channel::Sender<Update>,
) -> Result<Vec<Option<Thumbnail>>, String> {
    let builder = Mpv::builder()?;
    for (name, value) in [
        ("vo", "libmpv"),
        ("ao", "null"),
        ("aid", "no"),
        ("sid", "no"),
        ("pause", "yes"),
        ("idle", "yes"),
        ("keep-open", "always"),
        ("hr-seek", "no"),
        ("hwdec", "no"),
        ("vd-lavc-skiploopfilter", "all"),
        ("vd-lavc-fast", "yes"),
        ("vd-lavc-threads", "2"),
        ("sws-fast", "yes"),
        ("cache", "no"),
        ("config", "no"),
        ("load-scripts", "no"),
        ("ytdl", "no"),
        ("osc", "no"),
        ("input-default-bindings", "no"),
        ("terminal", "no"),
        ("resume-playback", "no"),
        ("save-position-on-quit", "no"),
        ("sub-auto", "no"),
        ("audio-file-auto", "no"),
    ] {
        builder
            .set_option(name, value)
            .map_err(|error| error.to_string())?;
    }
    let mpv = Arc::new(builder.initialize().map_err(|error| error.to_string())?);

    let signal = Arc::new(FrameSignal::default());
    let mut context =
        RenderContext::new_software(mpv.clone()).map_err(|error| error.to_string())?;
    {
        let signal = signal.clone();
        context.set_update_callback(move || signal.notify());
    }

    let path = request.path.to_string_lossy();
    mpv.command(&["loadfile", &path])
        .map_err(|error| error.to_string())?;
    let mut scratch = vec![0u8; 16 * 16 * 4];
    wait_for_restart(
        &mpv,
        &context,
        &signal,
        cancel,
        Duration::from_secs(15),
        (16, 16),
        &mut scratch,
    )?;

    let params = mpv
        .get_property("video-params")
        .map_err(|error| error.to_string())?;
    let size = |key: &str| params.get(key).and_then(Node::as_i64).unwrap_or(0);
    let (mut dw, mut dh) = (size("dw"), size("dh"));
    if size("rotate") % 180 == 90 {
        std::mem::swap(&mut dw, &mut dh);
    }
    if dw <= 0 || dh <= 0 {
        return Err("the file has no video".into());
    }
    let width = request.width.max(16);
    let height = ((width as f64 * dh as f64 / dw as f64).round() as u32).max(8) & !1;

    let mut thumbnails = vec![None; COUNT];
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    for (index, slot) in thumbnails.iter_mut().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let time = request.duration * (index as f64 + 0.5) / COUNT as f64;
        if let Err(error) = mpv.command(&["seek", &format!("{time:.3}"), "absolute+keyframes"]) {
            log::debug!("thumbnail seek failed: {error}");
            continue;
        }
        if let Err(error) = wait_for_restart(
            &mpv,
            &context,
            &signal,
            cancel,
            Duration::from_secs(5),
            (width, height),
            &mut pixels,
        ) {
            log::debug!("thumbnail seek to {time:.1} s: {error}");
            continue;
        }
        context.frame_ready();
        if let Err(error) = context.render_software(width, height, &mut pixels) {
            log::debug!("thumbnail render failed: {error}");
            continue;
        }
        let mut rgba = pixels.clone();
        for pixel in rgba.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
        let thumbnail = Thumbnail {
            width,
            height,
            rgba,
        };
        *slot = Some(thumbnail.clone());
        if sender
            .send_blocking(Update::Ready(index, thumbnail))
            .is_err()
        {
            break;
        }
    }

    // The render context must go before the core it renders.
    drop(context);
    let _ = mpv.command(&["quit"]);
    Ok(thumbnails)
}

/// Waits for a seek (or the initial load) to finish. Frames that arrive in
/// the meantime are rendered right away: mpv holds a seek back until the
/// previous frame has been rendered.
fn wait_for_restart(
    mpv: &Mpv,
    context: &RenderContext,
    signal: &FrameSignal,
    cancel: &AtomicBool,
    timeout: Duration,
    (width, height): (u32, u32),
    pixels: &mut [u8],
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        if signal.take() && context.frame_ready() {
            let _ = context.render_software(width, height, pixels);
        }
        match mpv.wait_event(0.005) {
            Some(Event::PlaybackRestart) => return Ok(()),
            Some(Event::EndFile {
                error: Some(error), ..
            }) => return Err(error.to_string()),
            Some(Event::Shutdown) => return Err("mpv shut down".into()),
            _ => {}
        }
    }
    Err("timed out".into())
}

/// FNV-1a over everything that changes the thumbnails, so a different file,
/// an edited file or another size gets new ones.
fn cache_key(request: &Request, metadata: &std::fs::Metadata) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    feed(CACHE_MAGIC);
    feed(request.path.to_string_lossy().as_bytes());
    feed(&metadata.len().to_le_bytes());
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .unwrap_or_default();
    feed(&modified.as_nanos().to_le_bytes());
    feed(&request.width.to_le_bytes());
    feed(&(COUNT as u32).to_le_bytes());
    hash
}

/// Cache layout: magic, count, then per thumbnail a little-endian length
/// and that many bytes of JPEG (0 for a missing thumbnail).
fn write_cache(file: &Path, thumbnails: &[Option<Thumbnail>]) {
    let encode = |thumbnail: &Thumbnail| -> Option<Vec<u8>> {
        let rgb: Vec<u8> = thumbnail
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect();
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85)
            .encode(
                &rgb,
                thumbnail.width,
                thumbnail.height,
                image::ExtendedColorType::Rgb8,
            )
            .ok()?;
        Some(jpeg)
    };
    let mut data = CACHE_MAGIC.to_vec();
    data.extend_from_slice(&(thumbnails.len() as u32).to_le_bytes());
    for thumbnail in thumbnails {
        let jpeg = thumbnail.as_ref().and_then(encode).unwrap_or_default();
        data.extend_from_slice(&(jpeg.len() as u32).to_le_bytes());
        data.extend_from_slice(&jpeg);
    }
    let result = (|| -> std::io::Result<()> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temporary = file.with_extension("partial");
        std::fs::File::create(&temporary)?.write_all(&data)?;
        std::fs::rename(&temporary, file)
    })();
    if let Err(error) = result {
        log::warn!("could not cache thumbnails in {}: {error}", file.display());
    }
}

fn read_cache(file: &Path) -> Option<Vec<Option<Thumbnail>>> {
    let mut data = Vec::new();
    std::fs::File::open(file)
        .ok()?
        .read_to_end(&mut data)
        .ok()?;
    let rest = data.strip_prefix(CACHE_MAGIC.as_slice())?;
    let (count, mut rest) = rest.split_first_chunk::<4>()?;
    let count = u32::from_le_bytes(*count) as usize;
    if count != COUNT {
        return None;
    }
    let mut thumbnails = Vec::with_capacity(count);
    for _ in 0..count {
        let (length, tail) = rest.split_first_chunk::<4>()?;
        let length = u32::from_le_bytes(*length) as usize;
        let (jpeg, tail) = tail.split_at_checked(length)?;
        rest = tail;
        thumbnails.push((length > 0).then(|| decode(jpeg)).flatten());
    }
    Some(thumbnails)
}

fn decode(jpeg: &[u8]) -> Option<Thumbnail> {
    let image = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg)
        .ok()?
        .to_rgba8();
    Some(Thumbnail {
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
    })
}

/// Deletes the least recently used cache files until the cache fits.
fn evict(dir: &Path, limit: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "thumbs"))
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            Some((metadata.modified().ok()?, metadata.len(), entry.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
    files.sort();
    for (_, size, path) in files {
        if total <= limit {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= size;
        }
    }
}

/// The total size of the thumbnail cache, for the preferences window.
pub fn cache_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.metadata().ok())
                .map(|metadata| metadata.len())
                .sum()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_video(dir: &Path) -> Option<PathBuf> {
        // A short clip made on the fly, so the test needs no fixtures.
        crate::mpv::testing::headless()?;
        let path = dir.join("clip.mkv");
        let status = std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc2=size=320x180:rate=10:duration=6")
            .args(["-c:v", "libx264", "-g", "5", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .status()
            .ok()?;
        status.success().then_some(path)
    }

    #[test]
    fn thumbnails_are_generated_then_served_from_the_cache() {
        let dir = std::env::temp_dir().join(format!("patina-thumbs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let Some(path) = test_video(&dir) else {
            eprintln!("skipping: needs libmpv and ffmpeg");
            return;
        };
        let request = || Request {
            path: path.clone(),
            duration: 6.0,
            width: 64,
            cache_dir: Some(dir.join("cache")),
            cache_limit_bytes: 10 << 20,
        };
        let collect = |receiver: async_channel::Receiver<Update>| {
            let mut thumbnails = Vec::new();
            while let Ok(update) = receiver.recv_blocking() {
                match update {
                    Update::Ready(index, thumbnail) => thumbnails.push((index, thumbnail)),
                    Update::Finished => break,
                }
            }
            thumbnails
        };

        let (_job, receiver) = start(request());
        let made = collect(receiver);
        assert!(
            made.len() >= COUNT / 2,
            "made only {} thumbnails",
            made.len()
        );
        let (_, first) = &made[0];
        assert_eq!((first.width, first.height), (64, 36));
        assert_eq!(first.rgba.len(), 64 * 36 * 4);
        // testsrc2 is colourful: the picture is not a blank frame.
        assert!(
            first
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[0] > 100 || p[1] > 100)
        );

        let (_job, receiver) = start(request());
        let cached = collect(receiver);
        assert_eq!(cached.len(), made.len());
        assert_eq!(cached[0].1.width, 64);
        assert!(cache_size(&dir.join("cache")) > 0);

        evict(&dir.join("cache"), 0);
        assert_eq!(cache_size(&dir.join("cache")), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
