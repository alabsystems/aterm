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

    /// The hinter target this mode stands for; `None` = hinting disabled.
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

/// One recorded outline segment, in hinted pixel space (y up).
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
mod tests {
    use super::*;

    /// The `wght` axis tag, in the same big-endian `u32` spelling the renderer
    /// stores coords in ([`crate::variation::WGHT_TAG`]).
    const WGHT: u32 = u32::from_be_bytes(*b"wght");

    /// The embedded DejaVu Sans Mono (the default `embedded-font` feature), so these tests
    /// never depend on system font files.
    fn dejavu() -> &'static [u8] {
        crate::embedded_font()
    }

    fn full_instance(px: f32) -> HintingInstance {
        HintingInstance::new(dejavu(), 0, px, &[], HintMode::Full.options().unwrap()).unwrap()
    }

    /// Per-row peak coverage ≥ `t` — a row whose brightest texel is near-full
    /// renders a solid (not washed) stroke crossing there.
    fn rows_with_peak(cov: &[u8], w: usize, h: usize, t: u8) -> usize {
        (0..h)
            .filter(|&r| (0..w).any(|c| cov[r * w + c] >= t))
            .count()
    }

    /// At 12px the grid-fitted 'l' has a near-saturated stem texel on nearly every row, where
    /// the unhinted raster's fractional stem phase splits the stem across two columns.
    #[test]
    fn full_hinting_snaps_the_l_stem_at_12px() {
        let hint = full_instance(12.0);
        let gid = unicode_gid(dejavu(), 0, 'l').unwrap();
        let (w, h, _, _, _, cov) = hinted_glyph_raster(dejavu(), 0, gid, 12.0, &hint, &[]).unwrap();
        let hinted_rows = rows_with_peak(&cov, w, h, 240);
        let fd =
            crate::font::Font::from_bytes(dejavu(), crate::font::FontSettings::default()).unwrap();
        let (m, b) = fd.rasterize('l', 12.0);
        let fontdue_rows = rows_with_peak(&b, m.width, m.height, 240);
        assert!(
            hinted_rows >= h.saturating_sub(2),
            "hinted 'l' at 12px must peak >=240 on nearly every row ({hinted_rows}/{h} rows: {cov:?})"
        );
        assert!(
            hinted_rows > fontdue_rows,
            "grid fitting must strictly beat the unhinted raster \
             (hinted {hinted_rows}/{h} rows vs fontdue {fontdue_rows}/{})",
            m.height
        );
    }

    /// The reported advance is linear (scaled hmtx), not the hinted advance, and bit-identical
    /// to the unhinted `Font`'s: cell geometry must not move with hinting.
    #[test]
    fn advance_stays_linear() {
        let jetbrains: &[u8] = include_bytes!("../tests/fixtures/jetbrains-mono.ttf");
        for bytes in [dejavu(), jetbrains] {
            let font =
                crate::font::Font::from_bytes(bytes, crate::font::FontSettings::default()).unwrap();
            for px in [12.0, 13.0] {
                let hint =
                    HintingInstance::new(bytes, 0, px, &[], HintMode::Full.options().unwrap())
                        .unwrap();
                for ch in ['M', 'i', 'W', '0'] {
                    let gid = unicode_gid(bytes, 0, ch).unwrap();
                    let (.., adv, _) = hinted_glyph_raster(bytes, 0, gid, px, &hint, &[]).unwrap();
                    let unhinted = font.metrics(ch, px).advance_width;
                    assert_eq!(
                        adv.to_bits(),
                        unhinted.to_bits(),
                        "{ch:?} @{px}px: hinted-path advance {adv} != unhinted advance {unhinted}"
                    );
                }
            }
        }
    }

    /// A space has no outline: blank raster carrying just the advance (the
    /// variation path's blank-glyph convention, which the blit treats as a
    /// no-op).
    #[test]
    fn space_is_blank_with_advance() {
        let hint = full_instance(12.0);
        let gid = unicode_gid(dejavu(), 0, ' ').unwrap();
        let (w, h, xmin, ymin, adv, cov) =
            hinted_glyph_raster(dejavu(), 0, gid, 12.0, &hint, &[]).unwrap();
        assert_eq!((w, h, xmin, ymin), (0, 0, 0, 0));
        assert!(cov.is_empty());
        assert!(adv > 0.0);
    }

    /// `HintMode::Off` yields no options, so the bank hands out no instance and the unhinted
    /// path runs bit-for-bit.
    #[test]
    fn off_mode_disables_the_bank() {
        let mut bank = HintBank::default();
        let bytes = dejavu();
        let key = bytes.as_ptr() as usize;
        assert!(
            bank.instance(key, bytes, 0, 12.0, HintMode::Off, &[], 0)
                .is_none()
        );
        assert!(
            bank.instance(key, bytes, 0, 12.0, HintMode::Full, &[], 0)
                .is_some()
        );
    }

    /// Re-fill the same recorded outline with a 4 px slack ring (four times production's) and
    /// crop the ink box back out.
    fn generous_slack_raster(pen: &PathPen, w: usize, h: usize) -> Vec<u8> {
        const SLACK: usize = 4;
        let (x_min, y_max) = (pen.min_x.floor(), pen.max_y.ceil());
        let mut ras = crate::raster::Rasterizer::new(w + 2 * SLACK, h + 2 * SLACK);
        pen.fill(&mut ras, x_min - SLACK as f32, y_max + SLACK as f32, 1.0);
        let gw = w + 2 * SLACK;
        let mut cov = vec![0u8; w * h];
        ras.for_each_pixel(|i, a| {
            let (gx, gy) = (i % gw, i / gw);
            let (Some(x), Some(y)) = (gx.checked_sub(SLACK), gy.checked_sub(SLACK)) else {
                return;
            };
            if x < w && y < h {
                cov[y * w + x] = (a * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
            }
        });
        cov
    }

    /// `(area, perimeter)` of the fitted outline in px² / px, by the shoelace
    /// formula over a fine flattening.
    fn area_and_perimeter(pen: &PathPen) -> (f32, f32) {
        const SUB: usize = 32;
        let (mut area, mut perim) = (0.0f32, 0.0f32);
        let mut poly: Vec<(f32, f32)> = Vec::new();
        let (mut start, mut last) = ((0.0f32, 0.0f32), (0.0f32, 0.0f32));
        let close = |poly: &mut Vec<(f32, f32)>, area: &mut f32, perim: &mut f32| {
            for i in 0..poly.len() {
                let ((x0, y0), (x1, y1)) = (poly[i], poly[(i + 1) % poly.len()]);
                *area += x0 * y1 - x1 * y0;
                *perim += ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
            }
            poly.clear();
        };
        for c in &pen.cmds {
            match *c {
                Cmd::Move(x, y) => {
                    close(&mut poly, &mut area, &mut perim);
                    start = (x, y);
                    last = start;
                    poly.push(start);
                }
                Cmd::Line(x, y) => {
                    last = (x, y);
                    poly.push(last);
                }
                Cmd::Quad(cx, cy, x, y) => {
                    for i in 1..=SUB {
                        let t = i as f32 / SUB as f32;
                        let m = 1.0 - t;
                        poly.push((
                            m * m * last.0 + 2.0 * m * t * cx + t * t * x,
                            m * m * last.1 + 2.0 * m * t * cy + t * t * y,
                        ));
                    }
                    last = (x, y);
                }
                Cmd::Curve(ax, ay, bx, by, x, y) => {
                    for i in 1..=SUB {
                        let t = i as f32 / SUB as f32;
                        let m = 1.0 - t;
                        let (m2, t2) = (m * m, t * t);
                        poly.push((
                            m2 * m * last.0 + 3.0 * m2 * t * ax + 3.0 * m * t2 * bx + t2 * t * x,
                            m2 * m * last.1 + 3.0 * m2 * t * ay + 3.0 * m * t2 * by + t2 * t * y,
                        ));
                    }
                    last = (x, y);
                }
                Cmd::Close => {
                    close(&mut poly, &mut area, &mut perim);
                    last = start;
                    poly.push(start);
                }
            }
        }
        close(&mut poly, &mut area, &mut perim);
        ((area * 0.5).abs(), perim)
    }

    /// The ppem sweep's glyphs: printable ASCII, the box-drawing and block run, a Nerd Font PUA
    /// sample (all the bundled symbols face carries), and a CJK sample for a face that has one.
    fn sweep_chars() -> Vec<char> {
        let mut v: Vec<char> = (' '..='~').collect();
        v.extend("─│┌┐└┘├┤┬┴┼━┃█▀▄▌▐░▒▓".chars());
        v.extend("\u{e0b0}\u{e0b2}\u{e5ff}\u{e62b}\u{e706}\u{e7a8}".chars());
        v.extend("\u{f005}\u{f00c}\u{f00d}\u{f011}\u{f015}\u{f09b}".chars());
        v.extend("\u{f120}\u{f121}\u{f0c9}\u{f1d3}\u{f269}\u{f4a0}".chars());
        v.extend("一二日本語漢字".chars());
        v
    }

    /// No glyph rasterizes as a filled block at any desktop ppem, in any mode.
    #[test]
    fn ppem_sweep_no_glyph_rasterizes_as_a_filled_block() {
        let faces: [(&str, &[u8]); 2] = [
            ("DejaVuSansMono (embedded)", dejavu()),
            (
                "SymbolsNerdFontMono (embedded)",
                crate::embedded_symbols_font(),
            ),
        ];
        let chars = sweep_chars();
        let modes = [HintMode::Full, HintMode::Light];
        let mut checked = 0usize;
        for (name, bytes) in faces {
            for mode in modes {
                for px in 12..=40 {
                    let target = mode.options().expect("the hinting modes carry a target");
                    let px = px as f32;
                    let hint = HintingInstance::new(bytes, 0, px, &[], target)
                        .expect("a hinting instance at every desktop ppem");
                    for &ch in &chars {
                        let Some(gid) = unicode_gid(bytes, 0, ch) else {
                            continue;
                        };
                        let (w, h, _, _, _, cov) =
                            hinted_glyph_raster(bytes, 0, gid, px, &hint, &[])
                                .expect("the hinted raster never fails on a bundled face");
                        if cov.is_empty() {
                            // blank glyph (space): advance only
                            continue;
                        }
                        checked += 1;
                        let mut pen = PathPen::default();
                        hint.draw(bytes, 0, gid, &mut pen)
                            .expect("the outline we just drew still draws");

                        let reference = generous_slack_raster(&pen, w, h);
                        let worst = cov
                            .iter()
                            .zip(&reference)
                            .map(|(a, b)| i32::from(*a) - i32::from(*b))
                            .map(i32::abs)
                            .max()
                            .unwrap_or(0);
                        assert!(
                            worst <= 1,
                            "{name} {mode:?} {px}px {ch:?} (U+{:04X}): coverage moved by \
                             {worst}/255 when the fill grid gained slack — the outline is \
                             sitting on the grid boundary and the rasterizer lost a scanline",
                            ch as u32
                        );

                        let ink = cov.iter().map(|&v| f32::from(v)).sum::<f32>() / 255.0;
                        let (area, perim) = area_and_perimeter(&pen);
                        let slack = 0.06 * perim + 0.75;
                        assert!(
                            (ink - area).abs() <= slack,
                            "{name} {mode:?} {px}px {ch:?} (U+{:04X}): {w}x{h} raster carries \
                             {ink:.2}px² of ink for an outline enclosing {area:.2}px² \
                             (perimeter {perim:.1}px, allowed drift {slack:.2}px²)",
                            ch as u32
                        );
                    }
                }
            }
        }
        assert!(
            checked > 5_000,
            "the ppem sweep must actually rasterize thousands of glyphs, got {checked}"
        );
    }

    /// At a variable instance the hinted path's advance is the instantiated unhinted raster's,
    /// to the bit: both read `hmtx` + `HVAR` at the same coords through the same reader.
    #[test]
    fn advance_at_an_instance_is_the_unhinted_one() {
        let bytes = crate::display_face_bytes("engraved").expect("the bundled engraved face");
        let bold = [(WGHT, 700.0)];
        let mut moved = 0usize;
        for px in [12.0f32, 13.0, 16.0] {
            let hint = HintingInstance::new(bytes, 0, px, &bold, HintMode::Full.options().unwrap())
                .unwrap();
            for ch in ['M', 'i', 'W', '0', '&'] {
                let gid = unicode_gid(bytes, 0, ch).unwrap();
                let (.., adv, _) = hinted_glyph_raster(bytes, 0, gid, px, &hint, &bold).unwrap();
                let (.., unhinted, _) =
                    crate::variation::varied_glyph_raster(bytes, 0, &bold, gid, px).unwrap();
                assert_eq!(
                    adv.to_bits(),
                    unhinted.to_bits(),
                    "{ch:?} @{px}px wght 700: hinted advance {adv} != unhinted {unhinted}"
                );
                let (.., default_cut, _) =
                    hinted_glyph_raster(bytes, 0, gid, px, &hint, &[]).unwrap();
                moved += usize::from(default_cut.to_bits() != adv.to_bits());
            }
        }
        assert!(
            moved > 0,
            "control: no probed advance differs between wght 700 and the default cut, \
             so the equality above cannot tell the two instances apart"
        );
    }

    /// The bank keys on the variation slot, so two instantiations of the same bytes at one px
    /// are distinct instances: the regular and `wght`≈700 bold cuts of a variable face with no
    /// Bold sibling (Windows' Cascadia Mono) coexist in one atlas.
    #[test]
    fn bank_keys_on_the_variation_slot() {
        let mut bank = HintBank::default();
        let bytes = dejavu();
        let ptr = bytes.as_ptr() as usize;
        let a = bank
            .instance(ptr, bytes, 0, 12.0, HintMode::Full, &[], 0)
            .unwrap();
        let b = bank
            .instance(ptr, bytes, 0, 12.0, HintMode::Full, &[(WGHT, 700.0)], 2)
            .unwrap();
        assert!(
            !std::sync::Arc::ptr_eq(&a, &b),
            "slot 0 and slot 2 must not share one hinting instance"
        );
        let a2 = bank
            .instance(ptr, bytes, 0, 12.0, HintMode::Full, &[], 0)
            .unwrap();
        assert!(std::sync::Arc::ptr_eq(&a, &a2));
    }
}
