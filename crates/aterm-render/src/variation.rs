// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Variable-font instantiation (W9).
//!
//! macOS ships SF Mono as a VARIABLE font (`SFNSMono.ttf`) whose `fvar`
//! default instance is "SF NS Mono Light" (`wght` default ≈ 294.67) — so
//! loading the file at its default instance renders faint, thin text (the
//! reason the built-in candidate order once demoted it below Menlo; with this
//! fix in place SF Mono now LEADS the candidates). The root fix
//! is to INSTANTIATE the face: parse `fvar` at load, resolve the `Regular`
//! named instance (or `wght = 400` clamped to the axis) by default, overlay
//! the user's `font_variation` / `font_weight` config, and apply the ONE
//! resulting coordinate list everywhere a face is consumed — the CoreText
//! descriptor (`kCTFontVariationAttribute`, via
//! `CTFontDescriptorCreateCopyWithVariation`), the rustybuzz shaping face
//! (`Face::set_variations`), and the ttf-parser metrics derivation
//! (`Face::set_variation`, HVAR/MVAR-aware) — so raster, shaping and cell
//! geometry can never disagree about which instance is on screen.
//!
//! This module holds the PURE policy: axis clamping, named-instance / weight
//! resolution, config-spec parsing, and the dark-theme weight-nudge safety
//! gate — each a total function of parsed facts, so every law is
//! machine-checkable without font I/O (`tests/variation_instantiation.rs`;
//! Tier-0 abstract twins `vf_axis_clamp_model` / `vf_nudge_gate_model` in
//! `aterm_spec::derive`, checked by the Trust `ty` compiler). The only
//! impure parts are the `fvar`/`name` probes and the coordinate-applied
//! metric measurements, all bounds-checked and total (`None` on malformed
//! input, never a panic).
//!
//! PORTABLE RASTER (FONT-2): fontdue has no variation API, so the portable path
//! (non-macOS, and `ATERM_RASTERIZER=fontdue`) cannot ask fontdue for an instance.
//! [`varied_glyph_raster`] closes that gap: ttf-parser applies the resolved coords to
//! the glyph OUTLINE and an `ab_glyph_rasterizer` coverage fill (nonzero winding + AA)
//! rasterizes it, so the portable path now draws the RESOLVED instance — not the `fvar`
//! default — matching the CoreText path's instance (grid geometry + shaping already
//! followed the coords). The renderer routes the primary face through it on that path
//! (`Renderer::vf_primary_gid_raster` / `vf_primary_char_raster`).

/// The `wght` (weight) axis tag, big-endian packed like every OpenType tag.
pub const WGHT_TAG: u32 = u32::from_be_bytes(*b"wght");
/// CSS/OpenType Regular weight — the default instantiation target.
pub const REGULAR_WGHT: f32 = 400.0;
/// CSS/OpenType Bold weight — the real-instance replacement for synthetic
/// dilation when the primary face carries a `wght` axis.
pub const BOLD_WGHT: f32 = 700.0;
/// The dark-theme weight nudge is permitted ONLY when the nudged instance's
/// `'M'` advance equals the default instance's within this many device px
/// (`|adv_nudged − adv_default| <= 0.25`): monospace variable fonts hold
/// advances constant across the weight axis, which is what makes a weight
/// nudge uniquely safe in a fixed grid — a face that fails this is not
/// advance-stable and must NOT be nudged (the W2 linear-corrected remap,
/// already on by default, remains its weight compensation).
pub const DARK_NUDGE_ADVANCE_TOL_PX: f32 = 0.25;

/// One `fvar` axis, normalized like ttf-parser does at parse: `min <= def <=
/// max` by construction (`min = min(min, def)`, `max = max(max, def)`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VfAxis {
    /// Big-endian packed 4-byte tag (`wght`, `opsz`, …).
    pub tag: u32,
    pub min: f32,
    pub def: f32,
    pub max: f32,
}

/// The parsed variable-font facts of one face: its axes plus the coords of
/// the `Regular` named instance when one exists (resolved through the `name`
/// table, case-insensitive), `None` for a non-variable face.
#[derive(Clone, Debug)]
pub struct VfProbe {
    pub axes: Vec<VfAxis>,
    /// Per-axis design-space coords of the named instance whose subfamily
    /// name is exactly "Regular" (len == `axes.len()`), if any.
    pub regular_coords: Option<Vec<f32>>,
}

