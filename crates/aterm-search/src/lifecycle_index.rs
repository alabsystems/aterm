// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! E2-redesign foundations: compact document identity + an explicit
//! grid→index lifecycle event alphabet (Wave 4 / Track 4A, milestone 1).
//!
//! ## Why this exists
//!
//! The legacy path keys a persistent [`SearchIndex`] directly by ever-growing
//! ABSOLUTE row: postings saturate row ids to `u32`, short queries scan a
//! numeric row range that grows without bound, and invalidation never removes
//! stale entries (Codex E2 correction — "the most serious roadmap mistake").
//! This module redesigns document identity, eviction, short-query scanning,
//! reflow, and lifecycle *before* any incremental hook-up:
//!
//! - **Compact doc ids**: terminal rows are contiguous, so each screen's
//!   retained window maps to a dense `u32` id window `abs_row - epoch_base`.
//!   Order is preserved (doc order == row order), so every range-bounded
//!   search path works unchanged, and ids never saturate: when the offset
//!   would exceed the id ceiling the index **re-epochs** (rebases ids at the
//!   oldest retained row and rebuilds postings — rare: once per ~4 B rows).
//! - **Explicit lifecycle events**: [`SearchLifecycleEvent`] is the whole
//!   alphabet — `Append`, `Replace`, `EvictBelow`, `Reflow`, `Clear`,
//!   `AltScreenSwitch` — applied by the grid driver, never inferred by
//!   content-generation diffing.
//! - **Bounded short-query scan**: a `< 3`-byte query scans the dense doc
//!   window (O(retained rows)), never `0..absolute_row_count`.
//! - **Eviction removes entries**: `EvictBelow`/`Reflow`/`Clear` really drop
//!   postings and cached text (via [`SearchIndex::retain_history_from`] /
//!   rebuild), instead of leaving stale rows behind a bumped generation.
//! - **ID-exhaustion handling**: if the *retained span itself* exceeds the id
//!   ceiling (impossible in production where the ceiling is `u32::MAX - 1`
//!   and retention is bounded in the low millions; reachable in tests via an
//!   injected ceiling), the index forces a retention cut to the newest
//!   `ceiling + 1` rows and reports honest incompleteness — it never wraps,
//!   collides, or silently saturates ids.
//!
//! ## Equivalence contract (the milestone gate)
//!
//! The acceptance test is the differential equivalence oracle in
//! `lifecycle_oracle_tests.rs`: property tests over bounded event sequences
//! spanning the whole alphabet assert that matches are **byte-identical** to a
//! from-scratch legacy [`SearchIndex`] built over the same surviving rows.
//! The one *documented* divergence is honesty: after retention eviction the
//! legacy rebuild forgets that rows were dropped (`incomplete == false`),
//! while this index keeps reporting `incomplete == true` with the true oldest
//! retained absolute row — the exact E2/E9 honesty direction the audit
//! mandates. Match sets and ordering are always identical.
//!
//! ## Verification obligations (waiver renewed at milestone 2)
//!
//! Milestone 2 landed the [`SearchLifecycleDriver`](crate::SearchLifecycleDriver)
//! (screen-routed events, alt-screen defer/replay per the 4A P2 contract) and
//! the 4A adversarial guards (P1 cap-watermark upsert guard, P3 reflow clamp,
//! P8 re-epoch watermark carry, P6 honest u32 payload boundary), each pinned
//! by the extended differential oracle (now including `is_regex` per P4 and
//! the two-screen driver battery). The ty_model! lifecycle actions remain
//! OUTSTANDING: they are deferred to the milestone that completes the
//! terminal-side live hook-up (grid event emission + wasm export rev), where
//! the action alphabet becomes externally observable. Until then the
//! differential oracle remains the recorded acceptance gate — waiver renewed
//! honestly in CHANGELOG.md, not silently dropped.

use crate::index::{DEFAULT_MAX_CACHED_LINES, SearchIndex, SearchOptionsError};
use crate::types::SearchDirection;

/// Production id ceiling: one below `u32::MAX` so the legacy
/// `line_as_u32` saturation value can never alias a real document.
const PRODUCTION_ID_CEILING: u32 = u32::MAX - 1;

