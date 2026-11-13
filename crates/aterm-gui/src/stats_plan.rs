// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Layout planner for the stats deck.
use aterm_vitals::MultiScaleSpec;
use aterm_vitals::words::TotalsForm;

use crate::deck_face::FaceMetrics;
use crate::stats_deck::{CORES_NAME, DECK_RESOURCES};
use crate::stats_paint::Bar;
use crate::stats_units::{Form, Forms, PLAN_SCALE, UnitFacts, UnitRow};

/// Stats deck layout form.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum DeckForm {
    /// Two blocks of rows in one band ([`DeckRows::folded_pt`]).
    #[default]
    Folded,
    /// Header line and one table of rows in full height ([`DeckRows::full_table_pt`]), only
    /// where `stats_deck_size` is full.
    Full,
}

/// The rows a deck holds, and so its height: the deck's height is a constant of its rows
/// (`docs/DESIGN-stats-deck-instruments-2026-11-06.md` §11, "The rule", A4), which no reading,
/// machine or width changes. A row with no reader keeps its place and says so; units come and go
/// by width, rows never do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum DeckRows {
    /// The record's rows, which the deck paints until A4's step N5: in Folded, block A's CPU,
    /// GPU, Memory and Disk and block B's Network, Processes, an empty row and Cores (62 pt); in
    /// Full, the six resource rows and a spare in wide and mid, and the Cores row and a spare in
    /// narrow and slim at a 10.5 pt pitch (100 pt).
    Resources,
    /// A4's instruments (§3, §11): in Folded, block A's CPU, GPU, Memory, Disk and Heat and block
    /// B's Network, Processes, Latency (empty until F8), Cores and Machine (73.5 pt); in Full,
    /// CPU, GPU, Memory, Disk, Heat, Network, Latency, Processes, Cores and Machine at every kind
    /// and the 12 pt pitch (136 pt).
    ///
    /// Outside tests only [`plan_folded_with`] and [`plan_full_with`] make it, and each carries
    /// its own `expect(dead_code)` until N5 paints with it. The variant carries none: rustc 1.96
    /// and earlier (the cells lint on rustup's unpinned `stable`, which can be one) count what an
    /// expected-dead item names as live, so an expectation here would go unfulfilled there
    /// (`docs/measured/main-reds-2026-10-20.md` §1.1).
    Instruments,
}

/// Folded's first row's top, pt below the deck's top (A1 §2.2).
const FOLD_TOP0: f64 = 14.0;
/// Folded's row pitch, pt.
const FOLD_PITCH: f64 = 11.5;
/// Folded's strip cell, pt: 17 device px at 2×, 9 at 1×.
const FOLD_CELL: f64 = 8.5;
/// Folded's band below its last row's pitch, pt: a band of n rows is 14 + n × 11.5 + 2 pt.
const FOLD_FOOT: f64 = 2.0;
/// What the run-queue tile takes past the Cores row's squares, pt at 1× (A4 §7): A1 §4.1's wall,
/// a device px with a point clear each side, then its 3 pt tile.
const CORES_QUEUE: f64 = 3.0 + 3.0;
/// The names the instruments' rows draw in the name column besides each resource's and the
/// Cores row's (A4 §3): the Machine row's name is the computer's type, in its own slot
/// ([`MachineRow`]).
const INSTRUMENT_NAMES: [&str; 2] = ["Heat", "Latency"];

impl DeckRows {
    /// The rows the deck paints. A4's step N5 paints [`Self::Instruments`], with the host's facts
    /// ([`plan_folded_with`], [`plan_full_with`]), and deletes [`Self::Resources`] with this.
    pub(crate) const PAINTED: Self = Self::Resources;

    /// Folded's rows in each block.
    const fn folded_count(self) -> u32 {
        match self {
            Self::Resources => 4,
            Self::Instruments => 5,
        }
    }

    /// Full's rows, and their pitch, at `kind`: the record's narrow and slim tables fit eight
    /// rows in 100 pt at 10.5 pt; the instruments' take the 12 pt pitch at every kind, keeping
    /// their cell and type sizes, so one Full height serves every kind (A4 §11).
    const fn full_rows(self, kind: FullKind) -> (u32, f64) {
        match (self, kind) {
            (Self::Resources, FullKind::Wide | FullKind::Mid) => (7, 12.0),
            (Self::Resources, FullKind::Narrow | FullKind::Slim) => (8, 10.5),
            (Self::Instruments, _) => (10, 12.0),
        }
    }

    /// The Folded deck's height, pt: its first row's top, its rows' pitches and the foot (62 and
    /// 73.5, A4 §11).
    pub(crate) const fn folded_pt(self) -> f64 {
        FOLD_TOP0 + self.folded_count() as f64 * FOLD_PITCH + FOLD_FOOT
    }

    /// The Full table's height, pt, before what it adds to the Folded height is rounded up to
    /// whole terminal cells: its first row's top and its rows' pitches, the same at every kind
    /// (100 and 136, A4 §11).
    pub(crate) const fn full_table_pt(self) -> f64 {
        let rows = Rows::full(FullKind::Wide, self);
        rows.top0 + rows.count as f64 * rows.pitch
    }

    /// What the Full form adds to the Folded height, device px at `scale`, rounded up to whole
    /// terminal cells of `cell_px`: the tab re-grids only when the form changes.
    pub(crate) fn full_delta_px(self, scale: f64, cell_px: usize) -> usize {
        if !(scale.is_finite() && scale > 0.0) {
            return 0;
        }
        let need = ((self.full_table_pt() - self.folded_pt()) * scale).round() as usize;
        let cell = cell_px.max(1);
        need.div_ceil(cell) * cell
    }

    /// The Folded dial's centre, pt below the deck's top: the middle of the band.
    const fn gauge_cy(self) -> f64 {
        match self {
            Self::Resources => 31.0,
            Self::Instruments => FOLD_GAUGE_CY,
        }
    }

    /// Whether Full at `kind` holds the Cores row: the record's narrow and slim, whose slot left
    /// of the names holds no gauge; the instruments' at every kind (A4 §7).
    const fn full_cores_row(self, kind: FullKind) -> bool {
        match self {
            Self::Resources => matches!(kind, FullKind::Narrow | FullKind::Slim),
            Self::Instruments => true,
        }
    }

    /// The names its rows draw in the name column besides each resource's and the Cores row's:
    /// the instruments' Heat and Latency rows' ([`INSTRUMENT_NAMES`]); the record's rows draw none.
    const fn row_names(self) -> &'static [&'static str] {
        match self {
            Self::Resources => &[],
            Self::Instruments => &INSTRUMENT_NAMES,
        }
    }

    /// Whether it holds the Machine row (A4 §8): block B's last in Folded, Full's last.
    const fn machine_row(self) -> bool {
        matches!(self, Self::Instruments)
    }

    /// What the Cores row's square forms take past their clusters, pt: the instruments' run-queue
    /// tile ([`CORES_QUEUE`]); the record's row draws none.
    const fn cores_queue(self) -> f64 {
        match self {
            Self::Resources => 0.0,
            Self::Instruments => CORES_QUEUE,
        }
    }
}

/// Configured `[sys] stats_deck_size` preference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum DeckSizePref {
    /// `Folded` whatever the window's height (the Full form had too
    /// many details).
    #[default]
    Auto,
    /// Always use `Full`.
    Full,
    /// Always use `Folded`.
    Folded,
}

impl DeckSizePref {
    /// All preferences in menu order.
    pub(crate) const ALL: [Self; 3] = [Self::Auto, Self::Full, Self::Folded];

    /// Parses `[sys] stats_deck_size`, defaulting to `Auto`.
    pub(crate) fn parse(value: Option<&str>) -> Self {
        value.and_then(Self::from_word).unwrap_or_default()
    }

    /// Returns the config string for this preference.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Full => "full",
            Self::Folded => "folded",
        }
    }

    /// Returns the menu title for this preference.
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Auto => "Automatic",
            Self::Full => "Full",
            Self::Folded => "Folded",
        }
    }

    /// Parses a strict config word into a [`DeckSizePref`].
    pub(crate) fn from_word(word: &str) -> Option<Self> {
        let word = word.trim();
        Self::ALL
            .into_iter()
            .find(|pref| pref.word().eq_ignore_ascii_case(word))
    }
}

/// The [`DeckForm`] `pref` gives: Full only where chosen, whatever the window's height.
pub(crate) const fn deck_form_for(pref: DeckSizePref) -> DeckForm {
    match pref {
        DeckSizePref::Full => DeckForm::Full,
        DeckSizePref::Auto | DeckSizePref::Folded => DeckForm::Folded,
    }
}

const X0: f64 = 12.0;
const MIN_TABLE_W: f64 = 160.0;
const DETAILS_MIN: f64 = 120.0;
const DETAILS_MAX: f64 = 560.0;
const BLOCK_GUTTER: f64 = 29.0;
const CORES_BLOCK_W: f64 = 68.0;
/// From the meter's end (its figure run, else its bar) to Details, pt.
const DETAILS_GAP: f64 = 10.0;
/// The least Details a Folded wide block lays, pt.
const FOLD_DETAILS_MIN: f64 = 130.0;
/// Details' share of a Full table's room from its strip's start, before the strip takes its own.
const DETAILS_SHARE: f64 = 0.18;
/// From the bar's end to the figure run, pt.
const RUN_GAP: f64 = 6.0;
/// The widest figure run's column where no Details stands past it, pt: a reading with its
/// maximum and a state's words after it (`112.6 of 128 GiB ● pressure warn`).
const RUN_MAX: f64 = 170.0;
/// The widest Folded block: its widest names, % and gaps (101 pt on Helvetica Neue), the
/// widest meter Folded wide lays ([`FOLD_WIDE`]) and the widest Details. Wider decks centre the
/// table.
const B_MAX: f64 = 101.0
    + FOLD_WIDE.strip.1
    + FOLD_WIDE.bar_gap
    + FOLD_WIDE.bar.1
    + RUN_GAP
    + FOLD_WIDE.run
    + DETAILS_GAP
    + DETAILS_MAX;
/// The least room between a scope's words in the legend over the bars and the next scope's
/// swatch, pt.
const LEGEND_GAP: f64 = 8.0;
/// The widest a legend's tab title stands, pt, before it is fitted (`Tab 1` is 25).
const TAB_HEAD_MAX: f64 = 48.0;
/// The strain gauge at the Folded deck's top left (the refined deck's Folded form, 2026-10-24):
/// its dial's radius, pt. The dial is 54 pt across (Full's is 62) and sat 4 pt inside the record's
/// 62 pt band at its top and its foot, the largest that band held with room for the bezel's
/// bloom; the instruments' taller band keeps it (A4 §2), 9.75 pt clear of each edge.
pub(crate) const FOLD_GAUGE_R: f64 = 27.0;
/// The dial's centre in the instruments' Folded band, pt below the deck's top: the middle of its
/// 73.5 pt (A4 §2, §11). The record's band centres it at 31.
pub(crate) const FOLD_GAUGE_CY: f64 = 36.75;
/// From the dial's right edge to its divider, and from the divider to block A, pt (Full's slot
/// leaves 11 and 9).
const FOLD_GAUGE_GAP: f64 = 9.0;
/// What the gauge takes from the blocks' width: the dial, its divider and the two gaps.
const FOLD_GAUGE_W: f64 = 2.0 * FOLD_GAUGE_R + 2.0 * FOLD_GAUGE_GAP;
/// From a units column's end to its % column, pt: each row's form ends against its % (A1 §2.1,
/// §2.2: units 148–194, % 197–227 at 1039 pt).
const UNITS_TO_PCT: f64 = 3.0;
pub(crate) const HEADER_PT: f64 = 9.5;
/// From a row's baseline to the top of the three-scope bar it draws under its % figure where no
/// bar column fits, pt (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §5): the digits, `<` and
/// `%` have no descenders, so a point clears them.
pub(crate) const UNDER_GAP: f64 = 1.0;
/// The height of that bar, pt: with [`UNDER_GAP`] it ends 3 pt below the baseline, inside the
/// row's pitch (1 pt clear of the next row in Folded, 0.5 pt in Full narrow and slim). At 1× in
/// Full, where the rows whose top falls on a half point hold 1 device px between the line's top
/// and a px short of the next row, the painter draws every row's line 1 px tall (`under_h` in
/// `stats_deck.rs`).
pub(crate) const UNDER_H: f64 = 2.0;
/// What a strip's header keeps clear at its left end, now, for the now marker and its beat
/// ([`crate::stats_live`]), pt.
pub(crate) const MARKER_ROOM: f64 = 8.0;
/// From the % column's end to its strip's start, pt. The % (the value now) stands between the
/// well and the strip (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §3.1), and the now marker's
/// line between the column and the strip: centred [`crate::stats_live::LINE_DX`] left of the
/// strip and at most a point wide, so its left edge stands 4 pt past the column's end at 1× and
/// at 2×, with the digits, the line and the live box reading as one place.
pub(crate) const PCT_TO_STRIP: f64 = 7.0;
/// Where the header's words left of a strip's now marker end, pt before the strip: a point past
/// the widest ring the marker's dot beats, centred [`crate::stats_live::LINE_DX`] left of the strip
/// and reaching [`crate::stats_live::DOT_R`] + [`crate::stats_live::BEAT_REACH`] from there. The
/// `now` over the % column ends here, about 3 pt short of the digits' right end (the ring drew over
/// it where it ended with them, the review of 2026-10-26), and so do state words that take the
/// column's header and, where there is no % column, words over the names.
pub(crate) const WORDS_TO_STRIP: f64 =
    crate::stats_live::LINE_DX + crate::stats_live::DOT_R + crate::stats_live::BEAT_REACH + 1.0;
/// From the well's end to its strip's start where no % column stands between them (under slim's
/// width, where the % goes first), pt: the now marker's line, [`crate::stats_live::LINE_DX`] left
/// of the strip and at most a point wide, then starts a point past the well's end, clear of the
/// widest row mark the well draws (0.36 of its size each side of the well's middle,
/// `Deck::mark`).
const BARE_STRIP_GAP: f64 = 4.0;
/// A Folded strip's slot, seconds: its `Strip::period_s`, which marks a strip as Folded (the live
/// edge slides a Folded strip's fine band and glides a Full one's), and the snapshot's slot, one
/// reading's second at the focused window's cadence (`aterm_vitals::ring::SLOT_MS`). It sets
/// neither how far a reading moves the strip nor what the strip spans: each reading that opens a
/// slot slides the fine band right by the time it opened over the fine box's period, half a 2 s
/// box ([`crate::stats_live::FineBand::step_px`], `docs/DESIGN-stats-deck-now-left-2026-10-26.md`
/// §3.4), and the strip spans its multi-scale horizon ([`Strip::span_s`]). The step's lever is the
/// fine band's share of its preset's weights in `aterm_vitals::ring` (live §8, 2026-10-26).
pub(crate) const FOLD_SLOT_S: u32 = 1;
/// The least room between a tick's words and the words beside them, pt.
const TICK_GAP: f64 = 6.0;
/// How far a tick's words may stand off centre over it, as a share of their width: the tick
/// stays under their middle half, so "–5 min" never reads as a minute later.
const TICK_SLACK: f64 = 0.25;
/// What the header keeps clear between words and the place they lead to, pt: a key span over the
/// names ends this far before the `now` over the % column, and a time label standing beside the
/// strip starts this far past its right end. Where there is no % column, words over the names end
/// [`WORDS_TO_STRIP`] before the strip, clear of the now marker's beat.
pub(crate) const STRIP_LEAD: f64 = 6.0;
const DETAILS_LONG: &str = "Details · point at a row for more";
const SWATCH_ADVANCE: f64 = 0.62 * HEADER_PT + 3.0;
const MARK_ADVANCE: f64 = 1.1 * HEADER_PT;
pub(crate) const KEY_GAP: f64 = 10.0;

