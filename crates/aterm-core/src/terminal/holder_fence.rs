// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The holder fence: which main-screen rows the foreground drawer wrote
//! (docs/DESIGN-card-engine-2026-10-19.md §3.7).
//!
//! A card is decorated only on rows its drawer image wrote while it held the
//! terminal. No byte says who wrote a row, so the host reports each change
//! of foreground holder ([`HolderCut`]) and the fence keeps, per drawer, the
//! row spans laid down while it was current. Shells and other commands never
//! hold a mark: a card they print is never decorated.
//!
//! Span boundaries are grid anchors ([`aterm_grid::AnchorSet`]), moved by the
//! grid's own row-motion choke points; one the grid cannot carry marks the set
//! lost, which taints every mark (nothing admitted) until a full clear
//! re-anchors the current one. A suspended drawer's rows are sealed with
//! fingerprints, so a reselect (`fg`) keeps only what nobody rewrote.
//!
//! Host-only: no byte reaches the fence. Zero cost until a drawer runs: the
//! anchor set stays empty and every grid hook is one branch.

use aterm_grid::{AnchorId, AnchorRole, FenceKnob, Grid, knob};

#[cfg(test)]
#[path = "holder_fence_tests.rs"]
mod tests;

/// Marks the fence holds at most: one current drawer and seven stopped ones.
pub const MAX_HOLDER_MARKS: usize = 8;

/// Spans a mark holds at most; overflow keeps the launch span and the newest.
pub const MAX_HOLDER_SPANS: usize = 4;

/// How the foreground holder changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CutKind {
    /// The foreground process group changed (a launch, a stop, an exit, `fg`).
    Group,
    /// The foreground image exec'd into another, even if its sampled drawer
    /// token is unchanged. The outgoing mark belongs to the old image.
    Exec,
}

/// One change of foreground holder, reported by the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HolderCut {
    /// The incoming holder's drawer token, or `None` for anything that is not
    /// a drawer (a shell, another command).
    pub to: Option<u64>,
    /// How the holder changed.
    pub kind: CutKind,
    /// The outgoing holder is still alive (stopped or backgrounded), so its
    /// mark is sealed and kept; gone, it is dropped.
    pub from_alive: bool,
}

/// What [`super::Terminal::adopt_holder_after_restore`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HolderAdoption {
    /// The carried fence's current mark is the holder's: it continues.
    Continued,
    /// The fence was seeded from the running command's signed output start.
    Seeded,
    /// Nothing is admitted until the drawer redraws.
    Bare,
}

/// The current holder's admitted rows, resolved for one frame: the render
/// input's copy of the fence (`RenderInput::holder`). `token == 0` holds
/// nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct HolderView {
    /// The current drawer's token, or `0` when no drawer is current.
    pub token: u64,
    /// How many of `spans` are filled.
    pub n: u8,
    /// Absolute row spans `[start, end)`, oldest first; an open span ends at
    /// `u64::MAX`.
    pub spans: [(u64, u64); MAX_HOLDER_SPANS],
    /// Nothing on the main screen is admitted (the lay found ink, or an
    /// anchor was lost).
    pub tainted: bool,
    /// The alternate screen was cleared while this drawer was current, so
    /// everything on it is the drawer's.
    pub alt_fresh: bool,
}

impl HolderView {
    /// No holder: admits nothing.
    pub const NONE: Self = Self {
        token: 0,
        n: 0,
        spans: [(0, 0); MAX_HOLDER_SPANS],
        tainted: false,
        alt_fresh: false,
    };

    /// The filled spans.
    #[must_use]
    pub fn spans(&self) -> &[(u64, u64)] {
        &self.spans[..usize::from(self.n).min(MAX_HOLDER_SPANS)]
    }

    /// Whether main-screen rows `[lo, hi)` were all written by the current
    /// drawer: one span holds them all, and nothing taints the mark.
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "Admit",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    pub fn admits_rows(&self, lo: u64, hi: u64) -> bool {
        self.token != 0
            && !self.tainted
            && lo < hi
            && self
                .spans()
                .iter()
                .any(|&(start, end)| start <= lo && hi <= end)
    }

    /// Whether the alternate screen holds only the current drawer's writes.
    #[must_use]
    pub fn admits_alt(&self) -> bool {
        self.token != 0 && self.alt_fresh
    }
}

