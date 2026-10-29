// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Prism Glass, the light-theme trail (`docs/design/PRISM-GLASS.md`): the phase level that draws
//! on a latched light scheme ([`LIVE`]), and the solver of the light page's tables (§1, §3 and
//! §15.2 1.4), whose glass129 the bed's ink table draws by under the glass (§15.2 1.5,
//! `ribbon::BedInkLut`).
//!
//! Every stop of the one walk gets two tones. The GLASS ([`glass_ink`]) sits behind text, at its
//! most chromatic legible lightness on the sRGB gamut edge: the anchors' cusp lightness,
//! interpolated with the hue along the arc, raised to the theme fg's floor (its bar plus
//! [`SOLVE_GUARD`]) and capped at [`GLASS_L_CAP`], with chroma at the edge capped at
//! [`GLASS_C_CAP`]. The lerp-aware lift then raises the walk table's entries until every colour
//! a slab can lay keeps the bar plus [`LERP_BAR_GUARD`] at every alpha ([`scan`]). The DEEP tone
//! ([`deep_ink`]) is the lip under the glass, the caret and the stars: full chroma at least
//! [`DEEP_UNDER_GLASS_L`] under the glass, held at [`AMBER_HOLD_HUE`] while it would be dark
//! amber, kept at [`ORANGE_FLOOR_L`] on the orange band, and folded over the olive at
//! [`FOLD_ARC`]. A page whose glass anchors lose their
//! chroma at the floor has no [`headroom`]: it is LIP-FORWARD, with no glass and a lip of its
//! own ([`deep_forward`]), held at [`AMBER_HOLD_HUE`] across its gold stretch where that would
//! be mud ([`amber_hold_forward`]) and folded off a thin cyan, snapping at the cyan waypoint
//! ([`cyan_fold`]). The caret ([`caret_table`]) is the lip solved to [`CARET_SOLVE`]
//! against the better of fg and page, folded off mud and snapped where a lerp would cross a side
//! or a fold.
//!
//! The shipped light pages are solved once into [`tables`]; any other pair is solved by
//! [`PaletteSolve`], a resumable state machine the frame advances by counted units
//! ([`SOLVE_UNITS_PER_FRAME`]), whose results a [`PaletteCache`] keeps for the last
//! [`PALETTE_CACHE_LEN`] pairs. The module is not called `prism`, which names the output
//! streak's Prism wake.

use aterm_hash::FxHashMap;

use crate::cursor_glow::Live;
use crate::rainbow_kitty::ribbon::{BED_INK_LUT_LEN, walk_pace};
use crate::spectrum::{SPECTRUM_ANCHOR_AT, SPECTRUM_LUT_LEN};

pub mod colour;
pub mod scan;
pub mod tables;

use colour::{contrast, cusp_l, edge_ink, gamut_edge, lch};

/// The live phase level every frame's [`Light`](crate::cursor_glow::Light) carries (§15.0.4):
/// each phase's last commit raises it one level, and until then a latched light scheme keeps
/// main's paper arm.
pub const LIVE: Live = Live::Off;

/// Entries in a walk table: the bed's ink table's length.
pub const GLASS_LEN: usize = BED_INK_LUT_LEN;

/// Entries in an arc table: the spectrum's length.
pub const ARC_LEN: usize = SPECTRUM_LUT_LEN;

/// The glass anchors on Default Light (`#2b2d33` on `#f6f7f9`), red to violet, hex
/// authoritative: each anchor's hue is its hex's, on every page.
pub const GLASS_ANCHORS: [u32; 8] = [
    0x00FF_7A7A,
    0x00FF_8B00,
    0x00FF_E000,
    0x004D_FB63,
    0x0000_F5FF,
    0x0014_ACFF,
    0x00BC_8CFF,
    0x00FC_66F3,
];

/// The cyan waypoint between green and blue, an arc index.
pub const CYAN_WAYPOINT: usize = 311;

/// Where the anchors sit, in arc indices: the spectrum's seven names and the cyan waypoint.
pub const ANCHOR_AT: [usize; 8] = [
    SPECTRUM_ANCHOR_AT[0],
    SPECTRUM_ANCHOR_AT[1],
    SPECTRUM_ANCHOR_AT[2],
    SPECTRUM_ANCHOR_AT[3],
    CYAN_WAYPOINT,
    SPECTRUM_ANCHOR_AT[4],
    SPECTRUM_ANCHOR_AT[5],
    SPECTRUM_ANCHOR_AT[6],
];

/// The deep tone's hues at the anchors, degrees.
pub const DEEP_HUES: [f64; 8] = [27.0, 50.0, 84.0, 145.0, 205.0, 264.0, 290.0, 315.0];

/// The theme fg's bar over the glass where its own page allows it: the dark bed's 5.25:1.
pub const GLASS_BAR: f64 = crate::cursor_glow::BODY_CONTRAST_BAR as f64;

/// The share of its own page contrast a low-contrast theme fg keeps instead: the bar is
/// `min(GLASS_BAR, 0.9 × fg/bg)`.
pub const PAGE_SHARE: f64 = 0.9;

/// What the glass is solved to above the bar: 5.45 on a 5.25 bar.
pub const SOLVE_GUARD: f64 = 0.20;

/// What every ramp the lift scans keeps above the bar: 5.35 on a 5.25 bar.
pub const LERP_BAR_GUARD: f64 = 0.10;

/// How far one round of the lift raises a flagged entry, OKLab L.
pub const LIFT_STEP_L: f64 = 0.002;

/// The most rounds the lift takes before the page is found without glass.
pub const MAX_LIFT_ROUNDS: usize = 16;

/// The glass's chroma cap, before rounding.
pub const GLASS_C_CAP: f64 = 0.24;

/// The glass's lightness cap.
pub const GLASS_L_CAP: f64 = 0.93;

/// The chroma every glass anchor keeps at its floor on a page with headroom.
pub const HEADROOM_C: f64 = 0.12;

/// How far the lip sits under the rounded glass, OKLab L.
pub const DEEP_UNDER_GLASS_L: f64 = 0.10;

/// The lip's least contrast against the page under glass.
pub const DEEP_PAGE_CR: f64 = 1.6;

/// The weight of lightness against chroma in the deep tone's solve: `L` maximises
/// `C − 0.10·|L − L_pref|` where `L_pref` is its upper bound.
pub const DEEP_L_WEIGHT: f64 = 0.10;

/// The AMBER HOLD's hue: a lip on the amber arcs holds here while it is darker than
/// [`AMBER_HOLD_L`], 1.5° short of the broad rule's mud band.
pub const AMBER_HOLD_HUE: f64 = 58.5;

/// The hue the AMBER HOLD reaches to, exclusive: the gold anchor's lip hue is inside it.
pub const AMBER_HOLD_TOP: f64 = 85.0;

/// The lightness the AMBER HOLD holds under, .015 above the broad rule's mud line.
pub const AMBER_HOLD_L: f64 = 0.765;

/// Where the glass crosses the yellow cusp hue [`FOLD_HUE`], as a share of the arc: the deep
/// tone steps from gold to green here and never lerps across it.
pub const FOLD_ARC: f64 = 0.3051;

/// The yellow cusp hue the glass crosses at [`FOLD_ARC`].
pub const FOLD_HUE: f64 = 110.0;

