// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The encoder: multi-pass command buffers.

use std::ffi::c_void;
use std::sync::Arc;

use super::ffi::{
    AutoreleasePool, ClassPtr, Device, Id, LOAD_ACTION_CLEAR, LOAD_ACTION_LOAD, LoadAction,
    MtlOrigin, MtlScissorRect, MtlSize, MtlViewport, Obj, PixelFormat, PrimitiveType,
    STORE_ACTION_STORE, Sel, checked_msg, class, sel, texture_height, texture_pixel_format_raw,
    texture_width,
};
use super::loss;
use super::resources::SealedTexture;

/// The most bytes Metal binds inline (`-setVertexBytes:length:atIndex:`): 4 KiB.
const INLINE_BYTES_MAX: usize = 4096;

/// What a render pass does with its results.
#[derive(Clone, Copy, Debug)]
pub(crate) enum StoreAction {
    /// `MTLStoreActionStore` — keep every texel.
    Store,
}

/// Everything one render pass declares up front.
pub(crate) struct RenderPassDesc<'a> {
    /// The colour attachment: a sealed texture or a format view of one (the sRGB/Unorm pair).
    pub(crate) target: &'a SealedTexture,
    /// What happens to `target`'s existing texels before the first draw.
    pub(crate) load: LoadAction,
    /// What happens to the pass's results — always [`StoreAction::Store`].
    pub(crate) store: StoreAction,
    /// `-setViewport:`, or the attachment's full extent when `None`.
    pub(crate) viewport: Option<MtlViewport>,
    /// `-setScissorRect:`, or the attachment's full extent when `None`, validated against the live attachment's extent.
    pub(crate) scissor: Option<MtlScissorRect>,
}

/// Test-only census of the blit encoders this thread opened ([`CommandBuffer::copy_texture_sub_rect`] and the readback copy): a present that only composes and draws opens none.
#[cfg(test)]
#[path = "encoder_blit_census_tests.rs"]
pub(crate) mod blit_census;

/// Test-only census of the FFI calls a device-loss refusal must precede: the `commandBuffer` mints in [`EncodeSession::begin`] and `Frame::present`, and the swapchain's `nextDrawable`.
#[cfg(test)]
#[path = "encoder_queue_ffi_census_tests.rs"]
pub(crate) mod queue_ffi_census;

/// The owner of the one render queue: it mints every [`CommandBuffer`], so command buffers
/// schedule in commit order.
#[derive(Debug)]
pub(crate) struct EncodeSession {
    /// The one `MTLCommandQueue` of the encode and present path.
    queue: Obj,
    /// The process device-loss latch every submission answers to.
    latch: Arc<loss::LossLatch>,
    /// The present being timed (`crate::gpu_time`) between [`Self::open_gpu_frame`] and [`Self::seal_gpu_frame`].
    gpu_frame: std::cell::Cell<Option<crate::gpu_time::FrameToken<'static>>>,
}

impl EncodeSession {
    /// Build the session's queue on `device`.
    pub(crate) fn new(device: &Device, latch: Arc<loss::LossLatch>) -> Result<Self, String> {
        if !latch.try_bind_encode_session() {
            return Err(
                "encode session refused: this loss latch already has a live \
                 EncodeSession — one latch means ONE queue, or Frame::present's \
                 latch cross-check would pass on a queue the rendering never used"
                    .to_owned(),
            );
        }
        let Some(queue) = device.new_command_queue() else {
            latch.unbind_encode_session();
            return Err("MTLCommandQueue allocation failed".to_owned());
        };
        Ok(Self {
            queue,
            latch,
            gpu_frame: std::cell::Cell::new(None),
        })
    }

    /// Start timing one present: open a `gpu_time` frame (only when a sink listens) that every command buffer committed until [`Self::seal_gpu_frame`] reports to.
    pub(crate) fn open_gpu_frame(&self) -> Option<u64> {
        if let Some(stale) = self.gpu_frame.take() {
            stale.seal();
        }
        let token = crate::gpu_time::open_frame()?;
        self.gpu_frame.set(Some(token));
        Some(token.serial())
    }

