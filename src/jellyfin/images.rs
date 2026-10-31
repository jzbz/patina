//! Posters, backdrops and other art from Jellyfin servers.
//!
//! The library view asks for dozens of images at once, more as the user
//! scrolls, and none of that work may happen on the interface thread. A
//! small pool of worker threads fetches each image, decodes it and scales
//! it down to the width it is shown at; on the interface thread it becomes
//! a `slint::Image`, and the most recently used are kept in memory. The
//! bytes the server sent are kept on disk as well, so the library shows its
//! posters at the next start without asking the server again. Files are
//! named after the URL, which carries the image's tag: the tag changes when
//! the image does, so an outdated file is never shown, and it simply ages
//! out of the cache.
//!
//! Each request carries the generation of whoever made it. The library view
//! moves its generation on when the user leaves a page, and the workers
//! skip requests whose generation has moved on, so the posters of a page
//! the user has left don't hold up the next one. When more are waiting than
//! fit on a screen the newest go first, since they are where the user has
//! scrolled to. A URL is fetched once however many widths it is asked for
//! at, while it waits or while it is fetched, and each width is made from
//! the same bytes.
//!
//! The pool knows nothing about HTTP: it is handed a function that fetches
//! an absolute URL.

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

/// Fetches the bytes at an absolute URL, with the headers, timeouts and
/// certificates of the server it is on. The cache is app-wide and holds
/// every signed-in server's images, so the function must route each URL by
/// its server (scheme, host and port), as `api::Clients::fetch` does: one
/// server's client would send another's requests without their token and
/// with the wrong certificates.
pub type Fetch = Arc<dyn Fn(&str) -> Result<Vec<u8>, String> + Send + Sync>;

/// Past this many waiting requests the newest are fetched first. A screen
/// of posters is fewer than this, so a page that just opened fills in from
/// the top.
const LONG_QUEUE: usize = 32;
/// A failed image isn't asked for again for this long, so that a view that
/// asks for it whenever a row is drawn doesn't keep the server busy.
const RETRY_AFTER: Duration = Duration::from_secs(30);
/// Images larger than this are refused rather than decoded. Art never comes
/// this large, and the server may not send what it was asked for.
pub(super) const MAX_SIDE: u32 = 8192;
const MAX_DECODE_BYTES: u64 = 64 << 20;
/// No image is kept wider than this, the widest the client asks the server
/// for (`api::image_width`), even one asked for at the server's own size.
const MAX_WIDTH: u32 = 3840;
/// Blurhashes are decoded this wide. They are a blur anyway, and the view
/// stretches them to the size of the image they stand in for.
const PLACEHOLDER_WIDTH: u32 = 32;
/// The cache's own files in its folder: images, and images being written.
/// Nothing else there is counted or deleted.
const EXTENSIONS: [&str; 2] = ["img", "partial"];

/// Names temporary files apart while several workers write at once.
static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct Options {
    /// Where fetched images are kept between runs; None keeps them in
    /// memory only.
    pub dir: Option<PathBuf>,
    /// The most the folder may hold, in bytes.
    pub disk_limit: u64,
    /// The most the decoded images kept in memory may take, in bytes.
    pub memory_limit: usize,
    pub workers: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            dir: None,
            disk_limit: 256 << 20,
            memory_limit: 96 << 20,
            workers: 4,
        }
    }
}

/// A requester's generation. Clones share it. The library view gives its
/// requests the same one and calls `bump` when the user leaves a page;
/// something that should always arrive, such as the playing track's cover,
/// keeps one of its own and never bumps it.
#[derive(Debug, Clone, Default)]
pub struct Generation(Arc<AtomicU64>);

impl Generation {
    /// Drops the requests made so far that no worker has started on, and
    /// the callbacks of all of them.
    pub fn bump(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    fn stamp(&self) -> Stamp {
        Stamp {
            counter: self.0.clone(),
            generation: self.0.load(Ordering::Relaxed),
        }
    }
}

/// The generation a request was made in.
#[derive(Debug, Clone)]
struct Stamp {
    counter: Arc<AtomicU64>,
    generation: u64,
}

impl Stamp {
    fn is_current(&self) -> bool {
        self.counter.load(Ordering::Relaxed) == self.generation
    }
}

/// What an image is kept under: its URL and the width it was scaled to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    url: String,
    max_width: Option<u32>,
}

/// What a worker sends back to the interface thread.
enum Outcome {
    Ready(Key, SharedPixelBuffer<Rgba8Pixel>),
    Failed(Key, String),
    /// Every generation that wanted it had moved on.
    Skipped(Key),
}

/// One width of an image, and everyone who asked for it.
struct Want {
    max_width: Option<u32>,
    stamps: Vec<Stamp>,
}

impl Want {
    fn is_current(&self) -> bool {
        self.stamps.iter().any(Stamp::is_current)
    }
}

/// Adds a request for `max_width` to the widths wanted of a URL.
fn want(wants: &mut Vec<Want>, max_width: Option<u32>, stamp: Stamp) {
    match wants.iter_mut().find(|want| want.max_width == max_width) {
        Some(want) => {
            want.stamps.retain(Stamp::is_current);
            if !want
                .stamps
                .iter()
                .any(|known| Arc::ptr_eq(&known.counter, &stamp.counter))
            {
                want.stamps.push(stamp);
            }
        }
        None => wants.push(Want {
            max_width,
            stamps: vec![stamp],
        }),
    }
}

/// A URL to fetch, and the widths to make of it.
struct Job {
    url: String,
    wants: Vec<Want>,
}

/// A URL a worker is fetching: the widths it is making, and those asked
/// for since, which it makes from the same image when it is done.
struct Running {
    widths: Vec<Option<u32>>,
    more: Vec<Want>,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    running: HashMap<String, Running>,
    closed: bool,
}

