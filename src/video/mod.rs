//! Draws mpv's video into an OpenGL texture that Slint shows as an image.
//!
//! mpv renders into a framebuffer object we own, sized to the window, only
//! when it has a new frame or the size changed. Slint then composites that
//! texture under the interface like any other image, so interface-only
//! redraws (hovering a button while paused) never touch the video.

pub mod gl_loader;
pub mod software;

use crate::mpv::Mpv;
use crate::mpv::render::{NativeDisplay, RenderContext};
use glow::HasContext;
use slint::{ComponentHandle, GraphicsAPI, RenderingState};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Callbacks from the renderer to the window that owns it.
pub struct Hooks {
    /// The frame texture changed (first frame, or after a resize).
    pub set_frame: Box<dyn Fn(slint::Image)>,
    /// mpv's renderer exists; files can be opened from now on.
    pub ready: Box<dyn Fn()>,
    pub error: Box<dyn Fn(String)>,
}

struct Target {
    texture: glow::Texture,
    framebuffer: glow::Framebuffer,
    width: u32,
    height: u32,
}

struct Renderer {
    gl: glow::Context,
    context: RenderContext,
    target: Option<Target>,
    /// `PATINA_FRAME_DUMP`: a PPM file that receives a rendered frame about
    /// once a second, for diagnosing video output problems.
    dump: Option<(std::path::PathBuf, std::time::Instant)>,
}

impl Renderer {
    fn new(
        gl: glow::Context,
        mpv: Arc<Mpv>,
        display: NativeDisplay,
        redraw: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, String> {
        gl_loader::initialize()?;
        // Apple's software renderer (a Mac without a GPU, such as a virtual
        // machine) draws mpv's default half-float intermediate frames as
        // black; eight bits per channel work.
        // SAFETY: the context is current, and RENDERER is a valid query.
        let renderer = unsafe { gl.get_parameter_string(glow::RENDERER) };
        if renderer.contains("Apple Software Renderer")
            && let Err(error) =
                mpv.set_property_async(crate::player::REPLY_QUIET, "fbo-format", "rgba8")
        {
            log::warn!("{error}");
        }
        // SAFETY: called from Slint's RenderingSetup, where the window's GL
        // context is current; the resolver is process-wide and never unloads.
        let mut context =
            unsafe { RenderContext::new_opengl(mpv, gl_loader::get_proc_address, display) }
                .map_err(|error| error.to_string())?;
        context.set_update_callback(redraw);
        let dump = std::env::var_os("PATINA_FRAME_DUMP")
            .map(|path| (path.into(), std::time::Instant::now()));
        Ok(Self {
            gl,
            context,
            target: None,
            dump,
        })
    }

    /// Renders a frame if needed. Returns the new image when the texture was
    /// recreated.
    fn render(&mut self, width: u32, height: u32) -> Result<Option<slint::Image>, String> {
        // Always ask mpv for its update flags, which also acknowledges them.
        let frame_ready = self.context.frame_ready();
        if width == 0 || height == 0 {
            return Ok(None);
        }
        let mut image = None;
        if self
            .target
            .as_ref()
            .is_none_or(|t| t.width != width || t.height != height)
        {
            // Create the replacement first so it never reuses the texture name
            // of the image Slint is still holding.
            let target = self.create_target(width, height)?;
            self.delete_target();
            // SAFETY: the texture is a complete RGBA 2D texture of this context
            // that stays alive until the next resize or teardown, and Slint only
            // uses it on this context.
            image = Some(unsafe {
                slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(
                    target.texture.0,
                    (width, height).into(),
                )
                .origin(slint::BorrowedOpenGLTextureOrigin::TopLeft)
                .build()
            });
            self.target = Some(target);
        }

        if frame_ready || image.is_some() {
            let target = self.target.as_ref().expect("target exists");
            let saved = GlState::save(&self.gl);
            // SAFETY: the framebuffer is complete and belongs to the current
            // context.
            let result = unsafe {
                self.gl
                    .bind_framebuffer(glow::FRAMEBUFFER, Some(target.framebuffer));
                self.gl.viewport(0, 0, width as i32, height as i32);
                self.gl.disable(glow::SCISSOR_TEST);
                self.gl.disable(glow::BLEND);
                self.context.render(
                    target.framebuffer.0.get(),
                    width as i32,
                    height as i32,
                    false,
                )
            };
            if let Some((path, last)) = &mut self.dump
                && last.elapsed() >= std::time::Duration::from_secs(1)
            {
                *last = std::time::Instant::now();
                // mpv leaves its own framebuffers bound; read ours.
                // SAFETY: binds a framebuffer of the current context.
                unsafe {
                    self.gl
                        .bind_framebuffer(glow::FRAMEBUFFER, Some(target.framebuffer))
                };
                dump_frame(&self.gl, path, width, height);
            }
            saved.restore(&self.gl);
            result.map_err(|error| error.to_string())?;
        }
        Ok(image)
    }

    fn create_target(&self, width: u32, height: u32) -> Result<Target, String> {
        let gl = &self.gl;
        let saved = GlState::save(gl);
        // SAFETY: plain object creation on the current context; bindings are
        // restored afterwards.
        let result = unsafe {
            let texture = gl.create_texture()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            let internal_format = if gl.version().is_embedded && gl.version().major < 3 {
                glow::RGBA
            } else {
                glow::RGBA8
            };
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                internal_format as i32,
                width as i32,
                height as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            for (parameter, value) in [
                (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, parameter, value as i32);
            }
            let framebuffer = gl.create_framebuffer()?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture),
                0,
            );
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            if status == glow::FRAMEBUFFER_COMPLETE {
                Ok(Target {
                    texture,
                    framebuffer,
                    width,
                    height,
                })
            } else {
                gl.delete_framebuffer(framebuffer);
                gl.delete_texture(texture);
                Err(format!(
                    "video framebuffer is incomplete (status {status:#x})"
                ))
            }
        };
        saved.restore(gl);
        result
    }

    fn delete_target(&mut self) {
        if let Some(target) = self.target.take() {
            // SAFETY: both objects belong to the current context and are no
            // longer referenced by an image Slint will draw.
            unsafe {
                self.gl.delete_framebuffer(target.framebuffer);
                self.gl.delete_texture(target.texture);
            }
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.delete_target();
    }
}

