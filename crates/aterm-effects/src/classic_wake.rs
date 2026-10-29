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
    born: Instant,
    life: f32,
    caret: (u16, u16),
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
    air_shares: Vec<f32>,
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

/// The probed rows' feet, the caret's cell carved out, and their soles
/// ([`WakeInk::feet`]).
pub(crate) type WakeFeet<'a> = (&'a [WakeFoot], [Option<WakeFoot>; 2]);

/// Over-text state for the mono face's dark bed.
pub(crate) struct WakeInk<'a> {
    /// Probed and observed text spans for the frame.
    pub(crate) text: TextView<'a>,
    /// The frame's over-ink account.
    pub(crate) ledger: &'a mut InkLedger,
    /// The probed rows' feet, the caret's cell carved out, and their soles:
    /// where the one-colour bed's rail may lie in its own row
    /// ([`crate::cursor_glow::BedShape::row_rail`]).
    pub(crate) feet: WakeFeet<'a>,
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
    (verts, dense): (&mut Vec<BeamVertex>, &mut Vec<BeamVertex>),
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
            if Some(r) != caret_row {
                let y0 = oy + r as f32 * ch;
                let dy = (y0 - v.y).max(v.y - (y0 + ch)).max(0.0);
                let dy2 = dy * dy;
                if dy2 < reach2 {
                    let mut c = col - 1;
                    while c <= col + 1 {
                        let x0 = ox + c as f32 * cw;
                        let dx = (x0 - v.x).max(v.x - (x0 + cw)).max(0.0);
                        if dx * dx + dy2 < reach2
                            && (text.covers_cell(r, c) || text.covers_cell(r, c - 1))
                        {
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
        self::up_stretch(dense, text, geom, (half, leave), anchored);
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
    dense: &mut [BeamVertex],
    text: TextView<'_>,
    geom: Geom,
    (half, leave): (f32, f32),
    anchored: bool,
) {
    if anchored {
        let mut shares = Vec::with_capacity(dense.len());
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
            &mut shares,
        );
        for (v, k) in dense.iter_mut().zip(&shares) {
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

/// The share of its light each of a tracer's `n` points (`at(i)`, window
/// px, dense along its stroke, the last the caret's) keeps, into `shares`:
/// `under` ([`TRACER_UNDER_LETTERS`]) where its stroke, `half` wide, comes within
/// [`TRACER_CLEAR_PX`] of a cell `seen` names a [`Passed::Letter`] other than
/// the caret's, none where it comes within reach of a [`Passed::Unread`] one
/// (the letter guard cannot hold letters it has not read, so light there is
/// the over-ink budget's, as the dark streak's is: [`TextView::over_text`]),
/// whole `leave` px along the stroke from every such point, eased between.
fn through_letters(
    n: usize,
    at: impl Fn(usize) -> (f32, f32),
    seen: impl Fn(i32, i32) -> Passed,
    geom: Geom,
    (half, leave, under): (f32, f32, f32),
    shares: &mut Vec<f32>,
) {
    shares.clear();
    if n == 0 {
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
            for c in col - 1..=col + 1 {
                if (r, c) == caret {
                    continue;
                }
                let what = seen(r, c);
                if what == Passed::Air {
                    continue;
                }
                let (x0, y0) = (ox + c as f32 * cw, oy + r as f32 * ch);
                let dx = (x0 - x).max(x - (x0 + cw)).max(0.0);
                let dy = (y0 - y).max(y - (y0 + ch)).max(0.0);
                if dx * dx + dy * dy < reach2 {
                    letter |= what == Passed::Letter;
                    unread |= what == Passed::Unread;
                }
            }
        }
        (letter, unread)
    };
    let mut along = Vec::with_capacity(n);
    let mut walked = 0.0f32;
    for i in 0..n {
        if i > 0 {
            let (a, b) = (at(i - 1), at(i));
            walked += crate::color_math::hypot_f32(b.0 - a.0, b.1 - a.1);
        }
        along.push(walked);
    }
    let marks: Vec<(bool, bool)> = (0..n).map(|i| near(at(i))).collect();
    // Each point's distance along the stroke from the nearest marked point,
    // both ways.
    let gap = |marked: &dyn Fn(usize) -> bool| {
        let mut gap = vec![f32::INFINITY; n];
        let mut last: Option<f32> = None;
        for i in 0..n {
            if marked(i) {
                last = Some(along[i]);
            }
            if let Some(l) = last {
                gap[i] = along[i] - l;
            }
        }
        last = None;
        for i in (0..n).rev() {
            if marked(i) {
                last = Some(along[i]);
            }
            if let Some(l) = last {
                gap[i] = gap[i].min(l - along[i]);
            }
        }
        gap
    };
    let (to_letter, to_unread) = (gap(&|i| marks[i].0), gap(&|i| marks[i].1));
    let inv_leave = 1.0 / leave.max(1.0);
    let eased = |floor: f32, d: f32| {
        let u = (d * inv_leave).clamp(0.0, 1.0);
        floor + (1.0 - floor) * u * u * (3.0 - 2.0 * u)
    };
    shares.extend(
        to_letter
            .iter()
            .zip(&to_unread)
            .map(|(&dl, &du)| eased(under, dl).min(eased(0.0, du))),
    );
}

/// Drop the points of a dense stroke `piece` that change nothing laid
/// unstraightened: a point inside a stretch of one share (`shares`, the
/// piece's own, point for point) on a straight line between its
/// neighbours. Its light between the points it keeps is the line's.
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
    /// under their bar (`f` at 3.93:1 of its own 4.06, looks8 K10).
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
            air_shares: Vec::new(),
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
            #[cfg(test)]
            ring_row_square: false,
            #[cfg(test)]
            ring_seen: std::cell::RefCell::new(Vec::new()),
            #[cfg(test)]
            tracer_top_only: false,
            #[cfg(test)]
            paper_muddy: false,
            #[cfg(test)]
            mono_lit_whole: false,
            #[cfg(test)]
            tracer_up_kept: false,
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
                ink.text,
                ink.ledger,
                ink.feet,
            );
            out.keep_laid(Self::MAX_QUADS);
            under.keep_laid(Self::MAX_QUADS);
            let from = halos.len();
            self.emit_mono_crown(now, cfg, geom, cur, halos);
            ink.ledger
                .admit_halos_masked(halos, from, InkLedger::HEAD_FLOOR);
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
            self.emit_light_crown(now, cfg, geom, cur, under.quads());
            self.emit_light_comet(now, cfg, geom, cur, under, ink.text);
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
        let full_life = cfg.duration.as_secs_f32().max(0.001);
        let spark_life = if typing {
            let heat_life = (full_life * 0.38).clamp(0.05, 0.11) * (1.0 + 1.3 * self.heat);
            if gap <= Self::CHAIN_GAP_MAX {
                heat_life.max((gap + Self::CHAIN_MARGIN).min(Self::CHAIN_LIFE_MAX))
            } else {
                heat_life
            }
        } else {
            full_life
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
                born: now,
                life: 0.18,
                caret: (cr, cc),
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
        text: TextView<'_>,
        ledger: &mut InkLedger,
        (feet, soles): WakeFeet<'_>,
    ) {
        let live = self.build_runs(now, cfg, geom, cur);
        if live == 0 {
            return;
        }
        let (ink, page) = CursorGlow::bed_pair(cfg);
        let budget = crate::cursor_glow::dark_bed_budget(ink);
        let shape = Self::MONO_BED;
        let air = crate::cursor_glow::AirLight::of(page, budget, shape);
        let chf = geom.ch as f32;
        let lit_page = crate::cursor_glow::page_lit(page, budget);
        let shape = if lit_page { shape.on_lit_page() } else { shape };
        let mut scratch = std::mem::take(&mut self.bed_scratch);
        scratch.inks.clear();
        scratch.rails.clear();
        scratch.feet.clear();
        scratch.feet.extend_from_slice(feet);
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
                        let b = match air {
                            Some(air) if in_air => air.ink(rgb, page, shape.sat),
                            _ if lit_page && !mono_lit_whole => crate::effect_util::lerp_rgb(
                                page & 0x00FF_FFFF,
                                crate::cursor_glow::dark_bed_ink(rgb, page, budget, shape.sat),
                                Self::MONO_LIT_BED_SHARE,
                            ),
                            _ => crate::cursor_glow::dark_bed_ink(rgb, page, budget, shape.sat),
                        };
                        let r = crate::rainbow_kitty::ribbon::rail_ink(rgb & 0x00FF_FFFF);
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
                    through_letters(
                        piece.len(),
                        |i| (piece[i].x, piece[i].y),
                        seen,
                        geom,
                        (core_thick, leave, TRACER_UNDER_LETTERS),
                        &mut shares,
                    );
                    // A dim theme's letters take no light (each keeps 97% of
                    // its own reading), so the guard cut the run through them
                    // away and only its stretch in the blank row showed, a
                    // slash anchored to neither caret
                    // (`classic-pink/solarized-dark/classic-dark-move-up.up`,
                    // looks10 round 2's L6). There it is laid under the
                    // letters as the ribbons' bed is, its colour at the
                    // page's own luminance (`page_lit_ink`): a hue, neither
                    // light nor shade, that leaves every letter its reading.
                    if dim_page && !up_unshaded {
                        through_letters(
                            piece.len(),
                            |i| (piece[i].x, piece[i].y),
                            seen,
                            geom,
                            (core_thick, leave, 0.0),
                            &mut self.up_shade,
                        );
                        // Laid at the tracer's own share of its light, the
                        // shade was a quarter of its hue (`#102735` on
                        // `#002b36`, 1.02-1.04:1, 9 to 16 levels): nothing at
                        // 1x, and what showed was the blank row's slash alone
                        // (looks10 round 2's judges). It is laid whole where
                        // the tracer is brightest, at its head, tapering as
                        // its light does toward the old caret, and kept from
                        // the letter guard, which weighed it with the lights
                        // in its cells and cut it with them
                        // (`InkLedger::note_page_shade_laid`): a hue at the
                        // page's own luminance, it takes nothing from the
                        // letters it lies under (`trail_letters_read`'s
                        // `a_one_colour_move_up_a_dim_page_is_one_stroke`).
                        #[cfg(any(test, debug_assertions))]
                        let faint = self.tracer_up_faint_shade;
                        #[cfg(not(any(test, debug_assertions)))]
                        let faint = false;
                        let peak = piece.iter().map(|p| p.cov).max().unwrap_or(0).max(1);
                        let gain = if faint { 1.0 } else { 255.0 / f32::from(peak) };
                        self.up_verts.clear();
                        for (i, p) in piece.iter().enumerate() {
                            let w = ((shares[i] - self.up_shade[i]) * (1.0 / TRACER_UNDER_LETTERS))
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
                            shares.copy_from_slice(&self.up_shade);
                        }
                    }
                    // A run that rises past letters keeps a share of its
                    // light in the air ([`TRACER_UP_AIR`]); one over blank
                    // rows alone keeps it whole.
                    let crosses = shares.iter().any(|&k| k <= TRACER_UNDER_LETTERS + 1e-3);
                    for (p, k) in piece.iter_mut().zip(shares.iter_mut()) {
                        if *k > TRACER_UNDER_LETTERS && crosses && !up_whole {
                            *k = TRACER_UNDER_LETTERS
                                + (*k - TRACER_UNDER_LETTERS)
                                    * ((TRACER_UP_AIR - TRACER_UNDER_LETTERS)
                                        / (1.0 - TRACER_UNDER_LETTERS));
                        }
                        p.cov = (f32::from(p.cov) * *k).round() as u8;
                    }
                    thin_exact(piece, &shares);
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
        text: TextView<'_>,
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
            cfg: &GlowConfig,
            ground: &LightGround,
            last_ink: &mut Option<(u32, u32)>,
            drop: f32,
            s: &CometSample,
        ) -> BeamVertex {
            let raw = wake.paper_color(cfg, s.pos);
            let color = match *last_ink {
                Some((prev, c)) if prev == raw => c,
                _ => {
                    let c = ground.body_ink(raw);
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
                    let v = sample_ink_vertex(self, cfg, &ground, &mut last_ink, drop, lone);
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
                        let mut v = sample_ink_vertex(self, cfg, &ground, &mut last_ink, drop, s);
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
                            (&mut verts, &mut dense),
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
        let cw = geom.cw as i32;
        let ch = geom.ch as i32;
        let cx = i32::from(geom.origin_x) + cc as i32 * cw;
        let cy = i32::from(geom.origin_y) + cr as i32 * ch;
        let r = (cfg.radius * chf_of(geom)) as i32;
        let (k, base_cov, color) = (
            3i32,
            50.0f32,
            if cfg.classic_mono {
                cfg.color
            } else {
                hsv2rgb(self.hue, 0.9, 1.0)
            },
        );
        let boost = self.typing_boost();
        let cov_scale = base_cov * (fade * cfg.intensity * boost / k as f32);
        let mut layer = 0;
        while layer < k {
            if out.len() >= Self::MAX_QUADS {
                return;
            }
            let grow = r * (k - layer) / k;
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
                cw + two_grow,
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
        let (cx, cy) = geom.cell_center(cr, cc);
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
            geom.cell_center(cr, cc),
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
            ledger.admit_masked(out.quads(), from..to, floor);
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
    /// spectrum ring's does while all four bars keep inside the grid and off
    /// every cell a line of text covers (the caret's own cell aside), holds
    /// the last size that does as it fades, and is not drawn when its first
    /// size does not; laid under the letters at the letter floor, a bar the
    /// floor would cut takes the ring with it. Cut where it crossed a letter
    /// or the grid's edge, an Enter's ring left a lone dim bar right of the
    /// caret on Solarized Dark and passed a bar through the `$` on dark (the
    /// trail gallery's classic `#d03a8c` `enter-bottom`, looks9 Q4).
    #[inline]
    fn lay_whole_mono_ring(
        &self,
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
        let Some(s) = Self::ring_reach(ring, t, geom, ink.text, RingLeast::Growth) else {
            return;
        };
        let from = out.quads().len();
        Self::ring_square_at(ring, s, premul_rgb(cfg.accent, cov), geom, out.quads());
        let to = out.quads().len();
        let laid = quads_area(&out.quads()[from..to]);
        ink.ledger
            .admit_masked(out.quads(), from..to, InkLedger::LETTER_FLOOR);
        let quads = out.quads();
        keep_whole(quads, from, to, laid);
        #[cfg(test)]
        self.ring_seen.replace(quads[from..].to_vec());
        out.keep_laid(Self::MAX_QUADS);
    }

    /// A whole landing ring's half side at `t` of its life, px: its growth
    /// ([`Self::ring_square`]'s) held at the largest size whose four bars lie
    /// inside the grid and off every cell a line of text covers but the
    /// caret's own; `None` when its `least` size already leaves or meets them
    /// (the one-colour ring on a dark page from its first size,
    /// [`Self::lay_whole_mono_ring`], and every ring's rim on paper from the
    /// caret's row, [`Self::emit_light_ring_rim`]).
    #[inline]
    fn ring_reach(
        ring: Ring,
        t: f32,
        geom: Geom,
        text: TextView<'_>,
        least: RingLeast,
    ) -> Option<i32> {
        let chf = chf_of(geom);
        let first = match least {
            RingLeast::Growth => (0.6 * chf) as i32,
            RingLeast::CaretRow => (0.5 * chf) as i32,
        };
        let want = ((0.6 + 1.0 * t) * chf) as i32;
        let th = (geom.ch as i32 / 8).max(2);
        let (cx, cy) = geom.cell_center(ring.row, ring.col);
        let (cx, cy) = (cx as i32, cy as i32);
        let (ox, oy) = (i32::from(geom.origin_x), i32::from(geom.origin_y));
        let (cw, ch) = (geom.cw as i32, geom.ch as i32);
        let (fx_l, fx_t, fx_r, fx_b) = (
            geom.fx_left(),
            geom.fx_top(),
            geom.fx_right(),
            geom.fx_bot(),
        );
        let max_s = (cx - fx_l).min(fx_r - cx).min(cy - fx_t).min(fx_b - cy);
        let min_s = (fx_l + th - cx)
            .max(fx_t + th - cy)
            .max(cx + th - fx_r)
            .max(cy + th - fx_b);
        if first < min_s || first > max_s {
            return None;
        }
        let want = want.min(max_s);
        let own = (i32::from(ring.row), i32::from(ring.col));
        let bar_cells_clear = |(r0, r1): (i32, i32), (c0, c1): (i32, i32)| {
            let mut r = r0;
            while r <= r1 {
                let mut c = c0;
                while c <= c1 {
                    if (r, c) != own && text.covers_cell(r, c) {
                        return false;
                    }
                    c += 1;
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
        let mut last_bounds = bounds_of(first);
        if !bounds_clear(last_bounds) {
            return None;
        }
        let mut s = first;
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
            let Some(s) = Self::ring_reach(ring, t, geom, text, RingLeast::CaretRow) else {
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
        let (cx, cy) = geom.cell_center(ring.row, ring.col);
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
        let (cx, _) = geom.cell_center(ring.row, ring.col);
        let cx = cx as i32;
        let (ox, cw) = (i32::from(geom.origin_x), geom.cw as i32);
        let (row, own) = (i32::from(ring.row), i32::from(ring.col));
        let clear = |x: i32| {
            let c = (x - ox).div_euclid(cw);
            x >= geom.fx_left() && x < geom.fx_right() && (c == own || !text.covers_cell(row, c))
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
                    geom.cell_center(r, c),
                    (
                        grow.min(CursorGlow::LIGHT_VEIL_REACH_CELLS * cw),
                        (grow * 0.85).min(0.5 * ch),
                    ),
                    cap.min(veil_cap),
                )
            }
            _ if unbound => (
                geom.cell_center(ring.row, ring.col),
                (grow, grow * 0.85),
                cap,
            ),
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
/// colour) takes the ring with it.
#[inline]
fn keep_whole(quads: &mut Vec<GlowQuad>, from: usize, to: usize, laid: u32) {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color_math::relative_luminance;
    use crate::cursor_glow::GlowStyle;
    use aterm_render::{HaloMode, halo_over_cap, over_premul};

    const fn geom() -> Geom {
        Geom {
            cw: 8,
            ch: 16,
            rows: 6,
            cols: 40,
            origin_x: 0,
            origin_y: 0,
            win_w: 320,
            win_h: 96,
            head: 0,
        }
    }

    fn cfg() -> GlowConfig {
        GlowConfig {
            style: GlowStyle::Classic,
            accent: 0x0078_FFB9,
            duration: Duration::from_millis(240),
            length: 18,
            intensity: 0.7,
            audible: true,
            radius: 0.6,
            ring: true,
            theme_fg: 0x00C8_C8D2,
            theme_bg: 0x0010_1016,
            beam: true,
            ribbon_tall: false,
            ..Default::default()
        }
    }

    const PAPER: u32 = 0x00FF_FFFF;
    const INK: u32 = 0x001F_2328;

    fn light_cfg() -> GlowConfig {
        GlowConfig {
            dark_theme: false,
            theme_fg: INK,
            theme_bg: PAPER,
            color: 0x0009_69DA,
            accent: 0x0054_AEFF,
            ..cfg()
        }
    }

    #[derive(Default)]
    struct Rig {
        w: ClassicWake,
        out: Vec<GlowQuad>,
        out_ramps: aterm_render::GlowRamps,
        halos: Vec<RainHalo>,
        under: Vec<GlowQuad>,
        under_ramps: aterm_render::GlowRamps,
        ledger: InkLedger,
        spans: crate::cursor_glow::TextSpans,
    }

    impl Rig {
        /// One tick, its ramps laid as the quads they stand for.
        fn tick(&mut self, cur: Option<(u16, u16)>, now: Instant, c: &GlowConfig, g: Geom) -> u64 {
            self.w.tick(
                cur,
                now,
                c,
                g,
                &mut crate::cursor_glow::glow_sink(&mut self.out, &mut self.out_ramps, true),
                &mut self.halos,
                &mut crate::cursor_glow::glow_sink(&mut self.under, &mut self.under_ramps, true),
                WakeInk {
                    text: crate::cursor_glow::TextView::new(self.spans, None),
                    ledger: &mut self.ledger,
                    feet: (&[], [None; 2]),
                },
            )
        }
    }

    fn ms(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    fn assert_banded(out: &[GlowQuad], g: Geom) {
        for q in out {
            assert!(
                i32::from(q.x) >= g.fx_left() && i32::from(q.x) + i32::from(q.w) <= g.fx_right(),
                "quad escapes the effects box horizontally: {q:?}"
            );
            assert!(
                i32::from(q.y) >= g.fx_top() && i32::from(q.y) + i32::from(q.h) <= g.fx_bot(),
                "quad escapes the effects box vertically: {q:?}"
            );
            let band = i32::from(q.y) - i32::from(g.origin_y);
            let row = band.div_euclid(g.ch as i32);
            assert_eq!(
                row,
                i32::from(q.row),
                "quad's row tag disagrees with its pixels: {q:?}"
            );
            assert!(
                (band + i32::from(q.h) - 1).div_euclid(g.ch as i32) == row,
                "quad spans two cell rows: {q:?}"
            );
            assert_eq!(
                q.alpha, 0,
                "on a dark ground the classic wake is ADDITIVE light only"
            );
        }
    }

    /// The one-colour landing ring's quads each frame for 300 ms after a jump
    /// from `(1, 30)` to `land`, the text `lines` probed, `cut` laying the ring
    /// as it was ([`ClassicWake::ring_cut`]).
    fn mono_ring_frames(
        land: (u16, u16),
        lines: &[(i32, i32, i32)],
        cut: bool,
    ) -> Vec<Vec<GlowQuad>> {
        let c = GlowConfig {
            classic_mono: true,
            color: 0x00D0_3A8C,
            accent: 0x00D0_3A8C,
            ..cfg()
        };
        ring_frames(&c, land, lines, (cut, false))
    }

    /// [`mono_ring_frames`] in the config `c`: on paper, the ring's rim.
    fn ring_frames(
        c: &GlowConfig,
        land: (u16, u16),
        lines: &[(i32, i32, i32)],
        (cut, row_square): (bool, bool),
    ) -> Vec<Vec<GlowQuad>> {
        let (g, c) = (geom(), *c);
        let mut rig = Rig {
            spans: crate::cursor_glow::TextSpans::of_lines(lines),
            ..Rig::default()
        };
        rig.w.ring_cut = cut;
        rig.w.ring_row_square = row_square;
        let t0 = Instant::now();
        rig.tick(Some((1, 30)), t0, &c, g);
        let mut frames = Vec::new();
        for k in 1..=40u64 {
            rig.out.clear();
            rig.under.clear();
            rig.halos.clear();
            rig.tick(Some(land), ms(t0, 8 * k), &c, g);
            frames.push(rig.w.ring_seen.take());
        }
        frames
    }

    /// Whether `quads` are a whole square outline (the four bars of one square,
    /// every pixel of its rim lit), and whether any lies on a cell `lines`
    /// covers but `own`.
    fn ring_reading(
        quads: &[GlowQuad],
        lines: &[(i32, i32, i32)],
        own: (u16, u16),
    ) -> (bool, bool) {
        let (whole, on_text, (x0, x1, y0, y1)) = rim_reading(quads, lines, own);
        (whole && (x1 - x0) == (y1 - y0), on_text)
    }

    /// Whether `quads` are a whole rectangular outline (every pixel of its rim
    /// lit), whether any lies on a cell `lines` covers but `own`, and its
    /// bounds `(x0, x1, y0, y1)`, inclusive.
    fn rim_reading(
        quads: &[GlowQuad],
        lines: &[(i32, i32, i32)],
        own: (u16, u16),
    ) -> (bool, bool, (i32, i32, i32, i32)) {
        let g = geom();
        let px: std::collections::BTreeSet<(i32, i32)> = quads
            .iter()
            .flat_map(|q| {
                (i32::from(q.y)..i32::from(q.y) + i32::from(q.h)).flat_map(move |y| {
                    (i32::from(q.x)..i32::from(q.x) + i32::from(q.w)).map(move |x| (x, y))
                })
            })
            .collect();
        let on_text = px.iter().any(|&(x, y)| {
            let (r, c) = (y.div_euclid(g.ch as i32), x.div_euclid(g.cw as i32));
            (r, c) != (i32::from(own.0), i32::from(own.1))
                && lines
                    .iter()
                    .any(|&(lr, a, b)| lr == r && (a..b).contains(&c))
        });
        let (x0, x1) = (
            px.iter().map(|p| p.0).min().unwrap_or(0),
            px.iter().map(|p| p.0).max().unwrap_or(0),
        );
        let (y0, y1) = (
            px.iter().map(|p| p.1).min().unwrap_or(0),
            px.iter().map(|p| p.1).max().unwrap_or(0),
        );
        let rim = (x0..=x1).all(|x| px.contains(&(x, y0)) && px.contains(&(x, y1)))
            && (y0..=y1).all(|y| px.contains(&(x0, y)) && px.contains(&(x1, y)));
        (rim, on_text, (x0, x1, y0, y1))
    }

    /// looks9 Q4, THE ONE-COLOUR RING IS WHOLE OR NOT DRAWN: on a dark page the
    /// one-colour face's landing ring grows while its four bars keep inside
    /// the grid and off the text's cells, holds that size as it fades, and is
    /// not drawn where its first size meets them. Cut where it crossed the
    /// grid's edge and the letters, an Enter's ring left a lone dim bar right
    /// of the caret on Solarized Dark and passed a bar through the `$` on dark
    /// (the trail gallery's classic `#d03a8c` `enter-bottom` e120). Negative
    /// control: the ring laid as it was ([`ClassicWake::ring_cut`]) breaks.
    #[test]
    fn the_one_colour_ring_is_whole_or_not_drawn() {
        // A landing beside a `$` with a line above it clear of the ring's first
        // size, and a landing on the grid's last row under a printed line.
        let mid: (u16, u16) = (2, 2);
        let mid_lines = [(2, 0, 1), (1, 10, 17)];
        let bottom: (u16, u16) = (5, 2);
        let bottom_lines = [(5, 0, 1), (4, 0, 19)];
        for (land, lines, wants_ring) in [(mid, &mid_lines, true), (bottom, &bottom_lines, false)] {
            let frames = mono_ring_frames(land, lines, false);
            let drawn = frames.iter().filter(|q| !q.is_empty()).count();
            for (k, quads) in frames.iter().enumerate().filter(|(_, q)| !q.is_empty()) {
                let (whole, on_text) = ring_reading(quads, lines, land);
                assert!(
                    whole,
                    "landing {land:?}, frame {k}: the ring is not whole: {quads:?}"
                );
                assert!(
                    !on_text,
                    "landing {land:?}, frame {k}: a bar lies on the text"
                );
            }
            if wants_ring {
                assert!(
                    drawn >= 10,
                    "landing {land:?}: the ring shows in {drawn} frames"
                );
            } else {
                assert_eq!(
                    drawn, 0,
                    "landing {land:?}: a ring that cannot be whole shows"
                );
            }
            let broken = mono_ring_frames(land, lines, true)
                .iter()
                .filter(|q| !q.is_empty())
                .filter(|q| {
                    let (whole, on_text) = ring_reading(q, lines, land);
                    !whole || on_text
                })
                .count();
            assert!(
                broken > 0,
                "control: landing {land:?}: the ring as it was stays whole and off the text"
            );
        }
    }

    /// looks10 L12, THE PAPER RING IS WHOLE AND OFF THE LETTERS: on paper the
    /// landing ring's rim (the spectrum face's and the one-colour face's) grows
    /// while its four bars keep inside the grid and off the text's cells; where
    /// its first size meets them (an Enter's landing on the last row under a
    /// printed line) it holds the caret's row, and where that meets them too (a
    /// landing inside a word) it is not drawn. Grown over them, an Enter's box
    /// ran its left bar down the `t` of the printed line and the `$` and its
    /// top bar along that line (the trail gallery's classic `enter-bottom` e120
    /// on GitHub Light and Default Light). Negative control: the rim laid as it
    /// was ([`ClassicWake::ring_cut`]) lies on the text. Held to the caret's row,
    /// it is a box whose top and bottom bars run past the caret's cell on both
    /// sides, where a block caret leaves them seen (looks10 round 2's L12: as a
    /// square as tall as the row it read as two side bars). Negative control:
    /// held to the row as a square ([`ClassicWake::ring_row_square`]), its bars
    /// barely pass the caret's cell.
    #[test]
    fn the_paper_ring_is_whole_and_off_the_letters() {
        let mid: (u16, u16) = (2, 2);
        let mid_lines = [(2, 0, 1), (1, 10, 17)];
        let bottom: (u16, u16) = (5, 2);
        let bottom_lines = [(5, 0, 1), (4, 0, 19)];
        let inside: (u16, u16) = (3, 6);
        let inside_lines = [(3, 0, 14)];
        let pink = GlowConfig {
            classic_mono: true,
            color: 0x00D0_3A8C,
            accent: 0x00D0_3A8C,
            ..light_cfg()
        };
        for c in [light_cfg(), pink] {
            let mono = c.classic_mono;
            for (land, lines, wants_ring) in [
                (mid, &mid_lines[..], true),
                (bottom, &bottom_lines[..], true),
                (inside, &inside_lines[..], false),
            ] {
                let frames = ring_frames(&c, land, lines, (false, false));
                let drawn = frames.iter().filter(|q| !q.is_empty()).count();
                for (k, quads) in frames.iter().enumerate().filter(|(_, q)| !q.is_empty()) {
                    let (whole, on_text, _) = rim_reading(quads, lines, land);
                    assert!(
                        whole && !on_text,
                        "mono {mono}, landing {land:?}, frame {k}: whole {whole}, on the \
                         text {on_text}: {quads:?}"
                    );
                }
                if wants_ring {
                    assert!(
                        drawn >= 10,
                        "mono {mono}, landing {land:?}: the ring shows in {drawn} frames"
                    );
                } else {
                    assert_eq!(
                        drawn, 0,
                        "mono {mono}, landing {land:?}: a ring that cannot be whole shows"
                    );
                }
            }
            let on_text = ring_frames(&c, bottom, &bottom_lines, (true, false))
                .iter()
                .filter(|q| !q.is_empty() && ring_reading(q, &bottom_lines, bottom).1)
                .count();
            assert!(
                on_text > 0,
                "control: mono {mono}: the Enter's rim as it was keeps off the text"
            );
            // The row's box: its least reach past the caret's cell on either side.
            let g = geom();
            let th = (g.ch as i32 / 8).max(2);
            let (cell_x0, cell_x1) = (
                i32::from(bottom.1) * g.cw as i32,
                (i32::from(bottom.1) + 1) * g.cw as i32 - 1,
            );
            let past = |row_square: bool| {
                ring_frames(&c, bottom, &bottom_lines, (false, row_square))
                    .iter()
                    .filter(|q| !q.is_empty())
                    .map(|q| {
                        let (_, _, (x0, x1, _, _)) = rim_reading(q, &bottom_lines, bottom);
                        (cell_x0 - x0).min(x1 - cell_x1)
                    })
                    .max()
                    .unwrap_or(0)
            };
            let reach = past(false);
            assert!(
                reach > 2 * th,
                "mono {mono}: the Enter's box runs only {reach} px past the caret's cell"
            );
            let square = past(true);
            assert!(
                square <= 2 * th,
                "control: mono {mono}: as a square its bars run {square} px past the caret"
            );
        }
    }

    #[test]
    fn the_comet_run_pool_keeps_its_buffers_between_frames() {
        let (g, c) = (geom(), cfg());
        let mut rig = Rig::default();
        let t0 = Instant::now();
        let frame = |rig: &mut Rig, k: u64| {
            let col = u16::try_from((k % 30) + 2).expect("column fits");
            let row = if k.is_multiple_of(7) { 4 } else { 2 };
            rig.tick(Some((row, col)), ms(t0, k * 8), &c, g);
        };
        for k in 0..120 {
            frame(&mut rig, k);
        }
        let pooled = |w: &ClassicWake| -> usize { w.comet_runs.iter().map(Vec::capacity).sum() };
        let warm = pooled(&rig.w);
        assert!(
            warm > 0,
            "fixture: the builder must have built something to pool"
        );
        assert!(
            !rig.w.comet_runs.is_empty(),
            "fixture: the pool holds at least one run buffer"
        );
        let mut slots_low = usize::MAX;
        let mut slots_high = 0usize;
        let mut cap_low = usize::MAX;
        for k in 120..600 {
            frame(&mut rig, k);
            slots_low = slots_low.min(rig.w.comet_runs.len());
            slots_high = slots_high.max(rig.w.comet_runs.len());
            cap_low = cap_low.min(pooled(&rig.w));
        }
        assert!(
            slots_high > 1,
            "fixture: the gesture must need more than one run, or the pool's own law is untested ({slots_high} high)"
        );
        assert_eq!(
            slots_low, slots_high,
            "the run pool SHRANK ({slots_low} low, {slots_high} high): its buffers are freed and rebuilt every redraw"
        );
        assert!(
            cap_low >= warm,
            "the run pool lost capacity between frames ({warm} warm, {cap_low} low)"
        );
    }

    #[test]
    fn the_classic_wake_spawns_on_motion_alone_with_no_license() {
        let (g, c) = (geom(), cfg());
        let mut rig = Rig::default();
        let t0 = Instant::now();
        rig.tick(Some((2, 4)), t0, &c, g);
        let fp = rig.tick(Some((4, 34)), ms(t0, 16), &c, g);
        assert!(!rig.out.is_empty(), "a licence-free jump must still light");
        assert_ne!(fp, 0, "a lit frame fingerprints non-zero");
        assert_banded(&rig.out, g);
    }

    #[test]
    fn a_wide_glyph_is_typing_and_a_longer_step_is_a_jump() {
        let (g, c) = (geom(), cfg());
        let t0 = Instant::now();
        let mut rig = Rig::default();
        rig.tick(Some((2, 4)), t0, &c, g);
        rig.tick(Some((2, 6)), ms(t0, 60), &c, g);
        rig.tick(Some((2, 8)), ms(t0, 120), &c, g);
        assert!(rig.w.heat > 0.0, "two fast wide glyphs earn typing heat");
        assert!(rig.w.ring.is_none(), "a wide glyph lays no landing ring");
        assert_eq!(rig.w.crown_window_ms, ClassicWake::CROWN_TYPING_MS);
        rig.tick(Some((2, 6)), ms(t0, 180), &c, g);
        assert!(rig.w.ring.is_none(), "erasing a wide glyph lays no ring");
        let heat = rig.w.heat;
        rig.tick(Some((3, 6)), ms(t0, 240), &c, g);
        assert!(rig.w.ring.is_none(), "a one-row step lays no ring");
        assert!(rig.w.heat > heat, "a one-row step is typing and earns heat");

        let mut jump = Rig::default();
        jump.tick(Some((2, 4)), t0, &c, g);
        jump.tick(Some((2, 7)), ms(t0, 60), &c, g);
        assert!(jump.w.ring.is_some(), "a three-column step is a jump");
        assert_eq!(jump.w.crown_window_ms, ClassicWake::CROWN_MS);
        assert_eq!(jump.w.heat, 0.0, "a jump earns no typing heat");
    }

    #[test]
    fn the_comet_ends_on_a_bar_caret() {
        let g = geom();
        let t0 = Instant::now();
        let centroid = |head_dx: f32| -> f32 {
            let c = GlowConfig {
                head_dx,
                radius: 0.0,
                ring: false,
                ..cfg()
            };
            let mut rig = Rig::default();
            rig.tick(Some((1, 5)), t0, &c, g);
            rig.tick(Some((5, 5)), ms(t0, 16), &c, g);
            assert!(!rig.out.is_empty(), "fixture: the jump lit");
            let (mut sum, mut weight) = (0.0f64, 0.0f64);
            for q in &rig.out {
                let lum = f64::from(
                    ((q.color >> 16) & 0xff) + ((q.color >> 8) & 0xff) + (q.color & 0xff),
                );
                let wgt = lum * f64::from(q.w) * f64::from(q.h);
                sum += wgt * (f64::from(q.x) + f64::from(q.w) / 2.0);
                weight += wgt;
            }
            (sum / weight) as f32
        };
        let cw = g.cw as f32;
        for head_dx in [0.08f32, 0.5] {
            let want = (5.0 + head_dx) * cw;
            let got = centroid(head_dx);
            assert!(
                (got - want).abs() <= 1.0,
                "head_dx {head_dx}: the streak runs at x {got}, not on the caret at {want}"
            );
        }
    }

    #[test]
    fn the_landing_ring_stroke_scales_with_the_cell() {
        let t0 = Instant::now();
        for (ch, th) in [(16usize, 2u16), (23, 2), (32, 4)] {
            let g = Geom {
                ch,
                win_h: (6 * ch) as u16,
                ..geom()
            };
            let c = GlowConfig {
                radius: 0.0,
                ..cfg()
            };
            let mut rig = Rig::default();
            rig.tick(Some((1, 4)), t0, &c, g);
            rig.tick(Some((4, 30)), ms(t0, 16), &c, g);
            let (cx, _) = g.cell_center(4, 30);
            let s = (0.6 * ch as f32) as i32;
            let left = (cx as i32 - s) as u16;
            assert!(
                rig.out.iter().any(|q| q.x == left && q.w == th),
                "ch {ch}: the ring's left bar is {th} px wide"
            );
        }
    }

    #[test]
    fn the_wake_decays_to_exactly_empty_and_disarms() {
        let t0 = Instant::now();
        for c in [cfg(), light_cfg()] {
            let g = geom();
            let mut rig = Rig::default();
            rig.tick(Some((2, 4)), t0, &c, g);
            rig.tick(Some((4, 34)), ms(t0, 16), &c, g);
            assert!(rig.w.is_active());
            let fp = rig.tick(Some((4, 34)), t0 + Duration::from_secs(4), &c, g);
            assert!(
                rig.out.is_empty() && rig.halos.is_empty(),
                "the wake decays to EXACTLY empty"
            );
            assert_eq!(fp, 0, "an empty frame fingerprints zero");
            assert!(!rig.w.is_active(), "and the animation timer disarms");
        }
    }

    #[test]
    fn disabled_returns_the_engine_to_rest() {
        let (g, mut c) = (geom(), cfg());
        let mut rig = Rig::default();
        let t0 = Instant::now();
        rig.tick(Some((2, 4)), t0, &c, g);
        rig.tick(Some((4, 34)), ms(t0, 16), &c, g);
        assert!(rig.w.is_active());
        c.enabled = false;
        let fp = rig.tick(Some((4, 34)), ms(t0, 32), &c, g);
        assert_eq!(fp, 0);
        assert!(rig.out.is_empty());
        assert!(!rig.w.is_active(), "disabled leaves nothing in flight");
    }

    #[test]
    fn the_mono_face_follows_the_configured_colour_and_the_spectrum_does_not() {
        let g = geom();
        let typed_on = |base: GlowConfig, mono: bool, color: u32, accent: u32| {
            let c = GlowConfig {
                classic_mono: mono,
                color,
                accent,
                ..base
            };
            let mut rig = Rig::default();
            let t0 = Instant::now();
            rig.tick(Some((2, 4)), t0, &c, g);
            rig.tick(Some((2, 5)), ms(t0, 16), &c, g);
            rig.out
                .iter()
                .chain(&rig.under)
                .map(|q| q.color)
                .collect::<Vec<_>>()
        };
        let typed = |mono: bool, color: u32, accent: u32| typed_on(cfg(), mono, color, accent);
        const GREEN: (u32, u32) = (0x0050_FA7B, 0x0078_FFB9);
        const AMBER: (u32, u32) = (0x00FF_A500, 0x00FF_D080);

        let mono_green = typed(true, GREEN.0, GREEN.1);
        assert_ne!(
            mono_green,
            typed(true, AMBER.0, AMBER.1),
            "the mono face follows the configured colour"
        );
        assert_eq!(
            typed(false, GREEN.0, GREEN.1),
            typed(false, AMBER.0, AMBER.1),
            "the spectrum face's comet and crown owe nothing to the theme"
        );
        assert_eq!(
            typed_on(light_cfg(), true, GREEN.0, GREEN.1).len(),
            typed_on(light_cfg(), false, GREEN.0, GREEN.1).len(),
            "the two faces differ in colour alone on a light ground"
        );
    }

    #[test]
    fn the_shared_ramp_speaks_the_engines_spectrum() {
        for i in 0..=8 {
            let (hue, pos) = (i as f32 * 0.11, i as f32 / 8.0);
            assert_eq!(
                crate::cursor_glow::style_comet_color(GlowStyle::Classic, 1, 2, hue, pos),
                comet_color(false, 1, 2, hue, pos)
            );
        }
        assert_eq!(
            comet_color(true, 0x00FF_0000, 0x0000_00FF, 0.3, 0.0),
            0x0000_00FF
        );
        assert_eq!(
            comet_color(true, 0x00FF_0000, 0x0000_00FF, 0.3, 1.0),
            0x00FF_0000
        );
    }

    fn dark_session_fold(c: &GlowConfig, g: Geom) -> u64 {
        dark_session_fold_as(c, g, |_| {})
    }

    /// [`dark_session_fold`] of a wake its test seams set first.
    fn dark_session_fold_as(c: &GlowConfig, g: Geom, seams: impl FnOnce(&mut ClassicWake)) -> u64 {
        use aterm_hash::fnv_mix_u64;
        let mut rig = Rig::default();
        seams(&mut rig.w);
        let t0 = Instant::now();
        let mut acc: u64 = 0;
        let mut frame = |rig: &mut Rig, cur: Option<(u16, u16)>, at: u64| {
            let fp = rig.tick(cur, ms(t0, at), c, g);
            assert!(
                rig.halos.iter().all(|h| h.mode == HaloMode::Add),
                "a dark ground lays no veils"
            );
            acc = fnv_mix_u64(acc, fp);
            acc = fnv_mix_u64(acc, rig.out.len() as u64);
            for q in &rig.out {
                for &word in q.damage_words().as_slice() {
                    acc = fnv_mix_u64(acc, word);
                }
            }
            for h in &rig.halos {
                for word in h.damage_words() {
                    acc = fnv_mix_u64(acc, word);
                }
            }
        };
        let mut at = 0;
        frame(&mut rig, Some((2, 4)), at);
        for k in 1..=8u16 {
            at += 60;
            frame(&mut rig, Some((2, 4 + k)), at);
            frame(&mut rig, Some((2, 4 + k)), at + 24);
        }
        at += 500;
        frame(&mut rig, Some((2, 13)), at);
        at += 80;
        frame(&mut rig, Some((2, 12)), at);
        at += 100;
        frame(&mut rig, Some((3, 12)), at);
        at += 150;
        frame(&mut rig, Some((5, 30)), at);
        for dt in [16u64, 90, 200] {
            frame(&mut rig, Some((5, 30)), at + dt);
        }
        at += 400;
        frame(&mut rig, Some((5, 3)), at);
        frame(&mut rig, Some((5, 3)), at + 40);
        at += 300;
        frame(&mut rig, Some((0, 20)), at);
        frame(&mut rig, Some((0, 20)), at + 70);
        at += 100;
        frame(&mut rig, None, at);
        at += 30;
        frame(&mut rig, Some((0, 21)), at);
        frame(&mut rig, Some((0, 21)), at + 4000);
        acc
    }

    #[test]
    fn the_mono_crown_is_one_smooth_round_bloom() {
        let g = geom();
        let mono = GlowConfig {
            classic_mono: true,
            ..cfg()
        };
        let column = |c: &GlowConfig| -> (Vec<i32>, usize) {
            let mut rig = Rig::default();
            let t0 = Instant::now();
            for k in 0..=8u16 {
                rig.tick(Some((2, 4 + k)), ms(t0, 50 * u64::from(k)), c, g);
            }
            rig.tick(Some((2, 12)), ms(t0, 420), c, g);
            let mut px = vec![0u32; g.win_w as usize * g.win_h as usize];
            for q in &rig.out {
                for y in q.y..q.y + q.h {
                    for x in q.x..q.x + q.w {
                        let p = &mut px[usize::from(y) * g.win_w as usize + usize::from(x)];
                        *p = over_premul(*p, q.color, q.alpha);
                    }
                }
            }
            for h in &rig.halos {
                let (cx, cy) = (i32::from(h.cx), i32::from(h.cy));
                let (rx2, ry2) = (i32::from(h.rx).pow(2), i32::from(h.ry).pow(2));
                for y in h.y..h.y + h.h {
                    let ny = aterm_render::halo_row_ny(i32::from(y) - cy, ry2);
                    if ny >= 256 {
                        continue;
                    }
                    for x in h.x..h.x + h.w {
                        let wt = aterm_render::halo_weight(i32::from(x) - cx, ny, rx2).min(255);
                        let p = &mut px[usize::from(y) * g.win_w as usize + usize::from(x)];
                        *p = aterm_render::add_sat(*p, aterm_render::premul_rgb(h.color, wt as u8));
                    }
                }
            }
            let x = 12 * g.cw + g.cw / 2;
            let bottom = 3 * g.ch;
            let line = (bottom..bottom + 2 * g.ch)
                .map(|y| {
                    let p = px[y * g.win_w as usize + x];
                    (((p >> 16) & 0xff).max((p >> 8) & 0xff).max(p & 0xff)) as i32
                })
                .collect();
            (line, rig.halos.len())
        };
        let steepest = |l: &[i32]| {
            let peak = l.iter().copied().max().unwrap_or(0).max(1);
            let step = l.windows(2).map(|w| (w[1] - w[0]).abs()).max().unwrap_or(0);
            step as f32 / peak as f32
        };
        let (round, halos) = column(&mono);
        assert!(halos > 0, "the mono crown is radial halos");
        assert!(
            round.iter().any(|&v| v > 0),
            "the bloom reaches past the caret's cell"
        );
        assert!(
            steepest(&round) <= 0.35,
            "the bloom steps by {:.2} of its light in one pixel: {round:?}",
            steepest(&round)
        );
        let (boxed, _) = column(&cfg());
        assert!(
            steepest(&boxed) > 0.35,
            "control: the box crown steps by only {:.2}: {boxed:?}",
            steepest(&boxed)
        );
    }

    /// The SPECTRUM wake on dark is v0.28's, byte for byte (the owner's
    /// ruling). The one-colour wake's fold was re-baked deliberately by
    /// looks8 K2, whose landing ring lies under the letters, again by looks9
    /// (Q4's whole ring, Q11's one stroke up a page), and by looks10 (L6: a
    /// stroke up a page runs from the old caret to the new): with L6's seam
    /// set back it folds to looks9's value, and with looks9's two set back
    /// too to looks8's, so each re-bake moves nothing else.
    #[test]
    fn the_dark_wake_is_byte_identical_to_its_base() {
        let mono = GlowConfig {
            classic_mono: true,
            ..cfg()
        };
        let padded = Geom {
            cw: 11,
            ch: 23,
            rows: 6,
            cols: 40,
            origin_x: 8,
            origin_y: 56,
            win_w: 456,
            win_h: 202,
            head: 48,
        };
        let got = [
            dark_session_fold(&cfg(), geom()),
            dark_session_fold(&mono, geom()),
            dark_session_fold(&cfg(), padded),
        ];
        assert_eq!(
            got,
            [
                11_293_809_440_103_835_944,
                2_735_246_743_558_469_609,
                8_579_084_186_203_313_172,
            ],
            "the dark classic wake drifted from its base"
        );
        let looks9 = dark_session_fold_as(&mono, geom(), |w| {
            w.tracer_up_stretch = true;
        });
        assert_eq!(
            looks9, 15_572_736_309_831_782_819,
            "control: the one-colour wake with looks10's seam set back is looks9's"
        );
        let looks8 = dark_session_fold_as(&mono, geom(), |w| {
            w.ring_cut = true;
            w.tracer_up_kept = true;
            w.tracer_up_stretch = true;
        });
        assert_eq!(
            looks8, 11_956_472_567_039_891_853,
            "control: the one-colour wake with looks9's seams set back is looks8's"
        );
        let bar = GlowConfig {
            head_dx: 0.08,
            ..cfg()
        };
        assert_ne!(
            dark_session_fold(&bar, geom()),
            got[0],
            "control: the pin must be able to see the comet move"
        );
    }

    const fn shipped_dark_at(length: usize, intensity: f32) -> GlowConfig {
        crate::cursor_fx::resolve_glow_config(
            crate::cursor_fx::GlowKnobs {
                enabled: true,
                color: None,
                accent: None,
                duration_ms: 260,
                length,
                intensity,
                radius: 0.6,
                ring: true,
            },
            crate::cursor_fx::GlowSpelling::parse("classic"),
            0x0050_FA7B,
            true,
            0x00D0_D0D0,
            0x0011_1318,
            0.5,
        )
    }

    const fn shipped_dark(length: usize) -> GlowConfig {
        shipped_dark_at(
            length,
            crate::cursor_fx::default_trail_intensity(Some(GlowStyle::Classic), false),
        )
    }

    #[test]
    fn the_host_resolved_dark_wake_is_byte_identical_to_its_base() {
        let shipped = shipped_dark(24);
        assert_eq!(
            (
                shipped.style,
                shipped.enabled,
                shipped.duration,
                shipped.length,
                shipped.intensity,
                shipped.radius,
                shipped.ring,
                shipped.classic_mono,
                shipped.dark_theme,
            ),
            (
                GlowStyle::Classic,
                true,
                Duration::from_millis(260),
                24,
                cfg().intensity,
                0.6,
                true,
                false,
                true,
            ),
            "the config law changed a knob Classic reads"
        );
        let padded = Geom {
            cw: 11,
            ch: 23,
            rows: 6,
            cols: 40,
            origin_x: 8,
            origin_y: 56,
            win_w: 456,
            win_h: 202,
            head: 48,
        };
        let got = [
            dark_session_fold(&shipped, geom()),
            dark_session_fold(&shipped, padded),
        ];
        assert_eq!(
            got,
            [8_976_404_549_136_510_798, 10_432_365_287_462_349_185],
            "the host-resolved dark classic wake drifted from its base"
        );
        assert_ne!(
            dark_session_fold(&shipped_dark(18), geom()),
            got[0],
            "control: a resized comet must fold to another print"
        );
        assert_ne!(
            dark_session_fold(&shipped_dark_at(24, 1.0), geom()),
            got[0],
            "control: the full brightness must fold to another print"
        );
    }

    /// THE PAPER SPECTRUM LAYS NO OLIVE (looks8 K10): Classic's spectrum face
    /// on paper darkens each colour to the paper's bar, and its yellows
    /// darkened read olive and khaki (the `enter-bottom` Return's tracer).
    /// Through a jump at every twelfth of the hue wheel, no stroke or crown
    /// quad on paper wears a hue between 45 and 80 degrees. Negative
    /// control: walking the whole wheel, some do.
    #[test]
    fn the_paper_spectrum_lays_no_olive() {
        let (g, c) = (geom(), light_cfg());
        let olive = |muddy: bool| {
            let mut n = 0;
            for k in 0..12 {
                let t0 = Instant::now();
                let mut rig = Rig::default();
                rig.w.paper_muddy = muddy;
                rig.tick(Some((1, 4)), t0, &c, g);
                rig.w.hue = k as f32 / 12.0;
                rig.tick(Some((4, 30)), ms(t0, 16), &c, g);
                // Each end's colour, its premultiplied light over its opacity.
                let own = |rgb: u32, a: u8| {
                    let ch =
                        |sh: u32| ((((rgb >> sh) & 0xFF) * 255) / u32::from(a.max(1))).min(255);
                    (ch(16) << 16) | (ch(8) << 8) | ch(0)
                };
                n += rig
                    .under
                    .iter()
                    .flat_map(|q| [(q.color, q.alpha), (q.color2, q.alpha2)])
                    .filter(|&(rgb, a)| {
                        let (h, s, _) = crate::color_math::rgb2hsv(own(rgb, a));
                        a >= 24 && s > 0.3 && (45.0..80.0).contains(&h)
                    })
                    .count();
            }
            n
        };
        assert!(
            olive(true) > 0,
            "control: the whole wheel lays olive on paper"
        );
        assert_eq!(olive(false), 0, "the paper spectrum lays olive");
    }

    /// A CHOSEN COLOUR'S BED KEEPS NEAR A LIT PAGE UNDER ITS LETTERS (looks8
    /// round 2's K10): on Solarized Dark the one-colour face's bed under the
    /// letters lies at the page's luminance in its own hue, and a colour far
    /// round the wheel from the page's (`#d03a8c` on the teal) laid a maroon
    /// smear the letters' edges blended over and read under their bar (`f`
    /// at 3.93:1 of its own 4.06). Under a lettered row a leap's bed keeps
    /// within half the distance from the page the whole hue took. Negative
    /// control: the whole hue.
    #[test]
    fn a_chosen_colour_s_bed_keeps_near_a_lit_page_under_its_letters() {
        let g = geom();
        let c = GlowConfig {
            classic_mono: true,
            color: 0x00D0_3A8C,
            accent: 0x00D0_3A8C,
            theme_fg: 0x0083_9496,
            theme_bg: 0x0000_2B36,
            ..cfg()
        };
        let lines = [(2, 0, 30)];
        let ch = g.ch as i32;
        let reach = |whole: bool| {
            let t0 = Instant::now();
            let mut rig = Rig {
                spans: crate::cursor_glow::TextSpans::of_lines(&lines),
                ..Rig::default()
            };
            rig.w.mono_lit_whole = whole;
            rig.tick(Some((2, 2)), t0, &c, g);
            rig.tick(Some((2, 28)), ms(t0, 16), &c, g);
            let own = |rgb: u32, a: u8| {
                let ch = |sh: u32| ((((rgb >> sh) & 0xFF) * 255) / u32::from(a.max(1))).min(255);
                (ch(16) << 16) | (ch(8) << 8) | ch(0)
            };
            let far = |rgb: u32| {
                [16u32, 8, 0]
                    .iter()
                    .map(|&sh| {
                        (((rgb >> sh) & 0xFF) as i32 - ((c.theme_bg >> sh) & 0xFF) as i32).abs()
                    })
                    .max()
                    .unwrap_or(0)
            };
            rig.under
                .iter()
                .filter(|q| {
                    let (y0, y1) = (i32::from(q.y), i32::from(q.y) + i32::from(q.h));
                    y0 >= 2 * ch + ch / 4 && y1 <= 3 * ch - ch / 4
                })
                .flat_map(|q| [(q.color, q.alpha), (q.color2, q.alpha2)])
                .filter(|&(_, a)| a >= 64)
                .map(|(rgb, a)| far(own(rgb, a)))
                .max()
                .unwrap_or(0)
        };
        let (kept, whole) = (reach(false), reach(true));
        assert!(
            whole >= 20,
            "control: the whole hue lies {whole} from the page"
        );
        assert!(
            2 * kept <= whole,
            "under the letters the bed lies {kept} from the page, the whole hue {whole}"
        );
    }

    #[test]
    fn a_light_ground_draws_ink_under_the_letters_and_a_veil_ring_and_no_additive_light() {
        let (g, c) = (geom(), light_cfg());
        let t0 = Instant::now();
        let mut rig = Rig::default();
        rig.tick(Some((1, 4)), t0, &c, g);
        let fp = rig.tick(Some((4, 30)), ms(t0, 16), &c, g);
        // The ring's own square, laid in ink over the paper (r17 CA4: the veil alone changed
        // some 20 px of a card by more than 32 levels).
        assert!(!rig.out.is_empty(), "a light-ground landing lays its ring");
        let (rx, ry) = g.cell_center(4, 30);
        let s = (0.6 * g.ch as f32) as i32 + 1;
        for q in &rig.out {
            assert!(
                q.alpha > 0 && f32::from(q.alpha) <= LIGHT_INK_ALPHA_CAP,
                "a light ground emits no additive quad, and its ring stays under the over-text \
                 ceiling: {q:?}"
            );
            let (x0, y0) = (i32::from(q.x), i32::from(q.y));
            let (x1, y1) = (x0 + i32::from(q.w), y0 + i32::from(q.h));
            assert!(
                x0 >= rx as i32 - s - 1
                    && x1 <= rx as i32 + s + 1
                    && y0 >= ry as i32 - s - 1
                    && y1 <= ry as i32 + s + 1,
                "the ring's square stands about the landing: {q:?}"
            );
        }
        assert!(!rig.under.is_empty(), "a light-ground jump is visible");
        assert_ne!(fp, 0, "a frame of ink and veils alone still fingerprints");
        for q in &rig.under {
            assert!(
                q.alpha > 0 && q.alpha2 > 0,
                "the stroke is source-over: {q:?}"
            );
            assert!(
                f32::from(q.alpha.max(q.alpha2)) <= LIGHT_BODY_ALPHA_CAP,
                "the stroke stays under the body cap: {q:?}"
            );
        }
        let ring = g.cell_center(4, 30);
        assert!(
            rig.halos.iter().all(|h| h.mode == HaloMode::Over),
            "every veil is source-over"
        );
        assert!(
            rig.halos.iter().any(|h| (h.cx, h.cy)
                == (ring.0.round() as u16, ring.1.round() as u16)
                && halo_over_cap(h.color) <= 120),
            "the landing ring is a capped veil on the landing cell"
        );
    }

    #[test]
    fn the_paper_landing_veil_stays_in_the_caret_row_at_the_text_bar() {
        let g = geom();
        let caret = (4u16, 12u16);
        let (ccx, _) = g.cell_center(caret.0, caret.1);
        let reach = CursorGlow::LIGHT_VEIL_REACH_CELLS * g.cw as f32 + 1.0;
        let (row_top, row_bot) = (
            i32::from(caret.0) * g.ch as i32,
            i32::from(caret.0 + 1) * g.ch as i32,
        );
        let (mut escapes, mut drawn) = (0usize, 0usize);
        for (fg, bg) in [(INK, PAPER), (0x0033_3333, 0x00FA_F8F5)] {
            for accent in [light_cfg().accent, 0x00D0_3A8C] {
                let c = GlowConfig {
                    theme_fg: fg,
                    theme_bg: bg,
                    accent,
                    color: accent,
                    ..light_cfg()
                };
                for unbound in [false, true] {
                    let born = Instant::now();
                    let mut w = ClassicWake {
                        ring: Some(Ring {
                            row: caret.0 - 1,
                            col: caret.1,
                            born,
                            life: 0.4,
                            caret,
                        }),
                        veil_unbound: unbound,
                        ..ClassicWake::default()
                    };
                    for (at, cur) in [(30u64, Some(caret)), (120, Some(caret)), (60, None)] {
                        let mut halos = Vec::new();
                        w.emit_light_ring(ms(born, at), &c, g, cur, &mut halos);
                        for h in &halos {
                            let inside = i32::from(h.y) >= row_top
                                && i32::from(h.y) + i32::from(h.h) <= row_bot
                                && (f32::from(h.cx) - ccx).abs() <= 1.0
                                && f32::from(h.rx) <= reach;
                            if unbound {
                                escapes += usize::from(!inside);
                                continue;
                            }
                            drawn += 1;
                            assert!(
                                inside,
                                "{accent:06x} on {bg:06x}, {at} ms, caret {cur:?}: the veil \
                                 {h:?} leaves the caret's cell and row"
                            );
                        }
                        if unbound {
                            continue;
                        }
                        let (cx, cy) = (halos[0].cx, halos[0].cy);
                        let under = halos
                            .iter()
                            .filter(|h| {
                                (h.x..h.x + h.w).contains(&cx) && (h.y..h.y + h.h).contains(&cy)
                            })
                            .fold(bg, |px, h| {
                                let a = u32::from(halo_over_cap(h.color));
                                let veil = h.color & 0x00FF_FFFF;
                                let mix = |sh: u32| {
                                    (((veil >> sh) & 0xff) * a
                                        + ((px >> sh) & 0xff) * (255 - a)
                                        + 127)
                                        / 255
                                };
                                (mix(16) << 16) | (mix(8) << 8) | mix(0)
                            });
                        let k = crate::color_math::contrast_ratio(fg, under);
                        assert!(
                            k >= 4.5,
                            "{accent:06x} on {bg:06x}, {at} ms: the text over the veil's centre \
                             reads {k:.2}:1"
                        );
                    }
                }
            }
        }
        assert!(drawn > 0, "the paper landing lays no veil");
        assert!(
            escapes > 0,
            "control: the unbound veil stays in the caret's row"
        );
    }

    #[allow(clippy::type_complexity)]
    fn light_frame(
        under: &[GlowQuad],
        g: Geom,
        row: u16,
        cols: std::ops::Range<u16>,
        times: usize,
    ) -> (Vec<u32>, Vec<(Vec<usize>, Vec<usize>)>) {
        let (w, h) = (usize::from(g.win_w), usize::from(g.win_h));
        let mut px = vec![PAPER; w * h];
        for _ in 0..times {
            for q in under {
                for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                    for x in usize::from(q.x)..(usize::from(q.x) + usize::from(q.w)).min(w) {
                        px[y * w + x] = over_premul(px[y * w + x], q.color, q.alpha) & 0x00FF_FFFF;
                    }
                }
            }
        }
        let mut cells = Vec::new();
        for col in cols {
            let (x0, y0) = (usize::from(col) * g.cw, usize::from(row) * g.ch);
            for y in y0 + 1..y0 + g.ch - 1 {
                let (mut stems, mut counters) = (Vec::new(), Vec::new());
                for x in x0 + 1..x0 + g.cw - 1 {
                    if matches!(x - x0, 2 | 5) {
                        stems.push(y * w + x);
                    } else {
                        counters.push(y * w + x);
                    }
                }
                for &i in &stems {
                    px[i] = INK;
                }
                cells.push((stems, counters));
            }
        }
        (px, cells)
    }

    fn ratio(px: &[u32], a: &[usize], b: &[usize]) -> f32 {
        let mean = |set: &[usize]| {
            set.iter().map(|&i| relative_luminance(px[i])).sum::<f32>() / set.len() as f32
        };
        let (la, lb) = (mean(a), mean(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn a_typed_run_on_light_is_an_underline_inside_its_row_that_keeps_the_letters_legible() {
        let (g, c) = (geom(), light_cfg());
        let crownless = GlowConfig { radius: 0.0, ..c };
        let t0 = Instant::now();
        let mut rig = Rig::default();
        let mut stroke = Rig::default();
        let row = 2u16;
        let (mut worst, mut worst_stacked, mut drawn) = (f32::MAX, f32::MAX, 0usize);
        let mut vivid = Rig::default();
        vivid.w.light_unsolved = true;
        let mut worst_vivid = f32::MAX;
        vivid.tick(Some((row, 3)), t0, &c, g);
        rig.tick(Some((row, 3)), t0, &c, g);
        stroke.tick(Some((row, 3)), t0, &crownless, g);
        for col in 4..=30u16 {
            rig.tick(Some((row, col)), ms(t0, 40 * u64::from(col)), &c, g);
            assert!(
                rig.out.is_empty() && !rig.under.is_empty(),
                "col {col}: ink only"
            );
            drawn += rig.under.len();
            stroke.tick(Some((row, col)), ms(t0, 40 * u64::from(col)), &crownless, g);
            let (top, bot) = (usize::from(row) * g.ch, usize::from(row + 1) * g.ch);
            for q in &rig.under {
                assert!(
                    usize::from(q.y) >= top && usize::from(q.y) + usize::from(q.h) <= bot,
                    "col {col}: the wake leaves the typed row: {q:?}"
                );
            }
            for q in &stroke.under {
                assert!(
                    usize::from(q.y) >= top + g.ch / 2
                        && usize::from(q.y) + usize::from(q.h) <= bot,
                    "col {col}: the stroke leaves the row's lower band: {q:?}"
                );
            }
            let typed = 3..col;
            let (px, cells) = light_frame(&rig.under, g, row, typed.clone(), 1);
            for (stems, counters) in &cells {
                worst = worst.min(ratio(&px, stems, counters));
            }
            let (px, cells) = light_frame(&rig.under, g, row, typed.clone(), 3);
            for (stems, counters) in &cells {
                worst_stacked = worst_stacked.min(ratio(&px, stems, counters));
            }
            vivid.tick(Some((row, col)), ms(t0, 40 * u64::from(col)), &c, g);
            let (px, cells) = light_frame(&vivid.under, g, row, typed, 1);
            for (stems, counters) in &cells {
                worst_vivid = worst_vivid.min(ratio(&px, stems, counters));
            }
        }
        assert!(drawn > 50, "fixture: the session drew strokes ({drawn})");
        assert!(worst >= 4.5, "the underline buries a letter: {worst:.2}:1");
        assert!(
            worst_stacked >= 4.5,
            "laid three deep the underline buries a letter: {worst_stacked:.2}:1"
        );
        assert!(
            worst_vivid < 4.5,
            "control: the unsolved ink must fail the bar ({worst_vivid:.2}:1)"
        );
    }

    #[test]
    fn the_light_crown_is_one_round_ink_mark_on_the_caret_row() {
        let (g, c) = (geom(), light_cfg());
        let crownless = GlowConfig { radius: 0.0, ..c };
        let t0 = Instant::now();
        let (mut rig, mut twin) = (Rig::default(), Rig::default());
        let row = 2u16;
        let mut at = 0;
        let crown_of = |rig: &mut Rig, twin: &mut Rig, col: u16, at: u64| {
            rig.tick(Some((row, col)), ms(t0, at), &c, g);
            twin.tick(Some((row, col)), ms(t0, at), &crownless, g);
            let n = rig.under.len() - twin.under.len();
            assert_eq!(
                rig.under[n..],
                twin.under[..],
                "col {col}: the stroke over the crown is the crownless stroke"
            );
            rig.under[..n].to_vec()
        };
        crown_of(&mut rig, &mut twin, 3, at);
        let mut hot = Vec::new();
        for col in 4..=14u16 {
            at += 40;
            hot = crown_of(&mut rig, &mut twin, col, at);
        }
        assert!(!hot.is_empty(), "a hot typed run wears its crown");
        let (cx, _) = g.cell_center(row, 14);
        let (top, bot) = (row * g.ch as u16, (row + 1) * g.ch as u16);
        assert!(
            !twin.under.is_empty() && twin.under.iter().all(|q| q.y >= top + g.ch as u16 / 2),
            "control: at Glow size 0 the wake lays its stroke and no crown"
        );
        let (mut left, mut right, mut peak_at, mut peak) = (f32::MAX, f32::MIN, 0.0, 0u8);
        for q in &hot {
            assert!(q.alpha > 0 && q.alpha2 > 0, "source-over ink: {q:?}");
            assert!(
                f32::from(q.alpha.max(q.alpha2)) <= LIGHT_BODY_ALPHA_CAP,
                "under the body cap: {q:?}"
            );
            assert!(
                q.y >= top && q.y + q.h <= bot,
                "the crown stays on the caret's row: {q:?}"
            );
            left = left.min(f32::from(q.x));
            right = right.max(f32::from(q.x + q.w));
            if q.alpha.max(q.alpha2) > peak {
                peak = q.alpha.max(q.alpha2);
                peak_at = f32::from(q.x) + f32::from(q.w) * 0.5;
            }
        }
        let half = g.cw as f32 * 0.5;
        let reach = half + c.radius * g.ch as f32 * 0.9;
        for (side, edge) in [("left", cx - left), ("right", right - cx)] {
            assert!(
                edge > half + 0.5 * (reach - half) && edge <= reach + 2.0,
                "the {side} edge sits {edge:.1} px from the caret's centre, reach {reach:.1}"
            );
        }
        assert!(
            ((cx - left) - (right - cx)).abs() <= 2.0,
            "round about the caret: {left:.1}..{right:.1} around {cx:.1}"
        );
        assert!(
            (peak_at - cx).abs() <= g.cw as f32 * 0.5,
            "deepest on the caret's cell, at {peak_at:.1} (caret {cx:.1})"
        );
        at += 4_000;
        crown_of(&mut rig, &mut twin, 14, at);
        at += 40;
        let cool = crown_of(&mut rig, &mut twin, 15, at);
        let deepest = |quads: &[GlowQuad]| quads.iter().map(|q| q.alpha.max(q.alpha2)).max();
        assert!(
            deepest(&cool).is_some_and(|cool| cool < peak),
            "a cool key inks a fainter crown: {:?} vs {peak}",
            deepest(&cool)
        );
        at += ClassicWake::CROWN_TYPING_MS + 1;
        assert!(
            crown_of(&mut rig, &mut twin, 15, at).is_empty(),
            "the crown ends with its window"
        );
    }

    #[test]
    fn a_vertical_jump_on_light_is_one_connected_stroke() {
        let (g, c) = (geom(), light_cfg());
        let t0 = Instant::now();
        let mut rig = Rig::default();
        let (from, to) = ((5u16, 6u16), (0u16, 6u16));
        rig.tick(Some(from), t0, &c, g);
        rig.tick(Some(to), ms(t0, 16), &c, g);
        assert!(!rig.under.is_empty(), "the jump is drawn");
        let x = g.cell_center(to.0, to.1).0.round() as u16;
        let drop = g.ch as f32 * ClassicWake::LIGHT_STROKE_DROP;
        let y_top = (g.cell_center(to.0, to.1).1 + drop).round() as u16;
        let y_bot = (g.cell_center(from.0, from.1).1 + drop).round() as u16 - 1;
        let gaps = |quads: &[GlowQuad]| {
            (y_top..y_bot)
                .filter(|&y| {
                    !quads
                        .iter()
                        .any(|q| q.y <= y && y < q.y + q.h && q.x <= x && x < q.x + q.w)
                })
                .count()
        };
        assert_eq!(gaps(&rig.under), 0, "the stroke joins every row it crosses");
        let (cw, ch) = (g.cw as u16, g.ch as u16);
        let reach = (f32::from(ch) * 0.22) as u16;
        let rail_quads: Vec<GlowQuad> = (to.0..=from.0)
            .flat_map(|row| {
                let (x0, y0) = (to.1 * cw, row * ch);
                [y0, y0 + ch - reach].map(|y| GlowQuad::flat(row, x0, y, cw, reach, c.color, 200))
            })
            .collect();
        assert!(
            gaps(&rail_quads) > 0,
            "control: per-cell rails leave gaps between rows"
        );
    }

    /// looks7 M8: on paper an Enter's tracer ran straight from the old
    /// line's end to the new line's start at the baseline's drop, across the
    /// printed line's letters: a strike-through through `brown fox` in the
    /// gallery's `enter-bottom`; passed behind each lettered row, it broke
    /// into a stub at the caret and a dash floating past the printed line's
    /// end (round 2). It keeps to the caret's row, for an Enter (a run from
    /// above) and for a move up a page across lettered rows (a run from
    /// below, round 3's review: eased in from the row's top alone it was a
    /// full stub cut at the row's bottom): no pixel of another row takes its
    /// ink, the caret's row shows it, and it fades in from the edge it
    /// enters by, no row of pixels lifting by more than half its brightest
    /// toward the caret, and walking its tail away from the caret no column
    /// drops by more than a quarter of its brightest. Laid across the rows
    /// (the first negative control) it strikes the lines it crosses; faded
    /// over a pixel (the second) it is cut at the row's edge. Across rows
    /// clear of letters it stays one stroke
    /// (`a_vertical_jump_on_light_is_one_connected_stroke`). A move up now
    /// keeps its longest clear stretch instead
    /// (`a_paper_move_up_draws_one_stroke_off_the_letters`); kept to the
    /// caret's row (`tracer_up_kept`), it holds these laws.
    #[test]
    fn a_paper_enter_s_tracer_keeps_to_the_caret_s_row() {
        let (g, c) = (geom(), light_cfg());
        let (cw, ch) = (g.cw as i32, g.ch as i32);
        // The echo on row 2 to column 26, the printed line on row 3 to 19.
        let lines = [(2, 0, 26), (3, 0, 19)];
        let held = |x: i32, y: i32| {
            let (row, col) = (y.div_euclid(ch), x.div_euclid(cw));
            lines
                .iter()
                .any(|&(r, first, end)| r == row && col >= first && col <= end)
        };
        struct Laid {
            struck: usize,
            off_row: usize,
            shown: usize,
            lift: f32,
            drop: f32,
        }
        // An Enter from the echo's end to the next prompt (from above), and
        // a move up from below the printed line across both lines.
        let enter = ((2u16, 26u16), (4u16, 2u16));
        let up = ((4u16, 20u16), (1u16, 2u16));
        let laid = |(from, to): ((u16, u16), (u16, u16)),
                    over: bool,
                    leave: Option<f32>,
                    top_only: bool|
         -> Laid {
            let t0 = Instant::now();
            let mut rig = Rig {
                spans: crate::cursor_glow::TextSpans::of_lines(&lines),
                ..Rig::default()
            };
            rig.w.tracer_over_letters = over;
            rig.w.tracer_leave_px = leave;
            rig.w.tracer_top_only = top_only;
            rig.w.tracer_up_kept = true;
            rig.tick(Some(from), t0, &c, g);
            let ww = usize::from(g.win_w);
            let caret_row = i32::from(to.0);
            let mut most = vec![0u32; ch as usize];
            let mut cols = vec![0u32; ww];
            let (mut struck, mut off_row, mut shown) = (0usize, 0usize, 0usize);
            for at in [16u64, 40, 80, 140] {
                rig.tick(Some(to), ms(t0, at), &c, g);
                let mut ink = vec![0u32; ww * usize::from(g.win_h)];
                for q in &rig.under {
                    let w = u32::from(q.w.max(1));
                    for y in i32::from(q.y)..i32::from(q.y) + i32::from(q.h) {
                        for x in i32::from(q.x)..i32::from(q.x) + i32::from(q.w) {
                            let i = (x - i32::from(q.x)) as u32;
                            let a = (u32::from(q.alpha) * (2 * w - (2 * i + 1))
                                + u32::from(q.alpha2) * (2 * i + 1)
                                + w)
                                / (2 * w);
                            let slot = &mut ink[y as usize * ww + x as usize];
                            *slot = (*slot).max(a);
                        }
                    }
                }
                for (i, &a) in ink.iter().enumerate() {
                    let (x, y) = ((i % ww) as i32, (i / ww) as i32);
                    let row = y.div_euclid(ch);
                    if row == caret_row {
                        let m = &mut most[y.rem_euclid(ch) as usize];
                        *m = (*m).max(a);
                        let m = &mut cols[x as usize];
                        *m = (*m).max(a);
                    }
                    if a <= 16 {
                        continue;
                    }
                    if held(x, y) {
                        struck += 1;
                    }
                    if row == caret_row {
                        shown += 1;
                    } else {
                        off_row += 1;
                    }
                }
            }
            let peak = most.iter().copied().max().unwrap_or(0).max(1) as f32;
            // Toward the caret from the edge the run enters by.
            if from.0 > to.0 {
                most.reverse();
            }
            let lift = most
                .windows(2)
                .map(|w| w[1].saturating_sub(w[0]) as f32 / peak)
                .fold(0.0, f32::max);
            // Along the tail, from the caret's column toward the one it left.
            let caret_x = (i32::from(to.1) * cw + cw / 2) as usize;
            let tail: Vec<u32> = if from.1 > to.1 {
                cols[caret_x..].to_vec()
            } else {
                cols[..=caret_x].iter().rev().copied().collect()
            };
            let drop = tail
                .windows(2)
                .map(|w| w[0].saturating_sub(w[1]) as f32 / peak)
                .fold(0.0, f32::max);
            Laid {
                struck,
                off_row,
                shown,
                lift,
                drop,
            }
        };
        for (name, run) in [("Enter", enter), ("up", up)] {
            let kept = laid(run, false, None, false);
            assert_eq!(
                (kept.struck, kept.off_row),
                (0, 0),
                "{name}: pixels of the lines the tracer crosses, and of the rows off the caret's, take its ink"
            );
            assert!(
                kept.shown > 0,
                "{name}: fixture: the tracer is drawn on the caret's row"
            );
            assert!(
                kept.drop <= 0.25,
                "{name}: the tracer's tail drops by {:.2} of its brightest from one column to the next",
                kept.drop
            );
            let over = laid(run, true, None, false);
            assert!(
                over.struck > 0 && over.off_row > 0,
                "{name}: control: laid across the rows, the tracer strikes nothing"
            );
        }
        // An Enter crosses most of the caret's row from its top: the rise is
        // gentle down the row too. A run from below crosses only the room
        // between the row's bottom and the caret's baseline, a few px, in
        // which its taper rises faster, so it is judged along its tail.
        let kept = laid(enter, false, None, false);
        assert!(
            kept.lift <= 0.5,
            "Enter: the tracer lifts by {:.2} of its brightest from one row of pixels to the next",
            kept.lift
        );
        let cut = laid(enter, false, Some(0.0), false);
        assert!(
            cut.lift > 0.5,
            "control: faded over a pixel, the Enter's tracer lifts by only {:.2}",
            cut.lift
        );
        let top_only = laid(up, false, None, true);
        assert!(
            top_only.drop > 0.25,
            "control: eased in from the row's top alone, the tail from below drops by only {:.2}",
            top_only.drop
        );
    }

    /// looks10 L6 (after looks9 Q11): on paper Classic and Classic pink drew a move up a page
    /// that lands among letters as its longest stretch clear of them, a dash floating between
    /// the old caret and the new, anchored to neither (`classic-light-move-up.up`). It runs from
    /// the old caret to the new, through the lettered rows at a share of its light
    /// ([`TRACER_UNDER_LETTERS_PAPER`]): each frame it is one stroke (one run of lit pixel columns),
    /// it shows, and its light reaches within two px of the landing marks round the caret
    /// (the letters it passes keep their bar by the letter guard). Negative control: kept to
    /// its longest stretch, it ends six px or more short of the landing.
    #[test]
    fn a_paper_move_up_runs_to_the_caret() {
        let g = geom();
        let (cw, ch) = (g.cw as i32, g.ch as i32);
        for c in [
            light_cfg(),
            GlowConfig {
                classic_mono: true,
                ..light_cfg()
            },
        ] {
            // The echo on row 2 to column 26, the printed line on row 3 to 19.
            let lines = [(2, 0, 26), (3, 0, 19)];
            // (lit px, most stroke runs in a frame, least gap to the landing in px)
            let laid = |stretch: bool| -> (usize, usize, i32) {
                let t0 = Instant::now();
                let mut rig = Rig {
                    spans: crate::cursor_glow::TextSpans::of_lines(&lines),
                    ..Rig::default()
                };
                rig.w.tracer_up_stretch = stretch;
                rig.tick(Some((4, 20)), t0, &c, g);
                let ww = usize::from(g.win_w);
                let (mut shown, mut runs, mut gap) = (0usize, 0usize, i32::MAX);
                for at in [16u64, 40, 80, 140] {
                    rig.tick(Some((1, 2)), ms(t0, at), &c, g);
                    let mut ink = vec![0u32; ww * usize::from(g.win_h)];
                    for q in &rig.under {
                        for y in i32::from(q.y)..i32::from(q.y) + i32::from(q.h) {
                            for x in i32::from(q.x)..i32::from(q.x) + i32::from(q.w) {
                                let slot = &mut ink[y as usize * ww + x as usize];
                                *slot = (*slot).max(u32::from(q.alpha.max(q.alpha2)));
                            }
                        }
                    }
                    // The caret's own cell and the landing marks round it
                    // are the landing's, not the tracer's.
                    let (cx0, cy0) = (2 * cw - cw, ch - ch / 2);
                    let (cx1, cy1) = (cx0 + 3 * cw - 1, cy0 + 2 * ch - 1);
                    let landing =
                        |x: i32, y: i32| (cx0..=cx1).contains(&x) && (cy0..=cy1).contains(&y);
                    let mut lit_cols = vec![false; ww];
                    for (i, &a) in ink.iter().enumerate() {
                        let (x, y) = ((i % ww) as i32, (i / ww) as i32);
                        if a <= 16 || landing(x, y) {
                            continue;
                        }
                        shown += 1;
                        gap = gap.min((cx0 - x).max(x - cx1).max(cy0 - y).max(y - cy1));
                        lit_cols[x as usize] = true;
                    }
                    let frame_runs = lit_cols.windows(2).filter(|w| w[1] && !w[0]).count()
                        + usize::from(lit_cols[0]);
                    runs = runs.max(frame_runs);
                }
                (shown, runs, gap)
            };
            let mono = c.classic_mono;
            let (shown, runs, gap) = laid(false);
            assert!(shown >= 60, "mono {mono}: the tracer shows only {shown} px");
            assert_eq!(runs, 1, "mono {mono}: the tracer falls into {runs} strokes");
            assert!(
                gap <= 2,
                "mono {mono}: the tracer ends {gap} px short of the landing"
            );
            let (_, _, stretch_gap) = laid(true);
            assert!(
                stretch_gap >= 6,
                "mono {mono}: control: kept to its longest stretch the tracer ends {stretch_gap} px short"
            );
        }
    }

    /// looks10 L6 (after looks9 Q11): on a dark page Classic pink's tracer for a move up across
    /// lettered rows kept its longest piece in the air, a dash floating between the old caret
    /// and the new, anchored to neither (`classic-pink/classic-dark-move-up.up`). It runs from
    /// the old caret to the new, through the lettered rows at a share of its light
    /// ([`TRACER_UNDER_LETTERS`]): each frame its light past the caret's cell is one run of lit
    /// pixel columns and it reaches within two px of the caret's cell (the letters it passes
    /// keep their bar by the letter guard). Negative control: kept to its longest piece, it
    /// ends six px or more short of the caret's cell.
    #[test]
    fn a_one_colour_move_up_on_a_dark_page_runs_to_the_caret() {
        let g = geom();
        let (cw, ch) = (g.cw as i32, g.ch as i32);
        let c = GlowConfig {
            classic_mono: true,
            color: 0x00D0_3A8C,
            accent: 0x00D0_3A8C,
            ..cfg()
        };
        // The old caret's line, a line it rises past with letters in its
        // middle, and the line it lands in (the probe knows three rows).
        let lines = [(4, 0, 1), (3, 10, 18), (2, 0, 34)];
        let (from, to) = ((4u16, 2u16), (2u16, 30u16));
        // (lit px, most runs in a frame, least gap to the caret's cell)
        let laid = |stretch: bool| -> (usize, usize, i32) {
            let t0 = Instant::now();
            let mut rig = Rig {
                spans: crate::cursor_glow::TextSpans::of_lines(&lines),
                ..Rig::default()
            };
            rig.w.tracer_up_stretch = stretch;
            rig.tick(Some(from), t0, &c, g);
            let ww = usize::from(g.win_w);
            let (mut shown, mut runs, mut gap) = (0usize, 0usize, i32::MAX);
            for at in [16u64, 40, 80] {
                rig.tick(Some(to), ms(t0, at), &c, g);
                // Both layers add premultiplied light: a pixel shows when the
                // light summed on it from the air strokes and the bed under
                // the letters reaches a visible step in any channel.
                let mut sum = vec![[0u32; 3]; ww * usize::from(g.win_h)];
                for q in rig.out.iter().chain(rig.under.iter()) {
                    for y in i32::from(q.y)..i32::from(q.y) + i32::from(q.h) {
                        for x in i32::from(q.x)..i32::from(q.x) + i32::from(q.w) {
                            let px = &mut sum[y as usize * ww + x as usize];
                            for (k, sh) in [16u32, 8, 0].into_iter().enumerate() {
                                px[k] += (q.color >> sh) & 0xFF;
                            }
                        }
                    }
                }
                let (cx0, cy0) = (i32::from(to.1) * cw - 2, i32::from(to.0) * ch - 2);
                let (cx1, cy1) = (cx0 + cw + 3, cy0 + ch + 3);
                let mut cols = vec![false; ww];
                for (i, px) in sum.iter().enumerate() {
                    let light = px.iter().copied().max().unwrap_or(0);
                    if light < 24 {
                        continue;
                    }
                    let (x, y) = ((i % ww) as i32, (i / ww) as i32);
                    if (cx0..=cx1).contains(&x) && (cy0..=cy1).contains(&y) {
                        continue;
                    }
                    shown += 1;
                    gap = gap.min((cx0 - x).max(x - cx1).max(cy0 - y).max(y - cy1));
                    cols[x as usize] = true;
                }
                let n = cols.windows(2).filter(|w| w[1] && !w[0]).count() + usize::from(cols[0]);
                runs = runs.max(n);
            }
            (shown, runs, gap)
        };
        let (shown, runs, gap) = laid(false);
        assert!(shown >= 200, "fixture: the tracer shows only {shown} px");
        assert_eq!(runs, 1, "the one-colour tracer falls into {runs} strokes");
        assert!(
            gap <= 2,
            "the one-colour tracer ends {gap} px short of the caret"
        );
        let (_, _, stretch_gap) = laid(true);
        assert!(
            stretch_gap >= 6,
            "control: kept to its longest piece the tracer ends {stretch_gap} px short of the caret"
        );
    }

    /// looks10 L6, round 1: the one-colour tracer up a dark page, run whole to the caret, was cut
    /// finer through the swept cells' centres as they stood, so it climbed in stairs, a flat in
    /// each row it crossed and a step between (`classic-pink/solarized-dark/classic-dark-move-up.up`),
    /// where looks9's straightened beam drew one slant. It is the line its straightened beam
    /// draws: each frame the core of each column it crosses between the two carets (the centre
    /// of the pixels at half its brightest or more) lies within two px of one straight line
    /// fitted through them all. Negative control: laid through the cells' centres, its columns
    /// stray four px or more from that line.
    #[test]
    fn a_one_colour_move_up_on_a_dark_page_is_one_slant() {
        let g = geom();
        let (cw, ch) = (g.cw, g.ch);
        let c = GlowConfig {
            classic_mono: true,
            color: 0x00D0_3A8C,
            accent: 0x00D0_3A8C,
            ..cfg()
        };
        let lines = [(4, 0, 1), (3, 10, 18), (2, 0, 34)];
        let (from, to) = ((4u16, 2u16), (2u16, 30u16));
        // (the least columns read in a frame, the most a column's core strays, px)
        let stray = |stairs: bool| -> (usize, f32) {
            let t0 = Instant::now();
            let mut rig = Rig {
                spans: crate::cursor_glow::TextSpans::of_lines(&lines),
                ..Rig::default()
            };
            rig.w.tracer_up_stairs = stairs;
            rig.tick(Some(from), t0, &c, g);
            let ww = usize::from(g.win_w);
            let (mut least, mut worst) = (usize::MAX, 0.0f32);
            for at in [16u64, 40, 80] {
                rig.tick(Some(to), ms(t0, at), &c, g);
                let mut light = vec![0u32; ww * usize::from(g.win_h)];
                for q in &rig.out {
                    for y in usize::from(q.y)..usize::from(q.y) + usize::from(q.h) {
                        for x in usize::from(q.x)..usize::from(q.x) + usize::from(q.w) {
                            let px = &mut light[y * ww + x];
                            *px += [16u32, 8, 0]
                                .into_iter()
                                .map(|sh| (q.color >> sh) & 0xFF)
                                .max()
                                .unwrap_or(0);
                        }
                    }
                }
                // The columns clear of both carets' cells and of the
                // landing round the new one, over the rows the move rises.
                let xs = (usize::from(from.1) + 2) * cw..(usize::from(to.1) - 3) * cw;
                let ys = usize::from(to.0) * ch..(usize::from(from.0) + 1) * ch;
                let mut cores = Vec::new();
                for x in xs {
                    let top = ys.clone().map(|y| light[y * ww + x]).max().unwrap_or(0);
                    if top < 24 {
                        continue;
                    }
                    let (mut sum, mut weight) = (0.0f32, 0.0f32);
                    for y in ys.clone() {
                        let l = light[y * ww + x];
                        if 2 * l >= top {
                            sum += y as f32 * l as f32;
                            weight += l as f32;
                        }
                    }
                    cores.push((x as f32, sum / weight));
                }
                least = least.min(cores.len());
                let k = cores.len() as f32;
                let (mx, my) = cores
                    .iter()
                    .fold((0.0, 0.0), |(a, b), &(x, y)| (a + x / k, b + y / k));
                let (sxy, sxx) = cores.iter().fold((0.0, 0.0), |(a, b), &(x, y)| {
                    (a + (x - mx) * (y - my), b + (x - mx) * (x - mx))
                });
                let slope = sxy / sxx;
                for &(x, y) in &cores {
                    worst = worst.max((y - (my + slope * (x - mx))).abs());
                }
            }
            (least, worst)
        };
        let (read, straight) = stray(false);
        assert!(read >= 24, "fixture: a frame reads only {read} columns");
        assert!(
            straight <= 2.0,
            "the one-colour tracer up a page strays {straight:.1} px from a line"
        );
        let (read, stairs) = stray(true);
        assert!(
            read >= 24,
            "control fixture: a frame reads only {read} columns"
        );
        assert!(
            stairs >= 4.0,
            "control: laid through the cells' centres the tracer strays only {stairs:.1} px"
        );
    }

    /// looks10 L6, round 2: on Solarized Dark a letter keeps 97% of its own reading under the
    /// lights, so the guard cut the one-colour tracer up a page off every lettered row it rose
    /// past, and only its stretch in the blank row showed, a glaring slash attached to neither
    /// caret (`classic-pink/solarized-dark/classic-dark-move-up.up`, 56-59 px short). Through the
    /// letters it is laid under them at the page's own luminance, one stroke on to the caret's
    /// cell, and its light in the air is a share of its whole, not a white-hot core. Negative
    /// controls: laid as round 1 laid it, nothing lies under the letters and the trace stops
    /// short; kept whole in the air, its core burns brighter than the share.
    #[test]
    fn a_one_colour_move_up_on_a_dim_page_runs_under_the_letters_to_the_caret() {
        let g = geom();
        let (cw, ch) = (g.cw as i32, g.ch as i32);
        let page = 0x0000_2B36;
        let c = GlowConfig {
            classic_mono: true,
            color: 0x00D0_3A8C,
            accent: 0x00D0_3A8C,
            theme_bg: page,
            theme_fg: 0x0083_9496,
            ..cfg()
        };
        assert!(crate::rainbow_kitty::ribbon::dim_theme(c.theme_fg, page));
        // From a short prompt up past a blank row into a long line's letters.
        let lines = [(4, 0, 5), (2, 0, 34)];
        let (from, to) = ((4u16, 6u16), (2u16, 30u16));
        let page_lum = relative_luminance(page);
        // (under-letter quads at the page's luminance on row 2's letters, their least gap to the
        // caret's cell, the brightest channel the air light reaches)
        let laid = |unshaded: bool, whole: bool| -> (usize, i32, u32) {
            let t0 = Instant::now();
            let mut rig = Rig {
                spans: crate::cursor_glow::TextSpans::of_lines(&lines).with_blank_row(3),
                ..Rig::default()
            };
            rig.w.tracer_up_unshaded = unshaded;
            rig.w.tracer_up_whole = whole;
            rig.tick(Some(from), t0, &c, g);
            let (mut on_letters, mut gap, mut air_peak) = (0usize, i32::MAX, 0u32);
            let (cx0, cy0) = (i32::from(to.1) * cw, i32::from(to.0) * ch);
            let (cx1, cy1) = (cx0 + cw, cy0 + ch);
            for at in [16u64, 40, 80] {
                rig.tick(Some(to), ms(t0, at), &c, g);
                for q in &rig.under {
                    if q.alpha < 48 {
                        continue;
                    }
                    let k = 255.0 / f32::from(q.alpha);
                    let un =
                        |sh: u32| ((((q.color >> sh) & 0xFF) as f32 * k).round() as u32).min(255);
                    let rgb = (un(16) << 16) | (un(8) << 8) | un(0);
                    let lum = relative_luminance(rgb);
                    if (lum - page_lum).abs() > 0.25 * page_lum {
                        continue;
                    }
                    let (x0, y0) = (i32::from(q.x), i32::from(q.y));
                    let (x1, y1) = (x0 + i32::from(q.w), y0 + i32::from(q.h));
                    if y0 >= 2 * ch && y1 <= 3 * ch && x1 > 20 * cw && x0 < 30 * cw {
                        on_letters += 1;
                    }
                    gap = gap.min((cx0 - x1).max(x0 - cx1).max(cy0 - y1).max(y0 - cy1).max(0));
                }
                for q in &rig.out {
                    let (x0, y0) = (i32::from(q.x), i32::from(q.y));
                    let x1 = x0 + i32::from(q.w);
                    // The blank row's air.
                    if y0 >= 3 * ch && y0 < 4 * ch && x1 > 12 * cw && x0 < 24 * cw {
                        let top = [16u32, 8, 0]
                            .into_iter()
                            .map(|sh| (q.color >> sh) & 0xFF)
                            .max()
                            .unwrap_or(0);
                        air_peak = air_peak.max(top);
                    }
                }
            }
            (on_letters, gap, air_peak)
        };
        let (on_letters, gap, air) = laid(false, false);
        assert!(
            on_letters >= 3,
            "the tracer lays only {on_letters} quads under row 2's letters"
        );
        assert!(
            gap <= 2,
            "the tracer stops {gap} px short of the caret's cell"
        );
        let (bare, bare_gap, _) = laid(true, false);
        assert_eq!(bare, 0, "control: round 1 laid nothing under the letters");
        assert!(
            bare_gap > 2,
            "control: round 1's trace reached within {bare_gap} px of the caret"
        );
        let (_, _, hot) = laid(false, true);
        assert!(air > 0, "fixture: the tracer lights the blank row's air");
        assert!(
            (air as f32) < 0.75 * hot as f32,
            "the air keeps {air} of the whole {hot}: a share, not the white-hot core"
        );
    }

    /// looks10 L6, round 2: on a dim page the one-colour tracer up a page lays none of its light
    /// over the letters it rises past; they are its shade's. Its fifth there took no letter's
    /// light, and the letter guard cuts every light over a cell to the one share its letter
    /// keeps, so it cut the shade with it to a trace of 5 bytes on the page and the slash in the
    /// blank row stood alone again (`classic-pink/solarized-dark/classic-dark-move-up.up`).
    /// Negative control: its fifth kept over the letters (`tracer_up_lit_letters`), the light
    /// lies on row 2's letters.
    #[test]
    fn a_one_colour_move_up_on_a_dim_page_leaves_its_letters_to_the_shade() {
        let g = geom();
        let (cw, ch) = (g.cw as i32, g.ch as i32);
        let page = 0x0000_2B36;
        let c = GlowConfig {
            classic_mono: true,
            color: 0x00D0_3A8C,
            accent: 0x00D0_3A8C,
            theme_bg: page,
            theme_fg: 0x0083_9496,
            ..cfg()
        };
        let lines = [(4, 0, 5), (2, 0, 34)];
        let (from, to) = ((4u16, 6u16), (2u16, 30u16));
        // (the light quads on row 2's letters off the caret's cell, the shade's quads there)
        let laid = |lit_letters: bool| -> (usize, usize) {
            let t0 = Instant::now();
            let mut rig = Rig {
                spans: crate::cursor_glow::TextSpans::of_lines(&lines).with_blank_row(3),
                ..Rig::default()
            };
            rig.w.tracer_up_lit_letters = lit_letters;
            rig.tick(Some(from), t0, &c, g);
            let (mut light, mut shade) = (0usize, 0usize);
            let on_letters = |q: &GlowQuad| {
                let (x0, y0) = (i32::from(q.x), i32::from(q.y));
                let (x1, y1) = (x0 + i32::from(q.w), y0 + i32::from(q.h));
                y0 >= 2 * ch && y1 <= 3 * ch && x1 > 18 * cw && x0 < 29 * cw
            };
            for at in [16u64, 40, 80] {
                rig.tick(Some(to), ms(t0, at), &c, g);
                light += rig
                    .out
                    .iter()
                    .filter(|q| on_letters(q))
                    .filter(|q| {
                        [16u32, 8, 0]
                            .into_iter()
                            .any(|sh| ((q.color | q.color2) >> sh) & 0xFF > 4)
                    })
                    .count();
                shade += rig
                    .under
                    .iter()
                    .filter(|q| on_letters(q) && q.alpha >= 48)
                    .count();
            }
            (light, shade)
        };
        let (light, shade) = laid(false);
        assert!(
            shade >= 3,
            "fixture: the shade lays only {shade} quads on row 2's letters"
        );
        assert_eq!(
            light, 0,
            "the tracer lays {light} quads of light on row 2's letters"
        );
        let (lit, _) = laid(true);
        assert!(
            lit > 0,
            "control: with its fifth kept the light lies on the letters"
        );
    }

    #[test]
    fn the_wake_rides_a_scroll_and_a_band_move() {
        let g = Geom {
            rows: 14,
            win_h: 14 * 16,
            ..geom()
        };
        let c = cfg();
        let t0 = Instant::now();
        let on_prompt_row = |rig: &Rig| {
            rig.w
                .sparks
                .iter()
                .filter(|s| s.row == 13 && (6..=15).contains(&s.col))
                .count()
        };
        let enter = |carry: &dyn Fn(&mut ClassicWake)| {
            let mut rig = Rig::default();
            rig.tick(Some((13, 15)), t0, &c, g);
            carry(&mut rig.w);
            rig.tick(Some((13, 2)), ms(t0, 16), &c, g);
            rig
        };
        let scroll = |w: &mut ClassicWake| w.translate_rows(&|row: u16| row.checked_sub(2));
        let band = |w: &mut ClassicWake| {
            w.translate_rows(&|row| crate::cursor_glow::band_row(row, 0, 13, -2));
        };
        for (name, carry) in [
            ("scroll", &scroll as &dyn Fn(&mut ClassicWake)),
            ("band", &band),
        ] {
            let rig = enter(carry);
            assert_eq!(
                on_prompt_row(&rig),
                0,
                "{name}: a comet along the new prompt row"
            );
            assert!(
                rig.w.sparks.iter().any(|s| s.row == 11),
                "{name}: the comet starts where the command's text now is"
            );
            let ring = rig.w.ring.expect("the Enter is a jump");
            assert_eq!(
                (ring.row, ring.col),
                (13, 2),
                "{name}: the ring sits on the landing cell"
            );
        }
        let untold = enter(&|_: &mut ClassicWake| {});
        assert!(
            on_prompt_row(&untold) > 0,
            "control: untold, the move reads as a horizontal jump on the prompt row"
        );
    }

    #[test]
    fn a_scroll_drops_what_it_carries_off_the_top() {
        let g = geom();
        let c = GlowConfig {
            radius: 0.0,
            ..cfg()
        };
        let t0 = Instant::now();
        let mut rig = Rig::default();
        rig.tick(Some((0, 4)), t0, &c, g);
        rig.tick(Some((1, 30)), ms(t0, 16), &c, g);
        assert!(
            rig.w.ring.is_some() && !rig.w.sparks.is_empty(),
            "fixture: lit"
        );
        rig.w.translate_rows(&|row: u16| row.checked_sub(2));
        assert!(
            rig.w.sparks.is_empty(),
            "sparks carried off the top are gone"
        );
        assert!(rig.w.ring.is_none(), "so is the ring");
        assert!(!rig.w.has_last(), "and the caret memory");
        let fp = rig.tick(Some((3, 10)), ms(t0, 32), &c, g);
        assert!(rig.w.sparks.is_empty(), "no comet from a forgotten cell");
        assert_eq!(fp, 0, "nothing in flight, nothing spawned");
    }

    #[test]
    fn reset_forgets_the_cursor_so_no_cross_space_comet_spawns() {
        let (g, c) = (geom(), cfg());
        let mut rig = Rig::default();
        let t0 = Instant::now();
        rig.tick(Some((2, 4)), t0, &c, g);
        rig.w.reset();
        assert!(!rig.w.is_active());
        let fp = rig.tick(Some((5, 30)), ms(t0, 16), &c, g);
        assert_eq!(fp, 0, "the first tick after a reset only re-observes");
        assert!(rig.out.is_empty());
    }

    #[test]
    fn fingerprint_remaps_zero_fold_on_additive_quads_to_one() {
        let mut out_q = Vec::new();
        let mut out_r = aterm_render::GlowRamps::default();
        let mut under_q = Vec::new();
        let mut under_r = aterm_render::GlowRamps::default();
        assert_eq!(
            ClassicWake::fingerprint(
                &mut crate::cursor_glow::glow_sink(&mut out_q, &mut out_r, true),
                &mut crate::cursor_glow::glow_sink(&mut under_q, &mut under_r, true),
                &[],
            ),
            0,
            "all streams empty -> 0"
        );
        // (1 << 40) ^ (0x0010_0000 << 20) == 0 and (1 << 8) ^ 256 == 0, so the
        // raw polynomial term is 0 even though `out` holds a visible quad.
        out_q.push(GlowQuad {
            row: 1,
            x: 0,
            y: 0,
            w: 1,
            h: 256,
            color: 0x0010_0000,
            alpha: 0,
            color2: 0x0010_0000,
            alpha2: 0,
        });
        assert_eq!(
            ClassicWake::fingerprint(
                &mut crate::cursor_glow::glow_sink(&mut out_q, &mut out_r, true),
                &mut crate::cursor_glow::glow_sink(&mut under_q, &mut under_r, true),
                &[],
            ),
            1,
            "any laid additive quad remaps a zero fold to 1"
        );
    }
}