/// Clamp a requested design-space value onto `axis`.
///
/// # Invariant (proven)
///
/// Total: for EVERY input (including NaN and ±∞) the result is finite and
/// within `[axis.min, axis.max]` — a non-finite request yields the axis
/// default. Exact when achievable: a finite request already inside the axis
/// bounds is returned unchanged. Tier-0 twin: `vf_axis_clamp_model`
/// (`aterm_spec::derive`, integer clamp law checked by the Trust `ty`
/// compiler; the float/NaN half is carried by the exhaustive lattice sweep
/// in `tests/variation_instantiation.rs`, per the box-drawing rounding-law
/// precedent).
#[must_use]
pub fn clamp_axis(axis: &VfAxis, req: f32) -> f32 {
    // Axis bounds come from `VfAxis` construction (min <= def <= max, all
    // finite); a degenerate axis (non-finite fields from a hand-built value)
    // fails the range test below and falls back to the default.
    if req.is_finite() && axis.min.is_finite() && axis.max.is_finite() && axis.min <= axis.max {
        req.clamp(axis.min, axis.max)
    } else if axis.def.is_finite() {
        axis.def
    } else {
        0.0
    }
}

/// Resolve the DEFAULT instantiation coords for `axes` (one design-space
/// value per axis, same order):
///
/// * the `Regular` named instance's coords when the font declares one
///   (each clamped to its axis, so a malformed instance can never escape
///   the axis bounds), else
/// * every axis at its default EXCEPT `wght`, which is pulled to
///   `400` clamped to the axis — the root fix for SF Mono's Light default.
///
/// Total by construction: every output value is `clamp_axis`-bounded.
#[must_use]
pub fn resolve_default_coords(axes: &[VfAxis], regular: Option<&[f32]>) -> Vec<f32> {
    if let Some(reg) = regular
        && reg.len() == axes.len()
    {
        return axes
            .iter()
            .zip(reg)
            .map(|(a, &v)| clamp_axis(a, v))
            .collect();
    }
    axes.iter()
        .map(|a| {
            if a.tag == WGHT_TAG {
                clamp_axis(a, REGULAR_WGHT)
            } else {
                clamp_axis(a, a.def)
            }
        })
        .collect()
}

/// Overlay user requests (`font_variation` / `font_weight`, as `(tag, value)`
/// pairs) onto resolved `coords`, clamping each onto its axis; a tag the font
/// has no axis for is ignored. Later requests win over earlier ones for the
/// same tag (so `font_weight` can override a `wght=` entry).
#[must_use]
pub fn apply_requests(axes: &[VfAxis], mut coords: Vec<f32>, requests: &[(u32, f32)]) -> Vec<f32> {
    for &(tag, value) in requests {
        for (i, axis) in axes.iter().enumerate() {
            if axis.tag == tag && i < coords.len() {
                coords[i] = clamp_axis(axis, value);
            }
        }
    }
    coords
}

/// Parse one config `font_variation` entry — `"wght=450"`, `"opsz = 14"` —
/// into a `(tag, value)` request. The tag is 1–4 printable-ASCII chars
/// (space-padded to 4, the OpenType convention); the value must be a finite
/// float. `None` on any malformed spec (the caller warns and skips it).
#[must_use]
pub fn parse_variation_spec(spec: &str) -> Option<(u32, f32)> {
    let (tag, value) = spec.split_once('=')?;
    let tag = tag.trim().as_bytes();
    if tag.is_empty() || tag.len() > 4 || !tag.iter().all(u8::is_ascii_graphic) {
        return None;
    }
    let value: f32 = value.trim().parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    let mut packed = [b' '; 4];
    packed[..tag.len()].copy_from_slice(tag);
    Some((u32::from_be_bytes(packed), value))
}

/// The dark-theme weight-nudge SAFETY GATE (W9 moonshot): whether nudged
/// coords may replace the un-nudged ones, given the `'M'` advance measured at
/// the nudged coords and at the `fvar` DEFAULT instance (both in device px at
/// the same size).
///
/// # Invariant (proven)
///
/// The nudge applies ONLY under advance invariance: `true` requires both
/// advances finite AND `|adv_nudged − adv_default| <= 0.25px`
/// ([`DARK_NUDGE_ADVANCE_TOL_PX`]). NaN/∞ (a failed measurement) can never
/// pass, so the gate is a checked PRECONDITION, not a heuristic. Tier-0
/// twin: `vf_nudge_gate_model` (`aterm_spec::derive`); exhaustive lattice +
/// negative controls in `tests/variation_instantiation.rs`.
#[must_use]
pub fn dark_nudge_permitted(adv_nudged_px: f32, adv_default_px: f32) -> bool {
    adv_nudged_px.is_finite()
        && adv_default_px.is_finite()
        && (adv_nudged_px - adv_default_px).abs() <= DARK_NUDGE_ADVANCE_TOL_PX
}

