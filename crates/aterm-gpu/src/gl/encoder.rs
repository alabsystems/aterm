// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Frame encoding on the GL arm: the verbs `device_layer::FrameEncoder` and `FramePass` forward to.

use super::device::{Current, GlDevice};
use super::ffi::{self, GLenum};
use super::pipelines::{GlPipeline, specify_instance_layout};
use super::resources::{GlBuffer, GlSampler, GlTexture};
use crate::device_layer::FrameLoad;
use crate::pipeline_table::VertexLayoutSpec;

/// One frame's encode on the GL arm: the context, current for the frame.
pub(crate) struct GlFrame<'s> {
    dev: &'s GlDevice,
    cur: Current<'s>,
}

/// The state a pass tracks to refuse a malformed draw.
#[derive(Clone, Copy)]
struct Bound {
    mode: GLenum,
    layout: Option<VertexLayoutSpec>,
    stream: bool,
    label: &'static str,
}

/// One open pass.
pub(crate) struct GlPass<'e, 's> {
    frame: &'e mut GlFrame<'s>,
    bound: Option<Bound>,
}

impl<'s> GlFrame<'s> {
    /// Hold `dev`'s context current for one frame.
    pub(crate) fn begin(dev: &'s GlDevice) -> Result<Self, String> {
        Ok(Self {
            dev,
            cur: dev.enter()?,
        })
    }

    /// Open a pass over `target`.
    pub(crate) fn begin_pass<'e>(
        &'e mut self,
        target: &GlTexture,
        srgb: bool,
        load: FrameLoad,
        scissor: Option<(u32, u32, u32, u32)>,
        uniforms: (&GlBuffer, u32),
    ) -> Result<GlPass<'e, 's>, String> {
        let fbo = target.fbo(&self.cur)?;
        let gl = self.cur.gl();
        let encode = if target.views { srgb } else { true };
        // SAFETY: the context is current (the frame's guard); the framebuffer and buffer names
        // belong to it; the clear colour is four floats.
        unsafe {
            (gl.bind_framebuffer)(ffi::GL_FRAMEBUFFER, fbo);
            (gl.viewport)(0, 0, target.width as i32, target.height as i32);
            if self.dev.caps().srgb_views {
                if encode {
                    (gl.enable)(ffi::GL_FRAMEBUFFER_SRGB_EXT);
                } else {
                    (gl.disable)(ffi::GL_FRAMEBUFFER_SRGB_EXT);
                }
            }
            if let FrameLoad::Clear(c) = load {
                // A clear ignores the viewport: one of a roomy texture keeps to its texels in use.
                if target.roomy() {
                    (gl.enable)(ffi::GL_SCISSOR_TEST);
                    (gl.scissor)(0, 0, target.width as i32, target.height as i32);
                } else {
                    (gl.disable)(ffi::GL_SCISSOR_TEST);
                }
                (gl.color_mask)(ffi::GL_TRUE, ffi::GL_TRUE, ffi::GL_TRUE, ffi::GL_TRUE);
                let rgba = [c.r as f32, c.g as f32, c.b as f32, c.a as f32];
                (gl.clear_bufferfv)(ffi::GL_COLOR, 0, rgba.as_ptr());
            }
            match scissor {
                Some((x, y, w, h)) => {
                    (gl.enable)(ffi::GL_SCISSOR_TEST);
                    (gl.scissor)(x as i32, y as i32, w as i32, h as i32);
                }
                None => (gl.disable)(ffi::GL_SCISSOR_TEST),
            }
            (gl.bind_buffer_base)(ffi::GL_UNIFORM_BUFFER, uniforms.1, uniforms.0.name);
        }
        self.cur.check("begin pass")?;
        Ok(GlPass {
            frame: self,
            bound: None,
        })
    }

    /// Copy a `width` x `height` rect of `src` at `src_origin` into `dst` at `dst_origin`.
    pub(crate) fn copy_rect(
        &mut self,
        src: &GlTexture,
        src_origin: (u32, u32),
        dst: &GlTexture,
        dst_origin: (u32, u32),
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        let inside = |t: &GlTexture, (x, y): (u32, u32)| {
            x.checked_add(width).is_some_and(|e| e <= t.width)
                && y.checked_add(height).is_some_and(|e| e <= t.height)
        };
        if !inside(src, src_origin) || !inside(dst, dst_origin) {
            return Err(format!(
                "gl: copy of {width}x{height} from {src_origin:?} in {}x{} to {dst_origin:?} in \
                 {}x{} leaves a texture",
                src.width, src.height, dst.width, dst.height
            ));
        }
        if src.name == dst.name {
            let apart = |a: u32, b: u32, len: u32| a + len <= b || b + len <= a;
            if !(apart(src_origin.0, dst_origin.0, width)
                || apart(src_origin.1, dst_origin.1, height))
            {
                return Err("gl: an overlapping copy within one texture".to_owned());
            }
        }
        if width == 0 || height == 0 {
            return Ok(());
        }
        let fbo = src.fbo(&self.cur)?;
        let gl = self.cur.gl();
        // SAFETY: the context is current; both rects checked inside their textures; the source
        // framebuffer is complete.
        unsafe {
            (gl.bind_framebuffer)(ffi::GL_READ_FRAMEBUFFER, fbo);
            (gl.bind_texture)(ffi::GL_TEXTURE_2D, dst.name);
            (gl.copy_tex_sub_image_2d)(
                ffi::GL_TEXTURE_2D,
                0,
                dst_origin.0 as i32,
                dst_origin.1 as i32,
                src_origin.0 as i32,
                src_origin.1 as i32,
                width as i32,
                height as i32,
            );
        }
        self.cur.check("texture copy")
    }

    /// End the frame: flush its commands to the driver.
    pub(crate) fn submit(self) -> Result<(), String> {
        // SAFETY: the context is current.
        unsafe { (self.cur.gl().flush)() };
        self.cur.check("submit")
    }
}

