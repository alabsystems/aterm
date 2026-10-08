// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The encoder: multi-pass command buffers.
//!
//! A frame needs several coalesced passes with pipeline, atlas and buffer
//! switches inside a pass, mid-frame texture copies, and non-blocking
//! completion. This module provides a command buffer that opens multiple render
//! passes, a pass encoder that switches state between draws with every bind
//! index supplied by the caller (from the pipeline table's `BindSpec`), blit
//! copies, and a [`Submitted`] handle whose [`Submitted::try_outcome`] polls
//! status instead of parking the thread.
//!
//! # Queue ownership
//!
//! `Frame::present`'s ordering argument holds only because command buffers on
//! one queue schedule in commit order. [`EncodeSession`] owns the process's
//! render queue, every [`CommandBuffer`] is minted by it, and
//! [`super::swapchain::Frame::present`] takes `&EncodeSession`, so the encode
//! path has no second queue. [`EncodeSession::new`] claims its latch
//! ([`loss::LossLatch::try_bind_encode_session`]) and refuses one already
//! claimed, so one latch means one queue; drop releases it for the loss
//! rebuild. Render targets are stamped with their loss domain
//! (`super::resources`), and [`CommandBuffer::render_pass`] and the copy verbs
//! refuse a foreign stamp, so work from another session cannot be presented.
//!
//! # The loss latch
//!
//! [`EncodeSession::begin`] refuses once the latch is lost, before any FFI.
//! [`Submitted::wait_outcome`] and [`Submitted::try_outcome`] feed the latch
//! themselves, as [`super::swapchain::PresentTicket`] does; a debug feed
//! counter makes the feed observable on healthy outcomes.
//!
//! # Autorelease discipline
//!
//! Entry points that produce autoreleased objects (command buffer, pass
//! descriptor, encoders) push a pool and retain what must outlive it into
//! [`Obj`]. Per-draw setters are void messages on live objects and use the
//! caller's frame pool.

use std::sync::Arc;

use super::ffi::{
    AutoreleasePool, ClassPtr, Device, Id, LOAD_ACTION_CLEAR, LOAD_ACTION_LOAD, LoadAction,
    MtlOrigin, MtlScissorRect, MtlSize, MtlViewport, Obj, PixelFormat, PrimitiveType,
    STORE_ACTION_STORE, Sel, class, msg, sel, texture_height, texture_pixel_format_raw,
    texture_width,
};
use super::loss;
use super::resources::SealedTexture;

/// What a render pass does with its results. Only `Store`: the renderer never
/// discards an attachment.
#[derive(Clone, Copy, Debug)]
pub(crate) enum StoreAction {
    /// `MTLStoreActionStore` — keep every texel.
    Store,
}

/// Everything one render pass declares up front. Pipeline, texture and buffer
/// state lives on [`PassEncoder`] and switches between draws.
pub(crate) struct RenderPassDesc<'a> {
    /// The colour attachment: a sealed texture or a format view of one (the
    /// sRGB/Unorm pair). [`CommandBuffer::render_pass`] refuses a loss-domain
    /// stamp that is not the session's.
    pub(crate) target: &'a SealedTexture,
    /// What happens to `target`'s existing texels before the first draw.
    pub(crate) load: LoadAction,
    /// What happens to the pass's results — always [`StoreAction::Store`].
    pub(crate) store: StoreAction,
    /// `-setViewport:`, or the attachment's full extent when `None`.
    pub(crate) viewport: Option<MtlViewport>,
    /// `-setScissorRect:`, or the attachment's full extent when `None`,
    /// validated against the live attachment's extent.
    pub(crate) scissor: Option<MtlScissorRect>,
}

/// Test-only census of the FFI calls a device-loss refusal must precede: the
/// `commandBuffer` mints in [`EncodeSession::begin`] and `Frame::present`, and
/// the swapchain's `nextDrawable`. One-shot helpers in `ffi.rs` are not
/// counted. Per thread, so parallel tests do not count each other's calls. The
/// loss tests assert the count stays flat across every refusal.
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

/// The owner of the render queue (see the module header). Thread-pinned
/// raw-pointer holder; not `Send`/`Sync`.
#[derive(Debug)]
pub(crate) struct EncodeSession {
    /// The one `MTLCommandQueue` of the encode and present path. Lent only to
    /// the blocking one-shot helpers via [`Self::one_shot_queue`].
    queue: Obj,
    /// The process device-loss latch every submission answers to.
    latch: Arc<loss::LossLatch>,
}

impl EncodeSession {
    /// Build the session's queue on `device`. One session per latch: the
    /// binding is claimed before any FFI, so a second session on a claimed
    /// latch is an `Err`. Dropping the session releases the claim for the
    /// device-loss rebuild.
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
        Ok(Self { queue, latch })
    }

    /// The session's latch, for wiring cross-checks (present compares it to
    /// the swapchain's by pointer).
    pub(crate) const fn latch(&self) -> &Arc<loss::LossLatch> {
        &self.latch
    }

    /// Lend the queue to a blocking one-shot helper ([`super::ffi::
    /// draw_and_read`], `dispatch_compute`). Sound because those helpers wait
    /// for completion before returning, and no second queue is constructed.
    pub(crate) const fn one_shot_queue(&self) -> &Obj {
        &self.queue
    }

    /// Mint one command buffer, or refuse before any FFI when the device is
    /// lost.
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
        // SAFETY: `commandBuffer` returns a +0 buffer owned by the pool; it is
        // retained to +1 to outlive the pop.
        let cb = unsafe {
            let get: unsafe extern "C" fn(Id, Sel) -> Id = msg();
            let raw = get(self.queue.id(), sel(c"commandBuffer"));
            Obj::retain(raw).ok_or_else(|| "commandBuffer returned nil".to_owned())?
        };
        Ok(CommandBuffer { cb, session: self })
    }
}

impl Drop for EncodeSession {
    fn drop(&mut self) {
        // Free the latch's session slot so a rebuild (the device-loss path)
        // can construct a successor. The queue itself is released by `Obj`.
        self.latch.unbind_encode_session();
    }
}

/// One command buffer under construction: any number of render passes and
/// blit copies, in encode order, then [`Self::commit`].
#[derive(Debug)]
pub(crate) struct CommandBuffer<'s> {
    /// +1 on the `MTLCommandBuffer`.
    cb: Obj,
    session: &'s EncodeSession,
}

impl<'s> CommandBuffer<'s> {
    /// Open one render pass. The [`PassEncoder`] borrows this command buffer
    /// mutably, so another pass, copy or `commit` cannot be encoded until it
    /// drops and runs `endEncoding` (Metal's one-live-encoder rule as a type).
    /// `mem::forget` on the encoder breaks this and raises an exception at the
    /// next encoder.
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
        // A pass onto a texture without render-target usage completes silently
        // without the validation layer; refuse it.
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
            let mk: unsafe extern "C" fn(ClassPtr, Sel) -> Id = msg();
            let rp = mk(rp_cls, sel(c"renderPassDescriptor"));