/// The name column at `pt`: `base`, or the widest name it draws and a margin. It draws each deck
/// row's name ([`DECK_RESOURCES`]), the Cores row's and `more`, the names the rows of a
/// [`DeckRows`] draw besides ([`DeckRows::row_names`]). "Processes" stays the widest on both
/// faces, so the instruments' Heat and Latency rows move no name column
/// (`the_instruments_rows_move_no_column`).
fn name_width(base: f64, pt: f64, m: &FaceMetrics, more: &[&str]) -> f64 {
    let widest = DECK_RESOURCES
        .map(aterm_vitals::words::resource_name)
        .into_iter()
        .chain([CORES_NAME])
        .chain(more.iter().copied())
        .map(|name| m.width(name, pt, true))
        .fold(0.0, f64::max);
    base.max((widest + 1.5).ceil())
}

/// The legend's least width on the header line, whole pt: `■ Mac ■ aterm ■ Tab`, the machine's
/// noun and the tab's title at their shortest ([`Header::legend`]).
fn legend_least(m: &FaceMetrics) -> f64 {
    let words = |s: &str| m.width(s, HEADER_PT, false);
    (3.0 * SWATCH_ADVANCE + words("Mac") + words("aterm") + words("Tab") + 2.0 * LEGEND_GAP).ceil()
}

const fn whole_width(w_pt: f64) -> f64 {
    if w_pt.is_finite() {
        w_pt.max(0.0).floor()
    } else {
        0.0
    }
}

/// Horizontal column span in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Col {
    pub(crate) x: f64,
    pub(crate) w: f64,
}

impl Col {
    const fn new(x: f64, w: f64) -> Self {
        Self { x, w }
    }

    /// Right edge (`x + w`) in points.
    pub(crate) fn end(self) -> f64 {
        self.x + self.w
    }
}

/// The least strip that holds the owner's six-hour preset, pt ([`Strip::spec_for`]).
const SIX_HOURS: f64 = 239.0;
/// The least strip that holds the four-hour preset, pt.
const FOUR_HOURS: f64 = 129.0;
/// The least strip that holds the six-minute preset, pt; a narrower one holds a minute.
const SIX_MINUTES: f64 = 79.0;
/// The least strip a % column stands beside, pt (A1 step R5): six minutes. Where a narrower
/// block's strip would hold only a minute beside it, the % goes and the strip takes its room
/// (Folded under 389 pt on Helvetica Neue), so the % and the gauge each come where the strip
/// beside them keeps the six minutes it held without them.
const PCT_STRIP: f64 = SIX_MINUTES;

/// One table's meter measures, pt (`docs/DESIGN-stats-deck-meter-2026-10-26.md` §2): how the
/// room right of the % column is shared between the strip, the scope bar and the figure run.
#[derive(Clone, Copy, Debug, PartialEq)]
struct MeterSpec {
    /// The least strip beside a bar column, and the most. The least is a preset's (A1 step R5):
    /// a bar column comes only where the strip beside it holds what the strip held with the bar
    /// under each %, four hours in Folded and six in Full, so its arrival never shortens the
    /// horizon.
    strip: (f64, f64),
    /// The strip's box pitch: it is a whole number of them wide, less the last box's gap.
    pitch: f64,
    /// The strip's share of the room (Details aside): the time series is the row's widest part.
    share: f64,
    /// From the strip's last box's pitch to the bar.
    bar_gap: f64,
    /// The bar's least and most width.
    bar: (f64, f64),
    /// The least bar with no run beside it, where that is longer than the bar's least: Full's,
    /// as long as the legend over it ([`legend_least`], set where the plan is laid).
    bar_alone: f64,
    /// The figure run's column: this Mac's reading with the row's maximum (`112.6 of 128 GiB`).
    run: f64,
    /// The least Details the table lays where room is left, and the least strip it leaves beside
    /// them; `None` where it lays none.
    details: Option<(f64, f64)>,
    /// The least strip where each row's bar stands under its % instead, where no bar column
    /// stands beside the strip; `None` where the bar never stands there.
    under: Option<f64>,
    /// The least strip a % column stands beside ([`PCT_STRIP`]): a table whose strip is
    /// narrower beside it does not fit with its %.
    pct_strip: f64,
}

/// Full wide: six hours' strip from 239 pt taking 0.65 of the room, a bar of 40 to 240 pt, a run
/// at 10 pt, and Details where they leave the strip 360 pt.
const FULL_WIDE: MeterSpec = MeterSpec {
    strip: (SIX_HOURS, 600.0),
    pitch: 5.0,
    share: 0.65,
    bar_gap: 8.0,
    bar: (40.0, 240.0),
    bar_alone: 0.0,
    run: 88.0,
    details: Some((DETAILS_MIN, 360.0)),
    under: None,
    pct_strip: PCT_STRIP,
};

/// Full mid: no Details, a 4 pt pitch, and the strip at 0.6 of the room, which keeps the run
/// beside a bar as long as it down to about 700 pt.
const FULL_MID: MeterSpec = MeterSpec {
    pitch: 4.0,
    share: 0.6,
    details: None,
    ..FULL_WIDE
};

/// Full narrow: six hours' strip at a 5 pt pitch. Its least was 99 pt until A1's step R5: a
/// narrow table first fits its bar column where the strip with the bar under each % already
/// holds six hours (from 354 pt on Helvetica Neue), so a shorter strip beside the column
/// shortened the horizon as the deck widened.
const FULL_NARROW: MeterSpec = MeterSpec {
    pitch: 5.0,
    bar_gap: 6.0,
    ..FULL_MID
};

/// Full slim: narrow's strip at a 4 pt pitch and a run at 9.5 pt, and where no bar column fits
/// beside six hours, the bar under each % beside a strip from 79 pt.
const FULL_SLIM: MeterSpec = MeterSpec {
    pitch: 4.0,
    run: 72.0,
    under: Some(SIX_MINUTES),
    ..FULL_NARROW
};

/// Folded wide: a strip from 129 pt taking eight tenths of the block's room (six tenths of the
/// block at the owner's 940 to 1040 pt), a bar of 32 to 160 pt, a run at 9.5 pt where the bar
/// is at least as long, and Details where they leave the strip 480 pt.
const FOLD_WIDE: MeterSpec = MeterSpec {
    strip: (FOUR_HOURS, 600.0),
    pitch: 5.0,
    share: 0.8,
    bar_gap: 6.0,
    bar: (32.0, 160.0),
    bar_alone: 0.0,
    run: 82.0,
    details: Some((FOLD_DETAILS_MIN, 480.0)),
    under: None,
    pct_strip: PCT_STRIP,
};

/// Folded mid: four hours' strip at a 4 pt pitch, a bar from 28 pt, no Details. The strip keeps
/// its share: a narrower block keeps a bar column by a shorter bar's least. Its least was 79 pt
/// until A1's step R5, and is four hours' now as every Folded bar column's is; at this share it is
/// implied, not load-bearing. [`column_holds`] lays mid's column from 170 pt of room, where the
/// share already lays 34 boxes, more than four hours' 33, so 79 and 129 plan alike at every width;
/// `column_holds` is what took main's narrow-to-mid step downs (639 and 647 pt on Helvetica
/// Neue), where mid's column fitted beside six minutes but did not hold at every wider room.
const FOLD_MID: MeterSpec = MeterSpec {
    strip: (FOUR_HOURS, 480.0),
    pitch: 4.0,
    bar: (28.0, 160.0),
    details: None,
    ..FOLD_WIDE
};

/// Folded narrow: four hours' strip at a 3 pt pitch and a bar from 24 pt. Its least was 59 pt
/// until A1's step R5: a narrow block first fits its bar column where the strip with the bar
/// under each % holds four hours (from 563 pt on Helvetica Neue), so a shorter strip beside the
/// column shortened the horizon as the deck widened.
const FOLD_NARROW: MeterSpec = MeterSpec {
    pitch: 3.0,
    bar: (24.0, 160.0),
    ..FOLD_MID
};

/// Folded slim: narrow's, and where no bar column fits beside four hours, the bar under each %
/// beside a strip from 59 pt (a minute; six minutes beside a % column, [`PCT_STRIP`]).
const FOLD_SLIM: MeterSpec = MeterSpec {
    under: Some(59.0),
    ..FOLD_NARROW
};

/// What a table lays right of its strip (the owner, 2026-10-26: "extend the progress brs so
/// that the fill more horizontal space. the stats in columns after the time series doesn't make
/// sense to me"): the scope bar, stretched over what the strip leaves, and at its end, where a
/// bar at least as long stands beside it, one figure run per row, this Mac's reading with the
/// row's maximum. No column of figures stands after the strip: aterm's and this tab's amounts
/// are the bar's segments.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Meter {
    /// The scope bar's column; `None` where each row's bar stands under its % instead.
    pub(crate) bar: Option<Col>,
    /// The figure run's column, [`RUN_GAP`] past the bar: each row's run starts at its left edge,
    /// in the first form that fits its width. `None` where the run gives way to the bar.
    pub(crate) run: Option<Col>,
}

impl Meter {
    /// Its columns, left to right.
    pub(crate) fn cols(&self) -> Vec<Col> {
        self.bar.into_iter().chain(self.run).collect()
    }

    /// Where its last column ends, pt; `None` with no bar column.
    pub(crate) fn end(&self) -> Option<f64> {
        self.run.or(self.bar).map(Col::end)
    }

    fn shift(&mut self, dx: f64) {
        for col in [&mut self.bar, &mut self.run].into_iter().flatten() {
            col.x += dx;
        }
    }
}

/// How many boxes of `spec`'s pitch a strip at most `w` wide (and at most `spec`'s most) lays.
fn strip_cols(spec: &MeterSpec, w: f64) -> u32 {
    ((w.min(spec.strip.1) + 1.0) / spec.pitch).floor().max(0.0) as u32
}

/// The fewest boxes of `spec`'s pitch whose strip is at least `w` wide: a strip of `n` boxes is
/// `n` pitches less the last box's gap, so a preset's least (129 pt at a 4 pt pitch) takes the box
/// that reaches it.
fn least_cols(spec: &MeterSpec, w: f64) -> u32 {
    ((w + 1.0) / spec.pitch).ceil().max(0.0) as u32
}

/// How a table shares the room from `x`, where its strip starts, to `end` (`docs/DESIGN-stats-deck-
/// meter-2026-10-26.md` §2): the strip's boxes, the meter, and the Details column past it; `None`
/// where `spec`'s strip, or its bar where it has no place under the %, does not fit.
///
/// [`share_meter`] shares the room between the strip, the bar and the run. Details, where `spec`
/// lays it, takes [`DETAILS_SHARE`] of the room (its least to [`DETAILS_MAX`]) where the rest
/// still lays the run it would without them and `spec`'s strip beside Details: Details never cost
/// a row its maximum, nor the time series its length. What the strip's and the bar's most leave
/// goes to the run's column (up to [`RUN_MAX`]) or to Details (up to [`DETAILS_MAX`]). Where no
/// bar column stands beside the strip at this room and every wider one ([`column_holds`]), a
/// table that may stands each row's bar under its % and gives the strip the room.
fn lay_meter(x: f64, end: f64, spec: &MeterSpec) -> Option<(u32, Meter, Option<Col>)> {
    let room = end - x;
    // The bar stands under each %, and the strip takes the room.
    let under = || {
        let cols = strip_cols(spec, room);
        (cols >= least_cols(spec, spec.under?)).then(|| (cols, Meter::default(), None))
    };
    if !column_holds(room, spec) {
        return under();
    }
    let without = share_meter(room, spec);
    let details_w = spec.details.and_then(|(least, strip)| {
        let d = (room * DETAILS_SHARE).floor().clamp(least, DETAILS_MAX);
        let with = share_meter(room - d - DETAILS_GAP, spec)?;
        let run = |laid: Option<(u32, f64, f64)>| laid.map(|(_, run, _)| run);
        let keeps = f64::from(with.0) * spec.pitch - 1.0 >= strip;
        (keeps && run(Some(with)) >= run(without)).then_some(d)
    });
    let m = details_w.map_or(room, |d| room - d - DETAILS_GAP);
    // The column holds at `room`, and Details stand only where it holds beside them.
    let Some((cols, run_w, bar_w)) = share_meter(m, spec) else {
        return under();
    };
    let bar = Col::new(x + f64::from(cols) * spec.pitch + spec.bar_gap, bar_w);
    let used = bar.end() - x + if run_w > 0.0 { RUN_GAP + run_w } else { 0.0 };
    let spare = (m - used).max(0.0);
    let run = (run_w > 0.0).then(|| {
        let w = if details_w.is_some() {
            run_w
        } else {
            (run_w + spare).min(RUN_MAX)
        };
        Col::new(bar.end() + RUN_GAP, w)
    });
    // Details stay narrower than the strip, the row's widest part.
    let strip_w = f64::from(cols) * spec.pitch - 1.0;
    let details = details_w.map(|d| {
        let at = run.map_or(bar.end(), Col::end) + DETAILS_GAP;
        Col::new(
            at,
            (d + spare)
                .min(DETAILS_MAX)
                .min(end - at)
                .min(strip_w - 1.0),
        )
    });
    Some((
        cols,
        Meter {
            bar: Some(bar),
            run,
        },
        details,
    ))
}

/// The strip's boxes, the run's width (0 for none) and the bar's for `m` pt from the strip's
/// start to the meter's end ([`lay_meter`]); `None` where no bar of `spec`'s least (with no run
/// beside it, `spec.bar_alone`) fits beside the strip.
///
/// The strip takes `spec.share` of the room, whole boxes between its least and its most: the
/// time series is the row's widest part, and it never cedes its share. The bar takes what it
/// leaves: the run stands at the bar's end only where the bar is then at least as long as the
/// run, so as the room narrows the run gives way first, then the bar (to a narrower spec's, or
/// under each %), and the strip last. A bar past its most gives the excess to the strip, up to
/// the strip's most.
fn share_meter(m: f64, spec: &MeterSpec) -> Option<(u32, f64, f64)> {
    let mut cols = strip_cols(spec, (m * spec.share).floor()).max(least_cols(spec, spec.strip.0));
    // From the strip's last box's pitch, and the bar's gap, to the meter's end.
    let rest = (m - f64::from(cols) * spec.pitch - spec.bar_gap).floor();
    if rest < spec.bar.0 {
        return None;
    }
    let with_run = (rest - RUN_GAP - spec.run).floor();
    let (run_w, mut bar_w) = if with_run >= spec.run.max(spec.bar.0) {
        (spec.run, with_run)
    } else if rest >= spec.bar_alone {
        (0.0, rest)
    } else {
        return None;
    };
    // What the bar's most leaves goes to the strip, up to its own most.
    if bar_w > spec.bar.1 {
        let more = strip_cols(
            spec,
            f64::from(cols) * spec.pitch - 1.0 + bar_w - spec.bar.1,
        )
        .max(cols);
        bar_w -= f64::from(more - cols) * spec.pitch;
        cols = more;
        bar_w = bar_w.min(spec.bar.1);
    }
    Some((cols, run_w, bar_w))
}

/// Whether a bar column stands beside `spec`'s strip at room `m` and at every wider room (A1
/// step R5): [`share_meter`] lays it at `m` and at each whole point more, up to the room from which
/// it always does ([`column_sure`]).
///
/// The strip takes whole boxes of its share, so the bar's rest falls by a pitch less a point at
/// each box the strip gains, and near its least a column that fits at one room may not fit a
/// point wider. A kind laid wherever its column fitted flipped back and forth with the room
/// modulo its pitch (Folded's kinds from 583 to 702 pt on Helvetica Neue before R5), moving the
/// name, the % and the strip at each flip. A column that holds only from the room it holds at
/// every wider one comes once and stays, so each kind fits from one width on, and a wider deck
/// never lays a narrower kind.
fn column_holds(m: f64, spec: &MeterSpec) -> bool {
    let sure = column_sure(spec);
    let mut room = m;
    loop {
        if share_meter(room, spec).is_none() {
            return false;
        }
        if room >= sure {
            return true;
        }
        room += 1.0;
    }
}

