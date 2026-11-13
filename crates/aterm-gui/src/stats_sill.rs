// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Compact stats sill rendered in the window's bottom padding without re-gridding.

use aterm_vitals::ink::Rgb;
use aterm_vitals::model::{CpuView, MemView, Resource, Snapshot, State};
use aterm_vitals::trend::TrendView;
use aterm_vitals::words;

use crate::stats_deck::{DeckGeom, DeckInput};
use crate::stats_paint::{Bar, Hit, Instrument, Mark, Painter, a, panel};
use crate::widget::{DrawPrim, rgba};

/// Minimum bottom padding in points required to show the sill.
pub(crate) const SILL_MIN_PT: f64 = 8.0;

/// The sill's height in device px: the window's bottom pad when it holds the sill, else 0 (hidden).
pub(crate) fn sill_px(pad: usize, scale: f64) -> usize {
    if !scale.is_finite() || scale <= 0.0 {
        return 0;
    }
    let min = (SILL_MIN_PT * scale).round() as usize;
    if pad >= min { pad } else { 0 }
}

/// The sill card's height in device px: the `sill` pad less the bottom the
/// drop-target border keeps ([`aterm_render::drop_overlay_border_px`] of the
/// `frame_w` × `frame_h` frame, 2–6 px), so nothing the sill draws or answers a
/// pointer in lies under that border. At 2× a 24 px pad keeps 18 px, the three
/// lanes' whole height; at 1× a 12 px pad over a 6 px border keeps 6, so the
/// Tab lane yields there ([`sill_paint`]).
pub(crate) fn sill_card_px(sill: usize, frame_w: usize, frame_h: usize) -> usize {
    sill.saturating_sub(aterm_render::drop_overlay_border_px(frame_w, frame_h, 0))
}

/// The gap between the sill's columns, in points.
const COL_GAP_PT: f32 = 12.0;

/// What the width law leaves at a sill `w_pt` points wide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Fold {
    /// The disk column draws its lanes (else only its ◢ when filling).
    pub(crate) disk: bool,
    /// The memory column draws its lanes (else only its pressure cap).
    pub(crate) memory: bool,
    /// One square a core (else one tile per cluster at its mean).
    pub(crate) per_core: bool,
    /// Under 240 pt: the well, the cluster tiles and the CPU column only.
    pub(crate) minimal: bool,
}

impl Fold {
    /// The law for a sill `w_pt` points wide.
    pub(crate) fn of(w_pt: f32) -> Self {
        Self {
            disk: w_pt >= 900.0,
            memory: w_pt >= 600.0,
            per_core: w_pt >= 400.0,
            minimal: w_pt < 240.0,
        }
    }
}

fn lane_rows(s: f32) -> [(f32, f32); 3] {
    let h = (2.0 * s).round().max(2.0);
    let gap = s.round().max(1.0);
    let top = s.round().max(1.0);
    [(top, h), (top + h + gap, h), (top + 2.0 * (h + gap), h)]
}

/// Paints the sill primitives and instrument hit regions.
pub(crate) fn sill_paint(g: &DeckGeom, input: &DeckInput<'_>) -> (Vec<DrawPrim>, Vec<Hit>) {
    let (mut prims, hits) = sill_instruments(g, input);
    if let Some(alpha) = input.plate {
        let s = g.scale.max(0.5);
        let r = (2.0 * s).round();
        let plates: Vec<DrawPrim> = hits
            .iter()
            .map(|h| {
                let (x, y, w, hh) = h.rect;
                panel(
                    (x - s).round(),
                    y.max(0.0).round(),
                    (w + 2.0 * s).round(),
                    hh.round(),
                    r,
                    rgba(input.inks.ground, alpha),
                )
            })
            .collect();
        prims.splice(0..0, plates);
    }
    (prims, hits)
}