    /// Stop timing the present [`Self::open_gpu_frame`] opened.
    pub(crate) fn seal_gpu_frame(&self) {
        if let Some(token) = self.gpu_frame.take() {
            token.seal();
        }
    }

    /// The session's latch, for wiring cross-checks (present compares it to the swapchain's by pointer).
    #[inline]
    pub(crate) const fn latch(&self) -> &Arc<loss::LossLatch> {
        &self.latch
    }

    /// Lend the queue to a blocking one-shot helper ([`super::ffi:: draw_and_read`], `dispatch_compute`).
    #[inline]
    pub(crate) const fn one_shot_queue(&self) -> &Obj {
        &self.queue
    }

    /// Mint one command buffer, or refuse before any FFI when the device is lost.
    pub(crate) fn begin(&self) -> Result<CommandBuffer<'_>, String> {
        if let Some(reason) = self.latch.reason() {
            return Err(format!(
                "encode refused: the device-loss latch is set ({reason}); \
                 not minting a command buffer on the dead queue — downgrade instead"
            ));
        }
        #[cfg(test)]
        queue_ffi_census::note();
        let _pool = AutoreleasePool::new();
        // SAFETY: `commandBuffer` returns a +0 buffer owned by the pool; it is retained to +1 to outlive the pop.
        let cb = unsafe {
            let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let raw = get(self.queue.id(), sel(c"commandBuffer"));
            Obj::retain(raw).ok_or_else(|| "commandBuffer returned nil".to_owned())?
        };
        Ok(CommandBuffer { cb, session: self })
    }
}

impl Drop for EncodeSession {
    fn drop(&mut self) {
        self.latch.unbind_encode_session();
    }
}

/// One command buffer under construction: any number of render passes and blit copies, in encode order, then [`Self::commit`].
#[derive(Debug)]
pub(crate) struct CommandBuffer<'s> {
    /// +1 on the `MTLCommandBuffer`.
    cb: Obj,
    session: &'s EncodeSession,
}

