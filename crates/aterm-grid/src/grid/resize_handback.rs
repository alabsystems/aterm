// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::borrow::Cow;

use aterm_scrollback::Line;

use super::Grid;
use super::scroll_convert::ScrolledRowExtras;
use super::scrollback_reflow::rewrap_cell_row;
use crate::row_u16;

/// The resize law a grid lays its rows out under: the current one, or an older one that `cast drift` replays a recording made under it with, named by the `aterm_resize_undo` value the recording's header carries ([`ResizeLaw::header_value`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ResizeLaw {
    /// `aterm_resize_undo: 1` (from 2026-09-29): no rows hand-back.
    Undo,
    /// `aterm_resize_undo: 2` as written until 2026-10-17: the rows hand-back.
    HandBack,
    /// `aterm_resize_undo: 2` as written from 2026-10-17: also the pushed rows owed back, and a grow that narrows split where its rewrap moves no row.
    PushedRows,
    /// `aterm_resize_undo: 3` (from 2026-10-19): a width change lays the rows out where the hand-back puts them, its rewrap's row moves recorded, in one pass at any height (`Grid::prepare_rewrap`), pushed rows owed included, from the head of the line the screen's top row continues (within four screens) and from the cell the window's top starts on inside its row; a line scrolled into history also counts as a row demoted; a shrink under owed pushed rows pushes the blank rows a push left, never trims them, and trims the program's past those; a push cuts the wraps at its seams and the grow back mends them; an erase below the cursor forgets the pushed rows.
    #[default]
    Rewrap,
}

impl ResizeLaw {
    /// The `aterm_resize_undo` value a recording made under this law carries.
    #[must_use]
    pub const fn header_value(self) -> u64 {
        match self {
            Self::Undo => 1,
            Self::HandBack | Self::PushedRows => 2,
            Self::Rewrap => 3,
        }
    }

    /// The laws a recording whose header carries `aterm_resize_undo: value` was made under, the newest first: 2 was written under two.
    #[must_use]
    pub const fn from_header(value: u64) -> &'static [Self] {
        match value {
            1 => &[Self::Undo],
            2 => &[Self::PushedRows, Self::HandBack],
            3 => &[Self::Rewrap],
            _ => &[],
        }
    }
}

/// The edge moves a resize the other way undoes first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Edge {
    /// Nothing owed.
    #[default]
    None,
    /// `rows` blank rows the trims took off the bottom, then `above` top rows demoted since, then `pushed` bottom rows pushed into history since (the corner of a cursor too near the top to demote): a grow puts `pushed` back at the bottom first, then reveals `above`, then appends `rows`.
    Trimmed {
        /// The trimmed blank rows.
        rows: u16,
        /// The rows demoted after them.
        above: u16,
        /// The bottom rows pushed after those: the newest `pushed` history lines, in the order they stood on the screen, the topmost first.
        pushed: u16,
        /// Where the pushes cut the lines they left (current law only).
        seams: Seams,
    },
    /// `rows` history lines the grows revealed at the top, then `above` blank rows appended since: a shrink takes `above` first, then demotes `rows`.
    Revealed {
        /// The revealed history lines.
        rows: u16,
        /// The blank rows appended after them.
        above: u16,
    },
}

/// What a bottom push left at its seams under the current law ([`ResizeLaw::Rewrap`]), owed back
/// with the pushed rows: how many blank rows it left under the cursor (`gap`), and whether it cut
/// the wrap of the first pushed row (`bottom`) and of the screen's new top row (`top`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Seams {
    pub(super) gap: u16,
    pub(super) top: bool,
    pub(super) bottom: bool,
}

/// How a rows-only shrink begins ([`Edge::plan_shrink`]).
#[derive(Clone, Copy, Debug)]
pub(super) struct ShrinkPlan {
    pub(super) natural: usize,
    pub(super) unreveal: usize,
    pub(super) forgot: bool,
}

/// What a rows-only shrink did, for [`Edge::after_shrink`].
pub(super) struct Shrunk {
    pub(super) rows: usize,
    pub(super) plan: ShrinkPlan,
    pub(super) trimmed: usize,
    pub(super) pushed: usize,
    pub(super) counted: bool,
}

/// What a rows-only grow did, for [`Edge::after_grow`].
pub(super) struct Grown {
    pub(super) rows: usize,
    pub(super) unpushed: usize,
    pub(super) natural: usize,
    pub(super) handed_back: usize,
    pub(super) revealed: usize,
    pub(super) restored_top: usize,
}

impl Edge {
    const fn trimmed(rows: u16, above: u16, pushed: u16) -> Self {
        if rows == 0 && pushed == 0 {
            Self::None
        } else {
            Self::Trimmed {
                rows,
                above,
                pushed,
                seams: Seams {
                    gap: 0,
                    top: false,
                    bottom: false,
                },
            }
        }
    }

    /// The seams the pushed rows owed left (none unless [`Edge::Trimmed`] with pushes).
    pub(super) const fn seams(self) -> Seams {
        match self {
            Self::Trimmed { pushed, seams, .. } if pushed > 0 => seams,
            _ => Seams {
                gap: 0,
                top: false,
                bottom: false,
            },
        }
    }

    /// `self` with the pushed rows' seams set to `seams` (unchanged when no pushed rows are owed).
    pub(super) const fn with_seams(self, seams: Seams) -> Self {
        match self {
            Self::Trimmed {
                rows,
                above,
                pushed,
                ..
            } if pushed > 0 => Self::Trimmed {
                rows,
                above,
                pushed,
                seams,
            },
            edge => edge,
        }
    }

    const fn revealed(rows: u16, above: u16) -> Self {
        if rows == 0 {
            Self::None
        } else {
            Self::Revealed { rows, above }
        }
    }

    const fn trimmed_rows(self) -> u16 {
        match self {
            Self::Trimmed { rows, .. } => rows,
            Self::None | Self::Revealed { .. } => 0,
        }
    }

