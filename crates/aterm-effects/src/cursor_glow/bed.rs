// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DARK BED: on a dark ground the beam family's streak and the classic
//! wake's mono tracer lie UNDER the letters they cross, as the rainbow
//! kitty's ribbon does ([`crate::rainbow_kitty::ribbon::BedInkLut`]).
//!
//! Additive light over a letter lifts its ink and its ground together until
//! the ink clips and only the ground keeps climbing, so a streak that reads
//! as light over the text can keep the letters legible only by dimming until
//! it is gone. A bed beneath the glyph ink can be as vivid as the letters
//! allow and no more: each stop of the style's ramp is put on the most
//! luminance at which the theme's ink still reads [`DARK_BED_BAR`] over it
//! ([`dark_bed_ink`]), at the most chroma that luminance allows, so the bed
//! keeps the style's hue and its time taper (a fading cell's bed thins toward
//! the page) while the letters on it stay at the bar.
//!
//! A typed run's bed rises from nothing over its oldest cell, and a comet's
//! (and sparkle's) is a wedge, whole at the caret and gone a few cells
//! behind it ([`BedShape::wedge`]). The colour at the bar's luminance is a
//! dark one, so the phaser keeps its bed on the night arc (blue, violet,
//! magenta: [`night_hue`]), where a warm or green stop at the bar's
//! luminance is an olive or a brown, and sparkle lays no body at all
//! ([`BedShape::dim`] of nothing: any colour dark enough is a lavender dusk
//! or a muddy brown under a gold comet): its gold rides its rail, soft at
//! both edges ([`BedShape::rail_rise`]), its bridge and its crown. The comet, the phaser, sparkle and
//! the classic wake's mono face lay their true hue at full value on a thin
//! vivid rail in the leading under the row, where no letter of the row
//! stands and the row below shows none ([`BedShape::rail`], the rainbow
//! kitty's rail).
//!
//! Over cells with no text (the probe's rows, and the frame's own cells for
//! the rows it missed: [`TextView`]), and over the bridge into the caret's
//! own cell (kept inside that cell, [`clip_quads_to`]), the style keeps its
//! additive beam at full strength ([`CursorGlow::emit_comet`]); a leap's bed
//! lies only under the text it crosses. Every other light that lands on a
//! letter spends the frame's over-ink account cell by cell
//! ([`InkLedger::admit_masked`]), whose ground for a bedded cell is the
//! bed's composite ([`InkLedger::lay_under`]).

use super::*;
use aterm_render::{GlowBlend, RibbonVertex, ribbon_beam};

/// The contrast the theme's ink keeps over the bed alone: WCAG's 4.5:1 for
/// text, plus the measure's own margin (an antialiased stroke's core reads a
/// few percent darker than its ink). Every additive light over a bedded
/// letter spends from here down to its tier's floor
/// ([`InkLedger::HEAD_FLOOR`]).
pub(super) const DARK_BED_BAR: f32 = 4.8;

/// The bed's opacity over a fresh cell. Its ink is solved at full opacity
/// ([`dark_bed_ink`]), so this and every fading level under it composite
/// darker still, and two beds stacked on one pixel never pass the solve.
pub(super) const DARK_BED_ALPHA_CAP: f32 = 236.0;

/// The bed's luminance budget's clamp, the rainbow bed's: a theme whose ink
/// is near black would solve a bed nobody sees, and the brightest budget is
/// the bar's own answer at a white ink.
const DARK_BED_LUMA_MIN: f32 = 0.020;
/// See [`DARK_BED_LUMA_MIN`].
const DARK_BED_LUMA_MAX: f32 = 0.150;

/// The bed's luminance budget for the theme's ink `ink`: the most relative
/// luminance a bed may composite at and leave `ink` [`DARK_BED_BAR`] over
/// it, clamped as [`DARK_BED_LUMA_MIN`] says.
#[must_use]
pub(crate) fn dark_bed_budget(ink: u32) -> f32 {
    let bar =
        (crate::color_math::relative_luminance(ink & 0x00FF_FFFF) + 0.05) / DARK_BED_BAR - 0.05;
    bar.clamp(DARK_BED_LUMA_MIN, DARK_BED_LUMA_MAX)
}

