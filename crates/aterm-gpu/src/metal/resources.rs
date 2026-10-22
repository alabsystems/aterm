// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Metal resource creation behind a device handle that stamps every texture
//! with its loss domain.
//!
//! The encoder's write verbs (`render_pass`, the copies) refuse a
//! [`SealedTexture`] whose `Arc<LossLatch>` stamp is not their session's (by
//! `Arc::ptr_eq`), and `Frame::present` refuses a session whose latch is not
//! the swapchain's, so a texture cannot be rendered on one session and
//! presented on another. Alias views inherit the parent's stamp. Buffers and
//! samplers are plain [`Obj`]s: they are never presented or rendered to.

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
    #[inline]
    pub(crate) fn new(dev: &Device, latch: Arc<LossLatch>) -> Self {
        Self {
            dev: dev.clone_ref(),
            latch,
        }
    }

    /// The device this mint creates on.
    #[inline]
    pub(crate) const fn device(&self) -> &Device {
        &self.dev
    }

    /// A 2-D texture stamped with this mint's loss domain.
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
    #[inline]
    pub(crate) fn buffer(&self, len: usize) -> Result<Obj, String> {
        self.dev
            .new_buffer(len)
            .ok_or_else(|| format!("MTLBuffer allocation failed ({len} bytes)"))
    }

    /// A sampler state. Unsealed on purpose — see the module header.
    #[inline]
    pub(crate) fn sampler(&self, desc: SamplerDesc) -> Result<Obj, String> {
        self.dev
            .new_sampler(desc)
            .ok_or_else(|| "MTLSamplerState allocation failed".to_owned())
    }
}

// Threading: `MTLResource`s and objc_retain/release are documented thread-safe
// (wgpu-hal relies on the same contract). `GpuRenderer` holds these types and
// crosses a thread at gui startup, hence Send/Sync; encoding stays on the
// session's thread.

// SAFETY: `MTLTexture` is an `MTLResource` (thread-safe per Metal's threading
// model); `Arc<LossLatch>` is Send+Sync. See the threading note above.
unsafe impl Send for SealedTexture {}
// SAFETY: as for `Send` — the wrapped object is documented thread-safe and
// this type adds only the immutable stamp.
unsafe impl Sync for SealedTexture {}

/// A shared-storage `MTLBuffer` from [`MetalResourceDevice::buffer`], wrapped so
/// it can cross threads. Coherent access to its contents relies on the writer
/// waiting for the GPU before rewriting.
#[derive(Debug)]
pub(crate) struct SharedBuffer(Obj);

// SAFETY: `MTLBuffer` is an `MTLResource`; see the threading note above.
unsafe impl Send for SharedBuffer {}
// SAFETY: as for `Send`.
unsafe impl Sync for SharedBuffer {}

impl SharedBuffer {
    /// Wrap a minted buffer.
    #[inline]
    pub(crate) const fn new(obj: Obj) -> Self {
        Self(obj)
    }

    /// The raw buffer for FFI (`buffer_write`, binds, readback length).
    #[inline]
    pub(crate) const fn obj(&self) -> &Obj {
        &self.0
    }

    /// The raw buffer, by value, for a one-shot buffer used and dropped on this
    /// thread.
    #[inline]
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
    #[inline]
    pub(super) const fn from_parts(obj: Obj, latch: Arc<LossLatch>) -> Self {
        Self { obj, latch }
    }

    /// The raw texture for read-only FFI (binding, property getters). Write
    /// verbs take `&SealedTexture` and check the stamp.
    #[inline]
    pub(crate) const fn obj(&self) -> &Obj {
        &self.obj
    }

    /// The loss-domain stamp.
    #[inline]
    pub(crate) const fn latch(&self) -> &Arc<LossLatch> {
        &self.latch
    }

    /// A second +1 handle to the same texture with the same stamp
    /// ([`Obj::clone_retained`]).
    #[inline]
    pub(crate) fn clone_handle(&self) -> Self {
        Self {
            obj: self.obj.clone_retained(),
            latch: Arc::clone(&self.latch),
        }
    }

    /// `newTextureViewWithPixelFormat:` inheriting this texture's stamp: the
    /// Unorm/sRGB alias pair is one storage, one loss domain.
    #[inline]
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
    #[inline]
    pub(crate) unsafe fn upload(&self, region: MtlRegion, bytes: &[u8], bytes_per_row: usize) {
        // SAFETY: forwarded contract, pinned by the caller.
        unsafe { ffi::texture_upload(&self.obj, region, bytes, bytes_per_row) }
    }

    /// `-width`, off the live object.
    #[inline]
    pub(crate) fn width(&self) -> usize {
        ffi::texture_width(&self.obj)
    }

    /// `-height`, off the live object.
    #[inline]
    pub(crate) fn height(&self) -> usize {
        ffi::texture_height(&self.obj)
    }
}
