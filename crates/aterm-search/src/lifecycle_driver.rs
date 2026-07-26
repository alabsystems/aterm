// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Screen-routed grid→index lifecycle driver (Wave 4/4A milestone 2).
//!
//! [`SearchLifecycleDriver`] is the layer the terminal-side hook-up talks to:
//! it routes the [`SearchLifecycleEvent`] alphabet into a
//! [`LifecycleSearchIndex`] with the screen semantics the raw index cannot
//! decide for itself.
//!
//! ## The alternate-screen contract (4A P2)
//!
//! While the ALTERNATE screen is active, main-screen retention transitions
//! can still occur underneath it — a memory-budget shrink evicts main
//! scrollback, and a resize rewraps the main grid that is currently hidden.
//! Those events must be reflected in the MAIN document space by the time it
//! is searchable again, and must never touch the alt document space. The
//! driver therefore:
//!
//! - applies `Append`/`Replace`/`Clear` to the ACTIVE screen immediately
//!   (content events always describe the screen that produced them);
//! - applies main-screen `EvictBelow`/`Reflow` immediately while the main
//!   screen is active;
//! - DEFERS main-screen `EvictBelow`/`Reflow` that arrive while the alt
//!   screen is active, and REPLAYS them in arrival order on alt exit —
//!   before any post-exit event is applied.
//!
//! Deferred events are compacted without reordering: consecutive evictions
//! coalesce to their maximum boundary, and a reflow whose layout starts at or
//! above the previous deferred reflow's start supersedes it (the later
//! layout re-supplies every retained row). A regressive reflow pair (later
//! start below the earlier) is kept verbatim — the index's P3 clamp makes
//! replay order-safe regardless.
//!
//! Known limitation (recorded, out of the P2 prescription's scope): a
//! main-screen `Clear` (ED3) arriving while the alt screen is active is not
//! routable through this driver yet; no current grid caller emits one. The
//! terminal-side hook-up must route ED3 while `!alt` only.
//!
//! ## Oracle obligations
//!
//! `lifecycle_oracle_tests.rs` drives random two-screen event sequences
//! (deferral windows included) against the model and asserts byte-identical
//! match sets on the active screen after every event, plus main-screen
//! catch-up equivalence at every alt exit.

use crate::index::SearchOptionsError;
use crate::lifecycle_index::{
    LifecycleSearchIndex, LifecycleSearchResults, SearchLifecycleEvent, UpsertOutcome,
};
use crate::types::SearchDirection;

/// A main-screen retention event deferred while the alt screen is active.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DeferredMainEvent {
    /// `EvictBelow { oldest_retained }`.
    EvictBelow(u64),
    /// `Reflow { first_retained, rows }`.
    Reflow(u64, Vec<String>),
}

/// Screen-routed lifecycle driver with the P2 alt-screen defer/replay
/// contract. See the module docs.
#[derive(Debug, Default)]
pub struct SearchLifecycleDriver {
    index: LifecycleSearchIndex,
    /// Main-screen retention events awaiting replay, in arrival order.
    deferred_main: Vec<DeferredMainEvent>,
}

impl SearchLifecycleDriver {
    /// Create with the default cache cap.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create with an explicit cache cap (see
    /// [`LifecycleSearchIndex::with_max_cached_lines`]).
    #[must_use]
    pub fn with_max_cached_lines(max_cached_lines: usize) -> Self {
        Self {
            index: LifecycleSearchIndex::with_max_cached_lines(max_cached_lines),
            deferred_main: Vec::new(),
        }
    }

    /// A new line entered the ACTIVE screen's retained buffer.
    pub fn append(&mut self, abs_row: u64, text: &str) -> UpsertOutcome {
        self.index.apply(&SearchLifecycleEvent::Append {
            abs_row,
            text: text.to_string(),
        })
    }

    /// The ACTIVE screen's retained line at `abs_row` changed in place.
    pub fn replace(&mut self, abs_row: u64, text: &str) -> UpsertOutcome {
        self.index.apply(&SearchLifecycleEvent::Replace {
            abs_row,
            text: text.to_string(),
        })
    }

