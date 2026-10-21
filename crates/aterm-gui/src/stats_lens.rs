// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LENS, the stats bars' close-up (`docs/DESIGN-system-awareness-2026-10-16.md`
//! §12.2): the three bars at full size with their words, 22 pt core tiles
//! with every channel, the balance bar and the telescope, and a focus well
//! for the instrument asked about, on a card hung beside the form it opened
//! from (4 pt under the Deck, 4 pt over the sill). It reserves no row: a
//! summoned overlay, folded into the window's one tray surface with the bars
//! (`App::fold_stats_deck`), so `image` captures carry it.
//!
//! * **Opening.** The pointer resting on a Deck or sill instrument for
//!   [`REST`] with no motion (the `StatsLens` deadline, armed by motion over
//!   an instrument and re-armed at each motion, so passing over never opens
//!   it); a click on one, which pins it; or ⌘I (`ToggleStatsLens`), the
//!   keyboard lens. It never opens itself, not for a stuck job, an agent or a
//!   value (O33).
//! * **Closing.** An unpinned hover lens closes when the pointer has left the
//!   lens and the form for [`LEAVE`]; any lens closes on a click outside, on
//!   focus loss, and on any key the terminal would receive, which then passes
//!   through (one frame, no spring). The keyboard lens keeps ←/→ (the
//!   instrument), ↑/↓ (the bar), Return (pin), ⌘C (copy the bar's sentence)
//!   and Esc (close, consumed).
//! * **Material.** The shell is an opaque step off the band ground with a
//!   rim and a soft shadow (the header's words sit on it, so nothing of the
//!   terminal or its effects may show through); every tile and bar sits in a
//!   well in the band ground, the ground the inks' floors were solved on.

use std::time::{Duration, Instant};

use aterm_vitals::ink::{self, Rgb, VitalsInks};
use aterm_vitals::model::{ProcScope, Snapshot};
use aterm_vitals::words;

use crate::stats_deck::{DeckInput, StatsBars};
use crate::stats_paint::{Bar, Clause, Hit, Instrument, Mark, Painter, Rect, a, panel};
use crate::widget::{DrawPrim, rgba};

/// How long the pointer rests on an instrument before the hover lens opens.
pub(crate) const REST: Duration = Duration::from_millis(250);
/// How long an unpinned hover lens outlives the pointer leaving it and the
/// form it opened from.
pub(crate) const LEAVE: Duration = Duration::from_millis(300);
/// The vitals cadence while a lens is open (2 Hz, design §6.1).
pub(crate) const LENS_PERIOD: Duration = Duration::from_millis(500);

/// How a lens was opened, which decides how it closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Opened {
    /// The pointer rested on an instrument: it leaves with the pointer.
    Hover,
    /// A click pinned it: it stays until a click outside, a key or Esc.
    Pinned,
    /// ⌘I: it takes ←/→/↑/↓, Return, ⌘C and Esc.
    Keyboard,
}

/// An open lens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Lens {
    /// The focus well's instrument.
    pub(crate) focus: Instrument,
    /// The selected bar (↑/↓; the narrow lens shows only it).
    pub(crate) bar: Bar,
    pub(crate) how: Opened,
}

/// A pointer rest armed over an instrument.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rest {
    /// When it opens the lens.
    pub(crate) due: Instant,
    /// What the pointer rests on.
    pub(crate) hit: Hit,
}

/// The lens's per-window state.
#[derive(Clone, Debug, Default)]
pub(crate) struct LensSlot {
    /// The open lens, if any.
    pub(crate) open: Option<Lens>,
    /// A rest armed over an instrument.
    pub(crate) rest: Option<Rest>,
    /// When an unpinned hover lens closes, the pointer having left.
    pub(crate) leave_due: Option<Instant>,
    /// The lens card's hit regions (card pixels) from its last layout.
    pub(crate) hits: Vec<Hit>,
    /// The lens card's placement in the frame: `(x, y, w, h)` of the card.
    pub(crate) rect: Option<(u32, u32, u32, u32)>,
}

/// The lens card's geometry: the card (shell plus shadow margin) in frame
/// device pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LensGeom {
    /// The card's frame origin.
    pub(crate) dx: u32,
    pub(crate) dy: u32,
    /// The card's size.
    pub(crate) w: f32,
    pub(crate) h: f32,
    /// The shadow margin round the shell.
    pub(crate) margin: f32,
    pub(crate) scale: f32,
    pub(crate) font_px: f32,
}

/// Where the lens hangs for a form card at `(card_dy, card_h)` over a grid
/// whose left edge is `grid_x`, `grid_w` × `grid_h` device px from
/// `grid_y`. `None` when the window is too small to hold one.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lens_geom(
    form: StatsBars,
    card_dy: u32,
    card_h: f32,
    grid_x: f32,
    grid_y: f32,
    grid_w: f32,
    grid_h: f32,
    scale: f32,
    font_px: f32,
) -> Option<LensGeom> {
    let s = scale.max(0.5);
    let shell_w = (1024.0 * s).min(grid_w - 24.0 * s).floor();
    let shell_h = (250.0 * s).min(0.7 * grid_h).floor();
    if shell_w < 160.0 * s || shell_h < 90.0 * s {
        return None;
    }
    let margin = (6.0 * s).round();
    let x = (grid_x + (grid_w - shell_w) / 2.0).round();
    let gap = (4.0 * s).round();
    let y = match form {
        StatsBars::Deck => card_dy as f32 + card_h + gap,
        StatsBars::Sill => card_dy as f32 - gap - shell_h,
        StatsBars::Off => grid_y + gap,
    }
    .max(0.0);
    Some(LensGeom {
        dx: (x - margin).max(0.0) as u32,
        dy: (y - margin).max(0.0) as u32,
        w: shell_w + 2.0 * margin,
        h: shell_h + 2.0 * margin,
        margin,
        scale: s,
        font_px,
    })
}

/// A scope's sentence (`aterm_vitals::words`), the one words function the
/// lens heading, the accessibility description and `sys` share.
pub(crate) fn sentence(snap: &Snapshot, bar: Bar, tab_key: Option<u64>, tab_label: &str) -> String {
    match bar {
        Bar::Global => words::global_sentence(snap),
        Bar::Aterm => words::aterm_sentence(snap),
        Bar::Tab => tab_key
            .and_then(|k| snap.tab(k))
            .map_or_else(|| format!("{tab_label}: measuring."), words::tab_sentence),
    }
}

/// A sentence as clauses (split at `; `), so a narrow well folds it from the
/// end instead of cutting a word.
fn sentence_clauses(text: &str, ink: Rgb) -> Vec<Clause> {
    let body = text.trim_end_matches('.');
    body.split("; ").map(|c| Clause::new(c, ink)).collect()
}

