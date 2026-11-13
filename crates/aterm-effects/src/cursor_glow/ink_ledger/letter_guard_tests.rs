// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn ramped(cuts: &[(i32, f32)], skip: Option<i32>) -> Vec<(i32, f32)> {
    climbed(cuts, skip, false)
}

fn climbed(cuts: &[(i32, f32)], skip: Option<i32>, climb: bool) -> Vec<(i32, f32)> {
    lettered_climbed(cuts, &[], skip, climb)
}

/// The ramp over `cuts`, the letters being the cut columns and `whole`
/// (letters left uncut).
fn lettered_climbed(
    cuts: &[(i32, f32)],
    whole: &[i32],
    skip: Option<i32>,
    climb: bool,
) -> Vec<(i32, f32)> {
    let mut out = cuts.to_vec();
    let lettered = |col: i32| whole.contains(&col) || cuts.iter().any(|c| c.0 == col);
    ramp_row(cuts, skip, climb, lettered, |col, share| {
        match out.iter_mut().find(|c| c.0 == col) {
            Some(c) => c.1 = share,
            None => out.push((col, share)),
        }
    });
    out.sort_by_key(|c| c.0);
    out
}

/// A word gap between two cut letters keeps no more light than they do
/// beyond one slope step, and every share only falls.
#[test]
fn a_row_s_cuts_ease_as_one_ramp_through_its_word_gaps() {
    // "as the": columns 0-1 cut, 2 a gap, 3-5 cut; 4 left whole by its own cut.
    let cuts = [(0, 0.20), (1, 0.30), (3, 0.25), (5, 0.60)];
    let out = ramped(&cuts, None);
    let at = |col: i32| out.iter().find(|c| c.0 == col).map_or(1.0, |c| c.1);
    assert!(
        at(2) <= at(1).min(at(3)) + RAMP_SLOPE + 1e-6,
        "the gap keeps {} against its letters' {} and {}",
        at(2),
        at(1),
        at(3)
    );
    for w in out.windows(2) {
        assert!(
            (w[1].1 - w[0].1).abs() <= RAMP_SLOPE * (w[1].0 - w[0].0) as f32 + 1e-6,
            "a step of {} between columns {} and {}",
            (w[1].1 - w[0].1).abs(),
            w[0].0,
            w[1].0
        );
    }
    for &(col, s) in &cuts {
        assert!(at(col) <= s, "column {col} rose from {s} to {}", at(col));
    }
    assert!(at(4) < 1.0, "the letter between two cuts eases with them");
    // The caret's cell is the caret's law, and nothing past the run is touched.
    let skipped = ramped(&cuts, Some(2));
    assert!(skipped.iter().all(|c| c.0 != 2));
    assert!(out.iter().all(|c| (0..=5).contains(&c.0)));
}

/// looks8 round 2: the dark landing ring's rim runs on under the text. A
/// one-pixel slab across `ab  cd` keeps [`RIM_UNDER_LETTERS`] of its light
/// on each lettered cell and the judge's [`RING`] round it, its whole
/// light elsewhere, and no pixel of its span goes dark. Negative control:
/// cut off the letters (a share of 0, as the rim was cut), the span breaks
/// into the pieces that read as stray glyphs.
#[test]
fn the_dark_rim_runs_on_under_the_letters_at_a_fifth_of_its_light() {
    let g = Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 320,
        win_h: 96,
        head: 0,
    };
    let row = 2u16;
    let letters = [(4, 'a'), (5, 'b'), (8, 'c'), (9, 'd')];
    let mut cells = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
    for (col, ch) in letters {
        cells[usize::from(row)][col].ch = ch;
    }
    let lettered = |x: i32| {
        letters
            .iter()
            .any(|&(c, _)| x >= c as i32 * 8 - RING && x < (c as i32 + 1) * 8 + RING)
    };
    let mut glow = CursorGlow::default();
    glow.observe_frame_text(Some((row, 20)), &cells, Instant::now());
    let slab = GlowQuad::flat(row, 0, row * 16 + 5, 120, 1, 0x0040_C080, 0);
    let lay = |share: f32| {
        let mut q = vec![GlowQuad::default(), slab];
        glow.pass_under_letters(&mut q, 1, g, share);
        q
    };
    let light = |q: &[GlowQuad], x: i32| {
        q[1..]
            .iter()
            .find(|p| x >= i32::from(p.x) && x < i32::from(p.x) + i32::from(p.w))
            .map(|p| (p.color, p.is_flat()))
    };
    let q = lay(RIM_UNDER_LETTERS);
    assert_eq!(
        q[0],
        GlowQuad::default(),
        "a quad before `from` is left alone"
    );
    let span: u32 = q[1..].iter().map(|p| u32::from(p.w)).sum();
    assert_eq!(span, 120, "the pieces tile the slab once: {q:?}");
    for x in 0..120 {
        let want = if lettered(x) {
            scale_rgb(slab.color, RIM_UNDER_LETTERS)
        } else {
            slab.color
        };
        assert_eq!(light(&q, x), Some((want, true)), "pixel {x}: {q:?}");
    }
    let cut = lay(0.0);
    assert!(
        (0..120).any(|x| light(&cut, x).is_none()),
        "control: the rim cut off the letters keeps its whole span: {cut:?}"
    );
}

/// looks7 WIDEN: a page whose text has no room for the GPU's bloom holds
/// it, and one whose text clears the floor keeps it. Solarized Dark's
/// text (4.75:1, its solid ink 4.04:1), a 5.0:1 grey on the dark page
/// and a 4.95:1 grey on paper hold it; the cold dark page's text and
/// GitHub Light's ink keep it: the negative control, which a hold on
/// every page (or on none) fails.
#[test]
fn a_dim_page_holds_the_bloom_and_a_bright_one_keeps_it() {
    let mut glow = CursorGlow::default();
    for (ink, ground) in [
        (0x0083_9496, 0x0000_2B36),
        (0x0085_8585, 0x0011_1318),
        (0x0070_7070, 0x00FF_FFFF),
    ] {
        assert!(
            glow.holds_bloom(ink, ground),
            "{ink:06x} on {ground:06x} keeps the bloom"
        );
    }
    for (ink, ground) in [(0x00C8_D3F5, 0x0011_1318), (0x001F_2328, 0x00FF_FFFF)] {
        assert!(
            !glow.holds_bloom(ink, ground),
            "{ink:06x} on {ground:06x} holds the bloom"
        );
    }
    // The memo answers for its own pair only.
    assert!(glow.holds_bloom(0x0083_9496, 0x0000_2B36));
    assert!(!glow.holds_bloom(0x00C8_D3F5, 0x0011_1318));
}

/// looks7 M4: a word gap between two letters cut deep keeps no more than
/// the straight line between their shares: Phaser's dark bar stood a
/// stripe in each gap, a share and a half of its letters' light. Past the
/// run's last cut the gap kept its whole light beside a letter cut to
/// two fifths (the gallery's `the quick`, `e` cut and `q` whole): it
/// keeps the line to the uncut letter's whole. Climbing the slope from
/// each letter (the negative control), it peaks, and past the run stays
/// whole. A long run of blanks still climbs the slope back toward its
/// whole light.
#[test]
fn a_word_gap_keeps_the_line_between_its_letters() {
    // "the quick": 'e' at 2, the gap at 3, 'q' at 4; a long blank run
    // from 6 to 17 between 'u' at 5 and a cut at 18.
    let cuts = [(1, 0.18), (2, 0.15), (4, 0.20), (5, 0.22), (18, 0.2)];
    let line = |out: &[(i32, f32)], col: i32| out.iter().find(|c| c.0 == col).map_or(1.0, |c| c.1);
    let out = ramped(&cuts, None);
    let gap = line(&out, 3);
    assert!(
        gap <= 0.5 * (0.15 + 0.20) + 1e-6,
        "the gap keeps {gap} between its letters' 0.15 and 0.20"
    );
    assert!(
        line(&out, 11) > 0.22 + 2.0 * RAMP_SLOPE,
        "the long blank run keeps only {} in its middle",
        line(&out, 11)
    );
    let climbed_gap = line(&climbed(&cuts, None, true), 3);
    assert!(
        climbed_gap > 0.20 + 0.01,
        "control: climbing the slope the gap keeps only {climbed_gap}, no more than its letters"
    );
    // "the quick", `e` cut at 2 and `q`, `u` at 4 and 5 left whole.
    let edge = [(1, 0.18), (2, 0.4)];
    let gap = line(&lettered_climbed(&edge, &[4, 5], None, false), 3);
    assert!(
        gap <= 0.5 * (0.4 + 1.0) + 1e-6,
        "past the run the gap keeps {gap} between its letters' 0.4 and whole"
    );
    let past = line(&lettered_climbed(&edge, &[4, 5], None, true), 3);
    assert!(
        past >= 1.0,
        "control: climbing, the gap past the run keeps {past}, under its whole light"
    );
}

/// Lane B5: the guard cuts a stream's ramps as it cuts the quads their
/// rows stand for, a rail laid clear of the ring included
/// ([`InkLedger::note_rail_laid`]): a rail's row, or its run's one
/// quad, is cut only where it meets a dimmed cell's box, and every cut
/// piece is truncated as the quad's is. A light across a dimmed row and
/// a rail below it, laid once as quads and once as ramps (every run a
/// ramp), are cut alike, light for light. Premise: the rail noted, its
/// quads keep light the feather would cut. Negative controls: the
/// rail's ramps not noted (its quads alone, [`InkLedger::note_rail`]),
/// or the ramps' cut pieces rounded, the ramps are cut otherwise.
#[test]
fn a_stream_s_ramps_are_cut_as_their_quads_rails_included() {
    use aterm_render::{BeamRows, RampCost, RampSink};
    // Cells 10 x 20 px; columns 4..7 of the row at y 40..60 dimmed.
    let dimmed = || {
        let mut ledger = InkLedger::EMPTY;
        ledger.grid = (0.0, 0.0, 10.0, 20.0);
        for (col, share) in [(4, 0.3), (5, 0.25), (6, 0.4)] {
            ledger.dims.push(Dim {
                x0: col as f32 * 10.0,
                y0: 40.0,
                x1: (col + 1) as f32 * 10.0,
                y1: 60.0,
                share,
            });
        }
        ledger
    };
    // A row's ends: additive, every third source-over.
    fn ends(y: u16) -> ((u32, u8), (u32, u8)) {
        let k = u32::from(y % 5 + 1);
        if y.is_multiple_of(3) {
            ((k * 0x000E_0804, 0x50), (k * 0x0004_080C, 0x48))
        } else {
            ((k * 0x0018_1008, 0), (k * 0x0006_0C14, 0))
        }
    }
    // The light across the row (y 30..56), then the rail below it: rows
    // 56..60 in the dimmed box, a run's tall light at 60..64 and rows
    // to 70, in the feather's reach and past it. The rail's marks.
    fn lay<S: BeamRows>(out: &mut S) -> (S::Mark, S::Mark) {
        out.slab();
        for y in 30..56 {
            let (a, b) = ends(y);
            out.row(i32::from(y / 20), y / 20, (30, y, 50), a, b);
        }
        let from = out.mark();
        out.slab();
        for y in 56..60 {
            let (a, b) = ends(y);
            out.row(3, 3, (30, y, 50), a, b);
        }
        out.rect(3, 3, (30, 60, 50, 4), (0x0020_1810, 0));
        for y in 64..70 {
            let (a, b) = ends(y);
            out.row(3, 3, (30, y, 50), a, b);
        }
        (from, out.mark())
    }
    let lead = GlowQuad::flat(1, 0, 10, 4, 4, 0x0010_1010, 0);
    let legacy = |rails: bool| {
        let mut quads = vec![lead];
        let (from, to) = lay(&mut quads);
        let mut ledger = dimmed();
        if rails {
            ledger.note_rail(from, to);
        }
        ledger.guard_mask_stream(&mut quads, &mut GlowRamps::default(), true);
        quads
    };
    let ramped = |(rails, rounded): (bool, bool)| {
        let (mut quads, mut ramps) = (vec![lead], GlowRamps::default());
        let (from, to) = {
            let mut sink = RampSink::new(&mut quads, &mut ramps).costed(RampCost::ALWAYS);
            lay(&mut sink)
        };
        assert!(
            to.1.items() > from.1.items(),
            "premise: the rail rides ramps"
        );
        let mut ledger = dimmed();
        if rails {
            ledger.note_rail_laid(from, to);
        } else {
            ledger.note_rail(from.0, to.0);
        }
        ledger.round_cuts = rounded;
        ledger.guard_mask_stream(&mut quads, &mut ramps, true);
        let mut got = Vec::new();
        ramps.expand_into(&quads, &mut got);
        (got, ramps.laid(quads.len()))
    };
    let key = |q: &GlowQuad| (q.y, q.x, q.w, q.h, q.color, q.alpha, q.color2, q.alpha2);
    let sorted = |mut v: Vec<GlowQuad>| {
        v.sort_by_key(key);
        v
    };
    let want = legacy(true);
    assert_ne!(
        sorted(want.clone()),
        sorted(legacy(false)),
        "premise: the rail keeps light the feather would cut"
    );
    let (got, laid) = ramped((true, false));
    assert_eq!(
        sorted(got),
        sorted(want.clone()),
        "the ramps are cut as their quads"
    );
    assert_eq!(laid, want.len(), "the stream counts alike");
    assert_ne!(
        sorted(ramped((false, false)).0),
        sorted(want.clone()),
        "control: the rail's ramps not noted, they are cut as the rail's quads are"
    );
    assert_ne!(
        sorted(ramped((true, true)).0),
        sorted(want),
        "control: rounded, the ramps' cut pieces are their quads' truncated"
    );
}