    const fn revealed_rows(self) -> u16 {
        match self {
            Self::Revealed { rows, .. } => rows,
            Self::None | Self::Trimmed { .. } => 0,
        }
    }

    /// The pushed rows owed (0 unless [`Edge::Trimmed`] with pushes).
    pub(super) const fn pushed_rows(self) -> u16 {
        match self {
            Self::Trimmed { pushed, .. } => pushed,
            Self::None | Self::Revealed { .. } => 0,
        }
    }

    fn scrolled(self, lines: u64, law: ResizeLaw) -> Self {
        match self {
            Self::Trimmed { pushed, .. } if pushed > 0 && lines > 0 => Self::None,
            Self::Trimmed {
                rows,
                above,
                pushed,
                seams,
            } => {
                let used = rows.min(u16::try_from(lines).unwrap_or(u16::MAX));
                let above = if law >= ResizeLaw::Rewrap {
                    above.saturating_add(used)
                } else {
                    above
                };
                Self::trimmed(rows - used, above, pushed).with_seams(seams)
            }
            edge => edge,
        }
    }

    const fn without_pushes(self) -> Self {
        match self {
            Self::Trimmed { pushed, .. } if pushed > 0 => Self::None,
            edge => edge,
        }
    }

    /// The owed window's top against the screen's, in rows (above it when negative): the screen's top when nothing is owed, the demoted rows above it, or the revealed lines' end below it.
    pub(super) fn top(self) -> isize {
        match self {
            Self::None => 0,
            Self::Trimmed { above, .. } => -isize::try_from(above).unwrap_or(isize::MAX),
            Self::Revealed { rows, .. } => isize::try_from(rows).unwrap_or(isize::MAX),
        }
    }

    const fn above(self) -> u16 {
        match self {
            Self::Trimmed { above, .. } | Self::Revealed { above, .. } => above,
            Self::None => 0,
        }
    }

    /// How a shrink of `rows` with the cursor on `cursor_row` begins: the appended rows it takes back as any shrink would, then the revealed lines it demotes back.
    pub(super) fn plan_shrink(self, rows: usize, cursor_row: usize) -> ShrinkPlan {
        let Self::Revealed { rows: owed, above } = self else {
            return ShrinkPlan {
                natural: 0,
                unreveal: 0,
                forgot: false,
            };
        };
        let natural = rows.min(usize::from(above));
        let owed = usize::from(owed);
        let forgot = cursor_row < owed;
        let unreveal = if forgot {
            0
        } else {
            (rows - natural).min(owed)
        };
        ShrinkPlan {
            natural,
            unreveal,
            forgot,
        }
    }

    /// What is owed after the shrink `took`, from `self` owed before it.
    pub(super) fn after_shrink(self, took: &Shrunk) -> Self {
        if !took.counted {
            return Self::None;
        }
        let ShrinkPlan {
            natural,
            unreveal,
            forgot,
        } = took.plan;
        let fresh = took.rows - natural - unreveal;
        let trimmed_fresh = took.trimmed - natural.min(took.trimmed);
        let Some(demoted_fresh) = (fresh - trimmed_fresh).checked_sub(took.pushed) else {
            return Self::None;
        };
        let pushed_fresh = row_u16(took.pushed);
        let demoted_fresh = row_u16(demoted_fresh);
        let fresh_only = Self::trimmed(row_u16(trimmed_fresh), demoted_fresh, pushed_fresh);
        let trimmed_fresh = row_u16(trimmed_fresh);
        match self {
            Self::Revealed { rows, above } if !forgot => {
                if natural < usize::from(above) {
                    Self::Revealed {
                        rows,
                        above: above - row_u16(natural),
                    }
                } else if unreveal < usize::from(rows) {
                    Self::revealed(rows - row_u16(unreveal), 0)
                } else {
                    fresh_only
                }
            }
            Self::Revealed { .. } | Self::None => fresh_only,
            Self::Trimmed {
                rows,
                above,
                pushed,
                seams,
            } if pushed > 0 => {
                if trimmed_fresh == 0 && demoted_fresh == 0 {
                    Self::Trimmed {
                        rows,
                        above,
                        pushed: pushed.saturating_add(pushed_fresh),
                        seams,
                    }
                } else {
                    // Trims or demotes after pushes (a blank row the pushes
                    // left at the bottom, or output between): the older
                    // moves are forgotten.
                    fresh_only
                }
            }
            Self::Trimmed {
                rows, above, seams, ..
            } => {
                if trimmed_fresh == 0 {
                    Self::Trimmed {
                        rows,
                        above: above.saturating_add(demoted_fresh),
                        pushed: pushed_fresh,
                        seams,
                    }
                } else if above == 0 {
                    Self::Trimmed {
                        rows: rows.saturating_add(trimmed_fresh),
                        above: demoted_fresh,
                        pushed: pushed_fresh,
                        seams,
                    }
                } else {
                    // Trims after demotes (output drew a blank tail between):
                    // the older moves are forgotten.
                    fresh_only
                }
            }
        }
    }

    /// How a grow of `rows` begins: `(unpushed, natural, handed_back)`, the pushed rows it puts back at the bottom, then (once they are all back) the demoted rows it reveals back as any grow would, then the trimmed blank rows it appends back.
    pub(super) fn plan_grow(self, rows: usize) -> (usize, usize, usize) {
        let Self::Trimmed {
            rows: owed,
            above,
            pushed,
            ..
        } = self
        else {
            return (0, 0, 0);
        };
        let unpushed = rows.min(usize::from(pushed));
        let rest = rows - unpushed;
        let natural = rest.min(usize::from(above));
        (unpushed, natural, (rest - natural).min(usize::from(owed)))
    }

