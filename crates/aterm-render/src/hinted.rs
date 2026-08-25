// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! NATIVE CRISPNESS (W13 Linux, then Windows): grid-fitted (hinted) glyph
//! rasterization for the native non-macOS raster paths.
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
//! Windows had no grid fitting at all until this module reached it — the raw
//! outline fill, on the one platform whose reference renderers (Windows
//! Terminal, VS Code, conhost) are all DirectWrite-hinted in the next window.
//!
//! This module is the non-CoreText twin of the macOS `ct_glyph` seam: skrifa runs the
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
//! VARIABLE INSTANCES (Windows): every entry point carries the user-space
//! variation coords, so the outline that gets grid-fitted is the INSTANTIATED
//! one — `HintingInstance::new` takes the normalized location, and skrifa's
//! hinted draw reads size + location back off the instance. That matters on
//! Windows specifically: the default face there
//! (`C:\Windows\Fonts\CascadiaMono.ttf`) is a variable font with no separate
//! Bold file, so BOTH the regular cut and the `wght`≈700 cut the real-bold
//! path (W9) draws are instances, and a hinter pinned to `LocationRef::default()`
//! would silently reset the configured weight. Linux passes an empty coord
//! slice, which normalizes to the same default location it always used.
//!
//! Gated to `cfg(any(all(unix, not(target_os = "macos")), windows))` at the
//! module declaration AND in Cargo.toml (target-gated dependency): macOS keeps
//! CoreText byte-identically, the wasm consumer keeps fontdue byte-identically
//! and compiles none of it.

use skrifa::{
    FontRef, MetadataProvider, Tag,
    instance::{Location, Size},
    outline::{
        DrawSettings, Engine, HintingInstance, HintingOptions, OutlinePen, SmoothMode, Target,
    },
};

/// Normalize user-space `(tag, value)` variation coords against `font`'s
/// `fvar`/`avar` — the skrifa twin of `ttf_parser::Face::set_variation`, which
/// the portable [`variation`](crate::variation) raster uses on the very same
/// coord slice. An EMPTY slice yields the font's default location, i.e. exactly
/// what `LocationRef::default()` meant before instances were plumbed through;
/// tags the face does not carry are ignored, same as the ttf-parser path.
fn location_of(font: &FontRef, coords: &[(u32, f32)]) -> Location {
    font.axes()
        .location(coords.iter().map(|&(tag, v)| (Tag::from_u32(tag), v)))
}

/// How the native (Linux / Windows) raster path grid-fits outlines. Resolved ONCE per
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

/// Per-face-size-and-INSTANCE [`HintingInstance`] cache — the skrifa twin of
/// the CoreText `ct_cache`. Building an instance replays `fpgm`/`prep` (or
/// computes autohinter glyph styles), so it must not happen per glyph; keyed
/// like `ct_cache` on the face bytes' POINTER identity (faces are immutable
/// `Arc`s), collection index, the raster px bits, and the variation SLOT
/// (`0` = default instance, `1` = the resolved primary coords, `2` = the bold
/// instance — the same three-slot discipline, and the same caller, as
/// `ct_cache`; one face can hold several instantiations of the same bytes).
#[derive(Default)]
pub(crate) struct HintBank {
    map: aterm_hash::FxHashMap<(usize, u32, u32, u8), Option<std::sync::Arc<HintingInstance>>>,
}

impl HintBank {
    /// Fetch (or build + memoize) the hinting instance for one face at `px`,
    /// at the variation `coords` discriminated by `slot`. A face skrifa cannot
    /// parse (or a degenerate px) memoizes `None`, so the fontdue fallback is
    /// taken WITHOUT re-attempting the build every glyph.
    ///
    /// The arity is the face's identity spelled out (`key_ptr`/`bytes`/`index`
    /// pin WHICH face, `px`/`mode` WHICH instance, `coords`/`slot` WHICH
    /// variation); a params struct here would be the same fields with one more
    /// name to keep in step at every call site.
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

