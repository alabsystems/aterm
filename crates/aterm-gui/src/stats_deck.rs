// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pixel-rendered Global, aterm, and Tab stats bars under the titlebar.

use std::sync::Arc;

use aterm_vitals::ink::{self, Rgb, VitalsInks};
use aterm_vitals::model::{DiskLevel, HeatCell, Resource, ResourceView, ScopeFig, Snapshot, State};
use aterm_vitals::words::{self, InkKind, Ladder, Locale, Platform, ScopeForms};

#[cfg(test)]
use crate::deck_face::Boxed;
use crate::deck_face::{
    ASCENT_EM, BoxKind, Boxes, DESCENT_EM, FaceMetrics, Txt, deck_metrics, fit, txts,
};
use crate::stats_paint::{Bar, Hit, Instrument, Painter, Rect, StatsMark, WellMark, panel};
use crate::stats_plan::{
    self as plan, BlockKind, BlockPlan, Cluster, Col, CoresAt, CoresMode, DeckForm, DeckSizePref,
    FullKind, FullPlan, HeaderPlan, HeaderText, KeyItem, Label, Rows, StateLabel, Strip,
};
use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, TextFace, TextWeight, rgba, text_prim};

/// The Folded deck's height in points (124 device px at 2×, 62 at 1×): two blocks of four rows.
pub(crate) const FOLDED_PT: f64 = 62.0;

/// The Full table's height in points, before what it adds to the Folded height is rounded up
/// to whole terminal cells.
pub(crate) const FULL_TABLE_PT: f64 = 100.0;

/// Which form the stats bars take (`[sys] stats_bars`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum StatsBars {
    /// The three bars at full size under the titlebar (the default).
    Deck,
    /// The three bars as light lanes in the window's bottom pad: no terminal row, ever.
    Sill,
    /// No bars.
    Off,
}

impl StatsBars {
    /// Every form, in the View ▸ Stats Bars menu's order.
    pub(crate) const ALL: [Self; 3] = [Self::Deck, Self::Sill, Self::Off];

    /// Parse the config value: `"deck"` (default), `"sill"`, `"off"`.
    pub(crate) fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some(v) if v.eq_ignore_ascii_case("off") || v.eq_ignore_ascii_case("false") => {
                Self::Off
            }
            Some(v) if v.eq_ignore_ascii_case("sill") => Self::Sill,
            _ => Self::Deck,
        }
    }

    /// The config word for this form.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Deck => "deck",
            Self::Sill => "sill",
            Self::Off => "off",
        }
    }

    /// The form's name in a menu.
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Deck => "Deck",
            Self::Sill => "Sill",
            Self::Off => "Off",
        }
    }

    /// The form a config word names, or `None` for a word that is not one.
    pub(crate) fn from_word(word: &str) -> Option<Self> {
        let word = word.trim();
        Self::ALL
            .into_iter()
            .find(|f| f.word().eq_ignore_ascii_case(word))
    }

    /// Whether any bars show.
    pub(crate) const fn shown(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// Whether the form takes room from the grid: the Deck's band in the head.
    pub(crate) const fn takes_room(self) -> bool {
        matches!(self, Self::Deck)
    }
}

/// The Deck's height in device pixels at `scale` in the window's form `deck`, or 0 when the
/// Deck is not the stats bars' form.
pub(crate) fn deck_px(form: StatsBars, deck: DeckForm, scale: f64, cell_px: usize) -> usize {
    if !(form.takes_room() && scale.is_finite() && scale > 0.0) {
        return 0;
    }
    let folded = (FOLDED_PT * scale).round() as usize;
    match deck {
        DeckForm::Folded => folded,
        DeckForm::Full => folded + crate::stats_plan::full_delta_px(scale, cell_px),
    }
}

/// An attached window's chrome headroom in device pixels.
pub(crate) fn chrome_head_px(
    head_pts: f64,
    scale: f64,
    form: StatsBars,
    deck: DeckForm,
    cell_px: usize,
) -> usize {
    band_px(head_pts, scale) + deck_px(form, deck, scale, cell_px)
}

fn band_px(head_pts: f64, scale: f64) -> usize {
    (head_pts * scale).round().max(0.0) as usize
}

/// Headless stats seam (development only, [`aterm_types::dev_seam!`]).
pub(crate) fn deck_on_headless() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        aterm_types::dev_seam!("ATERM_DEV_STATS_DECK_HEADLESS")
            .is_some_and(|v| v.to_str().is_some_and(|v| v.trim() == "1"))
    })
}

/// Whether `ws` draws the stats bars.
pub(crate) fn stats_drawn_on(ws: &crate::WindowState) -> bool {
    ws.os_window.is_some() || deck_on_headless() || tests_attached()
}

#[cfg(test)]
thread_local! {
    static TESTS_ATTACHED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn tests_attached() -> bool {
    TESTS_ATTACHED.with(std::cell::Cell::get)
}

#[cfg(not(test))]
const fn tests_attached() -> bool {
    false
}

/// [`ink::vitals_inks`] on a ground, its words and its accent, remembered for the last four
/// grounds (a solve takes microseconds, tens of them on a light ground).
pub(crate) fn cached_inks(ground: Rgb, value: Rgb, accent: Rgb) -> VitalsInks {
    type Key = (Rgb, Rgb, Rgb);
    thread_local! {
        static SOLVED: std::cell::RefCell<Vec<(Key, VitalsInks)>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }
    let key = (ground, value, accent);
    SOLVED.with(|cache| {
        if let Some((_, inks)) = cache.borrow().iter().find(|(k, _)| *k == key) {
            return *inks;
        }
        let inks = ink::vitals_inks(ground, value, accent);
        let mut cache = cache.borrow_mut();
        if cache.len() >= 4 {
            cache.remove(0);
        }
        cache.push((key, inks));
        inks
    })
}

impl crate::App {
    /// The stats bars' form from the live config.
    pub(crate) fn stats_bars_form(&self) -> StatsBars {
        self.config.stats_bars_or_default()
    }

    /// The deck's size setting from the live config (`[sys] stats_deck_size`).
    pub(crate) fn deck_size_pref(&self) -> DeckSizePref {
        self.config.stats_deck_size_or_default()
    }

    /// Window `wid`'s chrome head in device pixels for a `head_pts` titlebar band at `scale`.
    pub(crate) fn deck_head_px(
        &mut self,
        wid: crate::WindowId,
        head_pts: f64,
        scale: f64,
        frame_h_px: u32,
    ) -> usize {
        let form = self.stats_bars_form();
        let pref = self.deck_size_pref();
        let (_, cell_px) = self.win_cell_size(wid);
        let deck = match self.windows.get_mut(&wid) {
            Some(ws) => {
                if frame_h_px > 0 {
                    let titlebar = band_px(head_pts, scale) as f64;
                    let avail = crate::stats_plan::avail_pt(f64::from(frame_h_px), titlebar, scale);
                    ws.deck_form = crate::stats_plan::deck_form_for(avail, ws.deck_form, pref);
                }
                ws.deck_form
            }
            None => DeckForm::Folded,
        };
        chrome_head_px(head_pts, scale, form, deck, cell_px)
    }

    /// The Deck's height in window `wid`'s head when it draws there, in the window's own form
    /// and at its own cell height, as the head sites count it.
    pub(crate) fn deck_drawn_px(&self, wid: crate::WindowId) -> usize {
        let form = self.stats_bars_form();
        let Some(ws) = self.windows.get(&wid) else {
            return 0;
        };
        if ws.cols == 0 || !stats_drawn_on(ws) {
            return 0;
        }
        let (_, ch) = self.win_cell_size(wid);
        match deck_px(form, ws.deck_form, ws.scale, ch) {
            deck if deck > 0 && ws.metrics.head >= deck => deck,
            _ => 0,
        }
    }

    /// The Deck's rows in window `wid`'s frame in device pixels while it draws in the head
    /// (`Self::deck_drawn_px`).
    pub(crate) fn deck_rows_px(&self, wid: crate::WindowId) -> Option<std::ops::Range<usize>> {
        let deck = self.deck_drawn_px(wid);
        if deck == 0 {
            return None;
        }
        let head = self.windows.get(&wid)?.metrics.head;
        let top = if crate::tab_bar::STRIP_IS_CHROME_BAND {
            0
        } else {
            head - deck
        };
        Some(top..top + deck)
    }

    /// Where window `wid`'s strip band starts above its first row, in device px.
    pub(crate) fn strip_band_top_px(&self, wid: crate::WindowId) -> usize {
        let top = self.win_pad_top(wid) + self.win_head(wid);
        if crate::tab_bar::STRIP_IS_CHROME_BAND {
            top.saturating_sub(self.deck_drawn_px(wid))
        } else {
            top
        }
    }
}

/// Where a stats surface is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DeckGeom {
    /// Card width (the frame's width).
    pub(crate) w: f32,
    /// Card height ([`deck_px`], or the bottom pad for the sill).
    pub(crate) h: f32,
    /// The window's side pad.
    pub(crate) pad: f32,
    /// Device pixels per point.
    pub(crate) scale: f32,
    /// The terminal font's pixel size (the chrome text base).
    pub(crate) font_px: f32,
}

/// What a stats surface shows.
pub(crate) struct DeckInput<'a> {
    /// The latest snapshot, `None` before the first.
    pub(crate) snap: Option<&'a Snapshot>,
    /// This window's active tab's sessions, its focused pane first (`App::deck_tab_panes`):
    /// every figure of "this tab" is theirs summed ([`Snapshot::resource`], [`Snapshot::tab_of`]).
    pub(crate) tab_panes: Vec<u64>,
    /// Its label, `Tab 2`.
    pub(crate) tab_label: String,
    /// How many tabs this window holds.
    pub(crate) tab_count: usize,
    /// Inks for the ground.
    pub(crate) inks: VitalsInks,
    /// Differentiate Without Color (System Settings ▸ Accessibility).
    pub(crate) dwc: bool,
    /// The instance's deviations, worst first (⊘ > ☠ > ◔ > ◇ > stopped).
    pub(crate) marks: Vec<StatsMark>,
    /// The sill's plate behind each instrument, as an alpha, while the floor is translucent.
    pub(crate) plate: Option<u8>,
    /// The Deck's form for the window (Stats Deck v2 §2).
    pub(crate) form: DeckForm,
    /// Increase Contrast (System Settings ▸ Accessibility; §9.5).
    pub(crate) contrast: bool,
    /// Whose wording and which clock the words say times on.
    pub(crate) locale: Locale,
}

const HEADER_BASE_FULL: f64 = 11.5;
const HEADER_BASE_FOLDED: f64 = 9.5;
const HEADER_HIT_FULL: f64 = 14.5;
const HEADER_HIT_FOLDED: f64 = 12.5;
const GRID_FROM_FULL: f64 = 14.5;
const GRID_FROM_FOLDED: f64 = 12.5;
const SMALL_PT: f64 = 9.5;
const MARK_EM: f64 = 1.1;
const STATE_GAP: f64 = 6.0;
const STRIP_PERIODS: [u32; 3] = [10, 20, 30];

/// The scope bar's segments as shares of the row's capacity (§6), each
/// `0..=1`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct BarFill {
    /// The machine's; none on Tokens, which has no Global figure (§6.6).
    global: Option<f32>,
    aterm: Option<f32>,
    tab: Option<f32>,
    /// CPU's cores, a tick gap between each where they are 3 pt apart.
    ticks: Option<u32>,
    /// Disk's warn line, `1 − warn/total`, and whether the level is past it.
    warn_tick: Option<(f32, bool)>,
}

/// A row's state for where Details is absent (rule S) and for the fslim
/// header (§4.4).
#[derive(Clone, Debug, PartialEq)]
struct StateTxt {
    state: State,
    clauses: Vec<Txt>,
    word: Txt,
    amounts: Vec<Txt>,
    label: String,
    mark: WellMark,
    ink: InkKind,
}

/// A row's aterm and Tab cells (§6).
#[derive(Clone, Debug, PartialEq)]
enum ScopeTxt {
    /// None drawn (the GPU with no reading).
    None,
    /// A cell in each column.
    Each {
        aterm: (Vec<Txt>, InkKind),
        tab: (Vec<Txt>, InkKind),
    },
    /// One phrase across both.
    Span(Vec<Txt>, InkKind),
}

/// One Details clause (rule D).
#[derive(Clone, Debug, PartialEq)]
struct ClauseTxt {
    forms: Vec<Txt>,
    prio: u8,
    keep: bool,
    mark: Option<WellMark>,
    ink: InkKind,
}

/// Everything one row draws, read from the snapshot.
#[derive(Clone, Debug, PartialEq)]
struct RowWords {
    res: Resource,
    name: Txt,
    well: Option<(WellMark, InkKind)>,
    heat_now: Option<f32>,
    pct: Txt,
    /// The sentence in the strip's place (§6.2, §6.6), or "reading…": its
    /// forms, longest first (rule F).
    strip_text: Option<Vec<Txt>>,
    /// The strip's columns at each of [`STRIP_PERIODS`].
    cols: [Vec<HeatCell>; 3],
    bar: Option<BarFill>,
    /// The amount's forms on each ladder: Det (with the compact forms that
    /// repeat no Details figure after them), Cols, NoCols.
    amount: [Vec<Txt>; 3],
    state: Option<StateTxt>,
    scope: ScopeTxt,
    details: Vec<ClauseTxt>,
}

/// One cluster of cores, fastest first.
#[derive(Clone, Debug, PartialEq)]
struct CoreCluster {
    /// The OS's name; none where it names none (Linux's one "CPU").
    name: Option<String>,
    /// Each core's fast-average busy fraction; `None` when unreadable.
    heat: Vec<Option<f32>>,
}

/// The deck's words for one snapshot: what every row says, measured in the deck's face.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DeckWords {
    rows: Vec<RowWords>,
    cores: Vec<CoreCluster>,
    /// No snapshot yet: the rows say they are reading, and nothing is a hit.
    reading: bool,
}

/// The OS resources the Resource Monitor deck draws.
pub(crate) const DECK_RESOURCES: [Resource; 6] = [
    Resource::Cpu,
    Resource::Gpu,
    Resource::Memory,
    Resource::Disk,
    Resource::Network,
    Resource::Processes,
];