/// A room from which [`share_meter`] lays `spec`'s bar column at every wider room, pt: where the
/// bar's rest is at least `R`, the whole point at or above its least and its least with no run
/// beside it, whichever boxes the strip takes. The strip takes its least's boxes (at most its
/// least and a pitch) or its share's (at most the share and a point), and the rest is floored,
/// so `m − least − pitch − 1 − gap ≥ R` and `m (1 − share) − 2 − gap ≥ R` together hold it.
fn column_sure(spec: &MeterSpec) -> f64 {
    let r = spec.bar.0.max(spec.bar_alone).ceil();
    let by_least = spec.strip.0 + spec.pitch + 1.0 + spec.bar_gap + r;
    let by_share = (r + 2.0 + spec.bar_gap) / (1.0 - spec.share);
    by_least.max(by_share).ceil()
}

/// The ground between two boxes of one band, pt (`docs/DESIGN-stats-deck-units-2026-11-04.md`
/// §3).
pub(crate) const BOX_GAP: f64 = 1.0;
/// The ground between two bands, pt: a gate, where the header's tick for the coarser band stands
/// (A1 §3). No gridline is drawn: the gate's ground tells the bands apart.
pub(crate) const GATE: f64 = 3.0;

/// The gap after a box of band `a` before one of band `b`, pt.
fn gap_between(a: u8, b: u8) -> f64 {
    if a == b { BOX_GAP } else { GATE }
}

/// Progressive multi-scale heat strip geometry: the live `now` box at the left end, then fine
/// real-time boxes growing into wider aggregated time buckets toward the right.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Strip {
    pub(crate) x: f64,
    pub(crate) cols: u32,
    pub(crate) period_s: u32,
    pub(crate) pitch: f64,
    pub(crate) cell: f64,
    pub(crate) spec: MultiScaleSpec,
}

impl Strip {
    /// A Full strip's `period_s`: what a box of the legacy uniform 10-minute strip held, which
    /// tells a Full strip from a Folded one ([`FOLD_SLOT_S`]).
    const FULL_PERIOD_S: u32 = 10;

    /// A Full strip of `cols` boxes at `pitch` from `x`, its horizon the widest its width holds
    /// ([`Self::spec_for`]).
    fn full(x: f64, cols: u32, pitch: f64) -> Self {
        Self::of(x, cols, Self::FULL_PERIOD_S, pitch)
    }

    /// A Folded strip of `cols` boxes at `pitch` from `x`, its horizon the widest its width
    /// holds ([`Self::spec_for`]).
    pub(crate) fn folded(x: f64, cols: u32, pitch: f64) -> Self {
        Self::of(x, cols, FOLD_SLOT_S, pitch)
    }

    /// The widest of the owner's block presets (`docs/DESIGN-stats-deck-owner-2026-10-26.md` §4)
    /// a strip `w` pt wide (its boxes' pitches, less the last one's gap) holds: six hours from
    /// 239 pt, four from 129, six minutes from 79, else a minute. Each is where main's tiers
    /// first lay it (Full mid's 60 columns at 4 pt, Folded wide's 26 at 5, Folded mid's 20 at
    /// 4), so its scale keeps its ticks. With A1's rising weights and 3 pt gates
    /// (`docs/DESIGN-stats-deck-units-2026-11-04.md` §3) the narrowest box is the four-hour
    /// preset's 1 s box: 1 device px at 1× below 208 pt, a 2 px pitch, and 2 px at 2×.
    pub(crate) fn spec_for(w: f64) -> MultiScaleSpec {
        if w >= 239.0 {
            MultiScaleSpec::FULL_6H
        } else if w >= 129.0 {
            MultiScaleSpec::FOLD_4H
        } else if w >= 79.0 {
            MultiScaleSpec::COMPACT_6M
        } else {
            MultiScaleSpec::MINI_1M
        }
    }

    fn of(x: f64, cols: u32, period_s: u32, pitch: f64) -> Self {
        let w = f64::from(cols) * pitch - 1.0;
        Self {
            x,
            cols,
            period_s,
            pitch,
            cell: pitch - 1.0,
            spec: Self::spec_for(w),
        }
    }

    /// How far back the strip reaches, seconds: its multi-scale horizon (`21600`, `14400`, `360`
    /// or `60`).
    pub(crate) fn span_s(self) -> u32 {
        u32::try_from(self.spec.horizon_ms() / 1_000).unwrap_or(60)
    }

    /// Total horizontal width of the strip in points (`end() - x`).
    pub(crate) fn width(self) -> f64 {
        self.end() - self.x
    }

    /// Every box's left and right edge, pt, newest first, before rounding to device px: the one
    /// geometry its box rects ([`crate::stats_deck::multi_scale_slot_rects`]), its header ticks
    /// ([`Self::x_at_age_ms`]) and its events all come from
    /// (`docs/DESIGN-stats-deck-units-2026-11-04.md` §3). Boxes of one band stand [`BOX_GAP`]
    /// apart and two bands a [`GATE`] apart; the room the gaps leave is shared by the boxes'
    /// weights, each edge at its cumulative weight, so rounding each edge to the device px
    /// varies a box's width by at most a pixel within its band. The cumulative weights are whole
    /// numbers divided in `f64`, not `f32` fractions of the spec's weight, so edges a layout puts
    /// on the same half pixel round the same way
    /// (`from_one_second_up_no_box_is_narrower_than_a_box_of_a_finer_band`).
    pub(crate) fn slot_edges(self) -> impl Iterator<Item = (aterm_vitals::ProgSlot, (f64, f64))> {
        let slots = self.spec.iter_slots();
        let gaps: f64 = slots
            .clone()
            .scan(None, |previous, slot| {
                let gap = previous.map_or(0.0, |band| gap_between(band, slot.band));
                *previous = Some(slot.band);
                Some(gap)
            })
            .skip(1)
            .sum();
        let room = (self.width() - gaps).max(0.0);
        let total = f64::from(self.spec.total_weight());
        let (mut before, mut weight, mut previous) = (0.0, 0_u32, None);
        slots.map(move |slot| {
            if let Some(band) = previous {
                before += gap_between(band, slot.band);
            }
            let at = self.x + before;
            let left = at + room * f64::from(weight) / total;
            weight += u32::from(slot.weight);
            let right = at + room * f64::from(weight) / total;
            previous = Some(slot.band);
            (slot, (left, right))
        })
    }

    /// Materialize the shared geometry for coordinate and previous-layout test oracles.
    #[cfg(test)]
    pub(crate) fn box_edges(self) -> Vec<(f64, f64)> {
        let mut edges = Vec::with_capacity(self.spec.slot_count());
        edges.extend(self.slot_edges().map(|(_, edges)| edges));
        edges
    }

    /// Where on the strip an age `age_ms` (`now - event_ms`) stands, pt: inside the box that
    /// holds it, in proportion, each box reaching to the middle of the gap or gate on either side,
    /// so a band's first age (a header tick's) stands in the middle of its gate; `None` past the
    /// horizon or more than 2 s ahead of now.
    pub(crate) fn x_at_age_ms(self, age_ms: i64) -> Option<f64> {
        if age_ms < -2_000 || age_ms > self.spec.horizon_ms() {
            return None;
        }
        let age = age_ms.max(0);
        let mut edges = self.slot_edges().peekable();
        let mut previous = None;
        while let Some((slot, (left, right))) = edges.next() {
            if age > slot.age_hi_ms {
                previous = Some(right);
                continue;
            }
            let lo = previous.map_or(left, |prev| (prev + left) / 2.0);
            let hi = edges
                .peek()
                .map_or(right, |(_, (next, _))| (right + next) / 2.0);
            let span = (slot.age_hi_ms - slot.age_lo_ms).max(1) as f64;
            let into = ((age - slot.age_lo_ms) as f64 / span).clamp(0.0, 1.0);
            return Some(lo + (hi - lo) * into);
        }
        None
    }

    /// How fast its fine band moves right, pt a second: its first history box's pitch (its width
    /// and the gap after it) over that box's period
    /// (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §3.4).
    #[cfg(test)]
    pub(crate) fn pt_per_s(self) -> f64 {
        let slots = self.spec.slots();
        let edges = self.box_edges();
        slots
            .iter()
            .position(|slot| !slot.is_live_now)
            .filter(|&j| j + 1 < edges.len())
            .map_or(0.0, |j| {
                let slot = slots[j];
                let period_s = (slot.age_hi_ms - slot.age_lo_ms) as f64 / 1000.0;
                (edges[j + 1].0 - edges[j].0) / period_s.max(1e-3)
            })
    }

    /// Right edge of the last strip cell, the horizon (the oldest box's far edge).
    pub(crate) fn end(self) -> f64 {
        self.x + f64::from(self.cols.saturating_sub(1)) * self.pitch + self.cell
    }
}

/// Vertical row geometry and font sizes for a deck form.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rows {
    /// Baseline of the header line.
    pub(crate) header_baseline: f64,
    /// Top edge of row 0.
    pub(crate) top0: f64,
    /// Vertical pitch between rows.
    pub(crate) pitch: f64,
    /// Height of a strip cell.
    pub(crate) cell: f64,
    /// Total number of rows.
    pub(crate) count: u32,
    /// Text baseline offset from a row's top.
    pub(crate) baseline: f64,
    /// Font size for row names and percentages.
    pub(crate) name_pt: f64,
    /// Font size for resource amounts.
    pub(crate) amount_pt: f64,
    /// Font size for `aterm` and `Tab` figures.
    pub(crate) figure_pt: f64,
    /// Scope bar height.
    pub(crate) bar_h: f64,
    /// The rows they hold.
    pub(crate) deck: DeckRows,
}

impl Rows {
    const fn full(kind: FullKind, deck: DeckRows) -> Self {
        let (count, pitch) = deck.full_rows(kind);
        let (cell, baseline, bar_h) = match kind {
            FullKind::Wide | FullKind::Mid => (9.0, 8.0, 6.0),
            FullKind::Narrow | FullKind::Slim => (8.0, 7.0, 5.0),
        };
        let (name_pt, amount_pt, figure_pt) = match kind {
            FullKind::Wide | FullKind::Mid => (10.5, 10.0, 10.0),
            FullKind::Narrow => (10.0, 10.0, 10.0),
            FullKind::Slim => (9.5, 9.5, 9.5),
        };
        Self {
            header_baseline: 11.5,
            top0: 16.0,
            pitch,
            cell,
            count,
            baseline,
            name_pt,
            amount_pt,
            figure_pt,
            bar_h,
            deck,
        }
    }

    const fn folded(kind: BlockKind, deck: DeckRows) -> Self {
        Self {
            header_baseline: 9.5,
            top0: FOLD_TOP0,
            pitch: FOLD_PITCH,
            cell: FOLD_CELL,
            count: deck.folded_count(),
            baseline: 7.5,
            name_pt: match kind {
                BlockKind::Wide | BlockKind::Mid => 10.0,
                BlockKind::Narrow | BlockKind::Slim => 9.5,
            },
            amount_pt: 9.5,
            figure_pt: 9.5,
            bar_h: 5.0,
            deck,
        }
    }

    /// Returns the top y-coordinate of row `r`.
    pub(crate) fn top(&self, r: u32) -> f64 {
        self.top0 + f64::from(r) * self.pitch
    }

    /// The top and bottom of the three-scope bar row `r` draws under its % figure where no bar
    /// column fits, pt, as planned: [`UNDER_GAP`] below the digits' baseline and [`UNDER_H`]
    /// tall (the painter draws it from that top, shorter at 1× in Full, as [`UNDER_H`] says).
    pub(crate) fn under(&self, r: u32) -> (f64, f64) {
        let y = self.top(r) + self.baseline + UNDER_GAP;
        (y, y + UNDER_H)
    }
}

/// Width tier for the `Full` deck form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum FullKind {
    /// The gauge's slot, a strip of six hours at a 5 pt pitch, the meter, and Details.
    Wide,
    /// The gauge's slot, a strip of six hours at a 4 pt pitch, and the meter.
    Mid,
    /// A strip of six hours at a 5 pt pitch, and the meter.
    Narrow,
    /// A strip of six hours at a 4 pt pitch and the meter, or a strip from 80 pt and the bar
    /// under each %.
    Slim,
}

impl FullKind {
    /// All full kinds from widest to narrowest.
    pub(crate) const ALL: [Self; 4] = [Self::Wide, Self::Mid, Self::Narrow, Self::Slim];

    /// Candidate [`CoresMode`]s for its Cores row in priority order: wide and mid's cell-high
    /// squares fit beside the gauge's slot as Folded's wide and mid blocks' do.
    pub(crate) const fn cores_ladder(self) -> &'static [CoresMode] {
        match self {
            Self::Wide | Self::Mid => &[CoresMode::Big, CoresMode::Stack, CoresMode::Tiny],
            Self::Narrow | Self::Slim => &[CoresMode::Stack, CoresMode::Tiny],
        }
    }
}

/// The slot left of a Full wide or mid table's names, past a divider: it holds the strain gauge
/// (the refined deck, 2026-10-24), and a click in it opens the CPU lens, which keeps the
/// per-core view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GaugeSlot {
    pub(crate) at: Col,
    /// X-coordinate of the divider between the slot and the names.
    pub(crate) divider: f64,
}

/// The Machine row's columns (A4 §8, §9): the computer's name in the room left of its now
/// glyphs, and the glyphs, which end where the row's % would (each other row's value now), as
/// the units end against their %; the row's lane stands on the strip and the battery's charge in
/// the bar column. The glyphs keep the place of all four (lid, power, place, docked) whether or
/// not they draw, as a wall keeps its space (A1 §4.0): a desktop draws no lid or docked glyph,
/// a display comes and goes, and none of it moves the name.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MachineRow {
    /// The name's slot, from the name column's left edge; 0 wide where nothing fits.
    pub(crate) name: Col,
    /// The four glyphs' place.
    pub(crate) glyphs: Col,
}

/// The Machine row's glyphs (A4 §8): lid, power, place and docked.
const MACHINE_GLYPHS: f64 = 4.0;
/// Between two of the Machine row's glyphs, pt.
const GLYPH_GAP: f64 = 2.0;
/// From the Machine row's name to its first glyph, pt.
const NAME_TO_GLYPHS: f64 = 3.0;
/// The Machine row's name, pt; set at weight 600, measured bold (A4 §8).
pub(crate) const MACHINE_NAME_PT: f64 = 10.0;

/// The Machine row of a table whose name column is `name`, whose well is `well` and whose %
/// column, where it stands, is `pct`, in a cell `cell` pt tall: each glyph a square of the
/// units' height at [`PLAN_SCALE`] (9 pt in Folded's 8.5 pt cell), [`GLYPH_GAP`] apart, ending
/// where the % ends, or the well where no % stands; the name takes the room from its column's
/// left edge to [`NAME_TO_GLYPHS`] short of them.
fn machine_row(name: Col, well: Col, pct: Option<Col>, cell: f64) -> MachineRow {
    let glyph = (cell * PLAN_SCALE).ceil() / PLAN_SCALE;
    let w = MACHINE_GLYPHS * glyph + (MACHINE_GLYPHS - 1.0) * GLYPH_GAP;
    let end = pct.map_or(well.end(), Col::end);
    let glyphs = Col::new(end - w, w);
    MachineRow {
        name: Col::new(name.x, (glyphs.x - NAME_TO_GLYPHS - name.x).max(0.0)),
        glyphs,
    }
}

/// The Machine row's name in `room` pt (A4 §9, "Fit"): the first of `forms` (the computer's type,
/// then its family form; a Mac's type is already its family) that fits whole, else the family
/// form cut at its last word boundary that fits, with an ellipsis, never mid-word; with no form
/// read, or nothing of them fitting, `noun`, the OS's noun for the machine, where it fits; else
/// nothing. Measured at [`MACHINE_NAME_PT`] in bold, which stands no narrower than weight 600.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "A4 step N5 paints the Machine row")
)]
pub(crate) fn machine_name(forms: &[&str], noun: &str, room: f64, m: &FaceMetrics) -> String {
    let fits = |s: &str| m.width(s, MACHINE_NAME_PT, true) <= room + 0.01;
    let mut named: Vec<&str> = Vec::new();
    for form in forms.iter().map(|f| f.trim()).filter(|f| !f.is_empty()) {
        if !named.contains(&form) {
            named.push(form);
        }
    }
    if let Some(form) = named.iter().find(|form| fits(form)) {
        return (*form).to_owned();
    }
    let cut = named.last().and_then(|family| {
        family
            .match_indices(' ')
            .rev()
            .map(|(at, _)| format!("{}\u{2026}", family[..at].trim_end()))
            .find(|head| fits(head))
    });
    cut.or_else(|| fits(noun).then(|| noun.to_owned()))
        .unwrap_or_default()
}

