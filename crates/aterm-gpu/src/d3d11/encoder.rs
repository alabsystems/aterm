// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Frame encoding on the D3D11 arm: the verbs `device_layer::FrameEncoder`
//! and `FramePass` forward to.
//!
//! The immediate context records as it is called, so a [`D3dFrame`] is the
//! context held for one frame's encode and `submit` is a flush. The pass
//! vocabulary is wgpu's, with wgpu-dx12's semantics, so
//! `renderer.rs::run_frame_plan` drives this arm unchanged:
//!
//! * a pass attaches one render-target view; `Clear` writes every texel (D3D11's
//!   clear ignores the scissor and the write mask), then the scissor applies to
//!   draws, the whole target when the pass names none;
//! * a pass unbinds the pixel stage's textures first, so a texture the last
//!   pass sampled can be this pass's target (D3D11 would unbind it with a
//!   warning, and the oracle's barriers order it the same way);
//! * a pipeline switch sets shaders, input layout, topology, blend and
//!   rasterizer state;
//! * a draw refuses without a pipeline, and without a stream on a row that
//!   reads one, instead of drawing garbage;
//! * a copy is `CopySubresourceRegion`, and refuses an overlapping
//!   same-texture copy.

use aterm_win32::Win32::Foundation::RECT;
use aterm_win32::d3d11 as d3d;

use super::device::{Current, D3dDevice};
use super::pipelines::D3dPipeline;
use super::resources::{D3dBuffer, D3dSampler, D3dTexture};
use crate::device_layer::FrameLoad;
use crate::pipeline_table::VertexLayoutSpec;

/// The pixel-stage texture slots a pass clears before it binds its target:
/// every slot a [`crate::pipeline_table::BindSpec`] names (0..=2).
const PS_SLOTS: usize = 4;

/// One frame's encode on the D3D11 arm: the context, held for the frame.
pub(crate) struct D3dFrame<'s> {
    cur: Current<'s>,
}

/// The state a pass tracks to refuse a malformed draw.
#[derive(Clone, Copy)]
struct Bound {
    layout: Option<VertexLayoutSpec>,
    stream: bool,
    label: &'static str,
}

/// One open pass.
pub(crate) struct D3dPass<'e, 's> {
    frame: &'e mut D3dFrame<'s>,
    bound: Option<Bound>,
    /// The target's size, the scissor a pass with none takes.
    target: (u32, u32),
}

/// A scissor rect, `None` being the whole `target`.
#[inline]
fn scissor_rect(scissor: Option<(u32, u32, u32, u32)>, target: (u32, u32)) -> RECT {
    let (x, y, w, h) = scissor.unwrap_or((0, 0, target.0, target.1));
    RECT {
        left: x as i32,
        top: y as i32,
        right: x.saturating_add(w) as i32,
        bottom: y.saturating_add(h) as i32,
    }
}

impl<'s> D3dFrame<'s> {
    /// Hold `dev`'s context for one frame.
    pub(crate) fn begin(dev: &'s D3dDevice) -> Result<Self, String> {
        Ok(Self { cur: dev.enter()? })
    }