/// The bed's ink for the stop `rgb` over the page `bg` under `budget`: the
/// stop lifted to at least `sat` of HSV chroma (a pale stop at the bar's
/// luminance would read grey), then put on the most luminance whose
/// channel-wise brighter of itself and the page stays under `budget`, by one
/// monotone family: black, the stop scaled through itself to full value,
/// then toward white for a stop too dark at full value to reach the budget
/// (blue). The channel-wise maximum bounds any source-over composite of the
/// ink over the page at any opacity, a stack of them included.
#[must_use]
pub(crate) fn dark_bed_ink(rgb: u32, bg: u32, budget: f32, sat: f32) -> u32 {
    let table = crate::color_math::relative_luminance_table();
    let rgb = crate::spectrum::spectrum_with_min_saturation(rgb & 0x00FF_FFFF, sat);
    let ch = |c: u32, sh: u32| (c >> sh) & 0xff;
    let mx = ch(rgb, 16).max(ch(rgb, 8)).max(ch(rgb, 0));
    let full = if mx == 0 {
        0
    } else {
        let k = 255.0 / mx as f32;
        let up = |sh: u32| ((ch(rgb, sh) as f32 * k).round() as u32).min(255);
        (up(16) << 16) | (up(8) << 8) | up(0)
    };
    let family = |t: f32| {
        let pick = |sh: u32| {
            let c = ch(full, sh) as f32;
            let v = if t <= 0.5 {
                c * 2.0 * t
            } else {
                c + (255.0 - c) * (2.0 * t - 1.0)
            };
            (v as u32).min(255)
        };
        (pick(16) << 16) | (pick(8) << 8) | pick(0)
    };
    let seen = |ink: u32| {
        let top = |sh: u32| ch(ink, sh).max(ch(bg, sh));
        crate::color_math::relative_luminance_with(table, (top(16) << 16) | (top(8) << 8) | top(0))
    };
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..20 {
        let mid = 0.5 * (lo + hi);
        if seen(family(mid)) <= budget {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    family(lo)
}

/// A style's bed: its half-height and how it carries the style's fade.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BedShape {
    /// The bed's half-height at a fresh cell, as a share of the cell height.
    pub(crate) reach: f32,
    /// The share of that reach a fading cell gives up as it fades, so a
    /// comet's tail thins to a hairline while a beam's tube keeps its girth.
    pub(crate) thin: f32,
    /// The least HSV chroma the bed's ink carries ([`dark_bed_ink`]).
    pub(crate) sat: f32,
    /// The coverage of a fresh cell of the style, which lays the bed at
    /// [`DARK_BED_ALPHA_CAP`]; a fading cell's bed follows its coverage down.
    pub(crate) full: f32,
    /// A wedge's length in cells (`0` for none): the bed is whole at the
    /// run's head and thins and fades to nothing this far behind it, by
    /// distance, so a run is a comet's tail however its cells are aged.
    pub(crate) wedge: f32,
    /// The vivid rail's reach below the row, as a share of the cell height
    /// (`0` for none): the style's own colour at full value
    /// ([`crate::rainbow_kitty::ribbon::rail_ink`]) in the leading under
    /// the run, where no letter of the row stands, fading with the bed, and
    /// only over cells of the row below that hold no line of text (whose
    /// letters' tops it would light).
    pub(crate) rail: f32,
    /// How far the rail's soft top rises above the row's bottom, as a share
    /// of its reach: `0` stands it on the row's edge (a crisp rule), more
    /// melts its top into the last pixels of the row, under its descenders.
    pub(crate) rail_rise: f32,
    /// The hue family the bed's ink takes ([`BedHue`]).
    pub(crate) hue: BedHue,
    /// The share of the reach the bed's full-opacity core holds: the rest
    /// melts to nothing at the reach, so a small share is a feathered band
    /// with no edge to read as a selection's.
    pub(crate) core: f32,
    /// The share of the bar's luminance budget the bed is solved to
    /// ([`dark_bed_ink`]; `1` at the bar): a quieter ground under a style
    /// whose colour lives in its light, not under its letters.
    pub(crate) dim: f32,
}

/// The hue family of a style's bed ([`BedShape::hue`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum BedHue {
    /// The stop's own hue.
    True,
    /// The night arc ([`night_hue`]): a style that rolls through the spectrum
    /// keeps blue, violet and magenta under the letters, where a warm or
    /// green stop at the bar's luminance is an olive or a brown, and leaves
    /// its true hue to the rail and the light.
    Night,
}

impl BedShape {
    /// The shape every style's bed starts from: no wedge, no rail, the
    /// stop's own hue, a half-reach core, at the bar.
    pub(crate) const BASE: Self = Self {
        reach: 0.30,
        thin: 0.30,
        sat: 0.90,
        full: 215.0,
        wedge: 0.0,
        rail: 0.0,
        rail_rise: 0.0,
        hue: BedHue::True,
        core: 0.5,
        dim: 1.0,
    };