impl<'s> CommandBuffer<'s> {
    /// Open one render pass.
    pub(crate) fn render_pass<'cb>(
        &'cb mut self,
        desc: &RenderPassDesc<'_>,
    ) -> Result<PassEncoder<'cb, 's>, String> {
        // A target stamped with another loss domain would be rendered on a
        // queue that domain's present does not order against; refuse before
        // any FFI, by pointer identity.
        if !Arc::ptr_eq(desc.target.latch(), &self.session.latch) {
            return Err("render pass refused: the target texture was minted in a \
                 DIFFERENT loss domain than this session's — rendering it here \
                 is the two-latch cross-wired present (render on session 2, \
                 present on session 1); one loss domain per encode path"
                .to_owned());
        }
        let target = desc.target.obj();
        let usage = super::ffi::texture_usage(target);
        if usage & super::ffi::TEXTURE_USAGE_RENDER_TARGET == 0 {
            return Err(format!(
                "render pass target lacks TEXTURE_USAGE_RENDER_TARGET \
                 (usage bits {usage:#x}) — on the plain environment this \
                 draws nowhere and completes; create the texture with the \
                 render-target bit"
            ));
        }
        if let Some(sc) = desc.scissor {
            let (tw, th) = (texture_width(target), texture_height(target));
            if sc.width > tw || sc.height > th || sc.x > tw - sc.width || sc.y > th - sc.height {
                return Err(format!(
                    "scissor {}x{}+{}+{} leaves the {tw}x{th} attachment",
                    sc.width, sc.height, sc.x, sc.y
                ));
            }
        }
        let StoreAction::Store = desc.store;
        let _pool = AutoreleasePool::new();
        // SAFETY: the pass descriptor, `colorAttachments`, its subscript and the
        // encoder are autoreleased into the pool; the encoder is retained to +1
        // before the pop. Every setter is a message on a live object with its
        // prototype written out.
        let enc = unsafe {
            let rp_cls = class(c"MTLRenderPassDescriptor");
            let mk: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Id = checked_msg();
            let rp = mk(rp_cls, sel(c"renderPassDescriptor"));

            let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let atts = get(rp, sel(c"colorAttachments"));
            let sub: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Id = checked_msg();
            let a0 = sub(atts, sel(c"objectAtIndexedSubscript:"), 0);

            let set_obj: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
            set_obj(a0, sel(c"setTexture:"), target.id());
            let set_usize: unsafe extern "C-unwind" fn(Id, Sel, usize) = checked_msg();
            set_usize(a0, sel(c"setStoreAction:"), STORE_ACTION_STORE);
            match desc.load {
                LoadAction::Clear(c) => {
                    set_usize(a0, sel(c"setLoadAction:"), LOAD_ACTION_CLEAR);
                    let set_clear: unsafe extern "C-unwind" fn(Id, Sel, super::ffi::ClearColor) =
                        checked_msg();
                    set_clear(a0, sel(c"setClearColor:"), c);
                }
                LoadAction::Load => set_usize(a0, sel(c"setLoadAction:"), LOAD_ACTION_LOAD),
            }

            let mk_enc: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
            let raw = mk_enc(
                self.cb.id(),
                sel(c"renderCommandEncoderWithDescriptor:"),
                rp,
            );
            Obj::retain(raw).ok_or_else(|| {
                "renderCommandEncoderWithDescriptor: returned nil (already-committed \
                 command buffer, or another encoder is still open on it)"
                    .to_owned()
            })?
        };
        let pass = PassEncoder {
            enc,
            _cb: self,
            pipeline_set: std::cell::Cell::new(false),
            stream_set: std::cell::Cell::new(false),
        };
        if let Some(vp) = desc.viewport {
            pass.set_viewport(vp);
        }
        if let Some(sc) = desc.scissor {
            pass.set_scissor(sc);
        }
        Ok(pass)
    }

    /// Texture to texture, full extent.
    #[cfg(test)]
    pub(crate) fn copy_texture_to_texture(
        &mut self,
        src: &SealedTexture,
        dst: &SealedTexture,
    ) -> Result<(), String> {
        let (w, h) = (texture_width(src.obj()), texture_height(src.obj()));
        self.copy_texture_sub_rect(src, (0, 0), dst, (0, 0), w, h)
    }

    /// Texture to texture, sub-rect with independent origins (scroll band shift, scissored present compose).
    pub(crate) fn copy_texture_sub_rect(
        &mut self,
        src: &SealedTexture,
        src_origin: (usize, usize),
        dst: &SealedTexture,
        dst_origin: (usize, usize),
        width: usize,
        height: usize,
    ) -> Result<(), String> {
        for (side, tex) in [("source", src), ("destination", dst)] {
            if !Arc::ptr_eq(tex.latch(), &self.session.latch) {
                return Err(format!(
                    "copy refused: the {side} texture was minted in a DIFFERENT \
                     loss domain than this session's — one loss domain per \
                     encode path (the two-latch seal)"
                ));
            }
        }
        let (src, dst) = (src.obj(), dst.obj());
        if width == 0 || height == 0 {
            return Err(format!("a {width}x{height} copy region is empty"));
        }
        let (sw, sh) = (texture_width(src), texture_height(src));
        let (dw, dh) = (texture_width(dst), texture_height(dst));
        let (sx, sy) = src_origin;
        let (dx, dy) = dst_origin;
        if width > sw || height > sh || sx > sw - width || sy > sh - height {
            return Err(format!(
                "source rect {width}x{height}+{sx}+{sy} leaves the {sw}x{sh} texture"
            ));
        }
        if width > dw || height > dh || dx > dw - width || dy > dh - height {
            return Err(format!(
                "destination rect {width}x{height}+{dx}+{dy} leaves the {dw}x{dh} texture"
            ));
        }
        if src.id() == dst.id() {
            let x_overlap = sx < dx + width && dx < sx + width;
            let y_overlap = sy < dy + height && dy < sy + height;
            if x_overlap && y_overlap {
                return Err(format!(
                    "overlapping same-texture copy \
                     ({width}x{height}+{sx}+{sy} -> +{dx}+{dy}) is UNDEFINED \
                     per Metal and passes the validation layer silently — \
                     route it through a scratch texture"
                ));
            }
        }
        let sf = texture_pixel_format_raw(src);
        let df = texture_pixel_format_raw(dst);
        if !formats_copy_compatible(sf, df) {
            return Err(format!(
                "MTLPixelFormat {sf} -> {df} is not a copy-compatible pair this \
                 module models (identical formats or a Unorm/sRGB alias pair)"
            ));
        }
        #[cfg(test)]
        blit_census::note();
        let _pool = AutoreleasePool::new();
        // SAFETY: `blitCommandEncoder` returns an autoreleased encoder owned by
        // the pool through `endEncoding`. The rects were validated above.
        unsafe {
            let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let blit = get(self.cb.id(), sel(c"blitCommandEncoder"));
            let copy: unsafe extern "C-unwind" fn(
                Id,
                Sel,
                Id,
                usize,
                usize,
                MtlOrigin,
                MtlSize,
                Id,
                usize,
                usize,
                MtlOrigin,
            ) = checked_msg();
            copy(
                blit,
                sel(
                    c"copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:\
toTexture:destinationSlice:destinationLevel:destinationOrigin:",
                ),
                src.id(),
                0,
                0,
                MtlOrigin { x: sx, y: sy, z: 0 },
                MtlSize {
                    width,
                    height,
                    depth: 1,
                },
                dst.id(),
                0,
                0,
                MtlOrigin { x: dx, y: dy, z: 0 },
            );
            let void_msg: unsafe extern "C-unwind" fn(Id, Sel) = checked_msg();
            void_msg(blit, sel(c"endEncoding"));
        }
        Ok(())
    }

    /// Texture to buffer readback on any command buffer (submit, keep encoding, poll, read).
    pub(crate) fn copy_texture_to_buffer(
        &mut self,
        src: &SealedTexture,
        width: usize,
        height: usize,
        readback: &Obj,
        bytes_per_row: usize,
    ) -> Result<(), String> {
        // The readback also refuses a foreign loss domain.
        if !Arc::ptr_eq(src.latch(), &self.session.latch) {
            return Err("readback copy refused: the source texture was minted in a \
                 DIFFERENT loss domain than this session's — one loss domain \
                 per encode path (the two-latch seal)"
                .to_owned());
        }
        let src = src.obj();
        if width == 0 || height == 0 {
            return Err(format!("a {width}x{height} readback region is empty"));
        }
        let (tw, th) = (texture_width(src), texture_height(src));
        if width > tw || height > th {
            return Err(format!(
                "readback extent {width}x{height} exceeds the texture's {tw}x{th}"
            ));
        }
        let raw = texture_pixel_format_raw(src);
        let bpt = PixelFormat::from_raw(raw)
            .ok_or_else(|| {
                format!(
                    "source MTLPixelFormat {raw} is not one this module models, so its \
                     row stride cannot be checked"
                )
            })?
            .bytes_per_texel();
        let min_row = width * bpt;
        if bytes_per_row < min_row {
            return Err(format!(
                "destinationBytesPerRow({bytes_per_row}) must be >= {min_row} \
                 ({width} texels x {bpt} bytes for MTLPixelFormat {raw})"
            ));
        }
        let need = bytes_per_row
            .checked_mul(height)
            .ok_or_else(|| format!("readback size {bytes_per_row} x {height} overflows usize"))?;
        let have = super::ffi::buffer_length(readback);
        if have < need {
            return Err(format!(
                "readback buffer holds {have} bytes, needs {need} ({bytes_per_row} x {height})"
            ));
        }
        #[cfg(test)]
        blit_census::note();
        let _pool = AutoreleasePool::new();
        // SAFETY: as `copy_texture_sub_rect` — pooled autoreleased blit encoder; the copy's stride/extent/length were validated above.
        unsafe {
            let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let blit = get(self.cb.id(), sel(c"blitCommandEncoder"));
            let copy: unsafe extern "C-unwind" fn(
                Id,
                Sel,
                Id,
                usize,
                usize,
                MtlOrigin,
                MtlSize,
                Id,
                usize,
                usize,
                usize,
            ) = checked_msg();
            copy(
                blit,
                sel(c"copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toBuffer:destinationOffset:destinationBytesPerRow:destinationBytesPerImage:"),
                src.id(),
                0,
                0,
                MtlOrigin { x: 0, y: 0, z: 0 },
                MtlSize {
                    width,
                    height,
                    depth: 1,
                },
                readback.id(),
                0,
                bytes_per_row,
                need,
            );
            let void_msg: unsafe extern "C-unwind" fn(Id, Sel) = checked_msg();
            void_msg(blit, sel(c"endEncoding"));
        }
        Ok(())
    }

    /// Commit.
    pub(crate) fn commit(self) -> Submitted {
        // A timed present: the handler must be added before `commit`.
        if let Some(token) = self.session.gpu_frame.get() {
            report_gpu_span(self.cb.id(), token);
        }
        // SAFETY: `commit` is a void message on the owned command buffer; all encoders are ended (the `&mut` borrow rule) barring `mem::forget`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) = checked_msg();
            f(self.cb.id(), sel(c"commit"));
        }
        Submitted {
            cb: self.cb,
            latch: Arc::clone(&self.session.latch),
            settled: std::cell::Cell::new(None),
        }
    }
}

