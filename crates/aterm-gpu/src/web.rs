// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The browser entry: a `<canvas>` (or a worker's `OffscreenCanvas`) becomes a
//! [`GpuContext`] plus a pending [`CanvasSurface`], with no backend type in
//! the signature. aterm-gpu-web names only these.
//!
//! The backend is the first-party GL arm over WebGL2 (`crate::gl`,
//! docs/AUDIT-third-party-2026-10-15.md, wasm-platform WP.8): the canvas
//! arrives as a JS object in the glue's table (aterm-wasm-abi's `js`), the
//! arm makes a WebGL2 context on it through the module's `aterm_gl` imports,
//! and every GL call goes through those imports. The context lives on the
//! canvas, so the device is made first and the surface configured on it
//! once the renderer knows the frame size
//! ([`crate::GpuRenderer::configure_canvas_surface`]).
//!
//! Synchronous: nothing in WebGL2 bring-up waits.

use crate::GpuContext;
use crate::gl::device::GlDevice;

aterm_wasm_abi::js_type! {
    /// A `<canvas>` element in the document.
    pub struct HtmlCanvasElement = "HTMLCanvasElement";
}

aterm_wasm_abi::js_type! {
    /// An `OffscreenCanvas`, transferred to a worker.
    pub struct OffscreenCanvas = "OffscreenCanvas";
}

/// Where the WebGL2 context lives: a document canvas, or a canvas transferred
/// to a worker so rendering and present run off the main thread.
#[derive(Debug)]
pub enum CanvasTarget {
    /// A `<canvas>` element in the document.
    Canvas(HtmlCanvasElement),
    /// An `OffscreenCanvas` transferred to a worker.
    Offscreen(OffscreenCanvas),
}

/// A canvas surface made with its context but not yet configured. The one
/// thing to do with it is [`crate::GpuRenderer::configure_canvas_surface`].
#[derive(Debug)]
pub struct CanvasSurface(pub(crate) GlDevice);

impl GpuContext {
    /// Acquire the GPU for `target`: a WebGL2 context on the canvas (the
    /// device), and the canvas as the pending surface. `Err` when the canvas
    /// gives no WebGL2 context (unsupported, or another context already on
    /// it), in the words of the step that failed.
    pub fn for_canvas(target: CanvasTarget) -> Result<(Self, CanvasSurface), String> {
        let canvas = match target {
            CanvasTarget::Canvas(c) => c.0,
            CanvasTarget::Offscreen(c) => c.0,
        };
        let dev = GlDevice::on_canvas(canvas)
            .map_err(|e| format!("create canvas surface failed: {e}"))?;
        let caps = dev.caps().clone();
        let ctx = Self {
            adapter_name: caps.renderer,
            backend: "Gl".to_owned(),
            srgb_offscreen: caps.srgb_views,
            device_lost: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            device_loss_msg: std::sync::Arc::default(),
            visual_swapchain: false,
            gl_max_texture_dim: caps.max_texture_dim,
            gl_canvas: dev.clone(),
        };
        Ok((ctx, CanvasSurface(dev)))
    }
}
