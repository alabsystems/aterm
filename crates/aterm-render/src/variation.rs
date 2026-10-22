// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Variable-font instantiation.
//!
//! macOS ships SF Mono as a variable font (`SFNSMono.ttf`) whose `fvar` default
//! instance is Light (`wght` ≈ 294.67), so loading it at the default renders faint,
//! thin text. The face is instantiated instead: parse `fvar` at load, resolve the
//! `Regular` named instance (or `wght = 400` clamped to the axis), overlay the user's
//! `font_variation` / `font_weight`, and apply that one coordinate list everywhere a face
//! is consumed: the CoreText descriptor (`kCTFontVariationAttribute` via
//! `CTFontDescriptorCreateCopyWithVariation`) and the shaping and metrics
//! `aterm_font::Face` (`Face::set_variation`, HVAR/MVAR-aware), so raster, shaping and
//! cell geometry agree on the instance.
//!
//! This module holds the PURE policy: axis clamping, named-instance / weight
//! resolution, config-spec parsing, and the dark-theme weight-nudge safety
//! gate — each a total function of parsed facts, so every law is
//! machine-checkable without font I/O (`tests/variation_instantiation.rs`). The only
//! impure parts are the `fvar`/`name` probes and the coordinate-applied
//! metric measurements, all bounds-checked and total (`None` on malformed
//! input, never a panic).
//!
//! On the portable raster path, [`varied_glyph_raster`] applies the resolved coords to
//! the glyph outline and fills it with [`crate::raster`], so the primary face draws the
//! resolved instance rather than the `fvar` default (`Renderer::vf_primary_gid_raster`
//! / `vf_primary_char_raster`).

/// The `wght` (weight) axis tag, big-endian packed like every OpenType tag.
pub const WGHT_TAG: u32 = u32::from_be_bytes(*b"wght");
/// CSS/OpenType Regular weight — the default instantiation target.
pub const REGULAR_WGHT: f32 = 400.0;
/// CSS/OpenType Bold weight — the real-instance replacement for synthetic
/// dilation when the primary face carries a `wght` axis.
pub const BOLD_WGHT: f32 = 700.0;
/// The dark-theme weight nudge is permitted only when the nudged instance's `'M'`
/// advance is within this many device px of the default instance's. Monospace variable
/// fonts hold advances constant across the weight axis, which is what makes a nudge safe
/// in a fixed grid; a face that fails is not nudged.
pub const DARK_NUDGE_ADVANCE_TOL_PX: f32 = 0.25;

/// One `fvar` axis, normalized like aterm-font does at parse: `min <= def <=
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
/// bounds is returned unchanged. Checked by the exhaustive lattice sweep in
/// `tests/variation_instantiation.rs`.
#[inline]
#[must_use]
pub const fn clamp_axis(axis: &VfAxis, req: f32) -> f32 {
    // `VfAxis` construction guarantees min <= def <= max, all finite; a hand-built
    // degenerate axis fails the range test and falls back to the default.
    if req.is_finite() && axis.min.is_finite() && axis.max.is_finite() && axis.min <= axis.max {
        req.clamp(axis.min, axis.max)
    } else if axis.def.is_finite() {
        axis.def
    } else {
        0.0
    }
}

/// Resolve the default instantiation coords for `axes` (one value per axis, same
/// order): the `Regular` named instance's coords when the font declares one, each
/// clamped to its axis; else every axis at its default except `wght`, pulled to `400`
/// clamped to the axis (SF Mono's default is Light).
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
/// pass, so the gate is a checked PRECONDITION, not a heuristic. Exhaustive
/// lattice + negative controls in `tests/variation_instantiation.rs`.
#[inline]
#[must_use]
pub const fn dark_nudge_permitted(adv_nudged_px: f32, adv_default_px: f32) -> bool {
    adv_nudged_px.is_finite()
        && adv_default_px.is_finite()
        && (adv_nudged_px - adv_default_px).abs() <= DARK_NUDGE_ADVANCE_TOL_PX
}