/// Writes the bound framebuffer to a binary PPM file.
fn dump_frame(gl: &glow::Context, path: &std::path::Path, width: u32, height: u32) {
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    // SAFETY: reads the bound, complete framebuffer into a buffer of
    // exactly width × height RGBA pixels (RGBA rows need no row padding).
    unsafe {
        gl.read_pixels(
            0,
            0,
            width as i32,
            height as i32,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut pixels)),
        );
    }
    let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
    // Rows are stored top-down in the texture (TopLeft origin).
    for pixel in pixels.as_chunks::<4>().0 {
        ppm.extend_from_slice(&pixel[..3]);
    }
    if let Err(error) = std::fs::write(path, ppm) {
        log::warn!("could not write {}: {error}", path.display());
    }
}

/// The GL state mpv's renderer may change and Slint's renderer relies on.
struct GlState {
    framebuffer: i32,
    viewport: [i32; 4],
    scissor: bool,
    blend: bool,
    active_texture: i32,
    texture: i32,
    program: i32,
    vertex_array: i32,
    array_buffer: i32,
    unpack_alignment: i32,
}

impl GlState {
    fn save(gl: &glow::Context) -> Self {
        let mut viewport = [0; 4];
        // SAFETY: state queries on the current context.
        unsafe {
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            Self {
                framebuffer: gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING),
                viewport,
                scissor: gl.is_enabled(glow::SCISSOR_TEST),
                blend: gl.is_enabled(glow::BLEND),
                active_texture: gl.get_parameter_i32(glow::ACTIVE_TEXTURE),
                texture: gl.get_parameter_i32(glow::TEXTURE_BINDING_2D),
                program: gl.get_parameter_i32(glow::CURRENT_PROGRAM),
                vertex_array: gl.get_parameter_i32(glow::VERTEX_ARRAY_BINDING),
                array_buffer: gl.get_parameter_i32(glow::ARRAY_BUFFER_BINDING),
                unpack_alignment: gl.get_parameter_i32(glow::UNPACK_ALIGNMENT),
            }
        }
    }

    fn restore(&self, gl: &glow::Context) {
        fn object(id: i32) -> Option<NonZeroU32> {
            NonZeroU32::new(id as u32)
        }
        // SAFETY: restores object names queried from this same context.
        unsafe {
            gl.bind_framebuffer(
                glow::FRAMEBUFFER,
                object(self.framebuffer).map(glow::NativeFramebuffer),
            );
            let [x, y, w, h] = self.viewport;
            gl.viewport(x, y, w, h);
            for (capability, enabled) in [
                (glow::SCISSOR_TEST, self.scissor),
                (glow::BLEND, self.blend),
            ] {
                if enabled {
                    gl.enable(capability);
                } else {
                    gl.disable(capability);
                }
            }
            gl.active_texture(self.active_texture as u32);
            gl.bind_texture(
                glow::TEXTURE_2D,
                object(self.texture).map(glow::NativeTexture),
            );
            gl.use_program(object(self.program).map(glow::NativeProgram));
            gl.bind_vertex_array(object(self.vertex_array).map(glow::NativeVertexArray));
            gl.bind_buffer(
                glow::ARRAY_BUFFER,
                object(self.array_buffer).map(glow::NativeBuffer),
            );
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, self.unpack_alignment);
        }
    }
}

