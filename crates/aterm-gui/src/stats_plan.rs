// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Layout planner for the stats deck.
use aterm_vitals::MultiScaleSpec;
use aterm_vitals::words::TotalsForm;

use crate::deck_face::FaceMetrics;
use crate::stats_deck::{CORES_NAME, DECK_RESOURCES, FOLDED_PT, FULL_TABLE_PT};
use crate::stats_paint::Bar;
use crate::stats_units::{Form, PLAN_SCALE, UnitFacts, UnitRow};

/// Stats deck layout form.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum DeckForm {
    /// Two blocks of four rows in 62 pt.
    #[default]
    Folded,
    /// Header line and seven rows in full height, only where `stats_deck_size` is full.
    Full,
}

/// Configured `[sys] stats_deck_size` preference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum DeckSizePref {
    /// `Folded` whatever the window's height (the owner, 2026-10-24: the Full form had too
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

/// Returns the extra height in device pixels required by the full deck.
pub(crate) fn full_delta_px(scale: f64, cell_px: usize) -> usize {
    if !(scale.is_finite() && scale > 0.0) {
        return 0;
    }
    let need = ((FULL_TABLE_PT - FOLDED_PT) * scale).round() as usize;
    let cell = cell_px.max(1);
    need.div_ceil(cell) * cell
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
/// its dial's radius, pt. The dial is 54 pt across (Full's is 62) and sits 4 pt inside the
/// 62 pt band at its top and its foot, the largest the band holds with room for the bezel's
/// bloom.
pub(crate) const FOLD_GAUGE_R: f64 = 27.0;
/// The dial's centre, pt below the deck's top: the middle of the 62 pt band.
pub(crate) const FOLD_GAUGE_CY: f64 = 31.0;
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
/// row's name ([`DECK_RESOURCES`]) and the Cores row's while that row stands.
fn name_width(base: f64, pt: f64, m: &FaceMetrics) -> f64 {
    let widest = DECK_RESOURCES
        .map(aterm_vitals::words::resource_name)
        .into_iter()
        .chain([CORES_NAME])
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

/// One table's meter measures, pt (`docs/DESIGN-stats-deck-meter-2026-10-26.md` §2): how the
/// room right of the % column is shared between the strip, the scope bar and the figure run.
#[derive(Clone, Copy, Debug, PartialEq)]
struct MeterSpec {
    /// The strip's least and most width.
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
    /// Whether each row's bar may stand under its % where no bar column fits beside the strip.
    under: bool,
}

/// Full wide: six hours' strip from 239 pt taking 0.65 of the room, a bar of 40 to 240 pt, a run
/// at 10 pt, and Details where they leave the strip 360 pt.
const FULL_WIDE: MeterSpec = MeterSpec {
    strip: (239.0, 600.0),
    pitch: 5.0,
    share: 0.65,
    bar_gap: 8.0,
    bar: (40.0, 240.0),
    bar_alone: 0.0,
    run: 88.0,
    details: Some((DETAILS_MIN, 360.0)),
    under: false,
};

/// Full mid: no Details, a 4 pt pitch, and the strip at 0.6 of the room, which keeps the run
/// beside a bar as long as it down to about 700 pt.
const FULL_MID: MeterSpec = MeterSpec {
    pitch: 4.0,
    share: 0.6,
    details: None,
    ..FULL_WIDE
};

/// Full narrow: a strip from 99 pt at a 5 pt pitch.
const FULL_NARROW: MeterSpec = MeterSpec {
    strip: (99.0, 600.0),
    pitch: 5.0,
    bar_gap: 6.0,
    ..FULL_MID
};

/// Full slim: a strip from 79 pt at a 4 pt pitch, a run at 9.5 pt, and the bar under each %
/// where no column fits.
const FULL_SLIM: MeterSpec = MeterSpec {
    strip: (79.0, 600.0),
    pitch: 4.0,
    run: 72.0,
    under: true,
    ..FULL_NARROW
};

/// Folded wide: a strip from 129 pt taking eight tenths of the block's room (six tenths of the
/// block at the owner's 940 to 1040 pt), a bar of 32 to 160 pt, a run at 9.5 pt where the bar
/// is at least as long, and Details where they leave the strip 480 pt.
const FOLD_WIDE: MeterSpec = MeterSpec {
    strip: (129.0, 600.0),
    pitch: 5.0,
    share: 0.8,
    bar_gap: 6.0,
    bar: (32.0, 160.0),
    bar_alone: 0.0,
    run: 82.0,
    details: Some((FOLD_DETAILS_MIN, 480.0)),
    under: false,
};

/// Folded mid: a strip from 79 pt at a 4 pt pitch, a bar from 28 pt, no Details. The strip
/// keeps its share: a narrower block keeps a bar column by a shorter bar's least.
const FOLD_MID: MeterSpec = MeterSpec {
    strip: (79.0, 480.0),
    pitch: 4.0,
    bar: (28.0, 160.0),
    details: None,
    ..FOLD_WIDE
};

/// Folded narrow: a strip from 59 pt at a 3 pt pitch and a bar from 24 pt.
const FOLD_NARROW: MeterSpec = MeterSpec {
    strip: (59.0, 480.0),
    pitch: 3.0,
    bar: (24.0, 160.0),
    ..FOLD_MID
};

/// Folded slim: narrow's, with the bar under each % where no column fits.
const FOLD_SLIM: MeterSpec = MeterSpec {
    under: true,
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

/// How a table shares the room from `x`, where its strip starts, to `end` (`docs/DESIGN-stats-deck-
/// meter-2026-10-26.md` §2): the strip's boxes, the meter, and the Details column past it; `None`
/// where `spec`'s strip, or its bar where it has no place under the %, does not fit.
///
/// [`share_meter`] shares the room between the strip, the bar and the run. Details, where `spec`
/// lays it, takes [`DETAILS_SHARE`] of the room (its least to [`DETAILS_MAX`]) where the rest
/// still lays the run it would without them and `spec`'s strip beside Details: Details never cost
/// a row its maximum, nor the time series its length. What the strip's and the bar's most leave
/// goes to the run's column (up to [`RUN_MAX`]) or to Details (up to [`DETAILS_MAX`]). Where not
/// even the bar's least fits beside the strip's least, a table that may stands each row's bar
/// under its % and gives the strip the room.
fn lay_meter(x: f64, end: f64, spec: &MeterSpec) -> Option<(u32, Meter, Option<Col>)> {
    let room = end - x;
    let without = share_meter(room, spec);
    let details_w = spec.details.and_then(|(least, strip)| {
        let d = (room * DETAILS_SHARE).floor().clamp(least, DETAILS_MAX);
        let with = share_meter(room - d - DETAILS_GAP, spec)?;
        let run = |laid: Option<(u32, f64, f64)>| laid.map(|(_, run, _)| run);
        let keeps = f64::from(with.0) * spec.pitch - 1.0 >= strip;
        (keeps && run(Some(with)) >= run(without)).then_some(d)
    });
    let m = details_w.map_or(room, |d| room - d - DETAILS_GAP);
    let Some((cols, run_w, bar_w)) = share_meter(m, spec) else {
        if !spec.under {
            return None;
        }
        // The bar stands under each %, and the strip takes the room.
        let cols = strip_cols(spec, m);
        if cols < strip_cols(spec, spec.strip.0) {
            return None;
        }
        return Some((cols, Meter::default(), None));
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
    let mut cols = strip_cols(spec, (m * spec.share).floor()).max(strip_cols(spec, spec.strip.0));
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
    pub(crate) fn box_edges(self) -> Vec<(f64, f64)> {
        let slots = self.spec.slots();
        let gaps: f64 = slots
            .windows(2)
            .map(|w| gap_between(w[0].band, w[1].band))
            .sum();
        let room = (self.width() - gaps).max(0.0);
        let total = f64::from(self.spec.total_weight());
        let (mut before, mut weight) = (0.0, 0_u32);
        let mut edges = Vec::with_capacity(slots.len());
        for (j, slot) in slots.iter().enumerate() {
            if j > 0 {
                before += gap_between(slots[j - 1].band, slot.band);
            }
            let at = self.x + before;
            let left = at + room * f64::from(weight) / total;
            weight += u32::from(slot.weight);
            edges.push((left, at + room * f64::from(weight) / total));
        }
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
        let slots = self.spec.slots();
        let edges = self.box_edges();
        let n = slots.len().min(edges.len());
        (0..n).find_map(|j| {
            let slot = slots[j];
            if age > slot.age_hi_ms {
                return None;
            }
            let lo = if j == 0 {
                edges[0].0
            } else {
                (edges[j - 1].1 + edges[j].0) / 2.0
            };
            let hi = if j + 1 == n {
                edges[j].1
            } else {
                (edges[j].1 + edges[j + 1].0) / 2.0
            };
            let span = (slot.age_hi_ms - slot.age_lo_ms).max(1) as f64;
            let into = ((age - slot.age_lo_ms) as f64 / span).clamp(0.0, 1.0);
            Some(lo + (hi - lo) * into)
        })
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
}

impl Rows {
    const fn full(kind: FullKind) -> Self {
        let (pitch, cell, count, baseline, bar_h) = match kind {
            FullKind::Wide | FullKind::Mid => (12.0, 9.0, 7, 8.0, 6.0),
            FullKind::Narrow | FullKind::Slim => (10.5, 8.0, 8, 7.0, 5.0),
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
        }
    }

    const fn folded(kind: BlockKind) -> Self {
        Self {
            header_baseline: 9.5,
            top0: 14.0,
            pitch: 11.5,
            cell: 8.5,
            count: 4,
            baseline: 7.5,
            name_pt: match kind {
                BlockKind::Wide | BlockKind::Mid => 10.0,
                BlockKind::Narrow | BlockKind::Slim => 9.5,
            },
            amount_pt: 9.5,
            figure_pt: 9.5,
            bar_h: 5.0,
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
    /// Cores block, a strip of six hours at a 5 pt pitch, the meter, and Details.
    Wide,
    /// Cores block, a strip of six hours at a 4 pt pitch, and the meter.
    Mid,
    /// Cores row, a strip from 100 pt, the meter, and optional Details.
    Narrow,
    /// Cores row, a strip from 80 pt, and the meter, or the bar under each %.
    Slim,
}

impl FullKind {
    /// All full kinds from widest to narrowest.
    pub(crate) const ALL: [Self; 4] = [Self::Wide, Self::Mid, Self::Narrow, Self::Slim];
}

/// Placement of CPU cores in the `Full` deck form.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CoresAt {
    /// The slot left of the rows, past a divider: it holds the strain gauge (the refined deck,
    /// 2026-10-24), and a click in it opens the CPU lens, which keeps the per-core view.
    Block { at: Col, divider: f64 },
    /// Eighth row spanning from the strip to the right margin.
    Row(Col),
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
    pub(crate) cores: CoresAt,
    pub(crate) name: Col,
    pub(crate) well: Col,
    pub(crate) strip: Strip,
    /// Right-aligned percentage column, between the well and the strip.
    pub(crate) pct: Option<Col>,
    /// The scope bar and the figure run right of the strip; where the bar's column does not fit,
    /// each row draws its bar under its % ([`Self::bar_under`]).
    pub(crate) meter: Meter,
    pub(crate) details: Option<Col>,
}

impl FullPlan {
    /// The % column where no bar column fits: each row that prints a % draws its three-scope bar
    /// under the figure there (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §5).
    pub(crate) fn bar_under(&self) -> Option<Col> {
        self.pct.filter(|_| self.meter.bar.is_none())
    }

    /// Chooses the Cores row's [`CoresMode`] for `clusters`: narrow and slim have the row;
    /// wide and mid draw the strain gauge in [`CoresAt::Block`] and have none.
    pub(crate) fn cores_mode(
        &self,
        clusters: &[Cluster<'_>],
        m: &FaceMetrics,
    ) -> Option<CoresMode> {
        let CoresAt::Row(room) = self.cores else {
            return None;
        };
        Some(cores_row_mode(
            clusters,
            room.w,
            self.rows.cell,
            &[CoresMode::Stack, CoresMode::Tiny],
            CORES_CAPTION_PT,
            m,
        ))
    }

    fn shift(&mut self, dx: f64) {
        self.cores = match self.cores {
            CoresAt::Block { at, divider } => CoresAt::Block {
                at: Col::new(at.x + dx, at.w),
                divider: divider + dx,
            },
            CoresAt::Row(room) => CoresAt::Row(Col::new(room.x + dx, room.w)),
        };
        self.name.x += dx;
        self.well.x += dx;
        self.strip.x += dx;
        self.meter.shift(dx);
        for col in [&mut self.pct, &mut self.details].into_iter().flatten() {
            col.x += dx;
        }
    }
}

/// Computes the [`FullPlan`] for deck width `w_pt`.
pub(crate) fn plan_full(w_pt: f64, m: &FaceMetrics) -> FullPlan {
    let w = whole_width(w_pt);
    if let Some(plan) = FullKind::ALL
        .into_iter()
        .map(|kind| lay_full(w, kind, m, true))
        .find_map(|(plan, fits)| fits.then_some(plan))
    {
        return plan;
    }
    let right = w - X0;
    // Under slim's width the % column goes before the strip does: the strip takes its place.
    let (mut plan, _) = lay_full(w, FullKind::Slim, m, true);
    if plan.strip.end() > right {
        plan = lay_full(w, FullKind::Slim, m, false).0;
    }
    plan.pct = plan.pct.filter(|pct| pct.end() <= right);
    plan.ground_only = w < MIN_TABLE_W || plan.strip.end() > right;
    plan
}

/// The Full plan of `kind` at `w`, and whether it fits; `pct` lays the % column between the well
/// and the strip, and without it the strip stands where the column would.
fn lay_full(w: f64, kind: FullKind, m: &FaceMetrics, pct: bool) -> (FullPlan, bool) {
    let right = w - X0;
    let rows = Rows::full(kind);
    let (name_x, name_base) = match kind {
        FullKind::Wide | FullKind::Mid => (97.0, 56.0),
        FullKind::Narrow => (X0, 52.0),
        FullKind::Slim => (X0, 48.0),
    };
    let name = Col::new(name_x, name_width(name_base, rows.name_pt, m));
    let well = Col::new(name.end(), 10.0);
    // The % column (the value now) and then the strip, which reads outward from now.
    let pct_col = match kind {
        FullKind::Wide | FullKind::Mid | FullKind::Narrow => Col::new(well.end() + 3.0, 30.0),
        FullKind::Slim => Col::new(well.end(), 26.0),
    };
    let strip_x = if pct {
        pct_col.end() + PCT_TO_STRIP
    } else {
        well.end() + BARE_STRIP_GAP
    };
    let spec = match kind {
        FullKind::Wide => FULL_WIDE,
        FullKind::Mid => FULL_MID,
        FullKind::Narrow => FULL_NARROW,
        FullKind::Slim => FULL_SLIM,
    };
    // The legend stands over the meter's own columns, so a bar with no run is as long as it.
    let spec = MeterSpec {
        bar_alone: legend_least(m),
        ..spec
    };
    let cores = match kind {
        FullKind::Wide | FullKind::Mid => CoresAt::Block {
            at: Col::new(X0, CORES_BLOCK_W),
            divider: 88.0,
        },
        FullKind::Narrow | FullKind::Slim => {
            CoresAt::Row(Col::new(pct_col.x, (right - pct_col.x).max(0.0)))
        }
    };
    let mut plan = FullPlan {
        w,
        kind,
        ground_only: false,
        offset: 0.0,
        rows,
        cores,
        name,
        well,
        strip: Strip::full(strip_x, strip_cols(&spec, spec.strip.0), spec.pitch),
        pct: pct.then_some(pct_col),
        meter: Meter::default(),
        details: None,
    };
    // Wide needs its Details; slim, the last kind, takes whatever fits.
    let laid = lay_meter(strip_x, right, &spec)
        .filter(|(_, _, details)| kind != FullKind::Wide || details.is_some());
    let Some((cols, meter, details)) = laid else {
        return (plan, false);
    };
    plan.strip = Strip::full(strip_x, cols, spec.pitch);
    plan.meter = meter;
    plan.details = details;
    // What the widest strip, bar and Details leave centres the table.
    let laid_end = details
        .or(meter.run)
        .or(meter.bar)
        .map_or(plan.strip.end(), Col::end);
    let extra = right - laid_end;
    if kind == FullKind::Wide && extra > 0.0 {
        plan.offset = (extra / 2.0).floor();
        plan.shift(plan.offset);
    }
    (plan, plan.strip.end() <= right)
}

/// Width tier for each block of the `Folded` deck form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum BlockKind {
    /// A strip from 160 pt at a 5 pt pitch, the meter, and Details where room is left.
    Wide,
    /// A strip from 120 pt at a 4 pt pitch, and the meter.
    Mid,
    /// A strip from 60 pt at a 3 pt pitch, and the meter.
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
    /// Block A's units column, between the well and the %, where any row draws units; block B
    /// has none (A1 ruling 15).
    pub(crate) units: Option<UnitsCol>,
}

/// Block A's units column (`docs/DESIGN-stats-deck-units-2026-11-04.md` §2.4): where the % column
/// stood without it, [`UNITS_TO_PCT`] short of the % column, and as wide as the widest form its
/// rows take at 1×. Each form stands right-aligned in it, against its row's %.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct UnitsCol {
    pub(crate) at: Col,
    /// U_cap: the widest column the block holds without its strip losing the top preset
    /// ([`units_cap`]).
    pub(crate) cap: f64,
    /// Each row's form, in [`UnitRow::ALL`]'s order; `None` draws no units on its row.
    pub(crate) forms: [Option<Form>; 4],
}

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
}

impl FoldedGauge {
    /// The dial's centre, pt.
    pub(crate) fn centre(&self) -> (f64, f64) {
        (self.at.x + self.at.w / 2.0, FOLD_GAUGE_CY)
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

/// Computes the [`FoldedPlan`] for deck width `w_pt` with no units column, the plan the deck
/// paints until A1's step R6 paints the units and plans with the machine's [`UnitFacts`]
/// ([`plan_folded_with`]).
pub(crate) fn plan_folded(w_pt: f64, m: &FaceMetrics) -> FoldedPlan {
    plan_folded_with(w_pt, m, &UnitFacts::NONE)
}

/// Computes the [`FoldedPlan`] for deck width `w_pt`. The gauge comes first: the blocks take the
/// widest kind that fits beside it, and only where not even slim fits beside it (under about
/// 417 pt) does the deck draw no gauge and give the blocks its room. Block A then takes the
/// units column `units` measures where it costs the strip nothing of its horizon
/// ([`plan_units`]).
pub(crate) fn plan_folded_with(w_pt: f64, m: &FaceMetrics, units: &UnitFacts<'_>) -> FoldedPlan {
    let w = whole_width(w_pt);
    match lay_folded(w, true, m, units) {
        (plan, true) => plan,
        (_, false) => lay_folded(w, false, m, units).0,
    }
}

/// The Folded plan at `w` with the gauge or without it, and whether some block kind fits.
fn lay_folded(w: f64, gauge: bool, m: &FaceMetrics, units: &UnitFacts<'_>) -> (FoldedPlan, bool) {
    let g = if gauge { FOLD_GAUGE_W } else { 0.0 };
    let mut b = ((w - 2.0 * X0 - BLOCK_GUTTER - g) / 2.0).floor().max(0.0);
    let mut xa = X0 + g;
    if b > B_MAX {
        b = B_MAX;
        xa = X0 + g + ((w - 2.0 * X0 - (g + 2.0 * b + BLOCK_GUTTER)) / 2.0).floor();
    }
    let kind = BlockKind::ALL
        .into_iter()
        .find(|&kind| lay_block(xa, b, kind, m, true, 0.0).1);
    // Where not even slim fits, the % column goes and the strip takes its place.
    let block = |x: f64| match kind {
        Some(kind) => lay_block(x, b, kind, m, true, 0.0).0,
        None => lay_block(x, b, BlockKind::Slim, m, false, 0.0).0,
    };
    let mut blocks = [block(xa), block(xa + b + BLOCK_GUTTER)];
    // Block A's kind and horizon are the ones it has with no units column.
    if let Some(kind) = kind
        && let Some(col) = plan_units(&blocks[0], units)
    {
        blocks[0] = BlockPlan {
            units: Some(col),
            ..lay_block(xa, b, kind, m, true, col.at.w).0
        };
    }
    let plan = FoldedPlan {
        w,
        ground_only: w < MIN_TABLE_W || blocks.iter().any(|blk| blk.strip.end() > blk.at.end()),
        divider: xa + b + 14.0,
        gauge: gauge.then(|| FoldedGauge {
            at: Col::new(xa - g, 2.0 * FOLD_GAUGE_R),
            divider: xa - FOLD_GAUGE_GAP,
        }),
        blocks,
    };
    (plan, kind.is_some())
}

/// A Folded block kind's meter measures.
const fn fold_spec(kind: BlockKind) -> MeterSpec {
    match kind {
        BlockKind::Wide => FOLD_WIDE,
        BlockKind::Mid => FOLD_MID,
        BlockKind::Narrow => FOLD_NARROW,
        BlockKind::Slim => FOLD_SLIM,
    }
}

/// One Folded block of `kind` at `x`, `b` wide, and whether it fits; `pct` lays the % column
/// between the well and the strip, and without it the strip stands where the column would. A
/// units column `u` pt wide (0 for none) stands where the % column would, and the % column
/// [`UNITS_TO_PCT`] past it.
fn lay_block(
    x: f64,
    b: f64,
    kind: BlockKind,
    m: &FaceMetrics,
    pct: bool,
    u: f64,
) -> (BlockPlan, bool) {
    let rows = Rows::folded(kind);
    let (name_base, well_w, pct_gap, pct_w) = match kind {
        BlockKind::Wide => (52.0, 10.0, 2.0, 30.0),
        BlockKind::Mid => (52.0, 10.0, 2.0, 28.0),
        BlockKind::Narrow | BlockKind::Slim => (48.0, 7.0, 0.0, 26.0),
    };
    let spec = fold_spec(kind);
    let name = Col::new(x, name_width(name_base, rows.name_pt, m));
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
        strip: Strip::folded(strip_x, strip_cols(&spec, spec.strip.0), spec.pitch),
        pct: pct.then_some(pct_col),
        meter: Meter::default(),
        details: None,
        cores: Col::new(cores_x, (end - cores_x).max(0.0)),
        units: None,
    };
    // Slim, the last kind, stands the bar under each % where no column fits; the block fits
    // where its strip does.
    match lay_meter(strip_x, end, &spec) {
        Some((cols, meter, details)) => {
            plan.strip = Strip::folded(strip_x, cols, spec.pitch);
            plan.meter = meter;
            plan.details = details;
            (plan, plan.strip.end() <= end)
        }
        None => (plan, false),
    }
}

/// Block A's units (A1 §2.4): each row takes the first form of its ladder that `units` draws
/// within U_cap at 1× ([`units_cap`]), and the column is as wide as the widest of them; `None`
/// where no row takes one. `bare` is the block laid with none, whose kind and horizon the
/// column keeps.
fn plan_units(bare: &BlockPlan, units: &UnitFacts<'_>) -> Option<UnitsCol> {
    let cell = bare.rows.cell;
    if !units.draw_any(cell) {
        return None;
    }
    let at = bare.pct?.x;
    let cap = units_cap(bare)?;
    let forms = units.forms_within(cap, cell);
    let w = UnitRow::ALL
        .into_iter()
        .zip(forms)
        .filter_map(|(row, form)| units.width(row, form?, PLAN_SCALE, cell))
        .fold(0.0, f64::max);
    (w > 0.0).then_some(UnitsCol {
        at: Col::new(at, w),
        cap,
        forms,
    })
}

/// U_cap (A1 §2.4): the widest whole-point units column block A holds while its strip keeps the
/// top preset, six hours, beside a bar column, given `bare`, the block laid with none; `None`
/// where `bare`'s strip holds a shorter preset, or the top preset leaves not even the column's
/// gap to the %.
///
/// Only the top preset. A strip that holds a shorter one leaves room beside it (122 pt at 924 pt
/// on Helvetica Neue), but the first widths whose strip holds the top preset leave less than any
/// form (−3 pt at 925 pt), so a form drawn below them would go again as the deck widened. In the
/// top preset's room the cap grows with the block, so no row's form steps down as the deck
/// widens.
fn units_cap(bare: &BlockPlan) -> Option<f64> {
    let top = Strip::spec_for(f64::INFINITY);
    let spec = fold_spec(bare.kind);
    // What the units column and the meter share: the bare strip's room, less the gap to the %.
    let room = bare.at.end() - bare.strip.x - UNITS_TO_PCT;
    let keeps = |u: f64| {
        share_meter(room - u, &spec)
            .is_some_and(|(cols, _, _)| Strip::spec_for(f64::from(cols) * spec.pitch - 1.0) == top)
    };
    if bare.strip.spec != top || !keeps(0.0) {
        return None;
    }
    // Less room never lays more boxes, and room that holds the top preset leaves the bar more
    // than its least, so `keeps` holds up to the cap and past it nowhere.
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
    let full = FullKind::ALL.map(|kind| first(&|w| lay_full(w, kind, m, true).1));
    let block = BlockKind::ALL.map(|kind| first(&|b| lay_block(X0, b, kind, m, true, 0.0).1));
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

/// Returns the width required by a Cores row in `mode`, or `f64::INFINITY` if it cannot fit.
pub(crate) fn cores_row_width(
    clusters: &[Cluster<'_>],
    mode: CoresMode,
    cell: f64,
    caption_pt: f64,
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
            each.sum::<f64>() + 8.0 * clusters.len().saturating_sub(1) as f64
        }
    }
}

/// Chooses the first [`CoresMode`] in `ladder` that fits `room`, or `CountOnly`.
pub(crate) fn cores_row_mode(
    clusters: &[Cluster<'_>],
    room: f64,
    cell: f64,
    ladder: &[CoresMode],
    caption_pt: f64,
    m: &FaceMetrics,
) -> CoresMode {
    if total_cores(clusters) == 0 {
        return CoresMode::CountOnly;
    }
    ladder
        .iter()
        .copied()
        .find(|&mode| cores_row_width(clusters, mode, cell, caption_pt, m) <= room)
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
    /// the row is built around (the owner, 2026-10-26) and the ramp gives way. The test is the
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
            if let CoresAt::Row(_) = plan.cores {
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
mod tests {
    use super::*;
    use crate::stats_units::Gpu;

    const TEXT: HeaderText<'static> = HeaderText {
        tab: "Tab 2",
        machine: "this Mac",
        over: [true; 2],
        totals: &[],
    };

    fn hn() -> FaceMetrics {
        FaceMetrics::helvetica_neue()
    }

    /// Totals forms that measure as they read: figures that never change.
    fn said_totals(forms: &[&str]) -> Vec<TotalsForm> {
        forms
            .iter()
            .map(|&f| TotalsForm {
                said: f.to_owned(),
                widest: f.to_owned(),
                used: false,
            })
            .collect()
    }

    fn px(v: f64, s: f64) -> f64 {
        (v * s + 0.5).floor()
    }

    fn strip_col(strip: Strip) -> Col {
        Col::new(strip.x, strip.end() - strip.x)
    }

    fn full_cols(plan: &FullPlan) -> Vec<Col> {
        let mut cols = Vec::new();
        if let CoresAt::Block { at, divider } = plan.cores {
            cols.extend([at, Col::new(divider, 0.0)]);
        }
        cols.extend([plan.name, plan.well]);
        cols.extend(plan.pct);
        cols.push(strip_col(plan.strip));
        cols.extend(plan.meter.cols());
        cols.extend(plan.details);
        cols
    }

    fn block_cols(block: &BlockPlan) -> Vec<Col> {
        let mut cols = vec![block.name, block.well];
        cols.extend(block.units.map(|units| units.at));
        cols.extend(block.pct);
        cols.push(strip_col(block.strip));
        cols.extend(block.meter.cols());
        cols.extend(block.details);
        cols
    }

    fn assert_in_order(cols: &[Col], x0: f64, x1: f64, what: &str) {
        let mut edge = x0;
        for col in cols {
            assert!(
                col.w >= 0.0 && col.x >= edge,
                "{what}: {col:?} starts before {edge}"
            );
            edge = col.end();
        }
        assert!(edge <= x1, "{what}: the columns end at {edge}, past {x1}");
    }

    fn assert_on_pixels(cols: &[Col], what: &str) {
        for s in [1.0, 2.0] {
            for edge in cols.iter().flat_map(|col| [col.x, col.end()]) {
                assert_eq!(edge * s, px(edge, s), "{what}: edge {edge} at {s}×");
            }
        }
    }

    /// A Full plan's strip and meter: the strip's (x, cols, pitch), then its bar and run.
    fn full_meter(p: &FullPlan) -> ((f64, u32, f64), Option<Col>, Option<Col>) {
        (
            (p.strip.x, p.strip.cols, p.strip.pitch),
            p.meter.bar,
            p.meter.run,
        )
    }

    /// The strip takes the row (`docs/DESIGN-stats-deck-meter-2026-10-26.md` §2): from the
    /// strip's own start, the bar `bar_gap` past its end, the run [`RUN_GAP`] past the bar's,
    /// then Details, every edge on whole device px.
    #[test]
    fn plan_golden_tables_full() {
        let m = hn();
        let at = |w: f64| {
            let plan = plan_full(w, &m);
            assert_eq!(plan_full((2.0 * w + 1.0) / 2.0, &m), plan);
            assert_on_pixels(&full_cols(&plan), &format!("W {w}"));
            plan
        };
        let col = |x: f64, w: f64| Some(Col::new(x, w));

        // The owner's window: six hours' strip of 429 pt, a bar of 134 and the run with each
        // row's maximum.
        let p = at(1039.0);
        assert_eq!((p.kind, p.offset), (FullKind::Wide, 0.0));
        assert_eq!((p.name, p.pct), (Col::new(97.0, 56.0), col(166.0, 30.0)));
        assert_eq!(
            full_meter(&p),
            ((203.0, 86, 5.0), col(641.0, 134.0), col(781.0, 88.0))
        );
        assert_eq!((p.strip.end(), p.strip.span_s()), (632.0, 21_600));
        assert_eq!(p.details, col(879.0, 148.0));
        assert_eq!(
            p.cores,
            CoresAt::Block {
                at: Col::new(12.0, 68.0),
                divider: 88.0
            }
        );
        assert_eq!([2.0, 1.0].map(|s| px(p.strip.x, s)), [406.0, 203.0]);
        assert_eq!([2.0, 1.0].map(|s| px(p.strip.cell, s)), [8.0, 4.0]);

        let p = at(1424.0);
        assert_eq!(
            full_meter(&p),
            ((203.0, 120, 5.0), col(811.0, 240.0), col(1057.0, 88.0))
        );
        assert_eq!(p.details, col(1155.0, 257.0));
        // Past the strip's and the bar's most, the table centres and Details take the rest.
        let p = at(2560.0);
        assert_eq!((p.offset, p.strip.x, p.strip.cols), (416.0, 619.0, 120));
        assert_eq!(p.details, col(1571.0, 560.0));

        let p = at(940.0);
        assert_eq!(
            full_meter(&p),
            ((203.0, 76, 5.0), col(591.0, 103.0), col(700.0, 88.0))
        );
        assert_eq!(p.details, col(798.0, 130.0));
        // Mid lays no Details: the strip takes their room at a finer pitch.
        let p = at(900.0);
        assert_eq!(p.kind, FullKind::Mid);
        assert_eq!(
            full_meter(&p),
            ((203.0, 103, 4.0), col(623.0, 171.0), col(800.0, 88.0))
        );
        assert_eq!(p.details, None);
        let p = at(700.0);
        assert_eq!(
            full_meter(&p),
            ((203.0, 73, 4.0), col(503.0, 91.0), col(600.0, 88.0))
        );
        // Narrowing, the run gives way first, then the bar (as long as its legend at least),
        // and the strip, which keeps its share, last.
        let p = at(550.0);
        assert_eq!(
            (p.kind, p.name, p.pct),
            (FullKind::Narrow, Col::new(12.0, 52.0), col(77.0, 30.0))
        );
        assert_eq!(full_meter(&p), ((114.0, 51, 5.0), col(375.0, 163.0), None));
        assert_eq!(p.strip.span_s(), 21_600);
        let p = at(450.0);
        assert_eq!(full_meter(&p), ((114.0, 39, 5.0), col(315.0, 123.0), None));
        assert_eq!(p.cores, CoresAt::Row(Col::new(77.0, 361.0)));
        // Where no bar as long as its legend fits, it stands under each % and the strip takes
        // the room: four hours at 350 pt.
        let p = at(350.0);
        assert_eq!((p.kind, p.meter), (FullKind::Slim, Meter::default()));
        assert_eq!(
            (p.strip.x, p.strip.cols, p.strip.span_s()),
            (103.0, 59, 14_400)
        );
        assert!(p.bar_under().is_some());
        let p = at(210.0);
        assert_eq!((p.kind, p.meter), (FullKind::Slim, Meter::default()));
        assert!(p.bar_under().is_some());
    }

    /// Folded's blocks as Full's rows: the strip, the bar past it and the run past that, inside
    /// each block; at the owner's widths the strip is over half its block.
    #[test]
    fn plan_golden_tables_folded() {
        let m = hn();
        let at = |w: f64| {
            let plan = plan_folded(w, &m);
            assert_eq!(plan_folded((2.0 * w + 1.0) / 2.0, &m), plan);
            for block in &plan.blocks {
                assert_on_pixels(&block_cols(block), &format!("W {w}"));
            }
            plan
        };
        let col = |x: f64, w: f64| Some(Col::new(x, w));
        let block_meter = |b: &BlockPlan| {
            (
                (b.strip.x, b.strip.cols, b.strip.pitch),
                b.meter.bar,
                b.meter.run,
            )
        };

        let p = at(1039.0);
        assert_eq!(
            p.gauge,
            Some(FoldedGauge {
                at: Col::new(12.0, 54.0),
                divider: 75.0
            })
        );
        assert_eq!(
            p.blocks.map(|b| (b.kind, b.at)),
            [
                (BlockKind::Wide, Col::new(84.0, 457.0)),
                (BlockKind::Wide, Col::new(570.0, 457.0))
            ]
        );
        let [a, b] = p.blocks;
        assert_eq!((a.name, a.pct), (Col::new(84.0, 52.0), col(148.0, 30.0)));
        // Six hours' strip of 284 pt and the bar to the block's end: no run, whose room the
        // strip keeps.
        assert_eq!(block_meter(&a), ((185.0, 57, 5.0), col(476.0, 65.0), None));
        assert_eq!(
            (a.strip.end(), a.strip.span_s(), a.details),
            (469.0, 21_600, None)
        );
        assert_eq!(block_meter(&b), ((671.0, 57, 5.0), col(962.0, 65.0), None));

        let a = at(940.0).blocks[0];
        assert_eq!(a.at, Col::new(84.0, 407.0));
        assert_eq!(block_meter(&a), ((185.0, 49, 5.0), col(436.0, 55.0), None));
        let a = at(1424.0).blocks[0];
        assert_eq!(block_meter(&a), ((185.0, 87, 5.0), col(626.0, 107.0), None));
        // A run where the bar is as long as it, and Details where they leave the strip 480 pt.
        let a = at(2560.0).blocks[0];
        assert_eq!(
            block_meter(&a),
            ((185.0, 120, 5.0), col(791.0, 160.0), col(957.0, 82.0))
        );
        assert_eq!(a.details, col(1049.0, 252.0));
        // Narrower blocks keep the strip's share: a shorter bar, a finer pitch, then the bar
        // under the %.
        let a = at(750.0).blocks[0];
        assert_eq!(
            (a.kind, block_meter(&a)),
            (BlockKind::Wide, ((185.0, 33, 5.0), col(356.0, 40.0), None))
        );
        let a = at(650.0).blocks[0];
        assert_eq!(
            (a.kind, block_meter(&a)),
            (BlockKind::Mid, ((183.0, 32, 4.0), col(317.0, 29.0), None))
        );
        let a = at(600.0).blocks[0];
        assert_eq!((a.kind, a.meter), (BlockKind::Slim, Meter::default()));
        assert_eq!(
            (a.strip.x, a.strip.cols, a.strip.span_s()),
            (172.0, 50, 14_400)
        );
        assert!(a.bar_under().is_some());
        let a = at(450.0).blocks[0];
        assert_eq!((a.kind, a.meter), (BlockKind::Slim, Meter::default()));
        assert!(a.bar_under().is_some());
        let p = at(350.0);
        assert_eq!(
            (p.gauge, p.blocks[0].strip.x, p.blocks[0].strip.cols),
            (None, 100.0, 20)
        );

        // The owner's widths (2026-10-26, about 940 to 1040 pt): each strip is well over half
        // its block (six tenths, to a box), reaches back six hours, and is the widest thing on
        // its row; the bar takes the rest of the block.
        for w in (940..=1040).map(f64::from) {
            for b in at(w).blocks {
                assert_eq!(b.strip.span_s(), 21_600, "W {w}");
                assert_eq!(
                    b.meter.bar.map(Col::end),
                    Some(b.at.end()),
                    "W {w}: the bar ends the block"
                );
                assert!(
                    b.strip.width() >= 0.6 * b.at.w - b.strip.pitch,
                    "W {w}: a strip of {} in a block of {}",
                    b.strip.width(),
                    b.at.w
                );
                assert_widest(b.strip, &b.meter, b.details, &format!("W {w}"));
            }
        }
    }

    #[test]
    fn rows_sit_where_the_vertical_tables_say() {
        let m = hn();
        let tops = |rows: &Rows, s: f64| {
            (0..rows.count)
                .map(|r| px(rows.top(r), s))
                .collect::<Vec<_>>()
        };

        let wide = plan_full(1119.0, &m).rows;
        assert_eq!(
            (wide.header_baseline, wide.count, wide.cell, wide.baseline),
            (11.5, 7, 9.0, 8.0)
        );
        assert_eq!(
            tops(&wide, 2.0),
            [32.0, 56.0, 80.0, 104.0, 128.0, 152.0, 176.0]
        );
        assert_eq!([2.0, 1.0].map(|s| px(wide.cell, s)), [18.0, 9.0]);
        assert_eq!(
            (wide.name_pt, wide.amount_pt, wide.figure_pt, wide.bar_h),
            (10.5, 10.0, 10.0, 6.0)
        );

        let narrow = plan_full(400.0, &m).rows;
        assert_eq!(
            (narrow.count, narrow.pitch, narrow.cell, narrow.baseline),
            (8, 10.5, 8.0, 7.0)
        );
        assert_eq!(
            (narrow.name_pt, narrow.amount_pt, narrow.figure_pt),
            (10.0, 10.0, 10.0)
        );
        let slim = plan_full(250.0, &m).rows;
        assert_eq!((slim.name_pt, slim.amount_pt, slim.bar_h), (9.5, 9.5, 5.0));

        let folded = plan_folded(1119.0, &m).blocks[0].rows;
        assert_eq!(
            (folded.header_baseline, folded.count, folded.baseline),
            (9.5, 4, 7.5)
        );
        assert_eq!(tops(&folded, 2.0), [28.0, 51.0, 74.0, 97.0]);
        assert_eq!(tops(&folded, 1.0), [14.0, 26.0, 37.0, 49.0]);
        assert_eq!([2.0, 1.0].map(|s| px(folded.cell, s)), [17.0, 9.0]);
        assert_eq!(
            (
                folded.name_pt,
                folded.amount_pt,
                folded.figure_pt,
                folded.bar_h
            ),
            (10.0, 9.5, 9.5, 5.0)
        );
        assert_eq!(plan_folded(350.0, &m).blocks[0].rows.name_pt, 9.5);
    }

    /// The bar under a % figure stands where the now-left design's table puts it
    /// (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §5): from a point below the digits'
    /// baseline, 2 pt tall, clear of the next row's top by a point in Folded and half a point in
    /// Full narrow and slim, so it costs the deck no height.
    #[test]
    fn the_bar_under_the_percent_stays_inside_the_row_pitch() {
        let m = hn();
        for (what, rows, want, clear) in [
            (
                "Folded",
                plan_folded(350.0, &m).blocks[0].rows,
                (8.5, 10.5),
                1.0,
            ),
            ("Full slim", plan_full(210.0, &m).rows, (8.0, 10.0), 0.5),
            ("Full narrow", plan_full(400.0, &m).rows, (8.0, 10.0), 0.5),
            ("Full wide", plan_full(1119.0, &m).rows, (9.0, 11.0), 1.0),
        ] {
            for r in 0..rows.count {
                let (y0, y1) = rows.under(r);
                let top = rows.top(r);
                assert_eq!((y0 - top, y1 - top), want, "{what} row {r}");
                assert_eq!(rows.top(r + 1) - y1, clear, "{what} row {r}");
                assert_eq!(y0 - (top + rows.baseline), UNDER_GAP, "{what} row {r}");
            }
        }
    }

    /// Every row that prints a % keys its three-scope bar
    /// (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §5): in a column right of the strip in
    /// Full wide, mid and narrow, in fwide, fmid and fnarrow, and in Full slim with room left,
    /// else under the % figure (fslim's always), never both, on both faces at every width. The
    /// legend names the three scopes once, whole, or not at all.
    #[test]
    fn every_percent_has_its_bar_at_every_width() {
        let scopes = |head: &HeaderPlan| {
            head.labels
                .iter()
                .filter(|l| matches!(l.label, Label::Scope(_)))
                .count()
        };
        let faces = [hn(), FaceMetrics::test_stack()];
        let widths: Vec<(usize, u32)> = (0..faces.len())
            .flat_map(|face| (160..=5120).map(move |w| (face, w)))
            .collect();
        // Bars under the % seen in Full and in fslim, and Full slim's column.
        let seen = aterm_spec::check::each(&widths, |&(face, w)| {
            let (m, w) = (&faces[face], f64::from(w));
            let mut seen = [0_u32; 3];
            let full = plan_full(w, m);
            if !full.ground_only && full.pct.is_some() {
                let at = format!("Full {:?} W {w}", full.kind);
                let bar = full.meter.bar;
                assert!(bar.is_some() != full.bar_under().is_some(), "{at}");
                match (full.kind, bar) {
                    (FullKind::Slim, None) => seen[0] += 1,
                    (FullKind::Slim, Some(_)) => seen[2] += 1,
                    (_, bar) => assert!(bar.is_some(), "{at}: its column"),
                }
                let n = scopes(&header_full(&full, TEXT, None, m));
                assert!(n == 0 || n == 3, "{at}: the legend in part, {n} names");
            }
            let folded = plan_folded(w, m);
            if folded.ground_only {
                return seen;
            }
            for block in folded.blocks.iter().filter(|b| b.pct.is_some()) {
                let at = format!("Folded {:?} W {w}", block.kind);
                let bar = block.meter.bar;
                assert!(bar.is_some() != block.bar_under().is_some(), "{at}");
                match (block.kind, bar) {
                    (BlockKind::Slim, bar) => {
                        assert_eq!(bar, None, "{at}: fnarrow lays slim's column first");
                        seen[1] += 1;
                    }
                    (_, bar) => assert!(bar.is_some(), "{at}: its column"),
                }
            }
            let n = scopes(&header_folded(&folded, TEXT, [None, None], m));
            assert!(n == 0 || n == 3, "Folded W {w}: the legend once, {n} names");
            seen
        })
        .into_iter()
        .fold([0_u32; 3], |mut all, one| {
            all.iter_mut().zip(one).for_each(|(n, more)| *n += more);
            all
        });
        assert!(seen.iter().all(|&n| n > 10), "CONTROL: {seen:?}");
    }

    /// Where the bars stand under the % figures, the scope key (`this Mac ■ aterm ■ Tab`) takes
    /// the leftmost room over no strip that the scale and rule K's items leave, whole (the
    /// machine's last word standing for "this Mac" where only that fits), or none
    /// (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §5): it never costs the scale a tick or
    /// the key an item. Where bars stand under the % the scopes' heads (over their figures or
    /// shares) usually key them, so the header here is planned over rooms of its own.
    #[test]
    fn the_key_of_bars_under_the_percent_comes_after_the_scale() {
        let m = hn();
        // A minute's strip, whose header holds its label and a tick but not "over max".
        let strip = plan_folded(350.0, &m).blocks[0].strip;
        let words = |s: &str| m.width(s, HEADER_PT, false);
        let need =
            3.0 * SWATCH_ADVANCE + words("this Mac") + words("aterm") + words("Tab 2") + 16.0;
        let noun_need = need - words("this Mac") + words("Mac");
        let over = KeyItem::Over.width(&m) + KEY_GAP;
        let x0 = strip.end() + 20.0;
        let plan = |keys: &'static [KeyItem], room: f64, strip_room: bool, under: bool| {
            let mut head = Header::new(&m);
            head.keys = keys;
            head.time(strip);
            head.span(x0, x0 + room, strip_room);
            if under {
                head.under_key = Some(("this Mac".to_owned(), "Tab 2".to_owned()));
            }
            head.finish()
        };
        let names = |head: &HeaderPlan| -> Vec<(f64, String)> {
            head.labels
                .iter()
                .filter(|l| matches!(l.label, Label::Scope(_)))
                .map(|l| (l.x, l.text.clone()))
                .collect()
        };
        let rest = |head: &HeaderPlan| {
            let labels: Vec<HeaderLabel> = head
                .labels
                .iter()
                .filter(|l| !matches!(l.label, Label::Scope(_)))
                .cloned()
                .collect();
            (labels, head.key.clone(), head.ruler.clone())
        };
        let firsts = |head: &HeaderPlan| names(head).first().map(|n| n.0);
        let shown: &'static [KeyItem] = &[KeyItem::Over];
        for keys in [shown, &[]] {
            for room in [need - 0.5, need, over + need - 0.5, over + need, 400.0] {
                for strip_room in [false, true] {
                    let with = plan(keys, room, strip_room, true);
                    let at = format!("{keys:?}, {room} pt, over a strip {strip_room}");
                    assert_eq!(
                        rest(&with),
                        rest(&plan(keys, room, strip_room, false)),
                        "{at}: the key costs the scale and rule K nothing"
                    );
                    let said = names(&with);
                    assert!(said.is_empty() || said.len() == 3, "{at}: {said:?}");
                    if strip_room {
                        assert_eq!(said, [], "{at}: the key stands over no strip");
                    }
                }
            }
        }
        // "over max" takes the room's start (the strip's own header has none for it), and the
        // key the rest, when it holds the key's need.
        assert_eq!(
            placed(&plan(shown, 400.0, false, true), KeyItem::Over),
            Some(x0)
        );
        assert_eq!(
            firsts(&plan(shown, over + need, false, true)),
            Some(x0 + over)
        );
        let noun = |head: &HeaderPlan| names(head).first().map(|n| n.1.clone());
        assert_eq!(
            noun(&plan(shown, over + need - 0.5, false, true)).as_deref(),
            Some("Mac")
        );
        assert_eq!(
            firsts(&plan(shown, over + noun_need - 0.5, false, true)),
            None
        );
        assert_eq!(firsts(&plan(&[], need, false, true)), Some(x0));
        assert_eq!(
            noun(&plan(&[], need - 0.5, false, true)).as_deref(),
            Some("Mac")
        );
        assert_eq!(firsts(&plan(&[], noun_need - 0.5, false, true)), None);
        assert_eq!(
            names(&plan(shown, 400.0, false, true))
                .into_iter()
                .map(|n| n.1)
                .collect::<Vec<_>>(),
            ["this Mac", "aterm", "Tab 2"]
        );
    }

    #[test]
    fn breakpoints_follow_the_face() {
        let helvetica = breakpoints(&hn());
        assert_eq!(helvetica.full, [909.0, 565.0, 389.0, 194.0]);
        assert_eq!(helvetica.block, [274.0, 257.0, 229.0, 147.0]);
        assert_eq!(helvetica.folded, [673.0, 639.0, 583.0, 419.0]);
        assert_eq!(helvetica.folded_bare, 347.0);

        let stack = FaceMetrics::test_stack();
        assert_eq!(plan_full(1119.0, &stack).name.w, 59.0);
        assert_eq!(plan_full(580.0, &stack).name.w, 59.0);
        assert_eq!(plan_full(450.0, &stack).name.w, 56.0);
        let dejavu = breakpoints(&stack);
        assert_eq!(dejavu.full, [912.0, 572.0, 402.0, 199.0]);
        assert_eq!(dejavu.block, [278.0, 261.0, 234.0, 152.0]);
        assert_eq!(dejavu.folded, [681.0, 647.0, 593.0, 429.0]);
        assert_eq!(dejavu.folded_bare, 357.0);

        let drawn = |w: f64| {
            (
                !plan_full(w, &hn()).ground_only,
                !plan_folded(w, &hn()).ground_only,
            )
        };
        // Without the % column the strip starts BARE_STRIP_GAP past the well, so the narrowest
        // decks that draw a table are a point (Full) and four (Folded's two blocks) wider than
        // where the now marker's line stood over the well's marks (the review of 2026-10-26).
        assert_eq!(
            [164.0, 165.0, 288.0, 289.0].map(drawn),
            [(false, false), (true, false), (true, false), (true, true)]
        );

        for (m, bp) in [(&hn(), helvetica), (&stack, dejavu)] {
            for (kind, w) in FullKind::ALL.into_iter().zip(bp.full) {
                assert_eq!(plan_full(w, m).kind, kind);
            }
            for (kind, w) in FullKind::ALL.into_iter().zip(bp.full).take(3) {
                assert_ne!(plan_full(w - 1.0, m).kind, kind);
            }
            for (kind, w) in BlockKind::ALL.into_iter().zip(bp.folded) {
                let at = plan_folded(w, m);
                assert_eq!(at.blocks.map(|b| b.kind), [kind; 2]);
                assert!(at.gauge.is_some(), "{kind:?} at {w} has the gauge");
                let below = plan_folded(w - 1.0, m);
                if kind == BlockKind::Slim {
                    assert_eq!((below.gauge, below.blocks[0].kind), (None, kind));
                    assert!(below.blocks[0].pct.is_some());
                } else {
                    assert_ne!(below.blocks[0].kind, kind);
                }
            }
            let bare = plan_folded(bp.folded_bare, m);
            assert_eq!((bare.gauge, bare.blocks[0].kind), (None, BlockKind::Slim));
            assert!(bare.blocks[0].pct.is_some());
            let below = plan_folded(bp.folded_bare - 1.0, m).blocks[0];
            assert_eq!((below.kind, below.pct), (BlockKind::Slim, None));
        }
    }

    /// Where the now marker's line stands in a table with `well`, `pct` and `strip`, against
    /// what is beside it (`what` names the table): a point wide at most, clear of the well's
    /// marks and of the % column in device px at 1× and 2×. Under slim's width it once stood
    /// inside fslim's well, over the right edge of its marks (the review of 2026-10-26).
    fn marker_faults(what: &str, well: Col, pct: Option<Col>, strip: Strip) -> Vec<String> {
        let x = strip.x - crate::stats_live::LINE_DX;
        let mut faults = Vec::new();
        for s in [1.0_f64, 2.0] {
            // As `Deck::live_edge` draws it.
            let lw = (0.75 * s).round().max(1.0);
            let lx = (x * s - lw / 2.0).round();
            if lx < (well.end() + 0.5) * s {
                faults.push(format!(
                    "{what} {s}×: the line at {lx} px meets the well {well:?}"
                ));
            }
            if let Some(pct) = pct
                && lx < pct.end() * s
            {
                faults.push(format!("{what} {s}×: the line at {lx} px meets {pct:?}"));
            }
        }
        faults
    }

    /// The strip is the row's widest part (the owner, 2026-10-26: "see how the time series is
    /// too showr"): wider than the bar, the figure run and Details, each.
    fn assert_widest(strip: Strip, meter: &Meter, details: Option<Col>, what: &str) {
        for col in meter.cols().into_iter().chain(details) {
            assert!(
                strip.width() > col.w,
                "{what}: the strip {strip:?} is no wider than {col:?}"
            );
        }
    }

    /// Narrowing, the run gives way first, then the bar (to a narrower kind's least, then under
    /// each %), and the strip last (`docs/DESIGN-stats-deck-meter-2026-10-26.md` §2): at every
    /// width, on both faces, a run stands only beside a bar at least as long as the run's least,
    /// no width lays a run below one that lays no bar column, and every strip beside a bar keeps
    /// its kind's share of the meter's room (up to its most). CONTROL: runs, bar columns and bars under the % are
    /// each seen in both forms.
    #[test]
    fn narrowing_drops_the_run_then_the_bar_and_the_strip_keeps_its_share() {
        for m in [hn(), FaceMetrics::test_stack()] {
            // (form, width, strip, meter, the kind's share, the run's least, a bar under the %)
            let mut tables = Vec::new();
            for px2 in 320..=5_120 {
                let w = f64::from(px2) / 2.0;
                let full = plan_full(w, &m);
                if !full.ground_only {
                    let spec = match full.kind {
                        FullKind::Wide => FULL_WIDE,
                        FullKind::Mid => FULL_MID,
                        FullKind::Narrow => FULL_NARROW,
                        FullKind::Slim => FULL_SLIM,
                    };
                    let under = full.bar_under().is_some();
                    tables.push(("Full", w, full.strip, full.meter, spec, under));
                }
                let folded = plan_folded(w, &m);
                if !folded.ground_only {
                    for b in folded.blocks {
                        let spec = match b.kind {
                            BlockKind::Wide => FOLD_WIDE,
                            BlockKind::Mid => FOLD_MID,
                            BlockKind::Narrow => FOLD_NARROW,
                            BlockKind::Slim => FOLD_SLIM,
                        };
                        let under = b.bar_under().is_some();
                        tables.push(("Folded", w, b.strip, b.meter, spec, under));
                    }
                }
            }
            for form in ["Full", "Folded"] {
                let ours = || tables.iter().filter(move |t| t.0 == form);
                let lowest_run = ours()
                    .filter(|t| t.3.run.is_some())
                    .map(|t| t.1)
                    .fold(f64::INFINITY, f64::min);
                let highest_under = ours()
                    .filter(|t| t.3.bar.is_none())
                    .map(|t| t.1)
                    .fold(0.0, f64::max);
                assert!(
                    highest_under < lowest_run,
                    "{form}: a run at {lowest_run} pt, no bar column at {highest_under} pt"
                );
                let seen = (
                    ours().filter(|t| t.3.run.is_some()).count(),
                    ours().filter(|t| t.3.bar.is_some()).count(),
                    ours().filter(|t| t.5).count(),
                );
                assert!(
                    seen.0 > 0 && seen.1 > seen.0 && seen.2 > 0,
                    "CONTROL {form}: {seen:?}"
                );
                for &(_, w, strip, meter, spec, _) in ours() {
                    let Some(bar) = meter.bar else { continue };
                    if meter.run.is_some() {
                        assert!(bar.w >= spec.run, "{form} {w} pt: {bar:?} beside a run");
                    }
                    let room = bar.end() - strip.x;
                    assert!(
                        strip.width() + spec.pitch >= (spec.share * room).min(spec.strip.1),
                        "{form} {w} pt: a strip of {} short of its share of {room}",
                        strip.width()
                    );
                }
            }
        }
    }

    /// The machine's used of total as `words::machine_totals` says it for a 10-core Mac of 32 GiB
    /// and 4 TB, `mem` GiB and `disk` TB used: each form with the template it is measured by.
    fn used_totals(mem: &str, disk: &str) -> Vec<TotalsForm> {
        let form = |said: String, widest: &str, used: bool| TotalsForm {
            said,
            widest: widest.to_owned(),
            used,
        };
        let totals = [
            "10 cores \u{b7} 32 GiB \u{b7} 4 TB",
            "10c \u{b7} 32G \u{b7} 4T",
        ];
        vec![
            form(
                format!("10 cores \u{b7} {mem} of 32 GiB \u{b7} {disk} of 4 TB"),
                "10 cores \u{b7} 88.8 of 32 GiB \u{b7} 8.88 of 4 TB",
                true,
            ),
            form(
                format!("10c \u{b7} {mem}/32G \u{b7} {disk}/4T"),
                "10c \u{b7} 88.8/32G \u{b7} 8.88/4T",
                true,
            ),
            form(totals[0].to_owned(), totals[0], false),
            form(totals[1].to_owned(), totals[1], false),
        ]
    }

    /// Every form's plan at every width from 300 pt to 2560 pt that lays a header.
    fn header_plans(m: &FaceMetrics) -> Vec<(DeckForm, f64, FullPlan, FoldedPlan)> {
        let mut out = Vec::new();
        for w in (300..=2_560).map(f64::from) {
            let (full, folded) = (plan_full(w, m), plan_folded(w, m));
            for form in [DeckForm::Full, DeckForm::Folded] {
                out.push((form, w, full, folded));
            }
        }
        out
    }

    /// What the legend's machine name says in `form` with `totals`, and whether a run stands
    /// there (where the legend takes no totals); `None` where no legend or key names it.
    fn legend_machine(
        at: (DeckForm, &FullPlan, &FoldedPlan),
        totals: &[TotalsForm],
        m: &FaceMetrics,
    ) -> Option<String> {
        legend_machine_over(at, totals, [false; 2], m)
    }

    /// [`legend_machine`] with the blocks' cells drawing the over mark as `over` says.
    fn legend_machine_over(
        (form, full, folded): (DeckForm, &FullPlan, &FoldedPlan),
        totals: &[TotalsForm],
        over: [bool; 2],
        m: &FaceMetrics,
    ) -> Option<String> {
        let text = HeaderText {
            totals,
            over,
            ..TEXT
        };
        let head = match form {
            DeckForm::Full => header_full(full, text, None, m),
            DeckForm::Folded => header_folded(folded, text, [None, None], m),
        };
        heads(&head, m).into_iter().next().map(|h| h.0)
    }

    /// Which of `totals` the machine's name says (`this Mac · …`), or `None` for the name alone.
    fn form_of(said: Option<&str>, totals: &[TotalsForm]) -> Option<usize> {
        let said = said?;
        totals
            .iter()
            .position(|t| said == format!("this Mac \u{b7} {}", t.said))
    }

    fn run_stands(form: DeckForm, full: &FullPlan, folded: &FoldedPlan) -> bool {
        match form {
            DeckForm::Full => full.meter.run.is_some(),
            DeckForm::Folded => folded.blocks[0].meter.run.is_some(),
        }
    }

    /// The legend says the machine's used of total in its longest form that fits
    /// (`docs/DESIGN-stats-deck-flow-2026-11-02.md` §4): at every width of both forms, the form
    /// it says fits alone there and says the figures of the first of the four that does (the used
    /// of total, else the totals alone, else neither), each measured by its widest, so a form
    /// whose words fit but whose widest does not is not said. It is that first form itself but
    /// where Folded's over mark's key keeps its room over block B's strip, which takes a long
    /// wording to its short one and never costs a figure (`docs/DESIGN-stats-deck-meter-
    /// 2026-10-26.md` §4). CONTROL: each used-of-total form and the totals alone are each said at
    /// some width; the key's room shortens a form at some Folded width; and a form measured by
    /// its words alone is said where its widest does not fit.
    #[test]
    fn the_legend_says_used_of_total_in_its_longest_form_that_fits() {
        let m = hn();
        let totals = used_totals("14.4", "2.34");
        // What a form says: the used of total, the totals alone, or neither (`None`).
        let figures = |i: Option<usize>| i.map_or(2, |i| usize::from(!totals[i].used));
        let mut said = [0_usize; 5];
        let (mut by_words, mut keyed) = (0, 0);
        for (form, w, full, folded) in header_plans(&m) {
            let at = (form, &full, &folded);
            let chosen = form_of(legend_machine(at, &totals, &m).as_deref(), &totals);
            let fits: Vec<bool> = totals
                .iter()
                .map(|t| {
                    let one = std::slice::from_ref(t);
                    form_of(legend_machine(at, one, &m).as_deref(), one) == Some(0)
                })
                .collect();
            let first = fits.iter().position(|&f| f);
            assert!(
                chosen.is_none_or(|c| fits[c]),
                "{form:?} {w} pt: says {chosen:?}, which does not fit alone: {fits:?}"
            );
            assert_eq!(
                figures(chosen),
                figures(first),
                "{form:?} {w} pt: says {chosen:?} where {first:?} fits"
            );
            if chosen != first {
                assert_eq!(form, DeckForm::Folded, "{w} pt: {chosen:?} for {first:?}");
                assert_eq!(chosen, first.map(|f| f + 1), "{w} pt: its short wording");
                keyed += 1;
            }
            said[chosen.unwrap_or(4)] += 1;
            if !fits[0] {
                let lying = TotalsForm {
                    said: "10c".into(),
                    widest: totals[0].widest.clone(),
                    used: true,
                };
                let one = std::slice::from_ref(&lying);
                assert_ne!(
                    form_of(legend_machine(at, one, &m).as_deref(), one),
                    Some(0),
                    "{form:?} {w} pt: the words fit, the widest does not"
                );
                let honest = said_totals(&["10c"]);
                by_words += usize::from(
                    form_of(legend_machine(at, &honest, &m).as_deref(), &honest) == Some(0),
                );
            }
        }
        assert!(
            said.iter().all(|&n| n > 0),
            "CONTROL: each form is said: {said:?}"
        );
        assert!(
            keyed > 0,
            "CONTROL: the over mark's key's room shortens a form at some Folded width"
        );
        assert!(
            by_words > 0,
            "CONTROL: a short form's words fit where the long one does not"
        );
    }

    /// A changing reading never flips the legend's form (`docs/DESIGN-stats-deck-flow-
    /// 2026-11-02.md` §4): the same Mac read near empty, part used and near full, with the cells
    /// of neither block, either or both drawing the over mark, says the same form at every width
    /// of both forms, since each form is measured by its widest and Folded's keeps the over mark's
    /// key its room over B's strip whether or not a cell draws the mark; and Folded's legend ends
    /// at block B's meter whatever its words. CONTROL: the same forms measured by their words
    /// alone flip at some width, and at some Folded width the legend says a shorter form than
    /// fits in its room with the key's, so a legend that kept the key's room only while B draws
    /// the mark would flip there.
    #[test]
    fn a_changing_reading_never_flips_the_legends_form() {
        let m = hn();
        let readings = [
            ("0.1", "0.01"),
            ("3.0", "0.95"),
            ("14.4", "2.34"),
            ("31.9", "3.99"),
        ];
        let overs = [[false, false], [false, true], [true, false], [true, true]];
        let measured: Vec<Vec<TotalsForm>> =
            readings.iter().map(|&(g, t)| used_totals(g, t)).collect();
        let by_words: Vec<Vec<TotalsForm>> = measured
            .iter()
            .map(|forms| {
                forms
                    .iter()
                    .map(|t| TotalsForm {
                        said: t.said.clone(),
                        widest: t.said.clone(),
                        used: t.used,
                    })
                    .collect()
            })
            .collect();
        let (mut flips, mut used, mut keyed) = (0, 0, 0);
        for (form, w, full, folded) in header_plans(&m) {
            let at = (form, &full, &folded);
            let said = |totals: &[TotalsForm], over: [bool; 2]| {
                form_of(legend_machine_over(at, totals, over, &m).as_deref(), totals)
            };
            let forms: Vec<Option<usize>> = overs
                .iter()
                .flat_map(|&over| measured.iter().map(move |t| (t, over)))
                .map(|(t, over)| said(t, over))
                .collect();
            assert!(
                forms.windows(2).all(|p| p[0] == p[1]),
                "{form:?} {w} pt: {forms:?}"
            );
            used += usize::from(forms[0].is_some_and(|i| i < 2));
            let words: Vec<Option<usize>> = by_words.iter().map(|t| said(t, [false; 2])).collect();
            flips += usize::from(words.windows(2).any(|p| p[0] != p[1]));
            if form == DeckForm::Folded
                && let (Some(_), Some(b_end)) =
                    (folded.blocks[0].meter.bar, folded.blocks[1].meter.end())
            {
                let hi = b_end.min(folded.blocks[1].at.end());
                for (totals, over) in overs
                    .iter()
                    .flat_map(|&over| measured.iter().map(move |t| (t, over)))
                {
                    let text = HeaderText {
                        totals,
                        over,
                        ..TEXT
                    };
                    let head = header_folded(&folded, text, [None, None], &m);
                    if let Some(&(_, end)) = heads(&head, &m).last() {
                        assert!(
                            (end - hi).abs() < 1e-9,
                            "W {w} {over:?}: the legend ends at {end}"
                        );
                    }
                }
                // The longest of these forms that fits in the legend's whole room, the key's
                // included, against the one it says.
                let text = HeaderText {
                    totals: &measured[2],
                    ..TEXT
                };
                let tail = folded.blocks[0].meter.run.is_none();
                let lead = folded.blocks[1].strip.x + MARKER_ROOM;
                let whole = Header::new(&m)
                    .legend_forms(&text, tail)
                    .iter()
                    .position(|f| f.2 <= hi - lead + 1e-9)
                    .filter(|&i| i < measured[2].len());
                keyed += usize::from(forms[0] != whole);
            }
        }
        assert!(used > 0, "CONTROL: a used-of-total form is said");
        assert!(
            flips > 0,
            "CONTROL: measured by their words, the forms flip"
        );
        assert!(
            keyed > 0,
            "CONTROL: at some Folded width the key's room shortens the form it says"
        );
    }

    /// Where a run stands at the end of the bars, it says each row's maximum, so the legend says
    /// the machine's name alone (`docs/DESIGN-stats-deck-meter-2026-10-26.md` §4, kept by the
    /// flow record's §4): at every width of both forms where one stands, no totals follow the
    /// name. CONTROL: where none stands, the legend says the totals at some width.
    #[test]
    fn a_standing_run_takes_the_totals_from_the_legend() {
        let m = hn();
        let totals = used_totals("14.4", "2.34");
        let (mut with_run, mut totals_said) = (0, 0);
        for (form, w, full, folded) in header_plans(&m) {
            let said = legend_machine((form, &full, &folded), &totals, &m);
            if run_stands(form, &full, &folded) {
                with_run += 1;
                assert!(
                    said.as_deref().is_none_or(|s| !s.contains('\u{b7}')),
                    "{form:?} {w} pt: a run stands, yet the legend says {said:?}"
                );
            } else {
                totals_said += usize::from(form_of(said.as_deref(), &totals).is_some());
            }
        }
        assert!(
            with_run > 0 && totals_said > 0,
            "CONTROL: {with_run} runs, {totals_said} totals"
        );
    }

    /// The legend keys the bars once and never across the Folded blocks' divider: every scope
    /// name stands, with its swatch, inside one block, at every width, with and without the
    /// machine's totals. CONTROL: a legend is laid at most widths.
    #[test]
    fn the_legend_stays_inside_one_block() {
        let m = hn();
        let totals = said_totals(&[
            "18 cores \u{b7} 128 GiB \u{b7} 2 TB",
            "18c \u{b7} 128G \u{b7} 2T",
        ]);
        let mut laid = 0;
        for w in (300..=2_560).map(f64::from) {
            let plan = plan_folded(w, &m);
            if plan.ground_only {
                continue;
            }
            for text in [
                TEXT,
                HeaderText {
                    totals: &totals,
                    ..TEXT
                },
            ]
            .into_iter()
            .flat_map(|text| {
                [[true, false], [false, true], [true; 2], [false; 2]]
                    .map(|over| HeaderText { over, ..text })
            }) {
                let head = header_folded(&plan, text, [None, None], &m);
                let said = heads(&head, &m);
                laid += usize::from(said.len() == 3);
                for (l, (words, end)) in head
                    .labels
                    .iter()
                    .filter(|l| matches!(l.label, Label::Scope(_)))
                    .zip(&said)
                {
                    assert!(
                        plan.blocks
                            .iter()
                            .any(|b| l.x >= b.at.x - 1e-9 && *end <= b.at.end() + 1e-9),
                        "W {w}: {words:?} at {}..{end} crosses a block's edge",
                        l.x
                    );
                }
            }
        }
        assert!(laid > 3_000, "CONTROL: {laid} legends laid");
    }

    /// A state no row says keeps `now` beside each live edge's dot at the owner's widths and down
    /// to Full's narrow tables: it stands over the names beside `now`, or at the strip's left
    /// (the owner's hot scene said "Memory warn" over `now`, review of 2026-10-26).
    #[test]
    fn a_state_keeps_now_beside_the_dot() {
        let m = hn();
        let states = [
            StateLabel {
                full: "Memory warn +1",
                bare: "Memory warn",
            },
            StateLabel {
                full: "Disk full ~34 min +1",
                bare: "Disk full ~34 min",
            },
        ];
        let nows = |head: &HeaderPlan| head.labels.iter().filter(|l| l.label == Label::Now).count();
        let stated = |head: &HeaderPlan| {
            head.labels
                .iter()
                .filter(|l| matches!(l.label, Label::State(_)))
                .count()
        };
        for state in states {
            for w in [1039.0, 940.0, 700.0, 450.0, 400.0] {
                let plan = plan_full(w, &m);
                let head = header_full(&plan, TEXT, Some(state), &m);
                assert_eq!(
                    nows(&head),
                    1,
                    "Full {w} pt, {:?}: {:?}",
                    state.full,
                    head.labels
                );
                // Where Details stand, they say the state.
                assert_eq!(
                    stated(&head),
                    usize::from(plan.details.is_none()),
                    "Full {w} pt"
                );
            }
            for w in (940..=1_040).map(f64::from) {
                let plan = plan_folded(w, &m);
                let head = header_folded(&plan, TEXT, [Some(state), Some(state)], &m);
                assert_eq!(
                    nows(&head),
                    2,
                    "Folded {w} pt, {:?}: {:?}",
                    state.full,
                    head.labels
                );
                assert_eq!(stated(&head), 2, "Folded {w} pt");
            }
        }
    }

    #[test]
    fn every_width_plans_inside_the_card() {
        for m in [hn(), FaceMetrics::test_stack()] {
            let slim_beside = breakpoints(&m).folded[3];
            for w in (160..=5120).map(f64::from) {
                let full = plan_full(w, &m);
                assert_ne!(full.strip.period_s, FOLD_SLOT_S, "W {w}: a Full strip");
                assert_eq!(
                    full.strip.spec,
                    Strip::spec_for(full.strip.width()),
                    "W {w}"
                );
                if !full.ground_only {
                    // A reading a second moves its fine band 2 pt or more, half a 2 s box of 4 pt
                    // or more, to the rounding of the box's place in the strip.
                    assert!(full.strip.pt_per_s() >= 1.99, "Full W {w}");
                    assert_widest(
                        full.strip,
                        &full.meter,
                        full.details,
                        &format!("Full W {w}"),
                    );
                    assert_in_order(&full_cols(&full), X0, w - X0, &format!("Full W {w}"));
                    let what = format!("Full W {w}");
                    let faults = marker_faults(&what, full.well, full.pct, full.strip);
                    assert!(faults.is_empty(), "{faults:?}");
                }
                if full.offset > 0.0
                    && let (CoresAt::Block { at, .. }, Some(details)) = (full.cores, full.details)
                {
                    assert!(
                        (at.x - (w - details.end())).abs() <= 1.0,
                        "W {w} is not centred"
                    );
                }

                let folded = plan_folded(w, &m);
                let [a, b] = folded.blocks;
                assert!(a.at.end() < folded.divider && folded.divider < b.at.x);
                if let Some(g) = folded.gauge {
                    let [x0, y0, x1, y1] = g.dial();
                    assert!(
                        x0 >= X0 && x1 < g.divider && g.divider < a.at.x && a.at.x == a.name.x,
                        "Folded W {w}: the gauge left of block A: {g:?} {:?}",
                        a.at
                    );
                    assert!(y0 >= 4.0 && y1 <= 58.0, "Folded W {w}: {g:?} in the band");
                }
                assert_eq!(folded.gauge.is_some(), w >= slim_beside, "Folded W {w}");
                for block in &folded.blocks {
                    let pitch = match block.kind {
                        BlockKind::Wide => 5.0,
                        BlockKind::Mid => 4.0,
                        BlockKind::Narrow | BlockKind::Slim => 3.0,
                    };
                    assert_eq!(
                        (block.strip.period_s, block.strip.pitch),
                        (FOLD_SLOT_S, pitch),
                        "Folded W {w}"
                    );
                    assert_eq!(block.strip.spec, Strip::spec_for(block.strip.width()));
                    if !folded.ground_only {
                        // A reading a second moves its fine band 2 pt or more (the now-left
                        // design's time-true step, live §8's ruling of 2026-10-26).
                        assert!(block.strip.pt_per_s() >= 1.99, "Folded W {w}");
                        let what = format!("Folded W {w}");
                        assert_widest(block.strip, &block.meter, block.details, &what);
                    }
                    if !folded.ground_only {
                        assert_in_order(
                            &block_cols(block),
                            block.at.x,
                            block.at.end(),
                            &format!("Folded W {w}"),
                        );
                        let what = format!("Folded W {w}");
                        let faults = marker_faults(&what, block.well, block.pct, block.strip);
                        assert!(faults.is_empty(), "{faults:?}");
                    }
                }
                if !folded.ground_only {
                    assert!(a.at.x >= X0 && b.at.end() <= w - X0, "Folded W {w}");
                }
            }
        }
    }

    #[test]
    fn deck_height_is_a_function_of_the_window() {
        use crate::stats_deck::{StatsBars, deck_px};
        use DeckForm::{Folded, Full};
        let _: fn(DeckSizePref) -> DeckForm = deck_form_for;
        let _: fn(StatsBars, DeckForm, f64, usize) -> usize = deck_px;

        // Folded unless chosen Full: the form reads nothing of the window.
        assert_eq!(deck_form_for(DeckSizePref::Auto), Folded);
        assert_eq!(deck_form_for(DeckSizePref::Full), Full);
        assert_eq!(deck_form_for(DeckSizePref::Folded), Folded);

        assert_eq!(full_delta_px(2.0, 28), 84);
        assert_eq!(deck_px(StatsBars::Deck, Full, 2.0, 28), 208);
        for s in [1.0, 2.0] {
            let folded = (FOLDED_PT * s).round() as usize;
            let table = (FULL_TABLE_PT * s).round() as usize;
            for cell in 10..=40 {
                let at = format!("{cell} px cells at {s}×");
                assert_eq!(deck_px(StatsBars::Deck, Folded, s, cell), folded, "{at}");
                let full = deck_px(StatsBars::Deck, Full, s, cell);
                assert_eq!((full - folded) % cell, 0, "{at}");
                assert_eq!(full - folded, full_delta_px(s, cell), "{at}");
                assert!((table..table + cell).contains(&full), "{at}");
                for bars in [StatsBars::Sill, StatsBars::Off] {
                    for deck in [Folded, Full] {
                        assert_eq!(deck_px(bars, deck, s, cell), 0, "{bars:?} {at}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_deck_size_setting_reads_its_own_words() {
        for pref in DeckSizePref::ALL {
            assert_eq!(DeckSizePref::parse(Some(pref.word())), pref);
            assert_eq!(DeckSizePref::from_word(pref.word()), Some(pref));
            let loud = format!(" {} ", pref.word().to_uppercase());
            assert_eq!(DeckSizePref::parse(Some(&loud)), pref);
        }
        assert_eq!(DeckSizePref::parse(None), DeckSizePref::Auto);
        assert_eq!(DeckSizePref::parse(Some("huge")), DeckSizePref::Auto);
        assert_eq!(DeckSizePref::from_word("huge"), None);
    }

    fn machines() -> Vec<Vec<Cluster<'static>>> {
        const NAMES: [&[&str]; 4] = [
            &["Performance"],
            &["Performance", "Efficiency"],
            &["Super", "Performance", "Efficiency"],
            &["Super", "Performance", "Mid 2", "Efficiency"],
        ];
        let mut machines = Vec::new();
        for n in 4..=128_usize {
            machines.push(vec![Cluster {
                count: n,
                name: None,
            }]);
            for names in NAMES {
                let k = names.len();
                machines.push(
                    names
                        .iter()
                        .enumerate()
                        .map(|(i, &name)| Cluster {
                            count: n / k + usize::from(i < n % k),
                            name: Some(name),
                        })
                        .collect(),
                );
            }
        }
        machines
    }

    fn assert_row_fits(
        clusters: &[Cluster<'_>],
        mode: CoresMode,
        ladder: &[CoresMode],
        room: f64,
        cell: f64,
        caption_pt: f64,
        m: &FaceMetrics,
    ) {
        let total = count_words(clusters);
        if mode == CoresMode::CountOnly {
            let words = cores_count_text(clusters, room, caption_pt, m);
            assert!(
                words == total
                    || (words.starts_with(&format!("{total}: ")) && !words.ends_with('…')),
                "{clusters:?} in {room} pt says {words:?}"
            );
            assert!(m.width(&words, caption_pt, false) <= room);
        } else {
            assert!(ladder.contains(&mode), "{mode:?} is not this row's");
            assert!(
                cores_row_width(clusters, mode, cell, caption_pt, m) <= room,
                "{clusters:?} in {room} pt"
            );
        }
    }

    #[test]
    fn cores_fit_every_machine() {
        let machines = machines();
        for m in [hn(), FaceMetrics::test_stack()] {
            let bp = breakpoints(&m);
            let full_rows = [bp.full[2], bp.full[3]].map(|w| plan_full(w, &m));
            let folded = bp.folded.map(|w| plan_folded(w, &m).blocks[1]);
            for clusters in &machines {
                for plan in &full_rows {
                    let CoresAt::Row(room) = plan.cores else {
                        panic!("{:?} has a Cores row", plan.kind);
                    };
                    let mode = plan.cores_mode(clusters, &m).expect("a Cores row");
                    let ladder = [CoresMode::Stack, CoresMode::Tiny];
                    assert_row_fits(
                        clusters,
                        mode,
                        &ladder,
                        room.w,
                        plan.rows.cell,
                        CORES_CAPTION_PT,
                        &m,
                    );
                }
                for block in &folded {
                    let mode = block.cores_mode(clusters, &m);
                    let caption_pt = block.kind.cores_caption_pt();
                    let ladder = block.kind.cores_ladder();
                    assert_row_fits(
                        clusters,
                        mode,
                        ladder,
                        block.cores.w,
                        block.rows.cell,
                        caption_pt,
                        &m,
                    );
                }
            }

            let unread = [Cluster {
                count: 0,
                name: None,
            }];
            for clusters in [&[][..], &unread] {
                let rows = full_rows.iter().map(|plan| {
                    let CoresAt::Row(room) = plan.cores else {
                        panic!("{:?} has a Cores row", plan.kind);
                    };
                    (plan.cores_mode(clusters, &m).expect("a Cores row"), room.w)
                });
                let blocks = folded
                    .iter()
                    .map(|block| (block.cores_mode(clusters, &m), block.cores.w));
                for (mode, room) in rows.chain(blocks) {
                    assert_eq!(mode, CoresMode::CountOnly);
                    let words = cores_count_text(clusters, room, CORES_CAPTION_PT, &m);
                    assert!(words.ends_with("not read"), "the row says {words:?}");
                }
            }
        }
    }

    #[test]
    fn an_m5_draws_its_cores_large_where_it_can() {
        let m = hn();
        let m5 = [
            Cluster {
                count: 6,
                name: Some("Super"),
            },
            Cluster {
                count: 12,
                name: Some("Performance"),
            },
        ];
        assert_eq!(
            plan_full(1119.0, &m).cores_mode(&m5, &m),
            None,
            "the gauge's slot"
        );
        assert_eq!(
            plan_full(400.0, &m).cores_mode(&m5, &m),
            Some(CoresMode::Stack)
        );
        assert_eq!(
            plan_folded(1119.0, &m).blocks[1].cores_mode(&m5, &m),
            CoresMode::Big
        );
        assert_eq!(
            cores_count_text(&m5, 400.0, CORES_CAPTION_PT, &m),
            "18 cores: 6 Super, 12 Performance"
        );
        assert_eq!(
            cores_count_text(&m5, 60.0, CORES_CAPTION_PT, &m),
            "18 cores"
        );
    }

    fn placed(head: &HeaderPlan, item: KeyItem) -> Option<f64> {
        head.key
            .placed
            .iter()
            .find(|&&(i, _)| i == item)
            .map(|&(_, x)| x)
    }

    fn label_x(head: &HeaderPlan, label: Label) -> Option<f64> {
        head.labels.iter().find(|l| l.label == label).map(|l| l.x)
    }

    /// The header's scope names, each after its swatch: its words and where it ends.
    fn heads(head: &HeaderPlan, m: &FaceMetrics) -> Vec<(String, f64)> {
        head.labels
            .iter()
            .filter_map(|l| match l.label {
                Label::Scope(_) => {
                    let end = l.x + SWATCH_ADVANCE + m.width(&l.text, HEADER_PT, false);
                    Some((l.text.clone(), end))
                }
                _ => None,
            })
            .collect()
    }

    /// The legend over the bars: `names` in order, each after its swatch, the first from `x`
    /// and each next [`LEGEND_GAP`] past the one before.
    fn assert_legend(head: &HeaderPlan, m: &FaceMetrics, x: f64, names: &[&str]) {
        let got = heads(head, m);
        let words: Vec<&str> = got.iter().map(|h| h.0.as_str()).collect();
        assert_eq!(words, names, "{:?}", head.labels);
        let mut at = x;
        for (l, (_, end)) in head
            .labels
            .iter()
            .filter(|l| matches!(l.label, Label::Scope(_)))
            .zip(&got)
        {
            assert!((l.x - at).abs() < 1e-9, "{l:?} starts at {}, not {at}", l.x);
            at = end + LEGEND_GAP;
        }
    }

    #[test]
    fn the_key_goes_where_the_spec_says() {
        let m = hn();

        // The time strip's header carries the time label, the scale's ticks (the live edge's
        // design) and the key between them; what the ticks leave no room for goes, by weight,
        // to the next free span.
        let plan = plan_full(1119.0, &m);
        let [label, _] = time_words(plan.strip);
        assert_eq!(label, "6h ago", "a Full wide strip spans six hours");
        let head = header_full(&plan, TEXT, None, &m);
        let ticks: Vec<u32> = head.ruler.iter().map(|t| t.age_s).collect();
        assert!(
            ticks.contains(&3600) && ticks.contains(&600),
            "Full wide marks multi-scale horizon ticks: {ticks:?}"
        );
        assert!(placed(&head, KeyItem::Over).is_some());
        assert_eq!(
            (
                placed(&head, KeyItem::Ramp),
                placed(&head, KeyItem::NotRead)
            ),
            (None, None),
            "wide keys only the over mark"
        );
        // The legend once over the bars, from the bar's start, its swatches in the bar's inks;
        // the over mark's key over the strip, clear of it.
        let bar = plan.meter.bar.expect("wide's bar");
        let names = ["this Mac", "aterm", "Tab 2"];
        assert_legend(&head, &m, bar.x, &names);
        let over = placed(&head, KeyItem::Over).expect("the over mark's key");
        assert!(
            over + KeyItem::Over.width(&m) < bar.x,
            "the over key {over} before the legend"
        );
        assert_eq!(
            label_x(&head, Label::Now),
            Some(plan.strip.x - WORDS_TO_STRIP),
            "\"now\" beside the live edge's dot, clear of its beat"
        );
        assert_eq!(label_x(&head, Label::Details), None);
        // With no over mark on screen the key is empty, and the scale keeps what it had.
        let calm = header_full(
            &plan,
            HeaderText {
                over: [false; 2],
                ..TEXT
            },
            None,
            &m,
        );
        assert!(calm.key.placed.is_empty(), "{:?}", calm.key.placed);
        assert!(calm.ruler.len() >= head.ruler.len());

        // Narrow keys all three marks by rule K, the over mark over the names; the legend
        // stands over the bar, which is as long as it, from the bar's start.
        let plan = plan_full(400.0, &m);
        assert_eq!(plan.kind, FullKind::Narrow);
        let head = header_full(&plan, TEXT, None, &m);
        assert_eq!(placed(&head, KeyItem::Over), Some(plan.name.x));
        assert!(placed(&head, KeyItem::Ramp).is_some(), "{:?}", head.key);
        let bar = plan.meter.bar.expect("narrow's bar");
        let said = heads(&head, &m);
        let names: Vec<&str> = said.iter().map(|h| h.0.as_str()).collect();
        assert_eq!(names, ["Mac", "aterm", "Tab"], "{:?}", head.labels);
        assert_legend(&head, &m, bar.x, &names);
        assert!(said[2].1 <= bar.end() + 1e-9, "{said:?} past {bar:?}");
        // Where the bars stand under each %, the legend stands over the strip's header at its
        // left, past the now marker's room, and the scale keeps its label and a tick.
        let plan = plan_full(350.0, &m);
        assert!(plan.meter.bar.is_none() && plan.bar_under().is_some());
        let head = header_full(&plan, TEXT, None, &m);
        let said = heads(&head, &m);
        let names: Vec<&str> = said.iter().map(|h| h.0.as_str()).collect();
        assert_eq!(names.len(), 3, "{:?}", head.labels);
        assert_legend(&head, &m, plan.strip.x + MARKER_ROOM, &names);
        assert!(
            !head.ruler.is_empty(),
            "the scale keeps a tick: {:?}",
            head.labels
        );
        assert!(
            head.ruler
                .iter()
                .all(|t| t.left >= said[2].1 + KEY_GAP - 1e-9),
            "the ticks stand past the legend: {:?}",
            head.ruler
        );
        assert_eq!(
            label_x(&head, Label::Time).map(|x| x > said[2].1),
            Some(true)
        );

        // With marks in both blocks, Folded keys "over max" over block B's strip past its now
        // marker (block A's carries the time label, right-aligned over its old end, and the
        // scale). A single marked block is covered below.
        let plan = plan_folded(1119.0, &m);
        let head = header_folded(&plan, TEXT, [None, None], &m);
        let [a, b] = plan.blocks;
        assert_eq!(
            label_x(&head, Label::Time),
            Some(a.strip.end() - m.width("6h ago", HEADER_PT, false))
        );
        assert_eq!(
            head.labels
                .iter()
                .find(|l| l.label == Label::Time)
                .map(|l| l.text.as_str()),
            Some("6h ago")
        );
        assert!(!head.ruler.is_empty(), "Folded wide places X-axis ticks");
        assert_eq!(
            head.labels
                .iter()
                .filter(|l| l.label == Label::Now)
                .map(|l| l.x)
                .collect::<Vec<_>>(),
            [a.strip, b.strip].map(|strip| strip.x - WORDS_TO_STRIP),
            "\"now\" over each %, right-aligned clear of the dot's beat"
        );
        assert_eq!(placed(&head, KeyItem::Over), Some(b.strip.x + MARKER_ROOM));
        assert_eq!(
            (
                placed(&head, KeyItem::Ramp),
                placed(&head, KeyItem::NotRead)
            ),
            (None, None)
        );
        let calm = HeaderText {
            over: [false; 2],
            ..TEXT
        };
        assert!(
            header_folded(&plan, calm, [None, None], &m)
                .key
                .placed
                .is_empty()
        );
        // The legend once, at the deck's top right: ending at block B's bar's end, over its
        // bars and its strip's old end, clear of the over mark's key and never across the
        // blocks' divider; with the machine's totals where they fit, as no run says the rows'
        // maxima.
        let whole = |global: &str| {
            3.0 * SWATCH_ADVANCE
                + m.width(global, HEADER_PT, false)
                + m.width("aterm", HEADER_PT, false)
                + m.width("Tab 2", HEADER_PT, false)
                + 2.0 * LEGEND_GAP
        };
        let b_end = b.meter.bar.map(Col::end).expect("fwide's bar");
        assert_legend(&head, &m, b_end - whole("this Mac"), &names);
        let over = placed(&head, KeyItem::Over).expect("the over mark's key");
        assert!(over + KeyItem::Over.width(&m) + KEY_GAP <= b_end - whole("this Mac") + 1e-9);
        assert_eq!(label_x(&head, Label::Details), None);
        let totals = said_totals(&[
            "18 cores \u{b7} 128 GiB \u{b7} 2 TB",
            "18c \u{b7} 128G \u{b7} 2T",
        ]);
        for w in [1039.0, 940.0, 750.0] {
            let plan = plan_folded(w, &m);
            let [_, b] = plan.blocks;
            let b_end = b.meter.bar.map(Col::end).expect("the bar's column");
            let head = header_folded(&plan, TEXT, [None, None], &m);
            assert_legend(&head, &m, b_end - whole("this Mac"), &names);
            let with = HeaderText {
                totals: &totals,
                ..TEXT
            };
            let head = header_folded(&plan, with, [None, None], &m);
            let said = heads(&head, &m);
            let x0 = head
                .labels
                .iter()
                .find(|l| l.label == Label::Scope(Bar::Global))
                .map(|l| l.x)
                .expect("the legend");
            assert!(x0 >= b.strip.x + MARKER_ROOM, "W {w}: {:?}", head.labels);
            assert!((said[2].1 - b_end).abs() < 1e-9, "W {w}: {said:?}");
            assert!(said[0].0.starts_with("this Mac \u{b7} "), "W {w}: {said:?}");
        }
    }

    /// An fslim block's state takes the header: over its names, and where it needs more room
    /// over the % column's too, short of the widest ring the now marker's dot beats (the ring
    /// once drew over words that ended at the dot, review of 2026-10-26). Words too long even for
    /// that take the strip's header as well, whole: the marker's dot and `now` yield to them (one
    /// block at most), and the time scale stands over the other block's strip.
    #[test]
    fn an_fslim_state_takes_the_header() {
        let m = hn();
        let plan = plan_folded(450.0, &m);
        assert!(plan.blocks.iter().all(|b| b.kind == BlockKind::Slim));
        let [a, b] = plan.blocks;
        let (a_pct, b_pct) = (a.pct.expect("fslim's %"), b.pct.expect("fslim's %"));
        let short = StateLabel {
            full: "Disk full",
            bare: "Disk full",
        };
        let mid = StateLabel {
            full: "Memory warn +1",
            bare: "Memory warn",
        };
        let long = StateLabel {
            full: "Disk full ~34 min +1",
            bare: "Disk full ~34 min",
        };
        let nows = |head: &HeaderPlan| -> Vec<f64> {
            head.labels
                .iter()
                .filter(|l| l.label == Label::Now)
                .map(|l| l.x)
                .collect()
        };
        let state_of = |head: &HeaderPlan, x: f64| -> String {
            head.labels
                .iter()
                .find(|l| matches!(l.label, Label::State(_)) && l.x == x)
                .map(|l| l.text.clone())
                .expect("the block names its state")
        };
        let end_of = |x: f64, words: &str| x + MARK_ADVANCE + m.width(words, HEADER_PT, true);
        let ring = |strip: Strip| {
            use crate::stats_live::{BEAT_REACH, DOT_R, LINE_DX};
            strip.x - LINE_DX - DOT_R - BEAT_REACH
        };
        // "now" ends over its % column, a point short of the dot's widest ring.
        let now_end = |strip: Strip| strip.x - WORDS_TO_STRIP;
        for (strip, pct) in [(a.strip, a_pct), (b.strip, b_pct)] {
            let now_x = now_end(strip) - m.width("now", HEADER_PT, false);
            assert!(now_end(strip) <= ring(strip) - 1.0 + 1e-9 && now_x >= pct.x);
        }
        // A state that fits over the names leaves "now" over the % and every dot.
        let head = header_folded(&plan, TEXT, [Some(short), None], &m);
        assert_eq!(state_of(&head, a.name.x), "Disk full");
        assert_eq!(nows(&head), [now_end(a.strip), now_end(b.strip)]);
        assert!(time_on(&head, a.strip, &m), "{:?}", head.labels);
        assert_eq!(head.dotless, None);

        // One that needs the %'s header too takes it, ending where "now" would, short of the
        // dot's ring; "now" yields.
        let head = header_folded(&plan, TEXT, [Some(mid), None], &m);
        let words = state_of(&head, a.name.x);
        assert!(words.starts_with("Memory warn"), "{words:?}");
        let end = end_of(a.name.x, &words);
        assert!(
            end <= now_end(a.strip) + 1e-9 && end <= ring(a.strip) - 1.0 + 1e-9,
            "{words:?} ends at {end}"
        );
        assert!(
            end > now_end(a.strip) - m.width("now", HEADER_PT, false) - STRIP_LEAD,
            "CONTROL: {words:?} needed the %'s header"
        );
        assert_eq!(nows(&head), [now_end(b.strip)], "\"now\" yields over A");
        assert_eq!(head.dotless, None, "the dot stays");

        // A long one takes block A's strip's header too and is said whole; A's dot yields to it,
        // and the time scale stands over B's strip.
        let head = header_folded(&plan, TEXT, [Some(long), None], &m);
        let words = state_of(&head, a.name.x);
        assert_eq!(words, "Disk full ~34 min +1");
        assert!(
            end_of(a.name.x, &words) > now_end(a.strip),
            "CONTROL: it needed the strip"
        );
        assert!(end_of(a.name.x, &words) <= a.strip.end() + 1e-9);
        assert_eq!(head.dotless, Some(a.strip.x));
        assert_eq!(nows(&head), [now_end(b.strip)]);
        assert!(time_on(&head, b.strip, &m), "{:?}", head.labels);

        // Only one block spills: B's long state is fitted short of its dot.
        let head = header_folded(&plan, TEXT, [Some(long), Some(long)], &m);
        assert_eq!(state_of(&head, a.name.x), "Disk full ~34 min +1");
        let words = state_of(&head, b.name.x);
        let end = end_of(b.name.x, &words);
        assert!(words.ends_with('…'), "{words:?}");
        assert!(end <= now_end(b.strip) + 1e-9, "{words:?}");
        assert_eq!(head.dotless, Some(a.strip.x));
        assert!(nows(&head).is_empty(), "{:?}", head.labels);
        assert!(time_on(&head, b.strip, &m), "{:?}", head.labels);
        // B alone spilling keeps the scale over A.
        let head = header_folded(&plan, TEXT, [None, Some(long)], &m);
        assert_eq!(head.dotless, Some(b.strip.x));
        assert!(time_on(&head, a.strip, &m), "{:?}", head.labels);
    }

    /// Whether `head`'s time label stands on `strip`: over its right (old) end, right-aligned to
    /// it, or beside it, starting [`STRIP_LEAD`] past that end with a tick marking it.
    fn time_on(head: &HeaderPlan, strip: Strip, m: &FaceMetrics) -> bool {
        head.labels.iter().any(|l| {
            let w = m.width(&l.text, HEADER_PT, false);
            l.label == Label::Time
                && ((l.x + w - strip.end()).abs() < 1e-9 && head.time_tick.is_none()
                    || (l.x - (strip.end() + STRIP_LEAD)).abs() < 1e-9
                        && head.time_tick == Some(strip.end()))
        })
    }

    /// Every half-point width from 160 to 5120 pt on both faces, on every core: each whole width,
    /// and the half point past it where it plans otherwise (the plans read whole points,
    /// [`whole_width`]). A Folded plan that differs from the width before it only in `w` is not
    /// headed again, since `header_folded` reads only the blocks and `ground_only`.
    #[test]
    fn key_is_placed() {
        let long = StateLabel {
            full: "Disk emergency, out of file slots +3",
            bare: "Disk emergency, out of file slots",
        };
        let short = StateLabel {
            full: "Memory warn",
            bare: "Memory warn",
        };
        let check = |head: &HeaderPlan, m: &FaceMetrics, exempt: bool, what: &str| {
            let fits = head
                .key
                .spans
                .iter()
                .any(|s| s.x1 - s.x0 >= KeyItem::Over.width(m));
            assert_eq!(head.key.has(KeyItem::Over), fits, "{what}");
            assert!(exempt || fits, "{what}: no span holds over max");
            for &(item, x) in &head.key.placed {
                let inside = |s: &KeySpan| x >= s.x0 && x + item.width(m) <= s.x1;
                assert!(head.key.spans.iter().any(inside), "{what}: {item:?} at {x}");
            }
            let times = head
                .labels
                .iter()
                .filter(|l| l.label == Label::Time)
                .count();
            assert_eq!(times, 1, "{what}: one time label");
        };
        let faces = [hn(), FaceMetrics::test_stack()];
        let widths: Vec<(usize, u32)> = (0..faces.len())
            .flat_map(|face| (160..=5_120).map(move |w| (face, w)))
            .collect();
        // `header_folded` reads only the blocks and `ground_only`: plans equal but for `w` head
        // alike.
        let sans_w = |p: FoldedPlan| FoldedPlan { w: 0.0, ..p };
        aterm_spec::check::each(&widths, |&(face, w)| {
            let m = &faces[face];
            let w = f64::from(w);
            // Each whole width, and the half point past it where that plans otherwise.
            let (full, past) = (plan_full(w, m), plan_full(w + 0.5, m));
            let fulls = if past == full {
                vec![(full, w)]
            } else {
                vec![(full, w), (past, w + 0.5)]
            };
            for (full, w) in fulls.into_iter().filter(|(p, _)| !p.ground_only) {
                check(
                    &header_full(&full, TEXT, None, m),
                    m,
                    false,
                    &format!("Full W {w}"),
                );
                // A state over the names may take the key's room there.
                let said = full.details.is_none();
                check(
                    &header_full(&full, TEXT, Some(long), m),
                    m,
                    said,
                    &format!("Full W {w}, a state"),
                );
            }
            // A Folded plan equal but for `w` to the width before it was headed there.
            let (folded, past) = (plan_folded(w, m), plan_folded(w + 0.5, m));
            let mut folds = Vec::new();
            if w == 160.0 || sans_w(plan_folded(w - 1.0, m)) != sans_w(folded) {
                folds.push((folded, w));
            }
            if sans_w(past) != sans_w(folded) {
                folds.push((past, w + 0.5));
            }
            let states: [[Option<StateLabel<'_>>; 2]; 5] = [
                [None, None],
                [Some(long), None],
                [None, Some(short)],
                [Some(short), Some(long)],
                [Some(long), Some(long)],
            ];
            for (folded, w) in folds.into_iter().filter(|(p, _)| !p.ground_only) {
                for pair in states {
                    let both = pair.iter().all(Option::is_some);
                    let head = header_folded(&folded, TEXT, pair, m);
                    check(&head, m, both, &format!("Folded W {w}"));
                }
            }
        });
    }

    /// Mark preference never makes an unmarked strip into a scope-legend span. State words
    /// may occupy a block's names or strip header; the key still uses the free room it has.
    #[test]
    fn folded_over_keys_follow_the_marked_blocks_with_occupied_headers() {
        let state = StateLabel {
            full: "Disk emergency, out of file slots +3",
            bare: "Disk emergency, out of file slots",
        };
        for m in [hn(), FaceMetrics::test_stack()] {
            for w in [1119.0, 1039.0, 940.0, 750.0, 550.0, 350.0] {
                let plan = plan_folded(w, &m);
                for over in [[true, false], [false, true], [true; 2], [false; 2]] {
                    for states in [
                        [None, None],
                        [Some(state), None],
                        [None, Some(state)],
                        [Some(state), Some(state)],
                    ] {
                        let head = header_folded(&plan, HeaderText { over, ..TEXT }, states, &m);
                        let at = format!("W {w}, marks {over:?}, states {states:?}");
                        let width = KeyItem::Over.width(&m);
                        let x = placed(&head, KeyItem::Over);
                        let fits = head.key.spans.iter().any(|s| s.x1 - s.x0 >= width);
                        assert_eq!(x.is_some(), over.contains(&true) && fits, "{at}");
                        for span in &head.key.spans {
                            assert!(!span.over || span.strip, "{at}: {span:?}");
                            if span.strip {
                                let bi = usize::from(span.x0 >= plan.divider);
                                assert_eq!(span.over, over[bi], "{at}: {span:?}");
                            }
                        }
                        let marked_room = head
                            .key
                            .spans
                            .iter()
                            .any(|s| s.over && s.x1 - s.x0 >= width);
                        if let Some(x) = x {
                            assert!(
                                head.key.spans.iter().any(|s| {
                                    x >= s.x0 && x + width <= s.x1 && (!marked_room || s.over)
                                }),
                                "{at}: over max at {x}"
                            );
                        }
                    }
                }
            }
        }
    }

    const GIB: u64 = 1 << 30;

    /// This Mac (A1 §1.3): 4 Super and 6 Efficiency cores, a 10-core GPU and its Neural Engine,
    /// 32 GiB and a disk reader.
    fn this_mac() -> UnitFacts<'static> {
        UnitFacts {
            clusters: &[4, 6],
            gpu: Some(Gpu {
                cores: Some(10),
                neural_engine: true,
            }),
            memory: Some(32 * GIB),
            disk: true,
        }
    }

    /// The machines the planner's units laws sweep: this Mac with its Neural Engine and without
    /// it, a 12 + 4 core Max with a 40-core GPU, a 24 + 8 core Ultra with 80 GPU cores, three
    /// clusters, a GPU with no core count, a one-cluster Intel Mac whose GPU reads 0 cores, a
    /// 128-core Linux box with no GPU reading, and machines that read one row alone.
    fn unit_machines() -> Vec<UnitFacts<'static>> {
        let mac = this_mac();
        let gpu = |cores, neural_engine| {
            Some(Gpu {
                cores,
                neural_engine,
            })
        };
        vec![
            mac,
            UnitFacts {
                gpu: gpu(Some(10), false),
                ..mac
            },
            UnitFacts {
                clusters: &[12, 4],
                gpu: gpu(Some(40), true),
                memory: Some(64 * GIB),
                disk: true,
            },
            UnitFacts {
                clusters: &[24, 8],
                gpu: gpu(Some(80), true),
                memory: Some(192 * GIB),
                disk: true,
            },
            UnitFacts {
                clusters: &[5, 6, 4],
                gpu: gpu(Some(20), true),
                memory: Some(48 * GIB),
                disk: true,
            },
            UnitFacts {
                clusters: &[4, 4],
                gpu: gpu(None, false),
                memory: Some(8 * GIB),
                disk: true,
            },
            UnitFacts {
                clusters: &[16],
                gpu: gpu(Some(0), false),
                memory: Some(16 * GIB),
                disk: true,
            },
            UnitFacts {
                clusters: &[128],
                gpu: None,
                memory: Some(512 * GIB),
                disk: true,
            },
            UnitFacts {
                disk: true,
                ..UnitFacts::NONE
            },
            UnitFacts {
                gpu: gpu(None, true),
                ..UnitFacts::NONE
            },
        ]
    }

    /// How far down its row's ladder `form` stands: 0 for the widest, the ladder's length for none.
    fn rung(row: UnitRow, form: Option<Form>) -> usize {
        form.and_then(|f| row.ladder().iter().position(|&g| g == f))
            .unwrap_or(row.ladder().len())
    }

    /// Each of block A's rows' [`rung`] in `p`.
    fn rungs(p: &FoldedPlan) -> [usize; 4] {
        let forms = p.blocks[0].units.map_or([None; 4], |units| units.forms);
        std::array::from_fn(|i| rung(UnitRow::ALL[i], forms[i]))
    }

    /// What breaks A1 §2.4's monotone law in `seq`, plans in widening order, each its width, its
    /// rows' [`rung`]s and its blocks' horizons: a row whose form steps down its ladder, or a
    /// block whose strip's horizon shortens.
    fn widening_faults(seq: &[(f64, [usize; 4], [u32; 2])]) -> Vec<String> {
        seq.windows(2)
            .filter_map(|pair| {
                let [(w0, r0, h0), (w1, r1, h1)] = pair else {
                    return None;
                };
                let rows = (0..4)
                    .filter(|&i| r1[i] > r0[i])
                    .map(|i| format!("{:?} {}→{}", UnitRow::ALL[i], r0[i], r1[i]));
                let horizons = (0..2)
                    .filter(|&i| h1[i] < h0[i])
                    .map(|i| format!("block {i} {}→{} s", h0[i], h1[i]));
                let all: Vec<String> = rows.chain(horizons).collect();
                (!all.is_empty()).then(|| format!("{w0}→{w1} pt: {}", all.join(", ")))
            })
            .collect()
    }

    /// A1 §2.2 and §2.3's points for this Mac: at 1039 pt block A lays its units 148–194 (U 46
    /// within a U_cap of 54), the % 197–227, a six-hour strip of 49 boxes 234–478 and the bar
    /// 485–541; at 1440 pt the same units and % (U_cap 254: no form grows past Column, ruling
    /// 17), a strip of 81 boxes 234–638 and the bar 645–741. Every edge stands on whole device px
    /// at 1× and 2×, and block B is the plan without units. With no facts block A is the plan the
    /// deck paints, its % at 148 and its strip of 57 boxes at 185: planned units are inert until
    /// A1's step R6 paints them.
    #[test]
    fn folded_1039_and_1440_plan_units_percent_strip_and_bar_at_their_points() {
        let m = hn();
        let mac = this_mac();
        for (w, b_at, cap, cols, strip_end, bar) in [
            (
                1039.0,
                Col::new(570.0, 457.0),
                54.0,
                49,
                478.0,
                Col::new(485.0, 56.0),
            ),
            (
                1440.0,
                Col::new(770.0, 657.0),
                254.0,
                81,
                638.0,
                Col::new(645.0, 96.0),
            ),
        ] {
            let p = plan_folded_with(w, &m, &mac);
            assert_eq!(
                plan_folded_with((2.0 * w + 1.0) / 2.0, &m, &mac),
                p,
                "W {w}"
            );
            let bare = plan_folded(w, &m);
            assert_eq!(
                (p.blocks[1], p.divider, p.gauge),
                (bare.blocks[1], bare.divider, bare.gauge),
                "W {w}"
            );
            assert_eq!((p.blocks[1].at, p.blocks[1].units), (b_at, None), "W {w}");
            let a = p.blocks[0];
            assert_eq!(
                a.units,
                Some(UnitsCol {
                    at: Col::new(148.0, 46.0),
                    cap,
                    forms: [Some(Form::Column); 4],
                }),
                "W {w}"
            );
            assert_eq!(
                (a.kind, a.name, a.well, a.pct),
                (
                    BlockKind::Wide,
                    Col::new(84.0, 52.0),
                    Col::new(136.0, 10.0),
                    Some(Col::new(197.0, 30.0))
                ),
                "W {w}"
            );
            assert_eq!(
                (a.strip.x, a.strip.cols, a.strip.end(), a.strip.span_s()),
                (234.0, cols, strip_end, 21_600),
                "W {w}"
            );
            assert_eq!(
                (a.meter.bar, a.meter.run, a.details),
                (Some(bar), None, None)
            );
            assert_eq!(bar.end(), a.at.end(), "W {w}: the bar ends the block");
            assert_on_pixels(&block_cols(&a), &format!("W {w}"));
            assert_in_order(&block_cols(&a), a.at.x, a.at.end(), &format!("W {w}"));
        }
        let a = plan_folded(1039.0, &m).blocks[0];
        assert_eq!(
            (a.units, a.pct, a.strip.x, a.strip.cols, a.meter.bar),
            (
                None,
                Some(Col::new(148.0, 30.0)),
                185.0,
                57,
                Some(Col::new(476.0, 65.0))
            )
        );
        assert_eq!(
            plan_folded_with(1039.0, &m, &UnitFacts::NONE),
            plan_folded(1039.0, &m)
        );
    }

    /// A1 §2.4's ladder by width for this Mac, and A2 §3.5's with its Neural Engine: U_cap is
    /// b − 403 for a block A b pt wide, so a form U pt wide draws from W = 2U + 931 pt. Widening
    /// from 900 pt, the GPU's undivided tank comes at 947 pt (957 with the tile), the CPU's
    /// Cluster at 959, Disk's two tiles' place at 969, the CPU's Stack at 977, Memory's Half at
    /// 983, the GPU's Column at 993 (1003), the CPU's Column at 1011 and Memory's waffle at 1023.
    /// At the owner's 940 pt no units draw.
    #[test]
    fn this_macs_forms_step_up_where_the_design_says() {
        use Form::{Cluster, Column, Half, Stack, Undivided};
        use UnitRow::{Cpu, Disk, Memory};
        let m = hn();
        let steps = |facts: &UnitFacts<'_>| {
            let mut was = [None; 4];
            let mut steps = Vec::new();
            for w in (900..=1100).map(f64::from) {
                let a = plan_folded_with(w, &m, facts).blocks[0];
                if let Some(units) = a.units {
                    assert_eq!(units.cap, a.at.w - 403.0, "W {w}");
                }
                let forms = a.units.map_or([None; 4], |units| units.forms);
                for (i, form) in forms.into_iter().enumerate() {
                    if form != was[i] {
                        steps.push((w, UnitRow::ALL[i], form));
                        was[i] = form;
                    }
                }
            }
            steps
        };
        let mac = this_mac();
        let without = UnitFacts {
            gpu: Some(Gpu {
                cores: Some(10),
                neural_engine: false,
            }),
            ..mac
        };
        assert_eq!(
            steps(&without),
            [
                (947.0, UnitRow::Gpu, Some(Undivided)),
                (959.0, Cpu, Some(Cluster)),
                (969.0, Disk, Some(Column)),
                (977.0, Cpu, Some(Stack)),
                (983.0, Memory, Some(Half)),
                (993.0, UnitRow::Gpu, Some(Column)),
                (1011.0, Cpu, Some(Column)),
                (1023.0, Memory, Some(Column)),
            ]
        );
        assert_eq!(
            steps(&mac),
            [
                (957.0, UnitRow::Gpu, Some(Undivided)),
                (959.0, Cpu, Some(Cluster)),
                (969.0, Disk, Some(Column)),
                (977.0, Cpu, Some(Stack)),
                (983.0, Memory, Some(Half)),
                (1003.0, UnitRow::Gpu, Some(Column)),
                (1011.0, Cpu, Some(Column)),
                (1023.0, Memory, Some(Column)),
            ]
        );
        assert_eq!(plan_folded_with(940.0, &m, &mac).blocks[0].units, None);
        // A 12 + 4 core Max takes Stack at 1039 pt and a 24 + 8 core Ultra Cluster (A1 §4.1).
        for (clusters, form) in [(&[12_usize, 4][..], Stack), (&[24, 8][..], Cluster)] {
            let facts = UnitFacts { clusters, ..mac };
            let units = plan_folded_with(1039.0, &m, &facts).blocks[0].units;
            assert_eq!(units.and_then(|u| u.forms[0]), Some(form), "{clusters:?}");
        }
    }

    /// A1 §2.4 and ruling 11: the units column never costs block A's strip its preset, its kind
    /// or its bar column, at any whole width from 160 to 3200 pt, for any machine, on either face.
    /// Units draw only where block A's strip holds the top preset without them, every form taken
    /// is within U_cap, and the column is as wide as the widest of them. U_cap is tight: block A
    /// laid with a column up to U_cap keeps the top preset, and with a column a point wider loses
    /// it (CONTROL). Block B, the gauge and the divider never move.
    #[test]
    fn the_units_column_never_costs_the_strip_its_preset() {
        let top = Strip::spec_for(f64::INFINITY);
        let mut drawn = 0;
        for m in [hn(), FaceMetrics::test_stack()] {
            for facts in unit_machines() {
                for w in (160..=3200).map(f64::from) {
                    let bare = plan_folded(w, &m);
                    let p = plan_folded_with(w, &m, &facts);
                    let what = || format!("{facts:?} at {w} pt");
                    assert_eq!(
                        (p.blocks[1], p.gauge, p.divider, p.ground_only),
                        (bare.blocks[1], bare.gauge, bare.divider, bare.ground_only),
                        "{}",
                        what()
                    );
                    let (a, a0) = (p.blocks[0], bare.blocks[0]);
                    let Some(units) = a.units else {
                        assert_eq!(a, a0, "{}", what());
                        continue;
                    };
                    drawn += 1;
                    assert_eq!(
                        a0.strip.spec,
                        top,
                        "{}: units beside a shorter horizon",
                        what()
                    );
                    assert_eq!(
                        (a.kind, a.at, a.strip.spec),
                        (a0.kind, a0.at, top),
                        "{}",
                        what()
                    );
                    assert!(
                        a.meter.bar.is_some() && a.strip.end() <= a.at.end(),
                        "{}",
                        what()
                    );
                    let widths: Vec<f64> = UnitRow::ALL
                        .into_iter()
                        .zip(units.forms)
                        .filter_map(|(row, form)| facts.width(row, form?, PLAN_SCALE, a.rows.cell))
                        .collect();
                    assert!(widths.iter().all(|&fw| fw <= units.cap), "{}", what());
                    assert_eq!(widths.iter().copied().fold(0.0, f64::max), units.at.w);
                    for (u, keeps) in [
                        (0.0, true),
                        (units.at.w, true),
                        (units.cap, true),
                        (units.cap + 1.0, false),
                    ] {
                        let (laid, fits) = lay_block(a.at.x, a.at.w, a.kind, &m, true, u);
                        let kept = fits
                            && laid.kind == a.kind
                            && laid.meter.bar.is_some()
                            && laid.strip.spec == top;
                        assert_eq!(kept, keeps, "{}: a column of {u} pt", what());
                    }
                }
            }
        }
        assert!(drawn > 0, "CONTROL: no plan drew units");
    }

    /// A1 §2.4: as the deck widens a point at a time from 160 to 3200 pt, no row's form steps down
    /// its ladder, and the units column makes no strip's horizon shorten, for any machine, on
    /// either face. From the first width whose block A holds six hours nothing steps down at all.
    /// Below it, the planner without units shortens a horizon at a few widths of its own (A1 §2.4's
    /// as-built note): the units change none of them. CONTROL: U_cap as A1 first wrote it,
    /// keeping whatever preset block A's bare strip holds, leaves this Mac 122 pt at 924 pt, where
    /// the strip holds four hours, so every row draws its Column there, and nothing at 925 pt,
    /// where six hours leave −3; the same check finds the step down.
    #[test]
    fn a_wider_deck_never_draws_a_shorter_horizon_or_a_smaller_unit_form() {
        let top = Strip::spec_for(f64::INFINITY);
        for m in [hn(), FaceMetrics::test_stack()] {
            let plans = |facts: &UnitFacts<'_>| -> Vec<_> {
                (160..=3200)
                    .map(f64::from)
                    .map(|w| {
                        let p = plan_folded_with(w, &m, facts);
                        (w, rungs(&p), p.blocks.map(|b| b.strip.span_s()))
                    })
                    .collect()
            };
            let bare = plans(&UnitFacts::NONE);
            let bare_faults = widening_faults(&bare);
            let six_hours = (160..=3200)
                .map(f64::from)
                .position(|w| plan_folded(w, &m).blocks[0].strip.spec == top)
                .expect("six hours somewhere");
            for facts in unit_machines() {
                let seq = plans(&facts);
                assert_eq!(widening_faults(&seq), bare_faults, "{facts:?}");
                let faults = widening_faults(&seq[six_hours..]);
                assert!(faults.is_empty(), "{facts:?}: {faults:?}");
            }
        }

        let m = hn();
        let mac = this_mac();
        let first_rule = |w: f64| {
            let p = plan_folded(w, &m);
            let a = p.blocks[0];
            let spec = fold_spec(a.kind);
            let room = a.at.end() - a.strip.x - UNITS_TO_PCT;
            let keeps = |u: f64| {
                share_meter(room - u, &spec).is_some_and(|(cols, _, _)| {
                    Strip::spec_for(f64::from(cols) * spec.pitch - 1.0) == a.strip.spec
                })
            };
            // The widest whole column the preset leaves room for, short of none where it is
            // negative; the rule takes a column only where it is not.
            let slack = (-20..=room as i32).rev().map(f64::from).find(|&u| keeps(u));
            let cap = slack.filter(|&u| u >= 0.0);
            let forms = cap.map_or([None; 4], |cap| mac.forms_within(cap, a.rows.cell));
            let rungs = std::array::from_fn(|i| rung(UnitRow::ALL[i], forms[i]));
            ((slack, cap), (w, rungs, p.blocks.map(|b| b.strip.span_s())))
        };
        let (below, above) = (first_rule(924.0), first_rule(925.0));
        assert_eq!(
            (below.0, above.0),
            ((Some(122.0), Some(122.0)), (Some(-3.0), None))
        );
        assert_eq!((below.1.1, above.1.1), ([0; 4], [3, 2, 2, 1]));
        let faults = widening_faults(&[below.1, above.1]);
        assert_eq!(faults.len(), 1, "CONTROL: {faults:?}");
    }

    /// A1 §2.4: the column stands where the % stood, [`UNITS_TO_PCT`] short of the % now, and
    /// every row's form fits it at 1× and at 2× (where a hairline is half a point, so no form is
    /// wider), right-aligned against its %: each form's left edge inside the column and on a whole
    /// device px at both scales, and every column of the block in order on whole device px.
    /// Full's half of A1's law is step R5's.
    #[test]
    fn units_fit_their_column_at_every_folded_width_at_1x_and_2x() {
        let mut seen = 0;
        for m in [hn(), FaceMetrics::test_stack()] {
            for facts in unit_machines() {
                for w in (160..=3200).map(f64::from) {
                    let a = plan_folded_with(w, &m, &facts).blocks[0];
                    let Some(units) = a.units else { continue };
                    let what = || format!("{facts:?} at {w} pt");
                    let pct = a.pct.expect("a % column beside the units");
                    assert_eq!(
                        (units.at.x, units.at.end() + UNITS_TO_PCT),
                        (a.well.end() + 2.0, pct.x),
                        "{}",
                        what()
                    );
                    assert_in_order(&block_cols(&a), a.at.x, a.at.end(), &what());
                    assert_on_pixels(&block_cols(&a), &what());
                    for (row, form) in UnitRow::ALL.into_iter().zip(units.forms) {
                        let Some(form) = form else { continue };
                        for s in [1.0, 2.0] {
                            let fw = facts.width(row, form, s, a.rows.cell).expect("drawn");
                            let x = units.at.end() - fw;
                            assert!(x >= units.at.x, "{}: {row:?} {form:?} at {s}×", what());
                            assert_eq!(x * s, (x * s).round(), "{}: {row:?} at {s}×", what());
                            seen += 1;
                        }
                    }
                }
            }
        }
        assert!(seen > 0, "CONTROL: no form was laid");
    }

    /// A2 §3.5 (S5's planner half): the Neural Engine tile and its clear are 5 pt beside the tank,
    /// at 1× and 2×, in both the GPU's forms, and the forms ladder takes them: this Mac's GPU
    /// draws its undivided tank from 957 pt and its Column from 1003 (947 and 993 without the
    /// tile), its form never steps down as the deck widens to 1440 pt, and U stays Memory's 46 at
    /// 1039 pt. With no GPU the tile takes no space. CONTROL: a ladder that measured the tank
    /// alone would draw this Mac's Column at 993 pt, where the tile and the tank take 36 pt and
    /// U_cap is 31.
    #[test]
    fn the_forms_ladder_takes_the_tiles_width_monotonically() {
        let m = hn();
        let mac = this_mac();
        let tank = UnitFacts {
            gpu: Some(Gpu {
                cores: Some(10),
                neural_engine: false,
            }),
            ..mac
        };
        let cell = Rows::folded(BlockKind::Wide).cell;
        for form in [Form::Column, Form::Undivided] {
            for s in [1.0, 2.0] {
                let with = mac.width(UnitRow::Gpu, form, s, cell).expect("a tank");
                let alone = tank.width(UnitRow::Gpu, form, s, cell).expect("a tank");
                assert_eq!(with - alone, 5.0, "{form:?} at {s}×");
            }
        }
        let gpu_form = |facts: &UnitFacts<'_>, w: f64| {
            plan_folded_with(w, &m, facts).blocks[0]
                .units
                .and_then(|units| units.forms[1])
        };
        let first = |facts: &UnitFacts<'_>, form: Form| {
            (900..=1100)
                .map(f64::from)
                .find(|&w| gpu_form(facts, w) == Some(form))
        };
        let forms = [Form::Undivided, Form::Column];
        assert_eq!(forms.map(|f| first(&mac, f)), [Some(957.0), Some(1003.0)]);
        assert_eq!(forms.map(|f| first(&tank, f)), [Some(947.0), Some(993.0)]);
        let mut last = usize::MAX;
        for w in (900..=1440).map(f64::from) {
            let r = rung(UnitRow::Gpu, gpu_form(&mac, w));
            assert!(
                r <= last,
                "{w} pt steps the GPU down to {:?}",
                gpu_form(&mac, w)
            );
            last = r;
        }
        let at_1039 = plan_folded_with(1039.0, &m, &mac).blocks[0].units;
        assert_eq!(at_1039.map(|units| units.at.w), Some(46.0));
        let gpuless = UnitFacts { gpu: None, ..mac };
        for form in [Form::Column, Form::Undivided] {
            assert_eq!(gpuless.width(UnitRow::Gpu, form, 1.0, cell), None);
        }

        let at_993 = plan_folded_with(993.0, &m, &mac).blocks[0]
            .units
            .expect("units at 993 pt");
        assert_eq!((at_993.cap, at_993.forms[1]), (31.0, Some(Form::Undivided)));
        assert_eq!(gpu_form(&tank, 993.0), Some(Form::Column), "CONTROL");
        let column = mac.width(UnitRow::Gpu, Form::Column, PLAN_SCALE, cell);
        assert_eq!(column, Some(36.0), "CONTROL");
    }

    /// A1 §2.4 and §4.4: the front tab's device, which comes and goes with the tab, never moves
    /// block A's columns or drops a row's form. Wherever block A takes the Disk form, at every
    /// whole width from 160 to 3200 pt, for every machine, on either face, the column holds
    /// [`DISK_UNITS`] square tiles a point apart at 1× and at 2×, home's and the second's place,
    /// and the plan reads no count of the tiles a reading draws ([`UnitFacts::disk`]), so a second
    /// device draws inside the column the plan laid. CONTROL: on this Mac a form as wide as the
    /// tiles drawn would take Disk with home's tile alone at 949–968 pt, where U_cap leaves no
    /// room for a second, and lay U at 14 pt at 969–976 pt, where two tiles take 19: a second
    /// device would there drop the row's form or move the % and the strip 5 pt.
    #[test]
    fn a_front_tab_on_another_device_moves_no_column() {
        use aterm_vitals::units::DISK_UNITS;
        // DISK_UNITS square tiles, each the cell's whole device px, a point apart (A1 §4.4).
        let place = |cell: f64, s: f64| {
            let n = DISK_UNITS as f64;
            n * (cell * s).ceil() / s + (n - 1.0)
        };
        let mut seen = 0;
        for m in [hn(), FaceMetrics::test_stack()] {
            for facts in unit_machines() {
                for w in (160..=3200).map(f64::from) {
                    let a = plan_folded_with(w, &m, &facts).blocks[0];
                    let Some(units) = a.units else { continue };
                    if units.forms[3] != Some(Form::Column) {
                        continue;
                    }
                    let cell = a.rows.cell;
                    for s in [1.0, 2.0] {
                        let disk = facts.width(UnitRow::Disk, Form::Column, s, cell);
                        assert_eq!(disk, Some(place(cell, s)), "{facts:?} at {w} pt, {s}×");
                    }
                    assert!(
                        place(cell, PLAN_SCALE) <= units.at.w,
                        "{facts:?} at {w} pt: {units:?}"
                    );
                    seen += 1;
                }
            }
        }
        assert!(seen > 0, "CONTROL: no plan took the Disk form");

        let m = hn();
        let mac = this_mac();
        let cell = Rows::folded(BlockKind::Wide).cell;
        let others = UnitFacts { disk: false, ..mac };
        let (home, two) = (cell.ceil(), place(cell, PLAN_SCALE));
        // The column a form of `disk` pt would lay beside the other rows' forms within `cap`.
        let laid = |cap: f64, disk: f64| {
            UnitRow::ALL
                .into_iter()
                .zip(others.forms_within(cap, cell))
                .filter_map(|(row, form)| others.width(row, form?, PLAN_SCALE, cell))
                .chain((disk <= cap).then_some(disk))
                .fold(0.0, f64::max)
        };
        let (mut drops, mut moves) = (Vec::new(), Vec::new());
        for w in 900..=1100 {
            let Some(cap) = units_cap(&plan_folded(f64::from(w), &m).blocks[0]) else {
                continue;
            };
            let units = plan_folded_with(f64::from(w), &m, &mac).blocks[0].units;
            let planned = units.map_or(0.0, |units| units.at.w);
            assert_eq!(planned, laid(cap, two), "{w} pt");
            if home <= cap && two > cap {
                drops.push(w);
            } else if laid(cap, home) != planned {
                moves.push(w);
            }
        }
        assert_eq!(drops, (949..=968).collect::<Vec<_>>(), "CONTROL");
        assert_eq!(moves, (969..=976).collect::<Vec<_>>(), "CONTROL");
    }

    /// A1 §2.4's as-built note: block A's Details give way to its units, as [`lay_meter`] gives
    /// Details way to a row's run and its strip. Past the units column block A is the block laid
    /// bare U + 3 pt narrower, its strip, meter and Details moved right by the column, at every
    /// whole width from 160 to 3200 pt, for every machine, on either face, and block B lays its
    /// own as with no units; so block A may lay no Details where block B lays them. CONTROL: on
    /// Helvetica Neue block B lays Details and this Mac's block A none at 1825–1878 and
    /// 2243–2340 pt, and block A lays them and block B none at 1923–1976 pt: a rule that made
    /// the two blocks agree fails the check there.
    #[test]
    fn block_as_units_cost_its_details_their_width_and_block_b_nothing() {
        let (mut b_only, mut a_only) = (Vec::new(), Vec::new());
        for (face, m) in [("hn", hn()), ("stack", FaceMetrics::test_stack())] {
            for (i, facts) in unit_machines().into_iter().enumerate() {
                for w in 160..=3200 {
                    let p = plan_folded_with(f64::from(w), &m, &facts);
                    let [a, b] = p.blocks;
                    let Some(units) = a.units else { continue };
                    let what = || format!("{facts:?} at {w} pt on {face}");
                    let shift = units.at.w + UNITS_TO_PCT;
                    let (bare, fits) = lay_block(a.at.x, a.at.w - shift, a.kind, &m, true, 0.0);
                    assert!(fits, "{}", what());
                    let moved = |col: Col| Col::new(col.x + shift, col.w);
                    assert_eq!(
                        (a.strip, a.meter.bar, a.meter.run, a.details),
                        (
                            Strip {
                                x: bare.strip.x + shift,
                                ..bare.strip
                            },
                            bare.meter.bar.map(moved),
                            bare.meter.run.map(moved),
                            bare.details.map(moved)
                        ),
                        "{}",
                        what()
                    );
                    if (face, i) == ("hn", 0) {
                        match (a.details.is_some(), b.details.is_some()) {
                            (false, true) => b_only.push(w),
                            (true, false) => a_only.push(w),
                            _ => {}
                        }
                    }
                }
            }
        }
        let want: Vec<i32> = (1825..=1878).chain(2243..=2340).collect();
        assert_eq!(b_only, want, "CONTROL");
        assert_eq!(a_only, (1923..=1976).collect::<Vec<_>>(), "CONTROL");
    }
}