/// One explicit grid→index lifecycle event (the complete E2 event alphabet).
///
/// The grid driver — not content-generation diffing — describes every
/// retained-buffer transition with these events. Absolute rows are `u64`
/// (the engine's monotonic row space); the index maps them to compact
/// per-epoch doc ids internally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchLifecycleEvent {
    /// A new line entered the retained buffer at `abs_row`.
    Append {
        /// Absolute row of the new line.
        abs_row: u64,
        /// The line's text.
        text: String,
    },
    /// The text of the retained line at `abs_row` changed in place
    /// (visible-row cell writes). Stale replaces addressed below the
    /// retained window are dropped.
    Replace {
        /// Absolute row of the mutated line.
        abs_row: u64,
        /// The line's full new text.
        text: String,
    },
    /// Retention dropped every line below `oldest_retained` (scrollback
    /// limit shrink, memory-budget eviction). Marks results incomplete.
    EvictBelow {
        /// First absolute row still retained.
        oldest_retained: u64,
    },
    /// A resize rewrapped the retained buffer into a new contiguous layout
    /// starting at `first_retained`. Rebuilds the active screen's documents
    /// (reflow renumbers rows wholesale; ids re-epoch for free).
    Reflow {
        /// Absolute row of `rows[0]` after the rewrap.
        first_retained: u64,
        /// Every retained row's text, oldest first.
        rows: Vec<String>,
    },
    /// ED3 / RIS: all retained content dropped by explicit request. The next
    /// appended line will be at `next_row`. Resets the incompleteness signal
    /// (a deliberate clear is not truncation — legacy parity).
    Clear {
        /// Absolute row the buffer restarts at.
        next_row: u64,
    },
    /// The active screen switched between main and alternate. Entering the
    /// alternate screen starts an EMPTY alternate document space (the driver
    /// then appends the alt screen's rows); returning leaves the main-screen
    /// documents exactly as they were.
    AltScreenSwitch {
        /// True when the alternate screen becomes active.
        alt: bool,
    },
}

/// A search match in absolute-row space (`u64`, never saturated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbsRowMatch {
    /// Absolute row of the matched line.
    pub abs_row: u64,
    /// Starting display column (0-indexed, inclusive).
    pub start_col: usize,
    /// Ending display column (exclusive).
    pub end_col: usize,
}

/// Matches plus honest completeness in absolute-row space.
///
/// Unlike the legacy rebuild path, `incomplete` stays true across content
/// changes once retention has really dropped rows, and
/// `lowest_retained_abs_row` is the true oldest searchable absolute row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LifecycleSearchResults {
    /// The matches found, in the order produced by the search call.
    pub matches: Vec<AbsRowMatch>,
    /// True when eviction or a result cap may have dropped matches.
    pub incomplete: bool,
    /// Oldest absolute row still searchable. 0 when `incomplete` is false
    /// (legacy [`SearchResults`](crate::SearchResults) contract parity).
    pub lowest_retained_abs_row: u64,
}

/// [`LifecycleSearchResults`] lowered to the legacy u32 glue payload
/// (flat `[row, start_col, len]` triplets), honestly (4A P6).
///
/// The wasm glue's `search` contract carries u32 triplets, while the
/// lifecycle backend reports u64 absolute rows. Until the glue contract is
/// revved to u64 rows, matches whose row exceeds the u32 payload space are
/// DROPPED and reported via `incomplete`/`payload_overflow` — never
/// saturated to `u32::MAX`, which would alias the legacy saturation
/// sentinel and collide distinct rows. Column/length overflow saturates
/// exactly as the legacy export does (widths are u16-bounded in practice).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct U32PayloadResults {
    /// Flat `[row, start_col, len]` triplet per surviving match.
    pub triplets: Vec<u32>,
    /// True when eviction, a result cap, or payload overflow dropped matches.
    pub incomplete: bool,
    /// True when at least one match's absolute row exceeded the u32 payload
    /// space and was dropped (the honest-clamp arm of the P6 contract).
    pub payload_overflow: bool,
    /// Oldest absolute row still searchable (u64 — NOT clamped; metadata
    /// paths carry u64 end-to-end).
    pub lowest_retained_abs_row: u64,
}

impl LifecycleSearchResults {
    /// Lower to the legacy u32 triplet payload. See [`U32PayloadResults`].
    ///
    /// Rows at or above `u32::MAX` are dropped (the legacy sentinel value is
    /// reserved), flagged via `payload_overflow`, and folded into
    /// `incomplete` so no caller can mistake a lowered result for exhaustive.
    #[must_use]
    pub fn to_u32_payload(&self) -> U32PayloadResults {
        let mut triplets = Vec::with_capacity(self.matches.len() * 3);
        let mut payload_overflow = false;
        for m in &self.matches {
            match u32::try_from(m.abs_row) {
                Ok(row) if row != u32::MAX => {
                    triplets.push(row);
                    triplets.push(u32::try_from(m.start_col).unwrap_or(u32::MAX));
                    triplets.push(
                        u32::try_from(m.end_col.saturating_sub(m.start_col)).unwrap_or(u32::MAX),
                    );
                }
                _ => payload_overflow = true,
            }
        }
        U32PayloadResults {
            triplets,
            incomplete: self.incomplete || payload_overflow,
            payload_overflow,
            lowest_retained_abs_row: self.lowest_retained_abs_row,
        }
    }
}

