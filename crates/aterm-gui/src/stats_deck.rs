// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pixel-rendered Global, aterm, and Tab stats bars under the titlebar.

use std::sync::Arc;

use aterm_vitals::ink::{self, Rgb, VitalsInks};
use aterm_vitals::model::{DiskLevel, HeatSlot, Resource, ResourceView, ScopeFig, Snapshot, State};
use aterm_vitals::words::{self, InkKind, Locale, Platform};
use aterm_vitals::{
    GapKind, MultiScaleSpec, ProgSlot, Strain, StripEvent, StripEventKind, TimeGap,
};

#[cfg(test)]
use crate::deck_face::Boxed;
use crate::deck_face::{
    ASCENT_EM, BoxKind, Boxes, DESCENT_EM, FaceMetrics, Txt, deck_metrics, fit, txts,
};
use crate::stats_gauge::{self as gauge, Gauge, GaugeInks};
use crate::stats_live::{self as live, DeckLive, LiveLayer};
use crate::stats_paint::{Bar, Hit, Instrument, Painter, Rect, StatsMark, WellMark, panel};
use crate::stats_plan::{
    self as plan, BlockKind, BlockPlan, Cluster, Col, CoresAt, CoresMode, DeckForm, DeckSizePref,
    FOLD_GAUGE_R, FoldedGauge, FullKind, FullPlan, HeaderPlan, HeaderText, KeyItem, Label, Meter,
    Rows, StateLabel, Strip,
};
use crate::stats_steam as steam;
use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, TextFace, TextWeight, rgba, text_prim};

/// The slot left of the Full deck's names (the gauge's) and its divider run from this far below
/// the deck's top to this far above its foot, pt.
const SLOT_TOP: f64 = 4.0;
const SLOT_FOOT: f64 = 6.0;

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

/// The complete reserved deck, including its fixed Network State rows.
pub(crate) fn reserved_deck_px(
    form: StatsBars,
    deck: DeckForm,
    scale: f64,
    cell_px: usize,
) -> usize {
    deck_px(form, deck, scale, cell_px)
        + if form.takes_room() {
            crate::network_deck::height_px(scale)
        } else {
            0
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
    band_px(head_pts, scale) + reserved_deck_px(form, deck, scale, cell_px)
}

/// The deck a carried window frame was sized under ([`carried_deck`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CarriedDeck {
    pub(crate) form: DeckForm,
    /// The head it gives the window, in device px.
    pub(crate) head: usize,
}

impl CarriedDeck {
    /// Whether a window keeps this carried frame, re-gridding to it once, though the size
    /// setting decided the form `deck` for it: the deck's form, which never follows the window,
    /// sized it in the other form, so the head moved under the frame for something other than
    /// its rows.
    pub(crate) fn keeps_frame(self, deck: DeckForm) -> bool {
        self.form != deck
    }
}

