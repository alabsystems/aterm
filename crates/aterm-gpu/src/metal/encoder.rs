// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The encoder: multi-pass command buffers.

use std::sync::Arc;

use super::ffi::{
    AutoreleasePool, ClassPtr, Device, Id, LOAD_ACTION_CLEAR, LOAD_ACTION_LOAD, LoadAction,
    MtlOrigin, MtlScissorRect, MtlSize, MtlViewport, Obj, PixelFormat, PrimitiveType,
    STORE_ACTION_STORE, Sel, checked_msg, class, sel, texture_height, texture_pixel_format_raw,
    texture_width,
};
use super::loss;
use super::resources::SealedTexture;

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

/// Test-only census of the FFI calls a device-loss refusal must precede: the `commandBuffer` mints in [`EncodeSession::begin`] and `Frame::present`, and the swapchain's `nextDrawable`.
#[cfg(test)]
pub(crate) mod queue_ffi_census {
    use std::cell::Cell;

    thread_local! {
        static CALLS: Cell<u64> = const { Cell::new(0) };
    }

    /// One `commandBuffer` mint or `nextDrawable` about to run on this thread.
    pub(crate) fn note() {
        CALLS.with(|calls| calls.set(calls.get() + 1));
    }

    /// How many this thread has made so far.
    pub(crate) fn calls() -> u64 {
        CALLS.with(Cell::get)
    }
}

/// The owner of the render queue (see the module header).
#[derive(Debug)]
pub(crate) struct EncodeSession {
    /// The one `MTLCommandQueue` of the encode and present path. Lent only to
    /// the blocking one-shot helpers via [`Self::one_shot_queue`].
    queue: Obj,
    /// The process device-loss latch every submission answers to.
    latch: Arc<loss::LossLatch>,
    /// The present being timed (`crate::gpu_time`), between
    /// [`Self::open_gpu_frame`] and [`Self::seal_gpu_frame`]: every command
    /// buffer committed meanwhile reports its GPU span to it.
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
        // can construct a successor. The queue itself is released by `Obj`.
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
        // SAFETY: the pass descriptor, `colorAttachments`, its subscript and the encoder are autoreleased into the pool; the encoder is retained to +1 before the pop.
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
        let _pool = AutoreleasePool::new();
        // SAFETY: `blitCommandEncoder` returns an autoreleased encoder owned by the pool through `endEncoding`.
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
    // SAFETY: `MTLCommandBufferHandler` is `void (^)(id<MTLCommandBuffer>)` — exactly `(Id) -> ()`, the prototype this block is invoked with.
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

// SAFETY: the probe only reads command-buffer status (thread-safe reads) and waits; drop is `objc_release`, which is thread-safe.
unsafe impl Send for CbProbe {}

/// A committed command buffer: the non-blocking completion handle used in place of `addCompletedHandler:`.
#[derive(Debug)]
pub(crate) struct Submitted {
    cb: Obj,
    latch: Arc<loss::LossLatch>,
    /// The classified terminal outcome once one exists. The first terminal
    /// wait or poll feeds the latch and fills this; later calls answer from it,
    /// so each command buffer feeds the latch exactly once.
    settled: std::cell::Cell<Option<loss::CbOutcome>>,
}

impl Submitted {
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
mod tests {
    use super::*;
    use crate::metal::ffi::{
        self, ClearColor, TEXTURE_USAGE_PIXEL_FORMAT_VIEW, TEXTURE_USAGE_RENDER_TARGET,
        TEXTURE_USAGE_SHADER_READ,
    };
    use crate::metal::pipelines;
    use crate::metal::test_support::device;

    /// An `Rgba8Unorm` texture holding `bytes` (tight stride), sealed in
    /// `mint`'s loss domain.
    fn uploaded_texture(
        mint: &super::super::resources::MetalResourceDevice,
        w: usize,
        h: usize,
        bytes: &[u8],
    ) -> SealedTexture {
        let tex = mint
            .texture_2d(PixelFormat::Rgba8Unorm, w, h, TEXTURE_USAGE_SHADER_READ)
            .expect("texture");
        // SAFETY: managed-storage (non-Private) texture, exact extent and stride.
        unsafe { tex.upload(ffi::MtlRegion::full_2d(w, h), bytes, w * 4) };
        tex
    }