    /// ED3/RIS on the ACTIVE screen. See the module docs for the recorded
    /// main-under-alt limitation.
    pub fn clear(&mut self, next_row: u64) {
        self.index.apply(&SearchLifecycleEvent::Clear { next_row });
    }

    /// MAIN-screen retention dropped every line below `oldest_retained`.
    /// Deferred while the alt screen is active (P2), applied immediately
    /// otherwise.
    pub fn main_evict_below(&mut self, oldest_retained: u64) {
        if self.index.is_alt_active() {
            // Coalesce consecutive evictions to their maximum boundary.
            if let Some(DeferredMainEvent::EvictBelow(prior)) = self.deferred_main.last_mut() {
                *prior = (*prior).max(oldest_retained);
                return;
            }
            self.deferred_main
                .push(DeferredMainEvent::EvictBelow(oldest_retained));
            return;
        }
        self.index
            .apply(&SearchLifecycleEvent::EvictBelow { oldest_retained });
    }

    /// MAIN-screen rewrap into a new contiguous layout. Deferred while the
    /// alt screen is active (P2), applied immediately otherwise.
    pub fn main_reflow(&mut self, first_retained: u64, rows: Vec<String>) {
        if self.index.is_alt_active() {
            // A later reflow re-supplies every retained row: it supersedes an
            // immediately-preceding deferred reflow unless it is regressive
            // (starts below), which the P3 clamp handles at replay instead.
            if let Some(DeferredMainEvent::Reflow(prior_first, _)) = self.deferred_main.last()
                && first_retained >= *prior_first
            {
                self.deferred_main.pop();
            }
            self.deferred_main
                .push(DeferredMainEvent::Reflow(first_retained, rows));
            return;
        }
        self.index.apply(&SearchLifecycleEvent::Reflow {
            first_retained,
            rows,
        });
    }

    /// ALT-screen rewrap (a resize while the alt screen is displayed).
    /// Applies immediately to the active (alt) document space; on the main
    /// screen this is exactly [`main_reflow`](Self::main_reflow).
    pub fn active_reflow(&mut self, first_retained: u64, rows: Vec<String>) {
        if self.index.is_alt_active() {
            self.index.apply(&SearchLifecycleEvent::Reflow {
                first_retained,
                rows,
            });
        } else {
            self.main_reflow(first_retained, rows);
        }
    }

    /// Switch between the main and alternate screens. Leaving the alt screen
    /// replays deferred main-screen retention events, in arrival order,
    /// before returning — the main document space is caught up before any
    /// post-exit event can land.
    pub fn set_alt_screen(&mut self, alt: bool) {
        let was_alt = self.index.is_alt_active();
        self.index
            .apply(&SearchLifecycleEvent::AltScreenSwitch { alt });
        if was_alt && !alt {
            for event in std::mem::take(&mut self.deferred_main) {
                match event {
                    DeferredMainEvent::EvictBelow(oldest_retained) => {
                        self.index
                            .apply(&SearchLifecycleEvent::EvictBelow { oldest_retained });
                    }
                    DeferredMainEvent::Reflow(first_retained, rows) => {
                        self.index.apply(&SearchLifecycleEvent::Reflow {
                            first_retained,
                            rows,
                        });
                    }
                }
            }
        }
    }

    /// Search the ACTIVE screen, forward order.
    pub fn search_results_opts(
        &self,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
    ) -> Result<LifecycleSearchResults, SearchOptionsError> {
        self.index
            .search_results_opts(query, case_sensitive, is_regex)
    }

    /// Search the ACTIVE screen with an explicit direction.
    pub fn search_results_opts_direction(
        &self,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
        direction: SearchDirection,
    ) -> Result<LifecycleSearchResults, SearchOptionsError> {
        self.index
            .search_results_opts_direction(query, case_sensitive, is_regex, direction)
    }

    /// True while the alternate screen's document space is active.
    #[must_use]
    pub fn is_alt_active(&self) -> bool {
        self.index.is_alt_active()
    }

