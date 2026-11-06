// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

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

/// Per-thread outline, `gvar` delta, `COLR` paint-table and shaping step counts since the last
/// [`take_spent`].
#[cfg(all(any(test, debug_assertions), not(trust_verify)))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spent {
    /// Outline steps requested, including any refused by the budget.
    pub outline_steps: u64,
    /// `gvar` delta steps requested, including any refused by the budget.
    pub variation_steps: u64,
    /// `COLR` paint tables visited, including any refused by the budget.
    pub paint_tables: u64,
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
