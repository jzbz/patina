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
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

/// Whether this process draws without OpenGL: set by `PATINA_RENDERER`,
/// which Patina also sets itself when it relaunches after OpenGL failed.
pub fn enabled() -> bool {
    std::env::var_os("PATINA_RENDERER").is_some_and(|renderer| renderer == "software")
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
    thread: Option<JoinHandle<()>>,
}

impl SoftwareVideo {
    pub fn start(mpv: Arc<Mpv>, width: u32, height: u32, hooks: Hooks) -> Result<Self, String> {
        let shared = Arc::new(Shared::default());
        shared.update(|state| {
            state.width = width;
            state.height = height;
        });
        // Bounded, so the thread draws no faster than the window shows.
        let (sender, receiver) = async_channel::bounded::<Message>(2);
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("video".into())
            .spawn(move || run(&mpv, &thread_shared, &sender))
            .map_err(|error| format!("could not start the video thread: {error}"))?;
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
        Ok(Self {
            shared,
            thread: Some(thread),
        })
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
        if sender.send_blocking(Message::Frame(buffer)).is_err() {
            break;
        }
    }
    // The context goes before the thread does, while the core is alive: it
    // holds a reference to it.
    drop(context);
}