/// The deep tone's hue on the green side of the fold, at the fold itself (§1's `#83d600`); it
/// runs from here to the green anchor's.
pub const FOLD_GREEN_HUE: f64 = 132.0;

/// The walk table's last entry on the gold side of the fold.
pub const FOLD_AT: usize = walk_entry_before(FOLD_ARC);

/// THE ORANGE FLOOR: the least lightness of a lip from [`ORANGE_FLOOR_HUE`] up the spec rule's
/// orange band (h 40..85), [`LERP_L_GUARD`] over its mud line. A glass whose lip would sit under
/// it (a theme fg dark enough that the glass floor is under L .725 on the red-orange arcs) is
/// raised to keep the lip [`DEEP_UNDER_GLASS_L`] under it; Default Light's glass is above it
/// already.
pub const ORANGE_FLOOR_L: f64 = ORANGE_MUD_L + LERP_L_GUARD;

/// Where the ORANGE FLOOR starts, 1.5° short of the orange band as the AMBER HOLD is short of
/// the broad rule's, so a lerp from an entry under the floor stays off the band.
pub const ORANGE_FLOOR_HUE: f64 = ORANGE_MUD_HUES.0 - 1.5;

/// The arc table's last entry on the gold side of the fold.
pub const FOLD_AT_ARC: usize = arc_entry_before(FOLD_ARC);

/// The cyan waypoint as a share of the arc: where the green zone ends and the blue one starts.
const CYAN_ARC: f64 = CYAN_WAYPOINT as f64 / (ARC_LEN - 1) as f64;

/// The walk table's last entry before the cyan waypoint: a lip-forward lip whose CYAN FOLD
/// folded an entry of `(CYAN_AT, CYAN_AT + 1)` snaps across that pair ([`Palette::cyan_snap`]).
pub const CYAN_AT: usize = walk_entry_before(CYAN_ARC);

/// The arc table's last entry before the cyan waypoint, the arc table's [`CYAN_AT`].
pub const CYAN_AT_ARC: usize = arc_entry_before(CYAN_ARC);

/// A lip-forward lip's least contrast against the page.
pub const LIP_FORWARD_CR: f64 = 2.0;

/// The floor a lip-forward lip in the broad rule's mud band drops to, so it can reach
/// [`LIP_FORWARD_GOLD_L`].
pub const LIP_FORWARD_GOLD_CR: f64 = 1.95;

/// The lightness a lip-forward lip in the broad rule's mud band reaches: [`LERP_L_GUARD`] over
/// the broad rule's line, so no lerp between two neighbours there dips under it.
pub const LIP_FORWARD_GOLD_L: f64 = BROAD_MUD_L + LERP_L_GUARD;

/// The margin a lip floor keeps over the mud line it clears: an 8-bit lerp between two colours
/// over a line can dip a little under both.
pub const LERP_L_GUARD: f64 = 0.005;

/// The caret's cut-out bar.
pub const CARET_BAR: f64 = 4.6;

/// What the caret's cut-out is solved to.
pub const CARET_SOLVE: f64 = 4.65;

/// The least chroma of a vivid page-side caret fill.
pub const VIVID_C: f64 = 0.15;

/// How far a quantised solve steps when its hex lands a hair the wrong side of its bound, OKLab
/// L.
const NUDGE_L: f64 = 0.0005;

/// The most nudges one solve takes.
const MAX_NUDGES: u32 = 200;

/// The grid the deep tone's solve searches before it refines.
const ARGMAX_GRID: u32 = 16;

/// Bisection and ternary steps of every lightness solve: far under a step of the 8-bit grid.
const SOLVE_STEPS: u32 = 32;

/// The walk table's last entry before arc position `arc`.
const fn walk_entry_before(arc: f64) -> usize {
    let mut i = 0;
    while i + 1 < GLASS_LEN && (entry_arc(i + 1) as f64) < arc {
        i += 1;
    }
    i
}

/// The arc table's last entry before arc position `arc`.
const fn arc_entry_before(arc: f64) -> usize {
    let mut k = 0;
    while k + 1 < ARC_LEN && ((k + 1) as f64 / (ARC_LEN - 1) as f64) < arc {
        k += 1;
    }
    k
}

/// Walk table entry `i`'s arc position, a share of the arc: the bed's own walk
/// (`ribbon::walk_arc_at`).
#[inline]
const fn entry_arc(i: usize) -> f32 {
    walk_pace(i as f32 * (1.0 / (GLASS_LEN - 1) as f32))
}

/// Every walk table entry's arc position.
#[must_use]
pub fn walk_arcs() -> [f64; GLASS_LEN] {
    core::array::from_fn(|i| f64::from(entry_arc(i)))
}

/// Every arc table entry's arc position: entry `k` at `k / 510`.
#[must_use]
pub fn arc_arcs() -> [f64; ARC_LEN] {
    core::array::from_fn(|k| k as f64 / (ARC_LEN - 1) as f64)
}

/// The anchors' hues, off their hex.
fn anchor_hues() -> &'static [f64; 8] {
    static HUES: std::sync::OnceLock<[f64; 8]> = std::sync::OnceLock::new();
    HUES.get_or_init(|| GLASS_ANCHORS.map(|c| lch(c).h))
}

/// The anchors' cusp lightness, each the most chromatic lightness of its hue.
fn anchor_cusps() -> &'static [f64; 8] {
    static CUSPS: std::sync::OnceLock<[f64; 8]> = std::sync::OnceLock::new();
    CUSPS.get_or_init(|| anchor_hues().map(cusp_l))
}

/// The anchor segment `arc` (a share of the arc) falls in, and how far along it.
fn segment(arc: f64) -> (usize, f64) {
    let x = arc.clamp(0.0, 1.0) * (ARC_LEN - 1) as f64;
    let k = (0..7).find(|&k| x <= ANCHOR_AT[k + 1] as f64).unwrap_or(6);
    let (a, b) = (ANCHOR_AT[k] as f64, ANCHOR_AT[k + 1] as f64);
    (k, ((x - a) / (b - a)).clamp(0.0, 1.0))
}

/// The glass's preferred lightness and its hue at `arc`, both interpolated linearly between the
/// anchors' cusps and hues.
fn glass_pref(arc: f64) -> (f64, f64) {
    let (k, u) = segment(arc);
    let (cusps, hues) = (anchor_cusps(), anchor_hues());
    (
        cusps[k] + (cusps[k + 1] - cusps[k]) * u,
        hues[k] + (hues[k + 1] - hues[k]) * u,
    )
}

/// The glass at lightness `l` and hue `h`.
#[inline]
fn glass_at(l: f64, h: f64) -> u32 {
    edge_ink(l, h, GLASS_C_CAP)
}

/// The theme fg's bar on page `bg`: [`GLASS_BAR`], or [`PAGE_SHARE`] of its own page contrast
/// when that is lower.
#[must_use]
pub fn glass_bar(fg: u32, bg: u32) -> f64 {
    GLASS_BAR.min(PAGE_SHARE * contrast(fg, bg))
}

