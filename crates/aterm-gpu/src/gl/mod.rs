// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The first-party OpenGL ES 3.0 backend.

pub(crate) mod device;
pub(crate) mod encoder;
pub(crate) mod ffi;
pub(crate) mod pipelines;
pub(crate) mod resources;
pub(crate) mod shaders;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod glx;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod surface;

#[cfg(target_arch = "wasm32")]
pub(crate) mod canvas;

#[cfg(target_arch = "wasm32")]
pub(crate) use canvas::GlCanvasSurface as WindowSurface;
/// The swapchain a `GpuSurface` holds on the GL arm.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use surface::GlWindowSurface as WindowSurface;

/// A failed GL post and its frontend disposition.
#[derive(Debug)]
pub(crate) struct GlPresentError {
    pub(crate) kind: crate::SurfacePresentFailure,
    pub(crate) detail: String,
}

impl From<String> for GlPresentError {
    fn from(detail: String) -> Self {
        Self {
            kind: crate::SurfacePresentFailure::Validation,
            detail,
        }
    }
}

impl From<&str> for GlPresentError {
    fn from(detail: &str) -> Self {
        detail.to_owned().into()
    }
}

impl std::fmt::Display for GlPresentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

/// A headless device for a GL test.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn test_device() -> device::GlDevice {
    device::GlDevice::headless()
        .unwrap_or_else(|e| aterm_spec::host::could_not_run(format!("no GL device: {e}")))
}