/// A row span `[start, end)` of anchors in the main grid's set; an open span
/// (the current drawer's newest) has no end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Span {
    pub(crate) start: AnchorId,
    pub(crate) end: Option<AnchorId>,
}

/// A suspended mark's visible rows as they stood when it stopped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Seal {
    /// `(absolute row, fingerprint)` for each visible row in the mark's
    /// spans, ascending.
    pub(crate) rows: Vec<(u64, u64)>,
    /// The screen's absolute top at the seal: rows above it were history.
    pub(crate) base: u64,
    /// The anchor set's seal epoch at the seal: history rows are trusted only
    /// while nothing rekeyed or renumbered them.
    pub(crate) epoch: u64,
}

/// One drawer's admitted spans.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Mark {
    pub(crate) token: u64,
    pub(crate) spans: Vec<Span>,
    pub(crate) tainted: bool,
    pub(crate) alt_fresh: bool,
    pub(crate) seal: Option<Seal>,
    /// The fence clock when it was suspended (eviction picks the oldest).
    pub(crate) suspended_at: u64,
}

/// The fence: every drawer mark, and which is current.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HolderFence {
    pub(crate) current: Option<u8>,
    pub(crate) marks: Vec<Mark>,
    pub(crate) clock: u64,
    /// A restored stream had output without holder observations. Its carried
    /// command start cannot seed a writer range, even after another checkpoint.
    pub(crate) seed_untrusted: bool,
}

/// Where the main cursor stands, read once per fence operation.
#[derive(Clone, Copy)]
struct At {
    base: u64,
    rows: u16,
    row: u16,
    col: u16,
}

impl At {
    fn of(grid: &Grid) -> Self {
        let cursor = grid.cursor();
        let rows = grid.rows();
        Self {
            base: grid.visible_to_absolute(0),
            rows,
            row: cursor.row.min(rows.saturating_sub(1)),
            col: cursor.col,
        }
    }

    fn cursor_row(self) -> u64 {
        self.base + u64::from(self.row)
    }

    /// The row a new span starts on: the cursor's, or the next one when the
    /// cursor stands inside a row someone else began.
    fn lay_row(self) -> u64 {
        self.cursor_row() + u64::from(self.col != 0)
    }

    fn visible(self, row: u64) -> Option<u16> {
        row.checked_sub(self.base)
            .and_then(|r| u16::try_from(r).ok())
            .filter(|&r| r < self.rows)
    }
}

impl HolderFence {
    /// Whether any mark exists (the fence is free while none does).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }

    /// The current drawer's token.
    #[must_use]
    pub fn current_token(&self) -> Option<u64> {
        self.current_mark().map(|m| m.token)
    }