/// The least lightness at hue `h` whose glass keeps `target` on `fg`, under the cap; `None` when
/// none does.
fn floor_l(fg: u32, h: f64, target: f64) -> Option<f64> {
    let meets = |l: f64| contrast(fg, glass_at(l, h)) >= target;
    if !meets(GLASS_L_CAP) {
        return None;
    }
    let (mut lo, mut hi) = (0.0_f64, GLASS_L_CAP);
    for _ in 0..SOLVE_STEPS {
        let mid = 0.5 * (lo + hi);
        if meets(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Some(hi)
}

/// The glass point at `arc`, solved to `target` on `fg`: its lightness and hue, or `None` when
/// no lightness under the cap keeps the target.
fn glass_point(fg: u32, arc: f64, target: f64) -> Option<(f64, f64)> {
    let (pref, h) = glass_pref(arc);
    let floor = floor_l(fg, h, target)?;
    let room = if orange(deep_hue(arc)) {
        ORANGE_FLOOR_L + DEEP_UNDER_GLASS_L
    } else {
        0.0
    };
    let mut l = pref.max(floor).max(room).min(GLASS_L_CAP);
    for _ in 0..MAX_NUDGES {
        let ink = glass_at(l, h);
        if contrast(fg, ink) >= target && (lch(ink).l >= room || l >= GLASS_L_CAP) {
            return Some((l, h));
        }
        l = (l + NUDGE_L).min(GLASS_L_CAP);
    }
    None
}

/// The glass at `arc` (a share of the arc) on theme fg `fg` and page `bg`, before the lift: its
/// cusp lightness raised to the fg's floor ([`glass_bar`] plus [`SOLVE_GUARD`]), chroma at the
/// gamut edge. `None` when no lightness under [`GLASS_L_CAP`] keeps the floor.
#[must_use]
pub fn glass_ink(fg: u32, bg: u32, arc: f64) -> Option<u32> {
    let target = glass_bar(fg, bg) + SOLVE_GUARD;
    glass_point(fg, arc, target).map(|(l, h)| glass_at(l, h))
}

/// The glass anchors on `fg` and `bg`, red to violet; `None` where an anchor has no floor.
#[must_use]
pub fn glass_anchors(fg: u32, bg: u32) -> [Option<u32>; 8] {
    ANCHOR_AT.map(|at| glass_ink(fg, bg, at as f64 / (ARC_LEN - 1) as f64))
}

/// Whether `fg` on `bg` is a light page at all: ink darker than its page and the two 3:1 apart
/// (`host::light_latch::raw_light`'s polarity, without the frame's).
#[must_use]
pub fn light_page(fg: u32, bg: u32) -> bool {
    colour::luminance(fg) < colour::luminance(bg) && contrast(fg, bg) >= 3.0
}

/// Whether the page takes glass: every glass anchor keeps its floor with chroma at least
/// [`HEADROOM_C`]. A page without it is LIP-FORWARD.
#[must_use]
pub fn headroom(fg: u32, bg: u32) -> bool {
    light_page(fg, bg)
        && glass_anchors(fg, bg)
            .iter()
            .all(|a| a.is_some_and(|c| lch(c).c >= HEADROOM_C))
}

/// The deep tone's hue at `arc`: linear between [`DEEP_HUES`] at the anchors, but held at the
/// gold anchor's from yellow to [`FOLD_ARC`] and run from [`FOLD_GREEN_HUE`] to green past it.
fn deep_hue(arc: f64) -> f64 {
    let (k, u) = segment(arc);
    if k == 2 {
        if arc < FOLD_ARC {
            return DEEP_HUES[2];
        }
        let green = ANCHOR_AT[3] as f64 / (ARC_LEN - 1) as f64;
        let v = ((arc - FOLD_ARC) / (green - FOLD_ARC)).clamp(0.0, 1.0);
        return FOLD_GREEN_HUE + (DEEP_HUES[3] - FOLD_GREEN_HUE) * v;
    }
    DEEP_HUES[k] + (DEEP_HUES[k + 1] - DEEP_HUES[k]) * u
}

/// The edge colour at lightness `l` and hue `h`, uncapped.
#[inline]
fn deep_at(l: f64, h: f64) -> u32 {
    edge_ink(l, h, f64::INFINITY)
}

/// The greatest lightness at hue `h` whose edge colour keeps `floor` against `bg`.
fn page_l_max(bg: u32, h: f64, floor: f64) -> f64 {
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    for _ in 0..SOLVE_STEPS {
        let mid = 0.5 * (lo + hi);
        if contrast(bg, deep_at(mid, h)) >= floor {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// The lightness in `[lo, hi]` that maximises `C − DEEP_L_WEIGHT·|L − pref|` at hue `h`: a grid,
/// then a ternary search inside the best cell.
fn argmax_l(h: f64, lo: f64, hi: f64, pref: f64) -> f64 {
    if hi <= lo {
        return hi.max(0.0);
    }
    let score = |l: f64| gamut_edge(l, h) - DEEP_L_WEIGHT * (l - pref).abs();
    let step = (hi - lo) / f64::from(ARGMAX_GRID);
    let best = (0..=ARGMAX_GRID)
        .map(|k| lo + step * f64::from(k))
        .max_by(|a, b| score(*a).total_cmp(&score(*b)))
        .unwrap_or(hi);
    let (mut a, mut b) = ((best - step).max(lo), (best + step).min(hi));
    for _ in 0..SOLVE_STEPS {
        let m1 = a + (b - a) / 3.0;
        let m2 = b - (b - a) / 3.0;
        if score(m1) < score(m2) {
            a = m1;
        } else {
            b = m2;
        }
    }
    0.5 * (a + b)
}

/// The lip under `glass` at `arc` on page `bg`: the deep tone at least [`DEEP_UNDER_GLASS_L`]
/// under the rounded glass and [`DEEP_PAGE_CR`] against the page, at its most chromatic
/// lightness there, held at [`AMBER_HOLD_HUE`] while a hue past it would lay it darker than
/// [`AMBER_HOLD_L`], and no darker than [`ORANGE_FLOOR_L`] on the orange band where the glass
/// leaves room.
#[must_use]
pub fn deep_ink(bg: u32, glass: u32, arc: f64) -> u32 {
    let ceiling = lch(glass).l - DEEP_UNDER_GLASS_L;
    let solve = |h: f64| {
        let top = ceiling.min(page_l_max(bg, h, DEEP_PAGE_CR));
        let l = argmax_l(h, 0.0, top, ceiling);
        if orange(h) && l < ORANGE_FLOOR_L && top >= ORANGE_FLOOR_L {
            argmax_l(h, ORANGE_FLOOR_L, top, ceiling)
        } else {
            l
        }
    };
    let mut h = deep_hue(arc);
    let mut l = solve(h);
    if h > AMBER_HOLD_HUE && h < AMBER_HOLD_TOP && l < AMBER_HOLD_L {
        h = AMBER_HOLD_HUE;
        l = solve(h);
    }
    let floor = if orange(h) { ORANGE_FLOOR_L } else { 0.0 };
    let mut ink = deep_at(l, h);
    for _ in 0..MAX_NUDGES {
        let got = lch(ink).l;
        if got > ceiling {
            l -= NUDGE_L;
        } else if got < floor && l + NUDGE_L <= ceiling {
            l += NUDGE_L;
        } else {
            break;
        }
        ink = deep_at(l, h);
    }
    ink
}

/// A lip-forward lip at hue `h` on page `bg`: the deep tone at its most chromatic lightness that
/// keeps [`LIP_FORWARD_CR`] against the page, no darker than [`ORANGE_FLOOR_L`] on the orange
/// band where the page allows it.
fn forward_at(bg: u32, h: f64) -> u32 {
    let top = page_l_max(bg, h, LIP_FORWARD_CR);
    let mut l = argmax_l(h, 0.0, top, top);
    if orange(h) && l < ORANGE_FLOOR_L && top >= ORANGE_FLOOR_L {
        l = argmax_l(h, ORANGE_FLOOR_L, top, top);
    }
    deep_at(l, h)
}

/// A lip-forward page's lip at `arc` on page `bg`: the deep tone at its most chromatic lightness
/// that keeps [`LIP_FORWARD_CR`] against the page, no darker than [`ORANGE_FLOOR_L`] on the
/// orange band where the page allows it; in the broad rule's mud band it drops to
/// [`LIP_FORWARD_GOLD_CR`] to reach [`LIP_FORWARD_GOLD_L`].
#[must_use]
pub fn deep_forward(bg: u32, arc: f64) -> u32 {
    let h = deep_hue(arc);
    let mut ink = forward_at(bg, h);
    if (BROAD_MUD_HUES.0..=BROAD_MUD_HUES.1).contains(&h) && lch(ink).l < LIP_FORWARD_GOLD_L {
        let top = page_l_max(bg, h, LIP_FORWARD_GOLD_CR);
        if top >= LIP_FORWARD_GOLD_L {
            let mut l = argmax_l(h, LIP_FORWARD_GOLD_L, top, top);
            let mut gold = deep_at(l, h);
            for _ in 0..MAX_NUDGES {
                if lch(gold).l >= LIP_FORWARD_GOLD_L || l >= top {
                    break;
                }
                l = (l + NUDGE_L).min(top);
                gold = deep_at(l, h);
            }
            ink = gold;
        }
    }
    ink
}

/// Whether a lip at `arc` lies on the gold stretch: its deep hue strictly between
/// [`AMBER_HOLD_HUE`] and [`AMBER_HOLD_TOP`] (walk entries 20..=39, arc entries 77..=155).
fn on_gold_stretch(arc: f64) -> bool {
    let h = deep_hue(arc);
    h > AMBER_HOLD_HUE && h < AMBER_HOLD_TOP
}

/// THE LIP-FORWARD AMBER HOLD over a lip-forward page's solved lip `tables`, each a lip table
/// and its entries' arc positions: when any entry of either on the gold stretch (its deep hue
/// strictly between [`AMBER_HOLD_HUE`] and [`AMBER_HOLD_TOP`]) is [`mud`], every entry of both
/// on the stretch takes one colour, the lip-forward lip at [`AMBER_HOLD_HUE`] (§1's AMBER HOLD
/// carried across the page: a gold that cannot reach the broad rule's line at
/// [`LIP_FORWARD_GOLD_CR`] draws as amber). Whether it held.
pub fn amber_hold_forward(bg: u32, tables: [(&mut [u32], &[f64]); 2]) -> bool {
    let muddy = tables.iter().any(|(lips, arcs)| {
        lips.iter()
            .zip(arcs.iter())
            .any(|(&c, &arc)| on_gold_stretch(arc) && mud(c))
    });
    if !muddy {
        return false;
    }
    let held = forward_at(bg, AMBER_HOLD_HUE);
    for (lips, arcs) in tables {
        for (c, &arc) in lips.iter_mut().zip(arcs) {
            if on_gold_stretch(arc) {
                *c = held;
            }
        }
    }
    true
}

/// THE LIP-FORWARD CYAN FOLD over one lip-forward lip table `lips` at arc positions `arcs`, after
/// the [`amber_hold_forward`]: an entry is thin under [`HEADROOM_C`]. In the warm and green zones
/// (the CARET FOLD's: warm before [`FOLD_ARC`], green before [`CYAN_WAYPOINT`], blue from it) the
/// zone's first thin entry and every entry after it to the zone's end take the zone's last clean
/// entry before it; in the blue zone every entry from the zone's start through its last thin
/// entry takes the zone's first clean entry after it. A zone with no clean entry on that side is
/// left as it is, so every folded run ends at a snap: the fold or the cyan waypoint. Whether an
/// entry of the pair `(at, at + 1)` folded: `at` is [`CYAN_AT`] or [`CYAN_AT_ARC`], the pair the
/// lip then snaps across ([`Palette::cyan_snap`]).
pub fn cyan_fold(lips: &mut [u32], arcs: &[f64], at: usize) -> bool {
    let n = lips.len().min(arcs.len());
    let thin: Vec<bool> = lips[..n].iter().map(|&c| lch(c).c < HEADROOM_C).collect();
    let mut folded = vec![false; n];
    let mut start = 0;
    while start < n {
        let z = zone(arcs[start]);
        let end = (start..n).find(|&i| zone(arcs[i]) != z).unwrap_or(n);
        let run = if z < 2 {
            (start..end).find(|&i| thin[i]).map(|first| {
                let src = (start..first).next_back();
                (first..end, src)
            })
        } else {
            (start..end).rev().find(|&i| thin[i]).map(|last| {
                let src = (last + 1..end).next();
                (start..last + 1, src)
            })
        };
        if let Some((run, Some(src))) = run {
            let colour = lips[src];
            for i in run {
                lips[i] = colour;
                folded[i] = true;
            }
        }
        start = end;
    }
    folded.get(at).copied().unwrap_or(false) || folded.get(at + 1).copied().unwrap_or(false)
}

/// The broad rule's mud hues, degrees.
const BROAD_MUD_HUES: (f64, f64) = (60.0, 115.0);

/// The broad rule's line, OKLab L.
const BROAD_MUD_L: f64 = 0.75;

/// The spec rule's orange band, degrees: a colour here is mud darker than [`ORANGE_MUD_L`].
const ORANGE_MUD_HUES: (f64, f64) = (40.0, 85.0);

/// The spec rule's line under its orange band, OKLab L.
const ORANGE_MUD_L: f64 = 0.62;

/// Whether a lip hue takes the ORANGE FLOOR.
#[inline]
fn orange(h: f64) -> bool {
    (ORANGE_FLOOR_HUE..=ORANGE_MUD_HUES.1).contains(&h)
}

/// Whether `c` is mud under the spec rule (§1): chroma under .12, or on one of its bands
/// ([`mud_bands`]).
#[must_use]
pub fn mud_spec(c: u32) -> bool {
    lch(c).c < 0.12 || mud_bands(c)
}

/// Whether `c` lies on one of the spec rule's bands: hue 85..125 under L .82, or hue 40..85
/// under L .62. The glass is held to these and the broad rule; its chroma is the HEADROOM gate's,
/// at the anchors.
#[must_use]
pub fn mud_bands(c: u32) -> bool {
    let p = lch(c);
    ((85.0..=125.0).contains(&p.h) && p.l < 0.82)
        || ((ORANGE_MUD_HUES.0..=ORANGE_MUD_HUES.1).contains(&p.h) && p.l < ORANGE_MUD_L)
}

/// Whether `c` is mud under the broad rule (§1): chroma under .10, or a hue 60..115, under
/// L .75.
#[must_use]
pub fn mud_broad(c: u32) -> bool {
    let p = lch(c);
    p.l < BROAD_MUD_L && (p.c < 0.10 || (BROAD_MUD_HUES.0..=BROAD_MUD_HUES.1).contains(&p.h))
}

/// Whether `c` is mud under either rule.
#[must_use]
pub fn mud(c: u32) -> bool {
    mud_spec(c) || mud_broad(c)
}

/// Whether a page-side caret fill is vivid: chroma at least [`VIVID_C`], and no orange to green
/// (hue 35..135) darker than L .65.
#[must_use]
pub fn vivid(c: u32) -> bool {
    let p = lch(c);
    p.c >= VIVID_C && !((35.0..=135.0).contains(&p.h) && p.l < 0.65)
}

/// The fill of a caret over `lip` whose cut-out in `side` keeps [`CARET_SOLVE`]: the lip itself
/// when it does, else the lip's hue moved in lightness toward the far side of `side`, chroma
/// held to the lip's and the gamut edge. `None` when no lightness does.
#[must_use]
pub fn caret_fill(lip: u32, side: u32) -> Option<u32> {
    if contrast(lip, side) >= CARET_SOLVE {
        return Some(lip & 0x00FF_FFFF);
    }
    let p = lch(lip);
    let fill = |l: f64| colour::quantise(l, p.c.min(gamut_edge(l, p.h)), p.h);
    let darker = colour::luminance(side) > colour::luminance(lip);
    let far = if darker { 0.0 } else { 1.0 };
    if contrast(fill(far), side) < CARET_SOLVE {
        return None;
    }
    let (mut near, mut met) = (p.l, far);
    for _ in 0..SOLVE_STEPS {
        let mid = 0.5 * (near + met);
        if contrast(fill(mid), side) >= CARET_SOLVE {
            met = mid;
        } else {
            near = mid;
        }
    }
    Some(fill(met))
}

/// What a solved pair draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Glass, its lip and the deep caret.
    Glass,
    /// No glass: the page has no [`headroom`] and its lip carries the colour.
    LipForward,
    /// Not a light page ([`light_page`]): Prism Glass draws nothing here.
    Paper,
}

/// One caret entry on `fg` and `bg` over `lip`: its fill, and whether its cut-out is the fg. A
/// glass page cuts out whichever side's solved fill keeps more chroma (the page on a tie); a
/// lip-forward page's fg is too weak a side, so it cuts the page out of every entry.
#[must_use]
pub fn caret_entry(fg: u32, bg: u32, mode: Mode, lip: u32) -> (u32, bool) {
    let on_page = caret_fill(lip, bg);
    let on_fg = if mode == Mode::Glass {
        caret_fill(lip, fg)
    } else {
        None
    };
    match (on_page, on_fg) {
        (Some(p), Some(f)) if lch(f).c > lch(p).c => (f, true),
        (Some(p), _) => (p, false),
        (None, Some(f)) => (f, true),
        (None, None) => (lip & 0x00FF_FFFF, false),
    }
}

/// Which side of the fold and of the cyan waypoint an arc position lies on.
fn zone(arc: f64) -> u8 {
    if arc < FOLD_ARC {
        0
    } else if arc < CYAN_ARC {
        1
    } else {
        2
    }
}

/// One page's caret table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaretTable {
    /// Each entry's fill.
    pub fill: Vec<u32>,
    /// Whether each entry's cut-out is the theme fg (else the page).
    pub fg_side: Vec<bool>,
    /// Entries the CARET FOLD gave their clean neighbour's fill.
    pub folded: Vec<bool>,
    /// Neighbour pairs `(k, k + 1)` the caret snaps across instead of lerping.
    pub snap: Vec<bool>,
}

/// The CARET FOLD and the snaps over solved `fill` and `fg_side` at `arcs`: a fill that is mud,
/// or a page-side fill that is not [`vivid`], takes its nearest clean neighbour's fill and side
/// on its own side of the fold and of the cyan waypoint; the caret snaps at a side flip, where a
/// folded run meets another side of a fold, between folded entries of different fills, and
/// across [`FOLD_ARC`].
#[must_use]
pub fn caret_fold(fill: &[u32], fg_side: &[bool], arcs: &[f64]) -> CaretTable {
    let n = fill.len();
    let zones: Vec<u8> = arcs.iter().map(|&a| zone(a)).collect();
    let bad: Vec<bool> = (0..n)
        .map(|i| mud(fill[i]) || (!fg_side[i] && !vivid(fill[i])))
        .collect();
    let mut out = CaretTable {
        fill: fill.to_vec(),
        fg_side: fg_side.to_vec(),
        folded: vec![false; n],
        snap: vec![false; n.saturating_sub(1)],
    };
    for i in (0..n).filter(|&i| bad[i]) {
        let nearest = (0..n)
            .filter(|&j| !bad[j] && zones[j] == zones[i])
            .min_by_key(|&j| (j.abs_diff(i), j));
        if let Some(j) = nearest {
            out.fill[i] = fill[j];
            out.fg_side[i] = fg_side[j];
            out.folded[i] = true;
        }
    }
    for k in 0..n.saturating_sub(1) {
        let (a, b) = (k, k + 1);
        out.snap[k] = out.fg_side[a] != out.fg_side[b]
            || (zones[a] == 0 && zones[b] == 1)
            || ((out.folded[a] || out.folded[b]) && zones[a] != zones[b])
            || (out.folded[a] && out.folded[b] && out.fill[a] != out.fill[b]);
    }
    out
}

/// The caret table over `lips` at `arcs` on `fg` and `bg`.
#[must_use]
pub fn caret_table(fg: u32, bg: u32, mode: Mode, lips: &[u32], arcs: &[f64]) -> CaretTable {
    let (fill, fg_side): (Vec<u32>, Vec<bool>) = lips
        .iter()
        .map(|&lip| caret_entry(fg, bg, mode, lip))
        .unzip();
    caret_fold(&fill, &fg_side, arcs)
}

/// A set of walk table entries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Entries(pub [u64; 3]);

impl Entries {
    /// Whether entry `i` is in the set.
    #[inline]
    #[must_use]
    pub const fn has(&self, i: usize) -> bool {
        i < 192 && self.0[i / 64] >> (i % 64) & 1 == 1
    }

    /// Entry `i` in the set.
    #[inline]
    pub const fn insert(&mut self, i: usize) {
        if i < 192 {
            self.0[i / 64] |= 1 << (i % 64);
        }
    }

    /// How many entries the set holds.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> u32 {
        self.0[0].count_ones() + self.0[1].count_ones() + self.0[2].count_ones()
    }

    /// Whether the set is empty.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The set of the entries `flags` marks.
    #[must_use]
    pub fn of(flags: &[bool]) -> Self {
        let mut out = Self::default();
        for (i, _) in flags.iter().enumerate().filter(|(_, f)| **f) {
            out.insert(i);
        }
        out
    }
}

/// One light page's solved tables: the walk tables the bed samples and the arc tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    /// The theme fg it was solved on.
    pub fg: u32,
    /// The page it was solved on.
    pub bg: u32,
    /// What it draws.
    pub mode: Mode,
    /// The walk table's glass, after the lift; all zero unless [`Mode::Glass`].
    pub glass: [u32; GLASS_LEN],
    /// The walk table's lip: the deep tone under glass, or a lip-forward page's own.
    pub lip: [u32; GLASS_LEN],
    /// The arc table's glass, which the lift never moves; all zero unless [`Mode::Glass`].
    pub glass_arc: [u32; ARC_LEN],
    /// The arc table's lip.
    pub lip_arc: [u32; ARC_LEN],
    /// The caret's fills.
    pub caret: [u32; GLASS_LEN],
    /// The caret entries whose cut-out is the theme fg.
    pub caret_fg_side: Entries,
    /// The neighbour pairs `(k, k + 1)` the caret snaps across.
    pub caret_snap: Entries,
    /// The caret entries the CARET FOLD gave a neighbour's fill.
    pub caret_folded: Entries,
    /// The lip's last entry on the gold side of the fold: the sampler takes the nearer entry
    /// across `(fold_at, fold_at + 1)` instead of lerping.
    pub fold_at: u8,
    /// Whether the lip snaps across the cyan waypoint as it does across the fold: across
    /// `(CYAN_AT, CYAN_AT + 1)` in the walk table and `(CYAN_AT_ARC, CYAN_AT_ARC + 1)` in the arc
    /// table. Set on a lip-forward page whose [`cyan_fold`] folded an entry of either pair; false
    /// on every shipped page.
    pub cyan_snap: bool,
}

impl Palette {
    /// The shipped tables of `(fg, bg)`, when it is a shipped light page.
    #[must_use]
    pub fn shipped(fg: u32, bg: u32) -> Option<&'static Self> {
        tables::SHIPPED
            .iter()
            .find(|p| p.fg == fg & 0x00FF_FFFF && p.bg == bg & 0x00FF_FFFF)
    }

    /// Whether the page takes glass.
    #[inline]
    #[must_use]
    pub fn headroom(&self) -> bool {
        self.mode == Mode::Glass
    }
}

