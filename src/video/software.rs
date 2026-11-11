//! Video without OpenGL, for a window drawn by Slint's software renderer: a
//! machine without a GPU driver, a virtual machine, a remote desktop.
//!
//! mpv's software renderer runs on a thread of its own and draws each frame
//! into a pixel buffer the size of the window; the buffer reaches the window
//! as an ordinary image. It costs CPU that the OpenGL path doesn't, which is
//! why it is only the fallback.

use super::Hooks;
use crate::mpv::Mpv;
use crate::mpv::render::RenderContext;
use slint::{Rgba8Pixel, SharedPixelBuffer};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;

/// Whether Patina was asked to draw without OpenGL: by `PATINA_RENDERER`,
/// which it also sets itself when it relaunches after OpenGL failed. A window
/// can be drawn without OpenGL even so, when Slint found none to use.
pub fn enabled() -> bool {
    std::env::var_os("PATINA_RENDERER").is_some_and(|renderer| renderer == "software")
}

/// Why Patina ended its event loop to start again without OpenGL.
static RESTART: OnceLock<String> = OnceLock::new();

/// Ends the event loop, for Patina to start again without OpenGL: OpenGL
/// turned out unusable once the loop was running, and Slint chooses its
/// renderer once per process.
pub fn restart(reason: String) {
    if RESTART.set(reason).is_ok() {
        let _ = slint::quit_event_loop();
    }
}

/// Why [`restart`] ended the event loop, if it did.
pub fn restart_reason() -> Option<&'static str> {
    RESTART.get().map(String::as_str)
}

#[derive(Default)]
struct State {
    width: u32,
    height: u32,
    /// Render again even without a new frame: the window was resized.
    resized: bool,
    wake: bool,
    stop: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

impl Shared {
    fn update(&self, change: impl FnOnce(&mut State)) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        change(&mut state);
        state.wake = true;
        self.wake.notify_one();
    }
}

enum Message {
    Ready,
    Frame(SharedPixelBuffer<Rgba8Pixel>),
    Error(String),
}

/// The render thread. Dropping it stops the thread and frees mpv's render
/// context, which has to happen before the core is destroyed.
pub struct SoftwareVideo {
    shared: Arc<Shared>,
    /// The thread's end of the frame queue, kept so that dropping can close
    /// it: see the `Drop` impl.
    sender: async_channel::Sender<Message>,
    thread: Option<JoinHandle<()>>,
}

impl SoftwareVideo {
    pub fn start(mpv: Arc<Mpv>, width: u32, height: u32, hooks: Hooks) -> Result<Self, String> {
        let (video, receiver) = Self::spawn(mpv, width, height)?;
        slint::spawn_local(async move {
            while let Ok(message) = receiver.recv().await {
                match message {
                    Message::Ready => (hooks.ready)(),
                    Message::Frame(buffer) => (hooks.set_frame)(slint::Image::from_rgba8(buffer)),
                    Message::Error(error) => (hooks.error)(error),
                }
            }
        })
        .map_err(|error| error.to_string())?;
        Ok(video)
    }

    /// Starts the render thread, which sends what it has for the window to
    /// the receiver returned.
    fn spawn(
        mpv: Arc<Mpv>,
        width: u32,
        height: u32,
    ) -> Result<(Self, async_channel::Receiver<Message>), String> {
        let shared = Arc::new(Shared::default());
        shared.update(|state| {
            state.width = width;
            state.height = height;
        });
        // Bounded, so the thread draws no faster than the window shows.
        let (sender, receiver) = async_channel::bounded::<Message>(2);
        let thread_shared = shared.clone();
        let thread_sender = sender.clone();
        let thread = std::thread::Builder::new()
            .name("video".into())
            .spawn(move || run(&mpv, &thread_shared, &thread_sender))
            .map_err(|error| format!("could not start the video thread: {error}"))?;
        let video = Self {
            shared,
            sender,
            thread: Some(thread),
        };
        Ok((video, receiver))
    }

