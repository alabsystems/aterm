// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use crate::parser::NumFrom;

/// Steps one outline query's structure may take: a glyph entry, a TrueType
/// point or a CFF operator each.
const OUTLINE_STEPS: u32 = 65_536;

/// Steps one outline query's `gvar` deltas may take: one per tuple header,
/// and for each tuple that applies, one per point it moves, phantom points
/// included. Composite-heavy variable faces spend over a million at interior
/// instances (Handjet's 1,605 glyphs up to 1,214,281); a glyph of 65,535
/// points under 4,095 tuples would spend 268 million.
pub(crate) const VARIATION_STEPS: u32 = 1 << 23;

/// Cumulative expansion budget for a single outline query: its structure
/// against [`OUTLINE_STEPS`], its `gvar` deltas against [`VARIATION_STEPS`].
pub(crate) struct OutlineWork {
    steps: Option<u32>,
    variation: VariationWork,
}

impl Default for OutlineWork {
    #[inline]
    fn default() -> Self {
        Self {
            steps: Some(OUTLINE_STEPS),
            variation: VariationWork::default(),
        }
    }
}

impl OutlineWork {
    #[inline]
    pub(crate) fn take(&mut self) -> Option<()> {
        #[cfg(all(any(test, debug_assertions), not(trust_verify)))]
        note(|s| s.outline_steps += 1);
        self.steps = self.steps?.checked_sub(1);
        self.steps.map(|_| ())
    }

    /// Charge `steps` of `gvar` delta work ([`VariationWork::take`]).
    #[inline]
    pub(crate) fn vary(&mut self, steps: usize) -> Option<()> {
        self.variation.take(steps)
    }

    #[inline]
    pub(crate) const fn exhausted(&self) -> bool {
        self.steps.is_none() || self.variation.exhausted()
    }

    /// The structure and `gvar` delta steps this query has taken: all of a
    /// budget it ran out of.
    #[inline]
    pub(crate) fn spent(&self) -> (usize, usize) {
        let steps = OUTLINE_STEPS.saturating_sub(self.steps.unwrap_or(0));
        let variation = VARIATION_STEPS.saturating_sub(self.variation.0.unwrap_or(0));
        (usize::num_from(steps), usize::num_from(variation))
    }
}

/// The [`VARIATION_STEPS`] budget of one glyph's `gvar` deltas, charged for
/// work whose size is known before it starts, so a refused charge refuses
/// before any of it runs.
pub(crate) struct VariationWork(Option<u32>);

impl Default for VariationWork {
    #[inline]
    fn default() -> Self {
        Self(Some(VARIATION_STEPS))
    }
}

impl VariationWork {
    #[inline]
    pub(crate) fn take(&mut self, steps: usize) -> Option<()> {
        #[cfg(all(any(test, debug_assertions), not(trust_verify)))]
        note(|s| {
            s.variation_steps = s
                .variation_steps
                .saturating_add(u64::try_from(steps).unwrap_or(u64::MAX));
        });
        let steps = u32::try_from(steps).ok();
        self.0 = self
            .0
            .zip(steps)
            .and_then(|(left, steps)| left.checked_sub(steps));
        self.0.map(|_| ())
    }

    #[inline]
    pub(crate) const fn exhausted(&self) -> bool {
        self.0.is_none()
    }
}

/// Work one colour glyph's `COLR` paint walk may hand its painter: a paint
/// table or a v0 layer one unit, a gradient one more per colour stop, and an
/// outline [`PAINT_OUTLINE`] more. Noto's `COLR` v1 build spends at most
/// 19,816 (gid 3663: 6,665 tables, 23 stops, 3,282 outlines).
const PAINT_STEPS: u32 = 1 << 16;

/// Units of [`PAINT_STEPS`] an outline handed to the painter costs: the
/// painter outlines and fills the glyph, the heaviest thing it does.
const PAINT_OUTLINE: usize = 4;

/// Segments the outlines one colour glyph's paint walk hands its painter may
/// hold in all, each outline counted by outlining it: as many as four whole
/// outline queries have steps ([`OUTLINE_STEPS`] each). Noto's `COLR` v1
/// build holds at most 57,352 (gid 3812, 3,066 outlines).
const PAINT_SEGMENTS: u32 = 1 << 18;

/// Steps the outline queries of those outlines may take in all: each costs
/// what the walk's query to count its segments took, as the painter's query
/// to fill it takes again. As many as four whole queries may take
/// ([`OUTLINE_STEPS`] each); Noto's `COLR` v1 build takes at most 78,970
/// (gid 3812).
const PAINT_OUTLINE_STEPS: u32 = 4 * OUTLINE_STEPS;