impl Queue {
    /// The next request still wanted, newest first when many are waiting.
    /// The widths nobody wants any more are reported as skipped on the way.
    fn next(&mut self, results: &async_channel::Sender<Outcome>) -> Option<Job> {
        loop {
            let job = if self.jobs.len() > LONG_QUEUE {
                self.jobs.pop_back()
            } else {
                self.jobs.pop_front()
            }?;
            let (wanted, stale): (Vec<Want>, Vec<Want>) =
                job.wants.into_iter().partition(Want::is_current);
            for want in stale {
                let _ = results.try_send(Outcome::Skipped(Key {
                    url: job.url.clone(),
                    max_width: want.max_width,
                }));
            }
            if !wanted.is_empty() {
                self.running.insert(
                    job.url.clone(),
                    Running {
                        widths: wanted.iter().map(|want| want.max_width).collect(),
                        more: Vec::new(),
                    },
                );
                return Some(Job {
                    url: job.url,
                    wants: wanted,
                });
            }
        }
    }
}

/// What the workers share with the interface thread.
struct Shared {
    fetch: Fetch,
    disk: Option<Disk>,
    queue: Mutex<Queue>,
    wake: Condvar,
    results: async_channel::Sender<Outcome>,
}

impl Shared {
    fn enqueue(&self, key: Key, stamp: Stamp) {
        let mut queue = lock(&self.queue);
        if let Some(index) = queue.jobs.iter().position(|job| job.url == key.url) {
            let long = queue.jobs.len() > LONG_QUEUE;
            let Some(mut job) = queue.jobs.remove(index) else {
                return;
            };
            want(&mut job.wants, key.max_width, stamp);
            // Asked for again, it is on screen again: while the newest go
            // first, it joins them.
            if long {
                queue.jobs.push_back(job);
            } else {
                queue.jobs.insert(index, job);
            }
        } else if let Some(running) = queue.running.get_mut(&key.url) {
            // A width being made already reaches every waiter for it.
            if !running.widths.contains(&key.max_width) {
                want(&mut running.more, key.max_width, stamp);
            }
        } else {
            queue.jobs.push_back(Job {
                url: key.url,
                wants: vec![Want {
                    max_width: key.max_width,
                    stamps: vec![stamp],
                }],
            });
            drop(queue);
            self.wake.notify_one();
        }
    }

    fn work(&self) {
        while let Some(job) = self.wait_for_job() {
            // A panic while fetching or decoding must not take the worker
            // with it, or the URL would stay marked as running and never be
            // asked for again.
            let image = std::panic::catch_unwind(AssertUnwindSafe(|| self.obtain(&job.url)))
                .unwrap_or_else(|_| Err("the worker panicked".into()));
            let mut widths: Vec<Option<u32>> =
                job.wants.iter().map(|want| want.max_width).collect();
            loop {
                let outcomes: Vec<Outcome> = widths
                    .iter()
                    .map(|max_width| {
                        let key = Key {
                            url: job.url.clone(),
                            max_width: *max_width,
                        };
                        let made = image.as_ref().map_err(String::clone).and_then(|image| {
                            std::panic::catch_unwind(AssertUnwindSafe(|| fit(image, *max_width)))
                                .map_err(|_| "the worker panicked".to_owned())
                        });
                        match made {
                            Ok(buffer) => Outcome::Ready(key, buffer),
                            Err(error) => Outcome::Failed(key, error),
                        }
                    })
                    .collect();
                // Sent with the queue held, so that a request for a width
                // either finds it still running (and its waiter gets this
                // result) or not running (and is fetched anew).
                let mut queue = lock(&self.queue);
                for outcome in outcomes {
                    let _ = self.results.try_send(outcome);
                }
                let more = match queue.running.get_mut(&job.url) {
                    Some(running) if !running.more.is_empty() => {
                        let more: Vec<Option<u32>> =
                            running.more.drain(..).map(|want| want.max_width).collect();
                        running.widths.extend(&more);
                        more
                    }
                    _ => Vec::new(),
                };
                if more.is_empty() {
                    queue.running.remove(&job.url);
                    break;
                }
                widths = more;
            }
        }
    }

    fn wait_for_job(&self) -> Option<Job> {
        let mut queue = lock(&self.queue);
        loop {
            if queue.closed {
                return None;
            }
            if let Some(job) = queue.next(&self.results) {
                return Some(job);
            }
            queue = self
                .wake
                .wait(queue)
                .unwrap_or_else(|error| error.into_inner());
        }
    }

    /// The image from the disk cache, or else from the server.
    fn obtain(&self, url: &str) -> Result<image::DynamicImage, String> {
        if let Some(disk) = &self.disk
            && let Some(bytes) = disk.read(url)
        {
            match decode_image(&bytes) {
                Ok(image) => return Ok(image),
                Err(error) => {
                    log::debug!("dropping the cached image for {url}: {error}");
                    disk.remove(url);
                }
            }
        }
        let bytes = (self.fetch)(url)?;
        // Decoded before it is kept, so an error page is never cached.
        let image = decode_image(&bytes)?;
        if let Some(disk) = &self.disk {
            disk.write(url, &bytes);
        }
        Ok(image)
    }
}

/// The image cache's handle on the interface thread. Clones share it, and
/// its workers stop when the last one goes.
#[derive(Clone)]
pub struct Images(Rc<Inner>);

struct Inner {
    shared: Arc<Shared>,
    state: RefCell<State>,
}

struct State {
    memory: Memory,
    /// Callbacks waiting for an image, by what they asked for.
    waiting: HashMap<Key, Vec<Waiter>>,
    /// When images failed to load, so they aren't asked for again at once.
    failed: HashMap<Key, Instant>,
}

struct Waiter {
    stamp: Stamp,
    done: Box<dyn FnOnce(Image)>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        let mut queue = lock(&self.shared.queue);
        queue.closed = true;
        queue.jobs.clear();
        drop(queue);
        self.shared.wake.notify_all();
    }
}