    /// The window's size in physical pixels changed.
    pub fn resize(&self, width: u32, height: u32) {
        self.shared.update(|state| {
            if (state.width, state.height) != (width, height) {
                state.width = width;
                state.height = height;
                state.resized = true;
            }
        });
    }
}

impl Drop for SoftwareVideo {
    fn drop(&mut self) {
        self.shared.update(|state| state.stop = true);
        // The thread may be waiting to send a frame into a full queue, which
        // only the window's task empties, on this thread, which is about to
        // wait for it: closing the queue ends that send, and the thread with
        // it.
        self.sender.close();
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            log::error!("the video thread panicked");
        }
    }
}

fn run(mpv: &Arc<Mpv>, shared: &Arc<Shared>, sender: &async_channel::Sender<Message>) {
    let mut context = match RenderContext::new_software(mpv.clone()) {
        Ok(context) => context,
        Err(error) => {
            let _ = sender.send_blocking(Message::Error(format!(
                "Could not start video output: {error}"
            )));
            return;
        }
    };
    {
        let shared = shared.clone();
        context.set_update_callback(move || shared.update(|_| {}));
    }
    if sender.send_blocking(Message::Ready).is_err() {
        return;
    }
    let mut dump = super::FrameDump::from_env();
    loop {
        let (width, height, resized) = {
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            while !state.wake && !state.stop {
                state = shared
                    .wake
                    .wait(state)
                    .unwrap_or_else(|error| error.into_inner());
            }
            if state.stop {
                break;
            }
            state.wake = false;
            (
                state.width,
                state.height,
                std::mem::take(&mut state.resized),
            )
        };
        // Also acknowledges mpv's update, which it waits for before the next.
        let frame = context.frame_ready();
        if !(frame || resized) || width == 0 || height == 0 {
            continue;
        }
        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
        if let Err(error) = context.render_software(width, height, buffer.make_mut_bytes()) {
            log::warn!("{error}");
            continue;
        }
        // mpv writes "rgb0": the fourth byte is padding, not opacity.
        for pixel in buffer.make_mut_slice() {
            pixel.a = 255;
        }
        if let Some(dump) = &mut dump
            && dump.due()
        {
            dump.write(width, height, buffer.as_bytes());
        }
        if sender.send_blocking(Message::Frame(buffer)).is_err() {
            break;
        }
    }
    // The context goes before the thread does, while the core is alive: it
    // holds a reference to it.
    drop(context);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn dropping_ends_a_thread_waiting_for_room_in_the_frame_queue() {
        let builder = match Mpv::builder() {
            Ok(builder) => builder,
            Err(error) => {
                crate::mpv::testing::skip("PATINA_TEST_REQUIRE_MPV", &error);
                return;
            }
        };
        for (name, value) in [
            ("vo", "libmpv"),
            ("ao", "null"),
            ("idle", "yes"),
            ("config", "no"),
            ("terminal", "no"),
        ] {
            builder.set_option(name, value).unwrap();
        }
        let mpv = Arc::new(builder.initialize().unwrap());
        // The render context comes first, as in the window: mpv's video output
        // fails to start without one, and the file then plays with no video.
        let (video, receiver) = SoftwareVideo::spawn(mpv.clone(), 64, 36).unwrap();
        assert!(matches!(receiver.recv_blocking(), Ok(Message::Ready)));
        mpv.command(&["loadfile", "av://lavfi:testsrc2=size=64x36:rate=60"])
            .unwrap();
        // Nothing receives, as when the window's task can't run because the
        // interface thread is busy closing windows: the queue fills, and the
        // thread waits to send the next frame.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !receiver.is_full() {
            assert!(Instant::now() < deadline, "the frame queue never filled");
            std::thread::sleep(Duration::from_millis(10));
        }
        // Time for the thread to draw one more frame and wait with it.
        std::thread::sleep(Duration::from_millis(200));

        let (dropped, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(video);
            let _ = dropped.send(());
        });
        assert!(
            done.recv_timeout(Duration::from_secs(5)).is_ok(),
            "dropping the video waited forever for its thread"
        );
        drop(receiver);
        let _ = mpv.command(&["quit"]);
    }
}
