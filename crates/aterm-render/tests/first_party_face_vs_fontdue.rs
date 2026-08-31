// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE RETIREMENT EVIDENCE. `aterm_render::font::Font` replaced `fontdue` in
//! the shipped graph; this file is the proof that the replacement moved nothing
//! it was not allowed to move, and the MEASUREMENT of the one thing it did.
//!
//! `fontdue` is kept as a DEV-dependency for exactly this: an independent
//! second implementation, by another author, of the same job. Comparing against
//! it is the only statement that can separate "the new face is correct" from
//! "the new face is self-consistent".
//!
//! The sweep is exhaustive over both embedded faces — DejaVu Sans Mono (3 377
//! glyphs, the last-resort text face) and Symbols Nerd Font Mono (10 413, the
//! last-resort icon face) — at eight sizes, deliberately spanning fontdue's own
//! `FontSettings::scale` default of 40 px on both sides.
//!
//! # The five properties, in descending order of what a violation would cost
//!
//!  1. [`advance_widths_are_bit_identical_to_fontdue`] — `cell_w` IS an
//!     advance, so one ULP here moves the terminal GRID. Held at `==` on the
//!     raw `f32` bits.
//!  2. [`the_wider_box_is_empty_and_the_ink_never_moves`] — no ink is lost, no
//!     texel changes absolute position, and the ring the wider box adds carries
//!     less than one texel's worth of coverage across the entire sweep.
//!  3. [`coverage_tracks_fontdue_within_the_flattening_gap`] — the two flatten
//!     curves to different tolerances, so their masks differ by LSBs. Bounded,
//!     with the bound derived below and WATCHED FAILING on every run by
//!     [`the_coverage_bound_is_armed_and_its_sensitivity_is_measured`], which
//!     shares its constants.
//!  4. [`cmap_line_metrics_and_kern_agree_with_fontdue`] — the lookup tables,
//!     at `==`. `lookup_glyph_index` decides which FACE a cell draws from, so
//!     it is held over the whole BMP plus both private-use ranges.
//!  5. [`the_fit_scale_a_looser_ink_box_costs_is_bounded`] — the one MEASURED
//!     consequence of property 2's slack, priced rather than waved at.
//!
//! Plus [`an_out_of_range_glyph_id_is_empty_not_a_panic`] and
//! [`a_degenerate_size_is_empty_like_fontdue`], the deliberate differences.
//!
//! # Where the coverage bound comes from
//!
//! See [`COVERAGE_MEAN_BOUND`], which carries the derivation and the measured
//! table of what it does and does not catch.
//!
//! # Two places where fontdue is the one that is wrong
//!
//! Both are recorded here rather than silently sidestepped:
//!
//!  * fontdue only builds geometry for glyphs its cmap (or GSUB) reaches, and
//!    reports advance `0` for every OTHER glyph id. `hmtx` gives those glyphs a
//!    real advance and this crate reports it. Property 1 therefore sweeps the
//!    cmap-reachable set — every id a caller in this workspace can NAME — and
//!    [`unreachable_glyph_ids_are_where_fontdue_reports_a_zero_advance`] pins
//!    the difference so it cannot be mistaken for drift later.
//!  * fontdue's ink box is the extrema of an outline it flattened ONCE at parse
//!    time, and its `push` skips purely horizontal segments — so a box can miss
//!    ink that a horizontal stroke's endpoints define, and on an outward-bulging
//!    curve its chords stop short of the true extremum. This crate uses the
//!    face's DECLARED `glyph_bounding_box`, which is conservative in the safe
//!    direction: it can be too big (costing atlas space) but never too small
//!    (which would CLIP). Property 2 proves the containment and property 5
//!    prices the slack. Since `raster::FLATTEN_SAGITTA_PX` tightened this
//!    crate's flattening to ~37x finer than fontdue's, property 2 also MEASURES
//!    the second half of that: 22 texels across 109,704 rasters where the
//!    first-party fill reaches the sliver between fontdue's chord and the real
//!    curve, 29/255 of ink in total.

use aterm_render::font::{Font, FontSettings, Metrics};

const DEJAVU: &[u8] = include_bytes!("../assets/DejaVuSansMono.ttf");
const NERD: &[u8] = include_bytes!("../assets/SymbolsNerdFontMono-Regular.ttf");

/// 8-32 px spans "a dense pane on a 1x display" to "a large font on a 2x one".
const SIZES: [f32; 8] = [8.0, 10.0, 12.0, 14.0, 16.0, 20.0, 24.0, 32.0];

fn faces() -> [(&'static str, &'static [u8]); 2] {
    [("DejaVuSansMono", DEJAVU), ("SymbolsNerdFontMono", NERD)]
}

/// Both implementations parsed from the same bytes.
fn pair(bytes: &[u8]) -> (Font, fontdue::Font) {
    let ours = Font::from_bytes(bytes, FontSettings::default()).expect("first-party face parses");
    let theirs =
        fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).expect("fontdue parses");
    (ours, theirs)
}

fn face_of(bytes: &[u8]) -> ttf_parser::Face<'_> {
    ttf_parser::Face::parse(bytes, 0).expect("ttf-parser parses")
}

/// Every glyph id the face's cmap can produce — the ids a caller in this
/// workspace can name, since every glyph the renderer draws is reached through
/// a code point (or through `rustybuzz`, which substitutes among the same set).
fn cmap_reachable(bytes: &[u8]) -> Vec<u16> {
    let face = face_of(bytes);
    let mut seen = vec![false; usize::from(face.number_of_glyphs())];
    seen[0] = true; // `.notdef` is always addressable
    if let Some(cmap) = face.tables().cmap {
        for subtable in cmap.subtables {
            subtable.codepoints(|cp| {
                if let Some(gid) = subtable.glyph_index(cp)
                    && let Some(slot) = seen.get_mut(usize::from(gid.0))
                {
                    *slot = true;
                }
            });
        }
    }
    seen.iter()
        .enumerate()
        .filter_map(|(gid, &hit)| hit.then_some(gid as u16))
        .collect()
}

