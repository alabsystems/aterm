// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Memo of MATERIALIZED history rows for a scrolled-back viewport (SCR-1).
//!
//! WHY. Every read of a scrolled-back row goes through
//! [`Grid::visible_row_view`](crate::Grid::visible_row_view), which for any row
//! past the live/history boundary rebuilds a whole
//! [`MaterializedRow`](super::MaterializedRow) from the 3-tier store — a fresh
//! `vec![Cell; cols]`, a fresh extras map and a per-cluster `Arc<str>` per call.
//! Nothing between that call and the frame remembered anything, and the frame
//! path ([`Terminal::cell_frame_into`]) walks `0..rows` with no damage gate. So
//! the unit of work was "whole viewport, whole materialization, every presented
//! frame", while the actual information change is 3 rows per wheel notch and
//! ZERO rows for the ~18 pill-fade frames, every cursor-blink toggle, every
//! effects frame and every mouse-move of a selection drag that follows it.
//!
//! WHAT THIS IS. A tiny direct-mapped memo keyed by the row's ABSOLUTE row
//! number, held by the `Grid` whose history it describes. The same shape
//! already ships twice in-tree: the lazy buffer's per-line `OnceCell`
//! materialization (`scroll_convert.rs`) and the web facade's 1-row
//! `display_row_cache` keyed by `(alt, content_gen, display_offset, row)`
//! (`aterm-gpu-web/src/lib.rs`). This is that second one widened to a viewport
//! and re-keyed on row IDENTITY instead of viewport POSITION, which is what
//! makes an overlapping scroll a hit rather than a miss.
//!
//! ## The key, and why it is exactly this
//!
//! Slots are keyed on the absolute row number, and the WHOLE cache is stamped
//! with a [`HistoryEpoch`] — the identity of the history the slots were filled
//! from. A mismatch drops every slot; there is no partial invalidation, because
//! every event that can rewrite retained history rewrites an unknown set of
//! them.
//!
//! * `content_gen` — THE authority. It is bumped exactly once per CONTENT
//!   mutation and, crucially, is maintained at the paths that assign
//!   `Damage::Full` DIRECTLY without going through a `mark_content_*` wrapper
//!   (width reflow does this: `reflow.rs` sets `damage = Damage::Full` and then
//!   bumps `content_gen` by hand, with a comment saying why). That is the same
//!   signal the cached search index trusts to decide that absolute-row-keyed
//!   state over history must be rebuilt. Deliberately NOT invented here as a
//!   second, narrower counter: a new "history rewrite" epoch would have to be
//!   threaded through every one of those direct-damage sites, and the first one
//!   missed is a silently wrong glyph on screen. One signal, one place to fix.
//!   It does NOT move on a pure VIEWPORT change (`scroll_display`), which is
//!   precisely why a wheel scrub keeps its hits.
//! * `history_renumber_epoch` — belt and braces for the one retained-window
//!   mutation that is invisible to `content_gen`'s consumers by design: Kitty
//!   CSI +T unscroll removes the NEWEST scrollback lines, so every older row
//!   keeps its content but its absolute key shifts wholesale (see
//!   `GridStorage::history_renumber_epoch`).
//! * `cols` / `visible_rows` — the materialization is width-dependent
//!   (`materialize_from_line(.., cols)`) and the slot count is sized from the
//!   viewport. Both already imply a `content_gen` bump via resize; they are in
//!   the key so the cache cannot be wrong even if some future resize path
//!   forgets to.
//!
//! Retention EVICTION needs no key of its own: it drops the OLDEST lines and
//! preserves every survivor's absolute number, so an evicted row's slot can
//! never be reached again (no live `rev_idx` maps to it) — it is dead weight
//! until the modulo reuses it, never a wrong answer.
//!
//! ## The debug net
//!
//! In debug builds every HIT is re-materialized and compared against the memo
//! (see `Grid::materialized_history_row`). A stale hit is a wrong glyph or
//! colour on screen — the failure mode with the worst signal-to-noise — so it
//! is made loud where it is cheap. It also has a quiet second benefit: with the
//! re-materialize in place, `cfg(test)` builds perform exactly the same number
//! of `row_to_line` / materialize operations as before this cache existed, so
//! the crate's op-count tests keep measuring what they always measured.

use std::cell::RefCell;
use std::sync::Arc;

use super::MaterializedRow;

/// Fewest slots the memo ever holds. Keeps a tiny grid (a 1-row test harness,
/// a strip) from thrashing a 2-slot cache.
const MIN_SLOTS: usize = 8;