/// Parse face `index` of `bytes` for its variation facts: the `fvar` axes (via
/// aterm-font, which normalizes `min <= def <= max`) and the `Regular` named instance's
/// coords (from the raw `fvar` records, since aterm-font exposes axes only, with the
/// subfamily resolved through `name`). `None` for a non-variable or malformed face; a
/// malformed instance array degrades to `regular_coords: None`. Every read is
/// bounds-checked.
#[must_use]
pub fn probe(bytes: &[u8], index: u32) -> Option<VfProbe> {
    let face = aterm_font::Face::parse(bytes, index).ok()?;
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
fn regular_instance_coords(face: &aterm_font::Face<'_>, axis_count: usize) -> Option<Vec<f32>> {
    let data = face
        .raw_face()
        .table(aterm_font::Tag::from_bytes(b"fvar"))?;
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
fn name_is_regular(face: &aterm_font::Face<'_>, name_id: u16) -> bool {
    face.names().into_iter().any(|n| {
        n.name_id == name_id
            && n.is_unicode()
            && n.to_string()
                .is_some_and(|s| s.trim().eq_ignore_ascii_case("regular"))
    })
}

/// Coordinate-applied face metrics in device px, through aterm-font with every coord set
/// (HVAR advances; MVAR and typo-aware vertical metrics: `Face::ascender` prefers OS/2
/// typo metrics under `USE_TYPO_METRICS`, as `geometry_metrics` does unvaried).
#[derive(Clone, Copy, Debug)]
pub struct VariedMetricsPx {
    /// Above the baseline, positive.
    pub ascent: f32,
    /// Below the baseline, negative.
    pub descent: f32,
    pub line_gap: f32,
    /// `'M'` advance (the monospace cell-width probe).
    pub m_advance: f32,
}

/// The same metrics in the face's own units, from which [`VariedMetricsUnits::at_px`]
/// derives [`VariedMetricsPx`] at any size. Two instances with equal units have equal px
/// metrics, so a cell box derived from them matches at every size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VariedMetricsUnits {
    pub units_per_em: u16,
    pub ascent: i16,
    /// Negative below the baseline.
    pub descent: i16,
    pub line_gap: i16,
    pub m_advance: u16,
}

impl VariedMetricsUnits {
    /// These metrics at `px`. `None` for a degenerate `units_per_em` or a
    /// non-finite size.
    #[inline]
    #[must_use]
    pub const fn at_px(self, px: f32) -> Option<VariedMetricsPx> {
        let upem = self.units_per_em as f32;
        if upem <= 0.0 || !px.is_finite() {
            return None;
        }
        let scale = px / upem;
        Some(VariedMetricsPx {
            ascent: (self.ascent as f32) * scale,
            descent: (self.descent as f32) * scale,
            line_gap: (self.line_gap as f32) * scale,
            m_advance: (self.m_advance as f32) * scale,
        })
    }
}

/// Measure [`VariedMetricsUnits`] for face `index` of `bytes` with `coords`
/// applied. `None` when the face is malformed or has no `'M'`.
#[must_use]
pub fn varied_metrics_units(
    bytes: &[u8],
    index: u32,
    coords: &[(u32, f32)],
) -> Option<VariedMetricsUnits> {
    let mut face = aterm_font::Face::parse(bytes, index).ok()?;
    for &(tag, value) in coords {
        // A miss means no such axis, unreachable since the coords were resolved
        // against this face's own axes; ignore it rather than fail the metrics.
        let _ = face.set_variation(aterm_font::Tag(tag), value);
    }
    let gid = face.glyph_index('M')?;
    Some(VariedMetricsUnits {
        units_per_em: face.units_per_em(),
        ascent: face.ascender(),
        descent: face.descender(),
        line_gap: face.line_gap(),
        m_advance: face.glyph_hor_advance(gid)?,
    })
}

/// Measure [`VariedMetricsPx`] for face `index` of `bytes` at `px` with `coords`
/// applied: [`varied_metrics_units`] at `px`. `None` for a malformed face, no `'M'`, a
/// degenerate `units_per_em` or a non-finite `px`; the caller then uses the unvaried
/// metrics.
#[must_use]
pub fn varied_metrics_px(
    bytes: &[u8],
    index: u32,
    coords: &[(u32, f32)],
    px: f32,
) -> Option<VariedMetricsPx> {
    varied_metrics_units(bytes, index, coords)?.at_px(px)
}

/// Rasterize glyph `gid` of face `index` at `px` with `coords` (the resolved instance)
/// applied, as `(width, height, xmin, ymin, advance_width, coverage)`: `xmin` the left
/// bearing in px, `ymin` the baseline→bottom offset (negative below the baseline),
/// coverage `width*height` alpha bytes top row first. `None` on a malformed face, a
/// missing or oversized glyph, or a degenerate size, and the caller falls back to the
/// default instance. A blank glyph (space) is a 0×0 raster carrying the advance.
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

/// A face parsed once with its variation coords applied: the amortized
/// [`varied_glyph_raster`] for a caller that rasters many glyphs of one instance,
/// without an `aterm_font` dependency of its own. Each raster matches the wrapper's.
pub struct VariedFace<'a> {
    face: aterm_font::Face<'a>,
}