impl DeckWords {
    /// `input`'s snapshot in words, measured with `m`.
    pub(crate) fn new(input: &DeckInput<'_>, m: &FaceMetrics) -> Self {
        let Some(snap) = input.snap else {
            return Self::reading(m);
        };
        let rows = DECK_RESOURCES
            .iter()
            .map(|&res| RowWords::new(res, snap, &input.tab_panes, &input.locale, m))
            .collect();
        let cores = snap
            .cpu
            .as_ref()
            .map(|cpu| {
                cpu.clusters
                    .iter()
                    .map(|c| CoreCluster {
                        name: (c.name != "CPU").then(|| c.name.clone()),
                        heat: c
                            .cores
                            .iter()
                            .map(|&i| {
                                cpu.cores
                                    .get(i)
                                    .map(|core| core.load.level)
                                    .filter(|v| v.is_finite())
                            })
                            .collect(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            rows,
            cores,
            reading: false,
        }
    }

    fn reading(m: &FaceMetrics) -> Self {
        let rows = DECK_RESOURCES
            .iter()
            .map(|&res| RowWords {
                res,
                name: Txt::new(words::resource_name(res), m),
                well: None,
                heat_now: None,
                pct: Txt::new(words::pct(None), m),
                strip_text: Some(vec![Txt::new("reading\u{2026}", m)]),
                cols: [Vec::new(), Vec::new(), Vec::new()],
                bar: None,
                amount: [Vec::new(), Vec::new(), Vec::new()],
                state: None,
                scope: ScopeTxt::None,
                details: Vec::new(),
            })
            .collect();
        Self {
            rows,
            cores: Vec::new(),
            reading: true,
        }
    }

    fn clusters(&self) -> Vec<Cluster<'_>> {
        self.cores
            .iter()
            .map(|c| Cluster {
                count: c.heat.len(),
                name: c.name.as_deref(),
            })
            .collect()
    }
}

impl RowWords {
    fn new(res: Resource, snap: &Snapshot, panes: &[u64], loc: &Locale, m: &FaceMetrics) -> Self {
        let view = snap.resource(res, panes);
        let facts = &snap.facts;
        let forms = |ladder| words::amount_forms(res, &view, facts, ladder, loc);
        let mut det = forms(Ladder::Det);
        let cols = forms(Ladder::Cols);
        for f in &cols {
            if !f.contains("freed") && !det.contains(f) {
                det.push(f.clone());
            }
        }
        let strip_text = words::strip_words(res, snap).map(|forms| txts(forms, m));
        let unread_gpu = res == Resource::Gpu && view.now.is_none();
        let scope = if unread_gpu {
            ScopeTxt::None
        } else {
            match words::scope_forms(res, &view, loc) {
                ScopeForms::Each { aterm, tab } => ScopeTxt::Each {
                    aterm: (txts(aterm.forms, m), aterm.ink),
                    tab: (txts(tab.forms, m), tab.ink),
                },
                ScopeForms::Span(cell) => ScopeTxt::Span(txts(cell.forms, m), cell.ink),
            }
        };
        let state = words::state(res, snap, loc).map(|st| StateTxt {
            state: st.state,
            clauses: txts(st.clauses, m),
            word: Txt::new(st.word, m),
            amounts: txts(st.amount_forms, m),
            label: st.label,
            mark: WellMark::of(st.mark),
            ink: st.ink,
        });
        let details = words::details(res, snap, loc)
            .into_iter()
            .map(|c| ClauseTxt {
                forms: txts(c.forms, m),
                prio: c.prio,
                keep: c.keep,
                mark: c.mark.map(WellMark::of),
                ink: c.ink,
            })
            .collect();
        Self {
            res,
            name: Txt::new(words::resource_name(res), m),
            well: words::well(res, &view).map(|(mark, ink)| (WellMark::of(mark), ink)),
            heat_now: view.heat_now.filter(|h| !h.is_nan()),
            pct: Txt::new(words::pct(view.heat_now), m),
            strip_text,
            cols: STRIP_PERIODS
                .map(|per| view.heat_columns(per, (600 / per) as usize, snap.at_unix_ms)),
            bar: bar_fill(res, &view, snap),
            amount: [txts(det, m), txts(cols, m), txts(forms(Ladder::NoCols), m)],
            state,
            scope,
            details,
        }
    }
}

fn bar_fill(res: Resource, view: &ResourceView, snap: &Snapshot) -> Option<BarFill> {
    let cap = view.binding().capacity;
    let share = |fig: &ScopeFig| -> Option<f32> {
        let (v, c) = (fig.now?, cap?);
        (c > 0.0).then(|| (v / c).clamp(0.0, 1.0) as f32)
    };
    let machine = view.heat_now.filter(|h| !h.is_nan()).map(|h| h.min(1.0));
    let global = machine.filter(|_| res != Resource::Tokens);
    let scoped = res.scope_shares_capacity();
    let aterm = if scoped { share(&view.aterm) } else { None };
    let tab = if scoped { share(&view.tab) } else { None };
    if global.is_none() && aterm.is_none() {
        return None;
    }
    let ticks = (res == Resource::Cpu)
        .then(|| view.capacity.map(|c| c.round() as u32))
        .flatten();
    let warn_tick = (res == Resource::Disk)
        .then(|| {
            let d = snap.facts.disk.as_ref().filter(|d| d.watch)?;
            let total = view.capacity.filter(|&t| t > 0.0)?;
            let at = (1.0 - d.warn? as f64 / total).clamp(0.0, 1.0) as f32;
            Some((at, d.level.is_some_and(|l| l != DiskLevel::Ok)))
        })
        .flatten();
    Some(BarFill {
        global,
        aterm,
        tab,
        ticks,
        warn_tick,
    })
}

fn period_ix(strip: &Strip) -> usize {
    STRIP_PERIODS
        .iter()
        .position(|&p| p == strip.period_s)
        .unwrap_or(0)
}

const fn ladder_ix(details: bool, cols: bool) -> usize {
    match (details, cols) {
        (true, _) => 0,
        (false, true) => 1,
        (false, false) => 2,
    }
}

/// One table's columns: the Full plan's, or one Folded block's.
#[derive(Clone, Copy, Debug)]
struct Table {
    rows: Rows,
    name: Col,
    well: Col,
    strip: Strip,
    pct: Option<Col>,
    bar: Option<Col>,
    amount: Option<Col>,
    aterm: Option<Col>,
    tab: Option<Col>,
    details: Option<Col>,
    /// The well mark's size.
    mark_pt: f64,
    /// Where a row's hit region ends.
    end: f64,
}

impl Table {
    fn of_full(p: &FullPlan) -> Self {
        let end = [p.pct, p.bar, p.amount, p.aterm, p.tab, p.details]
            .into_iter()
            .flatten()
            .map(Col::end)
            .fold(p.strip.end(), f64::max);
        Self {
            rows: p.rows,
            name: p.name,
            well: p.well,
            strip: p.strip,
            pct: p.pct,
            bar: p.bar,
            amount: p.amount,
            aterm: p.aterm,
            tab: p.tab,
            details: p.details,
            mark_pt: match p.kind {
                FullKind::Wide | FullKind::Mid => 10.0,
                FullKind::Narrow | FullKind::Slim => 9.5,
            },
            end,
        }
    }

    fn of_block(b: &BlockPlan) -> Self {
        Self {
            rows: b.rows,
            name: b.name,
            well: b.well,
            strip: b.strip,
            pct: b.pct,
            bar: b.bar,
            amount: b.amount,
            aterm: b.aterm,
            tab: b.tab,
            details: b.details,
            mark_pt: match b.kind {
                BlockKind::Wide | BlockKind::Mid => 9.5,
                BlockKind::Narrow | BlockKind::Slim => 8.5,
            },
            end: b.at.end(),
        }
    }

    fn band(&self, r: u32) -> (f64, f64) {
        let gap = self.rows.pitch - self.rows.cell;
        let top = self.rows.top(r);
        (top - gap / 2.0, top + self.rows.cell + gap / 2.0)
    }

    fn ladder(&self) -> usize {
        ladder_ix(self.details.is_some(), self.aterm.is_some())
    }
}

struct HeadState<'w> {
    /// The block's name x, where the planner put its label.
    x: f64,
    res: Resource,
    st: &'w StateTxt,
    full: String,
}

/// A state's ink: the mark's ink its words take in the deck's rows and the lens's answer line.
pub(crate) const fn kind_ink(i: &VitalsInks, k: InkKind) -> Rgb {
    match k {
        InkKind::Words => i.words,
        InkKind::Words2 => i.words2,
        InkKind::Warn => i.warn_text,
        InkKind::Critical => i.critical_text,
        InkKind::Stuck => i.stuck,
        InkKind::Over => i.over,
    }
}

struct Lattice {
    x: f32,
    y: f32,
    step: f32,
    side: f32,
}

struct Deck<'a> {
    p: Painter<'a>,
    m: &'a FaceMetrics,
    /// The inks, with Increase Contrast's applied (§9.5).
    inks: VitalsInks,
    s: f64,
    /// The deck's width and height, pt.
    w_pt: f64,
    h_pt: f64,
    ic: bool,
    dwc: bool,
    /// Draw the prims, or only lay out (T-G4's sweep).
    draw: bool,
    /// Strip cells drawn (T-G9).
    cells: usize,
    tab: &'a str,
    machine: &'static str,
    /// Hits by precedence: the first containing the pointer wins.
    hits_first: Vec<Hit>,
    hits_rows: Vec<Hit>,
    hits_last: Vec<Hit>,
    boxes: Boxes,
    /// The header line as planned (T-G14).
    #[cfg(test)]
    head: Option<HeaderPlan>,
}

impl<'a> Deck<'a> {
    fn new(g: &DeckGeom, input: &'a DeckInput<'a>, m: &'a FaceMetrics, draw: bool) -> Self {
        let s = f64::from(g.scale.max(0.5));
        let mut inks = input.inks;
        if input.contrast {
            inks.hollow = inks.words2;
            inks.rim = inks.words2;
        }
        Self {
            p: Painter::new(&input.inks, g.scale, g.font_px, input.dwc),
            m,
            inks,
            s,
            w_pt: f64::from(g.w) / s,
            h_pt: f64::from(g.h) / s,
            ic: input.contrast,
            dwc: input.dwc,
            draw,
            cells: 0,
            tab: &input.tab_label,
            machine: input.locale.platform.machine(),
            hits_first: Vec::new(),
            hits_rows: Vec::new(),
            hits_last: Vec::new(),
            boxes: Boxes::default(),
            #[cfg(test)]
            head: None,
        }
    }

    fn px(&self, v: f64) -> f32 {
        (v * self.s + 0.5).floor() as f32
    }

    fn px_rect(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> Rect {
        let (a, b) = (self.px(x0), self.px(y0));
        (a, b, self.px(x1) - a, self.px(y1) - b)
    }

    fn note(&mut self, kind: BoxKind, r: [f64; 4], text: &str, room: Option<(f64, f64)>) {
        self.boxes.note(kind, r, text, room);
    }

    fn ink(&self, k: InkKind) -> Rgb {
        kind_ink(&self.inks, k)
    }

    fn mark_ink(&self, mark: WellMark, k: InkKind) -> Rgb {
        match mark {
            WellMark::Up | WellMark::Down | WellMark::Cause => self.inks.words2,
            _ => self.ink(k),
        }
    }

    fn heat(&self, step: u8) -> Rgb {
        let ramp = if self.ic {
            &self.inks.heat_ic
        } else {
            &self.inks.heat
        };
        ramp[usize::from(step.min(6))]
    }

    const fn scope_ink(&self, bar: Bar) -> Rgb {
        let i = &self.inks;
        match (bar, self.ic) {
            (Bar::Global, false) => i.machine,
            (Bar::Global, true) => i.machine_ic,
            (Bar::Aterm, false) => i.aterm,
            (Bar::Aterm, true) => i.aterm_ic,
            (Bar::Tab, false) => i.tab,
            (Bar::Tab, true) => i.tab_ic,
        }
    }

    fn rect(&mut self, (x, y, w, h): Rect, radius: f32, ink: Rgb) {
        if self.draw && w > 0.0 && h > 0.0 {
            self.p
                .prims
                .push(panel(x, y, w, h, radius, rgba(ink, 0xFF)));
        }
    }

    /// `s` at `(x, base)` in pt; answers its advance. `room` is the column
    /// it belongs to.
    #[expect(
        clippy::too_many_arguments,
        reason = "a text run's place, size, weight, ink and column"
    )]
    fn text(
        &mut self,
        x: f64,
        base: f64,
        s: String,
        pt: f64,
        bold: bool,
        ink: Rgb,
        room: Option<(f64, f64)>,
    ) -> f64 {
        if s.is_empty() {
            return 0.0;
        }
        let w = self.m.width(&s, pt, bold);
        self.note(
            BoxKind::Text,
            [x, base - ASCENT_EM * pt, x + w, base + DESCENT_EM * pt],
            &s,
            room,
        );
        if self.draw {
            let px = (pt * self.s) as f32;
            self.p.prims.push(text_prim(
                self.px(x),
                self.px(base),
                s,
                TypeStep::Caption.px_clamped(px, px, px),
                TextWeight::Regular,
                if bold { TextFace::UiBold } else { TextFace::Ui },
                rgba(ink, 0xFF),
            ));
        }
        w
    }

    fn mark(&mut self, kind: WellMark, cx: f64, cy: f64, size: f64, ink: Rgb) {
        let r = 0.36 * size;
        self.note(
            BoxKind::Mark,
            [cx - r, cy - r, cx + r, cy + r],
            kind.word(),
            None,
        );
        if self.draw {
            let lw = (0.9 * self.s).round().max(1.0) as f32;
            let s = self.s;
            self.p.mark_in(
                kind,
                ((cx * s) as f32, (cy * s) as f32, (r * s) as f32),
                crate::stats_paint::QUIET_ARC,
                ink,
                lw,
            );
        }
    }