/// The light pages whose tables ship ([`tables::SHIPPED`], in this order): the six glass pages,
/// then the two lip-forward ones. The sixth glass page is the effects tests' light fixture, ink
/// `#24292f` on white.
pub const SHIPPED_PAGES: [(&str, u32, u32); 8] = [
    ("Default Light", 0x002B_2D33, 0x00F6_F7F9),
    ("Aterm Day", 0x0020_2733, 0x00F8_FAFD),
    ("Linen", 0x003C_291E, 0x00F9_F3E6),
    ("Gruvbox Light", 0x003C_3836, 0x00FB_F1C7),
    ("GitHub Light", 0x001F_2328, 0x00FF_FFFF),
    ("the test fixture", 0x0024_292F, 0x00FF_FFFF),
    ("Solarized Light", 0x0058_6E75, 0x00FD_F6E3),
    ("Catppuccin Latte", 0x004C_4F69, 0x00EF_F1F5),
];

/// The most work units one light frame runs of a pending [`PaletteSolve`]: the units of the most
/// costly kind that fit 1 ms, an eighth of a 120 Hz frame. Set from aterm-bench's
/// `prism_glass_units` release bench, which times every unit of every shipped page's solve: on
/// 2026-10-27 the most costly was a lift round's interval scan on Linen, 243 µs (a glass page's
/// whole solve is 1,682 units when its lift takes one raising round, as Gruvbox Light's does, and
/// 1,813 when it takes two, as every other shipped glass page's does, 54 to 61 ms; a lip-forward
/// page's is 779 units and 29 to 34 ms).
pub const SOLVE_UNITS_PER_FRAME: usize = 4;