    /// Read a texture back through the session (begin, copy, commit, wait),
    /// returning tight-stride bytes; asserts `Completed`.
    fn read_texture(
        session: &EncodeSession,
        dev: &Device,
        tex: &SealedTexture,
        w: usize,
        h: usize,
    ) -> Vec<u8> {
        let row = w * 4;
        let rb = dev.new_buffer(row * h).expect("readback");
        let mut cb = session.begin().expect("cb");
        cb.copy_texture_to_buffer(tex, w, h, &rb, row)
            .expect("copy");
        assert_eq!(cb.commit().wait_outcome(), loss::CbOutcome::Completed);
        // SAFETY: shared storage; the command buffer completed above.
        unsafe { ffi::buffer_bytes(&rb, row * h) }
    }

    /// Texture copies: a full copy reproduces every texel; a sub-rect copy with
    /// distinct origins moves exactly the named bytes and leaves sentinels
    /// outside the destination rect untouched; a Unorm-to-sRGB-view copy moves
    /// raw bytes 1:1.
    #[test]
    fn texture_copies_move_exactly_the_named_bytes() {
        let dev = device();
        let _test_pool = AutoreleasePool::new();
        let latch = Arc::new(loss::LossLatch::new());
        let session = EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");
        let mint = super::super::resources::MetalResourceDevice::new(&dev, Arc::clone(&latch));

        const W: usize = 16;
        const H: usize = 16;
        let unique: Vec<u8> = (0..W * H)
            .flat_map(|i| {
                let (x, y) = (i % W, i / W);
                [(x * 16) as u8, (y * 16) as u8, (x + y) as u8, 255]
            })
            .collect();
        const SENTINEL: [u8; 4] = [7, 9, 11, 13];
        let sentinel_frame: Vec<u8> = SENTINEL.iter().copied().cycle().take(W * H * 4).collect();

        let src = uploaded_texture(&mint, W, H, &unique);

        let dst_full = uploaded_texture(&mint, W, H, &sentinel_frame);
        let mut cb = session.begin().expect("cb");
        cb.copy_texture_to_texture(&src, &dst_full)
            .expect("full copy");
        assert_eq!(cb.commit().wait_outcome(), loss::CbOutcome::Completed);
        assert_eq!(
            read_texture(&session, &dev, &dst_full, W, H),
            unique,
            "the full copy must reproduce the source byte-for-byte"
        );

        const CW: usize = 5;
        const CH: usize = 6;
        const SO: (usize, usize) = (3, 2);
        const DO: (usize, usize) = (8, 7);
        let dst = uploaded_texture(&mint, W, H, &sentinel_frame);
        let mut cb = session.begin().expect("cb");
        cb.copy_texture_sub_rect(&src, SO, &dst, DO, CW, CH)
            .expect("sub-rect copy");
        assert_eq!(cb.commit().wait_outcome(), loss::CbOutcome::Completed);
        let got = read_texture(&session, &dev, &dst, W, H);
        for (i, texel) in got.as_chunks::<4>().0.iter().enumerate() {
            let (x, y) = (i % W, i / W);
            let inside = x >= DO.0 && x < DO.0 + CW && y >= DO.1 && y < DO.1 + CH;
            let want: [u8; 4] = if inside {
                let (sx, sy) = (x - DO.0 + SO.0, y - DO.1 + SO.1);
                let j = (sy * W + sx) * 4;
                unique[j..j + 4].try_into().expect("4 bytes")
            } else {
                SENTINEL
            };
            assert_eq!(
                *texel, want,
                "({x},{y}): a transposed origin pair, a dropped source origin or a \
                 leaked write outside the destination rect lands exactly here"
            );
        }
        assert!(!latch.is_lost());

        let dst_pair = mint
            .texture_2d(
                PixelFormat::Rgba8Unorm,
                W,
                H,
                TEXTURE_USAGE_SHADER_READ
                    | TEXTURE_USAGE_RENDER_TARGET
                    | TEXTURE_USAGE_PIXEL_FORMAT_VIEW,
            )
            .expect("pair dst");
        let dst_view = dst_pair
            .alias_view(PixelFormat::Rgba8UnormSrgb)
            .expect("view");
        let mut cb = session.begin().expect("cb");
        cb.copy_texture_to_texture(&src, &dst_view)
            .expect("pair copy");
        assert_eq!(cb.commit().wait_outcome(), loss::CbOutcome::Completed);
        assert_eq!(
            read_texture(&session, &dev, &dst_pair, W, H),
            unique,
            "the Unorm->sRGB alias pair must copy raw bytes, not re-encode them"
        );
        crate::stderr_line!(
            "copy proof on {}: full, 5x6 sub-rect (3,2)->(8,7) and alias-pair copies \
             all byte-exact over {} texels",
            dev.name(),
            W * H
        );
    }