    /// `[mark] text`: the mark at `x + 0.42 em`, the words after 1.1 em;
    /// answers the advance.
    #[expect(
        clippy::too_many_arguments,
        reason = "a marked run's place, mark, size, weight, ink and column"
    )]
    fn marked(
        &mut self,
        x: f64,
        base: f64,
        mark: Option<(WellMark, Rgb)>,
        s: String,
        pt: f64,
        bold: bool,
        ink: Rgb,
        room: Option<(f64, f64)>,
    ) -> f64 {
        let mut adv = 0.0;
        if let Some((kind, mark_ink)) = mark {
            self.mark(kind, x + 0.42 * pt, base - 0.34 * pt, pt, mark_ink);
            adv = MARK_EM * pt;
        }
        adv + self.text(x + adv, base, s, pt, bold, ink, room)
    }

    fn ground(&mut self) {
        let (w, h) = (self.px(self.w_pt), self.px(self.h_pt));
        self.rect((0.0, 0.0, w, h), 0.0, self.inks.ground);
        let seam = self.px(self.h_pt - 1.0);
        self.rect((0.0, seam, w, h - seam), 0.0, self.inks.seam);
    }

    fn cap_px(&self) -> Option<(f32, f32)> {
        (self.s >= 2.0).then(|| {
            (
                (1.5 * self.s).round().max(2.0) as f32,
                (0.5 * self.s).round().max(1.0) as f32,
            )
        })
    }

    fn small_cells(&self, strip: &Strip, cell_h: f64) -> bool {
        self.px(strip.cell) < 4.0 || self.px(cell_h) < 4.0
    }

    fn strip(&mut self, strip: &Strip, top: f64, cell_h: f64, cols: &[HeatCell]) {
        let cap_room = self
            .cap_px()
            .map_or(0.0, |(h, gap)| f64::from(h + gap) / self.s);
        self.note(
            BoxKind::Strip,
            [strip.x, top - cap_room, strip.end(), top + cell_h],
            "",
            None,
        );
        let n = strip.cols as usize;
        self.cells += n;
        if !self.draw {
            return;
        }
        let (x0, y, h) = (self.px(strip.x), self.px(top), self.px(cell_h));
        let (pitch, cw) = (self.px(strip.pitch), self.px(strip.cell));
        let radius = if self.s < 2.0 { 0.0 } else { 1.0 };
        let small = self.small_cells(strip, cell_h);
        let cap = self.cap_px();
        let unread = n.saturating_sub(cols.len());
        for i in 0..n {
            let x = x0 + i as f32 * pitch;
            let cell = i
                .checked_sub(unread)
                .and_then(|j| cols.get(j))
                .copied()
                .unwrap_or_default();
            let Some(v) = cell.heat.filter(|v| !v.is_nan()) else {
                self.hollow_cell((x, y, cw, h), radius, small);
                continue;
            };
            let step = ink::heat_step(v);
            let hh = if self.dwc {
                ink::dwc_height(h as u32, step) as f32
            } else {
                h
            };
            self.rect((x, y + h - hh, cw, hh), radius, self.heat(step));
            if cell.over {
                let over = self.inks.over;
                match cap {
                    Some((ch, gap)) => self.rect((x, y - ch - gap, cw, ch), 0.0, over),
                    None => self.rect((x, y, cw, h.min(2.0)), 0.0, over),
                }
            }
        }
    }

    fn hollow_cell(&mut self, (x, y, w, h): Rect, radius: f32, small: bool) {
        let ink = self.inks.hollow;
        if small {
            self.rect((x, y + h - 1.0, w, 1.0), 0.0, ink);
        } else if self.draw {
            self.p.prims.push(DrawPrim::Stroke {
                x: x + 0.5,
                y: y + 0.5,
                w: w - 1.0,
                h: h - 1.0,
                radius,
                width: 1.0,
                color: rgba(ink, 0xFF),
            });
        }
    }

    fn gridlines(&mut self, strip: &Strip, y0: f64, y1: f64, holes: &[(f64, f64)]) {
        if !self.draw {
            return;
        }
        let per_min = (60 / strip.period_s.max(1)).max(1) as usize;
        let pitch = self.px(strip.pitch);
        let gap = pitch - self.px(strip.cell);
        let x0 = self.px(strip.x);
        let mut spans = Vec::new();
        let mut y = y0;
        for &(a, b) in holes {
            if a > y {
                spans.push((y, a));
            }
            y = y.max(b);
        }
        if y < y1 {
            spans.push((y, y1));
        }
        let grid = self.inks.grid;
        for c in (per_min..strip.cols as usize).step_by(per_min) {
            let gx = x0 + c as f32 * pitch - gap;
            for &(a, b) in &spans {
                let (ya, yb) = (self.px(a), self.px(b));
                self.rect((gx, ya, gap.max(1.0), yb - ya), 0.0, grid);
            }
        }
    }

    fn bar(&mut self, col: Col, top: f64, h_pt: f64, fill: &BarFill) {
        self.note(BoxKind::Bar, [col.x, top, col.end(), top + h_pt], "", None);
        if let Some((at, _)) = fill.warn_tick {
            let x = col.x + f64::from(at) * col.w;
            self.note(
                BoxKind::Tick,
                [x, top - 1.5, x + 1.0, top + h_pt + 1.5],
                "",
                None,
            );
        }
        if !self.draw {
            return;
        }
        let s = self.s as f32;
        let (x, w, y, h) = (self.px(col.x), self.px(col.w), self.px(top), self.px(h_pt));
        let r = (1.5 * s).round().max(1.0);
        self.rect((x, y, w, h), r, self.inks.track);
        let gap = s.round().max(1.0);
        let at = |v: f32| x + (v * w).round().max(gap);
        let seg = |deck: &mut Self, x1: f32, x2: f32, bar: Bar, first: bool| {
            if x2 - x1 >= gap {
                let hh = match (deck.dwc, bar) {
                    (false, _) | (true, Bar::Global) => h,
                    (true, Bar::Aterm) => (h * 0.67).round(),
                    (true, Bar::Tab) => (h * 0.33).round().max(1.0),
                };
                let ink = deck.scope_ink(bar);
                deck.rect(
                    (x1, y + h - hh, x2 - x1, hh),
                    if first { r } else { 0.0 },
                    ink,
                );
            }
        };
        match (fill.global, fill.aterm, fill.tab) {
            (Some(g), Some(a), Some(t)) => {
                let (gx, ax, tx) = (at(g), at(a.min(g)), at(t.min(a)));
                seg(self, x, tx, Bar::Tab, true);
                seg(self, tx + gap, ax, Bar::Aterm, false);
                seg(self, ax + gap, gx, Bar::Global, false);
            }
            (Some(g), Some(a), None) => {
                let (gx, ax) = (at(g), at(a.min(g)));
                seg(self, x, ax, Bar::Aterm, true);
                seg(self, ax + gap, gx, Bar::Global, false);
            }
            (Some(g), None, _) => seg(self, x, at(g), Bar::Global, true),
            (None, Some(a), Some(t)) => {
                let (ax, tx) = (at(a), at(t.min(a)));
                seg(self, x, tx, Bar::Tab, true);
                seg(self, tx + gap, ax, Bar::Aterm, false);
            }
            (None, Some(a), None) => seg(self, x, at(a), Bar::Aterm, true),
            (None, None, _) => {}
        }
        if let Some(n) = fill.ticks.filter(|&n| n > 1 && col.w / f64::from(n) >= 3.0) {
            let tw = (0.5 * s).round().max(1.0);
            let ground = self.inks.ground;
            for i in 1..n {
                let tx = x + (i as f32 * w / n as f32).round();
                self.rect((tx, y, tw, h), 0.0, ground);
            }
        }
        if let Some((at, warn)) = fill.warn_tick {
            let lx = x + (at * w).round();
            let ext = self.px(1.5);
            let ink = if warn {
                self.inks.warn_text
            } else {
                self.inks.rim
            };
            self.rect((lx, y - ext, s.round().max(1.0), h + 2.0 * ext), 0.0, ink);
        }
    }

    fn amount(&mut self, col: Col, base: f64, pt: f64, row: &RowWords, t: &Table) {
        let room = Some((col.x, col.end()));
        let words_ink = self.inks.words;
        let Some(st) = row.state.as_ref().filter(|_| t.details.is_none()) else {
            let forms = row.amount.get(t.ladder()).map_or(&[][..], Vec::as_slice);
            let text = fit(forms, col.w, pt, false, self.m);
            self.text(col.x, base, text, pt, false, words_ink, room);
            return;
        };
        let madv = MARK_EM * pt;
        let mark = Some((st.mark, self.mark_ink(st.mark, st.ink)));
        let ink = self.ink(st.ink);
        let says: Vec<&Txt> = st.clauses.iter().chain(std::iter::once(&st.word)).collect();
        for said in &says {
            for am in &st.amounts {
                if am.w(pt, false) + STATE_GAP + madv + said.w(pt, true) <= col.w + 0.01 {
                    let w = self.text(col.x, base, am.s.clone(), pt, false, words_ink, room);
                    let x = col.x + w + STATE_GAP;
                    self.marked(x, base, mark, said.s.clone(), pt, true, ink, room);
                    return;
                }
            }
        }
        let text = says
            .iter()
            .find(|said| madv + said.w(pt, true) <= col.w + 0.01)
            .map_or_else(
                || self.m.fit(&[&st.word.s], col.w - madv, pt, true),
                |said| said.s.clone(),
            );
        if !text.is_empty() {
            self.marked(col.x, base, mark, text, pt, true, ink, room);
        }
    }

    fn scope_cells(&mut self, row: &RowWords, aterm: Col, tab: Col, base: f64, pt: f64) {
        match &row.scope {
            ScopeTxt::None => {}
            ScopeTxt::Span(forms, ink) => {
                let room = tab.end() - aterm.x;
                let text = fit(forms, room, SMALL_PT, false, self.m);
                let ink = self.ink(*ink);
                self.text(
                    aterm.x,
                    base,
                    text,
                    SMALL_PT,
                    false,
                    ink,
                    Some((aterm.x, tab.end())),
                );
            }
            ScopeTxt::Each {
                aterm: (af, ai),
                tab: (tf, ti),
            } => {
                for (col, forms, ink) in [(aterm, af, ai), (tab, tf, ti)] {
                    let text = fit(forms, col.w, pt, false, self.m);
                    let ink = self.ink(*ink);
                    self.text(col.x, base, text, pt, false, ink, Some((col.x, col.end())));
                }
            }
        }
    }

    fn details(&mut self, col: Col, base: f64, clauses: &[ClauseTxt]) {
        let pt = SMALL_PT;
        let sep = Txt::new(" \u{b7} ", self.m);
        let madv = MARK_EM * pt;
        let bold =
            |c: &ClauseTxt| matches!(c.ink, InkKind::Warn | InkKind::Critical | InkKind::Stuck);
        let form = |c: &ClauseTxt, short: bool| {
            if short {
                c.forms.len().min(2).saturating_sub(1)
            } else {
                0
            }
        };
        let width = |arr: &[(&ClauseTxt, bool)]| -> f64 {
            arr.iter()
                .enumerate()
                .map(|(i, &(c, short))| {
                    c.forms
                        .get(form(c, short))
                        .map_or(0.0, |t| t.w(pt, bold(c)))
                        + if c.mark.is_some() { madv } else { 0.0 }
                        + if i > 0 { sep.w(pt, false) } else { 0.0 }
                })
                .sum()
        };
        let mut ps: Vec<&ClauseTxt> = clauses.iter().filter(|c| !c.forms.is_empty()).collect();
        ps.sort_by_key(|c| c.prio);
        let keepers: Vec<&ClauseTxt> = ps.iter().copied().filter(|c| c.keep).collect();
        let others: Vec<&ClauseTxt> = ps.iter().copied().filter(|c| !c.keep).collect();
        let mut choice: Option<Vec<(&ClauseTxt, bool)>> = None;
        'sets: for n in (0..=others.len()).rev() {
            let mut kept: Vec<&ClauseTxt> = keepers
                .iter()
                .chain(others.iter().take(n))
                .copied()
                .collect();
            kept.sort_by_key(|c| c.prio);
            let mut arr: Vec<(&ClauseTxt, bool)> = kept.iter().map(|&c| (c, false)).collect();
            if width(&arr) <= col.w + 0.01 {
                choice = Some(arr);
                break;
            }
            for i in (0..arr.len()).rev() {
                if let Some(slot) = arr.get_mut(i) {
                    slot.1 = true;
                }
                if width(&arr) <= col.w + 0.01 {
                    choice = Some(arr);
                    break 'sets;
                }
            }
        }
        let choice = choice.unwrap_or_else(|| {
            let base = if keepers.is_empty() {
                ps.iter().take(1).copied().collect()
            } else {
                keepers.clone()
            };
            base.into_iter().map(|c| (c, true)).collect()
        });
        let room = Some((col.x, col.end()));
        let words2 = self.inks.words2;
        let mut x = col.x;
        for (i, (c, short)) in choice.into_iter().enumerate() {
            let sep_w = if i > 0 { sep.w(pt, false) } else { 0.0 };
            let mark_w = if c.mark.is_some() { madv } else { 0.0 };
            let Some(t) = c.forms.get(form(c, short)) else {
                continue;
            };
            let left = col.end() - x - sep_w - mark_w;
            let text = fit(std::slice::from_ref(t), left, pt, bold(c), self.m);
            if text.is_empty() {
                break;
            }
            if i > 0 {
                x += self.text(x, base, sep.s.clone(), pt, false, words2, room);
            }
            let mark = c.mark.map(|m| (m, self.mark_ink(m, c.ink)));
            let ink = self.ink(c.ink);
            x += self.marked(x, base, mark, text, pt, bold(c), ink, room);
        }
    }

    fn row(&mut self, t: &Table, r: u32, row: &RowWords, reading: bool) {
        let rows = t.rows;
        let top = rows.top(r);
        let base = top + rows.baseline;
        let words_ink = self.inks.words;
        self.text(
            t.name.x,
            base,
            row.name.s.clone(),
            rows.name_pt,
            true,
            words_ink,
            Some((t.name.x, t.name.end())),
        );
        if let Some((mark, ink)) = row.well {
            let ink = self.mark_ink(mark, ink);
            let cy = base - 0.34 * t.mark_pt - 0.2;
            self.mark(mark, t.well.x + t.well.w / 2.0, cy, t.mark_pt, ink);
        }
        match row.strip_text.as_ref() {
            Some(forms) => {
                let room = t.strip.end() - t.strip.x;
                let text = fit(forms, room, SMALL_PT, false, self.m);
                let ink = self.inks.words2;
                let cap_room = self
                    .cap_px()
                    .map_or(0.0, |(h, gap)| f64::from(h + gap) / self.s);
                let base = base.min(top + rows.pitch - cap_room - DESCENT_EM * SMALL_PT);
                self.text(
                    t.strip.x,
                    base,
                    text,
                    SMALL_PT,
                    false,
                    ink,
                    Some((t.strip.x, t.strip.end())),
                );
            }
            None => {
                let cols = row
                    .cols
                    .get(period_ix(&t.strip))
                    .map_or(&[][..], Vec::as_slice);
                self.strip(&t.strip, top, rows.cell, cols);
            }
        }
        if let Some(col) = t.pct {
            let (ink, bold) = match row.heat_now {
                Some(h) => (self.inks.heat_text[usize::from(ink::heat_step(h))], true),
                None => (self.inks.words2, false),
            };
            let w = row.pct.w(rows.name_pt, bold);
            self.text(
                col.end() - w,
                base,
                row.pct.s.clone(),
                rows.name_pt,
                bold,
                ink,
                Some((col.x, col.end())),
            );
        }
        if let (Some(col), Some(fill)) = (t.bar, row.bar.as_ref()) {
            self.bar(col, top + (rows.cell - rows.bar_h) / 2.0, rows.bar_h, fill);
        }
        if let Some(col) = t.amount {
            self.amount(col, base, rows.amount_pt, row, t);
        }
        if let (Some(aterm), Some(tab)) = (t.aterm, t.tab) {
            self.scope_cells(row, aterm, tab, base, rows.figure_pt);
        }
        if let Some(col) = t.details {
            self.details(col, base, &row.details);
        }
        if reading {
            return;
        }
        let (y0, y1) = t.band(r);
        let inst = Instrument::of(row.res);
        if let (Some(aterm), Some(tab)) = (t.aterm, t.tab) {
            for (col, bar) in [(aterm, Bar::Aterm), (tab, Bar::Tab)] {
                let rect = self.px_rect(col.x, y0, col.end(), y1);
                self.hits_first.push(Hit::row(rect, inst, bar));
            }
        }
        let rect = self.px_rect(t.name.x, y0, t.end, y1);
        self.hits_rows.push(Hit::row(rect, inst, Bar::Global));
    }

    fn lattice(&self, x: f64, y: f64, pitch: f64, side: f64) -> Lattice {
        let step = (pitch * self.s).floor().max(2.0) as f32;
        Lattice {
            x: self.px(x),
            y: self.px(y),
            step,
            side: ((side * self.s).round() as f32).clamp(1.0, step - 1.0),
        }
    }

    fn core(&mut self, grid: &Lattice, (col, row): (usize, usize), heat: Option<f32>, radius: f32) {
        if !self.draw {
            return;
        }
        let side = grid.side;
        let rect = (
            grid.x + col as f32 * grid.step,
            grid.y + row as f32 * grid.step,
            side,
            side,
        );
        let radius = if side < 4.0 { 0.0 } else { radius };
        match heat {
            Some(v) => self.rect(rect, radius, self.heat(ink::heat_step(v))),
            None => self.hollow_cell(rect, radius, side < 4.0),
        }
    }

    fn cores_block(&mut self, at: Col, top0: f64, words: &DeckWords) {
        let clusters = words.clusters();
        let room = Some((at.x, at.end()));
        let title = plan::cores_block_title(&clusters, self.m);
        let words2 = self.inks.words2;
        self.text(at.x, HEADER_BASE_FULL, title, SMALL_PT, false, words2, room);
        let CoresMode::Block { pitch, sq, per_row } = plan::cores_block_mode(&clusters, self.h_pt)
        else {
            return;
        };
        let per_row = per_row.max(1) as usize;
        let radius = self.s.round().max(1.0) as f32;
        let mut y = top0;
        for (cluster, cc) in clusters.iter().zip(&words.cores) {
            let squares = match plan::cores_block_caption(cluster, self.m) {
                Some(caption) => {
                    let base = y + 8.0;
                    let ink = self.inks.words;
                    let w = self.text(at.x, base, caption.count, SMALL_PT, true, ink, room);
                    self.text(at.x + w, base, caption.name, SMALL_PT, false, words2, room);
                    y + 11.0
                }
                None => y + 1.0,
            };
            let grid = self.lattice(at.x, squares, pitch, sq);
            for (i, &heat) in cc.heat.iter().enumerate() {
                self.core(&grid, (i % per_row, i / per_row), heat, radius);
            }
            let rows = cluster.count.div_ceil(per_row) as f64;
            let across = cluster.count.min(per_row) as f64;
            let bottom = squares + rows * pitch - (pitch - sq);
            self.note(
                BoxKind::Core,
                [at.x, squares, at.x + across * pitch - (pitch - sq), bottom],
                "",
                None,
            );
            y = bottom + 4.0;
        }
    }

    fn cores_row(
        &mut self,
        room: Col,
        top: f64,
        cell: f64,
        mode: CoresMode,
        caption_pt: f64,
        words: &DeckWords,
    ) {
        let clusters = words.clusters();
        let base = top + cell - 1.0;
        let within = Some((room.x, room.end()));
        let (ink, words2) = (self.inks.words, self.inks.words2);
        if mode == CoresMode::CountOnly {
            let text = plan::cores_count_text(&clusters, room.w, caption_pt, self.m);
            self.text(room.x, base, text, caption_pt, false, words2, within);
            return;
        }
        let radius = self.s.round().max(1.0) as f32;
        let mut x = room.x;
        for (cluster, cc) in clusters.iter().zip(&words.cores) {
            x += self.text(
                x,
                base,
                format!("{} ", cluster.count),
                caption_pt,
                true,
                ink,
                within,
            );
            if let Some(name) = cluster.name {
                x += self.text(x, base, name.to_owned(), caption_pt, false, words2, within);
            }
            x += 3.0;
            let n = cc.heat.len();
            let (side, per_col, y0, step, r) = match mode {
                CoresMode::Big => (cell, 1, top, 10.0, radius),
                CoresMode::Tiny => (2.0, 3, top + (cell - 3.0 * 2.0 - 2.0 * 0.5) / 2.0, 2.5, 0.0),
                _ => ((cell - 0.5) / 2.0, 2, top, (cell - 0.5) / 2.0 + 0.5, 0.0),
            };
            let grid = self.lattice(x, y0, step, side);
            for (i, &heat) in cc.heat.iter().enumerate() {
                self.core(&grid, (i / per_col, i % per_col), heat, r);
            }
            let cols = n.div_ceil(per_col) as f64;
            let w = (cols - 1.0).max(0.0) * step + side;
            let bottom = if per_col == 1 {
                y0 + side
            } else {
                y0 + (per_col.min(n) as f64 - 1.0) * step + side
            };
            self.note(BoxKind::Core, [x, y0, x + w, bottom], "", None);
            x += w + 8.0;
        }
    }

    fn swatch(&mut self, x: f64, base: f64, bar: Bar) {
        let (w, h) = (0.62 * SMALL_PT, 0.7 * SMALL_PT);
        self.note(BoxKind::Swatch, [x, base - h, x + w, base], "", None);
        let rect = (
            self.px(x),
            self.px(base) - self.px(h),
            self.px(w),
            self.px(h),
        );
        let ink = self.scope_ink(bar);
        self.rect(rect, self.s.max(1.0) as f32, ink);
    }

    fn header(&mut self, head: &HeaderPlan, base: f64, small: bool, states: &[HeadState<'_>]) {
        #[cfg(test)]
        {
            self.head = Some(head.clone());
        }
        let words2 = self.inks.words2;
        let sep = " \u{b7} ";
        for label in &head.labels {
            match label.label {
                Label::Time => {
                    self.text(
                        label.x,
                        base,
                        label.text.clone(),
                        SMALL_PT,
                        false,
                        words2,
                        None,
                    );
                }
                Label::Now => {
                    let w = self.m.width(&label.text, SMALL_PT, false);
                    self.text(
                        label.x - w,
                        base,
                        label.text.clone(),
                        SMALL_PT,
                        false,
                        words2,
                        None,
                    );
                }
                Label::Scope(bar) => {
                    self.swatch(label.x, base, bar);
                    let x = label.x + 0.62 * SMALL_PT + 3.0;
                    self.two_inks(x, base, &label.text, sep);
                }
                Label::Details => self.two_inks(label.x, base, &label.text, sep),
                Label::State => {
                    let Some(st) = states.iter().find(|st| (st.x - label.x).abs() < 0.01) else {
                        continue;
                    };
                    let ink = self.ink(st.st.ink);
                    let mark = Some((st.st.mark, self.mark_ink(st.st.mark, st.st.ink)));
                    let w = self.marked(
                        label.x,
                        base,
                        mark,
                        label.text.clone(),
                        SMALL_PT,
                        true,
                        ink,
                        None,
                    );
                    let rect = self.px_rect(label.x, 0.0, label.x + w, HEADER_HIT_FOLDED);
                    self.hits_first
                        .push(Hit::row(rect, Instrument::of(st.res), Bar::Global));
                }
            }
        }
        for &(item, x) in &head.key.placed {
            self.key(item, x, base, small);
        }
    }

    fn two_inks(&mut self, x: f64, base: f64, text: &str, sep: &str) {
        let (head, tail) = text
            .split_once(sep)
            .map_or((text, None), |(a, b)| (a, Some(b)));
        let ink = self.inks.words;
        let w = self.text(x, base, head.to_owned(), SMALL_PT, false, ink, None);
        if let Some(tail) = tail {
            let ink = self.inks.words2;
            self.text(
                x + w,
                base,
                format!("{sep}{tail}"),
                SMALL_PT,
                false,
                ink,
                None,
            );
        }
    }

    fn key(&mut self, item: KeyItem, x: f64, base: f64, small: bool) {
        let words2 = self.inks.words2;
        let top = base - 7.0;
        match item {
            KeyItem::Ramp => {
                let w = self.text(x, base, "cold".into(), SMALL_PT, false, words2, None);
                let xx = x + w + 3.0;
                self.note(BoxKind::Swatch, [xx, top, xx + 34.0, base], "", None);
                let radius = (0.5 * self.s).round().max(1.0) as f32;
                for i in 0..7_u8 {
                    let rect = (
                        self.px(xx + f64::from(i) * 5.0),
                        self.px(top),
                        self.px(4.0),
                        self.px(7.0),
                    );
                    let ink = self.heat(i);
                    self.rect(rect, radius, ink);
                }
                self.text(
                    xx + 35.0 + 3.0,
                    base,
                    "hot".into(),
                    SMALL_PT,
                    false,
                    words2,
                    None,
                );
            }
            KeyItem::Over => {
                self.note(BoxKind::Swatch, [x, top, x + 4.0, base], "", None);
                let (cx, cy, cw, bottom) = (self.px(x), self.px(top), self.px(4.0), self.px(base));
                let (over, hot) = (self.inks.over, self.heat(6));
                match self.cap_px() {
                    Some((ch, gap)) => {
                        self.rect((cx, cy, cw, ch), 0.0, over);
                        let y = cy + ch + gap;
                        self.rect((cx, y, cw, bottom - y), 0.0, hot);
                    }
                    None => {
                        self.rect((cx, cy, cw, bottom - cy), 0.0, hot);
                        self.rect((cx, cy, cw, 2.0), 0.0, over);
                    }
                }
                self.text(
                    x + 7.0,
                    base,
                    "over max".into(),
                    SMALL_PT,
                    false,
                    words2,
                    None,
                );
            }
            KeyItem::NotRead => {
                self.note(BoxKind::Swatch, [x, top, x + 4.0, base], "", None);
                let rect = (
                    self.px(x),
                    self.px(top),
                    self.px(4.0),
                    self.px(base) - self.px(top),
                );
                self.hollow_cell(rect, if self.s < 2.0 { 0.0 } else { 1.0 }, small);
                self.text(
                    x + 7.0,
                    base,
                    "not read".into(),
                    SMALL_PT,
                    false,
                    words2,
                    None,
                );
            }
        }
    }

    fn full(&mut self, words: &DeckWords) {
        let plan = plan::plan_full(self.w_pt, self.m);
        self.ground();
        if plan.ground_only {
            return;
        }
        let t = Table::of_full(&plan);
        let rows = plan.rows;
        let gap = rows.pitch - rows.cell;
        let holes: Vec<(f64, f64)> = words
            .rows
            .iter()
            .zip(0_u32..)
            .filter(|(row, _)| row.strip_text.is_some())
            .map(|(_, r)| (rows.top(r) - gap / 2.0, rows.top(r) + rows.cell + gap / 2.0))
            .collect();
        let last = rows.top(DECK_RESOURCES.len() as u32 - 1) + rows.cell;
        self.gridlines(&plan.strip, GRID_FROM_FULL, last, &holes);
        match plan.cores {
            CoresAt::Block { at, divider } => {
                self.cores_block(at, rows.top0, words);
                let w = (0.5 * self.s).round().max(1.0) as f32;
                let (y0, y1) = (self.px(4.0), self.px(self.h_pt - 6.0));
                let grid = self.inks.grid;
                self.rect((self.px(divider), y0, w, y1 - y0), 0.0, grid);
                if !words.reading {
                    let rect = self.px_rect(at.x, 0.0, at.end(), self.h_pt - 6.0);
                    self.hits_first.push(Hit::cores(rect));
                }
            }
            CoresAt::Row(room) => {
                let r = DECK_RESOURCES.len() as u32;
                let top = rows.top(r);
                let ink = self.inks.words;
                self.text(
                    plan.name.x,
                    top + rows.baseline,
                    "Cores".into(),
                    rows.name_pt,
                    true,
                    ink,
                    Some((plan.name.x, plan.name.end())),
                );
                let mode = plan.cores_mode(&words.clusters(), self.h_pt, self.m);
                self.cores_row(room, top, rows.cell, mode, SMALL_PT, words);
                if !words.reading {
                    let (y0, y1) = t.band(r);
                    let rect = self.px_rect(plan.name.x, y0, room.end(), y1);
                    self.hits_first.push(Hit::cores(rect));
                }
            }
        }
        for (row, r) in words.rows.iter().zip(0_u32..) {
            self.row(&t, r, row, words.reading);
        }
        let text = HeaderText {
            tab: self.tab,
            machine: self.machine,
        };
        let head = plan::header_full(&plan, text, self.m);
        let small = self.small_cells(&plan.strip, rows.cell);
        self.header(&head, HEADER_BASE_FULL, small, &[]);
        if !words.reading {
            let rect = self.px_rect(0.0, 0.0, self.w_pt, HEADER_HIT_FULL);
            self.hits_last.push(Hit::header(rect));
        }
    }

    fn folded(&mut self, words: &DeckWords) {
        let plan = plan::plan_folded(self.w_pt, self.m);
        self.ground();
        if plan.ground_only {
            return;
        }
        let w = (0.5 * self.s).round().max(1.0) as f32;
        let (y0, y1) = (self.px(4.0), self.px(self.h_pt - 6.0));
        let grid = self.inks.grid;
        self.rect((self.px(plan.divider), y0, w, y1 - y0), 0.0, grid);
        let clusters = words.clusters();
        let mut states: Vec<HeadState<'_>> = Vec::new();
        for (bi, block) in plan.blocks.iter().enumerate() {
            let t = Table::of_block(block);
            let rows = block.rows;
            let ours: Vec<&RowWords> = words.rows.iter().skip(bi * 4).take(4).collect();
            let gap = 1.5;
            let holes: Vec<(f64, f64)> = ours
                .iter()
                .zip(0_u32..)
                .filter(|(row, _)| row.strip_text.is_some())
                .map(|(_, r)| (rows.top(r) - gap, rows.top(r) + rows.cell + gap))
                .collect();
            let last = rows.top(ours.len().saturating_sub(1) as u32) + rows.cell;
            self.gridlines(&block.strip, GRID_FROM_FOLDED, last, &holes);
            for (row, r) in ours.iter().zip(0_u32..) {
                self.row(&t, r, row, words.reading);
            }
            if let Some(st) = block_state(&ours, block.name.x) {
                states.push(st);
            }
            if bi == 1 {
                let r = 3;
                let top = rows.top(r);
                if block.kind != BlockKind::Slim {
                    let ink = self.inks.words;
                    self.text(
                        block.name.x,
                        top + rows.baseline,
                        "Cores".into(),
                        rows.name_pt,
                        true,
                        ink,
                        Some((block.name.x, block.name.end())),
                    );
                }
                let mode = block.cores_mode(&clusters, self.m);
                let caption = block.kind.cores_caption_pt();
                self.cores_row(block.cores, top, rows.cell, mode, caption, words);
                if !words.reading {
                    let (y0, y1) = t.band(r);
                    let rect = self.px_rect(block.name.x, y0, block.at.end(), y1);
                    self.hits_first.push(Hit::cores(rect));
                }
            }
        }
        let text = HeaderText {
            tab: self.tab,
            machine: self.machine,
        };
        let labels: [Option<StateLabel<'_>>; 2] = [0, 1].map(|bi| {
            let x = plan.blocks.get(bi).map(|b| b.name.x)?;
            states
                .iter()
                .find(|st| (st.x - x).abs() < 0.01)
                .map(|st| StateLabel {
                    full: &st.full,
                    bare: &st.st.label,
                })
        });
        let head = plan::header_folded(&plan, text, labels, self.m);
        let small = plan
            .blocks
            .first()
            .is_some_and(|b| self.small_cells(&b.strip, b.rows.cell));
        let states = if words.reading { Vec::new() } else { states };
        self.header(&head, HEADER_BASE_FOLDED, small, &states);
        if !words.reading {
            let rect = self.px_rect(0.0, 0.0, self.w_pt, HEADER_HIT_FOLDED);
            self.hits_last.push(Hit::header(rect));
        }
    }

    fn paint(&mut self, words: &DeckWords, form: DeckForm) {
        match form {
            DeckForm::Full => self.full(words),
            DeckForm::Folded => self.folded(words),
        }
    }

    fn hits(&mut self) -> Vec<Hit> {
        let mut hits = std::mem::take(&mut self.hits_first);
        hits.append(&mut self.hits_rows);
        hits.append(&mut self.hits_last);
        hits
    }
}

