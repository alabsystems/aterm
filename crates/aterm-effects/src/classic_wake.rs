// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `classic` cursor style: a thin four-layer anti-aliased comet along the swept cell path under a three-rectangle box-stack bloom crown, with an expanding square ring on a jump.

use std::time::Duration;

use aterm_time::Instant;

use aterm_render::{
    BeamRows, BeamScratch, BeamVertex, CometSample, GlowQuad, HaloMode, RainHalo, RampSink,
    comet_beam_over, comet_glow_quads, premul_rgb,
};

use crate::cursor_glow::{
    BedScratch, BedShape, CursorGlow, Geom, GlowConfig, InkLedger, InkRole, LIGHT_BODY_ALPHA_CAP,
    LIGHT_INK_ALPHA_CAP, LIGHT_MARK_QUADS, LightGround, TextView, clip_stream_to, hsv2rgb,
    lay_dark_bed, push_halo_rows, push_ink_ellipse, push_soft_veil, reink_quads, split_air_runs,
};
use crate::effect_util::{lerp_rgb, push_fx_rect as push_rect};
use crate::trail_sweep::line_cells_tail;

#[derive(Clone, Copy)]
struct Spark {
    row: u16,
    col: u16,
    born_cov: u8,
    pos: f32,
    life: f32,
    typing: bool,
    born: Instant,
}

#[derive(Clone, Copy)]
struct Ring {
    row: u16,
    col: u16,
    /// Its centre's offset from its cell's centre, px: the caret's seat at the landing
    /// ([`GlowConfig::caret_center`]; a bar's insertion point, `0` on a block).
    dx: f32,
    born: Instant,
    life: f32,
    caret: (u16, u16),
    /// The one-colour ring's verdict for this landing ([`MonoRing`]).
    mono: MonoRing,
}

impl Ring {
    /// Its centre in window pixels: its cell's centre moved by [`Ring::dx`].
    #[inline]
    fn center(&self, geom: Geom) -> (f32, f32) {
        let (x, y) = geom.cell_center(self.row, self.col);
        (x + self.dx, y)
    }
}

/// The one-colour landing ring's whole-or-not verdict, kept for its landing
/// ([`ClassicWake::lay_whole_mono_ring`]): once drawn whole it holds its size
/// and only grows, and once it goes out it is not drawn again. Judged afresh
/// from each frame's text, text that came and went hid the ring and showed
/// it again: a blink (looks9's review).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MonoRing {
    /// Not yet drawn whole: its first size still meets a letter, an edge or
    /// the letter floor (an Enter's ring beside the `$` is admitted whole
    /// only once it has faded a third of its life), and it shows once it
    /// does not.
    Unjudged,
    /// Drawn whole, its half side `0` px so far: it only grows.
    Drawn(i32),
    /// Drawn, then met by a later frame's text at the size it held: not
    /// drawn again this landing.
    Out,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct WakeLightKey {
    rgb: u32,
    fg: u32,
    bg: u32,
}

/// The classic wake's animation state.
pub struct ClassicWake {
    sparks: Vec<Spark>,
    path_scratch: Vec<(i32, i32)>,
    ring: Option<Ring>,
    last: Option<(u16, u16)>,
    last_move: Option<Instant>,
    crown_until: Option<Instant>,
    crown_window_ms: u64,
    hue: f32,
    heat: f32,
    heat_at: Option<Instant>,
    last_type: Option<Instant>,
    comet_runs: Vec<Vec<CometSample>>,
    comet_live: usize,
    light_verts: Vec<BeamVertex>,
    /// [`tracer_on_caret_row`]'s resident vertices.
    light_dense: Vec<BeamVertex>,
    spawned: Option<((u16, u16), (u16, u16))>,
    bed_scratch: BedScratch,
    air_runs: Vec<Vec<CometSample>>,
    /// One run's air pieces before [`one_air_piece`] keeps them.
    air_pieces: Vec<Vec<CometSample>>,
    /// Scratch: each point's share of a tracer through the letters ([`through_letters`]).
    air_shares: TracerShares,
    /// The runs up a page held out of the one-colour bed, by index.
    held_runs: Vec<(usize, Vec<CometSample>)>,
    /// A one-colour run up a page as its straightened beam draws it, before
    /// it is cut finer ([`dense_samples`]), and the straightening's buffers.
    up_line: Vec<CometSample>,
    up_line_scratch: BeamScratch,
    /// Scratch: a dim theme's run up a page, its share under the letters
    /// alone, and the stretch laid under them at the page's luminance.
    up_shade: Vec<f32>,
    up_verts: Vec<BeamVertex>,
    crown_ink_memo: Option<(WakeLightKey, u32)>,
    veil_cap_memo: Option<(WakeLightKey, u8)>,
    /// Test-only: draw the mono face on a dark ground as additive light over the letters.
    #[cfg(test)]
    pub(crate) mono_unbedded: bool,
    /// Test-only: lay the light-ground stroke and crown in their unsolved vivid ink.
    #[cfg(test)]
    pub(crate) light_unsolved: bool,
    /// Test-only: lay the light-ground landing veil unbounded by the caret's cell and row.
    #[cfg(test)]
    pub(crate) veil_unbound: bool,
    /// Test-only: lay a paper Enter's tracer across the rows it crosses, as
    /// it was (looks7 M8's negative control).
    #[cfg(test)]
    pub(crate) tracer_over_letters: bool,
    /// Test-only: the span a paper Enter's tracer fades out over leaving the
    /// caret's row, px, in place of [`TRACER_LEAVE_SHARE`] of the row
    /// (looks7 round 2's control: a cut).
    #[cfg(test)]
    pub(crate) tracer_leave_px: Option<f32>,
    /// Test-only: admit the one-colour landing ring at the flying lights'
    /// floor, as it was (looks8 K2's negative control).
    #[cfg(test)]
    pub(crate) ring_flying: bool,
    /// A negative control's seam: lay the one-colour landing ring at its
    /// growing size, cut where the letter floor cuts it, as it was (looks9
    /// Q4's control, in this crate's tests), and the paper ring's rim so
    /// (looks10 L12's, in this crate's tests and the render replay's).
    #[cfg(any(test, debug_assertions))]
    pub(crate) ring_cut: bool,
    /// A negative control's seam: a typed spark lives its tuned life whatever the duration,
    /// as before access r3 (`CursorGlow::set_typed_life_scaled`).
    #[cfg(any(test, debug_assertions))]
    pub(crate) typed_life_unscaled: bool,
    /// A negative control's seam: the one-colour face's dark bed and rail floor a chosen
    /// colour's saturation to [`Self::MONO_BED`]'s and the rail's, as before composed O10(a)
    /// (`EffectsPipeline::set_classic_pin_own_chroma`).
    #[cfg(any(test, debug_assertions))]
    pub(crate) mono_pin_floored: bool,
    /// A negative control's seam: the one-colour rail's raise reads the page's whole
    /// brightness as grey, its tint along the pin's hue too, as before the after-land lows'
    /// W10b (`EffectsPipeline::set_classic_rail_tint_read`).
    #[cfg(any(test, debug_assertions))]
    pub(crate) mono_rail_tint_blind: bool,
    /// Test-only: judge the one-colour landing ring whole or not afresh
    /// from each frame's text, as looks9 Q4 judged it, not once a landing
    /// ([`MonoRing`], looks9's review's negative control).
    #[cfg(test)]
    pub(crate) ring_unlatched: bool,
    /// Test-only: keep the one-colour landing ring inside the window's grid
    /// alone, a split pane's edge inside it unread, as looks9 Q4 kept it
    /// (looks9's review's negative control).
    #[cfg(test)]
    pub(crate) ring_window_edge: bool,
    /// Test-only: the paper rim held to the caret's row is a square as tall as
    /// the row, as round 1 laid it (looks10 round 2's L12 negative control,
    /// [`ClassicWake::row_box_reach`]).
    #[cfg(test)]
    pub(crate) ring_row_square: bool,
    /// Test-only: the one-colour landing ring's quads as the frame last laid
    /// them, after the letter floor's cut.
    #[cfg(test)]
    pub(crate) ring_seen: std::cell::RefCell<Vec<GlowQuad>>,
    /// Test-only: a paper tracer kept to the caret's row eases in from the
    /// row's top whichever edge it enters by, as it did (looks7 round 3's
    /// review's control: a run from below is cut at the row's bottom).
    #[cfg(test)]
    pub(crate) tracer_top_only: bool,
    /// Test-only: the paper face walks the whole spectrum, through the hues
    /// paper darkens to olive, as it did (looks8 K10's negative control).
    #[cfg(test)]
    pub(crate) paper_muddy: bool,
    /// Test-only: the dark spectrum face's box bloom spans the caret's whole cell behind a
    /// bar too, as it did (`the_box_bloom_is_the_bar_s_behind_a_bar`'s negative control).
    #[cfg(test)]
    pub(crate) crown_cell_wide: bool,
    /// Test-only: the one-colour face's bed under a lit page's letters at its
    /// whole hue, as it was (looks8 round 2's negative control).
    #[cfg(test)]
    pub(crate) mono_lit_whole: bool,
    /// Test-only: a paper tracer from below keeps to the caret's row and a
    /// one-colour tracer across rows on a dark page lays every piece in the
    /// air, as they did (looks9 Q11's negative control,
    /// [`tracer_on_caret_row`], [`one_air_piece`]).
    #[cfg(test)]
    pub(crate) tracer_up_kept: bool,
    /// Test-only: the paper crown and tracer lay a chosen colour in its hue's
    /// vivid body ink, as they did (review9fix round 3's negative control,
    /// [`WakeInk::chosen`]).
    #[cfg(test)]
    pub(crate) paper_pin_saturated: bool,
    /// Test-only: a tracer up a page keeps one stretch in the air, the
    /// paper face its longest and the one-colour face its longest piece, as
    /// looks9 laid them (looks10 L6's negative control: a dash floating
    /// between the old caret and the new, anchored to neither).
    #[cfg(test)]
    pub(crate) tracer_up_stretch: bool,
    /// Test-only: a one-colour tracer up a dark page laid through the swept
    /// cells' centres unstraightened, as looks10 round 1 laid it (its
    /// negative control: stairs, a flat in each row it crosses).
    #[cfg(test)]
    pub(crate) tracer_up_stairs: bool,
    /// Test-only: a dim theme's one-colour run up a page lays nothing under
    /// the letters it rises past, as looks10 round 1 laid it (round 2's L6
    /// control: the guard cuts it there and a slash floats in the blank row).
    #[cfg(test)]
    pub(crate) tracer_up_unshaded: bool,
    /// Test-only: a one-colour run up a dark page keeps its whole light in
    /// the air, as looks10 round 1 laid it (round 2's L6 control).
    #[cfg(test)]
    pub(crate) tracer_up_whole: bool,
    /// Test-only: a dim theme's one-colour run up a page keeps its fifth of
    /// light over the letters above its shade, as looks10 round 2 first laid
    /// it (its control: the guard, cutting every light over a cell the light
    /// dims, takes the shade with it).
    #[cfg(test)]
    pub(crate) tracer_up_lit_letters: bool,
    /// A test's negative control: the one-colour tracer's shade under a dim
    /// page's letters laid at the tracer's own share of its light, weighed
    /// and cut by the letter guard with the lights in its cells, as looks10
    /// round 2 laid it, a quarter of its hue (round 3's L6).
    #[cfg(any(test, debug_assertions))]
    pub(crate) tracer_up_faint_shade: bool,
}

impl Default for ClassicWake {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

/// A probed row's foot: its box, `(x0, x1, y0, y1)` in window px.
pub(crate) type WakeFoot = (i32, i32, i32, i32);

/// The probed rows' feet, the caret's cell carved out, what each foot cell
/// leaves under its ink, and their soles ([`WakeInk::feet`]).
pub(crate) type WakeFeet<'a> = (
    &'a [WakeFoot],
    &'a [crate::cursor_glow::FootInk],
    [Option<WakeFoot>; 2],
);

/// Over-text state for the mono face's dark bed.
pub(crate) struct WakeInk<'a> {
    /// Probed and observed text spans for the frame.
    pub(crate) text: TextView<'a>,
    /// The frame's over-ink account.
    pub(crate) ledger: &'a mut InkLedger,
    /// Whether the trail's colour is one the user chose
    /// ([`crate::cursor_glow::ColourPins::trail`]). On paper the one-colour
    /// face's crown and tracer then wear it as itself, walked toward white
    /// only as far as the theme's text over it asks
    /// ([`LightGround::vivid_body_ink`]): in its own hue, its chroma only
    /// falling. Solved from its hue's vivid ink instead, a trail pinned to
    /// `#b0b0c8` (chroma 0.094) was laid up to `#9595e1` (0.298) on white.
    pub(crate) chosen: bool,
    /// The probed rows' feet, the caret's cell carved out, and their soles:
    /// where the one-colour bed's rail may lie in its own row
    /// ([`crate::cursor_glow::BedShape::row_rail`]).
    pub(crate) feet: WakeFeet<'a>,
    /// The page's lightness without its tint, where the one-colour face lays
    /// its light on a tinted dark page over a night ground
    /// ([`crate::cursor_glow::CursorGlow::classic_pin_grounded`]): its bed
    /// under a lit page's letters mixes toward it, not toward the page
    /// ([`ClassicWake::MONO_LIT_BED_SHARE`]).
    pub(crate) bed_ground: Option<u32>,
}

/// The share of a cell's height over which a paper Enter's tracer fades out
/// as it leaves the caret's row ([`tracer_on_caret_row`]).
const TRACER_LEAVE_SHARE: f32 = 0.6;

/// How near a lettered cell off the caret's row a paper tracer may pass,
/// px past its stroke's half width, and still be drawn whole.
const TRACER_CLEAR_PX: f32 = 3.0;

/// The longest step between a paper Enter's tracer's vertices once kept to
/// the caret's row, px: its fade eases over a few px, not over a cell's run
/// between two samples.
const TRACER_STEP_PX: f32 = 2.0;

