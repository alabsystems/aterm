// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Vertex planning for one contiguous rainbow-ribbon run.
//!
//! This is deliberately separate from `CursorGlow`'s ownership/cap loop: it
//! only turns already-resolved cells into the exact polyline that the shared
//! ribbon rasterizer consumes. Keeping it pure makes the head-first budget and
//! resident scratch ownership visible at the emitter call site.

use aterm_render::{RIBBON_LIFT_DN_MELT, RibbonVertex};

use super::{
    Geom, RAINBOW_RUN_TAIL_EASE, RibbonCell, rainbow_bed_cap_at, rainbow_bed_ink,
    rainbow_cycle_ink, rainbow_cycle_phase, rainbow_dn_ledge_room, rainbow_spine_lift,
};

/// Rebuild `verts` for one contiguous range in the sorted cell index.
///
/// The caller owns the run discovery, capacity budget, and eventual raster. This
/// function owns only boundary sampling: outer boundaries stay flat at their
/// cell's spectrum position, internal boundaries use the midpoint, and each
/// slab resolves its colour from that position rather than interpolating sRGB
/// chord colours through cyan.
///
/// `bed_ground` is the theme background the source-over bed displaces (`None`
/// on a light theme) and `strip_span` the baseline strip's span above the
/// spine, both resolved once for the whole mark by the caller.
#[allow(clippy::too_many_arguments)]
pub(super) fn plan_run(
    cells: &[RibbonCell],
    sorted: &[(u16, u16, u32)],
    s_lo: usize,
    s_hi: usize,
    seed_col: u16,
    geom: Geom,
    slabs: usize,
    bed_ground: Option<u32>,
    strip_span: f32,
    verts: &mut Vec<RibbonVertex>,
) {
    let (lo, hi) = (sorted[s_lo].1, sorted[s_hi].1);
    // The run's cell at column `col`, or `None` one step past either end —
    // the same answers the old per-column tree probe gave.
    let run_cell = |col: u16| -> Option<&RibbonCell> {
        (lo..=hi)
            .contains(&col)
            .then(|| &cells[sorted[s_lo + usize::from(col - lo)].2 as usize])
    };

    // THE BED'S ON-GLASS INK ([`rainbow_bed_ink`]): the glass luma floor,
    // which lifts a dark arc colour toward white until a full lay composites
    // above the ground it displaces, and then the hue GIVE-BACK, which turns
    // the ink so the composite wears the arc's own hue instead of the ground's
    // lean. `None` (a light theme) keeps the pure arc — there the bed's
    // currency is darkening and there is no theft.
    //
    // THE COVERAGE IS PART OF THE COLOUR, which is why this takes the pair:
    // how much hue the ground steals is how much of the pixel it keeps, so the
    // ink a slab wants at its own ceiling is not the ink the feathered tail
    // beside it wants. Both push sites below resolve `ink` first.
    // THE POSITION HANDED IN IS THE MONOTONE WALK, not a folded phase — the
    // planner interpolates it between neighbouring cells, and interpolating a
    // folded value would sweep the arc BACKWARDS across the wrap (violet →
    // blue → green → … → red, through cyan) in one cell. Folded here and only
    // here, so the interpolation stays monotone in walk space and the wrap
    // renders as the forward violet→magenta→red leg it is.
    // Folded ONCE per read: `rainbow_cycle_ink` folds its own argument, and
    // handing it a phase that is already inside one turn takes the fold's fast
    // path instead of doing the work twice per slab.
    let bed = |p: f32| {
        let phase = rainbow_cycle_phase(p);
        rainbow_bed_ink(rainbow_cycle_ink(phase), phase, bed_ground)
    };

    // ONE VERTEX PER SLAB, at the stride the beam tiles with, so the polyline
    // covers the run exactly, needs no end caps, and costs the SAME QUADS —
    // the beam walks `windows(2)` and tiles each segment by `step`, so a
    // segment that is already one slab wide emits one slab.
    verts.clear();
    verts.reserve((usize::from(hi - lo) + 2) * slabs);
    let mut last_pos = 0.0f32;
    let mut last_lit = 0.0f32;
    let mut last_gate = 1.0f32;
    let mut last_flow = 1.0f32;
    let cell_width = geom.cw as f32;

    // **THE LIVING-FLOW SWELL, SPENT AFTER THE CEILING** ([`RibbonCell::flow`]).
    //
    // A gain in `[1 − depth, 1]` on the coverage that has ALREADY been clamped
    // to its legibility ceiling, floored so it can never take a slab the mark
    // admitted back under the raster floor and open a hole in the middle of it.
    // Two properties follow, and they are the whole reason the channel is safe
    // to add to a surface this heavily certified:
    //
    // * **IT SPENDS NO LIGHT.** `emitted = min(request, cap, room) × gain` with
    //   `gain ≤ 1`, so every ceiling this planner applies — the bed's
    //   `rainbow_bed_cap_at`, the dn-side `rainbow_dn_ledge_room`, and through
    //   `ink` the strip's own `rainbow_spine_lift` — bounds the emitted value
    //   exactly as it did before, with no cap re-derived and no certificate
    //   re-signed.
    // * **IT CANNOT RE-COLOUR.** `color` is `bed(p)`, a pure function of the
    //   position, and the gain is not an argument to it. The mark's hue at
    //   every slab is bit-for-bit the hue it was before the flow existed.
    let flowed = |ink: f32, flow: f32| {
        let dipped = ink * flow;
        if ink >= 1.0 { dipped.max(1.0) } else { dipped }
    };

    // Walked in `u32` so the one-past-the-end boundary cannot wrap when the run
    // reaches `u16::MAX`.
    for boundary in u32::from(lo)..=u32::from(hi) + 1 {
        let left = boundary
            .checked_sub(1)
            .and_then(|col| u16::try_from(col).ok())
            .and_then(&run_cell);
        let right = u16::try_from(boundary).ok().and_then(&run_cell);
        let (a, b) = match (left, right) {
            (Some(a), Some(b)) => (a, b),
            (Some(a), None) => (a, a),
            (None, Some(b)) => (b, b),
            (None, None) => continue,
        };
        let mid = |u: f32, v: f32| (u + v) * 0.5;
        // THE FIELD AT THE BOUNDARY. Between two cells it is the midpoint of
        // their positions, read from the one field rather than mixed here
        // (§2.1: every layer reads the position out of the cell it covers).
        //
        // A run's outer edge reads its own cached position flat. It never
        // extrapolates into another run or consults the ancillary rolling hue,
        // phase, fold, or dwell laws.
        let pos = match (left, right) {
            // Both cells are already in this resolved run: the boundary is the
            // midpoint of their classic positions.
            (Some(a), Some(b)) => (a.pos + b.pos) * 0.5,
            // A run's outer edge reads its own cell flat — the classic field
            // has no walk to continue past the mark's end.
            (Some(a), None) => a.pos,
            (None, Some(b)) => b.pos,
            (None, None) => continue,
        };
        // **ONE VERTEX PER SLAB, AND THE POSITION IS WHAT RAMPS.**
        //
        // The beam interpolates a vertex's COLOUR to its neighbour's in sRGB,
        // which is a CHORD across the arc — the same operation whose
        // green->blue midpoint was defect (b) of the design of record. Over one
        // keystroke that chord is safe on the whole curve except in ONE place,
        // and it is the place the arc is deliberately fastest:
        // [`crate::spectrum::spectrum`] rushes through cyan at `0.30` of its
        // rate elsewhere, and a straight line between the two colours either
        // side of that rush walks straight back across it. MEASURED: the arc
        // dwells `3.51 %` of its parameter inside the design's cyan window; the
        // chord chain the beam paints at the shipped lay rate dwells
        // **7.47 %**, and on glass the emitted mark reads `5.17 %` of its
        // coloured area — over §2.3.4's `4 %` bound the arc itself is
        // comfortably inside.
        //
        // So the mark ramps its POSITION and resolves its colour from the arc
        // at every slab. The boundary vertices are unchanged bit for bit — same
        // `pos`, same geometry, same cap — and what used to be a chord between
        // them is now the curve.
        let prev = verts.last().map(|v: &RibbonVertex| (v.x, last_pos));
        let x = f32::from(geom.origin_x) + boundary as f32 * cell_width;
        let spine = mid(a.band.spine, b.band.spine);
        let up = mid(a.band.up, b.band.up);
        let dn = mid(a.band.dn, b.band.dn);
        let core_up = mid(a.band.core_up, b.band.core_up);
        let core_dn = mid(a.band.core_dn, b.band.core_dn);
        let lit = mid(a.lit, b.lit);
        let gate = mid(a.lift_gate, b.lift_gate);
        // The swell is read at the boundary the same way every other per-cell
        // scalar here is — the midpoint between the two cells it separates, a
        // run's outer edge flat — so it ramps ACROSS a cell rather than
        // stepping at its edge, and two neighbours' gains can never cross.
        let flow = mid(a.flow, b.flow);
        if let Some((px, ppos)) = prev {
            let back = verts.len() - 1;
            let (pspine, pup, pdn, pcu, pcd, plit, pgate, pflow) = (
                verts[back].spine,
                verts[back].up,
                verts[back].dn,
                verts[back].core_up,
                verts[back].core_dn,
                last_lit,
                last_gate,
                last_flow,
            );
            // The first segment of the run carries the tail ease's ramp back to
            // full ([`RAINBOW_RUN_TAIL_EASE`]).
            let seg_ease0 = if boundary == u32::from(lo) + 1 {
                RAINBOW_RUN_TAIL_EASE
            } else {
                1.0
            };
            for j in 1..slabs {
                let f = j as f32 / slabs as f32;
                let l = |u: f32, v: f32| u + (v - u) * f;
                let p = l(ppos, pos);
                let dn_j = l(pdn, dn);
                // The dn-side descent carries the body's melt AND the strip's
                // own — both are priced against the one room
                // ([`rainbow_dn_ledge_room`]) so their overlapping slopes
                // cannot compose a hard edge.
                let room = rainbow_dn_ledge_room(p, dn_j);
                let ink = flowed(
                    (l(plit, lit) * l(seg_ease0, 1.0))
                        .min(rainbow_bed_cap_at(p))
                        .min(room),
                    l(pflow, flow),
                );
                let lift = rainbow_spine_lift(ink, strip_span, dn_j * RIBBON_LIFT_DN_MELT)
                    .min((RIBBON_LIFT_DN_MELT * (room - ink)).max(0.0));
                verts.push(RibbonVertex {
                    // ROUNDED TO THE PIXEL LATTICE. The beam tiles each
                    // vertex-pair segment into integer pixel columns, and a
                    // fractional slab vertex (cw = 14 at Retina puts the
                    // thirds at 4.67 and 9.33) made the two segments either
                    // side of it CO-OWN the boundary column — the bed
                    // composites source-over, so that column composited twice:
                    // a static ~1.8×-bright 1-px comb at every interior slab
                    // vertex of every typed cell (the owner's "weird vertical
                    // line issue with the rainbow", 2026-08-31). On the pixel
                    // lattice the segments tile half-open and every column has
                    // one owner; the slab's colour sample moves by under half
                    // a device pixel.
                    x: l(px, x).round(),
                    spine: l(pspine, spine),
                    up: l(pup, up),
                    dn: dn_j,
                    core_up: l(pcu, core_up),
                    core_dn: l(pcd, core_dn),
                    color: bed(p),
                    cov: ink,
                    lift: lift * l(pgate, gate),
                    lift_span: strip_span,
                });
            }
        }
        last_pos = pos;
        last_lit = lit;
        last_gate = gate;
        last_flow = flow;
        let room = rainbow_dn_ledge_room(pos, dn);
        // THE TAIL EASE ([`RAINBOW_RUN_TAIL_EASE`]): the run's left outer
        // boundary — the mark's oldest edge — keeps a tenth of its coverage,
        // and the first cell's slabs ramp it back to full, so the slab ends
        // through a one-cell feather instead of a cliff.
        let tail_ease = if left.is_none() {
            RAINBOW_RUN_TAIL_EASE
        } else {
            1.0
        };
        let ink = flowed(
            (lit * tail_ease).min(rainbow_bed_cap_at(pos)).min(room),
            flow,
        );
        verts.push(RibbonVertex {
            x,
            spine,
            up,
            dn,
            core_up,
            core_dn,
            color: bed(pos),
            // THE STRIP'S OWN LIFT in the leading — the crisp baseline accent,
            // a share of this cell's own ink ([`rainbow_spine_lift`]), on its
            // own narrow span, priced with the body's melt against the one
            // dn-side ledge room.
            lift: rainbow_spine_lift(ink, strip_span, dn * RIBBON_LIFT_DN_MELT)
                .min((RIBBON_LIFT_DN_MELT * (room - ink)).max(0.0))
                * gate,
            lift_span: strip_span,
            // THE LEGIBILITY CEILING AS A SMOOTH ENVELOPE (§2.4). The vertex is
            // where the mark's COLOUR is resolved, so it is the only place its
            // ceiling can honestly be applied: the pair (colour, coverage) that
            // leaves here is exactly the pair `certify_rainbow_bed_over_gain`
            // certifies — the BED's ceiling, because the bed is the source-over
            // stream and its ground is displaced rather than added to. Capping
            // per CELL instead would pair a cell's ceiling with a boundary's
            // colour — up to a fifth of the arc apart at the shipped lay rate,
            // which is a whole ceiling's worth of light. Now that every SLAB is
            // a vertex, the pair is certified at every slab too.
            cov: ink,
        });
    }

    // HEAD FIRST. The seed is this run's newest cell, so starting from the
    // boundary nearest it makes the beam shed the far end — which is the older
    // light — when the budget runs out.
    if usize::from(seed_col - lo) > usize::from(hi - seed_col) {
        verts.reverse();
    }
}