fn block_state<'w>(rows: &[&'w RowWords], x: f64) -> Option<HeadState<'w>> {
    let with: Vec<(&RowWords, &StateTxt)> = rows
        .iter()
        .filter_map(|row| row.state.as_ref().map(|st| (*row, st)))
        .collect();
    let rank = |st: &StateTxt| (st.state, st.mark == WellMark::Low);
    let (row, st) = with.iter().copied().reduce(|best, next| {
        if rank(next.1) > rank(best.1) {
            next
        } else {
            best
        }
    })?;
    let more = with.len() - 1;
    Some(HeadState {
        x,
        res: row.res,
        st,
        full: if more > 0 {
            format!("{} +{more}", st.label)
        } else {
            st.label.clone()
        },
    })
}

/// Lay the Deck out and paint it.
pub(crate) fn deck_paint(g: &DeckGeom, input: &DeckInput<'_>) -> (Vec<DrawPrim>, Vec<Hit>) {
    let m = deck_metrics();
    let words = DeckWords::new(input, &m);
    let mut deck = Deck::new(g, input, &m, true);
    deck.paint(&words, input.form);
    let hits = deck.hits();
    (deck.p.prims, hits)
}

/// A laid-out deck as the tests read it.
#[cfg(test)]
pub(crate) struct Laid {
    pub(crate) prims: Vec<DrawPrim>,
    pub(crate) hits: Vec<Hit>,
    pub(crate) boxes: Vec<Boxed>,
    /// Strip cells drawn.
    pub(crate) cells: usize,
    /// The header line as planned.
    pub(crate) head: Option<HeaderPlan>,
}

