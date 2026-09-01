// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE RESOURCE MINT — Metal-side resource creation behind a device handle
//! that stamps every texture with its LOSS DOMAIN.
//!
//! # Why this exists (the W1 judge's two-latch residual)
//!
//! W1 sealed the one-latch arrangement: [`super::encoder::EncodeSession`] owns
//! the process's render queue, `EncodeSession::new` binds the latch and
//! refuses a latch already bound, and `Frame::present` cross-checks the
//! session's latch against the swapchain's. The judged residual
//! (`docs/measured/metal-port-map-2026-08-31.md` §6): a caller who constructs
//! a SECOND `LossLatch` and a second `EncodeSession` could still render a
//! frame's texture on session 2 and present on session 1, because NOTHING tied
//! a render target to a session. Every runtime check stayed green while the
//! present fired for work committed on a queue the present never ordered
//! against.
//!
//! The structural seal (W3, map §5): textures are created HERE, through a
//! [`MetalResourceDevice`] that carries the `Arc<LossLatch>` naming the loss
//! domain, and the mint stamps each [`SealedTexture`] with that latch. The
//! encoder's write verbs (`render_pass`, the copies) and `Frame::present`
//! cross-check the stamp by `Arc::ptr_eq` — so a latch-2 texture crossing a
//! latch-1 session is a NAMED refusal at the earliest API crossing, and the
//! two-latch cross-wired present has no remaining shape: every path that
//! writes the drawable is stamped-checked before any FFI.
//!
//! Views mint through [`SealedTexture::alias_view`] and INHERIT the parent's
//! stamp — the Unorm/sRGB alias pair (the FORMAT LAW) is one texture in two
//! types, so it is one loss domain by construction.
//!
//! Deliberately NOT sealed: buffers and samplers ([`MetalResourceDevice::
//! buffer`]/[`MetalResourceDevice::sampler`] hand out plain [`Obj`]s). They
//! are never presented and never render-encoded as targets; binding one
//! cross-session is an ordering question for W4's encode port, not the
//! present-integrity hazard this mint closes.

use std::sync::Arc;

use super::ffi::{self, Device, MtlRegion, Obj, PixelFormat, SamplerDesc};
use super::loss::LossLatch;

/// The session/device handle Metal-side resource creation goes through — the
/// mint. Holds the device (retained) and the loss-domain latch it stamps into
/// every texture. Any number of mints may share one latch (they are stamps,
/// not queues); the ONE-queue exclusivity lives on
/// [`super::encoder::EncodeSession`], which binds the latch.
#[derive(Debug)]
pub(crate) struct MetalResourceDevice {
    dev: Device,
    latch: Arc<LossLatch>,
}

impl MetalResourceDevice {
    /// A mint for `latch`'s loss domain on `dev` (retained, not copied — the
    /// same `MTLDevice`).
    pub(crate) fn new(dev: &Device, latch: Arc<LossLatch>) -> Self {
        Self {
            dev: dev.clone_ref(),
            latch,
        }
    }

    /// The device this mint creates on.
    pub(crate) const fn device(&self) -> &Device {
        &self.dev
    }

    /// The loss domain this mint stamps.
    pub(crate) const fn latch(&self) -> &Arc<LossLatch> {
        &self.latch
    }

    /// A 2-D texture stamped with this mint's loss domain — the ONLY way a
    /// Metal texture enters the encoder's write verbs.
    pub(crate) fn texture_2d(
        &self,
        format: PixelFormat,
        width: usize,
        height: usize,
        usage: usize,
    ) -> Result<SealedTexture, String> {
        let obj = self
            .dev
            .new_texture_2d(format, width, height, usage)
            .ok_or_else(|| format!("MTLTexture allocation failed ({width}x{height} {format:?})"))?;
        Ok(SealedTexture {
            obj,
            latch: Arc::clone(&self.latch),
        })
    }

    /// A shared-storage buffer. Unsealed on purpose — see the module header.
    pub(crate) fn buffer(&self, len: usize) -> Result<Obj, String> {
        self.dev
            .new_buffer(len)
            .ok_or_else(|| format!("MTLBuffer allocation failed ({len} bytes)"))
    }

    /// A sampler state. Unsealed on purpose — see the module header.
    pub(crate) fn sampler(&self, desc: SamplerDesc) -> Result<Obj, String> {
        self.dev
            .new_sampler(desc)
            .ok_or_else(|| "MTLSamplerState allocation failed".to_owned())
    }
}

// THREADING: unlike the queue/encoder/layer types (thread-pinned raw-pointer
// holders, per the metal module convention), RESOURCES are the class Apple
// documents as thread-safe — `MTLBuffer`/`MTLTexture`/`MTLSamplerState`
// conform to `MTLResource`, may be used from multiple threads, and
// objc_retain/objc_release are themselves thread-safe. wgpu-hal's Metal
// backend rests its own `Send + Sync` resource types on the same contract,
// and the pre-W3 renderer moved these very textures/buffers across threads as
// wgpu objects. The device layer's routed structs (`VertexBuffer`,
// `ResidentAtlas`, `TrayOverlay`) live inside `GpuRenderer`, which crosses a
// thread at gui startup — so the sealed resource types declare exactly the
// Send/Sync the objects they wrap are documented to have, and nothing more
// (the ENCODING of work into them stays on the session's thread, checked by
// the stamp).