impl<'a> VariedFace<'a> {
    /// Parse `bytes` at collection `index` and apply `coords` (unknown tags are
    /// ignored, exactly as [`varied_glyph_raster`] ignores them). `None` on a
    /// malformed face.
    #[must_use]
    pub fn parse(bytes: &'a [u8], index: u32, coords: &[(u32, f32)]) -> Option<Self> {
        let mut face = aterm_font::Face::parse(bytes, index).ok()?;
        for &(tag, value) in coords {
            let _ = face.set_variation(aterm_font::Tag(tag), value);
        }
        Some(Self { face })
    }

    /// [`varied_glyph_raster`] through this instance: the same
    /// `(width, height, xmin, ymin, advance_width, coverage)` tuple and the same
    /// `None` cases (missing/oversized glyph, degenerate size).
    #[inline]
    #[must_use]
    pub fn glyph_raster(
        &self,
        gid: u16,
        px: f32,
    ) -> Option<(usize, usize, i32, i32, f32, Vec<u8>)> {
        varied_glyph_raster_with_face(&self.face, gid, px)
    }

    /// Glyph `gid`'s linear advance at `px` on this instance, bit-identical to what
    /// [`Self::glyph_raster`] reports, so the grid-fitted raster ([`crate::hinted`])
    /// reports the same advance as the unhinted one. `None` on a degenerate size.
    #[cfg(any(
        all(unix, not(target_os = "macos")),
        windows,
        all(test, debug_assertions, feature = "embedded-font", target_os = "macos")
    ))]
    #[inline]
    pub(crate) fn linear_advance(&self, gid: u16, px: f32) -> Option<f32> {
        let scale = px_scale(&self.face, px)?;
        Some(linear_advance(&self.face, aterm_font::GlyphId(gid), scale))
    }
}

/// `px / units_per_em`, the scale every unhinted raster and linear advance in this module
/// applies to design units. `None` for a degenerate size or `units_per_em`.
#[inline]
fn px_scale(face: &aterm_font::Face, px: f32) -> Option<f32> {
    let upem = f32::from(face.units_per_em());
    (upem > 0.0 && px.is_finite() && px > 0.0).then(|| px / upem)
}

