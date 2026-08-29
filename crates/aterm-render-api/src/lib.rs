// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Renderer-agnostic surface for aterm (ATERM_DESIGN WS-F: "an injected
//! `Rasterizer`").
//!
//! The design mandates that the rasterizer is a dependency-injected trait so the
//! headless CPU path and the GPU path are swappable behind one interface, instead
//! of one being baked into the frontend. This crate is that seam: the [`Rasterizer`]
//! trait plus the per-frame data types both implementations exchange ([`Frame`],
//! [`RenderInput`]). `aterm-render` (CPU) and `aterm-gpu` (Metal/wgpu) each
//! implement [`Rasterizer`]; a frontend can hold `Box<dyn Rasterizer>` and pick at
//! runtime. `aterm-render` re-exports `Frame`/`RenderInput` so existing call sites
//! are unchanged.

use aterm_core::terminal::CursorStyle;

// The render SNAPSHOT type lives in `aterm-core` (`aterm_core::render::RenderInput`,
// REARCH A-3) so the ENGINE can build it (`Terminal::cell_frame_into`) without a
// dependency cycle — `aterm-core` cannot depend on this crate. Re-exported here so
// every existing `aterm_render_api::RenderInput` / `aterm_render::RenderInput` call
// site is unchanged; this crate now consumes the value, never `&Terminal`.
pub use aterm_core::render::{
    CharFg, DecoBlend, DecoGlyph, FireHaloCell, FireMode, FirePatch, GlowBlend, GlowQuad, HaloMode,
    InkCell, RainHalo, RenderInput, SceneAtlas, SelectionClip, SpriteQuad, TrailCell, WordDecoration,
};

/// A row-major RGBA framebuffer packed as `0xTTRRGGBB`.
///
/// The top byte is *transmittance*, not alpha: `TT = 0` is opaque and
/// `TT = 255` is fully transparent. Byte-oriented RGBA consumers therefore use
/// `alpha = 255 - TT`; [`Frame::rgba_bytes`] and [`Frame::to_png`] perform that
/// conversion.
#[derive(Clone, Debug)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
}

impl Frame {
    /// The framebuffer as tightly packed RGB bytes (3 per pixel, row-major),
    /// intentionally discarding the packed transmittance byte.
    #[must_use]
    pub fn rgb_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len() * 3);
        for &p in &self.pixels {
            out.push((p >> 16) as u8);
            out.push((p >> 8) as u8);
            out.push(p as u8);
        }
        out
    }

    /// The framebuffer as tightly packed straight-alpha RGBA bytes (4 per
    /// pixel, row-major).
    ///
    /// Packed frame pixels store transmittance in their top byte, so the output
    /// alpha is `255 - transmittance`.
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

    /// Encode the rendered screen as a PNG — this is `read_image` (ATERM_DESIGN
    /// §8): an intelligence reads the ACTUAL rendered pixels, not the engine's
    /// idea of the grid. Translucent framebuffer pixels retain their converted
    /// alpha in the RGBA output. Headless; no display needed.
    #[must_use]
    pub fn to_png(&self) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = aterm_png::Encoder::new(&mut out, self.width as u32, self.height as u32);
            enc.set_color(aterm_png::ColorType::Rgba);
            enc.set_depth(aterm_png::BitDepth::Eight);
            // Frame RGB is canonical non-linear sRGB. Emit the sRGB chunk so
            // wide-gamut/HDR viewers never have to guess how `image` bytes
            // should be interpreted.
            enc.set_source_srgb(aterm_png::SrgbRenderingIntent::Perceptual);
            let mut w = enc.write_header().expect("png header");
            w.write_image_data(&self.rgba_bytes()).expect("png data");
        }
        out
    }
}

/// A rendered frame's pixels WITHOUT necessarily owning them — the return of
/// [`Rasterizer::render_input_cached`], the per-frame hot path that avoids
/// cloning the whole framebuffer when the renderer can hand back a borrow.
///
/// The CPU [`crate::Frame`]-producing path clones its persistent damage cache
/// into an owned `Frame` (an O(w·h) memcpy + allocation every frame). The
/// windowed frontend then copies THAT into the presentation surface — two full
/// framebuffer copies per frame. `RenderView` lets a renderer return a BORROW of
/// its already-rendered cache instead, so the frontend's surface copy is the
/// only one. Renderers with no borrowable cache (the GPU readback path) return
/// the `Owned` variant via the trait's default `render_input_cached`, so the
/// behavior there is unchanged.
///
/// Either way the bytes are byte-identical to [`Rasterizer::render_input`]; only
/// the ownership (and thus the elided per-frame clone) differs.
pub enum RenderView<'a> {
    /// A borrow of the renderer's own framebuffer (no per-frame clone/alloc):
    /// valid only until the renderer is next mutated.
    Borrowed {
        width: usize,
        height: usize,
        pixels: &'a [u32],
    },
    /// An owned frame (renderers without a borrowable cache, e.g. the GPU
    /// readback path, and the default trait impl).
    Owned(Frame),
}