/// One screen's document space: a dense doc-id window over its retained rows.
#[derive(Debug)]
struct ScreenDocs {
    /// Postings/content index keyed by compact doc id (`abs_row - base`).
    index: SearchIndex,
    /// Absolute row of doc id 0 for the current epoch.
    base: u64,
    /// Retained window `[window_lo, window_hi)` in absolute rows. Contiguous
    /// by grid nature (terminal rows are dense); appends above `window_hi`
    /// extend it, `EvictBelow` advances `window_lo`.
    window_lo: u64,
    window_hi: u64,
    /// True once retention (not a deliberate `Clear`) really dropped rows.
    evicted_any: bool,
    /// High-water mark of retention cuts, in absolute rows.
    lowest_retained_abs: u64,
}

impl ScreenDocs {
    fn new(cap: usize) -> Self {
        Self {
            index: SearchIndex::with_capacity_and_max(0, cap),
            base: 0,
            window_lo: 0,
            window_hi: 0,
            evicted_any: false,
            lowest_retained_abs: 0,
        }
    }

    /// Doc id for `abs_row` in the current epoch, if it fits the ceiling.
    fn doc_offset(&self, abs_row: u64, ceiling: u32) -> Option<usize> {
        let off = abs_row.checked_sub(self.base)?;
        if off > u64::from(ceiling) {
            return None;
        }
        usize::try_from(off).ok()
    }

    /// Rebase doc id 0 at the oldest retained row and rebuild postings.
    ///
    /// O(retained rows); rare by construction (once per ~4 B appended rows in
    /// production). Preserves the honesty flags: internal cache-cap eviction
    /// that already occurred stays reported.
    fn reepoch(&mut self, cap: usize) {
        if self.index.results_may_be_incomplete() {
            self.evicted_any = true;
            // Why (4A P8): the rebuilt index forgets internal cache-cap
            // eviction (fresh watermark = 0), so carry the old watermark into
            // absolute rows or post-re-epoch results under-report it.
            self.lowest_retained_abs = self.lowest_retained_abs.max(
                self.base
                    .saturating_add(self.index.lowest_retained_line() as u64),
            );
        }
        let new_base = self.window_lo;
        let old = std::mem::replace(&mut self.index, SearchIndex::with_capacity_and_max(0, cap));
        let old_base = self.base;
        let mut rows: Vec<(u64, String)> = old
            .lines
            .into_iter()
            .map(|(doc, text)| (old_base.saturating_add(doc as u64), text))
            .collect();
        rows.sort_unstable_by_key(|(abs, _)| *abs);
        for (abs, text) in rows {
            if let Some(doc) = abs.checked_sub(new_base)
                && let Ok(doc) = usize::try_from(doc)
            {
                self.index.index_line(doc, &text);
            }
        }
        self.base = new_base;
    }

    /// Drop every retained row below `oldest_retained` (clamped to the
    /// window). Returns true when rows were actually dropped.
    fn evict_below(&mut self, oldest_retained: u64) -> bool {
        if oldest_retained <= self.window_lo {
            return false;
        }
        let clamped = oldest_retained.min(self.window_hi);
        if let Some(doc) = clamped.checked_sub(self.base)
            && let Ok(doc) = usize::try_from(doc)
        {
            self.index.retain_history_from(doc);
        }
        self.window_lo = clamped;
        self.evicted_any = true;
        self.lowest_retained_abs = self.lowest_retained_abs.max(clamped);
        true
    }

    /// Index (or overwrite) the line at `abs_row`, re-epoching — and, at id
    /// exhaustion, force-evicting — as needed. Returns false for a stale
    /// write below the retained window.
    fn upsert(&mut self, abs_row: u64, text: &str, ceiling: u32, cap: usize) -> UpsertOutcome {
        // First content event after construction/clear pins the window start.
        // Never below `window_lo`: a post-`Clear` stale write must not
        // resurrect the pre-clear row space.
        if self.window_lo == self.window_hi && self.index.is_empty() && abs_row >= self.window_lo {
            self.base = abs_row;
            self.window_lo = abs_row;
            self.window_hi = abs_row;
        }
        if abs_row < self.window_lo {
            return UpsertOutcome::StaleDropped;
        }
        // Why (4A P1): a write below the internal cache-cap eviction watermark
        // would RE-ADMIT an evicted row and diverge from a single-pass legacy
        // rebuild (probed under CAP=32). The watermark lives in TWO places:
        // the index-local flags cover eviction within the current epoch, and
        // `lowest_retained_abs` carries it across re-epochs (4A P8) — the
        // swapped-in fresh index resets its local flags, so the carry is the
        // ONLY record of pre-re-epoch eviction. Consult both, or a
        // post-re-epoch Replace below the carried watermark resurrects a row
        // that searches then return BELOW the reported lowest retained row.
        // Driver precondition: replaces target only rows at/above the cap
        // watermark (visible rows are the newest); guard anyway so a
        // violation drops instead of resurrecting.
        if self.evicted_any && abs_row < self.lowest_retained_abs {
            return UpsertOutcome::StaleDropped;
        }
        if self.index.results_may_be_incomplete()
            && abs_row
                < self
                    .base
                    .saturating_add(self.index.lowest_retained_line() as u64)
        {
            return UpsertOutcome::StaleDropped;
        }
        let mut outcome = UpsertOutcome::Applied;
        if self.doc_offset(abs_row, ceiling).is_none() {
            // Epoch offset overflow: rebase ids at the oldest retained row.
            self.reepoch(cap);
            outcome = UpsertOutcome::Reepoched;
            if self.doc_offset(abs_row, ceiling).is_none() {
                // The retained SPAN itself exceeds the id space: force a
                // retention cut to the newest `ceiling + 1` rows. Honest
                // (incomplete reported), never wrapping or colliding.
                let forced_lo = abs_row.saturating_sub(u64::from(ceiling));
                self.evict_below(forced_lo);
                self.reepoch(cap);
                outcome = UpsertOutcome::ForcedEviction;
            }
        }
        // In-range by construction after the re-epoch/forced-cut path above;
        // guard anyway so a logic regression drops rather than corrupts.
        let Some(doc) = self.doc_offset(abs_row, ceiling) else {
            return UpsertOutcome::StaleDropped;
        };
        self.index.index_line(doc, text);
        self.window_hi = self.window_hi.max(abs_row.saturating_add(1));
        outcome
    }

