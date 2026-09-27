// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The seamless update's HISTORY CARRY, grid half (2026-09-26).
//!
//! An in-session update used to hand the successor at most 256 lines of each
//! tab's scrollback (`aterm-gui`'s `MAX_HANDOFF_HISTORY_LINES`), fewer under
//! deadline pressure: the rest of a 100,000-line ring stayed behind with the
//! old process. The carry that replaces it has three grid-level steps, and
//! they are here so the three share one notion of "the same history":
//!
//! * [`Grid::history_fence`] — what a history export is fenced on: the
//!   renumber epoch (a width reflow, an unscroll), the scrollback-clear
//!   generation (ED3, RIS), the reveal generation (a rows-grow that handed
//!   history back to the screen), the width, and the absolute rows the
//!   retained history spans. While all of that holds, every absolute row the
//!   fence covers names the SAME line it named at the fence (eviction only
//!   ever drops the oldest, and keeps every survivor's key).
//! * [`Grid::history_lines_since_fence`] — a bounded slice of that history by
//!   absolute row, refused the moment the fence no longer holds, so the
//!   outgoing process can export the whole history in short lock holds while
//!   its readers are still live.
//! * [`OlderHistory`] + [`Grid::attach_older_history`] — the successor's
//!   import: the exported lines are built into a tiered store OFF the lock,
//!   then placed BEFORE the oldest line the grid retains under one brief lock
//!   whose cost is the grid's own tiered history (right after an update: the
//!   few hundred lines the screen carry brought), never the imported depth.
//!
//! [`Grid::reserve_older_history_keys`] is the fourth, smaller piece: the
//! successor raises its absolute-row counter by the lines it expects to
//! import BEFORE anything reads an absolute row, so the import later lands
//! under keys nobody has used and the live screen's keys never move. It hands
//! back an [`OlderHistoryClaim`], the successor's own fence: the attach is
//! refused once the pane's scrollback was CLEARED after the reserve (ED3, a
//! reset), so an import that lands late never puts back history the user
//! erased.

use aterm_scrollback::{Line, Scrollback, ScrollbackStorage};

use super::Grid;

/// What a history export is fenced on — see the module doc.
///
/// Two fences taken from the same grid describe the same retained history
/// (older lines possibly evicted since) exactly when [`Self::holds_at`] says
/// so; everything else about the grid may have moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryFence {
    /// `GridStorage::history_renumber_epoch` — any width reflow or unscroll
    /// advances it, and with it the text an absolute row names.
    pub renumber_epoch: u64,
    /// `GridStorage::scrollback_clear_gen` — ED3 and a full reset advance it.
    pub clear_gen: u64,
    /// `GridStorage::history_reveal_gen` — a rows-grow that handed history
    /// lines back to the screen advances it: a revealed line can be rewritten
    /// and pushed back under the SAME absolute key with other text, and
    /// neither the epoch nor `end` would show it (2026-09-26 review).
    pub reveal_gen: u64,
    /// The width every history line is wrapped at.
    pub cols: u16,
    /// Absolute row of the OLDEST retained history line.
    pub oldest: u64,
    /// Absolute row one past the NEWEST history line — the top visible row
    /// (`Grid::base_y`). Lines pushed after the fence land at `end` and above.
    pub end: u64,
    /// The tiered store was out with an off-thread reflow when this was taken:
    /// the history is not all here, so no export may be fenced on it.
    pub detached: bool,
}

impl HistoryFence {
    /// How many history lines the fence spans.
    #[must_use]
    pub fn lines(&self) -> u64 {
        self.end.saturating_sub(self.oldest)
    }

    /// Whether `now` — a fence taken LATER from the same grid — still
    /// describes this fence's history: same epoch, same clear generation, no
    /// reveal since, same width, neither side detached, and the absolute
    /// counter has not gone backwards (a replaced grid starts it over).
    #[must_use]
    pub fn holds_at(&self, now: &HistoryFence) -> bool {
        !self.detached
            && !now.detached
            && self.renumber_epoch == now.renumber_epoch
            && self.clear_gen == now.clear_gen
            && self.reveal_gen == now.reveal_gen
            && self.cols == now.cols
            && now.end >= self.end
    }
}

