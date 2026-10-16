// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The browser entry: a `<canvas>` (or a worker's `OffscreenCanvas`) becomes a
//! [`GpuContext`] plus a pending [`CanvasSurface`], with no backend type in
//! the signature. aterm-gpu-web names only these, so the backend under them
//! (wgpu's WebGL2 today, the first-party WebGL2 arm at
//! docs/AUDIT-third-party-2026-10-15.md GW.4) changes without touching it.
//!
//! WebGL can enumerate an adapter only against the canvas (the GL context
//! lives on it), so the surface is created first, the context second, and the
//! surface configured last, once the renderer knows the frame size
//! ([`crate::GpuRenderer::configure_canvas_surface`]).

use crate::GpuContext;

/// The two canvas element types the browser entry accepts, re-exported so a
/// caller names no backend crate for them.
pub use wgpu::web_sys::{HtmlCanvasElement, OffscreenCanvas};

/// Where the WebGL2 context lives: a document canvas, or a canvas transferred
/// to a worker so rendering and present run off the main thread.
#[derive(Debug)]
pub enum CanvasTarget {
    /// A `<canvas>` element in the document.
    Canvas(HtmlCanvasElement),
    /// An `OffscreenCanvas` transferred to a worker.
    Offscreen(OffscreenCanvas),
}

/// A canvas surface created against its context but not yet configured. The
/// one thing to do with it is [`crate::GpuRenderer::configure_canvas_surface`].
pub struct CanvasSurface(pub(crate) wgpu::Surface<'static>);

impl std::fmt::Debug for CanvasSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CanvasSurface")
    }
}

/// An empty `RawDisplayHandle::Web` provider. wgpu requires the instance to
/// carry a display handle before `create_surface()`; the WebGL backend reads
/// the canvas from the window handle and ignores this one.
#[derive(Debug)]
struct WebDisplay;

impl wgpu::rwh::HasDisplayHandle for WebDisplay {
    fn display_handle(&self) -> Result<wgpu::rwh::DisplayHandle<'_>, wgpu::rwh::HandleError> {
        let raw = wgpu::rwh::RawDisplayHandle::Web(wgpu::rwh::WebDisplayHandle::new());
        // SAFETY: the Web display handle is an empty marker (no borrowed data),
        // so a 'static borrow is sound — there is nothing for it to outlive.
        Ok(unsafe { wgpu::rwh::DisplayHandle::borrow_raw(raw) })
    }
}

impl GpuContext {
    /// Acquire the GPU for `target`: a GL-backend instance (the only backend in
    /// the wasm build), the canvas surface, then adapter and device with that
    /// surface as the compatibility target. Errors name the step that failed.
    pub async fn for_canvas(target: CanvasTarget) -> Result<(Self, CanvasSurface), String> {
        // `create_surface()` requires the instance to carry some display
        // handle; WebGL ignores it, so an empty `RawDisplayHandle::Web` marker
        // satisfies the check.
        let instance = wgpu::Instance::new(
            wgpu::InstanceDescriptor {
                backends: wgpu::Backends::GL,
                ..wgpu::InstanceDescriptor::new_without_display_handle()
            }
            .with_display_handle(Box::new(WebDisplay)),
        );
        let target = match target {
            CanvasTarget::Canvas(c) => wgpu::SurfaceTarget::Canvas(c),
            CanvasTarget::Offscreen(c) => wgpu::SurfaceTarget::OffscreenCanvas(c),
        };
        let surface = instance
            .create_surface(target)
            .map_err(|e| format!("create canvas surface failed: {e}"))?;
        let ctx = Self::from_instance_with_surface(instance, Some(&surface))
            .await
            .map_err(|e| format!("WebGL adapter/device init failed: {e}"))?;
        Ok((ctx, CanvasSurface(surface)))
    }
}