    /// The copy validators refuse, with a reason, out-of-bounds rects, empty
    /// regions, overlapping same-texture rects, incompatible format pairs, an understated readback stride and
    /// a short readback buffer, before any encoder exists, so the command buffer
    /// stays committable (the final commit proves it).
    #[test]
    fn the_copy_validators_refuse_what_metal_would_corrupt() {
        let dev = device();
        let _test_pool = AutoreleasePool::new();
        let latch = Arc::new(loss::LossLatch::new());
        let session = EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");
        let mint = super::super::resources::MetalResourceDevice::new(&dev, Arc::clone(&latch));

        let t16 = mint
            .texture_2d(PixelFormat::Rgba8Unorm, 16, 16, TEXTURE_USAGE_SHADER_READ)
            .expect("16x16");
        let t8 = mint
            .texture_2d(PixelFormat::Rgba8Unorm, 8, 8, TEXTURE_USAGE_SHADER_READ)
            .expect("8x8");
        let f16 = mint
            .texture_2d(PixelFormat::Rgba16Float, 16, 16, TEXTURE_USAGE_SHADER_READ)
            .expect("f16");
        let r8 = mint
            .texture_2d(PixelFormat::R8Unorm, 16, 16, TEXTURE_USAGE_SHADER_READ)
            .expect("r8");

        let mut cb = session.begin().expect("cb");

        let err = cb
            .copy_texture_sub_rect(&t16, (12, 0), &t16, (0, 0), 5, 5)
            .expect_err("source rect out of bounds");
        assert!(err.contains("source rect"), "got: {err}");
        let err = cb
            .copy_texture_sub_rect(&t16, (0, 0), &t8, (4, 4), 5, 5)
            .expect_err("destination rect out of bounds");
        assert!(err.contains("destination rect"), "got: {err}");
        let err = cb
            .copy_texture_sub_rect(&t16, (0, 0), &t16, (0, 0), 0, 4)
            .expect_err("empty region");
        assert!(err.contains("empty"), "got: {err}");
        let err = cb
            .copy_texture_sub_rect(&t16, (0, 3), &t16, (0, 0), 16, 8)
            .expect_err("overlapping same-texture band");
        assert!(err.contains("overlapping"), "got: {err}");
        let err = cb
            .copy_texture_to_texture(&t16, &f16)
            .expect_err("4-byte -> 8-byte texels");
        assert!(err.contains("copy-compatible"), "got: {err}");
        let err = cb
            .copy_texture_to_texture(&t16, &r8)
            .expect_err("4-byte -> 1-byte texels");
        assert!(err.contains("copy-compatible"), "got: {err}");

        let big = dev.new_buffer(128 * 16).expect("big");
        let err = cb
            .copy_texture_to_buffer(&f16, 16, 16, &big, 16 * 4)
            .expect_err("understated bytes_per_row");
        assert!(err.contains("destinationBytesPerRow"), "got: {err}");
        let short = dev.new_buffer(64).expect("short");
        let err = cb
            .copy_texture_to_buffer(&t16, 16, 16, &short, 16 * 4)
            .expect_err("short readback buffer");
        assert!(err.contains("holds"), "got: {err}");
        let err = cb
            .copy_texture_to_buffer(&t16, 0, 16, &big, 16 * 4)
            .expect_err("empty readback region");
        assert!(err.contains("empty"), "got: {err}");
        let err = cb
            .copy_texture_to_buffer(&t16, 17, 16, &big, 17 * 4)
            .expect_err("oversized readback region");
        assert!(err.contains("exceeds"), "got: {err}");

        assert_eq!(cb.commit().wait_outcome(), loss::CbOutcome::Completed);
        assert!(
            !latch.is_lost(),
            "refused copies are caller errors, not losses"
        );
    }