    /// Every mark's token, current or suspended.
    pub fn tokens(&self) -> impl Iterator<Item = u64> + '_ {
        self.marks.iter().map(|m| m.token)
    }

    fn current_mark(&self) -> Option<&Mark> {
        self.current.and_then(|i| self.marks.get(usize::from(i)))
    }

    /// The current holder's view over `grid` (the main grid).
    #[must_use]
    pub fn view(&self, grid: &Grid) -> HolderView {
        let Some(mark) = self.current_mark() else {
            return HolderView::NONE;
        };
        let anchors = grid.anchors();
        let mut view = HolderView {
            token: mark.token,
            tainted: mark.tainted || anchors.lost(),
            alt_fresh: mark.alt_fresh,
            ..HolderView::NONE
        };
        for span in &mark.spans {
            let Some(start) = anchors.row(span.start) else {
                continue;
            };
            let end = span.end.map_or(Some(u64::MAX), |e| anchors.row(e));
            let Some(end) = end else { continue };
            if start < end && usize::from(view.n) < MAX_HOLDER_SPANS {
                view.spans[usize::from(view.n)] = (start, end);
                view.n += 1;
            }
        }
        view
    }

    /// A lost anchor taints every mark holding a span; the set's flag then
    /// clears, so anchors placed from here on start clean.
    fn absorb_lost(&mut self, grid: &mut Grid) {
        if !grid.anchors().lost() {
            return;
        }
        for mark in &mut self.marks {
            if !mark.spans.is_empty() {
                mark.tainted = true;
            }
        }
        grid.anchors_mut().clear_lost();
    }

    fn free_span(grid: &mut Grid, span: Span) {
        grid.anchors_mut().free(span.start);
        if let Some(end) = span.end {
            grid.anchors_mut().free(end);
        }
    }

    fn drop_mark(&mut self, index: usize, grid: &mut Grid) {
        let mark = self.marks.remove(index);
        for span in mark.spans {
            Self::free_span(grid, span);
        }
        self.current = match self.current.map(usize::from) {
            Some(c) if c == index => None,
            Some(c) if c > index => u8::try_from(c - 1).ok(),
            other => other.and_then(|c| u8::try_from(c).ok()),
        };
        // The clock orders marks only: with none left the fence is the
        // empty one, as a session no drawer ran in holds.
        if self.marks.is_empty() {
            self.clock = 0;
        }
    }

    /// Drop a mark's spans that hold no row (`end <= start`, or an anchor
    /// gone).
    fn prune(mark: &mut Mark, grid: &mut Grid) {
        let anchors = grid.anchors();
        let (keep, gone): (Vec<Span>, Vec<Span>) = mark.spans.iter().partition(|span| {
            let start = anchors.row(span.start);
            let end = span.end.map_or(Some(u64::MAX), |e| anchors.row(e));
            matches!((start, end), (Some(s), Some(e)) if s < e)
        });
        mark.spans = keep;
        for span in gone {
            Self::free_span(grid, span);
        }
    }

    /// Open a span at `row` on `mark`: past [`MAX_HOLDER_SPANS`] the
    /// second-oldest goes, keeping the launch span. No anchor left: no span.
    fn open_span(mark: &mut Mark, row: u64, grid: &mut Grid) {
        if mark.spans.len() >= MAX_HOLDER_SPANS {
            let dropped = mark.spans.remove(1);
            Self::free_span(grid, dropped);
        }
        if let Some(start) = grid.anchors_mut().place(row, AnchorRole::Start) {
            mark.spans.push(Span { start, end: None });
        }
    }

    /// Close `mark`'s open span at `row` (exclusive).
    fn close_open_span(mark: &mut Mark, row: u64, grid: &mut Grid) {
        let Some(open) = mark.spans.iter_mut().find(|s| s.end.is_none()) else {
            return;
        };
        match grid.anchors_mut().place(row, AnchorRole::End) {
            Some(end) => open.end = Some(end),
            // No anchor for its end: the span cannot be bounded, so it goes.
            None => {
                let start = open.start;
                mark.spans.retain(|s| s.start != start);
                grid.anchors_mut().free(start);
            }
        }
        Self::prune(mark, grid);
    }

    /// Fingerprint each visible row of `mark`'s spans.
    fn seal(mark: &mut Mark, grid: &Grid, at: At) {
        let anchors = grid.anchors();
        let mut rows = Vec::new();
        for span in &mark.spans {
            let (Some(start), Some(end)) = (
                anchors.row(span.start),
                span.end.and_then(|e| anchors.row(e)),
            ) else {
                continue;
            };
            let lo = start.max(at.base);
            let hi = end.min(at.base + u64::from(at.rows));
            for row in lo..hi {
                if let Some(fp) = at.visible(row).and_then(|r| grid.live_row_fingerprint(r)) {
                    rows.push((row, fp));
                }
            }
        }
        rows.sort_unstable();
        rows.dedup();
        mark.seal = Some(Seal {
            rows,
            base: at.base,
            epoch: anchors.seal_epoch(),
        });
    }

    /// Cut a reselected mark's spans at the first row its seal cannot vouch
    /// for: a history row once anything rekeyed history, a sealed row changed
    /// or no longer held (past the ring), a row never sealed.
    fn check_seal(mark: &mut Mark, seal: &Seal, grid: &mut Grid) {
        let epoch_held = grid.anchors().seal_epoch() == seal.epoch;
        for span in &mark.spans {
            let anchors = grid.anchors();
            let (Some(start), Some(end_id)) = (anchors.row(span.start), span.end) else {
                continue;
            };
            let Some(end) = anchors.row(end_id) else {
                continue;
            };
            // History at the seal, untouched since: nobody can have written it.
            let trusted = if epoch_held {
                end.min(seal.base).max(start)
            } else {
                start
            };
            let bad = (trusted..end).find(|&row| {
                let sealed = seal
                    .rows
                    .binary_search_by_key(&row, |&(r, _)| r)
                    .ok()
                    .map(|i| seal.rows[i].1);
                // A row that scrolled into the ring since keeps its value.
                let now = grid.row_fingerprint_at_absolute(row);
                sealed.is_none() || now.is_none() || sealed != now
            });
            if let Some(row) = bad {
                grid.anchors_mut().set_row(end_id, row);
            }
        }
        Self::prune(mark, grid);
    }

    /// Lay a span for `mark` at the cursor, tainting the mark if anything is
    /// drawn from the cursor on.
    fn lay(mark: &mut Mark, grid: &mut Grid, at: At) {
        if !knob(FenceKnob::NoLayTaint) && grid.has_ink_from(at.row, at.col) {
            mark.tainted = true;
        }
        Self::open_span(mark, at.lay_row(), grid);
    }

    /// The marks a new one may join: [`MAX_HOLDER_MARKS`], or one under the
    /// count-eviction knob.
    fn capacity() -> usize {
        if knob(FenceKnob::CountEviction) {
            1
        } else {
            MAX_HOLDER_MARKS
        }
    }

    /// The foreground holder changed: §3.7's steps 1-4 (see [`HolderCut`]).
    /// `grid` is the main grid (the inactive one while the alternate screen
    /// is up), whose cursor is the main cursor.
    pub fn note_cut(&mut self, cut: HolderCut, grid: &mut Grid) {
        if self.marks.is_empty() && cut.to.is_none() {
            return;
        }
        self.absorb_lost(grid);
        let at = At::of(grid);
        if cut.kind == CutKind::Group && cut.to.is_some() && cut.to == self.current_token() {
            return;
        }
        self.clock = self.clock.wrapping_add(1);
        // 1-2. The outgoing span closes at the main cursor; the outgoing mark
        // is dropped (exec, or gone) or sealed and kept (alive).
        if let Some(index) = self.current.take().map(usize::from) {
            if cut.kind == CutKind::Exec
                && knob(FenceKnob::ExecInherits)
                && let Some(to) = cut.to
            {
                self.marks[index].token = to;
                self.current = u8::try_from(index).ok();
                return;
            }
            let mark = &mut self.marks[index];
            if !knob(FenceKnob::StopKeepsAltFresh) {
                mark.alt_fresh = false;
            }
            Self::close_open_span(mark, at.cursor_row(), grid);
            match (cut.kind, cut.from_alive) {
                (CutKind::Group, true) => {
                    Self::seal(mark, grid, at);
                    mark.suspended_at = self.clock;
                }
                _ => self.drop_mark(index, grid),
            }
        }
        // 3. The incoming holder.
        let Some(token) = cut.to else {
            return;
        };
        if let Some(index) = self.marks.iter().position(|m| m.token == token) {
            // Reselect: keep what the seal vouches for, then a new span.
            let mark = &mut self.marks[index];
            if let Some(seal) = mark.seal.take()
                && !knob(FenceKnob::NoSealCheck)
            {
                Self::check_seal(mark, &seal, grid);
            }
            Self::lay(mark, grid, at);
            self.current = u8::try_from(index).ok();
            return;
        }
        // A new mark: at capacity the oldest suspended one goes first, so
        // its anchors are free for the new span.
        self.make_room(grid);
        let mut mark = Mark {
            token,
            spans: Vec::new(),
            tainted: false,
            alt_fresh: false,
            seal: None,
            suspended_at: 0,
        };
        Self::lay(&mut mark, grid, at);
        self.push_current(mark);
    }

    /// A suspended drawer is gone (the host saw it die): its mark goes. The
    /// current one leaves by a [`HolderCut`] instead.
    pub fn retire(&mut self, token: u64, grid: &mut Grid) {
        if let Some(index) = self.marks.iter().position(|m| m.token == token)
            && self.current.map(usize::from) != Some(index)
        {
            self.drop_mark(index, grid);
        }
    }

    /// A full clear of the main screen from absolute row `top` (ED 2, ED 0
    /// from home) while a drawer is current: every row from `top` is blank
    /// and the current writer's from now on. Its open span starts no later
    /// than `top`, its closed spans end there, and a tainted mark or a lost
    /// anchor starts afresh with one span at `top`.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "FullClear",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    pub fn note_full_clear(&mut self, top: u64, grid: &mut Grid) {
        let Some(index) = self.current.map(usize::from) else {
            return;
        };
        self.absorb_lost(grid);
        let mark = &mut self.marks[index];
        if mark.tainted || !mark.spans.iter().any(|s| s.end.is_none()) {
            for span in std::mem::take(&mut mark.spans) {
                Self::free_span(grid, span);
            }
            mark.tainted = false;
            Self::open_span(mark, top, grid);
            return;
        }
        for span in &mark.spans {
            let anchors = grid.anchors_mut();
            match span.end {
                None => {
                    if anchors.row(span.start).is_some_and(|s| s > top) {
                        anchors.set_row(span.start, top);
                    }
                }
                Some(end) => {
                    if anchors.row(end).is_some_and(|e| e > top) {
                        anchors.set_row(end, top);
                    }
                }
            }
        }
        Self::prune(mark, grid);
    }

    /// The alternate screen was cleared (a `1049h`, repeated or not; a full
    /// clear on it): while a drawer is current, everything on it is its.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "Alt1049",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "AltClear",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    pub fn note_alt_cleared(&mut self) {
        if let Some(mark) = self
            .current
            .and_then(|i| self.marks.get_mut(usize::from(i)))
        {
            mark.alt_fresh = true;
        }
    }

    /// The alternate screen went away.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "AltExit",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    pub fn note_alt_exit(&mut self) {
        for mark in &mut self.marks {
            mark.alt_fresh = false;
        }
    }

    /// Evict the oldest suspended marks until a new one fits the capacity,
    /// before it lays its span: eight marks of four closed spans hold every
    /// anchor the grid has. No mark may be current.
    fn make_room(&mut self, grid: &mut Grid) {
        debug_assert!(self.current.is_none(), "a new mark replaces no current one");
        while self.marks.len() >= Self::capacity() {
            let Some(oldest) = self
                .marks
                .iter()
                .enumerate()
                .min_by_key(|(_, m)| m.suspended_at)
                .map(|(i, _)| i)
            else {
                break;
            };
            self.drop_mark(oldest, grid);
        }
    }

    /// Make `mark` current ([`Self::make_room`] made room for it).
    fn push_current(&mut self, mark: Mark) {
        debug_assert!(self.marks.len() < Self::capacity(), "room was made");
        self.marks.push(mark);
        self.current = u8::try_from(self.marks.len() - 1).ok();
    }

    /// After a restore, take `token` (a drawer when `drawer`) as the holder:
    /// a carried current mark of an Exact drawer's token continues when the
    /// carry is `untouched` (no byte came after it); otherwise the carried
    /// current mark and any of that token go, and a drawer gets one mark,
    /// its open span from `seed`'s row (`alt_fresh` its flag) or, without
    /// one, tainted until it clears the screen itself.
    pub fn adopt(
        &mut self,
        token: u64,
        drawer: bool,
        untouched: bool,
        seed: Option<(u64, bool)>,
        grid: &mut Grid,
    ) -> HolderAdoption {
        if drawer
            && untouched
            && token != 0
            && self.current_token() == Some(token)
            && (!self.seed_untrusted || self.current_mark().is_some_and(|m| !m.tainted))
        {
            return HolderAdoption::Continued;
        }
        let seed = if self.seed_untrusted { None } else { seed };
        self.absorb_lost(grid);
        if let Some(index) = self.current.map(usize::from) {
            self.drop_mark(index, grid);
        }
        if let Some(index) = self.marks.iter().position(|m| m.token == token) {
            self.drop_mark(index, grid);
        }
        if token == 0 || !drawer {
            return HolderAdoption::Bare;
        }
        self.make_room(grid);
        let mut mark = Mark {
            token,
            spans: Vec::new(),
            tainted: seed.is_none(),
            alt_fresh: seed.is_some_and(|(_, alt)| alt),
            seal: None,
            suspended_at: 0,
        };
        let at = At::of(grid);
        Self::open_span(&mut mark, seed.map_or(at.lay_row(), |(row, _)| row), grid);
        self.push_current(mark);
        if seed.is_some() {
            HolderAdoption::Seeded
        } else {
            HolderAdoption::Bare
        }
    }
}