/// Hang `addCompletedHandler:` on the uncommitted `cb`, reporting its
/// `GPUEndTime - GPUStartTime` to `token`'s frame (`crate::gpu_time`).
fn report_gpu_span(cb: Id, token: crate::gpu_time::FrameToken<'static>) {
    token.hold();
    let handler = move |done: Id| {
        let span = if loss::command_buffer_status(done) == loss::STATUS_COMPLETED {
            // SAFETY: Metal passes the live, completed command buffer; `GPUStartTime` and `GPUEndTime` are `CFTimeInterval` (double) getters, valid once the buffer completed.
            let (start_s, end_s) = unsafe {
                let get: unsafe extern "C-unwind" fn(Id, Sel) -> f64 = checked_msg();
                (
                    get(done, sel(c"GPUStartTime")),
                    get(done, sel(c"GPUEndTime")),
                )
            };
            crate::gpu_time::span_ns(start_s, end_s)
        } else {
            None
        };
        token.complete(span);
    };
    // SAFETY: `MTLCommandBufferHandler` is `void (^)(id<MTLCommandBuffer>)` —
    // exactly `(Id) -> ()`, the prototype this block is invoked with. The
    // closure captures one `Copy` token, does not allocate or panic, and the
    // sink contract forbids blocking or unwinding.
    let Some(block) = (unsafe { aterm_objc::RcBlock::new1(handler) }) else {
        token.complete(None);
        return;
    };
    // SAFETY: `addCompletedHandler:` on an uncommitted buffer copies (retains) the block, so ours is released independently when `block` drops.
    unsafe {
        let add: unsafe extern "C-unwind" fn(Id, Sel, aterm_objc::BlockPtr) = checked_msg();
        add(cb, sel(c"addCompletedHandler:"), block.as_block_ptr());
    }
}