    /// Rebuild this screen's documents from a full post-reflow layout.
    ///
    /// Driver precondition (4A P3): `first_retained >= window_lo` — a rewrap
    /// renumbers retained rows but never resurrects evicted/cleared row
    /// space. Violations are clamped: rows that would land below the current
    /// window are dropped, and the layout keeps its numbering from
    /// `first_retained` (row `i` at `first_retained + i`).
    fn reflow(&mut self, first_retained: u64, rows: &[String], cap: usize) {
        let clamped_first = first_retained.max(self.window_lo);
        let skip =
            usize::try_from(clamped_first.saturating_sub(first_retained)).unwrap_or(usize::MAX);
        let rows = rows.get(skip..).unwrap_or(&[]);
        // The grid re-supplies EVERY retained row, so prior internal
        // cache-cap eviction is genuinely repaired by this rebuild — only the
        // sticky retention honesty (`evicted_any`) survives.
        self.index = SearchIndex::with_capacity_and_max(rows.len(), cap);
        for (i, text) in rows.iter().enumerate() {
            self.index.index_line(i, text);
        }
        self.base = clamped_first;
        self.window_lo = clamped_first;
        self.window_hi = clamped_first.saturating_add(rows.len() as u64);
    }

    /// ED3/RIS: drop everything; the buffer restarts at `next_row`.
    fn clear(&mut self, next_row: u64) {
        self.index.clear();
        self.base = next_row;
        self.window_lo = next_row;
        self.window_hi = next_row;
        // A deliberate clear is not truncation (legacy rebuild parity).
        self.evicted_any = false;
        self.lowest_retained_abs = next_row;
    }
}

/// How an `Append`/`Replace` event was absorbed. Introspection for tests and
/// the milestone-2 driver; every variant leaves the index consistent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertOutcome {
    /// Indexed in the current epoch.
    Applied,
    /// Indexed after rebasing doc ids at the oldest retained row.
    Reepoched,
    /// Indexed after an id-exhaustion retention cut (results incomplete).
    ForcedEviction,
    /// Stale write below the retained window; dropped.
    StaleDropped,
}

/// Lifecycle-event-driven search index with compact document identity.
///
/// See the module docs for the design. Searches run against the active
/// screen's document space and return absolute-row results; the main screen's
/// documents survive alternate-screen excursions untouched.
#[derive(Debug)]
pub struct LifecycleSearchIndex {
    main: ScreenDocs,
    alt: ScreenDocs,
    active_alt: bool,
    id_ceiling: u32,
    max_cached_lines: usize,
    reepoch_count: u64,
}

impl LifecycleSearchIndex {
    /// Create with the default cache cap ([`DEFAULT_MAX_CACHED_LINES`]).
    #[must_use]
    pub fn new() -> Self {
        Self::with_max_cached_lines(DEFAULT_MAX_CACHED_LINES)
    }

    /// Create with an explicit cache cap (see
    /// [`SearchIndex::set_max_cached_lines`] semantics).
    #[must_use]
    pub fn with_max_cached_lines(max_cached_lines: usize) -> Self {
        Self::with_max_cached_and_ceiling(max_cached_lines, PRODUCTION_ID_CEILING)
    }

    /// Test seam: inject a small id ceiling to exercise re-epoch and
    /// id-exhaustion handling without appending 4 B rows.
    pub(crate) fn with_max_cached_and_ceiling(max_cached_lines: usize, id_ceiling: u32) -> Self {
        let cap = max_cached_lines.max(1);
        Self {
            main: ScreenDocs::new(cap),
            alt: ScreenDocs::new(cap),
            active_alt: false,
            id_ceiling,
            max_cached_lines: cap,
            reepoch_count: 0,
        }
    }

    fn active(&self) -> &ScreenDocs {
        if self.active_alt {
            &self.alt
        } else {
            &self.main
        }
    }