impl Images {
    /// Starts the workers. Images come back through the Slint event loop,
    /// which must exist by now.
    pub fn new(options: Options, fetch: Fetch) -> Self {
        let (images, results) = Self::start(options, fetch);
        let weak = Rc::downgrade(&images.0);
        let spawned = slint::spawn_local(async move {
            while let Ok(outcome) = results.recv().await {
                let Some(inner) = weak.upgrade() else { break };
                Images(inner).receive(outcome);
            }
        });
        if let Err(error) = spawned {
            log::warn!("could not receive Jellyfin images: {error}");
        }
        images
    }

    /// Starts the workers; their results arrive on the receiver, for
    /// `receive`.
    fn start(options: Options, fetch: Fetch) -> (Self, async_channel::Receiver<Outcome>) {
        let (results, receiver) = async_channel::unbounded();
        let shared = Arc::new(Shared {
            fetch,
            disk: options.dir.map(|dir| Disk::new(dir, options.disk_limit)),
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
            results,
        });
        for _ in 0..options.workers.max(1) {
            let shared = shared.clone();
            let spawned = std::thread::Builder::new()
                .name("jellyfin-images".into())
                .spawn(move || shared.work());
            if let Err(error) = spawned {
                log::warn!("could not start a Jellyfin image worker: {error}");
            }
        }
        let inner = Inner {
            shared,
            state: RefCell::new(State {
                memory: Memory::new(options.memory_limit),
                waiting: HashMap::new(),
                failed: HashMap::new(),
            }),
        };
        (Self(Rc::new(inner)), receiver)
    }

    /// The image at `url`, scaled down to at most `max_width` pixels wide
    /// (None or 0 keeps the server's size, up to `MAX_WIDTH`). When it is
    /// in memory it is returned at once and `done` is not called.
    /// Otherwise it is fetched, and `done` is called with it later on this
    /// thread, unless it fails (when the view keeps its placeholder) or
    /// `generation` has moved on by then. An image that failed isn't asked
    /// for again for half a minute.
    pub fn load(
        &self,
        generation: &Generation,
        url: &str,
        max_width: Option<u32>,
        done: impl FnOnce(Image) + 'static,
    ) -> Option<Image> {
        let key = Key {
            url: url.to_owned(),
            max_width: max_width.filter(|width| *width > 0),
        };
        let stamp = generation.stamp();
        {
            let mut state = self.0.state.borrow_mut();
            if let Some(image) = state.memory.get(&key) {
                return Some(image);
            }
            if let Some(failed) = state.failed.get(&key) {
                if failed.elapsed() < RETRY_AFTER {
                    return None;
                }
                state.failed.remove(&key);
            }
            let waiters = state.waiting.entry(key.clone()).or_default();
            waiters.retain(|waiter| waiter.stamp.is_current());
            waiters.push(Waiter {
                stamp: stamp.clone(),
                done: Box::new(done),
            });
        }
        self.0.shared.enqueue(key, stamp);
        None
    }

    fn receive(&self, outcome: Outcome) {
        let mut state = self.0.state.borrow_mut();
        let (key, image) = match outcome {
            Outcome::Ready(key, buffer) => {
                let bytes = buffer.width() as usize * buffer.height() as usize * 4;
                let image = Image::from_rgba8(buffer);
                state.memory.insert(key.clone(), image.clone(), bytes);
                (key, Some(image))
            }
            Outcome::Failed(key, error) => {
                log::debug!("could not load the image {}: {error}", key.url);
                state
                    .failed
                    .retain(|_, failed| failed.elapsed() < RETRY_AFTER);
                state.failed.insert(key.clone(), Instant::now());
                (key, None)
            }
            Outcome::Skipped(key) => {
                // A newer request for it, if any, was queued anew.
                if let Some(waiters) = state.waiting.get_mut(&key) {
                    waiters.retain(|waiter| waiter.stamp.is_current());
                    if waiters.is_empty() {
                        state.waiting.remove(&key);
                    }
                }
                return;
            }
        };
        let waiters = state.waiting.remove(&key).unwrap_or_default();
        // Called with the state released, since a callback may ask for more.
        drop(state);
        if let Some(image) = image {
            for waiter in waiters {
                if waiter.stamp.is_current() {
                    (waiter.done)(image.clone());
                }
            }
        }
    }

    /// Changes how much the disk cache may hold; it shrinks to fit when the
    /// next image is written.
    pub fn set_disk_limit(&self, bytes: u64) {
        if let Some(disk) = &self.0.shared.disk {
            disk.limit.store(bytes, Ordering::Relaxed);
        }
    }

    /// What the disk cache holds, in bytes, for the preferences window.
    /// The folder can hold thousands of files, so it is counted on a
    /// thread of its own, and the answer comes on the receiver.
    pub fn disk_usage(&self) -> async_channel::Receiver<u64> {
        self.on_disk(0, |disk| cache_size(&disk.dir))
    }

    /// Forgets every image, in memory at once and on disk on a thread of
    /// its own; the receiver gets () when the files are gone. Requests on
    /// their way still arrive.
    pub fn clear(&self) -> async_channel::Receiver<()> {
        {
            let mut state = self.0.state.borrow_mut();
            state.memory.clear();
            state.failed.clear();
        }
        self.on_disk((), Disk::clear)
    }

    /// Runs `work` on the disk cache off the interface thread, and sends
    /// what it gives; `none` at once when there is no disk cache.
    fn on_disk<T: Send + 'static>(
        &self,
        none: T,
        work: impl FnOnce(&Disk) -> T + Send + 'static,
    ) -> async_channel::Receiver<T> {
        let (sender, receiver) = async_channel::bounded(1);
        if self.0.shared.disk.is_none() {
            let _ = sender.try_send(none);
            return receiver;
        }
        let shared = self.0.shared.clone();
        let spawned = std::thread::Builder::new()
            .name("jellyfin-image-files".into())
            .spawn(move || {
                if let Some(disk) = &shared.disk {
                    let _ = sender.try_send(work(disk));
                }
            });
        // Without the thread the sender is gone, and the receiver says so.
        if let Err(error) = spawned {
            log::warn!("could not start a thread for the Jellyfin image cache: {error}");
        }
        receiver
    }
}

