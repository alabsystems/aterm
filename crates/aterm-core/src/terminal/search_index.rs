// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! P1.0b: O(1) reuse of the full-content search index.
//!
//! `cmd_search` historically rebuilt a fresh [`TerminalSearch`] over the ENTIRE
//! retained scrollback + visible rows on every socket call (~459 ms at 50k
//! lines — ~100× the cost of the actual query). The index is a *pure function*
//! of `(which grid is active, that grid's content)`, while the per-query pattern
//! is independent, so the same index can be reused across queries until the
//! content changes.
//!
//! [`Terminal::indexed_search`] caches the built index keyed by
//! `(modes.alternate_screen, content_seq())` and rebuilds only on a key miss.
//! The rebuild reproduces the EXACT indexing `cmd_search` used to do inline —
//! history lines keyed at `oldest_absolute_row()`, then visible rows keyed at
//! `oldest + scrollback` — so search RESULTS (matches, order, absolute-row
//! numbers, INCOMPLETE/eviction semantics) are byte-identical to the old path.
//!
//! ## Why the key cannot go stale
//!
//! The indexed set is the text of every retained addressable line plus each
//! line's absolute-row key. Both inputs are captured by the cache key:
//!
//! - **Line text & set membership** — changes only when cells are written, the
//!   screen scrolls content into scrollback, lines are erased, or the grid is
//!   reflowed/resized. Every such path bumps the active grid's `content_gen`
//!   (forwarded by [`Terminal::content_seq`]).
//! - **Absolute-row keys** — derived from `oldest_absolute_row()` (and
//!   `scrollback_lines()` / `rows()`). `oldest_absolute_row()` advances only when
//!   content scrolls off (a `scroll_up`, which bumps `content_gen`); resize
//!   changes `rows`/`scrollback` via a reflow that also bumps `content_gen`.
//! - **Active grid (main vs. alternate screen)** — swapping screens changes the
//!   whole indexed buffer, captured by the `alt_screen` key component. (The two
//!   grids keep independent `content_gen` counters, so the boolean disambiguates
//!   the otherwise-shared sequence space.)
//!
//! A pure viewport / `display_offset` scroll deliberately does NOT change the
//! retained set (the index always covers ALL retained lines, not the visible
//! page) and correctly does NOT bump `content_seq()`, so the cache is reused —
//! the desired O(1) win — without going stale.

use super::Terminal;
use crate::search::TerminalSearch;

/// A cached search index plus the cache key it was built for.
///
/// See [`Terminal::indexed_search`]. Stored in `Terminal::search_index`.
pub(crate) struct CachedSearchIndex {
    /// Whether the alternate screen was active when this index was built. Part
    /// of the cache key: the alt grid and main grid keep independent
    /// `content_gen` counters, so a screen swap that happens to land on the same
    /// sequence value must still invalidate.
    alt_screen: bool,
    /// The active grid's `content_seq()` at build time. Bumps on every content
    /// mutation, so a mismatch means the indexed text/keys may have changed.
    content_gen: u64,
    /// The fully built index over scrollback + visible rows (keyed by absolute
    /// row). Reused verbatim while the key matches.
    index: TerminalSearch,
}