/// [`deck_paint`] on words already read and a face's metrics, recording every box.
#[cfg(test)]
pub(crate) fn lay(
    g: &DeckGeom,
    input: &DeckInput<'_>,
    words: &DeckWords,
    m: &FaceMetrics,
    draw: bool,
) -> Laid {
    let mut deck = Deck::new(g, input, m, draw);
    deck.boxes.record();
    deck.paint(words, input.form);
    let hits = deck.hits();
    Laid {
        boxes: deck.boxes.take(),
        head: deck.head.take(),
        cells: deck.cells,
        prims: deck.p.prims,
        hits,
    }
}

/// The Deck's prims alone.
#[cfg(test)]
pub(crate) fn deck_prims(g: &DeckGeom, input: &DeckInput<'_>) -> Vec<DrawPrim> {
    deck_paint(g, input).0
}

/// A fingerprint of the prims, in order, by their fields ([`DrawPrim::fingerprint_into`]), so an identical layout presents nothing; never `0`.
pub(crate) fn prims_fp(prims: &[DrawPrim]) -> u64 {
    use std::hash::Hasher;
    let mut state = std::collections::hash_map::DefaultHasher::new();
    for p in prims {
        p.fingerprint_into(&mut state);
    }
    state.finish() | 1
}

/// The stats card's inputs for a window, read off the App.
pub(crate) struct DeckFrame {
    pub(crate) form: StatsBars,
    /// The sill's plate alpha ([`DeckInput::plate`]).
    pub(crate) plate: Option<u8>,
    pub(crate) geom: DeckGeom,
    pub(crate) dy: u32,
    /// [`DeckInput::tab_panes`].
    pub(crate) tab_panes: Vec<u64>,
    pub(crate) tab_label: String,
    pub(crate) tab_count: usize,
    pub(crate) inks: VitalsInks,
    /// The lens's inks: always solved on the band ground.
    pub(crate) lens_inks: VitalsInks,
    pub(crate) dwc: bool,
    /// The window's Deck form (§2).
    pub(crate) deck: DeckForm,
    /// Increase Contrast.
    pub(crate) contrast: bool,
}

impl crate::App {
    /// The window's frame height in device px as the presenters compose it: the head, the
    /// top pad, the rows and the strip, the bottom pad.
    pub(crate) fn stats_frame_h(&self, wid: crate::WindowId) -> usize {
        let Some(ws) = self.windows.get(&wid) else {
            return 0;
        };
        let (_, ch) = self.win_cell_size(wid);
        let composed_rows = usize::from(ws.rows).saturating_add(usize::from(self.chrome_rows(wid)));
        composed_rows
            .saturating_mul(ch)
            .saturating_add(self.win_head(wid))
            .saturating_add(self.win_pad_top(wid))
            .saturating_add(self.win_pad(wid))
    }

    pub(crate) fn deck_frame(&self, wid: crate::WindowId) -> Option<DeckFrame> {
        let form = self.stats_bars_form();
        if form == StatsBars::Off {
            return None;
        }
        let ws = self.windows.get(&wid)?;
        if !stats_drawn_on(ws) {
            return None;
        }
        if ws.cols == 0 {
            return None;
        }
        let (cw, _) = self.win_cell_size(wid);
        let pad = self.win_pad(wid);
        let w = ws.cols as usize * cw + 2 * pad;
        let (tab_key, tab_label) = self.deck_tab(wid);
        let tab_panes = self.deck_tab_panes(wid);
        let tab_count = ws.tab_set.tabs().len();
        // A deck scene's front tab is its window's, the one its readings name (its panes come
        // from `deck_tab_panes`, which the lens's sentence reads too).
        #[cfg(debug_assertions)]
        let (tab_label, tab_count) = match self.vitals.deck_scene {
            Some(scene) => {
                let (_, label, count) = deck_fixtures::DeckContext::of(scene).front_tab();
                (label, count)
            }
            None => (tab_label, tab_count),
        };
        let prefs = crate::native_appearance::current_preferences();
        let (dwc, contrast) = (prefs.differentiate_without_color, prefs.high_contrast);
        let palette = self.chrome_palette(crate::app_render::ChromeSurface::Header);
        let band = crate::chrome_band::band_colors(palette);
        let band_inks = cached_inks(band.bar_bg, band.value, band.accent);
        match form {
            StatsBars::Off => None,
            StatsBars::Deck => {
                let rows = self.deck_rows_px(wid)?;
                let (dy, deck) = (rows.start, rows.len());
                Some(DeckFrame {
                    form,
                    plate: None,
                    geom: DeckGeom {
                        w: w as f32,
                        h: deck as f32,
                        pad: pad as f32,
                        scale: ws.scale as f32,
                        font_px: self.win_font_px(wid),
                    },
                    dy: u32::try_from(dy).unwrap_or(0),
                    tab_panes,
                    tab_label,
                    tab_count,
                    inks: band_inks,
                    lens_inks: band_inks,
                    dwc,
                    deck: ws.deck_form,
                    contrast,
                })
            }
            StatsBars::Sill => {
                let sill = crate::stats_sill::sill_px(pad, ws.scale);
                if sill == 0 {
                    return None;
                }
                let frame_h = self.stats_frame_h(wid);
                let (floor, value, accent) = self.sill_floor(tab_key);
                let translucent = self.config.background_opacity_or_default() < 1.0
                    || self.config.wallpaper.is_some();
                let plate = translucent.then_some(if prefs.reduced_transparency {
                    0xFF
                } else {
                    235
                });
                Some(DeckFrame {
                    form,
                    plate,
                    geom: DeckGeom {
                        w: w as f32,
                        h: sill as f32,
                        pad: pad as f32,
                        scale: ws.scale as f32,
                        font_px: self.win_font_px(wid),
                    },
                    dy: u32::try_from(frame_h.saturating_sub(pad)).unwrap_or(0),
                    tab_panes,
                    tab_label,
                    tab_count,
                    inks: cached_inks(floor, value, accent),
                    lens_inks: band_inks,
                    dwc,
                    deck: ws.deck_form,
                    contrast,
                })
            }
        }
    }

    fn sill_floor(&self, tab_key: Option<u64>) -> (Rgb, Rgb, Rgb) {
        let rgb = |c: u32| [(c >> 16) as u8, (c >> 8) as u8, c as u8];
        let theme = self.theme;
        match tab_key.and_then(|sid| self.pool.own_colors(sid)) {
            Some(own) => {
                let c = |v: aterm_types::Rgb| [v.r, v.g, v.b];
                (
                    c(own.default_background),
                    c(own.default_foreground),
                    own.cursor_color.map_or(rgb(theme.cursor), c),
                )
            }
            None => (rgb(theme.bg), rgb(theme.fg), rgb(theme.cursor)),
        }
    }

    /// The instance's attention marks for the bars, worst first, from the stuck engine's view
    /// on the snapshot.
    pub(crate) fn stats_marks(&self) -> Vec<StatsMark> {
        self.vitals
            .snapshot
            .as_ref()
            .and_then(|s| s.jobs.as_deref())
            .map(crate::stats_paint::marks_of)
            .unwrap_or_default()
    }

    /// The pointer in the stats card's own pixels, while it is over the card.
    pub(crate) fn stats_card_point(&self, wid: crate::WindowId) -> Option<(f64, f64)> {
        let f = self.deck_frame(wid)?;
        let (x, y) = self.windows.get(&wid).map(|ws| ws.last_cursor_px)?;
        let (ox, oy) = self.frame_origin(wid);
        let (fx, fy) = (x - ox as f64, y - oy as f64);
        let top = f64::from(f.dy);
        ((top..top + f64::from(f.geom.h)).contains(&fy) && (0.0..f64::from(f.geom.w)).contains(&fx))
            .then_some((fx, fy - top))
    }

    /// The instrument under the pointer on the stats card, from the hit regions its last
    /// layout recorded.
    pub(crate) fn stats_hit_under_pointer(&self, wid: crate::WindowId) -> Option<Hit> {
        let (x, y) = self.stats_card_point(wid)?;
        let _ = self.stats_deck_fp(wid);
        let ws = self.windows.get(&wid)?;
        let hits = ws.stats_hits.borrow();
        hits.iter()
            .find(|h| crate::stats_paint::contains(h.rect, x, y))
            .copied()
    }

    fn deck_input_key(&self, f: &DeckFrame) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.vitals.snapshot.as_ref().map(|s| s.seq).hash(&mut h);
        f.form.hash(&mut h);
        for v in [f.geom.w, f.geom.h, f.geom.pad, f.geom.scale, f.geom.font_px] {
            v.to_bits().hash(&mut h);
        }
        f.dy.hash(&mut h);
        f.tab_panes.hash(&mut h);
        f.tab_label.hash(&mut h);
        f.tab_count.hash(&mut h);
        f.dwc.hash(&mut h);
        f.plate.hash(&mut h);
        f.deck.hash(&mut h);
        f.contrast.hash(&mut h);
        f.inks.hash(&mut h);
        Arc::as_ptr(&deck_metrics()).hash(&mut h);
        h.finish() | 1
    }

    fn deck_layout(&self, wid: crate::WindowId, f: &DeckFrame, key: u64) -> Vec<DrawPrim> {
        let input = DeckInput {
            snap: self.vitals.snapshot.as_deref(),
            tab_panes: f.tab_panes.clone(),
            tab_label: f.tab_label.clone(),
            tab_count: f.tab_count,
            inks: f.inks,
            dwc: f.dwc,
            marks: self.stats_marks(),
            plate: f.plate,
            form: f.deck,
            contrast: f.contrast,
            locale: Locale {
                platform: Platform::host(),
                utc_offset_s: crate::presence::local_offset_s().unwrap_or(0),
            },
        };
        let (prims, hits) = match f.form {
            StatsBars::Sill => crate::stats_sill::sill_paint(&f.geom, &input),
            _ => deck_paint(&f.geom, &input),
        };
        if let Some(ws) = self.windows.get(&wid) {
            ws.deck_memo
                .set((key, prims_fp(&prims) ^ u64::from(f.dy).rotate_left(40)));
            *ws.stats_hits.borrow_mut() = hits;
        }
        prims
    }

    /// The stats card's fingerprint for `wid` (`0` when it shows none), the `RepaintKey`
    /// term.
    pub(crate) fn stats_deck_fp(&self, wid: crate::WindowId) -> u64 {
        let lens = self.stats_lens_fp(wid);
        let Some(f) = self.deck_frame(wid) else {
            return lens;
        };
        let key = self.deck_input_key(&f);
        if let Some((k, fp)) = self.windows.get(&wid).map(|ws| ws.deck_memo.get())
            && k == key
        {
            return fp ^ lens;
        }
        let _ = self.deck_layout(wid, &f, key);
        self.windows.get(&wid).map_or(0, |ws| ws.deck_memo.get().1) ^ lens
    }

    /// Rasterize the stats card into `wid`'s paint-only `deck_card`, again only when its prims
    /// or placement moved, and the lens into its own card.
    pub(crate) fn splice_stats_deck(&mut self, wid: crate::WindowId) {
        self.splice_stats_lens(wid);
        let Some(f) = self.deck_frame(wid) else {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.deck_card = None;
                ws.deck_fold = None;
                ws.stats_hits.borrow_mut().clear();
            }
            return;
        };
        let key = self.deck_input_key(&f);
        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.deck_card.as_ref())
            .is_some_and(|c| c.geom == key)
        {
            return;
        }
        let prims = self.deck_layout(wid, &f, key);
        let fp = self.windows.get(&wid).map_or(0, |ws| ws.deck_memo.get().1);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if let Some(card) = ws.deck_card.as_mut()
            && card.fp == fp
        {
            card.geom = key;
            return;
        }
        let (rgba, pw, ph) = crate::tray_raster::rasterize_tray(
            &prims,
            f.geom.w as u32,
            f.geom.h as u32,
            1.0,
            [0, 0, 0, 0],
        );
        ws.deck_card = Some(crate::SettingsCard {
            scrim: None,
            rgba,
            pw,
            ph,
            dx: 0,
            dy: f.dy,
            fp,
            geom: key,
            raster_fingerprint_once: std::sync::OnceLock::new(),
        });
    }

    /// Fold the stats card and the open lens under the card the window would otherwise
    /// present (the backends take one tray surface), cached by the cards' identities.
    pub(crate) fn fold_stats_deck(&mut self, wid: crate::WindowId) {
        use std::hash::{Hash, Hasher};
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let top = ws
            .route_card
            .as_ref()
            .or(ws.settings_card.as_ref())
            .or(ws.badge_card.as_ref());
        if top.is_some_and(|t| t.scrim.is_some()) {
            ws.deck_fold = None;
            return;
        }
        let cards: Vec<&crate::SettingsCard> = [ws.deck_card.as_ref(), ws.lens_card.as_ref(), top]
            .into_iter()
            .flatten()
            .collect();
        if ws.lens_card.is_none() && (cards.len() < 2 || ws.deck_card.is_none()) {
            ws.deck_fold = None;
            return;
        }
        let mut identity = std::collections::hash_map::DefaultHasher::new();
        for card in &cards {
            card.fp.hash(&mut identity);
            card.geom.hash(&mut identity);
            card.dx.hash(&mut identity);
            card.dy.hash(&mut identity);
            card.pw.hash(&mut identity);
            card.ph.hash(&mut identity);
        }
        let fp = identity.finish() | 1;
        if ws.deck_fold.as_ref().is_some_and(|c| c.fp == fp) {
            return;
        }
        ws.deck_fold = fold_cards(&cards, fp);
    }
}

/// `cards` composited in order onto one surface over their union, the fold the backends present
/// ([`crate::App::fold_stats_deck`]), fingerprinted `fp`; `None` when the union's bytes do not
/// fit a `usize` or a card does not composite.
pub(crate) fn fold_cards(cards: &[&crate::SettingsCard], fp: u64) -> Option<crate::SettingsCard> {
    let x0 = cards.iter().map(|c| c.dx).min().unwrap_or(0);
    let y0 = cards.iter().map(|c| c.dy).min().unwrap_or(0);
    let x1 = cards.iter().map(|c| c.dx + c.pw).max().unwrap_or(0);
    let y1 = cards.iter().map(|c| c.dy + c.ph).max().unwrap_or(0);
    let (pw, ph) = (x1 - x0, y1 - y0);
    let len = usize::try_from(u64::from(pw) * u64::from(ph) * 4).ok()?;
    let mut rgba = vec![0; len];
    let ok = cards.iter().all(|card| {
        crate::tray_raster::composite_rgba_surface(
            &mut rgba,
            (pw, ph),
            (x0, y0),
            &card.rgba,
            (card.pw, card.ph),
            (card.dx, card.dy),
        )
    });
    ok.then_some(crate::SettingsCard {
        scrim: None,
        rgba,
        pw,
        ph,
        dx: x0,
        dy: y0,
        fp,
        geom: fp,
        raster_fingerprint_once: std::sync::OnceLock::new(),
    })
}

/// The deck's accessibility group's name (Stats Deck v2 §12).
#[cfg(a11y_tree)]
const A11Y_TITLE: &str = "Stats deck, last 10 minutes";