/// What deck sized a carried frame `carried_h` px tall: the first head (`head_in`) under which
/// the carried grid's frame (`frame_h`) sits less than a cell (`cell_h`) below `carried_h`,
/// trying the Full and then the Folded form. The two forms differ by whole cells
/// ([`deck_px`]), so a frame matches one of them. `None` when no deck gives that frame.
pub(crate) fn carried_deck(
    carried_h: u32,
    cell_h: usize,
    head_in: impl Fn(DeckForm) -> usize,
    frame_h: impl Fn(usize) -> u32,
) -> Option<CarriedDeck> {
    [DeckForm::Full, DeckForm::Folded]
        .into_iter()
        .map(|form| CarriedDeck {
            form,
            head: head_in(form),
        })
        .find(|c| {
            carried_h
                .checked_sub(frame_h(c.head))
                .is_some_and(|extra| (extra as usize) < cell_h)
        })
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

    /// What deck sized window `wid`'s carried frame, `carried_h` px tall, for its carried
    /// `rows`×`cols` grid under a `head_pts` titlebar band at `scale` ([`carried_deck`]).
    pub(crate) fn carried_deck_of(
        &self,
        wid: crate::WindowId,
        head_pts: f64,
        scale: f64,
        (rows, cols): (u16, u16),
        carried_h: u32,
    ) -> Option<CarriedDeck> {
        let bars = self.stats_bars_form();
        let (_, cell_h) = self.win_cell_size(wid);
        let band = band_px(head_pts, scale);
        let bare = u64::from(self.window_frame_px_for(wid, rows, cols).height)
            .saturating_sub(self.backend.head() as u64);
        carried_deck(
            carried_h,
            cell_h,
            |form| band + reserved_deck_px(bars, form, scale, cell_h),
            |head| u32::try_from(bare.saturating_add(head as u64)).unwrap_or(u32::MAX),
        )
    }

    /// Window `wid`'s chrome head in device pixels for a `head_pts` titlebar band at `scale`,
    /// its deck's form first decided from the size setting ([`plan::deck_form_for`]) where
    /// `frame_h_px` is a frame's height (0 keeps the form).
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
                    ws.deck_form = plan::deck_form_for(pref);
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
        match reserved_deck_px(form, ws.deck_form, ws.scale, ch) {
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
    /// Host-published immutable observations; absent in resource-only fixtures.
    pub(crate) network: Option<&'a aterm_types::network_state::NetworkStateSnapshot>,
    /// Stable machine/service selected in the network close-up.
    pub(crate) network_target: crate::network_deck::Target,
    /// The latest snapshot, `None` before the first.
    pub(crate) snap: Option<&'a Snapshot>,
    /// The snapshot immediately preceding [`Self::snap`], when available, for smooth value
    /// interpolation across readings.
    pub(crate) prev_snap: Option<&'a Snapshot>,
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
    /// The live edge this frame draws ([`crate::stats_live`]): the glide, the beat, and
    /// whether readings keep coming.
    pub(crate) live: DeckLive,
}

const HEADER_BASE_FULL: f64 = 11.5;
const HEADER_BASE_FOLDED: f64 = 9.5;
const HEADER_HIT_FULL: f64 = 14.5;
const HEADER_HIT_FOLDED: f64 = 12.5;
const SMALL_PT: f64 = 9.5;
/// The least height of a time-scale tick, device px.
const TICK_MIN_PX: f32 = 2.0;
const MARK_EM: f64 = 1.1;
const STATE_GAP: f64 = 6.0;
/// The least of the bar a scope that draws a fill takes as a click target, pt
/// ([`Deck::bar_segments`]).
const SEGMENT_HIT_MIN: f64 = 6.0;
const STRIP_SPECS: [MultiScaleSpec; 4] = [
    MultiScaleSpec::FULL_6H,
    MultiScaleSpec::FOLD_4H,
    MultiScaleSpec::COMPACT_6M,
    MultiScaleSpec::MINI_1M,
];
/// How far back one reading may speak for the seconds before it that hold none of their own, in
/// a strip's boxes cut from the last minute's readings ([`ResourceView::heat_multi_scale`]):
/// three of the background cadence, v2 §5.4's gap rule, so a strip read while aterm was behind
/// other apps keeps its readings and a longer gap stays unread.
const SLOT_COVER_MS: i64 = 15_000;

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

impl BarFill {
    /// The machine's, aterm's and this tab's fills in a bar `w` device px long, in whole px for a
    /// whole `w`: each its scope's share of `w`, at least `least` px while that share is above
    /// zero, and never narrower than a scope nested inside it; none for a scope with no figure
    /// (`None`), even where a scope inside it has one.
    ///
    /// `apart` is for a bar whose fills nest by length alone, each its whole height (the bar
    /// under a % figure): there a scope above zero ends `apart` px or more past the fill inside
    /// it, so some of its own ink shows however close the two round, and even where its share
    /// equals the one inside it, as the column shows such a scope's rim. No fill passes `w`, and
    /// where `w` is too short for every such px the fills inside give way. The bar's column,
    /// whose fills nest in height too, passes `0`.
    fn widths_px(&self, w: f32, least: f32, apart: f32) -> [f32; 3] {
        // Innermost first: this tab's, aterm's, the machine's.
        let scopes = [self.tab, self.aterm, self.global];
        let (mut ends, mut past) = ([0.0_f32; 3], [0.0_f32; 3]);
        let mut inner_end = 0.0_f32;
        for (i, v) in scopes.into_iter().enumerate() {
            if v.is_none() {
                continue;
            }
            if let Some(s) = v.filter(|v| v.is_finite() && *v > 0.0) {
                past[i] = apart;
                ends[i] = (s.clamp(0.0, 1.0) * w).round().max(least);
            }
            ends[i] = ends[i].max(inner_end + past[i]);
            inner_end = ends[i];
        }
        // From the outside in: none past `w`, nor past where the fill around it keeps its px.
        let mut room = w;
        for i in (0..scopes.len()).rev() {
            if scopes[i].is_some() {
                ends[i] = ends[i].min(room).max(0.0);
                room = ends[i] - past[i];
            }
        }
        [ends[2], ends[1], ends[0]]
    }
}

/// A row's state for where Details is absent (rule S) and for the fslim
/// header (§4.4).
#[derive(Clone, Debug, PartialEq)]
struct StateTxt {
    state: State,
    clauses: Vec<Txt>,
    word: Txt,
    label: String,
    mark: WellMark,
    ink: InkKind,
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
    /// Multi-scale progressive time-series slots for each of [`STRIP_SPECS`], newest first: the
    /// live `now` slot, then each older (and wider) one ([`MultiScaleSpec::slots`]).
    multi_scale: [Vec<HeatSlot>; 4],
    bar: Option<BarFill>,
    /// The row's figure run at the end of its bar ([`words::glance_run`]), longest first.
    run: Vec<Txt>,
    /// Visible hardware/capacity figures where no ordinary amount column fits.
    capacity: Vec<Txt>,
    /// Available space beside a separate used/total amount column.
    available: Vec<Txt>,
    state: Option<StateTxt>,
    /// The Details clauses: the glance's, then the other tables' ([`Table::said`]).
    details: [Vec<ClauseTxt>; 2],
}

/// One cluster of cores, fastest first.
#[derive(Clone, Debug, PartialEq)]
struct CoreCluster {
    /// The OS's name; none where it names none (Linux's one "CPU").
    name: Option<String>,
    /// Each core's fast-average busy fraction; `None` when unreadable.
    heat: Vec<Option<f32>>,
}

/// Previous snapshot's numeric targets for smooth [`DeckLive::blend`] interpolation.
#[derive(Clone, Debug, Default, PartialEq)]
struct DeckPrev {
    rows: Vec<(Option<f32>, Option<BarFill>)>,
    cores: Vec<Vec<Option<f32>>>,
}

impl DeckPrev {
    fn of(snap: &Snapshot, panes: &[u64]) -> Self {
        let rows = DECK_RESOURCES
            .iter()
            .map(|&res| {
                let view = snap.resource(res, panes);
                let heat = view.heat_now.filter(|h| !h.is_nan());
                let bar = bar_fill(res, &view, snap, panes);
                (heat, bar)
            })
            .collect();
        let cores = snap
            .cpu
            .as_ref()
            .map(|cpu| {
                cpu.clusters
                    .iter()
                    .map(|c| {
                        c.cores
                            .iter()
                            .map(|&i| {
                                cpu.cores
                                    .get(i)
                                    .map(|core| core.load.level)
                                    .filter(|v| v.is_finite())
                            })
                            .collect()
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { rows, cores }
    }
}

/// The deck's words for one snapshot: what every row says, measured in the deck's face.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DeckWords {
    rows: Vec<RowWords>,
    cores: Vec<CoreCluster>,
    /// Logical CPU cores reported by the snapshot, independent of readable load counters.
    cpu_total: Option<usize>,
    /// The processor's name for trailing room on the `Cores` row (`Apple M5 Max`, `M5 Max`),
    /// longest first.
    cores_tail_forms: Vec<Txt>,
    /// The machine's totals for the legend where no run says the rows' maxima
    /// ([`words::machine_totals`]), longest first.
    totals: Vec<String>,
    /// Recorded time discontinuities (sleep, crash, pause) within the ring's horizon.
    time_gaps: Vec<TimeGap>,
    /// Discrete event markers (crash, restart, sleep/wake, spike) on the time series.
    strip_events: Vec<StripEvent>,
    /// Previous snapshot's values for fluid [`DeckLive::blend`] transitions.
    prev: Option<DeckPrev>,
    /// How hard the whole Mac works now and for how long: the gauge's reading. `None` with no
    /// snapshot yet.
    strain: Option<Strain>,
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
        let mut tail_candidates: Vec<String> = Vec::new();
        if let Some(brand) = snap.facts.cpu_brand.as_deref().filter(|b| !b.is_empty()) {
            tail_candidates.push(brand.to_owned());
            let short = words::compact_cpu_brand(brand);
            if short != brand && !short.is_empty() {
                tail_candidates.push(short);
            }
        }
        let cores_tail_forms = txts(tail_candidates, m);
        let prev = input
            .prev_snap
            .map(|prev_snap| DeckPrev::of(prev_snap, &input.tab_panes));
        Self {
            rows,
            cores,
            cpu_total: snap.cpu.as_ref().map(|cpu| cpu.total()).filter(|&n| n > 0),
            cores_tail_forms,
            totals: words::machine_totals(snap),
            time_gaps: snap.time_gaps.clone(),
            strip_events: snap.strip_events.clone(),
            prev,
            strain: Some(Strain::of(snap)),
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
                multi_scale: [Vec::new(), Vec::new(), Vec::new(), Vec::new()],
                bar: None,
                run: Vec::new(),
                capacity: Vec::new(),
                available: Vec::new(),
                state: None,
                details: [Vec::new(), Vec::new()],
            })
            .collect();
        Self {
            rows,
            cores: Vec::new(),
            cpu_total: None,
            cores_tail_forms: Vec::new(),
            totals: Vec::new(),
            time_gaps: Vec::new(),
            strip_events: Vec::new(),
            prev: None,
            strain: None,
            reading: true,
        }
    }

    /// Whether a strip of `strip`'s multi-scale horizon draws an over mark in any slot.
    fn over_shown(&self, strip: &Strip) -> bool {
        self.rows.iter().any(|row| {
            row.strip_text.is_none() && row.multi_scale_slots(strip).iter().any(|s| s.cell.over)
        })
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
        let strip_text = words::strip_words(res, snap).map(|forms| txts(forms, m));
        let amount = words::monitor_amount(res, snap);
        let run = txts(
            if amount.is_empty() {
                words::glance_run(res, &view, snap, loc)
            } else {
                amount
            },
            m,
        );
        let state = words::state(res, snap, loc).map(|st| StateTxt {
            state: st.state,
            clauses: txts(st.clauses, m),
            word: Txt::new(st.word, m),
            label: st.label,
            mark: WellMark::of(st.mark),
            ink: st.ink,
        });
        let clauses = |clauses: Vec<words::Clause>| -> Vec<ClauseTxt> {
            clauses
                .into_iter()
                .map(|c| ClauseTxt {
                    forms: txts(c.forms, m),
                    prio: c.prio,
                    keep: c.keep,
                    mark: c.mark.map(WellMark::of),
                    ink: c.ink,
                })
                .collect()
        };
        let details = [
            clauses(words::glance_details(res, snap, panes, loc)),
            clauses(words::details(res, snap, loc)),
        ];
        Self {
            res,
            name: Txt::new(words::resource_name(res), m),
            well: words::well(res, &view).map(|(mark, ink)| (WellMark::of(mark), ink)),
            heat_now: view.heat_now.filter(|h| !h.is_nan()),
            pct: Txt::new(words::pct(view.heat_now), m),
            strip_text,
            multi_scale: STRIP_SPECS.map(|spec| {
                view.heat_multi_scale(spec, snap.at_unix_ms, SLOT_COVER_MS, &snap.time_gaps)
            }),
            bar: bar_fill(res, &view, snap, panes),
            run,
            capacity: txts(words::monitor_capacity(res, snap), m),
            available: txts(words::monitor_available(res, snap), m),
            state,
            details,
        }
    }
}

/// The scope bar's fills, 0.144.0's as main draws them (the owner, 2026-10-26: "the bar of "this
/// Mac, aterm, Tab 1" that's great"): the machine's share now, aterm's and this tab's nested in
/// it ([`Snapshot::bar_shares`]), the disk's and the network's scopes by their writes and
/// sockets. Tokens have none.
fn bar_fill(res: Resource, view: &ResourceView, snap: &Snapshot, panes: &[u64]) -> Option<BarFill> {
    if res == Resource::Tokens {
        return None;
    }
    let shares = snap.bar_shares(res, panes);
    let cap = view
        .binding()
        .capacity
        .or(view.capacity)
        .filter(|&c| c > 0.0);
    let share = |fig: &ScopeFig| -> Option<f32> {
        let (v, c) = (fig.now?, cap?);
        (c > 0.0).then(|| (v / c).clamp(0.0, 1.0) as f32)
    };
    let machine = view
        .heat_now
        .filter(|h| !h.is_nan())
        .map(|h| h.clamp(0.0, 1.0));
    let global = machine.or_else(|| shares.map(|s| s[0]));
    let (aterm, tab) = match res {
        Resource::Cpu | Resource::Memory | Resource::Processes => {
            let a = share(&view.aterm)
                .or_else(|| shares.filter(|_| snap.aterm.is_some()).map(|s| s[1]));
            let t = share(&view.tab).or_else(|| {
                shares
                    .filter(|_| !panes.is_empty() && snap.tab_of(panes).is_some())
                    .map(|s| s[2])
            });
            (a, t)
        }
        Resource::Disk | Resource::Network => {
            let has_aterm = snap.aterm.is_some()
                || view.aterm.now.is_some()
                || snap.net.as_ref().is_some_and(|n| n.aterm_sockets.is_some());
            let has_tab = !panes.is_empty()
                && (snap.tab_of(panes).is_some()
                    || view.tab.now.is_some()
                    || snap.net.as_ref().is_some_and(|n| !n.tab_sockets.is_empty()));
            let a = shares
                .filter(|s| has_aterm || s[1] > 0.0)
                .map(|s| s[1])
                .or_else(|| share(&view.aterm));
            let t = shares
                .filter(|s| has_tab || s[2] > 0.0)
                .map(|s| s[2])
                .or_else(|| share(&view.tab));
            (a, t)
        }
        Resource::Gpu | Resource::Tokens => (None, None),
    };
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

fn spec_ix(strip: &Strip) -> usize {
    STRIP_SPECS
        .iter()
        .position(|&s| s == strip.spec)
        .unwrap_or(0)
}

/// The slots of `slots`' fine band, a range of [`MultiScaleSpec::slots`]: the boxes of its finest
/// history period, right of the live box, which a reading's slide or glide moves
/// (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §3.4); `None` with no history box.
pub(crate) fn fine_band_slots(slots: &[ProgSlot]) -> Option<std::ops::Range<usize>> {
    let start = slots.iter().position(|slot| !slot.is_live_now)?;
    let period = |slot: &ProgSlot| slot.age_hi_ms - slot.age_lo_ms;
    let p = period(&slots[start]);
    let len = slots[start..]
        .iter()
        .take_while(|slot| !slot.is_live_now && period(slot) == p)
        .count();
    Some(start..start + len)
}

/// `strip`'s fine band at `scale` ([`live::FineBand`]): its boxes' pitch, device px, as
/// [`multi_scale_slot_rects`] lays them before rounding, and their period.
pub(crate) fn fine_band(strip: &Strip, scale: f64) -> live::FineBand {
    let slots = strip.spec.slots();
    let Some(slot) = fine_band_slots(&slots).and_then(|band| slots.get(band.start)) else {
        return live::FineBand::default();
    };
    let s = scale.max(0.5);
    let px = |v: f64| (v * s + 0.5).floor();
    let total_w = (px(strip.end()) - px(strip.x)).max(1.0);
    live::FineBand {
        box_px: f64::from(slot.x1 - slot.x0) * total_w,
        period_ms: slot.age_hi_ms - slot.age_lo_ms,
    }
}

/// Whole-pixel slot layout `(slot_idx, x_px, w_px, is_live_now)` for `strip` at `scale`, newest
/// first (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §3): the live `now` box at `strip.x`,
/// then each older box to its right, the oldest ending at the horizon, `strip.end()`.
///
/// `lag_px` displaces the fine band ([`fine_band_slots`]) that many device px left, toward now,
/// as a reading's slide or glide has yet to carry it right: its newest box then passes under the
/// live box, and at its old end a box enters that shows the band's own oldest slot, never a
/// coarser box's. The painter clips the band to the span it stands in at rest, so a box is listed
/// only while some of it shows there. The live box and the coarser boxes stand at their ticks.
pub(crate) fn multi_scale_slot_rects(
    strip: &Strip,
    scale: f64,
    lag_px: f32,
) -> Vec<(usize, f32, f32, bool)> {
    let s = scale.max(0.5);
    let px = |v: f64| (v * s + 0.5).floor() as f32;
    let x0 = px(strip.x);
    let x1 = px(strip.end());
    let total_w = (x1 - x0).max(1.0);
    let gap_px = px(1.0).max(1.0);
    let slots = strip.spec.slots();
    let n = slots.len();
    let mut rest: Vec<(usize, f32, f32, bool)> = Vec::with_capacity(n + 1);
    for (j, slot) in slots.iter().enumerate() {
        let lx = (x0 + slot.x0 * total_w).round();
        let rx = if j + 1 == n {
            x1
        } else {
            (x0 + slot.x1 * total_w).round() - gap_px
        };
        rest.push((j, lx, (rx - lx).max(1.0), slot.is_live_now));
    }
    let lag = lag_px.round();
    let Some(band) = fine_band_slots(&slots).filter(|band| lag > 0.0 && !band.is_empty()) else {
        return rest;
    };
    let oldest = band.end - 1;
    let (band_x0, (_, old_x, old_w, _)) = (rest[band.start].1, rest[oldest]);
    let band_x1 = old_x + old_w;
    // At rest the box after the band's oldest starts here: the entering box comes from there.
    let after = rest.get(band.end).map_or(x1 + gap_px, |r| r.1);
    let shows = |x: f32, w: f32| x.max(band_x0) + 1.0 <= (x + w).min(band_x1);
    let mut out = Vec::with_capacity(n + 1);
    out.extend_from_slice(&rest[..band.start]);
    out.extend(
        rest[band.clone()]
            .iter()
            .map(|&(j, lx, cw, live)| (j, lx - lag, cw, live))
            .filter(|&(_, x, w, _)| shows(x, w)),
    );
    if shows(after - lag, old_w) {
        out.push((oldest, after - lag, old_w, false));
    }
    out.extend_from_slice(&rest[band.end..]);
    out
}

impl RowWords {
    fn multi_scale_slots(&self, strip: &Strip) -> &[HeatSlot] {
        self.multi_scale
            .get(spec_ix(strip))
            .map_or(&[][..], Vec::as_slice)
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
    /// The scope bar and the figure run right of the strip.
    meter: Meter,
    /// The % column where no bar column fits: each row's bar stands under its figure
    /// ([`FullPlan::bar_under`], [`BlockPlan::bar_under`]).
    under: Option<Col>,
    details: Option<Col>,
    /// Whether the table's Details say the glance's clauses (§6.8): Full wide and mid, as the
    /// refined sketches drew them, and every Folded tier (the form every window draws unless
    /// Full is chosen). Full narrow and slim say the clauses before it.
    glance: bool,
    /// The well mark's size.
    mark_pt: f64,
    /// Where a row's hit region ends.
    end: f64,
}

impl Table {
    fn of_full(p: &FullPlan) -> Self {
        let end = p
            .meter
            .cols()
            .into_iter()
            .chain(p.pct)
            .chain(p.details)
            .map(Col::end)
            .fold(p.strip.end(), f64::max);
        Self {
            rows: p.rows,
            name: p.name,
            well: p.well,
            strip: p.strip,
            pct: p.pct,
            meter: p.meter,
            under: p.bar_under(),
            details: p.details,
            glance: matches!(p.kind, FullKind::Wide | FullKind::Mid),
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
            meter: b.meter,
            under: b.bar_under(),
            details: b.details,
            glance: true,
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

    /// The row's Details clauses this table says: the glance's or the others'.
    const fn said(&self) -> usize {
        if self.glance { 0 } else { 1 }
    }
}

struct HeadState<'w> {
    /// The block whose rows it is of: 0 (Full's table, Folded's block A) or 1 (block B), as its
    /// header label names it ([`Label::State`]).
    block: u8,
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
    /// The live edge drawn ([`DeckInput::live`]) and the snapshot's wall clock, unix ms.
    live: DeckLive,
    at_ms: Option<i64>,
    /// The card's rects the live edge moves, device px (`App::splice_stats_deck` re-rasters
    /// only these), the fastest fine band of its Full strips (which glide) and of its Folded
    /// strips (which slide), and how far the dial's tip travels per degree, device px (0 with no
    /// dial).
    live_rects: Vec<(LiveLayer, Rect)>,
    glide: live::FineBand,
    slide: live::FineBand,
    needle_px_per_deg: f64,
    tab: &'a str,
    machine: &'static str,
    /// Hits by precedence: the first containing the pointer wins.
    hits_first: Vec<Hit>,
    hits_rows: Vec<Hit>,
    hits_last: Vec<Hit>,
    boxes: Boxes,
    /// The header line: handed in already planned ([`DeckStill`]) or planned by [`Self::paint`],
    /// and as planned after it (T-G14).
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
            live: input.live,
            at_ms: input.snap.map(|snap| snap.at_unix_ms),
            live_rects: Vec::new(),
            glide: live::FineBand::default(),
            slide: live::FineBand::default(),
            needle_px_per_deg: 0.0,
            tab: &input.tab_label,
            machine: input.locale.platform.machine(),
            hits_first: Vec::new(),
            hits_rows: Vec::new(),
            hits_last: Vec::new(),
            boxes: Boxes::default(),
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

    /// What the bars under the % figures of a table's first `n` rows hold, device px: a live
    /// rect a point past each, which their tween re-rasters ([`Self::under_bar`]).
    fn under_rect(&self, col: Col, rows: &Rows, n: usize) -> Rect {
        let last = u32::try_from(n.saturating_sub(1)).unwrap_or(0);
        let (y0, _) = rows.under(0);
        let (_, y1) = rows.under(last);
        self.px_rect(col.x - 1.0, y0 - 1.0, col.end() + 1.0, y1 + 1.0)
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
        self.glyphs(x, base, s, pt, bold, ink);
        w
    }

    /// A row's % figure, right-aligned in `col`. Its box ends at the baseline: the figure is
    /// digits, `<`, `%` or `–` ([`words::pct`]), none of which descends, so a bar can stand under
    /// it (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §5).
    fn figure(&mut self, col: Col, base: f64, t: &Txt, pt: f64, bold: bool, ink: Rgb) {
        let w = t.w(pt, bold);
        let x = col.end() - w;
        self.note(
            BoxKind::Text,
            [x, base - ASCENT_EM * pt, col.end(), base],
            &t.s,
            Some((col.x, col.end())),
        );
        self.glyphs(x, base, t.s.clone(), pt, bold, ink);
    }

    /// Draws `s` from `x` on `base`, without noting its box.
    fn glyphs(&mut self, x: f64, base: f64, s: String, pt: f64, bold: bool, ink: Rgb) {
        if self.draw && !s.is_empty() {
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

    /// How far `strip`'s fine band still lags its slots in this frame, whole device px toward
    /// now ([`live::lag_px`]): the step of the reading it moves for
    /// ([`live::FineBand::step_px`]) less what its slide or glide has run of it
    /// ([`DeckLive::flow`]); 0 with every box in its slot.
    fn lag(&self, strip: &Strip) -> f32 {
        self.live.flow.map_or(0.0, |flow| {
            let step = fine_band(strip, self.s).step_px(flow.opened_ms);
            live::lag_px(step, flow.ran) as f32
        })
    }

    const fn gap_ink(&self, kind: GapKind) -> Rgb {
        match kind {
            GapKind::Crash => self.inks.critical_text,
            GapKind::Sleep => self.inks.stuck,
            GapKind::Paused => self.inks.words2,
        }
    }

    const fn event_ink(&self, kind: StripEventKind) -> Rgb {
        match kind {
            StripEventKind::Crash => self.inks.critical_text,
            StripEventKind::Restart => self.inks.warn_text,
            StripEventKind::SleepWake => self.inks.stuck,
            StripEventKind::PressureSpike => self.inks.warn_text,
        }
    }

    fn strip(
        &mut self,
        strip: &Strip,
        top: f64,
        cell_h: f64,
        slots: &[HeatSlot],
        live_heat: Option<f32>,
        cap_inside: bool,
    ) {
        // A caption's history, and the row below it, keep their over mark inside the strip,
        // as a 1x strip does. An exterior cap would enter the preceding thin history band.
        let cap = (!cap_inside).then(|| self.cap_px()).flatten();
        let cap_room = cap.map_or(0.0, |(h, gap)| f64::from(h + gap) / self.s);
        self.note(
            BoxKind::Strip,
            [strip.x, top - cap_room, strip.end(), top + cell_h],
            "",
            None,
        );
        let lag = self.lag(strip);
        let rects = multi_scale_slot_rects(strip, self.s, lag);
        self.cells += rects.len();
        if !self.draw {
            return;
        }
        let (y, h) = (self.px(top), self.px(cell_h));
        let radius = if self.s < 2.0 { 0.0 } else { 1.0 };
        let live_slot = rects.iter().copied().find(|r| r.3);
        // A moving fine band draws inside the span it stands in at rest: its newest box passes
        // under the live box and the entering one comes out from its old end, so neither the
        // live box nor a coarser box ever takes a moving box's ink.
        let band = fine_band_slots(&strip.spec.slots()).unwrap_or(0..0);
        let clip = (lag != 0.0).then(|| self.fine_band_span(strip)).flatten();
        let mut clipped = false;
        for &(idx, x, cw, is_live_now) in &rects {
            if is_live_now {
                continue;
            }
            match clip {
                Some((cx0, cx1)) if band.contains(&idx) && !clipped => {
                    let h_card = self.px(self.h_pt);
                    self.p.prims.push(DrawPrim::ClipPush {
                        x: cx0,
                        y: 0.0,
                        w: cx1 - cx0,
                        h: h_card,
                    });
                    clipped = true;
                }
                _ if clipped && !band.contains(&idx) => {
                    self.p.prims.push(DrawPrim::ClipPop);
                    clipped = false;
                }
                _ => {}
            }
            let slot_data = slots.get(idx).copied().unwrap_or_default();
            if let Some(gap_kind) = slot_data.cell.gap {
                self.gap_cell((x, y, cw, h), radius, gap_kind);
                continue;
            }
            let Some(v) = slot_data.cell.heat.filter(|v| !v.is_nan()) else {
                self.hollow_cell((x, y, cw, h), radius, cw < 4.0 || h < 4.0);
                continue;
            };
            let step = ink::heat_step(v);
            let hh = if self.dwc {
                ink::dwc_height(h as u32, step) as f32
            } else {
                h
            };
            self.rect(
                (x, y + h - hh, cw, hh),
                radius,
                self.inks.cell_at(v, self.ic),
            );
            if slot_data.cell.over {
                let over = self.inks.over;
                match cap {
                    Some((ch, gap)) => self.rect((x, y - ch - gap, cw, ch), 0.0, over),
                    None => self.rect((x, y, cw, h.min(2.0)), 0.0, over),
                }
            }
        }
        if clipped {
            self.p.prims.push(DrawPrim::ClipPop);
        }
        if let Some((idx, x, cw, _)) = live_slot {
            let slot_data = slots.get(idx).copied().unwrap_or_default();
            let v_opt = live_heat.or(slot_data.cell.heat).filter(|v| !v.is_nan());
            let Some(v) = v_opt else {
                self.hollow_cell((x, y, cw, h), radius, cw < 4.0 || h < 4.0);
                return;
            };
            // The strip cell's own ink for this value (`VitalsInks::cell_at`), at every level.
            let step = ink::heat_step(v);
            let cell = self.inks.cell_at(v, self.ic);
            let fill_rgb = if self.live.fresh {
                cell
            } else {
                ink::mix(self.inks.ground, cell, 0.55)
            };
            let hh = if self.dwc {
                ink::dwc_height(h as u32, step) as f32
            } else {
                h
            };
            self.rect((x, y + h - hh, cw, hh), radius, fill_rgb);
            if slot_data.cell.over {
                let over = self.inks.over;
                match cap {
                    Some((ch, gap)) => self.rect((x, y - ch - gap, cw, ch), 0.0, over),
                    None => self.rect((x, y, cw, h.min(2.0)), 0.0, over),
                }
            }
        }
    }

    /// The device-px span `x0..x1` `strip`'s fine band stands in at rest
    /// ([`fine_band_slots`]): from its newest box's left edge to its oldest's right edge.
    fn fine_band_span(&self, strip: &Strip) -> Option<(f32, f32)> {
        let band = fine_band_slots(&strip.spec.slots())?;
        let rest = multi_scale_slot_rects(strip, self.s, 0.0);
        let (_, x0, _, _) = *rest.get(band.start)?;
        let (_, ox, ow, _) = *rest.get(band.end.checked_sub(1)?)?;
        (ox + ow > x0).then_some((x0, ox + ow))
    }

    fn gap_cell(&mut self, (x, y, w, h): Rect, radius: f32, kind: GapKind) {
        let gap_ink = self.gap_ink(kind);
        let fill = ink::mix(self.inks.ground, gap_ink, if self.ic { 0.24 } else { 0.14 });
        self.rect((x, y, w, h), radius, fill);
        if self.draw && w >= 2.0 {
            let on = (1.5 * self.s).round().max(1.0) as f32;
            let rail = rgba(gap_ink, 0xCC);
            self.p.prims.push(DrawPrim::Dashed {
                x,
                y: y + 0.5,
                w,
                h: 0.0,
                width: 1.0,
                on,
                off: on,
                color: rail,
            });
            if h >= 4.0 {
                self.p.prims.push(DrawPrim::Dashed {
                    x,
                    y: y + h - 0.5,
                    w,
                    h: 0.0,
                    width: 1.0,
                    on,
                    off: on,
                    color: rail,
                });
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

    fn strip_spans(y0: f64, y1: f64, holes: &[(f64, f64)]) -> Vec<(f64, f64)> {
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
        spans
    }

    fn gridlines(&mut self, strip: &Strip, y0: f64, y1: f64, holes: &[(f64, f64)]) {
        if !self.draw {
            return;
        }
        // Scale-band gridlines anchor to the X-axis ticks (`lag_px = 0.0`) so band changes (the
        // live box's edge, `-10s`, `-1m`, `-10m`, `-1h`) never drift off their tick marks during
        // glide/slide: one in the gap before each band's first box, the live box's band aside. A
        // band is its boxes' period, not their width: neighbouring bands can share a weight.
        let rects = multi_scale_slot_rects(strip, self.s, 0.0);
        let slots = strip.spec.slots();
        let gap_px = self.px(1.0).max(1.0);
        let x0 = self.px(strip.x);
        let spans = Self::strip_spans(y0, y1, holes);
        let grid = self.inks.grid;
        let mut prev_band: Option<(i64, bool)> = None;
        for &(idx, sx, _, is_live_now) in &rects {
            let Some(slot) = slots.get(idx) else {
                continue;
            };
            let band = (slot.age_hi_ms - slot.age_lo_ms, is_live_now);
            let boundary = prev_band.is_some_and(|prev| prev != band);
            prev_band = Some(band);
            if !boundary {
                continue;
            }
            let gx = sx - gap_px;
            if gx < x0 {
                continue;
            }
            for &(a, b) in &spans {
                let (ya, yb) = (self.px(a), self.px(b));
                self.rect((gx, ya, 1.0, yb - ya), 0.0, grid);
            }
        }
    }

    /// Draws the strip's X-axis baseline rail, discontinuity break notches over [`TimeGap`]s, and
    /// vertical event marker lines + cap pips for [`StripEvent`]s (`Crash`, `Restart`,
    /// `SleepWake`, `PressureSpike`).
    fn strip_axis_and_events(
        &mut self,
        strip: &Strip,
        base: f64,
        y0: f64,
        last: f64,
        holes: &[(f64, f64)],
        words: &DeckWords,
    ) {
        if !self.draw {
            return;
        }
        let x0 = self.px(strip.x);
        let x1 = self.px(strip.end());
        if x1 <= x0 {
            return;
        }
        let y_tick0 = self.px(base + 1.0);
        let y_tick1 = self.px(base + 2.5).max(y_tick0 + TICK_MIN_PX);
        let y_axis = (y_tick1 - 1.0).max(y_tick0);
        self.rect((x0, y_axis, x1 - x0, 1.0), 0.0, self.inks.grid);
        let Some(now_ms) = self.at_ms else {
            return;
        };
        let horizon = strip.spec.horizon_ms();
        let total_w = x1 - x0;
        for gap in words.time_gaps.iter().rev().take(4) {
            if gap.end_ms <= now_ms - horizon || gap.start_ms >= now_ms {
                continue;
            }
            let age_start = (now_ms - gap.start_ms).clamp(0, horizon);
            let age_end = (now_ms - gap.end_ms).clamp(0, horizon);
            let (Some(f0), Some(f1)) = (
                strip.spec.x_frac_of_age_ms(age_start),
                strip.spec.x_frac_of_age_ms(age_end),
            ) else {
                continue;
            };
            // The gap's start is the older end, right of its end (x grows with age).
            let (gx_old, gx_new) = (
                (x0 + f0 * total_w).round().clamp(x0, x1),
                (x0 + f1 * total_w).round().clamp(x0, x1),
            );
            let (gx0, gx1) = (gx_new.min(gx_old), gx_new.max(gx_old));
            if gx1 > gx0 {
                let gink = self.gap_ink(gap.kind);
                self.rect((gx0, y_axis, gx1 - gx0, 1.0), 0.0, gink);
                let nh = (2.0 * self.s as f32).round().max(2.0);
                self.rect((gx0, y_axis + 1.0 - nh, 1.0, nh), 0.0, gink);
                if gx1 - gx0 >= 2.0 {
                    self.rect((gx1 - 1.0, y_axis + 1.0 - nh, 1.0, nh), 0.0, gink);
                }
            }
        }
        if words.strip_events.is_empty() {
            return;
        }
        let spans = Self::strip_spans(y0, last, holes);
        let mut events: Vec<&StripEvent> = words
            .strip_events
            .iter()
            .filter(|ev| ev.at_unix_ms <= now_ms && now_ms - ev.at_unix_ms <= horizon)
            .collect();
        // Highest-severity events first (Crash > Restart > PressureSpike > SleepWake), then newest.
        events.sort_by(|a, b| {
            b.kind
                .cmp(&a.kind)
                .then_with(|| b.at_unix_ms.cmp(&a.at_unix_ms))
        });
        let mut drawn_xs: Vec<i32> = Vec::with_capacity(6);
        for ev in events {
            if drawn_xs.len() >= 6 {
                break;
            }
            let Some(x_pt) = strip.x_at_age_ms(now_ms - ev.at_unix_ms) else {
                continue;
            };
            let ex = (x_pt * self.s) as f32;
            let bucket_px = ex.round() as i32;
            if drawn_xs.iter().any(|&px| (px - bucket_px).abs() <= 1) {
                continue;
            }
            drawn_xs.push(bucket_px);
            let ev_ink = self.event_ink(ev.kind);
            let lw = (0.85 * self.s).round().max(1.0) as f32;
            let lx = (ex - lw / 2.0).round().clamp(x0, (x1 - lw).max(x0));
            let pip = (2.2 * self.s).round().max(2.0) as f32;
            let px_pip = (ex - pip / 2.0).round().clamp(x0, (x1 - pip).max(x0));
            let py_pip = (y_axis + 0.5 - pip / 2.0).round().max(0.0);
            self.rect((px_pip, py_pip, pip, pip), (0.5 * self.s) as f32, ev_ink);
            for &(a, b) in &spans {
                let (ya, yb) = (self.px(a), self.px(b));
                if yb <= ya {
                    continue;
                }
                match ev.kind {
                    StripEventKind::Crash | StripEventKind::Restart => {
                        let alpha = if ev.kind == StripEventKind::Crash {
                            0xFF
                        } else {
                            0xE0
                        };
                        self.p
                            .prims
                            .push(panel(lx, ya, lw, yb - ya, 0.0, rgba(ev_ink, alpha)));
                    }
                    StripEventKind::SleepWake | StripEventKind::PressureSpike => {
                        let on = (2.0 * self.s).round().max(2.0) as f32;
                        let off = (1.5 * self.s).round().max(1.0) as f32;
                        self.p.prims.push(DrawPrim::Dashed {
                            x: lx,
                            y: ya,
                            w: 0.0,
                            h: yb - ya,
                            width: lw,
                            on,
                            off,
                            // Historical guides stay below the data's contrast.
                            // The full-strength cap pip still marks the event;
                            // Increase Contrast retains the stronger guide.
                            color: rgba(ev_ink, if self.ic { 0xDF } else { 0x70 }),
                        });
                    }
                }
            }
        }
    }

    /// `strip`'s live edge: its fine band's rect, the `now` box's rect, and the now
    /// marker at its left end, between the % column and the live box: a line from the dot on the
    /// header line at `base` down to `last` (the strip rows' bottom), the dot beating while
    /// readings keep coming, grey and hollow when they stop. Where the header's state words take
    /// the strip's header (`dotless`, [`HeaderPlan::dotless`]) the dot yields to them and the
    /// line starts at the rows' top, `top0`.
    fn live_edge(&mut self, strip: &Strip, base: f64, top0: f64, last: f64, dotless: bool) {
        let s = self.s;
        let card_h = self.px(self.h_pt);
        if let Some((bx0, bx1)) = self.fine_band_span(strip) {
            let (gx0, gx1) = (bx0 - 1.0, bx1 + 1.0);
            self.live_rects
                .push((LiveLayer::Glide, (gx0, 0.0, gx1 - gx0, card_h)));
        }
        if let Some((_, lx, lw, _)) = multi_scale_slot_rects(strip, self.s, 0.0)
            .into_iter()
            .find(|r| r.3)
        {
            // A device px of slack each side: the box draws no halo past its own rect.
            let nx0 = (lx - 1.0).max(0.0);
            let nx1 = lx + lw + 1.0;
            let ny0 = (self.px(top0) - 6.0).max(0.0);
            let ny1 = (self.px(last) + 4.0).min(card_h);
            if nx1 > nx0 && ny1 > ny0 {
                self.live_rects
                    .push((LiveLayer::NowBox, (nx0, ny0, nx1 - nx0, ny1 - ny0)));
            }
        }
        let band = fine_band(strip, s);
        if strip.period_s == plan::FOLD_SLOT_S {
            self.slide = self.slide.faster(band);
        } else {
            self.glide = self.glide.faster(band);
        }
        let x = strip.x - live::LINE_DX;
        let cy = base - live::DOT_RISE;
        let reach = live::DOT_R + live::BEAT_REACH;
        let (rx0, rx1) = (
            ((x - reach) * s).floor() as f32 - 1.0,
            ((x + reach) * s).ceil() as f32 + 1.0,
        );
        self.live_rects
            .push((LiveLayer::Marker, (rx0, 0.0, rx1 - rx0, card_h)));
        let r_pt = live::DOT_R;
        if !dotless {
            self.note(
                BoxKind::Mark,
                [x - r_pt, cy - r_pt, x + r_pt, cy + r_pt],
                "now",
                None,
            );
        }
        if !self.draw {
            return;
        }
        let fresh = self.live.fresh;
        let ink = if fresh {
            self.inks.words
        } else {
            self.inks.hollow
        };
        let lw = (0.75 * s).round().max(1.0) as f32;
        let (cx, cyp, r) = ((x * s) as f32, (cy * s) as f32, (r_pt * s) as f32);
        let y1 = self.px(last);
        let y0 = if dotless {
            self.px(top0).min(y1)
        } else {
            (cyp + r).min(y1)
        };
        let lx = (cx - lw / 2.0).round();
        if fresh {
            // Solid words ink: the strongest neutral on the card, so "now" outranks its grey.
            self.p
                .prims
                .push(panel(lx, y0, lw, y1 - y0, 0.0, rgba(ink, 0xFF)));
        } else {
            // Dashed hollow ink: a stopped line reads as stopped by its shape as well as its
            // ink, in either theme and with motion off. A dashed band with no width is a
            // vertical dashed line `lw` px thick.
            let dash = (live::STALE_DASH * s).round().max(1.0) as f32;
            self.p.prims.push(DrawPrim::Dashed {
                x: lx,
                y: y0,
                w: 0.0,
                h: y1 - y0,
                width: lw,
                on: dash,
                off: dash,
                color: rgba(ink, 0xFF),
            });
        }
        if dotless {
            return;
        }
        let clear = [0, 0, 0, 0];
        if !fresh {
            self.p.prims.push(DrawPrim::Ring {
                cx,
                cy: cyp,
                r_outer: r,
                thickness: lw,
                track: clear,
                frac: 1.0,
                fill: rgba(ink, 0xFF),
            });
            return;
        }
        if let Some(phase) = self.live.beat {
            let (grow, alpha) = live::beat_ring(phase);
            let a = (alpha * 255.0).round().clamp(0.0, 255.0) as u8;
            if a > 0 {
                self.p.prims.push(DrawPrim::Ring {
                    cx,
                    cy: cyp,
                    r_outer: ((r_pt + grow) * s) as f32,
                    thickness: (0.9 * s).max(1.0) as f32,
                    track: clear,
                    frac: 1.0,
                    fill: rgba(ink, a),
                });
            }
        }
        self.p.prims.push(DrawPrim::Dot {
            cx,
            cy: cyp,
            r,
            color: rgba(ink, 0xFF),
        });
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
        let inset = s.round().max(1.0);
        let [gw, aw, tw] = fill.widths_px(w, inset, 0.0);
        let ah = (h - 2.0 * inset).max(3.0).min(h);
        let th_inset = if ah >= 6.0 { inset } else { 1.0 };
        let th = (ah - 2.0 * th_inset).max(1.0).min(ah);
        let (ay, ty) = if self.dwc {
            (y + h - ah, y + h - th)
        } else {
            (y + ((h - ah) * 0.5).floor(), y + ((h - th) * 0.5).floor())
        };
        // Overlapping 3-scope bars from left edge `x`: Global outermost, aterm nested middle,
        // this tab innermost core.
        if gw > 0.0 {
            self.rect((x, y, gw, h), r, self.scope_ink(Bar::Global));
        }
        if aw > 0.0 {
            self.rect(
                (x, ay, aw, ah),
                (r - 0.5).max(0.5),
                self.scope_ink(Bar::Aterm),
            );
        }
        if tw > 0.0 {
            self.rect((x, ty, tw, th), 0.5, self.scope_ink(Bar::Tab));
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

    /// A row's figure run at the end of its bar, from `col.x` (`docs/DESIGN-stats-deck-meter-
    /// 2026-10-26.md` §3): the first of its forms that fits `col` (one that does not fit is left
    /// out, never cut), and with a `state` its words after it where both fit
    /// ([`Self::run_state`]), else the header says the state.
    fn run_cell(&mut self, col: Col, base: f64, pt: f64, run: &[Txt], state: Option<&StateTxt>) {
        let room = Some((col.x, col.end()));
        let words = self.inks.words;
        let form = Self::run_form(col.w, pt, run);
        let mut x = col.x;
        if let Some(form) = form {
            x += self.text(col.x, base, form.s.clone(), pt, false, words, room) + STATE_GAP;
        }
        if let Some(st) = state
            && let Some(said) = self.run_state(col.w, pt, run, st)
        {
            let mark = Some((st.mark, self.mark_ink(st.mark, st.ink)));
            let ink = self.ink(st.ink);
            self.marked(x, base, mark, said.s.clone(), pt, true, ink, room);
        }
    }

    /// The first of a row's `run` forms that fits `w`.
    fn run_form(w: f64, pt: f64, run: &[Txt]) -> Option<&Txt> {
        run.iter().find(|f| f.w(pt, false) <= w + 0.01)
    }

    /// What a run's cell `w` wide says of a row's `st` after its reading ([`Self::run_form`]):
    /// the state's clause, else its word, where it fits beside the reading. The reading is never
    /// given up for the state, which the header says where neither fits (`None`).
    fn run_state<'t>(&self, w: f64, pt: f64, run: &[Txt], st: &'t StateTxt) -> Option<&'t Txt> {
        let used = Self::run_form(w, pt, run).map_or(0.0, |f| f.w(pt, false) + STATE_GAP);
        let madv = MARK_EM * pt;
        st.clauses
            .iter()
            .chain(std::iter::once(&st.word))
            .find(|said| used + madv + said.w(pt, true) <= w + 0.01)
    }

    /// Whether row `row` of table `t` says its state: in its Details, or after its figure run
    /// ([`Self::run_state`]); a state no row says stands in the header.
    fn row_says_state(&self, t: &Table, row: &RowWords) -> bool {
        let Some(st) = &row.state else {
            return true;
        };
        t.details.is_some()
            || t.meter.run.is_some_and(|c| {
                self.run_state(c.w, t.rows.amount_pt, &row.run, st)
                    .is_some()
            })
    }

    /// Hardware facts share a row with its live history, without consuming causes or alerts in Details.
    /// The planned columns alone choose the geometry, so changing readings cannot move a bar.
    fn capacity_col(&self, t: &Table, row: &RowWords) -> Option<Col> {
        if row.strip_text.is_some()
            || !matches!(row.res, Resource::Memory | Resource::Disk | Resource::Gpu)
        {
            return None;
        }
        let end = if row.res == Resource::Gpu || t.meter.run.is_some() {
            t.strip.end()
        } else {
            t.details.map_or(t.end, |col| col.x - 3.0)
        };
        Some(Col {
            x: t.strip.x + 3.0,
            w: (end - t.strip.x - 3.0).max(0.0),
        })
    }

    fn capacity_moves_bar(t: &Table, row: &RowWords) -> bool {
        row.strip_text.is_none()
            && matches!(row.res, Resource::Memory | Resource::Disk)
            && t.meter.run.is_none()
    }

    fn capacity_forms<'r>(t: &Table, row: &'r RowWords) -> &'r [Txt] {
        if t.meter.run.is_some() && matches!(row.res, Resource::Memory | Resource::Disk) {
            &row.available
        } else {
            &row.capacity
        }
    }

    /// A capacity caption keeps the row's full time axis as a thin live band below its text.
    /// Its height and foot depend only on the planned row and scale, never on the reading.
    fn history_band(&self, t: &Table, row: &RowWords, r: u32) -> (f64, f64) {
        if self.capacity_col(t, row).is_none() {
            return (t.rows.top(r), t.rows.cell);
        }
        let h = self.px(1.0).max(1.0);
        let y = self.px(t.rows.top(r + 1)) - 1.0 - h;
        (f64::from(y) / self.s, f64::from(h) / self.s)
    }

    /// The bar's segments a click picks a scope by, pt from its left edge: this tab's fill, and
    /// aterm's past it ([`BarFill::widths_px`], as [`Self::bar`] draws them). A scope that draws
    /// a fill takes at least [`SEGMENT_HIT_MIN`] of the bar, so a tab using little is still a
    /// target and not a pixel or two.
    fn bar_segments(&self, col: Col, fill: &BarFill) -> [(f64, f64, Bar); 2] {
        self.bar_segments_with_gap(col, fill, 0.0)
    }

    /// A moved under-bar uses the same one-device-pixel separation as its drawn fills.
    fn bar_segments_with_gap(&self, col: Col, fill: &BarFill, apart: f32) -> [(f64, f64, Bar); 2] {
        let s = self.s as f32;
        let w = self.px(col.w);
        let [_, aw, tw] = fill.widths_px(w, s.round().max(1.0), apart);
        let pt = |v: f32| f64::from(v) / self.s;
        let tab = if tw > 0.0 {
            pt(tw).max(SEGMENT_HIT_MIN).min(col.w)
        } else {
            0.0
        };
        let aterm = if aw > 0.0 {
            pt(aw).max(tab + SEGMENT_HIT_MIN).min(col.w)
        } else {
            tab
        };
        [(0.0, tab, Bar::Tab), (tab, aterm, Bar::Aterm)]
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

    /// How tall the bars under a table's % figures are drawn, device px: [`plan::UNDER_H`], or
    /// as much of it as every row holds between its line's top, [`plan::UNDER_GAP`] below the
    /// digits' baseline ([`Rows::under`]), targeting a device px short of the next row's ink
    /// while keeping a device px of height at least. One height serves every row of the table, so the lines read alike where the
    /// rows' tops round differently: at 1× in Full narrow and slim, whose 10.5 pt pitch leaves
    /// half a point past the line (§5's table), the rows on a half point have room for 1 px, and
    /// every line there is 1 px.
    fn under_h(&self, rows: &Rows) -> f32 {
        (0..rows.count)
            .map(|r| {
                let ink_top = rows.top(r + 1) + rows.baseline - ASCENT_EM * rows.name_pt;
                let foot = (ink_top * self.s).floor() as f32 - 1.0;
                foot - self.px(rows.under(r).0)
            })
            .fold(self.px(plan::UNDER_H), f32::min)
            .max(1.0)
    }

    /// A row's three-scope bar under its % figure, where no bar column fits
    /// (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §5): the % column's width from `y0`
    /// ([`Rows::under`], [`plan::UNDER_GAP`] below the digits' baseline), `h` device px tall
    /// ([`Self::under_h`]), the bar's track and nested fills (the machine's, aterm's over it,
    /// this tab's over that) in the bar's inks, each the line's whole height, so they nest by
    /// length alone: a scope above zero ends a device px or more past the fill inside it
    /// ([`BarFill::widths_px`]'s `apart`), or its ink would hide under the inner fill wherever
    /// the two round alike. Its edges fall on whole device px, a scope above zero keeps one at
    /// least, and with `h` from [`Self::under_h`] it ends a device px or more short of the next
    /// row's top, so it never meets that row's cells. It carries the fills alone: no core ticks,
    /// and no Disk warn line, which stands 1.5 pt past a bar's top and foot and would meet the
    /// digits and the next row. Notes the rect it draws, and answers it, device px: a click there
    /// opens the row's lens, as one on the bar's column does.
    fn under_bar(&mut self, col: Col, y0: f64, h: f32, fill: &BarFill) -> Rect {
        let (x, y) = (self.px(col.x), self.px(y0));
        let w = self.px(col.end()) - x;
        let pt = |v: f32| f64::from(v) / self.s;
        self.note(BoxKind::Bar, [pt(x), pt(y), pt(x + w), pt(y + h)], "", None);
        if self.draw {
            let r = (0.5 * self.s).floor() as f32;
            let [gw, aw, tw] = fill.widths_px(w, 1.0, 1.0);
            self.rect((x, y, w, h), r, self.inks.track);
            for (bw, bar) in [(gw, Bar::Global), (aw, Bar::Aterm), (tw, Bar::Tab)] {
                if bw > 0.0 {
                    self.rect((x, y, bw, h), r, self.scope_ink(bar));
                }
            }
        }
        (x, y, w, h)
    }

    fn row(
        &mut self,
        t: &Table,
        r: u32,
        row: &RowWords,
        above: Option<&RowWords>,
        prev_row: Option<(Option<f32>, Option<BarFill>)>,
        reading: bool,
    ) {
        let capacity = self.capacity_col(t, row);
        let capacity_above = above.is_some_and(|row| self.capacity_col(t, row).is_some());
        let moves_bar = Self::capacity_moves_bar(t, row);
        let capacity_forms = Self::capacity_forms(t, row);
        let mut layout = *t;
        if moves_bar {
            layout.meter = Meter::default();
            layout.under = t.pct.or(t.under);
        }
        let t = &layout;
        let rows = t.rows;
        let top = rows.top(r);
        let base = top + rows.baseline;
        let words_ink = self.inks.words;
        let blend = self.live.blend;
        let live_heat = live::lerp_opt(prev_row.and_then(|p| p.0), row.heat_now, blend);
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
                let slots = row.multi_scale_slots(&t.strip);
                let (history_top, history_h) = self.history_band(t, row, r);
                self.strip(
                    &t.strip,
                    history_top,
                    history_h,
                    slots,
                    live_heat,
                    capacity.is_some() || capacity_above,
                );
            }
        }
        if let Some(col) = t.pct {
            let (ink, bold) = match row.heat_now {
                Some(h) => (self.inks.heat_text[usize::from(ink::heat_step(h))], true),
                None => (self.inks.words2, false),
            };
            self.figure(col, base, &row.pct, rows.name_pt, bold, ink);
        }
        let blended = row
            .bar
            .map(|fill| match (prev_row.and_then(|p| p.1), blend) {
                (Some(pf), Some(b)) => BarFill {
                    global: live::lerp_opt(pf.global, fill.global, Some(b)),
                    aterm: live::lerp_opt(pf.aterm, fill.aterm, Some(b)),
                    tab: live::lerp_opt(pf.tab, fill.tab, Some(b)),
                    ticks: fill.ticks,
                    warn_tick: fill.warn_tick,
                },
                _ => fill,
            });
        if let (Some(col), Some(fill)) = (t.meter.bar, blended.as_ref()) {
            self.bar(col, top + (rows.cell - rows.bar_h) / 2.0, rows.bar_h, fill);
        }
        let under = match (t.under, blended.as_ref()) {
            (Some(col), Some(fill)) => {
                Some(self.under_bar(col, rows.under(r).0, self.under_h(&rows), fill))
            }
            _ => None,
        };
        if moves_bar && let Some(rect) = under {
            self.live_rects.push((LiveLayer::Values, rect));
        }
        if let Some(col) = t.meter.run {
            let state = row.state.as_ref().filter(|_| t.details.is_none());
            self.run_cell(col, base, rows.amount_pt, &row.run, state);
        }
        if let Some(col) = t.details {
            self.details(col, base, &row.details[t.said()]);
        }
        if let Some(col) = capacity {
            let pt = 8.5;
            let caption_base = top + ASCENT_EM * pt;
            // Alerts keep their existing Details/run/header priority; capacity never displaces one.
            self.run_cell(col, caption_base, pt, capacity_forms, None);
        }
        if reading {
            return;
        }
        let (y0, y1) = t.band(r);
        let inst = Instrument::of(row.res);
        // A click on this tab's or aterm's segment of the bar opens the row's lens on that scope;
        // the rest of the row opens it on the machine's.
        if let (Some(col), Some(fill)) = (t.meter.bar, row.bar.as_ref()) {
            for (x0, x1, bar) in self.bar_segments(col, fill) {
                if x1 > x0 {
                    let rect = self.px_rect(col.x + x0, y0, col.x + x1, y1);
                    self.hits_first.push(Hit::row(rect, inst, bar));
                }
            }
        }
        // The bar under the % reaches past the row's band into the gap before the next row: it is
        // still this row's, as the bar's column is.
        if let Some(rect) = under {
            if moves_bar && let Some(fill) = blended.as_ref() {
                let col = Col {
                    x: f64::from(rect.0) / self.s,
                    w: f64::from(rect.2) / self.s,
                };
                for (x0, x1, bar) in self.bar_segments_with_gap(col, fill, 1.0) {
                    if x1 > x0 {
                        let from = self.px(x0);
                        let scoped = (rect.0 + from, rect.1, self.px(x1) - from, rect.3);
                        self.hits_first.push(Hit::row(scoped, inst, bar));
                    }
                }
            }
            self.hits_first.push(Hit::row(rect, inst, Bar::Global));
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
            Some(v) => self.rect(rect, radius, self.inks.cell_at(v, self.ic)),
            None => self.hollow_cell(rect, radius, side < 4.0),
        }
    }

    /// The strain gauge in Full's slot left of the row names (wide and mid), centred on the six
    /// rows ([`crate::stats_gauge`]).
    fn gauge(&mut self, at: Col, rows: &Rows, words: &DeckWords) {
        let cx = at.x + at.w / 2.0;
        let last = DECK_RESOURCES.len() as u32 - 1;
        let cy = (rows.top(0) + rows.top(last) + rows.cell) / 2.0;
        let slot = [at.x, SLOT_TOP, at.end(), self.h_pt - SLOT_FOOT];
        self.dial((cx, cy), gauge::R_FACE, slot, words);
        let text = words.cpu_total.map_or_else(
            || "cores unknown".to_owned(),
            |n| format!("{n} {}", if n == 1 { "core" } else { "cores" }),
        );
        let pt = SMALL_PT.min(SMALL_PT * (at.w - 4.0) / self.m.width(&text, SMALL_PT, false));
        let width = self.m.width(&text, pt, false);
        let base = (cy + gauge::R_FACE + SMALL_PT).min(slot[3] - DESCENT_EM * pt);
        self.text(
            cx - width / 2.0,
            base,
            text,
            pt,
            false,
            self.inks.words2,
            Some((at.x, at.end())),
        );
    }

    /// The strain gauge at the Folded deck's top left, as large as the band holds, its bloom
    /// kept inside the band (short of the seam) and left of its divider.
    fn folded_gauge(&mut self, g: &FoldedGauge, words: &DeckWords) {
        let slot = [0.0, 0.0, g.divider, self.h_pt - 1.0];
        self.dial(g.centre(), FOLD_GAUGE_R, slot, words);
    }

    /// The dial of radius `r` pt at `centre`, its bloom clipped to `slot` (pt, `x0, y0, x1, y1`).
    /// It draws the reading, or the needle in motion where the live edge carries one
    /// ([`DeckLive::needle`]); its slot is the live edge's [`LiveLayer::Gauge`] rect. A dial that
    /// reads hot steams (`crate::stats_steam`) at the live edge's steam clock
    /// ([`DeckLive::steam`]), and its slot is then a [`LiveLayer::Steam`] rect too.
    fn dial(&mut self, (cx, cy): (f64, f64), r: f64, slot: [f64; 4], words: &DeckWords) {
        self.note(BoxKind::Gauge, [cx - r, cy - r, cx + r, cy + r], "", None);
        let (x, y, w, h) = self.px_rect(slot[0], slot[1], slot[2], slot[3]);
        self.live_rects.push((LiveLayer::Gauge, (x, y, w, h)));
        self.needle_px_per_deg = self.needle_px_per_deg.max(gauge::tip_px_per_deg(r, self.s));
        let read = words.strain.as_ref().map_or(Gauge::UNREAD, Gauge::of);
        // The reading's strength, not the swinging needle's: a frame draws steam exactly where
        // its layout laid the steam's rect.
        let steam_k = steam::strength(read.needle);
        if steam_k > 0.0 {
            self.live_rects.push((LiveLayer::Steam, (x, y, w, h)));
        }
        if !self.draw {
            return;
        }
        let g = match self.live.needle {
            Some((deg, soak)) if read.needle.is_some() => Gauge::at_deg(
                f64::from(deg) / crate::stats_needle::ANGLE_PER_DEG,
                (f64::from(soak) / crate::stats_needle::SOAK_PER_ONE) as f32,
            ),
            _ => read,
        };
        let heat = if self.ic {
            self.inks.heat_ic
        } else {
            self.inks.heat
        };
        let inks = GaugeInks::new(&self.inks, heat, g.soak);
        let place = gauge::Place {
            centre: (self.px(cx), self.px(cy)),
            slot: [x, y, x + w, y + h],
            s: self.s,
            r,
        };
        gauge::draw(g, place, &inks, &mut self.p.prims);
        if let Some(t_ms) = self.live.steam {
            steam::draw(
                steam_k,
                t_ms,
                place,
                steam::ink(&self.inks),
                &mut self.p.prims,
            );
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
        let c_rect = self.px_rect(room.x - 2.0, top - 2.0, room.end() + 2.0, top + cell + 2.0);
        self.live_rects.push((LiveLayer::Values, c_rect));
        let radius = self.s.round().max(1.0) as f32;
        let blend = self.live.blend;
        let mut x = room.x;
        for (ci, (cluster, cc)) in clusters.iter().zip(&words.cores).enumerate() {
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
            let prev_cluster = words.prev.as_ref().and_then(|p| p.cores.get(ci));
            for (i, &heat) in cc.heat.iter().enumerate() {
                let prev_h = prev_cluster.and_then(|pc| pc.get(i)).copied().flatten();
                let blended = live::lerp_opt(prev_h, heat, blend);
                self.core(&grid, (i / per_col, i % per_col), blended, r);
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
        if let Some((tail, tw)) = words.cores_tail_forms.iter().find_map(|t| {
            let tw = self.m.width(&t.s, caption_pt, false);
            (x + tw <= room.end()).then_some((t, tw))
        }) {
            self.text(
                room.end() - tw,
                base,
                tail.s.clone(),
                caption_pt,
                false,
                words2,
                within,
            );
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
                    self.text(x, base, label.text.clone(), SMALL_PT, false, words2, None);
                }
                Label::Details => self.two_inks(label.x, base, &label.text, sep),
                Label::State(block) => {
                    let Some(st) = states.iter().find(|st| st.block == block) else {
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
                    let hit_h = if base == HEADER_BASE_FULL {
                        HEADER_HIT_FULL
                    } else {
                        HEADER_HIT_FOLDED
                    };
                    let rect = self.px_rect(label.x, 0.0, label.x + w, hit_h);
                    self.hits_first
                        .push(Hit::row(rect, Instrument::of(st.res), Bar::Global));
                }
            }
        }
        for &(item, x) in &head.key.placed {
            self.key(item, x, base, small);
        }
        for tick in &head.ruler {
            self.text(
                tick.left,
                base,
                tick.text.clone(),
                SMALL_PT,
                false,
                words2,
                None,
            );
            self.tick_mark(tick.x, base);
        }
        if let Some(x) = head.time_tick {
            self.tick_mark(x, base);
        }
    }

    /// A time-scale tick at `x` pt under the header line at `base`: one device px wide (half a
    /// point at 2×), from a point under the baseline to 2.5 pt under it and never under
    /// [`TICK_MIN_PX`] tall, so at 1× it reads as a tick and not a stray dot; it ends short of
    /// the strip's top (and its caps) in both forms.
    fn tick_mark(&mut self, x: f64, base: f64) {
        let w = (0.5 * self.s).round().max(1.0) as f32;
        let y0 = self.px(base + 1.0);
        let y1 = self.px(base + 2.5).max(y0 + TICK_MIN_PX);
        let x = ((x * self.s) as f32 - w / 2.0).round();
        let words2 = self.inks.words2;
        self.rect((x, y0, w, y1 - y0), 0.0, words2);
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
        let text = HeaderText {
            tab: self.tab,
            machine: self.machine,
            over: words.over_shown(&plan.strip),
            totals: &words.totals,
        };
        let t = Table::of_full(&plan);
        let unsaid: Vec<&RowWords> = words
            .rows
            .iter()
            .filter(|row| !self.row_says_state(&t, row))
            .collect();
        let state = block_state(&unsaid, 0);
        let label = state.as_ref().map(|st| StateLabel {
            full: &st.full,
            bare: &st.st.label,
        });
        let head = self
            .head
            .take()
            .unwrap_or_else(|| planned_header(|| plan::header_full(&plan, text, label, self.m)));
        let rows = plan.rows;
        let gap = rows.pitch - rows.cell;
        let holes: Vec<(f64, f64)> = words
            .rows
            .iter()
            .zip(0_u32..)
            .filter_map(|(row, r)| {
                let end = if row.strip_text.is_some() {
                    rows.top(r) + rows.cell + gap / 2.0
                } else if self.capacity_col(&t, row).is_some() {
                    self.history_band(&t, row, r).0
                } else {
                    return None;
                };
                let start = rows.top(r) - gap / 2.0;
                let start = r
                    .checked_sub(1)
                    .and_then(|r| {
                        words.rows.get(r as usize).and_then(|above| {
                            above.strip_text.is_none().then(|| {
                                let (top, height) = self.history_band(&t, above, r);
                                top + height
                            })
                        })
                    })
                    .map_or(start, |foot| start.max(foot));
                Some((start, end))
            })
            .collect();
        let last = rows.top(DECK_RESOURCES.len() as u32 - 1) + rows.cell;
        self.gridlines(&plan.strip, rows.top(0), last, &holes);
        let mut cores_row_hit = None;
        match plan.cores {
            CoresAt::Block { at, divider } => {
                self.gauge(at, &rows, words);
                let w = (0.5 * self.s).round().max(1.0) as f32;
                let (y0, y1) = (self.px(SLOT_TOP), self.px(self.h_pt - SLOT_FOOT));
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
                let mode = plan
                    .cores_mode(&words.clusters(), self.m)
                    .unwrap_or(CoresMode::CountOnly);
                self.cores_row(room, top, rows.cell, mode, SMALL_PT, words);
                if !words.reading {
                    let (y0, y1) = t.band(r);
                    cores_row_hit = Some(self.px_rect(plan.name.x, y0, room.end(), y1));
                }
            }
        }
        if let Some(bar) = plan.meter.bar {
            let br = self.px_rect(bar.x - 2.0, rows.top(0) - 2.0, bar.end() + 2.0, last + 2.0);
            self.live_rects.push((LiveLayer::Values, br));
        }
        if let Some(col) = plan.bar_under() {
            let rect = self.under_rect(col, &rows, words.rows.len());
            self.live_rects.push((LiveLayer::Values, rect));
        }
        for (row, r) in words.rows.iter().zip(0_u32..) {
            let above = r.checked_sub(1).and_then(|r| words.rows.get(r as usize));
            let prev_row = words
                .prev
                .as_ref()
                .and_then(|p| p.rows.get(r as usize))
                .copied();
            self.row(&t, r, row, above, prev_row, words.reading);
        }
        // After the rows: the foot of a bar under the last row's % reaches the Cores row's band.
        if let Some(rect) = cores_row_hit {
            self.hits_first.push(Hit::cores(rect));
        }
        self.strip_axis_and_events(
            &plan.strip,
            HEADER_BASE_FULL,
            rows.top(0),
            last,
            &holes,
            words,
        );
        let dotless = head.dotless == Some(plan.strip.x);
        self.live_edge(&plan.strip, HEADER_BASE_FULL, rows.top(0), last, dotless);
        let small = self.small_cells(&plan.strip, rows.cell);
        self.header(&head, HEADER_BASE_FULL, small, state.as_slice());
        self.head = Some(head);
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
        let states: Vec<HeadState<'_>> = plan
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(bi, block)| {
                let t = Table::of_block(block);
                let unsaid: Vec<&RowWords> = words
                    .rows
                    .iter()
                    .skip(bi * 4)
                    .take(4)
                    .filter(|row| !self.row_says_state(&t, row))
                    .collect();
                block_state(&unsaid, u8::try_from(bi).unwrap_or(1))
            })
            .collect();
        let text = HeaderText {
            tab: self.tab,
            machine: self.machine,
            over: plan
                .blocks
                .first()
                .is_some_and(|b| words.over_shown(&b.strip)),
            totals: &words.totals,
        };
        let labels: [Option<StateLabel<'_>>; 2] = [0, 1].map(|bi| {
            states
                .iter()
                .find(|st| st.block == bi)
                .map(|st| StateLabel {
                    full: &st.full,
                    bare: &st.st.label,
                })
        });
        let head = self
            .head
            .take()
            .unwrap_or_else(|| planned_header(|| plan::header_folded(&plan, text, labels, self.m)));
        let w = (0.5 * self.s).round().max(1.0) as f32;
        let (y0, y1) = (self.px(4.0), self.px(self.h_pt - 6.0));
        let grid = self.inks.grid;
        self.rect((self.px(plan.divider), y0, w, y1 - y0), 0.0, grid);
        if let Some(g) = plan.gauge {
            self.folded_gauge(&g, words);
            self.rect((self.px(g.divider), y0, w, y1 - y0), 0.0, grid);
            if !words.reading {
                let rect = self.px_rect(g.at.x, 0.0, g.at.end(), self.h_pt - 6.0);
                self.hits_first.push(Hit::cores(rect));
            }
        }
        let clusters = words.clusters();
        for (bi, block) in plan.blocks.iter().enumerate() {
            let t = Table::of_block(block);
            let rows = block.rows;
            let ours: Vec<&RowWords> = words.rows.iter().skip(bi * 4).take(4).collect();
            let gap = 1.5;
            let holes: Vec<(f64, f64)> = ours
                .iter()
                .zip(0_u32..)
                .filter_map(|(row, r)| {
                    let end = if row.strip_text.is_some() {
                        rows.top(r) + rows.cell + gap
                    } else if self.capacity_col(&t, row).is_some() {
                        self.history_band(&t, row, r).0
                    } else {
                        return None;
                    };
                    let start = rows.top(r) - gap;
                    let start = r
                        .checked_sub(1)
                        .and_then(|r| {
                            ours.get(r as usize).and_then(|above| {
                                above.strip_text.is_none().then(|| {
                                    let (top, height) = self.history_band(&t, above, r);
                                    top + height
                                })
                            })
                        })
                        .map_or(start, |foot| start.max(foot));
                    Some((start, end))
                })
                .collect();
            let last = ours.last().map_or(rows.top(0), |row| {
                let r = ours.len().saturating_sub(1) as u32;
                let (top, height) = self.history_band(&t, row, r);
                top + height
            });
            self.gridlines(&block.strip, rows.top(0), last, &holes);
            if let Some(bar) = block.meter.bar {
                let br = self.px_rect(bar.x - 2.0, rows.top(0) - 2.0, bar.end() + 2.0, last + 2.0);
                self.live_rects.push((LiveLayer::Values, br));
            }
            if let Some(col) = block.bar_under() {
                let rect = self.under_rect(col, &rows, ours.len());
                self.live_rects.push((LiveLayer::Values, rect));
            }
            for (row, r) in ours.iter().zip(0_u32..) {
                let row_ix = bi * 4 + r as usize;
                let above = r.checked_sub(1).and_then(|r| ours.get(r as usize)).copied();
                let prev_row = words
                    .prev
                    .as_ref()
                    .and_then(|p| p.rows.get(row_ix))
                    .copied();
                self.row(&t, r, row, above, prev_row, words.reading);
            }
            self.strip_axis_and_events(
                &block.strip,
                HEADER_BASE_FOLDED,
                rows.top(0),
                last,
                &holes,
                words,
            );
            let dotless = head.dotless == Some(block.strip.x);
            self.live_edge(&block.strip, HEADER_BASE_FOLDED, rows.top(0), last, dotless);
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
        let small = plan
            .blocks
            .first()
            .is_some_and(|b| self.small_cells(&b.strip, b.rows.cell));
        let states = if words.reading { Vec::new() } else { states };
        self.header(&head, HEADER_BASE_FOLDED, small, &states);
        self.head = Some(head);
        if !words.reading {
            let rect = self.px_rect(0.0, 0.0, self.w_pt, HEADER_HIT_FOLDED);
            self.hits_last.push(Hit::header(rect));
        }
    }

    fn paint(&mut self, words: &DeckWords, form: DeckForm) {
        // The Deck places its own pixels: its strips glide by fractions of one, which the
        // raster's device grid would turn into steps.
        if self.draw {
            self.p.prims.push(DrawPrim::SubpixelPush);
        }
        match form {
            DeckForm::Full => self.full(words),
            DeckForm::Folded => self.folded(words),
        }
        if self.draw {
            self.p.prims.push(DrawPrim::SubpixelPop);
        }
    }

    fn hits(&mut self) -> Vec<Hit> {
        let mut hits = std::mem::take(&mut self.hits_first);
        hits.append(&mut self.hits_rows);
        hits.append(&mut self.hits_last);
        hits
    }
}

fn block_state<'w>(rows: &[&'w RowWords], block: u8) -> Option<HeadState<'w>> {
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
        block,
        res: row.res,
        st,
        full: if more > 0 {
            format!("{} +{more}", st.label)
        } else {
            st.label.clone()
        },
    })
}

/// What a layout says of its live edge ([`crate::stats_live`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LiveOut {
    /// The card's rects the live edge moves, device px, and what moves each.
    pub(crate) rects: Vec<(LiveLayer, Rect)>,
    /// The fastest fine band of its Full strips, which glide with the wall clock (`Flow::Glide`);
    /// the default with no Full strip.
    pub(crate) glide: live::FineBand,
    /// The fastest fine band of its Folded strips, which slide over the frames after a reading
    /// that opens a slot (`Flow::Slide`); the default with no Folded strip.
    pub(crate) slide: live::FineBand,
    /// How far the dial's tip travels per degree, device px: one device pixel of the needle in
    /// motion (`stats_needle::View`); 0 with no dial.
    pub(crate) needle_px_per_deg: f64,
}

/// What the Deck's layout reads that its live edge never moves: the snapshot's words and the
/// header line as planned. A frame that moves only the live edge ([`crate::stats_live`]) lays
/// the deck out again over these, so it measures no words and plans no header line; a change
/// to anything else the layout reads changes their key and reads them again.
#[derive(Debug, Default)]
pub(crate) struct DeckStill {
    key: u64,
    words: Option<DeckWords>,
    head: Option<HeaderPlan>,
}

impl DeckStill {
    /// These for a layout whose inputs, but for the live edge, hash to `key`: kept when they
    /// were read for `key`, else dropped to be read again.
    pub(crate) fn at(&mut self, key: u64) -> &mut Self {
        if self.key != key {
            *self = Self {
                key,
                ..Self::default()
            };
        }
        self
    }
}

#[cfg(test)]
thread_local! {
    /// This thread's deck layouts, the words they read and the header lines they planned
    /// (test-only).
    static LAYOUT_WORK: std::cell::Cell<[u64; 3]> = const { std::cell::Cell::new([0; 3]) };
}

/// This thread's deck layouts ([`deck_paint_kept`]) so far, the snapshot words they read and the
/// header lines they planned (test-only): a frame's layout work is the difference across it.
/// Unix only, like the App-level live-deck tests in `stats_live_tests.rs` that read it.
#[cfg(all(test, unix))]
pub(crate) fn layout_work() -> [u64; 3] {
    LAYOUT_WORK.with(std::cell::Cell::get)
}

fn note_layout_work(_part: usize) {
    #[cfg(test)]
    LAYOUT_WORK.with(|work| {
        let mut w = work.get();
        w[_part] += 1;
        work.set(w);
    });
}

/// `plan()`, the header line planned (counted for [`layout_work`]).
fn planned_header(plan: impl FnOnce() -> HeaderPlan) -> HeaderPlan {
    note_layout_work(2);
    plan()
}

/// Lay the Deck out and paint it (the tests' form of [`deck_paint_live`]).
#[cfg(test)]
pub(crate) fn deck_paint(g: &DeckGeom, input: &DeckInput<'_>) -> (Vec<DrawPrim>, Vec<Hit>) {
    let (prims, hits, _) = deck_paint_live(g, input);
    (prims, hits)
}

/// Lay the Deck out and paint it, with what the layout says of its live edge.
#[cfg(test)]
pub(crate) fn deck_paint_live(
    g: &DeckGeom,
    input: &DeckInput<'_>,
) -> (Vec<DrawPrim>, Vec<Hit>, LiveOut) {
    deck_paint_kept(g, input, &mut DeckStill::default())
}

/// Lay the Deck out and paint it, with what the layout says of its live edge, over the words
/// and header line `still` keeps (read into it when it has none).
pub(crate) fn deck_paint_kept(
    g: &DeckGeom,
    input: &DeckInput<'_>,
    still: &mut DeckStill,
) -> (Vec<DrawPrim>, Vec<Hit>, LiveOut) {
    note_layout_work(0);
    let m = deck_metrics();
    let words = still.words.get_or_insert_with(|| {
        note_layout_work(1);
        DeckWords::new(input, &m)
    });
    let mut resource_geom = *g;
    let network_height = input.network.map_or(0.0, |_| {
        crate::network_deck::height_px(f64::from(g.scale)) as f32
    });
    resource_geom.h = (g.h - network_height).max(0.0);
    let mut deck = Deck::new(&resource_geom, input, &m, true);
    deck.head = still.head.take();
    deck.paint(words, input.form);
    still.head = deck.head.take();
    let mut hits = deck.hits();
    if let Some(snapshot) = input.network {
        let mut network_inks = input.inks;
        if input.contrast {
            network_inks.hollow = network_inks.words2;
            network_inks.machine = network_inks.machine_ic;
            network_inks.aterm = network_inks.aterm_ic;
        }
        let (prims, network_hits) = crate::network_deck::paint_with_inks(
            g,
            resource_geom.h,
            snapshot,
            &network_inks,
            input.dwc,
        );
        deck.p.prims.extend(prims);
        hits.extend(network_hits);
    }
    let out = LiveOut {
        rects: std::mem::take(&mut deck.live_rects),
        glide: deck.glide,
        slide: deck.slide,
        needle_px_per_deg: deck.needle_px_per_deg,
    };
    (deck.p.prims, hits, out)
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
    /// The live edge this frame draws ([`crate::App::deck_live_now`]); still for the sill.
    pub(crate) live: DeckLive,
    /// What it draws of the dial's needle ([`crate::stats_live::LiveFacts::needle_at`]), which
    /// its layout keeps as what the window drew; `None` with no needle and for the sill.
    pub(crate) needle: Option<crate::stats_needle::Drawn>,
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

    /// Geometry for the network close-up when the resource deck itself is hidden.
    pub(crate) fn network_lens_frame(&self, wid: crate::WindowId) -> Option<DeckFrame> {
        let ws = self.windows.get(&wid)?;
        let (cw, _) = self.win_cell_size(wid);
        let pad = self.win_pad(wid);
        let preferences = crate::native_appearance::current_preferences();
        let palette = self.chrome_palette(crate::app_render::ChromeSurface::Header);
        let band = crate::chrome_band::band_colors(palette);
        let inks = cached_inks(band.bar_bg, band.value, band.accent);
        Some(DeckFrame {
            form: StatsBars::Off,
            plate: None,
            geom: DeckGeom {
                w: (ws.cols as usize * cw + 2 * pad) as f32,
                h: 0.0,
                pad: pad as f32,
                scale: ws.scale as f32,
                font_px: self.win_font_px(wid),
            },
            dy: 0,
            tab_panes: self.deck_tab_panes(wid),
            tab_label: self.deck_tab(wid).1,
            tab_count: ws.tab_set.tabs().len(),
            inks,
            lens_inks: inks,
            dwc: preferences.differentiate_without_color,
            deck: ws.deck_form,
            contrast: preferences.high_contrast,
            live: DeckLive::STILL,
            needle: None,
        })
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
                let (live, needle) = self.deck_live_now(wid);
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
                    live,
                    needle,
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
                    live: DeckLive::STILL,
                    needle: None,
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
        self.network_snapshot.revision.hash(&mut h);
        self.network_snapshot.network.revision.hash(&mut h);
        std::mem::discriminant(&self.network_snapshot.network.visibility).hash(&mut h);
        self.network_snapshot.network.detail.hash(&mut h);
        self.network_snapshot.network.via_local_process.hash(&mut h);
        self.network_snapshot.network.retention_limited.hash(&mut h);
        crate::network_deck::hash_peer_reachability(&self.network_snapshot, &mut h);
        self.network_snapshot.services.hash(&mut h);
        self.vitals
            .prev_snapshot
            .as_ref()
            .map(|s| s.seq)
            .hash(&mut h);
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

    /// What frame `f`'s card shows: its inputs and the App's reading.
    pub(crate) fn deck_input(&self, f: &DeckFrame) -> DeckInput<'_> {
        DeckInput {
            network: (f.form == StatsBars::Deck).then_some(self.network_snapshot.as_ref()),
            network_target: crate::network_deck::Target::Overview,
            snap: self.vitals.snapshot.as_deref(),
            prev_snap: self.vitals.prev_snapshot.as_deref(),
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
            live: f.live,
        }
    }

    /// Lay out frame `f`'s card, whose key is `key` (`base` without the live edge), and keep
    /// what the layout says of it: its fingerprint, hits and live edge.
    fn deck_layout(
        &self,
        wid: crate::WindowId,
        f: &DeckFrame,
        (base, key): (u64, u64),
    ) -> Vec<DrawPrim> {
        let input = self.deck_input(f);
        let (prims, hits, out) = match f.form {
            StatsBars::Sill => {
                let (prims, hits) = crate::stats_sill::sill_paint(&f.geom, &input);
                (prims, hits, LiveOut::default())
            }
            _ => match self.windows.get(&wid) {
                Some(ws) => {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    base.hash(&mut h);
                    input.locale.utc_offset_s.hash(&mut h);
                    input.locale.platform.hash(&mut h);
                    let mut still = ws.deck_still.borrow_mut();
                    deck_paint_kept(&f.geom, &input, still.at(h.finish() | 1))
                }
                None => deck_paint_kept(&f.geom, &input, &mut DeckStill::default()),
            },
        };
        if let Some(ws) = self.windows.get(&wid) {
            ws.deck_memo
                .set((key, prims_fp(&prims) ^ u64::from(f.dy).rotate_left(40)));
            *ws.stats_hits.borrow_mut() = hits;
            ws.deck_live_drawn.set(Some(f.live.key()));
            ws.deck_glide.set((out.glide, out.slide));
            *ws.deck_live_rects.borrow_mut() = out.rects;
            ws.deck_needle_drawn.set(f.needle);
            // A dial that appears, leaves or changes size draws the reading at once: the needle
            // rests on its target (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §4.2).
            if ws.deck_needle_px.replace(out.needle_px_per_deg) != out.needle_px_per_deg
                && let Some(needle) = self.vitals.needle.get()
            {
                self.vitals
                    .needle
                    .set(Some(needle.snapped(std::time::Instant::now())));
                crate::metrics::note_needle_episode(0);
            }
        }
        prims
    }

    /// The deck card's key for frame `f`: what its layout reads ([`Self::deck_input_key`]) and
    /// its live edge, never `0`.
    fn deck_key(&self, f: &DeckFrame) -> (u64, u64) {
        let base = self.deck_input_key(f);
        (base, (base ^ f.live.key().rotate_left(17)) | 1)
    }

    /// The stats card's fingerprint for `wid` (`0` when it shows none), the `RepaintKey`
    /// term.
    pub(crate) fn stats_deck_fp(&self, wid: crate::WindowId) -> u64 {
        let lens = self.stats_lens_fp(wid);
        let Some(f) = self.deck_frame(wid) else {
            return lens;
        };
        let keys = self.deck_key(&f);
        if let Some((k, fp)) = self.windows.get(&wid).map(|ws| ws.deck_memo.get())
            && k == keys.1
        {
            return fp ^ lens;
        }
        let prims = self.deck_layout(wid, &f, keys);
        let Some(ws) = self.windows.get(&wid) else {
            return lens;
        };
        // The frame that asked for the fingerprint rasters this layout next.
        *ws.deck_laid.borrow_mut() = Some(DeckLaid {
            base: keys.0,
            key: keys.1,
            live: f.live,
            prims,
        });
        ws.deck_memo.get().1 ^ lens
    }

    /// Rasterize the stats card into `wid`'s paint-only `deck_card`, again only when its prims
    /// or placement moved, and the lens into its own card. The layout the frame's fingerprint
    /// made ([`Self::stats_deck_fp`]) is the one rastered, its live edge with it (a beat that
    /// ticked over since is the next frame's, as `service_stats_live` asks), so the deck is laid
    /// out once a frame. A frame that moved only the live edge re-rasters only the card's rects
    /// it moved ([`patch_live`]).
    pub(crate) fn splice_stats_deck(&mut self, wid: crate::WindowId) {
        self.splice_stats_lens(wid);
        let Some(f) = self.deck_frame(wid) else {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.deck_card = None;
                ws.deck_fold = None;
                ws.stats_hits.borrow_mut().clear();
                ws.deck_live_drawn.set(None);
                ws.deck_needle_px.set(0.0);
                ws.deck_needle_drawn.set(None);
                // A card built again is laid out again, so its hits and live edge come back.
                *ws.deck_laid.borrow_mut() = None;
            }
            return;
        };
        let (base, key) = self.deck_key(&f);
        let laid = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.deck_laid.borrow_mut().take())
            .filter(|laid| laid.base == base);
        let (key, live) = laid.as_ref().map_or((key, f.live), |l| (l.key, l.live));
        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.deck_card.as_ref())
            .is_some_and(|c| c.geom == key)
        {
            return;
        }
        let prims = match laid {
            Some(laid) => laid.prims,
            None => self.deck_layout(wid, &f, (base, key)),
        };
        let fp = self.windows.get(&wid).map_or(0, |ws| ws.deck_memo.get().1);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let (card_base, card_live) = ws.deck_live_card.get();
        if let Some(card) = ws.deck_card.as_mut()
            && card.fp == fp
        {
            card.geom = key;
            ws.deck_live_card.set((base, live));
            return;
        }
        if let Some(card) = ws.deck_card.as_mut()
            && card_base == base
            && card.dy == f.dy
            && patch_live(
                card,
                &prims,
                &ws.deck_live_rects.borrow(),
                &card_live,
                &live,
            )
        {
            card.fp = fp;
            card.geom = key;
            card.raster_fingerprint_once = std::sync::OnceLock::new();
            ws.deck_live_card.set((base, live));
            return;
        }
        ws.deck_live_card.set((base, live));
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

/// A deck card's layout, kept from the frame's fingerprint for the splice that rasters it
/// ([`crate::App::splice_stats_deck`]): the layout's key, `base` without its live edge, the live
/// edge it drew, and its prims.
pub(crate) struct DeckLaid {
    base: u64,
    key: u64,
    live: DeckLive,
    prims: Vec<DrawPrim>,
}

/// Re-raster into `card` only the rects of `rects` whose layer moved from `was` to `now`
/// ([`LiveLayer::moved`]), from the frame's whole `prims`: each rect's pixels are what the whole
/// raster would give there, since every prim is drawn into it, clipped. `false` (and `card`
/// untouched) when a rect leaves the card, so the caller rasters it whole.
pub(crate) fn patch_live(
    card: &mut crate::SettingsCard,
    prims: &[DrawPrim],
    rects: &[(LiveLayer, Rect)],
    was: &DeckLive,
    now: &DeckLive,
) -> bool {
    let (pw, ph) = (card.pw, card.ph);
    let mut regions = Vec::new();
    for &(layer, (x, y, w, h)) in rects {
        if !layer.moved(was, now) {
            continue;
        }
        let x0 = x.floor().max(0.0) as u32;
        let y0 = y.floor().max(0.0) as u32;
        let x1 = ((x + w).ceil().max(0.0) as u32).min(pw);
        let y1 = ((y + h).ceil().max(0.0) as u32).min(ph);
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let region = crate::tray_raster::RasterRect {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        };
        // The dial's slot moves with its needle and its steam: raster it once.
        if !regions.contains(&region) {
            regions.push(region);
        }
    }
    let stride = pw as usize * 4;
    if card.rgba.len() != stride * ph as usize {
        return false;
    }
    for r in regions {
        let px = crate::tray_raster::rasterize_tray_region(prims, 1.0, [0, 0, 0, 0], r);
        let row = r.width as usize * 4;
        if px.len() != row * r.height as usize {
            return false;
        }
        for (i, src) in px.chunks_exact(row).enumerate() {
            let at = (r.y as usize + i) * stride + r.x as usize * 4;
            let Some(dst) = card.rgba.get_mut(at..at + row) else {
                return false;
            };
            dst.copy_from_slice(src);
        }
    }
    true
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
const A11Y_TITLE: &str = "Stats deck, newest first";

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

/// The strain gauge's name to a screen reader (§12): the gauge draws [`Strain`] and no word.
#[cfg(a11y_tree)]
const A11Y_GAUGE: &str = "Strain";

/// The strain gauge as a reader meets it (§12): the needle as the meter's value (past capacity
/// reads 100), then in words the needle now and the bezel's hour, as the dial draws them
/// ([`gauge::soak_words`]). Every group has it, as every group has the cores, whether or not a
/// dial is drawn (a deck too narrow for one, or the Network lens alone).
#[cfg(a11y_tree)]
fn gauge_meter(snap: &Snapshot) -> crate::a11y_tree::StatsMeter {
    let meter = |value: Option<f32>, words: String| crate::a11y_tree::StatsMeter {
        name: A11Y_GAUGE.into(),
        value,
        words,
    };
    if snap.seq == 0 {
        return meter(None, format!("{A11Y_GAUGE}: reading."));
    }
    let g = Gauge::of(&Strain::of(snap));
    let (value, mut said) = match g.needle {
        None => (None, format!("{A11Y_GAUGE}: not read.")),
        Some(n) if n > 1.0 => (Some(100.0), format!("{A11Y_GAUGE} past capacity now.")),
        Some(n) => {
            let pct = words::pct(Some(n));
            let spoken = match pct.strip_suffix('%') {
                Some("<1") => "less than 1 percent".to_owned(),
                Some(p) => format!("{p} percent"),
                None => pct.clone(),
            };
            (
                meter_pct(Some(n)),
                format!("{A11Y_GAUGE} {spoken} of capacity now."),
            )
        }
    };
    if let Some(hour) = gauge::soak_words(g.soak) {
        said.push(' ');
        said.push_str(hour);
    }
    meter(value, said)
}

/// The deck as a reader meets it: the strain gauge at its top left, a meter for each resource in
/// the deck's order, then the cores.
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
        gauge: Some(gauge_meter(snap)),
        meters,
        cores_name: format!("{n} CPU {}", if n == 1 { "core" } else { "cores" }),
        cores,
    }
}