            let get: unsafe extern "C" fn(Id, Sel) -> Id = msg();
            let atts = get(rp, sel(c"colorAttachments"));
            let sub: unsafe extern "C" fn(Id, Sel, usize) -> Id = msg();
            let a0 = sub(atts, sel(c"objectAtIndexedSubscript:"), 0);

            let set_obj: unsafe extern "C" fn(Id, Sel, Id) = msg();
            set_obj(a0, sel(c"setTexture:"), target.id());
            let set_usize: unsafe extern "C" fn(Id, Sel, usize) = msg();
            set_usize(a0, sel(c"setStoreAction:"), STORE_ACTION_STORE);
            match desc.load {
                LoadAction::Clear(c) => {
                    set_usize(a0, sel(c"setLoadAction:"), LOAD_ACTION_CLEAR);
                    let set_clear: unsafe extern "C" fn(Id, Sel, super::ffi::ClearColor) = msg();
                    set_clear(a0, sel(c"setClearColor:"), c);
                }
                LoadAction::Load => set_usize(a0, sel(c"setLoadAction:"), LOAD_ACTION_LOAD),
            }

            let mk_enc: unsafe extern "C" fn(Id, Sel, Id) -> Id = msg();
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

    /// Texture to texture, sub-rect with independent origins (scroll band shift,
    /// scissored present compose). Validated off the live objects: both rects
    /// in bounds and the formats byte-compatible (identical, or the Unorm/sRGB
    /// pair).
    pub(crate) fn copy_texture_sub_rect(
        &mut self,
        src: &SealedTexture,
        src_origin: (usize, usize),
        dst: &SealedTexture,
        dst_origin: (usize, usize),
        width: usize,
        height: usize,
    ) -> Result<(), String> {
        // The copy verbs refuse a foreign loss domain on either side too.
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
        // Overlapping same-texture copies are undefined in Metal and accepted
        // silently by validation, so they are refused; disjoint same-texture
        // rects stay legal.
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
        // SAFETY: `blitCommandEncoder` returns an autoreleased encoder owned by
        // the pool through `endEncoding`. The rects were validated above.
        unsafe {
            let get: unsafe extern "C" fn(Id, Sel) -> Id = msg();
            let blit = get(self.cb.id(), sel(c"blitCommandEncoder"));
            let copy: unsafe extern "C" fn(
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
            ) = msg();
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
            let void_msg: unsafe extern "C" fn(Id, Sel) = msg();
            void_msg(blit, sel(c"endEncoding"));
        }
        Ok(())
    }

    /// Texture to buffer readback on any command buffer (submit, keep encoding,
    /// poll, read).
    ///
    /// Validates extent, format-derived minimum stride and destination length,
    /// as `draw_and_read` does. `readback` must be Shared storage; its bytes are
    /// meaningful only once this command buffer's outcome is terminal.
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
        // SAFETY: as `copy_texture_sub_rect` — pooled autoreleased blit
        // encoder; the copy's stride/extent/length were validated above.
        unsafe {
            let get: unsafe extern "C" fn(Id, Sel) -> Id = msg();
            let blit = get(self.cb.id(), sel(c"blitCommandEncoder"));
            let copy: unsafe extern "C" fn(
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
            ) = msg();
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
            let void_msg: unsafe extern "C" fn(Id, Sel) = msg();
            void_msg(blit, sel(c"endEncoding"));
        }
        Ok(())
    }

    /// Commit. The returned [`Submitted`] is the non-blocking handle: wait on
    /// it, or poll [`Submitted::try_outcome`] while encoding the next frame.
    pub(crate) fn commit(self) -> Submitted {
        // SAFETY: `commit` is a void message on the owned command buffer; all
        // encoders are ended (the `&mut` borrow rule) barring `mem::forget`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) = msg();
            f(self.cb.id(), sel(c"commit"));
        }
        Submitted {
            cb: self.cb,
            latch: Arc::clone(&self.session.latch),
            settled: std::cell::Cell::new(None),
        }
    }
}

/// Whether a blit between these two raw `MTLPixelFormat`s moves bytes 1:1:
/// identical formats or the Unorm/sRGB pair.
const fn formats_copy_compatible(a: usize, b: usize) -> bool {
    if a == b {
        return true;
    }
    matches!(
        (a, b),
        (70, 71) | (71, 70) | (80, 81) | (81, 80) // Rgba8 and Bgra8 Unorm<->sRGB
    )
}

/// One open render pass. Every method is a message on the retained encoder;
/// state persists across draws until changed.
#[derive(Debug)]
pub(crate) struct PassEncoder<'cb, 's> {
    /// +1 on the `MTLRenderCommandEncoder` (retained out of the creation
    /// pool). `endEncoding` runs exactly once, on drop.
    enc: Obj,
    /// The exclusive borrow that keeps this the command buffer's ONLY open
    /// encoder.
    _cb: &'cb mut CommandBuffer<'s>,
    /// Draw state Metal validates only in the debug layer: a draw with no
    /// pipeline crashes the driver, and an instanced draw with no stream
    /// silently draws nothing. The encoder tracks both and refuses.
    pipeline_set: std::cell::Cell<bool>,
    /// See [`Self::pipeline_set`]; flipped by [`Self::set_instance_stream`].
    stream_set: std::cell::Cell<bool>,
}