/// Whether a blit between these two raw `MTLPixelFormat`s moves bytes 1:1:
/// identical formats or the Unorm/sRGB pair.
#[inline]
const fn formats_copy_compatible(a: usize, b: usize) -> bool {
    if a == b {
        return true;
    }
    matches!(
        (a, b),
        (70, 71) | (71, 70) | (80, 81) | (81, 80) // Rgba8 and Bgra8 Unorm<->sRGB
    )
}

/// One open render pass.
#[derive(Debug)]
pub(crate) struct PassEncoder<'cb, 's> {
    /// +1 on the `MTLRenderCommandEncoder` (retained out of the creation
    /// pool).
    enc: Obj,
    /// The exclusive borrow that keeps this the command buffer's only open
    /// encoder.
    _cb: &'cb mut CommandBuffer<'s>,
    /// Draw state Metal validates only in the debug layer: a draw with no
    /// pipeline crashes the driver, and an instanced draw with no stream
    /// silently draws nothing.
    pipeline_set: std::cell::Cell<bool>,
    /// See [`Self::pipeline_set`]; flipped by [`Self::set_instance_stream`].
    stream_set: std::cell::Cell<bool>,
}

impl PassEncoder<'_, '_> {
    /// `-setRenderPipelineState:` — the mid-pass pipeline switch.
    #[inline]
    pub(crate) fn set_pipeline(&self, pso: &Obj) {
        self.pipeline_set.set(true);
        // SAFETY: plain object-argument message on the live encoder.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
            f(self.enc.id(), sel(c"setRenderPipelineState:"), pso.id());
        }
    }

    /// Fragment texture at `index` (the mid-pass atlas switch).
    #[inline]
    pub(crate) fn set_fragment_texture(&self, tex: &Obj, index: usize) {
        // SAFETY: plain message on the live encoder; the encoder retains what it needs for the pass.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id, usize) = checked_msg();
            f(
                self.enc.id(),
                sel(c"setFragmentTexture:atIndex:"),
                tex.id(),
                index,
            );
        }
    }

    /// Fragment sampler at `index`.
    #[inline]
    pub(crate) fn set_fragment_sampler(&self, sampler: &Obj, index: usize) {
        // SAFETY: as `set_fragment_texture`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id, usize) = checked_msg();
            f(
                self.enc.id(),
                sel(c"setFragmentSamplerState:atIndex:"),
                sampler.id(),
                index,
            );
        }
    }

    /// Fragment buffer at `index`, offset 0 (every uniform writes from 0).
    #[inline]
    pub(crate) fn set_fragment_buffer(&self, buf: &Obj, index: usize) {
        // SAFETY: as `set_fragment_texture`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id, usize, usize) = checked_msg();
            f(
                self.enc.id(),
                sel(c"setFragmentBuffer:offset:atIndex:"),
                buf.id(),
                0,
                index,
            );
        }
    }

    /// Vertex-stage buffer at `index`, the uniform slot (`cell.metal` and `hdr_glow.metal` at 0, `tray.metal` at 2).
    #[inline]
    pub(crate) fn set_vertex_buffer(&self, buf: &Obj, index: usize) -> Result<(), String> {
        if index == super::ffi::INSTANCE_STREAM_SLOT {
            return Err(format!(
                "vertex buffer index {index} is INSTANCE_STREAM_SLOT — bind streams \
                 through set_instance_stream, uniforms at their small MSL indices"
            ));
        }
        // SAFETY: as `set_fragment_buffer`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id, usize, usize) = checked_msg();
            f(
                self.enc.id(),
                sel(c"setVertexBuffer:offset:atIndex:"),
                buf.id(),
                0,
                index,
            );
        }
        Ok(())
    }

    /// `bytes` as the vertex stage's buffer at `index` (`-setVertexBytes:length:atIndex:`): the
    /// command buffer copies them during the call, so a small uniform needs no `MTLBuffer`.
    /// Metal takes at most 4 KiB this way.
    #[inline]
    pub(crate) fn set_vertex_bytes(&self, bytes: &[u8], index: usize) -> Result<(), String> {
        if index == super::ffi::INSTANCE_STREAM_SLOT {
            return Err(format!(
                "vertex buffer index {index} is INSTANCE_STREAM_SLOT — bind streams \
                 through set_instance_stream, uniforms at their small MSL indices"
            ));
        }
        if bytes.len() > INLINE_BYTES_MAX {
            return Err(format!(
                "{} bytes inline: Metal takes at most {INLINE_BYTES_MAX}",
                bytes.len()
            ));
        }
        // SAFETY: `-setVertexBytes:length:atIndex:` is `v40@0:8r^v16Q24Q32` on a live render
        // command encoder; it copies `length` bytes from the pointer during the call, and the
        // slice is live and that long.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, *const c_void, usize, usize) =
                checked_msg();
            f(
                self.enc.id(),
                sel(c"setVertexBytes:length:atIndex:"),
                bytes.as_ptr().cast(),
                bytes.len(),
                index,
            );
        }
        Ok(())
    }

    /// `bytes` as the fragment stage's buffer at `index`, as [`Self::set_vertex_bytes`]; a slice
    /// past 4 KiB binds nothing.
    #[inline]
    pub(crate) fn set_fragment_bytes(&self, bytes: &[u8], index: usize) {
        if bytes.len() > INLINE_BYTES_MAX {
            return;
        }
        // SAFETY: `-setFragmentBytes:length:atIndex:` as `set_vertex_bytes`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, *const c_void, usize, usize) =
                checked_msg();
            f(
                self.enc.id(),
                sel(c"setFragmentBytes:length:atIndex:"),
                bytes.as_ptr().cast(),
                bytes.len(),
                index,
            );
        }
    }

    /// Bind the per-instance stream at [`super::ffi::INSTANCE_STREAM_SLOT`], the slot no MSL uniform block uses.
    #[inline]
    pub(crate) fn set_instance_stream(&self, stream: &Obj) {
        self.set_instance_stream_at(stream, 0);
    }

    /// [`Self::set_instance_stream`] at a byte offset, for the bloom extract's `extract_first..` sub-stream.
    #[inline]
    pub(crate) fn set_instance_stream_at(&self, stream: &Obj, byte_offset: usize) {
        self.stream_set.set(true);
        // SAFETY: as `set_fragment_buffer`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id, usize, usize) = checked_msg();
            f(
                self.enc.id(),
                sel(c"setVertexBuffer:offset:atIndex:"),
                stream.id(),
                byte_offset,
                super::ffi::INSTANCE_STREAM_SLOT,
            );
        }
    }

    /// `-setScissorRect:` mid-pass, for re-scissoring between draw groups.
    #[inline]
    pub(crate) fn set_scissor(&self, sc: MtlScissorRect) {
        // SAFETY: `MTLScissorRect` travels indirectly per its ffi doc; plain message on the live encoder.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, MtlScissorRect) = checked_msg();
            f(self.enc.id(), sel(c"setScissorRect:"), sc);
        }
    }

    /// `-setViewport:` (the present's blit pass states the full destination).
    #[inline]
    pub(crate) fn set_viewport(&self, vp: MtlViewport) {
        // SAFETY: `MTLViewport` travels indirectly per its ffi doc.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, MtlViewport) = checked_msg();
            f(self.enc.id(), sel(c"setViewport:"), vp);
        }
    }

    /// Instanced draw from vertex 0: `(Triangle, 6, n)` for quads, alongside [`Self::draw_fullscreen_triangle`] and [`Self::draw_strip_quad`].
    #[inline]
    pub(crate) fn draw_instanced(
        &self,
        primitive: PrimitiveType,
        vertices: usize,
        instances: usize,
    ) -> Result<(), String> {
        if !self.stream_set.get() {
            return Err(
                "instanced draw with no instance stream bound — on the plain \
                 environment this encodes and completes SILENTLY with garbage \
                 vertex fetches (the misdraw class); only the validation layer \
                 aborts. Call set_instance_stream first, or use \
                 draw_fullscreen_triangle / draw_strip_quad for vertex-id \
                 shaders that read no stream."
                    .to_owned(),
            );
        }
        self.raw_draw(primitive, vertices, instances)
    }

    /// The 3-vertex fullscreen triangle (`vs_blit`/`vs_fs` synthesize their positions from `[[vertex_id]]`; no stream is bound or needed).
    #[inline]
    pub(crate) fn draw_fullscreen_triangle(&self) -> Result<(), String> {
        self.raw_draw(PrimitiveType::Triangle, 3, 1)
    }

    /// The tray's 4-vertex strip quad.
    #[inline]
    pub(crate) fn draw_strip_quad(&self) -> Result<(), String> {
        self.raw_draw(PrimitiveType::TriangleStrip, 4, 1)
    }

    /// Every draw funnels here; the pipeline check cannot be skipped.
    #[inline]
    fn raw_draw(
        &self,
        primitive: PrimitiveType,
        vertices: usize,
        instances: usize,
    ) -> Result<(), String> {
        if !self.pipeline_set.get() {
            return Err(
                "draw with no pipeline set — on the plain environment this is \
                 a driver SIGSEGV inside drawPrimitives, not a catchable \
                 error. Call set_pipeline first."
                    .to_owned(),
            );
        }
        // SAFETY: plain message on the live encoder; four NSUInteger args.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, usize, usize, usize, usize) = checked_msg();
            f(
                self.enc.id(),
                sel(c"drawPrimitives:vertexStart:vertexCount:instanceCount:"),
                primitive as usize,
                0,
                vertices,
                instances,
            );
        }
        Ok(())
    }
}