/// The most units a [`PaletteSolve`] runs: its ground, the anchors, both tables' glass, the
/// scan's ground and every lift round's origins and intervals, both tables' lip, the caret and
/// its fold.
pub const MAX_SOLVE_UNITS: usize = 1
    + 8
    + GLASS_LEN
    + ARC_LEN
    + 1
    + MAX_LIFT_ROUNDS * (scan::SLAB_DENSITIES.len() + GLASS_LEN - 1)
    + GLASS_LEN
    + ARC_LEN
    + GLASS_LEN
    + 1;

/// The pairs a [`PaletteCache`] keeps.
pub const PALETTE_CACHE_LEN: usize = 8;

#[cfg(any(test, debug_assertions))]
thread_local! {
    static SOLVE_UNITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The [`PaletteSolve`] units this thread has run since the last take, zeroed: the host's
/// light-fork pin holds it at zero on every dark ground (`prism_glass_solves == 0`). Debug and
/// test builds only.
#[cfg(any(test, debug_assertions))]
pub fn take_solves() -> usize {
    SOLVE_UNITS.with(|n| n.replace(0))
}

/// Where a [`PaletteSolve`] stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// Classify the page.
    Ground,
    /// Solve anchor `k`; after the last, decide the headroom.
    Anchor(usize),
    /// Solve walk table glass entry `i`.
    Glass(usize),
    /// Solve arc table glass entry `k`.
    GlassArc(usize),
    /// Build the scan's ground.
    Scanner,
    /// Take the origins of density `density` for lift round `round`.
    Origins { round: usize, density: usize },
    /// Scan the origins of table interval `interval` in lift round `round`.
    Scan { round: usize, interval: usize },
    /// Solve walk table lip entry `i` under the glass.
    Deep(usize),
    /// Solve arc table lip entry `k` under the glass.
    DeepArc(usize),
    /// Solve walk table lip-forward entry `i`.
    Forward(usize),
    /// Solve arc table lip-forward entry `k`; after the last, hold the amber and fold the cyan
    /// of both lip tables (`PaletteSolve::lip_forward_rules`).
    ForwardArc(usize),
    /// Solve caret entry `i`.
    Caret(usize),
    /// Fold the caret and snap it.
    Fold,
    /// Finished.
    Done,
}

