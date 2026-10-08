// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Cached full-content search index.
//!
//! The index over all retained scrollback plus visible rows is a pure function
//! of which grid is active and that grid's content, while the query pattern is
//! independent, so one index serves every query until the content changes.
//! [`Terminal::indexed_search`] caches it keyed by
//! `(modes.alternate_screen, content_seq())`: history lines keyed from
//! `oldest_absolute_row()`, then visible rows at `oldest + scrollback`.
//!
//! The key cannot go stale:
//!
//! - Line text and membership change only on writes, scroll into scrollback,
//!   erases, reflow or resize; all bump the active grid's `content_gen`
//!   ([`Terminal::content_seq`]).
//! - Absolute-row keys derive from `oldest_absolute_row()`, which advances only
//!   when content scrolls off (bumping `content_gen`); resize reflows bump it
//!   too.
//! - The active grid is part of the key (the two grids have independent
//!   counters).
//!
//! A pure viewport scroll does not change the retained set or `content_seq()`,
//! so the cache is reused.
//!
//! A miss refreshes in O(churn) rather than rebuilding in O(total):
//!
//! - Append: rows at or above the previous `indexed_end` are new; feed them.
//! - Replace: rows at or above the previous visible base may have been edited
//!   in place; re-feed them (unchanged rows are cheap).
//! - EvictBelow: `drop_history_below` removes evicted rows with fresh-build
//!   semantics (complete results, zero watermark).
//! - Reflow, alt-screen switch, protected-footer splice (`absolute_row_revision`)
//!   or Kitty-unscroll renumbering (`history_renumber_epoch`): full rebuild.
//!
//! The refresh is used only when it provably equals the full rebuild (guards in
//! `try_refresh_search_index`); the differential tests compare against
//! `legacy_results`, a from-scratch oracle.

use super::Terminal;
use crate::search::{DEFAULT_MAX_CACHED_LINES, TerminalSearch};

/// A cached search index plus the cache key it was built for (see
/// [`Terminal::indexed_search`]).
pub(crate) struct CachedSearchIndex {
    /// Whether the alternate screen was active when this index was built; the
    /// grids keep independent `content_gen` counters, so a swap landing on the
    /// same sequence value must still invalidate.
    alt_screen: bool,
    /// The active grid's `content_seq()` at build time. Bumps on every content
    /// mutation, so a mismatch means the indexed text/keys may have changed.
    content_gen: u64,
    /// Protected-footer splice revision at (re)build time. A splice renumbers
    /// absolute rows piecewise, so a mismatch forces the full rebuild.
    absolute_row_revision: u64,
    /// `Grid::history_renumber_epoch()` at (re)build time. Kitty CSI +T unscroll
    /// removes the newest scrollback lines, shifting older rows' absolute keys
    /// without changing the other stamps; a mismatch forces the full rebuild
    /// and is part of the hit key.
    history_renumber_epoch: u64,
    /// Grid width at (re)build time. A width change rewraps history wholesale,
    /// forcing the full rebuild; height-only changes move rows between history
    /// and visible without moving keys, so they stay refreshable.
    cols: u16,
    /// Absolute row of the oldest retained line at (re)build time. Retention only
    /// advances it; a decrease means renumbering (full rebuild).
    hist_base: usize,
    /// Absolute row of the top visible row at (re)build time. Rows at or above
    /// it were fed as visible text and may be edited in place; rows below are
    /// immutable history. The refresh re-feeds from `min(previous, current)` so
    /// reclassified rows get the text form a fresh build would give them.
    visible_base: usize,
    /// Exclusive end of the indexed absolute-row range at (re)build time. A
    /// shrink cannot be expressed as a refresh (full rebuild).
    indexed_end: usize,
    /// The fully built index over scrollback + visible rows (keyed by absolute
    /// row). Reused verbatim while the key matches.
    index: TerminalSearch,
}