impl Drop for PassEncoder<'_, '_> {
    fn drop(&mut self) {
        // SAFETY: `endEncoding` is sent exactly once, from drop.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) = checked_msg();
            f(self.enc.id(), sel(c"endEncoding"));
        }
    }
}

/// A probe-only +1 handle onto a committed command buffer, for the tap ring's status polling.
#[derive(Debug)]
pub(crate) struct CbProbe {
    cb: Obj,
}

impl CbProbe {
    /// A +1 probe onto `sub`'s command buffer.
    #[inline]
    pub(crate) fn of(sub: &Submitted) -> Self {
        Self {
            cb: sub.cb.clone_retained(),
        }
    }

    /// `None` while the command buffer is still in flight, `Some(outcome)` once terminal.
    #[inline]
    pub(crate) fn try_terminal(&self) -> Option<loss::CbOutcome> {
        match loss::outcome_of(self.cb.id()) {
            loss::CbOutcome::Unfinished { .. } => None,
            terminal => Some(terminal),
        }
    }

    /// Block until terminal — the tap's `finish` drain, off the hot path by definition.
    pub(crate) fn wait_terminal(&self) -> loss::CbOutcome {
        // SAFETY: `waitUntilCompleted` is a void message on the owned +1 command buffer; it returns once the status is terminal.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) = checked_msg();
            f(self.cb.id(), sel(c"waitUntilCompleted"));
        }
        loss::outcome_of(self.cb.id())
    }
}