/// A share (1 is 100 %) as a meter's value: a whole percent, as the % column
/// prints it, never past 100 (the over mark and the words carry the
/// excess); `None` when unknown.
#[cfg(a11y_tree)]
fn meter_pct(share: Option<f32>) -> Option<f32> {
    share.filter(|v| v.is_finite()).map(|v| {
        let pct = v * 100.0;
        if pct > 0.0 && pct < 0.5 {
            ((pct * 10.0).round() / 10.0).max(0.1)
        } else {
            pct.round().clamp(0.0, 100.0)
        }
    })
}

/// The deck as a reader meets it.
#[cfg(a11y_tree)]
pub(crate) fn stats_a11y_group(
    snap: Option<&Snapshot>,
    panes: &[u64],
    loc: &Locale,
) -> crate::a11y_tree::StatsGroup {
    use crate::a11y_tree::{StatsGroup, StatsMeter};
    let empty;
    let snap = match snap {
        Some(snap) => snap,
        None => {
            empty = Snapshot::default();
            &empty
        }
    };
    let meters = DECK_RESOURCES
        .iter()
        .map(|&res| StatsMeter {
            name: words::resource_name(res).into(),
            value: meter_pct(
                (snap.seq > 0)
                    .then(|| snap.resources[res.index()].heat_now)
                    .flatten(),
            ),
            words: words::row_sentence(res, snap, panes, loc),
        })
        .collect();
    let cores: Vec<StatsMeter> = snap.cpu.as_ref().map_or_else(Vec::new, |cpu| {
        cpu.clusters
            .iter()
            .flat_map(|cluster| {
                cluster.cores.iter().enumerate().map(|(k, &core)| {
                    let value = meter_pct(cpu.cores.get(core).map(|c| c.load.level));
                    StatsMeter {
                        name: format!("{} core {}", cluster.name, k + 1),
                        value,
                        words: value.map_or_else(
                            || "not read".to_owned(),
                            |p| format!("{p:.0} percent busy"),
                        ),
                    }
                })
            })
            .collect()
    });
    let n = cores.len();
    StatsGroup {
        name: A11Y_TITLE,
        meters,
        cores_name: format!("{n} CPU {}", if n == 1 { "core" } else { "cores" }),
        cores,
    }
}

impl crate::App {
    /// The stats deck for `wid`'s accessibility tree, while it shows one.
    #[cfg(a11y_tree)]
    pub(crate) fn stats_a11y(&self, wid: crate::WindowId) -> Option<crate::a11y_tree::GridStats> {
        let f = self.deck_frame(wid)?;
        let loc = Locale {
            platform: Platform::host(),
            utc_offset_s: crate::presence::local_offset_s().unwrap_or(0),
        };
        let (ox, oy) = self.frame_origin(wid);
        let bounds = aterm_a11y::model::Rect {
            x0: ox as f64,
            y0: oy as f64 + f64::from(f.dy),
            x1: ox as f64 + f64::from(f.geom.w),
            y1: oy as f64 + f64::from(f.dy) + f64::from(f.geom.h),
        };
        let lens = self
            .stats_lens_words(wid)
            .map(|(lg, lens, laid)| crate::a11y_tree::StatsLens {
                keys: lens.how != crate::stats_lens::Opened::Hover,
                name: laid.title,
                explainer: if lens.focus == Instrument::HowToRead {
                    crate::stats_lens::how_to_read_words(loc.platform)
                } else {
                    laid.explainer
                },
                wells: laid.wells,
                bounds: Some(aterm_a11y::model::Rect {
                    x0: ox as f64 + f64::from(lg.dx),
                    y0: oy as f64 + f64::from(lg.dy),
                    x1: ox as f64 + f64::from(lg.dx) + f64::from(lg.w),
                    y1: oy as f64 + f64::from(lg.dy) + f64::from(lg.h),
                }),
            });
        Some(crate::a11y_tree::GridStats {
            group: stats_a11y_group(self.vitals.snapshot.as_deref(), &f.tab_panes, &loc),
            bounds: Some(bounds),
            lens,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use aterm_vitals::model::{
        ClusterSpec, GpuSample, MemSample, Pressure, ProcSample, Sample, TabRoot, Ticks,
        VolumeSample,
    };

    /// Gives this test thread's windows the stats bars, as glass gives a
    /// real window ([`stats_drawn_on`]), until the guard drops.
    #[must_use]
    pub(crate) fn attach_stats() -> Attached {
        TESTS_ATTACHED.with(|c| c.set(true));
        Attached
    }

    /// [`attach_stats`]'s guard: dropping it takes the bars away again.
    pub(crate) struct Attached;

    impl Drop for Attached {
        fn drop(&mut self) {
            TESTS_ATTACHED.with(|c| c.set(false));
        }
    }

    #[test]
    fn only_a_change_into_or_out_of_the_deck_moves_the_head() {
        const CELL: usize = 28;
        let head = |bars, deck| chrome_head_px(28.0, 2.0, bars, deck, CELL);
        for deck in [DeckForm::Folded, DeckForm::Full] {
            for a in StatsBars::ALL {
                for b in StatsBars::ALL {
                    let moved = head(a, deck) != head(b, deck);
                    assert_eq!(
                        a.takes_room() != b.takes_room(),
                        moved,
                        "{a:?} to {b:?}, {deck:?}"
                    );
                }
            }
        }
        for bars in StatsBars::ALL {
            let unfold = head(bars, DeckForm::Full) - head(bars, DeckForm::Folded);
            assert_eq!(unfold != 0, bars.takes_room(), "{bars:?}");
            assert_eq!(unfold % CELL, 0, "{bars:?}");
        }
        assert_eq!(
            head(StatsBars::Sill, DeckForm::Folded),
            head(StatsBars::Off, DeckForm::Full)
        );
    }

    #[test]
    fn head_sites_agree() {
        use aterm_window::dpi::PhysicalSize;
        let _attached = attach_stats();
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let band_pts = 28.0;
        let mut cells = std::collections::BTreeSet::new();
        for scale in [1.0, 2.0] {
            let band = band_px(band_pts, scale);
            for font_px in [8.0, 11.0, 14.0, 18.0, 23.0, 28.0, 35.0] {
                {
                    let ws = app.windows.get_mut(&wid).expect("the window");
                    ws.scale = scale;
                    ws.head_pts = band_pts;
                    ws.metrics.font_px = font_px;
                }
                let (_, ch) = app.win_cell_size(wid);
                cells.insert(ch);
                let at = format!("{ch} px cells at {scale}×");
                let mut heads = Vec::new();
                for (avail, want) in [(700.0, DeckForm::Folded), (900.0, DeckForm::Full)] {
                    let frame = band + (FOLDED_PT * scale) as usize + (avail * scale) as usize;
                    let head = app.deck_head_px(wid, band_pts, scale, frame as u32);
                    let ws = app.windows.get_mut(&wid).expect("the window");
                    assert_eq!(ws.deck_form, want, "{at}");
                    ws.metrics.head = head;
                    app.backend.set_head(head);
                    let f = app.deck_frame(wid).expect("the Deck shows");
                    let (above, below) = if crate::tab_bar::STRIP_IS_CHROME_BAND {
                        (0, band)
                    } else {
                        (band, 0)
                    };
                    assert_eq!(f.dy as usize, above, "{at}");
                    assert_eq!(f.geom.h as usize + band, head, "{at}");
                    assert_eq!(
                        f.geom.h as usize,
                        deck_px(StatsBars::Deck, want, scale, ch),
                        "{at}"
                    );
                    assert_eq!(
                        f.dy as usize + f.geom.h as usize + below,
                        app.win_head(wid),
                        "the grid starts under the card and the band, {at}"
                    );
                    if crate::tab_bar::STRIP_IS_CHROME_BAND {
                        assert_eq!(
                            app.strip_band_top_px(wid),
                            app.win_pad_top(wid) + band,
                            "the band starts where the card ends, {at}"
                        );
                    }
                    heads.push(head);
                }
                let (folded, full) = (heads[0], heads[1]);
                assert_eq!((full - folded) % ch, 0, "{at}");
                let lost = (full - folded) / ch;
                let fit = |app: &mut crate::App, head: usize, h: usize| {
                    app.windows.get_mut(&wid).expect("the window").metrics.head = head;
                    app.backend.set_head(head);
                    let size = PhysicalSize::new(800, h as u32);
                    let (rows, _) = app.grid_dims_for(wid, size);
                    (rows, app.whole_cell_min_size(wid).height)
                };
                let low = full + 8 * ch;
                for h in low..low + 3 * ch {
                    let (folded_rows, folded_min) = fit(&mut app, folded, h);
                    let (full_rows, full_min) = fit(&mut app, full, h);
                    assert_eq!(
                        usize::from(folded_rows - full_rows),
                        lost,
                        "a {h} px frame, {at}"
                    );
                    assert_eq!((full_min - folded_min) % ch as u32, 0, "{at}");
                }
            }
        }
        assert!(
            cells.first().is_some_and(|c| *c <= 12) && cells.last().is_some_and(|c| *c >= 40),
            "the cells span 12 to 40 px: {cells:?}"
        );
    }

    #[test]
    fn the_deck_size_setting_forces_the_form() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let form = |app: &crate::App| app.windows[&wid].deck_form;
        let tall = 56 + 124 + 1800;
        let short = 56 + 124 + 1400;
        for (word, frame, want) in [
            ("folded", tall, DeckForm::Folded),
            ("full", short, DeckForm::Full),
            ("auto", short, DeckForm::Folded),
            ("auto", tall, DeckForm::Full),
            ("auto", 0, DeckForm::Full),
        ] {
            app.config
                .sys
                .get_or_insert_with(Default::default)
                .stats_deck_size = Some(word.into());
            let head = app.deck_head_px(wid, 28.0, 2.0, frame);
            assert_eq!(form(&app), want, "{word} at {frame} px");
            let (_, ch) = app.win_cell_size(wid);
            assert_eq!(
                head,
                56 + deck_px(StatsBars::Deck, want, 2.0, ch),
                "{word} at {frame} px"
            );
        }
    }

    #[test]
    fn a_restored_window_keeps_its_form_and_size() {
        use aterm_window::dpi::PhysicalSize;
        let _attached = attach_stats();
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let (band_pts, scale) = (28.0, 2.0);
        {
            let ws = app.windows.get_mut(&wid).expect("the window");
            ws.scale = scale;
            ws.head_pts = band_pts;
        }
        let (_, cell) = app.win_cell_size(wid);
        let cols = 80;
        let form = |app: &crate::App| app.windows[&wid].deck_form;
        let frame_in = |app: &mut crate::App, deck: DeckForm, rows: u16| {
            app.windows.get_mut(&wid).expect("the window").deck_form = deck;
            let head = app.deck_head_px(wid, band_pts, scale, 0);
            app.backend.set_head(head);
            let frame = app.window_frame_px_for(wid, rows, cols);
            let avail = crate::stats_plan::avail_pt(
                f64::from(frame.height),
                band_px(band_pts, scale) as f64,
                scale,
            );
            (frame, avail)
        };
        let attach = |app: &mut crate::App, rows: u16, carried: Option<PhysicalSize<u32>>| {
            app.windows.get_mut(&wid).expect("the window").deck_form = DeckForm::Folded;
            let head = app.attach_deck_head(wid, band_pts, scale, (rows, cols), carried);
            app.windows.get_mut(&wid).expect("the window").metrics.head = head;
            app.attach_frame_px(wid, rows, cols, carried)
        };
        let in_band = |avail: f64| (850.0..870.0).contains(&avail);
        let rem = (cell / 2) as u32;
        let mut checked = [false; 2];
        for rows in 20..240 {
            for (i, deck) in [DeckForm::Full, DeckForm::Folded].into_iter().enumerate() {
                let (frame, _) = frame_in(&mut app, deck, rows);
                let carried = PhysicalSize::new(frame.width, frame.height + rem);
                let avail = crate::stats_plan::avail_pt(
                    f64::from(carried.height),
                    band_px(band_pts, scale) as f64,
                    scale,
                );
                if !in_band(avail) {
                    continue;
                }
                checked[i] = true;
                let size = attach(&mut app, rows, Some(carried));
                assert_eq!(form(&app), deck, "{rows} rows at avail {avail}");
                assert_eq!(size, carried, "{deck:?}: the window keeps its size");
                if deck == DeckForm::Full {
                    let (_, folded_avail) = frame_in(&mut app, DeckForm::Folded, rows);
                    assert!(folded_avail < 850.0, "PRECONDITION: {folded_avail}");
                    let _ = attach(&mut app, rows, None);
                    assert_eq!(form(&app), DeckForm::Folded, "a new window of that grid");
                }
            }
        }
        assert_eq!(checked, [true; 2], "CONTROL: both forms met the band");
    }

    /// A snapshot after three readings: ten cores in two clusters, memory, a
    /// GPU, a volume, aterm with one tab running claude.
    pub(crate) fn snapshot() -> Snapshot {
        let mut v = aterm_vitals::Vitals::new();
        let tabs = vec![TabRoot {
            key: 7,
            tab: 7,
            shell_pid: 200,
            label: "Tab 1".into(),
            program: "claude".into(),
        }];
        let mk = |t: u64, busy: &[u64], cpu: &[u64]| Sample {
            at_ns: t * 1_000_000_000,
            cores: Some(
                busy.iter()
                    .map(|&b| Ticks {
                        busy: b,
                        total: t * 100,
                    })
                    .collect(),
            ),
            clusters: vec![
                ClusterSpec {
                    name: "Super".into(),
                    logical: vec![0, 1, 2, 3],
                },
                ClusterSpec {
                    name: "Efficiency".into(),
                    logical: vec![4, 5, 6, 7, 8, 9],
                },
            ],
            mem: Some(MemSample {
                total: 32 << 30,
                app: 12 << 30,
                wired: 4 << 30,
                compressed: Some(1 << 30),
                cached: 8 << 30,
                free: 7 << 30,
                swap_used: 0,
                swap_total: 0,
                pressure: Pressure::Normal,
                commit: None,
                ..MemSample::default()
            }),
            gpu: Some(GpuSample {
                device_pct: Some(12),
                cores: Some(10),
                ..GpuSample::default()
            }),
            volume: Some(VolumeSample {
                total: 2_000_000_000_000,
                free: 1_810_000_000_000,
                available: 1_810_000_000_000,
                mount: "/".into(),
            }),
            procs: Some(vec![
                ProcSample {
                    start: Some(1),
                    pid: 100,
                    ppid: 1,
                    cpu_ns: Some(cpu[0] * 1_000_000),
                    footprint: Some(300 << 20),
                    name: "aterm".into(),
                    disk_written: None,
                    uid: None,
                    ..ProcSample::default()
                },
                ProcSample {
                    start: Some(1),
                    pid: 200,
                    ppid: 100,
                    cpu_ns: Some(0),
                    footprint: Some(5 << 20),
                    name: "zsh".into(),
                    disk_written: None,
                    uid: None,
                    ..ProcSample::default()
                },
                ProcSample {
                    start: Some(1),
                    pid: 201,
                    ppid: 200,
                    cpu_ns: Some(cpu[1] * 1_000_000),
                    footprint: Some(400 << 20),
                    name: "claude".into(),
                    disk_written: None,
                    uid: None,
                    ..ProcSample::default()
                },
            ]),
            self_pid: 100,
            tabs: tabs.clone(),
            ..Sample::default()
        };
        let _ = v.absorb(&mk(10, &[0; 10], &[0, 0]));
        let _ = v.absorb(&mk(11, &[20, 30, 10, 0, 5, 5, 5, 5, 5, 5], &[100, 400]));
        v.absorb(&mk(
            12,
            &[120, 130, 60, 10, 10, 10, 10, 10, 10, 10],
            &[200, 1200],
        ))
    }

    pub(crate) fn dark_inks() -> VitalsInks {
        aterm_vitals::ink::vitals_inks([0x30, 0x31, 0x35], [0xF8, 0xF8, 0xF2], [0x50, 0xFA, 0x7B])
    }

    #[test]
    fn deck_inks_hold_on_every_built_in_band() {
        let blends = matches!(
            crate::chrome_band::PLATFORM_BAR_BASE,
            aterm_messages::ink::BarBase::Blend
        );
        let pinned = if blends { &PINNED_ON_BLENDS[..] } else { &[] };
        const PINNED_ON_BLENDS: [(&str, &str, f32); 5] = [
            ("Catppuccin Mocha", "heat_ic steps", 0.047),
            ("Dracula", "heat_ic steps", 0.038),
            ("Gruvbox Dark", "heat_ic steps", 0.042),
            ("Nord", "heat_ic steps", 0.038),
            ("One Dark", "heat_ic steps", 0.049),
        ];
        let bands = built_in_bands();
        assert!(bands.len() > 2, "CONTROL: themes to judge");
        let (mut failed, mut seen) = (Vec::new(), Vec::new());
        for (name, inks) in bands {
            for miss in inks.deck_floor_misses() {
                let line = format!("{name} (band {:?}): {}", inks.ground, miss.what);
                let pin = pinned
                    .iter()
                    .find(|(theme, floor, _)| *theme == name && miss.what.starts_with(floor));
                match pin {
                    Some(&(theme, floor, least)) if miss.out_of_reach => {
                        seen.push((theme, floor));
                        if miss.measured < least {
                            failed.push(format!("{line}, pinned at {least} or more"));
                        }
                    }
                    _ if miss.out_of_reach => {
                        failed.push(format!("{line}, out of reach, unpinned"))
                    }
                    _ => failed.push(line),
                }
            }
        }
        for &(theme, floor, _) in pinned {
            if !seen.contains(&(theme, floor)) {
                failed.push(format!("{theme}: {floor} is pinned out of reach but holds"));
            }
        }
        assert!(failed.is_empty(), "{}", failed.join("\n"));
    }

    /// Each built-in theme's name and the inks solved on its band.
    fn built_in_bands() -> Vec<(&'static str, VitalsInks)> {
        aterm_types::scheme::builtin_names()
            .into_iter()
            .map(|name| {
                let scheme = aterm_types::scheme::builtin(name).expect("listed scheme exists");
                let band = crate::chrome_band::band_colors(crate::chrome_band::ChromePalette {
                    theme: aterm_render::Theme::from(&scheme),
                    ansi: Some(crate::chrome_band::ansi_of_scheme(&scheme)),
                    authored: aterm_messages::tokens::AuthoredChrome::NONE,
                });
                (name, ink::vitals_inks(band.bar_bg, band.value, band.accent))
            })
            .collect()
    }

    /// The merged deck's inks on every built-in band, with Increase Contrast's below, then on
    /// the mockups' two grounds above the mockups' own inks, to judge against
    /// `docs/design-studies/top-deck-*-2026-10-23.png`.
    #[test]
    #[ignore = "writes PNGs for a person to read: set ATERM_DECK_GALLERY to a directory"]
    fn merged_ink_sheet() {
        let Some(dir) = std::env::var_os("ATERM_DECK_GALLERY") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).expect("the gallery directory");
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let (w, row) = (2240.0_f32, 112.0_f32);
        let bands = built_in_bands();
        let mut prims = Vec::new();
        for (i, (name, inks)) in bands.iter().enumerate() {
            ink_sheet_row(&mut prims, i as f32 * row, w, name, inks);
        }
        let mut y = bands.len() as f32 * row;
        for (ground, fg, accent, mockup) in [
            (
                [0x30, 0x31, 0x35],
                [0xF8, 0xF8, 0xF2],
                [0x50, 0xFA, 0x7B],
                MOCKUP_DARK,
            ),
            (
                [0xE2, 0xE3, 0xE5],
                [0x24, 0x29, 0x2F],
                [0x1A, 0x7F, 0x37],
                MOCKUP_LIGHT,
            ),
        ] {
            let inks = ink::vitals_inks(ground, fg, accent);
            y += ink_sheet_reference(&mut prims, y, w, &inks, &mockup);
        }
        let (rgba, pw, ph) =
            crate::tray_raster::rasterize_tray(&prims, w as u32, y as u32, 1.0, [0, 0, 0, 0xFF]);
        let png =
            crate::app_introspect::encode_rgba8_png(&rgba, pw, ph).expect("the sheet encodes");
        let file = dir.join("merged_inks.png");
        std::fs::write(&file, png).expect("the PNG writes");
        eprintln!("{} bands: {}", bands.len(), file.display());
    }

    /// The mockups' inks, as their generator drew them, in
    /// [`ink_sheet_reference`]'s order.
    const MOCKUP_DARK: [&str; 17] = [
        "#FF6CB8", "#1B1B1E", "#5EBEB6", "#C6B6FF", "#3A4448", "#464551", "#C3C4CA", "#A2A3A9",
        "#FFAE5C", "#404145", "#4C6277", "#607886", "#768D90", "#9DA27D", "#C3B660", "#E1CB57",
        "#FFE148",
    ];
    const MOCKUP_LIGHT: [&str; 17] = [
        "#B40065", "#FFFFFF", "#03645D", "#5434C8", "#C8D1D3", "#CECDDB", "#474E57", "#5E6570",
        "#A24200", "#FFFFFF", "#7E95AD", "#758FAA", "#6C8B9E", "#8E875D", "#A48A32", "#B78600",
        "#C27B00",
    ];

    fn hex(c: Rgb) -> String {
        format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
    }

    fn sheet_text(prims: &mut Vec<DrawPrim>, x: f32, baseline: f32, s: &str, color: Rgb) {
        sheet_words(prims, x, baseline, s, color, TypeStep::Body);
    }

    fn sheet_words(
        prims: &mut Vec<DrawPrim>,
        x: f32,
        baseline: f32,
        s: &str,
        color: Rgb,
        step: TypeStep,
    ) {
        prims.push(text_prim(
            x,
            baseline,
            s.to_owned(),
            step.px(20.0),
            TextWeight::Regular,
            TextFace::Ui,
            rgba(color, 0xFF),
        ));
    }

    /// One band's row: its name, then the merged inks as the deck draws them,
    /// above and under Increase Contrast below.
    fn ink_sheet_row(prims: &mut Vec<DrawPrim>, y: f32, w: f32, name: &str, inks: &VitalsInks) {
        prims.push(panel(0.0, y, w, 112.0, 0.0, rgba(inks.ground, 0xFF)));
        sheet_text(prims, 12.0, y + 34.0, name, inks.words);
        sheet_text(prims, 12.0, y + 62.0, &hex(inks.ground), inks.words2);
        for (k, m) in [inks.merged, inks.merged_ic].iter().enumerate() {
            let y = y + 6.0 + k as f32 * 52.0;
            let mut x = 300.0;
            for i in 0..=120 {
                let c = m.heat_at(i as f32 / 120.0);
                prims.push(panel(
                    x + i as f32 * 2.0,
                    y + 6.0,
                    2.0,
                    32.0,
                    0.0,
                    rgba(c, 0xFF),
                ));
            }
            x += 260.0;
            for (j, level) in [0.1_f32, 0.35, 0.6, 0.85, 1.0].into_iter().enumerate() {
                let tx = x + j as f32 * 22.0;
                prims.push(panel(tx, y + 4.0, 16.0, 36.0, 2.0, rgba(m.well, 0xFF)));
                let fh = 36.0 * level;
                prims.push(panel(
                    tx,
                    y + 40.0 - fh,
                    16.0,
                    fh,
                    2.0,
                    rgba(m.heat_at(level), 0xFF),
                ));
            }
            x += 124.0;
            prims.push(panel(x, y + 8.0, 190.0, 30.0, 15.0, rgba(m.needs, 0xFF)));
            sheet_text(prims, x + 14.0, y + 30.0, "Tab 1 password", m.needs_text);
            sheet_text(prims, x + 204.0, y + 30.0, "Needs you", m.needs);
            x += 330.0;
            for (ink, tint, words) in [
                (m.all_tabs, m.all_tabs_tint, "all tabs 57%"),
                (m.this_tab, m.this_tab_tint, "this tab 52%"),
            ] {
                prims.push(panel(x, y + 2.0, 400.0, 42.0, 6.0, rgba(tint, 0xFF)));
                sheet_text(prims, x + 10.0, y + 30.0, words, ink);
                sheet_text(prims, x + 150.0, y + 30.0, "22 GiB", m.secondary);
                prims.push(panel(
                    x + 236.0,
                    y + 14.0,
                    150.0,
                    16.0,
                    3.0,
                    rgba(m.well, 0xFF),
                ));
                prims.push(panel(x + 236.0, y + 14.0, 86.0, 16.0, 3.0, rgba(ink, 0xFF)));
                x += 412.0;
            }
            sheet_text(prims, x, y + 30.0, "10m", m.axis);
            sheet_text(prims, x + 70.0, y + 30.0, "now", m.axis);
            x += 140.0;
            for s in 0..3 {
                prims.push(panel(
                    x,
                    y + 34.0 - s as f32 * 9.0,
                    28.0,
                    6.0,
                    1.0,
                    rgba(inks.over, 0xFF),
                ));
            }
            sheet_text(prims, x + 36.0, y + 30.0, "~8 waiting", m.over_text);
        }
    }

    /// The mockups' grounds: each merged ink above the mockup's own, with both
    /// values. Returns the block's height.
    fn ink_sheet_reference(
        prims: &mut Vec<DrawPrim>,
        y: f32,
        w: f32,
        inks: &VitalsInks,
        mockup: &[&str; 17],
    ) -> f32 {
        let m = &inks.merged;
        let ours = [
            ("needs", m.needs),
            ("needs_text", m.needs_text),
            ("all_tabs", m.all_tabs),
            ("this_tab", m.this_tab),
            ("all_tabs_tint", m.all_tabs_tint),
            ("this_tab_tint", m.this_tab_tint),
            ("secondary", m.secondary),
            ("axis", m.axis),
            ("over_text", m.over_text),
            ("well", m.well),
        ]
        .into_iter()
        .chain((0..7).map(|k| ("heat", m.heat[k])));
        prims.push(panel(0.0, y, w, 230.0, 0.0, rgba(inks.ground, 0xFF)));
        sheet_text(prims, 12.0, y + 30.0, &hex(inks.ground), inks.words);
        sheet_text(prims, 12.0, y + 100.0, "ours", inks.words);
        sheet_text(prims, 12.0, y + 190.0, "the mockup's", inks.words);
        for (i, ((name, c), theirs)) in ours.zip(mockup).enumerate() {
            let x = 200.0 + i as f32 * 120.0;
            let theirs = u32::from_str_radix(&theirs[1..], 16).expect("a hex colour");
            let theirs = [(theirs >> 16) as u8, (theirs >> 8) as u8, theirs as u8];
            sheet_words(prims, x, y + 40.0, name, inks.words2, TypeStep::Caption);
            prims.push(panel(x, y + 52.0, 110.0, 40.0, 3.0, rgba(c, 0xFF)));
            sheet_words(prims, x, y + 116.0, &hex(c), inks.words, TypeStep::Caption);
            prims.push(panel(x, y + 142.0, 110.0, 40.0, 3.0, rgba(theirs, 0xFF)));
            sheet_words(
                prims,
                x,
                y + 206.0,
                &hex(theirs),
                inks.words,
                TypeStep::Caption,
            );
        }
        230.0
    }

    pub(crate) fn input(snap: &Snapshot) -> DeckInput<'_> {
        DeckInput {
            snap: Some(snap),
            tab_panes: vec![7],
            tab_label: "Tab 1".into(),
            tab_count: 1,
            inks: dark_inks(),
            dwc: false,
            marks: Vec::new(),
            plate: None,
            form: DeckForm::Full,
            contrast: false,
            locale: Locale {
                platform: Platform::MacOs,
                utc_offset_s: 0,
            },
        }
    }