/// The focus well's readout for an instrument: the words its header shows.
pub(crate) fn readout(snap: &Snapshot, focus: Instrument) -> String {
    match focus {
        Instrument::Cores => snap.cpu.as_ref().map_or_else(
            || "Cores: not read yet".to_string(),
            |c| {
                let names: Vec<String> = c
                    .clusters
                    .iter()
                    .map(|cl| format!("{} \u{00D7}{}", cl.name, cl.cores.len()))
                    .collect();
                format!(
                    "{} of {} cores busy{} \u{00B7} {}",
                    words::cores(c.busy.level),
                    c.total(),
                    c.busy
                        .motion(words::CORES_GATE)
                        .word()
                        .map_or(String::new(), |w| format!(", {w}")),
                    names.join(", ")
                )
            },
        ),
        Instrument::Gpu => snap.gpu.as_ref().map_or_else(
            || "GPU: not readable here".to_string(),
            |g| {
                let cores = g
                    .cores
                    .map_or_else(String::new, |n| format!("{n} cores, one device \u{00B7} "));
                format!("GPU \u{00B7} {cores}{:.0}% (whole percent)", (g.load.raw * 100.0).round())
            },
        ),
        Instrument::Memory => snap.mem.as_ref().map_or_else(
            || "Memory: not read yet".to_string(),
            |m| {
                let s = &m.sample;
                // A class this OS does not report is left out, never 0.
                let compressed = s.compressed.map_or_else(String::new, |c| {
                    format!(", compressed {}", words::gib_figure(c))
                });
                format!(
                    "{} of {} GiB used: app {}, wired {}{compressed} \u{00B7} cache {} \u{00B7} pressure {} \u{00B7} swap {}",
                    words::gib_figure(s.used()),
                    words::gib_total(s.total),
                    words::gib_figure(s.app),
                    words::gib_figure(s.wired),
                    words::gib_figure(s.cached),
                    s.pressure.word(),
                    if s.swap_used == 0 {
                        "0".to_string()
                    } else {
                        words::mem(s.swap_used)
                    }
                )
            },
        ),
        Instrument::Disk => snap.volume.as_ref().map_or_else(
            || "Disk: not read yet".to_string(),
            |v| {
                format!(
                    "{} free of {} on {}",
                    words::disk(v.available),
                    words::disk(v.total),
                    v.mount
                )
            },
        ),
        Instrument::Jobs => match snap.jobs.as_deref() {
            None => "Jobs: not read yet".to_string(),
            Some(view) => {
                let marks = crate::stats_paint::marks_of(view);
                if marks.is_empty() {
                    "nothing stuck, quiet or waiting".to_string()
                } else {
                    marks
                        .iter()
                        .map(|m| m.words())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            }
        },
    }
}

/// Paint the lens: its prims and its hit regions (card pixels).
pub(crate) fn lens_paint(
    lg: &LensGeom,
    input: &DeckInput<'_>,
    lens: &Lens,
) -> (Vec<DrawPrim>, Vec<Hit>) {
    let inks = input.inks;
    let s = lg.scale;
    let mut p = Painter::new(&inks, s, lg.font_px, input.dwc);
    let mut hits = Vec::new();
    let m = lg.margin;
    let (sx, sy, sw, sh) = (m, m, lg.w - 2.0 * m, lg.h - 2.0 * m);
    let radius = p.px(10.0);
    // The soft shadow: black at 18 % over 12 px, laid as rings.
    let rings = m as usize;
    for i in (1..=rings).rev() {
        let d = i as f32;
        let k = 0.18 * (1.0 - d / (m + 1.0)).powi(2) / rings as f32 * 2.5;
        p.prims.push(panel(
            sx - d,
            sy - d + (d * 0.4).round(),
            sw + 2.0 * d,
            sh + 2.0 * d,
            radius + d,
            rgba([0, 0, 0], (k * 255.0).round() as u8),
        ));
    }
    let shell = ink::mix(inks.ground, inks.groove, 0.9);
    p.prims
        .push(panel(sx, sy, sw, sh, radius, rgba(shell, 0xFF)));
    p.prims.push(DrawPrim::Stroke {
        x: sx,
        y: sy,
        w: sw,
        h: sh,
        radius,
        width: 1.0,
        color: a(inks.rim, 0.9),
    });
    let pad = p.px(10.0);
    let head_h = p.px(20.0);
    let (ix, iy, iw, ih) = (sx + pad, sy + pad, sw - 2.0 * pad, sh - 2.0 * pad);
    // The header: what this is, what the focus well shows, how to leave.
    let base = crate::tray_raster::row_baseline(iy, head_h, p.text_px.get());
    p.label(ix, base, "System".into(), inks.words, true, false);
    let title_w = p.label_w("System", true, false);
    let hint = match lens.how {
        Opened::Keyboard => {
            "\u{2190}\u{2192} instrument \u{00B7} \u{2191}\u{2193} bar \u{00B7} \u{2318}C copy \u{00B7} esc"
        }
        Opened::Pinned => "pinned \u{00B7} esc or click outside",
        Opened::Hover => "click to pin \u{00B7} \u{2318}I",
    };
    let hint_w = p.label_w(hint, false, true);
    let small_base = crate::tray_raster::row_baseline(iy, head_h, p.small_px.get());
    p.label(
        ix + iw - hint_w,
        small_base,
        hint.into(),
        inks.words2,
        false,
        true,
    );
    let Some(snap) = input.snap else {
        p.label(
            ix + title_w + p.px(8.0),
            base,
            "Reading the machine\u{2026}".into(),
            inks.words2,
            false,
            false,
        );
        return (p.prims, hits);
    };
    // The narrow lens shows one bar: a Global · aterm · Tab switch beside the
    // title says which, and the pointer (or ↑/↓) moves it (§12.2).
    let wide = iw >= 550.0 * s;
    let mut head_x = ix + title_w + p.px(10.0);
    if !wide {
        for (k, bar) in Bar::ALL.iter().enumerate() {
            let word = match bar {
                Bar::Global => "Global",
                Bar::Aterm => "aterm",
                Bar::Tab => "Tab",
            };
            let on = *bar == lens.bar;
            let ww = p.label_w(word, on, true);
            if on {
                let kh = p.hair().max(1.0) * 2.0;
                p.prims.push(panel(
                    head_x,
                    (iy + head_h - kh).round(),
                    ww,
                    kh,
                    0.0,
                    rgba(inks.tab, 0xFF),
                ));
            }
            p.label(
                head_x,
                small_base,
                word.into(),
                if on { inks.words } else { inks.words2 },
                on,
                true,
            );
            hits.push(Hit {
                rect: (head_x - p.px(3.0), iy, ww + p.px(6.0), head_h),
                instrument: lens.focus,
                bar: *bar,
            });
            head_x += ww + p.px(if k + 1 < Bar::ALL.len() { 8.0 } else { 12.0 });
        }
    }
    let head_room = (ix + iw - hint_w - p.px(14.0) - head_x).max(0.0);
    let readout = readout(snap, lens.focus);
    let parts = p.fit_clauses(
        &sentence_clauses(
            &format!("{} \u{00B7} {readout}", lens.focus.word()),
            inks.words2,
        ),
        head_room,
        false,
    );
    if !parts.is_empty() {
        let _ = p.clauses(head_x, base, parts, false);
    }

    let body_y = iy + head_h + p.px(4.0);
    let body_h = ih - head_h - p.px(4.0);
    let gap = p.px(6.0);
    // Wide: three bar wells beside the focus well. Narrow: the selected bar
    // over the focus well.
    let (bars, focus_rect): (Vec<(Bar, Rect)>, Rect) = if wide {
        let focus_w = (314.0 * s).min((iw * 0.42).floor());
        let bars_w = iw - focus_w - gap;
        let well_h = ((body_h - 2.0 * gap) / 3.0).floor();
        let bars = Bar::ALL
            .iter()
            .enumerate()
            .map(|(i, b)| (*b, (ix, body_y + i as f32 * (well_h + gap), bars_w, well_h)))
            .collect();
        (bars, (ix + bars_w + gap, body_y, focus_w, body_h))
    } else {
        let well_h = ((body_h - gap) * 0.36).floor();
        (
            vec![(lens.bar, (ix, body_y, iw, well_h))],
            (ix, body_y + well_h + gap, iw, body_h - well_h - gap),
        )
    };
    for (bar, rect) in bars {
        well(
            &mut p,
            rect,
            lens.bar == bar && lens.how == Opened::Keyboard,
        );
        hits.push(Hit {
            rect,
            instrument: if bar == Bar::Global {
                Instrument::Cores
            } else {
                lens.focus
            },
            bar,
        });
        match bar {
            Bar::Global => global_well(&mut p, rect, snap, &mut hits),
            Bar::Aterm => aterm_well(&mut p, rect, snap, input),
            Bar::Tab => tab_well(&mut p, rect, snap, input),
        }
    }
    well(&mut p, focus_rect, false);
    match lens.focus {
        Instrument::Cores => cores_focus(&mut p, focus_rect, snap, input),
        Instrument::Gpu => gpu_focus(&mut p, focus_rect, snap),
        Instrument::Memory => memory_focus(&mut p, focus_rect, snap),
        Instrument::Disk => disk_focus(&mut p, focus_rect, snap),
        Instrument::Jobs => jobs_focus(&mut p, focus_rect, snap, input),
    }
    (p.prims, hits)
}

/// A well: the band ground at α 1, a hairline rim (the accent's when the
/// keyboard has selected it).
fn well(p: &mut Painter<'_>, (x, y, w, h): Rect, selected: bool) {
    let inks = *p.inks;
    let r = p.px(6.0);
    p.prims.push(panel(x, y, w, h, r, rgba(inks.ground, 0xFF)));
    p.prims.push(DrawPrim::Stroke {
        x,
        y,
        w,
        h,
        radius: r,
        width: if selected { p.hair() * 2.0 } else { 1.0 },
        color: if selected {
            rgba(inks.tab, 0xFF)
        } else {
            a(inks.groove, 1.0)
        },
    });
}

/// A well's heading: its scope's key swatch and sentence, folded to fit.
/// Returns the baseline row's bottom.
fn heading(p: &mut Painter<'_>, (x, y, w, _): Rect, bar: Bar, text: &str) -> f32 {
    let inks = *p.inks;
    let pad = p.px(6.0);
    let row_h = p.px(16.0);
    let base = crate::tray_raster::row_baseline(y + pad / 2.0, row_h, p.small_px.get());
    let key_w = p.px(2.5).max(2.0);
    let key_h = (p.small_px.get() * 0.8).round();
    let ky = (y + pad / 2.0 + (row_h - key_h) / 2.0).round();
    match bar {
        Bar::Global => p.ramp_keyline(x + pad, ky, key_w, key_h),
        Bar::Aterm => p.keyline(x + pad, ky, key_w, key_h, inks.aterm),
        Bar::Tab => p.keyline(x + pad, ky, key_w, key_h, inks.tab),
    }
    let tx = x + pad + key_w + p.px(4.0);
    let parts = p.fit_clauses(&sentence_clauses(text, inks.words), x + w - pad - tx, true);
    let _ = p.clauses(tx, base, parts, true);
    y + pad / 2.0 + row_h
}

/// The Global well: tiles at the ladder's top rung, the GPU tank, memory and
/// disk.
fn global_well(p: &mut Painter<'_>, rect: Rect, snap: &Snapshot, hits: &mut Vec<Hit>) {
    let inks = *p.inks;
    let (x, y, w, h) = rect;
    let top = heading(p, rect, Bar::Global, &words::global_sentence(snap));
    let pad = p.px(6.0);
    let body_h = (y + h - pad - top).max(0.0);
    let mut cx = x + pad;
    if let Some(cpu) = snap.cpu.as_ref() {
        let gap = p.px(3.0);
        let lab = p.small_px.get() + p.px(2.0);
        let tile = (22.0 * p.s).min(body_h - lab).floor().max(6.0);
        let base = crate::tray_raster::row_baseline(top, lab, p.small_px.get());
        for cluster in &cpu.clusters {
            let start = cx;
            p.label(
                cx,
                base,
                format!("{} \u{00D7}{}", cluster.name, cluster.cores.len()),
                inks.words2,
                false,
                true,
            );
            let label_w = p.label_w(
                &format!("{} \u{00D7}{}", cluster.name, cluster.cores.len()),
                false,
                true,
            );
            for &core in &cluster.cores {
                if let Some(view) = cpu.cores.get(core) {
                    p.tile(
                        cx,
                        top + lab,
                        tile,
                        Mark::of(&view.load, 1.0, words::LOAD_GATE),
                        gap * 2.0,
                    );
                } else {
                    p.tile_unmeasured(cx, top + lab, tile, tile);
                }
                cx += tile + gap;
            }
            cx = cx.max(start + label_w + gap) + p.px(6.0);
            hits.push(Hit {
                rect: (start, top, cx - start, body_h),
                instrument: Instrument::Cores,
                bar: Bar::Global,
            });
        }
        if let Some(gpu) = snap.gpu.as_ref() {
            let start = cx;
            p.label(cx, base, "GPU".into(), inks.words2, false, true);
            let tw = (tile * 0.62).round();
            p.tank(
                (cx, top + lab, tw, tile),
                Mark::of(&gpu.load, 1.0, words::LOAD_GATE),
                gpu.cores,
                gap * 2.0,
            );
            cx += tw.max(p.label_w("GPU", false, true)) + p.px(8.0);
            hits.push(Hit {
                rect: (start, top, cx - start, body_h),
                instrument: Instrument::Gpu,
                bar: Bar::Global,
            });
        }
    }
    // Memory and disk share the rest, a lane and a figure each.
    let rest = (x + w - pad - cx).max(0.0);
    if rest < p.px(60.0) {
        return;
    }
    let lane_h = p.px(6.0);
    let row = |i: usize| top + (body_h / 2.0) * i as f32;
    let fig_w = p
        .figure_w("88.8/888G", true)
        .max(p.figure_w("8.88T free", true))
        + p.px(4.0);
    let lane_w = (rest - fig_w).max(0.0);
    if let Some(m) = snap.mem.as_ref() {
        let ry = row(0);
        let ly = (ry + (body_h / 2.0 - lane_h) / 2.0).round();
        crate::stats_deck::global_memory(p, cx, ly, lane_w, lane_h, m);
        let base = crate::tray_raster::row_baseline(ry, body_h / 2.0, p.small_px.get());
        p.figure(
            cx + lane_w + p.px(4.0),
            base,
            format!(
                "{}/{}G",
                words::gib_figure(m.sample.used()),
                words::gib_total(m.sample.total)
            ),
            inks.words,
            true,
        );
        hits.push(Hit {
            rect: (cx, ry, rest, body_h / 2.0),
            instrument: Instrument::Memory,
            bar: Bar::Global,
        });
    }
    if let Some(v) = snap.volume.as_ref() {
        let ry = row(1);
        let ly = (ry + (body_h / 2.0 - lane_h) / 2.0).round();
        let level = if v.total > 0 {
            v.total.saturating_sub(v.available) as f32 / v.total as f32
        } else {
            0.0
        };
        let slot = (cx - 2.0, ly - 2.0, lane_w + 4.0, lane_h + 4.0);
        p.lane(
            cx,
            ly,
            lane_w,
            lane_h,
            Mark::still(level),
            inks.mem[0],
            slot,
            false,
        );
        let base = crate::tray_raster::row_baseline(ry, body_h / 2.0, p.small_px.get());
        let free = words::disk(v.available)
            .replace(" TB", "T")
            .replace(" GB", "G");
        p.figure(
            cx + lane_w + p.px(4.0),
            base,
            format!("{free} free"),
            inks.words,
            true,
        );
        hits.push(Hit {
            rect: (cx, ry, rest, body_h / 2.0),
            instrument: Instrument::Disk,
            bar: Bar::Global,
        });
    }
}

/// The aterm well: the balance bar of what is busy now (Tab │ rest of aterm
/// │ others, 100 % = busy) and the telescope (Tab ⊂ aterm ⊂ busy ⊂
/// capacity, on the capacity scale).
fn aterm_well(p: &mut Painter<'_>, rect: Rect, snap: &Snapshot, input: &DeckInput<'_>) {
    let inks = *p.inks;
    let (x, y, w, h) = rect;
    let top = heading(p, rect, Bar::Aterm, &words::aterm_sentence(snap));
    let pad = p.px(6.0);
    let body_h = (y + h - pad - top).max(0.0);
    let Some(cpu) = snap.cpu.as_ref() else {
        return;
    };
    let capacity = cpu.total().max(1) as f32;
    let busy = cpu.busy.level.max(0.0);
    let at = snap
        .aterm
        .as_ref()
        .map_or(0.0, |s| s.cores.level)
        .min(busy.max(0.0));
    let tab = input
        .tab_key
        .and_then(|k| snap.tab(k))
        .map_or(0.0, |t| t.scope.cores.level)
        .min(at);
    let busy_label = format!("busy {}c", words::cores(busy));
    // The labels' column fits the wider of its two words, whatever the
    // figure (`busy 100.0c` on a big machine).
    let label_w = p
        .label_w("telescope", false, true)
        .max(p.label_w(&busy_label, false, true))
        + p.px(6.0);
    let bx = x + pad + label_w;
    let bw = (w - 2.0 * pad - label_w).max(0.0);
    let bar_h = p.px(8.0);
    let row_h = body_h / 2.0;
    // The balance bar.
    let ry = top;
    let ly = (ry + (row_h - bar_h) / 2.0).round();
    let base = crate::tray_raster::row_baseline(ry, row_h, p.small_px.get());
    p.label(x + pad, base, busy_label, inks.words2, false, true);
    let r = (bar_h / 2.0).floor();
    p.prims
        .push(panel(bx, ly, bw, bar_h, r, rgba(inks.track, 0xFF)));
    if busy > 0.01 {
        // Capsules from the left, the telescope's grammar: the others' part
        // is the whole bar, aterm's laid over it, the tab's over that, each
        // nearer part with a 1 px rim of the ground at its end, so the parts
        // stay apart without colour.
        let parts = [(tab, inks.tab), (at, inks.aterm), (busy, inks.others)];
        for (i, (end, ink)) in parts.iter().enumerate().rev() {
            let pw = (end / busy * bw).round().min(bw);
            if pw < 1.0 {
                continue;
            }
            if i < 2 && pw < bw {
                p.prims.push(panel(
                    bx,
                    ly,
                    (pw + 1.0).min(bw),
                    bar_h,
                    r,
                    rgba(inks.ground, 0xFF),
                ));
            }
            p.prims
                .push(panel(bx, ly, pw, bar_h, r.min(pw / 2.0), rgba(*ink, 0xFF)));
        }
    }
    // The telescope: nested bars on the capacity scale.
    let ry = top + row_h;
    let base = crate::tray_raster::row_baseline(ry, row_h, p.small_px.get());
    p.label(x + pad, base, "telescope".into(), inks.words2, false, true);
    let ty = (ry + (row_h - bar_h) / 2.0).round();
    let nest = [
        (capacity, inks.track, bar_h),
        (busy, inks.others, bar_h),
        (at, inks.aterm, (bar_h * 0.7).round()),
        (tab, inks.tab, (bar_h * 0.4).round()),
    ];
    for (v, c, nh) in nest {
        let nw = (v / capacity * bw).round();
        if nw >= 1.0 {
            p.prims.push(panel(
                bx,
                ty + ((bar_h - nh) / 2.0).round(),
                nw,
                nh,
                (nh / 2.0).floor(),
                rgba(c, 0xFF),
            ));
        }
    }
}

/// The Tab well: the tab's state and words, cores on the capacity scale,
/// its footprint.
fn tab_well(p: &mut Painter<'_>, rect: Rect, snap: &Snapshot, input: &DeckInput<'_>) {
    let inks = *p.inks;
    let (x, y, w, h) = rect;
    let text = sentence(snap, Bar::Tab, input.tab_key, &input.tab_label);
    let top = heading(p, rect, Bar::Tab, &text);
    let pad = p.px(6.0);
    let body_h = (y + h - pad - top).max(0.0);
    let Some(t) = input.tab_key.and_then(|k| snap.tab(k)) else {
        return;
    };
    let capacity = snap.total_cores().unwrap_or(1).max(1) as f32;
    let lane_h = p.px(6.0);
    let fig = words::cores_figure(
        t.scope.cores.level,
        None,
        t.scope.cores.motion(words::CORES_GATE),
    );
    let fig_w = p.figure_w("88.8c\u{25B4}", true) + p.px(6.0);
    let mem = t.scope.footprint.map_or_else(String::new, words::mem);
    let mem_w = p.figure_w("888 MiB", true) + p.px(6.0);
    let lx = x + pad;
    let lw = (w - 2.0 * pad - fig_w - mem_w).max(0.0);
    let ly = (top + (body_h - lane_h) / 2.0).round();
    let slot = (lx - 2.0, ly - 3.0, lw + 4.0, lane_h + 6.0);
    p.lane(
        lx,
        ly,
        lw,
        lane_h,
        Mark::of(&t.scope.cores, capacity, words::CORES_GATE),
        inks.tab,
        slot,
        false,
    );
    let base = crate::tray_raster::row_baseline(top, body_h, p.small_px.get());
    p.figure(lx + lw + p.px(6.0), base, fig, inks.words, true);
    p.figure(lx + lw + fig_w, base, mem, inks.words, true);
}

/// The Cores focus: a 60 s core × time strip (one row a core, a column a
/// second, gaps dotted), the busiest processes with their scope, and the
/// honest sentence about what macOS reports.
fn cores_focus(p: &mut Painter<'_>, rect: Rect, snap: &Snapshot, input: &DeckInput<'_>) {
    let inks = *p.inks;
    let (x, y, w, h) = rect;
    let pad = p.px(6.0);
    let row_h = p.px(16.0);
    let base = crate::tray_raster::row_baseline(y + pad / 2.0, row_h, p.small_px.get());
    p.label(
        x + pad,
        base,
        "Cores \u{00B7} the last minute".into(),
        inks.words,
        true,
        true,
    );
    let Some(cpu) = snap.cpu.as_ref() else {
        return;
    };
    let note = "macOS reports shares, not which core runs a process.";
    let strip_top = y + pad / 2.0 + row_h;
    let n = cpu.total().max(1);
    // The strip keeps a floor of 3 pt a core (never a band of dashes); the
    // busiest processes take what is left, then the note.
    let strip_min = (n as f32 * p.px(3.0)).min((h - pad - row_h).max(0.0));
    let left = (h - pad - row_h - strip_min - p.px(4.0)).max(0.0);
    let proc_rows = ((left / row_h).floor() as usize).min(snap.top.len().min(4));
    let left = left - proc_rows as f32 * row_h;
    let note_h = if left >= row_h { row_h } else { 0.0 };
    let procs_h = proc_rows as f32 * row_h;
    let strip_h = (h - pad - row_h - procs_h - note_h - p.px(4.0)).max(strip_min);
    // A row a core, tall enough for its name when the well has room
    // (10 pt), down to one pixel for a machine of hundreds.
    let cell_h = (strip_h / n as f32).floor().clamp(1.0, 10.0 * p.s);
    let label_w = p.figure_w("E10", true) + p.px(4.0);
    let cols = aterm_vitals::model::CORE_HISTORY_LEN;
    let cell_w = ((w - 2.0 * pad - label_w) / cols as f32).floor().max(1.0);
    let sx = x + pad + label_w;
    let label_h = p.small_px.get();
    // A name is drawn only where it does not overlap the one above it: every
    // core's when its row holds a name, else each cluster's first that fits.
    let mut label_floor = f32::MIN;
    let mut row = 0usize;
    for cluster in &cpu.clusters {
        for (k, &core) in cluster.cores.iter().enumerate() {
            let ry = strip_top + row as f32 * cell_h;
            let every = cell_h >= label_h * 0.9;
            if (every || k == 0) && ry >= label_floor {
                let b = crate::tray_raster::row_baseline(ry, cell_h.max(label_h), label_h);
                p.figure(
                    x + pad,
                    b,
                    format!("{}{}", cluster.short, k + 1),
                    inks.words2,
                    true,
                );
                label_floor = ry + label_h;
            }
            let hist = cpu.core_history.get(core).cloned().unwrap_or_default();
            let off = cols.saturating_sub(hist.len());
            for c in 0..cols {
                let cx = sx + c as f32 * cell_w;
                match hist.get(c.wrapping_sub(off)).copied().flatten() {
                    Some(v) if c >= off => p.prims.push(panel(
                        cx,
                        ry,
                        cell_w,
                        (cell_h - 1.0).max(1.0),
                        0.0,
                        rgba(inks.load_at(v), 0xFF),
                    )),
                    _ => {
                        if c % 2 == 0 {
                            p.prims.push(panel(
                                cx,
                                ry + (cell_h / 2.0).floor(),
                                cell_w,
                                1.0,
                                0.0,
                                a(inks.track, 0.8),
                            ));
                        }
                    }
                }
            }
            row += 1;
        }
    }
    // The busiest processes, with whose they are.
    let mut py = strip_top + n as f32 * cell_h + p.px(4.0);
    for tp in snap.top.iter().take(proc_rows) {
        let b = crate::tray_raster::row_baseline(py, row_h, p.small_px.get());
        let (chip, ink) = match tp.scope {
            ProcScope::Tab(k) => (
                snap.tab(k)
                    .map_or_else(|| "tab".to_string(), |t| t.label.to_lowercase()),
                if Some(k) == input.tab_key {
                    inks.tab
                } else {
                    inks.aterm
                },
            ),
            ProcScope::Aterm => ("aterm".to_string(), inks.aterm),
            ProcScope::Other => ("other".to_string(), inks.words2),
        };
        // A process's name is text whoever built it chose: drawn as the
        // person's sentence shows it (§13.3), never raw.
        p.label(
            x + pad,
            b,
            aterm_vitals::jobs::wire::person_name(&tp.name),
            inks.words,
            false,
            true,
        );
        let fx = x + w * 0.55;
        p.figure(
            fx,
            b,
            format!("{}c", words::cores(tp.cores)),
            inks.words,
            true,
        );
        let cw = p.figure_w("88.8c", true) + p.px(8.0);
        p.keyline(
            fx + cw,
            py + (row_h * 0.3).round(),
            p.px(2.5).max(2.0),
            (row_h * 0.4).round(),
            ink,
        );
        p.label(fx + cw + p.px(6.0), b, chip, inks.words2, false, true);
        py += row_h;
    }
    if note_h == 0.0 {
        return;
    }
    let b = crate::tray_raster::row_baseline(y + h - pad / 2.0 - note_h, note_h, p.small_px.get());
    let parts = p.fit_clauses(
        &[Clause::new(note, inks.words2).or_short("macOS reports shares only.")],
        w - 2.0 * pad,
        true,
    );
    let _ = p.clauses(x + pad, b, parts, true);
}

/// The GPU focus: the tank at full size with its compartments, the trace.
fn gpu_focus(p: &mut Painter<'_>, rect: Rect, snap: &Snapshot) {
    let inks = *p.inks;
    let (x, y, w, h) = rect;
    let pad = p.px(6.0);
    let row_h = p.px(16.0);
    let base = crate::tray_raster::row_baseline(y + pad / 2.0, row_h, p.small_px.get());
    p.label(
        x + pad,
        base,
        "GPU \u{00B7} one device".into(),
        inks.words,
        true,
        true,
    );
    let Some(g) = snap.gpu.as_ref() else {
        return;
    };
    let top = y + pad / 2.0 + row_h + p.px(4.0);
    let th = (h - pad - (top - y)).max(0.0);
    let tw = (th * 0.5).round().min(p.px(60.0));
    p.tank(
        (x + pad, top, tw, th),
        Mark::of(&g.load, 1.0, words::LOAD_GATE),
        g.cores,
        p.px(6.0),
    );
    let sx = x + pad + tw + p.px(10.0);
    p.spark(
        sx,
        top,
        (x + w - pad - sx).max(0.0),
        (th * 0.6).round(),
        &g.history,
        inks.load[3],
    );
}

/// The Memory focus: the class waffle, 1 GiB a square, with swap as dashed
/// squares past it, and the classes in words.
fn memory_focus(p: &mut Painter<'_>, rect: Rect, snap: &Snapshot) {
    let inks = *p.inks;
    let (x, y, w, h) = rect;
    let pad = p.px(6.0);
    let row_h = p.px(16.0);
    let base = crate::tray_raster::row_baseline(y + pad / 2.0, row_h, p.small_px.get());
    p.label(
        x + pad,
        base,
        "Memory \u{00B7} 1 GiB a square".into(),
        inks.words,
        true,
        true,
    );
    let Some(m) = snap.mem.as_ref() else {
        return;
    };
    let s = &m.sample;
    let gib = |b: u64| (b as f64 / f64::from(1u32 << 30)).round() as usize;
    let classes = [
        ("app", gib(s.app), inks.mem[0], 1.0),
        ("wired", gib(s.wired), inks.mem[0], 0.7),
        (
            "compressed",
            gib(s.compressed.unwrap_or(0)),
            inks.mem[1],
            1.0,
        ),
        ("cache", gib(s.cached), inks.mem[2], 1.0),
    ];
    let total = gib(s.total).max(1);
    let top = y + pad / 2.0 + row_h + p.px(2.0);
    let legend_h = row_h * 2.0;
    let area_w = w - 2.0 * pad;
    let area_h = (h - pad - (top - y) - legend_h).max(0.0);
    // The largest square that fits every GiB (and swap) in the area.
    let swap = gib(s.swap_used);
    let cells = total + swap;
    let mut side = (14.0 * p.s).round();
    while side > 3.0 {
        let per_row = ((area_w + 2.0) / (side + 2.0)).floor().max(1.0) as usize;
        if (cells.div_ceil(per_row) as f32) * (side + 2.0) <= area_h {
            break;
        }
        side -= 1.0;
    }
    let per_row = ((area_w + 2.0) / (side + 2.0)).floor().max(1.0) as usize;
    let mut i = 0usize;
    let mut cell = |p: &mut Painter<'_>, fill: Option<(Rgb, f32)>, dashed: bool| {
        let cx = x + pad + (i % per_row) as f32 * (side + 2.0);
        let cy = top + (i / per_row) as f32 * (side + 2.0);
        match fill {
            Some((c, al)) => {
                p.prims
                    .push(panel(cx, cy, side, side, (side * 0.18).round(), a(c, al)))
            }
            None if dashed => {
                let c = a(inks.warn, 0.9);
                p.dotted(cx, cy, cx + side, cy, 1.0, c);
                p.dotted(cx, cy + side - 1.0, cx + side, cy + side - 1.0, 1.0, c);
            }
            None => p.prims.push(panel(
                cx,
                cy,
                side,
                side,
                (side * 0.18).round(),
                rgba(inks.track, 0xFF),
            )),
        }
        i += 1;
    };
    let mut used = 0usize;
    for (_, n, c, al) in classes {
        for _ in 0..n.min(total.saturating_sub(used)) {
            cell(p, Some((c, al)), false);
        }
        used = (used + n).min(total);
    }
    for _ in used..total {
        cell(p, None, false);
    }
    for _ in 0..swap {
        cell(p, None, true);
    }
    let ly = y + h - pad / 2.0 - legend_h;
    let mut clauses: Vec<Clause> = classes
        .iter()
        .map(|(name, _, _, _)| *name)
        .zip([Some(s.app), Some(s.wired), s.compressed, Some(s.cached)])
        // A class this OS does not report is left out of the legend.
        .filter_map(|(name, b)| {
            b.map(|b| Clause::new(format!("{name} {}", words::gib_figure(b)), inks.words2))
        })
        .collect();
    clauses.push(Clause::new(
        format!("free {}", words::gib_figure(s.free)),
        inks.words2,
    ));
    let parts = p.fit_clauses(&clauses, area_w, true);
    let b = crate::tray_raster::row_baseline(ly, row_h, p.small_px.get());
    let _ = p.clauses(x + pad, b, parts, true);
    let pressure_ink = match s.pressure {
        aterm_vitals::Pressure::Warn => inks.warn,
        aterm_vitals::Pressure::Critical => inks.critical,
        _ => inks.words2,
    };
    let mut second = vec![Clause::new(
        format!("pressure {}", s.pressure.word()),
        pressure_ink,
    )];
    second.push(Clause::new(
        if s.swap_used == 0 {
            "swap 0".to_string()
        } else {
            format!("swap {}", words::mem(s.swap_used))
        },
        if s.swap_used == 0 {
            inks.words2
        } else {
            inks.warn
        },
    ));
    // The commit charge against its limit, where the OS keeps one (design
    // §9.6): Windows enforces it, Linux's overcommit makes it an estimate.
    if let Some(c) = s.commit {
        second.push(Clause::new(
            format!(
                "commit {} of {} GiB{}",
                words::gib_figure(c.charge),
                words::gib_total(c.limit),
                if c.exact { "" } else { ", estimate" }
            ),
            if c.near_limit().is_some() {
                inks.warn
            } else {
                inks.words2
            },
        ));
    }
    let parts = p.fit_clauses(&second, area_w, true);
    let b = crate::tray_raster::row_baseline(ly + row_h, row_h, p.small_px.get());
    let _ = p.clauses(x + pad, b, parts, true);
}

