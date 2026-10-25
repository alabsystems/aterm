// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stats Deck instrument lens overlay ([`lens_body`], [`REST`], [`LEAVE`]).

use std::time::{Duration, Instant};

use aterm_vitals::Rates;
use aterm_vitals::explain::episodes::CLAUSE_STALE_MS;
use aterm_vitals::explain::{
    CauseId, Context, Evidence, Lead, Part, Role, Voucher, WhySubject, account_of,
};
use aterm_vitals::ink::{self, Rgb, VitalsInks};
use aterm_vitals::model::{
    CpuView, DiskFacts, LinkKind, Pressure, ProcScope, Resource, ResourceView, Snapshot,
};
use aterm_vitals::words::{self, InkKind, Ladder, Locale, Platform};

use crate::deck_face::{ASCENT_EM, DESCENT_EM, FaceMetrics, deck_metrics};
use crate::stats_deck::{DeckInput, StatsBars, kind_ink};
use crate::stats_paint::{Bar, Instrument, Painter, WellMark, a, panel};
use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, Rgba, TextFace, TextWeight, rgba, text_prim};

/// How long the pointer rests on an instrument before the hover lens opens.
pub(crate) const REST: Duration = Duration::from_millis(250);
/// How long an unpinned hover lens outlives the pointer leaving it and the form it opened on.
pub(crate) const LEAVE: Duration = Duration::from_millis(300);
/// The vitals cadence while a lens is open (2 Hz).
pub(crate) const LENS_PERIOD: Duration = Duration::from_millis(500);

/// How a lens was opened, which decides how it closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Opened {
    /// The pointer rested on an instrument.
    Hover,
    /// A click pinned it.
    Pinned,
    /// ⌘I: it takes ←/→/↑/↓, Return, ⌘C and Esc.
    Keyboard,
}

/// An open lens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Lens {
    /// The instrument it shows.
    pub(crate) focus: Instrument,
    /// The scope its lists show (↑/↓).
    pub(crate) bar: Bar,
    pub(crate) how: Opened,
}

/// A pointer rest armed over an instrument.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rest {
    /// When it opens the lens.
    pub(crate) due: Instant,
    /// What the pointer rests on.
    pub(crate) hit: crate::stats_paint::Hit,
}

/// The lens's per-window state.
#[derive(Clone, Debug, Default)]
pub(crate) struct LensSlot {
    /// The open lens.
    pub(crate) open: Option<Lens>,
    /// A rest armed over an instrument.
    pub(crate) rest: Option<Rest>,
    /// When an unpinned hover lens closes, the pointer having left.
    pub(crate) leave_due: Option<Instant>,
    /// The lens card's placement in the frame.
    pub(crate) rect: Option<(u32, u32, u32, u32)>,
    /// The instrument and scope the last lens showed.
    pub(crate) last: Option<(Instrument, Bar)>,
    /// The open lens's shell raster, kept while its prims stay the same.
    pub(crate) shell: Option<ShellRaster>,
}

/// The lens shell's raster: its shadow rings, ground and rim, which a new reading leaves as they
/// were, so the card's body is drawn over a copy of it.
#[derive(Clone)]
pub(crate) struct ShellRaster {
    /// The shell's prims and size it was rasterized from.
    fp: u64,
    rgba: std::sync::Arc<[u8]>,
    pw: u32,
    ph: u32,
}

impl std::fmt::Debug for ShellRaster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellRaster")
            .field("fp", &self.fp)
            .field("pw", &self.pw)
            .field("ph", &self.ph)
            .finish_non_exhaustive()
    }
}

/// The lens card's geometry.
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

impl LensGeom {
    /// The shell's width and height, pt.
    fn shell_pt(&self) -> (f64, f64) {
        let s = f64::from(self.scale.max(0.5));
        (
            f64::from(self.w - 2.0 * self.margin) / s,
            f64::from(self.h - 2.0 * self.margin) / s,
        )
    }
}

/// Where the lens hangs for a form card at `(card_dy, card_h)` over the grid.
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

/// What the How to read lens says to a reader, and what its ⌘C copies.
pub(crate) fn how_to_read_words(platform: Platform) -> String {
    format!(
        "Each row is one resource. The row covers the last 10 minutes, oldest on the left; each \
         cell is 10 seconds. Colour runs from cold, 0 percent of the resource's maximum, to hot, \
         100 percent. A mark above a cell means the resource ran over its maximum. An outlined \
         cell, or a short line, was not read. The bar shows {} in grey, aterm in green and this \
         tab in bright green.",
        platform.machine()
    )
}

/// The instrument ⌘C copies.
pub(crate) fn lens_sentence(
    snap: &Snapshot,
    focus: Instrument,
    panes: &[u64],
    loc: &Locale,
) -> String {
    match focus.resource() {
        Some(res) => words::row_sentence(res, snap, panes, loc),
        None => how_to_read_words(loc.platform),
    }
}

/// The next instrument ←/→ moves to over `Instrument::ALL`, wrapping.
pub(crate) fn step_instrument(focus: Instrument, forward: bool) -> Instrument {
    let all = Instrument::ALL;
    let n = all.len();
    match all.iter().position(|i| *i == focus) {
        Some(i) if forward => all[(i + 1) % n],
        Some(i) => all[(i + n - 1) % n],
        None if forward => all[0],
        None => all[n - 1],
    }
}

/// The scope ↑/↓ moves to, wrapping.
fn step_bar(bar: Bar, forward: bool) -> Bar {
    let n = Bar::ALL.len();
    let i = Bar::ALL.iter().position(|b| *b == bar).unwrap_or(0);
    Bar::ALL[if forward {
        (i + 1) % n
    } else {
        (i + n - 1) % n
    }]
}

/// The shell's side margin.
const SIDE: f64 = 12.0;
/// Two columns from this shell width.
const TWO_COLUMNS_PT: f64 = 640.0;
/// Line 1's baseline.
const LINE1_BASE: f64 = 18.0;
/// The explainer's first baseline, and the frame's line pitch.
const EXPLAINER_BASE: f64 = 34.0;
const PITCH: f64 = 13.0;
/// The gap between two stacked sections.
const GAP: f64 = 4.0;
/// No ink comes nearer the shell's bottom edge than this.
const FOOT: f64 = 10.0;
/// A one-line section.
const LINE_H: f64 = 13.0;
/// The name's size on line 1, a section heading's, the words', a note's and an axis label's.
const NAME_PT: f64 = 12.0;
const HEAD_PT: f64 = 10.5;
const BODY_PT: f64 = 10.0;
const NOTE_PT: f64 = 9.5;
const AXIS_PT: f64 = 9.0;
/// A section's heading baseline under its top, and its first row's.
const HEAD_BASE: f64 = 10.0;
const ROW0_BASE: f64 = 24.0;
/// A tolerance for measured widths.
const EPS: f64 = 0.01;

/// Where a section is laid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Place {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) w: f64,
    pub(crate) h: f64,
}

type Draw<'d> = Box<dyn Fn(&mut Pen<'_>, Place) + 'd>;

/// One form a section can take.
pub(crate) struct Form<'d> {
    h: f64,
    grow: bool,
    /// Whether it fits only as its column's last section (Cores' grow form over Why now).
    last: bool,
    /// A one-line form's label.
    label: Option<String>,
    draw: Draw<'d>,
}

fn form<'d>(h: f64, draw: impl Fn(&mut Pen<'_>, Place) + 'd) -> Form<'d> {
    Form {
        h,
        grow: false,
        last: false,
        label: None,
        draw: Box::new(draw),
    }
}

fn grow<'d>(h: f64, draw: impl Fn(&mut Pen<'_>, Place) + 'd) -> Form<'d> {
    Form {
        h,
        grow: true,
        last: false,
        label: None,
        draw: Box::new(draw),
    }
}

/// A one-line form.
fn line_form<'d>(label: &str, forms: Vec<String>) -> Form<'d> {
    let owned = label.to_owned();
    Form {
        h: LINE_H,
        grow: false,
        last: false,
        label: Some(label.to_owned()),
        draw: Box::new(move |pen, p| one_line(pen, p, &owned, &forms)),
    }
}

/// One section of a lens's body.
pub(crate) struct Section<'d> {
    pub(crate) name: String,
    pub(crate) words: Vec<String>,
    pub(crate) right: bool,
    /// The parts it draws under labels of their own (Space now's Freed).
    pub(crate) parts: Vec<(String, String)>,
    forms: Vec<Form<'d>>,
}

/// What line 1 and the explainer say, the answer under them, and the body.
pub(crate) struct LensDoc<'d> {
    name: &'static str,
    /// Where the reading comes from and when, longest first.
    sources: Vec<String>,
    explainer: String,
    /// "Wrong?" (Disk) or the row's state now.
    answer: Option<(&'static str, String, InkKind)>,
    /// In priority order.
    sections: Vec<Section<'d>>,
    /// The order two columns stack them in, top down (each column keeps its own).
    two_order: Vec<usize>,
    /// Whether the first section always shows (Disk's Space now).
    keeps_first: bool,
}

/// Whether a shell `lw` pt wide takes two columns, and each column's width.
fn columns(lw: f64) -> (bool, f64) {
    if lw >= TWO_COLUMNS_PT {
        (true, (lw - 3.0 * SIDE) / 2.0)
    } else {
        (false, lw - 2.0 * SIDE)
    }
}

/// One placed section.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Placed {
    pub(crate) section: usize,
    pub(crate) form: usize,
    pub(crate) place: Place,
}

/// The body: on a shell `lw` pt wide, two columns of `(lw − 36)/2` from 640 pt.
pub(crate) fn lens_body(
    lw: f64,
    top: f64,
    bottom: f64,
    sections: &[Section<'_>],
    (two_order, keeps_first): (&[usize], bool),
) -> Vec<Placed> {
    let (two, cw) = columns(lw);
    let cols: Vec<(f64, Vec<usize>)> = if two {
        let order: Vec<usize> = if two_order.is_empty() {
            (0..sections.len()).collect()
        } else {
            two_order.to_vec()
        };
        let side = |right: bool| -> Vec<usize> {
            order
                .iter()
                .copied()
                .filter(|&i| sections.get(i).is_some_and(|sec| sec.right == right))
                .collect()
        };
        vec![(SIDE, side(false)), (2.0 * SIDE + cw, side(true))]
    } else {
        vec![(SIDE, (0..sections.len()).collect())]
    };
    (0..=sections.len())
        .rev()
        .find_map(|shown| {
            let mut out = Vec::new();
            for (x, ids) in &cols {
                let ids: Vec<usize> = ids.iter().copied().filter(|&i| i < shown).collect();
                out.extend(stack((*x, cw), (top, bottom), sections, &ids, keeps_first)?);
            }
            Some(out)
        })
        .unwrap_or_default()
}

/// One column at `x`, `cw` wide, holding every one of `ids` in order, a [`Form::last`]
/// one only as the last; `None` when they do not all fit.
fn stack(
    (x, cw): (f64, f64),
    (top, bottom): (f64, f64),
    sections: &[Section<'_>],
    ids: &[usize],
    keeps_first: bool,
) -> Option<Vec<Placed>> {
    let mut out = Vec::with_capacity(ids.len());
    let mut y = top;
    for (k, &i) in ids.iter().enumerate() {
        let after: f64 = if keeps_first && i == 0 {
            0.0
        } else {
            ids[k + 1..]
                .iter()
                .map(|&j| {
                    sections[j]
                        .forms
                        .last()
                        .map_or(f64::INFINITY, |f| GAP + f.h)
                })
                .sum()
        };
        let room = bottom - y;
        let fi = sections[i]
            .forms
            .iter()
            .position(|f| (!f.last || k + 1 == ids.len()) && f.h + after <= room + EPS)?;
        let f = &sections[i].forms[fi];
        let h = if f.grow {
            (room - after).max(f.h).min(room)
        } else {
            f.h
        };
        out.push(Placed {
            section: i,
            form: fi,
            place: Place { x, y, w: cw, h },
        });
        y += h + GAP;
    }
    Some(out)
}

/// A drawn text's box (advance, ascent, descent), pt from the shell's corner.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TextBox {
    pub(crate) text: String,
    pub(crate) r: [f64; 4],
}

pub(crate) struct Pen<'a> {
    p: Painter<'a>,
    m: &'a FaceMetrics,
    inks: VitalsInks,
    /// Increase Contrast.
    ic: bool,
    s: f64,
    /// The shell's corner in the card, device px.
    ox: f32,
    oy: f32,
    /// Draw the prims, or only lay out.
    draw: bool,
    /// Where the one-liners stacked in this section's column start their words.
    line_tab: f64,
    #[cfg(test)]
    boxes: Vec<TextBox>,
}

impl<'a> Pen<'a> {
    /// `v` pt in whole device px.
    fn px(&self, v: f64) -> f32 {
        (v * self.s + 0.5).floor() as f32
    }

    fn width(&self, s: &str, pt: f64, bold: bool) -> f64 {
        self.m.width(s, pt, bold)
    }

    /// `s` cut to `room` with "…" (rule F).
    fn cut(&self, s: &str, room: f64, pt: f64, bold: bool) -> String {
        self.m.fit(&[s], room, pt, bold)
    }

    /// `s` at `(x, base)`.
    fn text(&mut self, x: f64, base: f64, s: &str, pt: f64, bold: bool, ink: Rgb) -> f64 {
        if s.is_empty() {
            return 0.0;
        }
        let w = self.width(s, pt, bold);
        #[cfg(test)]
        self.boxes.push(TextBox {
            text: s.to_owned(),
            r: [x, base - ASCENT_EM * pt, x + w, base + DESCENT_EM * pt],
        });
        if self.draw {
            let px = (pt * self.s) as f32;
            let at = (self.ox + self.px(x), self.oy + self.px(base));
            self.p.prims.push(text_prim(
                at.0,
                at.1,
                s.to_owned(),
                TypeStep::Caption.px_clamped(px, px, px),
                TextWeight::Regular,
                if bold { TextFace::UiBold } else { TextFace::Ui },
                rgba(ink, 0xFF),
            ));
        }
        w
    }

    /// `s` ending at `right`.
    fn text_end(&mut self, right: f64, base: f64, s: &str, pt: f64, bold: bool, ink: Rgb) -> f64 {
        let w = self.width(s, pt, bold);
        self.text(right - w, base, s, pt, bold, ink)
    }

    /// A filled rect from pt edges.
    fn rect(&mut self, x: f64, y: f64, w: f64, h: f64, radius: f64, ink: Rgba) {
        if !self.draw {
            return;
        }
        let (x0, y0, x1, y1) = (self.px(x), self.px(y), self.px(x + w), self.px(y + h));
        if x1 > x0 && y1 > y0 {
            let r = ((radius * self.s) as f32).min((x1 - x0) / 2.0);
            self.p
                .prims
                .push(panel(self.ox + x0, self.oy + y0, x1 - x0, y1 - y0, r, ink));
        }
    }

    /// A hairline rule one device px tall from `x` over `w` at `y`.
    fn rule(&mut self, x: f64, y: f64, w: f64, ink: Rgba) {
        if !self.draw {
            return;
        }
        let (x0, x1, y0) = (self.px(x), self.px(x + w), self.px(y));
        if x1 > x0 {
            self.p
                .prims
                .push(panel(self.ox + x0, self.oy + y0, x1 - x0, 1.0, 0.0, ink));
        }
    }

    /// A 1 pt vertical tick at `x` from `y0` to `y1`.
    fn tick(&mut self, x: f64, y0: f64, y1: f64, ink: Rgba) {
        if !self.draw {
            return;
        }
        let lw = self.s.round().max(1.0) as f32;
        let (a, b) = (self.px(y0), self.px(y1));
        if b > a {
            self.p.prims.push(panel(
                self.ox + self.px(x) - (lw / 2.0).floor(),
                self.oy + a,
                lw,
                b - a,
                0.0,
                ink,
            ));
        }
    }

    /// A 1 pt segment.
    fn line(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, ink: Rgba) {
        if !self.draw {
            return;
        }
        let s = self.s as f32;
        self.p.prims.push(DrawPrim::Line {
            x1: self.ox + (x1 * self.s) as f32,
            y1: self.oy + (y1 * self.s) as f32,
            x2: self.ox + (x2 * self.s) as f32,
            y2: self.oy + (y2 * self.s) as f32,
            width: s,
            color: ink,
        });
    }

    /// An outlined rect, one device px wide.
    fn hollow(&mut self, x: f64, y: f64, w: f64, h: f64, ink: Rgba) {
        if !self.draw {
            return;
        }
        let (x0, y0, x1, y1) = (self.px(x), self.px(y), self.px(x + w), self.px(y + h));
        if x1 > x0 && y1 > y0 {
            self.p.prims.push(DrawPrim::Stroke {
                x: self.ox + x0,
                y: self.oy + y0,
                w: x1 - x0,
                h: y1 - y0,
                radius: 0.0,
                width: 1.0,
                color: ink,
            });
        }
    }

