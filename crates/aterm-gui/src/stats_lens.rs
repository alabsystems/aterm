// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stats Deck instrument lens overlay ([`lens_body`], [`REST`], [`LEAVE`]).

use std::time::{Duration, Instant};

use aterm_vitals::Rates;
use aterm_vitals::explain::episodes::CLAUSE_STALE_MS;
use aterm_vitals::explain::{
    CauseId, Context, Evidence, Lead, Part, Role, Voucher, WhySubject, account_of,
};
use aterm_vitals::history::{WEEK_BUCKET_S, WEEK_BUCKETS};
use aterm_vitals::ink::{self, Rgb, VitalsInks};
use aterm_vitals::model::{
    CpuView, DeedBy, DeedKind, DiskDeed, DiskFacts, HeatCell, LinkKind, Pressure, ProcScope,
    Resource, ResourceView, Snapshot, StripEventKind,
};
use aterm_vitals::ring::{Agg, Cell, GapKind};
use aterm_vitals::sys::{exe_word, held};
use aterm_vitals::words::{self, InkKind, Ladder, Locale, Platform};

use crate::deck_face::{ASCENT_EM, DESCENT_EM, FaceMetrics, deck_metrics};
use crate::stats_deck::{DeckInput, StatsBars, kind_ink};
use crate::stats_paint::{Bar, Instrument, Painter, WellMark, a, panel};
use crate::stats_skyline::{self as skyline, BoxHeat, BoxValue, Skyline};
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
    /// ⌘I: it takes ←/→/↑/↓, Return, ⌘C and Esc, and Tab in the Disk lens.
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

/// Which span the Disk lens's used-space chart draws: Tab switches it in a lens that takes keys
/// (`docs/DESIGN-disk-custody-2026-11-12.md` §4). Kept per window, never saved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum DiskSpan {
    /// The last 24 hours, in 10-minute columns.
    #[default]
    Day,
    /// The last 3 weeks, in 3-hour columns.
    Weeks,
}

impl DiskSpan {
    /// The span Tab switches to.
    pub(crate) const fn other(self) -> Self {
        match self {
            Self::Day => Self::Weeks,
            Self::Weeks => Self::Day,
        }
    }
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
    /// Arrow presses awaiting a successful frame of their selected lens.
    pub(crate) input: crate::metrics::PendingChromeInputStamp,
    input_selection: Option<LensSelection>,
    presented_selection: Option<Option<LensSelection>>,
    scope_tab: Option<crate::tab_model::TabId>,
    /// Stable peer/service selection within the Network State instrument.
    pub(crate) network_target: crate::network_deck::Target,
    /// The span the Disk lens's used-space chart draws (Tab).
    pub(crate) disk_span: DiskSpan,
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

impl LensSlot {
    fn selection(&self, active_tab: Option<crate::tab_model::TabId>) -> Option<LensSelection> {
        let lens = self.open?;
        Some(LensSelection {
            lens,
            target: if lens.focus == Instrument::NetworkState {
                self.network_target
            } else {
                crate::network_deck::Target::Overview
            },
            // Resource lenses read the front tab's panes and label, even in their global
            // comparison rows. Network State reads machine/service evidence independently.
            tab: if lens.focus == Instrument::NetworkState {
                None
            } else {
                Some(active_tab?)
            },
        })
    }

    /// A host sync observes every active-tab transition, including a return before painting.
    pub(crate) fn sync_tab_scope(&mut self, active_tab: Option<crate::tab_model::TabId>) {
        if self.scope_tab != active_tab {
            self.scope_tab = active_tab;
            if self
                .input_selection
                .is_some_and(|selection| selection.tab.is_some())
            {
                self.retire_input();
            }
        }
    }