/// Glyph `g`'s linear advance at `scale`: its `hmtx` advance (`HVAR` applied at the
/// face's coords, `0` past the table) times `scale`, the product `font::Font` reports for
/// a static face.
#[inline]
fn linear_advance(face: &aterm_font::Face, g: aterm_font::GlyphId, scale: f32) -> f32 {
    f32::from(face.glyph_hor_advance(g).unwrap_or(0)) * scale
}

/// [`varied_glyph_raster`] on an already-parsed face with the coords applied, so the
/// caller can amortize the parse (e.g. after resolving the gid through the same face's
/// cmap). The caller must build `face` at the right collection index and apply the
/// variation coords first, as [`varied_glyph_raster`] does.
#[inline]
#[must_use]
pub fn varied_glyph_raster_with_face(
    face: &aterm_font::Face,
    gid: u16,
    px: f32,
) -> Option<(usize, usize, i32, i32, f32, Vec<u8>)> {
    varied_glyph_raster_with_face_at(face, gid, px, 0.0)
}

/// [`varied_glyph_raster_with_face`] with the outline moved `dx` px right
/// (a pen's fraction of a pixel, `0.0..1.0`) before it is filled: the
/// coverage a glyph placed at a fractional pen position has, so a run of
/// small text keeps its true spacing instead of each glyph snapping to a
/// whole pixel. `xmin` is the moved ink box's left edge; at `dx = 0` the
/// raster is exactly [`varied_glyph_raster_with_face`]'s.
#[must_use]
pub fn varied_glyph_raster_with_face_at(
    face: &aterm_font::Face,
    gid: u16,
    px: f32,
    dx: f32,
) -> Option<(usize, usize, i32, i32, f32, Vec<u8>)> {
    let scale = px_scale(face, px)?;
    let dx = if dx.is_finite() { dx } else { 0.0 };
    let g = aterm_font::GlyphId(gid);
    let advance = linear_advance(face, g, scale);
    // No bounding box ⇒ a blank glyph (e.g. space): 0×0 raster, advance only.
    let Some(bbox) = face.glyph_bounding_box(g) else {
        return Some((0, 0, 0, 0, advance, Vec::new()));
    };
    // Pixel-space ink box (floor/ceil so the outline always fits inside the raster).
    let x_min = (f32::from(bbox.x_min) * scale + dx).floor();
    let x_max = (f32::from(bbox.x_max) * scale + dx).ceil();
    let y_min = (f32::from(bbox.y_min) * scale).floor();
    let y_max = (f32::from(bbox.y_max) * scale).ceil();
    let w = (x_max - x_min) as i32;
    let h = (y_max - y_min) as i32;
    if w <= 0 || h <= 0 || w > 4096 || h > 4096 {
        return None;
    }
    let (w, h) = (w as usize, h as usize);
    // Fill with RASTER_PAD px of slack on every side, then crop: the outline must never
    // sit on the grid boundary (see RASTER_PAD), and a design-space `x_min` of 0, which
    // most glyphs have, scales to exactly that.
    //
    // The grid is sized from the font's declared `glyph_bounding_box`, which its `glyf`
    // data can exceed (a loose bbox, or a hostile font). The outline is not clamped,
    // since clipping real ink would cut ordinary glyphs; containment is `crate::raster`'s
    // job (its `# Hardening` section).
    let pad = RASTER_PAD as f32;
    let mut ras = crate::raster::Rasterizer::new(w + 2 * RASTER_PAD, h + 2 * RASTER_PAD);
    let mut b = OutlineToRaster {
        ras: &mut ras,
        scale,
        ox: x_min - pad - dx,
        oy: y_max + pad,
        last: crate::raster::point(0.0, 0.0),
        start: crate::raster::point(0.0, 0.0),
    };
    // `outline_glyph` returns None for a glyph with no outline — already handled by the
    // bbox check, but stay defensive (fall back rather than emit an all-zero raster).
    face.outline_glyph(g, &mut b)?;
    b.close_contour();
    let cov = crop_padded_coverage(&ras, w, h);
    Some((w, h, x_min as i32, y_min as i32, advance, cov))
}

