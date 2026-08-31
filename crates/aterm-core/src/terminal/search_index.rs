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
//!
//! ## E2 terminal-side increment: a miss refreshes O(churn), not O(total)
//!
//! P1.0b made the REPEAT query O(1); a miss still paid a full O(total-retained)
//! rebuild (~459 ms at 50k lines) for ANY content change — one echoed keystroke
//! included. That invalidation granularity ("anything changed anywhere") is the
//! root cost the E2 lifecycle redesign names; the terminal-side hook-up here
//! applies its event alphabet derived at the search boundary from retained-
//! window arithmetic, the same churn-bounded scheme the GUI snapshot cache
//! already ships (`control_query.rs`):
//!
//! - **Append**: rows at/above the previous `indexed_end` are new — feed them.
//! - **Replace**: rows at/above the previous VISIBLE base may have been edited
//!   in place (the only in-place-mutable rows) — re-feed them all; the index's
//!   unchanged-row skip makes the untouched ones cheap.
//! - **EvictBelow**: `oldest_absolute_row()` only advances when the grid really
//!   dropped rows — `drop_history_below` removes them with FRESH-BUILD
//!   semantics (complete results, zero watermark), because to the terminal
//!   they are nonexistent, not un-searchable.
//! - **Reflow / AltScreenSwitch / splice / renumber**: a width change, an
//!   active-screen swap, a protected-footer splice (`absolute_row_revision`)
//!   or a Kitty-unscroll history renumbering (`history_renumber_epoch`) moves
//!   keys wholesale — fall back to the FULL rebuild, which stays in place as
//!   both the fallback arm and the tests' differential oracle.
//!
//! The refresh is used only when it is PROVABLY byte-identical to that full
//! rebuild (all guards in `try_refresh_search_index`); rows below the previous
//! visible base are immutable while the guards hold, so per-miss work is
//! O(appended rows + visible rows) — the churn — instead of O(total retained).
//! Behavior identity is pinned by the in-file differential tests against
//! `legacy_results` (the from-scratch oracle).