/// A resumable solve of one pair's [`Palette`]. Each unit is one table entry's solve (an
/// anchor's, the glass's, the lip's or the caret's bisection) or one table interval's scan in a
/// lift round, plus one to classify the page and one to fold the caret, so a frame that runs a
/// bounded count of units does bounded work, and the count, never a clock, decides where the
/// solve stands.
#[derive(Clone, Debug)]
pub struct PaletteSolve {
    fg: u32,
    bg: u32,
    stage: Stage,
    units: usize,
    target: f64,
    lift_target: f64,
    anchors: [Option<u32>; 8],
    points: [(f64, f64); GLASS_LEN],
    raised: [u8; GLASS_LEN],
    rounds: usize,
    flagged: [bool; GLASS_LEN],
    ground: Option<scan::Ground>,
    /// This round's breakpoints.
    breaks: Vec<f64>,
    /// This round's origins, bucketed by the table interval of their first vertex: each origin
    /// and the density it is scanned at.
    origins: Vec<Vec<(f64, usize)>>,
    arcs: [f64; GLASS_LEN],
    arc_arcs: [f64; ARC_LEN],
    fills: [u32; GLASS_LEN],
    sides: [bool; GLASS_LEN],
    palette: Palette,
}

impl PaletteSolve {
    /// A solve of theme fg `fg` on page `bg`, before its first unit.
    #[must_use]
    pub fn new(fg: u32, bg: u32) -> Self {
        let (fg, bg) = (fg & 0x00FF_FFFF, bg & 0x00FF_FFFF);
        Self {
            fg,
            bg,
            stage: Stage::Ground,
            units: 0,
            target: 0.0,
            lift_target: 0.0,
            anchors: [None; 8],
            points: [(0.0, 0.0); GLASS_LEN],
            raised: [0; GLASS_LEN],
            rounds: 0,
            flagged: [false; GLASS_LEN],
            ground: None,
            breaks: Vec::new(),
            origins: Vec::new(),
            arcs: walk_arcs(),
            arc_arcs: arc_arcs(),
            fills: [0; GLASS_LEN],
            sides: [false; GLASS_LEN],
            palette: Palette {
                fg,
                bg,
                mode: Mode::Paper,
                glass: [0; GLASS_LEN],
                lip: [0; GLASS_LEN],
                glass_arc: [0; ARC_LEN],
                lip_arc: [0; ARC_LEN],
                caret: [0; GLASS_LEN],
                caret_fg_side: Entries::default(),
                caret_snap: Entries::default(),
                caret_folded: Entries::default(),
                fold_at: FOLD_AT as u8,
                cyan_snap: false,
            },
        }
    }