/// Decoded images, the least recently used dropped past a size.
struct Memory {
    entries: HashMap<Key, Entry>,
    bytes: usize,
    limit: usize,
    clock: u64,
}

struct Entry {
    image: Image,
    bytes: usize,
    used: u64,
}

impl Memory {
    fn new(limit: usize) -> Self {
        Self {
            entries: HashMap::new(),
            bytes: 0,
            limit,
            clock: 0,
        }
    }

    fn get(&mut self, key: &Key) -> Option<Image> {
        self.clock += 1;
        let entry = self.entries.get_mut(key)?;
        entry.used = self.clock;
        Some(entry.image.clone())
    }

    /// Keeps an image, unless it alone would take more than half the
    /// limit: that one goes to the callbacks waiting for it only, rather
    /// than push out everything else.
    fn insert(&mut self, key: Key, image: Image, bytes: usize) {
        if bytes > self.limit / 2 {
            return;
        }
        self.clock += 1;
        let entry = Entry {
            image,
            bytes,
            used: self.clock,
        };
        if let Some(old) = self.entries.insert(key.clone(), entry) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
        while self.bytes > self.limit {
            let oldest = self
                .entries
                .iter()
                .filter(|(known, _)| **known != key)
                .min_by_key(|(_, entry)| entry.used)
                .map(|(known, _)| known.clone());
            let Some(oldest) = oldest else { break };
            if let Some(entry) = self.entries.remove(&oldest) {
                self.bytes -= entry.bytes;
            }
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
}

/// The bytes the server sent, one file per URL.
struct Disk {
    dir: PathBuf,
    limit: AtomicU64,
    /// What the folder holds: counted at the first write, then kept up to
    /// date, so that writing an image doesn't mean listing the folder.
    used: Mutex<Option<u64>>,
    /// Set while a worker evicts, so that only one does at a time.
    evicting: AtomicBool,
}

impl Disk {
    fn new(dir: PathBuf, limit: u64) -> Self {
        Self {
            dir,
            limit: AtomicU64::new(limit),
            used: Mutex::new(None),
            evicting: AtomicBool::new(false),
        }
    }

    fn path(&self, url: &str) -> PathBuf {
        self.dir.join(format!("{:016x}.img", fnv1a(url.as_bytes())))
    }

    fn read(&self, url: &str) -> Option<Vec<u8>> {
        let path = self.path(url);
        let bytes = std::fs::read(&path).ok()?;
        // Mark it recently used, for eviction.
        if let Ok(file) = std::fs::File::options().write(true).open(&path) {
            let _ = file.set_modified(SystemTime::now());
        }
        Some(bytes)
    }

    /// Writes the file whole or not at all, then evicts the least recently
    /// used files if the folder is over its limit. The count is only
    /// looked at and changed under the lock; listing and deleting files
    /// happen outside it, so that clearing the cache and the other workers
    /// don't wait on them.
    fn write(&self, url: &str, bytes: &[u8]) {
        let path = self.path(url);
        let temporary = path.with_extension(format!(
            "{}-{}.partial",
            std::process::id(),
            NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(&self.dir)?;
            std::fs::File::create(&temporary)?.write_all(bytes)?;
            std::fs::rename(&temporary, &path)
        })();
        if let Err(error) = result {
            log::debug!(
                "could not cache an image in {}: {error}",
                self.dir.display()
            );
            let _ = std::fs::remove_file(&temporary);
            return;
        }
        let counted = {
            let mut used = lock(&self.used);
            if let Some(total) = used.as_mut() {
                *total += bytes.len() as u64;
            }
            *used
        };
        let total = counted.unwrap_or_else(|| {
            let total = cache_size(&self.dir);
            *lock(&self.used) = Some(total);
            total
        });
        let limit = self.limit.load(Ordering::Relaxed);
        if total > limit && !self.evicting.swap(true, Ordering::AcqRel) {
            // Down to a little under the limit, so that it doesn't run
            // again with the next image.
            let left = evict(&self.dir, limit - limit / 10);
            *lock(&self.used) = Some(left);
            self.evicting.store(false, Ordering::Release);
        }
    }

    fn remove(&self, url: &str) {
        if std::fs::remove_file(self.path(url)).is_ok() {
            *lock(&self.used) = None;
        }
    }

    /// Deletes the cache's files, and nothing else in the folder.
    fn clear(&self) {
        evict(&self.dir, 0);
        *lock(&self.used) = None;
    }
}

/// Decodes a JPEG or PNG, within the limits above. Trickplay tiles are
/// decoded so too, at their own size.
pub(super) fn decode_image(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    reader.decode().map_err(|error| error.to_string())
}

/// Decodes a JPEG or PNG and scales it down to `max_width`, keeping its
/// shape, and to `MAX_WIDTH` without one, as a worker does in two steps;
/// the tests' way of looking at an image.
#[cfg(test)]
pub(super) fn decode(
    bytes: &[u8],
    max_width: Option<u32>,
) -> Result<SharedPixelBuffer<Rgba8Pixel>, String> {
    Ok(fit(&decode_image(bytes)?, max_width))
}

/// A decoded image at no more than `max_width` (or `MAX_WIDTH`) wide, as
/// pixels Slint takes.
fn fit(image: &image::DynamicImage, max_width: Option<u32>) -> SharedPixelBuffer<Rgba8Pixel> {
    let width = max_width
        .filter(|width| *width > 0)
        .map_or(MAX_WIDTH, |width| width.min(MAX_WIDTH));
    if image.width() > width {
        let height = scaled_height(image.width(), image.height(), width);
        pixels(&image.resize_exact(width, height, image::imageops::FilterType::Triangle))
    } else {
        pixels(image)
    }
}

/// The image's pixels as RGBA, copied once: posters and backdrops are
/// JPEGs, which decode to RGB.
fn pixels(image: &image::DynamicImage) -> SharedPixelBuffer<Rgba8Pixel> {
    let (width, height) = (image.width(), image.height());
    match image {
        image::DynamicImage::ImageRgba8(rgba) => {
            SharedPixelBuffer::clone_from_slice(rgba.as_raw(), width, height)
        }
        image::DynamicImage::ImageRgb8(rgb) => {
            let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
            for (to, from) in buffer.make_mut_slice().iter_mut().zip(rgb.pixels()) {
                let [r, g, b] = from.0;
                *to = Rgba8Pixel { r, g, b, a: 255 };
            }
            buffer
        }
        other => {
            let rgba = other.to_rgba8();
            SharedPixelBuffer::clone_from_slice(rgba.as_raw(), width, height)
        }
    }
}

/// The height that keeps a `width` by `height` image's shape at `to_width`.
fn scaled_height(width: u32, height: u32, to_width: u32) -> u32 {
    let width = u64::from(width.max(1));
    let scaled = (u64::from(height) * u64::from(to_width) + width / 2) / width;
    scaled.clamp(1, u64::from(u32::MAX)) as u32
}

/// A blurred stand-in for an image that hasn't arrived yet, from the
/// blurhash the server sends with each item. `width` and `height` give its
/// shape, the image's or the frame's it fills; it is decoded small, and the
/// view stretches it. None when the hash isn't a blurhash.
pub fn placeholder(hash: &str, width: u32, height: u32) -> Option<Image> {
    placeholder_pixels(hash, width, height).map(Image::from_rgba8)
}

fn placeholder_pixels(
    hash: &str,
    width: u32,
    height: u32,
) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let decoded_height = if width == 0 || height == 0 {
        PLACEHOLDER_WIDTH
    } else {
        scaled_height(width, height, PLACEHOLDER_WIDTH).min(PLACEHOLDER_WIDTH * 4)
    };
    let pixels = blurhash::decode(hash, PLACEHOLDER_WIDTH, decoded_height, 1.0).ok()?;
    Some(SharedPixelBuffer::clone_from_slice(
        &pixels,
        PLACEHOLDER_WIDTH,
        decoded_height,
    ))
}

/// FNV-1a. Unlike std's hasher it is fixed, so cached files keep their
/// names from one Patina version to the next.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The cache's own files in `dir`, with their last use and size.
fn cache_files(dir: &Path) -> Vec<(SystemTime, u64, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| EXTENSIONS.contains(&extension))
        })
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() {
                return None;
            }
            let used = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            Some((used, metadata.len(), entry.path()))
        })
        .collect()
}