/// Parse face `index` of `bytes` for its variation facts: the `fvar` axes
/// (via ttf-parser, which normalizes `min <= def <= max`) plus the `Regular`
/// named instance's coords (parsed from the raw `fvar` instance records —
/// ttf-parser exposes axes only — with the subfamily name resolved through
/// the `name` table, case-insensitive). `None` for a non-variable or
/// malformed face; a malformed INSTANCE array degrades to `regular_coords:
/// None` (the wght=400 clamp then applies), never a panic — every read is
/// bounds-checked.
#[must_use]
pub fn probe(bytes: &[u8], index: u32) -> Option<VfProbe> {
    let face = ttf_parser::Face::parse(bytes, index).ok()?;
    let axes: Vec<VfAxis> = face
        .variation_axes()
        .into_iter()
        .map(|a| VfAxis {
            tag: u32::from_be_bytes(a.tag.to_bytes()),
            min: a.min_value,
            def: a.def_value,
            max: a.max_value,
        })
        .collect();
    if axes.is_empty() {
        return None;
    }
    let regular_coords = regular_instance_coords(&face, axes.len());
    Some(VfProbe {
        axes,
        regular_coords,
    })
}

/// The design-space coords of the named instance whose subfamily name is
/// "Regular", from the raw `fvar` table (bounds-checked; `None` on any
/// structural inconsistency).
fn regular_instance_coords(face: &ttf_parser::Face<'_>, axis_count: usize) -> Option<Vec<f32>> {
    let data = face
        .raw_face()
        .table(ttf_parser::Tag::from_bytes(b"fvar"))?;
    let u16_at = |off: usize| -> Option<u16> {
        Some(u16::from_be_bytes([*data.get(off)?, *data.get(off + 1)?]))
    };
    // fvar header: majorVersion, minorVersion, axesArrayOffset, reserved,
    // axisCount, axisSize, instanceCount, instanceSize.
    if u16_at(0)? != 1 {
        return None;
    }
    let axes_off = usize::from(u16_at(4)?);
    let n_axes = usize::from(u16_at(8)?);
    let axis_size = usize::from(u16_at(10)?);
    let n_inst = usize::from(u16_at(12)?);
    let inst_size = usize::from(u16_at(14)?);
    // The instance record is subfamilyNameID + flags + one Fixed per axis
    // (+ an optional trailing postScriptNameID we don't read).
    if n_axes != axis_count || inst_size < 4 + 4 * n_axes {
        return None;
    }
    let inst_base = axes_off.checked_add(n_axes.checked_mul(axis_size)?)?;
    for i in 0..n_inst {
        let rec = inst_base.checked_add(i.checked_mul(inst_size)?)?;
        let name_id = u16_at(rec)?;
        if !name_is_regular(face, name_id) {
            continue;
        }
        let mut coords = Vec::with_capacity(n_axes);
        for a in 0..n_axes {
            let off = rec + 4 + 4 * a;
            let raw = i32::from_be_bytes([
                *data.get(off)?,
                *data.get(off + 1)?,
                *data.get(off + 2)?,
                *data.get(off + 3)?,
            ]);
            // Fixed 16.16 design-space value.
            coords.push(raw as f32 / 65536.0);
        }
        return Some(coords);
    }
    None
}

/// Whether `name` record `name_id` decodes to "Regular" (case-insensitive,
/// trimmed) in any Unicode-encoded entry.
fn name_is_regular(face: &ttf_parser::Face<'_>, name_id: u16) -> bool {
    face.names().into_iter().any(|n| {
        n.name_id == name_id
            && n.is_unicode()
            && n.to_string()
                .is_some_and(|s| s.trim().eq_ignore_ascii_case("regular"))
    })
}

/// Coordinate-applied face metrics in device px, derived through ttf-parser
/// with every `(tag, value)` coord set (HVAR advances; MVAR / typo-aware
/// vertical metrics — `Face::ascender` prefers the OS/2 typo metrics when
/// the face sets `USE_TYPO_METRICS`, the same law `geometry_metrics`
/// applies on the unvaried path).
#[derive(Clone, Copy, Debug)]
pub struct VariedMetricsPx {
    /// Above the baseline, positive.
    pub ascent: f32,
    /// Below the baseline, NEGATIVE (fontdue's convention).
    pub descent: f32,
    pub line_gap: f32,
    /// `'M'` advance (the monospace cell-width probe).
    pub m_advance: f32,
}