fn sill_instruments(g: &DeckGeom, input: &DeckInput<'_>) -> (Vec<DrawPrim>, Vec<Hit>) {
    let inks = input.inks;
    let s = g.scale.max(0.5);
    let mut p = Painter::new(&inks, s, g.font_px, input.dwc);
    let mut hits = Vec::new();
    let (w, h) = (g.w.round(), g.h.round());
    let left = g.pad.round();
    let right = (w - left).max(left);
    let Some(snap) = input.snap else {
        return (p.prims, hits);
    };
    let fold = Fold::of((right - left) / s);
    let rows = lane_rows(s);
    let lane_bottom = rows[2].0 + rows[2].1;
    // The card ends where the drop-target border begins (`sill_card_px`), so
    // a lane is drawn only where it fits whole: the lanes yield bottom-up,
    // the Tab lane first (at 1× a 12 px pad over a 6 px border holds Global
    // and aterm). The lens and the sentences still say every figure.
    let fits = |i: usize| rows[i].0 + rows[i].1 <= h;
    let show_aterm = fits(1);
    let show_tab = input.tab_count >= 2 && fits(2);
    let shown = [fits(0), show_aterm, show_tab];
    let band_y = 0.0f32;
    let band_h = (lane_bottom + s.round()).min(h);

    let well_w = p.px(56.0);
    let mut x = left;
    if !input.marks.is_empty() {
        // The marks span the lanes the card holds; a count is drawn only
        // where its figures fit the band whole.
        let r = ((lane_bottom.min(band_h) - rows[0].0) / 2.0)
            .floor()
            .max(1.0);
        let cy = (rows[0].0 + r).round();
        let counted = p.small_px.get() * crate::tray_raster::chrome_cap_ratio() <= band_h - band_y;
        let mut mx = x;
        let shown = input.marks.len().min(3);
        for m in input.marks.iter().take(shown) {
            p.mark(m.kind, mx + r, cy, r, m.frac);
            mx += 2.0 * r + p.px(1.0);
            if counted {
                let c = m.count.to_string();
                let cw = p.figure_w(&c, true);
                let base = crate::tray_raster::row_baseline(band_y, band_h, p.small_px.get());
                p.figure(mx, base, c, inks.words2, true);
                mx += cw;
            }
            mx += p.px(4.0);
        }
        if counted && input.marks.len() > 3 {
            let base = crate::tray_raster::row_baseline(band_y, band_h, p.small_px.get());
            p.figure(
                mx,
                base,
                format!("+{}", input.marks.len() - 3),
                inks.words2,
                true,
            );
        }
        hits.push(Hit::row(
            (x, band_y, well_w, band_h),
            Instrument::Processes,
            Bar::Aterm,
        ));
    }
    x += well_w + p.px(8.0);

    if let Some(cpu) = snap.cpu.as_ref() {
        x = core_row(&mut p, cpu, snap, x, band_h, fold, &mut hits);
        x += p.px(12.0);
    }

    let total_cores = snap.total_cores().map_or(1.0, |n| n as f32).max(1.0);
    let room = (right - x).max(0.0);
    let gap = p.px(COL_GAP_PT);
    let bracket_room = p.px(2.0);
    let (cpu_w, mem_w, disk_w) = if fold.minimal {
        (room.max(0.0).min(room), 0.0, 0.0)
    } else {
        let cols = [true, fold.memory, fold.disk];
        let weights = [55.0f32, 25.0, 20.0];
        let gaps = cols.iter().filter(|c| **c).count().saturating_sub(1) as f32 * gap;
        let total: f32 = weights
            .iter()
            .zip(cols.iter())
            .filter(|(_, c)| **c)
            .map(|(w, _)| *w)
            .sum();
        let share = |i: usize| {
            if cols[i] {
                ((room - gaps - bracket_room) * weights[i] / total).floor()
            } else {
                0.0
            }
        };
        (share(0), share(1), share(2))
    };
    if (fold.minimal && cpu_w < p.px(80.0)) || !shown[0] {
        return (p.prims, hits);
    }

    let pinned = snap
        .cpu
        .as_ref()
        .is_some_and(|c| c.busy.level >= total_cores * 0.98);
    let tab = snap.tab_of(&input.tab_panes);
    let busy_level = snap.cpu.as_ref().map_or(0.0, |c| c.busy.level);
    let lanes: [(Bar, Option<&TrendView>, Rgb); 3] = [
        (
            Bar::Global,
            Some(&snap.global.cores),
            inks.load_at(busy_level / total_cores),
        ),
        (
            Bar::Aterm,
            snap.aterm.as_ref().map(|a| &a.cores),
            inks.aterm,
        ),
        (Bar::Tab, tab.as_ref().map(|t| &t.scope.cores), inks.tab),
    ];
    for (i, (bar, trend, ink)) in lanes.iter().enumerate() {
        if !shown[i] {
            continue;
        }
        let (ly, lh) = rows[i];
        let slot = (x - 1.0, ly - 1.0, cpu_w + 2.0, lh + 2.0);
        match trend {
            Some(t) => {
                let m = Mark::of(t, total_cores, words::CORES_GATE);
                p.lane(x, ly, cpu_w, lh, m, *ink, slot, true);
            }
            None => groove(&mut p, x, ly, cpu_w, lh),
        }
        hits.push(Hit::row(
            (x, ly - rows[0].0.min(1.0), cpu_w + bracket_room, lh + 2.0),
            Instrument::Cpu,
            *bar,
        ));
    }
    let bh = (5.0 * s).round().min(band_h - rows[0].0);
    p.bracket(x + cpu_w + 1.0, rows[0].0, bh, pinned);
    x += cpu_w + bracket_room + gap;

    if let Some(m) = snap.mem.as_ref() {
        if fold.memory && mem_w > 0.0 {
            memory_lanes(
                &mut p,
                x,
                &rows,
                mem_w,
                m,
                snap,
                tab.as_ref(),
                shown,
                &mut hits,
            );
            x += mem_w + gap;
        } else if !fold.minimal {
            // Folded: only the pressure cap, and only at warn or above.
            if let Some(c) = pressure_ink(&inks, m) {
                let cw = (2.0 * s).round();
                p.prims
                    .push(panel(x, rows[0].0, cw, rows[0].1, 0.0, rgba(c, 0xFF)));
                hits.push(Hit::row(
                    (x, 0.0, cw + 2.0, band_h),
                    Instrument::Memory,
                    Bar::Global,
                ));
                x += cw + gap;
            }
        }
    }

    if let Some(v) = snap.volume.as_ref() {
        let wedge = disk_wedge(snap, &inks);
        if fold.disk && disk_w > 0.0 {
            let (ly, lh) = rows[0];
            let used = v.used();
            let level = if v.total > 0 {
                used as f32 / v.total as f32
            } else {
                0.0
            };
            let slot = (x - 1.0, ly - 1.0, disk_w + 2.0, lh + 2.0);
            p.lane(
                x,
                ly,
                disk_w,
                lh,
                Mark::still(level),
                inks.mem[0],
                slot,
                true,
            );
            if let Some(ink) = wedge {
                filling_mark(&mut p, x + disk_w + 1.0, ly, lh, ink);
            }
            hits.push(Hit::row(
                (x, 0.0, disk_w + p.px(4.0), lh + ly + 1.0),
                Instrument::Disk,
                Bar::Global,
            ));
            let view = snap.resource(Resource::Disk, &input.tab_panes);
            let tab_rate = (!input.tab_panes.is_empty())
                .then_some(view.tab.now)
                .flatten();
            let scopes = [
                (Bar::Aterm, view.aterm.now, inks.aterm, rows[1], shown[1]),
                (Bar::Tab, tab_rate, inks.tab, rows[2], shown[2]),
            ];
            for (bar, rate, ink, (sy, sh), drawn) in scopes {
                if !drawn {
                    continue;
                }
                write_lane(&mut p, x, sy, disk_w, sh, rate, ink);
                hits.push(Hit::row(
                    (x, sy - 1.0, disk_w, sh + 2.0),
                    Instrument::Disk,
                    bar,
                ));
            }
        } else if let Some(ink) = wedge.filter(|_| !fold.minimal) {
            filling_mark(&mut p, x, rows[0].0, rows[0].1, ink);
            hits.push(Hit::row(
                (x, 0.0, p.px(6.0), band_h),
                Instrument::Disk,
                Bar::Global,
            ));
        }
    }
    (p.prims, hits)
}