/// Deletes the least recently used of the cache's files until they come
/// to at most `limit` bytes, and returns what they come to then.
/// Empties a disk cache's folder as `Images::clear` does, without a cache:
/// for signing out before any image was asked for. Blocks.
pub fn clear_folder(dir: &Path) {
    evict(dir, 0);
}

fn evict(dir: &Path, limit: u64) -> u64 {
    let mut files = cache_files(dir);
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
    total
}

fn cache_size(dir: &Path) -> u64 {
    cache_files(dir).iter().map(|(_, size, _)| size).sum()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::mpsc;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([(x * 255 / width) as u8, (y * 255 / height) as u8, 40, 200])
        });
        let mut bytes = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbImage::from_fn(width, height, |x, _| {
            if x < width / 2 {
                image::Rgb([220, 30, 30])
            } else {
                image::Rgb([30, 30, 220])
            }
        });
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 95)
            .encode_image(&image)
            .unwrap();
        bytes
    }

    fn pixel(buffer: &SharedPixelBuffer<Rgba8Pixel>, x: u32, y: u32) -> [u8; 4] {
        let p = buffer.as_slice()[(y * buffer.width() + x) as usize];
        [p.r, p.g, p.b, p.a]
    }

    fn close(a: [u8; 4], b: [u8; 4], tolerance: u8) -> bool {
        a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= tolerance)
    }

    /// A folder of the test's own, removed when the test ends, passed or
    /// failed.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "patina-jellyfin-images-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_folder_is_cleared_without_a_cache() {
        let folder = Folder::new("clear-folder");
        std::fs::create_dir_all(&folder.0).unwrap();
        std::fs::write(folder.0.join("0123456789abcdef.img"), b"jpeg").unwrap();
        std::fs::write(folder.0.join("notes.txt"), b"theirs").unwrap();
        clear_folder(&folder.0);
        assert!(!folder.0.join("0123456789abcdef.img").exists());
        assert!(folder.0.join("notes.txt").exists());
    }

    /// What a receiver from `disk_usage` or `clear` gets.
    fn answer<T>(receiver: async_channel::Receiver<T>) -> T {
        receiver
            .recv_blocking()
            .expect("an answer from the disk thread")
    }

    /// A stand-in for the client: it serves `body` for every URL, records
    /// what it was asked for, and holds URLs ending in "/gate" until told
    /// to go on, so that a test knows what the one worker is doing.
    struct StandIn {
        fetched: Arc<Mutex<Vec<String>>>,
        started: mpsc::Receiver<()>,
        release: mpsc::Sender<()>,
    }

    impl StandIn {
        fn fetched(&self) -> Vec<String> {
            lock(&self.fetched).clone()
        }
    }

    fn stand_in(
        body: impl Fn(&str) -> Result<Vec<u8>, String> + Send + Sync + 'static,
    ) -> (Fetch, StandIn) {
        let fetched = Arc::new(Mutex::new(Vec::new()));
        let (started_sender, started) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (started_sender, released) = (Mutex::new(started_sender), Mutex::new(released));
        let log = fetched.clone();
        let fetch: Fetch = Arc::new(move |url: &str| {
            lock(&log).push(url.to_owned());
            if url.ends_with("/gate") {
                let _ = lock(&started_sender).send(());
                let _ = lock(&released).recv();
            }
            body(url)
        });
        (
            fetch,
            StandIn {
                fetched,
                started,
                release,
            },
        )
    }

    fn one_worker(dir: Option<PathBuf>) -> Options {
        Options {
            dir,
            workers: 1,
            ..Options::default()
        }
    }

    /// Hands `count` results from the workers to the cache, as the event
    /// loop would.
    fn pump(images: &Images, results: &async_channel::Receiver<Outcome>, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        for _ in 0..count {
            loop {
                match results.try_recv() {
                    Ok(outcome) => {
                        images.receive(outcome);
                        break;
                    }
                    Err(async_channel::TryRecvError::Empty) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("no result from the workers: {error}"),
                }
            }
        }
    }

    /// Asks for an image and counts the callbacks it gets.
    fn ask(images: &Images, generation: &Generation, url: &str) -> Rc<Cell<u32>> {
        let calls = Rc::new(Cell::new(0));
        let counter = calls.clone();
        let immediate = images.load(generation, url, None, move |_| {
            counter.set(counter.get() + 1);
        });
        assert!(immediate.is_none(), "{url} was already in memory");
        calls
    }

    #[test]
    fn jpeg_and_png_are_decoded() {
        let buffer = decode(&jpeg(64, 32), None).unwrap();
        assert_eq!((buffer.width(), buffer.height()), (64, 32));
        assert!(close(pixel(&buffer, 4, 16), [220, 30, 30, 255], 16));
        assert!(close(pixel(&buffer, 60, 16), [30, 30, 220, 255], 16));

        let buffer = decode(&png(64, 32), None).unwrap();
        assert_eq!((buffer.width(), buffer.height()), (64, 32));
        // PNG is lossless, and keeps its transparency.
        assert_eq!(pixel(&buffer, 32, 16), [127, 127, 40, 200]);

        assert!(decode(b"<html>Not found</html>", None).is_err());
        assert!(decode(&[], None).is_err());
    }

    #[test]
    fn images_are_scaled_down_but_never_up() {
        let bytes = png(64, 33);
        let buffer = decode(&bytes, Some(16)).unwrap();
        assert_eq!((buffer.width(), buffer.height()), (16, 8));
        let buffer = decode(&bytes, Some(200)).unwrap();
        assert_eq!((buffer.width(), buffer.height()), (64, 33));
        let buffer = decode(&bytes, Some(0)).unwrap();
        assert_eq!((buffer.width(), buffer.height()), (64, 33));
        // Asked for at the server's own size, an image is still made no
        // wider than the widest the client asks for.
        let wide = png(MAX_WIDTH * 2, 10);
        let buffer = decode(&wide, None).unwrap();
        assert_eq!((buffer.width(), buffer.height()), (MAX_WIDTH, 5));
        let buffer = decode(&wide, Some(MAX_WIDTH * 2)).unwrap();
        assert_eq!(buffer.width(), MAX_WIDTH);
        // A very wide image keeps at least a row.
        assert_eq!(scaled_height(4000, 10, 100), 1);
        assert_eq!(scaled_height(300, 450, 200), 300);
    }

    #[test]
    fn files_are_named_by_a_fixed_hash_of_the_url() {
        // FNV-1a's published values: a different hash would orphan every
        // cached file.
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        let disk = Disk::new(PathBuf::from("cache"), 1000);
        assert_eq!(
            disk.path("a"),
            Path::new("cache").join("af63dc4c8601ec8c.img")
        );
    }

    #[test]
    fn the_disk_cache_keeps_what_was_written() {
        let folder = Folder::new("round-trip");
        let dir = folder.0.clone();
        let disk = Disk::new(dir.clone(), 1 << 20);
        let url = "http://127.0.0.1:8096/Items/1/Images/Primary?tag=a&maxWidth=300";
        assert_eq!(disk.read(url), None);
        disk.write(url, b"poster");
        assert_eq!(disk.read(url).as_deref(), Some(&b"poster"[..]));
        assert_eq!(disk.read(&url.replace("tag=a", "tag=b")), None);
        disk.write(url, b"new poster");
        assert_eq!(disk.read(url).as_deref(), Some(&b"new poster"[..]));
        // No temporary files are left behind.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        assert_eq!(cache_size(&dir), 10);
        disk.remove(url);
        assert_eq!(disk.read(url), None);
        disk.write(url, b"poster");
        // Clearing deletes the cache's files and nothing else, and leaves
        // the folder.
        std::fs::write(dir.join("jellyfin.json"), "{}").unwrap();
        disk.clear();
        assert_eq!(cache_size(&dir), 0);
        assert!(dir.join("jellyfin.json").exists());
        assert_eq!(disk.read(url), None);
        disk.write(url, b"poster");
        assert_eq!(cache_size(&dir), 6);
    }

    #[test]
    fn the_disk_cache_drops_the_least_recently_used() {
        let folder = Folder::new("eviction");
        let dir = folder.0.clone();
        let disk = Disk::new(dir.clone(), 2500);
        let age = |url: &str, hours: u64| {
            std::fs::File::options()
                .write(true)
                .open(disk.path(url))
                .unwrap()
                .set_modified(SystemTime::now() - Duration::from_secs(hours * 3600))
                .unwrap();
        };
        // Someone else's file, older than any image, is neither counted
        // nor deleted.
        std::fs::create_dir_all(&dir).unwrap();
        let other = dir.join("notes.txt");
        std::fs::write(&other, [0; 5000]).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&other)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(10 * 3600))
            .unwrap();
        disk.write("a", &[0; 1000]);
        disk.write("b", &[0; 1000]);
        age("a", 3);
        age("b", 2);
        // Reading marks it used, so b is now the oldest.
        assert!(disk.read("a").is_some());
        disk.write("c", &[0; 1000]);
        assert!(disk.read("b").is_none());
        assert!(disk.read("a").is_some() && disk.read("c").is_some());
        assert_eq!(cache_size(&dir), 2000);

        // A file being written by a worker that is gone counts too.
        std::fs::write(dir.join("0123456789abcdef.1-2.partial"), [0; 300]).unwrap();
        assert_eq!(cache_size(&dir), 2300);
        assert_eq!(evict(&dir, 0), 0);
        assert_eq!(cache_size(&dir), 0);
        assert!(other.exists());
    }

    #[test]
    fn memory_keeps_the_most_recently_used_within_its_limit() {
        let image = || Image::from_rgba8(SharedPixelBuffer::new(5, 5));
        let key = |url: &str| Key {
            url: url.into(),
            max_width: None,
        };
        let mut memory = Memory::new(250);
        memory.insert(key("a"), image(), 100);
        memory.insert(key("b"), image(), 100);
        assert!(memory.get(&key("a")).is_some());
        memory.insert(key("c"), image(), 100);
        assert!(memory.get(&key("b")).is_none());
        assert!(memory.get(&key("a")).is_some());
        assert!(memory.get(&key("c")).is_some());
        assert_eq!(memory.bytes, 200);
        // One that alone takes more than half the limit is not kept, so
        // that it doesn't push out all the others.
        memory.insert(key("big"), image(), 126);
        assert!(memory.get(&key("big")).is_none());
        assert_eq!(memory.entries.len(), 2);
        memory.clear();
        assert_eq!(memory.bytes, 0);
    }

    #[test]
    fn an_image_is_fetched_once_then_served_from_memory() {
        let poster = png(40, 60);
        let (fetch, server) = stand_in(move |_| Ok(poster.clone()));
        let (images, results) = Images::start(one_worker(None), fetch);
        let generation = Generation::default();

        // The worker is held on the first, so the next two are asked for
        // together and wait in the queue.
        let gate = ask(&images, &generation, "http://127.0.0.1/gate");
        server.started.recv().unwrap();
        let first = ask(&images, &generation, "http://127.0.0.1/poster");
        let second = ask(&images, &generation, "http://127.0.0.1/poster");
        server.release.send(()).unwrap();
        pump(&images, &results, 2);
        assert_eq!((gate.get(), first.get(), second.get()), (1, 1, 1));
        assert_eq!(
            server.fetched(),
            ["http://127.0.0.1/gate", "http://127.0.0.1/poster"]
        );

        let image = images
            .load(&generation, "http://127.0.0.1/poster", None, |_| {
                panic!("called for an image in memory")
            })
            .unwrap();
        assert_eq!((image.size().width, image.size().height), (40, 60));
        // Another width is another image.
        let small = Rc::new(Cell::new(None));
        let slot = small.clone();
        let immediate = images.load(
            &generation,
            "http://127.0.0.1/poster",
            Some(20),
            move |image| {
                slot.set(Some(image.size()));
            },
        );
        assert!(immediate.is_none());
        pump(&images, &results, 1);
        let size = small.get().unwrap();
        assert_eq!((size.width, size.height), (20, 30));
        assert_eq!(server.fetched().len(), 3);
    }

    #[test]
    fn requests_of_a_page_left_are_skipped() {
        let poster = png(8, 8);
        let (fetch, server) = stand_in(move |_| Ok(poster.clone()));
        let (images, results) = Images::start(one_worker(None), fetch);
        let page = Generation::default();
        let cover = Generation::default();

        let gate = ask(&images, &page, "http://127.0.0.1/gate");
        server.started.recv().unwrap();
        let left = ask(&images, &page, "http://127.0.0.1/left");
        let lasting = ask(&images, &cover, "http://127.0.0.1/cover");
        page.bump();
        let next = ask(&images, &page, "http://127.0.0.1/next");
        server.release.send(()).unwrap();
        pump(&images, &results, 4);

        // The page's own generation moved on; the cover's did not.
        assert_eq!(
            server.fetched(),
            [
                "http://127.0.0.1/gate",
                "http://127.0.0.1/cover",
                "http://127.0.0.1/next"
            ]
        );
        assert_eq!(
            (gate.get(), left.get(), lasting.get(), next.get()),
            (0, 0, 1, 1)
        );
        // What was fetched for the old page is kept for when the user
        // comes back.
        assert!(
            images
                .load(&page, "http://127.0.0.1/gate", None, |_| {})
                .is_some()
        );
        // And what was skipped is fetched when asked for again.
        let again = ask(&images, &page, "http://127.0.0.1/left");
        pump(&images, &results, 1);
        assert_eq!(again.get(), 1);
        assert_eq!(server.fetched().len(), 4);
    }

    #[test]
    fn the_newest_requests_go_first_when_many_are_waiting() {
        let poster = png(8, 8);
        let (fetch, server) = stand_in(move |_| Ok(poster.clone()));
        let (images, results) = Images::start(one_worker(None), fetch);
        let generation = Generation::default();

        let _gate = ask(&images, &generation, "http://127.0.0.1/gate");
        server.started.recv().unwrap();
        let count = LONG_QUEUE + 8;
        let calls: Vec<_> = (0..count)
            .map(|index| ask(&images, &generation, &format!("http://127.0.0.1/{index}")))
            .collect();
        server.release.send(()).unwrap();
        pump(&images, &results, count + 1);

        assert!(calls.iter().all(|calls| calls.get() == 1));
        let order: Vec<String> = server.fetched()[1..]
            .iter()
            .map(|url| url.trim_start_matches("http://127.0.0.1/").to_owned())
            .collect();
        // The eight past a screenful newest first, then the rest in order.
        let expected: Vec<String> = (LONG_QUEUE..count)
            .rev()
            .chain(0..LONG_QUEUE)
            .map(|index| index.to_string())
            .collect();
        assert_eq!(order, expected);
    }

    #[test]
    fn a_failed_image_is_not_asked_for_again_at_once() {
        let (fetch, server) = stand_in(|_| Err("404 Not Found".into()));
        let (images, results) = Images::start(one_worker(None), fetch);
        let generation = Generation::default();
        let calls = ask(&images, &generation, "http://127.0.0.1/missing");
        pump(&images, &results, 1);
        assert_eq!(calls.get(), 0);
        let again = ask(&images, &generation, "http://127.0.0.1/missing");
        std::thread::sleep(Duration::from_millis(50));
        assert!(results.is_empty());
        assert_eq!((again.get(), server.fetched().len()), (0, 1));
    }

    #[test]
    fn the_disk_cache_outlives_the_cache_and_bad_files_are_fetched_again() {
        let folder = Folder::new("restart");
        let dir = folder.0.clone();
        let url = "http://127.0.0.1/Items/1/Images/Primary?tag=a";
        let poster = jpeg(30, 45);
        let (fetch, server) = stand_in(move |_| Ok(poster.clone()));
        {
            let (images, results) = Images::start(one_worker(Some(dir.clone())), fetch.clone());
            let calls = ask(&images, &Generation::default(), url);
            pump(&images, &results, 1);
            assert_eq!(calls.get(), 1);
            assert!(answer(images.disk_usage()) > 0);
        }
        assert_eq!(server.fetched().len(), 1);

        // A new start finds it on disk.
        let (offline, _) = stand_in(|_| Err("offline".into()));
        let (images, results) = Images::start(one_worker(Some(dir.clone())), offline);
        let calls = ask(&images, &Generation::default(), url);
        pump(&images, &results, 1);
        assert_eq!(calls.get(), 1);

        // A damaged file is dropped and fetched again.
        let file = Disk::new(dir.clone(), 0).path(url);
        std::fs::write(&file, b"damaged").unwrap();
        let (images, results) = Images::start(one_worker(Some(dir.clone())), fetch);
        let calls = ask(&images, &Generation::default(), url);
        pump(&images, &results, 1);
        assert_eq!(calls.get(), 1);
        assert_eq!(server.fetched().len(), 2);
        assert!(decode(&std::fs::read(&file).unwrap(), None).is_ok());

        // Cleared off the interface thread, which hears when it is done.
        answer(images.clear());
        assert_eq!(answer(images.disk_usage()), 0);
        assert!(!file.exists());
        let calls = ask(&images, &Generation::default(), url);
        pump(&images, &results, 1);
        assert_eq!(calls.get(), 1);
        assert_eq!(server.fetched().len(), 3);

        // Without a disk cache the answers come at once.
        let (memory_only, _) = Images::start(one_worker(None), server_less());
        assert_eq!(answer(memory_only.disk_usage()), 0);
        answer(memory_only.clear());
    }

    /// A fetch that never succeeds, for a cache that isn't asked for
    /// anything.
    fn server_less() -> Fetch {
        Arc::new(|_: &str| Err("offline".into()))
    }

    #[test]
    fn a_url_is_fetched_once_for_every_width_asked() {
        let poster = png(40, 60);
        let (fetch, server) = stand_in(move |_| Ok(poster.clone()));
        let (images, results) = Images::start(one_worker(None), fetch);
        let generation = Generation::default();
        let sized = |url: &str, width: Option<u32>| {
            let size = Rc::new(Cell::new(None));
            let slot = size.clone();
            let immediate = images.load(&generation, url, width, move |image| {
                slot.set(Some((image.size().width, image.size().height)));
            });
            assert!(immediate.is_none());
            size
        };

        // While it waits: two widths of one URL, as two rows a pixel or
        // two apart ask for the same rounded URL.
        let gate = sized("http://127.0.0.1/gate", None);
        server.started.recv().unwrap();
        let narrow = sized("http://127.0.0.1/poster", Some(20));
        let wider = sized("http://127.0.0.1/poster", Some(30));
        server.release.send(()).unwrap();
        pump(&images, &results, 3);
        assert_eq!(gate.get(), Some((40, 60)));
        assert_eq!(
            (narrow.get(), wider.get()),
            (Some((20, 30)), Some((30, 45)))
        );
        assert_eq!(server.fetched().len(), 2);

        // While it is fetched: the widths asked for meanwhile are made
        // from the same bytes, and one being made already is not asked
        // for again.
        let first = sized("http://127.0.0.1/cover/gate", Some(10));
        server.started.recv().unwrap();
        let second = sized("http://127.0.0.1/cover/gate", Some(16));
        let again = sized("http://127.0.0.1/cover/gate", Some(10));
        server.release.send(()).unwrap();
        pump(&images, &results, 2);
        assert_eq!(first.get(), Some((10, 15)));
        assert_eq!(again.get(), Some((10, 15)));
        assert_eq!(second.get(), Some((16, 24)));
        assert_eq!(server.fetched().len(), 3);
        std::thread::sleep(Duration::from_millis(50));
        assert!(results.is_empty());
    }

    #[test]
    fn the_workers_stop_when_the_cache_goes() {
        let (images, results) = Images::start(Options::default(), server_less());
        let calls = ask(&images, &Generation::default(), "http://127.0.0.1/missing");
        pump(&images, &results, 1);
        assert_eq!(calls.get(), 0);
        drop(images);
        // The results channel closes once the last worker has let go of it.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match results.try_recv() {
                Err(async_channel::TryRecvError::Closed) => break,
                _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
                other => panic!("the workers are still running: {:?}", other.is_ok()),
            }
        }
    }

    #[test]
    fn blurhashes_become_small_placeholders_of_the_right_shape() {
        // A blurhash made here of an image red on the left, blue on the
        // right.
        let (width, height) = (40u32, 60u32);
        let rgba: Vec<u8> = (0..width * height)
            .flat_map(|index| {
                if index % width < width / 2 {
                    [230, 20, 20, 255]
                } else {
                    [20, 20, 230, 255]
                }
            })
            .collect();
        let hash = blurhash::encode(4, 3, width, height, &rgba).unwrap();

        let buffer = placeholder_pixels(&hash, 200, 300).unwrap();
        assert_eq!((buffer.width(), buffer.height()), (32, 48));
        let [r, _, b, a] = pixel(&buffer, 2, 24);
        assert!(r > b && a == 255, "left is {r}, {b}");
        let [r, _, b, _] = pixel(&buffer, 29, 24);
        assert!(b > r, "right is {r}, {b}");

        let backdrop = placeholder(&hash, 1920, 1080).unwrap();
        assert_eq!((backdrop.size().width, backdrop.size().height), (32, 18));
        assert_eq!(placeholder(&hash, 0, 0).unwrap().size().height, 32);
        assert!(placeholder("not a hash", 200, 300).is_none());
        assert!(placeholder("", 200, 300).is_none());
        assert!(placeholder(&hash[..hash.len() - 2], 200, 300).is_none());
    }
}