/// Their `gvar` delta steps in all, likewise four whole queries'
/// ([`VARIATION_STEPS`] each).
const PAINT_OUTLINE_VARIATION: u32 = 4 * VARIATION_STEPS;

/// Region-axis evaluations the item variation deltas of one colour glyph's
/// paint walk may make in all: those its variable paint tables and clip
/// boxes read, and the two each variable colour stop it hands out holds,
/// which the painter reads, all at the instance it paints. As many as 16
/// whole deltas may make (`DELTA_STEPS` each). Over the variable `COLR`
/// faces Nabla, Honk, Foldit, Kalnia Glaze and `colr_1_variable.ttf` at their
/// default, least and greatest instances, a walk makes at most 2,288
/// (`colr_1_variable.ttf` gid 156, of 44 axes; Foldit 79).
const PAINT_DELTA_STEPS: u32 = 1 << 20;

/// The [`PAINT_STEPS`], [`PAINT_SEGMENTS`], [`PAINT_OUTLINE_STEPS`],
/// [`PAINT_OUTLINE_VARIATION`] and [`PAINT_DELTA_STEPS`] budgets of one
/// colour glyph's paint walk, charged before the walk or its painter does
/// the work, so a refused charge refuses before the work runs. The walk
/// learns an outline's segments and steps by outlining it, so its own query
/// may run one past them. One refusal spends them all: the walk stops there.
pub(crate) struct PaintWork {
    steps: Option<u32>,
    segments: Option<u32>,
    outline_steps: Option<u32>,
    outline_variation: Option<u32>,
    deltas: Option<u32>,
}

impl Default for PaintWork {
    #[inline]
    fn default() -> Self {
        Self {
            steps: Some(PAINT_STEPS),
            segments: Some(PAINT_SEGMENTS),
            outline_steps: Some(PAINT_OUTLINE_STEPS),
            outline_variation: Some(PAINT_OUTLINE_VARIATION),
            deltas: Some(PAINT_DELTA_STEPS),
        }
    }
}

impl PaintWork {
    #[inline]
    pub(crate) fn take(&mut self, units: usize) -> Option<()> {
        #[cfg(all(any(test, debug_assertions), not(trust_verify)))]
        note(|s| {
            s.paint_work = s
                .paint_work
                .saturating_add(u64::try_from(units).unwrap_or(u64::MAX));
        });
        self.steps = spend(self.steps, units);
        self.settle()
    }

    /// Charge an outline: [`PAINT_OUTLINE`] units, then what `query` spends,
    /// which runs only once the units are granted: it outlines the glyph
    /// under a fresh [`OutlineWork`], as the painter will, and returns its
    /// segments, charged with the steps it took.
    #[inline]
    pub(crate) fn outline(&mut self, query: impl FnOnce(&mut OutlineWork) -> usize) -> Option<()> {
        self.take(PAINT_OUTLINE)?;
        let mut work = OutlineWork::default();
        let segments = query(&mut work);
        let (steps, variation) = work.spent();
        #[cfg(all(any(test, debug_assertions), not(trust_verify)))]
        note(|s| {
            s.paint_segments = s
                .paint_segments
                .saturating_add(u64::try_from(segments).unwrap_or(u64::MAX));
        });
        self.segments = spend(self.segments, segments);
        self.outline_steps = spend(self.outline_steps, steps);
        self.outline_variation = spend(self.outline_variation, variation);
        self.settle()
    }

    /// Charge `steps` region-axis evaluations of item variation deltas
    /// against [`PAINT_DELTA_STEPS`].
    #[inline]
    pub(crate) fn vary(&mut self, steps: usize) -> Option<()> {
        #[cfg(all(any(test, debug_assertions), not(trust_verify)))]
        note(|s| {
            s.paint_delta_steps = s
                .paint_delta_steps
                .saturating_add(u64::try_from(steps).unwrap_or(u64::MAX));
        });
        self.deltas = spend(self.deltas, steps);
        self.settle()
    }

    /// Whether a charge was refused, so the walk stopped short of the glyph.
    #[inline]
    pub(crate) const fn refused(&self) -> bool {
        self.steps.is_none()
    }

    #[inline]
    fn settle(&mut self) -> Option<()> {
        if self.steps.is_none()
            || self.segments.is_none()
            || self.outline_steps.is_none()
            || self.outline_variation.is_none()
            || self.deltas.is_none()
        {
            self.steps = None;
            self.segments = None;
            self.outline_steps = None;
            self.outline_variation = None;
            self.deltas = None;
            return None;
        }
        Some(())
    }
}