/// The ◢ the disk column ends in, and its ink, while the volume wants a look:
/// the Disk row's state (the watch's level, sessions stuck on a full disk, a
/// stalled watch, another volume low, filling within the hour), the judgement
/// the Deck's Disk well marks, or time to full under a day (`facts.filling`).
/// `None` on a calm volume, however full: a level alone is the lane's length.
fn disk_wedge(snap: &Snapshot, inks: &aterm_vitals::ink::VitalsInks) -> Option<Rgb> {
    match snap.resources[Resource::Disk.index()].state {
        Some(State::Critical) => Some(inks.critical),
        Some(_) => Some(inks.warn),
        None => snap.facts.filling.map(|_| inks.warn),
    }
}

/// Where the disk column's scope scale starts: 64 KiB/s, under which a
/// scope's lane is its groove alone.
const WRITE_FLOOR: f64 = 64.0 * 1024.0;
/// Where it ends: 1 GiB/s fills the lane.
const WRITE_CEIL: f64 = 1024.0 * 1024.0 * 1024.0;

/// A write rate's length on the disk column's scope scale: log2 from
/// [`WRITE_FLOOR`] to [`WRITE_CEIL`], in quarter octaves, so a steady rate
/// keeps its pixel; `None` at or under the floor and for no reading.
fn write_level(bytes_per_s: f64) -> Option<f32> {
    if bytes_per_s.partial_cmp(&WRITE_FLOOR) != Some(std::cmp::Ordering::Greater) {
        return None;
    }
    let octaves = ((bytes_per_s / WRITE_FLOOR).log2() * 4.0).round() / 4.0;
    let span = (WRITE_CEIL / WRITE_FLOOR).log2();
    Some((octaves / span).clamp(0.0, 1.0) as f32)
}