use super::Terminal;
use crate::search::{DEFAULT_MAX_CACHED_LINES, TerminalSearch};

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
    /// Protected-footer splice revision at (re)build time. A splice renumbers
    /// absolute rows piecewise, so a refresh over the old keys would lie —
    /// mismatch forces the full rebuild.
    absolute_row_revision: u64,
    /// `Grid::history_renumber_epoch()` at (re)build time. Kitty CSI +T
    /// unscroll removes the NEWEST scrollback lines, shifting every older
    /// retained history row's absolute key while leaving `content_gen`
    /// arithmetic, `base_y()` and the splice revision unchanged — the one
    /// mutation the other stamps cannot see. Mismatch forces the full rebuild
    /// (and is part of the HIT key too: unscroll alone must not serve stale).
    history_renumber_epoch: u64,
    /// Grid width at (re)build time. A width change rewraps history wholesale
    /// (every retained row's text/key can change) — mismatch forces the full
    /// rebuild. Height-only changes reclassify rows between history and
    /// visible without moving absolute keys, so they stay refreshable.
    cols: u16,
    /// Absolute row of the OLDEST retained line at (re)build time. Retention
    /// only ever advances it (evicting the oldest rows, keys of survivors
    /// unchanged); a DECREASE means renumbering — full rebuild.
    hist_base: usize,
    /// Absolute row of the top VISIBLE row at (re)build time. Rows at/above
    /// this were fed in visible text form and may have been edited in place;
    /// rows below are immutable history in history text form. The refresh
    /// re-feeds from `min(previous, current)` so reclassified rows always
    /// carry the text form a from-scratch build would give them.
    visible_base: usize,
    /// Exclusive end of the indexed absolute-row range at (re)build time. A
    /// SHRINK cannot be expressed as a refresh (the index has no
    /// truncate-above) — full rebuild.
    indexed_end: usize,
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
        // The renumber epoch joins the HIT key: a Kitty unscroll shifts every
        // older history row's absolute key without necessarily bumping the
        // active grid's `content_seq` arithmetic this cache can observe, so an
        // epoch advance must invalidate even a seq-matching entry.
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
            // Current retained-window geometry, shared by the refresh guards,
            // the refresh itself, and the stamps cached with the result.
            // Mirrors `build_search_index`'s coordinate derivation exactly.
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
                // The legacy full build stays as the fallback arm AND the
                // differential oracle the refresh is tested against.
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

    /// Try to serve a cache miss by INCREMENTALLY refreshing the previously
    /// cached index instead of rebuilding it from scratch (the E2 terminal-side
    /// churn milestone — see the module docs). Returns `None` whenever the
    /// refresh is not PROVABLY byte-identical to `build_search_index` over the
    /// current retained window; the caller then pays the full rebuild.
    ///
    /// ## Why the refresh equals a from-scratch rebuild (given the guards)
    ///
    /// - Rows below the previous VISIBLE base are immutable history while the
    ///   guards hold: nothing edits a history row in place, splices
    ///   (`absolute_row_revision`), rewraps (`cols`) and renumberings
    ///   (`history_renumber_epoch`) are all fenced to the rebuild arm, and
    ///   retention only drops the oldest rows (fenced to only ADVANCE:
    ///   `hist_base` monotone). Their indexed text is already the history text
    ///   form a fresh build would produce, by induction: every refresh re-feeds
    ///   from `min(previous visible base, current visible base)`, so any row
    ///   that changed classification since the last (re)build is re-fed in its
    ///   NEW form (visible rows via `get_line_text`, history rows via
    ///   `line_text_bounded` — the exact sources `build_search_index` uses).
    /// - `drop_history_below` removes the evicted prefix with FRESH-BUILD
    ///   semantics (no sticky `incomplete`, zero watermark) — see its docs.
    /// - No internal cache-cap eviction can fire on either path: the guards
    ///   require the cached index to be eviction-free and the whole new window
    ///   to fit under `DEFAULT_MAX_CACHED_LINES` (the only cap this cache ever
    ///   builds with — `TerminalSearch::new`), so the fed key range
    ///   `[hist_base, indexed_end)` never exceeds the cap transiently either.
    ///   Beyond-cap sessions keep the legacy rebuild byte-for-byte.
    ///
    /// Cost: O(appended rows + visible rows) per content-changing miss — the
    /// churn — instead of O(total retained). The bloom filter keeps stale bits
    /// from replaced rows (negative filter: false-positive candidates only,
    /// results identical, saturation self-heals via `rebuild_bloom`).
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
                // Retention only advances; a retreat means renumbering
                // (defensive: reattach of an offloaded reflow can grow
                // scrollback back — its width change already fences, but the
                // guard must not depend on that coupling).
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

        // EvictBelow: rows the grid no longer retains disappear with
        // fresh-build (complete) semantics.
        index.drop_history_below(hist_base);

        // Replace + Append: re-feed everything from the OLDER of the two
        // visible bases (in-place-editable rows, reclassified rows, appended
        // rows). The unchanged-row skip in `SearchIndex::index_line` makes
        // re-fed identical rows cheap (no posting-list work).
        let refresh_start = cached.visible_base.min(visible_base).max(hist_base);
        use super::selection::{MAX_SCROLLBACK_LINE_SCAN_BYTES, line_text_bounded};
        if refresh_start < visible_base {
            let grid = &self.grid;
            let start_hist = refresh_start.saturating_sub(hist_base);
            let count = visible_base.saturating_sub(refresh_start);
            index.index_numbered_content_owned((0..count).map(|j| {
                let absolute_row = refresh_start.saturating_add(j);
                let text = grid
                    .get_history_line(start_hist.saturating_add(j))
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
        //
        // Fed STREAMING, not collected: the owned batch walks the iterator once
        // and moves each bounded `String` into the index, with no second copy.
        // Materializing a `Vec<String>` first bought nothing but a second
        // simultaneous residency of the WHOLE scrollback text (plus a 24-byte
        // spine per line) on top of the index being built.
        // Behavior is unchanged: the same lines are enumerated in the same order
        // and keyed at `base + offset`, so the line set, absolute-row keys and
        // eviction/INCOMPLETE semantics — and therefore every `SearchMatch`
        // coordinate — are identical. The only difference is that the
        // `get_history_line` reads now interleave with trigram insertion instead
        // of all preceding it, which nothing can observe: `get_history_line`
        // takes `&self`, so the grid cannot mutate (nor `content_gen` bump)
        // mid-build. The `legacy_results` oracle test below deliberately keeps
        // the collect form and asserts result equality — it is this change's
        // regression guard.
        use super::selection::{MAX_SCROLLBACK_LINE_SCAN_BYTES, line_text_bounded};
        let hist_base = usize::try_from(oldest).unwrap_or(usize::MAX);
        search.index_numbered_content_owned((0..scrollback).map(|i| {
            let absolute_row = hist_base.saturating_add(i);
            let text = grid
                .get_history_line(i)
                .map(|l| {
                    line_text_bounded(l.as_bytes(), MAX_SCROLLBACK_LINE_SCAN_BYTES).into_owned()
                })
                .unwrap_or_default();
            (absolute_row, text)
        }));

        // Visible rows 0..rows → absolute oldest + scrollback + r. Combining-aware
        // `get_line_text` so accents / ZWJ clusters survive (FIDELITY I-1).
        // `rows` is a u16, so `i32::from` is lossless (mirrors the legacy
        // `r as i32` where `r` was a u16-bounded usize). Streamed for the same
        // reason as the history above.
        let vis_base = hist_base.saturating_add(scrollback);
        search.index_numbered_content_owned((0..rows).map(|r| {
            let absolute_row = vis_base.saturating_add(usize::from(r));
            let text = self.get_line_text(i32::from(r), None).unwrap_or_default();
            (absolute_row, text)
        }));

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

    /// Number of INCREMENTAL refreshes (churn-bounded re-feeds) among those
    /// misses — `search_index_rebuilds() - search_index_refreshes()` is the
    /// count of FULL O(total-retained) rebuilds. Monotonic; introspection only
    /// (it is the observable that pins the churn path in tests and the churn
    /// bench).
    #[must_use]
    #[inline]
    pub fn search_index_refreshes(&self) -> u64 {
        self.search_index_refreshes
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

    /// `restore_checkpoint` REPLACES the grids, so every stamp the cached
    /// index was keyed against (`content_gen`, the absolute-row counter, the
    /// renumber epoch) restarts from a fresh grid's values — a coincidental
    /// collision would serve PRE-restore results. The release in
    /// `restore_checkpoint` is what makes that impossible; this pins it (both
    /// the results and the fact that the next search really rebuilt).
    #[test]
    fn restore_checkpoint_drops_the_cached_search_index() {
        let mut source = Terminal::new(6, 40);
        for i in 0..30 {
            source.process(format!("alpha {i} needle\r\n").as_bytes());
        }
        let saved = source.checkpoint();

        // A DIFFERENT terminal, with its own primed cache over other content.
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

    /// SCRIPTED DIFFERENTIAL — the SA-2 ship gate. The refresh arm is only
    /// legitimate if it is indistinguishable from a full rebuild under EVERY
    /// mutation class its guards reason about, not just the ones the targeted
    /// tests name. This drives one terminal through a long deterministic
    /// mixture of appends, in-place visible edits, erases, scrollback clears,
    /// height AND width resizes, alt-screen switches, retention shrinks and
    /// Kitty unscrolls, searching after EVERY step, and requires the cached
    /// path to equal the from-scratch `legacy_results` oracle every time.
    ///
    /// Two-sided by construction: the script must exercise BOTH arms (some
    /// searches served by the O(churn) refresh, some falling back to the full
    /// rebuild), or a script that never refreshed would "pass" while proving
    /// nothing.
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
                // Kitty CSI +T unscroll: history renumbered with no other
                // observable stamp (the epoch fence) — plus the occasional
                // retention shrink (front eviction).
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

    /// The E2 churn claim, pinned: after ordinary streaming output the miss is
    /// served by an INCREMENTAL refresh (evicted prefix dropped, previous
    /// visible rows + appended rows re-fed), not a full rebuild — and every
    /// refreshed result equals the from-scratch legacy oracle. The counter
    /// deltas are the two-sided reach guard: refreshes MUST advance (the churn
    /// path really engaged) and full rebuilds MUST NOT (no O(total) work hid
    /// inside the loop).
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

    /// Ring-cap eviction during streaming — the steady state of EVERY capped
    /// session (each appended line advances `oldest_absolute_row`): the
    /// refresh must drop the evicted prefix with FRESH-BUILD semantics — same
    /// matches AND same completeness as the legacy rebuild (which reports
    /// complete over the surviving rows).
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
        for batch in 0..4 {
            let oldest_before = t.grid().oldest_absolute_row();
            for i in 0..10 {
                t.process(format!("more {batch}-{i} needle\r\n").as_bytes());
            }
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

    /// Width change (history rewrapped wholesale) and Kitty CSI +T unscroll
    /// (history renumbered with NO other observable stamp) must both fall back
    /// to the FULL rebuild — a refresh over shifted keys would silently return
    /// matches at wrong rows. The epoch assertion is the wiring guard for
    /// `Grid::history_renumber_epoch`.
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
        // The WIRING guard, stated the same way the unscroll arm states it. The
        // refresh assertion below is also satisfied by the reuse guard's
        // `cached.cols == cols` fence, so on its own it holds with the width-reflow
        // epoch bump (`scrollback_reflow.rs`) deleted — and then a LATER width
        // change that happens to restore `cols` refreshes over renumbered keys.
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