impl Terminal {
    /// Return a full-content search index, reusing the cache when the active
    /// grid's content is unchanged (P1.0b — the O(1) win).
    ///
    /// The returned index covers EVERY still-retained addressable line keyed by
    /// ABSOLUTE row — scrollback history `0..scrollback_lines` at absolute
    /// `oldest + i`, then visible rows `0..rows` at absolute
    /// `oldest + scrollback + r` — so each
    /// [`SearchMatch::line`](crate::search::SearchMatch) is already an absolute
    /// row. Run the per-query pattern with
    /// `indexed_search(...).search_results_opts(pat, case, regex)`.
    ///
    /// On a cache hit (key `(alternate_screen, content_seq())` unchanged) this
    /// returns the cached index WITHOUT rebuilding. On a miss it rebuilds the
    /// index identically to the legacy inline `cmd_search` indexing — producing
    /// byte-identical results — then caches it under the new key.
    ///
    /// `&mut self` is required because the cache lives on the terminal; the
    /// returned `&TerminalSearch` is immutable so callers cannot mutate cached
    /// coordinates.
    pub fn indexed_search(&mut self) -> &TerminalSearch {
        let key_alt = self.modes.alternate_screen;
        let key_gen = self.content_seq();

        let hit = match &self.search_index {
            Some(cached) => cached.alt_screen == key_alt && cached.content_gen == key_gen,
            None => false,
        };

        if !hit {
            let index = self.build_search_index();
            self.search_index = Some(CachedSearchIndex {
                alt_screen: key_alt,
                content_gen: key_gen,
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

    /// Build a fresh full-content index over the active grid.
    ///
    /// This is the EXACT indexing `cmd_search` performed inline before P1.0b —
    /// kept here (where the grid + `get_line_text` + `TerminalSearch` all live)
    /// so the cached and uncached paths are guaranteed identical. Any change to
    /// the line set, ordering, or absolute-row keys here would change search
    /// results, so it must mirror the legacy loop exactly.
    fn build_search_index(&self) -> TerminalSearch {
        let grid = &self.grid;
        let oldest = grid.oldest_absolute_row();
        let scrollback = grid.scrollback_lines();
        let rows = self.rows();

        let mut search = TerminalSearch::new();

        // Scrollback history 0..scrollback → absolute oldest + i. Line text via
        // `get_history_line` (the same source the legacy loop used), but bounded:
        // a stored `Line`'s byte length is unbounded (a crafted checkpoint can
        // inject a multi-MiB `Line`), so a plain `to_string()` here would allocate
        // the whole line per row — the same memory-amplification DoS closed in
        // `get_line_text`, reachable via the control `search` path. Legitimate lines
        // are far under the ceiling, so this never changes indexed content.
        use super::selection::{MAX_SCROLLBACK_LINE_SCAN_BYTES, line_text_bounded};
        let history: Vec<String> = (0..scrollback)
            .map(|i| {
                grid.get_history_line(i)
                    .map(|l| line_text_bounded(l.as_bytes(), MAX_SCROLLBACK_LINE_SCAN_BYTES))
                    .unwrap_or_default()
            })
            .collect();
        let hist_base = usize::try_from(oldest).unwrap_or(usize::MAX);
        search.index_visible_content(hist_base, &history);

        // Visible rows 0..rows → absolute oldest + scrollback + r. Combining-aware
        // `get_line_text` so accents / ZWJ clusters survive (FIDELITY I-1).
        // `rows` is a u16, so `i32::from` is lossless (mirrors the legacy
        // `r as i32` where `r` was a u16-bounded usize).
        let visible: Vec<String> = (0..rows)
            .map(|r| self.get_line_text(i32::from(r), None).unwrap_or_default())
            .collect();
        let vis_base = hist_base.saturating_add(scrollback);
        search.index_visible_content(vis_base, &visible);

        search
    }

    /// Number of full search-index REBUILDS (cache misses) performed so far.
    ///
    /// Monotonic; advances by one each time [`indexed_search`](Self::indexed_search)
    /// rebuilds the index (content changed or first call) and never on a reuse.
    /// A repeat query with no intervening content change leaves this unchanged —
    /// the observable signature of the O(1) cache hit. Introspection only.
    #[must_use]
    #[inline]
    pub fn search_index_rebuilds(&self) -> u64 {
        self.search_index_rebuilds
    }

    /// Release the search index's heap: drop both the cached full-content index
    /// and any budgeted search — in-flight OR retained-completed (fed E-1: a
    /// completed scan keeps its index for `search_summary` to read; release is
    /// the eviction that frees it) — so their grown allocations return to the
    /// allocator (fed E-1 `search_index_release` — real federation eviction of a
    /// dormant pane's footprint, not a logical clear that retains capacity).
    ///
    /// Dropping the whole `CachedSearchIndex` reclaims strictly more than
    /// [`SearchIndex::release`](aterm_search::SearchIndex::release) (the entire
    /// struct, not just its containers) AND is the ONLY correct eviction: an
    /// in-place `release()` would leave the cache KEY (`content_seq`) matching an
    /// emptied index, so the next [`indexed_search`](Self::indexed_search) would
    /// return zero matches as a false cache hit. After release the next search
    /// rebuilds from the live buffer — byte-identical results, one rebuild paid.
    pub fn release_search_index(&mut self) {
        self.search_index = None;
        self.budgeted_search = None;
    }
}

#[cfg(test)]
mod tests {
    use super::Terminal;

    /// Build the index the legacy (uncached) way for a behavior-identity
    /// reference. Mirrors what `cmd_search` used to do inline AND what
    /// `build_search_index` does now — so any divergence between the cached
    /// path and "rebuild every time" surfaces as a result mismatch.
    fn legacy_results(t: &Terminal, pat: &str) -> Vec<(usize, usize, usize)> {
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

    fn cached_results(t: &mut Terminal, pat: &str) -> Vec<(usize, usize, usize)> {
        let res = t
            .indexed_search()
            .search_results_opts(pat, false, false)
            .expect("search ok");
        res.matches
            .iter()
            .map(|m| (m.line, m.start_col, m.len()))
            .collect()
    }

    /// Two consecutive identical searches with NO content change between them
    /// return IDENTICAL results, and the second REUSES the cache (no rebuild) —
    /// the O(1) win. Then a search AFTER a content write rebuilds and reflects
    /// the new content. Throughout, the cached results equal the legacy
    /// (rebuild-every-time) results: behavior-identity is non-negotiable.
    #[test]
    fn repeat_search_reuses_cache_write_invalidates() {
        let mut t = Terminal::new(6, 40);
        // Push a needle off-screen into scrollback, plus filler.
        t.process(b"NEEDLE_alpha\r\n");
        for i in 0..20 {
            t.process(format!("filler line {i}\r\n").as_bytes());
        }

        // First search: a cache MISS -> exactly one rebuild.
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

        // Second IDENTICAL search, no content change -> cache HIT, NO rebuild,
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

        // A DIFFERENT pattern still reuses the SAME index (the per-query pattern
        // is independent of the indexed content) — still no rebuild.
        let r_filler = cached_results(&mut t, "filler");
        assert_eq!(
            t.search_index_rebuilds(),
            rebuilds_after_first,
            "a different pattern on unchanged content must NOT rebuild"
        );
        assert_eq!(r_filler, legacy_results(&t, "filler"));
        assert!(r_filler.len() >= 2, "many filler rows match");

        // Now WRITE new content: the next search must REBUILD and reflect it.
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

    /// A HEIGHT-ONLY resize must leave search fresh AND complete: the cache
    /// invalidates (content_gen bumps in resize) and the rebuilt index still
    /// covers the full retained history. Pre-fix this looked like "search
    /// staleness after a height-only resize" from the host: the ring-only
    /// rows-only resize dropped ALL ring history and renumbered the
    /// survivors, so post-resize searches lost every scrollback match and
    /// reported shifted absolute rows (orc port finding).
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

    /// Retention shrink IS a content change (introspection-harness finding):
    /// immediately after `set_scrollback_line_limit` evicts lines, a search
    /// must not return absolute rows that `line <abs>` already reports as
    /// evicted — pre-fix the index only re-synced on the NEXT write. Pinned on
    /// BOTH retention arms: the tiered store (native sessions — the arm the
    /// harness caught, whose truncation never bumped `content_gen`) and the
    /// ring-only grid (the wasm engines' shape).
    #[test]
    fn retention_shrink_invalidates_the_cached_index() {
        // TIERED arm: a small ring so the bulk of history lives in the store.
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

        // RING-ONLY arm (`Terminal::new` — no store).
        let mut ring = Terminal::new(6, 40);
        check(&mut ring, "ring-only");
    }

    /// A memory-BUDGET shrink IS a content change too: `set_memory_budget`
    /// evicts retained cold-tier scrollback (handle_memory_pressure), so — like
    /// `set_scrollback_line_limit` — the content_gen-keyed search index must be
    /// invalidated, or a cached search keeps returning absolute rows that
    /// `line <abs>` already reports evicted, until the next write. The budget
    /// path only called `clamp_display_offset` (viewport-only, no content bump),
    /// never `mark_content_full` — the exact class of the fixed line-limit bug.
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

        // Shrink the scrollback budget hard: evicts the oldest cold lines,
        // advancing oldest_absolute_row(). (Ignore any EnforcementFailed — the
        // hot tier holds active data that cannot be evicted; the cold eviction
        // still happened, which is what this pins.)
        let _ = t.set_memory_budget(4096);
        assert!(
            t.grid().scrollback_lines() < lines_before,
            "a tight budget must really evict retained lines \
             ({} -> {})",
            lines_before,
            t.grid().scrollback_lines()
        );

        // No content write since the prime: pre-fix the cache is REUSED and
        // returns evicted absolute rows; post-fix mark_content_full rebuilds it.
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

    /// `release_search_index` drops the cache so the next search REBUILDS (heap
    /// reclaimed) yet returns byte-identical results — the federation-eviction
    /// contract: real reclaim, never a false empty cache hit.
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

        // Release with NO content change: an in-place clear would leave the
        // content_seq key matching an emptied index (false hit, zero matches);
        // dropping forces a rebuild.
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

    /// A pure viewport scroll (display_offset) must NOT bump `content_seq`, so it
    /// must NOT invalidate the cache — the index already covers ALL retained
    /// lines regardless of which page is visible. Results stay identical.
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
}