/// The Disk focus: the volume's container bar and its figures.
fn disk_focus(p: &mut Painter<'_>, rect: Rect, snap: &Snapshot) {
    let inks = *p.inks;
    let (x, y, w, _) = rect;
    let pad = p.px(6.0);
    let row_h = p.px(16.0);
    let base = crate::tray_raster::row_baseline(y + pad / 2.0, row_h, p.small_px.get());
    p.label(
        x + pad,
        base,
        "Disk \u{00B7} the tab's volume".into(),
        inks.words,
        true,
        true,
    );
    let Some(v) = snap.volume.as_ref() else {
        return;
    };
    let top = y + pad / 2.0 + row_h + p.px(6.0);
    let bh = p.px(12.0);
    let bw = w - 2.0 * pad;
    let level = if v.total > 0 {
        v.total.saturating_sub(v.available) as f32 / v.total as f32
    } else {
        0.0
    };
    let slot = (x + pad - 2.0, top - 3.0, bw + 4.0, bh + 6.0);
    p.lane(
        x + pad,
        top,
        bw,
        bh,
        Mark::still(level),
        inks.mem[0],
        slot,
        false,
    );
    let lines = [
        format!(
            "{} free of {}",
            words::disk(v.available),
            words::disk(v.total)
        ),
        format!("{} used", words::disk(v.total.saturating_sub(v.available))),
        format!("on {}", v.mount),
    ];
    for (i, line) in lines.into_iter().enumerate() {
        let ry = top + bh + p.px(4.0) + i as f32 * row_h;
        let b = crate::tray_raster::row_baseline(ry, row_h, p.small_px.get());
        let parts = p.fit_clauses(
            &[Clause::new(
                line,
                if i == 0 { inks.words } else { inks.words2 },
            )],
            bw,
            true,
        );
        let _ = p.clauses(x + pad, b, parts, true);
    }
}