// ---------------------------------------------------------------------------
// THE THREE WAYS A DIFFERENTIAL LIES, and what is done about each here.
//
//  1. AN EMPTY CORPUS PASSES EVERYTHING. Every property below counts what it
//     actually compared PER FACE and fails on a face that contributed nothing.
//     A global total cannot do that job: the 10 413-glyph Nerd face alone
//     clears any global floor while DejaVu silently drops to zero.
//     -> `reachable_or_die`, `PerFace::require`.
//  2. A SUITE THAT HAS ONLY EVER PASSED proves nothing about the direction
//     that matters for a replacement. Every property here has been watched
//     failing under a real mutation of `src/font.rs` / `src/variation.rs`,
//     re-runnable by re-applying the stub named in the ledger below.
//
//     | # | mutation                                       | result |
//     |---|------------------------------------------------|--------|
//     | 1 | `rasterize_indexed` returns an empty bitmap     | 3 red: box, coverage, armed-bound |
//     | 2 | right box + advance, ALL-ZERO mask             | 2 red: coverage, armed-bound (the box property passes — see below) |
//     | 3 | advance scaled by 1 + 1e-6                     | 1 red: advances, on gid 0 at 8px |
//     | 4 | out-of-range `gid` guard deleted               | GREEN — equivalent mutant, see below |
//     | 5 | `advances[gid]` instead of `.get(gid)`         | 1 red: the out-of-range test, on the exact `index out of bounds` fontdue had |
//     | 6 | `floor()` instead of `round()` on the coverage byte | GREEN — a real blind spot, see below |
//
//     MUTATION 2 IS THE INFORMATIVE ONE. A glyph with the right box, the right
//     advance and no ink at all sails past
//     [`the_wider_box_is_empty_and_the_ink_never_moves`] — "every added texel
//     is zero" is trivially true of a mask that is all zero. Only the coverage
//     property catches it. That is the division of labour between properties 2
//     and 3, demonstrated rather than asserted, and it is why neither can be
//     dropped in favour of the other.
//
//     MUTATION 4 IS EQUIVALENT, not a hole. With the guard deleted,
//     `varied_glyph_raster_with_face` still returns `Some((0, 0, 0, 0, advance,
//     vec![]))` for an id past the end, because `glyph_bounding_box` gives
//     `None` — so the observable behaviour is unchanged and no test COULD see
//     it. Mutation 5 is the same defect made real, and it dies.
//
//     MUTATION 6 IS A MEASURED BLIND SPOT, recorded rather than hidden.
//     Flipping the coverage byte's rounding mode moves property 3's mean from
//     1.1924 to 1.2919 /255 — a tenth of an LSB, nowhere near the 3.0 bound.
//     A differential against a second rasterizer cannot see a quantisation
//     change of half a level, and no tolerance that admits the flattening gap
//     ever will. `tests/rasterizer_oracle.rs`'s BIT equality against
//     `ab_glyph_rasterizer` is the guard that covers this class; that is the
//     split the decision doc chose, and this is the measurement behind it.
//  3. TOLERANCE THEATRE. A bound nobody has seen fail is a number, not a
//     bound. [`the_coverage_bound_is_armed_and_its_sensitivity_is_measured`]
//     drives DEFECTIVE rasters through the SAME comparator and the SAME
//     constants as property 3 and requires a red verdict — on every run, not
//     once by hand.
// ---------------------------------------------------------------------------

/// The per-face floor on cmap-reachable glyphs. Both embedded faces reach
/// thousands; a face that suddenly reaches a handful means the cmap walk broke,
/// and a sweep over a handful proves nothing while still printing `ok`.
const MIN_REACHABLE_PER_FACE: usize = 3_000;

/// [`cmap_reachable`] with the vacuity floor applied and the face NAMED in the
/// failure.
fn reachable_or_die(name: &str, bytes: &[u8]) -> Vec<u16> {
    let reachable = cmap_reachable(bytes);
    assert!(
        reachable.len() >= MIN_REACHABLE_PER_FACE,
        "{name}: only {} cmap-reachable glyphs, floor {MIN_REACHABLE_PER_FACE} — \
         the sweep would be vacuous and would still report ok",
        reachable.len()
    );
    reachable
}

/// Per-face comparison counts, so "how much did this property actually check"
/// is answerable for EACH face and not only in total.
#[derive(Default)]
struct PerFace(Vec<(String, u64)>);

impl PerFace {
    fn add(&mut self, name: &str, n: u64) {
        self.0.push((name.to_string(), n));
    }

    /// Every face must have contributed at least `floor`. Returns the total.
    fn require(&self, floor: u64, what: &str) -> u64 {
        assert_eq!(
            self.0.len(),
            faces().len(),
            "{} of {} faces were swept for {what} — a face was skipped entirely",
            self.0.len(),
            faces().len()
        );
        for (name, n) in &self.0 {
            assert!(
                *n >= floor,
                "{name} contributed {n} {what}, floor {floor} — this face went \
                 vacuous and the other face's count was covering for it"
            );
        }
        self.0.iter().map(|(_, n)| n).sum()
    }