/// Measure [`VariedMetricsPx`] for face `index` of `bytes` at `px` with
/// `coords` applied. `None` when the face is malformed, has no `'M'`, or a
/// degenerate `units_per_em` — the caller falls back to the unvaried
/// (fontdue) metrics, so this is always a safe enhancement.
#[must_use]
pub fn varied_metrics_px(
    bytes: &[u8],
    index: u32,
    coords: &[(u32, f32)],
    px: f32,
) -> Option<VariedMetricsPx> {
    let mut face = ttf_parser::Face::parse(bytes, index).ok()?;
    for &(tag, value) in coords {
        // `None` here means "not variable / no such axis" — the axis list the
        // coords were resolved against came from this same face, so ignore a
        // (theoretically unreachable) miss rather than failing the metrics.
        let _ = face.set_variation(ttf_parser::Tag(tag), value);
    }
    let upem = f32::from(face.units_per_em());
    if upem <= 0.0 || !px.is_finite() {
        return None;
    }
    let scale = px / upem;
    let gid = face.glyph_index('M')?;
    let m_advance = f32::from(face.glyph_hor_advance(gid)?) * scale;
    Some(VariedMetricsPx {
        ascent: f32::from(face.ascender()) * scale,
        descent: f32::from(face.descender()) * scale,
        line_gap: f32::from(face.line_gap()) * scale,
        m_advance,
    })
}

/// FONT-2: rasterize glyph `gid` of face `index` at `px` with `coords` (the resolved
/// variable instance) APPLIED, in the fontdue-compatible tuple
/// `(width, height, xmin, ymin, advance_width, coverage)`. This closes the portable
/// raster's variation gap — fontdue has no variation API, so off macOS (or under
/// `ATERM_RASTERIZER=fontdue`) a variable primary would draw only its `fvar` DEFAULT
/// instance; here ttf-parser applies the coords to the OUTLINE and a coverage
/// rasterizer (nonzero winding + AA) fills it, so text renders at the resolved weight.
///
/// Metrics match fontdue's convention so the tuple drops into the raster path: `xmin`
/// is the left-side bearing in px, `ymin` the baseline→bottom offset (negative below
/// the baseline), coverage is `width*height` alpha bytes top-row first. `None` on a
/// malformed face, a missing/oversized glyph, or a degenerate size — the caller then
/// falls back to fontdue (default instance), so this is always a SAFE enhancement. A
/// blank glyph (space: no outline) returns a 0×0 raster carrying just the advance.
#[must_use]
pub fn varied_glyph_raster(
    bytes: &[u8],
    index: u32,
    coords: &[(u32, f32)],
    gid: u16,
    px: f32,
) -> Option<(usize, usize, i32, i32, f32, Vec<u8>)> {
    VariedFace::parse(bytes, index, coords)?.glyph_raster(gid, px)
}

/// A face parsed ONCE with its variation coords applied — the amortized form of
/// [`varied_glyph_raster`] for a caller that rasters MANY glyphs of one instance
/// (a shaped label, a specimen line): one `Face::parse` + coord replay per run
/// instead of per glyph, without the caller taking a `ttf_parser` dependency of
/// its own. Each raster is byte-identical to the byte-slice wrapper's.
pub struct VariedFace<'a> {
    face: ttf_parser::Face<'a>,
}

impl<'a> VariedFace<'a> {
    /// Parse `bytes` at collection `index` and apply `coords` (unknown tags are
    /// ignored, exactly as [`varied_glyph_raster`] ignores them). `None` on a
    /// malformed face.
    #[must_use]
    pub fn parse(bytes: &'a [u8], index: u32, coords: &[(u32, f32)]) -> Option<Self> {
        let mut face = ttf_parser::Face::parse(bytes, index).ok()?;
        for &(tag, value) in coords {
            let _ = face.set_variation(ttf_parser::Tag(tag), value);
        }
        Some(Self { face })
    }

    /// [`varied_glyph_raster`] through this instance: the same
    /// `(width, height, xmin, ymin, advance_width, coverage)` tuple and the same
    /// `None` cases (missing/oversized glyph, degenerate size).
    #[must_use]
    pub fn glyph_raster(
        &self,
        gid: u16,
        px: f32,
    ) -> Option<(usize, usize, i32, i32, f32, Vec<u8>)> {
        varied_glyph_raster_with_face(&self.face, gid, px)
    }
}