    /// The shape of `style`'s bed, `None` for a style that lays none (fire's
    /// streak is a vein over its flames; water's wake is its own under-ink
    /// body; the rainbow kitty's ribbon is its own bed). `beam_pw` is the
    /// beam's power envelope, which thins its tube.
    pub(super) fn of(style: GlowStyle, beam_pw: f32) -> Option<Self> {
        let shape = |reach, thin, sat, full| {
            Some(Self {
                reach,
                thin,
                sat,
                full,
                ..Self::BASE
            })
        };
        match style {
            // A quiet tracer in the cursor's colour, through the x-height.
            GlowStyle::Lumen => shape(0.26, 0.45, 0.90, 215.0),
            // The icy tail: whole under the caret, a wedge thinning and
            // fading to nothing three and a half cells behind it, over an
            // icy rail that tapers with it.
            GlowStyle::Comet => shape(0.40, 0.75, 0.85, 215.0).map(|s| Self {
                wedge: 3.5,
                rail: 0.16,
                ..s
            }),
            // The charged conductor: a narrow violet cable the bolts fork off.
            GlowStyle::Laser => shape(0.22, 0.30, 0.85, 255.0),
            // The fat bar of spectrum, each cell in the hue it was laid in:
            // the bar in the night arc, the spectrum itself on the rail.
            GlowStyle::Phaser => shape(0.40, 0.15, 1.0, 126.0).map(|s| Self {
                rail: 0.18,
                hue: BedHue::Night,
                ..s
            }),
            // The gold comet the grains fall out of: nothing under the
            // letters (any colour dark enough to keep them at the bar is a
            // dusk of lavender or a muddy brown, never gold); its gold tail
            // is its rail, soft at both edges and fading by alpha as a
            // comet's tail four cells behind the head, with its bridge onto
            // the caret and its crown.
            GlowStyle::Sparkle => shape(0.40, 0.30, 0.0, 215.0).map(|s| Self {
                wedge: 4.0,
                rail: SPARKLE_RAIL,
                rail_rise: SPARKLE_RAIL_RISE,
                core: 0.12,
                dim: 0.0,
                ..s
            }),
            // The tube: even girth, collapsing as it powers down.
            GlowStyle::Beam => shape(0.14 + 0.26 * beam_pw, 0.0, 0.85, 235.0),
            _ => None,
        }
    }

    /// The bed's opacity and half-height (px) over a sample of coverage
    /// `cov` in a cell `ch` tall, its spine [`DARK_BED_DY`] below the
    /// middle, times `k` (the wedge's and the tail's taper): the reach never
    /// crosses the row's edges.
    fn at(self, cov: u8, ch: f32, k: f32) -> (f32, f32) {
        let share = (f32::from(cov) / self.full).clamp(0.0, 1.0) * k.clamp(0.0, 1.0);
        let reach = ch * self.reach * (1.0 - self.thin * (1.0 - share));
        let room = (ch * (0.5 - DARK_BED_DY)).max(0.0);
        (
            DARK_BED_ALPHA_CAP * share,
            reach.clamp(0.5f32.min(room), room),
        )
    }
}

/// The bed's spine below the cell's middle, as a share of the cell height:
/// the middle of the x-height band, where the letters' bodies sit.
const DARK_BED_DY: f32 = 0.04;

/// Sparkle's gold rail's reach below the row, as a share of the cell
/// height ([`BedShape::rail`]): its whole tail.
const SPARKLE_RAIL: f32 = 0.24;
/// Sparkle's rail's soft top ([`BedShape::rail_rise`]).
const SPARKLE_RAIL_RISE: f32 = 0.5;

/// How far a typed run's bed takes to rise from nothing at its tail, in
/// cells: the oldest cell's bed feathers in rather than starting square on
/// a cell edge.
const DARK_BED_TAIL_CELLS: f32 = 1.0;

/// The rail's full-strength plateau as a share of its reach: the top half
/// holds and the bottom half melts, as the rainbow kitty's rail does.
const DARK_RAIL_CORE: f32 = 0.5;

/// The nested passes a leap's tube is laid in, widest first, so its cross
/// section falls off from the spine to nothing instead of standing as a
/// flat slab of one opacity.
const LEAP_LAYERS: usize = 6;
/// The longest stretch of air, in a leap's cells, its bed tube carries on
/// across between two lines of text ([`lay_dark_bed`]).
const LEAP_GAP_CELLS: usize = 3;
/// How much wider than the bed's own girth a leap's outermost pass reaches,
/// so its soft edge carries past where the flat tube ended.
const LEAP_SPREAD: f32 = 1.4;

/// The night arc's hue, degrees, for a hue `h`: the wheel folded onto the
/// arc from azure (195°) through blue and violet to a magenta-pink (330°),
/// the hues whose colour holds at the bar's luminance. Continuous around
/// the wheel (it folds once, at the yellow-green the bar would turn to
/// olive), so a rolling hue rolls smoothly: green → azure, cyan → blue,
/// blue → violet, magenta → violet-magenta, red → magenta, orange →
/// magenta-pink, yellow back toward violet.
#[must_use]
pub(crate) fn night_hue(h: f32) -> f32 {
    let u = (h - 90.0).rem_euclid(360.0);
    if u <= 180.0 {
        195.0 + 75.0 * u / 180.0
    } else if u <= 300.0 {
        270.0 + 60.0 * (u - 180.0) / 120.0
    } else {
        330.0 - 135.0 * (u - 300.0) / 60.0
    }
}

/// `rgb` on the night arc ([`night_hue`]), its saturation and value kept.
#[must_use]
pub(crate) fn night_rgb(rgb: u32) -> u32 {
    let (h, s, v) = crate::color_math::rgb2hsv(rgb & 0x00FF_FFFF);
    crate::color_math::hsv2rgb(night_hue(h), s, v)
}