// SAFETY: `MTLTexture` is an `MTLResource` (thread-safe per Metal's threading
// model); `Arc<LossLatch>` is Send+Sync (atomics + OnceLock). See the
// THREADING note above.
unsafe impl Send for SealedTexture {}
// SAFETY: as for `Send` — the wrapped object is documented thread-safe and
// this type adds only the immutable stamp.
unsafe impl Sync for SealedTexture {}

/// A shared-storage `MTLBuffer` minted by [`MetalResourceDevice::buffer`],
/// wrapped so the device layer's buffer enum can carry it across threads
/// (see the THREADING note): the buffer OBJECT is thread-safe; coherent
/// access to its CONTENTS is the writer's documented wait-before-rewrite
/// discipline, unchanged from the raw `Obj` form.
#[derive(Debug)]
pub(crate) struct SharedBuffer(Obj);

// SAFETY: `MTLBuffer` is an `MTLResource` — see the THREADING note.
unsafe impl Send for SharedBuffer {}
// SAFETY: as for `Send`.
unsafe impl Sync for SharedBuffer {}

impl SharedBuffer {
    /// Wrap a minted buffer.
    pub(crate) const fn new(obj: Obj) -> Self {
        Self(obj)
    }

    /// The raw buffer for FFI (`buffer_write`, binds, readback length).
    pub(crate) const fn obj(&self) -> &Obj {
        &self.0
    }

    /// A second +1 handle to the SAME buffer ([`Obj::clone_retained`]) — the
    /// armed renderer's per-frame rig borrows nothing, so it clones handles.
    pub(crate) fn clone_handle(&self) -> Self {
        Self(self.0.clone_retained())
    }
}

/// A Metal texture (or a format-alias view of one) carrying the
/// `Arc<LossLatch>` of the loss domain it was minted in. The stamp is what the
/// encoder's write verbs and `Frame::present` cross-check, which is what makes
/// "render on session 2, present on session 1" a named refusal instead of a
/// green-across-every-check misorder.
///
/// No public constructor outside this module tree: production mints through
/// [`MetalResourceDevice`], the swapchain stamps its drawable with its own
/// latch, and views inherit. A texture with a forged stamp is therefore not
/// expressible without editing the metal module itself.
#[derive(Debug)]
pub(crate) struct SealedTexture {
    obj: Obj,
    latch: Arc<LossLatch>,
}

impl SealedTexture {
    /// Stamp an already-created texture object into `latch`'s domain — the
    /// swapchain's drawable path (the drawable is vended by CoreAnimation,
    /// not minted here, but its loss domain IS the swapchain's).
    pub(super) fn from_parts(obj: Obj, latch: Arc<LossLatch>) -> Self {
        Self { obj, latch }
    }

    /// The raw texture, for read-only FFI (binding as a fragment texture,
    /// property getters). The write verbs take `&SealedTexture` and check the
    /// stamp; handing this `Obj` to them is not possible by type.
    pub(crate) const fn obj(&self) -> &Obj {
        &self.obj
    }

    /// The loss-domain stamp.
    pub(crate) const fn latch(&self) -> &Arc<LossLatch> {
        &self.latch
    }

    /// A second +1 handle to the SAME texture, carrying the SAME loss-domain
    /// stamp — not a copy and not a re-mint, so it cannot launder a texture
    /// into another domain ([`Obj::clone_retained`]). The armed renderer's
    /// per-frame rig assembly clones the resident offscreen/atlas handles
    /// with this instead of borrowing (the rig owns its resolution set).
    pub(crate) fn clone_handle(&self) -> Self {
        Self {
            obj: self.obj.clone_retained(),
            latch: Arc::clone(&self.latch),
        }
    }

    /// `newTextureViewWithPixelFormat:` inheriting THIS texture's stamp — the
    /// Unorm/sRGB alias pair is one storage, one loss domain.
    pub(crate) fn alias_view(&self, format: PixelFormat) -> Option<SealedTexture> {
        ffi::texture_view(&self.obj, format).map(|obj| SealedTexture {
            obj,
            latch: Arc::clone(&self.latch),
        })
    }

    /// Upload CPU bytes into level 0 — [`ffi::texture_upload`] on the sealed
    /// object (uploads are CPU-synchronous `replaceRegion:`, no queue, so no
    /// stamp check is needed or possible here).
    ///
    /// # Safety
    /// As [`ffi::texture_upload`]: the texture's storage is not `Private`, its
    /// dimensions cover `region`, and `bytes` holds at least
    /// `bytes_per_row * region.size.height` bytes at that stride.
    pub(crate) unsafe fn upload(&self, region: MtlRegion, bytes: &[u8], bytes_per_row: usize) {
        // SAFETY: forwarded contract, pinned by the caller.
        unsafe { ffi::texture_upload(&self.obj, region, bytes, bytes_per_row) }
    }

    /// `-width`, off the live object.
    pub(crate) fn width(&self) -> usize {
        ffi::texture_width(&self.obj)
    }

    /// `-height`, off the live object.
    pub(crate) fn height(&self) -> usize {
        ffi::texture_height(&self.obj)
    }
}