/// Column layout plan for the `Full` deck form.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FullPlan {
    /// Deck width in whole points.
    pub(crate) w: f64,
    pub(crate) kind: FullKind,
    /// Whether the deck is too narrow to draw anything besides its background.
    pub(crate) ground_only: bool,
    /// Horizontal centering offset applied to all columns, where the widest strip, bar and
    /// Details leave room.
    pub(crate) offset: f64,
    pub(crate) rows: Rows,
    /// The gauge's slot left of the names, in a wide or mid table.
    pub(crate) gauge: Option<GaugeSlot>,
    /// The Cores row's room, from the units' left edge to the centred table's right edge (the
    /// right margin less [`Self::offset`]), where the table holds the row ([`DeckRows`]): the
    /// record's narrow and slim tables, the instruments' every table (A4 §7).
    pub(crate) cores: Option<Col>,
    pub(crate) name: Col,
    pub(crate) well: Col,
    pub(crate) strip: Strip,
    /// Right-aligned percentage column, between the well and the strip.
    pub(crate) pct: Option<Col>,
    /// The scope bar and the figure run right of the strip; where the bar's column does not fit,
    /// each row draws its bar under its % ([`Self::bar_under`]).
    pub(crate) meter: Meter,
    pub(crate) details: Option<Col>,
    /// The units column between the well and the %, in a wide or mid table where any row draws
    /// units (A1 §2.5, A4 §5).
    pub(crate) units: Option<UnitsCol>,
    /// The Machine row, where the table holds it (A4 §8).
    pub(crate) machine: Option<MachineRow>,
}

impl FullPlan {
    /// The % column where no bar column fits: each row that prints a % draws its three-scope bar
    /// under the figure there (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §5).
    pub(crate) fn bar_under(&self) -> Option<Col> {
        self.pct.filter(|_| self.meter.bar.is_none())
    }

    /// Chooses the Cores row's [`CoresMode`] for `clusters`, where the table holds the row.
    pub(crate) fn cores_mode(
        &self,
        clusters: &[Cluster<'_>],
        m: &FaceMetrics,
    ) -> Option<CoresMode> {
        let room = self.cores?;
        Some(cores_row_mode(
            clusters,
            room.w,
            self.rows.cell,
            self.kind.cores_ladder(),
            CORES_CAPTION_PT,
            self.rows.deck.cores_queue(),
            m,
        ))
    }

    fn shift(&mut self, dx: f64) {
        if let Some(slot) = &mut self.gauge {
            slot.at.x += dx;
            slot.divider += dx;
        }
        self.name.x += dx;
        self.well.x += dx;
        self.strip.x += dx;
        self.meter.shift(dx);
        for col in [&mut self.pct, &mut self.details, &mut self.cores]
            .into_iter()
            .flatten()
        {
            col.x += dx;
        }
        if let Some(units) = &mut self.units {
            units.at.x += dx;
        }
    }

    /// Lays its Machine row, where its rows hold one, from its columns as laid.
    fn lay_machine(&mut self) {
        self.machine = self
            .rows
            .deck
            .machine_row()
            .then(|| machine_row(self.name, self.well, self.pct, self.rows.cell));
    }
}

/// Computes the [`FullPlan`] the deck paints for deck width `w_pt`: its rows
/// ([`DeckRows::PAINTED`]) with no units column, until A4's step N5 paints the instruments'
/// rows and their units with the machine's [`UnitFacts`] ([`plan_full_with`]).
pub(crate) fn plan_full(w_pt: f64, m: &FaceMetrics) -> FullPlan {
    full_by(w_pt, m, &UnitFacts::NONE, full_spec, DeckRows::PAINTED)
}

/// Computes the instruments' [`FullPlan`] for deck width `w_pt` (A4 §11): ten rows at every
/// kind, the widest kind that fits, and in a wide or mid table the units column `units`
/// measures where it costs the strip nothing of its horizon (A1 §2.5, step R5). Its columns are
/// [`plan_full`]'s with no facts: the instruments' rows move none.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "A4 step N5 paints the instruments' rows with the host's facts"
    )
)]
pub(crate) fn plan_full_with(w_pt: f64, m: &FaceMetrics, units: &UnitFacts) -> FullPlan {
    full_by(w_pt, m, units, full_spec, DeckRows::Instruments)
}

/// [`plan_full_with`] of `deck`'s rows with each kind's meter measures from `specs`.
fn full_by(
    w_pt: f64,
    m: &FaceMetrics,
    units: &UnitFacts,
    specs: FullSpecs,
    deck: DeckRows,
) -> FullPlan {
    let w = whole_width(w_pt);
    let laid = FullKind::ALL.into_iter().find_map(|kind| {
        let (plan, fits) = lay_full(w, kind, m, true, 0.0, specs, deck);
        fits.then_some(plan)
    });
    let mut plan = if let Some(plan) = laid {
        full_units(plan, m, units, specs).unwrap_or(plan)
    } else {
        let right = w - X0;
        // Under slim's width the % column goes before the strip does: the strip takes its place.
        let (mut plan, _) = lay_full(w, FullKind::Slim, m, true, 0.0, specs, deck);
        if plan.strip.end() > right {
            plan = lay_full(w, FullKind::Slim, m, false, 0.0, specs, deck).0;
        }
        plan.pct = plan.pct.filter(|pct| pct.end() <= right);
        plan.ground_only = w < MIN_TABLE_W || plan.strip.end() > right;
        plan
    };
    plan.lay_machine();
    plan
}

/// `bare` laid again around its units column (A1 §2.5, step R5), where it is wide or mid and
/// `units` draws a form within U_cap; `None` where it draws none. Its kind and horizon are the
/// ones it has with no units column, and its Details give way to the units as Folded block A's
/// do (A1 §2.4's as-built note). Narrow and slim draw none: mid's first width leaves no room for
/// a column ([`units_cap`] is `None` there), so a form a narrow table drew would go again as the
/// deck widened. Its one column holds every row's form, block B's rows' with block A's (A4 §5).
fn full_units(
    bare: FullPlan,
    m: &FaceMetrics,
    units: &UnitFacts,
    specs: FullSpecs,
) -> Option<FullPlan> {
    if !matches!(bare.kind, FullKind::Wide | FullKind::Mid) {
        return None;
    }
    let spec = full_meter_spec(bare.kind, m, specs);
    // Where the % stood, before the table was centred.
    let at = bare.pct?.x - bare.offset;
    let room = bare.w - X0 - (bare.strip.x - bare.offset) - UNITS_TO_PCT;
    let col = plan_units(
        at,
        room,
        bare.strip.spec,
        bare.rows.cell,
        &spec,
        units,
        &UnitRow::ALL,
        f64::INFINITY,
    )?;
    let (mut plan, _) = lay_full(bare.w, bare.kind, m, true, col.at.w, specs, bare.rows.deck);
    plan.units = Some(UnitsCol {
        at: Col::new(col.at.x + plan.offset, col.at.w),
        ..col
    });
    Some(plan)
}

/// A Full kind's meter measures as the planner lays them.
const fn full_spec(kind: FullKind) -> MeterSpec {
    match kind {
        FullKind::Wide => FULL_WIDE,
        FullKind::Mid => FULL_MID,
        FullKind::Narrow => FULL_NARROW,
        FullKind::Slim => FULL_SLIM,
    }
}

/// Each Full kind's meter measures: [`full_spec`], or another table's in a law's control.
type FullSpecs = fn(FullKind) -> MeterSpec;

/// `kind`'s meter measures from `specs` on face `m`: the legend stands over the meter's own
/// columns, so a bar with no run is as long as it.
fn full_meter_spec(kind: FullKind, m: &FaceMetrics, specs: FullSpecs) -> MeterSpec {
    MeterSpec {
        bar_alone: legend_least(m),
        ..specs(kind)
    }
}

/// The Full plan of `kind` holding `deck`'s rows at `w`, and whether it fits; `pct` lays the %
/// column between the well and the strip, and without it the strip stands where the column
/// would. A units column `u` pt wide (0 for none) stands where the % column would, and the %
/// column [`UNITS_TO_PCT`] past it; the caller places the column's forms ([`full_units`]) and
/// lays the Machine row ([`FullPlan::lay_machine`]).
fn lay_full(
    w: f64,
    kind: FullKind,
    m: &FaceMetrics,
    pct: bool,
    u: f64,
    specs: FullSpecs,
    deck: DeckRows,
) -> (FullPlan, bool) {
    let right = w - X0;
    let rows = Rows::full(kind, deck);
    let (name_x, name_base) = match kind {
        FullKind::Wide | FullKind::Mid => (97.0, 56.0),
        FullKind::Narrow => (X0, 52.0),
        FullKind::Slim => (X0, 48.0),
    };
    let name = Col::new(
        name_x,
        name_width(name_base, rows.name_pt, m, deck.row_names()),
    );
    let well = Col::new(name.end(), 10.0);
    // The units (the activity now, by part), the % column (the value now) and then the strip,
    // which reads outward from now.
    let (units_x, pct_w) = match kind {
        FullKind::Wide | FullKind::Mid | FullKind::Narrow => (well.end() + 3.0, 30.0),
        FullKind::Slim => (well.end(), 26.0),
    };
    let pct_col = if u > 0.0 {
        Col::new(units_x + u + UNITS_TO_PCT, pct_w)
    } else {
        Col::new(units_x, pct_w)
    };
    let strip_x = if pct {
        pct_col.end() + PCT_TO_STRIP
    } else {
        well.end() + BARE_STRIP_GAP
    };
    let spec = full_meter_spec(kind, m, specs);
    let mut plan = FullPlan {
        w,
        kind,
        ground_only: false,
        offset: 0.0,
        rows,
        gauge: matches!(kind, FullKind::Wide | FullKind::Mid).then_some(GaugeSlot {
            at: Col::new(X0, CORES_BLOCK_W),
            divider: 88.0,
        }),
        // The Cores row spans from where the units stand, with or without a units column, to
        // the right margin; a centred table narrows it to its own right edge below.
        cores: deck
            .full_cores_row(kind)
            .then(|| Col::new(units_x, (right - units_x).max(0.0))),
        name,
        well,
        strip: Strip::full(strip_x, least_cols(&spec, least_strip(&spec)), spec.pitch),
        pct: pct.then_some(pct_col),
        meter: Meter::default(),
        details: None,
        units: None,
        machine: None,
    };
    // Wide needs its Details; slim, the last kind, takes whatever fits. Laid again around its
    // units, a table keeps the kind it has without them, and its Details give way to the column.
    let laid = lay_meter(strip_x, right, &spec)
        .filter(|(_, _, details)| kind != FullKind::Wide || u > 0.0 || details.is_some());
    let Some((cols, meter, details)) = laid else {
        return (plan, false);
    };
    plan.strip = Strip::full(strip_x, cols, spec.pitch);
    plan.meter = meter;
    plan.details = details;
    // What the widest strip, bar and Details leave centres the table, and a Cores row spans the
    // centred table: the right margin less the offset at each side.
    let laid_end = details
        .or(meter.run)
        .or(meter.bar)
        .map_or(plan.strip.end(), Col::end);
    let extra = right - laid_end;
    if kind == FullKind::Wide && extra > 0.0 {
        plan.offset = (extra / 2.0).floor();
        plan.shift(plan.offset);
        if let Some(room) = &mut plan.cores {
            room.w = (room.w - 2.0 * plan.offset).max(0.0);
        }
    }
    let fits = plan.strip.end() <= right && (!pct || plan.strip.width() >= spec.pct_strip);
    (plan, fits)
}

/// The narrowest strip `spec`'s table lays: its least beside the bar under each %, where the bar
/// may stand there, else beside a bar column.
fn least_strip(spec: &MeterSpec) -> f64 {
    spec.under.unwrap_or(spec.strip.0)
}

/// Width tier for each block of the `Folded` deck form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum BlockKind {
    /// A strip of four hours or more at a 5 pt pitch, the meter, and Details where room is left.
    Wide,
    /// A strip of four hours or more at a 4 pt pitch, and the meter.
    Mid,
    /// A strip of four hours or more at a 3 pt pitch, and the meter.
    Narrow,
    /// Strip and percentage, and the meter or the scope bar under each %, with state in the
    /// header.
    Slim,
}

impl BlockKind {
    /// All block kinds from widest to narrowest.
    pub(crate) const ALL: [Self; 4] = [Self::Wide, Self::Mid, Self::Narrow, Self::Slim];

    /// Candidate [`CoresMode`]s for the Cores row in priority order.
    pub(crate) const fn cores_ladder(self) -> &'static [CoresMode] {
        match self {
            Self::Wide | Self::Mid => &[CoresMode::Big, CoresMode::Stack, CoresMode::Tiny],
            Self::Narrow => &[CoresMode::Stack, CoresMode::Tiny],
            Self::Slim => &[CoresMode::Tiny],
        }
    }

    /// Font size for Cores row captions.
    pub(crate) const fn cores_caption_pt(self) -> f64 {
        match self {
            Self::Slim => 9.0,
            Self::Wide | Self::Mid | Self::Narrow => CORES_CAPTION_PT,
        }
    }
}

/// Column layout plan for one block of the `Folded` deck form.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BlockPlan {
    pub(crate) kind: BlockKind,
    /// Horizontal span of the block.
    pub(crate) at: Col,
    pub(crate) rows: Rows,
    pub(crate) name: Col,
    pub(crate) well: Col,
    pub(crate) strip: Strip,
    /// Right-aligned percentage column between the well and the strip, if fitted.
    pub(crate) pct: Option<Col>,
    /// The scope bar and the figure run right of the strip; where the bar's column does not fit,
    /// each row draws its bar under its % ([`Self::bar_under`]).
    pub(crate) meter: Meter,
    pub(crate) details: Option<Col>,
    /// Horizontal span available for Block B's Cores row.
    pub(crate) cores: Col,
    /// The block's units column, between the well and the %, where any of its rows draws units:
    /// block A's (A1 §2.4), and the instruments' block B's (A4 §5), at most [`B_UNITS_MAX`].
    pub(crate) units: Option<UnitsCol>,
    /// Block B's Machine row, where its rows hold one (A4 §8).
    pub(crate) machine: Option<MachineRow>,
}

/// A table's units column (`docs/DESIGN-stats-deck-units-2026-11-04.md` §2.4): where the % column
/// stood without it, [`UNITS_TO_PCT`] short of the % column, and as wide as the widest form its
/// rows take at 1×. Each form stands right-aligned in it, against its row's %.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct UnitsCol {
    pub(crate) at: Col,
    /// The widest column the table takes: U_cap, the widest it holds without its strip losing
    /// the top preset ([`units_cap`]), and in block B no wider than [`B_UNITS_MAX`].
    pub(crate) cap: f64,
    /// Each row's form, in [`UnitRow::ALL`]'s order; `None` draws no units on its row, as on
    /// every row the table does not hold.
    pub(crate) forms: Forms,
}

impl UnitsCol {
    /// `row`'s form; `None` where it draws no units here.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "A4 step N5 paints each row's units")
    )]
    pub(crate) const fn form(&self, row: UnitRow) -> Option<Form> {
        self.forms[row.index()]
    }
}

/// Block B's widest units column, pt (A4 §5, U_B): its forms (the interface tiles, 15 pt at 1×,
/// and the processes' flow, pile and stuck columns, 15) and Latency's TCP waits tile when F8
/// builds the row stand within it, so its strip keeps six hours from 1039 pt.
const B_UNITS_MAX: f64 = 17.0;