/// Slack, in px, between the ink box and the coverage grid every [`crate::raster`] fill
/// in this crate rasterizes into. The outline is translated by this much so it never
/// touches the grid boundary; [`crop_padded_coverage`] reads the ink box back out, so the
/// mask and its `(width, height, xmin, ymin)` metrics are unchanged.
///
/// The rasterizer marches a segment's `x` incrementally (`x += dxdy * dy`), so a segment
/// exactly on `x = 0` can drift to `-1.19e-7`, floor to `-1`, and hit the far-left skip,
/// dropping that scanline's area. One accumulator runs across the whole buffer, so every
/// later texel is offset and the glyph paints as a broken block. Grid fitting makes such
/// an edge common: the autohinter snaps stems to whole pixels and the ink box's left
/// edge is `floor(min_x)`. The right edge (`x1i == width` writes into the next row)
/// needs the same slack. `crate::subpixel` already has it: its 1 px FIR5 pad is three
/// samples at 3×.
pub(crate) const RASTER_PAD: usize = 1;

/// Read the `w`×`h` ink box back out of a grid filled with [`RASTER_PAD`] px of slack on
/// every side (the outline translated by `+PAD` on both axes). The padding ring can only
/// hold antialiasing spill from an edge already inside the box, so it is discarded.
pub(crate) fn crop_padded_coverage(ras: &crate::raster::Rasterizer, w: usize, h: usize) -> Vec<u8> {
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

/// Feeds an aterm-font glyph outline (design units, y up) into a [`crate::raster`]
/// coverage grid (pixels, y down, origin at the ink box's top-left).
struct OutlineToRaster<'a> {
    ras: &'a mut crate::raster::Rasterizer,
    scale: f32,
    ox: f32, // ink-box left edge, in px (subtracted)
    oy: f32, // ink-box top edge, in px (y is flipped about it)
    last: crate::raster::Point,
    start: crate::raster::Point,
}

impl OutlineToRaster<'_> {
    #[inline]
    fn map(&self, x: f32, y: f32) -> crate::raster::Point {
        crate::raster::point(x * self.scale - self.ox, self.oy - y * self.scale)
    }
    /// Close the current contour back to its start: outlines are implicitly closed, and
    /// the rasterizer needs the closing edge for winding.
    fn close_contour(&mut self) {
        if self.last != self.start {
            self.ras.draw_line(self.last, self.start);
            self.last = self.start;
        }
    }
}

impl aterm_font::OutlineSink for OutlineToRaster<'_> {
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