    fn active_mut(&mut self) -> &mut ScreenDocs {
        if self.active_alt {
            &mut self.alt
        } else {
            &mut self.main
        }
    }

    /// Apply one lifecycle event. Returns the append/replace outcome for
    /// content events and [`UpsertOutcome::Applied`] for the others.
    pub fn apply(&mut self, event: &SearchLifecycleEvent) -> UpsertOutcome {
        let (ceiling, cap) = (self.id_ceiling, self.max_cached_lines);
        match event {
            SearchLifecycleEvent::Append { abs_row, text }
            | SearchLifecycleEvent::Replace { abs_row, text } => {
                let outcome = self.active_mut().upsert(*abs_row, text, ceiling, cap);
                if matches!(
                    outcome,
                    UpsertOutcome::Reepoched | UpsertOutcome::ForcedEviction
                ) {
                    self.reepoch_count = self.reepoch_count.saturating_add(1);
                }
                outcome
            }
            SearchLifecycleEvent::EvictBelow { oldest_retained } => {
                self.active_mut().evict_below(*oldest_retained);
                UpsertOutcome::Applied
            }
            SearchLifecycleEvent::Reflow {
                first_retained,
                rows,
            } => {
                self.active_mut().reflow(*first_retained, rows, cap);
                UpsertOutcome::Applied
            }
            SearchLifecycleEvent::Clear { next_row } => {
                self.active_mut().clear(*next_row);
                UpsertOutcome::Applied
            }
            SearchLifecycleEvent::AltScreenSwitch { alt } => {
                if *alt && !self.active_alt {
                    self.alt = ScreenDocs::new(cap);
                }
                self.active_alt = *alt;
                UpsertOutcome::Applied
            }
        }
    }

    /// Search the active screen, forward order. See
    /// [`search_results_opts_direction`](Self::search_results_opts_direction).
    pub fn search_results_opts(
        &self,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
    ) -> Result<LifecycleSearchResults, SearchOptionsError> {
        self.search_results_opts_direction(
            query,
            case_sensitive,
            is_regex,
            SearchDirection::Forward,
        )
    }

    /// Search the active screen with the legacy option surface, returning
    /// absolute-row results plus honest completeness.
    ///
    /// Matching, ordering, caps, and per-mode behavior are the legacy
    /// [`SearchIndex`] engine's verbatim (same code, compact doc keys); only
    /// the row coordinates are mapped back to absolute space.
    pub fn search_results_opts_direction(
        &self,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
        direction: SearchDirection,
    ) -> Result<LifecycleSearchResults, SearchOptionsError> {
        let screen = self.active();
        let inner = screen.index.search_results_opts_direction(
            query,
            case_sensitive,
            is_regex,
            direction,
        )?;
        let matches = inner
            .matches
            .iter()
            .map(|m| AbsRowMatch {
                abs_row: screen.base.saturating_add(m.line as u64),
                start_col: m.start_col,
                end_col: m.end_col,
            })
            .collect();
        let incomplete = inner.incomplete || screen.evicted_any;
        let lowest_retained_abs_row = if incomplete {
            screen.lowest_retained_abs.max(
                screen
                    .base
                    .saturating_add(screen.index.lowest_retained_line() as u64),
            )
        } else {
            0
        };
        Ok(LifecycleSearchResults {
            matches,
            incomplete,
            lowest_retained_abs_row,
        })
    }

    /// The active screen's retained absolute-row window `[lo, hi)`.
    #[must_use]
    pub fn retained_row_window(&self) -> (u64, u64) {
        let screen = self.active();
        (screen.window_lo, screen.window_hi)
    }

    /// True while the alternate screen's document space is active.
    #[must_use]
    pub fn is_alt_active(&self) -> bool {
        self.active_alt
    }

    /// Number of id re-epochs (rebases) performed so far. Introspection.
    #[must_use]
    pub fn reepoch_count(&self) -> u64 {
        self.reepoch_count
    }
}

