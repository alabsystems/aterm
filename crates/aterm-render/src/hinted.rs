// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Grid-fitted (hinted) glyph rasterization for the native non-macOS raster paths (Linux and Windows).

use aterm_font::autohint::{HintingInstance, Target};

/// How the native (Linux / Windows) raster path grid-fits outlines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum HintMode {
    /// FreeType-ported autohinter, normal target: snaps stems and horizontal features in both axes, the crispest antialiased result and the default.
    #[default]
    Full,
    /// Autohinter, light target: vertical-only grid fit (the common desktop Linux `hintslight` look) — crossbars/tops snap, stem phases keep the designed fractional positions.
    Light,
    /// No grid fitting: the unhinted path, bit-for-bit.
    Off,
}

impl HintMode {
    /// Parse one mode spelling of the config key (`font_hinting`, via `Renderer::set_font_hinting`).
    pub(crate) fn parse(s: &str) -> Self {
        match s.trim() {
            "light" | "native" => Self::Light,
            "off" | "0" | "none" | "false" => Self::Off,
            _ => Self::Full,
        }
    }

    /// Whether the `ATERM_RASTERIZER` development seam pins the raster backend to the byte-stable portable path the golden/parity tests use ([`crate::portable_raster_forced`]).
    #[inline]
    pub(crate) fn portable_forced() -> bool {
        crate::portable_raster_forced()
    }

    /// The mode a renderer is built with: [`HintMode::Full`], or `Off` under [`Self::portable_forced`].
    #[inline]
    pub(crate) fn initial() -> Self {
        if Self::portable_forced() {
            Self::Off
        } else {
            Self::Full
        }
    }

    /// The hinter target this mode stands for; `None` = hinting disabled. Horizontal fitting
    /// inside the cell is wanted (that is the crispness); advances reported upstream stay linear
    /// regardless.
    #[inline]
    const fn options(self) -> Option<Target> {
        match self {
            Self::Full => Some(Target::Normal),
            Self::Light => Some(Target::Light),
            Self::Off => None,
        }
    }
}

/// Per-face, size and instance [`HintingInstance`] cache, the hinter's twin of the CoreText `ct_cache`.
#[derive(Default)]
pub(crate) struct HintBank {
    map: aterm_hash::FxHashMap<(usize, u32, u32, u8), Option<std::sync::Arc<HintingInstance>>>,
}

impl HintBank {
    /// Fetch (or build and memoize) the hinting instance for one face at `px`, at the variation `coords` discriminated by `slot`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn instance(
        &mut self,
        key_ptr: usize,
        bytes: &[u8],
        index: u32,
        px: f32,
        mode: HintMode,
        coords: &[(u32, f32)],
        slot: u8,
    ) -> Option<std::sync::Arc<HintingInstance>> {
        self.instance_with(key_ptr, bytes, index, px, mode.options(), coords, slot)
    }

    /// [`Self::instance`] with the hinter target handed in directly: the seam the subpixel raster ([`crate::subpixel`]) uses to memoize LCD-target instances in its own bank (the map key does not carry the target, so one bank must never hold two targets for one face and px).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn instance_with(
        &mut self,
        key_ptr: usize,
        bytes: &[u8],
        index: u32,
        px: f32,
        target: Option<Target>,
        coords: &[(u32, f32)],
        slot: u8,
    ) -> Option<std::sync::Arc<HintingInstance>> {
        let target = target?;
        if !px.is_finite() || px <= 0.0 {
            return None;
        }
        self.map
            .entry((key_ptr, index, px.to_bits(), slot))
            .or_insert_with(|| {
                HintingInstance::new(bytes, index, px, coords, target).map(std::sync::Arc::new)
            })
            .clone()
    }

    /// Drop every memoized instance (heavy-teardown twin of `ct_cache.clear()` in `set_px`: instances are px-keyed, so this reclaims dead sizes).
    pub(crate) fn clear(&mut self) {
        self.map.clear();
    }
}

/// Map a char through the face's Unicode cmap with aterm-font's `glyph_index`, as production does through the renderer's memoized `primary_unicode_gid`/`unicode_gid`.
#[cfg(all(test, feature = "embedded-font"))]
pub(crate) fn unicode_gid(bytes: &[u8], index: u32, ch: char) -> Option<u16> {
    let face = aterm_font::Face::parse(bytes, index).ok()?;
    face.glyph_index(ch).map(|g| g.0).filter(|&g| g != 0)
}

/// Rasterize glyph `gid` of the face at `px` with `hint` applied, as the tuple `(width, height, xmin, ymin, advance, coverage)` the raster path expects (the conventions of [`variation::varied_glyph_raster`](crate::variation::varied_glyph_raster)).
pub(crate) fn hinted_glyph_raster(
    bytes: &[u8],
    index: u32,
    gid: u16,
    px: f32,
    hint: &HintingInstance,
    coords: &[(u32, f32)],
) -> Option<(usize, usize, i32, i32, f32, Vec<u8>)> {
    let advance =
        crate::variation::VariedFace::parse(bytes, index, coords)?.linear_advance(gid, px)?;
    let mut pen = PathPen::default();
    hint.draw(bytes, index, gid, &mut pen)?;
    if pen.is_blank() {
        // No outline at all (space and friends): blank raster, advance only —
        // the blank-glyph convention of the variation path.
        return Some((0, 0, 0, 0, advance, Vec::new()));
    }
    let x_min = pen.min_x.floor();
    let x_max = pen.max_x.ceil();
    let y_min = pen.min_y.floor();
    let y_max = pen.max_y.ceil();
    let (w, h) = ((x_max - x_min) as i32, (y_max - y_min) as i32);
    if w <= 0 || h <= 0 || w > 4096 || h > 4096 {
        return None;
    }
    let (w, h) = (w as usize, h as usize);
    let pad = crate::variation::RASTER_PAD as f32;
    let mut ras = crate::raster::Rasterizer::new(
        w + 2 * crate::variation::RASTER_PAD,
        h + 2 * crate::variation::RASTER_PAD,
    );
    pen.fill(&mut ras, x_min - pad, y_max + pad, 1.0);
    let cov = crate::variation::crop_padded_coverage(&ras, w, h);
    Some((w, h, x_min as i32, y_min as i32, advance, cov))
}