/// The successor's fence on its own import: the scrollback-clear generation
/// of the grid whose keys [`Grid::reserve_older_history_keys`] reserved.
/// [`Grid::attach_older_history`] requires it, and refuses once that grid's
/// scrollback was cleared since — the import lands on a worker, after Commit,
/// while the adopted shell and whatever it replays already run, and an ED3 or
/// a reset in that window is the user's word that this history is GONE
/// (2026-09-26 review: an import after `ESC [3J` put back the 46 lines the
/// clear had just removed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OlderHistoryClaim {
    clear_gen: u64,
}

/// Why [`Grid::history_lines_since_fence`] refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryFenceBroken {
    /// The history changed under the fence: a width reflow, an unscroll, a
    /// scrollback clear, a reset, or an off-thread reflow now holds it.
    Moved,
    /// The requested row was evicted since the fence (retention outran the
    /// reader of this history).
    Evicted,
}

impl std::fmt::Display for HistoryFenceBroken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Moved => "the history was rewrapped, cleared or reset after it was fenced",
            Self::Evicted => "retention evicted lines the export had not read yet",
        })
    }
}

/// Lines to place BEFORE a grid's oldest retained line, already built into a
/// tiered store — the expensive half of an import, done off any lock.
#[derive(Debug)]
pub struct OlderHistory {
    // Boxed: the store is a large value, and every refusal hands it back.
    store: Box<Scrollback>,
    lines: usize,
    cols: u16,
}

impl OlderHistory {
    /// Build `lines` (oldest first, wrapped at `from_cols`) for a grid `cols`
    /// wide: rewrapped when the widths differ (the successor's window may
    /// have settled at another width since the export), then pushed into an
    /// UNBOUNDED store — neither a line limit nor a memory budget may evict a
    /// line here, because this is not where retention is decided; the grid's
    /// own settings are applied at [`Grid::attach_older_history`].
    // Cost: O(imported history) in ONE call, honestly — the rewrap and the
    // store build, like `PendingScrollbackReflow::reflow`. It takes no lock:
    // its one caller is the successor's history-import worker, which holds
    // none while it runs, and nothing on the main thread calls it.
    #[must_use]
    pub fn build(lines: &[Line], from_cols: u16, cols: u16) -> Self {
        let mut store = Scrollback::with_defaults();
        store.set_line_limit(None);
        // `usize::MAX` cannot be over budget, so this never evicts; the result
        // is discarded because enforcement on an empty store cannot fail.
        let _ = store.set_memory_budget(usize::MAX);
        if from_cols == cols || lines.is_empty() {
            for line in lines {
                store.push_line(line.clone());
            }
        } else {
            for line in super::scrollback_reflow::reflow_scrollback_lines(lines, cols) {
                store.push_line(line);
            }
        }
        let count = store.line_count();
        Self {
            store: Box::new(store),
            lines: count,
            cols,
        }
    }

    /// How many lines the built store holds (after any rewrap).
    #[must_use]
    pub fn lines(&self) -> usize {
        self.lines
    }

    /// The width the lines are wrapped at.
    #[must_use]
    pub fn cols(&self) -> u16 {
        self.cols
    }
}

/// Why [`Grid::attach_older_history`] refused. Each gives the built history
/// back so the caller may rebuild or retry without re-reading its source.
#[derive(Debug)]
pub enum OlderHistoryRefusal {
    /// An off-thread reflow holds the tiered store; retry once it is back.
    ReflowInFlight(OlderHistory),
    /// The grid is no longer as wide as the history was built for.
    WidthMoved {
        /// The grid's width now.
        now: u16,
        /// The history, still at the width it was built for.
        older: OlderHistory,
    },
    /// The grid keeps no in-memory tiered store (a ring-only or disk-backed
    /// grid) to place history in front of.
    NoTieredStore(OlderHistory),
    /// The pane's scrollback was cleared (ED3, a reset) after the keys were
    /// reserved ([`OlderHistoryClaim`]): the history stays cleared. Not a
    /// loss — the user erased what the import would have put back.
    Cleared(OlderHistory),
}