/// `holder_fence` environment actions: the writers' own bytes. The fence
/// reads no byte (zero cost in the parser), so no fence code performs them;
/// the Tier-1 walk (`holder_fence_conformance`) writes each on the real
/// terminal and projects what it left.
#[cfg(any(test, debug_assertions))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "holder_fence",
    action = "Draw",
    reason = "The current drawer's own line: the fence reads no byte, its open span already holds the row."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "holder_fence",
    action = "Rewind",
    reason = "The shell's line left under the cursor: what the next lay's taint scan reads."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "holder_fence",
    action = "Climb",
    reason = "The shell's line below a blank cursor row: what the next lay's taint scan reads."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "holder_fence",
    action = "Settle",
    reason = "The shell's newline past its own lines: the cursor back at the frontier."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "holder_fence",
    action = "Scribble",
    reason = "The shell rewriting a stopped drawer's row: the seal judges it at the reselect."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "holder_fence",
    action = "AltDraw",
    reason = "Another writer's ink on the alternate screen, which only a clear makes a drawer's."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn holder_fence_environment_waivers() {}

impl super::Terminal {
    /// The fence and the main grid its anchors live in (the inactive one
    /// while the alternate screen is up), borrowed apart. The one place that
    /// names the main grid mutably: the cuts, the checkpoint's install and
    /// the width lend all reach it here, so the anchors never land in the
    /// other grid.
    pub(super) fn fence_and_main_grid(&mut self) -> (&mut HolderFence, &mut Grid) {
        let grid = if self.modes.alternate_screen {
            self.alt_grid.as_mut().unwrap_or(&mut self.grid)
        } else {
            &mut self.grid
        };
        (&mut self.holder_fence, grid)
    }