    fn retire_input(&mut self) {
        self.input.retire();
        self.input_selection = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LensSelection {
    lens: Lens,
    target: crate::network_deck::Target,
    tab: Option<crate::tab_model::TabId>,
}

/// The lens selection actually included in a composed frame. `None` means painted absence;
/// an absent receipt means visibility was not established (for example, a covering card).
#[derive(Clone, Copy, Debug)]
pub(crate) struct LensPaintReceipt {
    selection: Option<LensSelection>,
    input: Option<crate::metrics::ChromeInputToken>,
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
        "Each row is one resource, read from now: the live now cell at the left end, then \
         older intervals to the right; the time scale over the strips says how far back they \
         reach: up to 6 hours where a strip has the room, 4 hours, 6 minutes or a minute on a \
         narrower one. Cells are blocks of 1 second, 10 seconds, 1 minute, 10 minutes and 1 \
         hour, from live now outward, each size starting where the scale marks it. Colour runs \
         from cold, 0 percent of the resource's maximum, to hot, 100 percent; the now cell is \
         coloured as the strip's cells are. A mark above a cell means the resource ran over its \
         maximum. An outlined cell or shaded gap was not read (sleep, crash or pause), and \
         vertical lines mark crash, wake or pressure events. The bar right of the strip, or on \
         a narrow deck the line under each percent figure, shows {machine} in grey, aterm in \
         green and this tab in bright green, named once over the bars. Where it fits, the \
         bar's end says {machine}'s amount now in the row's unit and the row's maximum; a row's \
         close-up says aterm's and this tab's.",
        machine = platform.machine()
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

/// The CPU lens's bounded export, tied to the executable that produced it.
pub(crate) fn cpu_diagnostic_report(snap: &Snapshot) -> String {
    use aterm_vitals::sys::json::Jv;
    let mut report = aterm_vitals::sys::hardware::report_json(snap);
    report.set(
        "diagnostic_executable",
        std::env::current_exe()
            .ok()
            .map_or_else(|| Jv::why("not_sampled"), |p| Jv::s(p.to_string_lossy())),
    );
    report.set("build", Jv::s(aterm_stamp::app_version()));
    report.to_text()
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
/// The most lines Held by lists, and the fewest a list of it holds before it is one line.
const HELD_MOST: usize = 8;
const HELD_LEAST: usize = 3;
/// The most lines of Held by a reader hears as elements of their own.
const HELD_SAID: usize = 6;
/// How far apart a hatch's strokes start, pt.
const HATCH_PITCH: f64 = 4.0;
/// The least width a hatched span of a bar is drawn at, pt: a period of the hatch and a stroke,
/// so however few its bytes it holds a whole stroke and never reads as its ground.
const HATCH_LEAST: f64 = HATCH_PITCH + 2.0;
/// The age, s, past which Held by says how old its rows are: three of the gathers an open Disk
/// lens keeps up ([`crate::vitals_host::HELD_EVERY`]), so a lens that is open and read says
/// nothing, and the frame a lens opens on, or one drawn after every window was hidden, does.
const HELD_STALE_S: u64 = 3 * crate::vitals_host::HELD_EVERY.as_secs();
/// The age, s, of the oldest walk past which Held by says how old its sizes may be.
const HELD_OLD_WALK_S: u64 = 600;

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

/// The body, using the same column choice as its measured section forms.
pub(crate) fn lens_body(
    (two, cw): (bool, f64),
    top: f64,
    bottom: f64,
    sections: &[Section<'_>],
    (two_order, keeps_first): (&[usize], bool),
) -> Vec<Placed> {
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

    /// 45° strokes in `stroke` over the rect at `(x, y)`, `w` by `h` pt, whose ground the caller
    /// drew: one from each multiple of [`HATCH_PITCH`] along the shell, so a rect that moves or
    /// grows does not slide its stripes, each cut to the rect less half a stroke's width (a
    /// [`DrawPrim::Line`] has round caps and no clip). A rect too narrow for a stroke takes
    /// none and reads as its ground, so a caller whose ground alone means something else gives
    /// a hatched rect [`HATCH_LEAST`] at least (Space now's bar).
    fn hatch(&mut self, x: f64, y: f64, w: f64, h: f64, stroke: Rgb) {
        if !self.draw {
            return;
        }
        let (x0, x1) = (x + 0.5, x + w - 0.5);
        let (y0, y1) = (y + 0.5, y + h - 0.5);
        let rise = y1 - y0;
        if x1 <= x0 || rise <= 0.0 {
            return;
        }
        let ink = rgba(stroke, 0xFF);
        let mut bx = ((x0 - rise) / HATCH_PITCH).ceil() * HATCH_PITCH;
        while bx < x1 {
            // The stroke runs from (bx, y1) up to (bx + rise, y0); these are its ends in the rect.
            let (ax, cx) = (bx.max(x0), (bx + rise).min(x1));
            if (cx - ax) * std::f64::consts::SQRT_2 >= 0.5 {
                self.line(ax, y1 - (ax - bx), cx, y1 - (cx - bx), ink);
            }
            bx += HATCH_PITCH;
        }
    }

    /// A session's mark `r` pt in radius centred at `(cx, cy)`: a disc where `filled`, else a
    /// ring one line wide. It is no [`WellMark`]: those mark what wants attention.
    fn dot(&mut self, cx: f64, cy: f64, r: f64, filled: bool, ink: Rgb) {
        if !self.draw {
            return;
        }
        let s = self.s;
        let (cx, cy) = (self.ox + (cx * s) as f32, self.oy + (cy * s) as f32);
        let r = (r * s) as f32;
        let color = rgba(ink, 0xFF);
        self.p.prims.push(if filled {
            DrawPrim::Dot { cx, cy, r, color }
        } else {
            DrawPrim::Ring {
                cx,
                cy,
                r_outer: r,
                thickness: s.round().max(1.0) as f32,
                track: color,
                start: 0.0,
                frac: 0.0,
                fill: color,
            }
        });
    }

    /// A caret under `(x, y)`, its apex there, 6 pt wide and 3.5 pt tall, one line wide: the
    /// cap's mark under Space now's bar.
    fn caret(&mut self, x: f64, y: f64, ink: Rgba) {
        self.line(x - 3.0, y + 3.5, x, y, ink);
        self.line(x, y, x + 3.0, y + 3.5, ink);
    }

    /// A strip box [`skyline::skyline_box_in`] laid in device px, square-cornered.
    fn skyline(&mut self, sky: &Skyline) {
        if self.draw {
            let prims = sky.prims(&self.inks, 0.0, (self.ox, self.oy));
            self.p.prims.extend(prims);
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

    /// ⇥ (the Tab key) drawn as Lines, the faces lacking the glyph: an arrow to a bar, as wide as
    /// [`Seg::TabKey`] with the gap after it.
    fn tab_key(&mut self, x: f64, base: f64) -> f64 {
        const L: f64 = 3.4;
        const HW: f64 = 1.8;
        let ink = rgba(self.inks.words2, 0xFF);
        let yc = base - 3.0;
        let tip = x + 1.0 + 2.0 * L;
        self.line(x + 1.0, yc, tip, yc, ink);
        self.line(tip, yc, tip - HW, yc - HW, ink);
        self.line(tip, yc, tip - HW, yc + HW, ink);
        self.line(tip + 1.0, yc - L, tip + 1.0, yc + L, ink);
        12.0
    }

    /// A cell's ink at `v`, the Deck's own ([`aterm_vitals::ink::VitalsInks::cell_at`]): the heat
    /// ramp's step for it.
    fn heat(&self, v: f32) -> Rgb {
        self.inks.cell_at(v.max(0.0), self.ic)
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

/// A section heading whose words beside it always show: the first of `subs` that fits, else
/// the last cut with "…" (what Held by's rows stand on, which no width sheds).
fn heading_kept(pen: &mut Pen<'_>, p: Place, name: &str, subs: &[String]) {
    let base = p.y + HEAD_BASE;
    let name = pen.cut(name, p.w, HEAD_PT, true);
    let ink = pen.inks.words;
    let nw = pen.text(p.x, base, &name, HEAD_PT, true, ink);
    let sub = fit_cut(pen.m, subs, (p.w - nw - 8.0).max(0.0), NOTE_PT, false);
    let ink = pen.inks.words2;
    pen.text(p.x + nw + 8.0, base, &sub, NOTE_PT, false, ink);
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
    /// Its words: its name, its scope and its figure, an empty figure left out.
    fn said(&self) -> String {
        let named = match &self.scope {
            Some(scope) => format!("{} in {scope}", self.name),
            None => self.name.clone(),
        };
        if self.figure.is_empty() {
            named
        } else {
            format!("{named}, {}", self.figure)
        }
    }
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
        ProcScope::Tab(_) | ProcScope::Aterm | ProcScope::Shared => {
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
    let has_share = rows.iter().any(|r| r.share.is_some());
    let longest = rows
        .iter()
        .map(|r| pen.width(&r.name, BODY_PT, false))
        .fold(0.0, f64::max);
    // A list of scopes and no bars (What the software did) gives its names the bars' room.
    let (name_w, scope_w) = if has_scope && !has_share {
        let scope_w = (avail * 0.1).clamp(36.0, 52.0).min(avail);
        ((avail - scope_w - 4.0).max(0.0), scope_w)
    } else {
        let name_w = if has_scope || has_share {
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
        (name_w, scope_w)
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

/// Discontinuity span mapped onto a trace's fractional bucket indices.
struct TraceGap {
    start: f64,
    end: f64,
    kind: GapKind,
}

/// Vertical strip event marker mapped onto a trace's fractional bucket index.
struct TraceEvent {
    at: f64,
    label: String,
    ink: InkKind,
}

/// A trace's data and words: a strip of boxes on uniform time, each drawn by
/// [`skyline::skyline_box_in`] at its axis height (`docs/DESIGN-stats-deck-units-2026-11-04.md`
/// §7, "Lens traces keep their axes and time base").
struct Trace {
    /// The values, oldest first: each column's mean.
    vals: Vec<Option<f64>>,
    /// Each column's largest reading in the values' unit, its peak tick; `None` where the history
    /// keeps one figure a column.
    peaks: Vec<Option<f64>>,
    /// Each column's heat as its lens names it, a share of capacity: the mean's (the column's
    /// ink), the largest reading's (the tick's) and the over share (the cap).
    heat: Vec<HeatCell>,
    /// The labels under the plot.
    under: &'static [(f64, &'static str)],
    /// The least span the axis shows, in the values' unit.
    min_span: f64,
    /// A rule at a fractional bucket index, and its label.
    mark: Option<(f64, String)>,
    /// Discontinuity spans within the trace window.
    gaps: Vec<TraceGap>,
    /// Strip event markers within the trace window.
    events: Vec<TraceEvent>,
    /// Removals the software made, at fractional bucket indices: a short tick under the axis each.
    removals: Vec<f64>,
    /// The span chosen, where the heading names the spans it can switch between (the Disk lens's
    /// used-space chart); `None` where it names none.
    chips: Option<DiskSpan>,
}

/// Maps `snap`'s discontinuity gaps and strip event markers onto fractional bucket coordinates of a
/// `cols`-bucket trace starting at `start_ms` (`bucket_ms` per bucket).
fn trace_overlays(
    snap: &Snapshot,
    start_ms: i64,
    bucket_ms: i64,
    cols: usize,
) -> (Vec<TraceGap>, Vec<TraceEvent>) {
    if cols == 0 || bucket_ms <= 0 {
        return (Vec::new(), Vec::new());
    }
    let span = cols as f64;
    let b_ms = bucket_ms as f64;
    let gaps = snap
        .time_gaps
        .iter()
        .filter_map(|g| {
            let start = ((g.start_ms - start_ms) as f64 / b_ms).clamp(0.0, span);
            let end = ((g.end_ms - start_ms) as f64 / b_ms).clamp(0.0, span);
            (end > start).then_some(TraceGap {
                start,
                end,
                kind: g.kind,
            })
        })
        .collect();
    let mut events: Vec<(u8, TraceEvent)> = snap
        .strip_events
        .iter()
        .filter_map(|ev| {
            let at = (ev.at_unix_ms - start_ms) as f64 / b_ms;
            if !(0.0..=span).contains(&at) {
                return None;
            }
            let (prio, word, ink) = match ev.kind {
                StripEventKind::Crash => (0, "crash", InkKind::Critical),
                StripEventKind::PressureSpike => (1, "pressure", InkKind::Warn),
                StripEventKind::SleepWake => (2, "woke", InkKind::Words2),
                StripEventKind::Restart => (3, "restarted", InkKind::Words2),
            };
            Some((
                prio,
                TraceEvent {
                    at,
                    label: word.to_owned(),
                    ink,
                },
            ))
        })
        .collect();
    events.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.at.total_cmp(&b.1.at)));
    let events = events.into_iter().map(|(_, ev)| ev).collect();
    (gaps, events)
}

/// `view`'s last hour as 60 one-minute trace columns ending at `now`: each column's mean and
/// largest reading of the first series over `unit` (never below 0), and its heat as the deck's
/// row takes it ([`ResourceView::heat_columns`]): a share of capacity now, on a two-series row
/// (Processes: all, and yours against your user's limit) the larger of the two shares, the limit
/// that binds first (the R3 lens fixes' review, 2026-11-06). A one-series row's is its first
/// series' share alone.
fn hour_columns(
    view: &ResourceView,
    now: i64,
    unit: f64,
) -> (Vec<Option<f64>>, Vec<Option<f64>>, Vec<HeatCell>) {
    let (vals, peaks) = trace_figures(&view.columns(60, 60, now), unit);
    (vals, peaks, view.heat_columns(60, 60, now))
}

/// `cells` as a trace's figures over `unit` (never below 0): each column's mean, and its largest
/// reading for its peak tick.
fn trace_figures(cells: &[Cell], unit: f64) -> (Vec<Option<f64>>, Vec<Option<f64>>) {
    let of = |v: Option<f64>| v.map(|b| (b / unit).max(0.0));
    let vals = cells.iter().map(|cell| of(cell.mean)).collect();
    let peaks = cells.iter().map(|cell| of(cell.max)).collect();
    (vals, peaks)
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
        if lo >= 0.0 {
            let lo0 = (mid - min_span / 2.0).max(0.0);
            (lo0, lo0 + min_span)
        } else {
            (mid - min_span / 2.0, mid + min_span / 2.0)
        }
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

/// Each read column of `t` as a strip box between `x0` and `x1` and from `y0` down to `y1` (pt),
/// at its height on the axis `lo..hi`: whole device px each edge, a device px of ground between
/// columns 4 px or more apart. Unread columns keep the trace's own gap marks.
fn trace_boxes(
    pen: &Pen<'_>,
    t: &Trace,
    (x0, y0, x1, y1): (f64, f64, f64, f64),
    (lo, hi): (f64, f64),
) -> Vec<Skyline> {
    let cols = t.vals.len().max(1);
    let pitch = (x1 - x0) / cols as f64;
    let gap = if pitch * pen.s >= 4.0 { 1.0 } else { 0.0 };
    let span = (hi - lo).max(f64::MIN_POSITIVE);
    let share = |v: f64| ((v - lo) / span) as f32;
    let (top, bottom) = (pen.px(y0), pen.px(y1));
    t.vals
        .iter()
        .enumerate()
        .filter_map(|(j, v)| {
            let v = (*v)?;
            let xa = pen.px(x0 + j as f64 * pitch);
            let xb = pen.px(x0 + (j + 1) as f64 * pitch) - gap;
            let h = t.heat.get(j).copied().unwrap_or_default();
            let value = BoxValue {
                mean: Some(share(v)),
                max: t.peaks.get(j).copied().flatten().map(share),
                over: h.over,
            };
            let heat = BoxHeat {
                mean: h.heat,
                max: h.max,
            };
            (xb > xa && bottom > top).then(|| {
                skyline::skyline_box_in(
                    (xa, top, xb - xa, bottom - top),
                    value,
                    heat,
                    pen.s,
                    &pen.inks,
                    pen.ic,
                )
            })
        })
        .collect()
}

/// The trace `t` in `p` under its heading.
fn draw_trace(pen: &mut Pen<'_>, p: Place, t: &Trace) {
    let inks = pen.inks;
    let read: Vec<f64> = t.vals.iter().chain(&t.peaks).flatten().copied().collect();
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
    let cols = t.vals.len().max(1);
    let pitch = (x1 - x0) / cols as f64;
    let skies = trace_boxes(pen, t, (x0, y0, x1, y1), (lo, hi));
    // Plates first, the axis rules over them, then each column, tick and cap over the rules.
    for sky in &skies {
        let plate = Skyline {
            plate: sky.plate,
            ..Skyline::default()
        };
        pen.skyline(&plate);
    }
    for (k, l) in labels.iter().enumerate() {
        let yy = y1 - ch * k as f64 / n as f64;
        pen.rule(x0, yy, x1 - x0, rgba(inks.grid, 0xFF));
        pen.text_end(x0 - 4.0, yy + 3.2, l, AXIS_PT, false, inks.words2);
    }
    for sky in &skies {
        let rest = Skyline {
            plate: None,
            ..*sky
        };
        pen.skyline(&rest);
    }
    let mut rule_xs: Vec<f64> = Vec::with_capacity(t.events.len() + 1);
    if let Some((at, _)) = &t.mark {
        let fx = x0 + at * pitch;
        if (x0..=x1).contains(&fx) {
            rule_xs.push(fx);
        }
    }
    for ev in &t.events {
        let fx = x0 + ev.at * pitch;
        if (x0..=x1).contains(&fx) {
            rule_xs.push(fx);
        }
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
        let gap_kind = t
            .gaps
            .iter()
            .filter(|g| g.start < j as f64 + 0.5 && g.end > start as f64 - 0.5)
            .map(|g| g.kind)
            .max();
        let hollow_ink = match gap_kind {
            Some(GapKind::Crash) => rgba(inks.critical, 0xCC),
            Some(GapKind::Sleep) => rgba(inks.words2, 0xAA),
            _ => rgba(inks.hollow, 0xFF),
        };
        if pitch * pen.s >= 3.0 {
            for k in start..j {
                let xk = x0 + k as f64 * pitch;
                pen.hollow(xk, y1 - 4.0, pitch, 4.0, hollow_ink);
            }
        } else {
            pen.hollow(xa, y1 - 4.0, xb - xa, 4.0, hollow_ink);
        }
        let (words, word_ink) = match gap_kind {
            Some(GapKind::Crash) => ("crash", inks.warn_text),
            Some(GapKind::Sleep) => ("asleep", inks.words2),
            Some(GapKind::Paused) => ("paused", inks.words2),
            None => ("not read", inks.words2),
        };
        let lw = pen.width(words, AXIS_PT, false);
        let span_fits = if gap_kind.is_some() {
            lw + 6.0 <= xb - xa
        } else {
            lw <= xb - xa + 16.0
        };
        if ch >= 30.0 && span_fits && lw <= x1 - x0 {
            let lx = ((xa + xb) / 2.0 - lw / 2.0).clamp(x0, x1 - lw);
            let crosses_rule = rule_xs
                .iter()
                .any(|&rx| rx >= lx - 3.0 && rx <= lx + lw + 3.0);
            if !crosses_rule && !said.iter().any(|&(a, b)| lx < b + 6.0 && a < lx + lw + 6.0) {
                pen.text(lx, y1 - 8.0, words, AXIS_PT, false, word_ink);
                said.push((lx, lx + lw));
            }
        }
    }
    let mut top_said: Vec<(f64, f64)> = Vec::new();
    let mut place_top = |pen: &mut Pen<'_>, fx: f64, label: &str, tick_ink: Rgba, text_ink: Rgb| {
        if !(x0..=x1).contains(&fx) {
            return;
        }
        pen.tick(fx, y0 - 2.0, y1, tick_ink);
        if ch < 24.0 {
            return;
        }
        let lw = pen.width(label, AXIS_PT, false);
        let candidates = [fx - 4.0 - lw, fx + 4.0];
        for lx in candidates {
            let crosses_other_rule = rule_xs
                .iter()
                .any(|&rx| (rx - fx).abs() > 0.5 && rx >= lx - 2.5 && rx <= lx + lw + 2.5);
            if lx >= x0
                && lx + lw <= x1
                && !crosses_other_rule
                && !top_said
                    .iter()
                    .any(|&(a, b)| lx < b + 6.0 && a < lx + lw + 6.0)
            {
                pen.text(lx, y0 + 8.0, label, AXIS_PT, false, text_ink);
                top_said.push((lx, lx + lw));
                break;
            }
        }
    };
    if let Some((at, label)) = &t.mark {
        place_top(
            pen,
            x0 + at * pitch,
            label,
            rgba(inks.words, 0xFF),
            inks.words,
        );
    }
    for ev in &t.events {
        let ev_ink = kind_ink(&inks, ev.ink);
        place_top(
            pen,
            x0 + ev.at * pitch,
            &ev.label,
            rgba(ev_ink, 0xDD),
            ev_ink,
        );
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
    // Each removal: a short 1 pt tick in a strip under the axis, ending above the labels under
    // it, so a removal reads by its shape and place and never by its heat alone; its ink is the
    // words ink this ground solved.
    for &at in &t.removals {
        let fx = x0 + at * pitch;
        if (x0..=x1).contains(&fx) {
            pen.tick(fx, y1 + 1.0, y1 + 4.0, rgba(inks.words, 0xFF));
        }
    }
}

/// The span labels at a trace heading's right end, `24 h · 3 weeks`, the chosen one in the words
/// ink and underlined, the other in the secondary ink (the key hint on line 1 names Tab, which
/// switches them). Drawn only where they fit beside `name` and the first of `subs` that fits
/// without them, so they never cost the heading its growth words, which keep the form they have
/// without the labels. The width they take with their gap, else 0.
fn span_chips(pen: &mut Pen<'_>, p: Place, name: &str, subs: &[String], chosen: DiskSpan) -> f64 {
    const PAD: f64 = 4.0;
    let spans = [(DiskSpan::Day, "24 h"), (DiskSpan::Weeks, "3 weeks")];
    let dot_w = pen.width("\u{b7}", NOTE_PT, false);
    let w = spans
        .iter()
        .map(|(_, s)| pen.width(s, NOTE_PT, false))
        .sum::<f64>()
        + dot_w
        + 2.0 * PAD;
    // What [`heading`] lays at the full width: its name, and the growth words that fit beside it.
    let name_w = pen.width(&pen.cut(name, p.w, HEAD_PT, true), HEAD_PT, true);
    let sub_w = first_fit(pen.m, subs, p.w - name_w - 8.0, NOTE_PT, false)
        .map_or(0.0, |sub| 8.0 + pen.width(&sub, NOTE_PT, false));
    if name_w + sub_w + 8.0 + w > p.w + EPS {
        return 0.0;
    }
    let base = p.y + HEAD_BASE;
    let inks = pen.inks;
    let mut x = p.x + p.w - w;
    for (k, (span, words)) in spans.into_iter().enumerate() {
        if k > 0 {
            x += pen.text(x + PAD, base, "\u{b7}", NOTE_PT, false, inks.words2) + 2.0 * PAD;
        }
        let ink = if span == chosen {
            inks.words
        } else {
            inks.words2
        };
        let sw = pen.text(x, base, words, NOTE_PT, false, ink);
        if span == chosen {
            pen.rule(x, base + 2.0, sw, rgba(ink, 0xFF));
        }
        x += sw;
    }
    w + 8.0
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
                let chips = trace
                    .chips
                    .map_or(0.0, |span| span_chips(pen, p, &name, &subs, span));
                heading(
                    pen,
                    Place {
                        w: p.w - chips,
                        ..p
                    },
                    &name,
                    &subs,
                );
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

/// Two days, ms: a trace's ages read in days from here.
const TWO_DAYS_MS: i64 = 48 * 3_600_000;

/// `at_ms` as its local day, `Oct 27`: month first, as the style guide's American usage writes a
/// date.
fn month_day(at_ms: i64, loc: &Locale) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let local_s = at_ms
        .div_euclid(1000)
        .saturating_add(loc.offset_at_ms(at_ms));
    let (_, month, day) = aterm_types::rfc3339::civil_from_days(local_s.div_euclid(86_400));
    let month = MONTHS[usize::try_from(month - 1).unwrap_or(0).min(11)];
    format!("{month} {day}")
}

/// When a deed was done, as its row says it: the clock within 6 days (`10:30`, `Wed 10:30`), its
/// day before that (`Oct 27`).
fn deed_when(at_ms: i64, now_ms: i64, loc: &Locale) -> String {
    if now_ms.saturating_sub(at_ms) < 6 * 86_400_000 {
        loc.clock(at_ms, now_ms)
    } else {
        month_day(at_ms, loc)
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
        let (k, one, many) = if ms >= TWO_DAYS_MS {
            ((ms as f64 / 86_400_000.0).round() as u64, "day", "days")
        } else if ms >= 3_600_000 {
            ((ms as f64 / 3_600_000.0).round() as u64, "hour", "hours")
        } else {
            ((ms as f64 / 60_000.0).round() as u64, "minute", "minutes")
        };
        format!("{k} {} ago", if k == 1 { one } else { many })
    };
    // A trace of more than two days names each gap's days too.
    let days = n as i64 * bucket_ms > TWO_DAYS_MS;
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
                let ms = start_ms + i as i64 * bucket_ms;
                let mut t = clock_s(ms, loc.offset_at_ms(ms));
                t.truncate(5);
                if days {
                    format!("{} {t}", month_day(ms, loc))
                } else {
                    t
                }
            };
            out.push_str(&format!(" Not read from {} to {}.", at(start), at(j)));
            gaps += 1;
        }
    }
    out
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
    fresh: bool,
    /// The span the Disk lens's used-space chart draws.
    disk_span: DiskSpan,
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
            ProcScope::Shared => "shared aterm".to_owned(),
            ProcScope::Other => "other".to_owned(),
        }
    }

    fn at(&self) -> String {
        clock_s(
            self.snap.at_unix_ms,
            self.loc.offset_at_ms(self.snap.at_unix_ms),
        )
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PresenceAxis {
    Elapsed { start: u64, span: u64 },
    Recorded { last: usize },
}

impl PresenceAxis {
    fn for_samples(samples: &[aterm_types::network_state::PresenceSample], observed: u64) -> Self {
        let start = samples.first().map_or(observed, |sample| sample.at_ms);
        if samples.windows(2).all(|pair| pair[0].at_ms < pair[1].at_ms)
            && samples.last().is_none_or(|sample| sample.at_ms <= observed)
        {
            Self::Elapsed {
                start,
                span: observed.saturating_sub(start).max(1),
            }
        } else {
            Self::Recorded {
                last: samples.len().saturating_sub(1),
            }
        }
    }

    fn fraction(self, index: usize, at: u64) -> f64 {
        match self {
            Self::Elapsed { start, span } => {
                at.saturating_sub(start).min(span) as f64 / span as f64
            }
            Self::Recorded { last } => index as f64 / last.max(1) as f64,
        }
    }

    const fn heading(self) -> &'static str {
        match self {
            Self::Elapsed { .. } => "Observed presence",
            Self::Recorded { .. } => "Recorded order",
        }
    }

    const fn words(self) -> &'static str {
        match self {
            Self::Elapsed { .. } => {
                "Recorded samples only; blank intervals do not establish reachability"
            }
            Self::Recorded { .. } => {
                "Samples in recorded order; spacing does not represent elapsed time. Blank intervals do not establish reachability"
            }
        }
    }
}

/// The lens for `c`'s instrument.
fn lab_network_doc(
    snapshot: Option<&aterm_types::network_state::NetworkStateSnapshot>,
    lab: Option<&crate::lab_deck::LabDeck>,
    fleet: Option<&crate::network_deck::field::Field>,
    target: crate::network_deck::Target,
    caption: Option<&crate::network_deck::PlaceCaption>,
    metrics: &FaceMetrics,
    width: f64,
) -> LensDoc<'static> {
    let empty = aterm_types::network_state::NetworkStateSnapshot::default();
    let snapshot = snapshot.unwrap_or(&empty);
    let empty_lab = crate::lab_deck::LabDeck::default();
    let lab = lab.unwrap_or(&empty_lab);
    let mut rows =
        crate::network_deck::details_lab_with_caption(snapshot, lab, fleet, target, caption);
    let fleet_target = fleet.is_some()
        && matches!(
            target,
            crate::network_deck::Target::Fleet
                | crate::network_deck::Target::Seat(crate::network_deck::SeatKey::Fleet(_))
                | crate::network_deck::Target::Group(_)
        );
    let observed = matches!(
        target,
        crate::network_deck::Target::Seat(crate::network_deck::SeatKey::Observed(_))
    );
    // A selected service needs its own scope before generic network guidance.
    // Keep Scope in the complete report, but present it once in the lens header.
    let scope = if target == crate::network_deck::Target::Accounts {
        rows.iter()
            .position(|(name, _)| name == "Scope")
            .map(|index| rows.remove(index).1)
    } else {
        None
    };
    let line_tab = rows
        .iter()
        .map(|(name, _)| tab_after(metrics.width(name, HEAD_PT, true)))
        .fold(0.0, f64::max);
    let mut sections: Vec<_> = rows
        .into_iter()
        .map(|(name, value)| {
            if (target == crate::network_deck::Target::Overview && name != "Observer")
                || metrics.width(&value, BODY_PT, false) <= width - line_tab
            {
                return line_section(&name, &name, vec![value], false);
            }
            let lines = wrap(metrics, &value, width, BODY_PT);
            let label = name.clone();
            let compact = ((target == crate::network_deck::Target::Overview && name == "Observer")
                || observed
                || fleet_target)
                .then(|| line_form(&name, vec![value.clone()]));
            let mut forms = vec![form(rows_h(lines.len(), false), move |pen, p| {
                heading(pen, p, &label, &[]);
                let ink = pen.inks.words2;
                for (i, line) in lines.iter().enumerate() {
                    pen.text(
                        p.x,
                        p.y + ROW0_BASE + PITCH * i as f64,
                        line,
                        BODY_PT,
                        false,
                        ink,
                    );
                }
            })];
            if let Some(compact) = compact {
                forms.push(compact);
            }
            Section {
                name,
                words: vec![value],
                right: false,
                parts: Vec::new(),
                forms,
            }
        })
        .collect();
    if let crate::network_deck::Target::Seat(crate::network_deck::SeatKey::Observed(slot)) = target
        && let Some(peer) = snapshot.network.peers.iter().find(|peer| peer.slot == slot)
        && !peer.samples.is_empty()
    {
        let samples = peer.samples.clone();
        let observed = snapshot.network.observed_at_ms;
        let axis = PresenceAxis::for_samples(&samples, observed);
        let history = form(42.0, move |pen, p| {
            let color = pen.inks.words2;
            pen.text(p.x, p.y + HEAD_BASE, axis.heading(), HEAD_PT, true, color);
            let width = (p.w - 8.0).max(1.0);
            for (index, sample) in samples.iter().enumerate() {
                let x = p.x + axis.fraction(index, sample.at_ms) * width;
                let fill = if sample.state == aterm_types::network_state::Reachability::Reachable {
                    pen.inks.machine
                } else {
                    pen.inks.hollow
                };
                pen.rect(x, p.y + 19.0, 4.0, 7.0, 0.0, rgba(fill, 255));
                if sample.state != aterm_types::network_state::Reachability::Reachable {
                    pen.rect(
                        x + 1.0,
                        p.y + 20.0,
                        2.0,
                        5.0,
                        0.0,
                        rgba(pen.inks.ground, 255),
                    );
                }
            }
            let end = "Last observation";
            let first = pen.cut(
                "First retained observation",
                (p.w - pen.width(end, BODY_PT, false) - 8.0).max(0.0),
                BODY_PT,
                false,
            );
            pen.text(p.x, p.y + 38.0, &first, BODY_PT, false, color);
            pen.text_end(p.x + p.w, p.y + 38.0, end, BODY_PT, false, color);
        });
        // Retained samples belong with the selected peer, before general network context.
        let before_context = sections
            .iter()
            .position(|section| section.name == "Network")
            .unwrap_or(sections.len());
        sections.insert(
            before_context,
            Section {
                name: "Observed presence".into(),
                words: vec![axis.words().into()],
                right: false,
                parts: Vec::new(),
                forms: vec![history],
            },
        );
    }
    LensDoc {
        name: "Network State",
        sources: vec![if target == crate::network_deck::Target::Accounts {
            "Local observation of the Commons account catalog".into()
        } else if matches!(target, crate::network_deck::Target::LabAccount(_)) {
            "The lab registry and journal as this Mac last read them".into()
        } else if fleet_target {
            fleet.map_or_else(String::new, |fleet| {
                if fleet.copy_path {
                    "This Mac's node view of the lab's seats".into()
                } else {
                    "This Mac's survey of every fleet Mac".into()
                }
            })
        } else {
            "This machine's local observer and retained authenticated history".into()
        }],
        explainer: scope.unwrap_or_else(|| {
            if fleet_target {
                "Seat: filled answered, half carried, hollow not answering, dotted not counted; hair under: another build; corner: needs an update. Below: bar busy, diamond waiting, capped bar at a limit, cross stuck; a hair quiet; nothing where not read. ↑/↓: the seats that need attention.".into()
            } else if observed {
                "↑/↓: machine or service. Outbound addresses are historical.".into()
            } else if matches!(target, crate::network_deck::Target::LabAccount(_)) {
                "Account cell: the heat of its most used window; the over-max ink at its limit; hollow with no current reading; this Mac's rim where this Mac prefers it. ↑/↓: machine, service or account.".into()
            } else {
                "Filled peer: reachable; hollow peer: unconfirmed. Warnings: peer = unknown visibility, local = partial history, service = refresh failed. ↑/↓: machine or service; ←/→: instrument. Reachability grants no account or secret access.".into()
            }
        }),
        answer: Some((
            "Now",
            match target {
                crate::network_deck::Target::LabAccount(id) => lab.row(id).map_or_else(
                    || "No longer among the lab's rows".into(),
                    |row| row.summary.clone(),
                ),
                _ if fleet_target => fleet.map_or_else(String::new, crate::network_deck::field::now_line),
                _ => crate::network_deck::summary_with_caption(snapshot, caption),
            },
            InkKind::Words2,
        )),
        sections,
        two_order: Vec::new(),
        keeps_first: true,
    }
}

#[cfg(test)]
#[path = "stats_lens_network_tests.rs"]
mod network_tests;

fn doc_for<'d>(c: Ctx<'d>, focus: Instrument) -> LensDoc<'d> {
    match focus {
        Instrument::Disk => disk_doc(c),
        Instrument::Cpu => cpu_doc(c),
        Instrument::Memory => memory_doc(c),
        Instrument::Network => network_doc(c),
        Instrument::Processes => processes_doc(c),
        Instrument::HowToRead => how_to_read(c),
        Instrument::NetworkState => how_to_read(c),
        Instrument::Tokens | Instrument::Gpu => text_well(c, focus),
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
    // Held by, then What the software did, come after Filling and top the right column in that
    // order, over Writing now and Where the space is; with no held rows or no deed that section
    // is left out, and with neither the lens is as it was.
    let held = held_section(c);
    let did = did_section(c);
    let two_order = match (held.is_some(), did.is_some()) {
        (false, false) => vec![0, 1, 3, 2],
        (true, false) | (false, true) => vec![0, 1, 2, 4, 3],
        (true, true) => vec![0, 1, 2, 3, 5, 4],
    };
    let mut sections = vec![space_section(c), filling_section(c)];
    sections.extend(held);
    sections.extend(did);
    sections.extend([where_section(c), writing_section(c)]);
    LensDoc {
        name: "Disk",
        sources,
        explainer,
        answer,
        sections,
        two_order,
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
    let space =
        words::space_line(Resource::Disk, snap).unwrap_or_else(|| words::disk_space_line(&v));
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
        let t = c.loc.clock(*at, snap.at_unix_ms);
        let g = gb(*b as f64);
        vec![
            format!("aterm {g} today ({t}, {what})"),
            format!("aterm {g} today, {t}"),
            format!("{g} today"),
        ]
    });
    // What the used space is, from the rows Held by lists over the same volume, and the cap
    // the budget holds it to: both are readings, and neither moves the level or its lines.
    let held = snap.facts.disk.as_ref().and_then(|d| d.held.as_deref());
    let walk = held.map_or(held::Walk::Complete, |h| h.walk);
    let spans = held.and_then(|h| class_spans(&held_lines(h, Some(v.used()), c.panes), h.walk));
    let cap = d.and_then(|d| d.cap);
    let key = space_key(spans, walk, cap);
    let mut words = vec![space.clone()];
    words.extend(legend.first().cloned());
    if let Some(spans) = &spans {
        words.push(spans.said());
        words.extend(walk.clause().map(str::to_owned));
    }
    if let Some(cap) = cap {
        words.push(format!("cap {} used", gb(cap as f64)));
    }
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
        let name_w = pen.width(&pen.cut("Space now", p.w, HEAD_PT, true), HEAD_PT, true);
        let beside = Place {
            x: p.x + name_w + 12.0,
            w: p.w - name_w - 12.0,
            ..p
        };
        draw_space_key(pen, beside, &key);
        let inks = pen.inks;
        let (by, bh) = (p.y + 16.0, 10.0);
        let total = v.total.max(1) as f64;
        pen.rect(p.x, by, p.w, bh, 1.5, rgba(inks.track, 0xFF));
        let used = p.w * (v.used() as f64 / total).clamp(0.0, 1.0);
        let machine = pen.machine();
        pen.rect(p.x, by, used, bh, 1.5, rgba(machine, 0xFF));
        if let Some(spans) = spans {
            // From the fill's end leftward: what a build makes again, solid; then what is costly
            // to have again, the same hue hatched (`hatch_ink`). What is left of the fill is
            // kept or not classified, in the machine's ink as before.
            // A span with bytes is never narrower than `least`: two device px solid, and
            // hatched a whole period, so a few costly bytes do not read as rebuildable.
            let share = |bytes: u64, least: f64| {
                if bytes == 0 {
                    0.0
                } else {
                    (p.w * bytes as f64 / total).max(least)
                }
            };
            let e2 = p.x + used;
            let e1 = (e2 - share(spans.rebuildable, 2.0 / pen.s)).max(p.x);
            let e0 = (e1 - share(spans.costly, HATCH_LEAST)).max(p.x);
            let hue = class_hue(pen);
            pen.rect(e0, by, e2 - e0, bh, 0.0, rgba(hue, 0xFF));
            let stroke = hatch_ink(pen);
            pen.hatch(e0, by, e1 - e0, bh, stroke);
        }
        for (_, b, critical) in &lines {
            let lx = p.x + p.w * (1.0 - (*b as f64 / total).clamp(0.0, 1.0));
            let ink = if *critical { inks.critical } else { inks.warn };
            pen.tick(lx, by - 2.0, by + bh + 2.0, rgba(ink, 0xFF));
        }
        if let Some(cap) = cap {
            // The cap counts used bytes, so it is measured from the bar's start. It is a reading,
            // never a level, and no ink alone says so: where a level's line is a tick across the
            // bar in its own ink, the cap is a caret under the bar in the secondary words' ink,
            // on the shell's ground whatever fill the bar has above it.
            let cx = p.x + p.w * (cap as f64 / total).clamp(0.0, 1.0);
            pen.caret(cx, by + bh + 0.75, rgba(inks.words2, 0xFF));
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

/// Each bucket of free space as a used-space column on a volume of `total` bytes: its time-weighted
/// mean used (GB, the column), its most used (GB, its tick: the bucket's least free) and its
/// heat, its mean's share of `total`.
fn used_columns(
    buckets: &[Option<Agg>],
    total: Option<u64>,
) -> (Vec<Option<f64>>, Vec<Option<f64>>, Vec<HeatCell>) {
    let used = |free: f64| total.map(|t| (t as f64 - free.max(0.0)).max(0.0));
    let mean_used: Vec<Option<f64>> = buckets
        .iter()
        .map(|b| b.and_then(|b| used(b.value()?)))
        .collect();
    let vals: Vec<Option<f64>> = mean_used.iter().map(|u| u.map(|b| b / 1e9)).collect();
    let peaks: Vec<Option<f64>> = buckets
        .iter()
        .map(|b| b.and_then(|b| used(b.min)).map(|u| u / 1e9))
        .collect();
    let heat: Vec<HeatCell> = mean_used
        .iter()
        .map(|u| HeatCell {
            heat: u.zip(total).map(|(u, t)| (u / t as f64) as f32),
            ..HeatCell::default()
        })
        .collect();
    (vals, peaks, heat)
}

/// The used-space chart's subtitle forms (the disk's growth) and its one-line forms (`full in`
/// first while the disk fills), whichever span it draws.
fn filling_words(snap: &Snapshot) -> (Vec<String>, Vec<String>) {
    let growth = growth_forms(&snap.facts.disk_rates);
    let mut line = growth.clone();
    if let Some(f) = snap.facts.filling {
        line.insert(0, format!("full in {}", words::duration_approx(f.secs)));
    }
    (growth, line)
}

/// GB used, last 24 hours: occupancy as space used, so a column's height and its heat both mean
/// full, as the deck's Disk row does (`docs/DESIGN-stats-deck-units-2026-11-04.md` §7). Each
/// 10-minute column is the space its readings used on average, each weighed by its time, and its
/// tick the most any used ([`aterm_vitals::history::FreeHistory`] keeps each bucket's free space
/// as an [`aterm_vitals::Agg`]: its mean and its least). With the three-week span chosen (Tab),
/// [`weeks_section`] draws it instead; the heading names both spans while they are offered
/// ([`spans_offered`]).
fn filling_section(c: Ctx<'_>) -> Section<'_> {
    if c.disk_span == DiskSpan::Weeks {
        return weeks_section(c);
    }
    let snap = c.snap;
    let now = snap.at_unix_ms;
    const BUCKET_MS: i64 = aterm_vitals::history::FREE_BUCKET_S * 1000;
    let total = snap
        .volume
        .as_ref()
        .map(|v| v.total)
        .or_else(|| snap.free_history.volume().map(|v| v.total))
        .filter(|&t| t > 0);
    let buckets = snap.free_history.buckets(now);
    let (vals, peaks, heat) = used_columns(&buckets, total);
    let n = vals.len() as i64;
    let start_ms = (now.div_euclid(BUCKET_MS) + 1 - n) * BUCKET_MS;
    let freed = watched(snap).and_then(|d| d.freed_today.as_ref());
    let mark = freed.and_then(|(b, at, _)| {
        let idx = (at - start_ms) as f64 / BUCKET_MS as f64;
        (0.0..n as f64).contains(&idx).then(|| {
            let t = c.loc.clock(*at, now);
            (idx, format!("aterm freed {} \u{b7} {t}", gb(*b as f64)))
        })
    });
    let (growth, line) = filling_words(snap);
    let (gaps, events) = trace_overlays(snap, start_ms, BUCKET_MS, vals.len());
    let trace = Trace {
        vals,
        peaks,
        heat,
        under: DAY,
        min_span: 1.0,
        mark,
        gaps,
        events,
        removals: Vec::new(),
        chips: spans_offered(snap, DiskSpan::Day).then_some(DiskSpan::Day),
    };
    let mut said = trace_said(
        "GB used over the last 24 hours",
        "gigabytes",
        &trace,
        (start_ms, BUCKET_MS),
        &c.loc,
    );
    if let Some((b, at, _)) = freed {
        said.push_str(&format!(
            " aterm freed {} at {}.",
            words::speak(&gb(*b as f64)),
            c.loc.clock(*at, now)
        ));
    }
    trace_section(
        c,
        "GB used, last 24 hours",
        growth,
        trace,
        said,
        "Filling",
        line,
        false,
    )
}

/// Whether the Disk lens's used-space chart offers its two spans with `chosen` drawn: once the
/// three-week history holds a reading older than the last day (before that the three weeks would
/// show no more than the day does), and while the three weeks are chosen, so Tab can switch back.
fn spans_offered(snap: &Snapshot, chosen: DiskSpan) -> bool {
    // The last day's hours are the series' last 24; any reading before them is older.
    chosen == DiskSpan::Weeks
        || snap.free_weeks.buckets(snap.at_unix_ms)[..WEEK_BUCKETS - 24]
            .iter()
            .any(Option::is_some)
}

/// The labels under a three-week trace.
const WEEKS: &[(f64, &str)] = &[
    (0.0, "3 weeks ago"),
    (1.0 / 3.0, "2 weeks"),
    (2.0 / 3.0, "1 week"),
    (1.0, "now"),
];

/// The hours a three-week trace's column merges.
const WEEK_MERGE: usize = 3;
/// A three-week trace's columns.
const WEEK_COLUMNS: usize = WEEK_BUCKETS / WEEK_MERGE;
/// An hour, ms: a three-week bucket.
const WEEK_HOUR_MS: i64 = WEEK_BUCKET_S * 1000;
/// A three-week trace's column, ms.
const WEEK_COLUMN_MS: i64 = WEEK_HOUR_MS * WEEK_MERGE as i64;

/// The removals the journal holds in the three-week trace starting at `start_ms`, up to the
/// snapshot's now: each hour's start and its bytes, oldest first.
fn weeks_removals(snap: &Snapshot, start_ms: i64) -> Vec<(i64, u64)> {
    let now = snap.at_unix_ms;
    snap.facts
        .disk
        .iter()
        .flat_map(|d| d.removals.iter().copied())
        .filter(|&(at, b)| b > 0 && (start_ms..=now).contains(&at))
        .collect()
}

/// The used-space chart over the last 3 weeks, and where it starts (unix ms): 168 columns of 3
/// hours, each the hour buckets of [`aterm_vitals::history::FreeWeeks`] in it merged (504 hour
/// columns would be under a point wide). The columns keep the clock's 3-hour grid, the last
/// holding now, so a column's hours never change as the hours pass. Each removal the journal
/// holds is a tick under the axis at its hour's middle, and the largest is marked with its bytes
/// and its day: `freed`, not `aterm freed`, since a tab removes too.
fn weeks_trace(c: Ctx<'_>) -> (Trace, i64) {
    let snap = c.snap;
    let now = snap.at_unix_ms;
    let total = snap
        .volume
        .as_ref()
        .map(|v| v.total)
        .or_else(|| snap.free_weeks.volume().map(|v| v.total))
        .filter(|&t| t > 0);
    let start = (now.div_euclid(WEEK_COLUMN_MS) + 1 - WEEK_COLUMNS as i64) * WEEK_COLUMN_MS;
    let hours = snap.free_weeks.buckets(now);
    let first_hour = now.div_euclid(WEEK_HOUR_MS) + 1 - WEEK_BUCKETS as i64;
    let merged: Vec<Option<Agg>> = (0..WEEK_COLUMNS)
        .map(|j| {
            let from = start.div_euclid(WEEK_HOUR_MS) + (j * WEEK_MERGE) as i64;
            (from..from + WEEK_MERGE as i64)
                .filter_map(|h| {
                    let i = usize::try_from(h - first_hour).ok()?;
                    hours.get(i).copied().flatten()
                })
                .fold(None, |acc: Option<Agg>, b| {
                    let mut acc = acc.unwrap_or_default();
                    acc.merge(&b);
                    Some(acc)
                })
        })
        .collect();
    let (vals, peaks, heat) = used_columns(&merged, total);
    let at_col = |at: i64| (at + WEEK_HOUR_MS / 2 - start) as f64 / WEEK_COLUMN_MS as f64;
    let removed = weeks_removals(snap, start);
    let removals = removed.iter().map(|&(at, _)| at_col(at)).collect();
    let mark = removed
        .iter()
        .copied()
        .reduce(|a, b| if b.1 > a.1 { b } else { a })
        .map(|(at, b)| {
            (
                at_col(at),
                format!("freed {} \u{b7} {}", gb(b as f64), month_day(at, &c.loc)),
            )
        });
    let trace = Trace {
        vals,
        peaks,
        heat,
        under: WEEKS,
        min_span: 1.0,
        mark,
        gaps: Vec::new(),
        events: Vec::new(),
        removals,
        chips: Some(DiskSpan::Weeks),
    };
    (trace, start)
}

/// GB used, last 3 weeks: [`filling_section`]'s chart over [`weeks_trace`], its removals marked
/// and said as the chart marks them, by the hour (a tick an hour that had any); the rates and
/// `full in` stay the day's.
fn weeks_section(c: Ctx<'_>) -> Section<'_> {
    let snap = c.snap;
    let (trace, start) = weeks_trace(c);
    let mut said = trace_said(
        "GB used over the last 3 weeks",
        "gigabytes",
        &trace,
        (start, WEEK_COLUMN_MS),
        &c.loc,
    );
    let removed = weeks_removals(snap, start);
    let sum = removed.iter().fold(0_u64, |a, &(_, b)| a.saturating_add(b));
    let most = removed
        .iter()
        .copied()
        .reduce(|a, b| if b.1 > a.1 { b } else { a });
    if let Some((at, b)) = most {
        let speak = |b: u64| words::speak(&gb(b as f64));
        let day = month_day(at, &c.loc);
        if removed.len() == 1 {
            said.push_str(&format!(" Removed {} on {day}.", speak(b)));
        } else {
            said.push_str(&format!(
                " Removed {} in {} separate hours; the most, {} on {day}.",
                speak(sum),
                removed.len(),
                speak(b)
            ));
        }
    }
    let (growth, line) = filling_words(snap);
    trace_section(
        c,
        "GB used, last 3 weeks",
        growth,
        trace,
        said,
        "Filling",
        line,
        false,
    )
}

/// What the software did (`docs/DESIGN-disk-custody-2026-11-12.md` §4): the deeds the host read
/// off the disk journal's tail, newest first, each with when it was done, what it records, whose
/// it is (aterm, or the tab whose session did it or that a free-space ask concerns, the subject
/// of its words) and its bytes. The journal records the machine's disk, so the list ignores
/// ↑/↓'s scope. `None` with no deed, which leaves the lens as it was.
fn did_section(c: Ctx<'_>) -> Option<Section<'_>> {
    let snap = c.snap;
    let now = snap.at_unix_ms;
    let mut deeds: Vec<&DiskDeed> = snap.facts.disk.as_ref()?.deeds.iter().collect();
    if deeds.is_empty() {
        return None;
    }
    deeds.sort_by_key(|d| std::cmp::Reverse(d.at_unix_ms));
    let rows: Vec<Row> = deeds
        .iter()
        .map(|d| {
            let (chip, scope) = match &d.by {
                DeedBy::Aterm => (Some(ProcScope::Aterm), "aterm".to_owned()),
                DeedBy::Tab(Some(k)) => {
                    (Some(ProcScope::Tab(*k)), c.scope_word(ProcScope::Tab(*k)))
                }
                DeedBy::Tab(None) => (None, "a tab".to_owned()),
            };
            let mut name = format!(
                "{} {}",
                deed_when(d.at_unix_ms, now, &c.loc),
                deed_words(&d.kind, &d.by)
            );
            if d.n > 1 {
                name.push_str(&format!(" \u{d7}{}", d.n));
            }
            let figure = match (d.bytes.filter(|&b| b > 0), d.free) {
                (Some(b), _) => gb(b as f64),
                (None, Some(free)) => format!("{} free", gb(free as f64)),
                (None, None) => String::new(),
            };
            Row {
                chip,
                name,
                scope: Some(scope),
                share: None,
                figure,
            }
        })
        .collect();
    let first = rows[0].said();
    let mut line = Vec::new();
    if rows.len() > 1 {
        line.push(format!("{first} \u{b7} {} more", rows.len() - 1));
    }
    line.push(first);
    Some(list_section(
        c,
        "What the software did".into(),
        vec![
            "from the disk journal, newest first".into(),
            "newest first".into(),
        ],
        rows,
        None,
        4,
        "Journal",
        line,
        true,
    ))
}

/// A deed's words: what the journal's row says, its subject `by` (the scope column beside it).
/// A free-space ask's row whose session it names reads with that session as its subject (it was
/// asked, chosen, freed space); one that names none, as aterm's step.
fn deed_words(kind: &DeedKind, by: &DeedBy) -> String {
    let what = |w: &str| {
        if w.is_empty() {
            "a folder".to_owned()
        } else {
            w.to_owned()
        }
    };
    let session = matches!(by, DeedBy::Tab(_));
    match kind {
        DeedKind::Checked => "checked the disk".into(),
        DeedKind::Began(w) => format!("began removing {}", what(w)),
        DeedKind::Removed(w) => format!("removed {}", what(w)),
        DeedKind::CouldNotRemove(w) => format!("could not remove {}", what(w)),
        DeedKind::Refused(w) => format!("refused to remove {}", what(w)),
        DeedKind::Kept(w) => format!("kept {}", what(w)),
        DeedKind::Stopped => "stopped cleaning build caches".into(),
        DeedKind::ReserveReleased => "released the reserve file".into(),
        DeedKind::ReserveWritten => "wrote the reserve file".into(),
        DeedKind::ReserveRefused => "could not change the reserve file".into(),
        DeedKind::RanShort => "ran short of disk space".into(),
        DeedKind::Asked(said) => ask_words(said, session),
        DeedKind::Other(named) => named.clone(),
    }
}

/// A free-space ask's row (`aterm_agent::harness::disk::ask`) in words: its `said` word, read
/// with the session it names as its subject where it names one (`session`), else as aterm's
/// step; a word this build does not know reads as the row does, `free-ask` and its word.
fn ask_words(said: &str, session: bool) -> String {
    let words = match (said, session) {
        ("quiet", _) => "found room on the disk",
        ("freeing", _) => "freed space itself, asking no session",
        ("cooldown", _) => "held off asking: a session was asked lately",
        ("opened", _) => "began asking sessions to free space",
        ("nominating", true) => "was put forward to free space",
        ("nominating", false) => "gathered sessions to ask to free space",
        ("worker-runs", _) => "was freeing space as the disk worker",
        ("elected", _) => "was chosen to free space",
        ("owed", _) => "was to be asked to free space",
        ("asked", _) => "was asked to free space",
        ("freed", _) => "freed space when asked",
        ("handoff", _) => "missed its turn to free space",
        ("returned", _) => "was busy, so it was not asked",
        ("waiting", _) => "waited for a session it could ask",
        ("out", _) => "stopped asking sessions",
        ("settled", _) => "stopped asking: a session freed space",
        ("spent", _) => "had no asks left",
        ("ended", _) => "stopped asking: there is room again",
        ("step typed", _) => "was sent the request to free space",
        ("step wait", _) => "could not be sent the request yet",
        ("step released", _) => "may not be asked to free space",
        ("step gone", _) => "was asked no more",
        ("worker", _) => "started a disk worker",
        ("worker-started", _) => "began freeing space as the disk worker",
        ("worker-over", true) => "finished as the disk worker",
        ("worker-over", false) => "its disk worker finished",
        (other, _) => return format!("free-ask {other}"),
    };
    words.to_owned()
}

/// A size as Held by and Space now's key word it: the walker's last figure, never shown without
/// what it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Size {
    bytes: u64,
    /// The bytes are a lower bound.
    floor: bool,
    /// Why it was not measured, where it was not.
    unread: Option<&'static str>,
    /// The bytes were cut to what the larger holdings left of the used space
    /// ([`held::Shown::clamped`]), so they are less than the walker measured.
    cut: bool,
}

impl Size {
    /// `lines` together: a lower bound where any of them is one or was not measured, cut where
    /// any of them was, and not measured where none of them was.
    fn of<'l>(lines: impl IntoIterator<Item = &'l HeldLine>) -> Self {
        let mut sum = Self {
            bytes: 0,
            floor: false,
            unread: None,
            cut: false,
        };
        let mut measured = false;
        for line in lines {
            sum.bytes = sum.bytes.saturating_add(line.bytes);
            sum.floor |= line.floor || line.unread.is_some();
            sum.cut |= line.cut;
            measured |= line.unread.is_none();
            if sum.unread.is_none() {
                sum.unread = line.unread;
            }
        }
        if measured {
            sum.unread = None;
        }
        sum
    }

    /// As drawn: `12 GB`, `≥ 12 GB` for a lower bound, `12 GB cut` where it was cut to the used
    /// space (the bound no longer holds of what is shown), `–` where it was not measured.
    fn drawn(self) -> String {
        match (self.unread, self.cut, self.floor) {
            (Some(_), ..) => "\u{2013}".to_owned(),
            (None, true, _) => format!("{} cut", gb(self.bytes as f64)),
            (None, false, true) => format!("\u{2265} {}", gb(self.bytes as f64)),
            (None, false, false) => gb(self.bytes as f64),
        }
    }

    /// As said: the figure, `at least` it for a lower bound, that it was cut to fit the used
    /// space, or that it was not measured and why.
    fn said(self) -> String {
        match (self.unread, self.cut, self.floor) {
            (Some(why), ..) => format!("not measured ({why})"),
            (None, true, _) => format!("{}, cut to fit the used space", gb(self.bytes as f64)),
            (None, false, true) => format!("at least {}", gb(self.bytes as f64)),
            (None, false, false) => gb(self.bytes as f64),
        }
    }
}

/// One line of Held by: a holding as `sys disk held` prints it, or, last, the used bytes no
/// holding names. It carries no path: the lens draws none.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HeldLine {
    project: String,
    worktree: held::Worktree,
    mark: held::Mark,
    /// The tab of the session at work in it, 1-based, and that session's program.
    tab: Option<usize>,
    exe: Option<String>,
    /// The session those words name is the front tab's: every session at work that claims it
    /// is, so the nearest of them, whose tab and program the row holds, is too.
    mine: bool,
    /// Its bytes after the clamp to the used bytes, whether they are a lower bound, why they
    /// were not measured where they were not, and whether the clamp cut them.
    bytes: u64,
    floor: bool,
    unread: Option<&'static str>,
    cut: bool,
    class: held::Class,
    next: held::Next,
}

impl HeldLine {
    /// Whether it is the last line: the used bytes no holding names.
    fn is_other(&self) -> bool {
        self.worktree == held::Worktree::Other
    }

    fn size(&self) -> Size {
        Size {
            bytes: self.bytes,
            floor: self.floor,
            unread: self.unread,
            cut: self.cut,
        }
    }

    /// Its session in words: the program and tab of the one at work in it, as far as the row
    /// holds them, or that it is gone, or that there is none.
    fn session_words(&self) -> String {
        match (self.mark, self.exe.as_deref(), self.tab) {
            (held::Mark::AtWork, Some(exe), Some(tab)) => format!("{exe} tab {tab}"),
            (held::Mark::AtWork, None, Some(tab)) => format!("tab {tab}"),
            (held::Mark::AtWork, Some(exe), None) => exe.to_owned(),
            (held::Mark::AtWork, None, None) => "at work".to_owned(),
            (held::Mark::Gone, ..) => "session gone".to_owned(),
            (held::Mark::NoSession, ..) => "no session".to_owned(),
        }
    }
}

/// Held by's lines from the rows `h` the host gathered, as `sys disk held` prints them: one per
/// holding in [`held::balance`]'s order (largest first, the unmeasured last), its bytes cut to
/// what the rows above it leave of `used` (and marked cut where that took any of them, as the
/// reply's `clamped=` does), and where `used` is known a last line for the bytes no holding
/// names, so the lines add up to `used`. `panes` are the front tab's sessions. Every row the
/// lens draws or says, and each span of Space now's bar, comes from this list.
fn held_lines(h: &held::Holdings, used: Option<u64>, panes: &[u64]) -> Vec<HeldLine> {
    let balanced = held::balance(&h.rows, used);
    let mut lines: Vec<HeldLine> = balanced
        .rows
        .iter()
        .map(|shown| {
            let row = &h.rows[shown.i];
            let exe = row.exe.as_deref().map(exe_word).filter(|w| *w != "-");
            HeldLine {
                project: row.project.clone(),
                worktree: row.worktree,
                mark: row.mark,
                tab: row.tab,
                exe: exe.map(str::to_owned),
                mine: !row.keys.is_empty() && row.keys.iter().all(|k| panes.contains(k)),
                bytes: shown.bytes,
                floor: matches!(row.bytes, held::Bytes::AtLeast(_)),
                unread: match row.bytes {
                    held::Bytes::Unmeasured(why) => Some(why),
                    held::Bytes::Measured(_) | held::Bytes::AtLeast(_) => None,
                },
                cut: shown.clamped,
                class: row.class,
                next: row.next,
            }
        })
        .collect();
    if let Some(other) = balanced.other {
        lines.push(HeldLine {
            project: "other".to_owned(),
            worktree: held::Worktree::Other,
            mark: held::Mark::NoSession,
            tab: None,
            exe: None,
            mine: false,
            bytes: other,
            floor: false,
            unread: None,
            cut: false,
            class: held::Class::Unclassified,
            next: held::Next::Unknown,
        });
    }
    lines
}

/// The used bytes by class, for Space now's bar and its key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ClassSpans {
    kept: u64,
    /// What no holding names: the grey span holds it with the kept bytes, and it is never
    /// called kept.
    unclassified: u64,
    costly: u64,
    rebuildable: u64,
    /// The classed figures are lower bounds, and so the unclassified one an upper bound: a
    /// holding's size is a lower bound or was not measured, or a walk is still running.
    floor: bool,
    /// A holding's size was cut to what the larger ones left of the used space: the holdings
    /// measure more than is used, so nothing is left unclassified and no figure bounds the
    /// rest from above.
    cut: bool,
}

/// What Space now's key and Held by say where a holding's size was cut to the used space.
const CUT_BRIEF: &str = "sizes cut to fit";
const CUT_CLAUSE: &str =
    "the holdings measure more than the used space, so the smallest are cut to fit";

impl ClassSpans {
    /// The four figures, each apart, and that sizes were cut where they were.
    fn said(&self) -> String {
        let least = if self.floor { "at least " } else { "" };
        let most = if self.floor && !self.cut {
            "at most "
        } else {
            ""
        };
        let cut = if self.cut {
            format!(" \u{b7} {CUT_CLAUSE}")
        } else {
            String::new()
        };
        format!(
            "classes: kept {least}{} \u{b7} unclassified {most}{} \u{b7} costly {least}{} \u{b7} \
             rebuildable {least}{}{cut}",
            gb(self.kept as f64),
            gb(self.unclassified as f64),
            gb(self.costly as f64),
            gb(self.rebuildable as f64)
        )
    }
}

/// `lines`' bytes by class while the walks stand at `walk`; `None` with no line.
fn class_spans(lines: &[HeldLine], walk: held::Walk) -> Option<ClassSpans> {
    if lines.is_empty() {
        return None;
    }
    let sum = |class: held::Class| {
        lines
            .iter()
            .filter(|l| l.class == class)
            .fold(0_u64, |sum, l| sum.saturating_add(l.bytes))
    };
    Some(ClassSpans {
        kept: sum(held::Class::Kept),
        unclassified: sum(held::Class::Unclassified),
        costly: sum(held::Class::Costly),
        rebuildable: sum(held::Class::Rebuildable),
        floor: walk == held::Walk::Walking || lines.iter().any(|l| l.floor || l.unread.is_some()),
        cut: lines.iter().any(|l| l.cut),
    })
}

/// The one hue of the bytes aterm can have again: aterm's ink (§11.3), `aterm_ic` under
/// Increase Contrast. It is no heat, warn or critical ink: the lens keeps those for how full the
/// disk is.
fn class_hue(pen: &Pen<'_>) -> Rgb {
    chip_ink(pen, ProcScope::Aterm, &[])
}

/// The ink of a hatch's strokes over [`class_hue`]: the track's, so the strokes read as gaps in
/// the fill, where the hue stands off the track by 2:1 (3:1 under Increase Contrast, the floor
/// of a mark); else the secondary words', or the words' where those stand off the hue further
/// and the secondary ones do not reach that floor. On a black ground the track is nearly as
/// dark as the hue is dim, and a hatch nobody can see would draw costly bytes as rebuildable.
fn hatch_ink(pen: &Pen<'_>) -> Rgb {
    let hue = class_hue(pen);
    let floor = if pen.ic { ink::NON_TEXT } else { 2.0 };
    let stands = |c: Rgb| ink::contrast(hue, c);
    let (track, words2, words) = (pen.inks.track, pen.inks.words2, pen.inks.words);
    if stands(track) >= floor {
        track
    } else if stands(words2) >= floor || stands(words2) >= stands(words) {
        words2
    } else {
        words
    }
}

/// A class's swatch, 6 by 7 pt on the baseline `base`: the machine's ink for what is kept or not
/// classified, [`class_hue`] for what a build makes again, and that hue hatched
/// ([`hatch_ink`]) for what is costly to have again. No swatch is drawn without its word.
fn class_swatch(pen: &mut Pen<'_>, x: f64, base: f64, class: held::Class) {
    let (y, w, h) = (base - 7.0, 6.0, 7.0);
    match class {
        held::Class::Kept | held::Class::Unclassified => {
            let ink = pen.machine();
            pen.rect(x, y, w, h, 1.0, rgba(ink, 0xFF));
        }
        held::Class::Costly | held::Class::Rebuildable => {
            let hue = class_hue(pen);
            pen.rect(x, y, w, h, 1.0, rgba(hue, 0xFF));
            if class == held::Class::Costly {
                let stroke = hatch_ink(pen);
                pen.hatch(x, y, w, h, stroke);
            }
        }
    }
}

/// Space now's key, on its heading's line: what each fill of the bar is, and the cap's figure.
#[derive(Clone, Debug, Default)]
struct SpaceKey {
    /// Each class with bytes, left to right as the bar lays them: its swatch's class, its word
    /// and its figure. Empty where nothing is costly or rebuildable: the bar is then one fill.
    entries: Vec<(held::Class, &'static str, String)>,
    /// The walk in a few words while it is not complete, else that sizes were cut where they
    /// were: no figure is drawn without it, and it stays beside the words alone.
    brief: Option<&'static str>,
    /// `cap 1.4 TB`, where the budget has one.
    cap: Option<String>,
}

/// Space now's key for `spans` while the walks stand at `walk`, with the budget's `cap`. The
/// grey fill holds the kept bytes and what no holding names, so its word says both; where the
/// figures are bounds, the classed ones read `≥` and the grey one, the rest of the used bytes,
/// `≤`, which it loses where sizes were cut (the rest is then nothing, and the kept bytes are
/// cut like the others).
fn space_key(spans: Option<ClassSpans>, walk: held::Walk, cap: Option<u64>) -> SpaceKey {
    let cut = spans.is_some_and(|s| s.cut);
    let mut key = SpaceKey {
        entries: Vec::new(),
        brief: walk.brief().or(cut.then_some(CUT_BRIEF)),
        cap: cap.map(|cap| format!("cap {}", gb(cap as f64))),
    };
    let Some(s) = spans.filter(|s| s.costly > 0 || s.rebuildable > 0) else {
        return key;
    };
    let least = if s.floor { "\u{2265} " } else { "" };
    let most = if s.floor && !s.cut { "\u{2264} " } else { "" };
    let grey = s.kept.saturating_add(s.unclassified);
    for (class, word, bytes, sign) in [
        (held::Class::Kept, "kept and unclassified", grey, most),
        (held::Class::Costly, "costly", s.costly, least),
        (
            held::Class::Rebuildable,
            "rebuildable",
            s.rebuildable,
            least,
        ),
    ] {
        if bytes > 0 {
            key.entries
                .push((class, word, format!("{sign}{}", gb(bytes as f64))));
        }
    }
    key
}

/// `key` on the heading's baseline of `p`, which starts after the heading's name: the first
/// that fits of each class's swatch, word and figure and of each swatch and word alone, either
/// with the brief after it (the walk's while a walk is not complete) and the cap's figure ending
/// at the right edge; then the words and the brief without the cap's figure, then the words
/// alone. A fill is told by its hue, so its word outlasts the cap's figure, which shows alone
/// only where no word fits; and the bar's spans are sizes, so the brief outlasts it too.
fn draw_space_key(pen: &mut Pen<'_>, p: Place, key: &SpaceKey) {
    /// A swatch and the gap after it, the gap between two entries, and the gap before the cap.
    const SWATCH: f64 = 9.0;
    const BETWEEN: f64 = 10.0;
    const BEFORE_CAP: f64 = 12.0;
    let base = p.y + HEAD_BASE;
    let ink = pen.inks.words2;
    let cap_w = key
        .cap
        .as_deref()
        .map_or(0.0, |cap| pen.width(cap, NOTE_PT, false));
    let cap_room = if key.cap.is_some() {
        cap_w + BEFORE_CAP
    } else {
        0.0
    };
    let tail = key
        .brief
        .map_or_else(String::new, |brief| format!("\u{b7} {brief}"));
    let figures: Vec<String> = key
        .entries
        .iter()
        .map(|(_, word, figure)| format!("{word} {figure}"))
        .collect();
    let words: Vec<String> = key
        .entries
        .iter()
        .map(|(_, word, _)| (*word).to_owned())
        .collect();
    let span = |texts: &[String], tail: &str| {
        let entries: f64 = texts
            .iter()
            .map(|t| SWATCH + pen.width(t, NOTE_PT, false))
            .sum();
        let after = if tail.is_empty() {
            0.0
        } else {
            4.0 + pen.width(tail, NOTE_PT, false)
        };
        entries + BETWEEN * texts.len().saturating_sub(1) as f64 + after
    };
    let chosen = [
        (&figures, tail.as_str(), true),
        (&words, tail.as_str(), true),
        (&words, tail.as_str(), false),
        (&words, "", false),
    ]
    .into_iter()
    .find(|&(texts, tail, capped)| {
        let room = if capped { cap_room } else { 0.0 };
        !texts.is_empty() && span(texts, tail) + room <= p.w + EPS
    });
    if let Some((texts, tail, _)) = chosen {
        let mut x = p.x;
        for ((class, ..), text) in key.entries.iter().zip(texts) {
            class_swatch(pen, x, base, *class);
            x += SWATCH;
            x += pen.text(x, base, text, NOTE_PT, false, ink) + BETWEEN;
        }
        pen.text(x - BETWEEN + 4.0, base, tail, NOTE_PT, false, ink);
    }
    if chosen.is_none_or(|(.., capped)| capped)
        && let Some(cap) = key.cap.as_deref()
        && cap_w <= p.w + EPS
    {
        pen.text_end(p.x + p.w, base, cap, NOTE_PT, false, ink);
    }
}

/// What one drawn line of Held by is.
#[derive(Clone, Debug, PartialEq, Eq)]
enum HeldShown {
    Line(HeldLine),
    /// The lines a list too short for them all folds into one: how many, and their size.
    Fold(usize, Size),
}

/// The `n` lines a list of `lines` shows: all of them where they fit, else the first, one line
/// for those folded, and the last where it is the bytes no holding names, which every list
/// keeps so that its lines still add up.
fn held_shown(lines: &[HeldLine], n: usize) -> Vec<HeldShown> {
    if lines.len() <= n {
        return lines.iter().cloned().map(HeldShown::Line).collect();
    }
    let other = lines.last().filter(|l| l.is_other());
    let tail = usize::from(other.is_some());
    let head = n.saturating_sub(1 + tail);
    let folded = &lines[head..lines.len() - tail];
    let mut out: Vec<HeldShown> = lines[..head].iter().cloned().map(HeldShown::Line).collect();
    out.push(HeldShown::Fold(folded.len(), Size::of(folded)));
    out.extend(other.cloned().map(HeldShown::Line));
    out
}

/// Held by's lines regrouped by who holds them, each group with its size: each program at work,
/// largest first; the sessions that are gone; aterm's own folders that no session claims; the
/// other folders no session claims; and, always an entry of its own, what no holding names. The
/// second figure is how many of the groups, from the first, are programs.
fn by_program(lines: &[HeldLine]) -> (Vec<(String, Size)>, usize) {
    let mut programs: Vec<(String, Vec<&HeldLine>)> = Vec::new();
    let (mut gone, mut own, mut none, mut other) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for line in lines {
        match (line.mark, line.worktree) {
            (_, held::Worktree::Other) => other.push(line),
            (held::Mark::AtWork, _) => {
                let name = line.exe.as_deref().unwrap_or("at work");
                match programs.iter_mut().find(|(n, _)| n == name) {
                    Some((_, of)) => of.push(line),
                    None => programs.push((name.to_owned(), vec![line])),
                }
            }
            (held::Mark::Gone, _) => gone.push(line),
            (
                held::Mark::NoSession,
                held::Worktree::Aterm | held::Worktree::Cache | held::Worktree::Tooling,
            ) => own.push(line),
            (held::Mark::NoSession, _) => none.push(line),
        }
    }
    let mut groups: Vec<(String, Size)> = programs
        .into_iter()
        .map(|(name, of)| (name, Size::of(of)))
        .collect();
    groups.sort_by_key(|(_, size)| std::cmp::Reverse(size.bytes));
    let n = groups.len();
    for (name, of) in [
        ("gone", gone),
        ("aterm's own", own),
        ("no session", none),
        ("unclassified", other),
    ] {
        if !of.is_empty() {
            groups.push((name.to_owned(), Size::of(of)));
        }
    }
    (groups, n)
}

/// `clause` as a sentence: its first letter a capital, and a period after it.
fn sentence(clause: &str) -> String {
    let mut chars = clause.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    format!("{}{}.", first.to_uppercase(), chars.as_str())
}

/// Held by (`docs/DESIGN-disk-custody-2026-11-12.md` §4): who holds home's volume, from the rows
/// the host last gathered for `sys disk held` ([`held_lines`]): each holding's project and kind
/// of folder, a filled mark with the program and tab of the session at work in it or a hollow
/// one where that session is gone, its class in a swatch and a word, its size, and last what
/// aterm's next pass does with it by itself, in words: nothing here is a button, and nothing
/// acts. The lines add up to the used space, the last being what no holding names; a list too
/// short for them all folds the smallest into one line. The heading regroups them by program
/// ([`by_program`]). The sizes are the disk walker's last and the rows the host's last gather:
/// the lens starts no walk and no gather and waits on neither, so it says what it draws from.
/// While a walk is not complete its words stay beside every size, in the heading and at the
/// head of the one-line form; before them go how old the rows are, once they are older than an
/// open lens keeps them ([`HELD_STALE_S`]: the frame a lens opens on, or one after every window
/// was hidden), and how old the sizes may be, once the oldest walk behind them is
/// ([`HELD_OLD_WALK_S`]). A size the clamp to the used space cut reads `cut`. The volume is the
/// machine's, so the list ignores ↑/↓'s scope. `None` with no line, which leaves the lens as it
/// was.
fn held_section(c: Ctx<'_>) -> Option<Section<'_>> {
    let snap = c.snap;
    let disk = snap.facts.disk.as_ref()?;
    let h = disk.held.as_deref()?;
    let used = snap.volume.as_ref().map(|v| v.used());
    let lines = held_lines(h, used, c.panes);
    if lines.is_empty() {
        return None;
    }
    let walk = h.walk;
    let (groups, programs) = by_program(&lines);
    let drawn_key = |groups: &[(String, Size)]| {
        let entries: Vec<String> = groups
            .iter()
            .map(|(name, size)| format!("{name} {}", size.drawn()))
            .collect();
        entries.join(" \u{b7} ")
    };
    let key = drawn_key(&groups);
    // What the rows and their sizes stand on, where that is not a fresh gather over complete,
    // recent walks: how old the rows are, how old the sizes may be, and where the walks are.
    let ago = |secs: u64| {
        if secs < 60 {
            format!("{secs} s")
        } else {
            words::duration_approx(secs as f64)
        }
    };
    let as_of = disk
        .held_age_s
        .filter(|&age| age > HELD_STALE_S)
        .map(|age| format!("as of {} ago", ago(age)));
    let old = h
        .oldest_walk_s
        .filter(|&age| age >= HELD_OLD_WALK_S)
        .map(|age| format!("sizes up to {} old", ago(age)));
    let lead_with = |walk: Option<&str>| -> Option<String> {
        let parts: Vec<&str> = as_of
            .as_deref()
            .into_iter()
            .chain(old.as_deref())
            .chain(walk)
            .collect();
        (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
    };
    let (long, brief) = (lead_with(walk.clause()), lead_with(walk.brief()));
    // With nothing to say of them the heading says the key, or its programs alone; else those
    // words lead and are cut before they are dropped.
    let subs: Vec<String> = match (&long, &brief) {
        (Some(long), Some(brief)) => {
            vec![format!("{long} \u{b7} {key}"), long.clone(), brief.clone()]
        }
        _ => {
            let mut subs = vec![key];
            if programs > 0 && programs < groups.len() {
                subs.push(drawn_key(&groups[..programs]));
            }
            subs
        }
    };
    let led = brief.is_some();

    let said_key: Vec<String> = groups
        .iter()
        .map(|(name, size)| format!("{name} {}", size.said()))
        .collect();
    let mut words = vec![match used {
        Some(used) => format!(
            "Largest first; the rows add up to {} used.",
            gb(used as f64)
        ),
        None => "Largest first.".to_owned(),
    }];
    words.push(format!("By program: {}.", said_key.join(", ")));
    if lines.iter().any(|l| l.cut) {
        words.push(sentence(CUT_CLAUSE));
    }
    words.extend(as_of.as_deref().map(sentence));
    words.extend(old.as_deref().map(sentence));
    words.extend(walk.clause().map(sentence));
    let parts: Vec<(String, String)> = held_shown(&lines, HELD_SAID)
        .iter()
        .map(|shown| match shown {
            HeldShown::Line(l) => (
                format!("{} {}", l.project, l.worktree.word()),
                format!(
                    "{}, {}, {}; next: {}",
                    l.session_words(),
                    l.size().said(),
                    l.class.word(),
                    l.next.says()
                ),
            ),
            HeldShown::Fold(k, size) => (format!("{k} more"), size.said()),
        })
        .collect();

    // The one-line form: the largest holding, how many more there are and what no holding
    // names, each form led by the brief words on what they stand on so that a cut keeps them.
    let lead = brief
        .as_ref()
        .map_or_else(String::new, |brief| format!("{brief} \u{b7} "));
    let holdings: Vec<&HeldLine> = lines.iter().filter(|l| !l.is_other()).collect();
    let other = lines
        .iter()
        .find(|l| l.is_other())
        .map(|l| format!("{} unclassified", l.size().drawn()));
    let line: Vec<String> = match holdings.first() {
        Some(first) => {
            let tail = other.map_or_else(String::new, |other| format!(" \u{b7} {other}"));
            let more = match holdings.len() - 1 {
                0 => String::new(),
                more => format!(" \u{b7} {more} more"),
            };
            let size = first.size().drawn();
            let count = match holdings.len() {
                1 => "1 holding".to_owned(),
                n => format!("{n} holdings"),
            };
            vec![
                format!(
                    "{lead}{} {}, {}, {size}{more}{tail}",
                    first.project,
                    first.worktree.word(),
                    first.session_words()
                ),
                format!("{lead}{}, {size}{more}{tail}", first.project),
                format!(
                    "{lead}{count} {}{tail}",
                    Size::of(holdings.iter().copied()).drawn()
                ),
            ]
        }
        None => vec![format!("{lead}{}", other.unwrap_or_default())],
    };

    let panes = c.panes;
    let most = lines.len().min(HELD_MOST);
    let least = lines.len().min(HELD_LEAST);
    let mut forms = Vec::new();
    for n in (least..=most).rev() {
        let (shown, subs) = (held_shown(&lines, n), subs.clone());
        forms.push(form(rows_h(n, false), move |pen, p| {
            if led {
                heading_kept(pen, p, "Held by", &subs);
            } else {
                heading(pen, p, "Held by", &subs);
            }
            draw_held(pen, p, &shown, panes);
        }));
    }
    forms.push(line_form("Held", line));
    Some(Section {
        name: "Held by".into(),
        words,
        right: true,
        parts,
        forms,
    })
}

/// One drawn line of Held by, in the words of each of its columns.
struct HeldCells {
    /// Its class, for its swatch; none on the fold.
    class: Option<held::Class>,
    project: String,
    kind: &'static str,
    /// Its session's mark, where it has one: filled at work and hollow gone, and whether the
    /// session its words name is the front tab's ([`HeldLine::mine`]).
    mark: Option<(bool, bool)>,
    session: String,
    class_word: &'static str,
    size: String,
    next: Option<held::Next>,
}

impl HeldCells {
    fn of(shown: &HeldShown) -> Self {
        match shown {
            HeldShown::Line(l) => Self {
                class: Some(l.class),
                project: l.project.clone(),
                kind: l.worktree.word(),
                mark: match l.mark {
                    held::Mark::AtWork => Some((true, l.mine)),
                    held::Mark::Gone => Some((false, false)),
                    held::Mark::NoSession => None,
                },
                session: l.session_words(),
                class_word: l.class.word(),
                size: l.size().drawn(),
                next: Some(l.next),
            },
            HeldShown::Fold(k, size) => Self {
                class: None,
                project: format!("{k} more"),
                kind: "",
                mark: None,
                session: String::new(),
                class_word: "",
                size: size.drawn(),
                next: None,
            },
        }
    }
}

/// The widest of `texts` at the body size.
fn widest<'t>(pen: &Pen<'_>, texts: impl Iterator<Item = &'t str>) -> f64 {
    texts
        .map(|t| pen.width(t, BODY_PT, false))
        .fold(0.0, f64::max)
}

/// Held by's `shown` lines under its heading at `p`'s top, one a pitch, in columns: the class's
/// swatch, the project, the kind of folder, the session's mark and words, the class's word, the
/// size ending its column, and last what the next pass does. That column is as wide as its
/// widest statement, so none is cut: every line's brief words ([`held::Next::brief`], the long
/// ones on a line where they fit), else every line's short ones ([`held::Next::short`]), else
/// no column, and the said form has the whole statement. Every other text is cut to its own
/// column. A section too narrow for them all sheds, in this order, the next pass, the kind, the
/// session's words (its mark stays) and last the class's word with its swatch: a class is told
/// by its swatch's hue, and no hue is drawn without its word. The project and the size never
/// go, and with no next pass the sizes end at the section's edge as every list's figures do.
fn draw_held(pen: &mut Pen<'_>, p: Place, shown: &[HeldShown], panes: &[u64]) {
    /// The swatch and the gap after it, the gap between two columns, and the mark's room.
    const LEAD: f64 = 10.0;
    const BETWEEN: f64 = 8.0;
    const MARK: f64 = 10.0;
    let inks = pen.inks;
    let cells: Vec<HeldCells> = shown.iter().map(HeldCells::of).collect();
    let size_w = widest(pen, cells.iter().map(|c| c.size.as_str()));
    let kind_w = widest(pen, cells.iter().map(|c| c.kind));
    let session_w = widest(pen, cells.iter().map(|c| c.session.as_str()));
    let class_w = widest(pen, cells.iter().map(|c| c.class_word));
    let briefs: Vec<String> = cells
        .iter()
        .filter_map(|c| c.next.map(held::Next::brief))
        .collect();
    let brief_w = widest(pen, briefs.iter().map(String::as_str));
    let short_w = widest(
        pen,
        cells.iter().filter_map(|c| c.next.map(held::Next::short)),
    );
    let project_w =
        widest(pen, cells.iter().map(|c| c.project.as_str())).min((p.w * 0.28).max(40.0));
    // `next` is the next pass's column's width, 0 with no column.
    let need = |next: f64, class: bool, kind: bool, words: bool| {
        let column = |on: bool, w: f64| if on { BETWEEN + w } else { 0.0 };
        LEAD + project_w
            + column(kind, kind_w)
            + BETWEEN
            + MARK
            + if words { session_w } else { 0.0 }
            + column(class, class_w)
            + BETWEEN
            + size_w
            + column(next > 0.0, next)
    };
    let (next_col, class, kind, words) = [
        (brief_w, true, true, true),
        (short_w, true, true, true),
        (0.0, true, true, true),
        (0.0, true, false, true),
        (0.0, true, false, false),
        (0.0, false, false, false),
    ]
    .into_iter()
    .find(|&(next, class, kind, words)| need(next, class, kind, words) <= p.w + EPS)
    .unwrap_or((0.0, false, false, false));
    let next = next_col > 0.0;
    // Where even the mark and the size leave too little, the project gives its room up.
    let over = (need(next_col, class, kind, words) - p.w).max(0.0);
    let project_w = (project_w - over).max(0.0);
    let kind_x = p.x + LEAD + project_w + BETWEEN;
    let mark_x = if kind {
        kind_x + kind_w + BETWEEN
    } else {
        kind_x
    };
    let class_x = mark_x + MARK + if words { session_w } else { 0.0 } + BETWEEN;
    let size_x = if class {
        class_x + class_w + BETWEEN
    } else {
        class_x
    };
    let size_right = if next { size_x + size_w } else { p.x + p.w };
    let next_x = size_right + BETWEEN;
    let next_w = p.x + p.w - next_x;
    for (i, cell) in cells.iter().enumerate() {
        let base = p.y + ROW0_BASE + PITCH * i as f64;
        if class && let Some(of) = cell.class {
            class_swatch(pen, p.x, base, of);
        }
        let project = pen.cut(&cell.project, project_w, BODY_PT, false);
        let ink = if cell.class.is_some() {
            inks.words
        } else {
            inks.words2
        };
        pen.text(p.x + LEAD, base, &project, BODY_PT, false, ink);
        if kind {
            let s = pen.cut(cell.kind, kind_w, BODY_PT, false);
            pen.text(kind_x, base, &s, BODY_PT, false, inks.words2);
        }
        let at_work = matches!(cell.mark, Some((true, _)));
        if let Some((filled, mine)) = cell.mark {
            // The front tab's session takes the tab's ink, as its chips do.
            let tab = panes
                .first()
                .map(|&pane| chip_ink(pen, ProcScope::Tab(pane), panes));
            let ink = match (filled, mine) {
                (true, true) => tab.unwrap_or(inks.words),
                (true, false) => inks.words,
                (false, _) => inks.words2,
            };
            pen.dot(mark_x + 3.5, base - 3.4, 2.75, filled, ink);
        }
        if words {
            let s = pen.cut(&cell.session, session_w, BODY_PT, false);
            let ink = if at_work { inks.words } else { inks.words2 };
            pen.text(mark_x + MARK, base, &s, BODY_PT, false, ink);
        }
        if class {
            let s = pen.cut(cell.class_word, class_w, BODY_PT, false);
            pen.text(class_x, base, &s, BODY_PT, false, inks.words2);
        }
        let size = pen.cut(&cell.size, p.w - LEAD, BODY_PT, false);
        pen.text_end(size_right, base, &size, BODY_PT, false, inks.words);
        if next && let Some(pass) = cell.next {
            // The column holds the widest of the form it was sized by, so the last of these fits.
            let forms = [pass.says(), pass.brief(), pass.short().to_owned()];
            let s = fit_cut(pen.m, &forms, next_w.max(next_col), BODY_PT, false);
            pen.text(next_x, base, &s, BODY_PT, false, inks.words2);
        }
    }
}

/// Where the space is (§7.4, §7.7).
fn where_section(c: Ctx<'_>) -> Section<'_> {
    let snap = c.snap;
    let d = snap.facts.disk.as_ref();
    let hogs = d.map_or(&[][..], |d| d.hogs.as_slice());
    let sizing = d.is_some_and(|d| d.sizing);
    let at = d
        .and_then(|d| d.hogs_at)
        .map(|t| c.loc.clock(t, snap.at_unix_ms));
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

/// The CPU lens's ⌘⇧P line: what the key does and what it does not, one row in the two-column
/// lens's 476 pt column at a 1036.5 pt Folded deck; a narrower column wraps it.
#[cfg(target_os = "macos")]
const CPU_POLICY_KEY: &str =
    "\u{2318}\u{21e7}P reads CPU policy once, by password; changes no policy, keeps no password.";

/// Whether ⌘⇧C in an open CPU lens copies its diagnostic report on `platform`, and so whether the
/// lens says so: where aterm's hardcoded ⌘ (Super) shortcut suite is live
/// ([`crate::app_input::HARDCODED_SUPER_CHORDS`]), never on Linux. There the lens still takes the
/// chord as its ⌘C (Shift aside), which copies its sentence.
pub(crate) const fn cpu_report_key(platform: Platform) -> bool {
    !matches!(platform, Platform::Linux)
}

/// The CPU lens (§11.4).
fn cpu_doc(c: Ctx<'_>) -> LensDoc<'_> {
    let snap = c.snap;
    let n = snap.total_cores().unwrap_or(0);
    let at = c.at();
    let sources = match snap.cpu.as_ref().filter(|_| n > 0) {
        Some(cpu) => {
            let clusters_word = if cpu.clusters.len() == 1 {
                "cluster"
            } else {
                "clusters"
            };
            let mut out = Vec::new();
            if let Some(brand) = snap.facts.cpu_brand.as_deref().filter(|b| !b.is_empty()) {
                out.push(format!(
                    "{brand} \u{b7} {n} cores in {} {clusters_word} \u{b7} as of {at}",
                    cpu.clusters.len(),
                ));
                out.push(format!("{brand} \u{b7} {n} cores \u{b7} as of {at}"));
            }
            out.push(format!(
                "{n} cores in {} {clusters_word} \u{b7} as of {at}",
                cpu.clusters.len(),
            ));
            out.push(format!("{n} cores \u{b7} as of {at}"));
            out.push(format!("as of {at}"));
            out
        }
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
    // Ten-second columns' worth of over readings: each column adds its over share.
    let over = view
        .columns(10, 60, snap.at_unix_ms)
        .iter()
        .map(aterm_vitals::ring::Cell::over_share)
        .sum::<f64>()
        .round() as usize;
    let over_words = match over {
        0 => "not in the last 10 min".to_owned(),
        k if k < 6 => format!("{} s of the last 10 min", k * 10),
        k => format!("{:.0} of the last 10 min", (k as f64 / 6.0).round()),
    };
    let load = match (snap.facts.load1, n) {
        // Linux's load counts threads waiting on disk too, so its excess over the cores is no
        // count of the threads waiting for one.
        (Some(l), _) if c.loc.platform == Platform::Linux => vec![
            format!("1-minute load {l:.1} (counts disk waits too)"),
            format!("1-minute load {l:.1}"),
        ],
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
    if snap.hardware.is_some() || snap.authorized_cpu_policy.is_some() {
        let text = aterm_vitals::sys::hardware::lines(snap);
        // The full report preserves every channel. Keep the panel's recovery
        // actions first so smaller lenses do not hide them below the channel
        // list, each wrapped to the column so no width cuts what one says.
        #[cfg(target_os = "macos")]
        let policy = Some(if crate::cpu_policy_authorization::pending() {
            "Authorized collection pending; ⌘⇧X cancels. No new collection until its exact collector exits.".to_owned()
        } else {
            CPU_POLICY_KEY.to_owned()
        });
        #[cfg(not(target_os = "macos"))]
        let policy: Option<String> = None;
        let keys: Vec<String> = cpu_report_key(c.loc.platform)
            .then(|| {
                "\u{2318}\u{21e7}C copies the diagnostic report. Cause not established.".to_owned()
            })
            .into_iter()
            .chain(policy)
            .collect();
        let items: Vec<Vec<String>> = keys
            .iter()
            .map(|k| wrap(c.m, k, c.cw, BODY_PT))
            .chain(text.iter().map(|l| vec![l.clone()]))
            .collect();
        let line = keys.first().or(text.first()).cloned().into_iter().collect();
        let words = keys.into_iter().chain(text).collect();
        if !items.is_empty() {
            sections.insert(
                0,
                rows_section("Hardware & limits", items, line, words, true),
            );
        }
    }
    sections.extend(why);
    sections.extend(cpu_accounting_section(c));
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
    let mut programs: Vec<_> = snap
        .programs
        .iter()
        .filter(|t| c.in_scope(t.scope) && t.cores > 0.0)
        .collect();
    programs.sort_by(|a, b| b.cores.total_cmp(&a.cores));
    let rows: Vec<Row> = programs
        .into_iter()
        .map(|t| Row {
            chip: Some(t.scope),
            name: program_label(&t.name, t.n as usize),
            scope: Some(c.scope_word(t.scope)),
            share: Some(f64::from(t.cores) / n),
            figure: format!("{} cores", words::cores(t.cores)),
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

fn cpu_accounting_section(c: Ctx<'_>) -> Option<Section<'_>> {
    let view = c.view(Resource::Cpu);
    let figures = [
        ("aterm", ProcScope::Aterm, &view.aterm),
        (
            c.tab_label,
            ProcScope::Tab(c.panes.first().copied().unwrap_or(0)),
            &view.tab,
        ),
    ];
    let rows: Vec<Row> = figures
        .into_iter()
        .filter_map(|(name, scope, fig)| {
            if c.bar == Bar::Tab && scope == ProcScope::Aterm {
                return None;
            }
            if matches!(scope, ProcScope::Tab(0)) {
                return None;
            }
            let charge = words::cpu_charge_words(fig)?;
            let actual = fig.now?;
            let (prefix, shown) = if fig.unread > 0 {
                ("≥", (actual * 100.0).floor() / 100.0)
            } else {
                ("", actual)
            };
            Some(Row {
                chip: Some(scope),
                name: name.into(),
                scope: None,
                share: None,
                figure: format!("{prefix}{shown:.2} confirmed · {charge}"),
            })
        })
        .collect();
    if rows.is_empty() {
        return None;
    }
    let line = vec![rows.iter().map(Row::said).collect::<Vec<_>>().join(" · ")];
    Some(list_section(
        c,
        "CPU accounting".into(),
        vec!["cores · confirmed CPU and counter charges".into()],
        rows,
        Some(words::CPU_CHARGE_NOTE.into()),
        2,
        "CPU accounting",
        line,
        true,
    ))
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
        CauseId::AtermWork => (
            true,
            "aterm outside tabs".to_owned(),
            "aterm outside tabs".to_owned(),
        ),
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
    let exact = if f.split.exact {
        "exact, "
    } else {
        "partial, "
    };
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
    let explainer = "Heat is how much memory is in use: apps, wired and compressed. Available \
                     is what the system can hand out without swapping."
        .to_owned();
    let now = snap.at_unix_ms;
    let (vals, peaks, heat) = hour_columns(&c.view(Resource::Memory), now, 1_073_741_824.0);
    let start = (now.div_euclid(60_000) - 59) * 60_000;
    let (gaps, events) = trace_overlays(snap, start, 60_000, vals.len());
    let trace = Trace {
        vals,
        peaks,
        heat,
        under: HOUR,
        min_span: 0.5,
        mark: None,
        gaps,
        events,
        removals: Vec::new(),
        chips: None,
    };
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
        memory_capacity_section(c),
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
        keeps_first: true,
    }
}

/// The measured physical capacity, kept even when the lens has room for no history.
fn memory_capacity_section(c: Ctx<'_>) -> Section<'_> {
    let Some(m) = c.snap.mem.as_ref().filter(|m| m.sample.total > 0) else {
        return line_section("Capacity", "Capacity", vec!["not read yet".into()], false);
    };
    let sample = m.sample;
    let line = words::memory_space_line(&sample);
    let short = format!(
        "{} used · {} available · {} total GiB",
        words::gib_figure(sample.used()),
        words::gib_figure(sample.available),
        words::gib_total(sample.total)
    );
    let rows = [
        format!("Used {} GiB", words::gib_figure(sample.used())),
        format!(
            "Available {} GiB (without swapping)",
            words::gib_figure(sample.available)
        ),
        format!("Total {} GiB", words::gib_total(sample.total)),
    ];
    let full = form(rows_h(rows.len(), false), move |pen, p| {
        heading(pen, p, "Capacity", &[]);
        let ink = pen.inks.words2;
        for (i, row) in rows.iter().enumerate() {
            let text = pen.cut(row, p.w, BODY_PT, false);
            pen.text(
                p.x,
                p.y + ROW0_BASE + PITCH * i as f64,
                &text,
                BODY_PT,
                false,
                ink,
            );
        }
    });
    Section {
        name: "Capacity".into(),
        words: vec![line.clone()],
        right: false,
        parts: Vec::new(),
        forms: vec![full, line_form("Capacity", vec![line, short])],
    }
}

/// The physical page classes, with a separately marked residual and a complete legend.
fn classes_section(c: Ctx<'_>) -> Section<'_> {
    use aterm_vitals::model::{MemoryClassKind, MemoryPartition};
    let Some(m) = c.snap.mem.as_ref() else {
        return line_section("Where it is", "Classes", vec!["not read yet".into()], false);
    };
    let physical = m.sample.physical_classes(Some(c.loc.platform));
    let exact = physical.partition == MemoryPartition::Exact;
    let classes: Vec<_> = physical
        .classes
        .iter()
        .map(|class| {
            let name = words::memory_class_name(class.kind, physical.partition);
            let value = class.bytes.map_or_else(|| "not read".into(), words::mem);
            (class.kind, class.bytes, format!("{name} {value}"))
        })
        .collect();
    let status = words::memory_classes_status(&physical, m.sample.used());
    let words = vec![
        classes
            .iter()
            .map(|class| class.2.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        status.clone(),
    ];
    let status_lines = if exact {
        Vec::new()
    } else {
        wrap(c.m, &status, c.cw, NOTE_PT)
    };
    let (mut x, mut row, mut rows) = (0.0, 0_usize, 1_usize);
    let legend: Vec<_> = classes
        .iter()
        .enumerate()
        .map(|(i, class)| {
            let lines = wrap(c.m, &class.2, (c.cw - 8.0).max(0.0), NOTE_PT);
            let width = 8.0
                + lines
                    .iter()
                    .map(|line| c.m.width(line, NOTE_PT, false))
                    .fold(0.0, f64::max);
            if x > 0.0 && (lines.len() > 1 || x + width > c.cw + EPS) {
                x = 0.0;
                row += 1;
            }
            let entry = (i, x, row, lines);
            rows = rows.max(row + entry.3.len());
            if entry.3.len() > 1 {
                row += entry.3.len();
                x = 0.0;
            } else {
                x += width + 10.0;
            }
            entry
        })
        .collect();
    let base = if exact {
        39.0
    } else {
        ROW0_BASE + PITCH * status_lines.len() as f64
    };
    let full = form(
        base + 3.0 + PITCH * rows.saturating_sub(1) as f64,
        move |pen, p| {
            if exact {
                heading(pen, p, "Where it is", std::slice::from_ref(&status));
                let (by, bh) = (p.y + 16.0, 10.0);
                let mut x = p.x;
                for (kind, bytes, _) in &classes {
                    let width = (p.w * bytes.unwrap_or(0) as f64 / physical.total as f64)
                        .min((p.x + p.w - x).max(0.0));
                    pen.rect(x, by, width, bh, 0.0, memory_class_ink(&pen.inks, *kind));
                    if *kind == MemoryClassKind::Residual {
                        memory_residual_hatch(pen, x, by, width, bh);
                    }
                    x += width;
                }
            } else {
                heading(pen, p, "Where it is", &[]);
                for (i, line) in status_lines.iter().enumerate() {
                    pen.text(
                        p.x,
                        p.y + ROW0_BASE + PITCH * i as f64,
                        line,
                        NOTE_PT,
                        false,
                        pen.inks.words2,
                    );
                }
            }
            for (i, x, row, lines) in &legend {
                let kind = classes[*i].0;
                let x = p.x + x;
                let y = p.y + base + PITCH * *row as f64;
                pen.rect(x, y - 6.5, 6.0, 6.0, 1.0, memory_class_ink(&pen.inks, kind));
                if kind == MemoryClassKind::Residual {
                    memory_residual_hatch(pen, x, y - 6.5, 6.0, 6.0);
                }
                for (j, line) in lines.iter().enumerate() {
                    pen.text(
                        x + 8.0,
                        y + PITCH * j as f64,
                        line,
                        NOTE_PT,
                        false,
                        pen.inks.words2,
                    );
                }
            }
        },
    );
    Section {
        name: "Where it is".into(),
        words,
        right: true,
        parts: Vec::new(),
        // A smaller form must not silently omit physical classes.
        forms: vec![full],
    }
}

/// Existing page-class inks; free memory and the residual also differ by the latter's hatch.
fn memory_class_ink(inks: &VitalsInks, kind: aterm_vitals::model::MemoryClassKind) -> Rgba {
    use aterm_vitals::model::MemoryClassKind;
    match kind {
        MemoryClassKind::App => a(inks.mem[0], 1.0),
        MemoryClassKind::Wired => a(inks.mem[0], 0.7),
        MemoryClassKind::Compressed => a(inks.mem[1], 1.0),
        MemoryClassKind::Cache => a(inks.mem[2], 1.0),
        MemoryClassKind::Free | MemoryClassKind::Residual => rgba(inks.track, 0xFF),
    }
}

/// Keep the hatch's stroke inside the residual, including a very narrow final segment.
fn memory_residual_hatch(pen: &mut Pen<'_>, x: f64, y: f64, w: f64, h: f64) {
    let (x, y, w, h) = (x + 0.5, y + 0.5, w - 1.0, h - 1.0);
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let ink = rgba(pen.inks.words2, 0xFF);
    let mut offset = 0.0;
    while offset < w + h {
        let x0 = (offset - h).max(0.0);
        let x1 = offset.min(w);
        if x1 > x0 {
            pen.line(
                x + x0,
                y + h - (offset - x0),
                x + x1,
                y + h - (offset - x1),
                ink,
            );
        }
        offset += 5.0;
    }
}

/// Biggest now: footprints in the chosen scope. Its heading gives aterm's and this tab's
/// footprints, and after each one larger than the figure of the scope around it
/// ([`words::footprints_over`], `docs/DESIGN-stats-deck-units-2026-11-04.md` §4.6), which the bar
/// cannot show, says so; its one-line form and its words say it too. Those words take no line of
/// their own, so a footprint that crosses its parent between readings changes this section's words
/// and moves no section, form or tab.
fn biggest_section(c: Ctx<'_>) -> Section<'_> {
    let snap = c.snap;
    let total = snap.mem.as_ref().map_or(1, |m| m.sample.total.max(1)) as f64;
    let mut programs: Vec<_> = snap
        .programs
        .iter()
        .filter(|t| c.in_scope(t.scope) && t.footprint.is_some())
        .collect();
    programs.sort_by_key(|t| std::cmp::Reverse(t.footprint));
    let rows: Vec<Row> = programs
        .into_iter()
        .map(|t| Row {
            chip: Some(t.scope),
            name: program_label(&t.name, t.n as usize),
            scope: Some(c.scope_word(t.scope)),
            share: t.footprint.map(|b| b as f64 / total),
            figure: format!(
                "{}{}",
                if t.footprint_unread > 0 { "≥" } else { "" },
                words::mem(t.footprint.unwrap_or(0))
            ),
        })
        .collect();
    let view = c.view(Resource::Memory);
    let over = words::footprints_over(
        Resource::Memory,
        &view,
        (!c.panes.is_empty()).then_some(c.tab_label),
        &c.loc,
    );
    let clause = |tab: bool| {
        over.iter()
            .find(|f| f.tab == tab)
            .map(words::FootprintOver::clause)
    };
    let mut scopes: Vec<(String, Option<String>)> = Vec::new();
    if let Some(b) = view.aterm.now {
        scopes.push((
            format!(
                "aterm {}{}",
                if view.aterm.unread > 0 { "≥" } else { "" },
                words::mem(b as u64)
            ),
            clause(false),
        ));
    }
    if let Some(b) = view.tab.now {
        scopes.push((
            format!(
                "{} {}{}",
                c.tab_label,
                if view.tab.unread > 0 { "≥" } else { "" },
                words::mem(b as u64)
            ),
            clause(true),
        ));
    }
    let figures = if scopes.is_empty() {
        "physical footprint".to_owned()
    } else {
        scopes
            .iter()
            .map(|(f, _)| f.as_str())
            .collect::<Vec<_>>()
            .join(" \u{b7} ")
    };
    // The forms that say a footprint is larger, longest first: each figure with its clause, then
    // both figures under one clause. Each footprint over has its figure among `scopes`.
    let mut larger: Vec<String> = Vec::new();
    if !over.is_empty() {
        larger.push(
            scopes
                .iter()
                .map(|(f, cl)| {
                    cl.as_ref()
                        .map_or_else(|| f.clone(), |cl| format!("{f}, {cl}"))
                })
                .collect::<Vec<_>>()
                .join(" \u{b7} "),
        );
        if over.len() == 2 {
            larger.push(format!("{figures}, each larger than the one around it"));
        }
    }
    let said: Vec<String> = over.iter().map(words::FootprintOver::said).collect();
    let name = format!("Biggest now{}", c.suffix());
    if rows.is_empty() {
        let line = larger.into_iter().chain([figures.clone()]).collect();
        let mut sec = line_section(&name, "Biggest", line, true);
        sec.words = vec![figures];
        sec.words.extend(said);
        return sec;
    }
    let subs: Vec<String> = larger
        .into_iter()
        .chain([figures])
        .chain(scopes.into_iter().next().map(|(f, _)| f))
        .collect();
    // The one-line form names the biggest process; a footprint larger than its parent follows it
    // as speech says it, or both under one clause.
    let first = rows[0].said();
    let mut alone = Vec::new();
    if !said.is_empty() {
        alone.push(said.join(" \u{b7} "));
    }
    if let [a, b] = over.as_slice() {
        alone.push(format!(
            "{}'s and {}'s footprints are each larger than the one around it",
            a.who, b.who
        ));
    }
    let line: Vec<String> = alone
        .iter()
        .map(|w| format!("{first} \u{b7} {w}"))
        .chain([first.clone()])
        .collect();
    let mut sec = list_section(
        c,
        name,
        subs,
        rows,
        None,
        if c.two { 4 } else { 3 },
        "Biggest",
        line,
        true,
    );
    sec.words.extend(said);
    sec
}

/// Swap: used, and its traffic.
fn swap_section(c: Ctx<'_>) -> Section<'_> {
    let snap = c.snap;
    let forms = match snap.mem.as_ref() {
        None => vec!["not read yet".to_owned()],
        Some(m) => {
            let used_w = match m.sample.swap_used {
                None => "usage not read".to_owned(),
                Some(0) => "none used".to_owned(),
                Some(used) => format!("{} used", words::mem(used)),
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
    let (vals, peaks, heat) = hour_columns(&c.view(Resource::Processes), now, 1.0);
    let start = (now.div_euclid(60_000) - 59) * 60_000;
    let (gaps, events) = trace_overlays(snap, start, 60_000, vals.len());
    let trace = Trace {
        vals,
        peaks,
        heat,
        under: HOUR,
        min_span: 5.0,
        mark: None,
        gaps,
        events,
        removals: Vec::new(),
        chips: None,
    };
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
    let room = f.room;
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
            share: share(room.procs, room.procs_max),
            figure: of(room.procs, room.procs_max),
        },
        Row {
            chip: Some(ProcScope::Other),
            name: "yours".into(),
            scope: None,
            share: share(room.user_procs, room.user_procs_max),
            figure: of(room.user_procs, room.user_procs_max),
        },
    ];
    if let Some(n) = view.aterm.now {
        rows.push(Row {
            chip: Some(ProcScope::Aterm),
            name: "aterm".into(),
            scope: None,
            share: share(Some(n as u64), room.procs_max),
            figure: words::grouped(n.round() as u64),
        });
    }
    if let (Some(n), Some(&k)) = (view.tab.now, c.panes.first()) {
        rows.push(Row {
            chip: Some(ProcScope::Tab(k)),
            name: c.tab_label.to_owned(),
            scope: None,
            share: share(Some(n as u64), room.procs_max),
            figure: words::grouped(n.round() as u64),
        });
    }
    let line = vec![
        format!(
            "{} \u{b7} yours {}",
            of(room.procs, room.procs_max),
            of(room.user_procs, room.user_procs_max)
        ),
        of(room.procs, room.procs_max),
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

/// The `sys` query that lists every job the Jobs section lists for `bar`:
/// the Tab lens lists its tab's, as `aterm ctl sys jobs` reads the caller's
/// tab; the Global and aterm lenses list every tab's, `scope=aterm`. Spelt
/// from the grammar's own words, so the overflow line never names a query
/// `sys` refuses.
fn jobs_query(bar: Bar) -> String {
    use aterm_vitals::sys::{ScopeArg, View};
    let jobs = View::Jobs.word();
    match bar {
        Bar::Tab => format!("aterm ctl sys {jobs}"),
        Bar::Global | Bar::Aterm => {
            format!("aterm ctl sys {jobs} scope={}", ScopeArg::Aterm.word())
        }
    }
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
        let short = match (r.working_descendant, r.quiet_ms) {
            (Some(_), Some(q)) if matches!(r.verdict, Verdict::Stuck | Verdict::Quiet) => format!(
                "local {} {}, child activity",
                r.verdict.word(),
                aterm_vitals::jobs::wire::age_words(q)
            ),
            (Some(_), None) if matches!(r.verdict, Verdict::Stuck | Verdict::Quiet) => {
                format!("local {}, child activity", r.verdict.word())
            }
            (_, Some(q)) if matches!(r.verdict, Verdict::Stuck | Verdict::Quiet) => format!(
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
    let scoped = view.rows.iter().filter(|r| tab_ok(r.tab));
    let zombies: u64 = view
        .findings
        .zombies
        .iter()
        .filter(|z| z.tab.is_some_and(tab_ok))
        .map(|z| u64::from(z.count))
        .sum();
    let condition = |v| aterm_vitals::jobs::wire::verdict_summary(scoped.clone(), v);
    let summary = [
        condition(Verdict::Stuck),
        (zombies > 0).then(|| format!("{zombies} zombies")),
        condition(Verdict::Quiet),
        condition(Verdict::WaitingYou),
        condition(Verdict::Parked),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(", ");
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
    let query = jobs_query(c.bar);
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
                let s = format!("+{} more ({query})", lines.len() - shown);
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
                "an outline or gap: not read (sleep, crash or pause); lines mark events".into(),
                "outline or gap: not read".into(),
            ],
        ),
        (
            2,
            [
                "live now at the left, then 1 s and wider cells further back".into(),
                "left cell: live now".into(),
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
                        "multi-scale intervals, live now at the left".into(),
                        "multi-scale from now".into(),
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
                            pen.rect(p.x, base - 7.0, 8.0, 7.0, 1.0, rgba(ink, 0xFF));
                        }
                    }
                    let s = fit_cut(pen.m, forms, p.w - 14.0, BODY_PT, false);
                    pen.text(p.x + 14.0, base, &s, BODY_PT, false, inks.words2);
                }
            }),
            line_form(
                "Cells",
                vec![
                    "multi-scale from live now \u{b7} cap: over max \u{b7} gap or line: sleep, crash"
                        .into(),
                    "multi-scale \u{b7} cap: over max \u{b7} outline: not read".into(),
                ],
            ),
        ],
    });
    let machine = c.loc.platform.machine();
    let mut said = format!(
        "{machine} in grey, aterm in green, this tab in bright green; the table gives each one's \
         amount now in the row's unit."
    );
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
                        "the bar, or the line under each %, nests them; up and down choose a \
                         lens's scope"
                            .into(),
                        "the bar, or the line under each %, nests them".into(),
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
        explainer: "Each row is one resource, read from the live now cell at the left \
                    end; the scale over the strips says how far back they reach (up to 6 h \
                    where a strip has the room), in blocks of 1 s, 10 s, 1 min, 10 min and 1 h."
            .into(),
        answer: None,
        sections,
        two_order: Vec::new(),
        keeps_first: false,
    }
}

fn link_kind_word(kind: LinkKind) -> Option<&'static str> {
    match kind {
        LinkKind::Wifi => Some("Wi-Fi"),
        LinkKind::Ethernet => Some("Ethernet"),
        LinkKind::Cellular => Some("Cellular"),
        LinkKind::Vpn => Some("VPN"),
        LinkKind::Bridge => Some("bridge"),
        LinkKind::Other => None,
    }
}

fn net_rate_word(bytes_per_s: f64) -> String {
    let bps = bytes_per_s.max(0.0);
    if bps < 100_000.0 {
        format!("{} KB/s", words::grouped((bps / 1e3).round() as u64))
    } else {
        words::rate_mb_lone(bps)
    }
}

fn link_speed_word(bits_per_s: u64) -> String {
    let bytes_per_s = (bits_per_s as f64 / 8.0).max(0.0);
    if bytes_per_s < 100_000.0 {
        format!(
            "{} KB/s",
            words::grouped((bytes_per_s / 1e3).round() as u64)
        )
    } else {
        words::rate_mb(bytes_per_s)
    }
}

/// The Network lens: live download/upload split, 1-hour throughput trace, scoped open sockets,
/// connectivity/latency monitoring, and TCP state summary.
fn network_doc(c: Ctx<'_>) -> LensDoc<'_> {
    let snap = c.snap;
    let at = c.at();
    let mut sources = Vec::new();
    if let Some(link) = snap.facts.link.as_ref() {
        let kind = link_kind_word(link.kind);
        let speed = link.rate_bps.map(link_speed_word);
        if let (Some(k), Some(sp)) = (kind, speed.as_deref()) {
            sources.push(format!("{} ({k}, link {sp}) \u{b7} as of {at}", link.name));
        }
        if let Some(k) = kind {
            sources.push(format!("{} ({k}) \u{b7} as of {at}", link.name));
        }
        sources.push(format!("{} \u{b7} as of {at}", link.name));
    }
    sources.push(format!("as of {at}"));
    let explainer = "Heat is the busier direction's rate as a share of its 7-day peak, never \
                     above the link's rate."
        .to_owned();
    let mut sections = vec![
        traffic_section(c),
        net_trace_section(c),
        sockets_section(c),
        connectivity_section(c),
    ];
    if let Some(tcp_sec) = tcp_states_section(c) {
        sections.push(tcp_sec);
    }
    LensDoc {
        name: "Network",
        sources,
        explainer,
        answer: c.now_answer(Resource::Network, "peak"),
        sections,
        two_order: Vec::new(),
        keeps_first: false,
    }
}

fn traffic_section(c: Ctx<'_>) -> Section<'_> {
    let snap = c.snap;
    let view = c.view(Resource::Network);
    if let Some(r) = snap.net.as_ref().and_then(|n| n.rates) {
        let cap = view
            .capacity
            .unwrap_or(1.0)
            .max(r.rx_bps.max(r.tx_bps))
            .max(1.0);
        let rows = vec![
            Row {
                chip: Some(ProcScope::Other),
                name: "download".into(),
                scope: Some(format!("{}/s", words::tokens_k(r.rx_pps))),
                share: Some((r.rx_bps / cap).clamp(0.0, 1.0)),
                figure: format!("\u{2193} {}", net_rate_word(r.rx_bps)),
            },
            Row {
                chip: Some(ProcScope::Other),
                name: "upload".into(),
                scope: Some(format!("{}/s", words::tokens_k(r.tx_pps))),
                share: Some((r.tx_bps / cap).clamp(0.0, 1.0)),
                figure: format!("\u{2191} {}", net_rate_word(r.tx_bps)),
            },
        ];
        let mut subs = Vec::new();
        if let Some(link) = snap.facts.link.as_ref() {
            if let Some(sp) = link.rate_bps.map(link_speed_word) {
                subs.push(format!("{} \u{b7} link {sp}", link.name));
            }
            subs.push(link.name.clone());
        }
        subs.push("download and upload".into());
        let rx_w = net_rate_word(r.rx_bps);
        let tx_w = net_rate_word(r.tx_bps);
        // The short form names the busier direction, the series that binds (the larger share of
        // its own capacity, as the deck row's arrow and the trace's columns take it), with its
        // arrow and its live rate, not the larger raw rate (the second review, 2026-11-06).
        let busier = if view.second.as_ref().is_some_and(|s| s.binds) {
            format!("\u{2191} {tx_w}")
        } else {
            format!("\u{2193} {rx_w}")
        };
        let line = vec![format!("\u{2193} {rx_w} \u{b7} \u{2191} {tx_w}"), busier];
        return list_section(
            c,
            "Traffic now".into(),
            subs,
            rows,
            None,
            2,
            "Traffic",
            line,
            false,
        );
    }
    // The busier direction is the series that binds: the larger share of its own capacity, or
    // with no share known the larger rate, the rule the deck's row takes on its 10 s "now" and
    // the trace takes on each minute's mean; `view.now` is receiving's alone.
    let forms = match view.binding().now {
        Some(bps) => {
            let w = net_rate_word(bps);
            vec![format!("{w} busier direction"), w]
        }
        None => vec!["not read yet".into()],
    };
    line_section("Traffic now", "Traffic", forms, false)
}

/// Throughput's last hour as its trace, its unit and its first minute (Unix ms): column by
/// column, the busier direction's minute, as its heat, the deck's Network row and the words say
/// ([`ResourceView::busier_columns`]).
fn net_trace(c: Ctx<'_>) -> (Trace, &'static str, i64) {
    let snap = c.snap;
    let now = snap.at_unix_ms;
    let (cells, heat) = c.view(Resource::Network).busier_columns(60, 60, now);
    let peak = cells
        .iter()
        .filter_map(|cell| cell.mean)
        .fold(0.0_f64, f64::max);
    let (scale, unit, min_span) = if peak >= 100_000.0 {
        (1_000_000.0, "MB/s", 0.5)
    } else {
        (1_000.0, "KB/s", 5.0)
    };
    let (vals, peaks) = trace_figures(&cells, scale);
    let start = (now.div_euclid(60_000) - 59) * 60_000;
    let (gaps, events) = trace_overlays(snap, start, 60_000, vals.len());
    let trace = Trace {
        vals,
        peaks,
        heat,
        under: HOUR,
        min_span,
        mark: None,
        gaps,
        events,
        removals: Vec::new(),
        chips: None,
    };
    (trace, unit, start)
}

/// Throughput, last hour ([`net_trace`]).
fn net_trace_section(c: Ctx<'_>) -> Section<'_> {
    let (trace, unit, start) = net_trace(c);
    let said = trace_said(
        "Throughput over the last hour",
        unit,
        &trace,
        (start, 60_000),
        &c.loc,
    );
    let first = trace.vals.iter().flatten().next().copied();
    let last = trace.vals.iter().rev().flatten().next().copied();
    let line = match (first, last) {
        (Some(a), Some(b)) => vec![format!("{a:.1} to {b:.1} {unit}")],
        _ => vec!["not read yet".to_owned()],
    };
    trace_section(
        c,
        "Throughput, last hour",
        vec![format!("{unit}, busier direction"), unit.into()],
        trace,
        said,
        "Last hour",
        line,
        false,
    )
}

fn sockets_section(c: Ctx<'_>) -> Section<'_> {
    let snap = c.snap;
    let name = format!("Open sockets{}", c.suffix());
    let net = snap.net.as_ref();
    let tcp_total = net.and_then(|n| n.tcp).and_then(|t| t.known_total());
    let aterm_s = net.and_then(|n| n.aterm_sockets);
    let denom = tcp_total.unwrap_or(0).max(aterm_s.unwrap_or(0)).max(1) as f64;
    let mut rows: Vec<Row> = Vec::new();
    if c.bar == Bar::Global
        && let Some(total) = tcp_total
    {
        rows.push(Row {
            chip: Some(ProcScope::Other),
            name: c.loc.platform.machine().into(),
            scope: Some("TCP".into()),
            share: Some((f64::from(total) / denom).clamp(0.0, 1.0)),
            figure: format!("{} open", words::grouped(u64::from(total))),
        });
    }
    if matches!(c.bar, Bar::Global | Bar::Aterm)
        && let Some(at_s) = aterm_s
    {
        rows.push(Row {
            chip: Some(ProcScope::Aterm),
            name: "aterm".into(),
            scope: None,
            share: Some((f64::from(at_s) / denom).clamp(0.0, 1.0)),
            figure: format!("{} sockets", words::grouped(u64::from(at_s))),
        });
    }
    if let Some(net) = net {
        let mut tabs: Vec<(u64, u32)> = net
            .tab_sockets
            .iter()
            .copied()
            .filter(|&(k, count)| {
                c.in_scope(ProcScope::Tab(k)) && (count > 0 || c.panes.contains(&k))
            })
            .collect();
        tabs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for (k, count) in tabs {
            rows.push(Row {
                chip: Some(ProcScope::Tab(k)),
                name: c.scope_word(ProcScope::Tab(k)),
                scope: None,
                share: Some((f64::from(count) / denom).clamp(0.0, 1.0)),
                figure: format!("{} sockets", words::grouped(u64::from(count))),
            });
        }
    }
    if rows.is_empty() {
        let missing = if snap.resources[Resource::Network.index()].aterm.missing
            == Some(aterm_vitals::NoFigure::NotPerApp)
        {
            "no per-app network figure"
        } else if aterm_s.is_none() {
            // No process's sockets were read (on Linux, before the first TCP dump or once it
            // retired): unread, not none.
            "open sockets not read"
        } else {
            "none open in this scope"
        };
        return line_section(&name, "Sockets", vec![missing.into()], true);
    }
    let line: Vec<String> = rows.iter().take(2).map(Row::said).collect();
    let note = aterm_s.map(|_| "throughput is machine-wide; open sockets counted per scope".into());
    list_section(
        c,
        name,
        vec!["live sockets by scope".into(), "by scope".into()],
        rows,
        note,
        if c.two { 4 } else { 3 },
        "Sockets",
        line,
        true,
    )
}

fn connectivity_section(c: Ctx<'_>) -> Section<'_> {
    let snap = c.snap;
    let Some(net) = snap.net.as_ref() else {
        return line_section(
            "Connectivity",
            "State",
            vec!["link state not read yet".into(), "not read".into()],
            true,
        );
    };
    let state = net.state.label();
    let rtt_long = net.srtt_ms.map(|rtt| format!("median TCP RTT {rtt:.1} ms"));
    let rtt_short = net.srtt_ms.map(words::rtt_word);
    let err_drop = net.rates.and_then(|r| {
        let errs = r.rx_err_ps + r.tx_err_ps;
        let drops = r.rx_drop_ps + r.tx_drop_ps.unwrap_or(0.0);
        (errs > 0.0 || drops > 0.0).then(|| format!("errors {errs:.1}/s, drops {drops:.1}/s"))
    });
    let counted = snap.facts.link.as_ref().and_then(|link| match link.kind {
        LinkKind::Vpn => Some(format!("{} VPN (counted on carrier)", link.name)),
        LinkKind::Bridge => Some(format!("{} bridge", link.name)),
        LinkKind::Wifi | LinkKind::Ethernet | LinkKind::Cellular | LinkKind::Other => None,
    });
    let mut forms = Vec::new();
    let mut full = state.to_owned();
    if let Some(rtt) = &rtt_long {
        full.push_str(&format!(" \u{b7} {rtt}"));
    }
    if let Some(ed) = &err_drop {
        full.push_str(&format!(" \u{b7} {ed}"));
    }
    if let Some(cnt) = &counted {
        full.push_str(&format!(" \u{b7} {cnt}"));
    }
    forms.push(full);
    if let Some(rtt) = &rtt_long {
        forms.push(format!("{state} \u{b7} {rtt}"));
    }
    if let Some(rtt) = &rtt_short {
        forms.push(format!("{state} \u{b7} {rtt}"));
    }
    forms.push(state.to_owned());
    forms.dedup();
    line_section("Connectivity", "State", forms, true)
}

fn tcp_states_section(c: Ctx<'_>) -> Option<Section<'_>> {
    let tcp = c.snap.net.as_ref()?.tcp?;
    if tcp.counters_only {
        let est = words::grouped(u64::from(tcp.established));
        let mut full = vec![format!("{est} established or close-wait")];
        if tcp.time_wait > 0 {
            full.push(format!(
                "{} time-wait or fin-wait-2",
                words::grouped(u64::from(tcp.time_wait))
            ));
        }
        let forms = dedup(vec![full.join(" \u{b7} "), format!("{est} est")]);
        return Some(line_section("TCP states", "TCP", forms, true));
    }
    let mut full = vec![
        format!("{} established", words::grouped(u64::from(tcp.established))),
        format!("{} listen", words::grouped(u64::from(tcp.listen))),
    ];
    if tcp.time_wait > 0 {
        full.push(format!(
            "{} time-wait",
            words::grouped(u64::from(tcp.time_wait))
        ));
    }
    if tcp.close_wait > 0 {
        full.push(format!(
            "{} close-wait",
            words::grouped(u64::from(tcp.close_wait))
        ));
    }
    if tcp.connecting() > 0 {
        full.push(format!(
            "{} connecting",
            words::grouped(u64::from(tcp.connecting()))
        ));
    }
    let short = format!(
        "{} est \u{b7} {} listen",
        words::grouped(u64::from(tcp.established)),
        words::grouped(u64::from(tcp.listen))
    );
    let forms = dedup(vec![full.join(" \u{b7} "), short]);
    Some(line_section("TCP states", "TCP", forms, true))
}

/// A text-only well (§11.4).
fn text_well(c: Ctx<'_>, focus: Instrument) -> LensDoc<'_> {
    let snap = c.snap;
    let res = focus.resource().unwrap_or(Resource::Tokens);
    let at = c.at();
    let (name, explainer) = match res {
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
        Resource::Tokens => {
            match f.plan.as_ref() {
                Some(p) => {
                    let mut w = p.window_word.clone();
                    if let Some(first) = w.get(..1) {
                        w = first.to_uppercase() + w.get(1..).unwrap_or_default();
                    }
                    // What is left, as every account figure reads (harness::left).
                    lines.push(format!(
                        "{w} limit {}% {}, read at {}",
                        aterm_agent::harness::left::exact(u32::from(p.used_x10)),
                        aterm_agent::harness::left::WORD,
                        c.loc.clock(p.read_at, snap.at_unix_ms)
                    ));
                }
                None => {
                    lines.retain(|l| !l.starts_with("plan limit"));
                    lines.push("No current plan reading on this machine".into());
                }
            }
            lines.push("Codex and agy tabs are not counted".into());
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
                lines.push(if g.unified {
                    "GPU memory total not reported; available not reported; uses unified system memory".into()
                } else {
                    "GPU memory total not reported; available not reported".into()
                });
            }
            match (f.gpu_resets, f.gpu.as_ref().and_then(|g| g.recoveries)) {
                (Some((n, since)), _) => lines.push(format!(
                    "reset {n}\u{d7} since {}",
                    c.loc.clock(since, snap.at_unix_ms)
                )),
                (None, Some(count)) => {
                    lines.push(format!("GPU recovery counter observed: {count}"))
                }
                // Only macOS has a reader of the counter; elsewhere its line is absent.
                (None, None) if c.loc.platform == Platform::MacOs => {
                    lines.push("GPU recovery counter not read".to_owned());
                }
                (None, None) => {}
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
    // Line 1 names the GPU with its cores and the memory it holds in use
    // ([`words::gpu_source`]), as speech and ⌘C do; shorter forms drop the memory, then the cores.
    let mut sources = Vec::new();
    if res == Resource::Gpu
        && let Some(g) = snap.gpu_sample()
    {
        let model = g
            .name
            .as_deref()
            .filter(|name| !name.is_empty())
            .unwrap_or("GPU");
        if let Some(source) = words::gpu_source(snap) {
            sources.push(format!("{source} · as of {at}"));
        }
        if let Some(cores) = g.cores {
            sources.push(format!("{model} · {cores} cores · as of {at}"));
        }
        sources.push(format!("{model} · as of {at}"));
        sources.dedup();
    }
    sources.push(format!("as of {at}"));
    let sections = vec![
        lines_section(
            if c.fresh { "Now" } else { "Last" },
            now_lines,
            vec![sentence.clone()],
            false,
        ),
        lines_section("Details", lines.clone(), lines, true),
    ];
    LensDoc {
        name,
        sources,
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
    let line: Vec<String> = lines.first().cloned().into_iter().collect();
    let items = lines.into_iter().map(|l| vec![l]).collect();
    rows_section(name, items, line, words, right)
}

/// A heading over `items`, each one or more rows: its forms show the first k items whole, from
/// all of them down to one (a row wider than the column cut), then `line` on one line.
fn rows_section<'d>(
    name: &'static str,
    items: Vec<Vec<String>>,
    line: Vec<String>,
    words: Vec<String>,
    right: bool,
) -> Section<'d> {
    let mut forms = Vec::new();
    for k in (1..=items.len()).rev() {
        let shown = items[..k].concat();
        forms.push(form(rows_h(shown.len(), false), move |pen, p| {
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
    /// The Tab key, ⇥.
    TabKey,
}

/// The key hint's forms for a lens opened `how`, longest first (§11.3); with `spans` (a Disk lens
/// whose chart offers its two spans) a lens that takes keys names Tab, which switches them.
const fn hints(how: Opened, spans: bool) -> &'static [&'static [Seg]] {
    use Seg::{Across, T, TabKey, UpDown};
    match (how, spans) {
        (Opened::Pinned, true) => &[
            &[
                T("pinned \u{b7} "),
                Across,
                T("resources \u{b7} "),
                UpDown,
                T("scope \u{b7} "),
                TabKey,
                T("span \u{b7} esc closes"),
            ],
            &[T("pinned \u{b7} "), TabKey, T("span \u{b7} esc closes")],
            &[TabKey, T("span \u{b7} esc closes")],
            &[T("esc closes")],
        ],
        (Opened::Keyboard, true) => &[
            &[
                Across,
                T("resources \u{b7} "),
                UpDown,
                T("scope \u{b7} "),
                TabKey,
                T("span \u{b7} esc closes"),
            ],
            &[
                Across,
                T("resources \u{b7} "),
                TabKey,
                T("span \u{b7} esc closes"),
            ],
            &[TabKey, T("span \u{b7} esc closes")],
            &[T("esc closes")],
        ],
        (Opened::Pinned, false) => &[
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
        (Opened::Keyboard, false) => &[
            &[
                Across,
                T("resources \u{b7} "),
                UpDown,
                T("scope \u{b7} esc closes"),
            ],
            &[Across, T("resources \u{b7} esc closes")],
            &[T("esc closes")],
        ],
        (Opened::Hover, _) => &[&[T("click pins it")], &[T("click pins")]],
    }
}

fn hint_w(m: &FaceMetrics, hint: &[Seg]) -> f64 {
    hint.iter()
        .map(|s| match s {
            Seg::T(t) => m.width(t, NOTE_PT, false),
            Seg::Across => 20.0,
            Seg::UpDown => 16.0,
            Seg::TabKey => 12.0,
        })
        .sum()
}

/// Draw the header while it clears the foot and leaves `reserve` pt of body; `spans`: the lens is
/// a Disk lens whose chart offers its two spans ([`hints`]).
fn head(
    pen: &mut Pen<'_>,
    doc: &LensDoc<'_>,
    how: Opened,
    spans: bool,
    lw: f64,
    lh: f64,
    reserve: f64,
) -> f64 {
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
    let hints = hints(how, spans);
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
                Seg::TabKey => pen.tab_key(x, LINE1_BASE),
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
    let (two, cw) = if lens.focus == Instrument::NetworkState {
        (false, lw - 2.0 * SIDE)
    } else {
        columns(lw)
    };
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
    let empty = Snapshot::default();
    let snap = if lens.focus == Instrument::NetworkState {
        Some(input.snap.unwrap_or(&empty))
    } else {
        input.snap
    };
    match snap {
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
            let _ = head(&mut pen, &doc, lens.how, false, lw, lh, 0.0);
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
                fresh: input.live.fresh,
                disk_span: input.disk_span,
            };
            let mut doc = if lens.focus == Instrument::NetworkState {
                lab_network_doc(
                    input.network,
                    input.lab,
                    input.fleet,
                    input.network_target,
                    input.network_place,
                    metrics,
                    cw,
                )
            } else {
                doc_for(c, lens.focus)
            };
            if !matches!(lens.focus, Instrument::NetworkState | Instrument::HowToRead)
                && !input.live.fresh
            {
                doc.explainer = format!(
                    "Updates delayed; last reading {}. {}",
                    c.at(),
                    doc.explainer
                );
                if let Some((label, _, _)) = &mut doc.answer
                    && *label == "Now"
                {
                    *label = "Last";
                }
            }
            let reserve = doc
                .sections
                .first()
                .filter(|_| doc.keeps_first)
                .and_then(|s| s.forms.last())
                .map_or(0.0, |f| f.h);
            let spans = lens.focus == Instrument::Disk && spans_offered(snap, input.disk_span);
            let top = head(&mut pen, &doc, lens.how, spans, lw, lh, reserve);
            let placed = lens_body(
                (two, cw),
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
        let f = self.deck_frame(wid).or_else(|| {
            (lens.focus == Instrument::NetworkState)
                .then(|| self.network_lens_frame(wid))
                .flatten()
        })?;
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
    fn lens_input<'a>(
        &'a self,
        f: &'a crate::stats_deck::DeckFrame,
        wid: crate::WindowId,
    ) -> DeckInput<'a> {
        DeckInput {
            network: Some(self.network_snapshot.as_ref()),
            network_place: f.network_place.as_ref(),
            lab: Some(self.lab_deck.as_ref()),
            fleet: self.fleet_field.as_deref(),
            network_target: self
                .windows
                .get(&wid)
                .map_or(crate::network_deck::Target::Overview, |ws| {
                    ws.stats_lens.network_target
                }),
            disk_span: self
                .windows
                .get(&wid)
                .map_or(DiskSpan::Day, |ws| ws.stats_lens.disk_span),
            snap: self.vitals.snapshot.as_deref(),
            prev_snap: None,
            tab_panes: f.tab_panes.clone(),
            tab_label: f.tab_label.clone(),
            tab_count: f.tab_count,
            inks: f.lens_inks,
            dwc: f.dwc,
            marks: self.stats_marks(),
            plate: None,
            form: f.deck,
            contrast: f.contrast,
            locale: crate::stats_deck::host_locale(self.vitals.snapshot.as_deref()),
            live: if f.live.fresh {
                crate::stats_live::DeckLive::STILL
            } else {
                crate::stats_live::DeckLive::STALE
            },
        }
    }

    /// The open lens's fingerprint for `wid` (`0` when none).
    pub(crate) fn stats_lens_fp(&self, wid: crate::WindowId) -> u64 {
        let Some((lg, lens, f)) = self.lens_frame(wid) else {
            return 0;
        };
        self.stats_lens_fp_for(wid, &lg, &lens, &f)
    }

    /// Key the same captured frame that the lens paints.
    fn stats_lens_fp_for(
        &self,
        wid: crate::WindowId,
        lg: &LensGeom,
        lens: &Lens,
        f: &crate::stats_deck::DeckFrame,
    ) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.vitals.snapshot.as_ref().map(|s| s.seq).hash(&mut h);
        self.network_snapshot.revision.hash(&mut h);
        std::mem::discriminant(&self.network_snapshot.network.visibility).hash(&mut h);
        self.network_snapshot.network.detail.hash(&mut h);
        self.network_snapshot.network.observed_at_ms.hash(&mut h);
        self.network_snapshot.network.via_local_process.hash(&mut h);
        self.network_snapshot.network.retention_limited.hash(&mut h);
        self.network_snapshot.network.accounts_exchange.hash(&mut h);
        crate::network_deck::hash_peer_reachability(&self.network_snapshot, &mut h);
        self.network_snapshot.services.hash(&mut h);
        self.lab_deck_feed.revision().hash(&mut h);
        if lens.focus == Instrument::NetworkState {
            f.network_place.hash(&mut h);
            self.fleet_field
                .as_ref()
                .map(|field| field.key)
                .hash(&mut h);
        }
        self.windows
            .get(&wid)
            .map(|ws| (ws.stats_lens.network_target, ws.stats_lens.disk_span))
            .hash(&mut h);
        lens.hash(&mut h);
        if lens.focus != Instrument::NetworkState {
            f.live.fresh.hash(&mut h);
        }
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
            if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
                let ws = &mut **route_guard_ws;
                let had_rect = ws.stats_lens.rect.take().is_some();
                ws.stats_lens.retire_input();
                ws.lens_card = None;
                ws.stats_lens.open = None;
                ws.stats_lens.leave_due = None;
                ws.stats_lens.shell = None;
                if had_rect {
                    ws.native_rehover = true;
                }
            }
            return;
        };
        let fp = self.stats_lens_fp_for(wid, &lg, &lens, &f);
        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.lens_card.as_ref())
            .is_some_and(|c| c.fp == fp)
        {
            return;
        }
        let input = self.lens_input(&f, wid);
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
        let Some(mut route_guard_ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let ws = &mut **route_guard_ws;
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
            serial: 0,
            delta: None,
        });
        drop(route_guard_ws);
        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.claude_lights.glass_hover_px())
            .is_some_and(|(x, y)| self.stats_lens_at(wid, x, y))
        {
            self.note_band_covered(wid);
            self.evict_stale_glass_hover(wid);
        }
        self.evict_native_hover_under_stats_lens(wid);
    }

    /// The open lens laid out, not drawn, for the accessibility tree.
    #[cfg(any(a11y_tree, test))]
    pub(crate) fn stats_lens_words(
        &self,
        wid: crate::WindowId,
    ) -> Option<(LensGeom, Lens, LensLaid)> {
        let (lg, lens, f) = self.lens_frame(wid)?;
        let input = self.lens_input(&f, wid);
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
        if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
            let ws = &mut **route_guard_ws;
            ws.stats_lens.retire_input();
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
        if focus == Instrument::NetworkState
            && let Some(crate::stats_paint::Hit {
                kind: crate::stats_paint::HitKind::Network(target),
                ..
            }) = self.stats_hit_under_pointer(wid)
            && let Some(mut ws) = self.windows.get_mut(&wid)
        {
            ws.stats_lens.network_target = target;
        }
        self.set_stats_lens(wid, Lens { focus, bar, how });
        self.vitals.next_due = Some(Instant::now());
    }

    /// The explicit Network State command also works with the resource deck hidden.
    pub(crate) fn open_network_lens(&mut self) -> bool {
        let Some(wid) = self
            .frontmost_window
            .or_else(|| self.windows.keys().next().copied())
        else {
            return false;
        };
        self.open_network_lens_in(wid)
    }

    /// Open the Network State lens on `wid`, even with the resource deck hidden.
    pub(crate) fn open_network_lens_in(&mut self, wid: crate::WindowId) -> bool {
        let Some(frame) = self
            .deck_frame(wid)
            .or_else(|| self.network_lens_frame(wid))
        else {
            return false;
        };
        if self.lens_geom_for(wid, &frame).is_none() {
            return false;
        }
        self.set_stats_lens(
            wid,
            Lens {
                focus: Instrument::NetworkState,
                bar: Bar::Global,
                how: Opened::Keyboard,
            },
        );
        true
    }

    /// Retire pointer timers on blur, keeping an explicitly opened lens and its selection.
    pub(crate) fn stats_lens_focus_lost(&mut self, wid: crate::WindowId) {
        let hover = {
            let Some(mut route_guard_ws) = self.windows.get_mut(&wid) else {
                return;
            };
            let ws = &mut **route_guard_ws;
            ws.stats_lens.retire_input();
            ws.stats_lens.rest = None;
            ws.stats_lens.leave_due = None;
            ws.stats_lens
                .open
                .is_some_and(|lens| lens.how == Opened::Hover)
        };
        if hover {
            self.close_stats_lens(wid);
        }
    }

    /// Close `wid`'s lens (no spring: the next frame is the first without it).
    pub(crate) fn close_stats_lens(&mut self, wid: crate::WindowId) -> bool {
        let Some(mut route_guard_ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        let ws = &mut **route_guard_ws;
        ws.stats_lens.retire_input();
        let was = ws.stats_lens.open.take().is_some();
        ws.stats_lens.leave_due = None;
        ws.stats_lens.rest = None;
        if was {
            ws.native_rehover = true;
            drop(route_guard_ws);
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

    /// The rows whose lens a window holds open, each once ([`crate::vitals_host::Request::lenses`]).
    pub(crate) fn open_lens_rows(&self) -> Vec<Resource> {
        let mut rows = Vec::new();
        for row in self
            .windows
            .values()
            .filter_map(|ws| ws.stats_lens.open?.focus.resource())
        {
            if !rows.contains(&row) {
                rows.push(row);
            }
        }
        rows
    }

    /// Whether window pixel `(x, y)` lies inside `wid`'s open stats lens card.
    pub(crate) fn stats_lens_at(&self, wid: crate::WindowId, x: f64, y: f64) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        let (Some((dx, dy, w, h)), Some(_)) = (ws.stats_lens.rect, ws.stats_lens.open) else {
            return false;
        };
        let (ox, oy) = self.frame_origin(wid);
        let (fx, fy) = (x - ox as f64 - f64::from(dx), y - oy as f64 - f64::from(dy));
        (0.0..f64::from(w)).contains(&fx) && (0.0..f64::from(h)).contains(&fy)
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
        if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
            let ws = &mut **route_guard_ws;
            ws.stats_lens.retire_input();
        }
        if self.windows.get(&wid).is_none_or(|ws| ws.overlay_open()) {
            return false;
        }
        if self.lens_card_point(wid).is_some() {
            if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
                let ws = &mut **route_guard_ws;
                ws.stats_lens.leave_due = None;
                ws.stats_lens.rest = None;
            }
            return true;
        }
        let form = self.window_bars(wid);
        let hit = self.stats_hit_under_pointer(wid);
        let over_card = self.stats_card_point(wid).is_some();
        let owns = match form {
            StatsBars::Deck => over_card,
            StatsBars::Sill => hit.is_some(),
            StatsBars::Off => false,
        };
        let Some(mut route_guard_ws) = self.windows.get_mut(&wid) else {
            return owns;
        };
        let ws = &mut **route_guard_ws;
        let open = ws.stats_lens.open;
        match hit {
            Some(hit) => {
                ws.stats_lens.leave_due = None;
                ws.stats_lens.rest = match open {
                    Some(l) if l.how != Opened::Hover => None,
                    Some(l)
                        if l.focus == hit.instrument()
                            && l.bar == hit.bar()
                            && match hit.kind {
                                crate::stats_paint::HitKind::Network(target) => {
                                    ws.stats_lens.network_target == target
                                }
                                _ => true,
                            } =>
                    {
                        None
                    }
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
        if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
            let ws = &mut **route_guard_ws;
            ws.stats_lens.retire_input();
            ws.stats_lens.rest = None;
            if ws.stats_lens.open.is_some_and(|l| l.how == Opened::Hover) {
                ws.stats_lens.leave_due = Some(now + LEAVE);
            }
        }
    }

    /// A press: on the lens it pins a hover lens.
    pub(crate) fn stats_press(&mut self, wid: crate::WindowId) -> bool {
        if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
            let ws = &mut **route_guard_ws;
            ws.stats_lens.retire_input();
        }
        if self.windows.get(&wid).is_none_or(|ws| ws.overlay_open()) {
            return false;
        }
        if self.lens_card_point(wid).is_some() {
            if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
                let ws = &mut **route_guard_ws;
                if let Some(open) = ws.stats_lens.open.as_mut()
                    && open.how == Opened::Hover
                {
                    open.how = Opened::Pinned;
                    if let Some(w) = ws.os_window.as_ref() {
                        w.request_redraw();
                    }
                }
            }
            return true;
        }
        let form = self.window_bars(wid);
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
        match self.window_bars(wid) {
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
        if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
            let ws = &mut **route_guard_ws;
            ws.stats_lens.sync_tab_scope(ws.tab_set.active_id());
        }
        let arrow = matches!(
            key,
            Key::Named(
                NamedKey::ArrowUp
                    | NamedKey::ArrowDown
                    | NamedKey::ArrowLeft
                    | NamedKey::ArrowRight
            )
        );
        let before = self
            .windows
            .get(&wid)
            .filter(|ws| arrow && !inert && !ws.occluded && ws.stats_lens.open.is_some())
            .map(|ws| {
                (
                    ws.stats_lens.selection(ws.tab_set.active_id()),
                    crate::metrics::native_input_arrival(),
                )
            });
        let taken = self.stats_lens_key_untimed(wid, mods, key, copy_chord, inert);
        if taken
            && let Some((before, arrival)) = before
            && let Some(mut ws) = self.windows.get_mut(&wid)
            && let Some(after) = ws.stats_lens.selection(ws.tab_set.active_id())
            && before != Some(after)
        {
            // A superseded selection that never reached glass has no measured answer.
            ws.stats_lens.retire_input();
            let _ = ws.stats_lens.input.arm(arrival);
            ws.stats_lens.input_selection = Some(after);
        }
        taken
    }

    /// The pending arrows whose selection this composition actually includes. A higher card
    /// can cover a cached lens, so presence in `lens_card` alone is no paint acknowledgment.
    pub(crate) fn stats_lens_input_receipt(
        &mut self,
        wid: crate::WindowId,
    ) -> Option<LensPaintReceipt> {
        let fp = self.stats_lens_fp(wid);
        let within_surface = self.stats_lens_within_surface(wid);
        let obstructed = self
            .paste_banner
            .as_ref()
            .is_some_and(|banner| banner.wid == wid)
            || self.close_band(wid).is_some()
            || self
                .autopilot_menu
                .as_ref()
                .is_some_and(|menu| menu.wid == wid)
            || self
                .autopilot_menu_spent
                .as_ref()
                .is_some_and(|menu| menu.wid == wid);
        let mut route_guard_ws = self.windows.get_mut(&wid)?;
        let ws = &mut **route_guard_ws;
        ws.stats_lens.sync_tab_scope(ws.tab_set.active_id());
        if !obstructed
            && !ws.overlay_open()
            && ws.tab_menu.is_none()
            && ws.route_card.is_none()
            && ws.settings_card.is_none()
            && ws.badge_card.is_none()
            && match ws.stats_lens.selection(ws.tab_set.active_id()) {
                // The lens reaches the glass in the CPU fold, or as a layer of the tray stack
                // the renderer folds itself (`App::stack_or_fold_stats_deck`), which keeps no
                // fold.
                Some(_) => {
                    within_surface
                        && (ws.deck_fold.is_some() || ws.tray_stacked)
                        && ws.lens_card.as_ref().is_some_and(|card| card.fp == fp)
                }
                None => ws.lens_card.is_none(),
            }
        {
            let selection = ws.stats_lens.selection(ws.tab_set.active_id());
            if ws.stats_lens.presented_selection.is_none()
                || selection != ws.stats_lens.input_selection
                || ws.stats_lens.presented_selection == Some(selection)
            {
                ws.stats_lens.retire_input();
            }
            Some(LensPaintReceipt {
                selection,
                input: ws.stats_lens.input.token(),
            })
        } else {
            ws.stats_lens.retire_input();
            None
        }
    }

    pub(crate) fn stats_lens_within_surface(&self, wid: crate::WindowId) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        let (Some(surface), Some(card)) = (ws.win_px, ws.lens_card.as_ref()) else {
            return false;
        };
        let (cw, _) = self.win_cell_size(wid);
        let Some(frame_w) = usize::from(ws.cols).checked_mul(cw).and_then(|w| {
            self.win_pad(wid)
                .checked_mul(2)
                .and_then(|pad| w.checked_add(pad))
        }) else {
            return false;
        };
        let placement = aterm_render::frame_placement(
            surface.width as usize,
            surface.height as usize,
            frame_w,
            self.stats_frame_h(wid),
            aterm_render::SideInsetPx(self.win_side_inset_px(wid)),
        );
        let within = |start: u32, len: u32, axis: aterm_render::AxisPlacement| {
            let start = start as usize;
            len > 0
                && start >= axis.crop_before
                && start
                    .checked_add(len as usize)
                    .zip(axis.crop_before.checked_add(axis.visible))
                    .is_some_and(|(end, visible_end)| end <= visible_end)
        };
        within(card.dx, card.pw, placement.x) && within(card.dy, card.ph, placement.y)
    }

    /// A successful own frame acknowledges only the selection its composition carried.
    pub(crate) fn stats_lens_presented(
        &mut self,
        wid: crate::WindowId,
        receipt: Option<LensPaintReceipt>,
        visible: bool,
        input: crate::metrics::PresentInput,
    ) {
        let Some(mut route_guard_ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let ws = &mut **route_guard_ws;
        if !visible || input.capture_forced() {
            ws.stats_lens.retire_input();
            return;
        }
        ws.stats_lens.presented_selection = receipt.map(|paint| paint.selection);
        if let Some(token) = receipt.and_then(|paint| paint.input) {
            ws.stats_lens.input.record(token, input);
        } else if receipt.is_none() {
            ws.stats_lens.retire_input();
        }
    }

    /// Debug readback of desired and own-presented selections, without forcing a capture.
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn stats_lens_chrome_lines(&self) -> Vec<String> {
        self.windows
            .iter()
            .map(|(wid, ws)| {
                let show = |selection: Option<LensSelection>| {
                    selection.map_or_else(
                        || "closed".to_owned(),
                        |selection| {
                            let tab = selection
                                .tab
                                .map_or_else(String::new, |tab| format!("@TabId({tab})"));
                            format!(
                                "{:?}:{:?}:{:?}{tab}",
                                selection.lens.focus, selection.lens.bar, selection.target
                            )
                        },
                    )
                };
                format!(
                    "stats-lens window={} desired={} presented={} pending={}",
                    wid.0,
                    show(ws.stats_lens.selection(ws.tab_set.active_id())),
                    ws.stats_lens
                        .presented_selection
                        .map_or_else(|| "unknown".to_owned(), show),
                    u8::from(ws.stats_lens.input.token().is_some())
                )
            })
            .collect()
    }

    fn stats_lens_key_untimed(
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
            // The lens's two copies write the person's clipboard, which a control client's
            // gesture never reaches: a key an `hwkey` burst posted arrives here by the person's
            // key path under the controller mark ([`App::dispatch_hw_key`]).
            if cpu_report_key(Platform::host())
                && open.focus == Instrument::Cpu
                && command("c")
                && mods.shift_key()
            {
                if !self.refuse_clipboard_gesture("CPU diagnostic report copy")
                    && let Some(snap) = self.vitals.snapshot.as_deref()
                {
                    let _ = crate::control::pbcopy(&cpu_diagnostic_report(snap));
                }
                return true;
            }
            #[cfg(target_os = "macos")]
            if open.focus == Instrument::Cpu && command("p") && mods.shift_key() {
                let _ = self.collect_cpu_policy(wid);
                return true;
            }
            #[cfg(target_os = "macos")]
            if open.focus == Instrument::Cpu && command("x") && mods.shift_key() {
                self.cancel_cpu_policy(wid);
                return true;
            }
            if open.focus == Instrument::NetworkState
                && matches!(key, Key::Named(NamedKey::ArrowDown | NamedKey::ArrowUp))
            {
                let band = self.network_band_px(wid);
                if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
                    let ws = &mut **route_guard_ws;
                    ws.stats_lens.retire_input();
                    ws.stats_lens.network_target = crate::network_deck::step(
                        &self.network_snapshot,
                        &self.lab_deck,
                        self.fleet_field.as_deref(),
                        band,
                        ws.stats_lens.network_target,
                        matches!(key, Key::Named(NamedKey::ArrowDown)),
                    );
                }
                self.request_window_redraw(wid);
                return true;
            }
            // Tab switches the Disk lens's used-space chart between the last 24 hours and the
            // last 3 weeks. A named key, as the lens's others are: every printable key still
            // closes the lens and goes on to the program. ⌘, ⌃ and ⌥ with Tab go on as they did.
            if open.focus == Instrument::Disk
                && !(mods.super_key() || mods.control_key() || mods.alt_key())
                && matches!(key, Key::Named(NamedKey::Tab))
            {
                if let Some(mut ws) = self.windows.get_mut(&wid) {
                    ws.stats_lens.retire_input();
                    ws.stats_lens.disk_span = ws.stats_lens.disk_span.other();
                }
                self.request_window_redraw(wid);
                return true;
            }
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
                if !self.refuse_clipboard_gesture("lens copy") {
                    let _ = self.copy_stats_sentence(wid);
                }
                return true;
            }
        }
        self.close_stats_lens(wid);
        false
    }

    /// Whether a pinned or keyboard lens owns Copy, and whether its sentence is available.
    /// `Some(false)` retains ownership when the snapshot is unavailable.
    pub(crate) fn stats_lens_copy_ready(&self, wid: crate::WindowId) -> Option<bool> {
        self.windows
            .get(&wid)
            .and_then(|ws| ws.stats_lens.open)
            .filter(|lens| lens.how != Opened::Hover)
            .map(|lens| lens.focus == Instrument::NetworkState || self.vitals.snapshot.is_some())
    }

    /// The open lens's sentence.
    pub(crate) fn stats_lens_sentence(&self, wid: crate::WindowId) -> Option<String> {
        let open = self.windows.get(&wid).and_then(|ws| ws.stats_lens.open)?;
        if open.focus == Instrument::NetworkState {
            let target = self.windows.get(&wid)?.stats_lens.network_target;
            return Some(
                crate::network_deck::details_lab(
                    &self.network_snapshot,
                    &self.lab_deck,
                    self.fleet_field.as_deref(),
                    target,
                )
                .into_iter()
                .map(|(name, value)| format!("{name}: {value}"))
                .collect::<Vec<_>>()
                .join(". "),
            );
        }
        let snap = self.vitals.snapshot.as_deref()?;
        let panes = self.deck_tab_panes(wid);
        let loc = crate::stats_deck::host_locale(Some(snap));
        let sentence = lens_sentence(snap, open.focus, &panes, &loc);
        Some(
            if open.focus != Instrument::HowToRead && !self.deck_live_now(wid).0.fresh {
                let at = clock_s(snap.at_unix_ms, loc.offset_at_ms(snap.at_unix_ms));
                format!("Updates delayed; last reading {at}. {sentence}")
            } else {
                sentence
            },
        )
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
                if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
                    let ws = &mut **route_guard_ws;
                    ws.stats_lens.rest = None;
                }
                let still = self
                    .stats_hit_under_pointer(wid)
                    .is_some_and(|h| h.kind == rest.hit.kind);
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
    pub(crate) fn request_window_redraw(&self, wid: crate::WindowId) {
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