    /// A mark `size` pt across centred at `(cx, cy)` (§10).
    fn mark(&mut self, kind: WellMark, cx: f64, cy: f64, size: f64, frac: f32, ink: Rgb) {
        if !self.draw {
            return;
        }
        let r = 0.36 * size;
        let lw = (0.9 * self.s).round().max(1.0) as f32;
        let s = self.s;
        let (ox, oy) = (self.ox, self.oy);
        self.p.mark_in(
            kind,
            (ox + (cx * s) as f32, oy + (cy * s) as f32, (r * s) as f32),
            frac,
            ink,
            lw,
        );
    }

    /// ←→ (`across`) or ↑↓ drawn as Lines, the faces lacking the glyphs.
    fn arrows(&mut self, x: f64, base: f64, across: bool) -> f64 {
        const L: f64 = 3.4;
        const HW: f64 = 1.8;
        let ink = rgba(self.inks.words2, 0xFF);
        if across {
            let yc = base - 3.0;
            for (xc, dir) in [(x + 4.0, -1.0), (x + 13.0, 1.0)] {
                let tip = xc + dir * L;
                self.line(xc - L, yc, xc + L, yc, ink);
                self.line(tip, yc, tip - dir * HW, yc - HW, ink);
                self.line(tip, yc, tip - dir * HW, yc + HW, ink);
            }
            20.0
        } else {
            let yc = base - 3.4;
            for (xc, dir) in [(x + 3.0, -1.0), (x + 10.0, 1.0)] {
                let tip = yc + dir * L;
                self.line(xc, yc - L, xc, yc + L, ink);
                self.line(xc, tip, xc - HW, tip - dir * HW, ink);
                self.line(xc, tip, xc + HW, tip - dir * HW, ink);
            }
            16.0
        }
    }

    /// Heat step `step`'s ink.
    fn heat(&self, v: f32) -> Rgb {
        let step = usize::from(ink::heat_step(v.max(0.0)).min(6));
        if self.ic {
            self.inks.heat_ic[step]
        } else {
            self.inks.heat[step]
        }
    }

    /// The machine's ink, `machine_ic` under Increase Contrast.
    const fn machine(&self) -> Rgb {
        if self.ic {
            self.inks.machine_ic
        } else {
            self.inks.machine
        }
    }

    /// The shell: a soft shadow laid as rings, the opaque ground and the rim.
    fn shell(&mut self, lg: &LensGeom) {
        let inks = self.inks;
        let m = lg.margin;
        let (sx, sy, sw, sh) = (m, m, lg.w - 2.0 * m, lg.h - 2.0 * m);
        let radius = self.px(8.0);
        let rings = m as usize;
        for i in (1..=rings).rev() {
            let d = i as f32;
            let k = 0.18 * (1.0 - d / (m + 1.0)).powi(2) / rings as f32 * 2.5;
            self.p.prims.push(panel(
                sx - d,
                sy - d + (d * 0.4).round(),
                sw + 2.0 * d,
                sh + 2.0 * d,
                radius + d,
                rgba([0, 0, 0], (k * 255.0).round() as u8),
            ));
        }
        let shell = ink::mix(inks.ground, inks.words, 0.04);
        self.p
            .prims
            .push(panel(sx, sy, sw, sh, radius, rgba(shell, 0xFF)));
        self.p.prims.push(DrawPrim::Stroke {
            x: sx,
            y: sy,
            w: sw,
            h: sh,
            radius,
            width: 1.0,
            color: a(inks.rim, 0.9),
        });
    }
}

/// `text` wrapped to `width` where the shared rule lets a line break ([`aterm_grapheme::pieces`]).
fn wrap(m: &FaceMetrics, text: &str, width: f64, pt: f64) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for piece in aterm_grapheme::pieces(text, aterm_grapheme::BreakKind::is_word_boundary) {
        let word = piece.text;
        let t = if line.is_empty() {
            word.to_owned()
        } else if piece.spaced {
            format!("{line} {word}")
        } else {
            format!("{line}{word}")
        };
        if line.is_empty() || m.width(&t, pt, false) <= width + EPS {
            line = t;
        } else {
            out.push(std::mem::replace(&mut line, word.to_owned()));
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    out.into_iter()
        .map(|l| {
            if m.width(&l, pt, false) <= width + EPS {
                l
            } else {
                m.fit(&[&l], width, pt, false)
            }
        })
        .filter(|l| !l.is_empty())
        .collect()
}

/// The first of `forms` that fits `room`.
fn first_fit(m: &FaceMetrics, forms: &[String], room: f64, pt: f64, bold: bool) -> Option<String> {
    forms
        .iter()
        .find(|f| m.width(f, pt, bold) <= room + EPS)
        .cloned()
}

/// Rule F: the first of `forms` that fits, else the last cut with "…".
fn fit_cut(m: &FaceMetrics, forms: &[String], room: f64, pt: f64, bold: bool) -> String {
    let refs: Vec<&str> = forms.iter().map(String::as_str).collect();
    m.fit(&refs, room, pt, bold)
}

/// A section heading at `p`'s top, with the first of `subs` that fits beside it.
fn heading(pen: &mut Pen<'_>, p: Place, name: &str, subs: &[String]) {
    let base = p.y + HEAD_BASE;
    let name = pen.cut(name, p.w, HEAD_PT, true);
    let ink = pen.inks.words;
    let nw = pen.text(p.x, base, &name, HEAD_PT, true, ink);
    let room = p.w - nw - 8.0;
    if let Some(sub) = first_fit(pen.m, subs, room, NOTE_PT, false) {
        let ink = pen.inks.words2;
        pen.text(p.x + nw + 8.0, base, &sub, NOTE_PT, false, ink);
    }
}

/// Where a one-line section's words start after its label.
fn tab_after(label_w: f64) -> f64 {
    [44.0, 52.0, 64.0]
        .into_iter()
        .find(|t| label_w + 8.0 <= *t)
        .unwrap_or(label_w + 8.0)
}

/// A one-line form.
fn one_line(pen: &mut Pen<'_>, p: Place, label: &str, forms: &[String]) {
    let base = p.y + HEAD_BASE;
    let label = pen.cut(label, p.w, HEAD_PT, true);
    let ink = pen.inks.words;
    let lw = pen.text(p.x, base, &label, HEAD_PT, true, ink);
    let tab = tab_after(lw).max(pen.line_tab);
    let s = fit_cut(pen.m, forms, p.w - tab, BODY_PT, false);
    let ink = pen.inks.words2;
    pen.text(p.x + tab, base, &s, BODY_PT, false, ink);
}

/// A section that is one line in every form.
fn line_section<'d>(name: &str, label: &str, forms: Vec<String>, right: bool) -> Section<'d> {
    let words = forms.first().cloned().into_iter().collect();
    Section {
        name: name.to_owned(),
        words,
        right,
        parts: Vec::new(),
        forms: vec![line_form(label, forms)],
    }
}

/// One row of a list.
#[derive(Clone, Debug, PartialEq)]
struct Row {
    /// Whose it is: the chip's ink.
    chip: Option<ProcScope>,
    name: String,
    scope: Option<String>,
    /// The bar's share, `0..=1`.
    share: Option<f64>,
    figure: String,
}

impl Row {
    fn said(&self) -> String {
        match &self.scope {
            Some(scope) => format!("{} in {scope}, {}", self.name, self.figure),
            None => format!("{}, {}", self.name, self.figure),
        }
    }
}

/// Processes of one name in one scope as one row.
fn grouped<'a>(
    items: impl IntoIterator<Item = (&'a str, ProcScope, f64)>,
) -> Vec<(String, ProcScope, f64)> {
    let mut groups: Vec<(&str, ProcScope, f64, usize)> = Vec::new();
    for (name, scope, v) in items {
        match groups.iter_mut().find(|g| g.0 == name && g.1 == scope) {
            Some(g) => {
                g.2 += v;
                g.3 += 1;
            }
            None => groups.push((name, scope, v, 1)),
        }
    }
    groups.sort_by(|a, b| b.2.total_cmp(&a.2));
    groups
        .into_iter()
        .map(|(name, scope, v, n)| (program_label(name, n), scope, v))
        .collect()
}

/// A program's row name: its name as a person reads it, `×n` where it runs as several processes.
fn program_label(name: &str, n: usize) -> String {
    let name = aterm_vitals::jobs::wire::person_name(name);
    if n > 1 {
        format!("{name} \u{d7}{n}")
    } else {
        name
    }
}

/// A list's height.
fn rows_h(n: usize, note: bool) -> f64 {
    PITCH * (n + usize::from(note)) as f64 + 14.0
}

/// A chip's ink (§11.3): this tab's for a process in any of its `panes`.
fn chip_ink(pen: &Pen<'_>, scope: ProcScope, panes: &[u64]) -> Rgb {
    match scope {
        ProcScope::Tab(k) if panes.contains(&k) => {
            if pen.ic {
                pen.inks.tab_ic
            } else {
                pen.inks.tab
            }
        }
        ProcScope::Tab(_) | ProcScope::Aterm => {
            if pen.ic {
                pen.inks.aterm_ic
            } else {
                pen.inks.aterm
            }
        }
        ProcScope::Other => pen.machine(),
    }
}

/// `rows` under a heading at `p`'s top.
fn draw_rows(pen: &mut Pen<'_>, p: Place, rows: &[Row], panes: &[u64]) {
    let fig_w = rows
        .iter()
        .map(|r| pen.width(&r.figure, BODY_PT, false))
        .fold(0.0, f64::max);
    let lead = 10.0;
    let avail = (p.w - lead - fig_w - 8.0).max(0.0);
    let has_scope = rows.iter().any(|r| r.scope.is_some());
    let longest = rows
        .iter()
        .map(|r| pen.width(&r.name, BODY_PT, false))
        .fold(0.0, f64::max);
    let name_w = if has_scope || rows.iter().any(|r| r.share.is_some()) {
        (longest + 10.0).clamp(60.0, 160.0).min(avail * 0.5)
    } else {
        avail
    };
    let scope_w = if has_scope {
        (avail * 0.1)
            .clamp(36.0, 52.0)
            .min((avail - name_w - 4.0).max(0.0))
    } else {
        0.0
    };
    let bar_x = p.x + lead + name_w + 4.0 + if has_scope { scope_w + 4.0 } else { 0.0 };
    let bar_w = p.x + p.w - fig_w - 8.0 - bar_x;
    let inks = pen.inks;
    for (i, r) in rows.iter().enumerate() {
        let base = p.y + ROW0_BASE + PITCH * i as f64;
        if let Some(scope) = r.chip {
            let ink = chip_ink(pen, scope, panes);
            pen.rect(p.x, base - 7.0, 6.0, 7.0, 1.0, rgba(ink, 0xFF));
        }
        let name = pen.cut(&r.name, name_w, BODY_PT, false);
        pen.text(p.x + lead, base, &name, BODY_PT, false, inks.words);
        if let Some(scope) = &r.scope {
            let scope = pen.cut(scope, scope_w, BODY_PT, false);
            pen.text(
                p.x + lead + name_w + 4.0,
                base,
                &scope,
                BODY_PT,
                false,
                inks.words2,
            );
        }
        if bar_w >= 24.0
            && let Some(share) = r.share
        {
            pen.rect(bar_x, base - 6.5, bar_w, 6.0, 1.0, rgba(inks.track, 0xFF));
            let fill = (bar_w * share.clamp(0.0, 1.0)).max(2.0 / pen.s);
            let ink = pen.machine();
            pen.rect(bar_x, base - 6.5, fill, 6.0, 1.0, rgba(ink, 0xFF));
        }
        pen.text_end(p.x + p.w, base, &r.figure, BODY_PT, false, inks.words);
    }
}

/// A list section: its heading and subtitle, `rows`, an optional note.
#[expect(clippy::too_many_arguments, reason = "a list's words, rows and forms")]
fn list_section<'d>(
    c: Ctx<'d>,
    name: String,
    subs: Vec<String>,
    rows: Vec<Row>,
    note: Option<String>,
    most: usize,
    label: &str,
    line: Vec<String>,
    right: bool,
) -> Section<'d> {
    let mut words: Vec<String> = rows.iter().map(Row::said).collect();
    words.extend(note.clone());
    let panes = c.panes;
    let mut forms = Vec::new();
    let counts: Vec<usize> = if c.two {
        vec![rows.len().min(most)]
    } else {
        let mut v = vec![rows.len().min(most), rows.len().min(2)];
        v.dedup();
        v
    };
    for n in counts.into_iter().filter(|&n| n > 0) {
        let with_note = note.is_some() && c.two;
        let (rows, subs, name, note) =
            (rows[..n].to_vec(), subs.clone(), name.clone(), note.clone());
        forms.push(form(rows_h(n, with_note), move |pen, p| {
            heading(pen, p, &name, &subs);
            draw_rows(pen, p, &rows, panes);
            if with_note && let Some(note) = &note {
                let base = p.y + ROW0_BASE + PITCH * rows.len() as f64;
                let lead = 10.0;
                let s = pen.cut(note, p.w - lead, NOTE_PT, false);
                let ink = pen.inks.words2;
                pen.text(p.x + lead, base, &s, NOTE_PT, false, ink);
            }
        }));
    }
    forms.push(line_form(label, line));
    Section {
        name,
        words,
        right,
        parts: Vec::new(),
        forms,
    }
}

/// A step trace's data and words.
struct Trace {
    /// The values, oldest first.
    vals: Vec<Option<f64>>,
    /// The labels under the plot.
    under: &'static [(f64, &'static str)],
    /// The least span the axis shows, in the values' unit.
    min_span: f64,
    /// A rule at a fractional bucket index, and its label.
    mark: Option<(f64, String)>,
}

/// The labels under a 24 hour trace and an hour's.
const DAY: &[(f64, &str)] = &[
    (0.0, "24 h ago"),
    (0.25, "18 h"),
    (0.5, "12 h"),
    (0.75, "6 h"),
    (1.0, "now"),
];
const HOUR: &[(f64, &str)] = &[
    (0.0, "60 min ago"),
    (0.25, "45 min"),
    (0.5, "30 min"),
    (0.75, "15 min"),
    (1.0, "now"),
];

/// The smallest of 1, 2 and 5 times a power of ten at or above `v`.
fn nice_up(v: f64) -> f64 {
    let e = 10f64.powf(v.max(1e-9).log10().floor());
    for k in [1.0, 2.0, 5.0, 10.0] {
        if k * e >= v * (1.0 - 1e-9) {
            return k * e;
        }
    }
    10.0 * e
}

/// The smallest axis step at or above `v`.
fn step_up(v: f64) -> f64 {
    let e = 10f64.powf(v.max(1e-9).log10().floor());
    [1.0, 2.0, 2.5, 3.0, 4.0, 5.0, 10.0]
        .into_iter()
        .map(|k| k * e)
        .find(|s| *s >= v * (1.0 - 1e-9))
        .unwrap_or(10.0 * e)
}

/// An axis of `n` intervals over `lo..hi` (at least `min_span` wide).
fn axis(lo: f64, hi: f64, n: usize, min_span: f64) -> (f64, f64) {
    let mid = (lo + hi) / 2.0;
    let (lo, hi) = if hi - lo < min_span {
        (mid - min_span / 2.0, mid + min_span / 2.0)
    } else {
        (lo, hi)
    };
    let mut step = step_up((hi - lo) / n as f64);
    for _ in 0..64 {
        let floor = (lo / step).floor() * step;
        if floor + step * n as f64 >= hi - 1e-9 {
            return (floor, step);
        }
        step = step_up(step * 1.0001);
    }
    (lo, (hi - lo) / n as f64)
}

/// The decimals an axis step needs for its labels to be exact (at most 2).
fn decimals(step: f64) -> usize {
    (0..2)
        .find(|&d| {
            let v = step * 10f64.powi(d as i32);
            (v - v.round()).abs() < 1e-6
        })
        .unwrap_or(2)
}

/// A trace's height for a plot `ch` pt tall.
fn trace_h(ch: f64) -> f64 {
    ch + 32.0
}