/// The property tests' seeded randomness: a splitmix64 stream.
struct Mix(u64);

impl Mix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u32) -> u32 {
        (self.next() % u64::from(n.max(1))) as u32
    }

    fn chance(&mut self, in_ten: u32) -> bool {
        self.below(10) < in_ten
    }
}

/// What a reference reading leaves out, for the property test's
/// negative controls.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lapse {
    None,
    /// Each pixel under its first quad light alone.
    FirstLightOnly,
    /// No halo.
    NoHalos,
    /// No core reading ([`Judge::cover`]): the solid ink alone.
    NoCore,
    /// The core read in the bare colour, as if no light lay over the text.
    BareFg,
    /// The core read at every pixel of the cell, not only where its glyph lays ink.
    WholeCell,
    /// Each ring's grounds read by their least and greatest luminance, not by each
    /// channel's least and greatest (the guard before the underlined letters' review).
    Luminance,
}

/// The share the guard's first form weighed: at each share the
/// bisection tries, every pixel of the ring box composited light by
/// light in the cell's order (an additive cell's whole light summed and
/// scaled), every ink pixel read against the readings of each channel's
/// least and greatest ground of its whole ring, and as reading 1:1 where
/// those readings span its own (the judge's medians lie between them).
fn share_by_every_pixel(
    j: Judge,
    bar: f32,
    cell: &[(Stream, u32)],
    streams: Streams<'_>,
    lapse: Lapse,
) -> f32 {
    let lum = crate::color_math::relative_luminance;
    let channel_pick = |a: u32, b: u32, max: bool| -> u32 {
        [16u32, 8, 0].into_iter().fold(0, |out, sh| {
            let (x, y) = ((a >> sh) & 0xff, (b >> sh) & 0xff);
            out | (if max { x.max(y) } else { x.min(y) }) << sh
        })
    };
    let halos = streams.halos;
    let cell: Vec<(Stream, u32)> = cell
        .iter()
        .copied()
        .filter(|&(st, _)| {
            lapse != Lapse::NoHalos || !matches!(st, Stream::HaloAdd | Stream::HaloOver)
        })
        .collect();
    let additive = cell.iter().all(|&(st, i)| match st {
        Stream::HaloAdd => true,
        Stream::HaloOver => false,
        _ => {
            let q = streams.quad(st, i as usize);
            q.alpha | q.alpha2 == 0
        }
    });
    let has_over = cell.iter().any(|&(st, _)| !st.on_ground());
    let covers = |st: Stream, i: u32, x: i32, y: i32| -> bool {
        let (bx, by, bw, bh) = match st {
            Stream::HaloAdd | Stream::HaloOver => {
                let h = &halos[i as usize];
                (h.x, h.y, h.w, h.h)
            }
            _ => {
                let q = streams.quad(st, i as usize);
                (q.x, q.y, q.w, q.h)
            }
        };
        x >= i32::from(bx)
            && x < i32::from(bx) + i32::from(bw)
            && y >= i32::from(by)
            && y < i32::from(by) + i32::from(bh)
    };
    // A pixel's composite over `base` at share `s`, the ground's lights
    // (`ground`) or the over-text ones alone.
    let px = |base: u32, x: i32, y: i32, ground: bool, s: f32| -> u32 {
        if additive {
            let mut lift = [0u16; 3];
            for &(st, i) in &cell {
                if !covers(st, i, x, y) || (st.on_ground() && !ground) {
                    continue;
                }
                let c = match st {
                    Stream::HaloAdd => {
                        let h = &halos[i as usize];
                        aterm_render::premul_rgb(h.color & 0x00FF_FFFF, halo_wt(h, x, y))
                    }
                    _ => quad_at(streams.quad(st, i as usize), x, 1.0).0,
                };
                for (k, sh) in [16u32, 8, 0].into_iter().enumerate() {
                    lift[k] = lift[k].saturating_add(((c >> sh) & 0xff) as u16);
                }
            }
            let ch = |sh: u32, l: u16| (((base >> sh) & 0xff) + (f32::from(l) * s) as u32).min(255);
            return (ch(16, lift[0]) << 16) | (ch(8, lift[1]) << 8) | ch(0, lift[2]);
        }
        let mut p = base;
        let mut quads_laid = 0;
        for &(st, i) in &cell {
            if !covers(st, i, x, y) {
                continue;
            }
            match st {
                Stream::HaloAdd => {
                    let h = &halos[i as usize];
                    let wt = halo_wt(h, x, y);
                    if wt > 0 {
                        let c = if s >= 1.0 {
                            h.color & 0x00FF_FFFF
                        } else {
                            scale_rgb(h.color & 0x00FF_FFFF, s)
                        };
                        p = aterm_render::add_sat(p, aterm_render::premul_rgb(c, wt));
                    }
                }
                Stream::HaloOver => {
                    let h = &halos[i as usize];
                    let wt = halo_wt(h, x, y);
                    if let Some(cap) = over_cap(h.color, s)
                        && wt > 0
                    {
                        p = aterm_render::over_rgb(p, h.color & 0x00FF_FFFF, wt.min(cap));
                    }
                }
                _ => {
                    if st.on_ground() && !ground {
                        continue;
                    }
                    let (c, a) = quad_at(streams.quad(st, i as usize), x, s);
                    if (c, a) == (0, 0) {
                        continue;
                    }
                    quads_laid += 1;
                    if lapse == Lapse::FirstLightOnly && quads_laid > 1 {
                        continue;
                    }
                    p = aterm_render::over_premul(p, c, a);
                }
            }
        }
        p
    };
    let holds = |s: f32| -> bool {
        let r = RING;
        if j.x1 - j.x0 <= 2 * r || j.y1 - j.y0 <= 2 * r {
            return true;
        }
        let ratio = |a: f32, b: f32| (a.max(b) + 0.05) / (a.min(b) + 0.05);
        for y in j.y0 + r..j.y1 - r {
            for x in j.x0 + r..j.x1 - r {
                let li = if has_over {
                    lum(px(j.ink, x, y, false, s))
                } else {
                    j.l_ink
                };
                let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
                let (mut lo_c, mut hi_c) = (0x00FF_FFFF_u32, 0u32);
                for yy in y - r..=y + r {
                    for xx in x - r..=x + r {
                        let g = px(j.ground, xx, yy, true, s);
                        let l = lum(g);
                        lo = lo.min(l);
                        hi = hi.max(l);
                        lo_c = channel_pick(lo_c, g, false);
                        hi_c = channel_pick(hi_c, g, true);
                    }
                }
                let channels = lapse != Lapse::Luminance;
                if channels {
                    (lo, hi) = (lum(lo_c), lum(hi_c));
                }
                if (channels && 1.0 < bar && lo <= li && li <= hi)
                    || ratio(li, lo) < bar
                    || ratio(li, hi) < bar
                {
                    return false;
                }
                // The core: its coverage of the colour under the over-text lights, the
                // rest each ring ground's own lit ground.
                let [bx0, by0, bx1, by1] = j.core_box;
                let seated = (bx0..bx1).contains(&(x - j.x0)) && (by0..by1).contains(&(y - j.y0));
                if let Some((c, bar_c)) = j.cover
                    && lapse != Lapse::NoCore
                    && (seated || lapse == Lapse::WholeCell)
                {
                    let f = if has_over && lapse != Lapse::BareFg {
                        lum(px(j.fg, x, y, false, s))
                    } else {
                        j.l_fg
                    };
                    let on = |g: f32| ratio(c * f + (1.0 - c) * g, g);
                    if (lo <= f && f <= hi) || on(lo) < bar_c || on(hi) < bar_c {
                        return false;
                    }
                }
            }
        }
        true
    };
    if holds(1.0) {
        return 1.0;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..STEPS {
        let mid = 0.5 * (lo + hi);
        if holds(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// A cell's ring box, bar, lights by stream, halos and lights in order.
type RandomCell = (
    Judge,
    f32,
    [Vec<GlowQuad>; 4],
    Vec<RainHalo>,
    Vec<(Stream, u32)>,
);

/// A random cell's ring box, text and lights: quads and one-pixel
/// rows on the ground and over the ink, gradients and flat ones, adding
/// and source-over, and halos of both modes, laid in the guard's order.
fn random_cell(mix: &mut Mix) -> RandomCell {
    let (x0, y0) = (16 + mix.below(400) as i32, 16 + mix.below(300) as i32);
    let (w, h) = if mix.chance(8) {
        (18 + 2 * RING, 38 + 2 * RING)
    } else {
        (3 + mix.below(28) as i32, 3 + mix.below(46) as i32)
    };
    let (x1, y1) = (x0 + w, y0 + h);
    let rgb = |mix: &mut Mix| (mix.next() as u32) & 0x00FF_FFFF;
    let (ink, ground) = (rgb(mix), rgb(mix));
    let additive = mix.chance(3);
    let premul = |mix: &mut Mix, a: u8| -> u32 {
        if a == 0 {
            return (mix.next() as u32) & 0x003F_3F3F;
        }
        let ch = |mix: &mut Mix| mix.below(u32::from(a) + 1);
        (ch(mix) << 16) | (ch(mix) << 8) | ch(mix)
    };
    let quad = |mix: &mut Mix, row: bool| -> GlowQuad {
        let a = if additive { 0 } else { mix.below(256) as u8 };
        let a2 = if additive || mix.chance(5) {
            a
        } else {
            mix.below(256) as u8
        };
        let color = premul(mix, a);
        let color2 = if mix.chance(3) {
            color
        } else {
            premul(mix, a2)
        };
        GlowQuad {
            row: 0,
            x: (x0 - 8 + mix.below(w as u32 + 16) as i32) as u16,
            y: (y0 - 4 + mix.below(h as u32 + 8) as i32) as u16,
            w: 1 + mix.below(if row { 9 } else { 30 }) as u16,
            h: if row { 1 } else { 1 + mix.below(12) as u16 },
            color,
            alpha: a,
            color2,
            alpha2: a2,
        }
    };
    let mut quads: [Vec<GlowQuad>; 4] = Default::default();
    let mut cell = Vec::new();
    // The under stream's quads and rows interleaved, then the over
    // stream's, as `row_lights` lays them.
    for (stream, row_stream, from) in [
        (Stream::Under, Stream::UnderRow, 0),
        (Stream::Over, Stream::OverRow, 2),
    ] {
        let n = mix.below(if from == 0 { 60 } else { 12 });
        for _ in 0..n {
            let row = mix.chance(6);
            let (st, at) = if row {
                (row_stream, from + 1)
            } else {
                (stream, from)
            };
            quads[at].push(quad(mix, row));
            cell.push((st, (quads[at].len() - 1) as u32));
        }
    }
    let mut halos = Vec::new();
    for mode in [HaloMode::Add, HaloMode::Over] {
        if (mode == HaloMode::Over && additive) || !mix.chance(3) {
            continue;
        }
        for _ in 0..1 + mix.below(2) {
            let (cx, cy) = (
                x0 + mix.below(w as u32) as i32,
                y0 + mix.below(h as u32) as i32,
            );
            let (rx, ry) = (2 + mix.below(24) as i32, 2 + mix.below(24) as i32);
            let color = if mode == HaloMode::Add {
                premul(mix, 0)
            } else {
                rgb(mix) | 0x8000_0000
            };
            halos.push(RainHalo {
                row: 0,
                x: (cx - rx).max(0) as u16,
                y: (cy - ry).max(0) as u16,
                w: (2 * rx + 1) as u16,
                h: (2 * ry + 1) as u16,
                color,
                cx: cx as u16,
                cy: cy as u16,
                rx: rx as u16,
                ry: ry as u16,
                mode,
            });
            let st = if mode == HaloMode::Add {
                Stream::HaloAdd
            } else {
                Stream::HaloOver
            };
            cell.push((st, (halos.len() - 1) as u32));
        }
    }
    let solid = solid_ink(ink, ground);
    let lum = crate::color_math::relative_luminance;
    let (l_ink, l_ground) = (lum(solid), lum(ground));
    let ratio = |a: f32, b: f32| (a.max(b) + 0.05) / (a.min(b) + 0.05);
    let bar = letter_bar(ratio(l_ink, l_ground), true);
    // Half the cells' faces lay the core at a share of the colour, read at its own bar.
    let l_fg = lum(ink);
    let cover = mix.chance(5).then(|| {
        let c = 0.3 + 0.69 * (mix.below(1000) as f32 / 1000.0);
        let l_c = c * l_fg + (1.0 - c) * l_ground;
        (c, letter_bar(ratio(l_c, l_ground), true))
    });
    // Where the glyph lays ink: some box of the cell, or the whole of it.
    let (iw, ih) = ((w - 2 * RING).max(1), (h - 2 * RING).max(1));
    let core_box = if mix.chance(7) {
        let (bx, by) = (mix.below(iw as u32) as i32, mix.below(ih as u32) as i32);
        let (bw, bh) = (
            1 + mix.below((iw - bx) as u32) as i32,
            1 + mix.below((ih - by) as u32) as i32,
        );
        [RING + bx, RING + by, RING + bx + bw, RING + by + bh]
    } else {
        [RING, RING, w - RING, h - RING]
    };
    let j = Judge {
        x0,
        y0,
        x1,
        y1,
        ink: solid,
        ground,
        l_ink,
        l_ground,
        fg: ink,
        l_fg,
        cover,
        core_box,
        ring: false,
        hold: RasterHold::Bar,
    };
    (j, bar, quads, halos, cell)
}

/// The guard's share for a cell, laid once and read row by row at each
/// share, is the share the first form weighed composing every pixel
/// of the ring box light by light, over randomized cells, the core of
/// a face that lays it at a share of the colour read too where its glyph
/// lays ink; each negative control (a reading that drops the stacked
/// lights, the halos, the core, or the over-text lights on the core's
/// colour, reads the core over the whole cell, or reads a ring's grounds
/// by their luminance) differs from it on some cell, so the cells
/// exercise each.
#[test]
fn the_guard_s_share_is_the_share_of_every_pixel_composited() {
    let mut mix = Mix(0x6C65_7474_6572);
    let mut guard = LetterGuard::default();
    let (mut cut, mut lapses) = (0, [0usize; 6]);
    let cases = 1500;
    for case in 0..cases {
        let (j, bar, quads, halos, cell) = random_cell(&mut mix);
        let streams = Streams {
            quads: [&quads[0], &quads[1], &quads[2], &quads[3]],
            halos: &halos,
        };
        guard.cell.clear();
        guard.cell.extend_from_slice(&cell);
        let got = guard.share(j, bar, streams);
        let want = share_by_every_pixel(j, bar, &cell, streams, Lapse::None);
        assert_eq!(
            got.to_bits(),
            want.to_bits(),
            "case {case}: the guard's share {got} against every pixel's {want}"
        );
        cut += usize::from(got < 1.0);
        for (k, lapse) in [
            Lapse::FirstLightOnly,
            Lapse::NoHalos,
            Lapse::NoCore,
            Lapse::BareFg,
            Lapse::WholeCell,
            Lapse::Luminance,
        ]
        .into_iter()
        .enumerate()
        {
            // A control is shown once it misreads one cell; more decide nothing.
            if lapses[k] == 0 {
                let off = share_by_every_pixel(j, bar, &cell, streams, lapse);
                lapses[k] += usize::from(off.to_bits() != want.to_bits());
            }
        }
    }
    assert!(
        cut > cases / 10,
        "the cells cut too seldom to weigh the bisection: {cut}"
    );
    assert!(
        lapses[0] > 0,
        "control: no cell's share read its stacked lights"
    );
    assert!(lapses[1] > 0, "control: no cell's share read its halos");
    assert!(lapses[2] > 0, "control: no cell's share read its core");
    assert!(
        lapses[3] > 0,
        "control: no cell's core read the lights over the text"
    );
    assert!(
        lapses[4] > 0,
        "control: no cell's core read only where its glyph lays ink"
    );
    assert!(
        lapses[5] > 0,
        "control: no cell's share read its grounds by each channel"
    );
}

/// The ring box's cell, px, for [`a_ring_cell_is_painted_as_every_pixel_composited`]: the
/// size [`random_cell`] gives most of its boxes.
const RING_CELL: (usize, usize) = (18, 38);

/// Rasters of a [`RING_CELL`] cell: strokes of full coverage with a part-covered edge, and a
/// rim so faint it rounds to the ground, which a ring can hold.
fn ring_rasters() -> ring::RasterTable {
    let (cw, ch) = RING_CELL;
    let raster = |x: std::ops::Range<usize>, y: std::ops::Range<usize>| {
        let mut cov = vec![0u8; cw * ch];
        for yy in y.clone() {
            for xx in x.clone() {
                cov[yy * cw + xx] = 255;
            }
            cov[yy * cw + x.end] = 120;
            cov[yy * cw + x.start - 1] = 1;
        }
        Some(aterm_render::GlyphRaster::placed(&cov, cw, (0, 0)).expect("ink"))
    };
    vec![
        raster(4..6, 6..30),
        raster(2..16, 18..20),
        raster(8..11, 2..36),
        raster(1..17, 30..33),
    ]
    .into()
}

/// A cell's neighbourhood: `ground` under its glyph, most neighbours on that ground, some
/// lettered, some on another ground or under a fill, some opaque.
fn random_hood(mix: &mut Mix, ground: u32, ink: u32) -> [ring::Neighbour; 9] {
    std::array::from_fn(|i| {
        let glyph = |mix: &mut Mix| Some((mix.below(4) as u8, ink));
        if i == 4 {
            return ring::Neighbour::Known {
                ground,
                base: if mix.chance(2) {
                    ink ^ 0x0040_4040
                } else {
                    ground
                },
                glyph: glyph(mix),
            };
        }
        if mix.chance(1) {
            return ring::Neighbour::Opaque;
        }
        let g = if mix.chance(2) {
            ground ^ 0x0020_1008
        } else {
            ground
        };
        ring::Neighbour::Known {
            ground: g,
            base: if mix.chance(2) { g ^ 0x0008_1020 } else { g },
            glyph: if mix.chance(4) { glyph(mix) } else { None },
        }
    })
}

/// A ring box's frame at share `s`, every pixel in `read` composited light by light as the
/// renderer draws it: on its neighbour's base, a pixel a glyph lays text on takes the
/// under-text lights, its glyphs' levels in draw order, then the over-text lights; any other
/// pixel takes every quad; both take the halos last.
fn ring_frame_by_every_pixel(
    (j, cells, table): (Judge, &[ring::Neighbour; 9], &ring::RasterTable),
    (cell, streams): (&[(Stream, u32)], Streams<'_>),
    read: &[usize],
    s: f32,
) -> Vec<u32> {
    let (cw, ch) = (RING_CELL.0 as i64, RING_CELL.1 as i64);
    let (w, r) = ((j.x1 - j.x0) as i64, i64::from(RING));
    let h = i64::from(j.y1 - j.y0);
    let side = |v: i64, cell: i64| {
        if v < r {
            0
        } else if v < r + cell {
            1
        } else {
            2
        }
    };
    // What no reading reads holds nothing a reading could use.
    let mut frame = vec![0x00C0_0FFEu32; (w * h) as usize];
    for &k in read {
        let (bx, by) = (k as i64 % w, k as i64 / w);
        let (x, y) = (j.x0 + bx as i32, j.y0 + by as i32);
        let at = (side(by, ch) * 3 + side(bx, cw)) as usize;
        let ring::Neighbour::Known { base, .. } = cells[at] else {
            panic!("a read pixel in an opaque cell");
        };
        // The levels the neighbourhood's glyphs lay here, in draw order.
        let mut levels = Vec::new();
        for (i, cell) in cells.iter().enumerate() {
            let ring::Neighbour::Known {
                ground: g,
                glyph: Some((glyph, fg)),
                ..
            } = *cell
            else {
                continue;
            };
            let raster = table[usize::from(glyph)].as_ref().expect("a raster");
            let ox = r + (i as i64 % 3 - 1) * cw + i64::from(raster.x);
            let oy = r + (i as i64 / 3 - 1) * ch + i64::from(raster.y);
            let (rx, ry) = (bx - ox, by - oy);
            if (0..raster.w as i64).contains(&rx) && (0..raster.h() as i64).contains(&ry) {
                let cov = raster.cov[ry as usize * raster.w + rx as usize];
                if cov != 0 {
                    levels.push(aterm_render::TextLevel::new(fg, g, cov, false));
                }
            }
        }
        let covers = |q: &GlowQuad| {
            x >= i32::from(q.x)
                && x < i32::from(q.x) + i32::from(q.w)
                && y >= i32::from(q.y)
                && y < i32::from(q.y) + i32::from(q.h)
        };
        let quads_over = |mut p: u32, ground: bool, over: bool| {
            for &(st, i) in cell {
                if matches!(st, Stream::HaloAdd | Stream::HaloOver)
                    || (st.on_ground() && !ground)
                    || (!st.on_ground() && !over)
                {
                    continue;
                }
                let q = streams.quad(st, i as usize);
                if covers(q) {
                    let (c, a) = quad_at(q, x, s);
                    if (c, a) != (0, 0) {
                        p = aterm_render::over_premul(p, c, a);
                    }
                }
            }
            p
        };
        let mut p = if levels.is_empty() {
            quads_over(base, true, true)
        } else {
            let under = quads_over(base, true, false);
            let on = levels.iter().fold(under, |p, level| level.over(p));
            quads_over(on, false, true)
        };
        for &(st, i) in cell {
            if !matches!(st, Stream::HaloAdd | Stream::HaloOver) {
                continue;
            }
            let hl = &streams.halos[i as usize];
            let reaches = x >= i32::from(hl.x)
                && x < i32::from(hl.x) + i32::from(hl.w)
                && y >= i32::from(hl.y)
                && y < i32::from(hl.y) + i32::from(hl.h);
            let wt = if reaches { halo_wt(hl, x, y) } else { 0 };
            if wt == 0 {
                continue;
            }
            let rgb = hl.color & 0x00FF_FFFF;
            if st == Stream::HaloAdd {
                let c = if s >= 1.0 { rgb } else { scale_rgb(rgb, s) };
                p = aterm_render::add_sat(p, aterm_render::premul_rgb(c, wt));
            } else if let Some(cap) = over_cap(hl.color, s) {
                p = aterm_render::over_rgb(p, rgb, wt.min(cap));
            }
        }
        frame[k] = p;
    }
    frame
}

/// A RING CELL IS PAINTED AND WEIGHED AS EVERY PIXEL COMPOSITED. Over randomized neighbourhoods
/// under randomized lights, at the shares the bisection tries: every pixel a reading reads
/// ([`LetterGuard::paint_ring`]) is the one [`ring_frame_by_every_pixel`] composes light by
/// light, [`LetterGuard::holds_ring`] answers as the reading of that frame does either side of
/// it with the whole reading at its own bar (`ring::RingCell::whole_holds`), and the guard's
/// share is that frame's bisection. A paint of those pixels alone (the solid ink and its rings,
/// the core and its bed) is exact only if a reading reads no other, which the poison in every
/// other pixel of its frame shows.
/// NEGATIVE CONTROLS (2026-11-05), each fails its pixel comparison: no light beneath the text
/// taken for read (`lit_under` never set), a read of the solid ink's pixels without their rings
/// (on a ring pixel), a dropped `more_under` (on a pixel under two lights), and text levels
/// left off a cell lit under its text. A third of the cases lose their lights beneath the text,
/// for the arm of the paint that takes the text as the read laid it, which random lights seldom
/// reach: a read laying that text on the cell's ground and not its neighbour's base fired "case 6
/// share 1: box pixel 443 (x 3, y 20)", and that arm leaving the text off fired "case 0 share 1:
/// box pixel 444 (x 4, y 20)".
#[test]
fn a_ring_cell_is_painted_as_every_pixel_composited() {
    struct Px<'a>(&'a [u32]);
    impl ring::FramePx for Px<'_> {
        fn at(&self, k: usize) -> u32 {
            self.0[k]
        }
    }
    let table = ring_rasters();
    let mut mix = Mix(0x7269_6E67);
    let mut guard = LetterGuard::default();
    let (cw, ch) = RING_CELL;
    let (w, h) = (cw + 2 * RING as usize, ch + 2 * RING as usize);
    let (mut weighed, mut cut, mut under, mut over, mut halos_on, mut rim, mut bare) =
        (0, 0, 0, 0, 0, 0, 0);
    for case in 0..240 {
        let (j0, _, quads, halos, mut cell) = random_cell(&mut mix);
        if (j0.x1 - j0.x0, j0.y1 - j0.y0) != (w as i32, h as i32) {
            continue;
        }
        // Nearly every random cell has a light beneath its text, so some are left with none:
        // a letter under a cursor trail's fading fill, whose text lies on its neighbour's base.
        if case % 3 == 0 {
            cell.retain(|&(st, _)| !st.on_ground());
            if cell.is_empty() {
                continue;
            }
        }
        let j = Judge::ring((j0.x0, j0.y0, j0.x1, j0.y1), (j0.ink, j0.ground));
        let cells = random_hood(&mut mix, j.ground, j.ink);
        let streams = Streams {
            quads: [&quads[0], &quads[1], &quads[2], &quads[3]],
            halos: &halos,
        };
        guard
            .ring
            .read((w, h), (cw, ch), RING as usize, &cells, (&table, false));
        let read = guard.ring.read_pixels();
        if read.is_empty() {
            continue;
        }
        weighed += 1;
        let has_over = cell.iter().any(|&(st, _)| !st.on_ground());
        over += usize::from(has_over);
        halos_on += usize::from(!halos.is_empty());
        rim += usize::from(guard.ring.ground_text);
        guard.cell.clear();
        guard.cell.extend_from_slice(&cell);
        guard.lay_lights(j, streams, has_over);
        under += usize::from(guard.lit_under);
        bare += usize::from(!guard.lit_under);
        let frame_at =
            |s: f32| ring_frame_by_every_pixel((j, &cells, &table), (&cell, streams), &read, s);
        for s in [1.0, 0.5, 0.75, 0.25, 0.125, 0.875, 1.0 / 128.0] {
            let want = frame_at(s);
            guard.paint_ring(s, w, has_over);
            for &k in &read {
                assert_eq!(
                    guard.px_text[k] & 0x00FF_FFFF,
                    want[k],
                    "case {case} share {s}: box pixel {k} (x {}, y {})",
                    k % w,
                    k / w
                );
            }
            let reading = guard.ring.reading(&Px(&want));
            let whole = guard.ring.whole_holds(&Px(&want), RasterHold::Bar);
            for (bar, holds) in [
                (reading.next_down(), true),
                (reading, true),
                (reading.next_up(), reading.is_infinite()),
            ] {
                assert_eq!(
                    guard.holds_ring(j, s, bar, has_over),
                    holds && whole,
                    "case {case} share {s}: holds {bar} on a reading of {reading}, the whole \
                     reading holding {whole}"
                );
            }
        }
        let bar = raster_bar(guard.ring.own);
        let holds = |guard: &mut LetterGuard, s: f32| {
            let frame = frame_at(s);
            guard.ring.reading(&Px(&frame)) >= bar
                && guard.ring.whole_holds(&Px(&frame), RasterHold::Bar)
        };
        let want = if holds(&mut guard, 1.0) {
            1.0
        } else {
            let (mut lo, mut hi) = (0.0f32, 1.0f32);
            for _ in 0..STEPS {
                let mid = 0.5 * (lo + hi);
                if holds(&mut guard, mid) {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            lo
        };
        let got = guard.share(j, bar, streams);
        assert_eq!(got.to_bits(), want.to_bits(), "case {case}: {got} {want}");
        cut += usize::from(got < 1.0);
    }
    for (what, n) in [
        ("weighed", weighed),
        ("cut", cut),
        ("lit under its text", under),
        ("with no light beneath its text", bare),
        ("lit over it", over),
        ("under a halo", halos_on),
        ("with text on a pixel read as its ground", rim),
    ] {
        assert!(n > 0, "no cell {what}");
    }
}

/// A ground and a letter's ink for [`the_bounds_answer_as_the_bisection`]: mostly a dark page
/// and light ink, which a bound can pass whole, and some paper and dark ink, which no bound
/// answers.
fn bounded_pair(mix: &mut Mix) -> (u32, u32) {
    let rgb = |r: u32, g: u32, b: u32| (r << 16) | (g << 8) | b;
    if mix.chance(4) {
        let ground = rgb(
            232 + mix.below(24),
            232 + mix.below(24),
            214 + mix.below(42),
        );
        return (ground, rgb(mix.below(120), mix.below(120), mix.below(120)));
    }
    let ground = rgb(mix.below(56), mix.below(56), mix.below(56));
    let ink = rgb(
        150 + mix.below(106),
        150 + mix.below(106),
        150 + mix.below(106),
    );
    (ground, ink)
}

/// A thin stroke's rasters for [`the_bounds_answer_as_the_bisection`]: [`ring_rasters`]' four,
/// then strokes of one full column between four part-covered ones, each at its own coverage, so
/// that the glyph's core (pixels [`aterm_render::glyph_ink::CORE_SHARE`] of the way to its
/// extreme) is mostly part-covered ink, dimmer than its solid ink: its whole reading falls under
/// its bar on a lifted ground where its ring still keeps its own.
fn bound_rasters() -> ring::RasterTable {
    let (cw, ch) = RING_CELL;
    let mut table: Vec<_> = ring_rasters().iter().cloned().collect();
    for part in [180u8, 200, 220, 240] {
        let mut cov = vec![0u8; cw * ch];
        for y in 3..35 {
            for (x, c) in [(6, part), (7, part), (8, 255), (9, part), (10, part)] {
                cov[y * cw + x] = c;
            }
        }
        table.push(Some(
            aterm_render::GlyphRaster::placed(&cov, cw, (0, 0)).expect("ink"),
        ));
    }
    table.into()
}

/// THE BOUNDS ANSWER AS THE BISECTION. A cell read by its glyph's own pixels whose lights all
/// add, with no cursor trail's fill in its ring box and no text on a pixel a reading reads as its
/// ground, is passed whole by [`LetterGuard::judge`] with no paint where its light ink on a darker
/// ground keeps its bar on every frame its lights can make: the brightest ground they can make
/// keeps its dimmest solid ink at its bar and its core at its whole reading's
/// ([`LetterGuard::ring_passes_whole`]). Over randomized neighbourhoods (fills and faint rims
/// among them, and thin strokes whose core is dimmer than their solid ink under a light washing
/// the whole box) on dark pages and paper under randomized adding lights, faint to bright, the
/// judge's share is the bisection's ([`LetterGuard::share`]) on every cell, under the frame's
/// lights ([`RasterHold::Bar`]) and under the page shades ([`RasterHold::Own`], the hold
/// [`CursorGlow::hold_page_shades`] judges with), and the bound passes some and refuses some for
/// their whole reading alone, which the bisection cuts.
/// NEGATIVE CONTROLS (2026-11-11): `filled` as it was (some box pixel's clean colour not its
/// neighbour's base, so every lettered cell) fired "no cell the bound passed whole"; the fill left
/// unread fired "case 1, #ad99a2 on #1b0228: the judge's share 1 (the bound passing it: true)
/// against the bisection's 0"; the whole reading left unread (`whole_clears`) fired "case 83, Bar,
/// #cbe1a0 on #052703: the judge's share 1 (the bound passing it: true) against the bisection's
/// 0.8203125". Text on the ground left unread, on the bed alone or anywhere, fails no case: each
/// faint rim here that rounds to the ground on the clean frame rounds to the lifted ground under
/// the lights, so that precondition of the bound is held and not pinned.
#[test]
fn the_bounds_answer_as_the_bisection() {
    let table = bound_rasters();
    let mut mix = Mix(0x0062_6F75_6E64);
    let mut guard = LetterGuard::default();
    let (cw, ch) = RING_CELL;
    let (w, h) = (cw + 2 * RING as usize, ch + 2 * RING as usize);
    let (mut passed, mut whole, mut cut, mut none) = (0, 0, 0, 0);
    let (mut filled, mut texted, mut refused, mut refused_cut) = (0, 0, 0, 0);
    let mut owned = 0;
    for case in 0..3000 {
        let (ground, ink) = bounded_pair(&mut mix);
        let (x0, y0) = (40 + mix.below(200) as i32, 40 + mix.below(200) as i32);
        let (x1, y1) = (x0 + w as i32, y0 + h as i32);
        // Faint to bright: a bound settles the first, the bisection weighs the last.
        let mask = [0x0001_0101_u32, 0x0003_0303, 0x000F_0F0F, 0x003F_3F3F][case % 4];
        let mut quads: [Vec<GlowQuad>; 4] = Default::default();
        let mut cell = Vec::new();
        for _ in 0..mix.below(9) {
            let at = mix.below(4) as usize;
            let row = at % 2 == 1;
            let color = (mix.next() as u32) & mask;
            let color2 = if mix.chance(5) {
                color
            } else {
                (mix.next() as u32) & mask
            };
            quads[at].push(GlowQuad {
                row: 0,
                x: (x0 - 6 + mix.below(w as u32 + 12) as i32) as u16,
                y: (y0 - 4 + mix.below(h as u32 + 8) as i32) as u16,
                w: 1 + mix.below(if row { 9 } else { 30 }) as u16,
                h: if row { 1 } else { 1 + mix.below(20) as u16 },
                color,
                alpha: 0,
                color2,
                alpha2: 0,
            });
            let st = [
                Stream::Under,
                Stream::UnderRow,
                Stream::Over,
                Stream::OverRow,
            ][at];
            cell.push((st, (quads[at].len() - 1) as u32));
        }
        // The guard's order: the under stream's lights, then the over stream's, then halos.
        cell.sort_by_key(|&(st, _)| u8::from(!st.on_ground()));
        let mut halos = Vec::new();
        if mix.chance(3) {
            let (cx, cy) = (
                x0 + mix.below(w as u32) as i32,
                y0 + mix.below(h as u32) as i32,
            );
            let (rx, ry) = (2 + mix.below(24) as i32, 2 + mix.below(24) as i32);
            halos.push(RainHalo {
                row: 0,
                x: (cx - rx).max(0) as u16,
                y: (cy - ry).max(0) as u16,
                w: (2 * rx + 1) as u16,
                h: (2 * ry + 1) as u16,
                color: (mix.next() as u32) & mask,
                cx: cx as u16,
                cy: cy as u16,
                rx: rx as u16,
                ry: ry as u16,
                mode: HaloMode::Add,
            });
            cell.push((Stream::HaloAdd, 0));
        }
        if cell.is_empty() {
            continue;
        }
        let mut cells = random_hood(&mut mix, ground, ink);
        // A third with no lettered neighbour, whose text would lie on pixels read as ground.
        if case % 3 == 0 {
            for (i, n) in cells.iter_mut().enumerate() {
                if let (false, ring::Neighbour::Known { glyph, .. }) = (i == 4, n) {
                    *glyph = None;
                }
            }
        }
        guard
            .ring
            .read((w, h), (cw, ch), RING as usize, &cells, (&table, false));
        // A sixth of the light letters on a dark page a thin stroke ([`bound_rasters`]) on its
        // bare page, alone under one grey light washing its whole box, the brightest that keeps
        // its ring's bar on the ground it lifts to: the bound reckons that ground exactly, and a
        // core dimmer than the solid ink may not keep its whole reading's bar there.
        let table_l = crate::color_math::relative_luminance_table();
        let lum = |c: u32| crate::color_math::relative_luminance_with(table_l, c);
        if case % 6 == 5 && lum(ink) > lum(ground) {
            for (i, n) in cells.iter_mut().enumerate() {
                if let ring::Neighbour::Known { glyph, base, .. } = n {
                    *glyph = (i == 4).then_some((4 + (case / 6 % 4) as u8, ink));
                    if i == 4 {
                        *base = ground;
                    }
                }
            }
            guard
                .ring
                .read((w, h), (cw, ch), RING as usize, &cells, (&table, false));
            let l_min = guard.ring.l_min;
            let bar = raster_bar(guard.ring.own);
            let grey = (0..=255u32).rev().find(|&g| {
                let lg = lum(lifted(ground, [g; 3]));
                lg < l_min && (l_min + 0.05) / (lg + 0.05) >= bar
            });
            if let Some(g) = grey {
                quads = Default::default();
                quads[0].push(GlowQuad {
                    row: 0,
                    x: (x0 - 2) as u16,
                    y: (y0 - 2) as u16,
                    w: w as u16 + 4,
                    h: h as u16 + 4,
                    color: 0x0001_0101 * g,
                    alpha: 0,
                    color2: 0x0001_0101 * g,
                    alpha2: 0,
                });
                cell = vec![(Stream::Under, 0)];
                halos.clear();
            }
        }
        let streams = Streams {
            quads: [&quads[0], &quads[1], &quads[2], &quads[3]],
            halos: &halos,
        };
        filled += usize::from(guard.ring.filled());
        texted += usize::from(guard.ring.ground_text);
        guard.cell.clear();
        guard.cell.extend_from_slice(&cell);
        for hold in [RasterHold::Bar, RasterHold::Own] {
            let before = (guard.bound_passed, guard.whole_refused);
            let got = guard.judge(
                (x0, y0, x1, y1),
                (ink, ground),
                &mut None,
                (None, Some(hold)),
                streams,
            );
            let bound = guard.bound_passed > before.0;
            let refused_whole = guard.whole_refused > before.1;
            let bar = hold.bar(guard.ring.own);
            let j = Judge {
                hold,
                ..Judge::ring((x0, y0, x1, y1), (ink, ground))
            };
            let want = guard.share(j, bar, streams);
            assert_eq!(
                got.to_bits(),
                want.to_bits(),
                "case {case}, {hold:?}, #{ink:06x} on #{ground:06x}: the judge's share {got} (the \
                 bound passing it: {bound}) against the bisection's {want}"
            );
            passed += usize::from(bound);
            owned += usize::from(bound && hold == RasterHold::Own);
            whole += usize::from(!bound && got >= 1.0);
            none += usize::from(got == 0.0);
            cut += usize::from(got > 0.0 && got < 1.0);
            refused += usize::from(refused_whole);
            refused_cut += usize::from(refused_whole && got < 1.0);
        }
    }
    for (what, n) in [
        ("the bound passed whole", passed),
        ("the bound passed whole under the page shades' hold", owned),
        ("the bisection kept whole", whole),
        ("given none", none),
        ("cut", cut),
        ("under a trail's fill", filled),
        ("with text on a pixel read as its ground", texted),
        ("the bound refused for its whole reading alone", refused),
        (
            "the bound refused for its whole reading alone and the bisection cut",
            refused_cut,
        ),
    ] {
        assert!(n > 0, "no cell {what}");
    }
}

/// A LETTER ON A DIM PAGE KEEPS A LEVEL IN HAND: read at the guard's bar on this frame, it still
/// reads the judges' share of its own with one level of any channel of its ink or its ground
/// off, on Solarized Dark and a dim grey, by its ring and by its whole reading on paper
/// (`raster_bar`, [`LEVEL_ROOM`]; a window's frame rounds that far off this one). A letter at
/// or over the floor keeps the floor, the room that was always there, and one at AA under it,
/// the flood's `poppy` on Default Light and a grey on white, keeps AA and the level: a level
/// off still reads AA. So does one the judges read at AA, to their two decimals ([`AA_READ`]):
/// the flood's `o` on GitHub Light, 4.4973. Negative controls: at [`letter_bar`] alone a level
/// down reads under the share; held at its whole reading, a level down reads one at 4.5 or
/// over under AA (the `o`, under 4.5 already, shows nothing there); and with AA held from 4.5
/// itself ([`with_aa_unrounded`]), the `o` a level down reads under the judges' AA.
#[test]
fn a_letter_on_a_dim_page_keeps_a_level_in_hand() {
    fn lum(c: u32) -> f32 {
        crate::color_math::relative_luminance(c)
    }
    fn ratio(a: u32, b: u32) -> f32 {
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }
    // What a level off costs a reading of `ink` on `ground`, the worst channel of either, as a
    // share of the reading.
    fn level_off(ink: u32, ground: u32) -> f32 {
        let r = ratio(ink, ground);
        let mut worst = 1.0f32;
        for sh in [16u32, 8, 0] {
            let nudged = |c: u32, by: i32| {
                let v = ((c >> sh) & 0xff) as i32 + by;
                (c & !(0xff << sh)) | (v.clamp(0, 255) as u32) << sh
            };
            for by in [-1, 1] {
                worst = worst
                    .min(ratio(nudged(ink, by), ground) / r)
                    .min(ratio(ink, nudged(ground, by)) / r);
            }
        }
        worst
    }
    // A letter's stroke on its page, and the same stroke as a light lays it: Solarized Dark's
    // `c` and a thinner stroke of it under a flame (`#929592` on `#3b3230`), a dim grey on
    // white, and the Cursor card's `.` on its lit bed (`#fffbf8`).
    for (name, ink, page, lit_ink, lit) in [
        (
            "solarized",
            0x007B_8C8Eu32,
            0x0000_2B36u32,
            0x0092_9592u32,
            0x003B_3230u32,
        ),
        (
            "solarized's rim",
            0x0077_888Au32,
            0x0000_2B36u32,
            0x0092_9592u32,
            0x003B_3230u32,
        ),
        (
            "paper grey",
            0x0078_7878u32,
            0x00FF_FFFFu32,
            0x0078_7878u32,
            0x00FF_E0D0u32,
        ),
        (
            "the card's dot",
            0x0080_8182u32,
            0x00FF_FFFFu32,
            0x0080_8182u32,
            0x00FF_FBF8u32,
        ),
    ] {
        let own = ratio(ink, page);
        assert!(
            own < WCAG_AA,
            "fixture: {name} reads {own} on its page, at AA or over"
        );
        let off = level_off(lit_ink, lit);
        assert!(
            raster_bar(own) * off >= own * OWN_SHARE,
            "{name}: held at {:.3} and a level off at {:.3}, under the judges' {:.3}",
            raster_bar(own),
            raster_bar(own) * off,
            own * OWN_SHARE
        );
        assert!(
            letter_bar(own, false) * off < own * OWN_SHARE,
            "control, {name}: at the judges' own bar a level off still reads its share"
        );
    }
    assert_eq!(raster_bar(LETTER_FLOOR), LETTER_FLOOR);
    assert_eq!(raster_bar(9.0), LETTER_FLOOR);
    let (mut whole_under, mut unrounded_under) = (0, 0);
    for (name, ink, page, lit_ink, lit) in [
        (
            "paper at AA",
            0x00C0_4082u32,
            0x00F6_F7F9u32,
            0x00C0_4082u32,
            0x00FF_E0D0u32,
        ),
        (
            "grey at AA",
            0x0076_7676u32,
            0x00FF_FFFFu32,
            0x0076_7676u32,
            0x00FF_E0D0u32,
        ),
        (
            "pink at the judges' AA",
            0x00C1_479Fu32,
            0x00FF_FFFFu32,
            0x00C1_479Fu32,
            0x00FF_E0D0u32,
        ),
    ] {
        let own = ratio(ink, page);
        assert!(
            (AA_READ..LETTER_FLOOR).contains(&own),
            "fixture: {name} reads {own} on its page"
        );
        let off = level_off(lit_ink, lit);
        assert!(
            raster_bar(own) * off >= WCAG_AA,
            "{name}: held at {:.3} and a level off at {:.3}, under AA",
            raster_bar(own),
            raster_bar(own) * off
        );
        // Only a letter at 4.5 or over can show the control: one the judges round up to AA
        // (the pink, 4.497) reads under it at its whole reading, a level off or not.
        whole_under += usize::from(own >= WCAG_AA && own * off < WCAG_AA);
        unrounded_under += usize::from(with_aa_unrounded(|| raster_bar(own)) * off < AA_READ);
    }
    assert!(
        whole_under > 0,
        "control: held at its whole reading, a level off reads AA for every letter at 4.5 or \
         over"
    );
    assert!(
        unrounded_under > 0,
        "control: with AA held from 4.5, a level off reads the judges' AA for every letter \
         at it"
    );
}

/// A quad's light read with no division ([`quad_light`]) is the
/// gradient law's ([`quad_at`]): at every column of every width up to
/// [`RECIP_W`] for the channel pairs that make the largest numerators,
/// and on randomized quads of widths past it too. The control, a
/// reciprocal rounded down, misreads some column.
#[test]
fn a_quad_s_light_read_with_no_division_is_the_gradient_law_s() {
    let quad = |w: u16, (c, a): (u32, u8), (c2, a2): (u32, u8)| GlowQuad {
        row: 0,
        x: 40,
        y: 0,
        w,
        h: 1,
        color: c,
        alpha: a,
        color2: c2,
        alpha2: a2,
    };
    let law = |q: &GlowQuad, x: i32| {
        let (c, a) = quad_at(q, x, 1.0);
        pack_light(c, a)
    };
    let ends = [
        (0x00FF_FFFF, 255),
        (0, 0),
        (0x00FF_00FF, 255),
        (0x0000_FF00, 1),
    ];
    let mut short = 0usize;
    for w in 1..=RECIP_W as u16 {
        for &a in &ends {
            for &b in &ends {
                let q = quad(w, a, b);
                for x in 40..40 + i32::from(w) {
                    assert_eq!(quad_light(&q, x), law(&q, x), "width {w} at {x}");
                }
                // The control: `⌊2^36 / 2w⌋` in place of the ceiling.
                let d = 2 * u64::from(w);
                let down = (1u64 << 36) / d;
                for i in 0..u32::from(w) {
                    let m = 2 * i + 1;
                    let n = u64::from(255 * (2 * u32::from(w) - m) + u32::from(w));
                    short += usize::from((n * down) >> 36 != n / d);
                }
            }
        }
    }
    assert!(
        short > 0,
        "control: a reciprocal rounded down read every column"
    );
    let mut mix = Mix(0x7175_6164);
    for case in 0..200_000 {
        let w = 1 + mix.below(if case % 4 == 0 { 3_000 } else { 64 }) as u16;
        let end = |mix: &mut Mix| (mix.next() as u32 & 0x00FF_FFFF, mix.below(256) as u8);
        let (a, b) = (end(&mut mix), end(&mut mix));
        let b = if mix.chance(3) { a } else { b };
        let q = quad(w, a, b);
        let x = 32 + mix.below(u32::from(w) + 16) as i32;
        assert_eq!(
            quad_light(&q, x),
            law(&q, x),
            "case {case}: width {w} at {x}"
        );
    }
}

/// A ground reads under one ink's bar (over `1.0`) exactly where
/// [`LetterGuard::clear_of`] puts it between its clear readings, as the
/// ring test of a cell with no over-text light reads it: over randomized
/// inks, bars and grounds, each bound and its neighbours among them, the
/// threshold test is the quotient's. The control, a dark bound one step
/// under, misreads one.
#[test]
fn a_ground_under_the_bar_reads_between_the_clear_readings() {
    let ratio = |a: f32, b: f32| (a.max(b) + 0.05) / (a.min(b) + 0.05);
    let mut guard = LetterGuard::default();
    let mut mix = Mix(0x7269_6E67);
    let unit = |mix: &mut Mix| (mix.next() >> 40) as f32 / (1u64 << 24) as f32;
    let mut short = 0usize;
    for case in 0..20_000 {
        let li = unit(&mut mix);
        let bar = 1.0f32.next_up() + 6.5 * unit(&mut mix);
        let (dark, light) = guard.clear_of(li, li, bar);
        let mut grounds: Vec<f32> = (0..16).map(|_| unit(&mut mix)).collect();
        for b in [dark, light] {
            if b.is_finite() {
                grounds.extend([b.next_down(), b, b.next_up()]);
            }
        }
        for g in grounds.into_iter().filter(|g| (0.0..=1.0).contains(g)) {
            assert_eq!(
                g > dark && g < light,
                ratio(li, g) < bar,
                "case {case}: ink {li}, bar {bar}, ground {g}, bounds {dark}..{light}"
            );
            let under = dark.next_down();
            short += usize::from((g > under && g < light) != (ratio(li, g) < bar));
        }
    }
    assert!(
        short > 0,
        "control: a dark bound one step under read every ground alike"
    );
}

/// A share of `k / 128` scales a byte exactly as the float product
/// does, for every byte and every `k` the bisection weighs; a share off
/// that grid (the control) keeps the float product.
#[test]
fn a_bisection_share_scales_a_byte_as_its_float_product() {
    for k in 1..128u32 {
        let s = k as f32 / 128.0;
        let share = Share::of(s);
        assert!(matches!(share, Share::Of128(kk) if kk == k), "k {k}");
        for c in 0..=255u32 {
            assert_eq!(share.scale(c), (c as f32 * s) as u32, "k {k}, byte {c}");
        }
    }
    assert!(matches!(Share::of(1.0), Share::Whole));
    assert!(
        matches!(Share::of(0.3), Share::Any(_)),
        "control: an off-grid share"
    );
}

/// A pixel one quad light lies on reads, at every share under one the
/// bisection weighs, at most [`LetterGuard::vouch`]'s bound from its
/// composite there, `max(d + 1, o' + 2)` per channel: for every pair of
/// shares `k < k'` on the grid over grounds, colours and opacities at
/// their extremes and at random. The controls, each bound a step
/// tighter (`o' + 1`, or the bare `d`), are crossed by some pixel.
#[test]
fn a_lesser_share_keeps_a_lone_light_s_pixel_under_its_vouched_bound() {
    let composite = |d: u32, c: u32, a: u32, k: u32| {
        over_packed(
            d,
            pack_light(c, a as u8),
            if k >= 128 {
                Share::Whole
            } else {
                Share::Of128(k)
            },
        )
    };
    let mut mix = Mix(0x0076_6F75_6368);
    let ends = [0u32, 1, 2, 127, 128, 253, 254, 255];
    let mut triples = Vec::new();
    for &d in &ends {
        for &c in &ends {
            for &a in &ends {
                triples.push((d, c, a));
            }
        }
    }
    for _ in 0..1500 {
        triples.push((mix.below(256), mix.below(256), mix.below(256)));
    }
    let (mut tight_o, mut tight_d) = (0usize, 0usize);
    for &(d, c, a) in &triples {
        let outs: Vec<u32> = (0..=128).map(|k| composite(d, c, a, k)).collect();
        for kk in 1..=128usize {
            let o2 = outs[kk];
            for (k, &o) in outs.iter().enumerate().take(kk).skip(1) {
                assert!(
                    o <= (d + 1).max(o2 + 2).min(255),
                    "ground {d}, colour {c}, opacity {a}: share {k} reads {o}, over the bound from {kk}'s {o2}"
                );
                tight_o += usize::from(o > (d + 1).max(o2 + 1).min(255));
                tight_d += usize::from(o > d.max(o2 + 2).min(255));
            }
        }
    }
    assert!(tight_o > 0, "control: `o' + 1` bounded every lesser share");
    assert!(
        tight_d > 0,
        "control: the bare ground bounded every lesser share"
    );
}

/// Cells the guard weighs near the whole first: no over-text light, no
/// halo, source-over quads scaled until the cell sits on its bar (the
/// faintest scale whose whole light it cuts). With the last frame's
/// [`NEAR_WHOLE`] noted for every cell, the guard's share is the share
/// it weighs with none noted and the share of every pixel composited;
/// some cells it vouches for at once ([`LetterGuard::vouch`]) and some
/// it does not, so both arms run.
#[test]
fn a_cell_weighed_near_the_whole_first_keeps_the_bisection_s_share() {
    let mut mix = Mix(0x6E65_6172);
    let mut guard = LetterGuard::default();
    let (mut vouched, mut weighed, mut trusting, mut cells) = (0usize, 0usize, 0usize, 0usize);
    while cells < 300 {
        let (j, bar, quads, _, cell) = random_cell(&mut mix);
        let cell: Vec<(Stream, u32)> = cell.into_iter().filter(|&(st, _)| st.on_ground()).collect();
        if cell.is_empty()
            || quads[0]
                .iter()
                .chain(&quads[1])
                .all(|q| q.alpha | q.alpha2 == 0)
        {
            continue;
        }
        let scaled = |f: u32| -> [Vec<GlowQuad>; 4] {
            let by = |v: u32| v * f / 256;
            let sc =
                |c: u32| (by((c >> 16) & 0xff) << 16) | (by((c >> 8) & 0xff) << 8) | by(c & 0xff);
            let mut out: [Vec<GlowQuad>; 4] = Default::default();
            for (o, qs) in out.iter_mut().zip(&quads) {
                o.extend(qs.iter().map(|q| GlowQuad {
                    color: sc(q.color),
                    color2: sc(q.color2),
                    alpha: by(u32::from(q.alpha).max(1)) as u8,
                    alpha2: by(u32::from(q.alpha2).max(1)) as u8,
                    ..*q
                }));
            }
            out
        };
        let weigh = |guard: &mut LetterGuard, qs: &[Vec<GlowQuad>; 4], noted: bool| {
            let streams = Streams {
                quads: [&qs[0], &qs[1], &qs[2], &qs[3]],
                halos: &[],
            };
            guard.cell.clear();
            guard.cell.extend_from_slice(&cell);
            guard.near_whole.clear();
            if noted {
                guard.near_whole.push(box_at(j.x0, j.y0));
            }
            guard.share(j, bar, streams)
        };
        // The faintest scale (of 256) whose whole light the cell cuts.
        if weigh(&mut guard, &scaled(256), false) >= 1.0 {
            continue;
        }
        let (mut lo, mut hi) = (0u32, 256u32);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if weigh(&mut guard, &scaled(mid), false) >= 1.0 {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let qs = scaled(hi);
        let plain = weigh(&mut guard, &qs, false);
        let near = weigh(&mut guard, &qs, true);
        let streams = Streams {
            quads: [&qs[0], &qs[1], &qs[2], &qs[3]],
            halos: &[],
        };
        let want = share_by_every_pixel(j, bar, &cell, streams, Lapse::None);
        assert_eq!(
            plain.to_bits(),
            want.to_bits(),
            "cell {cells}: share {plain} against every pixel's {want}"
        );
        assert_eq!(
            near.to_bits(),
            want.to_bits(),
            "cell {cells}: weighed near the whole first {near}, every pixel {want}"
        );
        // Whether the near-whole share holds by its composite alone.
        let flags = (guard.additive(streams), false);
        if flags.0 {
            continue;
        }
        cells += 1;
        guard.cell.clear();
        guard.cell.extend_from_slice(&cell);
        guard.lay_lights(j, streams, false);
        if guard.holds(j, NEAR_WHOLE, bar, flags) {
            weighed += 1;
            vouched += usize::from(guard.vouch(j, NEAR_WHOLE, bar));
            // The control: a probe that took the near-whole share's own
            // verdict for every lesser one.
            trusting += usize::from(want != NEAR_WHOLE);
        }
    }
    assert!(
        vouched > 0,
        "no cell held near the whole by its composite alone"
    );
    assert!(
        vouched < weighed,
        "every cell that held near the whole held by its composite alone: {vouched}"
    );
    assert!(
        trusting > 0,
        "control: every cell that held near the whole kept that share"
    );
}

/// The guard's ramp cut reads its dims as a fresh reading does
/// ([`InkLedger::knots`]): over randomized dims across several text
/// rows and a stream of rows and tall pieces over them, laid once as
/// quads (each sliced by dims gathered afresh) and once as ramps (a row
/// whose one-row box gathers the dims the row above gathered reusing
/// them, and a row every near dim spans reusing the row above's masks),
/// the two are cut alike, light for light. At every point a slice reads
/// of a randomized box, sliced by one ledger box after box as a fresh
/// ledger slices it, the mask over the dims `knots` kept near is the
/// mask over every dim, and a box `knots` finds no dim near reads the
/// whole light at its corners. Premise: the ramps gathered their near
/// dims fewer times than their quads did, so the reuse ran.
#[test]
fn a_ramp_cut_reuses_its_near_dims_as_a_fresh_reading_reads_them() {
    use aterm_render::{BeamRows, RampCost, RampSink};
    #[derive(Clone, Copy)]
    enum Piece {
        Row(u16, u16, u16, (u32, u8), (u32, u8)),
        Tall(u16, u16, u16, u16, (u32, u8)),
    }
    fn lay<S: BeamRows>(out: &mut S, slabs: &[Vec<Piece>], (oy, ch): (u16, u16)) {
        for slab in slabs {
            out.slab();
            for &p in slab {
                match p {
                    Piece::Row(x, y, w, a, b) => {
                        let band = (y - oy) / ch;
                        out.row(i32::from(band), band, (x, y, w), a, b);
                    }
                    Piece::Tall(x, y, w, h, end) => {
                        let band = (y - oy) / ch;
                        out.rect(i32::from(band), band, (x, y, w, h), end);
                    }
                }
            }
        }
        out.mark();
    }
    let key = |q: &GlowQuad| (q.y, q.x, q.w, q.h, q.color, q.alpha, q.color2, q.alpha2);
    let sorted = |mut v: Vec<GlowQuad>| {
        v.sort_by_key(key);
        v
    };
    let mut mix = Mix(0x6E65_6172_6469_6D73);
    let (o, rows) = (8i32, 4i32);
    let (mut gathered_ramps, mut gathered_quads) = (0u64, 0u64);
    for case in 0..300 {
        let (cw, ch) = (6 + mix.below(12) as i32, 12 + mix.below(24) as i32);
        let ledger_with = |dims: &[Dim]| {
            let mut ledger = InkLedger::EMPTY;
            ledger.grid = (o as f32, o as f32, cw as f32, ch as f32);
            ledger.dims.extend_from_slice(dims);
            ledger
        };
        // Ring boxes of cut cells, as the guard lays its dims.
        let mut dims = Vec::new();
        for _ in 0..1 + mix.below(12) {
            let (col, row) = (mix.below(12) as i32, mix.below(rows as u32) as i32);
            dims.push(Dim {
                x0: (o + col * cw - RING) as f32,
                y0: (o + row * ch - RING) as f32,
                x1: (o + (col + 1) * cw + RING) as f32,
                y1: (o + (row + 1) * ch + RING) as f32,
                share: mix.below(128) as f32 / 128.0,
            });
        }
        let end = |mix: &mut Mix| -> (u32, u8) {
            let a = if mix.chance(3) { mix.below(256) } else { 0 };
            let ch = |mix: &mut Mix| mix.below(if a == 0 { 64 } else { a + 1 });
            ((ch(mix) << 16) | (ch(mix) << 8) | ch(mix), a as u8)
        };
        let mut slabs = Vec::new();
        for _ in 0..1 + mix.below(4) {
            let x = (o + mix.below((12 * cw) as u32) as i32) as u16;
            let w = 1 + mix.below((4 * cw) as u32) as u16;
            let y0 = o + mix.below((rows * ch) as u32) as i32;
            let y1 = (y0 + 1 + mix.below((2 * ch) as u32) as i32).min(o + rows * ch);
            let mut slab = Vec::new();
            let mut y = y0;
            while y < y1 {
                if mix.below(12) == 0 {
                    let h = (2 + mix.below(8) as i32).min(y1 - y);
                    slab.push(Piece::Tall(x, y as u16, w, h as u16, end(&mut mix)));
                    y += h;
                } else {
                    slab.push(Piece::Row(x, y as u16, w, end(&mut mix), end(&mut mix)));
                    y += 1;
                }
            }
            slabs.push(slab);
        }
        let grid = (o as u16, ch as u16);
        let mut quads = Vec::new();
        lay(&mut quads, &slabs, grid);
        let mut fresh = ledger_with(&dims);
        fresh.guard_mask_stream(&mut quads, &mut GlowRamps::default(), false);
        gathered_quads += fresh.near_gen;
        let (mut rq, mut ramps) = (Vec::new(), GlowRamps::default());
        {
            let mut sink = RampSink::new(&mut rq, &mut ramps).costed(RampCost::ALWAYS);
            lay(&mut sink, &slabs, grid);
        }
        let mut reused = ledger_with(&dims);
        reused.guard_mask_stream(&mut rq, &mut ramps, false);
        gathered_ramps += reused.near_gen;
        let mut got = Vec::new();
        ramps.expand_into(&rq, &mut got);
        assert_eq!(
            sorted(got),
            sorted(quads),
            "case {case}: the ramps are cut as their quads"
        );
        // One ledger slices box after box, half of them across the box
        // before's columns, as a fresh ledger slices each; the mask over
        // the near dims alone is the mask over every dim.
        let mut ledger = ledger_with(&dims);
        let feather = ledger.feather();
        let mut across = None;
        for _ in 0..24 {
            let (x0, x1) = match across {
                Some(xs) if mix.chance(5) => xs,
                _ => {
                    let x0 = mix.below((14 * cw) as u32) as i32;
                    (x0, x0 + 1 + mix.below((4 * cw) as u32) as i32)
                }
            };
            across = Some((x0, x1));
            let y0 = mix.below(((rows + 1) * ch) as u32) as i32;
            let y1 = y0 + 1 + mix.below(ch as u32) as i32;
            let met = ledger.knots((x0, y0, x1, y1));
            let mut once = ledger_with(&dims);
            assert_eq!(
                met,
                once.knots((x0, y0, x1, y1)),
                "case {case}: a box's dims met"
            );
            if met {
                assert_eq!(
                    (&ledger.knots_x, &ledger.knots_y),
                    (&once.knots_x, &once.knots_y),
                    "case {case}: box ({x0}, {y0})..({x1}, {y1}) sliced afresh"
                );
            }
            let points: Vec<(f32, f32)> = if met {
                let ys: Vec<f32> = ledger
                    .knots_y
                    .windows(2)
                    .map(|k| 0.5 * (k[0] + k[1]) as f32)
                    .collect();
                let xs: Vec<f32> = ledger
                    .knots_x
                    .iter()
                    .map(|&x| x as f32)
                    .chain(
                        ledger
                            .knots_x
                            .windows(2)
                            .map(|k| 0.5 * (k[0] + k[1]) as f32),
                    )
                    .collect();
                ys.iter()
                    .flat_map(|&y| xs.iter().map(move |&x| (x, y)))
                    .collect()
            } else {
                let ym = 0.5 * (y0 + y1) as f32;
                vec![(x0 as f32, ym), (x1 as f32, ym)]
            };
            for (x, y) in points {
                let kept = if met {
                    ledger.mask_at_with(x, y, feather)
                } else {
                    1.0
                };
                let near = std::mem::replace(&mut ledger.near, ledger.dims.clone());
                let every = ledger.mask_at_with(x, y, feather);
                ledger.near = near;
                assert_eq!(
                    kept.to_bits(),
                    every.to_bits(),
                    "case {case}: box ({x0}, {y0})..({x1}, {y1}) at ({x}, {y})"
                );
            }
        }
    }
    assert!(
        gathered_ramps < gathered_quads,
        "premise: the ramps gathered their near dims {gathered_ramps} times, their quads {gathered_quads}"
    );
}

/// §15.2 1.9: the letter guard never weighs nor cuts the light-owned glass
/// ([`InkLedger::note_light_owned_laid`]): the yield cuts it to the words
/// (`glass_yield`). A dark glass under a row of paper's letters, laid as
/// quads and as ramps, keeps every light it was laid with, while a light
/// laid after it on the same letters, not light-owned, is cut as before.
/// Negative control: the glass not noted, the guard cuts it.
#[test]
fn the_letter_guard_never_cuts_light_owned_glass() {
    use aterm_render::{BeamRows, RampCost, RampSink};
    let g = Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 320,
        win_h: 96,
        head: 0,
    };
    let row = 2u16;
    let paper = aterm_core::terminal::RenderCell {
        fg: [0x2B, 0x2D, 0x33],
        bg: [0xF6, 0xF7, 0xF9],
        ..aterm_core::terminal::RenderCell::default()
    };
    let mut cells = vec![vec![paper; g.cols]; g.rows];
    for (col, ch) in [(2, 'e'), (3, 'c'), (4, 'h'), (5, 'o'), (7, 'h'), (8, 'i')] {
        cells[usize::from(row)][col].ch = ch;
    }
    // The glass across the row's letters, a dark source-over light, then
    // a light after it on the same letters.
    fn lay<S: BeamRows>(out: &mut S, row: u16) -> (S::Mark, S::Mark) {
        let from = out.mark();
        out.slab();
        for y in row * 16..row * 16 + 16 {
            out.row(
                i32::from(row),
                row,
                (0, y, 96),
                (0x0030_0838, 0xC8),
                (0x0008_3030, 0xC0),
            );
        }
        let to = out.mark();
        out.slab();
        out.rect(
            i32::from(row),
            row,
            (8, row * 16 + 4, 64, 8),
            (0x0020_2020, 0xB0),
        );
        (from, to)
    }
    let guard = |owned: bool, ramped: bool| {
        let mut glow = CursorGlow::default();
        glow.observe_frame_text(Some((row, 30)), &cells, Instant::now());
        let (mut quads, mut ramps) = (Vec::new(), GlowRamps::default());
        let (from, to) = if ramped {
            let mut sink = RampSink::new(&mut quads, &mut ramps).costed(RampCost::ALWAYS);
            lay(&mut sink, row)
        } else {
            let (from, to) = lay(&mut quads, row);
            ((from, ramps.mark()), (to, ramps.mark()))
        };
        if owned {
            glow.ink_ledger.note_light_owned_laid(from, to);
        }
        let mut laid = Vec::new();
        ramps.expand_into(&quads, &mut laid);
        let glass_n = laid.len() - 1;
        glow.under_out = quads;
        glow.under_ramps = ramps;
        let mut over = Vec::new();
        let cfg = GlowConfig {
            style: GlowStyle::RainbowKitty,
            ..GlowConfig::default()
        };
        let _ = glow.guard_letters(&mut over, (Some((row, 30)), true), (g, &cfg), (&[], 0));
        let mut got = Vec::new();
        glow.under_ramps.expand_into(&glow.under_out, &mut got);
        (laid, got, glass_n)
    };
    for ramped in [false, true] {
        let (laid, got, n) = guard(true, ramped);
        assert!(
            got.len() >= n && got[..n] == laid[..n],
            "ramped {ramped}: the light-owned glass is laid whole: {got:?}"
        );
        assert_ne!(
            got[n..],
            laid[n..],
            "ramped {ramped}: the light after the glass is cut as before"
        );
        let (laid, got, n) = guard(false, ramped);
        assert!(
            got.len() < n || got[..n] != laid[..n],
            "ramped {ramped}, control: the glass not light-owned is cut"
        );
    }
}

/// A PAGE SHADE IS HELD TO THE LETTERS IT REACHES ([`CursorGlow::hold_page_shades`]): a shade
/// at Solarized Dark's own luminance in a red's hue ([`crate::cursor_glow::page_lit_ink`],
/// noted as a page shade), laid in one-pixel columns as Beam's body is, whose top edge crosses
/// a row of solid `l`s at each pixel row of their cells, under Solarized's text (4.75 on its
/// page, kept at the floor). Each column is laid as its quads, and as Beam lays its body,
/// through a [`aterm_render::RampSink`]: one ramp a column, its rows one pixel tall but a run
/// of four (`join`, `joined`) and running past the letters' ring band. The guard leaves the
/// shade whole away from the letters' ring boxes, and over them cuts it until every letter
/// keeps its bar by the guard's own reading of its pixels: the guard read again over what it
/// laid cuts nothing more. Negative control ([`CursorGlow::set_letter_guard_shades`]): kept
/// whole, the shade over the letters is laid as it came, and on some edge row it lowers a
/// letter under its bar, as a guard holding it reads, laid either way.
#[test]
fn a_page_shade_is_held_to_the_letters_it_reaches() {
    use aterm_render::glyph_ink::GlyphCoverage;
    use aterm_render::{BeamRows, RampCost, RampSink};
    let g = Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 320,
        win_h: 96,
        head: 0,
    };
    let row = 2u16;
    const PAGE: u32 = 0x0000_2B36;
    let page = aterm_core::terminal::RenderCell {
        fg: [0x83, 0x94, 0x96],
        bg: [0x00, 0x2B, 0x36],
        ..aterm_core::terminal::RenderCell::default()
    };
    let mut cells = vec![vec![page; g.cols]; g.rows];
    let letters = 2..6;
    for col in letters.clone() {
        cells[usize::from(row)][col].ch = 'l';
    }
    // The face's `l`: a solid stem two pixels wide, rows 2 to 13 of its cell.
    let at = usize::from(aterm_render::GlyphInk::index('l', false));
    let mut regular: [GlyphCoverage; 95] = std::array::from_fn(|_| GlyphCoverage::default());
    let bold: [GlyphCoverage; 95] = std::array::from_fn(|_| GlyphCoverage::default());
    regular[at].max = 255;
    regular[at].cell_box = Some([3, 2, 5, 14]);
    let mut rasters = vec![None; 190];
    rasters[at] = Some(aterm_render::GlyphRaster {
        x: 3,
        y: 2,
        w: 2,
        cov: vec![255; 2 * 12].into_boxed_slice(),
    });
    let face = std::sync::Arc::new(
        aterm_render::GlyphInk::from_coverage(regular, bold, false).with_rasters(rasters),
    );
    let shade = crate::cursor_glow::page_lit_ink(0x00DC_322F, PAGE);
    // The ramps' rows run this far, past the letters' ring band.
    let foot = (row + 1) * 16 + RING as u16 + 4;
    // The shade from window row `top`, columns 0 to 11: as quads, down to the band's foot;
    // `ramped`, as ramps down to `foot`, each column's two rows, its run of four, its rest.
    let laid = |top: u16, ramped: bool| -> (Vec<GlowQuad>, GlowRamps) {
        let (mut quads, mut ramps) = (Vec::new(), GlowRamps::default());
        if !ramped {
            quads = (0..12 * 8)
                .map(|x| GlowQuad {
                    row,
                    x,
                    y: top,
                    w: 1,
                    h: (row + 1) * 16 - top,
                    color: shade,
                    alpha: 0xFF,
                    color2: shade,
                    alpha2: 0xFF,
                })
                .collect();
            return (quads, ramps);
        }
        let mut sink = RampSink::new(&mut quads, &mut ramps).costed(RampCost::ALWAYS);
        let end = (shade, 0xFF);
        for x in 0..12 * 8 {
            sink.slab();
            let mut y = top;
            while y < foot {
                if y == top + 2 {
                    sink.rect(0, row, (x, y, 1, 4), end);
                    y += 4;
                } else {
                    sink.row(0, row, (x, y, 1), end, end);
                    y += 1;
                }
            }
        }
        drop(sink);
        (quads, ramps)
    };
    // The stream the guard leaves of `(quads, ramps)`, expanded into its lights in draw order.
    let guard = |(quads, ramps): &(Vec<GlowQuad>, GlowRamps), held: bool| -> Vec<GlowQuad> {
        let mut glow = CursorGlow::default();
        glow.observe_glyph_ink(Some(face.clone()));
        glow.observe_frame_text(Some((row, 30)), &cells, Instant::now());
        glow.set_letter_guard_shades(held);
        glow.ink_ledger.note_page_shade_laid(
            (0, GlowRamps::default().mark()),
            (quads.len(), ramps.mark()),
        );
        glow.under_out.clone_from(quads);
        glow.under_ramps = ramps.clone();
        let cfg = GlowConfig {
            style: GlowStyle::Beam,
            ..GlowConfig::default()
        };
        let _ = glow.guard_letters(
            &mut Vec::new(),
            (Some((row, 30)), true),
            (g, &cfg),
            (&[], 0),
        );
        let mut lights = Vec::new();
        glow.under_ramps.expand_into(&glow.under_out, &mut lights);
        lights
    };
    let expand = |(quads, ramps): &(Vec<GlowQuad>, GlowRamps)| {
        let mut lights = Vec::new();
        ramps.expand_into(quads, &mut lights);
        lights
    };
    // Whether a light meets a letter's ring box.
    let over_letters = |q: &GlowQuad| {
        let (x, y) = (i32::from(q.x), i32::from(q.y));
        let band = (i32::from(row) * 16 - RING, (i32::from(row) + 1) * 16 + RING);
        x + i32::from(q.w) > letters.start as i32 * 8 - RING
            && x < letters.end as i32 * 8 + RING
            && y + i32::from(q.h) > band.0
            && y < band.1
    };
    for ramped in [false, true] {
        let mut lowered = 0;
        for top in row * 16..(row + 1) * 16 {
            let stream = laid(top, ramped);
            if ramped {
                assert!(
                    stream.0.is_empty()
                        && stream.1.items.len() == 12 * 8
                        && stream.1.items.iter().all(|r| r.joined == 3 && r.join == 2),
                    "fixture: each column is one ramp with a run of four"
                );
            } else {
                assert!(stream.1.is_empty(), "fixture: the shade is laid as quads");
            }
            let shade = expand(&stream);
            let held = guard(&stream, true);
            assert_eq!(
                held.len(),
                shade.len(),
                "ramped {ramped}, edge at {top}: the shade's lights stay in place"
            );
            for (q, h) in shade.iter().zip(&held) {
                if !over_letters(q) {
                    assert_eq!(
                        q, h,
                        "ramped {ramped}, edge at {top}: the shade off the letters' ring boxes is \
                         whole"
                    );
                }
            }
            let again = (held.clone(), GlowRamps::default());
            assert_eq!(
                guard(&again, true),
                held,
                "ramped {ramped}, edge at {top}: a letter under the shade as the guard laid it \
                 reads under its bar"
            );
            assert_eq!(
                guard(&stream, false),
                shade,
                "control, ramped {ramped}, edge at {top}: the shade is kept whole"
            );
            if held != shade {
                lowered += 1;
            }
        }
        assert!(
            lowered > 0,
            "control, ramped {ramped}: kept whole, the shade lowers no letter under its bar on \
             any edge row"
        );
    }
}

/// A LETTER UNDER ITS RASTER BAR KEEPS A SHADE THAT DOES NOT LOWER IT
/// ([`CursorGlow::hold_page_shades`], [`shade_bar`]): solid `l`s in `#7f9092` on Solarized
/// Dark's page read 4.5086 there, at AA as the judges read it and under AA's level in hand
/// ([`raster_bar`], 4.563), as Beam's `w`, `j` and `g` read on Linux's face; three `l`s in
/// `#eee8d5` stand between two of them, far over the floor. A page shade at the page's
/// luminance in a red's hue ([`crate::cursor_glow::page_lit_ink`]) laid wholly over their ring
/// boxes is left whole: every ring reads the shade alone, no brighter than the page. Where the
/// shade's top edge crosses their cells, a ring lying on it and on the page reads a ground
/// brighter than either, and on some edge row the guard cuts the shade over the dim `l`s; it
/// cuts nothing off their ring boxes, the bright `l`s between them included (eased along the
/// row as the guard's other cuts are, the hold cut Beam's body from under the letters between
/// two it held), and a guard reading again over what it laid cuts nothing more. Negative
/// control ([`CursorGlow::set_letter_guard_shades_to_own`]): held to their raster bar, the
/// shade wholly over them is cut. A verdict weighed at the raster bar is not taken for one at
/// the letter's own reading: the frame after one held at the bar leaves the shade whole.
#[test]
fn a_letter_under_its_raster_bar_keeps_a_shade_that_does_not_lower_it() {
    use aterm_render::glyph_ink::GlyphCoverage;
    let g = Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 320,
        win_h: 96,
        head: 0,
    };
    let row = 2u16;
    const PAGE: u32 = 0x0000_2B36;
    const INK: u32 = 0x007F_9092;
    let page = aterm_core::terminal::RenderCell {
        fg: [0x7F, 0x90, 0x92],
        bg: [0x00, 0x2B, 0x36],
        ..aterm_core::terminal::RenderCell::default()
    };
    let mut cells = vec![vec![page; g.cols]; g.rows];
    // The dim `l`s at columns 2 and 6, the bright ones between.
    let dim = [2usize, 6];
    for (col, cell) in cells[usize::from(row)]
        .iter_mut()
        .enumerate()
        .take(7)
        .skip(2)
    {
        cell.ch = 'l';
        if !dim.contains(&col) {
            cell.fg = [0xEE, 0xE8, 0xD5];
        }
    }
    let table = crate::color_math::relative_luminance_table();
    let lum = |c: u32| crate::color_math::relative_luminance_with(table, c);
    let own = (lum(INK) + 0.05) / (lum(PAGE) + 0.05);
    assert!(
        (AA_READ..raster_bar(own)).contains(&own),
        "fixture: the dim `l` reads {own}, at AA and under its raster bar {}",
        raster_bar(own)
    );
    // The face's `l`: a solid stem two pixels wide, rows 2 to 13 of its cell.
    let at = usize::from(aterm_render::GlyphInk::index('l', false));
    let mut regular: [GlyphCoverage; 95] = std::array::from_fn(|_| GlyphCoverage::default());
    let bold: [GlyphCoverage; 95] = std::array::from_fn(|_| GlyphCoverage::default());
    regular[at].max = 255;
    regular[at].cell_box = Some([3, 2, 5, 14]);
    let mut rasters = vec![None; 190];
    rasters[at] = Some(aterm_render::GlyphRaster {
        x: 3,
        y: 2,
        w: 2,
        cov: vec![255; 2 * 12].into_boxed_slice(),
    });
    let face = std::sync::Arc::new(
        aterm_render::GlyphInk::from_coverage(regular, bold, false).with_rasters(rasters),
    );
    let shade = crate::cursor_glow::page_lit_ink(0x00DC_322F, PAGE);
    assert!(
        lum(shade) <= lum(PAGE) && shade & 0x00FF_FFFF != PAGE,
        "fixture: the shade {shade:06x} lies off the page's hue, at or under its luminance"
    );
    // The shade in one-pixel columns 0 to 11, from window row `top` to the band's foot.
    let band = (i32::from(row) * 16 - RING, (i32::from(row) + 1) * 16 + RING);
    let laid = |top: u16| -> Vec<GlowQuad> {
        (0..12 * 8)
            .map(|x| GlowQuad {
                row,
                x,
                y: top,
                w: 1,
                h: band.1 as u16 - top,
                color: shade,
                alpha: 0xFF,
                color2: shade,
                alpha2: 0xFF,
            })
            .collect()
    };
    // The shade `quads` as one guard leaves it on the last of its frames, each frame's letters
    // held to their own reading or to their raster bar as `to_own` says in turn.
    let guard = |quads: &[GlowQuad], to_own: &[bool]| -> Vec<GlowQuad> {
        let mut glow = CursorGlow::default();
        glow.observe_glyph_ink(Some(face.clone()));
        let cfg = GlowConfig {
            style: GlowStyle::Beam,
            ..GlowConfig::default()
        };
        for &to_own in to_own {
            glow.observe_frame_text(Some((row, 30)), &cells, Instant::now());
            glow.set_letter_guard_shades_to_own(to_own);
            glow.ink_ledger.clear_rails();
            let ramps = GlowRamps::default();
            glow.ink_ledger
                .note_page_shade_laid((0, ramps.mark()), (quads.len(), ramps.mark()));
            glow.under_out = quads.to_vec();
            glow.under_ramps = ramps;
            let _ = glow.guard_letters(
                &mut Vec::new(),
                (Some((row, 30)), true),
                (g, &cfg),
                (&[], 0),
            );
        }
        glow.under_out
    };
    let over = laid(band.0 as u16);
    assert_eq!(
        guard(&over, &[true]),
        over,
        "the shade wholly over the letters' ring boxes is cut"
    );
    assert_ne!(
        guard(&over, &[false]),
        over,
        "control: held to their raster bar, the letters keep the shade wholly over them whole"
    );
    assert_eq!(
        guard(&over, &[false, true]),
        over,
        "the frame after one held at the raster bar takes that frame's verdicts"
    );
    // Whether a light meets a dim `l`'s ring box.
    let on_dim = |q: &GlowQuad| {
        let x = i32::from(q.x);
        dim.iter().any(|&col| {
            x + i32::from(q.w) > col as i32 * 8 - RING && x < (col as i32 + 1) * 8 + RING
        })
    };
    let mut lowered = 0;
    for top in row * 16..(row + 1) * 16 {
        let shade = laid(top);
        let held = guard(&shade, &[true]);
        assert_eq!(
            held.len(),
            shade.len(),
            "edge at {top}: the shade's lights stay in place"
        );
        for (q, h) in shade.iter().zip(&held) {
            if !on_dim(q) {
                assert_eq!(
                    q, h,
                    "edge at {top}: the shade off the dim letters' ring boxes is cut"
                );
            }
        }
        assert_eq!(
            guard(&held, &[true]),
            held,
            "edge at {top}: a letter under the shade as the guard laid it reads under its own \
             reading"
        );
        lowered += usize::from(held != shade);
    }
    assert!(
        lowered > 0,
        "no edge row's shade lowers a letter under its own reading"
    );
}

/// ON THE GLASS THE THEME'S LETTERS KEEP ITS BAR BY EACH SOLID PIXEL ([`LetterGuard::census_bar`],
/// §15.2 1.12's glyph minimum): Default Light's text (`#2b2d33` on `#f6f7f9`), a row of `l`s whose
/// stem is two whole columns and an edge column at 253 of 255 (solid ink to the judges), under an
/// additive light over the edge column and the ground right of it, as the caret's aurora lies
/// over the letter it has just printed. The light lifts the edge pixel under the glass's 5.25
/// against its lit ring while each pixel row's lower median, a whole pixel the light misses,
/// keeps the whole ink's reading. On a frame that draws the glass the guard cuts the light over
/// the letters, and a guard reading again over what it laid cuts nothing more; a light that
/// leaves the edge pixel over the bar is kept whole there. Negative controls: on main's paper,
/// and on a sheer page that draws no glass, where the letters keep their raster bar alone, the
/// whole light is kept.
#[test]
fn on_the_glass_the_theme_s_letters_keep_its_bar_by_each_solid_pixel() {
    use aterm_render::glyph_ink::GlyphCoverage;
    fn ratio(a: u32, b: u32) -> f32 {
        let (x, y) = (
            crate::color_math::relative_luminance(a),
            crate::color_math::relative_luminance(b),
        );
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }
    const FG: u32 = 0x002B_2D33;
    const PAGE: u32 = 0x00F6_F7F9;
    const EDGE: u8 = 253;
    let g = Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 320,
        win_h: 96,
        head: 0,
    };
    let row = 2u16;
    let paper = aterm_core::terminal::RenderCell {
        fg: [0x2B, 0x2D, 0x33],
        bg: [0xF6, 0xF7, 0xF9],
        ..aterm_core::terminal::RenderCell::default()
    };
    let mut cells = vec![vec![paper; g.cols]; g.rows];
    let letters = 2..6;
    for col in letters.clone() {
        cells[usize::from(row)][col].ch = 'l';
    }
    // The face's `l`: columns 3 and 4 whole and column 5 at `EDGE`, rows 2 to 13 of its cell.
    let at = usize::from(aterm_render::GlyphInk::index('l', false));
    let mut regular: [GlyphCoverage; 95] = std::array::from_fn(|_| GlyphCoverage::default());
    let bold: [GlyphCoverage; 95] = std::array::from_fn(|_| GlyphCoverage::default());
    regular[at].max = 255;
    regular[at].cell_box = Some([3, 2, 6, 14]);
    let mut rasters = vec![None; 190];
    rasters[at] = Some(aterm_render::GlyphRaster {
        x: 3,
        y: 2,
        w: 3,
        cov: [255, 255, EDGE].repeat(12).into_boxed_slice(),
    });
    let face = std::sync::Arc::new(
        aterm_render::GlyphInk::from_coverage(regular, bold, false).with_rasters(rasters),
    );
    // An additive light of `lift` on each channel over each letter's edge column and the two
    // ground columns right of it, down its stem.
    let light = |lift: u32| -> Vec<GlowQuad> {
        letters
            .clone()
            .map(|col| GlowQuad {
                row,
                x: col as u16 * 8 + 5,
                y: row * 16 + 2,
                w: 3,
                h: 12,
                color: lift * 0x0001_0101,
                alpha: 0,
                color2: lift * 0x0001_0101,
                alpha2: 0,
            })
            .collect()
    };
    let guard = |over: &[GlowQuad], light: Light| -> Vec<GlowQuad> {
        let mut glow = CursorGlow::default();
        glow.observe_glyph_ink(Some(face.clone()));
        glow.observe_frame_text(Some((row, 30)), &cells, Instant::now());
        let cfg = GlowConfig {
            style: GlowStyle::RainbowKitty,
            dark_theme: false,
            theme_fg: FG,
            theme_bg: PAGE,
            light,
            ..GlowConfig::default()
        };
        let mut over = over.to_vec();
        let _ = glow.guard_letters(&mut over, (Some((row, 30)), true), (g, &cfg), (&[], 0));
        over
    };
    // What the edge pixel reads under a light of `lift` against its ring, which the light lifts
    // to white, and what the whole ink reads against the page beside it.
    let edge = aterm_render::blend_text(PAGE, FG, PAGE, EDGE, false);
    let lit = |lift: u32| {
        ratio(
            aterm_render::add_sat(edge, lift * 0x0001_0101),
            aterm_render::add_sat(PAGE, lift * 0x0001_0101),
        )
    };
    let whole = ratio(FG, PAGE);
    assert!(
        ratio(edge, PAGE) >= SOLID_SHARE * whole && whole >= LETTER_FLOOR,
        "fixture: the edge column is solid ink ({} of {whole})",
        ratio(edge, PAGE)
    );
    let bar = crate::rainbow_kitty::prism_glass::GLASS_BAR as f32;
    let (strong, faint) = (0x3C, 0x14);
    assert!(
        lit(strong) < bar && lit(faint) >= bar,
        "fixture: the strong light lifts the edge under the glass's bar ({}), the faint one not \
         ({})",
        lit(strong),
        lit(faint)
    );
    let glass = Light::at(Live::Glass).on_page(true);
    assert!(glass.glass(), "fixture: the frame draws the glass");
    let laid = light(strong);
    let held = guard(&laid, glass);
    assert_ne!(
        held, laid,
        "on the glass the strong light over the edge is cut"
    );
    assert_eq!(
        guard(&held, glass),
        held,
        "a letter under the light as the guard laid it reads under the glass's bar"
    );
    let faint = light(faint);
    assert_eq!(
        guard(&faint, glass),
        faint,
        "a light that keeps every solid pixel over the bar is kept whole on the glass"
    );
    for (page, what) in [
        (Light::OFF, "main's paper"),
        (Light::at(Live::Glass).on_page(false), "a sheer page"),
    ] {
        assert!(!page.glass(), "fixture: {what} draws no glass");
        assert_eq!(
            guard(&laid, page),
            laid,
            "control, {what}: held to its raster bar alone, the letter keeps the whole light"
        );
    }
}