/// A paper tracer across rows (`verts`, the run from the old caret to the
/// new one) that would pass a lettered cell off the caret's row `caret_row`
/// keeps to the caret's row. Laid straight across the rows at the
/// baseline's drop, an Enter's tracer struck through the printed line
/// (`brown fox` in the gallery's `enter-bottom`, looks7 M8); passed behind
/// each lettered row instead, it broke into a stub at the caret and a dash
/// floating past the printed line's end (looks7 round 2). Each vertex, the
/// line cut to [`TRACER_STEP_PX`] steps, keeps its light on the caret's row
/// only, its stroke's `half` width inside the row, easing in from the edge
/// the run enters the row by (its top for a run from above, as an Enter's;
/// its bottom for one from below, as a move up a page) over `leave` px, or
/// over the room between that edge and the caret's stroke where that is
/// less: one tail rising from the new caret toward the line it left, which
/// fades out before it leaves the row. Eased in from the top alone, a run
/// from below kept a full stub at the caret cut at the row's bottom (looks7
/// round 3's review). A run whose rows are clear of letters is drawn whole,
/// one stroke across them; without a caret, a run that crosses letters
/// keeps no light.
///
/// A run from below (a move up a page) that lands among letters has no room
/// on the caret's row: kept there it drew nothing but its landing square
/// (`classic-light-move-up`, looks9 Q11). With `up_stretch` it runs whole
/// from the old caret to the new, passing the lettered cells off the
/// caret's at [`TRACER_UNDER_LETTERS`] of its light ([`up_stretch`]).
#[inline]
fn tracer_on_caret_row(
    (verts, dense, shares): (
        &mut Vec<BeamVertex>,
        &mut Vec<BeamVertex>,
        &mut TracerShares,
    ),
    text: TextView<'_>,
    geom: Geom,
    caret_row: Option<i32>,
    (half, leave, top_only, up_stretch): (f32, f32, bool, bool),
    anchored: bool,
) {
    if verts.len() < 2 {
        return;
    }
    let (ox, oy) = (f32::from(geom.origin_x), f32::from(geom.origin_y));
    let (cw, ch) = (geom.cw as f32, geom.ch as f32);
    dense.clear();
    let mut vi = 0;
    while vi + 1 < verts.len() {
        let (a, b) = (verts[vi], verts[vi + 1]);
        vi += 1;
        let n = (crate::color_math::hypot_f32(b.x - a.x, b.y - a.y) * (1.0 / TRACER_STEP_PX))
            .ceil()
            .max(1.0) as u32;
        let inv_n = 1.0 / n as f32;
        let mut i = 0;
        while i < n {
            let t = i as f32 * inv_n;
            dense.push(BeamVertex {
                x: a.x + (b.x - a.x) * t,
                y: a.y + (b.y - a.y) * t,
                color: lerp_rgb(a.color, b.color, t),
                cov: (f32::from(a.cov) + (f32::from(b.cov) - f32::from(a.cov)) * t).round() as u8,
            });
            i += 1;
        }
    }
    dense.push(verts[verts.len() - 1]);
    let reach = half + TRACER_CLEAR_PX;
    let reach2 = reach * reach;
    let (inv_cw, inv_ch) = (1.0 / cw, 1.0 / ch);
    let mut crosses = false;
    let mut di = 0;
    'verts: while di < dense.len() {
        let v = &dense[di];
        di += 1;
        let (row, col) = (
            ((v.y - oy) * inv_ch).floor() as i32,
            ((v.x - ox) * inv_cw).floor() as i32,
        );
        let mut r = row - 1;
        while r <= row + 1 {
            if Some(r) != caret_row && text.covers_col_range(r, (col - 2, col + 1)) {
                let y0 = oy + r as f32 * ch;
                let dy = (y0 - v.y).max(v.y - (y0 + ch)).max(0.0);
                let dy2 = dy * dy;
                if dy2 < reach2 {
                    let mut c = col - 1;
                    while c <= col + 1 {
                        let x0 = ox + c as f32 * cw;
                        let dx = (x0 - v.x).max(v.x - (x0 + cw)).max(0.0);
                        if dx * dx + dy2 < reach2 && text.covers_col_range(r, (c - 1, c)) {
                            crosses = true;
                            break 'verts;
                        }
                        c += 1;
                    }
                }
            }
            r += 1;
        }
    }
    if !crosses {
        return;
    }
    let Some(row) = caret_row else {
        let mut vi = 0;
        while vi < dense.len() {
            dense[vi].cov = 0;
            vi += 1;
        }
        std::mem::swap(verts, dense);
        return;
    };
    let (top, bottom) = (oy + row as f32 * ch, oy + (row + 1) as f32 * ch);
    // The edge the run enters the caret's row by: where it lies walking back
    // from the caret, the run's last vertex.
    let mut from_below = false;
    if !top_only {
        let mut idx = dense.len();
        while idx > 0 {
            idx -= 1;
            let v = &dense[idx];
            if v.y - half < top || v.y + half > bottom {
                from_below = v.y + half > bottom;
                break;
            }
        }
    }
    if from_below && up_stretch {
        self::up_stretch((dense, shares), text, geom, (half, leave), anchored);
        std::mem::swap(verts, dense);
        return;
    }
    let room = match dense.last() {
        Some(v) if from_below => bottom - (v.y + half),
        Some(v) => v.y - half - top,
        None => leave,
    };
    let span = leave.min(room).max(1.0);
    let inv_span = 1.0 / span;
    let mut vi = 0;
    while vi < dense.len() {
        let v = &mut dense[vi];
        vi += 1;
        if v.y - half < top || v.y + half > bottom {
            v.cov = 0;
            continue;
        }
        let inside = if from_below {
            bottom - (v.y + half)
        } else {
            v.y - half - top
        };
        let u = (inside * inv_span).clamp(0.0, 1.0);
        let k = u * u * (3.0 - 2.0 * u);
        v.cov = (f32::from(v.cov) * k).round() as u8;
    }
    std::mem::swap(verts, dense);
}

/// A tracer up a page (`dense`, cut to [`TRACER_STEP_PX`] steps, its last
/// vertex the caret's) runs whole from the old caret to the new: where its
/// stroke, `half` wide, comes within [`TRACER_CLEAR_PX`] of a lettered cell
/// off the caret's own it keeps [`TRACER_UNDER_LETTERS_PAPER`] of its ink,
/// easing back to whole over `leave` px along it ([`through_letters`]).
/// Kept to its longest stretch clear of the letters (`anchored` false, as
/// looks9 laid it), it was a dash floating between the old caret and the
/// new, anchored to neither (`classic-light-move-up.up`, looks10 L6).
#[inline]
fn up_stretch(
    (dense, shares): (&mut [BeamVertex], &mut TracerShares),
    text: TextView<'_>,
    geom: Geom,
    (half, leave): (f32, f32),
    anchored: bool,
) {
    if anchored {
        through_letters(
            dense.len(),
            |i| (dense[i].x, dense[i].y),
            |r, c| {
                if text.covers_cell(r, c) {
                    Passed::Letter
                } else {
                    Passed::Air
                }
            },
            geom,
            (half, leave, TRACER_UNDER_LETTERS_PAPER),
            shares,
            None,
        );
        for (v, k) in dense.iter_mut().zip(&shares.shares) {
            v.cov = (f32::from(v.cov) * k).round() as u8;
        }
        return;
    }
    let (ox, oy) = (f32::from(geom.origin_x), f32::from(geom.origin_y));
    let (cw, ch) = (geom.cw as f32, geom.ch as f32);
    let (inv_cw, inv_ch) = (1.0 / cw, 1.0 / ch);
    let reach = half + TRACER_CLEAR_PX;
    let reach2 = reach * reach;
    let clear = |v: &BeamVertex| {
        let (row, col) = (
            ((v.y - oy) * inv_ch).floor() as i32,
            ((v.x - ox) * inv_cw).floor() as i32,
        );
        let mut r = row - 1;
        while r <= row + 1 {
            if text.covers_col_range(r, (col - 1, col + 1)) {
                let y0 = oy + r as f32 * ch;
                let dy = (y0 - v.y).max(v.y - (y0 + ch)).max(0.0);
                let dy2 = dy * dy;
                if dy2 < reach2 {
                    let mut c = col - 1;
                    while c <= col + 1 {
                        let x0 = ox + c as f32 * cw;
                        let dx = (x0 - v.x).max(v.x - (x0 + cw)).max(0.0);
                        if dx * dx + dy2 < reach2 && text.covers_cell(r, c) {
                            return false;
                        }
                        c += 1;
                    }
                }
            }
            r += 1;
        }
        true
    };
    // The longest run of clear vertices, by length along the stroke.
    let (mut best, mut best_len) = ((0usize, 0usize), 0.0f32);
    let mut start: Option<(usize, f32)> = None;
    let mut along = 0.0f32;
    let mut i = 0;
    while i <= dense.len() {
        if i > 0 && i < dense.len() {
            let (a, b) = (dense[i - 1], dense[i]);
            along += crate::color_math::hypot_f32(b.x - a.x, b.y - a.y);
        }
        let ok = i < dense.len() && clear(&dense[i]);
        match (ok, start) {
            (true, None) => start = Some((i, along)),
            (false, Some((from, at))) => {
                let len = along - at;
                if len > best_len {
                    (best, best_len) = ((from, i), len);
                }
                start = None;
            }
            _ => {}
        }
        i += 1;
    }
    let mut i = 0;
    while i < best.0 {
        dense[i].cov = 0;
        i += 1;
    }
    let span = leave.min(0.5 * best_len).max(1.0);
    let inv_span = 1.0 / span;
    let mut walked = 0.0f32;
    while i < best.1 {
        if i > best.0 {
            let (a, b) = (dense[i - 1], dense[i]);
            walked += crate::color_math::hypot_f32(b.x - a.x, b.y - a.y);
        }
        let u = (walked.min(best_len - walked) * inv_span).clamp(0.0, 1.0);
        let k = u * u * (3.0 - 2.0 * u);
        dense[i].cov = (f32::from(dense[i].cov) * k).round() as u8;
        i += 1;
    }
    while i < dense.len() {
        dense[i].cov = 0;
        i += 1;
    }
}

/// The share of its light a classic tracer up a page keeps where it passes a
/// lettered cell off the caret's own ([`through_letters`]): faint, as if
/// seen between the strokes, as the dark landing ring's rim passes under
/// the letters (`RIM_UNDER_LETTERS`). The letter guard holds every letter
/// it passes to its bar.
const TRACER_UNDER_LETTERS: f32 = 0.2;
/// The share of its light a one-colour tracer up a dark page keeps in the
/// air between the rows it rises past ([`through_letters`] maps a whole share
/// onto it, the letters' [`TRACER_UNDER_LETTERS`] kept): whole, its core
/// burned white-hot in the blank row and over the `}`, the run through the
/// lettered rows a faint thread beside it, so it read as a dash with thin
/// tails, and on Solarized Dark, whose letters take no light, a lone glaring
/// slash (`#ffbaff` where looks9's was `#927aae`; looks10 round 2's L6).
const TRACER_UP_AIR: f32 = 0.55;
/// How much wider than the tracer's core its stretch under a dim theme's
/// letters is laid ([`TRACER_UP_AIR`]'s run): a hue at the page's own
/// luminance reads only as wide as a stroke, not a hairline.
const TRACER_UP_SHADE_THICK: f32 = 1.6;
/// [`TRACER_UNDER_LETTERS`] on paper, where the tracer is ink: at a fifth
/// of its ink it all but vanished behind the letters and stopped short of
/// the caret's landing square to the eye.
const TRACER_UNDER_LETTERS_PAPER: f32 = 0.4;

/// What a tracer passing a cell sees there ([`through_letters`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Passed {
    /// A blank cell.
    Air,
    /// A cell a line of text covers.
    Letter,
    /// A cell of a row the frame does not show.
    Unread,
}

/// [`through_letters`]'s output, each point's share of a tracer's light, and its walk's
/// resident buffers: kept by the engine, so a frame's tracers allocate nothing once the
/// longest has been walked.
struct TracerShares {
    shares: Vec<f32>,
    /// Each point's distance along the stroke.
    along: Vec<f32>,
    /// Each point's (near a letter, near an unread row).
    marks: Vec<(bool, bool)>,
    /// Each point's distance along the stroke to the nearest of each mark.
    gaps: Vec<(f32, f32)>,
}

impl TracerShares {
    const EMPTY: Self = Self {
        shares: Vec::new(),
        along: Vec::new(),
        marks: Vec::new(),
        gaps: Vec::new(),
    };
}