    /// [`Self::instance`] with the skrifa options handed in directly — the seam
    /// the subpixel raster ([`crate::subpixel`]) uses to memoize LCD-target
    /// instances in its OWN bank (the map key does not carry the target, so one
    /// bank must never hold two targets for the same face+px). `None` options =
    /// hinting disabled = no instance. The `(coords, slot)` instance discipline
    /// is the same as [`Self::instance`]'s; the subpixel raster's stage-1 set
    /// excludes variable instances, so it always passes `(&[], 0)`.
    #[allow(clippy::too_many_arguments)] // see `instance`
    pub(crate) fn instance_with(
        &mut self,
        key_ptr: usize,
        bytes: &[u8],
        index: u32,
        px: f32,
        options: Option<HintingOptions>,
        coords: &[(u32, f32)],
        slot: u8,
    ) -> Option<std::sync::Arc<HintingInstance>> {
        let options = options?;
        if !px.is_finite() || px <= 0.0 {
            return None;
        }
        self.map
            .entry((key_ptr, index, px.to_bits(), slot))
            .or_insert_with(|| {
                let font = FontRef::from_index(bytes, index).ok()?;
                let location = location_of(&font, coords);
                HintingInstance::new(&font.outline_glyphs(), Size::new(px), &location, options)
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
///
/// `coords` must be the SAME user-space variation coords `hint` was built at
/// (the caller's slot discipline guarantees it): the outline comes from the
/// instance, so the advance has to be measured at that instance too — an
/// `HVAR`-varying face would otherwise report the default cut's width.
pub(crate) fn hinted_glyph_raster(
    bytes: &[u8],
    index: u32,
    gid: u16,
    px: f32,
    hint: &HintingInstance,
    coords: &[(u32, f32)],
) -> Option<(usize, usize, i32, i32, f32, Vec<u8>)> {
    let font = FontRef::from_index(bytes, index).ok()?;
    let glyph_id = skrifa::GlyphId::from(gid);
    let location = location_of(&font, coords);
    // LINEAR advance (scaled hmtx — fontdue's exact math), never the hinted
    // one: metrics upstream must not move when hinting lands (module doc).
    let advance = font
        .glyph_metrics(Size::new(px), &location)
        .advance_width(glyph_id)
        .unwrap_or(0.0);
    let outline = font.outline_glyphs().get(glyph_id)?;
    let mut pen = PathPen::default();
    // Pedantic OFF: a bytecode error inside one glyph degrades to skrifa's
    // unhinted outline of the same glyph rather than failing the raster.
    outline
        .draw(DrawSettings::hinted(hint, false), &mut pen)
        .ok()?;
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
    // Same sanity caps as the variation raster: a degenerate or absurd box is
    // a `None` (fontdue fallback), never a huge allocation.
    if w <= 0 || h <= 0 || w > 4096 || h > 4096 {
        return None;
    }
    let (w, h) = (w as usize, h as usize);
    // Fill into a grid with `RASTER_PAD` px of slack on every side and crop the
    // ink box back out. Grid fitting is exactly what makes the unpadded fill
    // unsafe: the autohinter snaps stem edges to whole pixels, so the fitted
    // outline routinely sits EXACTLY on `x = floor(min_x)` — the grid's left
    // edge — where the rasterizer's incremental x march drifts sub-ULP negative
    // and drops a whole scanline's area, smearing the rest of the glyph into a
    // filled block. See `variation::RASTER_PAD` for the full mechanism and the
    // measured damage map ('?' at ppem 17, '2' at ppem 19 on the default face).
    let pad = crate::variation::RASTER_PAD as f32;
    let mut ras = ab_glyph_rasterizer::Rasterizer::new(
        w + 2 * crate::variation::RASTER_PAD,
        h + 2 * crate::variation::RASTER_PAD,
    );
    // `fill` translates by `x_min` and flips y about `y_max`, so shifting those
    // origins by the pad IS the +PAD translate into the grid's interior.
    pen.fill(&mut ras, x_min - pad, y_max + pad, 1.0);
    let cov = crate::variation::crop_padded_coverage(&ras, w, h);
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

    /// The `wght` axis tag, in the same big-endian `u32` spelling the renderer
    /// stores coords in ([`crate::variation::WGHT_TAG`]).
    const WGHT: u32 = u32::from_be_bytes(*b"wght");

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
            &location_of(&font, &[]),
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
                hinted_glyph_raster(dejavu(), 0, gid, 12.0, &hint, &[]).unwrap();
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
        let (w, h, _, _, _, cov) = hinted_glyph_raster(dejavu(), 0, gid, 12.0, &hint, &[]).unwrap();
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
            let (.., adv, _) = hinted_glyph_raster(dejavu(), 0, gid, 12.0, &hint, &[]).unwrap();
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
            hinted_glyph_raster(dejavu(), 0, gid, 12.0, &hint, &[]).unwrap();
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
        let a = hinted_glyph_raster(dejavu(), 0, gid, 12.0, &hint, &[]).unwrap();
        let b = hinted_glyph_raster(dejavu(), 0, gid, 12.0, &hint, &[]).unwrap();
        assert_eq!(a, b);
    }

    /// `HintMode::Off` yields no options, so the bank hands out no instance:
    /// the fontdue path is reached bit-for-bit.
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

    /// Re-fill the SAME recorded outline into a grid with a GENEROUS slack ring
    /// (4 px, four times what production uses) and crop the ink box back out.
    /// The coverage of a correct fill cannot depend on how much empty grid
    /// surrounds it — so this is the reference the production raster is held to.
    fn generous_slack_raster(pen: &PathPen, w: usize, h: usize) -> Vec<u8> {
        const SLACK: usize = 4;
        let (x_min, y_max) = (pen.min_x.floor(), pen.max_y.ceil());
        let mut ras = ab_glyph_rasterizer::Rasterizer::new(w + 2 * SLACK, h + 2 * SLACK);
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

    /// `(area, perimeter)` of the fitted outline in px / px², by the shoelace
    /// formula over a fine flattening. The area is the ink a correct nonzero
    /// fill MUST deposit; the perimeter bounds how far antialiasing (and the
    /// rasterizer's own coarser curve flattening) can move it, since both are
    /// boundary effects.
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

    /// The glyph set the ppem sweep covers: every printable ASCII code point
    /// (the digits the bug ate live here), the box-drawing / block run a
    /// terminal draws frames and progress bars out of, a Nerd-Font PUA sample
    /// (the bundled symbols face carries nothing else, and its icon outlines
    /// look nothing like Latin text), and a CJK sample for any face that
    /// carries one.
    fn sweep_chars() -> Vec<char> {
        let mut v: Vec<char> = (' '..='~').collect();
        v.extend("─│┌┐└┘├┤┬┴┼━┃█▀▄▌▐░▒▓".chars());
        v.extend("\u{e0b0}\u{e0b2}\u{e5ff}\u{e62b}\u{e706}\u{e7a8}".chars());
        v.extend("\u{f005}\u{f00c}\u{f00d}\u{f011}\u{f015}\u{f09b}".chars());
        v.extend("\u{f120}\u{f121}\u{f0c9}\u{f1d3}\u{f269}\u{f4a0}".chars());
        v.extend("一二日本語漢字".chars());
        v
    }

    /// THE ppem-19 BROKEN-DIGIT REGRESSION (see `variation::RASTER_PAD`).
    ///
    /// Grid fitting snaps stem edges to whole pixels, so a fitted outline
    /// routinely sits EXACTLY on `x = floor(min_x)` — the coverage grid's left
    /// edge. `ab_glyph_rasterizer` marches x incrementally down a segment's
    /// scanlines, drifts a sub-ULP past that edge, floors to `-1`, and drops the
    /// whole scanline's area; `for_each_pixel`'s single running accumulator then
    /// carries the loss through every remaining texel and the glyph paints as a
    /// filled block. On the shipped default face that ate `'2'` at ppem 19 —
    /// which is `round(15 * 1.25)`, i.e. what the Linux auto-scale law hands
    /// every 125%-scale desktop — and `'?'` at ppem 17.
    ///
    /// TWO independent nets over the whole ppem range, both modes-wide:
    ///
    /// 1. SLACK INVARIANCE (sharp). Coverage cannot depend on how much empty
    ///    grid surrounds the outline, so the production raster must match a
    ///    4-px-slack fill of the same outline to within one 8-bit step.
    ///    Pre-fix, the two broken glyphs differ from it on EVERY texel, by up
    ///    to 180/255.
    /// 2. INK PLAUSIBILITY (the "implausibly high ink for its shape" net).
    ///    Rasterized ink must equal the fitted outline's geometric area; the
    ///    slack is proportional to the outline's PERIMETER because antialiasing
    ///    and the rasterizer's own curve flattening are boundary effects.
    ///    Measured over the 11 571 (mode, ppem, glyph) combinations this sweep
    ///    covers, the honest residual peaks at 0.029·perimeter; the pre-fix
    ///    `'2'` at ppem 19 sat at 0.097·perimeter and `'?'` at ppem 17 at
    ///    0.491·perimeter, so 0.06·perimeter separates them with margin on
    ///    both sides.
    #[test]
    fn ppem_sweep_no_glyph_rasterizes_as_a_filled_block() {
        // Both bundled faces: the text default the bug bit, and the Nerd icon
        // face, whose outlines are shaped nothing like Latin text.
        #[allow(unused_mut)]
        let mut faces: Vec<(&str, &[u8])> = vec![("DejaVuSansMono (embedded)", dejavu())];
        #[cfg(feature = "embedded-symbols")]
        faces.push(("SymbolsNerdFontMono (embedded)", crate::embedded_symbols_font()));
        let chars = sweep_chars();
        let modes = [HintMode::Full, HintMode::Light, HintMode::Native];
        let mut checked = 0usize;
        for (name, bytes) in faces {
            let font = FontRef::new(bytes).expect("bundled face parses");
            for mode in modes {
                for px in 12..=40 {
                    let opts = mode.options().expect("the three hinting modes carry options");
                    let px = px as f32;
                    let hint = HintingInstance::new(
                        &font.outline_glyphs(),
                        Size::new(px),
                        &location_of(&font, &[]),
                        opts,
                    )
                    .expect("a hinting instance at every desktop ppem");
                    for &ch in &chars {
                        let Some(gid) = unicode_gid(bytes, 0, ch) else {
                            continue;
                        };
                        let (w, h, _, _, _, cov) =
                            hinted_glyph_raster(bytes, 0, gid, px, &hint, &[])
                                .expect("the hinted raster never fails on a bundled face");
                        if cov.is_empty() {
                            continue; // blank glyph (space): advance only
                        }
                        checked += 1;
                        // Re-draw the same outline to measure it against.
                        let outline = font
                            .outline_glyphs()
                            .get(skrifa::GlyphId::from(gid))
                            .expect("the gid we just rasterized still resolves");
                        let mut pen = PathPen::default();
                        outline
                            .draw(DrawSettings::hinted(&hint, false), &mut pen)
                            .expect("the outline we just drew still draws");

                        // 1. slack invariance
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

                        // 2. ink plausibility
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
        // A sweep that quietly checked nothing would pass forever.
        assert!(
            checked > 5_000,
            "the ppem sweep must actually rasterize thousands of glyphs, got {checked}"
        );
    }

    /// A VARIABLE system face — Windows' platform default (Cascadia Mono) or
    /// any other `%WINDIR%\Fonts` face with a `wght` axis. Panics rather than
    /// skipping: a test that silently returns when its fixture is missing
    /// passes forever and proves nothing, and the embedded DejaVu (no `fvar`)
    /// cannot stand in — every location on it normalizes to empty, which is
    /// exactly the vacuity this guards against.
    #[cfg(windows)]
    fn variable_system_face() -> (String, Vec<u8>) {
        let dir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_string());
        let mut candidates = vec![format!("{dir}\\Fonts\\CascadiaMono.ttf")];
        if let Ok(rd) = std::fs::read_dir(format!("{dir}\\Fonts")) {
            candidates.extend(rd.flatten().filter_map(|e| {
                let p = e.path();
                (p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ttf")))
                    .then(|| p.to_str().map(str::to_string))
                    .flatten()
            }));
        }
        for path in candidates {
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let usable = FontRef::new(&bytes).is_ok_and(|f| {
                f.axes()
                    .iter()
                    .any(|a| a.tag() == Tag::from_u32(WGHT) && a.max_value() >= 600.0)
            });
            if usable {
                return (path, bytes);
            }
        }
        panic!("no variable font with a `wght` axis under {dir}\\Fonts");
    }

    /// The coord seam, on a face that can actually show it: an EMPTY slice is
    /// the `fvar` DEFAULT location (the pre-instance behaviour this module
    /// shipped with, which Linux still relies on byte-for-byte), and a REAL
    /// request moves off it. Without the second half the first is vacuous —
    /// a static face normalizes everything to the default.
    #[cfg(windows)]
    #[test]
    fn coords_normalize_into_the_location() {
        let (path, bytes) = variable_system_face();
        let font = FontRef::new(&bytes).unwrap();
        let default = location_of(&font, &[]);
        assert!(
            !default.coords().is_empty(),
            "{path}: fixture precondition — a variable face has fvar coords"
        );
        assert!(
            default.coords().iter().all(|c| c.to_f32() == 0.0),
            "{path}: empty coords must be the fvar default location, got {:?}",
            default.coords()
        );
        let bold = location_of(&font, &[(WGHT, 700.0)]);
        assert!(
            bold.coords().iter().any(|c| c.to_f32() != 0.0),
            "{path}: a real wght request must move off the default location \
             (the hinter would otherwise fit the wrong cut)"
        );
    }

    /// The bank keys on the variation SLOT, so two instantiations of the SAME
    /// bytes at the same px are distinct instances — the invariant that lets
    /// the regular cut and the `wght`≈700 bold cut coexist in one atlas
    /// (Windows' Cascadia Mono ships as one variable file with no Bold
    /// sibling, so both cuts come out of these very bytes).
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
        // …and the same slot memoizes, so `fpgm`/`prep` is not replayed per glyph.
        let a2 = bank
            .instance(ptr, bytes, 0, 12.0, HintMode::Full, &[], 0)
            .unwrap();
        assert!(std::sync::Arc::ptr_eq(&a, &a2));
    }
}