/// The trace `t` in `p` under its heading.
fn draw_trace(pen: &mut Pen<'_>, p: Place, t: &Trace) {
    let inks = pen.inks;
    let read: Vec<f64> = t.vals.iter().flatten().copied().collect();
    let ch = p.h - 32.0;
    let (Some(&first), true) = (read.first(), ch >= 8.0) else {
        return;
    };
    let (mn, mx) = read
        .iter()
        .fold((first, first), |(a, b), &v| (a.min(v), b.max(v)));
    let n = if ch >= 60.0 { 3 } else { 2 };
    let (lo, step) = axis(mn, mx, n, t.min_span);
    let hi = lo + step * n as f64;
    let dec = decimals(step).max(decimals(lo));
    let labels: Vec<String> = (0..=n)
        .map(|k| format!("{:.*}", dec, lo + step * k as f64))
        .collect();
    let gutter = labels
        .iter()
        .map(|l| pen.width(l, AXIS_PT, false))
        .fold(0.0, f64::max)
        + 4.0;
    let (x0, x1) = (p.x + gutter, p.x + p.w);
    let y0 = p.y + 18.0;
    let y1 = y0 + ch;
    if x1 - x0 < 24.0 {
        return;
    }
    for (k, l) in labels.iter().enumerate() {
        let yy = y1 - ch * k as f64 / n as f64;
        pen.rule(x0, yy, x1 - x0, rgba(inks.grid, 0xFF));
        pen.text_end(x0 - 4.0, yy + 3.2, l, AXIS_PT, false, inks.words2);
    }
    let cols = t.vals.len().max(1);
    let pitch = (x1 - x0) / cols as f64;
    let yv = |v: f64| y1 - (v - lo) / (hi - lo) * ch;
    let ink = rgba(inks.words2, 0xFF);
    let mut run: Option<(f64, f64)> = None;
    for (j, v) in t.vals.iter().enumerate() {
        let xa = x0 + j as f64 * pitch;
        match (v.map(yv), run) {
            (Some(yy), Some((sx, sy))) if (yy - sy).abs() < 0.05 => {
                let _ = sx;
            }
            (Some(yy), Some((sx, sy))) => {
                pen.line(sx, sy, xa, sy, ink);
                pen.line(xa, sy, xa, yy, ink);
                run = Some((xa, yy));
            }
            (Some(yy), None) => run = Some((xa, yy)),
            (None, Some((sx, sy))) => {
                pen.line(sx, sy, xa, sy, ink);
                run = None;
            }
            (None, None) => {}
        }
    }
    if let Some((sx, sy)) = run {
        pen.line(sx, sy, x1, sy, ink);
    }
    let mut said: Vec<(f64, f64)> = Vec::new();
    let mut j = 0;
    while j < cols {
        if t.vals.get(j).is_some_and(Option::is_some) {
            j += 1;
            continue;
        }
        let start = j;
        while j < cols && t.vals.get(j).is_some_and(Option::is_none) {
            j += 1;
        }
        let (xa, xb) = (x0 + start as f64 * pitch, x0 + j as f64 * pitch);
        if pitch * pen.s >= 3.0 {
            for k in start..j {
                let xk = x0 + k as f64 * pitch;
                pen.hollow(xk, y1 - 4.0, pitch, 4.0, rgba(inks.hollow, 0xFF));
            }
        } else {
            pen.hollow(xa, y1 - 4.0, xb - xa, 4.0, rgba(inks.hollow, 0xFF));
        }
        let words = "not read";
        let lw = pen.width(words, AXIS_PT, false);
        if ch >= 30.0 && lw <= xb - xa + 16.0 && lw <= x1 - x0 {
            let lx = ((xa + xb) / 2.0 - lw / 2.0).clamp(x0, x1 - lw);
            if !said.iter().any(|&(a, b)| lx < b + 6.0 && a < lx + lw + 6.0) {
                pen.text(lx, y1 - 8.0, words, AXIS_PT, false, inks.words2);
                said.push((lx, lx + lw));
            }
        }
    }
    if let Some((at, label)) = &t.mark {
        let fx = x0 + at * pitch;
        if (x0..=x1).contains(&fx) {
            pen.tick(fx, y0 - 2.0, y1, rgba(inks.words, 0xFF));
            let lw = pen.width(label, AXIS_PT, false);
            if fx - 4.0 - lw >= x0 {
                pen.text(fx - 4.0 - lw, y0 + 8.0, label, AXIS_PT, false, inks.words);
            } else if fx + 4.0 + lw <= x1 {
                pen.text(fx + 4.0, y0 + 8.0, label, AXIS_PT, false, inks.words);
            }
        }
    }
    let base = y1 + 12.0;
    let mut placed: Vec<(f64, f64)> = Vec::new();
    let last = t.under.len().saturating_sub(1);
    let order = [0, last].into_iter().chain(1..last);
    for i in order {
        let Some(&(share, words)) = t.under.get(i) else {
            continue;
        };
        let lw = pen.width(words, AXIS_PT, false);
        let ax = x0 + share * (x1 - x0);
        let lx = if share <= 0.0 {
            ax
        } else if share >= 1.0 {
            ax - lw
        } else {
            ax - lw / 2.0
        };
        if lw > x1 - x0 {
            continue;
        }
        let lx = lx.clamp(x0, x1 - lw);
        if placed
            .iter()
            .any(|&(a, b)| lx < b + 6.0 && a < lx + lw + 6.0)
        {
            continue;
        }
        pen.text(lx, base, words, AXIS_PT, false, inks.words2);
        placed.push((lx, lx + lw));
    }
}

/// A trace section.
#[expect(
    clippy::too_many_arguments,
    reason = "a trace section's words and forms"
)]
fn trace_section<'d>(
    c: Ctx<'d>,
    name: &str,
    subs: Vec<String>,
    trace: Trace,
    said: String,
    label: &str,
    line: Vec<String>,
    right: bool,
) -> Section<'d> {
    let mut forms = Vec::new();
    if trace.vals.iter().any(Option::is_some) {
        let trace = std::rc::Rc::new(trace);
        let heights: &[f64] = if c.two {
            &[36.0]
        } else {
            &[56.0, 48.0, 40.0, 36.0]
        };
        for &ch in heights {
            let (trace, subs, name) = (std::rc::Rc::clone(&trace), subs.clone(), name.to_owned());
            let draw = move |pen: &mut Pen<'_>, p: Place| {
                heading(pen, p, &name, &subs);
                draw_trace(pen, p, &trace);
            };
            forms.push(if c.two {
                grow(trace_h(ch), draw)
            } else {
                form(trace_h(ch), draw)
            });
        }
    }
    let words = vec![said];
    forms.push(line_form(label, line));
    Section {
        name: name.to_owned(),
        words,
        right,
        parts: Vec::new(),
        forms,
    }
}

/// Bytes printed alone ([`words::bytes_lone`]).
fn gb(bytes: f64) -> String {
    words::bytes_lone(bytes.max(0.0).round() as u64)
}

/// A write rate printed alone ([`words::rate_mb_lone`]).
fn mb_s(bytes_per_s: f64) -> String {
    words::rate_mb_lone(bytes_per_s)
}

/// `at_unix_ms` as local `HH:MM:SS`.
fn clock_s(at_unix_ms: i64, utc_offset_s: i64) -> String {
    let secs = at_unix_ms
        .div_euclid(1000)
        .saturating_add(utc_offset_s)
        .rem_euclid(86_400);
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// The disk's growth (§7.7).
fn growth_forms(r: &Rates) -> Vec<String> {
    let change = |g: f64| -> (&'static str, String) {
        if g.abs() < 1e8 {
            ("held steady", String::new())
        } else if g > 0.0 {
            ("grew", gb(g))
        } else {
            ("shrank", gb(-g))
        }
    };
    let join = |verb: &str, amount: &str| {
        if amount.is_empty() {
            verb.to_owned()
        } else {
            format!("{verb} {amount}")
        }
    };
    let short = |g: f64, span: &str| {
        if g.abs() < 1e8 {
            format!("steady in {span}")
        } else {
            let sign = if g > 0.0 { "+" } else { "\u{2212}" };
            format!("{sign}{} in {span}", gb(g.abs()))
        }
    };
    let g10 = r.rate10.map(|v| v * 600.0);
    let g60 = r.rate60.map(|v| v * 3600.0);
    match (g10, g60) {
        (Some(a), Some(b)) => {
            let ((va, aa), (vb, ab)) = (change(a), change(b));
            let second = if va == vb && !ab.is_empty() {
                ab
            } else {
                join(vb, &ab)
            };
            vec![
                format!(
                    "used {} in the last 10 min, {second} in the last hour",
                    join(va, &aa)
                ),
                format!("{}, {}", short(a, "10 min"), short(b, "1 h")),
            ]
        }
        (Some(a), None) => {
            let (v, amount) = change(a);
            vec![
                format!("used {} in the last 10 min", join(v, &amount)),
                short(a, "10 min"),
            ]
        }
        (None, Some(b)) => {
            let (v, amount) = change(b);
            vec![
                format!("used {} in the last hour", join(v, &amount)),
                short(b, "1 h"),
            ]
        }
        (None, None) => vec!["growth not read yet".to_owned()],
    }
}

/// A trace's sentence.
fn trace_said(what: &str, unit: &str, t: &Trace, span: (i64, i64), loc: &Locale) -> String {
    let (start_ms, bucket_ms) = span;
    let fmt = |v: f64| {
        if v >= 100.0 {
            format!("{v:.0}")
        } else {
            format!("{v:.1}")
        }
    };
    let n = t.vals.len();
    let Some((i0, v0)) = t
        .vals
        .iter()
        .enumerate()
        .find_map(|(i, v)| v.map(|v| (i, v)))
    else {
        return format!("{what}: not read yet.");
    };
    let last = t.vals.iter().rev().flatten().next().copied().unwrap_or(v0);
    let ago = |i: usize| {
        let ms = (n - i) as i64 * bucket_ms;
        let (k, one, many) = if ms >= 3_600_000 {
            ((ms as f64 / 3_600_000.0).round() as u64, "hour", "hours")
        } else {
            ((ms as f64 / 60_000.0).round() as u64, "minute", "minutes")
        };
        format!("{k} {} ago", if k == 1 { one } else { many })
    };
    let mut out = format!("{what}: {} {unit} {}, {} now.", fmt(v0), ago(i0), fmt(last));
    let mut j = i0;
    let mut gaps = 0;
    while j < n && gaps < 3 {
        if t.vals[j].is_some() {
            j += 1;
            continue;
        }
        let start = j;
        while j < n && t.vals[j].is_none() {
            j += 1;
        }
        if j < n {
            let at = |i: usize| {
                let mut t = clock_s(start_ms + i as i64 * bucket_ms, loc.utc_offset_s);
                t.truncate(5);
                t
            };
            out.push_str(&format!(" Not read from {} to {}.", at(start), at(j)));
            gaps += 1;
        }
    }
    out
}

/// Bytes as GiB with one decimal, for the memory lens.
fn gib(bytes: u64) -> f64 {
    bytes as f64 / 1_073_741_824.0
}

/// What every lens's builder reads.
#[derive(Clone, Copy)]
struct Ctx<'d> {
    snap: &'d Snapshot,
    m: &'d FaceMetrics,
    /// The front tab's sessions ([`DeckInput::tab_panes`]).
    panes: &'d [u64],
    tab_label: &'d str,
    loc: Locale,
    bar: Bar,
    /// Two columns, and a column's width.
    two: bool,
    cw: f64,
}

impl Ctx<'_> {
    /// Whether a process of `scope` belongs to the scope ↑/↓ chose.
    fn in_scope(&self, scope: ProcScope) -> bool {
        match self.bar {
            Bar::Global => true,
            Bar::Aterm => scope != ProcScope::Other,
            Bar::Tab => matches!(scope, ProcScope::Tab(k) if self.panes.contains(&k)),
        }
    }

    /// The chosen scope as a list heading's suffix.
    fn suffix(&self) -> String {
        match self.bar {
            Bar::Global => String::new(),
            Bar::Aterm => " in aterm".to_owned(),
            Bar::Tab => format!(" in {}", self.tab_label),
        }
    }

    /// A process's scope in a word.
    fn scope_word(&self, scope: ProcScope) -> String {
        match scope {
            ProcScope::Tab(k) => self
                .snap
                .tab(k)
                .map_or_else(|| "a tab".to_owned(), |t| t.label.clone()),
            ProcScope::Aterm => "aterm".to_owned(),
            ProcScope::Other => "other".to_owned(),
        }
    }

    fn at(&self) -> String {
        clock_s(self.snap.at_unix_ms, self.loc.utc_offset_s)
    }

    fn view(&self, res: Resource) -> ResourceView {
        self.snap.resource(res, self.panes)
    }

    /// The row's state now (the answer of every lens but Disk's).
    fn now_answer(&self, res: Resource, of: &str) -> Option<(&'static str, String, InkKind)> {
        let view = self.view(res);
        let mut text = match view.now {
            None => "not read yet".to_owned(),
            Some(_) => {
                let amount =
                    words::amount_forms(res, &view, &self.snap.facts, Ladder::Det, &self.loc)
                        .into_iter()
                        .next()
                        .unwrap_or_default();
                match view.heat_now {
                    Some(h) => format!("{} of {of} \u{b7} {amount}", words::pct(Some(h))),
                    None => amount,
                }
            }
        };
        let state = words::state(res, self.snap, &self.loc);
        if let Some(st) = &state
            && let Some(clause) = st.clauses.first()
        {
            text.push_str(&format!(" \u{b7} {clause}"));
        }
        Some(("Now", text, state.map_or(InkKind::Words2, |st| st.ink)))
    }
}

/// The lens for `c`'s instrument.
fn doc_for<'d>(c: Ctx<'d>, focus: Instrument) -> LensDoc<'d> {
    match focus {
        Instrument::Disk => disk_doc(c),
        Instrument::Cpu => cpu_doc(c),
        Instrument::Memory => memory_doc(c),
        Instrument::Processes => processes_doc(c),
        Instrument::HowToRead => how_to_read(c),
        Instrument::Network | Instrument::Tokens | Instrument::Gpu => text_well(c, focus),
    }
}

/// The disk watch's facts while it runs (the harness is on).
fn watched(snap: &Snapshot) -> Option<&DiskFacts> {
    snap.facts.disk.as_ref().filter(|d| d.watch)
}