    /// What is owed after the grow `took`, from `self` owed before it.
    pub(super) fn after_grow(self, took: &Grown) -> Self {
        let fresh = took.rows - took.unpushed - took.natural - took.handed_back;
        let revealed_fresh = took.revealed - took.natural.min(took.revealed);
        let appended_fresh = row_u16(fresh - revealed_fresh);
        let revealed_fresh = row_u16(revealed_fresh.saturating_sub(took.restored_top));
        match self {
            Self::Trimmed {
                rows,
                above,
                pushed,
                seams,
            } => {
                if took.unpushed < usize::from(pushed) {
                    Self::Trimmed {
                        rows,
                        above,
                        pushed: pushed - row_u16(took.unpushed),
                        seams,
                    }
                } else if took.natural < usize::from(above) {
                    Self::Trimmed {
                        rows,
                        above: above - row_u16(took.natural),
                        pushed: 0,
                        seams: Seams::default(),
                    }
                } else if took.handed_back < usize::from(rows) {
                    Self::trimmed(rows - row_u16(took.handed_back), 0, 0)
                } else {
                    Self::revealed(revealed_fresh, appended_fresh)
                }
            }
            Self::None => Self::revealed(revealed_fresh, appended_fresh),
            Self::Revealed { rows, above } => {
                if revealed_fresh == 0 {
                    Self::Revealed {
                        rows,
                        above: above.saturating_add(appended_fresh),
                    }
                } else if above == 0 {
                    Self::Revealed {
                        rows: rows.saturating_add(revealed_fresh),
                        above: appended_fresh,
                    }
                } else {
                    // Reveals after appends (output scrolled history in
                    // between): the older moves are forgotten.
                    Self::revealed(revealed_fresh, appended_fresh)
                }
            }
        }
    }
}

impl Edge {
    /// What is owed after a width change laid the rows out from a [`RewrapPlan`]: the owed window's top `off` rows below the screen's top (above it when negative), `tail` blank rows under the cursor against the window's `base_tail`, and `pushed` of its rows pushed off the bottom.
    pub(super) fn after_rewrap(off: isize, tail: usize, base_tail: usize, pushed: usize) -> Self {
        let rows = |n: usize| row_u16(n);
        if off > 0 {
            match tail.checked_sub(base_tail) {
                Some(appended) if pushed == 0 => {
                    Self::revealed(rows(off.unsigned_abs()), rows(appended))
                }
                _ => Self::None,
            }
        } else {
            base_tail.checked_sub(tail).map_or(Self::None, |trimmed| {
                Self::trimmed(rows(trimmed), rows(off.unsigned_abs()), rows(pushed))
            })
        }
    }
}

/// Where a width change finds the window the hand-back owes back ([`Grid::prepare_rewrap`]), and the blank rows under the cursor it had.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RewrapPlan {
    pub(super) top: BaseTop,
    pub(super) tail: usize,
}

/// The owed window's top, against the first row the screen rewrap lays out (the screen's top, the head of the line it continued brought onto it, `Grid::reveal_line_head`), and the cell of its row it starts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BaseTop {
    /// This many rows above it at the new width, in history, starting this many cells into that row.
    Above(usize, u16),
    /// On this source row at or under it (counted from it), this many cells into it ([`HandBack::skew`]), which the rewrap carries to its new row and column.
    Below(usize, u16),
}

/// [`Edge`] as of a counter reading.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HandBack {
    edge: Edge,
    at: u64,
    one_pass: bool,
    refill: Refill,
    lifted: u16,
    planned: Option<RewrapPlan>,
    fill: Option<FillOwed>,
    skew: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FillOwed {
    top: isize,
    base_tail: usize,
    skew: u16,
}

/// What the rows step of a split resize moved (`Grid::split_rows_step`), offered to the rows its width change frees (a widen that unwraps lines, the tail of a line whose head is in history lifted into the history rewrap) before the bottom-anchoring fill pulls history for the rest ([`Grid::begin_pass`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Refill {
    pub(super) trimmed: u16,
    pub(super) back: u16,
    pub(super) owed: u16,
}

impl Refill {
    /// Of `freed` rows the width change freed, how many stay blank on the screen.
    pub(super) fn blanks_back(self, freed: usize) -> usize {
        usize::from(self.trimmed).min(freed.saturating_sub(usize::from(self.back)))
    }
}

impl HandBack {
    fn owed(self, counter: u64, law: ResizeLaw) -> Edge {
        counter
            .checked_sub(self.at)
            .map_or(Edge::None, |scrolled| self.edge.scrolled(scrolled, law))
    }
}

impl Grid {
    /// What a rows-only resize starts from ([`Edge::None`] under the ConPTY policy, which hands nothing back).
    pub(super) fn handback_owed(&self, native: bool) -> Edge {
        if native {
            self.storage
                .handback
                .owed(self.storage.absolute_row_counter, self.storage.resize_law)
        } else {
            Edge::None
        }
    }

    /// Record `edge` owed as of now, after the rows-only resize that moved the counter for its own trims and appends.
    pub(super) fn set_handback(&mut self, edge: Edge) {
        self.storage.handback = HandBack {
            edge,
            at: self.storage.absolute_row_counter,
            ..HandBack::default()
        };
    }

    /// Set the row counter for a move that scrolled nothing into history: lines put back in front of the ring, or keys raised to cover a rewrap or a restore.
    pub(super) fn renumber_rows(&mut self, counter: u64) {
        let owed = self
            .storage
            .handback
            .owed(self.storage.absolute_row_counter, self.storage.resize_law);
        let skew = self.window_skew();
        if counter != self.storage.absolute_row_counter {
            self.lose_anchors_renumbered();
        }
        self.storage.absolute_row_counter = counter;
        self.storage.handback = HandBack {
            edge: owed,
            at: counter,
            skew,
            ..self.storage.handback
        };
    }

    /// What a width rewrap keeps of what was owed before it (`owed`, read before it moved anything): the trims less one per row it pushed into history, as lines scrolled there use them, and less one per trimmed row it put back ([`Self::begin_pass`]); set against the counter it left.
    pub(super) fn keep_handback_across_rewrap(&mut self, owed: Edge, used: usize) {
        self.set_handback(
            owed.without_pushes()
                .scrolled(used as u64, self.storage.resize_law),
        );
    }

