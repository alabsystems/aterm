// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE QUAD ORACLE: the card as it was drawn before fx layers, a stream of
//! [`GlowQuad`]s each inside one cell-row band, kept for the tests
//! (`docs/DESIGN-card-engine-2026-10-19.md` §4.9, migration exactness). The
//! layers are held to it pixel for pixel
//! (`the_card_layers_equal_the_legacy_quads`), so every law its tests prove
//! of the quads holds of the layers. Its one change from the shipped
//! emitter is the trail held still from a rim's width short of the kitty
//! (`TrailLay::held`), which both draw. Its budget is gone: it lays every
//! mark of a frame, whole.

use aterm_render::{GlowQuad, premul_rgb};
use aterm_time::Instant;

use super::{
    Clock, Emit, Geo, HarnessCard, KITTY_PALETTE, KittyInk, Paint, Ramps, Rect, STAR_PRINT_FULL,
    WHITE, clip_ramp, ink, lay_cut, light, rim_px, rim_strips,
};
use crate::effect_util::{STAR_CORE_ADD, STAR_GLINT_COV, StarPart, star_pieces};
use crate::genome::mix;

/// The one way a quad leaves the oracle: clipped, split at every row band
/// and tagged with its row.
#[derive(Clone, Copy, Debug)]
pub(super) struct Sink {
    pub(super) geo: Geo,
}

impl Sink {
    /// Push `r` (clipped to `clip` and the box) as a horizontal ramp from `l`
    /// at its left edge to `rt` at its right; split at every row band.
    pub(super) fn push(&self, out: &mut Vec<GlowQuad>, clip: Rect, r: Rect, l: Paint, rt: Paint) {
        // Additive zero light adds nothing; a source-over quad still darkens.
        if l.alpha == 0 && l.color == 0 && rt.color == 0 {
            return;
        }
        let c = r.meet(clip).meet(self.geo.clip);
        if c.is_empty() {
            return;
        }
        let (l, rt) = clip_ramp(r, c, l, rt);
        let g = self.geo;
        let mut y = c.y0;
        while y < c.y1 {
            let band = (y - g.oy).div_euclid(g.ch);
            let end = g.band(band + 1).min(c.y1);
            if let Ok(row) = u16::try_from(g.row_base + band) {
                // In range: `c` lies inside the u16 pixel space.
                out.push(GlowQuad::gradient(
                    row,
                    c.x0 as u16,
                    y as u16,
                    (c.x1 - c.x0) as u16,
                    (end - y) as u16,
                    l.color,
                    l.alpha,
                    rt.color,
                    rt.alpha,
                ));
            }
            y = end;
        }
    }

    /// [`Self::push`] for one flat colour.
    pub(super) fn flat(&self, out: &mut Vec<GlowQuad>, clip: Rect, r: Rect, p: Paint) {
        self.push(out, clip, r, p, p);
    }
}

/// The border's and the trail's ramps as quads.
pub(super) struct QuadSink<'a> {
    pub(super) sink: Sink,
    pub(super) out: &'a mut Vec<GlowQuad>,
}

impl Ramps for QuadSink<'_> {
    fn ramp(&mut self, clip: Rect, r: Rect, l: Paint, rt: Paint) {
        self.sink.push(self.out, clip, r, l, rt);
    }

    fn side(&mut self, clip: Rect, r: Rect, p: Paint) {
        self.sink.flat(self.out, clip, r, p);
    }
}

/// Lay the flat piece `r`, clipped to `clip`, in `paint`, but where it lies
/// on `on_trail` (a light frame's trail ink, segment by segment) in `glint`:
/// on the trail one quad a run of segments that share the piece's rows, off
/// it the piece less every segment ([`lay_cut`]).
pub(super) fn lay_toned(
    out: &mut Vec<GlowQuad>,
    sink: &Sink,
    clip: Rect,
    r: Rect,
    (paint, glint): (Paint, Paint),
    on_trail: &[Rect],
) {
    if on_trail.is_empty() {
        sink.flat(out, clip, r, paint);
        return;
    }
    let q = r.meet(clip);
    if q.is_empty() {
        return;
    }
    let lo = on_trail.partition_point(|c| c.x1 <= q.x0);
    let span = on_trail[lo..].partition_point(|c| c.x0 < q.x1);
    let on_trail = &on_trail[lo..lo + span];
    let Some(last) = on_trail.len().checked_sub(1) else {
        sink.flat(out, clip, q, paint);
        return;
    };
    let mut run: Option<Rect> = None;
    for c in on_trail.iter().map(|c| c.meet(q)).filter(|c| !c.is_empty()) {
        run = match run {
            Some(p) if (p.y0, p.y1, p.x1) == (c.y0, c.y1, c.x0) => Some(Rect { x1: c.x1, ..p }),
            Some(p) => {
                sink.flat(out, clip, p, glint);
                Some(c)
            }
            None => Some(c),
        };
    }
    if let Some(p) = run {
        sink.flat(out, clip, p, glint);
    }
    lay_cut(q, 0, last as i64, &|k| on_trail[k as usize], &mut |piece| {
        sink.flat(out, clip, piece, paint);
        true
    });
}