/// The Disk lens (§7.7).
fn disk_doc(c: Ctx<'_>) -> LensDoc<'_> {
    let snap = c.snap;
    let d = watched(snap);
    let vol = snap.volume.as_ref();
    let startup = vol.is_none_or(|v| matches!(v.mount.as_str(), "/" | "/System/Volumes/Data"));
    let disk = if startup {
        "the startup disk".to_owned()
    } else {
        vol.map_or_else(String::new, |v| {
            v.mount
                .rsplit('/')
                .find(|s| !s.is_empty())
                .unwrap_or(&v.mount)
                .to_owned()
        })
    };
    let at = c.at();
    let sources = vec![format!("{disk} \u{b7} as of {at}"), format!("as of {at}")];
    let explainer = match d {
        Some(d) => match (d.warn, d.floor) {
            (Some(w), Some(f)) => format!(
                "Heat is how full {disk} is. aterm warns below {} free and cleans its build \
                 caches below {}.",
                gb(w as f64),
                gb(f as f64)
            ),
            (Some(w), None) => format!(
                "Heat is how full {disk} is. aterm warns below {} free.",
                gb(w as f64)
            ),
            _ => format!("Heat is how full {disk} is."),
        },
        None => format!(
            "Heat is how full {disk} is. aterm's disk watch is off, so it sets no warning \
             lines and cleans nothing."
        ),
    };
    let answer = words::disk_wrong(snap, &c.loc).map(|t| {
        let ink = if t.starts_with("Yes") {
            words::state(Resource::Disk, snap, &c.loc).map_or(InkKind::Warn, |st| st.ink)
        } else {
            InkKind::Words2
        };
        ("Wrong?", t, ink)
    });
    let sections = vec![
        space_section(c),
        filling_section(c),
        where_section(c),
        writing_section(c),
    ];
    LensDoc {
        name: "Disk",
        sources,
        explainer,
        answer,
        sections,
        two_order: vec![0, 1, 3, 2],
        keeps_first: true,
    }
}

/// Space now, with Freed (§7.7).
fn space_section(c: Ctx<'_>) -> Section<'_> {
    let snap = c.snap;
    let Some(v) = snap.volume.clone() else {
        return line_section("Space now", "Space", vec!["not read yet".into()], false);
    };
    let d = watched(snap);
    let space = words::disk_space_line(&v);
    let lines: Vec<(&str, u64, bool)> = d.map_or_else(Vec::new, |d| {
        [
            ("warn", d.warn, false),
            ("clean", d.floor, false),
            ("emergency", d.emergency, true),
        ]
        .into_iter()
        .filter_map(|(w, b, critical)| b.map(|b| (w, b, critical)))
        .collect()
    });
    let legend: Vec<String> = if lines.is_empty() {
        Vec::new()
    } else {
        let long: Vec<String> = lines
            .iter()
            .map(|(w, b, _)| format!("{w} {}", gb(*b as f64)))
            .collect();
        let mut forms = vec![format!("lines: {}", long.join(" \u{b7} "))];
        if lines.iter().all(|(_, b, _)| *b < 999_500_000_000) {
            let n = lines.len();
            let short: Vec<String> = lines
                .iter()
                .enumerate()
                .map(|(i, (w, b, _))| {
                    let g = gb(*b as f64);
                    if i + 1 < n {
                        format!("{w} {}", g.trim_end_matches(" GB"))
                    } else {
                        format!("{w} {g}")
                    }
                })
                .collect();
            forms.push(short.join(" \u{b7} "));
        }
        forms
    };
    let freed: Option<Vec<String>> = d.and_then(|d| d.freed_today.as_ref()).map(|(b, at, what)| {
        let t = words::clock(*at, snap.at_unix_ms, c.loc.utc_offset_s);
        let g = gb(*b as f64);
        vec![
            format!("aterm {g} today ({t}, {what})"),
            format!("aterm {g} today, {t}"),
            format!("{g} today"),
        ]
    });
    let mut words = vec![space.clone()];
    words.extend(legend.first().cloned());
    let parts: Vec<(String, String)> = freed
        .as_ref()
        .and_then(|f| f.first())
        .map(|f| ("Freed".to_owned(), f.clone()))
        .into_iter()
        .collect();
    let pieces: Vec<&str> = space.split(" \u{b7} ").collect();
    let parts_line = pieces[..pieces.len().min(2)].join(" \u{b7} ");
    let free_line = pieces.get(1).copied().unwrap_or(&space).to_owned();
    let freed_g = d
        .and_then(|d| d.freed_today.as_ref())
        .map(|(b, ..)| gb(*b as f64));
    let line = match &freed_g {
        Some(g) => vec![
            format!("{space} \u{b7} aterm freed {g} today"),
            format!("{parts_line} \u{b7} freed {g}"),
            parts_line,
            free_line,
        ],
        None => vec![space.clone(), parts_line, free_line],
    };
    let h = if freed.is_some() { 56.0 } else { 42.0 };
    let full = form(h, move |pen, p| {
        heading(pen, p, "Space now", &[]);
        let inks = pen.inks;
        let (by, bh) = (p.y + 16.0, 10.0);
        let total = v.total.max(1) as f64;
        pen.rect(p.x, by, p.w, bh, 1.5, rgba(inks.track, 0xFF));
        let used = p.w * (v.used() as f64 / total).clamp(0.0, 1.0);
        let machine = pen.machine();
        pen.rect(p.x, by, used, bh, 1.5, rgba(machine, 0xFF));
        for (_, b, critical) in &lines {
            let lx = p.x + p.w * (1.0 - (*b as f64 / total).clamp(0.0, 1.0));
            let ink = if *critical { inks.critical } else { inks.warn };
            pen.tick(lx, by - 2.0, by + bh + 2.0, rgba(ink, 0xFF));
        }
        let base = p.y + 39.0;
        let s = pen.cut(&space, p.w, NOTE_PT, false);
        let lw = pen.text(p.x, base, &s, NOTE_PT, false, inks.words);
        if let Some(l) = first_fit(pen.m, &legend, p.w - lw - 12.0, NOTE_PT, false) {
            pen.text_end(p.x + p.w, base, &l, NOTE_PT, false, inks.words2);
        }
        if let Some(freed) = &freed {
            let base = p.y + 53.0;
            let fw = pen.text(p.x, base, "Freed", HEAD_PT, true, inks.words);
            let tab = tab_after(fw);
            let s = fit_cut(pen.m, freed, p.w - tab, BODY_PT, false);
            pen.text(p.x + tab, base, &s, BODY_PT, false, inks.words2);
        }
    });
    Section {
        name: "Space now".into(),
        words,
        right: false,
        parts,
        forms: vec![full, line_form("Space", line)],
    }
}

/// GB free, last 24 hours.
fn filling_section(c: Ctx<'_>) -> Section<'_> {
    let snap = c.snap;
    let now = snap.at_unix_ms;
    const BUCKET_MS: i64 = aterm_vitals::history::FREE_BUCKET_S * 1000;
    let vals: Vec<Option<f64>> = snap
        .free_history
        .window(now)
        .into_iter()
        .map(|v| v.map(|b| b as f64 / 1e9))
        .collect();
    let n = vals.len() as i64;
    let start_ms = (now.div_euclid(BUCKET_MS) + 1 - n) * BUCKET_MS;
    let freed = watched(snap).and_then(|d| d.freed_today.as_ref());
    let mark = freed.and_then(|(b, at, _)| {
        let idx = (at - start_ms) as f64 / BUCKET_MS as f64;
        (0.0..n as f64).contains(&idx).then(|| {
            let t = words::clock(*at, now, c.loc.utc_offset_s);
            (idx, format!("aterm freed {} \u{b7} {t}", gb(*b as f64)))
        })
    });
    let growth = growth_forms(&snap.facts.disk_rates);
    let trace = Trace {
        vals,
        under: DAY,
        min_span: 1.0,
        mark,
    };
    let mut said = trace_said(
        "GB free over the last 24 hours",
        "gigabytes",
        &trace,
        (start_ms, BUCKET_MS),
        &c.loc,
    );
    if let Some((b, at, _)) = freed {
        said.push_str(&format!(
            " aterm freed {} at {}.",
            words::speak(&gb(*b as f64)),
            words::clock(*at, now, c.loc.utc_offset_s)
        ));
    }
    let mut line = growth.clone();
    if let Some(f) = snap.facts.filling {
        line.insert(0, format!("full in {}", words::duration_approx(f.secs)));
    }
    trace_section(
        c,
        "GB free, last 24 hours",
        growth,
        trace,
        said,
        "Filling",
        line,
        false,
    )
}

/// Where the space is (§7.4, §7.7).
fn where_section(c: Ctx<'_>) -> Section<'_> {
    let snap = c.snap;
    let d = snap.facts.disk.as_ref();
    let hogs = d.map_or(&[][..], |d| d.hogs.as_slice());
    let sizing = d.is_some_and(|d| d.sizing);
    let at = d
        .and_then(|d| d.hogs_at)
        .map(|t| words::clock(t, snap.at_unix_ms, c.loc.utc_offset_s));
    let size = |h: &aterm_vitals::model::Hog| {
        if h.partial {
            format!("\u{2265} {}", gb(h.bytes as f64))
        } else {
            gb(h.bytes as f64)
        }
    };
    let subs: Vec<String> = match (sizing, &at) {
        (true, Some(t)) => vec![
            format!("sizing folders\u{2026} last sized {t}"),
            "sizing folders\u{2026}".into(),
        ],
        (true, None) => vec!["sizing folders\u{2026}".into()],
        (false, Some(t)) => vec![
            format!("folders aterm can clean, sized at {t}"),
            format!("sized {t}"),
        ],
        (false, None) => Vec::new(),
    };
    if hogs.is_empty() {
        let says = if sizing {
            "sizing folders\u{2026}"
        } else {
            "no folders sized yet"
        };
        return line_section("Where the space is", "Where", vec![says.into()], true);
    }
    let rows: Vec<Row> = hogs
        .iter()
        .take(3)
        .map(|h| Row {
            chip: None,
            name: h.path.clone(),
            scope: None,
            share: None,
            figure: size(h),
        })
        .collect();
    let first = &hogs[0];
    let rest: Vec<&aterm_vitals::model::Hog> = hogs.iter().skip(1).collect();
    let mut line = Vec::new();
    let most = format!("most in {}, {}", first.path, size(first));
    if !rest.is_empty() {
        let sum: u64 = rest.iter().map(|h| h.bytes).sum();
        line.push(format!(
            "{most} \u{b7} {} more {}, {}",
            rest.len(),
            if rest.len() == 1 { "folder" } else { "folders" },
            gb(sum as f64)
        ));
    }
    line.push(most);
    list_section(
        c,
        "Where the space is".into(),
        subs,
        rows,
        None,
        3,
        "Where",
        line,
        true,
    )
}

/// Writing now (§7.7).
fn writing_section(c: Ctx<'_>) -> Section<'_> {
    let snap = c.snap;
    let writers: Vec<(String, ProcScope, f64)> = snap
        .facts
        .writers
        .iter()
        .filter(|w| c.in_scope(w.scope))
        .map(|w| {
            let n = usize::try_from(w.n).unwrap_or(usize::MAX);
            (program_label(&w.name, n), w.scope, w.bytes_per_s)
        })
        .collect();
    let name = format!("Writing now{}", c.suffix());
    if writers.is_empty() {
        let says = match c.loc.platform {
            Platform::Linux => "no process's writes are read on Linux",
            _ => "nothing written in the last reading",
        };
        return line_section(&name, "Writing", vec![says.into()], true);
    }
    let peak = nice_up(writers.iter().map(|w| w.2).fold(1.0, f64::max));
    let rows: Vec<Row> = writers
        .iter()
        .take(4)
        .map(|(name, scope, rate)| Row {
            chip: Some(*scope),
            name: name.clone(),
            scope: Some(c.scope_word(*scope)),
            share: Some(rate / peak),
            figure: mb_s(*rate),
        })
        .collect();
    let top = &rows[0];
    let lead = format!(
        "{} in {}, {}",
        top.name,
        top.scope.clone().unwrap_or_default(),
        top.figure
    );
    let mut line = Vec::new();
    if let Some(all) = snap.resources[Resource::Disk.index()].aterm.now {
        line.push(format!("{lead} \u{b7} all of aterm {}", mb_s(all)));
    }
    line.push(lead);
    let note = match c.loc.platform {
        Platform::MacOs => Some("macOS's own writes and other users' are not counted".to_owned()),
        Platform::Linux | Platform::Windows => {
            Some("the system's own writes and other users' are not counted".to_owned())
        }
    };
    list_section(
        c,
        name,
        vec![
            "bytes your processes wrote per second, not net growth".into(),
            "bytes written per second, not growth".into(),
        ],
        rows,
        note,
        4,
        "Writing",
        line,
        true,
    )
}

/// The CPU lens (§11.4).
fn cpu_doc(c: Ctx<'_>) -> LensDoc<'_> {
    let snap = c.snap;
    let n = snap.total_cores().unwrap_or(0);
    let at = c.at();
    let sources = match snap.cpu.as_ref().filter(|_| n > 0) {
        Some(cpu) => vec![
            format!(
                "{n} cores in {} {} \u{b7} as of {at}",
                cpu.clusters.len(),
                if cpu.clusters.len() == 1 {
                    "cluster"
                } else {
                    "clusters"
                }
            ),
            format!("{n} cores \u{b7} as of {at}"),
            format!("as of {at}"),
        ],
        None => vec![format!("as of {at}")],
    };
    let explainer = if n > 0 {
        format!(
            "Heat is how many of the {n} cores were busy, as a share of all of them: 100% is \
             every core busy all the time, whoever's work it was."
        )
    } else {
        "Heat is how many of the cores were busy, as a share of all of them.".to_owned()
    };
    let view = c.view(Resource::Cpu);
    let over = view
        .columns(10, 60, snap.at_unix_ms)
        .iter()
        .filter(|cell| cell.over)
        .count();
    let over_words = match over {
        0 => "not in the last 10 min".to_owned(),
        k if k < 6 => format!("{} s of the last 10 min", k * 10),
        k => format!("{:.0} of the last 10 min", (k as f64 / 6.0).round()),
    };
    let load = match (snap.facts.load1, n) {
        (Some(l), n) if n > 0 => {
            let waiting = (l - n as f64).round();
            let base = format!("{l:.1} on {n} cores");
            vec![
                if waiting >= 1.0 {
                    format!("{base}: ~{waiting:.0} waiting for a core")
                } else {
                    format!("{base}: none waiting for a core")
                },
                base,
            ]
        }
        _ => vec!["not read yet".to_owned()],
    };
    let why = why_now_section(c);
    let mut sections = vec![
        cores_section(c, why.is_some()),
        busiest_section(c),
        line_section("Load", "Load", load, true),
        line_section("Over max", "Over max", vec![over_words], true),
    ];
    sections.extend(why);
    LensDoc {
        name: "CPU",
        sources,
        explainer,
        answer: c.now_answer(Resource::Cpu, "capacity"),
        sections,
        two_order: Vec::new(),
        keeps_first: false,
    }
}

/// Cores, last 60 s: a core × second strip per cluster with its caption.
fn cores_section(c: Ctx<'_>, under: bool) -> Section<'_> {
    let Some(cpu) = c.snap.cpu.as_ref().filter(|cpu| cpu.total() > 0) else {
        return line_section(
            "Cores, last 60 s",
            "Cores",
            vec!["not read yet".into()],
            false,
        );
    };
    let n = cpu.total();
    let busy_half = (0..n)
        .filter(|&core| {
            let hist: Vec<f32> = cpu
                .core_history
                .get(core)
                .map(|h| h.iter().flatten().copied().collect())
                .unwrap_or_default();
            !hist.is_empty() && hist.iter().sum::<f32>() / hist.len() as f32 > 0.5
        })
        .count();
    let captions: Vec<String> = cpu
        .clusters
        .iter()
        .map(|cl| format!("{} {}", cl.cores.len(), cl.name))
        .collect();
    let summary = format!("{busy_half} of {n} cores busy over half the last minute");
    let words = vec![format!("{}; {summary}", captions.join(", "))];
    let gaps = 3.0 * cpu.clusters.len().saturating_sub(1) as f64;
    let strip_h = |rh: f64| 16.0 + n as f64 * rh + gaps + 2.0;
    let subs = vec![
        "a row a core, a column a second".to_owned(),
        "a row a core".to_owned(),
    ];
    let mut forms = Vec::new();
    let heights: &[(f64, bool)] = match (c.two, under) {
        (true, false) => &[(1.0, true)],
        (true, true) => &[
            (1.0, true),
            (4.0, false),
            (3.0, false),
            (2.0, false),
            (1.0, false),
        ],
        (false, _) => &[(4.0, false), (3.0, false), (2.0, false), (1.0, false)],
    };
    for &(rh, grows) in heights {
        let subs = subs.clone();
        let draw = move |pen: &mut Pen<'_>, p: Place| {
            heading(pen, p, "Cores, last 60 s", &subs);
            draw_cores(pen, p, cpu, gaps);
        };
        forms.push(if grows {
            Form {
                last: under,
                ..grow(strip_h(rh), draw)
            }
        } else {
            form(strip_h(rh), draw)
        });
    }
    let line = vec![summary.clone(), format!("{busy_half} of {n} busy")];
    forms.push(line_form("Cores", line));
    Section {
        name: "Cores, last 60 s".into(),
        words,
        right: false,
        parts: Vec::new(),
        forms,
    }
}

/// The strip: each cluster's caption in a gutter, then its cores in heat inks.
fn draw_cores(pen: &mut Pen<'_>, p: Place, cpu: &CpuView, gaps: f64) {
    let inks = pen.inks;
    let n = cpu.total().max(1);
    let top = p.y + 16.0;
    let rh = ((p.y + p.h - 2.0 - top - gaps) / n as f64).clamp(1.0 / pen.s, 8.0);
    let gutter = cpu
        .clusters
        .iter()
        .map(|cl| {
            pen.width(&cl.cores.len().to_string(), AXIS_PT, true)
                + 3.0
                + pen.width(&cl.name, AXIS_PT, false)
        })
        .fold(0.0, f64::max)
        .min(p.w * 0.3)
        + 6.0;
    let cols = aterm_vitals::model::CORE_HISTORY_LEN;
    let gx = p.x + gutter;
    let cw = (p.w - gutter) / cols as f64;
    let gap = if rh * pen.s >= 3.0 { 1.0 / pen.s } else { 0.0 };
    let mut y = top;
    let mut caption_floor = f64::MIN;
    for cl in &cpu.clusters {
        let base = y + 7.0;
        if base - ASCENT_EM * AXIS_PT >= caption_floor && base + DESCENT_EM * AXIS_PT <= p.y + p.h {
            let count = cl.cores.len().to_string();
            let cw0 = pen.text(p.x, base, &count, AXIS_PT, true, inks.words);
            let name = pen.cut(&cl.name, gutter - 6.0 - cw0 - 3.0, AXIS_PT, false);
            pen.text(p.x + cw0 + 3.0, base, &name, AXIS_PT, false, inks.words2);
            caption_floor = base + DESCENT_EM * AXIS_PT + 1.0;
        }
        for (r, &core) in cl.cores.iter().enumerate() {
            let ry = y + r as f64 * rh;
            let hist = cpu.core_history.get(core).map_or(&[][..], Vec::as_slice);
            let off = cols.saturating_sub(hist.len());
            let mut run: Option<(usize, Rgb)> = None;
            for col in 0..=cols {
                let ink = (col < cols && col >= off)
                    .then(|| hist.get(col - off).copied().flatten())
                    .flatten()
                    .map(|v| pen.heat(v));
                match (run, ink) {
                    (Some((_, a)), Some(b)) if a == b => {}
                    (prev, next) => {
                        if let Some((c0, ink)) = prev {
                            pen.rect(
                                gx + c0 as f64 * cw,
                                ry,
                                (col - c0) as f64 * cw,
                                rh - gap,
                                0.0,
                                rgba(ink, 0xFF),
                            );
                        }
                        run = next.map(|ink| (col, ink));
                    }
                }
            }
        }
        y += cl.cores.len() as f64 * rh + 3.0;
    }
}

/// The busiest program in the chosen scope, its processes grouped by name: `rustc ×9 in Tab 3,
/// 5.9 cores`, and how long it has been up once it has run long (`, up 47 min`; `up ≥ 47 min`
/// while a member's birth went unread).
fn busiest_line(c: Ctx<'_>) -> Option<String> {
    let snap = c.snap;
    let b = snap
        .facts
        .busiest
        .as_ref()
        .filter(|b| c.bar == Bar::Global || b.scope.is_some_and(|s| c.in_scope(s)))?;
    let who = b
        .scope
        .map(|s| format!(" in {}", c.scope_word(s)))
        .unwrap_or_default();
    let times = if b.n > 1 {
        format!(" \u{d7}{}", b.n)
    } else {
        String::new()
    };
    let up = words::up_words(b.age_ms(snap.at_unix_ms), b.unborn > 0)
        .map(|up| format!(", {up}"))
        .unwrap_or_default();
    Some(format!(
        "{}{times}{who}, {} cores{up}",
        aterm_vitals::jobs::wire::person_name(&b.name),
        words::cores(b.cores)
    ))
}

/// Busiest now: the busiest processes in the chosen scope.
fn busiest_section(c: Ctx<'_>) -> Section<'_> {
    let snap = c.snap;
    let n = snap.total_cores().unwrap_or(1).max(1) as f64;
    let rows: Vec<Row> = grouped(
        snap.top
            .iter()
            .filter(|t| c.in_scope(t.scope))
            .map(|t| (t.name.as_str(), t.scope, f64::from(t.cores))),
    )
    .into_iter()
    .map(|(name, scope, cores)| Row {
        chip: Some(scope),
        name,
        scope: Some(c.scope_word(scope)),
        share: Some(cores / n),
        figure: format!("{} cores", words::cores(cores as f32)),
    })
    .collect();
    let name = format!("Busiest now{}", c.suffix());
    let mut line: Vec<String> = busiest_line(c).into_iter().collect();
    line.extend(rows.first().map(Row::said));
    if rows.is_empty() {
        line.push("nothing busy in this scope".into());
        return line_section(&name, "Busiest", line, true);
    }
    list_section(
        c,
        name,
        vec![
            "cores-worth over the last reading".into(),
            "cores-worth".into(),
        ],
        rows,
        None,
        if c.two { 6 } else { 4 },
        "Busiest",
        line,
        true,
    )
}

/// Where a row's words start after the ∵ mark.
const MARK_ADV: f64 = 11.0;

/// Why now (RD §4.2), worded from the account [`account_of`] gives; `None` unless a covering
/// episode is open on facts no older than [`CLAUSE_STALE_MS`].
fn why_now_section(c: Ctx<'_>) -> Option<Section<'_>> {
    let snap = c.snap;
    let a = account_of(WhySubject::Cpu, c.panes, snap);
    let f = a.facts.as_deref()?;
    let age_ms = snap.at_ns.saturating_sub(f.at_ns) / 1_000_000;
    let top = f.open.iter().find(|o| o.id.covering());
    let top = top.filter(|_| age_ms <= CLAUSE_STALE_MS)?;
    let held = a
        .causes
        .iter()
        .find(|s| s.cause.id.covering() && s.opened_unix_ms.is_some())
        .map(|s| {
            let context = Context::ORDER
                .into_iter()
                .find(|c| s.cause.contexts.contains(c));
            (
                s.cause.id,
                context,
                s.cause.covers_mc,
                Some(&s.cause.evidence),
            )
        });
    let (id, context, covers_mc, evidence) = held.unwrap_or_else(|| {
        let context = Context::ORDER
            .into_iter()
            .find(|c| top.contexts.contains(c));
        (top.id, context, top.covers_mc, None)
    });
    let services = words::services_word(c.loc.platform.name());
    let two = |mc: i64| cents((mc + 5).div_euclid(10));

    let (marked, base, short) = match id {
        CauseId::Activity(act) => {
            let word = act.activity().word.to_owned();
            (true, word.clone(), word)
        }
        CauseId::AppWork(coalition) => {
            let lead = f
                .split
                .rows
                .iter()
                .find(|r| r.id == coalition)
                .and_then(|r| r.lead.as_ref());
            let name = lead.and_then(|l| l.exe.as_deref()).map_or_else(
                || "an app".to_owned(),
                aterm_vitals::jobs::wire::person_name,
            );
            let voucher = lead.map_or(Voucher::Unknown, |l| l.voucher);
            let chip = match voucher {
                Voucher::Apple => "Apple".to_owned(),
                Voucher::DeveloperId => lead.and_then(Lead::team_id).map_or_else(
                    || "Developer ID".to_owned(),
                    |t| format!("Developer ID {t}"),
                ),
                Voucher::AdHoc => "ad-hoc".to_owned(),
                v => v.word().to_owned(),
            };
            let whose = if voucher == Voucher::Apple {
                "macOS (as you)"
            } else {
                "yours"
            };
            (true, format!("{name} \u{b7} {chip} \u{b7} {whose}"), name)
        }
        CauseId::TabWork(k) => {
            let work = snap.tab(k).map_or_else(
                || "a tab's work".to_owned(),
                |t| format!("{}'s work", t.label),
            );
            (true, work.clone(), work)
        }
        CauseId::AtermWork => (true, "aterm itself".to_owned(), "aterm itself".to_owned()),
        CauseId::Unexplained => {
            let base = match evidence {
                Some(Evidence::Uncovered { part }) => {
                    let word = match part {
                        Part::Services => services,
                        Part::Others => "others",
                        Part::Aterm => "aterm",
                    };
                    format!("no cause aterm can name: {word}")
                }
                Some(Evidence::Unaccounted) => "no cause aterm can name: unaccounted".to_owned(),
                _ => "no cause aterm can name".to_owned(),
            };
            (false, base.clone(), base)
        }
        CauseId::Context(_) => return None,
    };
    let long = words::cause_words(&base, context);
    let covers = if held.is_some() {
        format!("{} cores", two(covers_mc))
    } else {
        format!("was {} cores", two(covers_mc))
    };

    let at_least = a.tab_at_least(snap);
    let p = a.parts.unwrap_or_else(|| f.split.parts_of(&[]));
    let [services_h, others_h, tab_h, rest_h, residual_h] = hundredths(
        [p.services, p.others, p.tab, p.aterm_rest, p.unaccounted],
        f.split.global_mc,
    );
    let tab_part = (!c.panes.is_empty()).then(|| match a.parts {
        Some(_) => format!(
            ", {} {}{}",
            c.tab_label,
            if at_least { "at least " } else { "" },
            cents(tab_h)
        ),
        None => format!(", {} not read", c.tab_label),
    });
    let mut parts = [
        (services, p.services, services_h, None),
        ("others", p.others, others_h, None),
        ("aterm", p.tab + p.aterm_rest, tab_h + rest_h, tab_part),
    ];
    parts.sort_by_key(|x| std::cmp::Reverse(x.1));
    let residual = (residual_h != 0).then(|| {
        let sign = if residual_h > 0 { "+" } else { "" };
        format!("unaccounted {sign}{}", cents(residual_h))
    });
    let split_of = |tab: bool, residual: Option<&String>| {
        let words = parts.iter().map(|(w, _, h, part)| {
            let part = part.as_deref().filter(|_| tab).unwrap_or_default();
            format!("{w} {}{part}", cents(*h))
        });
        words
            .chain(residual.cloned())
            .collect::<Vec<_>>()
            .join(" \u{b7} ")
    };
    let split = split_of(true, residual.as_ref());
    let splits = dedup(vec![
        split.clone(),
        split_of(false, residual.as_ref()),
        split_of(false, None),
    ]);
    let busy = format!("{} cores", two(f.split.global_mc));

    let y = &a.you;
    let of = |mc: i64| {
        format!(
            "{}{} of {} cores",
            if at_least { "at least " } else { "" },
            words::cores(mc as f32 / 1000.0),
            words::cores(f.split.global_mc as f32 / 1000.0)
        )
    };
    let you = match (y.role, y.figure) {
        (Role::Main, Some(t)) => format!("this tab is the main cause: {}", of(t)),
        (Role::Minor, Some(t)) => {
            let pct = y
                .share_pm
                .filter(|_| !at_least)
                .map(|pm| format!(" ({:.1}%)", f64::from(pm) / 10.0))
                .unwrap_or_default();
            format!("this tab uses {}{pct}; it is not the cause", of(t))
        }
        (Role::Unavailable(_), Some(t)) => {
            format!(
                "this tab uses {}; whether it is the cause is not read now",
                of(t)
            )
        }
        _ => y.clause().to_owned(),
    };
    let trend = snap.cpu.as_ref().map_or("trend not read yet", |cpu| {
        cpu.busy
            .motion(words::CORES_GATE)
            .word()
            .unwrap_or("steady")
    });

    let step = if a.nothing_needed {
        "nothing needed"
    } else {
        "no step offered yet"
    };
    let how = "measured per coalition";
    let signed = match id {
        CauseId::Activity(act) => format!(", {} by its code signature", act.activity().word),
        _ => String::new(),
    };

    let rows: [(Vec<String>, Option<String>); 4] = [
        (
            dedup(vec![long.clone(), short.clone()]),
            Some(covers.clone()),
        ),
        (splits, Some(busy.clone())),
        (
            dedup(vec![
                format!("{you} \u{b7} {trend}"),
                format!("{} \u{b7} {trend}", y.clause()),
            ]),
            None,
        ),
        (
            dedup(vec![
                format!("{step} \u{b7} {how}{signed}"),
                format!("{step} \u{b7} {how}"),
                step.to_owned(),
            ]),
            None,
        ),
    ];
    let said = vec![
        format!("{}{long}, {covers}.", if marked { "Why: " } else { "" }),
        format!("{busy} busy: {split}."),
        format!("{you}, {trend}."),
        format!("{step}; {how}{signed}."),
    ];
    let window = window_words(f.window_ns);
    let exact = if f.split.exact { "exact, " } else { "" };
    let subs = vec![
        format!("{window} \u{b7} {exact}{} coalitions", f.split.listed),
        window,
    ];
    let line = dedup(vec![
        format!("{long}; {}", y.clause()),
        format!("{short}; {}", y.clause()),
        short,
    ]);
    let full = form(rows_h(rows.len(), false), move |pen, p| {
        heading(pen, p, "Why now", &subs);
        let ink = pen.inks.words2;
        for (i, (forms, figure)) in rows.iter().enumerate() {
            let base = p.y + ROW0_BASE + PITCH * i as f64;
            let fig_w = figure.as_deref().map_or(0.0, |s| {
                pen.text_end(p.x + p.w, base, s, BODY_PT, false, ink) + 8.0
            });
            let x = if i == 0 && marked {
                cause_mark(pen, p.x, base);
                MARK_ADV
            } else {
                0.0
            };
            let s = fit_cut(pen.m, forms, p.w - x - fig_w, BODY_PT, false);
            pen.text(p.x + x, base, &s, BODY_PT, false, ink);
        }
    });
    let one = Form {
        h: LINE_H,
        grow: false,
        last: false,
        label: Some("Why now".to_owned()),
        draw: Box::new(move |pen, p| {
            let base = p.y + HEAD_BASE;
            let label = pen.cut("Why now", p.w, HEAD_PT, true);
            let ink = pen.inks.words;
            let lw = pen.text(p.x, base, &label, HEAD_PT, true, ink);
            let mut x = tab_after(lw).max(pen.line_tab);
            if marked {
                cause_mark(pen, p.x + x, base);
                x += MARK_ADV;
            }
            let s = fit_cut(pen.m, &line, p.w - x, BODY_PT, false);
            let ink = pen.inks.words2;
            pen.text(p.x + x, base, &s, BODY_PT, false, ink);
        }),
    };
    Some(Section {
        name: "Why now".into(),
        words: said,
        right: false,
        parts: Vec::new(),
        forms: vec![full, one],
    })
}

/// The ∵ mark in the words2 ink ([`MARK_ADV`]).
fn cause_mark(pen: &mut Pen<'_>, x: f64, base: f64) {
    let ink = pen.inks.words2;
    pen.mark(
        WellMark::Cause,
        x + 4.0,
        base - 3.4,
        BODY_PT,
        crate::stats_paint::QUIET_ARC,
        ink,
    );
}

/// Millicores `mcs` in hundredths of a core.
fn hundredths<const N: usize>(mcs: [i64; N], total_mc: i64) -> [i64; N] {
    let mut out = mcs.map(|mc| mc.div_euclid(10));
    let short = (total_mc + 5).div_euclid(10) - out.iter().sum::<i64>();
    let mut order: [usize; N] = std::array::from_fn(|i| i);
    order.sort_by_key(|&i| std::cmp::Reverse(mcs[i].rem_euclid(10)));
    for &i in order.iter().take(usize::try_from(short).unwrap_or(0)) {
        out[i] += 1;
    }
    out
}

/// Why now's window, `window_ns` long, as `1 s window` (`0.9 s window`):
/// rounded to a tenth of a second first, then in whole seconds where that
/// reads 1.0 s or more and with its one decimal under it, so a window that
/// wobbles about a second (995 to 1,004 ms from one split tick to the next)
/// keeps its words.
fn window_words(window_ns: u64) -> String {
    const SECOND_NS: u64 = 1_000_000_000;
    const TENTH_NS: u64 = SECOND_NS / 10;
    let tenths = window_ns.saturating_add(TENTH_NS / 2) / TENTH_NS;
    if tenths >= 10 {
        let secs = window_ns.saturating_add(SECOND_NS / 2) / SECOND_NS;
        format!("{secs} s window")
    } else {
        format!("0.{tenths} s window")
    }
}

/// Hundredths of a core `h` as `2.17` (`-0.05`).
fn cents(h: i64) -> String {
    let sign = if h < 0 { "-" } else { "" };
    format!("{sign}{}.{:02}", h.abs() / 100, h.abs() % 100)
}

/// `forms` with each repeat of the one before it dropped.
fn dedup(mut forms: Vec<String>) -> Vec<String> {
    forms.dedup();
    forms
}

/// The Memory lens (§11.4).
fn memory_doc(c: Ctx<'_>) -> LensDoc<'_> {
    let snap = c.snap;
    let at = c.at();
    let sources = match snap.mem.as_ref() {
        Some(m) => vec![
            format!(
                "{} GiB of memory \u{b7} as of {at}",
                words::gib_total(m.sample.total)
            ),
            format!("as of {at}"),
        ],
        None => vec![format!("as of {at}")],
    };
    let explainer = "Heat is how much memory is in use: apps, wired and compressed. File cache \
                     is not counted; macOS gives it back when apps need it."
        .to_owned();
    let now = snap.at_unix_ms;
    let vals: Vec<Option<f64>> = c
        .view(Resource::Memory)
        .columns(60, 60, now)
        .iter()
        .map(|cell| cell.mean.map(|b| b / 1_073_741_824.0))
        .collect();
    let trace = Trace {
        vals,
        under: HOUR,
        min_span: 0.5,
        mark: None,
    };
    let start = (now.div_euclid(60_000) - 59) * 60_000;
    let said = trace_said(
        "Used over the last hour",
        "GiB",
        &trace,
        (start, 60_000),
        &c.loc,
    );
    let first = trace.vals.iter().flatten().next().copied();
    let last = trace.vals.iter().rev().flatten().next().copied();
    let line = match (first, last) {
        (Some(a), Some(b)) => vec![format!("{a:.1} to {b:.1} GiB")],
        _ => vec!["not read yet".to_owned()],
    };
    let sections = vec![
        classes_section(c),
        trace_section(
            c,
            "Used, last hour",
            vec!["GiB, the minute's mean".into(), "GiB".into()],
            trace,
            said,
            "Last hour",
            line,
            false,
        ),
        biggest_section(c),
        swap_section(c),
        pressure_section(c),
    ];
    LensDoc {
        name: "Memory",
        sources,
        explainer,
        answer: c.now_answer(Resource::Memory, "capacity"),
        sections,
        two_order: Vec::new(),
        keeps_first: false,
    }
}

/// Where it is: the classes as one stacked bar, and their legend.
fn classes_section(c: Ctx<'_>) -> Section<'_> {
    let Some(m) = c.snap.mem.as_ref() else {
        return line_section("Where it is", "Classes", vec!["not read yet".into()], false);
    };
    let s = &m.sample;
    let classes: Vec<(usize, f32, &str, &str, u64)> = [
        (0, 1.0, "app", "app", Some(s.app)),
        (0, 0.7, "wired", "wired", Some(s.wired)),
        (1, 1.0, "compressed", "compr.", s.compressed),
        (2, 1.0, "file cache", "cache", Some(s.cached)),
        (3, 1.0, "free", "free", Some(s.free)),
    ]
    .into_iter()
    .filter_map(|(i, al, long, short, b)| b.map(|b| (i, al, long, short, b)))
    .collect();
    let total = s.total.max(1);
    let words: Vec<String> = vec![
        classes
            .iter()
            .map(|(_, _, long, _, b)| format!("{long} {:.1} GiB", gib(*b)))
            .collect::<Vec<_>>()
            .join(", "),
    ];
    let line = vec![
        classes
            .iter()
            .map(|(_, _, long, _, b)| format!("{long} {:.1}", gib(*b)))
            .collect::<Vec<_>>()
            .join(" \u{b7} ")
            + " GiB",
    ];
    let subs = vec![
        format!(
            "{} of {} GiB in use",
            words::gib_figure(s.used()),
            words::gib_total(s.total)
        ),
        format!("{} GiB in use", words::gib_figure(s.used())),
    ];
    let full = form(42.0, move |pen, p| {
        heading(pen, p, "Where it is", &subs);
        let inks = pen.inks;
        let (by, bh) = (p.y + 16.0, 10.0);
        pen.rect(p.x, by, p.w, bh, 1.5, rgba(inks.track, 0xFF));
        let mut x = p.x;
        for (i, al, _, _, b) in &classes {
            if *i == 3 {
                continue;
            }
            let w = (p.w * (*b as f64 / total as f64))
                .min(p.x + p.w - x)
                .max(0.0);
            pen.rect(x, by, w, bh, 0.0, a(inks.mem[*i], *al));
            x += w;
        }
        let base = p.y + 39.0;
        let item = |pen: &Pen<'_>, word: &str, b: u64, k: usize| {
            let unit = if k == 0 { " GiB" } else { "" };
            let t = format!("{word} {:.1}{unit}", gib(b));
            let w = pen.width(&t, NOTE_PT, false);
            (t, w)
        };
        let fits = |pen: &Pen<'_>, short: bool| {
            classes
                .iter()
                .enumerate()
                .map(|(k, (_, _, long, sh, b))| {
                    item(pen, if short { sh } else { long }, *b, k).1 + 18.0
                })
                .sum::<f64>()
                <= p.w
        };
        let short = !fits(pen, false);
        let mut x = p.x;
        for (k, (i, al, long, sh, b)) in classes.iter().enumerate() {
            let (t, w) = item(pen, if short { sh } else { long }, *b, k);
            if x + 8.0 + w > p.x + p.w {
                break;
            }
            let ink = if *i == 3 {
                rgba(inks.track, 0xFF)
            } else {
                a(inks.mem[*i], *al)
            };
            pen.rect(x, base - 6.5, 6.0, 6.0, 1.0, ink);
            pen.text(x + 8.0, base, &t, NOTE_PT, false, inks.words2);
            x += 8.0 + w + 10.0;
        }
    });
    Section {
        name: "Where it is".into(),
        words,
        right: false,
        parts: Vec::new(),
        forms: vec![full, line_form("Classes", line)],
    }
}

/// Biggest now: footprints in the chosen scope.
fn biggest_section(c: Ctx<'_>) -> Section<'_> {
    let snap = c.snap;
    let total = snap.mem.as_ref().map_or(1, |m| m.sample.total.max(1)) as f64;
    let rows: Vec<Row> = grouped(snap.top.iter().filter_map(|t| {
        t.footprint
            .filter(|_| c.in_scope(t.scope))
            .map(|f| (t.name.as_str(), t.scope, f as f64))
    }))
    .into_iter()
    .map(|(name, scope, bytes)| Row {
        chip: Some(scope),
        name,
        scope: Some(c.scope_word(scope)),
        share: Some(bytes / total),
        figure: words::mem(bytes as u64),
    })
    .collect();
    let view = c.view(Resource::Memory);
    let mut scopes = Vec::new();
    if let Some(b) = view.aterm.now {
        scopes.push(format!("aterm {}", words::mem(b as u64)));
    }
    if let Some(b) = view.tab.now {
        scopes.push(format!("{} {}", c.tab_label, words::mem(b as u64)));
    }
    let subs: Vec<String> = if scopes.is_empty() {
        vec!["physical footprint".into()]
    } else {
        vec![scopes.join(" \u{b7} "), scopes[0].clone()]
    };
    let name = format!("Biggest now{}", c.suffix());
    let mut line: Vec<String> = rows.first().map(Row::said).into_iter().collect();
    if rows.is_empty() {
        line.push(
            subs.first()
                .cloned()
                .unwrap_or_else(|| "no footprint read".into()),
        );
        return line_section(&name, "Biggest", line, true);
    }
    list_section(
        c,
        name,
        subs,
        rows,
        None,
        if c.two { 4 } else { 3 },
        "Biggest",
        line,
        true,
    )
}

/// Swap: used, and its traffic.
fn swap_section(c: Ctx<'_>) -> Section<'_> {
    let snap = c.snap;
    let forms = match snap.mem.as_ref() {
        None => vec!["not read yet".to_owned()],
        Some(m) => {
            let used = m.sample.swap_used;
            let used_w = if used == 0 {
                "none used".to_owned()
            } else {
                format!("{} used", words::mem(used))
            };
            match snap.facts.swap_rate {
                Some(r) if r > 0.0 => vec![
                    format!("{used_w} \u{b7} {} in and out", words::rate_mb(r)),
                    used_w,
                ],
                Some(_) => vec![format!("{used_w} \u{b7} no traffic"), used_w],
                None => vec![used_w],
            }
        }
    };
    line_section("Swap", "Swap", forms, true)
}

/// What the pressure word means.
fn pressure_section(c: Ctx<'_>) -> Section<'_> {
    let forms = match c.snap.mem.as_ref().map(|m| m.sample.pressure) {
        Some(Pressure::Normal) => vec![
            "normal: memory is not short; compressing and the file cache give way first".into(),
            "normal: memory is not short".into(),
        ],
        Some(Pressure::Warn) => vec![
            "warn: macOS is compressing and swapping to keep up".into(),
            "warn: compressing and swapping".into(),
        ],
        Some(Pressure::Critical) => vec![
            "critical: memory is short, and macOS may ask apps to quit".into(),
            "critical: memory is short".into(),
        ],
        Some(Pressure::Unknown) | None => vec!["not read here".into()],
    };
    line_section("Pressure", "Pressure", forms, true)
}

/// The Processes lens (§11.4).
fn processes_doc(c: Ctx<'_>) -> LensDoc<'_> {
    let snap = c.snap;
    let threads = c.loc.platform == Platform::Linux;
    let unit = if threads { "threads" } else { "processes" };
    let at = c.at();
    let explainer = format!(
        "Heat is how close the count of {unit} is to the limit that binds first: {}'s, or the \
         one on your user.",
        c.loc.platform.machine()
    );
    let now = snap.at_unix_ms;
    let vals: Vec<Option<f64>> = c
        .view(Resource::Processes)
        .columns(60, 60, now)
        .iter()
        .map(|cell| cell.mean)
        .collect();
    let trace = Trace {
        vals,
        under: HOUR,
        min_span: 5.0,
        mark: None,
    };
    let start = (now.div_euclid(60_000) - 59) * 60_000;
    let said = trace_said(
        &format!("Running over the last hour, {unit}"),
        unit,
        &trace,
        (start, 60_000),
        &c.loc,
    );
    let first = trace.vals.iter().flatten().next().copied();
    let last = trace.vals.iter().rev().flatten().next().copied();
    let line = match (first, last) {
        (Some(a), Some(b)) => vec![format!(
            "{} to {} {unit}",
            words::grouped(a.round() as u64),
            words::grouped(b.round() as u64)
        )],
        _ => vec!["not read yet".to_owned()],
    };
    let sections = vec![
        counts_section(c, unit),
        trace_section(
            c,
            "Last hour",
            vec![format!("{unit} running, the minute's mean")],
            trace,
            said,
            "Last hour",
            line,
            false,
        ),
        jobs_section(c),
    ];
    LensDoc {
        name: "Processes",
        sources: vec![format!("as of {at}")],
        explainer,
        answer: c.now_answer(Resource::Processes, "the limit"),
        sections,
        two_order: Vec::new(),
        keeps_first: false,
    }
}

/// Counts against limits.
fn counts_section<'d>(c: Ctx<'d>, unit: &'static str) -> Section<'d> {
    let snap = c.snap;
    let f = &snap.facts;
    let (system, user) = f.pid_counts;
    let limits = f.proc_limits;
    let of = |n: Option<u64>, l: Option<u64>| match (n, l) {
        (Some(n), Some(l)) => format!("{} of {}", words::grouped(n), words::grouped(l)),
        (Some(n), None) => words::grouped(n),
        (None, _) => "not read".into(),
    };
    let share = |n: Option<u64>, l: Option<u64>| {
        n.zip(l)
            .filter(|(_, l)| *l > 0)
            .map(|(n, l)| n as f64 / l as f64)
    };
    let view = c.view(Resource::Processes);
    let machine = c.loc.platform.machine();
    let mut rows = vec![
        Row {
            chip: Some(ProcScope::Other),
            name: machine.into(),
            scope: None,
            share: share(system, limits.system),
            figure: of(system, limits.system),
        },
        Row {
            chip: Some(ProcScope::Other),
            name: "yours".into(),
            scope: None,
            share: share(user, limits.per_user),
            figure: of(user, limits.per_user),
        },
    ];
    if let Some(n) = view.aterm.now {
        rows.push(Row {
            chip: Some(ProcScope::Aterm),
            name: "aterm".into(),
            scope: None,
            share: share(Some(n as u64), limits.system),
            figure: words::grouped(n.round() as u64),
        });
    }
    if let (Some(n), Some(&k)) = (view.tab.now, c.panes.first()) {
        rows.push(Row {
            chip: Some(ProcScope::Tab(k)),
            name: c.tab_label.to_owned(),
            scope: None,
            share: share(Some(n as u64), limits.system),
            figure: words::grouped(n.round() as u64),
        });
    }
    let line = vec![
        format!(
            "{} \u{b7} yours {}",
            of(system, limits.system),
            of(user, limits.per_user)
        ),
        of(system, limits.system),
    ];
    list_section(
        c,
        "Counts".into(),
        vec![format!("{unit} running, against their limits"), unit.into()],
        rows,
        None,
        4,
        "Counts",
        line,
        false,
    )
}

/// The jobs list: every job in the chosen scope that is not fine.
fn jobs_section(c: Ctx<'_>) -> Section<'_> {
    use aterm_vitals::jobs::{Verdict, wire::person_name};
    let snap = c.snap;
    let name = format!("Jobs{}", c.suffix());
    let Some(view) = snap.jobs.as_deref() else {
        return line_section(&name, "Jobs", vec!["reading the jobs\u{2026}".into()], true);
    };
    let tab_ok = |tab: u64| match c.bar {
        Bar::Tab => c.panes.contains(&tab),
        Bar::Global | Bar::Aterm => true,
    };
    let tab_word = |key: u64| {
        snap.tab(key)
            .map_or_else(|| "a tab".to_owned(), |t| t.label.clone())
    };
    let mut lines: Vec<(WellMark, f32, String, String, String)> = Vec::new();
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
        .filter(|r| tab_ok(r.tab))
        .filter_map(|r| rank(r.verdict).map(|k| (k, r)))
        .collect();
    rows.sort_by_key(|((order, _), r)| (*order, std::cmp::Reverse(r.quiet_ms.unwrap_or(0))));
    for ((_, mark), r) in rows {
        let frac = r
            .phi_milli
            .map_or(crate::stats_paint::QUIET_ARC, crate::stats_paint::quiet_arc);
        let short = match r.quiet_ms {
            Some(q) if matches!(r.verdict, Verdict::Stuck | Verdict::Quiet) => format!(
                "{} {}",
                r.verdict.word(),
                aterm_vitals::jobs::wire::age_words(q)
            ),
            _ => r.verdict.word().to_owned(),
        };
        lines.push((
            mark,
            frac,
            format!("{} {}", tab_word(r.tab), person_name(&r.exe)),
            r.said(),
            short,
        ));
    }
    for z in view
        .findings
        .zombies
        .iter()
        .filter(|z| z.tab.is_some_and(tab_ok))
    {
        lines.push((
            WellMark::Zombies,
            1.0,
            format!(
                "{} {}",
                z.tab.map_or_else(String::new, tab_word),
                person_name(&z.name)
            ),
            format!("{} unreaped children of pid {}", z.count, z.ppid),
            format!("{} zombies", z.count),
        ));
    }
    lines.sort_by_key(|(mark, ..)| *mark);
    let marks = crate::stats_paint::marks_of(view);
    let summary = if marks.is_empty() {
        "nothing stuck, quiet or waiting for you".to_owned()
    } else {
        marks
            .iter()
            .map(|m| m.words())
            .collect::<Vec<_>>()
            .join(", ")
    };
    if lines.is_empty() {
        return line_section(
            &name,
            "Jobs",
            vec!["nothing stuck, quiet or waiting for you".into()],
            true,
        );
    }
    let words: Vec<String> = lines
        .iter()
        .map(|(m, _, who, said, _)| format!("{} {who}: {said}", m.word()))
        .collect();
    let lines = std::rc::Rc::new(lines);
    let draw_jobs = {
        let lines = std::rc::Rc::clone(&lines);
        let name = name.clone();
        move |pen: &mut Pen<'_>, p: Place| {
            heading(pen, p, &name, &[]);
            let inks = pen.inks;
            let room = (((p.h - 14.0) / PITCH).floor().max(1.0)) as usize;
            let more = lines.len().saturating_sub(room);
            let shown = if more > 0 {
                room.saturating_sub(1)
            } else {
                room
            };
            for (i, (mark, frac, who, said, short)) in lines.iter().take(shown).enumerate() {
                let base = p.y + ROW0_BASE + PITCH * i as f64;
                let ink = match mark {
                    WellMark::Zombies => inks.warn_text,
                    _ => inks.stuck,
                };
                pen.mark(*mark, p.x + 4.0, base - 3.4, BODY_PT, *frac, ink);
                let who = pen.cut(who, (p.w - 14.0) * 0.4, BODY_PT, false);
                let ww = pen.text(p.x + 14.0, base, &who, BODY_PT, false, inks.words);
                let room = p.w - 14.0 - ww - 8.0;
                let s = fit_cut(pen.m, &[said.clone(), short.clone()], room, BODY_PT, false);
                pen.text(p.x + 14.0 + ww + 8.0, base, &s, BODY_PT, false, inks.words2);
            }
            if more > 0 {
                let base = p.y + ROW0_BASE + PITCH * shown as f64;
                let s = format!("+{} more (aterm ctl sys jobs)", lines.len() - shown);
                let s = pen.cut(&s, p.w, NOTE_PT, false);
                pen.text(p.x, base, &s, NOTE_PT, false, inks.words2);
            }
        }
    };
    let n = lines.len();
    let mut forms = Vec::new();
    if c.two {
        forms.push(grow(rows_h(1, false), draw_jobs));
    } else {
        let draw = std::rc::Rc::new(draw_jobs);
        let mut counts = vec![n.min(4), n.min(2)];
        counts.dedup();
        for k in counts {
            let draw = std::rc::Rc::clone(&draw);
            let rows = if k < n { k + 1 } else { k };
            forms.push(form(rows_h(rows, false), move |pen, p| draw(pen, p)));
        }
    }
    let line = vec![summary];
    forms.push(line_form("Jobs", line));
    Section {
        name,
        words,
        right: true,
        parts: Vec::new(),
        forms,
    }
}

/// How to read (§11.4).
fn how_to_read(c: Ctx<'_>) -> LensDoc<'_> {
    let m = c.m;
    let cw = c.cw;
    let mut sections = Vec::new();
    let heat_words = ["cold, under 5% of the maximum", "under 5%"];
    let hot_words = ["hot, 95% and over", "95%+"];
    sections.push(Section {
        name: "Heat".into(),
        words: vec![
            "A cell's colour is its share of the row's maximum: cold under 5 percent, hot at \
             95 percent and over."
                .into(),
        ],
        right: false,
        parts: Vec::new(),
        forms: vec![
            form(38.0, move |pen, p| {
                heading(
                    pen,
                    p,
                    "Heat",
                    &[
                        "a cell's colour: its share of the row's maximum".into(),
                        "share of max".into(),
                    ],
                );
                let sw = ((p.w.min(280.0) + 2.0) / 7.0 - 2.0).max(4.0);
                for k in 0..7 {
                    let ink = if pen.ic {
                        pen.inks.heat_ic[k]
                    } else {
                        pen.inks.heat[k]
                    };
                    pen.rect(
                        p.x + k as f64 * (sw + 2.0),
                        p.y + 16.0,
                        sw,
                        8.0,
                        1.0,
                        rgba(ink, 0xFF),
                    );
                }
                let ramp_w = 7.0 * (sw + 2.0) - 2.0;
                let base = p.y + 35.0;
                let words2 = pen.inks.words2;
                for (cold, hot) in heat_words.iter().zip(&hot_words) {
                    let (wa, wb) = (
                        pen.width(cold, AXIS_PT, false),
                        pen.width(hot, AXIS_PT, false),
                    );
                    if wa + wb + 8.0 <= ramp_w.min(p.w) + EPS {
                        pen.text(p.x, base, cold, AXIS_PT, false, words2);
                        pen.text_end(p.x + ramp_w, base, hot, AXIS_PT, false, words2);
                        break;
                    }
                }
            }),
            line_form(
                "Heat",
                vec![
                    "cold under 5% to hot at 95% of max".into(),
                    "share of max".into(),
                ],
            ),
        ],
    });
    let cells: Vec<(u8, [String; 2])> = vec![
        (
            0,
            [
                "a cap above a cell: the resource ran over its maximum".into(),
                "a cap: over max".into(),
            ],
        ),
        (
            1,
            [
                "an outlined cell, or a short line: not read".into(),
                "outline: not read".into(),
            ],
        ),
        (
            2,
            [
                "the % at a row's right: now, the last 10 seconds".into(),
                "%: now".into(),
            ],
        ),
    ];
    let cells_words: Vec<String> = cells.iter().map(|(_, w)| w[0].clone()).collect();
    sections.push(Section {
        name: "Cells".into(),
        words: cells_words,
        right: false,
        parts: Vec::new(),
        forms: vec![
            form(rows_h(3, false), move |pen, p| {
                heading(
                    pen,
                    p,
                    "Cells",
                    &[
                        "10 seconds each, 10 minutes a row".into(),
                        "10 s each".into(),
                    ],
                );
                let inks = pen.inks;
                for (i, (kind, forms)) in cells.iter().enumerate() {
                    let base = p.y + ROW0_BASE + PITCH * i as f64;
                    match kind {
                        0 => {
                            let ink = pen.heat(0.7);
                            pen.rect(p.x, base - 7.0, 8.0, 7.0, 0.0, rgba(ink, 0xFF));
                            pen.rect(p.x, base - 10.0, 8.0, 1.5, 0.0, rgba(inks.over, 0xFF));
                        }
                        1 => pen.hollow(p.x, base - 7.0, 8.0, 7.0, rgba(inks.hollow, 0xFF)),
                        _ => {
                            let ink = pen.heat(0.97);
                            pen.rect(p.x, base - 7.0, 8.0, 7.0, 0.0, rgba(ink, 0xFF));
                        }
                    }
                    let s = fit_cut(pen.m, forms, p.w - 14.0, BODY_PT, false);
                    pen.text(p.x + 14.0, base, &s, BODY_PT, false, inks.words2);
                }
            }),
            line_form(
                "Cells",
                vec!["10 s each \u{b7} a cap: over max \u{b7} an outline: not read".into()],
            ),
        ],
    });
    let machine = c.loc.platform.machine();
    let mut said = format!("{machine} in grey, aterm in green, this tab in bright green.");
    if let Some(first) = said.get(..1) {
        said = first.to_uppercase() + said.get(1..).unwrap_or_default();
    }
    sections.push(Section {
        name: "Scopes".into(),
        words: vec![said],
        right: true,
        parts: Vec::new(),
        forms: vec![
            form(rows_h(1, false), move |pen, p| {
                heading(
                    pen,
                    p,
                    "Scopes",
                    &[
                        "the bar nests them; up and down choose a lens's scope".into(),
                        "nested".into(),
                    ],
                );
                let base = p.y + ROW0_BASE;
                let mut x = p.x;
                let items = [
                    (pen.machine(), machine),
                    (chip_ink(pen, ProcScope::Aterm, &[]), "aterm"),
                    (chip_ink(pen, ProcScope::Tab(0), &[0]), "this tab"),
                ];
                let words = pen.inks.words;
                for (ink, word) in items {
                    let w = pen.width(word, BODY_PT, false);
                    if x + 14.0 + w > p.x + p.w {
                        break;
                    }
                    pen.rect(x, base - 7.0, 10.0, 7.0, 1.0, rgba(ink, 0xFF));
                    pen.text(x + 14.0, base, word, BODY_PT, false, words);
                    x += 14.0 + w + 14.0;
                }
            }),
            line_form(
                "Scopes",
                vec![
                    format!("this tab within aterm within {machine}"),
                    format!("tab, aterm, {machine}"),
                ],
            ),
        ],
    });
    let marks: [(WellMark, &str); 7] = [
        (WellMark::Stuck, "stuck"),
        (WellMark::Quiet, "quiet"),
        (WellMark::Waiting, "waiting for you"),
        (WellMark::Zombies, "zombies"),
        (WellMark::Low, "disk low"),
        (WellMark::Dot, "memory pressure, a GPU reset"),
        (WellMark::Cause, "the cause aterm names"),
    ];
    let item_w = |w: &str| 14.0 + m.width(w, BODY_PT, false) + 12.0;
    let mut mark_rows = 1;
    let mut x = 0.0;
    for (_, w) in &marks {
        if x > 0.0 && x + item_w(w) > cw {
            mark_rows += 1;
            x = 0.0;
        }
        x += item_w(w);
    }
    sections.push(Section {
        name: "Marks".into(),
        words: vec![
            "Stuck, quiet, waiting for you, zombies, disk low, memory pressure or a GPU reset, \
             and the cause aterm names."
                .into(),
        ],
        right: true,
        parts: Vec::new(),
        forms: vec![
            form(rows_h(mark_rows, false), move |pen, p| {
                heading(
                    pen,
                    p,
                    "Marks",
                    &[
                        "a row's state beside its name, or its cause".into(),
                        "a row's state or cause".into(),
                    ],
                );
                let inks = pen.inks;
                let (mut x, mut row) = (0.0, 0usize);
                for (kind, w) in &marks {
                    let iw = 14.0 + pen.width(w, BODY_PT, false) + 12.0;
                    if x > 0.0 && x + iw > p.w {
                        row += 1;
                        x = 0.0;
                    }
                    let base = p.y + ROW0_BASE + PITCH * row as f64;
                    let ink = match kind {
                        WellMark::Zombies | WellMark::Low | WellMark::Dot => inks.warn_text,
                        WellMark::Cause => inks.words2,
                        _ => inks.stuck,
                    };
                    pen.mark(
                        *kind,
                        p.x + x + 5.0,
                        base - 3.4,
                        BODY_PT,
                        crate::stats_paint::QUIET_ARC,
                        ink,
                    );
                    let s = pen.cut(w, p.w - x - 14.0, BODY_PT, false);
                    pen.text(p.x + x + 14.0, base, &s, BODY_PT, false, inks.words2);
                    x += iw;
                }
            }),
            line_form(
                "Marks",
                vec!["stuck, quiet, waiting, zombies, low, pressure, cause".into()],
            ),
        ],
    });
    let gestures: Vec<String> = vec![
        "rest on a row for its close-up; click to pin it".into(),
        "command-I opens it from the keyboard".into(),
        "in it, the arrows choose the row and the scope; esc closes".into(),
    ];
    let lines: Vec<String> = gestures
        .iter()
        .flat_map(|g| wrap(m, g, cw, BODY_PT))
        .collect();
    let mut forms = Vec::new();
    for k in (1..=lines.len()).rev() {
        let shown = lines[..k].to_vec();
        forms.push(form(rows_h(k, false), move |pen, p| {
            heading(pen, p, "Gestures", &[]);
            let ink = pen.inks.words2;
            for (i, l) in shown.iter().enumerate() {
                pen.text(
                    p.x,
                    p.y + ROW0_BASE + PITCH * i as f64,
                    l,
                    BODY_PT,
                    false,
                    ink,
                );
            }
        }));
    }
    forms.push(line_form("Gestures", vec![gestures[0].clone()]));
    sections.push(Section {
        name: "Gestures".into(),
        words: gestures,
        right: true,
        parts: Vec::new(),
        forms,
    });
    LensDoc {
        name: "How to read",
        sources: vec!["the grammar every row shares".into()],
        explainer: "Each row is one resource over the last 10 minutes, oldest on the left. \
                    Each cell is 10 seconds (20 or 30 in a narrow deck), coloured by its share \
                    of the resource's maximum."
            .into(),
        answer: None,
        sections,
        two_order: Vec::new(),
        keeps_first: false,
    }
}

/// A text-only well (§11.4).
fn text_well(c: Ctx<'_>, focus: Instrument) -> LensDoc<'_> {
    let snap = c.snap;
    let res = focus.resource().unwrap_or(Resource::Network);
    let at = c.at();
    let (name, explainer) = match res {
        Resource::Network => (
            "Network",
            "Heat is the busier direction's rate as a share of its 7-day peak, never above the \
             link's rate.",
        ),
        Resource::Tokens => (
            "Tokens",
            "Heat is aterm's output tokens per minute as a share of aterm's busiest 30 seconds \
             in the last 7 days.",
        ),
        _ => ("GPU", "Heat is how much of the time the GPU was busy."),
    };
    let details = words::details(res, snap, &c.loc);
    let mut sentence = words::row_sentence(res, snap, c.panes, &c.loc);
    for d in &details {
        if let Some(i) = sentence.find(&d.spoken) {
            sentence.replace_range(i..i + d.spoken.len(), "");
        }
    }
    let sentence = sentence.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut lines: Vec<String> = details
        .iter()
        .filter_map(|d| d.forms.first().cloned())
        .collect();
    let f = &snap.facts;
    match res {
        Resource::Network => {
            if let Some(net) = snap.net.as_ref() {
                if let Some(tcp) = net.tcp {
                    let mut states = Vec::new();
                    if tcp.time_wait > 0 {
                        states.push(format!(
                            "{} time-wait",
                            words::grouped(u64::from(tcp.time_wait))
                        ));
                    }
                    if tcp.close_wait > 0 {
                        states.push(format!(
                            "{} close-wait",
                            words::grouped(u64::from(tcp.close_wait))
                        ));
                    }
                    if tcp.connecting() > 0 {
                        states.push(format!(
                            "{} connecting",
                            words::grouped(u64::from(tcp.connecting()))
                        ));
                    }
                    if !states.is_empty() {
                        lines.push(format!("TCP states: {}", states.join(" \u{b7} ")));
                    }
                }
                if let Some(r) = net.rates {
                    let errs = r.rx_err_ps + r.tx_err_ps;
                    let drops = r.rx_drop_ps + r.tx_drop_ps.unwrap_or(0.0);
                    if errs > 0.0 || drops > 0.0 {
                        lines.push(format!("errors {errs:.1}/s \u{b7} drops {drops:.1}/s"));
                    }
                }
            }
            let counted = f.link.as_ref().and_then(|link| match link.kind {
                LinkKind::Vpn => Some(format!(
                    "{}: VPN, its bytes are counted on the link that carries them",
                    link.name
                )),
                LinkKind::Bridge => Some(format!(
                    "{}: a bridge, its members' bytes are counted on them",
                    link.name
                )),
                LinkKind::Wifi | LinkKind::Ethernet | LinkKind::Cellular | LinkKind::Other => None,
            });
            lines.extend(counted);
            lines.push("no per-app network figure".into());
        }
        Resource::Tokens => {
            match f.plan.as_ref() {
                Some(p) => {
                    let mut w = p.window_word.clone();
                    if let Some(first) = w.get(..1) {
                        w = first.to_uppercase() + w.get(1..).unwrap_or_default();
                    }
                    lines.push(format!(
                        "{w} limit {:.0}% used, read at {}",
                        f64::from(p.used_x10) / 10.0,
                        words::clock(p.read_at, snap.at_unix_ms, c.loc.utc_offset_s)
                    ));
                }
                None => {
                    lines.retain(|l| !l.starts_with("plan limit"));
                    lines.push("No current plan reading on this machine".into());
                }
            }
            lines.push("Codex, Gemini and agy tabs are not counted".into());
        }
        _ => {
            if let Some(g) = f.gpu.as_ref() {
                let stages: Vec<String> = [("renderer", g.renderer_pct), ("tiler", g.tiler_pct)]
                    .into_iter()
                    .filter_map(|(w, p)| p.map(|p| format!("{w} {p}%")))
                    .collect();
                if !stages.is_empty() {
                    lines.push(stages.join(" \u{b7} "));
                }
                if let Some(a) = g.alloc_bytes {
                    lines.push(format!("{} GPU memory allocated", words::mem(a)));
                }
            }
            match f.gpu_resets {
                Some((n, since)) => lines.push(format!(
                    "reset {n}\u{d7} since {}",
                    words::clock(since, snap.at_unix_ms, c.loc.utc_offset_s)
                )),
                None => lines.push(f.gpu.as_ref().and_then(|g| g.recoveries).map_or_else(
                    || "GPU recovery counter not read".to_owned(),
                    |count| format!("GPU recovery counter observed: {count}"),
                )),
            }
            if snap.resources[Resource::Gpu.index()].aterm.missing
                == Some(aterm_vitals::NoFigure::NotPerApp)
            {
                lines.push(format!(
                    "no per-app GPU figure on {}",
                    c.loc.platform.name()
                ));
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    lines.retain(|l| seen.insert(l.clone()));
    let now_lines = wrap(c.m, &sentence, c.cw, BODY_PT);
    let sections = vec![
        lines_section("Now", now_lines, vec![sentence.clone()], false),
        lines_section("Details", lines.clone(), lines, true),
    ];
    LensDoc {
        name,
        sources: vec![format!("as of {at}")],
        explainer: explainer.into(),
        answer: None,
        sections,
        two_order: Vec::new(),
        keeps_first: false,
    }
}

/// A heading over `lines`, one a row.
fn lines_section<'d>(
    name: &'static str,
    lines: Vec<String>,
    words: Vec<String>,
    right: bool,
) -> Section<'d> {
    let mut forms = Vec::new();
    for k in (1..=lines.len()).rev() {
        let shown = lines[..k].to_vec();
        forms.push(form(rows_h(k, false), move |pen, p| {
            heading(pen, p, name, &[]);
            let ink = pen.inks.words2;
            for (i, l) in shown.iter().enumerate() {
                let s = pen.cut(l, p.w, BODY_PT, false);
                pen.text(
                    p.x,
                    p.y + ROW0_BASE + PITCH * i as f64,
                    &s,
                    BODY_PT,
                    false,
                    ink,
                );
            }
        }));
    }
    let line: Vec<String> = lines.first().cloned().into_iter().collect();
    forms.push(line_form(name, line));
    Section {
        name: name.into(),
        words,
        right,
        parts: Vec::new(),
        forms,
    }
}

/// A key hint's pieces.
#[derive(Clone, Copy, Debug)]
enum Seg {
    T(&'static str),
    Across,
    UpDown,
}

/// The key hint's forms for a lens opened `how`, longest first (§11.3).
const fn hints(how: Opened) -> &'static [&'static [Seg]] {
    use Seg::{Across, T, UpDown};
    match how {
        Opened::Pinned => &[
            &[
                T("pinned \u{b7} "),
                Across,
                T("resources \u{b7} "),
                UpDown,
                T("scope \u{b7} esc closes"),
            ],
            &[T("pinned \u{b7} esc closes")],
            &[T("esc closes")],
        ],
        Opened::Keyboard => &[
            &[
                Across,
                T("resources \u{b7} "),
                UpDown,
                T("scope \u{b7} esc closes"),
            ],
            &[Across, T("resources \u{b7} esc closes")],
            &[T("esc closes")],
        ],
        Opened::Hover => &[&[T("click pins it")], &[T("click pins")]],
    }
}

fn hint_w(m: &FaceMetrics, hint: &[Seg]) -> f64 {
    hint.iter()
        .map(|s| match s {
            Seg::T(t) => m.width(t, NOTE_PT, false),
            Seg::Across => 20.0,
            Seg::UpDown => 16.0,
        })
        .sum()
}

/// Draw the header while it clears the foot and leaves `reserve` pt of body.
fn head(pen: &mut Pen<'_>, doc: &LensDoc<'_>, how: Opened, lw: f64, lh: f64, reserve: f64) -> f64 {
    let m = pen.m;
    let inks = pen.inks;
    let limit = lh - FOOT;
    if LINE1_BASE + DESCENT_EM * NAME_PT > limit {
        return lh;
    }
    let right = lw - SIDE;
    let name = pen.cut(doc.name, right - SIDE, NAME_PT, true);
    let nw = pen.text(SIDE, LINE1_BASE, &name, NAME_PT, true, inks.words);
    let tx = SIDE + nw + 8.0;
    let hints = hints(how);
    let mut chosen: Option<(usize, Option<usize>)> = None;
    'hints: for (hi, hint) in hints.iter().enumerate() {
        let hw = hint_w(m, hint);
        for (si, sub) in doc.sources.iter().enumerate() {
            if tx + m.width(sub, BODY_PT, false) + 12.0 + hw <= right + EPS {
                chosen = Some((hi, Some(si)));
                break 'hints;
            }
        }
    }
    if chosen.is_none() {
        chosen = hints
            .iter()
            .position(|h| tx + hint_w(m, h) <= right + EPS)
            .map(|hi| (hi, None));
    }
    if let Some((hi, si)) = chosen {
        if let Some(sub) = si.and_then(|si| doc.sources.get(si)) {
            pen.text(tx, LINE1_BASE, sub, BODY_PT, false, inks.words2);
        }
        let hint = hints[hi];
        let mut x = right - hint_w(m, hint);
        for seg in hint {
            x += match seg {
                Seg::T(t) => pen.text(x, LINE1_BASE, t, NOTE_PT, false, inks.words2),
                Seg::Across => pen.arrows(x, LINE1_BASE, true),
                Seg::UpDown => pen.arrows(x, LINE1_BASE, false),
            };
        }
    }
    let mut last = LINE1_BASE;
    let mut base = EXPLAINER_BASE;
    let fits = |base: f64| {
        base + DESCENT_EM * HEAD_PT <= limit && (reserve <= 0.0 || base + 12.0 + reserve <= limit)
    };
    let mut cut = false;
    let lines = wrap(m, &doc.explainer, right - SIDE, BODY_PT);
    let shown = draw_lines(pen, (SIDE, right), &mut base, &lines, inks.words, &fits);
    if shown > 0 {
        last = base - PITCH;
    }
    cut |= shown < lines.len();
    if let Some((label, text, kind)) = doc.answer.as_ref().filter(|_| !cut) {
        let label_w = m.width(label, HEAD_PT, true);
        let ax = SIDE + (label_w + 8.0).max(52.0);
        let lines = wrap(m, text, right - ax, BODY_PT);
        if !lines.is_empty() && fits(base) {
            pen.text(SIDE, base, label, HEAD_PT, true, inks.words);
            let ink = kind_ink(&inks, *kind);
            let shown = draw_lines(pen, (ax, right), &mut base, &lines, ink, &fits);
            if shown > 0 {
                last = base - PITCH;
            }
            cut |= shown < lines.len();
        }
    }
    if cut && reserve <= 0.0 {
        return lh;
    }
    let rule = last + 7.0;
    if rule + 5.0 >= limit {
        return lh;
    }
    pen.rule(SIDE, rule, right - SIDE, rgba(inks.grid, 0xFF));
    rule + 5.0
}

/// A wrapped paragraph's `lines` from `x`, one a pitch from `*base`, while `fits` the baseline.
fn draw_lines(
    pen: &mut Pen<'_>,
    (x, right): (f64, f64),
    base: &mut f64,
    lines: &[String],
    ink: Rgb,
    fits: &dyn Fn(f64) -> bool,
) -> usize {
    let mut shown = 0;
    while shown < lines.len() && fits(*base) {
        let line = match lines.get(shown + 1) {
            Some(next) if !fits(*base + PITCH) => pen.cut(
                &format!("{} {next}", lines[shown]),
                right - x,
                BODY_PT,
                false,
            ),
            _ => lines[shown].clone(),
        };
        pen.text(x, *base, &line, BODY_PT, false, ink);
        shown += 1;
        *base += PITCH;
    }
    shown
}

/// A laid lens: its prims, what a reader hears, and how its body went.
pub(crate) struct LensLaid {
    pub(crate) prims: Vec<DrawPrim>,
    /// How many of `prims`, from the first, draw the shell (none when not drawn).
    pub(crate) shell: usize,
    /// "`<Name>` close-up".
    pub(crate) title: String,
    /// The explainer.
    pub(crate) explainer: String,
    /// The answer's and each placed section's heading and lines.
    pub(crate) wells: Vec<(String, String)>,
    /// Each column's sections in the order it stacks them.
    pub(crate) stacks: Vec<Vec<usize>>,
    /// Each column's placed sections, top down.
    pub(crate) placed: Vec<Vec<(usize, usize)>>,
    #[cfg(test)]
    pub(crate) boxes: Vec<TextBox>,
}

/// Lay the lens `lens` on `lg` from `input`, measuring its words with `m`.
pub(crate) fn lay_lens(
    lg: &LensGeom,
    input: &DeckInput<'_>,
    lens: &Lens,
    m: &FaceMetrics,
    draw: bool,
) -> LensLaid {
    let mut inks = input.inks;
    if input.contrast {
        inks.hollow = inks.words2;
        inks.rim = inks.words2;
    }
    let metrics = m;
    let s = f64::from(lg.scale.max(0.5));
    let mut pen = Pen {
        p: Painter::new(&inks, lg.scale, lg.font_px, input.dwc),
        m: metrics,
        inks,
        ic: input.contrast,
        s,
        ox: lg.margin,
        oy: lg.margin,
        draw,
        line_tab: 0.0,
        #[cfg(test)]
        boxes: Vec::new(),
    };
    if draw {
        pen.shell(lg);
    }
    let shell = pen.p.prims.len();
    let (lw, lh) = lg.shell_pt();
    let (two, cw) = columns(lw);
    let mut laid = LensLaid {
        prims: Vec::new(),
        shell,
        title: format!("{} close-up", lens.focus.word()),
        explainer: String::new(),
        wells: Vec::new(),
        stacks: Vec::new(),
        placed: Vec::new(),
        #[cfg(test)]
        boxes: Vec::new(),
    };
    match input.snap {
        None => {
            let doc = LensDoc {
                name: lens.focus.word(),
                sources: Vec::new(),
                explainer: "Reading the machine\u{2026}".into(),
                answer: None,
                sections: Vec::new(),
                two_order: Vec::new(),
                keeps_first: false,
            };
            let _ = head(&mut pen, &doc, lens.how, lw, lh, 0.0);
            laid.explainer = doc.explainer;
        }
        Some(snap) => {
            let c = Ctx {
                snap,
                m: metrics,
                panes: &input.tab_panes,
                tab_label: &input.tab_label,
                loc: input.locale,
                bar: lens.bar,
                two,
                cw,
            };
            let doc = doc_for(c, lens.focus);
            let reserve = doc
                .sections
                .first()
                .filter(|_| doc.keeps_first)
                .and_then(|s| s.forms.last())
                .map_or(0.0, |f| f.h);
            let top = head(&mut pen, &doc, lens.how, lw, lh, reserve);
            let placed = lens_body(
                lw,
                top,
                lh - FOOT,
                &doc.sections,
                (&doc.two_order, doc.keeps_first),
            );
            let tab_of = |x: f64| {
                placed
                    .iter()
                    .filter(|pl| pl.place.x == x)
                    .filter_map(|pl| doc.sections[pl.section].forms[pl.form].label.as_deref())
                    .map(|l| tab_after(metrics.width(l, HEAD_PT, true)))
                    .fold(0.0, f64::max)
            };
            for pl in &placed {
                let sec = &doc.sections[pl.section];
                pen.line_tab = tab_of(pl.place.x);
                (sec.forms[pl.form].draw)(&mut pen, pl.place);
            }
            if let Some((label, text, _)) = &doc.answer {
                laid.wells.push(((*label).to_owned(), text.clone()));
            }
            for pl in &placed {
                let sec = &doc.sections[pl.section];
                laid.wells.push((sec.name.clone(), sec.words.join(" ")));
                laid.wells.extend(sec.parts.iter().cloned());
            }
            let n = doc.sections.len();
            laid.stacks = if two {
                let order: Vec<usize> = if doc.two_order.is_empty() {
                    (0..n).collect()
                } else {
                    doc.two_order.clone()
                };
                [false, true]
                    .into_iter()
                    .map(|right| {
                        order
                            .iter()
                            .copied()
                            .filter(|&i| doc.sections[i].right == right)
                            .collect()
                    })
                    .collect()
            } else {
                vec![(0..n).collect()]
            };
            laid.placed = vec![Vec::new(); laid.stacks.len()];
            for pl in &placed {
                let col = usize::from(two && pl.place.x > SIDE + EPS);
                laid.placed[col].push((pl.section, pl.form));
            }
            laid.explainer = doc.explainer;
        }
    }
    #[cfg(test)]
    {
        laid.boxes = std::mem::take(&mut pen.boxes);
    }
    laid.prims = pen.p.prims;
    laid
}

/// Paint the lens: its prims (card pixels).
#[cfg(test)]
pub(crate) fn lens_paint(lg: &LensGeom, input: &DeckInput<'_>, lens: &Lens) -> Vec<DrawPrim> {
    lay_lens(lg, input, lens, &deck_metrics(), true).prims
}

impl crate::App {
    /// The lens card's geometry for `wid`.
    pub(crate) fn lens_frame(
        &self,
        wid: crate::WindowId,
    ) -> Option<(LensGeom, Lens, crate::stats_deck::DeckFrame)> {
        let lens = self.windows.get(&wid)?.stats_lens.open?;
        let f = self.deck_frame(wid)?;
        let lg = self.lens_geom_for(wid, &f)?;
        Some((lg, lens, f))
    }

    /// Where a lens would hang in `wid` from the form `f` draws.
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

    /// Whether `wid` can show a lens now.
    pub(crate) fn stats_lens_fits(&self, wid: crate::WindowId) -> bool {
        self.deck_frame(wid)
            .is_some_and(|f| self.lens_geom_for(wid, &f).is_some())
    }

    /// The lens's input for `wid`.
    fn lens_input(&self, f: &crate::stats_deck::DeckFrame) -> DeckInput<'_> {
        DeckInput {
            snap: self.vitals.snapshot.as_deref(),
            tab_panes: f.tab_panes.clone(),
            tab_label: f.tab_label.clone(),
            tab_count: f.tab_count,
            inks: f.lens_inks,
            dwc: f.dwc,
            marks: self.stats_marks(),
            plate: None,
            form: f.deck,
            contrast: f.contrast,
            locale: Locale {
                platform: Platform::host(),
                utc_offset_s: crate::presence::local_offset_s().unwrap_or(0),
            },
            live: crate::stats_live::DeckLive::STILL,
        }
    }

    /// The open lens's fingerprint for `wid` (`0` when none).
    pub(crate) fn stats_lens_fp(&self, wid: crate::WindowId) -> u64 {
        use std::hash::{Hash, Hasher};
        let Some((lg, lens, f)) = self.lens_frame(wid) else {
            return 0;
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.vitals.snapshot.as_ref().map(|s| s.seq).hash(&mut h);
        lens.hash(&mut h);
        (lg.dx, lg.dy, lg.w.to_bits(), lg.h.to_bits()).hash(&mut h);
        f.lens_inks.hash(&mut h);
        f.tab_panes.hash(&mut h);
        f.tab_label.hash(&mut h);
        (f.dwc, f.contrast).hash(&mut h);
        std::sync::Arc::as_ptr(&deck_metrics()).hash(&mut h);
        h.finish().rotate_left(17) | 1
    }

    /// Rasterize the open lens into `wid`'s `lens_card`, only when its fingerprint moved: its body
    /// over the shell's raster, which is rasterized again only when the shell's prims move.
    pub(crate) fn splice_stats_lens(&mut self, wid: crate::WindowId) {
        let Some((lg, lens, f)) = self.lens_frame(wid) else {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.lens_card = None;
                ws.stats_lens.rect = None;
                ws.stats_lens.open = None;
                ws.stats_lens.leave_due = None;
                ws.stats_lens.shell = None;
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
        let input = self.lens_input(&f);
        let laid = lay_lens(&lg, &input, &lens, &deck_metrics(), true);
        let (shell, body) = laid.prims.split_at(laid.shell);
        let (w, h) = (lg.w as u32, lg.h as u32);
        let shell_fp = crate::stats_deck::prims_fp(shell) ^ (u64::from(w) << 32 | u64::from(h));
        let ground = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.stats_lens.shell.clone())
            .filter(|kept| kept.fp == shell_fp)
            .unwrap_or_else(|| {
                let (rgba, pw, ph) =
                    crate::tray_raster::rasterize_tray(shell, w, h, 1.0, [0, 0, 0, 0]);
                ShellRaster {
                    fp: shell_fp,
                    rgba: rgba.into(),
                    pw,
                    ph,
                }
            });
        let (pw, ph) = (ground.pw, ground.ph);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        // The outgoing card's buffer takes the shell's copy, so a reading allocates no new card.
        let mut px = ws
            .lens_card
            .take()
            .map(|card| card.rgba)
            .unwrap_or_default();
        px.clear();
        px.extend_from_slice(&ground.rgba);
        let rgba = crate::tray_raster::rasterize_tray_over(body, 1.0, px, pw, ph);
        ws.stats_lens.shell = Some(ground);
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

    /// The open lens laid out, not drawn, for the accessibility tree.
    #[cfg(any(a11y_tree, test))]
    pub(crate) fn stats_lens_words(
        &self,
        wid: crate::WindowId,
    ) -> Option<(LensGeom, Lens, LensLaid)> {
        let (lg, lens, f) = self.lens_frame(wid)?;
        let input = self.lens_input(&f);
        let laid = lay_lens(&lg, &input, &lens, &deck_metrics(), false);
        Some((lg, lens, laid))
    }

    /// Show `lens` in `wid`.
    fn set_stats_lens(&mut self, wid: crate::WindowId, lens: Lens) {
        let was = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.stats_lens.open)
            .map(|l| l.focus);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.stats_lens.open = Some(lens);
            ws.stats_lens.last = Some((lens.focus, lens.bar));
            ws.stats_lens.rest = None;
            ws.stats_lens.leave_due = None;
        }
        if lens.focus == Instrument::Disk && was != Some(Instrument::Disk) {
            self.disk_lens_opened();
        }
        self.request_window_redraw(wid);
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
        self.set_stats_lens(wid, Lens { focus, bar, how });
        self.vitals.next_due = Some(Instant::now());
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

    /// ⌘I (`ToggleStatsLens`).
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
        let (focus, bar) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.stats_lens.last)
            .unwrap_or((Instrument::Cpu, Bar::Global));
        self.open_stats_lens(wid, focus, bar, Opened::Keyboard);
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

    /// Pointer motion over the stats surfaces.
    pub(crate) fn stats_pointer_motion(&mut self, wid: crate::WindowId, now: Instant) -> bool {
        if self.windows.get(&wid).is_none_or(|ws| ws.overlay_open()) {
            return false;
        }
        if self.lens_card_point(wid).is_some() {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.stats_lens.leave_due = None;
                ws.stats_lens.rest = None;
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
        let open = ws.stats_lens.open;
        match hit {
            Some(hit) => {
                ws.stats_lens.leave_due = None;
                ws.stats_lens.rest = match open {
                    Some(l) if l.how != Opened::Hover => None,
                    Some(l) if l.focus == hit.instrument() && l.bar == hit.bar() => None,
                    _ => Some(Rest {
                        due: now + REST,
                        hit,
                    }),
                };
            }
            None => {
                ws.stats_lens.rest = None;
                if open.is_some_and(|l| l.how == Opened::Hover)
                    && ws.stats_lens.leave_due.is_none()
                    && !over_card
                {
                    ws.stats_lens.leave_due = Some(now + LEAVE);
                }
            }
        }
        owns
    }

    /// The pointer left the window.
    pub(crate) fn stats_pointer_left(&mut self, wid: crate::WindowId, now: Instant) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.stats_lens.rest = None;
            if ws.stats_lens.open.is_some_and(|l| l.how == Opened::Hover) {
                ws.stats_lens.leave_due = Some(now + LEAVE);
            }
        }
    }

    /// A press: on the lens it pins a hover lens.
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
            self.open_stats_lens(wid, hit.instrument(), hit.bar(), Opened::Pinned);
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

    /// The lens's keys.
    pub(crate) fn stats_lens_key(
        &mut self,
        wid: crate::WindowId,
        mods: aterm_window::keyboard::ModifiersState,
        key: &aterm_window::keyboard::Key,
        copy_chord: bool,
        inert: bool,
    ) -> bool {
        use aterm_window::keyboard::{Key, NamedKey};
        let Some(open) = self.windows.get(&wid).and_then(|ws| ws.stats_lens.open) else {
            return false;
        };
        if inert {
            return false;
        }
        let command = |c: &str| {
            mods.super_key() && matches!(key, Key::Character(k) if k.eq_ignore_ascii_case(c))
        };
        if command("i") {
            self.close_stats_lens(wid);
            return true;
        }
        if open.how != Opened::Hover {
            let next = match key {
                Key::Named(NamedKey::ArrowRight) => Some(Lens {
                    focus: step_instrument(open.focus, true),
                    ..open
                }),
                Key::Named(NamedKey::ArrowLeft) => Some(Lens {
                    focus: step_instrument(open.focus, false),
                    ..open
                }),
                Key::Named(NamedKey::ArrowDown) => Some(Lens {
                    bar: step_bar(open.bar, true),
                    ..open
                }),
                Key::Named(NamedKey::ArrowUp) => Some(Lens {
                    bar: step_bar(open.bar, false),
                    ..open
                }),
                Key::Named(NamedKey::Enter) if open.how == Opened::Keyboard => Some(Lens {
                    how: Opened::Pinned,
                    ..open
                }),
                _ => None,
            };
            if let Some(next) = next {
                self.set_stats_lens(wid, next);
                return true;
            }
            if matches!(key, Key::Named(NamedKey::Escape)) {
                self.close_stats_lens(wid);
                return true;
            }
            if command("c") || copy_chord {
                let _ = self.copy_stats_sentence(wid);
                return true;
            }
        }
        self.close_stats_lens(wid);
        false
    }

    /// The open lens's sentence.
    pub(crate) fn stats_lens_sentence(&self, wid: crate::WindowId) -> Option<String> {
        let open = self.windows.get(&wid).and_then(|ws| ws.stats_lens.open)?;
        let snap = self.vitals.snapshot.as_deref()?;
        let panes = self.deck_tab_panes(wid);
        let loc = Locale {
            platform: Platform::host(),
            utc_offset_s: crate::presence::local_offset_s().unwrap_or(0),
        };
        Some(lens_sentence(snap, open.focus, &panes, &loc))
    }

    /// Copy the open lens's sentence (⌘C in a pinned or keyboard lens).
    pub(crate) fn copy_stats_sentence(&self, wid: crate::WindowId) -> bool {
        self.stats_lens_sentence(wid)
            .is_some_and(|text| crate::control::pbcopy(&text))
    }

    /// The soonest lens deadline.
    pub(crate) fn stats_lens_deadline(&self) -> Option<Instant> {
        self.windows
            .values()
            .flat_map(|ws| [ws.stats_lens.rest.map(|r| r.due), ws.stats_lens.leave_due])
            .flatten()
            .min()
    }

    /// The `StatsLens` deadline's work, judged every wake.
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
                let still = self.stats_hit_under_pointer(wid).is_some_and(|h| {
                    h.instrument() == rest.hit.instrument() && h.bar() == rest.hit.bar()
                });
                if still {
                    self.open_stats_lens(wid, rest.hit.instrument(), rest.hit.bar(), Opened::Hover);
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

/// The lens's frame, its sections' forms and T-G16.
#[cfg(test)]
#[path = "stats_lens_tests.rs"]
mod tests;

/// The lens and the sill driven through the App's own pointer, key, focus
/// and config routing, read off what the tab's program received.
#[cfg(all(test, unix))]
#[path = "stats_lens_behaviour_tests.rs"]
mod behaviour_tests;