impl Default for TracerShares {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

/// The share of its light each of a tracer's `n` points (`at(i)`, window
/// px, dense along its stroke, the last the caret's) keeps, into `shares`:
/// `under` ([`TRACER_UNDER_LETTERS`]) where its stroke, `half` wide, comes within
/// [`TRACER_CLEAR_PX`] of a cell `seen` names a [`Passed::Letter`] other than
/// the caret's, none where it comes within reach of a [`Passed::Unread`] one
/// (the letter guard cannot hold letters it has not read, so light there is
/// the over-ink budget's, as the dark streak's is: [`TextView::over_text`]),
/// whole `leave` px along the stroke from every such point, eased between.
/// `shade`, when given, takes the same shares with no light kept under letters
/// (`under` of `0`), read off the same walk.
#[inline]
fn through_letters(
    n: usize,
    at: impl Fn(usize) -> (f32, f32),
    seen: impl Fn(i32, i32) -> Passed,
    geom: Geom,
    (half, leave, under): (f32, f32, f32),
    scratch: &mut TracerShares,
    shade: Option<&mut Vec<f32>>,
) {
    let TracerShares {
        shares,
        along,
        marks,
        gaps,
    } = scratch;
    shares.clear();
    if n == 0 {
        if let Some(shade) = shade {
            shade.clear();
        }
        return;
    }
    let (ox, oy) = (f32::from(geom.origin_x), f32::from(geom.origin_y));
    let (cw, ch) = (geom.cw as f32, geom.ch as f32);
    let (inv_cw, inv_ch) = (1.0 / cw, 1.0 / ch);
    let cell_of = |(x, y): (f32, f32)| {
        (
            ((y - oy) * inv_ch).floor() as i32,
            ((x - ox) * inv_cw).floor() as i32,
        )
    };
    let caret = cell_of(at(n - 1));
    let reach = half + TRACER_CLEAR_PX;
    let reach2 = reach * reach;
    // (near a letter, near an unread row)
    let near = |(x, y): (f32, f32)| {
        let (row, col) = cell_of((x, y));
        let (mut letter, mut unread) = (false, false);
        for r in row - 1..=row + 1 {
            let y0 = oy + r as f32 * ch;
            let dy = (y0 - y).max(y - (y0 + ch)).max(0.0);
            let dy2 = dy * dy;
            if dy2 >= reach2 {
                continue;
            }
            for c in col - 1..=col + 1 {
                if (r, c) == caret {
                    continue;
                }
                let x0 = ox + c as f32 * cw;
                let dx = (x0 - x).max(x - (x0 + cw)).max(0.0);
                if dx * dx + dy2 < reach2 {
                    match seen(r, c) {
                        Passed::Air => {}
                        Passed::Letter => letter = true,
                        Passed::Unread => unread = true,
                    }
                    if letter && unread {
                        return (true, true);
                    }
                }
            }
        }
        (letter, unread)
    };
    // One walk reads each point once: its distance along the stroke and its marks.
    along.clear();
    marks.clear();
    let mut walked = 0.0f32;
    let mut prev = at(0);
    for i in 0..n {
        let here = if i > 0 { at(i) } else { prev };
        if i > 0 {
            walked += crate::color_math::hypot_f32(here.0 - prev.0, here.1 - prev.1);
        }
        along.push(walked);
        marks.push(near(here));
        prev = here;
    }
    // Each point's distance along the stroke from the nearest point near a
    // letter and from the nearest near an unread row, both ways, in one pass
    // each way.
    gaps.clear();
    gaps.resize(n, (f32::INFINITY, f32::INFINITY));
    let (mut last_l, mut last_u): (Option<f32>, Option<f32>) = (None, None);
    for i in 0..n {
        let (l, u) = marks[i];
        if l {
            last_l = Some(along[i]);
        }
        if u {
            last_u = Some(along[i]);
        }
        if let Some(x) = last_l {
            gaps[i].0 = along[i] - x;
        }
        if let Some(x) = last_u {
            gaps[i].1 = along[i] - x;
        }
    }
    (last_l, last_u) = (None, None);
    for i in (0..n).rev() {
        let (l, u) = marks[i];
        if l {
            last_l = Some(along[i]);
        }
        if u {
            last_u = Some(along[i]);
        }
        if let Some(x) = last_l {
            gaps[i].0 = gaps[i].0.min(x - along[i]);
        }
        if let Some(x) = last_u {
            gaps[i].1 = gaps[i].1.min(x - along[i]);
        }
    }
    let inv_leave = 1.0 / leave.max(1.0);
    let eased = |floor: f32, d: f32| {
        let u = (d * inv_leave).clamp(0.0, 1.0);
        floor + (1.0 - floor) * u * u * (3.0 - 2.0 * u)
    };
    shares.extend(
        gaps.iter()
            .map(|&(dl, du)| eased(under, dl).min(eased(0.0, du))),
    );
    if let Some(shade) = shade {
        shade.clear();
        shade.extend(
            gaps.iter()
                .map(|&(dl, du)| eased(0.0, dl).min(eased(0.0, du))),
        );
    }
}

/// Drop the points of a dense stroke `piece` that change nothing laid
/// unstraightened: a point inside a stretch of one share (`shares`, the
/// piece's own, point for point) on a straight line between its
/// neighbours. Its light between the points it keeps is the line's.
#[inline]
fn thin_exact(piece: &mut Vec<CometSample>, shares: &[f32]) {
    let n = piece.len().min(shares.len());
    if n < 3 {
        return;
    }
    let mut kept = 1usize;
    for i in 1..n - 1 {
        let (a, b, c) = (piece[kept - 1], piece[i], piece[i + 1]);
        let turn = (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x);
        let flat = shares[i - 1] == shares[i] && shares[i] == shares[i + 1];
        if !flat || turn.abs() > 0.01 {
            piece[kept] = b;
            kept += 1;
        }
    }
    piece[kept] = piece[n - 1];
    piece.truncate(kept + 1);
}

/// `run` cut to [`TRACER_STEP_PX`] steps, into `dense`.
#[inline]
fn dense_samples(run: &[CometSample], dense: &mut Vec<CometSample>) {
    dense.clear();
    for w in run.windows(2) {
        let (a, b) = (w[0], w[1]);
        let n = (crate::color_math::hypot_f32(b.x - a.x, b.y - a.y) * (1.0 / TRACER_STEP_PX))
            .ceil()
            .max(1.0) as u32;
        let inv_n = 1.0 / n as f32;
        for i in 0..n {
            let t = i as f32 * inv_n;
            dense.push(CometSample {
                x: a.x + (b.x - a.x) * t,
                y: a.y + (b.y - a.y) * t,
                cov: (f32::from(a.cov) + (f32::from(b.cov) - f32::from(a.cov)) * t).round() as u8,
                pos: a.pos + (b.pos - a.pos) * t,
            });
        }
    }
    if let Some(&last) = run.last() {
        dense.push(last);
    }
}

/// How far a one-colour air piece's knees are drawn back from the letters,
/// in core widths ([`one_air_piece`]): its outer bloom's half width (the
/// comet's widest layer, 5.5 cores) and a little.
const MONO_AIR_PULL_CORES: f32 = 3.2;

/// Of the air pieces `air[from..n]` one run across rows split into, keep the
/// longest and the caret's stub (the last, when `stub`): a one-colour
/// tracer up a dark page across lettered rows fell into dashes between
/// them, the lowest touching `[vim]` below the old caret
/// (`classic-pink/classic-dark-move-up.up`, looks9 Q11). Each kept piece's
/// knees are drawn back `pull` px from the letters. Returns the pieces
/// kept, moved to the front of `air[from..]`.
#[inline]
fn one_air_piece(
    air: &mut [Vec<CometSample>],
    (from, n): (usize, usize),
    stub: bool,
    pull: f32,
) -> usize {
    let pieces = n.saturating_sub(from);
    let body = if stub {
        pieces.saturating_sub(1)
    } else {
        pieces
    };
    // Each knee (a dark end at a lettered cell's edge) drawn back `pull` px
    // into the air, at half its neighbour's light: laid at the edge, the
    // stroke's light met the letters, and a straight piece between two dark
    // knees, straightened to them, laid none.
    let mut pi = from;
    while pi < from + body {
        let piece = &mut air[pi];
        pi += 1;
        let len = piece.len();
        if len < 2 {
            continue;
        }
        let ends = [(0, 1), (len - 1, len - 2)];
        let mut ei = 0;
        while ei < ends.len() {
            let (end, next) = ends[ei];
            ei += 1;
            let (k, a) = (piece[end], piece[next]);
            if k.cov != 0 {
                continue;
            }
            let d = crate::color_math::hypot_f32(a.x - k.x, a.y - k.y);
            let t = (pull / d.max(1e-3)).min(1.0);
            piece[end].x = k.x + (a.x - k.x) * t;
            piece[end].y = k.y + (a.y - k.y) * t;
            piece[end].cov = a.cov / 2;
        }
    }
    if body <= 1 {
        return pieces;
    }
    #[inline]
    const fn piece_len(r: &[CometSample]) -> f32 {
        let mut sum = 0.0f32;
        let mut i = 1;
        while i < r.len() {
            let (a, b) = (r[i - 1], r[i]);
            sum += crate::color_math::hypot_f32(b.x - a.x, b.y - a.y);
            i += 1;
        }
        sum
    }
    let mut longest = from;
    let mut longest_len = piece_len(&air[from]);
    let mut i = from + 1;
    while i < from + body {
        let len = piece_len(&air[i]);
        if len.total_cmp(&longest_len).is_ge() {
            longest = i;
            longest_len = len;
        }
        i += 1;
    }
    air.swap(from, longest);
    if stub {
        air.swap(from + 1, n - 1);
        2
    } else {
        1
    }
}

/// Returns the comet colour at path position `pos` (`0` tail to `1` head).
#[must_use]
#[inline]
pub const fn comet_color(mono: bool, color: u32, accent: u32, hue: f32, pos: f32) -> u32 {
    if mono {
        lerp_rgb(accent, color, pos)
    } else {
        hsv2rgb((hue + pos * 0.5) % 1.0, 0.95, 1.0)
    }
}

impl ClassicWake {
    const MAX_QUADS: usize = 8192;
    const MAX_SPARKS: usize = 512;
    const CROWN_MS: u64 = 200;
    const CROWN_TYPING_MS: u64 = 350;
    const HEAT_GAIN: f32 = 0.16;
    const HEAT_DECAY_TAU: f32 = 0.9;
    const HEAT_GAP_FULL: f32 = 0.09;
    const HEAT_GAP_ZERO: f32 = 0.40;
    const CHAIN_GAP_MAX: f32 = 0.40;
    const CHAIN_MARGIN: f32 = 0.10;
    const CHAIN_LIFE_MAX: f32 = 0.50;
    /// The share of its hue the one-colour face's bed keeps under a lit
    /// page's letters ([`crate::cursor_glow::page_lit_ink`]), the rest the
    /// page: a chosen colour far round the wheel from the page's (`#d03a8c`
    /// on Solarized Dark's teal) laid a maroon smear under the letters at
    /// the page's luminance, and the letters' edges, blended over it, read
    /// under their bar (`f` at 3.93:1 of its own 4.06, looks8 K10). On a
    /// tinted dark page the rest is the page's lightness without its tint
    /// ([`WakeInk::bed_ground`]): mixed toward the teal, the pink pin's bed
    /// read blue (hue 222, composed r1's O4(b)).
    const MONO_LIT_BED_SHARE: f32 = 0.4;

