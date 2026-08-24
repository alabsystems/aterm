// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LINUX CRISPNESS (W13): grid-fitted (hinted) glyph rasterization for the
//! native non-macOS raster path.
//!
//! fontdue rasterizes an outline faithfully but has NO hinting: at desktop
//! sizes (DejaVu Sans Mono 12px on a scale-1.0 display) stems land at
//! fractional pixel phases (`fract(bounds.xmin)` is baked into the bitmap) and
//! cap/x-height horizontal features straddle two pixel rows — measured on a
//! live frame as a 'T' crossbar split ~75%/25% across adjacent rows and every
//! stem trailing a ~20%-coverage ghost column. That is the "fuzzy on Linux"
//! the macOS path never shows, because CoreText applies its own grid
//! discipline + smoothing.
//!
//! This module is the Linux twin of the macOS `ct_glyph` seam: skrifa runs the
//! font's OWN TrueType bytecode or the FreeType-ported autohinter
//! ([`skrifa::outline::HintingInstance`]) and hands back a grid-fitted outline
//! in pixel space, which the SAME `ab_glyph_rasterizer` coverage fill as the
//! FONT-2 variation path (`variation::varied_glyph_raster`) turns into an
//! 8-bit mask in fontdue's metric convention
//! `(width, height, xmin, ymin, advance, bytes)`. Every function returns
//! `Option` and every `None` falls back to the untouched fontdue path, so this
//! is always a SAFE enhancement — exactly the CoreText seam's contract.
//!
//! Measured at DejaVu Sans Mono 12px over a pangram + digits strip: fully
//! covered (>240) texels rise from 6.6% (fontdue) to 25.7% (mode `full`), and
//! mid-grey fringe (40..=215) texels drop from 59.7% to 47.7% — stems and
//! crossbars snap to whole pixels instead of smearing across two.
//!
//! ADVANCES STAY LINEAR: the returned advance is the scaled `hmtx` value
//! (identical math to fontdue's), never the hinted advance — cell geometry,
//! wide-glyph centering and spill accounting are byte-identical to the
//! unhinted path. Only the coverage and its integer placement change.
//!
//! Gated to `cfg(all(unix, not(target_os = "macos")))` at the module
//! declaration AND in Cargo.toml (target-gated dependency): macOS keeps
//! CoreText byte-identically, Windows/wasm keep fontdue byte-identically.

use skrifa::{
    instance::{LocationRef, Size},
    outline::{
        DrawSettings, Engine, HintingInstance, HintingOptions, OutlinePen, SmoothMode, Target,
    },
    FontRef, MetadataProvider,
};

/// How the native Linux raster path grid-fits outlines. Resolved ONCE per
/// renderer from `ATERM_FONT_HINTING` (construction-time, like
/// `ATERM_RASTERIZER`); rasterized coverage is cached per glyph, so a
/// mid-flight env flip must not split the atlas between two modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum HintMode {
    /// FreeType-ported autohinter, normal smooth target: snaps stems and
    /// horizontal features in BOTH axes — the crispest antialiased result and
    /// the DEFAULT. Chosen over the font's own bytecode because skrifa's
    /// TrueType interpreter (like FreeType's v40) applies bytecode
    /// vertical-only, which leaves the stem fringing this module exists to
    /// remove; the autohinter is also uniform across fonts with and without
    /// bytecode.
    #[default]
    Full,
    /// Autohinter, light target: vertical-only grid fit (the common desktop
    /// Linux `hintslight` look) — crossbars/tops snap, stem phases keep the
    /// designed fractional positions.
    Light,
    /// The font's own hinting engine when it has one (TrueType bytecode / CFF
    /// interpreter), autohinter otherwise — FreeType's engine-selection rule.
    Native,
    /// No grid fitting: the pure fontdue path, bit-for-bit.
    Off,
}

impl HintMode {
    /// Parse one mode spelling — shared by the env read ([`Self::from_env`])
    /// and the config key (`font_hinting`, via `Renderer::set_font_hinting`).
    /// Unrecognized = the default ([`HintMode::Full`]), the same forgiving
    /// shape the env always had; explicit disable spellings match the
    /// workspace's usual off/0/none family.
    pub(crate) fn parse(s: &str) -> Self {
        match s.trim() {
            "light" => Self::Light,
            "native" => Self::Native,
            "off" | "0" | "none" | "false" => Self::Off,
            _ => Self::Full,
        }
    }