impl Terminal {
    /// A full-content search index, reusing the cache while the active grid's
    /// content is unchanged.
    ///
    /// The index covers every retained addressable line keyed by absolute row
    /// (history `0..scrollback_lines` at `oldest + i`, visible rows at
    /// `oldest + scrollback + r`), so each
    /// [`SearchMatch::line`](crate::search::SearchMatch) is an absolute row. Query
    /// with `indexed_search(...).search_results_opts(pat, case, regex)`.
    ///
    /// A hit (key `(alternate_screen, content_seq())` unchanged) returns the
    /// cached index; a miss refreshes or rebuilds it. The result is immutable so
    /// callers cannot alter cached coordinates.
    pub fn indexed_search(&mut self) -> &TerminalSearch {
        let key_alt = self.modes.alternate_screen;
        let key_gen = self.content_seq();
        // The renumber epoch is part of the hit key: a Kitty unscroll shifts
        // older rows' keys without necessarily moving `content_seq`.
        let key_epoch = self.grid.history_renumber_epoch();

        let hit = match &self.search_index {
            Some(cached) => {
                cached.alt_screen == key_alt
                    && cached.content_gen == key_gen
                    && cached.history_renumber_epoch == key_epoch
            }
            None => false,
        };

        if !hit {
            // Current retained-window geometry, shared by the refresh guards, the
            // refresh and the cached stamps; mirrors `build_search_index`.
            let oldest = self.grid.oldest_absolute_row();
            let scrollback = self.grid.scrollback_lines();
            let rows = self.rows();
            let cols = self.cols();
            let revision = self.absolute_row_revision;
            let hist_base = usize::try_from(oldest).unwrap_or(usize::MAX);
            let visible_base = hist_base.saturating_add(scrollback);
            let indexed_end = visible_base.saturating_add(usize::from(rows));

            let index = match self.try_refresh_search_index(
                key_alt,
                revision,
                key_epoch,
                cols,
                hist_base,
                visible_base,
                indexed_end,
            ) {
                Some(index) => {
                    self.search_index_refreshes = self.search_index_refreshes.wrapping_add(1);
                    index
                }
                // The full build is the fallback and the refresh's test oracle.
                None => self.build_search_index(),
            };
            self.search_index = Some(CachedSearchIndex {
                alt_screen: key_alt,
                content_gen: key_gen,
                absolute_row_revision: revision,
                history_renumber_epoch: key_epoch,
                cols,
                hist_base,
                visible_base,
                indexed_end,
                index,
            });
            self.search_index_rebuilds = self.search_index_rebuilds.wrapping_add(1);
        }

        // `self.search_index` is `Some` here: it was `Some` on a hit and we just
        // assigned it on a miss.
        &self
            .search_index
            .as_ref()
            .expect("search_index populated above")
            .index
    }