/// `budget` (a relative luminance, [`dark_bed_budget`]) dimmed to `dim` of
/// its rise over the page `page`, so a dimmed bed is still solved against
/// what lies under it.
#[must_use]
pub(crate) fn dim_budget(budget: f32, page: u32, dim: f32) -> f32 {
    let floor = crate::color_math::relative_luminance(page & 0x00FF_FFFF);
    floor + (budget - floor).max(0.0) * dim.clamp(0.0, 1.0)
}

/// The dark bed's working buffers, held by each engine that lays one (the
/// aurora and the classic wake), so a bed frame allocates nothing after the
/// first growth: each swept sample's ink and rail ink, the ribbon's
/// vertices, the leap's tube.
#[derive(Default)]
pub(crate) struct BedScratch {
    /// The bed's ink per sample, in run order ([`lay_dark_bed`]'s `inks`),
    /// filled by the engine, which owns the colour rule.
    pub(crate) inks: Vec<u32>,
    /// The rail's ink per sample, in run order (empty for a shape with no
    /// rail).
    pub(crate) rails: Vec<u32>,
    verts: Vec<RibbonVertex>,
    tube: Vec<BeamVertex>,
    /// A leap tube's per-vertex opacity, for its nested soft passes.
    tube_cov: Vec<u8>,
    /// Each sample's taper (the wedge's and the tail's), in run order.
    taper: Vec<f32>,
}