    /// Whether `ATERM_RASTERIZER=fontdue` — the byte-stable portable path the
    /// golden/parity tests export — is pinning the raster backend. It forces
    /// `Off` at construction AND wins over the `font_hinting` config setter,
    /// so those tests keep the exact fontdue bytes they were written against.
    pub(crate) fn fontdue_forced() -> bool {
        std::env::var("ATERM_RASTERIZER").ok().as_deref() == Some("fontdue")
    }

    /// Parse `ATERM_FONT_HINTING`. Unset or unrecognized = the default
    /// ([`HintMode::Full`]); [`Self::fontdue_forced`] forces `Off`.
    pub(crate) fn from_env() -> Self {
        if Self::fontdue_forced() {
            return Self::Off;
        }
        match std::env::var("ATERM_FONT_HINTING").ok().as_deref() {
            Some(s) => Self::parse(s),
            None => Self::Full,
        }
    }

    /// The skrifa options this mode stands for; `None` = hinting disabled.
    fn options(self) -> Option<HintingOptions> {
        let target = |mode| Target::Smooth {
            mode,
            // GETINFO's "ClearType symmetric rendering" bit assumes vertical
            // supersampling this analytic rasterizer does not do; off keeps
            // interpreter-hinted stems from widening into blur. Interpreter
            // paths only — the autohinter ignores it.
            symmetric_rendering: false,
            // Horizontal fitting is wanted INSIDE the cell (that is the
            // crispness); advances reported upstream stay linear regardless,
            // see the module doc.
            preserve_linear_metrics: false,
        };
        match self {
            Self::Full => Some(HintingOptions {
                engine: Engine::Auto(None),
                target: target(SmoothMode::Normal),
            }),
            Self::Light => Some(HintingOptions {
                engine: Engine::Auto(None),
                target: target(SmoothMode::Light),
            }),
            Self::Native => Some(HintingOptions {
                engine: Engine::AutoFallback,
                target: target(SmoothMode::Normal),
            }),
            Self::Off => None,
        }
    }
}

/// Per-face-and-size [`HintingInstance`] cache — the skrifa twin of the
/// CoreText `ct_cache`. Building an instance replays `fpgm`/`prep` (or
/// computes autohinter glyph styles), so it must not happen per glyph; keyed
/// like `ct_cache` on the face bytes' POINTER identity (faces are immutable
/// `Arc`s), collection index, and the raster px bits.
#[derive(Default)]
pub(crate) struct HintBank {
    map: aterm_hash::FxHashMap<(usize, u32, u32), Option<std::sync::Arc<HintingInstance>>>,
}

impl HintBank {
    /// Fetch (or build + memoize) the hinting instance for one face at `px`.
    /// A face skrifa cannot parse (or a degenerate px) memoizes `None`, so the
    /// fontdue fallback is taken WITHOUT re-attempting the build every glyph.
    pub(crate) fn instance(
        &mut self,
        key_ptr: usize,
        bytes: &[u8],
        index: u32,
        px: f32,
        mode: HintMode,
    ) -> Option<std::sync::Arc<HintingInstance>> {
        self.instance_with(key_ptr, bytes, index, px, mode.options())
    }

    /// [`Self::instance`] with the skrifa options handed in directly — the seam
    /// the subpixel raster ([`crate::subpixel`]) uses to memoize LCD-target
    /// instances in its OWN bank (the map key does not carry the target, so one
    /// bank must never hold two targets for the same face+px). `None` options =
    /// hinting disabled = no instance.
    pub(crate) fn instance_with(
        &mut self,
        key_ptr: usize,
        bytes: &[u8],
        index: u32,
        px: f32,
        options: Option<HintingOptions>,
    ) -> Option<std::sync::Arc<HintingInstance>> {
        let options = options?;
        if !px.is_finite() || px <= 0.0 {
            return None;
        }
        self.map
            .entry((key_ptr, index, px.to_bits()))
            .or_insert_with(|| {
                let font = FontRef::from_index(bytes, index).ok()?;
                HintingInstance::new(
                    &font.outline_glyphs(),
                    Size::new(px),
                    LocationRef::default(),
                    options,
                )
                .ok()
                .map(std::sync::Arc::new)
            })
            .clone()
    }

