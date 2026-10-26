//! mpv's OpenGL render API.
//!
//! The render context belongs to the thread whose OpenGL context was current
//! when it was created; every method here must be called on that thread with
//! that context current.

use super::{Mpv, Result, check, ffi};
use std::ffi::{c_int, c_void};
use std::ptr::{self, NonNull};
use std::sync::Arc;

/// Platform display connection that lets mpv share it for hardware decoding.
#[derive(Debug, Clone, Copy)]
pub enum NativeDisplay {
    None,
    X11(*mut c_void),
    Wayland(*mut c_void),
}

type UpdateCallback = Box<dyn Fn() + Send + Sync>;

pub struct RenderContext {
    raw: NonNull<ffi::mpv_render_context>,
    update: Option<Box<UpdateCallback>>,
    mpv: Arc<Mpv>,
}

impl RenderContext {
    /// Creates an OpenGL render context for `mpv`.
    ///
    /// # Safety
    /// An OpenGL context must be current on this thread, and must remain the
    /// current context whenever this render context is used or dropped.
    /// `get_proc_address` must resolve functions of that context for as long
    /// as the render context exists.
    pub unsafe fn new_opengl(
        mpv: Arc<Mpv>,
        get_proc_address: ffi::GetProcAddress,
        display: NativeDisplay,
    ) -> Result<Self> {
        let api = mpv.api();
        let mut init = ffi::mpv_opengl_init_params {
            get_proc_address: Some(get_proc_address),
            get_proc_address_ctx: ptr::null_mut(),
        };
        let mut advanced_control: c_int = 0;
        let mut params = vec![
            ffi::mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_API_TYPE,
                data: ffi::MPV_RENDER_API_TYPE_OPENGL.as_ptr() as *mut c_void,
            },
            ffi::mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: (&raw mut init).cast(),
            },
            ffi::mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_ADVANCED_CONTROL,
                data: (&raw mut advanced_control).cast(),
            },
        ];
        match display {
            NativeDisplay::X11(display) => params.push(ffi::mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_X11_DISPLAY,
                data: display,
            }),
            NativeDisplay::Wayland(display) => params.push(ffi::mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_WL_DISPLAY,
                data: display,
            }),
            NativeDisplay::None => {}
        }
        params.push(ffi::mpv_render_param {
            type_: ffi::MPV_RENDER_PARAM_INVALID,
            data: ptr::null_mut(),
        });

        let mut raw = ptr::null_mut();
        // SAFETY: the parameter list is terminated and every pointee outlives
        // the call; the caller guarantees a current GL context.
        let code =
            unsafe { (api.mpv_render_context_create)(&mut raw, mpv.raw(), params.as_mut_ptr()) };
        check(code, || "creating the mpv OpenGL renderer".to_owned())?;
        let raw = NonNull::new(raw)
            .ok_or_else(|| super::Error::new(-1, "mpv returned no render context"))?;
        Ok(Self {
            raw,
            update: None,
            mpv,
        })
    }

    /// Calls `callback` whenever mpv has a new frame or wants a redraw.
    ///
    /// The callback runs on an mpv thread and must not call into mpv.
    pub fn set_update_callback(&mut self, callback: impl Fn() + Send + Sync + 'static) {
        let callback: Box<UpdateCallback> = Box::new(Box::new(callback));
        let data = (&*callback as *const UpdateCallback).cast_mut().cast();
        // SAFETY: `data` stays valid until the callback is replaced or removed
        // in Drop, which happens before `self.update` is dropped.
        unsafe {
            (self.mpv.api().mpv_render_context_set_update_callback)(
                self.raw.as_ptr(),
                Some(trampoline),
                data,
            );
        }
        self.update = Some(callback);
    }

    /// Returns true when a new video frame is ready to be rendered.
    pub fn frame_ready(&self) -> bool {
        // SAFETY: valid context, called on its GL thread.
        let flags = unsafe { (self.mpv.api().mpv_render_context_update)(self.raw.as_ptr()) };
        flags & ffi::MPV_RENDER_UPDATE_FRAME != 0
    }

    /// Renders the current frame into framebuffer `fbo`, sized `width`×`height`.
    ///
    /// # Safety
    /// `fbo` must be a complete framebuffer of the current GL context.
    pub unsafe fn render(&self, fbo: u32, width: i32, height: i32, flip_y: bool) -> Result<()> {
        let mut target = ffi::mpv_opengl_fbo {
            fbo: fbo as c_int,
            w: width,
            h: height,
            internal_format: 0,
        };
        let mut flip: c_int = c_int::from(flip_y);
        // Never block the UI thread waiting for a frame's display time; frames
        // are already requested at the right moment by the update callback.
        let mut block: c_int = 0;
        let mut params = [
            ffi::mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_OPENGL_FBO,
                data: (&raw mut target).cast(),
            },
            ffi::mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_FLIP_Y,
                data: (&raw mut flip).cast(),
            },
            ffi::mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME,
                data: (&raw mut block).cast(),
            },
            ffi::mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];
        // SAFETY: valid context and terminated parameter list; the caller
        // guarantees the framebuffer.
        let code = unsafe {
            (self.mpv.api().mpv_render_context_render)(self.raw.as_ptr(), params.as_mut_ptr())
        };
        check(code, || "rendering video".to_owned())
    }

    /// Tells mpv a frame was presented, for its display-sync timing.
    pub fn report_swap(&self) {
        // SAFETY: valid context.
        unsafe { (self.mpv.api().mpv_render_context_report_swap)(self.raw.as_ptr()) }
    }
}

impl Drop for RenderContext {
    fn drop(&mut self) {
        let api = self.mpv.api();
        // SAFETY: the context is valid and (per `new_opengl`'s contract) its GL
        // context is current. The callback is cleared before its data is freed.
        unsafe {
            (api.mpv_render_context_set_update_callback)(self.raw.as_ptr(), None, ptr::null_mut());
            (api.mpv_render_context_free)(self.raw.as_ptr());
        }
    }
}

unsafe extern "C" fn trampoline(data: *mut c_void) {
    if data.is_null() {
        return;
    }
    // SAFETY: `data` is the boxed callback registered in set_update_callback,
    // alive until the callback is unregistered. Panics must not unwind into C.
    let callback = unsafe { &*data.cast::<UpdateCallback>() };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(callback));
}