    /// Serve a cache miss by incrementally refreshing the cached index. Returns
    /// `None` unless the refresh provably equals `build_search_index` over the
    /// current window; the caller then rebuilds.
    ///
    /// Why it is equal, given the guards:
    ///
    /// - Rows below the previous visible base are immutable history: splices,
    ///   rewraps and renumberings are fenced to the rebuild arm, and retention
    ///   only drops the oldest rows (`hist_base` monotone). Every refresh
    ///   re-feeds from `min(previous, current visible base)`, so a row that
    ///   changed classification is re-fed in its new form (visible via
    ///   `get_line_text`, history via `line_text_bounded`, as the build does).
    /// - `drop_history_below` removes the evicted prefix with fresh-build
    ///   semantics.
    /// - No internal cap eviction fires on either path: the guards require an
    ///   eviction-free cached index and a window under
    ///   `DEFAULT_MAX_CACHED_LINES` (the cap `TerminalSearch::new` uses).
    ///
    /// Cost: O(appended rows + visible rows) per miss. The bloom filter keeps
    /// stale bits from replaced rows (false-positive candidates only; saturation
    /// self-heals via `rebuild_bloom`).
    #[allow(
        clippy::too_many_arguments,
        reason = "geometry tuple computed once by the caller and shared with the cache stamps"
    )]
    fn try_refresh_search_index(
        &mut self,
        key_alt: bool,
        revision: u64,
        renumber_epoch: u64,
        cols: u16,
        hist_base: usize,
        visible_base: usize,
        indexed_end: usize,
    ) -> Option<TerminalSearch> {
        {
            let cached = self.search_index.as_ref()?;
            let reusable = cached.alt_screen == key_alt
                && cached.absolute_row_revision == revision
                && cached.history_renumber_epoch == renumber_epoch
                && cached.cols == cols
                // Retention only advances; a retreat means renumbering (a
                // re-attached offloaded reflow can grow scrollback back; its width
                // change also fences, but this guard stands on its own).
                && cached.hist_base <= hist_base
                // The index cannot truncate above; a shrinking end falls back.
                && cached.indexed_end <= indexed_end
                // Cap guard: both the cached and the refreshed window must be
                // eviction-free for the fresh-equality argument to hold.
                && indexed_end.saturating_sub(hist_base) <= DEFAULT_MAX_CACHED_LINES
                && !cached.index.results_may_be_incomplete();
            if !reusable {
                return None;
            }
        }
        let cached = self.search_index.take()?;
        let mut index = cached.index;

        // EvictBelow: rows no longer retained go, with fresh-build semantics.
        index.drop_history_below(hist_base);

        // Replace + Append: re-feed from the older of the two visible bases.
        // `SearchIndex::index_line` skips unchanged rows cheaply.
        let refresh_start = cached.visible_base.min(visible_base).max(hist_base);
        use super::selection::{MAX_SCROLLBACK_LINE_SCAN_BYTES, line_text_bounded};
        if refresh_start < visible_base {
            let start_hist = refresh_start.saturating_sub(hist_base);
            let count = visible_base.saturating_sub(refresh_start);
            // Dense history walk: item `j` is `get_history_line(start_hist + j)`
            // (`None` becomes empty text), so absolute-row keys are unchanged.
            // `chain(repeat(None))` pads if the walk were ever shorter than
            // `count`.
            let lines = self
                .grid
                .history_lines_from(start_hist)
                .chain(std::iter::repeat_with(|| None))
                .take(count);
            index.index_numbered_content_owned(lines.enumerate().map(|(j, line)| {
                let absolute_row = refresh_start.saturating_add(j);
                let text = line
                    .map(|l| {
                        line_text_bounded(l.as_bytes(), MAX_SCROLLBACK_LINE_SCAN_BYTES).into_owned()
                    })
                    .unwrap_or_default();
                (absolute_row, text)
            }));
        }
        let rows = self.rows();
        index.index_numbered_content_owned((0..rows).map(|r| {
            let absolute_row = visible_base.saturating_add(usize::from(r));
            let text = self.get_line_text(i32::from(r), None).unwrap_or_default();
            (absolute_row, text)
        }));
        Some(index)
    }

    /// Build a fresh full-content index over the active grid. The line set,
    /// order and absolute-row keys here define search results.
    fn build_search_index(&self) -> TerminalSearch {
        let grid = &self.grid;
        let oldest = grid.oldest_absolute_row();
        let scrollback = grid.scrollback_lines();
        let rows = self.rows();

        let mut search = TerminalSearch::new();

        // History 0..scrollback at absolute oldest + i, via the dense history
        // walk (`Grid::history_lines_from`, item `i` == `get_history_line(i)`,
        // unreadable lines as empty placeholders so later lines keep their
        // rows). It decodes each warm block / cold page once. Text is bounded by
        // MAX_SCROLLBACK_LINE_SCAN_BYTES: a crafted checkpoint can inject a
        // multi-MiB `Line`.
        //
        // Fed streaming, moving each bounded `String` into the index, so the
        // whole scrollback text is never resident twice. The walk borrows the
        // grid immutably, so nothing can change mid-build. The `legacy_results`
        // oracle keeps the per-line collect form and checks equality.
        use super::selection::{MAX_SCROLLBACK_LINE_SCAN_BYTES, line_text_bounded};
        let hist_base = usize::try_from(oldest).unwrap_or(usize::MAX);
        let history = grid
            .history_lines_from(0)
            .chain(std::iter::repeat_with(|| None))
            .take(scrollback);
        search.index_numbered_content_owned(history.enumerate().map(|(i, line)| {
            let absolute_row = hist_base.saturating_add(i);
            let text = line
                .map(|l| {
                    line_text_bounded(l.as_bytes(), MAX_SCROLLBACK_LINE_SCAN_BYTES).into_owned()
                })
                .unwrap_or_default();
            (absolute_row, text)
        }));

        // Visible rows 0..rows at absolute oldest + scrollback + r, via the
        // combining-aware `get_line_text` so accents and ZWJ clusters survive.
        // Streamed like the history.
        let vis_base = hist_base.saturating_add(scrollback);
        search.index_numbered_content_owned((0..rows).map(|r| {
            let absolute_row = vis_base.saturating_add(usize::from(r));
            let text = self.get_line_text(i32::from(r), None).unwrap_or_default();
            (absolute_row, text)
        }));

        search
    }

    /// Number of search-index rebuilds (cache misses) so far: advances when
    /// [`indexed_search`](Self::indexed_search) rebuilds or refreshes, never on
    /// a reuse. Introspection only.
    #[must_use]
    #[inline]
    pub fn search_index_rebuilds(&self) -> u64 {
        self.search_index_rebuilds
    }

    /// Number of incremental refreshes among those misses;
    /// `search_index_rebuilds() - search_index_refreshes()` is the number of
    /// full rebuilds. Introspection only.
    #[must_use]
    #[inline]
    pub fn search_index_refreshes(&self) -> u64 {
        self.search_index_refreshes
    }

    /// Release the search index's heap: drop the cached full-content index and
    /// any budgeted search (in flight or completed and retained for
    /// `search_summary`), returning their allocations to the allocator.
    ///
    /// Dropping the whole `CachedSearchIndex` is the correct eviction: an
    /// in-place [`SearchIndex::release`](aterm_search::SearchIndex::release)
    /// would leave the cache key matching an emptied index, so the next
    /// [`indexed_search`](Self::indexed_search) would return a false empty hit.
    /// The next search rebuilds from the live buffer.
    pub fn release_search_index(&mut self) {
        self.search_index = None;
        self.budgeted_search = None;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::Terminal;

    /// Build the index the uncached way as a behavior-identity reference, so
    /// any divergence between the cached path and a per-search rebuild shows as
    /// a result mismatch.
    pub(crate) fn legacy_results(t: &Terminal, pat: &str) -> Vec<(usize, usize, usize)> {
        use crate::search::TerminalSearch;
        let grid = t.grid();
        let oldest = grid.oldest_absolute_row();
        let scrollback = grid.scrollback_lines();
        let rows = t.rows() as usize;
        let mut search = TerminalSearch::new();
        let history: Vec<String> = (0..scrollback)
            .map(|i| {
                grid.get_history_line(i)
                    .map(|l| l.to_string())
                    .unwrap_or_default()
            })
            .collect();
        let hist_base = usize::try_from(oldest).unwrap_or(usize::MAX);
        search.index_visible_content(hist_base, &history);
        let visible: Vec<String> = (0..rows)
            .map(|r| t.get_line_text(r as i32, None).unwrap_or_default())
            .collect();
        let vis_base = hist_base.saturating_add(scrollback);
        search.index_visible_content(vis_base, &visible);
        let res = search
            .search_results_opts(pat, false, false)
            .expect("search ok");
        res.matches
            .iter()
            .map(|m| (m.line, m.start_col, m.len()))
            .collect()
    }

    /// A terminal whose history runs through a tiered store holding a corrupt
    /// warm block between its cold tier and its healthy warm blocks, with a
    /// needle on both sides: a skipping history reader would shift later lines
    /// onto their neighbours' rows.
    pub(crate) fn corrupt_tiered_terminal() -> (Terminal, usize) {
        use aterm_grid::Grid;
        use aterm_scrollback::Scrollback;
        // Cold holds the oldest lines (needle at 2), hot the newest (needle at
        // 38); the corrupt block is injected at the warm front, between them,
        // and nothing afterwards evicts it.
        let mut sb = Scrollback::with_block_size(4, 12, 10_000_000, 4);
        for i in 0..40 {
            match i {
                2 => sb.push_str("NEEDLE_before the corrupt block"),
                38 => sb.push_str("NEEDLE_after the corrupt block"),
                _ => sb.push_str(&format!("carried {i:03}")),
            }
        }
        assert!(
            sb.cold_line_count() > 2,
            "the first needle must sit in cold"
        );
        let corrupt = 5;
        sb.inject_corrupted_warm_block(corrupt);
        let mut t = Terminal::with_grid(Grid::with_tiered_scrollback(6, 40, 5, sb));
        for i in 0..20 {
            t.process(format!("live {i:03}\r\n").as_bytes());
        }
        assert_eq!(
            t.grid()
                .history_lines_from(0)
                .filter(Option::is_none)
                .count(),
            corrupt,
            "the fixture's corrupt block is still in the store"
        );
        (t, corrupt)
    }

    /// Over a corrupt tiered segment the cached index's matches equal the
    /// per-line oracle's (same lines, same absolute rows), and the needle after
    /// the corrupt block lands on its own row.
    #[test]
    fn search_over_a_corrupt_tiered_segment_keeps_absolute_rows() {
        let (mut t, corrupt) = corrupt_tiered_terminal();
        let got = cached_results(&mut t, "NEEDLE_");
        assert_eq!(got, legacy_results(&t, "NEEDLE_"));
        assert_eq!(got.len(), 2, "both needles are found: {got:?}");
        let oldest = usize::try_from(t.grid().oldest_absolute_row()).expect("row");
        // The corrupt block's placeholder rows sit between the two needles.
        assert_eq!(got[0].0, oldest + 2);
        assert_eq!(got[1].0, oldest + 38 + corrupt);
    }

    pub(crate) fn cached_results(t: &mut Terminal, pat: &str) -> Vec<(usize, usize, usize)> {
        let res = t
            .indexed_search()
            .search_results_opts(pat, false, false)
            .expect("search ok");
        res.matches
            .iter()
            .map(|m| (m.line, m.start_col, m.len()))
            .collect()
    }

    /// Two identical searches with no content change return identical results
    /// and the second reuses the cache; a search after a write rebuilds and
    /// sees the new content. Cached results always equal the oracle's.
    #[test]
    fn repeat_search_reuses_cache_write_invalidates() {
        let mut t = Terminal::new(6, 40);
        // Push a needle off-screen into scrollback, plus filler.
        t.process(b"NEEDLE_alpha\r\n");
        for i in 0..20 {
            t.process(format!("filler line {i}\r\n").as_bytes());
        }

        // First search: a cache miss -> exactly one rebuild.
        let before = t.search_index_rebuilds();
        let r1 = cached_results(&mut t, "NEEDLE_alpha");
        assert_eq!(
            t.search_index_rebuilds(),
            before + 1,
            "first search must rebuild the index once"
        );
        assert_eq!(
            r1,
            legacy_results(&t, "NEEDLE_alpha"),
            "results must match the legacy index"
        );
        assert_eq!(r1.len(), 1, "the scrolled-off needle is found exactly once");

        // Second identical search, no content change -> cache hit, no rebuild,
        // byte-identical results.
        let rebuilds_after_first = t.search_index_rebuilds();
        let r2 = cached_results(&mut t, "NEEDLE_alpha");
        assert_eq!(
            t.search_index_rebuilds(),
            rebuilds_after_first,
            "the repeat search must REUSE the cache (no rebuild) — the O(1) win"
        );
        assert_eq!(
            r1, r2,
            "reused-cache results must be identical to the first search"
        );

        // A different pattern still reuses the same index (the per-query pattern
        // is independent of the indexed content) — still no rebuild.
        let r_filler = cached_results(&mut t, "filler");
        assert_eq!(
            t.search_index_rebuilds(),
            rebuilds_after_first,
            "a different pattern on unchanged content must NOT rebuild"
        );
        assert_eq!(r_filler, legacy_results(&t, "filler"));
        assert!(r_filler.len() >= 2, "many filler rows match");

        // Now write new content: the next search must rebuild and reflect it.
        t.process(b"NEEDLE_beta later\r\n");
        let rebuilds_before_write_search = t.search_index_rebuilds();
        let r_beta = cached_results(&mut t, "NEEDLE_beta");
        assert_eq!(
            t.search_index_rebuilds(),
            rebuilds_before_write_search + 1,
            "a search after a content write must rebuild the index"
        );
        assert_eq!(r_beta, legacy_results(&t, "NEEDLE_beta"));
        assert_eq!(r_beta.len(), 1, "the freshly written needle is found");

        // And the original needle is still found post-rebuild (content retained).
        assert_eq!(cached_results(&mut t, "NEEDLE_alpha"), r1);
    }

    /// A height-only resize leaves search fresh and complete: the cache
    /// invalidates and the rebuilt index covers the full retained history with
    /// unshifted absolute rows.
    #[test]
    fn height_only_resize_keeps_history_searchable_with_stable_rows() {
        let mut t = Terminal::new(10, 40);
        for i in 0..30 {
            t.process(format!("needle-{i}\r\n").as_bytes());
        }
        let before = cached_results(&mut t, "needle-");
        assert_eq!(before.len(), 30, "every written line matches pre-resize");

        // Height-only shrink (cols unchanged).
        t.resize(5, 40);
        let after = cached_results(&mut t, "needle-");
        assert_eq!(
            after.len(),
            30,
            "history stays searchable across a height-only resize"
        );
        assert_eq!(
            after,
            legacy_results(&t, "needle-"),
            "the post-resize cache equals a from-scratch rebuild (not stale)"
        );
        // Lines that did not move (deep history) keep their absolute rows.
        let hit = |t: &mut Terminal, pat: &str| cached_results(t, pat)[0].0;
        assert_eq!(hit(&mut t, "needle-0"), 0, "unmoved line keeps its row");
        assert_eq!(hit(&mut t, "needle-20"), 20, "unmoved line keeps its row");
        assert_eq!(t.grid().oldest_absolute_row(), 0, "nothing was evicted");
    }

    /// A retention shrink is a content change: right after
    /// `set_scrollback_line_limit` evicts lines, search returns no evicted
    /// absolute rows. Pinned on both retention arms: the tiered store (native
    /// sessions) and the ring-only grid (the wasm engines' shape).
    #[test]
    fn retention_shrink_invalidates_the_cached_index() {
        // tiered arm: a small ring so the bulk of history lives in the store.
        let sb = aterm_scrollback::Scrollback::new(64, 512, 8_000_000);
        let mut tiered = Terminal::with_scrollback(6, 40, 8, sb);
        let check = |t: &mut Terminal, label: &str| {
            for i in 0..200 {
                t.process(format!("needle-{i}\r\n").as_bytes());
            }
            let before = cached_results(t, "needle-");
            assert_eq!(before.len(), 200, "{label}: all lines retained pre-shrink");

            t.set_scrollback_line_limit(Some(50));

            let after = cached_results(t, "needle-");
            assert_eq!(
                after,
                legacy_results(t, "needle-"),
                "{label}: the post-shrink search equals a from-scratch rebuild (not stale)"
            );
            let oldest = usize::try_from(t.grid().oldest_absolute_row()).unwrap();
            assert!(
                after.iter().all(|&(line, _, _)| line >= oldest),
                "{label}: no match may point at an evicted absolute row (< {oldest})"
            );
            assert!(
                after.len() < before.len(),
                "{label}: the shrink really evicted matches"
            );
        };
        check(&mut tiered, "tiered");

        // Ring-only arm (`Terminal::new`, no store).
        let mut ring = Terminal::new(6, 40);
        check(&mut ring, "ring-only");
    }

    /// A memory-budget shrink is a content change too: `set_memory_budget`
    /// evicts cold-tier scrollback, so the search cache must invalidate rather
    /// than keep returning evicted rows until the next write.
    #[test]
    fn memory_budget_shrink_invalidates_the_cached_index() {
        // Small hot/warm tiers so the bulk of history spills to the compressible
        // cold tier, where a tight budget can evict the oldest lines.
        let sb = aterm_scrollback::Scrollback::new(8, 16, 64_000_000);
        let mut t = Terminal::with_scrollback(6, 40, 8, sb);
        for i in 0..400 {
            t.process(format!("needle-{i}\r\n").as_bytes());
        }
        // Prime the cached index over the full retained set.
        let before = cached_results(&mut t, "needle-");
        assert_eq!(before.len(), 400, "all lines retained + found pre-shrink");
        let lines_before = t.grid().scrollback_lines();

        // Shrink the budget hard, evicting the oldest cold lines. An
        // EnforcementFailed (the hot tier holds active data) is ignored; the cold
        // eviction still happened.
        let _ = t.set_memory_budget(4096);
        assert!(
            t.grid().scrollback_lines() < lines_before,
            "a tight budget must really evict retained lines \
             ({} -> {})",
            lines_before,
            t.grid().scrollback_lines()
        );

        // No write since the prime: the cache must still rebuild.
        let after = cached_results(&mut t, "needle-");
        assert_eq!(
            after,
            legacy_results(&t, "needle-"),
            "the post-budget-shrink search equals a from-scratch rebuild (not stale)"
        );
        let oldest = usize::try_from(t.grid().oldest_absolute_row()).unwrap();
        assert!(
            after.iter().all(|&(line, _, _)| line >= oldest),
            "no match may point at an evicted absolute row (< {oldest})"
        );
        assert!(
            after.len() < before.len(),
            "the budget shrink really evicted matches ({} -> {})",
            before.len(),
            after.len()
        );
    }

    /// `release_search_index` drops the cache so the next search rebuilds, with
    /// identical results (never a false empty hit).
    #[test]
    fn release_drops_cache_then_next_search_rebuilds_identically() {
        let mut t = Terminal::new(6, 40);
        t.process(b"NEEDLE_alpha\r\n");
        for i in 0..20 {
            t.process(format!("filler line {i}\r\n").as_bytes());
        }
        let r1 = cached_results(&mut t, "NEEDLE_alpha");
        assert_eq!(r1.len(), 1);
        let rebuilds = t.search_index_rebuilds();

        // Release with no content change: the next search must rebuild, not hit
        // a matching key over an emptied index.
        t.release_search_index();
        let r2 = cached_results(&mut t, "NEEDLE_alpha");
        assert_eq!(
            t.search_index_rebuilds(),
            rebuilds + 1,
            "release must force the next search to rebuild (real reclaim)"
        );
        assert_eq!(
            r2, r1,
            "post-release results are byte-identical (not empty)"
        );
    }

    /// A pure viewport scroll does not bump `content_seq`, so the cache is
    /// reused with identical results.
    #[test]
    fn viewport_scroll_reuses_cache() {
        let mut t = Terminal::new(6, 40);
        t.process(b"NEEDLE_alpha\r\n");
        for i in 0..20 {
            t.process(format!("filler line {i}\r\n").as_bytes());
        }
        let r1 = cached_results(&mut t, "NEEDLE_alpha");
        let rebuilds = t.search_index_rebuilds();
        let gen_before = t.content_seq();

        // Scroll the viewport up into scrollback (content unchanged).
        t.grid_mut().scroll_display(5);
        assert_eq!(
            t.content_seq(),
            gen_before,
            "a pure viewport scroll must NOT bump content_seq"
        );

        let r2 = cached_results(&mut t, "NEEDLE_alpha");
        assert_eq!(
            t.search_index_rebuilds(),
            rebuilds,
            "a pure viewport scroll must NOT invalidate the search cache"
        );
        assert_eq!(r1, r2, "results identical across a viewport scroll");
    }

    /// `restore_checkpoint` replaces the grids, so every stamp the cache keyed
    /// on restarts from fresh values and could collide; the release in
    /// `restore_checkpoint` forces a rebuild. Pins the results and the rebuild.
    #[test]
    fn restore_checkpoint_drops_the_cached_search_index() {
        let mut source = Terminal::new(6, 40);
        for i in 0..30 {
            source.process(format!("alpha {i} needle\r\n").as_bytes());
        }
        let saved = source.checkpoint();

        // A different terminal, with its own primed cache over other content.
        let mut target = Terminal::new(6, 40);
        for i in 0..30 {
            target.process(format!("beta {i} other\r\n").as_bytes());
        }
        let primed = cached_results(&mut target, "needle");
        assert!(
            primed.is_empty(),
            "precondition: no needle before the restore"
        );

        target.restore_checkpoint(&saved);
        let rebuilds_before = target.search_index_rebuilds();
        let after = cached_results(&mut target, "needle");
        assert_eq!(
            target.search_index_rebuilds(),
            rebuilds_before + 1,
            "the restored buffer must be indexed afresh, never served from the \
             pre-restore cache"
        );
        assert_eq!(
            after,
            legacy_results(&target, "needle"),
            "post-restore search must equal a from-scratch rebuild"
        );
        assert!(!after.is_empty(), "the restored content is searchable");
    }

    /// Scripted differential: one terminal driven through a long deterministic
    /// mix of appends, in-place edits, erases, scrollback clears, height and
    /// width resizes, alt-screen switches, retention shrinks and Kitty
    /// unscrolls, searching after every step; the cached path must equal the
    /// `legacy_results` oracle each time. The script must exercise both the
    /// refresh and the full-rebuild arms.
    #[test]
    fn scripted_mutation_mixture_keeps_the_refresh_equal_to_a_rebuild() {
        let sb = aterm_scrollback::Scrollback::new(64, 512, 8_000_000);
        let mut t = Terminal::with_scrollback(8, 40, 64, sb);
        t.set_scrollback_line_limit(Some(600));

        // Deterministic xorshift: a fixed script, replayable byte for byte.
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut roll = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        let mut written = 0usize;
        let mut on_alt = false;
        let mut deepest = 0usize;
        let full_rebuilds = |t: &Terminal| t.search_index_rebuilds() - t.search_index_refreshes();
        let refreshes_at_start = t.search_index_refreshes();
        let fulls_at_start = full_rebuilds(&t);

        for step in 0..140u64 {
            let r = roll();
            match r % 12 {
                // Ordinary streaming output — the refresh arm's home turf, and
                // the majority of the script so history really accumulates.
                0..=6 => {
                    for _ in 0..=(r >> 8) % 14 {
                        t.process(format!("needle {written} alpha row\r\n").as_bytes());
                        written += 1;
                    }
                }
                // In-place edit of visible rows (the only mutable rows).
                7 => {
                    t.process(b"\x1b[H");
                    t.process(format!("beta {step} needle overwrite").as_bytes());
                }
                // Erases: the screen, and (rarely) the scrollback itself.
                8 => {
                    if r % 48 == 8 {
                        t.process(b"\x1b[3J");
                    } else {
                        t.process(b"\x1b[2J");
                    }
                }
                // Resizes: height-only reclassifies rows without moving keys;
                // a width change rewraps history wholesale (rebuild fence).
                9 => {
                    if r % 2 == 0 {
                        let cols = t.cols();
                        t.resize(4 + u16::try_from(r % 8).unwrap_or(0), cols);
                    } else {
                        let rows = t.rows();
                        t.resize(rows, 24 + u16::try_from(r % 24).unwrap_or(0));
                    }
                }
                // Alt-screen switch (rebuild fence, both directions).
                10 => {
                    if on_alt {
                        t.process(b"\x1b[?1049l");
                    } else {
                        t.process(b"\x1b[?1049h");
                    }
                    on_alt = !on_alt;
                }
                // Kitty CSI +T unscroll (history renumbered, seen only via the
                // epoch), plus an occasional retention shrink.
                _ => {
                    if !on_alt {
                        let n = 1 + usize::try_from(r % 3).unwrap_or(0);
                        let _ = t.grid_mut().unscroll_from_scrollback(n);
                    }
                    if r % 3 == 0 {
                        t.set_scrollback_line_limit(Some(
                            250 + usize::try_from(r % 350).unwrap_or(0),
                        ));
                    }
                }
            }
            deepest = deepest.max(t.grid().scrollback_lines());

            for query in ["needle", "alpha", "beta"] {
                assert_eq!(
                    cached_results(&mut t, query),
                    legacy_results(&t, query),
                    "step {step} (roll {r:#x}), query {query:?}: the cached path \
                     must equal a from-scratch rebuild"
                );
            }
        }

        assert!(
            deepest >= 150,
            "the script must build REAL history depth for the oracle to mean \
             anything (deepest scrollback was {deepest})"
        );
        assert!(
            t.search_index_refreshes() > refreshes_at_start,
            "the script must exercise the incremental refresh arm"
        );
        assert!(
            full_rebuilds(&t) > fulls_at_start,
            "the script must also exercise the full-rebuild fallback arm"
        );
    }

    /// After ordinary streaming output a miss is served by an incremental
    /// refresh, and every result equals the oracle. Refreshes must advance and
    /// full rebuilds must not.
    #[test]
    fn streaming_output_refreshes_incrementally_and_equals_legacy() {
        let mut t = Terminal::new(6, 40);
        for i in 0..50 {
            t.process(format!("seed line {i} needle\r\n").as_bytes());
        }
        let r0 = cached_results(&mut t, "needle");
        assert_eq!(r0, legacy_results(&t, "needle"));
        let full_rebuilds = |t: &Terminal| t.search_index_rebuilds() - t.search_index_refreshes();
        let full_before = full_rebuilds(&t);

        for batch in 0..5 {
            for i in 0..8 {
                t.process(format!("batch {batch} line {i} needle\r\n").as_bytes());
            }
            let refreshes_before = t.search_index_refreshes();
            let got = cached_results(&mut t, "needle");
            assert_eq!(
                t.search_index_refreshes(),
                refreshes_before + 1,
                "a streaming-output miss must be served by the churn refresh"
            );
            assert_eq!(
                got,
                legacy_results(&t, "needle"),
                "refreshed results must equal a from-scratch rebuild"
            );
        }
        assert_eq!(
            full_rebuilds(&t),
            full_before,
            "no full O(total) rebuild may hide inside the churn loop"
        );
    }

    /// Ring-cap eviction during streaming (every capped session's steady
    /// state): the refresh drops the evicted prefix with fresh-build semantics,
    /// matching the oracle's matches and completeness.
    #[test]
    fn ring_eviction_steady_state_refreshes_and_equals_legacy() {
        let mut t = crate::terminal::TerminalBuilder::new()
            .size(6, 40)
            .ring_buffer_size(30)
            .build();
        for i in 0..60 {
            t.process(format!("fill {i} needle\r\n").as_bytes());
        }
        let _ = cached_results(&mut t, "needle");
        // Single-row advances retain a compressed byte prefix; enough cycles
        // force several amortized compactions. Include Unicode columns and
        // overlapping case-folded postings in the fresh-index comparison.
        for batch in 0..160 {
            let oldest_before = t.grid().oldest_absolute_row();
            t.process(format!("more {batch} NEEDLE 日本語 needle\r\n").as_bytes());
            assert!(
                t.grid().oldest_absolute_row() > oldest_before,
                "precondition: the full ring must be evicting between searches"
            );
            let refreshes_before = t.search_index_refreshes();
            let got = cached_results(&mut t, "needle");
            assert_eq!(
                t.search_index_refreshes(),
                refreshes_before + 1,
                "ring-eviction churn must stay on the refresh path"
            );
            assert_eq!(got, legacy_results(&t, "needle"));
            for query in ["NEEDLE", "日本語", "more"] {
                assert_eq!(cached_results(&mut t, query), legacy_results(&t, query));
            }
            let res = t
                .indexed_search()
                .search_results_opts("needle", false, false)
                .expect("search ok");
            assert!(
                !res.incomplete,
                "grid-retention drops are fresh-build-complete, never sticky-incomplete"
            );
        }
    }

    /// A width change (history rewrapped) and a Kitty CSI +T unscroll (history
    /// renumbered) both fall back to the full rebuild. The epoch assertion
    /// guards `Grid::history_renumber_epoch` wiring.
    #[test]
    fn width_change_and_unscroll_rebuild_instead_of_refreshing() {
        // Width-change arm.
        let mut t = Terminal::new(6, 40);
        for i in 0..40 {
            t.process(format!("wrapline {i} needle\r\n").as_bytes());
        }
        let _ = cached_results(&mut t, "needle");
        let refreshes = t.search_index_refreshes();
        let epoch = t.grid().history_renumber_epoch();
        t.resize(6, 33);
        // Wiring guard: the refresh assertion below also holds via the
        // `cached.cols == cols` fence, so it alone would pass without the
        // width-reflow epoch bump (`scrollback_reflow.rs`), and a later width
        // change restoring `cols` would then refresh over renumbered keys.
        assert!(
            t.grid().history_renumber_epoch() > epoch,
            "a width rewrap renumbers history and must advance the epoch"
        );
        let got = cached_results(&mut t, "needle");
        assert_eq!(
            t.search_index_refreshes(),
            refreshes,
            "a width change must NOT take the refresh arm (history rewrapped)"
        );
        assert_eq!(got, legacy_results(&t, "needle"));

        // Unscroll arm: a tiered store so unscroll really removes the newest
        // history lines (ring-only grids route to a plain region scroll).
        let sb = aterm_scrollback::Scrollback::new(8, 64, 8_000_000);
        let mut t = Terminal::with_scrollback(6, 40, 8, sb);
        for i in 0..80 {
            t.process(format!("uline {i} needle\r\n").as_bytes());
        }
        let _ = cached_results(&mut t, "needle");
        let epoch = t.grid().history_renumber_epoch();
        let removed = t.grid_mut().unscroll_from_scrollback(3);
        assert!(
            removed > 0,
            "precondition: unscroll must remove history lines"
        );
        assert!(
            t.grid().history_renumber_epoch() > epoch,
            "unscroll must advance the renumber epoch (the rebuild fence)"
        );
        let refreshes = t.search_index_refreshes();
        let got = cached_results(&mut t, "needle");
        assert_eq!(
            t.search_index_refreshes(),
            refreshes,
            "a history renumbering must force a full rebuild"
        );
        assert_eq!(got, legacy_results(&t, "needle"));
    }
}