impl std::fmt::Display for OlderHistoryRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReflowInFlight(_) => formatter.write_str("a scrollback rewrap was in flight"),
            Self::WidthMoved { now, older } => write!(
                formatter,
                "the pane is {now} columns wide, not the {} the history was built for",
                older.cols
            ),
            Self::NoTieredStore(_) => {
                formatter.write_str("the pane keeps no in-memory history store")
            }
            Self::Cleared(_) => formatter.write_str("the pane's scrollback was cleared since"),
        }
    }
}

impl Grid {
    /// The fence a history export of this grid is taken on. See
    /// [`HistoryFence`]. A pure read.
    #[must_use]
    pub fn history_fence(&self) -> HistoryFence {
        HistoryFence {
            renumber_epoch: self.storage.history_renumber_epoch,
            clear_gen: self.storage.scrollback_clear_gen,
            reveal_gen: self.storage.history_reveal_gen,
            cols: self.cols(),
            oldest: self.oldest_absolute_row(),
            end: self.oldest_absolute_row() + self.scrollback_lines() as u64,
            detached: self.storage.scrollback_detached_for_reflow,
        }
    }

    /// Up to `max` history lines starting at absolute row `from`, and never
    /// past `fence.end` — the lines `fence` covers, oldest first, read while
    /// the fence still holds. An empty `Ok` once `from` reaches `fence.end`.
    ///
    /// A line the store cannot decode is an empty line, the dense-slot
    /// posture every other history projection takes, so absolute rows and
    /// line counts stay exact.
    // COST: O(max × cols) — bounded by the caller's chunk, so an exporter can
    // walk a deep history in short lock holds while the grid keeps changing.
    pub fn history_lines_since_fence(
        &self,
        fence: &HistoryFence,
        from: u64,
        max: usize,
    ) -> Result<Vec<Line>, HistoryFenceBroken> {
        let now = self.history_fence();
        if !fence.holds_at(&now) {
            return Err(HistoryFenceBroken::Moved);
        }
        if from < now.oldest {
            return Err(HistoryFenceBroken::Evicted);
        }
        let to = fence.end.min(from.saturating_add(max as u64));
        let Some(count) = to.checked_sub(from) else {
            return Ok(Vec::new());
        };
        let first = usize::try_from(from - now.oldest).map_err(|_| HistoryFenceBroken::Moved)?;
        let count = usize::try_from(count).map_err(|_| HistoryFenceBroken::Moved)?;
        let mut lines = Vec::with_capacity(count);
        for index in first..first + count {
            match self.try_get_history_line(index) {
                Ok(Some(line)) => lines.push(line.into_owned()),
                Ok(None) | Err(_) => lines.push(Line::new()),
            }
        }
        Ok(lines)
    }

    /// Raise the absolute-row counter by `lines`, so history later placed in
    /// front of the oldest line ([`Self::attach_older_history`]) lands under
    /// keys nothing has used, and no retained or visible row's key moves.
    ///
    /// ONLY for a grid that was just built from a checkpoint, before anything
    /// has read an absolute row from it — the successor's adopt path, ahead of
    /// its reader. Raised there, the rows below the oldest retained line read
    /// as evicted until the import fills them, which is exactly what they are
    /// if it never does.
    ///
    /// Returns the claim the import must present ([`OlderHistoryClaim`]).
    #[must_use]
    pub fn reserve_older_history_keys(&mut self, lines: u64) -> OlderHistoryClaim {
        self.storage.absolute_row_counter = self.storage.absolute_row_counter.saturating_add(lines);
        OlderHistoryClaim {
            clear_gen: self.storage.scrollback_clear_gen,
        }
    }