    /// Open a pass over `target`. `srgb` picks the view on a texture with
    /// both ([`D3dTexture::views`]): the sRGB view encodes on write and blends
    /// in linear light, the Unorm view writes raw bytes.
    pub(crate) fn begin_pass<'e>(
        &'e mut self,
        target: &D3dTexture,
        srgb: bool,
        load: FrameLoad,
        scissor: Option<(u32, u32, u32, u32)>,
        uniforms: (&D3dBuffer, u32),
    ) -> Result<D3dPass<'e, 's>, String> {
        let ctx = self.cur.ctx();
        let rtv = target.rtv(srgb).raw();
        let none = [std::ptr::null_mut(); PS_SLOTS];
        let viewport = d3d::D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: target.width as f32,
            Height: target.height as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        let size = (target.width, target.height);
        let rect = scissor_rect(scissor, size);
        // SAFETY: the context is held (the frame's guard); the view, buffer and
        // arrays are live for the calls; the clear colour is four floats.
        unsafe {
            (ctx.vt.PSSetShaderResources)(ctx.raw, 0, PS_SLOTS as u32, none.as_ptr());
            (ctx.vt.OMSetRenderTargets)(ctx.raw, 1, &rtv, std::ptr::null_mut());
            (ctx.vt.RSSetViewports)(ctx.raw, 1, &viewport);
            if let FrameLoad::Clear(c) = load {
                let rgba = [c.r as f32, c.g as f32, c.b as f32, c.a as f32];
                (ctx.vt.ClearRenderTargetView)(ctx.raw, rtv, rgba.as_ptr());
            }
            (ctx.vt.RSSetScissorRects)(ctx.raw, 1, &rect);
        }
        let mut pass = D3dPass {
            frame: self,
            bound: None,
            target: size,
        };
        pass.bind_uniform(uniforms.0, uniforms.1);
        Ok(pass)
    }

    /// Copy a `width` x `height` rect of `src` at `src_origin` into `dst` at
    /// `dst_origin`. Both rects must lie inside their textures, and a copy
    /// within one texture must not overlap itself.
    pub(crate) fn copy_rect(
        &mut self,
        src: &D3dTexture,
        src_origin: (u32, u32),
        dst: &D3dTexture,
        dst_origin: (u32, u32),
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        let inside = |t: &D3dTexture, (x, y): (u32, u32)| {
            x.checked_add(width).is_some_and(|e| e <= t.width)
                && y.checked_add(height).is_some_and(|e| e <= t.height)
        };
        if !inside(src, src_origin) || !inside(dst, dst_origin) {
            return Err(format!(
                "d3d11: copy of {width}x{height} from {src_origin:?} in {}x{} to {dst_origin:?} \
                 in {}x{} leaves a texture",
                src.width, src.height, dst.width, dst.height
            ));
        }
        if src.tex.raw() == dst.tex.raw() {
            let apart = |a: u32, b: u32, len: u32| a + len <= b || b + len <= a;
            if !(apart(src_origin.0, dst_origin.0, width)
                || apart(src_origin.1, dst_origin.1, height))
            {
                return Err("d3d11: an overlapping copy within one texture".to_owned());
            }
        }
        if width == 0 || height == 0 {
            return Ok(());
        }
        let region = d3d::D3D11_BOX {
            left: src_origin.0,
            top: src_origin.1,
            front: 0,
            right: src_origin.0 + width,
            bottom: src_origin.1 + height,
            back: 1,
        };
        let ctx = self.cur.ctx();
        // SAFETY: the context is held; both rects checked inside their
        // textures, which are of one format family (the renderer copies
        // between offscreens of one format).
        unsafe {
            (ctx.vt.CopySubresourceRegion)(
                ctx.raw,
                dst.tex.raw(),
                0,
                dst_origin.0,
                dst_origin.1,
                0,
                src.tex.raw(),
                0,
                &region,
            );
        }
        Ok(())
    }

    /// End the frame: flush its commands to the driver, and report a removed
    /// device. The context is released when the frame drops.
    pub(crate) fn submit(self) -> Result<(), String> {
        let ctx = self.cur.ctx();
        // SAFETY: the context is held.
        unsafe { (ctx.vt.Flush)(ctx.raw) };
        self.cur.check("submit")
    }
}

impl D3dPass<'_, '_> {
    /// Bind `buf` as the constant buffer at `binding` for both stages, for
    /// the pass's later draws (a present's crown binds its own inside the
    /// blit's pass).
    #[inline]
    pub(crate) fn bind_uniform(&mut self, buf: &D3dBuffer, binding: u32) {
        let ctx = self.frame.cur.ctx();
        let b = buf.buf.raw();
        // SAFETY: the context is held; a constant buffer of this device; the
        // slot is a BindSpec slot (< 14).
        unsafe {
            (ctx.vt.VSSetConstantBuffers)(ctx.raw, binding, 1, &b);
            (ctx.vt.PSSetConstantBuffers)(ctx.raw, binding, 1, &b);
        }
    }

    /// Narrow (or, with `None`, widen back to the whole target) the scissor
    /// for the pass's later draws (a present's crown, inside the blit's pass).
    #[inline]
    pub(crate) fn set_scissor(&mut self, scissor: Option<(u32, u32, u32, u32)>) {
        let ctx = self.frame.cur.ctx();
        let rect = scissor_rect(scissor, self.target);
        // SAFETY: the context is held; one rect.
        unsafe { (ctx.vt.RSSetScissorRects)(ctx.raw, 1, &rect) };
    }

