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
    self as plan, BOX_GAP, BlockKind, BlockPlan, Cluster, Col, CoresMode, DeckForm, DeckRows,
    DeckSizePref, FOLD_GAUGE_R, FoldedGauge, FullKind, FullPlan, GaugeSlot, HeaderPlan, HeaderText,
    KeyItem, Label, Meter, Rows, StateLabel, Strip,
};
use crate::stats_skyline::{self as skyline, BoxValue, Skyline};
use crate::stats_steam as steam;
use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, TextFace, TextWeight, rgba, text_prim};

/// The slot left of the Full deck's names (the gauge's) and its divider run from this far below
/// the deck's top to this far above its foot, pt.
const SLOT_TOP: f64 = 4.0;
const SLOT_FOOT: f64 = 6.0;

/// The Folded deck's height in points (124 device px at 2×, 62 at 1×): two blocks of the rows it
/// paints ([`DeckRows::PAINTED`]), four each until A4's step N5 paints five (73.5 pt).
pub(crate) const FOLDED_PT: f64 = DeckRows::PAINTED.folded_pt();

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
        DeckForm::Full => folded + DeckRows::PAINTED.full_delta_px(scale, cell_px),
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
    /// Card height ([`deck_px`], or for the sill the bottom pad less the
    /// drop-target border, [`crate::stats_sill::sill_card_px`]).
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
    /// The frame's visible place words, shared by its cache key and network paint.
    pub(crate) network_place: Option<&'a crate::network_deck::PlaceCaption>,
    /// Stable machine/service selected in the network close-up.
    pub(crate) network_target: crate::network_deck::Target,
    /// The lab's account rows ([`crate::lab_deck`]), drawn on the network band's Shared State
    /// line; absent where the network band is.
    pub(crate) lab: Option<&'a crate::lab_deck::LabDeck>,
    /// The Network State band's Fleet Field ([`crate::network_deck::field`]); `None` where the
    /// band draws the observer's peers (no fleet glance, no fleet) or is not drawn.
    pub(crate) fleet: Option<&'a crate::network_deck::field::Field>,
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
    /// zero, and never wider than the fill of the scope it nests in, so the machine's fill is
    /// exactly its share, the row's % (`docs/DESIGN-stats-deck-units-2026-11-04.md` §4.6), and a
    /// scope inside that reads more (a footprint counted another way) is clamped to it, never
    /// widening it; none for a scope with no figure (`None`, or not finite), whose room the
    /// scope inside it takes from the one around it, and none inside a scope at zero.
    ///
    /// `apart` is for a bar whose fills nest by length alone, each its whole height (the bar
    /// under a % figure): there a fill ends `apart` px or more short of the fill around it that
    /// is above zero, so some of the outer scope's ink shows however close the two round, as the
    /// column shows its rim; where a fill is too short for that px the fills inside give way. No
    /// fill passes `w`. The bar's column, whose fills nest in height too, passes `0`.
    fn widths_px(&self, w: f32, least: f32, apart: f32) -> [f32; 3] {
        // Outermost first: the machine's, aterm's, this tab's.
        let mut ends = [0.0_f32; 3];
        let mut room = w.max(0.0);
        for (i, v) in [self.global, self.aterm, self.tab].into_iter().enumerate() {
            let Some(v) = v.filter(|v| v.is_finite()) else {
                continue;
            };
            if v > 0.0 {
                ends[i] = (v.min(1.0) * w).round().max(least).min(room);
                room = (ends[i] - apart).max(0.0);
            } else {
                room = 0.0;
            }
        }
        ends
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

/// Try priority prefixes in one buffer, shortening each from its least urgent end.
fn choose_details(
    clauses: &[ClauseTxt],
    available: f64,
    width: impl Fn(&[(&ClauseTxt, bool)]) -> f64,
) -> Vec<(&ClauseTxt, bool)> {
    let mut choice = Vec::with_capacity(clauses.iter().filter(|c| !c.forms.is_empty()).count());
    choice.extend(
        clauses
            .iter()
            .filter(|c| !c.forms.is_empty())
            .map(|c| (c, false)),
    );
    // A mandatory clause precedes an optional one at the same priority; ties within
    // either group retain their input order.
    choice.sort_by_key(|(c, _)| (c.prio, !c.keep));
    let first = choice.first().map(|&(c, _)| c);
    loop {
        for (_, short) in &mut choice {
            *short = false;
        }
        if width(&choice) <= available {
            return choice;
        }
        for i in (0..choice.len()).rev() {
            choice[i].1 = true;
            if width(&choice) <= available {
                return choice;
            }
        }
        if let Some(i) = choice.iter().rposition(|(c, _)| !c.keep) {
            choice.remove(i);
        } else {
            if choice.is_empty()
                && let Some(c) = first
            {
                choice.push((c, true));
            }
            return choice;
        }
    }
}

#[cfg(test)]
#[path = "stats_details_tests.rs"]
mod details_tests;

#[cfg(test)]
#[path = "stats_event_buffer_tests.rs"]
mod event_buffer_tests;

#[cfg(test)]
#[path = "stats_slot_rect_tests.rs"]
mod slot_rect_tests;

/// Everything one row draws, read from the snapshot.
#[derive(Clone, Debug, PartialEq)]
struct RowWords {
    res: Resource,
    name: Txt,
    well: Option<(WellMark, InkKind)>,
    heat_now: Option<f32>,
    /// The live box's peak tick and over share: the trailing 10 s's largest reading and the
    /// share of those readings' read time over ([`ResourceView::live_box`]); its column is
    /// `heat_now`.
    live_peak: Option<f32>,
    live_over: f32,
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
    /// The machine's used of total for the legend where no run says the rows' maxima
    /// ([`words::machine_totals`]), longest first.
    totals: Vec<words::TotalsForm>,
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

/// The name of the row that draws the CPU's cores: block B's fourth in Folded, Full's eighth in
/// narrow and slim.
pub(crate) const CORES_NAME: &str = "Cores";

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
            .unwrap_or_else(|| {
                snap.total_cores()
                    .map(|count| CoreCluster {
                        name: None,
                        heat: vec![None; count],
                    })
                    .into_iter()
                    .collect()
            });
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
            cpu_total: snap.total_cores(),
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
                live_peak: None,
                live_over: 0.0,
                pct: Txt::new(words::pct(None), m),
                strip_text: Some(vec![Txt::new("reading\u{2026}", m)]),
                multi_scale: [Vec::new(), Vec::new(), Vec::new(), Vec::new()],
                bar: None,
                run: Vec::new(),
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

    /// Whether one of `rows` draws an over cap in `strip`'s multi-scale horizon, its live box's
    /// included.
    fn over_shown<'r>(rows: impl IntoIterator<Item = &'r RowWords>, strip: &Strip) -> bool {
        rows.into_iter().any(|row| {
            row.strip_text.is_none()
                && (row.live_over > 0.0
                    || row
                        .multi_scale_slots(strip)
                        .iter()
                        .any(|s| !s.slot.is_live_now && s.cell.over > 0.0))
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
        let run = txts(words::glance_run(res, &view, snap, loc), m);
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
        let (live_peak, live_over) = view.live_box(snap.at_unix_ms);
        Self {
            res,
            name: Txt::new(words::resource_name(res), m),
            well: words::well(res, &view).map(|(mark, ink)| (WellMark::of(mark), ink)),
            heat_now: view.heat_now.filter(|h| !h.is_nan()),
            live_peak: live_peak.filter(|h| !h.is_nan()),
            live_over,
            pct: Txt::new(words::pct(view.heat_now), m),
            strip_text,
            multi_scale: STRIP_SPECS.map(|spec| {
                view.heat_multi_scale(spec, snap.at_unix_ms, SLOT_COVER_MS, &snap.time_gaps)
            }),
            bar: bar_fill(res, &view, snap, panes),
            run,
            state,
            details,
        }
    }
}

/// The scope bar's fills, 0.144.0's as main draws them (the owner, 2026-10-26: "the bar of "this
/// Mac, aterm, Tab 1" that's great"): the machine's share now, aterm's and this tab's nested in
/// it ([`Snapshot::bar_shares`]), the disk's and the network's scopes by their writes and
/// sockets, and the GPU's by their share of its busy time (client time times κ,
/// `aterm_vitals::gpu_clients`), each GPU scope absent while no per-app read is held or the
/// device's busy is unread, so an unread GPU draws no bar. Tokens have none.
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
        // A scope's share of the device draws only inside the device's own fill: while its
        // busy is unread there is nothing to share, and the row draws no bar.
        Resource::Gpu if global.is_none() => (None, None),
        Resource::Gpu => {
            let held = |fig: &ScopeFig| share(fig).filter(|_| fig.missing.is_none());
            let tab = (!panes.is_empty()).then(|| held(&view.tab)).flatten();
            (held(&view.aterm), tab)
        }
        Resource::Tokens => (None, None),
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
    fine_band_range(slots.iter().copied())
}

fn fine_band_range(slots: impl Iterator<Item = ProgSlot>) -> Option<std::ops::Range<usize>> {
    let mut slots = slots.enumerate();
    let (start, first) = slots.find(|(_, slot)| !slot.is_live_now)?;
    let period = |slot: &ProgSlot| slot.age_hi_ms - slot.age_lo_ms;
    let p = period(&first);
    let len = 1 + slots
        .map(|(_, slot)| slot)
        .take_while(|slot| !slot.is_live_now && period(slot) == p)
        .count();
    Some(start..start + len)
}

/// `strip`'s fine band at `scale` ([`live::FineBand`]): its boxes' pitch, device px, as
/// [`Strip::slot_edges`] lays them before rounding, and their period.
pub(crate) fn fine_band(strip: &Strip, scale: f64) -> live::FineBand {
    let mut edges = strip.slot_edges();
    let Some((slot, (left, right))) = edges.find(|(slot, _)| !slot.is_live_now) else {
        return live::FineBand::default();
    };
    let pitch = edges
        .next()
        .map_or(right - left + BOX_GAP, |(_, (next, _))| next - left);
    live::FineBand {
        box_px: pitch * scale.max(0.5),
        period_ms: slot.age_hi_ms - slot.age_lo_ms,
    }
}

/// Whole-pixel slot layout `(slot_idx, x_px, w_px, is_live_now)` for `strip` at `scale`, newest
/// first (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §3): the live `now` box at `strip.x`,
/// then each older box to its right, the oldest ending at the horizon, `strip.end()`. Each box's
/// edges are [`Strip::slot_edges`]' rounded to the device px, so a band's boxes stand a point apart
/// and two bands a gate apart (`docs/DESIGN-stats-deck-units-2026-11-04.md` §3), and the header's
/// ticks, which come from the same edges ([`Strip::x_at_age_ms`]), stand in the gates.
///
/// `lag_px` displaces the fine band ([`fine_band_slots`]) that many device px left, toward now,
/// as a reading's slide or glide has yet to carry it right: its newest box then passes under the
/// live box, and at its old end a box enters, a pitch past the band's oldest, that shows the
/// band's own oldest slot, never a coarser box's. The painter clips the band to the span it
/// stands in at rest, so a box is listed only while some of it shows there. The live box and the
/// coarser boxes stand at their ticks.
pub(crate) fn multi_scale_slot_rects(
    strip: &Strip,
    scale: f64,
    lag_px: f32,
) -> Vec<(usize, f32, f32, bool)> {
    let s = scale.max(0.5);
    let px = |v: f64| (v * s + 0.5).floor() as f32;
    let lag = lag_px.round();
    let count = strip.spec.slot_count();
    let mut rest = Vec::with_capacity(count.saturating_add(usize::from(lag > 0.0 && count > 0)));
    rest.extend(strip.slot_edges().enumerate().map(|(j, (slot, (l, r)))| {
        let lx = px(l);
        (j, lx, (px(r) - lx).max(1.0), slot.is_live_now)
    }));
    let Some(band) = (lag > 0.0)
        .then(|| fine_band_range(strip.spec.iter_slots()))
        .flatten()
    else {
        return rest;
    };
    let gap_px = px(BOX_GAP).max(1.0);
    let oldest = band.end - 1;
    let (band_x0, (_, old_x, old_w, _)) = (rest[band.start].1, rest[oldest]);
    let band_x1 = old_x + old_w;
    // The entering box stands a pitch past the band's oldest, as the band's own boxes do.
    let after = band_x1 + gap_px;
    let shows = |x: f32, w: f32| x.max(band_x0) + 1.0 <= (x + w).min(band_x1);
    for rect in &mut rest[band.clone()] {
        rect.1 -= lag;
    }
    rest.retain(|&(j, x, w, _)| !band.contains(&j) || shows(x, w));
    if shows(after - lag, old_w) {
        let at = rest.partition_point(|rect| rect.0 < band.end);
        rest.insert(at, (oldest, after - lag, old_w, false));
    }
    rest
}

impl RowWords {
    fn multi_scale_slots(&self, strip: &Strip) -> &[HeatSlot] {
        self.multi_scale
            .get(spec_ix(strip))
            .map_or(&[][..], Vec::as_slice)
    }
}

/// A time-scale tick at `x` pt as device px at `scale`: its left edge and its width, one device
/// px (half a point at 2×), centred on `x`.
pub(crate) fn tick_px(x: f64, scale: f64) -> (f32, f32) {
    let w = (0.5 * scale).round().max(1.0) as f32;
    (((x * scale) as f32 - w / 2.0).round(), w)
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
    /// The over caps the strip boxes drew since the last axis rail, device px, as each shows
    /// (a moving fine band's clipped to its span), each with its box's band: the rail breaks
    /// over a cap it would touch ([`Self::rail_pieces`]).
    caps: Vec<(Rect, u8)>,
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
            caps: Vec::new(),
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

    /// The room an over cap takes above a strip's top, pt: its clear gap and its height
    /// ([`skyline::cap_px`]).
    fn cap_room(&self) -> f64 {
        let (clear, h) = skyline::cap_px(self.s);
        f64::from(clear + h) / self.s
    }

    /// Draws one strip box as [`skyline::skyline_box`] laid it.
    fn skyline(&mut self, sky: &Skyline, radius: f32) {
        if self.draw {
            let prims = sky.prims(&self.inks, radius, (0.0, 0.0));
            self.p.prims.extend(prims);
        }
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

    /// A gap's ink: sleep is `hollow`, nothing read, never `stuck`, the colour of a stuck
    /// process (`docs/DESIGN-stats-deck-units-2026-11-04.md` §3, "States"); paused is `words2`.
    /// Increase Contrast lifts both a step, sleep to its `hollow` (`words2`) and paused to
    /// `words`, so the three kinds stay apart in both modes.
    const fn gap_ink(&self, kind: GapKind) -> Rgb {
        match kind {
            GapKind::Crash => self.inks.critical_text,
            GapKind::Sleep => self.inks.hollow,
            GapKind::Paused if self.ic => self.inks.words,
            GapKind::Paused => self.inks.words2,
        }
    }

    /// An event line's ink: a sleep's wake is `hollow`, as its gap is.
    const fn event_ink(&self, kind: StripEventKind) -> Rgb {
        match kind {
            StripEventKind::Crash => self.inks.critical_text,
            StripEventKind::Restart => self.inks.warn_text,
            StripEventKind::SleepWake => self.inks.hollow,
            StripEventKind::PressureSpike => self.inks.warn_text,
        }
    }

    /// One row's strip: every box by [`skyline::skyline_box`], the live box's column the 10 s
    /// mean `live.0` (the %), its tick the 10 s max and its cap the 10 s over share (`live.1`,
    /// [`ResourceView::live_box`]); a gap's box its gap mark.
    fn strip(
        &mut self,
        strip: &Strip,
        top: f64,
        cell_h: f64,
        slots: &[HeatSlot],
        live: (Option<f32>, BoxValue),
    ) {
        let cap_room = self.cap_room();
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
        let spec_slots = strip.spec.slots();
        let band_of = |idx: usize| spec_slots.get(idx).map_or(0, |slot| slot.band);
        // A moving fine band draws inside the span it stands in at rest: its newest box passes
        // under the live box and the entering one comes out from its old end, so neither the
        // live box nor a coarser box ever takes a moving box's ink.
        let band = fine_band_slots(&spec_slots).unwrap_or(0..0);
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
            let value = BoxValue {
                mean: slot_data.cell.heat,
                max: slot_data.cell.max,
                over: slot_data.cell.over,
            };
            let sky = skyline::skyline_box((x, y, cw, h), value, self.s, &self.inks, self.ic);
            self.skyline(&sky, radius);
            self.note_cap(sky.cap, if clipped { clip } else { None }, band_of(idx));
        }
        if clipped {
            self.p.prims.push(DrawPrim::ClipPop);
        }
        if let Some((idx, x, cw, _)) = live_slot {
            let slot_data = slots.get(idx).copied().unwrap_or_default();
            let (live_heat, live_box) = live;
            let value = BoxValue {
                mean: live_heat.or(slot_data.cell.heat),
                max: live_box.max.or(slot_data.cell.max),
                over: live_box.over,
            };
            let mut sky = skyline::skyline_box((x, y, cw, h), value, self.s, &self.inks, self.ic);
            // A live box whose readings stopped draws its marks toward the ground.
            if !self.live.fresh {
                let ground = self.inks.ground;
                for mark in [&mut sky.column, &mut sky.tick].into_iter().flatten() {
                    mark.1 = ink::mix(ground, mark.1, 0.55);
                }
            }
            self.skyline(&sky, radius);
            self.note_cap(sky.cap, None, band_of(idx));
        }
    }

    /// Keeps a box's over `cap` for the axis rail to break over, as much of it as shows inside
    /// `clip` (a moving fine band's span, `x0..x1`), with the box's `band`.
    fn note_cap(&mut self, cap: Option<Rect>, clip: Option<(f32, f32)>, band: u8) {
        let Some((x, y, w, h)) = cap else {
            return;
        };
        let (a, b) = clip.map_or((x, x + w), |(c0, c1)| (x.max(c0), (x + w).min(c1)));
        if b > a {
            self.caps.push(((a, y, b - a, h), band));
        }
    }

    /// The pieces `(a, b)` of a 1 device px rail along `y` from `x0` to `x1`, broken over each
    /// over cap of `caps` that would touch it, with a device px of ground kept either side
    /// (`docs/DESIGN-stats-deck-units-2026-11-04.md` §3; the R3 lens fixes, 2026-11-06). At 1×
    /// row 0's caps stand in the device row under the rail, where an unbroken rail reads as the
    /// caps' own edge; at 2× they keep a px of ground under it, and the rail is whole. A piece
    /// under 3 px left between two caps of one band goes too, so no speck of rail stands between
    /// them. A piece across a gate stays: it belongs to two bands, and where one of them is the
    /// fine band its px outside that band's span would otherwise turn on a moving cap, outside
    /// the rect the live edge re-rasters for it; each break reaches only a px past its cap, which
    /// keeps a moving box's breaks inside that rect.
    fn rail_pieces((x0, x1): (f32, f32), y: f32, caps: &[(Rect, u8)]) -> Vec<(f32, f32)> {
        let mut breaks: Vec<(f32, f32, u8)> = caps
            .iter()
            .filter(|&&((_, cy, _, ch), _)| cy <= y + 1.0 && cy + ch >= y)
            .map(|&((cx, _, cw, _), band)| (cx - 1.0, cx + cw + 1.0, band))
            .collect();
        breaks.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut pieces = Vec::with_capacity(breaks.len() + 1);
        // The rail from `x`, and the band of the break it starts after.
        let (mut x, mut after) = (x0, None);
        for (a, b, band) in breaks {
            let a = a.min(x1);
            if a > x && (a - x >= 3.0 || after != Some(band)) {
                pieces.push((x, a));
            }
            if b > x {
                (x, after) = (b, Some(band));
            }
        }
        if x1 > x {
            pieces.push((x, x1));
        }
        pieces
    }

    /// The rail's `pieces` ([`Self::rail_pieces`]) along `y` in `ink`, as much of each as stands
    /// inside `x0..x1`.
    fn rail(&mut self, pieces: &[(f32, f32)], (x0, x1): (f32, f32), y: f32, ink: Rgb) {
        for &(a, b) in pieces {
            let (a, b) = (a.max(x0), b.min(x1));
            if b > a {
                self.rect((a, y, b - a, 1.0), 0.0, ink);
            }
        }
    }

    /// An event's cap pip, the rounded square `(x, y, w, h)` whose bottom row is the axis rail's
    /// (the second review of the R3 lens fixes, 2026-11-06): it stands up from the rail, as a
    /// gap's notch does, and its px on the rail's row are drawn only on the rail's `pieces`
    /// ([`Self::rail_pieces`]), so over row 0's cap, where the rail breaks at 1×, the pip keeps
    /// the break's device px of ground over the cap, and at 2× the ground row under the rail stays
    /// ground. Where the rail stands under all of it the pip is one panel; elsewhere each part is
    /// the same panel under a clip, so the pip keeps its corners.
    fn pip(&mut self, (x, y, w, h): Rect, pieces: &[(f32, f32)], radius: f32, ink: Rgb) {
        if !self.draw || w <= 0.0 || h <= 0.0 {
            return;
        }
        let fill = rgba(ink, 0xFF);
        let rail_y = y + h - 1.0;
        if pieces.iter().any(|&(a, b)| a <= x && x + w <= b) {
            self.p.prims.push(panel(x, y, w, h, radius, fill));
            return;
        }
        let above = (h > 1.0).then_some((x, y, w, h - 1.0));
        let feet = pieces.iter().filter_map(|&(a, b)| {
            let (a, b) = (a.max(x), b.min(x + w));
            (b > a).then_some((a, rail_y, b - a, 1.0))
        });
        for (cx, cy, cw, ch) in above.into_iter().chain(feet) {
            self.p.prims.push(DrawPrim::ClipPush {
                x: cx,
                y: cy,
                w: cw,
                h: ch,
            });
            self.p.prims.push(panel(x, y, w, h, radius, fill));
            self.p.prims.push(DrawPrim::ClipPop);
        }
    }

    /// The columns a time gap's notches stand in over its span `gx0..gx1` of the rail, its newer
    /// end's first (the second review's own review, 2026-11-06, and the third review). A notch
    /// stands only on a column of the span with a device px of ground between it and every pip,
    /// each `w` px wide from its left px in `pips`: vitals sets an aterm crash and its restart at a
    /// crash gap's two ends and a wake at a sleep gap's newer end, and a pip stands up from the
    /// rail as tall as a notch, so a notch on its end would stand under the pip, and one beside it
    /// would read as part of it. The newer end's notch stands on the first such column from its
    /// end, the older end's on the last, each on its end where no pip stands within a device px of
    /// it. The two keep a device px of ground between them too, unless both stand on their ends
    /// (a span 2 px wide): where one has stepped inward and they would stand side by side, as one
    /// mark 2 px wide in the gap's ink, only the one nearer its own end stands, the newer where
    /// they are as near. A span with no such column draws no notch: its pips mark its ends.
    fn notch_columns((gx0, gx1): (f32, f32), pips: &[f32], w: f32) -> [Option<f32>; 2] {
        let clear = |c: &f32| pips.iter().all(|&a| *c < a - 1.0 || *c >= a + w + 1.0);
        let mut cols = (gx0 as i32..gx1 as i32).map(|k| k as f32).filter(clear);
        match (cols.next(), cols.next_back()) {
            (Some(n), Some(o)) if o - n < 2.0 && (n > gx0 || o < gx1 - 1.0) => {
                [Some(if gx1 - 1.0 - o < n - gx0 { o } else { n }), None]
            }
            (newer, older) => [newer, older],
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
            self.p.prims.push(skyline::outline(
                (x, y, w, h),
                radius,
                (0.0, 0.0),
                rgba(ink, 0xFF),
            ));
        }
    }

    fn strip_spans(
        y0: f64,
        y1: f64,
        holes: &[(f64, f64)],
    ) -> impl Iterator<Item = (f64, f64)> + '_ {
        let mut y = y0;
        let mut holes = holes.iter().copied();
        let mut tail = Some(y1);
        std::iter::from_fn(move || {
            for (a, b) in holes.by_ref() {
                let start = y;
                y = y.max(b);
                if a > start {
                    return Some((start, a));
                }
            }
            tail.take().filter(|&end| y < end).map(|end| (y, end))
        })
    }

    /// Draws the strip's X-axis baseline rail (broken over a row-0 over cap it would touch,
    /// [`Self::rail_pieces`]), discontinuity break notches over [`TimeGap`]s, whose span of the
    /// rail takes the gap's ink px for px (the grid rail's own pieces, clipped to the gap, so it
    /// breaks and drops exactly where they do: the R3 lens fixes' review, 2026-11-06) and whose
    /// notches stand on that span of the rail, their foot its px (that review's own review), and
    /// vertical event marker lines + cap pips for [`StripEvent`]s (`Crash`, `Restart`,
    /// `SleepWake`, `PressureSpike`), each pip standing up from the rail, its foot the rail's px
    /// ([`Self::pip`]; the second review, 2026-11-06). The events are placed first, so a notch
    /// an event's pip would stand on steps inward clear of it ([`Self::notch_columns`]; the second
    /// review's own review, 2026-11-06), and the gaps are drawn before the pips, so each pip
    /// stands over a gap's rail and keeps its foot there.
    fn strip_axis_and_events(
        &mut self,
        strip: &Strip,
        base: f64,
        y0: f64,
        last: f64,
        holes: &[(f64, f64)],
        words: &DeckWords,
    ) {
        let caps = std::mem::take(&mut self.caps);
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
        let pieces = Self::rail_pieces((x0, x1), y_axis, &caps);
        self.rail(&pieces, (x0, x1), y_axis, self.inks.grid);
        let Some(now_ms) = self.at_ms else {
            return;
        };
        let horizon = strip.spec.horizon_ms();
        if words.strip_events.is_empty() && words.time_gaps.is_empty() {
            return;
        }
        // Each event's pip and line, `(pip's left px, line's left px, kind)`, placed before the
        // gaps are drawn, so a gap's notch can stand clear of every pip.
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
        // The existing six-column limit bounds the markers and their pip projection.
        const MARK_LIMIT: usize = 6;
        let mut drawn_xs: aterm_alloc::ArrayVec<i32, MARK_LIMIT> = aterm_alloc::ArrayVec::new();
        let lw = (0.85 * self.s).round().max(1.0) as f32;
        let pip = (2.2 * self.s).round().max(2.0) as f32;
        let mut marks: aterm_alloc::ArrayVec<(f32, f32, StripEventKind), MARK_LIMIT> =
            aterm_alloc::ArrayVec::new();
        for ev in events {
            if drawn_xs.len() >= MARK_LIMIT {
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
            let lx = (ex - lw / 2.0).round().clamp(x0, (x1 - lw).max(x0));
            let px_pip = (ex - pip / 2.0).round().clamp(x0, (x1 - pip).max(x0));
            marks.push((px_pip, lx, ev.kind));
        }
        let pips: aterm_alloc::ArrayVec<f32, MARK_LIMIT> = marks.iter().map(|m| m.0).collect();
        for gap in words.time_gaps.iter().rev().take(4) {
            if gap.end_ms <= now_ms - horizon || gap.start_ms >= now_ms {
                continue;
            }
            let age_start = (now_ms - gap.start_ms).clamp(0, horizon);
            let age_end = (now_ms - gap.end_ms).clamp(0, horizon);
            let (Some(old), Some(new)) = (strip.x_at_age_ms(age_start), strip.x_at_age_ms(age_end))
            else {
                continue;
            };
            // The gap's start is the older end, right of its end (x grows with age).
            let (gx_old, gx_new) = (
                ((old * self.s) as f32).round().clamp(x0, x1),
                ((new * self.s) as f32).round().clamp(x0, x1),
            );
            let (gx0, gx1) = (gx_new.min(gx_old), gx_new.max(gx_old));
            if gx1 > gx0 {
                let gink = self.gap_ink(gap.kind);
                self.rail(&pieces, (gx0, gx1), y_axis, gink);
                // Each end's notch stands up from the rail's row; its foot on that row is the gap's
                // own rail, just drawn, so where the rail breaks over a cap the notch does too and
                // keeps a device px of ground over the cap. Where an event's pip stands on an end
                // or within a device px of its notch, the notch steps inward, clear of the pip
                // ([`Self::notch_columns`]).
                let nh = (2.0 * self.s as f32).round().max(2.0);
                for c in Self::notch_columns((gx0, gx1), &pips, pip)
                    .into_iter()
                    .flatten()
                {
                    self.rect((c, y_axis + 1.0 - nh, 1.0, nh - 1.0), 0.0, gink);
                }
            }
        }
        if marks.is_empty() {
            return;
        }
        for &(px_pip, _, kind) in &marks {
            let pip_rect = (px_pip, y_axis + 1.0 - pip, pip, pip);
            let ev_ink = self.event_ink(kind);
            self.pip(pip_rect, &pieces, (0.5 * self.s) as f32, ev_ink);
        }
        let rows: Vec<(f32, f32)> = Self::strip_spans(y0, last, holes)
            .map(|(a, b)| (self.px(a), self.px(b)))
            .filter(|(ya, yb)| yb > ya)
            .collect();
        // A device px of ground each side keeps a line off the boxes' columns and ticks
        // (`docs/DESIGN-stats-deck-units-2026-11-04.md` §3, "States"). Every keyline goes down
        // before any line: two events' lines can touch, and a keyline drawn after its neighbour's
        // line would cut it.
        let ground = rgba(self.inks.ground, 0xFF);
        for &(_, lx, _) in &marks {
            for &(ya, yb) in &rows {
                for kx in [lx - 1.0, lx + lw] {
                    self.p.prims.push(panel(kx, ya, 1.0, yb - ya, 0.0, ground));
                }
            }
        }
        for &(_, lx, kind) in &marks {
            let ev_ink = self.event_ink(kind);
            for &(ya, yb) in &rows {
                match kind {
                    StripEventKind::Crash | StripEventKind::Restart => {
                        let alpha = if kind == StripEventKind::Crash {
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
                start: 0.0,
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
                    start: 0.0,
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

    /// The bar's segments a click picks a scope by, pt from its left edge: this tab's fill, and
    /// aterm's past it ([`BarFill::widths_px`], as [`Self::bar`] draws them). A scope that draws
    /// a fill takes at least [`SEGMENT_HIT_MIN`] of the bar, so a tab using little is still a
    /// target and not a pixel or two.
    fn bar_segments(&self, col: Col, fill: &BarFill) -> [(f64, f64, Bar); 2] {
        let s = self.s as f32;
        let w = self.px(col.w);
        let [_, aw, tw] = fill.widths_px(w, s.round().max(1.0), 0.0);
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
        let choice = choose_details(clauses, col.w + 0.01, width);
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
    /// length alone: each fill ends a device px or more short of the fill around it
    /// ([`BarFill::widths_px`]'s `apart`), or the outer ink would hide under the inner fill
    /// wherever the two round alike. Its edges fall on whole device px; the machine's fill above
    /// zero keeps one at least, and a scope above zero keeps one where the fill around it is two
    /// or more, while inside a fill of one px it draws none, giving way. With `h` from
    /// [`Self::under_h`] the bar ends a device px or more short of the next
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
        prev_row: Option<(Option<f32>, Option<BarFill>)>,
        reading: bool,
    ) {
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
                let cap_room = self.cap_room();
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
                let live_box = BoxValue {
                    mean: None,
                    max: row.live_peak,
                    over: row.live_over,
                };
                self.strip(&t.strip, top, rows.cell, slots, (live_heat, live_box));
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
        if let Some(col) = t.meter.run {
            let state = row.state.as_ref().filter(|_| t.details.is_none());
            self.run_cell(col, base, rows.amount_pt, &row.run, state);
        }
        if let Some(col) = t.details {
            self.details(col, base, &row.details[t.said()]);
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
    /// [`TICK_MIN_PX`] tall, so at 1× it reads as a tick and not a stray dot. It ends on the axis
    /// rail's foot, short of the strip's top in both forms. At 2× a device px of ground stands
    /// between the two and row 0's over caps; at 1× a cap stands flush under them, never in them
    /// (`the_over_cap_sits_outside_the_plate_and_clear_of_the_row_above`), and the rail breaks
    /// over it (`the_rail_breaks_over_a_cap_it_would_touch`).
    fn tick_mark(&mut self, x: f64, base: f64) {
        let (x, w) = tick_px(x, self.s);
        let y0 = self.px(base + 1.0);
        let y1 = self.px(base + 2.5).max(y0 + TICK_MIN_PX);
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
                // A hot box drawn as the strip draws one, its whole-length cap at the swatch's top.
                let (cx, cy, cw, bottom) = (self.px(x), self.px(top), self.px(4.0), self.px(base));
                let (clear, cap_h) = skyline::cap_px(self.s);
                let y = cy + cap_h + clear;
                let full = BoxValue {
                    mean: Some(1.0),
                    max: None,
                    over: 1.0,
                };
                let sky = skyline::skyline_box(
                    (cx, y, cw, bottom - y),
                    full,
                    self.s,
                    &self.inks,
                    self.ic,
                );
                self.skyline(&sky, 0.0);
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
            over: [DeckWords::over_shown(&words.rows, &plan.strip), false],
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
            .filter(|(row, _)| row.strip_text.is_some())
            .map(|(_, r)| (rows.top(r) - gap / 2.0, rows.top(r) + rows.cell + gap / 2.0))
            .collect();
        let last = rows.top(DECK_RESOURCES.len() as u32 - 1) + rows.cell;
        let mut cores_row_hit = None;
        if let Some(GaugeSlot { at, divider }) = plan.gauge {
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
        if let Some(room) = plan.cores {
            let r = DECK_RESOURCES.len() as u32;
            let top = rows.top(r);
            let ink = self.inks.words;
            self.text(
                plan.name.x,
                top + rows.baseline,
                CORES_NAME.into(),
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
        if let Some(bar) = plan.meter.bar {
            let br = self.px_rect(bar.x - 2.0, rows.top(0) - 2.0, bar.end() + 2.0, last + 2.0);
            self.live_rects.push((LiveLayer::Values, br));
        }
        if let Some(col) = plan.bar_under() {
            let rect = self.under_rect(col, &rows, words.rows.len());
            self.live_rects.push((LiveLayer::Values, rect));
        }
        for (row, r) in words.rows.iter().zip(0_u32..) {
            let prev_row = words
                .prev
                .as_ref()
                .and_then(|p| p.rows.get(r as usize))
                .copied();
            self.row(&t, r, row, prev_row, words.reading);
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
            over: [0, 1].map(|bi| {
                DeckWords::over_shown(
                    words.rows.iter().skip(bi * 4).take(4),
                    &plan.blocks[bi].strip,
                )
            }),
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
                .filter(|(row, _)| row.strip_text.is_some())
                .map(|(_, r)| (rows.top(r) - gap, rows.top(r) + rows.cell + gap))
                .collect();
            let last = rows.top(ours.len().saturating_sub(1) as u32) + rows.cell;
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
                let prev_row = words
                    .prev
                    .as_ref()
                    .and_then(|p| p.rows.get(row_ix))
                    .copied();
                self.row(&t, r, row, prev_row, words.reading);
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
                        CORES_NAME.into(),
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
        if input.contrast {
            network_inks.heat = network_inks.heat_ic;
        }
        let empty = crate::lab_deck::LabDeck::default();
        let (prims, network_hits) = crate::network_deck::paint_with_inks(
            g,
            resource_geom.h,
            snapshot,
            input.lab.unwrap_or(&empty),
            input.fleet,
            input.network_place,
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
    pub(crate) network_place: Option<crate::network_deck::PlaceCaption>,
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
            network_place: crate::network_deck::capture_place_caption(),
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
                    network_place: crate::network_deck::capture_place_caption(),
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
                // The card stops where the drop-target border begins.
                let card_h = crate::stats_sill::sill_card_px(sill, w, frame_h);
                if card_h == 0 {
                    return None;
                }
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
                    network_place: ws
                        .stats_lens
                        .open
                        .filter(|lens| lens.focus == crate::stats_paint::Instrument::NetworkState)
                        .and_then(|_| crate::network_deck::capture_place_caption()),
                    plate,
                    geom: DeckGeom {
                        w: w as f32,
                        h: card_h as f32,
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
        self.network_snapshot.network.accounts_exchange.hash(&mut h);
        crate::network_deck::hash_peer_reachability(&self.network_snapshot, &mut h);
        self.network_snapshot.services.hash(&mut h);
        self.lab_deck_feed.revision().hash(&mut h);
        if f.form == StatsBars::Deck {
            f.network_place.hash(&mut h);
            self.fleet_field
                .as_ref()
                .map(|field| field.key)
                .hash(&mut h);
        }
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
    pub(crate) fn deck_input<'a>(&'a self, f: &'a DeckFrame) -> DeckInput<'a> {
        let snap = self.vitals.snapshot.as_deref();
        DeckInput {
            network: (f.form == StatsBars::Deck).then_some(self.network_snapshot.as_ref()),
            network_place: f.network_place.as_ref(),
            network_target: crate::network_deck::Target::Overview,
            lab: (f.form == StatsBars::Deck).then_some(self.lab_deck.as_ref()),
            fleet: (f.form == StatsBars::Deck)
                .then_some(self.fleet_field.as_deref())
                .flatten(),
            snap,
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
            locale: host_locale(snap),
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
            && let Some(rows) = patch_live_rows(
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
            ws.deck_motion.card_changed(rows);
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
            serial: 0,
            delta: None,
        });
        ws.deck_motion.card_changed((0, ph));
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
            card.serial.hash(&mut identity);
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
/// untouched) when a rect leaves the card, so the caller rasters it whole. The splice calls
/// [`patch_live_rows`], which says which rows it drew.
#[cfg(test)]
pub(crate) fn patch_live(
    card: &mut crate::SettingsCard,
    prims: &[DrawPrim],
    rects: &[(LiveLayer, Rect)],
    was: &DeckLive,
    now: &DeckLive,
) -> bool {
    patch_live_rows(card, prims, rects, was, now).is_some()
}

/// [`patch_live`], saying which of the card's rows it re-rastered: `y0..y1` around every rect it
/// drew, `(0, 0)` where no rect moved; `None` where [`patch_live`] says `false`.
pub(crate) fn patch_live_rows(
    card: &mut crate::SettingsCard,
    prims: &[DrawPrim],
    rects: &[(LiveLayer, Rect)],
    was: &DeckLive,
    now: &DeckLive,
) -> Option<(u32, u32)> {
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
        return None;
    }
    let rows = regions
        .iter()
        .map(|r| (r.y, r.y + r.height))
        .reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)))
        .unwrap_or((0, 0));
    for r in regions {
        let px = crate::tray_raster::rasterize_tray_region(prims, 1.0, [0, 0, 0, 0], r);
        let row = r.width as usize * 4;
        if px.len() != row * r.height as usize {
            return None;
        }
        for (i, src) in px.chunks_exact(row).enumerate() {
            let at = (r.y as usize + i) * stride + r.x as usize * 4;
            let dst = card.rgba.get_mut(at..at + row)?;
            dst.copy_from_slice(src);
        }
    }
    Some(rows)
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
        serial: 0,
        delta: None,
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

/// Host [`Locale`] for `snap`, carrying per-instant local offsets across daylight-saving changes.
pub(crate) fn host_locale(snap: Option<&Snapshot>) -> Locale {
    let utc_offset_s = snap
        .and_then(|s| crate::presence::local_offset_at(s.at_unix_ms.div_euclid(1000)))
        .or_else(crate::presence::local_offset_s)
        .unwrap_or(0);
    Locale {
        platform: Platform::host(),
        utc_offset_s,
        utc_offset_at: Some(crate::presence::local_offset_at),
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
        let loc = host_locale(self.vitals.snapshot.as_deref());
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
        for (name, words) in crate::network_deck::meters(
            &self.network_snapshot,
            &self.lab_deck,
            self.fleet_field.as_deref(),
            (f.geom.w, f.geom.scale),
        ) {
            group.meters.push(crate::a11y_tree::StatsMeter {
                name,
                value: None,
                words,
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
#[path = "stats_deck_tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "stats_deck_paint_tests.rs"]
pub(crate) mod paint_tests;

#[cfg(test)]
#[path = "stats_live_paint_tests.rs"]
mod live_paint_tests;