impl PassEncoder<'_, '_> {
    /// `-setRenderPipelineState:` — the mid-pass pipeline switch.
    pub(crate) fn set_pipeline(&self, pso: &Obj) {
        self.pipeline_set.set(true);
        // SAFETY: plain object-argument message on the live encoder.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) = msg();
            f(self.enc.id(), sel(c"setRenderPipelineState:"), pso.id());
        }
    }

    /// Fragment texture at `index` (the mid-pass atlas switch).
    pub(crate) fn set_fragment_texture(&self, tex: &Obj, index: usize) {
        // SAFETY: plain message on the live encoder; the encoder retains what
        // it needs for the pass.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, usize) = msg();
            f(
                self.enc.id(),
                sel(c"setFragmentTexture:atIndex:"),
                tex.id(),
                index,
            );
        }
    }

    /// Fragment sampler at `index`.
    pub(crate) fn set_fragment_sampler(&self, sampler: &Obj, index: usize) {
        // SAFETY: as `set_fragment_texture`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, usize) = msg();
            f(
                self.enc.id(),
                sel(c"setFragmentSamplerState:atIndex:"),
                sampler.id(),
                index,
            );
        }
    }

    /// Fragment buffer at `index`, offset 0 (every uniform writes from 0).
    pub(crate) fn set_fragment_buffer(&self, buf: &Obj, index: usize) {
        // SAFETY: as `set_fragment_texture`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, usize, usize) = msg();
            f(
                self.enc.id(),
                sel(c"setFragmentBuffer:offset:atIndex:"),
                buf.id(),
                0,
                index,
            );
        }
    }

    /// Vertex-stage buffer at `index`, the uniform slot (`cell.metal` and
    /// `hdr_glow.metal` at 0, `tray.metal` at 2). Not for the instance stream,
    /// which binds through [`Self::set_instance_stream`]; this refuses that slot.
    pub(crate) fn set_vertex_buffer(&self, buf: &Obj, index: usize) -> Result<(), String> {
        if index == super::ffi::INSTANCE_STREAM_SLOT {
            return Err(format!(
                "vertex buffer index {index} is INSTANCE_STREAM_SLOT — bind streams \
                 through set_instance_stream, uniforms at their small MSL indices"
            ));
        }
        // SAFETY: as `set_fragment_buffer`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, usize, usize) = msg();
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

    /// Bind the per-instance stream at [`super::ffi::INSTANCE_STREAM_SLOT`], the
    /// slot the MSL scan keeps clear. Any other index lands on the uniforms.
    pub(crate) fn set_instance_stream(&self, stream: &Obj) {
        self.set_instance_stream_at(stream, 0);
    }

    /// [`Self::set_instance_stream`] at a byte offset, for the bloom extract's
    /// `extract_first..` sub-stream. The offset must be a multiple of the
    /// instance stride and inside the buffer.
    pub(crate) fn set_instance_stream_at(&self, stream: &Obj, byte_offset: usize) {
        self.stream_set.set(true);
        // SAFETY: as `set_fragment_buffer`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, usize, usize) = msg();
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
    pub(crate) fn set_scissor(&self, sc: MtlScissorRect) {
        // SAFETY: `MTLScissorRect` travels indirectly per its ffi doc; plain
        // message on the live encoder.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, MtlScissorRect) = msg();
            f(self.enc.id(), sel(c"setScissorRect:"), sc);
        }
    }

    /// `-setViewport:` — the letterbox (one call site in the renderer).
    pub(crate) fn set_viewport(&self, vp: MtlViewport) {
        // SAFETY: `MTLViewport` travels indirectly per its ffi doc.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, MtlViewport) = msg();
            f(self.enc.id(), sel(c"setViewport:"), vp);
        }
    }

    /// Instanced draw from vertex 0: `(Triangle, 6, n)` for quads, alongside
    /// [`Self::draw_fullscreen_triangle`] and [`Self::draw_strip_quad`]. A zero
    /// instance or vertex count encodes as Metal's legal no-op (an empty group).
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

    /// The 3-vertex fullscreen triangle (`vs_blit`/`vs_fs` synthesize their
    /// positions from `[[vertex_id]]`; no stream is bound or needed).
    pub(crate) fn draw_fullscreen_triangle(&self) -> Result<(), String> {
        self.raw_draw(PrimitiveType::Triangle, 3, 1)
    }

    /// The tray's 4-vertex strip quad.
    pub(crate) fn draw_strip_quad(&self) -> Result<(), String> {
        self.raw_draw(PrimitiveType::TriangleStrip, 4, 1)
    }

    /// Every draw funnels here; the pipeline check cannot be skipped.
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
            let f: unsafe extern "C" fn(Id, Sel, usize, usize, usize, usize) = msg();
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
            let f: unsafe extern "C" fn(Id, Sel) = msg();
            f(self.enc.id(), sel(c"endEncoding"));
        }
    }
}

/// A probe-only +1 handle onto a committed command buffer, for the tap ring's
/// status polling. It never feeds the loss latch; the owning [`Submitted`]
/// does, so a tap cannot double-book a loss.
#[derive(Debug)]
pub(crate) struct CbProbe {
    cb: Obj,
}

impl CbProbe {
    /// A +1 probe onto `sub`'s command buffer.
    pub(crate) fn of(sub: &Submitted) -> Self {
        Self {
            cb: sub.cb.clone_retained(),
        }
    }

    /// `None` while the command buffer is still in flight, `Some(outcome)`
    /// once terminal. Feeds nothing (see the type doc).
    pub(crate) fn try_terminal(&self) -> Option<loss::CbOutcome> {
        match loss::outcome_of(self.cb.id()) {
            loss::CbOutcome::Unfinished { .. } => None,
            terminal => Some(terminal),
        }
    }

    /// Block until terminal — the tap's `finish` drain, off the hot path by
    /// definition. Feeds nothing.
    pub(crate) fn wait_terminal(&self) -> loss::CbOutcome {
        // SAFETY: `waitUntilCompleted` is a void message on the owned +1
        // command buffer; it returns once the status is terminal.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) = msg();
            f(self.cb.id(), sel(c"waitUntilCompleted"));
        }
        loss::outcome_of(self.cb.id())
    }
}

// SAFETY: the probe only reads command-buffer status (thread-safe reads) and
// waits; drop is `objc_release`, which is thread-safe. The tap ring's
// `WindowGpu` crosses a thread once at startup.
unsafe impl Send for CbProbe {}