/// A scope's disk lane: its write rate, striped because it is a rate and not
/// an amount, so it never nests under the volume's lane; the groove alone at
/// or under 64 KiB/s.
fn write_lane(p: &mut Painter<'_>, x: f32, y: f32, w: f32, h: f32, rate: Option<f64>, ink: Rgb) {
    groove(p, x, y, w, h);
    let Some(level) = rate.and_then(write_level) else {
        return;
    };
    let len = (level * w).round().max(p.s.round().max(1.0));
    p.dotted(x, y, x + len, y, h, rgba(ink, 0xFF));
}

fn groove(p: &mut Painter<'_>, x: f32, y: f32, w: f32, h: f32) {
    let gh = (h / 2.0).round().max(1.0);
    let gy = y + ((h - gh) / 2.0).round();
    let track = p.inks.track;
    p.prims
        .push(panel(x, gy, w, gh, (gh / 2.0).floor(), a(track, 0.7)));
}

fn filling_mark(p: &mut Painter<'_>, x: f32, y: f32, h: f32, ink: Rgb) {
    let side = (h + 2.0).round();
    let c = rgba(ink, 0xFF);
    for i in 0..side as usize {
        let k = i as f32 + 1.0;
        p.prims
            .push(panel(x + side - k, y - 1.0 + k - 1.0, k, 1.0, 0.0, c));
    }
}

const fn pressure_ink(inks: &aterm_vitals::ink::VitalsInks, m: &MemView) -> Option<Rgb> {
    match m.sample.pressure {
        aterm_vitals::Pressure::Warn => Some(inks.warn),
        aterm_vitals::Pressure::Critical => Some(inks.critical),
        _ => None,
    }
}

/// The memory column's lanes; `shown` is which lanes the card holds (Global, aterm, Tab).
#[allow(clippy::too_many_arguments)]
fn memory_lanes(
    p: &mut Painter<'_>,
    x: f32,
    rows: &[(f32, f32); 3],
    w: f32,
    m: &MemView,
    snap: &Snapshot,
    tab: Option<&aterm_vitals::model::TabView>,
    shown: [bool; 3],
    hits: &mut Vec<Hit>,
) {
    let inks = *p.inks;
    let total = m.sample.total.max(1) as f32;
    let (ly, lh) = rows[0];
    groove(p, x, ly, w, lh);
    let r = (lh / 2.0).floor();
    let parts = [
        (
            m.sample.app.saturating_add(m.sample.wired) as f32,
            inks.mem[0],
            1.0,
        ),
        (m.sample.compressed.unwrap_or(0) as f32, inks.mem[1], 1.0),
        (m.sample.cached as f32, inks.mem[2], 0.75),
    ];
    let mut mx = x;
    p.prims.push(DrawPrim::ClipPush { x, y: ly, w, h: lh });
    for (k, (bytes, c, alpha)) in parts.iter().enumerate() {
        let pw = (bytes / total * w).round().min(x + w - mx);
        if pw >= 1.0 {
            let (px0, pw0) = if k == 0 { (mx - r, pw + r) } else { (mx, pw) };
            p.prims.push(panel(
                px0,
                ly,
                pw0,
                lh,
                if k == 0 { r } else { 0.0 },
                a(*c, *alpha),
            ));
            mx += pw;
        }
    }
    p.prims.push(DrawPrim::ClipPop);
    let used_end = x + (m.sample.used() as f32 / total * w).round();
    if let Some(c) = pressure_ink(&inks, m) {
        let cw = (2.0 * p.s).round();
        p.prims.push(panel(
            (used_end - cw).max(x),
            ly - 1.0,
            cw,
            lh + 2.0,
            0.0,
            rgba(c, 0xFF),
        ));
    }
    if m.sample.swap_used > 0 {
        // Swap: a dashed capsule past the column's end, 1 pt after it and
        // ending 1 pt before the next column, so it stays in the gap.
        let sw = (m.sample.swap_used as f32 / total * w)
            .round()
            .clamp(p.px(2.0), p.px(COL_GAP_PT - 2.0));
        let sx = x + w + p.px(1.0);
        p.dotted(
            sx,
            ly + (lh / 2.0).floor(),
            sx + sw,
            ly + (lh / 2.0).floor(),
            lh / 2.0,
            a(inks.warn, 0.9),
        );
    }
    hits.push(Hit::row(
        (x, 0.0, w, lh + ly + 1.0),
        Instrument::Memory,
        Bar::Global,
    ));
    let cap = aterm_vitals::model::gib(m.sample.total).max(f32::EPSILON);
    let scopes = [
        (Bar::Aterm, snap.aterm.as_ref(), inks.aterm, rows[1]),
        (Bar::Tab, tab.map(|t| &t.scope), inks.tab, rows[2]),
    ];
    for ((bar, scope, ink, (sy, sh)), &drawn) in scopes.into_iter().zip(&shown[1..]) {
        if !drawn {
            continue;
        }
        match scope.and_then(|sc| sc.footprint.map(|f| (sc, f))) {
            Some((sc, f)) => {
                let mut t = sc.footprint_gib;
                if t.level == 0.0 {
                    t = TrendView::flat(aterm_vitals::model::gib(f));
                }
                let mk = Mark::of(&t, cap, words::FOOTPRINT_GATE_GIB);
                let slot = (x - 1.0, sy - 1.0, w + 2.0, sh + 2.0);
                p.lane(x, sy, w, sh, mk, ink, slot, true);
            }
            None => groove(p, x, sy, w, sh),
        }
        hits.push(Hit::row(
            (x, sy - 1.0, w, sh + 2.0),
            Instrument::Memory,
            bar,
        ));
    }
}