    /// An injected `PageFault` through [`Submitted::settle`] latches, and every
    /// later [`EncodeSession::begin`] refuses naming the first loss, with no
    /// command-buffer mint ([`queue_ffi_census`]).
    #[test]
    fn an_injected_loss_refuses_further_encodes_fast_and_named() {
        let dev = device();
        let _test_pool = AutoreleasePool::new();
        let latch = Arc::new(loss::LossLatch::new());
        let session = EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");

        let minted = queue_ffi_census::calls();
        let s0 = session.begin().expect("healthy begin").commit();
        assert_eq!(
            queue_ffi_census::calls(),
            minted + 1,
            "a healthy begin mints exactly one command buffer"
        );
        assert_eq!(s0.wait_outcome(), loss::CbOutcome::Completed);
        assert!(!latch.is_lost());

        let retry = s0.settle(loss::classify(
            loss::STATUS_ERROR,
            Some(loss::ERROR_TIMEOUT),
        ));
        assert!(matches!(retry, loss::CbOutcome::Retryable { .. }));
        drop(
            session
                .begin()
                .expect("a retryable failure must not stop encoding")
                .commit(),
        );

        let lost = s0.settle(loss::classify(
            loss::STATUS_ERROR,
            Some(loss::ERROR_PAGE_FAULT),
        ));
        assert!(matches!(lost, loss::CbOutcome::Lost { .. }));
        assert!(latch.is_lost());

        for attempt in 0..2 {
            let minted = queue_ffi_census::calls();
            let err = session
                .begin()
                .expect_err("begin after the loss must refuse");
            assert!(
                err.contains("encode refused") && err.contains("PageFault"),
                "attempt {attempt}: the refusal names the first loss: {err}"
            );
            assert_eq!(
                queue_ffi_census::calls(),
                minted,
                "attempt {attempt}: the refusal must precede any FFI — it \
                 minted a command buffer on the dead queue"
            );
        }
    }

    /// One latch, one session: a second session on the same latch is refused,
    /// and dropping the session frees the slot for the loss rebuild.
    #[test]
    fn one_latch_one_session_and_drop_frees_the_slot() {
        let dev = device();
        let _test_pool = AutoreleasePool::new();
        let latch = Arc::new(loss::LossLatch::new());
        let session = EncodeSession::new(&dev, Arc::clone(&latch)).expect("first session");

        let err = match EncodeSession::new(&dev, Arc::clone(&latch)) {
            Ok(_) => panic!(
                "a second session on the SAME latch must refuse — a rogue \
                 twin presents on a queue the rendering never used"
            ),
            Err(e) => e,
        };
        assert!(
            err.contains("one latch means ONE queue"),
            "the refusal names the seal: {err}"
        );

        assert_eq!(
            session.begin().expect("cb").commit().wait_outcome(),
            loss::CbOutcome::Completed
        );

        drop(session);
        let rebuilt = EncodeSession::new(&dev, Arc::clone(&latch))
            .expect("after the first session drops, the latch is claimable again");
        assert_eq!(
            rebuilt.begin().expect("cb").commit().wait_outcome(),
            loss::CbOutcome::Completed
        );
    }
    /// One command buffer feeds the latch exactly once through the status paths,
    /// however `wait_outcome` and `try_outcome` are mixed.
    #[test]
    fn terminal_outcomes_feed_the_latch_exactly_once_across_wait_and_poll() {
        let dev = device();
        let _test_pool = AutoreleasePool::new();
        let latch = Arc::new(loss::LossLatch::new());
        let session = EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");

        let s1 = session.begin().expect("cb 1").commit();
        #[cfg(debug_assertions)]
        let before = latch.fed_count();
        assert_eq!(s1.wait_outcome(), loss::CbOutcome::Completed);
        #[cfg(debug_assertions)]
        assert_eq!(latch.fed_count(), before + 1, "the wait feeds once");
        assert_eq!(s1.try_outcome(), Some(loss::CbOutcome::Completed));
        assert_eq!(s1.wait_outcome(), loss::CbOutcome::Completed);
        assert_eq!(s1.try_outcome(), Some(loss::CbOutcome::Completed));
        #[cfg(debug_assertions)]
        assert_eq!(
            latch.fed_count(),
            before + 1,
            "polls and waits AFTER the terminal answer feed nothing — the \
             once-feed cache is what keeps per-submit feed counts truthful"
        );

        let s2 = session.begin().expect("cb 2").commit();
        let polled = loop {
            if let Some(o) = s2.try_outcome() {
                break o;
            }
            std::hint::spin_loop();
        };
        assert_eq!(polled, loss::CbOutcome::Completed);
        #[cfg(debug_assertions)]
        let after_poll = latch.fed_count();
        #[cfg(debug_assertions)]
        assert_eq!(after_poll, before + 2, "the one terminal poll fed once");
        assert_eq!(s2.wait_outcome(), loss::CbOutcome::Completed);
        #[cfg(debug_assertions)]
        assert_eq!(
            latch.fed_count(),
            after_poll,
            "a wait after the terminal poll answers from the cache"
        );

        #[cfg(debug_assertions)]
        {
            let seam_before = latch.fed_count();
            let _ = s2.settle(loss::CbOutcome::Completed);
            let _ = s2.settle(loss::CbOutcome::Completed);
            assert_eq!(
                latch.fed_count(),
                seam_before + 2,
                "settle is the injection seam and must feed every call"
            );
        }
        assert!(!latch.is_lost());
    }

