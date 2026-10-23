// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Browser WebGL2 entry producing a [`GpuContext`] and pending [`CanvasSurface`]
//! ([`crate::GpuRenderer::configure_canvas_surface`]).

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

/// Where the WebGL2 context lives.
#[derive(Debug)]
pub enum CanvasTarget {
    /// A `<canvas>` element in the document.
    Canvas(HtmlCanvasElement),
    /// An `OffscreenCanvas` transferred to a worker.
    Offscreen(OffscreenCanvas),
}

/// A canvas surface made with its context but not yet configured.
#[derive(Debug)]
pub struct CanvasSurface(pub(crate) GlDevice);

impl GpuContext {
    /// Acquire the GPU for `target`.
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