impl BlockPlan {
    /// The % column where no bar column fits (fnarrow and fslim): each row that prints a % draws
    /// its three-scope bar under the figure there
    /// (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §5).
    pub(crate) fn bar_under(&self) -> Option<Col> {
        self.pct.filter(|_| self.meter.bar.is_none())
    }

    /// Chooses the [`CoresMode`] for Block B's Cores row.
    pub(crate) fn cores_mode(&self, clusters: &[Cluster<'_>], m: &FaceMetrics) -> CoresMode {
        cores_row_mode(
            clusters,
            self.cores.w,
            self.rows.cell,
            self.kind.cores_ladder(),
            self.kind.cores_caption_pt(),
            self.rows.deck.cores_queue(),
            m,
        )
    }
}

/// Where the strain gauge sits in a Folded deck: at its top left, block A moved right of it
/// (the refined deck, 2026-10-24).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FoldedGauge {
    /// The dial's span across, [`FOLD_GAUGE_R`] each side of its centre.
    pub(crate) at: Col,
    /// X-coordinate of the divider between the dial and block A.
    pub(crate) divider: f64,
    /// The dial's centre, pt below the deck's top: the band's middle ([`FOLD_GAUGE_CY`] in the
    /// instruments' band).
    pub(crate) cy: f64,
}

impl FoldedGauge {
    /// The dial's centre, pt.
    pub(crate) fn centre(&self) -> (f64, f64) {
        (self.at.x + self.at.w / 2.0, self.cy)
    }

    /// The dial's box, pt: `x0, y0, x1, y1` (the painter notes the same box from [`Self::centre`]).
    #[cfg(test)]
    pub(crate) fn dial(&self) -> [f64; 4] {
        let (cx, cy) = self.centre();
        let r = FOLD_GAUGE_R;
        [cx - r, cy - r, cx + r, cy + r]
    }
}

/// Column layout plan for the `Folded` deck form.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FoldedPlan {
    /// Deck width in whole points.
    pub(crate) w: f64,
    /// Whether the deck is too narrow to draw anything besides its background.
    pub(crate) ground_only: bool,
    /// X-coordinate of the vertical divider between the two blocks.
    pub(crate) divider: f64,
    /// The strain gauge at the top left; `None` where not even two slim blocks fit beside it.
    pub(crate) gauge: Option<FoldedGauge>,
    /// Layout plans for Block A and Block B.
    pub(crate) blocks: [BlockPlan; 2],
}

/// Computes the [`FoldedPlan`] the deck paints for deck width `w_pt`: its rows
/// ([`DeckRows::PAINTED`]) with no units column, until A4's step N5 paints the instruments'
/// rows and their units with the machine's [`UnitFacts`] ([`plan_folded_with`]).
pub(crate) fn plan_folded(w_pt: f64, m: &FaceMetrics) -> FoldedPlan {
    folded_by(w_pt, m, &UnitFacts::NONE, fold_spec, DeckRows::PAINTED)
}

/// Computes the instruments' [`FoldedPlan`] for deck width `w_pt` (A4 §11): five rows a block.
/// The gauge comes first: the blocks take the widest kind that fits beside it, and only where
/// not even slim fits beside it with its % (under 461 pt on Helvetica Neue) does the deck draw no
/// gauge and give the blocks its room. Each block then takes the units column `units` measures
/// where it costs the strip nothing of its horizon ([`plan_units`]), block B's no wider than
/// [`B_UNITS_MAX`] (A4 §5), and block B lays its Machine row (A4 §8). Its columns are
/// [`plan_folded`]'s with no facts: the instruments' rows move none.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "A4 step N5 paints the instruments' rows with the host's facts"
    )
)]
pub(crate) fn plan_folded_with(w_pt: f64, m: &FaceMetrics, units: &UnitFacts) -> FoldedPlan {
    folded_by(w_pt, m, units, fold_spec, DeckRows::Instruments)
}

/// [`plan_folded_with`] of `deck`'s rows with each kind's meter measures from `specs`.
fn folded_by(
    w_pt: f64,
    m: &FaceMetrics,
    units: &UnitFacts,
    specs: FoldSpecs,
    deck: DeckRows,
) -> FoldedPlan {
    let w = whole_width(w_pt);
    match lay_folded(w, true, m, units, specs, deck) {
        (plan, true) => plan,
        (_, false) => lay_folded(w, false, m, units, specs, deck).0,
    }
}

/// The Folded plan at `w` with the gauge or without it, and whether some block kind fits.
fn lay_folded(
    w: f64,
    gauge: bool,
    m: &FaceMetrics,
    units: &UnitFacts,
    specs: FoldSpecs,
    deck: DeckRows,
) -> (FoldedPlan, bool) {
    let g = if gauge { FOLD_GAUGE_W } else { 0.0 };
    let mut b = ((w - 2.0 * X0 - BLOCK_GUTTER - g) / 2.0).floor().max(0.0);
    let mut xa = X0 + g;
    if b > B_MAX {
        b = B_MAX;
        xa = X0 + g + ((w - 2.0 * X0 - (g + 2.0 * b + BLOCK_GUTTER)) / 2.0).floor();
    }
    let kind = BlockKind::ALL
        .into_iter()
        .find(|&kind| lay_block(xa, b, kind, m, true, 0.0, specs, deck).1);
    // Where not even slim fits, the % column goes and the strip takes its place.
    let block = |x: f64| match kind {
        Some(kind) => lay_block(x, b, kind, m, true, 0.0, specs, deck).0,
        None => lay_block(x, b, BlockKind::Slim, m, false, 0.0, specs, deck).0,
    };
    let mut blocks = [block(xa), block(xa + b + BLOCK_GUTTER)];
    // Each block's kind and horizon are the ones it has with no units column.
    let rows: [(&[UnitRow], f64); 2] = [
        (&UnitRow::BLOCK_A, f64::INFINITY),
        (&UnitRow::BLOCK_B, B_UNITS_MAX),
    ];
    for (blk, (rows, bound)) in blocks.iter_mut().zip(rows) {
        if let Some(kind) = kind
            && let Some(col) = block_units(blk, units, specs, rows, bound)
        {
            *blk = BlockPlan {
                units: Some(col),
                ..lay_block(blk.at.x, b, kind, m, true, col.at.w, specs, deck).0
            };
        }
    }
    let machine = &mut blocks[1];
    machine.machine = deck
        .machine_row()
        .then(|| machine_row(machine.name, machine.well, machine.pct, machine.rows.cell));
    let plan = FoldedPlan {
        w,
        ground_only: w < MIN_TABLE_W || blocks.iter().any(|blk| blk.strip.end() > blk.at.end()),
        divider: xa + b + 14.0,
        gauge: gauge.then(|| FoldedGauge {
            at: Col::new(xa - g, 2.0 * FOLD_GAUGE_R),
            divider: xa - FOLD_GAUGE_GAP,
            cy: deck.gauge_cy(),
        }),
        blocks,
    };
    (plan, kind.is_some())
}

/// A Folded block kind's meter measures as the planner lays them.
const fn fold_spec(kind: BlockKind) -> MeterSpec {
    match kind {
        BlockKind::Wide => FOLD_WIDE,
        BlockKind::Mid => FOLD_MID,
        BlockKind::Narrow => FOLD_NARROW,
        BlockKind::Slim => FOLD_SLIM,
    }
}

/// Each Folded kind's meter measures: [`fold_spec`], or another block's in a law's control.
type FoldSpecs = fn(BlockKind) -> MeterSpec;

/// One Folded block of `kind` at `x`, `b` wide, holding `deck`'s rows, and whether it fits; `pct`
/// lays the % column between the well and the strip, and without it the strip stands where the
/// column would. A units column `u` pt wide (0 for none) stands where the % column would, and the
/// % column [`UNITS_TO_PCT`] past it.
#[expect(
    clippy::too_many_arguments,
    reason = "a block's place, kind and face, and the measures a law's control varies"
)]
fn lay_block(
    x: f64,
    b: f64,
    kind: BlockKind,
    m: &FaceMetrics,
    pct: bool,
    u: f64,
    specs: FoldSpecs,
    deck: DeckRows,
) -> (BlockPlan, bool) {
    let rows = Rows::folded(kind, deck);
    let (name_base, well_w, pct_gap, pct_w) = match kind {
        BlockKind::Wide => (52.0, 10.0, 2.0, 30.0),
        BlockKind::Mid => (52.0, 10.0, 2.0, 28.0),
        BlockKind::Narrow | BlockKind::Slim => (48.0, 7.0, 0.0, 26.0),
    };
    let spec = specs(kind);
    let name = Col::new(x, name_width(name_base, rows.name_pt, m, deck.row_names()));
    let well = Col::new(name.end(), well_w);
    // The units (the activity now, by part), the % column (the value now) and then the strip,
    // which reads outward from now.
    let units_x = well.end() + pct_gap;
    let pct_col = if u > 0.0 {
        Col::new(units_x + u + UNITS_TO_PCT, pct_w)
    } else {
        Col::new(units_x, pct_w)
    };
    let strip_x = if pct {
        pct_col.end() + PCT_TO_STRIP
    } else {
        well.end() + BARE_STRIP_GAP
    };
    let end = x + b;
    let cores_x = if kind == BlockKind::Slim {
        name.x
    } else {
        units_x
    };
    let mut plan = BlockPlan {
        kind,
        at: Col::new(x, b),
        rows,
        name,
        well,
        strip: Strip::folded(strip_x, least_cols(&spec, least_strip(&spec)), spec.pitch),
        pct: pct.then_some(pct_col),
        meter: Meter::default(),
        details: None,
        cores: Col::new(cores_x, (end - cores_x).max(0.0)),
        units: None,
        machine: None,
    };
    // Slim, the last kind, stands the bar under each % where no column fits; the block fits
    // where its strip does, and with its % where the strip holds six minutes beside it.
    match lay_meter(strip_x, end, &spec) {
        Some((cols, meter, details)) => {
            plan.strip = Strip::folded(strip_x, cols, spec.pitch);
            plan.meter = meter;
            plan.details = details;
            let fits = plan.strip.end() <= end && (!pct || plan.strip.width() >= spec.pct_strip);
            (plan, fits)
        }
        None => (plan, false),
    }
}

/// A block's units column for `rows` at most `bound` pt wide ([`plan_units`]), given `bare`, the
/// block laid with none.
fn block_units(
    bare: &BlockPlan,
    units: &UnitFacts,
    specs: FoldSpecs,
    rows: &[UnitRow],
    bound: f64,
) -> Option<UnitsCol> {
    let room = bare.at.end() - bare.strip.x - UNITS_TO_PCT;
    let spec = specs(bare.kind);
    plan_units(
        bare.pct?.x,
        room,
        bare.strip.spec,
        bare.rows.cell,
        &spec,
        units,
        rows,
        bound,
    )
}

/// A table's units (A1 §2.4, §2.5): each of `rows` takes the first form of its ladder that
/// `units` draws within U_cap at 1× ([`units_cap`]), or within `bound` where that is narrower,
/// and the column, at `at` where the bare table's % stood, is as wide as the widest of them;
/// `None` where no row takes one. The bare table's strip holds `held` with `room` from its start
/// to the table's end less the column's gap to the %, and its kind lays `spec`'s meter: the
/// column keeps that kind and horizon. `cell` is the table's strip cell, which the forms' height
/// is.
#[expect(
    clippy::too_many_arguments,
    reason = "the bare table's measures and the rows and bound its column takes"
)]
fn plan_units(
    at: f64,
    room: f64,
    held: MultiScaleSpec,
    cell: f64,
    spec: &MeterSpec,
    units: &UnitFacts,
    rows: &[UnitRow],
    bound: f64,
) -> Option<UnitsCol> {
    if !units.draw_any(cell, rows) {
        return None;
    }
    let cap = units_cap(room, held, spec)?.min(bound);
    let forms = units.forms_within(cap, cell, rows);
    let w = units.widest(&forms, cell);
    (w > 0.0).then_some(UnitsCol {
        at: Col::new(at, w),
        cap,
        forms,
    })
}

/// U_cap (A1 §2.4): the widest whole-point units column a table holds while its strip keeps the
/// top preset, six hours, beside a bar column, where its bare strip holds `held` with `room`
/// shared by the units column and `spec`'s meter; `None` where `held` is a shorter preset, or the
/// top preset leaves not even the column's gap to the %.
///
/// Only the top preset. A strip that holds a shorter one leaves room beside it (122 pt at 924 pt
/// on Helvetica Neue), but the first widths whose strip holds the top preset leave less than any
/// form (−3 pt at 925 pt), so a form drawn below them would go again as the deck widened. In the
/// top preset's room the cap grows with the table, so no row's form steps down as the deck
/// widens.
fn units_cap(room: f64, held: MultiScaleSpec, spec: &MeterSpec) -> Option<f64> {
    let top = Strip::spec_for(f64::INFINITY);
    let keeps = |u: f64| {
        share_meter(room - u, spec)
            .is_some_and(|(cols, _, _)| Strip::spec_for(f64::from(cols) * spec.pitch - 1.0) == top)
            && column_holds(room - u, spec)
    };
    if held != top || !keeps(0.0) {
        return None;
    }
    // Less room never lays more boxes, and a column that holds at a room holds at every wider
    // one, so `keeps` holds up to the cap and past it nowhere. `share_meter` alone is not
    // monotone: near the bar's least its column can fit at a room and not a point wider, and
    // `lay_meter` stands no column there. At today's measures no top-preset room is such a room,
    // so the guard changes no plan; it keeps the cap to the rooms `lay_meter` stands the column in
    // whatever the measures (`the_units_cap_leaves_a_room_the_bar_column_holds_in`).
    let (mut lo, mut hi) = (0.0, room.floor() + 1.0);
    while hi - lo > 1.0 {
        let mid = ((lo + hi) / 2.0).floor();
        if keeps(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Some(lo)
}

/// Minimum widths at which each full and folded kind fits on a face.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Breakpoints {
    /// Minimum deck widths for [`FullKind::ALL`].
    pub(crate) full: [f64; 4],
    /// Minimum block widths for [`BlockKind::ALL`].
    pub(crate) block: [f64; 4],
    /// Minimum deck widths (`2B + 53` and the gauge's 72) for [`BlockKind::ALL`].
    pub(crate) folded: [f64; 4],
    /// Minimum deck width (`2B + 53`) for slim blocks without the gauge.
    pub(crate) folded_bare: f64,
}

/// Scans whole-point widths to find the [`Breakpoints`] for `m`.
#[cfg(test)]
pub(crate) fn breakpoints(m: &FaceMetrics) -> Breakpoints {
    let first = |fits: &dyn Fn(f64) -> bool| {
        (0..=8192_u32)
            .map(f64::from)
            .find(|&w| fits(w))
            .unwrap_or(f64::INFINITY)
    };
    let deck = DeckRows::PAINTED;
    let full =
        FullKind::ALL.map(|kind| first(&|w| lay_full(w, kind, m, true, 0.0, full_spec, deck).1));
    let block = BlockKind::ALL
        .map(|kind| first(&|b| lay_block(X0, b, kind, m, true, 0.0, fold_spec, deck).1));
    let folded = block.map(|b| 2.0 * b + 2.0 * X0 + BLOCK_GUTTER + FOLD_GAUGE_W);
    Breakpoints {
        full,
        block,
        folded,
        folded_bare: block[3] + block[3] + 2.0 * X0 + BLOCK_GUTTER,
    }
}

const CORES_CAPTION_PT: f64 = 9.5;

const CORES_NOT_READ: [&str; 2] = ["cores not read", "not read"];

/// CPU core cluster in OS performance order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cluster<'a> {
    pub(crate) count: usize,
    /// Cluster name (`"Performance"`), or `None` when unnamed.
    pub(crate) name: Option<&'a str>,
}

/// Display mode for CPU cores.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CoresMode {
    /// Single row of cell-high squares at 10 pt pitch.
    Big,
    /// Two stacked rows of half-height squares.
    Stack,
    /// Three stacked rows of 2 pt squares at 2.5 pt pitch.
    Tiny,
    /// Text-only core count when no square layout fits.
    CountOnly,
}