impl crate::App {
    /// The stats deck for `wid`'s accessibility tree, while it shows one.
    #[cfg(a11y_tree)]
    pub(crate) fn stats_a11y(&self, wid: crate::WindowId) -> Option<crate::a11y_tree::GridStats> {
        let f = self.deck_frame(wid).or_else(|| {
            self.windows
                .get(&wid)
                .and_then(|ws| ws.stats_lens.open)
                .filter(|lens| lens.focus == Instrument::NetworkState)
                .and_then(|_| self.network_lens_frame(wid))
        })?;
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
        let mut group = stats_a11y_group(self.vitals.snapshot.as_deref(), &f.tab_panes, &loc);
        for target in crate::network_deck::targets(&self.network_snapshot) {
            let words = crate::network_deck::details(&self.network_snapshot, target);
            let name = words.get(1).map_or_else(
                || "Network State".to_owned(),
                |(name, value)| format!("{name}: {value}"),
            );
            group.meters.push(crate::a11y_tree::StatsMeter {
                name,
                value: None,
                words: words
                    .into_iter()
                    .map(|(name, value)| format!("{name}: {value}"))
                    .collect::<Vec<_>>()
                    .join(". "),
            });
        }
        Some(crate::a11y_tree::GridStats {
            group,
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

    #[test]
    fn changing_the_relay_source_invalidates_the_resource_deck() {
        let mut app = crate::App::headless_for_test();
        let wid = *app.windows.keys().next().expect("window");
        let frame = app.network_lens_frame(wid).expect("network frame");
        let before = app.deck_input_key(&frame);
        let mut snapshot = app.network_snapshot.as_ref().clone();
        snapshot.network.via_local_process = Some(42);
        app.network_snapshot = std::sync::Arc::new(snapshot);
        assert_ne!(app.deck_input_key(&frame), before);
    }

    #[test]
    fn retention_boundary_invalidates_the_deck_without_a_revision_change() {
        let mut app = crate::App::headless_for_test();
        let wid = *app.windows.keys().next().expect("window");
        let frame = app.network_lens_frame(wid).expect("network frame");
        let before = app.deck_input_key(&frame);
        let original = app.network_snapshot.as_ref().clone();
        let mut snapshot = original.clone();
        snapshot.network.retention_limited = !snapshot.network.retention_limited;
        assert_eq!(snapshot.revision, original.revision);
        assert_eq!(snapshot.network.revision, original.network.revision);
        app.network_snapshot = std::sync::Arc::new(snapshot);
        assert_ne!(app.deck_input_key(&frame), before);
    }

    #[test]
    fn service_expiry_changes_the_resource_deck_cache_key_without_a_source_revision() {
        let mut app = crate::App::headless_for_test();
        let wid = *app.windows.keys().next().expect("window");
        let mut snapshot = aterm_types::network_state::NetworkStateSnapshot::default();
        snapshot.services.secrets.availability =
            aterm_types::network_state::ServiceAvailability::Cached;
        snapshot.services.secrets.observed_at_ms = 1000;
        app.network_snapshot = std::sync::Arc::new(snapshot);
        let frame = app.network_lens_frame(wid).expect("network frame");
        let before = app.deck_input_key(&frame);
        app.network_snapshot =
            crate::network_deck::fresh_snapshot(app.network_snapshot.clone(), Some(61_001));
        assert_eq!(app.network_snapshot.revision, 0);
        assert_ne!(app.deck_input_key(&frame), before);
    }

    #[test]
    fn peer_expiry_changes_the_resource_deck_cache_key_without_a_source_revision() {
        use aterm_types::network_state::{
            MachineIdentity, PeerObservation, Reachability, Visibility,
        };
        let mut app = crate::App::headless_for_test();
        let wid = *app.windows.keys().next().expect("window");
        let mut snapshot = aterm_types::network_state::NetworkStateSnapshot::default();
        snapshot.network.visibility = Visibility::Active;
        snapshot.network.observed_at_ms = 10_000;
        snapshot.network.peers.push(PeerObservation {
            slot: 1,
            identity: MachineIdentity {
                machine_id: [1; 16],
                incarnation: [2; 16],
                label: None,
            },
            first_seen_ms: 1_000,
            last_seen_ms: 2_000,
            last_checked_ms: Some(2_000),
            reachability: Reachability::Reachable,
            instances: None,
            samples: Vec::new(),
        });
        app.network_snapshot = std::sync::Arc::new(snapshot);
        let frame = app.network_lens_frame(wid).expect("network frame");
        let before = app.deck_input_key(&frame);
        app.network_snapshot =
            crate::network_deck::fresh_snapshot(app.network_snapshot.clone(), Some(17_001));
        assert_eq!(app.network_snapshot.revision, 0);
        assert_eq!(app.network_snapshot.network.revision, 0);
        assert_ne!(app.deck_input_key(&frame), before);
    }

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

    fn set_size(app: &mut crate::App, pref: DeckSizePref) {
        app.config
            .sys
            .get_or_insert_with(Default::default)
            .stats_deck_size = Some(pref.word().into());
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
                // Full only where chosen, whatever the window's height.
                let frames = [
                    (DeckSizePref::Auto, DeckForm::Folded),
                    (DeckSizePref::Full, DeckForm::Full),
                ]
                .map(|(pref, want)| (band + ((FOLDED_PT + 900.0) * scale) as usize, pref, want));
                for (frame, pref, want) in frames {
                    set_size(&mut app, pref);
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
                        reserved_deck_px(StatsBars::Deck, want, scale, ch),
                        "{at}"
                    );
                    assert_eq!(app.deck_drawn_px(wid), f.geom.h as usize, "{at}");
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

    /// `stats_deck_size` forces the form whatever the window's height; auto keeps the Deck
    /// Folded at every height; a frame of 0 keeps the form.
    #[test]
    fn the_deck_size_setting_forces_the_form() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let form = |app: &crate::App| app.windows[&wid].deck_form;
        let (_, ch) = app.win_cell_size(wid);
        let (tall, short) = (56 + 124 + 1800, 56 + 124 + 1400);
        for (word, frame, want) in [
            ("folded", tall, DeckForm::Folded),
            ("full", short, DeckForm::Full),
            ("auto", 0, DeckForm::Full),
            ("auto", short, DeckForm::Folded),
            ("full", tall, DeckForm::Full),
            ("auto", tall, DeckForm::Folded),
            ("auto", 0, DeckForm::Folded),
            ("auto", short, DeckForm::Folded),
        ] {
            let at = format!("{word} at {frame} px");
            app.config
                .sys
                .get_or_insert_with(Default::default)
                .stats_deck_size = Some(word.into());
            let head = app.deck_head_px(wid, 28.0, 2.0, frame);
            assert_eq!(form(&app), want, "{at}");
            assert_eq!(
                head,
                56 + reserved_deck_px(StatsBars::Deck, want, 2.0, ch),
                "{at}"
            );
        }
    }

    /// A restored window keeps the frame the Deck sized for it (Folded at 62 pt, or Full). It
    /// takes the form the setting gives, Folded unless full, whatever form sized it, and its
    /// grid follows the frame: a window Full sized under the old Automatic gets the rows Full
    /// took back.
    #[test]
    fn a_restored_window_keeps_its_size() {
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
        let px = |deck| deck_px(StatsBars::Deck, deck, scale, cell);
        let full_rows = (px(DeckForm::Full) - px(DeckForm::Folded)) / cell;
        assert!(full_rows > 0, "PRECONDITION: Full takes whole rows");
        let rows_in = |form: DeckForm| match form {
            DeckForm::Full => full_rows as i32,
            DeckForm::Folded => 0,
        };
        let form = |app: &crate::App| app.windows[&wid].deck_form;
        let frame_in = |app: &mut crate::App, deck: DeckForm, rows: u16| {
            app.windows.get_mut(&wid).expect("the window").deck_form = deck;
            let head = app.deck_head_px(wid, band_pts, scale, 0);
            app.backend.set_head(head);
            app.window_frame_px_for(wid, rows, cols)
        };
        let attach = |app: &mut crate::App, rows: u16, carried: Option<PhysicalSize<u32>>| {
            app.windows.get_mut(&wid).expect("the window").deck_form = DeckForm::Folded;
            let head = app.attach_deck_head(wid, band_pts, scale);
            app.windows.get_mut(&wid).expect("the window").metrics.head = head;
            app.attach_frame_px(wid, rows, cols, carried)
        };
        let rem = (cell / 2) as u32;
        for rows in [20, 57, 120] {
            for pref in DeckSizePref::ALL {
                set_size(&mut app, pref);
                for deck in [DeckForm::Full, DeckForm::Folded] {
                    let frame = frame_in(&mut app, deck, rows);
                    let carried = PhysicalSize::new(frame.width, frame.height + rem);
                    let at = format!("{pref:?}: a {deck:?} frame of {rows} rows");
                    let size = attach(&mut app, rows, Some(carried));
                    let want = match pref {
                        DeckSizePref::Full => DeckForm::Full,
                        DeckSizePref::Auto | DeckSizePref::Folded => DeckForm::Folded,
                    };
                    assert_eq!(form(&app), want, "{at}");
                    assert_eq!(size, carried, "the window keeps its size, {at}");
                    let held = app.grid_dims_for(wid, size);
                    assert_eq!(held.1, cols, "{at}");
                    let gained = rows_in(deck) - rows_in(want);
                    assert_eq!(
                        i32::from(held.0) - i32::from(rows),
                        gained,
                        "the grid takes back what Full took, {at}"
                    );
                    let _ = attach(&mut app, rows, None);
                    assert_eq!(form(&app), want, "a new window of that grid, {at}");
                }
            }
        }
    }

    /// A carried frame no deck sized (one from another display scale, twice or half the size
    /// its grid takes here, or one with no deck in its height) is never kept: the window opens
    /// in the form and at the size it would with nothing carried. CONTROL: a frame the Full
    /// deck sized, under Automatic, is kept at its own size.
    #[test]
    fn a_frame_no_deck_sized_sizes_the_deck_as_nothing_carried() {
        use aterm_window::dpi::PhysicalSize;
        let _attached = attach_stats();
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let (band_pts, scale, cols) = (28.0, 2.0, 80);
        {
            let ws = app.windows.get_mut(&wid).expect("the window");
            ws.scale = scale;
            ws.head_pts = band_pts;
        }
        let (_, cell) = app.win_cell_size(wid);
        let band = band_px(band_pts, scale);
        let attach = |app: &mut crate::App, rows, carried: Option<PhysicalSize<u32>>| {
            app.windows.get_mut(&wid).expect("the window").deck_form = DeckForm::Folded;
            let head = app.attach_deck_head(wid, band_pts, scale);
            app.windows.get_mut(&wid).expect("the window").metrics.head = head;
            let size = app.attach_frame_px(wid, rows, cols, carried);
            (app.windows[&wid].deck_form, size)
        };
        for rows in 3..=16u16 {
            let (form, size) = attach(&mut app, rows, None);
            let bare = size.height as usize - app.backend.head();
            let doubled = PhysicalSize::new(size.width * 2, size.height * 2);
            let halved = PhysicalSize::new(size.width / 2, size.height / 2);
            let no_deck = PhysicalSize::new(size.width, (bare + band) as u32);
            for carried in [doubled, halved, no_deck] {
                let at = format!("{rows} rows, carried {carried:?}");
                assert_eq!(attach(&mut app, rows, Some(carried)), (form, size), "{at}");
            }
            let full = (bare
                + chrome_head_px(band_pts, scale, StatsBars::Deck, DeckForm::Full, cell))
                as u32;
            let full = PhysicalSize::new(size.width, full);
            assert_eq!(
                attach(&mut app, rows, Some(full)),
                (form, full),
                "CONTROL: {rows} rows, a frame the Full deck sized is kept"
            );
        }
    }

    /// A new window (nothing carried) opens Folded under Automatic and holds the grid it was
    /// opened for.
    #[test]
    fn a_new_window_opens_folded_and_holds_its_grid() {
        let _attached = attach_stats();
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let (band_pts, scale, cols) = (28.0, 2.0, 80);
        {
            let ws = app.windows.get_mut(&wid).expect("the window");
            ws.scale = scale;
            ws.head_pts = band_pts;
        }
        for rows in 3..=16u16 {
            let at = format!("a new window of {rows} rows");
            app.windows.get_mut(&wid).expect("the window").deck_form = DeckForm::Full;
            let head = app.attach_deck_head(wid, band_pts, scale);
            app.windows.get_mut(&wid).expect("the window").metrics.head = head;
            assert_eq!(app.windows[&wid].deck_form, DeckForm::Folded, "{at}");
            let size = app.attach_frame_px(wid, rows, cols, None);
            assert_eq!(app.grid_dims_for(wid, size), (rows, cols), "{at}");
        }
    }

    /// A window the Deck sized Full, attached under Automatic (which is Folded), keeps its
    /// frame, and every tab of it re-grids once to what that frame holds, booked `site=deck`.
    /// A window sized in the form it takes re-grids nothing.
    #[test]
    fn a_frame_the_other_form_sized_regrids_each_tab_once_at_its_attach() {
        for deck in [DeckForm::Full, DeckForm::Folded] {
            let at = format!("a {deck:?} frame attached under Automatic");
            let _attached = attach_stats();
            let (mut app, wid, sessions) = three_panes();
            let (band_pts, scale) = (28.0, 2.0);
            {
                let ws = app.windows.get_mut(&wid).expect("the window");
                ws.scale = scale;
                ws.head_pts = band_pts;
            }
            let (rows, cols) = (app.windows[&wid].rows, app.windows[&wid].cols);
            app.windows.get_mut(&wid).expect("the window").deck_form = deck;
            let head = app.deck_head_px(wid, band_pts, scale, 0);
            app.backend.set_head(head);
            let carried = app.window_frame_px_for(wid, rows, cols);
            let before = sites_of(&app, &sessions);
            let head = app.attach_deck_head(wid, band_pts, scale);
            app.windows.get_mut(&wid).expect("the window").metrics.head = head;
            let size = app.attach_frame_px(wid, rows, cols, Some(carried));
            assert_eq!(size, carried, "the window keeps its frame, {at}");
            let held = app.grid_dims_for(wid, size);
            app.regrid_to_attach_frame(wid, size);
            app.regrid_to_attach_frame(wid, size);
            let ws = &app.windows[&wid];
            assert_eq!((ws.rows, ws.cols), held, "{at}");
            let booked = new_sites(&before, &sites_of(&app, &sessions));
            assert_eq!(app.windows[&wid].deck_form, DeckForm::Folded, "{at}");
            if deck == DeckForm::Folded {
                assert_eq!(held, (rows, cols), "{at}");
                assert!(booked.iter().all(Vec::is_empty), "{booked:?}, {at}");
            } else {
                assert_ne!(held, (rows, cols), "PRECONDITION: the head moved, {at}");
                assert!(
                    booked.iter().all(|b| b == &["site=deck"]),
                    "one re-grid each, booked deck: {booked:?}, {at}"
                );
            }
        }
    }

    /// A window of two tabs, the second split: three panes, each its own session.
    fn three_panes() -> (crate::App, crate::WindowId, Vec<u64>) {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let first = app
            .focused_session_id(wid)
            .expect("the first tab's session");
        let second = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(second));
        let split = app.split_active_stub_tab(wid);
        let sessions = vec![first, second, split];
        assert_eq!(app.windows[&wid].tab_set.tabs().len(), 2, "PRECONDITION");
        (app, wid, sessions)
    }

    /// Each session's resize rows' `site=` words, oldest first.
    fn sites_of(app: &crate::App, sessions: &[u64]) -> Vec<Vec<String>> {
        sessions
            .iter()
            .map(|id| {
                let reply = app
                    .pool
                    .get(*id)
                    .expect("a live session")
                    .ctx
                    .timeline
                    .lock()
                    .unwrap()
                    .resizes()
                    .reply(0, None);
                reply
                    .lines()
                    .filter(|l| l.starts_with("resize "))
                    .filter_map(|l| l.split(' ').find(|w| w.starts_with("site=")))
                    .map(str::to_owned)
                    .collect()
            })
            .collect()
    }

    /// What each session booked since `before`.
    fn new_sites(before: &[Vec<String>], after: &[Vec<String>]) -> Vec<Vec<String>> {
        before
            .iter()
            .zip(after)
            .map(|(b, a)| a[b.len()..].to_vec())
            .collect()
    }

    /// A switch of the deck's size in the config re-grids every tab of the window once,
    /// booked `site=deck`, and a reload that keeps the size re-grids none. A headless
    /// window's head is the backend's, so the test moves it to the new size's deck as
    /// `refresh_all_window_metrics` moves an attached window's during the reload.
    #[test]
    fn a_size_switch_regrids_each_tab_once_booked_deck() {
        use aterm_window::dpi::PhysicalSize;
        let _attached = attach_stats();
        let (mut app, wid, sessions) = three_panes();
        let scratch = aterm_tempfile::tempdir().expect("scratch dir");
        let dir = scratch.path().to_path_buf();
        let text = |pref: DeckSizePref, bars: &str| {
            format!("[sys]\n{bars}stats_deck_size = \"{}\"\n", pref.word())
        };
        let apply = |app: &mut crate::App, text: String| {
            crate::app_colorscheme::install_resolved_colors::apply_config_text(app, &dir, &text);
        };
        let head_of = |app: &mut crate::App, pref: DeckSizePref| {
            let (_, cell) = app.win_cell_size(wid);
            let ws = app.windows.get_mut(&wid).expect("the window");
            ws.deck_form = plan::deck_form_for(pref);
            let head = chrome_head_px(0.0, ws.scale, StatsBars::Deck, ws.deck_form, cell);
            ws.metrics.head = head;
            app.backend.set_head(head);
        };
        let size = PhysicalSize::new(1600, 1400);
        app.windows.get_mut(&wid).expect("the window").win_px = Some(size);
        apply(&mut app, text(DeckSizePref::Auto, ""));
        head_of(&mut app, DeckSizePref::Auto);
        app.on_resize(wid, size);
        let folded_grid = app.grid_dims_for(wid, size);
        assert_eq!(
            (app.windows[&wid].rows, app.windows[&wid].cols),
            folded_grid
        );
        let deck = "stats_bars = \"deck\"\n";
        for (pref, bars, regrids) in [
            (DeckSizePref::Full, "", true),
            (DeckSizePref::Full, deck, false),
            (DeckSizePref::Auto, deck, true),
            (DeckSizePref::Auto, "", false),
        ] {
            let at = format!("to {pref:?} {bars:?}");
            let before = sites_of(&app, &sessions);
            let grid = (app.windows[&wid].rows, app.windows[&wid].cols);
            head_of(&mut app, pref);
            apply(&mut app, text(pref, bars));
            assert_eq!(app.deck_size_pref(), pref, "{at}");
            let booked = new_sites(&before, &sites_of(&app, &sessions));
            let now = (app.windows[&wid].rows, app.windows[&wid].cols);
            if regrids {
                assert_eq!(now, app.grid_dims_for(wid, size), "{at}");
                assert_ne!(now, grid, "PRECONDITION: the head moved the grid, {at}");
                assert!(
                    booked.iter().all(|b| b == &["site=deck"]),
                    "one re-grid each, booked deck: {booked:?}, {at}"
                );
            } else {
                assert_eq!(now, grid, "{at}");
                assert!(booked.iter().all(Vec::is_empty), "{booked:?}, {at}");
            }
        }
        assert_eq!(
            (app.windows[&wid].rows, app.windows[&wid].cols),
            folded_grid,
            "back where it started"
        );
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
            tab_id: None,
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

    pub(crate) fn input(snap: &Snapshot) -> DeckInput<'_> {
        DeckInput {
            network: None,
            network_target: crate::network_deck::Target::Overview,
            snap: Some(snap),
            prev_snap: None,
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
            live: DeckLive::STILL,
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
            tab_id: None,
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
        let deck = reserved_deck_px(
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
        let deck = reserved_deck_px(
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
    fn visible_capacity_rows_keep_the_drawn_tab_and_aterm_bar_click_targets() {
        use crate::stats_paint::HitKind;
        let m = FaceMetrics::helvetica_neue();
        let mut moved = 0;
        for positive in [true, false] {
            let mut snap = snapshot();
            let view = &mut snap.resources[Resource::Memory.index()];
            let total = view.capacity.expect("memory capacity");
            view.aterm.now = Some(if positive { total * 0.42 } else { 0.0 });
            view.tabs = vec![(
                7,
                ScopeFig {
                    now: Some(if positive { total * 0.15 } else { 0.0 }),
                    ..ScopeFig::default()
                },
            )];
            for form in [DeckForm::Full, DeckForm::Folded] {
                for columns in [80, 120] {
                    let mut i = input(&snap);
                    i.form = form;
                    let g = DeckGeom {
                        w: columns as f32 * 8.0 * GEOM.scale,
                        ..GEOM
                    };
                    let width = f64::from(g.w / g.scale);
                    let table = match form {
                        DeckForm::Full => Table::of_full(&plan::plan_full(width, &m)),
                        DeckForm::Folded => {
                            Table::of_block(&plan::plan_folded(width, &m).blocks[0])
                        }
                    };
                    let words = DeckWords::new(&i, &m);
                    let deck = Deck::new(&g, &i, &m, false);
                    deck.capacity_col(&table, &words.rows[2])
                        .expect("visible capacity");
                    let under = Deck::capacity_moves_bar(&table, &words.rows[2]);
                    moved += usize::from(positive && under);
                    let col = if under {
                        table.pct
                    } else {
                        table.meter.bar.or(table.under)
                    }
                    .expect("scope bar");
                    let laid = lay(&g, &i, &words, &m, true);
                    let said = drawn_row_text(&laid, &table, 2, g.scale);
                    assert!(
                        ["17", "32", "15", "avail"].iter().all(|v| said.contains(v)),
                        "{form:?}/{columns}: memory capacity must be drawn in its row: {said}"
                    );
                    let row_top = table.rows.top(2);
                    let bar = laid
                        .boxes
                        .iter()
                        .find(|b| {
                            b.kind == BoxKind::Bar
                                && (b.r[0] - col.x).abs() < 0.6
                                && b.r[1] >= row_top - 0.6
                                && b.r[3] <= row_top + table.rows.pitch + 0.6
                        })
                        .expect("drawn memory bar");
                    let scale = g.scale;
                    let panel = |ink| {
                        laid.prims.iter().find_map(|p| match p {
                            DrawPrim::Panel {
                                x, y, w, h, fill, ..
                            } if *fill == rgba(ink, 0xFF)
                                && (*x - bar.r[0] as f32 * scale).abs() < 1.0
                                && *y >= bar.r[1] as f32 * scale - 0.6
                                && *y + *h <= bar.r[3] as f32 * scale + 0.6
                                && *w > 0.0 =>
                            {
                                Some((*x, *y, *w, *h))
                            }
                            _ => None,
                        })
                    };
                    if !positive {
                        assert!(panel(i.inks.tab).is_none() && panel(i.inks.aterm).is_none());
                        assert!(
                            !laid.hits.iter().any(|h| matches!(
                                h.kind,
                                HitKind::Row(Instrument::Memory, Bar::Tab | Bar::Aterm)
                            )),
                            "zero fills have no scoped hit"
                        );
                        continue;
                    }
                    let tab = panel(i.inks.tab).expect("positive tab fill");
                    let aterm = panel(i.inks.aterm).expect("positive aterm fill");
                    let y = tab.1 + tab.3 / 2.0;
                    let aterm_start = (tab.0 + tab.2).max(aterm.0 + SEGMENT_HIT_MIN as f32 * scale);
                    assert!(
                        aterm_start < aterm.0 + aterm.2,
                        "fixture must leave an aterm-only part"
                    );
                    for (x, expected) in [
                        (tab.0 + tab.2 / 2.0, Bar::Tab),
                        ((aterm_start + aterm.0 + aterm.2) / 2.0, Bar::Aterm),
                    ] {
                        let hit = laid
                            .hits
                            .iter()
                            .find(|h| {
                                let (hx, hy, hw, hh) = h.rect;
                                x >= hx && x < hx + hw && y >= hy && y < hy + hh
                            })
                            .expect("drawn bar hit");
                        assert_eq!(
                            hit.kind,
                            HitKind::Row(Instrument::Memory, expected),
                            "{form:?}/{columns}, moved={under}"
                        );
                    }
                }
            }
        }
        assert!(
            moved > 0,
            "the regression must exercise a relocated capacity bar"
        );
    }

    /// Text primitives whose baseline belongs to this resource's actual row, including its
    /// history caption and amount. Another row's header or figure cannot satisfy a capacity fact.
    fn drawn_row_text(laid: &Laid, table: &Table, r: u32, scale: f32) -> String {
        let top = table.rows.top(r) as f32 * scale;
        let foot = table.rows.top(r + 1) as f32 * scale;
        let left = table.name.x as f32 * scale;
        let right = table.end as f32 * scale;
        laid.prims
            .iter()
            .filter_map(|p| match p {
                DrawPrim::Text {
                    x, baseline: y, s, ..
                } if *x >= left && *x < right && *y >= top && *y < foot => Some(s.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn normal_width_monitors_draw_used_available_total_and_gpu_identity() {
        let mut snap = snapshot();
        let cpu = snap.cpu.as_mut().expect("fixture CPU");
        let old_count = cpu.total();
        cpu.cores.resize_with(18, Default::default);
        cpu.clusters
            .last_mut()
            .expect("fixture cluster")
            .cores
            .extend(old_count..18);
        snap.resources[Resource::Cpu.index()].capacity = Some(18.0);
        let memory = &mut snap.mem.as_mut().expect("fixture memory").sample;
        memory.total = 128 << 30;
        memory.app = 80 << 30;
        memory.wired = 12 << 30;
        memory.compressed = Some(4 << 30);
        memory.cached = 24 << 30;
        memory.free = 8 << 30;
        let memory_view = &mut snap.resources[Resource::Memory.index()];
        memory_view.now = Some((96_u64 << 30) as f64);
        memory_view.capacity = Some((128_u64 << 30) as f64);
        snap.volume = Some(aterm_vitals::model::VolumeSample {
            total: 8_000_000_000_000,
            free: 400_000_000_000,
            available: 240_000_000_000,
            mount: "/".into(),
        });
        let disk_view = &mut snap.resources[Resource::Disk.index()];
        disk_view.now = Some(7_760_000_000_000.0);
        disk_view.capacity = Some(8_000_000_000_000.0);
        let gpu = snap.facts.gpu.as_mut().expect("fixture GPU");
        gpu.name = Some("Apple M5 Max".into());
        gpu.cores = Some(40);
        gpu.in_use_bytes = Some(13 * (1_u64 << 30) / 10);
        let m = FaceMetrics::helvetica_neue();
        for form in [DeckForm::Full, DeckForm::Folded] {
            for columns in [80, 120] {
                let mut i = input(&snap);
                i.form = form;
                let g = DeckGeom {
                    w: columns as f32 * 8.0 * GEOM.scale,
                    ..GEOM
                };
                let laid = lay(&g, &i, &DeckWords::new(&i, &m), &m, true);
                let width = f64::from(g.w / g.scale);
                let table = match form {
                    DeckForm::Full => Table::of_full(&plan::plan_full(width, &m)),
                    DeckForm::Folded => Table::of_block(&plan::plan_folded(width, &m).blocks[0]),
                };
                let text = laid
                    .prims
                    .iter()
                    .filter_map(|p| match p {
                        DrawPrim::Text { s, .. } => Some(s.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                if form == DeckForm::Full {
                    assert!(
                        text.contains("18 cores"),
                        "{form:?}/{columns}: CPU total must be visible: {text}"
                    );
                }
                for (row, facts) in [
                    (2, ["96", "128", "32", "GiB", "avail"]),
                    (3, ["7.60", "8", "240", "TB", "avail"]),
                ] {
                    let said = drawn_row_text(&laid, &table, row, g.scale);
                    assert!(
                        facts.iter().all(|fact| said.contains(fact)),
                        "{form:?}/{columns}: capacity is not visible in its monitor row: {said}; all text: {text}"
                    );
                    assert!(
                        said.contains("used") && !said.contains("unavailable"),
                        "{form:?}/{columns}: occupied bytes must be labeled used: {said}"
                    );
                }
                let gpu_text = drawn_row_text(&laid, &table, 1, g.scale);
                assert!(
                    gpu_text.contains("M5 Max") && gpu_text.contains("40 cores"),
                    "{form:?}/{columns}: {text}"
                );
                let histories: Vec<_> = laid
                    .boxes
                    .iter()
                    .filter(|b| b.kind == BoxKind::Strip)
                    .collect();
                assert_eq!(
                    histories.len(),
                    DECK_RESOURCES.len(),
                    "{form:?}/{columns}: capacity must retain every resource's history"
                );
                for row in [1, 2, 3] {
                    assert!(
                        histories.iter().any(|b| {
                            b.r[0] == table.strip.x
                                && b.r[2] == table.strip.end()
                                && b.r[1] >= table.rows.top(row)
                                && b.r[3] < table.rows.top(row + 1)
                                && b.r[3] > b.r[1]
                        }),
                        "{form:?}/{columns}: row {row} keeps its whole time axis below the caption"
                    );
                }
                assert_eq!(g.h, GEOM.h, "capacity text adds no chrome height");
            }
        }
        snap.cpu = None;
        let mut i = input(&snap);
        i.form = DeckForm::Full;
        let g = DeckGeom {
            w: 80.0 * 8.0 * GEOM.scale,
            ..GEOM
        };
        let (prims, _) = deck_paint(&g, &i);
        assert!(
            prims.iter().any(|p| matches!(p, DrawPrim::Text { s, .. } if s.contains("cores") && s.contains("unknown"))),
            "an unread CPU core count must stay explicit"
        );
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

    /// Each scope's head carries its ink as a swatch where the heads have room for one, on the
    /// face the deck draws.
    #[test]
    fn every_bar_label_carries_its_ink_as_a_key() {
        let snap = snapshot();
        let inks = dark_inks();
        let m = FaceMetrics::helvetica_neue();
        for (form, w) in [(DeckForm::Full, 2238.0), (DeckForm::Folded, 2238.0)] {
            let mut i = input(&snap);
            i.form = form;
            let g = DeckGeom { w, ..GEOM };
            let prims = lay(&g, &i, &DeckWords::new(&i, &m), &m, true).prims;
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

    /// A bar's fills are whole px and nested (the machine's ⊇ aterm's ⊇ this tab's); a scope
    /// above zero keeps the least it is given however small its share, and a scope with no
    /// figure draws nothing even where a scope inside it has one. The bar's column nests them in
    /// height too, so there a scope that rounds to the length of the one inside it still shows
    /// as a rim; the bar under a % nests them by length alone, so there (`apart`) a scope above
    /// zero ends a px past the one inside it, and at the bar's end the fills inside give way.
    #[test]
    fn a_bars_fills_are_whole_nested_and_never_lose_a_scope() {
        let fill = |global, aterm, tab| BarFill {
            global,
            aterm,
            tab,
            ticks: None,
            warn_tick: None,
        };
        let w = 52.0;
        assert_eq!(
            fill(Some(0.7), Some(0.4), Some(0.1)).widths_px(w, 1.0, 0.0),
            [36.0, 21.0, 5.0]
        );
        assert_eq!(
            fill(Some(0.3), Some(0.001), Some(0.0005)).widths_px(w, 1.0, 0.0),
            [16.0, 1.0, 1.0],
            "a scope above zero keeps a px"
        );
        assert_eq!(
            fill(Some(0.3), Some(0.001), Some(0.0005)).widths_px(w, 2.0, 0.0),
            [16.0, 2.0, 2.0]
        );
        assert_eq!(
            fill(Some(0.0), Some(0.0), Some(0.0)).widths_px(w, 1.0, 0.0),
            [0.0; 3]
        );
        assert_eq!(
            fill(Some(f32::NAN), None, Some(0.2)).widths_px(w, 1.0, 0.0),
            [10.0, 0.0, 10.0],
            "the machine's fill covers this tab's; no aterm figure, no aterm fill"
        );
        assert_eq!(
            fill(None, Some(0.2), Some(0.3)).widths_px(w, 1.0, 0.0),
            [0.0, 16.0, 16.0]
        );
        assert_eq!(
            fill(Some(1.4), None, None).widths_px(w, 1.0, 0.0),
            [52.0, 0.0, 0.0]
        );
        assert_eq!(
            fill(Some(0.5), None, None).widths_px(0.5, 1.0, 0.0),
            [0.5, 0.0, 0.0],
            "the least a scope keeps is never wider than the bar"
        );
        // The bar under a %: aterm's 0.605 and this tab's 0.6 both round to 31 of 52 px.
        assert_eq!(
            fill(Some(0.7), Some(0.605), Some(0.6)).widths_px(w, 1.0, 0.0),
            [36.0, 31.0, 31.0],
            "CONTROL: in the column, aterm's rim shows it"
        );
        assert_eq!(
            fill(Some(0.7), Some(0.605), Some(0.6)).widths_px(w, 1.0, 1.0),
            [36.0, 32.0, 31.0],
            "aterm keeps a px past this tab"
        );
        assert_eq!(
            fill(Some(0.61), Some(0.605), Some(0.6)).widths_px(w, 1.0, 1.0),
            [33.0, 32.0, 31.0],
            "and the machine a px past aterm"
        );
        assert_eq!(
            fill(Some(0.3), Some(0.001), Some(0.0005)).widths_px(w, 1.0, 1.0),
            [16.0, 2.0, 1.0]
        );
        assert_eq!(
            fill(Some(1.0), Some(0.995), Some(0.99)).widths_px(w, 1.0, 1.0),
            [52.0, 51.0, 50.0],
            "at the bar's end the fills inside give way"
        );
        assert_eq!(
            fill(Some(0.5), Some(0.5), Some(0.5)).widths_px(w, 1.0, 1.0),
            [28.0, 27.0, 26.0],
            "a scope whose share equals the one inside it keeps a px of its own"
        );
        assert_eq!(
            fill(Some(0.5), Some(0.5), Some(0.5)).widths_px(w, 1.0, 0.0),
            [26.0; 3],
            "CONTROL: the column draws them alike and shows the rims"
        );
        assert_eq!(
            fill(None, Some(0.2), Some(0.3)).widths_px(w, 1.0, 1.0),
            [0.0, 17.0, 16.0],
            "aterm's figure under this tab's still covers it, and shows"
        );
        assert_eq!(
            fill(Some(0.0), Some(0.2), Some(0.0)).widths_px(w, 1.0, 1.0),
            [10.0, 10.0, 0.0],
            "a scope at zero keeps no px"
        );
        assert_eq!(
            fill(Some(0.4), None, Some(0.399)).widths_px(w, 1.0, 1.0),
            [22.0, 0.0, 21.0],
            "with no aterm figure, the machine keeps its px past this tab"
        );
        assert_eq!(
            fill(Some(0.9), Some(0.8), Some(0.7)).widths_px(2.0, 1.0, 1.0),
            [2.0, 1.0, 0.0],
            "a bar too short for every px keeps the outer scopes'"
        );
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
        assert_eq!(g.name, "Stats deck, newest first");
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
            "Disk 98 percent of capacity: 1.96 of 2.00 terabytes unavailable. aterm writes 410 \
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

#[cfg(test)]
#[path = "stats_live_paint_tests.rs"]
mod live_paint_tests;