/// [`varied_glyph_raster`] on an ALREADY-PARSED face — the hot-path form. The
/// raster is identical; only the `Face::parse` + `set_variation` replay moves to
/// the caller, which can amortize it (e.g. a caller that must resolve the gid
/// through the same face's cmap first, instead of parsing the bytes twice).
///
/// The caller is responsible for building `face` at the correct collection
/// `index` and applying the variation coords BEFORE calling this — the byte-slice
/// [`varied_glyph_raster`] wrapper above does exactly that. (Same convention as
/// `ligature_shaping::shape_ligature_run_with_face`.)
#[must_use]
pub fn varied_glyph_raster_with_face(
    face: &ttf_parser::Face,
    gid: u16,
    px: f32,
) -> Option<(usize, usize, i32, i32, f32, Vec<u8>)> {
    let upem = f32::from(face.units_per_em());
    if upem <= 0.0 || !px.is_finite() || px <= 0.0 {
        return None;
    }
    let scale = px / upem;
    let g = ttf_parser::GlyphId(gid);
    let advance = f32::from(face.glyph_hor_advance(g).unwrap_or(0)) * scale;
    // No bounding box ⇒ a blank glyph (e.g. space): 0×0 raster, advance only.
    let Some(bbox) = face.glyph_bounding_box(g) else {
        return Some((0, 0, 0, 0, advance, Vec::new()));
    };
    // Pixel-space ink box (floor/ceil so the outline always fits inside the raster).
    let x_min = (f32::from(bbox.x_min) * scale).floor();
    let x_max = (f32::from(bbox.x_max) * scale).ceil();
    let y_min = (f32::from(bbox.y_min) * scale).floor();
    let y_max = (f32::from(bbox.y_max) * scale).ceil();
    let w = (x_max - x_min) as i32;
    let h = (y_max - y_min) as i32;
    if w <= 0 || h <= 0 || w > 4096 || h > 4096 {
        return None;
    }
    let (w, h) = (w as usize, h as usize);
    // Fill into a grid with RASTER_PAD px of slack on every side, the outline
    // translated into its interior, then crop: the outline must never sit ON
    // the grid boundary (see RASTER_PAD). A design-space `x_min` of 0 — which
    // most glyphs of most fonts have — scales to an exactly-0 left edge, which
    // is precisely the case that detonates.
    let pad = RASTER_PAD as f32;
    let mut ras = ab_glyph_rasterizer::Rasterizer::new(w + 2 * RASTER_PAD, h + 2 * RASTER_PAD);
    let mut b = OutlineToRaster {
        ras: &mut ras,
        scale,
        ox: x_min - pad,
        oy: y_max + pad,
        last: ab_glyph_rasterizer::point(0.0, 0.0),
        start: ab_glyph_rasterizer::point(0.0, 0.0),
    };
    // `outline_glyph` returns None for a glyph with no outline — already handled by the
    // bbox check, but stay defensive (fall back rather than emit an all-zero raster).
    face.outline_glyph(g, &mut b)?;
    b.close_contour();
    let cov = crop_padded_coverage(&ras, w, h);
    Some((w, h, x_min as i32, y_min as i32, advance, cov))
}

/// Slack, in px, between the ink box and the coverage grid every
/// `ab_glyph_rasterizer` fill in this crate actually rasterizes into. The
/// outline is translated by this much so it can NEVER touch the grid's
/// boundary; [`crop_padded_coverage`] reads the ink box back out, so the
/// returned mask and its `(width, height, xmin, ymin)` metrics are unchanged.
///
/// WHY (the ppem-19 broken-digit bug): `ab_glyph_rasterizer` marches a
/// segment's `x` INCREMENTALLY down its scanlines (`x += dxdy * dy`), so `x`
/// at the last scanline drifts a fraction of an ULP off the segment's true
/// endpoint. That is harmless in the grid's interior — but a segment sitting
/// EXACTLY on `x = 0` drifts to `-1.19e-7`, whose `floor()` is `-1`, and the
/// rasterizer has no clamp: `linestart + x0i < 0` makes it `continue`, DROPPING
/// that whole scanline's area (or, on a row past the first, crediting it to the
/// PREVIOUS row). Because `for_each_pixel` carries ONE running accumulator
/// across the entire flat buffer — each row's contributions are trusted to sum
/// to zero so the accumulator resets at the row boundary — the lost area never
/// comes back: every texel after it is offset by a constant, and a glyph paints
/// as a broken filled block.
///
/// A segment exactly on `x = 0` is not exotic; GRID FITTING MAKES IT THE
/// COMMON CASE, because the autohinter snaps stem edges to whole pixels and the
/// ink box's left edge is `floor(min_x)` — i.e. that very integer. Measured over
/// 62 304 (face, hint mode, ppem 6..=64, glyph) combinations across DejaVu Sans
/// Mono Regular/Bold, Noto Sans CJK and Symbols Nerd Font, it detonated on
/// exactly two: the DEFAULT face's `'?'` at ppem 17 and `'2'` at ppem 19 — and
/// ppem 19 is `round(15 * 1.25)`, the Linux auto-scale law at the most common
/// fractional desktop scale, so every 125% user saw broken digits by default.
/// The same reasoning applies at the right edge (`x1i == width` writes into the
/// next row) and is fixed by the same slack.
///
/// [`crate::subpixel`] needs no change: its ink box is already widened by 1 px
/// per side for the FIR5 filter spread, which is 3 subpixel samples of slack at
/// its 3× horizontal resolution.
pub(crate) const RASTER_PAD: usize = 1;