fn total_cores(clusters: &[Cluster<'_>]) -> usize {
    clusters.iter().map(|c| c.count).sum()
}

/// Returns the width required by a Cores row in `mode`, or `f64::INFINITY` if it cannot fit: each
/// cluster's count, name and squares, and in a square form `queue` pt past them, the run-queue
/// tile and its wall in the instruments' row (A4 §7). A count draws no tile.
pub(crate) fn cores_row_width(
    clusters: &[Cluster<'_>],
    mode: CoresMode,
    cell: f64,
    caption_pt: f64,
    queue: f64,
    m: &FaceMetrics,
) -> f64 {
    let squares = |count: usize| {
        let n = count as f64;
        match mode {
            CoresMode::Big => (n - 1.0) * 10.0 + cell,
            CoresMode::Stack => {
                let side = (cell - 0.5) / 2.0;
                let cols = (n / 2.0).ceil();
                cols * side + (cols - 1.0) * 0.5
            }
            CoresMode::Tiny => ((n / 3.0).ceil() - 1.0) * 2.5 + 2.0,
            CoresMode::CountOnly => f64::INFINITY,
        }
    };
    match mode {
        CoresMode::CountOnly => m.width(&count_words(clusters), caption_pt, false),
        CoresMode::Big | CoresMode::Stack | CoresMode::Tiny => {
            let each = clusters.iter().map(|cluster| {
                let count = format!("{} ", cluster.count);
                m.width(&count, caption_pt, true)
                    + cluster
                        .name
                        .map_or(0.0, |name| m.width(name, caption_pt, false))
                    + 3.0
                    + squares(cluster.count)
            });
            each.sum::<f64>() + 8.0 * clusters.len().saturating_sub(1) as f64 + queue
        }
    }
}

/// Chooses the first [`CoresMode`] in `ladder` that fits `room` with `queue` pt past its squares
/// ([`cores_row_width`]), or `CountOnly`.
pub(crate) fn cores_row_mode(
    clusters: &[Cluster<'_>],
    room: f64,
    cell: f64,
    ladder: &[CoresMode],
    caption_pt: f64,
    queue: f64,
    m: &FaceMetrics,
) -> CoresMode {
    if total_cores(clusters) == 0 {
        return CoresMode::CountOnly;
    }
    ladder
        .iter()
        .copied()
        .find(|&mode| cores_row_width(clusters, mode, cell, caption_pt, queue, m) <= room)
        .unwrap_or(CoresMode::CountOnly)
}

fn count_words(clusters: &[Cluster<'_>]) -> String {
    match total_cores(clusters) {
        0 => CORES_NOT_READ[1].to_owned(),
        total => format!("{total} cores"),
    }
}

/// Formats the text for a `CountOnly` Cores row within `room` points.
pub(crate) fn cores_count_text(
    clusters: &[Cluster<'_>],
    room: f64,
    caption_pt: f64,
    m: &FaceMetrics,
) -> String {
    if total_cores(clusters) == 0 {
        return m.fit(&CORES_NOT_READ, room, caption_pt, false);
    }
    let total = count_words(clusters);
    let named: Vec<String> = clusters
        .iter()
        .filter_map(|c| c.name.map(|name| format!("{} {name}", c.count)))
        .collect();
    if named.is_empty() {
        return m.fit(&[&total], room, caption_pt, false);
    }
    let each = format!("{total}: {}", named.join(", "));
    m.fit(&[&each, &total], room, caption_pt, false)
}

/// Kind of label placed on the header line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Label {
    /// How far back the strip reaches, `"10 min ago"` ([`time_words`]), over or beside it.
    Time,
    /// Right-aligned `"now"` over a percentage column.
    Now,
    /// A scope's name after its swatch, in the legend over the bars or in the key to the bars
    /// under the % figures; the machine's may carry its used of total
    /// (`this Mac · 10c · 14.4/32G · 2.34/4T`).
    Scope(Bar),
    /// Details column header.
    Details,
    /// The worst state no row of block `0` (Full's table, or Folded's block A) or `1` (block B)
    /// says: over its names, or at its strip's left where they leave it no room.
    State(u8),
}

/// Positioned label on the header line.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HeaderLabel {
    pub(crate) label: Label,
    /// Left edge (or right edge for [`Label::Now`]) in points.
    pub(crate) x: f64,
    pub(crate) text: String,
}

/// Dynamic text inputs for the header line.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HeaderText<'a> {
    /// Active tab label (`"Tab 2"`).
    pub(crate) tab: &'a str,
    /// The machine's noun (`"this Mac"` or `"this computer"`): the legend's word for the bar's
    /// grey.
    pub(crate) machine: &'a str,
    /// Whether a strip on screen draws an over mark, by table: Full reads the first entry;
    /// Folded reads block A then block B and prefers a key beside a block that draws the mark.
    /// The glance (Full wide and mid, and Folded) keys "over max" only then; Full narrow and
    /// slim key all three marks by rule K.
    pub(crate) over: [bool; 2],
    /// The machine's used of total, longest first (`10 cores · 14.4 of 32 GiB · 2.34 of 4 TB`,
    /// `10c · 14.4/32G · 2.34/4T`, then the totals alone): the legend says them after the
    /// machine's noun where no row's run says its maximum, each form measured by its widest.
    pub(crate) totals: &'a [TotalsForm],
}

/// Worst-state strings for an `fslim` block header.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StateLabel<'a> {
    /// Full `<Resource> <word> +N` string.
    pub(crate) full: &'a str,
    /// Bare `<Resource> <word>` fallback string.
    pub(crate) bare: &'a str,
}

/// Legend item in the header key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyItem {
    /// Heat ramp from `"cold"` to `"hot"`.
    Ramp,
    /// `"over max"` cell indicator.
    Over,
    /// `"not read"` cell indicator.
    NotRead,
}

impl KeyItem {
    const SEARCH: [Self; 3] = [Self::Over, Self::Ramp, Self::NotRead];
    const DRAW: [Self; 3] = [Self::Ramp, Self::Over, Self::NotRead];

    const fn weight(self) -> u32 {
        match self {
            Self::Over => 4,
            Self::Ramp => 2,
            Self::NotRead => 1,
        }
    }

    /// Returns the advance width of this key item on the header line.
    pub(crate) fn width(self, m: &FaceMetrics) -> f64 {
        let words = |s: &str| m.width(s, HEADER_PT, false);
        match self {
            Self::Ramp => words("cold") + 3.0 + 35.0 + 3.0 + words("hot"),
            Self::Over => 7.0 + words("over max"),
            Self::NotRead => 7.0 + words("not read"),
        }
    }
}

/// Free horizontal span on the header line available for key items.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct KeySpan {
    pub(crate) x0: f64,
    pub(crate) x1: f64,
    /// Whether this span lies above a heat strip.
    pub(crate) strip: bool,
    /// Whether "over max" prefers this strip: in Folded, its block draws an over mark.
    /// Separate from `strip`, which also keeps scope legends off heat strips.
    pub(crate) over: bool,
}

/// Placement of legend key items across free header spans.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KeyPlan {
    /// Free header spans from left to right.
    pub(crate) spans: Vec<KeySpan>,
    /// Placed key items and their x-coordinates from left to right.
    pub(crate) placed: Vec<(KeyItem, f64)>,
}

impl KeyPlan {
    /// Returns whether `item` was placed.
    #[cfg(test)]
    pub(crate) fn has(&self, item: KeyItem) -> bool {
        self.placed.iter().any(|&(placed, _)| placed == item)
    }
}

/// How well an assignment of the key's items does: more weight, then more
/// items over strips, then further left.
#[derive(Clone, Copy)]
struct KeyScore {
    weight: u32,
    over_strips: u32,
    x_sum: f64,
}

impl KeyScore {
    fn beats(self, other: Self) -> bool {
        (self.weight, self.over_strips)
            .cmp(&(other.weight, other.over_strips))
            .then_with(|| other.x_sum.total_cmp(&self.x_sum))
            .is_gt()
    }
}

/// Assigns the [`KeyItem`]s among `items` to free header `spans` to maximize priority weight.
pub(crate) fn place_key(mut spans: Vec<KeySpan>, items: &[KeyItem], m: &FaceMetrics) -> KeyPlan {
    spans.sort_by(|a, b| a.x0.total_cmp(&b.x0));
    let widths = KeyItem::SEARCH.map(|item| item.width(m));
    let narrowest = widths.iter().copied().fold(f64::INFINITY, f64::min);
    let choices: Vec<Option<usize>> = std::iter::once(None)
        .chain(
            (0..spans.len())
                .filter(|&i| spans.get(i).is_some_and(|s| s.x1 - s.x0 >= narrowest))
                .map(Some),
        )
        .collect();
    let score = |combo: [Option<usize>; 3]| {
        let mut used = vec![(0.0, 0_u32); spans.len()];
        let mut score = KeyScore {
            weight: 0,
            over_strips: 0,
            x_sum: 0.0,
        };
        for ((item, width), at) in KeyItem::SEARCH.into_iter().zip(widths).zip(combo) {
            let Some(at) = at else { continue };
            if !items.contains(&item) {
                return None;
            }
            let (span, (taken, n)) = (spans.get(at)?, used.get_mut(at)?);
            *taken += width + if *n > 0 { KEY_GAP } else { 0.0 };
            *n += 1;
            if *taken > span.x1 - span.x0 {
                return None;
            }
            score.weight += item.weight();
            score.over_strips += u32::from(if item == KeyItem::Over {
                span.over
            } else {
                span.strip
            });
            score.x_sum += span.x0;
        }
        Some(score)
    };
    let mut best: Option<(KeyScore, [Option<usize>; 3])> = None;
    // An item `items` leaves out stands nowhere: every combo that places one scores `None`, so it
    // is searched at `None` alone, which visits the combos that score in the same order.
    let [over_at, ramp_at, not_read_at] = KeyItem::SEARCH.map(|item| {
        if items.contains(&item) {
            choices.as_slice()
        } else {
            &[None]
        }
    });
    for &over in over_at {
        for &ramp in ramp_at {
            for &not_read in not_read_at {
                let combo = [over, ramp, not_read];
                if let Some(s) = score(combo)
                    && best.is_none_or(|(b, _)| s.beats(b))
                {
                    best = Some((s, combo));
                }
            }
        }
    }
    let combo = best.map_or([None; 3], |(_, combo)| combo);
    let mut placed = Vec::new();
    for (at, span) in spans.iter().enumerate() {
        let mut x = span.x0;
        for item in KeyItem::DRAW {
            let k = KeyItem::SEARCH.iter().position(|&i| i == item).unwrap_or(0);
            if combo.get(k).copied().flatten() == Some(at) {
                placed.push((item, x));
                x += widths.get(k).copied().unwrap_or(0.0) + KEY_GAP;
            }
        }
    }
    KeyPlan { spans, placed }
}

/// One tick of the time scale over the strip that carries the time label: the strip's left end
/// is now, and a tick `age_s` back sits where the strip's multi-scale axis puts that age
/// ([`Strip::x_at_age_ms`]), further right the further back.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RulerTick {
    /// How long ago the tick is, seconds.
    pub(crate) age_s: u32,
    /// The tick's x, pt.
    pub(crate) x: f64,
    /// Its words' left edge, pt: centred on the tick, or pushed along until clear of the now
    /// marker and the time label, never so far that the tick leaves their middle half
    /// ([`TICK_SLACK`]).
    pub(crate) left: f64,
    /// Its words, `–15m`, `–10s` ([`tick_words`]).
    pub(crate) text: String,
}

/// The words of a tick `age_s` back (`–1h`, `–10m`, `–1m`, `–10s`).
pub(crate) fn tick_words(age_s: u32) -> String {
    format!("\u{2013}{}", compact_age_words(age_s))
}

/// Compact horizon age (`6h`, `1h`, `10m`, `1m`, `10s`) for X-axis ticks and labels.
fn compact_age_words(age_s: u32) -> String {
    if age_s >= 3_600 && age_s.is_multiple_of(3_600) {
        format!("{}h", age_s / 3_600)
    } else if age_s >= 60 && age_s.is_multiple_of(60) {
        format!("{}m", age_s / 60)
    } else {
        format!("{age_s}s")
    }
}

/// The time label over (or beside) `strip`, long then short: how far back its multi-scale
/// horizon reaches (`6h ago` / `–6h`, `4h ago` / `–4h`, `6m ago` / `–6m`, `1m ago` / `–1m`).
pub(crate) fn time_words(strip: Strip) -> [String; 2] {
    let span = compact_age_words(strip.span_s());
    [format!("{span} ago"), format!("\u{2013}{span}")]
}

/// Interior horizon ages `strip`'s multi-scale X-axis marks, most wanted first: the coarsest
/// (they say how the scale compresses), then finer; each is where a block size starts.
pub(crate) fn tick_ages(strip: Strip) -> Vec<u32> {
    let horizon_s = strip.span_s();
    strip
        .spec
        .ticks
        .iter()
        .rev()
        .filter_map(|&(age_ms, _)| u32::try_from(age_ms / 1_000).ok())
        .filter(|&age_s| age_s > 0 && age_s < horizon_s)
        .collect()
}

/// Where a tick `age_s` back on `strip` goes when its words, `w` wide, must stay inside
/// `lo..hi` with the tick under their middle half, or `None` when they cannot.
fn tick_slot(strip: Strip, age_s: u32, w: f64, (lo, hi): (f64, f64)) -> Option<(f64, f64)> {
    let x = strip.x_at_age_ms(i64::from(age_s) * 1_000)?;
    let centred = x - w / 2.0;
    let slack = w * TICK_SLACK;
    let (least, most) = (lo.max(centred - slack), (hi - w).min(centred + slack));
    (least <= most).then(|| (x, centred.clamp(least, most)))
}

/// Where the time label stands on the header line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimeAt {
    /// Over the strip's right (old) end, right-aligned to it, the scale's ticks before it.
    Over,
    /// Beside the strip, starting [`STRIP_LEAD`] past its right end (where a tick marks it), in
    /// a free span the scope key leaves there: the strip's whole header is then the scale's.
    Beside,
}

/// Where a time label can stand, and what it leaves.
struct TimeRoom {
    /// The label's left edge, pt.
    x: f64,
    /// The key's spans with the label's own place taken out of them.
    spans: Vec<KeySpan>,
    /// The strip's own free header, pt: from past the now marker to short of the label.
    key: (f64, f64),
    /// The room it leaves the ticks' words, pt.
    ticks: (f64, f64),
}

/// The time label and its scale as the planner settles them.
struct TimeScale {
    label: String,
    /// The label's left edge, pt.
    x: f64,
    ticks: Vec<RulerTick>,
    /// The tick at the strip's right end when the label stands beside it.
    end_tick: Option<f64>,
    /// The key's spans with what the label and the ticks leave of them.
    spans: Vec<KeySpan>,
}

/// Complete layout plan for the header line and its legend key.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct HeaderPlan {
    pub(crate) labels: Vec<HeaderLabel>,
    pub(crate) key: KeyPlan,
    /// The time scale over the strip that carries the time label, left to right.
    pub(crate) ruler: Vec<RulerTick>,
    /// The tick at that strip's right end (its horizon), pt, when the time label stands beside
    /// the strip rather than over it.
    pub(crate) time_tick: Option<f64>,
    /// The key's items this header may place (rule K searches only these).
    pub(crate) keys: &'static [KeyItem],
    /// The left end (pt) of the strip whose now marker draws no dot: a Folded slim block's
    /// state words too long for the room before the marker take that strip's header too, and
    /// its line alone marks now ([`header_folded`]).
    pub(crate) dotless: Option<f64>,
}