#[inline]
fn spend(left: Option<u32>, units: usize) -> Option<u32> {
    left.zip(u32::try_from(units).ok())
        .and_then(|(left, units)| left.checked_sub(units))
}

/// Per-table `GSUB` traversal budget shared across shaper and autohinter lookups.
pub(crate) struct LayoutWork {
    left: core::cell::Cell<u64>,
    refused: core::cell::Cell<bool>,
}

impl LayoutWork {
    /// Budget for `traversals` traversals of a table of `len` bytes.
    #[inline]
    pub(crate) const fn for_table(len: usize, traversals: u64) -> Self {
        let one = (len as u64).saturating_mul(8);
        let one = if one < 16_384 { 16_384 } else { one };
        Self {
            left: core::cell::Cell::new(one.saturating_mul(traversals)),
            refused: core::cell::Cell::new(false),
        }
    }

    /// Spend `units`, returning `None` and marking the table refused when exhausted.
    #[inline]
    pub(crate) fn spend(&self, units: u64) -> Option<()> {
        match self.left.get().checked_sub(units) {
            Some(left) if !self.refused.get() => {
                self.left.set(left);
                Some(())
            }
            _ => {
                self.refused.set(true);
                None
            }
        }
    }

    /// Whether a walk ran past the budget.
    #[cfg(any(feature = "hint", debug_assertions))]
    #[inline]
    pub(crate) fn refused(&self) -> bool {
        self.refused.get()
    }
}

/// Per-thread outline, `gvar` delta, `COLR` paint-table, paint-work, segment, item variation
/// delta and clip-record, and shaping step counts since the last [`take_spent`].
#[cfg(all(any(test, debug_assertions), not(trust_verify)))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spent {
    /// Outline steps requested, including any refused by the budget.
    pub outline_steps: u64,
    /// `gvar` delta steps requested, including any refused by the budget.
    pub variation_steps: u64,
    /// `COLR` paint tables visited, including any refused by the budget.
    pub paint_tables: u64,
    /// `COLR` paint work units charged (tables, v0 layers, colour stops and outlines), including
    /// any refused by the budget.
    pub paint_work: u64,
    /// Segments of the outlines a `COLR` walk charged, including any refused by the budget.
    pub paint_segments: u64,
    /// Region-axis evaluations of item variation deltas a `COLR` walk charged, including any
    /// refused by the budget.
    pub paint_delta_steps: u64,
    /// Region-axis evaluations item variation deltas made, wherever they were read.
    pub delta_steps: u64,
    /// `COLR` clip records read looking up a colour glyph's clip box.
    pub clip_records: u64,
    /// `GSUB`/`GPOS` work units charged by the shaper, including any refused by the budget.
    pub layout_ops: u64,
    /// Steps of `GPOS` attachment propagation: each glyph advance it reads (every read goes
    /// through one counted accessor) and each attachment link it applies.
    pub attachment_steps: u64,
    /// Glyphs `GSUB` copied to move between a rule's positions, a rewind's shift of the input
    /// included.
    pub glyphs_moved: u64,
    /// Glyphs `GSUB` passed walking a ligature's marks: back to the ligature to see whether a
    /// match may skip it, or on to renumber them after a new ligature.
    pub marks_walked: u64,
}

#[cfg(all(any(test, debug_assertions), not(trust_verify)))]
std::thread_local! {
    static SPENT: std::cell::Cell<Spent> = const {
        std::cell::Cell::new(Spent {
            outline_steps: 0,
            variation_steps: 0,
            paint_tables: 0,
            paint_work: 0,
            paint_segments: 0,
            paint_delta_steps: 0,
            delta_steps: 0,
            clip_records: 0,
            layout_ops: 0,
            attachment_steps: 0,
            glyphs_moved: 0,
            marks_walked: 0,
        })
    };
}

#[cfg(all(any(test, debug_assertions), not(trust_verify)))]
#[inline]
pub(crate) fn note(charge: impl FnOnce(&mut Spent)) {
    SPENT.with(|cell| {
        let mut spent = cell.get();
        charge(&mut spent);
        cell.set(spent);
    });
}

/// Return and reset this thread's [`Spent`] counters.
#[cfg(all(any(test, debug_assertions), not(trust_verify)))]
#[must_use]
pub fn take_spent() -> Spent {
    SPENT.with(|cell| cell.replace(Spent::default()))
}