    /// The pair it solves.
    #[inline]
    #[must_use]
    pub const fn pair(&self) -> (u32, u32) {
        (self.fg, self.bg)
    }

    /// The units it has run.
    #[inline]
    #[must_use]
    pub const fn units(&self) -> usize {
        self.units
    }

    /// The lift rounds it has scanned, the clean one included.
    #[inline]
    #[must_use]
    pub const fn lift_rounds(&self) -> usize {
        self.rounds
    }

    /// How many times the lift raised each glass entry.
    #[inline]
    #[must_use]
    pub const fn raised(&self) -> &[u8; GLASS_LEN] {
        &self.raised
    }

    /// What its next unit solves: `ground`, `anchor`, `glass`, `glass_arc`, `scanner`,
    /// `origins`, `scan`, `deep`, `deep_arc`, `forward`, `forward_arc`, `caret`, `fold`, or
    /// `done`.
    #[must_use]
    pub const fn next_unit(&self) -> &'static str {
        match self.stage {
            Stage::Ground => "ground",
            Stage::Anchor(_) => "anchor",
            Stage::Glass(_) => "glass",
            Stage::GlassArc(_) => "glass_arc",
            Stage::Scanner => "scanner",
            Stage::Origins { .. } => "origins",
            Stage::Scan { .. } => "scan",
            Stage::Deep(_) => "deep",
            Stage::DeepArc(_) => "deep_arc",
            Stage::Forward(_) => "forward",
            Stage::ForwardArc(_) => "forward_arc",
            Stage::Caret(_) => "caret",
            Stage::Fold => "fold",
            Stage::Done => "done",
        }
    }

    /// The palette, once the solve has finished.
    #[inline]
    #[must_use]
    pub fn done(&self) -> Option<&Palette> {
        (self.stage == Stage::Done).then_some(&self.palette)
    }

    /// Run at most `budget` units; the units run.
    pub fn advance(&mut self, budget: usize) -> usize {
        let mut ran = 0;
        while ran < budget && self.stage != Stage::Done {
            self.unit();
            ran += 1;
        }
        ran
    }

    /// One unit.
    fn unit(&mut self) {
        self.units += 1;
        #[cfg(any(test, debug_assertions))]
        SOLVE_UNITS.with(|n| n.set(n.get() + 1));
        self.stage = match self.stage {
            Stage::Ground => {
                if light_page(self.fg, self.bg) {
                    self.target = glass_bar(self.fg, self.bg) + SOLVE_GUARD;
                    self.lift_target = glass_bar(self.fg, self.bg) + LERP_BAR_GUARD;
                    Stage::Anchor(0)
                } else {
                    Stage::Done
                }
            }
            Stage::Anchor(k) => {
                let at = ANCHOR_AT[k] as f64 / (ARC_LEN - 1) as f64;
                self.anchors[k] =
                    glass_point(self.fg, at, self.target).map(|(l, h)| glass_at(l, h));
                if k + 1 < 8 {
                    Stage::Anchor(k + 1)
                } else if self
                    .anchors
                    .iter()
                    .all(|a| a.is_some_and(|c| lch(c).c >= HEADROOM_C))
                {
                    self.palette.mode = Mode::Glass;
                    Stage::Glass(0)
                } else {
                    self.palette.mode = Mode::LipForward;
                    Stage::Forward(0)
                }
            }
            Stage::Glass(i) => match glass_point(self.fg, self.arcs[i], self.target) {
                Some(point) => {
                    self.points[i] = point;
                    self.palette.glass[i] = glass_at(point.0, point.1);
                    if i + 1 < GLASS_LEN {
                        Stage::Glass(i + 1)
                    } else {
                        Stage::GlassArc(0)
                    }
                }
                None => self.lip_forward(),
            },
            Stage::GlassArc(k) => match glass_ink(self.fg, self.bg, self.arc_arcs[k]) {
                Some(ink) => {
                    self.palette.glass_arc[k] = ink;
                    if k + 1 < ARC_LEN {
                        Stage::GlassArc(k + 1)
                    } else {
                        Stage::Scanner
                    }
                }
                None => self.lip_forward(),
            },
            Stage::Scanner => {
                self.ground = Some(scan::Ground::new(self.fg, self.bg));
                Stage::Origins {
                    round: 0,
                    density: 0,
                }
            }
            Stage::Origins { round, density } => self.origins_unit(round, density),
            Stage::Scan { round, interval } => self.scan_unit(round, interval),
            Stage::Deep(i) => {
                self.palette.lip[i] = deep_ink(self.bg, self.palette.glass[i], self.arcs[i]);
                if i + 1 < GLASS_LEN {
                    Stage::Deep(i + 1)
                } else {
                    Stage::DeepArc(0)
                }
            }
            Stage::DeepArc(k) => {
                self.palette.lip_arc[k] =
                    deep_ink(self.bg, self.palette.glass_arc[k], self.arc_arcs[k]);
                if k + 1 < ARC_LEN {
                    Stage::DeepArc(k + 1)
                } else {
                    Stage::Caret(0)
                }
            }
            Stage::Forward(i) => {
                self.palette.lip[i] = deep_forward(self.bg, self.arcs[i]);
                if i + 1 < GLASS_LEN {
                    Stage::Forward(i + 1)
                } else {
                    Stage::ForwardArc(0)
                }
            }
            Stage::ForwardArc(k) => {
                self.palette.lip_arc[k] = deep_forward(self.bg, self.arc_arcs[k]);
                if k + 1 < ARC_LEN {
                    Stage::ForwardArc(k + 1)
                } else {
                    self.lip_forward_rules();
                    Stage::Caret(0)
                }
            }
            Stage::Caret(i) => {
                (self.fills[i], self.sides[i]) =
                    caret_entry(self.fg, self.bg, self.palette.mode, self.palette.lip[i]);
                if i + 1 < GLASS_LEN {
                    Stage::Caret(i + 1)
                } else {
                    Stage::Fold
                }
            }
            Stage::Fold => {
                let caret = caret_fold(&self.fills, &self.sides, &self.arcs);
                self.palette.caret.copy_from_slice(&caret.fill);
                self.palette.caret_fg_side = Entries::of(&caret.fg_side);
                self.palette.caret_snap = Entries::of(&caret.snap);
                self.palette.caret_folded = Entries::of(&caret.folded);
                Stage::Done
            }
            Stage::Done => Stage::Done,
        };
    }

    /// The lift is over: drop the scan's state.
    fn release_scan(&mut self) {
        self.ground = None;
        self.breaks = Vec::new();
        self.origins = Vec::new();
    }

    /// Both lip-forward lip tables are solved: the LIP-FORWARD AMBER HOLD over both, then the
    /// LIP-FORWARD CYAN FOLD over each, inside the last `ForwardArc` unit so the caret is solved
    /// from the folded lip and no unit is added.
    fn lip_forward_rules(&mut self) {
        amber_hold_forward(
            self.bg,
            [
                (&mut self.palette.lip[..], &self.arcs[..]),
                (&mut self.palette.lip_arc[..], &self.arc_arcs[..]),
            ],
        );
        let walk = cyan_fold(&mut self.palette.lip, &self.arcs, CYAN_AT);
        let arc = cyan_fold(&mut self.palette.lip_arc, &self.arc_arcs, CYAN_AT_ARC);
        self.palette.cyan_snap = walk || arc;
    }

    /// The glass cannot keep its bar: the page draws lip-forward instead.
    fn lip_forward(&mut self) -> Stage {
        self.palette.mode = Mode::LipForward;
        self.palette.glass = [0; GLASS_LEN];
        self.palette.glass_arc = [0; ARC_LEN];
        Stage::Forward(0)
    }

    /// One table interval's scan in lift round `round`.
    /// One density's origins for lift round `round`; the first also takes the table's
    /// breakpoints.
    fn origins_unit(&mut self, round: usize, density: usize) -> Stage {
        if density == 0 {
            self.flagged = [false; GLASS_LEN];
            self.breaks = scan::breakpoints(&self.palette.glass);
            self.origins = vec![Vec::new(); GLASS_LEN - 1];
        }
        let slabs = scan::SLAB_DENSITIES[density];
        for t in scan::origins(&self.breaks, slabs.step) {
            self.origins[scan::interval_of(t)].push((t, density));
        }
        if density + 1 < scan::SLAB_DENSITIES.len() {
            Stage::Origins {
                round,
                density: density + 1,
            }
        } else {
            Stage::Scan { round, interval: 0 }
        }
    }

    fn scan_unit(&mut self, round: usize, interval: usize) -> Stage {
        let Some(ground) = self.ground.as_mut() else {
            return Stage::Scanner;
        };
        let mut verdicts: FxHashMap<((u32, u32), usize), bool> = FxHashMap::default();
        for &(t, d) in &self.origins[interval] {
            let slabs = scan::SLAB_DENSITIES[d];
            let pair = scan::pair_at(&self.palette.glass, t, slabs.step);
            let under = *verdicts.entry((pair, d)).or_insert_with(|| {
                [scan::Columns::K16, scan::Columns::Gpu]
                    .iter()
                    .any(|&m| ground.pair_under(pair.0, pair.1, slabs, m, self.lift_target))
            });
            if under {
                for i in scan::bracket(t, slabs.step) {
                    self.flagged[i.min(GLASS_LEN - 1)] = true;
                }
            }
        }
        if interval + 1 < GLASS_LEN - 1 {
            return Stage::Scan {
                round,
                interval: interval + 1,
            };
        }
        self.rounds = round + 1;
        if !self.flagged.contains(&true) {
            self.release_scan();
            return Stage::Deep(0);
        }
        if self.rounds >= MAX_LIFT_ROUNDS {
            self.release_scan();
            return self.lip_forward();
        }
        for i in (0..GLASS_LEN).filter(|&i| self.flagged[i]) {
            let (l, h) = self.points[i];
            let l = (l + LIFT_STEP_L).min(GLASS_L_CAP);
            self.points[i] = (l, h);
            self.palette.glass[i] = glass_at(l, h);
            self.raised[i] = self.raised[i].saturating_add(1);
        }
        Stage::Origins {
            round: round + 1,
            density: 0,
        }
    }
}