struct Header<'m> {
    m: &'m FaceMetrics,
    labels: Vec<HeaderLabel>,
    spans: Vec<KeySpan>,
    /// The strip whose header carries the time label and the scale.
    time: Option<Strip>,
    /// Whether the time strip's block draws an over mark (Full retains strip preference).
    time_over: bool,
    /// Where the time strip's header is free from, pt: past the now marker's room, or past a
    /// state standing at the strip's left ([`Self::state`]).
    lead: f64,
    /// The key's items this header may place.
    keys: &'static [KeyItem],
    /// [`HeaderPlan::dotless`].
    dotless: Option<f64>,
    /// The scope key's Global and Tab words where the bars stand under the % figures, placed
    /// last ([`Self::place_under_key`]).
    under_key: Option<(String, String)>,
    /// A legend over the time strip's header from its x, in the forms that fit there, longest
    /// first, and the scope key's words that stand for it where each would cost the scale its
    /// last tick ([`Self::finish`]).
    strip_legend: Option<(f64, Vec<LegendForm>, (String, String))>,
}

/// One form of the legend: the machine's words, the tab's, and the width the three take.
type LegendForm = (String, String, f64);

/// Where a block's worst state stood ([`Header::state`]).
#[derive(Clone, Copy, Debug, PartialEq)]
enum StateAt {
    /// No state: `now` stands over the %, and the names' header is free up to the returned x.
    Free(f64),
    /// Over the names, beside `now`.
    Names,
    /// At the strip's left; its header is free from the returned x.
    Lead(f64),
    /// Over the names and the % column, `now` yielding to it.
    Yielded,
}

impl<'m> Header<'m> {
    const fn new(m: &'m FaceMetrics) -> Self {
        Self {
            m,
            labels: Vec::new(),
            spans: Vec::new(),
            time: None,
            time_over: true,
            lead: 0.0,
            keys: &KeyItem::SEARCH,
            dotless: None,
            under_key: None,
            strip_legend: None,
        }
    }

    fn words(&self, s: &str) -> f64 {
        self.m.width(s, HEADER_PT, false)
    }

    fn label(&mut self, label: Label, x: f64, text: impl Into<String>) {
        self.labels.push(HeaderLabel {
            label,
            x,
            text: text.into(),
        });
    }

    fn span(&mut self, x0: f64, x1: f64, strip: bool) {
        self.spans.push(KeySpan {
            x0,
            x1,
            strip,
            over: strip,
        });
    }

    fn strip_span(&mut self, x0: f64, x1: f64, over: bool) {
        self.spans.push(KeySpan {
            x0,
            x1,
            strip: true,
            over,
        });
    }

    fn time(&mut self, strip: Strip) {
        self.time = Some(strip);
    }

    /// `"now"` over the % column before `strip`, right-aligned just left of the now marker's
    /// widest beat ([`WORDS_TO_STRIP`]); answers where words before it end ([`STRIP_LEAD`] short
    /// of it).
    fn now(&mut self, strip: Strip) -> f64 {
        let end = strip.x - WORDS_TO_STRIP;
        self.label(Label::Now, end, "now");
        end - self.words("now") - STRIP_LEAD
    }

    /// A block's `now` and its worst `state` (`docs/DESIGN-stats-deck-meter-2026-10-26.md` §4):
    /// with no state, `now` over the % and the names' header free; else the state over the names
    /// beside `now` where it fits whole, else at its strip's left, past the now marker's room,
    /// where it leaves the strip's header `keep` pt before `limit` (the time label, or the
    /// legend's least), so `now` stays; else, as an fslim block's, over the names and the %
    /// column, `now` yielding to it, up to the marker's beat.
    #[expect(
        clippy::too_many_arguments,
        reason = "one block's names, % and strip, and what its header keeps"
    )]
    fn state(
        &mut self,
        block: u8,
        name_x: f64,
        pct: bool,
        strip: Strip,
        state: Option<StateLabel<'_>>,
        keep: f64,
        limit: f64,
    ) -> StateAt {
        let words_end = strip.x - WORDS_TO_STRIP;
        let now_w = if pct {
            self.words("now") + STRIP_LEAD
        } else {
            0.0
        };
        let Some(st) = state else {
            let end = if pct { self.now(strip) } else { words_end };
            return StateAt::Free(end);
        };
        let forms = [st.full, st.bare];
        let w = |f: &str| MARK_ADVANCE + self.m.width(f, HEADER_PT, true);
        if let Some(f) = forms
            .into_iter()
            .find(|f| w(f) <= words_end - now_w - name_x + 0.01)
        {
            if pct {
                self.now(strip);
            }
            self.label(Label::State(block), name_x, f);
            return StateAt::Names;
        }
        let lo = strip.x + MARKER_ROOM;
        if let Some(f) = forms
            .into_iter()
            .find(|f| lo + w(f) + TICK_GAP + keep <= limit + 0.01)
        {
            if pct {
                self.now(strip);
            }
            self.label(Label::State(block), lo, f);
            return StateAt::Lead(lo + w(f) + TICK_GAP);
        }
        let room = words_end - name_x - MARK_ADVANCE;
        let words = self.m.fit(&forms, room, HEADER_PT, true);
        self.label(Label::State(block), name_x, words);
        StateAt::Yielded
    }

    fn scope(&mut self, bar: Bar, x: f64, words: String) -> f64 {
        let w = SWATCH_ADVANCE + self.words(&words);
        self.label(Label::Scope(bar), x, words);
        w
    }

    /// The legend over the bars (`■ this Mac ■ aterm ■ Tab 1`), once, in `lo..hi`, from `lo` or,
    /// with `right`, ending at `hi` (`docs/DESIGN-stats-deck-meter-2026-10-26.md` §4): each
    /// scope's swatch in the bar's ink and its name, [`LEGEND_GAP`] apart. With `tail`, the
    /// machine's name first carries its used of total (`this Mac · 10c · 14.4/32G · 2.34/4T`,
    /// `docs/DESIGN-stats-deck-flow-2026-11-02.md` §4), the longest form whose widest
    /// ([`TotalsForm::widest`]) fits, so a changing reading never moves it to another form. Where
    /// none fits, the totals go first, then the machine's noun gives way to its last word
    /// (`Mac`), then the tab's title to `Tab`; a title past [`TAB_HEAD_MAX`] is `Tab` from the
    /// first. Where even that does not fit, nothing is laid (`None`). Answers where it starts and
    /// ends.
    fn legend(
        &mut self,
        (lo, hi): (f64, f64),
        text: &HeaderText<'_>,
        tail: bool,
        right: bool,
    ) -> Option<(f64, f64)> {
        let (global, tab, _) = self
            .legend_forms(text, tail)
            .into_iter()
            .find(|form| form.2 <= hi - lo + 1e-9)?;
        // The form fits by its widest; right-aligned, it stands by its words.
        let x = if right {
            hi - self.legend_need(&global, &tab)
        } else {
            lo
        };
        Some((x, self.legend_at(x, &global, &tab)))
    }

    /// How wide the legend is with the machine's words `global` and the tab's `tab`.
    fn legend_need(&self, global: &str, tab: &str) -> f64 {
        3.0 * SWATCH_ADVANCE
            + self.words(global)
            + self.words("aterm")
            + self.words(tab)
            + 2.0 * LEGEND_GAP
    }

    /// The legend's forms, longest first, each with the room it needs: with the machine's used
    /// of total where `tail`, measured by its widest, then the machine's name and the tab's
    /// title (`Tab` past [`TAB_HEAD_MAX`]), the machine's last word, then `Tab`.
    fn legend_forms(&self, text: &HeaderText<'_>, tail: bool) -> Vec<LegendForm> {
        let machine = text.machine;
        let noun = machine.rsplit(' ').next().unwrap_or(machine);
        let tab = if self.words(text.tab) > TAB_HEAD_MAX {
            "Tab"
        } else {
            text.tab
        };
        let totals = text.totals.iter().filter(|_| tail).map(|t| {
            let need = self.legend_need(&format!("{machine} \u{b7} {}", t.widest), tab);
            (format!("{machine} \u{b7} {}", t.said), tab.to_owned(), need)
        });
        let plain = [(machine, tab), (noun, tab), (noun, "Tab")]
            .map(|(g, t)| (g.to_owned(), t.to_owned(), self.legend_need(g, t)));
        totals.chain(plain).collect()
    }

    /// Lays the legend's three names from `x`, each after its swatch; where it ends.
    fn legend_at(&mut self, x: f64, global: &str, tab: &str) -> f64 {
        let mut at = x;
        for (bar, words) in [
            (Bar::Global, global),
            (Bar::Aterm, "aterm"),
            (Bar::Tab, tab),
        ] {
            at += self.scope(bar, at, words.to_owned()) + LEGEND_GAP;
        }
        at - LEGEND_GAP
    }

    /// The scope key (`this Mac ■ aterm ■ Tab`) where the bars stand under the % figures
    /// (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §5) or no legend stands over them, once
    /// the scale and rule K's items have their places: in the leftmost room they leave over no
    /// strip that holds its three names (the machine's noun giving way to its last word, `Mac`),
    /// the Tab's cut to fit as over a bar column but given up to 48 pt. Where none does, the key
    /// is dropped, before the scale, and How to read still names the inks.
    fn place_under_key(&mut self, key: &KeyPlan, global: &str, tab: &str) {
        let noun = global.rsplit(' ').next().unwrap_or(global);
        let need = |global: &str| {
            3.0 * SWATCH_ADVANCE
                + self.words(global)
                + 8.0
                + self.words("aterm")
                + 8.0
                + self.words(tab).min(TAB_HEAD_MAX)
        };
        let rooms: Vec<(f64, f64)> = key
            .spans
            .iter()
            .filter(|span| !span.strip)
            .map(|span| {
                let x0 = key
                    .placed
                    .iter()
                    .filter(|&&(_, x)| span.x0 <= x + 1e-9 && x < span.x1)
                    .map(|&(item, x)| x + item.width(self.m) + KEY_GAP)
                    .fold(span.x0, f64::max);
                (x0, span.x1)
            })
            .collect();
        let found = [global, noun].into_iter().find_map(|global| {
            rooms
                .iter()
                .copied()
                .filter(|&(x0, x1)| x1 - x0 >= need(global) - 1e-9)
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .map(|room| (global, room))
        });
        if let Some((global, (x0, x1))) = found {
            let mut x = x0;
            for (bar, words) in [
                (Bar::Global, global),
                (Bar::Aterm, "aterm"),
                (Bar::Tab, tab),
            ] {
                let words = if bar == Bar::Tab {
                    self.m
                        .fit(&[words], x1 - x - SWATCH_ADVANCE, HEADER_PT, false)
                } else {
                    words.to_owned()
                };
                if words.is_empty() {
                    break;
                }
                x += self.scope(bar, x, words) + 8.0;
            }
        }
    }

    /// `base` (the key's spans) with the time strip's own: what lies in `room` (from past the
    /// now marker to short of the time label) that `ticks` leave.
    fn spans_with(&self, base: &[KeySpan], room: (f64, f64), ticks: &[RulerTick]) -> Vec<KeySpan> {
        let mut spans = base.to_vec();
        let mut x0 = room.0;
        let mut by_x: Vec<&RulerTick> = ticks.iter().collect();
        by_x.sort_by(|a, b| a.left.total_cmp(&b.left));
        for tick in by_x {
            spans.push(KeySpan {
                x0,
                x1: tick.left - KEY_GAP,
                strip: true,
                over: self.time_over,
            });
            x0 = tick.left + self.words(&tick.text) + KEY_GAP;
        }
        spans.push(KeySpan {
            x0,
            x1: room.1,
            strip: true,
            over: self.time_over,
        });
        spans.retain(|s| s.x1 > s.x0);
        spans
    }

    /// The ticks whose words fit `room` on `strip`, each alone, most wanted first.
    fn tick_room(&self, strip: Strip, room: (f64, f64)) -> Vec<RulerTick> {
        tick_ages(strip)
            .into_iter()
            .filter_map(|age_s| {
                let text = tick_words(age_s);
                let (x, left) = tick_slot(strip, age_s, self.words(&text), room)?;
                Some(RulerTick {
                    age_s,
                    x,
                    left,
                    text,
                })
            })
            .collect()
    }

    /// Where a time label `w` wide can stand `at` `strip`, or `None` when no free span the
    /// scope key leaves right of the strip starts at its lead to hold it beside the strip.
    fn time_room(&self, strip: Strip, w: f64, at: TimeAt) -> Option<TimeRoom> {
        let lo = (strip.x + MARKER_ROOM).max(self.lead);
        match at {
            TimeAt::Over => {
                let x = strip.end() - w;
                Some(TimeRoom {
                    x,
                    spans: self.spans.clone(),
                    key: (lo, x - KEY_GAP),
                    ticks: (lo, x - TICK_GAP),
                })
            }
            TimeAt::Beside => {
                let x = strip.end() + STRIP_LEAD;
                let at = self.spans.iter().position(|s| {
                    !s.strip && s.x0 >= strip.end() - 1e-9 && s.x0 <= x + 1e-9 && s.x1 >= x + w
                })?;
                let mut spans = self.spans.clone();
                if let Some(span) = spans.get_mut(at) {
                    span.x0 = x + w + KEY_GAP;
                }
                spans.retain(|s| s.x1 > s.x0);
                Some(TimeRoom {
                    x,
                    spans,
                    key: (lo, x - KEY_GAP),
                    ticks: (lo, x - TICK_GAP),
                })
            }
        }
    }

    /// What `spans` let the key keep: the items rule K places, and whether the heat ramp stands
    /// over a strip, the colours it explains.
    fn key_keeps(&self, spans: Vec<KeySpan>) -> (Vec<KeyItem>, bool) {
        let plan = place_key(spans, self.keys, self.m);
        let items = plan.placed.iter().map(|&(item, _)| item).collect();
        let ramp_over_strip = plan.placed.iter().any(|&(item, x)| {
            item == KeyItem::Ramp
                && plan
                    .spans
                    .iter()
                    .any(|s| s.strip && s.x0 <= x + 1e-9 && x < s.x1)
        });
        (items, ramp_over_strip)
    }

    /// The time label and its scale. The label stands over the strip's right (old) end,
    /// right-aligned to it, in its long form, else its short one, when that leaves a tick room;
    /// else beside the strip, past that end, long then short; else long and alone over the
    /// strip. The scale is weighed against the key the long label alone leaves (v2's own
    /// header, rule K): it takes ticks in order of want while the
    /// key keeps every item it places and the heat ramp over a strip; failing that, every item;
    /// failing that, every item but its least, "not read", since in rule K's order the scale
    /// ranks after the ramp and before "not read"; failing that, "over max" alone: where a narrow
    /// deck's strip header holds the ramp or a tick but not both, the scale reads the time series
    /// the row is built around and the ramp gives way. The test is the
    /// items themselves, not their summed weights, which can hold while one item is traded for
    /// another. It never costs "over max" a place; where every placement would, the label stands
    /// alone.
    fn ruler(&self, strip: Strip) -> TimeScale {
        let [long, short] = time_words(strip);
        let alone_x = strip.end() - self.words(&long);
        let alone = ((strip.x + MARKER_ROOM).max(self.lead), alone_x - KEY_GAP);
        let (items, ramp) = self.key_keeps(self.spans_with(&self.spans, alone, &[]));
        let but_not_read: Vec<KeyItem> = items
            .iter()
            .copied()
            .filter(|&item| item != KeyItem::NotRead)
            .collect();
        // Where the strip's header holds no tick beside them, the ramp's and not read's keys give
        // way to the scale; the over mark's never does.
        let over_only: Vec<KeyItem> = items
            .iter()
            .copied()
            .filter(|&item| item == KeyItem::Over)
            .collect();
        let placements = [
            (&long, TimeAt::Over),
            (&short, TimeAt::Over),
            (&long, TimeAt::Beside),
            (&short, TimeAt::Beside),
        ];
        let costs = [
            (items.as_slice(), ramp),
            (items.as_slice(), false),
            (but_not_read.as_slice(), false),
            (over_only.as_slice(), false),
        ];
        let clear = |chosen: &[RulerTick], t: &RulerTick| {
            let w = self.words(&t.text);
            chosen.iter().all(|c| {
                let cw = self.words(&c.text);
                t.left + w + TICK_GAP <= c.left || c.left + cw + TICK_GAP <= t.left
            })
        };
        for (must, ramp) in costs {
            for (label, at) in placements {
                let Some(TimeRoom {
                    x,
                    spans: base,
                    key,
                    ticks: room,
                }) = self.time_room(strip, self.words(label), at)
                else {
                    continue;
                };
                let mut chosen: Vec<RulerTick> = Vec::new();
                for t in self.tick_room(strip, room) {
                    let mut trial = chosen.clone();
                    trial.push(t.clone());
                    let (kept, r) = self.key_keeps(self.spans_with(&base, key, &trial));
                    if clear(&chosen, &t) && must.iter().all(|i| kept.contains(i)) && (r || !ramp) {
                        chosen = trial;
                    }
                }
                if chosen.is_empty() {
                    continue;
                }
                chosen.sort_by(|a, b| a.left.total_cmp(&b.left));
                return TimeScale {
                    label: label.clone(),
                    x,
                    spans: self.spans_with(&base, key, &chosen),
                    ticks: chosen,
                    end_tick: (at == TimeAt::Beside).then_some(strip.end()),
                };
            }
        }
        TimeScale {
            label: long,
            x: alone_x,
            spans: self.spans_with(&self.spans, alone, &[]),
            ticks: Vec::new(),
            end_tick: None,
        }
    }

    /// The header's plan: the time label and its scale over the time strip, a legend over that
    /// strip's header where one waits and a form of it leaves the scale a tick (else the scope
    /// key the legend stands for), the key's items, and the scope key where it waits.
    fn finish(mut self) -> HeaderPlan {
        let (mut ruler, mut time_tick) = (Vec::new(), None);
        if let Some(strip) = self.time {
            let mut scale = self.ruler(strip);
            if let Some((x, forms, key)) = self.strip_legend.take() {
                let lead = self.lead;
                let fit = forms.into_iter().find_map(|(global, tab, w)| {
                    self.lead = lead.max(x + w + KEY_GAP);
                    let scale = self.ruler(strip);
                    (!scale.ticks.is_empty()).then_some((global, tab, scale))
                });
                match fit {
                    Some((global, tab, with)) => {
                        self.legend_at(x, &global, &tab);
                        scale = with;
                    }
                    None => {
                        self.lead = lead;
                        self.under_key = Some(key);
                    }
                }
            }
            self.spans = scale.spans;
            self.label(Label::Time, scale.x, scale.label);
            ruler = scale.ticks;
            time_tick = scale.end_tick;
        }
        let key = place_key(std::mem::take(&mut self.spans), self.keys, self.m);
        if let Some((global, tab)) = self.under_key.take() {
            self.place_under_key(&key, &global, &tab);
        }
        HeaderPlan {
            key,
            labels: self.labels,
            ruler,
            time_tick,
            keys: self.keys,
            dotless: self.dotless,
        }
    }
}