impl RenderView<'_> {
    /// Frame width in pixels.
    #[must_use]
    pub fn width(&self) -> usize {
        match self {
            RenderView::Borrowed { width, .. } => *width,
            RenderView::Owned(f) => f.width,
        }
    }

    /// Frame height in pixels.
    #[must_use]
    pub fn height(&self) -> usize {
        match self {
            RenderView::Borrowed { height, .. } => *height,
            RenderView::Owned(f) => f.height,
        }
    }

    /// The packed `0xTTRRGGBB` pixels, row-major — borrowed in the `Borrowed`
    /// case (no copy), borrowed from the owned `Frame` otherwise. `TT` is
    /// transmittance (`0` opaque, `255` transparent), matching [`Frame`].
    #[must_use]
    pub fn pixels(&self) -> &[u32] {
        match self {
            RenderView::Borrowed { pixels, .. } => pixels,
            RenderView::Owned(f) => &f.pixels,
        }
    }
}

/// The injected rasterizer interface (ATERM_DESIGN WS-F). One trait, two
/// implementations: `aterm_render::Renderer` (CPU, headless) and
/// `aterm_gpu::GpuRenderer` (Metal/wgpu). A frontend depends on this trait, not
/// on a concrete renderer, so the rasterizer is chosen by injection.
///
/// As of REARCH A-3 the trait is `&Terminal`-free: the engine produces the
/// snapshot ([`aterm_core::terminal::Terminal::cell_frame_into`]) and a renderer
/// consumes only [`RenderInput`]. The old `render(&Terminal, ...)` method is gone.
pub trait Rasterizer {
    /// Pixel size of one cell, `(width, height)`.
    fn cell_size(&self) -> (usize, usize);

    /// Render from a pre-extracted, owned snapshot — the lock-free frame path.
    fn render_input(&mut self, input: &RenderInput) -> Frame;

    /// Render from a pre-extracted snapshot but return a [`RenderView`] — the
    /// per-frame PRESENTATION hot path, which only needs to copy the pixels into
    /// a surface, not own a `Frame`. A renderer that keeps its rendered pixels in
    /// a persistent cache (the CPU damage cache) returns a BORROW of that cache,
    /// eliding the per-frame `Frame` clone + allocation `render_input` would do.
    ///
    /// The default forwards to [`Self::render_input`] and wraps the owned `Frame`
    /// (no borrow available, e.g. the GPU readback path) — so this is a strict
    /// superset of `render_input` and is always safe to call. The bytes are
    /// byte-identical to `render_input`.
    fn render_input_cached(&mut self, input: &RenderInput) -> RenderView<'_> {
        RenderView::Owned(self.render_input(input))
    }

    /// Push the cursor blink phase (`on` = solid) into the renderer's own state;
    /// applied at the next `render_input`. The frontend owns the blink clock.
    fn set_cursor_blink_phase(&mut self, on: bool);

    /// Override the rendered cursor style regardless of DECSCUSR (e.g.
    /// `HollowBlock` while the window is unfocused); `None` clears the override.
    fn set_cursor_style_override(&mut self, style: Option<CursorStyle>);
}

#[cfg(test)]
mod tests {
    use super::Frame;

    fn alpha_fixture() -> Frame {
        Frame {
            width: 3,
            height: 1,
            pixels: vec![0x0011_2233, 0x7f44_5566, 0xff77_8899],
        }
    }

    #[test]
    fn rgba_bytes_inverts_packed_transmittance() {
        assert_eq!(
            alpha_fixture().rgba_bytes(),
            [
                0x11, 0x22, 0x33, 0xff, // opaque
                0x44, 0x55, 0x66, 0x80, // half-transmittance
                0x77, 0x88, 0x99, 0x00, // fully transparent
            ]
        );
    }

    #[test]
    fn png_preserves_converted_frame_alpha() {
        let frame = alpha_fixture();
        let png = frame.to_png();
        let mut reader = aterm_png::Decoder::new(std::io::Cursor::new(png))
            .read_info()
            .expect("decode header");
        assert_eq!(reader.info().color_type, aterm_png::ColorType::Rgba);
        assert_eq!(
            reader.info().srgb,
            Some(aterm_png::SrgbRenderingIntent::Perceptual),
            "renderer PNGs carry explicit sRGB metadata"
        );
        let mut decoded = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut decoded).expect("decode pixels");
        assert_eq!(
            &decoded[..info.buffer_size()],
            frame.rgba_bytes().as_slice()
        );
    }
}