/// Read the `w`×`h` ink box back out of a coverage grid that was filled with
/// [`RASTER_PAD`] px of slack on every side (i.e. a `(w + 2*PAD)`×`(h + 2*PAD)`
/// rasterizer whose outline was translated by `+PAD` in both axes). The padding
/// ring is discarded: by construction it can only hold antialiasing spill from
/// an edge that is already inside the box.
pub(crate) fn crop_padded_coverage(
    ras: &ab_glyph_rasterizer::Rasterizer,
    w: usize,
    h: usize,
) -> Vec<u8> {
    let pad = RASTER_PAD;
    let gw = w + 2 * pad;
    let mut cov = vec![0u8; w * h];
    ras.for_each_pixel(|i, a| {
        let (gx, gy) = (i % gw, i / gw);
        let (Some(x), Some(y)) = (gx.checked_sub(pad), gy.checked_sub(pad)) else {
            return;
        };
        if x < w && y < h {
            cov[y * w + x] = (a * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
        }
    });
    cov
}

/// Feeds a ttf-parser glyph outline (design units, y-UP) into an `ab_glyph_rasterizer`
/// coverage grid (pixels, y-DOWN, origin at the ink box's top-left).
struct OutlineToRaster<'a> {
    ras: &'a mut ab_glyph_rasterizer::Rasterizer,
    scale: f32,
    ox: f32, // ink-box left edge, in px (subtracted)
    oy: f32, // ink-box top edge, in px (y is flipped about it)
    last: ab_glyph_rasterizer::Point,
    start: ab_glyph_rasterizer::Point,
}

impl OutlineToRaster<'_> {
    #[inline]
    fn map(&self, x: f32, y: f32) -> ab_glyph_rasterizer::Point {
        ab_glyph_rasterizer::point(x * self.scale - self.ox, self.oy - y * self.scale)
    }
    /// Close the current contour with an implicit segment back to its start (TrueType/
    /// CFF outlines are implicitly closed; ab_glyph needs the closing edge for winding).
    fn close_contour(&mut self) {
        if self.last != self.start {
            self.ras.draw_line(self.last, self.start);
            self.last = self.start;
        }
    }
}

impl ttf_parser::OutlineBuilder for OutlineToRaster<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close_contour();
        let p = self.map(x, y);
        self.start = p;
        self.last = p;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.ras.draw_line(self.last, p);
        self.last = p;
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let c = self.map(x1, y1);
        let p = self.map(x, y);
        self.ras.draw_quad(self.last, c, p);
        self.last = p;
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let c0 = self.map(x1, y1);
        let c1 = self.map(x2, y2);
        let p = self.map(x, y);
        self.ras.draw_cubic(self.last, c0, c1, p);
        self.last = p;
    }
    fn close(&mut self) {
        self.close_contour();
    }
}