/// Lay the dark bed under `runs` into `under` (the glyph-under stream),
/// source-over over the page `page` in each sample's ink (`scratch.inks`,
/// one per sample in run order, already put on the bar by
/// [`dark_bed_ink`]), and tell the over-ink account `ledger` what now lies
/// under each cell's letters. A one-row run (a typed run) is a soft-edged
/// ribbon from its first cell's outer edge to its last's, rising from
/// nothing over its oldest cell ([`DARK_BED_TAIL_CELLS`]) and, for a wedge,
/// fading to nothing behind its head ([`BedShape::wedge`]), with the rail
/// under it ([`BedShape::rail`], `scratch.rails`); a run across rows (a
/// jump) is a soft tube along the straight line the swept cells step down
/// ([`LEAP_LAYERS`]).
#[allow(
    clippy::too_many_arguments,
    reason = "the runs, the grid, the pair's page, the shape, the frame's text and the three buffers it fills"
)]
pub(crate) fn lay_dark_bed(
    runs: &[Vec<CometSample>],
    geom: Geom,
    page: u32,
    shape: BedShape,
    text: &TextView<'_>,
    scratch: &mut BedScratch,
    ledger: &mut InkLedger,
    under: &mut Vec<GlowQuad>,
) {
    let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
    let (oxf, oyf) = (f32::from(geom.origin_x), f32::from(geom.origin_y));
    let clip = geom.beam_clip();
    let stride = (geom.cw / 8).max(1);
    let quad_budget = under.len().saturating_add(CursorGlow::MAX_QUADS);
    let dy = chf * DARK_BED_DY;
    let BedScratch {
        inks,
        rails,
        verts,
        tube,
        tube_cov,
        taper,
    } = scratch;
    let cell_of = |x: f32, y: f32| {
        (
            ((y - oyf) / chf).floor() as i32,
            ((x - oxf) / cwf).floor() as i32,
        )
    };
    let smooth = |t: f32| {
        let t = t.clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let mut at = 0usize;
    for run in runs {
        if run.is_empty() {
            continue;
        }
        let ink = &inks[at..at + run.len()];
        let rail = rails.get(at..at + run.len());
        at += run.len();
        let one_row = run.iter().all(|s| s.y == run[0].y);
        // From the first cell's outer edge to the last cell's, so the bed
        // covers every letter of the run whole.
        let dir = if run.len() > 1 && run[run.len() - 1].x < run[0].x {
            -1.0
        } else {
            1.0
        };
        let (first, last) = (run[0], run[run.len() - 1]);
        let (tail_x, head_x) = (first.x - dir * cwf * 0.5, last.x + dir * cwf * 0.5);
        // The taper at `x`: rising off the tail's edge, and for a wedge
        // falling off the head's cell behind it.
        let taper_at = |x: f32| {
            if !one_row {
                return 1.0;
            }
            let tail = smooth((x - tail_x).abs() / (cwf * DARK_BED_TAIL_CELLS));
            let wedge = if shape.wedge > 0.0 {
                let behind = ((last.x - x) * dir).max(0.0);
                (1.0 - behind / (cwf * shape.wedge)).clamp(0.0, 1.0)
            } else {
                1.0
            };
            tail * wedge
        };
        taper.clear();
        taper.extend(run.iter().map(|s| taper_at(s.x)));
        // What each sample composites to over the page, charged to its own
        // cell and its neighbour's (the bed lerps between them).
        let mut prev: Option<((i32, i32), u32)> = None;
        for ((s, &rgb), &k) in run.iter().zip(ink).zip(taper.iter()) {
            let a = shape.at(s.cov, chf, k).0 as u8;
            let composite = aterm_render::over_premul(page, premul_rgb(rgb, a), a);
            let here = cell_of(s.x, s.y);
            ledger.lay_under(here, composite);
            if let Some((cell, before)) = prev {
                ledger.lay_under(here, before);
                ledger.lay_under(cell, composite);
            }
            prev = Some((here, composite));
        }
        if one_row {
            let vertex = |x: f32, s: &CometSample, rgb: u32, k: f32| {
                let (alpha, reach) = shape.at(s.cov, chf, k);
                RibbonVertex {
                    x,
                    spine: s.y + dy,
                    up: reach,
                    dn: reach,
                    core_up: reach * shape.core,
                    core_dn: reach * shape.core,
                    color: rgb,
                    cov: alpha,
                    lift: 0.0,
                    lift_span: 0.0,
                }
            };
            verts.clear();
            verts.push(vertex(tail_x, &first, ink[0], 0.0));
            for ((s, &rgb), &k) in run.iter().zip(ink).zip(taper.iter()) {
                verts.push(vertex(s.x, s, rgb, k));
            }
            // Over a head cell of air (the caret's, or the one it left on an
            // Enter) the bed fades out across the cell from the last letter's
            // edge instead of standing to the cell's far edge: no letter needs
            // it there, and once the caret has gone a square-ended block of
            // bed stood in the empty cell.
            let (head_row, head_col) = cell_of(last.x, last.y);
            let air_head = run.len() > 1 && !text.over_text(head_row, head_col);
            if air_head && let Some(v) = verts.last_mut() {
                let (alpha, _) = shape.at(last.cov, chf, taper[taper.len() - 1] * 0.5);
                v.cov = alpha;
            }
            let head_k = if air_head { 0.0 } else { taper_at(head_x) };
            verts.push(vertex(head_x, &last, ink[ink.len() - 1], head_k));
            // A shape with no share of the bar lays no body (sparkle's).
            if shape.dim > 0.0 {
                ribbon_beam(
                    under,
                    clip,
                    verts,
                    1.0,
                    stride,
                    quad_budget,
                    GlowBlend::Over,
                );
            }
            // The vivid rail in the leading under the row, where no letter
            // of the row stands: the stop at full value, fading with the
            // bed. Not under the grid's last row.
            let row = cell_of(first.x, first.y).0;
            if let Some(rail) = rail
                && shape.rail > 0.0
                && row + 1 < geom.rows as i32
            {
                let bottom = oyf + (row + 1) as f32 * chf;
                let reach = shape.rail * chf;
                // Only where the row below shows no letter: a line of text
                // there (a selection, earlier output) would have its
                // letters' tops in the rail, so the rail goes out over it.
                let leading = |x: f32| !text.over_text(row + 1, cell_of(x, first.y).1);
                let rail_vertex = |x: f32, s: &CometSample, rgb: u32, k: f32| RibbonVertex {
                    x,
                    spine: bottom,
                    up: reach * shape.rail_rise,
                    dn: reach,
                    core_up: 0.0,
                    core_dn: reach * DARK_RAIL_CORE * (1.0 - shape.rail_rise),
                    color: rgb,
                    cov: if leading(x) {
                        shape.at(s.cov, chf, k).0
                    } else {
                        0.0
                    },
                    lift: 0.0,
                    lift_span: 0.0,
                };
                verts.clear();
                verts.push(rail_vertex(tail_x, &first, rail[0], 0.0));
                for ((s, &rgb), &k) in run.iter().zip(rail).zip(taper.iter()) {
                    verts.push(rail_vertex(s.x, s, rgb, k));
                }
                verts.push(rail_vertex(
                    head_x,
                    &last,
                    rail[rail.len() - 1],
                    taper_at(head_x),
                ));
                ribbon_beam(
                    under,
                    clip,
                    verts,
                    1.0,
                    stride,
                    quad_budget,
                    GlowBlend::Over,
                );
            }
        } else {
            // A leap: a tube through each stretch of the swept cells that
            // crosses text (over air the beam is the leap's light, and a bed
            // there would only outline it), at the stretch's mean girth
            // (`comet_beam_over` takes one thickness), each vertex at its
            // own opacity and ink, reaching half a step past its end cells.
            let over = |s: &CometSample| {
                let (row, col) = cell_of(s.x, s.y);
                text.over_text(row, col)
            };
            let mut i = 0;
            while i < run.len() {
                if !over(&run[i]) {
                    i += 1;
                    continue;
                }
                let start = i;
                // One tube across a short stretch of air between two lines
                // of text (the cells past a line's end that a leap's diagonal
                // crosses on its way to the next line): two tubes with a gap
                // between them read as two offset slabs, not one leap.
                loop {
                    while i < run.len() && over(&run[i]) {
                        i += 1;
                    }
                    let mut j = i;
                    while j < run.len() && !over(&run[j]) && j - i < LEAP_GAP_CELLS {
                        j += 1;
                    }
                    if j > i && j < run.len() && over(&run[j]) {
                        i = j;
                        continue;
                    }
                    break;
                }
                let stretch = &run[start..i];
                let reach = stretch
                    .iter()
                    .map(|s| shape.at(s.cov, chf, 1.0).1)
                    .sum::<f32>()
                    / stretch.len() as f32;
                let vertex = |s: &CometSample, rgb: u32, x: f32, y: f32| BeamVertex {
                    x,
                    y: y + dy,
                    color: rgb,
                    cov: shape.at(s.cov, chf, 1.0).0 as u8,
                };
                tube.clear();
                if start > 0 {
                    let (a, b) = (run[start - 1], run[start]);
                    tube.push(vertex(&b, ink[start], 0.5 * (a.x + b.x), 0.5 * (a.y + b.y)));
                }
                tube.extend(
                    stretch
                        .iter()
                        .zip(&ink[start..i])
                        .map(|(s, &rgb)| vertex(s, rgb, s.x, s.y)),
                );
                if i < run.len() {
                    let (a, b) = (run[i - 1], run[i]);
                    tube.push(vertex(&a, ink[i - 1], 0.5 * (a.x + b.x), 0.5 * (a.y + b.y)));
                }
                if tube.len() >= 2 && shape.dim > 0.0 {
                    // Straight along the true diagonal (a leap's cells step
                    // down a straight line, so the line through its two ends
                    // is the path; the cells' stair drew a stair of slabs)
                    // and soft across it: nested passes, widest first, each
                    // laying the share of a vertex's opacity that stacks to
                    // the whole on the spine (the ink is solved opaque, so
                    // any stack of it keeps the bar).
                    // Its two ends fade in and out over half a cell, so a
                    // stretch over a line of text does not stop square at
                    // the line's first and last letters.
                    let (a, b) = (tube[0], tube[tube.len() - 1]);
                    let len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
                    let f = (cwf * 0.5 / len.max(1.0)).min(0.3);
                    let at = |k: f32, cov: u8| BeamVertex {
                        x: a.x + (b.x - a.x) * k,
                        y: a.y + (b.y - a.y) * k,
                        color: if k < 0.5 { a.color } else { b.color },
                        cov,
                    };
                    let (ca, cb) = (a.cov, b.cov);
                    tube.clear();
                    tube.extend([at(0.0, 0), at(f, ca), at(1.0 - f, cb), at(1.0, 0)]);
                    tube_cov.clear();
                    tube_cov.extend(tube.iter().map(|v| v.cov));
                    for k in 0..LEAP_LAYERS {
                        let shrink = 1.0 - k as f32 / LEAP_LAYERS as f32;
                        for (v, &a) in tube.iter_mut().zip(tube_cov.iter()) {
                            let a = f32::from(a) / 255.0;
                            let layer = 1.0 - (1.0 - a).powf(1.0 / LEAP_LAYERS as f32);
                            v.cov = (layer * 255.0).round().clamp(0.0, 255.0) as u8;
                        }
                        comet_beam_over(
                            under,
                            clip,
                            tube,
                            2.0 * reach * LEAP_SPREAD * shrink,
                            1,
                            0.0,
                        );
                    }
                }
            }
        }
    }
    under.truncate(quad_budget);
}

/// A wedge's runs ([`BedShape::wedge`]) into the pool `out`, returning how
/// many are live: each one-row run is laid by distance from its head, in
/// the direction it came from, `wedge` cells long in quarter-cell knots at
/// the run's freshest coverage and with the path position falling from the
/// head (`1`) to the tail's tip (`0`), so the tail is a comet's whatever
/// span of cells is still live at a typing cadence; a run across rows (a
/// leap) is its own samples.
fn wedge_runs(
    runs: &[Vec<CometSample>],
    geom: Geom,
    wedge: f32,
    out: &mut Vec<Vec<CometSample>>,
) -> usize {
    let cwf = geom.cw as f32;
    let knots = (wedge * 4.0).ceil().max(1.0) as usize;
    let mut n = 0usize;
    for run in runs {
        let Some(&head) = run.last() else {
            continue;
        };
        if n == out.len() {
            out.push(Vec::new());
        }
        let dst = &mut out[n];
        dst.clear();
        n += 1;
        if !run.iter().all(|s| s.y == head.y) {
            dst.extend_from_slice(run);
            continue;
        }
        let back = match run.first() {
            Some(tail) if run.len() > 1 && tail.x > head.x => 1.0,
            _ => -1.0,
        };
        let cov = run.iter().map(|s| s.cov).max().unwrap_or(head.cov);
        let span = wedge * cwf;
        dst.extend((0..=knots).rev().map(|k| {
            let d = span * k as f32 / knots as f32;
            CometSample {
                x: head.x + back * d,
                y: head.y,
                cov,
                pos: 1.0 - d / span,
            }
        }));
    }
    n
}

/// Cut the quads `out[from..]` to the pixel box `(x0, y0, x1, y1)` (a
/// gradient quad keeps its own colour at the cut), dropping what lies
/// wholly outside: the dark bridge onto the caret's cell, whose light is
/// the caret's own and must not stand above and below it as a stub of the
/// beam's halo.
pub(crate) fn clip_quads_to(
    out: &mut Vec<GlowQuad>,
    from: usize,
    (x0, y0, x1, y1): (i32, i32, i32, i32),
) {
    let mut kept = from;
    for i in from..out.len() {
        let q = out[i];
        let (qx0, qy0) = (i32::from(q.x), i32::from(q.y));
        let (qx1, qy1) = (qx0 + i32::from(q.w), qy0 + i32::from(q.h));
        let (cx0, cx1) = (qx0.max(x0), qx1.min(x1));
        let (cy0, cy1) = (qy0.max(y0), qy1.min(y1));
        if cx0 >= cx1 || cy0 >= cy1 {
            continue;
        }
        let w = f32::from(q.w.max(1));
        let lerp = |a: u32, b: u32, x: i32| {
            let t = (x - qx0) as f32 / w;
            let ch = |sh: u32| {
                let (u, v) = (((a >> sh) & 0xff) as f32, ((b >> sh) & 0xff) as f32);
                ((u + (v - u) * t).round() as u32).min(255)
            };
            (ch(16) << 16) | (ch(8) << 8) | ch(0)
        };
        let lerp_a = |a: u8, b: u8, x: i32| {
            let t = (x - qx0) as f32 / w;
            (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8
        };
        let mut c = q;
        c.x = cx0 as u16;
        c.y = cy0 as u16;
        c.w = (cx1 - cx0) as u16;
        c.h = (cy1 - cy0) as u16;
        if cx0 != qx0 || cx1 != qx1 {
            c.color = lerp(q.color, q.color2, cx0);
            c.color2 = lerp(q.color, q.color2, cx1);
            if q.alpha != 0 {
                c.alpha = lerp_a(q.alpha, q.alpha2, cx0).max(1);
                c.alpha2 = lerp_a(q.alpha, q.alpha2, cx1).max(1);
            }
        }
        out[kept] = c;
        kept += 1;
    }
    out.truncate(kept);
}

/// Split the swept `runs` into the runs the dark beam draws, into the pool
/// `air` (returning how many are live): the stretches of each run over air,
/// each ending where it meets text at a KNEE of zero coverage on the text
/// cell's edge, so the beam rises from nothing at the edge of a letter's
/// cell to its full strength at the first cell of air and never lights the
/// letter. `text` is what the frame shows of text ([`TextView::over_text`]:
/// a row nobody read counts as text, so the full beam lands only where the
/// frame showed air); `caret` says the last run ends on the caret's own
/// cell, which is air whatever the probe says (its letter, if any, is under
/// the caret).
pub(crate) fn split_air_runs(
    runs: &[Vec<CometSample>],
    geom: Geom,
    text: &TextView<'_>,
    caret: bool,
    air: &mut Vec<Vec<CometSample>>,
) -> usize {
    let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
    let (oxf, oyf) = (f32::from(geom.origin_x), f32::from(geom.origin_y));
    let over_text = |s: &CometSample| {
        let (row, col) = (
            ((s.y - oyf) / chf).floor() as i32,
            ((s.x - oxf) / cwf).floor() as i32,
        );
        text.over_text(row, col)
    };
    // Where the segment from the text sample `t` toward the air sample `a`
    // leaves `t`'s cell, at zero coverage.
    let knee = |t: &CometSample, a: &CometSample| {
        let (dx, dy) = (a.x - t.x, a.y - t.y);
        let exit = |d: f32, half: f32| {
            if d.abs() > 1e-3 {
                half / d.abs()
            } else {
                f32::INFINITY
            }
        };
        let k = exit(dx, cwf * 0.5).min(exit(dy, chf * 0.5)).min(1.0);
        CometSample {
            x: t.x + dx * k,
            y: t.y + dy * k,
            cov: 0,
            pos: a.pos,
        }
    };
    for r in air.iter_mut() {
        r.clear();
    }
    let mut n = 0usize;
    for (ri, run) in runs.iter().enumerate() {
        let head = caret && ri + 1 == runs.len();
        let is_air = |i: usize| (head && i + 1 == run.len()) || !over_text(&run[i]);
        let mut i = 0;
        while i < run.len() {
            if !is_air(i) {
                i += 1;
                continue;
            }
            let start = i;
            while i < run.len() && is_air(i) {
                i += 1;
            }
            if n == air.len() {
                air.push(Vec::new());
            }
            let out = &mut air[n];
            if start > 0 {
                out.push(knee(&run[start - 1], &run[start]));
            }
            out.extend_from_slice(&run[start..i]);
            if i < run.len() {
                out.push(knee(&run[i], &run[i - 1]));
            }
            n += 1;
        }
    }
    n
}

impl CursorGlow {
    /// Lay the dark bed under the runs `self.comet_runs[..runs_len]`
    /// ([`lay_dark_bed`]), each sample in the ink of `color(self, pos)` put on
    /// the bar for the frame's pair ([`dark_bed_ink`]).
    pub(super) fn emit_dark_bed(
        &mut self,
        cfg: &GlowConfig,
        geom: Geom,
        runs_len: usize,
        shape: BedShape,
        color: &dyn Fn(&CursorGlow, f32) -> u32,
        under: &mut Vec<GlowQuad>,
    ) {
        let (ink, page) = Self::bed_pair(cfg);
        let budget = dim_budget(dark_bed_budget(ink), page, shape.dim);
        let mut scratch = std::mem::take(&mut self.bed_scratch);
        scratch.inks.clear();
        scratch.rails.clear();
        let mut wedges = std::mem::take(&mut self.wedge_runs);
        let runs: &[Vec<CometSample>] = if shape.wedge > 0.0 {
            let n = wedge_runs(&self.comet_runs[..runs_len], geom, shape.wedge, &mut wedges);
            &wedges[..n]
        } else {
            &self.comet_runs[..runs_len]
        };
        for run in runs {
            for s in run {
                let raw = color(self, s.pos);
                let hue = match shape.hue {
                    BedHue::Night => night_rgb(raw),
                    BedHue::True => raw,
                };
                scratch
                    .inks
                    .push(self.bed_ink(hue, page, budget, shape.sat));
                if shape.rail > 0.0 {
                    scratch
                        .rails
                        .push(crate::rainbow_kitty::ribbon::rail_ink(raw & 0x00FF_FFFF));
                }
            }
        }
        let text = TextView::new(self.probed_text_spans(), Some(&self.frame_text));
        lay_dark_bed(
            runs,
            geom,
            page,
            shape,
            &text,
            &mut scratch,
            &mut self.ink_ledger,
            under,
        );
        self.bed_scratch = scratch;
        self.wedge_runs = wedges;
    }

    /// [`dark_bed_ink`], or (a test's negative control,
    /// [`Self::bed_unsolved`]) the stop at its full value.
    fn bed_ink(&self, rgb: u32, page: u32, budget: f32, sat: f32) -> u32 {
        if self.bed_unsolved() {
            let rgb = crate::spectrum::spectrum_with_min_saturation(rgb & 0x00FF_FFFF, sat);
            let mx = ((rgb >> 16) & 0xff)
                .max((rgb >> 8) & 0xff)
                .max(rgb & 0xff)
                .max(1);
            let up = |sh: u32| ((((rgb >> sh) & 0xff) * 255) / mx).min(255);
            (up(16) << 16) | (up(8) << 8) | up(0)
        } else {
            dark_bed_ink(rgb, page, budget, sat)
        }
    }

    /// [`split_air_runs`] of `self.comet_runs[..runs_len]` into
    /// `self.air_runs`, against what this frame shows of text (the probe's
    /// lines and the frame's own cells, [`Self::text_view`]).
    pub(super) fn split_air_runs(&mut self, geom: Geom, runs_len: usize, caret: bool) -> usize {
        let text = TextView::new(self.probed_text_spans(), Some(&self.frame_text));
        split_air_runs(
            &self.comet_runs[..runs_len],
            geom,
            &text,
            caret,
            &mut self.air_runs,
        )
    }

    /// The pixel box of the caret's cell `(row, col)`, window px.
    pub(crate) fn caret_box(geom: Geom, row: u16, col: u16) -> (i32, i32, i32, i32) {
        let (cw, ch) = (geom.cw as i32, geom.ch as i32);
        let x0 = i32::from(geom.origin_x) + i32::from(col) * cw;
        let y0 = i32::from(geom.origin_y) + i32::from(row) * ch;
        (x0, y0, x0 + cw, y0 + ch)
    }

    /// Whether an air run is only the head's bridge: a knee on the head
    /// letter's edge (zero coverage) and the caret's own sample.
    pub(super) fn is_bridge(run: &[CometSample]) -> bool {
        matches!(run, [knee, _] if knee.cov == 0)
    }

    /// What this frame shows of text: the row probe's lines, and the
    /// frame's own cells for the rows it missed ([`TextView`]).
    pub(crate) fn text_view(&self) -> TextView<'_> {
        TextView::new(self.probed_text_spans(), Some(&self.frame_text))
    }

    /// The caret row's letters as columns `first..end`, for the typing
    /// glow's light ink
    /// ([`crate::cursor_momentum::MomentumGlowConfig::glyph_span`]): the row
    /// probe's line, else the frame's own cells' (`None` for a row either
    /// read blank), and every column left of the caret where neither read
    /// the row.
    pub(crate) fn caret_row_glyphs(&self, (row, col): (u16, u16)) -> Option<(i32, i32)> {
        let row = i32::from(row);
        let view = self.text_view();
        if view.spans.reads_row(row) {
            return view
                .spans
                .lines()
                .find(|line| line.0 == row)
                .map(|(_, first, end)| (first, end));
        }
        match view.frame_line(row) {
            Some(span) => span,
            None => (col > 0).then_some((0, i32::from(col))),
        }
    }
}