/// The header line of `strip` alone with the key's free `spans` beside it, keying the over mark
/// (the tests' way to a time placement no deck width needs on the faces the deck draws).
#[cfg(test)]
pub(crate) fn header_of_strip(strip: Strip, spans: &[KeySpan], m: &FaceMetrics) -> HeaderPlan {
    let mut head = Header::new(m);
    head.keys = &[KeyItem::Over];
    head.spans = spans.to_vec();
    head.time(strip);
    head.finish()
}

/// Lays out the header line and key for a `Full` deck plan. `state` is the worst of the states
/// no row says (in its Details, or in its run's cell after the reading, which the painter
/// measures); where no Details stands, it stands over the names beside `now`, else at the
/// strip's left before the scale, else over the names and the % column, `now` yielding to it
/// ([`Header::state`]). The legend keys the bars once, over the bar's column from its left edge
/// ([`Header::legend`]), or where the bars stand under each % over the strip's header at its
/// left, before the scale; with the machine's totals where no run says the rows' maxima. Where it
/// does not fit there the scope key takes what room the scale and rule K's items leave, or none.
pub(crate) fn header_full(
    plan: &FullPlan,
    text: HeaderText<'_>,
    state: Option<StateLabel<'_>>,
    m: &FaceMetrics,
) -> HeaderPlan {
    if plan.ground_only {
        return HeaderPlan::default();
    }
    let mut head = Header::new(m);
    head.time(plan.strip);
    // Wide and mid say the glance's header: "now" beside the live edge's dot (the owner's), the
    // scopes' names alone, no "Details" (its clauses say what they are), and a key for the over
    // mark only while a strip draws one.
    let long = matches!(plan.kind, FullKind::Wide | FullKind::Mid);
    if long {
        head.keys = if text.over[0] { &[KeyItem::Over] } else { &[] };
    }
    let state = state.filter(|_| plan.details.is_none());
    // A state at the strip's left leaves the time label room in its long form, which the scale
    // keeps when no tick fits beside it.
    let label = head.words(&time_words(plan.strip)[0]);
    let at = head.state(
        0,
        plan.name.x,
        plan.pct.is_some(),
        plan.strip,
        state,
        label,
        plan.strip.end(),
    );
    match at {
        StateAt::Free(names_end) => {
            if plan.gauge.is_none() {
                head.span(plan.name.x, names_end, false);
            }
        }
        StateAt::Lead(lead) => head.lead = lead,
        StateAt::Names | StateAt::Yielded => {}
    }
    // Over the bars and their runs, to Details or the deck's right margin: the legend, then room
    // for the over mark's key; narrow and slim key the strip's ramp and not read by rule K too.
    // Where no legend fits there, the scope key takes what room rule K and the scale leave.
    let meter_end = plan.details.map_or(plan.w - X0, |d| d.x - KEY_GAP);
    let tail = plan.meter.run.is_none();
    let legend = if let Some(bar) = plan.meter.bar {
        head.legend((bar.x, meter_end), &text, tail, false)
            .map(|(_, end)| head.span(end + KEY_GAP, meter_end, false))
    } else if plan.bar_under().is_some() {
        // The bars stand under each %: the legend over the strip's header at its left, past the
        // now marker's room (or the state there), in the longest form that leaves the time label
        // its room at the end and the scale a tick ([`Header::finish`]).
        let lo = (plan.strip.x + MARKER_ROOM).max(head.lead);
        let hi = plan.strip.end() - label - KEY_GAP;
        let forms = head
            .legend_forms(&text, tail)
            .into_iter()
            .filter(|form| lo + form.2 <= hi + 1e-9)
            .collect();
        let key = (text.machine.to_owned(), text.tab.to_owned());
        head.strip_legend = Some((lo, forms, key));
        Some(())
    } else {
        None
    };
    if legend.is_none() && (plan.meter.bar.is_some() || plan.bar_under().is_some()) {
        if let Some(bar) = plan.meter.bar {
            head.span(bar.x, meter_end, false);
        }
        head.under_key = Some((text.machine.to_owned(), text.tab.to_owned()));
    }
    if let Some(details) = plan.details {
        if long {
            head.span(details.x, details.end(), false);
        } else {
            let words = if head.words(DETAILS_LONG) <= details.w {
                DETAILS_LONG
            } else {
                "Details"
            };
            head.label(Label::Details, details.x, words);
            head.span(
                details.x + head.words(words) + KEY_GAP,
                details.end(),
                false,
            );
        }
    }
    head.finish()
}

/// Lays out the header line and key for a `Folded` deck plan. Folded says the glance (§6.8) at
/// every tier, so its header does as Full wide and mid's: "now" over each %, "this Mac" for
/// Global, no "Details", and a key for the over mark only while a strip draws one. `states` are
/// each block's worst that no row of it says (the painter measures its runs' cells). A strip
/// span is preferred for "over max" only when its block draws the mark; otherwise the key takes
/// the leftmost room, without changing which spans physically lie over strips.
///
/// Where the blocks lay a bar column (`docs/DESIGN-stats-deck-meter-2026-10-26.md` §4), block A's
/// strip carries the time label and the scale, and block B's header ends with the legend, once
/// for both blocks' bars: right-aligned to B's meter, over its bars and as far left over its
/// strip's old end as it needs, with the machine's used of total where no run says the rows'
/// maxima, in the longest form that leaves the over mark's key its room over B's strip whether or
/// not a cell draws the mark, so a changing reading never flips its form; where the form that
/// fits beside the key says fewer figures than one that fits in its room, or none fits beside
/// it, the legend takes that room and the key what is left. It never crosses the blocks'
/// divider. Each block's state stands over its names beside `now`, else at its strip's left,
/// leaving A's scale its short label and B's legend its least ([`Header::state`]); else over its
/// names and %, `now` yielding.
///
/// Where the bars stand under each % (fslim), an fslim block's state words stand over its names,
/// and where they need more room they take the % column's too, `now` yielding to them, up to
/// where `now` ends ([`WORDS_TO_STRIP`]): never past it, where the now marker's dot beats, so both
/// strips keep their headers and block A's carries the time label. Where even the room before
/// the marker is too short, one block's words take its strip's header too, its marker's dot
/// yields ([`HeaderPlan::dotless`]), and the other block's strip carries the time label. The
/// scope key then takes the leftmost room left over no strip ([`Header::place_under_key`]).
pub(crate) fn header_folded(
    plan: &FoldedPlan,
    text: HeaderText<'_>,
    states: [Option<StateLabel<'_>>; 2],
    m: &FaceMetrics,
) -> HeaderPlan {
    if plan.ground_only {
        return HeaderPlan::default();
    }
    let [a, b] = plan.blocks;
    let mut head = Header::new(m);
    head.keys = if text.over.contains(&true) {
        &[KeyItem::Over]
    } else {
        &[]
    };
    let (Some(_), Some(b_end)) = (a.meter.bar, b.meter.end()) else {
        return header_folded_under(head, plan, text, states);
    };
    let legend_hi = b_end.min(b.at.end());
    let label = head.words(&time_words(a.strip)[0]);
    let keep = [
        (label, a.strip.end()),
        (legend_least(m) + KEY_GAP - TICK_GAP, legend_hi),
    ];
    let mut b_lead = b.strip.x + MARKER_ROOM;
    for (i, ((block, state), (keep, limit))) in plan.blocks.iter().zip(states).zip(keep).enumerate()
    {
        let at = head.state(
            u8::try_from(i).unwrap_or(1),
            block.name.x,
            block.pct.is_some(),
            block.strip,
            state,
            keep,
            limit,
        );
        let lead = match at {
            StateAt::Free(names_end) => {
                head.span(block.name.x, names_end, false);
                None
            }
            StateAt::Lead(lead) => Some(lead),
            StateAt::Names | StateAt::Yielded => None,
        };
        match (i, lead) {
            (0, Some(lead)) => head.lead = lead,
            (_, Some(lead)) => b_lead = lead + KEY_GAP - TICK_GAP,
            _ => {}
        }
        if let Some(details) = block.details {
            head.span(details.x, details.end(), false);
        }
    }
    // Block A's meter keeps its header free; block B's ends with the legend.
    if let (Some(bar), Some(end)) = (a.meter.bar, a.meter.end()) {
        head.span(bar.x, end.min(a.at.end()), false);
    }
    let tail = a.meter.run.is_none();
    // The over mark's key keeps its room over B's strip before the legend takes a longer
    // wording (`docs/DESIGN-stats-deck-meter-2026-10-26.md` §4), whether or not a cell draws the
    // mark now: the over flags are readings, and a changing reading never flips the legend's
    // form. It never costs a figure: where the form that fits beside it says fewer (the totals
    // alone for the used of total, or no totals, which where no run stands alone say each row's
    // maximum), the legend takes the room and the key what is left.
    let key_room = KeyItem::Over.width(m) + KEY_GAP;
    let forms = head.legend_forms(&text, tail);
    let first_fit = |lo: f64| forms.iter().position(|f| f.2 <= legend_hi - lo + 1e-9);
    // What a form says: the used of total, the totals alone, or no totals.
    let figures = |i: usize| match text.totals.get(i).filter(|_| tail) {
        Some(t) if t.used => 0,
        Some(_) => 1,
        None => 2,
    };
    let keyed = first_fit(b_lead + key_room)
        .zip(first_fit(b_lead))
        .is_some_and(|(keyed, alone)| figures(keyed) == figures(alone));
    let lo = if keyed { b_lead + key_room } else { b_lead };
    let legend = head.legend((lo, legend_hi), &text, tail, true);
    match legend {
        Some((x0, _)) => head.strip_span(b_lead, x0 - KEY_GAP, text.over[1]),
        None => {
            head.strip_span(b_lead, legend_hi, text.over[1]);
            head.under_key = Some((text.machine.to_owned(), text.tab.to_owned()));
        }
    }
    head.time(a.strip);
    head.time_over = text.over[0];
    head.finish()
}

/// [`header_folded`] where the bars stand under each %: the fslim header.
fn header_folded_under(
    mut head: Header<'_>,
    plan: &FoldedPlan,
    text: HeaderText<'_>,
    states: [Option<StateLabel<'_>>; 2],
) -> HeaderPlan {
    let m = head.m;
    let mut spilled = None;
    for (i, (block, state)) in plan.blocks.iter().zip(states).enumerate() {
        // Where words left of the now marker end, clear of its beat.
        let words_end = block.strip.x - WORDS_TO_STRIP;
        let names_end = if block.pct.is_some() {
            words_end - head.words("now") - STRIP_LEAD
        } else {
            words_end
        };
        let block_ix = u8::try_from(i).unwrap_or(1);
        match state {
            None => {
                if block.pct.is_some() {
                    head.now(block.strip);
                }
                head.span(block.name.x, names_end, false);
            }
            Some(state) => {
                let name_room = names_end - block.name.x;
                let forms = [state.full, state.bare];
                // The room before the marker's dot and its beat, left of the strip.
                let before_dot = words_end - block.name.x;
                let fits_before =
                    |f: &&str| MARK_ADVANCE + m.width(f, HEADER_PT, true) <= before_dot + 0.01;
                let words = if MARK_ADVANCE + m.width(state.full, HEADER_PT, true) <= name_room {
                    if block.pct.is_some() {
                        head.now(block.strip);
                    }
                    state.full.to_owned()
                } else if spilled.is_none() && !forms.iter().any(fits_before) {
                    // Words too long for the room before the marker take the strip's header
                    // too, to its old end: the marker's dot and `now` yield to them (its line
                    // still marks now down the rows), and the time scale stands over the other
                    // block's strip. One block spills at most.
                    spilled = Some(i);
                    let room = block.strip.end() - block.name.x - MARK_ADVANCE;
                    m.fit(&forms, room, HEADER_PT, true)
                } else {
                    m.fit(&forms, before_dot - MARK_ADVANCE, HEADER_PT, true)
                };
                head.label(Label::State(block_ix), block.name.x, words);
            }
        }
        if let Some(details) = block.details {
            head.span(details.x, details.end(), false);
        }
    }
    let under = plan.blocks.iter().any(|block| block.bar_under().is_some());
    let [a, b] = plan.blocks.map(|block| block.strip);
    // Where neither block spills, the legend stands over block B's strip, right-aligned to its
    // old end as over a bar column, and the key takes what it leaves; else the scope key takes
    // what room the scale and rule K's items leave.
    let mut legend = false;
    match spilled {
        None => {
            head.time(a);
            head.time_over = text.over[0];
            let lo = b.x + MARKER_ROOM;
            let at = if under {
                head.legend((lo, b.end()), &text, true, true)
            } else {
                None
            };
            legend = at.is_some();
            head.strip_span(lo, at.map_or(b.end(), |(x0, _)| x0 - KEY_GAP), text.over[1]);
        }
        Some(0) => {
            head.time(b);
            head.time_over = text.over[1];
        }
        Some(_) => {
            head.time(a);
            head.time_over = text.over[0];
        }
    }
    if under && !legend {
        head.under_key = Some((text.machine.to_owned(), text.tab.to_owned()));
    }
    head.dotless = spilled.and_then(|i| plan.blocks.get(i)).map(|b| b.strip.x);
    head.finish()
}

#[cfg(test)]
#[path = "stats_plan_tests.rs"]
mod tests;