/// TEST-ONLY candidacy scan shared by the two FONT-2 variation tests (here and the
/// end-to-end fontdue-path test in `lib.rs`): find a wght-axis variable font AND a
/// common glyph that actually LAYS INK at the axis-min instance. A font qualifying on
/// the axis alone is NOT enough — macOS ships special-purpose VFs (e.g.
/// `ADTNumeric.ttc`) whose cmap maps letters to BLANK glyphs, so a first-axis-hit pick
/// would assert weight-variation on a glyph with no outline (a legitimate 0×0 raster
/// at every instance). Returns `(bytes, wght, char, gid)` of the first font whose
/// candidate glyph inks.
#[cfg(test)]
pub(crate) fn inked_wght_font() -> Option<(Vec<u8>, VfAxis, char, u16)> {
    for path in crate::font_files() {
        if std::fs::metadata(&path).map_or(u64::MAX, |m| m.len()) > 16 * 1024 * 1024 {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Some(p) = probe(&bytes, 0) else { continue };
        let Some(ax) = p.axes.iter().find(|a| a.tag == WGHT_TAG).copied() else {
            continue;
        };
        if ax.max <= ax.min {
            continue;
        }
        let Ok(face) = ttf_parser::Face::parse(&bytes, 0) else {
            continue;
        };
        for c in ['M', 'o', 'n', '0'] {
            let Some(gid) = face.glyph_index(c).map(|g| g.0).filter(|&g| g != 0) else {
                continue;
            };
            // Candidacy: the min-instance raster must exist AND carry ink.
            if varied_glyph_raster(&bytes, 0, &[(WGHT_TAG, ax.min)], gid, 64.0)
                .is_some_and(|(_, _, _, _, _, cov)| cov.iter().any(|&b| b > 0))
            {
                return Some((bytes, ax, c, gid));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FONT-2: the portable outline raster ACTUALLY applies the variation coords — the
    /// `wght`-min and `wght`-max instances of the same glyph produce DIFFERENT coverage.
    /// (The pre-FONT-2 fontdue path drew the `fvar` default regardless, so the two would
    /// be byte-identical.) Host-portable: skips cleanly if no variable font with an
    /// INKED common glyph is installed (see [`inked_wght_font`] — a blank-glyph VF like
    /// ADTNumeric must not be picked; its 0×0-at-every-instance raster is correct).
    #[test]
    fn varied_raster_reflects_the_weight_axis() {
        let Some((bytes, wght, _, gid)) = inked_wght_font() else {
            eprintln!("SKIP: no variable font with a wght axis + an inked glyph on this host");
            return;
        };
        let px = 64.0;
        let light = varied_glyph_raster(&bytes, 0, &[(WGHT_TAG, wght.min)], gid, px);
        let heavy = varied_glyph_raster(&bytes, 0, &[(WGHT_TAG, wght.max)], gid, px);
        let (Some(l), Some(h)) = (light, heavy) else {
            eprintln!("SKIP: raster declined for this font");
            return;
        };
        // Different instance ⇒ different raster (dims and/or coverage). This is the whole
        // point of FONT-2: the coords reach the OUTLINE, not just the metrics.
        let differ = (l.0, l.1) != (h.0, h.1) || l.5 != h.5;
        assert!(
            differ,
            "wght min vs max must yield different rasters (variation reached the raster); \
             light {}x{} heavy {}x{}",
            l.0, l.1, h.0, h.1
        );
        // Sanity: both rasters carry ink.
        let ink = |c: &[u8]| c.iter().map(|&b| u32::from(b)).sum::<u32>();
        assert!(
            ink(&l.5) > 0 && ink(&h.5) > 0,
            "both instances must lay down ink"
        );
    }

    /// [`RASTER_PAD`] EXISTS, demonstrated on the exact geometry that ate the
    /// hinted `'2'` at ppem 19 — no font, no hinter, just the rasterizer.
    ///
    /// `ab_glyph_rasterizer` marches a segment's x incrementally
    /// (`xnext = x + dxdy * dy`), and for the segment
    /// `(1.828125, 0.21875) → (0.0, 0.875)` that arithmetic overshoots to
    /// `-1.19e-7`: `floor()` is `-1`, `linestart + x0i` is negative, and the
    /// scanline's whole area is `continue`d away. `for_each_pixel` carries one
    /// running accumulator across the flat buffer, so the loss offsets every
    /// texel after it. Filled flush against the grid's left edge this triangle
    /// loses 46% of its ink and smears the rest; given ONE pixel of slack it is
    /// exact, and more slack changes nothing.
    #[test]
    fn a_boundary_hugging_outline_needs_the_raster_pad() {
        // Area by the shoelace formula: exactly what a correct fill deposits.
        let tri = [(0.0f32, 0.875f32), (1.828125, 0.21875), (1.828125, 3.0)];
        let area = 0.5
            * (0..3)
                .map(|i| {
                    let (a, b) = (tri[i], tri[(i + 1) % 3]);
                    a.0 * b.1 - b.0 * a.1
                })
                .sum::<f32>()
                .abs();
        let ink_at = |pad: usize| {
            let (w, h) = (2usize, 3usize);
            let (gw, gh) = (w + 2 * pad, h + 2 * pad);
            let mut ras = ab_glyph_rasterizer::Rasterizer::new(gw, gh);
            let m = |p: &(f32, f32)| ab_glyph_rasterizer::point(p.0 + pad as f32, p.1 + pad as f32);
            for i in 0..3 {
                ras.draw_line(m(&tri[i]), m(&tri[(i + 1) % 3]));
            }
            let mut sum = 0.0f32;
            ras.for_each_pixel(|_, a| sum += a);
            sum
        };
        let flush = ink_at(0);
        let padded = ink_at(RASTER_PAD);
        let generous = ink_at(4 * RASTER_PAD);
        assert!(
            (padded - area).abs() < 0.01,
            "with RASTER_PAD the fill must deposit the outline's area: {padded} vs {area}"
        );
        assert!(
            (generous - padded).abs() < 0.01,
            "more slack must change nothing: {generous} vs {padded}"
        );
        assert!(
            (flush - area).abs() > 0.5,
            "PRECONDITION: flush against the grid edge this outline must lose ink \
             ({flush} vs {area}) — if it no longer does, the rasterizer changed and \
             this test has stopped proving why RASTER_PAD is there"
        );
    }

    /// The PORTABLE raster's half of the [`RASTER_PAD`] law: coverage may not
    /// depend on how much empty grid surrounds the outline. The trap is the
    /// same one the hinted seam fell into — a design-space `x_min` whose scaled
    /// value is a whole pixel (a `bbox.x_min` of 0, which most glyphs of most
    /// fonts have, always is) puts the outline flush against the grid's left
    /// edge. Held to a 4×-slack refill over every printable ASCII code point
    /// and the box-drawing run, at every desktop ppem, on the bundled face.
    #[test]
    #[cfg(feature = "embedded-font")]
    fn varied_raster_is_invariant_to_grid_slack() {
        const SLACK: usize = 4;
        let bytes = crate::embedded_font();
        let face = ttf_parser::Face::parse(bytes, 0).expect("the bundled face parses");
        let upem = f32::from(face.units_per_em());
        let mut checked = 0usize;
        for pxi in 12..=40u32 {
            let px = pxi as f32;
            let scale = px / upem;
            let mut chars: Vec<char> = (' '..='~').collect();
            chars.extend("─│┌┐└┘├┤┬┴┼━┃█▀▄▌▐░▒▓".chars());
            for ch in chars {
                let Some(g) = face.glyph_index(ch) else { continue };
                let Some((w, h, _, _, _, cov)) = varied_glyph_raster_with_face(&face, g.0, px)
                else {
                    continue;
                };
                if cov.is_empty() {
                    continue;
                }
                checked += 1;
                // The same outline, same mapping, into a grid with four times
                // the production slack.
                let bbox = face.glyph_bounding_box(g).expect("an inked glyph has a bbox");
                let x_min = (f32::from(bbox.x_min) * scale).floor();
                let y_max = (f32::from(bbox.y_max) * scale).ceil();
                let mut ras = ab_glyph_rasterizer::Rasterizer::new(w + 2 * SLACK, h + 2 * SLACK);
                let mut b = OutlineToRaster {
                    ras: &mut ras,
                    scale,
                    ox: x_min - SLACK as f32,
                    oy: y_max + SLACK as f32,
                    last: ab_glyph_rasterizer::point(0.0, 0.0),
                    start: ab_glyph_rasterizer::point(0.0, 0.0),
                };
                face.outline_glyph(g, &mut b)
                    .expect("the glyph we just drew still draws");
                b.close_contour();
                let gw = w + 2 * SLACK;
                let mut reference = vec![0u8; w * h];
                ras.for_each_pixel(|i, a| {
                    let (gx, gy) = (i % gw, i / gw);
                    let (Some(x), Some(y)) = (gx.checked_sub(SLACK), gy.checked_sub(SLACK))
                    else {
                        return;
                    };
                    if x < w && y < h {
                        reference[y * w + x] = (a * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
                    }
                });
                let worst = cov
                    .iter()
                    .zip(&reference)
                    .map(|(a, b)| i32::from(*a) - i32::from(*b))
                    .map(i32::abs)
                    .max()
                    .unwrap_or(0);
                assert!(
                    worst <= 1,
                    "{ch:?} at {pxi}px: coverage moved by {worst}/255 when the fill grid \
                     gained slack — the outline is sitting on the grid boundary and the \
                     rasterizer is losing a scanline"
                );
            }
        }
        assert!(
            checked > 2_000,
            "the sweep must rasterize thousands of glyphs, got {checked}"
        );
    }

    /// A blank glyph (no outline) rasters to a 0x0 tile carrying just the advance —
    /// never a panic or a phantom bitmap.
    #[test]
    fn varied_raster_blank_glyph_is_zero_sized() {
        // Build the request against whatever variable font exists; assert the space
        // glyph (if present) yields an empty raster with a positive advance.
        for path in crate::font_files() {
            if std::fs::metadata(&path).map_or(u64::MAX, |m| m.len()) > 16 * 1024 * 1024 {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(face) = ttf_parser::Face::parse(&bytes, 0) else {
                continue;
            };
            let Some(sp) = face.glyph_index(' ').filter(|g| g.0 != 0) else {
                continue;
            };
            if let Some((w, h, _, _, adv, cov)) = varied_glyph_raster(&bytes, 0, &[], sp.0, 32.0) {
                assert_eq!((w, h), (0, 0), "space has no ink box");
                assert!(cov.is_empty(), "no coverage for a blank glyph");
                assert!(adv >= 0.0, "advance is finite and non-negative");
                return; // one real font is enough
            }
        }
        eprintln!("SKIP: no font with a space glyph to probe");
    }
}