impl GlPass<'_, '_> {
    /// Bind `buf` as the uniform block at `binding` for the pass's later draws.
    #[inline]
    pub(crate) fn bind_uniform(&mut self, buf: &GlBuffer, binding: u32) {
        // SAFETY: the context is current; the buffer belongs to it.
        unsafe {
            (self.frame.cur.gl().bind_buffer_base)(ffi::GL_UNIFORM_BUFFER, binding, buf.name);
        }
    }

    /// Scissor the pass's later draws to `(x, y, w, h)`, or lift the scissor.
    #[inline]
    pub(crate) fn set_scissor(&mut self, scissor: Option<(u32, u32, u32, u32)>) {
        let gl = self.frame.cur.gl();
        // SAFETY: the context is current; valid capability and extents.
        unsafe {
            match scissor {
                Some((x, y, w, h)) => {
                    (gl.enable)(ffi::GL_SCISSOR_TEST);
                    (gl.scissor)(x as i32, y as i32, w as i32, h as i32);
                }
                None => (gl.disable)(ffi::GL_SCISSOR_TEST),
            }
        }
    }

    /// Switch to `pipe`: program, vertex array, blend and write mask.
    #[inline]
    pub(crate) fn set_pipeline(&mut self, pipe: &GlPipeline) {
        let gl = self.frame.cur.gl();
        let m = |b: bool| if b { ffi::GL_TRUE } else { ffi::GL_FALSE };
        // SAFETY: the context is current; the pipeline's names belong to it.
        unsafe {
            (gl.use_program)(pipe.program);
            (gl.bind_vertex_array)(pipe.vao);
            match pipe.blend {
                None => (gl.disable)(ffi::GL_BLEND),
                Some([src_rgb, dst_rgb, src_a, dst_a, op_rgb, op_a]) => {
                    (gl.enable)(ffi::GL_BLEND);
                    (gl.blend_func_separate)(src_rgb, dst_rgb, src_a, dst_a);
                    (gl.blend_equation_separate)(op_rgb, op_a);
                }
            }
            (gl.color_mask)(
                m(pipe.mask[0]),
                m(pipe.mask[1]),
                m(pipe.mask[2]),
                m(pipe.mask[3]),
            );
        }
        self.bound = Some(Bound {
            mode: pipe.mode,
            layout: pipe.layout,
            stream: false,
            label: pipe.label,
        });
    }

    /// Bind `tex` with `sampler` at texture unit `unit`.
    #[inline]
    pub(crate) fn bind_texture(
        &mut self,
        tex: &GlTexture,
        sampler: &GlSampler,
        unit: u32,
        raw: bool,
    ) {
        let gl = self.frame.cur.gl();
        // SAFETY: the context is current; names belong to it; unit < the GLES 3.0 minimum of 16
        // (BindSpec slots are 0).
        unsafe {
            (gl.active_texture)(ffi::GL_TEXTURE0 + unit);
            (gl.bind_texture)(ffi::GL_TEXTURE_2D, tex.name);
            (gl.bind_sampler)(unit, sampler.name);
            if tex.views {
                let decode = if raw {
                    ffi::GL_SKIP_DECODE_EXT
                } else {
                    ffi::GL_DECODE_EXT
                };
                (gl.sampler_parameteri)(sampler.name, ffi::GL_TEXTURE_SRGB_DECODE_EXT, decode);
            }
        }
    }

    /// Bind the per-draw instance stream into the current pipeline's vertex array.
    #[inline]
    pub(crate) fn set_stream(&mut self, buf: &GlBuffer) {
        let Some(bound) = self.bound.as_mut() else {
            return;
        };
        let Some(layout) = bound.layout else {
            return;
        };
        bound.stream = true;
        let cur = &self.frame.cur;
        // SAFETY: the context is current; the pipeline's vertex array is bound (set_pipeline) and
        // the stream buffer is bound just below.
        unsafe {
            (cur.gl().bind_buffer)(ffi::GL_ARRAY_BUFFER, buf.name);
            specify_instance_layout(cur, &layout);
        }
    }

    /// Draw `instances` instances of `vertices` vertices with the current pipeline.
    #[inline]
    pub(crate) fn draw(&mut self, vertices: u32, instances: u32) -> Result<(), String> {
        let bound = self.bound.ok_or("gl: a draw with no pipeline")?;
        if bound.layout.is_some() && !bound.stream {
            return Err(format!(
                "gl: a draw with {} and no instance stream bound",
                bound.label
            ));
        }
        let gl = self.frame.cur.gl();
        // SAFETY: the context is current; program, vertex array and (for a streamed row) the
        // instance attributes are specified.
        unsafe { (gl.draw_arrays_instanced)(bound.mode, 0, vertices as i32, instances as i32) };
        Ok(())
    }
}
