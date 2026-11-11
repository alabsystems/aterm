// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The renderer seam: the [`Rasterizer`] trait both the CPU renderer and
//! `aterm-gpu` implement, plus the [`Frame`] and per-frame input types they share.

use aterm_core::terminal::CursorStyle;

pub use aterm_core::render::{
    CharFg, DecoBlend, DecoGlyph, FireHaloCell, FireMode, FirePatch, GlowBlend, GlowEnd, GlowQuad,
    GlowRamp, GlowRamps, HaloMode, InkCell, Laid, RainHalo, RampMark, RenderInput, SceneAtlas,
    SelectionClip, TrailCell, WordDecoration, end_alpha, end_color, glow_end,
};

/// A row-major RGBA framebuffer packed as `0xTTRRGGBB` (`TT` is transmittance: `0` opaque, `255` transparent).
#[derive(Clone, Debug)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
}

impl Frame {
    /// The framebuffer as tightly packed straight-alpha RGBA bytes (4 per pixel,
    /// row-major), alpha `255 - transmittance`.
    #[must_use]
    pub fn rgba_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len() * 4);
        for &p in &self.pixels {
            out.push((p >> 16) as u8);
            out.push((p >> 8) as u8);
            out.push(p as u8);
            out.push(255 - (p >> 24) as u8);
        }
        out
    }

    /// Encode the rendered screen as an RGBA PNG.
    #[must_use]
    pub fn to_png(&self) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = aterm_png::Encoder::new(&mut out, self.width as u32, self.height as u32);
            enc.set_color(aterm_png::ColorType::Rgba);
            enc.set_depth(aterm_png::BitDepth::Eight);
            enc.set_source_srgb(aterm_png::SrgbRenderingIntent::Perceptual);
            let mut w = enc.write_header().expect("png header");
            w.write_image_data(&self.rgba_bytes()).expect("png data");
        }
        out
    }
}

/// A rendered frame's pixels, borrowed or owned, returned by [`Rasterizer::render_input_cached`].
pub enum RenderView<'a> {
    /// A borrow of the renderer's own framebuffer, valid until the renderer is next mutated.
    Borrowed {
        width: usize,
        height: usize,
        pixels: &'a [u32],
    },
    /// An owned frame.
    Owned(Frame),
}

impl RenderView<'_> {
    /// Frame width in pixels.
    #[must_use]
    #[inline]
    pub const fn width(&self) -> usize {
        match self {
            RenderView::Borrowed { width, .. } => *width,
            RenderView::Owned(f) => f.width,
        }
    }

    /// Frame height in pixels.
    #[must_use]
    #[inline]
    pub const fn height(&self) -> usize {
        match self {
            RenderView::Borrowed { height, .. } => *height,
            RenderView::Owned(f) => f.height,
        }
    }

    /// The packed `0xTTRRGGBB` pixels, row-major (`TT` is transmittance: `0` opaque, `255` transparent).
    #[must_use]
    #[inline]
    pub fn pixels(&self) -> &[u32] {
        match self {
            RenderView::Borrowed { pixels, .. } => pixels,
            RenderView::Owned(f) => &f.pixels,
        }
    }
}

/// Rasterizer interface implemented by CPU (`aterm_render::Renderer`) and GPU (`aterm_gpu::GpuRenderer`) renderers.
pub trait Rasterizer {
    /// Pixel size of one cell, `(width, height)`.
    fn cell_size(&self) -> (usize, usize);

    /// Render from a pre-extracted, owned snapshot — the lock-free frame path.
    fn render_input(&mut self, input: &RenderInput) -> Frame;

    /// Render from a pre-extracted snapshot into a borrowed or owned [`RenderView`].
    fn render_input_cached(&mut self, input: &RenderInput) -> RenderView<'_> {
        RenderView::Owned(self.render_input(input))
    }

    /// Push the cursor blink phase (`on` = solid) into the renderer's state.
    fn set_cursor_blink_phase(&mut self, on: bool);

    /// Override the rendered cursor style regardless of DECSCUSR (`None` clears the override).
    fn set_cursor_style_override(&mut self, style: Option<CursorStyle>);
}

const _: Option<&dyn Rasterizer> = None;

#[cfg(test)]
#[path = "render_api_tests.rs"]
mod tests;