impl Default for LifecycleSearchIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn append(idx: &mut LifecycleSearchIndex, abs_row: u64, text: &str) -> UpsertOutcome {
        idx.apply(&SearchLifecycleEvent::Append {
            abs_row,
            text: text.to_string(),
        })
    }

    fn rows_of(res: &LifecycleSearchResults) -> Vec<u64> {
        res.matches.iter().map(|m| m.abs_row).collect()
    }

    fn find(idx: &LifecycleSearchIndex, query: &str) -> LifecycleSearchResults {
        idx.search_results_opts(query, false, false)
            .expect("search")
    }

    #[test]
    fn append_and_replace_are_searchable_at_absolute_rows() {
        let mut idx = LifecycleSearchIndex::new();
        append(&mut idx, 0, "alpha needle one");
        append(&mut idx, 1, "no hit here");
        append(&mut idx, 2, "needle two");
        assert_eq!(rows_of(&find(&idx, "needle")), vec![0, 2]);

        // Replace removes the old text's postings and indexes the new text.
        idx.apply(&SearchLifecycleEvent::Replace {
            abs_row: 2,
            text: "replaced, no hit".to_string(),
        });
        assert_eq!(rows_of(&find(&idx, "needle")), vec![0]);
        assert!(!find(&idx, "needle").incomplete);
    }

    #[test]
    fn evict_below_removes_matches_and_reports_honest_incompleteness() {
        let mut idx = LifecycleSearchIndex::new();
        for i in 0..10 {
            append(&mut idx, i, &format!("needle {i}"));
        }
        idx.apply(&SearchLifecycleEvent::EvictBelow { oldest_retained: 4 });
        let res = find(&idx, "needle");
        assert_eq!(rows_of(&res), vec![4, 5, 6, 7, 8, 9]);
        assert!(res.incomplete, "retention eviction must be reported");
        assert_eq!(res.lowest_retained_abs_row, 4);

        // A stale Replace below the retained window is dropped, not indexed.
        let out = idx.apply(&SearchLifecycleEvent::Replace {
            abs_row: 1,
            text: "needle resurrected".to_string(),
        });
        assert_eq!(out, UpsertOutcome::StaleDropped);
        assert_eq!(rows_of(&find(&idx, "needle")), vec![4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn clear_resets_rows_and_completeness_like_ed3() {
        let mut idx = LifecycleSearchIndex::new();
        for i in 0..6 {
            append(&mut idx, i, "needle before clear");
        }
        idx.apply(&SearchLifecycleEvent::EvictBelow { oldest_retained: 2 });
        assert!(find(&idx, "needle").incomplete);

        idx.apply(&SearchLifecycleEvent::Clear { next_row: 6 });
        let res = find(&idx, "needle");
        assert!(res.matches.is_empty(), "ED3/RIS drops all content");
        assert!(!res.incomplete, "a deliberate clear is not truncation");

        // Post-clear stale writes below next_row are dropped.
        let out = append(&mut idx, 3, "needle stale");
        assert_eq!(out, UpsertOutcome::StaleDropped);
        assert!(find(&idx, "needle").matches.is_empty());

        // New content lands at the post-clear row space.
        append(&mut idx, 6, "needle after clear");
        assert_eq!(rows_of(&find(&idx, "needle")), vec![6]);
    }

    #[test]
    fn alt_screen_switch_isolates_and_preserves_main_documents() {
        let mut idx = LifecycleSearchIndex::new();
        append(&mut idx, 0, "main needle");
        idx.apply(&SearchLifecycleEvent::AltScreenSwitch { alt: true });
        assert!(idx.is_alt_active());
        assert!(
            find(&idx, "needle").matches.is_empty(),
            "entering alt starts an empty alternate document space"
        );
        append(&mut idx, 100, "alt needle");
        assert_eq!(rows_of(&find(&idx, "needle")), vec![100]);

        idx.apply(&SearchLifecycleEvent::AltScreenSwitch { alt: false });
        assert_eq!(
            rows_of(&find(&idx, "needle")),
            vec![0],
            "main documents survive the alt excursion untouched"
        );

        // Re-entering alt starts empty again (1049 semantics).
        idx.apply(&SearchLifecycleEvent::AltScreenSwitch { alt: true });
        assert!(find(&idx, "needle").matches.is_empty());
    }

    #[test]
    fn reflow_renumbers_rows_wholesale() {
        let mut idx = LifecycleSearchIndex::new();
        for i in 0..4 {
            append(&mut idx, i, "needle wide line");
        }
        // Rewrap doubled the row count and shifted the origin.
        let rows: Vec<String> = (0..8)
            .map(|i| {
                if i % 2 == 0 {
                    "needle".to_string()
                } else {
                    "wide line".to_string()
                }
            })
            .collect();
        idx.apply(&SearchLifecycleEvent::Reflow {
            first_retained: 2,
            rows,
        });
        assert_eq!(rows_of(&find(&idx, "needle")), vec![2, 4, 6, 8]);
        assert_eq!(idx.retained_row_window(), (2, 10));
    }

    #[test]
    fn short_query_scan_is_bounded_by_retained_rows_at_huge_abs_rows() {
        let mut idx = LifecycleSearchIndex::new();
        // Rows far beyond u32: the legacy abs-keyed index saturates here.
        let base = (u64::from(u32::MAX)) * 16 + 7;
        for i in 0..50 {
            append(&mut idx, base + i, &format!("x{}", i % 10));
        }
        SearchIndex::take_case_insensitive_candidate_visits();
        let res = find(&idx, "x1");
        assert_eq!(res.matches.len(), 5);
        assert!(
            res.matches.iter().all(|m| m.abs_row > u64::from(u32::MAX)),
            "absolute rows beyond u32 are reported exactly, never saturated"
        );
        assert_eq!(
            rows_of(&res),
            vec![base + 1, base + 11, base + 21, base + 31, base + 41]
        );
        let visits = SearchIndex::take_case_insensitive_candidate_visits();
        assert!(
            visits <= 50,
            "short-query scan must be bounded by the retained window ({visits} visits)"
        );
    }

    #[test]
    fn id_exhaustion_reepochs_after_eviction_without_saturation() {
        // Ceiling of 99: only 100 doc ids exist per epoch.
        let mut idx = LifecycleSearchIndex::with_max_cached_and_ceiling(1000, 99);
        for i in 0..80 {
            append(&mut idx, i, &format!("needle {i}"));
        }
        idx.apply(&SearchLifecycleEvent::EvictBelow {
            oldest_retained: 60,
        });
        // Rows 100.. exceed the epoch-0 offset ceiling; the index must
        // re-epoch at the retained boundary and keep going.
        for i in 80..140 {
            append(&mut idx, i, &format!("needle {i}"));
        }
        assert!(idx.reepoch_count() >= 1, "offset overflow must re-epoch");
        let res = find(&idx, "needle");
        assert_eq!(rows_of(&res), (60..140).collect::<Vec<u64>>());
        assert!(res.incomplete);
        assert_eq!(res.lowest_retained_abs_row, 60);
    }

    /// 4A P1: a replace below the internal cache-cap eviction watermark must
    /// be DROPPED, never re-admitted (the CAP=32 probe divergence).
    #[test]
    fn replace_below_cap_watermark_is_dropped_not_readmitted() {
        let mut idx = LifecycleSearchIndex::with_max_cached_lines(8);
        for i in 0..40 {
            append(&mut idx, i, &format!("filler {i}"));
        }
        let pre = find(&idx, "filler");
        assert!(pre.incomplete, "the tiny cap must have evicted");
        let watermark = pre.lowest_retained_abs_row;
        assert!(watermark > 0);

        // A replace below the cap watermark (but inside the retention window,
        // window_lo == 0) must be dropped by the P1 guard.
        let out = idx.apply(&SearchLifecycleEvent::Replace {
            abs_row: 0,
            text: "needle resurrected".to_string(),
        });
        assert_eq!(out, UpsertOutcome::StaleDropped);
        assert!(
            find(&idx, "needle").matches.is_empty(),
            "an evicted row must never be re-admitted below the cap watermark"
        );

        // At/above the watermark, replaces still apply.
        let out = idx.apply(&SearchLifecycleEvent::Replace {
            abs_row: watermark,
            text: "needle live".to_string(),
        });
        assert_eq!(out, UpsertOutcome::Applied);
        assert_eq!(rows_of(&find(&idx, "needle")), vec![watermark]);
    }

    /// 4A P3: a reflow can never resurrect evicted row space — layouts
    /// starting below the retained window are clamped (prefix rows dropped).
    #[test]
    fn reflow_below_window_lo_is_clamped_never_resurrected() {
        let mut idx = LifecycleSearchIndex::new();
        for i in 0..20 {
            append(&mut idx, i, "needle original");
        }
        idx.apply(&SearchLifecycleEvent::EvictBelow {
            oldest_retained: 10,
        });
        // Out-of-contract reflow claims rows 5..15.
        idx.apply(&SearchLifecycleEvent::Reflow {
            first_retained: 5,
            rows: (0..10).map(|i| format!("needle reflowed {i}")).collect(),
        });
        let res = find(&idx, "needle");
        assert_eq!(
            rows_of(&res),
            (10..15).collect::<Vec<u64>>(),
            "rows below the eviction watermark are dropped, the rest keep \
             their claimed numbering"
        );
        assert_eq!(idx.retained_row_window(), (10, 15));
        assert!(res.incomplete, "retention honesty survives the reflow");
        assert_eq!(res.lowest_retained_abs_row, 10);
    }

    /// 4A P8: a re-epoch after internal cache-cap eviction must carry the
    /// eviction watermark forward — never under-report it.
    #[test]
    fn reepoch_after_cap_eviction_keeps_the_watermark_honest() {
        // Tiny cache cap forces internal eviction; tiny ceiling forces a
        // re-epoch when the contiguous stream crosses offset 100.
        let mut idx = LifecycleSearchIndex::with_max_cached_and_ceiling(8, 99);
        for i in 0..100 {
            append(&mut idx, i, &format!("filler {i}"));
        }
        let pre = find(&idx, "filler");
        assert!(pre.incomplete);
        let watermark = pre.lowest_retained_abs_row;
        assert!(
            watermark > 0,
            "cap eviction must have advanced the watermark"
        );

        // Offset 100 exceeds the 99 ceiling: re-epoch (window_lo is still 0,
        // so the rebase alone would reset the reported watermark to ~0).
        let out = append(&mut idx, 100, "filler 100");
        assert!(matches!(
            out,
            UpsertOutcome::Reepoched | UpsertOutcome::ForcedEviction
        ));
        let post = find(&idx, "filler");
        assert!(
            post.incomplete,
            "cap eviction stays reported after re-epoch"
        );
        assert!(
            post.lowest_retained_abs_row >= watermark,
            "the re-epoch must not under-report the cap-eviction watermark \
             ({} < {watermark})",
            post.lowest_retained_abs_row
        );
    }

    /// 4A P1×P8: after a re-epoch the fresh index forgets the cache-cap
    /// eviction watermark (index-local flags reset to complete/0), so the
    /// upsert guard must consult the CARRIED absolute watermark — otherwise a
    /// Replace below it but above `window_lo` re-admits an evicted row and
    /// searches return a match below the reported lowest retained row.
    #[test]
    fn replace_below_carried_watermark_after_reepoch_is_dropped() {
        // Tiny cache cap forces internal eviction; tiny ceiling forces a
        // re-epoch when the contiguous stream crosses offset 100.
        let mut idx = LifecycleSearchIndex::with_max_cached_and_ceiling(8, 99);
        for i in 0..100 {
            append(&mut idx, i, &format!("filler {i}"));
        }
        let pre = find(&idx, "filler");
        assert!(pre.incomplete);
        let watermark = pre.lowest_retained_abs_row;
        assert!(
            watermark > 1,
            "cap eviction must have advanced the watermark"
        );

        // Offset 100 exceeds the 99 ceiling: re-epoch. Only the carried
        // watermark now remembers the eviction (window_lo is still 0).
        let out = append(&mut idx, 100, "filler 100");
        assert!(matches!(
            out,
            UpsertOutcome::Reepoched | UpsertOutcome::ForcedEviction
        ));

        // A replace below the pre-re-epoch watermark must be dropped, not
        // re-admitted through the fresh index's reset local flags.
        let out = idx.apply(&SearchLifecycleEvent::Replace {
            abs_row: watermark - 1,
            text: "needle resurrected".to_string(),
        });
        assert_eq!(out, UpsertOutcome::StaleDropped);
        assert!(
            find(&idx, "needle").matches.is_empty(),
            "an evicted row must never be re-admitted below the carried watermark"
        );

        // No search may return a match below the reported lowest retained row.
        let all = find(&idx, "filler");
        assert!(all.incomplete);
        assert!(all.lowest_retained_abs_row >= watermark);
        assert!(
            all.matches
                .iter()
                .all(|m| m.abs_row >= all.lowest_retained_abs_row),
            "matches below the reported lowest retained row are the exact \
             divergence the carried-watermark guard exists to prevent"
        );
    }

    /// 4A P6 (both ways, in-range arm): rows inside the u32 payload space
    /// lower to exact triplets with no overflow signal.
    #[test]
    fn u32_payload_is_exact_for_in_range_rows() {
        let mut idx = LifecycleSearchIndex::new();
        append(&mut idx, 7, "needle here");
        // Highest representable payload row (u32::MAX is reserved).
        let top = u64::from(u32::MAX) - 1;
        append(&mut idx, top, "needle top");
        let payload = find(&idx, "needle").to_u32_payload();
        assert_eq!(
            payload.triplets,
            vec![7, 0, 6, u32::MAX - 1, 0, 6],
            "in-range rows lower exactly (row, start_col, len)"
        );
        assert!(!payload.payload_overflow);
        assert!(!payload.incomplete);
    }

    /// 4A P6 (both ways, overflow arm): rows beyond the u32 payload space are
    /// dropped and reported — never aliased to the legacy u32::MAX sentinel.
    #[test]
    fn u32_payload_drops_and_reports_overflowing_rows() {
        let mut idx = LifecycleSearchIndex::new();
        let base = u64::from(u32::MAX);
        append(&mut idx, base, "needle sentinel row");
        append(&mut idx, base + 1, "needle beyond");
        let res = find(&idx, "needle");
        assert_eq!(res.matches.len(), 2, "u64 path reports both rows exactly");
        let payload = res.to_u32_payload();
        assert!(payload.triplets.is_empty(), "no row may alias u32::MAX");
        assert!(payload.payload_overflow);
        assert!(
            payload.incomplete,
            "payload overflow must fold into incomplete"
        );
    }

    #[test]
    fn id_exhaustion_with_oversized_span_forces_honest_retention_cut() {
        // Retained span cannot exceed ceiling+1 = 32 docs.
        let mut idx = LifecycleSearchIndex::with_max_cached_and_ceiling(1000, 31);
        let mut forced = false;
        for i in 0..100 {
            let out = append(&mut idx, i, &format!("needle {i}"));
            forced |= out == UpsertOutcome::ForcedEviction;
        }
        assert!(forced, "an oversized span must force a retention cut");
        let res = find(&idx, "needle");
        assert_eq!(
            rows_of(&res),
            (68..100).collect::<Vec<u64>>(),
            "exactly the newest ceiling+1 rows stay searchable"
        );
        assert!(res.incomplete, "the forced cut must be reported");
        assert_eq!(res.lowest_retained_abs_row, 68);
    }
}