    /// Drop every memoized instance (heavy-teardown twin of `ct_cache.clear()`
    /// in `set_px`: instances are px-keyed, so this reclaims dead sizes).
    pub(crate) fn clear(&mut self) {
        self.map.clear();
    }
}

/// Map a char through the face's UNICODE cmap (skrifa's charmap — the same
/// subtable preference as ttf-parser's `glyph_index`, which the production
/// paths use via the renderer's memoized `primary_unicode_gid`/`unicode_gid`
/// resolvers; this standalone form serves the module's own tests).
#[cfg(all(test, feature = "embedded-font"))]
pub(crate) fn unicode_gid(bytes: &[u8], index: u32, ch: char) -> Option<u16> {
    let font = FontRef::from_index(bytes, index).ok()?;
    let gid: u32 = font.charmap().map(ch)?.to_u32();
    u16::try_from(gid).ok().filter(|&g| g != 0)
}

/// Rasterize glyph `gid` of the face at `px` with `hint` applied, in the
/// fontdue-compatible tuple `(width, height, xmin, ymin, advance, coverage)` —
/// the drop-in shape the raster path expects (see
/// [`variation::varied_glyph_raster`](crate::variation::varied_glyph_raster),
/// whose metric conventions this follows exactly). `xmin`/`ymin` are the
/// FLOOR of the grid-fitted outline's ink box (baseline-relative, y up), so
/// the integer blit anchor `(cell_x + xmin, baseline - height - ymin)` places
/// the fitted outline at pixel phase < 1 ulp — hinted edges land ON texel
/// boundaries instead of between them. A blank glyph (space) is `(0, 0, ..)`
/// with just the advance; `None` on any parse/draw failure = fontdue fallback.
pub(crate) fn hinted_glyph_raster(
    bytes: &[u8],
    index: u32,
    gid: u16,
    px: f32,
    hint: &HintingInstance,
) -> Option<(usize, usize, i32, i32, f32, Vec<u8>)> {
    let font = FontRef::from_index(bytes, index).ok()?;
    let glyph_id = skrifa::GlyphId::from(gid);
    // LINEAR advance (scaled hmtx — fontdue's exact math), never the hinted
    // one: metrics upstream must not move when hinting lands (module doc).
    let advance = font
        .glyph_metrics(Size::new(px), LocationRef::default())
        .advance_width(glyph_id)
        .unwrap_or(0.0);
    let outline = font.outline_glyphs().get(glyph_id)?;
    let mut pen = PathPen::default();
    // Pedantic OFF: a bytecode error inside one glyph degrades to skrifa's
    // unhinted outline of the same glyph rather than failing the raster.
    outline
        .draw(DrawSettings::hinted(hint, false), &mut pen)
        .ok()?;
    if pen.cmds.is_empty() || !pen.min_x.is_finite() {
        // No outline at all (space and friends): blank raster, advance only —
        // the blank-glyph convention of the variation path.
        return Some((0, 0, 0, 0, advance, Vec::new()));
    }
    let x_min = pen.min_x.floor();
    let x_max = pen.max_x.ceil();
    let y_min = pen.min_y.floor();
    let y_max = pen.max_y.ceil();
    let (w, h) = ((x_max - x_min) as i32, (y_max - y_min) as i32);
    // Same sanity caps as the variation raster: a degenerate or absurd box is
    // a `None` (fontdue fallback), never a huge allocation.
    if w <= 0 || h <= 0 || w > 4096 || h > 4096 {
        return None;
    }
    let (w, h) = (w as usize, h as usize);
    let mut ras = ab_glyph_rasterizer::Rasterizer::new(w, h);
    pen.fill(&mut ras, x_min, y_max, 1.0);
    let mut cov = vec![0u8; w * h];
    ras.for_each_pixel(|i, a| {
        if let Some(slot) = cov.get_mut(i) {
            *slot = (a * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
        }
    });
    Some((w, h, x_min as i32, y_min as i32, advance, cov))
}

/// One recorded outline segment, in hinted pixel space (y UP). Recorded rather
/// than streamed because the ink box must be known BEFORE the rasterizer (and
/// its y flip about the box top) can exist.
enum Cmd {
    Move(f32, f32),
    Line(f32, f32),
    Quad(f32, f32, f32, f32),
    Curve(f32, f32, f32, f32, f32, f32),
    Close,
}

/// Collects a skrifa outline (pixels, y up) and its ink bounds, then replays
/// it into an `ab_glyph_rasterizer` grid (pixels, y DOWN, origin at the ink
/// box's top-left) — the same mapping as `variation::OutlineToRaster`, minus
/// the design-unit scale (skrifa already delivers pixel coordinates).
/// `pub(crate)` for the subpixel raster ([`crate::subpixel`]), which replays
/// the same recorded outline at 3× horizontal resolution.
#[derive(Default)]
pub(crate) struct PathPen {
    cmds: Vec<Cmd>,
    pub(crate) min_x: f32,
    pub(crate) min_y: f32,
    pub(crate) max_x: f32,
    pub(crate) max_y: f32,
}

impl PathPen {
    pub(crate) fn is_blank(&self) -> bool {
        self.cmds.is_empty() || !self.min_x.is_finite()
    }

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

    /// Replay into `ras`, flipping y about `y_max` and translating by `x_min`,
    /// with the x axis scaled by `xs` AFTER the translate (`xs = 1.0` is the
    /// exact identity — multiplying an f32 by 1.0 is bit-precise — and `3.0`
    /// is the subpixel raster's horizontal oversample). Contours are
    /// implicitly closed (TrueType/CFF convention; ab_glyph needs the closing
    /// edge for nonzero winding), matching `OutlineToRaster`.
    pub(crate) fn fill(
        &self,
        ras: &mut ab_glyph_rasterizer::Rasterizer,
        x_min: f32,
        y_max: f32,
        xs: f32,
    ) {
        let map = |x: f32, y: f32| ab_glyph_rasterizer::point((x - x_min) * xs, y_max - y);
        let mut last = ab_glyph_rasterizer::point(0.0, 0.0);
        let mut start = last;
        let close = |ras: &mut ab_glyph_rasterizer::Rasterizer,
                         last: &mut ab_glyph_rasterizer::Point,
                         start: ab_glyph_rasterizer::Point| {
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

impl OutlinePen for PathPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.see(x, y);
        self.cmds.push(Cmd::Move(x, y));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.see(x, y);
        self.cmds.push(Cmd::Line(x, y));
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.see(cx, cy);
        self.see(x, y);
        self.cmds.push(Cmd::Quad(cx, cy, x, y));
    }
    fn curve_to(&mut self, c0x: f32, c0y: f32, c1x: f32, c1y: f32, x: f32, y: f32) {
        self.see(c0x, c0y);
        self.see(c1x, c1y);
        self.see(x, y);
        self.cmds.push(Cmd::Curve(c0x, c0y, c1x, c1y, x, y));
    }
    fn close(&mut self) {
        self.cmds.push(Cmd::Close);
    }
}

#[cfg(all(test, feature = "embedded-font"))]
mod tests {
    use super::*;

    /// The embedded DejaVu Sans Mono — present under the (default)
    /// `embedded-font` feature, so these tests never depend on system font
    /// files (and vanish, rather than break, on a --no-default-features run).
    fn dejavu() -> &'static [u8] {
        crate::embedded_font()
    }

    fn full_instance(px: f32) -> HintingInstance {
        let font = FontRef::new(dejavu()).unwrap();
        HintingInstance::new(
            &font.outline_glyphs(),
            Size::new(px),
            LocationRef::default(),
            HintMode::Full.options().unwrap(),
        )
        .unwrap()
    }

    /// The blit anchors must be the FLOOR of the fitted ink box: xmin <= every
    /// outline x < xmin + width, same for y — the "origins snap to integer
    /// pixels" law. Verified over the printable ASCII set at the live 12px.
    #[test]
    fn ink_box_floor_law_ascii_12px() {
        let hint = full_instance(12.0);
        for ch in ' '..='~' {
            let Some(gid) = unicode_gid(dejavu(), 0, ch) else {
                continue;
            };
            let (w, h, _xmin, _ymin, adv, cov) =
                hinted_glyph_raster(dejavu(), 0, gid, 12.0, &hint).unwrap();
            assert_eq!(cov.len(), w * h, "coverage is width*height for {ch:?}");
            assert!(adv > 0.0, "printable ASCII advances are positive ({ch:?})");
            assert!(w <= 32 && h <= 32, "sane 12px ink box for {ch:?}: {w}x{h}");
        }
    }

    /// Per-row peak coverage ≥ `t` — a row whose brightest texel is near-full
    /// renders a solid (not washed) stroke crossing there.
    fn rows_with_peak(cov: &[u8], w: usize, h: usize, t: u8) -> usize {
        (0..h)
            .filter(|&r| (0..w).any(|c| cov[r * w + c] >= t))
            .count()
    }

    /// CRISPNESS, the measured claim of this module: at 12px the grid-fitted
    /// 'l' carries a near-fully-saturated stem texel on (almost) EVERY row,
    /// where the unhinted fontdue raster's fractional stem phase splits the
    /// same stem ~85/190 across two columns (measured, see the module doc) and
    /// peaks near-full on at most a couple of rows.
    #[test]
    fn full_hinting_snaps_the_l_stem_at_12px() {
        let hint = full_instance(12.0);
        let gid = unicode_gid(dejavu(), 0, 'l').unwrap();
        let (w, h, _, _, _, cov) = hinted_glyph_raster(dejavu(), 0, gid, 12.0, &hint).unwrap();
        let hinted_rows = rows_with_peak(&cov, w, h, 240);
        let fd = fontdue::Font::from_bytes(dejavu(), fontdue::FontSettings::default()).unwrap();
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

    /// The reported advance is LINEAR (scaled hmtx — fontdue's math), not the
    /// hinted advance: cell geometry must not move when hinting lands.
    #[test]
    fn advance_stays_linear() {
        let hint = full_instance(12.0);
        let fd = fontdue::Font::from_bytes(dejavu(), fontdue::FontSettings::default()).unwrap();
        for ch in ['M', 'i', 'W', '0'] {
            let gid = unicode_gid(dejavu(), 0, ch).unwrap();
            let (.., adv, _) = hinted_glyph_raster(dejavu(), 0, gid, 12.0, &hint).unwrap();
            let fd_adv = fd.metrics(ch, 12.0).advance_width;
            assert!(
                (adv - fd_adv).abs() < 0.01,
                "{ch:?}: hinted-path advance {adv} != fontdue linear advance {fd_adv}"
            );
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
            hinted_glyph_raster(dejavu(), 0, gid, 12.0, &hint).unwrap();
        assert_eq!((w, h, xmin, ymin), (0, 0, 0, 0));
        assert!(cov.is_empty());
        assert!(adv > 0.0);
    }

    /// Determinism: the same (face, gid, px, mode) rasterizes byte-identically
    /// twice — the atlas-cache assumption.
    #[test]
    fn raster_is_deterministic() {
        let hint = full_instance(12.0);
        let gid = unicode_gid(dejavu(), 0, 'g').unwrap();
        let a = hinted_glyph_raster(dejavu(), 0, gid, 12.0, &hint).unwrap();
        let b = hinted_glyph_raster(dejavu(), 0, gid, 12.0, &hint).unwrap();
        assert_eq!(a, b);
    }

    /// `HintMode::Off` yields no options, so the bank hands out no instance:
    /// the fontdue path is reached bit-for-bit.
    #[test]
    fn off_mode_disables_the_bank() {
        let mut bank = HintBank::default();
        let bytes = dejavu();
        assert!(bank
            .instance(bytes.as_ptr() as usize, bytes, 0, 12.0, HintMode::Off)
            .is_none());
        assert!(bank
            .instance(bytes.as_ptr() as usize, bytes, 0, 12.0, HintMode::Full)
            .is_some());
    }
}