/// `fg` on `bg`'s palette, solved to the end.
#[must_use]
pub fn solve(fg: u32, bg: u32) -> Palette {
    let mut s = PaletteSolve::new(fg, bg);
    s.advance(usize::MAX);
    s.palette
}

/// The palettes of the last [`PALETTE_CACHE_LEN`] unshipped pairs a light frame asked for, and
/// the solve of the pair it is asking for now.
#[derive(Clone, Debug, Default)]
pub struct PaletteCache {
    /// Each kept palette and the request that last touched it.
    slots: Vec<(u64, Palette)>,
    /// The requests made, the recency clock.
    clock: u64,
    /// The solve in progress.
    pending: Option<PaletteSolve>,
}

impl PaletteCache {
    /// An empty cache.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: Vec::new(),
            clock: 0,
            pending: None,
        }
    }

    /// One light frame's request for `(fg, bg)`: the shipped or kept palette, or, while it is
    /// being solved, `None` after at most [`SOLVE_UNITS_PER_FRAME`] more units of its solve. A
    /// request for another pair that is neither shipped nor kept drops the solve in progress; a
    /// shipped or kept pair answers without touching it.
    pub fn frame(&mut self, fg: u32, bg: u32) -> Option<&Palette> {
        if let Some(p) = Palette::shipped(fg, bg) {
            return Some(p);
        }
        let (fg, bg) = (fg & 0x00FF_FFFF, bg & 0x00FF_FFFF);
        self.clock += 1;
        if let Some(k) = self
            .slots
            .iter()
            .position(|(_, p)| (p.fg, p.bg) == (fg, bg))
        {
            self.slots[k].0 = self.clock;
            return Some(&self.slots[k].1);
        }
        let solve = match &mut self.pending {
            Some(s) if s.pair() == (fg, bg) => s,
            slot => slot.insert(PaletteSolve::new(fg, bg)),
        };
        solve.advance(SOLVE_UNITS_PER_FRAME);
        let palette = *solve.done()?;
        self.pending = None;
        if self.slots.len() >= PALETTE_CACHE_LEN
            && let Some(oldest) = self
                .slots
                .iter()
                .enumerate()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(k, _)| k)
        {
            self.slots.swap_remove(oldest);
        }
        self.slots.push((self.clock, palette));
        self.slots.last().map(|(_, p)| p)
    }

    /// How many palettes it keeps.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether it keeps none.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Whether it keeps `(fg, bg)`'s palette.
    #[must_use]
    pub fn holds(&self, fg: u32, bg: u32) -> bool {
        self.slots
            .iter()
            .any(|(_, p)| (p.fg, p.bg) == (fg & 0x00FF_FFFF, bg & 0x00FF_FFFF))
    }

    /// The pair being solved.
    #[must_use]
    pub fn pending(&self) -> Option<(u32, u32)> {
        self.pending.as_ref().map(PaletteSolve::pair)
    }

    /// The light words (`docs/design/PRISM-GLASS.md` §15.0.2(a)): each kept pair with the
    /// request that last touched it, and the pair being solved with the units it has run; none
    /// for a cache never asked for an unshipped pair. Never compared. Debug and test builds only.
    #[cfg(any(test, debug_assertions))]
    pub fn light_words(&self, w: &mut impl crate::state_words::Words) {
        use crate::state_words::Words as _;
        if self.slots.is_empty() && self.pending.is_none() {
            return;
        }
        w.put("palette.clock", self.clock);
        w.list("palette.kept", &self.slots, |f, (at, p)| {
            f.put("fg", u64::from(p.fg));
            f.put("bg", u64::from(p.bg));
            f.put("at", *at);
        });
        w.opt("palette.pending", self.pending.as_ref(), |f, s| {
            f.put("fg", u64::from(s.fg));
            f.put("bg", u64::from(s.bg));
            f.put("units", s.units as u64);
        });
    }
}

#[cfg(test)]
mod tests;
