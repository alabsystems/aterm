// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The renderer seam: the [`Rasterizer`] trait both the CPU renderer and
//! `aterm-gpu` implement, plus the [`Frame`] and per-frame input types they share.

use aterm_core::terminal::CursorStyle;

// The per-frame snapshot types live in `aterm-core` so the engine can build them.
pub use aterm_core::render::{
    CharFg, DecoBlend, DecoGlyph, FireHaloCell, FireMode, FirePatch, GlowBlend, GlowQuad, HaloMode,
    InkCell, RainHalo, RenderInput, SceneAtlas, SelectionClip, SpriteQuad, TrailCell,
    WordDecoration,
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

    /// Encode the rendered screen as an RGBA PNG (`read_image`, ATERM_DESIGN §8): the
    /// actual rendered pixels, not the engine's idea of the grid. Headless.
    #[must_use]
    pub fn to_png(&self) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = aterm_png::Encoder::new(&mut out, self.width as u32, self.height as u32);
            enc.set_color(aterm_png::ColorType::Rgba);
            enc.set_depth(aterm_png::BitDepth::Eight);
            // Frame RGB is non-linear sRGB; the sRGB chunk says so, so wide-gamut
            // viewers need not guess.
            enc.set_source_srgb(aterm_png::SrgbRenderingIntent::Perceptual);
            let mut w = enc.write_header().expect("png header");
            w.write_image_data(&self.rgba_bytes()).expect("png data");
        }
        out
    }
}

/// A rendered frame's pixels, borrowed or owned: the return of
/// [`Rasterizer::render_input_cached`]. Borrowing the renderer's cache leaves the
/// frontend's surface copy as the only per-frame framebuffer copy; renderers with no
/// borrowable cache (the GPU readback path) return `Owned`. The bytes match
/// [`Rasterizer::render_input`] either way.
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

    /// The packed `0xTTRRGGBB` pixels, row-major — borrowed in the `Borrowed`
    /// case (no copy), borrowed from the owned `Frame` otherwise. `TT` is
    /// transmittance (`0` opaque, `255` transparent), matching [`Frame`].
    #[must_use]
    #[inline]
    pub fn pixels(&self) -> &[u32] {
        match self {
            RenderView::Borrowed { pixels, .. } => pixels,
            RenderView::Owned(f) => &f.pixels,
        }
    }
}

/// The injected rasterizer interface, implemented by `aterm_render::Renderer` (CPU,
/// headless) and `aterm_gpu::GpuRenderer`. A frontend depends on this trait, not on a
/// concrete renderer. It never sees a `Terminal`: the engine produces the snapshot
/// ([`aterm_core::terminal::Terminal::cell_frame_into`]) and a renderer consumes only
/// [`RenderInput`].
pub trait Rasterizer {
    /// Pixel size of one cell, `(width, height)`.
    fn cell_size(&self) -> (usize, usize);

    /// Render from a pre-extracted, owned snapshot — the lock-free frame path.
    fn render_input(&mut self, input: &RenderInput) -> Frame;

    /// Render from a pre-extracted snapshot into a [`RenderView`], the per-frame
    /// presentation path: a renderer with a persistent pixel cache (the CPU damage
    /// cache) returns a borrow of it and skips the `Frame` clone. The default wraps
    /// [`Self::render_input`]'s owned `Frame`; the bytes are identical either way.
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

// The trait stays dyn-compatible: a frontend holds `Box<dyn Rasterizer>`.
const _: Option<&dyn Rasterizer> = None;

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