    /// Tab 1 split in two panes after three readings a second apart: claude in pane `a` (1 core,
    /// 400 MiB) and cargo in pane `b` (2 cores, 600 MiB), each under its own zsh, on ten cores.
    pub(crate) fn split_tab(a: u64, b: u64) -> Snapshot {
        split_tab_running(a, b, ["claude", "cargo"])
    }

    /// [`split_tab`] with the panes' programs named `names`.
    pub(crate) fn split_tab_running(a: u64, b: u64, names: [&str; 2]) -> Snapshot {
        use aterm_vitals::model::ProcLimits;
        let pane = |key: u64, shell_pid: u32| TabRoot {
            key,
            tab: a.min(b),
            shell_pid,
            label: "Tab 1".into(),
            program: String::new(),
        };
        let proc = |pid: u32, ppid: u32, cpu_ms: u64, mib: u64, name: &str| ProcSample {
            start: Some(1),
            pid,
            ppid,
            cpu_ns: Some(cpu_ms * 1_000_000),
            footprint: Some(mib << 20),
            name: name.into(),
            ..ProcSample::default()
        };
        let mk = |t: u64| Sample {
            at_ns: t * 1_000_000_000,
            at_unix_ms: 1_792_000_800_000 + t as i64 * 1000,
            mono_ms: t * 1000,
            cadence_ms: 1000,
            cores: Some(vec![
                Ticks {
                    busy: t * 50,
                    total: t * 100,
                };
                10
            ]),
            mem: Some(MemSample {
                total: 32 << 30,
                app: 12 << 30,
                ..MemSample::default()
            }),
            procs: Some(vec![
                proc(100, 1, 0, 300, "aterm"),
                proc(200, 100, 0, 5, "zsh"),
                proc(201, 200, t * 1000, 400, names[0]),
                proc(300, 100, 0, 5, "zsh"),
                proc(301, 300, t * 2000, 600, names[1]),
            ]),
            self_pid: 100,
            tabs: vec![pane(a, 200), pane(b, 300)],
            proc_limits: ProcLimits {
                system: Some(1000),
                per_user: Some(500),
            },
            pid_counts: (Some(900), Some(400)),
            ..Sample::default()
        };
        let mut v = aterm_vitals::Vitals::new();
        let _ = v.absorb(&mk(10));
        let _ = v.absorb(&mk(11));
        v.absorb(&mk(12))
    }