    /// The primary-content grid, mutably: the inactive one while the
    /// alternate screen is up.
    pub(super) fn main_grid_mut(&mut self) -> &mut Grid {
        self.fence_and_main_grid().1
    }

    /// The foreground holder changed (see [`HolderCut`]). The host calls this
    /// under the terminal lock, after the bytes the outgoing holder wrote and
    /// before any the incoming one writes.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "LaunchA",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "LaunchB",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "StopA",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "StopB",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "FgA",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "FgB",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "ExitA",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "ExitB",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "ExecBA",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "ExecAB",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    pub fn note_holder_cut(&mut self, cut: HolderCut) {
        let (fence, grid) = self.fence_and_main_grid();
        fence.note_cut(cut, grid);
    }

    /// A stopped drawer died: its mark goes.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "KillA",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    pub fn retire_holder_mark(&mut self, token: u64) {
        let (fence, grid) = self.fence_and_main_grid();
        fence.retire(token, grid);
    }

    /// The current holder's admitted rows (the render input's
    /// `RenderInput::holder`).
    #[must_use]
    pub fn holder_view(&self) -> HolderView {
        self.holder_fence.view(self.main_grid())
    }

    /// The holder fence.
    #[must_use]
    pub fn holder_fence(&self) -> &HolderFence {
        &self.holder_fence
    }

    /// After a restore (a seamless rung of any kind, a crash restore, a
    /// predecessor with no fence), take `token` as the foreground holder,
    /// `drawer_exact` when it is a drawer whose build passes Exact. Call it
    /// once, right after the restore and before any output is fed.
    ///
    /// A carried fence whose current mark is `token`'s continues only for an
    /// Exact drawer, and only when no byte was parsed between the carry's
    /// install and this call: a crash journal's tail replay feeds bytes whose
    /// cuts were never journaled (a Ctrl-Z, `clear; cat card`, `fg` inside
    /// the tail), so after one the carried current mark is not trusted (the
    /// tail fails closed). That uncertainty also survives an intermediate
    /// checkpoint and forbids a seed from the old running block: replay may
    /// have ignored its old-nonce D/C marks. It is retained for this lineage;
    /// a later owned full clear can still produce a current mark that an
    /// untouched restore continues. Otherwise a drawer is seeded from the running
    /// command's output start (OSC 133 C), only when shell-integration marks
    /// are nonce-signed, so no program can have forged it; `alt_fresh` when
    /// the session is on the alternate screen with that command still
    /// running. Without a signed running block it stays bare until it clears
    /// the screen itself.
    pub fn adopt_holder_after_restore(&mut self, token: u64, drawer_exact: bool) -> HolderAdoption {
        // One adoption consumes the install. A repeated/out-of-sequence
        // call must not turn the carried C into fresh writer authority.
        let untouched = self.holder_carried_at.take() == Some(self.transient.screen_mutations);
        self.holder_fence.seed_untrusted |= !untouched;
        let seed = self
            .shell
            .current_block
            .as_ref()
            .filter(|_| self.modes.require_shell_integration_nonce)
            .filter(|block| block.state == super::BlockState::Executing)
            .and_then(|block| block.output_start_row)
            .map(|row| (row, self.modes.alternate_screen));
        let (fence, grid) = self.fence_and_main_grid();
        fence.adopt(token, drawer_exact, untouched, seed, grid)
    }
}