    /// `name=count` for the log line.
    fn report(&self) -> String {
        self.0
            .iter()
            .map(|(n, c)| format!("{n}={c}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Mean |Δ| over every shared texel, in 0..=255 units.
///
/// DERIVED, not fitted. An 8-bit mask quantises coverage at 1/255, and a
/// flattening chord that deviates from the true curve by `t` px displaces the
/// edge by at most `t` and so perturbs a cell's coverage by at most `t`. The
/// survey behind `docs/measured/fontdue-oracle-decision-2026-08-29.md` measures
/// both rasterizers against an exact analytic reference: fontdue **0.252/255**
/// mean, this crate's fill **0.073/255** since `raster::FLATTEN_SAGITTA_PX`
/// replaced `ab_glyph_rasterizer`'s 0.144 px flattening budget. Their
/// difference from EACH OTHER is bounded by the sum, ~0.325/255 predicted —
/// property 3 reads **0.366/255** over 30.0 M texels, which is that prediction
/// confirmed. 1.0 is 2.7x the reading.
///
/// This constant was 3.0 while the first-party fill scored 0.810/255 and the
/// reading was 1.19/255; the tightening moved both, so the ceiling came down
/// with them rather than being left as slack nobody would notice accumulating
/// into.
///
/// AND IT HAS BEEN WATCHED FAILING.
/// [`the_coverage_bound_is_armed_and_its_sensitivity_is_measured`] drives
/// defective rasters through this same constant on every run. The measured
/// trip points, on DejaVu at 12/16 px (a subset whose honest baseline is
/// 0.41/255, so the real headroom is 2.4x):
///
/// | defect                       | mean \|Δ\| | verdict |
/// |------------------------------|-----------|---------|
/// | none (control)               | 0.41      | pass    |
/// | scale +0.05 % (real raster)  | 0.58      | pass    |
/// | scale +0.1 %  (real raster)  | 0.82      | pass    |
/// | scale +0.2 %  (real raster)  | 1.31      | CAUGHT  |
/// | scale +0.5 %  (real raster)  | 2.92      | CAUGHT  |
/// | scale +1 %    (real raster)  | 5.66      | CAUGHT  |
/// | translate 0.01 px            | 0.89      | pass    |
/// | translate 0.02 px            | 1.48      | CAUGHT  |
/// | translate 0.05 px            | 3.40      | CAUGHT  |
/// | translate 0.10 px            | 6.66      | CAUGHT  |
///
/// So the honest statement of what this file holds is: a systematic scale error
/// of 0.2 % or a systematic translation of 0.02 px is caught; anything quieter
/// than that is not. Both trip points are ~2.5x finer than they were at the old
/// flattening — the accuracy the tightening bought is accuracy this differential
/// now spends on sensitivity. That is a bound. ONE constant serves both the
/// guard and the proof it fires, so the two can never drift apart.
const COVERAGE_MEAN_BOUND: f32 = 1.0;

/// Max |Δ| on any single texel. A texel a curve edge crosses concentrates the
/// whole flattening disagreement in one cell, so this is necessarily far looser
/// than the mean — but the survey measured a RASTER_PAD grid escape at 105 MEAN
/// and 167 max, so 96 still separates "flattens differently" from "draws the
/// wrong thing". That separation is what sets this number, so unlike
/// [`COVERAGE_MEAN_BOUND`] it did NOT come down with the flattening; the
/// reading did, from 71/255 to 42/255.
const COVERAGE_MAX_BOUND: u8 = 96;

/// A deliberate defect injected into the FIRST-PARTY side of the comparison, so
/// the bound can be watched rejecting rather than only accepting.
#[derive(Clone, Copy, Debug)]
enum Defect {
    /// The honest face. The control.
    None,
    /// Rasterize at `px * f` while being asked for `px` — exactly what a wrong
    /// `units_per_em`, a wrong `scale_factor` or a stale DPR produces. This
    /// runs the REAL first-party code path end to end; nothing is synthesised.
    Scale(f32),
    /// Translate the finished mask `dx` px right by linear resampling — the
    /// half-texel class (a rounding-mode change in the sampling grid), which
    /// [`Defect::Scale`] does not cover because a translation leaves the
    /// glyph's SIZE alone. This one IS synthetic: linear resampling is an exact
    /// translation only on a linear ramp and blurs across a sharp edge, so it
    /// APPROXIMATES a true sub-texel shift rather than reproducing one. Read
    /// its numbers as the scale of the class, not as a calibration.
    ShiftX(f32),
}

/// One first-party raster, defect applied.
fn raster_with(ours: &Font, gid: u16, px: f32, defect: Defect) -> (Metrics, Vec<u8>) {
    match defect {
        Defect::None => ours.rasterize_indexed(gid, px),
        Defect::Scale(f) => ours.rasterize_indexed(gid, px * f),
        Defect::ShiftX(dx) => {
            let (m, cov) = ours.rasterize_indexed(gid, px);
            if m.width == 0 {
                return (m, cov);
            }
            let mut out = vec![0u8; cov.len()];
            for row in 0..m.height {
                for col in 0..m.width {
                    let here = f32::from(cov[row * m.width + col]);
                    let left = if col == 0 {
                        0.0
                    } else {
                        f32::from(cov[row * m.width + col - 1])
                    };
                    out[row * m.width + col] =
                        (here * (1.0 - dx) + left * dx).round().clamp(0.0, 255.0) as u8;
                }
            }
            (m, out)
        }
    }
}

/// The measured disagreement between the two masks, and the verdict the bounds
/// return on it.
#[derive(Default)]
struct Divergence {
    texels: u64,
    total_abs: f64,
    worst: u8,
    worst_at: String,
    per_face: PerFace,
}

impl Divergence {
    fn mean(&self) -> f32 {
        if self.texels == 0 {
            return f32::INFINITY; // an empty comparison is never a pass
        }
        (self.total_abs / self.texels as f64) as f32
    }

    /// THE bound, in one place. `Err` carries the reason so the armed test can
    /// print WHY a defect was caught rather than merely that it was.
    fn verdict(&self) -> Result<(), String> {
        if self.texels == 0 {
            return Err("no texels were compared at all".to_string());
        }
        let mean = self.mean();
        if mean > COVERAGE_MEAN_BOUND {
            return Err(format!(
                "mean |Δ| {mean:.4}/255 exceeds {COVERAGE_MEAN_BOUND}/255"
            ));
        }
        if self.worst > COVERAGE_MAX_BOUND {
            return Err(format!(
                "worst texel {}/255 at {} exceeds {COVERAGE_MAX_BOUND}/255",
                self.worst, self.worst_at
            ));
        }
        Ok(())
    }

    fn line(&self) -> String {
        format!(
            "mean |Δ| {:.4}/255, max {}/255 at {} over {} shared texels",
            self.mean(),
            self.worst,
            if self.worst_at.is_empty() {
                "-"
            } else {
                &self.worst_at
            },
            self.texels
        )
    }
}

/// Compare one face's masks texel-for-texel at matching ABSOLUTE positions —
/// never by index, which would silently compare different pixels whenever the
/// two boxes differ — and fold the result into `out`.
fn accumulate(
    out: &mut Divergence,
    name: &str,
    ours: &Font,
    theirs: &fontdue::Font,
    gids: &[u16],
    sizes: &[f32],
    defect: Defect,
) {
    let before = out.texels;
    for &px in sizes {
        for &gid in gids {
            let (a, acov) = raster_with(ours, gid, px, defect);
            let (b, bcov) = theirs.rasterize_indexed(gid, px);
            if a.width == 0 || b.width == 0 {
                continue;
            }
            let (al, ab) = (i64::from(a.xmin), i64::from(a.ymin));
            let (bl, bb) = (i64::from(b.xmin), i64::from(b.ymin));
            let (a_top, b_top) = (ab + a.height as i64, bb + b.height as i64);
            for row in 0..a.height {
                let y = a_top - 1 - row as i64;
                let brow = b_top - 1 - y;
                if brow < 0 || brow >= b.height as i64 {
                    continue;
                }
                for col in 0..a.width {
                    let bcol = al + col as i64 - bl;
                    if bcol < 0 || bcol >= b.width as i64 {
                        continue;
                    }
                    let d = acov[row * a.width + col]
                        .abs_diff(bcov[brow as usize * b.width + bcol as usize]);
                    out.total_abs += f64::from(d);
                    out.texels += 1;
                    if d > out.worst {
                        out.worst = d;
                        out.worst_at = format!("{name} gid {gid} @ {px}px");
                    }
                }
            }
        }
    }
    out.per_face.add(name, out.texels - before);
}

/// PROPERTY 1. The advance is the terminal grid.
///
/// `cell_w` is `metrics('M').advance_width`, every column position is a
/// multiple of it, and every test in the workspace that asserts a pixel column
/// inherits it. Held at BIT equality — not `approx_eq`, not a tolerance — over
/// every ADDRESSABLE glyph of both embedded faces at every size in [`SIZES`].
#[test]
fn advance_widths_are_bit_identical_to_fontdue() {
    let mut per_face = PerFace::default();
    let mut checked = 0u64;
    for (name, bytes) in faces() {
        let (ours, theirs) = pair(bytes);
        let reachable = reachable_or_die(name, bytes);
        let before = checked;
        for px in SIZES {
            for &gid in &reachable {
                let a = ours.metrics_indexed(gid, px).advance_width;
                let b = theirs.metrics_indexed(gid, px).advance_width;
                assert_eq!(
                    a.to_bits(),
                    b.to_bits(),
                    "{name}: advance for gid {gid} at {px}px is {a} first-party vs {b} fontdue"
                );
                checked += 1;
            }
        }
        per_face.add(name, checked - before);
    }
    let total = per_face.require(20_000, "advance comparisons");
    assert_eq!(total, checked);
    eprintln!(
        "advances bit-identical on {checked}/{checked} (addressable glyph, size) pairs [{}]",
        per_face.report()
    );
}

/// The counterpart to property 1: OUTSIDE the set fontdue chose to LOAD, the
/// two disagree, and it is fontdue that is wrong. Pinned so the disagreement
/// stays understood rather than becoming folklore — and so a future change that
/// made the first-party face ALSO report zero would be caught as the regression
/// it is.
///
/// "The set fontdue loaded" is cmap-reachable UNION GSUB-reachable (its
/// `FontSettings::load_substitutions` default), and the GSUB half is not
/// enumerable from here — so the discriminator is fontdue's OWN answer: every
/// glyph it reports a zero advance for while `hmtx` says otherwise.
#[test]
fn unreachable_glyph_ids_are_where_fontdue_reports_a_zero_advance() {
    let bytes = DEJAVU;
    let (ours, theirs) = pair(bytes);
    let face = face_of(bytes);
    let mut zeroed = 0u64;
    for gid in 0..face.number_of_glyphs() {
        let hmtx = face
            .glyph_hor_advance(ttf_parser::GlyphId(gid))
            .unwrap_or(0);
        let a = ours.metrics_indexed(gid, 16.0).advance_width;
        let b = theirs.metrics_indexed(gid, 16.0).advance_width;
        if hmtx == 0 || b != 0.0 {
            continue; // no disagreement to pin
        }
        assert!(
            a > 0.0,
            "gid {gid} has hmtx advance {hmtx} but the first-party face reported {a}"
        );
        zeroed += 1;
    }
    assert!(
        zeroed > 0,
        "DejaVu has no glyph fontdue leaves unloaded — this test needs a face that does"
    );
    eprintln!("{zeroed} DejaVu glyphs carry a real hmtx advance fontdue reports as 0");
}

/// PROPERTY 2. No ink is lost, no ink appears, and nothing moves.
///
/// The two derive the ink box differently on purpose (see the module docs), so
/// the boxes are NOT equal. What must hold — and what actually matters on
/// screen — is stronger and more specific than equality:
///
///  * CONTAINMENT: the first-party box contains fontdue's on all four sides, so
///    a glyph can never be clipped by the swap.
///  * ALIGNMENT: every texel the two share sits at the same ABSOLUTE position
///    (`xmin + col`, and the baseline-relative row), so nothing shifts.
///  * EMPTINESS: every texel the first-party box adds is ZERO, so the extra
///    area costs atlas space and nothing else.
///
/// The slack is bounded too, because "contains" alone would permit an
/// unboundedly large box.
#[test]
fn the_wider_box_is_empty_and_the_ink_never_moves() {
    /// Widest slack, in px, between the declared box and fontdue's outline box
    /// on any one side. Measured maximum over the whole sweep is 9 (a Nerd
    /// Font icon at 32px whose declared box is generous); 12 leaves room for a
    /// font update without leaving room for a defect.
    const SLACK_BOUND: i64 = 12;
    /// Heaviest single texel of RING ink — first-party coverage in a texel
    /// fontdue's ink box excludes. See the loop below for what these two
    /// numbers are separating. Measured maximum over the whole sweep is 4/255;
    /// 8 is the same LSB-scale ceiling the accuracy guard puts on a worst cell.
    const RING_TEXEL_BOUND: u8 = 8;
    /// TOTAL ring ink over the whole sweep, in 1/255ths — i.e. "less than one
    /// fully covered texel, across all 109,704 rasters". Measured total is
    /// 29/255. This is the bound that does the work: a first-party box that was
    /// genuinely too big and genuinely inked would blow past it immediately,
    /// however light each individual texel was.
    const RING_TOTAL_BOUND: u32 = 255;

    let mut per_face = PerFace::default();
    let mut compared = 0u64;
    let mut identical = 0u64;
    let mut worst_slack = (0i64, String::new());
    let mut ring_total = 0u32;
    let mut ring_texels = 0u64;
    let mut worst_ring = (0u8, String::new());
    for (name, bytes) in faces() {
        let (ours, theirs) = pair(bytes);
        let reachable = reachable_or_die(name, bytes);
        let before = compared;
        for px in SIZES {
            for &gid in &reachable {
                let (a, acov) = ours.rasterize_indexed(gid, px);
                let (b, bcov) = theirs.rasterize_indexed(gid, px);
                if a.width == 0 && b.width == 0 {
                    continue;
                }
                compared += 1;
                let at = format!("{name} gid {gid} @ {px}px");
                identical += u64::from(
                    (a.width, a.height, a.xmin, a.ymin) == (b.width, b.height, b.xmin, b.ymin),
                );

                // Containment, side by side, in absolute pixel coordinates.
                let (al, ar) = (i64::from(a.xmin), i64::from(a.xmin) + a.width as i64);
                let (bl, br) = (i64::from(b.xmin), i64::from(b.xmin) + b.width as i64);
                let (ab, at_) = (i64::from(a.ymin), i64::from(a.ymin) + a.height as i64);
                let (bb, bt) = (i64::from(b.ymin), i64::from(b.ymin) + b.height as i64);
                for (slack, side) in [
                    (bl - al, "left"),
                    (ar - br, "right"),
                    (bb - ab, "bottom"),
                    (at_ - bt, "top"),
                ] {
                    assert!(
                        slack >= 0,
                        "{at}: the first-party box CUTS fontdue's on the {side} \
                         ({}x{} at ({},{}) vs {}x{} at ({},{}))",
                        a.width,
                        a.height,
                        a.xmin,
                        a.ymin,
                        b.width,
                        b.height,
                        b.xmin,
                        b.ymin
                    );
                    if slack > worst_slack.0 {
                        worst_slack = (slack, format!("{at} ({side})"));
                    }
                }

                // Emptiness of the added ring: first-party ink outside
                // fontdue's box.
                //
                // This was `== 0` while both rasterizers flattened at
                // `ab_glyph_rasterizer`'s 0.144 px sagitta. It is not any more,
                // and the reason is fontdue's box rather than this crate's ink:
                // fontdue's ink box is the extrema of an outline it flattened
                // ONCE at parse time (see the header), so on an outward-bulging
                // curve its chords stop short of the true extremum and its box
                // stops with them. `raster::FLATTEN_SAGITTA_PX` now targets
                // 1/255 px, ~37x finer, so the first-party fill reaches the
                // sliver between fontdue's chord and the real curve — and lays
                // it down in the ring, inside the DECLARED box that sized the
                // grid.
                //
                // Measured over all 109,704 reachable rasters of both faces:
                // 22 such texels, all on the Nerd face, 19 of them at 1/255,
                // heaviest 4/255, 29/255 of ink in total. Bounded rather than
                // waived: the property this is really holding — "the first-party
                // box is loose, never wrong" — survives, because a box that was
                // genuinely too big would fill its ring with real coverage.
                for row in 0..a.height {
                    for col in 0..a.width {
                        let x = al + col as i64;
                        let y = at_ - 1 - row as i64;
                        if x >= bl && x < br && y >= bb && y < bt {
                            continue;
                        }
                        let v = acov[row * a.width + col];
                        if v == 0 {
                            continue;
                        }
                        assert!(
                            v <= RING_TEXEL_BOUND,
                            "{at}: texel at ({x},{y}) is OUTSIDE fontdue's ink box and carries \
                             coverage {v}/255, past the {RING_TEXEL_BOUND}/255 sliver bound — \
                             that is ink, not the flattening gap, and the declared bounding \
                             box is not merely loose"
                        );
                        ring_total += u32::from(v);
                        ring_texels += 1;
                        if v > worst_ring.0 {
                            worst_ring = (v, format!("{at} ({x},{y})"));
                        }
                    }
                }
                // ...and symmetrically, fontdue must have no ink outside OUR
                // box either. Containment makes this vacuous, which is the
                // point: it is the assertion that would fire first if
                // containment were ever weakened.
                for row in 0..b.height {
                    for col in 0..b.width {
                        let x = bl + col as i64;
                        let y = bt - 1 - row as i64;
                        if x >= al && x < ar && y >= ab && y < at_ {
                            continue;
                        }
                        assert_eq!(
                            bcov[row * b.width + col],
                            0,
                            "{at}: fontdue has ink the first-party box does not cover"
                        );
                    }
                }
            }
        }
        per_face.add(name, compared - before);
    }
    let total = per_face.require(20_000, "rasterized glyphs");
    assert_eq!(total, compared);
    assert!(
        ring_total <= RING_TOTAL_BOUND,
        "the first-party masks lay {ring_total}/255 of ink outside fontdue's ink boxes across \
         {ring_texels} texels (heaviest {} at {}), past the {RING_TOTAL_BOUND}/255 total — \
         that is more than the flattening gap can account for",
        worst_ring.0,
        worst_ring.1
    );
    assert!(
        worst_slack.0 <= SLACK_BOUND,
        "ink box slack {} px at {} exceeds {SLACK_BOUND}",
        worst_slack.0,
        worst_slack.1
    );
    eprintln!(
        "ink boxes: {identical}/{compared} identical to fontdue, the rest contain it \
         (worst slack {} px at {}); ring ink {ring_total}/255 over {ring_texels} texels \
         (heaviest {} at {}) [{}]",
        worst_slack.0,
        worst_slack.1,
        worst_ring.0,
        worst_ring.1,
        per_face.report()
    );
}

/// PROPERTY 3. The mask itself.
///
/// Compared where the boxes OVERLAP, texel against texel at the same absolute
/// position — not by index, which would silently compare different pixels
/// whenever the boxes differ.
///
/// Two statistics, because they fail differently: the MEAN catches a systematic
/// error (a gain, a rounding-mode flip, a half-texel shift) and the MAX catches
/// a structural one (a dropped contour, an escaped grid, an inverted winding).
#[test]
fn coverage_tracks_fontdue_within_the_flattening_gap() {
    let mut d = Divergence::default();
    for (name, bytes) in faces() {
        let (ours, theirs) = pair(bytes);
        let reachable = reachable_or_die(name, bytes);
        accumulate(
            &mut d,
            name,
            &ours,
            &theirs,
            &reachable,
            &SIZES,
            Defect::None,
        );
    }
    // PER FACE, not in total: the Nerd face alone would clear a global floor.
    let total = d.per_face.require(1_000_000, "shared texels");
    eprintln!(
        "coverage vs fontdue: {} [{}]",
        d.line(),
        d.per_face.report()
    );
    assert_eq!(total, d.texels);
    if let Err(why) = d.verdict() {
        panic!("the first-party mask left the flattening gap: {why}");
    }
}

/// THE THIRD LIE, closed on every run: a tolerance nobody has seen fail is a
/// number, not a bound.
///
/// This drives DEFECTIVE first-party rasters through the SAME [`accumulate`]
/// comparator and the SAME [`COVERAGE_MEAN_BOUND`] / [`COVERAGE_MAX_BOUND`]
/// constants property 3 uses, and requires a RED verdict. If someone widens the
/// bound to quiet a failure, this test goes red too — which is the only
/// structural reason to believe property 3 is a guard and not decoration.
///
/// It also answers the question a bound alone cannot: HOW WRONG does the face
/// have to be before this file notices? The sweep prints the trip point.
/// [`Defect::Scale`] is the honest half — a real raster from the real code path
/// at a slightly wrong size, which is what a broken `scale_factor` or
/// `units_per_em` produces.
#[test]
fn the_coverage_bound_is_armed_and_its_sensitivity_is_measured() {
    // One face, two sizes: enough texels for the mean to be stable (≈1.6 M)
    // and small enough to sweep a dozen defects.
    let bytes = DEJAVU;
    let name = "DejaVuSansMono";
    let (ours, theirs) = pair(bytes);
    let reachable = reachable_or_die(name, bytes);
    let sizes = [12.0f32, 16.0];

    let measure = |defect: Defect| {
        let mut d = Divergence::default();
        accumulate(&mut d, name, &ours, &theirs, &reachable, &sizes, defect);
        d
    };

    // The CONTROL. The same corpus, the same comparator, no defect: it must
    // pass, or every rejection below is just a broken harness rejecting
    // everything.
    let control = measure(Defect::None);
    assert!(
        control.texels > 500_000,
        "the control compared only {} texels — the sensitivity readings below \
         would be noise",
        control.texels
    );
    assert!(
        control.verdict().is_ok(),
        "the undefected control is already red — every rejection below is \
         meaningless: {:?}",
        control.verdict()
    );
    eprintln!(
        "\nARMED-BOUND SWEEP (bound: mean <= {COVERAGE_MEAN_BOUND}/255, max <= {COVERAGE_MAX_BOUND}/255)"
    );
    eprintln!("  control (no defect): {} -> PASS", control.line());

    // The sweep. Every entry is measured and printed whether it trips or not,
    // so the trip point is a READING rather than a claim.
    let mut caught = Vec::new();
    for defect in [
        Defect::Scale(1.0005),
        Defect::Scale(1.001),
        Defect::Scale(1.002),
        Defect::Scale(1.005),
        Defect::Scale(1.01),
        Defect::Scale(1.02),
        Defect::ShiftX(0.01),
        Defect::ShiftX(0.02),
        Defect::ShiftX(0.05),
        Defect::ShiftX(0.1),
        Defect::ShiftX(0.25),
    ] {
        let d = measure(defect);
        assert!(
            d.texels > 0,
            "{defect:?} produced no comparable texels — the defect destroyed the \
             corpus instead of perturbing it, so a red verdict would prove nothing"
        );
        match d.verdict() {
            Ok(()) => eprintln!("  {defect:?}: {} -> pass", d.line()),
            Err(why) => {
                eprintln!("  {defect:?}: {} -> CAUGHT ({why})", d.line());
                caught.push(defect);
            }
        }
    }

    // THE PINNED SENSITIVITY. These two must be caught at the SHIPPED bound —
    // not at a bound tightened for the occasion. A 1% scale error moves a 16px
    // glyph by a sixth of a pixel at its right edge, and a tenth-pixel
    // translation is the half-texel class at its quietest; a differential that
    // cannot see either is not holding a rasterizer to anything.
    //
    // Only these two are ASSERTED, and both clear the bound by ~6x (5.66 and
    // 6.66 against 1.0), so they survive whatever a different target's float
    // rounding does to the fourth decimal. The rows in the sweep above that sit
    // close to the line — `ShiftX(0.01)` at 0.89 — are PRINTED, never asserted,
    // precisely because an 11% margin is not a portable claim.
    for required in [Defect::Scale(1.01), Defect::ShiftX(0.1)] {
        let d = measure(required);
        assert!(
            d.verdict().is_err(),
            "{required:?} passed the bound: {} — {COVERAGE_MEAN_BOUND}/255 is a \
             number, not a guard, and property 3 is decoration",
            d.line()
        );
    }
    assert!(
        caught.len() >= 2,
        "only {} of the swept defects were caught",
        caught.len()
    );
    eprintln!(
        "  => {} of 11 defects caught at the shipped bound; the honest face passes\n",
        caught.len()
    );
}

/// PROPERTY 4. The lookup tables — cmap, `hhea` line metrics, legacy `kern`.
///
/// `lookup_glyph_index` is held at equality over the whole BMP plus both
/// private-use ranges, because it decides WHICH FACE a cell draws from: the
/// styled/fallback routing lattice is proven against fontdue's
/// enumerated-cmap answer, and the first-party face reproduces that
/// enumeration deliberately (see `aterm_render::font`'s module docs).
#[test]
fn cmap_line_metrics_and_kern_agree_with_fontdue() {
    let mut mapped = 0u64;
    for (name, bytes) in faces() {
        let (ours, theirs) = pair(bytes);
        let before = mapped;

        for cp in (0u32..=0xFFFF)
            .chain(0xF0000..=0xF00FF)
            .chain(0x10FF00..=0x10FFFF)
        {
            let Some(ch) = char::from_u32(cp) else {
                continue;
            };
            let a = ours.lookup_glyph_index(ch);
            let b = theirs.lookup_glyph_index(ch);
            assert_eq!(a, b, "{name}: cmap for U+{cp:04X} is {a} vs fontdue {b}");
            mapped += u64::from(a != 0);
        }
        assert!(
            mapped - before >= 1_000,
            "{name}: the cmap sweep mapped only {} code points — vacuous",
            mapped - before
        );

        for px in SIZES {
            let a = ours.horizontal_line_metrics(px).expect("line metrics");
            let b = theirs.horizontal_line_metrics(px).expect("line metrics");
            assert_eq!(
                [
                    a.ascent.to_bits(),
                    a.descent.to_bits(),
                    a.line_gap.to_bits(),
                    a.new_line_size.to_bits()
                ],
                [
                    b.ascent.to_bits(),
                    b.descent.to_bits(),
                    b.line_gap.to_bits(),
                    b.new_line_size.to_bits()
                ],
                "{name}: line metrics differ at {px}px: {a:?} vs fontdue {b:?}"
            );
        }

        // Legacy `kern`, over the printable ASCII square. NEITHER embedded face
        // carries a `kern` table (both kern through GPOS, which is
        // `ligature_shaping`'s job), so this half is currently an agreement on
        // `None` — which is exactly what would break first if the table lookup
        // started inventing values.
        for l in ' '..='~' {
            for r in ' '..='~' {
                let a = ours.horizontal_kern(l, r, 16.0);
                let b = theirs.horizontal_kern(l, r, 16.0);
                assert_eq!(
                    a.map(f32::to_bits),
                    b.map(f32::to_bits),
                    "{name}: kern {l:?}{r:?} is {a:?} vs fontdue {b:?}"
                );
            }
        }
    }
    eprintln!("cmap agreed on the full sweep ({mapped} mapped code points)");
}

/// PROPERTY 5. What the looser ink box actually COSTS, priced.
///
/// `fallback_fit_scale` shrinks a proportional fallback raster to fit its
/// terminal cell, and it reads the ink box — so a box that is larger than the
/// ink shrinks the glyph slightly more than necessary. That is the ONE
/// user-visible consequence of property 2's slack, and it is measured here
/// rather than argued about.
///
/// It reaches the screen only where the portable face is the raster actually
/// used: wasm, and the fail-safe behind CoreText (macOS) or the grid-fitted
/// `hinted` path (Linux/Windows), both of which derive their own tight boxes.
#[test]
fn the_fit_scale_a_looser_ink_box_costs_is_bounded() {
    /// Worst per-glyph shrink, as a fraction. MEASURED maximum over the sweep
    /// is 0.191, on DejaVu's U+10FA (Georgian letter Han, gid 1291), whose
    /// DECLARED bounding box runs to x 1323 while its outline stops near 1000 —
    /// so the fitted glyph is ~19% smaller than fontdue would have drawn it.
    /// It is the only glyph in either embedded face past 0.11 (gid 2695 is
    /// 0.105; every other one of the 13 713 is under 0.07), and it reaches the
    /// screen only on the fail-safe raster path. 0.25 is the bound: it holds
    /// the known worst case with room for a font update and would still catch a
    /// glyph fitted to half its size.
    const WORST_BOUND: f32 = 0.25;
    /// Mean shrink over every fitted glyph. MEASURED at 0.0037 — this is the
    /// statistic that would move if the slack became systematic rather than a
    /// property of two unusual glyphs.
    const MEAN_BOUND: f32 = 0.01;

    // A 16px face in a 10x21 cell — the shape of a real desktop terminal cell
    // at that size, and the case where a symbol fallback is actually fitted.
    let (cell_w, cell_h, px) = (10usize, 21usize, 16.0f32);
    let mut per_face = PerFace::default();
    let mut worst = (0f32, String::new());
    let mut total = 0f64;
    let mut fitted = 0u64;
    for (name, bytes) in faces() {
        let (ours, theirs) = pair(bytes);
        let before = fitted;
        for &gid in &reachable_or_die(name, bytes) {
            let a = ours.metrics_indexed(gid, px);
            let b = theirs.metrics_indexed(gid, px);
            if a.width == 0 || b.width == 0 {
                continue;
            }
            let sa = aterm_render::fallback_fit_scale(
                cell_w,
                cell_h,
                a.width,
                a.height,
                a.xmin,
                a.advance_width,
            );
            let sb = aterm_render::fallback_fit_scale(
                cell_w,
                cell_h,
                b.width,
                b.height,
                b.xmin,
                b.advance_width,
            );
            // The looser box can only fit SMALLER, never larger.
            assert!(
                sa <= sb + 1e-6,
                "{name} gid {gid}: the first-party box fitted LARGER ({sa} vs {sb})"
            );
            let d = sb - sa;
            total += f64::from(d);
            fitted += 1;
            if d > worst.0 {
                worst = (d, format!("{name} gid {gid}"));
            }
        }
        per_face.add(name, fitted - before);
    }
    let swept = per_face.require(1_000, "fitted glyphs");
    assert_eq!(swept, fitted);
    let mean = (total / fitted as f64) as f32;
    eprintln!(
        "fallback fit-scale vs fontdue over {fitted} glyphs: mean shrink {mean:.5}, \
         worst {:.5} at {} [{}]",
        worst.0,
        worst.1,
        per_face.report()
    );
    assert!(
        mean <= MEAN_BOUND,
        "mean shrink {mean} exceeds {MEAN_BOUND}"
    );
    assert!(
        worst.0 <= WORST_BOUND,
        "worst shrink {} at {} exceeds {WORST_BOUND}",
        worst.0,
        worst.1
    );
}

/// THE DELIBERATE DIFFERENCE, pinned so it cannot regress back.
///
/// fontdue's `rasterize_indexed` is `&self.glyphs[index as usize]` — an
/// out-of-range glyph id PANICS. This crate takes glyph ids from shaping, from
/// other faces' cmaps and from `sbix`/`COLR` records, so an id this face does
/// not have is a routing outcome and must cost an empty raster, not a crash.
///
/// The advance is checked to be zero too: a glyph that does not exist must not
/// move the pen either.
#[test]
fn an_out_of_range_glyph_id_is_empty_not_a_panic() {
    for (name, bytes) in faces() {
        let ours = Font::from_bytes(bytes, FontSettings::default()).expect("parses");
        let n = face_of(bytes).number_of_glyphs();
        for gid in [n, n.wrapping_add(1), u16::MAX] {
            let (m, cov) = ours.rasterize_indexed(gid, 16.0);
            assert!(
                cov.is_empty() && m.width == 0 && m.height == 0,
                "{name}: gid {gid} (count {n}) rasterized {}x{} with {} bytes",
                m.width,
                m.height,
                cov.len()
            );
            assert_eq!(
                m.advance_width, 0.0,
                "{name}: gid {gid} past the end still advanced the pen"
            );
            let m2 = ours.metrics_indexed(gid, 16.0);
            assert_eq!(
                (m2.width, m2.height, m2.advance_width),
                (0, 0, 0.0),
                "{name}: metrics for out-of-range gid {gid} were not empty"
            );
        }
    }
}

/// A non-positive or non-finite size must be refused the way fontdue refuses
/// it — `(default metrics, empty mask)` — because the DPR path can hand a zero
/// px through on a degenerate surface and a panic there is a lost frame.
#[test]
fn a_degenerate_size_is_empty_like_fontdue() {
    let (ours, theirs) = pair(DEJAVU);
    let gid = ours.lookup_glyph_index('M');
    assert_ne!(gid, 0, "DejaVu covers 'M'");
    for px in [0.0f32, -1.0, -0.0] {
        let (m, cov) = ours.rasterize_indexed(gid, px);
        let (bm, bcov) = theirs.rasterize_indexed(gid, px);
        assert!(cov.is_empty() && bcov.is_empty(), "{px}px produced a mask");
        assert_eq!(
            (m.width, m.height, m.xmin, m.ymin),
            (bm.width, bm.height, bm.xmin, bm.ymin),
            "{px}px metrics differ from fontdue"
        );
    }
    // NaN has no fontdue counterpart worth pinning (it indexes and multiplies
    // straight through); the requirement here is only that it cannot panic or
    // allocate.
    let (m, cov) = ours.rasterize_indexed(gid, f32::NAN);
    assert!(cov.is_empty() && m.width == 0 && m.height == 0);
}