/// The Jobs focus: every job in aterm's tabs that is not fine, worst first
/// (its mark, its tab, its name, the person's sentence for its verdict),
/// then the zombie findings; a calm line when there are none.
fn jobs_focus(p: &mut Painter<'_>, rect: Rect, snap: &Snapshot, input: &DeckInput<'_>) {
    use crate::stats_paint::WellMark;
    use aterm_vitals::jobs::{Verdict, wire::person_name};
    let inks = *p.inks;
    let (x, y, w, h) = rect;
    let pad = p.px(6.0);
    let row_h = p.px(16.0);
    let base = crate::tray_raster::row_baseline(y + pad / 2.0, row_h, p.small_px.get());
    p.label(x + pad, base, "Jobs".into(), inks.words, true, true);
    let tab_number = |key: u64| {
        snap.tab(key)
            .map_or_else(|| "a tab".to_string(), |t| t.label.clone())
    };
    // (mark, the arc, the line's clauses) for each job that is not fine,
    // worst first. The verdict's sentence folds to its word and age first.
    let mut lines: Vec<(WellMark, f32, Vec<Clause>)> = Vec::new();
    if let Some(view) = snap.jobs.as_deref() {
        let rank = |v: Verdict| match v {
            Verdict::Stuck => Some((0, WellMark::Stuck)),
            Verdict::Quiet => Some((2, WellMark::Quiet)),
            Verdict::WaitingYou => Some((3, WellMark::Waiting)),
            Verdict::Parked => Some((4, WellMark::Stopped)),
            _ => None,
        };
        let mut rows: Vec<_> = view
            .rows
            .iter()
            .filter_map(|r| rank(r.verdict).map(|k| (k, r)))
            .collect();
        rows.sort_by_key(|((order, _), r)| (*order, std::cmp::Reverse(r.quiet_ms.unwrap_or(0))));
        for ((_, mark), r) in rows {
            let frac = r.phi_milli.map_or(1.0, crate::stats_paint::quiet_arc);
            let short = match r.quiet_ms {
                Some(q) if matches!(r.verdict, Verdict::Stuck | Verdict::Quiet) => {
                    format!(
                        "{} {}",
                        r.verdict.word(),
                        aterm_vitals::jobs::wire::age_words(q)
                    )
                }
                _ => r.verdict.word().to_string(),
            };
            lines.push((
                mark,
                frac,
                vec![
                    Clause::new(
                        format!("{} {}", tab_number(r.tab), person_name(&r.exe)),
                        inks.words,
                    ),
                    Clause::new(r.said(), inks.words).or_short(short),
                ],
            ));
        }
        for z in view.findings.zombies.iter().filter(|z| z.tab.is_some()) {
            lines.push((
                WellMark::Zombies,
                1.0,
                vec![
                    Clause::new(
                        format!(
                            "{} {}",
                            z.tab.map_or_else(String::new, tab_number),
                            person_name(&z.name)
                        ),
                        inks.words,
                    ),
                    Clause::new(
                        format!("{} unreaped children of pid {}", z.count, z.ppid),
                        inks.words,
                    )
                    .or_short(format!("{} zombies", z.count)),
                ],
            ));
        }
        // Zombies rank second, after stuck jobs.
        lines.sort_by_key(|(mark, _, _)| *mark);
    }
    if lines.is_empty() {
        let b = crate::tray_raster::row_baseline(y + pad / 2.0 + row_h, row_h, p.small_px.get());
        let calm = if snap.jobs.is_some() || !input.marks.is_empty() {
            "Nothing stuck, quiet or waiting for you."
        } else {
            "Reading the jobs\u{2026}"
        };
        let parts = p.fit_clauses(&[Clause::new(calm, inks.words2)], w - 2.0 * pad, true);
        let _ = p.clauses(x + pad, b, parts, true);
        return;
    }
    let r = (row_h * 0.32).round();
    let room = ((h - pad - row_h) / row_h).floor().max(1.0) as usize;
    let more = lines.len().saturating_sub(room);
    let shown = if more > 0 {
        room.saturating_sub(1)
    } else {
        room
    };
    for (i, (mark, frac, line)) in lines.iter().take(shown).enumerate() {
        let ry = y + pad / 2.0 + row_h * (i as f32 + 1.0);
        p.mark(*mark, x + pad + r, (ry + row_h / 2.0).round(), r, *frac);
        let b = crate::tray_raster::row_baseline(ry, row_h, p.small_px.get());
        let tx = x + pad + 2.0 * r + p.px(6.0);
        let parts = p.fit_clauses(line, x + w - pad - tx, true);
        let _ = p.clauses(tx, b, parts, true);
    }
    if more > 0 {
        let ry = y + pad / 2.0 + row_h * (shown as f32 + 1.0);
        let b = crate::tray_raster::row_baseline(ry, row_h, p.small_px.get());
        p.label(
            x + pad,
            b,
            format!("+{} more", lines.len() - shown),
            inks.words2,
            false,
            true,
        );
    }
}