    /// The newest history lines left the ring with the counter unmoved (an unscroll put them back on the screen, a bottom-anchoring fill pulled them into the viewport): the pushed rows owed are no longer the newest lines, so they are forgotten with the moves owed under them, and a grow reveals history as an aterm before the hand-back did.
    pub(super) fn forget_pushed_rows(&mut self) {
        let edge = self.storage.handback.edge;
        let kept = edge.without_pushes();
        if kept.top() != edge.top() {
            self.storage.handback.skew = 0;
        }
        self.storage.handback.edge = kept;
    }

    /// The cells into its row at which the owed window's top starts ([`HandBack::skew`]), while what is owed still puts the top on the row it was set on: with no line scrolled into history since, or, under the current law, trims owed, which keep the top on its line as lines scroll past it. 0 otherwise, a top no longer owed among them (the demoted rows above trims all used up).
    pub(super) fn window_skew(&self) -> u16 {
        let handback = self.storage.handback;
        let counter = self.storage.absolute_row_counter;
        let law = self.storage.resize_law;
        let owed = handback.owed(counter, law);
        let kept = if counter == handback.at {
            owed.top() == handback.edge.top()
        } else {
            law >= ResizeLaw::Rewrap
                && matches!(handback.edge, Edge::Trimmed { pushed: 0, .. })
                && matches!(owed, Edge::Trimmed { .. })
        };
        if kept { handback.skew } else { 0 }
    }

    /// The width change or rows-only resize just recorded what is owed: the window's top, `top` rows below the screen's (above it when negative), starts `skew` cells into its row, kept while what is owed puts the top there.
    pub(super) fn keep_skew(&mut self, top: isize, skew: u16) {
        if self.handback_owed(true).top() == top {
            self.storage.handback.skew = skew;
        }
    }

    /// The program may redraw the window's top row: the cursor moved up onto or above it, or the screen was erased.
    pub(super) fn forget_skew_at(&mut self, row: u16) {
        let top = self.storage.handback.edge.top();
        if top >= 0 && isize::from(row.cast_signed()) <= top {
            self.storage.handback.skew = 0;
        }
    }

    /// An erase of every row below the cursor (ED 0, ED 2, from wherever the cursor is), under the current law: at the taller height the rows owed for a push stood below the screen and the erase took them too, so a grow must not put them back under what the program just drew.
    pub(super) fn forget_pushed_rows_erased(&mut self) {
        if self.storage.resize_law >= ResizeLaw::Rewrap {
            self.forget_pushed_rows();
        }
    }

    /// A shrink pushed `newer` rows into history above the `older` pushed rows still owed, which the screen held under them: put the newer rows' ring extras in front of the older ones', as the rows go.
    pub(super) fn put_pushed_extras_above(&mut self, older: usize, newer: usize) {
        let extras = &mut self.storage.ring_extras;
        let mut tail: Vec<_> = (0..older + newer)
            .map_while(|_| extras.pop_back())
            .collect();
        tail.reverse();
        let newer_slots = tail.split_off(tail.len().saturating_sub(newer));
        extras.extend(newer_slots);
        extras.extend(tail);
    }

    /// Under the current law, take the wraps off at the seams of a bottom push a rows-only shrink just rotated into the (linearized) ring ([`Seams`]): its `pushed` rows from ring index `block`, the topmost first, then, when it went above rows an earlier shrink pushed and still owes, those (`older`: their count and seams), then the screen.
    pub(super) fn cut_push_seams(
        &mut self,
        block: usize,
        pushed: usize,
        older: Option<(u16, Seams)>,
    ) -> Seams {
        let rows = &mut self.storage.rows;
        let mut seams = Seams::default();
        if let Some(first) = rows.get_mut(block).filter(|row| row.is_wrapped()) {
            first.set_wrapped(false);
            seams.bottom = true;
        }
        let older_rows = older.map_or(0, |(rows, _)| usize::from(rows));
        if let Some((_, older)) = older {
            seams.top = older.top;
            if older.bottom
                && let Some(first) = rows.get_mut(block + pushed)
            {
                first.set_wrapped(true);
            }
        }
        if let Some(top) = rows
            .get_mut(block + pushed + older_rows)
            .filter(|row| row.is_wrapped())
        {
            top.set_wrapped(false);
            seams.top = true;
        }
        seams
    }

    /// Under the current law, a rows-only grow just put the top `unpushed` of the `pushed` rows owed back under the screen (`[rest][screen][them]` from ring index `hist - pushed`, linearized; `visible` screen rows): the wraps their seams took off go back where a row again follows the one it continued, and the rest's new first row, whose predecessor is on the screen again, loses its own.
    pub(super) fn mend_push_seams(
        &mut self,
        hist: usize,
        visible: usize,
        pushed: usize,
        unpushed: usize,
        seams: Seams,
    ) -> Seams {
        let rows = &mut self.storage.rows;
        if seams.bottom
            && let Some(first) = rows.get_mut(hist - unpushed + visible)
        {
            first.set_wrapped(true);
        }
        if unpushed < pushed {
            let cut = rows
                .get_mut(hist - pushed)
                .filter(|row| row.is_wrapped())
                .map(|row| row.set_wrapped(false))
                .is_some();
            Seams {
                bottom: cut,
                ..seams
            }
        } else {
            if seams.top
                && let Some(top) = rows.get_mut(hist - pushed)
            {
                top.set_wrapped(true);
            }
            Seams::default()
        }
    }

    /// After a resize that pushed rows off the bottom or put some back, under the current law: the blank rows now under the cursor are the ones the pushes left between drawn rows ([`Seams::gap`]).
    pub(super) fn note_push_gap(&mut self) {
        let edge = self.storage.handback.edge;
        if self.storage.resize_law >= ResizeLaw::Rewrap && edge.pushed_rows() > 0 {
            let gap = row_u16(self.trailing_blank_rows_below_cursor());
            self.storage.handback.edge = edge.with_seams(Seams {
                gap,
                ..edge.seams()
            });
        }
    }