fn native_display(window: &slint::Window) -> NativeDisplay {
    use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
    use slint::winit_030::WinitWindowAccessor;
    window
        .with_winit_window(
            |winit| match winit.display_handle().map(|handle| handle.as_raw()) {
                Ok(RawDisplayHandle::Wayland(handle)) => {
                    NativeDisplay::Wayland(handle.display.as_ptr())
                }
                Ok(RawDisplayHandle::Xlib(handle)) => handle
                    .display
                    .map(|display| NativeDisplay::X11(display.as_ptr()))
                    .unwrap_or(NativeDisplay::None),
                _ => NativeDisplay::None,
            },
        )
        .unwrap_or(NativeDisplay::None)
}

/// Starts drawing `mpv`'s video in `component`'s window.
pub fn attach<C: ComponentHandle + 'static>(
    component: &C,
    mpv: Arc<Mpv>,
    hooks: Hooks,
) -> Result<(), slint::SetRenderingNotifierError> {
    let weak = component.as_weak();
    let mut renderer: Option<Renderer> = None;
    let redraw_pending = Arc::new(AtomicBool::new(false));

    component
        .window()
        .set_rendering_notifier(move |state, api| match state {
            RenderingState::RenderingSetup => {
                let GraphicsAPI::NativeOpenGL { get_proc_address } = api else {
                    (hooks.error)("Patina needs an OpenGL renderer to show video.".into());
                    return;
                };
                // SAFETY: Slint's loader is valid during this callback, and glow
                // resolves every function it needs right here.
                let gl = unsafe {
                    glow::Context::from_loader_function_cstr(|name| get_proc_address(name))
                };
                let Some(component) = weak.upgrade() else {
                    return;
                };
                let display = native_display(component.window());
                let redraw = {
                    let weak = weak.clone();
                    let pending = redraw_pending.clone();
                    move || {
                        if pending.swap(true, Ordering::AcqRel) {
                            return;
                        }
                        let weak = weak.clone();
                        let cleared = pending.clone();
                        let posted = slint::invoke_from_event_loop(move || {
                            cleared.store(false, Ordering::Release);
                            if let Some(component) = weak.upgrade() {
                                component.window().request_redraw();
                            }
                        });
                        if posted.is_err() {
                            pending.store(false, Ordering::Release);
                        }
                    }
                };
                match Renderer::new(gl, mpv.clone(), display, redraw) {
                    Ok(new) => {
                        renderer = Some(new);
                        (hooks.ready)();
                    }
                    Err(error) => (hooks.error)(format!("Could not start video output: {error}")),
                }
            }
            RenderingState::BeforeRendering => {
                let (Some(renderer), Some(component)) = (renderer.as_mut(), weak.upgrade()) else {
                    return;
                };
                let size = component.window().size();
                match renderer.render(size.width, size.height) {
                    Ok(Some(image)) => (hooks.set_frame)(image),
                    Ok(None) => {}
                    Err(error) => {
                        log::error!("{error}");
                        (hooks.error)(format!("Video output failed: {error}"));
                    }
                }
            }
            RenderingState::AfterRendering => {
                if let Some(renderer) = &renderer {
                    renderer.context.report_swap();
                }
            }
            RenderingState::RenderingTeardown => {
                log::debug!("video renderer torn down");
                renderer = None;
            }
            _ => {}
        })
}