/// One recorded outline segment, in hinted pixel space (y up). Recorded rather than streamed
/// because the ink box must be known before the rasterizer (and its y flip) can exist.
enum Cmd {
    Move(f32, f32),
    Line(f32, f32),
    Quad(f32, f32, f32, f32),
    Curve(f32, f32, f32, f32, f32, f32),
    Close,
}

/// Collects an outline (pixels, y up) and its ink bounds, then replays it into a [`crate::raster`] grid (pixels, y down, origin at the ink box's top-left): the same mapping as `variation::OutlineToRaster`, minus the design-unit scale (callers record pixel coordinates).
#[derive(Default)]
pub(crate) struct PathPen {
    cmds: Vec<Cmd>,
    pub(crate) min_x: f32,
    pub(crate) min_y: f32,
    pub(crate) max_x: f32,
    pub(crate) max_y: f32,
}

impl PathPen {
    #[inline]
    pub(crate) fn is_blank(&self) -> bool {
        self.cmds.is_empty() || !self.min_x.is_finite()
    }

    #[inline]
    fn see(&mut self, x: f32, y: f32) {
        if self.cmds.is_empty() {
            (self.min_x, self.max_x) = (x, x);
            (self.min_y, self.max_y) = (y, y);
        } else {
            self.min_x = self.min_x.min(x);
            self.min_y = self.min_y.min(y);
            self.max_x = self.max_x.max(x);
            self.max_y = self.max_y.max(y);
        }
    }

    /// Replay into `ras`, flipping y about `y_max` and translating by `x_min`, with x scaled by `xs` after the translate (`1.0` is an exact identity; `3.0` is the subpixel raster's horizontal oversample).
    pub(crate) fn fill(
        &self,
        ras: &mut crate::raster::Rasterizer,
        x_min: f32,
        y_max: f32,
        xs: f32,
    ) {
        let map = |x: f32, y: f32| crate::raster::point((x - x_min) * xs, y_max - y);
        let mut last = crate::raster::point(0.0, 0.0);
        let mut start = last;
        let close = |ras: &mut crate::raster::Rasterizer,
                     last: &mut crate::raster::Point,
                     start: crate::raster::Point| {
            if *last != start {
                ras.draw_line(*last, start);
                *last = start;
            }
        };
        for c in &self.cmds {
            match *c {
                Cmd::Move(x, y) => {
                    close(ras, &mut last, start);
                    last = map(x, y);
                    start = last;
                }
                Cmd::Line(x, y) => {
                    let p = map(x, y);
                    ras.draw_line(last, p);
                    last = p;
                }
                Cmd::Quad(cx, cy, x, y) => {
                    let c1 = map(cx, cy);
                    let p = map(x, y);
                    ras.draw_quad(last, c1, p);
                    last = p;
                }
                Cmd::Curve(c0x, c0y, c1x, c1y, x, y) => {
                    let a = map(c0x, c0y);
                    let b = map(c1x, c1y);
                    let p = map(x, y);
                    ras.draw_cubic(last, a, b, p);
                    last = p;
                }
                Cmd::Close => close(ras, &mut last, start),
            }
        }
        close(ras, &mut last, start);
    }
}

/// The recording half (pixels, y up), as plain methods so both the `OutlineSink` impl below
/// and the subpixel raster's unhinted outline can feed it.
impl PathPen {
    #[inline]
    pub(crate) fn move_to(&mut self, x: f32, y: f32) {
        self.see(x, y);
        self.cmds.push(Cmd::Move(x, y));
    }
    #[inline]
    pub(crate) fn line_to(&mut self, x: f32, y: f32) {
        self.see(x, y);
        self.cmds.push(Cmd::Line(x, y));
    }
    #[inline]
    pub(crate) fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.see(cx, cy);
        self.see(x, y);
        self.cmds.push(Cmd::Quad(cx, cy, x, y));
    }
    #[inline]
    pub(crate) fn curve_to(&mut self, c0x: f32, c0y: f32, c1x: f32, c1y: f32, x: f32, y: f32) {
        self.see(c0x, c0y);
        self.see(c1x, c1y);
        self.see(x, y);
        self.cmds.push(Cmd::Curve(c0x, c0y, c1x, c1y, x, y));
    }
    #[inline]
    pub(crate) fn close(&mut self) {
        self.cmds.push(Cmd::Close);
    }
}

impl aterm_font::OutlineSink for PathPen {
    fn move_to(&mut self, x: f32, y: f32) {
        PathPen::move_to(self, x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        PathPen::line_to(self, x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        PathPen::quad_to(self, x1, y1, x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        PathPen::curve_to(self, x1, y1, x2, y2, x, y);
    }
    fn close(&mut self) {
        PathPen::close(self);
    }
}

#[cfg(all(test, feature = "embedded-font"))]
#[path = "hinted_tests.rs"]
mod tests;