    /// Take the ring extras of the top `take` rows of the `pushed` rows owed (the newest `pushed` history rows, topmost first), which a grow puts back under the screen: `(i, extras)` for the `i`th of them that has any.
    pub(super) fn take_unpushed_extras(
        &mut self,
        pushed: usize,
        take: usize,
    ) -> Vec<(usize, Box<ScrolledRowExtras>)> {
        let extras = &mut self.storage.ring_extras;
        let mut block: Vec<_> = (0..pushed).map_while(|_| extras.pop_back()).collect();
        block.reverse();
        let missing = pushed - block.len();
        let kept = block.split_off(take.saturating_sub(missing).min(block.len()));
        extras.extend(kept);
        block
            .into_iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.map(|bx| (missing + i, bx)))
            .collect()
    }

    /// Forget what is owed and run the next resize as an aterm before the hand-back did: a grow reveals history first, a shrink trims first, and a resize that moves both axes runs as one pass, with no plan (`Grid::prepare_rewrap`) and not as its rows step then the width change (`Grid::split_rows_step`).
    pub fn forget_handback(&mut self) {
        self.storage.handback = HandBack {
            one_pass: true,
            ..HandBack::default()
        };
    }

    /// Drop what is owed: a width change with no rewrap, or under ConPTY, lays the rows out again with nothing to hand back.
    pub(super) fn drop_handback(&mut self) {
        self.storage.handback = HandBack::default();
    }

    /// Whether this resize runs as one pass ([`Self::forget_handback`]).
    pub(super) const fn handback_one_pass(&self) -> bool {
        self.storage.handback.one_pass
    }

    /// Offer the next pass, a split's width change, `refill` ([`Self::begin_pass`]).
    pub(super) const fn offer_refill(&mut self, refill: Refill) {
        self.storage.handback.refill = refill;
    }

    /// A resize pass begins: the one-pass request it answers is spent, and so is the refill offered to it, which this returns.
    pub(super) fn begin_pass(&mut self) -> Refill {
        self.storage.handback.one_pass = false;
        self.storage.handback.lifted = 0;
        std::mem::take(&mut self.storage.handback.refill)
    }

    /// The offloaded resize lifted `rows` rows off the top of the screen into its history job before its pass ([`Self::settled_rows`]).
    pub(super) const fn note_rows_lifted(&mut self, rows: usize) {
        self.storage.handback.lifted = row_u16(rows);
    }

    /// The screen's height as the resize under way found it: `visible_rows`, and the rows an offloaded resize lifted into its history job before the pass, which its rewrap lays out again as the synchronous resize's own lift does.
    pub(super) const fn settled_rows(&self) -> u16 {
        self.storage
            .visible_rows
            .saturating_add(self.storage.handback.lifted)
    }

    /// Where a width change to `new_cols` finds the window owed back, read before anything moves (the current law, [`ResizeLaw::Rewrap`]), with the head of the line the screen's top row continues brought onto the screen first ([`Self::reveal_line_head`]), so the rewrap lays that line out from its head with the cursor on it.
    pub(super) fn prepare_rewrap(&mut self, new_cols: u16) -> Option<RewrapPlan> {
        let tail = self
            .trailing_blank_rows_below_cursor()
            .saturating_sub(self.storage.fill_debt.rows);
        let edge = self.handback_owed(true);
        let (skew_top, skew) = (edge.top(), self.window_skew());
        let owed = match edge {
            Edge::Trimmed {
                pushed: 0, rows, ..
            } => Some((skew_top, Some(tail + usize::from(rows)))),
            Edge::Revealed { above, .. } => Some((skew_top, tail.checked_sub(usize::from(above)))),
            Edge::None | Edge::Trimmed { .. } => None,
        };
        let head = self.line_head_rows()?;
        if head > 0 && !self.reveal_line_head(head) {
            return None;
        }
        let rows = usize::from(self.storage.visible_rows);
        let plan = |top: isize, tail: Option<usize>| -> Option<RewrapPlan> {
            let tail = tail?;
            let skew = if top == skew_top { skew } else { 0 };
            let top = top.checked_add(isize::try_from(head).ok()?)?;
            let top = match usize::try_from(top) {
                Ok(row) => (row < rows).then_some(BaseTop::Below(row, skew))?,
                Err(_) => {
                    let (rows, col) = self.rows_down_to(top, skew, new_cols)?;
                    BaseTop::Above(rows, col)
                }
            };
            Some(RewrapPlan { top, tail })
        };
        owed.and_then(|(top, tail)| plan(top, tail))
            .or_else(|| plan(0, Some(tail)))
    }

    /// How many history rows a width change reads up to find a line's head ([`Self::prepare_rewrap`]): four screens.
    pub(super) fn head_walk(&self) -> usize {
        4 * usize::from(self.storage.visible_rows.max(1))
    }

    fn line_head_rows(&self) -> Option<usize> {
        let continues = self
            .storage
            .row_at_screen(0)
            .is_some_and(|row| row.is_wrapped() && row.line_size() == super::LineSize::SingleWidth);
        if !continues {
            return Some(0);
        }
        let walk = self.head_walk().min(self.storage.scrollback_lines());
        for i in 0..walk {
            #[cfg(any(test, debug_assertions))]
            crate::test_counters::count_handback_history_reads(1);
            let line = self.try_history_line_rev(i).ok().flatten()?;
            if !line.is_wrapped() {
                return Some(i + 1);
            }
        }
        None
    }

    fn reveal_line_head(&mut self, rows: usize) -> bool {
        let visible = usize::from(self.storage.visible_rows);
        let ring = self.storage.total_lines - visible;
        if rows > ring {
            #[cfg(any(test, debug_assertions))]
            crate::test_counters::count_handback_history_reads(rows - ring);
            self.pull_history_into_ring(rows - ring);
        }
        let ring = self.storage.total_lines - visible;
        let Some(shift) = u16::try_from(rows)
            .ok()
            .filter(|_| rows <= ring)
            .filter(|shift| self.storage.visible_rows.checked_add(*shift).is_some())
        else {
            return false;
        };
        let keep = ring - rows;
        let mut extras = Vec::new();
        for j in (0..rows).rev() {
            if self.storage.ring_extras.len() <= keep {
                break;
            }
            if let Some(bx) = self.storage.ring_extras.pop_back().flatten() {
                extras.push((row_u16(j), bx));
            }
        }
        let offset = std::mem::take(&mut self.storage.display_offset);
        self.migrate_complex_ring_to_extras();
        self.migrate_rgb_ring_to_extras();
        self.storage.display_offset = offset;
        self.storage.extras.invalidate_rings();
        let bottom = self.storage.visible_rows + shift - 1;
        self.storage.extras.shift_region_down_by(0, bottom, shift);
        self.storage.visible_rows += shift;
        for (row, bx) in extras {
            self.inject_scrolled_extras(row, &bx);
        }
        self.storage.cursor.row += shift;
        if self.storage.saved_cursor.valid {
            self.storage.saved_cursor.cursor.row =
                self.storage.saved_cursor.cursor.row.saturating_add(shift);
        }
        self.storage.display_offset = self
            .storage
            .display_offset
            .min(self.storage.scrollback_lines());
        self.storage.history_reveal_gen = self.storage.history_reveal_gen.wrapping_add(1);
        self.note_rows_rekeyed();
        true
    }

    fn rows_down_to(&self, from: isize, skew: u16, new_cols: u16) -> Option<(usize, u16)> {
        let line_at = |at: isize| -> Option<Line> {
            #[cfg(any(test, debug_assertions))]
            crate::test_counters::count_handback_history_reads(1);
            self.try_history_line_rev(at.unsigned_abs().checked_sub(1)?)
                .ok()
                .flatten()
                .map(Cow::into_owned)
        };
        let mut lines = vec![line_at(from)?];
        let mut head = from;
        while lines.last().is_some_and(Line::is_wrapped) {
            if lines.len() > self.head_walk() {
                return None;
            }
            head -= 1;
            lines.push(line_at(head)?);
        }
        lines.reverse();
        let first = lines.len() - 1;
        for at in from + 1..0 {
            lines.push(line_at(at)?);
        }
        let (rows, at) = rewrap_cell_row(&lines, new_cols, first, usize::from(skew));
        let (row, col) = at?;
        Some((rows - row, col))
    }

    /// Before a resize, under the current law: a Native width change puts the rows owed for a push back under the screen at the old width first, as a rows-only grow does, so its rewrap lays them out with the window they belong to ([`Self::prepare_rewrap`]) and pushes again only what the new height leaves no room for.
    pub(super) fn hand_back_before_resize(
        &mut self,
        new_cols: u16,
        reflow_mode: super::reflow::ReflowMode,
        policy: super::reflow::ResizePolicy,
    ) -> Option<crate::ResizeShape> {
        let law = self.storage.resize_law;
        if law == ResizeLaw::Undo {
            self.forget_handback();
            return None;
        }
        let pushed = self.handback_owed(true).pushed_rows();
        if law != ResizeLaw::Rewrap
            || pushed == 0
            || policy != super::reflow::ResizePolicy::Native
            || !matches!(reflow_mode, super::reflow::ReflowMode::Enabled)
            || new_cols.clamp(1, crate::MAX_GRID_COLS) == self.storage.cols
            || !self.keeps_handback_history()
            || self.handback_one_pass()
        {
            return None;
        }
        let rows = self
            .storage
            .visible_rows
            .checked_add(pushed)
            .filter(|rows| *rows <= crate::MAX_GRID_ROWS)?;
        let offset = std::mem::take(&mut self.storage.display_offset);
        self.resize_axes(rows, self.storage.cols, reflow_mode, policy);
        self.storage.display_offset = offset;
        self.storage.last_resize_row_shift = 0;
        Some(self.storage.last_resize_shape)
    }

    /// One record for a resize [`Self::hand_back_before_resize`] began: the width change's, with the rows the hand-back put back under the screen, so the resize's record says it gave them back (`restored_bottom`) as a rows grow does, and the rows the width change pushed off again are its own `pushed`.
    pub(super) fn merge_handback_step(&mut self, handed: crate::ResizeShape) {
        let shape = &mut self.storage.last_resize_shape;
        shape.restored_bottom = shape.restored_bottom.saturating_add(handed.restored_bottom);
    }

    /// Under the current law, the rows owed for a push come back from wherever the grid keeps them, as the newest history lines: a rewrap leaves them in the tiered store when the grid has one (`Grid::pull_history_into_ring`).
    pub(super) fn seat_pushed_rows_in_ring(&mut self, owed: Edge) -> bool {
        let pushed = usize::from(owed.pushed_rows());
        let ring = self.storage.total_lines - usize::from(self.storage.visible_rows);
        if pushed > ring && self.storage.resize_law >= ResizeLaw::Rewrap {
            self.pull_history_into_ring(pushed - ring);
        }
        pushed <= self.storage.total_lines - usize::from(self.storage.visible_rows)
    }

    /// The grid keeps history the hand-back can move rows through: a ring that retains lines, or a store past it.
    pub(super) fn keeps_handback_history(&self) -> bool {
        self.storage.max_scrollback > 0 || self.storage.stages_evicted_rows()
    }

    /// The offloaded resize made `plan` before it took the history away: the pass spends it rather than plan against a history it no longer has.
    pub(super) const fn offer_plan(&mut self, plan: RewrapPlan) {
        self.storage.handback.planned = Some(plan);
    }

    /// The plan the offloaded resize offered, spent.
    pub(super) const fn take_plan(&mut self) -> Option<RewrapPlan> {
        self.storage.handback.planned.take()
    }

    /// Owe what a width change laid out from a plan: the window's top `top` rows below the screen's (above it when negative), `skew` cells into its row, and its `base_tail` blank rows, with the fill done, or `deferred` to the re-attach of an offloaded resize, which pulls history until `target` blank rows are left.
    pub(super) fn owe_after_rewrap(
        &mut self,
        top: isize,
        skew: u16,
        target: usize,
        base_tail: usize,
        deferred: bool,
    ) {
        let tail = self.trailing_blank_rows_below_cursor();
        let assumed = if deferred {
            tail.saturating_sub(target)
        } else {
            0
        };
        let assumed_top = top.saturating_add(isize::try_from(assumed).unwrap_or(isize::MAX));
        self.set_handback(Edge::after_rewrap(
            assumed_top,
            tail - assumed,
            base_tail,
            0,
        ));
        self.keep_skew(assumed_top, skew);
        if deferred {
            self.storage.handback.fill = Some(FillOwed {
                top,
                base_tail,
                skew,
            });
        }
    }

    /// The re-attach of an offloaded resize ran its fill, which pulled `pulled` rows of history (0 with none left to pull, or no fill): what the width change owes is where that left the window.
    pub(super) fn settle_fill(&mut self, pulled: usize) {
        let Some(fill) = self.storage.handback.fill.take() else {
            return;
        };
        if self.storage.handback.at != self.storage.absolute_row_counter {
            return;
        }
        let top = fill
            .top
            .saturating_add(isize::try_from(pulled).unwrap_or(isize::MAX));
        self.set_handback(Edge::after_rewrap(
            top,
            self.trailing_blank_rows_below_cursor(),
            fill.base_tail,
            0,
        ));
        self.keep_skew(top, fill.skew);
    }

    /// The resize law this grid lays its rows out under ([`ResizeLaw`]).
    #[must_use]
    pub const fn resize_law(&self) -> ResizeLaw {
        self.storage.resize_law
    }

    /// Lay the rows out under `law` from the next resize on: an older law for the replay of a recording made under it (`cast drift`).
    pub const fn set_resize_law(&mut self, law: ResizeLaw) {
        self.storage.resize_law = law;
    }

    /// An erase of the whole screen (ED 2): the program draws the new screen from the top, so the revealed history lines are no longer owed back, nor the cell of its row the window's top started on.
    pub(super) fn forget_revealed_rows(&mut self) {
        if let Edge::Revealed { .. } = self.storage.handback.edge {
            self.storage.handback = HandBack::default();
        }
        self.forget_skew_at(0);
    }

    /// The cursor just moved up (CUP, HVP, VPA, home, DECRC, CUU, RI): left among the revealed history lines, it is the program's own to draw on, a redraw from the top with or without an erase first, so they are no longer owed back and the shrink trims under its screen, as on one that never grew.
    #[inline]
    pub(super) fn note_cursor_moved_up(&mut self) {
        if let Edge::Revealed { rows, .. } = self.storage.handback.edge
            && self.storage.cursor.row < rows
        {
            self.storage.handback = HandBack::default();
        }
        if self.storage.handback.skew > 0 {
            self.forget_skew_at(self.storage.cursor.row);
        }
    }

    /// How many blank rows a Native grow would append back before revealing history afresh (0: none).
    #[must_use]
    pub fn trimmed_rows(&self) -> u16 {
        self.handback_owed(true).trimmed_rows()
    }

    /// How many revealed history lines a Native shrink would demote back before trimming afresh (0: none).
    #[must_use]
    pub fn revealed_rows(&self) -> u16 {
        self.handback_owed(true).revealed_rows()
    }

    /// How many drawn rows a bottom-push put into history that a grow or a width change puts back under the screen (0: none).
    #[must_use]
    pub fn pushed_rows(&self) -> u16 {
        self.handback_owed(true).pushed_rows()
    }

    /// What is owed, signed, for a checkpoint: positive for trimmed blank rows, negative for revealed history lines, 0 for nothing.
    #[must_use]
    pub fn handback_rows(&self) -> i32 {
        match self.handback_owed(true).without_pushes() {
            Edge::None => 0,
            Edge::Trimmed { rows, .. } => i32::from(rows),
            Edge::Revealed { rows, .. } => -i32::from(rows),
        }
    }

    /// The natural moves made after what is owed, for a checkpoint: rows demoted after trims, or blank rows appended after reveals.
    #[must_use]
    pub fn handback_above(&self) -> u16 {
        self.handback_owed(true).without_pushes().above()
    }

    /// Take a checkpoint's [`Self::handback_rows`] and [`Self::handback_above`] as owed from now.
    pub fn seed_handback(&mut self, rows: i32, above: u16) {
        let bound = |n: u32| u16::try_from(n).unwrap_or(u16::MAX);
        let edge = if rows >= 0 {
            Edge::trimmed(bound(rows.unsigned_abs()), above, 0)
        } else {
            Edge::revealed(bound(rows.unsigned_abs()), above)
        };
        self.set_handback(edge);
    }

    /// Capture the full owed hand-back state (pushed rows, cut seams, window skew and resize law included) for an in-process replay keyframe.
    #[must_use]
    pub fn capture_handback_carry(&self) -> GridHandbackCarry {
        GridHandbackCarry {
            edge: self.handback_owed(true),
            skew: self.window_skew(),
            resize_law: self.storage.resize_law,
        }
    }

    /// Install an in-process replay keyframe's [`GridHandbackCarry`] onto this restored grid when its retained history still holds every owed pushed row.
    pub fn apply_handback_carry(&mut self, carry: GridHandbackCarry) {
        self.storage.resize_law = carry.resize_law;
        if usize::from(carry.edge.pushed_rows()) <= self.storage.scrollback_lines() {
            self.set_handback(carry.edge);
            self.keep_skew(carry.edge.top(), carry.skew);
        } else {
            self.set_handback(carry.edge.without_pushes());
        }
    }
}