    /// The draw-state checks: a draw with no pipeline, an instanced draw with no
    /// stream, and a pass onto a non-render-target texture are named errors.
    #[test]
    fn the_draw_state_machine_refuses_what_the_driver_punishes() {
        let dev = device();
        let _test_pool = AutoreleasePool::new();
        let latch = Arc::new(loss::LossLatch::new());
        let session = EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");

        let mint = super::super::resources::MetalResourceDevice::new(&dev, Arc::clone(&latch));
        let target = mint
            .texture_2d(
                PixelFormat::Rgba8Unorm,
                8,
                8,
                TEXTURE_USAGE_RENDER_TARGET
                    | TEXTURE_USAGE_SHADER_READ
                    | TEXTURE_USAGE_PIXEL_FORMAT_VIEW,
            )
            .expect("target");
        let target_srgb = target
            .alias_view(PixelFormat::Rgba8UnormSrgb)
            .expect("sRGB render-target view");
        let sampled_only = mint
            .texture_2d(PixelFormat::Rgba8Unorm, 8, 8, TEXTURE_USAGE_SHADER_READ)
            .expect("sampled-only");

        let mut cb = session.begin().expect("cb");
        let err = cb
            .render_pass(&RenderPassDesc {
                target: &sampled_only,
                load: LoadAction::Clear(ClearColor {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                }),
                store: StoreAction::Store,
                viewport: None,
                scissor: None,
            })
            .expect_err("non-render-target pass must refuse");
        assert!(err.contains("TEXTURE_USAGE_RENDER_TARGET"), "got: {err}");

        {
            let pass = cb
                .render_pass(&RenderPassDesc {
                    target: &target,
                    load: LoadAction::Clear(ClearColor {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    }),
                    store: StoreAction::Store,
                    viewport: None,
                    scissor: None,
                })
                .expect("pass");
            let err = pass
                .draw_fullscreen_triangle()
                .expect_err("no-pipeline draw must refuse");
            assert!(err.contains("no pipeline"), "got: {err}");
            let err = pass
                .draw_instanced(PrimitiveType::Triangle, 6, 1)
                .expect_err("no-pipeline instanced draw must refuse");
            assert!(
                err.contains("no instance stream") || err.contains("no pipeline"),
                "got: {err}"
            );
        }

        {
            let cell = pipelines::compile_library(&dev, crate::pipeline_table::ShaderLibrary::Cell)
                .expect("cell library");
            let bg = pipelines::build(
                &dev,
                &cell,
                crate::pipeline_table::Pipeline::Bg.spec(),
                PixelFormat::Rgba8Unorm,
            )
            .expect("bg pipeline");
            let pass = cb
                .render_pass(&RenderPassDesc {
                    target: &target_srgb,
                    load: LoadAction::Load,
                    store: StoreAction::Store,
                    viewport: None,
                    scissor: None,
                })
                .expect("pass");
            pass.set_pipeline(&bg);
            let err = pass
                .draw_instanced(PrimitiveType::Triangle, 6, 1)
                .expect_err("stream-less instanced draw must refuse");
            assert!(err.contains("no instance stream"), "got: {err}");
        }
        drop(cb);

        let uniform = dev.new_buffer(64).expect("uniform");
        let mut cb = session.begin().expect("cb2");
        {
            let pass = cb
                .render_pass(&RenderPassDesc {
                    target: &target,
                    load: LoadAction::Load,
                    store: StoreAction::Store,
                    viewport: None,
                    scissor: None,
                })
                .expect("pass");
            let err = pass
                .set_vertex_buffer(&uniform, super::super::ffi::INSTANCE_STREAM_SLOT)
                .expect_err("binding a vertex uniform at the stream slot must refuse");
            assert!(
                err.contains("INSTANCE_STREAM_SLOT") || err.contains("30"),
                "got: {err}"
            );
        }
        drop(cb);

        let mut cb = session.begin().expect("cb3");
        let err = cb
            .copy_texture_sub_rect(&target, (0, 0), &target, (0, 2), 8, 6)
            .expect_err("overlapping self-copy must refuse");
        assert!(err.contains("overlapping same-texture"), "got: {err}");
        cb.copy_texture_sub_rect(&target, (0, 0), &target, (0, 4), 8, 4)
            .expect("disjoint same-texture copy is legal");
        let outcome = cb.commit().wait_outcome();
        assert_eq!(outcome, loss::CbOutcome::Completed);
        assert!(!latch.is_lost());
    }
}