/// The checked-in variable face (`assets/game/Cinzel-var.ttf`, OFL), its `wght` axis and
/// a common glyph that lays ink at the axis-min instance, as `(bytes, wght, char, gid)`,
/// for the variation tests here and in `lib.rs`.
#[cfg(test)]
pub(crate) fn inked_wght_font() -> (Vec<u8>, VfAxis, char, u16) {
    let bytes = include_bytes!("../assets/game/Cinzel-var.ttf").to_vec();
    let ax = probe(&bytes, 0)
        .and_then(|p| p.axes.iter().find(|a| a.tag == WGHT_TAG).copied())
        .expect("Cinzel-var carries a wght axis");
    assert!(
        ax.max > ax.min,
        "Cinzel-var's wght axis spans a range: {ax:?}"
    );
    let inked = {
        let face = aterm_font::Face::parse(&bytes, 0).expect("Cinzel-var parses");
        ['M', 'o', 'n', '0'].into_iter().find_map(|c| {
            let gid = face.glyph_index(c).map(|g| g.0).filter(|&g| g != 0)?;
            varied_glyph_raster(&bytes, 0, &[(WGHT_TAG, ax.min)], gid, 64.0)
                .is_some_and(|(_, _, _, _, _, cov)| cov.iter().any(|&b| b > 0))
                .then_some((c, gid))
        })
    };
    let (c, gid) = inked.expect("Cinzel-var lays ink at its wght minimum");
    (bytes, ax, c, gid)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`RASTER_PAD`] EXISTS, demonstrated on the exact geometry that ate the
    /// hinted `'2'` at ppem 19 — no font, no hinter, just the rasterizer.
    ///
    /// [`crate::raster`] marches a segment's x incrementally
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
            let mut ras = crate::raster::Rasterizer::new(gw, gh);
            let m = |p: &(f32, f32)| crate::raster::point(p.0 + pad as f32, p.1 + pad as f32);
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

    /// The portable raster's coverage does not depend on how much empty grid surrounds
    /// the outline (a scaled `bbox.x_min` of 0 puts it flush against the left edge):
    /// held against a 4×-slack refill over printable ASCII and the box-drawing run at
    /// every desktop ppem on the bundled face.
    #[test]
    #[cfg(feature = "embedded-font")]
    fn varied_raster_is_invariant_to_grid_slack() {
        const SLACK: usize = 4;
        let bytes = crate::embedded_font();
        let face = aterm_font::Face::parse(bytes, 0).expect("the bundled face parses");
        let upem = f32::from(face.units_per_em());
        let mut checked = 0usize;
        for pxi in 12..=40u32 {
            let px = pxi as f32;
            let scale = px / upem;
            let mut chars: Vec<char> = (' '..='~').collect();
            chars.extend("─│┌┐└┘├┤┬┴┼━┃█▀▄▌▐░▒▓".chars());
            for ch in chars {
                let Some(g) = face.glyph_index(ch) else {
                    continue;
                };
                let Some((w, h, _, _, _, cov)) = varied_glyph_raster_with_face(&face, g.0, px)
                else {
                    continue;
                };
                if cov.is_empty() {
                    continue;
                }
                checked += 1;
                // The same outline and mapping, with four times the production slack.
                let bbox = face
                    .glyph_bounding_box(g)
                    .expect("an inked glyph has a bbox");
                let x_min = (f32::from(bbox.x_min) * scale).floor();
                let y_max = (f32::from(bbox.y_max) * scale).ceil();
                let mut ras = crate::raster::Rasterizer::new(w + 2 * SLACK, h + 2 * SLACK);
                let mut b = OutlineToRaster {
                    ras: &mut ras,
                    scale,
                    ox: x_min - SLACK as f32,
                    oy: y_max + SLACK as f32,
                    last: crate::raster::point(0.0, 0.0),
                    start: crate::raster::point(0.0, 0.0),
                };
                face.outline_glyph(g, &mut b)
                    .expect("the glyph we just drew still draws");
                b.close_contour();
                let gw = w + 2 * SLACK;
                let mut reference = vec![0u8; w * h];
                ras.for_each_pixel(|i, a| {
                    let (gx, gy) = (i % gw, i / gw);
                    let (Some(x), Some(y)) = (gx.checked_sub(SLACK), gy.checked_sub(SLACK)) else {
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

    /// A blank glyph (the bundled JetBrains Mono's space) rasters to a 0x0 tile carrying
    /// just the advance.
    #[test]
    fn varied_raster_blank_glyph_is_zero_sized() {
        let bytes = include_bytes!("../assets/bundled/JetBrainsMono-Regular.ttf");
        let face = aterm_font::Face::parse(bytes, 0).expect("the bundled face parses");
        let sp = face
            .glyph_index(' ')
            .filter(|g| g.0 != 0)
            .expect("the bundled face maps a space");
        let (w, h, _, _, adv, cov) =
            varied_glyph_raster(bytes, 0, &[], sp.0, 32.0).expect("the space rasters");
        assert_eq!((w, h), (0, 0), "space has no ink box");
        assert!(cov.is_empty(), "no coverage for a blank glyph");
        assert!(adv >= 0.0, "advance is finite and non-negative");
    }
}