/// Canonical wire parts of a [`GridHandbackCarry`] for versioned state serialization.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CanonicalHandbackCarry {
    /// Edge variant tag (`0 = None`, `1 = Trimmed`, `2 = Revealed`).
    pub edge_kind: u8,
    /// Trimmed blank rows or revealed history lines.
    pub rows: u16,
    /// Rows demoted after trims or blank rows appended after reveals.
    pub above: u16,
    /// Bottom rows pushed into history (`Trimmed` only).
    pub pushed: u16,
    /// Blank gap left under the cursor by bottom pushes (`Trimmed` with `pushed > 0` only).
    pub seam_gap: u16,
    /// Whether bottom pushes cut the screen's new top row (`Trimmed` with `pushed > 0` only).
    pub seam_top: bool,
    /// Whether bottom pushes cut the first pushed row (`Trimmed` with `pushed > 0` only).
    pub seam_bottom: bool,
    /// Column skew of the owed window's top row.
    pub skew: u16,
    /// Resize law tag (`0 = Undo`, `1 = HandBack`, `2 = PushedRows`, `3 = Rewrap`).
    pub resize_law: u8,
}

/// The in-process resize hand-back state a replay keyframe carries past a checkpoint's `(handback_rows, handback_above)`: the full owed [`Edge`] (pushed rows and their cut seams included), the owed window's column [`HandBack::skew`], and the active [`ResizeLaw`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GridHandbackCarry {
    edge: Edge,
    skew: u16,
    resize_law: ResizeLaw,
}