fn core_row(
    p: &mut Painter<'_>,
    cpu: &CpuView,
    snap: &Snapshot,
    x0: f32,
    band_h: f32,
    fold: Fold,
    hits: &mut Vec<Hit>,
) -> f32 {
    let s = p.s;
    let n = cpu.total();
    // 16 px squares at 2×, 7 px at 1×; never taller than the lanes' band,
    // which ends at the card's foot (6 px squares on a 6 px card at 1×).
    let top = s.round().max(1.0) - (s / 2.0).round().min(1.0);
    let big = if s >= 1.5 { (8.0 * s).round() } else { 7.0 };
    let big = big.min((band_h - top).max(4.0));
    let gutter = (1.5 * s).round().max(1.0);
    let cluster_gap = (4.0 * s).round();
    let per_cluster = !fold.per_core || n > 96;
    let two_rows = !per_cluster && n > 24;
    let tile = if two_rows {
        ((big - gutter) / 2.0).floor().max(3.0)
    } else {
        big
    };
    let mut x = x0;
    let start = x0;
    for (ci, cluster) in cpu.clusters.iter().enumerate() {
        if ci > 0 {
            x += cluster_gap;
        }
        if per_cluster {
            let loads: Vec<f32> = cluster
                .cores
                .iter()
                .filter_map(|&c| cpu.cores.get(c).map(|v| v.load.level))
                .collect();
            let mean = loads.iter().sum::<f32>() / loads.len().max(1) as f32;
            let peak = loads.iter().copied().fold(0.0f32, f32::max);
            p.tile(x, top, big, Mark::still(mean), gutter);
            let ty = (top + big - peak * big).round();
            let tide = p.inks.tide;
            p.prims
                .push(panel(x + 2.0, ty, big - 4.0, 1.0, 0.0, a(tide, 0.9)));
            x += big;
            continue;
        }
        let cols = if two_rows {
            cluster.cores.len().div_ceil(2)
        } else {
            cluster.cores.len()
        };
        for (i, &core) in cluster.cores.iter().enumerate() {
            let (col, row) = if two_rows { (i / 2, i % 2) } else { (i, 0) };
            let tx = x + col as f32 * (tile + gutter);
            let ty = top + row as f32 * (tile + gutter);
            match cpu.cores.get(core) {
                Some(view) => {
                    let m = Mark::of(&view.load, 1.0, words::LOAD_GATE);
                    p.tile(tx, ty, tile, m, gutter);
                }
                None => p.tile_unmeasured(tx, ty, tile, tile),
            }
        }
        x += cols as f32 * (tile + gutter) - gutter;
    }
    hits.push(Hit::row(
        (start, 0.0, x - start, band_h),
        Instrument::Cpu,
        Bar::Global,
    ));
    if let Some(gpu) = snap.gpu.as_ref() {
        x += cluster_gap;
        let tw = (6.0 * s).round().max(4.0);
        let m = Mark::of(&gpu.load, 1.0, words::LOAD_GATE);
        p.tank((x, top, tw, big), m, None, gutter);
        hits.push(Hit::row(
            (x - 1.0, 0.0, tw + 2.0, band_h),
            Instrument::Gpu,
            Bar::Global,
        ));
        x += tw;
    }
    x
}

#[cfg(test)]
#[path = "stats_sill_tests.rs"]
mod tests;