impl crate::App {
    /// The lens card's geometry for `wid`, when a lens is open and the
    /// window shows a form to hang it from.
    pub(crate) fn lens_frame(
        &self,
        wid: crate::WindowId,
    ) -> Option<(LensGeom, Lens, crate::stats_deck::DeckFrame)> {
        let lens = self.windows.get(&wid)?.stats_lens.open?;
        let f = self.deck_frame(wid)?;
        let lg = self.lens_geom_for(wid, &f)?;
        Some((lg, lens, f))
    }

    /// Where a lens would hang in `wid` from the form `f` draws; `None` when
    /// the window is too small to hold one (then no lens opens, and an open
    /// one closes: a lens with nothing on screen would swallow the arrows).
    fn lens_geom_for(
        &self,
        wid: crate::WindowId,
        f: &crate::stats_deck::DeckFrame,
    ) -> Option<LensGeom> {
        let ws = self.windows.get(&wid)?;
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid) as f32;
        let grid_w = (usize::from(ws.cols) * cw) as f32;
        let grid_h = (usize::from(ws.rows) * ch) as f32;
        let grid_y = (self.win_head(wid) + self.win_pad_top(wid)) as f32;
        lens_geom(
            f.form,
            f.dy,
            f.geom.h,
            pad,
            grid_y,
            grid_w,
            grid_h,
            f.geom.scale,
            f.geom.font_px,
        )
    }

    /// Whether `wid` can show a lens now: it draws a form and has room.
    pub(crate) fn stats_lens_fits(&self, wid: crate::WindowId) -> bool {
        self.deck_frame(wid)
            .is_some_and(|f| self.lens_geom_for(wid, &f).is_some())
    }

    /// The open lens's fingerprint for `wid` (`0` when none), folded into the
    /// stats term of the `RepaintKey`.
    pub(crate) fn stats_lens_fp(&self, wid: crate::WindowId) -> u64 {
        use std::hash::{Hash, Hasher};
        let Some((lg, lens, f)) = self.lens_frame(wid) else {
            return 0;
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.vitals.snapshot.as_ref().map(|s| s.seq).hash(&mut h);
        lens.hash(&mut h);
        (lg.dx, lg.dy, lg.w.to_bits(), lg.h.to_bits()).hash(&mut h);
        format!("{:?}", f.lens_inks).hash(&mut h);
        f.tab_key.hash(&mut h);
        f.dwc.hash(&mut h);
        // The marks come from the snapshot, whose sequence is hashed above.
        h.finish().rotate_left(17) | 1
    }

    /// Rasterize the open lens into `wid`'s `lens_card`, only when its
    /// fingerprint moved; clear it when the lens is closed.
    pub(crate) fn splice_stats_lens(&mut self, wid: crate::WindowId) {
        let Some((lg, lens, f)) = self.lens_frame(wid) else {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.lens_card = None;
                ws.stats_lens.hits.clear();
                ws.stats_lens.rect = None;
                // A lens the window can no longer show (the bars went off,
                // the window shrank) is closed, not kept invisibly.
                ws.stats_lens.open = None;
                ws.stats_lens.leave_due = None;
            }
            return;
        };
        let fp = self.stats_lens_fp(wid);
        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.lens_card.as_ref())
            .is_some_and(|c| c.fp == fp)
        {
            return;
        }
        let input = DeckInput {
            snap: self.vitals.snapshot.as_deref(),
            tab_key: f.tab_key,
            tab_label: f.tab_label.clone(),
            tab_count: f.tab_count,
            inks: lens_inks(&f),
            seam: f.seam,
            dwc: f.dwc,
            marks: self.stats_marks(),
            plate: None,
        };
        let (prims, hits) = lens_paint(&lg, &input, &lens);
        let (rgba, pw, ph) =
            crate::tray_raster::rasterize_tray(&prims, lg.w as u32, lg.h as u32, 1.0, [0, 0, 0, 0]);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        ws.stats_lens.hits = hits;
        ws.stats_lens.rect = Some((lg.dx, lg.dy, pw, ph));
        ws.lens_card = Some(crate::SettingsCard {
            scrim: None,
            rgba,
            pw,
            ph,
            dx: lg.dx,
            dy: lg.dy,
            fp,
            geom: fp,
            raster_fingerprint_once: std::sync::OnceLock::new(),
        });
    }

    /// Open the lens on `wid` at `focus` / `bar`, `how` it was asked.
    pub(crate) fn open_stats_lens(
        &mut self,
        wid: crate::WindowId,
        focus: Instrument,
        bar: Bar,
        how: Opened,
    ) {
        if !self.stats_lens_fits(wid) {
            return;
        }
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.stats_lens.open = Some(Lens { focus, bar, how });
            ws.stats_lens.rest = None;
            ws.stats_lens.leave_due = None;
        }
        // The lens reads at 2 Hz: ask for the next reading now.
        self.vitals.next_due = Some(Instant::now());
        self.request_window_redraw(wid);
    }

    /// Close `wid`'s lens (no spring: the next frame is the first without it).
    pub(crate) fn close_stats_lens(&mut self, wid: crate::WindowId) -> bool {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        let was = ws.stats_lens.open.take().is_some();
        ws.stats_lens.leave_due = None;
        ws.stats_lens.rest = None;
        if was {
            self.request_window_redraw(wid);
        }
        was
    }

    /// ⌘I (`ToggleStatsLens`): open the keyboard lens on the front window,
    /// or close the open one.
    pub(crate) fn toggle_stats_lens(&mut self) -> bool {
        let Some(wid) = self
            .frontmost_window
            .or_else(|| self.windows.keys().next().copied())
        else {
            return false;
        };
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.stats_lens.open.is_some())
        {
            self.close_stats_lens(wid);
            return true;
        }
        if !self.stats_lens_fits(wid) {
            return false;
        }
        self.open_stats_lens(wid, Instrument::Cores, Bar::Global, Opened::Keyboard);
        true
    }

    /// Whether any window has a lens open (the vitals cadence quickens).
    pub(crate) fn any_stats_lens_open(&self) -> bool {
        self.windows.values().any(|ws| ws.stats_lens.open.is_some())
    }

    /// The pointer in `wid`'s lens card pixels, while it is over the lens.
    fn lens_card_point(&self, wid: crate::WindowId) -> Option<(f64, f64)> {
        let ws = self.windows.get(&wid)?;
        let (dx, dy, w, h) = ws.stats_lens.rect?;
        ws.stats_lens.open?;
        let (x, y) = ws.last_cursor_px;
        let (ox, oy) = self.frame_origin(wid);
        let (fx, fy) = (x - ox as f64 - f64::from(dx), y - oy as f64 - f64::from(dy));
        ((0.0..f64::from(w)).contains(&fx) && (0.0..f64::from(h)).contains(&fy)).then_some((fx, fy))
    }

    /// Pointer motion over the stats surfaces: arm the rest over an
    /// instrument, move the open lens's focus with the pointer, start the
    /// hover lens's leave. True when the stats surfaces own the pointer (the
    /// Deck's band, a sill instrument, the open lens), so the motion never
    /// reaches the pane.
    pub(crate) fn stats_pointer_motion(&mut self, wid: crate::WindowId, now: Instant) -> bool {
        if self.windows.get(&wid).is_none_or(|ws| ws.overlay_open()) {
            return false;
        }
        // Over the open lens: hovering an instrument moves the focus well.
        if let Some((lx, ly)) = self.lens_card_point(wid) {
            let hit = self.windows.get(&wid).and_then(|ws| {
                ws.stats_lens
                    .hits
                    .iter()
                    .rev()
                    .find(|h| crate::stats_paint::contains(h.rect, lx, ly))
                    .copied()
            });
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.stats_lens.leave_due = None;
                if let (Some(open), Some(hit)) = (ws.stats_lens.open.as_mut(), hit)
                    && (open.focus != hit.instrument || open.bar != hit.bar)
                {
                    open.focus = hit.instrument;
                    open.bar = hit.bar;
                    if let Some(w) = ws.os_window.as_ref() {
                        w.request_redraw();
                    }
                }
            }
            return true;
        }
        let form = self.stats_bars_form();
        let hit = self.stats_hit_under_pointer(wid);
        let over_card = self.stats_card_point(wid).is_some();
        let owns = match form {
            StatsBars::Deck => over_card,
            StatsBars::Sill => hit.is_some(),
            StatsBars::Off => false,
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return owns;
        };
        match hit {
            Some(hit) => {
                ws.stats_lens.leave_due = None;
                match ws.stats_lens.open.as_mut() {
                    // An open lens follows the pointer across the form.
                    Some(open) => {
                        if open.focus != hit.instrument || open.bar != hit.bar {
                            open.focus = hit.instrument;
                            open.bar = hit.bar;
                            if let Some(w) = ws.os_window.as_ref() {
                                w.request_redraw();
                            }
                        }
                    }
                    // Re-armed at each motion: the hand pays for the lens.
                    None => {
                        ws.stats_lens.rest = Some(Rest {
                            due: now + REST,
                            hit,
                        });
                    }
                }
            }
            None => {
                ws.stats_lens.rest = None;
                if ws.stats_lens.open.is_some_and(|l| l.how == Opened::Hover)
                    && ws.stats_lens.leave_due.is_none()
                    && !over_card
                {
                    ws.stats_lens.leave_due = Some(now + LEAVE);
                }
            }
        }
        owns
    }

    /// The pointer left the window: no rest can complete, and a hover lens
    /// starts its leave.
    pub(crate) fn stats_pointer_left(&mut self, wid: crate::WindowId, now: Instant) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.stats_lens.rest = None;
            if ws.stats_lens.open.is_some_and(|l| l.how == Opened::Hover) {
                ws.stats_lens.leave_due = Some(now + LEAVE);
            }
        }
    }

    /// A press: on an instrument it pins the lens there (true: consumed);
    /// outside an open lens it closes it (false: the press goes on).
    pub(crate) fn stats_press(&mut self, wid: crate::WindowId) -> bool {
        if self.windows.get(&wid).is_none_or(|ws| ws.overlay_open()) {
            return false;
        }
        if self.lens_card_point(wid).is_some() {
            if let Some(ws) = self.windows.get_mut(&wid)
                && let Some(open) = ws.stats_lens.open.as_mut()
                && open.how == Opened::Hover
            {
                open.how = Opened::Pinned;
                if let Some(w) = ws.os_window.as_ref() {
                    w.request_redraw();
                }
            }
            return true;
        }
        let form = self.stats_bars_form();
        let hit = self.stats_hit_under_pointer(wid);
        let over_card = self.stats_card_point(wid).is_some();
        if let Some(hit) = hit {
            self.open_stats_lens(wid, hit.instrument, hit.bar, Opened::Pinned);
            return true;
        }
        self.close_stats_lens(wid);
        form == StatsBars::Deck && over_card
    }

    /// Whether the stats surfaces own a wheel or release at the pointer.
    pub(crate) fn stats_owns_pointer(&self, wid: crate::WindowId) -> bool {
        if self.windows.get(&wid).is_none_or(|ws| ws.overlay_open()) {
            return false;
        }
        if self.lens_card_point(wid).is_some() {
            return true;
        }
        match self.stats_bars_form() {
            StatsBars::Deck => self.stats_card_point(wid).is_some(),
            StatsBars::Sill => self.stats_hit_under_pointer(wid).is_some(),
            StatsBars::Off => false,
        }
    }

    /// The lens's keys: the keyboard lens takes ←/→/↑/↓, Return, ⌘C and Esc
    /// (true: consumed); any other key closes any lens and passes through
    /// (false). A bare modifier is inert.
    pub(crate) fn stats_lens_key(
        &mut self,
        wid: crate::WindowId,
        mods: aterm_window::keyboard::ModifiersState,
        key: &aterm_window::keyboard::Key,
        inert: bool,
    ) -> bool {
        use aterm_window::keyboard::{Key, NamedKey};
        let Some(open) = self.windows.get(&wid).and_then(|ws| ws.stats_lens.open) else {
            return false;
        };
        if inert {
            return false;
        }
        if open.how == Opened::Keyboard {
            let step =
                |i: usize, n: usize, fwd: bool| if fwd { (i + 1) % n } else { (i + n - 1) % n };
            let fi = Instrument::ALL
                .iter()
                .position(|i| *i == open.focus)
                .unwrap_or(0);
            let bi = Bar::ALL.iter().position(|b| *b == open.bar).unwrap_or(0);
            let next = match key {
                Key::Named(NamedKey::ArrowRight) => Some(Lens {
                    focus: Instrument::ALL[step(fi, Instrument::ALL.len(), true)],
                    ..open
                }),
                Key::Named(NamedKey::ArrowLeft) => Some(Lens {
                    focus: Instrument::ALL[step(fi, Instrument::ALL.len(), false)],
                    ..open
                }),
                Key::Named(NamedKey::ArrowDown) => Some(Lens {
                    bar: Bar::ALL[step(bi, Bar::ALL.len(), true)],
                    ..open
                }),
                Key::Named(NamedKey::ArrowUp) => Some(Lens {
                    bar: Bar::ALL[step(bi, Bar::ALL.len(), false)],
                    ..open
                }),
                Key::Named(NamedKey::Enter) => Some(Lens {
                    how: Opened::Pinned,
                    ..open
                }),
                _ => None,
            };
            if let Some(next) = next {
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.stats_lens.open = Some(next);
                }
                self.request_window_redraw(wid);
                return true;
            }
            if matches!(key, Key::Named(NamedKey::Escape)) {
                self.close_stats_lens(wid);
                return true;
            }
            if mods.super_key()
                && let Key::Character(c) = key
                && c.eq_ignore_ascii_case("c")
            {
                let _ = self.copy_stats_sentence(wid);
                return true;
            }
        }
        if open.how == Opened::Pinned && matches!(key, Key::Named(NamedKey::Escape)) {
            self.close_stats_lens(wid);
            return true;
        }
        // Any other key closes the lens and goes on to the terminal.
        self.close_stats_lens(wid);
        false
    }

    /// Copy the open lens's selected bar sentence (⌘C in the keyboard lens).
    pub(crate) fn copy_stats_sentence(&self, wid: crate::WindowId) -> bool {
        let Some(open) = self.windows.get(&wid).and_then(|ws| ws.stats_lens.open) else {
            return false;
        };
        let Some(snap) = self.vitals.snapshot.as_deref() else {
            return false;
        };
        let (tab_key, tab_label) = self.deck_tab(wid);
        let text = sentence(snap, open.bar, tab_key, &tab_label);
        crate::control::pbcopy(&text)
    }

    /// The soonest lens deadline: a rest that opens it, a leave that closes
    /// it. `None` when neither is armed.
    pub(crate) fn stats_lens_deadline(&self) -> Option<Instant> {
        self.windows
            .values()
            .flat_map(|ws| [ws.stats_lens.rest.map(|r| r.due), ws.stats_lens.leave_due])
            .flatten()
            .min()
    }

    /// The `StatsLens` deadline's work, judged every wake: open a lens whose
    /// rest completed with the pointer still on its instrument, close a hover
    /// lens whose leave ran out.
    pub(crate) fn service_stats_lens(&mut self, now: Instant) {
        // Nothing armed or nothing due (the steady case): no work, and no
        // allocation on a wake that is not this deadline's.
        if self.stats_lens_deadline().is_none_or(|due| due > now) {
            return;
        }
        let wids: Vec<crate::WindowId> = self.windows.keys().copied().collect();
        for wid in wids {
            let (rest, leave) = match self.windows.get(&wid) {
                Some(ws) => (ws.stats_lens.rest, ws.stats_lens.leave_due),
                None => continue,
            };
            if let Some(rest) = rest
                && rest.due <= now
            {
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.stats_lens.rest = None;
                }
                let still = self
                    .stats_hit_under_pointer(wid)
                    .is_some_and(|h| h.instrument == rest.hit.instrument && h.bar == rest.hit.bar);
                if still {
                    self.open_stats_lens(wid, rest.hit.instrument, rest.hit.bar, Opened::Hover);
                }
            }
            if let Some(due) = leave
                && due <= now
            {
                self.close_stats_lens(wid);
            }
        }
    }

    /// Ask `wid` for a frame.
    fn request_window_redraw(&self, wid: crate::WindowId) {
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
    }
}