impl GridHandbackCarry {
    /// Derive the baseline carry a restored checkpoint's `(handback_rows, handback_above)` seeds under `resize_law`.
    #[must_use]
    pub const fn from_cursor(rows: i32, above: u16, resize_law: ResizeLaw) -> Self {
        let abs = rows.unsigned_abs();
        let bounded = if abs > u16::MAX as u32 {
            u16::MAX
        } else {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "explicitly clamped to u16::MAX above"
            )]
            {
                abs as u16
            }
        };
        let edge = if rows >= 0 {
            Edge::trimmed(bounded, above, 0)
        } else {
            Edge::revealed(bounded, above)
        };
        Self {
            edge,
            skew: 0,
            resize_law,
        }
    }

    /// Drop owed pushed rows when `max_history` retained scrollback lines cannot hold all of them, matching [`Grid::apply_handback_carry`].
    #[must_use]
    pub const fn bounded(self, max_history: usize) -> Self {
        if (self.edge.pushed_rows() as usize) <= max_history {
            self
        } else {
            Self {
                edge: self.edge.without_pushes(),
                skew: 0,
                resize_law: self.resize_law,
            }
        }
    }

    /// Decompose into canonical wire parts for versioned state serialization.
    #[must_use]
    pub const fn to_canonical(self) -> CanonicalHandbackCarry {
        let resize_law = match self.resize_law {
            ResizeLaw::Undo => 0,
            ResizeLaw::HandBack => 1,
            ResizeLaw::PushedRows => 2,
            ResizeLaw::Rewrap => 3,
        };
        match self.edge {
            Edge::None => CanonicalHandbackCarry {
                edge_kind: 0,
                rows: 0,
                above: 0,
                pushed: 0,
                seam_gap: 0,
                seam_top: false,
                seam_bottom: false,
                skew: self.skew,
                resize_law,
            },
            Edge::Trimmed {
                rows,
                above,
                pushed,
                seams,
            } => {
                let effective = if pushed > 0 {
                    seams
                } else {
                    Seams {
                        gap: 0,
                        top: false,
                        bottom: false,
                    }
                };
                CanonicalHandbackCarry {
                    edge_kind: 1,
                    rows,
                    above,
                    pushed,
                    seam_gap: effective.gap,
                    seam_top: effective.top,
                    seam_bottom: effective.bottom,
                    skew: self.skew,
                    resize_law,
                }
            }
            Edge::Revealed { rows, above } => CanonicalHandbackCarry {
                edge_kind: 2,
                rows,
                above,
                pushed: 0,
                seam_gap: 0,
                seam_top: false,
                seam_bottom: false,
                skew: self.skew,
                resize_law,
            },
        }
    }

    /// Reconstruct from canonical wire parts, returning `None` on non-canonical or unknown tags.
    #[must_use]
    pub const fn from_canonical(raw: CanonicalHandbackCarry) -> Option<Self> {
        let resize_law = match raw.resize_law {
            0 => ResizeLaw::Undo,
            1 => ResizeLaw::HandBack,
            2 => ResizeLaw::PushedRows,
            3 => ResizeLaw::Rewrap,
            _ => return None,
        };
        let edge = match raw.edge_kind {
            0 => {
                if raw.rows != 0
                    || raw.above != 0
                    || raw.pushed != 0
                    || raw.seam_gap != 0
                    || raw.seam_top
                    || raw.seam_bottom
                {
                    return None;
                }
                Edge::None
            }
            1 => {
                if raw.rows == 0 && raw.above == 0 && raw.pushed == 0 {
                    return None;
                }
                if raw.pushed == 0 && (raw.seam_gap != 0 || raw.seam_top || raw.seam_bottom) {
                    return None;
                }
                Edge::Trimmed {
                    rows: raw.rows,
                    above: raw.above,
                    pushed: raw.pushed,
                    seams: Seams {
                        gap: raw.seam_gap,
                        top: raw.seam_top,
                        bottom: raw.seam_bottom,
                    },
                }
            }
            2 => {
                if raw.rows == 0
                    || raw.pushed != 0
                    || raw.seam_gap != 0
                    || raw.seam_top
                    || raw.seam_bottom
                {
                    return None;
                }
                Edge::Revealed {
                    rows: raw.rows,
                    above: raw.above,
                }
            }
            _ => return None,
        };
        Some(Self {
            edge,
            skew: raw.skew,
            resize_law,
        })
    }
}

#[cfg(test)]
#[path = "resize_handback_tests.rs"]
mod tests;