/// Most slots the memo ever holds — the memory bound. A slot is one
/// materialized row (`cols` cells plus its sparse extras), so this caps the
/// memo at ~512 rows of cells: at the shipped 24-50 row window the clamp never
/// binds (2x the viewport is 48-100 slots), and a pathologically tall grid
/// trades hit rate for a bounded footprint rather than growing without limit.
const MAX_SLOTS: usize = 512;

/// The identity of the history a set of memo slots was filled from. Any change
/// drops every slot — see the module docs for why each term is here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::grid) struct HistoryEpoch {
    /// `GridStorage::content_gen` — the authority on "content changed".
    pub(in crate::grid) content_gen: u64,
    /// `GridStorage::history_renumber_epoch` — absolute keys shifted wholesale.
    pub(in crate::grid) renumber: u64,
    /// Width the rows were materialized at.
    pub(in crate::grid) cols: u16,
    /// Viewport height, which sizes the slot table.
    pub(in crate::grid) visible_rows: u16,
}

/// One filled slot: the absolute row it holds, and the shared row itself.
///
/// `Arc`, not a borrow: `visible_row_view` takes `&self`, so handing out a
/// `Ref` guard would make two simultaneously-live row views a PANIC instead of
/// a slow path. A refcount bump is the price of never turning a read pattern
/// into a crash.
#[derive(Debug)]
struct Slot {
    abs_row: u64,
    row: Arc<MaterializedRow>,
}

#[derive(Debug, Default)]
struct Inner {
    /// `None` until the first fill; `Some` stamps what the slots describe.
    epoch: Option<HistoryEpoch>,
    /// Direct-mapped table indexed by `abs_row % slots.len()`.
    slots: Vec<Option<Slot>>,
}

impl Inner {
    /// Bring the table in line with `epoch`, dropping every slot if the history
    /// it describes is not the history being read.
    fn sync(&mut self, epoch: HistoryEpoch) {
        if self.epoch == Some(epoch) {
            return;
        }
        self.epoch = Some(epoch);
        let capacity = capacity_for(epoch.visible_rows);
        // `clear` + `resize_with` keeps the Vec's allocation across an
        // invalidation (the common case is a re-fill at the same size), while
        // dropping every stale `Arc`.
        self.slots.clear();
        self.slots.resize_with(capacity, || None);
    }

    /// The slot an absolute row maps to, or `None` for an empty table.
    fn index_of(&self, abs_row: u64) -> Option<usize> {
        let len = u64::try_from(self.slots.len()).ok()?;
        if len == 0 {
            return None;
        }
        usize::try_from(abs_row % len).ok()
    }
}

/// Slots for a viewport: two screens' worth, so a wheel scrub still hits the
/// rows it just scrolled past when the user reverses direction, and so no two
/// rows of ONE viewport can ever collide (consecutive keys are distinct modulo
/// any table at least as large as the viewport).
fn capacity_for(visible_rows: u16) -> usize {
    usize::from(visible_rows)
        .saturating_mul(2)
        .clamp(MIN_SLOTS, MAX_SLOTS)
}

/// The per-`Grid` memo. Interior-mutable because the read it serves
/// (`Grid::visible_row_view`) takes `&self`; `Grid` is already `!Sync` (its
/// `StyleTable` is), so this adds no auto-trait change.
#[derive(Debug, Default)]
pub(in crate::grid) struct ViewportRowCache {
    inner: RefCell<Inner>,
}

impl ViewportRowCache {
    /// The memoized row for `abs_row` under `epoch`, or `None` on a miss.
    /// Syncing to `epoch` is part of the lookup, so a caller cannot forget it.
    pub(in crate::grid) fn lookup(
        &self,
        epoch: HistoryEpoch,
        abs_row: u64,
    ) -> Option<Arc<MaterializedRow>> {
        let mut inner = self.inner.borrow_mut();
        inner.sync(epoch);
        let idx = inner.index_of(abs_row)?;
        match &inner.slots[idx] {
            Some(slot) if slot.abs_row == abs_row => Some(Arc::clone(&slot.row)),
            _ => None,
        }
    }

    /// Memoize `row` for `abs_row` under `epoch`, replacing whatever shared the
    /// slot. The borrow is taken and released here and NEVER held across the
    /// materialize that produced `row` — the one discipline that keeps this
    /// `RefCell` panic-free by construction.
    pub(in crate::grid) fn store(
        &self,
        epoch: HistoryEpoch,
        abs_row: u64,
        row: &Arc<MaterializedRow>,
    ) {
        let mut inner = self.inner.borrow_mut();
        inner.sync(epoch);
        let Some(idx) = inner.index_of(abs_row) else {
            return;
        };
        inner.slots[idx] = Some(Slot {
            abs_row,
            row: Arc::clone(row),
        });
    }
}