// SAFETY: the probe only reads command-buffer status (thread-safe reads) and
// waits; drop is `objc_release`, which is thread-safe. The tap ring's
// `WindowGpu` crosses a thread once at startup.
unsafe impl Send for CbProbe {}

/// A committed command buffer: the non-blocking completion handle used in place of `addCompletedHandler:`.
#[derive(Debug)]
pub(crate) struct Submitted {
    cb: Obj,
    latch: Arc<loss::LossLatch>,
    /// The classified terminal outcome once one exists, cached so each command buffer feeds the latch once.
    settled: std::cell::Cell<Option<loss::CbOutcome>>,
}

impl Submitted {
    /// How long the GPU ran this command buffer, `GPUEndTime - GPUStartTime` (zero until it ran).
    pub(crate) fn gpu_span(&self) -> aterm_time::Duration {
        let (start, end) = super::ffi::gpu_times(self.cb.id());
        aterm_time::Duration::from_secs_f64((end - start).max(0.0))
    }

    /// Block until terminal, classify, feed the latch once, return the outcome (same contract as [`super::swapchain::PresentTicket::wait_outcome`]).
    pub(crate) fn wait_outcome(&self) -> loss::CbOutcome {
        if let Some(done) = self.settled.get() {
            return done;
        }
        // SAFETY: `waitUntilCompleted` is a void message on the owned +1 command buffer; it returns once the status is terminal.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) = checked_msg();
            f(self.cb.id(), sel(c"waitUntilCompleted"));
        }
        self.settle_terminal(loss::outcome_of(self.cb.id()))
    }

    /// Poll: `None` while in flight, `Some(outcome)` once terminal.
    #[inline]
    pub(crate) fn try_outcome(&self) -> Option<loss::CbOutcome> {
        if let Some(done) = self.settled.get() {
            return Some(done);
        }
        match loss::outcome_of(self.cb.id()) {
            loss::CbOutcome::Unfinished { .. } => None,
            terminal => Some(self.settle_terminal(terminal)),
        }
    }

    /// Record the one terminal outcome: feed the latch, fill the cache.
    #[inline]
    fn settle_terminal(&self, outcome: loss::CbOutcome) -> loss::CbOutcome {
        self.settled.set(Some(outcome));
        self.settle(outcome)
    }

    /// Record one classified outcome on the process latch and return it; the injection seam, like `PresentTicket::settle`.
    #[inline]
    pub(crate) fn settle(&self, outcome: loss::CbOutcome) -> loss::CbOutcome {
        self.latch.record(&outcome);
        outcome
    }
}

#[cfg(test)]
#[path = "encoder_tests.rs"]
mod tests;