    /// Fresh dormant classic-wake animation state.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            sparks: Vec::new(),
            path_scratch: Vec::new(),
            ring: None,
            last: None,
            last_move: None,
            crown_until: None,
            crown_window_ms: 0,
            hue: 0.0,
            heat: 0.0,
            heat_at: None,
            last_type: None,
            comet_runs: Vec::new(),
            comet_live: 0,
            light_verts: Vec::new(),
            light_dense: Vec::new(),
            spawned: None,
            bed_scratch: BedScratch::EMPTY,
            air_runs: Vec::new(),
            air_pieces: Vec::new(),
            air_shares: TracerShares::EMPTY,
            held_runs: Vec::new(),
            up_line: Vec::new(),
            up_line_scratch: BeamScratch::EMPTY,
            up_shade: Vec::new(),
            up_verts: Vec::new(),
            crown_ink_memo: None,
            veil_cap_memo: None,
            #[cfg(test)]
            mono_unbedded: false,
            #[cfg(test)]
            light_unsolved: false,
            #[cfg(test)]
            veil_unbound: false,
            #[cfg(test)]
            tracer_over_letters: false,
            #[cfg(test)]
            tracer_leave_px: None,
            #[cfg(test)]
            ring_flying: false,
            #[cfg(any(test, debug_assertions))]
            ring_cut: false,
            #[cfg(any(test, debug_assertions))]
            typed_life_unscaled: false,
            #[cfg(any(test, debug_assertions))]
            mono_pin_floored: false,
            #[cfg(any(test, debug_assertions))]
            mono_rail_tint_blind: false,
            #[cfg(test)]
            ring_unlatched: false,
            #[cfg(test)]
            ring_window_edge: false,
            #[cfg(test)]
            ring_row_square: false,
            #[cfg(test)]
            ring_seen: std::cell::RefCell::new(Vec::new()),
            #[cfg(test)]
            tracer_top_only: false,
            #[cfg(test)]
            paper_muddy: false,
            #[cfg(test)]
            crown_cell_wide: false,
            #[cfg(test)]
            mono_lit_whole: false,
            #[cfg(test)]
            tracer_up_kept: false,
            #[cfg(test)]
            paper_pin_saturated: false,
            #[cfg(test)]
            tracer_up_stretch: false,
            #[cfg(test)]
            tracer_up_stairs: false,
            #[cfg(test)]
            tracer_up_unshaded: false,
            #[cfg(test)]
            tracer_up_whole: false,
            #[cfg(test)]
            tracer_up_lit_letters: false,
            #[cfg(any(test, debug_assertions))]
            tracer_up_faint_shade: false,
        }
    }

    #[inline]
    const fn typing_boost(&self) -> f32 {
        0.28 + 0.92 * self.heat
    }

    /// Returns whether any wake light is still alive.
    #[must_use]
    #[inline]
    pub const fn is_active(&self) -> bool {
        !self.sparks.is_empty() || self.ring.is_some() || self.crown_until.is_some()
    }

    /// Takes the `(from, to)` move the last tick spawned on.
    #[inline]
    pub const fn take_spawn(&mut self) -> Option<((u16, u16), (u16, u16))> {
        self.spawned.take()
    }

    /// Returns whether the engine remembers a cursor cell.
    #[must_use]
    #[inline]
    pub const fn has_last(&self) -> bool {
        self.last.is_some()
    }

    /// Returns the number of live swept-path samples.
    #[must_use]
    #[inline]
    pub const fn live_sparks(&self) -> usize {
        self.sparks.len()
    }

    /// Returns the live sparks' cells.
    #[cfg(test)]
    pub(crate) fn spark_cells(&self) -> Vec<(u16, u16)> {
        self.sparks.iter().map(|s| (s.row, s.col)).collect()
    }

    /// Returns the `(first, last)` row range spanned by live sparks.
    #[inline]
    pub(crate) const fn row_range(&self) -> Option<(u16, u16)> {
        let sparks = self.sparks.as_slice();
        if sparks.is_empty() {
            return None;
        }
        let mut lo = sparks[0].row;
        let mut hi = lo;
        let mut i = 1;
        while i < sparks.len() {
            let r = sparks[i].row;
            if r < lo {
                lo = r;
            }
            if r > hi {
                hi = r;
            }
            i += 1;
        }
        Some((lo, hi))
    }

    /// Returns the landing ring's cell.
    #[cfg(test)]
    pub(crate) const fn ring_cell(&self) -> Option<(u16, u16)> {
        match self.ring {
            Some(r) => Some((r.row, r.col)),
            None => None,
        }
    }

    /// Drops all in-flight light and forgets the last cursor position.
    #[inline]
    pub fn reset(&mut self) {
        self.sparks.clear();
        self.ring = None;
        self.last = None;
        self.spawned = None;
        self.last_move = None;
        self.crown_until = None;
        self.crown_window_ms = Self::CROWN_MS;
        self.heat = 0.0;
        self.heat_at = None;
        self.last_type = None;
    }

    /// Translates row-addressed members under `map`, dropping any that leave the grid.
    #[inline]
    pub fn translate_rows(&mut self, map: &impl Fn(u16) -> Option<u16>) {
        self.sparks.retain_mut(|s| match map(s.row) {
            Some(row) => {
                s.row = row;
                true
            }
            None => false,
        });
        if let Some(r) = self.ring {
            self.ring = map(r.row).map(|row| Ring { row, ..r });
        }
        if let Some((row, col)) = self.last {
            self.last = map(row).map(|r| (r, col));
        }
    }

    /// Advances one frame and emits wake quads and halos.
    #[allow(
        clippy::too_many_arguments,
        reason = "clock, config, geometry, cursor, the three streams and the over-text facts"
    )]
    #[inline]
    pub(crate) fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        out: &mut RampSink<'_>,
        halos: &mut Vec<RainHalo>,
        under: &mut RampSink<'_>,
        mut ink: WakeInk<'_>,
    ) -> u64 {
        out.clear();
        halos.clear();
        under.clear();
        self.spawned = None;
        if !cfg.enabled || cfg.intensity <= 0.0 || geom.cw == 0 || geom.ch == 0 {
            self.sparks.clear();
            self.ring = None;
            self.crown_until = None;
            self.last = cur;
            self.heat = 0.0;
            self.heat_at = None;
            self.last_type = None;
            return 0;
        }

        if let Some(t0) = self.heat_at
            && self.heat > 0.0
        {
            const INV_HEAT_DECAY_TAU: f32 = 1.0 / ClassicWake::HEAT_DECAY_TAU;
            let dt = now.saturating_duration_since(t0).as_secs_f32();
            if dt > 0.0 {
                self.heat *= crate::color_math::exp_f32(-dt * INV_HEAT_DECAY_TAU);
                if self.heat < 0.005 {
                    self.heat = 0.0;
                }
            }
        }
        self.heat_at = Some(now);

        if let (Some((pr, pc)), Some((cr, cc))) = (self.last, cur)
            && (pr != cr || pc != cc)
        {
            let (dr, dc) = (cr.abs_diff(pr), cc.abs_diff(pc));
            let typing = dr.max(dc) <= 1 || (dr == 0 && dc == 2);
            self.crown_window_ms = if typing {
                Self::CROWN_TYPING_MS
            } else {
                Self::CROWN_MS
            };
            self.spawn((pr, pc), (cr, cc), typing, now, cfg, geom);
            self.spawned = Some(((pr, pc), (cr, cc)));
            self.last_move = Some(now);
            self.crown_until = Some(now + Duration::from_millis(self.crown_window_ms));
        }
        self.last = cur;

        if !self.sparks.is_empty() {
            let mut last_alive: Option<((Instant, u32), bool)> = None;
            self.sparks.retain(|s| {
                let key = (s.born, s.life.to_bits());
                match last_alive {
                    Some((prev_key, alive)) if prev_key == key => alive,
                    _ => {
                        let alive = now.saturating_duration_since(s.born).as_secs_f32() < s.life;
                        last_alive = Some((key, alive));
                        alive
                    }
                }
            });
            if self.sparks.len() > Self::MAX_SPARKS {
                let drop = self.sparks.len() - Self::MAX_SPARKS;
                self.sparks.drain(0..drop);
            }
        }
        if let Some(r) = self.ring
            && now.saturating_duration_since(r.born).as_secs_f32() >= r.life
        {
            self.ring = None;
        }
        if matches!(self.crown_until, Some(t) if now >= t) {
            self.crown_until = None;
        }

        if cfg.dark_theme && cfg.classic_mono && !self.mono_unbedded() {
            self.emit_mono_dark(
                now,
                cfg,
                geom,
                cur,
                (out, under),
                (ink.text, ink.chosen),
                ink.ledger,
                (ink.feet, ink.bed_ground),
            );
            out.keep_laid(Self::MAX_QUADS);
            under.keep_laid(Self::MAX_QUADS);
            let from = halos.len();
            self.emit_mono_crown(now, cfg, geom, cur, halos);
            if halos.len() > from {
                ink.ledger
                    .admit_halos_masked(halos, from, InkLedger::HEAD_FLOOR);
            }
            // THE ONE-COLOUR LANDING RING KEEPS EVERY LETTER'S BAR. It lands
            // on the new prompt, whose `$` its left bar crosses. Drawn over
            // the letters, the frame's bloom and SDR crown (the post passes
            // over `cursor_glow_add`, which no admission reads) spread its bar
            // a cell past every cut and lifted the ground beside the `$`'s
            // strokes to 4.21-4.32:1 (the trail gallery's classic-pink
            // `enter-bottom` e120, looks7, 10 of 10 captures). It lies under
            // the letters instead, where neither pass reaches it, admitted
            // at the letter floor. The SPECTRUM ring stays the ruled v0.28
            // light, over the letters and unadmitted.
            #[cfg(test)]
            let flying = self.ring_flying;
            #[cfg(not(test))]
            let flying = false;
            if flying {
                self.lay_mono_ring(now, cfg, geom, out, ink.ledger, InkLedger::FLYING_FLOOR);
            } else {
                self.lay_whole_mono_ring(now, cfg, geom, under, &mut ink);
            }
            out.keep_laid(Self::MAX_QUADS);
        } else if cfg.dark_theme {
            self.emit_comet(now, cfg, geom, cur, out);
            out.keep_laid(Self::MAX_QUADS);
            if out.laid() < Self::MAX_QUADS {
                self.emit_crown(now, cfg, geom, cur, out.quads());
            }
            if out.laid() < Self::MAX_QUADS {
                self.emit_ring(now, cfg, geom, out.quads());
            }
            out.keep_laid(Self::MAX_QUADS);
        } else {
            #[cfg(test)]
            let chosen = ink.chosen && !self.paper_pin_saturated;
            #[cfg(not(test))]
            let chosen = ink.chosen;
            let chosen = chosen && cfg.classic_mono;
            self.emit_light_crown(now, cfg, geom, cur, under.quads(), chosen);
            self.emit_light_comet(now, cfg, geom, cur, under, (ink.text, chosen));
            under.keep_laid(Self::MAX_QUADS);
            if let Some(ring) = &mut self.ring
                && let Some((r, c)) = cur
                && usize::from(r) < geom.rows
                && usize::from(c) < geom.cols
            {
                ring.caret = (r, c);
            }
            self.emit_light_ring(now, cfg, geom, cur, halos);
            halos.truncate(CursorGlow::MAX_HALOS);
            if out.laid() < Self::MAX_QUADS {
                let from = out.quads().len();
                self.emit_light_ring_rim(now, cfg, geom, ink.text, out.quads());
                let to = out.quads().len();
                if to > from {
                    let laid = quads_area(&out.quads()[from..to]);
                    ink.ledger
                        .admit_masked(out.quads(), from..to, InkLedger::FLYING_FLOOR);
                    #[cfg(any(test, debug_assertions))]
                    let cut = self.ring_cut;
                    #[cfg(not(any(test, debug_assertions)))]
                    let cut = false;
                    if !cut {
                        keep_whole(out.quads(), from, to, laid);
                    }
                }
                #[cfg(test)]
                self.ring_seen.replace(
                    out.quads()
                        .get(from..)
                        .map_or_else(Vec::new, <[GlowQuad]>::to_vec),
                );
            }
            out.keep_laid(Self::MAX_QUADS);
        }

        Self::fingerprint(out, under, halos)
    }

    fn fingerprint(out: &mut RampSink<'_>, under: &mut RampSink<'_>, halos: &[RainHalo]) -> u64 {
        let mut fp: u64 = 0;
        let out_quads = out.quads().as_slice();
        let mut qi = 0;
        while qi < out_quads.len() {
            let q = &out_quads[qi];
            qi += 1;
            fp = fp.wrapping_mul(1_000_003).wrapping_add(
                ((q.row as u64) << 40)
                    ^ ((q.x as u64) << 28)
                    ^ ((q.y as u64) << 16)
                    ^ ((q.w as u64) << 8)
                    ^ (q.h as u64)
                    ^ ((q.color as u64) << 20),
            );
        }
        let under_quads = under.quads().as_slice();
        let mut uqi = 0;
        while uqi < under_quads.len() {
            let q = &under_quads[uqi];
            uqi += 1;
            fp = fp.wrapping_mul(1_000_003).wrapping_add(
                ((q.row as u64) << 40)
                    ^ ((q.x as u64) << 28)
                    ^ ((q.y as u64) << 16)
                    ^ ((q.w as u64) << 8)
                    ^ (q.h as u64)
                    ^ ((q.color as u64) << 20)
                    ^ ((q.color2 as u64) << 4)
                    ^ (u64::from(q.alpha) << 56)
                    ^ (u64::from(q.alpha2) << 48),
            );
        }
        if !halos.is_empty() {
            let mut hi = 0;
            while hi < halos.len() {
                let words = halos[hi].damage_words();
                hi += 1;
                let mut wi = 0;
                while wi < words.len() {
                    fp = fp.wrapping_mul(1_000_003).wrapping_add(words[wi]);
                    wi += 1;
                }
            }
        }
        let ramp_streams = [&*out.ramps(), &*under.ramps()];
        let mut si = 0;
        while si < ramp_streams.len() {
            let ramps = ramp_streams[si];
            si += 1;
            let items = ramps.items.as_slice();
            let mut ri = 0;
            while ri < items.len() {
                let r = &items[ri];
                ri += 1;
                fp = fp.wrapping_mul(1_000_003).wrapping_add(
                    (u64::from(r.at) << 32)
                        ^ (u64::from(r.row) << 40)
                        ^ (u64::from(r.x) << 28)
                        ^ (u64::from(r.y) << 16)
                        ^ (u64::from(r.w) << 8)
                        ^ u64::from(r.h)
                        ^ (u64::from(r.joined) << 48)
                        ^ (u64::from(r.join) << 56),
                );
                let ink = r.ink as usize;
                let chunks = ramps.ends[ink..ink + r.end_count()].as_chunks::<2>().0;
                let mut ci = 0;
                while ci < chunks.len() {
                    let e = &chunks[ci];
                    ci += 1;
                    fp = fp
                        .wrapping_mul(1_000_003)
                        .wrapping_add(u64::from(e[0]) << 32 | u64::from(e[1]));
                }
            }
        }
        if out.laid() > 0 || under.laid() > 0 || !halos.is_empty() {
            fp = fp.max(1);
        }
        fp
    }

    #[inline]
    fn spawn(
        &mut self,
        (pr, pc): (u16, u16),
        (cr, cc): (u16, u16),
        typing: bool,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
    ) {
        let gap = if typing {
            let gap = match self.last_type {
                Some(t) => now.saturating_duration_since(t).as_secs_f32(),
                None => f32::MAX,
            };
            const INV_HEAT_GAP_SPAN: f32 =
                1.0 / (ClassicWake::HEAT_GAP_ZERO - ClassicWake::HEAT_GAP_FULL);
            let cadence = 1.0 - ((gap - Self::HEAT_GAP_FULL) * INV_HEAT_GAP_SPAN).clamp(0.0, 1.0);
            self.heat = (self.heat + cadence * Self::HEAT_GAIN).min(1.0);
            self.last_type = Some(now);
            gap
        } else {
            f32::MAX
        };
        let boost = if typing { self.typing_boost() } else { 1.0 };
        let spark_life = if typing {
            // The life tuned at the default duration, times the duration's share of it
            // (`cursor_glow::typed_life_basis`), chained keys too.
            #[cfg(any(test, debug_assertions))]
            let unscaled = self.typed_life_unscaled;
            #[cfg(not(any(test, debug_assertions)))]
            let unscaled = false;
            let (tuned, scale) = crate::cursor_glow::typed_life_basis(cfg.duration, unscaled);
            let full_life = tuned.max(0.001);
            let heat_life = (full_life * 0.38).clamp(0.05, 0.11) * (1.0 + 1.3 * self.heat);
            let life = if gap <= Self::CHAIN_GAP_MAX {
                heat_life.max((gap + Self::CHAIN_MARGIN).min(Self::CHAIN_LIFE_MAX))
            } else {
                heat_life
            };
            crate::cursor_glow::scaled_typed_life(life, scale)
        } else {
            cfg.duration.as_secs_f32().max(0.001)
        };

        let max_len = cfg.length.clamp(1, Self::MAX_SPARKS);
        self.path_scratch.clear();
        line_cells_tail(
            &mut self.path_scratch,
            (pr as i32, pc as i32),
            (cr as i32, cc as i32),
            max_len,
            false,
        );
        if !self.path_scratch.is_empty() {
            let inv_n = 1.0 / self.path_scratch.len() as f32;
            let (rows_i32, cols_i32) = (geom.rows as i32, geom.cols as i32);
            let mut i = 0;
            while i < self.path_scratch.len() {
                let (r, c) = self.path_scratch[i];
                let idx = i;
                i += 1;
                if r < 0 || c < 0 || r >= rows_i32 || c >= cols_i32 {
                    continue;
                }
                let pos = (idx as f32 + 1.0) * inv_n;
                let born_cov = ((40.0 + 175.0 * pos) * boost).min(255.0) as u8;
                self.sparks.push(Spark {
                    row: r as u16,
                    col: c as u16,
                    born_cov,
                    pos,
                    life: spark_life,
                    typing,
                    born: now,
                });
            }
            if self.sparks.len() > Self::MAX_SPARKS {
                let drop = self.sparks.len() - Self::MAX_SPARKS;
                self.sparks.drain(0..drop);
            }
        }

        self.hue = (self.hue + 0.07).fract();

        if cfg.ring && !typing {
            self.ring = Some(Ring {
                row: cr,
                col: cc,
                dx: (cfg.head_dx - 0.5) * geom.cw as f32,
                born: now,
                life: 0.18,
                caret: (cr, cc),
                mono: MonoRing::Unjudged,
            });
        }
    }

    #[inline]
    fn spark_tf(s: &Spark, now: Instant) -> f32 {
        const PI_OVER_045: f32 = std::f32::consts::PI / 0.45;
        let age = now.saturating_duration_since(s.born).as_secs_f32();
        let frac = (age / s.life).clamp(0.0, 1.0);
        if s.typing {
            if frac < 0.55 {
                1.0
            } else {
                0.5 * (1.0 + crate::supernova::sin_cos_f32((frac - 0.55) * PI_OVER_045).1)
            }
        } else {
            1.0 - frac
        }
    }

    #[inline]
    fn build_runs(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
    ) -> usize {
        if self.sparks.is_empty() {
            return 0;
        }
        let dx = (cfg.head_dx - 0.5) * geom.cw as f32;
        if self.comet_runs.is_empty() {
            self.comet_runs.push(Vec::new());
        }
        self.comet_live = 0;
        self.comet_runs[0].clear();
        let mut prev: Option<(u16, u16)> = None;
        let mut head_cov = 0u8;
        let mut last_tf: Option<((Instant, u32, bool), f32)> = None;
        let mut spark_idx = 0;
        while spark_idx < self.sparks.len() {
            let s = &self.sparks[spark_idx];
            spark_idx += 1;
            let key = (s.born, s.life.to_bits(), s.typing);
            let tf = match last_tf {
                Some((prev_key, cached)) if prev_key == key => cached,
                _ => {
                    let cached = Self::spark_tf(s, now);
                    last_tf = Some((key, cached));
                    cached
                }
            };
            let cov = (f32::from(s.born_cov) * tf * cfg.intensity) as u8;
            let here = (s.row, s.col);
            if cov == 0 {
                prev = Some(here);
                continue;
            }
            if let Some((pr, pc)) = prev {
                let far = (s.row as i32 - pr as i32)
                    .abs()
                    .max((s.col as i32 - pc as i32).abs())
                    > 1;
                if far && !self.comet_runs[self.comet_live].is_empty() {
                    self.comet_live += 1;
                    if self.comet_live == self.comet_runs.len() {
                        self.comet_runs.push(Vec::new());
                    }
                    self.comet_runs[self.comet_live].clear();
                }
            }
            let (x, y) = geom.cell_center(s.row, s.col);
            self.comet_runs[self.comet_live].push(CometSample {
                x: x + dx,
                y,
                cov,
                pos: s.pos,
            });
            prev = Some(here);
            head_cov = cov;
        }
        let live = if self.comet_runs[0].is_empty() {
            0
        } else {
            self.comet_live + 1
        };
        if live == 0 {
            return 0;
        }
        if let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
            && let Some(last) = self.comet_runs[..live].last_mut()
        {
            let (x, y) = geom.cell_center(cr, cc);
            last.push(CometSample {
                x: x + dx,
                y,
                cov: head_cov,
                pos: 1.0,
            });
        }
        live
    }

    #[inline]
    fn emit_comet(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        out: &mut RampSink<'_>,
    ) {
        let live = self.build_runs(now, cfg, geom, cur);
        if live == 0 {
            return;
        }
        let chf = geom.ch as f32;
        let core_thick = (chf * 0.13).max(2.0);
        let s = core_thick as i32;
        let straighten = (geom.cw as f32).max(chf) * 0.8;
        let clip = geom.beam_clip();
        let (mono, color, accent, hue) = (cfg.classic_mono, cfg.color, cfg.accent, self.hue);
        let color_fn = |pos| comet_color(mono, color, accent, hue, pos);

        let mut ri = 0;
        while ri < live {
            let r = &self.comet_runs[ri];
            ri += 1;
            if r.len() < 2 {
                let s0 = r[0];
                push_rect(
                    out.quads(),
                    geom,
                    s0.x as i32 - s / 2,
                    s0.y as i32 - s / 2,
                    s,
                    s,
                    premul_rgb(color_fn(s0.pos), s0.cov),
                );
                continue;
            }
            comet_glow_quads(out, clip, r, core_thick, straighten, &color_fn);
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "clock, config, geometry, cursor, both streams and the over-text facts"
    )]
    #[inline]
    fn emit_mono_dark(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        (out, under): (&mut RampSink<'_>, &mut RampSink<'_>),
        (text, chosen): (TextView<'_>, bool),
        ledger: &mut InkLedger,
        ((feet, foot_ink, soles), bed_ground): (WakeFeet<'_>, Option<u32>),
    ) {
        let live = self.build_runs(now, cfg, geom, cur);
        if live == 0 {
            return;
        }
        let (ink, page) = CursorGlow::bed_pair(cfg);
        let budget = crate::cursor_glow::dark_bed_budget(ink);
        // A colour the user chose keeps its own chroma in the bed and the air's light, as
        // every other style's pinned bed does (`BedShape::pinned`), and its rail composes on
        // the page to it ([`Self::pinned_rail_ink`]): floored to the bed's saturation of 0.9
        // and the rail's of 1.0, Classic laid the pink pin `#d03a8c` (chroma 150) at 196, and
        // its rail read up to 18% more vivid than the pin (composed r3, O10(a)).
        #[cfg(any(test, debug_assertions))]
        let chosen = chosen && !self.mono_pin_floored;
        let shape = if chosen {
            Self::MONO_BED.pinned()
        } else {
            Self::MONO_BED
        };
        let air = crate::cursor_glow::AirLight::of(page, budget, shape);
        #[cfg(any(test, debug_assertions))]
        let tint_blind = self.mono_rail_tint_blind;
        #[cfg(not(any(test, debug_assertions)))]
        let tint_blind = false;
        let chf = geom.ch as f32;
        let lit_page = crate::cursor_glow::page_lit(page, budget);
        let shape = if lit_page { shape.on_lit_page() } else { shape };
        let mut scratch = std::mem::take(&mut self.bed_scratch);
        scratch.inks.clear();
        scratch.rails.clear();
        scratch.feet.clear();
        scratch.feet.extend_from_slice(feet);
        scratch.foot_ink.clear();
        scratch.foot_ink.extend_from_slice(foot_ink);
        scratch.soles = soles;
        scratch.rail_drop = if lit_page {
            crate::cursor_glow::lit_rail_drop(chf)
        } else {
            0.0
        };
        #[cfg(test)]
        let mono_lit_whole = self.mono_lit_whole;
        #[cfg(not(test))]
        let mono_lit_whole = false;
        #[cfg(test)]
        let one_piece = !self.tracer_up_kept;
        #[cfg(not(test))]
        let one_piece = true;
        #[cfg(test)]
        let anchored = !self.tracer_up_stretch;
        #[cfg(not(test))]
        let anchored = true;
        #[cfg(test)]
        let stairs = self.tracer_up_stairs;
        #[cfg(not(test))]
        let stairs = false;
        let up = |run: &[CometSample]| matches!((run.first(), run.last()), (Some(a), Some(b)) if a.y - b.y >= 0.5 * chf);
        // A run up a page across rows lays no bed: under the lines it rises
        // past, its stretches read as dashes on their letters, the lowest on
        // `[vim]` below the old caret (looks9 Q11). It keeps one stroke in
        // the air ([`one_air_piece`]).
        let mut held = std::mem::take(&mut self.held_runs);
        held.clear();
        if one_piece {
            let mut ri = 0;
            while ri < live {
                if up(&self.comet_runs[ri]) {
                    held.push((ri, std::mem::take(&mut self.comet_runs[ri])));
                }
                ri += 1;
            }
        }
        let mut last: Option<(u32, bool, u32, u32)> = None;
        let mut lit = std::mem::take(&mut scratch.air);
        let mut ri = 0;
        while ri < live {
            let run = &self.comet_runs[ri];
            ri += 1;
            crate::cursor_glow::air_lit(run, geom, &text, (air.is_some(), false), &mut lit);
            let n_samples = run.len().min(lit.len());
            let mut si = 0;
            while si < n_samples {
                let s = &run[si];
                let in_air = lit[si];
                si += 1;
                let rgb = self.comet_color(cfg, s.pos);
                let (bed_c, rail_c) = match last {
                    Some((prev, was_air, b, r)) if prev == rgb && was_air == in_air => (b, r),
                    _ => {
                        let (air_sat, bed_sat) = (shape.sat_for(rgb), shape.under_sat_for(rgb));
                        let b = match air {
                            Some(air) if in_air => air.ink(rgb, page, air_sat),
                            _ if lit_page && !mono_lit_whole => crate::effect_util::lerp_rgb(
                                bed_ground.unwrap_or(page) & 0x00FF_FFFF,
                                crate::cursor_glow::dark_bed_ink(rgb, page, budget, bed_sat),
                                Self::MONO_LIT_BED_SHARE,
                            ),
                            _ => crate::cursor_glow::dark_bed_ink(rgb, page, budget, bed_sat),
                        };
                        let r = if shape.pinned {
                            Self::pinned_rail_ink(rgb & 0x00FF_FFFF, page, tint_blind)
                        } else {
                            crate::rainbow_kitty::ribbon::rail_ink(rgb & 0x00FF_FFFF)
                        };
                        last = Some((rgb, in_air, b, r));
                        (b, r)
                    }
                };
                scratch.inks.push(bed_c);
                scratch.rails.push(rail_c);
            }
        }
        scratch.air = lit;
        lay_dark_bed(
            &self.comet_runs[..live],
            geom,
            page,
            shape,
            &text,
            &mut scratch,
            ledger,
            under,
        );
        self.bed_scratch = scratch;
        for (ri, run) in held.drain(..) {
            self.comet_runs[ri] = run;
        }
        self.held_runs = held;
        let caret = matches!(
            cur,
            Some((r, c)) if (r as usize) < geom.rows && (c as usize) < geom.cols
        );
        let core_thick = (chf * 0.13).max(2.0);
        let straighten = (geom.cw as f32).max(chf) * 0.8;
        // The pieces laid as they are, unstraightened: a stroke run whole
        // through the letters keeps its share at each of them only where
        // its points stand (straightened, a straight run up a page kept its
        // two ends and lit the rows between at their light).
        let mut exact = 0u128;
        let dim_page = crate::rainbow_kitty::ribbon::dim_theme(cfg.theme_fg, page);
        #[cfg(test)]
        let (up_unshaded, up_whole, up_lit_letters) = (
            self.tracer_up_unshaded,
            self.tracer_up_whole,
            self.tracer_up_lit_letters,
        );
        #[cfg(not(test))]
        let (up_unshaded, up_whole, up_lit_letters) = (false, false, false);
        let clip = geom.beam_clip();
        let (mono, color, accent, hue) = (cfg.classic_mono, cfg.color, cfg.accent, self.hue);
        let color_fn = |pos| comet_color(mono, color, accent, hue, pos);
        let n = if one_piece {
            let mut pieces = std::mem::take(&mut self.air_pieces);
            let mut shares = std::mem::take(&mut self.air_shares);
            let mut n = 0usize;
            let mut ri = 0;
            while ri < live {
                let run = &self.comet_runs[ri];
                let last_run = ri + 1 == live;
                ri += 1;
                if anchored && up(run) {
                    // A run up a page runs whole from the old caret to the
                    // new, through the lettered rows it rises past at a share
                    // of its light ([`through_letters`]): kept to its longest
                    // piece in the air it was a dash anchored to neither
                    // caret (`classic-pink/classic-dark-move-up.up`, looks10 L6).
                    // Its line is the one its straightened beam draws: cut
                    // finer through the swept cells' centres as they stand,
                    // it climbed in stairs, a flat in each row it crossed
                    // (`classic-pink/solarized-dark/classic-dark-move-up.up`, looks10
                    // round 1).
                    if n == self.air_runs.len() {
                        self.air_runs.push(Vec::new());
                    }
                    let line = if stairs {
                        run.as_slice()
                    } else {
                        self.up_line_scratch
                            .straighten_into(run, straighten, &mut self.up_line);
                        self.up_line.as_slice()
                    };
                    let piece = &mut self.air_runs[n];
                    dense_samples(line, piece);
                    let seen = |r: i32, c: i32| {
                        if !text.knows_row(r) {
                            Passed::Unread
                        } else if text.covers_cell(r, c) {
                            Passed::Letter
                        } else {
                            Passed::Air
                        }
                    };
                    let leave = chf * TRACER_LEAVE_SHARE;
                    let shade_too = dim_page && !up_unshaded;
                    through_letters(
                        piece.len(),
                        |i| (piece[i].x, piece[i].y),
                        seen,
                        geom,
                        (core_thick, leave, TRACER_UNDER_LETTERS),
                        &mut shares,
                        shade_too.then_some(&mut self.up_shade),
                    );
                    // A dim theme's letters take no light (each keeps 97% of
                    // its own reading), so the guard cut the run through them
                    // away and only its stretch in the blank row showed, a
                    // slash anchored to neither caret
                    // (`classic-pink/solarized-dark/classic-dark-move-up.up`,
                    // looks10 round 2's L6). There it is laid under the
                    // letters as the ribbons' bed is, its colour at the
                    // page's own luminance (`page_lit_ink`): a hue, neither
                    // light nor shade, which the letter guard holds to each
                    // letter it reaches by itself.
                    if shade_too {
                        // Laid at the tracer's own share of its light, the
                        // shade was a quarter of its hue (`#102735` on
                        // `#002b36`, 1.02-1.04:1, 9 to 16 levels): nothing at
                        // 1x, and what showed was the blank row's slash alone
                        // (looks10 round 2's judges). It is laid whole where
                        // the tracer is brightest, at its head, tapering as
                        // its light does toward the old caret, and kept from
                        // the letter guard's weighing of the lights in its
                        // cells, which cut it with them; the guard holds it
                        // to the letters it lies under by itself
                        // (`InkLedger::note_page_shade_laid`,
                        // `trail_letters_read`'s
                        // `a_one_colour_move_up_a_dim_page_is_one_stroke`).
                        #[cfg(any(test, debug_assertions))]
                        let faint = self.tracer_up_faint_shade;
                        #[cfg(not(any(test, debug_assertions)))]
                        let faint = false;
                        let peak = piece.iter().map(|p| p.cov).max().unwrap_or(0).max(1);
                        let gain = if faint { 1.0 } else { 255.0 / f32::from(peak) };
                        self.up_verts.clear();
                        for (i, p) in piece.iter().enumerate() {
                            let w = ((shares.shares[i] - self.up_shade[i])
                                * (1.0 / TRACER_UNDER_LETTERS))
                                .clamp(0.0, 1.0);
                            self.up_verts.push(BeamVertex {
                                x: p.x,
                                y: p.y,
                                color: crate::cursor_glow::page_lit_ink(color_fn(p.pos), page),
                                cov: (f32::from(p.cov) * gain * w).round().min(255.0) as u8,
                            });
                        }
                        let from = under.mark();
                        comet_beam_over(
                            under,
                            clip,
                            &self.up_verts,
                            core_thick * TRACER_UP_SHADE_THICK,
                            1,
                            0.0,
                        );
                        if !faint {
                            ledger.note_page_shade_laid(from, under.mark());
                        }
                        // The letters are the shade's: the light keeps none
                        // of itself over them. Its fifth there took no
                        // letter's light (each keeps 97% of its reading),
                        // and the guard, cutting every light over a cell to
                        // the one share its letter keeps, cut the shade with
                        // it to a twelfth, a trace of 5 bytes on the page
                        // (the gallery's `up`, looks10 round 2).
                        if !up_lit_letters {
                            shares.shares.copy_from_slice(&self.up_shade);
                        }
                    }
                    // A run that rises past letters keeps a share of its
                    // light in the air ([`TRACER_UP_AIR`]); one over blank
                    // rows alone keeps it whole.
                    let crosses = shares
                        .shares
                        .iter()
                        .any(|&k| k <= TRACER_UNDER_LETTERS + 1e-3);
                    for (p, k) in piece.iter_mut().zip(shares.shares.iter_mut()) {
                        if *k > TRACER_UNDER_LETTERS && crosses && !up_whole {
                            *k = TRACER_UNDER_LETTERS
                                + (*k - TRACER_UNDER_LETTERS)
                                    * ((TRACER_UP_AIR - TRACER_UNDER_LETTERS)
                                        / (1.0 - TRACER_UNDER_LETTERS));
                        }
                        p.cov = (f32::from(p.cov) * *k).round() as u8;
                    }
                    thin_exact(piece, &shares.shares);
                    if n < 128 {
                        exact |= 1 << n;
                    }
                    n += 1;
                    continue;
                }
                let k = split_air_runs(
                    std::slice::from_ref(run),
                    geom,
                    &text,
                    caret && last_run,
                    None,
                    &mut pieces,
                );
                let up = up(run);
                let stub = caret
                    && last_run
                    && matches!(pieces[..k].last().map(Vec::as_slice), Some([knee, _]) if knee.cov == 0);
                let keep = if up {
                    one_air_piece(&mut pieces, (0, k), stub, MONO_AIR_PULL_CORES * core_thick)
                } else {
                    k
                };
                let mut pi = 0;
                while pi < keep {
                    let piece = &pieces[pi];
                    pi += 1;
                    if n == self.air_runs.len() {
                        self.air_runs.push(Vec::new());
                    }
                    self.air_runs[n].clone_from(piece);
                    n += 1;
                }
            }
            self.air_pieces = pieces;
            self.air_shares = shares;
            n
        } else {
            split_air_runs(
                &self.comet_runs[..live],
                geom,
                &text,
                caret,
                None,
                &mut self.air_runs,
            )
        };
        let s = core_thick as i32;
        let caret_cell = cur.map(|(cr, cc)| CursorGlow::caret_box(geom, cr, cc));
        let mut i = 0;
        while i < n {
            let r = &self.air_runs[i];
            let stub = if caret && i + 1 == n && matches!(r.as_slice(), [knee, _] if knee.cov == 0)
            {
                caret_cell
            } else {
                None
            };
            i += 1;
            let from = out.mark();
            if r.len() < 2 {
                let s0 = r[0];
                push_rect(
                    out.quads(),
                    geom,
                    s0.x as i32 - s / 2,
                    s0.y as i32 - s / 2,
                    s,
                    s,
                    premul_rgb(color_fn(s0.pos), s0.cov),
                );
                continue;
            }
            let straight = if i <= 128 && exact & (1 << (i - 1)) != 0 {
                0.0
            } else {
                straighten
            };
            comet_glow_quads(out, clip, r, core_thick, straight, &color_fn);
            if let Some(cell) = stub {
                clip_stream_to(out, from, cell);
            }
        }
    }

    /// The light the one-colour rail's core lays over a dark page, of 255
    /// ([`Self::pinned_rail_ink`]): a fresh cell's bed opacity, which the rail's core rows wear
    /// ([`crate::cursor_glow::DARK_BED_ALPHA_CAP`]). At 128, the core read 0.37 of OKLCH chroma per
    /// lightness under the pink pin's 0.337 (10% more vivid) wherever the letter guard left it
    /// whole, as it does since it reads each letter by the ring round its strokes (round 27):
    /// cut to about half its light under the letters, as the guard cut every lettered cell's
    /// box, it had read near the pin.
    const PIN_RAIL_CORE: f32 = crate::cursor_glow::DARK_BED_ALPHA_CAP;

    /// The one-colour rail's ink for a colour the user chose, `rgb`, over the dark `page`: the
    /// pin's hue at full value, its saturation raised by the share the page's grey takes of the
    /// rail's core (`grey / PIN_RAIL_CORE`), so the rail composes on the page to the pin's own
    /// chroma at its lightness. Laid at the pin's saturation itself (`bed_rail_ink`'s pinned
    /// ink), the page diluted the rail to 0.28-0.31 of OKLCH chroma per lightness under the pink
    /// pin's 0.337; floored to full saturation, as unpinned, it read up to 0.40 (composed
    /// O10(a)). The grey is [`Self::page_grey_under`]'s: a page tinted in the pin's own hue
    /// lends the rail chroma where a grey one dilutes it, and raised by Solarized Dark's whole
    /// brightness a blue pin's rail reads 14-21% over the pin there (the after-land lows' W10b;
    /// `tint_blind`, a test's negative control, raises by the whole brightness again).
    #[inline]
    #[must_use]
    const fn pinned_rail_ink(rgb: u32, page: u32, tint_blind: bool) -> u32 {
        let (h, s, _) = crate::color_math::rgb2hsv(rgb);
        let grey = Self::page_grey_under(page, rgb, tint_blind);
        let raised = s * (1.0 + grey / Self::PIN_RAIL_CORE);
        crate::color_math::hsv2rgb(h, if raised < 1.0 { raised } else { 1.0 }, 1.0)
    }

    /// The grey `page` lays under a rail in `pin`'s hue, of 255: its brightest channel, less its
    /// chroma (that channel less its least) by the share of its OKLab tint that runs along the
    /// pin's hue (the cosine between the two, none where they part by a right angle or more).
    /// A neutral page, or a pin across the page's tint (Classic pink on Solarized Dark's teal),
    /// keeps the page's whole brightness; a blue pin on that teal, much less. `tint_blind`: the
    /// brightest channel alone.
    #[inline]
    #[must_use]
    const fn page_grey_under(page: u32, pin: u32, tint_blind: bool) -> f32 {
        let (r, g, b) = ((page >> 16) & 0xff, (page >> 8) & 0xff, page & 0xff);
        let hi = if r > g { r } else { g };
        let hi = (if hi > b { hi } else { b }) as f32;
        if tint_blind {
            return hi;
        }
        let lo = if r < g { r } else { g };
        let lo = (if lo < b { lo } else { b }) as f32;
        let [_, pa, pb] = crate::color_math::oklab(page);
        let [_, ia, ib] = crate::color_math::oklab(pin);
        let tint = crate::color_math::hypot_f32(pa, pb) * crate::color_math::hypot_f32(ia, ib);
        if tint <= 1e-9 {
            return hi;
        }
        let along = (pa * ia + pb * ib) / tint;
        hi - (hi - lo) * if along > 0.0 { along } else { 0.0 }
    }

    /// The one-colour face's bed on a dark page. Its rail lies in its own
    /// row where the row below is no air ([`BedShape::row_rail`]): laid under
    /// the row into a selection's fill, the frame cut it away, and in the
    /// gallery's `context` on Solarized Dark classic pink drew only its caret
    /// (looks10 round 2).
    const MONO_BED: BedShape = BedShape {
        reach: 0.10,
        thin: 0.45,
        sat: 0.90,
        full: 215.0,
        rail: 0.12,
        row_rail: true,
        ..BedShape::BASE
    };

    #[cfg(test)]
    const fn mono_unbedded(&self) -> bool {
        self.mono_unbedded
    }
    #[cfg(not(test))]
    #[inline(always)]
    const fn mono_unbedded(&self) -> bool {
        false
    }

    const LIGHT_STROKE_DROP: f32 = 0.30;
    const LIGHT_STROKE_THICK: f32 = 0.12;
    const LIGHT_STROKE_GAIN: f32 = 1.9;

    #[inline]
    fn emit_light_comet(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        under: &mut RampSink<'_>,
        (text, chosen): (TextView<'_>, bool),
    ) {
        #[cfg(test)]
        let kept = !self.tracer_over_letters;
        #[cfg(not(test))]
        let kept = true;
        let caret_row = cur.map(|(r, _)| i32::from(r));
        let live = self.build_runs(now, cfg, geom, cur);
        if live == 0 {
            return;
        }
        let mut dense = std::mem::take(&mut self.light_dense);
        let mut shares = std::mem::take(&mut self.air_shares);
        let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
        #[cfg(test)]
        let leave = self.tracer_leave_px.unwrap_or(chf * TRACER_LEAVE_SHARE);
        #[cfg(not(test))]
        let leave = chf * TRACER_LEAVE_SHARE;
        #[cfg(test)]
        let top_only = self.tracer_top_only;
        #[cfg(not(test))]
        let top_only = false;
        #[cfg(test)]
        let up_stretch = !self.tracer_up_kept;
        #[cfg(not(test))]
        let up_stretch = true;
        #[cfg(test)]
        let anchored = !self.tracer_up_stretch;
        #[cfg(not(test))]
        let anchored = true;
        let drop = chf * Self::LIGHT_STROKE_DROP;
        let thick = (chf * Self::LIGHT_STROKE_THICK).max(1.5);
        let clip = geom.beam_clip();
        let ground = LightGround::of(cfg);
        #[cfg(test)]
        let ground = LightGround {
            unsolved: self.light_unsolved,
            ..ground
        };
        let mut verts = std::mem::take(&mut self.light_verts);
        let mut last_ink: Option<(u32, u32)> = None;
        #[inline]
        const fn sample_ink_vertex(
            wake: &ClassicWake,
            (cfg, chosen): (&GlowConfig, bool),
            ground: &LightGround,
            last_ink: &mut Option<(u32, u32)>,
            drop: f32,
            s: &CometSample,
        ) -> BeamVertex {
            let raw = wake.paper_color(cfg, s.pos);
            let color = match *last_ink {
                Some((prev, c)) if prev == raw => c,
                _ => {
                    let c = if chosen {
                        ground.vivid_body_ink(raw)
                    } else {
                        ground.body_ink(raw)
                    };
                    *last_ink = Some((raw, c));
                    c
                }
            };
            BeamVertex {
                x: s.x,
                y: s.y + drop,
                color,
                cov: (s.cov as f32 * ClassicWake::LIGHT_STROKE_GAIN).min(LIGHT_BODY_ALPHA_CAP)
                    as u8,
            }
        }
        let mut ri = 0;
        while ri < live {
            if under.laid() >= Self::MAX_QUADS {
                break;
            }
            let run = &self.comet_runs[ri];
            ri += 1;
            verts.clear();
            match run.as_slice() {
                [] => continue,
                [lone] => {
                    let v =
                        sample_ink_vertex(self, (cfg, chosen), &ground, &mut last_ink, drop, lone);
                    let dx = cwf * 0.45;
                    verts.push(BeamVertex { x: v.x - dx, ..v });
                    verts.push(BeamVertex { x: v.x + dx, ..v });
                }
                [first, .., last] => {
                    let (ax, ay, bx, by) = (first.x, first.y, last.x, last.y);
                    let (dx, dy) = (bx - ax, by - ay);
                    let len2 = dx * dx + dy * dy;
                    let across_rows = ay != by && len2 > 0.0;
                    let inv_len2 = if across_rows { 1.0 / len2 } else { 0.0 };
                    let mut si = 0;
                    while si < run.len() {
                        let s = &run[si];
                        si += 1;
                        let mut v =
                            sample_ink_vertex(self, (cfg, chosen), &ground, &mut last_ink, drop, s);
                        if across_rows {
                            let t =
                                (((s.x - ax) * dx + (s.y - ay) * dy) * inv_len2).clamp(0.0, 1.0);
                            v.x = ax + dx * t;
                            v.y = ay + dy * t + drop;
                        }
                        step_over_paper_skip(&mut verts, v);
                        verts.push(v);
                    }
                    if across_rows && kept {
                        tracer_on_caret_row(
                            (&mut verts, &mut dense, &mut shares),
                            text,
                            geom,
                            caret_row,
                            (0.5 * thick, leave, top_only, up_stretch),
                            anchored,
                        );
                    }
                }
            }
            comet_beam_over(under, clip, &verts, thick, 1, 0.0);
        }
        self.light_verts = verts;
        self.light_dense = dense;
        self.air_shares = shares;
    }

    #[inline]
    fn crown_fade(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
    ) -> Option<((u16, u16), f32)> {
        if cfg.radius <= 0.0 {
            return None;
        }
        self.crown_until?;
        let (cr, cc) = cur?;
        if cr as usize >= geom.rows || cc as usize >= geom.cols {
            return None;
        }
        let age_ms = now.saturating_duration_since(self.last_move?).as_millis() as u64;
        let window = self.crown_window_ms.max(1);
        if age_ms < window {
            Some(((cr, cc), 1.0 - age_ms as f32 / window as f32))
        } else {
            None
        }
    }

    #[inline]
    fn emit_crown(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        out: &mut Vec<GlowQuad>,
    ) {
        let Some(((cr, cc), fade)) = self.crown_fade(now, cfg, geom, cur) else {
            return;
        };
        const K: i32 = 3;
        const BASE_COV: f32 = 50.0;
        let boost = self.typing_boost();
        let cov_scale = BASE_COV * (fade * cfg.intensity * boost / K as f32);
        if cov_scale * (K as f32) < 1.0 {
            return;
        }
        let cw = geom.cw as i32;
        let ch = geom.ch as i32;
        let cx = i32::from(geom.origin_x) + cc as i32 * cw;
        let cy = i32::from(geom.origin_y) + cr as i32 * ch;
        // The box blooms round the caret's body: the whole cell a block covers, and behind a bar
        // (`head_dx` 0.08) the bar, twice its insertion point across from the cell's left edge
        // (composed r1's judges, O4(a): a box sized to a block cell framed a 2 px bar). A block's
        // box is the cell's, byte for byte (the v0.28 restoration).
        #[cfg(test)]
        let barred = cfg.head_dx < 0.5 && !self.crown_cell_wide;
        #[cfg(not(test))]
        let barred = cfg.head_dx < 0.5;
        let bw = if barred {
            ((2.0 * cfg.head_dx * geom.cw as f32).round() as i32).clamp(2.min(cw), cw)
        } else {
            cw
        };
        let r = (cfg.radius * chf_of(geom)) as i32;
        let color = if cfg.classic_mono {
            cfg.color
        } else {
            hsv2rgb(self.hue, 0.9, 1.0)
        };
        let mut layer = 0;
        while layer < K {
            if out.len() >= Self::MAX_QUADS {
                return;
            }
            let grow = r * (K - layer) / K;
            let cov = ((layer as f32 + 1.0) * cov_scale).min(255.0) as u8;
            layer += 1;
            if cov == 0 {
                continue;
            }
            let two_grow = 2 * grow;
            push_rect(
                out,
                geom,
                cx - grow,
                cy - grow,
                bw + two_grow,
                ch + two_grow,
                premul_rgb(color, cov),
            );
        }
    }

    #[inline]
    fn emit_mono_crown(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        halos: &mut Vec<RainHalo>,
    ) {
        let Some(((cr, cc), fade)) = self.crown_fade(now, cfg, geom, cur) else {
            return;
        };
        let peak = (Self::MONO_CROWN_COV * fade * cfg.intensity * self.typing_boost()).min(210.0);
        if peak < 1.0 {
            return;
        }
        let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
        let r = cfg.radius * chf;
        let (cx, cy) = cfg.caret_center(&geom, cr, cc);
        push_halo_rows(
            halos,
            geom,
            (cx, cy),
            (cwf * 0.5 + r * 0.9, chf * 0.5 + r * 0.55),
            premul_rgb(cfg.color, peak as u8),
            HaloMode::Add,
        );
    }

    const MONO_CROWN_COV: f32 = 95.0;

    const LIGHT_CROWN_COV: f32 = 100.0 * Self::LIGHT_STROKE_GAIN;

    #[inline]
    fn emit_light_crown(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        under: &mut Vec<GlowQuad>,
        chosen: bool,
    ) {
        let Some(((cr, cc), fade)) = self.crown_fade(now, cfg, geom, cur) else {
            return;
        };
        let peak = (Self::LIGHT_CROWN_COV * fade * cfg.intensity * self.typing_boost())
            .min(LIGHT_BODY_ALPHA_CAP);
        if peak < 1.0 {
            return;
        }
        let color = if cfg.classic_mono {
            cfg.color
        } else {
            #[cfg(test)]
            let hue = if self.paper_muddy {
                self.hue
            } else {
                paper_crown_hue(self.hue)
            };
            #[cfg(not(test))]
            let hue = paper_crown_hue(self.hue);
            hsv2rgb(hue, 0.9, 1.0)
        };
        #[cfg(not(test))]
        let unsolved = false;
        #[cfg(test)]
        let unsolved = self.light_unsolved;
        let body_ink = if unsolved {
            color & 0x00FF_FFFF
        } else if chosen {
            LightGround::of(cfg).vivid_body_ink(color & 0x00FF_FFFF)
        } else {
            let key = WakeLightKey {
                rgb: color,
                fg: cfg.theme_fg,
                bg: cfg.theme_bg,
            };
            match self.crown_ink_memo {
                Some((k, v)) if k == key => v,
                _ => {
                    let v = LightGround::of(cfg).body_ink(color);
                    self.crown_ink_memo = Some((key, v));
                    v
                }
            }
        };
        let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
        let reach = cwf * 0.5 + cfg.radius * chf * 0.9;
        let budget = under.len().saturating_add(LIGHT_MARK_QUADS);
        push_ink_ellipse(
            under,
            geom,
            cfg.caret_center(&geom, cr, cc),
            (reach, reach),
            chf * 0.5,
            body_ink,
            peak,
            budget,
        );
    }

    #[inline]
    fn ring_now(&self, now: Instant, cfg: &GlowConfig) -> Option<(Ring, f32, u8)> {
        let ring = self.ring?;
        let age = now.saturating_duration_since(ring.born).as_secs_f32();
        let t = (age / ring.life).clamp(0.0, 1.0);
        let cov = (200.0 * (1.0 - t) * cfg.intensity) as u8;
        if cov > 0 { Some((ring, t, cov)) } else { None }
    }

    /// The one-colour landing ring laid into `out` and admitted at `floor`
    /// (the frame's ring: [`Self::emit_ring`]).
    #[inline]
    fn lay_mono_ring(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        out: &mut RampSink<'_>,
        ledger: &mut InkLedger,
        floor: f32,
    ) {
        if out.laid() < Self::MAX_QUADS {
            let from = out.quads().len();
            self.emit_ring(now, cfg, geom, out.quads());
            let to = out.quads().len();
            if to > from {
                ledger.admit_masked(out.quads(), from..to, floor);
            }
            #[cfg(test)]
            self.ring_seen.replace(
                out.quads()
                    .get(from..)
                    .map_or_else(Vec::new, <[GlowQuad]>::to_vec),
            );
            out.keep_laid(Self::MAX_QUADS);
        }
    }

    /// THE ONE-COLOUR RING IS WHOLE OR NOT DRAWN. Its square grows as the
    /// spectrum ring's does while all four bars keep inside its pane's grid
    /// and off every cell a line of text covers (the caret's own cell
    /// aside), holds the last size that does as it fades, and is not drawn
    /// when its first size does not; laid under the letters at the letter
    /// floor, a bar the floor would cut takes the ring with it. Cut where it
    /// crossed a letter or the grid's edge, an Enter's ring left a lone dim
    /// bar right of the caret on Solarized Dark and passed a bar through the
    /// `$` on dark (the trail gallery's classic `#d03a8c` `enter-bottom`,
    /// looks9 Q4). The verdict is the landing's ([`MonoRing`]): once drawn
    /// whole and then met by a later frame's text at the size it holds, it
    /// stays out until the next landing.
    #[inline]
    fn lay_whole_mono_ring(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        out: &mut RampSink<'_>,
        ink: &mut WakeInk<'_>,
    ) {
        #[cfg(test)]
        if self.ring_cut {
            self.lay_mono_ring(now, cfg, geom, out, ink.ledger, InkLedger::LETTER_FLOOR);
            return;
        }
        if out.laid() >= Self::MAX_QUADS {
            return;
        }
        let Some((ring, t, cov)) = self.ring_now(now, cfg) else {
            return;
        };
        #[cfg(test)]
        let unlatched = self.ring_unlatched;
        #[cfg(not(test))]
        let unlatched = false;
        #[cfg(test)]
        let pane_read = !self.ring_window_edge;
        #[cfg(not(test))]
        let pane_read = true;
        let held = match ring.mono {
            _ if unlatched => None,
            MonoRing::Out => return,
            MonoRing::Unjudged => None,
            MonoRing::Drawn(s) => Some(s),
        };
        // Not drawn: a drawn ring goes out for its landing, an undrawn one
        // waits to be drawn whole.
        let mut verdict = if held.is_some() {
            MonoRing::Out
        } else {
            MonoRing::Unjudged
        };
        if let Some(s) = Self::ring_reach(
            ring,
            t,
            geom,
            ink.text,
            RingLeast::Growth,
            (held, pane_read),
        ) {
            let from = out.quads().len();
            Self::ring_square_at(ring, s, premul_rgb(cfg.accent, cov), geom, out.quads());
            let to = out.quads().len();
            let laid = quads_area(&out.quads()[from..to]);
            ink.ledger
                .admit_masked(out.quads(), from..to, InkLedger::LETTER_FLOOR);
            let quads = out.quads();
            if keep_whole(quads, from, to, laid) {
                verdict = MonoRing::Drawn(s);
            }
            #[cfg(test)]
            self.ring_seen.replace(quads[from..].to_vec());
            out.keep_laid(Self::MAX_QUADS);
        } else {
            #[cfg(test)]
            self.ring_seen.replace(Vec::new());
        }
        if let Some(r) = &mut self.ring {
            r.mono = verdict;
        }
    }

    /// A whole landing ring's half side at `t` of its life, px: its growth
    /// ([`Self::ring_square`]'s) from the size it `held` (its `least`, on its
    /// first frame), held at the largest size whose four bars lie inside its
    /// pane's grid and off every cell a line of text covers but the caret's
    /// own; `None` when the size it starts from already leaves or meets them
    /// (the one-colour ring on a dark page from its first size,
    /// [`Self::lay_whole_mono_ring`], and every ring's rim on paper from the
    /// caret's row, [`Self::emit_light_ring_rim`]). A split's focused pane
    /// has its edge inside the window's grid, where the GUI clips the glow to
    /// the pane ([`crate::cursor_glow::TextView::past_pane`]): read by the
    /// window's edge alone, a ring on a pane's last row was laid across the
    /// divider and cut there to three bars (looks9's review). The effects
    /// box's head room over the window's first row stays the ring's on a pane
    /// at the window's top (`pane_read` false reads the window's edge alone,
    /// a test's control).
    #[inline]
    fn ring_reach(
        ring: Ring,
        t: f32,
        geom: Geom,
        text: TextView<'_>,
        least: RingLeast,
        (held, pane_read): (Option<i32>, bool),
    ) -> Option<i32> {
        let chf = chf_of(geom);
        let first = match least {
            RingLeast::Growth => (0.6 * chf) as i32,
            RingLeast::CaretRow => (0.5 * chf) as i32,
        };
        let want = ((0.6 + 1.0 * t) * chf) as i32;
        let th = (geom.ch as i32 / 8).max(2);
        let (cx, cy) = ring.center(geom);
        let (cx, cy) = (cx as i32, cy as i32);
        let (ox, oy) = (i32::from(geom.origin_x), i32::from(geom.origin_y));
        let (cw, ch) = (geom.cw as i32, geom.ch as i32);
        let (fx_l, fx_t, fx_r, fx_b) = (
            geom.fx_left(),
            geom.fx_top(),
            geom.fx_right(),
            geom.fx_bot(),
        );
        let (grid_rows, grid_cols) = (geom.rows as i32, geom.cols as i32);
        // The sizes whose four bars lie inside the effects box, in closed form.
        let max_s = (cx - fx_l).min(fx_r - cx).min(cy - fx_t).min(fx_b - cy);
        let min_s = (fx_l + th - cx)
            .max(fx_t + th - cy)
            .max(cx + th - fx_r)
            .max(cy + th - fx_b);
        let want = want.min(max_s);
        let own = (i32::from(ring.row), i32::from(ring.col));
        let bar_cells_clear = |(r0, r1): (i32, i32), (c0, c1): (i32, i32)| {
            let mut r = r0;
            while r <= r1 {
                if r == own.0 && own.1 >= c0 && own.1 <= c1 {
                    if text.covers_col_range(r, (c0, own.1 - 1))
                        || text.covers_col_range(r, (own.1 + 1, c1))
                    {
                        return false;
                    }
                } else if text.covers_col_range(r, (c0, c1)) {
                    return false;
                }
                if pane_read && r >= 0 && r < grid_rows {
                    let mut c = c0.max(0);
                    let c_end = c1.min(grid_cols - 1);
                    while c <= c_end {
                        if text.past_pane(r, c) {
                            return false;
                        }
                        c += 1;
                    }
                }
                r += 1;
            }
            true
        };
        let bounds_of = |s: i32| {
            let (x0, y0) = (cx - s, cy - s);
            let (rx0, ry0) = (cx + s - th, cy + s - th);
            let (x1, y1) = (cx + s - 1, cy + s - 1);
            (
                (x0 - ox).div_euclid(cw),
                (x0 + th - 1 - ox).div_euclid(cw),
                (rx0 - ox).div_euclid(cw),
                (x1 - ox).div_euclid(cw),
                (y0 - oy).div_euclid(ch),
                (y0 + th - 1 - oy).div_euclid(ch),
                (ry0 - oy).div_euclid(ch),
                (y1 - oy).div_euclid(ch),
            )
        };
        let bounds_clear = |b: (i32, i32, i32, i32, i32, i32, i32, i32)| {
            let (c_l0, c_l1, c_r0, c_r1, r_t0, r_t1, r_b0, r_b1) = b;
            bar_cells_clear((r_t0, r_t1), (c_l0, c_r1))
                && bar_cells_clear((r_b0, r_b1), (c_l0, c_r1))
                && bar_cells_clear((r_t1 + 1, r_b0 - 1), (c_l0, c_l1))
                && bar_cells_clear((r_t1 + 1, r_b0 - 1), (c_r0, c_r1))
        };
        let mut s = match held {
            Some(s) => s.max(first),
            None => first,
        };
        if s < min_s || s > max_s {
            return None;
        }
        let mut last_bounds = bounds_of(s);
        if !bounds_clear(last_bounds) {
            return None;
        }
        while s < want {
            let next_bounds = bounds_of(s + 1);
            if next_bounds != last_bounds && !bounds_clear(next_bounds) {
                break;
            }
            last_bounds = next_bounds;
            s += 1;
        }
        Some(s)
    }

    #[inline]
    fn emit_ring(&self, now: Instant, cfg: &GlowConfig, geom: Geom, out: &mut Vec<GlowQuad>) {
        let Some((ring, t, cov)) = self.ring_now(now, cfg) else {
            return;
        };
        Self::ring_square(ring, t, premul_rgb(cfg.accent, cov), geom, out);
    }

    /// The light ring's own mark: the dark ring's square, laid over the paper in the accent's
    /// ink under the over-text ceiling, as Water's light ripples are. The soft veil
    /// ([`Self::emit_light_ring`]), capped to keep the theme's text legible, barely tints the
    /// paper on its own.
    ///
    /// THE PAPER RING IS WHOLE AND OFF THE LETTERS, as the one-colour ring on a dark page is
    /// ([`Self::lay_whole_mono_ring`]): it grows while its four bars keep inside the grid and
    /// off every cell a line of text covers (the caret's own aside, [`Self::ring_reach`]), and
    /// where its first size already meets them it holds the caret's row, a square as tall as
    /// the row about the caret, or is not drawn when that meets them too. Grown over them, an
    /// Enter's box ran its left bar down the `t` of the printed line and the `$` and its top
    /// bar along that line, and the grid's edge cut its bottom bar away (the trail gallery's
    /// classic `enter-bottom` e120 on paper, looks10 L12); not drawn there, Ring on jumps
    /// showed nothing in the settings card, whose jump lands beside a line of text. Held to
    /// the caret's row it is a box as tall as the row and wider than the caret
    /// ([`Self::row_box_reach`]): a square as tall as the row hid its top and bottom bars
    /// under a block caret, and on e120 it read as two pale side bars hugging the caret
    /// (looks10 round 2's L12).
    #[inline]
    fn emit_light_ring_rim(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        text: TextView<'_>,
        out: &mut Vec<GlowQuad>,
    ) {
        let Some((ring, t, cov)) = self.ring_now(now, cfg) else {
            return;
        };
        #[cfg(any(test, debug_assertions))]
        let cut = self.ring_cut;
        #[cfg(not(any(test, debug_assertions)))]
        let cut = false;
        #[cfg(test)]
        let row_square = self.ring_row_square;
        #[cfg(not(test))]
        let row_square = false;
        let (hw, hh) = if cut {
            let s = ((0.6 + 1.0 * t) * chf_of(geom)) as i32;
            (s, s)
        } else {
            let Some(s) = Self::ring_reach(ring, t, geom, text, RingLeast::CaretRow, (None, true))
            else {
                return;
            };
            if s > (0.5 * chf_of(geom)) as i32 || row_square {
                (s, s)
            } else {
                (Self::row_box_reach(ring, t, geom, text, s), s)
            }
        };
        let from = out.len();
        Self::ring_rect_at(ring, (hw, hh), premul_rgb(0x00FF_FFFF, cov), geom, out);
        if out.len() > from {
            let ink = InkRole::OverText.ink(cfg.accent);
            reink_quads(&mut out[from..], LIGHT_INK_ALPHA_CAP as u8, |_| ink);
        }
    }

    /// A ring's square outline at `t` of its life, growing from 0.6 to 1.6 rows about its cell.
    #[inline]
    fn ring_square(ring: Ring, t: f32, premul: u32, geom: Geom, out: &mut Vec<GlowQuad>) {
        let s = ((0.6 + 1.0 * t) * chf_of(geom)) as i32;
        Self::ring_square_at(ring, s, premul, geom, out);
    }

    /// A ring's square outline of half side `s` px about its cell.
    #[inline]
    fn ring_square_at(ring: Ring, s: i32, premul: u32, geom: Geom, out: &mut Vec<GlowQuad>) {
        Self::ring_rect_at(ring, (s, s), premul, geom, out);
    }

    /// A ring's outline of half width `hw` and half height `hh` px about its cell.
    #[inline]
    fn ring_rect_at(
        ring: Ring,
        (hw, hh): (i32, i32),
        premul: u32,
        geom: Geom,
        out: &mut Vec<GlowQuad>,
    ) {
        let th = (geom.ch as i32 / 8).max(2);
        let (cx, cy) = ring.center(geom);
        let (cx, cy) = (cx as i32, cy as i32);
        let (x0, y0) = (cx - hw, cy - hh);
        push_rect(out, geom, x0, y0, 2 * hw, th, premul);
        push_rect(out, geom, x0, cy + hh - th, 2 * hw, th, premul);
        push_rect(out, geom, x0, y0, th, 2 * hh, premul);
        push_rect(out, geom, cx + hw - th, y0, th, 2 * hh, premul);
    }

    /// The half width of a paper rim held to the caret's row (half height
    /// `s`, [`Self::emit_light_ring_rim`]): it grows from `s` as the ring
    /// would, while its side bars keep inside the effect's bounds and
    /// [`Self::ROW_BOX_PAD`] px off every cell of the caret's row a line of
    /// text covers (the caret's own aside).
    #[inline]
    fn row_box_reach(ring: Ring, t: f32, geom: Geom, text: TextView<'_>, s: i32) -> i32 {
        let want = ((0.6 + 1.0 * t) * chf_of(geom)) as i32;
        let (cx, _) = ring.center(geom);
        let cx = cx as i32;
        let (ox, cw) = (i32::from(geom.origin_x), geom.cw as i32);
        let (fx_l, fx_r) = (geom.fx_left(), geom.fx_right());
        let (row, own) = (i32::from(ring.row), i32::from(ring.col));
        let clear = |x: i32| {
            let c = (x - ox).div_euclid(cw);
            x >= fx_l && x < fx_r && (c == own || !text.covers_cell(row, c))
        };
        let mut hw = s;
        while hw < want {
            let n = hw + 1;
            if !clear(cx - n - Self::ROW_BOX_PAD) || !clear(cx + n - 1 + Self::ROW_BOX_PAD) {
                break;
            }
            hw = n;
        }
        hw
    }

    /// How far a paper rim held to the caret's row keeps off a lettered
    /// cell, px ([`Self::row_box_reach`]).
    const ROW_BOX_PAD: i32 = 2;

    #[inline]
    fn emit_light_ring(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        halos: &mut Vec<RainHalo>,
    ) {
        let Some((ring, t, cov)) = self.ring_now(now, cfg) else {
            return;
        };
        let grow = (0.7 + 1.0 * t) * chf_of(geom);
        let veil = lerp_rgb(cfg.accent, 0x0000_0000, 0.28);
        let cap = (u32::from(cov) / 2).clamp(1, 120) as u8;
        #[cfg(not(test))]
        let unbound = false;
        #[cfg(test)]
        let unbound = self.veil_unbound;
        let on_grid = match cur {
            Some((r, c)) if usize::from(r) < geom.rows && usize::from(c) < geom.cols => {
                Some((r, c))
            }
            _ if usize::from(ring.caret.0) < geom.rows && usize::from(ring.caret.1) < geom.cols => {
                Some(ring.caret)
            }
            _ => None,
        };
        let (centre, radii, cap) = match on_grid {
            Some((r, c)) if !unbound => {
                let (cw, ch) = (geom.cw as f32, chf_of(geom));
                let key = WakeLightKey {
                    rgb: veil,
                    fg: cfg.theme_fg,
                    bg: cfg.theme_bg,
                };
                let veil_cap = match self.veil_cap_memo {
                    Some((k, v)) if k == key => v,
                    _ => {
                        let v = CursorGlow::light_veil_cap(veil, cfg.theme_fg, cfg.theme_bg);
                        self.veil_cap_memo = Some((key, v));
                        v
                    }
                };
                (
                    cfg.caret_center(&geom, r, c),
                    (
                        grow.min(CursorGlow::LIGHT_VEIL_REACH_CELLS * cw),
                        (grow * 0.85).min(0.5 * ch),
                    ),
                    cap.min(veil_cap),
                )
            }
            _ if unbound => (ring.center(geom), (grow, grow * 0.85), cap),
            _ => return,
        };
        push_soft_veil(halos, geom, centre, radii, veil, cap);
    }

    #[inline]
    const fn comet_color(&self, cfg: &GlowConfig, pos: f32) -> u32 {
        comet_color(cfg.classic_mono, cfg.color, cfg.accent, self.hue, pos)
    }

    /// The spectrum face's colour on paper: its walk skips the hues paper
    /// cannot show clean ([`crate::color_math::light_spectrum_hue`]), which
    /// its ink, darkened to the paper's bar, laid as olive and khaki (the
    /// `enter-bottom` Return's tracer, looks8 K10). The mono face keeps its
    /// own colours.
    #[inline]
    const fn paper_color(&self, cfg: &GlowConfig, pos: f32) -> u32 {
        #[cfg(test)]
        if self.paper_muddy {
            return self.comet_color(cfg, pos);
        }
        if cfg.classic_mono {
            self.comet_color(cfg, pos)
        } else {
            hsv2rgb(
                crate::color_math::light_spectrum_hue((self.hue + pos * 0.5) % 1.0),
                0.95,
                1.0,
            )
        }
    }
}

/// The spectrum face's crown on paper at `hue` (turns): a hue paper cannot
/// show clean ([`crate::color_math::LIGHT_MUDDY_HUES`]), whose ink darkened
/// to the paper's bar reads olive, held at the band's nearer edge, and every
/// other hue as it is. Remapped whole as its stroke's walk is
/// ([`crate::color_math::light_spectrum_hue`]), the crown under a landing's
/// ring took another colour at every hue, and in the settings card the ring
/// on paper all but vanished over it.
#[inline]
const fn paper_crown_hue(hue: f32) -> f32 {
    let (b0, b1) = crate::color_math::LIGHT_MUDDY_HUES;
    let h = hue - hue.floor();
    if h > b0 && h < b1 {
        if h - b0 <= b1 - h { b0 } else { b1 }
    } else {
        h
    }
}

/// Before `next` joins `verts`, a step where the paper spectrum jumps the
/// hues paper cannot show ([`crate::color_math::LIGHT_MUDDY_HUES`]): where
/// the last vertex lies on one side of the skip and `next` on the other,
/// two vertices at their midpoint, the last's colour then `next`'s, so the
/// stroke turns from orange to green at a hard edge and never blends
/// through the olive the skip leaves out (looks8 K10).
#[inline]
fn step_over_paper_skip(verts: &mut Vec<BeamVertex>, next: BeamVertex) {
    #[inline]
    const fn side(rgb: u32) -> u8 {
        let (b0, b1) = crate::color_math::LIGHT_MUDDY_HUES;
        let (h, s, _) = crate::color_math::rgb2hsv(rgb & 0x00FF_FFFF);
        if s < 0.3 {
            0
        } else if h <= b0 * 360.0 + 1.0 {
            1
        } else if h >= b1 * 360.0 - 1.0 && h < 180.0 {
            2
        } else {
            0
        }
    }
    let Some(&last) = verts.last() else {
        return;
    };
    if last.color == next.color {
        return;
    }
    let (a, b) = (side(last.color), side(next.color));
    if a == 0 || b == 0 || a == b {
        return;
    }
    let mid = BeamVertex {
        x: 0.5 * (last.x + next.x),
        y: 0.5 * (last.y + next.y),
        cov: ((u16::from(last.cov) + u16::from(next.cov)) / 2) as u8,
        color: last.color,
    };
    verts.push(mid);
    verts.push(BeamVertex {
        color: next.color,
        ..mid
    });
}

#[inline]
const fn chf_of(geom: Geom) -> f32 {
    geom.ch as f32
}

/// The least size a whole landing ring may take ([`ClassicWake::ring_reach`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RingLeast {
    /// Its growth's first size, 0.6 of a row about the caret's cell.
    Growth,
    /// A square as tall as the caret's row about the caret's cell.
    CaretRow,
}

/// The pixels `quads` cover, overlaps counted twice.
#[inline]
const fn quads_area(quads: &[GlowQuad]) -> u32 {
    let mut sum = 0u32;
    let mut i = 0;
    while i < quads.len() {
        sum += quads[i].w as u32 * quads[i].h as u32;
        i += 1;
    }
    sum
}

/// Keep a ring laid at `from..to` of `quads` whole or drop it: an admission
/// that cut it (its `laid` area shrank, a bar went, or a piece took another
/// colour) takes the ring with it. Returns whether it was kept.
#[inline]
fn keep_whole(quads: &mut Vec<GlowQuad>, from: usize, to: usize, laid: u32) -> bool {
    let kept = match quads.get(from..) {
        Some(slice) => quads_area(slice),
        None => 0,
    };
    let mut same_color = true;
    if from < quads.len() {
        let c0 = quads[from].color;
        let mut i = from + 1;
        while i < quads.len() {
            if quads[i].color != c0 {
                same_color = false;
                break;
            }
            i += 1;
        }
    }
    let whole = quads.len() == to && kept == laid && same_color;
    if !whole {
        quads.truncate(from);
    }
    whole
}

#[cfg(test)]
#[path = "classic_wake_tests.rs"]
mod tests;