    /// Number of main-screen retention events currently deferred.
    #[must_use]
    pub fn deferred_main_events(&self) -> usize {
        self.deferred_main.len()
    }

    /// The ACTIVE screen's retained absolute-row window `[lo, hi)`.
    #[must_use]
    pub fn retained_row_window(&self) -> (u64, u64) {
        self.index.retained_row_window()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows_of(res: &LifecycleSearchResults) -> Vec<u64> {
        res.matches.iter().map(|m| m.abs_row).collect()
    }

    fn find(driver: &SearchLifecycleDriver, query: &str) -> LifecycleSearchResults {
        driver
            .search_results_opts(query, false, false)
            .expect("search")
    }

    /// P2 core: a main-screen eviction during an alt excursion must not touch
    /// the alt documents, and must be applied (with honest flags) by the time
    /// the main screen is searchable again.
    #[test]
    fn main_eviction_under_alt_is_deferred_and_replayed_on_exit() {
        let mut driver = SearchLifecycleDriver::new();
        for i in 0..10 {
            driver.append(i, &format!("needle main {i}"));
        }
        driver.set_alt_screen(true);
        driver.append(100, "needle alt");

        driver.main_evict_below(4);
        assert_eq!(driver.deferred_main_events(), 1, "deferred, not applied");
        assert_eq!(
            rows_of(&find(&driver, "needle")),
            vec![100],
            "the deferred main eviction must not touch the alt documents"
        );
        assert!(!find(&driver, "needle").incomplete, "alt stays complete");

        driver.set_alt_screen(false);
        assert_eq!(driver.deferred_main_events(), 0, "replayed on exit");
        let res = find(&driver, "needle");
        assert_eq!(rows_of(&res), (4..10).collect::<Vec<u64>>());
        assert!(res.incomplete, "the replayed eviction is reported honestly");
        assert_eq!(res.lowest_retained_abs_row, 4);
    }

    /// P2: a main-screen reflow during an alt excursion is deferred and the
    /// replayed layout wins on exit; consecutive evictions coalesce.
    #[test]
    fn main_reflow_under_alt_is_deferred_and_evictions_coalesce() {
        let mut driver = SearchLifecycleDriver::new();
        for i in 0..8 {
            driver.append(i, "needle before");
        }
        driver.set_alt_screen(true);

        driver.main_evict_below(2);
        driver.main_evict_below(3);
        assert_eq!(
            driver.deferred_main_events(),
            1,
            "consecutive evictions coalesce to the maximum"
        );
        driver.main_reflow(3, (0..4).map(|i| format!("needle reflowed {i}")).collect());
        driver.main_reflow(3, (0..3).map(|i| format!("needle reflowed2 {i}")).collect());
        assert_eq!(
            driver.deferred_main_events(),
            2,
            "a covering reflow supersedes the prior deferred reflow"
        );

        driver.set_alt_screen(false);
        let res = find(&driver, "reflowed2");
        assert_eq!(rows_of(&res), vec![3, 4, 5], "the latest layout wins");
        assert!(res.incomplete, "the replayed eviction stays reported");
        assert_eq!(res.lowest_retained_abs_row, 3);
    }

    /// Main-screen events with the MAIN screen active apply immediately —
    /// deferral is strictly an alt-excursion behavior. Re-entering alt after
    /// a replay starts a fresh alt space and a fresh deferral window.
    #[test]
    fn main_events_apply_immediately_outside_alt_excursions() {
        let mut driver = SearchLifecycleDriver::new();
        for i in 0..6 {
            driver.append(i, "needle x");
        }
        driver.main_evict_below(2);
        assert_eq!(driver.deferred_main_events(), 0);
        assert_eq!(rows_of(&find(&driver, "needle")), vec![2, 3, 4, 5]);

        driver.set_alt_screen(true);
        assert!(
            find(&driver, "needle").matches.is_empty(),
            "fresh alt space"
        );
        driver.main_evict_below(4);
        driver.set_alt_screen(false);
        assert_eq!(rows_of(&find(&driver, "needle")), vec![4, 5]);
    }
}