/// The lens's inks: solved on the Deck's band ground for either form, so the
/// words keep their floors over any terminal theme (the sill's own inks are
/// solved on the terminal background, which the lens does not sit on).
fn lens_inks(f: &crate::stats_deck::DeckFrame) -> VitalsInks {
    f.lens_inks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats_deck::tests::{input, snapshot};
    use aterm_vitals::Motion;
    use aterm_vitals::trend::TrendView;

    fn geom() -> LensGeom {
        lens_geom(
            StatsBars::Deck,
            80,
            124.0,
            24.0,
            208.0,
            2190.0,
            1652.0,
            2.0,
            26.0,
        )
        .expect("fits")
    }

    #[test]
    fn the_lens_hangs_under_the_deck_and_over_the_sill() {
        let deck = geom();
        // Shell 2,048 × 500 at 2×, 8 px under the Deck's foot (80 + 124).
        assert_eq!(deck.w - 2.0 * deck.margin, 2048.0);
        assert_eq!(deck.h - 2.0 * deck.margin, 500.0);
        assert_eq!(deck.dy as f32 + deck.margin, 80.0 + 124.0 + 8.0);
        let sill = lens_geom(
            StatsBars::Sill,
            1876,
            24.0,
            24.0,
            84.0,
            2190.0,
            1792.0,
            2.0,
            26.0,
        )
        .expect("fits");
        assert_eq!(sill.dy as f32 + sill.margin + 500.0, 1876.0 - 8.0);
        assert!(
            lens_geom(
                StatsBars::Deck,
                0,
                124.0,
                24.0,
                124.0,
                200.0,
                100.0,
                2.0,
                26.0
            )
            .is_none()
        );
    }

    #[test]
    fn the_lens_draws_the_three_wells_and_the_focus() {
        let snap = snapshot();
        let i = input(&snap);
        for focus in Instrument::ALL {
            let lens = Lens {
                focus,
                bar: Bar::Global,
                how: Opened::Keyboard,
            };
            let (prims, hits) = lens_paint(&geom(), &i, &lens);
            let texts: Vec<&str> = prims
                .iter()
                .filter_map(|p| match p {
                    DrawPrim::Text { s, .. } => Some(s.as_str()),
                    _ => None,
                })
                .collect();
            for bar in Bar::ALL {
                assert!(hits.iter().any(|h| h.bar == bar), "{focus:?} {bar:?}");
            }
            assert!(
                texts.iter().any(|t| t.starts_with(focus.word())),
                "{focus:?}: {texts:?}"
            );
        }
    }

    #[test]
    fn the_cores_focus_draws_a_minute_per_core_and_the_busiest() {
        let mut snap = snapshot();
        if let Some(cpu) = snap.cpu.as_mut() {
            cpu.core_history = vec![vec![Some(0.5); 60]; 10];
        }
        snap.top = vec![aterm_vitals::model::TopProc {
            name: "cargo".into(),
            cores: 2.1,
            footprint: None,
            scope: ProcScope::Tab(7),
        }];
        let i = input(&snap);
        let lens = Lens {
            focus: Instrument::Cores,
            bar: Bar::Global,
            how: Opened::Hover,
        };
        let (prims, _) = lens_paint(&geom(), &i, &lens);
        let half = rgba(i.inks.load_at(0.5), 0xFF);
        let cells = prims
            .iter()
            .filter(|p| matches!(p, DrawPrim::Panel { fill, .. } if *fill == half))
            .count();
        assert!(cells >= 600, "{cells} cells for 10 cores × 60 s");
        let texts: Vec<&str> = prims
            .iter()
            .filter_map(|p| match p {
                DrawPrim::Text { s, .. } => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert!(texts.contains(&"cargo"), "{texts:?}");
    }

    #[test]
    fn the_narrow_lens_shows_the_selected_bar_over_the_focus() {
        let snap = snapshot();
        let i = input(&snap);
        let lg = lens_geom(
            StatsBars::Deck,
            80,
            124.0,
            24.0,
            208.0,
            900.0,
            1000.0,
            2.0,
            26.0,
        )
        .expect("fits");
        let lens = Lens {
            focus: Instrument::Memory,
            bar: Bar::Tab,
            how: Opened::Keyboard,
        };
        let (_, hits) = lens_paint(&lg, &i, &lens);
        // The body holds the selected bar alone; the header's switch offers
        // the three bars, one region each, keeping the focus.
        let body = lg.margin + lg.scale * 10.0 + lg.scale * 20.0;
        let (switch, wells): (Vec<&Hit>, Vec<&Hit>) = hits.iter().partition(|h| h.rect.1 < body);
        assert!(wells.iter().all(|h| h.bar == Bar::Tab), "{wells:?}");
        assert_eq!(switch.len(), 3, "{switch:?}");
        assert!(switch.iter().all(|h| h.instrument == Instrument::Memory));
        for bar in Bar::ALL {
            assert!(switch.iter().any(|h| h.bar == bar), "{bar:?}");
        }
        // The wide lens needs no switch: its three wells are all there.
        let (_, wide_hits) = lens_paint(&geom(), &i, &lens);
        let header = geom().margin + 2.0 * 10.0 + 2.0 * 20.0;
        assert!(
            wide_hits.iter().all(|h| h.rect.1 >= header),
            "{wide_hits:?}"
        );
    }

    /// The Jobs focus lists what is not fine, worst first (stuck, then the
    /// zombies a tab holds, then quiet), each line its tab, its program and
    /// the engine's sentence; past its room it counts the rest. A calm view
    /// says so, and before the first pass it says it is reading.
    #[test]
    fn the_jobs_focus_lists_the_engines_rows_worst_first() {
        use crate::stats_paint::tests::{job_row, zombies};
        use aterm_vitals::jobs::{Findings, JobsView, Verdict};
        let lens = Lens {
            focus: Instrument::Jobs,
            bar: Bar::Global,
            how: Opened::Keyboard,
        };
        let texts = |snap: &Snapshot| -> Vec<String> {
            let mut i = input(snap);
            i.marks = snap
                .jobs
                .as_deref()
                .map(crate::stats_paint::marks_of)
                .unwrap_or_default();
            let (prims, _) = lens_paint(&geom(), &i, &lens);
            prims
                .iter()
                .filter_map(|p| match p {
                    DrawPrim::Text { s, .. } => Some(s.clone()),
                    _ => None,
                })
                .collect()
        };
        let mut snap = snapshot();
        let t = texts(&snap);
        assert!(t.iter().any(|s| s.starts_with("Reading the jobs")), "{t:?}");
        snap.jobs = Some(std::sync::Arc::new(JobsView {
            rows: vec![job_row("j1", Verdict::Fine, None, None)],
            ..Default::default()
        }));
        let t = texts(&snap);
        assert!(
            t.iter()
                .any(|s| s == "Nothing stuck, quiet or waiting for you."),
            "{t:?}"
        );
        snap.jobs = Some(std::sync::Arc::new(JobsView {
            rows: vec![
                job_row("j1", Verdict::Quiet, Some(4_000), Some(30_000)),
                job_row("j2", Verdict::Fine, None, None),
                job_row("j3", Verdict::Stuck, Some(9_000), Some(90_000)),
            ],
            findings: Findings {
                zombies: vec![zombies(300, 3, Some(7)), zombies(1, 9, None)],
                ..Default::default()
            },
            ..Default::default()
        }));
        let t = texts(&snap);
        // Each line: "<tab> <program>", the separator, the sentence.
        let lines: Vec<(&str, &str)> = t
            .iter()
            .enumerate()
            .filter(|(_, s)| s.starts_with("Tab 1 "))
            .filter_map(|(i, s)| t.get(i + 2).map(|said| (s.as_str(), said.as_str())))
            .collect();
        assert_eq!(
            lines.len(),
            3,
            "the fine job and the zombies outside aterm draw no line: {t:?}"
        );
        assert_eq!(lines[0].0, "Tab 1 make");
        assert!(lines[0].1.starts_with("stuck"), "{lines:?}");
        assert_eq!(lines[1].0, "Tab 1 node");
        assert!(lines[1].1.starts_with('3'), "{lines:?}");
        assert_eq!(lines[2].0, "Tab 1 make");
        assert!(lines[2].1.starts_with("quiet"), "{lines:?}");

        // More jobs than lines: the last line counts the rest.
        snap.jobs = Some(std::sync::Arc::new(JobsView {
            rows: (0..40)
                .map(|n| job_row(&format!("j{n}"), Verdict::Stuck, Some(9_000), Some(90_000)))
                .collect(),
            ..Default::default()
        }));
        let t = texts(&snap);
        let shown = t.iter().filter(|s| s.starts_with("Tab 1 ")).count();
        let more = format!("+{} more", 40 - shown);
        assert!(shown > 0 && shown < 40, "{shown}");
        assert!(t.iter().any(|s| s.starts_with(&more)), "{more}: {t:?}");
    }

    #[test]
    fn the_readouts_carry_their_instruments_figures() {
        let snap = snapshot();
        let cpu = snap.cpu.as_ref().expect("cpu");
        let cores = readout(&snap, Instrument::Cores);
        assert!(cores.contains(&words::cores(cpu.busy.level)), "{cores}");
        assert!(cores.contains(&cpu.total().to_string()), "{cores}");
        let mem = snap.mem.as_ref().expect("memory");
        let memory = readout(&snap, Instrument::Memory);
        assert!(
            memory.contains(&words::gib_total(mem.sample.total)),
            "{memory}"
        );
        let gpu = snap.gpu.as_ref().expect("gpu");
        let g = readout(&snap, Instrument::Gpu);
        assert!(
            g.contains(&format!("{:.0}%", (gpu.load.raw * 100.0).round())),
            "{g}"
        );
        let vol = snap.volume.as_ref().expect("volume");
        let disk = readout(&snap, Instrument::Disk);
        assert!(disk.contains(&words::disk(vol.available)), "{disk}");
        // Before a reading, each says so rather than inventing a figure.
        let empty = Snapshot::default();
        for focus in Instrument::ALL {
            assert!(
                !readout(&empty, focus).chars().any(|c| c.is_ascii_digit()),
                "{focus:?}"
            );
        }
    }

    #[test]
    fn a_sentence_folds_at_its_clauses() {
        let c = sentence_clauses(
            "Global: 3.1 of 10 cores busy; GPU 4%; disk 1.81 TB free.",
            [0, 0, 0],
        );
        assert_eq!(c.len(), 3);
        assert_eq!(c[2].full, "disk 1.81 TB free");
    }

    #[test]
    fn motion_is_named_in_the_cores_readout() {
        let mut snap = snapshot();
        if let Some(cpu) = snap.cpu.as_mut() {
            cpu.busy = TrendView {
                level: 4.0,
                slow: 2.0,
                raw: 4.0,
                ..TrendView::flat(4.0)
            };
        }
        let r = readout(&snap, Instrument::Cores);
        let rising = snap
            .cpu
            .as_ref()
            .is_some_and(|c| c.busy.motion(words::CORES_GATE) == Motion::Rising);
        assert_eq!(r.contains("rising"), rising, "{r}");
    }
}

/// The lens and the sill driven through the App's own pointer, key, focus
/// and config routing, read off what the tab's program received.
#[cfg(all(test, unix))]
#[path = "stats_lens_behaviour_tests.rs"]
mod behaviour_tests;