    /// Switch to `pipe`: shaders, input layout, topology, blend and
    /// rasterizer state.
    #[inline]
    pub(crate) fn set_pipeline(&mut self, pipe: &D3dPipeline) {
        let ctx = self.frame.cur.ctx();
        let blend_factor = [0.0f32; 4];
        // SAFETY: the context is held; the pipeline's objects belong to this
        // device; no class instances.
        unsafe {
            (ctx.vt.VSSetShader)(ctx.raw, pipe.vs.raw(), std::ptr::null(), 0);
            (ctx.vt.PSSetShader)(ctx.raw, pipe.ps.raw(), std::ptr::null(), 0);
            (ctx.vt.IASetInputLayout)(
                ctx.raw,
                pipe.input
                    .as_ref()
                    .map_or(std::ptr::null_mut(), |i| i.raw()),
            );
            (ctx.vt.IASetPrimitiveTopology)(ctx.raw, pipe.topology);
            (ctx.vt.OMSetBlendState)(ctx.raw, pipe.blend.raw(), blend_factor.as_ptr(), u32::MAX);
            (ctx.vt.RSSetState)(ctx.raw, pipe.raster.raw());
        }
        self.bound = Some(Bound {
            layout: pipe.layout,
            stream: false,
            label: pipe.label,
        });
    }

    /// Bind `tex` with `sampler` at slot `unit`. `raw` samples a texture with
    /// views through its Unorm view (no decode).
    #[inline]
    pub(crate) fn bind_texture(
        &mut self,
        tex: &D3dTexture,
        sampler: &D3dSampler,
        unit: u32,
        raw: bool,
    ) {
        let ctx = self.frame.cur.ctx();
        let srv = tex.srv(raw).raw();
        let s = sampler.state.raw();
        // SAFETY: the context is held; a view and a sampler of this device;
        // the slot is a BindSpec slot (< 16).
        unsafe {
            (ctx.vt.PSSetShaderResources)(ctx.raw, unit, 1, &srv);
            (ctx.vt.PSSetSamplers)(ctx.raw, unit, 1, &s);
        }
    }

    /// Bind `tex` through its Unorm view at the vertex stage's slot `unit`,
    /// which its shader reads by `Load`, with no sampler: the bloom's item
    /// list (`BindSpec::vertex_items`).
    #[inline]
    pub(crate) fn bind_vertex_texture(&mut self, tex: &D3dTexture, unit: u32) {
        let ctx = self.frame.cur.ctx();
        let srv = tex.srv(true).raw();
        // SAFETY: the context is held; a view of this device; the slot is a
        // BindSpec slot (< 16).
        unsafe { (ctx.vt.VSSetShaderResources)(ctx.raw, unit, 1, &srv) };
    }

    /// Bind the per-draw instance stream at input slot 0 with the current
    /// pipeline's stride. A stream the pass cannot take (no pipeline yet, or a
    /// row that reads none) leaves it unstreamed, and the next draw refuses by
    /// name.
    #[inline]
    pub(crate) fn set_stream(&mut self, buf: &D3dBuffer) {
        let Some(bound) = self.bound.as_mut() else {
            return;
        };
        let Some(layout) = bound.layout else {
            return;
        };
        bound.stream = true;
        let ctx = self.frame.cur.ctx();
        let b = buf.buf.raw();
        let stride = layout.stride as u32;
        let offset = 0u32;
        // SAFETY: the context is held; a vertex buffer of this device; one
        // slot's buffer, stride and offset.
        unsafe { (ctx.vt.IASetVertexBuffers)(ctx.raw, 0, 1, &b, &stride, &offset) };
    }

    /// Draw `instances` instances of `vertices` vertices with the current
    /// pipeline (6 for a quad list, 4 for the tray strip, 3 for a fullscreen
    /// triangle).
    #[inline]
    pub(crate) fn draw(&mut self, vertices: u32, instances: u32) -> Result<(), String> {
        let bound = self.bound.ok_or("d3d11: a draw with no pipeline")?;
        if bound.layout.is_some() && !bound.stream {
            return Err(format!(
                "d3d11: a draw with {} and no instance stream bound",
                bound.label
            ));
        }
        let ctx = self.frame.cur.ctx();
        // SAFETY: the context is held; shaders, layout and (for a streamed row)
        // the instance stream are bound.
        unsafe { (ctx.vt.DrawInstanced)(ctx.raw, vertices, instances, 0, 0) };
        // No per-draw error read on the hot path: a removed device answers
        // the frame's `submit`.
        Ok(())
    }
}