/// One four-point star of [`star_pieces`], clipped to `clip`: additive light
/// on dark; on light `color` is the print ink it is laid in, every piece
/// source-over, opaque from a settled star's coverage up
/// ([`STAR_PRINT_FULL`]) and the nucleus with the body, and the paper's
/// white where a piece lies on the trail's ink (`on_trail`, [`lay_toned`];
/// empty on dark).
#[allow(
    clippy::too_many_arguments,
    reason = "output + sink + clip + centre + arm + coverage + glint + colour + the \
              trail's ink + theme: the push_twinkle_star call with its clip, tones and theme"
)]
pub(super) fn lay_star(
    out: &mut Vec<GlowQuad>,
    sink: &Sink,
    clip: Rect,
    (sx, sy): (i64, i64),
    arm: i64,
    cov: f32,
    gold: bool,
    color: u32,
    on_trail: &[Rect],
    dark: bool,
) {
    if cov < 1.0 || arm < 1 {
        return;
    }
    // One piece of the star at coverage `c` (0..255): in its colour, and in
    // the paper's white where it lies on the trail's ink.
    let tone = |rgb: u32, c: f32| {
        if dark {
            light(rgb, c)
        } else {
            ink(rgb, c * 255.0 / STAR_PRINT_FULL)
        }
    };
    let paint = |c: f32| (tone(color, c), tone(WHITE, c));
    let full = paint(cov.floor());
    let mut ended = false;
    let arm32 = i32::try_from(arm).unwrap_or(i32::MAX);
    for piece in star_pieces(arm32, gold).iter() {
        let p = match piece.part {
            StarPart::Body => full,
            // Additive, the crossing supplies part of the nucleus; ink lays
            // it whole.
            StarPart::Core if dark => paint((cov * STAR_CORE_ADD).floor()),
            StarPart::Core => full,
            StarPart::Tip { taper, .. } => {
                let tip_cov = (cov * taper).floor();
                ended |= dark && premul_rgb(color, tip_cov as u8) == 0;
                if ended {
                    continue;
                }
                paint(tip_cov)
            }
            // Dim light on dark. On paper the star's own ink at the body's
            // strength: a third of it let the paper through, a pale halo.
            StarPart::Glint if dark => paint((cov * STAR_GLINT_COV).floor()),
            StarPart::Glint => full,
        };
        let r = Rect {
            x0: sx + i64::from(piece.x),
            y0: sy + i64::from(piece.y),
            x1: sx + i64::from(piece.x + piece.w),
            y1: sy + i64::from(piece.y + piece.h),
        };
        lay_toned(out, sink, clip, r, p, on_trail);
    }
}

/// The kitty: its frame's art as opaque source-over rectangles — each run of
/// one colour along a pixel row, carried down the rows under it that repeat
/// it exactly (the same columns and colour, neither neighbour that colour) —
/// split at the row bands they cross. The authored art's runs are compared
/// with the production sprite; this quad count is not a production budget.
pub(super) fn kitty_quads(cx: &Emit<'_>, sink: &Sink, out: &mut Vec<GlowQuad>) {
    let Some((at, pose)) = cx.kitty else {
        return;
    };
    let frame = at.art.frame(pose);
    let inks = KittyInk::new(cx.scene.coat, cx.scene.eye, cx.scene.dark);
    let p = at.p;
    for (r, row) in frame.iter().enumerate() {
        let px = row.as_bytes();
        let mut c = 0;
        while c < px.len() {
            let ch = px[c];
            let run = px[c..].iter().take_while(|&&b| b == ch).count();
            // Row `other` holds this very run.
            let repeats = |other: &str| {
                let o = other.as_bytes();
                o[c..c + run].iter().all(|&b| b == ch)
                    && (c == 0 || o[c - 1] != ch)
                    && o.get(c + run).is_none_or(|&b| b != ch)
            };
            let carried = r > 0 && repeats(frame[r - 1]);
            if let (false, Some(rgb)) = (carried, inks.of(ch)) {
                let down = frame[r + 1..]
                    .iter()
                    .take_while(|other| repeats(other))
                    .count();
                let q = Rect {
                    x0: at.rect.x0 + c as i64 * p,
                    y0: at.rect.y0 + r as i64 * p,
                    x1: at.rect.x0 + (c + run) as i64 * p,
                    y1: at.rect.y0 + (r + 1 + down) as i64 * p,
                };
                let paint = Paint {
                    color: rgb,
                    alpha: 255,
                };
                sink.flat(out, cx.geo.clip, q, paint);
            }
            c += run;
        }
    }
    debug_assert!(KITTY_PALETTE[0] == b'.');
}