    /// The deck of a split tab, focused on pane 8, reads the whole tab: each scoped row's "this
    /// tab" share is both panes', inside all tabs, and its words say so. CONTROL: the focused
    /// pane alone is less.
    #[test]
    fn a_split_tabs_deck_reads_every_pane() {
        let snap = split_tab(7, 8);
        let m = deck_metrics();
        let deck = |panes: Vec<u64>| DeckInput {
            tab_panes: panes,
            tab_count: 2,
            ..input(&snap)
        };
        let bar = |input: &DeckInput<'_>, res: Resource| {
            DeckWords::new(input, &m)
                .rows
                .into_iter()
                .find(|r| r.res == res)
                .and_then(|r| r.bar)
                .expect("a scoped row's bar")
        };
        let near = |a: Option<f32>, b: f64| a.is_some_and(|a| (f64::from(a) - b).abs() < 1e-4);
        let gib = (32u64 << 30) as f64;
        for (res, tab, focused) in [
            (Resource::Cpu, 0.3, 0.2),
            (
                Resource::Memory,
                (1010u64 << 20) as f64 / gib,
                (605u64 << 20) as f64 / gib,
            ),
            (Resource::Processes, 0.004, 0.002),
        ] {
            let (whole, alone) = (bar(&deck(vec![8, 7]), res), bar(&deck(vec![8]), res));
            assert!(near(whole.tab, tab), "{res:?}: {:?} vs {tab}", whole.tab);
            assert!(near(alone.tab, focused), "CONTROL {res:?}: {:?}", alone.tab);
            assert!(whole.tab <= whole.aterm, "{res:?}: this tab ≤ all tabs");
            assert_eq!(
                bar(&deck(vec![7, 8]), res),
                whole,
                "{res:?}: whichever pane leads"
            );
        }
        let cpu =
            |panes: &[u64]| words::row_sentence(Resource::Cpu, &snap, panes, &input(&snap).locale);
        assert!(
            cpu(&[8, 7]).contains("Tab 1 3.0 cores."),
            "{}",
            cpu(&[8, 7])
        );
        assert!(
            cpu(&[8]).contains("Tab 1 2.0 cores."),
            "CONTROL: {}",
            cpu(&[8])
        );
        let tab = snap.tab_of(&[8, 7]).expect("the tab");
        assert_eq!(
            (tab.key, tab.scope.procs),
            (8, 4),
            "the sill's lanes and the chip's meter"
        );
        assert!(
            (tab.scope.cores.raw - 3.0).abs() < 1e-4,
            "{:?}",
            tab.scope.cores
        );
    }

    const GEOM: DeckGeom = DeckGeom {
        w: 2238.0,
        h: 208.0,
        pad: 24.0,
        scale: 2.0,
        font_px: 26.0,
    };

    /// The Deck sits at the head's foot under macOS's titlebar; where the strip is the
    /// window's band (Linux, Windows), at the head's top, so the band keeps its whole height
    /// under it.
    #[cfg(unix)]
    #[test]
    fn the_deck_takes_the_heads_foot_under_a_titlebar_and_its_top_over_a_band() {
        let _attached = attach_stats();
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.config
            .sys
            .get_or_insert_with(Default::default)
            .stats_bars = Some("deck".into());
        let deck = deck_px(
            StatsBars::Deck,
            app.windows[&wid].deck_form,
            app.windows[&wid].scale,
            app.win_cell_size(wid).1,
        );
        let head = deck + 14;
        app.windows.get_mut(&wid).expect("window").metrics.head = head;
        app.backend.set_head(head);
        let pad_top = app.win_pad_top(wid);
        let rows = app.deck_rows_px(wid).expect("the head holds the Deck");
        assert_eq!(rows.len(), deck);
        let (want, band_top) = if crate::tab_bar::STRIP_IS_CHROME_BAND {
            (0..deck, pad_top + head - deck)
        } else {
            (head - deck..head, pad_top + head)
        };
        assert_eq!(rows, want);
        assert_eq!(app.strip_band_top_px(wid), band_top);
        let f = app.deck_frame(wid).expect("the Deck draws");
        assert_eq!(
            f.dy as usize, rows.start,
            "the card is drawn where its rows are"
        );
        app.windows.get_mut(&wid).expect("window").metrics.head = deck - 1;
        app.backend.set_head(deck - 1);
        assert_eq!(app.deck_rows_px(wid), None);
        assert_eq!(app.strip_band_top_px(wid), pad_top + deck - 1);
        app.config
            .sys
            .get_or_insert_with(Default::default)
            .stats_bars = Some("off".into());
        app.windows.get_mut(&wid).expect("window").metrics.head = head;
        app.backend.set_head(head);
        assert_eq!(app.deck_rows_px(wid), None);
        assert_eq!(app.strip_band_top_px(wid), pad_top + head);
    }

    /// A point over the Deck is not a strip column, though a point above the
    /// grid clamps onto the strip's row: a press there is the Deck's
    /// (`App::stats_press`), and a pointer resting on it lights no tab and
    /// steps no strip drag.
    #[cfg(unix)]
    #[test]
    fn a_point_over_the_deck_is_no_strip_column() {
        let _attached = attach_stats();
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 1;
        app.splice_tab_strip(wid);
        app.config
            .sys
            .get_or_insert_with(Default::default)
            .stats_bars = Some("deck".into());
        let deck = deck_px(
            StatsBars::Deck,
            app.windows[&wid].deck_form,
            app.windows[&wid].scale,
            app.win_cell_size(wid).1,
        );
        let head = deck + 14;
        app.windows.get_mut(&wid).expect("window").metrics.head = head;
        app.backend.set_head(head);
        let rows = app.deck_rows_px(wid).expect("the head holds the Deck");
        let (cw, ch) = app.win_cell_size(wid);
        let (fx, fy) = app.frame_origin(wid);
        let col = 3u16;
        let x = fx as f64 + (app.win_pad(wid) + usize::from(col) * cw + cw / 2) as f64;
        let over_deck = fy as f64 + (rows.start + rows.len() / 2) as f64;
        let on_strip = fy as f64 + (app.win_pad_top(wid) + head + ch / 2) as f64;
        assert_eq!(app.strip_col_at(wid, x, over_deck), None, "over the Deck");
        assert_eq!(app.strip_col_at(wid, x, on_strip), Some(col), "the strip");
        app.config
            .sys
            .get_or_insert_with(Default::default)
            .stats_bars = Some("off".into());
        assert_eq!(app.deck_rows_px(wid), None);
        assert_eq!(
            app.strip_col_at(wid, x, over_deck),
            Some(col),
            "control: with the Deck off the same point clamps onto the strip"
        );
    }

    #[test]
    fn the_stats_bars_setting_reads_its_own_words() {
        assert_eq!(StatsBars::parse(None), StatsBars::Deck);
        assert_eq!(StatsBars::parse(Some("off")), StatsBars::Off);
        assert_eq!(StatsBars::parse(Some("sill")), StatsBars::Sill);
        assert_eq!(StatsBars::parse(Some(" Sill ")), StatsBars::Sill);
        assert_eq!(StatsBars::parse(Some("nonsense")), StatsBars::Deck);
        for f in StatsBars::ALL {
            assert_eq!(StatsBars::from_word(f.word()), Some(f));
            assert_eq!(StatsBars::parse(Some(f.word())), f);
        }
    }

    #[test]
    fn the_same_snapshot_lays_the_same_pixels() {
        let snap = snapshot();
        for form in [DeckForm::Full, DeckForm::Folded] {
            let mut i = input(&snap);
            i.form = form;
            let a = deck_prims(&GEOM, &i);
            let b = deck_prims(&GEOM, &i);
            assert_eq!(prims_fp(&a), prims_fp(&b), "{form:?}");
        }
    }

    #[test]
    fn a_panel_one_ulp_over_or_two_prims_swapped_is_another_deck() {
        let snap = snapshot();
        let laid = deck_prims(&GEOM, &input(&snap));
        let mut moved = laid.clone();
        let x = moved
            .iter_mut()
            .find_map(|p| match p {
                DrawPrim::Panel { x, .. } => Some(x),
                _ => None,
            })
            .expect("the deck lays a panel");
        *x = f32::from_bits(x.to_bits() + 1);
        assert_ne!(prims_fp(&moved), prims_fp(&laid));
        let mut swapped = laid.clone();
        swapped.swap(0, laid.len() - 1);
        assert_ne!(prims_fp(&swapped), prims_fp(&laid));
    }

    #[test]
    fn before_the_first_snapshot_the_deck_says_it_is_reading() {
        let empty = Snapshot::default();
        for form in [DeckForm::Full, DeckForm::Folded] {
            let mut i = input(&empty);
            i.snap = None;
            i.form = form;
            let (prims, hits) = deck_paint(&GEOM, &i);
            let said: Vec<String> = prims
                .iter()
                .filter_map(|p| match p {
                    DrawPrim::Text { s, .. } => Some(s.clone()),
                    _ => None,
                })
                .collect();
            for name in ["CPU", "Memory", "Disk", "Processes"] {
                assert!(said.iter().any(|s| s == name), "{form:?}: {said:?}");
            }
            let reading = said.iter().filter(|s| *s == "reading\u{2026}").count();
            assert_eq!(reading, DECK_RESOURCES.len(), "{form:?}: {said:?}");
            assert!(hits.is_empty(), "{form:?}: {hits:?}");
        }
    }

    #[test]
    fn every_bar_label_carries_its_ink_as_a_key() {
        let snap = snapshot();
        let inks = dark_inks();
        for (form, w) in [(DeckForm::Full, 2238.0), (DeckForm::Folded, 2238.0)] {
            let mut i = input(&snap);
            i.form = form;
            let g = DeckGeom { w, ..GEOM };
            let prims = deck_prims(&g, &i);
            for ink in [inks.machine, inks.aterm, inks.tab] {
                assert!(
                    prims
                        .iter()
                        .any(|p| matches!(p, DrawPrim::Panel { fill, w, y, .. }
                        if fill[..3] == ink[..] && *w <= 12.0 && *y < 28.0)),
                    "{form:?}: {ink:?}"
                );
            }
        }
    }

    /// A meter's value is the share as a whole percent up to 100, but a
    /// share above 0 that the % column prints as "<1%" keeps a tenth, so it
    /// is not 0 beside words that say "less than 1 percent".
    #[cfg(a11y_tree)]
    #[test]
    fn a_meter_under_half_a_percent_is_not_zero() {
        assert_eq!(super::meter_pct(Some(0.004)), Some(0.4));
        assert_eq!(super::meter_pct(Some(0.000_1)), Some(0.1));
        assert_eq!(super::meter_pct(Some(0.0)), Some(0.0));
        assert_eq!(super::meter_pct(Some(0.006)), Some(1.0));
        assert_eq!(super::meter_pct(Some(0.424)), Some(42.0));
        assert_eq!(super::meter_pct(Some(1.3)), Some(100.0));
        assert_eq!(super::meter_pct(Some(f32::NAN)), None);
        assert_eq!(super::meter_pct(None), None);
        assert_eq!(words::pct(Some(0.004)), "<1%");
    }

    /// T-G12 (Stats Deck v2 §12): the deck reads as one group of six OS resource meters and
    /// its CPU cores, each meter saying its row's words.
    #[cfg(a11y_tree)]
    #[test]
    fn a11y_meters() {
        use super::paint_tests::{FRONT, Scene, scene};
        use aterm_vitals::model::NoFigure;
        const MAC: Locale = Locale {
            platform: Platform::MacOs,
            utc_offset_s: 0,
        };
        let said = |snap: &Snapshot| stats_a11y_group(Some(snap), &[FRONT], &MAC);
        let meter = |snap: &Snapshot, res: Resource| {
            let want = words::resource_name(res);
            said(snap)
                .meters
                .into_iter()
                .find(|m| m.name == want)
                .expect("meter in DECK_RESOURCES")
        };

        let calm = scene(Scene::Calm);
        let g = said(&calm);
        assert_eq!(g.name, "Stats deck, last 10 minutes");
        let names: Vec<&str> = g.meters.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            ["CPU", "GPU", "Memory", "Disk", "Network", "Processes"]
        );
        for (m, &res) in g.meters.iter().zip(&DECK_RESOURCES) {
            assert_eq!(m.words, words::row_sentence(res, &calm, &[FRONT], &MAC));
        }
        assert_eq!(g.meters[0].value, Some(98.0));
        assert_eq!(
            g.meters[0].words,
            "CPU 98 percent of capacity: 17.7 of 18 cores. aterm 6.8 cores, Tab 2 0.1 cores. \
             Rose from about 49 percent. Over capacity 6 of the last 10 minutes. 8 waiting for \
             a core, load 25.7 on 18 cores. Busiest rustc, 9 processes in Tab 3, 5.9 cores."
        );
        let trouble = scene(Scene::Trouble);
        let cpu = meter(&trouble, Resource::Cpu);
        assert_eq!(
            cpu.words,
            words::row_sentence(Resource::Cpu, &trouble, &[FRONT], &MAC)
        );
        assert!(
            cpu.words.contains(" Why: Spotlight, after start-up. "),
            "{}",
            cpu.words
        );
        assert_eq!(g.cores_name, "18 CPU cores");
        assert_eq!(g.cores.len(), 18);
        assert_eq!(g.cores[0].name, "Super core 1");
        assert_eq!(g.cores[6].name, "Performance core 1");
        let core_names: std::collections::BTreeSet<&str> =
            g.cores.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(core_names.len(), 18, "every core's name is its own");
        for m in g.meters.iter().chain(&g.cores) {
            let v = m.value.expect("read");
            assert!((0.0..=100.0).contains(&v), "{m:?}");
        }
        assert_eq!(
            g.cores[7].words,
            format!("{:.0} percent busy", g.cores[7].value.unwrap())
        );

        let disk = meter(&scene(Scene::Trouble), Resource::Disk);
        assert_eq!(disk.value, Some(98.0));
        assert_eq!(
            disk.words,
            "Disk 98 percent of capacity: 1.96 of 2.00 terabytes used. aterm writes 410 \
             megabytes per second, Tab 2 writes 0.2 megabytes per second. Steady near 98 \
             percent. Disk full in about 34 minutes, filling 1.2 gigabytes per minute. Disk low, \
             41 gigabytes free. aterm freed 12 gigabytes today. Used grew 12 gigabytes in the \
             last 10 minutes. Most in ~/aterm/target, 212 gigabytes."
        );

        let mut first = scene(Scene::FirstRun);
        first.facts.net_read_s = 3_600;
        let net = meter(&first, Resource::Network);
        assert_eq!(net.value, Some(9.0));
        assert_eq!(
            net.words,
            "Network 9 percent of its link rate: 2.1 of 22.9 megabytes per second, downloading. \
             Learning its peak: 1 of 24 hours read. No per-app figure. Not read for 9 of the \
             last 10 minutes. Sending 0.3 of 22.9 megabytes per second. Wi-Fi en0, link 22.9 \
             megabytes per second."
        );

        let no_agent = ScopeFig {
            missing: Some(NoFigure::NoAgent),
            ..ScopeFig::default()
        };
        first.resources[Resource::Tokens.index()].tabs = vec![(FRONT, no_agent)];
        assert_eq!(
            words::row_sentence(Resource::Tokens, &first, &[FRONT], &MAC),
            "Tokens: learning aterm's peak, 1 of 10 minutes. aterm 3.1 thousand per minute. Tab \
             2 has no agent. Not read for 9 of the last 10 minutes. Input 410 thousand tokens \
             per minute, cache re-reads included. Plan limit not read."
        );

        let mut unread = scene(Scene::Calm);
        unread.resources[Resource::Gpu.index()] = ResourceView::default();
        let gpu = meter(&unread, Resource::Gpu);
        assert_eq!((gpu.value, gpu.words.as_str()), (None, "GPU: not read."));

        let reading = stats_a11y_group(None, &[FRONT], &MAC);
        for (m, &res) in reading.meters.iter().zip(&DECK_RESOURCES) {
            assert_eq!(m.value, None, "{m:?}");
            assert_eq!(m.words, format!("{}: reading.", words::resource_name(res)));
        }
        assert_eq!(reading.meters.len(), DECK_RESOURCES.len());
        assert!(reading.cores.is_empty());
    }
}

#[cfg(test)]
#[path = "stats_deck_paint_tests.rs"]
pub(crate) mod paint_tests;

#[cfg(any(test, debug_assertions))]
#[path = "stats_deck_fixtures.rs"]
pub(crate) mod deck_fixtures;