/// A committed command buffer: the non-blocking completion handle used in
/// place of `addCompletedHandler:`.
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
    /// Block until terminal, classify, feed the latch once, return the outcome
    /// (same contract as [`super::swapchain::PresentTicket::wait_outcome`]).
    /// Answers from the cache once settled.
    pub(crate) fn wait_outcome(&self) -> loss::CbOutcome {
        if let Some(done) = self.settled.get() {
            return done;
        }
        // SAFETY: `waitUntilCompleted` is a void message on the owned +1
        // command buffer; it returns once the status is terminal.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) = msg();
            f(self.cb.id(), sel(c"waitUntilCompleted"));
        }
        self.settle_terminal(loss::outcome_of(self.cb.id()))
    }

    /// Poll: `None` while in flight, `Some(outcome)` once terminal. The first
    /// terminal answer feeds the latch like [`Self::wait_outcome`]; later calls
    /// answer from the cache and feed nothing.
    pub(crate) fn try_outcome(&self) -> Option<loss::CbOutcome> {
        if let Some(done) = self.settled.get() {
            return Some(done);
        }
        match loss::outcome_of(self.cb.id()) {
            loss::CbOutcome::Unfinished { .. } => None,
            terminal => Some(self.settle_terminal(terminal)),
        }
    }

    /// Record the ONE terminal outcome: feed the latch, fill the cache.
    fn settle_terminal(&self, outcome: loss::CbOutcome) -> loss::CbOutcome {
        self.settled.set(Some(outcome));
        self.settle(outcome)
    }

    /// Record one classified outcome on the process latch and return it; the
    /// injection seam, like `PresentTicket::settle`. Uncached.
    pub(crate) fn settle(&self, outcome: loss::CbOutcome) -> loss::CbOutcome {
        self.latch.record(&outcome);
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metal::ffi::{
        self, ClearColor, SamplerDesc, TEXTURE_USAGE_PIXEL_FORMAT_VIEW,
        TEXTURE_USAGE_RENDER_TARGET, TEXTURE_USAGE_SHADER_READ,
    };
    use crate::metal::pipelines;
    use crate::metal::test_support::{CellUniforms, as_bytes, device};
    use crate::pipeline_table::Pipeline;

    /// Pack `BgInstance`s (tight 12 bytes: `[u16;4]` rect + `[u8;4]` colour).
    fn bg_stream(instances: &[([u16; 4], [u8; 4])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (rect, colour) in instances {
            for v in rect {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(colour);
        }
        out
    }

    /// Pack FLAT `GlowInstance`s (tight 16 bytes: `[u16;4]` rect + the
    /// `[u8;4]` colour at BOTH ends of the ramp) for the glow-family rows.
    fn glow_stream(instances: &[([u16; 4], [u8; 4])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (rect, colour) in instances {
            for v in rect {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(colour);
            out.extend_from_slice(colour);
        }
        out
    }

    /// One `GlyphInstance` fixture: rect, uv, colour, bg.
    type GlyphFixture = ([f32; 4], [f32; 4], [u8; 4], [u8; 4]);

    /// Pack `GlyphInstance`s (40 bytes: `f32x4` rect + `f32x4` uv + colour +
    /// bg, both `Unorm8x4`).
    fn glyph_stream(instances: &[GlyphFixture]) -> Vec<u8> {
        let mut out = Vec::new();
        for (rect, uv, colour, bg) in instances {
            for v in rect {
                out.extend_from_slice(&v.to_le_bytes());
            }
            for v in uv {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(colour);
            out.extend_from_slice(bg);
        }
        out
    }

    fn shared_buffer(dev: &Device, bytes: &[u8]) -> Obj {
        let buf = dev.new_buffer(bytes.len()).expect("buffer");
        // SAFETY: fresh exactly-sized shared buffer; no GPU work in flight.
        unsafe { ffi::buffer_write(&buf, bytes) };
        buf
    }

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
        // SAFETY: managed-storage (non-Private) texture, exact extent and
        // stride.
        unsafe { tex.upload(ffi::MtlRegion::full_2d(w, h), bytes, w * 4) };
        tex
    }

    const BLACK: LoadAction = LoadAction::Clear(ClearColor {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    });

    /// A multi-pass, multi-draw scene through one command buffer byte-matches
    /// the same content drawn by repeated one-shot [`ffi::draw_and_read`] calls.
    ///
    /// * Two passes on the two views of one offscreen: pass 1 on the sRGB view
    ///   (Clear, with its own scissor that clips content), pass 2 on the Unorm
    ///   base (Load, a glow scissored to a rect that extends past pass 1's).
    /// * Pipeline switches inside pass 1 (bg, color_glyph, cursor_blend,
    ///   color_glyph) and atlas switches (A, B, A).
    /// * Passes 3 and 4 on an aux target: an opaque cover, then `Clear(MID)`
    ///   plus a small quad, so a Clear encoded as Load fails on known bytes.
    /// * Decoy binds one slot over the real ones (texture, sampler, vertex
    ///   buffer), which clobber the real bind if indices are ignored or
    ///   transposed.
    ///
    /// The reference arm reads back after every step and checks each draw
    /// changed only its own rect. Equality is exact.
    #[test]
    fn a_multi_pass_multi_draw_scene_matches_repeated_one_shot_draws() {
        let Some(dev) = device() else { return };
        let _test_pool = AutoreleasePool::new();
        let latch = Arc::new(loss::LossLatch::new());
        let session = EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");
        let mint = super::super::resources::MetalResourceDevice::new(&dev, Arc::clone(&latch));

        const W: usize = 16;
        const H: usize = 16;

        // The four table rows of the scene.
        let cell = pipelines::compile_library(&dev, crate::pipeline_table::ShaderLibrary::Cell)
            .expect("cell.metal");
        let present = PixelFormat::Bgra8Unorm; // irrelevant to offscreen rows
        let build = |p: Pipeline| {
            pipelines::build(&dev, &cell, p.spec(), present)
                .unwrap_or_else(|e| panic!("{} build: {e}", p.name()))
        };
        let bg = build(Pipeline::Bg);
        let color_glyph = build(Pipeline::ColorGlyph);
        let cursor = build(Pipeline::CursorBlend);
        let glow = build(Pipeline::GlowAdd);
        assert_eq!(
            pipelines::metal_format(Pipeline::ColorGlyph.spec().target, present),
            PixelFormat::Rgba8UnormSrgb,
            "the glyph draws attach the sRGB view"
        );
        assert_eq!(
            pipelines::metal_format(Pipeline::GlowAdd.spec().target, present),
            PixelFormat::Rgba8Unorm,
            "the glow pass attaches the Unorm base"
        );

        // Two DIFFERENT atlases, so a dropped rebind is a byte diff.
        let pattern_a: Vec<u8> = (0..8usize * 8)
            .flat_map(|i| {
                let (x, y) = (i % 8, i / 8);
                [(x * 31) as u8, (y * 29) as u8, 200, 255]
            })
            .collect();
        let pattern_b: Vec<u8> = (0..8usize * 8)
            .flat_map(|i| {
                let (x, y) = (i % 8, i / 8);
                [40, (x * 23 + 7) as u8, (y * 27 + 11) as u8, 255]
            })
            .collect();
        assert_ne!(pattern_a, pattern_b, "the two atlases must differ");
        let atlas_a = uploaded_texture(&mint, 8, 8, &pattern_a);
        let atlas_b = uploaded_texture(&mint, 8, 8, &pattern_b);
        let sampler = dev
            .new_sampler(SamplerDesc::NEAREST_CLAMP)
            .expect("sampler");
        // Decoy sampler: linear, so if it clobbers slot 0 the glyph bytes
        // change.
        let decoy_sampler = dev
            .new_sampler(SamplerDesc::LINEAR_CLAMP)
            .expect("decoy sampler");

        #[expect(clippy::cast_precision_loss, reason = "16-texel extents")]
        let uniforms = CellUniforms {
            screen: [W as f32, H as f32],
            text_blend: 0.0,
            pad: 0.0,
        };
        // SAFETY: `repr(C)` into an exactly-sized fresh shared buffer.
        let ubuf = shared_buffer(&dev, unsafe { as_bytes(&uniforms) });
        // Decoy uniforms: a half-size screen, so clobbering slot 0 doubles every
        // quad.
        let wrong_uniforms = CellUniforms {
            screen: [8.0, 8.0],
            text_blend: 0.0,
            pad: 0.0,
        };
        // SAFETY: as above.
        let wrong_ubuf = shared_buffer(&dev, unsafe { as_bytes(&wrong_uniforms) });

        // One stream per draw, so the reference arm can rebind the identical
        // buffer per one-shot call.
        let s_bg = shared_buffer(
            &dev,
            &bg_stream(&[
                ([0, 0, 8, 8], [200, 40, 120, 255]),
                ([8, 8, 8, 8], [40, 200, 90, 255]),
            ]),
        );
        let s_glyph_a1 = shared_buffer(
            &dev,
            &glyph_stream(&[(
                [1.0, 1.0, 4.0, 4.0],
                [0.0, 0.0, 1.0, 1.0],
                [255, 255, 255, 255],
                [0, 0, 0, 255],
            )]),
        );
        let s_cursor = shared_buffer(&dev, &bg_stream(&[([10, 2, 4, 4], [230, 180, 40, 140])]));
        let s_glyph_b = shared_buffer(
            &dev,
            &glyph_stream(&[(
                [9.0, 1.0, 4.0, 4.0],
                [0.0, 0.0, 1.0, 1.0],
                [255, 255, 255, 255],
                [0, 0, 0, 255],
            )]),
        );
        let s_glyph_a2 = shared_buffer(
            &dev,
            &glyph_stream(&[(
                [2.0, 9.0, 4.0, 4.0],
                [0.25, 0.25, 0.5, 0.5],
                [255, 255, 255, 255],
                [0, 0, 0, 255],
            )]),
        );
        let s_glow = shared_buffer(
            &dev,
            &glow_stream(&[
                ([0, 0, 16, 16], [30, 60, 20, 255]),
                ([6, 6, 6, 6], [90, 10, 50, 180]),
            ]),
        );
        const GLOW_SCISSOR: MtlScissorRect = MtlScissorRect {
            x: 4,
            y: 4,
            width: 8,
            height: 8,
        };
        // Pass 1's scissor clips at y = 10, inside the second bg quad and the
        // last glyph, and above the bottom rows of GLOW_SCISSOR, so pass 1's
        // clipping is observable and pass 2 escapes it.
        const PASS1_SCISSOR: MtlScissorRect = MtlScissorRect {
            x: 0,
            y: 0,
            width: 16,
            height: 10,
        };
        // Aux-target fixtures for the Clear-erases proof (passes 3 and 4).
        const COVER: [u8; 4] = [210, 120, 60, 255];
        const SMALL: [u8; 4] = [10, 240, 10, 255];
        const SMALL_RECT: [u16; 4] = [1, 1, 3, 3];
        const MID_CLEAR: LoadAction = LoadAction::Clear(ClearColor {
            r: 0.5,
            g: 0.25,
            b: 0.75,
            a: 1.0,
        });
        let s_cover = shared_buffer(&dev, &bg_stream(&[([0, 0, 16, 16], COVER)]));
        let s_small = shared_buffer(&dev, &bg_stream(&[(SMALL_RECT, SMALL)]));

        let offscreen = |dev: &Device| {
            dev.new_texture_2d(
                PixelFormat::Rgba8Unorm,
                W,
                H,
                TEXTURE_USAGE_RENDER_TARGET
                    | TEXTURE_USAGE_SHADER_READ
                    | TEXTURE_USAGE_PIXEL_FORMAT_VIEW,
            )
            .expect("offscreen")
        };

        let row = W * PixelFormat::Rgba8Unorm.bytes_per_texel();
        let quad6 = super::super::pipelines::metal_primitive_type(Pipeline::Bg.spec().topology);
        // Binding slots come from the table's `BindSpec`; the decoys sit one
        // slot over the real ones.
        let vu = Pipeline::Bg
            .spec()
            .binds
            .vertex_uniform
            .expect("the cell rows have a vertex uniform") as usize;
        let glyph_binds = Pipeline::ColorGlyph.spec().binds;
        let (atlas_tex_slot, atlas_samp_slot) = (
            glyph_binds.fragment_textures[0] as usize,
            glyph_binds.fragment_samplers[0] as usize,
        );

        // ---- Reference arm: repeated one-shot draw_and_read ------------
        let one_tex = offscreen(&dev);
        let one_srgb = ffi::texture_view(&one_tex, PixelFormat::Rgba8UnormSrgb).expect("view");
        let rb = dev.new_buffer(row * H).expect("readback");
        let mut steps: Vec<Vec<u8>> = Vec::new();
        let mut one_shot = |pso: &Obj,
                            dst: &Obj,
                            load: LoadAction,
                            scissor: Option<MtlScissorRect>,
                            tex: Option<&Obj>,
                            stream: &Obj,
                            instances: usize| {
            ffi::draw_and_read(
                session.one_shot_queue(),
                &ffi::Pass {
                    pso,
                    dst,
                    dst_w: W,
                    dst_h: H,
                    load,
                    viewport: None,
                    scissor,
                    src_tex: tex.map(|t| (t, atlas_tex_slot)),
                    sampler: tex.map(|_| (&sampler, atlas_samp_slot)),
                    uniform: None,
                    vertex_uniform: Some((&ubuf, vu)),
                    draw: Some(ffi::DrawCall {
                        primitive: quad6,
                        vertices: 6,
                        instances,
                        stream: Some(stream),
                    }),
                },
                &rb,
                row,
            )
            .expect("one-shot draw");
            // SAFETY: shared storage, written before draw_and_read returned.
            steps.push(unsafe { ffi::buffer_bytes(&rb, row * H) });
        };
        one_shot(&bg, &one_srgb, BLACK, Some(PASS1_SCISSOR), None, &s_bg, 2);
        one_shot(
            &color_glyph,
            &one_srgb,
            LoadAction::Load,
            Some(PASS1_SCISSOR),
            Some(atlas_a.obj()),
            &s_glyph_a1,
            1,
        );
        one_shot(
            &cursor,
            &one_srgb,
            LoadAction::Load,
            Some(PASS1_SCISSOR),
            None,
            &s_cursor,
            1,
        );
        one_shot(
            &color_glyph,
            &one_srgb,
            LoadAction::Load,
            Some(PASS1_SCISSOR),
            Some(atlas_b.obj()),
            &s_glyph_b,
            1,
        );
        one_shot(
            &color_glyph,
            &one_srgb,
            LoadAction::Load,
            Some(PASS1_SCISSOR),
            Some(atlas_a.obj()),
            &s_glyph_a2,
            1,
        );
        one_shot(
            &glow,
            &one_tex,
            LoadAction::Load,
            Some(GLOW_SCISSOR),
            None,
            &s_glow,
            2,
        );
        // The aux reference: full opaque cover, then Clear(MID) + small quad —
        // the deterministic content the Clear-erases proof stands on.
        let aux_ref = offscreen(&dev);
        let aux_ref_srgb = ffi::texture_view(&aux_ref, PixelFormat::Rgba8UnormSrgb).expect("view");
        one_shot(&bg, &aux_ref_srgb, BLACK, None, None, &s_cover, 1);
        one_shot(&bg, &aux_ref_srgb, MID_CLEAR, None, None, &s_small, 1);
        let expected = steps[5].clone();
        let aux_expected = steps[7].clone();

        // Each step's diff from the previous is non-empty and inside that
        // draw's rect, so the final equality is not vacuous.
        let confined = |before: &[u8], after: &[u8], rect: [usize; 4], what: &str| {
            let mut diffs = 0usize;
            for (i, (b, a)) in before
                .as_chunks::<4>()
                .0
                .iter()
                .zip(after.as_chunks::<4>().0)
                .enumerate()
            {
                let (px, py) = (i % W, i / W);
                if b != a {
                    diffs += 1;
                    assert!(
                        px >= rect[0]
                            && px < rect[0] + rect[2]
                            && py >= rect[1]
                            && py < rect[1] + rect[3],
                        "{what}: ({px},{py}) changed outside its own {rect:?} rect"
                    );
                }
            }
            assert!(diffs > 0, "{what}: the draw changed nothing — a dead arm");
        };
        let clear_frame = vec![[0u8, 0, 0, 255]; W * H].concat();
        // Pass-1 rects are their draw rects CLIPPED by PASS1_SCISSOR — a
        // change at y >= 10 in any of them means the scissor was dropped.
        confined(
            &clear_frame,
            &steps[0],
            [0, 0, 16, 10],
            "bg (scissor-clipped)",
        );
        assert_ne!(steps[0], clear_frame, "bg must paint over the clear");
        // The second bg quad's surviving band (y 8..10) is painted.
        assert!(
            (8..10).any(|y| (8..16).any(|x| {
                let i = (y * W + x) * 4;
                steps[0][i..i + 4] != clear_frame[i..i + 4]
            })),
            "the bg quad's band above the pass-1 scissor edge must be painted"
        );
        confined(&steps[0], &steps[1], [1, 1, 4, 4], "glyph atlas A #1");
        confined(&steps[1], &steps[2], [10, 2, 4, 4], "cursor");
        confined(&steps[2], &steps[3], [9, 1, 4, 4], "glyph atlas B");
        confined(
            &steps[3],
            &steps[4],
            [2, 9, 4, 1],
            "glyph atlas A #2 (scissor-clipped)",
        );
        confined(&steps[4], &steps[5], [4, 4, 8, 8], "scissored glow");
        // Pass 2's glow writes rows 10-11, inside its scissor and outside pass
        // 1's; a leaked pass-1 scissor would clip them.
        assert!(
            (10..12).any(|y| (4..12).any(|x| {
                let i = (y * W + x) * 4;
                steps[5][i..i + 4] != steps[4][i..i + 4]
            })),
            "the glow must write below y=10: pass 1's scissor does not apply to pass 2"
        );

        // The cover is total and visible, and the MID clear erased it.
        let cover_texel: [u8; 4] = steps[6][0..4].try_into().expect("4 bytes");
        assert_ne!(cover_texel, [0, 0, 0, 255], "the cover differs from black");
        assert!(
            steps[6]
                .as_chunks::<4>()
                .0
                .iter()
                .all(|t| *t == cover_texel),
            "the opaque cover quad must paint every aux texel"
        );
        let mid_texel: [u8; 4] = steps[7][(15 * W + 15) * 4..(15 * W + 15) * 4 + 4]
            .try_into()
            .expect("4 bytes");
        assert_ne!(
            mid_texel, cover_texel,
            "the MID clear's bytes must differ from the cover's, or erasure is invisible"
        );
        for (i, texel) in steps[7].as_chunks::<4>().0.iter().enumerate() {
            let (x, y) = (i % W, i / W);
            let inside = x >= usize::from(SMALL_RECT[0])
                && x < usize::from(SMALL_RECT[0] + SMALL_RECT[2])
                && y >= usize::from(SMALL_RECT[1])
                && y < usize::from(SMALL_RECT[1] + SMALL_RECT[3]);
            if !inside {
                assert_eq!(
                    *texel, mid_texel,
                    "({x},{y}): outside the small quad the MID clear must have \
                     erased the cover — a Clear encoded as Load lands exactly here"
                );
            }
        }
        assert_ne!(
            steps[7][(2 * W + 2) * 4..(2 * W + 2) * 4 + 4],
            mid_texel[..],
            "the small quad must be visible over the MID clear"
        );

        // ---- Encoder arm: one command buffer, two passes ---------------
        let sealed_offscreen = || {
            mint.texture_2d(
                PixelFormat::Rgba8Unorm,
                W,
                H,
                TEXTURE_USAGE_RENDER_TARGET
                    | TEXTURE_USAGE_SHADER_READ
                    | TEXTURE_USAGE_PIXEL_FORMAT_VIEW,
            )
            .expect("offscreen")
        };
        let enc_tex = sealed_offscreen();
        let enc_srgb = enc_tex
            .alias_view(PixelFormat::Rgba8UnormSrgb)
            .expect("view");
        let rb2 = dev.new_buffer(row * H).expect("readback 2");

        let aux_tex = sealed_offscreen();
        let aux_srgb = aux_tex
            .alias_view(PixelFormat::Rgba8UnormSrgb)
            .expect("view");
        let rb3 = dev.new_buffer(row * H).expect("readback 3");

        let mut cb = session.begin().expect("command buffer");
        {
            let pass = cb
                .render_pass(&RenderPassDesc {
                    target: &enc_srgb,
                    load: BLACK,
                    store: StoreAction::Store,
                    viewport: None,
                    scissor: Some(PASS1_SCISSOR),
                })
                .expect("pass 1");
            pass.set_vertex_buffer(&ubuf, vu)
                .expect("uniforms at the tabled slot");
            // DECOY one slot over (unread by the MSL): inert now, but an
            // ignored index or a transposed pair lets it clobber slot 0.
            pass.set_vertex_buffer(&wrong_ubuf, vu + 1)
                .expect("decoy one over");
            pass.set_pipeline(&bg);
            pass.set_instance_stream(&s_bg);
            pass.draw_instanced(quad6, 6, 2)
                .expect("armed draw: pipeline and stream are set in this fixture");
            pass.set_pipeline(&color_glyph); // switch 1
            pass.set_fragment_texture(atlas_a.obj(), atlas_tex_slot);
            pass.set_fragment_sampler(&sampler, atlas_samp_slot);
            pass.set_instance_stream(&s_glyph_a1);
            pass.draw_instanced(quad6, 6, 1)
                .expect("armed draw: pipeline and stream are set in this fixture");
            pass.set_pipeline(&cursor); // switch 2
            pass.set_instance_stream(&s_cursor);
            pass.draw_instanced(quad6, 6, 1)
                .expect("armed draw: pipeline and stream are set in this fixture");
            pass.set_pipeline(&color_glyph); // switch 3
            pass.set_fragment_texture(atlas_b.obj(), atlas_tex_slot); // atlas switch 1
            pass.set_fragment_texture(atlas_a.obj(), atlas_tex_slot + 1); // decoy pair, unread slot
            pass.set_fragment_sampler(&decoy_sampler, atlas_samp_slot + 1); // decoy pair
            pass.set_instance_stream(&s_glyph_b);
            pass.draw_instanced(quad6, 6, 1)
                .expect("armed draw: pipeline and stream are set in this fixture");
            pass.set_fragment_texture(atlas_a.obj(), atlas_tex_slot); // atlas switch 2
            pass.set_instance_stream(&s_glyph_a2);
            pass.draw_instanced(quad6, 6, 1)
                .expect("armed draw: pipeline and stream are set in this fixture");
        } // endEncoding
        {
            let pass = cb
                .render_pass(&RenderPassDesc {
                    target: &enc_tex,
                    load: LoadAction::Load,
                    store: StoreAction::Store,
                    viewport: None,
                    scissor: Some(GLOW_SCISSOR),
                })
                .expect("pass 2");
            pass.set_vertex_buffer(&ubuf, vu)
                .expect("uniforms at the tabled slot");
            pass.set_pipeline(&glow);
            pass.set_instance_stream(&s_glow);
            pass.draw_instanced(quad6, 6, 2)
                .expect("armed draw: pipeline and stream are set in this fixture");
        } // endEncoding
        // Passes 3 and 4: the Clear-erases proof, same command buffer.
        {
            let pass = cb
                .render_pass(&RenderPassDesc {
                    target: &aux_srgb,
                    load: BLACK,
                    store: StoreAction::Store,
                    viewport: None,
                    scissor: None,
                })
                .expect("pass 3");
            pass.set_vertex_buffer(&ubuf, vu)
                .expect("uniforms at the tabled slot");
            pass.set_pipeline(&bg);
            pass.set_instance_stream(&s_cover);
            pass.draw_instanced(quad6, 6, 1)
                .expect("armed draw: pipeline and stream are set in this fixture");
        } // endEncoding
        {
            let pass = cb
                .render_pass(&RenderPassDesc {
                    target: &aux_srgb,
                    load: MID_CLEAR,
                    store: StoreAction::Store,
                    viewport: None,
                    scissor: None,
                })
                .expect("pass 4");
            pass.set_vertex_buffer(&ubuf, vu)
                .expect("uniforms at the tabled slot");
            pass.set_pipeline(&bg);
            pass.set_instance_stream(&s_small);
            pass.draw_instanced(quad6, 6, 1)
                .expect("armed draw: pipeline and stream are set in this fixture");
        } // endEncoding
        cb.copy_texture_to_buffer(&enc_tex, W, H, &rb2, row)
            .expect("readback copy");
        cb.copy_texture_to_buffer(&aux_tex, W, H, &rb3, row)
            .expect("aux readback copy");
        let outcome = cb.commit().wait_outcome();
        assert_eq!(outcome, loss::CbOutcome::Completed, "the scene completes");
        assert!(!latch.is_lost());

        // SAFETY: shared storage; the command buffer completed above.
        let actual = unsafe { ffi::buffer_bytes(&rb2, row * H) };
        if actual != expected {
            let mut diffs = 0usize;
            let mut first = None;
            for (i, (e, a)) in expected
                .as_chunks::<4>()
                .0
                .iter()
                .zip(actual.as_chunks::<4>().0)
                .enumerate()
            {
                if e != a {
                    diffs += 1;
                    if first.is_none() {
                        first = Some((i % W, i / W, *e, *a));
                    }
                }
            }
            let (x, y, e, a) = first.expect("some texel differs");
            panic!(
                "THE ENCODER ARM IS NOT BYTE-IDENTICAL TO THE ONE-SHOT ARM: {diffs} of \
                 {} texels differ; first at ({x},{y}): one-shot {e:02x?} != encoder \
                 {a:02x?} — a dropped pipeline switch, a stale atlas bind, a stream at \
                 the wrong slot, a Load turned Clear or an ignored scissor lands here",
                W * H
            );
        }
        // SAFETY: shared storage; the command buffer completed above.
        let aux_actual = unsafe { ffi::buffer_bytes(&rb3, row * H) };
        assert_eq!(
            aux_actual, aux_expected,
            "THE AUX TARGET DIVERGED: a mid-command-buffer Clear that failed to \
             erase pass 3's cover (or an erased small quad) lands here"
        );
        crate::stderr_line!(
            "multi-pass proof on {}: 4 passes / 9 draws / 7 pipeline sets / 2 atlas \
             switches / decoy binds at slot 1 byte-identical to 8 one-shot draws \
             over 2x{} texels",
            dev.name(),
            W * H
        );
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
    /// raw bytes 1:1. Every source texel is unique, so origin mistakes land on
    /// a named texel.
    #[test]
    fn texture_copies_move_exactly_the_named_bytes() {
        let Some(dev) = device() else { return };
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

        // FULL copy: every texel of a sentinel-filled destination becomes src.
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

        // SUB-RECT copy: 5x6 from src (3,2) to dst (8,7).
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

        // Copying into the sRGB view of a Unorm texture moves raw bytes: the
        // Unorm base reads back the source's bytes.
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
    /// regions, incompatible format pairs, an understated readback stride and
    /// a short readback buffer, before any encoder exists, so the command buffer
    /// stays committable (the final commit proves it).
    #[test]
    fn the_copy_validators_refuse_what_metal_would_corrupt() {
        let Some(dev) = device() else { return };
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
            .copy_texture_to_texture(&t16, &f16)
            .expect_err("4-byte -> 8-byte texels");
        assert!(err.contains("copy-compatible"), "got: {err}");
        let err = cb
            .copy_texture_to_texture(&t16, &r8)
            .expect_err("4-byte -> 1-byte texels");
        assert!(err.contains("copy-compatible"), "got: {err}");

        // The stride-bug class, on THIS path: a 16-wide f16 row is 128 bytes,
        // and 16*4 understates it exactly the way the fixed blit bug did.
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

        // Every refusal above encoded NOTHING: the same command buffer still
        // commits and completes clean.
        assert_eq!(cb.commit().wait_outcome(), loss::CbOutcome::Completed);
        assert!(
            !latch.is_lost(),
            "refused copies are caller errors, not losses"
        );
    }

    /// Non-blocking completion via [`Submitted::try_outcome`]. Command buffer A
    /// carries a heavy fill and B commits behind it on the same queue:
    ///
    /// * the first poll of B is `None` and feeds nothing;
    /// * after `A.wait_outcome()`, polling B yields `Some(Completed)` with
    ///   exactly one feed;
    /// * B waited after A observes A terminal (commit order on one queue).
    #[test]
    fn try_outcome_polls_without_blocking_and_feeds_the_latch_once_terminal() {
        let Some(dev) = device() else { return };
        let _test_pool = AutoreleasePool::new();
        let latch = Arc::new(loss::LossLatch::new());
        let session = EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");

        const W: usize = 512;
        const H: usize = 512;
        let cell = pipelines::compile_library(&dev, crate::pipeline_table::ShaderLibrary::Cell)
            .expect("cell.metal");
        let bg_spec = Pipeline::Bg.spec();
        let bg = pipelines::build(&dev, &cell, bg_spec, PixelFormat::Bgra8Unorm).expect("bg");
        let mint = super::super::resources::MetalResourceDevice::new(&dev, Arc::clone(&latch));
        let target = mint
            .texture_2d(
                PixelFormat::Rgba8UnormSrgb,
                W,
                H,
                TEXTURE_USAGE_RENDER_TARGET,
            )
            .expect("target");
        #[expect(clippy::cast_precision_loss, reason = "512-texel extents")]
        let uniforms = CellUniforms {
            screen: [W as f32, H as f32],
            text_blend: 0.0,
            pad: 0.0,
        };
        // SAFETY: `repr(C)` into an exactly-sized fresh shared buffer.
        let ubuf = shared_buffer(&dev, unsafe { as_bytes(&uniforms) });
        let stream = shared_buffer(
            &dev,
            &bg_stream(&[([0, 0, W as u16, H as u16], [200, 40, 120, 255])]),
        );

        // A take proves the `None` poll only if this thread reached it while A
        // was still running; a take where preemption outlasted the fill is
        // drained and repeated. A blocking `try_outcome` fails every take.
        const TAKES: usize = 5;
        let mut polled = None;
        for _ in 0..TAKES {
            // A: the heavy submit.
            let mut cb_a = session.begin().expect("cb A");
            {
                let pass = cb_a
                    .render_pass(&RenderPassDesc {
                        target: &target,
                        load: BLACK,
                        store: StoreAction::Store,
                        viewport: None,
                        scissor: None,
                    })
                    .expect("heavy pass");
                let vu = bg_spec
                    .binds
                    .vertex_uniform
                    .expect("the bg row has a vertex uniform") as usize;
                pass.set_vertex_buffer(&ubuf, vu).expect("uniforms");
                pass.set_pipeline(&bg);
                pass.set_instance_stream(&stream);
                pass.draw_instanced(pipelines::metal_primitive_type(bg_spec.topology), 6, 4_000)
                    .expect("armed draw: pipeline and stream are set in this fixture");
            }
            let a = cb_a.commit();

            // B: empty, committed behind A on the same queue.
            let b = session.begin().expect("cb B").commit();

            #[cfg(debug_assertions)]
            let fed_before = latch.fed_count();
            if b.try_outcome().is_some() {
                assert_eq!(a.wait_outcome(), loss::CbOutcome::Completed, "A completes");
                continue;
            }
            #[cfg(debug_assertions)]
            assert_eq!(
                latch.fed_count(),
                fed_before,
                "a None poll must feed the latch NOTHING"
            );
            polled = Some((a, b));
            break;
        }
        let (a, b) = polled.expect(
            "B polled terminal at the first poll on every take — the heavy submit \
             finished before this thread could poll, which contradicts the \
             512x512 x 4000-instance fill, or `try_outcome` waits for the outcome",
        );

        assert_eq!(a.wait_outcome(), loss::CbOutcome::Completed, "A completes");

        // Poll B to completion, wall-clock bounded, never parking.
        #[cfg(debug_assertions)]
        let fed_before_b = latch.fed_count();
        let started = std::time::Instant::now();
        let outcome = loop {
            if let Some(o) = b.try_outcome() {
                break o;
            }
            // A hang detector: B's completion is the GPU's, and the loop
            // breaks the moment the status poll sees it.
            assert!(
                started.elapsed() < std::time::Duration::from_secs(60),
                "B never turned terminal — status polling is broken"
            );
            std::hint::spin_loop();
        };
        assert_eq!(outcome, loss::CbOutcome::Completed, "B completes behind A");
        #[cfg(debug_assertions)]
        assert_eq!(
            latch.fed_count(),
            fed_before_b + 1,
            "the ONE terminal poll must feed the latch exactly once — if this is \
             red, try_outcome stopped settling and a polled-only caller would \
             never latch a real loss"
        );

        // In-order: A was already terminal when B turned terminal.
        assert_eq!(
            a.try_outcome(),
            Some(loss::CbOutcome::Completed),
            "one queue commits in order: B terminal implies A terminal"
        );
        assert!(!latch.is_lost());
    }

    /// An injected `PageFault` through [`Submitted::settle`] latches, and every
    /// later [`EncodeSession::begin`] refuses naming the first loss, with no
    /// command-buffer mint ([`queue_ffi_census`]).
    #[test]
    fn an_injected_loss_refuses_further_encodes_fast_and_named() {
        let Some(dev) = device() else { return };
        let _test_pool = AutoreleasePool::new();
        let latch = Arc::new(loss::LossLatch::new());
        let session = EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");

        // Healthy first: an empty submit completes without latching, and the
        // census sees its mint.
        let minted = queue_ffi_census::calls();
        let s0 = session.begin().expect("healthy begin").commit();
        assert_eq!(
            queue_ffi_census::calls(),
            minted + 1,
            "a healthy begin mints exactly one command buffer"
        );
        assert_eq!(s0.wait_outcome(), loss::CbOutcome::Completed);
        assert!(!latch.is_lost());

        // Retryable passes through without closing the session.
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

        // The injected loss.
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
        let Some(dev) = device() else { return };
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

        // The first session still works after the refused construction.
        assert_eq!(
            session.begin().expect("cb").commit().wait_outcome(),
            loss::CbOutcome::Completed
        );

        // Drop frees the slot: the rebuild path constructs a successor.
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
        let Some(dev) = device() else { return };
        let _test_pool = AutoreleasePool::new();
        let latch = Arc::new(loss::LossLatch::new());
        let session = EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");

        // Wait first, then poll: the polls answer from the cache.
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

        // Poll first, then wait: the wait answers from the cache.
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

        // The injection seam stays UNCACHED: every settle feeds.
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
        let Some(dev) = device() else { return };
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

        // A pass onto a texture with no RENDER_TARGET usage is refused by
        // name, before any encoder exists.
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

        // Draw with no pipeline: refused by name, never reaches the driver.
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
            // The stream check fires first for the instanced form — either
            // named refusal is correct; the point is it never encodes.
            assert!(
                err.contains("no instance stream") || err.contains("no pipeline"),
                "got: {err}"
            );
        }

        // Instanced draw with a pipeline but NO stream: the silent-misdraw
        // class, refused by name. The vertex-id draws stay legal.
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
                    // `Bg` targets the sRGB view; on raw Unorm Metal would abort
                    // at `setRenderPipelineState:` before the guard under test.
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

        // `set_vertex_buffer` refuses the instance stream slot.
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

        // An overlapping same-texture copy is refused by name; a disjoint one
        // completes.
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