/// The rim as quads: each strip ([`rim_strips`]) in its paint
/// ([`Emit::rim_paint`]), clipped to the box inside the border's stroke and
/// cut, segment by segment, where it would lie on the trail.
pub(super) fn rim_quads(cx: &Emit<'_>, sink: &Sink, out: &mut Vec<GlowQuad>) {
    let Some((at, pose)) = cx.kitty else {
        return;
    };
    let (p, rim) = (at.p, rim_px(at.p, cx.geo.t));
    if rim == 0 {
        return;
    }
    let frame = at.art.frame(pose);
    let r0 = at.rect;
    let centre = ((r0.x0 + r0.x1) as f32 * 0.5, (r0.y0 + r0.y1) as f32 * 0.5);
    let clip = cx.geo.inner();
    let trail = cx.trail_lay();
    rim_strips(frame, p, rim, |q| {
        let q = q.shift(r0.x0, r0.y0);
        let mid = ((q.x0 + q.x1) as f32 * 0.5, (q.y0 + q.y1) as f32 * 0.5);
        let paint = cx.rim_paint(mid, centre);
        let mut lay = |piece: Rect| {
            sink.flat(out, clip, piece, paint);
            true
        };
        // The trail's segments under the strip, if any.
        match trail {
            Some(t) if q.x0 < t.drawn && q.x1 > t.tx0 => {
                let first = (q.x0 - t.tx0).max(0) / t.seg;
                let last = (q.x1.min(t.drawn) - 1 - t.tx0) / t.seg;
                lay_cut(q, first, last, &|k| cx.trail_cover(&t, k), &mut lay);
            }
            _ => {
                lay(q);
            }
        }
    });
}

/// Every quad of `cx`'s frame: the border and (but in serious mode) the
/// trail into `under`, the kitty, its rim and the stars into `over`.
pub(super) fn quads(cx: &Emit<'_>, under: &mut Vec<GlowQuad>, over: &mut Vec<GlowQuad>) {
    let sink = Sink { geo: cx.geo };
    cx.border(&mut QuadSink { sink, out: under });
    if cx.scene.serious {
        return;
    }
    cx.trail(&mut QuadSink { sink, out: under });
    kitty_quads(cx, &sink, over);
    rim_quads(cx, &sink, over);
    let trail = cx.trail_ink();
    let dark = cx.scene.dark;
    cx.each_star(&mut |s| {
        lay_star(
            over,
            &sink,
            s.clip,
            s.centre,
            s.arm,
            s.cov,
            s.gold,
            s.color,
            trail.covers(),
            dark,
        );
    });
}

impl HarnessCard {
    /// The oracle's frame: [`Self::draw`]'s step, its quads appended to
    /// `under` and `over`, and the frame's fingerprint returned — `0` when
    /// nothing was drawn; otherwise a hash of every quad appended, folded
    /// with the frame index while an intro animates and with the step in
    /// the flow.
    pub(super) fn tick(
        &mut self,
        now: Instant,
        scene: &super::CardScene,
        animate: bool,
        under: &mut Vec<GlowQuad>,
        over: &mut Vec<GlowQuad>,
    ) -> u64 {
        let geo = Geo::new(scene);
        let Some(clock) = self.step(now, scene, animate, geo.is_some()) else {
            return 0;
        };
        let Some(geo) = geo else {
            return 0;
        };
        let (u0, o0) = (under.len(), over.len());
        quads(&Emit::new(scene, geo, clock, self.seed), under, over);
        frame_fp(&under[u0..], &over[o0..], clock)
    }
}

/// A frame's fingerprint ([`HarnessCard::tick`]): `0` when it holds no
/// quad; otherwise a hash of every quad, `under`'s then `over`'s, folded
/// with the frame index while an intro animates and with the step in the
/// flow (`clock`).
pub(super) fn frame_fp(under: &[GlowQuad], over: &[GlowQuad], clock: Clock) -> u64 {
    if under.is_empty() && over.is_empty() {
        return 0;
    }
    let mut h = mix(0x4841_524E_4553_5321 ^ under.len() as u64);
    for q in under.iter().chain(over) {
        for w in q.damage_words() {
            h = mix(h ^ w);
        }
    }
    if let Some(t) = clock.t_ms {
        h = mix(h ^ (1 + t / super::FRAME_MS));
    }
    if let Some(f) = clock.flow {
        h = mix(h ^ (0xF10F ^ f.step));
    }
    h
}
