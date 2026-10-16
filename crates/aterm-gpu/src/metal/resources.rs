// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Metal resource creation behind a device handle that stamps every texture
//! with its loss domain.
//!
//! [`super::encoder::EncodeSession`] owns the render queue and binds a
//! `LossLatch`, and `Frame::present` cross-checks the session's latch against
//! the swapchain's. Without more, a caller with a second latch and session
//! could render a texture on one session and present on the other. So
//! textures are created here, through a [`MetalResourceDevice`] carrying the
//! `Arc<LossLatch>`, and each [`SealedTexture`] is stamped with it. The
//! encoder's write verbs (`render_pass`, copies) and `Frame::present` check
//! the stamp by `Arc::ptr_eq`, so a cross-domain texture is refused before
//! any FFI.
//!
//! Views from [`SealedTexture::alias_view`] inherit the parent's stamp (the
//! Unorm/sRGB alias pair is one texture, so one loss domain).
//!
//! Buffers and samplers ([`MetalResourceDevice::buffer`] /
//! [`MetalResourceDevice::sampler`]) are plain [`Obj`]s: they are never
//! presented or used as render targets.

use std::sync::Arc;

use super::ffi::{self, Device, MtlRegion, Obj, PixelFormat, SamplerDesc};
use super::loss::LossLatch;

/// The handle Metal resource creation goes through. Holds the retained device
/// and the loss-domain latch it stamps into every texture. Mints may share a
/// latch; queue exclusivity lives on [`super::encoder::EncodeSession`].
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
    #[cfg(test)]
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

// Threading: Metal resources (`MTLBuffer`/`MTLTexture`/`MTLSamplerState`,
// all `MTLResource`) are documented thread-safe, as are objc_retain/release;
// wgpu-hal relies on the same contract. The device layer's `VertexBuffer`,
// `ResidentAtlas` and `TrayOverlay` live in `GpuRenderer`, which crosses a
// thread at gui startup, so the sealed types declare exactly that Send/Sync.
// Encoding into them stays on the session's thread, checked by the stamp.

// SAFETY: `MTLTexture` is an `MTLResource` (thread-safe per Metal's threading
// model); `Arc<LossLatch>` is Send+Sync. See the threading note above.
unsafe impl Send for SealedTexture {}
// SAFETY: as for `Send` — the wrapped object is documented thread-safe and
// this type adds only the immutable stamp.
unsafe impl Sync for SealedTexture {}

/// A shared-storage `MTLBuffer` minted by [`MetalResourceDevice::buffer`],
/// wrapped so it can cross threads (see the threading note). The object is
/// thread-safe; coherent access to its contents relies on the writer waiting
/// before rewriting.
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

    /// The raw buffer, by value: a one-shot buffer the caller binds and drops
    /// on this thread.
    pub(crate) fn into_obj(self) -> Obj {
        self.0
    }
}

/// A Metal texture (or a format-alias view of one) carrying the
/// `Arc<LossLatch>` of the loss domain it was minted in, which the encoder's
/// write verbs and `Frame::present` cross-check.
///
/// Constructed only inside the metal module: via [`MetalResourceDevice`], the
/// swapchain's drawable stamp, or view inheritance, so a forged stamp is not
/// expressible.
#[derive(Debug)]
pub(crate) struct SealedTexture {
    obj: Obj,
    latch: Arc<LossLatch>,
}

impl SealedTexture {
    /// Stamp an existing texture into `latch`'s domain: the swapchain's
    /// drawable, vended by CoreAnimation but in the swapchain's loss domain.
    pub(super) fn from_parts(obj: Obj, latch: Arc<LossLatch>) -> Self {
        Self { obj, latch }
    }

    /// The raw texture for read-only FFI (binding, property getters). Write
    /// verbs take `&SealedTexture` and check the stamp.
    pub(crate) const fn obj(&self) -> &Obj {
        &self.obj
    }

    /// The loss-domain stamp.
    pub(crate) const fn latch(&self) -> &Arc<LossLatch> {
        &self.latch
    }

    /// A second +1 handle to the same texture with the same stamp
    /// ([`Obj::clone_retained`]), so it cannot move a texture into another
    /// domain. The armed renderer's per-frame rig clones resident handles
    /// with this.
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

    /// Upload CPU bytes into level 0 via [`ffi::texture_upload`]. Uploads are
    /// CPU-synchronous `replaceRegion:` with no queue, so no stamp check.
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