    /// Whether `claim` still admits an import here: this grid's scrollback
    /// has not been cleared since the reserve that minted it. A pure read — an
    /// importer asks it before paying for the read and decode of a sidecar
    /// the attach would refuse anyway.
    #[must_use]
    pub fn older_history_claim_holds(&self, claim: OlderHistoryClaim) -> bool {
        claim.clear_gen == self.storage.scrollback_clear_gen
    }

    /// Place `older` BEFORE the oldest line this grid retains — unless its
    /// scrollback was cleared since `claim` was minted
    /// ([`OlderHistoryRefusal::Cleared`]) — then apply the grid's own
    /// retention (its line limit and memory budget evict the oldest, as they
    /// would have in the process that exported the history).
    /// Returns how many of the imported lines were retained.
    ///
    /// Every retained line keeps its absolute key when the counter was
    /// reserved for the import ([`Self::reserve_older_history_keys`]); when it
    /// was not (or a rewrap made the history longer than reserved), the
    /// counter is raised to cover it and that is announced the way every
    /// other renumbering is — the renumber epoch advances and host
    /// coordinates are invalidated. The epoch advances either way, because
    /// the cached search index assumes the oldest row only ever moves forward.
    // COST: O(tiered-history-lines) under the caller's lock — the grid's OWN
    // tiered lines are re-pushed behind the imported ones (right after an
    // update: the screen carry's few hundred lines); the imported depth was
    // paid by `OlderHistory::build`, off the lock. Called from the successor's
    // history-import worker only.
    pub fn attach_older_history(
        &mut self,
        older: OlderHistory,
        claim: OlderHistoryClaim,
    ) -> Result<usize, OlderHistoryRefusal> {
        // First: a cleared history stays cleared, whatever else holds (an
        // erase during an offloaded reflow bumps the generation too).
        if !self.older_history_claim_holds(claim) {
            return Err(OlderHistoryRefusal::Cleared(older));
        }
        if self.storage.scrollback_detached_for_reflow {
            return Err(OlderHistoryRefusal::ReflowInFlight(older));
        }
        if older.cols != self.cols() {
            return Err(OlderHistoryRefusal::WidthMoved {
                now: self.cols(),
                older,
            });
        }
        if !matches!(self.storage.scrollback, Some(ScrollbackStorage::Memory(_))) {
            return Err(OlderHistoryRefusal::NoTieredStore(older));
        }
        if older.lines == 0 {
            return Ok(0);
        }
        // Everything older than the ring is in the store once the lazy buffer
        // is settled, so the store's lines are exactly the ones to go behind.
        self.drain_lazy_buffer();
        let limit = self.scrollback_line_limit();
        let budget = self.scrollback_memory_budget();
        let before = self.scrollback_lines();
        let mut store = *older.store;
        if let Some(ScrollbackStorage::Memory(current)) = self.storage.scrollback.as_ref() {
            for index in 0..current.line_count() {
                match current.get_line(index) {
                    Ok(Some(line)) => store.push_line(line.into_owned()),
                    Ok(None) | Err(_) => store.push_line(Line::new()),
                }
            }
        }
        self.storage.scrollback = Some(ScrollbackStorage::Memory(store));
        // The keys: base_y (and with it every retained and visible row's key)
        // is `counter - visible` and does not move, so only the oldest row
        // moves back — unless the counter cannot cover the history it now
        // retains, when it is raised and the move is a renumbering.
        let needed = u64::from(self.storage.visible_rows) + self.scrollback_lines() as u64;
        if self.storage.absolute_row_counter < needed {
            self.storage.absolute_row_counter = needed;
            self.invalidate_host_coordinates();
        }
        self.storage.history_renumber_epoch = self.storage.history_renumber_epoch.saturating_add(1);
        if let Some(budget) = budget {
            // An enforcement failure leaves the store over its budget, as the
            // store's own pushes would; the next drain enforces again.
            let _ = self.set_scrollback_memory_budget(budget);
        }
        self.set_scrollback_line_limit(limit);
        self.storage.mark_content_full();
        self.clamp_display_offset();
        Ok(self.scrollback_lines().saturating_sub(before))
    }
}

#[cfg(test)]
#[path = "history_carry_tests.rs"]
mod tests;
